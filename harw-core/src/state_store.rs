//! `StateStore` — die Persistenz-Naht des Core.
//!
//! Analog zu codex' `StateDbHandle`: der Core kennt **nie** eine konkrete
//! Datenbank, sondern nur diesen Trait. v1 liefert eine in-memory-Impl und
//! einen dauerhaften Adapter über [`TranscriptStore`]; SQLite/S3 kommen später
//! als weitere Impls hinter derselben Schnittstelle.
//!
//! # Verantwortung (W4a/A-SESS)
//! - **Verlauf:** `save_turn`/`save_history`/`load_history`. Beide
//!   mitgelieferten Impls reparieren beim Laden offene Tool-Calls ohne
//!   Ergebnis ([`repair_open_tool_calls`], F-150): ein Absturz zwischen dem
//!   Persistieren eines `ToolCall` und seinem `ToolResult` macht eine Session
//!   sonst bei jedem Resume unbrauchbar, weil Provider einen solchen Verlauf
//!   ablehnen.
//! - **Sitzungszustand:** `save_session_state`/`load_session_state` tragen
//!   Modus, Token-Nutzung und Aktivierung ([`SessionStateSnapshot`], F-157).
//!   Beide Trait-Methoden haben Defaults (kein Zustand), damit bestehende
//!   Test-Impls weiter kompilieren.
//! - **Sequenzvergabe:** [`TranscriptStateStore`] hält je Session ein eigenes
//!   Schloss statt einer globalen Sperre über den fsync (F-159) und führt die
//!   blockierende Datei-I/O über `tokio::task::spawn_blocking` aus, sofern eine
//!   Tokio-Runtime läuft.
//!
//! # Nebenläufigkeit
//! Alle Typen sind `Send + Sync`. Parallele Schreibzugriffe auf **dieselbe**
//! Session serialisieren sich über deren Sequenz-Schloss (keine doppelt
//! vergebene Sequenznummer, kein Verschränken paralleler Turns innerhalb eines
//! Datensatzes); verschiedene Sessions schreiben parallel.
//!
//! # Fehler
//! [`StateStoreError`] — die Variantenmenge ist bewusst unverändert, weil
//! `turn_loop::state_store_error` sie erschöpfend abbildet.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_core::state_store::{InMemoryStateStore, StateStore};
//! use harw_types::SessionId;
//! # async fn demo() {
//! let store = InMemoryStateStore::new();
//! let history = store.load_history(&SessionId::new()).await;
//! # let _ = history;
//! # }
//! ```

use crate::activation::{SessionActivation, ToolProfile};
use crate::history::ConversationHistory;
use crate::mode::InteractionMode;
use harw_extension_api::ExtFuture;
use harw_protocol::items::{ResultTrust, ToolCallResult, ToolResultItem, TurnItem};
use harw_session_store::{RecordKind, TranscriptStore};
use harw_tools::ToolName;
use harw_types::{ItemId, SessionId, ThreadRef, TokenUsage, ToolCallId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};

/// Errors returned by a [`StateStore`] operation.
///
/// The trait deliberately owns this boundary instead of exposing a concrete
/// backend error.  In-memory storage currently only produces
/// [`StateStoreError::PoisonedMutex`]; durable implementations can preserve
/// their typed backend error through [`StateStoreError::SessionStore`].
#[derive(Debug)]
pub enum StateStoreError {
    /// The store's mutex was poisoned by a panic while it was locked.
    ///
    /// Also reported when the blocking I/O task of [`TranscriptStateStore`]
    /// panicked or was cancelled by a runtime shutdown: in both cases the
    /// per-session state may be inconsistent, exactly as with a poisoned lock.
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

// ---------------------------------------------------------------------------
// Sitzungszustand (F-157)
// ---------------------------------------------------------------------------

/// Aktuelle Version des persistierten [`SessionStateSnapshot`].
pub const SESSION_STATE_VERSION: u32 = 1;

// Diskriminator im Payload eines `RecordKind::Lifecycle`-Datensatzes; andere
// Lifecycle-Schreiber (Gateway, Pairing) nutzen `{"event": …}` und werden
// dadurch nie als Sitzungszustand gelesen.
const SESSION_STATE_RECORD_KIND: &str = "core_session_state";

/// Persistierbare Form eines [`ToolProfile`].
///
/// # Description
/// `ToolProfile` selbst trägt kein `serde` (liegt außerhalb dieses Moduls);
/// diese Spiegelung hält die Wire-Form stabil (`snake_case`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PersistedToolProfile {
    /// Entspricht [`ToolProfile::Minimal`].
    Minimal,
    /// Entspricht [`ToolProfile::Coding`].
    Coding,
    /// Entspricht [`ToolProfile::Full`].
    Full,
}

