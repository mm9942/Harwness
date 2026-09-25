//! One test per diagnostic code of `harw_agent_dsl::diagnostics::CATALOG`.
//!
//! Each test feeds a minimal definition through `compile_agent` (parse →
//! resolve → lower_v2) and checks that exactly the expected code appears with
//! its catalog severity. Codes that only a resolver error can produce and
//! that `compile_agent` cannot reach from TOML (invalid ID string, invalid
//! semver, I/O) are checked through `Diagnostic::from_dsl_error`.
//! `test_every_catalog_code_has_a_test` keeps the list below in sync with the
//! catalog.

mod common;

use common::{TestError, TestResult, ctx};
use harw_agent_dsl::bind::ContextProgramLibrary;
use harw_agent_dsl::diagnostics::{CATALOG, Diagnostic, Diagnostics, Severity, SourceFile};
use harw_agent_dsl::error::DslError;
use harw_agent_dsl::ids::DefinitionId;
use harw_agent_dsl::layers::DefinitionLayer;
use harw_agent_dsl::lower_v2::{LowerSources, MapInstructionsLoader, compile_agent};
use time::OffsetDateTime;

/// Every code with a test in this file.
const TESTED_CODES: &[&str] = &[
    "HARW-PARSE-001",
    "HARW-PARSE-002",
    "HARW-PARSE-003",
    "HARW-PARSE-004",
    "HARW-SCHEMA-001",
    "HARW-SCHEMA-002",
    "HARW-SCHEMA-003",
    "HARW-SCHEMA-004",
    "HARW-SCHEMA-005",
    "HARW-SCHEMA-006",
    "HARW-SCHEMA-007",
    "HARW-RESOLVE-001",
    "HARW-RESOLVE-002",
    "HARW-RESOLVE-003",
    "HARW-RESOLVE-004",
    "HARW-RESOLVE-005",
    "HARW-RESOLVE-006",
    "HARW-RESOLVE-007",
    "HARW-PATCH-001",
    "HARW-PATCH-002",
    "HARW-PATCH-003",
    "HARW-PATCH-004",
    "HARW-PATCH-005",
    "HARW-AUTH-001",
    "HARW-AUTH-002",
    "HARW-ROLE-001",
    "HARW-ROLE-002",
    "HARW-TOOL-001",
    "HARW-TOOL-002",
    "HARW-TOOL-003",
    "HARW-CTX-001",
    "HARW-CTX-002",
    "HARW-CTX-003",
    "HARW-RETURN-001",
    "HARW-RETURN-002",
    "HARW-MODEL-001",
    "HARW-MODEL-002",
    "HARW-MODEL-003",
    "HARW-SKILL-001",
    "HARW-BINARY-001",
    "HARW-BINARY-002",
    "HARW-BINARY-003",
    "HARW-BINARY-004",
    "HARW-BINARY-005",
];

const TARGET: &str = "acme.agent.t@1";
const TARGET_PATH: &str = "agents/t/definition.toml";

/// A minimal agent: `header` holds extra top-level keys, `body` tables.
fn agent(header: &str, body: &str) -> String {
    format!(
        "schema = \"harwness.agent/v1\"\nid = \"{TARGET}\"\nversion = \"1.0.0\"\nrole = \"worker\"\nspecialization = \"t\"\n{header}\n{body}\n"
    )
}

/// A base definition `acme.agent.base@1` with the given tables.
fn base(body: &str) -> String {
    format!(
        "schema = \"harwness.agent/v1\"\nid = \"acme.agent.base@1\"\nversion = \"1.0.0\"\nrole = \"worker\"\nspecialization = \"base\"\n\n{body}\n"
    )
}

fn target_file(text: &str) -> SourceFile {
    SourceFile::new(DefinitionLayer::UserGlobal, TARGET_PATH, text)
}

fn base_file(text: &str) -> SourceFile {
    SourceFile::new(DefinitionLayer::BuiltIn, "builtin/base.toml", text)
}

/// Runs `compile_agent`; returns the error diagnostics, or the trace
/// diagnostics (warnings/notes) of a successful lowering.
fn run_with(
    files: &[SourceFile],
    library: Option<&ContextProgramLibrary>,
    loader: Option<&MapInstructionsLoader>,
) -> TestResult<Diagnostics> {
    let target = DefinitionId::parse(TARGET).map_err(ctx("target id"))?;
    let mut sources = LowerSources::new(files);
    if let Some(library) = library {
        sources = sources.with_context_programs(library);
    }
    if let Some(loader) = loader {
        sources = sources.with_instructions(loader);
    }
    Ok(
        match compile_agent(&target, &sources, OffsetDateTime::UNIX_EPOCH) {
            Ok(ir) => Diagnostics::from(ir.trace.diagnostics),
            Err(diagnostics) => diagnostics,
        },
    )
}

