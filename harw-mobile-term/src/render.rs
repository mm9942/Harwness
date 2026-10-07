//! What to print when the view changed. Pure: it returns lines.
//!
//! The terminal is a phone, so lines are short (word-wrapped, a hanging
//! indent) and plain ASCII; nothing depends on colour or cursor movement.

use harw_mobile_core::{Activity, EntryKind, SessionView, ToolState};
use harw_types::ReviewDecision;

/// Default line width on a phone terminal.
pub const DEFAULT_WIDTH: usize = 40;

/// Remembers what was already shown, so each call prints only what is new.
#[derive(Debug, Default)]
pub struct Printer {
    width: usize,
    entries: Vec<(String, Option<ToolState>)>,
    asked: Vec<String>,
    decided: Vec<String>,
    was_running: bool,
}

/// Word-wraps `text` after `prefix`; continuation lines are indented.
fn wrap(prefix: &str, text: &str, width: usize) -> Vec<String> {
    let indent = " ".repeat(prefix.chars().count().min(4));
    let mut lines = Vec::new();
    for (n, paragraph) in text.split('\n').enumerate() {
        let mut line = if n == 0 {
            prefix.to_owned()
        } else {
            indent.clone()
        };
        let mut empty = true;
        for word in paragraph.split_whitespace() {
            let add = word.chars().count() + usize::from(!empty);
            if !empty && line.chars().count() + add > width {
                lines.push(std::mem::replace(&mut line, indent.clone()));
                empty = true;
            }
            if !empty {
                line.push(' ');
            }
            line.push_str(word);
            empty = false;
        }
        lines.push(line);
    }
    lines
}

fn tool_word(state: ToolState) -> &'static str {
    match state {
        ToolState::Running => "running",
        ToolState::Done => "ok",
        ToolState::Failed => "failed",
    }
}

impl Printer {
    /// A printer wrapping at `width` columns.
    #[must_use]
    pub fn new(width: usize) -> Self {
        Self {
            width: width.max(20),
            ..Self::default()
        }
    }

    /// The lines to print for what changed since the last call.
    pub fn update(&mut self, view: &SessionView) -> Vec<String> {
        let mut out = Vec::new();
        let running = matches!(view.activity(), Activity::Running(_));
        if running && !self.was_running {
            // A turn that starts is shown before what it produced.
            out.push("- working".to_owned());
        }
        for entry in view.transcript() {
            let state = match &entry.kind {
                EntryKind::Tool { state, .. } => Some(*state),
                _ => None,
            };
            match self.entries.iter_mut().find(|(key, _)| key == entry.key()) {
                None => {
                    self.entries.push((entry.key().to_owned(), state));
                    match &entry.kind {
                        EntryKind::User(text) => out.extend(wrap("you: ", text, self.width)),
                        EntryKind::Assistant(text) => {
                            out.extend(wrap("agent: ", text, self.width));
                        }
                        EntryKind::Tool { name, state } => {
                            out.push(format!("* {name} {}", tool_word(*state)))
                        }
                        EntryKind::Error(text) => out.extend(wrap("! ", text, self.width)),
                    }
                }
                Some((_, shown)) => {
                    if let (EntryKind::Tool { name, state }, Some(old)) = (&entry.kind, *shown) {
                        if old != *state {
                            out.push(format!("* {name} {}", tool_word(*state)));
                            *shown = Some(*state);
                        }
                    }
                }
            }
        }
        for (n, request) in view.pending_approvals().enumerate() {
            let id = request.id.as_str().to_owned();
            if self.asked.contains(&id) {
                continue;
            }
            out.extend(wrap(
                &format!("? [{}] ", n + 1),
                &format!(
                    "{} (risk {})",
                    request.summary,
                    format!("{:?}", request.risk).to_lowercase()
                ),
                self.width,
            ));
            out.push("  /y approve, /n reject".to_owned());
            self.asked.push(id);
        }
        for done in view.decided() {
            let id = done.request_id.as_str().to_owned();
            if self.decided.contains(&id) {
                continue;
            }
            let word = match done.decision {
                ReviewDecision::Rejected => "rejected",
                ReviewDecision::Approved | ReviewDecision::ApprovedOnce => "approved",
            };
            out.push(format!("- {id} {word} by {}", done.by));
            self.decided.push(id);
        }
        if !running && self.was_running {
            match view.last_failure() {
                Some(reason) => out.extend(wrap("! turn failed: ", reason, self.width)),
                None => out.push("- done".to_owned()),
            }
        }
        self.was_running = running;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ensure};

    #[test]
    fn wrapping_keeps_words_whole_and_indents_continuations() -> TestResult {
        let lines = wrap("agent: ", "one two three four five six seven", 16);
        ensure(
            lines.iter().all(|l| l.chars().count() <= 16),
            "within width",
        )?;
        ensure(lines[0].starts_with("agent: "), "prefix on the first line")?;
        ensure(
            lines.iter().skip(1).all(|l| l.starts_with("    ")),
            "hanging indent",
        )?;
        let joined: Vec<&str> = lines
            .iter()
            .flat_map(|l| l.split_whitespace())
            .filter(|w| *w != "agent:")
            .collect();
        ensure(
            joined == ["one", "two", "three", "four", "five", "six", "seven"],
            "no word is lost or split",
        )
    }

    #[test]
    fn paragraphs_keep_their_breaks() -> TestResult {
        let lines = wrap("agent: ", "first\nsecond", 40);
        ensure(lines.len() == 2, "one line per paragraph")?;
        ensure(lines[1].trim() == "second", "second paragraph")
    }

