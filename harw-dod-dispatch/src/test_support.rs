//! Test-Fehlertyp dieses Crates (siehe `harw-test-support`): ersetzt
//! `panic!`/`unwrap`/`expect` in Tests.

harw_test_support::define_test_error!(
    pub(crate),
    Json(serde_json::Error) => "JSON-Fehler",
    Dispatch(crate::DispatchError) => "Dispatch-Fehler"
);
