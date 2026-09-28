//! End-to-end tests of the compiler over real built-in and bundled
//! definitions and small families in temporary layers. No cargo, no runner:
//! the artifact backend runs against a fake runner file and a stub probe.

use std::path::{Path, PathBuf};

use harw_agent_artifact::{Artifact, Bundle};
use harw_agent_compiler::backend::runner::{CAPABILITIES_SCHEMA, RunnerCapabilities, RunnerProbe};
use harw_agent_compiler::bin_dir::BinDir;
use harw_agent_compiler::commands::{AgentCommand, BuildArgs, CommandContext, run_command};
use harw_agent_compiler::graph::{GraphFormat, delegation_graph, render, rights_graph};
use harw_agent_compiler::render::render_diagnostics;
use harw_agent_compiler::scaffold::{ScaffoldRole, scaffold};
use harw_agent_compiler::testing::EchoStub;
use harw_agent_compiler::{
    AgentInput, CompileError, Compiled, Compiler, CompilerEnv, CompilerOptions, HARW_VERSION,
};
use harw_agent_dsl::ir_v2::Interface;

type TestResult = Result<(), Box<dyn std::error::Error>>;

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
            // Wie `harw-agent-runner --capabilities`: Interfaces stehen nur
            // unter `interfaces`, nie unter `features`.
            features: harw_registry_defaults::capability_catalog::PROVIDER_FEATURES
                .iter()
                .map(|feature| (*feature).to_owned())
                .collect(),
            child_protocol: None,
        })
    }
}

/// An isolated harw home with the given definitions under `agents/<name>/`.
///
/// Also installs the real built-in defaults (`harw-registry-defaults`), as
/// the `harw` binary does at start.
fn home_with(
    definitions: &[(&str, &str)],
) -> Result<(tempfile::TempDir, CompilerEnv), Box<dyn std::error::Error>> {
    harw_registry_defaults::compiler_defaults::install();
    let root = tempfile::tempdir()?;
    let home = root.path().join("home");
    for (name, text) in definitions {
        let dir = home.join("agents").join(name);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("definition.toml"), text)?;
    }
    std::fs::create_dir_all(&home)?;
    let env = CompilerEnv::isolated(home, root.path().to_path_buf());
    Ok((root, env))
}

fn compile(env: &CompilerEnv, name: &str) -> Result<Compiled, CompileError> {
    let mut compiler = Compiler::new(env.clone(), CompilerOptions::default())?;
    compiler.compile_input(&AgentInput::Name(name.to_owned()))
}

fn diagnostics_of(result: Result<Compiled, CompileError>) -> Result<Vec<String>, String> {
    match result {
        Ok(compiled) => Ok(compiled
            .diagnostics
            .codes()
            .iter()
            .map(|c| (*c).to_owned())
            .collect()),
        Err(CompileError::Diagnostics(diagnostics)) => Ok(diagnostics
            .codes()
            .iter()
            .map(|c| (*c).to_owned())
            .collect()),
        Err(other) => Err(other.to_string()),
    }
}

const WORKER: &str = "schema = \"harwness.agent/v1\"\nversion = \"1.0.0\"\nextends = { id = \"harwness.agent.worker-base@1\" }\nrole = \"worker\"\n";

fn worker(name: &str, body: &str) -> String {
    format!("id = \"acme.agent.{name}@1\"\nspecialization = \"{name}\"\n{WORKER}\n{body}\n")
}

#[test]
fn test_evidence_critic_artifact_is_reproducible() -> TestResult {
    let (_root, env) = home_with(&[])?;
    let first = compile(&env, "evidence-critic")?;
    let mut later = Compiler::new(
        env.clone(),
        CompilerOptions {
            now: time::OffsetDateTime::UNIX_EPOCH,
            ..CompilerOptions::default()
        },
    )?;
    let second = later.compile_input(&AgentInput::Name("evidence-critic".to_owned()))?;
    assert_eq!(
        first.artifact.digest(),
        second.artifact.digest(),
        "same input, same digest"
    );
    assert_eq!(first.artifact.to_bytes(), second.artifact.to_bytes());
    // The artifact reads back and carries the snapshot of the compiled IR.
    let parsed = Artifact::from_bytes(&first.artifact.to_bytes())?;
    let ir = harw_agent_compiler::artifact_out::ir_from_artifact(&parsed)?;
    assert!(ir.verify_snapshot());
    assert!(
        ir.trace.is_empty(),
        "no trace (timestamps, paths) in the header"
    );
    Ok(())
}

