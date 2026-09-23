//! Tolerantes Parsen und Validieren von [`crate::types::ResearchFinding`].
//!
//! Verantwortungsbereich: `parse_finding` schneidet eine umschließende
//! ```` ```json ```` -Fence weg, bevor geparst wird (Modelle antworten häufig
//! mit Markdown-Fences trotz gegenteiliger Anweisung). `validate_finding`
//! prüft die Vertragsregeln aus `agent-definition-dsl.md` §13 (reduzierte
//! Fassung für read-only Worker): Pflichtfelder nicht leer, und ab
//! `Confidence::Medium` müssen Belege vorhanden sein.

use crate::error::{ResearchError, ResearchResult};
use crate::types::{Confidence, ResearchFinding};

/// Entfernt eine umschließende ```` ``` ```` - oder ```` ```json ````
/// -Code-Fence, falls vorhanden; ansonsten wird der (getrimmte) Text
/// unverändert zurückgegeben.
///
/// `pub(crate)`, weil `return_envelope::parse_return_envelope` dieselbe
/// Toleranz braucht.
pub(crate) fn strip_code_fence(raw: &str) -> &str {
    let trimmed = raw.trim();
    let Some(after_open) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    // Optionaler Sprach-Tag direkt nach der öffnenden Fence (z. B. "json").
    let after_open = after_open
        .strip_prefix("json")
        .unwrap_or(after_open)
        .trim_start_matches('\n');
    let Some(body) = after_open.strip_suffix("```") else {
        return trimmed;
    };
    body.trim()
}

/// Parst ein [`ResearchFinding`] aus rohem Modell-Text.
///
/// # Description
/// Toleriert eine umschließende ```` ```json ```` -Fence (siehe
/// [`strip_code_fence`]). Führt keine inhaltliche Validierung durch — dafür
/// [`validate_finding`] verwenden.
///
/// # Errors
/// - [`ResearchError::Json`]: wenn der (fence-bereinigte) Text kein gültiges
///   `ResearchFinding`-JSON ist.
pub fn parse_finding(raw: &str) -> ResearchResult<ResearchFinding> {
    let cleaned = strip_code_fence(raw);
    serde_json::from_str(cleaned).map_err(ResearchError::from)
}

/// Validiert ein bereits geparstes [`ResearchFinding`] gegen die
/// Recherche-Vertragsregeln.
///
/// # Errors
/// - [`ResearchError::EmptyField`]: wenn `question_id`, `conclusion`,
///   `produced_by`, ein `evidence[].locator` oder ein
///   `verified_versions[].version` leer bzw. nur Whitespace ist.
/// - [`ResearchError::EvidenceRequired`]: wenn `confidence >= Medium` und
///   `evidence` leer ist.
pub fn validate_finding(finding: &ResearchFinding) -> ResearchResult<()> {
    if finding.question_id.as_str().trim().is_empty() {
        return Err(ResearchError::EmptyField {
            field: "question_id",
        });
    }
    if finding.conclusion.trim().is_empty() {
        return Err(ResearchError::EmptyField {
            field: "conclusion",
        });
    }
    if finding.produced_by.trim().is_empty() {
        return Err(ResearchError::EmptyField {
            field: "produced_by",
        });
    }
    if finding.confidence >= Confidence::Medium && finding.evidence.is_empty() {
        return Err(ResearchError::EvidenceRequired {
            question_id: finding.question_id.clone(),
        });
    }
    for source in &finding.evidence {
        if source.locator.trim().is_empty() {
            return Err(ResearchError::EmptyField { field: "locator" });
        }
    }
    for version in &finding.verified_versions {
        if version.version.trim().is_empty() {
            return Err(ResearchError::EmptyField { field: "version" });
        }
    }
    Ok(())
}

