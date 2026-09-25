//! `~/.harw/bin`: installed compiled agents and their versions.
//!
//! # Layout
//! ```text
//! ~/.harw/bin/
//!   <name>                                  → symlink to the current version (copy where symlinks fail)
//!   .versions/<name>/current                the directory name of the current version
//!   .versions/<name>/<version>-<digest12>/<name>        the binary (or <name>.harwa for --artifact-only)
//!   .versions/<name>/<version>-<digest12>/build.json    the build record
//!   .runners/<target>/<harw-version>/harw-agent-runner  runner copy from `make install`
//! ```
//!
//! Every build is kept as a version; `~/.harw/bin/<name>` always points at
//! the current one and is switched atomically (a new symlink renamed over
//! the old one). `harw agent use` switches it, `harw agent clean` removes
//! old versions (never the current one).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::CompileError;

/// Name of the versions directory under the bin directory.
pub const VERSIONS_DIR: &str = ".versions";

/// Marker file with the current version's directory name.
pub const CURRENT_MARKER: &str = "current";

/// Build record file name.
pub const BUILD_RECORD_FILE: &str = "build.json";

/// Schema label of [`BuildRecord`].
pub const BUILD_RECORD_SCHEMA: &str = "harwness.agent-build/v1";

/// What `build.json` records about one build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildRecord {
    /// Always [`BUILD_RECORD_SCHEMA`].
    pub schema: String,
    /// Binary name.
    pub name: String,
    /// Definition ID.
    pub definition_id: String,
    /// Definition version.
    pub version: String,
    /// Artifact digest (hex).
    pub artifact_digest: String,
    /// v7 snapshot digest of the compiled IR (hex).
    pub snapshot: String,
    /// v7 snapshot of the lowered definition before the compiler passes
    /// (compared by `harw agent list` to detect stale builds).
    #[serde(default)]
    pub source_snapshot: String,
    /// Built-in interfaces.
    pub interfaces: Vec<String>,
    /// harw version of the compiler.
    pub harw_version: String,
    /// Build time, RFC 3339.
    pub built_at: String,
    /// Build time, Unix seconds.
    pub built_at_unix: i64,
    /// Install order within this agent's versions (set by
    /// [`BinDir::install`]; the primary sort key).
    #[serde(default)]
    pub sequence: u64,
    /// `artifact`, `artifact-only` or `native`.
    pub backend: String,
    /// Target triple.
    pub target: String,
    /// File name inside the version directory.
    pub file: String,
}

/// One installed version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstalledVersion {
    /// Directory name (`<version>-<digest12>`).
    pub dir_name: String,
    /// The version directory.
    pub dir: PathBuf,
    /// The installed file.
    pub file: PathBuf,
    /// The record.
    pub record: BuildRecord,
    /// Whether `~/.harw/bin/<name>` points at it.
    pub current: bool,
}

/// A removed (or, dry run, removable) version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemovedVersion {
    /// Agent name.
    pub name: String,
    /// Directory name.
    pub dir_name: String,
    /// Bytes freed.
    pub bytes: u64,
}

/// The bin directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinDir {
    root: PathBuf,
}

