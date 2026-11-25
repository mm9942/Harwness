//! Acceptance of the minor-4 `manage`/`stepup` admission in the host
//! (contract K-W3c §8.2, integration D5). Synthetic ids and temp dirs only;
//! the operations backend is a counting fake.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use harw_protocol::session_wire::{
    CatalogGetParams, CatalogResult, CommandEffect, CommandRunParams, CommandRunResult,
    CommandsListParams, CommandsListResult, ConfigGetParams, ConfigGetResult, ConfigResetParams,
    ConfigSetParams, HelloAck, HelloParams, MANAGE_WIRE_MINOR, PageParams, ProjectsParams,
    SecretPutParams, SecretPutResult, StepUpCancelParams, StepUpConfirmParams, features,
};
use harw_protocol::{PortError, SessionPort};
use harw_types::{PermissionTier, SessionId, TenantId};

use super::{HostConfig, HostConnection, SessionHost};
use crate::approvals::MemoryApprovals;
use crate::driver::{
    CancelSignal, DriverFuture, EventSink, ExecutionAdmission, Setting, TurnDriver, TurnInput,
    TurnOutcome,
};
use crate::error::HostError;
use crate::identity::ClientIdentity;
use crate::identity::test_identity::scoped;
use crate::index::{
    ProfileIndex, ProfilePage, ProfileRow, ProfileShare, ProjectPage, ResolvedProject, ShareMode,
    ShareSettings, ThreadOrigin,
};
use crate::ops::{OpsCaller, OpsPort, OpsRun, OpsSession, SessionAction};
use crate::replay::MemoryTranscripts;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Minimal runtime: admission and creation succeed, turns complete.
struct QuietDriver;

impl TurnDriver for QuietDriver {
    fn prepare_execution(&self, _: &SessionId, _: ExecutionAdmission) -> DriverFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn create_session(&self, _: &SessionId, _: Option<&str>) -> DriverFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }

    fn run_turn(
        &self,
        _: TurnInput,
        _: CancelSignal,
        _: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        Box::pin(async { Ok(TurnOutcome::Completed) })
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

/// Scripted profile index over a temp profile store.
struct FakeIndex {
    root: PathBuf,
    rows: Mutex<Vec<ProfileRow>>,
    workdirs: Mutex<HashMap<SessionId, PathBuf>>,
}

impl ProfileIndex for FakeIndex {
    fn sessions_root(&self) -> &Path {
        &self.root
    }

    fn projects(&self, _: &ProjectsParams) -> Result<ProjectPage, HostError> {
        Ok(ProjectPage {
            projects: Vec::new(),
            next_cursor: None,
        })
    }

    fn page(&self, _: &PageParams) -> Result<ProfilePage, HostError> {
        Ok(ProfilePage {
            rows: self.newest(usize::MAX)?,
            next_cursor: None,
        })
    }

    fn row(&self, id: &SessionId) -> Result<Option<ProfileRow>, HostError> {
        Ok(self
            .rows
            .lock()
            .map_err(|_| HostError::Storage("rows poisoned".into()))?
            .iter()
            .find(|row| &row.session_id == id)
            .cloned())
    }

    fn newest(&self, limit: usize) -> Result<Vec<ProfileRow>, HostError> {
        let mut rows: Vec<ProfileRow> = self
            .rows
            .lock()
            .map_err(|_| HostError::Storage("rows poisoned".into()))?
            .iter()
            .filter(|row| !row.archived)
            .cloned()
            .collect();
        rows.sort_by_key(|row| std::cmp::Reverse(row.updated_at));
        rows.truncate(limit);
        Ok(rows)
    }

    fn resolve_project(&self, _: &str) -> Result<Option<ResolvedProject>, HostError> {
        Ok(None)
    }

    fn workdir(&self, id: &SessionId) -> Result<Option<PathBuf>, HostError> {
        Ok(self
            .workdirs
            .lock()
            .map_err(|_| HostError::Storage("workdirs poisoned".into()))?
            .get(id)
            .cloned())
    }
}

fn unused<T: 'static>() -> DriverFuture<'static, T> {
    Box::pin(async { Err(HostError::Denied("fake ops: method not scripted".into())) })
}

/// Counting operations backend. Only `command_run` and `connection_closed`
/// are scripted; every other method answers an error and counts the call.
struct FakeOps {
    stepup: bool,
    run: Mutex<OpsRun>,
    calls: AtomicUsize,
    runs: Mutex<Vec<(OpsCaller, Option<OpsSession>)>>,
    closed: Mutex<Vec<String>>,
}

impl FakeOps {
    fn new(stepup: bool, run: OpsRun) -> Arc<Self> {
        Arc::new(Self {
            stepup,
            run: Mutex::new(run),
            calls: AtomicUsize::new(0),
            runs: Mutex::new(Vec::new()),
            closed: Mutex::new(Vec::new()),
        })
    }

    fn count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn bump(&self) {
        self.calls.fetch_add(1, Ordering::SeqCst);
    }
}

impl OpsPort for FakeOps {
    fn stepup_enabled(&self) -> bool {
        self.stepup
    }

    fn commands_list(
        &self,
        _: OpsCaller,
        _: Option<OpsSession>,
        _: CommandsListParams,
    ) -> DriverFuture<'_, CommandsListResult> {
        self.bump();
        unused()
    }

    fn command_run(
        &self,
        caller: OpsCaller,
        session: Option<OpsSession>,
        _: CommandRunParams,
    ) -> DriverFuture<'_, OpsRun> {
        self.bump();
        if let Ok(mut runs) = self.runs.lock() {
            runs.push((caller, session));
        }
        let run = self
            .run
            .lock()
            .map(|run| run.clone())
            .map_err(|_| HostError::Storage("run poisoned".into()));
        Box::pin(async move { run })
    }

    fn catalog_get(
        &self,
        _: OpsCaller,
        _: Option<OpsSession>,
        _: CatalogGetParams,
    ) -> DriverFuture<'_, CatalogResult> {
        self.bump();
        unused()
    }

    fn config_get(&self, _: OpsCaller, _: ConfigGetParams) -> DriverFuture<'_, ConfigGetResult> {
        self.bump();
        unused()
    }

    fn config_set(&self, _: OpsCaller, _: ConfigSetParams) -> DriverFuture<'_, CommandRunResult> {
        self.bump();
        unused()
    }

    fn config_reset(
        &self,
        _: OpsCaller,
        _: ConfigResetParams,
    ) -> DriverFuture<'_, CommandRunResult> {
        self.bump();
        unused()
    }

    fn secret_put(&self, _: OpsCaller, _: SecretPutParams) -> DriverFuture<'_, SecretPutResult> {
        self.bump();
        unused()
    }

    fn stepup_confirm(
        &self,
        _: OpsCaller,
        _: StepUpConfirmParams,
    ) -> DriverFuture<'_, CommandRunResult> {
        self.bump();
        unused()
    }

    fn stepup_cancel(&self, _: OpsCaller, _: StepUpCancelParams) -> DriverFuture<'_, ()> {
        self.bump();
        unused()
    }

    fn connection_closed(&self, identity: &ClientIdentity) {
        if let Ok(mut closed) = self.closed.lock() {
            closed.push(identity.label.clone());
        }
    }
}