impl From<ToolProfile> for PersistedToolProfile {
    fn from(profile: ToolProfile) -> Self {
        match profile {
            ToolProfile::Minimal => Self::Minimal,
            ToolProfile::Coding => Self::Coding,
            ToolProfile::Full => Self::Full,
        }
    }
}

impl From<PersistedToolProfile> for ToolProfile {
    fn from(profile: PersistedToolProfile) -> Self {
        match profile {
            PersistedToolProfile::Minimal => Self::Minimal,
            PersistedToolProfile::Coding => Self::Coding,
            PersistedToolProfile::Full => Self::Full,
        }
    }
}

/// Persistierbare Sicht auf eine [`SessionActivation`], abgetastet über eine
/// endliche Menge von Werkzeugnamen.
///
/// # Description
/// `SessionActivation` legt ihre Override-Mengen nicht offen. Der Snapshot
/// hält deshalb das Profil plus, für jeden abgetasteten Namen (in der Praxis:
/// jedes im Registry registrierte Werkzeug), ob er sichtbar war. Für jeden
/// abgetasteten Namen liefert [`Self::to_activation`] exakt dieselbe
/// Sichtbarkeit wie das Original; für nicht abgetastete Namen gilt das Profil.
///
/// **Nicht erfasst:** deaktivierte Instructions-/Context-Labels (nicht
/// aufzählbar über die öffentliche API; heute ohne Produktionsaufrufer).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActivationSnapshot {
    /// Grobprofil der Aktivierung.
    pub profile: PersistedToolProfile,
    /// Abgetastete Namen, die sichtbar waren.
    pub enabled_tools: BTreeSet<String>,
    /// Abgetastete Namen, die verborgen waren.
    pub disabled_tools: BTreeSet<String>,
}

impl ActivationSnapshot {
    /// Tastet `activation` über `tool_names` ab.
    ///
    /// # Arguments
    /// - `activation` (`&SessionActivation`): die abzubildende Aktivierung.
    /// - `tool_names` (`I: IntoIterator<Item = &str>`): die abzutastenden Namen.
    ///
    /// # Returns
    /// Den Snapshot; jeder Name landet in genau einer der beiden Mengen.
    ///
    /// # Concurrency
    /// Rein, allokiert nur den Snapshot.
    #[must_use]
    pub fn capture<'n, I>(activation: &SessionActivation, tool_names: I) -> Self
    where
        I: IntoIterator<Item = &'n str>,
    {
        let mut enabled_tools = BTreeSet::new();
        let mut disabled_tools = BTreeSet::new();
        for name in tool_names {
            if activation.is_tool_enabled(&ToolName::new(name)) {
                enabled_tools.insert(name.to_owned());
            } else {
                disabled_tools.insert(name.to_owned());
            }
        }
        Self {
            profile: activation.profile().into(),
            enabled_tools,
            disabled_tools,
        }
    }

    /// Baut die abgebildete [`SessionActivation`] wieder auf.
    ///
    /// # Description
    /// Profil plus explizite `enable_tool`/`disable_tool`-Overrides je
    /// abgetastetem Namen. Das Ergebnis ist **keine** Autorität für sich: ein
    /// Aufrufer schneidet es immer mit der aktuellen Basis
    /// (siehe `AgentSession::restore_state`).
    ///
    /// # Returns
    /// Eine frische `SessionActivation`.
    #[must_use]
    pub fn to_activation(&self) -> SessionActivation {
        let mut activation = SessionActivation::new(self.profile.into());
        for name in &self.enabled_tools {
            activation.enable_tool(ToolName::new(name.as_str()));
        }
        for name in &self.disabled_tools {
            activation.disable_tool(ToolName::new(name.as_str()));
        }
        activation
    }
}

