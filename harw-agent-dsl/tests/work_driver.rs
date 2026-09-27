//! `[work_driver]` (DSL §8.3): parsing through `compile_agent`, defaults,
//! range and key diagnostics (`HARW-DRIVER-*`), the delegation-rights check
//! and the guarantee that an IR without the table serializes and hashes
//! exactly as before the table existed.

mod common;

use common::{TestError, TestResult, ctx};
use harw_agent_dsl::diagnostics::{Diagnostics, SourceFile};
use harw_agent_dsl::ids::DefinitionId;
use harw_agent_dsl::ir_v2::{AgentIr, WorkDriverSpec};
use harw_agent_dsl::layers::DefinitionLayer;
use harw_agent_dsl::lower_v2::{LowerSources, compile_agent};
use time::OffsetDateTime;

const TARGET: &str = "acme.agent.driver@1";

/// An orchestrator that may delegate to `executor` and `critic`; `extra` is
/// appended verbatim (e.g. a `[work_driver]` table).
fn orchestrator(extra: &str) -> String {
    format!(
        r#"schema = "harwness.agent/v1"
id = "{TARGET}"
version = "1.0.0"
role = "child-orchestrator"
specialization = "driver"

[spawn]
max_depth = 2

[delegation]
targets = ["executor", "critic"]

[lifecycle]
max_attempts = 3

[verification]
commands = ["cargo test"]
{extra}
"#
    )
}

fn compile(text: &str) -> Result<AgentIr, Diagnostics> {
    let files = [SourceFile::new(
        DefinitionLayer::UserGlobal,
        "agents/driver/definition.toml",
        text,
    )];
    let target = DefinitionId::parse(TARGET)
        .map_err(|error| Diagnostics::from(harw_agent_dsl::Diagnostic::from_dsl_error(&error)))?;
    compile_agent(
        &target,
        &LowerSources::new(&files),
        OffsetDateTime::UNIX_EPOCH,
    )
}

fn compile_ok(text: &str) -> TestResult<AgentIr> {
    compile(text).map_err(|diagnostics| TestError::Unexpected(diagnostics.to_string()))
}

fn compile_err(text: &str) -> TestResult<Diagnostics> {
    match compile(text) {
        Ok(_) => Err(TestError::Unexpected("lowering succeeded".to_owned())),
        Err(diagnostics) => Ok(diagnostics),
    }
}

fn spec(ir: &AgentIr) -> TestResult<&WorkDriverSpec> {
    ir.work_driver
        .as_ref()
        .ok_or(TestError::Missing("work_driver"))
}

/// Asserts `code` at `path` is among the diagnostics.
fn expect_at(diagnostics: &Diagnostics, code: &str, path: &str) -> TestResult {
    let found = diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == code && diagnostic.path.as_deref() == Some(path));
    if found {
        Ok(())
    } else {
        Err(TestError::Unexpected(format!(
            "{code} at `{path}` expected, got:\n{diagnostics}"
        )))
    }
}

#[test]
fn test_full_section_is_lowered_into_the_spec() -> TestResult {
    let ir = compile_ok(&orchestrator(
        r#"
[work_driver]
max_iterations = 12
max_parallel_workers = 6
max_attempts_per_worker = 5
stall_iterations = 3
worker_role = "executor"
judge_role = "critic"
verify = ["cargo test -p harw-agent-dsl", "cargo clippy"]
token_budget = 500000
wall_budget_secs = 3600
"#,
    ))?;
    assert_eq!(
        spec(&ir)?,
        &WorkDriverSpec {
            max_iterations: 12,
            max_parallel_workers: 6,
            max_attempts_per_worker: 5,
            stall_iterations: 3,
            worker_role: "executor".to_owned(),
            judge_role: Some("critic".to_owned()),
            verify: vec![
                "cargo test -p harw-agent-dsl".to_owned(),
                "cargo clippy".to_owned(),
            ],
            token_budget: Some(500_000),
            wall_budget_secs: Some(3600),
        }
    );
    assert!(ir.verify_snapshot());
    Ok(())
}

#[test]
fn test_minimal_section_fills_the_defaults() -> TestResult {
    let ir = compile_ok(&orchestrator("[work_driver]\nworker_role = \"executor\"\n"))?;
    let spec = spec(&ir)?;
    assert_eq!(spec.max_iterations, WorkDriverSpec::DEFAULT_MAX_ITERATIONS);
    assert_eq!(
        spec.max_parallel_workers,
        WorkDriverSpec::DEFAULT_MAX_PARALLEL_WORKERS
    );
    assert_eq!(
        spec.max_attempts_per_worker, 3,
        "defaults to [lifecycle] max_attempts"
    );
    assert_eq!(
        spec.stall_iterations,
        WorkDriverSpec::DEFAULT_STALL_ITERATIONS
    );
    assert_eq!(spec.worker_role, "executor");
    assert_eq!(spec.judge_role, None);
    assert!(spec.verify.is_empty(), "verify is never taken implicitly");
    assert_eq!(spec.token_budget, None);
    assert_eq!(spec.wall_budget_secs, None);
    Ok(())
}

