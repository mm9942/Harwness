//! Positive integration test for the `metrics!` macro.
//!
//! Requires `harw-observe` (Vertragsabschnitt A.2, `MetricKey`/`MetricKind`/
//! `Unit`/`Cardinality`) as a dev-dependency; see `harw-macros/Cargo.toml`.
//!
//! # Design-doc reference
//! AW0-02-Brief, Abschnitt 2 (`metrics!`).

harw_macros::metrics! {
    /// Wie viele Kinder aufgenommen wurden.
    CHILD_ADMITTED: counter, unit = count, labels = ["clan", "role"], cardinality = bounded(64),
        name = "harw_child_admitted_total";
    /// Aktuelle Kontextkosten.
    CONTEXT_COST: gauge, unit = tokens, labels = [], cardinality = single,
        name = "harw_context_cost_tokens";
}

#[test]
fn metrics_macro_generates_expected_consts() {
    assert_eq!(CHILD_ADMITTED.name, "harw_child_admitted_total");
    assert_eq!(CHILD_ADMITTED.kind, harw_observe::MetricKind::Counter);
    assert_eq!(CHILD_ADMITTED.unit, harw_observe::Unit::Count);
    assert_eq!(CHILD_ADMITTED.labels.len(), 2);
    assert_eq!(CHILD_ADMITTED.labels[0].as_str(), "clan");
    assert_eq!(CHILD_ADMITTED.labels[1].as_str(), "role");
    assert_eq!(
        CHILD_ADMITTED.cardinality,
        harw_observe::Cardinality::Bounded(64)
    );
}

#[test]
fn metrics_macro_generates_second_entry_and_registration_slice() {
    assert_eq!(CONTEXT_COST.name, "harw_context_cost_tokens");
    assert_eq!(CONTEXT_COST.kind, harw_observe::MetricKind::Gauge);
    assert_eq!(CONTEXT_COST.unit, harw_observe::Unit::Tokens);
    assert!(CONTEXT_COST.labels.is_empty());
    assert_eq!(CONTEXT_COST.cardinality, harw_observe::Cardinality::Single);

    assert_eq!(ALL.len(), 2);
    assert_eq!(ALL[0].name, CHILD_ADMITTED.name);
    assert_eq!(ALL[1].name, CONTEXT_COST.name);
}
