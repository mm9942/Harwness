//! Die Eskalationsleiter selbst: [`Ladder`].
//!
//! # Die Zulässigkeitsfrage: aufrufen statt nachbauen
//! [`Ladder::admissible`] beantwortet, ob eine Aktion ab einer Stufe
//! durchgesetzt werden darf, indem sie ausschließlich
//! [`WardenAction::is_admissible_from`] aufruft — die in
//! `harw-dod-warden-proto` (AW5-02) bereits vollständig deklarierte,
//! compilergeprüfte Matrix (`FreezeCgroup`/`ReleaseCgroup` ab
//! `RuleTriggered`, `IsolateNetwork`/`KillProcessTree` erst ab `Escalated`).
//! Diese Crate hält **keine eigene Kopie** dieser Matrix: eine zweite Kopie
//! driftet, sobald `harw-dod-warden-proto` eine Aktion hinzufügt oder eine
//! Stufe umbenennt, ohne dass diese Crate mitgeändert wird. Wäre der
//! Warden-Proto-Crate etwas an dieser Auskunft nicht öffentlich zugänglich
//! gewesen, wäre das ein zu meldender Befund — es ist es nicht:
//! `is_admissible_from` ist bereits `pub`.
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
//! einzigen Aktionen, die `Escalated` überhaupt freischaltet
//! (`IsolateNetwork`, `KillProcessTree`) sind teuer oder irreversibel (siehe
//! `harw-dod-warden-proto/src/action.rs`-Moduldoku, Abschnitt „Die
//! Zulässigkeitsmatrix in Worten") — ein Befund mit mittlerem Gewicht oder
//! nur verknüpftem Nachweis reicht dafür nicht, egal wie oft er auftritt.
//!
//! Es gibt keine dritte Stufe und keinen Zwischenzustand: [`Ladder::stage_for`]
//! liefert entweder `None`, `Some(RuleTriggered)` oder `Some(Escalated)` —
//! eine erschöpfende `match`-Stelle über zwei `bool`-Bedingungen, keine
//! Zählung über die Zeit, keine Häufigkeitsschwelle. Das ist absichtlich eng:
//! eine Leiter, die sich nicht in einem Absatz erklären lässt, ist zu
//! kompliziert für eine Sicherheitsentscheidung (Brief, Abschnitt
//! „`Ladder::admissible`").
//!
//! # Determinismus
//! [`Ladder::stage_for`] ist eine reine Funktion der Felder von
//! `Finding<Triaged>`: keine Systemuhr, kein Zufall, keine Historie über
//! frühere Aufrufe. Zwei strukturell identische Befunde ergeben immer
//! dieselbe Stufe — siehe `tests::test_stage_for_is_deterministic_for_identical_findings`.
//!
//! # Nebenläufigkeit
//! `Ladder` ist ein zustandsloser Marker-Typ; beide Methoden sind reine
//! Funktionen, sicher aus jedem Thread aufrufbar.
//!
//! # Andockung für `harw-plan-bridge` (Knoten AW6-04)
//! [`Ladder::stage_for_or_reject`] ist ein dünner `Result`-Wrapper um
//! [`Ladder::stage_for`] für Aufrufer außerhalb dieser Crate — namentlich
//! `harw-plan-bridge`, das einen Sicherheitsbefund als Nachweis an einen
//! Plan-Knoten andocken will und eine Ablehnung inhaltsfrei
//! (`crate::error::EscalateError::NotEscalatable`) statt als bloßes `None`
//! braucht. Die Kante entsteht bewusst nur in diese Richtung: diese Crate
//! erfährt nichts von `harw-plan` oder `harw-plan-bridge` (kein
//! Abhängigkeitseintrag, kein `use`) — `harw-plan-bridge` zieht die Kante zu
//! dieser Crate, nie umgekehrt.

use harw_dod_rules::{Finding, FindingKind, Triaged, Verdict};
use harw_dod_signals::{Hardness, Severity};
use harw_dod_warden_proto::{EscalationStage, WardenAction};

