//! `harw-research` — Strukturierte Recherche-Ergebnisse für read-only
//! Sub-Agenten (Explorer/Researcher).
//!
//! Verantwortungsbereich: Definiert den normalisierten Recherche-Vertrag aus
//! `coding-philosophy.md` §4 ("Research Can Fan Out Aggressively" — gebundene
//! Fragen, Quellgrenzen, normalisierte Ergebnisse statt Freitext) sowie den
//! reduzierten Return-Contract für read-only Kind-Agenten aus
//! `agent-definition-dsl.md` §13. Read-only Sub-Agenten liefern ihr Ergebnis
//! als validiertes JSON dieses Typs statt als Freitext-Essay.
//!
//! Module:
//! - [`types`]: `ResearchQuestion`, `ResearchFinding`, `FindingBundle` und
//!   ihre Bausteine (`SourceReference`, `VersionReference`, `Confidence`, …).
//! - [`schema`]: JSON-Schema- und Prompt-Erzeugung für `ResearchFinding`.
//! - [`validate`]: toleranter Parser (schneidet ```json-Fences weg) und
//!   Validierungsregeln.
//! - [`return_envelope`]: der reduzierte Kind-Agenten-Return-Contract.
//! - [`error`]: der zentrale Fehlertyp [`ResearchError`] und Alias
//!   [`ResearchResult`].
//!
//! Dieses Crate orchestriert und persistiert nichts selbst — es definiert nur
//! den Datenvertrag; Aufrufer (Orchestratoren, Speicherschichten) entscheiden,
//! wie Fragen verteilt und Ergebnisse abgelegt werden.
//!
//! # Concurrency
//! Alle Typen sind reine serde-Daten (`String`, `Vec`, `Option`,
//! `jiff::Timestamp`, geschlossene Enums) ohne interne Mutabilität — deshalb
//! automatisch `Send + Sync`. Kein `Arc`, kein `Mutex`, keine Threads.
//!
//! # Fehlertyp
//! [`ResearchError`] (siehe `error`-Modul), erzeugt über
//! `#[derive(harw_macros::HarwError)]`; Alias [`ResearchResult`].
//!
//! # Examples
//! ```rust,no_run
//! use harw_research::parse_and_validate;
//!
//! // `parse_and_validate` accepts plain JSON as well as JSON wrapped in a
//! // Markdown code fence (as models often emit despite instructions).
//! let raw = r#"{"question_id":"q-1","conclusion":"jiff 0.2.32 is current",
//!     "evidence":[{"kind":"cargo_registry_source",
//!     "locator":"crates.io/crates/jiff","retrieved_at":"2026-08-27T00:00:00Z"}],
//!     "confidence":"high","produced_by":"explorer-1",
//!     "produced_at":"2026-08-27T00:00:00Z"}"#;
//! let finding = parse_and_validate(raw).unwrap();
//! assert_eq!(finding.produced_by, "explorer-1");
//! ```

#![forbid(unsafe_code)]

pub mod error;
pub mod return_envelope;
pub mod schema;
pub mod types;
pub mod validate;

pub use error::{ResearchError, ResearchResult};
pub use return_envelope::{
    ReturnEnvelope, ReturnOutcome, envelope_with_finding, parse_return_envelope,
};
pub use schema::{finding_json_schema, finding_schema_prompt};
pub use types::{
    Confidence, FindingBundle, Freshness, QuestionId, QuestionScope, ResearchFinding,
    ResearchQuestion, SourceClass, SourceReference, VersionReference,
};
pub use validate::{parse_and_validate, parse_finding, validate_finding};
