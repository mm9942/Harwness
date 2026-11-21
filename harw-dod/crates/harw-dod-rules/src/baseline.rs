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
//! gemeldet wird; eine `Superseded`-Baseline ist kein aktiver Bezugspunkt
//! mehr und nimmt an keiner Prüfung teil.
//!
//! # Eindeutigkeit (PL-100 WP1.7)
//! Je Metrik gibt es höchstens **eine aktive** (nicht `Superseded`)
//! Baseline. Zwei aktive Baselines derselben Metrik sind ein Fehler
//! ([`BaselineError::DuplicateMetric`]), kein „die letzte gewinnt": welcher
//! von zwei Bereichen gilt, hinge sonst von der Reihenfolge des Satzes ab.
//! [`check_baselines`] prüft einen Satz vor der Verwendung; die Regel selbst
//! bewertet eine mehrdeutige Metrik nicht, sondern meldet den Satz als
//! eigenen Befund (fail-closed, siehe [`crate::rules::BaselineDeviationRule`]).
//!
//! # Wertebereiche
//! Eine Grenze darf nie NaN sein, und `min <= max` muss gelten
//! ([`BaselineError::InvalidRange`]). Unendliche Grenzen sind erlaubt und
//! bedeuten einen einseitigen Bereich (z. B. `f64::NEG_INFINITY..=100.0`).
//!
//! # Nebenläufigkeit
//! [`Baseline`] und [`PalaceStatus`] sind reine `Send + Sync`-Daten ohne
//! geteilten Zustand.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use harw_dod_signals::{Hardness, SecurityEvidence};
use harw_untrusted_text::escape_line;

use crate::finding::FindingKind;

