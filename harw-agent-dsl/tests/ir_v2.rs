//! Integration tests for Agent IR v2 (#22 wave 1): lowering, serde, the v7
//! snapshot hash, the legacy view and the resolver fixes, through the public
//! API only.

mod common;

use common::{TestError, TestResult, ctx};
use harw_agent_dsl::bind::ContextProgramLibrary;
use harw_agent_dsl::diagnostics::{Diagnostics, SourceFile};
use harw_agent_dsl::ids::DefinitionId;
use harw_agent_dsl::ir_v2::{
    AGENT_IR_SCHEMA, AGENT_IR_SNAPSHOT_DOMAIN, AgentIr, Effort, Interface, NetworkMode,
    ReturnContract,
};
use harw_agent_dsl::layers::DefinitionLayer;
use harw_agent_dsl::lower_v2::{LowerSources, MapInstructionsLoader, compile_agent};
use harw_agent_dsl::parse::parse_toml;
use harw_agent_dsl::resolve::resolve_definition;
use harw_agent_dsl::{ExecutableAgentIr, lower};
use time::OffsetDateTime;

const BASE: &str = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.v2-base@1"
version = "1.0.0"
role = "worker"
specialization = "v2-base"

[lifecycle]
allow_pause = false
allow_rerun = true
max_attempts = 2

[context]
must_include = ["task.objective", "task.read_scope"]
exclude = ["full_parent_transcript"]
"#;

const AGENT: &str = r#"
schema = "harwness.agent/v1"
id = "acme.agent.reviewer@1"
version = "1.2.0"
extends = { id = "harwness.agent.v2-base@1" }
role = "worker"
specialization = "reviewer"
name = "Reviewer"
description = "Reviews things."
reasoning_effort = "medium"
instructions_file = "system.md"
skills = ["code-review"]

[work]
mode = "review"
may_write_code = false
may_research_web = true

[tools]
admitted = ["fs.read", "fs.grep", "web.fetch", "skills.load"]
forbidden = ["fs.write", "shell.exec"]

[spawn]
max_depth = 0

[spawn.budget]
max_tokens = 80000
max_tool_calls = 50
max_wall_secs = 600
effort_cap = "high"

[return]
contract = "harwness.return.research-finding@1"
validators = ["non-empty"]

[models]
provider = "anthropic"
model = "claude-sonnet-5"
effort = "high"
fallbacks = ["openai/gpt-5.6-codex"]
required_env = ["ANTHROPIC_API_KEY"]

[limits]
max_tool_calls = 40

[verification]
profile = "harwness.verification.review@1"

[network]
hosts = ["docs.rs"]

[binary]
name = "reviewer"
interfaces = ["cli", "mcp"]
default_interface = "mcp"
"#;

const PROGRAM_BASE: &str = r#"
schema = "harwness.context/v1"
id = "harwness.context.base@1"
version = "1.0.0"
exclude = ["secret.*"]

[[sections]]
name = "task.objective"
strength = "must-include"
detail = "full"
trust = "instruction"
"#;

const PROGRAM_REVIEW: &str = r#"
schema = "harwness.context/v1"
id = "harwness.context.review@1"
version = "1.0.0"
extends = { id = "harwness.context.base@1" }

[[sections]]
name = "diff.changeset"
strength = "must-include"
detail = "full"
trust = "evidence"

[[sections]]
name = "history.tail"
detail = "summary"
trust = "data"
"#;

fn id(value: &str) -> TestResult<DefinitionId> {
    DefinitionId::parse(value).map_err(ctx("id parses"))
}

fn files(agent: &str) -> Vec<SourceFile> {
    vec![
        SourceFile::new(DefinitionLayer::BuiltIn, "builtin/v2-base.toml", BASE),
        SourceFile::new(
            DefinitionLayer::UserGlobal,
            "agents/reviewer/definition.toml",
            agent,
        ),
    ]
}

fn loader(text: &str) -> MapInstructionsLoader {
    let mut loader = MapInstructionsLoader::new();
    loader.insert("agents/reviewer/system.md", text);
    loader
}

fn library() -> TestResult<ContextProgramLibrary> {
    ContextProgramLibrary::from_builtin_sources(&[
        ("base", PROGRAM_BASE),
        ("review", PROGRAM_REVIEW),
    ])
    .map_err(ctx("library parses"))
}

