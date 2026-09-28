//! Sliding-window replay protection over per-sender sequence numbers.
//!
//! # Model
//! A [`ReplayWindow`] remembers the highest accepted sequence number and a
//! 128-bit bitmap of the [`REPLAY_WINDOW_BITS`] numbers at and below it
//! (bit `i` = `highest - i` was accepted). A number
//! - above `highest` is new: the window slides forward;
//! - within the window and unmarked is a reordered, new frame: accepted;
//! - within the window and marked is a duplicate: rejected;
//! - below the window is too old to judge: rejected (fail closed).
//!
//! # Check before commit (§9)
//! [`ReplayWindow::check`] does not change state; [`ReplayWindow::commit`]
//! checks and records. A receiver calls `check` early if it wants to shed
//! obvious replays cheaply, but calls `commit` only after the sender
//! signature verified and the frame decrypted — otherwise an attacker could
//! burn sequence numbers with forged frames.
//!
//! [`SenderReplayWindows`] keeps one window per sender with a hard cap on
//! the number of senders, so memory stays bounded.

use std::collections::HashMap;

use crate::error::EncryptError;
use crate::names::NodeId;

/// Width of the replay window in sequence numbers.
pub const REPLAY_WINDOW_BITS: u64 = 128;

/// Replay state of one sender.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplayWindow {
    highest: Option<u64>,
    bitmap: u128,
}

impl ReplayWindow {
    /// An empty window: every sequence number is new.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            highest: None,
            bitmap: 0,
        }
    }

    /// The highest accepted sequence number.
    #[must_use]
    pub const fn highest(&self) -> Option<u64> {
        self.highest
    }

    /// Whether `sequence` would be accepted. Does not change state.
    ///
    /// # Errors
    /// [`EncryptError::ReplayDuplicate`] or [`EncryptError::ReplayTooOld`].
    pub fn check(&self, sequence: u64) -> Result<(), EncryptError> {
        let Some(highest) = self.highest else {
            return Ok(());
        };
        if sequence > highest {
            return Ok(());
        }
        let age = highest - sequence;
        if age >= REPLAY_WINDOW_BITS {
            return Err(EncryptError::ReplayTooOld);
        }
        if self.bitmap & (1u128 << age) != 0 {
            Err(EncryptError::ReplayDuplicate)
        } else {
            Ok(())
        }
    }

    /// Check `sequence` and, if accepted, record it.
    ///
    /// # Errors
    /// As [`Self::check`]; on error the state is unchanged.
    pub fn commit(&mut self, sequence: u64) -> Result<(), EncryptError> {
        self.check(sequence)?;
        match self.highest {
            None => {
                self.highest = Some(sequence);
                self.bitmap = 1;
            }
            Some(highest) if sequence > highest => {
                let shift = sequence - highest;
                self.bitmap = if shift >= REPLAY_WINDOW_BITS {
                    1
                } else {
                    (self.bitmap << shift) | 1
                };
                self.highest = Some(sequence);
            }
            Some(highest) => {
                // `check` guaranteed `highest - sequence < REPLAY_WINDOW_BITS`.
                self.bitmap |= 1u128 << (highest - sequence);
            }
        }
        Ok(())
    }
}

/// Replay windows for many senders, bounded in count.
#[derive(Debug, Clone)]
pub struct SenderReplayWindows {
    windows: HashMap<NodeId, ReplayWindow>,
    max_senders: usize,
}

impl SenderReplayWindows {
    /// Track at most `max_senders` distinct senders.
    #[must_use]
    pub fn new(max_senders: usize) -> Self {
        Self {
            windows: HashMap::new(),
            max_senders,
        }
    }

    /// Whether `sequence` from `sender` would be accepted.
    ///
    /// # Errors
    /// [`ReplayWindow::check`]'s errors, or
    /// [`EncryptError::ReplayTooManySenders`] for an unknown sender when the
    /// capacity is exhausted.
    pub fn check(&self, sender: &NodeId, sequence: u64) -> Result<(), EncryptError> {
        match self.windows.get(sender) {
            Some(window) => window.check(sequence),
            None if self.windows.len() >= self.max_senders => {
                Err(EncryptError::ReplayTooManySenders)
            }
            None => Ok(()),
        }
    }

    /// Check and record `sequence` from `sender`.
    ///
    /// # Errors
    /// As [`Self::check`]; on error nothing is recorded.
    pub fn commit(&mut self, sender: &NodeId, sequence: u64) -> Result<(), EncryptError> {
        self.check(sender, sequence)?;
        self.windows
            .entry(sender.clone())
            .or_default()
            .commit(sequence)
    }

    /// Number of tracked senders.
    #[must_use]
    pub fn len(&self) -> usize {
        self.windows.len()
    }

    /// Whether no sender is tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }
}
