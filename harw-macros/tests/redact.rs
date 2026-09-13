//! Positive integration test for `#[derive(Redact)]`.
//!
//! Requires `harw-observe` (Vertragsabschnitt A.5, `Redact`/`Redacted`) and
//! `blake3` as dev-dependencies; see `harw-macros/Cargo.toml`.
//!
//! # Design-doc reference
//! AW0-02-Brief, Abschnitt 3 (`#[derive(Redact)]`).

use harw_macros::Redact;
use harw_observe::{Redact as _, Redacted};

#[derive(Redact)]
struct ChildEvent {
    #[redact(show)]
    clan: String,
    #[redact(hash)]
    token: String,
    // Kein Attribut: darf niemals in der Ausgabe erscheinen.
    //
    // `dead_code` ist hier ein falsches Signal, und zwar ein aufschlussreiches:
    // dass dieses Feld nie gelesen wird, **ist** die geprüfte Eigenschaft.
    // `#[derive(Redact)]` darf es nicht anfassen, und der Test unten prüft,
    // dass `do-not-log` in der Ausgabe nicht vorkommt. Wegzulöschen nähme dem
    // Test seinen Gegenstand.
    #[allow(dead_code)]
    secret: String,
}

#[test]
fn redact_derive_shows_hashes_and_omits_fields() {
    let event = ChildEvent {
        clan: "north".to_owned(),
        token: "abc123".to_owned(),
        secret: "do-not-log".to_owned(),
    };

    match event.redact() {
        Redacted::Shown(rendered) => {
            assert!(rendered.contains("clan: north"));
            assert!(rendered.contains("token: "));
            assert!(!rendered.contains("abc123"));
            assert!(!rendered.contains("do-not-log"));
            assert!(!rendered.contains("secret"));
        }
        other => panic!("expected Redacted::Shown, got {other:?}"),
    }
}

#[test]
fn redact_hash_is_deterministic_and_truncated_to_16_chars() {
    let a = ChildEvent {
        clan: "north".to_owned(),
        token: "same-token".to_owned(),
        secret: "x".to_owned(),
    };
    let b = ChildEvent {
        clan: "south".to_owned(),
        token: "same-token".to_owned(),
        secret: "y".to_owned(),
    };

    let extract_hash = |rendered: &str| -> String {
        rendered
            .split("token: ")
            .nth(1)
            .and_then(|rest| rest.split(&[',', '}'][..]).next())
            .expect("token field must be present")
            .trim()
            .to_owned()
    };

    let (Redacted::Shown(a_rendered), Redacted::Shown(b_rendered)) = (a.redact(), b.redact())
    else {
        panic!("expected both to be Redacted::Shown");
    };

    let a_hash = extract_hash(&a_rendered);
    let b_hash = extract_hash(&b_rendered);

    assert_eq!(a_hash.len(), 16);
    assert_eq!(a_hash, b_hash, "same token must hash to the same value");
}

#[derive(Redact)]
struct Empty;

#[test]
fn redact_unit_struct_shows_empty_braces() {
    match Empty.redact() {
        Redacted::Shown(rendered) => assert_eq!(rendered, "Empty {}"),
        other => panic!("expected Redacted::Shown, got {other:?}"),
    }
}
