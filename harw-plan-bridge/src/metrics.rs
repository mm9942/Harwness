//! Telemetrie der Planschleife: acht Fragen zu einem bisher unbeobachteten
//! Regelkreis (Knoten AW1-05).
//!
//! # Verantwortungsbereich
//! [`crate::controller::PlanController`] und [`crate::job_bridge::PlanJobBridge`]
//! bilden den Regelkreis dieses Systems: hier entscheidet sich, wie ein Plan
//! sich weiterentwickelt. Bislang lief dieser Regelkreis vollständig
//! unbeobachtet. Dieses Modul deklariert die Metrikschlüssel über
//! [`harw_macros::metrics!`] und stellt `record_*`-Funktionen bereit, die von
//! den `_observed`-Varianten der Controller-Methoden aufgerufen werden:
//! [`PlanController::reconcile_observed`](crate::controller::PlanController::reconcile_observed),
//! [`PlanController::apply_observed`](crate::controller::PlanController::apply_observed),
//! [`PlanJobBridge::on_job_completed_observed`](crate::job_bridge::PlanJobBridge::on_job_completed_observed),
//! [`PlanJobBridge::on_job_failed_observed`](crate::job_bridge::PlanJobBridge::on_job_failed_observed).
//!
//! # Welche Frage jede Metrik beantwortet
//! - [`RECONCILE_STEPS_TOTAL`]: Welche der neun Schrittarten laufen
//!   wirklich? Eine Schrittart, die über eine ganze Sitzung bei null steht,
//!   ist ein Hinweis, dass ein Zweig des Controllers nie erreicht wird.
//! - [`INVALIDATIONS_TOTAL`]: Wie oft wird ein Plan-Knoten ungültig?
//! - [`EXPLORE_INSERTED_TOTAL`]: Wie oft schiebt die Schleife eine
//!   Exploration vor einen Knoten?
//! - [`EVIDENCE_ATTACHED_TOTAL`]: Wie oft wird ein Nachweis angehängt?
//! - [`PROPOSALS_PENDING`]: Wie viele Vorschläge aus der letzten
//!   Reconcile-Runde warten auf eine externe Entscheidung (Modell oder
//!   Mensch)?
//! - [`GOAL_AGE_DAYS`]: Wie alt ist das gebundene Ziel? Ein Ziel, das nicht
//!   altert, wird nicht bearbeitet.
//! - [`GOAL_COVERAGE`]: Welcher Anteil der Akzeptanzkriterien des gebundenen
//!   Ziels ist durch Evidenz belegt?
//!
//! # Was hier bewusst fehlt: `scope_violation_rate`
//! `harw_plan::admission::validate_patch` ist die Funktion, die einen
//! Schreibversuch außerhalb des Reviers ablehnt — genau das, was diese Metrik
//! zählen sollte. Sie hat aber, Stand dieses Knotens, **keinen einzigen
//! Aufrufer** außerhalb ihres eigenen Testmoduls (`harw-plan/src/admission.rs`)
//! — weder in dieser Crate noch anderswo im Workspace. `harw-plan-bridge`
//! baut zwar über [`crate::job_bridge`] den [`harw_plan::admission::MutationContract`]
//! und legt ihn in den Job-Payload (siehe `job_bridge::node_payload`), ruft
//! `validate_patch` selbst aber nirgends auf — die Prüfung eines
//! eingereichten Patches gegen diesen Vertrag ist (noch) nicht implementiert.
//! Eine Metrik an eine Funktion zu hängen, die nie aufgerufen wird, würde
//! dauerhaft `0` anzeigen und das fälschlich als "keine Scope-Verletzungen"
//! lesen lassen — das wäre eine Metrik, die lügt, schlimmer als keine. Diese
//! Crate deklariert `scope_violation_rate` deshalb **nicht**. Sobald ein
//! Aufrufer von `validate_patch` innerhalb dieser Crate entsteht, gehört die
//! Metrik dorthin.
//!
//! ## Nachtrag: der Kontrakt hat einen Konsumenten — aber keinen, der prüft
//! Eine nachträgliche Suche über den gesamten Workspace zeigt: der
//! [`harw_plan::admission::MutationContract`] selbst hat sehr wohl einen
//! Konsumenten außerhalb von `harw-plan` und `harw-plan-bridge` —
//! `harw-cli/src/job_worker.rs`, Funktion `derive_plan_node_sandbox` (mit
//! Helfer `contract_permission_ceiling`). Sie liest den Kontrakt, der über
//! `job_bridge::node_payload` in den Job-Payload gewandert ist, und leitet
//! daraus die `SandboxSpec` ab, unter der der Worker-Turn läuft. Das ist ein
//! echter Produktionspfad: `execute_plan_node_claim` → `PlanNodePayload::parse`
//! → `derive_plan_node_sandbox` → Turn-Ausführung.
//!
//! Trotzdem bleibt `validate_patch` unerreicht, denn `derive_plan_node_sandbox`
//! prüft etwas anderes: Es bildet `contract.allowed_paths` auf eine einzige
//! binäre Berechtigung ab (`Permission::WriteWorkspace` an/aus, je nachdem, ob
//! überhaupt erlaubte Pfade existieren — siehe `contract_permission_ceiling`)
//! und schränkt damit den gesamten Workspace-Zugriff des Turns ein. Es gibt an
//! dieser Stelle keine Liste einzeln geänderter Dateien (kein `UnifiedDiff`,
//! kein `PatchFile`) — der Agent schreibt frei innerhalb des restringierten
//! Sandbox-Verzeichnisses, und niemand vergleicht die tatsächlich berührten
//! Pfade Datei für Datei gegen `allowed_paths`/`forbidden_paths`. Das ist genau
//! die Granularität, die `validate_patch` bietet und die hier fehlt.
//!
//! **Wo die Prüfung stattdessen stehen müsste:** `harw-cli/src/job_worker.rs`,
//! entweder (a) in `derive_plan_node_sandbox` selbst, wenn dort künftig ein
//! dateigenaues Sandbox-Modell entsteht, oder (b) als neuer Schritt zwischen
//! Turn-Ende und `report_plan_node_outcome`/`PlanJobBridge::on_job_completed`,
//! der die tatsächlich geänderten Dateien des Sandbox-Verzeichnisses (z. B.
//! über einen Filesystem- oder Git-Diff) in ein `UnifiedDiff` fasst und gegen
//! den mitgeführten `MutationContract` per `validate_patch` prüft, bevor der
//! Job als erfolgreich an den Plan zurückgemeldet wird. Beide Stellen liegen
//! in `harw-cli`, außerhalb des Schreibbereichs dieses Auftrags — deshalb
//! bleibt `scope_violation_rate` hier weiterhin unideklariert, und diese Crate
//! erhält keine neue Aufrufstelle von `validate_patch`: eine Aufrufstelle
//! innerhalb von `harw-plan-bridge` einzubauen wäre eine Prüfung an einer
//! Stelle, die kein echter Patch durchläuft — der Job-Payload, den diese Crate
//! baut, trägt nie eine Liste geänderter Dateien, nur den Kontrakt selbst.
//!
//! # Kardinalität von `reconcile_steps_total`
//! [`crate::controller::ReconcileStep`] hat genau neun Varianten
//! (`AttachEvidence`, `Invalidate`, `InsertExplore`, `ProposeExpand`,
//! `ProposeCondense`, `MarkReady`, `AdmitJobs`, `GoalStatus`, `AskModel`);
//! das Label `step` kann also nie mehr als neun verschiedene Werte annehmen.
//! Deshalb `cardinality = bounded(9)` statt unbegrenzt: ein Label mit
//! unbegrenzter Wertemenge lässt die Zeitreihendatenbank wachsen, bis sie
//! kippt, und der häufigste Weg dorthin ist ein Label, das versehentlich eine
//! ID trägt statt einer geschlossenen Aufzählung. [`step_label`] ist ein
//! erschöpfendes `match` ohne Wildcard: eine zehnte `ReconcileStep`-Variante
//! bricht diese Funktion beim Kompilieren, statt die Kardinalitätsgrenze
//! still zu verletzen.
//!
//! # Keine Inhalte in Labels
//! Jedes in diesem Modul vergebene Label ist eine Schrittart aus der oben
//! genannten Neun-Werte-Menge (`step`) oder gar kein Label — nie ein
//! Knotentitel, ein Dateipfad oder Modelltext. Das ist die Regel, die
//! Telemetrie von einem Datenleck trennt.
//!
//! # Wie der Sink hereinkommt
//! [`PlanController`](crate::controller::PlanController) ist ein
//! zustandsloser Namensraum ohne Konstruktor — alle Methoden sind
//! assoziierte Funktionen, es gibt also keine Instanz, an der ein
//! `TelemetrySink` dauerhaft hängen könnte, ohne aus dem Namensraum eine
//! zustandsbehaftete Struktur zu machen und damit jeden bestehenden Aufrufer
//! von [`PlanController::reconcile`](crate::controller::PlanController::reconcile)
//! und [`PlanController::apply`](crate::controller::PlanController::apply) zu
//! brechen. Die neuen `_observed`-Methoden nehmen den Sink deshalb als
//! `Option<&dyn TelemetrySink>`-Parameter entgegen, statt ihn global zu
//! halten: eine globale Variable machte jede messende Funktion unprüfbar,
//! weil ein Test dann nicht mehr sagen könnte, was tatsächlich emittiert
//! wurde. `None` unterdrückt jede Emission, ohne dass der Aufrufer einen
//! `NullSink` konstruieren müsste.
//!
//! # Concurrency
//! Alle Funktionen dieses Moduls sind rein und zustandslos; `TelemetrySink:
//! Send + Sync` erlaubt Aufrufe aus beliebigen Threads gleichzeitig. Keine
//! globale Variable, kein `Mutex`.
//!
//! # Examples
//! ```rust
//! use harw_observe::NullSink;
//! use harw_plan_bridge::ReconcileStep;
//!
//! let sink = NullSink;
//! let steps: Vec<ReconcileStep> = Vec::new();
//! harw_plan_bridge::metrics::record_reconcile_steps(&sink, &steps);
//! ```