/// Der über den Verlauf hinaus persistierte Zustand einer Session (F-157).
///
/// # Description
/// Wird von `AgentSession::state_snapshot` erzeugt und von
/// `AgentSession::restore_state` angewandt. Die Basis-Aktivierung wird beim
/// Laden **nie erweitert**: geladene Werte werden ausschließlich mit der
/// aktuellen Basis geschnitten.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionStateSnapshot {
    /// Formatversion, aktuell [`SESSION_STATE_VERSION`].
    pub version: u32,
    /// Interaktionsmodus zum Zeitpunkt des Snapshots.
    pub mode: InteractionMode,
    /// Aufsummierte Token-Nutzung der Session.
    pub total_usage: TokenUsage,
    /// Digest der Agent-IR, unter der die Basis entstand (`None`: keine IR).
    pub executable_snapshot_id: Option<String>,
    /// Basis-Aktivierung (vor dem Modus-Schnitt).
    pub base_activation: ActivationSnapshot,
    /// Wirksame Aktivierung inklusive Laufzeit-Overrides.
    pub activation: ActivationSnapshot,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionStateRecord {
    kind: String,
    state: SessionStateSnapshot,
}

fn session_state_payload(state: &SessionStateSnapshot) -> StateStoreResult<serde_json::Value> {
    serde_json::to_value(SessionStateRecord {
        kind: SESSION_STATE_RECORD_KIND.to_owned(),
        state: state.clone(),
    })
    .map_err(|error| harw_session_store::SessionStoreError::from(error).into())
}

fn is_session_state_payload(payload: &serde_json::Value) -> bool {
    payload
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|kind| kind == SESSION_STATE_RECORD_KIND)
}

fn decode_session_state(payload: serde_json::Value) -> StateStoreResult<SessionStateSnapshot> {
    serde_json::from_value::<SessionStateRecord>(payload)
        .map(|record| record.state)
        .map_err(|error| harw_session_store::SessionStoreError::from(error).into())
}

// ---------------------------------------------------------------------------
// Reparatur offener Tool-Calls (F-150)
// ---------------------------------------------------------------------------

/// Fehlertext des synthetischen Ergebnisses für einen unterbrochenen Tool-Call.
pub const INTERRUPTED_TOOL_CALL_MESSAGE: &str = "tool call was interrupted before its result \
was recorded; the harness did not observe whether it ran — verify the state before retrying";

fn synthetic_interrupted_result(call_id: ToolCallId) -> TurnItem {
    TurnItem::ToolResult(ToolResultItem {
        id: ItemId::from_str(format!("synthetic-result-{}", call_id.as_str())),
        call_id,
        result: ToolCallResult::error(INTERRUPTED_TOOL_CALL_MESSAGE),
        duration_ms: 0,
        trust: ResultTrust::Runtime,
    })
}

// Schließt die laufende Tool-Runde: je offenem Call ein synthetisches Ergebnis.
fn flush_synthetic_results(
    pending: &mut Vec<ToolCallId>,
    items: &mut Vec<TurnItem>,
    repaired: &mut Vec<ToolCallId>,
) {
    for call_id in pending.drain(..) {
        repaired.push(call_id.clone());
        items.push(synthetic_interrupted_result(call_id));
    }
}

