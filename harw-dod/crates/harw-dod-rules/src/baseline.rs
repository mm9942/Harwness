//! Baseline-Typen, lokal zu `harw-dod-rules`.
//!
//! # Verantwortungsbereich
//! Dieses Modul wurde während des Refactorings von
//! `harw-knowledge::security::Baseline` nach `harw-dod-rules` geboren, damit
//! `harw-sentinel` nicht die gesamte Wissensbasis samt HTTP-Client-Stack
//! importieren muss (siehe detaillierte Historie in früheren Revisionen).
//!
//! # Aktuelles Datenmodell
//! Eine [`Baseline`] beschreibt einen erlaubten Wertebereich für eine bestimmte
//! Metrik. Regeln wie [`crate::rules::BaselineDeviationRule`] gruppieren
//! Baselines nach ihrer Metrik und prüfen, ob ein beobachteter Messwert
//! innerhalb des Bereichs liegt. Der Lebenszyklus-Status
//! ([`PalaceStatus`]) bestimmt, ob eine Verletzung direkt als
//! `RuleTriggered` (`Established`) oder nur als `Anomaly` (`Provisional`)
//! gemeldet wird.
//!
//! # Nebenläufigkeit
//! [`Baseline`] und [`PalaceStatus`] sind reine `Send + Sync`-Daten ohne
//! geteilten Zustand.

use std::borrow::Cow;
use std::fmt;

use harw_dod_signals::{Hardness, SecurityEvidence};

use crate::finding::FindingKind;

/// Lebenszyklus-Status einer [`Baseline`] (lokal zu `harw-dod-rules`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PalaceStatus {
    /// Durable truth, die eine Regel direkt auslösen darf.
    Established,
    /// Beobachtet, aber noch nicht Review-gehärtet — nur `Anomaly`.
    Provisional,
    /// Zurückgezogen: kein aktiver Bezugspunkt mehr, gate't nichts.
    Superseded,
}

/// Eine eingefrorene Verhaltens-Baseline, gegen die eine Regel neue
/// Beobachtungen prüft (lokal zu `harw-dod-rules`; siehe Moduldoku).
#[derive(Debug, Clone, PartialEq)]
pub struct Baseline {
    /// Stabiler Bezeichner, als formatierte Zeichenkette gespeichert.
    pub id: String,
    /// Menschenlesbare Beschreibung dessen, was diese Baseline
    /// charakterisiert.
    pub title: String,
    /// Metrik, für die diese Baseline einen erwarteten Wertebereich festlegt.
    pub metric: Cow<'static, str>,
    /// Untere Grenze des erwarteten Wertebereichs (inklusiv).
    pub min: f64,
    /// Obere Grenze des erwarteten Wertebereichs (inklusiv).
    pub max: f64,
    /// Epistemische Distanz der Beweise, aus denen diese Baseline abgeleitet
    /// wurde.
    pub hardness: Hardness,
    /// Die eingefrorenen Beobachtungen, aus denen diese Baseline abgeleitet
    /// wurde.
    pub evidence: SecurityEvidence,
    /// Lebenszyklus-Status: gate't, ob eine Regel `RuleTriggered`
    /// (`Established`) oder nur `Anomaly` (`Provisional`) auslösen darf.
    pub status: PalaceStatus,
    /// Thematische Tags, unabhängig vom Verweisgraphen.
    pub tags: Vec<String>,
}

impl Baseline {
    /// Erstellt eine frische Baseline für eine Metrik mit einem
    /// erlaubten Wertebereich.
    ///
    /// # Errors
    /// - [`BaselineError::InvalidRange`] wenn `min > max`.
    /// - [`BaselineError::EvidenceEncoding`] wenn die leere Startevidenz
    ///   nicht kodierbar ist (in der Praxis nicht erreichbar für leere
    ///   `samples`/`events`).
    pub fn new(
        id: impl fmt::Display,
        metric: impl Into<Cow<'static, str>>,
        min: f64,
        max: f64,
    ) -> Result<Self, BaselineError> {
        if min > max {
            return Err(BaselineError::InvalidRange {
                metric: metric.into().into_owned(),
                min,
                max,
            });
        }
        let id = id.to_string();
        Ok(Self {
            id: id.clone(),
            title: id,
            metric: metric.into(),
            min,
            max,
            hardness: Hardness::Observed,
            evidence: SecurityEvidence::capture(vec![], vec![], jiff::Timestamp::UNIX_EPOCH)
                .map_err(|source| BaselineError::EvidenceEncoding {
                    reason: source.to_string(),
                })?,
            status: PalaceStatus::Provisional,
            tags: Vec::new(),
        })
    }

