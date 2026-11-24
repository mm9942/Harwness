use super::*;
use harw_protocol::items::{
    AssistantMessageItem, ContentPart, ErrorItem, TurnItem, UserMessageItem,
};
use harw_protocol::media::ImageFormat;
use harw_session_store::{RecordKind, TranscriptRecord};
use harw_types::{ContentDigest, ThreadRef};
use serde_json::{Value, json};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn id(text: &str) -> ItemId {
    ItemId::try_from_str(text).expect("fixture ID")
}

fn time(sequence: u64) -> jiff::Timestamp {
    jiff::Timestamp::from_second(1_700_000_000_i64 + sequence as i64).expect("fixture time")
}

fn fixture_media(hex: &str) -> MediaRef {
    MediaRef::new(
        ContentDigest::of(hex.as_bytes()),
        ImageFormat::Png,
        10,
        10,
        10,
    )
    .expect("fixture media")
}

fn record(sid: &SessionId, sequence: u64, kind: RecordKind, payload: Value) -> TranscriptRecord {
    TranscriptRecord::new(
        sid.clone(),
        ThreadRef::from_str("fixture-thread"),
        sequence,
        time(sequence),
        kind,
        payload,
    )
}

fn item(sid: &SessionId, sequence: u64, item: TurnItem) -> TranscriptRecord {
    record(
        sid,
        sequence,
        RecordKind::Item,
        serde_json::to_value(item).expect("fixture item"),
    )
}

fn mark(sid: &SessionId, sequence: u64, marker: &str, item_count: usize) -> TranscriptRecord {
    record(
        sid,
        sequence,
        RecordKind::Lifecycle,
        json!({"marker": marker, "item_count": item_count}),
    )
}

fn user(name: &str, text: &str, media: Option<MediaRef>) -> TurnItem {
    let mut content = vec![ContentPart::Text { text: text.into() }];
    if let Some(media) = media {
        content.push(ContentPart::Media {
            media,
            detail: None,
        });
        content.push(ContentPart::ImageUrl {
            url: "https://ignored.invalid/image".into(),
            detail: None,
        });
    }
    TurnItem::UserMessage(UserMessageItem {
        id: id(name),
        content,
    })
}

fn assistant(name: &str, text: &str) -> TurnItem {
    TurnItem::AssistantMessage(AssistantMessageItem {
        id: id(name),
        content: vec![ContentPart::Text { text: text.into() }],
        phase: None,
    })
}

fn checkpoint_next(n: u64) -> NonZeroU64 {
    NonZeroU64::new(n).expect("nonempty fixture journal")
}