impl BinDir {
    /// The bin directory at `root` (`~/.harw/bin`).
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// The root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `~/.harw/bin/<name>`.
    #[must_use]
    pub fn link(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    /// `~/.harw/bin/.versions/<name>`.
    #[must_use]
    pub fn versions_dir(&self, name: &str) -> PathBuf {
        self.root.join(VERSIONS_DIR).join(name)
    }

    /// `<version>-<first 12 hex of the digest>`.
    #[must_use]
    pub fn version_dir_name(version: &str, digest: &str) -> String {
        let short: String = digest.chars().take(12).collect();
        format!("{version}-{short}")
    }

    /// Installs `bytes` as a new version and, if `make_current`, points
    /// `~/.harw/bin/<name>` at it.
    ///
    /// # Description
    /// The file is written with mode `0o755` when `executable`. Installing
    /// the same version and digest again rewrites it (same bytes) and
    /// refreshes the record.
    ///
    /// # Errors
    /// [`CompileError::Io`] on a write failure.
    pub fn install(
        &self,
        record: &BuildRecord,
        bytes: &[u8],
        executable: bool,
        make_current: bool,
    ) -> Result<InstalledVersion, CompileError> {
        let mut record = record.clone();
        record.sequence = self
            .versions(&record.name)?
            .iter()
            .map(|version| version.record.sequence)
            .max()
            .map_or(1, |last| last + 1);
        let record = &record;
        let dir_name = Self::version_dir_name(&record.version, &record.artifact_digest);
        let dir = self.versions_dir(&record.name).join(&dir_name);
        std::fs::create_dir_all(&dir)
            .map_err(CompileError::io(format!("create {}", dir.display())))?;
        let file = dir.join(&record.file);
        if executable {
            harw_agent_artifact::write_executable(&file, bytes)?;
        } else {
            std::fs::write(&file, bytes)
                .map_err(CompileError::io(format!("write {}", file.display())))?;
        }
        let json = serde_json::to_vec_pretty(record)
            .map_err(|error| CompileError::Other(format!("serialize build record: {error}")))?;
        let record_path = dir.join(BUILD_RECORD_FILE);
        std::fs::write(&record_path, json)
            .map_err(CompileError::io(format!("write {}", record_path.display())))?;
        if make_current {
            self.set_current(&record.name, &dir_name)?;
        }
        Ok(InstalledVersion {
            current: make_current,
            dir_name,
            dir,
            file,
            record: record.clone(),
        })
    }

    /// Names with at least one installed version, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        let Ok(read_dir) = std::fs::read_dir(self.root.join(VERSIONS_DIR)) else {
            return Vec::new();
        };
        let mut names: Vec<String> = read_dir
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        names.sort();
        names
    }

    /// The current version's directory name, if any.
    #[must_use]
    pub fn current_dir_name(&self, name: &str) -> Option<String> {
        std::fs::read_to_string(self.versions_dir(name).join(CURRENT_MARKER))
            .ok()
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty())
    }

    /// Every installed version of `name`, oldest first.
    ///
    /// # Errors
    /// Never for a missing directory (empty list); [`CompileError::Io`] if
    /// the directory cannot be listed.
    pub fn versions(&self, name: &str) -> Result<Vec<InstalledVersion>, CompileError> {
        let root = self.versions_dir(name);
        let read_dir = match std::fs::read_dir(&root) {
            Ok(read_dir) => read_dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(CompileError::Io {
                    context: format!("list {}", root.display()),
                    source: error,
                });
            }
        };
        let current = self.current_dir_name(name);
        let mut versions: Vec<InstalledVersion> = read_dir
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| {
                let dir = entry.path();
                let dir_name = entry.file_name().into_string().ok()?;
                let text = std::fs::read(dir.join(BUILD_RECORD_FILE)).ok()?;
                let record: BuildRecord = serde_json::from_slice(&text).ok()?;
                Some(InstalledVersion {
                    current: current.as_deref() == Some(dir_name.as_str()),
                    file: dir.join(&record.file),
                    dir,
                    dir_name,
                    record,
                })
            })
            .collect();
        versions.sort_by(|a, b| {
            a.record
                .sequence
                .cmp(&b.record.sequence)
                .then_with(|| a.record.built_at_unix.cmp(&b.record.built_at_unix))
                .then_with(|| a.dir_name.cmp(&b.dir_name))
        });
        Ok(versions)
    }

    /// The current version of `name`.
    #[must_use]
    pub fn current(&self, name: &str) -> Option<InstalledVersion> {
        self.versions(name)
            .ok()?
            .into_iter()
            .find(|version| version.current)
    }

    /// Switches `~/.harw/bin/<name>` to the version `selector` names: a
    /// directory name, a definition version (if unique) or a digest prefix
    /// (at least 4 hex characters).
    ///
    /// # Errors
    /// [`CompileError::Other`] if nothing or more than one version matches.
    pub fn use_version(
        &self,
        name: &str,
        selector: &str,
    ) -> Result<InstalledVersion, CompileError> {
        let versions = self.versions(name)?;
        if versions.is_empty() {
            return Err(CompileError::Other(format!(
                "`{name}` has no installed versions (build it with `harw agent build {name}`)"
            )));
        }
        let exact: Vec<&InstalledVersion> = versions
            .iter()
            .filter(|version| version.dir_name == selector)
            .collect();
        let matches: Vec<&InstalledVersion> = if exact.is_empty() {
            let selector_lower = selector.to_ascii_lowercase();
            let by_digest =
                selector_lower.len() >= 4 && selector_lower.chars().all(|c| c.is_ascii_hexdigit());
            versions
                .iter()
                .filter(|version| {
                    version.record.version == selector
                        || (by_digest
                            && version.record.artifact_digest.starts_with(&selector_lower))
                })
                .collect()
        } else {
            exact
        };
        match matches.as_slice() {
            [one] => {
                let chosen = (*one).clone();
                if chosen.record.file.ends_with(".harwa") {
                    return Err(CompileError::Other(format!(
                        "`{}` is an artifact-only build and cannot be the current binary",
                        chosen.dir_name
                    )));
                }
                self.set_current(name, &chosen.dir_name)?;
                Ok(InstalledVersion {
                    current: true,
                    ..chosen
                })
            }
            [] => Err(CompileError::Other(format!(
                "no version of `{name}` matches `{selector}`; installed: {}",
                versions
                    .iter()
                    .map(|version| version.dir_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
            many => Err(CompileError::Other(format!(
                "`{selector}` matches several versions of `{name}`: {}; name one directly",
                many.iter()
                    .map(|version| version.dir_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    /// Points `~/.harw/bin/<name>` at `.versions/<name>/<dir_name>/<name>`.
    ///
    /// # Description
    /// A relative symlink is created under a temporary name and renamed over
    /// the link (atomic on POSIX). Where symlinks are unavailable, the file
    /// is copied to a temporary name and renamed. The marker file
    /// `.versions/<name>/current` records the choice either way.
    ///
    /// # Errors
    /// [`CompileError::Io`] on failure.
    pub fn set_current(&self, name: &str, dir_name: &str) -> Result<(), CompileError> {
        let dir = self.versions_dir(name).join(dir_name);
        let record_text = std::fs::read(dir.join(BUILD_RECORD_FILE)).map_err(CompileError::io(
            format!("read {}", dir.join(BUILD_RECORD_FILE).display()),
        ))?;
        let record: BuildRecord = serde_json::from_slice(&record_text)
            .map_err(|error| CompileError::Other(format!("{}: {error}", dir.display())))?;
        let target_file = dir.join(&record.file);
        let link = self.link(name);
        let temporary = self
            .root
            .join(format!(".{name}.tmp-{}", std::process::id()));
        let _ = std::fs::remove_file(&temporary);
        let relative = Path::new(VERSIONS_DIR)
            .join(name)
            .join(dir_name)
            .join(&record.file);
        if !make_symlink(&relative, &temporary) {
            std::fs::copy(&target_file, &temporary).map_err(CompileError::io(format!(
                "copy {} to {}",
                target_file.display(),
                temporary.display()
            )))?;
        }
        std::fs::rename(&temporary, &link).map_err(CompileError::io(format!(
            "switch {} to {dir_name}",
            link.display()
        )))?;
        let marker = self.versions_dir(name).join(CURRENT_MARKER);
        std::fs::write(&marker, format!("{dir_name}\n"))
            .map_err(CompileError::io(format!("write {}", marker.display())))?;
        Ok(())
    }

    /// Removes old versions of `name`, keeping the current one plus the
    /// `keep` most recent others.
    ///
    /// # Errors
    /// [`CompileError::Io`] if a directory cannot be removed.
    pub fn prune_versions(
        &self,
        name: &str,
        keep: usize,
        dry_run: bool,
    ) -> Result<Vec<RemovedVersion>, CompileError> {
        let versions = self.versions(name)?;
        let mut others: Vec<&InstalledVersion> =
            versions.iter().filter(|version| !version.current).collect();
        // Newest first; everything after the first `keep` goes.
        others.sort_by(|a, b| {
            b.record
                .sequence
                .cmp(&a.record.sequence)
                .then_with(|| b.record.built_at_unix.cmp(&a.record.built_at_unix))
                .then_with(|| b.dir_name.cmp(&a.dir_name))
        });
        let mut removed = Vec::new();
        for version in others.into_iter().skip(keep) {
            let bytes = crate::cache::dir_size(&version.dir);
            if !dry_run {
                std::fs::remove_dir_all(&version.dir).map_err(CompileError::io(format!(
                    "remove {}",
                    version.dir.display()
                )))?;
            }
            removed.push(RemovedVersion {
                name: name.to_owned(),
                dir_name: version.dir_name.clone(),
                bytes,
            });
        }
        Ok(removed)
    }
}

#[cfg(unix)]
fn make_symlink(target: &Path, link: &Path) -> bool {
    std::os::unix::fs::symlink(target, link).is_ok()
}

#[cfg(not(unix))]
fn make_symlink(_target: &Path, _link: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(name: &str, version: &str, digest: &str, at: i64) -> BuildRecord {
        BuildRecord {
            schema: BUILD_RECORD_SCHEMA.to_owned(),
            name: name.to_owned(),
            definition_id: format!("acme.agent.{name}@1"),
            version: version.to_owned(),
            artifact_digest: digest.to_owned(),
            snapshot: "00".repeat(32),
            source_snapshot: "11".repeat(32),
            interfaces: vec!["cli".to_owned()],
            harw_version: crate::env::HARW_VERSION.to_owned(),
            built_at: format!("t{at}"),
            built_at_unix: at,
            sequence: 0,
            backend: "artifact".to_owned(),
            target: "x86_64-unknown-linux-gnu".to_owned(),
            file: name.to_owned(),
        }
    }

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn test_install_links_the_current_version() -> TestResult {
        let root = tempfile::tempdir()?;
        let bin = BinDir::new(root.path().join("bin"));
        std::fs::create_dir_all(bin.root())?;
        let first = bin.install(
            &record("ec", "1.0.0", &"a".repeat(64), 1),
            b"one",
            true,
            true,
        )?;
        assert_eq!(first.dir_name, "1.0.0-aaaaaaaaaaaa");
        assert_eq!(std::fs::read(bin.link("ec"))?, b"one");
        let second = bin.install(
            &record("ec", "1.0.1", &"b".repeat(64), 2),
            b"two",
            true,
            true,
        )?;
        assert_eq!(std::fs::read(bin.link("ec"))?, b"two");
        let versions = bin.versions("ec")?;
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].dir_name, first.dir_name, "oldest first");
        assert!(versions[1].current && !versions[0].current);
        assert_eq!(bin.names(), ["ec"]);
        Ok(())
    }

    #[test]
    fn test_use_switches_by_version_digest_or_dir_name() -> TestResult {
        let root = tempfile::tempdir()?;
        let bin = BinDir::new(root.path().join("bin"));
        std::fs::create_dir_all(bin.root())?;
        bin.install(
            &record("ec", "1.0.0", &"a1".repeat(32), 1),
            b"one",
            true,
            true,
        )?;
        bin.install(
            &record("ec", "1.0.1", &"b2".repeat(32), 2),
            b"two",
            true,
            true,
        )?;
        let chosen = bin.use_version("ec", "1.0.0")?;
        assert!(chosen.current);
        assert_eq!(std::fs::read(bin.link("ec"))?, b"one");
        bin.use_version("ec", "b2b2")?;
        assert_eq!(std::fs::read(bin.link("ec"))?, b"two");
        bin.use_version("ec", "1.0.0-a1a1a1a1a1a1")?;
        assert_eq!(
            bin.current("ec").map(|v| v.record.version),
            Some("1.0.0".to_owned())
        );
        assert!(bin.use_version("ec", "9.9.9").is_err());
        Ok(())
    }

    #[test]
    fn test_prune_keeps_current_and_the_newest_n() -> TestResult {
        let root = tempfile::tempdir()?;
        let bin = BinDir::new(root.path().join("bin"));
        std::fs::create_dir_all(bin.root())?;
        for (at, digest) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            let at = i64::try_from(at)?;
            bin.install(
                &record("ec", &format!("1.0.{at}"), &digest.repeat(64), at),
                b"x",
                true,
                true,
            )?;
        }
        // Make the oldest version current: it must survive any keep.
        bin.use_version("ec", "1.0.0")?;
        let dry = bin.prune_versions("ec", 1, true)?;
        assert_eq!(dry.len(), 3);
        assert_eq!(bin.versions("ec")?.len(), 5, "dry run removes nothing");
        let removed = bin.prune_versions("ec", 1, false)?;
        let names: Vec<&str> = removed.iter().map(|r| r.dir_name.as_str()).collect();
        assert_eq!(
            names,
            [
                "1.0.3-dddddddddddd",
                "1.0.2-cccccccccccc",
                "1.0.1-bbbbbbbbbbbb"
            ]
        );
        let left: Vec<String> = bin
            .versions("ec")?
            .into_iter()
            .map(|v| v.dir_name)
            .collect();
        assert_eq!(left, ["1.0.0-aaaaaaaaaaaa", "1.0.4-eeeeeeeeeeee"]);
        assert_eq!(std::fs::read(bin.link("ec"))?, b"x");
        assert_eq!(bin.prune_versions("ec", 0, false)?.len(), 1);
        assert_eq!(
            bin.versions("ec")?.len(),
            1,
            "only the current version is left"
        );
        Ok(())
    }
}