use harw_observe::{FieldValue, MetricValue, TelemetrySink};
use harw_plan::Plan;
use harw_plan::goal::{Goal, evaluate_goal};
use time::OffsetDateTime;

use crate::controller::ReconcileStep;

/// Label-Feldname für die Schrittart in [`RECONCILE_STEPS_TOTAL`].
const STEP_LABEL: harw_observe::FieldName = harw_macros::field!("step");

harw_macros::metrics! {
    /// Welche der neun `ReconcileStep`-Arten tatsächlich laufen (Label
    /// `step`, Kardinalität auf die neun Varianten begrenzt — siehe
    /// Moduldokumentation, Abschnitt "Kardinalität von
    /// `reconcile_steps_total`").
    RECONCILE_STEPS_TOTAL: counter, unit = count, labels = ["step"], cardinality = bounded(9),
        name = "harw_plan_reconcile_steps_total";
    /// Wie oft ein Plan-Knoten ungültig wird.
    INVALIDATIONS_TOTAL: counter, unit = count, labels = [], cardinality = single,
        name = "harw_plan_invalidations_total";
    /// Wie oft die Schleife eine Exploration vor einen Knoten schiebt.
    EXPLORE_INSERTED_TOTAL: counter, unit = count, labels = [], cardinality = single,
        name = "harw_plan_explore_inserted_total";
    /// Wie oft ein Nachweis an einen Knoten angehängt wird.
    EVIDENCE_ATTACHED_TOTAL: counter, unit = count, labels = [], cardinality = single,
        name = "harw_plan_evidence_attached_total";
    /// Wie viele Vorschläge aus der letzten Reconcile-Runde auf eine externe
    /// Entscheidung (Modell oder Mensch) warten.
    PROPOSALS_PENDING: gauge, unit = count, labels = [], cardinality = single,
        name = "harw_plan_proposals_pending";
    /// Alter des gebundenen Ziels in Tagen, gemessen gegen den injizierten
    /// Referenzzeitpunkt. `harw-observe::Unit` kennt keine Basiseinheit
    /// "Tage"; `unit = count` ist deshalb bewusst gewählt — die Einheit
    /// steckt im Namen, wie es das `metrics!`-Makro für `count` und `ratio`
    /// ausdrücklich zulässt.
    GOAL_AGE_DAYS: gauge, unit = count, labels = [], cardinality = single,
        name = "harw_plan_goal_age_days";
    /// Anteil der Akzeptanzkriterien des gebundenen Ziels, der durch Evidenz
    /// belegt ist (`0.0` bis `1.0`).
    GOAL_COVERAGE: gauge, unit = ratio, labels = [], cardinality = single,
        name = "harw_plan_goal_coverage";
}

