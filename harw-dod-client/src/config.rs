//! The Harw-DoD system configuration (`/etc/harw-dod/config.toml`), as seen
//! from harw.
//!
//! Parsing, validation and the trust walk (every path component opened
//! without following symlinks, root-owned, not group/world-writable) are
//! delegated to `harw_dod_config`, so harw and the daemons judge the same
//! file the same way. This module only classifies the outcome for display.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use harw_dod_config::{
    Config, ConfigError, ObservationScope, ResolvedObservationProfile, SYSTEM_CONFIG_PATH, Sensor,
};
use serde::{Deserialize, Serialize};

use crate::error::DodClientError;
use crate::fsio::sanitize;

/// Longest kept reason text, in bytes.
const MAX_REASON_BYTES: usize = 512;

/// Observation scope of the active profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProfileScope {
    /// Whole-host observation (explicitly selected).
    Host,
    /// A set of cgroup-v2 subtrees (paths relative to the cgroup mount).
    Cgroups {
        /// The configured cgroup paths.
        paths: Vec<PathBuf>,
        /// Whether descendants are included.
        include_descendants: bool,
    },
}

/// The explicitly selected observation profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveProfile {
    /// Profile id (`active_profile`).
    pub id: String,
    /// Observation scope.
    pub scope: ProfileScope,
    /// Enabled sensors (`exec`, `tcp-connect`).
    pub sensors: Vec<String>,
    /// Number of allowed egress CIDRs.
    pub egress_allow_cidrs: usize,
}

/// Outcome of reading the system configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConfigReport {
    /// The configuration parsed and validated.
    Valid {
        /// The file that was read.
        path: PathBuf,
        /// BLAKE3 digest of the exact bytes (hex).
        config_digest: String,
        /// The selected profile; `None` when `active_profile` is unset
        /// (observation must not be enabled then).
        active_profile: Option<ActiveProfile>,
    },
    /// The file does not exist.
    Missing {
        /// The path that was looked up.
        path: PathBuf,
    },
    /// The file exists but does not validate.
    Invalid {
        /// The file that was read.
        path: PathBuf,
        /// Sanitized validation error.
        reason: String,
    },
    /// The path or a component of it is a symlink, not root-owned, or
    /// group/world-writable — the daemons would refuse it, so harw does too.
    Untrusted {
        /// The refused path.
        path: PathBuf,
        /// Sanitized description.
        reason: String,
    },
    /// This process may not read the file (typical for non-root operators).
    Unreadable {
        /// The path.
        path: PathBuf,
        /// Sanitized description.
        reason: String,
    },
}

impl ConfigReport {
    /// `true` for a valid configuration with a selected profile.
    #[must_use]
    pub const fn is_activatable(&self) -> bool {
        matches!(
            self,
            Self::Valid {
                active_profile: Some(_),
                ..
            }
        )
    }
}

/// Reads and classifies the system configuration.
///
/// `path = None` reads [`SYSTEM_CONFIG_PATH`] through
/// `harw_dod_config::load_system_config`; an explicit path goes through
/// `harw_dod_config::load_config` (absolute paths only). Symlinks are never
/// followed.
///
/// # Errors
/// [`DodClientError::Io`] for I/O failures other than "not found" and
/// "permission denied" (which are reported as [`ConfigReport::Missing`] and
/// [`ConfigReport::Unreadable`]).
pub fn read_system_config(path: Option<&Path>) -> Result<ConfigReport, DodClientError> {
    let shown = path.map_or_else(|| PathBuf::from(SYSTEM_CONFIG_PATH), Path::to_path_buf);
    if let Some(link) = first_symlink(&shown) {
        return Ok(ConfigReport::Untrusted {
            reason: format!("`{}` is a symlink", sanitize_path(&link)),
            path: shown,
        });
    }
    let loaded = match path {
        Some(path) => harw_dod_config::load_config(path),
        None => harw_dod_config::load_system_config(),
    };
    classify(shown, loaded)
}