use crate::error::{EscalateError, EscalateResult};

/// Die Eskalationsleiter: zustandsloser Marker-Typ für die beiden Fragen
/// „ist diese Aktion ab dieser Stufe zulässig" und „welche Stufe hat dieser
/// Befund erreicht".
///
/// # Description
/// Siehe Moduldoku für beide Regeln in Worten.
pub struct Ladder;

impl Ladder {
    /// Befragt die in `harw-dod-warden-proto` deklarierte Zulässigkeitsmatrix.
    ///
    /// # Description
    /// Reiner Aufruf von [`WardenAction::is_admissible_from`] — siehe
    /// Moduldoku, Abschnitt „Die Zulässigkeitsfrage: aufrufen statt
    /// nachbauen", für die Begründung, warum diese Crate die Matrix nicht
    /// selbst hält.
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
    /// use harw_dod_escalate::Ladder;
    /// use harw_dod_warden_proto::{EscalationStage, WardenAction};
    /// use harw_types::CgroupId;
    ///
    /// let freeze = WardenAction::FreezeCgroup {
    ///     cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
    /// };
    /// assert!(Ladder::admissible(&freeze, EscalationStage::RuleTriggered));
    ///
    /// let kill = WardenAction::KillProcessTree {
    ///     cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
    /// };
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
    /// Siehe Moduldoku, Abschnitt „Die Aufstiegsregel in Worten", für die
    /// vollständige Begründung. Reine, deterministische Funktion der Felder
    /// von `finding` — keine Systemuhr, kein Zufall.
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
    /// use harw_dod_escalate::Ladder;
    /// use harw_dod_rules::{Finding, Triaged};
    ///
    /// fn stage(finding: &Finding<Triaged>) {
    ///     let _ = Ladder::stage_for(finding);
    /// }
    /// ```
    #[must_use]
    pub fn stage_for(finding: &Finding<Triaged>) -> Option<EscalationStage> {
        if finding.kind != FindingKind::RuleTriggered {
            return None;
        }
        if *finding.verdict() != Verdict::Confirmed {
            return None;
        }
        if finding.severity == Severity::Critical && finding.hardness == Hardness::Observed {
            Some(EscalationStage::Escalated)
        } else {
            Some(EscalationStage::RuleTriggered)
        }
    }

