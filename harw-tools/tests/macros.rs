//! Integration test exercising the `#[derive(Tool)]` and `#[tool]` macros
//! against the real `harw_tools` types.

mod common;

use common::{TestError, TestResult, ctx};
use harw_authority::{
    Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
};
use harw_macros::{Tool, tool};
use harw_tools::{
    JsonSchemaType, ToolCall, ToolExecutionContext, ToolExecutor, ToolName, ToolOutput, ToolSpec,
    ToolsError,
};
use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Tool, Deserialize)]
#[tool(name = "search", description = "Search docs")]
struct SearchArgs {
    /// The query string
    #[allow(dead_code)]
    query: String,
    /// Max results
    #[tool(default = 10)]
    #[serde(default)]
    #[allow(dead_code)]
    top_k: u8,
    /// Optional tags
    #[allow(dead_code)]
    tags: Option<Vec<String>>,
}

#[test]
fn derive_tool_builds_spec() -> TestResult {
    let ToolSpec::Function(spec) = SearchArgs::tool_spec();
    assert_eq!(spec.name.as_str(), "search");
    assert_eq!(spec.description, "Search docs");
    assert!(!spec.strict);

    let params = &spec.parameters;
    assert_eq!(params.schema_type, Some(JsonSchemaType::Object));

    let props = params
        .properties
        .as_ref()
        .ok_or(TestError::Missing("properties"))?;
    assert_eq!(
        props["query"].schema_type,
        Some(JsonSchemaType::String),
        "String maps to string"
    );
    assert_eq!(
        props["query"].description.as_deref(),
        Some("The query string")
    );
    assert_eq!(props["top_k"].schema_type, Some(JsonSchemaType::Integer));
    assert_eq!(props["tags"].schema_type, Some(JsonSchemaType::Array));
    assert_eq!(
        props["tags"]
            .items
            .as_ref()
            .ok_or(TestError::Missing("tags.items"))?
            .schema_type,
        Some(JsonSchemaType::String)
    );

    // `query` is required; `top_k` (default) and `tags` (Option) are not.
    let required = params
        .required
        .as_ref()
        .ok_or(TestError::Missing("required"))?;
    assert_eq!(required, &vec!["query".to_string()]);
    Ok(())
}

#[derive(Tool, Deserialize)]
struct FetchArgs {
    id: u32,
}

#[tool(description = "Fetches invoice")]
async fn fetch_invoice(
    context: &ToolExecutionContext,
    args: FetchArgs,
) -> Result<ToolOutput, ToolsError> {
    assert!(
        context
            .sandbox()
            .permissions()
            .contains(Permission::ReadWorkspace)
    );
    Ok(ToolOutput::text(format!("invoice {}", args.id)))
}

#[test]
fn tool_attr_builds_executor() -> TestResult {
    assert_eq!(FetchInvoiceTool::NAME, "fetch_invoice");
    assert_eq!(FetchInvoiceTool::DESCRIPTION, "Fetches invoice");

    let call = ToolCall {
        id: ToolCallId::from_str("call-1"),
        name: ToolName::new("fetch_invoice"),
        arguments: serde_json::json!({ "id": 42 }),
    };

    let executor = FetchInvoiceTool;
    let out = futures_lite_block_on(executor.execute(&test_execution_context()?, &call))
        .map_err(ctx("ok"))?;
    match out {
        ToolOutput::Text { content } => assert_eq!(content, "invoice 42"),
        other => {
            return Err(TestError::Unexpected(format!(
                "unexpected output: {other:?}"
            )));
        }
    }
    Ok(())
}

fn test_execution_context() -> TestResult<ToolExecutionContext> {
    let harness_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or(TestError::Missing("harw-tools has a workspace parent"))?
        .to_path_buf();
    let registry = WorkspaceRegistry::build(
        &harness_root,
        [WorkspaceRegistration {
            tenant: TenantId::from_str("test-tenant"),
            workspace: WorkspaceId::from_str("tools-tests"),
            root: PathBuf::from("harw-tools"),
        }],
    )
    .map_err(ctx("test workspace is registered"))?;
    let sandbox = SandboxSpec::from_resolved(
        registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("tools-tests"),
            )
            .map_err(ctx("test workspace resolves"))?,
        PermissionSet::from_policy([Permission::ReadWorkspace]),
    );
    Ok(ToolExecutionContext::new(
        SessionId::new(),
        TurnId::new(),
        sandbox,
    ))
}

/// Tiny dependency-free block_on so the test needs no async runtime.
///
/// `Waker::noop` and `pin!` cover both halves that previously needed `unsafe`:
/// the no-op vtable and the stack pinning.
fn futures_lite_block_on<F: std::future::Future>(fut: F) -> F::Output {
    use std::task::{Context, Poll, Waker};

    let mut cx = Context::from_waker(Waker::noop());
    let mut fut = std::pin::pin!(fut);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(v) => return v,
            Poll::Pending => continue,
        }
    }
}
