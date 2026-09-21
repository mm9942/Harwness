//! Integration tests for `harw-operations`.
//!
//! Exercises the public API (re-exports from `lib.rs`) end-to-end:
//! - Constructing concrete `Operation` implementations.
//! - Verifying `OperationMeta` fields exposed through the trait.
//! - Running `Operation::run` for all three `OpError` variants and the happy path.
//! - Confirming `OpError` Display / `std::error::Error` surface through the public path.
//! - Confirming that `Arc<dyn Operation>` can be shared across Tokio tasks.

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use harw_operations::{
    ApprovalPolicy, CommandVisibility, OpContext, OpError, OpFuture, OpInput, OpInvocation,
    OpOutput, Operation, OperationCategory, OperationDomain, OperationMeta, PermissionTier,
    ServiceMap, Surface, WebMethod,
};
use harw_operations::operation::BusyAvailability;
use harw_operations::adapter::WebAdapter;
use harw_operations::registry::OperationRegistry;
use harw_authority::{Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};

// ── Test fixtures (minimal concrete Operation impls) ─────────────────────────

struct EchoOp;

impl Operation for EchoOp {
    fn meta(&self) -> &OperationMeta {
        static META: OnceLock<OperationMeta> = OnceLock::new();
        META.get_or_init(|| OperationMeta {
            name: "echo",
            summary: "Gibt die Eingabe zurück.",
            domain: OperationDomain::Misc,
            permission: PermissionTier::Observer,
            surfaces: vec![
                Surface::Command {
                    path: "/echo",
                    visibility: CommandVisibility::ChannelParity,
                },
                Surface::ModelTool {
                    readonly: true,
                    approval: ApprovalPolicy::None,
                },
            ],
            aliases: &[],
            category: OperationCategory::Misc,
            args_schema: None,
            output_schema: None,
            busy: BusyAvailability::DeferredUntilTurnEnd,
        })
    }

    fn run<'a>(&'a self, _ctx: &'a OpContext, input: OpInput) -> OpFuture<'a> {
        Box::pin(async move {
            let joined = input.invocation.raw_args().join(" ");
            Ok(OpOutput::from(joined))
        })
    }
}

struct FailingOp {
    variant: FailVariant,
}

#[derive(Clone, Copy)]
enum FailVariant {
    InvalidArgs,
    Execution,
    NotAvailable,
}

impl Operation for FailingOp {
    fn meta(&self) -> &OperationMeta {
        static META: OnceLock<OperationMeta> = OnceLock::new();
        META.get_or_init(|| OperationMeta {
            name: "failing",
            summary: "Schlägt immer fehl.",
            domain: OperationDomain::Agents,
            permission: PermissionTier::Owner,
            surfaces: vec![],
            aliases: &[],
            category: OperationCategory::Misc,
            args_schema: None,
            output_schema: None,
            busy: BusyAvailability::DeferredUntilTurnEnd,
        })
    }

    fn run<'a>(&'a self, _ctx: &'a OpContext, _input: OpInput) -> OpFuture<'a> {
        let v = self.variant;
        Box::pin(async move {
            match v {
                FailVariant::InvalidArgs => {
                    Err(OpError::InvalidArguments("integration-test".to_owned()))
                }
                FailVariant::Execution => Err(OpError::Execution("integration-test".to_owned())),
                FailVariant::NotAvailable => {
                    Err(OpError::NotAvailable("integration-test".to_owned()))
                }
            }
        })
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn raw_input(args: &[&str]) -> OpInput {
    OpInput::command("/echo", args.iter().map(|s| (*s).to_owned()).collect())
}

fn empty_input() -> OpInput {
    OpInput::default()
}

/// Erstellt einen minimalen `OpContext` für Integrationstests.
/// Legt temporäre Verzeichnisse an, baut eine WorkspaceRegistry und
/// konstruiert einen SandboxSpec mit ReadWorkspace-Berechtigung.
fn make_test_ctx() -> (OpContext, PathBuf) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static CTX_COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = CTX_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!(
        "harw-ops-integration-{}-{}",
        std::process::id(),
        id
    ));
    std::fs::create_dir_all(tmp.join("ws")).unwrap();
    let registry = WorkspaceRegistry::build(
        &tmp,
        [WorkspaceRegistration {
            tenant: TenantId::from_str("test-tenant"),
            workspace: WorkspaceId::from_str("ws"),
            root: PathBuf::from("ws"),
        }],
    )
    .unwrap();
    let binding = registry
        .resolve(
            &TenantId::from_str("test-tenant"),
            &WorkspaceId::from_str("ws"),
        )
        .unwrap();
    let sandbox = SandboxSpec::from_resolved(
        binding,
        PermissionSet::from_policy([Permission::ReadWorkspace]),
    );
    let ctx = OpContext::new(SessionId::new(), TurnId::new(), sandbox, ServiceMap::new());
    (ctx, tmp)
}

