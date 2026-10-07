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

use harw_protocol::items::{ContentPart, TurnItem};
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

/// Entries kept in the chat view; the oldest are dropped first.
const KEEP_TRANSCRIPT: usize = 500;

/// How a tool call ended, as far as the phone knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    /// Requested, no result yet.
    Running,
    /// Completed successfully.
    Done,
    /// Completed with an error.
    Failed,
}

/// What one chat entry shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntryKind {
    /// What the user (on any device) sent.
    User(String),
    /// What the agent answered.
    Assistant(String),
    /// A tool call.
    Tool {
        /// Tool name.
        name: String,
        /// Where it stands.
        state: ToolState,
    },
    /// A turn-level error item.
    Error(String),
}

/// One line of the chat view. The key is the item or call id, so applying a
/// replayed frame never adds an entry twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    key: String,
    /// What it shows.
    pub kind: EntryKind,
}

impl Entry {
    /// The item or call id this entry was made from.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }
}

/// Plain text of a message; an image shows as a marker.
fn text_of(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } => text.as_str(),
            ContentPart::ImageUrl { .. } | ContentPart::Media { .. } => "[image]",
        })
        .collect::<Vec<_>>()
        .join("\n")
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
    transcript: Vec<Entry>,
    live: Option<(TurnId, String)>,
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
            SessionFrame::Snapshot {
                turn_id,
                assistant_text,
                ..
            } => self.live = Some((turn_id.clone(), assistant_text.clone())),
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
                self.live = None;
            }
            TurnEvent::TurnFailed { reason, .. } => {
                self.activity = Activity::Idle;
                self.live = None;
                self.last_failure = Some(reason.clone());
            }
            TurnEvent::ItemAdded { item, .. } => self.item(item),
            TurnEvent::ToolCallCompleted {
                call_id, result, ..
            } => {
                let state = if result.is_success() {
                    ToolState::Done
                } else {
                    ToolState::Failed
                };
                for entry in &mut self.transcript {
                    if entry.key == call_id.as_str() {
                        if let EntryKind::Tool { state: slot, .. } = &mut entry.kind {
                            *slot = state;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Add the chat entry of `item` unless one with its key exists.
    fn item(&mut self, item: &TurnItem) {
        let (key, kind) = match item {
            TurnItem::UserMessage(m) => (m.id.as_str(), EntryKind::User(text_of(&m.content))),
            TurnItem::AssistantMessage(m) => {
                // The full message replaces the partial text shown so far.
                self.live = None;
                (m.id.as_str(), EntryKind::Assistant(text_of(&m.content)))
            }
            TurnItem::ToolCall(call) => (
                call.call_id.as_str(),
                EntryKind::Tool {
                    name: call.tool_name.clone(),
                    state: ToolState::Running,
                },
            ),
            TurnItem::Error(e) => (e.id.as_str(), EntryKind::Error(e.message.clone())),
            TurnItem::ToolResult(_) | TurnItem::Reasoning(_) => return,
        };
        if self.transcript.iter().any(|entry| entry.key == key) {
            return;
        }
        self.transcript.push(Entry {
            key: key.to_owned(),
            kind,
        });
        if self.transcript.len() > KEEP_TRANSCRIPT {
            self.transcript.remove(0);
        }
    }

    fn reset(&mut self) {
        self.cursor = None;
        self.activity = Activity::Idle;
        self.pending.clear();
        self.decided.clear();
        self.last_failure = None;
        self.transcript.clear();
        self.live = None;
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

    /// The chat so far, oldest first.
    #[must_use]
    pub fn transcript(&self) -> &[Entry] {
        &self.transcript
    }

    /// The agent's partial answer of the running turn, if the host sent one.
    #[must_use]
    pub fn live_text(&self) -> Option<&str> {
        self.live.as_ref().map(|(_, text)| text.as_str())
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

    fn item(durable: u64, item: TurnItem) -> FrameEnvelope {
        at(
            durable,
            SessionFrame::Turn(TurnEvent::ItemAdded {
                turn_id: TurnId::from_str("t1"),
                item,
            }),
        )
    }

    fn assistant(id: &str, text: &str) -> TurnItem {
        TurnItem::AssistantMessage(harw_protocol::items::AssistantMessageItem {
            id: harw_types::ItemId::from_str(id),
            content: vec![ContentPart::Text {
                text: text.to_owned(),
            }],
            phase: None,
        })
    }

    #[test]
    fn the_chat_shows_user_and_assistant_messages_once() -> TestResult {
        let mut view = SessionView::new();
        let user = TurnItem::UserMessage(harw_protocol::items::UserMessageItem {
            id: harw_types::ItemId::from_str("u1"),
            content: vec![ContentPart::Text {
                text: "hi".to_owned(),
            }],
        });
        view.apply(&item(1, user));
        view.apply(&item(2, assistant("m1", "hello")));
        view.apply(&item(2, assistant("m1", "hello")));
        let kinds: Vec<&EntryKind> = view.transcript().iter().map(|e| &e.kind).collect();
        ensure(
            kinds
                == [
                    &EntryKind::User("hi".to_owned()),
                    &EntryKind::Assistant("hello".to_owned()),
                ],
            "two entries, the replayed message is not doubled",
        )
    }

    #[test]
    fn a_tool_call_runs_then_finishes_or_fails() -> TestResult {
        let mut view = SessionView::new();
        let call = |id: &str, name: &str| {
            TurnItem::ToolCall(harw_protocol::items::ToolCallItem {
                id: harw_types::ItemId::from_str(format!("item-{id}")),
                call_id: harw_types::ToolCallId::from_str(id),
                tool_name: name.to_owned(),
                arguments: serde_json::Value::Null,
            })
        };
        view.apply(&item(1, call("c1", "fs.read")));
        view.apply(&item(2, call("c2", "shell")));
        let done = |durable: u64, id: &str, ok: bool| {
            at(
                durable,
                SessionFrame::Turn(TurnEvent::ToolCallCompleted {
                    turn_id: TurnId::from_str("t1"),
                    call_id: harw_types::ToolCallId::from_str(id),
                    result: if ok {
                        harw_protocol::items::ToolCallResult::success(serde_json::Value::Null)
                    } else {
                        harw_protocol::items::ToolCallResult::error("denied")
                    },
                    duration_ms: 3,
                    placement: None,
                }),
            )
        };
        view.apply(&done(3, "c1", true));
        view.apply(&done(4, "c2", false));
        let states: Vec<ToolState> = view
            .transcript()
            .iter()
            .filter_map(|e| match &e.kind {
                EntryKind::Tool { state, .. } => Some(*state),
                _ => None,
            })
            .collect();
        ensure(
            states == [ToolState::Done, ToolState::Failed],
            "each call keeps its own outcome",
        )
    }

    #[test]
    fn partial_text_is_shown_until_the_full_message_or_the_turn_end() -> TestResult {
        let mut view = SessionView::new();
        let snapshot = |durable: u64, text: &str| {
            at(
                durable,
                SessionFrame::Snapshot {
                    turn_id: TurnId::from_str("t1"),
                    assistant_text: text.to_owned(),
                    reasoning_collapsed: true,
                },
            )
        };
        view.apply(&snapshot(1, "Let me"));
        ensure(view.live_text() == Some("Let me"), "partial text")?;
        view.apply(&snapshot(2, "Let me check"));
        ensure(
            view.live_text() == Some("Let me check"),
            "the newest snapshot",
        )?;
        view.apply(&item(3, assistant("m1", "Let me check the files.")));
        ensure(view.live_text().is_none(), "the full message replaces it")?;
        view.apply(&snapshot(4, "next"));
        view.apply(&at(
            5,
            SessionFrame::Turn(TurnEvent::TurnCompleted {
                turn_id: TurnId::from_str("t1"),
                usage: None,
            }),
        ));
        ensure(view.live_text().is_none(), "the turn end clears it")
    }

    #[test]
    fn the_chat_is_bounded_and_drops_the_oldest() -> TestResult {
        let mut view = SessionView::new();
        for n in 0..(KEEP_TRANSCRIPT as u64 + 10) {
            view.apply(&item(n + 1, assistant(&format!("m{n}"), "x")));
        }
        ensure(view.transcript().len() == KEEP_TRANSCRIPT, "bounded")?;
        ensure(
            view.transcript().first().map(|e| e.key.as_str()) == Some("m10"),
            "the oldest were dropped",
        )
    }
}
