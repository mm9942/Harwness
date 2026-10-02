//! Policy, report and error types of a single directory sweep.

use std::borrow::Cow;
use std::fmt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Simple file-name matcher: optional prefix, suffix and substring, no globs.
///
/// A name matches when it starts with `prefix`, ends with `suffix` and
/// contains `contains` (each only if set). [`NameMatch::any`] matches every
/// name.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NameMatch {
    /// Required name prefix, if any.
    pub prefix: Option<Cow<'static, str>>,
    /// Required name suffix, if any.
    pub suffix: Option<Cow<'static, str>>,
    /// Required substring, if any.
    pub contains: Option<Cow<'static, str>>,
}

impl NameMatch {
    /// Matches every file name.
    #[must_use]
    pub const fn any() -> Self {
        Self {
            prefix: None,
            suffix: None,
            contains: None,
        }
    }

    /// Matches names starting with `prefix`.
    #[must_use]
    pub const fn prefix(prefix: &'static str) -> Self {
        Self {
            prefix: Some(Cow::Borrowed(prefix)),
            ..Self::any()
        }
    }

    /// Matches names ending with `suffix`.
    #[must_use]
    pub const fn suffix(suffix: &'static str) -> Self {
        Self {
            suffix: Some(Cow::Borrowed(suffix)),
            ..Self::any()
        }
    }

    /// Matches names containing `needle`.
    #[must_use]
    pub const fn contains(needle: &'static str) -> Self {
        Self {
            contains: Some(Cow::Borrowed(needle)),
            ..Self::any()
        }
    }

    /// Matches names starting with `prefix` and ending with `suffix`.
    #[must_use]
    pub const fn prefix_suffix(prefix: &'static str, suffix: &'static str) -> Self {
        Self {
            prefix: Some(Cow::Borrowed(prefix)),
            suffix: Some(Cow::Borrowed(suffix)),
            ..Self::any()
        }
    }

    /// Whether `name` (a bare file name) matches.
    #[must_use]
    pub fn matches(&self, name: &str) -> bool {
        self.prefix.as_deref().is_none_or(|p| name.starts_with(p))
            && self.suffix.as_deref().is_none_or(|s| name.ends_with(s))
            && self.contains.as_deref().is_none_or(|c| name.contains(c))
    }
}

/// Limits and switches of one sweep over one directory.
///
/// Enforcement order is age, then count, then bytes; the `keep_newest`
/// newest matching files are never removed by any rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// Remove matching files whose mtime is older than this. `None` = no age rule.
    pub max_age: Option<Duration>,
    /// Keep at most this many total bytes of matching files. `None` = no byte rule.
    pub max_bytes: Option<u64>,
    /// Keep at most this many matching files. `None` = no count rule.
    pub max_files: Option<usize>,
    /// The newest N matching files are exempt from every rule.
    pub keep_newest: usize,
    /// Which directory entries the policy applies to.
    pub name_match: NameMatch,
    /// Plan only: report what would be removed, delete nothing.
    pub dry_run: bool,
    /// Wall-clock cut-off; checked before every delete. `None` = unbounded.
    pub deadline: Option<Instant>,
}

impl RetentionPolicy {
    /// A policy with no limits that matches everything and keeps the newest
    /// file (`keep_newest = 1`), applying for real.
    #[must_use]
    pub const fn unlimited() -> Self {
        Self {
            max_age: None,
            max_bytes: None,
            max_files: None,
            keep_newest: 1,
            name_match: NameMatch::any(),
            dry_run: false,
            deadline: None,
        }
    }
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self::unlimited()
    }
}

/// Which rule selected a file for removal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovalReason {
    /// Older than `max_age`.
    Age,
    /// Over `max_files` (oldest first).
    Count,
    /// Over `max_bytes` (oldest first).
    Bytes,
}

impl RemovalReason {
    /// Stable lowercase label for logs and CLI output.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Age => "age",
            Self::Count => "count",
            Self::Bytes => "bytes",
        }
    }
}

/// One file removed (or, in a dry run, that would be removed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal {
    /// Full path of the file.
    pub path: PathBuf,
    /// Size in bytes at plan time.
    pub bytes: u64,
    /// The rule that selected it.
    pub reason: RemovalReason,
}

/// A per-file problem; the sweep continues past it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemError {
    /// File the problem concerns.
    pub path: PathBuf,
    /// Rendered cause.
    pub message: String,
}

/// Outcome of a sweep. Partial when `timed_out`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    /// Files removed, or planned for removal in a dry run, in plan order
    /// (oldest first).
    pub removed: Vec<Removal>,
    /// Matching files that remain (including planned removals the deadline
    /// cut off).
    pub kept: usize,
    /// Total bytes of the kept files.
    pub kept_bytes: u64,
    /// Entries ignored without being matched: symlinks, non-regular files,
    /// `*.lock`, dot-temp files and non-UTF-8 names.
    pub skipped: usize,
    /// Per-file failures (the sweep continued).
    pub errors: Vec<ItemError>,
    /// The deadline expired before all planned removals ran.
    pub timed_out: bool,
    /// This was a dry run: nothing was deleted.
    pub dry_run: bool,
}

impl Report {
    /// Sum of `bytes` over [`Report::removed`].
    #[must_use]
    pub fn removed_bytes(&self) -> u64 {
        self.removed.iter().map(|r| r.bytes).sum()
    }
}

/// Whether a sweep deletes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepMode {
    /// Plan and report only.
    DryRun,
    /// Delete for real.
    Apply,
}

/// Failure of a whole sweep (per-file failures live in [`Report::errors`]).
#[derive(Debug)]
pub enum RetentionError {
    /// The directory could not be inspected or listed.
    Io {
        /// The directory.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// The sweep target is not a real directory (a symlink or a file).
    NotADirectory(PathBuf),
    /// A security-relevant class was asked to apply without explicit opt-in.
    OptInRequired(&'static str),
    /// An ephemeral class is switched off (`enabled = false`) and was asked
    /// to apply.
    Disabled(&'static str),
    /// No class with this id is registered.
    UnknownClass(String),
}

impl fmt::Display for RetentionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => {
                write!(f, "retention: cannot read {}: {source}", path.display())
            }
            Self::NotADirectory(path) => write!(
                f,
                "retention: {} is not a real directory (symlinks are never followed)",
                path.display()
            ),
            Self::OptInRequired(id) => write!(
                f,
                "retention: class `{id}` is security-relevant; deletion needs explicit opt-in"
            ),
            Self::Disabled(id) => write!(f, "retention: class `{id}` is disabled"),
            Self::UnknownClass(id) => write!(f, "retention: unknown class `{id}`"),
        }
    }
}

impl std::error::Error for RetentionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}