// Obergrenze (in Zeichen) für einen Metriknamen in einer Fehlermeldung: der
// Name kann aus einer modellgeschriebenen Wissensnotiz stammen.
const ERROR_METRIC_MAX_CHARS: usize = 64;

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
    /// - [`BaselineError::InvalidRange`] wenn `min > max` oder eine Grenze
    ///   NaN ist (unendliche Grenzen sind erlaubt, siehe Moduldoku).
    /// - [`BaselineError::EvidenceEncoding`] wenn die leere Startevidenz
    ///   nicht kodierbar ist (in der Praxis nicht erreichbar für leere
    ///   `samples`/`events`).
    pub fn new(
        id: impl fmt::Display,
        metric: impl Into<Cow<'static, str>>,
        min: f64,
        max: f64,
    ) -> Result<Self, BaselineError> {
        let metric: Cow<'static, str> = metric.into();
        check_range(metric.as_ref(), min, max)?;
        let id = id.to_string();
        Ok(Self {
            id: id.clone(),
            title: id,
            metric,
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
    ///
    /// NaN liegt nie im Bereich. Ob ein nicht endlicher Wert überhaupt
    /// bewertet werden darf, entscheidet der Aufrufer:
    /// [`crate::rules::BaselineDeviationRule`] bewertet ihn nicht.
    #[must_use]
    pub fn contains(&self, value: f64) -> bool {
        (self.min..=self.max).contains(&value)
    }

    // Ob diese Baseline ein aktiver Bezugspunkt ist (nicht `Superseded`).
    fn is_active(&self) -> bool {
        self.status != PalaceStatus::Superseded
    }
}

// Prüft einen Wertebereich: keine Grenze NaN, `min <= max`. Unendliche
// Grenzen sind erlaubt (einseitiger Bereich, siehe Moduldoku).
fn check_range(metric: &str, min: f64, max: f64) -> Result<(), BaselineError> {
    if min.is_nan() || max.is_nan() || min > max {
        return Err(BaselineError::InvalidRange {
            metric: metric.to_owned(),
            min,
            max,
        });
    }
    Ok(())
}

/// Prüft einen Baseline-Satz, bevor er einer Regel übergeben wird.
///
/// # Description
/// Betrachtet nur aktive Baselines (nicht `Superseded`, siehe Moduldoku).
/// Jede aktive Baseline braucht einen gültigen Wertebereich, und je Metrik
/// darf es höchstens eine aktive Baseline geben. Gemeldet wird der erste
/// Verstoß in der Reihenfolge des Satzes. Wer Baselines lädt (z. B. aus
/// `/etc`), weist einen Satz mit Fehler ab, statt ihn zu verwenden; die
/// Regel [`crate::rules::BaselineDeviationRule`] bewertet eine betroffene
/// Metrik ohnehin nicht und meldet sie stattdessen als mehrdeutigen bzw.
/// ungültigen Satz.
///
/// # Arguments
/// - `baselines` (`&[Baseline]`): der zu prüfende Satz.
///
/// # Returns
/// `Ok(())`, wenn jede aktive Metrik eindeutig und gültig ist.
///
/// # Errors
/// - [`BaselineError::InvalidRange`]: eine aktive Baseline hat eine
///   NaN-Grenze oder `min > max`.
/// - [`BaselineError::DuplicateMetric`]: eine Metrik hat mehr als eine
///   aktive Baseline.
///
/// # Concurrency
/// Rein, ohne geteilten Zustand.
///
/// # Examples
/// ```rust
/// use harw_dod_rules::baseline::{Baseline, BaselineError, check_baselines};
///
/// # fn main() -> Result<(), BaselineError> {
/// let first = Baseline::new("baseline/a", "cpu-load", 0.0, 80.0)?;
/// let second = Baseline::new("baseline/b", "cpu-load", 0.0, 90.0)?;
/// assert_eq!(check_baselines(std::slice::from_ref(&first)), Ok(()));
/// assert_eq!(
///     check_baselines(&[first, second]),
///     Err(BaselineError::DuplicateMetric {
///         metric: "cpu-load".to_owned()
///     })
/// );
/// # Ok(())
/// # }
/// ```
pub fn check_baselines(baselines: &[Baseline]) -> Result<(), BaselineError> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for baseline in baselines.iter().filter(|baseline| baseline.is_active()) {
        check_range(baseline.metric(), baseline.min, baseline.max)?;
        if !seen.insert(baseline.metric()) {
            return Err(BaselineError::DuplicateMetric {
                metric: baseline.metric().to_owned(),
            });
        }
    }
    Ok(())
}

/// Ordnet jeder Metrik ihre aktive Baseline zu, ohne „die letzte gewinnt".
///
/// # Description
/// `Superseded`-Baselines fehlen im Index. Eine Metrik mit ungültigem
/// Bereich ergibt [`BaselineError::InvalidRange`], eine Metrik mit mehr als
/// einer aktiven Baseline [`BaselineError::DuplicateMetric`] (auch wenn
/// eine davon ungültig ist). Das Ergebnis hängt nicht von der Reihenfolge
/// des Satzes ab.
///
/// # Arguments
/// - `baselines` (`&[Baseline]`): der Satz aus dem
///   [`crate::rule::RuleContext`].
///
/// # Returns
/// Je Metrik die eine aktive Baseline oder den Grund, warum die Metrik
/// nicht bewertet werden darf.
pub(crate) fn active_baselines_by_metric(
    baselines: &[Baseline],
) -> BTreeMap<&str, Result<&Baseline, BaselineError>> {
    let mut index: BTreeMap<&str, Result<&Baseline, BaselineError>> = BTreeMap::new();
    for baseline in baselines.iter().filter(|baseline| baseline.is_active()) {
        let metric = baseline.metric();
        let entry = if index.contains_key(metric) {
            Err(BaselineError::DuplicateMetric {
                metric: metric.to_owned(),
            })
        } else {
            check_range(metric, baseline.min, baseline.max).map(|()| baseline)
        };
        index.insert(metric, entry);
    }
    index
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
    /// Der angegebene Wert passt nicht zur Baseline (z. B. kein endlicher
    /// Skalar: NaN oder unendlich, PL-100 WP1.7).
    UnsupportedValue,
    /// Es gibt keine Baseline für die angefragte Metrik.
    MissingBaseline,
    /// Der Baseline-Bereich ist leer oder negativ (`min > max`), oder eine
    /// Grenze ist NaN.
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
    /// Eine Metrik hat mehr als eine aktive (nicht `Superseded`) Baseline
    /// (PL-100 WP1.7): welcher Bereich gilt, ist mehrdeutig.
    DuplicateMetric {
        /// Name der mehrfach belegten Metrik.
        metric: String,
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
            Self::InvalidRange { metric, min, max } => write!(
                f,
                "invalid baseline range for '{}': min {min}, max {max}",
                escape_line(metric, ERROR_METRIC_MAX_CHARS)
            ),
            Self::EvidenceEncoding { reason } => {
                write!(f, "failed to encode empty baseline evidence: {reason}")
            }
            Self::DuplicateMetric { metric } => write!(
                f,
                "metric '{}' has more than one active baseline",
                escape_line(metric, ERROR_METRIC_MAX_CHARS)
            ),
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

    // Baseline mit eigenem Status, ohne den Bereich zu prüfen (wie
    // `with_details` oder ein Struct-Literal aus `harw-knowledge`).
    fn unchecked(
        id: &str,
        metric: &'static str,
        min: f64,
        max: f64,
        status: PalaceStatus,
    ) -> TestResult<Baseline> {
        let mut baseline =
            Baseline::with_details(id, id, metric, min, max, Hardness::Observed, evidence()?);
        baseline.status = status;
        Ok(baseline)
    }

    /// PL-100 WP1.7: eine NaN-Grenze ist kein Wertebereich.
    #[test]
    fn test_baseline_new_rejects_nan_bounds() {
        for (min, max) in [(f64::NAN, 80.0), (0.0, f64::NAN), (f64::NAN, f64::NAN)] {
            assert!(
                matches!(
                    Baseline::new("cpu", "cpu", min, max),
                    Err(BaselineError::InvalidRange { .. })
                ),
                "NaN-Grenze ({min}, {max}) muss abgewiesen werden"
            );
        }
    }

    /// Unendliche Grenzen sind ein einseitiger Bereich, NaN liegt nie darin.
    #[test]
    fn test_baseline_new_accepts_infinite_bounds_as_one_sided_range() -> TestResult {
        let baseline = Baseline::new("cpu", "cpu", f64::NEG_INFINITY, 100.0)
            .map_err(ctx("einseitiger Bereich ist gültig"))?;
        assert!(baseline.contains(-1.0e300));
        assert!(!baseline.contains(100.5));
        assert!(!baseline.contains(f64::NAN));
        Ok(())
    }

    #[test]
    fn test_check_baselines_accepts_unique_metrics() -> TestResult {
        let set = [
            unchecked("a", "cpu-load", 0.0, 80.0, PalaceStatus::Provisional)?,
            unchecked("b", "mem-used", 0.0, 90.0, PalaceStatus::Established)?,
        ];
        assert_eq!(check_baselines(&set), Ok(()));
        assert_eq!(check_baselines(&[]), Ok(()));
        Ok(())
    }

    /// PL-100 WP1.7: zwei aktive Baselines derselben Metrik sind ein Fehler,
    /// unabhängig von Status und Reihenfolge.
    #[test]
    fn test_check_baselines_rejects_duplicate_active_metric() -> TestResult {
        let provisional = unchecked("a", "cpu-load", 0.0, 80.0, PalaceStatus::Provisional)?;
        let established = unchecked("b", "cpu-load", 0.0, 95.0, PalaceStatus::Established)?;
        let other = unchecked("c", "mem-used", 0.0, 90.0, PalaceStatus::Provisional)?;
        let expected = Err(BaselineError::DuplicateMetric {
            metric: "cpu-load".to_owned(),
        });

        assert_eq!(
            check_baselines(&[provisional.clone(), other.clone(), established.clone()]),
            expected
        );
        assert_eq!(
            check_baselines(&[established, other, provisional]),
            expected
        );
        Ok(())
    }

    /// Eine `Superseded`-Baseline ist kein aktiver Bezugspunkt: sie neben
    /// ihrer Nachfolgerin ist kein Duplikat, und ihr Bereich zählt nicht.
    #[test]
    fn test_check_baselines_ignores_superseded_baselines() -> TestResult {
        let set = [
            unchecked("old", "cpu-load", 0.0, 10.0, PalaceStatus::Superseded)?,
            unchecked("broken", "cpu-load", 50.0, 10.0, PalaceStatus::Superseded)?,
            unchecked("new", "cpu-load", 0.0, 80.0, PalaceStatus::Established)?,
        ];
        assert_eq!(check_baselines(&set), Ok(()));
        Ok(())
    }

    /// Auch ohne `Baseline::new` gebaute Baselines (`with_details`,
    /// Struct-Literal) werden auf ihren Bereich geprüft.
    #[test]
    fn test_check_baselines_rejects_invalid_ranges_built_without_validation() -> TestResult {
        let reversed = unchecked("a", "cpu-load", 80.0, 0.0, PalaceStatus::Provisional)?;
        let nan = unchecked("b", "mem-used", f64::NAN, 1.0, PalaceStatus::Established)?;

        assert!(matches!(
            check_baselines(&[reversed]),
            Err(BaselineError::InvalidRange { .. })
        ));
        assert!(matches!(
            check_baselines(&[nan]),
            Err(BaselineError::InvalidRange { .. })
        ));
        Ok(())
    }

    /// Der Index der Regel kennt kein „die letzte gewinnt": eine doppelte
    /// Metrik ist in jeder Reihenfolge ein Fehler, andere Metriken bleiben
    /// bewertbar, `Superseded` fehlt.
    #[test]
    fn test_active_index_marks_duplicates_regardless_of_order() -> TestResult {
        let wide = unchecked("wide", "cpu-load", 0.0, 100.0, PalaceStatus::Provisional)?;
        let narrow = unchecked("narrow", "cpu-load", 0.0, 80.0, PalaceStatus::Provisional)?;
        let other = unchecked("mem", "mem-used", 0.0, 90.0, PalaceStatus::Established)?;
        let retired = unchecked("io", "io-wait", 0.0, 5.0, PalaceStatus::Superseded)?;

        for set in [
            [wide.clone(), narrow.clone(), other.clone(), retired.clone()],
            [narrow.clone(), retired.clone(), other.clone(), wide.clone()],
        ] {
            let index = active_baselines_by_metric(&set);
            assert!(matches!(
                index.get("cpu-load"),
                Some(Err(BaselineError::DuplicateMetric { .. }))
            ));
            assert!(matches!(index.get("mem-used"), Some(Ok(b)) if b.id == "mem"));
            assert!(index.get("io-wait").is_none());
            assert_eq!(index.len(), 2);
        }
        Ok(())
    }

    #[test]
    fn test_active_index_reports_invalid_range_of_a_single_baseline() -> TestResult {
        let set = [unchecked(
            "a",
            "cpu-load",
            f64::NAN,
            80.0,
            PalaceStatus::Established,
        )?];
        let index = active_baselines_by_metric(&set);
        assert!(matches!(
            index.get("cpu-load"),
            Some(Err(BaselineError::InvalidRange { .. }))
        ));
        Ok(())
    }

    /// Der Metrikname kann aus einer modellgeschriebenen Notiz stammen: die
    /// Fehlermeldung escapt ihn.
    #[test]
    fn test_error_messages_escape_the_metric_name() {
        let duplicate = BaselineError::DuplicateMetric {
            metric: "cpu\u{202E}load\nFAKE".to_owned(),
        }
        .to_string();
        let range = BaselineError::InvalidRange {
            metric: "cpu\u{202E}load".to_owned(),
            min: f64::NAN,
            max: 1.0,
        }
        .to_string();

        for message in [&duplicate, &range] {
            assert!(!message.contains('\u{202E}'), "{message}");
            assert!(!message.contains('\n'), "{message}");
            assert!(message.contains("cpu\\u{202e}load"), "{message}");
        }
        assert!(duplicate.contains("more than one active baseline"));
        assert!(range.contains("min NaN, max 1"));
    }
}