/// Ergänzt jeden Tool-Call ohne Ergebnis um ein synthetisches Fehler-Ergebnis.
///
/// # Description
/// Ein Call gilt als offen, wenn **nirgends** im Verlauf ein `ToolResult` mit
/// seiner `call_id` steht (ein spätes, echtes Ergebnis wird also nie
/// dupliziert). Das synthetische Ergebnis (`ToolCallResult::Error`,
/// `ResultTrust::Runtime`, deterministische `ItemId`
/// `synthetic-result-<call_id>`) wird am Ende der Tool-Runde des Calls
/// eingefügt — unmittelbar vor dem ersten folgenden Item, das weder
/// `ToolCall` noch `ToolResult` ist, bzw. am Verlaufsende. Damit folgt es der
/// Gruppierung von `ConversationHistory::atomic_groups`.
///
/// Idempotent: ein zweiter Aufruf findet nichts mehr. Die Reparatur wird nicht
/// in das Append-only-Transkript zurückgeschrieben (die Position ließe sich
/// dort nicht herstellen); sie wird bei jedem Laden deterministisch wiederholt.
///
/// # Arguments
/// - `history` (`&mut ConversationHistory`): der zu reparierende Verlauf.
///
/// # Returns
/// Die `call_id`s der reparierten Calls in Verlaufsreihenfolge (leer, wenn
/// nichts offen war — dann wird nichts kopiert).
///
/// # Concurrency
/// Rein; verlangt exklusiven Zugriff auf den Verlauf.
pub fn repair_open_tool_calls(history: &mut ConversationHistory) -> Vec<ToolCallId> {
    let open: HashSet<ToolCallId> = {
        let answered: HashSet<&ToolCallId> = history
            .items()
            .iter()
            .filter_map(|item| match item {
                TurnItem::ToolResult(result) => Some(&result.call_id),
                _ => None,
            })
            .collect();
        history
            .items()
            .iter()
            .filter_map(|item| match item {
                TurnItem::ToolCall(call) if !answered.contains(&call.call_id) => {
                    Some(call.call_id.clone())
                }
                _ => None,
            })
            .collect()
    };
    if open.is_empty() {
        return Vec::new();
    }

    let mut repaired: Vec<ToolCallId> = Vec::new();
    let mut pending: Vec<ToolCallId> = Vec::new();
    let mut scheduled: HashSet<ToolCallId> = HashSet::new();
    let mut items = Vec::with_capacity(history.len().saturating_add(open.len()));
    for item in history.items() {
        match item {
            TurnItem::ToolCall(call) => {
                if open.contains(&call.call_id) && scheduled.insert(call.call_id.clone()) {
                    pending.push(call.call_id.clone());
                }
            }
            TurnItem::ToolResult(_) => {}
            _ => flush_synthetic_results(&mut pending, &mut items, &mut repaired),
        }
        items.push(item.clone());
    }
    flush_synthetic_results(&mut pending, &mut items, &mut repaired);

    tracing::warn!(
        repaired_tool_calls = repaired.len(),
        "history.repair_open_tool_calls"
    );
    *history = ConversationHistory::from_items(items);
    repaired
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Persistenz-Abstraktion für Session-Verläufe und Sitzungszustand.
///
/// Bewusst minimal: ein Turn-Item speichern, einen Verlauf laden, einen
/// Sitzungszustand speichern/laden. Alles Weitere (Cursor, Snapshots, Forks)
/// sind additive Methoden für später.
pub trait StateStore: Send + Sync {
    /// Persistiert ein einzelnes `TurnItem` für die gegebene Session.
    fn save_turn<'a>(
        &'a self,
        sid: &'a SessionId,
        item: &'a TurnItem,
    ) -> ExtFuture<'a, StateStoreResult<()>>;

    /// Lädt den vollständigen Verlauf einer Session.
    ///
    /// Impls sollen offene Tool-Calls über [`repair_open_tool_calls`]
    /// reparieren; die mitgelieferten Impls tun das.
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

    /// Persistiert den Sitzungszustand (Modus, Nutzung, Aktivierung).
    ///
    /// # Description
    /// Der zuletzt gespeicherte Zustand gewinnt beim Laden. Default: kein
    /// Zustandsspeicher — der Aufruf ist ein No-op, damit reine Verlaufs-Impls
    /// (Test-Doubles) unverändert gültig bleiben.
    ///
    /// # Errors
    /// Backend-spezifisch, siehe [`StateStoreError`].
    fn save_session_state<'a>(
        &'a self,
        _sid: &'a SessionId,
        _state: &'a SessionStateSnapshot,
    ) -> ExtFuture<'a, StateStoreResult<()>> {
        Box::pin(async { Ok(()) })
    }

    /// Lädt den zuletzt gespeicherten Sitzungszustand.
    ///
    /// # Returns
    /// `Ok(None)`, wenn nie ein Zustand gespeichert wurde (Default-Impl: immer).
    ///
    /// # Errors
    /// Backend-spezifisch; ein nicht dekodierbarer jüngster Zustand ist ein
    /// [`StateStoreError::SessionStore`].
    fn load_session_state<'a>(
        &'a self,
        _sid: &'a SessionId,
    ) -> ExtFuture<'a, StateStoreResult<Option<SessionStateSnapshot>>> {
        Box::pin(async { Ok(None) })
    }
}

