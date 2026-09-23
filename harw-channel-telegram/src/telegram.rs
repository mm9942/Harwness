use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};

use jiff::{SignedDuration, Timestamp};

use harw_channel::{
    Admission, AttachmentSupport, ChannelAdapter, ChannelCapabilities, ChannelError, ChannelSendOp,
    DeferralReason, InboundEvent, InlineAction, MarkdownSupport, OutboundContent, PairingRecord,
    PairingStore, RejectionReason, SessionKey, ThreadRef, ThreadSupport,
};
use harw_types::{PeerId, TenantId};

use crate::approval_tokens::ApprovalCallbackContext;
use crate::{
    ApprovalTokenStore, PendingApproval, TelegramChannelConfig, TelegramChannelError,
    TelegramChannelResult, TelegramSandbox, TopicMode,
};

const UNPAIRED_TENANT: &str = "harw:unpaired";
const INGRESS_UNAVAILABLE_MESSAGE: &str = "Telegram ingress is not configured, already running, or its sink/lock is gone";
const CHANNEL_MISMATCH_REASON: &str = "inbound event channel does not match Telegram binding";
/// Sliding-window width for `max_updates_per_peer_per_min` (§3.5) and for the
/// "at most one throttle notice per window" de-duplication.
const RATE_LIMIT_WINDOW: SignedDuration = SignedDuration::from_secs(60);
/// The single throttled reply sent at most once per rate-limit window. Kept
/// intentionally short and free of Telegram MarkdownV2 reserved characters so
/// no downstream escaping decision is required for it.
const THROTTLE_NOTICE_TEXT: &str = "Zu schnell — bitte kurz warten und erneut senden.";
/// Antwort nach erfolgreicher In-Channel-Einlösung eines `/pair <Code>`.
/// Enthält bewusst weder Code noch Tenant.
const PAIRING_SUCCESS_TEXT: &str = "Pairing erfolgreich — du kannst jetzt schreiben";
/// Einheitliche Antwort für *jeden* fehlgeschlagenen Einlöseversuch
/// (ungültig, abgelaufen, bereits verwendet, Speicherfehler). Unterscheidet
/// die Ursachen absichtlich nicht, damit ein Absender den Code-Raum nicht
/// über unterschiedliche Antworten abtasten kann.
const PAIRING_FAILURE_TEXT: &str =
    "Pairing fehlgeschlagen — Code ungültig, abgelaufen oder bereits verwendet";
/// Crockford-Base32-Alphabet der Pairing-Codes (`harw_channel::PairingCode`).
const PAIRING_CODE_ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// One per-rate-limit-key sliding-window counter (§3.5).
///
/// `last_counted_update` makes [`TelegramChannel::record_inbound_rate`]
/// idempotent for a repeated call with the *same* event (the admission
/// pipeline may legitimately re-check an already-admitted event), so a
/// caller invoking it twice for one update never consumes two slots of the
/// peer's budget.
#[derive(Debug, Clone)]
struct RateWindow {
    window_start: Timestamp,
    count: u32,
    last_counted_update: Option<String>,
}

/// A single outbound reply produced by the rate-limit gate itself (§3.5's
/// "single throttled reply per window at most"), decoupled from the ordinary
/// admitted-event runtime path since a rejected event never reaches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThrottleNotice {
    /// The peer/session-partition identity the notice must be delivered to
    /// (mirrors `InboundEvent::peer`, §3.3).
    pub peer: PeerId,
    /// The originating thread, if any, so a forum-topic reply lands in the
    /// same topic as the throttled traffic.
    pub thread: Option<ThreadRef>,
    /// The harness-native content to render; always a plain [`OutboundContent::Message`].
    pub content: OutboundContent,
}

/// Ergebnis einer In-Channel-Einlösung von `/pair <Code>` (§3.2).
///
/// Trägt niemals den Code selbst: weder Notice noch Outcome dürfen den
/// einmaligen Geheimwert in Logs oder Antworten tragen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingOutcome {
    /// Der Code wurde eingelöst; der Peer ist nun durable an `tenant` gebunden.
    Paired {
        /// Der Tenant, an den der Peer gebunden wurde.
        tenant: TenantId,
    },
    /// Einlösung fehlgeschlagen (ungültig, abgelaufen, bereits eingelöst,
    /// umkämpft oder Speicherfehler) — bewusst ohne Ursache.
    Failed,
}

/// Antwort der Adapter-Perimeter auf einen `/pair <Code>`-DM eines
/// ungepairten Peers, analog zu [`ThrottleNotice`]: der Adapter entscheidet
/// und erzeugt nur die Notice, die Zustellung übernimmt die Transport-
/// Komposition (z. B. `harw gateway`) über [`TelegramChannel::with_pairing_sink`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingNotice {
    /// Der DM-Peer, an den die Antwort geht (entspricht `InboundEvent::peer`).
    pub peer: PeerId,
    /// Der ursprüngliche Thread, falls vorhanden.
    pub thread: Option<ThreadRef>,
    /// Das Ergebnis der Einlösung, ohne Code.
    pub outcome: PairingOutcome,
    /// Der zu rendernde harness-native Inhalt; immer eine schlichte
    /// [`OutboundContent::Message`] ohne Code.
    pub content: OutboundContent,
}

/// Pure Telegram binding; HTTP polling/webhook delivery is intentionally wired
/// later behind this policy boundary.
#[derive(Clone)]
pub struct TelegramChannel {
    config: TelegramChannelConfig,
    sandbox: TelegramSandbox,
    pairing: Arc<PairingStore>,
    approval_tokens: Arc<ApprovalTokenStore>,
    /// Normalized events supplied by the Telegram transport boundary. The
    /// adapter never creates a transport or resolves credentials itself.
    /// A slot rather than a permanently borrowed receiver: an ingress runner
    /// takes exclusive ownership before blocking in `recv`, then restores it
    /// when it stops. The mutex therefore protects handoff only, never a
    /// blocking receive.
    ingress: Option<Arc<Mutex<Option<Receiver<InboundEvent>>>>>,
    /// Counts events dropped by the pinning gate in [`Self::forward_ingress_event`]
    /// before any durable replay claim or journal write happens (F-040/S5).
    /// In-memory only, without sender/message content, so a flood of
    /// unpinned traffic stays observable without becoming persistence.
    rejected_unpinned: Arc<AtomicU64>,
    /// In-memory, per-rate-limit-key sliding-window counters enforcing
    /// `max_updates_per_peer_per_min` (§3.5). Deliberately process-local: a
    /// restart resetting the window is an acceptable trade-off for a
    /// perimeter throttle, unlike pairing/replay state which must survive
    /// restarts.
    rate_limits: Arc<Mutex<HashMap<String, RateWindow>>>,
    /// Last time a [`ThrottleNotice`] was emitted per rate-limit key, so at
    /// most one "too fast" reply goes out per window even though many
    /// updates from a flooding peer are rejected in that same window.
    last_throttle_notice: Arc<Mutex<HashMap<String, Timestamp>>>,
    /// Optional outbound handoff for [`ThrottleNotice`]s. `None` (the
    /// default) means rate-limited traffic is dropped silently; a transport
    /// composition (e.g. `harw gateway`) opts in via
    /// [`Self::with_throttle_sink`].
    throttle_sink: Option<Arc<Mutex<Sender<ThrottleNotice>>>>,
    /// Optionale Übergabe für [`PairingNotice`]s nach einer In-Channel-
    /// Einlösung von `/pair <Code>`. Die Einlösung selbst passiert auch ohne
    /// Sink; `None` bedeutet nur, dass der Peer keine Rückmeldung bekommt.
    pairing_sink: Option<Arc<Mutex<Sender<PairingNotice>>>>,
}

impl std::fmt::Debug for TelegramChannel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TelegramChannel")
            .field("config", &self.config)
            .field("sandbox", &self.sandbox)
            .field("pairing", &self.pairing)
            .field("ingress", &self.ingress)
            .field(
                "rejected_unpinned_sender_count",
                &self.rejected_unpinned.load(Ordering::Relaxed),
            )
            .field("throttle_sink_configured", &self.throttle_sink.is_some())
            .field("pairing_sink_configured", &self.pairing_sink.is_some())
            .finish_non_exhaustive()
    }
}

impl TelegramChannel {
    #[must_use]
    pub fn new(config: TelegramChannelConfig, pairing: Arc<PairingStore>) -> Self {
        Self {
            config,
            pairing,
            approval_tokens: Arc::new(ApprovalTokenStore::new()),
            sandbox: TelegramSandbox::reduced_default(),
            ingress: None,
            rejected_unpinned: Arc::new(AtomicU64::new(0)),
            rate_limits: Arc::new(Mutex::new(HashMap::new())),
            last_throttle_notice: Arc::new(Mutex::new(HashMap::new())),
            throttle_sink: None,
            pairing_sink: None,
        }
    }

