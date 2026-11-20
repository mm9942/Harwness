//! Test-Fehlertyp dieses Crates (siehe `harw-test-support`): ersetzt
//! `panic!`/`unwrap`/`expect` in Tests (Rust Coding Bible R087/R165/R182).

harw_test_support::define_test_error!(pub(crate));
