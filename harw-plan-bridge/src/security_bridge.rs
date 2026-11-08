//! Plan-Andockung für Sicherheitsbefunde (Knoten AW6-04): wie ein DoD-Befund
//! als Nachweis an einen Plan-Knoten hängt, und wie ein Vertragsverstoß dem
//! Plan *vorgeschlagen*, nie aufgezwungen wird.
//!
//! # Warum ein Befund vorschlägt statt zu invalidieren
//! Ein Sicherheitsbefund entsteht aus Sensordaten (`harw-dod-signals`), und
//! Sensordaten sind angreiferkontrolliert: ein Dateiname, ein
//! Verbindungsziel, ein Prozessname — jedes davon kann von der Gegenseite
//! frei gewählt werden. Gäbe es einen Weg von „ein Sicherheitsbefund
//! meldet einen Vertragsverstoß" direkt zu „ein Plan-Knoten wird
//! invalidiert", wäre das ein Denial-of-Service gegen die eigene Arbeit,
//! ausgelöst von außen, ohne dass irgendeine Regel dieses Projekts verletzt
//! wurde: die Gegenseite müsste nur einen Vertragsverstoß simulieren, um
//! beliebige Plan-Fortschritte zu verwerfen. [`propose_invalidation_for_contract_violation`]
//! erzeugt deshalb **ausschließlich** [`ReconcileStep::AskModel`] — eine der
//! Varianten, die [`crate::controller::PlanController::apply`] nachweislich
//! nie selbst ausführt, sondern unverändert an den Aufrufer zurückgibt
//! (siehe `controller`-Moduldoku, Abschnitt „Wer darf was", sowie den
//! `apply`-Match-Arm, dessen letzter Zweig `AskModel` zusammen mit
//! `AdmitJobs`, `ProposeExpand` und `ProposeCondense` unangewendet in
//! `deferred` sammelt). Diese Funktion kann also
//! **strukturell** keinen [`ReconcileStep::Invalidate`] erzeugen — es gibt in
//! diesem Modul keine Codezeile, die diese Variante überhaupt nennt. Nur ein
//! Modell oder ein Mensch, der den Vorschlag liest und eigenständig
//! [`harw_plan::InvalidationCondition::ContractChanged`] anwendet, kann den
//! Knoten tatsächlich ungültig machen.
//!
//! # Der Zeitversatz: `offset_from_timestamp`, nicht neu gebaut
//! `harw-plan` datiert Nachweise (`EvidenceRef::attached_at`) in
//! `time::OffsetDateTime`, ein Sicherheitsbefund (`Finding<Triaged>::observed_at`,
//! `harw-dod-rules`) in `jiff::Timestamp`. Dieses Crate hat für genau diese
//! Umrechnung bereits eine einzige Stelle:
//! [`crate::finding_store::offset_from_timestamp`]. [`evidence_for_security_finding`]
//! benutzt sie unverändert, statt eine zweite Umrechnung zu schreiben, die
//! bei der nächsten Zeitzonen- oder Bereichsfrage vom Original abweichen
//! könnte. An den Rändern (ein Befund, dessen `observed_at` weit vor dem
//! `time`-darstellbaren Bereich liegt oder weit danach) fällt sie auf
//! `OffsetDateTime::UNIX_EPOCH` zurück statt zu panicken — siehe deren
//! eigene Moduldoku.
//!
//! # Kommt der Digest an? Ja — strukturell, nicht per Konvention
//! `harw_plan::EvidenceRef::digest` ist `Option<ContentDigest>`, und ohne
//! Digest ist ein Nachweis nicht zitierfähig: er zeigt auf nichts, das sich
//! später als unverändert nachweisen lässt (siehe `harw-plan/src/types.rs`,
//! `EvidenceRef`-Moduldoku). [`evidence_for_security_finding`] verlangt
//! deshalb eine `&SecurityEvidence` als Parameter — nicht nur den Befund
//! selbst. `SecurityEvidence::digest` ist dort **kein** `Option`: der
//! einzige öffentliche Konstruktor, [`SecurityEvidence::capture`], berechnet
//! ihn selbst und kann kein `SecurityEvidence` ohne Digest herstellen (siehe
//! `harw-dod-signals/src/evidence.rs`-Moduldoku, Abschnitt „Warum ein
//! Digest"). Wer also ein `&SecurityEvidence` vorweisen kann, trägt den
//! Digest zwangsläufig mit — [`evidence_for_security_finding`] ist deshalb
//! eine totale Funktion, die nie `digest: None` zurückgibt und keinen
//! Fehlerfall für „Digest fehlt" braucht. Ein `Finding<Triaged>` allein
//! trägt diesen Digest **nicht** (`harw-dod-rules::rule::RuleContext` reicht
//! nur rohe `&[HostSample]`/`&[SecurityEvent]`-Schnitte durch, keine
//! eingefrorene `SecurityEvidence`) — deshalb der zusätzliche Parameter,
//! statt `digest: None` einzusetzen und weiterzugehen.
//!
//! # Die abgelehnte Andockung
//! [`dock_security_finding`] dockt einen Befund nur an, wenn er überhaupt
//! eine Eskalationsstufe rechtfertigt: eine ausgelöste Regel
//! (`FindingKind::RuleTriggered`), von einem Menschen bestätigt
//! (`Verdict::Confirmed`) — genau die Bedingung, unter der
//! `harw_dod_ladder::Ladder::stage_for` eine Stufe liefert. Ein Befund, der
//! keine Stufe rechtfertigt, wird nicht als Nachweis angehängt und erzeugt
//! auch keinen Invalidierungs-Vorschlag. Die Ablehnung ist
//! [`crate::error::PlanBridgeError::SecurityDocking`] mit einer festen, nicht
//! interpolierten Meldung — wörtlich dieselbe wie vor PL-100 WP 3.0; diese
//! Crate fügt keinen Befundinhalt hinzu.
//!
//! # Wer die Kante zieht (PL-100 WP 3.0, Leitprinzip I8)
//! `harw-dod-rules` kennt `harw-plan` und `harw-plan-bridge` nicht — kein
//! Abhängigkeitseintrag, kein `use`. Dieses Modul zieht die Kante in die
//! einzig zulässige Richtung: `harw-plan-bridge` hängt von `harw-dod-rules`
//! und `harw-dod-signals` ab, nie umgekehrt. Bis WP 3.0 lief die Kante über
//! `harw-dod-escalate` und zog damit dessen Signier- und Speicherpfad
//! (`harw-dod-warden-proto`, `harw-session-store`-Freezes) in jeden Prozess,
//! der dieses Crate linkt — auch in modellnahe wie `harw-runtime`.
//!
//! Seit WP 3.0 linkt dieses Crate weder `harw-dod-escalate` noch die
//! Eskalationsleiter `harw-dod-ladder`: `xtask/arch-policy.toml` verbietet
//! modellnahen Prozessen beide (`[[forbidden_crates]]`, I8). Die Andockung
//! braucht von der Leiter nur die Frage „gibt es überhaupt eine Stufe" und
//! prüft sie deshalb selbst (`justifies_an_escalation_stage`). Damit diese
//! zweite Formulierung nicht still von der Leiter abweicht, vergleicht
//! `tests::test_docking_condition_matches_the_ladder_on_every_combination`
//! sie mit `Ladder::stage_for` über jede Kombination aus Art, Urteil,
//! Gewicht und Nachweisgrad; `harw-dod-ladder` ist dafür nur
//! Dev-Abhängigkeit und liegt nie in der ausgelieferten Hülle.
//!
//! # Determinismus
//! Beide öffentlichen Funktionen sind reine Funktionen ihrer Argumente:
//! keine Systemuhr, kein Zufall. Derselbe Befund und derselbe Plan ergeben
//! immer denselben Vorschlag.
//!
//! # Concurrency
//! Zustandslos, `Send + Sync`, aus jedem Thread aufrufbar.
//!
//! # Fehler
//! [`crate::error::PlanBridgeError::SecurityDocking`] — siehe oben.