/// Kombiniert [`parse_finding`] und [`validate_finding`] in einem Schritt.
///
/// # Errors
/// Siehe [`parse_finding`] und [`validate_finding`].
pub fn parse_and_validate(raw: &str) -> ResearchResult<ResearchFinding> {
    let finding = parse_finding(raw)?;
    validate_finding(&finding)?;
    Ok(finding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::types::{QuestionId, SourceClass, SourceReference};

    fn ts() -> TestResult<jiff::Timestamp> {
        "2026-08-27T00:00:00Z"
            .parse()
            .map_err(ctx("fixture timestamp is valid"))
    }

    fn base_finding(
        confidence: Confidence,
        evidence: Vec<SourceReference>,
    ) -> TestResult<ResearchFinding> {
        Ok(ResearchFinding {
            question_id: QuestionId::new("q-1"),
            conclusion: "some conclusion".to_owned(),
            evidence,
            verified_versions: vec![],
            constraints: vec![],
            compatibility_notes: vec![],
            unresolved_questions: vec![],
            confidence,
            produced_by: "explorer-1".to_owned(),
            produced_at: ts()?,
        })
    }

    fn evidence() -> TestResult<Vec<SourceReference>> {
        Ok(vec![SourceReference {
            kind: SourceClass::OfficialDocs,
            locator: "https://docs.rs/jiff".to_owned(),
            retrieved_at: ts()?,
            digest: None,
            excerpt: "…".to_owned(),
        }])
    }

    #[test]
    fn test_parse_finding_strips_json_fence() -> TestResult {
        let raw = "```json\n{\"question_id\":\"q-1\",\"conclusion\":\"c\",\"evidence\":[],\
                   \"confidence\":\"low\",\"produced_by\":\"agent-1\",\
                   \"produced_at\":\"2026-08-27T00:00:00Z\"}\n```";
        let finding = parse_finding(raw).map_err(ctx("parse_finding"))?;
        assert_eq!(finding.question_id.as_str(), "q-1");
        assert_eq!(finding.confidence, Confidence::Low);
        Ok(())
    }

    #[test]
    fn test_parse_finding_without_fence_still_works() -> TestResult {
        let raw = "{\"question_id\":\"q-2\",\"conclusion\":\"c\",\"evidence\":[],\
                   \"confidence\":\"low\",\"produced_by\":\"agent-1\",\
                   \"produced_at\":\"2026-08-27T00:00:00Z\"}";
        let finding = parse_finding(raw).map_err(ctx("parse_finding"))?;
        assert_eq!(finding.question_id.as_str(), "q-2");
        Ok(())
    }

    #[test]
    fn test_parse_finding_invalid_json_returns_json_error() -> TestResult {
        let result = parse_finding("not json at all");
        let Err(err) = result else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(err, ResearchError::Json(_)));
        Ok(())
    }

    #[test]
    fn test_validate_finding_high_without_evidence_is_err() -> TestResult {
        let finding = base_finding(Confidence::High, vec![])?;
        let result = validate_finding(&finding);
        let Err(err) = result else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(
            err,
            ResearchError::EvidenceRequired { question_id } if question_id.as_str() == "q-1"
        ));
        Ok(())
    }

    #[test]
    fn test_validate_finding_low_without_evidence_is_ok() -> TestResult {
        let finding = base_finding(Confidence::Low, vec![])?;
        assert!(validate_finding(&finding).is_ok());
        Ok(())
    }

    #[test]
    fn test_validate_finding_medium_with_evidence_is_ok() -> TestResult {
        let finding = base_finding(Confidence::Medium, evidence()?)?;
        assert!(validate_finding(&finding).is_ok());
        Ok(())
    }

    #[test]
    fn test_validate_finding_empty_question_id_is_err() -> TestResult {
        let mut finding = base_finding(Confidence::Low, vec![])?;
        finding.question_id = QuestionId::new("   ");
        let result = validate_finding(&finding);
        let Err(err) = result else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(
            err,
            ResearchError::EmptyField {
                field: "question_id"
            }
        ));
        Ok(())
    }

    #[test]
    fn test_validate_finding_empty_locator_is_err() -> TestResult {
        let mut bad_evidence = evidence()?;
        bad_evidence[0].locator = " ".to_owned();
        let finding = base_finding(Confidence::High, bad_evidence)?;
        let result = validate_finding(&finding);
        let Err(err) = result else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        assert!(matches!(
            err,
            ResearchError::EmptyField { field: "locator" }
        ));
        Ok(())
    }

    #[test]
    fn test_parse_and_validate_end_to_end() -> TestResult {
        let raw = "```json\n{\"question_id\":\"q-3\",\"conclusion\":\"c\",\
                   \"evidence\":[{\"kind\":\"web\",\"locator\":\"https://example.com\",\
                   \"retrieved_at\":\"2026-08-27T00:00:00Z\"}],\
                   \"confidence\":\"high\",\"produced_by\":\"agent-1\",\
                   \"produced_at\":\"2026-08-27T00:00:00Z\"}\n```";
        let finding = parse_and_validate(raw).map_err(ctx("parse_and_validate"))?;
        assert_eq!(finding.confidence, Confidence::High);
        Ok(())
    }
}