    #[test]
    fn an_over_long_word_is_not_split() -> TestResult {
        let lines = wrap(
            "agent: ",
            "https://example.test/a/very/long/path/that/does/not/fit",
            20,
        );
        ensure(
            lines
                .iter()
                .any(|l| l.contains("https://example.test/a/very/long/path/that/does/not/fit")),
            "kept whole so it can be copied",
        )
    }

    use harw_protocol::items::{
        AssistantMessageItem, ContentPart, ToolCallItem, ToolCallResult, TurnItem,
    };
    use harw_protocol::session_wire::{Cursor, FrameEnvelope, SessionFrame};
    use harw_protocol::{ApprovalKind, ApprovalRequest, TurnEvent};
    use harw_types::{
        ApprovalId, ItemId, RiskLevel, SessionId, ThreadId, ToolCallId, TurnId, WorkId,
    };

    fn frame(durable: u64, frame: SessionFrame) -> FrameEnvelope {
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

    fn turn(durable: u64, event: TurnEvent) -> FrameEnvelope {
        frame(durable, SessionFrame::Turn(event))
    }

    fn approval(id: &str, summary: &str) -> SessionFrame {
        let now = jiff::Timestamp::UNIX_EPOCH;
        SessionFrame::ApprovalRequested(ApprovalRequest {
            id: ApprovalId::from_str(id),
            work_id: WorkId::from_str("w"),
            kind: ApprovalKind::DynamicTool {
                turn_id: TurnId::from_str("t"),
                tool_name: "shell".to_owned(),
                arguments: serde_json::Value::Null,
            },
            summary: summary.to_owned(),
            risk: RiskLevel::High,
            requested_at: now,
            timeout_at: ApprovalRequest::default_timeout_at(now),
            decisions: vec![ReviewDecision::Approved, ReviewDecision::Rejected],
        })
    }

    #[test]
    fn only_what_is_new_is_printed_and_nothing_twice() -> TestResult {
        let mut view = SessionView::new();
        let mut printer = Printer::new(40);
        ensure(
            printer.update(&view).is_empty(),
            "an empty view prints nothing",
        )?;

        view.apply(&turn(
            1,
            TurnEvent::TurnStarted {
                turn_id: TurnId::from_str("t1"),
                thread_id: ThreadId::from_str("th"),
            },
        ));
        view.apply(&turn(
            2,
            TurnEvent::ItemAdded {
                turn_id: TurnId::from_str("t1"),
                item: TurnItem::AssistantMessage(AssistantMessageItem {
                    id: ItemId::from_str("m1"),
                    content: vec![ContentPart::Text {
                        text: "I will look.".to_owned(),
                    }],
                    phase: None,
                }),
            },
        ));
        let first = printer.update(&view);
        ensure(
            first == ["- working", "agent: I will look."],
            "working and the message",
        )?;
        ensure(
            printer.update(&view).is_empty(),
            "the second call prints nothing",
        )
    }

    #[test]
    fn a_tool_is_shown_running_then_once_more_with_its_outcome() -> TestResult {
        let mut view = SessionView::new();
        let mut printer = Printer::new(40);
        view.apply(&turn(
            1,
            TurnEvent::ItemAdded {
                turn_id: TurnId::from_str("t1"),
                item: TurnItem::ToolCall(ToolCallItem {
                    id: ItemId::from_str("i1"),
                    call_id: ToolCallId::from_str("c1"),
                    tool_name: "fs.read".to_owned(),
                    arguments: serde_json::Value::Null,
                }),
            },
        ));
        ensure(printer.update(&view) == ["* fs.read running"], "running")?;
        view.apply(&turn(
            2,
            TurnEvent::ToolCallCompleted {
                turn_id: TurnId::from_str("t1"),
                call_id: ToolCallId::from_str("c1"),
                result: ToolCallResult::error("denied"),
                duration_ms: 1,
                placement: None,
            },
        ));
        ensure(printer.update(&view) == ["* fs.read failed"], "failed")?;
        ensure(printer.update(&view).is_empty(), "and then quiet")
    }

    #[test]
    fn approvals_are_numbered_announced_once_and_closed_with_who_decided() -> TestResult {
        let mut view = SessionView::new();
        let mut printer = Printer::new(40);
        view.apply(&frame(1, approval("a1", "run ls")));
        view.apply(&frame(2, approval("a2", "delete tmp")));
        let asked = printer.update(&view);
        ensure(
            asked.iter().any(|l| l == "? [1] run ls (risk high)"),
            "first card",
        )?;
        ensure(
            asked.iter().any(|l| l.starts_with("? [2] delete tmp")),
            "second card",
        )?;
        ensure(printer.update(&view).is_empty(), "announced once")?;
        view.apply(&frame(
            3,
            SessionFrame::ApprovalResolved {
                request_id: ApprovalId::from_str("a1"),
                decision: ReviewDecision::Rejected,
                by: "tablet".to_owned(),
            },
        ));
        ensure(
            printer.update(&view) == ["- a1 rejected by tablet"],
            "who decided, once",
        )
    }

    #[test]
    fn a_failed_turn_shows_its_reason() -> TestResult {
        let mut view = SessionView::new();
        let mut printer = Printer::new(40);
        view.apply(&turn(
            1,
            TurnEvent::TurnStarted {
                turn_id: TurnId::from_str("t1"),
                thread_id: ThreadId::from_str("th"),
            },
        ));
        printer.update(&view);
        view.apply(&turn(
            2,
            TurnEvent::TurnFailed {
                turn_id: TurnId::from_str("t1"),
                reason: "provider unreachable".to_owned(),
                retryable: true,
            },
        ));
        ensure(
            printer.update(&view) == ["! turn failed: provider unreachable"],
            "the reason is shown",
        )
    }
}
