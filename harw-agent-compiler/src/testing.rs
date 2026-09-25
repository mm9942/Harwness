//! `harw agent test`: validate, build in memory, run the definition's
//! example cases.
//!
//! Cases live in `tests/*.toml` next to `definition.toml`:
//!
//! ```toml
//! name = "reviews a sourced claim"
//! prompt = "Prüfe die Quelle in report.md"
//!
//! [expect]
//! # Static checks against the rights manifest (run now):
//! tools = ["fs.read"]              # must be in the manifest
//! not_tools = ["shell.exec"]       # must not be in the manifest
//! # Answer checks (need a run of the agent):
//! contains = ["Behalten"]
//! not_contains = ["TODO"]
//! ```
//!
//! # Two [`CaseRunner`]s
//! [`EchoStub`] never runs the agent (every answer check reports `pending`,
//! never `passed`); harw-cli and the `/agent` op used it as their only
//! option before wave 3B, and both still default to it, so it stays here
//! unchanged for them to keep building against.
//!
//! [`SubprocessCaseRunner`] is the real one wave 3B adds. It does **not**
//! link `harw-agent-runner` in-process — this crate has no dependency on it
//! at all, on purpose (see this crate's `Cargo.toml` and
//! `crate::commands::run_agent`'s doc comment, which makes the same call for
//! `harw agent run`): it locates a runner exactly as `harw agent build`'s
//! artifact backend does ([`crate::backend::runner::locate_runner`]),
//! appends the case's already-built [`Artifact`] to it in memory
//! ([`harw_agent_artifact::append_to_executable`]), execs the combined
//! bytes as a throwaway one-shot process with `--json`, and reads the
//! agent's final answer back out of the emitted `harwness_sdk::SdkEvent`
//! JSON lines. Whoever wires up a `CommandContext` (currently `harw-cli`'s
//! and `harw-ops`'s `EchoStub` call sites) switches to this once a runner is
//! actually being shipped; that wiring is outside this file's scope.

use std::path::{Path, PathBuf};

use harw_agent_artifact::Artifact;
use harw_agent_dsl::ir_v2::AgentIr;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::backend::runner::locate_runner;
use crate::env::CompilerEnv;
use crate::error::CompileError;

/// Runs one prompt against a built artifact.
pub trait CaseRunner {
    /// The agent's final answer to `prompt`.
    ///
    /// # Errors
    /// `Err(reason)` if the agent cannot run.
    fn run(&self, artifact: &Artifact, prompt: &str) -> Result<String, String>;

    /// `false` for a stand-in that does not really run the agent.
    fn is_real(&self) -> bool;
}

/// The pre-wave-3B stand-in: runs nothing. See the module docs for why this
/// stays alongside [`SubprocessCaseRunner`] rather than being replaced.
#[derive(Debug, Clone, Copy, Default)]
pub struct EchoStub;

impl CaseRunner for EchoStub {
    fn run(&self, _artifact: &Artifact, prompt: &str) -> Result<String, String> {
        Ok(prompt.to_owned())
    }

    fn is_real(&self) -> bool {
        false
    }
}

/// Runs a case's prompt through a located `harw-agent-runner`, out of
/// process. See the module docs for why (no in-process dependency on the
/// runner) and how (locate, embed in memory, exec, parse `--json`).
pub struct SubprocessCaseRunner<'a> {
    env: &'a CompilerEnv,
}

impl<'a> SubprocessCaseRunner<'a> {
    /// Borrows `env` for locating a runner (the same search `harw agent
    /// build` uses: `--runner`, next to `harw`, the install record, then the
    /// harw home).
    #[must_use]
    pub fn new(env: &'a CompilerEnv) -> Self {
        Self { env }
    }
}

impl CaseRunner for SubprocessCaseRunner<'_> {
    fn run(&self, artifact: &Artifact, prompt: &str) -> Result<String, String> {
        let runner = locate_runner(self.env, None, &self.env.host_target)
            .map_err(|error| error.to_string())?;
        let runner_bytes = std::fs::read(&runner.path)
            .map_err(|error| format!("read {}: {error}", runner.path.display()))?;
        let binary = harw_agent_artifact::append_to_executable(&runner_bytes, artifact);
        let temp = std::env::temp_dir().join(format!(
            "harw-agent-test-{}-{}",
            std::process::id(),
            artifact.digest()
        ));
        harw_agent_artifact::write_executable(&temp, &binary)
            .map_err(|error| format!("write {}: {error}", temp.display()))?;
        let result = self.run_temp(&temp, prompt);
        let _ = std::fs::remove_file(&temp);
        result
    }

    fn is_real(&self) -> bool {
        true
    }
}

