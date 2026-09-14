//! Integration tests for the `#[operation]` proc-macro attribute.
//!
//! These tests exercise the macro's code-generation by compiling concrete
//! annotated functions and asserting on the resulting `Operation` impls.
//! No async runtime is required because the tests only inspect `meta()`.

#![allow(dead_code)]

use harw_macros::operation;
use harw_operations::{FromRawArgs, OpContext, OpError, OpOutput, Operation};
use serde::Deserialize;

// ── helpers ───────────────────────────────────────────────────────────────────

#[derive(Default, Deserialize)]
struct EmptyArgs {}

impl FromRawArgs for EmptyArgs {
    fn from_raw_args(_tokens: &[String]) -> Result<Self, OpError> {
        Ok(Self {})
    }
}

// ── basic compile + meta check ────────────────────────────────────────────────

#[operation(
    name = "test_noop",
    summary = "Test-Operation.",
    domain = "misc",
    permission = "observer",
    command(path = "/test_noop", visibility = "tui_only")
)]
async fn test_noop(_ctx: &OpContext, _args: EmptyArgs) -> Result<OpOutput, OpError> {
    Ok(OpOutput::from("ok".to_owned()))
}

#[test]
fn compiles_and_exposes_operation_impl() {
    let op = TestNoopOperation;
    assert_eq!(op.meta().name, "test_noop");
    assert!(!op.meta().surfaces.is_empty());
}

// ── domain and permission round-trip ─────────────────────────────────────────

#[derive(Default, Deserialize)]
struct NoArgs {}

impl FromRawArgs for NoArgs {
    fn from_raw_args(_tokens: &[String]) -> Result<Self, OpError> {
        Ok(Self {})
    }
}

#[operation(
    name = "session_status",
    summary = "Session-Status.",
    domain = "session",
    permission = "operator"
)]
async fn session_status(_ctx: &OpContext, _args: NoArgs) -> Result<OpOutput, OpError> {
    Ok(OpOutput::from(String::new()))
}

#[test]
fn domain_and_permission_correct() {
    use harw_operations::{OperationDomain, PermissionTier};

    let op = SessionStatusOperation;
    let meta = op.meta();
    assert_eq!(meta.name, "session_status");
    assert_eq!(meta.domain, OperationDomain::Session);
    assert_eq!(meta.permission, PermissionTier::Operator);
    assert!(
        meta.surfaces.is_empty(),
        "no surface declared → surfaces must be empty"
    );
}

// ── model_tool surface ────────────────────────────────────────────────────────

#[derive(Default, Deserialize)]
struct ReadArgs {}

impl FromRawArgs for ReadArgs {
    fn from_raw_args(_tokens: &[String]) -> Result<Self, OpError> {
        Ok(Self {})
    }
}

#[operation(
    name = "catalog_read",
    summary = "Katalog lesen.",
    domain = "catalog_config",
    permission = "maintainer",
    model_tool(readonly, approval = "none")
)]
async fn catalog_read(_ctx: &OpContext, _args: ReadArgs) -> Result<OpOutput, OpError> {
    Ok(OpOutput::from(String::new()))
}

#[test]
fn model_tool_surface_present() {
    use harw_operations::{ApprovalPolicy, Surface};

    let op = CatalogReadOperation;
    let meta = op.meta();
    assert_eq!(meta.surfaces.len(), 1);
    assert_eq!(
        meta.surfaces[0],
        Surface::ModelTool {
            readonly: true,
            approval: ApprovalPolicy::None
        },
    );
}

// ── both surfaces ─────────────────────────────────────────────────────────────

#[derive(Default, Deserialize)]
struct DualArgs {}

impl FromRawArgs for DualArgs {
    fn from_raw_args(_tokens: &[String]) -> Result<Self, OpError> {
        Ok(Self {})
    }
}

#[operation(
    name = "knowledge_search",
    summary = "Wissensbasis durchsuchen.",
    domain = "knowledge",
    permission = "owner",
    command(path = "/knowledge/search", visibility = "channel_parity"),
    model_tool(approval = "always")
)]
async fn knowledge_search(_ctx: &OpContext, _args: DualArgs) -> Result<OpOutput, OpError> {
    Ok(OpOutput::from(String::new()))
}

#[test]
fn both_surfaces_present() {
    use harw_operations::{ApprovalPolicy, CommandVisibility, Surface};

    let op = KnowledgeSearchOperation;
    let meta = op.meta();
    assert_eq!(meta.surfaces.len(), 2);
    assert!(meta.surfaces.contains(&Surface::Command {
        path: "/knowledge/search",
        visibility: CommandVisibility::ChannelParity,
    }));
    assert!(meta.surfaces.contains(&Surface::ModelTool {
        readonly: false,
        approval: ApprovalPolicy::Always,
    }));
}

// ── meta() returns the same pointer (OnceLock stability) ──────────────────────

#[test]
fn meta_returns_stable_pointer() {
    let op = TestNoopOperation;
    let p1 = op.meta() as *const _;
    let p2 = op.meta() as *const _;
    assert_eq!(
        p1, p2,
        "meta() must return the same OnceLock-backed pointer each time"
    );
}

// ── struct is Send + Sync ─────────────────────────────────────────────────────

#[test]
fn operation_struct_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<TestNoopOperation>();
    assert_send_sync::<KnowledgeSearchOperation>();
}

// ── agent_tool surface ────────────────────────────────────────────────────────

