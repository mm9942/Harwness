//! Der Controller-Loop: aus Zustand werden nächste Schritte.
//!
//! # Verantwortungsbereich
//! [`PlanController::reconcile`] leitet aus einem Snapshot (Goal, Plan, neue
//! Findings, Job-Zustände, Konfiguration, Zeitpunkt) eine **geordnete** Liste
//! von [`ReconcileStep`]s ab. [`PlanController::apply`] wendet davon genau die
//! Schritte an, die Runtime-Aktionen sind, und gibt die übrigen als Vorschläge
//! an das Modell zurück.
//!
//! # Reinheit
//! `reconcile` macht keine I/O, benutzt keinen Zufall und liest keine
//! Systemzeit — `now` wird injiziert. Zweimal auf demselben Zustand aufgerufen
//! liefert es dieselbe Schrittfolge. Das ist keine Nebenbemerkung, sondern die
//! Bedingung dafür, dass ein Plan-Reconcile überhaupt testbar und
//! nachvollziehbar ist (coding-philosophy.md §6–§8).
//!
//! Wo eine unterlagerte Funktion keine Ordnung garantiert
//! (`graph::condense_candidates` liefert Gruppen aus einer `HashMap`), sortiert
//! dieser Controller nach, statt sich auf Zufall zu verlassen.
//!
//! # Wer darf was
//! - Der Controller **schlägt vor** — [`ReconcileStep::ProposeExpand`],
//!   [`ReconcileStep::ProposeCondense`], [`ReconcileStep::AskModel`] sind
//!   Eingaben für das Modell, keine Mutationen.
//! - Der Controller **erklärt kein Ziel für erreicht**:
//!   [`ReconcileStep::GoalStatus`] wird von `apply` bewusst *nicht* angewandt,
//!   sondern zurückgegeben. Nur ein menschlicher Akteur darf ein Goal auf
//!   `Achieved` setzen (`harw_plan::goal::validate_goal_action`).
//! - [`ReconcileStep::AdmitJobs`] braucht einen `JobStore` und wird deshalb
//!   ebenfalls zurückgegeben — es ist die Aufgabe von
//!   [`crate::job_bridge::PlanJobBridge`].
//!
//! # Was der Controller *nicht* tut
//! Er analysiert keine Semantik. Ob ein Finding einem `output_contract`
//! inhaltlich widerspricht, kann er nicht entscheiden und behauptet es auch
//! nicht. Der einzige prüfbare Fall — ein Finding mit offenen Fragen an einem
//! bereits abgeschlossenen Knoten — wird als [`ReconcileStep::AskModel`]
//! ausgegeben, damit ein Modell die Frage beantwortet.
//!
//! # Exportierte Typen
//! [`ReconcileInput`], [`ReconcileStep`], [`PlanController`].
//!
//! # Concurrency
//! `PlanController` ist ein zustandsloser Namensraum (`Send + Sync`).
//! `reconcile` arbeitet auf einem Snapshot ohne gehaltene Locks; `apply`
//! serialisiert über die `PlanStore`-Implementierung.
//!
//! # Fehler
//! `apply` gibt [`PlanBridgeError::Plan`] weiter, wenn `harw-plan` eine
//! Mutation ablehnt, und [`PlanBridgeError::GoalUnbound`], wenn ein
//! `GoalStatus`-Vorschlag ohne gebundenen Goal-Store ankommt.

use std::collections::HashSet;

use harw_job_runtime::JobState;
use harw_observe::TelemetrySink;
use harw_plan::actions::{PlanAction, PlanEvent};
use harw_plan::goal::{Goal, GoalStatus, GoalStore, evaluate_goal};
use harw_plan::graph;
use harw_plan::{
    EvidenceKind, EvidenceRef, InvalidationCondition, PathOrSymbol, Plan, PlanNode, PlanNodeKind,
    PlanNodeStatus, PlanStore, PlanToolConfig, TaskId,
};
use harw_research::ResearchFinding;
use serde::Serialize;
use time::OffsetDateTime;

use crate::error::PlanBridgeError;
use crate::finding_store::evidence_for_finding;

/// Mindestgröße einer Gruppe, damit sie als Verdichtungskandidat gilt.
const CONDENSE_MIN_GROUP: usize = 3;

/// Ab dieser Zahl von `write_scope`-Einträgen gilt ein Draft-Knoten als grob.
const COARSE_WRITE_SCOPE: usize = 5;

/// Akteur, unter dem Job-Evidenz angehängt wird.
const JOB_ACTOR: &str = "runtime:job";

/// Knotenarten, die als Job an einen Worker gehen.
const JOB_KINDS: &[PlanNodeKind] = &[
    PlanNodeKind::Coding,
    PlanNodeKind::Integration,
    PlanNodeKind::Verification,
    PlanNodeKind::Docs,
];

/// Der vollständige Eingabezustand einer Reconcile-Runde.
///
/// # Description
/// Alles, was `reconcile` liest, steht hier — es gibt keine verborgene
/// Abhängigkeit auf Systemzeit, Dateisystem oder globalen Zustand.
///
/// # Concurrency
/// Reiner Referenz-Container; `Send + Sync`, solange die referenzierten Werte
/// es sind.
#[derive(Debug, Clone, Copy)]
pub struct ReconcileInput<'a> {
    /// Das gebundene Goal, falls vorhanden.
    pub goal: Option<&'a Goal>,
    /// Der aktuelle Plan-Snapshot.
    pub plan: &'a Plan,
    /// Findings, die seit der letzten Runde hinzugekommen sind.
    pub new_findings: &'a [ResearchFinding],
    /// Beobachtete Job-Zustände als `(WorkId-String, Zustand)`.
    pub job_states: &'a [(String, JobState)],
    /// Die geltende Plan-Tool-Konfiguration.
    pub config: &'a PlanToolConfig,
    /// Der injizierte Referenzzeitpunkt.
    pub now: OffsetDateTime,
}

/// Ein einzelner Schritt, den der Controller vorschlägt oder anwendet.
///
/// # Description
/// Die Reihenfolge in der von `reconcile` gelieferten Liste ist bedeutsam:
/// Evidenz wird angehängt, bevor invalidiert wird, und invalidiert wird, bevor
/// Bereitschaft erklärt wird.
///
/// # Konvention: Job-Evidenz schließt einen Knoten ab
/// Ein [`Self::AttachEvidence`] mit `evidence.kind == EvidenceKind::Job`
/// bedeutet "der zugehörige Job ist erfolgreich beendet". [`PlanController::apply`]
/// setzt den Knoten deshalb im selben Schritt auf `Completed`. Für jede andere
/// `EvidenceKind` bleibt der Status unberührt.
///
/// # Concurrency
/// Reiner Werttyp, `Clone + Send + Sync`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum ReconcileStep {
    /// Hängt einen Nachweis an einen Knoten (siehe Konvention oben).
    AttachEvidence {
        /// Der belegte Knoten.
        task: TaskId,
        /// Der anzuhängende Nachweis.
        evidence: EvidenceRef,
    },
    /// Invalidiert eine Menge von Knoten mit einem Grund.
    Invalidate {
        /// Die zu invalidierenden Knoten.
        ids: Vec<TaskId>,
        /// Der Grund der Invalidierung.
        condition: InvalidationCondition,
    },
    /// Schiebt eine Exploration vor einen Knoten, der ohne sie nicht laufen darf.
    InsertExplore {
        /// Der Knoten, der die Exploration voraussetzt.
        before: TaskId,
        /// Der einzufügende `Explore`-Knoten.
        ///
        /// `Box`ed, weil `PlanNode` (mehrere `Vec`-Felder, zwei
        /// `OffsetDateTime`, mehrere `Option`s) die anderen `ReconcileStep`-
        /// Varianten sonst um ein Vielfaches aufbläht — jede Variante würde
        /// die Größe der größten tragen, obwohl `InsertExplore` der einzige
        /// Fall ist, der einen ganzen `PlanNode` transportiert. Alle
        /// Konstruktions- und Match-Stellen liegen in diesem Modul; der
        /// einzige externe Leser (`harw-ops/src/plan.rs`) greift nur lesend
        /// auf `node.id` zu, was durch `Box`s `Deref` unverändert funktioniert.
        node: Box<PlanNode>,
    },
    /// Vorschlag an das Modell: einen groben Knoten zerlegen.
    ProposeExpand {
        /// Der zu zerlegende Knoten.
        parent: TaskId,
        /// Begründung für den Vorschlag.
        reason: String,
    },
    /// Vorschlag an das Modell: eine Gruppe erledigter Recherche verdichten.
    ProposeCondense {
        /// Die zu verdichtenden Knoten.
        group: Vec<TaskId>,
        /// Begründung für den Vorschlag.
        reason: String,
    },
    /// Erklärt Knoten für ausführbar (`Status = Ready`).
    MarkReady {
        /// Die betroffenen Knoten.
        ids: Vec<TaskId>,
    },
    /// Bittet die Job-Bridge, diese Knoten als Jobs zu admittieren.
    AdmitJobs {
        /// Die zu admittierenden Knoten.
        ids: Vec<TaskId>,
    },
    /// Vorschlag an einen menschlichen Akteur: Zielstatus setzen.
    GoalStatus {
        /// Der vorgeschlagene Status.
        status: GoalStatus,
        /// Begründung des Vorschlags.
        reason: String,
    },
    /// Frage an das Modell, die der Controller nicht selbst entscheiden darf.
    AskModel {
        /// Der vollständige, präzise Prompt.
        prompt: String,
    },
}

