//! Session-scoped additional workspace roots.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

/// Maximum additional roots registered for a session.
pub const MAX_EXTRA_ROOTS: usize = 8;

/// A validated, canonical additional workspace root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraRoot {
    /// Canonical filesystem path.
    pub path: PathBuf,
    /// Whether this root is also persisted in project configuration.
    pub persisted: bool,
}

/// Shared, session-scoped set of additional workspace roots.
#[derive(Debug, Clone)]
pub struct ExtraRootsCell(Arc<RwLock<Vec<ExtraRoot>>>);

impl Default for ExtraRootsCell {
    fn default() -> Self {
        Self::new()
    }
}

impl PartialEq for ExtraRootsCell {
    fn eq(&self, other: &Self) -> bool {
        self.snapshot() == other.snapshot()
    }
}

impl Eq for ExtraRootsCell {}

impl ExtraRootsCell {
    /// Creates an empty root set.
    #[must_use]
    pub fn new() -> Self {
        Self(Arc::new(RwLock::new(Vec::new())))
    }

    /// Returns a point-in-time copy of the registered roots.
    #[must_use]
    pub fn snapshot(&self) -> Vec<ExtraRoot> {
        self.0.read().map(|roots| roots.clone()).unwrap_or_default()
    }

    /// Returns whether `path` lies under a registered root.
    #[must_use]
    pub fn contains_path(&self, path: &Path) -> bool {
        self.0
            .read()
            .is_ok_and(|roots| roots.iter().any(|root| path.starts_with(&root.path)))
    }

    /// Returns whether every root in this cell is contained by `parent`.
    #[must_use]
    pub fn is_subset_of(&self, parent: &Self) -> bool {
        let roots = self.snapshot();
        let parent_roots = parent.snapshot();
        roots.iter().all(|root| {
            parent_roots
                .iter()
                .any(|parent_root| root.path.starts_with(&parent_root.path))
        })
    }

    /// Validates and adds a root, returning whether it was newly added.
    pub fn add(
        &self,
        path: &Path,
        persisted: bool,
        primary_root: &Path,
        user_home: Option<&Path>,
    ) -> Result<bool, ExtraRootError> {
        let canonical = validate_extra_root(path, primary_root, user_home)?;
        let mut roots = self.0.write().map_err(|_| ExtraRootError::Poisoned)?;
        if let Some(root) = roots
            .iter_mut()
            .find(|root| canonical == root.path || canonical.starts_with(&root.path))
        {
            root.persisted |= persisted;
            return Ok(false);
        }
        if roots.len() >= MAX_EXTRA_ROOTS {
            return Err(ExtraRootError::TooMany {
                max: MAX_EXTRA_ROOTS,
            });
        }
        roots.push(ExtraRoot {
            path: canonical,
            persisted,
        });
        Ok(true)
    }

    /// Removes a canonical root, returning whether it was present.
    pub fn remove(&self, path: &Path) -> bool {
        self.0.write().is_ok_and(|mut roots| {
            let count = roots.len();
            roots.retain(|root| root.path != path);
            roots.len() != count
        })
    }
}

/// Validates and canonicalizes a candidate additional workspace root.
pub fn validate_extra_root(
    candidate: &Path,
    primary_root: &Path,
    user_home: Option<&Path>,
) -> Result<PathBuf, ExtraRootError> {
    let canonical = candidate
        .canonicalize()
        .map_err(|error| ExtraRootError::Io {
            path: candidate.to_path_buf(),
            reason: error.to_string(),
        })?;
    if !canonical.is_dir() {
        return Err(ExtraRootError::NotDirectory { path: canonical });
    }
    if canonical.parent().is_none() {
        return Err(ExtraRootError::RootDirectory);
    }
    let primary = primary_root
        .canonicalize()
        .map_err(|error| ExtraRootError::Io {
            path: primary_root.to_path_buf(),
            reason: error.to_string(),
        })?;
    if user_home
        .is_some_and(|home| home.canonicalize().unwrap_or_else(|_| home.to_path_buf()) == canonical)
    {
        return Err(ExtraRootError::UserHome { path: canonical });
    }
    if primary.starts_with(&canonical) && primary != canonical {
        return Err(ExtraRootError::AncestorOfPrimary { path: canonical });
    }
    if canonical.starts_with(&primary) {
        return Err(ExtraRootError::AlreadyContained { path: canonical });
    }
    Ok(canonical)
}

/// Error returned when managing an additional workspace root.
pub enum ExtraRootError {
    /// A path could not be canonicalized.
    Io { path: PathBuf, reason: String },
    /// The candidate is not a directory.
    NotDirectory { path: PathBuf },
    /// The candidate is the filesystem root.
    RootDirectory,
    /// The candidate is the user's home directory.
    UserHome { path: PathBuf },
    /// The candidate contains the primary workspace root.
    AncestorOfPrimary { path: PathBuf },
    /// The candidate is already inside the primary workspace root.
    AlreadyContained { path: PathBuf },
    /// The session root limit was reached.
    TooMany { max: usize },
    /// The shared state lock was poisoned.
    Poisoned,
}

impl fmt::Display for ExtraRootError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, reason } => write!(
                formatter,
                "could not resolve extra workspace root '{}': {reason}",
                path.display()
            ),
            Self::NotDirectory { path } => {
                write!(formatter, "'{}' is not a directory", path.display())
            }
            Self::RootDirectory => write!(
                formatter,
                "the filesystem root cannot be an extra workspace root"
            ),
            Self::UserHome { path } => write!(
                formatter,
                "'{}' is the user's home directory",
                path.display()
            ),
            Self::AncestorOfPrimary { path } => write!(
                formatter,
                "'{}' contains the primary workspace root",
                path.display()
            ),
            Self::AlreadyContained { path } => write!(
                formatter,
                "'{}' is already within the primary workspace root",
                path.display()
            ),
            Self::TooMany { max } => write!(
                formatter,
                "at most {max} extra workspace roots may be registered"
            ),
            Self::Poisoned => write!(formatter, "extra workspace root state is poisoned"),
        }
    }
}

impl fmt::Debug for ExtraRootError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl std::error::Error for ExtraRootError {}
