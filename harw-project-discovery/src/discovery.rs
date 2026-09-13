//! Root detection and doc-cascade loading — synchronous API.
//!
//! # Responsibility
//!
//! This module owns:
//! - [`DiscoveryConfig`]: configurable knobs for marker names, doc filenames,
//!   byte budgets, and walk depth.
//! - [`discover_project`]: the main synchronous entry-point that walks the
//!   filesystem upward to find the project root and then collects doc files
//!   from the root down to `cwd`.
//! - [`ProjectContext`]: the discovery result (root path + ordered docs).
//! - [`DiscoveredDoc`]: a single loaded doc file with its content.
//! - [`DiscoveryError`]: all error variants that can arise from discovery.
//!
//! # Algorithm (see [`discover_project`])
//!
//! 1. Canonicalise `cwd`.
//! 2. Walk upward (max `max_walk_depth` steps), testing each directory for any
//!    configured root marker. First match → `project_root`.
//! 3. Collect the directory chain from `project_root` to `cwd` (inclusive),
//!    root first.
//! 4. For each directory in the chain, try each `doc_filenames` entry in order;
//!    load the first one found, subject to per-doc and total byte budgets.
//!
//! # Concurrency
//!
//! All I/O is synchronous (`std::fs`). Safe to call from any thread.
//!
//! # Errors
//!
//! - [`DiscoveryError::InvalidCwd`] — `cwd.canonicalize()` fails.
//! - [`DiscoveryError::Io`] — unexpected I/O error during directory traversal.
//! - [`DiscoveryError::Utf8`] — a doc file is not valid UTF-8.

use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::string::FromUtf8Error;

use tracing::{debug, trace, warn};

// ─── Error ───────────────────────────────────────────────────────────────────

/// All errors that can arise during project discovery.
///
/// # Description
///
/// Covers three distinct failure modes: a cwd that cannot be canonicalised,
/// an unexpected I/O failure during traversal, and a doc file that is not
/// valid UTF-8.
///
/// # Examples
///
/// ```rust,no_run
/// use harw_project_discovery::DiscoveryError;
/// use std::path::PathBuf;
///
/// let err = DiscoveryError::InvalidCwd(PathBuf::from("/nonexistent"));
/// println!("{err}");
/// ```
#[derive(Debug)]
pub enum DiscoveryError {
    /// The provided `cwd` could not be canonicalised (does not exist or lacks
    /// permissions).
    InvalidCwd(PathBuf),

    /// An I/O error occurred while reading directory entries or file metadata.
    Io(io::Error),

    /// A discovered doc file could not be decoded as UTF-8.
    Utf8(FromUtf8Error),
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCwd(p) => write!(f, "cannot canonicalize cwd `{}`", p.display()),
            Self::Io(e) => write!(f, "I/O error during project discovery: {e}"),
            Self::Utf8(e) => write!(f, "doc file is not valid UTF-8: {e}"),
        }
    }
}

impl std::error::Error for DiscoveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidCwd(_) => None,
            Self::Io(e) => Some(e),
            Self::Utf8(e) => Some(e),
        }
    }
}

impl From<io::Error> for DiscoveryError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<FromUtf8Error> for DiscoveryError {
    fn from(e: FromUtf8Error) -> Self {
        Self::Utf8(e)
    }
}

// ─── Config ──────────────────────────────────────────────────────────────────

/// Configuration for the project-discovery walk.
///
/// # Description
///
/// Controls which filesystem entries are treated as project-root markers,
/// which filenames are scanned for documentation, and byte budget limits
/// that prevent loading excessively large files.
///
/// # Arguments (builder setters)
///
/// - [`with_root_markers`](Self::with_root_markers): replaces the default
///   marker list.
/// - [`with_doc_filenames`](Self::with_doc_filenames): replaces the default
///   doc-filename list.
///
/// # Concurrency
///
/// `DiscoveryConfig` is `Send + Sync` and may be shared across threads.
///
/// # Examples
///
/// ```rust
/// use harw_project_discovery::DiscoveryConfig;
///
/// let cfg = DiscoveryConfig::new()
///     .with_root_markers(vec![".git".to_string()])
///     .with_doc_filenames(vec!["HARW.md".to_string()]);
/// assert_eq!(cfg.max_bytes_per_doc, 32 * 1024);
/// ```
#[derive(Debug, Clone)]
pub struct DiscoveryConfig {
    /// Filesystem entries (file or directory) whose presence marks a project root.
    ///
    /// Checked in order; the first ancestor directory that contains any of these
    /// entries is considered the project root.
    pub root_markers: Vec<String>,

