//! The native build cache (`~/.harw/cache/agent-builds`) and its garbage
//! collector, plus the `[agent_compiler]` settings.
//!
//! # Layout
//! ```text
//! ~/.harw/cache/agent-builds/
//!   target/                one shared cargo target dir for every native build
//!   blobs/<blake3>         the payload pool of every build, each blob once
//!   <key>/                 one generated crate per (artifact, features, target, version)
//!     Cargo.toml, src/main.rs, agent.harwa, rust-toolchain.toml, Cargo.lock
//!     stamp.json           harw version, agent, last use, referenced blobs
//! ```
//!
//! # Collection order
//! [`collect_garbage`] removes, in this order, and never a protected path:
//! 1. with `all`: every crate directory and the shared target dir;
//! 2. crate directories of other harw versions;
//! 3. crate directories unused for longer than `older_than_secs`;
//! 4. all but the `keep` most recently used crate directories;
//! 5. least recently used crate directories while the cache exceeds
//!    `max_bytes`;
//! 6. if it still does, the shared target dir (a `cargo clean` done on the
//!    file system), sparing only the files in `keep_outputs`;
//! 7. blobs in `blobs/` that no remaining crate directory references (the
//!    stamps list each build's pool blobs; reference counting).
//!
//! The collector only ever deletes below the cache root; `-o` outputs and
//! the installed versions under `~/.harw/bin` are outside it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::env::{CompilerEnv, HARW_VERSION};
use crate::error::CompileError;

/// Default cache cap: 5 GiB.
pub const DEFAULT_CACHE_MAX_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// Default number of old versions `harw agent clean` keeps per agent.
pub const DEFAULT_KEEP_VERSIONS: usize = 3;

/// Name of the shared target directory.
pub const TARGET_DIR: &str = "target";

/// Stamp file of a crate directory.
pub const STAMP_FILE: &str = "stamp.json";

/// Content-addressed blob store shared by all builds (`blobs/<blake3>`).
pub const BLOBS_DIR: &str = "blobs";

/// `[agent_compiler]` (and `active_uia_definition`) from the layer
/// `config.toml` files (last wins).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCompilerSettings {
    /// Cache cap in bytes.
    pub cache_max_bytes: Option<u64>,
    /// Old versions kept by the automatic cleanup after a native build.
    pub keep_versions: Option<usize>,
    /// Build the active UIA automatically (default `true`).
    pub auto_build_uia: Option<bool>,
    /// `active_uia_definition` of the layer configs (last wins).
    pub active_uia_definition: Option<String>,
}

impl AgentCompilerSettings {
    /// Reads `[agent_compiler]` from `<layer>/config.toml` of every layer;
    /// later layers override earlier ones. Unreadable files are skipped.
    #[must_use]
    pub fn load(env: &CompilerEnv) -> Self {
        let mut settings = Self::default();
        for layer in &env.layers {
            let Ok(text) = std::fs::read_to_string(layer.join("config.toml")) else {
                continue;
            };
            let Ok(table) = toml::from_str::<toml::Table>(&text) else {
                continue;
            };
            if let Some(uia) = table
                .get("active_uia_definition")
                .and_then(toml::Value::as_str)
            {
                settings.active_uia_definition = Some(uia.to_owned());
            }
            let Some(section) = table.get("agent_compiler").and_then(toml::Value::as_table) else {
                continue;
            };
            if let Some(value) = section
                .get("auto_build_uia")
                .and_then(toml::Value::as_bool)
            {
                settings.auto_build_uia = Some(value);
            }
            if let Some(value) = section
                .get("cache_max_bytes")
                .and_then(toml::Value::as_integer)
                .and_then(|value| u64::try_from(value).ok())
            {
                settings.cache_max_bytes = Some(value);
            }
            if let Some(value) = section
                .get("keep_versions")
                .and_then(toml::Value::as_integer)
                .and_then(|value| usize::try_from(value).ok())
            {
                settings.keep_versions = Some(value);
            }
        }
        settings
    }

    /// Whether the active UIA is built automatically.
    #[must_use]
    pub fn auto_build_uia(&self) -> bool {
        self.auto_build_uia.unwrap_or(true)
    }

    /// The effective cache cap.
    #[must_use]
    pub fn max_bytes(&self) -> u64 {
        self.cache_max_bytes.unwrap_or(DEFAULT_CACHE_MAX_BYTES)
    }
}

/// What a crate directory records about its last use.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheStamp {
    /// harw version that generated the crate.
    pub harw_version: String,
    /// Agent binary name.
    pub name: String,
    /// Artifact digest (hex).
    pub artifact_digest: String,
    /// Target triple.
    pub target: String,
    /// Last use, Unix seconds.
    pub last_used: u64,
    /// Blobs of the build's payload pool (hex), stored once in `blobs/`.
    #[serde(default)]
    pub blobs: Vec<String>,
}

