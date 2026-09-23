//! Zentrale Mutationslogik für `harw-plan`.
//!
//! Verantwortungsbereich: Dieses Modul ist die **einzige** Stelle, an der eine
//! [`PlanAction`] den Zustand eines [`Plan`] verändert. `InMemoryPlanStore` und
//! `FilePlanStore` hielten bis zu diesem Stand zwei unabhängige Kopien
//! derselben Logik; beide delegieren jetzt an [`apply_mutation`]. Jede neue
//! Aktionsvariante muss damit nur noch an einer Stelle gepflegt werden.
//!
//! Was dieses Modul **nicht** tut:
//! - Es validiert nicht. Der Aufrufer ruft vorher `crate::validate::validate`
//!   auf; hier wird davon ausgegangen, dass die Aktion zulässig ist.
//! - Es verwaltet keine Revisionen. Die Revisionsnummer gehört dem Store
//!   (`plan.revision` wird unmittelbar nach diesem Aufruf gesetzt).
//! - Es schreibt nichts auf die Platte und hält keine Locks.
//!
//! Runtime-eigene Felder (`created_at`, `updated_at`, `attached_at`) werden
//! hier gesetzt und aus der Payload verworfen (Design-Doc §7). Der `actor`
//! stammt ausschließlich aus dem Aufrufkontext, niemals aus der Aktion.
//!
//! Sichtbarkeit: crate-privat (`mod mutation;` in `lib.rs`), da die einzige
//! zulässige öffentliche Mutationsschnittstelle `PlanStore::apply` ist.
//!
//! # Concurrency
//! Rein funktional auf einem exklusiv geliehenen `&mut Plan` — keine Locks,
//! keine Threads, kein globaler Zustand. Die Synchronisation liegt beim Store.
//!
//! # Errors
//! Keine. [`apply_mutation`] kann nicht fehlschlagen: unbekannte Knoten-IDs
//! sind ein No-op (die Existenzprüfung ist Aufgabe der Validierung).
//!
//! # Beispiel
//! ```text
//! validate(&plan, &action)?;                      // Aufrufer
//! apply_mutation(&mut plan, &action, actor, now); // dieses Modul
//! plan.updated_at = now;                          // Store
//! plan.revision   = revision;                     // Store
//! ```

use std::collections::HashSet;

use time::OffsetDateTime;
use tracing::trace;

use crate::actions::{NodePatch, PlanAction};
use crate::ids::TaskId;
use crate::types::{
    Assignment, EvidenceKind, EvidenceRef, Plan, PlanNode, PlanNodeKind, PlanNodeStatus,
};

/// Versuchszähler des ersten Ausführungsversuchs.
///
/// `Assignment::attempt` wird hier 1-basiert geführt: der erste Versuch trägt
/// `1`, jede Wiedereröffnung erhöht um eins. Damit ist `attempt` direkt die
/// Anzahl der Anläufe und nicht die Anzahl der Retries.
const FIRST_ATTEMPT: u32 = 1;

/// Locator-Präfix der Evidenz, in der eine `Condense`-Zusammenfassung abgelegt
/// wird. Der vollständige Locator lautet `condense:<summary>`.
const CONDENSE_LOCATOR_PREFIX: &str = "condense";