fn compile_with(
    agent: &str,
    instructions: &str,
    now: OffsetDateTime,
) -> Result<AgentIr, Diagnostics> {
    let files = files(agent);
    let loader = loader(instructions);
    let library = ContextProgramLibrary::new();
    let sources = LowerSources::new(&files)
        .with_instructions(&loader)
        .with_context_programs(&library);
    let target = DefinitionId::parse("acme.agent.reviewer@1")
        .map_err(|error| Diagnostics::from(harw_agent_dsl::Diagnostic::from_dsl_error(&error)))?;
    compile_agent(&target, &sources, now)
}

fn compile(agent: &str) -> TestResult<AgentIr> {
    compile_with(agent, "Review carefully.", OffsetDateTime::UNIX_EPOCH)
        .map_err(|diagnostics| TestError::Unexpected(diagnostics.to_string()))
}

#[test]
fn test_lower_v2_types_every_section() -> TestResult {
    let ir = compile(AGENT)?;
    assert_eq!(ir.schema, AGENT_IR_SCHEMA);
    assert_eq!(ir.id, id("acme.agent.reviewer@1")?);
    assert_eq!(ir.version.0, semver::Version::new(1, 2, 0));
    assert_eq!(ir.name.as_deref(), Some("Reviewer"));
    assert_eq!(ir.reasoning_effort, Some(Effort::Medium));
    assert_eq!(ir.instructions.text, "Review carefully.");
    assert_eq!(ir.instructions.source.as_deref(), Some("system.md"));
    assert_eq!(ir.tools.admitted.len(), 4);
    assert_eq!(ir.spawn.max_depth, Some(0));
    let budget = ir
        .spawn
        .budget
        .clone()
        .ok_or(TestError::Missing("budget"))?;
    assert_eq!(budget.max_tokens, Some(80_000));
    assert_eq!(budget.effort_cap, Some(Effort::High));
    assert!(ir.lifecycle.allow_rerun);
    assert_eq!(ir.lifecycle.max_attempts, Some(2));
    assert_eq!(
        ir.context.must_include,
        ["task.objective", "task.read_scope"]
    );
    assert_eq!(
        ir.return_pipeline.contract,
        Some(ReturnContract::ResearchFinding)
    );
    assert_eq!(ir.return_pipeline.validators, ["non-empty"]);
    let models = ir.models.clone().ok_or(TestError::Missing("models"))?;
    assert_eq!(models.fallbacks[0].provider, "openai");
    assert_eq!(models.required_env, ["ANTHROPIC_API_KEY"]);
    assert_eq!(
        ir.limits.as_ref().and_then(|limits| limits.max_tool_calls),
        Some(40)
    );
    assert_eq!(
        ir.work.as_ref().and_then(|work| work.mode.as_deref()),
        Some("review")
    );
    assert!(ir.verification.is_some());
    assert_eq!(ir.skill_names(), ["code-review"]);
    assert!(ir.skills.entries.iter().all(|entry| entry.hash.is_none()));
    assert_eq!(ir.binary.interfaces, [Interface::Cli, Interface::Mcp]);
    assert_eq!(ir.binary.default_interface, Interface::Mcp);
    assert_eq!(ir.permissions.network.mode, NetworkMode::Allowlist);
    assert_eq!(ir.permissions.network.hosts, ["docs.rs"]);
    assert_eq!(ir.permissions.required_env, ["ANTHROPIC_API_KEY"]);
    assert!(!ir.permissions.shell);
    assert!(ir.permissions.filesystem.read && !ir.permissions.filesystem.write);
    assert!(ir.verify_snapshot());
    Ok(())
}

#[test]
fn test_serde_roundtrip_preserves_the_ir_and_its_snapshot() -> TestResult {
    let ir = compile(AGENT)?;
    let json = serde_json::to_string_pretty(&ir).map_err(ctx("serialize"))?;
    let back: AgentIr = serde_json::from_str(&json).map_err(ctx("deserialize"))?;
    assert_eq!(back, ir);
    assert!(back.verify_snapshot());
    Ok(())
}

