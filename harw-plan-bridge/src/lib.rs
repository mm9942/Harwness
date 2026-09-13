//! `harw-plan-bridge` — die Naht zwischen Goal, Plan, Findings und Jobs.
//!
//! # Zweck
//! Ein Plan wird nicht einmal erzeugt und dann abgearbeitet, sondern
//! **entwickelt sich** (philosophy.md §5, §15; coding-philosophy.md §6–§8):
//!
//! - Recherche-Ergebnisse werden zu Evidenz an Plan-Knoten
//!   ([`finding_store`], [`controller`]).
//! - Riskante Arbeit bekommt vorher eine Exploration
//!   ([`ReconcileStep::InsertExplore`]).
//! - Grobe Knoten werden zur Zerlegung vorgeschlagen, erledigte Recherche zur
//!   Verdichtung ([`ReconcileStep::ProposeExpand`],
//!   [`ReconcileStep::ProposeCondense`]).
//! - Fertige Knoten werden zu Jobs, deren Ergebnis als Evidenz zurückfließt
//!   ([`job_bridge`]).
//! - Deklarative Zellen der Agent-DSL werden zu konkreten Fan-out-Wellen
//!   ([`cells`]).
//! - Und das Ziel überlebt jeden Compact und jeden Modellwechsel
//!   ([`goal_context`]).
//! - Und die Planschleife selbst wird beobachtbar: Zähler und Gauges für den
//!   Regelkreis aus Reconcile, Apply und Job-Rückkanal ([`metrics`]).
//! - Und Plan-Knoten werden — begrenzt auf das eigene Revier — zu
//!   [`harw_context::Fragment`]en für die Montage ([`plan_context`],
//!   Knoten AW3-03), samt der Registrierungsprüfung, die eine Namensraum-
//!   Kollision oder eine zu hohe Trust-Behauptung abweist
//!   ([`fragment_registry`]).
//! - Und ein Sicherheitsbefund hängt sich als Nachweis an, ohne den Plan
//!   je zu kommandieren: ein Vertragsverstoß erzeugt einen Vorschlag,
//!   niemals eine Invalidierung ([`security_bridge`], Knoten AW6-04).
//!
//! # Verantwortungsbereich
//! Diese Crate **besitzt keinen Zustand**. Plan und Goal gehören `harw-plan`,
//! Jobs gehören `harw-session-store`, Findings gehören `harw-research`. Hier
//! liegt ausschließlich die Übersetzung zwischen ihnen — und die Regeln, in
//! welcher Reihenfolge sie greifen.
//!
//! # Die Reinheit des Controllers
//! [`PlanController::reconcile`] ist eine reine Funktion: kein Dateisystem,
//! kein Netz, kein Zufall, keine Systemzeit — `now` wird injiziert. Zweimal
//! auf demselben Zustand aufgerufen liefert sie dieselbe Schrittfolge. Alles,
//! was Nebenwirkungen hat, steckt in [`PlanController::apply`] und
//! [`job_bridge`]; alles, was eine *Entscheidung* eines Menschen oder eines
//! Modells verlangt, wird als Vorschlag zurückgegeben statt still ausgeführt.
//!
//! # Wer darf ein Ziel für erreicht erklären
//! Niemand in dieser Crate. `reconcile` darf
//! [`ReconcileStep::GoalStatus`] vorschlagen; `apply` wendet ihn ausdrücklich
//! **nicht** an. Die Entscheidung bleibt bei einem menschlichen Akteur — so,
//! wie `harw_plan::goal::validate_goal_action` es erzwingt.
//!
//! # Zwei Zeitachsen
//! `harw-plan` datiert in `time::OffsetDateTime`, `harw-research` und
//! `harw-job-runtime` in `jiff::Timestamp`. Diese Crate ist die Naht zwischen
//! beiden und konvertiert an genau einer Stelle:
//! [`offset_from_timestamp`].
//!
//! # Concurrency
//! Alle exportierten Typen sind `Send + Sync`. Die Controller- und
//! Bridge-Typen sind zustandslos; die Stores, gegen die sie arbeiten, bringen
//! ihre eigene Thread-Sicherheit mit. Diese Crate spannt **keine**
//! Transaktion über Plan-Store und Job-Store — wo das sichtbar wird, ist es
//! an der jeweiligen Methode dokumentiert.
//!
//! # Fehler
//! Ein einziges Enum: [`PlanBridgeError`] (siehe [`error`]).
//!
//! # Examples
//! ```rust,no_run
//! use harw_plan_bridge::{PlanController, ReconcileInput};
//!
//! # fn demo(input: ReconcileInput<'_>, plan: &dyn harw_plan::PlanStore) {
//! let steps = PlanController::reconcile(input);
//! // Angewandt wird nur, was Runtime-Aktion ist; der Rest kommt zurück.
//! let applied = PlanController::apply(&steps, plan, None, "runtime");
//! let _ = applied;
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod cells;
pub mod context_ext;
pub mod controller;
pub mod error;
pub mod finding_store;
pub mod fragment_registry;
pub mod goal_context;
pub mod job_bridge;
pub mod metrics;
pub mod plan_context;
pub mod security_bridge;

