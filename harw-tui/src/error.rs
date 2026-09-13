//! Presentation-facing command errors.

use crate::PermissionTier;
use std::fmt;

pub type CommandResult<T> = Result<T, CommandError>;

/// Errors returned before a command reaches a runtime handler.
#[derive(PartialEq, Eq)]
pub enum CommandError {
    InvalidCommandName {
        input: String,
    },
    UnknownCommand {
        input: String,
        suggestion: Option<String>,
    },
    UnterminatedQuote {
        quote: char,
    },
    InvalidEscape {
        input: String,
    },
    TrailingTokens {
        command: String,
        extra: Vec<String>,
    },
    ReservedPrefix {
        prefix: char,
    },
    PermissionDenied {
        command: String,
        required: PermissionTier,
        actual: PermissionTier,
    },
    TuiOnlyCommand {
        command: String,
    },
    ScopeReduced {
        command: String,
        reason: &'static str,
    },
    CapabilityDenied {
        capability: &'static str,
    },
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCommandName { input } => {
                write!(f, "'{input}' is not a valid command name")
            }
            Self::UnknownCommand { input, suggestion } => match suggestion {
                Some(suggestion) => write!(
                    f,
                    "unknown command '{input}'; did you mean '/{suggestion}'?"
                ),
                None => write!(f, "unknown command '{input}'"),
            },
            Self::UnterminatedQuote { quote } => {
                write!(f, "unterminated {quote} quote in command input")
            }
            Self::InvalidEscape { input } => {
                write!(f, "unsupported escape sequence '{input}' in command input")
            }
            Self::TrailingTokens { command, extra } => {
                write!(
                    f,
                    "'{command}' does not accept trailing tokens: {}",
                    extra.join(" ")
                )
            }
            Self::ReservedPrefix { prefix } => write!(f, "the '{prefix}' input prefix is reserved"),
            Self::PermissionDenied {
                command,
                required,
                actual,
            } => write!(
                f,
                "command '{command}' requires {required:?} permission; caller has {actual:?}"
            ),
            Self::TuiOnlyCommand { command } => {
                write!(
                    f,
                    "command '{command}' is available only in the terminal UI"
                )
            }
            Self::ScopeReduced { command, reason } => {
                write!(
                    f,
                    "command '{command}' is restricted on this channel: {reason}"
                )
            }
            Self::CapabilityDenied { capability } => {
                write!(f, "capability '{capability}' is not enabled")
            }
        }
    }
}

impl fmt::Debug for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for CommandError {}
