//! The compiler over the real built-in defaults (`harw-registry-defaults`,
//! injected through `builtins::BuiltinDefaults`): discovery of the built-in
//! and bundled definitions, the rights algebra over the capability catalog,
//! the generic base-role ceilings and the automatic UIA build.
//!
//! These tests were unit tests before the compiler stopped depending on
//! `harw-registry-defaults`; they live here because only an integration test
//! links the same compiler crate the registry's implementation is written
//! against.

use std::collections::BTreeSet;
use std::path::Path;

use harw_agent_compiler::backend::runner::{CAPABILITIES_SCHEMA, RunnerCapabilities, RunnerProbe};
use harw_agent_compiler::bin_dir::BinDir;
use harw_agent_compiler::discovery::{Origin, SourceSet};
use harw_agent_compiler::rights::{BuiltinCeilings, RightsSet, classes_of, delta, is_writing};
use harw_agent_compiler::uia::{AutoBuildOutcome, auto_build_uia};
use harw_agent_compiler::{CompileError, CompilerEnv, HARW_VERSION};
use harw_agent_dsl::ir_v2::{Effort, Interface};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn install() {
    harw_registry_defaults::compiler_defaults::install();
}

fn set(tools: &[&str], depth: Option<u32>, budget: Option<u64>) -> RightsSet {
    RightsSet::ceiling(
        tools.iter().map(|tool| (*tool).to_owned()),
        depth,
        budget,
        None,
    )
}

#[test]
fn test_delta_reports_tools_classes_depth_budget_effort() {
    install();
    let mut claimed = set(&["fs.read", "shell.exec"], Some(3), Some(90));
    claimed.effort_cap = Some(Effort::High);
    let mut limit = set(&["fs.read"], Some(1), Some(80));
    limit.effort_cap = Some(Effort::Low);
    let delta = delta(&claimed, &limit, &BTreeSet::new());
    assert_eq!(delta.added_tools, ["shell.exec"]);
    assert_eq!(delta.added_classes, ["shell"]);
    assert_eq!(delta.depth, Some((3, 1)));
    assert_eq!(delta.budget, Some((90, 80)));
    assert_eq!(delta.effort, Some((Effort::High, Effort::Low)));
    assert_eq!(delta.lines().len(), 5);
}

#[test]
fn test_handoffs_to_declared_targets_are_exempt() {
    install();
    let claimed = set(
        &["transfer_to_explorer", "transfer_to_executor"],
        None,
        None,
    );
    let limit = set(&[], None, None);
    let targets: BTreeSet<String> = ["explorer".to_owned()].into();
    let delta = delta(&claimed, &limit, &targets);
    assert_eq!(delta.added_tools, ["transfer_to_executor"]);
    assert_eq!(delta.added_classes, ["agent"]);
}

#[test]
fn test_classes_and_writing_come_from_the_catalog() {
    install();
    let tools: BTreeSet<String> = ["fs.read", "fs.write", "no.such.tool"]
        .iter()
        .map(|tool| (*tool).to_owned())
        .collect();
    let classes: Vec<String> = classes_of(&tools).into_iter().collect();
    assert_eq!(classes, ["read", "write"]);
    assert!(is_writing(&set(&["fs.write"], None, None)));
    assert!(is_writing(&set(&["shell.exec"], None, None)));
    assert!(!is_writing(&set(&["fs.read"], None, None)));
}

#[test]
fn test_generic_worker_ceiling_is_the_analyst() -> Result<(), CompileError> {
    install();
    let ceilings = BuiltinCeilings::load(time::OffsetDateTime::UNIX_EPOCH)?;
    let analyst = ceilings
        .role("analyst")
        .ok_or(CompileError::Other("analyst".into()))?;
    let ceiling = ceilings
        .ceiling_for(analyst)
        .ok_or(CompileError::Other("ceiling".into()))?;
    assert_eq!(ceiling.base_role, "analyst");
    assert_eq!(ceiling.reason, "self");
    Ok(())
}

#[test]
fn test_builtin_and_bundled_definitions_are_found() -> Result<(), CompileError> {
    install();
    let set = SourceSet::discover(&[])?;
    assert!(set.find("explorer").is_some(), "built-in role");
    let critic = set.find("evidence-critic");
    assert_eq!(
        critic.map(|entry| entry.origin.clone()),
        Some(Origin::Bundled),
        "bundled definition"
    );
    assert!(set.find("no-such-agent").is_none());
    assert!(
        set.suggestions("evidence-critc")
            .contains(&"evidence-critic".to_owned())
    );
    assert!(
        !set.compilable_names()
            .iter()
            .any(|name| name == "worker-base" || name == "child-orchestrator-base"),
        "bases are not compilable"
    );
    Ok(())
}

