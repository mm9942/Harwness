//! Peer bridge through a real assembly: `PeerBridgeContributor` registers
//! `peer.send` for a UIA root only, and only when `[peer_bridge]` is enabled.
//!
//! [`ModelSource::Echo`] as model: no network, no provider, no secret.

#[allow(
    dead_code,
    reason = "shared helper module; this test uses only part of it"
)]
mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{TestError, TestResult, ctx};
use harw_core::{InMemoryStateStore, StateStore};
use harw_operations::operation::Surface;
use harw_runtime::assembly::{RuntimeAssembly, RuntimeStores};
use harw_runtime::model::ModelSource;
use harw_runtime::peer_bridge_wiring::{PEER_SEND_TOOL, peer_bridge_root};
use harw_runtime::spec::{EntryKind, RuntimeSpec};
use harw_types::{IngressSurface, PermissionTier, Principal, PrincipalKind};
use tempfile::TempDir;

type SessionEventAlias = harw_protocol::events::SessionEvent;

const PEER_BRIDGE_CONFIG: &str = "\n[mcp_listener]\nenabled = false\n\n[[mcp_listener.principals]]\nid = \"claude\"\ncredential_ref = \"env:HARW_PEER_BRIDGE_TEST_TOKEN\"\ntenant = \"local\"\nworkspace = \"default\"\n\n[peer_bridge]\nenabled = true\n\n[[peer_bridge.peers]]\nid = \"claude\"\nprincipal = \"claude\"\nconnectors = [\"spool\", \"mcp\"]\n";

struct Fixture {
    _dir: TempDir,
    home: PathBuf,
    project: PathBuf,
}

/// Empty project plus a minimal UIA in the default profile.
fn fixture(peer_bridge: bool) -> TestResult<Fixture> {
    let dir = tempfile::tempdir()?;
    let home = dir.path().join("home");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&home).map_err(ctx("home"))?;
    std::fs::create_dir_all(&project).map_err(ctx("project"))?;
    std::fs::write(project.join("Cargo.toml"), "[workspace]\n").map_err(ctx("marker"))?;
    write_profile(&home, peer_bridge)?;
    Ok(Fixture {
        _dir: dir,
        home,
        project,
    })
}

fn write_profile(home: &Path, peer_bridge: bool) -> TestResult {
    let profile_dir = home.join("profiles").join("default");
    let agent_dir = profile_dir.join("agents").join("fixture-uia");
    std::fs::create_dir_all(&agent_dir).map_err(ctx("fixture uia dir"))?;
    std::fs::write(
        agent_dir.join("definition.toml"),
        "schema = \"harwness.agent/v1\"\nid = \"harwness.agent.fixture-uia@1\"\nversion = \"1.0.0\"\nrole = \"user-interface\"\nspecialization = \"terminal-ui\"\n",
    )
    .map_err(ctx("fixture uia definition"))?;
    let mut config = "active_uia_definition = \"harwness.agent.fixture-uia@1\"\n".to_owned();
    if peer_bridge {
        config.push_str(PEER_BRIDGE_CONFIG);
    }
    std::fs::write(profile_dir.join("config.toml"), config).map_err(ctx("fixture profile config"))
}

fn spec_for(entry: EntryKind, fixture: &Fixture) -> RuntimeSpec {
    RuntimeSpec {
        entry,
        home: fixture.home.clone(),
        cwd: fixture.project.clone(),
        principal: Principal::trusted_ingress(
            PrincipalKind::Human,
            "peer-bridge",
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

fn assemble(entry: EntryKind, fixture: &Fixture) -> TestResult<RuntimeAssembly> {
    let (events, _event_rx) = tokio::sync::mpsc::unbounded_channel::<SessionEventAlias>();
    let state_store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());
    RuntimeAssembly::builder(spec_for(entry, fixture))
        .model(ModelSource::Echo("echo: peer bridge".to_owned()))
        .stores(RuntimeStores {
            state_store,
            job_store: None,
            approval_store: None,
        })
        .session_events(events)
        .build()
        .map_err(TestError::Runtime)
}

#[test]
fn enabled_peer_bridge_registers_peer_send_for_the_uia_root_only() -> TestResult {
    let fixture = fixture(true)?;

    let uia = assemble(EntryKind::Tui, &fixture)?;
    assert!(
        uia.operations().find_by_name(PEER_SEND_TOOL).is_some(),
        "peer.send must be registered for a UIA root"
    );
    let root = uia.peer_bridge_root();
    assert!(root.is_some(), "UIA root must expose a peer bridge root");
    assert_eq!(root, peer_bridge_root(&fixture.home));

    let job = assemble(EntryKind::JobPrompt, &fixture)?;
    assert!(
        job.operations().find_by_name(PEER_SEND_TOOL).is_none(),
        "peer.send must not be registered for a job"
    );
    assert!(job.peer_bridge_root().is_none());
    Ok(())
}

#[test]
fn disabled_peer_bridge_registers_nothing() -> TestResult {
    let fixture = fixture(false)?;

    let uia = assemble(EntryKind::Tui, &fixture)?;
    assert!(
        uia.operations().find_by_name(PEER_SEND_TOOL).is_none(),
        "peer.send must not be registered without [peer_bridge]"
    );
    assert!(uia.peer_bridge_root().is_none());
    Ok(())
}

#[test]
fn peer_send_has_a_model_tool_surface() -> TestResult {
    let fixture = fixture(true)?;
    let uia = assemble(EntryKind::Tui, &fixture)?;
    let op = uia
        .operations()
        .find_by_name(PEER_SEND_TOOL)
        .ok_or_else(|| TestError::Missing("peer.send operation"))?;
    let meta = op.meta();
    assert!(
        meta.surfaces
            .iter()
            .any(|s| matches!(s, Surface::ModelTool { readonly: false, .. })),
        "peer.send must be a non-readonly model tool"
    );
    assert!(meta.summary.starts_with("Sends "));
    Ok(())
}
