//! Cloud listing through the real UIA operation-provider factory.
#[allow(dead_code, reason = "shared test support")]
mod common;

use common::{TestError, TestResult};
use harw_core::{InMemoryStateStore, StateStore};
use harw_runtime::{EntryKind, ModelSource, RuntimeAssembly, RuntimeSpec, RuntimeStores};
use harw_tools::{ToolCall, ToolExecutionContext, ToolName, ToolOutput};
use harw_types::{
    IngressSurface, PermissionTier, Principal, PrincipalKind, SessionId, ToolCallId, TurnId,
};
use std::sync::Arc;

async fn execute_cloud_operation(
    entry: EntryKind,
    principal: Principal,
    name: &str,
) -> TestResult<ToolOutput> {
    let scratch = tempfile::tempdir()?;
    let home = scratch.path().join("home");
    let project = scratch.path().join("project");
    std::fs::create_dir_all(&project)?;
    std::fs::write(project.join("Cargo.toml"), "[workspace]\n")?;
    let profile = home.join("profiles/selected");
    let agent = profile.join("agents/fixture-uia");
    std::fs::create_dir_all(&agent)?;
    std::fs::write(home.join("active_profile"), "selected\n")?;
    std::fs::write(
        agent.join("definition.toml"),
        "schema = \"harwness.agent/v1\"\nid = \"harwness.agent.fixture-uia@1\"\nversion = \"1.0.0\"\nrole = \"user-interface\"\nspecialization = \"terminal-ui\"\n",
    )?;
    std::fs::write(
        profile.join("config.toml"),
        "active_uia_definition = \"harwness.agent.fixture-uia@1\"\n",
    )?;
    let spec = RuntimeSpec {
        entry,
        home: home.clone(),
        cwd: project,
        principal,
        mode_override: None,
        active_agent: None,
        reasoning_effort: None,
        approval_override: None,
        model_override: None,
        embedded: None,
        child_backend: None,
    };
    let (events, _rx) = tokio::sync::mpsc::unbounded_channel();
    let state_store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());
    let assembly = RuntimeAssembly::builder(spec)
        .model(ModelSource::Echo("echo".into()))
        .stores(RuntimeStores {
            state_store,
            job_store: None,
            approval_store: None,
        })
        .session_events(events.clone())
        .build()?;
    let (turn_events, _turn_rx) = tokio::sync::mpsc::unbounded_channel();
    let root = assembly.new_root_session(
        assembly.root_session_id().clone(),
        events,
        turn_events,
        None,
    )?;
    let remote = profile.join("session-host/remote");
    std::fs::create_dir_all(&remote)?;
    std::fs::write(
        remote.join("node-devices.conf"),
        "node-a|device-a|acme|owner|active|Selected|approve\nnode-b|device-b|other|observer|active|Other\n",
    )?;
    let wrong = home.join("profiles/default/session-host/remote");
    std::fs::create_dir_all(&wrong)?;
    std::fs::write(
        wrong.join("node-devices.conf"),
        "wrong-node|wrong-device|acme|owner|active|Wrong\n",
    )?;
    let Some(executor) = harw_core::turn_loop::find_executor(&root.session, &ToolName::new(name))
    else {
        return Ok(ToolOutput::error("cloud operation is not exposed"));
    };
    let execution_context =
        ToolExecutionContext::new(SessionId::new(), TurnId::new(), assembly.sandbox().clone());
    let call = ToolCall {
        id: ToolCallId::new(),
        name: ToolName::new(name),
        arguments: serde_json::json!({}),
    };
    match executor.execute(&execution_context, &call).await {
        Ok(output) => Ok(output),
        Err(error) => Ok(ToolOutput::error(error.to_string())),
    }
}

fn principal(kind: PrincipalKind, surface: IngressSurface, tier: PermissionTier) -> Principal {
    Principal::trusted_ingress(kind, "cloud-test", surface, tier)
}

#[tokio::test]
async fn local_profile_read_uses_real_device_tenants_without_synthetic_mapping() -> TestResult {
    let output = execute_cloud_operation(
        EntryKind::Tui,
        principal(
            PrincipalKind::Human,
            IngressSurface::Tui,
            PermissionTier::Operator,
        ),
        "cloud.enrollments.list",
    )
    .await?;
    let ToolOutput::Text { content } = output else {
        return Err(TestError::Unexpected(format!(
            "expected listing: {output:?}"
        )));
    };
    assert!(content.contains("local_profile_operator"), "{content}");
    assert!(content.contains("device-a"), "{content}");
    assert!(content.contains("device-b"), "{content}");
    assert!(!content.contains("wrong-device"));
    Ok(())
}

#[tokio::test]
async fn remote_model_and_observer_entries_have_no_local_profile_capability() -> TestResult {
    for (entry, caller) in [
        (
            EntryKind::Analyze,
            principal(
                PrincipalKind::Human,
                IngressSurface::Tui,
                PermissionTier::Owner,
            ),
        ),
        (
            EntryKind::JobPrompt,
            principal(
                PrincipalKind::Human,
                IngressSurface::Tui,
                PermissionTier::Owner,
            ),
        ),
        (
            EntryKind::Tui,
            principal(
                PrincipalKind::Model,
                IngressSurface::Child,
                PermissionTier::Owner,
            ),
        ),
        (
            EntryKind::SessionHost,
            principal(
                PrincipalKind::Human,
                IngressSurface::Tui,
                PermissionTier::Owner,
            ),
        ),
        (
            EntryKind::Tui,
            principal(
                PrincipalKind::Human,
                IngressSurface::Gateway,
                PermissionTier::Owner,
            ),
        ),
        (
            EntryKind::Tui,
            principal(
                PrincipalKind::Model,
                IngressSurface::Tui,
                PermissionTier::Owner,
            ),
        ),
        (
            EntryKind::Tui,
            principal(
                PrincipalKind::Human,
                IngressSurface::Tui,
                PermissionTier::Observer,
            ),
        ),
    ] {
        let output = execute_cloud_operation(entry, caller, "cloud.enrollments.list").await?;
        assert!(matches!(output, ToolOutput::Error { .. }), "{output:?}");
    }
    Ok(())
}

#[tokio::test]
async fn local_profile_read_capability_does_not_authorize_service_mutations() -> TestResult {
    let output = execute_cloud_operation(
        EntryKind::Tui,
        principal(
            PrincipalKind::Human,
            IngressSurface::Tui,
            PermissionTier::Owner,
        ),
        "cloud.cloudctl.up",
    )
    .await?;
    let ToolOutput::Error { message } = output else {
        return Err(TestError::Unexpected(format!(
            "expected refusal: {output:?}"
        )));
    };
    assert!(
        message.contains("caller tenant") || message.contains("not exposed"),
        "{message}"
    );
    Ok(())
}
