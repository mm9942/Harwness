//! `StateStore` — die Persistenz-Naht des Core.
//!
//! Analog zu codex' `StateDbHandle`: der Core kennt **nie** eine konkrete
//! Datenbank, sondern nur diesen Trait. v1 liefert eine in-memory-Impl;
//! SQLite/S3 kommen später als weitere Impls hinter derselben Schnittstelle.
//!
//! Der Trait existiert ab Tag 1 — sonst sickert die Persistenz-Annahme in den
//! Core und bläht die Gravity Well auf.

use crate::history::ConversationHistory;
use harw_extension_api::ExtFuture;
use harw_protocol::items::TurnItem;
use harw_session_store::{RecordKind, TranscriptStore};
use harw_types::{SessionId, ThreadRef};
use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, MutexGuard};

/// Errors returned by a [`StateStore`] operation.
///
/// The trait deliberately owns this boundary instead of exposing a concrete
/// backend error.  In-memory storage currently only produces
/// [`StateStoreError::PoisonedMutex`]; durable implementations can preserve
/// their typed backend error through [`StateStoreError::SessionStore`].
#[derive(Debug)]
pub enum StateStoreError {
    /// The store's mutex was poisoned by a panic while it was locked.
    PoisonedMutex { operation: &'static str },

    /// A `harw-session-store` backend could not complete an operation.
    SessionStore(harw_session_store::SessionStoreError),

    /// The persisted transcript has exhausted the sequence-number space.
    SequenceExhausted { session: SessionId },
}

impl fmt::Display for StateStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PoisonedMutex { operation } => {
                write!(formatter, "state store mutex poisoned during {operation}")
            }
            Self::SessionStore(error) => write!(formatter, "session store error: {error}"),
            Self::SequenceExhausted { session } => {
                write!(
                    formatter,
                    "transcript sequence exhausted for session {}",
                    session.as_str()
                )
            }
        }
    }
}

impl std::error::Error for StateStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PoisonedMutex { .. } => None,
            Self::SessionStore(error) => Some(error),
            Self::SequenceExhausted { .. } => None,
        }
    }
}

impl From<harw_session_store::SessionStoreError> for StateStoreError {
    fn from(error: harw_session_store::SessionStoreError) -> Self {
        Self::SessionStore(error)
    }
}

/// Result type used by the state-store boundary.
pub type StateStoreResult<T> = Result<T, StateStoreError>;

fn lock_state<'a, T>(
    mutex: &'a Mutex<T>,
    operation: &'static str,
) -> StateStoreResult<MutexGuard<'a, T>> {
    mutex
        .lock()
        .map_err(|_| StateStoreError::PoisonedMutex { operation })
}

/// Persistenz-Abstraktion für Session-Verläufe.
///
/// Bewusst minimal: ein Turn-Item speichern, einen Verlauf laden. Alles
/// Weitere (Cursor, Snapshots, Forks) sind additive Methoden für später.
///
pub trait StateStore: Send + Sync {
    /// Persistiert ein einzelnes `TurnItem` für die gegebene Session.
    fn save_turn<'a>(
        &'a self,
        sid: &'a SessionId,
        item: &'a TurnItem,
    ) -> ExtFuture<'a, StateStoreResult<()>>;

    /// Lädt den vollständigen Verlauf einer Session.
    fn load_history<'a>(
        &'a self,
        sid: &'a SessionId,
    ) -> ExtFuture<'a, StateStoreResult<ConversationHistory>>;

    /// Persistiert einen kompletten Verlauf. Default: ersetzt den gespeicherten
    /// Verlauf, indem jedes Item einzeln über `save_turn` geschrieben wird.
    ///
    /// Backends mit Bulk-Write (SQLite-Transaktion, S3-Put) überschreiben das
    /// für Effizienz; die in-memory-Impl tut es bewusst, um Duplikate zu
    /// vermeiden.
    fn save_history<'a>(
        &'a self,
        sid: &'a SessionId,
        history: &'a ConversationHistory,
    ) -> ExtFuture<'a, StateStoreResult<()>> {
        Box::pin(async move {
            for item in history.items() {
                self.save_turn(sid, item).await?;
            }
            Ok(())
        })
    }
}