pub use crate::cells::CellPlan;
pub use crate::context_ext::{OpContextPlanExt, register_plan_services};
pub use crate::controller::{PlanController, ReconcileInput, ReconcileStep};
pub use crate::error::{PlanBridgeError, PlanBridgeResult};
pub use crate::finding_store::{
    FindingStore, evidence_for_finding, finding_locator, offset_from_timestamp,
};
pub use crate::fragment_registry::{
    FragmentProviderDeclaration, FragmentProviderRegistry, FragmentRegistryError,
    FragmentRegistryResult,
};
pub use crate::goal_context::{DEFAULT_MAX_CHARS, GoalContextProvider};
pub use crate::job_bridge::{JobAdmissionTemplate, PlanJobBridge};
pub use crate::plan_context::{
    PLAN_CONTEXT_MAX_TRUST, PLAN_CONTEXT_MAY_CARRY_USER_CONTENT, PLAN_CONTEXT_NAMESPACE,
    PlanContextProvider,
};
pub use crate::security_bridge::{
    dock_security_finding, evidence_for_security_finding, propose_invalidation_for_contract_violation,
};

/// Gemeinsame Test-Fixtures für alle Module dieser Crate.
///
/// # Woher der Goal-Store kommt
/// Bis AP W1-08b hatte `harw_plan::goal::GoalStore` (der Trait) noch keine
/// Implementierung, und dieses Modul baute sich dafür ein eigenes
/// `#[cfg(test)]`-Double. Seit `harw_plan::InMemoryGoalStore` existiert (siehe
/// `harw-plan/src/goal_store.rs`) ist das Double ersatzlos entfallen:
/// [`InMemoryGoalStore`] ist hier nur noch ein `pub(crate)`-Re-Export der
/// echten Implementierung, damit `context_ext`, `controller` und
/// `goal_context` sie unverändert über `crate::testing::InMemoryGoalStore`
/// importieren können.
#[cfg(test)]
pub(crate) mod testing {
    use harw_plan::actions::PlanAction;
    use harw_plan::goal::{Goal, GoalId, GoalStatus, Invariant};
    use harw_plan::types::{Criterion, VerificationStep};
    use harw_plan::{
        EvidenceKind, EvidenceRef, InMemoryPlanStore, PathOrSymbol, Plan, PlanId, PlanNode,
        PlanNodeKind, PlanNodeStatus, PlanStore, PlanToolConfig, RevisionId, TaskId,
    };
    use harw_research::{Confidence, QuestionId, ResearchFinding, SourceClass, SourceReference};
    use time::OffsetDateTime;

    /// Re-Export des echten Goal-Stores für die Aufrufstellen in dieser Crate.
    pub(crate) use harw_plan::InMemoryGoalStore;

    /// Ein einzelner, von [`RecordingSink`] aufgezeichneter Messpunkt.
    ///
    /// Nur für Tests des Metrik-Moduls (Knoten AW1-05): erfasst genau das, was
    /// ein echter `TelemetrySink` empfangen hätte.
    #[derive(Debug, Clone)]
    pub(crate) struct RecordedMetric {
        /// Name der Messgröße (`MetricKey::name`).
        pub(crate) name: &'static str,
        /// Der gemessene Wert.
        pub(crate) value: harw_observe::MetricValue,
        /// Die konkreten Label-Belegungen dieses Aufrufs.
        pub(crate) labels: Vec<(harw_observe::FieldName, harw_observe::FieldValue)>,
    }

    /// Ein Test-`TelemetrySink`, der jeden empfangenen Messwert aufzeichnet.
    ///
    /// # Description
    /// Belegt, dass eine bestimmte `record_*`-Stelle tatsächlich emittiert
    /// (Knoten AW1-05), ohne ein echtes Sink-Backend (Datei, Prometheus) zu
    /// benötigen.
    ///
    /// # Concurrency
    /// `Send + Sync` über einen `std::sync::Mutex`; `record` ist aus
    /// beliebigen Threads aufrufbar, wie es der `TelemetrySink`-Vertrag
    /// verlangt. Ein vergifteter Mutex (nach einer Panik während `record`)
    /// wird als "nichts aufgezeichnet" behandelt statt selbst zu panicken —
    /// ein Test-Sink darf den beobachteten Test nicht zusätzlich zum
    /// Absturz bringen.
    #[derive(Debug, Default)]
    pub(crate) struct RecordingSink {
        records: std::sync::Mutex<Vec<RecordedMetric>>,
    }

