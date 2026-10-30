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

impl CommandError {
    /// TUI projection; general Display remains independent of session locale.
    pub(crate) fn present_for(&self, language: harw_config::UiLanguage, typed: &str) -> String {
        use crate::i18n::tr_args_for;
        let t = |key, english, args: &[(&str, &str)]| tr_args_for(language, key, english, args);
        match self {
            Self::UnknownCommand {
                suggestion: Some(s),
                ..
            } => t(
                "dispatch.suggestion",
                "Unknown command: {command} (did you mean /{suggestion}?)",
                &[("command", typed), ("suggestion", s)],
            ),
            Self::UnknownCommand { .. } => t(
                "dispatch.unknown",
                "Unknown command: {command}",
                &[("command", typed)],
            ),
            Self::PermissionDenied {
                required, actual, ..
            } => t(
                "dispatch.denied",
                "Permission denied: {command} requires {required}; current tier is {actual}",
                &[
                    ("command", typed),
                    ("required", &format!("{required:?}")),
                    ("actual", &format!("{actual:?}")),
                ],
            ),
            Self::InvalidCommandName { input } => t(
                "command_feedback.invalid_name",
                "'{input}' is not a valid command name",
                &[("input", input)],
            ),
            Self::UnterminatedQuote { quote } => t(
                "command_feedback.quote",
                "Unterminated {quote} quote in command input",
                &[("quote", &quote.to_string())],
            ),
            Self::InvalidEscape { input } => t(
                "command_feedback.escape",
                "Unsupported escape sequence '{input}' in command input",
                &[("input", input)],
            ),
            Self::TrailingTokens { command, extra } => t(
                "command_feedback.trailing",
                "'{command}' does not accept trailing tokens: {extra}",
                &[("command", command), ("extra", &extra.join(" "))],
            ),
            Self::ReservedPrefix { prefix } => t(
                "command_feedback.reserved",
                "The '{prefix}' input prefix is reserved",
                &[("prefix", &prefix.to_string())],
            ),
            Self::TuiOnlyCommand { command } => t(
                "command_feedback.tui_only",
                "Command '{command}' is available only in the terminal UI",
                &[("command", command)],
            ),
            Self::ScopeReduced { command, reason } => t(
                "command_feedback.scope",
                "Command '{command}' is restricted on this channel: {reason}",
                &[("command", command), ("reason", reason)],
            ),
            Self::CapabilityDenied { capability } => t(
                "command_feedback.capability",
                "Capability '{capability}' is not enabled",
                &[("capability", capability)],
            ),
        }
    }
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