// ---------------------------------------------------------------------------
// TranscriptStateStore
// ---------------------------------------------------------------------------

/// Deterministically derives the transcript thread that owns a core session.
///
/// The mapper is a function pointer rather than a stateful closure: a durable
/// transcript must resolve the same [`SessionId`] to the same [`ThreadRef`]
/// after a process restart.
pub type SessionThreadMapper = fn(&SessionId) -> ThreadRef;

// Sequenz-Schloss einer Session. `None`: noch nicht aus dem Transkript
// wiederhergestellt.
#[derive(Debug, Default)]
struct SequenceSlot {
    next: Option<u64>,
}

// Runs blocking file I/O off the async executor when a Tokio runtime is
// available (also valid on a `current_thread` runtime), inline otherwise.
async fn run_blocking<T, F>(operation: &'static str, work: F) -> StateStoreResult<T>
where
    F: FnOnce() -> StateStoreResult<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle
            .spawn_blocking(work)
            .await
            .map_err(|_| StateStoreError::PoisonedMutex { operation })?,
        Err(_) => work(),
    }
}

fn recovered_next_sequence(store: &TranscriptStore, sid: &SessionId) -> StateStoreResult<u64> {
    match store.reader(sid) {
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

/// Durable [`StateStore`] backed by a [`TranscriptStore`].
///
/// A caller supplies both the transcript store and the deterministic
/// session-to-thread mapper. Sequence state is recovered from the persisted
/// transcript on first write for each session, then guarded **per session**
/// together with the durable append: concurrent saves for one session can
/// never reuse a sequence number, while different sessions never wait on each
/// other's fsync (F-159). Items are stored as [`RecordKind::Item`], session
/// state as [`RecordKind::Lifecycle`] with a `core_session_state` payload.
pub struct TranscriptStateStore {
    transcript_store: Arc<TranscriptStore>,
    thread_for_session: SessionThreadMapper,
    sessions: Mutex<HashMap<String, Arc<Mutex<SequenceSlot>>>>,
}

impl TranscriptStateStore {
    /// Creates a durable adapter.
    ///
    /// `transcript_store` supplies the durable JSONL backend. `thread_for_session`
    /// must be deterministic across restarts for every session it may receive.
    #[must_use]
    pub fn new(transcript_store: TranscriptStore, thread_for_session: SessionThreadMapper) -> Self {
        Self {
            transcript_store: Arc::new(transcript_store),
            thread_for_session,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    // Holt (oder legt an) das Sequenz-Schloss einer Session. Die globale Map
    // ist nur für diesen Nachschlag gesperrt, nie über I/O.
    fn session_slot(&self, sid: &SessionId) -> StateStoreResult<Arc<Mutex<SequenceSlot>>> {
        let mut sessions = lock_state(&self.sessions, "transcript_save_turn")?;
        Ok(Arc::clone(
            sessions.entry(sid.as_str().to_owned()).or_default(),
        ))
    }

    async fn append_record(
        &self,
        sid: &SessionId,
        kind: RecordKind,
        payload: serde_json::Value,
    ) -> StateStoreResult<()> {
        let slot = self.session_slot(sid)?;
        let store = Arc::clone(&self.transcript_store);
        let thread = (self.thread_for_session)(sid);
        let sid = sid.clone();
        run_blocking("transcript_save_turn", move || {
            let mut slot = lock_state(slot.as_ref(), "transcript_save_turn")?;
            let sequence = match slot.next {
                Some(sequence) => sequence,
                None => recovered_next_sequence(&store, &sid)?,
            };
            let next = sequence
                .checked_add(1)
                .ok_or_else(|| StateStoreError::SequenceExhausted {
                    session: sid.clone(),
                })?;
            let record = harw_session_store::TranscriptRecord::new(
                sid,
                thread,
                sequence,
                jiff::Timestamp::now(),
                kind,
                payload,
            );
            store.append(&record)?;
            slot.next = Some(next);
            Ok(())
        })
        .await
    }
}

impl StateStore for TranscriptStateStore {
    fn save_turn<'a>(
        &'a self,
        sid: &'a SessionId,
        item: &'a TurnItem,
    ) -> ExtFuture<'a, StateStoreResult<()>> {
        Box::pin(async move {
            let payload = serde_json::to_value(item)
                .map_err(harw_session_store::SessionStoreError::from)?;
            self.append_record(sid, RecordKind::Item, payload).await
        })
    }

    fn load_history<'a>(
        &'a self,
        sid: &'a SessionId,
    ) -> ExtFuture<'a, StateStoreResult<ConversationHistory>> {
        Box::pin(async move {
            let store = Arc::clone(&self.transcript_store);
            let thread = (self.thread_for_session)(sid);
            let sid = sid.clone();
            let mut history = run_blocking("transcript_load_history", move || {
                let reader = match store.reader(&sid) {
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
            .await?;
            repair_open_tool_calls(&mut history);
            Ok(history)
        })
    }

    fn save_session_state<'a>(
        &'a self,
        sid: &'a SessionId,
        state: &'a SessionStateSnapshot,
    ) -> ExtFuture<'a, StateStoreResult<()>> {
        Box::pin(async move {
            let payload = session_state_payload(state)?;
            self.append_record(sid, RecordKind::Lifecycle, payload).await
        })
    }

    fn load_session_state<'a>(
        &'a self,
        sid: &'a SessionId,
    ) -> ExtFuture<'a, StateStoreResult<Option<SessionStateSnapshot>>> {
        Box::pin(async move {
            let store = Arc::clone(&self.transcript_store);
            let thread = (self.thread_for_session)(sid);
            let sid = sid.clone();
            run_blocking("transcript_load_session_state", move || {
                let reader = match store.reader(&sid) {
                    Ok(reader) => reader,
                    Err(harw_session_store::SessionStoreError::NotFound { .. }) => {
                        return Ok(None);
                    }
                    Err(error) => return Err(error.into()),
                };
                let mut latest: Option<serde_json::Value> = None;
                for record in reader {
                    let record = record?;
                    if record.kind == RecordKind::Lifecycle
                        && record.thread == thread
                        && is_session_state_payload(&record.payload)
                    {
                        latest = Some(record.payload);
                    }
                }
                latest.map(decode_session_state).transpose()
            })
            .await
        })
    }
}