use harw_dod_rules::{Finding, FindingKind, Triaged, Verdict};
use harw_dod_signals::SecurityEvidence;
use harw_plan::admission::ScopeViolation;
use harw_plan::{EvidenceKind, EvidenceRef, TaskId};

use crate::controller::ReconcileStep;
use crate::error::{PlanBridgeError, PlanBridgeResult};
use crate::finding_store::offset_from_timestamp;

/// Baut den Lokator eines Sicherheitsbefund-Nachweises.
///
/// # Description
/// Rein berechenbar aus der Regel-ID und der stabilen Identität des
/// Befundes — kein I/O, keine Abhängigkeit vom `FindingStore`-Layout
/// (dieser Nachweis liegt nicht als Markdown-Datei ab, siehe
/// [`EvidenceKind::Other`]-Wahl in [`evidence_for_security_finding`]).
///
/// # Arguments
/// - `finding` (`&Finding<Triaged>`): der triagierte Sicherheitsbefund.
///
/// # Returns
/// `"security-finding:<rule_id>:<finding-id>"`.
fn security_finding_locator(finding: &Finding<Triaged>) -> String {
    format!(
        "security-finding:{}:{}",
        finding.rule_id(),
        finding.id().as_str()
    )
}

/// Baut einen [`EvidenceRef`] aus einem triagierten Sicherheitsbefund.
///
/// # Description
/// Siehe Moduldoku, Abschnitt „Kommt der Digest an?", für die Begründung,
/// warum diese Funktion `&SecurityEvidence` statt nur `&Finding<Triaged>`
/// verlangt: nur so trägt der zurückgegebene Nachweis strukturell immer
/// einen Digest. `EvidenceKind::Other` ist die bewusste Wahl unter den acht
/// vorhandenen Arten (`harw-plan/src/types.rs`, `EvidenceKind`) — ein
/// Sicherheitsbefund ist weder ein `Finding` im Sinne von
/// `harw-research::ResearchFinding` (das eine eigene, hier nicht passende
/// Markdown-Ablage über [`crate::finding_store::FindingStore`] hat) noch
/// eine der übrigen sechs, enger gefassten Arten.
///
/// # Arguments
/// - `finding` (`&Finding<Triaged>`): der triagierte Sicherheitsbefund.
/// - `security_evidence` (`&SecurityEvidence`): die eingefrorene Beobachtung
///   hinter dem Befund, deren `digest` strukturell nie fehlt.
/// - `actor` (`&str`): wer diesen Nachweis anhängt (z. B. `"dod:escalate"`).
///
/// # Returns
/// Einen [`EvidenceRef`] mit `digest: Some(security_evidence.digest)`.
///
/// # Errors
/// Keine — totale Konstruktion.
///
/// # Examples
/// ```rust,no_run
/// use harw_dod_rules::{Finding, Triaged};
/// use harw_dod_signals::SecurityEvidence;
/// use harw_plan_bridge::security_bridge::evidence_for_security_finding;
///
/// fn build(finding: &Finding<Triaged>, evidence: &SecurityEvidence) {
///     let _reference = evidence_for_security_finding(finding, evidence, "dod:escalate");
/// }
/// ```
#[must_use]
pub fn evidence_for_security_finding(
    finding: &Finding<Triaged>,
    security_evidence: &SecurityEvidence,
    actor: &str,
) -> EvidenceRef {
    EvidenceRef {
        kind: EvidenceKind::Other,
        locator: security_finding_locator(finding),
        attached_at: offset_from_timestamp(finding.observed_at()),
        actor: actor.to_owned(),
        digest: Some(security_evidence.digest),
    }
}

