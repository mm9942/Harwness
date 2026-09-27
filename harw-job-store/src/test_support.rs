//! Test error type of this crate: replaces `panic!`/`unwrap`/`expect` in
//! tests (Rust Coding Bible R087/R165/R182). Tests return [`TestResult`].

use std::fmt;

/// Failure of a test; every failure is an `Err`, never a panic.
pub(crate) enum TestError {
    /// An expected value was absent.
    Missing(&'static str),
    /// A result had an unexpected shape.
    Unexpected(String),
    /// A foreign error with context (replaces `expect("…")`).
    Context {
        /// What was being attempted.
        context: &'static str,
        /// Rendered source error.
        source: String,
    },
    /// I/O error from test fixtures.
    Io(std::io::Error),
    /// JSON error from test fixtures.
    Json(serde_json::Error),
    /// Error of the crate under test.
    Store(crate::error::StoreError),
}

/// Result of a test function or helper.
pub(crate) type TestResult<T = ()> = Result<T, TestError>;

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(what) => write!(f, "expected value missing: {what}"),
            Self::Unexpected(message) => write!(f, "unexpected result: {message}"),
            Self::Context { context, source } => write!(f, "{context}: {source}"),
            Self::Io(error) => write!(f, "I/O error in test: {error}"),
            Self::Json(error) => write!(f, "JSON error in test: {error}"),
            Self::Store(error) => write!(f, "store error in test: {error}"),
        }
    }
}

// Debug delegates to Display (Bible R081) so failing tests stay readable.
impl fmt::Debug for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for TestError {}

impl From<std::io::Error> for TestError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for TestError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

impl From<crate::error::StoreError> for TestError {
    fn from(error: crate::error::StoreError) -> Self {
        Self::Store(error)
    }
}

/// Builds a `map_err` adapter that attaches context to a foreign error.
pub(crate) fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |error| TestError::Context {
        context,
        source: error.to_string(),
    }
}
