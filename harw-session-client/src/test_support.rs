//! Test error type of this crate (see `harw-test-support`).

harw_test_support::define_test_error!(pub(crate));

/// Fails the test when `condition` does not hold.
pub(crate) fn ensure(condition: bool, what: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(TestError::Unexpected(what.to_owned()))
    }
}
