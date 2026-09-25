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
//! # The run is a stub until wave 3
//! Answer checks need the agent to run offline. Until `harw-agent-runner`
//! exists (#22 wave 3), [`EchoStub`] stands in: it does not run the agent
//! and every answer check is reported as `pending`, never as passed. Wave 3
//! implements [`CaseRunner`] with the runner library (offline echo
//! provider) and passes it to [`run_cases`].

use std::path::{Path, PathBuf};

use harw_agent_artifact::Artifact;
use harw_agent_dsl::ir_v2::AgentIr;
use serde::{Deserialize, Serialize};

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

/// The stand-in until the runner exists: runs nothing.
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
        self.error.is_none() && self.checks.iter().all(|check| check.status != CheckStatus::Failed)
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
    if !runner_is_real && results.iter().any(|result| result.checks.iter().any(|c| c.status == CheckStatus::Pending)) {
        out.push_str("note: answer checks are pending: running the agent needs harw-agent-runner (#22 wave 3); only static checks ran\n");
    }
    for result in results {
        let mark = if result.ok() { "ok" } else { "FAILED" };
        out.push_str(&format!("{mark:<6} {} ({})\n", result.name, result.file.display()));
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
        let bad: Result<TestCase, _> = toml::from_str("name = \"n\"\nprompt = \"p\"\nexpected = 1\n");
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
}
