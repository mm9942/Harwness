//! Kontradiktion-Index für epistemische Signale.
//!
//! # Verantwortungsbereich
//! Dieses Modul implementiert den Konflikt-Erkennungs-Layer aus dem
//! Architektur-Review (epistemischer Layer). Es arbeitet **ausschließlich**
//! über Referenzen auf `&[EpistemicSignal]` — keine eigene Datenhaltung.
//!
//! Erkannte Konfliktarten:
//! - [`ContradictionReason::ExplicitCrossReference`] — Signal-A listet Signal-B in `contradictions`.
//! - [`ContradictionReason::OppositeOutcomes`] — gleicher Scope, `Confirmed` ↔ `Refuted`.
//! - [`ContradictionReason::ScopeConflictWithConfidenceDelta`] — gleicher Scope, starke Konfidenz-Differenz.
//! - [`ContradictionReason::StaleVsFresh`] — gleicher Scope, eines `Expired`, das andere `Permanent`.
//!
//! # Schlüsseltypen
//! - [`Contradiction`] — ein detektierter Widerspruch zwischen zwei Signals.
//! - [`ContradictionReason`] — Ursache des Widerspruchs.
//!
//! # Nebenläufigkeit
//! Alle exportierten Funktionen sind rein funktional (keine globale Zustände,
//! kein interner Mutex). Sie sind `Send + Sync`-kompatibel. Parallele Aufrufe
//! auf denselben Slices sind sicher; der Aufrufer ist für Synchronisation
//! verantwortlich, wenn die zugrundeliegenden Signals gleichzeitig mutiert werden.
//!
//! # Fehler
//! Dieses Modul produziert keine eigenen Fehler. Alle Funktionen geben
//! direkt Werte zurück.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_memory::contradiction_index::{detect_contradictions, build_index, severe_contradictions_for};
//! use harw_memory::epistemic::{EpistemicSignal, Provenance, Confidence, Validity, MemoryScope, OutcomeVerdict};
//! use time::OffsetDateTime;
//!
//! let now = OffsetDateTime::now_utc();
//! let signals = vec![
//!     EpistemicSignal {
//!         id: "a".to_owned(), statement: "A".to_owned(),
//!         origin: Provenance::AgentObservation { task_id: "t1".to_owned() },
//!         confidence: Confidence::High, validity: Validity::Permanent,
//!         scope: MemoryScope::UserGlobal, contradictions: vec!["b".to_owned()],
//!         outcome: None, created_at: now, last_used: None,
//!         independent_confirmations: 0, salience: 50,
//!     },
//! ];
//! let contradictions = detect_contradictions(&signals);
//! let index = build_index(&contradictions);
//! let count = severe_contradictions_for(&contradictions, "a", 70);
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─────────────────────────────────────────────────────────────────────────────
// Öffentliche Typen
// ─────────────────────────────────────────────────────────────────────────────

/// Ein detektierter Konflikt zwischen zwei epistemischen Signalen.
///
/// # Beschreibung
/// Repräsentiert einen einzelnen Widerspruch zwischen `left_id` und `right_id`.
/// Das Paar ist gerichtet: `left_id` ist stets das Signal mit dem kleineren
/// Index im ursprünglichen Slice (i < j-Ordnung).
///
/// `severity` gibt an, wie stark der Widerspruch ist (0..=100). Höhere Werte
/// bedeuten dringlichere Konflikte.
///
/// # Concurrency
/// Reiner Wert-Typ, `Send + Sync`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Contradiction {
    /// ID des linken Signals (geringerer Index).
    pub left_id: String,
    /// ID des rechten Signals (höherer Index).
    pub right_id: String,
    /// Ursache des Widerspruchs.
    pub reason: ContradictionReason,
    /// Konfliktstärke 0..=100 — höher ist konflikthafter.
    pub severity: u8,
}

