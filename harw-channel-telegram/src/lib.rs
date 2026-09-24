//! Telegram's capability-reducing channel binding.
//!
//! Diese Crate enthält die reine Grenzlogik des Telegram-Kanals, die vor jedem
//! Transport korrekt sein muss: Absender-Zulassung (Pinning, optional
//! ungepinntes DM-Pairing), In-Channel-Pairing, Gruppen-Erwähnungs-Gates,
//! Topic-Session-Partitionierung, Rate-Limits, einmalige Approval-Callback-
//! Tokens mit TTL, Work-Requests, Chat-Zustand je Session und den lokalen
//! Anhangs-Cache. Der HTTP-Client (Bot-API, Long-Polling, Webhook, Rendering)
//! lebt in `harw-channel-telegram-transport`; diese Crate löst selbst keine
//! Credentials auf und öffnet keine Netzwerkverbindungen.

#![forbid(unsafe_code)]

mod approval_tokens;
mod attachment_cache;
mod chat_state;
mod config;
mod error;
mod sandbox;
mod telegram;
mod work_request;

pub use approval_tokens::{
    ApprovalCallbackContext, ApprovalTokenStore, MAX_OUTSTANDING_APPROVAL_TOKENS, PendingApproval,
    TelegramChatId, TelegramMessageId, TelegramThreadId,
};
pub use attachment_cache::{AttachmentCache, CachedAttachment};
pub use chat_state::{ChatState, ChatStateStore, telegram_session_id};
pub use config::{
    DEFAULT_ATTACHMENT_MAX_BYTES, DEFAULT_ATTACHMENT_MAX_COUNT,
    DEFAULT_MAX_UPDATES_PER_PEER_PER_MIN, TelegramChannelConfig, TopicMode,
};
pub use error::{TelegramChannelError, TelegramChannelResult};
pub use sandbox::TelegramSandbox;
pub use telegram::{PairingNotice, PairingOutcome, TelegramChannel, ThrottleNotice};
pub use work_request::{
    ApprovedWorkRequest, LaunchReceipt, TELEGRAM_WORK_REQUEST_JOB_KIND, WorkLaunchError,
    WorkLauncher, WorkRequestActor, WorkRequestRecord, WorkRequestState, WorkRequestStore,
    launch_sandboxed_worker,
};

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