fn run(files: &[SourceFile]) -> TestResult<Diagnostics> {
    run_with(files, None, None)
}

/// Asserts that `code` is present with its catalog severity; returns it.
fn expect(diagnostics: &Diagnostics, code: &str) -> TestResult<Diagnostic> {
    let entry = CATALOG
        .iter()
        .find(|entry| entry.code == code)
        .ok_or_else(|| TestError::Unexpected(format!("{code} is not in the catalog")))?;
    let found = diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == code)
        .ok_or_else(|| TestError::Unexpected(format!("{code} expected, got:\n{diagnostics}")))?;
    assert_eq!(found.severity, entry.severity, "{code}");
    assert!(found.help.is_some(), "{code} carries a help text");
    Ok(found.clone())
}

fn expect_one(files: &[SourceFile], code: &str) -> TestResult<Diagnostic> {
    expect(&run(files)?, code)
}

#[test]
fn test_every_catalog_code_has_a_test() {
    for entry in CATALOG {
        assert!(
            TESTED_CODES.contains(&entry.code),
            "{} has no test in diagnostic_codes.rs",
            entry.code
        );
    }
    assert_eq!(TESTED_CODES.len(), CATALOG.len());
}

// ── PARSE ────────────────────────────────────────────────────────────────

#[test]
fn test_parse_001_syntax_error_with_span() -> TestResult {
    let diagnostic = expect_one(&[target_file("schema = [unclosed")], "HARW-PARSE-001")?;
    assert_eq!(
        diagnostic.span.map(|span| span.file),
        Some(TARGET_PATH.to_owned())
    );
    Ok(())
}

#[test]
fn test_parse_002_missing_required_field() -> TestResult {
    let text = "schema = \"harwness.agent/v1\"\nid = \"acme.agent.t@1\"\nversion = \"1.0.0\"\nspecialization = \"t\"\n";
    expect_one(&[target_file(text)], "HARW-PARSE-002")?;
    Ok(())
}

#[test]
fn test_parse_003_wrong_type() -> TestResult {
    let diagnostic = expect_one(
        &[target_file(&agent("", "[spawn]\nmax_depth = \"one\""))],
        "HARW-PARSE-003",
    )?;
    assert_eq!(diagnostic.path.as_deref(), Some("spawn.max_depth"));
    assert!(diagnostic.span.is_some());
    Ok(())
}

#[test]
fn test_parse_004_out_of_range() -> TestResult {
    let diagnostic = expect_one(
        &[target_file(&agent("", "[spawn]\nmax_depth = -1"))],
        "HARW-PARSE-004",
    )?;
    assert_eq!(diagnostic.path.as_deref(), Some("spawn.max_depth"));
    Ok(())
}

// ── SCHEMA ───────────────────────────────────────────────────────────────

#[test]
fn test_schema_001_schema_mismatch_with_line() -> TestResult {
    let text = agent("", "").replace("harwness.agent/v1", "harwness.agent/v9");
    let diagnostic = expect_one(&[target_file(&text)], "HARW-SCHEMA-001")?;
    assert_eq!(diagnostic.path.as_deref(), Some("schema"));
    assert_eq!(diagnostic.span.map(|span| span.line), Some(1));
    Ok(())
}

#[test]
fn test_schema_002_unknown_table_with_suggestion() -> TestResult {
    let diagnostic = expect_one(
        &[target_file(&agent("", "[tols]\nadmitted = []"))],
        "HARW-SCHEMA-002",
    )?;
    assert!(
        diagnostic
            .help
            .as_deref()
            .is_some_and(|help| help.contains("did you mean `[tools]`")),
        "{diagnostic}"
    );
    Ok(())
}

#[test]
fn test_schema_003_forward_compatible_table_is_a_warning() -> TestResult {
    expect_one(
        &[target_file(&agent(
            "",
            "[compatibility]\nmin_harwness = \"0.1.0\"",
        ))],
        "HARW-SCHEMA-003",
    )?;
    Ok(())
}

#[test]
fn test_schema_004_unknown_key() -> TestResult {
    let diagnostic = expect_one(
        &[target_file(&agent("", "[tools]\nadmited = []"))],
        "HARW-SCHEMA-004",
    )?;
    assert_eq!(diagnostic.path.as_deref(), Some("tools.admited"));
    Ok(())
}