#[test]
fn test_resolve_skills_embeds_and_hashes() -> TestResult {
    let (_root, env) = home_with(&[])?;
    let compiled = compile(&env, "evidence-critic")?;
    let entry = &compiled.unit.ir.skills.entries[0];
    assert_eq!(entry.name, "evidence-quality-review");
    let bundle = Bundle::from_artifact(&compiled.artifact)?;
    let root = bundle.header.root.clone();
    let skill = bundle
        .file(
            &compiled.artifact,
            &root,
            "skills/evidence-quality-review/instructions.md",
        )
        .ok_or("skill in the pool")?;
    let expected = harw_agent_artifact::ArtifactDigest::of(skill).to_hex();
    assert_eq!(entry.hash.as_deref(), Some(expected.as_str()));
    assert!(
        bundle
            .file(&compiled.artifact, &root, "instructions/system.md")
            .is_some()
    );
    let manifest = bundle
        .file(
            &compiled.artifact,
            &root,
            "skills/evidence-quality-review/skill.toml",
        )
        .ok_or("skill manifest in the pool")?;
    let manifest_text = std::str::from_utf8(manifest)?;
    let table: toml::Table = toml::from_str(manifest_text)?;
    assert_eq!(
        table.get("name").and_then(toml::Value::as_str),
        Some("evidence-quality-review"),
        "{manifest_text}"
    );
    Ok(())
}

#[test]
fn test_embedded_skill_bundle_builds_a_skill_index() -> TestResult {
    let (_root, env) = home_with(&[])?;
    let compiled = compile(&env, "evidence-critic")?;
    let bundle = Bundle::from_artifact(&compiled.artifact)?;
    let root = bundle.header.root.clone();
    let entry = bundle.root().ok_or("root entry")?;
    let mut refs: Vec<(String, String)> = Vec::new();
    for reference in &entry.payload_refs {
        if reference.kind != "skill" {
            continue;
        }
        let bytes = bundle
            .file(&compiled.artifact, &root, &reference.logical_path)
            .ok_or("skill payload in the pool")?;
        let text = std::str::from_utf8(bytes)?.to_owned();
        refs.push((reference.logical_path.clone(), text));
    }
    let refs_as_str_pairs: Vec<(&str, &str)> = refs
        .iter()
        .map(|(path, contents)| (path.as_str(), contents.as_str()))
        .collect();
    let index = harw_catalog::SkillIndex::build_with_bundle(&[], &refs_as_str_pairs);
    let indexed = index
        .get("evidence-quality-review")
        .ok_or("skill in the index built from the embedded bundle")?;
    let instructions = bundle
        .file(
            &compiled.artifact,
            &root,
            "skills/evidence-quality-review/instructions.md",
        )
        .ok_or("instructions in the pool")?;
    let expected = std::str::from_utf8(instructions)?;
    assert_eq!(
        indexed.snapshot().instructions,
        expected,
        "the index's instructions are the embedded instructions.md text"
    );
    Ok(())
}