/// Kanonische Form eines Schrittes für den Gleichheitsvergleich.
///
/// Gibt `None` zurück, wenn die Serialisierung scheitert; der Vergleich wertet
/// das konservativ als "ungleich", statt Gleichheit zu behaupten.
fn canonical(step: &ReconcileStep) -> Option<serde_json::Value> {
    serde_json::to_value(step).ok()
}

/// Strukturelle Gleichheit über die serialisierte Form.
///
/// # Description
/// Ein abgeleitetes `PartialEq` ist nicht möglich: `EvidenceRef`, `PlanNode`
/// und `InvalidationCondition` aus `harw-plan` leiten `PartialEq` nicht ab.
/// Statt einen handgeschriebenen Feldvergleich zu pflegen, der beim nächsten
/// Feld in `PlanNode` still unvollständig würde, wird die serialisierte Form
/// verglichen — sie erfasst *jedes* Feld und bleibt bei Erweiterungen korrekt.
/// `serde_json::Map` ist ohne `preserve_order` eine `BTreeMap`, der Vergleich
/// ist damit unabhängig von der Feldreihenfolge.
impl PartialEq for ReconcileStep {
    fn eq(&self, other: &Self) -> bool {
        match (canonical(self), canonical(other)) {
            (Some(left), Some(right)) => left == right,
            _ => false,
        }
    }
}

/// Der Controller — ein zustandsloser Namensraum.
///
/// # Concurrency
/// Enthält keinen Zustand; alle Methoden sind assoziierte Funktionen und aus
/// beliebig vielen Threads gleichzeitig aufrufbar.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlanController;

impl PlanController {
    /// Leitet aus dem aktuellen Zustand die nächsten Schritte ab.
    ///
    /// # Description
    /// Rein: keine I/O, kein Zufall, `now` wird injiziert. Die Regeln laufen in
    /// dieser Reihenfolge (AP W3-06..13, Regeln a–h):
    ///
    /// a. Jedes neue Finding, das sich einem Knoten zuordnen lässt, wird als
    ///    [`ReconcileStep::AttachEvidence`] angehängt. Zuordnung über
    ///    `TaskId == question_id` oder über einen `Research`/`Explore`-Knoten,
    ///    dessen `objective` die `question_id` enthält.
    /// b. Ein Finding mit offenen Fragen an einem bereits `Completed`-Knoten
    ///    erzeugt eine [`ReconcileStep::AskModel`]-Frage. Es wird **keine**
    ///    semantische Widerspruchsanalyse versucht.
    /// c. Knoten aus `graph::missing_explorations` bekommen je einen
    ///    generierten `Explore`-Knoten (`<zielid>-explore`) vorgeschaltet.
    /// d. Gruppen aus `graph::condense_candidates` (ab
    ///    [`CONDENSE_MIN_GROUP`] Mitgliedern) werden als
    ///    [`ReconcileStep::ProposeCondense`] vorgeschlagen — sortiert, damit
    ///    die Ausgabe deterministisch ist.
    /// e. Composite-Knoten mit ausschließlich abgeschlossenen Kindern werden
    ///    bereit gemeldet bzw. zum Abschluss angeregt; Draft-Knoten mit
    ///    mindestens [`COARSE_WRITE_SCOPE`] Schreibzielen als
    ///    [`ReconcileStep::ProposeExpand`].
    /// f. Ausführbare Knoten der Arten [`JOB_KINDS`] werden zur Admission
    ///    vorgeschlagen, die übrigen auf `Ready` gesetzt. Knoten, denen laut
    ///    (c) noch eine Exploration fehlt, werden dabei übersprungen — sie
    ///    dürfen laut Plan-Validation ohnehin nicht starten.
    /// g. Beobachtete Job-Zustände: `Completed` erzeugt Job-Evidenz (und damit
    ///    den Abschluss des Knotens, siehe Konvention an [`ReconcileStep`]),
    ///    `Failed` eine Invalidierung.
    /// h. Ist ein Goal gebunden und vollständig belegt, wird
    ///    [`ReconcileStep::GoalStatus`] als *Vorschlag* ausgegeben.
    ///
    /// # Arguments
    /// - `input` (`ReconcileInput<'_>`): der vollständige Eingabezustand.
    ///
    /// # Returns
    /// Die geordnete Schrittfolge; leer, wenn nichts zu tun ist.
    ///
    /// # Concurrency
    /// Rein funktional; hält keine Locks und ist beliebig parallel aufrufbar.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_plan_bridge::{PlanController, ReconcileInput};
    /// # fn demo(input: ReconcileInput<'_>) {
    /// let steps = PlanController::reconcile(input);
    /// assert!(steps.len() >= 0);
    /// # }
    /// ```
    pub fn reconcile(input: ReconcileInput<'_>) -> Vec<ReconcileStep> {
        let plan = input.plan;
        let plan_id = plan.id.as_str();
        let mut steps: Vec<ReconcileStep> = Vec::new();

        // ── a) Findings werden zu Evidenz ────────────────────────────────
        let mut matched: Vec<(TaskId, &ResearchFinding)> = Vec::new();
        for finding in input.new_findings {
            let Some(node) = match_finding_node(plan, finding) else {
                tracing::debug!(
                    question_id = %finding.question_id,
                    "Finding ohne zugeordneten Plan-Knoten — übersprungen"
                );
                continue;
            };
            steps.push(ReconcileStep::AttachEvidence {
                task: node.id.clone(),
                evidence: evidence_for_finding(plan_id, finding, &finding.produced_by),
            });
            matched.push((node.id.clone(), finding));
        }

        // ── b) Offene Fragen an bereits abgeschlossenen Knoten ────────────
        for (task, finding) in &matched {
            if finding.unresolved_questions.is_empty() {
                continue;
            }
            let Some(node) = find_node(plan, task) else {
                continue;
            };
            if node.status != PlanNodeStatus::Completed {
                continue;
            }
            steps.push(ReconcileStep::AskModel {
                prompt: contradiction_prompt(finding, node),
            });
        }

        // ── c) Fehlende Explorationen ────────────────────────────────────
        let missing = graph::missing_explorations(plan, input.config, input.now);
        let missing_set: HashSet<&str> = missing.iter().map(TaskId::as_str).collect();
        for target_id in &missing {
            let Some(target) = find_node(plan, target_id) else {
                continue;
            };
            let explore_id = TaskId::new(format!("{target_id}-explore"));
            if plan.nodes.iter().any(|node| node.id == explore_id) {
                // Die Exploration existiert bereits — sie ist nur noch nicht
                // abgeschlossen. Kein zweiter Knoten.
                continue;
            }
            steps.push(ReconcileStep::InsertExplore {
                before: target_id.clone(),
                node: Box::new(explore_node_for(target, explore_id, input.now)),
            });
        }

        // ── d) Verdichtungskandidaten ────────────────────────────────────
        let mut groups = graph::condense_candidates(plan, CONDENSE_MIN_GROUP);
        for group in &mut groups {
            group.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        }
        // `condense_candidates` liefert die Gruppen aus einer `HashMap` und
        // garantiert keine Ordnung. Ohne diese Sortierung wäre `reconcile`
        // nicht deterministisch.
        groups.sort_by(|left, right| {
            let left_key = left.first().map(TaskId::as_str).unwrap_or_default();
            let right_key = right.first().map(TaskId::as_str).unwrap_or_default();
            left_key.cmp(right_key)
        });
        for group in groups {
            steps.push(ReconcileStep::ProposeCondense {
                reason: format!(
                    "{} abgeschlossene Research/Explore-Knoten teilen sich einen read_scope — \
                     verdichte sie zu einem Contract-Knoten.",
                    group.len()
                ),
                group,
            });
        }

        // ── e) Composite-Abschluss und grobe Knoten ──────────────────────
        let mut composite_ready: Vec<TaskId> = Vec::new();
        let mut composite_hints: Vec<ReconcileStep> = Vec::new();
        let mut coarse: Vec<ReconcileStep> = Vec::new();
        for node in &plan.nodes {
            if node.kind == PlanNodeKind::Composite {
                let children = graph::children_of(plan, &node.id);
                let all_done = !children.is_empty()
                    && children
                        .iter()
                        .all(|child| child.status == PlanNodeStatus::Completed);
                if all_done {
                    match node.status {
                        PlanNodeStatus::Draft | PlanNodeStatus::Blocked => {
                            composite_ready.push(node.id.clone());
                        }
                        PlanNodeStatus::Ready | PlanNodeStatus::InProgress => {
                            composite_hints.push(ReconcileStep::AskModel {
                                prompt: format!(
                                    "Composite-Knoten '{}' hat nur noch abgeschlossene Kinder \
                                     ({} Stück). Schließe ihn ab oder benenne die Arbeit, die \
                                     noch fehlt.",
                                    node.id,
                                    children.len()
                                ),
                            });
                        }
                        _ => {}
                    }
                }
            }

            if node.status == PlanNodeStatus::Draft && node.write_scope.len() >= COARSE_WRITE_SCOPE
            {
                coarse.push(ReconcileStep::ProposeExpand {
                    parent: node.id.clone(),
                    reason: format!(
                        "Knoten '{}' ist noch Draft und beansprucht {} Schreibziele — das ist \
                         zu grob für einen Ausführungsschritt. Zerlege ihn.",
                        node.id,
                        node.write_scope.len()
                    ),
                });
            }
        }
        if !composite_ready.is_empty() {
            steps.push(ReconcileStep::MarkReady {
                ids: composite_ready.clone(),
            });
        }
        steps.extend(composite_hints);
        steps.extend(coarse);

        // ── g) (vorgezogen berechnet) Job-Zustände ───────────────────────
        // Die *Ausgabe* bleibt in Regel-Reihenfolge (f vor g); die Berechnung
        // muss aber vorgezogen werden, damit (f) Knoten überspringt, deren Job
        // gerade terminal geworden ist.
        let (job_steps, job_touched) = job_state_steps(plan, input.job_states, input.now);

        // ── f) Bereitschaft und Admission ────────────────────────────────
        let composite_set: HashSet<&str> = composite_ready.iter().map(TaskId::as_str).collect();
        let mut admit: Vec<TaskId> = Vec::new();
        let mut mark: Vec<TaskId> = Vec::new();
        for node in graph::ready_nodes(plan) {
            let id = node.id.as_str();
            if missing_set.contains(id) || composite_set.contains(id) || job_touched.contains(id) {
                continue;
            }
            if JOB_KINDS.contains(&node.kind) {
                admit.push(node.id.clone());
            } else {
                mark.push(node.id.clone());
            }
        }
        if !admit.is_empty() {
            steps.push(ReconcileStep::AdmitJobs { ids: admit });
        }
        if !mark.is_empty() {
            steps.push(ReconcileStep::MarkReady { ids: mark });
        }

        // ── g) Job-Zustände (Ausgabe) ────────────────────────────────────
        steps.extend(job_steps);

        // ── h) Zielbewertung als Vorschlag ───────────────────────────────
        if let Some(goal) = input.goal {
            let report = evaluate_goal(goal, plan);
            // Ein Goal ohne Akzeptanzkriterien meldet per Definition
            // `coverage == 1.0`. Es deshalb für erreicht zu erklären wäre
            // genau der Fehler, den philosophy.md §5 verbietet.
            let fully_covered = !goal.acceptance_criteria.is_empty()
                && report.criteria_open.is_empty()
                && report.coverage >= 1.0;
            if fully_covered && report.invariants_violated.is_empty() {
                steps.push(ReconcileStep::GoalStatus {
                    status: GoalStatus::Achieved,
                    reason: format!(
                        "Alle {} Akzeptanzkriterien sind durch Evidenz abgeschlossener Knoten \
                         belegt und keine Invariante ist verletzt. Nur ein menschlicher Akteur \
                         darf das Ziel für erreicht erklären.",
                        goal.acceptance_criteria.len()
                    ),
                });
            }
        }

        steps
    }