// ── OperationMeta via public re-export ────────────────────────────────────────

#[test]
fn test_echo_op_meta_name() {
    let op = EchoOp;
    assert_eq!(op.meta().name, "echo");
}

#[test]
fn test_echo_op_meta_domain() {
    let op = EchoOp;
    assert_eq!(op.meta().domain, OperationDomain::Misc);
}

#[test]
fn test_echo_op_meta_permission() {
    let op = EchoOp;
    assert_eq!(op.meta().permission, PermissionTier::Observer);
}

#[test]
fn test_echo_op_meta_two_surfaces() {
    let op = EchoOp;
    assert_eq!(op.meta().surfaces.len(), 2);
}

#[test]
fn test_echo_op_meta_first_surface_is_command() {
    let op = EchoOp;
    match &op.meta().surfaces[0] {
        Surface::Command { path, visibility } => {
            assert_eq!(*path, "/echo");
            assert_eq!(*visibility, CommandVisibility::ChannelParity);
        }
        other => panic!("Erwartet Command, war: {other:?}"),
    }
}

#[test]
fn test_echo_op_meta_second_surface_is_model_tool() {
    let op = EchoOp;
    match &op.meta().surfaces[1] {
        Surface::ModelTool { readonly, approval } => {
            assert!(*readonly);
            assert_eq!(*approval, ApprovalPolicy::None);
        }
        other => panic!("Erwartet ModelTool, war: {other:?}"),
    }
}

// ── Operation::run — happy path ───────────────────────────────────────────────

#[tokio::test]
async fn test_echo_op_run_returns_joined_args() {
    let op = EchoOp;
    let (ctx, tmp) = make_test_ctx();
    let result = op.run(&ctx, raw_input(&["hello", "world"])).await;
    std::fs::remove_dir_all(tmp).ok();
    match result {
        Ok(out) => assert_eq!(out.text, "hello world"),
        Err(e) => panic!("Unerwarteter Fehler: {e}"),
    }
}

#[tokio::test]
async fn test_echo_op_run_with_empty_input_returns_empty_string() {
    let op = EchoOp;
    let (ctx, tmp) = make_test_ctx();
    let result = op.run(&ctx, empty_input()).await;
    std::fs::remove_dir_all(tmp).ok();
    match result {
        Ok(out) => assert!(out.text.is_empty()),
        Err(e) => panic!("Unerwarteter Fehler: {e}"),
    }
}

#[tokio::test]
async fn test_echo_op_run_with_single_arg() {
    let op = EchoOp;
    let (ctx, tmp) = make_test_ctx();
    let result = op.run(&ctx, raw_input(&["only"])).await;
    std::fs::remove_dir_all(tmp).ok();
    match result {
        Ok(out) => assert_eq!(out.text, "only"),
        Err(e) => panic!("Unerwarteter Fehler: {e}"),
    }
}

// ── Operation::run — all error variants ──────────────────────────────────────

#[tokio::test]
async fn test_failing_op_run_invalid_arguments() {
    let op = FailingOp {
        variant: FailVariant::InvalidArgs,
    };
    let (ctx, tmp) = make_test_ctx();
    let result = op.run(&ctx, empty_input()).await;
    std::fs::remove_dir_all(tmp).ok();
    match result {
        Err(OpError::InvalidArguments(msg)) => {
            assert_eq!(msg, "integration-test");
        }
        other => panic!("Erwartet InvalidArguments, war: {other:?}"),
    }
}

