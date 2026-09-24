//! Gesprächs-Sitzungen für zugelassene Telegram-Nachrichten.
//!
//! # Verantwortung
//! [`TelegramSessionDispatcher`] nimmt zugelassene Nachrichten nicht
//! blockierend entgegen ([`TelegramSessionDispatcher::dispatch`]) und führt je
//! Chat ([`SessionKey`]) genau einen Turn nach dem anderen aus (FIFO), über
//! verschiedene Chats hinweg parallel auf höchstens `max_parallel_sessions`
//! Worker-Threads. Jeder Worker besitzt eine eigene `current_thread`-Runtime.
//!
//! # Kontinuität
//! Die Sitzungs-ID eines Chats stammt aus [`ChatStateStore::session_id`]
//! (`/new` erhöht dort die Generation). Jeder Turn baut eine
//! [`AgentSession`] mit dieser ID und hydriert sie aus dem Transkript-Store
//! ([`AgentSession::hydrate_from_store`]) – so setzt jede Nachricht das
//! bisherige Gespräch fort, auch über Neustarts hinweg.
//!
//! # Streaming
//! Während eines Turns werden `TurnEvent::AssistantDelta`-Ereignisse
//! gesammelt und höchstens alle [`STREAM_UPDATE_INTERVAL`] (1,5 s) als
//! Streaming-Blase aktualisiert (Schlüssel = Turn-ID); am Ende ersetzt
//! `stream_finish_async` die Blase durch den endgültigen Text. Alle
//! [`TYPING_REFRESH`] wird „schreibt …“ erneuert.
//!
//! # Freigaben
//! Endet ein Turn mit `TurnOutcome::AwaitingApproval`, wird die Sitzung
//! geparkt, der Chat blockiert (spätere Nachrichten warten in der FIFO) und
//! eine gebundene Freigabe mit `request_id = "turn:<request>"` an den
//! Absender der auslösenden Nachricht geschickt. Die Entscheidung kommt über
//! [`TurnApprovalSink::resolve`] (Callback-Worker) und wird als
//! Wiederaufnahme-Job vorn in die FIFO des Chats gestellt; dort läuft
//! [`resume_after_approval`] mit `ApprovalActor::ChannelPeer`. Ein Sweeper
//! prüft alle [`SWEEP_INTERVAL`] (30 s) abgelaufene Freigaben
//! (`ApprovalResolution::timed_out`), räumt abgelaufene Tokens und
//! Anhänge auf. Die Freigabe-Nachricht wird beim Abschluss (Entscheidung
//! oder Ablauf) durch den Wiederaufnahme-Job geschlossen
//! (`close_approval_async`).
//!
//! # Sicherheit / Logging
//! Nachrichtentexte, Modellantworten und Tokens werden nie geloggt.
//! Sperren sind vergiftungstolerant; keine `unwrap`/`expect`.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak, mpsc as std_mpsc};
use std::time::{Duration, Instant};

use harw_channel::{ApprovalAction, ApprovalPrompt, InboundEvent, OutboundContent, SessionKey};
use harw_channel_telegram::ChatStateStore;
use harw_channel_telegram_transport::TelegramRenderer;
use harw_core::{
    AgentSession, ApprovalResolution, CoreResult, ModelProvider, TurnInput, TurnOutcome,
    resume_after_approval, run_turn,
};
use harw_extension_api::empty_extension_registry;
use harw_protocol::TurnEvent;
use harw_types::{AgentRole, ApprovalActor, PeerId};
use jiff::{SignedDuration, Timestamp};
use tokio::runtime::Runtime;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use super::telegram_attachments::{IngestReport, TelegramAttachmentIntake};
use super::telegram_callbacks::TurnApprovalSink;

/// Mindestabstand zwischen zwei Streaming-Aktualisierungen.
pub(super) const STREAM_UPDATE_INTERVAL: Duration = Duration::from_millis(1500);
/// Abstand, in dem „schreibt …“ erneuert wird (Telegram zeigt es ~5 s).
pub(super) const TYPING_REFRESH: Duration = Duration::from_secs(4);
/// Intervall des Sweepers für abgelaufene Freigaben, Tokens und Anhänge.
pub(super) const SWEEP_INTERVAL: Duration = Duration::from_secs(30);
/// Präfix der `request_id` von Turn-Freigaben (Gegenstück in
/// `telegram_callbacks`).
const TURN_REQUEST_PREFIX: &str = "turn:";
/// Höchstzahl wartender Nachrichten je Chat; weitere werden verworfen.
const MAX_QUEUED_PER_KEY: usize = 32;
/// Obergrenze für Worker-Threads, unabhängig von der Konfiguration.
const MAX_WORKER_THREADS: usize = 64;
/// Ablehnungsgrund einer Nutzer-Ablehnung (wie `From<ReviewDecision>`).
const REJECTED_BY_USER: &str = "rejected by user";
/// Ablehnungsgrund, wenn die Freigabe nicht zugestellt werden konnte.
const APPROVAL_UNDELIVERABLE: &str = "approval prompt could not be delivered";

/// Verdrahtung des Dispatchers (von `gateway.rs` befüllt).
pub(super) struct TelegramSessionConfig {
    /// Governter Modell-Provider der Gateway-Runtime.
    pub provider: Arc<dyn ModelProvider>,
    /// Wurzel der Gateway-Transkripte (dieselbe wie für Dream).
    pub transcript_root: PathBuf,
    /// Renderer mit dem geteilten Approval-Token-Store der Bindung.
    pub renderer: Arc<TelegramRenderer>,
    /// Chat-Zustand (Sitzungsgeneration, Workspace-Alias).
    pub chat_state: Arc<ChatStateStore>,
    /// Anhang-Aufnahme; `None` lehnt Anhänge mit Hinweis ab.
    pub attachments: Option<Arc<TelegramAttachmentIntake>>,
    /// Höchstzahl gleichzeitig laufender Chats (Worker-Threads, ≥ 1).
    pub max_parallel_sessions: usize,
    /// Obergrenze der Wartezeit auf eine Freigabe (zusätzlich zur
    /// `timeout_at` der Kern-Session; die frühere gilt).
    pub approval_ttl: SignedDuration,
}