    /// Wie [`Self::reconcile`], zusätzlich mit Telemetrie über die
    /// Planschleife (Knoten AW1-05).
    ///
    /// # Description
    /// Ruft [`Self::reconcile`] unverändert auf — die Reconcile-Logik ändert
    /// sich durch diese Methode nicht. Ist `sink` `Some`, emittiert sie
    /// danach [`crate::metrics::RECONCILE_STEPS_TOTAL`] für jeden erzeugten
    /// Schritt und [`crate::metrics::PROPOSALS_PENDING`] für die Zahl der
    /// Vorschläge dieser Runde; ist zusätzlich ein Goal gebunden, auch
    /// [`crate::metrics::GOAL_AGE_DAYS`] und [`crate::metrics::GOAL_COVERAGE`].
    ///
    /// [`PlanController`] ist ein zustandsloser Namensraum ohne Konstruktor —
    /// es gibt keine Instanz, an der ein Sink dauerhaft hinterlegt werden
    /// könnte, ohne jeden bestehenden Aufrufer von [`Self::reconcile`] zu
    /// brechen. Der Sink wird deshalb als `Option`-Parameter hereingereicht
    /// statt global gehalten (siehe [`crate::metrics`], Abschnitt "Wie der
    /// Sink hereinkommt").
    ///
    /// # Arguments
    /// - `input` (`ReconcileInput<'_>`): identisch zu [`Self::reconcile`].
    /// - `sink` (`Option<&dyn TelemetrySink>`): Ziel der Messwerte; `None`
    ///   unterdrückt jede Emission.
    ///
    /// # Returns
    /// Dieselbe Schrittfolge wie [`Self::reconcile`].
    ///
    /// # Concurrency
    /// Wie [`Self::reconcile`]; `sink.record` ist aus jedem Thread aufrufbar.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_plan_bridge::{PlanController, ReconcileInput};
    /// # fn demo(input: ReconcileInput<'_>) {
    /// let steps = PlanController::reconcile_observed(input, None);
    /// assert!(steps.len() >= 0);
    /// # }
    /// ```
    pub fn reconcile_observed(
        input: ReconcileInput<'_>,
        sink: Option<&dyn TelemetrySink>,
    ) -> Vec<ReconcileStep> {
        let steps = Self::reconcile(input);
        if let Some(sink) = sink {
            crate::metrics::record_reconcile_steps(sink, &steps);
            crate::metrics::record_proposals_pending(sink, &steps);
            if let Some(goal) = input.goal {
                crate::metrics::record_goal_age_days(sink, goal, input.now);
                crate::metrics::record_goal_coverage(sink, goal, input.plan);
            }
        }
        steps
    }