#[tokio::test]
async fn test_failing_op_run_execution_error() {
    let op = FailingOp {
        variant: FailVariant::Execution,
    };
    let (ctx, tmp) = make_test_ctx();
    let result = op.run(&ctx, empty_input()).await;
    std::fs::remove_dir_all(tmp).ok();
    match result {
        Err(OpError::Execution(msg)) => {
            assert_eq!(msg, "integration-test");
        }
        other => panic!("Erwartet Execution, war: {other:?}"),
    }
}

#[tokio::test]
async fn test_failing_op_run_not_available_error() {
    let op = FailingOp {
        variant: FailVariant::NotAvailable,
    };
    let (ctx, tmp) = make_test_ctx();
    let result = op.run(&ctx, empty_input()).await;
    std::fs::remove_dir_all(tmp).ok();
    match result {
        Err(OpError::NotAvailable(msg)) => {
            assert_eq!(msg, "integration-test");
        }
        other => panic!("Erwartet NotAvailable, war: {other:?}"),
    }
}

// ── OpError public surface ────────────────────────────────────────────────────

#[test]
fn test_op_error_display_via_public_reexport() {
    let err = OpError::Execution("public path".to_owned());
    let text = err.to_string();
    assert!(text.contains("Ausführungsfehler"));
    assert!(text.contains("public path"));
}

#[test]
fn test_op_error_is_std_error_via_public_reexport() {
    use std::error::Error;
    let err = OpError::NotAvailable("test".to_owned());
    let trait_obj: &dyn Error = &err;
    assert!(trait_obj.source().is_none());
    assert!(!trait_obj.to_string().is_empty());
}

#[test]
fn test_op_error_partial_eq_via_public_reexport() {
    let a = OpError::InvalidArguments("x".to_owned());
    let b = OpError::InvalidArguments("x".to_owned());
    assert_eq!(a, b);
}

// ── PermissionTier ordering via public reexport ───────────────────────────────

#[test]
fn test_permission_tier_ordering_public_reexport() {
    assert!(PermissionTier::Observer < PermissionTier::Owner);
    assert!(PermissionTier::Operator <= PermissionTier::Maintainer);
}

// ── Arc<dyn Operation> concurrency ────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_arc_dyn_operation_concurrent_execution() {
    let (ctx, tmp) = make_test_ctx();
    let ctx = Arc::new(ctx);
    let op: Arc<dyn Operation> = Arc::new(EchoOp);
    let mut handles = Vec::new();
    for i in 0..8_u32 {
        let op_ref = Arc::clone(&op);
        let ctx_ref = Arc::clone(&ctx);
        let arg = format!("task-{i}");
        handles.push(tokio::spawn(async move {
            op_ref.run(&ctx_ref, raw_input(&[&arg])).await
        }));
    }
    for (i, handle) in handles.into_iter().enumerate() {
        let join_result = handle.await;
        assert!(join_result.is_ok(), "task {i} panicked");
        let op_result = join_result.unwrap();
        assert!(op_result.is_ok(), "task {i} returned Err: {:?}", op_result);
        let out = op_result.unwrap();
        assert!(
            out.text.starts_with("task-"),
            "task {i} output malformed: {}",
            out.text
        );
    }
    std::fs::remove_dir_all(tmp).ok();
}

// ── OpInput / OpOutput via public reexport ────────────────────────────────────

#[test]
fn test_op_input_default_via_public_reexport() {
    let input = OpInput::default();
    assert!(input.invocation.raw_args().is_empty());
    assert_eq!(input.invocation.json_args(), &serde_json::Value::Null);
}

// ── OpInvocation via public reexport ──────────────────────────────────────────

#[test]
fn test_op_invocation_command_via_public_reexport() {
    let input = OpInput::command("/model", vec!["list".to_owned()]);
    assert!(input.invocation.is_command());
    assert_eq!(input.invocation.raw_args(), &["list".to_owned()]);
}

#[test]
fn test_op_invocation_model_tool_via_public_reexport() {
    let args = serde_json::json!({ "cmd": "status" });
    let input = OpInput::model_tool(args.clone());
    assert!(input.invocation.is_model_tool());
    assert_eq!(input.invocation.json_args(), &args);
}

