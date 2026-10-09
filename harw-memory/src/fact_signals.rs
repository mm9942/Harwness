//! Brücke von Fakten-Rückmeldung zum Widerspruchs-Index (X5).
//!
//! `contradiction_index` arbeitet auf [`EpistemicSignal`]en, nicht auf Fakten.
//! Diese Brücke bildet Fakten mit belastbarer Rückmeldung (genutzt vs.
//! korrigiert) auf Signale ab und nutzt davon nur die Regel „entgegengesetzte
//! Ergebnisse im selben Themenkorb": ein Fakt, der überwiegend genutzt
//! wurde, neben einem, der überwiegend korrigiert wurde, zum selben Thema.
//! Das Ergebnis sind Konflikt-Hinweise zur Prüfung, nie automatische Löschung.
//!
//! Themenkorb: Fakt-Typ plus erstes aussagekräftiges Wort des Namens.

use time::OffsetDateTime;

use crate::consolidation::Conflict;
use crate::contradiction_index::{ContradictionReason, detect_contradictions};
use crate::epistemic::{
    Confidence, EpistemicSignal, MemoryScope, OutcomeVerdict, Provenance, Validity,
};
use crate::facts::{Fact, Feedback};

/// Mindest-Schweregrad, ab dem ein Treffer als Hinweis zählt.
const MIN_SEVERITY: u8 = 70;
/// Kürzeste Wortlänge für den Themenschlüssel.
const MIN_TOPIC_LEN: usize = 4;

fn topic_key(fact: &Fact) -> String {
    let word = fact
        .name
        .split(|c: char| !c.is_alphanumeric())
        .find(|w| w.chars().count() >= MIN_TOPIC_LEN)
        .unwrap_or(&fact.name)
        .to_lowercase();
    format!("{}/{word}", fact.fact_type)
}

fn verdict(feedback: &Feedback) -> Option<OutcomeVerdict> {
    if feedback.corrected > feedback.used {
        Some(OutcomeVerdict::Refuted)
    } else if feedback.used > 0 && feedback.used > feedback.corrected {
        Some(OutcomeVerdict::Confirmed)
    } else {
        None
    }
}

/// Konflikt-Hinweise aus entgegengesetzter Rückmeldung zum selben Thema.
#[must_use]
pub fn conflicts_from_feedback(facts: &[Fact], feedback: &[(String, Feedback)]) -> Vec<Conflict> {
    let now = OffsetDateTime::now_utc();
    let signals: Vec<EpistemicSignal> = facts
        .iter()
        .filter_map(|fact| {
            let (_, fb) = feedback.iter().find(|(name, _)| name == &fact.name)?;
            let outcome = verdict(fb)?;
            Some(EpistemicSignal {
                id: fact.name.clone(),
                statement: fact.description.clone(),
                origin: Provenance::AgentObservation {
                    task_id: "feedback".to_owned(),
                },
                confidence: Confidence::Medium,
                validity: Validity::Permanent,
                scope: MemoryScope::Project {
                    id: topic_key(fact),
                },
                contradictions: Vec::new(),
                outcome: Some(outcome),
                created_at: now,
                last_used: None,
                independent_confirmations: 0,
                salience: 0,
            })
        })
        .collect();
    detect_contradictions(&signals)
        .into_iter()
        .filter(|c| c.reason == ContradictionReason::OppositeOutcomes && c.severity >= MIN_SEVERITY)
        .map(|c| Conflict {
            left: c.left_id,
            right: c.right_id,
            note: "Rückmeldung widerspricht sich: einer wurde genutzt, der andere überwiegend korrigiert (gleiches Thema)".to_owned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{FactScope, FactType};

    fn fact(name: &str, fact_type: FactType) -> Fact {
        let now = OffsetDateTime::now_utc();
        Fact {
            name: name.to_owned(),
            description: name.to_owned(),
            fact_type,
            scope: FactScope::Project,
            created: now,
            updated: now,
            confidence: 0.7,
            sources: Vec::new(),
            tags: Vec::new(),
            body: "x".to_owned(),
        }
    }

    fn fb(used: u64, corrected: u64) -> Feedback {
        Feedback {
            delivered: used + corrected,
            used,
            corrected,
        }
    }

    #[test]
    fn opposite_feedback_on_the_same_topic_becomes_a_conflict_hint() {
        let facts = [
            fact("nextest-preferred", FactType::Preference),
            fact("nextest-forbidden", FactType::Preference),
            fact("bazel-preferred", FactType::Preference),
        ];
        let feedback = vec![
            ("nextest-preferred".to_owned(), fb(4, 0)),
            ("nextest-forbidden".to_owned(), fb(0, 3)),
            ("bazel-preferred".to_owned(), fb(0, 3)),
        ];
        let conflicts = conflicts_from_feedback(&facts, &feedback);
        assert_eq!(conflicts.len(), 1, "{conflicts:?}");
        assert_eq!(conflicts[0].left, "nextest-preferred");
        assert_eq!(conflicts[0].right, "nextest-forbidden");
    }

    #[test]
    fn facts_without_decisive_feedback_or_in_other_topics_never_conflict() {
        let facts = [
            fact("alpha-one", FactType::Pitfall),
            fact("beta-two", FactType::Pitfall),
            fact("alpha-three", FactType::Preference),
        ];
        let feedback = vec![
            ("alpha-one".to_owned(), fb(0, 3)),
            ("beta-two".to_owned(), fb(3, 0)),
            ("alpha-three".to_owned(), fb(2, 2)),
        ];
        assert!(conflicts_from_feedback(&facts, &feedback).is_empty());
    }
}
