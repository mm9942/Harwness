use super::*;
use harw_protocol::items::{AssistantMessageItem, ContentPart, TurnItem, UserMessageItem};
use harw_protocol::session_wire::{PageParams, ProjectsParams};
use harw_session_host::driver::ArchiveJournalBinding;
use harw_session_host::index::{
    ProfileIndex, ProfilePage, ProfileRow, ProfileShare, ProjectPage, ResolvedProject, ShareMode,
    ShareSettings,
};
use harw_session_host::{DurableTranscripts, HostError};
use harw_session_store::{RecordKind, TranscriptRecord, TranscriptStore, meta};
use harw_types::{ItemId, TenantId, ThreadRef};
use serde_json::{Value, json};
use std::io::Write as _;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

const FEATURE: &str = "archive_checkpoint_v1";
const ISSUE: &str = "session.revision_checkpoint.issue";
const DESCRIBE: &str = "session.revision_checkpoint.describe";

struct Index(PathBuf);

impl ProfileIndex for Index {
    fn sessions_root(&self) -> &Path {
        &self.0
    }
    fn projects(&self, _: &ProjectsParams) -> Result<ProjectPage, HostError> {
        Ok(ProjectPage {
            projects: Vec::new(),
            next_cursor: None,
        })
    }
    fn page(&self, _: &PageParams) -> Result<ProfilePage, HostError> {
        Ok(ProfilePage {
            rows: Vec::new(),
            next_cursor: None,
        })
    }
    fn row(&self, _: &SessionId) -> Result<Option<ProfileRow>, HostError> {
        Ok(None)
    }
    fn newest(&self, _: usize) -> Result<Vec<ProfileRow>, HostError> {
        Ok(Vec::new())
    }
    fn resolve_project(&self, _: &str) -> Result<Option<ResolvedProject>, HostError> {
        Ok(None)
    }
    fn workdir(&self, id: &SessionId) -> Result<Option<PathBuf>, HostError> {
        Ok(meta::load(&self.0, id)?
            .and_then(|m| m.project_root)
            .filter(|dir| dir.is_dir()))
    }
}

struct Driver {
    root: PathBuf,
    published: Option<NonZeroU64>,
    effects: AtomicUsize,
    media: harw_media::MediaStore,
}

fn thread(sid: &SessionId) -> ThreadRef {
    ThreadRef::from_str(format!("archive-wire:{}", sid.as_str()))
}

impl TurnDriver for Driver {
    fn supports_archive_checkpoints(&self) -> bool {
        true
    }
    fn archive_published_head(&self, _: &SessionId) -> Result<Option<NonZeroU64>, HostError> {
        Ok(self.published)
    }
    fn archive_journal_binding(&self, sid: &SessionId) -> Result<ArchiveJournalBinding, HostError> {
        Ok(ArchiveJournalBinding {
            root: self.root.clone(),
            thread: thread(sid),
        })
    }
    fn archive_media_store(&self) -> Option<&harw_media::MediaStore> {
        Some(&self.media)
    }
    fn prepare_execution(
        &self,
        id: &SessionId,
        admission: harw_session_host::driver::ExecutionAdmission,
    ) -> DriverFuture<'_, ()> {
        EchoDriver.prepare_execution(id, admission)
    }
    fn create_session(&self, id: &SessionId, title: Option<&str>) -> DriverFuture<'_, ()> {
        EchoDriver.create_session(id, title)
    }
    fn run_turn(
        &self,
        _: TurnInput,
        _: CancelSignal,
        _: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        self.effects.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(HostError::Driver("archive started a model".into())) })
    }
    fn resume_after_approval(
        &self,
        _: &SessionId,
        _: CancelSignal,
        _: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        self.effects.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(HostError::Driver("archive started a tool".into())) })
    }
    fn apply_setting(&self, id: &SessionId, setting: Setting) -> DriverFuture<'_, ()> {
        EchoDriver.apply_setting(id, setting)
    }
    fn model_name(&self, _: &SessionId) -> Option<String> {
        None
    }
}

fn open_archive_host(
    root: &Path,
    next: Option<NonZeroU64>,
) -> TestResult<(SessionHost, Arc<Driver>)> {
    let sessions = root.join("sessions");
    std::fs::create_dir_all(&sessions)?;
    let driver = Arc::new(Driver {
        root: sessions.clone(),
        published: next,
        effects: AtomicUsize::new(0),
        media: harw_media::MediaStore::open(root.join("media"))?,
    });
    let host = SessionHost::open_with_profile(
        HostConfig::new(root.join("state")),
        driver.clone(),
        Arc::new(DurableTranscripts::open(&sessions)?),
        Arc::new(MemoryApprovals::new()),
        ToolHost::disabled(),
        ProfileShare {
            settings: ShareSettings {
                mode: ShareMode::Continue,
                tenant: TenantId::try_from_str("archive-v5-fixture")?,
                legacy_rows: 10,
                listener_tier: PermissionTier::Operator,
            },
            index: Arc::new(Index(sessions)),
        },
    )?;
    Ok((host, driver))
}

