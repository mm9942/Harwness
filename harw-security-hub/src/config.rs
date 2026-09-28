//! Daemon configuration (`/etc/harw-security-hub/config.toml`, masterplan
//! v2 §39).
//!
//! Every table is `deny_unknown_fields`. Production defaults are compiled
//! constants; there is no fallback to a project or home directory. Per
//! drift-report decision D1/D2 the SecurityHub owns its own socket under the
//! host-infrastructure namespace: [`DEFAULT_SOCKET_PATH`].
//!
//! ```toml
//! [server]
//! socket = "/run/harw/infra/security.sock"
//! issuer = "harw-security-hub"
//! node = "node-1"
//! host = "host-a"
//!
//! [findings]
//! path = "/var/lib/harw-dod/export/findings.jsonl"
//! min_severity = "high"
//! window_secs = 86400
//!
//! [policy]
//! max_ttl_secs = 900
//! verifier_uids = [990]
//!
//! [[policy.peers]]
//! uid = 1000
//! tenant = "acme"
//! principal = { kind = "human", id = "alice", surface = "cli", tier = "owner" }
//! ```

use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use harw_types::{HostId, ImpactSeverity, NodeId};

use crate::error::HubError;
use crate::findings::FindingLimits;
use crate::policy::SecurityPolicy;
use crate::table::DEFAULT_MAX_CONTEXTS;

/// Default config file location.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/harw-security-hub/config.toml";
/// Default socket (drift report D1/D2: own `security.sock` under
/// `/run/harw/infra/`).
pub const DEFAULT_SOCKET_PATH: &str = "/run/harw/infra/security.sock";
/// Default issuer name recorded on every context.
pub const DEFAULT_ISSUER: &str = "harw-security-hub";
/// Default correlation window for DoD findings.
pub const DEFAULT_FINDING_WINDOW_SECS: u64 = 24 * 60 * 60;
/// Largest accepted config file. This file maps uids to principal, tenant,
/// tier and trust zone, so it is the root of every context the hub issues
/// and must never be read without a bound (matches harw-netsec's cap on
/// its own config).
const MAX_CONFIG_BYTES: u64 = 256 * 1024;

fn default_socket() -> PathBuf {
    PathBuf::from(DEFAULT_SOCKET_PATH)
}

fn default_issuer() -> String {
    DEFAULT_ISSUER.to_owned()
}

fn default_max_contexts() -> usize {
    DEFAULT_MAX_CONTEXTS
}

fn default_min_severity() -> ImpactSeverity {
    ImpactSeverity::High
}

fn default_window_secs() -> u64 {
    DEFAULT_FINDING_WINDOW_SECS
}

fn default_max_tail_bytes() -> u64 {
    FindingLimits::default().max_tail_bytes
}

fn default_max_line_bytes() -> usize {
    FindingLimits::default().max_line_bytes
}

fn default_max_records() -> usize {
    FindingLimits::default().max_records
}

/// Complete daemon configuration.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HubConfig {
    /// Listener and identity settings.
    #[serde(default)]
    pub server: ServerConfig,
    /// Optional read-only DoD finding export.
    #[serde(default)]
    pub findings: Option<FindingsConfig>,
    /// The peer policy.
    pub policy: SecurityPolicy,
}

/// `[server]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    /// Unix socket path. Ignored under `--systemd-socket`, where the socket
    /// unit (`harw-security-hub.socket`) owns path, mode and group.
    #[serde(default = "default_socket")]
    pub socket: PathBuf,
    /// Issuer name recorded on contexts.
    #[serde(default = "default_issuer")]
    pub issuer: String,
    /// This installation's node id, bound into contexts.
    #[serde(default)]
    pub node: Option<NodeId>,
    /// This host's id as used by DoD findings (correlation key).
    #[serde(default)]
    pub host: Option<HostId>,
    /// Upper bound on simultaneously held contexts.
    #[serde(default = "default_max_contexts")]
    pub max_contexts: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            socket: default_socket(),
            issuer: default_issuer(),
            node: None,
            host: None,
            max_contexts: default_max_contexts(),
        }
    }
}

/// `[findings]`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FindingsConfig {
    /// JSON Lines export written by the DoD side.
    pub path: PathBuf,
    /// Lowest severity attached to the posture.
    #[serde(default = "default_min_severity")]
    pub min_severity: ImpactSeverity,
    /// How far back findings count as "recent".
    #[serde(default = "default_window_secs")]
    pub window_secs: u64,
    /// Bytes read from the end of the export.
    #[serde(default = "default_max_tail_bytes")]
    pub max_tail_bytes: u64,
    /// Longest parsed line.
    #[serde(default = "default_max_line_bytes")]
    pub max_line_bytes: usize,
    /// Most findings attached.
    #[serde(default = "default_max_records")]
    pub max_records: usize,
}