#[test]
fn test_schema_005_alias_conflict() -> TestResult {
    expect_one(
        &[target_file(&agent(
            "",
            "[spawn]\nmax_depth = 1\n\n[spawn_contract]\nmax_depth = 1",
        ))],
        "HARW-SCHEMA-005",
    )?;
    Ok(())
}

#[test]
fn test_schema_006_empty_specialization() -> TestResult {
    let text = agent("", "").replace("specialization = \"t\"", "specialization = \"\"");
    expect_one(&[target_file(&text)], "HARW-SCHEMA-006")?;
    Ok(())
}

#[test]
fn test_schema_007_instructions_file_missing() -> TestResult {
    let loader = MapInstructionsLoader::new();
    let diagnostics = run_with(
        &[target_file(&agent(
            "instructions_file = \"missing.md\"",
            "",
        ))],
        None,
        Some(&loader),
    )?;
    expect(&diagnostics, "HARW-SCHEMA-007")?;
    Ok(())
}

// ── RESOLVE ──────────────────────────────────────────────────────────────

#[test]
fn test_resolve_001_missing_base() -> TestResult {
    expect_one(
        &[target_file(&agent(
            "extends = { id = \"acme.agent.nope@1\" }",
            "",
        ))],
        "HARW-RESOLVE-001",
    )?;
    Ok(())
}

#[test]
fn test_resolve_002_missing_mixin() -> TestResult {
    expect_one(
        &[target_file(&agent(
            "mixins = [{ id = \"acme.mixin.nope@1\" }]",
            "",
        ))],
        "HARW-RESOLVE-002",
    )?;
    Ok(())
}

#[test]
fn test_resolve_003_cycle() -> TestResult {
    expect_one(
        &[target_file(&agent(
            &format!("extends = {{ id = \"{TARGET}\" }}"),
            "",
        ))],
        "HARW-RESOLVE-003",
    )?;
    Ok(())
}

#[test]
fn test_resolve_004_shadowed_table_is_a_warning() -> TestResult {
    let diagnostic = expect_one(
        &[
            base_file(&base("[work]\nmode = \"base-mode\"")),
            target_file(&agent(
                "extends = { id = \"acme.agent.base@1\" }",
                "[work]\nmode = \"own-mode\"",
            )),
        ],
        "HARW-RESOLVE-004",
    )?;
    assert_eq!(diagnostic.path.as_deref(), Some("work"));
    Ok(())
}

#[test]
fn test_resolve_005_invalid_id() -> TestResult {
    let Err(error) = DefinitionId::parse("not-an-id") else {
        return Err(TestError::Unexpected("invalid id must fail".to_owned()));
    };
    assert_eq!(Diagnostic::from_dsl_error(&error).code, "HARW-RESOLVE-005");
    Ok(())
}

#[test]
fn test_resolve_006_invalid_version() {
    let error = DslError::Semver("unexpected character".to_owned());
    assert_eq!(Diagnostic::from_dsl_error(&error).code, "HARW-RESOLVE-006");
}

#[test]
fn test_resolve_007_io() {
    let error = DslError::Io {
        path: "agents/t/definition.toml".into(),
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "gone"),
    };
    assert_eq!(Diagnostic::from_dsl_error(&error).code, "HARW-RESOLVE-007");
}

// ── PATCH ────────────────────────────────────────────────────────────────

#[test]
fn test_patch_001_unknown_operator_or_malformed_value() -> TestResult {
    expect_one(
        &[target_file(&agent("", "[patch]\ntools = 3"))],
        "HARW-PATCH-001",
    )?;
    Ok(())
}

#[test]
fn test_patch_002_missing_path() -> TestResult {
    let diagnostic = expect_one(
        &[target_file(&agent("", "[patch.nothere]\nappend = [\"x\"]"))],
        "HARW-PATCH-002",
    )?;
    assert_eq!(diagnostic.path.as_deref(), Some("patch.nothere"));
    assert!(diagnostic.span.is_some());
    Ok(())
}

#[test]
fn test_patch_003_type_mismatch() -> TestResult {
    expect_one(
        &[target_file(&agent("", "[patch.skills]\nappend = \"x\""))],
        "HARW-PATCH-003",
    )?;
    Ok(())
}

