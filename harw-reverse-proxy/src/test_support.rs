//! Panic-free test helpers: tests return [`TestResult`] and use [`ensure`].

use std::fmt;

/// Test failure.
pub(crate) struct TestError(pub String);

impl fmt::Debug for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<E: std::error::Error> From<E> for TestError {
    fn from(e: E) -> Self {
        Self(e.to_string())
    }
}

/// Result of a test function.
pub(crate) type TestResult = Result<(), TestError>;

/// Fails with `msg` when `cond` is false.
pub(crate) fn ensure(cond: bool, msg: &str) -> TestResult {
    if cond {
        Ok(())
    } else {
        Err(TestError(msg.to_owned()))
    }
}