// ---------------------------------------------------------------------------
// InMemoryStateStore
// ---------------------------------------------------------------------------

/// Default-Impl: hält alle Verläufe in einer `Mutex`-geschützten Map.
///
/// `Send + Sync` über `std::sync::Mutex` — kein `tokio::Mutex` nötig, da die
/// kritischen Abschnitte synchron und kurz sind (keine `.await` im Lock).
#[derive(Debug, Default)]
pub struct InMemoryStateStore {
    inner: Mutex<HashMap<String, Vec<TurnItem>>>,
    states: Mutex<HashMap<String, SessionStateSnapshot>>,
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
            let mut history = ConversationHistory::from_items(items);
            repair_open_tool_calls(&mut history);
            Ok(history)
        })
    }

    fn save_session_state<'a>(
        &'a self,
        sid: &'a SessionId,
        state: &'a SessionStateSnapshot,
    ) -> ExtFuture<'a, StateStoreResult<()>> {
        Box::pin(async move {
            lock_state(&self.states, "save_session_state")?
                .insert(sid.as_str().to_owned(), state.clone());
            Ok(())
        })
    }

    fn load_session_state<'a>(
        &'a self,
        sid: &'a SessionId,
    ) -> ExtFuture<'a, StateStoreResult<Option<SessionStateSnapshot>>> {
        Box::pin(async move {
            Ok(lock_state(&self.states, "load_session_state")?
                .get(sid.as_str())
                .cloned())
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
                let _guard = store.sessions.lock().unwrap();
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

    fn tool_call(call_id: &str) -> TurnItem {
        TurnItem::ToolCall(harw_protocol::items::ToolCallItem {
            id: ItemId::new(),
            call_id: ToolCallId::from_str(call_id),
            tool_name: "fs.read".to_owned(),
            arguments: serde_json::json!({}),
        })
    }

    fn tool_result(call_id: &str) -> TurnItem {
        TurnItem::ToolResult(ToolResultItem {
            id: ItemId::new(),
            call_id: ToolCallId::from_str(call_id),
            result: ToolCallResult::success(serde_json::json!("ok")),
            duration_ms: 1,
            trust: ResultTrust::Untrusted,
        })
    }

    fn user_item(text: &str) -> TurnItem {
        let mut history = ConversationHistory::new();
        history.push_user_text(text);
        history.items()[0].clone()
    }

    fn kinds(history: &ConversationHistory) -> Vec<String> {
        history
            .items()
            .iter()
            .map(|item| match item {
                TurnItem::ToolCall(call) => format!("call:{}", call.call_id.as_str()),
                TurnItem::ToolResult(result) => format!("result:{}", result.call_id.as_str()),
                TurnItem::UserMessage(_) => "user".to_owned(),
                _ => "other".to_owned(),
            })
            .collect()
    }

    #[test]
    fn test_repair_open_tool_calls_inserts_runtime_error_at_end_of_round() {
        let mut history = ConversationHistory::from_items(vec![
            tool_call("a"),
            tool_call("b"),
            tool_result("b"),
            user_item("next"),
        ]);

        let repaired = repair_open_tool_calls(&mut history);

        assert_eq!(repaired, vec![ToolCallId::from_str("a")]);
        assert_eq!(
            kinds(&history),
            vec!["call:a", "call:b", "result:b", "result:a", "user"]
        );
        let TurnItem::ToolResult(synthetic) = &history.items()[3] else {
            panic!("synthetic result expected at index 3");
        };
        assert_eq!(synthetic.trust, ResultTrust::Runtime);
        assert_eq!(
            synthetic.result,
            ToolCallResult::error(INTERRUPTED_TOOL_CALL_MESSAGE)
        );
        assert_eq!(synthetic.id.as_str(), "synthetic-result-a");
    }

    #[test]
    fn test_repair_open_tool_calls_is_idempotent_and_keeps_answered_history() {
        let mut history = ConversationHistory::from_items(vec![tool_call("a")]);
        assert_eq!(repair_open_tool_calls(&mut history).len(), 1);
        assert_eq!(kinds(&history), vec!["call:a", "result:a"]);

        assert!(repair_open_tool_calls(&mut history).is_empty());
        assert_eq!(history.len(), 2);
    }

    #[test]
    fn test_repair_open_tool_calls_never_duplicates_a_late_result() {
        let mut history = ConversationHistory::from_items(vec![
            tool_call("a"),
            user_item("interleaved"),
            tool_result("a"),
        ]);

        assert!(repair_open_tool_calls(&mut history).is_empty());
        assert_eq!(history.len(), 3);
    }

    #[tokio::test]
    async fn test_load_history_repairs_open_calls_in_both_stores() {
        let sid = session();
        let memory = InMemoryStateStore::new();
        let persisted = ConversationHistory::from_items(vec![user_item("q"), tool_call("open")]);
        memory.save_history(&sid, &persisted).await.unwrap();
        let loaded = memory.load_history(&sid).await.unwrap();
        assert_eq!(kinds(&loaded), vec!["user", "call:open", "result:open"]);

        let temp = tempfile::tempdir().unwrap();
        let durable =
            TranscriptStateStore::new(TranscriptStore::new(temp.path()), transcript_thread);
        durable.save_history(&sid, &persisted).await.unwrap();
        let loaded = durable.load_history(&sid).await.unwrap();
        assert_eq!(kinds(&loaded), vec!["user", "call:open", "result:open"]);
    }

    fn snapshot(mode: InteractionMode, input_tokens: u64) -> SessionStateSnapshot {
        let mut activation = SessionActivation::new(ToolProfile::Full);
        activation.disable_tool(ToolName::new("fs.write"));
        let names = ["fs.read", "fs.write"];
        SessionStateSnapshot {
            version: SESSION_STATE_VERSION,
            mode,
            total_usage: TokenUsage {
                input_tokens,
                output_tokens: 2,
                reasoning_tokens: None,
                cached_tokens: None,
            },
            executable_snapshot_id: None,
            base_activation: ActivationSnapshot::capture(
                &SessionActivation::default(),
                names.iter().copied(),
            ),
            activation: ActivationSnapshot::capture(&activation, names.iter().copied()),
        }
    }

    #[tokio::test]
    async fn test_transcript_session_state_latest_wins_and_ignores_foreign_lifecycle() {
        let temp = tempfile::tempdir().unwrap();
        let sid = session();
        let store = TranscriptStateStore::new(TranscriptStore::new(temp.path()), transcript_thread);
        assert_eq!(store.load_session_state(&sid).await.unwrap(), None);

        store
            .save_session_state(&sid, &snapshot(InteractionMode::Plan, 1))
            .await
            .unwrap();
        store.save_turn(&sid, &error_item("between")).await.unwrap();
        store
            .save_session_state(&sid, &snapshot(InteractionMode::Explore, 7))
            .await
            .unwrap();
        store
            .append_record(
                &sid,
                RecordKind::Lifecycle,
                serde_json::json!({ "event": "opened" }),
            )
            .await
            .unwrap();

        let restarted =
            TranscriptStateStore::new(TranscriptStore::new(temp.path()), transcript_thread);
        let loaded = restarted.load_session_state(&sid).await.unwrap();
        assert_eq!(loaded, Some(snapshot(InteractionMode::Explore, 7)));
        let history = restarted.load_history(&sid).await.unwrap();
        assert_eq!(history.len(), 1, "state records never leak into the history");
    }

    #[tokio::test]
    async fn test_in_memory_session_state_round_trips() {
        let store = InMemoryStateStore::new();
        let sid = session();
        store
            .save_session_state(&sid, &snapshot(InteractionMode::Work, 3))
            .await
            .unwrap();
        assert_eq!(
            store.load_session_state(&sid).await.unwrap(),
            Some(snapshot(InteractionMode::Work, 3))
        );
    }

    #[test]
    fn test_activation_snapshot_round_trip_preserves_probed_visibility() {
        let mut activation = SessionActivation::new(ToolProfile::Minimal);
        activation.enable_tool(ToolName::new("fs.read"));
        let captured =
            ActivationSnapshot::capture(&activation, ["fs.read", "fs.write"].iter().copied());

        let restored = captured.to_activation();

        assert_eq!(restored.profile(), ToolProfile::Minimal);
        assert!(restored.is_tool_enabled(&ToolName::new("fs.read")));
        assert!(!restored.is_tool_enabled(&ToolName::new("fs.write")));
        assert!(!restored.is_tool_enabled(&ToolName::new("unprobed")));
    }

    #[tokio::test]
    async fn test_transcript_store_parallel_sessions_get_independent_sequences() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(TranscriptStateStore::new(
            TranscriptStore::new(temp.path()),
            transcript_thread,
        ));
        let mut handles = Vec::new();
        for session_index in 0..2 {
            for item_index in 0..5 {
                let store = Arc::clone(&store);
                handles.push(tokio::spawn(async move {
                    let sid = SessionId::from_str(format!("parallel-{session_index}"));
                    store
                        .save_turn(&sid, &error_item(&format!("item {item_index}")))
                        .await
                }));
            }
        }
        for handle in handles {
            handle.await.unwrap().unwrap();
        }

        for session_index in 0..2 {
            let mut sequences = TranscriptStore::new(temp.path())
                .reader(&SessionId::from_str(format!("parallel-{session_index}")))
                .unwrap()
                .collect::<harw_session_store::SessionStoreResult<Vec<_>>>()
                .unwrap()
                .into_iter()
                .map(|record| record.sequence)
                .collect::<Vec<_>>();
            sequences.sort_unstable();
            assert_eq!(sequences, (0..5).collect::<Vec<u64>>());
        }
    }
}
