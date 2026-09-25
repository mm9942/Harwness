//! Integration tests for `[binary] child_execution` (#22 wave 3C).
//!
//! `child_execution` controls how a compiled binary runs its child agents:
//! `"job"` (the default, each child a separate job-managed process) or
//! `"in-process"`. The default must leave every golden IR JSON and the v7
//! snapshot hash byte-identical, so it is `#[serde(default,
//! skip_serializing_if = "ChildExecution::is_default")]` on [`Binary`].

mod common;

use common::{TestError, TestResult, ctx};
use harw_agent_dsl::diagnostics::{Diagnostics, SourceFile};
use harw_agent_dsl::ids::DefinitionId;
use harw_agent_dsl::ir_v2::{AgentIr, Binary, ChildExecution, Interface};
use harw_agent_dsl::layers::DefinitionLayer;
use harw_agent_dsl::lower_v2::{LowerSources, compile_agent};
use time::OffsetDateTime;

const TARGET: &str = "acme.agent.t@1";
const TARGET_PATH: &str = "agents/t/definition.toml";

/// A minimal agent definition; `binary_body` is the full `[binary]` table
/// (including the header), or empty for no `[binary]` table at all.
fn agent(binary_body: &str) -> String {
    format!(
        "schema = \"harwness.agent/v1\"\nid = \"{TARGET}\"\nversion = \"1.0.0\"\nrole = \"worker\"\nspecialization = \"t\"\n\n{binary_body}\n"
    )
}

fn compile(binary_body: &str) -> TestResult<AgentIr> {
    let target = DefinitionId::parse(TARGET).map_err(ctx("target id"))?;
    let file = SourceFile::new(DefinitionLayer::UserGlobal, TARGET_PATH, agent(binary_body));
    let files = [file];
    let sources = LowerSources::new(&files);
    compile_agent(&target, &sources, OffsetDateTime::UNIX_EPOCH)
        .map_err(|diagnostics: Diagnostics| TestError::Unexpected(diagnostics.to_string()))
}

#[test]
fn test_default_child_execution_is_job() -> TestResult {
    let without_table = compile("")?;
    assert_eq!(without_table.binary.child_execution, ChildExecution::Job);

    let with_table = compile("[binary]\ninterfaces = [\"cli\"]")?;
    assert_eq!(with_table.binary.child_execution, ChildExecution::Job);
    Ok(())
}

#[test]
fn test_child_execution_default_is_not_serialized() -> TestResult {
    let ir = compile("[binary]\ninterfaces = [\"cli\"]")?;
    let value = serde_json::to_value(&ir).map_err(ctx("to_value"))?;
    let binary = value
        .get("binary")
        .and_then(serde_json::Value::as_object)
        .ok_or(TestError::Missing("binary"))?;
    assert!(
        !binary.contains_key("child_execution"),
        "the default `job` must not appear in the IR JSON: {binary:?}"
    );
    Ok(())
}

#[test]
fn test_in_process_parses_and_round_trips() -> TestResult {
    let ir = compile("[binary]\ninterfaces = [\"cli\"]\nchild_execution = \"in-process\"")?;
    assert_eq!(ir.binary.child_execution, ChildExecution::InProcess);

    let json = serde_json::to_string(&ir).map_err(ctx("serialize"))?;
    assert!(json.contains("\"child_execution\":\"in-process\""));
    let back: AgentIr = serde_json::from_str(&json).map_err(ctx("deserialize"))?;
    assert_eq!(back, ir);
    assert!(back.verify_snapshot());
    Ok(())
}

#[test]
fn test_explicit_job_matches_the_implicit_default() -> TestResult {
    let implicit = compile("[binary]\ninterfaces = [\"cli\"]")?;
    let explicit = compile("[binary]\ninterfaces = [\"cli\"]\nchild_execution = \"job\"")?;
    assert_eq!(implicit.binary, explicit.binary);
    assert_eq!(
        implicit.snapshot, explicit.snapshot,
        "an explicit default must hash exactly like an absent key"
    );
    assert_eq!(implicit.canonical_json(), explicit.canonical_json());
    Ok(())
}

/// Golden-stability test (byte-identical): a `Binary` value at the default
/// `child_execution` serializes to exactly the three-key object the DSL
/// produced before this field existed, in the same field order. If this
/// literal ever needs to change, every golden IR JSON fixture and recorded
/// snapshot hash across the workspace must be re-reviewed for a break.
#[test]
fn test_golden_binary_json_is_unchanged_without_the_key() -> TestResult {
    let binary = Binary {
        name: "reviewer".to_owned(),
        interfaces: vec![Interface::Cli, Interface::Mcp],
        default_interface: Interface::Mcp,
        child_execution: ChildExecution::default(),
    };
    let json = serde_json::to_string(&binary).map_err(ctx("serialize"))?;
    assert_eq!(
        json,
        r#"{"name":"reviewer","interfaces":["cli","mcp"],"default_interface":"mcp"}"#
    );
    Ok(())
}