// ---------------------------------------------------------------------------
// Scheduler: FIFO je Schlüssel, parallel über Schlüssel
// ---------------------------------------------------------------------------

/// Warteschlange eines Chats.
struct Lane<J> {
    queue: VecDeque<J>,
    /// Ein Worker bearbeitet gerade einen Job dieses Chats.
    running: bool,
    /// Eine Freigabe ist offen; bis zur Entscheidung startet kein Job.
    blocked: bool,
    /// Der Schlüssel steht in `Scheduler::ready`.
    scheduled: bool,
}

impl<J> Default for Lane<J> {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            running: false,
            blocked: false,
            scheduled: false,
        }
    }
}

/// Reiner, thread-freier Planer (unter einem Mutex von [`Pool`] benutzt).
///
/// Invariante: ein Schlüssel steht genau dann in `ready`, wenn seine Lane
/// weder läuft noch blockiert ist und Jobs wartet.
struct Scheduler<J> {
    lanes: HashMap<SessionKey, Lane<J>>,
    ready: VecDeque<SessionKey>,
    closed: bool,
    max_queued: usize,
}

impl<J> Scheduler<J> {
    fn new(max_queued: usize) -> Self {
        Self {
            lanes: HashMap::new(),
            ready: VecDeque::new(),
            closed: false,
            max_queued,
        }
    }

    /// Hängt `job` hinten an; `Err(job)` bei voller Warteschlange oder
    /// geschlossenem Planer.
    fn enqueue(&mut self, key: SessionKey, job: J) -> Result<(), J> {
        if self.closed {
            return Err(job);
        }
        let lane = self.lanes.entry(key.clone()).or_default();
        if lane.queue.len() >= self.max_queued {
            return Err(job);
        }
        lane.queue.push_back(job);
        self.schedule(&key);
        Ok(())
    }

    /// Stellt `job` vorn an und hebt eine Blockade auf (Wiederaufnahme nach
    /// einer Freigabe-Entscheidung; umgeht die Warteschlangen-Grenze).
    fn enqueue_front_and_unblock(&mut self, key: SessionKey, job: J) {
        let lane = self.lanes.entry(key.clone()).or_default();
        lane.queue.push_front(job);
        lane.blocked = false;
        self.schedule(&key);
    }

    /// Blockiert den Chat (nur während eines laufenden Jobs sinnvoll).
    fn block(&mut self, key: &SessionKey) {
        if let Some(lane) = self.lanes.get_mut(key) {
            lane.blocked = true;
        }
    }

    fn schedule(&mut self, key: &SessionKey) {
        if let Some(lane) = self.lanes.get_mut(key) {
            if !lane.running && !lane.blocked && !lane.scheduled && !lane.queue.is_empty() {
                lane.scheduled = true;
                self.ready.push_back(key.clone());
            }
        }
    }

    /// Nächster ausführbarer Job; markiert dessen Chat als laufend.
    fn take_next(&mut self) -> Option<(SessionKey, J)> {
        while let Some(key) = self.ready.pop_front() {
            let Some(lane) = self.lanes.get_mut(&key) else {
                continue;
            };
            lane.scheduled = false;
            if lane.running || lane.blocked {
                continue;
            }
            if let Some(job) = lane.queue.pop_front() {
                lane.running = true;
                return Some((key, job));
            }
            self.cleanup(&key);
        }
        None
    }

    /// Ein Job des Chats ist beendet.
    fn finish(&mut self, key: &SessionKey) {
        if let Some(lane) = self.lanes.get_mut(key) {
            lane.running = false;
        }
        self.schedule(key);
        self.cleanup(key);
    }

    fn cleanup(&mut self, key: &SessionKey) {
        let idle = self.lanes.get(key).is_some_and(|lane| {
            !lane.running && !lane.blocked && !lane.scheduled && lane.queue.is_empty()
        });
        if idle {
            self.lanes.remove(key);
        }
    }

    #[cfg(test)]
    fn lane_count(&self) -> usize {
        self.lanes.len()
    }
}

/// Thread-sicherer Rahmen um [`Scheduler`].
struct Pool<J> {
    state: Mutex<Scheduler<J>>,
    wake: Condvar,
}