#[test]
fn test_layer_definition_shadows_the_bundled_copy() -> TestResult {
    install();
    let home = tempfile::tempdir()?;
    let dir = home.path().join("agents").join("evidence-critic");
    std::fs::create_dir_all(&dir)?;
    let bundled = harw_home::bundled_files()
        .iter()
        .find(|file| file.relative_path == "agents/evidence-critic/definition.toml")
        .map(|file| file.contents)
        .ok_or("bundled evidence-critic")?;
    std::fs::write(dir.join("definition.toml"), bundled)?;
    let set = SourceSet::discover(&[home.path().to_path_buf()])?;
    let found = set.find("evidence-critic").ok_or("found")?;
    assert!(matches!(found.origin, Origin::Layer(_)));
    let copies = set
        .entries
        .iter()
        .filter(|entry| entry.id.to_string() == "harwness.agent.evidence-critic@1")
        .count();
    assert_eq!(copies, 1, "no overlay of identical copies");
    Ok(())
}

struct FullRunner;

impl RunnerProbe for FullRunner {
    fn capabilities(&self, _runner: &Path) -> Result<RunnerCapabilities, CompileError> {
        Ok(RunnerCapabilities {
            schema: CAPABILITIES_SCHEMA.to_owned(),
            runner_version: HARW_VERSION.to_owned(),
            target: harw_agent_compiler::env::host_target(),
            artifact_formats: vec![harw_agent_artifact::FORMAT_VERSION],
            ir_schema: harw_agent_dsl::AGENT_IR_SCHEMA.to_owned(),
            interfaces: Interface::ALL
                .iter()
                .map(|i| i.as_str().to_owned())
                .collect(),
            features: harw_registry_defaults::capability_catalog::PROVIDER_FEATURES
                .iter()
                .map(|feature| (*feature).to_owned())
                .chain(Interface::ALL.iter().map(|i| i.as_str().to_owned()))
                .collect(),
            child_protocol: None,
        })
    }
}

const UIA: &str = "schema = \"harwness.agent/v1\"\nid = \"user.agent.mia@1\"\nversion = \"1.0.0\"\nrole = \"user-interface\"\nspecialization = \"terminal-ui\"\nname = \"Mia\"\n";

#[test]
fn test_auto_build_uia_builds_once_then_only_on_change() -> TestResult {
    install();
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    let agent = home.join("agents").join("mia");
    std::fs::create_dir_all(&agent)?;
    std::fs::write(agent.join("definition.toml"), UIA)?;
    std::fs::write(agent.join("identity.md"), "I am Mia.\n")?;
    std::fs::write(
        home.join("config.toml"),
        "active_uia_definition = \"user.agent.mia@1\"\n",
    )?;
    let env = CompilerEnv::isolated(home, root.path().to_path_buf());
    let runner = env
        .home_runner_dir(&env.host_target)
        .join(harw_agent_compiler::env::RUNNER_BINARY);
    std::fs::create_dir_all(runner.parent().ok_or("parent")?)?;
    std::fs::write(runner, b"fake runner")?;

    let AutoBuildOutcome::Built { name, path, digest } = auto_build_uia(&env, &FullRunner) else {
        return Err("first run builds".into());
    };
    assert_eq!(name, "harw-uia-terminal-ui");
    assert!(path.is_file());
    assert!(env.bin_dir().join(&name).exists(), "current link");
    assert_eq!(
        auto_build_uia(&env, &FullRunner),
        AutoBuildOutcome::Unchanged { name: name.clone() }
    );
    // A bundle file changes: the digest changes, it rebuilds.
    std::fs::write(
        env.home.join("agents").join("mia").join("identity.md"),
        "I am Mia, now with more detail.\n",
    )?;
    let AutoBuildOutcome::Built { digest: second, .. } = auto_build_uia(&env, &FullRunner) else {
        return Err("a changed bundle rebuilds".into());
    };
    assert_ne!(digest, second);
    assert_eq!(BinDir::new(env.bin_dir()).versions(&name)?.len(), 2);
    Ok(())
}
