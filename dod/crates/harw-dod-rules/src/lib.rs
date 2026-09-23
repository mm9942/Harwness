//! Regeln, Baselines und die Geburt eines Befunds (Knoten AW4-03,
//! Contract-Master `docs/aw-contract-master.md` §G.1).
//!
//! # Zweck
//! Diese Crate ist der Ort, an dem aus Beobachtungen — `HostSample` und
//! `SecurityEvent` aus `harw-dod-signals` — **Befunde** werden. Sie besitzt:
//! - [`Finding<S>`] und alle drei Typestate-Übergänge (Modul [`finding`]) —
//!   siehe unten, warum sie hier liegen und nicht in `harw-dod-signals`.
//! - `trait` [`rule::Rule`] und [`rule::RuleContext`] (Modul [`rule`]) — die
//!   reine Schnittstelle, die aus Beobachtungen Befunde macht.
//! - [`engine::run_rules`] (Modul [`engine`]) — führt Regeln aus und
//!   zertifiziert ihre Befunde mit einer stabilen Identität.
//! - Drei konkrete Regeln (Modul [`rules`]): [`rules::EgressFlowRule`],
//!   [`rules::StructureDriftRule`], [`rules::BaselineDeviationRule`] — siehe
//!   [`rules`]-Moduldoku für die zwei Regeln, die hier absichtlich fehlen,
//!   und warum.
//! - Den Baseline-Zugriff und die Baseline-Gate-Regel (Modul [`baseline`]).
//! - Die eine echte Konfidenz-Konvertierung im Baum (Modul [`confidence`]).
//! - [`advisory::correlate_advisories`] (Modul [`advisory`], Knoten AW7-06):
//!   den Intel-Scout-Join zwischen hereingereichten Advisories und dem
//!   tatsächlich gebauten `Cargo.lock` — siehe dortige Moduldoku, warum diese
//!   Funktion bewusst keine [`rule::Rule`] ist.
//!
//! # Warum `Finding<S>` hier lebt und nicht in `harw-dod-signals`
//! Der ursprüngliche Arbeitsplan legte `Finding<Raw|RuleChecked|Triaged>`
//! nach `harw-dod-signals`, mit `pub(crate)`-Konstruktoren dort. Das ist
//! unbaubar: von dort aus wären die Konstruktoren für die elf Sensor-Crates,
//! die `harw-dod-signals` einzeln importieren, unerreichbar, und das
//! Typestate-Muster funktioniert nur, wenn Typbesitzer und Übergangsbesitzer
//! dieselbe Crate sind. **Verbindlich (Contract-Master §G.1):** `Finding<S>`
//! und alle drei Übergänge leben in dieser Crate. `harw-dod-signals` enthält
//! kein `Finding` — wer es sucht, findet es hier. **Sensoren minten nie einen
//! Befund**; sie emittieren `HostSample` und `SecurityEvent`, aus denen
//! ausschließlich diese Crate ein `Finding` macht. Siehe [`finding`]-Moduldoku
//! für die vollständige Begründung, einschließlich der Analogie zu einer
//! Stelle, an der dieses Projekt genau diese Bewegung — Unausdrückbarkeit
//! statt Laufzeit-Check — bereits einmal verpasst hat.
//!
//! # Reinheitsauflage
//! Jede [`rule::Rule::evaluate`]-Implementierung ist eine reine Funktion:
//! kein I/O, kein Netz, kein Subprozess, **keine Systemuhr**. `now` kommt
//! ausschließlich über [`rule::RuleContext::now`] injiziert. Diese Auflage
//! ist der Grund, warum jede Regel in [`rules`] einen Determinismus-Test
//! trägt: zweimal mit demselben `RuleContext` ausgewertet, ergibt sie
//! byteidentische Befunde.
//!
//! # Die Baseline-Regel
//! Eine `Established`-Baseline darf `FindingKind::RuleTriggered` auslösen,
//! eine `Provisional`-Baseline nur `FindingKind::Anomaly`. Der Unterschied
//! ist der ganze Zweck des Status-Feldes: eine unbestätigte Beobachtung darf
//! keine Eskalation auslösen, sonst wird die Eskalationsleiter durch Rauschen
//! entwertet. Siehe [`baseline`]-Moduldoku für die Regel selbst
//! ([`baseline::finding_kind_for_status`]) und [`rules::BaselineDeviationRule`]
//! für ihre Verdrahtung in eine vollständige Regelauswertung.
//!
//! # Nebenläufigkeit
//! Alle Typen dieser Crate sind reine Werte ohne innere Veränderlichkeit:
//! `Send + Sync`. [`rule::Rule`] verlangt `Send + Sync` als Supertrait, damit
//! [`engine::run_rules`] mehrere Regeln parallel über denselben,
//! unveränderlichen [`rule::RuleContext`] laufen lassen könnte, ohne Sperren.
//!
//! # Fehler
//! Diese Crate definiert keinen eigenen Fehlertyp: jede Operation — Regeln,
//! Typestate-Übergänge, die Konfidenz-Konvertierung, das Baseline-Gate — ist
//! total. Wo eine Regel nichts findet, liefert sie einen leeren `Vec`, nie
//! einen Fehler.
//!
//! # Examples
//! ```rust
//! use harw_authority::NetworkScope;
//! use harw_dod_rules::rule::{Rule, RuleContext};
//! use harw_dod_rules::rules::EgressFlowRule;
//! use harw_dod_rules::{run_rules, triage, Verdict};
//! use harw_dod_signals::{EventKind, SecurityEvent};
//! use harw_types::SensorId;
//!
//! let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);
//! let events = vec![SecurityEvent {
//!     sensor: SensorId::from_str("net-0"),
//!     observed_at: jiff::Timestamp::UNIX_EPOCH,
//!     actor: None,
//!     kind: EventKind::EgressFlow {
//!         destination: "evil.example.com".to_owned(),
//!         port: 443,
//!     },
//! }];
//! let ctx = RuleContext {
//!     now: jiff::Timestamp::UNIX_EPOCH,
//!     samples: &[],
//!     events: &events,
//!     baselines: &[],
//!     network_scope: &scope,
//! };
//!
//! let rule: &dyn Rule = &EgressFlowRule;
//! let checked = run_rules(&[rule], &ctx);
//! let finding = checked.into_iter().next().expect("EgressFlowRule löst aus");
//! let triaged = triage(finding, Verdict::Confirmed);
//! assert_eq!(*triaged.verdict(), Verdict::Confirmed);
//! ```
//!
//! # Stand
//! Ersetzt das Gerüst aus Knoten AW0-00. Inhalt aus Knoten **AW4-03**;
//! Ebene **L4** im Zielgraphen.

#![forbid(unsafe_code)]

pub mod advisory;
pub mod baseline;
pub mod confidence;
pub mod engine;
pub mod finding;
pub mod rule;
pub mod rules;

pub use advisory::{Advisory, correlate_advisories};
pub use baseline::{Baseline, PalaceStatus, finding_kind_for_status};
pub use confidence::epistemic_confidence_for;
pub use engine::run_rules;
pub use finding::{Finding, FindingKind, Raw, RuleChecked, Triaged, Verdict, triage};
// Nur für Tests abhängiger Crates (Feature `test-support`, z. B. von
// `harw-dod-escalate`s `Ladder`-Tests genutzt) — siehe `finding.rs`-Moduldoku
// zu `triaged_finding_for_test` für die Begründung und die geschützte
// Invariante.
#[cfg(any(test, feature = "test-support"))]
pub use finding::triaged_finding_for_test;
pub use rule::{Rule, RuleContext};
pub use rules::{BaselineDeviationRule, EgressFlowRule, StructureDriftRule};

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
