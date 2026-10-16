//! Durable [`TranscriptSource`] over the session store (S04).
//!
//! Thin composition over [`StoreTranscripts`] (which already reads the JSONL
//! store, treats a missing transcript as empty and fails on a corrupt or torn
//! record) plus the cursor rules of PL-65 §3.4:
//!
//! - a cursor is `(generation, durable, live)`; `durable` is the transcript
//!   sequence of the next record the client has not seen;
//! - the transcript is truth: `durable` must not point past the transcript
//!   head, and a cursor from another generation is stale;
//! - both are *rejected* here ([`HostError::Protocol`]); the caller answers
//!   with a resync and the client never fabricates a position;
//! - `live` indexes the volatile live ring, not the transcript, so it is
//!   not interpreted here.
//!
//! Reading never mutates the store. A torn tail (crash mid-append) makes
//! reads fail closed with [`HostError::Storage`] until the writer-side
//! [`DurableTranscripts::repair_tail`] has quarantined it.

use std::path::Path;

use harw_protocol::{Cursor, FrameEnvelope};
use harw_session_store::store::TailRepair;
use harw_session_store::{SessionStoreError, TranscriptRecord, TranscriptStore};
use harw_types::SessionId;

use crate::error::HostError;
use crate::replay::{StoreTranscripts, TranscriptSource, replay};

/// [`TranscriptSource`] over the durable transcript store.
pub struct DurableTranscripts {
    inner: StoreTranscripts,
    writer: TranscriptStore,
}

impl std::fmt::Debug for DurableTranscripts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DurableTranscripts")
            .field("root", &self.writer.root())
            .finish()
    }
}

impl DurableTranscripts {
    /// Source rooted at the transcript directory `root` (`<root>/<id>.jsonl`).
    ///
    /// The directory is not created or touched; an absent one is an empty
    /// source.
    ///
    /// # Errors
    /// Never fails today; the `Result` keeps room for storage validation.
    pub fn open(root: &Path) -> Result<Self, HostError> {
        Ok(Self {
            inner: StoreTranscripts::new(TranscriptStore::new(root)),
            writer: TranscriptStore::new(root),
        })
    }

    /// Validate a client cursor against the transcript of `session`.
    ///
    /// # Errors
    /// [`HostError::Protocol`] when `cursor.generation` is not `generation`
    /// or `cursor.durable` lies beyond the transcript head;
    /// [`HostError::Storage`] when the transcript cannot be read.
    pub fn check_cursor(
        &self,
        session: &SessionId,
        generation: u32,
        cursor: &Cursor,
    ) -> Result<(), HostError> {
        if cursor.generation != generation {
            return Err(HostError::Protocol(
                "cursor from another transcript generation".into(),
            ));
        }
        let head = self.head(session)?;
        if cursor.durable > head {
            return Err(HostError::Protocol(
                "cursor is beyond the transcript head".into(),
            ));
        }
        Ok(())
    }

    /// Cursor of the transcript head: the position of the next record.
    pub fn head_cursor(&self, session: &SessionId, generation: u32) -> Result<Cursor, HostError> {
        Ok(Cursor {
            generation,
            durable: self.head(session)?,
            live: 0,
        })
    }

    /// Durable frames after `from`, at most `limit`, after validating the
    /// cursor (see [`Self::check_cursor`]). Each frame's cursor is the
    /// "next unseen" position, so the last one is the next `from`.
    pub fn replay_from(
        &self,
        session: &SessionId,
        generation: u32,
        from: &Cursor,
        limit: usize,
    ) -> Result<Vec<FrameEnvelope>, HostError> {
        self.check_cursor(session, generation, from)?;
        replay(self, session, generation, from.durable, limit)
    }

    /// Writer-side recovery: quarantine and cut a torn last line, after which
    /// reads succeed again. A missing transcript is clean.
    pub fn repair_tail(&self, session: &SessionId) -> Result<TailRepair, HostError> {
        match self.writer.repair_tail(session) {
            Ok(outcome) => Ok(outcome),
            Err(SessionStoreError::NotFound { .. }) => Ok(TailRepair::Clean),
            Err(error) => Err(error.into()),
        }
    }
}

impl TranscriptSource for DurableTranscripts {
    fn read_from(
        &self,
        session: &SessionId,
        from_sequence: u64,
        limit: usize,
    ) -> Result<Vec<TranscriptRecord>, HostError> {
        self.inner.read_from(session, from_sequence, limit)
    }

    fn head(&self, session: &SessionId) -> Result<u64, HostError> {
        self.inner.head(session)
    }
}