    /// Filenames scanned for documentation in each directory along the chain.
    ///
    /// Per directory, filenames are tried in order; the first one found is loaded
    /// and the rest are skipped.
    pub doc_filenames: Vec<String>,

    /// Maximum bytes to read from a single doc file. Content beyond this limit
    /// is truncated at the last complete UTF-8 character.
    pub max_bytes_per_doc: u64,

    /// Maximum total bytes across all loaded docs. Discovery stops collecting
    /// further docs once this limit would be exceeded.
    pub max_total_bytes: u64,

    /// Maximum number of parent-directory hops when searching for the project root.
    ///
    /// If no marker is found within this many hops, `cwd` is used as the root.
    pub max_walk_depth: usize,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            root_markers: vec![
                ".git".to_string(),
                "Cargo.toml".to_string(),
                "package.json".to_string(),
                "pyproject.toml".to_string(),
            ],
            doc_filenames: vec![
                "HARW.md".to_string(),
                "AGENTS.md".to_string(),
                "CLAUDE.md".to_string(),
            ],
            max_bytes_per_doc: 32 * 1024,
            max_total_bytes: 128 * 1024,
            max_walk_depth: 64,
        }
    }
}

impl DiscoveryConfig {
    /// Creates a `DiscoveryConfig` with all default values.
    ///
    /// # Description
    ///
    /// Equivalent to `DiscoveryConfig::default()`. Provided for ergonomic
    /// builder-chain initialization.
    ///
    /// # Returns
    ///
    /// A `DiscoveryConfig` with default root markers, doc filenames, and byte
    /// budgets (see type-level docs).
    ///
    /// # Examples
    ///
    /// ```rust
    /// use harw_project_discovery::DiscoveryConfig;
    /// let cfg = DiscoveryConfig::new();
    /// assert_eq!(cfg.root_markers.len(), 4);
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Replaces the root-marker list and returns `self` for chaining.
    ///
    /// # Description
    ///
    /// Each entry is checked as both a file and a directory. The first ancestor
    /// that contains any entry wins.
    ///
    /// # Arguments
    ///
    /// - `m` (`Vec<String>`): the new marker names (e.g., `[".git"]`).
    ///
    /// # Returns
    ///
    /// `Self` with `root_markers` replaced.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use harw_project_discovery::DiscoveryConfig;
    /// let cfg = DiscoveryConfig::new().with_root_markers(vec![".git".to_string()]);
    /// assert_eq!(cfg.root_markers, vec![".git"]);
    /// ```
    pub fn with_root_markers(mut self, m: Vec<String>) -> Self {
        self.root_markers = m;
        self
    }

    /// Replaces the doc-filename list and returns `self` for chaining.
    ///
    /// # Description
    ///
    /// Filenames are tried in order per directory; the first match in a given
    /// directory is loaded and subsequent names are skipped.
    ///
    /// # Arguments
    ///
    /// - `d` (`Vec<String>`): the new doc filenames (e.g., `["HARW.md"]`).
    ///
    /// # Returns
    ///
    /// `Self` with `doc_filenames` replaced.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use harw_project_discovery::DiscoveryConfig;
    /// let cfg = DiscoveryConfig::new().with_doc_filenames(vec!["HARW.md".to_string()]);
    /// assert_eq!(cfg.doc_filenames, vec!["HARW.md"]);
    /// ```
    pub fn with_doc_filenames(mut self, d: Vec<String>) -> Self {
        self.doc_filenames = d;
        self
    }
}

// ─── Output Types ────────────────────────────────────────────────────────────

/// A single doc file discovered and loaded during project discovery.
///
/// # Description
///
/// Carries the absolute path, the bare filename (for labeling), and the
/// decoded UTF-8 content of the file.
///
/// # Concurrency
///
/// `Clone`-able; safe to share across threads.
#[derive(Debug, Clone)]
pub struct DiscoveredDoc {
    /// Absolute path of the file on disk.
    pub path: PathBuf,

    /// Bare filename (e.g., `"HARW.md"`).
    pub filename: String,

    /// UTF-8 content of the file, possibly truncated to `max_bytes_per_doc`.
    pub content: String,
}