/// Deterministically derives the transcript thread that owns a core session.
///
/// The mapper is a function pointer rather than a stateful closure: a durable
/// transcript must resolve the same [`SessionId`] to the same [`ThreadRef`]
/// after a process restart.
pub type SessionThreadMapper = fn(&SessionId) -> ThreadRef;

/// Durable [`StateStore`] backed by a [`TranscriptStore`].
///
/// A caller supplies both the transcript store and the deterministic
/// session-to-thread mapper. Sequence state is recovered from the persisted
/// transcript on first write for each session, then guarded together with the
/// durable append so concurrent saves through this adapter cannot reuse a
/// sequence number.
pub struct TranscriptStateStore {
    transcript_store: TranscriptStore,
    thread_for_session: SessionThreadMapper,
    next_sequences: Mutex<HashMap<String, u64>>,
}

impl TranscriptStateStore {
    /// Creates a durable adapter.
    ///
    /// `transcript_store` supplies the durable JSONL backend. `thread_for_session`
    /// must be deterministic across restarts for every session it may receive.
    #[must_use]
    pub fn new(transcript_store: TranscriptStore, thread_for_session: SessionThreadMapper) -> Self {
        Self {
            transcript_store,
            thread_for_session,
            next_sequences: Mutex::new(HashMap::new()),
        }
    }

    fn recovered_next_sequence(&self, sid: &SessionId) -> StateStoreResult<u64> {
        match self.transcript_store.reader(sid) {
            Ok(reader) => {
                let mut next_sequence = 0_u64;
                for record in reader {
                    let record = record?;
                    let candidate = record.sequence.checked_add(1).ok_or_else(|| {
                        StateStoreError::SequenceExhausted {
                            session: sid.clone(),
                        }
                    })?;
                    next_sequence = next_sequence.max(candidate);
                }
                Ok(next_sequence)
            }
            Err(harw_session_store::SessionStoreError::NotFound { .. }) => Ok(0),
            Err(error) => Err(error.into()),
        }
    }

    fn append_turn(&self, sid: &SessionId, item: &TurnItem) -> StateStoreResult<()> {
        let mut next_sequences = lock_state(&self.next_sequences, "transcript_save_turn")?;
        let sequence = match next_sequences.get(sid.as_str()) {
            Some(sequence) => *sequence,
            None => self.recovered_next_sequence(sid)?,
        };
        let record = harw_session_store::TranscriptRecord::new(
            sid.clone(),
            (self.thread_for_session)(sid),
            sequence,
            jiff::Timestamp::now(),
            RecordKind::Item,
            serde_json::to_value(item).map_err(harw_session_store::SessionStoreError::from)?,
        );
        self.transcript_store.append(&record)?;
        next_sequences.insert(sid.as_str().to_owned(), sequence.saturating_add(1));
        Ok(())
    }
}

impl StateStore for TranscriptStateStore {
    fn save_turn<'a>(
        &'a self,
        sid: &'a SessionId,
        item: &'a TurnItem,
    ) -> ExtFuture<'a, StateStoreResult<()>> {
        Box::pin(async move { self.append_turn(sid, item) })
    }

    fn load_history<'a>(
        &'a self,
        sid: &'a SessionId,
    ) -> ExtFuture<'a, StateStoreResult<ConversationHistory>> {
        Box::pin(async move {
            let thread = (self.thread_for_session)(sid);
            let reader = match self.transcript_store.reader(sid) {
                Ok(reader) => reader,
                Err(harw_session_store::SessionStoreError::NotFound { .. }) => {
                    return Ok(ConversationHistory::new());
                }
                Err(error) => return Err(error.into()),
            };
            let mut items = Vec::new();
            for record in reader {
                let record = record?;
                if record.kind == RecordKind::Item && record.thread == thread {
                    items.push(
                        serde_json::from_value(record.payload)
                            .map_err(harw_session_store::SessionStoreError::from)?,
                    );
                }
            }
            Ok(ConversationHistory::from_items(items))
        })
    }
}

