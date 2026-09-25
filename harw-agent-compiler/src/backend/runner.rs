//! Finding `harw-agent-runner` and asking it what it can do.
//!
//! # Lookup order
//! 1. `--runner <path>` (only this path; a missing file is an error);
//! 2. `harw-agent-runner` next to the running `harw` (only for the host
//!    target) — where `make install` and the release archive put it;
//! 3. the `bindir` of the install record (`~/.harw/install.toml`), if the
//!    record's target is the requested one;
//! 4. `~/.harw/bin/.runners/<target>/<version>/harw-agent-runner` (the copy
//!    `make install` leaves in the harw home);
//! 5. `~/.harw/runners/<target>/<version>/harw-agent-runner` (manually
//!    installed runners, e.g. for cross targets).
//!
//! # Capabilities contract (`harw-agent-runner --capabilities`)
//! The runner prints one JSON object on stdout and exits `0`:
//!
//! ```json
//! {
//!   "schema": "harwness.agent-runner.capabilities/v1",
//!   "runner_version": "0.3.0",
//!   "target": "x86_64-unknown-linux-gnu",
//!   "artifact_formats": [1],
//!   "ir_schema": "harwness.agent-ir/v2",
//!   "interfaces": ["cli", "repl", "mcp", "http", "tui"],
//!   "features": ["core", "tool-fs", "tool-doc", "..."],
//!   "child_protocol": "harwness.agent-child/v1"
//! }
//! ```
//!
//! The build fails unless the schema matches, `artifact_formats` contains
//! the artifact format version, `ir_schema` is the IR schema, and
//! `interfaces` and `features` cover what the agent needs. The runner does
//! not need an embedded artifact to answer; `--capabilities` is handled
//! before the footer lookup.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::env::{CompilerEnv, HARW_VERSION, RUNNER_BINARY};
use crate::error::CompileError;

/// Schema label of the capabilities answer.
pub const CAPABILITIES_SCHEMA: &str = "harwness.agent-runner.capabilities/v1";

/// The child-process protocol label a runner reports.
pub const CHILD_PROTOCOL: &str = "harwness.agent-child/v1";

/// Where a runner candidate comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RunnerSource {
    /// `--runner`.
    Flag,
    /// Next to the running `harw`.
    NextToHarw,
    /// The install record's `bindir`.
    InstallRecord,
    /// `~/.harw/bin/.runners/<target>/<version>`.
    HarwBin,
    /// `~/.harw/runners/<target>/<version>`.
    HarwRunners,
}

/// One candidate location.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunnerCandidate {
    /// Source.
    pub source: RunnerSource,
    /// Path of the executable.
    pub path: PathBuf,
}

/// The candidates in lookup order (see module docs).
#[must_use]
pub fn runner_candidates(
    env: &CompilerEnv,
    explicit: Option<&Path>,
    target: &str,
) -> Vec<RunnerCandidate> {
    if let Some(path) = explicit {
        return vec![RunnerCandidate {
            source: RunnerSource::Flag,
            path: env.resolve(path),
        }];
    }
    let binary = runner_file_name(target);
    let mut candidates = Vec::new();
    let harw_dir = env
        .current_exe
        .as_deref()
        .and_then(Path::parent)
        .filter(|_| target == env.host_target);
    if let Some(dir) = harw_dir {
        candidates.push(RunnerCandidate {
            source: RunnerSource::NextToHarw,
            path: dir.join(&binary),
        });
    }
    if let Some(bindir) = env
        .install_record()
        .filter(|record| record.target == target)
        .and_then(|record| record.bindir)
    {
        candidates.push(RunnerCandidate {
            source: RunnerSource::InstallRecord,
            path: bindir.join(&binary),
        });
    }
    candidates.push(RunnerCandidate {
        source: RunnerSource::HarwBin,
        path: env.bin_runner_dir(target).join(&binary),
    });
    candidates.push(RunnerCandidate {
        source: RunnerSource::HarwRunners,
        path: env.home_runner_dir(target).join(&binary),
    });
    let mut seen: Vec<PathBuf> = Vec::new();
    candidates.retain(|candidate| {
        if seen.contains(&candidate.path) {
            false
        } else {
            seen.push(candidate.path.clone());
            true
        }
    });
    candidates
}

/// The runner file name for a target (`.exe` on Windows targets).
#[must_use]
pub fn runner_file_name(target: &str) -> String {
    if target.contains("windows") {
        format!("{RUNNER_BINARY}.exe")
    } else {
        RUNNER_BINARY.to_owned()
    }
}