/// Maps a load result onto a [`ConfigReport`].
fn classify(
    path: PathBuf,
    loaded: Result<Config, ConfigError>,
) -> Result<ConfigReport, DodClientError> {
    let config = match loaded {
        Ok(config) => config,
        Err(ConfigError::Io(error)) => {
            return match error.kind() {
                io::ErrorKind::NotFound => Ok(ConfigReport::Missing { path }),
                io::ErrorKind::PermissionDenied => Ok(ConfigReport::Unreadable {
                    reason: reason(&error),
                    path,
                }),
                _ => Err(DodClientError::Io {
                    path,
                    source: error,
                }),
            };
        }
        Err(
            error @ (ConfigError::UntrustedSystemPath { .. }
            | ConfigError::UntrustedExplicitPath { .. }
            | ConfigError::UnexpectedSystemPathType { .. }
            | ConfigError::UnexpectedExplicitPathType { .. }
            | ConfigError::InvalidExplicitPath { .. }),
        ) => {
            return Ok(ConfigReport::Untrusted {
                reason: reason(&error),
                path,
            });
        }
        Err(error) => {
            return Ok(ConfigReport::Invalid {
                reason: reason(&error),
                path,
            });
        }
    };
    let config_digest = config.config_digest().to_string();
    match config.resolve_active() {
        Ok(profile) => Ok(ConfigReport::Valid {
            path,
            config_digest,
            active_profile: Some(active_profile(&profile)),
        }),
        Err(ConfigError::MissingActiveProfile) => Ok(ConfigReport::Valid {
            path,
            config_digest,
            active_profile: None,
        }),
        Err(error) => Ok(ConfigReport::Invalid {
            reason: reason(&error),
            path,
        }),
    }
}

fn active_profile(profile: &ResolvedObservationProfile) -> ActiveProfile {
    let scope = match profile.scope() {
        ObservationScope::Host => ProfileScope::Host,
        ObservationScope::Cgroups(scope) => ProfileScope::Cgroups {
            paths: scope
                .paths()
                .iter()
                .map(|path| path.as_path().to_path_buf())
                .collect(),
            include_descendants: scope.include_descendants(),
        },
    };
    let sensors = profile
        .sensors()
        .iter()
        .map(|sensor| match sensor {
            Sensor::Exec => "exec".to_owned(),
            Sensor::TcpConnect => "tcp-connect".to_owned(),
        })
        .collect();
    ActiveProfile {
        id: profile.profile_id().as_str().to_owned(),
        scope,
        sensors,
        egress_allow_cidrs: profile.egress_allow_cidrs().len(),
    }
}

/// The first component of `path` (from the root down) that is a symlink.
///
/// Advisory only — the trust decision is the descriptor walk inside
/// `harw_dod_config`; this check just names the symlink instead of an
/// opaque `ELOOP`.
fn first_symlink(path: &Path) -> Option<PathBuf> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => return Some(current),
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    None
}

fn reason(error: &dyn std::fmt::Display) -> String {
    sanitize(&error.to_string(), MAX_REASON_BYTES, ' ')
}

