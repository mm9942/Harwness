//! Guard for `examples/agents/driven-orchestrator`, the reference WorkDriver
//! definition (backlog C-05). The example must keep compiling through the
//! real pipeline, and its `[work_driver]` table must reach the IR with the
//! values the WorkDriver guide documents. A copy without the table must lower
//! to `work_driver == None`, so the positive assertions cannot pass vacuously.
//! No cargo, no runner: compiling never starts an agent.

use std::path::{Path, PathBuf};

use harw_agent_compiler::{AgentInput, Compiled, Compiler, CompilerEnv, CompilerOptions};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const DEFINITION: &str = "definition.toml";

/// The example's directory, found from the compiler crate's manifest.
fn example_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("the compiler crate has no parent directory")?
        .to_path_buf();
    let dir = workspace
        .join("examples")
        .join("agents")
        .join("driven-orchestrator");
    if !dir.join(DEFINITION).is_file() {
        return Err(format!("missing {}", dir.join(DEFINITION).display()).into());
    }
    Ok(dir)
}

/// Compiles the definition directory `dir` in an isolated harw home, with the
/// real built-in defaults installed as the `harw` binary does at start.
fn compile_dir(dir: &Path, scratch: &Path) -> Result<Compiled, Box<dyn std::error::Error>> {
    harw_registry_defaults::compiler_defaults::install();
    let home = scratch.join("home");
    std::fs::create_dir_all(&home)?;
    let env = CompilerEnv::isolated(home, scratch.to_path_buf());
    let mut compiler = Compiler::new(env, CompilerOptions::default())?;
    Ok(compiler.compile_input(&AgentInput::Path(dir.to_path_buf()))?)
}

/// `text` without the `[work_driver]` table: from its header up to (not
/// including) the next table header at the start of a line.
fn without_work_driver(text: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed == "[work_driver]" {
            inside = true;
            continue;
        }
        if inside && line.starts_with('[') {
            inside = false;
        }
        if !inside {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[test]
fn test_driven_orchestrator_example_lowers_its_work_driver() -> TestResult {
    let dir = example_dir()?;
    let scratch = tempfile::tempdir()?;
    let compiled = compile_dir(&dir, scratch.path())?;

    let driver = compiled
        .unit
        .ir
        .work_driver
        .as_ref()
        .ok_or("the example's [work_driver] table did not reach the IR")?;
    assert_eq!(driver.worker_role, "executor");
    assert_eq!(driver.judge_role.as_deref(), Some("analyst"));
    assert_eq!(driver.max_iterations, 8);
    assert_eq!(driver.max_parallel_workers, 3);
    assert_eq!(driver.max_attempts_per_worker, 3);
    assert_eq!(driver.stall_iterations, 2);
    assert_eq!(
        driver.verify,
        vec![
            "cargo test --workspace".to_owned(),
            "cargo clippy --workspace -- -D warnings".to_owned(),
        ]
    );
    assert_eq!(driver.token_budget, Some(800_000));
    assert_eq!(driver.wall_budget_secs, Some(5400));
    Ok(())
}

#[test]
fn test_driven_orchestrator_without_work_driver_table_has_no_driver() -> TestResult {
    let source = example_dir()?;
    let scratch = tempfile::tempdir()?;

    // The example directory holds `definition.toml` and `system.md` (the
    // `instructions_file`); copy every regular file, rewriting the definition.
    let copy = scratch.path().join("driven-orchestrator");
    std::fs::create_dir_all(&copy)?;
    for entry in std::fs::read_dir(&source)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        if name == DEFINITION {
            let original = std::fs::read_to_string(entry.path())?;
            let stripped = without_work_driver(&original);
            // Check the parsed tables, not the text: the example's comments
            // mention `[work_driver]` and stay in the copy.
            let before: toml::Table = toml::from_str(&original)?;
            let after: toml::Table = toml::from_str(&stripped)?;
            assert!(
                before.contains_key("work_driver") && !after.contains_key("work_driver"),
                "the [work_driver] table was not removed from the copy"
            );
            std::fs::write(copy.join(&name), stripped)?;
        } else {
            std::fs::copy(entry.path(), copy.join(&name))?;
        }
    }

    let compiled = compile_dir(&copy, scratch.path())?;
    assert!(
        compiled.unit.ir.work_driver.is_none(),
        "a definition without [work_driver] must lower to work_driver == None"
    );
    Ok(())
}
