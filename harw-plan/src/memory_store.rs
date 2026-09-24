//! In-Memory-Implementierung des `PlanStore`-Traits.
//!
//! Verantwortungsbereich: `InMemoryPlanStore` — thread-sicherer Store via
//! `std::sync::RwLock`. Für Tests und temporäre Pläne ohne Persistenz.
//!
//! Die Mutationslogik selbst liegt **nicht** hier, sondern in
//! `crate::mutation` — dieselbe Funktion, die auch `FilePlanStore` aufruft.
//! Dieses Modul verantwortet ausschließlich Locking, Revisionsvergabe,
//! Konfigurationsdurchsetzung, die Event-History und (Runde 5, Teil P) den
//! Plan-Katalog: mehrere Pläne mit eigener Revisionsfolge und History, genau
//! einer davon aktiv (siehe [`crate::catalog`]).
//!
//! Exportierte Typen: [`InMemoryPlanStore`].

use std::sync::RwLock;

use time::OffsetDateTime;
use tracing::{debug, info};

use crate::actions::{PlanAction, PlanEvent};
use crate::catalog::{PlanApproval, PlanMeta, PlanSummary};
use crate::config::PlanToolConfig;
use crate::error::{PlanError, PlanResult};
use crate::ids::{PlanId, RevisionId};
use crate::store::{PlanRevision, PlanStore, check_batch_target, stage_actions};
use crate::types::Plan;

/// Ein Plan im Katalog samt eigener History und Revisionsfolge.
struct Slot {
    /// Aktueller Planzustand.
    plan: Plan,
    /// Event-History dieses Plans (append-only).
    history: Vec<PlanEvent>,
    /// Nächste Revisionsnummer dieses Plans.
    next_revision: RevisionId,
    /// Katalogdaten (Archiv, Freigabestand).
    meta: PlanMeta,
}

/// Interner Zustand des In-Memory-Stores.
struct Inner {
    /// Alle Pläne in Anlagereihenfolge.
    plans: Vec<Slot>,
    /// Der aktive Plan (Ziel aller Mutationen).
    active: Option<PlanId>,
}

impl Inner {
    // Index des Plans `id`.
    fn position(&self, id: &PlanId) -> Option<usize> {
        self.plans.iter().position(|slot| &slot.plan.id == id)
    }

    // Der aktive Plan.
    fn active_slot(&self) -> Option<&Slot> {
        let active = self.active.as_ref()?;
        self.plans.iter().find(|slot| &slot.plan.id == active)
    }

    // Der aktive Plan, veränderlich.
    fn active_slot_mut(&mut self) -> Option<&mut Slot> {
        let active = self.active.clone()?;
        self.plans.iter_mut().find(|slot| slot.plan.id == active)
    }

    // Der Plan `id` oder `PlanUnknown`.
    fn slot_mut(&mut self, id: &PlanId) -> PlanResult<&mut Slot> {
        self.plans
            .iter_mut()
            .find(|slot| &slot.plan.id == id)
            .ok_or_else(|| PlanError::PlanUnknown { id: id.clone() })
    }
}

/// Thread-sicherer In-Memory-`PlanStore`.
///
/// # Description
/// Nutzt `std::sync::RwLock<Inner>` intern. Validierung erfolgt vor jeder
/// Mutation. Ideal für Unit-Tests und Szenarien ohne Persistenz. Hält
/// beliebig viele Pläne; Mutationen, `current()`, `history()` und
/// `revision()` beziehen sich auf den aktiven Plan.
///
/// # Concurrency
/// `Send + Sync` durch `RwLock`. Lese-Operationen halten nur einen Lese-Lock;
/// `apply` hält einen exklusiven Schreib-Lock.
///
/// # Examples
/// ```rust,no_run
/// use harw_plan::memory_store::InMemoryPlanStore;
/// use harw_plan::store::PlanStore;
/// use harw_plan::actions::PlanAction;
/// use harw_plan::ids::PlanId;
///
/// let store = InMemoryPlanStore::new();
/// store.apply(PlanAction::Create {
///     plan_id: PlanId::new("p-1"),
///     goal: "Ziel".to_owned(),
/// }, "orchestrator").unwrap();
/// let plan = store.current().unwrap();
/// assert_eq!(plan.goal_statement, "Ziel");
/// ```
pub struct InMemoryPlanStore {
    inner: RwLock<Inner>,
    config: PlanToolConfig,
}

