//! Daemon configuration (`/etc/harw-netsec/config.toml`).
//!
//! # Principles (masterplan §39, drift report D1)
//! - The daemon owns its configuration; Harwness only references the socket.
//! - Production defaults are compiled constants for the standard system
//!   paths. There is **no** fallback to `$HOME`, the working directory or a
//!   project path; tests pass explicit temp paths.
//! - `allowed_uids` has no default: an empty allowlist is a configuration
//!   error, not "allow everyone".
//!
//! ```toml
//! # socket = "/run/harw/infra/network.sock"   # default
//! # state_dir = "/var/lib/harw-netsec"         # default
//! allowed_uids = [0, 990]
//! # max_body_bytes = 65536
//! # max_nodes = 4096
//! # max_connections = 64
//!
//! [[zones]]
//! id = "local"
//! display_name = "Local host"
//! ```

use std::collections::{BTreeSet, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{NetsecError, NetsecResult};
use crate::ids::ZoneId;
use crate::model::Zone;

/// Default configuration file.
pub const DEFAULT_CONFIG_PATH: &str = "/etc/harw-netsec/config.toml";
/// Default control socket (`/run/harw/infra/`, crypto drift report D1).
pub const DEFAULT_SOCKET_PATH: &str = "/run/harw/infra/network.sock";
/// Default persistent state directory.
pub const DEFAULT_STATE_DIR: &str = "/var/lib/harw-netsec";
/// Default request body limit.
pub const DEFAULT_MAX_BODY_BYTES: usize = 64 * 1024;
/// Hard upper bound for `max_body_bytes`.
pub const HARD_MAX_BODY_BYTES: usize = 1024 * 1024;
/// Default node capacity.
pub const DEFAULT_MAX_NODES: usize = 4096;
/// Default concurrent connection limit.
pub const DEFAULT_MAX_CONNECTIONS: usize = 64;
/// Upper bound for the configuration file size.
const MAX_CONFIG_BYTES: u64 = 256 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default)]
    socket: Option<PathBuf>,
    #[serde(default)]
    state_dir: Option<PathBuf>,
    allowed_uids: Vec<u32>,
    #[serde(default)]
    max_body_bytes: Option<usize>,
    #[serde(default)]
    max_nodes: Option<usize>,
    #[serde(default)]
    max_connections: Option<usize>,
    #[serde(default)]
    zones: Vec<Zone>,
}

/// Validated daemon configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetsecConfig {
    /// Path of the control socket (ignored under socket activation).
    pub socket_path: PathBuf,
    /// Persistent state directory.
    pub state_dir: PathBuf,
    /// Peer uids (`SO_PEERCRED`) allowed to use the API.
    pub allowed_uids: BTreeSet<u32>,
    /// Maximum accepted request body in bytes.
    pub max_body_bytes: usize,
    /// Maximum number of registered nodes.
    pub max_nodes: usize,
    /// Maximum number of concurrently served connections.
    pub max_connections: usize,
    /// Configured trust zones (seeded into the store at start-up).
    pub zones: Vec<Zone>,
}