impl FindingsConfig {
    /// The reader limits described by this section.
    #[must_use]
    pub fn limits(&self) -> FindingLimits {
        FindingLimits {
            max_tail_bytes: self.max_tail_bytes,
            max_line_bytes: self.max_line_bytes,
            max_records: self.max_records,
            ..FindingLimits::default()
        }
    }
}

impl HubConfig {
    /// Parses and validates a config from TOML text.
    ///
    /// # Errors
    /// [`HubError::ConfigParse`] for malformed TOML or unknown fields,
    /// [`HubError::ConfigInvalid`] for semantic violations.
    pub fn from_toml_str(text: &str) -> Result<Self, HubError> {
        let config: Self =
            toml::from_str(text).map_err(|error| HubError::ConfigParse(error.to_string()))?;
        config.validate().map_err(HubError::ConfigInvalid)?;
        Ok(config)
    }

    /// Reads and validates the config at `path`.
    ///
    /// This file maps uids to principal, tenant, tier and trust zone, so it
    /// is the root of every context the hub issues. Before ever opening it,
    /// this checks the file's `lstat` metadata — which cannot block even if
    /// `path` names a FIFO, unlike opening it would — to reject symlinks,
    /// anything that is not a plain regular file, and group- or
    /// world-writable permissions; for [`DEFAULT_CONFIG_PATH`] it also
    /// rejects a non-root owner. `open` itself still follows a symlink in
    /// the last path component, so between that `lstat` check and the
    /// `open` call the path could be swapped out for one, or the file
    /// `lstat` inspected could be chmod'ed or chown'ed in place without
    /// changing which inode `open` returns; once the file is open, this
    /// re-checks the now-open handle's `fstat` `(dev, ino)` against the
    /// earlier `lstat` and rejects a mismatch, then re-runs the same
    /// permission and ownership check against that `fstat` metadata,
    /// closing both races before any byte is read. The read itself is
    /// capped at `MAX_CONFIG_BYTES`.
    ///
    /// # Errors
    /// [`HubError::Read`] if the path cannot be statted, is not a
    /// sufficiently trusted regular file (checked both before opening and
    /// again against the open handle), was replaced between the trust
    /// check and the open, or exceeds `MAX_CONFIG_BYTES`; plus everything
    /// from [`Self::from_toml_str`].
    pub fn load(path: &Path) -> Result<Self, HubError> {
        let read_error = |source: io::Error| HubError::Read {
            path: path.to_path_buf(),
            source,
        };
        let require_root = path == Path::new(DEFAULT_CONFIG_PATH);
        let checked_metadata = std::fs::symlink_metadata(path).map_err(read_error)?;
        check_trusted_metadata(&checked_metadata, require_root).map_err(read_error)?;

        let file = std::fs::File::open(path).map_err(read_error)?;
        let opened_metadata = file.metadata().map_err(read_error)?;
        ensure_not_replaced_during_open(&checked_metadata, &opened_metadata).map_err(read_error)?;
        check_trusted_metadata(&opened_metadata, require_root).map_err(read_error)?;

        let mut text = String::new();
        file.take(MAX_CONFIG_BYTES + 1)
            .read_to_string(&mut text)
            .map_err(read_error)?;
        if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_CONFIG_BYTES {
            return Err(read_error(io::Error::other(format!(
                "exceeds {MAX_CONFIG_BYTES} bytes"
            ))));
        }
        Self::from_toml_str(&text)
    }

    /// Semantic checks.
    ///
    /// # Errors
    /// A human-readable reason.
    pub fn validate(&self) -> Result<(), String> {
        if !self.server.socket.is_absolute() {
            return Err("server.socket must be an absolute path".to_owned());
        }
        if self.server.issuer.trim().is_empty() {
            return Err("server.issuer must not be blank".to_owned());
        }
        if self.server.max_contexts == 0 {
            return Err("server.max_contexts must be positive".to_owned());
        }
        if let Some(findings) = &self.findings {
            if !findings.path.is_absolute() {
                return Err("findings.path must be an absolute path".to_owned());
            }
            if findings.window_secs == 0
                || findings.max_tail_bytes == 0
                || findings.max_line_bytes == 0
                || findings.max_records == 0
            {
                return Err("findings limits and window must be positive".to_owned());
            }
            if self.server.host.is_none() {
                return Err("findings requires server.host (the correlation key)".to_owned());
            }
        }
        self.policy.validate()
    }
}