/// Ordnet einen [`ReconcileStep`] seinem Label-Wert für
/// [`RECONCILE_STEPS_TOTAL`] zu.
///
/// # Description
/// Erschöpfendes `match` ohne Wildcard: eine zehnte `ReconcileStep`-Variante
/// bricht diese Funktion beim Kompilieren, statt die dokumentierte
/// Kardinalitätsgrenze `bounded(9)` still zu verletzen (siehe
/// Moduldokumentation, Abschnitt "Kardinalität von
/// `reconcile_steps_total`"). Die Werte sind identisch mit dem
/// `serde`-Tag von `ReconcileStep` (`#[serde(tag = "step", rename_all =
/// "snake_case")]`).
///
/// # Arguments
/// - `step` (`&ReconcileStep`): der zu benennende Schritt.
///
/// # Returns
/// Der `snake_case`-Name der Variante.
fn step_label(step: &ReconcileStep) -> &'static str {
    match step {
        ReconcileStep::AttachEvidence { .. } => "attach_evidence",
        ReconcileStep::Invalidate { .. } => "invalidate",
        ReconcileStep::InsertExplore { .. } => "insert_explore",
        ReconcileStep::ProposeExpand { .. } => "propose_expand",
        ReconcileStep::ProposeCondense { .. } => "propose_condense",
        ReconcileStep::MarkReady { .. } => "mark_ready",
        ReconcileStep::AdmitJobs { .. } => "admit_jobs",
        ReconcileStep::GoalStatus { .. } => "goal_status",
        ReconcileStep::AskModel { .. } => "ask_model",
    }
}