#[test]
fn archive_prefix_preserves_exact_occurrences_beyond_ui_tail_across_later_branches() -> TestResult {
    let dir = tempfile::tempdir()?;
    let store = TranscriptStore::new(dir.path());
    let sid = SessionId::new();
    let first = fixture_media("a");
    let distinct = fixture_media("b");
    let mut early = vec![mark(&sid, 0, "history_replace_started", 4)];
    early.push(item(
        &sid,
        1,
        user("first", "same text", Some(first.clone())),
    ));
    early.push(item(
        &sid,
        2,
        user("second", "same text", Some(distinct.clone())),
    ));
    early.push(item(&sid, 3, assistant("early-reply", "preserved")));
    early.push(item(
        &sid,
        4,
        TurnItem::Error(ErrorItem {
            id: id("hidden"),
            message: "not part of export".into(),
            retryable: false,
        }),
    ));
    early.push(mark(&sid, 5, "history_replaced", 4));
    for n in 6..=225 {
        early.push(item(&sid, n, user(&format!("u-{n}"), "mid", None)));
    }
    early.push(item(&sid, 226, assistant("archived-click", "through here")));
    early.push(item(&sid, 227, user("old-later", "not copied", None)));
    store.append_group(&early)?;
    let anchor = capture_checkpoint_v1(&store, &sid, checkpoint_next(228)).expect("closed source");
    let stored_checkpoint = serde_json::to_vec(&anchor)?;

    store.append_group(&[
        mark(&sid, 228, "history_replace_started", 2),
        item(&sid, 229, user("first", "different later occurrence", None)),
        item(&sid, 230, assistant("branch-answer", "new branch")),
        mark(&sid, 231, "history_replaced", 2),
        item(&sid, 232, user("later", "next branch extension", None)),
        mark(&sid, 233, "history_replace_started", 1),
        item(
            &sid,
            234,
            assistant("latest-branch-answer", "second replacement"),
        ),
        mark(&sid, 235, "history_replaced", 1),
    ])?;
    drop(store);

    let restarted = TranscriptStore::new(dir.path());
    let anchor: ArchivedCheckpointV1 = serde_json::from_slice(&stored_checkpoint)?;
    let export = export_inclusive_v1(
        &restarted,
        &sid,
        &anchor,
        &ArchivedRowV1 {
            sequence: 226,
            id: id("archived-click"),
        },
    )
    .expect("retained old prefix is exact after reopen");
    assert_eq!(export.through, 226);
    assert_eq!(export.rows.len(), 224);
    assert_eq!(
        export.rows.first().map(|r| r.source_recorded_at),
        Some(time(1))
    );
    assert_eq!(
        export.rows.last().map(|r| r.source_recorded_at),
        Some(time(226))
    );
    assert_eq!(
        export.rows.last().map(|r| &r.id),
        Some(&id("archived-click"))
    );
    for (row, expected) in export.rows[..2].iter().zip([first, distinct]) {
        assert!(matches!(&row.message, ExportMessageV1::User { content }
            if content == &vec![ExportUserPartV1::Text { text: "same text".into() },
                ExportUserPartV1::StoredMedia {media: expected, detail: None}]));
    }
    let exported = serde_json::to_string(&export)?;
    for absent in [
        "ignored.invalid",
        "not part of export",
        "old-later",
        "branch-answer",
        "next branch extension",
        "second replacement",
    ] {
        assert!(!exported.contains(absent));
    }

    assert_eq!(
        export_inclusive_v1(
            &restarted,
            &sid,
            &anchor,
            &ArchivedRowV1 {
                sequence: 4,
                id: id("hidden"),
            }
        ),
        Err(ArchivedForkError::ClickNotVisible),
        "an internal record cannot be the clicked message"
    );
    let newer =
        capture_checkpoint_v1(&restarted, &sid, checkpoint_next(233)).expect("new revision");
    let same_id = export_inclusive_v1(
        &restarted,
        &sid,
        &newer,
        &ArchivedRowV1 {
            sequence: 229,
            id: id("first"),
        },
    )
    .expect("new occurrence of reused item ID");
    assert_eq!(same_id.rows.len(), 1);
    assert_eq!(
        same_id.rows[0].source_recorded_at,
        time(229),
        "a reused ID carries the exact selected occurrence timestamp, not a claimed first appearance"
    );
    let latest =
        capture_checkpoint_v1(&restarted, &sid, checkpoint_next(236)).expect("latest revision");
    assert_eq!(
        export_inclusive_v1(
            &restarted,
            &sid,
            &latest,
            &ArchivedRowV1 {
                sequence: 229,
                id: id("first")
            }
        ),
        Err(ArchivedForkError::ClickNotVisible)
    );
    assert_eq!(
        export_inclusive_v1(
            &restarted,
            &sid,
            &newer,
            &ArchivedRowV1 {
                sequence: 226,
                id: id("archived-click")
            }
        ),
        Err(ArchivedForkError::ClickNotVisible)
    );
    Ok(())
}

#[test]
fn archive_prefix_rejects_wrong_scope_changed_records_truncation_and_wrong_row() -> TestResult {
    let dir = tempfile::tempdir()?;
    let store = TranscriptStore::new(dir.path());
    let sid = SessionId::new();
    let other = SessionId::new();
    let seed = |word| {
        [
            mark(&sid, 0, "history_replace_started", 1),
            item(&sid, 1, user("first", word, None)),
            mark(&sid, 2, "history_replaced", 1),
        ]
    };
    store.append_group(&seed("original"))?;
    let anchor =
        capture_checkpoint_v1(&store, &sid, checkpoint_next(3)).expect("original checkpoint");
    let row = ArchivedRowV1 {
        sequence: 1,
        id: id("first"),
    };
    assert_eq!(
        export_inclusive_v1(&store, &other, &anchor, &row),
        Err(ArchivedForkError::WrongSource)
    );
    assert_eq!(
        export_inclusive_v1(
            &store,
            &sid,
            &anchor,
            &ArchivedRowV1 {
                sequence: 1,
                id: id("not-first")
            }
        ),
        Err(ArchivedForkError::ClickNotVisible)
    );
    store.rewrite(&sid, &seed("same ID and position, changed data"))?;
    assert_eq!(
        export_inclusive_v1(&store, &sid, &anchor, &row),
        Err(ArchivedForkError::Changed)
    );
    let mut retimed = seed("original");
    retimed[1].recorded_at = time(99);
    store.rewrite(&sid, &retimed)?;
    assert_eq!(
        export_inclusive_v1(&store, &sid, &anchor, &row),
        Err(ArchivedForkError::Changed)
    );
    store.rewrite(&sid, &seed("original")[..2])?;
    assert_eq!(
        export_inclusive_v1(&store, &sid, &anchor, &row),
        Err(ArchivedForkError::Unavailable)
    );
    Ok(())
}

