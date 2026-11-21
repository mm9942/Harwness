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
        install_host_port();
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

/// Golden-Vergleich der Oberfläche eines `ToolProvider`: Namen, Spezifikations-
/// JSON, Parallelitäts-Zusagen, Executor-Auflösung und (falls der Provider sie
/// trägt) die deklarierten Berechtigungen.
///
/// Die Golden-Dateien unter `tests/golden/` wurden **vor** der Migration auf
/// `tool_provider!` vom handgeschriebenen Provider erfasst; der Test beweist,
/// dass die Migration die Oberfläche nicht verändert. Absichtliche Änderungen
/// werden mit `HARW_UPDATE_GOLDEN=1 cargo test -p <crate>` neu erfasst.
pub(crate) mod golden {
    use super::{TestError, TestResult, ctx};
    use harw_extension_api::contributors::ToolProvider;
    use harw_tools::{Permission, ToolName};
    use serde_json::{Map, Value, json};
    use std::path::PathBuf;

    /// Baut die zu vergleichende Oberfläche des Providers.
    pub(crate) fn surface(
        provider: &dyn ToolProvider,
        permissions: Option<&[Option<Permission>]>,
    ) -> TestResult<Value> {
        let specs = provider.tools();
        let mut parallel = Map::new();
        let mut executors = Map::new();
        for spec in &specs {
            let name = ToolName::new(spec.name());
            parallel.insert(
                spec.name().to_owned(),
                Value::Bool(provider.parallel_safe(&name)),
            );
            executors.insert(
                spec.name().to_owned(),
                Value::Bool(provider.executor(&name).is_some()),
            );
        }
        let unknown = ToolName::new("golden.unknown");
        Ok(json!({
            "tools": serde_json::to_value(&specs).map_err(ctx("specs serialise"))?,
            "parallel_safe": parallel,
            "has_executor": executors,
            "unknown_has_executor": provider.executor(&unknown).is_some(),
            "unknown_parallel_safe": provider.parallel_safe(&unknown),
            "permissions": permissions.map(|declared| {
                declared.iter().map(|permission| format!("{permission:?}")).collect::<Vec<_>>()
            }),
        }))
    }

    /// Vergleicht `actual` mit `tests/golden/<file>`; mit `HARW_UPDATE_GOLDEN`
    /// wird die Datei stattdessen geschrieben.
    pub(crate) fn assert_golden(file: &str, actual: &Value) -> TestResult {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("golden")
            .join(file);
        if std::env::var_os("HARW_UPDATE_GOLDEN").is_some() {
            let parent = path
                .parent()
                .ok_or(TestError::Missing("golden file has a parent directory"))?;
            std::fs::create_dir_all(parent).map_err(ctx("golden directory is creatable"))?;
            let mut text =
                serde_json::to_string_pretty(actual).map_err(ctx("golden serialises"))?;
            text.push('\n');
            std::fs::write(&path, text).map_err(ctx("golden file is writable"))?;
            return Ok(());
        }
        let text = std::fs::read_to_string(&path).map_err(ctx("golden file is readable"))?;
        let expected: Value = serde_json::from_str(&text).map_err(ctx("golden file parses"))?;
        if &expected != actual {
            return Err(TestError::Unexpected(format!(
                "provider surface differs from golden {file}\nexpected: {expected:#}\nactual: {actual:#}"
            )));
        }
        Ok(())
    }
}

/// Installs the host command port once per test binary (jobs start only
/// through the job runtime).
pub(crate) fn install_host_port() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = harw_test_support::unique_tmp("harw-tool-job", "command-jobs");
        if let Ok(port) = harw_command::JobCommandPort::host(&dir) {
            let _ = harw_command::install(std::sync::Arc::new(port));
        }
    });
}

/// Fake cgroup backend for `job.suspend` / `job.resume` tests (PL-100
/// WP 3.5): records every freeze and thaw, keeps the requested
/// (`cgroup.freeze`) and the kernel (`cgroup.events frozen`) state per
/// cgroup, can deny both operations, and lets a test change the state
/// "outside harw" (`FakeCgroup::set_outside`). [`fake_cgroup::FakeHolds`]
/// is the Warden's containment index for these tests;
/// [`fake_cgroup::marker_index`] builds a real marker directory
/// (`MarkerDirHolds`) in a temporary directory.
#[cfg(target_os = "linux")]
pub(crate) mod fake_cgroup {
    use super::{TestResult, ctx};
    use crate::freeze::{
        CgroupFreezer, ContainmentHolds, ContainmentLease, FreezeError, JobFreezer, MarkerDirHolds,
    };
    use harw_job_linux::cgroup::{CgroupBackend, CgroupHandle, CgroupSpec, CgroupStats};
    use harw_job_linux::{CgroupError, LinuxProcess};
    use std::collections::BTreeSet;
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