/// Emittiert [`RECONCILE_STEPS_TOTAL`] für jeden von `reconcile` erzeugten
/// Schritt.
///
/// # Description
/// Aufrufstelle:
/// [`PlanController::reconcile_observed`](crate::controller::PlanController::reconcile_observed),
/// direkt nach dem unveränderten Aufruf von
/// [`PlanController::reconcile`](crate::controller::PlanController::reconcile).
/// Ein Schritt erhöht den Zähler unabhängig davon, ob `apply` ihn später
/// tatsächlich anwendet oder als Vorschlag zurückgibt — die Frage, die diese
/// Metrik beantwortet, ist "welcher Zweig des Controllers wurde erreicht",
/// nicht "welche Mutation geschah".
///
/// # Arguments
/// - `sink` (`&dyn TelemetrySink`): das Ziel der Messwerte.
/// - `steps` (`&[ReconcileStep]`): die von `reconcile` gelieferte
///   Schrittfolge.
///
/// # Concurrency
/// Reine Funktion; `sink.record` ist aus jedem Thread aufrufbar.
pub fn record_reconcile_steps(sink: &dyn TelemetrySink, steps: &[ReconcileStep]) {
    for step in steps {
        sink.record(
            &RECONCILE_STEPS_TOTAL,
            MetricValue::Count(1),
            &[(STEP_LABEL, FieldValue::Str(step_label(step)))],
        );
    }
}

/// Setzt [`PROPOSALS_PENDING`] auf die Zahl der Vorschläge dieser Runde.
///
/// # Description
/// Zählt [`ReconcileStep::ProposeExpand`], [`ReconcileStep::ProposeCondense`],
/// [`ReconcileStep::AskModel`] und [`ReconcileStep::GoalStatus`] — die vier
/// Schrittarten, die laut Moduldokumentation von `controller.rs` Eingaben für
/// ein Modell oder einen menschlichen Akteur sind, keine Mutationen.
/// [`ReconcileStep::AdmitJobs`] zählt bewusst nicht mit: es wartet auf die
/// Job-Bridge, nicht auf eine externe Entscheidung.
///
/// # Arguments
/// - `sink` (`&dyn TelemetrySink`): das Ziel der Messwerte.
/// - `steps` (`&[ReconcileStep]`): die von `reconcile` gelieferte
///   Schrittfolge.
///
/// # Concurrency
/// Reine Funktion.
pub fn record_proposals_pending(sink: &dyn TelemetrySink, steps: &[ReconcileStep]) {
    let pending = steps
        .iter()
        .filter(|step| {
            matches!(
                step,
                ReconcileStep::ProposeExpand { .. }
                    | ReconcileStep::ProposeCondense { .. }
                    | ReconcileStep::AskModel { .. }
                    | ReconcileStep::GoalStatus { .. }
            )
        })
        .count();
    sink.record(&PROPOSALS_PENDING, MetricValue::Gauge(pending as f64), &[]);
}