#[test]
fn archive_prefix_excludes_open_unframed_and_oversized_ranges() -> TestResult {
    let dir = tempfile::tempdir()?;
    let store = TranscriptStore::new(dir.path());
    let sid = SessionId::new();
    store.append_group(&[
        item(&sid, 0, user("normal", "first", None)),
        mark(&sid, 1, "history_replace_started", 1),
        item(&sid, 2, user("selected", "inside pending", None)),
        mark(&sid, 3, "history_replaced", 1),
        mark(&sid, 4, "history_replaced", 1),
    ])?;
    assert_eq!(
        capture_checkpoint_v1(&store, &sid, checkpoint_next(1)),
        Err(ArchivedForkError::MissingFrame)
    );
    assert_eq!(
        capture_checkpoint_v1(&store, &sid, checkpoint_next(3)),
        Err(ArchivedForkError::InvalidJournal)
    );
    assert_eq!(
        capture_checkpoint_v1(&store, &sid, checkpoint_next(5)),
        Err(ArchivedForkError::UnverifiedRewrite)
    );
    assert_eq!(
        capture_checkpoint_v1(&store, &sid, checkpoint_next(32_769)),
        Err(ArchivedForkError::ExceedsLimit)
    );
    Ok(())
}

#[test]
fn discovered_pre_edit_is_original_visible_version_not_rewritten_prefix() -> TestResult {
    let dir = tempfile::tempdir()?;
    let store = TranscriptStore::new(dir.path());
    let sid = SessionId::new();
    let a = fixture_media("original-a");
    let b = fixture_media("original-b");
    let first = user("first", "same", Some(a.clone()));
    let mut rows = vec![
        item(&sid, 0, first.clone()),
        item(&sid, 1, user("second", "same", Some(b.clone()))),
    ];
    for sequence in 2..222 {
        rows.push(item(
            &sid,
            sequence,
            assistant(&format!("reply-{sequence}"), "old"),
        ));
    }
    let mut start = mark(&sid, 222, START, 1);
    start.payload["rewind_target_item_id"] = json!("second");
    rows.extend([
        start,
        item(&sid, 223, first),
        mark(&sid, 224, END, 1),
        item(&sid, 225, user("later", "not part of prior view", None)),
        mark(&sid, 226, START, 1),
        item(&sid, 227, assistant("compacted", "new")),
        mark(&sid, 228, END, 1),
    ]);
    store.append_group(&rows)?;
    // A physically retained complete successor is not sufficient until the
    // corresponding writer-proven horizon includes its END.
    assert_eq!(
        capture_previous_rewrite_v1(&store, &sid, checkpoint_next(222), None),
        Ok(None)
    );
    assert_eq!(
        capture_previous_rewrite_v1(&store, &sid, checkpoint_next(224), None),
        Err(ArchivedForkError::InvalidJournal)
    );
    let (latest, _) = capture_previous_rewrite_v1(&store, &sid, checkpoint_next(229), None)
        .expect("verified latest snapshot")
        .expect("generic replacement");
    assert!(!latest.explicit_edit);
    assert_eq!(latest.before_sequence.get(), 226);
    let (prior, proof) = capture_previous_rewrite_v1(
        &store,
        &sid,
        checkpoint_next(229),
        Some(latest.before_sequence),
    )
    .expect("verified earlier snapshot")
    .expect("explicit edit");
    assert!(prior.explicit_edit);
    assert_eq!(prior.before_sequence.get(), 222);
    assert_eq!(prior.recorded_at, time(224));
    let serialized = serde_json::to_vec(&proof)?;
    drop(store);

    let store = TranscriptStore::new(dir.path());
    let proof = serde_json::from_slice(&serialized)?;
    let (tail, next) =
        page_checkpoint_v1(&store, &sid, &proof, None, 4).expect("latest prior-version page");
    assert_eq!(tail[3].locator.sequence, 221);
    assert_eq!(tail[3].recorded_at, time(221));
    assert_eq!(next, Some(218));
    let seed = export_inclusive_v1(
        &store,
        &sid,
        &proof,
        &ArchivedRowV1 { sequence: 221, id: id("reply-221") },
    )
    .expect("exact old inclusive branch");
    assert_eq!(seed.rows.len(), 222);
    assert_eq!(seed.media_refs().collect::<Vec<_>>(), vec![&a, &b]);
    assert_eq!(
        export_inclusive_v1(
            &store,
            &sid,
            &proof,
            &ArchivedRowV1 { sequence: 223, id: id("first") },
        ),
        Err(ArchivedForkError::ClickNotVisible),
        "same item ID in rewritten prefix is not the selected occurrence"
    );
    Ok(())
}

