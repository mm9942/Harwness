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
//!    configured root marker. First match → `project_root`. The walk never
//!    accepts `$HOME` itself as root unless `cwd` already *is* `$HOME` (a
//!    marker such as a dotfiles `~/.git` or a stray `~/package.json` must not
//!    turn the whole home directory into the workspace — Befund S1/F-028),
//!    and a marker directory is only trusted if it is owned by the current
//!    user and not world-writable (Befund S1/F-028).
//! 3. Collect the directory chain from `project_root` to `cwd` (inclusive),
//!    root first.
//! 4. For each directory in the chain, try each `doc_filenames` entry in order;
//!    load the first one found, subject to per-doc and total byte budgets. A
//!    candidate that cannot be read (e.g. `EACCES`) or is not valid UTF-8 is
//!    skipped with a `warn!` diagnostic instead of aborting the whole
//!    discovery (Befund S2/F-165).
//!
//! # Concurrency
//!
//! All I/O is synchronous (`std::fs`). Safe to call from any thread.
//!
//! # Errors
//!
//! - [`DiscoveryError::InvalidCwd`] — `cwd.canonicalize()` fails. This is the
//!   only error [`discover_project`] currently returns: per-doc I/O/UTF-8
//!   problems are skipped and logged (see point 4 above), not propagated.
//! - [`DiscoveryError::Io`] / [`DiscoveryError::Utf8`] remain part of the
//!   public error type for API stability but are not constructed by this
//!   crate today.

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

