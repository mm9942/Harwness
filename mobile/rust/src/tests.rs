use super::*;
use harw_node_transport::ML_DSA_65_PUBLIC_KEY_LEN;

#[test]
fn identity_round_trip_exports_only_public_fields() -> Result<(), MobileError> {
    let material = generate_identity("Phone".into())?;
    assert_eq!(material.private_key.len(), 32);
    let core = MobileCore::new(material.private_key, "Phone".into())?;
    assert_eq!(core.enrollment(), material.enrollment);
    let json = core.export_enrollment_json()?;
    assert_eq!(parse_device_enrollment(&json)?, material.enrollment);
    assert_eq!(
        material.enrollment.public_key_hex.len(),
        ML_DSA_65_PUBLIC_KEY_LEN * 2
    );
    let object: serde_json::Value = serde_json::from_str(&json).expect("valid enrollment JSON");
    assert_eq!(object.as_object().expect("JSON object").len(), 4);
    assert!(MobileCore::new(vec![0; 31], "Phone".into()).is_err());
    Ok(())
}

#[test]
fn profile_checks_version_endpoint_and_public_pin() -> Result<(), MobileError> {
    let enrollment = generate_identity("Host".into())?.enrollment;
    let mut profile = ServerProfile {
        version: 1,
        label: enrollment.label,
        endpoint: "[::1]:7410".into(),
        server_node_id: enrollment.node_id,
        server_public_key_hex: enrollment.public_key_hex,
    };
    assert_eq!(parse_server_profile(profile_json(&profile))?, profile);
    profile.server_public_key_hex.pop();
    assert!(matches!(
        parse_server_profile(profile_json(&profile)),
        Err(MobileError::InvalidProfile { .. })
    ));
    profile.server_public_key_hex.push('x');
    assert!(parse_server_profile(profile_json(&profile)).is_err());
    profile.server_public_key_hex.pop();
    profile.server_public_key_hex.push('0');
    for endpoint in ["https://127.0.0.1:7410", "0.0.0.0:7410", "127.0.0.1:0"] {
        profile.endpoint = endpoint.into();
        assert!(parse_server_profile(profile_json(&profile)).is_err());
    }
    profile.endpoint = "127.0.0.1:7410".into();
    profile.version = 0;
    assert!(parse_server_profile(profile_json(&profile)).is_err());
    Ok(())
}

fn profile_json(profile: &ServerProfile) -> String {
    serde_json::to_string(profile).expect("profile can be encoded")
}

#[test]
fn provider_handoff_rejects_missing_plan_scope_and_oversized_tokens() {
    let fixture = || ChatgptProviderCredential {
        client_id: "fixture-client".into(),
        access_token: "fixture-access".into(),
        refresh_token: "fixture-refresh".into(),
        id_token: "header.payload.signature".into(),
        scope: "openid offline_access chatgpt.tokens.use.direct resource.invoke".into(),
        expires_at: u64::try_from(jiff::Timestamp::now().as_second()).unwrap_or(0) + 60,
        earliest_refresh_at: None,
        subject: "fixture-subject".into(),
        email: None,
    };
    let mut no_plan_scope = fixture();
    no_plan_scope.scope = "openid offline_access resource.invoke".into();
    assert!(no_plan_scope.request_body().is_err());

    let mut oversized = fixture();
    oversized.refresh_token = "a".repeat(65_537);
    assert!(oversized.request_body().is_err());
    assert!(fixture().request_body().is_ok());
}

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use harw_node_listener::{DeviceRecord, DeviceRegistry, NodeListener, RegistryIdentityMapper};
use harw_node_transport::{
    NodeTransportServer, PinnedPeers, ServerOptions, TranscriptWrappedVerifier,
};
use harw_protocol::items::{AssistantMessageItem, ContentPart, TurnItem, UserMessageItem};
use harw_protocol::{ApprovalKind, ApprovalRequest, TurnEvent};
use harw_session_host::approvals::{ApprovalBackend, MemoryApprovals};
use harw_session_host::driver::{
    CancelSignal, DriverEvent, DriverFuture, EventSink, ExecutionAdmission, Setting, TurnDriver,
    TurnInput, TurnOutcome,
};
use harw_session_host::replay::{MemoryTranscripts, TranscriptSource};
use harw_session_host::{HostConfig, HostError, SessionHost};
use harw_session_store::{RecordKind, TranscriptRecord};
use harw_session_ws::WsLimits;
use harw_types::{
    ApprovalId, DeviceId, ItemId, NodeId, PermissionTier, ReviewDecision, RiskLevel, SessionId,
    TenantId, ThreadId, ThreadRef, TurnId, WorkId,
};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const WAIT: Duration = Duration::from_secs(8);

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct Driver {
    calls: AtomicUsize,
    transcripts: Arc<MemoryTranscripts>,
    approvals: Arc<MemoryApprovals>,
}

