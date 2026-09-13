//! Positive integration test for the `field!` macro.
//!
//! Requires `harw-observe` (Vertragsabschnitt A.1, `FieldName`) as a
//! dev-dependency; see `harw-macros/Cargo.toml`.
//!
//! # Design-doc reference
//! AW0-02-Brief, Abschnitt 1 (`field!`).

use harw_macros::field;

#[test]
fn field_macro_expands_to_valid_field_name() {
    let f = field!("child.admitted");
    assert_eq!(f.as_str(), "child.admitted");
}

#[test]
fn field_macro_accepts_multi_segment_names() {
    let f = field!("clan.role.id");
    assert_eq!(f.as_str(), "clan.role.id");
}