async fn open_ws(
    host: &SessionHost,
    actor: ClientIdentity,
) -> TestResult<WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>> {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    spawn_server_for(server_io, host.clone(), actor, false);
    let (request, key) = upgrade::client_request("localhost")?;
    let response = client(client_io, request).await?;
    upgrade::verify_client_response(&response, &key)?;
    let io = TokioIo::new(hyper::upgrade::on(response).await?);
    Ok(WebSocketStream::from_raw_socket(io, Role::Client, None).await)
}

fn persist_fixture(root: &Path, source: &SessionId) -> TestResult {
    let mut file = std::fs::File::create(root.join(format!("{}.jsonl", source.as_str())))?;
    let records = [
        (
            "lifecycle",
            json!({"marker":"history_replace_started","item_count":1}),
        ),
        (
            "item",
            json!({"type":"user_message","id":ItemId::new(),
            "content":[{"type":"text","text":"opaque fixture content"}]}),
        ),
        (
            "lifecycle",
            json!({"marker":"history_replaced","item_count":1}),
        ),
    ];
    for (n, (kind, payload)) in records.into_iter().enumerate() {
        serde_json::to_writer(
            &mut file,
            &json!({
                "session_id":source,"thread":thread(source),"sequence":n,
                "recorded_at":"2023-11-14T22:13:20Z","kind":kind,"payload":payload,
            }),
        )?;
        file.write_all(b"\n")?;
    }
    file.sync_all()?;
    Ok(())
}