    /// Wendet die Runtime-Schritte an und gibt die Vorschläge zurück.
    ///
    /// # Description
    /// Angewandt werden [`ReconcileStep::AttachEvidence`],
    /// [`ReconcileStep::Invalidate`], [`ReconcileStep::InsertExplore`] und
    /// [`ReconcileStep::MarkReady`].
    ///
    /// Nicht angewandt, sondern unverändert zurückgegeben werden:
    /// - [`ReconcileStep::AskModel`], [`ReconcileStep::ProposeExpand`],
    ///   [`ReconcileStep::ProposeCondense`] — Vorschläge an das Modell.
    /// - [`ReconcileStep::AdmitJobs`] — braucht einen `JobStore`; siehe
    ///   [`crate::job_bridge::PlanJobBridge::admit_ready_nodes`].
    /// - [`ReconcileStep::GoalStatus`] — nur ein menschlicher Akteur darf ein
    ///   Goal als erreicht erklären.
    ///
    /// Ein `AttachEvidence` mit `kind == EvidenceKind::Job` zieht zusätzlich
    /// ein `SetStatus(Completed)` nach sich (Konvention an [`ReconcileStep`]).
    ///
    /// Die Verarbeitung bricht beim ersten Fehler ab; bereits erzeugte Events
    /// sind dann bereits im Store persistiert — der Aufrufer reagiert darauf
    /// mit einer neuen `reconcile`-Runde, nicht mit einem Rollback.
    ///
    /// # Arguments
    /// - `steps` (`&[ReconcileStep]`): die anzuwendende Schrittfolge.
    /// - `plan` (`&dyn PlanStore`): der zu mutierende Plan-Store.
    /// - `goal` (`Option<&dyn GoalStore>`): der gebundene Goal-Store, falls
    ///   vorhanden. Wird nur geprüft, nie mutiert.
    /// - `actor` (`&str`): der Akteur, unter dem die Mutationen laufen.
    ///
    /// # Returns
    /// `(Vec<PlanEvent>, Vec<ReconcileStep>)` — die erzeugten Plan-Events und
    /// die nicht angewandten Vorschläge, in Eingabereihenfolge.
    ///
    /// # Errors
    /// - [`PlanBridgeError::Plan`]: wenn `harw-plan` eine Mutation ablehnt.
    /// - [`PlanBridgeError::GoalUnbound`]: wenn ein `GoalStatus`-Vorschlag
    ///   ankommt, obwohl kein Goal-Store gebunden ist — der Vorschlag hätte
    ///   dann keinen Adressaten.
    ///
    /// # Concurrency
    /// Serialisiert über die Mutationsstelle des `PlanStore`; mehrere
    /// gleichzeitige `apply`-Aufrufe auf denselben Store sind sicher, aber
    /// nicht als Transaktion isoliert.
    pub fn apply(
        steps: &[ReconcileStep],
        plan: &dyn PlanStore,
        goal: Option<&dyn GoalStore>,
        actor: &str,
    ) -> Result<(Vec<PlanEvent>, Vec<ReconcileStep>), PlanBridgeError> {
        let mut events: Vec<PlanEvent> = Vec::new();
        let mut deferred: Vec<ReconcileStep> = Vec::new();

        for step in steps {
            match step {
                ReconcileStep::AttachEvidence { task, evidence } => {
                    let closes_node = evidence.kind == EvidenceKind::Job;
                    events.push(plan.apply(
                        PlanAction::AttachEvidence {
                            id: task.clone(),
                            evidence: evidence.clone(),
                        },
                        actor,
                    )?);
                    if closes_node {
                        events.push(plan.apply(
                            PlanAction::SetStatus {
                                id: task.clone(),
                                status: PlanNodeStatus::Completed,
                                reason: Some(format!(
                                    "Job '{}' erfolgreich beendet",
                                    evidence.locator
                                )),
                            },
                            actor,
                        )?);
                    }
                }

                ReconcileStep::Invalidate { ids, condition } => {
                    events.push(plan.apply(
                        PlanAction::Invalidate {
                            ids: ids.clone(),
                            condition: condition.clone(),
                        },
                        actor,
                    )?);
                }

                ReconcileStep::InsertExplore { before, node } => {
                    events.push(plan.apply(
                        PlanAction::AddNode {
                            node: node.as_ref().clone(),
                        },
                        actor,
                    )?);
                    events.push(plan.apply(
                        PlanAction::AddDependency {
                            child: before.clone(),
                            parent: node.id.clone(),
                        },
                        actor,
                    )?);
                }

                ReconcileStep::MarkReady { ids } => {
                    for id in ids {
                        events.push(plan.apply(
                            PlanAction::SetStatus {
                                id: id.clone(),
                                status: PlanNodeStatus::Ready,
                                reason: Some("alle Abhängigkeiten abgeschlossen".to_owned()),
                            },
                            actor,
                        )?);
                    }
                }

                ReconcileStep::GoalStatus { .. } => {
                    // Bewusst nicht angewandt: nur ein menschlicher Akteur darf
                    // ein Goal für erreicht erklären. Ohne gebundenen
                    // Goal-Store hätte der Vorschlag aber keinen Adressaten.
                    if goal.is_none() {
                        return Err(PlanBridgeError::GoalUnbound);
                    }
                    deferred.push(step.clone());
                }

                ReconcileStep::AdmitJobs { .. }
                | ReconcileStep::ProposeExpand { .. }
                | ReconcileStep::ProposeCondense { .. }
                | ReconcileStep::AskModel { .. } => deferred.push(step.clone()),
            }
        }

        tracing::info!(
            applied = events.len(),
            deferred = deferred.len(),
            actor = actor,
            "Reconcile-Schritte angewandt"
        );
        Ok((events, deferred))
    }

    /// Wie [`Self::apply`], zusätzlich mit Telemetrie über tatsächlich
    /// angewandte Seiteneffekte (Knoten AW1-05).
    ///
    /// # Description
    /// Ruft [`Self::apply`] unverändert auf. Bei `Ok` emittiert diese
    /// Methode, falls `sink` `Some` ist,
    /// [`crate::metrics::record_apply_side_effects`] über die
    /// *eingegebene* Schrittfolge: `apply` bricht beim ersten Fehler ab, bei
    /// `Ok` wurde also jeder Schritt entweder angewandt oder als Vorschlag
    /// zurückgegeben, und ein Auszählen von `steps` ist damit exakt (siehe
    /// [`crate::metrics::record_apply_side_effects`]). Bei `Err` wird nichts
    /// emittiert — der Fehler wird unverändert weitergereicht, ohne dass
    /// diese Methode einen nicht abgeschlossenen Lauf als vollständig
    /// gemessen ausgibt.
    ///
    /// # Arguments
    /// - `steps` (`&[ReconcileStep]`): identisch zu [`Self::apply`].
    /// - `plan` (`&dyn PlanStore`): identisch zu [`Self::apply`].
    /// - `goal` (`Option<&dyn GoalStore>`): identisch zu [`Self::apply`].
    /// - `actor` (`&str`): identisch zu [`Self::apply`].
    /// - `sink` (`Option<&dyn TelemetrySink>`): Ziel der Messwerte; `None`
    ///   unterdrückt jede Emission.
    ///
    /// # Returns
    /// Dasselbe Ergebnis wie [`Self::apply`].
    ///
    /// # Errors
    /// Wie [`Self::apply`].
    ///
    /// # Concurrency
    /// Wie [`Self::apply`].
    pub fn apply_observed(
        steps: &[ReconcileStep],
        plan: &dyn PlanStore,
        goal: Option<&dyn GoalStore>,
        actor: &str,
        sink: Option<&dyn TelemetrySink>,
    ) -> Result<(Vec<PlanEvent>, Vec<ReconcileStep>), PlanBridgeError> {
        let result = Self::apply(steps, plan, goal, actor)?;
        if let Some(sink) = sink {
            crate::metrics::record_apply_side_effects(sink, steps);
        }
        Ok(result)
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Interne Hilfsfunktionen
// ──────────────────────────────────────────────────────────────────────────────

/// Sucht einen Knoten anhand seiner ID.
fn find_node<'a>(plan: &'a Plan, id: &TaskId) -> Option<&'a PlanNode> {
    plan.nodes.iter().find(|node| &node.id == id)
}

/// Ordnet ein Finding dem Knoten zu, den es belegt.
///
/// Erste Konvention: `TaskId == question_id`. Zweite Konvention: ein
/// `Research`- oder `Explore`-Knoten, dessen `objective` die `question_id`
/// als Teilzeichenkette enthält. Beide Suchen laufen in `plan.nodes`-Ordnung
/// und sind damit deterministisch.
fn match_finding_node<'a>(plan: &'a Plan, finding: &ResearchFinding) -> Option<&'a PlanNode> {
    let question_id = finding.question_id.as_str();
    if let Some(node) = plan
        .nodes
        .iter()
        .find(|node| node.id.as_str() == question_id)
    {
        return Some(node);
    }
    plan.nodes.iter().find(|node| {
        matches!(node.kind, PlanNodeKind::Research | PlanNodeKind::Explore)
            && node.objective.contains(question_id)
    })
}

/// Baut den Prompt für Regel (b).
fn contradiction_prompt(finding: &ResearchFinding, node: &PlanNode) -> String {
    let questions = finding
        .unresolved_questions
        .iter()
        .map(|question| format!("  - {}", question.trim()))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Finding '{}' wirft neue Fragen auf, die den bereits abgeschlossenen Knoten '{}' \
         betreffen.\n\nZiel des Knotens: {}\nSchlussfolgerung des Findings: {}\n\nOffene \
         Fragen:\n{}\n\nPrüfe, ob '{}' invalidiert werden muss. Antworte mit einer \
         Invalidierung samt Grund oder mit einer Begründung, warum der Knoten gültig bleibt.",
        finding.question_id,
        node.id,
        node.objective.trim(),
        finding.conclusion.trim(),
        questions,
        node.id
    )
}

