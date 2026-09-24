//! Telegram's outbound renderer and its deliberately transient delivery state.
//!
//! This module is the transport side of rendering only.  It does not know
//! about sessions or runtimes: callers give it harness-native outbound
//! content and a Telegram chat id.

use std::{
    collections::{HashMap, VecDeque},
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};

use harw_channel::{ApprovalPrompt, ChannelSendOp, InlineAction, OutboundContent, chunk_text};
use harw_channel_telegram::{
    ApprovalCallbackContext, ApprovalTokenStore, TelegramChatId, TelegramMessageId,
    TelegramThreadId,
};
use harw_types::PeerId;

use crate::{
    client::{InlineKeyboardButton, SentMessage, TelegramClient},
    error::{TelegramTransportError, TransportResult},
};

/// A rendered approval whose callback payloads are opaque tokens issued by an
/// [`ApprovalTokenStore`].
///
/// Call [`TelegramRenderer::send_prepared_approval_async`] to obtain an
/// [`ApprovalDelivery`] containing Telegram's actual message result before
/// binding these tokens to the trusted callback context.
#[derive(Debug)]
pub struct PreparedApproval {
    text: String,
    inline_actions: Vec<InlineAction>,
    issued_tokens: Vec<String>,
}

impl PreparedApproval {
    /// The opaque callback tokens issued for this approval's actions.
    #[must_use]
    pub fn issued_tokens(&self) -> &[String] {
        &self.issued_tokens
    }

    fn operation(&self) -> ChannelSendOp {
        ChannelSendOp::SendMessage {
            text: self.text.clone(),
            inline_actions: self.inline_actions.clone(),
        }
    }
}

/// A delivered approval message and the tokens rendered in its keyboard.
///
/// This value can only be produced after Telegram returns a `sendMessage`
/// result. Its [`Self::bind`] method deliberately receives that result for
/// every token, so callers must derive the remaining trusted callback context
/// (notably the peer) from the same delivery.
#[derive(Debug)]
pub struct ApprovalDelivery {
    message: SentMessage,
    issued_tokens: Vec<String>,
}

impl ApprovalDelivery {
    /// Telegram's authoritative result for the approval message.
    #[must_use]
    pub fn message(&self) -> &SentMessage {
        &self.message
    }

    /// The opaque callback tokens rendered in this delivered message.
    #[must_use]
    pub fn issued_tokens(&self) -> &[String] {
        &self.issued_tokens
    }

    /// Binds every issued token using the actual Telegram delivery result.
    ///
    /// The callback must add the trusted peer and invoke the shared
    /// [`ApprovalTokenStore`] with its complete callback context. There is no
    /// context-free redemption path at this transport boundary.
    pub fn bind<F>(&self, mut bind_token: F) -> Result<(), ApprovalBindingError>
    where
        F: FnMut(&str, &SentMessage) -> bool,
    {
        for (bound, token) in self.issued_tokens.iter().enumerate() {
            if !bind_token(token, &self.message) {
                return Err(ApprovalBindingError::Rejected { bound });
            }
        }
        Ok(())
    }
}

/// A token could not be bound to the message Telegram actually delivered.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ApprovalBindingError {
    /// The binding callback rejected a token after `bound` earlier tokens were
    /// successfully bound.
    Rejected { bound: usize },
}

impl std::fmt::Display for ApprovalBindingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected { .. } => formatter.write_str("approval token binding was rejected"),
        }
    }
}

impl std::fmt::Debug for ApprovalBindingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, formatter)
    }
}

impl std::error::Error for ApprovalBindingError {}

/// Clock used by the token buckets.  Keeping time behind this small trait
/// makes throttling deterministic without sleeping in unit tests.
pub trait Clock: Send + Sync {
    fn now(&self) -> Duration;
}

#[derive(Debug, Default)]
struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START.get_or_init(std::time::Instant::now).elapsed()
    }
}

/// The only streaming mode currently safe to select.  `Draft` is reserved for
/// a future Telegram draft API and is fail-closed when requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamingStrategy {
    ThrottledEdit,
    Draft,
}

/// Bounded, conservative outbound-rendering policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RendererConfig {
    pub max_message_len: usize,
    pub per_chat_per_sec: u32,
    pub global_per_sec: u32,
    pub cache_capacity: usize,
    pub strategy: StreamingStrategy,
}

impl Default for RendererConfig {
    fn default() -> Self {
        Self {
            max_message_len: 4096,
            per_chat_per_sec: 1,
            global_per_sec: 30,
            cache_capacity: 256,
            strategy: StreamingStrategy::ThrottledEdit,
        }
    }
}

#[derive(Debug, Clone)]
struct Bucket {
    tokens: f64,
    at: Duration,
}

impl Bucket {
    fn new(rate: u32, at: Duration) -> Self {
        Self {
            tokens: rate as f64,
            at,
        }
    }

