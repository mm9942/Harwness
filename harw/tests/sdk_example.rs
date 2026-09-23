//! Integration test for the `harw` facade crate.
//!
//! Spec source: task "Write a minimal SDK-consumer integration test proving that
//! the `harw` facade crate is enough to compose a runnable HARW setup end-to-end."
//!
//! This test proves that:
//! (a) the facade re-exports enough to be usable without reaching into sub-crates,
//! (b) the module layout makes sense from a consumer perspective, and
//! (c) the ergonomics of `harw::prelude::*` are acceptable.

mod common;

use common::{TestResult, ctx};
use harw::prelude::*;

// TOML fixture copied from harw-agent-dsl/src/raw.rs unit tests (minimal shape).
const MINIMAL_AGENT_TOML: &str = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.sdk-example@1"
version = "1.0.0"
role = "worker"
specialization = "sdk-example"
"#;

/// Verifies that the `harw` facade exposes enough API surface to compose a
/// minimal, plausible HARW setup without touching any sub-crate directly.
///
/// Steps:
/// 1. Build an empty `OperationRegistry` via the prelude type.
/// 2. Register all 18 built-in ops via `harw::ops::builtins::register_all`.
/// 3. Assert the registry contains at least 18 operations.
/// 4. Verify each of the 18 canonical operation names is present by name.
/// 5. Verify alias reachability: `m` → model, `p` → provider, `reasoning` → effort.
/// 6. Build an `AssembledRegistry` via `harw::defaults::assemble_default_registry`.
/// 7. Parse a minimal agent TOML using `harw::agent::parse::parse_toml`.
/// 8. Assert the resulting `RawAgentDefinition` carries the expected `id.name`.
#[test]
fn sdk_composes_minimal_runnable_setup() -> TestResult {
    // ── Step 1: empty registry via prelude type ───────────────────────────────
    let mut registry = OperationRegistry::new();
    assert!(registry.is_empty(), "fresh registry must be empty");

    // ── Step 2: register all 18 built-in ops ─────────────────────────────────
    harw::ops::builtins::register_all(&mut registry);

    // ── Step 3: at least 18 ops registered ───────────────────────────────────
    assert!(
        registry.len() >= 18,
        "expected at least 18 ops after register_all, got {}",
        registry.len()
    );

    // ── Step 4: all 18 canonical names are reachable by name ─────────────────
    let canonical_names = [
        "help",
        "status",
        "quit",
        "new",
        "work",
        "ps",
        "attach",
        "stop",
        "diff",
        "agent",
        "skills",
        "plugins",
        "model",
        "provider",
        "permissions",
        "compact",
        "memory",
        "effort",
    ];
    for name in canonical_names {
        assert!(
            registry.find_by_name(name).is_some(),
            "canonical op '{name}' not found in registry after register_all",
        );
    }

    // ── Step 5: alias reachability ────────────────────────────────────────────
    // OperationRegistry exposes `iter()` + `meta().aliases`; no `find_by_alias`
    // method exists, so we implement the lookup here (proving API ergonomics).
    let find_by_alias = |alias: &str| -> Option<&str> {
        registry
            .iter()
            .find(|op| {
                let meta = op.meta();
                meta.aliases.contains(&alias)
            })
            .map(|op| op.meta().name)
    };

    assert_eq!(
        find_by_alias("m"),
        Some("model"),
        "alias 'm' must resolve to op 'model'"
    );
    assert_eq!(
        find_by_alias("p"),
        Some("provider"),
        "alias 'p' must resolve to op 'provider'"
    );
    assert_eq!(
        find_by_alias("reasoning"),
        Some("effort"),
        "alias 'reasoning' must resolve to op 'effort'"
    );

    // ── Step 6: AssembledRegistry via harw::defaults ──────────────────────────
    let assembled = harw::defaults::assemble_default_registry(std::env::temp_dir()).map_err(
        ctx("assemble_default_registry must succeed with temp_dir as cwd"),
    )?;
    // Confirm at least one tool provider is present (fs + shell = 5 tools).
    let total_tools: usize = assembled
        .registry
        .tool_providers()
        .iter()
        .map(|p| p.tools().len())
        .sum();
    assert!(
        total_tools > 0,
        "assembled registry must expose at least one tool, got {total_tools}"
    );

    // ── Step 7: parse minimal agent TOML via harw::agent::parse ──────────────
    let raw = harw::agent::parse::parse_toml(MINIMAL_AGENT_TOML)
        .map_err(ctx("minimal agent TOML must parse without error"))?;

    // ── Step 8: resulting RawAgentDefinition has the expected id.name ─────────
    assert_eq!(
        raw.id.name, "sdk-example",
        "parsed agent id.name must match the fixture"
    );
    assert_eq!(
        raw.specialization, "sdk-example",
        "parsed agent specialization must match the fixture"
    );
    Ok(())
}

