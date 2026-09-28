//! R18 D-B end to end through a real assembly
//! (`docs/planning/65-cloud-sessions/contracts/R18-tool-gateway.md` §6):
//! `RuntimeAssemblyBuilder::gateway_port` → `GatewayContributor` registers
//! the port-backed `gateway.*` operations for a UIA root only, and the port
//! reaches the Slash, model-tool and Web service maps, never the Job map.
//! The diagnostics operations (`gateway.health`, `gateway.logs`,
//! `gateway.channels.*`) are registered for a UIA root without any port.
//!
//! [`ModelSource::Echo`] as model — no network, no provider, no secret. The
//! port never answers; assembling must not call it.

#[allow(
    dead_code,
    reason = "shared helper module; this test uses only part of it"
)]
mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{TestError, TestResult, ctx};
use harw_core::{InMemoryStateStore, StateStore};
use harw_ops::gateway_ops::{GATEWAY_DIAGNOSTICS_OPS, GATEWAY_MUTATION_OPS, GATEWAY_READ_OPS};
use harw_protocol::session_wire::{
    GatewayConnectionsResult, GatewayDrainParams, GatewayListenerInfo, GatewayListenerSetParams,
    GatewayListenersResult, GatewayRevokeParams, GatewayRevokeResult, GatewayStatus,
    GatewayToolRights, GatewayToolRightsParams, GatewayToolsResult, SessionSummary,
};
use harw_protocol::{GatewayPort, PortError, PortFuture};
use harw_runtime::assembly::{RuntimeAssembly, RuntimeStores};
use harw_runtime::model::ModelSource;
use harw_runtime::services::ServiceSurface;
use harw_runtime::spec::{EntryKind, RuntimeSpec};
use harw_types::{IngressSurface, PermissionTier, Principal, PrincipalKind};
use tempfile::TempDir;

type SessionEventAlias = harw_protocol::events::SessionEvent;

/// A gateway port that counts calls and never answers successfully.
#[derive(Default)]
struct CountingPort {
    calls: AtomicUsize,
}

impl CountingPort {
    fn refuse<'a, T: Send + 'a>(&self) -> PortFuture<'a, T> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(Err(PortError::Transport(
            "test port".to_owned(),
        ))))
    }
}

impl GatewayPort for CountingPort {
    fn status(&self) -> PortFuture<'_, GatewayStatus> {
        self.refuse()
    }

    fn connections(&self) -> PortFuture<'_, GatewayConnectionsResult> {
        self.refuse()
    }

    fn sessions(&self) -> PortFuture<'_, Vec<SessionSummary>> {
        self.refuse()
    }

    fn listeners(&self) -> PortFuture<'_, GatewayListenersResult> {
        self.refuse()
    }

    fn tools(&self) -> PortFuture<'_, GatewayToolsResult> {
        self.refuse()
    }

    fn revoke_connection(
        &self,
        _params: GatewayRevokeParams,
    ) -> PortFuture<'_, GatewayRevokeResult> {
        self.refuse()
    }

    fn drain(&self, _params: GatewayDrainParams) -> PortFuture<'_, GatewayStatus> {
        self.refuse()
    }

    fn set_listener(
        &self,
        _params: GatewayListenerSetParams,
    ) -> PortFuture<'_, GatewayListenerInfo> {
        self.refuse()
    }

    fn grant_tools(&self, _params: GatewayToolRightsParams) -> PortFuture<'_, GatewayToolRights> {
        self.refuse()
    }

    fn narrow_tools(&self, _params: GatewayToolRightsParams) -> PortFuture<'_, GatewayToolRights> {
        self.refuse()
    }
}

struct Fixture {
    _dir: TempDir,
    home: PathBuf,
    project: PathBuf,
}

/// Empty project plus a minimal UIA in the default profile (same layout as
/// `rights_matrix.rs`/`infrastructure.rs`).
fn fixture() -> TestResult<Fixture> {
    let dir = tempfile::tempdir()?;
    let home = dir.path().join("home");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&home).map_err(ctx("home"))?;
    std::fs::create_dir_all(&project).map_err(ctx("project"))?;
    std::fs::write(project.join("Cargo.toml"), "[workspace]\n").map_err(ctx("marker"))?;
    write_profile(&home)?;
    Ok(Fixture {
        _dir: dir,
        home,
        project,
    })
}