impl SubprocessCaseRunner<'_> {
    fn run_temp(&self, executable: &Path, prompt: &str) -> Result<String, String> {
        let output = std::process::Command::new(executable)
            .arg("--json")
            .arg(prompt)
            .output()
            .map_err(|error| format!("run {}: {error}", executable.display()))?;
        if !output.status.success() {
            return Err(format!(
                "the runner exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        final_answer(&stdout).ok_or_else(|| "the runner reported no final answer".to_owned())
    }
}

/// Picks the agent's answer out of `harw-agent-runner --json`'s event lines
/// (`iface::cli`'s `event_json`, one `harwness_sdk::SdkEvent` per line): the
/// root's final `message` if one arrived, else every root `text_delta`
/// concatenated (a provider that only streams, never sending a final
/// message item).
fn final_answer(json_lines: &str) -> Option<String> {
    let mut deltas = String::new();
    let mut message: Option<String> = None;
    for line in json_lines.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let is_root = event
            .get("source")
            .and_then(|source| source.get("parent"))
            .is_none_or(Value::is_null);
        if !is_root {
            continue;
        }
        match event.get("type").and_then(Value::as_str) {
            Some("text_delta") => {
                if let Some(text) = event.get("text").and_then(Value::as_str) {
                    deltas.push_str(text);
                }
            }
            Some("message") if event.get("final_answer") == Some(&Value::Bool(true)) => {
                message = event.get("text").and_then(Value::as_str).map(str::to_owned);
            }
            _ => {}
        }
    }
    message.or_else(|| (!deltas.is_empty()).then_some(deltas))
}

/// Expectations of a case.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Expectations {
    /// Tools that must be in the manifest.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Tools that must not be in the manifest.
    #[serde(default)]
    pub not_tools: Vec<String>,
    /// Substrings the answer must contain.
    #[serde(default)]
    pub contains: Vec<String>,
    /// Substrings the answer must not contain.
    #[serde(default)]
    pub not_contains: Vec<String>,
}

/// A case file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TestCase {
    /// Case name.
    pub name: String,
    /// The prompt.
    pub prompt: String,
    /// Expectations.
    #[serde(default)]
    pub expect: Expectations,
}

/// Outcome of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckStatus {
    /// Checked and fine.
    Passed,
    /// Checked and wrong.
    Failed,
    /// Needs the runner (wave 3).
    Pending,
}

/// One check of a case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CheckResult {
    /// What was checked.
    pub check: String,
    /// Result.
    pub status: CheckStatus,
}

/// One case's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CaseResult {
    /// The case file.
    pub file: PathBuf,
    /// Case name (or the file name if it does not parse).
    pub name: String,
    /// A parse error of the case file.
    pub error: Option<String>,
    /// The checks.
    pub checks: Vec<CheckResult>,
}

impl CaseResult {
    /// `true` without an error or a failed check.
    #[must_use]
    pub fn ok(&self) -> bool {
        self.error.is_none()
            && self
                .checks
                .iter()
                .all(|check| check.status != CheckStatus::Failed)
    }
}

/// The `tests/*.toml` files next to a definition, sorted.
#[must_use]
pub fn case_files(definition_dir: &Path) -> Vec<PathBuf> {
    let Ok(read_dir) = std::fs::read_dir(definition_dir.join("tests")) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("toml"))
        .collect();
    files.sort();
    files
}

