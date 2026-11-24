use super::*;
use crate::driver::{ArchiveJournalBinding, CancelSignal, DriverFuture, TurnDriver};
use crate::identity::test_identity::scoped;
use crate::index::{ProfileIndex, ProfilePage, ProfileRow, ProjectPage, ResolvedProject};
use base64::Engine as _;
use harw_protocol::items::{AssistantMessageItem, ContentPart, TurnItem, UserMessageItem};
use harw_session_store::{RecordKind, TranscriptRecord, TranscriptStore};
use harw_types::{ItemId, PermissionTier, ThreadRef};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct ArchiveFixtureIndex {
    root: PathBuf,
    project: ResolvedProject,
}

impl ProfileIndex for ArchiveFixtureIndex {
    fn sessions_root(&self) -> &Path {
        &self.root
    }
    fn projects(&self, _: &ProjectsParams) -> Result<ProjectPage, HostError> {
        unreachable!("test does not enumerate projects")
    }
    fn page(&self, _: &PageParams) -> Result<ProfilePage, HostError> {
        unreachable!("test does not enumerate profile pages")
    }
    fn row(&self, _: &SessionId) -> Result<Option<ProfileRow>, HostError> {
        Ok(None)
    }
    fn newest(&self, _: usize) -> Result<Vec<ProfileRow>, HostError> {
        Ok(Vec::new())
    }
    fn resolve_project(&self, _: &str) -> Result<Option<ResolvedProject>, HostError> {
        panic!("archive must not call creation-capable project resolution")
    }
    fn workdir(&self, id: &SessionId) -> Result<Option<PathBuf>, HostError> {
        let present = meta::load(&self.root, id)?.is_some_and(|meta| {
            meta.project_key.as_deref() == Some(&self.project.project_key)
                && meta.project_root.as_deref() == Some(self.project.project_root.as_path())
                && self.project.project_root.is_dir()
        });
        Ok(present.then(|| self.project.project_root.clone()))
    }
}

struct ArchiveFixtureDriver {
    sessions: PathBuf,
    media: harw_media::MediaStore,
    effects: AtomicUsize,
}

impl TurnDriver for ArchiveFixtureDriver {
    fn archive_published_head(&self, _: &SessionId) -> Result<Option<NonZeroU64>, HostError> {
        Ok(NonZeroU64::new(4))
    }

    fn archive_journal_binding(
        &self,
        session: &SessionId,
    ) -> Result<ArchiveJournalBinding, HostError> {
        Ok(ArchiveJournalBinding {
            root: self.sessions.clone(),
            thread: ThreadRef::from_str(format!("test-archive:{}", session.as_str())),
        })
    }

    fn archive_media_store(&self) -> Option<&harw_media::MediaStore> {
        Some(&self.media)
    }

    fn create_session(&self, _: &SessionId, _: Option<&str>) -> DriverFuture<'_, ()> {
        self.effects.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(HostError::Driver("archive invoked runtime creation".into())) })
    }
    fn run_turn(
        &self,
        _: TurnInput,
        _: CancelSignal,
        _: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        self.effects.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(HostError::Driver("archive invoked model".into())) })
    }
    fn resume_after_approval(
        &self,
        _: &SessionId,
        _: CancelSignal,
        _: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        self.effects.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(HostError::Driver("archive invoked tools".into())) })
    }
    fn apply_setting(&self, _: &SessionId, _: Setting) -> DriverFuture<'_, ()> {
        Box::pin(async { Err(HostError::Driver("archive changed settings".into())) })
    }
    fn model_name(&self, _: &SessionId) -> Option<String> {
        None
    }
}

async fn connection(host: &SessionHost, uid: u32, tenant: &TenantId) -> TestResult<HostConnection> {
    let conn = host.connect(scoped(uid, PermissionTier::Operator, tenant))?;
    conn.hello(HelloParams {
        client_label: "archive-fixture".into(),
        wire_minor: PROJECTS_WIRE_MINOR,
        features: vec![features::PROJECTS.to_owned()],
        requested_caps: None,
    })
    .await?;
    Ok(conn)
}

