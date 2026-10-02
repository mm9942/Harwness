//! `harw-mobile-core` (experimental): the view model a phone client renders.
//!
//! A pure state machine: feed it the [`FrameEnvelope`]s of one attached
//! session and read the state back. It performs no I/O and owns no transport,
//! so the same model drives a terminal UI and a Rust GUI, and tests need no
//! runtime. Which approvals the user may resolve is decided by the host
//! (device opt-in); this crate only shows what the host sends.

#![forbid(unsafe_code)]

pub mod controller;
#[cfg(test)]
mod test_support;

pub use controller::Controller;

use harw_protocol::session_wire::{Cursor, FrameEnvelope, PresenceEntry, SessionFrame};
use harw_protocol::{ApprovalRequest, TurnEvent};
use harw_types::{ApprovalId, ReviewDecision, TurnId};

/// What the client must do after a frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Nothing; re-render.
    None,
    /// Drop the view and re-attach from this cursor.
    Reattach(Cursor),
}

/// A decided approval, kept for the "recent" list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decided {
    /// The request that was decided.
    pub request_id: ApprovalId,
    /// The decision.
    pub decision: ReviewDecision,
    /// Host-derived label of who decided.
    pub by: String,
}

/// Turn activity of the root session.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Activity {
    /// No turn running.
    #[default]
    Idle,
    /// A turn is running.
    Running(TurnId),
}

/// The state of one attached session.
#[derive(Debug, Clone, Default)]
pub struct SessionView {
    cursor: Option<Cursor>,
    activity: Activity,
    pending: Vec<ApprovalRequest>,
    decided: Vec<Decided>,
    attached: Vec<PresenceEntry>,
    last_failure: Option<String>,
}

/// Decided approvals kept for display.
const KEEP_DECIDED: usize = 20;

impl SessionView {
    /// An empty view (before the first frame).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Apply one frame. Frames strictly behind the current cursor (an older
    /// generation, or an earlier position in this one) are ignored: they are a
    /// late replay. Frames at the same position are applied, and applying a
    /// frame twice is harmless because each step is idempotent.
    pub fn apply(&mut self, envelope: &FrameEnvelope) -> Action {
        if let Some(seen) = &self.cursor {
            if is_stale(seen, &envelope.cursor) {
                return Action::None;
            }
        }
        self.cursor = Some(envelope.cursor);
        match &envelope.frame {
            SessionFrame::Turn(event) => self.turn(event),
            SessionFrame::ApprovalRequested(request) => {
                self.pending.retain(|open| open.id != request.id);
                self.pending.push(request.clone());
            }
            SessionFrame::ApprovalResolved {
                request_id,
                decision,
                by,
            } => {
                self.pending.retain(|open| &open.id != request_id);
                self.decided.retain(|done| &done.request_id != request_id);
                self.decided.push(Decided {
                    request_id: request_id.clone(),
                    decision: *decision,
                    by: by.clone(),
                });
                if self.decided.len() > KEEP_DECIDED {
                    self.decided.remove(0);
                }
            }
            SessionFrame::Presence { attached } => self.attached.clone_from(attached),
            SessionFrame::Resync { head, .. } => {
                self.reset();
                return Action::Reattach(*head);
            }
            SessionFrame::Lagged { resume_from } => {
                return Action::Reattach(*resume_from);
            }
            _ => {}
        }
        Action::None
    }

    fn turn(&mut self, event: &TurnEvent) {
        match event {
            TurnEvent::TurnStarted { turn_id, .. } => {
                self.activity = Activity::Running(turn_id.clone());
                self.last_failure = None;
            }
            TurnEvent::TurnCompleted { .. } | TurnEvent::TurnAborted { .. } => {
                self.activity = Activity::Idle;
            }
            TurnEvent::TurnFailed { reason, .. } => {
                self.activity = Activity::Idle;
                self.last_failure = Some(reason.clone());
            }
            _ => {}
        }
    }

