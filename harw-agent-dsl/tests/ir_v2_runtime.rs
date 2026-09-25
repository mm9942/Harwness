//! IR v2 helpers the runtime relies on (#22 wave 1B): clamping an
//! [`AgentIr`] under a ceiling (mirror of the legacy clamp, never widening)
//! and the closed vocabulary of return validators.

mod common;

use common::{TestError, TestResult};
use harw_agent_dsl::ExecutableAgentIr;
use harw_agent_dsl::diagnostics::{Diagnostics, SourceFile};
use harw_agent_dsl::ids::DefinitionId;
use harw_agent_dsl::ir_v2::{AgentIr, Effort, NetworkMode, ReturnValidator};
use harw_agent_dsl::layers::DefinitionLayer;
use harw_agent_dsl::lower_v2::{LowerSources, compile_agent};
use time::OffsetDateTime;

const CEILING: &str = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.clamp-ceiling@1"
version = "1.0.0"
role = "worker"
specialization = "clamp-ceiling"

[tools]
admitted = ["fs.read", "fs.grep", "web.fetch"]
forbidden = ["shell.exec"]

[spawn]
max_depth = 1

[spawn.budget]
max_tokens = 1000
effort_cap = "medium"

[lifecycle]
allow_pause = false
allow_rerun = true
max_attempts = 2
"#;

const CHILD: &str = r#"
schema = "harwness.agent/v1"
id = "acme.agent.clamp-child@1"
version = "1.0.0"
role = "worker"
specialization = "clamp-child"

[tools]
admitted = ["fs.read", "fs.write", "web.fetch", "shell.exec"]

[spawn]
max_depth = 5

[spawn.budget]
max_tokens = 5000
max_tool_calls = 7
effort_cap = "high"

[lifecycle]
allow_pause = true
allow_rerun = true
max_attempts = 3
"#;

fn compile_one(source: &str, target: &str) -> Result<AgentIr, Diagnostics> {
    let files = vec![SourceFile::new(
        DefinitionLayer::UserGlobal,
        "agents/clamp/definition.toml",
        source,
    )];
    let sources = LowerSources::new(&files);
    let target = DefinitionId::parse(target)
        .map_err(|error| Diagnostics::from(harw_agent_dsl::Diagnostic::from_dsl_error(&error)))?;
    compile_agent(&target, &sources, OffsetDateTime::UNIX_EPOCH)
}

fn compile(source: &str, target: &str) -> TestResult<AgentIr> {
    compile_one(source, target).map_err(|diagnostics| TestError::Unexpected(diagnostics.to_string()))
}

fn pair() -> TestResult<(AgentIr, AgentIr)> {
    Ok((
        compile(CHILD, "acme.agent.clamp-child@1")?,
        compile(CEILING, "harwness.agent.clamp-ceiling@1")?,
    ))
}

#[test]
fn test_clamped_ir_keeps_only_what_both_sides_allow() -> TestResult {
    let (child, ceiling) = pair()?;
    let clamped = child.clamped_to(&ceiling);

    assert_eq!(clamped.tools.admitted, ["fs.read", "web.fetch"]);
    assert!(clamped.tools.forbidden.iter().any(|tool| tool == "shell.exec"));
    assert_eq!(clamped.spawn.max_depth, Some(1));
    let budget = clamped.spawn.budget.clone().ok_or(TestError::Missing("budget"))?;
    assert_eq!(budget.max_tokens, Some(1000));
    assert_eq!(budget.max_tool_calls, Some(7));
    assert_eq!(budget.effort_cap, Some(Effort::Medium));
    assert!(!clamped.lifecycle.allow_pause);
    assert!(clamped.lifecycle.allow_rerun);
    assert_eq!(clamped.lifecycle.max_attempts, Some(2));

    // The manifest is derived again from the clamped tools.
    assert!(!clamped.permissions.filesystem.write);
    assert!(!clamped.permissions.shell);
    assert_eq!(clamped.permissions.network.mode, NetworkMode::Allowlist);
    assert_eq!(clamped.permissions.tools, ["fs.read", "web.fetch"]);
    assert_eq!(clamped.permissions.spawn.max_depth, 1);
    assert!(clamped.verify_snapshot(), "the snapshot covers the clamped IR");
    assert_ne!(clamped.snapshot, child.snapshot);

    // Identity and descriptive sections stay the child's.
    assert_eq!(clamped.id, child.id);
    assert_eq!(clamped.specialization, child.specialization);
    Ok(())
}

