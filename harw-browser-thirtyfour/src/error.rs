//! Typed failures owned by the thirtyfour Firefox adapter.
//!
//! The adapter keeps backend-specific operation context here and converts it
//! into the stable `harw-browser` error vocabulary only at the public runtime
//! boundary.

use std::fmt;
use std::path::PathBuf;

/// Exact operation that failed while preparing a persistent Firefox profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileArchiveOperation {
    ReadDirectory,
    ReadEntry,
    WriteArchive,
    Encode,
}

impl fmt::Display for ProfileArchiveOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ReadDirectory => "read profile directory",
            Self::ReadEntry => "read profile entry",
            Self::WriteArchive => "write profile archive",
            Self::Encode => "encode profile archive",
        })
    }
}

/// The WebDriver lifecycle operation that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverOperation {
    ConfigureCapabilities,
    StartSession,
    ConnectBidi,
    ExecuteCommand,
    SubscribeEvents,
    ReadDriverLogs,
    QuitSession,
}

impl fmt::Display for DriverOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ConfigureCapabilities => "configure Firefox capabilities",
            Self::StartSession => "start session",
            Self::ConnectBidi => "connect WebDriver BiDi",
            Self::ExecuteCommand => "execute browser command",
            Self::SubscribeEvents => "subscribe to browser events",
            Self::ReadDriverLogs => "read driver logs",
            Self::QuitSession => "quit session",
        })
    }
}

/// A typed failure produced inside the Firefox/thirtyfour adapter.
#[derive(Debug)]
pub enum AdapterError {
    /// A non-timeout WebDriver or capability-construction operation failed.
    Driver {
        operation: DriverOperation,
        detail: String,
    },
    /// A bounded driver operation exceeded its deadline.
    DriverTimeout {
        operation: DriverOperation,
        detail: String,
    },
    /// The connected browser cannot provide a required adapter capability.
    CapabilityUnavailable {
        capability: &'static str,
        detail: String,
    },
    /// Persistent Firefox profile preparation failed before driver startup.
    ProfileArchive {
        directory: PathBuf,
        operation: ProfileArchiveOperation,
        detail: String,
    },
}

impl fmt::Display for AdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Driver { operation, detail } => {
                write!(formatter, "driver operation '{operation}' failed: {detail}")
            }
            Self::DriverTimeout { operation, detail } => {
                write!(
                    formatter,
                    "driver operation '{operation}' timed out: {detail}"
                )
            }
            Self::CapabilityUnavailable { capability, detail } => write!(
                formatter,
                "browser capability '{capability}' is unavailable: {detail}"
            ),
            Self::ProfileArchive {
                directory,
                operation,
                detail,
            } => write!(
                formatter,
                "could not {operation} at '{}': {detail}",
                directory.display()
            ),
        }
    }
}

impl std::error::Error for AdapterError {}

impl From<AdapterError> for harw_browser::Error {
    fn from(error: AdapterError) -> Self {
        match error {
            AdapterError::Driver { operation, detail } => match operation {
                DriverOperation::ConfigureCapabilities => Self::InvalidArgument {
                    detail: format!("could not {operation}: {detail}"),
                },
                DriverOperation::StartSession
                | DriverOperation::ConnectBidi
                | DriverOperation::ExecuteCommand
                | DriverOperation::SubscribeEvents
                | DriverOperation::ReadDriverLogs
                | DriverOperation::QuitSession => Self::CapabilityUnavailable {
                    detail: format!("Firefox adapter could not {operation}: {detail}"),
                },
            },
            AdapterError::DriverTimeout { operation, detail } => Self::Timeout {
                detail: format!("driver operation '{operation}' timed out: {detail}"),
            },
            AdapterError::CapabilityUnavailable { capability, detail } => {
                Self::CapabilityUnavailable {
                    detail: format!("{capability}: {detail}"),
                }
            }
            AdapterError::ProfileArchive {
                directory,
                operation: ProfileArchiveOperation::ReadDirectory,
                detail,
            } => Self::InvalidArgument {
                detail: format!(
                    "could not read Firefox profile directory '{}': {detail}",
                    directory.display()
                ),
            },
            AdapterError::ProfileArchive {
                directory,
                operation: ProfileArchiveOperation::ReadEntry,
                detail,
            } => Self::InvalidArgument {
                detail: format!(
                    "could not read an entry in Firefox profile directory '{}': {detail}",
                    directory.display()
                ),
            },
            AdapterError::ProfileArchive {
                directory,
                operation: ProfileArchiveOperation::WriteArchive,
                detail,
            } => Self::CapabilityUnavailable {
                detail: format!(
                    "could not write Firefox profile archive for '{}': {detail}",
                    directory.display()
                ),
            },
            AdapterError::ProfileArchive {
                directory,
                operation: ProfileArchiveOperation::Encode,
                detail,
            } => Self::CapabilityUnavailable {
                detail: format!(
                    "could not encode Firefox profile archive for '{}': {detail}",
                    directory.display()
                ),
            },
        }
    }
}