#[tokio::test]
async fn archive_checkpoint_v5_gates_wire_and_reauthorizes_retained_receipt_after_restart()
-> TestResult {
    let tmp = tempfile::tempdir()?;
    let (host, driver) = open_archive_host(tmp.path(), NonZeroU64::new(3))?;
    let mut old = open_ws(&host, identity()).await?;
    let ack = call(
        &mut old,
        "old",
        METHOD_SESSION_HELLO,
        json!({"client_label":"archive-test","wire_minor":4,"features":[FEATURE]}),
    )
    .await?;
    let ack: HelloAck = serde_json::from_value(ack.result.ok_or("old hello")?)?;
    assert_eq!(ack.wire_minor, 4);
    assert!(!ack.features.iter().any(|it| it == FEATURE));
    let old_create = call(
        &mut old,
        "old-create",
        METHOD_SESSION_CREATE,
        json!({"title":"source"}),
    )
    .await?;
    let source: SessionSummary = serde_json::from_value(old_create.result.ok_or("old create")?)?;
    persist_fixture(&driver.root, &source.session_id)?;
    let params =
        json!({"session_id":source.session_id,"expected":{"generation":0,"durable_next":3}});

    let mut v5 = open_ws(&host, identity()).await?;
    let ack = call(
        &mut v5,
        "v5",
        METHOD_SESSION_HELLO,
        json!({"client_label":"archive-test","wire_minor":5,"features":[FEATURE]}),
    )
    .await?;
    let ack: HelloAck = serde_json::from_value(ack.result.ok_or("new hello")?)?;
    assert_eq!(ack.wire_minor, 5);
    assert_eq!(ack.features, vec![FEATURE]);
    let old_denied = call(&mut old, "old-denied", ISSUE, params.clone()).await?;
    assert_eq!(old_denied.error.map(|e| e.code), Some(error_codes::DENIED));
    let mut unnegotiated = open_ws(&host, identity()).await?;
    let ack = call(
        &mut unnegotiated,
        "not-requested",
        METHOD_SESSION_HELLO,
        json!({"client_label":"archive-test","wire_minor":5}),
    )
    .await?;
    assert!(ack.error.is_none());
    let not_requested = call(&mut unnegotiated, "denied", ISSUE, params.clone()).await?;
    assert_eq!(
        not_requested.error.map(|e| e.code),
        Some(error_codes::DENIED)
    );
    let mut malformed = params.clone();
    malformed["history"] = json!([{"role":"assistant","text":"client cannot seed"}]);
    let malformed = call(&mut v5, "malformed", ISSUE, malformed).await?;
    assert_eq!(
        malformed.error.map(|e| e.code),
        Some(error_codes::INVALID_PARAMS)
    );
    let mut stale = params.clone();
    stale["expected"]["durable_next"] = json!(2);
    let stale = call(&mut v5, "stale", ISSUE, stale).await?;
    assert_eq!(stale.error.map(|e| e.code), Some(error_codes::BUSY));

    let issued = call(&mut v5, "issue", ISSUE, params.clone()).await?;
    let issued = issued
        .result
        .ok_or_else(|| format!("checkpoint denied: {:?}", issued.error))?;
    assert_eq!(issued["revision"]["durable_next"], 3);
    assert!(
        issued["checkpoint_id"]
            .as_str()
            .is_some_and(|id| id.len() >= 32)
    );
    assert!(issued.get("history").is_none());
    assert!(!issued.to_string().contains("opaque fixture content"));
    let duplicate = call(&mut v5, "again", ISSUE, params.clone()).await?;
    assert_eq!(duplicate.result, Some(issued.clone()));
    let listed = call(
        &mut v5,
        "list-after-capture",
        harw_protocol::methods::METHOD_SESSION_LIST,
        Value::Null,
    )
    .await?;
    assert_eq!(
        listed
            .result
            .and_then(|result| result["sessions"].as_array().map(Vec::len)),
        Some(1)
    );
    let describe = json!({"session_id":source.session_id,"checkpoint_id":issued["checkpoint_id"]});

    old.close(None).await?;
    v5.close(None).await?;
    unnegotiated.close(None).await?;
    assert_eq!(driver.effects.load(Ordering::SeqCst), 0);
    drop(host);
    let (reopened, restored_driver) = open_archive_host(tmp.path(), None)?;
    let mut owner = open_ws(&reopened, identity()).await?;
    let _ = call(
        &mut owner,
        "reopen",
        METHOD_SESSION_HELLO,
        json!({"client_label":"archive-test","wire_minor":5,"features":[FEATURE]}),
    )
    .await?;
    let resolved = call(&mut owner, "describe", DESCRIBE, describe.clone()).await?;
    assert_eq!(resolved.result, Some(issued.clone()));
    let no_writer = call(&mut owner, "no-writer", ISSUE, params).await?;
    assert_eq!(no_writer.error.map(|e| e.code), Some(error_codes::DENIED));

    let mut other = identity();
    other.principal = Principal::trusted_ingress(
        PrincipalKind::Human,
        "uid:2000",
        IngressSurface::Tui,
        PermissionTier::Operator,
    );
    other.actor = ApprovalActor::Operator {
        id: "uid:2000".into(),
    };
    let mut other = open_ws(&reopened, other).await?;
    let _ = call(
        &mut other,
        "other",
        METHOD_SESSION_HELLO,
        json!({"client_label":"archive-test","wire_minor":5,"features":[FEATURE]}),
    )
    .await?;
    let refused = call(&mut other, "other-describe", DESCRIBE, describe.clone()).await?;
    assert_eq!(refused.error.map(|e| e.code), Some(error_codes::NOT_FOUND));

    std::fs::remove_file(
        restored_driver
            .root
            .join(format!("{}.jsonl", source.session_id.as_str())),
    )?;
    let gone = call(&mut owner, "gone", DESCRIBE, describe).await?;
    assert!(gone.error.is_some());
    assert_eq!(restored_driver.effects.load(Ordering::SeqCst), 0);
    Ok(())
}

const PAGE: &str = "session.revision_checkpoint.page";
const FORK: &str = "session.fork_inclusive";

