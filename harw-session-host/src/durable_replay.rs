//! Durable [`TranscriptSource`] adapter entry point (S04).
//!
//! `StoreTranscripts` in `replay.rs` already reads the JSONL store; S04 owns
//! the durable-composition entry point used by the daemon: open by store
//! root, repair-aware, head-consistent with the approval backend.
//!
//! Skeleton: every call answers [`HostError::NotImplemented`].

use std::path::{Path, PathBuf};

use harw_session_store::TranscriptRecord;
use harw_types::SessionId;

use crate::error::HostError;
use crate::replay::TranscriptSource;

const WHAT: &str = "DurableTranscripts (S04)";

/// [`TranscriptSource`] over the durable transcript store.
#[derive(Debug)]
pub struct DurableTranscripts {
    _root: PathBuf,
}

impl DurableTranscripts {
    /// Source rooted at the session store directory `root`.
    ///
    /// # Errors
    /// [`HostError::NotImplemented`] in the skeleton; storage errors later.
    pub fn open(root: &Path) -> Result<Self, HostError> {
        let _ = root;
        Err(HostError::NotImplemented(WHAT))
    }
}

impl TranscriptSource for DurableTranscripts {
    fn read_from(
        &self,
        session: &SessionId,
        from_sequence: u64,
        limit: usize,
    ) -> Result<Vec<TranscriptRecord>, HostError> {
        let _ = (session, from_sequence, limit);
        Err(HostError::NotImplemented(WHAT))
    }

    fn head(&self, session: &SessionId) -> Result<u64, HostError> {
        let _ = session;
        Err(HostError::NotImplemented(WHAT))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_is_typed_not_implemented() {
        let result = DurableTranscripts::open(Path::new("."));
        assert!(matches!(result, Err(HostError::NotImplemented(_))));
    }
}
