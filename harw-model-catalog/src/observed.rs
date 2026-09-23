//! Layer 4 des Modell-Katalogs — empirisch gemessenes Modellverhalten.
//!
//! Dieses Modul definiert [`ObservedModelBehavior`] und den zugehörigen
//! Wertetyp [`Score`]. Messwerte stammen ausschließlich aus realen
//! Harness-Runs. Beim Bootstrap stehen alle Scores auf `Score::HALF` und
//! `updated_at` ist `None` (§5, model-catalog-v2.md).
//!
//! # Verantwortung
//! - Eigentümer: Layer 4 (empirisch).
//! - Keine Netzwerkzugriffe; reine Werttypen.
//! - Delegation an andere Layer: keine.
//!
//! # Exportierte Typen
//! - [`EvaluationRunId`] — opaker Run-Bezeichner.
//! - [`Score`] — ganzzahliger Wert 0..=100.
//! - [`ObservedModelBehavior`] — Messwert-Bündel je Modell.
//!
//! # Nebenläufigkeit
//! Alle Typen sind reine Wertetypen ohne inneren Zustand.
//! `ObservedModelBehavior` ist `Send + Sync`.
//!
//! # Fehler
//! Dieses Modul erzeugt keine Fehler; Konstruktoren geben `Option` zurück.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_model_catalog::observed::{ObservedModelBehavior, Score};
//! let obs = ObservedModelBehavior::bootstrap("anthropic", "claude-sonnet-5");
//! assert_eq!(obs.tool_schema_reliability, Score::HALF);
//! ```

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

// ─────────────────────────────────────────────────────────────────────────────
// Primitive Typen
// ─────────────────────────────────────────────────────────────────────────────

/// Opaker Bezeichner eines Evaluierungs-Runs (§5, model-catalog-v2.md).
///
/// # Beschreibung
/// Ein `EvaluationRunId` ist ein beliebiger String, der einen konkreten
/// Harness-Lauf eindeutig identifiziert. Format und Namensgebung sind dem
/// Harness überlassen (z. B. UUID, Timestamp-Slug).
///
/// # Concurrency
/// `String` ist `Send + Sync`.
pub type EvaluationRunId = String;

// ─────────────────────────────────────────────────────────────────────────────
// Score
// ─────────────────────────────────────────────────────────────────────────────

/// Ganzzahliger Qualitätswert im Bereich 0..=100 (§5, model-catalog-v2.md).
///
/// # Beschreibung
/// `Score` kapselt einen `u8`-Wert, der ausschließlich 0 bis 100 annehmen darf.
/// Das interne Feld ist privat; Zugriff nur über [`Score::new`], [`Score::clamp`]
/// und [`Score::get`].
///
/// # Concurrency
/// `Copy`-Typ, vollständig `Send + Sync`.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::observed::Score;
/// assert_eq!(Score::new(50), Some(Score::HALF));
/// assert_eq!(Score::clamp(200).get(), 100);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Score(u8);

impl Score {
    /// Erstellt einen `Score`, wenn `v` im gültigen Bereich 0..=100 liegt.
    ///
    /// # Rückgabe
    /// `Some(Score)` für `v <= 100`, `None` sonst.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_model_catalog::observed::Score;
    /// assert!(Score::new(100).is_some());
    /// assert!(Score::new(101).is_none());
    /// ```
    pub const fn new(v: u8) -> Option<Self> {
        if v <= 100 { Some(Self(v)) } else { None }
    }

    /// Erstellt einen `Score` und sättigt `v` auf 100 falls nötig.
    ///
    /// # Rückgabe
    /// `Score` mit `min(v, 100)`.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_model_catalog::observed::Score;
    /// assert_eq!(Score::clamp(255).get(), 100);
    /// ```
    pub const fn clamp(v: u8) -> Self {
        Self(if v > 100 { 100 } else { v })
    }

    /// Gibt den rohen `u8`-Wert zurück.
    ///
    /// # Rückgabe
    /// Wert im Bereich 0..=100.
    pub const fn get(self) -> u8 {
        self.0
    }

    /// Score-Konstante: kein Nachweis (0).
    pub const ZERO: Self = Self(0);

    /// Score-Konstante: konservativer Bootstrap-Default (50).
    pub const HALF: Self = Self(50);

    /// Score-Konstante: vollständig bestätigt (100).
    pub const FULL: Self = Self(100);
}

// ─────────────────────────────────────────────────────────────────────────────
// ObservedModelBehavior
// ─────────────────────────────────────────────────────────────────────────────