/// Default-Impl: hält alle Verläufe in einer `Mutex`-geschützten Map.
///
/// `Send + Sync` über `std::sync::Mutex` — kein `tokio::Mutex` nötig, da die
/// kritischen Abschnitte synchron und kurz sind (keine `.await` im Lock).
#[derive(Debug, Default)]
pub struct InMemoryStateStore {
    inner: Mutex<HashMap<String, Vec<TurnItem>>>,
}

impl InMemoryStateStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Anzahl der Sessions mit gespeichertem Verlauf.
    #[must_use]
    pub fn session_count(&self) -> usize {
        self.try_session_count()
            .unwrap_or_else(|error| panic!("unable to count sessions: {error}"))
    }

    /// Fallible variant of [`Self::session_count`].
    pub fn try_session_count(&self) -> StateStoreResult<usize> {
        lock_state(&self.inner, "session_count").map(|map| map.len())
    }

    /// Anzahl gespeicherter Items für eine Session (Test-/Diagnose-Hilfe).
    #[must_use]
    pub fn turn_count(&self, sid: &SessionId) -> usize {
        self.try_turn_count(sid)
            .unwrap_or_else(|error| panic!("unable to count turns: {error}"))
    }

    /// Fallible variant of [`Self::turn_count`].
    pub fn try_turn_count(&self, sid: &SessionId) -> StateStoreResult<usize> {
        lock_state(&self.inner, "turn_count").map(|map| map.get(sid.as_str()).map_or(0, Vec::len))
    }
}

