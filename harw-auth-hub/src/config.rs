//! Hub configuration: socket path, allowed peers and optional bearer tokens.
//!
//! The file is TOML and parsed strictly: every table rejects unknown fields
//! (`deny_unknown_fields`), so a typo never silently widens or narrows a
//! grant. Example:
//!
//! ```toml
//! socket_path = "/run/harw/infra/secure.sock"   # optional, this is the default
//!
//! [[peers]]
//! uid = 990                    # SO_PEERCRED uid of the connecting process
//! principal = "harw-web"
//! grants = [
//!   { namespace = "app", ops = ["read-public", "encrypt", "admin"] },
//! ]
//!
//! [[bearer_tokens]]            # optional fallback for peers not listed above
//! principal = "ops-admin"
//! token_file = "/etc/harw-auth-hub/tokens/ops-admin.token"   # mode 0600
//! grants = [ { namespace = "app", ops = ["all"] } ]
//! ```
//!
//! # Operation groups
//!
//! `ops` entries name the `OpSet` groups of `crypt_guard_service`:
//!
//! | Name | Operations |
//! |------|------------|
//! | `read-public` | describe, public key, verify |
//! | `encrypt` | encrypt, wrap |
//! | `secret-egress` | decrypt, unwrap (grant deliberately) |
//! | `sign` | sign |
//! | `rewrap` | rewrap |
//! | `admin` | generate, rotate, disable, enable, destroy |
//! | `all` | every operation |
//!
//! # File permissions
//!
//! - The config file must not be writable by group or others.
//! - Every token file must be a regular file (no symlink) without any group
//!   or other permission bit (`0600` or `0400`). Token bytes are copied once
//!   into zeroizing memory and never logged.

use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crypt_guard_hyper::BearerTokens;
use crypt_guard_service::{
    KeyNamespace, NamespacePolicy, OpSet, Principal as CgPrincipal, SecretBytes,
};
use serde::Deserialize;

use crate::error::HubError;

/// Default socket path when neither the config nor the command line names one.
pub const DEFAULT_SOCKET_PATH: &str = "/run/harw/infra/secure.sock";

/// Default config file path of the binary.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/harw-auth-hub/config.toml";

/// Minimum length of a bearer token, in bytes.
pub const MIN_TOKEN_LEN: usize = 16;

/// Maximum length of a bearer token, in bytes.
pub const MAX_TOKEN_LEN: usize = 4096;

/// Maximum length of a principal name.
const MAX_PRINCIPAL_LEN: usize = 128;

/// A named group of operations, as written in `ops = [...]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum OpGroup {
    /// `OpSet::READ_PUBLIC`.
    ReadPublic,
    /// `OpSet::ENCRYPT`.
    Encrypt,
    /// `OpSet::SECRET_EGRESS`.
    SecretEgress,
    /// `OpSet::SIGN`.
    Sign,
    /// `OpSet::REWRAP`.
    Rewrap,
    /// `OpSet::ADMIN`.
    Admin,
    /// `OpSet::ALL`.
    All,
}