    /// Wie [`Self::stage_for`], aber als `Result` statt als `Option` — für
    /// Aufrufer, die eine Ablehnung als Fehler statt als bloße Abwesenheit
    /// behandeln wollen (siehe Moduldoku, Abschnitt „Andockung für
    /// `harw-plan-bridge`").
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
    /// - [`EscalateError::NotEscalatable`]: der Befund rechtfertigt keine
    ///   Stufe (siehe [`Self::stage_for`]) — eine feste, nicht interpolierte
    ///   Meldung (siehe `crate::error`-Moduldoku, Abschnitt „Inhaltsfreie
    ///   Fehlermeldungen").
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_dod_escalate::Ladder;
    /// use harw_dod_rules::{Finding, Triaged};
    ///
    /// fn check(finding: &Finding<Triaged>) {
    ///     if let Err(rejection) = Ladder::stage_for_or_reject(finding) {
    ///         eprintln!("Andockung abgelehnt: {rejection}");
    ///     }
    /// }
    /// ```
    pub fn stage_for_or_reject(finding: &Finding<Triaged>) -> EscalateResult<EscalationStage> {
        Self::stage_for(finding).ok_or(EscalateError::NotEscalatable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_dod_rules::rule::{Rule, RuleContext};
    use harw_dod_rules::rules::EgressFlowRule;
    use harw_dod_rules::{run_rules, triage};
    use harw_dod_signals::{EventKind, SecurityEvent};
    use harw_sandbox::NetworkScope;
    use harw_types::{CgroupId, SensorId};

    fn triggered_finding(severity: Severity, hardness: Hardness, verdict: Verdict) -> Finding<Triaged> {
        let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);
        let events = vec![SecurityEvent {
            sensor: SensorId::from_str("net-0"),
            observed_at: jiff::Timestamp::UNIX_EPOCH,
            actor: None,
            kind: EventKind::EgressFlow {
                destination: "evil.example.com".to_owned(),
                port: 443,
            },
        }];
        let ctx = RuleContext {
            now: jiff::Timestamp::UNIX_EPOCH,
            samples: &[],
            events: &events,
            baselines: &[],
            network_scope: &scope,
        };
        let rule: &dyn Rule = &EgressFlowRule;
        let checked = run_rules(&[rule], &ctx);
        let mut finding = checked.into_iter().next().expect("EgressFlowRule löst aus");
        finding.severity = severity;
        finding.hardness = hardness;
        triage(finding, verdict)
    }

    // -- Zulässigkeitsmatrix: aufrufen statt nachbauen -----------------------

    #[test]
    fn test_admissible_allows_freeze_from_rule_triggered_and_forbids_kill() {
        let freeze = WardenAction::FreezeCgroup {
            cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
        };
        assert!(Ladder::admissible(&freeze, EscalationStage::RuleTriggered));

        let kill = WardenAction::KillProcessTree {
            cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
        };
        assert!(!Ladder::admissible(&kill, EscalationStage::RuleTriggered));
        assert!(Ladder::admissible(&kill, EscalationStage::Escalated));
    }

    // -- Aufstiegsregel -------------------------------------------------------

    #[test]
    fn test_stage_for_returns_none_for_unconfirmed_verdict() {
        let finding = triggered_finding(Severity::Critical, Hardness::Observed, Verdict::NeedsReview);
        assert_eq!(Ladder::stage_for(&finding), None);
    }

    #[test]
    fn test_stage_for_returns_rule_triggered_when_not_maximally_severe() {
        let finding = triggered_finding(Severity::High, Hardness::Observed, Verdict::Confirmed);
        assert_eq!(Ladder::stage_for(&finding), Some(EscalationStage::RuleTriggered));
    }

    #[test]
    fn test_stage_for_returns_rule_triggered_when_not_maximally_hard() {
        let finding = triggered_finding(Severity::Critical, Hardness::Correlated, Verdict::Confirmed);
        assert_eq!(Ladder::stage_for(&finding), Some(EscalationStage::RuleTriggered));
    }

    #[test]
    fn test_stage_for_returns_escalated_when_critical_and_observed() {
        let finding = triggered_finding(Severity::Critical, Hardness::Observed, Verdict::Confirmed);
        assert_eq!(Ladder::stage_for(&finding), Some(EscalationStage::Escalated));
    }

    #[test]
    fn test_stage_for_is_deterministic_for_identical_findings() {
        let a = triggered_finding(Severity::Critical, Hardness::Observed, Verdict::Confirmed);
        let b = triggered_finding(Severity::Critical, Hardness::Observed, Verdict::Confirmed);
        assert_eq!(Ladder::stage_for(&a), Ladder::stage_for(&b));
    }

    // -- `stage_for_or_reject`: die Andockung für `harw-plan-bridge` --------

    #[test]
    fn test_stage_for_or_reject_matches_stage_for_when_escalatable() {
        let finding = triggered_finding(Severity::Critical, Hardness::Observed, Verdict::Confirmed);
        assert!(matches!(
            Ladder::stage_for_or_reject(&finding),
            Ok(EscalationStage::Escalated)
        ));
    }

    #[test]
    fn test_stage_for_or_reject_rejects_unescalatable_finding_content_free() {
        let finding = triggered_finding(Severity::Critical, Hardness::Observed, Verdict::NeedsReview);
        let err = Ladder::stage_for_or_reject(&finding).unwrap_err();
        assert!(matches!(err, EscalateError::NotEscalatable));
        // Inhaltsfrei: keine Ziffern, keine der bekannten ID-Präfixe.
        let text = err.to_string();
        assert!(!text.chars().any(|c| c.is_ascii_digit()));
        assert!(!text.contains("finding-"));
    }
}