    fn take(&mut self, rate: u32, now: Duration) -> bool {
        let elapsed = now.saturating_sub(self.at).as_secs_f64();
        self.at = now;
        self.tokens = (self.tokens + elapsed * rate as f64).min(rate as f64);
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

#[derive(Debug, Default)]
struct RenderState {
    messages: HashMap<(i64, String), i64>,
    order: VecDeque<(i64, String)>,
    pending: HashMap<(i64, String), String>,
    chat_buckets: HashMap<i64, Bucket>,
    global_bucket: Option<Bucket>,
}

/// Höchstens so lange wartet ein Abschluss (Stream-Ende, Freigabe schließen)
/// auf ein freies Token der lokalen Buckets, bevor er trotzdem sendet.
const FINISH_TOKEN_WAIT: Duration = Duration::from_secs(5);

/// Abstand zwischen zwei Versuchen, ein Bucket-Token zu erhalten.
const TOKEN_POLL_STEP: Duration = Duration::from_millis(100);

/// Präfix der Cache-Schlüssel für Streaming-Blasen, damit sie nie mit
/// `StatusUpdate`-Schlüsseln kollidieren.
const STREAM_KEY_PREFIX: &str = "stream:";

/// Auslassungszeichen für gekürzte Zwischenstände.
const ELLIPSIS: char = '…';

/// Telegram outbound renderer.  The cache and pending status values are
/// process-local optimizations; they are never durable conversation state.
pub struct TelegramRenderer {
    client: Arc<TelegramClient>,
    approval_tokens: Arc<ApprovalTokenStore>,
    config: RendererConfig,
    clock: Arc<dyn Clock>,
    state: Mutex<RenderState>,
    finish_token_wait: Duration,
}

impl TelegramRenderer {
    #[must_use]
    pub fn new(client: Arc<TelegramClient>) -> Self {
        Self::with_config_and_clock_and_approval_tokens(
            client,
            RendererConfig::default(),
            Arc::new(SystemClock),
            Arc::new(ApprovalTokenStore::new()),
        )
    }

    #[must_use]
    pub fn with_config(client: Arc<TelegramClient>, config: RendererConfig) -> Self {
        Self::with_config_and_clock_and_approval_tokens(
            client,
            config,
            Arc::new(SystemClock),
            Arc::new(ApprovalTokenStore::new()),
        )
    }

    /// Konstruiert einen konfigurierten Renderer, der den Freigabe-Token-Store
    /// mit dem Callback-Ingress teilt (ein `Arc` für Adapter und Renderer).
    #[must_use]
    pub fn with_config_and_approval_tokens(
        client: Arc<TelegramClient>,
        config: RendererConfig,
        approval_tokens: Arc<ApprovalTokenStore>,
    ) -> Self {
        Self::with_config_and_clock_and_approval_tokens(
            client,
            config,
            Arc::new(SystemClock),
            approval_tokens,
        )
    }

    /// Constructs a renderer using a token store shared with callback ingress.
    #[must_use]
    pub fn with_approval_tokens(
        client: Arc<TelegramClient>,
        approval_tokens: Arc<ApprovalTokenStore>,
    ) -> Self {
        Self::with_config_and_clock_and_approval_tokens(
            client,
            RendererConfig::default(),
            Arc::new(SystemClock),
            approval_tokens,
        )
    }

    #[must_use]
    pub fn with_config_and_clock(
        client: Arc<TelegramClient>,
        config: RendererConfig,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self::with_config_and_clock_and_approval_tokens(
            client,
            config,
            clock,
            Arc::new(ApprovalTokenStore::new()),
        )
    }

    /// Constructs a fully configured renderer using a token store shared with
    /// callback ingress.
    #[must_use]
    pub fn with_config_and_clock_and_approval_tokens(
        client: Arc<TelegramClient>,
        config: RendererConfig,
        clock: Arc<dyn Clock>,
        approval_tokens: Arc<ApprovalTokenStore>,
    ) -> Self {
        Self {
            client,
            approval_tokens,
            config,
            clock,
            state: Mutex::new(RenderState::default()),
            finish_token_wait: FINISH_TOKEN_WAIT,
        }
    }

    /// Der geteilte Freigabe-Token-Store dieses Renderers.
    #[must_use]
    pub fn approval_tokens(&self) -> &Arc<ApprovalTokenStore> {
        &self.approval_tokens
    }

    /// Issues opaque callback tokens and renders an approval for explicit,
    /// context-bound delivery.
    #[must_use]
    pub fn prepare_approval(&self, prompt: &ApprovalPrompt) -> PreparedApproval {
        let mut issued_tokens = Vec::with_capacity(prompt.actions.len());
        let inline_actions = prompt
            .actions
            .iter()
            .map(|action| {
                let token = self
                    .approval_tokens
                    .issue(&prompt.request_id, &action.decision);
                issued_tokens.push(token.clone());
                InlineAction {
                    label: action.label.clone(),
                    callback_payload: token,
                }
            })
            .collect();
        PreparedApproval {
            text: format!("Approval requested ({})\n{}", prompt.risk, prompt.summary),
            inline_actions,
            issued_tokens,
        }
    }

    /// Sends a prepared approval and returns the actual delivery context that
    /// must be used to bind its issued tokens before callback redemption.
    pub async fn send_prepared_approval_async(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        approval: PreparedApproval,
    ) -> TransportResult<ApprovalDelivery> {
        if self.config.strategy == StreamingStrategy::Draft {
            return Err(draft_error());
        }
        if !self.take_token(chat_id) {
            return Err(rate_error("sendMessage"));
        }
        let keyboard = keyboard(&approval.inline_actions);
        let message = self
            .client
            .send_message(chat_id, &approval.text, thread_id, keyboard.as_deref())
            .await?;
        Ok(ApprovalDelivery {
            message,
            issued_tokens: approval.issued_tokens,
        })
    }

    /// Sendet eine Freigabe und bindet deren Tokens an die tatsächlich
    /// zugestellte Nachricht und an den berechtigten Genehmiger.
    ///
    /// Ablauf: [`Self::prepare_approval`] → [`Self::send_prepared_approval_async`]
    /// → [`ApprovalDelivery::bind`] mit einem [`ApprovalCallbackContext`], dessen
    /// `peer` die Telegram-User-ID des Genehmigers (`approver`) und dessen
    /// `thread_id` der von Telegram gemeldete `message_thread_id` ist.
    ///
    /// Scheitert das Senden oder das Binden, werden alle für
    /// `prompt.request_id` ausgegebenen Tokens widerrufen; ein ungebundener
    /// Button kann danach nie eingelöst werden.
    ///
    /// # Errors
    /// Transport-/API-Fehler beim Senden, lokales Ratenlimit, `Draft`-Strategie
    /// oder eine abgelehnte Token-Bindung.
    pub async fn send_bound_approval_async(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        prompt: &ApprovalPrompt,
        approver: &PeerId,
    ) -> TransportResult<SentMessage> {
        let prepared = self.prepare_approval(prompt);
        let delivery = match self
            .send_prepared_approval_async(chat_id, thread_id, prepared)
            .await
        {
            Ok(delivery) => delivery,
            Err(error) => {
                let _ = self.approval_tokens.revoke_request(&prompt.request_id);
                return Err(error);
            }
        };
        let bound = delivery.bind(|token, message| {
            let mut context = ApprovalCallbackContext::new(
                TelegramChatId(message.chat_id),
                TelegramMessageId(message.message_id),
                approver.clone(),
            );
            if let Some(thread) = message.message_thread_id {
                context = context.with_thread(TelegramThreadId(thread));
            }
            self.approval_tokens.bind(token, context)
        });
        if bound.is_err() {
            let _ = self.approval_tokens.revoke_request(&prompt.request_id);
            // Best effort: tote Schaltflächen entfernen; der Fehler der
            // Bindung bleibt maßgeblich.
            let message = delivery.message();
            let _ = self
                .client
                .edit_message_reply_markup(message.chat_id, message.message_id, None)
                .await;
            return Err(TelegramTransportError::ApiRejected {
                method: "bindApproval",
                code: 0,
                description:
                    "Freigabe-Token konnten nicht an die zugestellte Nachricht gebunden werden"
                        .to_owned(),
            });
        }
        Ok(delivery.message)
    }

    /// Aktualisiert die Streaming-Blase `stream_key` in `chat_id` mit dem
    /// bisherigen Gesamttext.
    ///
    /// Der Text wird auf `max_message_len` Zeichen gekürzt (mit „…“). Ist ein
    /// Bucket leer, wird nur der neueste Stand vorgemerkt (Koaleszieren) und
    /// `Ok` geliefert; der nächste Aufruf oder [`Self::stream_finish_async`]
    /// überträgt dann den aktuellen Text. Telegrams 400 „message is not
    /// modified“ gilt als Erfolg.
    ///
    /// # Errors
    /// Transport-/API-Fehler oder `Draft`-Strategie.
    pub async fn stream_update_async(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        stream_key: &str,
        text: &str,
    ) -> TransportResult<()> {
        if self.config.strategy == StreamingStrategy::Draft {
            return Err(draft_error());
        }
        if text.trim().is_empty() {
            return Ok(());
        }
        let key = stream_cache_key(chat_id, stream_key);
        let latest = truncate_with_ellipsis(text, self.config.max_message_len);
        if !self.take_token(chat_id) {
            self.lock_state().pending.insert(key, latest);
            return Ok(());
        }
        // Der aktuelle Text ist immer neuer als ein vorgemerkter Stand.
        let _ = self.take_pending(&key);
        match self.message_id(&key) {
            Some(message_id) => {
                match self
                    .client
                    .edit_message_text(chat_id, message_id, &latest, None)
                    .await
                {
                    Ok(_) => Ok(()),
                    Err(error) if is_not_modified(&error) => Ok(()),
                    Err(error) => {
                        if is_bad_request(&error) {
                            // Blase gelöscht o. ä.: nächster Stand beginnt neu.
                            self.forget(&key);
                        }
                        Err(error)
                    }
                }
            }
            None => {
                let sent = self
                    .client
                    .send_message(chat_id, &latest, thread_id, None)
                    .await?;
                self.remember(key, sent.message_id);
                Ok(())
            }
        }
    }

    /// Schließt den Stream `stream_key` mit dem vollständigen Endtext ab.
    ///
    /// Wartet höchstens 5 s auf ein Bucket-Token (und sendet danach trotzdem,
    /// weil der Endtext nicht verloren gehen darf; Telegrams 429 behandelt der
    /// Client). Der erste Chunk ersetzt die Streaming-Blase (bzw. wird neu
    /// gesendet, wenn keine existiert oder sie nicht mehr editierbar ist), alle
    /// weiteren Chunks folgen als eigene Nachrichten. Der Schlüssel wird in
    /// jedem Fall vergessen.
    ///
    /// # Errors
    /// Transport-/API-Fehler oder `Draft`-Strategie.
    pub async fn stream_finish_async(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        stream_key: &str,
        text: &str,
    ) -> TransportResult<()> {
        if self.config.strategy == StreamingStrategy::Draft {
            return Err(draft_error());
        }
        let key = stream_cache_key(chat_id, stream_key);
        let message_id = self.message_id(&key);
        self.forget(&key);
        if text.trim().is_empty() {
            return Ok(());
        }
        let mut chunks = chunk_text(text, self.config.max_message_len).into_iter();
        let Some(first) = chunks.next() else {
            return Ok(());
        };
        self.wait_for_token(chat_id).await;
        let edited = match message_id {
            Some(message_id) => match self
                .client
                .edit_message_text(chat_id, message_id, &first, None)
                .await
            {
                Ok(_) => true,
                Err(error) if is_not_modified(&error) => true,
                Err(error) if is_bad_request(&error) => false,
                Err(error) => return Err(error),
            },
            None => false,
        };
        if !edited {
            self.client
                .send_message(chat_id, &first, thread_id, None)
                .await?;
        }
        for chunk in chunks {
            self.wait_for_token(chat_id).await;
            self.client
                .send_message(chat_id, &chunk, thread_id, None)
                .await?;
        }
        Ok(())
    }

    /// Schließt eine zugestellte Freigabe: entfernt zuerst die Tastatur (damit
    /// keine weiteren Klicks möglich sind) und ersetzt dann den Text durch
    /// `outcome_text`. 400 „message is not modified“ gilt jeweils als Erfolg;
    /// beide Schritte werden versucht, der erste echte Fehler wird gemeldet.
    ///
    /// # Errors
    /// Transport-/API-Fehler oder `Draft`-Strategie.
    pub async fn close_approval_async(
        &self,
        chat_id: i64,
        message_id: i64,
        outcome_text: &str,
    ) -> TransportResult<()> {
        if self.config.strategy == StreamingStrategy::Draft {
            return Err(draft_error());
        }
        self.wait_for_token(chat_id).await;
        let markup = self
            .client
            .edit_message_reply_markup(chat_id, message_id, None)
            .await;
        let text = if outcome_text.trim().is_empty() {
            Ok(())
        } else {
            let outcome = truncate_with_ellipsis(outcome_text, self.config.max_message_len);
            self.client
                .edit_message_text(chat_id, message_id, &outcome, None)
                .await
                .map(|_| ())
        };
        tolerate_not_modified(markup)?;
        tolerate_not_modified(text)
    }

    /// Zeigt „schreibt …“ im Chat bzw. Forum-Thema an. Chat-Aktionen belasten
    /// die Nachrichten-Buckets bewusst nicht.
    ///
    /// # Errors
    /// Transport-/API-Fehler.
    pub async fn typing_async(&self, chat_id: i64, thread_id: Option<i64>) -> TransportResult<()> {
        self.client
            .send_chat_action(chat_id, thread_id, "typing")
            .await
    }

    /// Wartet höchstens `finish_token_wait` auf ein Bucket-Token.
    async fn wait_for_token(&self, chat_id: i64) -> bool {
        let mut waited = Duration::ZERO;
        loop {
            if self.take_token(chat_id) {
                return true;
            }
            if waited >= self.finish_token_wait {
                return false;
            }
            let step = TOKEN_POLL_STEP.min(self.finish_token_wait.saturating_sub(waited));
            tokio::time::sleep(step).await;
            waited += step;
        }
    }

    /// Purely maps harness content to Telegram-native operations.
    #[must_use]
    pub fn render(&self, content: &OutboundContent) -> Vec<ChannelSendOp> {
        match content {
            OutboundContent::Message { markdown } => {
                chunk_text(markdown, self.config.max_message_len)
                    .into_iter()
                    .map(|text| ChannelSendOp::SendMessage {
                        text,
                        inline_actions: Vec::new(),
                    })
                    .collect()
            }
            OutboundContent::StatusUpdate {
                status_key,
                markdown,
            } => vec![ChannelSendOp::EditMessage {
                status_key: status_key.clone(),
                text: markdown.clone(),
            }],
            OutboundContent::Approval(prompt) => vec![self.prepare_approval(prompt).operation()],
        }
    }

    /// Sends content, coalescing status deltas whenever either token bucket is
    /// empty.
    ///
    /// Approval callback data is always opaque, including through this legacy
    /// compatibility path. Callers that need callbacks to be redeemable must
    /// instead use [`Self::prepare_approval`] and
    /// [`Self::send_prepared_approval_async`] so they can bind the tokens to
    /// the returned Telegram delivery context.
    pub async fn send_async(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        content: &OutboundContent,
    ) -> TransportResult<()> {
        if self.config.strategy == StreamingStrategy::Draft {
            return Err(draft_error());
        }
        match content {
            OutboundContent::StatusUpdate {
                status_key,
                markdown,
            } => {
                let key = (chat_id, status_key.clone());
                let message_id = self.message_id(&key);
                if !self.take_token(chat_id) {
                    self.state
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .pending
                        .insert(key, markdown.clone());
                    return Ok(());
                }
                let latest = self.take_pending(&key).unwrap_or_else(|| markdown.clone());
                if let Some(message_id) = message_id {
                    self.client
                        .edit_message_text(chat_id, message_id, &latest, None)
                        .await?;
                } else {
                    let sent = self
                        .client
                        .send_message(chat_id, &latest, thread_id, None)
                        .await?;
                    self.remember(key, sent.message_id);
                }
                Ok(())
            }
            _ => {
                for op in self.render(content) {
                    let ChannelSendOp::SendMessage {
                        text,
                        inline_actions,
                    } = op
                    else {
                        continue;
                    };
                    if !self.take_token(chat_id) {
                        return Err(rate_error("sendMessage"));
                    }
                    let keyboard = keyboard(&inline_actions);
                    self.client
                        .send_message(chat_id, &text, thread_id, keyboard.as_deref())
                        .await?;
                }
                Ok(())
            }
        }
    }

    /// Explicitly edits a message.  Oversized content is sent as additional
    /// messages after the first chunk, since Telegram edits are single-message.
    pub async fn edit_async(
        &self,
        chat_id: i64,
        message_id: i64,
        content: &OutboundContent,
    ) -> TransportResult<()> {
        if self.config.strategy == StreamingStrategy::Draft {
            return Err(draft_error());
        }
        let text = match content {
            OutboundContent::Message { markdown }
            | OutboundContent::StatusUpdate { markdown, .. } => markdown,
            OutboundContent::Approval(ApprovalPrompt { summary, .. }) => summary,
        };
        let chunks = chunk_text(text, self.config.max_message_len);
        if !self.take_token(chat_id) {
            return Err(rate_error("editMessageText"));
        }
        self.client
            .edit_message_text(chat_id, message_id, &chunks[0], None)
            .await?;
        for chunk in chunks.into_iter().skip(1) {
            if !self.take_token(chat_id) {
                return Err(rate_error("sendMessage"));
            }
            self.client
                .send_message(chat_id, &chunk, None, None)
                .await?;
        }
        Ok(())
    }

    fn take_token(&self, chat_id: i64) -> bool {
        let now = self.clock.now();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let global_ok = {
            let global = state
                .global_bucket
                .get_or_insert_with(|| Bucket::new(self.config.global_per_sec, now));
            global.take(self.config.global_per_sec, now)
        };
        if !global_ok {
            return false;
        }
        let chat_ok = {
            let chat = state
                .chat_buckets
                .entry(chat_id)
                .or_insert_with(|| Bucket::new(self.config.per_chat_per_sec, now));
            chat.take(self.config.per_chat_per_sec, now)
        };
        if !chat_ok {
            if let Some(global) = state.global_bucket.as_mut() {
                global.tokens = (global.tokens + 1.0).min(self.config.global_per_sec as f64);
            }
            false
        } else {
            true
        }
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, RenderState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Vergisst Blase, Reihenfolge-Eintrag und vorgemerkten Stand eines Schlüssels.
    fn forget(&self, key: &(i64, String)) {
        let mut state = self.lock_state();
        state.messages.remove(key);
        state.pending.remove(key);
        state.order.retain(|existing| existing != key);
    }

    fn message_id(&self, key: &(i64, String)) -> Option<i64> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .messages
            .get(key)
            .copied()
    }

    fn take_pending(&self, key: &(i64, String)) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pending
            .remove(key)
    }

    fn remember(&self, key: (i64, String), message_id: i64) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.messages.contains_key(&key) {
            state.order.retain(|existing| existing != &key);
        }
        state.messages.insert(key.clone(), message_id);
        state.order.push_back(key);
        while state.order.len() > self.config.cache_capacity {
            if let Some(old) = state.order.pop_front() {
                state.messages.remove(&old);
            }
        }
    }
}