impl InMemoryPlanStore {
    /// Erstellt einen neuen, leeren `InMemoryPlanStore` mit aktivierten Standardwerten.
    ///
    /// Dieser Kompatibilitätspfad verwendet explizit die aktivierten
    /// Standardwerte. Neue Aufrufer sollen [`Self::with_config`] verwenden,
    /// damit ihre Tool-Konfiguration an der Store-Grenze erzwungen wird.
    pub fn new() -> Self {
        // `enabled_defaults()` ist aktiviert und hat `max_nodes > 0`; die
        // Prüfung aus `with_config` kann hier nicht scheitern und wird deshalb
        // ohne `expect()` übersprungen.
        Self::from_parts(PlanToolConfig::enabled_defaults())
    }

    // Baut den leeren Store ohne erneute Konfigurationsprüfung.
    fn from_parts(config: PlanToolConfig) -> Self {
        Self {
            inner: RwLock::new(Inner {
                plans: Vec::new(),
                active: None,
            }),
            config,
        }
    }

    /// Erstellt einen neuen, leeren `InMemoryPlanStore` und erzwingt die
    /// Plan-Tool-Konfiguration.
    ///
    /// # Errors
    /// - [`PlanError::Config`] wenn die Konfiguration deaktiviert oder ungültig ist.
    pub fn with_config(config: PlanToolConfig) -> PlanResult<Self> {
        config.require_enabled().map_err(PlanError::Config)?;
        Ok(Self::from_parts(config))
    }

    // Lese-Lock mit einheitlichem Fehler.
    fn read(&self) -> PlanResult<std::sync::RwLockReadGuard<'_, Inner>> {
        self.inner
            .read()
            .map_err(|_| PlanError::Io(std::io::Error::other("RwLock vergiftet")))
    }

    // Schreib-Lock mit einheitlichem Fehler.
    fn write(&self) -> PlanResult<std::sync::RwLockWriteGuard<'_, Inner>> {
        self.inner
            .write()
            .map_err(|_| PlanError::Io(std::io::Error::other("RwLock vergiftet")))
    }
}

impl Default for InMemoryPlanStore {
    fn default() -> Self {
        Self::new()
    }
}

impl PlanStore for InMemoryPlanStore {
    fn current(&self) -> PlanResult<Plan> {
        let inner = self.read()?;
        inner
            .active_slot()
            .map(|slot| slot.plan.clone())
            .ok_or(PlanError::PlanNotFound)
    }

    fn revision(&self) -> RevisionId {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        match inner.active_slot() {
            Some(slot) => slot.plan.revision,
            None => RevisionId::new(0),
        }
    }

