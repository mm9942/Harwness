//! Machine-local host alias store (W00 §8, S07).
//!
//! State path: `$HARW_STATE_DIR/hosts/<alias>.json`. Aliases are local
//! operator state; an endpoint is a locator only and the record pins the
//! expected authenticated host identity. Changing trust is an explicit,
//! trusted action; nothing auto-creates an alias.
//!
//! Skeleton: signatures are frozen, bodies answer
//! [`RemoteError::NotImplemented`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::RemoteError;

/// One stored host alias.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostAlias {
    /// Operator-chosen alias name.
    pub alias: String,
    /// Locator: `unix:<path>` or `node:<host:port>`.
    pub endpoint: String,
    /// Node id the host must prove (node endpoints only).
    pub expected_node: Option<String>,
}

/// Reads and writes alias records under a state directory.
#[derive(Clone, Debug)]
pub struct AliasStore {
    _dir: PathBuf,
}

impl AliasStore {
    /// Store rooted at `state_dir` (records live in `<state_dir>/hosts`).
    #[must_use]
    pub fn new(state_dir: &Path) -> Self {
        Self {
            _dir: state_dir.join("hosts"),
        }
    }

    /// Load one alias.
    ///
    /// # Errors
    /// [`RemoteError::NotImplemented`] in the skeleton.
    pub fn get(&self, alias: &str) -> Result<Option<HostAlias>, RemoteError> {
        let _ = alias;
        Err(RemoteError::NotImplemented("AliasStore::get (S07)"))
    }

    /// Create or replace one alias (explicit trusted action).
    ///
    /// # Errors
    /// [`RemoteError::NotImplemented`] in the skeleton.
    pub fn put(&self, record: &HostAlias) -> Result<(), RemoteError> {
        let _ = record;
        Err(RemoteError::NotImplemented("AliasStore::put (S07)"))
    }

    /// All stored aliases.
    ///
    /// # Errors
    /// [`RemoteError::NotImplemented`] in the skeleton.
    pub fn list(&self) -> Result<Vec<HostAlias>, RemoteError> {
        Err(RemoteError::NotImplemented("AliasStore::list (S07)"))
    }

    /// Remove one alias; `Ok(false)` when it did not exist.
    ///
    /// # Errors
    /// [`RemoteError::NotImplemented`] in the skeleton.
    pub fn remove(&self, alias: &str) -> Result<bool, RemoteError> {
        let _ = alias;
        Err(RemoteError::NotImplemented("AliasStore::remove (S07)"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_is_typed_not_implemented() {
        let store = AliasStore::new(Path::new("."));
        assert!(matches!(store.list(), Err(RemoteError::NotImplemented(_))));
    }
}