fn keyboard(actions: &[InlineAction]) -> Option<Vec<Vec<InlineKeyboardButton>>> {
    if actions.is_empty() {
        return None;
    }
    Some(
        actions
            .iter()
            .map(|action| {
                vec![InlineKeyboardButton {
                    text: action.label.clone(),
                    callback_data: Some(action.callback_payload.clone()),
                    url: None,
                }]
            })
            .collect(),
    )
}

fn rate_error(method: &'static str) -> TelegramTransportError {
    TelegramTransportError::ApiRejected {
        method,
        code: 429,
        description: "local outbound token bucket is full".to_owned(),
    }
}

fn draft_error() -> TelegramTransportError {
    TelegramTransportError::ApiRejected {
        method: "draft",
        code: 0,
        description: "draft streaming is reserved and unavailable".to_owned(),
    }
}

fn stream_cache_key(chat_id: i64, stream_key: &str) -> (i64, String) {
    (chat_id, format!("{STREAM_KEY_PREFIX}{stream_key}"))
}

/// Kürzt `text` auf höchstens `max_chars` Zeichen; gekürzte Texte enden auf „…“.
/// `max_chars == 0` bedeutet „unbegrenzt“ (wie bei [`chunk_text`]).
fn truncate_with_ellipsis(text: &str, max_chars: usize) -> String {
    if max_chars == 0 || text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut truncated: String = text.chars().take(max_chars - 1).collect();
    truncated.push(ELLIPSIS);
    truncated
}