impl CacheStamp {
    /// Writes the stamp into `dir`.
    ///
    /// # Errors
    /// [`CompileError::Io`] on a write failure.
    pub fn write(&self, dir: &Path) -> Result<(), CompileError> {
        let bytes = serde_json::to_vec_pretty(self)
            .map_err(|error| CompileError::Other(format!("serialize cache stamp: {error}")))?;
        std::fs::write(dir.join(STAMP_FILE), bytes)
            .map_err(CompileError::io(format!("write {}", dir.join(STAMP_FILE).display())))
    }

    fn read(dir: &Path) -> Option<Self> {
        let bytes = std::fs::read(dir.join(STAMP_FILE)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }
}

/// What the collector should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcPolicy {
    /// Cache cap in bytes.
    pub max_bytes: u64,
    /// Keep at most this many crate directories (most recently used).
    pub keep: Option<usize>,
    /// Remove crate directories unused for longer than this.
    pub older_than_secs: Option<u64>,
    /// Remove everything.
    pub all: bool,
    /// Only report.
    pub dry_run: bool,
}

impl GcPolicy {
    /// The automatic policy after a build: only the cap and version changes.
    #[must_use]
    pub fn automatic(max_bytes: u64) -> Self {
        Self {
            max_bytes,
            keep: None,
            older_than_secs: None,
            all: false,
            dry_run: false,
        }
    }
}

/// One removed (or, dry run, removable) path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Removal {
    /// The path.
    pub path: PathBuf,
    /// Bytes freed.
    pub bytes: u64,
    /// Why (`all`, `old-version`, `older-than`, `keep`, `cap`, `target-clean`).
    pub reason: String,
}

/// The collector's report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GcReport {
    /// The cache root.
    pub root: PathBuf,
    /// Size before.
    pub before_bytes: u64,
    /// Size after (for a dry run: the size it would have).
    pub after_bytes: u64,
    /// What went (or would go).
    pub removed: Vec<Removal>,
    /// Whether nothing was deleted.
    pub dry_run: bool,
}

/// Recursive size of a file or directory (symlinks counted, not followed).
#[must_use]
pub fn dir_size(path: &Path) -> u64 {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !metadata.is_dir() {
        return metadata.len();
    }
    let Ok(read_dir) = std::fs::read_dir(path) else {
        return 0;
    };
    read_dir
        .filter_map(Result::ok)
        .map(|entry| dir_size(&entry.path()))
        .sum()
}

/// A crate directory with its last use.
struct CrateDir {
    path: PathBuf,
    stamp: Option<CacheStamp>,
    last_used: u64,
    bytes: u64,
}

fn crate_dirs(root: &Path) -> Vec<CrateDir> {
    let Ok(read_dir) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut dirs: Vec<CrateDir> = read_dir
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name() != TARGET_DIR && entry.file_name() != BLOBS_DIR)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| {
            let path = entry.path();
            let stamp = CacheStamp::read(&path);
            let modified = entry
                .metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |duration| duration.as_secs());
            CrateDir {
                last_used: stamp.as_ref().map_or(modified, |stamp| stamp.last_used),
                bytes: dir_size(&path),
                stamp,
                path,
            }
        })
        .collect();
    // Oldest first, path as tie breaker.
    dirs.sort_by(|a, b| a.last_used.cmp(&b.last_used).then_with(|| a.path.cmp(&b.path)));
    dirs
}

fn is_protected(path: &Path, protect: &[PathBuf]) -> bool {
    protect.iter().any(|protected| protected == path || protected.starts_with(path))
}

/// Removes a path (unless `dry_run`) and records it.
fn remove(
    path: &Path,
    bytes: u64,
    reason: &str,
    dry_run: bool,
    report: &mut GcReport,
) -> Result<(), CompileError> {
    if !dry_run {
        let result = if path.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
        match result {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(CompileError::Io {
                    context: format!("remove {}", path.display()),
                    source: error,
                });
            }
        }
    }
    report.removed.push(Removal {
        path: path.to_path_buf(),
        bytes,
        reason: reason.to_owned(),
    });
    Ok(())
}