#[test]
fn test_attempts_default_without_lifecycle_and_stall_capped_by_iterations() -> TestResult {
    let text = orchestrator("[work_driver]\nworker_role = \"executor\"\nmax_iterations = 1\n")
        .replace("[lifecycle]\nmax_attempts = 3\n", "");
    let ir = compile_ok(&text)?;
    let spec = spec(&ir)?;
    assert_eq!(
        spec.max_attempts_per_worker,
        WorkDriverSpec::DEFAULT_MAX_ATTEMPTS_PER_WORKER
    );
    assert_eq!(spec.max_iterations, 1);
    assert_eq!(spec.stall_iterations, 1, "the default is capped");
    Ok(())
}

#[test]
fn test_unknown_key_is_driver_001_with_suggestion() -> TestResult {
    let diagnostics = compile_err(&orchestrator(
        "[work_driver]\nworker_role = \"executor\"\nmax_iteration = 8\n",
    ))?;
    expect_at(&diagnostics, "HARW-DRIVER-001", "work_driver.max_iteration")?;
    let help = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "HARW-DRIVER-001")
        .and_then(|diagnostic| diagnostic.help.clone())
        .unwrap_or_default();
    assert!(help.contains("did you mean `max_iterations`"), "{help}");
    assert!(!diagnostics.contains_code("HARW-SCHEMA-004"));
    Ok(())
}

#[test]
fn test_out_of_range_values_are_driver_002() -> TestResult {
    for (line, path) in [
        ("max_iterations = 0", "work_driver.max_iterations"),
        ("max_iterations = 1001", "work_driver.max_iterations"),
        (
            "max_parallel_workers = 0",
            "work_driver.max_parallel_workers",
        ),
        (
            "max_parallel_workers = 65",
            "work_driver.max_parallel_workers",
        ),
        (
            "max_attempts_per_worker = 101",
            "work_driver.max_attempts_per_worker",
        ),
        ("stall_iterations = 0", "work_driver.stall_iterations"),
        (
            "max_iterations = 3\nstall_iterations = 4",
            "work_driver.stall_iterations",
        ),
        ("token_budget = 0", "work_driver.token_budget"),
        ("wall_budget_secs = 0", "work_driver.wall_budget_secs"),
    ] {
        let diagnostics = compile_err(&orchestrator(&format!(
            "[work_driver]\nworker_role = \"executor\"\n{line}\n"
        )))?;
        expect_at(&diagnostics, "HARW-DRIVER-002", path)?;
    }
    Ok(())
}

#[test]
fn test_negative_and_wrongly_typed_values_use_the_parse_codes() -> TestResult {
    let negative = compile_err(&orchestrator(
        "[work_driver]\nworker_role = \"executor\"\nmax_iterations = -1\n",
    ))?;
    expect_at(&negative, "HARW-PARSE-004", "work_driver.max_iterations")?;
    let wrong = compile_err(&orchestrator(
        "[work_driver]\nworker_role = \"executor\"\nverify = \"cargo test\"\n",
    ))?;
    expect_at(&wrong, "HARW-PARSE-003", "work_driver.verify")?;
    Ok(())
}

#[test]
fn test_missing_or_malformed_roles_are_driver_003() -> TestResult {
    let missing = compile_err(&orchestrator("[work_driver]\nmax_iterations = 8\n"))?;
    expect_at(&missing, "HARW-DRIVER-003", "work_driver")?;
    let malformed = compile_err(&orchestrator(
        "[work_driver]\nworker_role = \"Executor Role\"\n",
    ))?;
    expect_at(&malformed, "HARW-DRIVER-003", "work_driver.worker_role")?;
    let judge = compile_err(&orchestrator(
        "[work_driver]\nworker_role = \"executor\"\njudge_role = \"\"\n",
    ))?;
    expect_at(&judge, "HARW-DRIVER-003", "work_driver.judge_role")?;
    Ok(())
}

#[test]
fn test_blank_verify_command_is_driver_005() -> TestResult {
    let diagnostics = compile_err(&orchestrator(
        "[work_driver]\nworker_role = \"executor\"\nverify = [\"cargo test\", \"  \"]\n",
    ))?;
    expect_at(&diagnostics, "HARW-DRIVER-005", "work_driver.verify[1]")?;
    Ok(())
}

