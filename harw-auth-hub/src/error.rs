//! Error type of the Auth/Crypto Hub process.
//!
//! These are *startup and transport* errors (config, token files, socket,
//! systemd activation, signal registration, the persistent key store, the
//! crypto worker thread). Per-request crypto errors never surface here: they
//! are mapped to HTTP statuses by `crypt_guard_hyper` without backend detail.

use std::fmt;
use std::io;
use std::path::PathBuf;

/// Everything that can stop the hub from starting or serving.
#[derive(Debug)]
#[non_exhaustive]
pub enum HubError {
    /// A file (config or token) could not be read.
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
    /// A file has permissions that are too wide for what it holds.
    InsecurePermissions {
        /// The file.
        path: PathBuf,
        /// Its mode bits (`st_mode & 0o7777`).
        mode: u32,
        /// What was expected.
        expected: &'static str,
    },
    /// A bearer token file is unusable (symlink, not a regular file, empty,
    /// too short, not printable ASCII). Its content is never included.
    TokenFile {
        /// The file.
        path: PathBuf,
        /// Why.
        reason: &'static str,
    },
    /// Socket setup failed.
    Socket {
        /// The socket path (or `<systemd>`).
        path: PathBuf,
        /// The I/O error.
        source: io::Error,
    },
    /// Another process is already accepting on the socket path.
    SocketInUse(PathBuf),
    /// The socket path exists and is not a socket; it is never removed.
    NotASocket(PathBuf),
    /// systemd socket activation was requested but the environment is
    /// missing, foreign or malformed.
    Systemd(String),
    /// Registering the shutdown signal handlers failed.
    Signal(io::Error),
    /// The Tokio runtime could not be started.
    Runtime(io::Error),
    /// The persistent key store could not be opened, created or locked.
    KeyStore(crate::sealed::SealedStoreError),
    /// The crypto worker thread could not be spawned.
    Worker(io::Error),
}

impl fmt::Display for HubError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read { path, source } => write!(f, "cannot read '{}': {source}", path.display()),
            Self::ConfigParse(message) => write!(f, "invalid config: {message}"),
            Self::ConfigInvalid(message) => write!(f, "invalid config: {message}"),
            Self::InsecurePermissions {
                path,
                mode,
                expected,
            } => write!(
                f,
                "insecure permissions {mode:04o} on '{}': expected {expected}",
                path.display()
            ),
            Self::TokenFile { path, reason } => {
                write!(f, "bearer token file '{}': {reason}", path.display())
            }
            Self::Socket { path, source } => {
                write!(f, "socket '{}': {source}", path.display())
            }
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
            Self::Runtime(source) => write!(f, "cannot start the Tokio runtime: {source}"),
            Self::KeyStore(source) => write!(f, "key store: {source}"),
            Self::Worker(source) => write!(f, "cannot spawn the crypto worker thread: {source}"),
        }
    }
}

impl std::error::Error for HubError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Read { source, .. }
            | Self::Socket { source, .. }
            | Self::Signal(source)
            | Self::Runtime(source)
            | Self::Worker(source) => Some(source),
            Self::KeyStore(source) => Some(source),
            _ => None,
        }
    }
}

impl From<crate::sealed::SealedStoreError> for HubError {
    fn from(source: crate::sealed::SealedStoreError) -> Self {
        Self::KeyStore(source)
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use crate::sealed::SealedStoreError;
    use crate::test_support::TestResult;

    use super::HubError;

    #[test]
    fn key_store_display_prefixes_inner_error() -> TestResult {
        let inner = SealedStoreError::Locked(std::path::PathBuf::from("/run/harw/store.sealed"));
        let error = HubError::from(inner);
        assert_eq!(
            error.to_string(),
            "key store: sealed key store: '/run/harw/store.sealed' is locked by another process"
        );
        Ok(())
    }

    #[test]
    fn key_store_source_is_inner_error() -> TestResult {
        let error = HubError::KeyStore(SealedStoreError::Authentication);
        assert!(matches!(
            error
                .source()
                .and_then(|source| source.downcast_ref::<SealedStoreError>()),
            Some(SealedStoreError::Authentication)
        ));
        Ok(())
    }

    #[test]
    fn worker_display_and_source() -> TestResult {
        let io_error = std::io::Error::other("no threads available");
        let error = HubError::Worker(io_error);
        assert_eq!(
            error.to_string(),
            "cannot spawn the crypto worker thread: no threads available"
        );
        assert!(error.source().is_some());
        Ok(())
    }
}
