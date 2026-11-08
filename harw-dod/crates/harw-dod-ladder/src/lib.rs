//! Die Eskalationsleiter: [`Ladder`] (PL-100 WP 3.0, aus `harw-dod-escalate`
//! herausgelöst).
//!
//! # Zweck
//! Zwei Fragen, beide reine Funktionen ohne Systemuhr, Zufall oder Historie:
//! „ist diese Aktion ab dieser Stufe zulässig" ([`Ladder::admissible`]) und
//! „welche Stufe hat dieser triagierte Befund erreicht" ([`Ladder::stage_for`],
//! [`Ladder::stage_for_or_reject`]).
//!
//! # Warum eine eigene Crate (Leitprinzip I8)
//! Die Leiter signiert nichts und schreibt keinen Zustand. Bis PL-100 WP 3.0
//! lag sie in `harw-dod-escalate`, das zusätzlich den v1-Beleg baute und den
//! `FreezeStore` schrieb; wer nur die Stufe eines Befundes wissen wollte
//! (damals `harw-plan-bridge`), zog damit Signier- und Speichercode in seine
//! Hülle.
//! Diese Crate hängt nur an `harw-dod-rules`, `harw-dod-signals` und dem
//! signierfreien Vokabular `harw-dod-warden-types` — nie an
//! `harw-dod-warden-proto`. `harw-dod-escalate` re-exportiert sie unter den
//! bisherigen Pfaden (`harw_dod_escalate::Ladder`,
//! `harw_dod_escalate::ladder::Ladder`).
//!
//! # Die Zulässigkeitsfrage: aufrufen statt nachbauen
//! [`Ladder::admissible`] beantwortet, ob eine Aktion ab einer Stufe
//! durchgesetzt werden darf, indem sie ausschließlich
//! [`WardenAction::is_admissible_from`] aufruft — die in
//! `harw-dod-warden-types` vollständig deklarierte, compilergeprüfte Matrix
//! (`FreezeCgroup`/`ThawCgroup` ab `RuleTriggered`,
//! `IsolateNetwork`/`LiftIsolation`/`KillProcessTree` erst ab `Escalated`).
//! Diese Crate hält **keine eigene Kopie** dieser Matrix: eine zweite Kopie
//! driftet, sobald eine Aktion hinzukommt oder eine Stufe umbenannt wird.
//!
//! # Die Aufstiegsregel in Worten
//! **Ein `Finding<Triaged>` erreicht überhaupt erst die unterste Stufe
//! (`EscalationStage::RuleTriggered`), wenn es zwei Bedingungen gleichzeitig
//! erfüllt: es ist keine bloße Abweichung, sondern eine ausgelöste Regel
//! (`FindingKind::RuleTriggered` statt `Anomaly` — die Baseline-Regel in
//! `harw-dod-rules` sorgt bereits dafür, dass eine unbestätigte Beobachtung
//! diese Art nie trägt), und ein Reviewer hat es als echt bestätigt
//! (`Verdict::Confirmed` statt `FalsePositive`/`NeedsReview`). Erfüllt es
//! beides nicht, gibt es für diesen Befund überhaupt keine Stufe — er
//! rechtfertigt keine Aktion.**
//!
//! **Von `RuleTriggered` steigt ein Befund nur dann auf die zweite Stufe
//! (`EscalationStage::Escalated`) auf, wenn er gleichzeitig das
//! schwerstmögliche Gewicht (`Severity::Critical`) UND den härtesten
//! Nachweisgrad (`Hardness::Observed`, direkt beobachtet statt verknüpft
//! oder geschlossen) trägt.** Beide Schwellen müssen gleichzeitig erreicht
//! sein — nicht nur eine. Diese Konjunktion ist bewusst konservativ: die
//! Aktionen, die `Escalated` freischaltet (`IsolateNetwork`, `LiftIsolation`,
//! `KillProcessTree`), sind teuer oder irreversibel (siehe
//! `harw-dod-warden-types/src/action.rs`-Moduldoku, Abschnitt „Die
//! Zulässigkeitsmatrix in Worten") — ein Befund mit mittlerem Gewicht oder
//! nur verknüpftem Nachweis reicht dafür nicht, egal wie oft er auftritt.
//!
//! Es gibt keine dritte Stufe und keinen Zwischenzustand: [`Ladder::stage_for`]
//! liefert entweder `None`, `Some(RuleTriggered)` oder `Some(Escalated)` —
//! keine Zählung über die Zeit, keine Häufigkeitsschwelle.
//!
//! `Verdict::Confirmed` stammt nie aus einem Modell: `harw_dod_rules::triage_record`
//! deckelt ein Modell-`Confirmed` auf `NeedsReview` (PL-100 WP0.11; Invariante 2
//! des Bedrohungsmodells: ein Modell-`Confirmed` ergibt nie
//! `Ladder::stage_for = Some`).
//!
//! # Wer die Leiter linkt (Leitprinzip I8)
//! Nur die Eskalationsseite (`harw-dod-escalate`, später `harw-escalator`).
//! Modellnahe Prozesse (`harw-runtime`, `harw-ops`, `harw-cli`, `harw`, ihre
//! Bausteine und Einbetter) linken sie nicht: `xtask/arch-policy.toml`
//! verbietet sie ihnen transitiv (`[[forbidden_crates]]`). Auch
//! `harw-plan-bridge` (Knoten AW6-04) nutzte bis zu dieser Trennung
//! [`Ladder::stage_for_or_reject`]; es prüft die Frage „gibt es überhaupt
//! eine Stufe" seit WP 3.0 selbst und pinnt die Gleichheit mit
//! [`Ladder::stage_for`] in einem Test, für den diese Crate dort nur
//! Dev-Abhängigkeit ist. Ändert sich die Aufstiegsregel unten, schlägt jener
//! Test fehl. Diese Crate kennt weder `harw-plan` noch `harw-plan-bridge`.
//!
//! [`Ladder::stage_for_or_reject`] bleibt ein dünner `Result`-Wrapper um
//! [`Ladder::stage_for`] für Aufrufer, die eine Ablehnung inhaltsfrei
//! ([`LadderError::NotEscalatable`]) statt als bloßes `None` brauchen.
//!
//! # Determinismus
//! [`Ladder::stage_for`] ist eine reine Funktion der Felder von
//! `Finding<Triaged>`. Zwei strukturell identische Befunde ergeben immer
//! dieselbe Stufe — siehe `tests::test_stage_for_is_deterministic_for_identical_findings`.
//!
//! # Nebenläufigkeit
//! `Ladder` ist ein zustandsloser Marker-Typ; alle Methoden sind reine
//! Funktionen, sicher aus jedem Thread aufrufbar.
//!
//! # Fehler
//! [`LadderError`] — eine feste, nicht interpolierte Meldung.
//!
//! # Examples
//! ```rust
//! use harw_dod_ladder::Ladder;
//! use harw_dod_warden_types::{EscalationStage, WardenAction};
//! use harw_types::CgroupId;
//!
//! let Ok(cgroup) = CgroupId::try_from_str("harw.slice/job-1") else { return };
//! let thaw = WardenAction::ThawCgroup { cgroup };
//! assert!(Ladder::admissible(&thaw, EscalationStage::RuleTriggered));
//! ```