/// The result of a single [`discover_project`] call.
///
/// # Description
///
/// Records the canonicalised working directory, the detected project root,
/// and the ordered list of documentation files collected from root → cwd.
///
/// # Concurrency
///
/// `Clone`-able; safe to share across threads.
#[derive(Debug, Clone)]
pub struct ProjectContext {
    /// The canonicalised `cwd` passed to [`discover_project`].
    pub cwd: PathBuf,

    /// Detected project root. Equals `cwd` when no marker was found within
    /// `max_walk_depth` hops.
    pub project_root: PathBuf,

    /// Documentation files ordered from project root → cwd.
    ///
    /// At most one file per directory (the first matching `doc_filenames` entry),
    /// subject to byte-budget constraints.
    pub docs: Vec<DiscoveredDoc>,
}

// ─── Core Function ───────────────────────────────────────────────────────────

/// Detects the project root and collects doc-cascade files relative to `cwd`.
///
/// # Description
///
/// Implements a three-phase algorithm:
///
/// 1. **Canonicalise** — calls `cwd.canonicalize()`. Propagates as
///    [`DiscoveryError::InvalidCwd`] on failure.
/// 2. **Root walk** — walks upward from `cwd`, up to
///    `config.max_walk_depth` parent hops, testing each directory for any
///    entry in `config.root_markers` (both file and directory). The first
///    ancestor that has a marker match is the `project_root`. Fallback: `cwd`
///    itself if no marker is found.
/// 3. **Doc collect** — builds the directory chain \[root, …, cwd\] (root
///    first). For each directory in the chain, the first `doc_filenames` entry
///    that exists as a regular file is read (up to `max_bytes_per_doc` bytes).
///    Symlinked candidates are skipped. Once the cumulative loaded size would
///    exceed `max_total_bytes`, collection stops.
///
/// # Arguments
///
/// - `cwd` (`&Path`): the directory to start from (need not be canonicalised).
/// - `config` (`&DiscoveryConfig`): controls markers, filenames, and budgets.
///
/// # Returns
///
/// `Ok(ProjectContext)` on success. `Err(DiscoveryError)` on I/O or encoding
/// failure.
///
/// # Errors
///
/// - [`DiscoveryError::InvalidCwd`]: `cwd` cannot be canonicalised.
/// - [`DiscoveryError::Io`]: unexpected I/O failure during traversal.
/// - [`DiscoveryError::Utf8`]: a doc file is not valid UTF-8.
///
/// # Concurrency
///
/// Fully synchronous; may be called from any thread.
///
/// # Examples
///
/// ```rust,no_run
/// use harw_project_discovery::{discover_project, DiscoveryConfig};
/// use std::path::Path;
///
/// let cfg = DiscoveryConfig::default();
/// let ctx = discover_project(Path::new("."), &cfg).expect("discovery failed");
/// println!("root: {}", ctx.project_root.display());
/// ```
pub fn discover_project(
    cwd: &Path,
    config: &DiscoveryConfig,
) -> Result<ProjectContext, DiscoveryError> {
    // Phase 1 — canonicalize
    let cwd = cwd
        .canonicalize()
        .map_err(|_| DiscoveryError::InvalidCwd(cwd.to_owned()))?;
    debug!(cwd = %cwd.display(), "starting project discovery");

    // Phase 2 — root walk
    let project_root = find_project_root(&cwd, config);
    debug!(
        root = %project_root.display(),
        "project root resolved"
    );

    // Phase 3 — doc cascade collect
    let chain = build_dir_chain(&project_root, &cwd);
    let docs = collect_docs(&chain, config)?;

    Ok(ProjectContext {
        cwd,
        project_root,
        docs,
    })
}

// ─── Internals ───────────────────────────────────────────────────────────────

/// Walks upward from `cwd` looking for a root marker.
///
/// Returns the first ancestor (inclusive of `cwd`) that contains any marker,
/// or `cwd` itself after `max_walk_depth` hops without a match.
fn find_project_root(cwd: &Path, config: &DiscoveryConfig) -> PathBuf {
    let mut current = cwd.to_owned();
    for depth in 0..=config.max_walk_depth {
        trace!(
            dir = %current.display(),
            depth,
            "checking for root markers"
        );
        if has_any_marker(&current, &config.root_markers) {
            return current;
        }
        match current.parent() {
            Some(parent) => current = parent.to_owned(),
            None => break,
        }
    }
    warn!(
        cwd = %cwd.display(),
        max_depth = config.max_walk_depth,
        "no root marker found within walk depth; falling back to cwd"
    );
    cwd.to_owned()
}