/// SDK example: registry + compiled agent definition + executable runtime context.
///
/// Closes required E2E test list item #12: "HARW SDK example builds a minimal
/// registry, compiles an agent definition, and produces an executable runtime
/// context." Extends `sdk_composes_minimal_runnable_setup` (which stops after
/// parsing a `RawAgentDefinition`) by carrying the pipeline through resolution,
/// lowering to an `ExecutableAgentIr`, and constructing a live `AgentSession` —
/// all using only the public `harw::` facade, never a sub-crate directly.
///
/// Steps:
/// 1. Parse `MINIMAL_AGENT_TOML` via `harw::agent::parse::parse_toml`.
/// 2. Resolve it via `harw::agent::resolve::resolve_definition` with a single
///    `harw::agent::layers::DefinitionLayer::BuiltIn` layer.
/// 3. Lower the resolved definition via `harw::agent::lower` into an
///    `harw::agent::ExecutableAgentIr` and assert `ir.id().name` matches.
/// 4. Assemble a default registry via `harw::defaults::assemble_default_registry`.
/// 5. Construct an `harw::core::AgentSession` — the executable runtime context —
///    from the assembled registry, an unbounded `tokio::sync::mpsc` channel, and
///    `harw::types::AgentRole::Assistant`.
#[tokio::test]
async fn sdk_example_toml_to_executable_runtime_context() -> TestResult {
    // ── Step 1: parse the minimal agent TOML ──────────────────────────────────
    let raw = harw::agent::parse::parse_toml(MINIMAL_AGENT_TOML)
        .map_err(ctx("minimal agent TOML must parse without error"))?;

    // ── Step 2: resolve into a ResolvedAgentDefinition ────────────────────────
    let id = harw::agent::ids::DefinitionId::parse("harwness.agent.sdk-example@1")
        .map_err(ctx("DefinitionId::parse must succeed for the fixture id"))?;
    let layers = vec![(harw::agent::layers::DefinitionLayer::BuiltIn, raw)];
    let resolved =
        harw::agent::resolve::resolve_definition(&id, &layers, time::OffsetDateTime::now_utc())
            .map_err(ctx(
                "resolve_definition must succeed for the minimal fixture",
            ))?;

    // ── Step 3: lower into an ExecutableAgentIr ───────────────────────────────
    let ir: harw::agent::ExecutableAgentIr = harw::agent::lower(&resolved)
        .map_err(ctx("lower must succeed for the resolved definition"))?;
    assert_eq!(
        ir.id().name,
        "sdk-example",
        "lowered ExecutableAgentIr.id().name must match the fixture"
    );

    // ── Step 4: assemble a default registry ───────────────────────────────────
    let assembled = harw::defaults::assemble_default_registry(std::env::temp_dir()).map_err(
        ctx("assemble_default_registry must succeed with temp_dir as cwd"),
    )?;

    // ── Step 5: construct an executable runtime context (AgentSession) ───────
    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
    let session = harw::core::AgentSession::new(
        harw::types::AgentRole::Assistant,
        None,
        assembled.registry,
        event_tx,
    );

    assert_eq!(
        *session.role(),
        harw::types::AgentRole::Assistant,
        "constructed AgentSession must carry the Assistant role"
    );
    assert!(
        !session.id().as_str().is_empty(),
        "constructed AgentSession must have a non-empty generated SessionId"
    );
    Ok(())
}
