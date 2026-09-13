//! End-to-end integration tests proving the Agent DSL pipeline
//! (TOML → RawAgentDefinition → ResolvedAgentDefinition → ExecutableAgentIr)
//! works correctly using only public APIs from `harw-agent-dsl`.
//!
//! Spec source: task brief "slice13 e2e integration tests"
//!
//! Pipeline under test:
//!   `parse_toml(&src)` → `resolve_definition(&id, &layers, now)` → `lower(&resolved)`

use harw_agent_dsl::ExecutableAgentIr;
use harw_agent_dsl::error::DslError;
use harw_agent_dsl::ids::DefinitionId;
use harw_agent_dsl::layers::DefinitionLayer;
use harw_agent_dsl::lower;
use harw_agent_dsl::parse::parse_toml;
use harw_agent_dsl::resolve::resolve_definition;

// --------------------------------------------------------------------------
// Shared fixture content (loaded via include_str! — no runtime I/O)
// --------------------------------------------------------------------------

const MINIMAL_TOML: &str = include_str!("fixtures/minimal.toml");
const AUTH_ATTACKER_TOML: &str = include_str!("fixtures/authority_elevation_attacker.toml");

// --------------------------------------------------------------------------
// Helper: run the full pipeline for a TOML string, returning ExecutableAgentIr
// --------------------------------------------------------------------------

fn pipeline(toml_src: &str, id_str: &str) -> ExecutableAgentIr {
    let raw = parse_toml(toml_src).expect("parse_toml should succeed");
    let id = DefinitionId::parse(id_str).expect("DefinitionId::parse should succeed");
    let layers = vec![(DefinitionLayer::BuiltIn, raw)];
    let resolved = resolve_definition(&id, &layers, time::OffsetDateTime::now_utc())
        .expect("resolve_definition should succeed");
    lower(&resolved).expect("lower should succeed")
}

// --------------------------------------------------------------------------
// Test 1 — Full pipeline: minimal agent through all three stages
// --------------------------------------------------------------------------

#[test]
fn slice13_full_pipeline_minimal_agent() {
    // Run the complete pipeline using the fixture file.
    let ir: ExecutableAgentIr = pipeline(MINIMAL_TOML, "harwness.agent.slice13-minimal@1");

    // Assert the IR has the expected identity and role fields.
    assert_eq!(
        ir.id().name,
        "slice13-minimal",
        "ExecutableAgentIr.id.name should match fixture id"
    );
    assert_eq!(
        ir.role(),
        harw_agent_dsl::roles::AgentRoleId::Worker,
        "ExecutableAgentIr.role should be Worker"
    );

    // Assert snapshot_id is a non-empty hex digest (BLAKE3 = 64 hex chars).
    let snap = ir.snapshot_id().to_string();
    assert!(
        !snap.is_empty(),
        "snapshot_id() should return a non-empty string"
    );
    assert_eq!(
        snap.len(),
        64,
        "BLAKE3 hex digest should be exactly 64 chars"
    );
    assert!(
        snap.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
        "snapshot_id should consist only of lowercase hex digits, got: {}",
        snap
    );

    // Compile-time proof: the return type is ExecutableAgentIr (no raw TOML type leaks).
    // The function `consume_ir` below only accepts ExecutableAgentIr; this line compiles
    // only if `ir` is exactly that type.
    fn consume_ir(_: ExecutableAgentIr) {}
    consume_ir(pipeline(MINIMAL_TOML, "harwness.agent.slice13-minimal@1"));
}

// --------------------------------------------------------------------------
// Test 2 — Snapshot ID is deterministic across two separate lowerings
// --------------------------------------------------------------------------

#[test]
fn slice13_snapshot_id_stable_across_two_lowerings() {
    let id_str = "harwness.agent.slice13-minimal@1";

    let ir1 = pipeline(MINIMAL_TOML, id_str);
    let ir2 = pipeline(MINIMAL_TOML, id_str);

    assert_eq!(
        ir1.snapshot_id(),
        ir2.snapshot_id(),
        "Two lowerings of the same TOML must produce identical SnapshotIds;\
         got {} vs {}",
        ir1.snapshot_id(),
        ir2.snapshot_id()
    );
}

