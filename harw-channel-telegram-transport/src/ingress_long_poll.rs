//! Dedicated, restart-safe Telegram long-poll ingress.
//!
//! This runner owns a single OS thread and a Tokio current-thread runtime. It
//! intentionally never enters, borrows, or bridges to a gateway runtime: the
//! only hand-off is the bounded synchronous ingress channel.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use harw_channel::{ChannelId, InboundEvent};

use crate::{
    BotInfo, DedupWindow, TelegramClient, TelegramOffsetStore, TransportResult,
    error::TelegramTransportError,
    mapping::{RawUpdate, map_update},
};

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const DEFAULT_ALLOWED_UPDATES: [&str; 2] = ["message", "edited_message"];

/// Cooperative shutdown signal for a long-poll runner.
///
/// Shutdown is observed before each request and each update in a returned
/// batch. A request already in flight is allowed to complete so its response
/// can remain subject to the normal durable-offset policy.
#[derive(Clone, Default)]
pub struct LongPollShutdown(Arc<AtomicBool>);

impl LongPollShutdown {
    /// Requests that the runner stop before its next unit of work.
    pub fn request_shutdown(&self) {
        self.0.store(true, Ordering::Release);
    }

    fn is_requested(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Inputs owned by one dedicated Telegram long-poll runner.
///
/// The offset store must be profile- and channel-scoped by the caller. The
/// supplied sender must originate from [`std::sync::mpsc::sync_channel`], so a
/// saturated downstream boundary cannot turn into unbounded memory growth.
pub struct LongPollConfig {
    client: TelegramClient,
    channel_id: String,
    bot: BotInfo,
    offset_store: TelegramOffsetStore,
    sender: SyncSender<InboundEvent>,
    shutdown: LongPollShutdown,
    dedup: DedupWindow,
    timeout_secs: u64,
    allowed_updates: Vec<String>,
}

impl LongPollConfig {
    /// Creates a long-poll runner configuration from already-resolved values.
    #[must_use]
    pub fn new(
        client: TelegramClient,
        channel_id: impl Into<String>,
        bot: BotInfo,
        offset_store: TelegramOffsetStore,
        sender: SyncSender<InboundEvent>,
        shutdown: LongPollShutdown,
    ) -> Self {
        Self {
            client,
            channel_id: channel_id.into(),
            bot,
            offset_store,
            sender,
            shutdown,
            dedup: DedupWindow::new(1_024),
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            allowed_updates: DEFAULT_ALLOWED_UPDATES
                .iter()
                .map(|kind| (*kind).to_owned())
                .collect(),
        }
    }

    /// Replaces the process-local deduplication window.
    #[must_use]
    pub fn with_dedup_window(mut self, dedup: DedupWindow) -> Self {
        self.dedup = dedup;
        self
    }

    /// Changes the Telegram request timeout. Zero is accepted for tests and
    /// for callers that deliberately prefer immediate polling.
    #[must_use]
    pub fn with_timeout_secs(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = timeout_secs;
        self
    }
}

/// Starts a dedicated long-poll thread with its own current-thread Tokio runtime.
///
/// The returned handle yields any client, mapping, sink, or offset persistence
/// failure as a typed transport error. Errors are also logged through `tracing`
/// using the transport error's redacted display form.
pub fn spawn_long_poll_thread(
    config: LongPollConfig,
) -> TransportResult<JoinHandle<TransportResult<()>>> {
    thread::Builder::new()
        .name("harw-telegram-long-poll".to_owned())
        .spawn(move || {
            let result = run_thread(config);
            if let Err(error) = &result {
                tracing::error!(error = %error, "Telegram long-poll runner stopped");
            }
            result
        })
        .map_err(TelegramTransportError::from)
}

fn run_thread(config: LongPollConfig) -> TransportResult<()> {
    tokio::runtime::Builder::new_current_thread()
        // `TelegramClient` uses Reqwest/Hyper TCP sockets. `enable_time` alone
        // lets retries sleep but panics on the first socket operation.
        .enable_io()
        .enable_time()
        .build()
        .map_err(TelegramTransportError::from)?
        .block_on(run_long_poll(config))
}

async fn run_long_poll(config: LongPollConfig) -> TransportResult<()> {
    let LongPollConfig {
        client,
        channel_id,
        bot,
        offset_store,
        sender,
        shutdown,
        dedup,
        timeout_secs,
        allowed_updates,
    } = config;
    let allowed_updates = allowed_updates
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let mut persisted_offset = restart_offset(offset_store.load()?);
    // Zaehlt nicht dekodierbare ("Poison"-)Updates seit Threadstart. Rein
    // diagnostisch (Log-Feld), nicht durabel — siehe `decode_update`s Doku.
    let mut poison_update_count: u64 = 0;

    'poll: while !shutdown.is_requested() {
        let updates = client
            .get_updates(persisted_offset, timeout_secs, &allowed_updates)
            .await?;

        for value in updates {
            if shutdown.is_requested() {
                break 'poll;
            }

            let (update_id_hint, decoded) = decode_update(value);
            let update = match decoded {
                Ok(update) => update,
                Err(error) => {
                    // Ein einzelnes nicht dekodierbares Update darf diesen
                    // Thread nicht dauerhaft beenden (S6 „Poison-Update"):
                    // zaehlen, loggen (ohne Rohinhalt), und — sofern die
                    // `update_id` aus dem Rohwert noch lesbar war — den
                    // Offset trotzdem darueber hinweg vorruecken, damit
                    // Telegram dasselbe kaputte Update nicht endlos erneut
                    // zustellt. Ohne lesbare `update_id` bleibt der Offset
                    // unveraendert; dieses eine Update wird dann beim naechsten
                    // Poll erneut versucht (kann aber niemals den Thread
                    // beenden).
                    poison_update_count += 1;
                    tracing::warn!(
                        error = %error,
                        update_id = ?update_id_hint,
                        poison_update_count,
                        "Telegram long-poll: skipping a non-decodable update"
                    );
                    if let Some(update_id) = update_id_hint {
                        advance_offset_past_poison_update(
                            &offset_store,
                            &mut persisted_offset,
                            update_id,
                        )?;
                    }
                    continue;
                }
            };

            let safely_processed = match map_update(&update, bot.id, bot.username.as_deref()) {
                Some(mut event) if dedup.claim_update(update.update_id) => {
                    event.channel = ChannelId::from_str(channel_id.clone());
                    match send_event_with_backoff(&sender, event, &shutdown).await? {
                        SendOutcome::Sent => true,
                        SendOutcome::ShutdownRequested => break 'poll,
                    }
                }
                Some(_) => true,
                None => true,
            };

            if safely_processed {
                let next_offset =
                    next_offset_after_safe_processing(persisted_offset, update.update_id)?;
                offset_store.store(next_offset)?;
                persisted_offset = Some(next_offset);
            }
        }
    }

    Ok(())
}

/// Decodes one raw Telegram update, also returning its `update_id` when that
/// single top-level field could still be read even if the rest of the
/// payload failed schema validation. Telegram always sends `update_id` as a
/// top-level integer, so this succeeds far more often than a full
/// `RawUpdate` decode and lets the poison-update path in [`run_long_poll`]
/// advance the durable offset past an update it otherwise cannot process.
fn decode_update(value: serde_json::Value) -> (Option<i64>, TransportResult<RawUpdate>) {
    let update_id_hint = value.get("update_id").and_then(serde_json::Value::as_i64);
    let decoded =
        serde_json::from_value(value).map_err(|error| TelegramTransportError::MalformedUpdate {
            update_id: update_id_hint,
            reason: error.to_string(),
        });
    (update_id_hint, decoded)
}

/// Advances and persists the durable offset past a poison update whose
/// `update_id` could still be read from the raw payload. A malformed id
/// (negative, or one that cannot produce a valid next offset) is logged and
/// left un-advanced rather than propagated: [`next_offset_after_safe_processing`]'s
/// own validation already guards the durable offset file against that case,
/// and this path must never turn a poison update into a thread-ending error.
fn advance_offset_past_poison_update(
    offset_store: &TelegramOffsetStore,
    persisted_offset: &mut Option<i64>,
    update_id: i64,
) -> TransportResult<()> {
    match next_offset_after_safe_processing(*persisted_offset, update_id) {
        Ok(next_offset) => {
            offset_store.store(next_offset)?;
            *persisted_offset = Some(next_offset);
            Ok(())
        }
        Err(error) => {
            tracing::warn!(
                error = %error,
                "Telegram long-poll: poison update id could not advance the durable offset"
            );
            Ok(())
        }
    }
}

/// Outcome of [`send_event_with_backoff`]: either the event reached the
/// bounded admission channel, or the caller asked to stop while a full sink
/// was being retried.
///
/// `Debug` ist abgeleitet, damit Tests den unerwarteten Erfolgsfall benennen
/// können (`expect_err`) und Log-Felder den Ausgang zeigen dürfen.
#[derive(Debug)]
enum SendOutcome {
    Sent,
    ShutdownRequested,
}

/// Sends one normalized event to the bounded admission channel, backing off
/// and retrying while the sink is momentarily full instead of ending the
/// long-poll thread (S6 „stirbt bei vollem Sink"). A disconnected receiver
/// has no possible recovery and is still reported as a fatal transport
/// error, so the caller (and its gateway-level supervision/restart) can tell
/// the two apart.
async fn send_event_with_backoff(
    sender: &SyncSender<InboundEvent>,
    mut event: InboundEvent,
    shutdown: &LongPollShutdown,
) -> TransportResult<SendOutcome> {
    const INITIAL_BACKOFF: Duration = Duration::from_millis(50);
    const MAX_BACKOFF: Duration = Duration::from_secs(5);
    let mut backoff = INITIAL_BACKOFF;

    loop {
        match sender.try_send(event) {
            Ok(()) => return Ok(SendOutcome::Sent),
            Err(TrySendError::Disconnected(_)) => {
                return Err(TelegramTransportError::Io(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "Telegram long-poll ingress sink is disconnected",
                )));
            }
            Err(TrySendError::Full(returned_event)) => {
                if shutdown.is_requested() {
                    return Ok(SendOutcome::ShutdownRequested);
                }
                event = returned_event;
                tracing::warn!(
                    backoff_ms = u64::try_from(backoff.as_millis()).unwrap_or(u64::MAX),
                    "Telegram long-poll ingress sink is full, backing off"
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(MAX_BACKOFF);
            }
        }
    }
}

fn restart_offset(stored_offset: Option<i64>) -> Option<i64> {
    stored_offset
}

fn next_offset_after_safe_processing(
    persisted_offset: Option<i64>,
    update_id: i64,
) -> TransportResult<i64> {
    if update_id < 0 {
        return Err(TelegramTransportError::MalformedUpdate {
            update_id: Some(update_id),
            reason: "Telegram update id must be non-negative".to_owned(),
        });
    }

    let candidate =
        update_id
            .checked_add(1)
            .ok_or_else(|| TelegramTransportError::MalformedUpdate {
                update_id: Some(update_id),
                reason: "Telegram update id cannot produce a valid next offset".to_owned(),
            })?;
    Ok(persisted_offset.map_or(candidate, |offset| offset.max(candidate)))
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use jiff::Timestamp;

    use super::{
        LongPollShutdown, SendOutcome, advance_offset_past_poison_update, decode_update,
        next_offset_after_safe_processing, restart_offset, send_event_with_backoff,
    };
    use crate::TelegramOffsetStore;
    use harw_channel::{ChannelId, InboundEvent, PeerId};

    #[test]
    fn safe_processing_advances_to_the_next_update_id() {
        assert_eq!(next_offset_after_safe_processing(None, 41).unwrap(), 42);
        assert_eq!(next_offset_after_safe_processing(Some(42), 42).unwrap(), 43);
    }

    #[test]
    fn offset_never_regresses_for_an_old_response_item() {
        assert_eq!(next_offset_after_safe_processing(Some(99), 12).unwrap(), 99);
    }

    #[test]
    fn restart_uses_the_durable_next_offset_not_the_last_update_id() {
        assert_eq!(restart_offset(None), None);
        assert_eq!(restart_offset(Some(43)), Some(43));
    }

    #[test]
    fn impossible_update_ids_do_not_create_a_durable_offset() {
        assert!(next_offset_after_safe_processing(None, -1).is_err());
        assert!(next_offset_after_safe_processing(None, i64::MAX).is_err());
    }

    fn sample_event() -> InboundEvent {
        InboundEvent {
            channel: ChannelId::from_str("telegram:ops"),
            peer: PeerId::from_str("100"),
            thread: None,
            sender: None,
            text: Some("hello".to_owned()),
            mentioned: false,
            attachments: Vec::new(),
            raw_event_id: Some("1".to_owned()),
            received_at: Timestamp::now(),
        }
    }

    #[test]
    fn decode_update_reads_the_update_id_hint_from_an_otherwise_malformed_payload() {
        // `RawUpdate::message`/`edited_message`/`callback_query` sind alle
        // `#[serde(default)] Option<_>`, weil Telegram-Updatearten, die dieses
        // Schema nicht modelliert (z. B. `poll`, `chat_member`), legitim
        // keines dieser Felder setzen. Ein bloß unbekanntes Zusatzfeld ist
        // also kein Dekodierfehler mehr. Um trotzdem eine wirklich kaputte
        // Nutzlast zu erzeugen, bekommt `message` hier einen falschen Typ
        // (String statt Objekt), was `RawMessage`s Deserialisierung sicher
        // scheitern lässt. `update_id` bleibt als rohes JSON-Feld lesbar
        // (Poison-Update, S6).
        let poison = serde_json::json!({ "update_id": 77, "message": "not-a-message-object" });

        let (update_id_hint, decoded) = decode_update(poison);

        assert_eq!(update_id_hint, Some(77));
        let error = decoded.expect_err("malformed shape must fail RawUpdate decoding");
        assert!(!error.to_string().contains("not-a-message-object"));
    }

    #[test]
    fn decode_update_hint_is_none_when_update_id_itself_is_unreadable() {
        let poison = serde_json::json!({ "not_even_an_update_id": true });

        let (update_id_hint, decoded) = decode_update(poison);

        assert_eq!(update_id_hint, None);
        assert!(decoded.is_err());
    }

    #[test]
    fn poison_update_advances_the_durable_offset_so_the_next_update_gets_processed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let offset_store = TelegramOffsetStore::new(dir.path());
        let mut persisted_offset = None;

        // Poison-Update mit lesbarer `update_id=41`.
        advance_offset_past_poison_update(&offset_store, &mut persisted_offset, 41)
            .expect("advancing past a poison update must not fail");

        assert_eq!(persisted_offset, Some(42));
        assert_eq!(offset_store.load().unwrap(), Some(42));

        // Das nächste, gesund dekodierte Update (id=42) wird ganz normal
        // weiterverarbeitet: der Offset rückt konsistent weiter vor.
        let next = next_offset_after_safe_processing(persisted_offset, 42).unwrap();
        assert_eq!(next, 43);
    }