#[operation(
    name = "test_agent",
    summary = "Agent-Tool-Test.",
    domain = "agents",
    permission = "operator",
    agent_tool(child = "researcher", authority = "reduce_to_read_only", budget = "8k")
)]
async fn test_agent(_ctx: &OpContext, _args: EmptyArgs) -> Result<OpOutput, OpError> {
    Ok(OpOutput::from("ok".to_owned()))
}

#[test]
fn agent_tool_surface_is_registered() {
    use harw_operations::Surface;

    let op = TestAgentOperation;
    let has_agent_tool = op.meta().surfaces.iter().any(|s| {
        matches!(
            s,
            Surface::AgentTool {
                child_name: "researcher",
                ..
            }
        )
    });
    assert!(
        has_agent_tool,
        "AgentTool surface with child_name='researcher' must be present"
    );
}

#[test]
fn agent_tool_surface_fields_match() {
    use harw_operations::Surface;

    let op = TestAgentOperation;
    let meta = op.meta();
    assert_eq!(meta.surfaces.len(), 1);
    assert_eq!(
        meta.surfaces[0],
        Surface::AgentTool {
            child_name: "researcher",
            authority_reducer: "reduce_to_read_only",
            budget_hint: "8k",
        },
    );
}

// ── web surface (UI-05: `Surface::Web` finally reachable from `#[operation]`) ──

#[derive(Default, Deserialize)]
struct WebOnlyArgs {}

impl FromRawArgs for WebOnlyArgs {
    fn from_raw_args(_tokens: &[String]) -> Result<Self, OpError> {
        Ok(Self {})
    }
}

#[operation(
    name = "test_web_only",
    summary = "Nur eine Web-Fläche, kein Command, kein ModelTool.",
    domain = "misc",
    permission = "observer",
    web(path = "/api/test-web-only", method = "get", approval = "none")
)]
async fn test_web_only(_ctx: &OpContext, _args: WebOnlyArgs) -> Result<OpOutput, OpError> {
    Ok(OpOutput::from("ok".to_owned()))
}

#[test]
fn web_surface_method_and_fields_match() {
    use harw_operations::{ApprovalPolicy, Surface, WebMethod};

    let op = TestWebOnlyOperation;
    let meta = op.meta();
    assert_eq!(meta.surfaces.len(), 1);
    assert_eq!(
        meta.surfaces[0],
        Surface::Web {
            path: "/api/test-web-only",
            method: WebMethod::Get,
            approval: ApprovalPolicy::None,
        },
    );
}

#[derive(Default, Deserialize)]
struct WebWriteArgs {}

impl FromRawArgs for WebWriteArgs {
    fn from_raw_args(_tokens: &[String]) -> Result<Self, OpError> {
        Ok(Self {})
    }
}

#[operation(
    name = "test_web_write",
    summary = "Web-Fläche mit method = post, approval = always.",
    domain = "execution",
    permission = "operator",
    web(path = "/api/test-web-write", method = "post", approval = "always")
)]
async fn test_web_write(_ctx: &OpContext, _args: WebWriteArgs) -> Result<OpOutput, OpError> {
    Ok(OpOutput::from("ok".to_owned()))
}

#[test]
fn web_surface_with_explicit_post_method() {
    // F-031: `method` ist Pflicht und wird nicht mehr aus `readonly`
    // abgeleitet — dieser Test belegt den expliziten `method = "post"`-Fall
    // (vormals: kein `readonly`-Schlüssel → impliziter `false`-Default).
    use harw_operations::{ApprovalPolicy, Surface, WebMethod};

    let op = TestWebWriteOperation;
    assert_eq!(
        op.meta().surfaces[0],
        Surface::Web {
            path: "/api/test-web-write",
            method: WebMethod::Post,
            approval: ApprovalPolicy::Always,
        },
    );
}

// ── all three surfaces side by side (command + model_tool + web) ──────────────

#[derive(Default, Deserialize)]
struct TripleArgs {}

impl FromRawArgs for TripleArgs {
    fn from_raw_args(_tokens: &[String]) -> Result<Self, OpError> {
        Ok(Self {})
    }
}

#[operation(
    name = "test_triple_surface",
    summary = "Command, ModelTool und Web gleichzeitig — keine ersetzt die andere.",
    domain = "session",
    permission = "observer",
    command(path = "/triple", visibility = "channel_parity"),
    model_tool(readonly, approval = "none"),
    web(path = "/api/triple", method = "get", approval = "none")
)]
async fn test_triple_surface(_ctx: &OpContext, _args: TripleArgs) -> Result<OpOutput, OpError> {
    Ok(OpOutput::from("ok".to_owned()))
}

#[test]
fn command_model_tool_and_web_surfaces_all_coexist() {
    use harw_operations::{ApprovalPolicy, CommandVisibility, Surface, WebMethod};

    let op = TestTripleSurfaceOperation;
    let meta = op.meta();
    assert_eq!(meta.surfaces.len(), 3, "all three surfaces must be present");
    assert!(meta.surfaces.contains(&Surface::Command {
        path: "/triple",
        visibility: CommandVisibility::ChannelParity,
    }));
    assert!(meta.surfaces.contains(&Surface::ModelTool {
        readonly: true,
        approval: ApprovalPolicy::None,
    }));
    assert!(meta.surfaces.contains(&Surface::Web {
        path: "/api/triple",
        method: WebMethod::Get,
        approval: ApprovalPolicy::None,
    }));
}