    /// Connects a transport-owned normalized-event receiver to this adapter.
    ///
    /// The receiver is intentionally an input of the binding rather than an
    /// HTTP client hidden inside it: credential resolution and Telegram update
    /// normalization remain at the transport boundary, while this adapter owns
    /// durable replay, pairing, approval callbacks, session, and admission
    /// policy.
    #[must_use]
    pub fn with_ingress_receiver(
        config: TelegramChannelConfig,
        pairing: Arc<PairingStore>,
        ingress: Receiver<InboundEvent>,
    ) -> Self {
        Self {
            config,
            pairing,
            approval_tokens: Arc::new(ApprovalTokenStore::new()),
            sandbox: TelegramSandbox::reduced_default(),
            ingress: Some(Arc::new(Mutex::new(Some(ingress)))),
            rejected_unpinned: Arc::new(AtomicU64::new(0)),
            rate_limits: Arc::new(Mutex::new(HashMap::new())),
            last_throttle_notice: Arc::new(Mutex::new(HashMap::new())),
            throttle_sink: None,
            pairing_sink: None,
        }
    }

    /// Registers an outbound handoff for [`ThrottleNotice`]s (§3.5).
    ///
    /// A transport composition uses this to deliver the "you're sending too
    /// fast" reply; without it, rate-limited traffic is dropped silently
    /// (still rejected, just without a human-visible reply).
    #[must_use]
    pub fn with_throttle_sink(mut self, sink: Sender<ThrottleNotice>) -> Self {
        self.throttle_sink = Some(Arc::new(Mutex::new(sink)));
        self
    }

    /// Registriert eine Übergabe für [`PairingNotice`]s (§3.2).
    ///
    /// Die Transport-Komposition stellt darüber die Bestätigung bzw. die
    /// einheitliche Fehlermeldung einer In-Channel-Einlösung von
    /// `/pair <Code>` zu. Ohne Sink wird trotzdem eingelöst, nur still.
    #[must_use]
    pub fn with_pairing_sink(mut self, sink: Sender<PairingNotice>) -> Self {
        self.pairing_sink = Some(Arc::new(Mutex::new(sink)));
        self
    }

    #[must_use]
    pub fn config(&self) -> &TelegramChannelConfig {
        &self.config
    }

    /// Binds an opaque approval callback token to the exact Telegram message
    /// that rendered its button and the callback sender context.
    pub fn bind_approval_callback(&self, token: &str, context: ApprovalCallbackContext) -> bool {
        self.approval_tokens.bind(token, context)
    }

    /// Consumes an approval callback token only when its callback context
    /// exactly matches the context recorded for the rendered button.
    pub fn consume_approval_callback(
        &self,
        token: &str,
        context: &ApprovalCallbackContext,
    ) -> Option<PendingApproval> {
        self.approval_tokens.consume_bound(token, context)
    }

    /// Durably resolves the tenant a peer is paired to on this channel, or
    /// `None` if unpaired/revoked (§3.2). This is the sole tenant-resolution
    /// path; there is no in-memory binding table.
    pub fn resolve_tenant(&self, peer: &PeerId) -> TelegramChannelResult<Option<TenantId>> {
        self.pairing
            .lookup_binding(&self.config.channel_id, peer)
            .map_err(TelegramChannelError::from)
    }

    /// Atomically redeems a pairing code presented by `actor`, binding it to the
    /// code's tenant (§3.2). Single-use: a second redemption fails.
    pub fn redeem_pairing(
        &self,
        code: &str,
        actor: &PeerId,
        now: Timestamp,
    ) -> TelegramChannelResult<PairingRecord> {
        self.pairing
            .redeem_once(&self.config.channel_id, code, actor, now)
            .map_err(TelegramChannelError::from)
    }

    /// Channel-scoped replay gate: returns `true` the first time this Telegram
    /// `update_id` is seen, `false` for any retried delivery (§5). The ingress
    /// path requires this identifier before it calls the replay gate.
    pub fn claim_update(&self, event: &InboundEvent) -> TelegramChannelResult<bool> {
        match event.raw_event_id.as_deref() {
            Some(update_id) => self
                .pairing
                .claim_once(&event.channel, update_id, event.received_at)
                .map_err(TelegramChannelError::from),
            None => Ok(true),
        }
    }

    /// Whether a resolved tenant is a real pairing (not the unpaired sentinel).
    /// The single explicit guard that keeps `harw:unpaired` from ever reaching
    /// an `Admitted` verdict or admission/JobIntent downstream.
    fn is_paired(tenant: &TenantId) -> bool {
        tenant.as_str() != UNPAIRED_TENANT
    }

    /// Whether `event` carries an identified sender on the pinning allowlist.
    /// Pure and stateless (no I/O, no store access) so it can run before any
    /// durable work — this is the single pinning check shared by the
    /// pre-claim gate in [`Self::forward_ingress_event`] and [`Self::admit`].
    fn sender_pinned(&self, event: &InboundEvent) -> bool {
        event
            .sender
            .as_ref()
            .is_some_and(|sender| self.config.is_sender_identity_pinned(&sender.id))
    }

    /// Number of ingress events dropped by the pinning gate so far (F-040).
    /// Diagnostic only: process-local, resets on restart, never persisted.
    #[must_use]
    pub fn rejected_unpinned_sender_count(&self) -> u64 {
        self.rejected_unpinned.load(Ordering::Relaxed)
    }

    /// The sliding-window rate-limit key for `event` (§3.5).
    ///
    /// Prefers the *actual sender* (`SenderRef::id`) over `InboundEvent::peer`
    /// so the per-peer budget in a shared group session ("a single paired
    /// user hammering the bot") is scoped to the human who sent the message,
    /// not to the whole group's `PeerId`. DMs have no meaningful distinction
    /// between the two, since `peer` and `sender.id` coincide there.
    fn rate_limit_key(event: &InboundEvent) -> String {
        event
            .sender
            .as_ref()
            .map(|sender| sender.id.clone())
            .unwrap_or_else(|| event.peer.as_str().to_owned())
    }

    /// Applies and idempotently records one inbound event against the
    /// per-peer sliding-window budget (§3.5's `max_updates_per_peer_per_min`).
    ///
    /// # Returns
    /// `true` if the event stays within the configured budget for its
    /// window, `false` once the budget for the current window is exhausted.
    ///
    /// # Concurrency
    /// A repeated call for an event carrying the *same* `raw_event_id` as the
    /// last call for this key does not consume an additional slot: the
    /// admission pipeline may re-check an already-admitted event (see the
    /// module docs), and this must not silently halve the configured budget.
    fn record_inbound_rate(&self, event: &InboundEvent) -> bool {
        let limit = self.config.max_updates_per_peer_per_min;
        let key = Self::rate_limit_key(event);
        let now = event.received_at;
        let mut windows = self
            .rate_limits
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let window = windows.entry(key).or_insert_with(|| RateWindow {
            window_start: now,
            count: 0,
            last_counted_update: None,
        });

        if window_expired(window.window_start, now) {
            window.window_start = now;
            window.count = 0;
            window.last_counted_update = None;
        }

        if event.raw_event_id.is_some() && window.last_counted_update == event.raw_event_id {
            // Same event re-checked (not a fresh update): report the prior
            // decision without consuming another slot.
            return window.count <= limit;
        }

        window.count = window.count.saturating_add(1);
        window.last_counted_update = event.raw_event_id.clone();
        window.count <= limit
    }

    /// Emits at most one [`ThrottleNotice`] per rate-limit window (§3.5).
    ///
    /// Called only from [`Self::forward_ingress_event`], which processes each
    /// unique inbound event exactly once, so this cannot double-notify for a
    /// single event the way [`Self::admit`] can be re-invoked for one.
    fn maybe_notify_throttled(&self, event: &InboundEvent) {
        let Some(sink) = self.throttle_sink.as_ref() else {
            return;
        };
        let key = Self::rate_limit_key(event);
        let now = event.received_at;
        let should_notify = {
            let mut last = self
                .last_throttle_notice
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let should = match last.get(&key) {
                Some(previous) => window_expired(*previous, now),
                None => true,
            };
            if should {
                last.insert(key, now);
            }
            should
        };
        if !should_notify {
            return;
        }
        let notice = ThrottleNotice {
            peer: event.peer.clone(),
            thread: event.thread.clone(),
            content: OutboundContent::Message {
                markdown: THROTTLE_NOTICE_TEXT.to_owned(),
            },
        };
        if let Ok(sink) = sink.lock() {
            // Best-effort: a full/closed outbound handoff must not block or
            // fail admission, which has already made its (correct) decision.
            let _ = sink.send(notice);
        }
    }

