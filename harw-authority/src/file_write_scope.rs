//! Lexical exact-file write ceilings. These are requests, never grants.
//! Filesystem callers must additionally resolve paths beneath their workspace,
//! deny symlink races, and enforce live ownership/preimage checks where needed.

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

const MAX_FILES: usize = 256;
const MAX_PATH_BYTES: usize = 4096;

/// A narrowing-only write ceiling. Private representation prevents constructing
/// unchecked paths. `Workspace` compatibility is represented by `workspace()`;
/// an empty exact set means no writable files, not unrestricted access.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileWriteScope {
    files: Option<BTreeSet<String>>,
}

/// Invalid or oversized exact-file write scope. Contains no supplied path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileWriteScopeError;

impl fmt::Display for FileWriteScopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("write scope must contain at most 256 plain relative file paths")
    }
}

impl std::error::Error for FileWriteScopeError {}

impl FileWriteScope {
    /// Legacy workspace-wide ceiling; this does not grant WriteWorkspace.
    #[must_use]
    pub fn workspace() -> Self {
        Self::default()
    }

    /// Exact workspace-relative files, not prefixes or patterns. Rejects
    /// ambiguous spellings instead of normalizing them into another grant.
    ///
    /// # Errors
    /// Invalid paths or more than 256 supplied entries.
    pub fn exact_files<I, S>(paths: I) -> Result<Self, FileWriteScopeError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut files = BTreeSet::new();
        for (index, path) in paths.into_iter().enumerate() {
            let path = path.into();
            if index >= MAX_FILES || !valid_path(&path) {
                return Err(FileWriteScopeError);
            }
            files.insert(path);
        }
        Ok(Self { files: Some(files) })
    }

    #[must_use]
    pub fn is_workspace(&self) -> bool {
        self.files.is_none()
    }

    /// Exact lexical membership. The caller must still validate the resolved
    /// filesystem path. Non-UTF8 and non-canonical lexical spellings fail closed.
    #[must_use]
    pub fn allows_path(&self, path: &Path) -> bool {
        let Some(path) = path.to_str() else {
            return false;
        };
        valid_path(path) && self.files.as_ref().is_none_or(|files| files.contains(path))
    }

    #[must_use]
    pub fn intersection(&self, other: &Self) -> Self {
        match (&self.files, &other.files) {
            (None, _) => other.clone(),
            (_, None) => self.clone(),
            (Some(left), Some(right)) => Self {
                files: Some(left.intersection(right).cloned().collect()),
            },
        }
    }

    #[must_use]
    pub fn is_subset_of(&self, parent: &Self) -> bool {
        match (&self.files, &parent.files) {
            (_, None) => true,
            (None, Some(_)) => false,
            (Some(child), Some(parent)) => child.is_subset(parent),
        }
    }
}

fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_PATH_BYTES
        && !path.chars().any(char::is_control)
        && !path.contains(['\\', ':'])
        && path.split('/').all(|part| !matches!(part, "" | "." | ".."))
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ScopeRef<'a> {
    Workspace,
    ExactFiles { paths: &'a BTreeSet<String> },
}

impl Serialize for FileWriteScope {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.files {
            None => ScopeRef::Workspace.serialize(serializer),
            Some(paths) => ScopeRef::ExactFiles { paths }.serialize(serializer),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ScopeKind {
    Workspace,
    ExactFiles,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeWire {
    kind: ScopeKind,
    paths: Option<ScopePaths>,
}

// Enforce the collection bound during deserialization, before accumulating an
// arbitrarily large Vec from an untrusted request/snapshot.
struct ScopePaths(Vec<String>);

impl<'de> Deserialize<'de> for ScopePaths {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct PathsVisitor;
        impl<'de> serde::de::Visitor<'de> for PathsVisitor {
            type Value = ScopePaths;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("at most 256 plain relative file paths")
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                if seq.size_hint().is_some_and(|size| size > MAX_FILES) {
                    return Err(serde::de::Error::custom(FileWriteScopeError));
                }
                let mut paths = Vec::new();
                while let Some(path) = seq.next_element::<String>()? {
                    if paths.len() >= MAX_FILES || !valid_path(&path) {
                        return Err(serde::de::Error::custom(FileWriteScopeError));
                    }
                    paths.push(path);
                }
                Ok(ScopePaths(paths))
            }
        }
        deserializer.deserialize_seq(PathsVisitor)
    }
}

impl<'de> Deserialize<'de> for FileWriteScope {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = ScopeWire::deserialize(deserializer)?;
        match (wire.kind, wire.paths) {
            (ScopeKind::Workspace, None) => Ok(Self::workspace()),
            (ScopeKind::ExactFiles, Some(paths)) => {
                Self::exact_files(paths.0).map_err(serde::de::Error::custom)
            }
            _ => Err(serde::de::Error::custom("invalid file write scope shape")),
        }
    }
}