/// Runs every case file against a compiled agent.
///
/// # Errors
/// Never for a bad case (it is reported in its result);
/// [`CompileError::Io`] is reserved for future runners.
pub fn run_cases(
    ir: &AgentIr,
    artifact: &Artifact,
    files: &[PathBuf],
    runner: &dyn CaseRunner,
) -> Result<Vec<CaseResult>, CompileError> {
    let mut results = Vec::new();
    for file in files {
        let fallback = file
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("case")
            .to_owned();
        let case = std::fs::read_to_string(file)
            .map_err(|error| error.to_string())
            .and_then(|text| toml::from_str::<TestCase>(&text).map_err(|error| error.to_string()));
        let case = match case {
            Ok(case) => case,
            Err(error) => {
                results.push(CaseResult {
                    file: file.clone(),
                    name: fallback,
                    error: Some(error),
                    checks: Vec::new(),
                });
                continue;
            }
        };
        let mut checks = Vec::new();
        for tool in &case.expect.tools {
            checks.push(CheckResult {
                check: format!("manifest admits {tool}"),
                status: status(ir.permissions.tools.contains(tool)),
            });
        }
        for tool in &case.expect.not_tools {
            checks.push(CheckResult {
                check: format!("manifest does not admit {tool}"),
                status: status(!ir.permissions.tools.contains(tool)),
            });
        }
        let has_answer_checks =
            !case.expect.contains.is_empty() || !case.expect.not_contains.is_empty();
        let answer = if has_answer_checks && runner.is_real() {
            Some(runner.run(artifact, &case.prompt))
        } else {
            None
        };
        for needle in &case.expect.contains {
            checks.push(CheckResult {
                check: format!("answer contains {needle:?}"),
                status: match &answer {
                    Some(Ok(text)) => status(text.contains(needle.as_str())),
                    Some(Err(_)) => CheckStatus::Failed,
                    None => CheckStatus::Pending,
                },
            });
        }
        for needle in &case.expect.not_contains {
            checks.push(CheckResult {
                check: format!("answer does not contain {needle:?}"),
                status: match &answer {
                    Some(Ok(text)) => status(!text.contains(needle.as_str())),
                    Some(Err(_)) => CheckStatus::Failed,
                    None => CheckStatus::Pending,
                },
            });
        }
        results.push(CaseResult {
            file: file.clone(),
            name: case.name,
            error: None,
            checks,
        });
    }
    Ok(results)
}

fn status(ok: bool) -> CheckStatus {
    if ok {
        CheckStatus::Passed
    } else {
        CheckStatus::Failed
    }
}

