//! Return-Contract für read-only Kind-Agenten.
//!
//! Verantwortungsbereich: `ReturnEnvelope` trägt das Ergebnis eines
//! Kind-Agenten (Ausgang, Zusammenfassung, Artefakte, Blocker, Payload)
//! zurück an seinen Eltern-Agenten — eine reduzierte Fassung des
//! Return-Contracts aus `agent-definition-dsl.md` §13 für read-only
//! Explorer-/Researcher-Worker (kein `state_revision_after`, kein
//! `priority`/`severity`, weil diese Worker keinen Plan-Zustand mutieren).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{ResearchError, ResearchResult};
use crate::types::ResearchFinding;
use crate::validate::strip_code_fence;

/// Ausgang eines Kind-Agenten-Laufs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReturnOutcome {
    /// Der Auftrag wurde vollständig erfüllt.
    Success,
    /// Der Auftrag wurde teilweise erfüllt.
    Partial,
    /// Der Auftrag konnte nicht erfüllt werden.
    Failed,
}

/// Reduzierter Return-Contract für read-only Kind-Agenten
/// (`agent-definition-dsl.md` §13).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReturnEnvelope {
    /// Bezeichner des berichtenden Agenten.
    pub agent_id: String,
    /// Optionale Zuordnung zu einem Plan-Knoten.
    #[serde(default)]
    pub plan_node_id: Option<String>,
    /// Ausgang des Laufs.
    pub outcome: ReturnOutcome,
    /// Kurze menschenlesbare Zusammenfassung.
    pub summary: String,
    /// Pfade oder Bezeichner erzeugter Artefakte.
    #[serde(default)]
    pub artifacts: Vec<String>,
    /// Dinge, die den Agenten blockiert haben.
    #[serde(default)]
    pub blockers: Vec<String>,
    /// Nicht blockierende Warnungen.
    #[serde(default)]
    pub warnings: Vec<String>,
    /// Vorgeschlagene nächste Schritte für den Elternagenten.
    #[serde(default)]
    pub suggested_next_actions: Vec<String>,
    /// Strukturierte Nutzlast (z. B. ein serialisiertes [`ResearchFinding`]).
    pub payload: Value,
}

/// Parst eine [`ReturnEnvelope`] aus rohem Kind-Agenten-Text.
///
/// # Description
/// Toleriert eine umschließende ```` ```json ```` -Fence wie
/// [`crate::validate::parse_finding`].
///
/// # Errors
/// - [`ResearchError::Json`]: wenn der (fence-bereinigte) Text kein gültiges
///   `ReturnEnvelope`-JSON ist.
pub fn parse_return_envelope(raw: &str) -> ResearchResult<ReturnEnvelope> {
    let cleaned = strip_code_fence(raw);
    serde_json::from_str(cleaned).map_err(ResearchError::from)
}

/// Baut eine `Success`-Envelope, die ein [`ResearchFinding`] als Payload trägt.
///
/// # Description
/// `artifacts`/`blockers`/`warnings`/`suggested_next_actions` bleiben leer;
/// `summary` wird aus `finding.conclusion` übernommen. Die Serialisierung von
/// `finding` nach `serde_json::Value` kann für die hier verwendeten Typen
/// praktisch nicht fehlschlagen; ein Fehlerfall fällt auf `Value::Null` zurück
/// statt zu paniken.
///
/// # Arguments
/// - `agent_id` (`&str`): Bezeichner des berichtenden Kind-Agenten.
/// - `plan_node_id` (`Option<&str>`): optionale Plan-Knoten-Zuordnung.
/// - `finding` (`&ResearchFinding`): das zu verpackende Recherche-Ergebnis.
///
/// # Returns
/// Eine `ReturnEnvelope` mit `outcome = Success` und `payload` = serialisiertes
/// `finding`.
#[must_use]
pub fn envelope_with_finding(
    agent_id: &str,
    plan_node_id: Option<&str>,
    finding: &ResearchFinding,
) -> ReturnEnvelope {
    let payload = serde_json::to_value(finding).unwrap_or(Value::Null);
    ReturnEnvelope {
        agent_id: agent_id.to_owned(),
        plan_node_id: plan_node_id.map(str::to_owned),
        outcome: ReturnOutcome::Success,
        summary: finding.conclusion.to_owned(),
        artifacts: Vec::new(),
        blockers: Vec::new(),
        warnings: Vec::new(),
        suggested_next_actions: Vec::new(),
        payload,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Confidence, QuestionId};

    fn sample_finding() -> ResearchFinding {
        ResearchFinding {
            question_id: QuestionId::new("q-1"),
            conclusion: "jiff 0.2.32 is current".to_owned(),
            evidence: vec![],
            verified_versions: vec![],
            constraints: vec![],
            compatibility_notes: vec![],
            unresolved_questions: vec![],
            confidence: Confidence::Low,
            produced_by: "explorer-1".to_owned(),
            produced_at: "2026-08-27T00:00:00Z".parse().unwrap(),
        }
    }

    #[test]
    fn test_return_outcome_serializes_snake_case() {
        let json = serde_json::to_string(&ReturnOutcome::Success).unwrap();
        assert_eq!(json, "\"success\"");
    }

    #[test]
    fn test_envelope_with_finding_sets_payload_and_summary() {
        let finding = sample_finding();
        let env = envelope_with_finding("explorer-1", Some("node-42"), &finding);

        assert_eq!(env.agent_id, "explorer-1");
        assert_eq!(env.plan_node_id.as_deref(), Some("node-42"));
        assert_eq!(env.outcome, ReturnOutcome::Success);
        assert_eq!(env.summary, "jiff 0.2.32 is current");
        assert!(env.artifacts.is_empty());
        assert!(env.blockers.is_empty());

        let expected_payload = serde_json::to_value(&finding).unwrap();
        assert_eq!(env.payload, expected_payload);
    }

    #[test]
    fn test_envelope_with_finding_plan_node_id_none() {
        let finding = sample_finding();
        let env = envelope_with_finding("explorer-1", None, &finding);
        assert_eq!(env.plan_node_id, None);
    }

    #[test]
    fn test_parse_return_envelope_strips_json_fence() {
        let raw = "```json\n{\"agent_id\":\"a1\",\"outcome\":\"partial\",\
                   \"summary\":\"partly done\",\"payload\":{}}\n```";
        let env = parse_return_envelope(raw).unwrap();
        assert_eq!(env.agent_id, "a1");
        assert_eq!(env.outcome, ReturnOutcome::Partial);
        assert!(env.artifacts.is_empty());
    }

    #[test]
    fn test_parse_return_envelope_invalid_json_returns_json_error() {
        let err = parse_return_envelope("nope").unwrap_err();
        assert!(matches!(err, ResearchError::Json(_)));
    }

    #[test]
    fn test_return_envelope_roundtrip() {
        let finding = sample_finding();
        let env = envelope_with_finding("explorer-1", Some("node-1"), &finding);
        let json = serde_json::to_string(&env).unwrap();
        let back: ReturnEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, back);
    }
}