impl Driver {
    fn persist(&self, session: &SessionId, item: &TurnItem) -> Result<(), HostError> {
        let sequence = self.transcripts.head(session)?;
        let payload =
            serde_json::to_value(item).map_err(|error| HostError::Storage(error.to_string()))?;
        self.transcripts.append(TranscriptRecord::new(
            session.clone(),
            ThreadRef::from_str("root"),
            sequence,
            jiff::Timestamp::now(),
            RecordKind::Item,
            payload,
        ));
        Ok(())
    }
}

impl TurnDriver for Driver {
    fn prepare_execution(&self, _: &SessionId, _: ExecutionAdmission) -> DriverFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn create_session(&self, _: &SessionId, _: Option<&str>) -> DriverFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn run_turn(
        &self,
        input: TurnInput,
        _: CancelSignal,
        sink: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.persist(
                &input.session_id,
                &TurnItem::UserMessage(UserMessageItem {
                    id: ItemId::new(),
                    content: vec![ContentPart::Text {
                        text: input.text.clone(),
                    }],
                }),
            )?;
            let turn_id = TurnId::new();
            sink.emit(DriverEvent::Turn(TurnEvent::TurnStarted {
                turn_id: turn_id.clone(),
                thread_id: ThreadId::new(),
            }));
            if input.text == "request approval" {
                let requested_at = jiff::Timestamp::now();
                let request = ApprovalRequest {
                    id: ApprovalId::new(),
                    work_id: WorkId::new(),
                    kind: ApprovalKind::DynamicTool {
                        turn_id,
                        tool_name: "fixture".into(),
                        arguments: serde_json::Value::Null,
                    },
                    summary: "Approval fixture".into(),
                    risk: RiskLevel::Low,
                    requested_at,
                    timeout_at: ApprovalRequest::default_timeout_at(requested_at),
                    decisions: vec![ReviewDecision::Rejected],
                };
                self.approvals.issue(&input.session_id, request.clone())?;
                sink.emit(DriverEvent::ApprovalRequested(request));
                return Ok(TurnOutcome::AwaitingApproval);
            }
            let text = format!("Reply to {}", input.text);
            sink.emit(DriverEvent::Turn(TurnEvent::AssistantDelta {
                turn_id: turn_id.clone(),
                text: text.clone(),
            }));
            let mut item = AssistantMessageItem {
                id: ItemId::new(),
                content: vec![ContentPart::Text { text }],
                phase: None,
            };
            self.persist(&input.session_id, &TurnItem::AssistantMessage(item.clone()))?;
            // The real driver uses distinct IDs for the stored item and the
            // streaming notification.
            item.id = ItemId::new();
            sink.emit(DriverEvent::Turn(TurnEvent::ItemAdded {
                turn_id: turn_id.clone(),
                item: TurnItem::AssistantMessage(item),
            }));
            sink.emit(DriverEvent::Turn(TurnEvent::TurnCompleted {
                turn_id,
                usage: None,
            }));
            Ok(TurnOutcome::Completed)
        })
    }

    fn resume_after_approval(
        &self,
        _: &SessionId,
        _: CancelSignal,
        _: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        Box::pin(async { Ok(TurnOutcome::Completed) })
    }

    fn apply_setting(&self, _: &SessionId, _: Setting) -> DriverFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn model_name(&self, _: &SessionId) -> Option<String> {
        None
    }
}