    /// Löst einen `/pair <Code>`-DM eines gepinnten, aber ungepairten Peers
    /// direkt im Channel ein (§3.2) — das In-Channel-Gegenstück zu
    /// `harw connect --pair`.
    ///
    /// Nur aufgerufen für Events mit `Admission::Deferred(Onboarding)`, also
    /// nachdem Struktur-, Pinning-, Replay-, Rate-Limit- (§3.5, begrenzt
    /// damit auch Einlöseversuche pro Absender) und Gruppen-Gates gegriffen
    /// haben. Zusätzlich gilt: nur private DMs (Absender == DM-Peer, keine
    /// konfigurierte Gruppe) und nur ein syntaktisch gültiger Code berühren
    /// den Pairing-Store — beliebiger Text erzeugt dort keine Lock-Datei.
    /// Alle anderen zurückgestellten Events werden wie bisher verworfen.
    ///
    /// Fehler der Einlösung sind nicht fatal für die Ingress-Schleife und
    /// werden nie geloggt oder in die Antwort übernommen, da die Fehler-
    /// Varianten des Stores den Code enthalten.
    fn redeem_deferred_pairing(&self, event: &InboundEvent) {
        let Some(sender) = event.sender.as_ref() else {
            return;
        };
        if sender.id != event.peer.as_str() || self.config.is_group(&event.peer) {
            return;
        }
        let Some(code) = event.text.as_deref().and_then(parse_pair_command) else {
            return;
        };
        let outcome = match self.redeem_pairing(&code, &event.peer, event.received_at) {
            Ok(record) => PairingOutcome::Paired {
                tenant: record.tenant,
            },
            Err(_) => PairingOutcome::Failed,
        };
        self.notify_pairing(event, outcome);
    }

    /// Übergibt eine [`PairingNotice`] best-effort an den optionalen Sink.
    fn notify_pairing(&self, event: &InboundEvent, outcome: PairingOutcome) {
        let Some(sink) = self.pairing_sink.as_ref() else {
            return;
        };
        let text = match outcome {
            PairingOutcome::Paired { .. } => PAIRING_SUCCESS_TEXT,
            PairingOutcome::Failed => PAIRING_FAILURE_TEXT,
        };
        let notice = PairingNotice {
            peer: event.peer.clone(),
            thread: event.thread.clone(),
            outcome,
            content: OutboundContent::Message {
                markdown: text.to_owned(),
            },
        };
        if let Ok(sink) = sink.lock() {
            // Best-effort wie beim Throttle-Sink: eine volle/geschlossene
            // Übergabe darf die bereits durchgeführte Einlösung nicht
            // zurückdrehen oder die Ingress-Schleife stoppen.
            let _ = sink.send(notice);
        }
    }

    /// Returns the intersection-only capability profile for this remote binding.
    #[must_use]
    pub fn sandbox(&self) -> &TelegramSandbox {
        &self.sandbox
    }

    fn validate_channel(&self, event: &InboundEvent) -> TelegramChannelResult<()> {
        if event.channel != self.config.channel_id {
            return Err(TelegramChannelError::ChannelMismatch {
                expected: self.config.channel_id.clone(),
                actual: event.channel.clone(),
            });
        }
        Ok(())
    }

    /// Rejects transport-normalized events that cannot represent a useful,
    /// replayable Telegram update. This must run before pairing or replay
    /// work: malformed input must not probe durable state or consume an
    /// update id.
    fn is_valid_ingress_event(event: &InboundEvent) -> bool {
        if event.channel.as_str().trim().is_empty()
            || event.peer.as_str().trim().is_empty()
            || event
                .raw_event_id
                .as_deref()
                .is_none_or(|update_id| update_id.trim().is_empty())
        {
            return false;
        }
        if event
            .thread
            .as_ref()
            .is_some_and(|thread| thread.as_str().trim().is_empty())
            || event
                .sender
                .as_ref()
                .is_some_and(|sender| sender.id.trim().is_empty())
            || event
                .attachments
                .iter()
                .any(|attachment| attachment.remote_id.trim().is_empty())
        {
            return false;
        }

        event
            .text
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty())
            || !event.attachments.is_empty()
    }

    fn render_text(&self, markdown: &str) -> Vec<ChannelSendOp> {
        self.capabilities()
            .chunk(&escape_markdown_v2(markdown))
            .into_iter()
            .map(|text| ChannelSendOp::SendMessage {
                text,
                inline_actions: Vec::new(),
            })
            .collect()
    }

    /// Processes one transport-normalized event through the Telegram perimeter.
    ///
    /// Pinning is checked before the replay claim, before channel mismatches
    /// are otherwise treated as durable-eligible, and before any session
    /// work: an unpinned sender must not be able to make this binding write a
    /// single file or journal entry (F-040/F-041 remediation of S5 — the plan
    /// and §2.3 both require unauthenticated traffic to create no journal
    /// entries). Only events that survive pinning, durable replay claiming,
    /// pairing/session resolution, and admission enter the runtime sink.
    /// Events deferred for onboarding never enter the runtime sink; a private
    /// `/pair <Code>` DM among them is redeemed in-channel via
    /// [`Self::redeem_deferred_pairing`], everything else is dropped. The
    /// reduced sandbox remains attached to this adapter and is never widened
    /// by ingress.
    fn forward_ingress_event(
        &self,
        event: InboundEvent,
        sink: &Sender<InboundEvent>,
    ) -> TelegramChannelResult<()> {
        if !Self::is_valid_ingress_event(&event) {
            return Ok(());
        }
        self.validate_channel(&event)?;
        // Zustandsloses Pinning-Gate zuerst: kein Claim, keine Journal-/
        // Dateischreibung für einen nicht gepinnten Absender. Nur ein
        // In-Memory-Zähler beobachtet das, niemals der Nachrichteninhalt oder
        // die Sender-Identität selbst.
        if !self.sender_pinned(&event) {
            self.rejected_unpinned.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
        // Telegram update delivery is at-least-once. The structural and
        // pinning gates above ensure this binding can make replay claiming
        // durable before the event reaches a potentially billable runtime
        // sink.
        if !self.claim_update(&event)? {
            return Ok(());
        }

        let key = self.derive_session_key(&event)?;
        match self.admit(&event, &key) {
            Admission::Admitted => {
                sink.send(event)
                    .map_err(|_| TelegramChannelError::IngressUnavailable)?;
            }
            Admission::Rejected(RejectionReason::RateLimited) => {
                self.maybe_notify_throttled(&event);
            }
            Admission::Deferred(DeferralReason::Onboarding) => {
                self.redeem_deferred_pairing(&event);
            }
            Admission::Rejected(_) => {}
        }
        Ok(())
    }
}

/// Extrahiert den Code aus einem `/pair <Code>`- bzw. `/pair@bot <Code>`-
/// Kommando (Befehl case-insensitiv, wie `harw gateway` ihn erkennt).
///
/// Genau zwei Tokens; der Code wird auf ASCII-Großbuchstaben normalisiert
/// und muss die Form `XXXX-XXXX` im Crockford-Base32-Alphabet haben, sonst
/// `None` — so erreicht kein Freitext den Pairing-Store.
fn parse_pair_command(text: &str) -> Option<String> {
    let mut tokens = text.split_ascii_whitespace();
    let command = tokens.next()?.to_ascii_lowercase();
    let code = tokens.next()?;
    if tokens.next().is_some() {
        return None;
    }
    let is_pair = command == "/pair"
        || command
            .strip_prefix("/pair@")
            .is_some_and(|bot| !bot.is_empty());
    if !is_pair {
        return None;
    }
    let code = code.to_ascii_uppercase();
    let bytes = code.as_bytes();
    let well_formed = bytes.len() == 9
        && bytes.iter().enumerate().all(|(index, byte)| {
            if index == 4 {
                *byte == b'-'
            } else {
                PAIRING_CODE_ALPHABET.contains(byte)
            }
        });
    well_formed.then_some(code)
}