impl OpGroup {
    /// The CryptGuard operation set of this group.
    #[must_use]
    pub fn op_set(self) -> OpSet {
        match self {
            Self::ReadPublic => OpSet::READ_PUBLIC,
            Self::Encrypt => OpSet::ENCRYPT,
            Self::SecretEgress => OpSet::SECRET_EGRESS,
            Self::Sign => OpSet::SIGN,
            Self::Rewrap => OpSet::REWRAP,
            Self::Admin => OpSet::ADMIN,
            Self::All => OpSet::ALL,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    socket_path: Option<PathBuf>,
    #[serde(default)]
    peers: Vec<RawPeer>,
    #[serde(default)]
    bearer_tokens: Vec<RawBearer>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPeer {
    uid: u32,
    principal: String,
    #[serde(default)]
    grants: Vec<RawGrant>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBearer {
    principal: String,
    token_file: PathBuf,
    #[serde(default)]
    grants: Vec<RawGrant>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGrant {
    namespace: String,
    ops: Vec<OpGroup>,
}

/// One namespace grant: `ops` in `namespace`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    /// The key namespace.
    pub namespace: KeyNamespace,
    /// The granted operations.
    pub ops: OpSet,
}

/// An allowed local peer, identified by its `SO_PEERCRED` uid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerEntry {
    /// Kernel-reported uid of the connecting process.
    pub uid: u32,
    /// The principal the uid authenticates as.
    pub principal: CgPrincipal,
    /// The principal's grants.
    pub grants: Vec<Grant>,
}

/// A bearer-token principal (fallback for peers not listed by uid).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BearerEntry {
    /// The principal the token authenticates as.
    pub principal: CgPrincipal,
    /// Absolute path of the token file (content is never stored here).
    pub token_file: PathBuf,
    /// The principal's grants.
    pub grants: Vec<Grant>,
}

/// Validated hub configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HubConfig {
    /// The `AF_UNIX` socket path (ignored under systemd socket activation).
    pub socket_path: PathBuf,
    /// Allowed peers by uid (unique uids).
    pub peers: Vec<PeerEntry>,
    /// Optional bearer-token principals.
    pub bearer_tokens: Vec<BearerEntry>,
}

impl HubConfig {
    /// Parse and validate a config from TOML text. Does not touch the file
    /// system (token files are read by [`HubConfig::load_bearer_tokens`]).
    ///
    /// # Errors
    ///
    /// [`HubError::ConfigParse`] for invalid TOML, unknown fields or unknown
    /// op groups; [`HubError::ConfigInvalid`] for semantic errors (relative
    /// paths, duplicate uids or token files, invalid principal or namespace
    /// names, empty `ops`, nothing configured at all).
    pub fn from_toml_str(text: &str) -> Result<Self, HubError> {
        let raw: RawConfig =
            toml::from_str(text).map_err(|error| HubError::ConfigParse(error.to_string()))?;
        Self::validate(raw)
    }

    /// Read, permission-check, parse and validate the config file at `path`.
    ///
    /// # Errors
    ///
    /// [`HubError::Read`] if the file cannot be read,
    /// [`HubError::InsecurePermissions`] if it is group- or world-writable,
    /// otherwise as [`HubConfig::from_toml_str`].
    pub fn load(path: &Path) -> Result<Self, HubError> {
        let read_error = |source| HubError::Read {
            path: path.to_path_buf(),
            source,
        };
        let mut file = File::open(path).map_err(read_error)?;
        let metadata = file.metadata().map_err(read_error)?;
        let mode = metadata.permissions().mode() & 0o7777;
        if mode & 0o022 != 0 {
            return Err(HubError::InsecurePermissions {
                path: path.to_path_buf(),
                mode,
                expected: "not writable by group or others",
            });
        }
        let mut text = String::new();
        file.read_to_string(&mut text).map_err(read_error)?;
        Self::from_toml_str(&text)
    }