use std::fmt;

use harw_dod_rules::{Finding, FindingKind, Triaged, Verdict};
use harw_dod_signals::{Hardness, Severity};
use harw_dod_warden_types::{EscalationStage, WardenAction};

/// Fehler dieser Crate (Contract-Master §H.1: ein Fehlertyp je Crate).
///
/// # Description
/// Inhaltsfrei: die Meldung ist eine feste Zeichenkette ohne Kennungen,
/// Ziffern oder Befundfelder. Dieselbe Meldung trug bis PL-100 WP 3.0
/// `harw_dod_escalate::EscalateError::NotEscalatable`; `harw-dod-escalate`
/// faltet [`LadderError::NotEscalatable`] per `From` wieder in diese Variante.
///
/// # Errors
/// Erzeugt von [`Ladder::stage_for_or_reject`].
#[derive(Clone, Copy, PartialEq, Eq, harw_macros::HarwError)]
pub enum LadderError {
    /// [`Ladder::stage_for`] hat keine Eskalationsstufe ermittelt — der Befund
    /// ist entweder keine ausgelöste Regel (`FindingKind::RuleTriggered`) oder
    /// nicht bestätigt.
    #[msg("finding is not eligible for any escalation stage")]
    NotEscalatable,
}

/// Formatiert `LadderError` über seine [`std::fmt::Display`]-Meldung.
///
/// # Description
/// `#[derive(harw_macros::HarwError)]` erzeugt kein `Debug`; diese Impl
/// delegiert an `Display`, statt eine zweite Formatierung zu pflegen.
impl fmt::Debug for LadderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Die Eskalationsleiter: zustandsloser Marker-Typ für die beiden Fragen
/// „ist diese Aktion ab dieser Stufe zulässig" und „welche Stufe hat dieser
/// Befund erreicht".
///
/// # Description
/// Siehe Moduldoku für beide Regeln in Worten.
pub struct Ladder;