/// Whether more than [`RATE_LIMIT_WINDOW`] has elapsed since `window_start`,
/// i.e. whether a sliding-window counter/notice anchored at `window_start`
/// must reset as of `now`. Timestamp overflow (astronomically unlikely for
/// wall-clock inputs) is treated as expired so the gate fails open toward
/// resetting rather than getting stuck permanently closed.
fn window_expired(window_start: Timestamp, now: Timestamp) -> bool {
    window_start
        .checked_add(RATE_LIMIT_WINDOW)
        .map_or(true, |window_end| now >= window_end)
}

impl ChannelAdapter for TelegramChannel {
    type Error = TelegramChannelError;

    fn capabilities(&self) -> ChannelCapabilities {
        ChannelCapabilities {
            markdown: MarkdownSupport::BasicV1,
            max_message_len: 4096,
            attachments: AttachmentSupport {
                max_size_bytes: 20_000_000,
                max_count_per_message: 10,
                allowed_kinds: vec![
                    "image/*".to_owned(),
                    "application/pdf".to_owned(),
                    "text/plain".to_owned(),
                ],
            },
            edits: true,
            reactions: false,
            threads: ThreadSupport::Native,
            inline_actions: true,
        }
    }

    fn derive_session_key(&self, event: &InboundEvent) -> TelegramChannelResult<SessionKey> {
        self.validate_channel(event)?;
        let tenant = self
            .resolve_tenant(&event.peer)?
            .unwrap_or_else(|| TenantId::from_str(UNPAIRED_TENANT));
        let thread = match self.config.topic_mode {
            TopicMode::PerTopicSession => event.thread.clone(),
            TopicMode::SharedSession => None,
        };
        Ok(SessionKey::new(
            tenant,
            event.channel.clone(),
            event.peer.clone(),
            thread,
        ))
    }

    fn admit(&self, event: &InboundEvent, key: &SessionKey) -> Admission {
        if !self.sender_pinned(event) {
            return Admission::Rejected(RejectionReason::NotAllowlisted);
        }
        if self.validate_channel(event).is_err() {
            return Admission::Rejected(RejectionReason::Other("channel mismatch".to_owned()));
        }
        if !self.record_inbound_rate(event) {
            return Admission::Rejected(RejectionReason::RateLimited);
        }
        if self.config.is_group(&event.peer) {
            if self.config.require_mention_in_groups && !event.mentioned {
                return Admission::Rejected(RejectionReason::NoMention);
            }
            if !self.config.group_allowed_senders.is_empty()
                && event
                    .sender
                    .as_ref()
                    .is_none_or(|sender| !self.config.group_allowed_senders.contains(&sender.id))
            {
                return Admission::Rejected(RejectionReason::NotAllowlisted);
            }
        }
        if !Self::is_paired(&key.tenant) {
            return Admission::Deferred(DeferralReason::Onboarding);
        }
        Admission::Admitted
    }

    fn render_outbound(&self, content: &OutboundContent) -> Vec<ChannelSendOp> {
        match content {
            OutboundContent::Message { markdown } => self.render_text(markdown),
            OutboundContent::StatusUpdate {
                status_key,
                markdown,
            } => vec![ChannelSendOp::EditMessage {
                status_key: status_key.clone(),
                text: escape_markdown_v2(markdown),
            }],
            OutboundContent::Approval(prompt) => vec![ChannelSendOp::SendMessage {
                text: escape_markdown_v2(&format!(
                    "Approval requested ({})\\n{}",
                    prompt.risk, prompt.summary
                )),
                inline_actions: prompt
                    .actions
                    .iter()
                    .map(|action| InlineAction {
                        label: action.label.clone(),
                        callback_payload: self
                            .approval_tokens
                            .issue(&prompt.request_id, &action.decision),
                    })
                    .collect(),
            }],
        }
    }

    fn run_ingress(&self, sink: Sender<InboundEvent>) -> TelegramChannelResult<()> {
        let ingress = self
            .ingress
            .as_ref()
            .ok_or(TelegramChannelError::IngressUnavailable)?
            .clone();

        // `Receiver` is not `Sync`, so one runner must own it exclusively.
        // Extract it while locked, but release the channel mutex before the
        // potentially unbounded receive below.
        let receiver = ingress
            .lock()
            .map_err(|_| TelegramChannelError::IngressUnavailable)?
            .take()
            .ok_or(TelegramChannelError::IngressUnavailable)?;

        let result = loop {
            let event = match receiver.recv() {
                Ok(event) => event,
                Err(_) => break Ok(()),
            };
            if let Err(error) = self.forward_ingress_event(event, &sink) {
                break Err(error);
            }
        };

        // Keep the transport receiver reusable after a stopped runner or an
        // event-processing failure, matching the prior receiver lifetime.
        let mut slot = ingress
            .lock()
            .map_err(|_| TelegramChannelError::IngressUnavailable)?;
        *slot = Some(receiver);
        result
    }
}

impl From<TelegramChannelError> for ChannelError {
    fn from(value: TelegramChannelError) -> Self {
        match value {
            TelegramChannelError::Pairing(error) => error,
            TelegramChannelError::ChannelMismatch { actual, .. } => ChannelError::AdmissionDenied {
                channel: actual,
                reason: CHANNEL_MISMATCH_REASON.to_owned(),
            },
            TelegramChannelError::IngressUnavailable => ChannelError::IngressUnavailable {
                channel: harw_types::ChannelId::from_str("telegram"),
                reason: INGRESS_UNAVAILABLE_MESSAGE.to_owned(),
            },
            TelegramChannelError::UnpairedPeer { peer } => ChannelError::Unpaired {
                // This legacy error variant predates channel context. Keep
                // the conversion typed and fail closed rather than claiming
                // that pairing admission is an unimplemented feature.
                channel: harw_types::ChannelId::from_str("telegram"),
                peer,
            },
            // The work-request variants below likewise predate a channel-id
            // field (they are surfaced directly to a Telegram reply by
            // `harw-cli/src/gateway.rs`, not through this admission-pipeline
            // conversion); folded into `AdmissionDenied`/`OperationUnsupported`
            // with the same "telegram" placeholder channel used above so this
            // match stays exhaustive without inventing new `ChannelError`
            // shapes for a path that does not exercise them today.
            TelegramChannelError::WorkspaceUnresolved { alias, tenant, .. } => {
                ChannelError::AdmissionDenied {
                    channel: harw_types::ChannelId::from_str("telegram"),
                    reason: format!("workspace alias '{alias}' unresolved for tenant '{tenant}'"),
                }
            }
            TelegramChannelError::InvalidWorkRequestRole { role } => {
                ChannelError::AdmissionDenied {
                    channel: harw_types::ChannelId::from_str("telegram"),
                    reason: format!("invalid work-request role '{role}'"),
                }
            }
            TelegramChannelError::WorkRequestNotFound { work_id } => {
                ChannelError::AdmissionDenied {
                    channel: harw_types::ChannelId::from_str("telegram"),
                    reason: format!("work request '{work_id}' is unknown"),
                }
            }
            TelegramChannelError::WorkRequestInvalidTransition {
                work_id,
                from,
                action,
            } => ChannelError::AdmissionDenied {
                channel: harw_types::ChannelId::from_str("telegram"),
                reason: format!("work request '{work_id}' cannot be {action} from state '{from}'"),
            },
            TelegramChannelError::LaunchNotYetAvailable { work_id } => {
                ChannelError::OperationUnsupported {
                    channel: harw_types::ChannelId::from_str("telegram"),
                    operation: "sandboxed-launch",
                    detail: format!("work request '{work_id}'"),
                }
            }
            TelegramChannelError::Io(error) => ChannelError::Io(error),
            TelegramChannelError::Serde(error) => ChannelError::Serde(error),
        }
    }
}

