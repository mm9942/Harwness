//! Golden JSON of `AgentIr` (IR v2, #22 wave 1) for every built-in role, every
//! built-in base layer and every bundled definition under
//! `harw-home/assets/agents/*/definition.toml`, plus the check that the legacy
//! view `ExecutableAgentIr::from(&ir)` equals `builtin_agent_definitions` for
//! every built-in role.
//!
//! # Goldens
//! The goldens live in `tests/golden_ir/<kind>-<name>.json` (pretty JSON with
//! a trailing newline; resolution timestamps are the Unix epoch, file labels
//! are repository-relative, so the files are machine independent). A test
//! run compares; a run with `HARW_BLESS=1` rewrites every golden and removes
//! stale ones:
//!
//! ```text
//! HARW_BLESS=1 cargo test -p harw-registry-defaults --test golden_ir
//! ```
//!
//! Review the diff before committing: a golden change is a change of the
//! compiled agent (and of its v7 snapshot hash).

mod common;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;

use common::{TestError, TestResult, ctx};
use harw_agent_dsl::diagnostics::SourceFile;
use harw_agent_dsl::layers::DefinitionLayer;
use harw_agent_dsl::lower_v2::{LowerSources, MapInstructionsLoader, compile_agent};
use harw_agent_dsl::parse::parse_toml;
use harw_agent_dsl::{AgentIr, ExecutableAgentIr};
use harw_registry_defaults::embedded_agents::{
    builtin_agent_definitions, builtin_agent_irs, builtin_base_definitions, builtin_base_irs,
    builtin_context_program_library, builtin_source_files,
};
use time::OffsetDateTime;

/// Environment variable that rewrites the goldens.
const BLESS_ENV: &str = "HARW_BLESS";

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden_ir")
}

fn blessing() -> bool {
    std::env::var(BLESS_ENV).is_ok_and(|value| value == "1")
}

/// Relative directory of the bundled definitions (from the workspace root).
const ASSETS_REL: &str = "harw-home/assets/agents";

fn assets_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(ASSETS_REL)
}

/// Lowers every bundled `definition.toml` over the built-in layers.
///
/// # Description
/// All bundled definitions form one `UserGlobal` layer (as after
/// `harw home install`), so they may extend each other; their `system.md`
/// files are served from memory under repository-relative labels.
fn bundled_irs() -> TestResult<BTreeMap<String, AgentIr>> {
    let mut files = builtin_source_files();
    let mut loader = MapInstructionsLoader::new();
    let mut targets = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(assets_dir())
        .map_err(ctx("harw-home/assets/agents is readable"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.join("definition.toml").is_file())
        .collect();
    entries.sort();
    for dir in entries {
        let name = dir
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(TestError::Missing("asset directory name"))?
            .to_owned();
        let text = std::fs::read_to_string(dir.join("definition.toml"))
            .map_err(ctx("definition.toml is readable"))?;
        let label = format!("{ASSETS_REL}/{name}/definition.toml");
        for entry in std::fs::read_dir(dir.as_path()).map_err(ctx("asset directory is readable"))? {
            let path = entry.map_err(ctx("asset entry"))?.path();
            if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
                let file = path
                    .file_name()
                    .and_then(|file| file.to_str())
                    .ok_or(TestError::Missing("asset file name"))?
                    .to_owned();
                let content =
                    std::fs::read_to_string(path.as_path()).map_err(ctx("asset markdown"))?;
                loader.insert(format!("{ASSETS_REL}/{name}/{file}"), content);
            }
        }
        let raw = parse_toml(&text).map_err(ctx("bundled definition parses"))?;
        targets.push((name, raw.id));
        files.push(SourceFile::new(DefinitionLayer::UserGlobal, label, text));
    }
    let library = builtin_context_program_library().map_err(ctx("program library"))?;
    let sources = LowerSources::new(&files)
        .with_instructions(&loader)
        .with_context_programs(&library);
    let mut irs = BTreeMap::new();
    for (name, id) in targets {
        let ir =
            compile_agent(&id, &sources, OffsetDateTime::UNIX_EPOCH).map_err(|diagnostics| {
                TestError::Unexpected(format!("{name} does not lower:\n{diagnostics}"))
            })?;
        irs.insert(name, ir);
    }
    Ok(irs)
}

