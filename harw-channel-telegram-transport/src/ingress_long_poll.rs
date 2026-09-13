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

    while !shutdown.is_requested() {
        let updates = client
            .get_updates(persisted_offset, timeout_secs, &allowed_updates)
            .await?;

        for value in updates {
            if shutdown.is_requested() {
                return Ok(());
            }

            let update = decode_update(value)?;
            let safely_processed = match map_update(&update, bot.id, bot.username.as_deref()) {
                Some(mut event) if dedup.claim_update(update.update_id) => {
                    event.channel = ChannelId::from_str(channel_id.clone());
                    send_event(&sender, event)?;
                    true
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

fn decode_update(value: serde_json::Value) -> TransportResult<RawUpdate> {
    serde_json::from_value(value).map_err(|error| TelegramTransportError::MalformedUpdate {
        update_id: None,
        reason: error.to_string(),
    })
}

fn send_event(sender: &SyncSender<InboundEvent>, event: InboundEvent) -> TransportResult<()> {
    sender.try_send(event).map_err(|error| match error {
        TrySendError::Full(_) => TelegramTransportError::Io(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "Telegram long-poll ingress sink is full",
        )),
        TrySendError::Disconnected(_) => TelegramTransportError::Io(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "Telegram long-poll ingress sink is disconnected",
        )),
    })
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
    use super::{next_offset_after_safe_processing, restart_offset};

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
}