/// Formt aus einem Vertragsverstoß einen Vorschlag an das Modell — **nie**
/// eine Invalidierung.
///
/// # Description
/// Siehe Moduldoku, Abschnitt „Warum ein Befund vorschlägt statt zu
/// invalidieren". Erzeugt ausschließlich [`ReconcileStep::AskModel`]; diese
/// Funktion nennt [`ReconcileStep::Invalidate`] an keiner Stelle. Der
/// erzeugte Prompt trägt genug Kontext (Knoten, Nachweis-Lokator, die
/// `Debug`-Form des Verstoßes), damit ein Modell selbst entscheiden kann, ob
/// [`harw_plan::InvalidationCondition::ContractChanged`] angewendet werden
/// soll — diese Funktion entscheidet es nicht.
///
/// # Arguments
/// - `task` (`&TaskId`): der Plan-Knoten, an dem der Verstoß beobachtet
///   wurde.
/// - `violation` (`&ScopeViolation`): der beobachtete Verstoß gegen den
///   `MutationContract` des Knotens.
/// - `evidence` (`&EvidenceRef`): der zugehörige, bereits gebaute Nachweis
///   (üblicherweise aus [`evidence_for_security_finding`]) — sein Lokator
///   wird im Prompt zitiert, damit ein Mensch ihn nachschlagen kann.
///
/// # Returns
/// [`ReconcileStep::AskModel`] mit einem deterministischen Prompt.
///
/// # Errors
/// Keine — totale Konstruktion.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::TaskId;
/// use harw_plan::admission::ScopeViolation;
/// use harw_plan_bridge::security_bridge::propose_invalidation_for_contract_violation;
///
/// fn propose(task: &TaskId, violation: &ScopeViolation, evidence: &harw_plan::EvidenceRef) {
///     let _step = propose_invalidation_for_contract_violation(task, violation, evidence);
/// }
/// ```
#[must_use]
pub fn propose_invalidation_for_contract_violation(
    task: &TaskId,
    violation: &ScopeViolation,
    evidence: &EvidenceRef,
) -> ReconcileStep {
    ReconcileStep::AskModel {
        prompt: format!(
            "Sicherheitsbefund '{}' zeigt am Plan-Knoten '{task}' einen möglichen \
             Vertragsverstoß: {violation:?}. Dies ist ausschließlich ein Vorschlag — pruefe, \
             ob dieser Knoten mit InvalidationCondition::ContractChanged invalidiert werden \
             soll. Diese Nachricht invalidiert den Knoten nicht selbst.",
            evidence.locator
        ),
    }
}

/// Ob `finding` überhaupt eine Eskalationsstufe rechtfertigt: eine
/// ausgelöste Regel (`FindingKind::RuleTriggered` statt `Anomaly`), von einem
/// Menschen bestätigt (`Verdict::Confirmed` statt `FalsePositive` oder
/// `NeedsReview`).
///
/// # Description
/// Dieselbe Bedingung, unter der `harw_dod_ladder::Ladder::stage_for`
/// `Some` liefert — ohne die Leiter zu linken (siehe Moduldoku, Abschnitt
/// „Wer die Kante zieht"). Gewicht und Nachweisgrad entscheiden dort nur
/// zwischen den beiden Stufen, nicht darüber, ob es eine gibt; diese
/// Funktion fragt sie deshalb nicht ab. Die Gleichheit mit der Leiter pinnt
/// `tests::test_docking_condition_matches_the_ladder_on_every_combination`.
///
/// # Returns
/// `true`, wenn der Befund eine Stufe rechtfertigt.
fn justifies_an_escalation_stage(finding: &Finding<Triaged>) -> bool {
    finding.kind() == FindingKind::RuleTriggered && *finding.verdict() == Verdict::Confirmed
}

