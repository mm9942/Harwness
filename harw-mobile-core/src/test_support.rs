//! Test error type of this crate (see `harw-test-support`): tests return
//! [`TestResult`] and report failures as `Err` instead of panicking.

harw_test_support::define_test_error!(pub(crate), Port(harw_protocol::PortError) => "port");

/// Fails the test when `condition` does not hold.
pub(crate) fn ensure(condition: bool, what: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(TestError::Unexpected(what.to_owned()))
    }
}