/// Returns `true` if `dir` contains any entry from `markers` (file or dir).
fn has_any_marker(dir: &Path, markers: &[String]) -> bool {
    markers.iter().any(|m| dir.join(m).exists())
}

/// Builds the directory chain from `root` to `cwd` (inclusive), root first.
///
/// If `cwd` is not a descendant of `root` (or equals `root`), the chain
/// contains only `[cwd]`.
fn build_dir_chain(root: &Path, cwd: &Path) -> Vec<PathBuf> {
    if root == cwd {
        return vec![cwd.to_owned()];
    }

    // Collect ancestors from cwd up to (and including) root.
    let mut chain = Vec::new();
    let mut cursor = cwd.to_owned();
    loop {
        chain.push(cursor.clone());
        if cursor == root {
            break;
        }
        match cursor.parent() {
            Some(p) => cursor = p.to_owned(),
            None => {
                // cwd is not under root — return just the cwd
                return vec![cwd.to_owned()];
            }
        }
    }
    chain.reverse(); // root → cwd order
    chain
}

/// Collects doc files from the directory chain, respecting byte budgets.
fn collect_docs(
    chain: &[PathBuf],
    config: &DiscoveryConfig,
) -> Result<Vec<DiscoveredDoc>, DiscoveryError> {
    let mut docs = Vec::new();
    let mut total_loaded: u64 = 0;

    'dirs: for dir in chain {
        for filename in &config.doc_filenames {
            let candidate = dir.join(filename);
            let metadata = match candidate.symlink_metadata() {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(DiscoveryError::Io(error)),
            };

            if metadata.file_type().is_symlink() {
                warn!(
                    path = %candidate.display(),
                    "skipping symlinked project instruction candidate"
                );
                continue;
            }

            if !metadata.is_file() {
                continue;
            }

            if total_loaded >= config.max_total_bytes {
                debug!(
                    total_loaded,
                    max = config.max_total_bytes,
                    "total byte budget exhausted; stopping doc collection"
                );
                break 'dirs;
            }

            let remaining_budget = config.max_total_bytes.saturating_sub(total_loaded);
            let cap = config.max_bytes_per_doc.min(remaining_budget);

            let file_size = metadata.len();
            let was_truncated = file_size > cap;
            let bytes = if cap == 0 {
                Vec::new()
            } else {
                let mut file = std::fs::File::open(&candidate)?.take(cap);
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)?;
                bytes
            };
            let content = if was_truncated {
                warn!(
                    path = %candidate.display(),
                    file_size,
                    cap,
                    "doc file exceeds per-doc cap; truncating"
                );
                truncate_utf8_boundary(bytes, true)?
            } else {
                truncate_utf8_boundary(bytes, false)?
            };

            let loaded_len = content.len() as u64;
            total_loaded += loaded_len;

            debug!(
                path = %candidate.display(),
                bytes = loaded_len,
                total = total_loaded,
                "loaded doc file"
            );

            docs.push(DiscoveredDoc {
                path: candidate,
                filename: filename.clone(),
                content,
            });

            // Only load the first matching filename per directory.
            break;
        }
    }

    Ok(docs)
}