/// Default project-root markers used by [`DiscoveryConfig::default`].
///
/// Aligned with the manifest detection of `harw-explorer`
/// (`harw-explorer/src/projects.rs`: Cargo, Node incl. `pnpm-workspace.yaml`,
/// Python `pyproject.toml`/`setup.cfg`/`setup.py`, Go `go.mod`, Git) and
/// extended by further ecosystems whose manifest sits at the project root
/// (Deno, PHP/Composer, Ruby/Bundler, Maven, Gradle) plus Mercurial as a
/// second VCS.
///
/// Only fixed file/directory names are supported (no globs such as `*.sln`).
/// Files that routinely appear in *sub*directories without denoting a
/// project root (e.g. `requirements.txt`, `Makefile`) are deliberately
/// omitted: because the nearest ancestor with any marker wins (see
/// [`DiscoveryConfig::root_markers`]), such a marker would shrink the root
/// to a subfolder and hide the repository-level doc cascade.
///
/// The order carries no precedence semantics between directories; within
/// one directory it merely fixes the probe order (VCS first, then the most
/// common manifests).
pub const DEFAULT_ROOT_MARKERS: &[&str] = &[
    // Versionskontrolle
    ".git",
    ".hg",
    // Rust
    "Cargo.toml",
    // Node / JavaScript / TypeScript
    "package.json",
    "pnpm-workspace.yaml",
    "deno.json",
    "deno.jsonc",
    // Python
    "pyproject.toml",
    "setup.cfg",
    "setup.py",
    // Go
    "go.mod",
    // PHP
    "composer.json",
    // Ruby
    "Gemfile",
    // JVM (Maven / Gradle)
    "pom.xml",
    "settings.gradle",
    "settings.gradle.kts",
    "build.gradle",
    "build.gradle.kts",
];

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
    /// Entries are fixed names (no globs), each tested via `dir.join(marker)`.
    /// Precedence is by *distance*, not by list position: the nearest ancestor
    /// of `cwd` (inclusive) that contains **any** of these entries is the
    /// project root. The order only determines which marker is probed first
    /// inside one directory. Defaults to [`DEFAULT_ROOT_MARKERS`].
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

    /// The current user's home directory, used as an upper boundary for the
    /// root-marker walk (Befund S1/F-028).
    ///
    /// `find_project_root` never climbs past this directory, and never
    /// accepts `home_dir` itself as the project root unless `cwd` already
    /// equals it (there is no narrower candidate left to prefer). This stops
    /// a marker placed directly in the home directory — a dotfiles bare
    /// repo's `~/.git`, or a stray `~/package.json` left behind by an
    /// unrelated `npm install` — from turning the *entire* home directory
    /// into a writable/executable sandbox for an unrelated `cwd` underneath
    /// it.
    ///
    /// Defaults to `$HOME` (read once via [`std::env::var_os`] in
    /// [`DiscoveryConfig::default`]) so production callers need no extra
    /// wiring, but the field itself makes the boundary deterministically
    /// testable via [`DiscoveryConfig::with_home_dir`] without mutating the
    /// process environment (`std::env::set_var`). `None` means no home
    /// boundary is enforced (e.g. `$HOME` is unset).
    pub home_dir: Option<PathBuf>,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        Self {
            root_markers: DEFAULT_ROOT_MARKERS
                .iter()
                .map(|marker| (*marker).to_string())
                .collect(),
            doc_filenames: vec![
                "HARW.md".to_string(),
                "AGENTS.md".to_string(),
                "CLAUDE.md".to_string(),
            ],
            max_bytes_per_doc: 32 * 1024,
            max_total_bytes: 128 * 1024,
            max_walk_depth: 64,
            home_dir: std::env::var_os("HOME").map(PathBuf::from),
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
    /// use harw_project_discovery::discovery::DEFAULT_ROOT_MARKERS;
    /// let cfg = DiscoveryConfig::new();
    /// assert_eq!(cfg.root_markers.len(), DEFAULT_ROOT_MARKERS.len());
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

    /// Replaces the home-directory boundary and returns `self` for chaining.
    ///
    /// # Description
    ///
    /// See [`DiscoveryConfig::home_dir`] for the full rationale. Passing
    /// `None` disables the boundary entirely (the walk may climb past what
    /// would otherwise be `$HOME`).
    ///
    /// # Arguments
    ///
    /// - `home` (`Option<PathBuf>`): the new home-directory boundary.
    ///
    /// # Returns
    ///
    /// `Self` with `home_dir` replaced.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use harw_project_discovery::DiscoveryConfig;
    /// use std::path::PathBuf;
    ///
    /// let cfg = DiscoveryConfig::new().with_home_dir(Some(PathBuf::from("/home/test")));
    /// assert_eq!(cfg.home_dir, Some(PathBuf::from("/home/test")));
    /// ```
    pub fn with_home_dir(mut self, home: Option<PathBuf>) -> Self {
        self.home_dir = home;
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

    // Phase 3 — doc cascade collect. Per-doc I/O/UTF-8 failures are skipped
    // and logged inside `collect_docs`, never propagated (Befund S2/F-165).
    let chain = build_dir_chain(&project_root, &cwd);
    let docs = collect_docs(&chain, config);

    Ok(ProjectContext {
        cwd,
        project_root,
        docs,
    })
}

// ─── Internals ───────────────────────────────────────────────────────────────

/// Walks upward from `cwd` looking for a trusted root marker.
///
/// Returns the first ancestor (inclusive of `cwd`) that contains any marker
/// in a directory owned by the current user and not world-writable, or `cwd`
/// itself after `max_walk_depth` hops (or the `$HOME` boundary) without a
/// match.
///
/// # `$HOME` boundary (Befund S1/F-028)
///
/// If `config.home_dir` is set, the walk never climbs past it, and never
/// accepts `home_dir` itself as the project root unless `cwd` already equals
/// it. Without this, a dotfiles bare repo's `~/.git` or a stray
/// `~/package.json` would silently turn the user's entire home directory
/// into a read/write/execute sandbox for any `cwd` started underneath it —
/// see the module-level docs and `docs/remediation/ledger/W1/W1-07.md`.
fn find_project_root(cwd: &Path, config: &DiscoveryConfig) -> PathBuf {
    // Kanonisieren, damit der Komponentenvergleich mit dem bereits
    // kanonisierten `cwd`-Pfad exakt ist (Symlinks in `$HOME` selbst wären
    // sonst ein Ausweichweg um die Grenze). Schlägt das fehl (z. B. `$HOME`
    // existiert nicht), gibt es keine wirksame Grenze — wie bisher ohne
    // dieses Feld.
    let home_dir = config
        .home_dir
        .as_deref()
        .and_then(|h| h.canonicalize().ok());
    let current_uid = current_uid();

    let mut current = cwd.to_owned();
    for depth in 0..=config.max_walk_depth {
        trace!(
            dir = %current.display(),
            depth,
            "checking for root markers"
        );

        let at_home = home_dir.as_deref() == Some(current.as_path());
        if at_home && current != cwd {
            debug!(
                home = %current.display(),
                cwd = %cwd.display(),
                "reached $HOME boundary above cwd; refusing to treat it as project root"
            );
            break;
        }

        if is_trusted_root_candidate(&current, config, current_uid) {
            return current;
        }

        if at_home {
            // `current == cwd == home_dir`: kein Marker gefunden, und
            // `$HOME` ist ohnehin die Obergrenze der Suche — nicht weiter
            // nach oben laufen.
            break;
        }

        match current.parent() {
            Some(parent) => current = parent.to_owned(),
            None => break,
        }
    }
    warn!(
        cwd = %cwd.display(),
        max_depth = config.max_walk_depth,
        "no trusted root marker found within walk depth; falling back to cwd"
    );
    cwd.to_owned()
}

/// Returns `true` if `dir` contains any entry from `markers` (file or dir).
fn has_any_marker(dir: &Path, markers: &[String]) -> bool {
    markers.iter().any(|m| dir.join(m).exists())
}

/// Returns the current process's real user id, or `None` if it cannot be
/// determined without adding a new dependency.
///
/// `harw-project-discovery`'s `Cargo.toml` is out of scope for this change
/// (see `docs/remediation/ledger/W1/W1-07.md`), so this does not use
/// `rustix::process::geteuid()` the way `harw-fsutil::perm` does. On Linux,
/// `/proc/self` is a symlink whose owning uid is the process's real uid
/// (`proc(5)`), so a plain `std::fs::metadata` stat gives an exact answer
/// without `unsafe` or a libc/rustix dependency. Elsewhere, the
/// owner check in [`is_trusted_root_candidate`] is skipped (`None`); the
/// world-writable check still applies.
#[cfg(target_os = "linux")]
fn current_uid() -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self").ok().map(|m| m.uid())
}

