//! What one turn amounted to.
//!
//! A client that runs a turn to its end wants a short account: the answer, how
//! many tools ran, how it ended, what it cost. [`TurnTally`] collects that
//! from the **root** session's turn events, one at a time. Whether an event is
//! the root's is the caller's call (child agents have their own events and
//! must not count here).
//!
//! With a prompt queued behind other turns, the client waits for several turn
//! ends; [`TurnTally::finished`] counts them.

use harw_protocol::TurnEvent;
use harw_protocol::items::{AssistantMessageItem, ContentPart, TurnItem};
use harw_types::{MessagePhase, TokenUsage};

/// How a turn ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnEnd {
    /// Finished regularly.
    Completed,
    /// Ended by an interrupt.
    Aborted,
    /// Failed, with the user-safe reason.
    Failed(String),
}

/// The account of a finished turn.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnSummary {
    /// The final answer if there was one, else the last assistant message.
    pub text: Option<String>,
    /// Tool calls of the root session.
    pub tool_calls: u32,
    /// How the turn ended.
    pub end: TurnEnd,
    /// Usage of the turn, if the host reported it.
    pub usage: Option<TokenUsage>,
}

/// The plain text of an assistant message: its text parts joined, images
/// dropped.
#[must_use]
pub fn message_text(message: &AssistantMessageItem) -> String {
    message
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::ImageUrl { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// Collects the root session's turn events.
#[derive(Debug, Clone, Default)]
pub struct TurnTally {
    final_text: Option<String>,
    last_text: Option<String>,
    tool_calls: u32,
    finished: usize,
    end: Option<TurnEnd>,
    usage: Option<TokenUsage>,
}

impl TurnTally {
    /// An empty tally.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Counts one event of the root session.
    pub fn apply_root(&mut self, event: &TurnEvent) {
        match event {
            TurnEvent::ItemAdded {
                item: TurnItem::AssistantMessage(message),
                ..
            } => {
                let text = message_text(message);
                if matches!(message.phase, Some(MessagePhase::FinalAnswer)) {
                    self.final_text = Some(text.clone());
                }
                self.last_text = Some(text);
            }
            TurnEvent::ToolCallRequested { .. } => self.tool_calls += 1,
            TurnEvent::TurnCompleted { usage, .. } => {
                self.finished += 1;
                self.end = Some(TurnEnd::Completed);
                self.usage.clone_from(usage);
            }
            TurnEvent::TurnAborted { .. } => {
                self.finished += 1;
                self.end = Some(TurnEnd::Aborted);
            }
            TurnEvent::TurnFailed { reason, .. } => {
                self.finished += 1;
                self.end = Some(TurnEnd::Failed(reason.clone()));
            }
            _ => {}
        }
    }

    /// How many turn ends were counted.
    #[must_use]
    pub fn finished(&self) -> usize {
        self.finished
    }

    /// The account so far. Without any turn end yet it reads as completed:
    /// ask [`Self::finished`] first.
    #[must_use]
    pub fn summary(self) -> TurnSummary {
        TurnSummary {
            text: self.final_text.or(self.last_text),
            tool_calls: self.tool_calls,
            end: self.end.unwrap_or(TurnEnd::Completed),
            usage: self.usage,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ensure};
    use harw_types::{ItemId, ToolCallId, TurnId};

    fn message(text: &str, phase: Option<MessagePhase>) -> TurnEvent {
        TurnEvent::ItemAdded {
            turn_id: TurnId::from_str("t"),
            item: TurnItem::AssistantMessage(AssistantMessageItem {
                id: ItemId::from_str(format!("m-{text}")),
                content: vec![ContentPart::Text {
                    text: text.to_owned(),
                }],
                phase,
            }),
        }
    }

    fn done() -> TurnEvent {
        TurnEvent::TurnCompleted {
            turn_id: TurnId::from_str("t"),
            usage: None,
        }
    }

    #[test]
    fn the_final_answer_wins_over_a_later_commentary() -> TestResult {
        let mut tally = TurnTally::new();
        tally.apply_root(&message("answer", Some(MessagePhase::FinalAnswer)));
        tally.apply_root(&message("afterthought", Some(MessagePhase::Commentary)));
        tally.apply_root(&done());
        ensure(
            tally.summary().text.as_deref() == Some("answer"),
            "final answer",
        )
    }

    #[test]
    fn without_a_final_answer_the_last_message_is_the_text() -> TestResult {
        let mut tally = TurnTally::new();
        tally.apply_root(&message("first", None));
        tally.apply_root(&message("second", None));
        ensure(
            tally.summary().text.as_deref() == Some("second"),
            "last message",
        )
    }

    #[test]
    fn tool_calls_are_counted_and_each_end_is_counted() -> TestResult {
        let mut tally = TurnTally::new();
        for n in 0..2 {
            tally.apply_root(&TurnEvent::ToolCallRequested {
                turn_id: TurnId::from_str("t"),
                call_id: ToolCallId::from_str(format!("c{n}")),
                tool_name: "fs.read".to_owned(),
                arguments: serde_json::Value::Null,
            });
        }
        ensure(tally.finished() == 0, "not finished")?;
        tally.apply_root(&done());
        tally.apply_root(&done());
        ensure(tally.finished() == 2, "two turn ends (a queued prompt)")?;
        ensure(tally.summary().tool_calls == 2, "two tool calls")
    }

    #[test]
    fn the_ways_a_turn_ends() -> TestResult {
        let mut aborted = TurnTally::new();
        aborted.apply_root(&TurnEvent::TurnAborted {
            turn_id: TurnId::from_str("t"),
        });
        ensure(aborted.summary().end == TurnEnd::Aborted, "aborted")?;
        let mut failed = TurnTally::new();
        failed.apply_root(&TurnEvent::TurnFailed {
            turn_id: TurnId::from_str("t"),
            reason: "boom".to_owned(),
            retryable: false,
        });
        ensure(
            failed.summary().end == TurnEnd::Failed("boom".to_owned()),
            "failed",
        )
    }

    #[test]
    fn an_image_part_is_dropped_from_the_text() -> TestResult {
        let item = AssistantMessageItem {
            id: ItemId::from_str("m"),
            content: vec![
                ContentPart::Text {
                    text: "a".to_owned(),
                },
                ContentPart::ImageUrl {
                    url: "u".to_owned(),
                    detail: None,
                },
                ContentPart::Text {
                    text: "b".to_owned(),
                },
            ],
            phase: None,
        };
        ensure(message_text(&item) == "ab", "text parts joined")
    }
}