#[test]
fn test_worker_role_outside_the_spawn_targets_is_driver_004() -> TestResult {
    let diagnostics = compile_err(&orchestrator(
        "[work_driver]\nworker_role = \"researcher\"\n",
    ))?;
    expect_at(&diagnostics, "HARW-DRIVER-004", "work_driver.worker_role")?;
    let judge = compile_err(&orchestrator(
        "[work_driver]\nworker_role = \"executor\"\njudge_role = \"umpire\"\n",
    ))?;
    expect_at(&judge, "HARW-DRIVER-004", "work_driver.judge_role")?;
    Ok(())
}

#[test]
fn test_role_without_delegation_rights_is_driver_004() -> TestResult {
    // A worker may not delegate at all, whatever its tables say.
    let worker = orchestrator("[work_driver]\nworker_role = \"executor\"\n")
        .replace("role = \"child-orchestrator\"", "role = \"worker\"")
        .replace("[spawn]\nmax_depth = 2\n", "");
    let diagnostics = compile_err(&worker)?;
    expect_at(&diagnostics, "HARW-DRIVER-004", "work_driver")?;

    // An orchestrator with no spawn depth left may not delegate either.
    let flat = orchestrator("[work_driver]\nworker_role = \"executor\"\n")
        .replace("max_depth = 2", "max_depth = 0");
    let diagnostics = compile_err(&flat)?;
    expect_at(&diagnostics, "HARW-DRIVER-004", "work_driver")?;

    // Without `[delegation] targets` nothing is a declared spawn target.
    let undeclared = orchestrator("[work_driver]\nworker_role = \"executor\"\n")
        .replace("[delegation]\ntargets = [\"executor\", \"critic\"]\n", "");
    let diagnostics = compile_err(&undeclared)?;
    expect_at(&diagnostics, "HARW-DRIVER-004", "work_driver.worker_role")?;
    Ok(())
}

#[test]
fn test_child_orchestrator_grant_counts_as_a_spawn_target() -> TestResult {
    let text = orchestrator("[work_driver]\nworker_role = \"coding-orchestrator\"\n").replace(
        "max_depth = 2",
        "max_depth = 2\nchild_orchestrators = [\"coding-orchestrator\"]",
    );
    let ir = compile_ok(&text)?;
    assert_eq!(spec(&ir)?.worker_role, "coding-orchestrator");
    Ok(())
}

#[test]
fn test_absent_section_leaves_json_and_snapshot_unchanged() -> TestResult {
    let without = compile_ok(&orchestrator(""))?;
    assert!(without.work_driver.is_none());

    // No key in the (canonical) JSON when the table is absent.
    let value = serde_json::to_value(&without).map_err(ctx("to_value"))?;
    let object = value.as_object().ok_or(TestError::Missing("object"))?;
    assert!(!object.contains_key("work_driver"));
    let canonical = String::from_utf8(without.canonical_json()).map_err(ctx("utf-8"))?;
    assert!(!canonical.contains("work_driver"), "{canonical}");

    // The same definition with the table, the spec removed again, is
    // byte-identical to the lowering without it: the section is the only
    // difference and it vanishes completely when `None`.
    let with = compile_ok(&orchestrator("[work_driver]\nworker_role = \"executor\"\n"))?;
    assert!(with.work_driver.is_some());
    assert_ne!(with.canonical_json(), without.canonical_json());
    assert_ne!(with.snapshot, without.snapshot);
    let mut stripped = with.clone();
    stripped.work_driver = None;
    assert_eq!(stripped.canonical_json(), without.canonical_json());
    assert_eq!(stripped.compute_snapshot(), without.compute_snapshot());

    // IR JSON written before the table existed deserializes unchanged.
    let json = serde_json::to_string(&without).map_err(ctx("serialize"))?;
    let back: AgentIr = serde_json::from_str(&json).map_err(ctx("deserialize"))?;
    assert_eq!(back, without);
    assert!(back.verify_snapshot());
    Ok(())
}

#[test]
fn test_spec_roundtrips_and_rejects_unknown_fields() -> TestResult {
    let ir = compile_ok(&orchestrator(
        "[work_driver]\nworker_role = \"executor\"\njudge_role = \"critic\"\n",
    ))?;
    let json = serde_json::to_string(&ir).map_err(ctx("serialize"))?;
    let back: AgentIr = serde_json::from_str(&json).map_err(ctx("deserialize"))?;
    assert_eq!(back, ir);
    assert!(back.verify_snapshot());

    let mut value = serde_json::to_value(&ir).map_err(ctx("to_value"))?;
    let driver = value
        .get_mut("work_driver")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or(TestError::Missing("work_driver"))?;
    driver.insert("surprise".to_owned(), serde_json::Value::Bool(true));
    assert!(serde_json::from_value::<AgentIr>(value).is_err());
    Ok(())
}