#[test]
fn mislabeled_or_unframed_history_never_becomes_a_verified_edit() -> TestResult {
    let dir = tempfile::tempdir()?;
    let store = TranscriptStore::new(dir.path());
    let sid = SessionId::new();
    let mut start = mark(&sid, 2, START, 1);
    start.payload["rewind_target_item_id"] = json!("second");
    store.append_group(&[
        item(&sid, 0, user("first", "original", None)),
        item(&sid, 1, user("second", "before", None)),
        start,
        item(&sid, 3, user("first", "changed though ID equals", None)),
        mark(&sid, 4, END, 1),
    ])?;
    let (generic, _) = capture_previous_rewrite_v1(&store, &sid, checkpoint_next(5), None)
        .expect("framed rewrite")
        .expect("generic predecessor");
    assert!(!generic.explicit_edit);

    let legacy = SessionId::new();
    store.append_group(&[
        item(&legacy, 0, user("one", "before", None)),
        mark(&legacy, 1, END, 1),
    ])?;
    assert_eq!(
        capture_previous_rewrite_v1(&store, &legacy, checkpoint_next(2), None),
        Err(ArchivedForkError::UnverifiedRewrite)
    );
    Ok(())
}

#[test]
fn displayed_retained_prefix_checks_text_and_media_and_drops_unseen_rows() -> TestResult {
    let dir = tempfile::tempdir()?;
    let store = TranscriptStore::new(dir.path());
    let sid = SessionId::new();
    let photo = fixture_media("exact");
    let shown = TurnItem::UserMessage(UserMessageItem {
        id: id("shown-user"),
        content: vec![
            ContentPart::Text { text: "same text".into() },
            ContentPart::Media { media: photo.clone(), detail: None },
        ],
    });
    store.append_group(&[
        item(&sid, 0, user("unseen", "prior page", None)),
        item(&sid, 1, shown),
        item(&sid, 2, assistant("shown-answer", "retained")),
        item(&sid, 3, user("after-click", "not included", None)),
        mark(&sid, 4, START, 1),
        item(&sid, 5, assistant("shown-answer", "new branch")),
        mark(&sid, 6, END, 1),
    ])?;
    let (boundary, checkpoint) = capture_previous_rewrite_v1(&store, &sid, checkpoint_next(7), None)
        .expect("closed successor rewrite")
        .expect("retained predecessor");
    assert_eq!(boundary.before_sequence.get(), 4);
    assert!(checkpoint.is_retained_predecessor());
    let clicked = ArchivedRowV1 { sequence: 2, id: id("shown-answer") };
    let (mut prefix, _fence) = export_inclusive_guarded_v1(&store, &sid, &checkpoint, &clicked)
        .expect("verified inclusive prefix");

    let user = |media: MediaRef| -> TestResult<DisplayedForkRowV1> {
        let body = displayed_fork_body_v1("same text\n[image]", &[media])?;
        Ok(DisplayedForkRowV1 {
            item_id: "shown-user".into(),
            role: DisplayedForkRoleV1::User,
            body_digest: *blake3::hash(&body).as_bytes(),
        })
    };
    let assistant = |text: &str| -> TestResult<DisplayedForkRowV1> {
        let body = displayed_fork_body_v1(text, &[])?;
        Ok(DisplayedForkRowV1 {
            item_id: "shown-answer".into(),
            role: DisplayedForkRoleV1::Assistant,
            body_digest: *blake3::hash(&body).as_bytes(),
        })
    };
    let proof = |rows: &[DisplayedForkRowV1]| -> TestResult<ArchivedDisplayedPrefixV1> {
        Ok(ArchivedDisplayedPrefixV1 {
            row_count: 2,
            digest: *blake3::hash(&archived_displayed_prefix_v1(rows)?).as_bytes(),
        })
    };
    let wrong_photo = proof(&[user(fixture_media("different"))?, assistant("retained")?])?;
    let wrong_reply = proof(&[user(photo.clone())?, assistant("new branch")?])?;
    assert_eq!(prefix.restrict_to_displayed(&wrong_photo), Err(ArchivedForkError::Changed));
    assert_eq!(prefix.restrict_to_displayed(&wrong_reply), Err(ArchivedForkError::Changed));
    assert_eq!(prefix.rows.len(), 3, "mismatch must not change prefix");
    prefix.restrict_to_displayed(&proof(&[user(photo.clone())?, assistant("retained")?])?)
        .expect("exact visible suffix");
    assert_eq!(prefix.rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        ["shown-user", "shown-answer"]);
    assert_eq!(prefix.media_refs().collect::<Vec<_>>(), [&photo]);
    assert_eq!(prefix.into_seed().map(|(when, _)| when).collect::<Vec<_>>(), [time(1), time(2)]);
    Ok(())
}
