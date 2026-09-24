//! Konkrete Sicherheitsregeln dieser Crate (Knoten AW4-03).
//!
//! # Verantwortungsbereich
//! Jedes Untermodul implementiert genau eine [`crate::rule::Rule`] mit
//! goldenen Testfällen (feste Eingabe, festes erwartetes Ergebnis). Drei der
//! vier im Arbeitsauftrag skizzierten Regeln sind hier gebaut:
//!
//! - [`EgressFlowRule`] — ein beobachteter ausgehender Fluss außerhalb des
//!   erlaubten [`harw_authority::NetworkScope`].
//! - [`StructureDriftRule`] — meldet jedes `EventKind::StructureDrift`-
//!   Ereignis (Schwere aus dem strukturierten `DriftSeverity`-Feld) sowie
//!   neue Metriken, die in keiner vorhandenen Baseline definiert sind.
//! - [`BaselineDeviationRule`] — demonstriert die Baseline-Regel
//!   (`Established` → `RuleTriggered`, `Provisional` → `Anomaly`) in einer
//!   vollständigen `Rule::evaluate`-Auswertung; siehe [`crate::baseline`]-
//!   Moduldoku für die Regel selbst.
//!
//! # Nebenläufigkeit
//! Jede Regel ist ein zustandsloser Unit-Struct: `Send + Sync`.
//!
//! # Fehler
//! Keine — siehe [`crate::rule`]-Moduldoku.

pub mod baseline_deviation;
pub mod egress_flow;
pub mod structure_drift;

pub use baseline_deviation::BaselineDeviationRule;
pub use egress_flow::EgressFlowRule;
pub use structure_drift::StructureDriftRule;

use crate::rule::Rule;

/// Alle in dieser Crate registrierten Regeln.
///
/// # Description
/// Wird von [`crate::engine::run_rules`] verwendet, um alle Regeln über
/// einem gemeinsamen [`RuleContext`] auszuführen.
#[must_use]
pub fn all_rules() -> &'static [&'static dyn Rule] {
    &[&EgressFlowRule, &StructureDriftRule, &BaselineDeviationRule]
}

/// Statisches Array aller Regeln für die parallele Ausführung.
pub const ALL_RULES: &[&dyn Rule] = &[&EgressFlowRule, &StructureDriftRule, &BaselineDeviationRule];