/// The first existing candidate.
///
/// # Errors
/// [`CompileError::RunnerNotFound`] naming every searched path.
pub fn locate_runner(
    env: &CompilerEnv,
    explicit: Option<&Path>,
    target: &str,
) -> Result<RunnerCandidate, CompileError> {
    let candidates = runner_candidates(env, explicit, target);
    candidates
        .iter()
        .find(|candidate| candidate.path.is_file())
        .cloned()
        .ok_or_else(|| CompileError::RunnerNotFound {
            target: target.to_owned(),
            searched: candidates
                .into_iter()
                .map(|candidate| candidate.path)
                .collect(),
        })
}

/// The parsed `--capabilities` answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerCapabilities {
    /// Always [`CAPABILITIES_SCHEMA`].
    pub schema: String,
    /// Runner version.
    pub runner_version: String,
    /// Target triple the runner was built for.
    pub target: String,
    /// Artifact format versions the runner reads.
    pub artifact_formats: Vec<u16>,
    /// IR schema the runner reads.
    pub ir_schema: String,
    /// Built-in interfaces.
    pub interfaces: Vec<String>,
    /// Enabled cargo features (tool providers and `core`).
    pub features: Vec<String>,
    /// Child-process protocol.
    #[serde(default)]
    pub child_protocol: Option<String>,
}

impl RunnerCapabilities {
    /// Parses the JSON answer.
    ///
    /// # Errors
    /// `Err(reason)` for invalid JSON or another schema.
    pub fn parse(text: &str) -> Result<Self, String> {
        let capabilities: Self = serde_json::from_str(text.trim())
            .map_err(|error| format!("`--capabilities` answered no valid JSON: {error}"))?;
        if capabilities.schema != CAPABILITIES_SCHEMA {
            return Err(format!(
                "`--capabilities` schema `{}`, expected `{CAPABILITIES_SCHEMA}`",
                capabilities.schema
            ));
        }
        Ok(capabilities)
    }

    /// Checks that the runner can run an agent with these interfaces and
    /// features. Returns the problems (empty: compatible).
    #[must_use]
    pub fn problems(&self, interfaces: &[String], features: &[String]) -> Vec<String> {
        let mut problems = Vec::new();
        if !self
            .artifact_formats
            .contains(&harw_agent_artifact::FORMAT_VERSION)
        {
            problems.push(format!(
                "it reads artifact formats {:?}, not {}",
                self.artifact_formats,
                harw_agent_artifact::FORMAT_VERSION
            ));
        }
        if self.ir_schema != harw_agent_dsl::AGENT_IR_SCHEMA {
            problems.push(format!(
                "it reads `{}`, not `{}`",
                self.ir_schema,
                harw_agent_dsl::AGENT_IR_SCHEMA
            ));
        }
        let missing_interfaces: Vec<&str> = interfaces
            .iter()
            .filter(|interface| !self.interfaces.contains(interface))
            .map(String::as_str)
            .collect();
        if !missing_interfaces.is_empty() {
            problems.push(format!(
                "it lacks the interfaces {}",
                missing_interfaces.join(", ")
            ));
        }
        let missing_features: Vec<&str> = features
            .iter()
            .filter(|feature| !self.features.contains(feature))
            .map(String::as_str)
            .collect();
        if !missing_features.is_empty() {
            problems.push(format!(
                "it lacks the tool providers {}",
                missing_features.join(", ")
            ));
        }
        problems
    }

    /// A note when the runner's version differs from harw's.
    #[must_use]
    pub fn version_note(&self) -> Option<String> {
        (self.runner_version != HARW_VERSION).then(|| {
            format!(
                "runner version {} differs from harw {HARW_VERSION}",
                self.runner_version
            )
        })
    }
}

/// Asks a runner for its capabilities.
pub trait RunnerProbe {
    /// Runs the runner's capability query.
    ///
    /// # Errors
    /// [`CompileError::RunnerIncompatible`] if the runner does not answer.
    fn capabilities(&self, runner: &Path) -> Result<RunnerCapabilities, CompileError>;
}

/// Runs `<runner> --capabilities` as a process.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessProbe;

