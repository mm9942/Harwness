/// Compile-fail test suite for the `#[derive(FromRawArgs)]` hardened diagnostics.
///
/// Each sub-file in `tests/compile_fail/*.rs` triggers exactly one of the seven
/// compile-time diagnostics added to `expand_from_raw_args`. The `.stderr`
/// snapshot files are generated on the first run via `TRYBUILD=overwrite`.
///
/// # Design-doc reference
/// Spec section "FromRawArgs-Derive-Makro — compile-fail suite".
#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/*.rs");
}
