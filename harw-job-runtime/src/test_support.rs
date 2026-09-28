//! Test error type of this crate: replaces `panic!`/`unwrap`/`expect` in
//! tests (Rust Coding Bible R087/R165/R182). Tests return [`TestResult`].

// The process scenarios that use every variant are Linux-only.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

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
}

/// Result of a test function or helper.
pub(crate) type TestResult<T = ()> = Result<T, TestError>;

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(what) => write!(f, "expected value missing: {what}"),
            Self::Unexpected(message) => write!(f, "unexpected result: {message}"),
            Self::Context { context, source } => write!(f, "{context}: {source}"),
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

/// Builds a `map_err` adapter that attaches context to a foreign error.
pub(crate) fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |error| TestError::Context {
        context,
        source: error.to_string(),
    }
}