/// Converts bounded file bytes to UTF-8 without splitting a character.
///
/// An incomplete sequence at the end is valid truncation only when the file
/// exceeded the read budget. Invalid UTF-8 elsewhere, or in an untruncated
/// file, remains an error.
fn truncate_utf8_boundary(bytes: Vec<u8>, was_truncated: bool) -> Result<String, DiscoveryError> {
    match String::from_utf8(bytes) {
        Ok(content) => Ok(content),
        Err(error) if was_truncated && error.utf8_error().error_len().is_none() => {
            let valid_up_to = error.utf8_error().valid_up_to();
            String::from_utf8(error.into_bytes()[..valid_up_to].to_vec())
                .map_err(DiscoveryError::Utf8)
        }
        Err(error) => Err(DiscoveryError::Utf8(error)),
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn default_cfg() -> DiscoveryConfig {
        DiscoveryConfig::default()
    }

    fn write(dir: &Path, name: &str, content: &str) {
        fs::write(dir.join(name), content).unwrap();
    }

    fn mkdir(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        fs::create_dir_all(&p).unwrap();
        p
    }

    // ── test_default_config_values ───────────────────────────────────────────

    #[test]
    fn test_default_config_values() {
        let cfg = DiscoveryConfig::default();
        assert!(cfg.root_markers.contains(&".git".to_string()));
        assert!(cfg.root_markers.contains(&"Cargo.toml".to_string()));
        assert!(cfg.root_markers.contains(&"package.json".to_string()));
        assert!(cfg.root_markers.contains(&"pyproject.toml".to_string()));
        assert!(cfg.doc_filenames.contains(&"HARW.md".to_string()));
        assert!(cfg.doc_filenames.contains(&"AGENTS.md".to_string()));
        assert!(cfg.doc_filenames.contains(&"CLAUDE.md".to_string()));
        assert_eq!(cfg.max_bytes_per_doc, 32 * 1024);
        assert_eq!(cfg.max_total_bytes, 128 * 1024);
        assert_eq!(cfg.max_walk_depth, 64);
    }

    // ── test_discover_finds_git_marker_root ──────────────────────────────────

    #[test]
    fn test_discover_finds_git_marker_root() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();

        // Place .git at root level
        mkdir(&root, ".git");

        // cwd is a nested subdirectory
        let sub = mkdir(&root, "src/nested");

        let cfg = default_cfg();
        let ctx = discover_project(&sub, &cfg).unwrap();
        assert_eq!(ctx.project_root, root);
    }

    // ── test_discover_finds_cargo_toml_root ──────────────────────────────────

    #[test]
    fn test_discover_finds_cargo_toml_root() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();

        write(&root, "Cargo.toml", "[package]\nname=\"x\"");

        let sub = mkdir(&root, "src");

        let cfg = DiscoveryConfig::new().with_root_markers(vec!["Cargo.toml".to_string()]);
        let ctx = discover_project(&sub, &cfg).unwrap();
        assert_eq!(ctx.project_root, root);
    }

    // ── test_discover_falls_back_to_cwd_without_marker ───────────────────────

    #[test]
    fn test_discover_falls_back_to_cwd_without_marker() {
        let tmp = TempDir::new().unwrap();
        let cwd = tmp.path().canonicalize().unwrap();

        let cfg =
            DiscoveryConfig::new().with_root_markers(vec!["nonexistent-marker-xyz".to_string()]);
        let ctx = discover_project(&cwd, &cfg).unwrap();
        assert_eq!(ctx.project_root, cwd);
    }

    // ── test_discover_loads_HARW_md_when_present ─────────────────────────────

    #[test]
    fn test_discover_loads_harw_md_when_present() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();

        mkdir(&root, ".git");
        write(&root, "HARW.md", "# Project instructions");

        let cfg = default_cfg();
        let ctx = discover_project(&root, &cfg).unwrap();

        assert_eq!(ctx.docs.len(), 1);
        assert_eq!(ctx.docs[0].filename, "HARW.md");
        assert_eq!(ctx.docs[0].content, "# Project instructions");
    }

    // ── test_discover_prioritizes_HARW_over_AGENTS_in_same_dir ───────────────

    #[test]
    fn test_discover_prioritizes_harw_over_agents_in_same_dir() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();

        mkdir(&root, ".git");
        write(&root, "HARW.md", "HARW content");
        write(&root, "AGENTS.md", "AGENTS content");

        let cfg = default_cfg();
        let ctx = discover_project(&root, &cfg).unwrap();

        // Only one doc per directory; HARW.md is first in the default list
        assert_eq!(ctx.docs.len(), 1);
        assert_eq!(ctx.docs[0].filename, "HARW.md");
        assert_eq!(ctx.docs[0].content, "HARW content");
    }

    // ── test_discover_skips_symlinked_instruction_candidate ─────────────────

    #[cfg(unix)]
    #[test]
    fn test_discover_skips_symlinked_instruction_candidate() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let outside = TempDir::new().unwrap();

        mkdir(&root, ".git");
        write(outside.path(), "outside.md", "outside instructions");
        symlink(outside.path().join("outside.md"), root.join("HARW.md")).unwrap();
        write(&root, "AGENTS.md", "local instructions");

        let ctx = discover_project(&root, &default_cfg()).unwrap();

        assert_eq!(ctx.docs.len(), 1);
        assert_eq!(ctx.docs[0].filename, "AGENTS.md");
        assert_eq!(ctx.docs[0].content, "local instructions");
    }

    // ── test_discover_walks_root_to_cwd_order ────────────────────────────────

    #[test]
    fn test_discover_walks_root_to_cwd_order() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();

        mkdir(&root, ".git");
        let mid = mkdir(&root, "mid");
        let leaf = mkdir(&mid, "leaf");

        write(&root, "HARW.md", "root doc");
        write(&mid, "HARW.md", "mid doc");
        write(&leaf, "HARW.md", "leaf doc");

        let cfg = default_cfg();
        let ctx = discover_project(&leaf, &cfg).unwrap();

        assert_eq!(ctx.docs.len(), 3);
        assert_eq!(ctx.docs[0].content, "root doc");
        assert_eq!(ctx.docs[1].content, "mid doc");
        assert_eq!(ctx.docs[2].content, "leaf doc");
    }

    // ── test_discover_respects_max_total_bytes ───────────────────────────────

    #[test]
    fn test_discover_respects_max_total_bytes() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();

        mkdir(&root, ".git");
        let sub = mkdir(&root, "sub");

        // Each doc is 100 bytes; set total cap to 150 so only the first loads fully,
        // the second is truncated to cap, and nothing else loads.
        write(&root, "HARW.md", "A".repeat(100).as_str());
        write(&sub, "HARW.md", "B".repeat(100).as_str());

        let cfg = DiscoveryConfig {
            max_total_bytes: 150,
            max_bytes_per_doc: 32 * 1024,
            ..default_cfg()
        };

        let ctx = discover_project(&sub, &cfg).unwrap();

        // First doc: 100 bytes — within 150 budget.
        // Second doc: would push to 200 — truncated to 50 remaining bytes.
        // Total docs: 2 (first full, second truncated).
        assert_eq!(ctx.docs.len(), 2);
        assert_eq!(ctx.docs[0].content.len(), 100);
        // Remaining budget after first: 50 bytes
        assert_eq!(ctx.docs[1].content.len(), 50);
    }

    // ── test_discover_truncates_at_utf8_boundary ────────────────────────────

    #[test]
    fn test_discover_truncates_at_utf8_boundary() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();

        mkdir(&root, ".git");
        write(&root, "HARW.md", "abc€tail");

        let cfg = DiscoveryConfig {
            max_bytes_per_doc: 5,
            ..default_cfg()
        };

        let ctx = discover_project(&root, &cfg).unwrap();

        assert_eq!(ctx.docs.len(), 1);
        assert_eq!(ctx.docs[0].content, "abc");
        assert!(ctx.docs[0]
            .content
            .is_char_boundary(ctx.docs[0].content.len()));
    }

    // ── test_discover_invalid_cwd_returns_error ──────────────────────────────

    #[test]
    fn test_discover_invalid_cwd_returns_error() {
        let cfg = default_cfg();
        let result = discover_project(Path::new("/nonexistent/path/xyz123"), &cfg);
        assert!(
            matches!(result, Err(DiscoveryError::InvalidCwd(_))),
            "expected InvalidCwd error"
        );
    }

    // ── test_builder_setters ─────────────────────────────────────────────────

    #[test]
    fn test_builder_setters() {
        let cfg = DiscoveryConfig::new()
            .with_root_markers(vec!["mymarker".to_string()])
            .with_doc_filenames(vec!["MY.md".to_string()]);

        assert_eq!(cfg.root_markers, vec!["mymarker"]);
        assert_eq!(cfg.doc_filenames, vec!["MY.md"]);
    }

    // ── test_discover_no_docs_when_none_present ──────────────────────────────

    #[test]
    fn test_discover_no_docs_when_none_present() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        mkdir(&root, ".git");

        let cfg = default_cfg();
        let ctx = discover_project(&root, &cfg).unwrap();
        assert!(ctx.docs.is_empty());
    }

    // ── test_discover_cwd_equals_root_when_cwd_has_marker ───────────────────

    #[test]
    fn test_discover_cwd_equals_root_when_cwd_has_marker() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        mkdir(&root, ".git");

        let cfg = default_cfg();
        let ctx = discover_project(&root, &cfg).unwrap();
        assert_eq!(ctx.project_root, ctx.cwd);
    }
}