/// Ursache einer erkannten Kontradiktion.
///
/// # Beschreibung
/// Jede Variante entspricht einer Erkennungsregel in [`detect_contradictions`].
/// Die Varianten sind exklusiv: pro Paar wird maximal eine Kontradiktion erzeugt.
/// Priorität: `ExplicitCrossReference` → `OppositeOutcomes` → `ScopeConflictWithConfidenceDelta`
/// → `StaleVsFresh`.
///
/// # Serialisierung
/// snake_case-Varianten (z. B. `"explicit_cross_reference"`).
///
/// # Concurrency
/// `Copy`-Typ, `Send + Sync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContradictionReason {
    /// Signal A referenziert Signal B explizit in seinem `contradictions`-Feld.
    ExplicitCrossReference,
    /// Beide Signals haben denselben Scope, aber stark unterschiedliche Confidence.
    ScopeConflictWithConfidenceDelta,
    /// Beide Signals haben denselben Scope und entgegengesetzte Outcomes
    /// (`Confirmed` vs `Refuted`).
    OppositeOutcomes,
    /// Beide Signals haben denselben Scope, eines ist `Expired`, das andere `Permanent`.
    StaleVsFresh,
}

// ─────────────────────────────────────────────────────────────────────────────
// Hilfsfunktionen (privat)
// ─────────────────────────────────────────────────────────────────────────────

/// Liefert den ordinalen Rang einer Confidence-Stufe (VeryLow=0 .. VeryHigh=4).
///
/// # Argumente
/// - `c` ([`crate::epistemic::Confidence`]): die Stufe.
///
/// # Rückgabe
/// `u8` im Bereich 0..=4.
///
/// # Concurrency
/// Rein funktional, thread-safe.
fn confidence_ordinal(c: crate::epistemic::Confidence) -> u8 {
    use crate::epistemic::Confidence;
    match c {
        Confidence::VeryLow => 0,
        Confidence::Low => 1,
        Confidence::Medium => 2,
        Confidence::High => 3,
        Confidence::VeryHigh => 4,
    }
}

/// Berechnet den Severity-Wert für `ScopeConflictWithConfidenceDelta`.
///
/// # Beschreibung
/// `severity = min(100, 50 + delta * 10)` wobei `delta = |ord(a) - ord(b)|`.
///
/// # Argumente
/// - `a` ([`crate::epistemic::Confidence`]): Konfidenz des ersten Signals.
/// - `b` ([`crate::epistemic::Confidence`]): Konfidenz des zweiten Signals.
///
/// # Rückgabe
/// `u8` im Bereich 50..=100.
///
/// # Concurrency
/// Rein funktional, thread-safe.
fn confidence_delta_severity(
    a: crate::epistemic::Confidence,
    b: crate::epistemic::Confidence,
) -> u8 {
    let delta = confidence_ordinal(a).abs_diff(confidence_ordinal(b));
    let raw = 50u16 + (delta as u16) * 10;
    raw.min(100) as u8
}

// ─────────────────────────────────────────────────────────────────────────────
// Öffentliche API
// ─────────────────────────────────────────────────────────────────────────────

