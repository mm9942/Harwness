//! Konkrete Sicherheitsregeln dieser Crate (Knoten AW4-03, PL-100 WP1.6).
//!
//! # Verantwortungsbereich
//! Jedes Untermodul implementiert genau eine [`crate::rule::Rule`] mit
//! goldenen Testfällen (feste Eingabe, festes erwartetes Ergebnis):
//!
//! - [`EgressFlowRule`] — ein beobachteter ausgehender Fluss außerhalb des
//!   erlaubten [`harw_authority::NetworkScope`].
//! - [`StructureDriftRule`] — meldet jedes `EventKind::StructureDrift`-
//!   Ereignis (Schwere aus dem strukturierten `DriftSeverity`-Feld) sowie
//!   neue Metriken, die in keiner vorhandenen Baseline definiert sind.
//! - [`BaselineDeviationRule`] — ein Messwert außerhalb seiner Baseline,
//!   mit Status-Gate (`Established` → `RuleTriggered`, `Provisional` →
//!   `Anomaly`, `Superseded` → nichts). Eine doppelte aktive Metrik oder ein
//!   ungültiger Bereich wird nicht bewertet, sondern genau einmal als
//!   mehrdeutiger bzw. ungültiger Satz gemeldet (`Anomaly`, fail-closed); ein
//!   nicht endlicher Wert ergibt keinen Befund (PL-100 WP1.7); siehe
//!   [`crate::baseline`]-Moduldoku für die Regel selbst.
//! - [`ListenerNewRule`] — ein Listener, den die
//!   [`crate::listener_history::ListenerHistory`] noch nicht kennt (ein
//!   neuer Port im flüchtigen Bereich ohne bekanntes TCP nur
//!   `Low`/`Anomaly`; höchstens 32 Einzelbefunde je Besitzer und
//!   Auswertung, darüber ein Sammelbefund).
//! - [`ExecWritablePathRule`] — ein Programmstart aus einem welt- oder
//!   benutzerbeschreibbaren Ort, über einen Datei-Deskriptor oder über
//!   `/proc/<pid>/cwd` (nur das Temp-Verzeichnis eskaliert).
//! - [`SensorDegradedRule`] — ein Sensor hat sich als beeinträchtigt
//!   gemeldet.
//! - [`PersistenceWriteRule`] — ein Schreibzugriff auf einen Persistenz-Ort
//!   (systemd, cron/at, SysV-Init, udev, update-motd, Shell-Profile,
//!   Login-Umgebung, Autostart, Loader-Preload/-Konfiguration, SSH, sshd).
//!
//! Alle Regeln stempeln einen Befund mit der Zeit der auslösenden
//! Beobachtung (PL-100 WP0.2). Angreiferkontrollierter Text (Pfade,
//! Prozessnamen, Sensor-Kennungen) erscheint in den Zusammenfassungen der
//! WP1.6-Regeln nur über `harw_untrusted_text::escape_line`.
//!
//! # Nebenläufigkeit
//! Jede Regel ist ein zustandsloser Unit-Struct: `Send + Sync`. Zustand
//! (Listener-Historie) und Konfiguration (Workspace-Wurzeln, zusätzliche
//! Home-Verzeichnisse, flüchtiger Portbereich) kommen über den
//! [`RuleContext`](crate::rule::RuleContext).
//!
//! # Fehler
//! Keine — siehe [`crate::rule`]-Moduldoku.

pub mod baseline_deviation;
pub mod egress_flow;
pub mod exec_writable_path;
pub mod listener_new;
pub(crate) mod path_class;
pub mod persistence_write;
pub mod sensor_degraded;
pub mod structure_drift;

pub use baseline_deviation::BaselineDeviationRule;
pub use egress_flow::EgressFlowRule;
pub use exec_writable_path::ExecWritablePathRule;
pub use listener_new::ListenerNewRule;
pub use persistence_write::PersistenceWriteRule;
pub use sensor_degraded::SensorDegradedRule;
pub use structure_drift::StructureDriftRule;

use crate::rule::Rule;

/// Alle in dieser Crate registrierten Regeln.
///
/// # Description
/// Dieselbe Liste wie [`ALL_RULES`]; wird von [`crate::engine::run_rules`]
/// über einem gemeinsamen [`RuleContext`](crate::rule::RuleContext)
/// ausgeführt.
#[must_use]
pub fn all_rules() -> &'static [&'static dyn Rule] {
    ALL_RULES
}

/// Statisches Array aller Regeln für die parallele Ausführung.
///
/// # Description
/// Jede neue Regel gehört hierher **und** in `known_rule_id`
/// (`crate::finding`), sonst wird ein Record dieser Regel beim Lesen
/// abgelehnt; der Test `test_every_registered_rule_is_a_known_rule_id` in
/// [`crate::finding`] hält beide Listen deckungsgleich.
pub const ALL_RULES: &[&dyn Rule] = &[
    &EgressFlowRule,
    &StructureDriftRule,
    &BaselineDeviationRule,
    &ListenerNewRule,
    &ExecWritablePathRule,
    &SensorDegradedRule,
    &PersistenceWriteRule,
];