    /// The effective uid of the test process (the owner of everything a
    /// test creates).
    pub(crate) fn own_uid() -> u32 {
        rustix::process::geteuid().as_raw()
    }

    /// Sets `mode` on `path` (tests must not depend on the umask).
    pub(crate) fn chmod(path: &Path, mode: u32) -> TestResult {
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(ctx("chmod"))
    }

    /// A Warden runtime directory `<base>/harw-warden` (`0755`) with an
    /// empty marker directory `holds` (`0755`), read by a
    /// [`MarkerDirHolds`] that accepts the test user as owner of both and
    /// knows no evidence of an installed Warden.
    pub(crate) fn marker_index(base: &Path) -> TestResult<MarkerDirHolds> {
        let runtime = base.join("harw-warden");
        let dir = runtime.join("holds");
        fs::create_dir_all(&dir).map_err(ctx("create marker dir"))?;
        chmod(&runtime, 0o755)?;
        chmod(&dir, 0o755)?;
        Ok(trusting_test_user(MarkerDirHolds::new(dir)))
    }

    /// `holds` accepting the test user as owner of the runtime directory
    /// and of the index, without evidence of an installed Warden (the
    /// host's real `/run/harw/warden.sock` must not matter).
    pub(crate) fn trusting_test_user(holds: MarkerDirHolds) -> MarkerDirHolds {
        holds
            .trusting_runtime_owner(own_uid())
            .allowing_own_index()
            .with_owner_evidence(Vec::<PathBuf>::new())
    }

    /// Writes the marker of `kernel_id` as a regular `0644` file.
    pub(crate) fn write_marker(holds: &MarkerDirHolds, kernel_id: u64) -> TestResult<PathBuf> {
        let marker = holds.dir().join(kernel_id.to_string());
        fs::write(&marker, b"").map_err(ctx("write marker"))?;
        chmod(&marker, 0o644)?;
        Ok(marker)
    }

