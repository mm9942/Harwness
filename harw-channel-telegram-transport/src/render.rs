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
use harw_channel_telegram::ApprovalTokenStore;

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

/// Telegram outbound renderer.  The cache and pending status values are
/// process-local optimizations; they are never durable conversation state.
pub struct TelegramRenderer {
    client: Arc<TelegramClient>,
    approval_tokens: Arc<ApprovalTokenStore>,
    config: RendererConfig,
    clock: Arc<dyn Clock>,
    state: Mutex<RenderState>,
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
        }
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

fn block_on<F>(future: F) -> TransportResult<()>
where
    F: Future<Output = TransportResult<()>>,
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
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn prepared_approval_callback_data_is_opaque_and_excludes_approval_data() {
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
            panic!("approval must render as a Telegram message");
        };
        assert_eq!(prepared.issued_tokens().len(), 1);
        assert_eq!(
            inline_actions[0].callback_payload,
            prepared.issued_tokens()[0]
        );
        assert!(!inline_actions[0].callback_payload.contains(request_id));
        assert!(!inline_actions[0].callback_payload.contains(decision));
    }

    #[test]
    fn approval_binding_requires_the_actual_send_result_context() {
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
            .expect("binding must receive Telegram's sendMessage result");

        assert!(called);
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

    #[tokio::test]
    async fn draft_strategy_fails_closed_before_network_use() {
        let renderer = renderer(RendererConfig {
            strategy: StreamingStrategy::Draft,
            ..RendererConfig::default()
        });
        let error = renderer
            .send_async(
                7,
                None,
                &OutboundContent::Message {
                    markdown: "draft".to_owned(),
                },
            )
            .await
            .expect_err("Draft must never select an unimplemented transport");
        assert_eq!(error.to_string(), "Telegram Bot API rejected 'draft' (0)");
    }
}