/// Findet alle Konflikte innerhalb einer Signal-Menge.
///
/// # Beschreibung
/// Vergleicht jedes geordnete Paar `(i, j)` mit `i < j` nach vier Regeln,
/// in dieser Priorität:
///
/// 1. **ExplicitCrossReference** (severity 90): Signal i listet Signal j in
///    `contradictions`, oder umgekehrt.
/// 2. **OppositeOutcomes** (severity 80): Gleicher Scope; eines hat
///    `Confirmed`, das andere `Refuted`.
/// 3. **ScopeConflictWithConfidenceDelta** (severity 50 + delta×10, max 100):
///    Gleicher Scope; Konfidenz-Differenz ≥ 1.
/// 4. **StaleVsFresh** (severity 30): Gleicher Scope; eines `Expired`,
///    das andere `Permanent`.
///
/// Pro Paar wird höchstens ein `Contradiction`-Eintrag erzeugt (erste
/// treffende Regel gewinnt). Die Regeln 2–4 werden nur bei gleichem Scope geprüft.
///
/// # Argumente
/// - `signals` (`&[EpistemicSignal]`): Slice der zu prüfenden Signale.
///
/// # Rückgabe
/// `Vec<Contradiction>` — leer, wenn keine Konflikte gefunden wurden.
///
/// # Panics
/// Keine.
///
/// # Concurrency
/// Rein funktional; keine Seiteneffekte. Thread-safe.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_memory::contradiction_index::detect_contradictions;
/// let contradictions = detect_contradictions(&[]);
/// assert!(contradictions.is_empty());
/// ```
pub fn detect_contradictions(signals: &[crate::epistemic::EpistemicSignal]) -> Vec<Contradiction> {
    use crate::epistemic::{OutcomeVerdict, Validity};

    let mut result = Vec::new();

    for i in 0..signals.len() {
        for j in (i + 1)..signals.len() {
            let a = &signals[i];
            let b = &signals[j];

            // Regel 1: ExplicitCrossReference
            let a_refs_b = a.contradictions.iter().any(|id| id == &b.id);
            let b_refs_a = b.contradictions.iter().any(|id| id == &a.id);
            if a_refs_b || b_refs_a {
                result.push(Contradiction {
                    left_id: a.id.clone(),
                    right_id: b.id.clone(),
                    reason: ContradictionReason::ExplicitCrossReference,
                    severity: 90,
                });
                continue;
            }

            // Regeln 2–4 nur bei gleichem Scope
            if a.scope != b.scope {
                continue;
            }

            // Regel 2: OppositeOutcomes
            let is_opposite = matches!(
                (a.outcome, b.outcome),
                (
                    Some(OutcomeVerdict::Confirmed),
                    Some(OutcomeVerdict::Refuted)
                ) | (
                    Some(OutcomeVerdict::Refuted),
                    Some(OutcomeVerdict::Confirmed)
                )
            );
            if is_opposite {
                result.push(Contradiction {
                    left_id: a.id.clone(),
                    right_id: b.id.clone(),
                    reason: ContradictionReason::OppositeOutcomes,
                    severity: 80,
                });
                continue;
            }

            // Regel 3: ScopeConflictWithConfidenceDelta (delta >= 1)
            if a.confidence != b.confidence {
                let severity = confidence_delta_severity(a.confidence, b.confidence);
                result.push(Contradiction {
                    left_id: a.id.clone(),
                    right_id: b.id.clone(),
                    reason: ContradictionReason::ScopeConflictWithConfidenceDelta,
                    severity,
                });
                continue;
            }

            // Regel 4: StaleVsFresh — eines Expired, anderes Permanent
            let stale_fresh = matches!(
                (&a.validity, &b.validity),
                (Validity::Expired, Validity::Permanent) | (Validity::Permanent, Validity::Expired)
            );
            if stale_fresh {
                result.push(Contradiction {
                    left_id: a.id.clone(),
                    right_id: b.id.clone(),
                    reason: ContradictionReason::StaleVsFresh,
                    severity: 30,
                });
            }
        }
    }

    result
}

