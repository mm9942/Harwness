//! Crypto-Infrastruktur H4/H5 end to end through a real assembly:
//! `[infrastructure]` in the profile config → `InfrastructureContributor`
//! registers the `infra.*` operations → the service reaches Slash and Web,
//! never the model-tool surface, and no `infra.*` name becomes a model tool.
//!
//! [`ModelSource::Echo`] as model — no network, no provider, no secret. The
//! configured sockets do not exist: assembling must not dial them.

#[allow(dead_code, reason = "shared helper module; this test uses only part of it")]
mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{TestError, TestResult, ctx};
use harw_core::{InMemoryStateStore, StateStore};
use harw_infra_client::InfrastructureAvailability;
use harw_runtime::assembly::{RuntimeAssembly, RuntimeStores};
use harw_runtime::model::ModelSource;
use harw_runtime::services::ServiceSurface;
use harw_runtime::spec::{EntryKind, RuntimeSpec};
use harw_types::{IngressSurface, PermissionTier, Principal, PrincipalKind};
use tempfile::TempDir;

type SessionEventAlias = harw_protocol::events::SessionEvent;

const INFRA_OPS: [&str; 4] = [
    "infra.status",
    "infra.health",
    "infra.auth.keys.describe",
    "infra.auth.keys.rotate",
];

struct Fixture {
    _dir: TempDir,
    home: PathBuf,
    project: PathBuf,
}

/// Empty project plus a minimal UIA in the default profile (same layout as
/// `rights_matrix.rs`), with `extra_config` appended to the profile config.
fn fixture(extra_config: &str) -> TestResult<Fixture> {
    let dir = tempfile::tempdir()?;
    let home = dir.path().join("home");
    let project = dir.path().join("project");
    std::fs::create_dir_all(&home).map_err(ctx("home"))?;
    std::fs::create_dir_all(&project).map_err(ctx("project"))?;
    std::fs::write(project.join("Cargo.toml"), "[workspace]\n").map_err(ctx("marker"))?;
    write_profile(&home, extra_config)?;
    Ok(Fixture {
        _dir: dir,
        home,
        project,
    })
}

fn write_profile(home: &Path, extra_config: &str) -> TestResult {
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
        format!("active_uia_definition = \"harwness.agent.fixture-uia@1\"\n{extra_config}"),
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
            "infrastructure",
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
        .model(ModelSource::Echo("echo: infrastructure".to_owned()))
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
fn without_infrastructure_section_nothing_is_registered() -> TestResult {
    let fixture = fixture("")?;
    let assembly = assemble(EntryKind::Tui, &fixture)?;
    for name in INFRA_OPS {
        assert!(
            assembly.operations().find_by_name(name).is_none(),
            "{name} must not exist without [infrastructure]"
        );
    }
    for surface in ServiceSurface::ALL {
        assert!(
            assembly
                .services()
                .service_map(surface)
                .get::<Arc<InfrastructureAvailability>>()
                .is_none(),
            "{}",
            surface.as_str()
        );
    }
    Ok(())
}

#[test]
fn configured_infrastructure_registers_ops_and_reaches_slash_and_web_only() -> TestResult {
    let fixture = fixture(
        "\n[infrastructure]\nauth_socket = \"/nonexistent/harw-infra-test/secure.sock\"\n",
    )?;
    let assembly = assemble(EntryKind::Tui, &fixture)?;

    for name in INFRA_OPS {
        assert!(
            assembly.operations().find_by_name(name).is_some(),
            "{name} must be registered with [infrastructure]"
        );
    }

    for surface in ServiceSurface::ALL {
        let map = assembly.services().service_map(surface);
        assert_eq!(
            map.get::<Arc<InfrastructureAvailability>>().is_some(),
            matches!(surface, ServiceSurface::Slash | ServiceSurface::Web),
            "{}",
            surface.as_str()
        );
    }

    // No `infra.*` operation is offered to the model as a tool.
    let snapshot = assembly.rights_snapshot();
    assert!(
        !snapshot.tools.iter().any(|tool| tool.contains("infra")),
        "{:?}",
        snapshot.tools
    );
    Ok(())
}

#[test]
fn invalid_infrastructure_section_is_not_fatal() -> TestResult {
    // Relative socket path: rejected by the client config, reported as a
    // warning. The operations stay registered and answer NotAvailable.
    let fixture = fixture("\n[infrastructure]\nnetwork_socket = \"run/network.sock\"\n")?;
    let assembly = assemble(EntryKind::Tui, &fixture)?;
    assert!(assembly.operations().find_by_name("infra.status").is_some());
    assert!(
        assembly
            .services()
            .service_map(ServiceSurface::Slash)
            .get::<Arc<InfrastructureAvailability>>()
            .is_none()
    );
    Ok(())
}