/// Text form of case results.
#[must_use]
pub fn render_cases(results: &[CaseResult], runner_is_real: bool) -> String {
    let mut out = String::new();
    if !runner_is_real
        && results.iter().any(|result| {
            result
                .checks
                .iter()
                .any(|c| c.status == CheckStatus::Pending)
        })
    {
        out.push_str("note: answer checks are pending: running the agent needs harw-agent-runner (#22 wave 3); only static checks ran\n");
    }
    for result in results {
        let mark = if result.ok() { "ok" } else { "FAILED" };
        out.push_str(&format!(
            "{mark:<6} {} ({})\n",
            result.name,
            result.file.display()
        ));
        if let Some(error) = &result.error {
            out.push_str(&format!("       error: {error}\n"));
        }
        for check in &result.checks {
            let label = match check.status {
                CheckStatus::Passed => "pass",
                CheckStatus::Failed => "FAIL",
                CheckStatus::Pending => "pending",
            };
            out.push_str(&format!("       {label:<7} {}\n", check.check));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_case_parses_and_rejects_unknown_keys() {
        let case: Result<TestCase, _> = toml::from_str(
            "name = \"n\"\nprompt = \"p\"\n[expect]\ntools = [\"fs.read\"]\ncontains = [\"x\"]\n",
        );
        assert!(case.is_ok());
        let bad: Result<TestCase, _> =
            toml::from_str("name = \"n\"\nprompt = \"p\"\nexpected = 1\n");
        assert!(bad.is_err());
    }

    #[test]
    fn test_stub_never_passes_answer_checks() {
        assert!(!EchoStub.is_real());
        let result = CaseResult {
            file: PathBuf::from("tests/a.toml"),
            name: "a".to_owned(),
            error: None,
            checks: vec![CheckResult {
                check: "answer contains \"x\"".to_owned(),
                status: CheckStatus::Pending,
            }],
        };
        assert!(result.ok(), "pending is not a failure");
        let text = render_cases(&[result], false);
        assert!(text.contains("wave 3"), "{text}");
        assert!(text.contains("pending"), "{text}");
    }

    #[test]
    fn test_final_answer_prefers_the_final_message_over_deltas() {
        let lines = "\
            {\"type\":\"text_delta\",\"source\":{\"parent\":null},\"text\":\"Hal\"}\n\
            {\"type\":\"text_delta\",\"source\":{\"parent\":null},\"text\":\"lo\"}\n\
            {\"type\":\"message\",\"source\":{\"parent\":null},\"text\":\"Hallo!\",\"final_answer\":true}\n\
            {\"type\":\"finished\",\"source\":{\"parent\":null},\"status\":\"completed\"}\n";
        assert_eq!(final_answer(lines).as_deref(), Some("Hallo!"));
    }

    #[test]
    fn test_final_answer_falls_back_to_concatenated_deltas() {
        let lines = "\
            {\"type\":\"text_delta\",\"source\":{\"parent\":null},\"text\":\"Hal\"}\n\
            {\"type\":\"text_delta\",\"source\":{\"parent\":null},\"text\":\"lo\"}\n";
        assert_eq!(final_answer(lines).as_deref(), Some("Hallo"));
    }

    #[test]
    fn test_final_answer_ignores_child_agent_events() {
        let lines = "\
            {\"type\":\"text_delta\",\"source\":{\"parent\":\"root\"},\"text\":\"nope\"}\n\
            {\"type\":\"message\",\"source\":{\"parent\":null},\"text\":\"ok\",\"final_answer\":true}\n";
        assert_eq!(final_answer(lines).as_deref(), Some("ok"));
    }

    #[test]
    fn test_final_answer_is_none_without_any_root_text() {
        assert_eq!(final_answer(""), None);
        assert_eq!(final_answer("not json\n"), None);
    }

    #[test]
    fn test_subprocess_runner_reports_a_missing_runner() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let env = CompilerEnv::isolated(dir.path().join("home"), dir.path().to_path_buf());
        let runner = SubprocessCaseRunner::new(&env);
        assert!(runner.is_real());
        let artifact =
            harw_agent_artifact::ArtifactBuilder::new(&serde_json::json!({"name": "demo"}))
                .build()?;
        let Err(error) = runner.run(&artifact, "hi") else {
            return Err("expected no runner to be installed".into());
        };
        assert!(error.contains("harw-agent-runner"), "{error}");
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn test_subprocess_runner_reports_pass_and_fail_through_a_fake_runner()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let home = dir.path().join("home");
        let env = CompilerEnv::isolated(home.clone(), dir.path().to_path_buf());
        let runner_path = env
            .home_runner_dir(&env.host_target)
            .join("harw-agent-runner");
        std::fs::create_dir_all(runner_path.parent().ok_or("parent")?)?;
        // A fake runner that echoes one `message` JSON line naming the
        // prompt it was given (its second positional argument: `--json
        // <prompt>`), then exits 0 — a real runner's `--json` shape for a
        // one-shot completed turn, just without an actual model behind it.
        // The explicit `exit 0` matters here: `run()` above appends the
        // artifact's raw bytes straight after this script via
        // `append_to_executable`, and `/bin/sh` parses a script file top to
        // bottom rather than stopping at some logical end, so without an
        // explicit exit it walks into the appended binary and can trip on a
        // stray byte that looks like shell syntax (e.g. `)`) — the same
        // reason `test_run_execs_a_bare_artifact_through_a_located_runner`
        // in `commands.rs` ends its fake runner with `exit 3`.
        let script = "#!/bin/sh\n\
            printf '{\"type\":\"message\",\"source\":{\"parent\":null},\"text\":\"echo: %s\",\"final_answer\":true}\\n' \"$2\"\n\
            exit 0\n";
        harw_agent_artifact::write_executable(&runner_path, script.as_bytes())?;
        let runner = SubprocessCaseRunner::new(&env);
        let artifact =
            harw_agent_artifact::ArtifactBuilder::new(&serde_json::json!({"name": "demo"}))
                .build()?;
        let answer = runner.run(&artifact, "hi there")?;
        assert_eq!(answer, "echo: hi there");
        Ok(())
    }
}