struct HostFixture {
    core: Arc<MobileCore>,
    profile: ServerProfile,
    driver: Arc<Driver>,
    host: Arc<SessionHost>,
    listener: JoinHandle<()>,
    dir: tempfile::TempDir,
}

impl HostFixture {
    async fn start() -> TestResult<Self> {
        let dir = tempfile::tempdir()?;
        let phone = generate_identity("Phone".into())?;
        let (server, local) = crate::identity::restore_identity(&[11; 32], "Server".into())?;
        let phone_id = NodeId::try_from_str(phone.enrollment.node_id)?;
        let mut pins = PinnedPeers::new();
        pins.pin(
            phone_id.clone(),
            hex::decode(phone.enrollment.public_key_hex)?,
        )?;
        let server_transport = NodeTransportServer::new(
            local,
            Arc::new(TranscriptWrappedVerifier::new(pins)),
            ServerOptions::default(),
        )?;
        DeviceRegistry::new(dir.path()).enroll(&DeviceRecord {
            node_id: phone_id,
            device: DeviceId::try_from_str("phone-device")?,
            tenant: TenantId::try_from_str("test-tenant")?,
            tier: PermissionTier::Operator,
            revoked: false,
            label: "Phone".into(),
            approve_optin: false,
        })?;
        let driver = Arc::new(Driver {
            calls: AtomicUsize::new(0),
            transcripts: Arc::new(MemoryTranscripts::new()),
            approvals: Arc::new(MemoryApprovals::new()),
        });
        let host = Arc::new(SessionHost::open(
            HostConfig::new(dir.path().join("host")),
            driver.clone(),
            driver.transcripts.clone(),
            driver.approvals.clone(),
        )?);
        let node_listener = NodeListener::new(
            server_transport,
            Arc::clone(&host),
            Arc::new(RegistryIdentityMapper::new(dir.path())),
            WsLimits::default(),
        );
        let socket = TcpListener::bind("127.0.0.1:0").await?;
        let profile = ServerProfile {
            version: 1,
            label: server.label,
            endpoint: socket.local_addr()?.to_string(),
            server_node_id: server.node_id,
            server_public_key_hex: server.public_key_hex,
        };
        let listener = tokio::spawn(async move {
            let _ = node_listener.serve(socket, std::future::pending()).await;
        });
        let core = MobileCore::new(phone.private_key, "Phone".into())?;
        Ok(Self {
            core,
            profile,
            driver,
            host,
            listener,
            dir,
        })
    }
}

impl Drop for HostFixture {
    fn drop(&mut self) {
        self.core.disconnect();
        self.listener.abort();
    }
}