#[test]
fn test_rights_check_rejects_widening_with_a_located_error() -> TestResult {
    let text = worker(
        "widener",
        "[tools]\nadmitted = [\n  \"fs.read\",\n  \"shell.exec\",\n]\n\n[spawn]\nmax_depth = 3\n",
    );
    let (_root, env) = home_with(&[("widener", &text)])?;
    let mut compiler = Compiler::new(env.clone(), CompilerOptions::default())?;
    let error = compiler
        .compile_input(&AgentInput::Name("widener".to_owned()))
        .err()
        .ok_or("widening must fail")?;
    let diagnostics = error.diagnostics().ok_or("diagnostics")?;
    let widening: Vec<_> = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "HARW-BUILD-004")
        .collect();
    assert_eq!(widening.len(), 2, "shell.exec and the depth: {diagnostics}");
    let rendered = render_diagnostics(diagnostics.as_slice(), &compiler.sources().files);
    assert!(rendered.contains("error[HARW-BUILD-004]"), "{rendered}");
    assert!(
        rendered.contains("definition.toml:11:"),
        "file:line of the tool: {rendered}"
    );
    assert!(
        rendered.contains("\"shell.exec\","),
        "the excerpt: {rendered}"
    );
    assert!(
        rendered.contains("| ") && rendered.contains('^'),
        "the caret: {rendered}"
    );
    Ok(())
}

#[test]
fn test_validate_roles_reachable_prune_and_models() -> TestResult {
    let steward = "schema = \"harwness.agent/v1\"\nid = \"acme.agent.steward@1\"\nversion = \"1.0.0\"\nrole = \"agent-steward\"\nspecialization = \"steward\"\n";
    let unknown = worker("unknown", "[tools]\nadmitted = [\"fs.reed\"]\n");
    let prunable = worker(
        "prunable",
        "[tools]\nadmitted = [\"fs.read\", \"parent.message\"]\n\n[models]\nprovider = \"anthropic\"\nmodel = \"claude-sonnet-5\"\n",
    );
    let (_root, env) = home_with(&[
        ("steward", steward),
        ("unknown", &unknown),
        ("prunable", &prunable),
    ])?;
    assert!(diagnostics_of(compile(&env, "steward"))?.contains(&"HARW-BUILD-001".to_owned()));
    let codes = diagnostics_of(compile(&env, "unknown"))?;
    assert!(codes.contains(&"HARW-BUILD-003".to_owned()), "{codes:?}");

    let compiled = compile(&env, "prunable")?;
    assert_eq!(
        compiled.unit.ir.permissions.required_env,
        ["ANTHROPIC_API_KEY"],
        "ResolveModels adds the provider's credential"
    );
    assert!(compiled.diagnostics.contains_code("HARW-BUILD-010"));
    assert!(compiled.unit.providers.contains_key("fs"));
    assert!(compiled.unit.features.contains("tool-fs"));
    assert!(compiled.unit.features.contains("core"));
    Ok(())
}

#[test]
fn test_prune_drops_spawn_tools_of_a_worker() -> TestResult {
    let text = worker(
        "pruner",
        "[tools]\nadmitted = [\"fs.read\", \"agent.status\"]\n",
    );
    let (_root, env) = home_with(&[("pruner", &text)])?;
    // agent.status is outside the analyst ceiling too; the rights check
    // reports it first. The prune pass alone:
    let mut compiler = Compiler::new(env.clone(), CompilerOptions::default())?;
    let entry = match compiler.resolve(&AgentInput::Name("pruner".to_owned()))? {
        harw_agent_compiler::discovery::Target::Entry(entry) => entry,
        harw_agent_compiler::discovery::Target::Broken(_) => return Err("broken".into()),
    };
    let mut unit = compiler.front_end(&entry).map_err(|d| d.to_string())?;
    use harw_agent_compiler::passes::{Pass, PruneUnusedTools};
    let diagnostics = PruneUnusedTools.run(&mut unit);
    assert!(diagnostics.contains_code("HARW-BUILD-009"));
    assert_eq!(unit.ir.tools.admitted, ["fs.read"]);
    assert_eq!(unit.ir.permissions.tools, ["fs.read"]);
    assert_eq!(unit.pruned.len(), 1);
    Ok(())
}

const LEAD: &str = "schema = \"harwness.agent/v1\"\nid = \"acme.agent.lead@1\"\nversion = \"1.0.0\"\nextends = { id = \"harwness.agent.child-orchestrator-base@1\" }\nrole = \"child-orchestrator\"\nspecialization = \"lead\"\n\n[delegation]\ntargets = [\"reader\", \"writer\"]\n";

