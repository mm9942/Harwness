//! Test error type of this crate: replaces panic!/unwrap/expect in tests.

use crate::error::{InfraClientError, InfraConfigError};
use crate::key::KeyRefError;

/// A failed test, returned as `Err` instead of panicking.
pub(crate) enum TestError {
    /// An expected value was missing (`Option` was `None`).
    Missing(&'static str),
    /// A result had an unexpected shape.
    Unexpected(String),
    /// A foreign error with context (replaces `expect("…")`).
    Context {
        context: &'static str,
        source: String,
    },
    /// Client call error.
    Client(InfraClientError),
    /// Configuration error.
    Config(InfraConfigError),
    /// Key reference error.
    KeyRef(KeyRefError),
    /// I/O error.
    Io(std::io::Error),
}

/// Result type of this crate's tests.
pub(crate) type TestResult<T = ()> = Result<T, TestError>;

impl std::fmt::Display for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TestError::Missing(what) => write!(f, "test: expected value missing: {what}"),
            TestError::Unexpected(detail) => write!(f, "test: unexpected result: {detail}"),
            TestError::Context { context, source } => write!(f, "test: {context}: {source}"),
            TestError::Client(err) => write!(f, "test: client error: {err}"),
            TestError::Config(err) => write!(f, "test: config error: {err}"),
            TestError::KeyRef(err) => write!(f, "test: key ref error: {err}"),
            TestError::Io(err) => write!(f, "test: I/O error: {err}"),
        }
    }
}

impl std::fmt::Debug for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for TestError {}

impl From<InfraClientError> for TestError {
    fn from(err: InfraClientError) -> Self {
        TestError::Client(err)
    }
}

impl From<InfraConfigError> for TestError {
    fn from(err: InfraConfigError) -> Self {
        TestError::Config(err)
    }
}

impl From<KeyRefError> for TestError {
    fn from(err: KeyRefError) -> Self {
        TestError::KeyRef(err)
    }
}

impl From<std::io::Error> for TestError {
    fn from(err: std::io::Error) -> Self {
        TestError::Io(err)
    }
}

/// Map a foreign error into [`TestError::Context`] (replaces `.expect("…")`).
pub(crate) fn ctx<E: std::fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |source| TestError::Context {
        context,
        source: source.to_string(),
    }
}
