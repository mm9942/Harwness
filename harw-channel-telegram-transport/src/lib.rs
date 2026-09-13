//! Real, token-owning Telegram Bot API transport feeding the pure
//! `harw-channel-telegram` admission boundary.
//!
//! This crate owns every piece of network I/O for Telegram: the Bot API HTTP
//! client, long-poll and webhook ingress loops, wire-format mapping, replay
//! dedup (perf-only — the durable gate is `harw_channel::PairingStore`),
//! throttled outbound rendering, and attachment intake. It deliberately
//! never touches `harw-core`/`harw-sandbox`/`harw-session-store` — actually
//! running an agent turn for an admitted event is a separate, out-of-scope
//! "runtime bridge" that consumes the [`hand_off::AdmittedEventConsumer`]
//! and implements [`hand_off::TelegramOutbound`] against this crate's
//! [`render::TelegramRenderer`].
//!
//! It additionally owns [`TelegramMirrorTransport`] (AW7-04): a
//! fire-and-forget binding of `harw_secrets::audit::mirror::MirrorTransport`
//! onto [`TelegramClient`], used as the out-of-band audit-chain mirror's
//! second network path. See that module's docs for why Telegram qualifies as
//! "out of band" here and what it does not protect against.

#![forbid(unsafe_code)]

mod client;
mod dedup;
mod error;
mod hand_off;
mod ingress_long_poll;
mod ingress_webhook;
mod mapping;
mod media;
mod mirror;
mod offset;
mod render;

pub use client::{
    BotCommand, BotInfo, InlineKeyboardButton, SentMessage, TelegramClient, TelegramFile,
};
pub use dedup::DedupWindow;
pub use error::{OffsetPersistenceOperation, TelegramTransportError, TransportResult};
pub use hand_off::{AdmittedEventConsumer, TelegramOutbound};
pub use ingress_long_poll::{LongPollConfig, LongPollShutdown, spawn_long_poll_thread};
pub use ingress_webhook::{WebhookConfig, run_webhook_server, webhook_router};
pub use mapping::{
    RawCallbackQuery, RawChat, RawDocument, RawMedia, RawMessage, RawMessageEntity, RawPhotoSize,
    RawUpdate, RawUser, TelegramCommand, map_update, parse_command,
};
pub use media::{AttachmentIntake, DownloadedAttachment};
pub use mirror::TelegramMirrorTransport;
pub use offset::TelegramOffsetStore;
pub use render::TelegramRenderer;