#[test]
fn test_child_closure_embeds_the_family_and_graphs_render() -> TestResult {
    let reader = worker(
        "reader",
        "[tools]\nadmitted = [\"fs.read\"]\n\n[spawn]\nmax_depth = 0\n",
    );
    let writer = worker(
        "writer",
        "[tools]\nadmitted = [\"fs.read\", \"fs.write\"]\n\n[spawn]\nmax_depth = 0\n",
    );
    let (_root, env) = home_with(&[("lead", LEAD), ("reader", &reader), ("writer", &writer)])?;
    let compiled = compile(&env, "lead")?;
    let names: Vec<&str> = compiled
        .unit
        .children
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    assert_eq!(names, ["reader", "writer"]);
    assert!(compiled.unit.children[0].read_only);
    assert!(!compiled.unit.children[1].read_only);
    for child in ["reader", "writer"] {
        let child = compile(&env, child)?;
        assert!(
            child.unit.features.is_subset(&compiled.unit.features),
            "the shared executable must link every child's tool providers"
        );
    }
    let bundle = Bundle::from_artifact(&compiled.artifact)?;
    assert_eq!(
        bundle.agents.len(),
        3,
        "root plus two children, one entry each"
    );
    let writer = bundle
        .agents
        .get("acme.agent.writer@1")
        .ok_or("writer entry")?;
    assert_eq!(
        bundle.header.agents["acme.agent.writer@1"].snapshot,
        compiled.unit.children[1].snapshot
    );
    assert!(writer.children.is_empty());
    let root = bundle.root().ok_or("root entry")?;
    let links: Vec<&str> = root
        .children
        .iter()
        .map(|child| child.name.as_str())
        .collect();
    assert_eq!(links, ["reader", "writer"]);

    let graph = delegation_graph(&[&compiled]);
    let text = render(std::slice::from_ref(&graph), GraphFormat::Text);
    assert!(text.contains("└─delegation→ writer"), "{text}");
    let dot = render(std::slice::from_ref(&graph), GraphFormat::Dot);
    assert!(dot.contains("\"lead\" -> \"reader\""), "{dot}");
    let mermaid = render(std::slice::from_ref(&graph), GraphFormat::Mermaid);
    assert!(
        mermaid.contains("nlead -->|\"delegation\"| nwriter"),
        "{mermaid}"
    );
    let rights = render(&[rights_graph(&compiled)], GraphFormat::Text);
    assert!(
        rights.contains("base role analysis-orchestrator"),
        "{rights}"
    );
    Ok(())
}

#[test]
fn test_requirements_are_derived_and_unioned_over_the_family() -> TestResult {
    use harw_agent_dsl::ir_v2::{RequirementLevel, TargetSpec};

    let reader = worker(
        "reader",
        "[tools]\nadmitted = [\"fs.read\"]\n\n[spawn]\nmax_depth = 0\n",
    );
    let writer = worker(
        "writer",
        "[tools]\nadmitted = [\"fs.read\", \"fs.write\"]\n\n[spawn]\nmax_depth = 0\n",
    );
    let (_root, env) = home_with(&[("lead", LEAD), ("reader", &reader), ("writer", &writer)])?;
    let host = TargetSpec::from_triple(&env.host_target).ok_or("host triple")?;

    let reader = compile(&env, "reader")?;
    let own = &reader.unit.ir.requirements;
    assert!(!own.filesystem_write && !own.process_exec && !own.host_access);
    assert_eq!(own.sandbox.filesystem, RequirementLevel::NotNeeded);
    assert_eq!(own.targets, std::slice::from_ref(&host), "the build target");

    let lead = compile(&env, "lead")?;
    let family = &lead.unit.ir.requirements;
    assert!(
        family.filesystem_write,
        "the writer child's write is unioned"
    );
    assert_eq!(family.sandbox.filesystem, RequirementLevel::BestEffort);
    assert_eq!(family.targets, [host]);
    assert!(lead.unit.ir.verify_snapshot(), "requirements are hashed");

    // An explicit `--target` names that target.
    let mut cross = Compiler::new(
        env.clone(),
        CompilerOptions {
            target: Some("aarch64-apple-darwin".to_owned()),
            ..CompilerOptions::default()
        },
    )?;
    let cross = cross.compile_input(&AgentInput::Name("reader".to_owned()))?;
    assert_eq!(
        cross.unit.ir.requirements.targets,
        [TargetSpec {
            os: "macos".to_owned(),
            arch: Some("aarch64".to_owned()),
        }]
    );
    Ok(())
}