#[test]
fn test_op_invocation_agent_tool_via_public_reexport() {
    let args = serde_json::json!({ "task": "review" });
    let input = OpInput::agent_tool("reviewer", args.clone());
    assert!(input.invocation.is_agent_tool());
    assert_eq!(input.invocation.json_args(), &args);
    match input.invocation {
        OpInvocation::AgentTool {
            child_name,
            args: got,
        } => {
            assert_eq!(child_name, "reviewer");
            assert_eq!(got, args);
        }
        other => panic!("Erwartet AgentTool, war: {other:?}"),
    }
}

#[test]
fn test_op_output_equality_via_public_reexport() {
    let a = OpOutput {
        text: "same".to_owned(),
        data: None,
    };
    let b = OpOutput {
        text: "same".to_owned(),
        data: None,
    };
    assert_eq!(a, b);
}

#[test]
fn test_op_output_inequality_via_public_reexport() {
    let a = OpOutput {
        text: "a".to_owned(),
        data: None,
    };
    let b = OpOutput {
        text: "b".to_owned(),
        data: None,
    };
    assert_ne!(a, b);
}

// ── OpContext public API ───────────────────────────────────────────────────────

#[test]
fn test_op_context_session_id_roundtrip() {
    // Build a dedicated temp dir so we can control the session_id and turn_id.
    let tmp = std::env::temp_dir().join(format!("harw-ops-ctx-roundtrip-{}", std::process::id()));
    std::fs::create_dir_all(tmp.join("ws")).unwrap();
    let registry = WorkspaceRegistry::build(
        &tmp,
        [WorkspaceRegistration {
            tenant: TenantId::from_str("t"),
            workspace: WorkspaceId::from_str("ws"),
            root: PathBuf::from("ws"),
        }],
    )
    .unwrap();
    let binding = registry
        .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("ws"))
        .unwrap();
    let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());
    let session = SessionId::from_str("test-session-123");
    let turn = TurnId::from_str("test-turn-456");
    let ctx = OpContext::new(session, turn, sandbox, ServiceMap::new());
    assert_eq!(ctx.session_id().as_str(), "test-session-123");
    assert_eq!(ctx.turn_id().as_str(), "test-turn-456");
    std::fs::remove_dir_all(tmp).ok();
}

#[test]
fn test_op_context_sandbox_accessible() {
    let (ctx, tmp) = make_test_ctx();
    let _sandbox = ctx.sandbox();
    // Sandbox accessible — does not panic.
    std::fs::remove_dir_all(tmp).ok();
}

// ── ServiceMap public API ─────────────────────────────────────────────────────

#[test]
fn test_service_map_insert_and_get() {
    struct Svc(u32);
    let mut map = ServiceMap::new();
    map.insert(Svc(99));
    assert_eq!(map.get::<Svc>().unwrap().0, 99);
}

#[test]
fn test_service_map_get_missing_returns_none() {
    let map = ServiceMap::new();
    assert!(map.get::<String>().is_none());
}

#[test]
fn test_service_map_overwrite() {
    let mut map = ServiceMap::new();
    map.insert(1_u32);
    map.insert(2_u32);
    assert_eq!(*map.get::<u32>().unwrap(), 2);
}

#[test]
fn test_op_context_service_lookup() {
    struct DbPool;
    let (ctx, tmp) = make_test_ctx();
    // No DbPool registered — must return None.
    assert!(ctx.service::<DbPool>().is_none());
    std::fs::remove_dir_all(tmp).ok();
}

// ── Surface::Web — fällt durch Registry und Adapter wie die anderen drei ───────
//
// UI-00 (harw-web): diese Tests belegen, dass eine Operation mit
// `Surface::Web` dieselben Wege durch `OperationRegistry` und ein
// dediziertes Adapter-Muster nimmt wie `Surface::Command`,
// `Surface::ModelTool` und `Surface::AgentTool` es bereits tun — additiv,
// kein bestehender Test wurde verändert.

struct WebOp;

impl Operation for WebOp {
    fn meta(&self) -> &OperationMeta {
        static META: OnceLock<OperationMeta> = OnceLock::new();
        META.get_or_init(|| OperationMeta {
            name: "web-op",
            summary: "Web-exponierte Testoperation.",
            domain: OperationDomain::Misc,
            permission: PermissionTier::Observer,
            surfaces: vec![Surface::Web {
                path: "/api/web-op",
                method: WebMethod::Get,
                approval: ApprovalPolicy::None,
            }],
            aliases: &[],
            category: OperationCategory::Misc,
            args_schema: None,
            output_schema: None,
            busy: BusyAvailability::DeferredUntilTurnEnd,
        })
    }