#[test]
fn test_patch_004_max_within_parent_exceeds_parent() -> TestResult {
    let diagnostic = expect_one(
        &[
            base_file(&base("[limits]\nmax_tool_calls = 40")),
            target_file(&agent(
                "extends = { id = \"acme.agent.base@1\" }",
                "[patch.limits]\nmax_tool_calls = { max-within-parent = 41 }",
            )),
        ],
        "HARW-PATCH-004",
    )?;
    assert_eq!(
        diagnostic.path.as_deref(),
        Some("patch.limits.max_tool_calls")
    );
    assert!(diagnostic.span.is_some());
    Ok(())
}

#[test]
fn test_patch_005_operators_mixed_with_nested_keys() -> TestResult {
    expect_one(
        &[target_file(&agent(
            "",
            "[patch.x]\nreplace = 1\ny = { append = [] }",
        ))],
        "HARW-PATCH-005",
    )?;
    Ok(())
}

// ── AUTH ─────────────────────────────────────────────────────────────────

#[test]
fn test_auth_001_elevation() -> TestResult {
    expect_one(
        &[target_file(&agent(
            "",
            "[authority]\ncapabilities = [\"filesystem.read\"]\n\n[patch.authority.capabilities]\nappend = [\"agent.spawn.child-orchestrator\"]",
        ))],
        "HARW-AUTH-001",
    )?;
    Ok(())
}

#[test]
fn test_auth_002_forbidden_operator() -> TestResult {
    expect_one(
        &[target_file(&agent(
            "",
            "[authority]\ncapabilities = [\"filesystem.read\"]\n\n[patch.authority.capabilities]\nreplace = [\"filesystem.read\"]",
        ))],
        "HARW-AUTH-002",
    )?;
    Ok(())
}

// ── ROLE ─────────────────────────────────────────────────────────────────

#[test]
fn test_role_001_mixin_role() -> TestResult {
    let mixin = "schema = \"harwness.mixin/v1\"\nid = \"acme.mixin.m@1\"\nversion = \"1.0.0\"\nrole = \"root-orchestrator\"\nspecialization = \"m\"\n";
    expect_one(
        &[
            SourceFile::new(DefinitionLayer::BuiltIn, "builtin/m.toml", mixin),
            target_file(&agent("mixins = [{ id = \"acme.mixin.m@1\" }]", "")),
        ],
        "HARW-ROLE-001",
    )?;
    Ok(())
}

#[test]
fn test_role_002_worker_lists_child_orchestrators() -> TestResult {
    expect_one(
        &[target_file(&agent(
            "",
            "[spawn]\nchild_orchestrators = [\"coding-orchestrator\"]",
        ))],
        "HARW-ROLE-002",
    )?;
    Ok(())
}

// ── TOOL ─────────────────────────────────────────────────────────────────

#[test]
fn test_tool_001_admitted_and_forbidden() -> TestResult {
    expect_one(
        &[target_file(&agent(
            "",
            "[tools]\nadmitted = [\"fs.read\"]\nforbidden = [\"fs.read\"]",
        ))],
        "HARW-TOOL-001",
    )?;
    Ok(())
}

#[test]
fn test_tool_002_duplicate_is_a_warning() -> TestResult {
    expect_one(
        &[target_file(&agent(
            "",
            "[tools]\nadmitted = [\"fs.read\", \"fs.read\"]",
        ))],
        "HARW-TOOL-002",
    )?;
    Ok(())
}

#[test]
fn test_tool_003_invalid_name() -> TestResult {
    expect_one(
        &[target_file(&agent("", "[tools]\nadmitted = [\"fs read\"]"))],
        "HARW-TOOL-003",
    )?;
    Ok(())
}

// ── CTX ──────────────────────────────────────────────────────────────────

const PROGRAM: &str = "schema = \"harwness.context/v1\"\nid = \"harwness.context.p@1\"\nversion = \"1.0.0\"\n\n[[sections]]\nname = \"plan.current\"\nstrength = \"must-include\"\n";

#[test]
fn test_ctx_001_unknown_program() -> TestResult {
    let library =
        ContextProgramLibrary::from_builtin_sources(&[("p", PROGRAM)]).map_err(ctx("library"))?;
    let diagnostics = run_with(
        &[target_file(&agent("", "[context]\nprogram = \"nope\""))],
        Some(&library),
        None,
    )?;
    let diagnostic = expect(&diagnostics, "HARW-CTX-001")?;
    assert!(diagnostic.message.contains("nope"));
    Ok(())
}