/// Every IR to pin, keyed by golden file stem.
fn all_irs() -> TestResult<BTreeMap<String, AgentIr>> {
    let mut all = BTreeMap::new();
    for (name, ir) in
        builtin_agent_irs(OffsetDateTime::UNIX_EPOCH).map_err(ctx("built-in roles"))?
    {
        all.insert(format!("builtin-{name}"), ir);
    }
    for (name, ir) in builtin_base_irs(OffsetDateTime::UNIX_EPOCH).map_err(ctx("built-in bases"))? {
        all.insert(format!("base-{name}"), ir);
    }
    for (name, ir) in bundled_irs()? {
        all.insert(format!("asset-{name}"), ir);
    }
    Ok(all)
}

#[test]
fn test_golden_agent_ir_for_every_builtin_and_bundled_definition() -> TestResult {
    let irs = all_irs()?;
    let dir = golden_dir();
    let bless = blessing();
    if bless {
        std::fs::create_dir_all(dir.as_path()).map_err(ctx("golden dir"))?;
    }
    let mut problems = Vec::new();
    for (stem, ir) in &irs {
        assert!(ir.verify_snapshot(), "{stem}: snapshot must verify");
        let path = dir.join(format!("{stem}.json"));
        let mut actual = serde_json::to_string_pretty(ir).map_err(ctx("serialize"))?;
        actual.push('\n');
        if bless {
            std::fs::write(path.as_path(), actual).map_err(ctx("write golden"))?;
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(expected) if expected == actual => {}
            Ok(_) => problems.push(format!("{} differs", path.display())),
            Err(error) => problems.push(format!("{} is missing ({error})", path.display())),
        }
    }
    // Stale goldens: files without a definition.
    let expected: BTreeSet<String> = irs.keys().map(|stem| format!("{stem}.json")).collect();
    if let Ok(entries) = std::fs::read_dir(dir.as_path()) {
        for entry in entries.filter_map(Result::ok) {
            let file = entry.file_name().to_string_lossy().into_owned();
            if file.ends_with(".json") && !expected.contains(&file) {
                if bless {
                    std::fs::remove_file(entry.path()).map_err(ctx("remove stale golden"))?;
                } else {
                    problems.push(format!("{file} is stale"));
                }
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(TestError::Unexpected(format!(
            "{} golden problem(s); run `{BLESS_ENV}=1 cargo test -p harw-registry-defaults --test golden_ir` and review the diff:\n{}",
            problems.len(),
            problems.join("\n")
        )))
    }
}

#[test]
fn test_golden_agent_ir_roundtrips_through_serde() -> TestResult {
    for (stem, ir) in all_irs()? {
        let json = serde_json::to_string(&ir).map_err(ctx("serialize"))?;
        let back: AgentIr = serde_json::from_str(&json).map_err(ctx("deserialize"))?;
        assert_eq!(back, ir, "{stem}");
    }
    Ok(())
}

/// Compares the legacy view of `ir` with `legacy` on every shared field.
fn assert_view_equals(name: &str, ir: &AgentIr, legacy: &ExecutableAgentIr) {
    let view = ExecutableAgentIr::from(ir);
    assert_eq!(
        view.snapshot_id(),
        legacy.snapshot_id(),
        "{name}: v6 snapshot"
    );
    assert_eq!(view.id(), legacy.id(), "{name}");
    assert_eq!(view.role(), legacy.role(), "{name}");
    assert_eq!(view.specialization(), legacy.specialization(), "{name}");
    assert_eq!(
        view.authority().capabilities,
        legacy.authority().capabilities,
        "{name}"
    );
    assert_eq!(view.reasoning_effort(), legacy.reasoning_effort(), "{name}");
    assert_eq!(view.skills(), legacy.skills(), "{name}");
    let (spawn, old_spawn) = (view.spawn_contract(), legacy.spawn_contract());
    assert_eq!(spawn.workspace_hint(), old_spawn.workspace_hint(), "{name}");
    assert_eq!(spawn.max_depth(), old_spawn.max_depth(), "{name}");
    assert_eq!(
        spawn.child_orchestrators(),
        old_spawn.child_orchestrators(),
        "{name}"
    );
    assert_eq!(
        spawn.budget().map(|b| (
            b.max_tokens(),
            b.max_tool_calls(),
            b.max_wall_secs(),
            b.effort_cap().map(str::to_owned)
        )),
        old_spawn.budget().map(|b| (
            b.max_tokens(),
            b.max_tool_calls(),
            b.max_wall_secs(),
            b.effort_cap().map(str::to_owned)
        )),
        "{name}"
    );
    assert_eq!(
        view.job_template().goal_kind(),
        legacy.job_template().goal_kind(),
        "{name}"
    );
    let (context, old_context) = (view.context_program(), legacy.context_program());
    assert_eq!(
        context.context_policy(),
        old_context.context_policy(),
        "{name}"
    );
    assert_eq!(context.must_include(), old_context.must_include(), "{name}");
    assert_eq!(context.exclude(), old_context.exclude(), "{name}");
    assert_eq!(
        context.section_detail(),
        old_context.section_detail(),
        "{name}"
    );
    assert_eq!(
        view.tool_surface().admitted(),
        legacy.tool_surface().admitted(),
        "{name}"
    );
    assert_eq!(
        view.tool_surface().forbidden(),
        legacy.tool_surface().forbidden(),
        "{name}"
    );
    let (lifecycle, old_lifecycle) = (view.lifecycle_machine(), legacy.lifecycle_machine());
    assert_eq!(
        lifecycle.allow_pause(),
        old_lifecycle.allow_pause(),
        "{name}"
    );
    assert_eq!(
        lifecycle.allow_rerun(),
        old_lifecycle.allow_rerun(),
        "{name}"
    );
    assert_eq!(
        lifecycle.max_attempts(),
        old_lifecycle.max_attempts(),
        "{name}"
    );
    assert_eq!(
        view.return_pipeline().validators(),
        legacy.return_pipeline().validators(),
        "{name}"
    );
    assert_eq!(
        view.return_pipeline().contract(),
        legacy.return_pipeline().contract(),
        "{name}"
    );
    let sources: Vec<(&str, &str)> = view
        .trace()
        .steps
        .iter()
        .map(|step| (step.source.as_str(), step.kind.as_str()))
        .collect();
    let old_sources: Vec<(&str, &str)> = legacy
        .trace()
        .steps
        .iter()
        .map(|step| (step.source.as_str(), step.kind.as_str()))
        .collect();
    assert_eq!(sources, old_sources, "{name}: resolution steps");
}

#[test]
fn test_v2_view_equals_legacy_lowering_for_every_builtin() -> TestResult {
    let legacy = builtin_agent_definitions(&HashMap::new()).map_err(ctx("legacy roles"))?;
    let irs = builtin_agent_irs(OffsetDateTime::now_utc()).map_err(ctx("v2 roles"))?;
    assert_eq!(
        legacy.keys().collect::<BTreeSet<_>>(),
        irs.keys().collect::<BTreeSet<_>>()
    );
    for (name, ir) in &irs {
        let old = legacy
            .get(name)
            .ok_or_else(|| TestError::Unexpected(format!("{name} missing in legacy")))?;
        assert_view_equals(name, ir, old);
    }

    let legacy_bases = builtin_base_definitions().map_err(ctx("legacy bases"))?;
    let base_irs = builtin_base_irs(OffsetDateTime::now_utc()).map_err(ctx("v2 bases"))?;
    assert_eq!(legacy_bases.len(), base_irs.len());
    for (name, ir) in &base_irs {
        let old = legacy_bases
            .get(name)
            .ok_or_else(|| TestError::Unexpected(format!("{name} missing in legacy bases")))?;
        assert_view_equals(name, ir, old);
    }
    Ok(())
}

#[test]
fn test_bound_builtin_programs_keep_every_section_in_v2() -> TestResult {
    let irs = builtin_agent_irs(OffsetDateTime::UNIX_EPOCH).map_err(ctx("v2 roles"))?;
    let explorer = irs.get("explorer").ok_or(TestError::Missing("explorer"))?;
    assert_eq!(explorer.context.program.as_deref(), Some("explore"));
    assert!(
        explorer
            .context
            .sections
            .iter()
            .any(|section| section.name == "repo.tree"),
        "normal-strength sections survive in v2"
    );
    let planner = irs.get("planner").ok_or(TestError::Missing("planner"))?;
    assert_eq!(
        planner.context.deferred,
        ["goal.invariants", "plan.current"]
    );
    Ok(())
}

#[test]
fn test_bundled_definitions_carry_their_instructions() -> TestResult {
    let irs = bundled_irs()?;
    let critic = irs
        .get("evidence-critic")
        .ok_or(TestError::Missing("evidence-critic"))?;
    assert_eq!(critic.instructions.source.as_deref(), Some("system.md"));
    assert!(!critic.instructions.text.is_empty());
    assert_eq!(critic.skill_names(), ["evidence-quality-review"]);
    assert_eq!(critic.binary.name, "evidence-critic");
    Ok(())
}