#[cfg(not(target_os = "linux"))]
fn current_uid() -> Option<u32> {
    None
}

/// Returns `true` if `dir` contains any root marker **and** `dir` itself is
/// safe to trust as sandbox authority: owned by `current_uid` (when known)
/// and not world-writable (Befund S1/F-028).
///
/// A directory that fails either check is treated exactly as if it had no
/// marker at all — the walk continues upward (or falls back to `cwd`) rather
/// than erroring, so a hostile ancestor (e.g. a world-writable `/tmp` with a
/// marker planted by another local user) can only narrow authority, never
/// widen it silently.
#[cfg(unix)]
fn is_trusted_root_candidate(
    dir: &Path,
    config: &DiscoveryConfig,
    current_uid: Option<u32>,
) -> bool {
    use std::os::unix::fs::MetadataExt;

    if !has_any_marker(dir, &config.root_markers) {
        return false;
    }

    let metadata = match std::fs::metadata(dir) {
        Ok(metadata) => metadata,
        Err(error) => {
            warn!(
                dir = %dir.display(),
                %error,
                "cannot stat root-marker directory; treating as untrusted"
            );
            return false;
        }
    };

    if let Some(uid) = current_uid {
        if metadata.uid() != uid {
            warn!(
                dir = %dir.display(),
                owner = metadata.uid(),
                current_uid = uid,
                "root-marker directory is not owned by the current user; ignoring marker"
            );
            return false;
        }
    }

    if metadata.mode() & 0o002 != 0 {
        warn!(
            dir = %dir.display(),
            mode = format!("{:o}", metadata.mode() & 0o777),
            "root-marker directory is world-writable; ignoring marker"
        );
        return false;
    }

    true
}

/// Non-Unix fallback: only the marker-presence check applies (no
/// owner/mode metadata via `std::os::unix::fs::MetadataExt` on this
/// platform).
#[cfg(not(unix))]
fn is_trusted_root_candidate(
    dir: &Path,
    config: &DiscoveryConfig,
    _current_uid: Option<u32>,
) -> bool {
    has_any_marker(dir, &config.root_markers)
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
///
/// A candidate that cannot be stat'ed/opened/read (e.g. `EACCES`, deleted
/// mid-walk) or is not valid UTF-8 is skipped with a `warn!` diagnostic and
/// the next `doc_filenames` entry in the same directory is tried instead —
/// exactly like the existing symlink-skip below. Discovery as a whole never
/// aborts because of a single bad doc file (Befund S2/F-165): a foreign
/// repository must not be able to deny service to `harw` (or to child-agent
/// spawns, which re-run discovery) just by shipping an unreadable or
/// mis-encoded `AGENTS.md`.
fn collect_docs(chain: &[PathBuf], config: &DiscoveryConfig) -> Vec<DiscoveredDoc> {
    let mut docs = Vec::new();
    let mut total_loaded: u64 = 0;

    'dirs: for dir in chain {
        for filename in &config.doc_filenames {
            let candidate = dir.join(filename);
            let metadata = match candidate.symlink_metadata() {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    warn!(
                        path = %candidate.display(),
                        %error,
                        "cannot stat project instruction candidate; skipping"
                    );
                    continue;
                }
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
                match read_capped(&candidate, cap) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        warn!(
                            path = %candidate.display(),
                            %error,
                            "cannot read project instruction candidate; skipping"
                        );
                        continue;
                    }
                }
            };

            if was_truncated {
                warn!(
                    path = %candidate.display(),
                    file_size,
                    cap,
                    "doc file exceeds per-doc cap; truncating"
                );
            }

            let content = match truncate_utf8_boundary(bytes, was_truncated) {
                Some(content) => content,
                None => {
                    warn!(
                        path = %candidate.display(),
                        "project instruction candidate is not valid UTF-8; skipping"
                    );
                    continue;
                }
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

    docs
}