#[test]
fn test_ctx_002_program_does_not_resolve() -> TestResult {
    let broken = PROGRAM.replace(
        "version = \"1.0.0\"\n",
        "version = \"1.0.0\"\nextends = { id = \"harwness.context.missing@1\" }\n",
    );
    let library = ContextProgramLibrary::from_builtin_sources(&[("p", broken.as_str())])
        .map_err(ctx("library"))?;
    let diagnostics = run_with(
        &[target_file(&agent("", "[context]\nprogram = \"p\""))],
        Some(&library),
        None,
    )?;
    expect(&diagnostics, "HARW-CTX-002")?;
    Ok(())
}

#[test]
fn test_ctx_003_deferred_sections_are_a_note() -> TestResult {
    let library =
        ContextProgramLibrary::from_builtin_sources(&[("p", PROGRAM)]).map_err(ctx("library"))?;
    let diagnostics = run_with(
        &[target_file(&agent("", "[context]\nprogram = \"p\""))],
        Some(&library),
        None,
    )?;
    let diagnostic = expect(&diagnostics, "HARW-CTX-003")?;
    assert_eq!(diagnostic.severity, Severity::Note);
    assert!(diagnostic.message.contains("plan.current"));
    Ok(())
}

// ── RETURN ───────────────────────────────────────────────────────────────

#[test]
fn test_return_001_unknown_contract() -> TestResult {
    let diagnostic = expect_one(
        &[target_file(&agent(
            "",
            "[return]\ncontract = \"harwness.return.nope@1\"",
        ))],
        "HARW-RETURN-001",
    )?;
    assert_eq!(diagnostic.path.as_deref(), Some("return.contract"));
    Ok(())
}

#[test]
fn test_return_002_invalid_validator() -> TestResult {
    expect_one(
        &[target_file(&agent("", "[return]\nvalidators = [\"\"]"))],
        "HARW-RETURN-002",
    )?;
    Ok(())
}

// ── MODEL ────────────────────────────────────────────────────────────────

#[test]
fn test_model_001_unknown_effort() -> TestResult {
    expect_one(
        &[target_file(&agent("", "[models]\neffort = \"ultra\""))],
        "HARW-MODEL-001",
    )?;
    Ok(())
}

#[test]
fn test_model_002_invalid_env_name() -> TestResult {
    expect_one(
        &[target_file(&agent(
            "",
            "[models]\nrequired_env = [\"api-key\"]",
        ))],
        "HARW-MODEL-002",
    )?;
    Ok(())
}

#[test]
fn test_model_003_invalid_fallback() -> TestResult {
    expect_one(
        &[target_file(&agent(
            "",
            "[models]\nfallbacks = [\"nomodel\"]",
        ))],
        "HARW-MODEL-003",
    )?;
    Ok(())
}

// ── SKILL ────────────────────────────────────────────────────────────────

#[test]
fn test_skill_001_invalid_skill() -> TestResult {
    let diagnostic = expect_one(
        &[target_file(&agent("skills = [\"ok\", \"Not_Ok\"]", ""))],
        "HARW-SKILL-001",
    )?;
    assert_eq!(diagnostic.path.as_deref(), Some("skills[1]"));
    assert!(diagnostic.span.is_some());
    Ok(())
}

// ── BINARY ───────────────────────────────────────────────────────────────

#[test]
fn test_binary_001_unknown_interface() -> TestResult {
    expect_one(
        &[target_file(&agent("", "[binary]\ninterfaces = [\"grpc\"]"))],
        "HARW-BINARY-001",
    )?;
    Ok(())
}

#[test]
fn test_binary_002_default_not_listed() -> TestResult {
    expect_one(
        &[target_file(&agent(
            "",
            "[binary]\ninterfaces = [\"cli\"]\ndefault_interface = \"mcp\"",
        ))],
        "HARW-BINARY-002",
    )?;
    Ok(())
}

#[test]
fn test_binary_003_empty_interfaces() -> TestResult {
    expect_one(
        &[target_file(&agent("", "[binary]\ninterfaces = []"))],
        "HARW-BINARY-003",
    )?;
    Ok(())
}

#[test]
fn test_binary_004_invalid_name() -> TestResult {
    expect_one(
        &[target_file(&agent("", "[binary]\nname = \"Bad Name\""))],
        "HARW-BINARY-004",
    )?;
    Ok(())
}

#[test]
fn test_binary_005_duplicate_interface_is_a_warning() -> TestResult {
    expect_one(
        &[target_file(&agent(
            "",
            "[binary]\ninterfaces = [\"cli\", \"cli\"]",
        ))],
        "HARW-BINARY-005",
    )?;
    Ok(())
}
