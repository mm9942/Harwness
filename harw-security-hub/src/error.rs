//! Error types of the SecurityHub.
//!
//! [`HubError`] covers *startup and transport* failures (config, socket,
//! systemd activation, signal registration). Per-request outcomes are never `HubError`s: a policy
//! decision is a [`crate::policy::PolicyDenied`], and the HTTP layer maps it to
//! a status code without internal detail.

use std::fmt;
use std::io;
use std::path::PathBuf;

/// Everything that can stop the hub from starting or serving.
#[derive(Debug)]
#[non_exhaustive]
pub enum HubError {
    /// The config file could not be read.
    Read {
        /// The file.
        path: PathBuf,
        /// The I/O error.
        source: io::Error,
    },
    /// The config is not valid TOML or does not match the schema
    /// (unknown fields are rejected).
    ConfigParse(String),
    /// The config parsed but is semantically invalid.
    ConfigInvalid(String),
    /// The issuer capability could not be created.
    Issuer(String),
    /// Socket setup failed.
    Socket {
        /// The socket path.
        path: PathBuf,
        /// The I/O error.
        source: io::Error,
    },
    /// Another process is already accepting on the socket path.
    SocketInUse(PathBuf),
    /// The socket path exists and is not a socket; it is never removed.
    NotASocket(PathBuf),
    /// systemd socket activation was requested (`--systemd-socket`) but the
    /// environment is invalid or does not carry exactly one socket.
    Systemd(String),
    /// Registering the shutdown signal handlers failed.
    Signal(io::Error),
}

impl fmt::Display for HubError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => write!(f, "cannot read '{}': {source}", path.display()),
            Self::ConfigParse(message) | Self::ConfigInvalid(message) => {
                write!(f, "invalid config: {message}")
            }
            Self::Issuer(message) => write!(f, "cannot create context issuer: {message}"),
            Self::Socket { path, source } => write!(f, "socket '{}': {source}", path.display()),
            Self::SocketInUse(path) => write!(
                f,
                "socket '{}' is in use by a running process",
                path.display()
            ),
            Self::NotASocket(path) => write!(
                f,
                "'{}' exists and is not a socket; refusing to remove it",
                path.display()
            ),
            Self::Systemd(message) => write!(f, "systemd socket activation: {message}"),
            Self::Signal(source) => write!(f, "cannot register signal handler: {source}"),
        }
    }
}

impl std::error::Error for HubError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. } | Self::Socket { source, .. } | Self::Signal(source) => {
                Some(source)
            }
            _ => None,
        }
    }
}