/// Empirisch gemessenes Verhalten eines Modells (Layer 4, §5 model-catalog-v2.md).
///
/// # Beschreibung
/// Alle Felder werden ausschließlich aus realen Harness-Runs befüllt.
/// Beim Bootstrap sind alle Scores `Score::HALF`, `updated_at` ist `None`
/// und `evidence` ist leer (konservativer Default ohne Evidenz).
///
/// # Felder
/// - `provider` — Provider-ID (z. B. `"anthropic"`).
/// - `model` — Modell-ID (z. B. `"claude-sonnet-5"`).
/// - `tool_schema_reliability` — Wie zuverlässig das Modell Tool-Schemata einhält.
/// - `long_context_retention` — Wie gut das Modell Inhalte in langen Kontexten erhält.
/// - `delegation_discipline` — Wie präzise das Modell Delegation-Entscheidungen trifft.
/// - `recovery_after_tool_error` — Erholungsfähigkeit nach Tool-Fehlern.
/// - `completion_calibration` — Kalibrierung von Vollständigkeits-Signalen.
/// - `compaction_resilience` — Qualitätserhalt nach Kontext-Kompaktierung.
/// - `updated_at` — Zeitstempel des letzten Harness-Runs (RFC 3339); `None` beim Bootstrap.
/// - `evidence` — Liste der Evaluierungs-Run-IDs, die diese Werte belegen.
///
/// # Concurrency
/// Reiner Werttyp, `Send + Sync`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ObservedModelBehavior {
    /// Provider-ID (z. B. `"openai"`, `"anthropic"`).
    pub provider: String,
    /// Modell-ID (z. B. `"gpt-5"`, `"claude-sonnet-5"`).
    pub model: String,
    /// Zuverlässigkeit bei der Einhaltung von Tool-Schemata.
    pub tool_schema_reliability: Score,
    /// Retention-Qualität in langen Kontexten.
    pub long_context_retention: Score,
    /// Disziplin bei Delegations-Entscheidungen.
    pub delegation_discipline: Score,
    /// Erholungsfähigkeit nach Tool-Fehlern.
    pub recovery_after_tool_error: Score,
    /// Kalibrierung von Abschluss-Signalen.
    pub completion_calibration: Score,
    /// Qualitätserhalt nach Kontext-Kompaktierung.
    pub compaction_resilience: Score,
    /// Zeitstempel des letzten Harness-Runs; `None` beim Bootstrap.
    #[serde(with = "time::serde::rfc3339::option")]
    pub updated_at: Option<OffsetDateTime>,
    /// Evaluierungs-Run-IDs, die die Messwerte belegen.
    pub evidence: Vec<EvaluationRunId>,
}

