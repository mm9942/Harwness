//! Disposable, persistent session/project working-set types.

/// Scope of a workbench directory.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WorkbenchScope {
    Session(String),
    Project(String),
}

impl WorkbenchScope {
    #[must_use]
    pub fn path_component(&self) -> String {
        match self {
            Self::Session(id) => format!("session:{id}"),
            Self::Project(slug) => format!("project:{slug}"),
        }
    }
}

/// An absolute file path pinned into the working set with a human explanation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedFile {
    pub absolute_path: String,
    pub note: String,
}

/// A lightweight hypothesis whose lifecycle must not pollute durable memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hypothesis {
    pub text: String,
    pub status: HypothesisStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HypothesisStatus {
    Testing,
    Confirmed,
    Rejected,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_are_namespaced_in_the_filesystem() {
        assert_eq!(
            WorkbenchScope::Project("harwness".to_owned()).path_component(),
            "project:harwness"
        );
    }
}
