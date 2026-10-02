//! Test-Fehlertyp dieses Crates: ersetzt `panic!`/`unwrap`/`expect` in Tests
//! (Rust Coding Bible R087/R165/R182). Tests geben [`TestResult`] zurück und
//! melden Fehlschläge als `Err` statt zu paniken.

harw_test_support::define_test_error!(pub(crate));

// ── Gemeinsame Test-Bausteine dieses Crates ───────────────────────────────────

use crate::event::RecordingNotifier;
use crate::launcher::{DirectLauncher, JobLauncher, PreparedJob};
use crate::manager::{JobManager, JobManagerConfig, StartRequest};
use crate::model::JobOwner;
use harw_authority::{
    Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
};
use harw_tools::ToolExecutionContext;
use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Baut eine Sandbox über `<root>/project` mit den gegebenen Berechtigungen.
pub(crate) fn sandbox(root: &Path, permissions: Vec<Permission>) -> TestResult<SandboxSpec> {
    let project = root.join("project");
    std::fs::create_dir_all(&project).map_err(ctx("create project dir"))?;
    let registry = WorkspaceRegistry::build(
        root,
        [WorkspaceRegistration {
            tenant: TenantId::from_str("test-tenant"),
            workspace: WorkspaceId::from_str("project"),
            root: project,
        }],
    )
    .map_err(ctx("workspace registry"))?;
    let binding = registry
        .resolve(
            &TenantId::from_str("test-tenant"),
            &WorkspaceId::from_str("project"),
        )
        .map_err(ctx("resolve workspace"))?;
    Ok(SandboxSpec::from_resolved(
        binding,
        PermissionSet::from_policy(permissions),
    ))
}

/// Ausführungskontext für die Sitzung `session`.
pub(crate) fn context(session: &str, sandbox: SandboxSpec) -> ToolExecutionContext {
    ToolExecutionContext::new(SessionId::from_str(session), TurnId::new(), sandbox)
}

/// Schnelle Zeiten für Tests.
pub(crate) fn fast_config(state_dir: &Path) -> JobManagerConfig {
    JobManagerConfig {
        stop_grace: Duration::from_secs(1),
        poll_interval: Duration::from_millis(20),
        error_debounce: Duration::from_millis(50),
        error_min_interval: Duration::ZERO,
        min_notify_every: Duration::from_millis(50),
        ..JobManagerConfig::new(state_dir)
    }
}

/// Testumgebung: Arbeitsverzeichnis, Manager, aufzeichnender Notifier.
pub(crate) struct Env {
    pub(crate) dir: tempfile::TempDir,
    pub(crate) manager: Arc<JobManager>,
    pub(crate) recorder: Arc<RecordingNotifier>,
}

impl Env {
    pub(crate) fn new() -> TestResult<Self> {
        Self::with_config(|config| config)
    }

    pub(crate) fn with_config(
        adjust: impl FnOnce(JobManagerConfig) -> JobManagerConfig,
    ) -> TestResult<Self> {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let recorder = Arc::new(RecordingNotifier::default());
        let config = adjust(fast_config(&dir.path().join("state")));
        let manager = JobManager::new(config, Arc::clone(&recorder) as Arc<dyn crate::JobNotifier>)
            .map_err(ctx("job manager"))?;
        Ok(Self {
            dir,
            manager,
            recorder,
        })
    }

    pub(crate) fn state_dir(&self) -> PathBuf {
        self.dir.path().join("state")
    }

    /// Kontext mit `ExecuteProcess` für `session`.
    pub(crate) fn exec_context(&self, session: &str) -> TestResult<ToolExecutionContext> {
        let spec = sandbox(
            &self.dir.path().join("ws"),
            vec![Permission::ReadWorkspace, Permission::ExecuteProcess],
        )?;
        Ok(context(session, spec))
    }

    /// Bereitet `command` über den Test-Startweg vor.
    pub(crate) async fn prepare(&self, command: &str) -> TestResult<PreparedJob> {
        let ctx = self.exec_context("prep")?;
        DirectLauncher
            .prepare(&ctx, command, 60)
            .await
            .map_err(|output| TestError::Unexpected(format!("prepare refused: {output:?}")))
    }

    /// Bereitet `argv` (kein Shell-Zwischenschritt, stdin bleibt offen) für
    /// [`JobManager::start_piped`] vor.
    pub(crate) async fn prepare_piped(&self, argv: &[&str]) -> TestResult<PreparedJob> {
        let ctx = self.exec_context("prep")?;
        DirectLauncher::prepare_piped(&ctx, argv)
            .await
            .map_err(|output| TestError::Unexpected(format!("prepare refused: {output:?}")))
    }
}

/// Startanfrage mit Besitzer `session` und Vorfahren `ancestors`.
pub(crate) fn request(name: &str, session: &str, ancestors: &[&str]) -> StartRequest {
    StartRequest {
        name: name.to_owned(),
        command: name.to_owned(),
        cwd: None,
        env_keys: Vec::new(),
        notify_every: Duration::from_millis(100),
        owner: JobOwner::new(session, ancestors.iter().map(|a| (*a).to_owned()).collect()),
    }
}

/// Wartet höchstens `limit`, bis `check` wahr ist.
pub(crate) async fn eventually(limit: Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        if check() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