async fn wait_for(
    core: &MobileCore,
    id: &str,
    ready: impl Fn(&SessionSnapshot) -> bool,
) -> TestResult<SessionSnapshot> {
    Ok(tokio::time::timeout(WAIT, async {
        loop {
            let snapshot = core.snapshot(id.to_owned())?;
            if ready(&snapshot) {
                return Ok::<_, MobileError>(snapshot);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_pin_never_establishes_a_session_connection() -> TestResult {
    let fixture = HostFixture::start().await?;
    let mut wrong = fixture.profile.clone();
    wrong.server_public_key_hex = generate_identity("Other server".into())?
        .enrollment
        .public_key_hex;
    assert!(matches!(
        fixture.core.connect(wrong),
        Err(MobileError::Connect { .. })
    ));
    assert_eq!(fixture.core.status().phase, ConnectionPhase::Failed);
    assert!(matches!(
        fixture.core.list_sessions(),
        Err(MobileError::Disconnected)
    ));
    fixture.core.connect(fixture.profile.clone())?;
    assert!(fixture.core.list_sessions()?.is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn provider_status_rejects_a_consent_profile_with_a_different_pin() -> TestResult {
    let fixture = HostFixture::start().await?;
    fixture.core.connect(fixture.profile.clone())?;
    let mut other_profile = fixture.profile.clone();
    other_profile.server_public_key_hex = generate_identity("Other server".into())?
        .enrollment
        .public_key_hex;
    assert!(matches!(
        fixture.core.chatgpt_provider_status(other_profile),
        Err(MobileError::InvalidProfile { .. })
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn independent_sessions_resume_with_no_duplicate_submit_and_stale_cas() -> TestResult {
    let fixture = HostFixture::start().await?;
    let core = &fixture.core;
    core.connect(fixture.profile.clone())?;
    let first = core.create_session(Some("First".into()))?.session_id;
    let second = core.create_session(Some("Second".into()))?.session_id;
    core.open_session(first.clone())?;
    core.open_session(second.clone())?;
    let first_head = wait_for(core, &first, |view| view.can_submit)
        .await?
        .head
        .ok_or("first head")?;
    let second_head = wait_for(core, &second, |view| view.can_submit)
        .await?
        .head
        .ok_or("second head")?;
    let send = |id: &str, text: &str, head, key: &str| {
        core.submit(id.into(), text.into(), head, key.into())
    };
    assert_eq!(
        send(&first, "one", first_head, "first-draft")?.outcome,
        SubmitOutcome::Accepted
    );
    assert_eq!(
        send(&second, "two", second_head, "first-draft")?.outcome,
        SubmitOutcome::Accepted
    );
    let first_view = wait_for(core, &first, |view| {
        view.online
            && view
                .transcript
                .iter()
                .any(|entry| entry.text == "one" && !entry.id.starts_with("mobile-"))
    })
    .await?;
    let second_view = wait_for(core, &second, |view| {
        view.online
            && view
                .transcript
                .iter()
                .any(|entry| entry.text == "two" && !entry.id.starts_with("mobile-"))
    })
    .await?;
    assert!(
        first_view
            .transcript
            .iter()
            .all(|entry| entry.text != "Reply to two")
    );
    assert!(
        second_view
            .transcript
            .iter()
            .all(|entry| entry.text != "Reply to one")
    );
    assert_eq!(
        first_view
            .transcript
            .iter()
            .filter(|entry| entry.text == "Reply to one")
            .count(),
        1
    );
    assert_eq!(
        send(&first, "stale", first_head, "stale-draft")?.outcome,
        SubmitOutcome::Stale
    );
    core.disconnect();
    assert!(!core.snapshot(first.clone())?.online);
    assert!(matches!(
        send(&first, "offline", first_head, "offline-draft"),
        Err(MobileError::Disconnected)
    ));
    core.reconnect()?;
    let resumed = wait_for(core, &first, |view| view.online).await?;
    let mut relabeled = fixture.profile.clone();
    relabeled.label = "Same server, new label".into();
    core.connect(relabeled)?;
    assert_eq!(
        send(
            &first,
            "one",
            resumed.head.ok_or("resumed head")?,
            "first-draft"
        )?
        .outcome,
        SubmitOutcome::Accepted
    );
    assert_eq!(
        resumed
            .transcript
            .iter()
            .filter(|entry| entry.text == "Reply to one")
            .count(),
        1
    );
    wait_for(core, &second, |view| view.online).await?;
    assert_eq!(fixture.driver.calls.load(Ordering::SeqCst), 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pending_approval_is_visible_but_phone_without_grant_cannot_resolve_it() -> TestResult {
    let fixture = HostFixture::start().await?;
    let core = &fixture.core;
    core.connect(fixture.profile.clone())?;
    let id = core.create_session(None)?.session_id;
    core.open_session(id.clone())?;
    let head = wait_for(core, &id, |view| view.can_submit)
        .await?
        .head
        .ok_or("session head")?;
    assert_eq!(
        core.submit(
            id.clone(),
            "request approval".into(),
            head,
            "approval-draft".into()
        )?
        .outcome,
        SubmitOutcome::Accepted
    );
    let snapshot = wait_for(core, &id, |view| view.online && !view.approvals.is_empty()).await?;
    assert!(!snapshot.can_approve);
    assert!(matches!(
        core.decide(
            id.clone(),
            snapshot.approvals[0].id.clone(),
            ApprovalDecision::Rejected
        ),
        Err(MobileError::Denied { .. })
    ));
    assert_eq!(
        fixture
            .driver
            .approvals
            .pending(&SessionId::try_from_str(id)?)?
            .len(),
        1
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registry_denial_during_reconnect_preserves_identity_and_view() -> TestResult {
    let fixture = HostFixture::start().await?;
    let core = &fixture.core;
    core.connect(fixture.profile.clone())?;
    let enrollment = core.enrollment();
    let id = core.create_session(None)?.session_id;
    core.open_session(id.clone())?;
    wait_for(core, &id, |view| view.online).await?;

    let registry = DeviceRegistry::new(fixture.dir.path());
    let backup = fixture.dir.path().join("temporarily-unavailable-registry");
    std::fs::rename(registry.path(), &backup)?;
    assert!(matches!(
        core.reconnect(),
        Err(MobileError::Connect { detail }) if detail.contains("status 403")
    ));
    let status = core.status();
    assert_eq!(status.phase, ConnectionPhase::Failed);
    assert_eq!(
        status.endpoint.as_deref(),
        Some(fixture.profile.endpoint.as_str())
    );
    assert_eq!(core.enrollment(), enrollment);
    assert!(!core.snapshot(id.clone())?.online);

    std::fs::rename(backup, registry.path())?;
    core.reconnect()?;
    assert_eq!(core.enrollment(), enrollment);
    wait_for(core, &id, |view| view.online).await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn policy_invalidation_clears_session_content_and_allows_explicit_reconnect() -> TestResult {
    let fixture = HostFixture::start().await?;
    let core = &fixture.core;
    core.connect(fixture.profile.clone())?;
    let id = core.create_session(None)?.session_id;
    core.open_session(id.clone())?;
    let head = wait_for(core, &id, |view| view.can_submit)
        .await?
        .head
        .ok_or("session head")?;
    let send = |head| {
        core.submit(
            id.clone(),
            "before invalidation".into(),
            head,
            "before-invalidation".into(),
        )
    };
    assert_eq!(send(head)?.outcome, SubmitOutcome::Accepted);
    wait_for(core, &id, |view| {
        view.online
            && view.transcript.iter().any(|entry| {
                entry.text == "before invalidation" && !entry.id.starts_with("mobile-")
            })
    })
    .await?;
    let enrollment = core.enrollment();
    fixture
        .host
        .invalidate_device_policy(&DeviceId::try_from_str("phone-device")?);
    let status = tokio::time::timeout(WAIT, async {
        loop {
            let status = core.status();
            if status.phase == ConnectionPhase::Revoked {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await?;
    assert_eq!(
        status.endpoint.as_deref(),
        Some(fixture.profile.endpoint.as_str())
    );
    assert!(!status.can_submit);
    assert!(core.snapshot(id.clone()).is_err());
    assert_eq!(core.enrollment(), enrollment);
    core.disconnect();
    assert_eq!(core.status().phase, ConnectionPhase::Revoked);
    assert!(matches!(core.list_sessions(), Err(MobileError::Revoked)));
    core.reconnect()?;
    core.open_session(id.clone())?;
    let head = wait_for(core, &id, |view| view.can_submit)
        .await?
        .head
        .ok_or("resumed session head")?;
    assert_eq!(send(head)?.outcome, SubmitOutcome::Accepted);
    assert_eq!(fixture.driver.calls.load(Ordering::SeqCst), 1);
    Ok(())
}