    /// The kernel id the fake reports for the cgroup `name`: a stable
    /// FNV-1a hash of the name (wrapping on purpose, as every hash does).
    pub(crate) fn fake_kernel_id(name: &str) -> u64 {
        name.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        })
    }

    /// In-memory containment index of the Warden: the kernel ids it holds,
    /// a switch that makes the index unreadable, and switches for its lease
    /// (refused as untrusted, or contended: someone else holds the lock).
    #[derive(Debug, Default)]
    pub(crate) struct FakeHolds {
        held: Mutex<BTreeSet<u64>>,
        unreadable: AtomicBool,
        lease_unavailable: AtomicBool,
        lease_contended: AtomicBool,
    }

    impl FakeHolds {
        /// The Warden writes its marker for `kernel_id` (before it freezes).
        pub(crate) fn hold(&self, kernel_id: u64) {
            lock(&self.held).insert(kernel_id);
        }

        /// The Warden removes its marker for `kernel_id` (after it thawed).
        pub(crate) fn release(&self, kernel_id: u64) {
            lock(&self.held).remove(&kernel_id);
        }

        /// Makes every lookup fail (`true`) or work again (`false`).
        pub(crate) fn set_unreadable(&self, unreadable: bool) {
            self.unreadable.store(unreadable, Ordering::SeqCst);
        }

        /// Makes every lease fail as an untrusted index (`true`) or work
        /// again (`false`); lookups are not affected.
        pub(crate) fn set_lease_unavailable(&self, unavailable: bool) {
            self.lease_unavailable.store(unavailable, Ordering::SeqCst);
        }

        /// Makes every lease contended (`true`: someone else holds the
        /// index lock for the whole retry budget) or unlocked again
        /// (`false`).
        pub(crate) fn set_lease_contended(&self, contended: bool) {
            self.lease_contended.store(contended, Ordering::SeqCst);
        }
    }

    impl ContainmentHolds for FakeHolds {
        fn is_held(&self, kernel_id: u64) -> Result<bool, FreezeError> {
            if self.unreadable.load(Ordering::SeqCst) {
                return Err(FreezeError::ContainmentIndexUnreadable {
                    reason: "the fake index is unreadable",
                });
            }
            Ok(lock(&self.held).contains(&kernel_id))
        }

        fn lease(&self) -> Result<ContainmentLease, FreezeError> {
            if self.lease_unavailable.load(Ordering::SeqCst) {
                return Err(FreezeError::ContainmentIndexUnreadable {
                    reason: "the fake lease is unavailable",
                });
            }
            if self.lease_contended.load(Ordering::SeqCst) {
                return Ok(ContainmentLease::contended());
            }
            Ok(ContainmentLease::unlocked())
        }
    }

    /// One recorded call: `(operation, cgroup name)`.
    pub(crate) type Call = (&'static str, String);

    /// The fake backend.
    #[derive(Debug, Default)]
    pub(crate) struct FakeCgroup {
        calls: Mutex<Vec<Call>>,
        // Cgroups whose own `cgroup.freeze` is 1.
        requested: Mutex<BTreeSet<String>>,
        // Cgroups the kernel reports frozen (own or inherited freeze).
        frozen: Mutex<BTreeSet<String>>,
        deny: bool,
    }

    // Poison-tolerant lock (a failed test must not hide the next failure).
    fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(PoisonError::into_inner)
    }

    // Error for every operation the job tool never uses.
    fn unused(name: &str) -> CgroupError {
        CgroupError::NotFound {
            path: name.to_owned(),
        }
    }

    impl FakeCgroup {
        /// A backend whose `freeze` and `thaw` fail with `PermissionDenied`.
        pub(crate) fn denying() -> Self {
            Self {
                deny: true,
                ..Self::default()
            }
        }

        /// All recorded freeze/thaw calls in order.
        pub(crate) fn calls(&self) -> Vec<Call> {
            lock(&self.calls).clone()
        }

        /// Changes the state of `name` as someone other than harw would
        /// (an operator, the Warden, an ancestor freeze): `requested` is its
        /// own `cgroup.freeze`, `frozen` the kernel view. Not recorded as a
        /// call.
        pub(crate) fn set_outside(&self, name: &str, requested: bool, frozen: bool) {
            for (set, member) in [(&self.requested, requested), (&self.frozen, frozen)] {
                let mut set = lock(set);
                if member {
                    set.insert(name.to_owned());
                } else {
                    set.remove(name);
                }
            }
        }

        // Records `operation` on `group` and applies it to both sets (the
        // fake freezes at once).
        fn apply(&self, operation: &'static str, group: &CgroupHandle) -> Result<(), CgroupError> {
            lock(&self.calls).push((operation, group.name().to_owned()));
            if self.deny {
                return Err(CgroupError::PermissionDenied {
                    path: group.name().to_owned(),
                });
            }
            let freeze = operation == "freeze";
            self.set_outside(group.name(), freeze, freeze);
            Ok(())
        }
    }

    impl CgroupBackend for FakeCgroup {
        fn create(&self, spec: &CgroupSpec) -> Result<CgroupHandle, CgroupError> {
            Err(unused(&spec.name))
        }

        fn reopen(&self, name: &str) -> Result<CgroupHandle, CgroupError> {
            Ok(CgroupHandle::new(
                name.to_owned(),
                format!("/fake/{name}"),
                None,
            ))
        }

        fn attach(&self, group: &CgroupHandle, _process: &LinuxProcess) -> Result<(), CgroupError> {
            Err(unused(group.name()))
        }

        fn attach_self(&self, group: &CgroupHandle) -> Result<(), CgroupError> {
            Err(unused(group.name()))
        }

        fn freeze(&self, group: &CgroupHandle) -> Result<(), CgroupError> {
            self.apply("freeze", group)
        }

        fn thaw(&self, group: &CgroupHandle) -> Result<(), CgroupError> {
            self.apply("thaw", group)
        }

        fn is_frozen(&self, group: &CgroupHandle) -> Result<bool, CgroupError> {
            Ok(lock(&self.frozen).contains(group.name()))
        }

        fn freeze_requested(&self, group: &CgroupHandle) -> Result<bool, CgroupError> {
            Ok(lock(&self.requested).contains(group.name()))
        }

        fn kernel_id(&self, group: &CgroupHandle) -> Result<u64, CgroupError> {
            Ok(fake_kernel_id(group.name()))
        }

        fn kill(&self, group: &CgroupHandle) -> Result<(), CgroupError> {
            Err(unused(group.name()))
        }

        fn stats(&self, group: &CgroupHandle) -> Result<CgroupStats, CgroupError> {
            Err(unused(group.name()))
        }

        fn remove(&self, group: CgroupHandle) -> Result<(), CgroupError> {
            Err(unused(group.name()))
        }
    }

    /// The delegated root the fake freezer is rooted at.
    pub(crate) const FAKE_ROOT: &str = "/harw.slice";

    /// A [`CgroupFreezer`] over `fake` rooted at [`FAKE_ROOT`] whose
    /// containment index holds nothing (never the host's real index).
    pub(crate) fn freezer(fake: &Arc<FakeCgroup>) -> Arc<dyn JobFreezer> {
        freezer_with_holds(fake, &Arc::new(FakeHolds::default()))
    }

    /// A [`CgroupFreezer`] over `fake` rooted at [`FAKE_ROOT`] that reads
    /// the containment index `holds`.
    pub(crate) fn freezer_with_holds(
        fake: &Arc<FakeCgroup>,
        holds: &Arc<FakeHolds>,
    ) -> Arc<dyn JobFreezer> {
        freezer_over(fake, Arc::clone(holds) as Arc<dyn ContainmentHolds>)
    }

    /// A [`CgroupFreezer`] over `fake` rooted at [`FAKE_ROOT`] that reads
    /// (and locks) the containment index `holds`, e.g. a [`MarkerDirHolds`]
    /// from [`marker_index`].
    pub(crate) fn freezer_over(
        fake: &Arc<FakeCgroup>,
        holds: Arc<dyn ContainmentHolds>,
    ) -> Arc<dyn JobFreezer> {
        Arc::new(CgroupFreezer::with_holds(
            Arc::clone(fake) as Arc<dyn CgroupBackend>,
            FAKE_ROOT,
            holds,
        ))
    }
}