fn sanitize_path(path: &Path) -> String {
    sanitize(&path.display().to_string(), MAX_REASON_BYTES, '?')
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const CGROUP_PROFILE: &str = r#"
schema_version = 1
mode = "observe"
active_profile = "harw-jobs"

[profiles.harw-jobs]
scope = "cgroups"
cgroup_paths = ["/harw.slice"]
include_descendants = true
sensors = ["tcp-connect", "exec"]
egress_allow_cidrs = ["192.0.2.0/24"]
"#;

    fn path() -> PathBuf {
        PathBuf::from("/etc/harw-dod/config.toml")
    }

    #[test]
    fn valid_config_reports_the_active_profile() -> TestResult {
        let report = classify(path(), Config::from_toml(CGROUP_PROFILE))?;
        let ConfigReport::Valid {
            config_digest,
            active_profile: Some(profile),
            ..
        } = &report
        else {
            return Err(format!("expected a valid report, got {report:?}").into());
        };
        assert_eq!(config_digest.len(), 64);
        assert_eq!(profile.id, "harw-jobs");
        assert_eq!(profile.sensors, ["exec", "tcp-connect"]);
        assert_eq!(profile.egress_allow_cidrs, 1);
        assert_eq!(
            profile.scope,
            ProfileScope::Cgroups {
                paths: vec![PathBuf::from("/harw.slice")],
                include_descendants: true
            }
        );
        assert!(report.is_activatable());
        Ok(())
    }

    #[test]
    fn valid_config_without_selection_is_not_activatable() -> TestResult {
        let unselected = CGROUP_PROFILE.replace("active_profile = \"harw-jobs\"\n", "");
        let report = classify(path(), Config::from_toml(&unselected))?;
        assert!(matches!(
            report,
            ConfigReport::Valid {
                active_profile: None,
                ..
            }
        ));
        assert!(!report.is_activatable());
        Ok(())
    }

    #[test]
    fn invalid_configs_carry_a_sanitized_reason() -> TestResult {
        let unknown_profile = CGROUP_PROFILE.replace(
            "active_profile = \"harw-jobs\"",
            "active_profile = \"other\"",
        );
        for input in [
            "schema_version = 2\nmode = \"observe\"\n[profiles.a]\nscope = \"host\"\nsensors = [\"exec\"]\n",
            "not toml at all = = =",
            unknown_profile.as_str(),
        ] {
            let report = classify(path(), Config::from_toml(input))?;
            let ConfigReport::Invalid { reason, .. } = &report else {
                return Err(format!("expected invalid, got {report:?}").into());
            };
            assert!(!reason.is_empty());
            assert!(!reason.contains('\n'));
        }
        Ok(())
    }

    #[test]
    fn missing_path_is_missing() -> TestResult {
        let missing = PathBuf::from(format!(
            "/harw-dod-client-test-missing-{}/config.toml",
            std::process::id()
        ));
        let report = read_system_config(Some(&missing))?;
        // Rootless containers can expose `/` as non-root-owned. Preserve
        // the production trust rejection instead of assuming host ownership.
        use std::os::unix::fs::MetadataExt as _;
        let root = fs::metadata("/")?;
        if root.uid() == 0 && root.mode() & 0o022 == 0 {
            assert_eq!(report, ConfigReport::Missing { path: missing });
        } else {
            assert!(matches!(report, ConfigReport::Untrusted { path, .. } if path == missing));
        }
        Ok(())
    }

    #[test]
    fn not_found_load_error_is_classified_as_missing() -> TestResult {
        let missing = path();
        let report = classify(
            missing.clone(),
            Err(ConfigError::Io(io::Error::from(io::ErrorKind::NotFound))),
        )?;
        assert_eq!(report, ConfigReport::Missing { path: missing });
        Ok(())
    }

    #[test]
    fn tempdir_files_and_symlinks_are_untrusted() -> TestResult {
        let dir = tempfile::tempdir()?;
        // A world-writable directory in the path is untrusted for every
        // user, including root, so the outcome does not depend on TMPDIR.
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o777))?;
        let file = dir.path().join("config.toml");
        fs::write(&file, CGROUP_PROFILE)?;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644))?;
        assert!(matches!(
            read_system_config(Some(&file))?,
            ConfigReport::Untrusted { .. }
        ));
        let link = dir.path().join("link.toml");
        symlink(&file, &link)?;
        let report = read_system_config(Some(&link))?;
        let ConfigReport::Untrusted { reason, .. } = &report else {
            return Err(format!("expected untrusted, got {report:?}").into());
        };
        assert!(reason.contains("symlink"), "{reason}");
        assert!(matches!(
            read_system_config(Some(Path::new("relative/config.toml")))?,
            ConfigReport::Untrusted { .. }
        ));
        Ok(())
    }

    #[test]
    fn report_serializes_with_a_state_tag() -> TestResult {
        let json = serde_json::to_value(ConfigReport::Missing { path: path() })?;
        assert_eq!(json["state"], "missing");
        Ok(())
    }
}
