//! Kontextvokabular: Fragmente, Vertrauensklassen, Budget und Decke.
//!
//! # Verantwortungsbereich
//! Besitzt [`Fragment`], [`TrustClass`], [`Stability`], [`FragmentOrigin`],
//! [`SectionName`], [`FragmentLabel`], [`Selector`], [`ContextBudgetSpec`],
//! [`ContextCeiling`], [`CeilingViolation`], [`DetailMode`] und
//! [`OmissionReason`] (Contract-Master AW0 §E). Reines Vokabular ohne
//! Montagelogik: diese Crate trifft keine Entscheidung darüber, *welche*
//! Fragmente letztlich in einen Prompt wandern — das ist
//! `Assembly<Gathered|Admitted|Budgeted|Rendered>` in `harw-core` (Knoten
//! AW1-03), nicht hier.
//!
//! # Kernzusagen
//! - [`ContextBudgetSpec::tighten`] ist ein punktweises Minimum über
//!   Gesamt- und Sektionsbudgets. Es gibt kein `widen`: ein Budget kann auf
//!   keinem Weg wachsen — das ist eine Typ-Eigenschaft, keine Konvention.
//! - [`ContextCeiling::intersect`] ist ein Schnitt, keine Vereinigung. Eine
//!   Decke wird beim Handoff an ein Kind im selben Schritt geschnitten wie
//!   die Berechtigungen (Vorbild: `harw_authority::NetworkScope`) — ein Kind
//!   kann seine Decke nie anheben, weil es keine Methode dafür gibt.
//! - [`TrustClass`]s Deklarationsreihenfolge (`Instruction < Evidence <
//!   Data`, aus der abgeleiteten `Ord`-Instanz) ist **nicht** die
//!   Vertrauensreihenfolge. `Instruction` ist die vertrauenswürdigste
//!   Klasse. Siehe [`TrustClass::trust_rank`] für die tatsächliche
//!   Bedeutung von „höher" im Sinn von `max_trust`, und die Moduldoku von
//!   [`fragment`] für die ausführliche Begründung.
//!
//! # Modulaufbau
//! - [`fragment`]: `TrustClass`, `Stability`, `FragmentOrigin`, `Fragment`,
//!   `FragmentLabel`, `SectionName`.
//! - [`selector`]: `Selector` (Glob über Sektions-/Fragmentnamen).
//! - [`budget`] — `ContextBudgetSpec`.
//! - [`ceiling`]: `ContextCeiling`, `CeilingViolation`.
//! - [`render`]: `DetailMode`, `OmissionReason`.
//! - [`reference`]: `FragmentReference`, `DigestStatus` — der Verweis, den
//!   `DetailMode::References` rendert (Knoten **AW5-07**). Siehe die
//!   Moduldoku von [`reference`] für die Begründung der Feldwahl, die
//!   Digest-Veralterungsprüfung, und warum die Kappe gegen den
//!   Umgehungsweg bewusst **nicht** hier, sondern bei `context.load` in
//!   `harw-tools` (`harw_tools::context_load`) liegt.
//! - [`error`]: `ContextError`, der einzige Fehlertyp dieser Crate für
//!   ungültige Namen.
//!
//! # Nebenläufigkeit
//! Reine Werttypen ohne innere Veränderlichkeit: alle exportierten Typen
//! sind `Send + Sync` (kein `Rc`/`RefCell`, kein geteilter Zustand).
//!
//! # Fehler
//! [`ContextError`] beim Konstruieren eines [`SectionName`],
//! [`FragmentLabel`] oder [`Selector`] aus einem leeren oder
//! steuerzeichenhaltigen String. [`CeilingViolation`] bei einer abgelehnten
//! [`ContextCeiling::admits`]-Prüfung.
//!
//! # Examples
//! ```rust
//! use harw_context::{ContextBudgetSpec, ContextCeiling, SectionName, TrustClass};
//! use harw_lens_types::BudgetSpec;
//! use std::collections::{BTreeMap, BTreeSet};
//!
//! let history = SectionName::try_new("history.tail").unwrap();
//! let mut sections = BTreeSet::new();
//! sections.insert(history);
//!
//! let ceiling = ContextCeiling {
//!     sections,
//!     max_trust: TrustClass::Evidence,
//!     budget: ContextBudgetSpec {
//!         total: BudgetSpec { total: 1_000 },
//!         per_section: BTreeMap::new(),
//!     },
//! };
//!
//! // Ein Kind erbt höchstens dieselbe Decke, nie eine größere:
//! let child_ceiling = ceiling.intersect(&ceiling);
//! assert_eq!(child_ceiling, ceiling);
//! ```
//!
//! # Stand
//! Inhalt aus Knoten **AW0-04**; Ebene **L1** im Zielgraphen.

pub mod budget;
pub mod ceiling;
pub mod error;
pub mod fragment;
pub mod reference;
pub mod render;
pub mod selector;

pub use budget::ContextBudgetSpec;
pub use ceiling::{CeilingViolation, ContextCeiling};
pub use error::ContextError;
pub use fragment::{Fragment, FragmentLabel, FragmentOrigin, SectionName, Stability, TrustClass};
pub use reference::{DigestStatus, FragmentReference};
pub use render::{DetailMode, OmissionReason};
pub use selector::Selector;

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