/// Setzt [`GOAL_AGE_DAYS`] auf das Alter des Ziels zum injizierten Zeitpunkt.
///
/// # Description
/// Liest nur bereits vorhandene, injizierte Werte (`goal.created_at`, `now`)
/// — es wird keine Systemzeit gelesen. Ein negatives Alter (Uhr vor
/// `created_at`) wird auf `0` gekappt, statt eine negative Zahl zu zeigen, die
/// keine sinnvolle Interpretation hätte.
///
/// # Arguments
/// - `sink` (`&dyn TelemetrySink`): das Ziel der Messwerte.
/// - `goal` (`&Goal`): das gebundene Ziel.
/// - `now` (`OffsetDateTime`): der injizierte Referenzzeitpunkt (aus
///   `ReconcileInput::now`).
///
/// # Concurrency
/// Reine Funktion.
pub fn record_goal_age_days(sink: &dyn TelemetrySink, goal: &Goal, now: OffsetDateTime) {
    let age_days = (now - goal.created_at).whole_days().max(0) as f64;
    sink.record(&GOAL_AGE_DAYS, MetricValue::Gauge(age_days), &[]);
}

/// Setzt [`GOAL_COVERAGE`] auf den Anteil belegter Akzeptanzkriterien.
///
/// # Arguments
/// - `sink` (`&dyn TelemetrySink`): das Ziel der Messwerte.
/// - `goal` (`&Goal`): das gebundene Ziel.
/// - `plan` (`&Plan`): der aktuelle Plan-Snapshot.
///
/// # Concurrency
/// Reine Funktion; ruft `harw_plan::goal::evaluate_goal` auf — dieselbe reine
/// Funktion, die auch Regel (h) von `reconcile` nutzt.
pub fn record_goal_coverage(sink: &dyn TelemetrySink, goal: &Goal, plan: &Plan) {
    let coverage = f64::from(evaluate_goal(goal, plan).coverage).clamp(0.0, 1.0);
    sink.record(&GOAL_COVERAGE, MetricValue::Gauge(coverage), &[]);
}

/// Erhöht [`INVALIDATIONS_TOTAL`] um eins.
///
/// Aufrufstellen: [`record_apply_side_effects`], nach einer erfolgreich
/// angewandten [`ReconcileStep::Invalidate`], sowie
/// [`PlanJobBridge::on_job_failed_observed`](crate::job_bridge::PlanJobBridge::on_job_failed_observed).
pub(crate) fn record_invalidation(sink: &dyn TelemetrySink) {
    sink.record(&INVALIDATIONS_TOTAL, MetricValue::Count(1), &[]);
}

/// Erhöht [`EXPLORE_INSERTED_TOTAL`] um eins.
///
/// Aufrufstelle: [`record_apply_side_effects`], nach einem erfolgreich
/// eingefügten [`ReconcileStep::InsertExplore`].
pub(crate) fn record_explore_inserted(sink: &dyn TelemetrySink) {
    sink.record(&EXPLORE_INSERTED_TOTAL, MetricValue::Count(1), &[]);
}

/// Erhöht [`EVIDENCE_ATTACHED_TOTAL`] um eins.
///
/// Aufrufstellen: [`record_apply_side_effects`], nach einer erfolgreich
/// angehängten [`ReconcileStep::AttachEvidence`], sowie
/// [`PlanJobBridge::on_job_completed_observed`](crate::job_bridge::PlanJobBridge::on_job_completed_observed).
pub(crate) fn record_evidence_attached(sink: &dyn TelemetrySink) {
    sink.record(&EVIDENCE_ATTACHED_TOTAL, MetricValue::Count(1), &[]);
}