/// Dockt einen triagierten Sicherheitsbefund vollständig an den Plan an:
/// Nachweis anhängen, und — falls ein Vertragsverstoß vorliegt — einen
/// Invalidierungs-Vorschlag anfügen.
///
/// # Description
/// Lehnt zuerst ab, wenn der Befund keine Eskalationsstufe rechtfertigt
/// (`justifies_an_escalation_stage`) — siehe Moduldoku, Abschnitt „Die
/// abgelehnte Andockung". Danach reine Verdrahtung:
/// [`evidence_for_security_finding`] gefolgt, falls `violation` gesetzt ist,
/// von [`propose_invalidation_for_contract_violation`]. Die zurückgegebene
/// Schrittfolge enthält an keiner Stelle [`ReconcileStep::Invalidate`].
///
/// # Arguments
/// - `task` (`&TaskId`): der Plan-Knoten, an den angedockt wird.
/// - `finding` (`&Finding<Triaged>`): der triagierte Sicherheitsbefund.
/// - `security_evidence` (`&SecurityEvidence`): die eingefrorene Beobachtung
///   hinter dem Befund.
/// - `violation` (`Option<&ScopeViolation>`): ein beobachteter
///   Vertragsverstoß, falls vorhanden.
/// - `actor` (`&str`): wer diese Andockung durchführt.
///
/// # Returns
/// Eine Schrittfolge mit genau einem [`ReconcileStep::AttachEvidence`] und,
/// falls `violation` gesetzt war, genau einem zusätzlichen
/// [`ReconcileStep::AskModel`].
///
/// # Errors
/// - [`crate::error::PlanBridgeError::SecurityDocking`]: der Befund
///   rechtfertigt keine Eskalationsstufe.
///
/// # Examples
/// Siehe die Tests dieses Moduls für ein vollständiges Beispiel über
/// `PlanController::apply`.
pub fn dock_security_finding(
    task: &TaskId,
    finding: &Finding<Triaged>,
    security_evidence: &SecurityEvidence,
    violation: Option<&ScopeViolation>,
    actor: &str,
) -> PlanBridgeResult<Vec<ReconcileStep>> {
    // Ohne Leiter (PL-100 WP 3.0, I8): dieselbe Bedingung wie
    // `Ladder::stage_for(..).is_some()`, siehe Moduldoku.
    if !justifies_an_escalation_stage(finding) {
        return Err(PlanBridgeError::SecurityDocking);
    }

    let evidence = evidence_for_security_finding(finding, security_evidence, actor);
    let mut steps = vec![ReconcileStep::AttachEvidence {
        task: task.clone(),
        evidence: evidence.clone(),
    }];
    if let Some(violation) = violation {
        steps.push(propose_invalidation_for_contract_violation(
            task, violation, &evidence,
        ));
    }
    Ok(steps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::NetworkScope;
    use harw_dod_rules::rule::{Rule, RuleContext};
    use harw_dod_rules::rules::EgressFlowRule;
    use harw_dod_rules::{run_rules, triaged_finding_for_test};
    use harw_dod_signals::{EventKind, Hardness, SecurityEvent, Severity};
    use harw_plan::admission::PathRule;
    use harw_plan::{PlanNodeStatus, PlanStore};
    use harw_types::{FindingId, SensorId};

    fn triaged_finding(
        severity: Severity,
        hardness: Hardness,
        verdict: Verdict,
    ) -> TestResult<Finding<Triaged>> {
        triaged_finding_at(severity, hardness, verdict, jiff::Timestamp::UNIX_EPOCH)
    }

    /// Wie [`triaged_finding`], aber mit injizierbarem `observed_at`
    /// (die Zeit des auslösenden `SecurityEvent` — `EgressFlowRule::evaluate`
    /// datiert den Befund seit PL-100 WP0.2 exakt darauf, nicht auf
    /// `RuleContext::now`). Gebraucht von den Rand-Tests der
    /// `offset_from_timestamp`-Umrechnung: `Finding`s Felder sind read-only
    /// (F-023, `harw-dod-rules::finding`-Moduldoku), es gibt also keinen
    /// nachträglichen Setter — nur der Weg über das ausgewertete Ereignis
    /// erreicht einen beliebigen `observed_at`-Wert. `RuleContext::now` wird
    /// zur Konsistenz auf denselben Wert gesetzt.
    ///
    /// `severity`/`hardness` bleiben Parameter (statt sie aus dem
    /// `EgressFlowRule`-Ergebnis zu übernehmen), damit jeder Aufrufer explizit
    /// dokumentiert, welche feste Kombination die Regel liefert — der
    /// `assert_eq!` unten macht das strukturell sichtbar, falls sich die
    /// Regel je ändert.
    fn triaged_finding_at(
        severity: Severity,
        hardness: Hardness,
        verdict: Verdict,
        observed_at: jiff::Timestamp,
    ) -> TestResult<Finding<Triaged>> {
        let scope = NetworkScope::from_hosts(["docs.rs".to_owned()]);
        let events = vec![SecurityEvent {
            sensor: SensorId::from_str("net-0"),
            observed_at,
            actor: None,
            kind: EventKind::EgressFlow {
                destination: "evil.example.com".to_owned(),
                port: 443,
            },
        }];
        let ctx = RuleContext {
            now: observed_at,
            samples: &[],
            events: &events,
            baselines: &[],
            network_scope: &scope,
            listener_history: None,
            rule_config: None,
        };
        // `run_rules` wertet seit der Engine-Überarbeitung intern immer
        // `ALL_RULES` aus (kein `&[&dyn Rule]`-Parameter mehr) und liefert
        // `Vec<Finding<Raw>>` — noch nicht geprüft. Bei leeren
        // `samples`/`baselines` lösen `BaselineDeviationRule` und
        // `StructureDriftRule` hier nichts aus; trotzdem wird explizit nach
        // der Regel-ID gefiltert statt blind das erste Element zu nehmen.
        let raw_findings = run_rules(&ctx);
        let finding = raw_findings
            .into_iter()
            .find(|f| f.rule_id() == EgressFlowRule.id())
            .ok_or(TestError::Missing("EgressFlowRule löst aus"))?;
        assert_eq!(
            finding.severity(),
            severity,
            "EgressFlowRule liefert eine feste Severity"
        );
        assert_eq!(
            finding.hardness(),
            hardness,
            "EgressFlowRule liefert eine feste Hardness"
        );
        // `Finding::check` (Raw -> RuleChecked) ist in `harw-dod-rules`
        // `pub(crate)` — von hier, einer fremden Crate, nicht aufrufbar
        // (siehe dessen `finding.rs`-Moduldoku samt `compile_fail`-Doctest
        // an `Finding::check`). `triaged_finding_for_test` ist der einzige
        // öffentliche Übergang, den Tests abhängiger Crates dafür nutzen
        // dürfen; er geht intern denselben Pfad (`Finding::raw(..).check(id)`
        // gefolgt von `triage`). Gefüttert wird er ausschließlich mit den
        // Feldern des soeben real von `EgressFlowRule` berechneten Befundes
        // — kein erfundener Kurzschluss von Raw direkt nach Triaged.
        Ok(triaged_finding_for_test(
            finding.rule_id(),
            finding.kind(),
            finding.severity(),
            finding.hardness(),
            finding.summary().to_owned(),
            finding.observed_at(),
            FindingId::new(),
            verdict,
        ))
    }

    fn sample_security_evidence() -> TestResult<SecurityEvidence> {
        SecurityEvidence::capture(vec![], vec![], jiff::Timestamp::UNIX_EPOCH)
            .map_err(ctx("leere Evidenz kodiert immer"))
    }

    fn sample_violation() -> ScopeViolation {
        ScopeViolation::Forbidden {
            path: "src/secret.rs".to_owned(),
            rule: PathRule::Exact("src/secret.rs".to_owned()),
        }
    }

    // -- evidence_for_security_finding: Digest kommt strukturell an --------

    #[test]
    fn test_evidence_for_security_finding_carries_the_evidence_digest() -> TestResult {
        let finding = triaged_finding(Severity::High, Hardness::Observed, Verdict::Confirmed)?;
        let evidence = sample_security_evidence()?;
        let reference = evidence_for_security_finding(&finding, &evidence, "dod:escalate");
        assert_eq!(reference.digest, Some(evidence.digest));
        assert_eq!(reference.kind, EvidenceKind::Other);
        assert_eq!(reference.actor, "dod:escalate");
        Ok(())
    }

    // -- offset_from_timestamp wird benutzt, auch an den Rändern -----------

    #[test]
    fn test_evidence_for_security_finding_uses_offset_from_timestamp_at_unix_epoch() -> TestResult {
        let finding = triaged_finding(Severity::High, Hardness::Observed, Verdict::Confirmed)?;
        let evidence = sample_security_evidence()?;
        let reference = evidence_for_security_finding(&finding, &evidence, "dod:escalate");
        assert_eq!(
            reference.attached_at,
            offset_from_timestamp(finding.observed_at())
        );
        Ok(())
    }

    #[test]
    fn test_evidence_for_security_finding_matches_the_independent_oracle_far_in_the_future()
    -> TestResult {
        // Unabhaengig von `offset_from_timestamp` selbst nachgerechnet, damit
        // dieser Test nicht nur die eigene Umrechnung gegen sich selbst
        // prueft: `time::OffsetDateTime::from_unix_timestamp_nanos` ist der
        // zugrunde liegende Baustein, den `offset_from_timestamp` benutzt.
        let far_future = jiff::Timestamp::MAX;
        let finding = triaged_finding_at(
            Severity::High,
            Hardness::Observed,
            Verdict::Confirmed,
            far_future,
        )?;
        let evidence = sample_security_evidence()?;
        let reference = evidence_for_security_finding(&finding, &evidence, "dod:escalate");
        let expected = time::OffsetDateTime::from_unix_timestamp_nanos(far_future.as_nanosecond())
            .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
        assert_eq!(reference.attached_at, expected);
        Ok(())
    }

    #[test]
    fn test_evidence_for_security_finding_matches_the_independent_oracle_before_plan_begin()
    -> TestResult {
        // Ein Befund, der lange vor dem Planbeginn beobachtet wurde (hier:
        // `Timestamp::MIN`) — derselbe unabhaengige Abgleich wie oben, nur am
        // anderen Rand.
        let before_begin = jiff::Timestamp::MIN;
        let finding = triaged_finding_at(
            Severity::High,
            Hardness::Observed,
            Verdict::Confirmed,
            before_begin,
        )?;
        let evidence = sample_security_evidence()?;
        let reference = evidence_for_security_finding(&finding, &evidence, "dod:escalate");
        let expected =
            time::OffsetDateTime::from_unix_timestamp_nanos(before_begin.as_nanosecond())
                .unwrap_or(time::OffsetDateTime::UNIX_EPOCH);
        assert_eq!(reference.attached_at, expected);
        Ok(())
    }

    // -- ContractViolation erzeugt einen Vorschlag, nie eine Invalidierung -

    #[test]
    fn test_propose_invalidation_for_contract_violation_is_never_invalidate() {
        let task = TaskId::new("t-1");
        let violation = sample_violation();
        let evidence = EvidenceRef {
            kind: EvidenceKind::Other,
            locator: "security-finding:egress-flow:finding-1".to_owned(),
            attached_at: offset_from_timestamp(jiff::Timestamp::UNIX_EPOCH),
            actor: "dod:escalate".to_owned(),
            digest: None,
        };
        let step = propose_invalidation_for_contract_violation(&task, &violation, &evidence);
        assert!(matches!(step, ReconcileStep::AskModel { .. }));
        assert!(!matches!(step, ReconcileStep::Invalidate { .. }));
    }

    #[test]
    fn test_dock_security_finding_never_reaches_apply_as_an_invalidation() -> TestResult {
        let task = TaskId::new("t-1");
        let finding = triaged_finding(Severity::High, Hardness::Observed, Verdict::Confirmed)?;
        let evidence = sample_security_evidence()?;
        let violation = sample_violation();

        let steps =
            dock_security_finding(&task, &finding, &evidence, Some(&violation), "dod:escalate")
                .map_err(ctx("konfirmierter, eskalationswuerdiger Befund dockt an"))?;

        assert_eq!(steps.len(), 2);
        assert!(matches!(steps[0], ReconcileStep::AttachEvidence { .. }));
        assert!(matches!(steps[1], ReconcileStep::AskModel { .. }));
        assert!(
            steps
                .iter()
                .all(|step| !matches!(step, ReconcileStep::Invalidate { .. }))
        );

        let store = crate::testing::seeded_plan_store(vec![crate::testing::coding_node(
            "t-1",
            PlanNodeStatus::Ready,
        )])?;
        let (events, deferred) =
            crate::controller::PlanController::apply(&steps, &store, None, "dod:escalate")
                .map_err(ctx("apply gelingt"))?;
        // Der Vorschlag wurde zurueckgegeben, nicht ausgefuehrt.
        assert_eq!(deferred, vec![steps[1].clone()]);
        // Kein `PlanEvent` beschreibt eine Invalidierung.
        assert!(
            !events
                .iter()
                .any(|event| format!("{event:?}").contains("Invalidat"))
        );
        // Der Knoten bleibt `Ready` — kein direkter Weg zur Invalidierung.
        let plan = store.current().map_err(ctx("Plan lesbar"))?;
        let node = plan
            .nodes
            .iter()
            .find(|node| node.id == task)
            .ok_or(TestError::Missing("Knoten existiert weiterhin"))?;
        assert_eq!(node.status, PlanNodeStatus::Ready);
        Ok(())
    }

    // -- Determinismus: gleicher Befund, gleicher Plan, gleicher Vorschlag -

    #[test]
    fn test_propose_invalidation_for_contract_violation_is_deterministic() {
        let task = TaskId::new("t-1");
        let violation = sample_violation();
        let evidence = EvidenceRef {
            kind: EvidenceKind::Other,
            locator: "security-finding:egress-flow:finding-1".to_owned(),
            attached_at: offset_from_timestamp(jiff::Timestamp::UNIX_EPOCH),
            actor: "dod:escalate".to_owned(),
            digest: None,
        };
        let first = propose_invalidation_for_contract_violation(&task, &violation, &evidence);
        let second = propose_invalidation_for_contract_violation(&task, &violation, &evidence);
        assert_eq!(first, second);
    }

    // -- abgelehnte Andockung: inhaltsfrei -----------------------------------

    #[test]
    fn test_dock_security_finding_rejects_unescalatable_finding_content_free() -> TestResult {
        let task = TaskId::new("t-1");
        let finding = triaged_finding(Severity::High, Hardness::Observed, Verdict::NeedsReview)?;
        let evidence = sample_security_evidence()?;

        let Err(error) = dock_security_finding(&task, &finding, &evidence, None, "dod:escalate")
        else {
            return Err(TestError::Unexpected(
                "ein nicht bestaetigter Befund dockt nicht an".to_string(),
            ));
        };
        assert!(matches!(error, PlanBridgeError::SecurityDocking));
        let text = error.to_string();
        // Die Meldung ist durch PL-100 WP 3.0 (Andockung ohne
        // `harw-dod-escalate` und ohne `harw-dod-ladder`) unverändert.
        assert_eq!(
            text,
            "Sicherheitsbefund qualifiziert sich nicht für eine Eskalationsstufe: \
             finding is not eligible for any escalation stage"
        );
        assert!(!text.chars().any(|c| c.is_ascii_digit()));
        assert!(!text.contains("t-1"));
        Ok(())
    }

    // -- Andockbedingung = Stufenregel der Leiter (I8) ----------------------

    // Exhaustive Indizes der vier Eingabe-Enums des Paritätstests. Eine neue
    // Variante bricht hier den Build (kein Platzhalter-Arm, keines der Enums
    // ist `#[non_exhaustive]`); fehlt sie danach in einer der Wertelisten des
    // Tests, schlägt dessen Vollständigkeitsprüfung fehl.
    const KIND_COUNT: usize = 2;
    const VERDICT_COUNT: usize = 3;
    const SEVERITY_COUNT: usize = 5;
    const HARDNESS_COUNT: usize = 3;

    // Index einer `FindingKind`-Variante, `0..KIND_COUNT`.
    fn kind_index(kind: FindingKind) -> usize {
        match kind {
            FindingKind::RuleTriggered => 0,
            FindingKind::Anomaly => 1,
        }
    }

    // Index einer `Verdict`-Variante, `0..VERDICT_COUNT`.
    fn verdict_index(verdict: Verdict) -> usize {
        match verdict {
            Verdict::Confirmed => 0,
            Verdict::FalsePositive => 1,
            Verdict::NeedsReview => 2,
        }
    }

    // Index einer `Severity`-Variante, `0..SEVERITY_COUNT`.
    fn severity_index(severity: Severity) -> usize {
        match severity {
            Severity::Info => 0,
            Severity::Low => 1,
            Severity::Medium => 2,
            Severity::High => 3,
            Severity::Critical => 4,
        }
    }

    // Index einer `Hardness`-Variante, `0..HARDNESS_COUNT`.
    fn hardness_index(hardness: Hardness) -> usize {
        match hardness {
            Hardness::Observed => 0,
            Hardness::Correlated => 1,
            Hardness::Inferred => 2,
        }
    }

    // Alle Indizes `0..count` als Menge, zum Vergleich mit den gesehenen.
    fn all_indices(count: usize) -> std::collections::BTreeSet<usize> {
        (0..count).collect()
    }

    /// PL-100 WP 3.0 (I8): `justifies_an_escalation_stage` ist genau die
    /// Bedingung, unter der `Ladder::stage_for` eine Stufe liefert — für jede
    /// Kombination aus Art, Urteil, Gewicht und Nachweisgrad. Die Leiter ist
    /// hier nur Dev-Abhängigkeit; ändert sich ihre Regel, schlägt dieser Test
    /// fehl, statt dass Andockung und Leiter still auseinanderlaufen. Dass die
    /// Wertelisten wirklich jede Variante enthalten, erzwingen die
    /// exhaustiven `*_index`-Funktionen oben: der Test vergleicht die
    /// gesehenen Indizes mit `0..*_COUNT`.
    #[test]
    fn test_docking_condition_matches_the_ladder_on_every_combination() -> TestResult {
        let id = FindingId::try_from_str("plan-bridge-ladder-parity").map_err(ctx("finding id"))?;
        let mut checked = 0usize;
        let mut docked = 0usize;
        let mut kinds = std::collections::BTreeSet::new();
        let mut verdicts = std::collections::BTreeSet::new();
        let mut severities = std::collections::BTreeSet::new();
        let mut hardnesses = std::collections::BTreeSet::new();
        for kind in [FindingKind::RuleTriggered, FindingKind::Anomaly] {
            for verdict in [
                Verdict::Confirmed,
                Verdict::FalsePositive,
                Verdict::NeedsReview,
            ] {
                for severity in [
                    Severity::Info,
                    Severity::Low,
                    Severity::Medium,
                    Severity::High,
                    Severity::Critical,
                ] {
                    for hardness in [Hardness::Observed, Hardness::Correlated, Hardness::Inferred] {
                        let finding = triaged_finding_for_test(
                            "egress-flow",
                            kind,
                            severity,
                            hardness,
                            "ladder parity fixture",
                            jiff::Timestamp::UNIX_EPOCH,
                            id.clone(),
                            verdict,
                        );
                        kinds.insert(kind_index(kind));
                        verdicts.insert(verdict_index(verdict));
                        severities.insert(severity_index(severity));
                        hardnesses.insert(hardness_index(hardness));
                        let docks = justifies_an_escalation_stage(&finding);
                        assert_eq!(
                            docks,
                            harw_dod_ladder::Ladder::stage_for(&finding).is_some(),
                            "{kind:?} / {verdict:?} / {severity:?} / {hardness:?}"
                        );
                        if docks {
                            docked = docked.saturating_add(1);
                        }
                        checked = checked.saturating_add(1);
                    }
                }
            }
        }
        assert_eq!(kinds, all_indices(KIND_COUNT), "jede FindingKind-Variante");
        assert_eq!(
            verdicts,
            all_indices(VERDICT_COUNT),
            "jede Verdict-Variante"
        );
        assert_eq!(
            severities,
            all_indices(SEVERITY_COUNT),
            "jede Severity-Variante"
        );
        assert_eq!(
            hardnesses,
            all_indices(HARDNESS_COUNT),
            "jede Hardness-Variante"
        );
        // 2 Arten x 3 Urteile x 5 Gewichte x 3 Nachweisgrade = 90; angedockt
        // wird nur RuleTriggered + Confirmed (5 x 3 = 15).
        let combinations = KIND_COUNT
            .saturating_mul(VERDICT_COUNT)
            .saturating_mul(SEVERITY_COUNT)
            .saturating_mul(HARDNESS_COUNT);
        assert_eq!(checked, combinations);
        assert_eq!(docked, SEVERITY_COUNT.saturating_mul(HARDNESS_COUNT));
        Ok(())
    }

    // -- Abhängigkeiten: keine Eskalations- oder Signierkante (I8) -----------

    /// Ob eine Nicht-Kommentarzeile von `manifest` den Paketnamen
    /// `forbidden` nennt (auch in Unterstrich-Schreibweise). Bewusst grob:
    /// jede Schreibweise einer Abhängigkeit (Schlüssel, Tabellenform
    /// `[dependencies.<name>]`, gequoteter Schlüssel, `[target.'…']`-Tabelle,
    /// `package = …` mit beliebigen Leerzeichen und Anführungszeichen) trägt
    /// den Namen wörtlich in einer solchen Zeile. Ein Fehlalarm (etwa ein
    /// Name in einem Zeilenend-Kommentar) ist billiger als ein Durchschlupf.
    fn manifest_mentions(manifest: &str, forbidden: &str) -> bool {
        let underscored = forbidden.replace('-', "_");
        manifest
            .lines()
            .map(str::trim_start)
            .filter(|line| !line.starts_with('#'))
            .any(|line| line.contains(forbidden) || line.contains(underscored.as_str()))
    }

    /// Der Detektor `manifest_mentions` erkennt jede Schreibweise einer
    /// Abhängigkeit und überspringt reine Kommentarzeilen.
    #[test]
    fn test_manifest_mentions_detects_every_dependency_spelling() {
        let name = "harw-dod-escalate";
        for spelling in [
            "harw-dod-escalate = { path = \"../x\" }",
            "[dependencies.harw-dod-escalate]",
            "\"harw-dod-escalate\" = { path = \"../x\" }",
            "[target.'cfg(unix)'.dependencies.harw-dod-escalate]",
            "[target.'cfg(unix)'.build-dependencies]\nladder = { package = \"harw-dod-escalate\" }",
            "ladder = { package='harw-dod-escalate', path = \"../x\" }",
            "ladder = {package=\"harw-dod-escalate\"}",
            "    harw_dod_escalate = { path = \"../x\" }",
        ] {
            let manifest = format!("[package]\nname = \"x\"\n\n[dependencies]\n{spelling}\n");
            assert!(manifest_mentions(&manifest, name), "{spelling}");
        }
        assert!(!manifest_mentions(
            "# nie harw-dod-escalate\n  # auch nicht harw_dod_escalate\n",
            name
        ));
    }

    /// PL-100 WP 3.0 (Leitprinzip I8): dieses Crate nennt weder
    /// `harw-dod-escalate` noch den Signierteil `harw-dod-warden-proto` noch
    /// den künftigen `harw-escalator` in einer Zeile seines Manifests außer
    /// Kommentaren — weder als Schlüssel noch umbenannt (`package = …`), in
    /// Tabellenform, unter `[target.'…']` oder als Dev-Abhängigkeit. Die
    /// transitive Hülle prüft das xtask-Gate `arch` (`[[forbidden_crates]]`).
    #[test]
    fn test_manifest_names_no_escalation_or_signing_crate() {
        let manifest = include_str!("../Cargo.toml");
        for forbidden in [
            "harw-dod-escalate",
            "harw-dod-warden-proto",
            "harw-escalator",
        ] {
            assert!(
                !manifest_mentions(manifest, forbidden),
                "Cargo.toml nennt {forbidden}"
            );
        }
    }

    /// PL-100 WP 3.0 (Leitprinzip I8): `harw-dod-ladder` steht im Manifest
    /// genau einmal, und zwar unter `[dev-dependencies]` — für
    /// `test_docking_condition_matches_the_ladder_on_every_combination`, nie
    /// in einer ausgelieferten Tabelle (`[dependencies]`,
    /// `[build-dependencies]`, `[target.'…']`, Tabellenform). Die transitive
    /// Hülle prüft das xtask-Gate `arch` (`[[forbidden_crates]]`).
    #[test]
    fn test_manifest_names_the_ladder_only_as_a_dev_dependency() {
        let manifest = include_str!("../Cargo.toml");
        let mut section = "";
        let mut mentions = 0usize;
        for line in manifest.lines().map(str::trim) {
            if line.starts_with('[') {
                section = line;
            }
            if !manifest_mentions(line, "harw-dod-ladder") {
                continue;
            }
            assert_eq!(
                section, "[dev-dependencies]",
                "harw-dod-ladder außerhalb von [dev-dependencies]: {line}"
            );
            mentions = mentions.saturating_add(1);
        }
        assert_eq!(mentions, 1, "harw-dod-ladder genau einmal im Manifest");
    }
}