#[test]
fn test_deny_unknown_fields_at_top_level_and_nested() -> TestResult {
    let ir = compile(AGENT)?;
    let value = serde_json::to_value(&ir).map_err(ctx("to_value"))?;

    let mut top = value.clone();
    let object = top.as_object_mut().ok_or(TestError::Missing("object"))?;
    object.insert("surprise".to_owned(), serde_json::Value::Bool(true));
    assert!(serde_json::from_value::<AgentIr>(top).is_err());

    let mut nested = value.clone();
    let tools = nested
        .get_mut("tools")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or(TestError::Missing("tools"))?;
    tools.insert("extra".to_owned(), serde_json::Value::Null);
    assert!(serde_json::from_value::<AgentIr>(nested).is_err());

    let mut permissions = value;
    let network = permissions
        .get_mut("permissions")
        .and_then(|value| value.get_mut("network"))
        .and_then(serde_json::Value::as_object_mut)
        .ok_or(TestError::Missing("permissions.network"))?;
    network.insert("ports".to_owned(), serde_json::Value::Null);
    assert!(serde_json::from_value::<AgentIr>(permissions).is_err());
    assert!(ir.verify_snapshot());
    Ok(())
}

#[test]
fn test_snapshot_is_stable_across_lowerings_and_ignores_the_trace() -> TestResult {
    let first = compile_with(AGENT, "Review carefully.", OffsetDateTime::UNIX_EPOCH)
        .map_err(|d| TestError::Unexpected(d.to_string()))?;
    let second = compile_with(AGENT, "Review carefully.", OffsetDateTime::now_utc())
        .map_err(|d| TestError::Unexpected(d.to_string()))?;
    assert_ne!(first.trace, second.trace, "the trace carries the timestamp");
    assert_eq!(first.snapshot, second.snapshot);
    assert_eq!(first.canonical_json(), second.canonical_json());
    let snapshot = first.snapshot.ok_or(TestError::Missing("snapshot"))?;
    assert_eq!(snapshot.domain, AGENT_IR_SNAPSHOT_DOMAIN);
    assert_eq!(snapshot.digest.len(), 64);
    Ok(())
}

#[test]
fn test_snapshot_changes_with_version_instructions_and_binary() -> TestResult {
    let original = compile(AGENT)?;
    let bumped = compile(&AGENT.replace("version = \"1.2.0\"", "version = \"1.2.1\""))?;
    assert_ne!(original.snapshot, bumped.snapshot, "version is hashed");

    let reworded = compile_with(AGENT, "Review very carefully.", OffsetDateTime::UNIX_EPOCH)
        .map_err(|d| TestError::Unexpected(d.to_string()))?;
    assert_ne!(
        original.snapshot, reworded.snapshot,
        "instructions are hashed"
    );

    let rebinned =
        compile(&AGENT.replace("default_interface = \"mcp\"", "default_interface = \"cli\""))?;
    assert_ne!(
        original.snapshot, rebinned.snapshot,
        "binary settings are hashed"
    );
    Ok(())
}

#[test]
fn test_snapshot_ignores_the_order_of_set_valued_lists() -> TestResult {
    let original = compile(AGENT)?;
    let reordered = compile(&AGENT.replace(
        "admitted = [\"fs.read\", \"fs.grep\", \"web.fetch\", \"skills.load\"]",
        "admitted = [\"skills.load\", \"web.fetch\", \"fs.grep\", \"fs.read\"]",
    ))?;
    assert_ne!(original.tools.admitted, reordered.tools.admitted);
    assert_eq!(original.snapshot, reordered.snapshot);
    Ok(())
}

#[test]
fn test_tampered_ir_fails_snapshot_verification() -> TestResult {
    let mut ir = compile(AGENT)?;
    ir.permissions.shell = true;
    assert!(!ir.verify_snapshot());
    Ok(())
}

