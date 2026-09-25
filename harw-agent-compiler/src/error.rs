//! [`CompileError`]: everything that stops a compile besides diagnostics.

use std::fmt;
use std::path::PathBuf;

use harw_agent_artifact::ArtifactError;
use harw_agent_dsl::Diagnostics;

/// The one-line rustup installer the native backend suggests (never runs).
pub const RUSTUP_INSTALL_HINT: &str =
    "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh";

/// Errors of the compiler driver and the backends.
///
/// # Description
/// Definition problems are [`CompileError::Diagnostics`] (codes, spans,
/// help). Everything else is an environment or I/O problem with a message
/// that names the fix.
#[derive(Debug)]
pub enum CompileError {
    /// The definition has errors; the diagnostics carry codes and spans.
    Diagnostics(Diagnostics),
    /// No definition with this name in any layer.
    UnknownAgent {
        /// The name as given.
        name: String,
        /// Similar names from the layers.
        suggestions: Vec<String>,
    },
    /// A path argument does not name a definition.
    NotADefinition {
        /// The path.
        path: PathBuf,
        /// Why.
        reason: String,
    },
    /// No runner found for the default backend.
    RunnerNotFound {
        /// The target triple.
        target: String,
        /// Every location that was searched, in order.
        searched: Vec<PathBuf>,
    },
    /// The runner does not answer `--capabilities` or lacks interfaces.
    RunnerIncompatible {
        /// The runner.
        runner: PathBuf,
        /// Why.
        reason: String,
    },
    /// No Rust toolchain for `--native`.
    ToolchainMissing {
        /// Where `cargo` was looked for.
        searched: Vec<PathBuf>,
    },
    /// No harw sources for `--native`.
    SourcesMissing {
        /// The candidate that was checked, if any.
        candidate: Option<PathBuf>,
        /// Why it does not qualify.
        reason: String,
    },
    /// `cargo build` failed.
    CargoFailed {
        /// Exit status as text.
        status: String,
        /// The crate directory.
        crate_dir: PathBuf,
    },
    /// The artifact could not be built or read.
    Artifact(ArtifactError),
    /// The artifact's bundle layout (pool, agent entries) is invalid.
    Bundle(harw_agent_artifact::BundleError),
    /// Reading or writing a file failed.
    Io {
        /// What was being done, including the path.
        context: String,
        /// The underlying error.
        source: std::io::Error,
    },
    /// Anything else, with a complete message.
    Other(String),
}

impl CompileError {
    /// Builds an [`CompileError::Io`] with a context message.
    pub fn io(context: impl Into<String>) -> impl FnOnce(std::io::Error) -> Self {
        let context = context.into();
        move |source| Self::Io { context, source }
    }

    /// The diagnostics, if this is a definition error.
    #[must_use]
    pub fn diagnostics(&self) -> Option<&Diagnostics> {
        match self {
            Self::Diagnostics(diagnostics) => Some(diagnostics),
            _ => None,
        }
    }

    /// Stable kind label for JSON output.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Diagnostics(_) => "diagnostics",
            Self::UnknownAgent { .. } => "unknown-agent",
            Self::NotADefinition { .. } => "not-a-definition",
            Self::RunnerNotFound { .. } => "runner-not-found",
            Self::RunnerIncompatible { .. } => "runner-incompatible",
            Self::ToolchainMissing { .. } => "toolchain-missing",
            Self::SourcesMissing { .. } => "sources-missing",
            Self::CargoFailed { .. } => "cargo-failed",
            Self::Artifact(_) => "artifact",
            Self::Bundle(_) => "bundle",
            Self::Io { .. } => "io",
            Self::Other(_) => "other",
        }
    }
}

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Diagnostics(diagnostics) => {
                let errors = diagnostics.iter().filter(|d| d.is_error()).count();
                write!(f, "the definition has {errors} error(s)")
            }
            Self::UnknownAgent { name, suggestions } => {
                write!(f, "no agent definition named `{name}` in any layer")?;
                if !suggestions.is_empty() {
                    write!(f, "; did you mean: {}", suggestions.join(", "))?;
                }
                f.write_str(" (`harw agent list` shows every definition)")
            }
            Self::NotADefinition { path, reason } => {
                write!(f, "{} is not an agent definition: {reason}", path.display())
            }
            Self::RunnerNotFound { target, searched } => {
                write!(
                    f,
                    "no harw-agent-runner for target {target}; searched: {}. \
                     Run `make install` in the harw sources (it installs the runner next to `harw`), \
                     pass `--runner <path>`, build with `--native`, or write only the artifact with `--artifact-only`",
                    display_paths(searched)
                )
            }
            Self::RunnerIncompatible { runner, reason } => write!(
                f,
                "the runner {} cannot run this agent: {reason}. \
                 Reinstall the full runner (`make install`) or build with `--native`",
                runner.display()
            ),
            Self::ToolchainMissing { searched } => write!(
                f,
                "`--native` needs a Rust toolchain, but `cargo` was not found (searched: {}). \
                 Install it with: {RUSTUP_INSTALL_HINT}  (harw never installs it for you). \
                 Without a toolchain, build without `--native`",
                display_paths(searched)
            ),
            Self::SourcesMissing { candidate, reason } => {
                f.write_str("`--native` needs the harw sources")?;
                if let Some(candidate) = candidate {
                    write!(f, " ({} checked)", candidate.display())?;
                }
                write!(
                    f,
                    ": {reason}. `make install` records them in ~/.harw/install.toml; \
                     otherwise pass `--harw-src <dir>` or set HARW_SRC"
                )
            }
            Self::CargoFailed { status, crate_dir } => write!(
                f,
                "`cargo build --release` failed ({status}); the generated crate is in {}",
                crate_dir.display()
            ),
            Self::Artifact(error) => write!(f, "agent artifact: {error}"),
            Self::Bundle(error) => write!(f, "agent artifact layout: {error}"),
            Self::Io { context, source } => write!(f, "{context}: {source}"),
            Self::Other(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for CompileError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Artifact(error) => Some(error),
            Self::Bundle(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            Self::Diagnostics(diagnostics) => Some(diagnostics),
            _ => None,
        }
    }
}

impl From<ArtifactError> for CompileError {
    fn from(error: ArtifactError) -> Self {
        Self::Artifact(error)
    }
}

impl From<Diagnostics> for CompileError {
    fn from(diagnostics: Diagnostics) -> Self {
        Self::Diagnostics(diagnostics)
    }
}

fn display_paths(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return "nothing".to_owned();
    }
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