fn is_bad_request(error: &TelegramTransportError) -> bool {
    matches!(error, TelegramTransportError::ApiRejected { code: 400, .. })
}

/// Telegram meldet ein Edit ohne Änderung als 400 „message is not modified“.
fn is_not_modified(error: &TelegramTransportError) -> bool {
    matches!(
        error,
        TelegramTransportError::ApiRejected { code: 400, description, .. }
            if description.to_ascii_lowercase().contains("message is not modified")
    )
}

fn tolerate_not_modified(result: TransportResult<()>) -> TransportResult<()> {
    match result {
        Err(error) if is_not_modified(&error) => Ok(()),
        other => other,
    }
}

fn block_on<T, F>(future: F) -> TransportResult<T>
where
    F: Future<Output = TransportResult<T>>,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(rate_error("synchronous-renderer"));
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(TelegramTransportError::from)?
        .block_on(future)
}

impl crate::TelegramOutbound for TelegramRenderer {
    fn send(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        content: &OutboundContent,
    ) -> TransportResult<()> {
        block_on(self.send_async(chat_id, thread_id, content))
    }

    fn edit(
        &self,
        chat_id: i64,
        message_id: i64,
        content: &OutboundContent,
    ) -> TransportResult<()> {
        block_on(self.edit_async(chat_id, message_id, content))
    }