/// Builds the `meta.json` of a stub job for suspend tests (PL-100 WP 3.5):
/// `state`, owner `session` with `ancestors`, no process (`pid` unset), and
/// — with `attempt` — a runtime-proven own cgroup
/// `<FAKE_ROOT>/harw-job-<attempt>`.
#[cfg(target_os = "linux")]
pub(crate) fn stub_job_meta(
    id: &str,
    state: crate::model::JobState,
    session: &str,
    ancestors: &[&str],
    attempt: Option<&str>,
) -> TestResult<crate::model::JobMeta> {
    use crate::model::{DedicatedCgroupProof, JobId, JobMeta, JobProcessIdentity};
    let job_id = JobId::parse(id).ok_or(TestError::Missing("valid job id"))?;
    let identity = attempt.map(|attempt| JobProcessIdentity {
        start_ticks: 1,
        executable: None,
        cgroup_path: Some(format!("{}/harw-job-{attempt}", fake_cgroup::FAKE_ROOT)),
        dedicated_cgroup: Some(DedicatedCgroupProof {
            job_id: Some(job_id.clone()),
            attempt_id: attempt.to_owned(),
            delegated_root: fake_cgroup::FAKE_ROOT.to_owned(),
        }),
    });
    let now = jiff::Timestamp::now();
    Ok(JobMeta {
        version: crate::model::META_VERSION,
        job_id,
        name: id.to_owned(),
        command: "sleep 1000".to_owned(),
        cwd: None,
        env_keys: Vec::new(),
        state,
        pid: None,
        proc_start_ticks: None,
        identity,
        executed_on_host: true,
        harw_instance: "harw-test".to_owned(),
        owner: JobOwner::new(
            session,
            ancestors
                .iter()
                .map(|ancestor| (*ancestor).to_owned())
                .collect(),
        ),
        created_at: now,
        started_at: Some(now),
        ended_at: None,
        exit_code: None,
        signal: None,
        stop_requested: false,
        detached: false,
        notify_every_secs: 0,
        progress: None,
        warnings: 0,
        errors: 0,
        launch_error: None,
        stragglers_reaped: false,
        log_truncated: false,
        launch_warnings: Vec::new(),
        origin_call_id: None,
        origin_tool: None,
        owner_agent: None,
        origin: None,
        suspended_at: None,
        suspended_secs: 0,
        suspend_phase: None,
    })
}