    fn run<'a>(&'a self, _ctx: &'a OpContext, input: OpInput) -> OpFuture<'a> {
        Box::pin(async move {
            let echoed = input
                .invocation
                .json_args()
                .get("q")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            Ok(OpOutput::from(echoed.to_owned()))
        })
    }
}

#[test]
fn test_web_op_meta_surface_is_web() {
    let op = WebOp;
    assert_eq!(op.meta().surfaces.len(), 1);
    match &op.meta().surfaces[0] {
        Surface::Web {
            path,
            method,
            approval,
        } => {
            assert_eq!(*path, "/api/web-op");
            assert_eq!(*method, WebMethod::Get);
            assert_eq!(*approval, ApprovalPolicy::None);
        }
        other => panic!("Erwartet Web, war: {other:?}"),
    }
}

#[test]
fn test_web_op_registers_in_operation_registry_and_is_found_by_name() {
    let mut registry = OperationRegistry::new();
    registry.register(Arc::new(WebOp));
    assert_eq!(registry.len(), 1);
    let found = registry.find_by_name("web-op");
    assert!(
        found.is_some(),
        "eine Operation mit Surface::Web muss über find_by_name auffindbar sein, \
         genau wie Operationen mit Surface::Command/ModelTool/AgentTool"
    );
}

#[test]
fn test_registry_by_surface_filters_web_surfaces() {
    let mut registry = OperationRegistry::new();
    registry.register(Arc::new(WebOp));
    registry.register(Arc::new(EchoOp));

    let web_ops = registry.by_surface(|s| matches!(s, Surface::Web { .. }));
    assert_eq!(
        web_ops.len(),
        1,
        "by_surface muss ausschließlich die Operation mit Surface::Web finden"
    );
    assert_eq!(web_ops[0].meta().name, "web-op");
}

#[test]
fn test_web_adapter_from_operation_registry_finds_route() {
    let mut registry = OperationRegistry::new();
    registry.register(Arc::new(WebOp));
    registry.register(Arc::new(EchoOp));

    // Dieselbe Bewegung, die harw-web beim Aufbau seiner Routentabelle macht:
    // über jede registrierte Operation iterieren und WebAdapter::from_operation
    // aufrufen. Nur Operationen mit Surface::Web liefern einen Adapter zurück —
    // kein Weg führt an einer Operation vorbei, die nicht in der Registry steht.
    let routes: Vec<WebAdapter> = registry
        .iter()
        .flat_map(|op| WebAdapter::from_operation(Arc::clone(op)))
        .collect();

    assert_eq!(
        routes.len(),
        1,
        "nur die Operation mit Surface::Web darf eine Route erzeugen"
    );
    assert_eq!(routes[0].path(), "/api/web-op");
    assert_eq!(routes[0].operation_name(), "web-op");
    assert_eq!(routes[0].permission(), PermissionTier::Observer);
}

#[test]
fn test_web_adapter_absent_for_operation_without_web_surface() {
    // EchoOp deklariert Command + ModelTool, aber keine Surface::Web —
    // WebAdapter::from_operation muss dafür einen leeren Vec liefern, sodass
    // kein Web-Pfad je eine nicht dafür registrierte Operation erreicht.
    let op: Arc<dyn Operation> = Arc::new(EchoOp);
    let adapters = WebAdapter::from_operation(op);
    assert!(adapters.is_empty());
}

#[tokio::test]
async fn test_web_adapter_invoke_runs_the_underlying_operation() {
    let op: Arc<dyn Operation> = Arc::new(WebOp);
    let adapters = WebAdapter::from_operation(op);
    let (ctx, tmp) = make_test_ctx();
    let result = adapters[0]
        .invoke(&ctx, serde_json::json!({ "q": "hallo-web" }))
        .await;
    std::fs::remove_dir_all(tmp).ok();
    match result {
        Ok(out) => assert_eq!(out.text, "hallo-web"),
        Err(e) => panic!("Unerwarteter Fehler: {e}"),
    }
}