#[tokio::test]
async fn archive_import_commits_exact_history_project_and_same_store_media_without_running()
-> TestResult {
    let temp = tempfile::tempdir()?;
    let sessions = temp.path().join("sessions");
    let workspace = temp.path().join("verified-project");
    std::fs::create_dir(&sessions)?;
    std::fs::create_dir(&workspace)?;
    let transcripts = TranscriptStore::new(&sessions);
    let driver = Arc::new(ArchiveFixtureDriver {
        sessions: sessions.clone(),
        media: harw_media::MediaStore::open(temp.path().join("media"))?,
        effects: AtomicUsize::new(0),
    });
    let other_store = harw_media::MediaStore::open(temp.path().join("wrong-media"))?;
    let gif = base64::engine::general_purpose::STANDARD
        .decode("R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7")?;
    let media = other_store.put(&gif)?;
    let tenant = TenantId::try_from_str("archive-tenant")?;
    let host = SessionHost::open_with_profile(
        HostConfig::new(temp.path().join("host")),
        driver.clone(),
        Arc::new(crate::DurableTranscripts::open(&sessions)?),
        Arc::new(crate::approvals::MemoryApprovals::new()),
        ToolHost::disabled(),
        ProfileShare {
            settings: crate::index::ShareSettings {
                mode: ShareMode::Continue,
                tenant: tenant.clone(),
                legacy_rows: 10,
                listener_tier: PermissionTier::Operator,
            },
            index: Arc::new(ArchiveFixtureIndex {
                root: sessions.clone(),
                project: ResolvedProject {
                    project_key: "verified-key".into(),
                    project_root: workspace.clone(),
                    label: "Verified".into(),
                },
            }),
        },
    )?;
    let alice = connection(&host, 1, &tenant).await?;
    let same_tenant_other_actor = connection(&host, 2, &tenant).await?;
    let foreign_tenant = TenantId::try_from_str("different-tenant")?;
    let other_tenant = connection(&host, 3, &foreign_tenant).await?;
    let source = SessionId::new();
    let record = alice.new_record(source.clone(), Some("source"), None, Some(tenant));
    host.inner.records.save(&record)?;
    meta::set_project(
        &sessions,
        &source,
        Some(&workspace),
        Some(&workspace),
        Some("verified-key"),
    )?;
    let user_id = ItemId::new();
    let assistant_id = ItemId::new();
    let user_time = jiff::Timestamp::from_second(1_700_000_000)?;
    let assistant_time = jiff::Timestamp::from_second(1_700_000_001)?;
    let thread = driver.archive_journal_binding(&source)?.thread;
    let rec = |seq, timestamp, kind, payload| {
        TranscriptRecord::new(
            source.clone(),
            thread.clone(),
            seq,
            timestamp,
            kind,
            payload,
        )
    };
    transcripts.append_group(&[
        rec(
            0,
            user_time,
            RecordKind::Lifecycle,
            serde_json::json!({"marker":"history_replace_started","item_count":2}),
        ),
        rec(
            1,
            user_time,
            RecordKind::Item,
            serde_json::to_value(TurnItem::UserMessage(UserMessageItem {
                id: user_id,
                content: vec![
                    ContentPart::Text {
                        text: "visible user".into(),
                    },
                    ContentPart::Media {
                        media: media.clone(),
                        detail: None,
                    },
                ],
            }))?,
        ),
        rec(
            2,
            assistant_time,
            RecordKind::Item,
            serde_json::to_value(TurnItem::AssistantMessage(AssistantMessageItem {
                id: assistant_id.clone(),
                content: vec![ContentPart::Text {
                    text: "visible reply".into(),
                }],
                phase: None,
            }))?,
        ),
        rec(
            3,
            assistant_time,
            RecordKind::Lifecycle,
            serde_json::json!({"marker":"history_replaced","item_count":2}),
        ),
    ])?;
    let revision = NonZeroU64::new(4).ok_or("fixture revision")?;
    assert!(
        other_tenant
            .issue_archive_ticket(&source, revision)
            .await
            .is_err()
    );
    assert!(
        alice
            .issue_archive_ticket(
                &source,
                NonZeroU64::new(5).ok_or("future fixture revision")?
            )
            .await
            .is_err()
    );
    let ticket = alice.issue_archive_ticket(&source, revision).await?;
    let clicked = ArchivedRowV1 {
        sequence: 2,
        id: assistant_id,
    };
    assert!(
        same_tenant_other_actor
            .import_archive_ticket(&ticket, &clicked, None)
            .await
            .is_err()
    );
    assert!(
        alice
            .import_archive_ticket(&ticket, &clicked, None)
            .await
            .is_err(),
        "media in an unrelated store passed"
    );
    assert_eq!(
        host.inner.records.list()?.len(),
        1,
        "failed imports cannot appear"
    );
    assert_eq!(driver.media.put(&gif)?, media);
    meta::set_project(&sessions, &source, None, None, None)?;
    assert!(
        alice
            .import_archive_ticket(&ticket, &clicked, None)
            .await
            .is_err(),
        "a moved source must not import with stale project scope"
    );
    assert_eq!(host.inner.records.list()?.len(), 1);
    meta::set_project(
        &sessions,
        &source,
        Some(&workspace),
        Some(&workspace),
        Some("verified-key"),
    )?;

    let (verified_seed, source_fence) =
        export_inclusive_guarded_v1(&transcripts, &source, &ticket.revision, &clicked)
            .map_err(archive_error)?;
    transcripts.append(&rec(
        4,
        assistant_time,
        RecordKind::Lifecycle,
        json!({"marker":"a later writer commit","item_count":0}),
    ))?;
    assert!(
        alice
            .commit_archive_destination(&ticket, verified_seed, source_fence, None)
            .is_err(),
        "source changed after validation; do not publish stale seed"
    );
    assert_eq!(host.inner.records.list()?.len(), 1, "no staged row leaked");

    let result = alice
        .import_archive_ticket(&ticket, &clicked, Some("Archive"))
        .await?;
    assert_ne!(source, result.session_id);
    let saved = transcripts
        .reader(&result.session_id)?
        .collect::<Result<Vec<_>, _>>()?;
    let visible: Vec<_> = saved
        .iter()
        .filter(|row| row.kind == RecordKind::Item)
        .collect();
    assert_eq!(
        visible.iter().map(|r| r.recorded_at).collect::<Vec<_>>(),
        vec![user_time, assistant_time]
    );
    let media = serde_json::to_value(media)?;
    assert_eq!(visible[0].payload["content"][1]["media"], media);
    assert_eq!(visible[1].payload["content"][0]["text"], "visible reply");
    assert!(
        visible.iter().all(
            |row| row.thread.as_str() == format!("test-archive:{}", result.session_id.as_str())
        )
    );
    let target_meta =
        meta::load(&sessions, &result.session_id)?.ok_or("project metadata not committed")?;
    assert_eq!(target_meta.project_key.as_deref(), Some("verified-key"));
    assert_eq!(
        target_meta.project_root.as_deref(),
        Some(workspace.as_path())
    );
    assert_eq!(host.inner.records.list()?.len(), 2);
    assert_eq!(driver.effects.load(Ordering::SeqCst), 0);

    // A crash can leave the archive intent, checked project and hosted
    // record synced but no final transcript. Only that exact hidden seed
    // and its original destination may be resumed.
    let target_record = host
        .inner
        .records
        .load(&result.session_id)?
        .ok_or("target record")?;
    let transaction = SessionId::try_from_str(
        saved[0].payload["transaction"]
            .as_str()
            .ok_or("transaction marker")?,
    )?;
    let destination = ArchiveDestination {
        id: result.session_id.clone(),
        transaction,
        at: target_record.created_at,
    };
    let target_path = transcripts.transcript_path(&destination.id)?;
    let journal = std::fs::read(&target_path)?;
    let intent = ArchivePublicationIntent::new(target_path.clone(), &journal);
    let gate = host
        .inner
        .records
        .root()
        .join(format!("{}.archive-intent", destination.id.as_str()));
    harw_session_store::store::persist_noclobber(&gate, &serde_json::to_vec(&intent)?)?;
    std::fs::remove_file(&target_path)?;
    assert!(host.inner.records.load(&destination.id)?.is_none());
    let (prefix, fence) =
        export_inclusive_guarded_v1(&transcripts, &source, &ticket.revision, &clicked)
            .map_err(archive_error)?;
    let resumed = alice.commit_archive_to(&ticket, prefix, fence, &destination, Some("Archive"))?;
    assert_eq!(resumed.session_id, destination.id);
    assert_eq!(std::fs::read(&target_path)?, journal);
    assert_eq!(host.inner.records.list()?.len(), 2);
    let revoked_ticket = alice.issue_archive_ticket(&source, revision).await?;
    std::fs::remove_dir(&workspace)?;
    assert!(alice.issue_archive_ticket(&source, revision).await.is_err());
    assert!(
        !workspace.exists(),
        "checking archive scope recreated a removed project"
    );
    std::fs::create_dir(&workspace)?;
    host.revoke_connection(alice.identity.connection);
    assert!(matches!(
        alice
            .import_archive_ticket(&revoked_ticket, &clicked, None)
            .await,
        Err(HostError::Revoked)
    ));
    assert_eq!(host.inner.records.list()?.len(), 2);
    Ok(())
}