fn write_profile(home: &Path) -> TestResult {
    let profile_dir = home.join("profiles").join("default");
    let agent_dir = profile_dir.join("agents").join("fixture-uia");
    std::fs::create_dir_all(&agent_dir).map_err(ctx("fixture uia dir"))?;
    std::fs::write(
        agent_dir.join("definition.toml"),
        "schema = \"harwness.agent/v1\"\nid = \"harwness.agent.fixture-uia@1\"\nversion = \"1.0.0\"\nrole = \"user-interface\"\nspecialization = \"terminal-ui\"\n",
    )
    .map_err(ctx("fixture uia definition"))?;
    std::fs::write(
        profile_dir.join("config.toml"),
        "active_uia_definition = \"harwness.agent.fixture-uia@1\"\n",
    )
    .map_err(ctx("fixture profile config"))
}

fn spec_for(entry: EntryKind, fixture: &Fixture) -> RuntimeSpec {
    RuntimeSpec {
        entry,
        home: fixture.home.clone(),
        cwd: fixture.project.clone(),
        principal: Principal::trusted_ingress(
            PrincipalKind::Human,
            "gateway-port",
            IngressSurface::Tui,
            PermissionTier::Owner,
        ),
        mode_override: None,
        active_agent: None,
        reasoning_effort: None,
        approval_override: None,
        model_override: None,
        embedded: None,
        child_backend: None,
    }
}

fn assemble(
    entry: EntryKind,
    fixture: &Fixture,
    port: Option<Arc<dyn GatewayPort>>,
) -> TestResult<RuntimeAssembly> {
    let (events, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEventAlias>();
    let state_store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());
    let builder = RuntimeAssembly::builder(spec_for(entry, fixture))
        .model(ModelSource::Echo("echo: gateway port".to_owned()))
        .stores(RuntimeStores {
            state_store,
            job_store: None,
            approval_store: None,
        })
        .session_events(events);
    let builder = match port {
        Some(port) => builder.gateway_port(port),
        None => builder,
    };
    builder.build().map_err(TestError::Runtime)
}

fn port_backed_ops() -> impl Iterator<Item = &'static str> {
    GATEWAY_READ_OPS.into_iter().chain(GATEWAY_MUTATION_OPS)
}

#[test]
fn with_a_port_the_port_backed_ops_register_for_a_uia_root_and_not_for_a_job() -> TestResult {
    let fixture = fixture()?;
    let port = Arc::new(CountingPort::default());
    let dyn_port: Arc<dyn GatewayPort> = Arc::clone(&port) as Arc<dyn GatewayPort>;

    // UIA root (TUI): every port-backed op and every diagnostics op.
    let uia = assemble(EntryKind::Tui, &fixture, Some(Arc::clone(&dyn_port)))?;
    for name in port_backed_ops().chain(GATEWAY_DIAGNOSTICS_OPS) {
        assert!(
            uia.operations().find_by_name(name).is_some(),
            "{name} must be registered for a UIA root with a gateway port"
        );
    }
    for surface in ServiceSurface::ALL {
        let map = uia.services().service_map(surface);
        let present = map.get::<Arc<dyn GatewayPort>>();
        assert_eq!(
            present.is_some(),
            surface != ServiceSurface::Job,
            "{}",
            surface.as_str()
        );
        if let Some(present) = present {
            assert!(Arc::ptr_eq(present, &dyn_port), "{}", surface.as_str());
        }
    }

    // Job root: no `gateway.*` op at all, and never the port on the Job map.
    let job = assemble(EntryKind::JobPrompt, &fixture, Some(Arc::clone(&dyn_port)))?;
    for name in port_backed_ops().chain(GATEWAY_DIAGNOSTICS_OPS) {
        assert!(
            job.operations().find_by_name(name).is_none(),
            "{name} must not be registered for a job root"
        );
    }
    assert!(
        job.services()
            .service_map(ServiceSurface::Job)
            .get::<Arc<dyn GatewayPort>>()
            .is_none()
    );

    // Assembling never talks to the gateway.
    assert_eq!(port.calls.load(Ordering::SeqCst), 0);
    Ok(())
}

#[test]
fn without_a_port_only_the_diagnostics_register_for_a_uia_root() -> TestResult {
    let fixture = fixture()?;
    let uia = assemble(EntryKind::Tui, &fixture, None)?;
    for name in port_backed_ops() {
        assert!(
            uia.operations().find_by_name(name).is_none(),
            "{name} must not exist without a gateway port"
        );
    }
    // F1: the channel reads and the local diagnostics need no port.
    for name in GATEWAY_DIAGNOSTICS_OPS {
        assert!(
            uia.operations().find_by_name(name).is_some(),
            "{name} must be registered for a UIA root without a gateway port"
        );
    }
    for surface in ServiceSurface::ALL {
        assert!(
            uia.services()
                .service_map(surface)
                .get::<Arc<dyn GatewayPort>>()
                .is_none(),
            "{}",
            surface.as_str()
        );
    }
    Ok(())
}
