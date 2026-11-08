//! Test-Fehlertyp dieses Crates (siehe `harw-test-support`): ersetzt
//! `panic!`/`unwrap`/`expect` in Tests (Rust Coding Bible R087/R165/R182).
//!
//! Dazu [`private_tempdir`]: ein Tempdir mit Modus `0700`, wie es
//! `WardenState::open` für das StateDirectory verlangt (`tempfile::tempdir`
//! legt unter der üblichen umask `0022` ein `0755`-Verzeichnis an).

harw_test_support::define_test_error!(pub(crate));

/// Creates a temporary directory that only its owner may access (`0700`).
///
/// # Errors
/// [`TestError::Context`], wenn das Verzeichnis nicht angelegt werden kann.
pub(crate) fn private_tempdir() -> TestResult<tempfile::TempDir> {
    use std::os::unix::fs::PermissionsExt as _;

    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir()
        .map_err(ctx("private tempdir"))
}