impl Ladder {
    /// Befragt die in `harw-dod-warden-types` deklarierte Zulässigkeitsmatrix.
    ///
    /// # Description
    /// Reiner Aufruf von [`WardenAction::is_admissible_from`] — siehe
    /// Moduldoku, Abschnitt „Die Zulässigkeitsfrage: aufrufen statt
    /// nachbauen".
    ///
    /// # Arguments
    /// - `action` (`&WardenAction`): die zu prüfende Aktion.
    /// - `stage` (`EscalationStage`): die Stufe, ab der geprüft wird.
    ///
    /// # Returns
    /// `true`, wenn `action` ab `stage` zulässig ist.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_ladder::Ladder;
    /// use harw_dod_warden_types::{EscalationStage, WardenAction};
    /// use harw_types::CgroupId;
    ///
    /// let Ok(cgroup) = CgroupId::try_from_str("cgroup-1") else { return };
    /// let freeze = WardenAction::FreezeCgroup { cgroup: cgroup.clone() };
    /// assert!(Ladder::admissible(&freeze, EscalationStage::RuleTriggered));
    ///
    /// let kill = WardenAction::KillProcessTree { cgroup };
    /// assert!(!Ladder::admissible(&kill, EscalationStage::RuleTriggered));
    /// assert!(Ladder::admissible(&kill, EscalationStage::Escalated));
    /// ```
    #[must_use]
    pub fn admissible(action: &WardenAction, stage: EscalationStage) -> bool {
        action.is_admissible_from(stage)
    }

    /// Ermittelt die Eskalationsstufe eines triagierten Befundes.
    ///
    /// # Description
    /// Siehe Moduldoku, Abschnitt „Die Aufstiegsregel in Worten". Reine,
    /// deterministische Funktion der Felder von `finding` — keine Systemuhr,
    /// kein Zufall.
    ///
    /// # Arguments
    /// - `finding` (`&Finding<Triaged>`): der triagierte Befund.
    ///
    /// # Returns
    /// `None`, wenn der Befund keine Stufe rechtfertigt (keine ausgelöste
    /// Regel oder nicht bestätigt). `Some(EscalationStage::RuleTriggered)`
    /// oder `Some(EscalationStage::Escalated)` sonst.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_dod_ladder::Ladder;
    /// use harw_dod_rules::{Finding, Triaged};
    ///
    /// fn has_stage(finding: &Finding<Triaged>) -> bool {
    ///     Ladder::stage_for(finding).is_some()
    /// }
    /// ```
    #[must_use]
    pub fn stage_for(finding: &Finding<Triaged>) -> Option<EscalationStage> {
        if finding.kind() != FindingKind::RuleTriggered {
            return None;
        }
        if *finding.verdict() != Verdict::Confirmed {
            return None;
        }
        if finding.severity() == Severity::Critical && finding.hardness() == Hardness::Observed {
            Some(EscalationStage::Escalated)
        } else {
            Some(EscalationStage::RuleTriggered)
        }
    }

