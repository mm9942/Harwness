//! The live event stream of a job.
//!
//! [`JobHandle::wait`](super::JobHandle::wait) gives the end result with a
//! bounded head and tail of the output. A surface that shows a running job
//! (terminal, job panel, WebSocket, chat) wants the output **as it happens**:
//! [`JobFrames`] delivers it.
//!
//! # Guarantees
//! - **Lossy by design.** A slow subscriber never slows the job down: frames
//!   go through a bounded broadcast buffer, and a subscriber that falls behind
//!   gets [`FrameEvent::Lagged`] with the number of frames it missed, then
//!   continues with the newest ones. The complete outcome is always in the
//!   [`JobResult`](super::JobResult).
//! - **No cost without a subscriber.** A chunk is only copied into a frame
//!   while somebody is subscribed.
//! - **No loss at the start** for a subscriber requested at submit time
//!   ([`Coordinator::submit_with`](super::Coordinator::submit_with)); a later
//!   [`JobHandle::subscribe`](super::JobHandle::subscribe) sees only what
//!   follows.
//! - The stream ends (`None`) when the job is finished and unregistered.
//!
//! Frames carry the **attempt** they belong to implicitly: every attempt
//! (including a retry) opens with [`JobFrame::AttemptStarted`].

use std::sync::{Arc, Mutex, PoisonError};

use harw_job_core::{AttemptId, ExitOutcome, SandboxReport};
use tokio::sync::broadcast;

/// Frames a job keeps in flight per subscriber before the slowest one lags.
pub const DEFAULT_FRAME_BUFFER: usize = 256;

/// One event of a running job.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum JobFrame {
    /// An attempt was started and counts as running; what is known about its
    /// enforcement is here.
    AttemptStarted {
        /// The attempt.
        attempt_id: AttemptId,
        /// Diagnostic PID of the primary process, if known.
        pid: Option<u32>,
        /// Enforcement known before the body ran.
        sandbox: Option<SandboxReport>,
    },
    /// A chunk of standard output (arbitrary boundaries).
    Stdout(Arc<[u8]>),
    /// A chunk of standard error (arbitrary boundaries).
    Stderr(Arc<[u8]>),
    /// A supervision note of the executor (not output of the job).
    Note(String),
    /// The attempt ended. A retry, if the policy admits one, follows with a
    /// new [`JobFrame::AttemptStarted`].
    AttemptEnded {
        /// How the primary process ended.
        outcome: ExitOutcome,
        /// Enforcement reported only after the process ran.
        sandbox: Option<SandboxReport>,
    },
}

/// What [`JobFrames::next`] yields.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FrameEvent {
    /// A frame.
    Frame(JobFrame),
    /// The subscriber fell behind: this many frames were dropped for it.
    Lagged(u64),
}

/// A subscription to one job's frames.
#[derive(Debug)]
pub struct JobFrames {
    receiver: broadcast::Receiver<JobFrame>,
}

impl JobFrames {
    /// The next event; `None` once the job is finished.
    pub async fn next(&mut self) -> Option<FrameEvent> {
        match self.receiver.recv().await {
            Ok(frame) => Some(FrameEvent::Frame(frame)),
            Err(broadcast::error::RecvError::Lagged(missed)) => Some(FrameEvent::Lagged(missed)),
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }
}

/// The sending side, shared by the coordinator's registry and the handle.
#[derive(Debug, Clone)]
pub(crate) struct FrameTap {
    sender: Arc<Mutex<Option<broadcast::Sender<JobFrame>>>>,
}

impl FrameTap {
    pub(crate) fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(1));
        Self {
            sender: Arc::new(Mutex::new(Some(sender))),
        }
    }

    /// A tap that is already closed (a handle whose result is known).
    pub(crate) fn closed() -> Self {
        Self {
            sender: Arc::new(Mutex::new(None)),
        }
    }

    /// Builds and sends a frame, but only while somebody is subscribed.
    pub(crate) fn emit_with(&self, frame: impl FnOnce() -> JobFrame) {
        let guard = self.sender.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(sender) = guard.as_ref() {
            if sender.receiver_count() > 0 {
                let _ = sender.send(frame());
            }
        }
    }

    /// A new subscription; already closed when the job is over.
    pub(crate) fn subscribe(&self) -> JobFrames {
        let guard = self.sender.lock().unwrap_or_else(PoisonError::into_inner);
        let receiver = match guard.as_ref() {
            Some(sender) => sender.subscribe(),
            None => broadcast::channel(1).1,
        };
        JobFrames { receiver }
    }

    /// Ends every subscription.
    pub(crate) fn close(&self) {
        self.sender
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(text: &str) -> JobFrame {
        JobFrame::Stdout(Arc::from(text.as_bytes()))
    }

    #[tokio::test]
    async fn frames_reach_a_subscriber_in_order_and_the_stream_ends_on_close() {
        let tap = FrameTap::new(8);
        let mut frames = tap.subscribe();
        tap.emit_with(|| out("a"));
        tap.emit_with(|| out("b"));
        tap.close();
        assert_eq!(frames.next().await, Some(FrameEvent::Frame(out("a"))));
        assert_eq!(frames.next().await, Some(FrameEvent::Frame(out("b"))));
        assert_eq!(frames.next().await, None);
    }

    #[tokio::test]
    async fn nothing_is_built_without_a_subscriber() {
        let tap = FrameTap::new(8);
        let built = std::cell::Cell::new(false);
        tap.emit_with(|| {
            built.set(true);
            out("x")
        });
        assert!(!built.get(), "no subscriber, no copy");
    }

    #[tokio::test]
    async fn a_slow_subscriber_lags_instead_of_blocking_the_sender() {
        let tap = FrameTap::new(2);
        let mut frames = tap.subscribe();
        for n in 0..10 {
            tap.emit_with(|| out(&n.to_string()));
        }
        // The sender never waited; the subscriber learns how much it missed.
        assert!(matches!(frames.next().await, Some(FrameEvent::Lagged(missed)) if missed >= 8));
        assert!(matches!(frames.next().await, Some(FrameEvent::Frame(_))));
    }

    #[tokio::test]
    async fn a_closed_tap_hands_out_finished_streams() {
        let tap = FrameTap::closed();
        assert_eq!(tap.subscribe().next().await, None);
        let live = FrameTap::new(4);
        live.close();
        assert_eq!(live.subscribe().next().await, None, "late subscribers too");
    }
}
