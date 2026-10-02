//! Idempotency keys for `turn.submit`.
//!
//! The host remembers recent `client_msg_id`s and answers a duplicate
//! `Accepted` **without queueing the new prompt**. A client that restarts and
//! counts from 1 again would therefore see its first prompts vanish. Each
//! [`IdempotencyKeys`] carries an epoch (start time plus a per-process
//! instance counter), so keys never repeat across restarts or between two
//! instances created in the same instant.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Instances created in this process.
static INSTANCES: AtomicU64 = AtomicU64::new(0);

/// A fresh epoch: nanoseconds since the Unix epoch in hex, a dot, and the
/// process-wide instance number. A clock set backwards could in theory repeat
/// the first part; the instance number still separates instances of one
/// process.
fn fresh_epoch() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    format!("{nanos:x}.{}", INSTANCES.fetch_add(1, Ordering::Relaxed))
}

/// The host accepts a `client_msg_id` of at most 128 bytes. Label and epoch
/// are bounded so that `label-epoch-n` always fits (48 + 1 + 40 + 1 + 20).
const MAX_LABEL_BYTES: usize = 48;
const MAX_EPOCH_BYTES: usize = 40;

/// At most `max` bytes of `text`, ASCII only: anything else becomes `_`, so
/// the cut can never split a character and the key stays plain text.
fn bounded(text: &str, max: usize) -> String {
    text.chars()
        .map(|c| if c.is_ascii_graphic() { c } else { '_' })
        .take(max)
        .collect()
}

/// A source of keys of the form `label-epoch-n`.
#[derive(Debug, Clone)]
pub struct IdempotencyKeys {
    prefix: String,
    sent: u64,
}

impl IdempotencyKeys {
    /// Keys for the device or client named `label`, with a fresh epoch.
    #[must_use]
    pub fn new(label: &str) -> Self {
        Self::with_epoch(label, &fresh_epoch())
    }

    /// Keys with a given epoch; for tests and for a client that persists its
    /// own epoch.
    #[must_use]
    pub fn with_epoch(label: &str, epoch: &str) -> Self {
        Self {
            prefix: format!(
                "{}-{}",
                bounded(label, MAX_LABEL_BYTES),
                bounded(epoch, MAX_EPOCH_BYTES)
            ),
            sent: 0,
        }
    }

    /// The key of the next prompt. Call it once per prompt and reuse the
    /// result for every retry of that prompt (a retry is the same message).
    pub fn next_key(&mut self) -> String {
        self.sent += 1;
        format!("{}-{}", self.prefix, self.sent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ensure};

    #[test]
    fn keys_count_up_within_one_source() -> TestResult {
        let mut keys = IdempotencyKeys::with_epoch("phone", "e1");
        ensure(keys.next_key() == "phone-e1-1", "first")?;
        ensure(keys.next_key() == "phone-e1-2", "second")
    }

    #[test]
    fn a_restarted_client_never_repeats_a_key() -> TestResult {
        // Same device label, as after an app restart.
        let mut before = IdempotencyKeys::new("phone");
        let mut after = IdempotencyKeys::new("phone");
        let mut all = vec![before.next_key(), before.next_key(), after.next_key()];
        all.sort_unstable();
        all.dedup();
        ensure(all.len() == 3, "three distinct keys")
    }

    #[test]
    fn a_long_or_odd_label_never_breaks_the_host_limit() -> TestResult {
        let label = "ü".repeat(500);
        let mut keys = IdempotencyKeys::new(&label);
        let key = keys.next_key();
        ensure(key.len() <= 128, "within 128 bytes")?;
        ensure(key.is_ascii(), "plain ASCII")?;
        let mut long_epoch = IdempotencyKeys::with_epoch(&"a".repeat(500), &"b".repeat(500));
        ensure(long_epoch.next_key().len() <= 128, "epoch bounded too")
    }

    #[test]
    fn the_label_stays_readable_in_the_key() -> TestResult {
        let mut keys = IdempotencyKeys::new("harw-mobile");
        ensure(keys.next_key().starts_with("harw-mobile-"), "label prefix")
    }
}