    impl RecordingSink {
        /// Erzeugt einen leeren Recorder.
        pub(crate) fn new() -> Self {
            Self::default()
        }

        /// Gibt eine Kopie aller bisher aufgezeichneten Messpunkte zurück.
        pub(crate) fn records(&self) -> Vec<RecordedMetric> {
            match self.records.lock() {
                Ok(guard) => guard.clone(),
                Err(_) => Vec::new(),
            }
        }

        /// Alle aufgezeichneten Messpunkte für eine benannte Messgröße.
        pub(crate) fn values_for(&self, name: &str) -> Vec<RecordedMetric> {
            self.records()
                .into_iter()
                .filter(|record| record.name == name)
                .collect()
        }
    }

    impl harw_observe::TelemetrySink for RecordingSink {
        fn record(
            &self,
            key: &harw_observe::MetricKey,
            value: harw_observe::MetricValue,
            labels: &[(harw_observe::FieldName, harw_observe::FieldValue)],
        ) {
            if let Ok(mut guard) = self.records.lock() {
                guard.push(RecordedMetric {
                    name: key.name,
                    value,
                    labels: labels.to_vec(),
                });
            }
        }

        fn flush(&self) {}

        fn name(&self) -> &'static str {
            "recording-test-sink"
        }
    }

    /// Fester Zeitpunkt auf der `jiff`-Achse (deterministische Tests).
    pub(crate) fn timestamp() -> jiff::Timestamp {
        match "2026-08-27T10:00:00Z".parse::<jiff::Timestamp>() {
            Ok(parsed) => parsed,
            // Ein festes Literal kann nicht scheitern; der Zweig hält die
            // Fixture frei von `unwrap()`.
            Err(_) => jiff::Timestamp::UNIX_EPOCH,
        }
    }

    /// Fester Zeitpunkt auf der `time`-Achse.
    pub(crate) fn plan_time() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + time::Duration::days(20_000)
    }

    /// Baut einen Plan-Knoten beliebiger Art.
    pub(crate) fn node_with(id: &str, kind: PlanNodeKind, status: PlanNodeStatus) -> PlanNode {
        PlanNode {
            id: TaskId::new(id),
            kind,
            wave: None,
            objective: format!("Ziel von {id}"),
            dependencies: Vec::new(),
            input_contracts: Vec::new(),
            output_contracts: Vec::new(),
            read_scope: Vec::new(),
            write_scope: vec![PathOrSymbol::new(format!("src/{id}.rs"))],
            forbidden_scope: Vec::new(),
            acceptance_criteria: Vec::new(),
            invalidation_conditions: Vec::new(),
            status,
            evidence: Vec::new(),
            assignment: None,
            parent: None,
            created_at: plan_time(),
            updated_at: plan_time(),
        }
    }

    /// Ein Arbeitsknoten (`Coding`).
    pub(crate) fn coding_node(id: &str, status: PlanNodeStatus) -> PlanNode {
        node_with(id, PlanNodeKind::Coding, status)
    }

    /// Ein Recherche-Knoten (`Research`).
    pub(crate) fn research_node(id: &str, status: PlanNodeStatus) -> PlanNode {
        node_with(id, PlanNodeKind::Research, status)
    }

    /// Baut einen Plan-Snapshot von Hand (ohne Store-Validation).
    pub(crate) fn plan_with(nodes: Vec<PlanNode>) -> Plan {
        Plan {
            id: PlanId::new("p-test"),
            revision: RevisionId::new(1),
            parent_revision: None,
            goal_statement: "Test-Ziel".to_owned(),
            goal_id: None,
            nodes,
            created_at: plan_time(),
            updated_at: plan_time(),
        }
    }

    /// Legt einen `InMemoryPlanStore` mit Plan und Knoten an.
    pub(crate) fn seeded_plan_store(nodes: Vec<PlanNode>) -> InMemoryPlanStore {
        let store = InMemoryPlanStore::new();
        if let Err(error) = store.apply(
            PlanAction::Create {
                plan_id: PlanId::new("p-test"),
                goal: "Test-Ziel".to_owned(),
            },
            "test",
        ) {
            panic!("Plan anlegen: {error}");
        }
        for node in nodes {
            let id = node.id.clone();
            if let Err(error) = store.apply(PlanAction::AddNode { node }, "test") {
                panic!("Knoten '{id}' anlegen: {error}");
            }
        }
        store
    }

    /// Konfiguration, die Exploration vor `Coding` verlangt.
    pub(crate) fn exploration_config() -> PlanToolConfig {
        PlanToolConfig {
            require_exploration_for: vec![PlanNodeKind::Coding, PlanNodeKind::Integration],
            ..PlanToolConfig::enabled_defaults()
        }
    }

    /// Ein gültiges Recherche-Ergebnis mit optionalen offenen Fragen.
    pub(crate) fn sample_finding(question_id: &str, unresolved: &[&str]) -> ResearchFinding {
        ResearchFinding {
            question_id: QuestionId::new(question_id),
            conclusion: format!("Antwort auf {question_id}"),
            evidence: vec![SourceReference {
                kind: SourceClass::LocalSource,
                locator: "src/lib.rs".to_owned(),
                retrieved_at: timestamp(),
                digest: None,
                excerpt: "Beleg".to_owned(),
            }],
            verified_versions: Vec::new(),
            constraints: Vec::new(),
            compatibility_notes: Vec::new(),
            unresolved_questions: unresolved
                .iter()
                .map(|question| (*question).to_owned())
                .collect(),
            confidence: Confidence::High,
            produced_by: "explorer-1".to_owned(),
            produced_at: timestamp(),
        }
    }

    /// Ein Ziel mit genau einem noch offenen Kriterium und einer Invariante.
    pub(crate) fn goal_with_open_criterion() -> Goal {
        Goal {
            id: GoalId::new("g-test"),
            revision: 1,
            statement: "Bridge fertigstellen".to_owned(),
            non_goals: vec!["kein Rewrite von harw-plan".to_owned()],
            invariants: vec![Invariant {
                id: "inv-1".to_owned(),
                statement: "keine unsafe-Blöcke".to_owned(),
                verification: vec![VerificationStep::Manual {
                    note: "review".to_owned(),
                }],
            }],
            acceptance_criteria: vec![Criterion {
                description: "alle Tests grün".to_owned(),
                verification: vec![VerificationStep::Command {
                    cmd: "cargo test".to_owned(),
                    expect_exit: 0,
                }],
            }],
            constraints: Vec::new(),
            open_questions: Vec::new(),
            status: GoalStatus::Active,
            plan_id: Some(PlanId::new("p-test")),
            plan_revision: Some(RevisionId::new(1)),
            evidence: Vec::new(),
            created_at: plan_time(),
            updated_at: plan_time(),
        }
    }

    /// Plan und Ziel, bei dem jedes Kriterium durch Evidenz belegt ist.
    pub(crate) fn covered_goal_fixture() -> (Plan, Goal) {
        let mut node = coding_node("t-1", PlanNodeStatus::Completed);
        node.evidence = vec![EvidenceRef {
            kind: EvidenceKind::Manual,
            locator: "handgeprueft".to_owned(),
            attached_at: plan_time(),
            actor: "operator".to_owned(),
            digest: None,
        }];
        let plan = plan_with(vec![node]);

        let mut goal = goal_with_open_criterion();
        goal.invariants = Vec::new();
        goal.acceptance_criteria = vec![Criterion {
            description: "manuell abgenommen".to_owned(),
            verification: vec![VerificationStep::Manual {
                note: "handgeprueft".to_owned(),
            }],
        }];
        (plan, goal)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        // Die früheren Tests `test_in_memory_goal_store_reports_no_goal_before_set`,
        // `test_in_memory_goal_store_records_history_and_revision` und
        // `test_in_memory_goal_store_rejects_a_model_declaring_achievement` prüften
        // ausschließlich das inzwischen entfernte Store-Double selbst. Dasselbe
        // Verhalten — kein Goal vor `Set`, Revisions-/History-Fortschreibung, die
        // Ablehnung eines `model:`-Akteurs bei `Achieved` — ist bereits in
        // `harw-plan/src/goal_store.rs` gegen die echte Implementierung getestet.
        // Sie sind ersatzlos entfallen, nicht neu geschrieben worden: ein zweiter
        // Test derselben Store-Mechanik gegen dieselbe Implementierung wäre keine
        // zusätzliche Abdeckung gewesen.

        #[test]
        fn test_covered_goal_fixture_is_actually_covered() {
            let (plan, goal) = covered_goal_fixture();
            let report = harw_plan::goal::evaluate_goal(&goal, &plan);
            assert!(report.criteria_open.is_empty(), "{report:?}");
            assert!(report.invariants_violated.is_empty(), "{report:?}");
            assert!(report.coverage >= 1.0);
        }

        #[test]
        fn test_seeded_plan_store_accepts_the_shared_fixtures() {
            let store = seeded_plan_store(vec![
                coding_node("t-1", PlanNodeStatus::Ready),
                coding_node("t-2", PlanNodeStatus::Completed),
            ]);
            match store.current() {
                Ok(plan) => assert_eq!(plan.nodes.len(), 2),
                Err(error) => panic!("current schlug fehl: {error}"),
            }
        }
    }
}