    fn apply(&self, action: PlanAction, actor: &str) -> PlanResult<PlanEvent> {
        let mut inner = self.write()?;

        let now = OffsetDateTime::now_utc();

        // `Create` legt einen **weiteren** Plan an und macht ihn aktiv
        // (Runde 5, Teil P). Eine bereits vergebene ID wird abgelehnt —
        // `Create` überschreibt nie.
        if let PlanAction::Create {
            ref plan_id,
            ref goal,
        } = action
        {
            self.config
                .validate_action(&action, 0)
                .map_err(PlanError::Config)?;
            // Grammatik an der Store-Grenze, auch für per `PlanId::new`
            // erzeugte IDs (F-013/G-032).
            let plan_id = PlanId::parse(plan_id.as_str())?;
            if inner.position(&plan_id).is_some() {
                return Err(PlanError::PlanExists { id: plan_id });
            }

            info!(plan_id = %plan_id, "Neuen Plan erstellen");
            let revision = RevisionId::new(1);
            let plan = Plan {
                id: plan_id.clone(),
                revision,
                parent_revision: None,
                goal_statement: goal.clone(),
                goal_id: None,
                nodes: Vec::new(),
                created_at: now,
                updated_at: now,
            };
            let event = PlanEvent {
                revision,
                action,
                actor: actor.to_owned(),
                applied_at: now,
            };
            inner.plans.push(Slot {
                plan,
                history: vec![event.clone()],
                next_revision: revision.next(),
                meta: PlanMeta::default(),
            });
            inner.active = Some(plan_id);
            return Ok(event);
        }

        let config = &self.config;
        let Some(slot) = inner.active_slot_mut() else {
            return Err(PlanError::PlanNotFound);
        };
        let revision = slot.next_revision;

        // Validierung + Mutation auf einem Kandidaten — dieselbe Mechanik wie
        // `apply_batch` und `FilePlanStore`.
        let (candidate, mut events) =
            stage_actions(&slot.plan, vec![action], actor, config, revision, now)
                .map_err(|(_, error)| error)?;
        let Some(event) = events.pop() else {
            return Err(PlanError::PlanNotFound);
        };

        slot.plan = candidate;
        slot.next_revision = revision.next();
        debug!(revision = %revision, actor = actor, "Aktion angewendet");
        slot.history.push(event.clone());
        Ok(event)
    }

    fn apply_batch(
        &self,
        plan: &PlanId,
        actions: Vec<PlanAction>,
        actor: &str,
        expected_rev: RevisionId,
    ) -> PlanResult<PlanRevision> {
        let mut inner = self.write()?;
        self.config.require_enabled().map_err(PlanError::Config)?;

        let config = &self.config;
        let Some(slot) = inner.active_slot_mut() else {
            return Err(PlanError::PlanNotFound);
        };
        let current = check_batch_target(Some(&slot.plan), plan, expected_rev)?;
        if actions.is_empty() {
            return Ok(PlanRevision {
                revision: current.revision,
                events: Vec::new(),
            });
        }

        let now = OffsetDateTime::now_utc();
        let first_revision = slot.next_revision;
        let (candidate, events) =
            stage_actions(current, actions, actor, config, first_revision, now).map_err(
                |(index, source)| PlanError::BatchActionRejected {
                    index,
                    source: Box::new(source),
                },
            )?;

        let revision = candidate.revision;
        slot.plan = candidate;
        slot.next_revision = revision.next();
        slot.history.extend(events.iter().cloned());
        info!(
            plan_id = %plan,
            revision = %revision,
            count = events.len(),
            actor = actor,
            "Batch atomar angewendet"
        );
        Ok(PlanRevision { revision, events })
    }

    fn history(&self, since: Option<RevisionId>) -> PlanResult<Vec<PlanEvent>> {
        let inner = self.read()?;
        let Some(slot) = inner.active_slot() else {
            return Ok(Vec::new());
        };
        let events = match since {
            None => slot.history.clone(),
            Some(rev) => slot
                .history
                .iter()
                .filter(|e| e.revision >= rev)
                .cloned()
                .collect(),
        };
        Ok(events)
    }

    fn list_plans(&self) -> PlanResult<Vec<PlanSummary>> {
        let inner = self.read()?;
        Ok(inner
            .plans
            .iter()
            .map(|slot| {
                let active = inner.active.as_ref() == Some(&slot.plan.id);
                PlanSummary::from_plan(&slot.plan, active, slot.meta)
            })
            .collect())
    }

    fn plan_by_id(&self, id: &PlanId) -> PlanResult<Plan> {
        let inner = self.read()?;
        inner
            .plans
            .iter()
            .find(|slot| &slot.plan.id == id)
            .map(|slot| slot.plan.clone())
            .ok_or_else(|| PlanError::PlanUnknown { id: id.clone() })
    }

    fn plan_meta(&self, id: &PlanId) -> PlanResult<PlanMeta> {
        let inner = self.read()?;
        inner
            .plans
            .iter()
            .find(|slot| &slot.plan.id == id)
            .map(|slot| slot.meta)
            .ok_or_else(|| PlanError::PlanUnknown { id: id.clone() })
    }