/// Empties `dir` except the files in `keep` (and the directories leading to
/// them). Returns the bytes removed.
fn clean_except(
    dir: &Path,
    keep: &[PathBuf],
    dry_run: bool,
    report: &mut GcReport,
) -> Result<u64, CompileError> {
    let Ok(read_dir) = std::fs::read_dir(dir) else {
        return Ok(0);
    };
    let mut entries: Vec<PathBuf> = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    let mut freed = 0;
    for path in entries {
        if keep.iter().any(|kept| kept == &path) {
            continue;
        }
        if keep.iter().any(|kept| kept.starts_with(&path)) {
            freed += clean_except(&path, keep, dry_run, report)?;
            continue;
        }
        let bytes = dir_size(&path);
        remove(&path, bytes, "target-clean", dry_run, report)?;
        freed += bytes;
    }
    Ok(freed)
}

/// Removes the blobs in `blobs/` that no remaining crate directory
/// references (reference counting over the stamps); with `all`, every blob.
/// Returns the bytes removed.
fn collect_blobs(
    root: &Path,
    removed_dirs: &[PathBuf],
    all: bool,
    dry_run: bool,
    report: &mut GcReport,
) -> Result<u64, CompileError> {
    let Ok(read_dir) = std::fs::read_dir(root.join(BLOBS_DIR)) else {
        return Ok(0);
    };
    let referenced: std::collections::BTreeSet<String> = if all {
        std::collections::BTreeSet::new()
    } else {
        crate_dirs(root)
            .into_iter()
            .filter(|dir| !removed_dirs.contains(&dir.path))
            .filter_map(|dir| dir.stamp)
            .flat_map(|stamp| stamp.blobs)
            .collect()
    };
    let mut blobs: Vec<PathBuf> = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    blobs.sort();
    let mut freed = 0;
    for path in blobs {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_owned();
        if referenced.contains(&name) {
            continue;
        }
        let bytes = dir_size(&path);
        remove(&path, bytes, "unreferenced-blob", dry_run, report)?;
        freed += bytes;
    }
    Ok(freed)
}

/// Stores a build's pool blobs in `blobs/` (each once; existing blobs are
/// left alone). Returns their hashes, sorted, for the crate's stamp.
///
/// # Errors
/// [`CompileError::Io`] on a write failure.
pub fn store_blobs(
    root: &Path,
    artifact: &harw_agent_artifact::Artifact,
) -> Result<Vec<String>, CompileError> {
    let dir = root.join(BLOBS_DIR);
    std::fs::create_dir_all(&dir).map_err(CompileError::io(format!("create {}", dir.display())))?;
    let blob_kind = harw_agent_artifact::PayloadKind::Other(harw_agent_artifact::bundle::BLOB_KIND.to_owned());
    let mut hashes = Vec::new();
    for payload in artifact.payloads() {
        if *payload.kind() != blob_kind {
            continue;
        }
        let hash = harw_agent_artifact::ArtifactDigest::from_bytes(*payload.blake3()).to_hex();
        let path = dir.join(&hash);
        if !path.is_file() {
            std::fs::write(&path, payload.bytes())
                .map_err(CompileError::io(format!("write {}", path.display())))?;
        }
        hashes.push(hash);
    }
    hashes.sort();
    Ok(hashes)
}

/// Collects garbage in the cache at `root` (see module docs).
///
/// # Arguments
/// - `protect`: crate directories that must survive (the current build).
/// - `keep_outputs`: files in the shared target dir that must survive a
///   target clean (the current build's binary).
/// - `now`: Unix seconds.
///
/// # Errors
/// [`CompileError::Io`] if a path cannot be removed.
pub fn collect_garbage(
    root: &Path,
    policy: &GcPolicy,
    protect: &[PathBuf],
    keep_outputs: &[PathBuf],
    now: u64,
) -> Result<GcReport, CompileError> {
    let before = dir_size(root);
    let mut report = GcReport {
        root: root.to_path_buf(),
        before_bytes: before,
        after_bytes: before,
        removed: Vec::new(),
        dry_run: policy.dry_run,
    };
    let mut total = before;
    let mut remaining: Vec<CrateDir> = Vec::new();
    for dir in crate_dirs(root) {
        if is_protected(&dir.path, protect) {
            continue;
        }
        let reason = if policy.all {
            Some("all")
        } else if dir
            .stamp
            .as_ref()
            .is_some_and(|stamp| stamp.harw_version != HARW_VERSION)
        {
            Some("old-version")
        } else if policy
            .older_than_secs
            .is_some_and(|secs| now.saturating_sub(dir.last_used) > secs)
        {
            Some("older-than")
        } else {
            None
        };
        match reason {
            Some(reason) => {
                remove(&dir.path, dir.bytes, reason, policy.dry_run, &mut report)?;
                total = total.saturating_sub(dir.bytes);
            }
            None => remaining.push(dir),
        }
    }
    if let Some(keep) = policy.keep {
        // `remaining` is oldest first; the newest `keep` stay.
        let excess = remaining.len().saturating_sub(keep);
        for dir in remaining.drain(..excess) {
            remove(&dir.path, dir.bytes, "keep", policy.dry_run, &mut report)?;
            total = total.saturating_sub(dir.bytes);
        }
    }
    let mut lru = remaining.into_iter();
    while total > policy.max_bytes {
        let Some(dir) = lru.next() else {
            break;
        };
        remove(&dir.path, dir.bytes, "cap", policy.dry_run, &mut report)?;
        total = total.saturating_sub(dir.bytes);
    }
    let target = root.join(TARGET_DIR);
    if policy.all || total > policy.max_bytes {
        let freed = clean_except(&target, keep_outputs, policy.dry_run, &mut report)?;
        total = total.saturating_sub(freed);
    }
    let removed_dirs: Vec<PathBuf> = report
        .removed
        .iter()
        .map(|removal| removal.path.clone())
        .collect();
    let freed = collect_blobs(root, &removed_dirs, policy.all, policy.dry_run, &mut report)?;
    total = total.saturating_sub(freed);
    report.after_bytes = if policy.dry_run {
        total
    } else {
        dir_size(root)
    };
    Ok(report)
}

