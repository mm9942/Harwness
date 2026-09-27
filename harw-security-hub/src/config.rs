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
    /// # Errors
    /// [`HubError::Read`] plus everything from [`Self::from_toml_str`].
    pub fn load(path: &Path) -> Result<Self, HubError> {
        let text = std::fs::read_to_string(path).map_err(|source| HubError::Read {
            path: path.to_path_buf(),
            source,
        })?;
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

#[cfg(test)]
mod tests {
    use super::{DEFAULT_SOCKET_PATH, HubConfig};
    use crate::test_support::{TestResult, ctx};
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
}
