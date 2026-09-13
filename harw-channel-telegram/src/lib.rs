//! Telegram's capability-reducing channel binding.
//!
//! The crate intentionally has no HTTP client yet. It owns the pure boundary
//! logic that must be correct before transport work begins: tenant admission,
//! group mention gates, topic session partitioning, and safe outbound rendering.

#![forbid(unsafe_code)]

mod approval_tokens;
mod config;
mod error;
mod sandbox;
mod telegram;

pub use approval_tokens::{ApprovalTokenStore, PendingApproval};
pub use config::{TelegramChannelConfig, TopicMode};
pub use error::{TelegramChannelError, TelegramChannelResult};
pub use sandbox::TelegramSandbox;
pub use telegram::TelegramChannel;