    fn send_approval(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        prompt: &ApprovalPrompt,
        approver: &PeerId,
    ) -> TransportResult<i64> {
        block_on(self.send_bound_approval_async(chat_id, thread_id, prompt, approver))
            .map(|message| message.message_id)
    }

    fn stream_update(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        stream_key: &str,
        text: &str,
    ) -> TransportResult<()> {
        block_on(self.stream_update_async(chat_id, thread_id, stream_key, text))
    }

    fn stream_finish(
        &self,
        chat_id: i64,
        thread_id: Option<i64>,
        stream_key: &str,
        text: &str,
    ) -> TransportResult<()> {
        block_on(self.stream_finish_async(chat_id, thread_id, stream_key, text))
    }

    fn close_approval(
        &self,
        chat_id: i64,
        message_id: i64,
        outcome_text: &str,
    ) -> TransportResult<()> {
        block_on(self.close_approval_async(chat_id, message_id, outcome_text))
    }

    fn typing(&self, chat_id: i64, thread_id: Option<i64>) -> TransportResult<()> {
        block_on(self.typing_async(chat_id, thread_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_channel::ApprovalAction;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[derive(Debug, Default)]
    struct TestClock(AtomicU64);

    impl Clock for TestClock {
        fn now(&self) -> Duration {
            Duration::from_millis(self.0.load(Ordering::Relaxed))
        }
    }

    fn renderer(config: RendererConfig) -> TelegramRenderer {
        TelegramRenderer::with_config_and_clock(
            Arc::new(TelegramClient::new("test-token")),
            config,
            Arc::new(TestClock::default()),
        )
    }

    #[test]
    fn render_reuses_shared_chunking_and_preserves_unicode_boundaries() {
        let renderer = renderer(RendererConfig {
            max_message_len: 3,
            ..RendererConfig::default()
        });
        let ops = renderer.render(&OutboundContent::Message {
            markdown: "ab🙂cd".to_owned(),
        });
        assert_eq!(ops.len(), 2);
        assert_eq!(
            ops[0],
            ChannelSendOp::SendMessage {
                text: "ab🙂".to_owned(),
                inline_actions: Vec::new(),
            }
        );
    }

    #[test]
    fn cache_is_bounded_and_reuses_latest_status_message() {
        let renderer = renderer(RendererConfig {
            cache_capacity: 2,
            ..RendererConfig::default()
        });
        renderer.remember((1, "a".to_owned()), 11);
        renderer.remember((1, "b".to_owned()), 12);
        renderer.remember((1, "a".to_owned()), 13);
        renderer.remember((1, "c".to_owned()), 14);
        assert_eq!(renderer.message_id(&(1, "a".to_owned())), Some(13));
        assert_eq!(renderer.message_id(&(1, "c".to_owned())), Some(14));
        assert_eq!(renderer.message_id(&(1, "b".to_owned())), None);
    }

    #[test]
    fn token_buckets_gate_per_chat_and_global_traffic() {
        let renderer = renderer(RendererConfig {
            per_chat_per_sec: 1,
            global_per_sec: 2,
            ..RendererConfig::default()
        });
        assert!(renderer.take_token(7));
        assert!(!renderer.take_token(7));
        assert!(renderer.take_token(8));
        assert!(!renderer.take_token(9));
    }

    #[test]
    fn throttled_status_keeps_only_the_latest_pending_delta() {
        let renderer = renderer(RendererConfig::default());
        let key = (7, "turn".to_owned());
        let mut state = renderer
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.pending.insert(key.clone(), "first".to_owned());
        state.pending.insert(key.clone(), "latest".to_owned());
        drop(state);
        assert_eq!(renderer.take_pending(&key).as_deref(), Some("latest"));
    }

    #[test]
    fn prepared_approval_callback_data_is_opaque_and_excludes_approval_data() -> TestResult {
        let renderer = renderer(RendererConfig::default());
        let request_id = "sensitive-request-id";
        let decision = "sensitive-decision";
        let prepared = renderer.prepare_approval(&ApprovalPrompt {
            request_id: request_id.to_owned(),
            summary: "Approve the operation".to_owned(),
            risk: "high".to_owned(),
            actions: vec![ApprovalAction {
                label: "Approve".to_owned(),
                decision: decision.to_owned(),
            }],
        });

        let ChannelSendOp::SendMessage { inline_actions, .. } = prepared.operation() else {
            return Err(TestError::Unexpected(
                "approval must render as a Telegram message".into(),
            ));
        };
        assert_eq!(prepared.issued_tokens().len(), 1);
        assert_eq!(
            inline_actions[0].callback_payload,
            prepared.issued_tokens()[0]
        );
        assert!(!inline_actions[0].callback_payload.contains(request_id));
        assert!(!inline_actions[0].callback_payload.contains(decision));
        Ok(())
    }

    #[test]
    fn approval_binding_requires_the_actual_send_result_context() -> TestResult {
        let delivery = ApprovalDelivery {
            message: SentMessage {
                message_id: 200,
                date: 0,
                chat_id: 100,
                message_thread_id: Some(42),
            },
            issued_tokens: vec!["opaque-token".to_owned()],
        };
        let mut called = false;

        delivery
            .bind(|token, message| {
                called = true;
                assert_eq!(token, "opaque-token");
                assert_eq!(message.chat_id, 100);
                assert_eq!(message.message_id, 200);
                assert_eq!(message.message_thread_id, Some(42));
                true
            })
            .map_err(ctx("binding must receive Telegram's sendMessage result"))?;

        assert!(called);
        Ok(())
    }

    #[test]
    fn rejected_approval_binding_returns_a_typed_error() {
        let delivery = ApprovalDelivery {
            message: SentMessage {
                message_id: 200,
                date: 0,
                chat_id: 100,
                message_thread_id: None,
            },
            issued_tokens: vec!["first".to_owned(), "second".to_owned()],
        };

        assert_eq!(
            delivery.bind(|token, _| token == "first"),
            Err(ApprovalBindingError::Rejected { bound: 1 })
        );
    }

    fn approval_prompt() -> ApprovalPrompt {
        ApprovalPrompt {
            request_id: "turn:req-1".to_owned(),
            summary: "Datei schreiben".to_owned(),
            risk: "medium".to_owned(),
            actions: vec![
                ApprovalAction {
                    label: "Erlauben".to_owned(),
                    decision: "approve".to_owned(),
                },
                ApprovalAction {
                    label: "Ablehnen".to_owned(),
                    decision: "deny".to_owned(),
                },
            ],
        }
    }

    #[test]
    fn truncation_keeps_short_text_and_marks_cut_text_with_ellipsis() {
        assert_eq!(truncate_with_ellipsis("abc", 3), "abc");
        assert_eq!(truncate_with_ellipsis("abcd", 3), "ab…");
        assert_eq!(truncate_with_ellipsis("🙂🙂🙂🙂", 2), "🙂…");
        assert_eq!(truncate_with_ellipsis("abcd", 0), "abcd");
        assert_eq!(truncate_with_ellipsis("abcd", 1), "…");
    }

    #[test]
    fn only_not_modified_bad_requests_are_tolerated() {
        let not_modified = TelegramTransportError::ApiRejected {
            method: "editMessageText",
            code: 400,
            description: "Bad Request: message is not modified: specified new message content \
                          and reply markup are exactly the same"
                .to_owned(),
        };
        let other_bad_request = TelegramTransportError::ApiRejected {
            method: "editMessageText",
            code: 400,
            description: "Bad Request: message to edit not found".to_owned(),
        };
        assert!(is_not_modified(&not_modified));
        assert!(!is_not_modified(&other_bad_request));
        assert!(is_bad_request(&other_bad_request));
        assert!(tolerate_not_modified(Err(not_modified)).is_ok());
        assert!(tolerate_not_modified(Err(other_bad_request)).is_err());
    }

    #[tokio::test]
    async fn throttled_stream_update_coalesces_truncated_latest_text() -> TestResult {
        let renderer = renderer(RendererConfig {
            per_chat_per_sec: 0,
            max_message_len: 4,
            ..RendererConfig::default()
        });
        renderer
            .stream_update_async(7, None, "turn-1", "erst")
            .await
            .map_err(ctx("gedrosselter Zwischenstand darf nicht fehlschlagen"))?;
        renderer
            .stream_update_async(7, None, "turn-1", "neuester Stand")
            .await
            .map_err(ctx("gedrosselter Zwischenstand darf nicht fehlschlagen"))?;
        let key = stream_cache_key(7, "turn-1");
        assert_eq!(renderer.take_pending(&key).as_deref(), Some("neu…"));
        assert_eq!(renderer.take_pending(&key), None);
        Ok(())
    }

    #[test]
    fn stream_keys_do_not_collide_with_status_keys() {
        assert_ne!(stream_cache_key(7, "turn"), (7, "turn".to_owned()));
    }

    #[tokio::test]
    async fn empty_stream_finish_forgets_bubble_without_network() -> TestResult {
        let renderer = renderer(RendererConfig::default());
        let key = stream_cache_key(7, "turn-1");
        renderer.remember(key.clone(), 55);
        renderer
            .lock_state()
            .pending
            .insert(key.clone(), "alt".to_owned());
        renderer
            .stream_finish_async(7, None, "turn-1", "  ")
            .await
            .map_err(ctx("leerer Abschluss darf nicht fehlschlagen"))?;
        assert_eq!(renderer.message_id(&key), None);
        assert_eq!(renderer.take_pending(&key), None);
        assert!(renderer.lock_state().order.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn failed_bound_approval_revokes_every_issued_token() -> TestResult {
        let store = Arc::new(ApprovalTokenStore::new());
        let renderer = TelegramRenderer::with_config_and_clock_and_approval_tokens(
            Arc::new(TelegramClient::new("test-token")),
            RendererConfig {
                per_chat_per_sec: 0,
                ..RendererConfig::default()
            },
            Arc::new(TestClock::default()),
            Arc::clone(&store),
        );
        assert!(Arc::ptr_eq(renderer.approval_tokens(), &store));
        let result = renderer
            .send_bound_approval_async(7, None, &approval_prompt(), &PeerId::from_str("42"))
            .await;
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "ohne Bucket-Token darf keine Freigabe gesendet werden".into(),
            ));
        };
        assert!(matches!(
            error,
            TelegramTransportError::ApiRejected { code: 429, .. }
        ));
        assert_eq!(store.len(), 0);
        Ok(())
    }

    #[tokio::test]
    async fn draft_strategy_fails_closed_for_streaming_and_approvals() -> TestResult {
        let store = Arc::new(ApprovalTokenStore::new());
        let renderer = TelegramRenderer::with_config_and_clock_and_approval_tokens(
            Arc::new(TelegramClient::new("test-token")),
            RendererConfig {
                strategy: StreamingStrategy::Draft,
                ..RendererConfig::default()
            },
            Arc::new(TestClock::default()),
            Arc::clone(&store),
        );
        if renderer
            .stream_update_async(7, None, "k", "text")
            .await
            .is_ok()
            || renderer
                .stream_finish_async(7, None, "k", "text")
                .await
                .is_ok()
            || renderer
                .close_approval_async(7, 1, "erledigt")
                .await
                .is_ok()
            || renderer
                .send_bound_approval_async(7, None, &approval_prompt(), &PeerId::from_str("42"))
                .await
                .is_ok()
        {
            return Err(TestError::Unexpected(
                "Draft muss vor jeder Netzwerknutzung scheitern".into(),
            ));
        }
        assert_eq!(store.len(), 0);
        Ok(())
    }

    #[test]
    fn synchronous_outbound_refuses_to_nest_inside_a_runtime() -> TestResult {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(ctx("Test-Runtime bauen"))?;
        let renderer = renderer(RendererConfig::default());
        let result =
            runtime.block_on(async { crate::TelegramOutbound::typing(&renderer, 7, None) });
        assert!(result.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn draft_strategy_fails_closed_before_network_use() -> TestResult {
        let renderer = renderer(RendererConfig {
            strategy: StreamingStrategy::Draft,
            ..RendererConfig::default()
        });
        let result = renderer
            .send_async(
                7,
                None,
                &OutboundContent::Message {
                    markdown: "draft".to_owned(),
                },
            )
            .await;
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "Draft must never select an unimplemented transport".into(),
            ));
        };
        assert_eq!(error.to_string(), "Telegram Bot API rejected 'draft' (0)");
        Ok(())
    }
}