#[test]
fn test_child_cache_keeps_ancestry_and_depth_separate() -> TestResult {
    use harw_agent_compiler::passes::ChildResolver;

    let reader = worker("reader", "[spawn]\nmax_depth = 0\n");
    let writer = worker("writer", "[spawn]\nmax_depth = 0\n");
    let (_root, env) = home_with(&[("lead", LEAD), ("reader", &reader), ("writer", &writer)])?;
    let compiler = Compiler::new(env, CompilerOptions::default())?;
    let first = compiler.compile_child("lead", 1, &["root".to_owned()])?;
    assert!(first.children.iter().all(|child| child.depth == 2));
    let deeper =
        compiler.compile_child("lead", 2, &["another-root".to_owned(), "middle".to_owned()])?;
    assert!(deeper.children.iter().all(|child| child.depth == 3));
    assert!(
        compiler
            .compile_child("lead", 1, &["reader".to_owned()])
            .is_err(),
        "a successful cache entry must not hide a cycle in another ancestry"
    );
    Ok(())
}

#[test]
fn test_child_may_not_exceed_the_parent_budget() -> TestResult {
    // Within its own ceiling (analyst: 120000 tokens) ...
    let greedy = worker(
        "greedy",
        "[tools]\nadmitted = [\"fs.read\"]\n\n[spawn]\nmax_depth = 0\n\n[spawn.budget]\nmax_tokens = 80000\n",
    );
    // ... but above what its parent can pass down.
    let lead = format!(
        "{}\n[patch.spawn.budget]\nmax_tokens = {{ min = 50000 }}\n",
        LEAD.replace("[\"reader\", \"writer\"]", "[\"greedy\"]")
    );
    let (_root, env) = home_with(&[("lead", &lead), ("greedy", &greedy)])?;
    let codes = diagnostics_of(compile(&env, "lead"))?;
    assert!(codes.contains(&"HARW-BUILD-013".to_owned()), "{codes:?}");
    Ok(())
}

#[test]
fn test_scaffolded_worker_checks_clean() -> TestResult {
    let (_root, env) = home_with(&[])?;
    let dir = env.home.join("agents").join("fresh");
    scaffold(&dir, "fresh", ScaffoldRole::Worker, None)?;
    let compiled = compile(&env, "fresh")?;
    assert!(!compiled.diagnostics.has_errors());
    assert_eq!(compiled.unit.ir.binary.name, "fresh");
    Ok(())
}