    /// Voller Konstruktor für Aufrufer, die Titel, Härte und Evidence selbst
    /// vorgeben.
    #[must_use]
    pub fn with_details(
        id: impl fmt::Display,
        title: impl Into<String>,
        metric: impl Into<Cow<'static, str>>,
        min: f64,
        max: f64,
        hardness: Hardness,
        evidence: SecurityEvidence,
    ) -> Self {
        Self {
            id: id.to_string(),
            title: title.into(),
            metric: metric.into(),
            min,
            max,
            hardness,
            evidence,
            status: PalaceStatus::Provisional,
            tags: Vec::new(),
        }
    }

    /// Befördert diese Baseline zu `Established`.
    ///
    /// # Errors
    /// [`BaselineError::PromotionNotReviewed`], sofern `reviewed` nicht
    /// `true` ist.
    pub fn promote_to_established(&mut self, reviewed: bool) -> Result<(), BaselineError> {
        if !reviewed {
            return Err(BaselineError::PromotionNotReviewed {
                from: format!("baseline/{}:provisional", self.id),
                to: format!("baseline/{}:established", self.id),
            });
        }
        self.status = PalaceStatus::Established;
        Ok(())
    }

    /// Liefert die der Baseline zugeordnete Metrik.
    ///
    /// Aus Kompatibilitätsgründen zu bestehenden Regeln, die von einem
    /// `genre`-Konzept sprechen, ist diese Methode auch als `genre`
    /// aufrufbar.
    #[must_use]
    pub fn metric(&self) -> &str {
        self.metric.as_ref()
    }

    /// Alias für [`Self::metric`], um älteren Regelcode weiterhin zu
    /// unterstützen.
    #[must_use]
    pub fn genre(&self) -> &str {
        self.metric()
    }

    /// Prüft, ob ein Skalarwert innerhalb des erlaubten Bereichs liegt.
    #[must_use]
    pub fn contains(&self, value: f64) -> bool {
        (self.min..=self.max).contains(&value)
    }
}

/// Fehler dieses Moduls.
#[derive(Debug, Clone, PartialEq)]
pub enum BaselineError {
    /// Eine Beförderung wurde ohne gesetztes `reviewed` versucht.
    PromotionNotReviewed {
        /// Label des Ausgangszustands (`baseline/<id>:provisional`).
        from: String,
        /// Label des angestrebten Zielzustands (`baseline/<id>:established`).
        to: String,
    },
    /// Der angegebene Wert passt nicht zur Baseline (z. B. kein Skalar).
    UnsupportedValue,
    /// Es gibt keine Baseline für die angefragte Metrik.
    MissingBaseline,
    /// Der Baseline-Bereich ist leer oder negativ (`min > max`).
    InvalidRange {
        /// Name der betroffenen Metrik.
        metric: String,
        /// Untere Grenze.
        min: f64,
        /// Obere Grenze.
        max: f64,
    },
    /// Die leere Startevidenz einer neuen Baseline ließ sich nicht kodieren
    /// (siehe [`harw_dod_signals::SecurityEvidence::capture`]). Trägt die
    /// Fehlermeldung als `String` statt des fremden `SignalsError`, da
    /// dieser weder `Clone` noch `PartialEq` ableitet.
    EvidenceEncoding {
        /// Die Fehlermeldung des zugrunde liegenden Kodierungsfehlers.
        reason: String,
    },
}

impl fmt::Display for BaselineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PromotionNotReviewed { from, to } => write!(
                f,
                "baseline promotion from {from} to {to} refused: not reviewed"
            ),
            Self::UnsupportedValue => write!(f, "value type not supported by baseline"),
            Self::MissingBaseline => write!(f, "no baseline defined for metric"),
            Self::InvalidRange { metric, min, max } => {
                write!(f, "invalid baseline range for '{metric}': {min} > {max}")
            }
            Self::EvidenceEncoding { reason } => {
                write!(f, "failed to encode empty baseline evidence: {reason}")
            }
        }
    }
}