struct Fixture {
    _state: tempfile::TempDir,
    profile: tempfile::TempDir,
    host: SessionHost,
    index: Arc<FakeIndex>,
    tenant: TenantId,
}

fn tenant() -> TestResult<TenantId> {
    Ok(TenantId::try_from_str("tenant-test")?)
}

fn fixture() -> TestResult<Fixture> {
    let state = tempfile::tempdir()?;
    let profile = tempfile::tempdir()?;
    let tenant = tenant()?;
    let index = Arc::new(FakeIndex {
        root: profile.path().to_path_buf(),
        rows: Mutex::new(Vec::new()),
        workdirs: Mutex::new(HashMap::new()),
    });
    let host = SessionHost::open_with_profile(
        HostConfig::new(state.path().to_path_buf()),
        Arc::new(QuietDriver) as Arc<dyn TurnDriver>,
        Arc::new(MemoryTranscripts::new()),
        Arc::new(MemoryApprovals::new()),
        crate::tool_host::ToolHost::disabled(),
        ProfileShare {
            settings: ShareSettings {
                mode: ShareMode::Continue,
                tenant: tenant.clone(),
                legacy_rows: 100,
                listener_tier: PermissionTier::Operator,
            },
            index: Arc::clone(&index) as Arc<dyn ProfileIndex>,
        },
    )?;
    Ok(Fixture {
        _state: state,
        profile,
        host,
        index,
        tenant,
    })
}

impl Fixture {
    /// A continuable profile session of the share tenant.
    fn add_row(&self, title: &str) -> TestResult<SessionId> {
        let id = SessionId::new();
        self.index
            .rows
            .lock()
            .map_err(|_| "rows poisoned")?
            .push(ProfileRow {
                session_id: id.clone(),
                title: title.to_owned(),
                stored_title: Some(title.to_owned()),
                project_key: None,
                project_label: None,
                updated_at: jiff::Timestamp::from_millisecond(1_764_000_000_000)?,
                turns: 1,
                first_message: Some("hallo".into()),
                origin: ThreadOrigin::Cli,
                last_origin: None,
                archived: false,
            });
        self.index
            .workdirs
            .lock()
            .map_err(|_| "workdirs poisoned")?
            .insert(id.clone(), self.profile.path().to_path_buf());
        Ok(id)
    }