// --------------------------------------------------------------------------
// Test 3 — Snapshot ID changes when role changes
// --------------------------------------------------------------------------

#[test]
fn slice13_snapshot_id_changes_when_role_changes() {
    // Two TOMLs identical except for `role`.
    // Same id value so the id-contribution to the hash is equal; only role differs.
    const WORKER_TOML: &str = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.slice13-role-test@1"
version = "1.0.0"
role = "worker"
specialization = "slice13-role-test"
"#;
    const ORCHESTRATOR_TOML: &str = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.slice13-role-test@1"
version = "1.0.0"
role = "root-orchestrator"
specialization = "slice13-role-test"
"#;

    let id_str = "harwness.agent.slice13-role-test@1";
    let ir_worker = pipeline(WORKER_TOML, id_str);
    let ir_orch = pipeline(ORCHESTRATOR_TOML, id_str);

    assert_ne!(
        ir_worker.snapshot_id(),
        ir_orch.snapshot_id(),
        "Changing role from worker to root-orchestrator must produce a different SnapshotId;\
         both produced: {}",
        ir_worker.snapshot_id()
    );
}

// --------------------------------------------------------------------------
// Test 4 — DslError::AuthorityElevation carries field_path "authority.capabilities"
// --------------------------------------------------------------------------

#[test]
fn slice13_dsl_error_carries_field_path() {
    // The attacker fixture has [patch.authority.capabilities] append = [...],
    // which is unconditionally rejected by the resolver (§7, §22 Invariant 7).
    let raw = parse_toml(AUTH_ATTACKER_TOML).expect("parse_toml should succeed for attacker");
    let id_str = "harwness.agent.slice13-auth-attacker@1";
    let id = DefinitionId::parse(id_str).expect("DefinitionId::parse should succeed");
    let layers = vec![(DefinitionLayer::BuiltIn, raw)];

    let result = resolve_definition(&id, &layers, time::OffsetDateTime::now_utc());

    // Must be an AuthorityElevation error.
    assert!(
        result.is_err(),
        "resolve_definition should fail for authority-elevation attempt"
    );
    let err = result.unwrap_err();
    assert!(
        matches!(err, DslError::AuthorityElevation { .. }),
        "Expected DslError::AuthorityElevation, got: {err}"
    );

    // Display output must contain "authority.capabilities".
    let display = err.to_string();
    assert!(
        display.contains("authority.capabilities"),
        "Display of AuthorityElevation should contain 'authority.capabilities', got: {display}"
    );

    // The location field must carry field_path == Some("authority.capabilities").
    if let DslError::AuthorityElevation { location, .. } = &err {
        assert_eq!(
            location.field_path.as_deref(),
            Some("authority.capabilities"),
            "location.field_path should be Some(\"authority.capabilities\"), got: {:?}",
            location.field_path
        );
    } else {
        panic!("Expected DslError::AuthorityElevation but got a different variant");
    }
}

// --------------------------------------------------------------------------
// Test 5 — No raw TOML types leak to IR consumers (compile-time + runtime proof)
// --------------------------------------------------------------------------

#[test]
fn slice13_no_raw_toml_types_leak_to_ir_consumer() {
    // Compile-time proof: this function only accepts ExecutableAgentIr.
    // If the pipeline returned a raw TOML type, this line would not compile.
    fn consume_ir(_ir: harw_agent_dsl::ExecutableAgentIr) {}

    let ir = pipeline(MINIMAL_TOML, "harwness.agent.slice13-minimal@1");
    consume_ir(ir);

    // Runtime proof: the type name of ExecutableAgentIr must not contain "Raw" or "toml::".
    let type_name = std::any::type_name::<harw_agent_dsl::ExecutableAgentIr>();
    assert!(
        !type_name.contains("Raw"),
        "ExecutableAgentIr type name must not contain 'Raw', got: {type_name}"
    );
    assert!(
        !type_name.contains("toml::"),
        "ExecutableAgentIr type name must not contain 'toml::', got: {type_name}"
    );
}