/// Wendet eine bereits validierte Aktion auf den Plan an.
///
/// # Description
/// Einzige Mutationsstelle des Crates. Runtime-eigene Felder (Timestamps,
/// Zuweisungen) werden hier gesetzt bzw. aus der Payload verworfen; Revision
/// und `updated_at` des Plans setzt der aufrufende Store unmittelbar danach.
///
/// Aktionen, die auf einen nicht vorhandenen Knoten verweisen, sind ein
/// stilles No-op — die Existenzprüfung gehört in `crate::validate`.
///
/// # Arguments
/// - `plan` (`&mut Plan`): der zu mutierende Plan; exklusiv geliehen.
/// - `action` (`&PlanAction`): die anzuwendende, bereits validierte Aktion.
/// - `actor` (`&str`): Akteur aus dem Aufrufkontext (Worker- oder
///   Menschen-Kennung). Wird für `SetStatus`, `Reopen` und `Condense`
///   verwendet; niemals aus der Payload übernommen.
/// - `now` (`OffsetDateTime`): einheitlicher Zeitstempel dieser Anwendung.
///
/// # Returns
/// Nichts — die Mutation erfolgt in-place.
///
/// # Errors
/// Keine; die Funktion kann nicht fehlschlagen.
///
/// # Panics
/// Keine.
///
/// # Concurrency
/// Erfordert exklusiven Zugriff auf `plan`. Der Aufrufer hält bereits den
/// Schreib-Lock des Stores.
pub(crate) fn apply_mutation(
    plan: &mut Plan,
    action: &PlanAction,
    actor: &str,
    now: OffsetDateTime,
) {
    match action {
        // `Create` legt einen neuen Plan an und wird deshalb im Store
        // gesondert behandelt — hier gibt es keinen Vorzustand zu mutieren.
        PlanAction::Create { .. } => {}

        PlanAction::AddNode { node } => {
            plan.nodes.push(materialize_node(node, None, now));
        }

        PlanAction::UpdateNode { id, patch } => {
            if let Some(node) = find_node_mut(plan, id) {
                apply_patch(node, patch);
                node.updated_at = now;
            }
        }

        PlanAction::AddDependency { child, parent } => {
            if let Some(node) = find_node_mut(plan, child) {
                // Idempotent: eine bereits vorhandene Kante wird nicht dupliziert.
                if !node.dependencies.contains(parent) {
                    node.dependencies.push(parent.clone());
                }
                node.updated_at = now;
            }
        }

        PlanAction::SetStatus { id, status, .. } => {
            if let Some(node) = find_node_mut(plan, id) {
                node.status = *status;
                // Der Übergang nach InProgress ist der Moment, in dem ein
                // Knoten tatsächlich von einem Worker übernommen wird.
                if *status == PlanNodeStatus::InProgress {
                    assign_worker(node, actor);
                }
                node.updated_at = now;
            }
        }

        PlanAction::AttachEvidence { id, evidence } => {
            if let Some(node) = find_node_mut(plan, id) {
                push_evidence_unique(&mut node.evidence, evidence, now);
                node.updated_at = now;
            }
        }

        PlanAction::Invalidate { ids, condition } => {
            for id in ids {
                if let Some(node) = find_node_mut(plan, id) {
                    node.status = PlanNodeStatus::Invalidated;
                    node.invalidation_conditions.push(condition.clone());
                    node.updated_at = now;
                }
            }
        }

        // `reason` wird bewusst nicht in die Invalidierungsbedingungen des
        // Knotens geschrieben: Reopen erklärt keine neue Invalidierung, sondern
        // startet einen neuen Ausführungsversuch derselben Task-Identität. Der
        // Grund bleibt über das `PlanEvent` in der History nachvollziehbar.
        PlanAction::Reopen { id, .. } => {
            if let Some(node) = find_node_mut(plan, id) {
                node.status = PlanNodeStatus::Draft;
                begin_next_attempt(node, actor);
                node.updated_at = now;
                trace!(node_id = %id, "Knoten wiedereröffnet");
            }
        }

        PlanAction::Expand { parent, children } => {
            for child in children {
                plan.nodes.push(materialize_node(child, Some(parent), now));
            }
            if let Some(node) = find_node_mut(plan, parent) {
                node.kind = PlanNodeKind::Composite;
                node.updated_at = now;
            }
            trace!(parent = %parent, children = children.len(), "Knoten zerlegt");
        }

        PlanAction::Condense {
            superseded,
            replacement,
            summary,
        } => {
            let mut replacement_node = materialize_node(replacement, None, now);

            // Evidenz aller ersetzten Knoten übernehmen, dedupliziert nach
            // (kind, locator). Der ursprüngliche `attached_at`-Zeitstempel
            // bleibt erhalten: es ist derselbe Nachweis, nur an anderer Stelle.
            let mut seen: HashSet<(EvidenceKind, String)> = replacement_node
                .evidence
                .iter()
                .map(|existing| (existing.kind, existing.locator.clone()))
                .collect();
            for id in superseded {
                let Some(source) = plan.nodes.iter().find(|candidate| &candidate.id == id) else {
                    continue;
                };
                for evidence in &source.evidence {
                    if seen.insert((evidence.kind, evidence.locator.clone())) {
                        replacement_node.evidence.push(evidence.clone());
                    }
                }
            }

            // Die Zusammenfassung wird als eigener Nachweis geführt, nicht an
            // `objective` angehängt: `objective` beschreibt die Aufgabe, nicht
            // deren Ergebnis.
            let summary_locator = format!("{CONDENSE_LOCATOR_PREFIX}:{summary}");
            if seen.insert((EvidenceKind::Manual, summary_locator.clone())) {
                replacement_node.evidence.push(EvidenceRef {
                    kind: EvidenceKind::Manual,
                    locator: summary_locator,
                    attached_at: now,
                    actor: actor.to_owned(),
                    // `summary` ist der vollständige Nachweisinhalt (er steckt
                    // bereits im Locator) und liegt hier als String vor.
                    digest: Some(harw_types::ContentDigest::of(summary.as_bytes())),
                });
            }

            plan.nodes.push(replacement_node);

            for id in superseded {
                if let Some(node) = find_node_mut(plan, id) {
                    node.status = PlanNodeStatus::Superseded;
                    node.updated_at = now;
                }
            }
            trace!(
                replacement = %replacement.id,
                superseded = superseded.len(),
                "Knoten verdichtet"
            );
        }

        // `new_parent_revision` wird hier bewusst nicht auf `plan.revision`
        // geschrieben: die Revisionsnummer gehört dem Store, der sie direkt
        // nach diesem Aufruf setzt. Die Payload dient allein der monotonen
        // Prüfung in `validate`.
        PlanAction::Supersede { .. } => {
            plan.parent_revision = Some(plan.revision);
            for node in plan.nodes.iter_mut() {
                if !matches!(
                    node.status,
                    PlanNodeStatus::Completed
                        | PlanNodeStatus::Invalidated
                        | PlanNodeStatus::Superseded
                ) {
                    node.status = PlanNodeStatus::Superseded;
                    node.updated_at = now;
                }
            }
        }

        // F-126: die Bindung wird am Plan festgehalten. `goal_statement`
        // bleibt unverändert (freier Text, keine Referenz).
        PlanAction::BindGoal { goal_id } => {
            plan.goal_id = Some(goal_id.clone());
            trace!(goal_id = %goal_id, "Plan an Goal gebunden");
        }

        // Lesende Aktion ohne Zustandsänderung.
        PlanAction::Inspect => {}
    }
}

/// Sucht einen Knoten anhand seiner ID und liefert eine exklusive Referenz.
fn find_node_mut<'a>(plan: &'a mut Plan, id: &TaskId) -> Option<&'a mut PlanNode> {
    plan.nodes.iter_mut().find(|node| &node.id == id)
}

/// Erzeugt aus einer Aktions-Payload den einzufügenden Knoten.
///
/// Setzt die runtime-eigenen Zeitstempel und optional die Eltern-Beziehung.
/// `parent = None` bedeutet "die Payload entscheidet" — `parent` ist kein
/// runtime-eigenes Feld, sondern Teil der Plandaten.
fn materialize_node(node: &PlanNode, parent: Option<&TaskId>, now: OffsetDateTime) -> PlanNode {
    let mut materialized = node.clone();
    if let Some(parent) = parent {
        materialized.parent = Some(parent.clone());
    }
    materialized.created_at = now;
    materialized.updated_at = now;
    // F-013 §5.1 Punkt 3: ein Payload-Nachweis mit Zukunfts-Zeitstempel würde
    // dauerhaft als „frische Exploration“ gelten. Der Zeitstempel wird auf
    // `now` gekappt; ältere Zeitstempel bleiben erhalten (sie können nur
    // früher verfallen, nie später).
    for evidence in &mut materialized.evidence {
        if evidence.attached_at > now {
            evidence.attached_at = now;
        }
    }
    materialized
}