/// Emittiert die Zähler für tatsächlich angewandte Seiteneffekte.
///
/// # Description
/// Aufrufstelle:
/// [`PlanController::apply_observed`](crate::controller::PlanController::apply_observed),
/// nach einem erfolgreichen (`Ok`) Aufruf von
/// [`PlanController::apply`](crate::controller::PlanController::apply).
/// `apply` bricht beim ersten Fehler ab und gibt dann `Err` zurück — bei `Ok`
/// wurde also jeder Schritt aus `steps` entweder angewandt oder als Vorschlag
/// zurückgegeben. Ein direktes Auszählen der *eingegebenen* `steps` (statt
/// der zurückgegebenen Events) ist für `Ok`-Ergebnisse deshalb exakt.
///
/// [`ReconcileStep::AttachEvidence`] erhöht [`EVIDENCE_ATTACHED_TOTAL`],
/// [`ReconcileStep::Invalidate`] erhöht [`INVALIDATIONS_TOTAL`],
/// [`ReconcileStep::InsertExplore`] erhöht [`EXPLORE_INSERTED_TOTAL`]. Die
/// übrigen sechs Schrittarten tragen in diesem Modul keinen Zähler.
///
/// # Arguments
/// - `sink` (`&dyn TelemetrySink`): das Ziel der Messwerte.
/// - `steps` (`&[ReconcileStep]`): die an `apply` übergebene Schrittfolge.
///
/// # Concurrency
/// Reine Funktion.
pub fn record_apply_side_effects(sink: &dyn TelemetrySink, steps: &[ReconcileStep]) {
    for step in steps {
        match step {
            ReconcileStep::AttachEvidence { .. } => record_evidence_attached(sink),
            ReconcileStep::Invalidate { .. } => record_invalidation(sink),
            ReconcileStep::InsertExplore { .. } => record_explore_inserted(sink),
            ReconcileStep::ProposeExpand { .. }
            | ReconcileStep::ProposeCondense { .. }
            | ReconcileStep::MarkReady { .. }
            | ReconcileStep::AdmitJobs { .. }
            | ReconcileStep::GoalStatus { .. }
            | ReconcileStep::AskModel { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{
        RecordingSink, coding_node, covered_goal_fixture, node_with, plan_with,
    };
    use harw_plan::{EvidenceKind, EvidenceRef, InvalidationCondition, PathOrSymbol, PlanNodeKind,
        PlanNodeStatus, TaskId};

    fn evidence(locator: &str) -> EvidenceRef {
        EvidenceRef {
            kind: EvidenceKind::Manual,
            locator: locator.to_owned(),
            attached_at: OffsetDateTime::UNIX_EPOCH,
            actor: "test".to_owned(),
            digest: None,
        }
    }

    #[test]
    fn test_record_reconcile_steps_emits_one_counter_per_step_with_correct_label() {
        let sink = RecordingSink::new();
        let steps = vec![
            ReconcileStep::MarkReady {
                ids: vec![TaskId::new("t-1")],
            },
            ReconcileStep::AskModel {
                prompt: "geheimer Knotentitel sollte hier nie landen".to_owned(),
            },
        ];

        record_reconcile_steps(&sink, &steps);

        let recorded = sink.values_for(RECONCILE_STEPS_TOTAL.name);
        assert_eq!(recorded.len(), 2);
        assert!(matches!(recorded[0].value, MetricValue::Count(1)));
        assert_eq!(recorded[0].labels.len(), 1);
        assert_eq!(recorded[0].labels[0].0.as_str(), "step");
        assert_eq!(
            recorded[0].labels[0].1,
            FieldValue::Str("mark_ready")
        );
        assert_eq!(recorded[1].labels[0].1, FieldValue::Str("ask_model"));
    }

    #[test]
    fn test_record_proposals_pending_counts_only_proposal_steps() {
        let sink = RecordingSink::new();
        let steps = vec![
            ReconcileStep::ProposeExpand {
                parent: TaskId::new("t-1"),
                reason: "zu grob".to_owned(),
            },
            ReconcileStep::AdmitJobs {
                ids: vec![TaskId::new("t-2")],
            },
            ReconcileStep::AskModel {
                prompt: "prüfe".to_owned(),
            },
        ];

        record_proposals_pending(&sink, &steps);

        let recorded = sink.values_for(PROPOSALS_PENDING.name);
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].value, MetricValue::Gauge(2.0));
    }

    #[test]
    fn test_record_goal_age_days_computes_days_since_created_at() {
        let sink = RecordingSink::new();
        let (_plan, goal) = covered_goal_fixture();
        let now = goal.created_at + time::Duration::days(7);

        record_goal_age_days(&sink, &goal, now);

        let recorded = sink.values_for(GOAL_AGE_DAYS.name);
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].value, MetricValue::Gauge(7.0));
    }

    #[test]
    fn test_record_goal_age_days_never_goes_negative() {
        let sink = RecordingSink::new();
        let (_plan, goal) = covered_goal_fixture();
        let earlier = goal.created_at - time::Duration::days(3);

        record_goal_age_days(&sink, &goal, earlier);

        let recorded = sink.values_for(GOAL_AGE_DAYS.name);
        assert_eq!(recorded[0].value, MetricValue::Gauge(0.0));
    }

    #[test]
    fn test_record_goal_coverage_is_between_zero_and_one() {
        let sink = RecordingSink::new();
        let (plan, goal) = covered_goal_fixture();

        record_goal_coverage(&sink, &goal, &plan);

        let recorded = sink.values_for(GOAL_COVERAGE.name);
        assert_eq!(recorded.len(), 1);
        match recorded[0].value {
            MetricValue::Gauge(value) => {
                assert!((0.0..=1.0).contains(&value), "coverage außerhalb [0,1]: {value}");
                assert_eq!(value, 1.0, "die Fixture ist vollständig belegt");
            }
            other => panic!("erwartet Gauge, bekommen: {other:?}"),
        }
    }

    #[test]
    fn test_record_apply_side_effects_counts_attach_invalidate_and_explore_only() {
        let sink = RecordingSink::new();
        let target = coding_node("t-1", PlanNodeStatus::Draft);
        let explore = node_with("t-1-explore", PlanNodeKind::Explore, PlanNodeStatus::Draft);
        let steps = vec![
            ReconcileStep::AttachEvidence {
                task: TaskId::new("t-1"),
                evidence: evidence("p-1/research/q-1.md"),
            },
            ReconcileStep::Invalidate {
                ids: vec![TaskId::new("t-2")],
                condition: InvalidationCondition::ManualInvalidate,
            },
            ReconcileStep::InsertExplore {
                before: target.id.clone(),
                node: Box::new(explore),
            },
            ReconcileStep::MarkReady {
                ids: vec![TaskId::new("t-3")],
            },
        ];

        record_apply_side_effects(&sink, &steps);

        assert_eq!(sink.values_for(EVIDENCE_ATTACHED_TOTAL.name).len(), 1);
        assert_eq!(sink.values_for(INVALIDATIONS_TOTAL.name).len(), 1);
        assert_eq!(sink.values_for(EXPLORE_INSERTED_TOTAL.name).len(), 1);
        // MarkReady triggert keinen dieser drei Zähler.
        assert_eq!(sink.records().len(), 3);
    }

    #[test]
    fn test_no_emitted_label_contains_a_node_title_a_path_or_free_text() {
        let sink = RecordingSink::new();
        let mut sensitive_node = coding_node(
            "t-super-geheimes-projekt",
            PlanNodeStatus::Draft,
        );
        sensitive_node.write_scope = vec![PathOrSymbol::new("secrets/customer-42/keys.pem")];
        sensitive_node.objective = "Migriere den Kundenschlüssel für Kunde ACME".to_owned();
        let plan = plan_with(vec![sensitive_node]);

        let steps = vec![
            ReconcileStep::ProposeExpand {
                parent: TaskId::new("t-super-geheimes-projekt"),
                reason: "Migriere den Kundenschlüssel für Kunde ACME ist zu grob".to_owned(),
            },
            ReconcileStep::AskModel {
                prompt: "secrets/customer-42/keys.pem betroffen?".to_owned(),
            },
        ];

        record_reconcile_steps(&sink, &steps);
        record_proposals_pending(&sink, &steps);
        let (_plan_unused, goal) = covered_goal_fixture();
        record_goal_age_days(&sink, &goal, goal.created_at);
        record_goal_coverage(&sink, &goal, &plan);

        const KNOWN_STEP_LABELS: &[&str] = &[
            "attach_evidence",
            "invalidate",
            "insert_explore",
            "propose_expand",
            "propose_condense",
            "mark_ready",
            "admit_jobs",
            "goal_status",
            "ask_model",
        ];

        for record in sink.records() {
            for (name, value) in &record.labels {
                assert_eq!(name.as_str(), "step", "unerwartetes Label: {name}");
                match value {
                    FieldValue::Str(text) => assert!(
                        KNOWN_STEP_LABELS.contains(text),
                        "Label trägt keinen bekannten Schrittnamen: {text}"
                    ),
                    other => panic!("erwartet FieldValue::Str, bekommen: {other:?}"),
                }
            }
        }
    }
}