#[test]
fn test_build_install_versions_use_and_inspect() -> TestResult {
    let (_root, env) = home_with(&[])?;
    let runner = env.home.join("fake-runner");
    std::fs::write(&runner, b"#!/bin/sh\necho fake runner\n")?;
    let out_dir = tempfile::tempdir()?;
    let mut progress = |_: &str| {};
    let mut ctx = CommandContext {
        env: env.clone(),
        probe: &FullRunner,
        case_runner: &EchoStub,
        progress: &mut progress,
    };
    let args = BuildArgs {
        target: "evidence-critic".to_owned(),
        interfaces: Some(vec![Interface::Cli, Interface::Mcp]),
        runner: Some(runner.clone()),
        output: Some(out_dir.path().to_path_buf()),
        ..BuildArgs::default()
    };
    let output = run_command(&mut ctx, AgentCommand::Build(args.clone()));
    assert_eq!(output.exit_code, 0, "{}", output.text);
    let installed = env.bin_dir().join("evidence-critic");
    assert!(
        installed.is_file(),
        "default install path ~/.harw/bin/<name>"
    );
    assert!(
        out_dir.path().join("evidence-critic").is_file(),
        "-o copies as well"
    );

    // A second build with other interfaces is another version.
    let mut second = args;
    second.interfaces = Some(vec![Interface::Cli]);
    second.output = None;
    let output = run_command(&mut ctx, AgentCommand::Build(second));
    assert_eq!(output.exit_code, 0, "{}", output.text);
    let bin = BinDir::new(env.bin_dir());
    let versions = bin.versions("evidence-critic")?;
    assert_eq!(versions.len(), 2);
    assert!(versions[1].current);

    // `use` switches back to the first one.
    let output = run_command(
        &mut ctx,
        AgentCommand::Use {
            name: "evidence-critic".to_owned(),
            version: versions[0].record.artifact_digest[..8].to_owned(),
        },
    );
    assert_eq!(output.exit_code, 0, "{}", output.text);
    assert_eq!(
        bin.current("evidence-critic").map(|v| v.dir_name),
        Some(versions[0].dir_name.clone())
    );

    // `inspect` by installed name reads the embedded artifact.
    let output = run_command(
        &mut ctx,
        AgentCommand::Inspect {
            target: "evidence-critic".to_owned(),
        },
    );
    assert_eq!(output.exit_code, 0, "{}", output.text);
    assert!(
        output.text.contains("harwness.agent.evidence-critic@1"),
        "{}",
        output.text
    );
    assert!(
        output.text.contains("evidence-quality-review"),
        "{}",
        output.text
    );
    assert_eq!(
        output.json["skills"][0]["verified"],
        serde_json::json!(true)
    );
    assert_eq!(output.json["container"], serde_json::json!("binary"));
    Ok(())
}

#[test]
fn test_missing_runner_is_a_clear_error() -> TestResult {
    let (_root, env) = home_with(&[])?;
    let mut progress = |_: &str| {};
    let mut ctx = CommandContext {
        env,
        probe: &FullRunner,
        case_runner: &EchoStub,
        progress: &mut progress,
    };
    let output = run_command(
        &mut ctx,
        AgentCommand::Build(BuildArgs {
            target: "evidence-critic".to_owned(),
            ..BuildArgs::default()
        }),
    );
    assert_eq!(output.exit_code, 1);
    assert!(output.text.contains("--artifact-only"), "{}", output.text);
    assert!(output.text.contains("make install"), "{}", output.text);
    Ok(())
}

#[test]
fn test_check_explain_and_fmt_commands() -> TestResult {
    let broken = "schema = \"harwness.agent/v1\"\nid = \"acme.agent.b@1\"\nversion = \"1.0.0\"\nrole = \"worker\"\nspecialization = \"b\"\n\n[tools]\nadmitted = [\"fs read\"]\n";
    let (_root, env) = home_with(&[("b", broken)])?;
    let mut progress = |_: &str| {};
    let mut ctx = CommandContext {
        env: env.clone(),
        probe: &FullRunner,
        case_runner: &EchoStub,
        progress: &mut progress,
    };
    let output = run_command(
        &mut ctx,
        AgentCommand::Check {
            targets: vec!["b".to_owned()],
        },
    );
    assert_eq!(output.exit_code, 1);
    assert!(output.text.contains("HARW-TOOL-003"), "{}", output.text);
    assert!(
        output.text.contains("definition.toml:8:"),
        "{}",
        output.text
    );

    let output = run_command(
        &mut ctx,
        AgentCommand::Explain {
            target: "HARW-TOOL-003".to_owned(),
            field: None,
        },
    );
    assert_eq!(output.exit_code, 0);
    assert!(output.text.contains("Fix: "), "{}", output.text);

    let output = run_command(
        &mut ctx,
        AgentCommand::Explain {
            target: "evidence-critic".to_owned(),
            field: Some("tool:fs.read".to_owned()),
        },
    );
    assert_eq!(output.exit_code, 0, "{}", output.text);
    assert!(output.text.contains("admitted"), "{}", output.text);
    assert!(output.text.contains("provider `fs`"), "{}", output.text);

    let path: PathBuf = env.home.join("agents").join("b").join("definition.toml");
    let output = run_command(
        &mut ctx,
        AgentCommand::Fmt {
            paths: vec![path.clone()],
            check: false,
        },
    );
    assert_eq!(output.exit_code, 0, "{}", output.text);
    let once = std::fs::read_to_string(&path)?;
    let output = run_command(
        &mut ctx,
        AgentCommand::Fmt {
            paths: vec![path.clone()],
            check: true,
        },
    );
    assert_eq!(
        output.exit_code, 0,
        "formatted files pass --check: {}",
        output.text
    );
    assert_eq!(std::fs::read_to_string(&path)?, once);
    Ok(())
}

