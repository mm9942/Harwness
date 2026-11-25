use super::*;
use harw_protocol::items::{AssistantMessageItem, ContentPart, ErrorItem, UserMessageItem};
use harw_types::ItemId;
use serde_json::json;

fn item(id: &str, text: &str, user: bool) -> serde_json::Value {
    let id = ItemId::try_from_str(id).expect("test item ID");
    let content = vec![ContentPart::Text { text: text.into() }];
    let item = if user {
        TurnItem::UserMessage(UserMessageItem { id, content })
    } else {
        TurnItem::AssistantMessage(AssistantMessageItem {
            id,
            content,
            phase: None,
        })
    };
    serde_json::to_value(item).expect("typed item")
}

fn fork(
    records: Vec<(RecordKind, serde_json::Value)>,
    target: &str,
) -> Result<Vec<TurnItem>, HostError> {
    let sid = SessionId::try_from_str("fork-test-source").expect("test session ID");
    let committed_head = records.len() as u64;
    let records = records.into_iter().enumerate().map(|(n, (kind, payload))| {
        Ok(TranscriptRecord::new(
            sid.clone(),
            harw_types::ThreadRef::from_str("single-source"),
            n as u64,
            jiff::Timestamp::now(),
            kind,
            payload,
        ))
    });
    canonical_fork_prefix(records, &sid, target, committed_head)
}

#[test]
fn exact_user_boundary_carries_only_visible_message_items() {
    let error = TurnItem::Error(ErrorItem {
        id: ItemId::try_from_str("internal").expect("test item ID"),
        message: "do not import diagnostics".into(),
        retryable: false,
    });
    let records = vec![
        (RecordKind::Item, item("u1", "first", true)),
        (
            RecordKind::Item,
            serde_json::to_value(error).expect("error item"),
        ),
        (RecordKind::Item, item("a1", "visible", false)),
        (RecordKind::Item, item("u2", "selected stays draft", true)),
        (RecordKind::Item, item("a2", "unselected", false)),
    ];
    let result = fork(records, "u2").expect("exact fork");
    assert_eq!(result.len(), 2);
    assert!(matches!(&result[0], TurnItem::UserMessage(m) if m.id.as_str() == "u1"));
    assert!(matches!(&result[1], TurnItem::AssistantMessage(m) if m.id.as_str() == "a1"));
}

#[test]
fn replacement_rejects_archived_target_and_accepts_exact_current_prefix() {
    let records = vec![
        (RecordKind::Item, item("old", "old suffix", true)),
        (
            RecordKind::Lifecycle,
            json!({"marker": "history_replace_started", "item_count": 2}),
        ),
        (RecordKind::Item, item("u1", "current first", true)),
        (RecordKind::Item, item("u2", "current boundary", true)),
        (
            RecordKind::Lifecycle,
            json!({"marker": "history_replaced", "item_count": 2}),
        ),
    ];
    assert!(fork(records.clone(), "old").is_err());
    assert!(
        matches!(&fork(records, "u2").expect("committed replacement")[..], [TurnItem::UserMessage(m)] if m.id.as_str() == "u1")
    );
}

#[test]
fn incomplete_replacement_or_duplicate_boundary_never_falls_back() {
    let unique = (RecordKind::Item, item("u1", "one", true));
    assert!(fork(vec![unique.clone(), unique.clone()], "u1").is_err());
    assert!(
        fork(
            vec![
                unique.clone(),
                (
                    RecordKind::Lifecycle,
                    json!({"marker": "history_replace_started", "item_count": 1})
                )
            ],
            "u1"
        )
        .is_err()
    );
    assert!(
        fork(
            vec![
                unique,
                (
                    RecordKind::Lifecycle,
                    json!({"marker": "history_replaced", "item_count": 99})
                )
            ],
            "u1"
        )
        .is_err()
    );
}

#[test]
fn equal_user_text_with_distinct_images_keeps_both_canonical_records() {
    let user = |id: &str, digest: char| {
        let mut payload = item(id, "same caption", true);
        payload["content"]
            .as_array_mut()
            .expect("content list")
            .push(json!({
                "type": "media",
                "media": {
                    "digest": std::iter::repeat_n(digest, 64).collect::<String>(),
                    "format": "png", "bytes": 100, "width": 10, "height": 10
                }
            }));
        payload
    };
    let first = user("u1", 'a');
    let second = user("u2", 'b');
    let selected = item("u3", "not in the seed", true);
    let result = fork(
        vec![
            (RecordKind::Item, first.clone()),
            (RecordKind::Item, second.clone()),
            (RecordKind::Item, selected),
        ],
        "u3",
    )
    .expect("exact fork");
    assert_eq!(
        serde_json::to_value(result).expect("serialize"),
        json!([first, second])
    );
}