/// Rejects anything but a plain, safely owned and permissioned regular
/// file, purely from `metadata` (an `lstat` or an `fstat`, this function
/// never opens or resolves a path itself): a symlink, a FIFO, a socket, a
/// device or a directory, and (on Unix) group- or world-writable
/// permissions. `require_root` additionally rejects a non-root owner, for
/// the compiled-in system config path. `load` calls this twice: once on
/// the pre-open `lstat` metadata, so a FIFO is rejected without ever
/// blocking on `open`, and again on the post-open `fstat` metadata, to
/// catch a same-inode permission or ownership change that happened in
/// the window between the two.
fn check_trusted_metadata(metadata: &std::fs::Metadata, require_root: bool) -> io::Result<()> {
    if metadata.file_type().is_symlink() {
        return Err(io::Error::other("is a symlink; refusing to follow it"));
    }
    if !metadata.is_file() {
        return Err(io::Error::other("is not a regular file"));
    }
    let mode = metadata.mode();
    if mode & 0o022 != 0 {
        return Err(io::Error::other(format!(
            "mode {:o} is writable by group or others",
            mode & 0o777
        )));
    }
    if require_root && metadata.uid() != 0 {
        return Err(io::Error::other(format!(
            "must be owned by root, found uid {}",
            metadata.uid()
        )));
    }
    Ok(())
}