#[tokio::test]
async fn archive_inclusive_v5_pages_pinned_rows_and_retries_one_destination_after_restart()
-> TestResult {
    let tmp = tempfile::tempdir()?;
    let project = tmp.path().join("verified-project");
    std::fs::create_dir(&project)?;
    let (host, driver) = open_archive_host(tmp.path(), NonZeroU64::new(6))?;
    let owner_actor = identity();
    let mut owner = open_ws(&host, owner_actor.clone()).await?;
    let hello = json!({"client_label":"archive-test","wire_minor":5,"features":[FEATURE]});
    let ack = call(&mut owner, "hello", METHOD_SESSION_HELLO, hello.clone()).await?;
    assert!(ack.error.is_none());
    let created = call(&mut owner, "source", METHOD_SESSION_CREATE, json!({})).await?;
    let source: SessionSummary = serde_json::from_value(created.result.ok_or("source creation")?)?;
    let source = source.session_id;
    meta::set_project(
        &driver.root,
        &source,
        Some(&project),
        Some(&project),
        Some("known-project"),
    )?;
    let transcript = TranscriptStore::new(&driver.root);
    let mut gif = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff\x21\xf9\x04\x01\x00\x00\x00\x00\x2c\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x01\x44\x00\x3b".to_vec();
    let media1 = driver.media.put(&gif)?;
    gif[13] = 0x28;
    let media2 = driver.media.put(&gif)?;
    assert_ne!(media1, media2);
    let before_item = ItemId::new();
    let user1 = ItemId::new();
    let user2 = ItemId::new();
    let clicked = ItemId::new();
    let fixed = |seq: u64| jiff::Timestamp::from_second(1_700_000_000 + seq as i64);
    let record = |sequence, kind, payload| -> TestResult<_> {
        Ok(TranscriptRecord::new(
            source.clone(),
            thread(&source),
            sequence,
            fixed(sequence)?,
            kind,
            payload,
        ))
    };
    let with_media = |id: ItemId, media| -> TestResult<_> {
        Ok(serde_json::to_value(TurnItem::UserMessage(
            UserMessageItem {
                id,
                content: vec![
                    ContentPart::Text {
                        text: "same prompt".into(),
                    },
                    ContentPart::Media {
                        media,
                        detail: None,
                    },
                ],
            },
        ))?)
    };
    let reply = |id: ItemId, text: &str| -> TestResult<_> {
        Ok(serde_json::to_value(TurnItem::AssistantMessage(
            AssistantMessageItem {
                id,
                content: vec![ContentPart::Text { text: text.into() }],
                phase: None,
            },
        ))?)
    };
    transcript.append_group(&[
        record(
            0,
            RecordKind::Lifecycle,
            json!({"marker":"history_replace_started","item_count":4}),
        )?,
        record(
            1,
            RecordKind::Item,
            with_media(user1.clone(), media1.clone())?,
        )?,
        record(
            2,
            RecordKind::Item,
            reply(before_item.clone(), "first reply")?,
        )?,
        record(
            3,
            RecordKind::Item,
            with_media(user2.clone(), media2.clone())?,
        )?,
        record(
            4,
            RecordKind::Item,
            reply(clicked.clone(), "archived selected reply")?,
        )?,
        record(
            5,
            RecordKind::Lifecycle,
            json!({"marker":"history_replaced","item_count":4}),
        )?,
    ])?;
    let minted = call(
        &mut owner,
        "issue",
        ISSUE,
        json!({"session_id":source,"expected":{"generation":0,"durable_next":6}}),
    )
    .await?;
    let minted = minted
        .result
        .ok_or_else(|| format!("issue denied {:?}", minted.error))?;
    let checkpoint_id = minted["checkpoint_id"].as_str().ok_or("checkpoint ID")?;
    transcript.append_group(&[
        record(
            6,
            RecordKind::Lifecycle,
            json!({"marker":"history_replace_started","item_count":1}),
        )?,
        record(
            7,
            RecordKind::Item,
            reply(ItemId::new(), "different later branch")?,
        )?,
        record(
            8,
            RecordKind::Lifecycle,
            json!({"marker":"history_replaced","item_count":1}),
        )?,
    ])?;

    let page = call(
        &mut owner,
        "page",
        PAGE,
        json!({"session_id":source,"checkpoint_id":checkpoint_id,"limit":2}),
    )
    .await?;
    let page = page
        .result
        .ok_or_else(|| format!("page denied {:?}", page.error))?;
    assert_eq!(page["next_before_sequence"], 3);
    assert_eq!(
        page["rows"][0]["locator"],
        json!({"sequence":3,"item_id":user2})
    );
    assert_eq!(page["rows"][0]["image_count"], 1);
    assert_eq!(
        page["rows"][1]["locator"],
        json!({"sequence":4,"item_id":clicked})
    );
    assert_eq!(page["rows"][1]["role"], "assistant");
    assert_eq!(
        page["rows"][1]["recorded_at"],
        serde_json::to_value(fixed(4)?)?
    );
    assert!(!page.to_string().contains("different later branch"));
    assert!(
        !page
            .to_string()
            .contains(media2.digest().to_string().as_str())
    );
    let older = call(
        &mut owner,
        "older",
        PAGE,
        json!({"session_id":source,"checkpoint_id":checkpoint_id,"before_sequence":3,"limit":2}),
    )
    .await?;
    let older = older.result.ok_or("older page")?;
    assert!(older.get("next_before_sequence").is_none_or(Value::is_null));
    assert_eq!(
        older["rows"][0]["locator"],
        json!({"sequence":1,"item_id":user1})
    );
    assert_eq!(
        older["rows"][1]["locator"],
        json!({"sequence":2,"item_id":before_item})
    );
    assert_eq!(
        older["rows"][0]["text_parts"],
        page["rows"][0]["text_parts"]
    );

    let request = json!({"session_id":source,"checkpoint_id":checkpoint_id,
        "row":{"sequence":4,"item_id":clicked},"client_operation_id":"archive-fixture-fixed"});
    let mut wrong_wire = request.clone();
    wrong_wire["media"] = json!([{"arbitrary":"not source"}]);
    let invalid = call(&mut owner, "invalid-fork", FORK, wrong_wire).await?;
    assert_eq!(
        invalid.error.map(|e| e.code),
        Some(error_codes::INVALID_PARAMS)
    );
    let fork = call(&mut owner, "fork", FORK, request.clone()).await?;
    let fork: SessionSummary = serde_json::from_value(
        fork.result
            .ok_or_else(|| format!("fork denied {:?}", fork.error))?,
    )?;
    assert_ne!(fork.session_id, source);
    let retry = call(&mut owner, "retry", FORK, request.clone()).await?;
    let again: SessionSummary = serde_json::from_value(retry.result.ok_or("replay result")?)?;
    assert_eq!(again.session_id, fork.session_id);
    let mut conflict = request.clone();
    conflict["row"] = json!({"sequence":2,"item_id":before_item});
    let conflicting = call(&mut owner, "conflict", FORK, conflict).await?;
    assert_eq!(conflicting.error.map(|e| e.code), Some(error_codes::DENIED));

    let rows: Vec<_> = transcript
        .reader(&fork.session_id)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|r| r.kind == RecordKind::Item)
        .collect();
    assert_eq!(rows.len(), 4);
    for (index, row) in rows.iter().enumerate() {
        assert_eq!(row.recorded_at, fixed(index as u64 + 1)?);
    }
    assert_eq!(
        rows[0].payload["content"][1]["media"],
        serde_json::to_value(media1)?
    );
    assert_eq!(
        rows[2].payload["content"][1]["media"],
        serde_json::to_value(media2)?
    );
    assert_eq!(
        rows[3].payload["content"][0]["text"],
        "archived selected reply"
    );
    assert_eq!(
        meta::load(&driver.root, &fork.session_id)?
            .ok_or("fork project")?
            .project_root,
        Some(project)
    );
    assert_eq!(driver.effects.load(Ordering::SeqCst), 0);
    owner.close(None).await?;
    // Simulate a crash after publication and before the completed operation
    // receipt was synced: recovery must recognize the same durable target.
    let operations = tmp
        .path()
        .join("state")
        .join("hosted-sessions")
        .join("archive-forks-v1");
    let intents: Vec<_> = std::fs::read_dir(operations)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert_eq!(intents.len(), 1);
    let mut pending: Value = serde_json::from_slice(&std::fs::read(&intents[0])?)?;
    assert_eq!(pending["complete"], true);
    pending["complete"] = json!(false);
    let mut file = std::fs::File::create(&intents[0])?;
    serde_json::to_writer(&mut file, &pending)?;
    file.sync_all()?;
    drop(host);

    let (restored, restored_driver) = open_archive_host(tmp.path(), None)?;
    let reconnect = identity();
    let mut same = open_ws(&restored, reconnect.clone()).await?;
    let _ = call(&mut same, "hello", METHOD_SESSION_HELLO, hello.clone()).await?;
    let retry = call(&mut same, "reopened", FORK, request.clone()).await?;
    let replayed: SessionSummary =
        serde_json::from_value(retry.result.ok_or("restart replay result")?)?;
    assert_eq!(replayed.session_id, fork.session_id);
    restored.revoke_connection(reconnect.connection);
    let revoked = call(&mut same, "revoked", FORK, request).await?;
    assert_eq!(revoked.error.map(|e| e.code), Some(error_codes::REVOKED));
    assert_eq!(
        std::fs::read_dir(&restored_driver.root)?
            .filter_map(Result::ok)
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("jsonl"))
            .count(),
        2
    );
    assert_eq!(restored_driver.effects.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn previous_archive_discovers_before_first_edit_without_manual_checkpoint() -> TestResult {
    const RETAINED: &str = "archive_retained_rewrite_v1";
    const PREVIOUS: &str = "session.revision_checkpoint.previous";
    let tmp = tempfile::tempdir()?;
    let (host, driver) = open_archive_host(tmp.path(), NonZeroU64::new(4))?;
    let actor = identity();
    let mut owner = open_ws(&host, actor.clone()).await?;
    let hello = json!({"client_label":"archive-previous","wire_minor":5,
        "features":[FEATURE, RETAINED]});
    let granted = call(&mut owner, "hello", METHOD_SESSION_HELLO, hello.clone()).await?;
    let granted: HelloAck = serde_json::from_value(granted.result.ok_or("hello")?)?;
    assert!(granted.features.iter().any(|feature| feature == RETAINED));
    let created = call(&mut owner, "source", METHOD_SESSION_CREATE, json!({})).await?;
    let source: SessionSummary = serde_json::from_value(created.result.ok_or("source creation")?)?;
    let source = source.session_id;
    let old_user = ItemId::new();
    let old_reply = ItemId::new();
    let time = |sequence: u64| jiff::Timestamp::from_second(1_700_000_000 + sequence as i64);
    let record = |sequence, kind, payload| -> TestResult<_> {
        Ok(TranscriptRecord::new(
            source.clone(), thread(&source), sequence, time(sequence)?, kind, payload,
        ))
    };
    let rows = [
        record(0, RecordKind::Item, json!({"type":"user_message","id":old_user,
            "content":[{"type":"text","text":"original prompt"}]}))?,
        record(1, RecordKind::Item, json!({"type":"assistant_message","id":old_reply,
            "content":[{"type":"text","text":"original reply"}]}))?,
        record(2, RecordKind::Lifecycle, json!({"marker":"history_replace_started",
            "rewind_target_item_id":old_user,"item_count":0}))?,
        record(3, RecordKind::Lifecycle, json!({"marker":"history_replaced","item_count":0}))?,
        record(4, RecordKind::Item, json!({"type":"user_message","id":ItemId::new(),
            "content":[{"type":"text","text":"replacement prompt"}]}))?,
    ];
    TranscriptStore::new(&driver.root).append_group(&rows)?;
    let params = json!({"session_id":source});
    let previous = call(&mut owner, "previous", PREVIOUS, params.clone()).await?;
    let previous = previous.result.ok_or_else(|| format!("previous {:?}", previous.error))?;
    assert_eq!(previous["before_sequence"], 2);
    assert_eq!(previous["cause"], "user_edit");
    assert_eq!(previous["recorded_at"], serde_json::to_value(time(3)?)?);
    let checkpoint = previous["checkpoint"]["checkpoint_id"].as_str().ok_or("checkpoint ID")?;
    let again = call(&mut owner, "again", PREVIOUS, params.clone()).await?;
    assert_eq!(again.result, Some(previous.clone()));
    let older = call(&mut owner, "older", PREVIOUS,
        json!({"session_id":source,"before_sequence":2})).await?;
    assert_eq!(older.result, Some(Value::Null));

    let mut old = open_ws(&host, actor.clone()).await?;
    let _ = call(&mut old, "hello", METHOD_SESSION_HELLO,
        json!({"client_label":"archive-v5-old","wire_minor":5,"features":[FEATURE]})).await?;
    let denied = call(&mut old, "denied", PREVIOUS, params.clone()).await?;
    assert_eq!(denied.error.map(|e| e.code), Some(error_codes::DENIED));

    let page_params = json!({"session_id":source,"checkpoint_id":checkpoint});
    let page = call(&mut owner, "page", PAGE, page_params.clone()).await?;
    let page = page.result.ok_or("retained page")?;
    assert_eq!(page["rows"].as_array().map(Vec::len), Some(2));
    assert_eq!(page["rows"][1]["locator"], json!({"sequence":1,"item_id":old_reply}));
    assert_eq!(page["rows"][1]["recorded_at"], serde_json::to_value(time(1)?)?);
    assert!(!page.to_string().contains("replacement prompt"));

    drop(old);
    drop(owner);
    drop(host);
    let (restored, restored_driver) = open_archive_host(tmp.path(), None)?;
    let mut owner = open_ws(&restored, actor).await?;
    let _ = call(&mut owner, "restored-hello", METHOD_SESSION_HELLO, hello).await?;
    let restored_page = call(&mut owner, "restored-page", PAGE, page_params).await?;
    assert_eq!(restored_page.result, Some(page));
    let cold = call(&mut owner, "cold", PREVIOUS, params).await?;
    assert!(cold.error.is_some(), "no fresh authority from physical EOF");
    let fork = call(&mut owner, "fork", FORK, json!({
        "session_id":source, "checkpoint_id":checkpoint,
        "row":{"sequence":1,"item_id":old_reply},
        "client_operation_id":"previous-archive-test",
    })).await?;
    let fork: SessionSummary = serde_json::from_value(fork.result.ok_or("verified archived fork")?)?;
    assert_ne!(fork.session_id, source);
    assert_eq!(driver.effects.load(Ordering::SeqCst), 0);
    assert_eq!(restored_driver.effects.load(Ordering::SeqCst), 0);
    Ok(())
}

// A valid PNG with an uncompressed zlib stream so storage size, not a codec
// approximation, exercises the multi-part path. Geometry is within the real
// MediaStore limits; each row has one PNG filter byte followed by grayscale.
fn media_fixture_png(side: u32) -> Vec<u8> {
    fn png_chunk(output: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        output.extend((data.len() as u32).to_be_bytes());
        output.extend(kind);
        output.extend(data);
        let mut crc = u32::MAX;
        for &byte in kind.iter().chain(data) {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xedb8_8320 & 0_u32.wrapping_sub(crc & 1));
            }
        }
        output.extend((!crc).to_be_bytes());
    }
    let raw_size = ((side + 1) * side) as usize;
    let mut pixels = vec![0_u8; raw_size];
    // Make equal-size fixtures with differing sides produce distinct digests.
    pixels[1] = (side & 0xff) as u8;
    let mut zlib = vec![0x78, 0x01];
    let chunks = pixels.chunks(u16::MAX as usize);
    let count = chunks.len();
    for (index, chunk) in chunks.enumerate() {
        zlib.push(u8::from(index + 1 == count));
        let len = chunk.len() as u16;
        zlib.extend(len.to_le_bytes());
        zlib.extend((!len).to_le_bytes());
        zlib.extend(chunk);
    }
    let (a, b) = pixels.iter().fold((1_u32, 0_u32), |(a, b), byte| {
        let a = (a + u32::from(*byte)) % 65_521;
        (a, (a + b) % 65_521)
    });
    zlib.extend(((b << 16) | a).to_be_bytes());
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend(side.to_be_bytes());
    header.extend(side.to_be_bytes());
    header.extend([8, 0, 0, 0, 0]);
    png_chunk(&mut png, b"IHDR", &header);
    png_chunk(&mut png, b"IDAT", &zlib);
    png_chunk(&mut png, b"IEND", &[]);
    png
}