    fn install(&self, ops: &Arc<FakeOps>) -> TestResult {
        self.host.install_ops(Arc::clone(ops) as Arc<dyn OpsPort>)?;
        Ok(())
    }
}

fn done(text: &str) -> OpsRun {
    OpsRun::Done(CommandRunResult {
        ok: true,
        text: text.into(),
        data: None,
        effect: CommandEffect::Applied,
        reason_code: None,
        stepup: None,
    })
}

fn stop_run() -> OpsRun {
    OpsRun::Session {
        action: SessionAction::Interrupt,
        result: CommandRunResult {
            ok: true,
            text: "stopped".into(),
            data: None,
            effect: CommandEffect::Applied,
            reason_code: None,
            stepup: None,
        },
    }
}

async fn hello(
    host: &SessionHost,
    identity: ClientIdentity,
    wire_minor: u32,
) -> TestResult<(HostConnection, HelloAck)> {
    let connection = host.connect(identity)?;
    let ack = connection
        .hello(HelloParams {
            client_label: "test".into(),
            wire_minor,
            features: vec![features::MANAGE.to_owned(), features::STEPUP.to_owned()],
            requested_caps: None,
        })
        .await?;
    Ok((connection, ack))
}

fn has(ack: &HelloAck, feature: &str) -> bool {
    ack.features.iter().any(|granted| granted == feature)
}

fn run_params(session: Option<&SessionId>) -> CommandRunParams {
    CommandRunParams {
        session_id: session.cloned(),
        line: "/stop".into(),
        client_msg_id: "msg-1".into(),
    }
}

#[tokio::test]
async fn hello_grants_manage_only_with_ops_and_minor4() -> TestResult {
    let fx = fixture()?;
    let operator = || scoped(1, PermissionTier::Operator, &fx.tenant);

    // No ops installed: nothing granted, even at minor 4.
    let (_none, ack) = hello(&fx.host, operator(), MANAGE_WIRE_MINOR).await?;
    assert!(!has(&ack, features::MANAGE));
    assert!(!has(&ack, features::STEPUP));

    fx.install(&FakeOps::new(true, done("ok")))?;

    // Ops installed, but an older client.
    let (_old, ack) = hello(&fx.host, operator(), MANAGE_WIRE_MINOR - 1).await?;
    assert!(!has(&ack, features::MANAGE));
    assert!(!has(&ack, features::STEPUP));

    // Ops installed and minor 4: both.
    let (_new, ack) = hello(&fx.host, operator(), MANAGE_WIRE_MINOR).await?;
    assert!(has(&ack, features::MANAGE));
    assert!(has(&ack, features::STEPUP));
    Ok(())
}

#[tokio::test]
async fn observer_config_set_is_denied() -> TestResult {
    let fx = fixture()?;
    let ops = FakeOps::new(true, done("ok"));
    fx.install(&ops)?;
    let (observer, ack) = hello(
        &fx.host,
        scoped(1, PermissionTier::Observer, &fx.tenant),
        MANAGE_WIRE_MINOR,
    )
    .await?;
    assert!(has(&ack, features::STEPUP), "denial must come from caps");
    let result = observer
        .config_set(ConfigSetParams {
            key: "ui.theme".into(),
            value: "dark".into(),
            client_msg_id: "msg-1".into(),
        })
        .await;
    assert!(matches!(result, Err(PortError::Denied(_))), "{result:?}");
    assert_eq!(ops.count(), 0, "the backend must not be reached");
    Ok(())
}