    /// Wie [`Self::stage_for`], aber als `Result` statt als `Option` — für
    /// Aufrufer, die eine Ablehnung als Fehler statt als bloße Abwesenheit
    /// behandeln wollen (siehe Moduldoku, Abschnitt „Wer die Leiter linkt").
    ///
    /// # Description
    /// Trägt keine eigene Logik: identische Aufstiegsregel, identischer
    /// Determinismus wie [`Self::stage_for`]. Der einzige Unterschied ist
    /// die Fehlerform der Ablehnung.
    ///
    /// # Arguments
    /// - `finding` (`&Finding<Triaged>`): der triagierte Befund.
    ///
    /// # Returns
    /// Die erreichte [`EscalationStage`].
    ///
    /// # Errors
    /// - [`LadderError::NotEscalatable`]: der Befund rechtfertigt keine
    ///   Stufe (siehe [`Self::stage_for`]) — eine feste, nicht interpolierte
    ///   Meldung.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_dod_ladder::{Ladder, LadderError};
    /// use harw_dod_rules::{Finding, Triaged};
    ///
    /// fn rejected(finding: &Finding<Triaged>) -> bool {
    ///     matches!(
    ///         Ladder::stage_for_or_reject(finding),
    ///         Err(LadderError::NotEscalatable)
    ///     )
    /// }
    /// ```
    pub fn stage_for_or_reject(finding: &Finding<Triaged>) -> LadderResult<EscalationStage> {
        Self::stage_for(finding).ok_or(LadderError::NotEscalatable)
    }
}

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_dod_rules::triaged_finding_for_test;
    use harw_types::{CgroupId, FindingId};

    // `Severity::Critical` wird derzeit von keiner echten Regel erzeugt
    // (siehe `harw-dod-rules/src/rules/*`), ist aber die einzige Schwelle,
    // die `Ladder::stage_for` auf `EscalationStage::Escalated` hebt — die
    // Fixtur muss sie deshalb direkt setzen können, statt über eine Regel
    // zu gehen. `triaged_finding_for_test` (Feature `test-support`, siehe
    // `harw-dod-rules/src/finding.rs`-Moduldoku) ist genau dafür da.
    fn triggered_finding(
        severity: Severity,
        hardness: Hardness,
        verdict: Verdict,
    ) -> TestResult<Finding<Triaged>> {
        finding_of_kind(FindingKind::RuleTriggered, severity, hardness, verdict)
    }

    fn finding_of_kind(
        kind: FindingKind,
        severity: Severity,
        hardness: Hardness,
        verdict: Verdict,
    ) -> TestResult<Finding<Triaged>> {
        Ok(triaged_finding_for_test(
            "egress-flow",
            kind,
            severity,
            hardness,
            "egress flow to evil.example.com:443 is outside the allowed network scope",
            jiff::Timestamp::UNIX_EPOCH,
            FindingId::try_from_str("ladder-test-finding").map_err(ctx("non-empty id"))?,
            verdict,
        ))
    }

    // -- Zulässigkeitsmatrix: aufrufen statt nachbauen -----------------------

    #[test]
    fn test_admissible_allows_freeze_from_rule_triggered_and_forbids_kill() -> TestResult {
        let freeze = WardenAction::FreezeCgroup {
            cgroup: CgroupId::try_from_str("cgroup-1").map_err(ctx("gültige CgroupId"))?,
        };
        assert!(Ladder::admissible(&freeze, EscalationStage::RuleTriggered));

        let kill = WardenAction::KillProcessTree {
            cgroup: CgroupId::try_from_str("cgroup-1").map_err(ctx("gültige CgroupId"))?,
        };
        assert!(!Ladder::admissible(&kill, EscalationStage::RuleTriggered));
        assert!(Ladder::admissible(&kill, EscalationStage::Escalated));
        Ok(())
    }

    #[test]
    fn test_admissible_matches_the_types_matrix_for_thaw_and_lift() -> TestResult {
        let cgroup = CgroupId::try_from_str("harw.slice/job-1").map_err(ctx("gültige CgroupId"))?;
        let thaw = WardenAction::ThawCgroup {
            cgroup: cgroup.clone(),
        };
        let lift = WardenAction::LiftIsolation { cgroup };
        for stage in [EscalationStage::RuleTriggered, EscalationStage::Escalated] {
            assert_eq!(
                Ladder::admissible(&thaw, stage),
                thaw.is_admissible_from(stage)
            );
            assert_eq!(
                Ladder::admissible(&lift, stage),
                lift.is_admissible_from(stage)
            );
        }
        assert!(Ladder::admissible(&thaw, EscalationStage::RuleTriggered));
        assert!(!Ladder::admissible(&lift, EscalationStage::RuleTriggered));
        Ok(())
    }

    // -- Aufstiegsregel -------------------------------------------------------

    #[test]
    fn test_stage_for_returns_none_for_unconfirmed_verdict() -> TestResult {
        let finding =
            triggered_finding(Severity::Critical, Hardness::Observed, Verdict::NeedsReview)?;
        assert_eq!(Ladder::stage_for(&finding), None);
        Ok(())
    }

    #[test]
    fn test_stage_for_returns_none_for_anomaly_even_when_confirmed() -> TestResult {
        let finding = finding_of_kind(
            FindingKind::Anomaly,
            Severity::Critical,
            Hardness::Observed,
            Verdict::Confirmed,
        )?;
        assert_eq!(Ladder::stage_for(&finding), None);
        Ok(())
    }

    #[test]
    fn test_stage_for_returns_rule_triggered_when_not_maximally_severe() -> TestResult {
        let finding = triggered_finding(Severity::High, Hardness::Observed, Verdict::Confirmed)?;
        assert_eq!(
            Ladder::stage_for(&finding),
            Some(EscalationStage::RuleTriggered)
        );
        Ok(())
    }

    #[test]
    fn test_stage_for_returns_rule_triggered_when_not_maximally_hard() -> TestResult {
        let finding =
            triggered_finding(Severity::Critical, Hardness::Correlated, Verdict::Confirmed)?;
        assert_eq!(
            Ladder::stage_for(&finding),
            Some(EscalationStage::RuleTriggered)
        );
        Ok(())
    }

    #[test]
    fn test_stage_for_returns_escalated_when_critical_and_observed() -> TestResult {
        let finding =
            triggered_finding(Severity::Critical, Hardness::Observed, Verdict::Confirmed)?;
        assert_eq!(
            Ladder::stage_for(&finding),
            Some(EscalationStage::Escalated)
        );
        Ok(())
    }

    #[test]
    fn test_stage_for_is_deterministic_for_identical_findings() -> TestResult {
        let a = triggered_finding(Severity::Critical, Hardness::Observed, Verdict::Confirmed)?;
        let b = triggered_finding(Severity::Critical, Hardness::Observed, Verdict::Confirmed)?;
        assert_eq!(Ladder::stage_for(&a), Ladder::stage_for(&b));
        Ok(())
    }

    // -- `stage_for_or_reject`: die `Result`-Form von `stage_for` -----------

    #[test]
    fn test_stage_for_or_reject_matches_stage_for_when_escalatable() -> TestResult {
        let finding =
            triggered_finding(Severity::Critical, Hardness::Observed, Verdict::Confirmed)?;
        assert!(matches!(
            Ladder::stage_for_or_reject(&finding),
            Ok(EscalationStage::Escalated)
        ));
        Ok(())
    }

    #[test]
    fn test_stage_for_or_reject_rejects_unescalatable_finding_content_free() -> TestResult {
        let finding =
            triggered_finding(Severity::Critical, Hardness::Observed, Verdict::NeedsReview)?;
        let Err(err) = Ladder::stage_for_or_reject(&finding) else {
            return Err(TestError::Unexpected(
                "stage_for_or_reject muss ablehnen".to_owned(),
            ));
        };
        assert_eq!(err, LadderError::NotEscalatable);
        // Inhaltsfrei: keine Ziffern, keine der bekannten ID-Präfixe; dieselbe
        // Meldung wie das frühere `EscalateError::NotEscalatable`.
        let text = err.to_string();
        assert_eq!(text, "finding is not eligible for any escalation stage");
        assert!(!text.chars().any(|c| c.is_ascii_digit()));
        assert!(!text.contains("finding-"));
        assert_eq!(format!("{err:?}"), text);
        Ok(())
    }

    #[test]
    fn test_ladder_result_alias_carries_ladder_error() {
        fn always_rejects() -> LadderResult<EscalationStage> {
            Err(LadderError::NotEscalatable)
        }

        assert_eq!(always_rejects(), Err(LadderError::NotEscalatable));
    }
}