/// Baut einen Reverse-Index: Signal-ID → Liste der Konflikt-Partner.
///
/// # Beschreibung
/// Iteriert über alle `Contradiction`-Einträge und trägt für jedes beteiligte
/// Signal die ID des Gegenübers ein. Der Index ist **unidirektional** pro Eintrag:
/// für ein Paar (left, right) wird `left → right` UND `right → left` eingetragen,
/// sodass jedes Signal seine vollständige Partnerliste erhält.
///
/// # Argumente
/// - `contradictions` (`&[Contradiction]`): Slice der vorher detektierten Konflikte.
///
/// # Rückgabe
/// `HashMap<String, Vec<String>>` — ID → Liste aller Konflikt-Partner (in
/// Reihenfolge der Iteration; keine Deduplizierung).
///
/// # Panics
/// Keine.
///
/// # Concurrency
/// Rein funktional; keine Seiteneffekte. Thread-safe.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_memory::contradiction_index::{Contradiction, ContradictionReason, build_index};
/// let c = vec![Contradiction { left_id: "a".to_owned(), right_id: "b".to_owned(),
///     reason: ContradictionReason::ExplicitCrossReference, severity: 90 }];
/// let idx = build_index(&c);
/// assert_eq!(idx["a"], vec!["b"]);
/// assert_eq!(idx["b"], vec!["a"]);
/// ```
pub fn build_index(contradictions: &[Contradiction]) -> HashMap<String, Vec<String>> {
    let mut index: HashMap<String, Vec<String>> = HashMap::new();
    for c in contradictions {
        index
            .entry(c.left_id.clone())
            .or_default()
            .push(c.right_id.clone());
        index
            .entry(c.right_id.clone())
            .or_default()
            .push(c.left_id.clone());
    }
    index
}