/// Opens `path` and reads at most `cap` bytes.
fn read_capped(path: &Path, cap: u64) -> io::Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)?.take(cap);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Converts bounded file bytes to UTF-8 without splitting a character.
///
/// An incomplete sequence at the end is valid truncation only when the file
/// exceeded the read budget. Invalid UTF-8 elsewhere, or in an untruncated
/// file, yields `None` — the caller skips the candidate and logs a
/// diagnostic instead of aborting discovery (Befund S2/F-165).
fn truncate_utf8_boundary(bytes: Vec<u8>, was_truncated: bool) -> Option<String> {
    match String::from_utf8(bytes) {
        Ok(content) => Some(content),
        Err(error) if was_truncated && error.utf8_error().error_len().is_none() => {
            let valid_up_to = error.utf8_error().valid_up_to();
            String::from_utf8(error.into_bytes()[..valid_up_to].to_vec()).ok()
        }
        Err(_) => None,
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use std::fs;
    use tempfile::TempDir;

    fn default_cfg() -> DiscoveryConfig {
        DiscoveryConfig::default()
    }

    fn write(dir: &Path, name: &str, content: &str) -> TestResult {
        fs::write(dir.join(name), content).map_err(ctx("Datei schreiben"))?;
        Ok(())
    }

    fn mkdir(dir: &Path, name: &str) -> TestResult<PathBuf> {
        let p = dir.join(name);
        fs::create_dir_all(&p).map_err(ctx("Verzeichnis anlegen"))?;
        Ok(p)
    }

    // ── test_default_config_values ───────────────────────────────────────────

    #[test]
    fn test_default_config_values() {
        let cfg = DiscoveryConfig::default();
        assert!(cfg.root_markers.contains(&".git".to_string()));
        assert!(cfg.root_markers.contains(&"Cargo.toml".to_string()));
        assert!(cfg.root_markers.contains(&"package.json".to_string()));
        assert!(cfg.root_markers.contains(&"pyproject.toml".to_string()));
        for marker in [
            "go.mod",
            "pnpm-workspace.yaml",
            "deno.json",
            "setup.py",
            "setup.cfg",
            "composer.json",
            "Gemfile",
            "pom.xml",
            "build.gradle",
            "build.gradle.kts",
        ] {
            assert!(
                cfg.root_markers.iter().any(|m| m == marker),
                "missing default marker {marker}"
            );
        }
        assert!(!cfg.root_markers.iter().any(|m| m == "requirements.txt"));
        assert_eq!(cfg.root_markers.len(), DEFAULT_ROOT_MARKERS.len());
        assert!(cfg.doc_filenames.contains(&"HARW.md".to_string()));
        assert!(cfg.doc_filenames.contains(&"AGENTS.md".to_string()));
        assert!(cfg.doc_filenames.contains(&"CLAUDE.md".to_string()));
        assert_eq!(cfg.max_bytes_per_doc, 32 * 1024);
        assert_eq!(cfg.max_total_bytes, 128 * 1024);
        assert_eq!(cfg.max_walk_depth, 64);
    }

    // ── test_discover_finds_git_marker_root ──────────────────────────────────

    #[test]
    fn test_discover_finds_git_marker_root() -> TestResult {
        let tmp = TempDir::new().map_err(ctx("Tempdir anlegen"))?;
        let root = tmp
            .path()
            .canonicalize()
            .map_err(ctx("Pfad kanonisieren"))?;

        // Place .git at root level
        mkdir(&root, ".git")?;

        // cwd is a nested subdirectory
        let sub = mkdir(&root, "src/nested")?;

        let cfg = default_cfg();
        let ctx_result = discover_project(&sub, &cfg).map_err(ctx("Discovery"))?;
        assert_eq!(ctx_result.project_root, root);
        Ok(())
    }

    // ── test_discover_finds_cargo_toml_root ──────────────────────────────────

    #[test]
    fn test_discover_finds_cargo_toml_root() -> TestResult {
        let tmp = TempDir::new().map_err(ctx("Tempdir anlegen"))?;
        let root = tmp
            .path()
            .canonicalize()
            .map_err(ctx("Pfad kanonisieren"))?;

        write(&root, "Cargo.toml", "[package]\nname=\"x\"")?;

        let sub = mkdir(&root, "src")?;

        let cfg = DiscoveryConfig::new().with_root_markers(vec!["Cargo.toml".to_string()]);
        let ctx_result = discover_project(&sub, &cfg).map_err(ctx("Discovery"))?;
        assert_eq!(ctx_result.project_root, root);
        Ok(())
    }

    // ── test_discover_finds_extended_default_markers ─────────────────────────

    #[test]
    fn test_discover_finds_extended_default_markers() -> TestResult {
        for marker in [
            "go.mod",
            "pom.xml",
            "Gemfile",
            "build.gradle.kts",
            "deno.json",
        ] {
            let tmp = TempDir::new().map_err(ctx("Tempdir anlegen"))?;
            let root = tmp
                .path()
                .canonicalize()
                .map_err(ctx("Pfad kanonisieren"))?;
            write(&root, marker, "")?;
            let sub = mkdir(&root, "src/deep")?;

            let ctx_result = discover_project(&sub, &default_cfg()).map_err(ctx("Discovery"))?;
            assert_eq!(ctx_result.project_root, root, "marker {marker}");
        }
        Ok(())
    }

    // ── test_discover_nearest_marker_wins_over_outer_git ─────────────────────

    #[test]
    fn test_discover_nearest_marker_wins_over_outer_git() -> TestResult {
        let tmp = TempDir::new().map_err(ctx("Tempdir anlegen"))?;
        let root = tmp
            .path()
            .canonicalize()
            .map_err(ctx("Pfad kanonisieren"))?;
        mkdir(&root, ".git")?;
        let module = mkdir(&root, "services/api")?;
        write(&module, "go.mod", "module example.com/api\n")?;
        let cwd = mkdir(&module, "internal")?;

        let ctx_result = discover_project(&cwd, &default_cfg()).map_err(ctx("Discovery"))?;
        assert_eq!(ctx_result.project_root, module);
        Ok(())
    }

    // ── test_discover_requirements_txt_is_not_a_marker ───────────────────────

    #[test]
    fn test_discover_requirements_txt_is_not_a_marker() -> TestResult {
        let tmp = TempDir::new().map_err(ctx("Tempdir anlegen"))?;
        let root = tmp
            .path()
            .canonicalize()
            .map_err(ctx("Pfad kanonisieren"))?;
        mkdir(&root, ".git")?;
        let docs = mkdir(&root, "docs")?;
        write(&docs, "requirements.txt", "sphinx\n")?;

        let ctx_result = discover_project(&docs, &default_cfg()).map_err(ctx("Discovery"))?;
        assert_eq!(ctx_result.project_root, root);
        Ok(())
    }

    // ── test_discover_falls_back_to_cwd_without_marker ───────────────────────

    #[test]
    fn test_discover_falls_back_to_cwd_without_marker() -> TestResult {
        let tmp = TempDir::new().map_err(ctx("Tempdir anlegen"))?;
        let cwd = tmp
            .path()
            .canonicalize()
            .map_err(ctx("Pfad kanonisieren"))?;

        let cfg =
            DiscoveryConfig::new().with_root_markers(vec!["nonexistent-marker-xyz".to_string()]);
        let ctx_result = discover_project(&cwd, &cfg).map_err(ctx("Discovery"))?;
        assert_eq!(ctx_result.project_root, cwd);
        Ok(())
    }

    // ── test_discover_loads_HARW_md_when_present ─────────────────────────────

    #[test]
    fn test_discover_loads_harw_md_when_present() -> TestResult {
        let tmp = TempDir::new().map_err(ctx("Tempdir anlegen"))?;
        let root = tmp
            .path()
            .canonicalize()
            .map_err(ctx("Pfad kanonisieren"))?;

        mkdir(&root, ".git")?;
        write(&root, "HARW.md", "# Project instructions")?;

        let cfg = default_cfg();
        let ctx_result = discover_project(&root, &cfg).map_err(ctx("Discovery"))?;

        assert_eq!(ctx_result.docs.len(), 1);
        assert_eq!(ctx_result.docs[0].filename, "HARW.md");
        assert_eq!(ctx_result.docs[0].content, "# Project instructions");
        Ok(())
    }

    // ── test_discover_prioritizes_HARW_over_AGENTS_in_same_dir ───────────────

    #[test]
    fn test_discover_prioritizes_harw_over_agents_in_same_dir() -> TestResult {
        let tmp = TempDir::new().map_err(ctx("Tempdir anlegen"))?;
        let root = tmp
            .path()
            .canonicalize()
            .map_err(ctx("Pfad kanonisieren"))?;

        mkdir(&root, ".git")?;
        write(&root, "HARW.md", "HARW content")?;
        write(&root, "AGENTS.md", "AGENTS content")?;

        let cfg = default_cfg();
        let ctx_result = discover_project(&root, &cfg).map_err(ctx("Discovery"))?;

        // Only one doc per directory; HARW.md is first in the default list
        assert_eq!(ctx_result.docs.len(), 1);
        assert_eq!(ctx_result.docs[0].filename, "HARW.md");
        assert_eq!(ctx_result.docs[0].content, "HARW content");
        Ok(())
    }

    // ── test_discover_skips_symlinked_instruction_candidate ─────────────────

    #[cfg(unix)]
    #[test]
    fn test_discover_skips_symlinked_instruction_candidate() -> TestResult {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().map_err(ctx("Tempdir anlegen"))?;
        let root = tmp
            .path()
            .canonicalize()
            .map_err(ctx("Pfad kanonisieren"))?;
        let outside = TempDir::new().map_err(ctx("Tempdir anlegen"))?;

        mkdir(&root, ".git")?;
        write(outside.path(), "outside.md", "outside instructions")?;
        symlink(outside.path().join("outside.md"), root.join("HARW.md"))
            .map_err(ctx("Symlink anlegen"))?;
        write(&root, "AGENTS.md", "local instructions")?;

        let ctx_result = discover_project(&root, &default_cfg()).map_err(ctx("Discovery"))?;

        assert_eq!(ctx_result.docs.len(), 1);
        assert_eq!(ctx_result.docs[0].filename, "AGENTS.md");
        assert_eq!(ctx_result.docs[0].content, "local instructions");
        Ok(())
    }

    // ── test_discover_walks_root_to_cwd_order ────────────────────────────────

    #[test]
    fn test_discover_walks_root_to_cwd_order() -> TestResult {
        let tmp = TempDir::new().map_err(ctx("Tempdir anlegen"))?;
        let root = tmp
            .path()
            .canonicalize()
            .map_err(ctx("Pfad kanonisieren"))?;

        mkdir(&root, ".git")?;
        let mid = mkdir(&root, "mid")?;
        let leaf = mkdir(&mid, "leaf")?;

        write(&root, "HARW.md", "root doc")?;
        write(&mid, "HARW.md", "mid doc")?;
        write(&leaf, "HARW.md", "leaf doc")?;

        let cfg = default_cfg();
        let ctx_result = discover_project(&leaf, &cfg).map_err(ctx("Discovery"))?;

        assert_eq!(ctx_result.docs.len(), 3);
        assert_eq!(ctx_result.docs[0].content, "root doc");
        assert_eq!(ctx_result.docs[1].content, "mid doc");
        assert_eq!(ctx_result.docs[2].content, "leaf doc");
        Ok(())
    }

    // ── test_discover_respects_max_total_bytes ───────────────────────────────

    #[test]
    fn test_discover_respects_max_total_bytes() -> TestResult {
        let tmp = TempDir::new().map_err(ctx("Tempdir anlegen"))?;
        let root = tmp
            .path()
            .canonicalize()
            .map_err(ctx("Pfad kanonisieren"))?;

        mkdir(&root, ".git")?;
        let sub = mkdir(&root, "sub")?;

        // Each doc is 100 bytes; set total cap to 150 so only the first loads fully,
        // the second is truncated to cap, and nothing else loads.
        write(&root, "HARW.md", "A".repeat(100).as_str())?;
        write(&sub, "HARW.md", "B".repeat(100).as_str())?;

        let cfg = DiscoveryConfig {
            max_total_bytes: 150,
            max_bytes_per_doc: 32 * 1024,
            ..default_cfg()
        };

        let ctx_result = discover_project(&sub, &cfg).map_err(ctx("Discovery"))?;

        // First doc: 100 bytes — within 150 budget.
        // Second doc: would push to 200 — truncated to 50 remaining bytes.
        // Total docs: 2 (first full, second truncated).
        assert_eq!(ctx_result.docs.len(), 2);
        assert_eq!(ctx_result.docs[0].content.len(), 100);
        // Remaining budget after first: 50 bytes
        assert_eq!(ctx_result.docs[1].content.len(), 50);
        Ok(())
    }

    // ── test_discover_truncates_at_utf8_boundary ────────────────────────────

    #[test]
    fn test_discover_truncates_at_utf8_boundary() -> TestResult {
        let tmp = TempDir::new().map_err(ctx("Tempdir anlegen"))?;
        let root = tmp
            .path()
            .canonicalize()
            .map_err(ctx("Pfad kanonisieren"))?;

        mkdir(&root, ".git")?;
        write(&root, "HARW.md", "abc€tail")?;

        let cfg = DiscoveryConfig {
            max_bytes_per_doc: 5,
            ..default_cfg()
        };

        let ctx_result = discover_project(&root, &cfg).map_err(ctx("Discovery"))?;

        assert_eq!(ctx_result.docs.len(), 1);
        assert_eq!(ctx_result.docs[0].content, "abc");
        assert!(
            ctx_result.docs[0]
                .content
                .is_char_boundary(ctx_result.docs[0].content.len())
        );
        Ok(())
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
    fn test_discover_no_docs_when_none_present() -> TestResult {
        let tmp = TempDir::new().map_err(ctx("Tempdir anlegen"))?;
        let root = tmp
            .path()
            .canonicalize()
            .map_err(ctx("Pfad kanonisieren"))?;
        mkdir(&root, ".git")?;

        let cfg = default_cfg();
        let ctx_result = discover_project(&root, &cfg).map_err(ctx("Discovery"))?;
        assert!(ctx_result.docs.is_empty());
        Ok(())
    }

    // ── test_discover_cwd_equals_root_when_cwd_has_marker ───────────────────

    #[test]
    fn test_discover_cwd_equals_root_when_cwd_has_marker() -> TestResult {
        let tmp = TempDir::new().map_err(ctx("Tempdir anlegen"))?;
        let root = tmp
            .path()
            .canonicalize()
            .map_err(ctx("Pfad kanonisieren"))?;
        mkdir(&root, ".git")?;

        let cfg = default_cfg();
        let ctx_result = discover_project(&root, &cfg).map_err(ctx("Discovery"))?;
        assert_eq!(ctx_result.project_root, ctx_result.cwd);
        Ok(())
    }

    // ── W1-07 (F-028/S1): $HOME-Grenze ───────────────────────────────────────

    #[test]
    fn test_home_dir_with_git_is_not_promoted_to_root_for_descendant_cwd() -> TestResult {
        // Simuliert ein Dotfiles-Bare-Repo `~/.git` (oder ein verirrtes
        // `~/package.json`): der Nutzer startet in einem Unterverzeichnis
        // von `$HOME`, ohne eigenen Marker. Ohne die Home-Grenze würde die
        // Suche `$HOME` als Root wählen und damit das gesamte Home
        // beschreib-/ausführbar machen.
        let home = TempDir::new().map_err(ctx("Tempdir anlegen"))?;
        let home_path = home
            .path()
            .canonicalize()
            .map_err(ctx("Pfad kanonisieren"))?;
        mkdir(&home_path, ".git")?;

        let sub = mkdir(&home_path, "Downloads/tool-xyz")?;

        let cfg = DiscoveryConfig {
            home_dir: Some(home_path.clone()),
            ..default_cfg()
        };

        let ctx_result = discover_project(&sub, &cfg).map_err(ctx("Discovery"))?;

        // Bestehende Semantik ohne Marker: cwd wird Root.
        assert_eq!(ctx_result.project_root, sub);
        assert_ne!(ctx_result.project_root, home_path);
        Ok(())
    }

    #[test]
    fn test_home_dir_itself_may_be_root_when_cwd_equals_home() -> TestResult {
        // Startet der Nutzer harw direkt in `$HOME` (cwd == home), gibt es
        // keine engere Wahl als `$HOME` selbst — ein dort liegender Marker
        // wird akzeptiert (mit den normalen Eigentümer-/Rechteprüfungen).
        let home = TempDir::new().map_err(ctx("Tempdir anlegen"))?;
        let home_path = home
            .path()
            .canonicalize()
            .map_err(ctx("Pfad kanonisieren"))?;
        mkdir(&home_path, ".git")?;

        let cfg = DiscoveryConfig {
            home_dir: Some(home_path.clone()),
            ..default_cfg()
        };

        let ctx_result = discover_project(&home_path, &cfg).map_err(ctx("Discovery"))?;
        assert_eq!(ctx_result.project_root, home_path);
        Ok(())
    }

    #[test]
    fn test_home_dir_none_disables_boundary() -> TestResult {
        // `with_home_dir(None)` schaltet die Grenze ausdrücklich ab — Marker
        // in `$HOME` verhalten sich dann wie jeder andere Vorfahre.
        let home = TempDir::new().map_err(ctx("Tempdir anlegen"))?;
        let home_path = home
            .path()
            .canonicalize()
            .map_err(ctx("Pfad kanonisieren"))?;
        mkdir(&home_path, ".git")?;
        let sub = mkdir(&home_path, "sub")?;

        let cfg = DiscoveryConfig::new()
            .with_root_markers(vec![".git".to_string()])
            .with_home_dir(None);

        let ctx_result = discover_project(&sub, &cfg).map_err(ctx("Discovery"))?;
        assert_eq!(ctx_result.project_root, home_path);
        Ok(())
    }

    // ── W1-07 (F-028/S1): Eigentümer/Weltschreibbarkeit ──────────────────────

    #[cfg(unix)]
    #[test]
    fn test_world_writable_marker_dir_is_ignored() -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().map_err(ctx("Tempdir anlegen"))?;
        let root = tmp
            .path()
            .canonicalize()
            .map_err(ctx("Pfad kanonisieren"))?;
        mkdir(&root, ".git")?;
        // Simuliert z. B. ein weltbeschreibbares `/tmp`, in das ein anderer
        // lokaler Nutzer einen Marker legen konnte.
        fs::set_permissions(&root, fs::Permissions::from_mode(0o777))
            .map_err(ctx("Rechte setzen"))?;

        let sub = mkdir(&root, "sub")?;

        // `max_walk_depth: 1` begrenzt die Suche auf `sub` (cwd) und `root`,
        // damit der Test nicht von zufälligen Markern in echten
        // Vorfahren-Verzeichnissen des Testsystems abhängt.
        let cfg = DiscoveryConfig {
            max_walk_depth: 1,
            ..default_cfg()
        };

        let ctx_result = discover_project(&sub, &cfg).map_err(ctx("Discovery"))?;

        // Der weltbeschreibbare Marker-Ordner wird ignoriert; ohne weiteren
        // (vertrauenswürdigen) Marker fällt die Suche auf cwd zurück.
        assert_eq!(ctx_result.project_root, sub);

        // Aufräumen, damit TempDir sich beim Drop löschen lässt (0o777 auf
        // dem Wurzelverzeichnis stört das Aufräumen selbst nicht, aber
        // restriktivere Testumgebungen könnten empfindlich sein).
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .map_err(ctx("Rechte setzen"))?;
        Ok(())
    }

    // ── W1-07 (F-165/S2): einzelne Dokumente überspringen statt abbrechen ───

    #[cfg(unix)]
    #[test]
    fn test_discover_skips_unreadable_doc_and_loads_rest() -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().map_err(ctx("Tempdir anlegen"))?;
        let root = tmp
            .path()
            .canonicalize()
            .map_err(ctx("Pfad kanonisieren"))?;
        mkdir(&root, ".git")?;
        let mid = mkdir(&root, "mid")?;

        write(&root, "HARW.md", "unreadable")?;
        fs::set_permissions(root.join("HARW.md"), fs::Permissions::from_mode(0o000))
            .map_err(ctx("Rechte setzen"))?;
        write(&mid, "HARW.md", "readable mid doc")?;

        // Läuft der Test mit DAC-Override (z. B. als root im Container), macht
        // `0o000` die Datei nicht unlesbar. Dann wird derselbe Fehlerpfad
        // (I/O-Fehler an einem einzelnen Kandidaten, kein `NotFound`) anders
        // ausgelöst: ein Kandidatenname über `NAME_MAX` (255 Byte) lässt
        // `symlink_metadata` deterministisch mit `ENAMETOOLONG` scheitern —
        // in *jedem* Verzeichnis der Kette, auch vor dem lesbaren `mid`-Dokument.
        // Das Root-Dokument wird in diesem Fall entfernt, damit es nicht
        // (lesbar) mitgezählt wird.
        let permissions_enforced = fs::File::open(root.join("HARW.md")).is_err();
        let cfg = if permissions_enforced {
            default_cfg()
        } else {
            fs::remove_file(root.join("HARW.md")).map_err(ctx("Root-Dokument entfernen"))?;
            let mut names = vec!["x".repeat(300)];
            names.extend(default_cfg().doc_filenames);
            default_cfg().with_doc_filenames(names)
        };

        let ctx_result = discover_project(&mid, &cfg).map_err(ctx("Discovery"))?;

        // Der fehlerhafte Kandidat wird übersprungen (kein Abbruch der
        // gesamten Discovery); das Dokument aus `mid` lädt trotzdem.
        assert_eq!(ctx_result.docs.len(), 1);
        assert_eq!(ctx_result.docs[0].content, "readable mid doc");

        // Aufräumen, damit TempDir sich beim Drop löschen lässt.
        if permissions_enforced {
            fs::set_permissions(root.join("HARW.md"), fs::Permissions::from_mode(0o644))
                .map_err(ctx("Rechte setzen"))?;
        }
        Ok(())
    }

    #[test]
    fn test_discover_skips_invalid_utf8_doc_and_loads_rest() -> TestResult {
        let tmp = TempDir::new().map_err(ctx("Tempdir anlegen"))?;
        let root = tmp
            .path()
            .canonicalize()
            .map_err(ctx("Pfad kanonisieren"))?;
        mkdir(&root, ".git")?;
        let mid = mkdir(&root, "mid")?;

        // Ungültige UTF-8-Bytes, NICHT am Ende abgeschnitten (kein
        // Truncation-Sonderfall) — muss trotzdem übersprungen statt die
        // Discovery abbrechen zu lassen.
        fs::write(root.join("HARW.md"), [0x48, 0x41, 0xff, 0xfe, 0x21])
            .map_err(ctx("Datei schreiben"))?;
        write(&mid, "HARW.md", "readable mid doc")?;

        let ctx_result = discover_project(&mid, &default_cfg()).map_err(ctx("Discovery"))?;

        assert_eq!(ctx_result.docs.len(), 1);
        assert_eq!(ctx_result.docs[0].content, "readable mid doc");
        Ok(())
    }

    // ── with_home_dir builder ─────────────────────────────────────────────────

    #[test]
    fn test_with_home_dir_builder_setter() {
        let cfg = DiscoveryConfig::new().with_home_dir(Some(PathBuf::from("/home/test")));
        assert_eq!(cfg.home_dir, Some(PathBuf::from("/home/test")));

        let cfg = cfg.with_home_dir(None);
        assert_eq!(cfg.home_dir, None);
    }
}