impl NetsecConfig {
    /// Builds a configuration with explicit paths and allowlist and the
    /// default limits plus one zone `local`. Intended for tests and
    /// embedding.
    ///
    /// # Errors
    /// [`NetsecError::Config`] if the result fails validation.
    pub fn new(
        socket_path: impl Into<PathBuf>,
        state_dir: impl Into<PathBuf>,
        allowed_uids: impl IntoIterator<Item = u32>,
    ) -> NetsecResult<Self> {
        let config = Self {
            socket_path: socket_path.into(),
            state_dir: state_dir.into(),
            allowed_uids: allowed_uids.into_iter().collect(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            max_nodes: DEFAULT_MAX_NODES,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            zones: vec![Zone {
                id: ZoneId::parse("local")?,
                display_name: "Local host".to_owned(),
            }],
        };
        config.validate()?;
        Ok(config)
    }

    /// Parses and validates TOML text.
    ///
    /// # Errors
    /// [`NetsecError::Config`].
    pub fn from_toml_str(text: &str) -> NetsecResult<Self> {
        let raw: RawConfig = toml::from_str(text).map_err(|error| NetsecError::Config {
            reason: error.to_string(),
        })?;
        let config = Self {
            socket_path: raw
                .socket
                .unwrap_or_else(|| PathBuf::from(DEFAULT_SOCKET_PATH)),
            state_dir: raw
                .state_dir
                .unwrap_or_else(|| PathBuf::from(DEFAULT_STATE_DIR)),
            allowed_uids: raw.allowed_uids.into_iter().collect(),
            max_body_bytes: raw.max_body_bytes.unwrap_or(DEFAULT_MAX_BODY_BYTES),
            max_nodes: raw.max_nodes.unwrap_or(DEFAULT_MAX_NODES),
            max_connections: raw.max_connections.unwrap_or(DEFAULT_MAX_CONNECTIONS),
            zones: raw.zones,
        };
        config.validate()?;
        Ok(config)
    }

    /// Reads, parses and validates the configuration file at `path`.
    ///
    /// # Errors
    /// [`NetsecError::Config`] (missing, too large or invalid file).
    pub fn load(path: &Path) -> NetsecResult<Self> {
        let config_error = |reason: String| NetsecError::Config { reason };
        let file = std::fs::File::open(path)
            .map_err(|error| config_error(format!("cannot open {}: {error}", path.display())))?;
        let mut text = String::new();
        file.take(MAX_CONFIG_BYTES + 1)
            .read_to_string(&mut text)
            .map_err(|error| config_error(format!("cannot read {}: {error}", path.display())))?;
        if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_CONFIG_BYTES {
            return Err(config_error(format!(
                "{} exceeds {MAX_CONFIG_BYTES} bytes",
                path.display()
            )));
        }
        Self::from_toml_str(&text)
    }

    /// Checks every constraint.
    ///
    /// # Errors
    /// [`NetsecError::Config`] naming the first violation.
    pub fn validate(&self) -> NetsecResult<()> {
        let fail = |reason: &str| {
            Err(NetsecError::Config {
                reason: reason.to_owned(),
            })
        };
        if !self.socket_path.is_absolute() {
            return fail("socket must be an absolute path");
        }
        if !self.state_dir.is_absolute() {
            return fail("state_dir must be an absolute path");
        }
        if self.allowed_uids.is_empty() {
            return fail("allowed_uids must name at least one uid");
        }
        if self.max_body_bytes == 0 || self.max_body_bytes > HARD_MAX_BODY_BYTES {
            return fail("max_body_bytes must be in 1..=1048576");
        }
        if self.max_nodes == 0 {
            return fail("max_nodes must be at least 1");
        }
        if self.max_connections == 0 {
            return fail("max_connections must be at least 1");
        }
        if self.zones.is_empty() {
            return fail("at least one [[zones]] entry is required");
        }
        let mut seen = HashSet::new();
        for zone in &self.zones {
            zone.validate().map_err(|error| NetsecError::Config {
                reason: format!("zone {}: {error}", zone.id),
            })?;
            if !seen.insert(zone.id.as_str()) {
                return Err(NetsecError::Config {
                    reason: format!("duplicate zone {}", zone.id),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    const MINIMAL: &str = r#"
allowed_uids = [0]
[[zones]]
id = "local"
display_name = "Local host"
"#;

    #[test]
    fn test_minimal_config_uses_compiled_defaults() -> TestResult {
        let config = NetsecConfig::from_toml_str(MINIMAL)?;
        assert_eq!(config.socket_path, PathBuf::from(DEFAULT_SOCKET_PATH));
        assert_eq!(config.state_dir, PathBuf::from(DEFAULT_STATE_DIR));
        assert_eq!(config.max_body_bytes, DEFAULT_MAX_BODY_BYTES);
        assert!(config.allowed_uids.contains(&0));
        Ok(())
    }

    #[test]
    fn test_default_socket_lives_under_run_harw_infra() {
        assert!(DEFAULT_SOCKET_PATH.starts_with("/run/harw/infra/"));
    }

    fn expect_config_error(text: &str) -> TestResult {
        match NetsecConfig::from_toml_str(text) {
            Err(NetsecError::Config { .. }) => Ok(()),
            other => Err(TestError::Unexpected(format!("{text}: {other:?}"))),
        }
    }

    #[test]
    fn test_invalid_configs_are_rejected() -> TestResult {
        let zone = "[[zones]]\nid = \"local\"\ndisplay_name = \"L\"\n";
        expect_config_error(&format!("allowed_uids = []\n{zone}"))?;
        expect_config_error("allowed_uids = [0]\n")?;
        expect_config_error(&format!("allowed_uids = [0]\nunknown = 1\n{zone}"))?;
        expect_config_error(&format!(
            "allowed_uids = [0]\nsocket = \"rel.sock\"\n{zone}"
        ))?;
        expect_config_error(&format!("allowed_uids = [0]\nstate_dir = \"var\"\n{zone}"))?;
        expect_config_error(&format!("allowed_uids = [0]\nmax_body_bytes = 0\n{zone}"))?;
        expect_config_error(&format!(
            "allowed_uids = [0]\nmax_body_bytes = 9999999\n{zone}"
        ))?;
        expect_config_error(&format!("allowed_uids = [0]\n{zone}{zone}"))?;
        expect_config_error("allowed_uids = [0]\n[[zones]]\nid = \"a/b\"\ndisplay_name = \"x\"\n")?;
        Ok(())
    }

    #[test]
    fn test_missing_file_is_a_config_error() -> TestResult {
        let temp = tempfile::tempdir()?;
        match NetsecConfig::load(&temp.path().join("absent.toml")) {
            Err(NetsecError::Config { .. }) => Ok(()),
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }

    #[test]
    fn test_load_reads_file() -> TestResult {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("config.toml");
        std::fs::write(&path, MINIMAL)?;
        let config = NetsecConfig::load(&path)?;
        assert_eq!(config.zones.len(), 1);
        Ok(())
    }

    #[test]
    fn test_new_builds_valid_config() -> TestResult {
        let config = NetsecConfig::new("/tmp/n.sock", "/tmp/state", [1000])?;
        assert_eq!(config.zones.len(), 1);
        assert!(NetsecConfig::new("/tmp/n.sock", "/tmp/state", []).is_err());
        Ok(())
    }
}
