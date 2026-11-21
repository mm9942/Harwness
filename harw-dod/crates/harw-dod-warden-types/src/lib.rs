//! Signierfreies Vokabular zwischen Eskalationsleiter und Warden (PL-100 WP 3.0).
//!
//! # Zweck
//! Diese Crate enthält den Teil des Warden-Protokolls, der ohne Signier-Code
//! auskommt: die Aktionstaxonomie ([`ProposedAction`]/[`WardenAction`],
//! [`Reversibility`], `action.rs`), die Eskalationsstufen
//! ([`EscalationStage`], `stage.rs`), das Ziel-Tripel einer Aktion
//! ([`CgroupKernelId`], [`CgroupTarget`], `target.rs`), die inhaltsfreie
//! Ablehnungskategorie ([`Denial`], `denial.rs`) und das Ergebnis einer
//! Aktion ([`WardenActionAudit`], [`WardenResponse`], `outcome.rs`).
//!
//! Sie wurde aus `harw-dod-warden-proto` herausgelöst (README §4, Phase 3,
//! „3.0 Abhängigkeiten aufteilen"): Prozesse, die mit Modellen sprechen,
//! linken keinen Signier-Code (Leitprinzip I8, Bedrohung B16). Wer nur das
//! Vokabular braucht — etwa die Eskalationsleiter (`harw-dod-ladder`) —
//! hängt an dieser Crate statt am Signierteil.
//!
//! # Was hier bewusst fehlt
//! Kein MAC, keine Schlüssel (`KeyRing`, `ProofKey`), keine
//! `SignedAuthorization`, kein Nonce-Ledger und keine Anfragehülle
//! (`WardenRequest`/`WardenReply`). All das bleibt in
//! `harw-dod-warden-proto`, das die Typen dieser Crate unter ihren bisherigen
//! Pfaden re-exportiert (`harw_dod_warden_proto::WardenAction`,
//! `harw_dod_warden_proto::action::WardenAction`, …). Der einzige Hash hier
//! ist der unverschlüsselte Bindungsdigest [`WardenAction::binding_digest`]
//! (BLAKE3 über `harw_types::ContentDigest`): er ist eine Eingabe des MAC,
//! kein MAC, und verleiht niemandem eine Berechtigung.
//!
//! # Kanonische Kodierung
//! [`canonical`] enthält die längenpräfixierten Bausteine, aus denen
//! [`WardenAction::binding_digest`] und [`CgroupTarget::put_canonical`] ihre
//! Bytes bilden. Der Signierteil baut die MAC-Eingabe aus denselben
//! Bausteinen; die Bytes sind durch die Aufteilung unverändert geblieben
//! (Golden-Tests in `action.rs` und `target.rs`).
//!
//! # Keine freien Argumente
//! `ProposedAction`/`WardenAction` werden von einem Modell vorgeschlagen.
//! Ein Feld, in das ein Modell freien Text schreiben kann, ist der Weg, auf
//! dem eine Modellausgabe zu einem Befehl wird. Jedes Aktionsfeld ist deshalb
//! eine `harw-types`-Kennung (hier: `CgroupId`), nie ein roher `String`.
//!
//! # Nebenläufigkeit
//! Reine, unveränderliche Werttypen. Kein internes Locking, keine Systemuhr
//! (`jiff::Timestamp`-Werte kommen immer als Parameter). Alle Typen sind
//! `Send + Sync`.
//!
//! # Fehler
//! [`WardenTypesError`] — einzige Variante: eine cgroup-Kernel-ID `0`.
//!
//! # Examples
//! ```rust
//! use harw_dod_warden_types::{EscalationStage, WardenAction};
//! use harw_types::CgroupId;
//!
//! let Ok(cgroup) = CgroupId::try_from_str("harw.slice/job-1") else { return };
//! let freeze = WardenAction::FreezeCgroup { cgroup };
//! assert!(freeze.is_admissible_from(EscalationStage::RuleTriggered));
//! assert!(!freeze.requires_irreversible_ack());
//! ```

pub mod action;
pub mod canonical;
pub mod denial;
pub mod error;
pub mod outcome;
pub mod stage;
pub mod target;

pub use action::{ProposedAction, Reversibility, WardenAction};
pub use denial::Denial;
pub use error::{WardenTypesError, WardenTypesResult};
pub use outcome::{WardenActionAudit, WardenResponse};
pub use stage::EscalationStage;
pub use target::{CgroupKernelId, CgroupTarget};

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