/// Escapes the Telegram MarkdownV2 reserved set without interpreting input as
/// Telegram markup. Rendering is therefore a reduction, never a privilege gain.
fn escape_markdown_v2(input: &str) -> String {
    const RESERVED: &[char] = &[
        '_', '*', '[', ']', '(', ')', '~', '`', '>', '#', '+', '-', '=', '|', '{', '}', '.', '!',
    ];
    let mut escaped = String::with_capacity(input.len());
    for character in input.chars() {
        if RESERVED.contains(&character) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::thread;
    use std::time::{Duration, Instant};

    use jiff::Timestamp;

    use super::*;
    use crate::approval_tokens::{TelegramChatId, TelegramMessageId, TelegramThreadId};
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_channel::{ApprovalAction, ApprovalPrompt, PairingStore, SenderRef, ThreadRef};
    use harw_types::{ChannelId, PeerId, TenantId};

    fn store() -> TestResult<(tempfile::TempDir, Arc<PairingStore>)> {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = Arc::new(PairingStore::new(dir.path()));
        Ok((dir, store))
    }

    fn config(channel: ChannelId) -> TelegramChannelConfig {
        let mut config = TelegramChannelConfig::new(channel);
        config.pinned_sender_ids.insert("alice".to_owned());
        config
    }

    fn pair(store: &PairingStore, channel: &ChannelId, peer: &str, tenant: &str) -> TestResult {
        let now = Timestamp::now();
        let code = store
            .issue_code(channel, &TenantId::from_str(tenant), b"seed12345", now)
            .map_err(ctx("issue"))?;
        store
            .redeem_once(channel, &code, &PeerId::from_str(peer), now)
            .map_err(ctx("redeem"))?;
        Ok(())
    }

    fn event(peer: &str, mentioned: bool) -> InboundEvent {
        InboundEvent {
            channel: ChannelId::from_str("telegram:ops"),
            peer: PeerId::from_str(peer),
            thread: Some(ThreadRef::from_str("42")),
            sender: Some(SenderRef {
                id: "alice".to_owned(),
                display_name: None,
            }),
            text: Some("hello".to_owned()),
            mentioned,
            attachments: Vec::new(),
            raw_event_id: Some("12".to_owned()),
            received_at: Timestamp::now(),
        }
    }

    #[test]
    fn unpaired_dm_defers_without_a_real_tenant() -> TestResult {
        let (_dir, store) = store()?;
        let adapter = TelegramChannel::new(config(ChannelId::from_str("telegram:ops")), store);
        let inbound = event("100", true);
        let key = adapter
            .derive_session_key(&inbound)
            .map_err(ctx("derive_session_key"))?;
        assert_eq!(key.tenant.as_str(), UNPAIRED_TENANT);
        assert_eq!(
            adapter.admit(&inbound, &key),
            Admission::Deferred(DeferralReason::Onboarding)
        );
        Ok(())
    }

    #[test]
    fn unpinned_sender_is_rejected_before_onboarding_or_group_mention_checks() -> TestResult {
        let (_dir, store) = store()?;
        let mut config = config(ChannelId::from_str("telegram:ops"));
        config.allowed_group_chats.insert("-1001".to_owned());
        let adapter = TelegramChannel::new(config, store);
        let mut inbound = event("-1001", false);
        inbound.sender = Some(SenderRef {
            id: "mallory".to_owned(),
            display_name: None,
        });
        let key = adapter
            .derive_session_key(&inbound)
            .map_err(ctx("derive_session_key"))?;

        assert_eq!(key.tenant.as_str(), UNPAIRED_TENANT);
        assert_eq!(
            adapter.admit(&inbound, &key),
            Admission::Rejected(RejectionReason::NotAllowlisted)
        );
        Ok(())
    }

    #[test]
    fn paired_dm_resolves_real_tenant_and_admits() -> TestResult {
        let (_dir, store) = store()?;
        let channel = ChannelId::from_str("telegram:ops");
        pair(&store, &channel, "100", "ops")?;
        let adapter = TelegramChannel::new(config(channel), store);
        let inbound = event("100", true);
        let key = adapter
            .derive_session_key(&inbound)
            .map_err(ctx("derive_session_key"))?;
        assert_eq!(key.tenant.as_str(), "ops");
        assert_eq!(adapter.admit(&inbound, &key), Admission::Admitted);
        let key_again = adapter
            .derive_session_key(&inbound)
            .map_err(ctx("derive_session_key"))?;
        assert_eq!(key, key_again);
        Ok(())
    }

    #[test]
    fn groups_require_a_mention_before_admission() -> TestResult {
        let (_dir, store) = store()?;
        let mut config = config(ChannelId::from_str("telegram:ops"));
        config.allowed_group_chats.insert("-1001".to_owned());
        let adapter = TelegramChannel::new(config, store);
        let inbound = event("-1001", false);
        let key = adapter
            .derive_session_key(&inbound)
            .map_err(ctx("derive_session_key"))?;
        assert_eq!(
            adapter.admit(&inbound, &key),
            Admission::Rejected(RejectionReason::NoMention)
        );
        Ok(())
    }

    #[test]
    fn shared_topic_mode_collapses_threads() -> TestResult {
        let (_dir, store) = store()?;
        let channel = ChannelId::from_str("telegram:ops");
        pair(&store, &channel, "100", "ops")?;
        let mut config = config(channel);
        config.topic_mode = TopicMode::SharedSession;
        let adapter = TelegramChannel::new(config, store);
        assert_eq!(
            adapter
                .derive_session_key(&event("100", true))
                .map_err(ctx("derive_session_key"))?
                .thread,
            None
        );
        Ok(())
    }

    #[test]
    fn approval_callbacks_require_bound_context_and_are_single_use() -> TestResult {
        let (_dir, store) = store()?;
        let adapter = TelegramChannel::new(config(ChannelId::from_str("telegram:ops")), store);
        let ops = adapter.render_outbound(&OutboundContent::Approval(ApprovalPrompt {
            request_id: "a-1".to_owned(),
            summary: "write file!".to_owned(),
            risk: "high".to_owned(),
            actions: vec![ApprovalAction {
                label: "Approve".to_owned(),
                decision: "approve".to_owned(),
            }],
        }));
        let ChannelSendOp::SendMessage {
            text,
            inline_actions,
        } = &ops[0]
        else {
            return Err(TestError::Unexpected(
                "approval must render as a Telegram message".to_owned(),
            ));
        };
        let unbound_token = &inline_actions[0].callback_payload;
        assert!(text.contains("file\\!"));
        assert!(!unbound_token.contains("a-1"));
        assert!(!unbound_token.contains("approve"));

        let context = ApprovalCallbackContext::new(
            TelegramChatId(100),
            TelegramMessageId(200),
            PeerId::from_str("100"),
        )
        .with_thread(TelegramThreadId(42));
        assert_eq!(
            adapter.consume_approval_callback(unbound_token, &context),
            None
        );
        assert!(!adapter.bind_approval_callback(unbound_token, context.clone()));
        assert_eq!(
            adapter.consume_approval_callback(unbound_token, &context),
            None
        );

        let ops = adapter.render_outbound(&OutboundContent::Approval(ApprovalPrompt {
            request_id: "a-1".to_owned(),
            summary: "write file!".to_owned(),
            risk: "high".to_owned(),
            actions: vec![ApprovalAction {
                label: "Approve".to_owned(),
                decision: "approve".to_owned(),
            }],
        }));
        let ChannelSendOp::SendMessage { inline_actions, .. } = &ops[0] else {
            return Err(TestError::Unexpected(
                "approval must render as a Telegram message".to_owned(),
            ));
        };
        let bound_token = &inline_actions[0].callback_payload;
        assert!(adapter.bind_approval_callback(bound_token, context.clone()));
        assert_eq!(
            adapter.consume_approval_callback(bound_token, &context),
            Some(PendingApproval {
                request_id: "a-1".to_owned(),
                decision: "approve".to_owned(),
            })
        );
        assert_eq!(
            adapter.consume_approval_callback(bound_token, &context),
            None
        );
        Ok(())
    }

    #[test]
    fn approval_callback_wrong_context_is_denied() -> TestResult {
        let (_dir, store) = store()?;
        let adapter = TelegramChannel::new(config(ChannelId::from_str("telegram:ops")), store);
        let ops = adapter.render_outbound(&OutboundContent::Approval(ApprovalPrompt {
            request_id: "a-2".to_owned(),
            summary: "write file!".to_owned(),
            risk: "high".to_owned(),
            actions: vec![ApprovalAction {
                label: "Approve".to_owned(),
                decision: "approve".to_owned(),
            }],
        }));
        let ChannelSendOp::SendMessage { inline_actions, .. } = &ops[0] else {
            return Err(TestError::Unexpected(
                "approval must render as a Telegram message".to_owned(),
            ));
        };
        let token = &inline_actions[0].callback_payload;
        let expected = ApprovalCallbackContext::new(
            TelegramChatId(100),
            TelegramMessageId(200),
            PeerId::from_str("100"),
        )
        .with_thread(TelegramThreadId(42));
        let wrong = ApprovalCallbackContext::new(
            TelegramChatId(999),
            TelegramMessageId(200),
            PeerId::from_str("100"),
        )
        .with_thread(TelegramThreadId(42));

        assert!(adapter.bind_approval_callback(token, expected.clone()));
        assert_eq!(adapter.consume_approval_callback(token, &wrong), None);
        assert_eq!(adapter.consume_approval_callback(token, &expected), None);
        Ok(())
    }

    #[test]
    fn telegram_profile_only_removes_upstream_capabilities() -> TestResult {
        use harw_authority::Permission::{ExecuteProcess, ReadWorkspace, WriteWorkspace};

        let (_dir, store) = store()?;
        let adapter = TelegramChannel::new(config(ChannelId::from_str("telegram:ops")), store);
        let reduced = adapter
            .sandbox()
            .reduce([ReadWorkspace, WriteWorkspace, ExecuteProcess]);

        assert!(reduced.contains(ReadWorkspace));
        assert!(!reduced.contains(WriteWorkspace));
        assert!(!reduced.contains(ExecuteProcess));
        Ok(())
    }

    #[test]
    fn claim_update_deduplicates_retried_updates() -> TestResult {
        let (_dir, store) = store()?;
        let adapter = TelegramChannel::new(config(ChannelId::from_str("telegram:ops")), store);
        let mut ev = event("100", true);
        ev.raw_event_id = Some("55".to_owned());
        assert!(adapter.claim_update(&ev).map_err(ctx("claim_update"))?);
        assert!(!adapter.claim_update(&ev).map_err(ctx("claim_update"))?);
        Ok(())
    }

    #[test]
    fn ingress_forwards_only_paired_first_delivery() -> TestResult {
        let (_dir, store) = store()?;
        let channel = ChannelId::from_str("telegram:ops");
        pair(&store, &channel, "100", "ops")?;
        let (ingress_tx, ingress_rx) = mpsc::channel();
        let adapter = TelegramChannel::with_ingress_receiver(config(channel), store, ingress_rx);
        let (sink_tx, sink_rx) = mpsc::channel();

        let inbound = event("100", true);
        ingress_tx.send(inbound.clone()).map_err(ctx("send"))?;
        ingress_tx.send(inbound).map_err(ctx("send"))?;
        drop(ingress_tx);

        adapter.run_ingress(sink_tx).map_err(ctx("run_ingress"))?;
        assert_eq!(sink_rx.recv().map_err(ctx("recv"))?.peer.as_str(), "100");
        assert!(sink_rx.try_recv().is_err());
        Ok(())
    }

    #[test]
    fn ingress_releases_channel_mutex_before_waiting_for_events() -> TestResult {
        let (_dir, store) = store()?;
        let channel = ChannelId::from_str("telegram:ops");
        pair(&store, &channel, "100", "ops")?;
        let (ingress_tx, ingress_rx) = mpsc::channel();
        let adapter = TelegramChannel::with_ingress_receiver(config(channel), store, ingress_rx);
        let ingress = adapter
            .ingress
            .as_ref()
            .ok_or(TestError::Missing("configured ingress"))?
            .clone();
        let (sink_tx, sink_rx) = mpsc::channel();
        let runner = adapter.clone();
        let join = thread::spawn(move || runner.run_ingress(sink_tx));

        // Poll on a wall-clock deadline rather than a fixed spin-count: a
        // fixed number of `yield_now()` calls is not guaranteed to give the
        // OS scheduler enough opportunities to actually run the spawned
        // runner thread when the host is under heavy concurrent load (e.g.
        // other test/agent activity competing for CPU), even though no
        // deadlock exists. If the mutex really were held across `recv`
        // (the bug this test guards against), `ingress.lock()` below would
        // itself block forever and the test would hang/time out instead of
        // reaching either assertion.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if ingress
                .lock()
                .map_err(ctx("ingress lock"))?
                .as_ref()
                .is_none()
            {
                assert!(
                    ingress.try_lock().is_ok(),
                    "the ingress mutex must not remain locked while recv blocks"
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "ingress runner did not extract the receiver within the timeout"
            );
            thread::yield_now();
        }

        ingress_tx
            .send(event("100", true))
            .map_err(ctx("send ingress event"))?;
        drop(ingress_tx);

        let run_result = join
            .join()
            .map_err(|_| TestError::Unexpected("ingress runner panicked".to_owned()))?;
        run_result.map_err(ctx("run_ingress"))?;
        assert_eq!(
            sink_rx
                .recv()
                .map_err(ctx("forwarded event"))?
                .peer
                .as_str(),
            "100"
        );
        assert!(ingress.lock().map_err(ctx("ingress lock"))?.is_some());
        Ok(())
    }

    #[test]
    fn ingress_drops_unpaired_and_rejected_events() -> TestResult {
        let (_dir, store) = store()?;
        let mut config = config(ChannelId::from_str("telegram:ops"));
        config.allowed_group_chats.insert("-1001".to_owned());
        let (ingress_tx, ingress_rx) = mpsc::channel();
        let adapter = TelegramChannel::with_ingress_receiver(config, store, ingress_rx);
        let (sink_tx, sink_rx) = mpsc::channel();

        ingress_tx.send(event("100", true)).map_err(ctx("send"))?;
        ingress_tx
            .send(event("-1001", false))
            .map_err(ctx("send"))?;
        drop(ingress_tx);

        adapter.run_ingress(sink_tx).map_err(ctx("run_ingress"))?;
        assert!(sink_rx.try_recv().is_err());
        Ok(())
    }

    #[test]
    fn ingress_drops_unpinned_sender_before_claiming_replay_or_pairing() -> TestResult {
        let (_dir, store) = store()?;
        let channel = ChannelId::from_str("telegram:ops");
        pair(&store, &channel, "100", "ops")?;
        let (ingress_tx, ingress_rx) = mpsc::channel();
        let adapter = TelegramChannel::with_ingress_receiver(config(channel), store, ingress_rx);
        let (sink_tx, sink_rx) = mpsc::channel();

        let mut inbound = event("100", true);
        inbound.sender = Some(SenderRef {
            id: "mallory".to_owned(),
            display_name: None,
        });
        ingress_tx.send(inbound.clone()).map_err(ctx("send"))?;
        drop(ingress_tx);

        adapter.run_ingress(sink_tx).map_err(ctx("run_ingress"))?;

        // Kein Treffer im Sink ...
        assert!(sink_rx.try_recv().is_err());
        // ... und vor allem: die Replay-Claim-Datei/Journal wurde nie
        // angelegt. Ein anschließender `claim_update` desselben Updates ist
        // also weiterhin der *erste* Claim (liefert `true`), nicht der
        // zweite (F-040/S5).
        assert!(
            adapter
                .claim_update(&inbound)
                .map_err(ctx("claim_update"))?
        );
        // Nur der zustandslose Zähler hat den Vorgang beobachtet.
        assert_eq!(adapter.rejected_unpinned_sender_count(), 1);
        Ok(())
    }

    #[test]
    fn ingress_drops_events_without_a_replayable_update_id() -> TestResult {
        let (_dir, store) = store()?;
        let channel = ChannelId::from_str("telegram:ops");
        pair(&store, &channel, "100", "ops")?;
        let (ingress_tx, ingress_rx) = mpsc::channel();
        let adapter = TelegramChannel::with_ingress_receiver(config(channel), store, ingress_rx);
        let (sink_tx, sink_rx) = mpsc::channel();

        let mut inbound = event("100", true);
        inbound.raw_event_id = None;
        ingress_tx.send(inbound).map_err(ctx("send"))?;
        drop(ingress_tx);

        adapter.run_ingress(sink_tx).map_err(ctx("run_ingress"))?;
        assert!(sink_rx.try_recv().is_err());
        Ok(())
    }

    #[test]
    fn ingress_drops_contentless_events_before_replay_claim_or_pairing() -> TestResult {
        let (_dir, store) = store()?;
        let channel = ChannelId::from_str("telegram:ops");
        pair(&store, &channel, "100", "ops")?;
        let (ingress_tx, ingress_rx) = mpsc::channel();
        let adapter =
            TelegramChannel::with_ingress_receiver(config(channel.clone()), store, ingress_rx);
        let (sink_tx, sink_rx) = mpsc::channel();

        let mut inbound = event("100", true);
        inbound.text = None;
        inbound.attachments.clear();
        ingress_tx.send(inbound.clone()).map_err(ctx("send"))?;
        drop(ingress_tx);

        adapter.run_ingress(sink_tx).map_err(ctx("run_ingress"))?;
        assert!(sink_rx.try_recv().is_err());
        assert!(
            adapter
                .claim_update(&inbound)
                .map_err(ctx("claim_update"))?
        );
        Ok(())
    }

    #[test]
    fn ingress_drops_whitespace_identifiers_and_malformed_payload_metadata() -> TestResult {
        let (_dir, store) = store()?;
        let channel = ChannelId::from_str("telegram:ops");
        pair(&store, &channel, "100", "ops")?;
        let (ingress_tx, ingress_rx) = mpsc::channel();
        let adapter = TelegramChannel::with_ingress_receiver(config(channel), store, ingress_rx);
        let (sink_tx, sink_rx) = mpsc::channel();

        let mut inbound = event("100", true);
        inbound.raw_event_id = Some("  ".to_owned());
        inbound.sender = Some(SenderRef {
            id: "  ".to_owned(),
            display_name: None,
        });
        ingress_tx.send(inbound.clone()).map_err(ctx("send"))?;
        drop(ingress_tx);

        adapter.run_ingress(sink_tx).map_err(ctx("run_ingress"))?;
        assert!(sink_rx.try_recv().is_err());
        assert!(
            adapter
                .claim_update(&inbound)
                .map_err(ctx("claim_update"))?
        );
        Ok(())
    }

    #[test]
    fn ingress_without_transport_receiver_fails_closed() -> TestResult {
        let (_dir, store) = store()?;
        let adapter = TelegramChannel::new(config(ChannelId::from_str("telegram:ops")), store);
        let (sink_tx, _sink_rx) = mpsc::channel();

        assert!(matches!(
            adapter.run_ingress(sink_tx),
            Err(TelegramChannelError::IngressUnavailable)
        ));
        Ok(())
    }

    #[test]
    fn pairing_error_conversion_preserves_core_error() {
        let error = ChannelError::PairingExpired {
            code: "pairing boundary test".to_owned(),
        };

        assert!(matches!(
            ChannelError::from(TelegramChannelError::Pairing(error)),
            ChannelError::PairingExpired { code } if code == "pairing boundary test"
        ));
    }

    #[test]
    fn channel_mismatch_conversion_denies_actual_channel_without_details() {
        let actual = ChannelId::from_str("telegram:unexpected");
        let expected = ChannelId::from_str("telegram:ops");

        assert!(matches!(
            ChannelError::from(TelegramChannelError::ChannelMismatch {
                expected,
                actual: actual.clone(),
            }),
            ChannelError::AdmissionDenied { channel, reason }
                if channel == actual
                    && reason == CHANNEL_MISMATCH_REASON
                    && !reason.contains("telegram:")
        ));
    }

    #[test]
    fn unpaired_peer_conversion_preserves_typed_admission_failure() {
        let peer = PeerId::from_str("100");

        assert!(matches!(
            ChannelError::from(TelegramChannelError::UnpairedPeer {
                peer: peer.clone(),
            }),
            ChannelError::Unpaired { channel, peer: actual_peer }
                if channel.as_str() == "telegram" && actual_peer == peer
        ));
    }

    #[test]
    fn rate_limit_rejects_beyond_budget_and_recovers_next_window() -> TestResult {
        let (_dir, store) = store()?;
        let channel = ChannelId::from_str("telegram:ops");
        pair(&store, &channel, "100", "ops")?;
        let mut config = config(channel);
        config.max_updates_per_peer_per_min = 2;
        let adapter = TelegramChannel::new(config, store);
        let base = Timestamp::now();

        let mut first = event("100", true);
        first.raw_event_id = Some("1".to_owned());
        first.received_at = base;
        let key = adapter
            .derive_session_key(&first)
            .map_err(ctx("derive_session_key"))?;
        assert_eq!(adapter.admit(&first, &key), Admission::Admitted);

        let mut second = event("100", true);
        second.raw_event_id = Some("2".to_owned());
        second.received_at = base;
        assert_eq!(adapter.admit(&second, &key), Admission::Admitted);

        let mut third = event("100", true);
        third.raw_event_id = Some("3".to_owned());
        third.received_at = base;
        assert_eq!(
            adapter.admit(&third, &key),
            Admission::Rejected(RejectionReason::RateLimited)
        );

        let mut fourth = event("100", true);
        fourth.raw_event_id = Some("4".to_owned());
        fourth.received_at = base
            .checked_add(jiff::SignedDuration::from_secs(61))
            .map_err(ctx("checked_add"))?;
        assert_eq!(adapter.admit(&fourth, &key), Admission::Admitted);
        Ok(())
    }

    #[test]
    fn rate_limit_is_idempotent_across_a_repeated_admit_call_for_one_event() -> TestResult {
        let (_dir, store) = store()?;
        let channel = ChannelId::from_str("telegram:ops");
        pair(&store, &channel, "100", "ops")?;
        let mut config = config(channel);
        config.max_updates_per_peer_per_min = 1;
        let adapter = TelegramChannel::new(config, store);
        let inbound = event("100", true);
        let key = adapter
            .derive_session_key(&inbound)
            .map_err(ctx("derive_session_key"))?;

        // A second `admit()` call for the exact same event (mirroring the
        // gateway's own redundant post-ingress admission re-check) must not
        // consume a second slot of the peer's budget.
        assert_eq!(adapter.admit(&inbound, &key), Admission::Admitted);
        assert_eq!(adapter.admit(&inbound, &key), Admission::Admitted);
        Ok(())
    }

    #[test]
    fn rate_limited_ingress_emits_at_most_one_throttle_notice_per_window() -> TestResult {
        let (_dir, store) = store()?;
        let channel = ChannelId::from_str("telegram:ops");
        pair(&store, &channel, "100", "ops")?;
        let mut config = config(channel);
        config.max_updates_per_peer_per_min = 1;
        let (ingress_tx, ingress_rx) = mpsc::channel();
        let (throttle_tx, throttle_rx) = mpsc::channel();
        let adapter = TelegramChannel::with_ingress_receiver(config, store, ingress_rx)
            .with_throttle_sink(throttle_tx);
        let (sink_tx, sink_rx) = mpsc::channel();

        let mut first = event("100", true);
        first.raw_event_id = Some("1".to_owned());
        let mut second = event("100", true);
        second.raw_event_id = Some("2".to_owned());
        let mut third = event("100", true);
        third.raw_event_id = Some("3".to_owned());
        ingress_tx.send(first).map_err(ctx("send"))?;
        ingress_tx.send(second).map_err(ctx("send"))?;
        ingress_tx.send(third).map_err(ctx("send"))?;
        drop(ingress_tx);

        adapter.run_ingress(sink_tx).map_err(ctx("run_ingress"))?;

        assert_eq!(sink_rx.recv().map_err(ctx("recv"))?.peer.as_str(), "100");
        assert!(sink_rx.try_recv().is_err());

        let notice = throttle_rx
            .recv()
            .map_err(ctx("exactly one throttle notice"))?;
        assert_eq!(notice.peer.as_str(), "100");
        assert!(throttle_rx.try_recv().is_err());
        Ok(())
    }

    #[test]
    fn no_throttle_notice_without_a_configured_sink() -> TestResult {
        let (_dir, store) = store()?;
        let channel = ChannelId::from_str("telegram:ops");
        pair(&store, &channel, "100", "ops")?;
        let mut config = config(channel);
        config.max_updates_per_peer_per_min = 0;
        let (ingress_tx, ingress_rx) = mpsc::channel();
        let adapter = TelegramChannel::with_ingress_receiver(config, store, ingress_rx);
        let (sink_tx, sink_rx) = mpsc::channel();

        ingress_tx.send(event("100", true)).map_err(ctx("send"))?;
        drop(ingress_tx);

        adapter.run_ingress(sink_tx).map_err(ctx("run_ingress"))?;
        assert!(sink_rx.try_recv().is_err());
        Ok(())
    }

    /// Konfiguration, in der der DM-Absender `100` gepinnt, aber (noch)
    /// nicht gepairt ist — der Zustand vor einer In-Channel-Einlösung.
    fn dm_config() -> TelegramChannelConfig {
        let mut config = config(ChannelId::from_str("telegram:ops"));
        config.pinned_sender_ids.insert("100".to_owned());
        config
    }

    /// Privater DM: Absender == Peer (Telegram-DM-Chat-ID == User-ID).
    fn dm_event(update: &str, text: &str) -> InboundEvent {
        let mut inbound = event("100", false);
        inbound.thread = None;
        inbound.sender = Some(SenderRef {
            id: "100".to_owned(),
            display_name: None,
        });
        inbound.text = Some(text.to_owned());
        inbound.raw_event_id = Some(update.to_owned());
        inbound
    }

    fn issue(store: &PairingStore, tenant: &str, seed: &[u8]) -> TestResult<String> {
        store
            .issue_code(
                &ChannelId::from_str("telegram:ops"),
                &TenantId::from_str(tenant),
                seed,
                Timestamp::now(),
            )
            .map_err(ctx("issue"))
    }

    /// Führt die Ingress-Schleife über `events` aus und liefert die an den
    /// Runtime-Sink weitergereichten Events sowie die Pairing-Notices.
    fn run_pairing_ingress(
        adapter_config: TelegramChannelConfig,
        store: Arc<PairingStore>,
        events: Vec<InboundEvent>,
    ) -> TestResult<(TelegramChannel, Vec<InboundEvent>, Vec<PairingNotice>)> {
        let (ingress_tx, ingress_rx) = mpsc::channel();
        let (pairing_tx, pairing_rx) = mpsc::channel();
        let adapter = TelegramChannel::with_ingress_receiver(adapter_config, store, ingress_rx)
            .with_pairing_sink(pairing_tx);
        let (sink_tx, sink_rx) = mpsc::channel();
        for inbound in events {
            ingress_tx.send(inbound).map_err(ctx("send"))?;
        }
        drop(ingress_tx);
        adapter.run_ingress(sink_tx).map_err(ctx("run_ingress"))?;
        let forwarded = sink_rx.try_iter().collect();
        let notices = pairing_rx.try_iter().collect();
        Ok((adapter, forwarded, notices))
    }

    #[test]
    fn pair_command_parser_accepts_only_well_formed_codes() {
        assert_eq!(
            parse_pair_command("/pair Y2GQ-DEYE"),
            Some("Y2GQ-DEYE".to_owned())
        );
        assert_eq!(
            parse_pair_command("  /PAIR@LinLinBot y2gq-deye "),
            Some("Y2GQ-DEYE".to_owned())
        );
        assert_eq!(parse_pair_command("/pair"), None);
        assert_eq!(parse_pair_command("/pair@ Y2GQ-DEYE"), None);
        assert_eq!(parse_pair_command("/pairing Y2GQ-DEYE"), None);
        assert_eq!(parse_pair_command("hey /pair Y2GQ-DEYE"), None);
        assert_eq!(parse_pair_command("/pair Y2GQ-DEYE extra"), None);
        assert_eq!(parse_pair_command("/pair Y2GQDEYE"), None);
        assert_eq!(parse_pair_command("/pair ILOU-ILOU"), None);
        assert_eq!(parse_pair_command("/pair ../../etc"), None);
    }

    #[test]
    fn unpaired_dm_pair_command_redeems_in_channel() -> TestResult {
        let (_dir, store) = store()?;
        let code = issue(&store, "ops", b"seed12345")?;
        let (adapter, forwarded, notices) = run_pairing_ingress(
            dm_config(),
            Arc::clone(&store),
            vec![dm_event("1", &format!("/pair {code}"))],
        )?;

        // Das `/pair`-Kommando selbst erreicht nie die Runtime ...
        assert!(forwarded.is_empty());
        // ... der Peer ist aber nun durable gebunden.
        assert_eq!(
            adapter
                .resolve_tenant(&PeerId::from_str("100"))
                .map_err(ctx("resolve_tenant"))?,
            Some(TenantId::from_str("ops"))
        );
        let [notice] = notices.as_slice() else {
            return Err(TestError::Unexpected(format!(
                "expected one pairing notice, got {}",
                notices.len()
            )));
        };
        assert_eq!(notice.peer.as_str(), "100");
        assert_eq!(
            notice.outcome,
            PairingOutcome::Paired {
                tenant: TenantId::from_str("ops")
            }
        );
        let OutboundContent::Message { markdown } = &notice.content else {
            return Err(TestError::Unexpected("notice must be a message".to_owned()));
        };
        assert!(!markdown.contains(&code));
        Ok(())
    }

    #[test]
    fn pair_command_with_bot_suffix_and_lowercase_code_redeems() -> TestResult {
        let (_dir, store) = store()?;
        let code = issue(&store, "ops", b"seed12345")?;
        let text = format!("/pair@HarwBot {}", code.to_ascii_lowercase());
        let (adapter, forwarded, notices) =
            run_pairing_ingress(dm_config(), store, vec![dm_event("1", &text)])?;

        assert!(forwarded.is_empty());
        assert_eq!(notices.len(), 1);
        assert!(
            adapter
                .resolve_tenant(&PeerId::from_str("100"))
                .map_err(ctx("resolve_tenant"))?
                .is_some()
        );
        Ok(())
    }

    #[test]
    fn invalid_or_reused_pair_code_fails_without_pairing_or_leaking_code() -> TestResult {
        let (_dir, store) = store()?;
        let channel = ChannelId::from_str("telegram:ops");
        let code = issue(&store, "ops", b"seed12345")?;
        // Der Code wurde bereits von einem anderen Peer eingelöst.
        store
            .redeem_once(&channel, &code, &PeerId::from_str("999"), Timestamp::now())
            .map_err(ctx("redeem"))?;

        let (adapter, forwarded, notices) = run_pairing_ingress(
            dm_config(),
            store,
            vec![
                dm_event("1", &format!("/pair {code}")),
                dm_event("2", "/pair ZZZZ-ZZZZ"),
            ],
        )?;

        assert!(forwarded.is_empty());
        assert_eq!(
            adapter
                .resolve_tenant(&PeerId::from_str("100"))
                .map_err(ctx("resolve_tenant"))?,
            None
        );
        assert_eq!(notices.len(), 2);
        for notice in &notices {
            assert_eq!(notice.outcome, PairingOutcome::Failed);
            let OutboundContent::Message { markdown } = &notice.content else {
                return Err(TestError::Unexpected("notice must be a message".to_owned()));
            };
            assert!(!markdown.contains(&code));
            assert!(!markdown.contains("ZZZZ-ZZZZ"));
        }
        Ok(())
    }

    #[test]
    fn deferred_non_pair_and_non_dm_events_are_still_dropped_silently() -> TestResult {
        let (_dir, store) = store()?;
        let code = issue(&store, "ops", b"seed12345")?;
        // Absender weicht vom Peer ab (kein privater DM mit diesem Absender).
        let mut foreign = dm_event("2", &format!("/pair {code}"));
        foreign.sender = Some(SenderRef {
            id: "alice".to_owned(),
            display_name: None,
        });
        let (adapter, forwarded, notices) = run_pairing_ingress(
            dm_config(),
            Arc::clone(&store),
            vec![dm_event("1", "hello"), foreign],
        )?;

        assert!(forwarded.is_empty());
        assert!(notices.is_empty());
        assert_eq!(
            adapter
                .resolve_tenant(&PeerId::from_str("100"))
                .map_err(ctx("resolve_tenant"))?,
            None
        );
        // Der Code blieb unberührt und ist weiterhin einlösbar.
        store
            .redeem_once(
                &ChannelId::from_str("telegram:ops"),
                &code,
                &PeerId::from_str("100"),
                Timestamp::now(),
            )
            .map_err(ctx("code must still be redeemable"))?;
        Ok(())
    }

    #[test]
    fn pair_attempts_are_bounded_by_the_per_sender_rate_limit() -> TestResult {
        let (_dir, store) = store()?;
        let code = issue(&store, "ops", b"seed12345")?;
        let mut adapter_config = dm_config();
        adapter_config.max_updates_per_peer_per_min = 1;
        let (adapter, forwarded, notices) = run_pairing_ingress(
            adapter_config,
            store,
            vec![
                dm_event("1", "/pair ZZZZ-ZZZZ"),
                dm_event("2", &format!("/pair {code}")),
            ],
        )?;

        // Nur der erste Versuch passiert das Rate-Limit; der zweite wird
        // abgewiesen, bevor er den Pairing-Store erreicht.
        assert!(forwarded.is_empty());
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].outcome, PairingOutcome::Failed);
        assert_eq!(
            adapter
                .resolve_tenant(&PeerId::from_str("100"))
                .map_err(ctx("resolve_tenant"))?,
            None
        );
        Ok(())
    }

    #[test]
    fn pair_command_redeems_even_without_a_pairing_sink() -> TestResult {
        let (_dir, store) = store()?;
        let code = issue(&store, "ops", b"seed12345")?;
        let (ingress_tx, ingress_rx) = mpsc::channel();
        let adapter = TelegramChannel::with_ingress_receiver(dm_config(), store, ingress_rx);
        let (sink_tx, sink_rx) = mpsc::channel();
        ingress_tx
            .send(dm_event("1", &format!("/pair {code}")))
            .map_err(ctx("send"))?;
        drop(ingress_tx);

        adapter.run_ingress(sink_tx).map_err(ctx("run_ingress"))?;
        assert!(sink_rx.try_recv().is_err());
        assert!(
            adapter
                .resolve_tenant(&PeerId::from_str("100"))
                .map_err(ctx("resolve_tenant"))?
                .is_some()
        );
        Ok(())
    }
}
