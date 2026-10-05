//! Test error type: tests return [`TestResult`] and report failures as `Err`
//! instead of panicking (no `unwrap`/`expect`).

use std::fmt;

/// Failure of a test.
pub(crate) enum TestError {
    /// A result did not have the expected shape.
    Unexpected(String),
    /// A foreign error, e.g. a policy error from a constructor.
    Source(String),
}

impl fmt::Debug for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unexpected(m) => write!(f, "unexpected: {m}"),
            Self::Source(m) => write!(f, "source error: {m}"),
        }
    }
}

impl<E: std::error::Error> From<E> for TestError {
    fn from(e: E) -> Self {
        Self::Source(e.to_string())
    }
}

/// Result of a test function.
pub(crate) type TestResult = Result<(), TestError>;

/// Fails the test when `cond` does not hold.
pub(crate) fn ensure(cond: bool, msg: &str) -> TestResult {
    if cond {
        Ok(())
    } else {
        Err(TestError::Unexpected(msg.to_owned()))
    }
}