impl RunnerProbe for ProcessProbe {
    fn capabilities(&self, runner: &Path) -> Result<RunnerCapabilities, CompileError> {
        let incompatible = |reason: String| CompileError::RunnerIncompatible {
            runner: runner.to_path_buf(),
            reason,
        };
        let output = std::process::Command::new(runner)
            .arg("--capabilities")
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|error| incompatible(format!("it cannot be started: {error}")))?;
        if !output.status.success() {
            return Err(incompatible(format!(
                "`--capabilities` exited with {} (runners before #22 wave 3 do not support it)",
                output.status
            )));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        RunnerCapabilities::parse(&text).map_err(incompatible)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::{INSTALL_RECORD_SCHEMA, InstallRecord};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn env_in(root: &Path) -> CompilerEnv {
        let mut env = CompilerEnv::isolated(root.join("home"), root.to_path_buf());
        env.current_exe = Some(root.join("usr-bin").join("harw"));
        env
    }

    #[test]
    fn test_lookup_order_for_the_host_target() {
        let root = Path::new("/r");
        let env = env_in(root);
        let target = env.host_target.clone();
        let sources: Vec<RunnerSource> = runner_candidates(&env, None, &target)
            .into_iter()
            .map(|candidate| candidate.source)
            .collect();
        assert_eq!(
            sources,
            [
                RunnerSource::NextToHarw,
                RunnerSource::HarwBin,
                RunnerSource::HarwRunners
            ]
        );
    }

    #[test]
    fn test_flag_wins_and_cross_targets_skip_the_harw_dir() {
        let env = env_in(Path::new("/r"));
        let flagged = runner_candidates(&env, Some(Path::new("my-runner")), &env.host_target);
        assert_eq!(flagged.len(), 1);
        assert_eq!(flagged[0].path, PathBuf::from("/r/my-runner"));
        let cross = runner_candidates(&env, None, "riscv64gc-unknown-linux-gnu");
        assert!(cross.iter().all(|c| c.source != RunnerSource::NextToHarw));
        assert!(cross[0].path.ends_with(format!(
            "bin/.runners/riscv64gc-unknown-linux-gnu/{HARW_VERSION}/harw-agent-runner"
        )));
    }

    #[test]
    fn test_locate_prefers_next_to_harw_then_install_record_then_home() -> TestResult {
        let root = tempfile::tempdir()?;
        let env = env_in(root.path());
        let target = env.host_target.clone();
        // Nothing installed: a clear error that names every location.
        let Err(CompileError::RunnerNotFound { searched, .. }) = locate_runner(&env, None, &target)
        else {
            return Err("expected RunnerNotFound".into());
        };
        assert_eq!(searched.len(), 3);

        // Only the home runner dir.
        let home_runner = env.home_runner_dir(&target).join(RUNNER_BINARY);
        std::fs::create_dir_all(home_runner.parent().ok_or("parent")?)?;
        std::fs::write(&home_runner, b"runner")?;
        assert_eq!(
            locate_runner(&env, None, &target)?.source,
            RunnerSource::HarwRunners
        );

        // The install record's bindir comes before the home dirs.
        let bindir = root.path().join("local-bin");
        std::fs::create_dir_all(&bindir)?;
        std::fs::write(bindir.join(RUNNER_BINARY), b"runner")?;
        InstallRecord {
            schema: INSTALL_RECORD_SCHEMA.to_owned(),
            version: HARW_VERSION.to_owned(),
            target: target.clone(),
            source_dir: None,
            bindir: Some(bindir.clone()),
        }
        .write(&env.home)?;
        assert_eq!(
            locate_runner(&env, None, &target)?.source,
            RunnerSource::InstallRecord
        );

        // Next to harw wins over everything but the flag.
        let next = root.path().join("usr-bin");
        std::fs::create_dir_all(&next)?;
        std::fs::write(next.join(RUNNER_BINARY), b"runner")?;
        assert_eq!(
            locate_runner(&env, None, &target)?.source,
            RunnerSource::NextToHarw
        );
        let flagged = locate_runner(&env, Some(&bindir.join(RUNNER_BINARY)), &target)?;
        assert_eq!(flagged.source, RunnerSource::Flag);
        Ok(())
    }

    #[test]
    fn test_capabilities_contract() -> Result<(), String> {
        let text = format!(
            r#"{{"schema":"{CAPABILITIES_SCHEMA}","runner_version":"{HARW_VERSION}","target":"x","artifact_formats":[1],"ir_schema":"harwness.agent-ir/v2","interfaces":["cli","mcp"],"features":["core","tool-fs"],"child_protocol":"{CHILD_PROTOCOL}"}}"#
        );
        let capabilities = RunnerCapabilities::parse(&text)?;
        assert!(
            capabilities
                .problems(
                    &["cli".to_owned()],
                    &["core".to_owned(), "tool-fs".to_owned()]
                )
                .is_empty()
        );
        let problems = capabilities.problems(&["http".to_owned()], &["tool-web".to_owned()]);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(capabilities.version_note().is_none());
        assert!(RunnerCapabilities::parse("{\"schema\":\"other\"}").is_err());
        assert!(RunnerCapabilities::parse("not json").is_err());
        Ok(())
    }
}