    fn validate(raw: RawConfig) -> Result<Self, HubError> {
        let socket_path = raw
            .socket_path
            .unwrap_or_else(|| PathBuf::from(DEFAULT_SOCKET_PATH));
        if !socket_path.is_absolute() {
            return Err(invalid(format!(
                "socket_path '{}' must be absolute",
                socket_path.display()
            )));
        }

        let mut uids = BTreeSet::new();
        let mut peers = Vec::with_capacity(raw.peers.len());
        for peer in raw.peers {
            if !uids.insert(peer.uid) {
                return Err(invalid(format!("duplicate peer uid {}", peer.uid)));
            }
            peers.push(PeerEntry {
                uid: peer.uid,
                principal: principal(&peer.principal)?,
                grants: grants(peer.grants)?,
            });
        }

        let mut token_files = BTreeSet::new();
        let mut bearer_tokens = Vec::with_capacity(raw.bearer_tokens.len());
        for bearer in raw.bearer_tokens {
            if !bearer.token_file.is_absolute() {
                return Err(invalid(format!(
                    "token_file '{}' must be absolute",
                    bearer.token_file.display()
                )));
            }
            if !token_files.insert(bearer.token_file.clone()) {
                return Err(invalid(format!(
                    "duplicate token_file '{}'",
                    bearer.token_file.display()
                )));
            }
            bearer_tokens.push(BearerEntry {
                principal: principal(&bearer.principal)?,
                token_file: bearer.token_file,
                grants: grants(bearer.grants)?,
            });
        }

        if peers.is_empty() && bearer_tokens.is_empty() {
            return Err(invalid(
                "no peers and no bearer_tokens configured: every request would be rejected"
                    .to_owned(),
            ));
        }

        Ok(Self {
            socket_path,
            peers,
            bearer_tokens,
        })
    }

    /// The deny-by-default CryptGuard policy of all configured grants.
    #[must_use]
    pub fn policy(&self) -> NamespacePolicy {
        let mut policy = NamespacePolicy::new();
        let entries = self
            .peers
            .iter()
            .map(|peer| (&peer.principal, &peer.grants))
            .chain(
                self.bearer_tokens
                    .iter()
                    .map(|bearer| (&bearer.principal, &bearer.grants)),
            );
        for (principal, grants) in entries {
            for grant in grants {
                policy.grant(principal.clone(), grant.namespace.clone(), grant.ops);
            }
        }
        policy
    }

    /// uid → principal map for the `SO_PEERCRED` authenticator.
    #[must_use]
    pub fn peer_principals(&self) -> HashMap<u32, CgPrincipal> {
        self.peers
            .iter()
            .map(|peer| (peer.uid, peer.principal.clone()))
            .collect()
    }

    /// Read every configured token file into a [`BearerTokens`] table, or
    /// `None` if no bearer tokens are configured.
    ///
    /// # Errors
    ///
    /// See [`read_token_file`].
    pub fn load_bearer_tokens(&self) -> Result<Option<BearerTokens>, HubError> {
        if self.bearer_tokens.is_empty() {
            return Ok(None);
        }
        let mut table = BearerTokens::new();
        for bearer in &self.bearer_tokens {
            let token = read_token_file(&bearer.token_file)?;
            table.insert(token, bearer.principal.clone());
        }
        Ok(Some(table))
    }
}

/// Read one bearer token file.
///
/// The file must be a regular file (not a symlink) with no group or other
/// permission bits. One trailing line ending is stripped. The token must be
/// [`MIN_TOKEN_LEN`]..=[`MAX_TOKEN_LEN`] bytes of printable, non-space ASCII
/// (it is sent as `Authorization: Bearer <token>`). The content is never
/// part of an error.
///
/// # Errors
///
/// [`HubError::Read`], [`HubError::InsecurePermissions`] or
/// [`HubError::TokenFile`].
pub fn read_token_file(path: &Path) -> Result<SecretBytes, HubError> {
    let read_error = |source| HubError::Read {
        path: path.to_path_buf(),
        source,
    };
    let token_error = |reason| HubError::TokenFile {
        path: path.to_path_buf(),
        reason,
    };

    let link = fs::symlink_metadata(path).map_err(read_error)?;
    if link.file_type().is_symlink() {
        return Err(token_error("is a symlink"));
    }
    let file = File::open(path).map_err(read_error)?;
    // Checked on the opened descriptor, not the path, so a swap between the
    // check above and `open` cannot slip a wider file past this check.
    let metadata = file.metadata().map_err(read_error)?;
    if !metadata.is_file() {
        return Err(token_error("is not a regular file"));
    }
    let mode = metadata.permissions().mode() & 0o7777;
    if mode & 0o077 != 0 {
        return Err(HubError::InsecurePermissions {
            path: path.to_path_buf(),
            mode,
            expected: "0600 or stricter (no group or other bits)",
        });
    }

    // Final capacity up front so `read_to_end` never reallocates and leaves
    // token bytes behind in a freed buffer.
    let mut buffer = Vec::with_capacity(MAX_TOKEN_LEN + 3);
    let limit = u64::try_from(MAX_TOKEN_LEN + 2).unwrap_or(u64::MAX);
    let read = file.take(limit).read_to_end(&mut buffer);
    let raw = SecretBytes::from_vec(buffer);
    read.map_err(read_error)?;

    let bytes = raw.as_ref();
    let trimmed = bytes
        .strip_suffix(b"\r\n")
        .or_else(|| bytes.strip_suffix(b"\n"))
        .unwrap_or(bytes);
    if trimmed.len() < MIN_TOKEN_LEN {
        return Err(token_error("token is shorter than 16 bytes"));
    }
    if trimmed.len() > MAX_TOKEN_LEN {
        return Err(token_error("token is longer than 4096 bytes"));
    }
    if !trimmed.iter().all(u8::is_ascii_graphic) {
        return Err(token_error(
            "token must be printable ASCII without whitespace",
        ));
    }
    Ok(SecretBytes::copy_from_slice(trimmed))
}

fn invalid(message: String) -> HubError {
    HubError::ConfigInvalid(message)
}

fn principal(name: &str) -> Result<CgPrincipal, HubError> {
    let valid = !name.is_empty()
        && name.len() <= MAX_PRINCIPAL_LEN
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'@'));
    if valid {
        Ok(CgPrincipal::new(name))
    } else {
        Err(invalid(format!(
            "principal '{name}' must be 1..=128 bytes of [A-Za-z0-9._@-]"
        )))
    }
}