    #[test]
    fn poison_update_with_an_invalid_id_leaves_the_offset_unadvanced() {
        let dir = tempfile::tempdir().expect("tempdir");
        let offset_store = TelegramOffsetStore::new(dir.path());
        let mut persisted_offset = Some(10);

        advance_offset_past_poison_update(&offset_store, &mut persisted_offset, -1)
            .expect("an unusable poison update id must not become a thread-ending error");

        assert_eq!(persisted_offset, Some(10));
        assert_eq!(offset_store.load().unwrap(), None);
    }

    #[tokio::test]
    async fn full_sink_backs_off_and_retries_instead_of_ending_the_runner() {
        let (tx, rx) = mpsc::sync_channel::<InboundEvent>(0);
        let shutdown = LongPollShutdown::default();

        let receiver = std::thread::spawn(move || rx.recv().expect("event eventually arrives"));

        let outcome = send_event_with_backoff(&tx, sample_event(), &shutdown)
            .await
            .expect("a momentarily full sink must not be a fatal error");
        assert!(matches!(outcome, SendOutcome::Sent));

        let received = receiver.join().expect("receiver thread panicked");
        assert_eq!(received.peer.as_str(), "100");
    }

    #[tokio::test]
    async fn disconnected_sink_is_reported_as_a_fatal_error() {
        let (tx, rx) = mpsc::sync_channel::<InboundEvent>(1);
        drop(rx);
        let shutdown = LongPollShutdown::default();

        let error = send_event_with_backoff(&tx, sample_event(), &shutdown)
            .await
            .expect_err("a disconnected receiver has no recovery");
        assert!(error.to_string().contains("local I/O failure"));
    }

    #[tokio::test]
    async fn shutdown_request_stops_a_full_sink_retry_promptly() {
        let (tx, _rx) = mpsc::sync_channel::<InboundEvent>(0);
        let shutdown = LongPollShutdown::default();
        shutdown.request_shutdown();

        let outcome = send_event_with_backoff(&tx, sample_event(), &shutdown)
            .await
            .expect("shutdown must not be reported as an error");
        assert!(matches!(outcome, SendOutcome::ShutdownRequested));
    }
}