impl std::error::Error for BaselineError {}

/// Leitet aus dem Lebenszyklus-Status einer Baseline ab, welche Befund-Art
/// eine Regel gegen sie auslösen darf.
#[must_use]
pub fn finding_kind_for_status(status: PalaceStatus) -> Option<FindingKind> {
    match status {
        PalaceStatus::Established => Some(FindingKind::RuleTriggered),
        PalaceStatus::Provisional => Some(FindingKind::Anomaly),
        PalaceStatus::Superseded => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    fn evidence() -> TestResult<SecurityEvidence> {
        SecurityEvidence::capture(vec![], vec![], jiff::Timestamp::UNIX_EPOCH)
            .map_err(ctx("empty evidence always encodes"))
    }

    #[test]
    fn test_established_yields_rule_triggered() {
        assert_eq!(
            finding_kind_for_status(PalaceStatus::Established),
            Some(FindingKind::RuleTriggered)
        );
    }

    #[test]
    fn test_provisional_yields_anomaly_only() {
        assert_eq!(
            finding_kind_for_status(PalaceStatus::Provisional),
            Some(FindingKind::Anomaly)
        );
    }

    #[test]
    fn test_superseded_yields_no_finding_kind() {
        assert_eq!(finding_kind_for_status(PalaceStatus::Superseded), None);
    }

    #[test]
    fn test_established_and_provisional_yield_different_kinds() {
        assert_ne!(
            finding_kind_for_status(PalaceStatus::Established),
            finding_kind_for_status(PalaceStatus::Provisional)
        );
    }

    #[test]
    fn test_baseline_new_starts_provisional_and_formats_a_display_id() -> TestResult {
        let baseline =
            Baseline::new("baseline/cpu", "cpu", 0.0, 100.0).map_err(ctx("baseline new"))?;

        assert_eq!(baseline.status, PalaceStatus::Provisional);
        assert_eq!(baseline.id, "baseline/cpu");
        assert_eq!(baseline.metric, "cpu");
        Ok(())
    }

    #[test]
    fn test_baseline_contains_works() -> TestResult {
        let baseline = Baseline::new("cpu", "cpu", 0.0, 80.0).map_err(ctx("baseline new"))?;
        assert!(baseline.contains(42.0));
        assert!(!baseline.contains(99.9));
        Ok(())
    }

    #[test]
    fn test_baseline_new_rejects_invalid_range() -> TestResult {
        let result = Baseline::new("cpu", "cpu", 80.0, 0.0);
        let Err(err) = result else {
            return Err(crate::test_support::TestError::Unexpected(
                "Err erwartet".into(),
            ));
        };
        assert!(matches!(err, BaselineError::InvalidRange { .. }));
        Ok(())
    }

    #[test]
    fn test_baseline_promotion_without_review_is_rejected() -> TestResult {
        let mut baseline = Baseline::with_details(
            "baseline/cpu",
            "cpu baseline",
            "cpu",
            0.0,
            100.0,
            Hardness::Observed,
            evidence()?,
        );

        let result = baseline.promote_to_established(false);
        let Err(error) = result else {
            return Err(crate::test_support::TestError::Unexpected(
                "unreviewed promotion must be refused".into(),
            ));
        };

        assert!(matches!(error, BaselineError::PromotionNotReviewed { .. }));
        assert_eq!(baseline.status, PalaceStatus::Provisional);
        Ok(())
    }

    #[test]
    fn test_baseline_promotion_with_review_becomes_established() -> TestResult {
        let mut baseline = Baseline::with_details(
            "baseline/cpu",
            "cpu baseline",
            "cpu",
            0.0,
            100.0,
            Hardness::Observed,
            evidence()?,
        );

        baseline
            .promote_to_established(true)
            .map_err(ctx("reviewed promotion succeeds"))?;

        assert_eq!(baseline.status, PalaceStatus::Established);
        Ok(())
    }

    #[test]
    fn test_baseline_new_accepts_a_harw_knowledge_artifact_id() -> TestResult {
        let id = harw_knowledge::artifact::ArtifactId::new("baseline/from-knowledge");
        let baseline = Baseline::new(id, "cpu", 0.0, 100.0).map_err(ctx("baseline new"))?;

        assert_eq!(baseline.id, "baseline/from-knowledge");
        Ok(())
    }
}