    fn reset(&mut self) {
        self.cursor = None;
        self.activity = Activity::Idle;
        self.pending.clear();
        self.decided.clear();
        self.last_failure = None;
    }

    /// Cursor of the last applied frame, to resume from after a reconnect.
    #[must_use]
    pub fn cursor(&self) -> Option<&Cursor> {
        self.cursor.as_ref()
    }

    /// Current turn activity.
    #[must_use]
    pub fn activity(&self) -> &Activity {
        &self.activity
    }

    /// Open approvals, oldest first.
    pub fn pending_approvals(&self) -> impl Iterator<Item = &ApprovalRequest> {
        self.pending.iter()
    }

    /// Recently decided approvals, oldest first.
    #[must_use]
    pub fn decided(&self) -> &[Decided] {
        &self.decided
    }

    /// Who is attached.
    #[must_use]
    pub fn attached(&self) -> &[PresenceEntry] {
        &self.attached
    }

    /// Reason of the last failed turn, cleared when the next turn starts.
    #[must_use]
    pub fn last_failure(&self) -> Option<&str> {
        self.last_failure.as_deref()
    }
}

/// `true` when `next` is strictly behind `seen`: an older generation, or an
/// earlier position within the same generation. Equal positions are not
/// stale: a cursor is a stream position, not a frame id, and the host sends
/// several frames at the same head (every pending approval, a snapshot,
/// presence). Replays stay harmless because every reducer step is idempotent.
fn is_stale(seen: &Cursor, next: &Cursor) -> bool {
    match next.generation.cmp(&seen.generation) {
        std::cmp::Ordering::Less => true,
        std::cmp::Ordering::Greater => false,
        std::cmp::Ordering::Equal => (next.durable, next.live) < (seen.durable, seen.live),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_protocol::ApprovalKind;
    use harw_types::{RiskLevel, SessionId, ThreadId, WorkId};

    use crate::test_support::{TestResult, ensure};

    fn at(durable: u64, frame: SessionFrame) -> FrameEnvelope {
        FrameEnvelope {
            session_id: SessionId::from_str("s1"),
            cursor: Cursor {
                generation: 1,
                durable,
                live: 0,
            },
            frame,
        }
    }

    fn request(id: &str) -> ApprovalRequest {
        let now = jiff::Timestamp::UNIX_EPOCH;
        ApprovalRequest {
            id: ApprovalId::from_str(id),
            work_id: WorkId::from_str("w1"),
            kind: ApprovalKind::DynamicTool {
                turn_id: TurnId::from_str("t1"),
                tool_name: "shell".to_owned(),
                arguments: serde_json::Value::Null,
            },
            summary: "run ls".to_owned(),
            risk: RiskLevel::Medium,
            requested_at: now,
            timeout_at: ApprovalRequest::default_timeout_at(now),
            decisions: vec![ReviewDecision::Approved, ReviewDecision::Rejected],
        }
    }

    #[test]
    fn approval_request_then_resolution_clears_the_inbox() -> TestResult {
        let mut view = SessionView::new();
        view.apply(&at(1, SessionFrame::ApprovalRequested(request("a1"))));
        ensure(view.pending_approvals().count() == 1, "one open approval")?;
        view.apply(&at(
            2,
            SessionFrame::ApprovalResolved {
                request_id: ApprovalId::from_str("a1"),
                decision: ReviewDecision::Approved,
                by: "phone".to_owned(),
            },
        ));
        ensure(view.pending_approvals().count() == 0, "inbox empty")?;
        ensure(view.decided().len() == 1, "kept in recent list")
    }

    #[test]
    fn replays_are_idempotent_and_older_positions_are_ignored() -> TestResult {
        let mut view = SessionView::new();
        let frame = at(5, SessionFrame::ApprovalRequested(request("a1")));
        view.apply(&frame);
        view.apply(&frame);
        ensure(view.pending_approvals().count() == 1, "replay is harmless")?;
        view.apply(&at(4, SessionFrame::ApprovalRequested(request("a2"))));
        ensure(
            view.pending_approvals().count() == 1,
            "older position ignored",
        )?;
        ensure(view.cursor().map(|c| c.durable) == Some(5), "cursor stays")?;
        let resolved = at(
            6,
            SessionFrame::ApprovalResolved {
                request_id: ApprovalId::from_str("a1"),
                decision: ReviewDecision::Approved,
                by: "phone".to_owned(),
            },
        );
        view.apply(&resolved);
        view.apply(&resolved);
        ensure(
            view.decided().len() == 1,
            "a replayed decision is kept once",
        )
    }

    #[test]
    fn frames_at_the_same_cursor_are_all_applied() -> TestResult {
        // The host sends every pending approval with the same head cursor.
        let mut view = SessionView::new();
        view.apply(&at(5, SessionFrame::ApprovalRequested(request("a1"))));
        view.apply(&at(5, SessionFrame::ApprovalRequested(request("a2"))));
        view.apply(&at(5, SessionFrame::ApprovalRequested(request("a3"))));
        ensure(
            view.pending_approvals().count() == 3,
            "equal positions are not stale",
        )
    }

    #[test]
    fn a_frame_from_an_older_generation_is_ignored() -> TestResult {
        let mut view = SessionView::new();
        let newer = FrameEnvelope {
            session_id: SessionId::from_str("s1"),
            cursor: Cursor {
                generation: 2,
                durable: 0,
                live: 0,
            },
            frame: SessionFrame::ApprovalRequested(request("a2")),
        };
        view.apply(&newer);
        view.apply(&at(99, SessionFrame::ApprovalRequested(request("old"))));
        ensure(view.pending_approvals().count() == 1, "rollback ignored")?;
        ensure(
            view.cursor().map(|c| c.generation) == Some(2),
            "generation stays",
        )
    }

    #[test]
    fn turn_lifecycle_drives_activity_and_failure() -> TestResult {
        let mut view = SessionView::new();
        view.apply(&at(
            1,
            SessionFrame::Turn(TurnEvent::TurnStarted {
                turn_id: TurnId::from_str("t1"),
                thread_id: ThreadId::from_str("th"),
            }),
        ));
        ensure(
            *view.activity() == Activity::Running(TurnId::from_str("t1")),
            "running",
        )?;
        view.apply(&at(
            2,
            SessionFrame::Turn(TurnEvent::TurnFailed {
                turn_id: TurnId::from_str("t1"),
                reason: "boom".to_owned(),
                retryable: false,
            }),
        ));
        ensure(*view.activity() == Activity::Idle, "idle after failure")?;
        ensure(view.last_failure() == Some("boom"), "failure shown")
    }

    #[test]
    fn resync_resets_the_view_and_asks_to_reattach() -> TestResult {
        let mut view = SessionView::new();
        view.apply(&at(1, SessionFrame::ApprovalRequested(request("a1"))));
        let head = Cursor {
            generation: 2,
            durable: 0,
            live: 0,
        };
        let action = view.apply(&at(
            2,
            SessionFrame::Resync {
                reason: "compaction".to_owned(),
                head,
            },
        ));
        ensure(action == Action::Reattach(head), "reattach from head")?;
        ensure(view.pending_approvals().count() == 0, "view dropped")?;
        ensure(view.cursor().is_none(), "cursor dropped")
    }

    #[test]
    fn a_new_generation_is_never_stale() -> TestResult {
        let mut view = SessionView::new();
        view.apply(&at(9, SessionFrame::ApprovalRequested(request("a1"))));
        let newer = FrameEnvelope {
            session_id: SessionId::from_str("s1"),
            cursor: Cursor {
                generation: 2,
                durable: 0,
                live: 0,
            },
            frame: SessionFrame::ApprovalRequested(request("a2")),
        };
        view.apply(&newer);
        ensure(
            view.pending_approvals().count() == 2,
            "generation 2 applied",
        )
    }
}