/// The legacy view built from the v2 IR equals `lower` for the same
/// definition, snapshot (v6) included.
#[test]
fn test_executable_view_equals_legacy_lower() -> TestResult {
    let ir = compile(AGENT)?;
    let view = ExecutableAgentIr::from(&ir);

    let layers = vec![
        (
            DefinitionLayer::BuiltIn,
            parse_toml(BASE).map_err(ctx("base parses"))?,
        ),
        (
            DefinitionLayer::UserGlobal,
            parse_toml(AGENT).map_err(ctx("agent parses"))?,
        ),
    ];
    let resolved = resolve_definition(
        &id("acme.agent.reviewer@1")?,
        &layers,
        OffsetDateTime::UNIX_EPOCH,
    )
    .map_err(ctx("resolves"))?;
    let legacy = lower(&resolved).map_err(ctx("lowers"))?;

    assert_eq!(view.snapshot_id(), legacy.snapshot_id());
    assert_eq!(view.id(), legacy.id());
    assert_eq!(view.reasoning_effort(), legacy.reasoning_effort());
    assert_eq!(view.skills(), legacy.skills());
    assert_eq!(
        view.tool_surface().admitted(),
        legacy.tool_surface().admitted()
    );
    assert_eq!(
        view.return_pipeline().contract(),
        legacy.return_pipeline().contract()
    );
    assert_eq!(
        view.spawn_contract().budget().and_then(|b| b.effort_cap()),
        Some("high")
    );
    assert_eq!(view.trace().steps.len(), legacy.trace().steps.len());
    Ok(())
}

#[test]
fn test_legacy_lower_rejects_an_unknown_return_contract() -> TestResult {
    let raw = parse_toml(
        r#"
schema = "harwness.agent/v1"
id = "harwness.agent.contract-bad@1"
version = "1.0.0"
role = "worker"
specialization = "contract-bad"

[return]
contract = "harwness.return.nope@1"
"#,
    )
    .map_err(ctx("parses"))?;
    let resolved = resolve_definition(
        &id("harwness.agent.contract-bad@1")?,
        &[(DefinitionLayer::BuiltIn, raw)],
        OffsetDateTime::UNIX_EPOCH,
    )
    .map_err(ctx("resolves"))?;
    let result = lower(&resolved);
    assert!(
        matches!(
            result,
            Err(harw_agent_dsl::error::DslError::UnknownReturnContract { .. })
        ),
        "{:?}",
        result.map(|ir| ir.specialization().to_owned())
    );
    Ok(())
}

#[test]
fn test_context_program_binding_keeps_detail_trust_and_strength() -> TestResult {
    let agent = AGENT.replace(
        "[work]",
        "[context]\nprogram = \"review\"\nmust_include = [\"history.tail\"]\n\n[work]",
    );
    let files = files(&agent);
    let loader = loader("x");
    let library = library()?;
    let sources = LowerSources::new(&files)
        .with_instructions(&loader)
        .with_context_programs(&library);
    let ir = compile_agent(
        &id("acme.agent.reviewer@1")?,
        &sources,
        OffsetDateTime::UNIX_EPOCH,
    )
    .map_err(|d| TestError::Unexpected(d.to_string()))?;
    assert_eq!(ir.context.program.as_deref(), Some("review"));
    assert_eq!(
        ir.context.program_id.as_deref(),
        Some("harwness.context.review@1")
    );
    assert_eq!(
        ir.context.policy.as_deref(),
        Some("harwness.context.review@1")
    );
    let names: Vec<&str> = ir
        .context
        .sections
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(names, ["task.objective", "diff.changeset", "history.tail"]);
    assert_eq!(
        ir.context.sections[1].trust,
        harw_context::TrustClass::Evidence
    );
    assert_eq!(
        ir.context.sections[2].detail,
        harw_context::DetailMode::Summary
    );
    assert_eq!(ir.context.deferred, ["diff.changeset"]);
    // Program must-includes inside the ceiling first, then inline selectors
    // (inherited from the base, then the definition's own).
    assert_eq!(
        ir.context.must_include,
        ["task.objective", "task.read_scope", "history.tail"]
    );
    assert!(ir.context.exclude.contains(&"secret.*".to_owned()));
    assert!(
        ir.trace
            .diagnostics
            .iter()
            .any(|d| d.code == "HARW-CTX-003"),
        "deferred sections are noted"
    );
    // The legacy view keeps `section_detail` empty (the runtime checks it
    // against the ceiling), the v2 IR keeps the full program.
    let view = ExecutableAgentIr::from(&ir);
    assert!(view.context_program().section_detail().is_empty());
    assert_eq!(
        view.context_program().context_policy(),
        Some("harwness.context.review@1")
    );
    Ok(())
}