#[tokio::test]
async fn session_media_preview_is_canonical_scoped_and_chunks_retained_original() -> TestResult {
    use harw_protocol::session_wire::{MEDIA_PREVIEW_MAX_BYTES, MEDIA_PREVIEW_PART_BYTES};
    const CAP: &str = "session_media_preview_v1";
    const PREVIEW: &str = "session.media.preview";
    let tmp = tempfile::tempdir()?;
    let (host, driver) = open_archive_host(tmp.path(), NonZeroU64::new(4))?;
    let actor = identity();
    let mut owner = open_ws(&host, actor.clone()).await?;
    let hello = json!({"client_label":"media-fixture","wire_minor":5,"features":[FEATURE, CAP]});
    let ack = call(&mut owner, "hello", METHOD_SESSION_HELLO, hello.clone()).await?;
    assert!(ack.result.ok_or("hello")?["features"].as_array().ok_or("features")?
        .iter().any(|feature| feature == CAP));
    let first = call(&mut owner, "create-one", METHOD_SESSION_CREATE, json!({})).await?;
    let first: SessionSummary = serde_json::from_value(first.result.ok_or("first source")?)?;
    let second = call(&mut owner, "create-two", METHOD_SESSION_CREATE, json!({})).await?;
    let second: SessionSummary = serde_json::from_value(second.result.ok_or("other source")?)?;

    let tiny = driver.media.put(&media_fixture_png(40))?;
    let large = driver.media.put(&media_fixture_png(2600))?;
    assert!((6 * 1024 * 1024..=MEDIA_PREVIEW_MAX_BYTES as u64).contains(&large.bytes()));
    let selected = ItemId::new();
    let other = ItemId::new();
    let fixture = |sid: &SessionId, first: &ItemId, second: &ItemId, image| -> TestResult {
        let t = jiff::Timestamp::from_second(1_700_000_000)?;
        let record = |seq, kind, data| {
            TranscriptRecord::new(sid.clone(), thread(sid), seq, t, kind, data)
        };
        TranscriptStore::new(&driver.root).append_group(&[
            record(0, RecordKind::Lifecycle,
                   json!({"marker":"history_replace_started","item_count":2})),
            record(1, RecordKind::Item, json!({"type":"user_message","id":first,
                   "content":[{"type":"text","text":"same"},{"type":"media","media":image}]})),
            record(2, RecordKind::Item, json!({"type":"user_message","id":second,
                   "content":[{"type":"text","text":"same"},{"type":"media","media":tiny}]})),
            record(3, RecordKind::Lifecycle, json!({"marker":"history_replaced","item_count":2})),
        ])?;
        Ok(())
    };
    fixture(&first.session_id, &selected, &other, large.clone())?;
    fixture(&second.session_id, &ItemId::new(), &ItemId::new(), tiny.clone())?;
    let mut request = json!({"session_id":first.session_id,"source":{"kind":"canonical",
        "item_id":selected},"attachment_index":0,"mode":"thumbnail"});
    let thumb = call(&mut owner, "thumb", PREVIEW, request.clone()).await?;
    assert_eq!(thumb.result, Some(json!({"status":"open_required"})));

    // A different session or a made-up slot cannot retrieve the selected
    // bytes even though the same host/store and exact digest exist.
    let mut wrong = request.clone();
    wrong["session_id"] = json!(second.session_id);
    let unknown = call(&mut owner, "wrong-session", PREVIEW, wrong).await?;
    assert_eq!(unknown.result, Some(json!({"status":"missing"})));
    let legacy = call(&mut owner, "legacy-member", "media.get",
        json!({"session_id":second.session_id,"media":tiny})).await?;
    assert!(legacy.result.is_some(), "legacy wire still reads actual membership");
    let unmapped = driver.media.put(&media_fixture_png(41))?;
    let legacy_denied = call(&mut owner, "legacy-nonmember", "media.get",
        json!({"session_id":second.session_id,"media":unmapped})).await?;
    assert!(legacy_denied.error.is_some(), "global digest is not authority");
    let mut wrong = request.clone();
    wrong["attachment_index"] = json!(1);
    let missing = call(&mut owner, "unknown-slot", PREVIEW, wrong).await?;
    assert_eq!(missing.result, Some(json!({"status":"missing"})));

    request["mode"] = json!("visible_preview");
    let mut expected_offset = 0_u64;
    for part in 0..(MEDIA_PREVIEW_MAX_BYTES / MEDIA_PREVIEW_PART_BYTES) {
        let reply = call(&mut owner, &format!("part-{part}"), PREVIEW, request.clone()).await?;
        let reply = reply.result.ok_or_else(|| format!("part error: {:?}", reply.error))?;
        assert_eq!(reply["status"], "part");
        assert_eq!(reply["total_bytes"], large.bytes());
        assert_eq!(reply["offset"], expected_offset);
        assert_eq!(reply["mime"], "image/png");
        let count = (large.bytes() - expected_offset).min(MEDIA_PREVIEW_PART_BYTES as u64);
        assert_eq!(reply["data_b64"].as_str().ok_or("data")?.len(),
            count.div_ceil(3) as usize * 4);
        assert!(serde_json::to_vec(&reply)?.len() < 1024 * 1024);
        expected_offset += count;
        if reply["continuation"].is_null() {
            break;
        }
        request["continuation"] = reply["continuation"].clone();
        if part == 0 {
            let mut stranger = open_ws(&host, actor.clone()).await?;
            let _ = call(&mut stranger, "hello", METHOD_SESSION_HELLO, hello.clone()).await?;
            let interposed = call(&mut stranger, "foreign-continuation", PREVIEW, request.clone()).await?;
            assert_eq!(interposed.result, Some(json!({"status":"busy"})));
        }
    }
    assert_eq!(expected_offset, large.bytes());

    // Archive bytes use the exact issued checkpoint and stored row, not
    // the current matching text or a bare digest.
    let issued = call(&mut owner, "issue", ISSUE,
        json!({"session_id":first.session_id,"expected":{"generation":0,"durable_next":4}})).await?;
    let id = issued.result.ok_or("archive receipt")?["checkpoint_id"].clone();
    let mut archived = json!({"session_id":first.session_id,
        "source":{"kind":"archive","checkpoint_id":id,"row":{"sequence":2,"item_id":other}},
        "attachment_index":0,"mode":"thumbnail"});
    let exact = call(&mut owner, "archived-image", PREVIEW, archived.clone()).await?;
    let exact = exact.result.ok_or("archived image")?;
    assert_eq!(exact["status"], "part");
    assert_eq!(exact["total_bytes"], tiny.bytes());
    archived["source"]["row"]["sequence"] = json!(1);
    let wrong = call(&mut owner, "archived-wrong-occurrence", PREVIEW, archived).await?;
    assert_eq!(wrong.result, Some(json!({"status":"missing"})));
    assert_eq!(driver.effects.load(Ordering::SeqCst), 0);
    Ok(())
}
