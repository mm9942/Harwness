//! Fehlertypen von `harw-research`.
//!
//! Folgt dem Muster aus `harw-provider/src/error.rs`: `#[derive(HarwError)]`
//! erzeugt `Display`, `std::error::Error` (inkl. `source()`), die
//! `#[from]`-Konvertierung für `serde_json::Error` **und** den passenden
//! `ResearchResult<T>`-Alias (weil der Enum-Name auf `Error` endet). Es wird
//! bewusst weder `anyhow` noch `thiserror` benutzt.
//!
//! Verantwortungsbereich: Alle Fehlerfälle beim Parsen und Validieren
//! strukturierter Recherche-Ergebnisse (coding-philosophy.md §4) und des
//! reduzierten Return-Contracts für read-only Kind-Agenten
//! (agent-definition-dsl.md §13).

use harw_macros::HarwError;

use crate::types::QuestionId;

/// Zentrale Fehlerklasse von `harw-research`.
///
/// Der `HarwError`-Derive emittiert zusätzlich `pub type ResearchResult<T>`.
#[derive(Debug, HarwError)]
pub enum ResearchError {
    /// JSON-(De-)Serialisierung ist fehlgeschlagen; die Display-Ausgabe
    /// delegiert an die `serde_json`-Fehlermeldung.
    #[from]
    Json(serde_json::Error),

    /// Ein Pflichtfeld (`question_id`, `conclusion`, `produced_by`, ein
    /// `locator` oder eine `version`) ist leer oder nur Whitespace.
    #[msg("field '{field}' must not be empty")]
    EmptyField { field: &'static str },

    /// `confidence >= Medium`, aber `evidence` ist leer — der Recherche-Vertrag
    /// verlangt Belege ab mittlerer Konfidenz.
    #[msg("question '{question_id}' has confidence >= medium but no evidence")]
    EvidenceRequired { question_id: QuestionId },

    /// Ein Wert konnte keiner bekannten `SourceClass`-Variante zugeordnet
    /// werden.
    #[msg("unknown source class '{value}'")]
    UnknownSourceClass { value: String },

    /// Die Struktur eines geparsten Werts entspricht nicht dem erwarteten
    /// Schema.
    #[msg("schema mismatch: {reason}")]
    SchemaMismatch { reason: String },

    /// Eine `ReturnEnvelope` fehlt ein Pflichtfeld.
    #[msg("return envelope incomplete: missing field '{missing}'")]
    EnvelopeIncomplete { missing: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_field_display_names_field() {
        let err = ResearchError::EmptyField { field: "question_id" };
        assert_eq!(err.to_string(), "field 'question_id' must not be empty");
    }

    #[test]
    fn test_evidence_required_display_names_question() {
        let err = ResearchError::EvidenceRequired {
            question_id: QuestionId::new("q-1"),
        };
        assert_eq!(
            err.to_string(),
            "question 'q-1' has confidence >= medium but no evidence"
        );
    }

    #[test]
    fn test_json_from_conversion_and_source() {
        let json_err = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let err: ResearchError = json_err.into();
        assert!(matches!(err, ResearchError::Json(_)));
        assert!(std::error::Error::source(&err).is_some());
    }

    #[test]
    fn test_unknown_source_class_and_schema_mismatch_display() {
        let a = ResearchError::UnknownSourceClass {
            value: "carrier-pigeon".to_owned(),
        };
        assert_eq!(a.to_string(), "unknown source class 'carrier-pigeon'");

        let b = ResearchError::SchemaMismatch {
            reason: "missing 'confidence'".to_owned(),
        };
        assert_eq!(b.to_string(), "schema mismatch: missing 'confidence'");
    }

    #[test]
    fn test_envelope_incomplete_display() {
        let err = ResearchError::EnvelopeIncomplete {
            missing: "summary".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "return envelope incomplete: missing field 'summary'"
        );
    }
}