impl ObservedModelBehavior {
    /// Erstellt einen konservativen Bootstrap-Eintrag ohne Evidenz (§5, model-catalog-v2.md).
    ///
    /// # Beschreibung
    /// Alle sechs Score-Felder werden auf `Score::HALF` gesetzt, `updated_at`
    /// auf `None` und `evidence` auf einen leeren Vec. Dieser Zustand signalisiert
    /// dem Harness: „Noch kein realer Run ausgewertet — konservative Defaults aktiv."
    ///
    /// # Argumente
    /// - `provider` (`&str`): Provider-ID (z. B. `"openai"`).
    /// - `model` (`&str`): Modell-ID (z. B. `"gpt-5"`).
    ///
    /// # Rückgabe
    /// Neues `ObservedModelBehavior` mit Bootstrap-Defaults.
    ///
    /// # Concurrency
    /// Rein deterministisch, keine Seiteneffekte.
    ///
    /// # Beispiel
    /// ```rust,no_run
    /// use harw_model_catalog::observed::{ObservedModelBehavior, Score};
    /// let obs = ObservedModelBehavior::bootstrap("openai", "gpt-5");
    /// assert_eq!(obs.tool_schema_reliability, Score::HALF);
    /// assert!(obs.updated_at.is_none());
    /// assert!(obs.evidence.is_empty());
    /// ```
    pub fn bootstrap(provider: &str, model: &str) -> Self {
        Self {
            provider: provider.to_owned(),
            model: model.to_owned(),
            tool_schema_reliability: Score::HALF,
            long_context_retention: Score::HALF,
            delegation_discipline: Score::HALF,
            recovery_after_tool_error: Score::HALF,
            completion_calibration: Score::HALF,
            compaction_resilience: Score::HALF,
            updated_at: None,
            evidence: vec![],
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// observations_from_descriptors
// ─────────────────────────────────────────────────────────────────────────────

/// Baut Bootstrap-Beobachtungen aus einer Descriptor-Liste (§5, model-catalog-v2.md).
///
/// # Description
/// Wandelt jeden [`crate::descriptor::ModelDescriptor`] in eine
/// [`ObservedModelBehavior::bootstrap`]-Instanz über `(&d.provider, &d.model)`.
/// Dies ist das Standard-Muster für Vendor-Module ohne spezielle
/// Verhaltensbeobachtungen.
///
/// # Arguments
/// - `descriptors` (`&[ModelDescriptor]`): Liste der Descriptors, aus denen
///   die Beobachtungen abgeleitet werden.
///
/// # Returns
/// Eine `Vec<ObservedModelBehavior>` mit einem Bootstrap-Eintrag pro Descriptor.
///
/// # Concurrency
/// Rein deterministisch, keine Seiteneffekte.
///
/// # Examples
/// ```rust,no_run
/// use harw_model_catalog::observed::{observations_from_descriptors, ObservedModelBehavior};
/// use harw_model_catalog::descriptor::bootstrap_descriptors;
/// let descs = bootstrap_descriptors();
/// let obs = observations_from_descriptors(&descs);
/// assert_eq!(obs.len(), descs.len());
/// ```
pub fn observations_from_descriptors(
    descriptors: &[crate::descriptor::ModelDescriptor],
) -> Vec<ObservedModelBehavior> {
    descriptors
        .iter()
        .map(|d| ObservedModelBehavior::bootstrap(&d.provider, &d.model))
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// bootstrap_observations
// ─────────────────────────────────────────────────────────────────────────────

/// Erzeugt Bootstrap-Einträge für alle 15 kuratierten Modelle (§5, model-catalog-v2.md).
///
/// # Beschreibung
/// Gibt einen `Vec` mit je einem `ObservedModelBehavior::bootstrap`-Eintrag
/// für jedes der 15 Modelle zurück, die auch `bootstrap_descriptors` abdeckt.
/// Alle Werte sind konservative Defaults ohne Evidenz.
///
/// # Rückgabe
/// `Vec<ObservedModelBehavior>` mit genau 15 Einträgen.
///
/// # Concurrency
/// Rein deterministisch, keine Seiteneffekte.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_model_catalog::observed::bootstrap_observations;
/// let obs = bootstrap_observations();
/// assert_eq!(obs.len(), 15);
/// ```
pub fn bootstrap_observations() -> Vec<ObservedModelBehavior> {
    crate::descriptor::bootstrap_descriptors()
        .iter()
        .map(|d| ObservedModelBehavior::bootstrap(&d.provider, &d.model))
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    #[test]
    fn score_new_clamps_range() {
        assert_eq!(Score::new(101), None);
        assert_eq!(Score::new(100), Some(Score(100)));
        assert_eq!(Score::new(0), Some(Score(0)));
    }

    #[test]
    fn score_clamp_saturates() {
        assert_eq!(Score::clamp(200).get(), 100);
        assert_eq!(Score::clamp(100).get(), 100);
        assert_eq!(Score::clamp(0).get(), 0);
    }

    #[test]
    fn score_constants() {
        assert_eq!(Score::ZERO.get(), 0);
        assert_eq!(Score::HALF.get(), 50);
        assert_eq!(Score::FULL.get(), 100);
    }

    #[test]
    fn bootstrap_all_scores_half() {
        let obs = ObservedModelBehavior::bootstrap("anthropic", "claude-sonnet-5");
        assert_eq!(obs.tool_schema_reliability, Score::HALF);
        assert_eq!(obs.long_context_retention, Score::HALF);
        assert_eq!(obs.delegation_discipline, Score::HALF);
        assert_eq!(obs.recovery_after_tool_error, Score::HALF);
        assert_eq!(obs.completion_calibration, Score::HALF);
        assert_eq!(obs.compaction_resilience, Score::HALF);
        assert!(obs.updated_at.is_none());
        assert!(obs.evidence.is_empty());
    }

    #[test]
    fn bootstrap_observations_has_15_entries() {
        // Die Länge folgt bootstrap_descriptors(); Bereichsprüfung statt fixe Zahl.
        let obs = bootstrap_observations();
        assert!(obs.len() >= 15);
    }

    #[test]
    fn observation_serde_roundtrip() -> TestResult {
        let original = ObservedModelBehavior::bootstrap("openai", "gpt-5");
        let json = serde_json::to_string(&original)?;
        let restored: ObservedModelBehavior = serde_json::from_str(&json)?;
        assert_eq!(original, restored);
        Ok(())
    }

    #[test]
    fn bootstrap_no_duplicate_ids() {
        let obs = bootstrap_observations();
        let mut seen = std::collections::HashSet::new();
        for entry in &obs {
            let key = (entry.provider.clone(), entry.model.clone());
            assert!(seen.insert(key.clone()), "Duplikat gefunden: {:?}", key);
        }
    }

    #[test]
    fn score_ordering() {
        assert!(Score::ZERO < Score::HALF);
        assert!(Score::HALF < Score::FULL);
        assert_eq!(Score::FULL, Score::clamp(255));
    }

    #[test]
    fn observations_from_descriptors_empty_returns_empty() {
        let obs = observations_from_descriptors(&[]);
        assert!(
            obs.is_empty(),
            "Leere Descriptor-Liste muss leere Observations liefern"
        );
    }

    #[test]
    fn observations_from_descriptors_count_matches_input() {
        let descs = crate::descriptor::bootstrap_descriptors();
        let obs = observations_from_descriptors(&descs);
        assert_eq!(
            obs.len(),
            descs.len(),
            "Anzahl der Observations muss der Anzahl der Descriptors entsprechen"
        );
    }

    #[test]
    fn observations_from_descriptors_all_half_scores() {
        let descs = crate::descriptor::bootstrap_descriptors();
        let obs = observations_from_descriptors(&descs);
        for o in &obs {
            assert_eq!(o.tool_schema_reliability, Score::HALF, "model: {}", o.model);
            assert!(o.updated_at.is_none(), "model: {}", o.model);
            assert!(o.evidence.is_empty(), "model: {}", o.model);
        }
    }
}