/// Erzeugt den `Explore`-Knoten, der einem Zielknoten vorgeschaltet wird.
fn explore_node_for(target: &PlanNode, id: TaskId, now: OffsetDateTime) -> PlanNode {
    // Die Exploration liest genau das, was der Zielknoten schreiben wird, und
    // schreibt selbst nichts — sie kann deshalb mit keinem anderen Knoten in
    // einen WriteScope-Konflikt geraten.
    let stale_scope: Vec<InvalidationCondition> = target
        .write_scope
        .first()
        .map(|scope| {
            vec![InvalidationCondition::ExplorationStale {
                scope: PathOrSymbol::new(scope.as_str()),
            }]
        })
        .unwrap_or_default();

    PlanNode {
        id,
        kind: PlanNodeKind::Explore,
        // Bewusst ohne Wave: die Welle ergibt sich topologisch aus der neuen
        // Kante, nicht aus einer geerbten Nummer.
        wave: None,
        objective: format!("Exploration für {}: {}", target.id, target.objective.trim()),
        dependencies: Vec::new(),
        input_contracts: Vec::new(),
        output_contracts: Vec::new(),
        read_scope: target.write_scope.clone(),
        write_scope: Vec::new(),
        forbidden_scope: target.forbidden_scope.clone(),
        acceptance_criteria: Vec::new(),
        invalidation_conditions: stale_scope,
        status: PlanNodeStatus::Draft,
        evidence: Vec::new(),
        assignment: None,
        parent: target.parent.clone(),
        created_at: now,
        updated_at: now,
    }
}

