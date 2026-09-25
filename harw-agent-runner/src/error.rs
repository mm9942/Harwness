//! `RunnerError`: the one error type of the `harw-agent-runner` binary.
//!
//! # Description
//! Every failure that stops the runner before an interface takes over maps
//! to a sysexits-style exit code ([`RunnerError::exit_code`]), fail-closed:
//! a tampered or unreadable artifact never falls through to running an
//! agent. `Display`/`std::error::Error` are hand-written (repo style, no
//! derive macro) so the message stays a single readable line.

use std::fmt;
use std::io;

use harw_agent_artifact::ArtifactError;
use harw_agent_artifact::bundle::BundleError;
use harwness_sdk::SdkError;

/// Invalid `--flag` usage or an unusable combination of flags (BSD `EX_USAGE`).
pub const EXIT_USAGE: u8 = 64;
/// The embedded artifact is missing, malformed or tampered (BSD `EX_DATAERR`).
pub const EXIT_DATA: u8 = 65;
/// A dependent subsystem (the SDK, the runtime assembly) failed
/// (BSD `EX_SOFTWARE`).
pub const EXIT_SOFTWARE: u8 = 70;
/// A file or socket could not be read or written (BSD `EX_IOERR`).
pub const EXIT_IO: u8 = 74;

/// A failure of the runner itself, before or instead of running an agent.
#[derive(Debug)]
pub enum RunnerError {
    /// No artifact is embedded in the running executable, or its footer,
    /// header or hashes do not check out.
    Artifact(ArtifactError),
    /// The embedded artifact's payload layout is not a valid bundle.
    Bundle(BundleError),
    /// `--interface` named, or the manifest's `[binary]` chose, an
    /// interface this runner build was not compiled with, or that the
    /// agent's `[binary] interfaces` does not list.
    UnknownInterface {
        /// The interface label that was requested.
        requested: String,
        /// The interfaces this agent's manifest allows, in order.
        allowed: Vec<String>,
    },
    /// The embedded manifest names no usable interface at all (an empty
    /// `[binary] interfaces` with no `--interface` override).
    NoInterface,
    /// The chosen interface is one the manifest allows, but this runner
    /// binary was not compiled with its feature.
    NotCompiled {
        /// The interface label that was requested.
        requested: String,
        /// The interfaces this runner build was compiled with.
        compiled: Vec<String>,
    },
    /// Assembling the embedded agent from the verified bundle failed
    /// (`harw_runtime::embedded::EmbeddedAgent::from_bundle`).
    Runtime(harw_runtime::RuntimeError),
    /// `--child` was given without `--child-protocol`, or another flag
    /// combination the parser cannot make sense of.
    Usage(String),
    /// Building the embedding SDK or the runtime assembly failed.
    Sdk(SdkError),
    /// Reading the executable, a manifest file or a socket failed.
    Io {
        /// What was being done.
        context: &'static str,
        /// The underlying error.
        source: io::Error,
    },
    /// A `--manifest`/`--capabilities` JSON payload could not be encoded.
    Json(serde_json::Error),
}

impl RunnerError {
    /// The process exit code this failure maps to.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Artifact(_) | Self::Bundle(_) => EXIT_DATA,
            Self::UnknownInterface { .. }
            | Self::NoInterface
            | Self::NotCompiled { .. }
            | Self::Usage(_) => EXIT_USAGE,
            Self::Sdk(_) | Self::Runtime(_) => EXIT_SOFTWARE,
            Self::Io { .. } => EXIT_IO,
            Self::Json(_) => EXIT_DATA,
        }
    }
}

impl fmt::Display for RunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Artifact(error) => write!(f, "embedded artifact: {error}"),
            Self::Bundle(error) => write!(f, "embedded bundle: {error}"),
            Self::UnknownInterface { requested, allowed } => write!(
                f,
                "interface `{requested}` is not available; this agent allows: {}",
                if allowed.is_empty() {
                    "(none)".to_owned()
                } else {
                    allowed.join(", ")
                }
            ),
            Self::NoInterface => {
                f.write_str("the embedded manifest names no interface and none was requested")
            }
            Self::NotCompiled {
                requested,
                compiled,
            } => write!(
                f,
                "interface `{requested}` is not compiled into this runner build; it has: {}",
                if compiled.is_empty() {
                    "(none)".to_owned()
                } else {
                    compiled.join(", ")
                }
            ),
            Self::Usage(detail) => write!(f, "usage: {detail}"),
            Self::Sdk(error) => write!(f, "{error}"),
            Self::Runtime(error) => write!(f, "{error}"),
            Self::Io { context, source } => write!(f, "{context}: {source}"),
            Self::Json(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for RunnerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Artifact(error) => Some(error),
            Self::Bundle(error) => Some(error),
            Self::Sdk(error) => Some(error),
            Self::Runtime(error) => Some(error),
            Self::Io { source, .. } => Some(source),
            Self::Json(error) => Some(error),
            Self::UnknownInterface { .. }
            | Self::NoInterface
            | Self::NotCompiled { .. }
            | Self::Usage(_) => None,
        }
    }
}

impl From<ArtifactError> for RunnerError {
    fn from(error: ArtifactError) -> Self {
        Self::Artifact(error)
    }
}

impl From<BundleError> for RunnerError {
    fn from(error: BundleError) -> Self {
        Self::Bundle(error)
    }
}

impl From<SdkError> for RunnerError {
    fn from(error: SdkError) -> Self {
        Self::Sdk(error)
    }
}

impl From<serde_json::Error> for RunnerError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn json_error() -> Result<serde_json::Error, Box<dyn std::error::Error>> {
        match serde_json::from_str::<()>("not json") {
            Ok(()) => Err("expected a JSON parse error".into()),
            Err(error) => Ok(error),
        }
    }

    #[test]
    fn test_exit_codes_are_sysexits_style() -> TestResult {
        let cases: [(RunnerError, u8); 5] = [
            (
                RunnerError::UnknownInterface {
                    requested: "http".to_owned(),
                    allowed: vec!["cli".to_owned()],
                },
                EXIT_USAGE,
            ),
            (RunnerError::NoInterface, EXIT_USAGE),
            (
                RunnerError::Usage("no --child-protocol".to_owned()),
                EXIT_USAGE,
            ),
            (
                RunnerError::Io {
                    context: "open executable",
                    source: io::Error::other("nope"),
                },
                EXIT_IO,
            ),
            (RunnerError::Json(json_error()?), EXIT_DATA),
        ];
        for (error, expected) in cases {
            assert_eq!(error.exit_code(), expected, "{error}");
        }
        Ok(())
    }

    #[test]
    fn test_display_names_the_interface_problem() {
        let error = RunnerError::UnknownInterface {
            requested: "http".to_owned(),
            allowed: vec!["cli".to_owned(), "repl".to_owned()],
        };
        let message = error.to_string();
        assert!(message.contains("http"), "{message}");
        assert!(message.contains("cli"), "{message}");
    }
}