#[test]
fn test_clamped_ir_never_widens_either_side() -> TestResult {
    let (child, ceiling) = pair()?;
    for clamped in [child.clamped_to(&ceiling), ceiling.clamped_to(&child)] {
        for tool in &clamped.permissions.tools {
            assert!(child.permissions.tools.contains(tool), "{tool}");
            assert!(ceiling.permissions.tools.contains(tool), "{tool}");
        }
        assert!(!clamped.permissions.shell || (child.permissions.shell && ceiling.permissions.shell));
        assert!(
            !clamped.permissions.filesystem.write
                || (child.permissions.filesystem.write && ceiling.permissions.filesystem.write)
        );
        assert!(clamped.permissions.spawn.max_depth <= child.permissions.spawn.max_depth);
        assert!(clamped.permissions.spawn.max_depth <= ceiling.permissions.spawn.max_depth);
    }
    Ok(())
}

/// The typed clamp and the legacy clamp agree on every field of the legacy
/// view (including its v6 snapshot), so the roster may clamp either form.
#[test]
fn test_clamped_ir_view_equals_the_legacy_clamp() -> TestResult {
    let (child, ceiling) = pair()?;
    let typed = ExecutableAgentIr::from(&child.clamped_to_with(&ceiling, &["fs.write"]));
    let legacy = ExecutableAgentIr::from(&child)
        .clamped_to_with(&ExecutableAgentIr::from(&ceiling), &["fs.write"]);
    assert_eq!(
        typed.tool_surface().admitted(),
        legacy.tool_surface().admitted()
    );
    assert_eq!(
        typed.tool_surface().forbidden(),
        legacy.tool_surface().forbidden()
    );
    assert_eq!(
        typed.spawn_contract().max_depth(),
        legacy.spawn_contract().max_depth()
    );
    assert_eq!(
        typed.lifecycle_machine().max_attempts(),
        legacy.lifecycle_machine().max_attempts()
    );
    assert_eq!(typed.snapshot_id(), legacy.snapshot_id());
    assert!(
        typed
            .tool_surface()
            .admitted()
            .iter()
            .any(|tool| tool == "fs.write"),
        "extra_allowed admits a tool the child itself admits"
    );

    let capped = child.with_max_depth_at_most(0).with_additional_admitted(&["fs.grep"]);
    assert_eq!(capped.spawn.max_depth, Some(0));
    assert!(capped.tools.admitted.iter().any(|tool| tool == "fs.grep"));
    assert!(capped.verify_snapshot());
    Ok(())
}

#[test]
fn test_return_validator_labels_roundtrip() {
    for validator in ReturnValidator::ALL {
        assert_eq!(ReturnValidator::parse(validator.as_str()), Some(validator));
    }
    assert_eq!(ReturnValidator::parse("schema.v1"), None);
    assert!(ReturnValidator::known_labels().contains("json-object"));
}

#[test]
fn test_known_validators_lower_and_unknown_ones_are_rejected() -> TestResult {
    let known = CHILD.replace(
        "[lifecycle]",
        "[return]\nvalidators = [\"non-empty\", \"json-object\"]\n\n[lifecycle]",
    );
    let ir = compile(&known, "acme.agent.clamp-child@1")?;
    assert_eq!(ir.return_pipeline.validators, ["non-empty", "json-object"]);

    let unknown = CHILD.replace(
        "[lifecycle]",
        "[return]\nvalidators = [\"redact.secrets\"]\n\n[lifecycle]",
    );
    let diagnostics = match compile_one(&unknown, "acme.agent.clamp-child@1") {
        Ok(_) => return Err(TestError::Unexpected("an unknown validator lowered".to_owned())),
        Err(diagnostics) => diagnostics,
    };
    let diagnostic = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "HARW-RETURN-002")
        .ok_or(TestError::Missing("HARW-RETURN-002"))?;
    assert!(diagnostic.message.contains("unknown validator"), "{}", diagnostic.message);
    assert_eq!(diagnostic.path.as_deref(), Some("return.validators[0]"));
    let rendered = diagnostics.to_string();
    assert!(rendered.contains("definition.toml"), "{rendered}");
    Ok(())
}