#[test]
fn archive_import_restart_gate_requires_the_whole_exact_seed() -> TestResult {
    let fixture = tempfile::tempdir()?;
    let sessions = fixture.path().join("sessions");
    let state = fixture.path().join("host");
    std::fs::create_dir(&sessions)?;
    let target = SessionId::new();
    let root = TranscriptStore::new(&sessions);
    let path = root.transcript_path(&target)?;
    let now = jiff::Timestamp::from_second(1_700_000_000)?;
    let thread = ThreadRef::from_str(format!("test-archive:{}", target.as_str()));
    let header = TranscriptRecord::new(
        target.clone(),
        thread.clone(),
        0,
        now,
        RecordKind::Lifecycle,
        json!({"marker":ARCHIVE_PUBLICATION_MARKER}),
    );
    let footer = TranscriptRecord::new(
        target.clone(),
        thread,
        1,
        now,
        RecordKind::Lifecycle,
        json!({"marker":"history_replaced","item_count":0}),
    );
    let partial = header.to_jsonl_line()?;
    let journal = partial.clone() + &footer.to_jsonl_line()?;
    let staged = RecordStore::new(&state);
    staged.reserve_archive_publication(
        &target,
        &ArchivePublicationIntent::new(path.clone(), journal.as_bytes()),
    )?;
    let record = HostedSessionRecord {
        session_id: target.clone(),
        tenant: None,
        owner_principal: "test".into(),
        fresh_human_origin: false,
        creator_execution: None,
        manual_cloud_work_receipt: None,
        exact_rewrites: Vec::new(),
        workspace: None,
        title: Some("staged archive".into()),
        created_at: now,
        updated_at: now,
        state: HostedState::Idle,
        host_epoch: 1,
        generation: 0,
        owner_agent: None,
        tool_agents: vec![],
    };
    assert_eq!(staged.create_if_absent(&record)?, CreateOutcome::Created);
    drop(staged);
    let restored = RecordStore::new(&state);
    assert!(restored.load(&target)?.is_none());
    assert!(restored.list()?.is_empty());
    std::fs::write(&path, partial.as_bytes())?;
    assert!(
        restored.load(&target)?.is_none(),
        "ready marker alone is not a committed seed"
    );
    let invalid = partial
        + &footer
            .to_jsonl_line()?
            .replace("history_replaced", "history_replacex");
    std::fs::write(&path, invalid.as_bytes())?;
    assert!(
        restored.list()?.is_empty(),
        "a wrong full-length journal cannot publish"
    );
    std::fs::remove_file(&path)?;
    harw_session_store::store::persist_noclobber(&path, journal.as_bytes())?;
    assert_eq!(restored.load(&target)?, Some(record.clone()));
    assert_eq!(restored.list()?, vec![record]);
    Ok(())
}
