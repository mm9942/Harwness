//! Meldet am Turn-Ende den Text der letzten Nutzernachricht und der letzten
//! Assistentenantwort an den [`crate::capture::ToolOutcomeObserver`].
//!
//! Grundlage der Rückmeldung zu gelieferten Gedächtnisfakten (genutzt oder
//! korrigiert). Nur Text; leere Texte werden nicht gemeldet.

use harw_protocol::items::{ContentPart, TurnItem};

use crate::session::AgentSession;

fn text_of(parts: &[ContentPart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text.as_str()),
            ContentPart::ImageUrl { .. } | ContentPart::Media { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Der Text der letzten Nutzernachricht und der letzten Assistentenantwort
/// nach ihr (jeweils `None`, wenn leer oder nicht vorhanden).
pub(crate) fn latest_turn_texts(items: &[TurnItem]) -> (Option<String>, Option<String>) {
    let mut user = None;
    let mut assistant = None;
    for item in items.iter().rev() {
        match item {
            TurnItem::AssistantMessage(message) if assistant.is_none() && user.is_none() => {
                let text = text_of(&message.content);
                if !text.trim().is_empty() {
                    assistant = Some(text);
                }
            }
            TurnItem::UserMessage(message) if user.is_none() => {
                let text = text_of(&message.content);
                if !text.trim().is_empty() {
                    user = Some(text);
                }
                break;
            }
            _ => {}
        }
    }
    (user, assistant)
}

/// Ruft `on_user_message`/`on_assistant_message` des registrierten
/// Beobachters auf (kein Beobachter: nichts).
pub(crate) fn notify_turn_texts(session: &AgentSession) {
    let Some(observer) = session.tool_outcome_observer() else {
        return;
    };
    let (user, assistant) = latest_turn_texts(session.history().items());
    if let Some(text) = user {
        observer.on_user_message(session.id(), &text);
    }
    if let Some(text) = assistant {
        observer.on_assistant_message(session.id(), &text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_protocol::items::{AssistantMessageItem, UserMessageItem};
    use harw_types::ItemId;

    fn user(text: &str) -> TurnItem {
        TurnItem::UserMessage(UserMessageItem {
            id: ItemId::new(),
            content: vec![ContentPart::Text {
                text: text.to_owned(),
            }],
        })
    }

    fn assistant(text: &str) -> TurnItem {
        TurnItem::AssistantMessage(AssistantMessageItem {
            id: ItemId::new(),
            content: vec![ContentPart::Text {
                text: text.to_owned(),
            }],
            phase: None,
        })
    }

    #[test]
    fn picks_the_last_user_message_and_the_answer_after_it() {
        let items = [
            user("first"),
            assistant("answer one"),
            user("second"),
            assistant("answer two"),
        ];
        let (u, a) = latest_turn_texts(&items);
        assert_eq!(u.as_deref(), Some("second"));
        assert_eq!(a.as_deref(), Some("answer two"));
    }

    #[test]
    fn an_empty_history_or_empty_texts_report_nothing() {
        assert_eq!(latest_turn_texts(&[]), (None, None));
        let (u, a) = latest_turn_texts(&[user("   "), assistant("")]);
        assert_eq!((u, a), (None, None));
    }

    #[test]
    fn an_answer_before_the_latest_user_message_is_not_reported() {
        let (u, a) = latest_turn_texts(&[assistant("old"), user("new question")]);
        assert_eq!(u.as_deref(), Some("new question"));
        assert_eq!(a, None);
    }
}
