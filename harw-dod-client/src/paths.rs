//! Where the Harw-DoD export lives.
//!
//! One resolver for every harw consumer (CLI, operations, TUI, the
//! `security_signals` context source), so they never disagree on the path.

use std::fs;
use std::io;
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
    /// [`SYSTEM_EXPORT_DIR`] exists but this process may not list it
    /// (typically: the user is not in the `harw-dod-readers` group). The
    /// paths still point at the system directory, so every read reports
    /// *unreadable* instead of silently showing the home fallback, which
    /// anything running as the harw user could have written.
    SystemUnreadable,
    /// The fallback below the harw home.
    Home,
}

impl ExportSource {
    /// `true` when the export is written only by the sentinel (the system
    /// directory is `0750 harw-sentinel:harw-dod-readers`) or was named by a
    /// trusted config layer. `Home` is below the harw home, where any
    /// process of the harw user can write; `SystemUnreadable` could not be
    /// read at all. Consumers that present findings as Harw-DoD facts show
    /// the source for both.
    #[must_use]
    pub const fn is_verified(self) -> bool {
        matches!(self, Self::Override | Self::System)
    }

    /// Operator hint for a source that is not [`Self::is_verified`].
    #[must_use]
    pub const fn hint(self) -> Option<&'static str> {
        match self {
            Self::SystemUnreadable => Some(
                "the system export exists but cannot be read; add the user to the harw-dod-readers group",
            ),
            Self::Home => Some(
                "export below the harw home, not the system sentinel export; treat findings as unverified",
            ),
            Self::Override | Self::System => None,
        }
    }
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
///
/// A system directory that exists but cannot be listed is **not** replaced
/// by the home fallback: the result is [`ExportSource::SystemUnreadable`]
/// with the system paths, so the caller reports an unreadable export rather
/// than findings from a directory the harw user can write.
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
    match probe_dir(system_dir) {
        DirProbe::Listable => return ExportPaths::in_dir(system_dir, ExportSource::System),
        DirProbe::Unreadable => {
            return ExportPaths::in_dir(system_dir, ExportSource::SystemUnreadable);
        }
        DirProbe::Absent => {}
    }
    let mut home = harw_home.to_path_buf();
    for part in HOME_EXPORT_SUBDIR {
        home.push(part);
    }
    ExportPaths::in_dir(&home, ExportSource::Home)
}

/// What the system export directory looks like from this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirProbe {
    /// Missing, a symlink or not a directory.
    Absent,
    /// A real directory this process can list.
    Listable,
    /// A real directory this process cannot list (permission or any other
    /// error): reported, never silently replaced.
    Unreadable,
}

fn probe_dir(dir: &Path) -> DirProbe {
    if !fs::symlink_metadata(dir).is_ok_and(|meta| meta.file_type().is_dir()) {
        return DirProbe::Absent;
    }
    match fs::read_dir(dir) {
        Ok(_) => DirProbe::Listable,
        Err(error) if error.kind() == io::ErrorKind::NotFound => DirProbe::Absent,
        Err(_) => DirProbe::Unreadable,
    }
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

    #[test]
    fn unreadable_system_dir_is_reported_instead_of_the_home_fallback() -> TestResult {
        use std::os::unix::fs::PermissionsExt as _;

        let scratch = tempfile::tempdir()?;
        let system = scratch.path().join("export");
        fs::create_dir(&system)?;
        fs::set_permissions(&system, fs::Permissions::from_mode(0o000))?;
        let paths = resolve_export_paths_with(None, Path::new("/home/op/.harw"), &system);
        let listable = fs::read_dir(&system).is_ok();
        fs::set_permissions(&system, fs::Permissions::from_mode(0o750))?;
        if listable {
            // Running with CAP_DAC_READ_SEARCH (root): the directory is
            // readable after all, so the ordinary rule applies.
            assert_eq!(paths.source, ExportSource::System);
            return Ok(());
        }
        assert_eq!(paths.source, ExportSource::SystemUnreadable);
        assert_eq!(paths.findings, system.join(FINDINGS_FILE));
        assert_eq!(paths.health, system.join(HEALTH_FILE));
        assert!(!paths.source.is_verified());
        assert!(
            paths
                .source
                .hint()
                .is_some_and(|hint| hint.contains("harw-dod-readers"))
        );
        Ok(())
    }

    #[test]
    fn only_override_and_system_count_as_verified() {
        assert!(ExportSource::Override.is_verified());
        assert!(ExportSource::System.is_verified());
        assert!(!ExportSource::Home.is_verified());
        assert!(!ExportSource::SystemUnreadable.is_verified());
        assert!(ExportSource::Home.hint().is_some());
        assert!(ExportSource::System.hint().is_none());
    }
}
