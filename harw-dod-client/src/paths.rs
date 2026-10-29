//! Where the Harw-DoD export lives.
//!
//! One resolver for every harw consumer (CLI, operations, TUI, the
//! `security_signals` context source), so they never disagree on the path.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name of the findings export (JSON Lines, one finding per line).
pub const FINDINGS_FILE: &str = "findings.jsonl";

/// File name of the sentinel health report (one JSON document).
pub const HEALTH_FILE: &str = "health.json";

/// Export directory written by the system-installed sentinel unit.
pub const SYSTEM_EXPORT_DIR: &str = "/var/lib/harw-dod/export";

/// Export directory below the harw home (`<home>/dod/export`), the
/// historical location read by `security_signals`.
pub const HOME_EXPORT_SUBDIR: [&str; 2] = ["dod", "export"];

/// Which rule selected the export directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportSource {
    /// The explicit `[dod] export_dir` override.
    Override,
    /// The readable system export directory ([`SYSTEM_EXPORT_DIR`]).
    System,
    /// The fallback below the harw home.
    Home,
}

/// The two files of the Harw-DoD export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportPaths {
    /// The findings export (`findings.jsonl`).
    pub findings: PathBuf,
    /// The sentinel health report (`health.json`).
    pub health: PathBuf,
    /// Which resolution rule produced these paths.
    pub source: ExportSource,
}

impl ExportPaths {
    /// Both files inside `dir`.
    #[must_use]
    pub fn in_dir(dir: &Path, source: ExportSource) -> Self {
        Self {
            findings: dir.join(FINDINGS_FILE),
            health: dir.join(HEALTH_FILE),
            source,
        }
    }
}

/// Resolves the export directory.
///
/// Order: 1. the explicit `[dod] export_dir` override (a relative override
/// is taken relative to `harw_home`), 2. [`SYSTEM_EXPORT_DIR`] if it is a
/// directory this process can list, 3. `<harw_home>/dod/export`.
#[must_use]
pub fn resolve_export_paths(override_: Option<&Path>, harw_home: &Path) -> ExportPaths {
    resolve_export_paths_with(override_, harw_home, Path::new(SYSTEM_EXPORT_DIR))
}

/// [`resolve_export_paths`] with an injectable system export directory
/// (tests, staged installs).
#[must_use]
pub fn resolve_export_paths_with(
    override_: Option<&Path>,
    harw_home: &Path,
    system_dir: &Path,
) -> ExportPaths {
    if let Some(dir) = override_ {
        let dir = if dir.is_absolute() {
            dir.to_path_buf()
        } else {
            harw_home.join(dir)
        };
        return ExportPaths::in_dir(&dir, ExportSource::Override);
    }
    if is_listable_dir(system_dir) {
        return ExportPaths::in_dir(system_dir, ExportSource::System);
    }
    let mut home = harw_home.to_path_buf();
    for part in HOME_EXPORT_SUBDIR {
        home.push(part);
    }
    ExportPaths::in_dir(&home, ExportSource::Home)
}

/// `true` when `dir` is a real directory (not a symlink) that can be listed.
fn is_listable_dir(dir: &Path) -> bool {
    fs::symlink_metadata(dir).is_ok_and(|meta| meta.file_type().is_dir())
        && fs::read_dir(dir).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn override_wins_and_relative_override_is_below_home() -> TestResult {
        let system = tempfile::tempdir()?;
        let home = Path::new("/home/op/.harw");
        let explicit =
            resolve_export_paths_with(Some(Path::new("/srv/dod-export")), home, system.path());
        assert_eq!(explicit.source, ExportSource::Override);
        assert_eq!(
            explicit.findings,
            PathBuf::from("/srv/dod-export/findings.jsonl")
        );
        assert_eq!(
            explicit.health,
            PathBuf::from("/srv/dod-export/health.json")
        );
        let relative = resolve_export_paths_with(Some(Path::new("export")), home, system.path());
        assert_eq!(
            relative.findings,
            PathBuf::from("/home/op/.harw/export/findings.jsonl")
        );
        Ok(())
    }

    #[test]
    fn readable_system_dir_beats_home() -> TestResult {
        let system = tempfile::tempdir()?;
        let paths = resolve_export_paths_with(None, Path::new("/home/op/.harw"), system.path());
        assert_eq!(paths.source, ExportSource::System);
        assert_eq!(paths.findings, system.path().join(FINDINGS_FILE));
        Ok(())
    }

    #[test]
    fn missing_or_symlinked_system_dir_falls_back_to_home() -> TestResult {
        let scratch = tempfile::tempdir()?;
        let home = Path::new("/home/op/.harw");
        let missing = resolve_export_paths_with(None, home, &scratch.path().join("absent"));
        assert_eq!(missing.source, ExportSource::Home);
        assert_eq!(
            missing.findings,
            PathBuf::from("/home/op/.harw/dod/export/findings.jsonl")
        );
        let real = scratch.path().join("real");
        fs::create_dir(&real)?;
        let link = scratch.path().join("link");
        std::os::unix::fs::symlink(&real, &link)?;
        assert_eq!(
            resolve_export_paths_with(None, home, &link).source,
            ExportSource::Home
        );
        Ok(())
    }
}