/// Rejects an already-open file whose `(dev, ino)` does not match the
/// `lstat` metadata `checked` before it was opened. `File::open` follows a
/// symlink in the last path component, so between the `symlink_metadata`
/// check and the `open` call the path could have been swapped for a
/// symlink pointing elsewhere; `(dev, ino)` uniquely identifies the file
/// that was actually opened, so a mismatch here can only mean the path no
/// longer names the file `checked` inspected.
fn ensure_not_replaced_during_open(
    checked: &std::fs::Metadata,
    opened: &std::fs::Metadata,
) -> io::Result<()> {
    if checked.dev() != opened.dev() || checked.ino() != opened.ino() {
        return Err(io::Error::other(
            "path was replaced between the trust check and opening it",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::time::Duration;

    use super::{
        DEFAULT_SOCKET_PATH, HubConfig, check_trusted_metadata, ensure_not_replaced_during_open,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_types::ImpactSeverity;

    #[test]
    fn test_minimal_config_uses_compiled_defaults() -> TestResult {
        let config = HubConfig::from_toml_str("[policy]\n").map_err(ctx("minimal config"))?;
        assert_eq!(config.server.socket.to_str(), Some(DEFAULT_SOCKET_PATH));
        assert_eq!(config.server.issuer, "harw-security-hub");
        assert!(config.findings.is_none());
        assert!(config.policy.peers.is_empty());
        Ok(())
    }

    #[test]
    fn test_full_config_parses() -> TestResult {
        let config = HubConfig::from_toml_str(
            r#"
            [server]
            socket = "/tmp/x/security.sock"
            node = "node-1"
            host = "host-a"

            [findings]
            path = "/var/lib/harw-dod/export/findings.jsonl"
            min_severity = "critical"

            [policy]
            verifier_uids = [990]

            [[policy.peers]]
            uid = 1000
            principal = { kind = "human", id = "alice", surface = "cli", tier = "owner" }
            "#,
        )
        .map_err(ctx("full config"))?;
        let findings = config.findings.as_ref().map(|f| f.min_severity);
        assert_eq!(findings, Some(ImpactSeverity::Critical));
        assert_eq!(config.policy.peers.len(), 1);
        Ok(())
    }

    #[test]
    fn test_invalid_configs_are_rejected() {
        for text in [
            // policy table is mandatory
            "",
            // unknown section
            "[policy]\n[extra]\n",
            // unknown server field
            "[server]\nport = 1\n[policy]\n",
            // relative socket
            "[server]\nsocket = \"rel.sock\"\n[policy]\n",
            // findings without host
            "[findings]\npath = \"/x.jsonl\"\n[policy]\n",
            // relative findings path
            "[server]\nhost = \"h\"\n[findings]\npath = \"x.jsonl\"\n[policy]\n",
        ] {
            assert!(HubConfig::from_toml_str(text).is_err(), "{text:?}");
        }
    }

    /// Creates a FIFO via `mkfifo(1)` (there is no safe-code std API for
    /// `mknod`). `false` if the binary is unavailable, in which case the
    /// caller skips rather than fails.
    fn make_fifo(path: &std::path::Path) -> bool {
        matches!(
            std::process::Command::new("mkfifo").arg(path).status(),
            Ok(status) if status.success()
        )
    }

    #[test]
    fn test_load_reads_a_trusted_config_from_disk() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[policy]\n").map_err(ctx("write"))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(ctx("chmod"))?;
        let config = HubConfig::load(&path).map_err(ctx("load"))?;
        assert!(config.policy.peers.is_empty());
        Ok(())
    }

    /// Covers the brief's three cases in one table, matching
    /// [`test_invalid_configs_are_rejected`]'s style: a directory (not a
    /// regular file), a chmod `0o666` config, a symlink and an oversized
    /// (> `MAX_CONFIG_BYTES`) config must all be rejected.
    #[test]
    fn test_load_rejects_untrusted_files() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;

        let writable = dir.path().join("writable.toml");
        std::fs::write(&writable, "[policy]\n").map_err(ctx("write"))?;
        std::fs::set_permissions(&writable, std::fs::Permissions::from_mode(0o666))
            .map_err(ctx("chmod"))?;

        let target = dir.path().join("real.toml");
        std::fs::write(&target, "[policy]\n").map_err(ctx("write"))?;
        let symlink = dir.path().join("link.toml");
        std::os::unix::fs::symlink(&target, &symlink).map_err(ctx("symlink"))?;

        let oversized = dir.path().join("oversized.toml");
        let cap = usize::try_from(super::MAX_CONFIG_BYTES).unwrap_or(usize::MAX);
        std::fs::write(&oversized, "#".repeat(cap + 1)).map_err(ctx("write"))?;

        for path in [dir.path().to_path_buf(), writable, symlink, oversized] {
            assert!(HubConfig::load(&path).is_err(), "{}", path.display());
        }
        Ok(())
    }

    /// A FIFO must be rejected by `lstat` before `load` ever opens it, or
    /// opening it would block this test (and a production startup) forever
    /// waiting for a writer that never comes.
    #[test]
    fn test_load_rejects_fifo_without_blocking() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("fifo");
        if !make_fifo(&path) {
            return Ok(());
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(HubConfig::load(&path).is_err());
        });
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(rejected) => {
                assert!(rejected);
                Ok(())
            }
            Err(_) => Err(TestError::Unexpected(
                "load() blocked on a FIFO instead of rejecting it".to_owned(),
            )),
        }
    }

    /// `load` only asks for a root owner on [`super::DEFAULT_CONFIG_PATH`]
    /// itself, which a sandboxed test cannot write to; exercise the
    /// `require_root` branch directly instead.
    #[test]
    fn test_check_trusted_metadata_enforces_root_ownership_when_required() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[policy]\n").map_err(ctx("write"))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .map_err(ctx("chmod"))?;
        let metadata = std::fs::symlink_metadata(&path).map_err(ctx("stat"))?;
        assert!(check_trusted_metadata(&metadata, false).is_ok());
        // A test runner is root in some containers; only then may an
        // arbitrary temp file legitimately pass the root-ownership check.
        let owned_by_root = metadata.uid() == 0;
        assert_eq!(
            check_trusted_metadata(&metadata, true).is_ok(),
            owned_by_root
        );
        Ok(())
    }

    /// `load`'s post-open re-check runs [`check_trusted_metadata`] on the
    /// open handle's `fstat` metadata, not the pre-open `lstat` metadata;
    /// confirm the same enforcement holds for that metadata source too,
    /// matching harw-authority's `validate_policy_metadata`, which also
    /// runs on the post-open `fstat` metadata rather than a pre-open
    /// `lstat`.
    #[test]
    fn test_check_trusted_metadata_also_rejects_fstat_metadata() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[policy]\n").map_err(ctx("write"))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666))
            .map_err(ctx("chmod"))?;
        let opened_metadata = std::fs::File::open(&path)
            .map_err(ctx("open"))?
            .metadata()
            .map_err(ctx("fstat"))?;
        assert!(check_trusted_metadata(&opened_metadata, false).is_err());
        Ok(())
    }

    /// Simulates the race `load` closes: the `lstat` metadata checked
    /// before opening a path must match the `fstat` metadata of whatever
    /// ends up open, or the path was swapped out in between (e.g. for a
    /// symlink) and must be rejected, matching harw-authority's
    /// `ensure_policy_not_replaced_detects_a_swapped_file`.
    #[test]
    fn test_ensure_not_replaced_during_open_detects_a_swapped_file() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let original = dir.path().join("config.toml");
        let swapped = dir.path().join("other.toml");
        std::fs::write(&original, "[policy]\n").map_err(ctx("write original"))?;
        std::fs::write(&swapped, "[policy]\n[extra]\n").map_err(ctx("write swapped"))?;

        let checked = std::fs::symlink_metadata(&original).map_err(ctx("stat original"))?;
        let opened_same = std::fs::File::open(&original)
            .map_err(ctx("open original"))?
            .metadata()
            .map_err(ctx("fstat original"))?;
        let opened_other = std::fs::File::open(&swapped)
            .map_err(ctx("open swapped"))?
            .metadata()
            .map_err(ctx("fstat swapped"))?;

        assert!(ensure_not_replaced_during_open(&checked, &opened_same).is_ok());
        assert!(ensure_not_replaced_during_open(&checked, &opened_other).is_err());
        Ok(())
    }
}