fn grants(raw: Vec<RawGrant>) -> Result<Vec<Grant>, HubError> {
    raw.into_iter()
        .map(|grant| {
            let namespace = KeyNamespace::new(&grant.namespace)
                .map_err(|_| invalid(format!("invalid namespace '{}'", grant.namespace)))?;
            if grant.ops.is_empty() {
                return Err(invalid(format!(
                    "grant for namespace '{}' has empty ops",
                    grant.namespace
                )));
            }
            let ops = grant
                .ops
                .iter()
                .fold(OpSet::NONE, |set, group| set.union(group.op_set()));
            Ok(Grant { namespace, ops })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    use crypt_guard_service::{OpKind, OpSet, Principal as CgPrincipal};

    use super::{HubConfig, read_token_file};
    use crate::error::HubError;
    use crate::test_support::{TestError, TestResult, ctx};

    const MINIMAL: &str = r#"
        [[peers]]
        uid = 1000
        principal = "harw-web"
        grants = [ { namespace = "app", ops = ["read-public", "encrypt"] } ]
    "#;

    fn write_file(path: &Path, content: &[u8], mode: u32) -> TestResult {
        let mut file = fs::File::create(path).map_err(ctx("create file"))?;
        file.write_all(content).map_err(ctx("write file"))?;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(ctx("set permissions"))?;
        Ok(())
    }

    #[test]
    fn minimal_config_parses_with_default_socket() -> TestResult {
        let config = HubConfig::from_toml_str(MINIMAL).map_err(ctx("parse"))?;
        assert_eq!(config.socket_path, Path::new(super::DEFAULT_SOCKET_PATH));
        assert_eq!(config.peers.len(), 1);
        let peer = config.peers.first().ok_or(TestError::Missing("peer"))?;
        assert_eq!(peer.uid, 1000);
        assert_eq!(peer.principal, CgPrincipal::new("harw-web"));
        let grant = peer.grants.first().ok_or(TestError::Missing("grant"))?;
        assert_eq!(grant.ops, OpSet::READ_PUBLIC.union(OpSet::ENCRYPT));
        Ok(())
    }

    #[test]
    fn policy_contains_configured_grants() -> TestResult {
        let config = HubConfig::from_toml_str(MINIMAL).map_err(ctx("parse"))?;
        let policy = config.policy();
        let namespace = crypt_guard_service::KeyNamespace::new("app").map_err(ctx("namespace"))?;
        let allowed = policy.allowed(&CgPrincipal::new("harw-web"), &namespace);
        assert!(allowed.contains(OpKind::Encrypt));
        assert!(allowed.contains(OpKind::Describe));
        assert!(!allowed.contains(OpKind::Decrypt));
        assert!(!allowed.contains(OpKind::Generate));
        Ok(())
    }

    #[test]
    fn unknown_top_level_field_is_rejected() {
        // Top-level keys must precede the first `[[peers]]` table.
        let text = format!("listen_tcp = \"0.0.0.0:1\"\n{MINIMAL}");
        assert!(matches!(
            HubConfig::from_toml_str(&text),
            Err(HubError::ConfigParse(_))
        ));
    }

    #[test]
    fn unknown_peer_field_is_rejected() {
        let text = r#"
            [[peers]]
            uid = 1000
            principal = "harw-web"
            gid = 1000
        "#;
        assert!(matches!(
            HubConfig::from_toml_str(text),
            Err(HubError::ConfigParse(_))
        ));
    }

    #[test]
    fn unknown_grant_field_is_rejected() {
        let text = r#"
            [[peers]]
            uid = 1000
            principal = "harw-web"
            grants = [ { namespace = "app", ops = ["all"], ttl = 5 } ]
        "#;
        assert!(matches!(
            HubConfig::from_toml_str(text),
            Err(HubError::ConfigParse(_))
        ));
    }

    #[test]
    fn unknown_op_group_is_rejected() {
        let text = r#"
            [[peers]]
            uid = 1000
            principal = "harw-web"
            grants = [ { namespace = "app", ops = ["everything"] } ]
        "#;
        assert!(matches!(
            HubConfig::from_toml_str(text),
            Err(HubError::ConfigParse(_))
        ));
    }

    #[test]
    fn duplicate_uid_is_rejected() {
        let text = r#"
            [[peers]]
            uid = 1000
            principal = "a"
            [[peers]]
            uid = 1000
            principal = "b"
        "#;
        assert!(matches!(
            HubConfig::from_toml_str(text),
            Err(HubError::ConfigInvalid(_))
        ));
    }

    #[test]
    fn invalid_namespace_and_principal_are_rejected() {
        let bad_namespace = r#"
            [[peers]]
            uid = 1000
            principal = "a"
            grants = [ { namespace = "../etc", ops = ["all"] } ]
        "#;
        assert!(matches!(
            HubConfig::from_toml_str(bad_namespace),
            Err(HubError::ConfigInvalid(_))
        ));
        let bad_principal = r#"
            [[peers]]
            uid = 1000
            principal = "has space"
        "#;
        assert!(matches!(
            HubConfig::from_toml_str(bad_principal),
            Err(HubError::ConfigInvalid(_))
        ));
    }

    #[test]
    fn empty_ops_and_relative_paths_are_rejected() {
        let empty_ops = r#"
            [[peers]]
            uid = 1000
            principal = "a"
            grants = [ { namespace = "app", ops = [] } ]
        "#;
        assert!(matches!(
            HubConfig::from_toml_str(empty_ops),
            Err(HubError::ConfigInvalid(_))
        ));
        let relative_socket = format!("socket_path = \"run/secure.sock\"\n{MINIMAL}");
        assert!(matches!(
            HubConfig::from_toml_str(&relative_socket),
            Err(HubError::ConfigInvalid(_))
        ));
        let relative_token = r#"
            [[bearer_tokens]]
            principal = "ops"
            token_file = "tokens/ops.token"
        "#;
        assert!(matches!(
            HubConfig::from_toml_str(relative_token),
            Err(HubError::ConfigInvalid(_))
        ));
    }

    #[test]
    fn empty_config_is_rejected() {
        assert!(matches!(
            HubConfig::from_toml_str(""),
            Err(HubError::ConfigInvalid(_))
        ));
    }

    #[test]
    fn group_writable_config_file_is_rejected() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("config.toml");
        write_file(&path, MINIMAL.as_bytes(), 0o664)?;
        assert!(matches!(
            HubConfig::load(&path),
            Err(HubError::InsecurePermissions { .. })
        ));
        write_file(&path, MINIMAL.as_bytes(), 0o640)?;
        let config = HubConfig::load(&path).map_err(ctx("load 0640 config"))?;
        assert_eq!(config.peers.len(), 1);
        Ok(())
    }

    #[test]
    fn token_file_with_group_bits_is_rejected() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("admin.token");
        write_file(&path, b"0123456789abcdef0123456789abcdef\n", 0o640)?;
        assert!(matches!(
            read_token_file(&path),
            Err(HubError::InsecurePermissions { .. })
        ));
        write_file(&path, b"0123456789abcdef0123456789abcdef\n", 0o644)?;
        assert!(matches!(
            read_token_file(&path),
            Err(HubError::InsecurePermissions { .. })
        ));
        Ok(())
    }

    #[test]
    fn token_file_0600_is_read_and_newline_stripped() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("admin.token");
        write_file(&path, b"0123456789abcdef0123456789abcdef\n", 0o600)?;
        let token = read_token_file(&path).map_err(ctx("read token"))?;
        assert_eq!(token.as_ref(), b"0123456789abcdef0123456789abcdef");
        Ok(())
    }

    #[test]
    fn short_or_non_printable_token_is_rejected() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("short.token");
        write_file(&path, b"short\n", 0o600)?;
        assert!(matches!(
            read_token_file(&path),
            Err(HubError::TokenFile { .. })
        ));
        write_file(&path, b"0123456789 abcdef0123456789abcdef", 0o600)?;
        assert!(matches!(
            read_token_file(&path),
            Err(HubError::TokenFile { .. })
        ));
        Ok(())
    }

    #[test]
    fn symlinked_token_file_is_rejected() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let target = dir.path().join("real.token");
        write_file(&target, b"0123456789abcdef0123456789abcdef", 0o600)?;
        let link = dir.path().join("link.token");
        std::os::unix::fs::symlink(&target, &link).map_err(ctx("symlink"))?;
        assert!(matches!(
            read_token_file(&link),
            Err(HubError::TokenFile { .. })
        ));
        Ok(())
    }

    #[test]
    fn token_error_never_contains_token_bytes() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("wide.token");
        write_file(&path, b"super-secret-token-value-123456", 0o644)?;
        let rendered = match read_token_file(&path) {
            Err(error) => error.to_string(),
            Ok(_) => return Err(TestError::Unexpected("wide token accepted".to_owned())),
        };
        assert!(!rendered.contains("super-secret"));
        Ok(())
    }

    #[test]
    fn load_bearer_tokens_builds_table() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("ops.token");
        write_file(&path, b"0123456789abcdef0123456789abcdef", 0o600)?;
        let text = format!(
            "[[bearer_tokens]]\nprincipal = \"ops\"\ntoken_file = \"{}\"\n\
             grants = [ {{ namespace = \"app\", ops = [\"all\"] }} ]\n",
            path.display()
        );
        let config = HubConfig::from_toml_str(&text).map_err(ctx("parse"))?;
        let table = config.load_bearer_tokens().map_err(ctx("load tokens"))?;
        let table = table.ok_or(TestError::Missing("bearer table"))?;
        assert_eq!(format!("{table:?}"), "BearerTokens([REDACTED; 1])");
        Ok(())
    }
}
