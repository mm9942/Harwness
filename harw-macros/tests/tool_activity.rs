//! PL-97 M2: `#[tool(activity(...), skill = "...")]` against the real
//! `harw_tools` types.
//!
//! Covers the generated `ACTIVITY` / `SKILL` constants, the provider tables
//! `TOOL_ACTIVITIES` / `TOOL_SKILLS`, the `manage_containers` permission and
//! the golden proof that neither key enters the model-facing `ToolSpec`.

mod common;

use common::{TestError, TestResult};
use harw_tools::{
    ActivityDescriptor, Effect, Permission, ToolExecutionContext, ToolOutput, ToolSpec, ToolsError,
};
use serde::Deserialize;

/// Arguments shared by the annotated tool and its plain twin.
#[derive(Deserialize, harw_macros::Tool)]
#[allow(dead_code)]
struct ReadArgs {
    /// Path to read.
    path: String,
}

#[harw_macros::tool(
    name = "demo.read",
    description = "Reads a file",
    permission = "read_workspace",
    parallel_safe,
    activity(
        domain = "filesystem",
        verb = "read",
        resource = "path",
        effect = "observe"
    ),
    skill = "demo-tools"
)]
async fn annotated_read(
    context: &ToolExecutionContext,
    args: ReadArgs,
) -> Result<ToolOutput, ToolsError> {
    let _ = (context, args);
    Ok(ToolOutput::text("annotated"))
}

/// The same tool without `activity` and `skill`.
#[harw_macros::tool(
    name = "demo.read",
    description = "Reads a file",
    permission = "read_workspace",
    parallel_safe
)]
async fn plain_read(
    context: &ToolExecutionContext,
    args: ReadArgs,
) -> Result<ToolOutput, ToolsError> {
    let _ = (context, args);
    Ok(ToolOutput::text("plain"))
}

/// Arguments with an optional resource field.
#[derive(Deserialize, harw_macros::Tool)]
#[allow(dead_code)]
struct EditArgs {
    /// File to edit.
    target: Option<String>,
    /// Replacement text.
    text: String,
}

#[harw_macros::tool(
    name = "demo.edit",
    permission = "write_workspace",
    activity(
        effect = "mutate",
        resource = "target",
        verb = "edit",
        domain = "filesystem"
    )
)]
async fn demo_edit(
    context: &ToolExecutionContext,
    args: EditArgs,
) -> Result<ToolOutput, ToolsError> {
    let _ = (context, args);
    Ok(ToolOutput::text("edited"))
}

/// Arguments without any field: `activity` without `resource`.
#[derive(Deserialize, harw_macros::Tool)]
struct NoArgs {}

#[harw_macros::tool(
    name = "demo.containers",
    permission = "manage_containers",
    activity(domain = "container", verb = "list", effect = "observe")
)]
async fn demo_containers(
    context: &ToolExecutionContext,
    args: NoArgs,
) -> Result<ToolOutput, ToolsError> {
    let _ = (context, args);
    Ok(ToolOutput::text("[]"))
}

/// Stateful tool with an external schema: both keys still work.
#[harw_macros::tool(
    name = "demo.run",
    state = u32,
    schema_from = run_spec,
    activity(domain = "job", verb = "start", resource = "path", effect = "execute"),
    skill = "job-tools"
)]
async fn demo_run(
    state: &u32,
    context: &ToolExecutionContext,
    args: ReadArgs,
) -> Result<ToolOutput, ToolsError> {
    let _ = (state, context, args);
    Ok(ToolOutput::text("started"))
}

fn run_spec() -> ToolSpec {
    ReadArgs::tool_spec()
}

/// A plain tool with its own name, for the provider tables.
#[harw_macros::tool(name = "demo.plain")]
async fn demo_plain(
    context: &ToolExecutionContext,
    args: NoArgs,
) -> Result<ToolOutput, ToolsError> {
    let _ = (context, args);
    Ok(ToolOutput::text("plain"))
}

harw_tools::tool_provider_core! {
    /// Provider over annotated and plain tools, in this order.
    struct DemoProvider { AnnotatedReadTool, DemoEditTool, DemoPlainTool, DemoContainersTool }
}

#[test]
fn annotated_tool_exposes_activity_and_skill() {
    assert_eq!(
        AnnotatedReadTool::ACTIVITY,
        Some(ActivityDescriptor::new(
            "filesystem",
            "read",
            Some("path"),
            Effect::Observe
        ))
    );
    assert_eq!(AnnotatedReadTool::SKILL, Some("demo-tools"));
    // Die Activity-Metadaten ändern weder Permission noch Parallelität.
    assert_eq!(AnnotatedReadTool::PERMISSION, PlainReadTool::PERMISSION);
    assert_eq!(
        AnnotatedReadTool::PARALLEL_SAFE,
        PlainReadTool::PARALLEL_SAFE
    );
}