impl StateStore for InMemoryStateStore {
    fn save_turn<'a>(
        &'a self,
        sid: &'a SessionId,
        item: &'a TurnItem,
    ) -> ExtFuture<'a, StateStoreResult<()>> {
        Box::pin(async move {
            let mut map = lock_state(&self.inner, "save_turn")?;
            map.entry(sid.as_str().to_owned())
                .or_default()
                .push(item.clone());
            Ok(())
        })
    }

    fn save_history<'a>(
        &'a self,
        sid: &'a SessionId,
        history: &'a ConversationHistory,
    ) -> ExtFuture<'a, StateStoreResult<()>> {
        Box::pin(async move {
            let mut map = lock_state(&self.inner, "save_history")?;
            map.insert(sid.as_str().to_owned(), history.items().to_vec());
            Ok(())
        })
    }

    fn load_history<'a>(
        &'a self,
        sid: &'a SessionId,
    ) -> ExtFuture<'a, StateStoreResult<ConversationHistory>> {
        Box::pin(async move {
            let items = lock_state(&self.inner, "load_history")?
                .get(sid.as_str())
                .cloned()
                .unwrap_or_default();
            Ok(ConversationHistory::from_items(items))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> SessionId {
        SessionId::from_str("state-store-test")
    }

    fn transcript_thread(sid: &SessionId) -> ThreadRef {
        ThreadRef::from_str(format!("core-session:{}", sid.as_str()))
    }

    fn error_item(message: &str) -> TurnItem {
        TurnItem::Error(harw_protocol::items::ErrorItem {
            id: harw_types::ItemId::new(),
            message: message.to_owned(),
            retryable: false,
        })
    }

    #[tokio::test]
    async fn in_memory_store_round_trips_history() {
        let store = InMemoryStateStore::new();
        let mut history = ConversationHistory::new();
        history.push_user_text("hello");

        store.save_history(&session(), &history).await.unwrap();

        assert_eq!(store.session_count(), 1);
        assert_eq!(store.turn_count(&session()), 1);
        let loaded = store.load_history(&session()).await.unwrap();
        assert_eq!(
            serde_json::to_value(loaded.items()).unwrap(),
            serde_json::to_value(history.items()).unwrap()
        );
    }

    #[tokio::test]
    async fn transcript_store_round_trips_items_with_deterministic_thread_mapping() {
        let temp = tempfile::tempdir().unwrap();
        let store = TranscriptStateStore::new(TranscriptStore::new(temp.path()), transcript_thread);
        let item = error_item("durable failure");

        store.save_turn(&session(), &item).await.unwrap();

        let loaded = store.load_history(&session()).await.unwrap();
        assert_eq!(
            serde_json::to_value(loaded.items()).unwrap(),
            serde_json::json!([item])
        );
        let records = TranscriptStore::new(temp.path())
            .reader(&session())
            .unwrap()
            .collect::<harw_session_store::SessionStoreResult<Vec<_>>>()
            .unwrap();
        assert_eq!(records[0].thread, transcript_thread(&session()));
        assert_eq!(records[0].sequence, 0);
    }

    #[tokio::test]
    async fn transcript_store_recovers_sequence_and_serializes_concurrent_saves() {
        let temp = tempfile::tempdir().unwrap();
        let initial =
            TranscriptStateStore::new(TranscriptStore::new(temp.path()), transcript_thread);
        initial
            .save_turn(&session(), &error_item("before restart"))
            .await
            .unwrap();

        let recovered =
            TranscriptStateStore::new(TranscriptStore::new(temp.path()), transcript_thread);
        let first = error_item("first concurrent save");
        let second = error_item("second concurrent save");
        let session_id = session();
        let (first_result, second_result) = tokio::join!(
            recovered.save_turn(&session_id, &first),
            recovered.save_turn(&session_id, &second),
        );
        first_result.unwrap();
        second_result.unwrap();

        let sequences = TranscriptStore::new(temp.path())
            .reader(&session())
            .unwrap()
            .collect::<harw_session_store::SessionStoreResult<Vec<_>>>()
            .unwrap()
            .into_iter()
            .map(|record| record.sequence)
            .collect::<Vec<_>>();
        assert_eq!(sequences, vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn transcript_store_rejects_sequence_exhaustion_during_recovery() {
        let temp = tempfile::tempdir().unwrap();
        let sid = session();
        let record = harw_session_store::TranscriptRecord::new(
            sid.clone(),
            transcript_thread(&sid),
            u64::MAX,
            jiff::Timestamp::now(),
            RecordKind::Item,
            serde_json::to_value(error_item("sequence exhaustion fixture")).unwrap(),
        );
        TranscriptStore::new(temp.path()).append(&record).unwrap();

        let store = TranscriptStateStore::new(TranscriptStore::new(temp.path()), transcript_thread);
        let error = store
            .save_turn(&sid, &error_item("must not append"))
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            StateStoreError::SequenceExhausted { session } if session.as_str() == sid.as_str()
        ));
    }

    #[tokio::test]
    async fn poisoned_mutex_is_reported_by_store_operations() {
        let store = InMemoryStateStore::new();
        std::thread::scope(|scope| {
            let handle = scope.spawn(|| {
                let _guard = store.inner.lock().unwrap();
                panic!("poison test");
            });
            assert!(handle.join().is_err());
        });

        let error = store
            .save_turn(
                &session(),
                &TurnItem::Error(harw_protocol::items::ErrorItem {
                    id: harw_types::ItemId::new(),
                    message: "failure".to_owned(),
                    retryable: false,
                }),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            StateStoreError::PoisonedMutex {
                operation: "save_turn"
            }
        ));
        assert!(matches!(
            store.try_session_count().unwrap_err(),
            StateStoreError::PoisonedMutex {
                operation: "session_count"
            }
        ));
    }

    #[tokio::test]
    async fn poisoned_transcript_sequence_mutex_is_reported() {
        let temp = tempfile::tempdir().unwrap();
        let store = TranscriptStateStore::new(TranscriptStore::new(temp.path()), transcript_thread);
        std::thread::scope(|scope| {
            let handle = scope.spawn(|| {
                let _guard = store.next_sequences.lock().unwrap();
                panic!("poison test");
            });
            assert!(handle.join().is_err());
        });

        let error = store
            .save_turn(&session(), &error_item("poisoned durable state"))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            StateStoreError::PoisonedMutex {
                operation: "transcript_save_turn"
            }
        ));
    }

    #[test]
    fn backend_failure_has_a_typed_error_boundary() {
        let error = StateStoreError::from(harw_session_store::SessionStoreError::NotFound {
            session: session(),
        });
        assert!(error.to_string().contains("no transcript found"));
        assert!(std::error::Error::source(&error).is_some());
    }
}