/// Überträgt alle angefassten Felder eines [`NodePatch`] auf den Knoten.
///
/// Das vollständige Destrukturieren von `patch` ist Absicht: ein neues Feld in
/// [`NodePatch`] erzeugt hier einen Compilefehler, statt still ignoriert zu
/// werden.
fn apply_patch(node: &mut PlanNode, patch: &NodePatch) {
    let NodePatch {
        objective,
        kind,
        wave,
        dependencies,
        read_scope,
        write_scope,
        forbidden_scope,
        input_contracts,
        output_contracts,
        acceptance_criteria,
        assignment,
    } = patch;

    if let Some(value) = objective {
        node.objective = value.clone();
    }
    if let Some(value) = kind {
        node.kind = *value;
    }
    // Doppel-Option: äußeres `Some` = angefasst, inneres `None` = löschen.
    if let Some(value) = wave {
        node.wave = *value;
    }
    if let Some(value) = dependencies {
        node.dependencies = value.clone();
    }
    if let Some(value) = read_scope {
        node.read_scope = value.clone();
    }
    if let Some(value) = write_scope {
        node.write_scope = value.clone();
    }
    if let Some(value) = forbidden_scope {
        node.forbidden_scope = value.clone();
    }
    if let Some(value) = input_contracts {
        node.input_contracts = value.clone();
    }
    if let Some(value) = output_contracts {
        node.output_contracts = value.clone();
    }
    if let Some(value) = acceptance_criteria {
        node.acceptance_criteria = value.clone();
    }
    // Doppel-Option: äußeres `Some` = angefasst, inneres `None` = entfernen.
    if let Some(value) = assignment {
        node.assignment = value.clone();
    }
}

/// Schreibt den ausführenden Worker in die Zuweisung des Knotens.
///
/// Ein vorhandener Versuchszähler und eine vorhandene Job-Referenz bleiben
/// erhalten — der Übergang nach `InProgress` beginnt keinen neuen Versuch,
/// sondern startet den bereits gezählten.
fn assign_worker(node: &mut PlanNode, actor: &str) {
    let assignment = match node.assignment.take() {
        Some(mut previous) => {
            previous.worker = actor.to_owned();
            previous
        }
        None => Assignment {
            worker: actor.to_owned(),
            attempt: FIRST_ATTEMPT,
            job: None,
        },
    };
    node.assignment = Some(assignment);
}

/// Erhöht den Versuchszähler des Knotens für einen neuen Anlauf.
///
/// Ohne bestehende Zuweisung wird der erste Versuch angelegt; als Worker wird
/// vorläufig der wiedereröffnende Akteur eingetragen, den der nächste Übergang
/// nach `InProgress` durch den tatsächlich ausführenden Worker ersetzt.
fn begin_next_attempt(node: &mut PlanNode, actor: &str) {
    let assignment = match node.assignment.take() {
        Some(mut previous) => {
            previous.attempt = previous.attempt.saturating_add(1);
            previous
        }
        None => Assignment {
            worker: actor.to_owned(),
            attempt: FIRST_ATTEMPT,
            job: None,
        },
    };
    node.assignment = Some(assignment);
}