/// Runner directories of other harw versions under `~/.harw/runners` and
/// `~/.harw/bin/.runners` (removed by `harw agent clean` and the automatic
/// cleanup).
#[must_use]
pub fn stale_runner_dirs(env: &CompilerEnv) -> Vec<PathBuf> {
    let mut stale = Vec::new();
    for base in [env.home.join("runners"), env.bin_dir().join(".runners")] {
        let Ok(targets) = std::fs::read_dir(&base) else {
            continue;
        };
        for target in targets.filter_map(Result::ok) {
            let Ok(versions) = std::fs::read_dir(target.path()) else {
                continue;
            };
            for version in versions.filter_map(Result::ok) {
                if version.file_name() != HARW_VERSION && version.path().is_dir() {
                    stale.push(version.path());
                }
            }
        }
    }
    stale.sort();
    stale
}

/// Unix seconds now.
#[must_use]
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn crate_dir(root: &Path, name: &str, last_used: u64, bytes: usize) -> Result<PathBuf, Box<dyn std::error::Error>> {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("agent.harwa"), vec![0_u8; bytes])?;
        CacheStamp {
            harw_version: HARW_VERSION.to_owned(),
            name: name.to_owned(),
            artifact_digest: "00".repeat(32),
            target: "x".to_owned(),
            last_used,
            blobs: vec![format!("blob-{name}")],
        }
        .write(&dir)?;
        Ok(dir)
    }

    #[test]
    fn test_cap_evicts_least_recently_used_first() -> TestResult {
        let root = tempfile::tempdir()?;
        let old = crate_dir(root.path(), "old", 10, 4000)?;
        let mid = crate_dir(root.path(), "mid", 20, 4000)?;
        let new = crate_dir(root.path(), "new", 30, 4000)?;
        let size = dir_size(root.path());
        // Room for two of the three directories.
        let policy = GcPolicy::automatic(size - 3000);
        let report = collect_garbage(root.path(), &policy, &[], &[], 100)?;
        assert_eq!(report.removed.len(), 1);
        assert_eq!(report.removed[0].path, old);
        assert_eq!(report.removed[0].reason, "cap");
        assert!(!old.exists() && mid.exists() && new.exists());
        assert!(report.after_bytes <= policy.max_bytes);
        Ok(())
    }

    #[test]
    fn test_protected_current_build_and_outputs_survive() -> TestResult {
        let root = tempfile::tempdir()?;
        let current = crate_dir(root.path(), "current", 1, 4000)?;
        let other = crate_dir(root.path(), "other", 50, 4000)?;
        let target_release = root.path().join(TARGET_DIR).join("release");
        std::fs::create_dir_all(target_release.join("deps"))?;
        let output = target_release.join("ec");
        std::fs::write(&output, vec![1_u8; 2000])?;
        std::fs::write(target_release.join("deps").join("libx.rlib"), vec![2_u8; 2000])?;
        // An `-o` output outside the cache.
        let elsewhere = tempfile::tempdir()?;
        let user_output = elsewhere.path().join("ec");
        std::fs::write(&user_output, b"binary")?;

        let policy = GcPolicy::automatic(1);
        let report = collect_garbage(
            root.path(),
            &policy,
            std::slice::from_ref(&current),
            std::slice::from_ref(&output),
            100,
        )?;
        assert!(current.exists(), "the current build's crate survives");
        assert!(output.exists(), "the current build's binary survives");
        assert!(!other.exists());
        assert!(!target_release.join("deps").exists(), "target cleaned");
        assert!(user_output.exists(), "outputs outside the cache are untouched");
        assert!(report.removed.iter().any(|removal| removal.reason == "target-clean"));
        Ok(())
    }

    #[test]
    fn test_dry_run_removes_nothing_and_predicts_the_size() -> TestResult {
        let root = tempfile::tempdir()?;
        let a = crate_dir(root.path(), "a", 10, 3000)?;
        let b = crate_dir(root.path(), "b", 20, 3000)?;
        let policy = GcPolicy {
            max_bytes: u64::MAX,
            keep: Some(1),
            older_than_secs: None,
            all: false,
            dry_run: true,
        };
        let report = collect_garbage(root.path(), &policy, &[], &[], 100)?;
        assert!(a.exists() && b.exists());
        assert_eq!(report.removed.len(), 1);
        assert_eq!(report.removed[0].path, a, "keep=1 keeps the newest");
        assert!(report.after_bytes < report.before_bytes);
        Ok(())
    }

    #[test]
    fn test_older_than_and_old_versions_go_first() -> TestResult {
        let root = tempfile::tempdir()?;
        let stale = crate_dir(root.path(), "stale", 10, 10)?;
        let fresh = crate_dir(root.path(), "fresh", 95, 10)?;
        let foreign = root.path().join("foreign");
        std::fs::create_dir_all(&foreign)?;
        CacheStamp {
            harw_version: "0.0.1-old".to_owned(),
            name: "x".to_owned(),
            artifact_digest: String::new(),
            target: "x".to_owned(),
            last_used: 99,
            blobs: Vec::new(),
        }
        .write(&foreign)?;
        let policy = GcPolicy {
            max_bytes: u64::MAX,
            keep: None,
            older_than_secs: Some(30),
            all: false,
            dry_run: false,
        };
        let report = collect_garbage(root.path(), &policy, &[], &[], 100)?;
        let reasons: Vec<(&Path, &str)> = report
            .removed
            .iter()
            .map(|removal| (removal.path.as_path(), removal.reason.as_str()))
            .collect();
        assert!(reasons.contains(&(stale.as_path(), "older-than")));
        assert!(reasons.contains(&(foreign.as_path(), "old-version")));
        assert!(fresh.exists());
        Ok(())
    }

    #[test]
    fn test_unreferenced_blobs_go_referenced_blobs_stay() -> TestResult {
        let root = tempfile::tempdir()?;
        let old = crate_dir(root.path(), "old", 10, 10)?;
        let kept = crate_dir(root.path(), "kept", 90, 10)?;
        let blobs = root.path().join(BLOBS_DIR);
        std::fs::create_dir_all(&blobs)?;
        for name in ["blob-old", "blob-kept", "blob-orphan"] {
            std::fs::write(blobs.join(name), b"bytes")?;
        }
        let policy = GcPolicy {
            max_bytes: u64::MAX,
            keep: None,
            older_than_secs: Some(30),
            all: false,
            dry_run: false,
        };
        let report = collect_garbage(root.path(), &policy, &[], &[], 100)?;
        assert!(!old.exists() && kept.exists());
        assert!(blobs.join("blob-kept").is_file(), "still referenced");
        assert!(!blobs.join("blob-old").exists(), "its only build is gone");
        assert!(!blobs.join("blob-orphan").exists(), "never referenced");
        assert_eq!(
            report
                .removed
                .iter()
                .filter(|removal| removal.reason == "unreferenced-blob")
                .count(),
            2
        );
        Ok(())
    }

    #[test]
    fn test_settings_read_the_last_layer() -> TestResult {
        let home = tempfile::tempdir()?;
        let profile = home.path().join("profiles").join("p");
        std::fs::create_dir_all(&profile)?;
        std::fs::write(
            home.path().join("config.toml"),
            "[agent_compiler]\ncache_max_bytes = 100\nkeep_versions = 2\n",
        )?;
        std::fs::write(profile.join("config.toml"), "[agent_compiler]\ncache_max_bytes = 200\n")?;
        let mut env = CompilerEnv::isolated(home.path().to_path_buf(), home.path().to_path_buf());
        env.layers.push(profile);
        let settings = AgentCompilerSettings::load(&env);
        assert_eq!(settings.cache_max_bytes, Some(200));
        assert_eq!(settings.keep_versions, Some(2));
        assert_eq!(
            AgentCompilerSettings::default().max_bytes(),
            DEFAULT_CACHE_MAX_BYTES
        );
        Ok(())
    }
}