    fn switch_plan(&self, id: &PlanId, actor: &str) -> PlanResult<Plan> {
        let mut inner = self.write()?;
        let slot = inner.slot_mut(id)?;
        slot.meta.archived = false;
        let plan = slot.plan.clone();
        inner.active = Some(id.clone());
        info!(plan_id = %id, actor = actor, "Aktiven Plan gewechselt");
        Ok(plan)
    }

    fn archive_plan(&self, id: &PlanId, actor: &str) -> PlanResult<()> {
        let mut inner = self.write()?;
        inner.slot_mut(id)?.meta.archived = true;
        if inner.active.as_ref() == Some(id) {
            inner.active = None;
        }
        info!(plan_id = %id, actor = actor, "Plan archiviert");
        Ok(())
    }

    fn set_approval(&self, id: &PlanId, approval: PlanApproval, actor: &str) -> PlanResult<()> {
        let mut inner = self.write()?;
        inner.slot_mut(id)?.meta.approval = approval;
        info!(plan_id = %id, actor = actor, approval = approval.label(), "Freigabestand gesetzt");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::PlanAction;
    use crate::config::{PlanToolConfig, PlanToolConfigError};
    use crate::error::PlanError;
    use crate::ids::{PathOrSymbol, PlanId, TaskId};
    use crate::store::PlanStore;
    use crate::test_support::TestResult;
    use crate::types::{EvidenceKind, EvidenceRef, PlanNode, PlanNodeKind, PlanNodeStatus};
    use std::sync::Arc;
    use time::OffsetDateTime;

    fn make_node(id: &str) -> PlanNode {
        PlanNode {
            id: TaskId::new(id),
            objective: "obj".to_owned(),
            dependencies: vec![],
            input_contracts: vec![],
            output_contracts: vec![],
            read_scope: vec![],
            write_scope: vec![PathOrSymbol::new(format!("src/{}.rs", id))],
            forbidden_scope: vec![],
            acceptance_criteria: vec![],
            invalidation_conditions: vec![],
            status: PlanNodeStatus::Draft,
            evidence: vec![],
            kind: PlanNodeKind::Coding,
            wave: None,
            assignment: None,
            parent: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn create_plan(store: &InMemoryPlanStore) -> TestResult {
        store.apply(
            PlanAction::Create {
                plan_id: PlanId::new("p-test"),
                goal: "Testziel".to_owned(),
            },
            "orchestrator",
        )?;
        Ok(())
    }

    fn config_with_max_nodes(max_nodes: usize) -> PlanToolConfig {
        PlanToolConfig {
            max_nodes,
            ..PlanToolConfig::enabled_defaults()
        }
    }

    #[test]
    fn test_create_and_inspect() -> TestResult {
        let store = InMemoryPlanStore::new();
        create_plan(&store)?;
        let plan = store.current()?;
        assert_eq!(plan.goal_statement, "Testziel");
        assert_eq!(plan.id, PlanId::new("p-test"));
        Ok(())
    }

    #[test]
    fn test_apply_chain() -> TestResult {
        let store = InMemoryPlanStore::new();
        create_plan(&store)?;

        // AddNode
        store.apply(
            PlanAction::AddNode {
                node: make_node("t1"),
            },
            "actor",
        )?;
        let plan = store.current()?;
        assert_eq!(plan.nodes.len(), 1);

        // AttachEvidence (InProgress erst nötig für Completed)
        store.apply(
            PlanAction::SetStatus {
                id: TaskId::new("t1"),
                status: PlanNodeStatus::Ready,
                reason: None,
            },
            "actor",
        )?;
        store.apply(
            PlanAction::SetStatus {
                id: TaskId::new("t1"),
                status: PlanNodeStatus::InProgress,
                reason: None,
            },
            "actor",
        )?;
        // Der Übergang nach InProgress muss den Akteur als Worker festhalten —
        // vor der Zusammenführung der Mutationslogik wurde `actor` hier verworfen.
        assert_eq!(
            store.current()?.nodes[0]
                .assignment
                .as_ref()
                .map(|assignment| assignment.worker.as_str()),
            Some("actor"),
            "SetStatus(InProgress) muss eine Zuweisung mit dem Akteur anlegen"
        );

        let ev = EvidenceRef {
            kind: EvidenceKind::CargoTest,
            locator: "run-001".to_owned(),
            attached_at: OffsetDateTime::UNIX_EPOCH,
            actor: "ci".to_owned(),
            digest: None,
        };
        store.apply(
            PlanAction::AttachEvidence {
                id: TaskId::new("t1"),
                evidence: ev,
            },
            "ci",
        )?;

        store.apply(
            PlanAction::SetStatus {
                id: TaskId::new("t1"),
                status: PlanNodeStatus::Completed,
                reason: None,
            },
            "actor",
        )?;

        let plan = store.current()?;
        assert_eq!(plan.nodes[0].status, PlanNodeStatus::Completed);
        assert_eq!(plan.nodes[0].evidence.len(), 1);

        // History muss mehrere Events enthalten
        let hist = store.history(None)?;
        assert!(
            hist.len() >= 5,
            "History hat zu wenig Einträge: {}",
            hist.len()
        );
        Ok(())
    }

    #[test]
    fn test_second_create_is_rejected_with_plan_exists() -> TestResult {
        let store = InMemoryPlanStore::new();
        create_plan(&store)?;

        // Runde 5, Teil P: nur eine bereits vergebene ID wird abgelehnt.
        let result = store.apply(
            PlanAction::Create {
                plan_id: PlanId::new("p-test"),
                goal: "darf nicht überschreiben".to_owned(),
            },
            "orchestrator",
        );

        assert!(
            matches!(&result, Err(PlanError::PlanExists { id }) if id == &PlanId::new("p-test")),
            "ein zweites Create mit derselben ID muss abgelehnt werden, Ergebnis: {result:?}"
        );
        assert!(
            result
                .as_ref()
                .err()
                .is_some_and(|error| error.to_string().contains("plan switch p-test")),
            "die Meldung verweist auf `switch`"
        );
        // Der bestehende Plan bleibt unangetastet, es entsteht kein Event.
        let plan = store.current()?;
        assert_eq!(plan.id, PlanId::new("p-test"));
        assert_eq!(plan.goal_statement, "Testziel");
        assert_eq!(store.history(None)?.len(), 1);
        assert_eq!(store.revision(), RevisionId::new(1));
        Ok(())
    }

    #[test]
    fn test_thread_safe_compile_check() {
        // Compile-Zeit-Check: InMemoryPlanStore ist Send + Sync
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<InMemoryPlanStore>();
        assert_send_sync::<Arc<InMemoryPlanStore>>();
    }

    #[test]
    fn test_with_config_rejects_disabled_tool() {
        let result = InMemoryPlanStore::with_config(PlanToolConfig::default());

        assert!(
            matches!(
                result,
                Err(PlanError::Config(PlanToolConfigError::Disabled))
            ),
            "deaktiviertes Tool muss einen typisierten Konfigurationsfehler liefern"
        );
    }

    #[test]
    fn test_configured_store_rejects_node_limit_before_mutation() -> TestResult {
        let store = InMemoryPlanStore::with_config(config_with_max_nodes(1))?;
        create_plan(&store)?;
        store.apply(
            PlanAction::AddNode {
                node: make_node("t1"),
            },
            "worker",
        )?;

        let result = store.apply(
            PlanAction::AddNode {
                node: make_node("t2"),
            },
            "worker",
        );

        assert!(
            matches!(
                result,
                Err(PlanError::Config(PlanToolConfigError::NodeLimitExceeded {
                    max_nodes: 1,
                    attempted_nodes: 2,
                }))
            ),
            "Knotenlimit muss AddNode mit einem typisierten Konfigurationsfehler ablehnen"
        );
        assert_eq!(store.current()?.nodes.len(), 1);
        assert_eq!(store.revision(), RevisionId::new(2));
        assert_eq!(store.history(None)?.len(), 2);
        Ok(())
    }

    // ── apply_batch ───────────────────────────────────────────────────────

    fn set_status(id: &str, status: PlanNodeStatus) -> PlanAction {
        PlanAction::SetStatus {
            id: TaskId::new(id),
            status,
            reason: None,
        }
    }

    #[test]
    fn test_apply_batch_applies_all_actions_with_sequential_revisions() -> TestResult {
        let store = InMemoryPlanStore::new();
        create_plan(&store)?;
        let before = store.revision();

        let result = store.apply_batch(
            &PlanId::new("p-test"),
            vec![
                PlanAction::AddNode {
                    node: make_node("explore"),
                },
                PlanAction::AddNode {
                    node: make_node("impl"),
                },
                PlanAction::AddDependency {
                    child: TaskId::new("impl"),
                    parent: TaskId::new("explore"),
                },
            ],
            "controller",
            before,
        )?;

        assert_eq!(result.events.len(), 3);
        assert_eq!(result.revision, RevisionId::new(before.value() + 3));
        let revisions: Vec<u64> = result.events.iter().map(|e| e.revision.value()).collect();
        assert_eq!(revisions, vec![2, 3, 4]);
        assert_eq!(store.revision(), result.revision);
        let plan = store.current()?;
        assert_eq!(plan.nodes.len(), 2);
        assert_eq!(plan.nodes[1].dependencies, vec![TaskId::new("explore")]);
        assert_eq!(store.history(None)?.len(), 4);
        Ok(())
    }

    #[test]
    fn test_apply_batch_failure_in_third_action_changes_nothing() -> TestResult {
        let store = InMemoryPlanStore::new();
        create_plan(&store)?;
        let before = store.revision();

        let result = store.apply_batch(
            &PlanId::new("p-test"),
            vec![
                PlanAction::AddNode {
                    node: make_node("t1"),
                },
                set_status("t1", PlanNodeStatus::Ready),
                // Ready → Completed ist nicht in der Matrix.
                set_status("t1", PlanNodeStatus::Completed),
            ],
            "controller",
            before,
        );

        assert!(
            matches!(
                &result,
                Err(PlanError::BatchActionRejected { index: 2, source })
                    if matches!(**source, PlanError::IllegalTransition { .. })
            ),
            "Ergebnis: {result:?}"
        );
        assert!(store.current()?.nodes.is_empty(), "nichts angewendet");
        assert_eq!(store.revision(), before);
        assert_eq!(store.history(None)?.len(), 1);

        // Die Revisionsvergabe ist nicht vorgerückt.
        let event = store.apply(
            PlanAction::AddNode {
                node: make_node("t1"),
            },
            "a",
        )?;
        assert_eq!(event.revision, RevisionId::new(2));
        Ok(())
    }

    #[test]
    fn test_apply_batch_revision_conflict_changes_nothing() -> TestResult {
        let store = InMemoryPlanStore::new();
        create_plan(&store)?;
        store.apply(
            PlanAction::AddNode {
                node: make_node("t1"),
            },
            "a",
        )?;

        let result = store.apply_batch(
            &PlanId::new("p-test"),
            vec![PlanAction::AddNode {
                node: make_node("t2"),
            }],
            "controller",
            RevisionId::new(1),
        );

        assert!(
            matches!(
                &result,
                Err(PlanError::RevisionConflict { expected, actual, .. })
                    if *expected == RevisionId::new(1) && *actual == RevisionId::new(2)
            ),
            "Ergebnis: {result:?}"
        );
        assert_eq!(store.current()?.nodes.len(), 1);
        Ok(())
    }

    #[test]
    fn test_apply_batch_other_plan_id_is_plan_not_found() -> TestResult {
        let store = InMemoryPlanStore::new();
        create_plan(&store)?;

        let result = store.apply_batch(
            &PlanId::new("p-other"),
            vec![PlanAction::Inspect],
            "controller",
            store.revision(),
        );

        assert!(matches!(result, Err(PlanError::PlanNotFound)));
        assert_eq!(store.history(None)?.len(), 1);
        Ok(())
    }

    #[test]
    fn test_apply_batch_rejects_create_inside_batch() -> TestResult {
        let store = InMemoryPlanStore::new();
        create_plan(&store)?;

        let result = store.apply_batch(
            &PlanId::new("p-test"),
            vec![PlanAction::Create {
                plan_id: PlanId::new("p-neu"),
                goal: "x".to_owned(),
            }],
            "controller",
            store.revision(),
        );

        assert!(matches!(
            result,
            Err(PlanError::BatchActionRejected { index: 0, .. })
        ));
        Ok(())
    }

    #[test]
    fn test_apply_batch_enforces_node_limit_with_running_count() -> TestResult {
        let store = InMemoryPlanStore::with_config(config_with_max_nodes(1))?;
        create_plan(&store)?;

        let result = store.apply_batch(
            &PlanId::new("p-test"),
            vec![
                PlanAction::AddNode {
                    node: make_node("t1"),
                },
                PlanAction::AddNode {
                    node: make_node("t2"),
                },
            ],
            "controller",
            store.revision(),
        );

        assert!(
            matches!(
                &result,
                Err(PlanError::BatchActionRejected { index: 1, source })
                    if matches!(
                        **source,
                        PlanError::Config(PlanToolConfigError::NodeLimitExceeded { .. })
                    )
            ),
            "Ergebnis: {result:?}"
        );
        assert!(store.current()?.nodes.is_empty());
        Ok(())
    }

    #[test]
    fn test_apply_batch_empty_is_noop() -> TestResult {
        let store = InMemoryPlanStore::new();
        create_plan(&store)?;

        let result =
            store.apply_batch(&PlanId::new("p-test"), Vec::new(), "c", store.revision())?;

        assert!(result.events.is_empty());
        assert_eq!(result.revision, RevisionId::new(1));
        assert_eq!(store.history(None)?.len(), 1);
        Ok(())
    }

    #[test]
    fn test_create_rejects_traversal_plan_id() {
        let store = InMemoryPlanStore::new();

        let result = store.apply(
            PlanAction::Create {
                plan_id: PlanId::new("../escape"),
                goal: "x".to_owned(),
            },
            "orchestrator",
        );

        assert!(matches!(
            result,
            Err(PlanError::InvalidId {
                field: "PlanId",
                ..
            })
        ));
        assert!(matches!(store.current(), Err(PlanError::PlanNotFound)));
    }

    #[test]
    fn test_bind_goal_sets_goal_id_through_store() -> TestResult {
        let store = InMemoryPlanStore::new();
        create_plan(&store)?;

        store.apply(
            PlanAction::BindGoal {
                goal_id: "g-1".to_owned(),
            },
            "human:alice",
        )?;

        assert_eq!(store.current()?.goal_id.as_deref(), Some("g-1"));
        Ok(())
    }

    // ── Runde 5, Teil P: Plan-Katalog ───────────────────────────────────────

    fn create(store: &InMemoryPlanStore, id: &str, goal: &str) -> TestResult {
        store.apply(
            PlanAction::Create {
                plan_id: PlanId::new(id),
                goal: goal.to_owned(),
            },
            "orchestrator",
        )?;
        Ok(())
    }

    #[test]
    fn test_two_plans_in_a_row_each_start_at_revision_one() -> TestResult {
        let store = InMemoryPlanStore::new();
        create(&store, "p-eins", "erstes Ziel")?;
        store.apply(
            PlanAction::AddNode {
                node: make_node("t-1"),
            },
            "orchestrator",
        )?;
        create(&store, "p-zwei", "zweites Ziel")?;

        let active = store.current()?;
        assert_eq!(active.id, PlanId::new("p-zwei"));
        assert_eq!(active.revision, RevisionId::new(1));
        assert_eq!(store.revision(), RevisionId::new(1));
        assert_eq!(store.history(None)?.len(), 1, "History je Plan");

        // Der erste Plan bleibt unverändert erhalten.
        let first = store.plan_by_id(&PlanId::new("p-eins"))?;
        assert_eq!(first.nodes.len(), 1);
        assert_eq!(first.revision, RevisionId::new(2));

        // Mutationen treffen den aktiven Plan.
        store.apply(
            PlanAction::AddNode {
                node: make_node("t-2"),
            },
            "orchestrator",
        )?;
        assert_eq!(store.current()?.nodes.len(), 1);
        assert_eq!(store.plan_by_id(&PlanId::new("p-eins"))?.nodes.len(), 1);
        Ok(())
    }

    #[test]
    fn test_auto_created_analyze_plan_does_not_block_create() -> TestResult {
        // Genau das Transkript: `/analyze` legt `plan-analyze` an, danach
        // ruft die UIA `plan create crypt-guard-hardening-v1` auf.
        let store = InMemoryPlanStore::new();
        create(&store, "plan-analyze", "Analyse des Arbeitsbereichs")?;
        create(&store, "crypt-guard-hardening-v1", "Härtung")?;
        assert_eq!(store.current()?.id, PlanId::new("crypt-guard-hardening-v1"));
        let ids: Vec<String> = store
            .list_plans()?
            .into_iter()
            .map(|summary| summary.id.into_inner())
            .collect();
        assert_eq!(ids, vec!["plan-analyze", "crypt-guard-hardening-v1"]);
        Ok(())
    }

    #[test]
    fn test_switch_archive_and_list() -> TestResult {
        let store = InMemoryPlanStore::new();
        create(&store, "p-a", "A")?;
        create(&store, "p-b", "B")?;

        let switched = store.switch_plan(&PlanId::new("p-a"), "human:test")?;
        assert_eq!(switched.id, PlanId::new("p-a"));
        assert_eq!(store.current()?.id, PlanId::new("p-a"));

        store.archive_plan(&PlanId::new("p-b"), "human:test")?;
        let listed = store.list_plans()?;
        assert_eq!(listed.len(), 2, "archivierte Pläne bleiben gelistet");
        let b = listed
            .iter()
            .find(|summary| summary.id == PlanId::new("p-b"))
            .ok_or(PlanError::PlanNotFound)?;
        assert!(b.meta.archived);
        assert!(!b.active);
        assert!(
            store.plan_by_id(&PlanId::new("p-b")).is_ok(),
            "nicht gelöscht"
        );

        // Archivieren des aktiven Plans lässt keinen aktiven zurück.
        store.archive_plan(&PlanId::new("p-a"), "human:test")?;
        assert!(matches!(store.current(), Err(PlanError::PlanNotFound)));
        assert_eq!(store.revision(), RevisionId::new(0));

        // `switch` holt einen Plan aus dem Archiv zurück.
        store.switch_plan(&PlanId::new("p-b"), "human:test")?;
        assert!(!store.plan_meta(&PlanId::new("p-b"))?.archived);
        assert_eq!(store.current()?.id, PlanId::new("p-b"));

        assert!(matches!(
            store.switch_plan(&PlanId::new("p-nix"), "human:test"),
            Err(PlanError::PlanUnknown { .. })
        ));
        Ok(())
    }

    #[test]
    fn test_approval_is_catalog_data_and_defaults_to_confirmed() -> TestResult {
        let store = InMemoryPlanStore::new();
        create(&store, "p-a", "A")?;
        let id = PlanId::new("p-a");
        assert_eq!(
            store.plan_meta(&id)?.approval,
            crate::PlanApproval::Confirmed
        );
        store.set_approval(&id, crate::PlanApproval::Proposed, "model:test")?;
        assert_eq!(
            store.plan_meta(&id)?.approval,
            crate::PlanApproval::Proposed
        );
        // Keine neue Revision: Katalogdaten sind keine Plan-Mutation.
        assert_eq!(store.revision(), RevisionId::new(1));
        Ok(())
    }
}