/// Hängt einen Nachweis an, sofern (kind, locator) noch nicht vorhanden ist.
///
/// Duplikate erzeugen keinen zweiten Eintrag (Design-Doc §5 Regel 9), erneuern
/// aber den `attached_at`-Zeitstempel des vorhandenen Eintrags: eine erneute
/// Exploration unter demselben Locator muss die Frist (Regel 12) wieder
/// aufladen können (G-014). Der `attached_at`-Zeitstempel der Payload wird
/// verworfen.
fn push_evidence_unique(
    evidence: &mut Vec<EvidenceRef>,
    candidate: &EvidenceRef,
    now: OffsetDateTime,
) {
    if let Some(existing) = evidence
        .iter_mut()
        .find(|existing| existing.kind == candidate.kind && existing.locator == candidate.locator)
    {
        existing.attached_at = now;
        return;
    }
    let mut attached = candidate.clone();
    attached.attached_at = now;
    evidence.push(attached);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{ContractRef, PathOrSymbol, PlanId, RevisionId};
    use crate::test_support::{TestError, TestResult};
    use crate::types::{Criterion, InvalidationCondition, VerificationStep};
    use time::Duration;

    /// Zeitstempel der Mutation — bewusst verschieden von `PAYLOAD_TIME`,
    /// damit sichtbar wird, welcher Wert gewonnen hat.
    fn now() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH + Duration::hours(42)
    }

    /// Zeitstempel, den eine Payload mitbringt und der verworfen werden muss.
    fn payload_time() -> OffsetDateTime {
        OffsetDateTime::UNIX_EPOCH
    }

    fn make_plan(nodes: Vec<PlanNode>) -> Plan {
        Plan {
            id: PlanId::new("p-mutation"),
            revision: RevisionId::new(7),
            parent_revision: None,
            goal_statement: "Mutationstests".to_owned(),
            goal_id: None,
            nodes,
            created_at: payload_time(),
            updated_at: payload_time(),
        }
    }

    fn make_node(id: &str, status: PlanNodeStatus) -> PlanNode {
        PlanNode {
            id: TaskId::new(id),
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
            kind: PlanNodeKind::Coding,
            wave: None,
            assignment: None,
            parent: None,
            created_at: payload_time(),
            updated_at: payload_time(),
        }
    }

    /// Ein abgeschlossener Research-Knoten — die Form, die `validate_condense`
    /// als verdichtbar zulässt.
    fn make_research_node(id: &str) -> PlanNode {
        let mut node = make_node(id, PlanNodeStatus::Completed);
        node.kind = PlanNodeKind::Research;
        node
    }

    /// Der Ersatzknoten einer Verdichtung muss laut `validate_condense` ein
    /// Contract-Knoten sein.
    fn make_contract_node(id: &str) -> PlanNode {
        let mut node = make_node(id, PlanNodeStatus::Draft);
        node.kind = PlanNodeKind::Contract;
        node
    }

    fn make_evidence(kind: EvidenceKind, locator: &str) -> EvidenceRef {
        EvidenceRef {
            kind,
            locator: locator.to_owned(),
            attached_at: payload_time(),
            actor: "ci".to_owned(),
            digest: None,
        }
    }

    fn find<'a>(plan: &'a Plan, id: &str) -> TestResult<&'a PlanNode> {
        let wanted = TaskId::new(id);
        plan.nodes
            .iter()
            .find(|node| node.id == wanted)
            .ok_or(TestError::Missing("Knoten im Plan"))
    }

    // ── AddNode ───────────────────────────────────────────────────────────

    #[test]
    fn test_add_node_overwrites_payload_timestamps() -> TestResult {
        let mut plan = make_plan(Vec::new());
        let action = PlanAction::AddNode {
            node: make_node("t1", PlanNodeStatus::Draft),
        };

        apply_mutation(&mut plan, &action, "worker", now());

        assert_eq!(plan.nodes.len(), 1);
        let node = find(&plan, "t1")?;
        assert_eq!(node.created_at, now());
        assert_eq!(node.updated_at, now());
        assert_eq!(node.status, PlanNodeStatus::Draft);
        Ok(())
    }

    #[test]
    fn test_add_node_keeps_payload_parent() -> TestResult {
        let mut plan = make_plan(Vec::new());
        let mut node = make_node("t1", PlanNodeStatus::Draft);
        node.parent = Some(TaskId::new("t-root"));
        let action = PlanAction::AddNode { node };

        apply_mutation(&mut plan, &action, "worker", now());

        assert_eq!(find(&plan, "t1")?.parent, Some(TaskId::new("t-root")));
        Ok(())
    }

    // ── UpdateNode / NodePatch ────────────────────────────────────────────

    #[test]
    fn test_update_node_patch_objective() -> TestResult {
        let mut plan = make_plan(vec![make_node("t1", PlanNodeStatus::Draft)]);
        let action = PlanAction::UpdateNode {
            id: TaskId::new("t1"),
            patch: NodePatch {
                objective: Some("neues Ziel".to_owned()),
                ..Default::default()
            },
        };

        apply_mutation(&mut plan, &action, "worker", now());

        let node = find(&plan, "t1")?;
        assert_eq!(node.objective, "neues Ziel");
        assert_eq!(node.updated_at, now());
        assert_eq!(
            node.created_at,
            payload_time(),
            "UpdateNode darf created_at nicht anfassen"
        );
        Ok(())
    }

    #[test]
    fn test_update_node_patch_kind() -> TestResult {
        let mut plan = make_plan(vec![make_node("t1", PlanNodeStatus::Draft)]);
        let action = PlanAction::UpdateNode {
            id: TaskId::new("t1"),
            patch: NodePatch {
                kind: Some(PlanNodeKind::Research),
                ..Default::default()
            },
        };

        apply_mutation(&mut plan, &action, "worker", now());

        assert_eq!(find(&plan, "t1")?.kind, PlanNodeKind::Research);
        Ok(())
    }

    #[test]
    fn test_update_node_patch_wave_sets_and_clears() -> TestResult {
        let mut plan = make_plan(vec![make_node("t1", PlanNodeStatus::Draft)]);

        let set = PlanAction::UpdateNode {
            id: TaskId::new("t1"),
            patch: NodePatch {
                wave: Some(Some(3)),
                ..Default::default()
            },
        };
        apply_mutation(&mut plan, &set, "worker", now());
        assert_eq!(find(&plan, "t1")?.wave, Some(3));

        // Äußeres `Some`, inneres `None` löscht die Wellen-Zuordnung.
        let clear = PlanAction::UpdateNode {
            id: TaskId::new("t1"),
            patch: NodePatch {
                wave: Some(None),
                ..Default::default()
            },
        };
        apply_mutation(&mut plan, &clear, "worker", now());
        assert_eq!(find(&plan, "t1")?.wave, None);
        Ok(())
    }

    #[test]
    fn test_update_node_patch_wave_untouched_when_outer_none() -> TestResult {
        let mut plan = make_plan(vec![make_node("t1", PlanNodeStatus::Draft)]);
        let set = PlanAction::UpdateNode {
            id: TaskId::new("t1"),
            patch: NodePatch {
                wave: Some(Some(5)),
                ..Default::default()
            },
        };
        apply_mutation(&mut plan, &set, "worker", now());

        // Äußeres `None` = Feld nicht angefasst.
        let untouched = PlanAction::UpdateNode {
            id: TaskId::new("t1"),
            patch: NodePatch {
                objective: Some("nur das Ziel".to_owned()),
                ..Default::default()
            },
        };
        apply_mutation(&mut plan, &untouched, "worker", now());

        assert_eq!(find(&plan, "t1")?.wave, Some(5));
        Ok(())
    }

    #[test]
    fn test_update_node_patch_dependencies() -> TestResult {
        let mut plan = make_plan(vec![make_node("t1", PlanNodeStatus::Draft)]);
        let action = PlanAction::UpdateNode {
            id: TaskId::new("t1"),
            patch: NodePatch {
                dependencies: Some(vec![TaskId::new("t0")]),
                ..Default::default()
            },
        };

        apply_mutation(&mut plan, &action, "worker", now());

        assert_eq!(find(&plan, "t1")?.dependencies, vec![TaskId::new("t0")]);
        Ok(())
    }

    #[test]
    fn test_update_node_patch_all_scopes() -> TestResult {
        let mut plan = make_plan(vec![make_node("t1", PlanNodeStatus::Draft)]);
        let action = PlanAction::UpdateNode {
            id: TaskId::new("t1"),
            patch: NodePatch {
                read_scope: Some(vec![PathOrSymbol::new("src/read.rs")]),
                write_scope: Some(vec![PathOrSymbol::new("src/write.rs")]),
                forbidden_scope: Some(vec![PathOrSymbol::new("src/secret.rs")]),
                ..Default::default()
            },
        };

        apply_mutation(&mut plan, &action, "worker", now());

        let node = find(&plan, "t1")?;
        assert_eq!(node.read_scope, vec![PathOrSymbol::new("src/read.rs")]);
        assert_eq!(node.write_scope, vec![PathOrSymbol::new("src/write.rs")]);
        assert_eq!(
            node.forbidden_scope,
            vec![PathOrSymbol::new("src/secret.rs")]
        );
        Ok(())
    }

    #[test]
    fn test_update_node_patch_contracts() -> TestResult {
        let mut plan = make_plan(vec![make_node("t1", PlanNodeStatus::Draft)]);
        let action = PlanAction::UpdateNode {
            id: TaskId::new("t1"),
            patch: NodePatch {
                input_contracts: Some(vec![ContractRef::new("in::A")]),
                output_contracts: Some(vec![ContractRef::new("out::B")]),
                ..Default::default()
            },
        };

        apply_mutation(&mut plan, &action, "worker", now());

        let node = find(&plan, "t1")?;
        assert_eq!(node.input_contracts, vec![ContractRef::new("in::A")]);
        assert_eq!(node.output_contracts, vec![ContractRef::new("out::B")]);
        Ok(())
    }

    #[test]
    fn test_update_node_patch_acceptance_criteria() -> TestResult {
        let mut plan = make_plan(vec![make_node("t1", PlanNodeStatus::Draft)]);
        let action = PlanAction::UpdateNode {
            id: TaskId::new("t1"),
            patch: NodePatch {
                acceptance_criteria: Some(vec![Criterion {
                    description: "cargo test grün".to_owned(),
                    verification: vec![VerificationStep::Command {
                        cmd: "cargo test".to_owned(),
                        expect_exit: 0,
                    }],
                }]),
                ..Default::default()
            },
        };

        apply_mutation(&mut plan, &action, "worker", now());

        let node = find(&plan, "t1")?;
        assert_eq!(node.acceptance_criteria.len(), 1);
        assert_eq!(node.acceptance_criteria[0].description, "cargo test grün");
        assert_eq!(node.acceptance_criteria[0].verification.len(), 1);
        Ok(())
    }

    #[test]
    fn test_update_node_patch_assignment_sets_and_clears() -> TestResult {
        let mut plan = make_plan(vec![make_node("t1", PlanNodeStatus::Draft)]);

        let set = PlanAction::UpdateNode {
            id: TaskId::new("t1"),
            patch: NodePatch {
                assignment: Some(Some(Assignment {
                    worker: "w-1".to_owned(),
                    attempt: 2,
                    job: Some("job-9".to_owned()),
                })),
                ..Default::default()
            },
        };
        apply_mutation(&mut plan, &set, "worker", now());
        assert_eq!(
            find(&plan, "t1")?.assignment,
            Some(Assignment {
                worker: "w-1".to_owned(),
                attempt: 2,
                job: Some("job-9".to_owned()),
            })
        );

        let clear = PlanAction::UpdateNode {
            id: TaskId::new("t1"),
            patch: NodePatch {
                assignment: Some(None),
                ..Default::default()
            },
        };
        apply_mutation(&mut plan, &clear, "worker", now());
        assert_eq!(find(&plan, "t1")?.assignment, None);
        Ok(())
    }

    #[test]
    fn test_update_node_with_empty_patch_only_touches_updated_at() -> TestResult {
        let mut plan = make_plan(vec![make_node("t1", PlanNodeStatus::Draft)]);
        let action = PlanAction::UpdateNode {
            id: TaskId::new("t1"),
            patch: NodePatch::default(),
        };

        apply_mutation(&mut plan, &action, "worker", now());

        let node = find(&plan, "t1")?;
        assert_eq!(node.objective, "Ziel von t1");
        assert_eq!(node.kind, PlanNodeKind::Coding);
        assert_eq!(node.updated_at, now());
        Ok(())
    }

    #[test]
    fn test_update_node_with_unknown_id_is_noop() -> TestResult {
        let mut plan = make_plan(vec![make_node("t1", PlanNodeStatus::Draft)]);
        let action = PlanAction::UpdateNode {
            id: TaskId::new("does-not-exist"),
            patch: NodePatch {
                objective: Some("ignoriert".to_owned()),
                ..Default::default()
            },
        };

        apply_mutation(&mut plan, &action, "worker", now());

        assert_eq!(plan.nodes.len(), 1);
        assert_eq!(find(&plan, "t1")?.objective, "Ziel von t1");
        assert_eq!(find(&plan, "t1")?.updated_at, payload_time());
        Ok(())
    }

    // ── AddDependency ─────────────────────────────────────────────────────

    #[test]
    fn test_add_dependency_is_idempotent() -> TestResult {
        let mut plan = make_plan(vec![
            make_node("t0", PlanNodeStatus::Completed),
            make_node("t1", PlanNodeStatus::Draft),
        ]);
        let action = PlanAction::AddDependency {
            child: TaskId::new("t1"),
            parent: TaskId::new("t0"),
        };

        apply_mutation(&mut plan, &action, "worker", now());
        apply_mutation(&mut plan, &action, "worker", now());

        assert_eq!(find(&plan, "t1")?.dependencies, vec![TaskId::new("t0")]);
        Ok(())
    }

    // ── SetStatus ─────────────────────────────────────────────────────────

    #[test]
    fn test_set_status_in_progress_creates_assignment_from_actor() -> TestResult {
        let mut plan = make_plan(vec![make_node("t1", PlanNodeStatus::Ready)]);
        let action = PlanAction::SetStatus {
            id: TaskId::new("t1"),
            status: PlanNodeStatus::InProgress,
            reason: None,
        };

        apply_mutation(&mut plan, &action, "worker-alpha", now());

        let node = find(&plan, "t1")?;
        assert_eq!(node.status, PlanNodeStatus::InProgress);
        assert_eq!(
            node.assignment,
            Some(Assignment {
                worker: "worker-alpha".to_owned(),
                attempt: FIRST_ATTEMPT,
                job: None,
            })
        );
        Ok(())
    }

    #[test]
    fn test_set_status_in_progress_keeps_attempt_and_job() -> TestResult {
        let mut node = make_node("t1", PlanNodeStatus::Ready);
        node.assignment = Some(Assignment {
            worker: "worker-alt".to_owned(),
            attempt: 3,
            job: Some("job-7".to_owned()),
        });
        let mut plan = make_plan(vec![node]);
        let action = PlanAction::SetStatus {
            id: TaskId::new("t1"),
            status: PlanNodeStatus::InProgress,
            reason: None,
        };

        apply_mutation(&mut plan, &action, "worker-neu", now());

        assert_eq!(
            find(&plan, "t1")?.assignment,
            Some(Assignment {
                worker: "worker-neu".to_owned(),
                attempt: 3,
                job: Some("job-7".to_owned()),
            })
        );
        Ok(())
    }

    #[test]
    fn test_set_status_non_in_progress_does_not_create_assignment() -> TestResult {
        let mut plan = make_plan(vec![make_node("t1", PlanNodeStatus::Draft)]);
        let action = PlanAction::SetStatus {
            id: TaskId::new("t1"),
            status: PlanNodeStatus::Ready,
            reason: Some("Deps grün".to_owned()),
        };

        apply_mutation(&mut plan, &action, "worker", now());

        let node = find(&plan, "t1")?;
        assert_eq!(node.status, PlanNodeStatus::Ready);
        assert_eq!(node.assignment, None);
        Ok(())
    }

    // ── AttachEvidence ────────────────────────────────────────────────────

    #[test]
    fn test_attach_evidence_sets_attached_at_and_ignores_duplicates() -> TestResult {
        let mut plan = make_plan(vec![make_node("t1", PlanNodeStatus::InProgress)]);
        let action = PlanAction::AttachEvidence {
            id: TaskId::new("t1"),
            evidence: make_evidence(EvidenceKind::CargoTest, "run-001"),
        };

        apply_mutation(&mut plan, &action, "ci", now());
        apply_mutation(&mut plan, &action, "ci", now());

        let node = find(&plan, "t1")?;
        assert_eq!(node.evidence.len(), 1, "Duplikat muss ignoriert werden");
        assert_eq!(node.evidence[0].kind, EvidenceKind::CargoTest);
        assert_eq!(node.evidence[0].attached_at, now());
        Ok(())
    }

    #[test]
    fn test_attach_evidence_distinguishes_kind_and_locator() -> TestResult {
        let mut plan = make_plan(vec![make_node("t1", PlanNodeStatus::InProgress)]);

        for evidence in [
            make_evidence(EvidenceKind::CargoTest, "run-001"),
            make_evidence(EvidenceKind::Clippy, "run-001"),
            make_evidence(EvidenceKind::CargoTest, "run-002"),
        ] {
            apply_mutation(
                &mut plan,
                &PlanAction::AttachEvidence {
                    id: TaskId::new("t1"),
                    evidence,
                },
                "ci",
                now(),
            );
        }

        assert_eq!(find(&plan, "t1")?.evidence.len(), 3);
        Ok(())
    }

    // ── Invalidate ────────────────────────────────────────────────────────

    #[test]
    fn test_invalidate_sets_status_and_appends_condition() -> TestResult {
        let mut plan = make_plan(vec![
            make_node("t1", PlanNodeStatus::Ready),
            make_node("t2", PlanNodeStatus::Draft),
        ]);
        let action = PlanAction::Invalidate {
            ids: vec![TaskId::new("t1"), TaskId::new("t2")],
            condition: InvalidationCondition::RepoRevisionMoved,
        };

        apply_mutation(&mut plan, &action, "orchestrator", now());

        for id in ["t1", "t2"] {
            let node = find(&plan, id)?;
            assert_eq!(node.status, PlanNodeStatus::Invalidated);
            assert_eq!(node.invalidation_conditions.len(), 1);
            assert!(matches!(
                node.invalidation_conditions[0],
                InvalidationCondition::RepoRevisionMoved
            ));
        }
        Ok(())
    }

    // ── Reopen ────────────────────────────────────────────────────────────

    #[test]
    fn test_reopen_resets_to_draft_and_bumps_attempt() -> TestResult {
        let mut node = make_node("t1", PlanNodeStatus::Invalidated);
        node.assignment = Some(Assignment {
            worker: "worker-alpha".to_owned(),
            attempt: 1,
            job: Some("job-1".to_owned()),
        });
        let mut plan = make_plan(vec![node]);
        let action = PlanAction::Reopen {
            id: TaskId::new("t1"),
            reason: "Upstream-Vertrag geändert".to_owned(),
        };

        apply_mutation(&mut plan, &action, "orchestrator", now());

        let node = find(&plan, "t1")?;
        assert_eq!(node.status, PlanNodeStatus::Draft);
        assert_eq!(
            node.assignment,
            Some(Assignment {
                worker: "worker-alpha".to_owned(),
                attempt: 2,
                job: Some("job-1".to_owned()),
            }),
            "Reopen erhöht nur den Versuchszähler"
        );
        assert_eq!(node.updated_at, now());
        Ok(())
    }

    #[test]
    fn test_reopen_without_assignment_starts_first_attempt() -> TestResult {
        let mut plan = make_plan(vec![make_node("t1", PlanNodeStatus::Invalidated)]);
        let action = PlanAction::Reopen {
            id: TaskId::new("t1"),
            reason: "erneut versuchen".to_owned(),
        };

        apply_mutation(&mut plan, &action, "orchestrator", now());

        assert_eq!(
            find(&plan, "t1")?.assignment,
            Some(Assignment {
                worker: "orchestrator".to_owned(),
                attempt: FIRST_ATTEMPT,
                job: None,
            })
        );
        Ok(())
    }

    #[test]
    fn test_reopen_keeps_existing_invalidation_conditions() -> TestResult {
        let mut node = make_node("t1", PlanNodeStatus::Invalidated);
        node.invalidation_conditions = vec![InvalidationCondition::ManualInvalidate];
        let mut plan = make_plan(vec![node]);
        let action = PlanAction::Reopen {
            id: TaskId::new("t1"),
            reason: "Grund gehört ins Event-Log".to_owned(),
        };

        apply_mutation(&mut plan, &action, "orchestrator", now());

        assert_eq!(
            find(&plan, "t1")?.invalidation_conditions.len(),
            1,
            "Reopen darf keine weitere Invalidierungsbedingung anhängen"
        );
        Ok(())
    }

    // ── Expand ────────────────────────────────────────────────────────────

    #[test]
    fn test_expand_inserts_children_with_parent_and_marks_composite() -> TestResult {
        let mut plan = make_plan(vec![make_node("t-parent", PlanNodeStatus::Draft)]);
        let action = PlanAction::Expand {
            parent: TaskId::new("t-parent"),
            children: vec![
                make_node("t-child-a", PlanNodeStatus::Draft),
                make_node("t-child-b", PlanNodeStatus::Draft),
            ],
        };

        apply_mutation(&mut plan, &action, "orchestrator", now());

        assert_eq!(plan.nodes.len(), 3);
        for id in ["t-child-a", "t-child-b"] {
            let child = find(&plan, id)?;
            assert_eq!(child.parent, Some(TaskId::new("t-parent")));
            assert_eq!(child.created_at, now());
            assert_eq!(child.updated_at, now());
        }
        let parent = find(&plan, "t-parent")?;
        assert_eq!(parent.kind, PlanNodeKind::Composite);
        assert_eq!(parent.updated_at, now());
        Ok(())
    }

    #[test]
    fn test_expand_overrides_payload_parent() -> TestResult {
        let mut plan = make_plan(vec![make_node("t-parent", PlanNodeStatus::Draft)]);
        let mut child = make_node("t-child", PlanNodeStatus::Draft);
        child.parent = Some(TaskId::new("t-falsch"));
        let action = PlanAction::Expand {
            parent: TaskId::new("t-parent"),
            children: vec![child],
        };

        apply_mutation(&mut plan, &action, "orchestrator", now());

        assert_eq!(
            find(&plan, "t-child")?.parent,
            Some(TaskId::new("t-parent")),
            "die Aktion bestimmt den Parent, nicht die Payload"
        );
        Ok(())
    }

    // ── Condense ──────────────────────────────────────────────────────────

    #[test]
    fn test_condense_transfers_deduplicated_evidence() -> TestResult {
        let mut first = make_research_node("t-r1");
        first.evidence = vec![
            make_evidence(EvidenceKind::Finding, "f-1"),
            make_evidence(EvidenceKind::Finding, "f-shared"),
        ];
        let mut second = make_research_node("t-r2");
        second.evidence = vec![
            make_evidence(EvidenceKind::Finding, "f-shared"),
            make_evidence(EvidenceKind::Finding, "f-2"),
        ];
        let mut plan = make_plan(vec![first, second]);

        let action = PlanAction::Condense {
            superseded: vec![TaskId::new("t-r1"), TaskId::new("t-r2")],
            replacement: make_contract_node("t-contract"),
            summary: "drei Fundstellen verdichtet".to_owned(),
        };
        apply_mutation(&mut plan, &action, "orchestrator", now());

        let replacement = find(&plan, "t-contract")?;
        let locators: Vec<&str> = replacement
            .evidence
            .iter()
            .map(|evidence| evidence.locator.as_str())
            .collect();
        assert_eq!(
            locators,
            vec![
                "f-1",
                "f-shared",
                "f-2",
                "condense:drei Fundstellen verdichtet"
            ],
            "Evidenz muss dedupliziert und in Quellreihenfolge übertragen werden"
        );
        assert_eq!(
            replacement.evidence[0].attached_at,
            payload_time(),
            "übertragene Evidenz behält ihren ursprünglichen Zeitstempel"
        );
        Ok(())
    }

    #[test]
    fn test_condense_records_summary_and_supersedes_sources() -> TestResult {
        let mut plan = make_plan(vec![make_research_node("t-r1")]);
        let action = PlanAction::Condense {
            superseded: vec![TaskId::new("t-r1")],
            replacement: make_contract_node("t-contract"),
            summary: "Ergebnis".to_owned(),
        };

        apply_mutation(&mut plan, &action, "orchestrator", now());

        assert_eq!(find(&plan, "t-r1")?.status, PlanNodeStatus::Superseded);

        let replacement = find(&plan, "t-contract")?;
        assert_eq!(
            replacement.objective, "Ziel von t-contract",
            "die Zusammenfassung darf das Ziel nicht überschreiben"
        );
        assert_eq!(replacement.created_at, now());
        let summary = &replacement.evidence[0];
        assert_eq!(summary.kind, EvidenceKind::Manual);
        assert_eq!(summary.locator, "condense:Ergebnis");
        assert_eq!(summary.actor, "orchestrator");
        assert_eq!(summary.attached_at, now());
        assert_eq!(
            summary.digest,
            Some(harw_types::ContentDigest::of("Ergebnis".as_bytes())),
            "die Condense-Zusammenfassung liegt als String vor und muss einen Digest tragen"
        );
        Ok(())
    }

    #[test]
    fn test_condense_ignores_unknown_superseded_ids() -> TestResult {
        let mut plan = make_plan(vec![make_research_node("t-r1")]);
        let action = PlanAction::Condense {
            superseded: vec![TaskId::new("t-r1"), TaskId::new("t-unbekannt")],
            replacement: make_contract_node("t-contract"),
            summary: "Ergebnis".to_owned(),
        };

        apply_mutation(&mut plan, &action, "orchestrator", now());

        assert_eq!(plan.nodes.len(), 2);
        assert_eq!(find(&plan, "t-contract")?.evidence.len(), 1);
        Ok(())
    }

    // ── Supersede ─────────────────────────────────────────────────────────

    #[test]
    fn test_supersede_sets_parent_revision_and_supersedes_open_nodes() -> TestResult {
        let mut plan = make_plan(vec![
            make_node("t-draft", PlanNodeStatus::Draft),
            make_node("t-ready", PlanNodeStatus::Ready),
            make_node("t-blocked", PlanNodeStatus::Blocked),
        ]);
        let action = PlanAction::Supersede {
            new_parent_revision: RevisionId::new(9),
        };

        apply_mutation(&mut plan, &action, "orchestrator", now());

        assert_eq!(plan.parent_revision, Some(RevisionId::new(7)));
        assert_eq!(
            plan.revision,
            RevisionId::new(7),
            "die Revision setzt der Store, nicht die Mutation"
        );
        for id in ["t-draft", "t-ready", "t-blocked"] {
            assert_eq!(find(&plan, id)?.status, PlanNodeStatus::Superseded);
            assert_eq!(find(&plan, id)?.updated_at, now());
        }
        Ok(())
    }

    #[test]
    fn test_supersede_keeps_terminal_node_status() -> TestResult {
        let mut plan = make_plan(vec![
            make_node("t-completed", PlanNodeStatus::Completed),
            make_node("t-invalidated", PlanNodeStatus::Invalidated),
        ]);
        let action = PlanAction::Supersede {
            new_parent_revision: RevisionId::new(9),
        };

        apply_mutation(&mut plan, &action, "orchestrator", now());

        assert_eq!(
            find(&plan, "t-completed")?.status,
            PlanNodeStatus::Completed
        );
        assert_eq!(
            find(&plan, "t-invalidated")?.status,
            PlanNodeStatus::Invalidated
        );
        assert_eq!(
            find(&plan, "t-completed")?.updated_at,
            payload_time(),
            "unveränderte Knoten dürfen keinen neuen Zeitstempel bekommen"
        );
        Ok(())
    }

    // ── No-ops ────────────────────────────────────────────────────────────

    #[test]
    fn test_bind_goal_sets_goal_id() -> TestResult {
        let mut plan = make_plan(vec![make_node("t1", PlanNodeStatus::Draft)]);
        let action = PlanAction::BindGoal {
            goal_id: "g-1".to_owned(),
        };

        apply_mutation(&mut plan, &action, "owner", now());

        assert_eq!(
            plan.goal_id.as_deref(),
            Some("g-1"),
            "F-126: goal_id gesetzt"
        );
        assert_eq!(
            plan.goal_statement, "Mutationstests",
            "BindGoal darf goal_statement nicht überschreiben"
        );
        assert_eq!(find(&plan, "t1")?.updated_at, payload_time());
        Ok(())
    }

    #[test]
    fn test_add_node_clamps_future_evidence_timestamp() -> TestResult {
        let mut plan = make_plan(Vec::new());
        let mut node = make_node("t1", PlanNodeStatus::Draft);
        let mut future = make_evidence(EvidenceKind::Finding, "q-1");
        future.attached_at = now() + Duration::days(3650);
        let mut past = make_evidence(EvidenceKind::Finding, "q-0");
        past.attached_at = now() - Duration::hours(1);
        node.evidence = vec![future, past];

        apply_mutation(&mut plan, &PlanAction::AddNode { node }, "worker", now());

        let node = find(&plan, "t1")?;
        assert_eq!(node.evidence[0].attached_at, now(), "Zukunft wird gekappt");
        assert_eq!(
            node.evidence[1].attached_at,
            now() - Duration::hours(1),
            "Vergangenheit bleibt"
        );
        Ok(())
    }

    #[test]
    fn test_attach_evidence_duplicate_refreshes_attached_at() -> TestResult {
        let mut plan = make_plan(vec![make_node("t1", PlanNodeStatus::Draft)]);
        let action = PlanAction::AttachEvidence {
            id: TaskId::new("t1"),
            evidence: make_evidence(EvidenceKind::Finding, "q-1"),
        };

        apply_mutation(&mut plan, &action, "explorer", now());
        let later = now() + Duration::hours(30);
        apply_mutation(&mut plan, &action, "explorer", later);

        let node = find(&plan, "t1")?;
        assert_eq!(node.evidence.len(), 1);
        assert_eq!(node.evidence[0].attached_at, later, "G-014: Frist erneuert");
        Ok(())
    }

    #[test]
    fn test_create_and_inspect_are_noops() -> TestResult {
        let mut plan = make_plan(vec![make_node("t1", PlanNodeStatus::Draft)]);

        apply_mutation(
            &mut plan,
            &PlanAction::Create {
                plan_id: PlanId::new("p-neu"),
                goal: "anderes Ziel".to_owned(),
            },
            "orchestrator",
            now(),
        );
        apply_mutation(&mut plan, &PlanAction::Inspect, "orchestrator", now());

        assert_eq!(plan.id, PlanId::new("p-mutation"));
        assert_eq!(plan.goal_statement, "Mutationstests");
        assert_eq!(plan.nodes.len(), 1);
        assert_eq!(find(&plan, "t1")?.updated_at, payload_time());
        Ok(())
    }
}