#[test]
fn test_user_program_from_a_higher_layer_binds() -> TestResult {
    let agent = AGENT.replace("[work]", "[context]\nprogram = \"mine\"\n\n[work]");
    let files = files(&agent);
    let loader = loader("x");
    let mut library = library()?;
    library
        .insert_sources(
            DefinitionLayer::UserGlobal,
            &[(
                "mine",
                "schema = \"harwness.context/v1\"\nid = \"acme.context.mine@1\"\nversion = \"1.0.0\"\n\n[[sections]]\nname = \"history.tail\"\nstrength = \"must-include\"\n",
            )],
        )
        .map_err(ctx("user program registers"))?;
    let sources = LowerSources::new(&files)
        .with_instructions(&loader)
        .with_context_programs(&library);
    let ir = compile_agent(
        &id("acme.agent.reviewer@1")?,
        &sources,
        OffsetDateTime::UNIX_EPOCH,
    )
    .map_err(|d| TestError::Unexpected(d.to_string()))?;
    assert_eq!(
        ir.context.program_id.as_deref(),
        Some("acme.context.mine@1")
    );
    assert_eq!(ir.context.must_include[0], "history.tail");
    Ok(())
}

#[test]
fn test_binary_defaults_to_the_specialization_and_cli() -> TestResult {
    let agent = AGENT.replace(
        "[binary]\nname = \"reviewer\"\ninterfaces = [\"cli\", \"mcp\"]\ndefault_interface = \"mcp\"\n",
        "",
    );
    let ir = compile(&agent)?;
    assert_eq!(ir.binary.name, "reviewer");
    assert_eq!(ir.binary.interfaces, [Interface::Cli]);
    assert_eq!(ir.binary.default_interface, Interface::Cli);
    Ok(())
}

#[test]
fn test_nested_patch_through_compile_agent() -> TestResult {
    let agent =
        format!("{AGENT}\n[patch.context.must_include]\nappend = [\"task.acceptance_criteria\"]\n");
    let ir = compile(&agent)?;
    assert_eq!(
        ir.context.must_include,
        [
            "task.objective",
            "task.read_scope",
            "task.acceptance_criteria"
        ]
    );
    Ok(())
}

#[test]
fn test_min_and_max_within_parent_through_compile_agent() -> TestResult {
    let base = BASE.replace(
        "[lifecycle]",
        "[limits]\nmax_tool_calls = 40\nmax_wall_time_seconds = 1800\n\n[lifecycle]",
    );
    let agent = AGENT
        .replace("[limits]\nmax_tool_calls = 40\n", "")
        .replace(
            "[binary]",
            "[patch.limits]\nmax_tool_calls = { min = 24 }\nmax_wall_time_seconds = { max-within-parent = 900 }\n\n[binary]",
        );
    let files = vec![
        SourceFile::new(
            DefinitionLayer::BuiltIn,
            "builtin/v2-base.toml",
            base.as_str(),
        ),
        SourceFile::new(
            DefinitionLayer::UserGlobal,
            "agents/reviewer/definition.toml",
            agent.as_str(),
        ),
    ];
    let loader = loader("x");
    let sources = LowerSources::new(&files).with_instructions(&loader);
    let ir = compile_agent(
        &id("acme.agent.reviewer@1")?,
        &sources,
        OffsetDateTime::UNIX_EPOCH,
    )
    .map_err(|d| TestError::Unexpected(d.to_string()))?;
    let limits = ir.limits.ok_or(TestError::Missing("limits"))?;
    assert_eq!(limits.max_tool_calls, Some(24));
    assert_eq!(limits.max_wall_time_seconds, Some(900));
    Ok(())
}

#[test]
fn test_wrongly_typed_value_is_an_error_not_absent() -> TestResult {
    let agent = AGENT.replace("max_depth = 0", "max_depth = \"zero\"");
    let Err(diagnostics) = compile_with(&agent, "x", OffsetDateTime::UNIX_EPOCH) else {
        return Err(TestError::Unexpected(
            "a string max_depth must not lower".to_owned(),
        ));
    };
    let diagnostic = diagnostics
        .iter()
        .find(|d| d.code == "HARW-PARSE-003")
        .ok_or(TestError::Missing("HARW-PARSE-003"))?;
    assert_eq!(diagnostic.path.as_deref(), Some("spawn.max_depth"));
    let span = diagnostic.span.as_ref().ok_or(TestError::Missing("span"))?;
    assert_eq!(span.file, "agents/reviewer/definition.toml");
    let line = agent
        .lines()
        .position(|line| line.starts_with("max_depth"))
        .ok_or(TestError::Missing("line"))?;
    assert_eq!(span.line as usize, line + 1);
    Ok(())
}