/// Übersetzt beobachtete Job-Zustände in Schritte (Regel g).
///
/// # Returns
/// Die Schritte und die Menge der berührten Knoten-IDs (als `&str`), damit
/// Regel (f) diese Knoten überspringt.
fn job_state_steps<'a>(
    plan: &'a Plan,
    job_states: &[(String, JobState)],
    now: OffsetDateTime,
) -> (Vec<ReconcileStep>, HashSet<&'a str>) {
    let mut steps = Vec::new();
    let mut touched: HashSet<&str> = HashSet::new();

    for (work_id, state) in job_states {
        let Some(node) = plan.nodes.iter().find(|node| {
            node.assignment
                .as_ref()
                .and_then(|assignment| assignment.job.as_deref())
                == Some(work_id.as_str())
        }) else {
            tracing::debug!(work_id = work_id, "Job ohne zugeordneten Plan-Knoten");
            continue;
        };

        match state {
            JobState::Completed => {
                touched.insert(node.id.as_str());
                steps.push(ReconcileStep::AttachEvidence {
                    task: node.id.clone(),
                    evidence: EvidenceRef {
                        kind: EvidenceKind::Job,
                        locator: work_id.clone(),
                        attached_at: now,
                        actor: JOB_ACTOR.to_owned(),
                        // `job_states` liefert nur die WorkId (Lokator); der
                        // Job-Output selbst wird hier nicht gelesen.
                        digest: None,
                    },
                });
            }
            JobState::Failed => {
                touched.insert(node.id.as_str());
                steps.push(ReconcileStep::Invalidate {
                    ids: vec![node.id.clone()],
                    condition: InvalidationCondition::ManualInvalidate,
                });
            }
            // Pending/Ready/Running/Blocked/Cancelled sind keine Ergebnisse:
            // ein laufender Job ändert am Plan nichts, ein abgebrochener wird
            // vom Abbrechenden bewusst nachgezogen.
            _ => {}
        }
    }

    (steps, touched)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::{
        EVIDENCE_ATTACHED_TOTAL, EXPLORE_INSERTED_TOTAL, GOAL_AGE_DAYS, GOAL_COVERAGE,
        INVALIDATIONS_TOTAL, PROPOSALS_PENDING, RECONCILE_STEPS_TOTAL,
    };
    use crate::testing::{
        InMemoryGoalStore, RecordingSink, coding_node, covered_goal_fixture, exploration_config,
        node_with, plan_with, research_node, sample_finding,
    };
    use harw_observe::MetricValue;
    use harw_plan::goal::Invariant;
    use harw_plan::types::{Assignment, VerificationStep};
    use harw_plan::{InMemoryPlanStore, PlanId};

    fn now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + time::Duration::days(20_000)
    }

    fn permissive_config() -> PlanToolConfig {
        PlanToolConfig::enabled_defaults()
    }

    fn input<'a>(
        plan: &'a Plan,
        findings: &'a [ResearchFinding],
        jobs: &'a [(String, JobState)],
        config: &'a PlanToolConfig,
        goal: Option<&'a Goal>,
    ) -> ReconcileInput<'a> {
        ReconcileInput {
            goal,
            plan,
            new_findings: findings,
            job_states: jobs,
            config,
            now: now(),
        }
    }

    #[test]
    fn test_finding_becomes_attach_evidence_on_the_matching_node() {
        let plan = plan_with(vec![research_node("q-1", PlanNodeStatus::InProgress)]);
        let findings = vec![sample_finding("q-1", &[])];
        let config = permissive_config();

        let steps = PlanController::reconcile(input(&plan, &findings, &[], &config, None));

        match steps.first() {
            Some(ReconcileStep::AttachEvidence { task, evidence }) => {
                assert_eq!(task, &TaskId::new("q-1"));
                assert_eq!(evidence.kind, EvidenceKind::Finding);
                assert_eq!(evidence.locator, "p-test/research/q-1.md");
            }
            other => panic!("erwartet AttachEvidence, bekommen: {other:?}"),
        }
    }

    #[test]
    fn test_finding_matches_a_research_node_by_objective() {
        let mut node = research_node("r-1", PlanNodeStatus::InProgress);
        node.objective = "Beantworte q-42 für die Bridge".to_owned();
        let plan = plan_with(vec![node]);
        let findings = vec![sample_finding("q-42", &[])];
        let config = permissive_config();

        let steps = PlanController::reconcile(input(&plan, &findings, &[], &config, None));

        assert!(matches!(
            steps.first(),
            Some(ReconcileStep::AttachEvidence { task, .. }) if task == &TaskId::new("r-1")
        ));
    }

    #[test]
    fn test_finding_with_open_questions_on_a_completed_node_asks_the_model() {
        let plan = plan_with(vec![research_node("q-1", PlanNodeStatus::Completed)]);
        let findings = vec![sample_finding("q-1", &["Gilt das auch für Edition 2024?"])];
        let config = permissive_config();

        let steps = PlanController::reconcile(input(&plan, &findings, &[], &config, None));

        let prompt = steps.iter().find_map(|step| match step {
            ReconcileStep::AskModel { prompt } => Some(prompt.as_str()),
            _ => None,
        });
        match prompt {
            Some(prompt) => {
                assert!(prompt.contains("q-1"));
                assert!(prompt.contains("invalidiert"));
                assert!(prompt.contains("Edition 2024"));
            }
            None => panic!("erwartet AskModel, bekommen: {steps:?}"),
        }
    }

    #[test]
    fn test_finding_with_open_questions_on_an_open_node_asks_nothing() {
        let plan = plan_with(vec![research_node("q-1", PlanNodeStatus::InProgress)]);
        let findings = vec![sample_finding("q-1", &["noch offen"])];
        let config = permissive_config();

        let steps = PlanController::reconcile(input(&plan, &findings, &[], &config, None));

        assert!(
            !steps
                .iter()
                .any(|step| matches!(step, ReconcileStep::AskModel { .. })),
            "unerwarteter AskModel: {steps:?}"
        );
    }

    #[test]
    fn test_coding_node_without_exploration_gets_an_explore_inserted() {
        let plan = plan_with(vec![coding_node("t-1", PlanNodeStatus::Draft)]);
        let config = exploration_config();

        let steps = PlanController::reconcile(input(&plan, &[], &[], &config, None));

        match steps
            .iter()
            .find(|step| matches!(step, ReconcileStep::InsertExplore { .. }))
        {
            Some(ReconcileStep::InsertExplore { before, node }) => {
                assert_eq!(before, &TaskId::new("t-1"));
                assert_eq!(node.id, TaskId::new("t-1-explore"));
                assert_eq!(node.kind, PlanNodeKind::Explore);
                assert!(node.objective.starts_with("Exploration für t-1:"));
                assert_eq!(node.read_scope, plan.nodes[0].write_scope);
                assert!(node.write_scope.is_empty());
            }
            other => panic!("erwartet InsertExplore, bekommen: {other:?}"),
        }
    }

    #[test]
    fn test_node_missing_exploration_is_not_admitted_as_a_job() {
        let plan = plan_with(vec![coding_node("t-1", PlanNodeStatus::Draft)]);
        let config = exploration_config();

        let steps = PlanController::reconcile(input(&plan, &[], &[], &config, None));

        assert!(
            !steps
                .iter()
                .any(|step| matches!(step, ReconcileStep::AdmitJobs { .. })),
            "Knoten ohne Exploration wurde admittiert: {steps:?}"
        );
    }

    #[test]
    fn test_existing_explore_node_is_not_duplicated() {
        let mut explore = research_node("t-1-explore", PlanNodeStatus::Draft);
        explore.kind = PlanNodeKind::Explore;
        let plan = plan_with(vec![coding_node("t-1", PlanNodeStatus::Draft), explore]);
        let config = exploration_config();

        let steps = PlanController::reconcile(input(&plan, &[], &[], &config, None));

        assert!(
            !steps
                .iter()
                .any(|step| matches!(step, ReconcileStep::InsertExplore { .. })),
            "Exploration doppelt vorgeschlagen: {steps:?}"
        );
    }

    #[test]
    fn test_three_research_nodes_with_overlapping_read_scope_propose_condense() {
        let nodes = ["r-1", "r-2", "r-3"]
            .into_iter()
            .map(|id| {
                let mut node = research_node(id, PlanNodeStatus::Completed);
                node.read_scope = vec![PathOrSymbol::new("src/shared.rs")];
                node
            })
            .collect();
        let plan = plan_with(nodes);
        let config = permissive_config();

        let steps = PlanController::reconcile(input(&plan, &[], &[], &config, None));

        match steps
            .iter()
            .find(|step| matches!(step, ReconcileStep::ProposeCondense { .. }))
        {
            Some(ReconcileStep::ProposeCondense { group, reason }) => {
                assert_eq!(
                    group,
                    &vec![TaskId::new("r-1"), TaskId::new("r-2"), TaskId::new("r-3")]
                );
                assert!(reason.contains("verdichte"));
            }
            other => panic!("erwartet ProposeCondense, bekommen: {other:?}"),
        }
    }

    #[test]
    fn test_two_overlapping_research_nodes_are_below_the_condense_threshold() {
        let nodes = ["r-1", "r-2"]
            .into_iter()
            .map(|id| {
                let mut node = research_node(id, PlanNodeStatus::Completed);
                node.read_scope = vec![PathOrSymbol::new("src/shared.rs")];
                node
            })
            .collect();
        let plan = plan_with(nodes);
        let config = permissive_config();

        let steps = PlanController::reconcile(input(&plan, &[], &[], &config, None));

        assert!(
            !steps
                .iter()
                .any(|step| matches!(step, ReconcileStep::ProposeCondense { .. })),
            "unter der Schwelle verdichtet: {steps:?}"
        );
    }

    #[test]
    fn test_ready_coding_node_is_proposed_for_job_admission() {
        let plan = plan_with(vec![coding_node("t-1", PlanNodeStatus::Ready)]);
        let config = permissive_config();

        let steps = PlanController::reconcile(input(&plan, &[], &[], &config, None));

        match steps
            .iter()
            .find(|step| matches!(step, ReconcileStep::AdmitJobs { .. }))
        {
            Some(ReconcileStep::AdmitJobs { ids }) => {
                assert_eq!(ids, &vec![TaskId::new("t-1")]);
            }
            other => panic!("erwartet AdmitJobs, bekommen: {other:?}"),
        }
    }

    #[test]
    fn test_ready_research_node_is_marked_ready_not_admitted() {
        let plan = plan_with(vec![research_node("r-1", PlanNodeStatus::Draft)]);
        let config = permissive_config();

        let steps = PlanController::reconcile(input(&plan, &[], &[], &config, None));

        assert!(
            steps
                .iter()
                .any(|step| matches!(step, ReconcileStep::MarkReady { ids } if ids == &vec![TaskId::new("r-1")])),
            "erwartet MarkReady: {steps:?}"
        );
        assert!(
            !steps
                .iter()
                .any(|step| matches!(step, ReconcileStep::AdmitJobs { .. })),
            "Research-Knoten wurde als Job admittiert: {steps:?}"
        );
    }

    #[test]
    fn test_coarse_draft_node_is_proposed_for_expansion() {
        let mut node = coding_node("t-big", PlanNodeStatus::Draft);
        node.write_scope = (0..COARSE_WRITE_SCOPE)
            .map(|index| PathOrSymbol::new(format!("src/mod_{index}.rs")))
            .collect();
        let plan = plan_with(vec![node]);
        let config = permissive_config();

        let steps = PlanController::reconcile(input(&plan, &[], &[], &config, None));

        assert!(
            steps.iter().any(|step| matches!(
                step,
                ReconcileStep::ProposeExpand { parent, .. } if parent == &TaskId::new("t-big")
            )),
            "erwartet ProposeExpand: {steps:?}"
        );
    }

    #[test]
    fn test_composite_with_only_completed_children_is_marked_ready() {
        let mut composite = node_with("c-1", PlanNodeKind::Composite, PlanNodeStatus::Draft);
        composite.write_scope = vec![PathOrSymbol::new("src/c.rs")];
        let mut child = coding_node("c-1-a", PlanNodeStatus::Completed);
        child.parent = Some(TaskId::new("c-1"));
        let plan = plan_with(vec![composite, child]);
        let config = permissive_config();

        let steps = PlanController::reconcile(input(&plan, &[], &[], &config, None));

        assert!(
            steps.iter().any(|step| matches!(
                step,
                ReconcileStep::MarkReady { ids } if ids.contains(&TaskId::new("c-1"))
            )),
            "erwartet MarkReady für Composite: {steps:?}"
        );
    }

    #[test]
    fn test_completed_job_produces_job_evidence() {
        let mut node = coding_node("t-1", PlanNodeStatus::InProgress);
        node.assignment = Some(Assignment {
            worker: "worker-1".to_owned(),
            attempt: 0,
            job: Some("work-abc".to_owned()),
        });
        let plan = plan_with(vec![node]);
        let jobs = vec![("work-abc".to_owned(), JobState::Completed)];
        let config = permissive_config();

        let steps = PlanController::reconcile(input(&plan, &[], &jobs, &config, None));

        match steps
            .iter()
            .find(|step| matches!(step, ReconcileStep::AttachEvidence { .. }))
        {
            Some(ReconcileStep::AttachEvidence { task, evidence }) => {
                assert_eq!(task, &TaskId::new("t-1"));
                assert_eq!(evidence.kind, EvidenceKind::Job);
                assert_eq!(evidence.locator, "work-abc");
            }
            other => panic!("erwartet Job-Evidenz, bekommen: {other:?}"),
        }
    }

    #[test]
    fn test_failed_job_invalidates_the_node() {
        let mut node = coding_node("t-1", PlanNodeStatus::InProgress);
        node.assignment = Some(Assignment {
            worker: "worker-1".to_owned(),
            attempt: 0,
            job: Some("work-fail".to_owned()),
        });
        let plan = plan_with(vec![node]);
        let jobs = vec![("work-fail".to_owned(), JobState::Failed)];
        let config = permissive_config();

        let steps = PlanController::reconcile(input(&plan, &[], &jobs, &config, None));

        match steps
            .iter()
            .find(|step| matches!(step, ReconcileStep::Invalidate { .. }))
        {
            Some(ReconcileStep::Invalidate { ids, condition }) => {
                assert_eq!(ids, &vec![TaskId::new("t-1")]);
                assert!(matches!(condition, InvalidationCondition::ManualInvalidate));
            }
            other => panic!("erwartet Invalidate, bekommen: {other:?}"),
        }
    }

    #[test]
    fn test_running_job_produces_no_step() {
        let mut node = coding_node("t-1", PlanNodeStatus::InProgress);
        node.assignment = Some(Assignment {
            worker: "worker-1".to_owned(),
            attempt: 0,
            job: Some("work-run".to_owned()),
        });
        let plan = plan_with(vec![node]);
        let jobs = vec![("work-run".to_owned(), JobState::Running)];
        let config = permissive_config();

        let steps = PlanController::reconcile(input(&plan, &[], &jobs, &config, None));
        assert!(steps.is_empty(), "unerwartete Schritte: {steps:?}");
    }

    #[test]
    fn test_reconcile_is_deterministic_across_two_runs() {
        let nodes = vec![
            {
                let mut node = research_node("r-3", PlanNodeStatus::Completed);
                node.read_scope = vec![PathOrSymbol::new("src/shared.rs")];
                node
            },
            {
                let mut node = research_node("r-1", PlanNodeStatus::Completed);
                node.read_scope = vec![PathOrSymbol::new("src/shared.rs")];
                node
            },
            {
                let mut node = research_node("r-2", PlanNodeStatus::Completed);
                node.read_scope = vec![PathOrSymbol::new("src/shared.rs")];
                node
            },
            coding_node("t-1", PlanNodeStatus::Ready),
            {
                let mut node = coding_node("t-big", PlanNodeStatus::Draft);
                node.write_scope = (0..COARSE_WRITE_SCOPE)
                    .map(|index| PathOrSymbol::new(format!("src/big_{index}.rs")))
                    .collect();
                node
            },
        ];
        let plan = plan_with(nodes);
        let findings = vec![sample_finding("r-1", &["offen"])];
        let config = permissive_config();

        let first = PlanController::reconcile(input(&plan, &findings, &[], &config, None));
        let second = PlanController::reconcile(input(&plan, &findings, &[], &config, None));

        assert_eq!(first, second, "reconcile ist nicht deterministisch");
        assert!(!first.is_empty(), "der Test prüft eine leere Folge");
    }

    #[test]
    fn test_goal_with_all_criteria_met_is_only_proposed() {
        let (plan, goal) = covered_goal_fixture();
        let config = permissive_config();

        let steps = PlanController::reconcile(input(&plan, &[], &[], &config, Some(&goal)));

        assert!(
            steps.iter().any(|step| matches!(
                step,
                ReconcileStep::GoalStatus { status, .. } if *status == GoalStatus::Achieved
            )),
            "erwartet GoalStatus-Vorschlag: {steps:?}"
        );
    }

    #[test]
    fn test_goal_without_criteria_is_never_declared_achieved() {
        let (plan, mut goal) = covered_goal_fixture();
        goal.acceptance_criteria.clear();
        let config = permissive_config();

        let steps = PlanController::reconcile(input(&plan, &[], &[], &config, Some(&goal)));

        assert!(
            !steps
                .iter()
                .any(|step| matches!(step, ReconcileStep::GoalStatus { .. })),
            "Goal ohne Kriterien wurde für erreicht erklärt: {steps:?}"
        );
    }

    #[test]
    fn test_goal_with_violated_invariant_is_not_proposed() {
        let (plan, mut goal) = covered_goal_fixture();
        goal.invariants.push(Invariant {
            id: "inv-unbelegt".to_owned(),
            statement: "niemals belegt".to_owned(),
            verification: vec![VerificationStep::Artifact {
                path: "gibt-es-nicht".to_owned(),
            }],
        });
        let config = permissive_config();

        let steps = PlanController::reconcile(input(&plan, &[], &[], &config, Some(&goal)));

        assert!(
            !steps
                .iter()
                .any(|step| matches!(step, ReconcileStep::GoalStatus { .. })),
            "verletzte Invariante ignoriert: {steps:?}"
        );
    }

    #[test]
    fn test_apply_does_not_apply_goal_status_but_returns_it() {
        let store = InMemoryPlanStore::new();
        if let Err(error) = store.apply(
            PlanAction::Create {
                plan_id: PlanId::new("p-apply"),
                goal: "Ziel".to_owned(),
            },
            "test",
        ) {
            panic!("Plan anlegen: {error}");
        }
        let goal_store = InMemoryGoalStore::new();
        let steps = vec![ReconcileStep::GoalStatus {
            status: GoalStatus::Achieved,
            reason: "alles belegt".to_owned(),
        }];

        let bound: &dyn GoalStore = &goal_store;
        let (events, deferred) = match PlanController::apply(&steps, &store, Some(bound), "test") {
            Ok(result) => result,
            Err(error) => panic!("apply schlug fehl: {error}"),
        };

        assert!(events.is_empty(), "GoalStatus wurde angewandt: {events:?}");
        assert_eq!(deferred, steps);
        assert_eq!(goal_store.revision(), 0, "Goal-Store wurde mutiert");
    }

    #[test]
    fn test_apply_rejects_goal_status_without_a_goal_store() {
        let store = InMemoryPlanStore::new();
        let steps = vec![ReconcileStep::GoalStatus {
            status: GoalStatus::Achieved,
            reason: "alles belegt".to_owned(),
        }];

        match PlanController::apply(&steps, &store, None, "test") {
            Err(PlanBridgeError::GoalUnbound) => {}
            other => panic!("erwartet GoalUnbound, bekommen: {other:?}"),
        }
    }

    #[test]
    fn test_apply_defers_proposals_and_admissions() {
        let store = InMemoryPlanStore::new();
        let steps = vec![
            ReconcileStep::AdmitJobs {
                ids: vec![TaskId::new("t-1")],
            },
            ReconcileStep::ProposeExpand {
                parent: TaskId::new("t-2"),
                reason: "zu grob".to_owned(),
            },
            ReconcileStep::ProposeCondense {
                group: vec![TaskId::new("r-1")],
                reason: "verdichten".to_owned(),
            },
            ReconcileStep::AskModel {
                prompt: "prüfe".to_owned(),
            },
        ];

        let (events, deferred) = match PlanController::apply(&steps, &store, None, "test") {
            Ok(result) => result,
            Err(error) => panic!("apply schlug fehl: {error}"),
        };

        assert!(events.is_empty());
        assert_eq!(deferred, steps);
    }

    #[test]
    fn test_apply_attaches_evidence_and_completes_on_job_evidence() {
        let store = InMemoryPlanStore::new();
        if let Err(error) = store.apply(
            PlanAction::Create {
                plan_id: PlanId::new("p-apply"),
                goal: "Ziel".to_owned(),
            },
            "test",
        ) {
            panic!("Plan anlegen: {error}");
        }
        let mut node = coding_node("t-1", PlanNodeStatus::Draft);
        node.assignment = None;
        if let Err(error) = store.apply(PlanAction::AddNode { node }, "test") {
            panic!("Knoten anlegen: {error}");
        }
        for status in [PlanNodeStatus::Ready, PlanNodeStatus::InProgress] {
            if let Err(error) = store.apply(
                PlanAction::SetStatus {
                    id: TaskId::new("t-1"),
                    status,
                    reason: None,
                },
                "test",
            ) {
                panic!("Status {status:?} setzen: {error}");
            }
        }

        let steps = vec![ReconcileStep::AttachEvidence {
            task: TaskId::new("t-1"),
            evidence: EvidenceRef {
                kind: EvidenceKind::Job,
                locator: "work-abc".to_owned(),
                attached_at: now(),
                actor: JOB_ACTOR.to_owned(),
                digest: None,
            },
        }];

        let (events, deferred) = match PlanController::apply(&steps, &store, None, "test") {
            Ok(result) => result,
            Err(error) => panic!("apply schlug fehl: {error}"),
        };
        assert_eq!(events.len(), 2, "erwartet AttachEvidence + SetStatus");
        assert!(deferred.is_empty());

        let plan = match store.current() {
            Ok(plan) => plan,
            Err(error) => panic!("current schlug fehl: {error}"),
        };
        assert_eq!(plan.nodes[0].status, PlanNodeStatus::Completed);
        assert_eq!(plan.nodes[0].evidence.len(), 1);
    }

    #[test]
    fn test_apply_inserts_the_explore_node_and_its_dependency_edge() {
        let store = InMemoryPlanStore::new();
        if let Err(error) = store.apply(
            PlanAction::Create {
                plan_id: PlanId::new("p-apply"),
                goal: "Ziel".to_owned(),
            },
            "test",
        ) {
            panic!("Plan anlegen: {error}");
        }
        let target = coding_node("t-1", PlanNodeStatus::Draft);
        if let Err(error) = store.apply(
            PlanAction::AddNode {
                node: target.clone(),
            },
            "test",
        ) {
            panic!("Knoten anlegen: {error}");
        }

        let steps = vec![ReconcileStep::InsertExplore {
            before: TaskId::new("t-1"),
            node: Box::new(explore_node_for(&target, TaskId::new("t-1-explore"), now())),
        }];

        let (events, _deferred) = match PlanController::apply(&steps, &store, None, "test") {
            Ok(result) => result,
            Err(error) => panic!("apply schlug fehl: {error}"),
        };
        assert_eq!(events.len(), 2, "erwartet AddNode + AddDependency");

        let plan = match store.current() {
            Ok(plan) => plan,
            Err(error) => panic!("current schlug fehl: {error}"),
        };
        let target_node = match plan.nodes.iter().find(|node| node.id == TaskId::new("t-1")) {
            Some(node) => node,
            None => panic!("Zielknoten verschwunden"),
        };
        assert_eq!(target_node.dependencies, vec![TaskId::new("t-1-explore")]);
    }

    #[test]
    fn test_reconcile_input_carries_no_hidden_clock() {
        // Zwei verschiedene `now`-Werte müssen sich in der Ausgabe
        // niederschlagen — sonst käme die Zeit von woanders her.
        let mut node = coding_node("t-1", PlanNodeStatus::InProgress);
        node.assignment = Some(Assignment {
            worker: "w".to_owned(),
            attempt: 0,
            job: Some("work-x".to_owned()),
        });
        let plan = plan_with(vec![node]);
        let jobs = vec![("work-x".to_owned(), JobState::Completed)];
        let config = permissive_config();

        let early = PlanController::reconcile(ReconcileInput {
            goal: None,
            plan: &plan,
            new_findings: &[],
            job_states: &jobs,
            config: &config,
            now: OffsetDateTime::UNIX_EPOCH,
        });
        let late = PlanController::reconcile(ReconcileInput {
            goal: None,
            plan: &plan,
            new_findings: &[],
            job_states: &jobs,
            config: &config,
            now: now(),
        });

        assert_ne!(early, late);
    }

    #[test]
    fn test_reconcile_observed_emits_reconcile_steps_total_with_step_labels() {
        let plan = plan_with(vec![research_node("r-1", PlanNodeStatus::Draft)]);
        let config = permissive_config();
        let sink = RecordingSink::new();

        let steps = PlanController::reconcile_observed(
            input(&plan, &[], &[], &config, None),
            Some(&sink),
        );

        assert!(!steps.is_empty(), "Fixture sollte mindestens einen Schritt erzeugen");
        let recorded = sink.values_for(RECONCILE_STEPS_TOTAL.name);
        assert_eq!(recorded.len(), steps.len());
        for record in &recorded {
            assert!(matches!(record.value, MetricValue::Count(1)));
            assert_eq!(record.labels.len(), 1);
            assert_eq!(record.labels[0].0.as_str(), "step");
        }
    }

    #[test]
    fn test_reconcile_observed_emits_proposals_pending() {
        let plan = plan_with(vec![coding_node("t-1", PlanNodeStatus::Draft)]);
        let config = exploration_config();
        let sink = RecordingSink::new();

        PlanController::reconcile_observed(input(&plan, &[], &[], &config, None), Some(&sink));

        assert_eq!(sink.values_for(PROPOSALS_PENDING.name).len(), 1);
    }

    #[test]
    fn test_reconcile_observed_emits_goal_age_and_coverage_only_when_goal_is_bound() {
        let (plan, goal) = covered_goal_fixture();
        let config = permissive_config();
        let sink = RecordingSink::new();

        PlanController::reconcile_observed(
            input(&plan, &[], &[], &config, Some(&goal)),
            Some(&sink),
        );

        let age = sink.values_for(GOAL_AGE_DAYS.name);
        let coverage = sink.values_for(GOAL_COVERAGE.name);
        assert_eq!(age.len(), 1);
        assert_eq!(coverage.len(), 1);
        assert_eq!(coverage[0].value, MetricValue::Gauge(1.0));

        let sink_without_goal = RecordingSink::new();
        PlanController::reconcile_observed(
            input(&plan, &[], &[], &config, None),
            Some(&sink_without_goal),
        );
        assert!(sink_without_goal.values_for(GOAL_AGE_DAYS.name).is_empty());
        assert!(sink_without_goal.values_for(GOAL_COVERAGE.name).is_empty());
    }

    #[test]
    fn test_reconcile_observed_without_a_sink_emits_nothing_and_still_returns_steps() {
        let plan = plan_with(vec![research_node("r-1", PlanNodeStatus::Draft)]);
        let config = permissive_config();

        let steps = PlanController::reconcile_observed(input(&plan, &[], &[], &config, None), None);

        assert!(!steps.is_empty());
    }

    #[test]
    fn test_apply_observed_emits_evidence_attached_total_on_job_evidence() {
        let store = InMemoryPlanStore::new();
        if let Err(error) = store.apply(
            PlanAction::Create {
                plan_id: PlanId::new("p-apply"),
                goal: "Ziel".to_owned(),
            },
            "test",
        ) {
            panic!("Plan anlegen: {error}");
        }
        let mut node = coding_node("t-1", PlanNodeStatus::Draft);
        node.assignment = None;
        if let Err(error) = store.apply(PlanAction::AddNode { node }, "test") {
            panic!("Knoten anlegen: {error}");
        }
        for status in [PlanNodeStatus::Ready, PlanNodeStatus::InProgress] {
            if let Err(error) = store.apply(
                PlanAction::SetStatus {
                    id: TaskId::new("t-1"),
                    status,
                    reason: None,
                },
                "test",
            ) {
                panic!("Status {status:?} setzen: {error}");
            }
        }
        let steps = vec![ReconcileStep::AttachEvidence {
            task: TaskId::new("t-1"),
            evidence: EvidenceRef {
                kind: EvidenceKind::Job,
                locator: "work-abc".to_owned(),
                attached_at: now(),
                actor: JOB_ACTOR.to_owned(),
                digest: None,
            },
        }];
        let sink = RecordingSink::new();

        if let Err(error) = PlanController::apply_observed(&steps, &store, None, "test", Some(&sink))
        {
            panic!("apply_observed schlug fehl: {error}");
        }

        assert_eq!(sink.values_for(EVIDENCE_ATTACHED_TOTAL.name).len(), 1);
        assert!(sink.values_for(INVALIDATIONS_TOTAL.name).is_empty());
        assert!(sink.values_for(EXPLORE_INSERTED_TOTAL.name).is_empty());
    }

    #[test]
    fn test_apply_observed_emits_invalidations_total_on_a_failed_job() {
        let store = InMemoryPlanStore::new();
        if let Err(error) = store.apply(
            PlanAction::Create {
                plan_id: PlanId::new("p-apply"),
                goal: "Ziel".to_owned(),
            },
            "test",
        ) {
            panic!("Plan anlegen: {error}");
        }
        if let Err(error) = store.apply(
            PlanAction::AddNode {
                node: coding_node("t-1", PlanNodeStatus::Draft),
            },
            "test",
        ) {
            panic!("Knoten anlegen: {error}");
        }
        let steps = vec![ReconcileStep::Invalidate {
            ids: vec![TaskId::new("t-1")],
            condition: InvalidationCondition::ManualInvalidate,
        }];
        let sink = RecordingSink::new();

        if let Err(error) = PlanController::apply_observed(&steps, &store, None, "test", Some(&sink))
        {
            panic!("apply_observed schlug fehl: {error}");
        }

        assert_eq!(sink.values_for(INVALIDATIONS_TOTAL.name).len(), 1);
    }

    #[test]
    fn test_apply_observed_without_a_sink_still_applies_but_emits_nothing() {
        let store = InMemoryPlanStore::new();
        if let Err(error) = store.apply(
            PlanAction::Create {
                plan_id: PlanId::new("p-apply"),
                goal: "Ziel".to_owned(),
            },
            "test",
        ) {
            panic!("Plan anlegen: {error}");
        }
        if let Err(error) = store.apply(
            PlanAction::AddNode {
                node: coding_node("t-1", PlanNodeStatus::Draft),
            },
            "test",
        ) {
            panic!("Knoten anlegen: {error}");
        }
        let steps = vec![ReconcileStep::Invalidate {
            ids: vec![TaskId::new("t-1")],
            condition: InvalidationCondition::ManualInvalidate,
        }];

        let (events, deferred) = match PlanController::apply_observed(&steps, &store, None, "test", None)
        {
            Ok(result) => result,
            Err(error) => panic!("apply_observed schlug fehl: {error}"),
        };
        assert_eq!(events.len(), 1);
        assert!(deferred.is_empty());
    }
}