impl<J> Pool<J> {
    fn new(max_queued: usize) -> Self {
        Self {
            state: Mutex::new(Scheduler::new(max_queued)),
            wake: Condvar::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Scheduler<J>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ändert den Planer und weckt alle wartenden Worker.
    fn with<R>(&self, change: impl FnOnce(&mut Scheduler<J>) -> R) -> R {
        let result = {
            let mut state = self.lock();
            change(&mut state)
        };
        self.wake.notify_all();
        result
    }

    fn close(&self) {
        self.with(|state| {
            state.closed = true;
            state.lanes.clear();
            state.ready.clear();
        });
    }

    /// Blockiert bis zum nächsten Job; `None` nach [`Self::close`].
    fn next_job(&self) -> Option<(SessionKey, J)> {
        let mut state = self.lock();
        loop {
            if state.closed {
                return None;
            }
            if let Some(next) = state.take_next() {
                return Some(next);
            }
            state = self
                .wake
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// Worker-Schleife: holt Jobs, bis der Pool geschlossen wird. Eine Panik im
/// Handler wird abgefangen, damit der Chat nicht dauerhaft „läuft“.
fn worker_loop<J>(pool: &Pool<J>, mut handle: impl FnMut(&SessionKey, J)) {
    while let Some((key, job)) = pool.next_job() {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handle(&key, job)));
        if outcome.is_err() {
            tracing::error!(channel = %key.channel, peer = %key.peer, "Telegram session job panicked");
        }
        pool.with(|state| state.finish(&key));
    }
}

// ---------------------------------------------------------------------------
// Jobs und geparkte Freigaben
// ---------------------------------------------------------------------------

/// Zielort im Chat.
#[derive(Debug, Clone, Copy)]
struct TurnTarget {
    chat_id: i64,
    thread_id: Option<i64>,
}

/// Eine auf Freigabe wartende Sitzung.
struct Parked {
    key: SessionKey,
    session: AgentSession,
    turn_rx: UnboundedReceiver<TurnEvent>,
    target: TurnTarget,
    /// Einziger zugelassener Entscheider (Absender der auslösenden Nachricht).
    approver: PeerId,
    /// `"turn:<request>"`.
    request_id: String,
    summary: String,
    /// Telegram-Nachricht mit den Schaltflächen, sobald zugestellt.
    message_id: Option<i64>,
    deadline: Timestamp,
}

struct ResumeJob {
    parked: Parked,
    actor: ApprovalActor,
    resolution: ApprovalResolution,
    /// Text, mit dem die Freigabe-Nachricht geschlossen wird.
    outcome_text: &'static str,
}

enum Job {
    Message(InboundEvent),
    Resume(Box<ResumeJob>),
}

/// Laufender Streaming-Zustand eines Turns.
struct StreamState {
    key: Option<String>,
    fallback_key: String,
    text: String,
    separator_pending: bool,
    last_push: Instant,
}

impl StreamState {
    fn new(fallback_key: String) -> Self {
        Self {
            key: None,
            fallback_key,
            text: String::new(),
            separator_pending: false,
            last_push: Instant::now(),
        }
    }

    fn stream_key(&self) -> String {
        self.key
            .clone()
            .unwrap_or_else(|| self.fallback_key.clone())
    }

    /// Nimmt ein Ereignis auf; `true`, wenn jetzt aktualisiert werden soll.
    fn absorb(&mut self, event: TurnEvent, now: Instant) -> bool {
        match event {
            TurnEvent::TurnStarted { turn_id, .. } => {
                self.key.get_or_insert_with(|| turn_id.to_string());
                false
            }
            TurnEvent::AssistantDelta { turn_id, text } => {
                self.key.get_or_insert_with(|| turn_id.to_string());
                if self.separator_pending && !self.text.is_empty() {
                    self.text.push_str("\n\n");
                }
                self.separator_pending = false;
                self.text.push_str(&text);
                should_push(self.last_push, now) && !self.text.trim().is_empty()
            }
            TurnEvent::ToolCallRequested { .. } => {
                self.separator_pending = true;
                false
            }
            _ => false,
        }
    }
}

/// Drosselung der Streaming-Aktualisierungen.
fn should_push(last_push: Instant, now: Instant) -> bool {
    now.saturating_duration_since(last_push) >= STREAM_UPDATE_INTERVAL
}

/// Normalisiert eine Freigabe-ID auf `"turn:<request>"`.
fn normalize_turn_request_id(request_id: &str) -> String {
    if request_id.starts_with(TURN_REQUEST_PREFIX) {
        request_id.to_owned()
    } else {
        format!("{TURN_REQUEST_PREFIX}{request_id}")
    }
}

/// Freigabe-Aufforderung für eine pausierte Turn-Anfrage.
fn turn_approval_prompt(request_id: &str, summary: &str) -> ApprovalPrompt {
    ApprovalPrompt {
        request_id: request_id.to_owned(),
        summary: summary.to_owned(),
        risk: "elevated".to_owned(),
        actions: vec![
            ApprovalAction {
                label: "Freigeben".to_owned(),
                decision: "approve".to_owned(),
            },
            ApprovalAction {
                label: "Ablehnen".to_owned(),
                decision: "deny".to_owned(),
            },
        ],
    }
}

/// Baut den User-Text eines Turns aus Nachrichtentext und Anhang-Präambel.
/// `None`, wenn nichts auszuführen ist.
fn compose_user_text(text: Option<&str>, report: Option<&IngestReport>) -> Option<String> {
    let text = text.map(str::trim).filter(|text| !text.is_empty());
    let report = report.filter(|report| report.has_ingested());
    match (text, report) {
        (None, None) => None,
        (Some(text), None) => Some(text.to_owned()),
        (text, Some(report)) => {
            let preamble = report.prompt_preamble();
            let body = text.unwrap_or("Bitte berücksichtige die angehängten Dateien.");
            Some(format!("{preamble}\n{body}"))
        }
    }
}

fn last_assistant_text(session: &AgentSession) -> Option<String> {
    session
        .history()
        .to_model_messages()
        .into_iter()
        .rev()
        .find_map(|message| match message {
            harw_core::ModelMessage::Assistant { text } if !text.trim().is_empty() => Some(text),
            _ => None,
        })
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

struct Engine {
    config: TelegramSessionConfig,
    pool: Pool<Job>,
    parked: Mutex<HashMap<String, Parked>>,
    stream_seq: AtomicU64,
}

impl Engine {
    fn parked(&self) -> MutexGuard<'_, HashMap<String, Parked>> {
        self.parked.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn process(&self, runtime: &Runtime, key: &SessionKey, job: Job) {
        match job {
            Job::Message(event) => self.process_message(runtime, key, event),
            Job::Resume(resume) => self.process_resume(runtime, *resume),
        }
    }

    fn new_stream(&self) -> StreamState {
        let seq = self.stream_seq.fetch_add(1, Ordering::Relaxed);
        StreamState::new(format!("tg-turn-{seq}"))
    }

    fn notify(&self, runtime: &Runtime, target: TurnTarget, markdown: String) {
        let content = OutboundContent::Message { markdown };
        if let Err(error) = runtime.block_on(self.config.renderer.send_async(
            target.chat_id,
            target.thread_id,
            &content,
        )) {
            tracing::warn!(error = %error, "Telegram session notice delivery failed");
        }
    }

    fn process_message(&self, runtime: &Runtime, key: &SessionKey, event: InboundEvent) {
        let Ok(chat_id) = event.peer.as_str().parse::<i64>() else {
            tracing::warn!(channel = %key.channel, peer = %key.peer, "Telegram peer is not a numeric chat id");
            return;
        };
        let target = TurnTarget {
            chat_id,
            thread_id: event
                .thread
                .as_ref()
                .and_then(|thread| thread.as_str().parse::<i64>().ok()),
        };
        let approver = event
            .sender
            .as_ref()
            .map(|sender| PeerId::from_str(sender.id.clone()))
            .unwrap_or_else(|| event.peer.clone());

        let report = if event.attachments.is_empty() {
            None
        } else {
            Some(match &self.config.attachments {
                Some(intake) => {
                    runtime.block_on(intake.ingest(key, &event.attachments, Timestamp::now()))
                }
                None => IngestReport {
                    rejected: (1..=event.attachments.len())
                        .map(|n| {
                            format!("Anhang {n}: Anhänge sind für diesen Chat nicht aktiviert")
                        })
                        .collect(),
                    ..IngestReport::default()
                },
            })
        };
        if let Some(notice) = report.as_ref().and_then(IngestReport::rejection_notice) {
            self.notify(runtime, target, notice);
        }
        let Some(user_text) = compose_user_text(event.text.as_deref(), report.as_ref()) else {
            tracing::debug!(channel = %key.channel, peer = %key.peer, "Telegram event carries nothing to run");
            return;
        };
        let mut input = TurnInput::user(user_text);
        if let Some(report) = report.as_ref().filter(|report| report.has_ingested()) {
            input.metadata =
                serde_json::json!({ "telegram": { "attachments": report.metadata() } });
        }

        let session_id = match self.config.chat_state.session_id(key) {
            Ok(session_id) => session_id,
            Err(error) => {
                tracing::error!(channel = %key.channel, peer = %key.peer, error = %error, "Telegram chat session id unavailable");
                self.notify(
                    runtime,
                    target,
                    "Die Sitzung konnte nicht geladen werden.".to_owned(),
                );
                return;
            }
        };
        let (event_tx, _event_rx) = unbounded_channel();
        let (turn_tx, mut turn_rx) = unbounded_channel();
        let mut session = AgentSession::new_with_id(
            session_id,
            AgentRole::Assistant,
            None,
            empty_extension_registry(),
            event_tx,
        )
        .with_turn_event_sink(turn_tx);
        let store = super::build_telegram_state_store(&self.config.transcript_root);
        if let Err(error) = runtime.block_on(session.hydrate_from_store(&store)) {
            tracing::error!(channel = %key.channel, peer = %key.peer, error = %error, "Telegram session hydration failed");
            self.notify(
                runtime,
                target,
                "Der bisherige Gesprächsverlauf konnte nicht geladen werden.".to_owned(),
            );
            return;
        }
        let mut stream = self.new_stream();
        let outcome = runtime.block_on(self.pump(
            run_turn(&mut session, self.config.provider.as_ref(), &store, input),
            &mut turn_rx,
            &mut stream,
            target,
        ));
        self.conclude(
            runtime, key, target, approver, session, turn_rx, stream, outcome,
        );
    }

    fn process_resume(&self, runtime: &Runtime, job: ResumeJob) {
        let ResumeJob {
            parked,
            actor,
            resolution,
            outcome_text,
        } = job;
        let Parked {
            key,
            mut session,
            mut turn_rx,
            target,
            approver,
            request_id,
            summary,
            message_id,
            deadline: _,
        } = parked;
        let _ = self
            .config
            .renderer
            .approval_tokens()
            .revoke_request(&request_id);
        if let Some(message_id) = message_id {
            let text = format!("{summary}\n\n{outcome_text}");
            if let Err(error) = runtime.block_on(self.config.renderer.close_approval_async(
                target.chat_id,
                message_id,
                &text,
            )) {
                tracing::warn!(error = %error, "Telegram turn approval could not be closed");
            }
        }
        let store = super::build_telegram_state_store(&self.config.transcript_root);
        let mut stream = self.new_stream();
        let outcome = runtime.block_on(self.pump(
            resume_after_approval(
                &mut session,
                self.config.provider.as_ref(),
                &store,
                actor,
                resolution,
            ),
            &mut turn_rx,
            &mut stream,
            target,
        ));
        self.conclude(
            runtime, &key, target, approver, session, turn_rx, stream, outcome,
        );
    }

    /// Treibt einen Turn und leert parallel die Turn-Ereignisse
    /// (Streaming, „schreibt …“).
    async fn pump<F>(
        &self,
        turn: F,
        turn_rx: &mut UnboundedReceiver<TurnEvent>,
        stream: &mut StreamState,
        target: TurnTarget,
    ) -> CoreResult<TurnOutcome>
    where
        F: Future<Output = CoreResult<TurnOutcome>>,
    {
        let mut turn = std::pin::pin!(turn);
        let mut typing = tokio::time::interval(TYPING_REFRESH);
        typing.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut events_open = true;
        loop {
            tokio::select! {
                biased;
                outcome = &mut turn => return outcome,
                event = turn_rx.recv(), if events_open => match event {
                    Some(event) => {
                        let now = Instant::now();
                        if stream.absorb(event, now) {
                            stream.last_push = now;
                            if let Err(error) = self
                                .config
                                .renderer
                                .stream_update_async(
                                    target.chat_id,
                                    target.thread_id,
                                    &stream.stream_key(),
                                    &stream.text,
                                )
                                .await
                            {
                                tracing::debug!(error = %error, "Telegram stream update failed");
                            }
                        }
                    }
                    None => events_open = false,
                },
                _ = typing.tick() => {
                    if let Err(error) = self
                        .config
                        .renderer
                        .typing_async(target.chat_id, target.thread_id)
                        .await
                    {
                        tracing::debug!(error = %error, "Telegram typing indicator failed");
                    }
                }
            }
        }
    }

    fn finish_stream(
        &self,
        runtime: &Runtime,
        target: TurnTarget,
        stream: &StreamState,
        text: &str,
    ) {
        if let Err(error) = runtime.block_on(self.config.renderer.stream_finish_async(
            target.chat_id,
            target.thread_id,
            &stream.stream_key(),
            text,
        )) {
            tracing::error!(error = %error, "Telegram turn reply delivery failed");
        }
    }

    /// Wertet das Turn-Ergebnis aus: Antwort senden, Hinweis senden oder
    /// die Sitzung für eine Freigabe parken.
    #[allow(clippy::too_many_arguments)]
    fn conclude(
        &self,
        runtime: &Runtime,
        key: &SessionKey,
        target: TurnTarget,
        approver: PeerId,
        session: AgentSession,
        mut turn_rx: UnboundedReceiver<TurnEvent>,
        stream: StreamState,
        outcome: CoreResult<TurnOutcome>,
    ) {
        // Nachzügler dieses Turns verwerfen, damit ein späterer
        // Wiederaufnahme-Stream nicht mit altem Text beginnt.
        while turn_rx.try_recv().is_ok() {}
        let notice = match outcome {
            Ok(TurnOutcome::Completed) => {
                let text = last_assistant_text(&session)
                    .unwrap_or_else(|| "(keine Textantwort)".to_owned());
                self.finish_stream(runtime, target, &stream, &text);
                return;
            }
            Ok(TurnOutcome::AwaitingApproval { request, .. }) => {
                self.park(
                    runtime,
                    key,
                    target,
                    approver,
                    session,
                    turn_rx,
                    &stream,
                    &request.to_string(),
                );
                return;
            }
            Ok(TurnOutcome::Cancelled { reason }) => {
                tracing::warn!(?reason, "Telegram turn cancelled");
                "Der Turn wurde abgebrochen."
            }
            Ok(TurnOutcome::Truncated) => "Die Antwort wurde abgeschnitten.",
            Ok(TurnOutcome::Refused { .. }) => "Das Modell hat die Anfrage abgelehnt.",
            Ok(TurnOutcome::Failed { reason }) => {
                tracing::warn!(reason = %reason, "Telegram turn failed");
                "Die Anfrage ist fehlgeschlagen."
            }
            Ok(TurnOutcome::AwaitingChild { .. }) => {
                tracing::warn!(
                    "Telegram turn requested a child agent; unsupported on this surface"
                );
                "Diese Anfrage würde einen Unteragenten starten; das wird über Telegram nicht unterstützt."
            }
            Err(error) => {
                tracing::error!(error = %error, "Telegram governed turn failed");
                "Die Anfrage konnte nicht verarbeitet werden."
            }
        };
        let text = if stream.text.trim().is_empty() {
            notice.to_owned()
        } else {
            format!("{}\n\n({notice})", stream.text)
        };
        self.finish_stream(runtime, target, &stream, &text);
    }

    #[allow(clippy::too_many_arguments)]
    fn park(
        &self,
        runtime: &Runtime,
        key: &SessionKey,
        target: TurnTarget,
        approver: PeerId,
        session: AgentSession,
        turn_rx: UnboundedReceiver<TurnEvent>,
        stream: &StreamState,
        request: &str,
    ) {
        if !stream.text.trim().is_empty() {
            self.finish_stream(runtime, target, stream, &stream.text);
        }
        let now = Timestamp::now();
        let ttl_deadline = now
            .checked_add(self.config.approval_ttl)
            .unwrap_or(Timestamp::MAX);
        let (summary, deadline) = match session.pending_approval() {
            Some(pending) => (
                format!(
                    "Freigabe erforderlich: Werkzeug `{}` möchte ausgeführt werden.",
                    pending.call.name
                ),
                pending.timeout_at.min(ttl_deadline),
            ),
            None => (
                "Freigabe erforderlich: eine Aktion möchte ausgeführt werden.".to_owned(),
                ttl_deadline,
            ),
        };
        let request_id = normalize_turn_request_id(request);
        let prompt = turn_approval_prompt(&request_id, &summary);

        // Reihenfolge: erst blockieren, dann parken – so kann eine schnelle
        // Entscheidung die Blockade nie vor ihrer Entstehung aufheben.
        self.pool.with(|state| state.block(key));
        self.parked().insert(
            request_id.clone(),
            Parked {
                key: key.clone(),
                session,
                turn_rx,
                target,
                approver: approver.clone(),
                request_id: request_id.clone(),
                summary,
                message_id: None,
                deadline,
            },
        );
        match runtime.block_on(self.config.renderer.send_bound_approval_async(
            target.chat_id,
            target.thread_id,
            &prompt,
            &approver,
        )) {
            Ok(sent) => {
                if let Some(parked) = self.parked().get_mut(&request_id) {
                    parked.message_id = Some(sent.message_id);
                } else {
                    tracing::debug!(
                        "Telegram turn approval resolved before its message id was recorded"
                    );
                }
            }
            Err(error) => {
                tracing::error!(error = %error, "Telegram turn approval could not be delivered");
                let parked = self.parked().remove(&request_id);
                if let Some(parked) = parked {
                    let actor = pending_actor_or(&parked);
                    self.pool.with(|state| {
                        state.enqueue_front_and_unblock(
                            parked.key.clone(),
                            Job::Resume(Box::new(ResumeJob {
                                parked,
                                actor,
                                resolution: ApprovalResolution::Reject {
                                    reason: APPROVAL_UNDELIVERABLE.to_owned(),
                                },
                                outcome_text: "Freigabe nicht zustellbar – abgelehnt.",
                            })),
                        );
                    });
                }
            }
        }
    }

    /// Nimmt eine Freigabe-Entscheidung entgegen (siehe [`TurnApprovals`]).
    fn resolve(&self, request_id: &str, approve: bool, actor: &PeerId) -> Result<String, String> {
        let request_id = normalize_turn_request_id(request_id);
        let parked = {
            let mut parked = self.parked();
            match parked.get(&request_id) {
                None => return Err("Diese Freigabe ist nicht mehr offen.".to_owned()),
                Some(entry) if &entry.approver != actor => {
                    return Err("Keine Berechtigung für diese Entscheidung.".to_owned());
                }
                Some(_) => {}
            }
            match parked.remove(&request_id) {
                Some(entry) => entry,
                None => return Err("Diese Freigabe ist nicht mehr offen.".to_owned()),
            }
        };
        let (resolution, outcome_text, reply) = if approve {
            (
                ApprovalResolution::Approve,
                "Entscheidung: freigegeben.",
                "Freigegeben – der Turn läuft weiter.",
            )
        } else {
            (
                ApprovalResolution::Reject {
                    reason: REJECTED_BY_USER.to_owned(),
                },
                "Entscheidung: abgelehnt.",
                "Abgelehnt.",
            )
        };
        let key = parked.key.clone();
        let actor = ApprovalActor::ChannelPeer {
            channel: key.channel.clone(),
            peer: actor.clone(),
        };
        self.pool.with(|state| {
            state.enqueue_front_and_unblock(
                key,
                Job::Resume(Box::new(ResumeJob {
                    parked,
                    actor,
                    resolution,
                    outcome_text,
                })),
            );
        });
        Ok(reply.to_owned())
    }

    /// Lehnt abgelaufene Freigaben ab und räumt Tokens/Anhänge auf.
    fn sweep(&self, now: Timestamp) {
        let expired: Vec<Parked> = {
            let mut parked = self.parked();
            let ids: Vec<String> = parked
                .iter()
                .filter(|(_, entry)| entry.deadline <= now)
                .map(|(id, _)| id.clone())
                .collect();
            ids.iter().filter_map(|id| parked.remove(id)).collect()
        };
        for parked in expired {
            tracing::info!(channel = %parked.key.channel, peer = %parked.key.peer, "Telegram turn approval timed out");
            let actor = pending_actor_or(&parked);
            self.pool.with(|state| {
                state.enqueue_front_and_unblock(
                    parked.key.clone(),
                    Job::Resume(Box::new(ResumeJob {
                        parked,
                        actor,
                        resolution: ApprovalResolution::timed_out(),
                        outcome_text: "Freigabe abgelaufen – abgelehnt.",
                    })),
                );
            });
        }
        let purged_tokens = self.config.renderer.approval_tokens().purge_expired(now);
        if purged_tokens > 0 {
            tracing::debug!(purged_tokens, "Telegram approval tokens purged");
        }
        if let Some(attachments) = &self.config.attachments {
            match attachments.purge_expired(now) {
                Ok(0) => {}
                Ok(purged) => tracing::debug!(purged, "Telegram attachments purged"),
                Err(error) => {
                    tracing::warn!(error = %error, "Telegram attachment cache purge failed");
                }
            }
        }
    }
}

/// Akteur der Kern-Pause oder, ohne Pause, der gebundene Chat-Entscheider.
fn pending_actor_or(parked: &Parked) -> ApprovalActor {
    parked
        .session
        .pending_approval()
        .map(|pending| pending.actor.clone())
        .unwrap_or_else(|| ApprovalActor::ChannelPeer {
            channel: parked.key.channel.clone(),
            peer: parked.approver.clone(),
        })
}

// ---------------------------------------------------------------------------
// Öffentliche Oberfläche
// ---------------------------------------------------------------------------

/// Verteilt zugelassene Nachrichten auf Chat-Sitzungen (siehe Moduldoku).
///
/// Beim Verwerfen werden Worker und Sweeper beendet; noch wartende
/// Nachrichten und geparkte Freigaben verfallen.
pub(super) struct TelegramSessionDispatcher {
    engine: Arc<Engine>,
    /// Wird nur verworfen: beendet den Sweeper.
    _sweeper_stop: std_mpsc::Sender<()>,
}

impl TelegramSessionDispatcher {
    /// Startet Worker-Threads (`max_parallel_sessions`, mindestens 1,
    /// höchstens 64) und den Sweeper.
    pub fn new(config: TelegramSessionConfig) -> Self {
        let workers = config.max_parallel_sessions.clamp(1, MAX_WORKER_THREADS);
        let engine = Arc::new(Engine {
            config,
            pool: Pool::new(MAX_QUEUED_PER_KEY),
            parked: Mutex::new(HashMap::new()),
            stream_seq: AtomicU64::new(0),
        });
        let mut started = 0usize;
        for index in 0..workers {
            let worker_engine = Arc::clone(&engine);
            let spawned = std::thread::Builder::new()
                .name(format!("tg-session-{index}"))
                .spawn(move || {
                    let runtime = match tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        Ok(runtime) => runtime,
                        Err(error) => {
                            tracing::error!(error = %error, "Telegram session worker runtime could not start");
                            return;
                        }
                    };
                    worker_loop(&worker_engine.pool, |key, job| {
                        worker_engine.process(&runtime, key, job);
                    });
                });
            match spawned {
                Ok(_) => started += 1,
                Err(error) => {
                    tracing::error!(error = %error, "Telegram session worker thread could not start");
                }
            }
        }
        if started == 0 {
            tracing::error!(
                "no Telegram session worker is running; admitted messages will not be answered"
            );
        }

        let (stop_tx, stop_rx) = std_mpsc::channel::<()>();
        let sweeper_engine = Arc::downgrade(&engine);
        if let Err(error) = std::thread::Builder::new()
            .name("tg-session-sweeper".to_owned())
            .spawn(move || run_sweeper(&sweeper_engine, &stop_rx))
        {
            tracing::error!(error = %error, "Telegram approval sweeper could not start");
        }
        Self {
            engine,
            _sweeper_stop: stop_tx,
        }
    }

    /// Reiht eine zugelassene Nachricht in die FIFO ihres Chats ein. Blockiert
    /// nie; bei voller Warteschlange wird die Nachricht verworfen (geloggt).
    pub fn dispatch(&self, key: SessionKey, event: InboundEvent) {
        let channel = key.channel.clone();
        let peer = key.peer.clone();
        if self
            .engine
            .pool
            .with(|state| state.enqueue(key, Job::Message(event)))
            .is_err()
        {
            tracing::warn!(channel = %channel, peer = %peer, "Telegram session queue full or closed; message dropped");
        }
    }

    /// Senke für Turn-Freigaben (`"turn:"`-Callbacks) zur Übergabe an den
    /// Callback-Worker.
    pub fn turn_approvals(&self) -> Arc<dyn TurnApprovalSink> {
        Arc::new(TurnApprovals {
            engine: Arc::downgrade(&self.engine),
        })
    }
}

impl Drop for TelegramSessionDispatcher {
    fn drop(&mut self) {
        self.engine.pool.close();
    }
}

fn run_sweeper(engine: &Weak<Engine>, stop: &std_mpsc::Receiver<()>) {
    loop {
        match stop.recv_timeout(SWEEP_INTERVAL) {
            Err(std_mpsc::RecvTimeoutError::Timeout) => {
                let Some(engine) = engine.upgrade() else {
                    return;
                };
                engine.sweep(Timestamp::now());
            }
            Ok(()) | Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// [`TurnApprovalSink`] des Dispatchers.
///
/// `resolve` prüft, dass `actor` der gebundene Entscheider ist, und stellt
/// die Wiederaufnahme vorn in die FIFO des Chats; die eigentliche
/// Fortsetzung (inkl. Schließen der Freigabe-Nachricht) läuft asynchron auf
/// einem Worker. Der Rückgabetext ist für den Chat bzw. die Callback-Antwort
/// gedacht.
struct TurnApprovals {
    engine: Weak<Engine>,
}

impl TurnApprovalSink for TurnApprovals {
    fn resolve(&self, request_id: &str, approve: bool, actor: &PeerId) -> Result<String, String> {
        match self.engine.upgrade() {
            Some(engine) => engine.resolve(request_id, approve, actor),
            None => Err("Diese Freigabe ist nicht mehr offen.".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use harw_channel::{ChannelId, TenantId};

    fn key(peer: &str) -> SessionKey {
        SessionKey::new(
            TenantId::from_str("tenant"),
            ChannelId::from_str("tg"),
            PeerId::from_str(peer),
            None,
        )
    }

    #[test]
    fn scheduler_runs_one_job_per_key_in_fifo_order() -> TestResult {
        let mut scheduler = Scheduler::new(8);
        let a = key("1");
        let b = key("2");
        scheduler
            .enqueue(a.clone(), "a1")
            .map_err(|_| TestError::Missing("a1"))?;
        scheduler
            .enqueue(a.clone(), "a2")
            .map_err(|_| TestError::Missing("a2"))?;
        scheduler
            .enqueue(b.clone(), "b1")
            .map_err(|_| TestError::Missing("b1"))?;

        let first = scheduler.take_next().ok_or(TestError::Missing("first"))?;
        let second = scheduler.take_next().ok_or(TestError::Missing("second"))?;
        assert_eq!((first.0.clone(), first.1), (a.clone(), "a1"));
        assert_eq!((second.0.clone(), second.1), (b.clone(), "b1"));
        // a läuft noch: a2 darf nicht parallel starten.
        assert!(scheduler.take_next().is_none());
        scheduler.finish(&a);
        let third = scheduler.take_next().ok_or(TestError::Missing("third"))?;
        assert_eq!((third.0.clone(), third.1), (a.clone(), "a2"));
        scheduler.finish(&a);
        scheduler.finish(&b);
        assert!(scheduler.take_next().is_none());
        assert_eq!(scheduler.lane_count(), 0);
        Ok(())
    }

    #[test]
    fn blocked_key_waits_until_resume_is_enqueued_in_front() -> TestResult {
        let mut scheduler = Scheduler::new(8);
        let a = key("1");
        scheduler
            .enqueue(a.clone(), "turn")
            .map_err(|_| TestError::Missing("turn"))?;
        let (_, job) = scheduler.take_next().ok_or(TestError::Missing("turn"))?;
        assert_eq!(job, "turn");
        scheduler.block(&a);
        scheduler
            .enqueue(a.clone(), "later")
            .map_err(|_| TestError::Missing("later"))?;
        scheduler.finish(&a);
        assert!(scheduler.take_next().is_none(), "blocked lane must not run");

        scheduler.enqueue_front_and_unblock(a.clone(), "resume");
        let (_, job) = scheduler.take_next().ok_or(TestError::Missing("resume"))?;
        assert_eq!(job, "resume");
        scheduler.finish(&a);
        let (_, job) = scheduler.take_next().ok_or(TestError::Missing("later"))?;
        assert_eq!(job, "later");
        scheduler.finish(&a);
        assert_eq!(scheduler.lane_count(), 0);
        Ok(())
    }

    #[test]
    fn resume_during_running_job_is_not_lost() -> TestResult {
        let mut scheduler = Scheduler::new(8);
        let a = key("1");
        scheduler
            .enqueue(a.clone(), "turn")
            .map_err(|_| TestError::Missing("turn"))?;
        let _ = scheduler.take_next().ok_or(TestError::Missing("turn"))?;
        scheduler.block(&a);
        // Entscheidung trifft ein, bevor der parkende Job beendet ist.
        scheduler.enqueue_front_and_unblock(a.clone(), "resume");
        assert!(scheduler.take_next().is_none(), "lane still running");
        scheduler.finish(&a);
        let (_, job) = scheduler.take_next().ok_or(TestError::Missing("resume"))?;
        assert_eq!(job, "resume");
        Ok(())
    }

    #[test]
    fn scheduler_bounds_queue_per_key() {
        let mut scheduler = Scheduler::new(2);
        let a = key("1");
        assert!(scheduler.enqueue(a.clone(), 1).is_ok());
        assert!(scheduler.enqueue(a.clone(), 2).is_ok());
        assert_eq!(scheduler.enqueue(a.clone(), 3), Err(3));
        assert!(scheduler.enqueue(key("2"), 4).is_ok());
    }

    /// Testjob: Nummer, optional ein Signal senden, optional auf eines warten.
    type TestJob = (
        u32,
        Option<std_mpsc::Sender<()>>,
        Option<Arc<Mutex<std_mpsc::Receiver<()>>>>,
    );

    #[test]
    fn worker_pool_keeps_fifo_per_key_and_runs_keys_in_parallel() -> TestResult {
        let pool: Arc<Pool<TestJob>> = Arc::new(Pool::new(64));
        let seen: Arc<Mutex<Vec<(String, u32)>>> = Arc::new(Mutex::new(Vec::new()));
        let (signal_tx, signal_rx) = std_mpsc::channel::<()>();
        let signal_rx = Arc::new(Mutex::new(signal_rx));

        // Schlüssel A wartet in Job 0 auf ein Signal, das erst Job 0 von B
        // sendet: das gelingt nur, wenn beide Schlüssel parallel laufen.
        pool.with(|state| -> Result<(), ()> {
            state
                .enqueue(key("A"), (0, None, Some(Arc::clone(&signal_rx))))
                .map_err(|_| ())?;
            for n in 1..5 {
                state.enqueue(key("A"), (n, None, None)).map_err(|_| ())?;
            }
            state
                .enqueue(key("B"), (0, Some(signal_tx.clone()), None))
                .map_err(|_| ())?;
            for n in 1..5 {
                state.enqueue(key("B"), (n, None, None)).map_err(|_| ())?;
            }
            Ok(())
        })
        .map_err(|()| TestError::Missing("enqueue"))?;
        drop(signal_tx);

        let mut handles = Vec::new();
        for _ in 0..2 {
            let pool = Arc::clone(&pool);
            let seen = Arc::clone(&seen);
            handles.push(std::thread::spawn(move || {
                worker_loop(&pool, |key, (n, send, wait)| {
                    if let Some(wait) = wait {
                        let received = wait
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .recv_timeout(Duration::from_secs(5));
                        if received.is_err() {
                            return;
                        }
                    }
                    if let Some(send) = send {
                        let _ = send.send(());
                    }
                    seen.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push((key.peer.as_str().to_owned(), n));
                });
            }));
        }

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let done = seen.lock().unwrap_or_else(PoisonError::into_inner).len();
            if done == 10 || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        pool.close();
        for handle in handles {
            handle
                .join()
                .map_err(|_| TestError::Unexpected("worker panicked".to_owned()))?;
        }
        let seen = seen.lock().unwrap_or_else(PoisonError::into_inner).clone();
        assert_eq!(
            seen.len(),
            10,
            "all jobs ran (parallel across keys): {seen:?}"
        );
        for peer in ["A", "B"] {
            let order: Vec<u32> = seen
                .iter()
                .filter(|(p, _)| p == peer)
                .map(|(_, n)| *n)
                .collect();
            assert_eq!(order, vec![0, 1, 2, 3, 4], "FIFO for {peer}");
        }
        Ok(())
    }

    #[test]
    fn stream_state_throttles_and_separates_rounds() {
        let start = Instant::now();
        let mut stream = StreamState::new("fallback".to_owned());
        assert_eq!(stream.stream_key(), "fallback");
        stream.last_push = start;
        let turn_id = harw_types::TurnId::from_str("t-1");
        let early = stream.absorb(
            TurnEvent::AssistantDelta {
                turn_id: turn_id.clone(),
                text: "Hallo".to_owned(),
            },
            start + Duration::from_millis(200),
        );
        assert!(!early, "updates are throttled to 1.5 s");
        assert_eq!(stream.stream_key(), "t-1");
        let _ = stream.absorb(
            TurnEvent::ToolCallRequested {
                turn_id: turn_id.clone(),
                call_id: harw_types::ToolCallId::from_str("c"),
                tool_name: "x".to_owned(),
                arguments: serde_json::Value::Null,
            },
            start,
        );
        let due = stream.absorb(
            TurnEvent::AssistantDelta {
                turn_id,
                text: "Welt".to_owned(),
            },
            start + STREAM_UPDATE_INTERVAL,
        );
        assert!(due);
        assert_eq!(stream.text, "Hallo\n\nWelt");
    }

    #[test]
    fn request_ids_are_normalized_to_turn_prefix() {
        assert_eq!(normalize_turn_request_id("abc"), "turn:abc");
        assert_eq!(normalize_turn_request_id("turn:abc"), "turn:abc");
        let prompt = turn_approval_prompt("turn:abc", "Freigabe?");
        assert_eq!(prompt.request_id, "turn:abc");
        let decisions: Vec<&str> = prompt.actions.iter().map(|a| a.decision.as_str()).collect();
        assert_eq!(decisions, vec!["approve", "deny"]);
    }

    #[test]
    fn user_text_combines_preamble_and_message() -> TestResult {
        assert_eq!(compose_user_text(None, None), None);
        assert_eq!(compose_user_text(Some("  "), None), None);
        assert_eq!(compose_user_text(Some(" hi "), None).as_deref(), Some("hi"));

        let rejected_only = IngestReport {
            rejected: vec!["x: Datei ist leer".to_owned()],
            ..IngestReport::default()
        };
        assert_eq!(compose_user_text(None, Some(&rejected_only)), None);

        let report = IngestReport {
            inline_text: vec![("n.txt".to_owned(), "inhalt".to_owned())],
            ..IngestReport::default()
        };
        let attachment_only = compose_user_text(None, Some(&report))
            .ok_or(TestError::Missing("attachment-only text"))?;
        assert!(attachment_only.starts_with("[Telegram-Anhänge"));
        assert!(attachment_only.ends_with("\nBitte berücksichtige die angehängten Dateien."));
        let with_text = compose_user_text(Some("Fasse zusammen"), Some(&report))
            .ok_or(TestError::Missing("text"))?;
        assert_eq!(
            with_text,
            format!("{}\nFasse zusammen", report.prompt_preamble())
        );
        Ok(())
    }
}