#[tokio::test]
async fn stop_via_command_run_interrupts_hosted_turn() -> TestResult {
    let fx = fixture()?;
    let ops = FakeOps::new(true, stop_run());
    fx.install(&ops)?;
    let id = fx.add_row("titel")?;
    let (caller, _ack) = hello(
        &fx.host,
        scoped(1, PermissionTier::Operator, &fx.tenant),
        MANAGE_WIRE_MINOR,
    )
    .await?;

    // Not hosted yet: the host refuses the session action itself.
    let refused = caller.command_run(run_params(Some(&id))).await?;
    assert!(!refused.ok);
    assert_eq!(refused.effect, CommandEffect::Refused);
    assert_eq!(
        refused.reason_code.as_deref(),
        Some(harw_protocol::session_wire::refusal_codes::SESSION_NOT_HOSTED)
    );

    // After adoption the host has a record and runs the interrupt.
    caller.adopt(id.clone()).await?;
    let result = caller.command_run(run_params(Some(&id))).await?;
    assert!(result.ok, "{result:?}");
    assert_eq!(result.text, "stopped");

    let runs = ops.runs.lock().map_err(|_| "runs poisoned")?;
    assert_eq!(runs.len(), 2);
    let session = runs[0].1.as_ref().ok_or("session missing")?;
    assert!(!session.hosted);
    let session = runs[1].1.as_ref().ok_or("session missing")?;
    assert!(session.hosted);
    assert_eq!(session.session_id, id);
    assert_eq!(runs[1].0.tier, PermissionTier::Operator);
    assert_eq!(runs[1].0.profile_tenant.as_ref(), Some(&fx.tenant));
    Ok(())
}

#[tokio::test]
async fn foreign_tenant_command_run_is_not_found() -> TestResult {
    let fx = fixture()?;
    let ops = FakeOps::new(true, stop_run());
    fx.install(&ops)?;
    let id = fx.add_row("titel")?;
    let foreign = TenantId::try_from_str("tenant-other")?;

    // A foreign tenant never reaches the backend and learns nothing.
    let (stranger, _ack) = hello(
        &fx.host,
        scoped(1, PermissionTier::Operator, &foreign),
        MANAGE_WIRE_MINOR,
    )
    .await?;
    let result = stranger.command_run(run_params(Some(&id))).await;
    assert!(
        matches!(result, Err(PortError::NotFound | PortError::Denied(_))),
        "{result:?}"
    );

    // The share tenant asking for an unknown session reads as not found.
    let (own, _ack) = hello(
        &fx.host,
        scoped(2, PermissionTier::Operator, &fx.tenant),
        MANAGE_WIRE_MINOR,
    )
    .await?;
    let result = own.command_run(run_params(Some(&SessionId::new()))).await;
    assert!(matches!(result, Err(PortError::NotFound)), "{result:?}");
    assert_eq!(ops.count(), 0);
    Ok(())
}

#[tokio::test]
async fn stepup_feature_requires_stepup_enabled() -> TestResult {
    let off = fixture()?;
    off.install(&FakeOps::new(false, done("ok")))?;
    let (_conn, ack) = hello(
        &off.host,
        scoped(1, PermissionTier::Operator, &off.tenant),
        MANAGE_WIRE_MINOR,
    )
    .await?;
    assert!(has(&ack, features::MANAGE));
    assert!(!has(&ack, features::STEPUP));

    let on = fixture()?;
    on.install(&FakeOps::new(true, done("ok")))?;
    let (_conn, ack) = hello(
        &on.host,
        scoped(1, PermissionTier::Operator, &on.tenant),
        MANAGE_WIRE_MINOR,
    )
    .await?;
    assert!(has(&ack, features::MANAGE));
    assert!(has(&ack, features::STEPUP));
    Ok(())
}

#[tokio::test]
async fn install_ops_twice_is_refused() -> TestResult {
    let fx = fixture()?;
    let first = FakeOps::new(true, done("first"));
    let second = FakeOps::new(true, done("second"));
    fx.install(&first)?;
    let again = fx.host.install_ops(Arc::clone(&second) as Arc<dyn OpsPort>);
    assert!(matches!(again, Err(HostError::Protocol(_))), "{again:?}");

    // The first backend stays in place.
    let (caller, _ack) = hello(
        &fx.host,
        scoped(1, PermissionTier::Operator, &fx.tenant),
        MANAGE_WIRE_MINOR,
    )
    .await?;
    let result = caller.command_run(run_params(None)).await?;
    assert_eq!(result.text, "first");
    assert_eq!(first.count(), 1);
    assert_eq!(second.count(), 0);
    Ok(())
}

#[tokio::test]
async fn drop_calls_connection_closed() -> TestResult {
    let fx = fixture()?;
    let ops = FakeOps::new(true, done("ok"));
    fx.install(&ops)?;
    let identity = scoped(7, PermissionTier::Operator, &fx.tenant);
    let label = identity.label.clone();
    let (connection, _ack) = hello(&fx.host, identity, MANAGE_WIRE_MINOR).await?;
    assert!(ops.closed.lock().map_err(|_| "closed poisoned")?.is_empty());
    drop(connection);
    let closed = ops.closed.lock().map_err(|_| "closed poisoned")?;
    assert_eq!(closed.as_slice(), [label]);
    Ok(())
}