#[test]
fn plain_tool_has_no_activity_and_no_skill() {
    assert_eq!(PlainReadTool::ACTIVITY, None);
    assert_eq!(PlainReadTool::SKILL, None);
}

/// Key order inside `activity(...)` is free; an `Option` field is a valid
/// resource.
#[test]
fn activity_keys_are_order_independent() {
    assert_eq!(
        DemoEditTool::ACTIVITY,
        Some(ActivityDescriptor::new(
            "filesystem",
            "edit",
            Some("target"),
            Effect::Mutate
        ))
    );
    assert_eq!(DemoEditTool::SKILL, None);
}

#[test]
fn activity_without_resource_has_no_resource_field() -> TestResult {
    let activity = DemoContainersTool::ACTIVITY.ok_or(TestError::Missing("activity"))?;
    assert_eq!(activity.resource_field, None);
    assert!(activity.is_known());
    Ok(())
}

/// PL-97 §2.2: `manage_containers` was missing from the permission table.
#[test]
fn manage_containers_permission_resolves() {
    assert_eq!(
        DemoContainersTool::PERMISSION,
        Some(Permission::ManageContainers)
    );
}

#[test]
fn stateful_tool_with_external_schema_takes_activity_and_skill() {
    assert_eq!(
        DemoRunTool::ACTIVITY,
        Some(ActivityDescriptor::new(
            "job",
            "start",
            Some("path"),
            Effect::Execute
        ))
    );
    assert_eq!(DemoRunTool::SKILL, Some("job-tools"));
    let _executor = DemoRunTool::new(7);
}

/// The descriptor is a true constant (built through the `const fn`
/// `ActivityDescriptor::new`).
#[test]
fn activity_is_usable_in_const_context() {
    const DESCRIPTOR: Option<ActivityDescriptor> = AnnotatedReadTool::ACTIVITY;
    const SKILL: Option<&str> = AnnotatedReadTool::SKILL;
    assert!(DESCRIPTOR.is_some());
    assert_eq!(SKILL, Some("demo-tools"));
}

/// Golden: the annotated tool advertises exactly the same spec JSON as its
/// plain twin, and that JSON contains nothing of the activity or skill.
#[test]
fn spec_json_is_unchanged_by_activity_and_skill() -> TestResult {
    let annotated = serde_json::to_value(AnnotatedReadTool::spec())
        .map_err(|error| TestError::Unexpected(error.to_string()))?;
    let plain = serde_json::to_value(PlainReadTool::spec())
        .map_err(|error| TestError::Unexpected(error.to_string()))?;
    let golden = serde_json::json!({
        "type": "function",
        "name": "demo.read",
        "description": "Reads a file",
        "parameters": {
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to read." }
            },
            "required": ["path"]
        }
    });
    assert_eq!(annotated, golden);
    assert_eq!(plain, golden);
    let text = annotated.to_string();
    for needle in ["filesystem", "observe", "demo-tools", "activity", "skill"] {
        assert!(
            !text.contains(needle),
            "{needle} leaked into the spec: {text}"
        );
    }
    Ok(())
}

#[test]
fn provider_tables_follow_declaration_order() {
    assert_eq!(
        DemoProvider::TOOL_NAMES,
        &["demo.read", "demo.edit", "demo.plain", "demo.containers"]
    );
    assert_eq!(
        DemoProvider::TOOL_ACTIVITIES,
        &[
            AnnotatedReadTool::ACTIVITY,
            DemoEditTool::ACTIVITY,
            None,
            DemoContainersTool::ACTIVITY,
        ]
    );
    assert_eq!(
        DemoProvider::TOOL_SKILLS,
        &[Some("demo-tools"), None, None, None]
    );
}

/// The rest of the generated provider surface still works next to the new
/// tables (and keeps every generated item in use).
#[test]
fn provider_serves_specs_executors_and_permissions() {
    let _provider = DemoProvider::new();
    let specs = DemoProvider::tool_specs();
    let names: Vec<&str> = specs.iter().map(ToolSpec::name).collect();
    assert_eq!(names, DemoProvider::TOOL_NAMES);
    assert_eq!(
        DemoProvider::TOOL_PERMISSIONS,
        &[
            Some(Permission::ReadWorkspace),
            Some(Permission::WriteWorkspace),
            None,
            Some(Permission::ManageContainers),
        ]
    );
    assert!(DemoProvider::tool_executor("demo.edit").is_some());
    assert!(DemoProvider::tool_executor("demo.unknown").is_none());
    assert!(DemoProvider::tool_parallel_safe("demo.read"));
    assert!(!DemoProvider::tool_parallel_safe("demo.edit"));
}
