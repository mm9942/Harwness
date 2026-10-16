//! Test error type (see `harw-test-support`).

harw_test_support::define_test_error!(pub(crate), Io(std::io::Error) => "I/O");