#[test]
fn test_children_sharing_a_skill_store_it_once() -> TestResult {
    let with_skill = |name: &str| {
        format!(
            "id = \"acme.agent.{name}@1\"\nspecialization = \"{name}\"\nskills = [\"evidence-quality-review\"]\n{WORKER}\n[tools]\nadmitted = [\"fs.read\"]\n\n[spawn]\nmax_depth = 0\n"
        )
    };
    let (_root, env) = home_with(&[
        ("lead", LEAD),
        ("reader", &with_skill("reader")),
        ("writer", &with_skill("writer")),
    ])?;
    let compiled = compile(&env, "lead")?;
    let bundle = Bundle::from_artifact(&compiled.artifact)?;
    let stats = bundle.pool_stats();
    let is_instructions = |reference: &&harw_agent_artifact::PayloadRef| {
        reference.kind == "skill" && reference.logical_path.ends_with("/instructions.md")
    };
    let is_manifest = |reference: &&harw_agent_artifact::PayloadRef| {
        reference.kind == "skill" && reference.logical_path.ends_with("/skill.toml")
    };
    let instructions_refs = bundle
        .agents
        .values()
        .flat_map(|entry| entry.payload_refs.iter())
        .filter(is_instructions)
        .count();
    assert_eq!(instructions_refs, 2, "both children reference the skill");
    let instructions_blobs: std::collections::BTreeSet<_> = bundle
        .agents
        .values()
        .flat_map(|entry| entry.payload_refs.iter())
        .filter(is_instructions)
        .map(|reference| reference.blake3)
        .collect();
    assert_eq!(instructions_blobs.len(), 1, "stored once");
    let manifest_refs = bundle
        .agents
        .values()
        .flat_map(|entry| entry.payload_refs.iter())
        .filter(is_manifest)
        .count();
    assert_eq!(manifest_refs, 2, "both children reference the manifest");
    let manifest_blobs: std::collections::BTreeSet<_> = bundle
        .agents
        .values()
        .flat_map(|entry| entry.payload_refs.iter())
        .filter(is_manifest)
        .map(|reference| reference.blake3)
        .collect();
    assert_eq!(manifest_blobs.len(), 1, "identical manifest is stored once");
    assert!(stats.saved_bytes() > 0);

    // inspect shows the refs per agent and the savings.
    let artifact_file = env.home.join("lead.harwa");
    std::fs::write(&artifact_file, compiled.artifact.to_bytes())?;
    let report = harw_agent_compiler::inspect::inspect_path(&artifact_file, None)?;
    let text = report.text();
    assert!(text.contains("einzigartig"), "{text}");
    assert!(text.contains("  requirements:\n"), "{text}");
    assert!(text.contains("    targets:    "), "{text}");
    assert!(text.contains("    sandbox:    filesystem="), "{text}");
    assert!(text.contains("gespart"), "{text}");
    assert!(
        text.contains("skill skills/evidence-quality-review/instructions.md"),
        "{text}"
    );
    assert_eq!(report.agents.len(), 3);
    Ok(())
}