/// Zählt die Kontradiktionen eines Signals mit `severity >= threshold`.
///
/// # Beschreibung
/// Filtert alle Einträge aus `contradictions`, die entweder `left_id` oder
/// `right_id` gleich `signal_id` sind, und zählt jene mit `severity >= threshold`.
///
/// # Argumente
/// - `contradictions` (`&[Contradiction]`): vorher detektierter Konflikt-Slice.
/// - `signal_id` (`&str`): ID des zu prüfenden Signals.
/// - `threshold` (`u8`): untere Grenze (inklusiv).
///
/// # Rückgabe
/// Anzahl der Kontradiktionen, die die Schwelle erreichen oder überschreiten.
///
/// # Panics
/// Keine.
///
/// # Concurrency
/// Rein funktional; keine Seiteneffekte. Thread-safe.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_memory::contradiction_index::{Contradiction, ContradictionReason, severe_contradictions_for};
/// let c = vec![Contradiction { left_id: "a".to_owned(), right_id: "b".to_owned(),
///     reason: ContradictionReason::OppositeOutcomes, severity: 80 }];
/// assert_eq!(severe_contradictions_for(&c, "a", 70), 1);
/// assert_eq!(severe_contradictions_for(&c, "a", 90), 0);
/// ```
pub fn severe_contradictions_for(
    contradictions: &[Contradiction],
    signal_id: &str,
    threshold: u8,
) -> usize {
    contradictions
        .iter()
        .filter(|c| (c.left_id == signal_id || c.right_id == signal_id) && c.severity >= threshold)
        .count()
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::epistemic::{
        Confidence, EpistemicSignal, MemoryScope, OutcomeVerdict, Provenance, Validity,
    };
    use crate::test_support::{TestResult, ctx};
    use time::OffsetDateTime;

    /// Erzeugt ein minimales `EpistemicSignal` für Tests.
    fn make_signal(
        id: &str,
        scope: MemoryScope,
        confidence: Confidence,
        validity: Validity,
        outcome: Option<OutcomeVerdict>,
        contradictions: Vec<String>,
    ) -> EpistemicSignal {
        EpistemicSignal {
            id: id.to_owned(),
            statement: format!("Statement for {id}"),
            origin: Provenance::AgentObservation {
                task_id: "task-test".to_owned(),
            },
            confidence,
            validity,
            scope,
            contradictions,
            outcome,
            created_at: OffsetDateTime::now_utc(),
            last_used: None,
            independent_confirmations: 0,
            salience: 50,
        }
    }

    // 1. Leerer Slice → leerer Vec
    #[test]
    fn test_no_contradictions_in_empty() {
        let result = detect_contradictions(&[]);
        assert!(
            result.is_empty(),
            "Leere Signal-Liste darf keine Kontradiktionen liefern"
        );
    }

    // 2. Einzelnes Signal → leerer Vec
    #[test]
    fn test_no_contradictions_in_singleton() {
        let sig = make_signal(
            "solo",
            MemoryScope::UserGlobal,
            Confidence::High,
            Validity::Permanent,
            None,
            vec![],
        );
        let result = detect_contradictions(&[sig]);
        assert!(
            result.is_empty(),
            "Einzelnes Signal darf keine Kontradiktionen liefern"
        );
    }

    // 3. ExplicitCrossReference erkannt
    #[test]
    fn test_explicit_cross_reference_detected() {
        let a = make_signal(
            "sig-a",
            MemoryScope::UserGlobal,
            Confidence::Medium,
            Validity::Permanent,
            None,
            vec!["sig-b".to_owned()],
        );
        let b = make_signal(
            "sig-b",
            MemoryScope::UserGlobal,
            Confidence::Medium,
            Validity::Permanent,
            None,
            vec![],
        );
        let result = detect_contradictions(&[a, b]);
        assert_eq!(result.len(), 1, "Exakt ein Widerspruch erwartet");
        assert_eq!(result[0].left_id, "sig-a");
        assert_eq!(result[0].right_id, "sig-b");
        assert_eq!(
            result[0].reason,
            ContradictionReason::ExplicitCrossReference
        );
        assert_eq!(result[0].severity, 90);
    }

    // 4. Gleicher Scope, entgegengesetzte Outcomes → OppositeOutcomes, severity 80
    #[test]
    fn test_same_scope_opposite_outcomes() {
        let a = make_signal(
            "confirmed",
            MemoryScope::UserGlobal,
            Confidence::High,
            Validity::Permanent,
            Some(OutcomeVerdict::Confirmed),
            vec![],
        );
        let b = make_signal(
            "refuted",
            MemoryScope::UserGlobal,
            Confidence::High,
            Validity::Permanent,
            Some(OutcomeVerdict::Refuted),
            vec![],
        );
        let result = detect_contradictions(&[a, b]);
        assert_eq!(result.len(), 1, "Exakt ein Widerspruch erwartet");
        assert_eq!(result[0].reason, ContradictionReason::OppositeOutcomes);
        assert_eq!(result[0].severity, 80);
    }

    // 5. Gleicher Scope, VeryLow vs VeryHigh → severity 90 (50 + 4*10)
    #[test]
    fn test_same_scope_confidence_delta() {
        let a = make_signal(
            "low-conf",
            MemoryScope::UserGlobal,
            Confidence::VeryLow,
            Validity::Permanent,
            None,
            vec![],
        );
        let b = make_signal(
            "high-conf",
            MemoryScope::UserGlobal,
            Confidence::VeryHigh,
            Validity::Permanent,
            None,
            vec![],
        );
        let result = detect_contradictions(&[a, b]);
        assert_eq!(result.len(), 1, "Exakt ein Widerspruch erwartet");
        assert_eq!(
            result[0].reason,
            ContradictionReason::ScopeConflictWithConfidenceDelta
        );
        // delta = 4, severity = min(100, 50 + 40) = 90
        assert_eq!(result[0].severity, 90);
    }

    // 6. Gleicher Scope, eines Expired, anderes Permanent → StaleVsFresh, severity 30
    #[test]
    fn test_stale_vs_fresh_detected() {
        let a = make_signal(
            "fresh",
            MemoryScope::UserGlobal,
            Confidence::High,
            Validity::Permanent,
            None,
            vec![],
        );
        let b = make_signal(
            "stale",
            MemoryScope::UserGlobal,
            Confidence::High,
            Validity::Expired,
            None,
            vec![],
        );
        let result = detect_contradictions(&[a, b]);
        assert_eq!(result.len(), 1, "Exakt ein Widerspruch erwartet");
        assert_eq!(result[0].reason, ContradictionReason::StaleVsFresh);
        assert_eq!(result[0].severity, 30);
    }

    // 7. Unterschiedliche Scopes → keine Kontradiktion (ohne explizite Referenz)
    #[test]
    fn test_different_scopes_no_contradiction() {
        let a = make_signal(
            "scope-global",
            MemoryScope::UserGlobal,
            Confidence::VeryLow,
            Validity::Expired,
            Some(OutcomeVerdict::Confirmed),
            vec![],
        );
        let b = make_signal(
            "scope-project",
            MemoryScope::Project {
                id: "proj-x".to_owned(),
            },
            Confidence::VeryHigh,
            Validity::Permanent,
            Some(OutcomeVerdict::Refuted),
            vec![],
        );
        let result = detect_contradictions(&[a, b]);
        assert!(
            result.is_empty(),
            "Unterschiedliche Scopes dürfen ohne explizite Referenz keine Kontradiktion liefern"
        );
    }

    // 8. build_index: 1 Signal mit 3 Konflikten → index[id].len == 3
    #[test]
    fn test_build_index_returns_multi_partners() {
        let contradictions = vec![
            Contradiction {
                left_id: "hub".to_owned(),
                right_id: "x1".to_owned(),
                reason: ContradictionReason::ExplicitCrossReference,
                severity: 90,
            },
            Contradiction {
                left_id: "hub".to_owned(),
                right_id: "x2".to_owned(),
                reason: ContradictionReason::OppositeOutcomes,
                severity: 80,
            },
            Contradiction {
                left_id: "hub".to_owned(),
                right_id: "x3".to_owned(),
                reason: ContradictionReason::StaleVsFresh,
                severity: 30,
            },
        ];
        let index = build_index(&contradictions);
        assert_eq!(
            index.get("hub").map(|v| v.len()),
            Some(3),
            "Hub-Signal muss 3 Partner im Index haben"
        );
        assert!(index.contains_key("x1"));
        assert!(index.contains_key("x2"));
        assert!(index.contains_key("x3"));
    }

    // 9. severe_contradictions_for filtert nach threshold
    #[test]
    fn test_severe_contradictions_filters_by_threshold() {
        let contradictions = vec![
            Contradiction {
                left_id: "s".to_owned(),
                right_id: "a".to_owned(),
                reason: ContradictionReason::ExplicitCrossReference,
                severity: 90,
            },
            Contradiction {
                left_id: "s".to_owned(),
                right_id: "b".to_owned(),
                reason: ContradictionReason::OppositeOutcomes,
                severity: 80,
            },
            Contradiction {
                left_id: "s".to_owned(),
                right_id: "c".to_owned(),
                reason: ContradictionReason::StaleVsFresh,
                severity: 30,
            },
        ];
        // threshold 70 → severity 90 und 80 zählen; 30 nicht
        assert_eq!(severe_contradictions_for(&contradictions, "s", 70), 2);
        // threshold 90 → nur severity 90
        assert_eq!(severe_contradictions_for(&contradictions, "s", 90), 1);
        // threshold 100 → keiner
        assert_eq!(severe_contradictions_for(&contradictions, "s", 100), 0);
        // unbekannte ID → 0
        assert_eq!(severe_contradictions_for(&contradictions, "unknown", 0), 0);
    }

    // 10. Serde-Roundtrip für Contradiction
    #[test]
    fn test_serde_roundtrip_contradiction() -> TestResult {
        let c = Contradiction {
            left_id: "left-1".to_owned(),
            right_id: "right-2".to_owned(),
            reason: ContradictionReason::ScopeConflictWithConfidenceDelta,
            severity: 70,
        };
        let json = serde_json::to_string(&c).map_err(ctx("Serialisierung fehlgeschlagen"))?;
        let back: Contradiction =
            serde_json::from_str(&json).map_err(ctx("Deserialisierung fehlgeschlagen"))?;
        assert_eq!(back, c, "Serde-Roundtrip muss identisch sein");
        // Prüfe snake_case-Variante im JSON
        assert!(
            json.contains("scope_conflict_with_confidence_delta"),
            "ContradictionReason muss als snake_case serialisiert werden"
        );
        Ok(())
    }
}
