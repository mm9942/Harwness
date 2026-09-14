//! In-Memory-Implementierung des `PlanStore`-Traits.
//!
//! Verantwortungsbereich: `InMemoryPlanStore` — thread-sicherer Store via
//! `std::sync::RwLock`. Für Tests und temporäre Pläne ohne Persistenz.
//!
//! Die Mutationslogik selbst liegt **nicht** hier, sondern in
//! `crate::mutation` — dieselbe Funktion, die auch `FilePlanStore` aufruft.
//! Dieses Modul verantwortet ausschließlich Locking, Revisionsvergabe,
//! Konfigurationsdurchsetzung und die Event-History.
//!
//! Exportierte Typen: [`InMemoryPlanStore`].

use std::sync::RwLock;

use time::OffsetDateTime;
use tracing::{debug, info};

use crate::actions::{PlanAction, PlanEvent};
use crate::config::PlanToolConfig;
use crate::error::{PlanError, PlanResult};
use crate::ids::{PlanId, RevisionId};
use crate::store::{PlanRevision, PlanStore, check_batch_target, stage_actions};
use crate::types::Plan;

/// Interner Zustand des In-Memory-Stores.
struct Inner {
    /// Aktueller Planzustand.
    plan: Option<Plan>,
    /// Event-History (append-only).
    history: Vec<PlanEvent>,
    /// Nächste Revisionsnummer.
    next_revision: RevisionId,
}

/// Thread-sicherer In-Memory-`PlanStore`.
///
/// # Description
/// Nutzt `std::sync::RwLock<Inner>` intern. Validierung erfolgt vor jeder
/// Mutation. Ideal für Unit-Tests und Szenarien ohne Persistenz.
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
                plan: None,
                history: Vec::new(),
                next_revision: RevisionId::new(1),
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
}

impl Default for InMemoryPlanStore {
    fn default() -> Self {
        Self::new()
    }
}

impl PlanStore for InMemoryPlanStore {
    fn current(&self) -> PlanResult<Plan> {
        let inner = self
            .inner
            .read()
            .map_err(|_| PlanError::Io(std::io::Error::other("RwLock vergiftet")))?;
        inner.plan.clone().ok_or(PlanError::PlanNotFound)
    }

    fn revision(&self) -> RevisionId {
        let inner = self.inner.read().unwrap_or_else(|e| e.into_inner());
        match &inner.plan {
            Some(p) => p.revision,
            None => RevisionId::new(0),
        }
    }

    fn apply(&self, action: PlanAction, actor: &str) -> PlanResult<PlanEvent> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| PlanError::Io(std::io::Error::other("RwLock vergiftet")))?;

        let now = OffsetDateTime::now_utc();

        // Für Create brauchen wir keinen bestehenden Plan — aber es darf auch
        // noch keiner existieren: `Create` legt an, es überschreibt nicht.
        if let PlanAction::Create {
            ref plan_id,
            ref goal,
        } = action
        {
            self.config
                .validate_action(&action, 0)
                .map_err(PlanError::Config)?;
            if let Some(existing) = inner.plan.as_ref() {
                return Err(PlanError::PlanExists {
                    id: existing.id.clone(),
                });
            }
            // Grammatik an der Store-Grenze, auch für per `PlanId::new`
            // erzeugte IDs (F-013/G-032).
            let plan_id = PlanId::parse(plan_id.as_str())?;

            info!(plan_id = %plan_id, "Neuen Plan erstellen");
            let revision = inner.next_revision;
            inner.next_revision = revision.next();
            let plan = Plan {
                id: plan_id,
                revision,
                parent_revision: None,
                goal_statement: goal.clone(),
                goal_id: None,
                nodes: Vec::new(),
                created_at: now,
                updated_at: now,
            };
            inner.plan = Some(plan);
            let event = PlanEvent {
                revision,
                action,
                actor: actor.to_owned(),
                applied_at: now,
            };
            inner.history.push(event.clone());
            return Ok(event);
        }

        let revision = inner.next_revision;
        let Some(plan) = inner.plan.as_ref() else {
            return Err(PlanError::PlanNotFound);
        };

        // Validierung + Mutation auf einem Kandidaten — dieselbe Mechanik wie
        // `apply_batch` und `FilePlanStore`.
        let (candidate, mut events) =
            stage_actions(plan, vec![action], actor, &self.config, revision, now)
                .map_err(|(_, error)| error)?;
        let Some(event) = events.pop() else {
            return Err(PlanError::PlanNotFound);
        };

        inner.plan = Some(candidate);
        inner.next_revision = revision.next();
        debug!(revision = %revision, actor = actor, "Aktion angewendet");
        inner.history.push(event.clone());
        Ok(event)
    }

    fn apply_batch(
        &self,
        plan: &PlanId,
        actions: Vec<PlanAction>,
        actor: &str,
        expected_rev: RevisionId,
    ) -> PlanResult<PlanRevision> {
        let mut inner = self
            .inner
            .write()
            .map_err(|_| PlanError::Io(std::io::Error::other("RwLock vergiftet")))?;
        self.config.require_enabled().map_err(PlanError::Config)?;

        let current = check_batch_target(inner.plan.as_ref(), plan, expected_rev)?;
        if actions.is_empty() {
            return Ok(PlanRevision {
                revision: current.revision,
                events: Vec::new(),
            });
        }

        let now = OffsetDateTime::now_utc();
        let first_revision = inner.next_revision;
        let (candidate, events) = stage_actions(
            current,
            actions,
            actor,
            &self.config,
            first_revision,
            now,
        )
        .map_err(|(index, source)| PlanError::BatchActionRejected {
            index,
            source: Box::new(source),
        })?;

        let revision = candidate.revision;
        inner.plan = Some(candidate);
        inner.next_revision = revision.next();
        inner.history.extend(events.iter().cloned());
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
        let inner = self
            .inner
            .read()
            .map_err(|_| PlanError::Io(std::io::Error::other("RwLock vergiftet")))?;
        let events = match since {
            None => inner.history.clone(),
            Some(rev) => inner
                .history
                .iter()
                .filter(|e| e.revision >= rev)
                .cloned()
                .collect(),
        };
        Ok(events)
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

    fn create_plan(store: &InMemoryPlanStore) {
        store
            .apply(
                PlanAction::Create {
                    plan_id: PlanId::new("p-test"),
                    goal: "Testziel".to_owned(),
                },
                "orchestrator",
            )
            .unwrap();
    }

    fn config_with_max_nodes(max_nodes: usize) -> PlanToolConfig {
        PlanToolConfig {
            max_nodes,
            ..PlanToolConfig::enabled_defaults()
        }
    }

    #[test]
    fn test_create_and_inspect() {
        let store = InMemoryPlanStore::new();
        create_plan(&store);
        let plan = store.current().unwrap();
        assert_eq!(plan.goal_statement, "Testziel");
        assert_eq!(plan.id, PlanId::new("p-test"));
    }

    #[test]
    fn test_apply_chain() {
        let store = InMemoryPlanStore::new();
        create_plan(&store);

        // AddNode
        store
            .apply(
                PlanAction::AddNode {
                    node: make_node("t1"),
                },
                "actor",
            )
            .unwrap();
        let plan = store.current().unwrap();
        assert_eq!(plan.nodes.len(), 1);

        // AttachEvidence (InProgress erst nötig für Completed)
        store
            .apply(
                PlanAction::SetStatus {
                    id: TaskId::new("t1"),
                    status: PlanNodeStatus::Ready,
                    reason: None,
                },
                "actor",
            )
            .unwrap();
        store
            .apply(
                PlanAction::SetStatus {
                    id: TaskId::new("t1"),
                    status: PlanNodeStatus::InProgress,
                    reason: None,
                },
                "actor",
            )
            .unwrap();
        // Der Übergang nach InProgress muss den Akteur als Worker festhalten —
        // vor der Zusammenführung der Mutationslogik wurde `actor` hier verworfen.
        assert_eq!(
            store.current().unwrap().nodes[0]
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
        store
            .apply(
                PlanAction::AttachEvidence {
                    id: TaskId::new("t1"),
                    evidence: ev,
                },
                "ci",
            )
            .unwrap();

        store
            .apply(
                PlanAction::SetStatus {
                    id: TaskId::new("t1"),
                    status: PlanNodeStatus::Completed,
                    reason: None,
                },
                "actor",
            )
            .unwrap();

        let plan = store.current().unwrap();
        assert_eq!(plan.nodes[0].status, PlanNodeStatus::Completed);
        assert_eq!(plan.nodes[0].evidence.len(), 1);

        // History muss mehrere Events enthalten
        let hist = store.history(None).unwrap();
        assert!(
            hist.len() >= 5,
            "History hat zu wenig Einträge: {}",
            hist.len()
        );
    }

    #[test]
    fn test_second_create_is_rejected_with_plan_exists() {
        let store = InMemoryPlanStore::new();
        create_plan(&store);

        let result = store.apply(
            PlanAction::Create {
                plan_id: PlanId::new("p-zweit"),
                goal: "darf nicht überschreiben".to_owned(),
            },
            "orchestrator",
        );

        assert!(
            matches!(&result, Err(PlanError::PlanExists { id }) if id == &PlanId::new("p-test")),
            "ein zweites Create muss fail-closed abgelehnt werden, Ergebnis: {result:?}"
        );
        // Der bestehende Plan bleibt unangetastet, es entsteht kein Event.
        let plan = store.current().unwrap();
        assert_eq!(plan.id, PlanId::new("p-test"));
        assert_eq!(plan.goal_statement, "Testziel");
        assert_eq!(store.history(None).unwrap().len(), 1);
        assert_eq!(store.revision(), RevisionId::new(1));
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
    fn test_configured_store_rejects_node_limit_before_mutation() {
        let store = InMemoryPlanStore::with_config(config_with_max_nodes(1)).unwrap();
        create_plan(&store);
        store
            .apply(
                PlanAction::AddNode {
                    node: make_node("t1"),
                },
                "worker",
            )
            .unwrap();

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
        assert_eq!(store.current().unwrap().nodes.len(), 1);
        assert_eq!(store.revision(), RevisionId::new(2));
        assert_eq!(store.history(None).unwrap().len(), 2);
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
    fn test_apply_batch_applies_all_actions_with_sequential_revisions() {
        let store = InMemoryPlanStore::new();
        create_plan(&store);
        let before = store.revision();

        let result = store
            .apply_batch(
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
            )
            .unwrap();

        assert_eq!(result.events.len(), 3);
        assert_eq!(result.revision, RevisionId::new(before.value() + 3));
        let revisions: Vec<u64> = result.events.iter().map(|e| e.revision.value()).collect();
        assert_eq!(revisions, vec![2, 3, 4]);
        assert_eq!(store.revision(), result.revision);
        let plan = store.current().unwrap();
        assert_eq!(plan.nodes.len(), 2);
        assert_eq!(plan.nodes[1].dependencies, vec![TaskId::new("explore")]);
        assert_eq!(store.history(None).unwrap().len(), 4);
    }

    #[test]
    fn test_apply_batch_failure_in_third_action_changes_nothing() {
        let store = InMemoryPlanStore::new();
        create_plan(&store);
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
        assert!(store.current().unwrap().nodes.is_empty(), "nichts angewendet");
        assert_eq!(store.revision(), before);
        assert_eq!(store.history(None).unwrap().len(), 1);

        // Die Revisionsvergabe ist nicht vorgerückt.
        let event = store
            .apply(PlanAction::AddNode { node: make_node("t1") }, "a")
            .unwrap();
        assert_eq!(event.revision, RevisionId::new(2));
    }

    #[test]
    fn test_apply_batch_revision_conflict_changes_nothing() {
        let store = InMemoryPlanStore::new();
        create_plan(&store);
        store
            .apply(PlanAction::AddNode { node: make_node("t1") }, "a")
            .unwrap();

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
        assert_eq!(store.current().unwrap().nodes.len(), 1);
    }

    #[test]
    fn test_apply_batch_other_plan_id_is_plan_not_found() {
        let store = InMemoryPlanStore::new();
        create_plan(&store);

        let result = store.apply_batch(
            &PlanId::new("p-other"),
            vec![PlanAction::Inspect],
            "controller",
            store.revision(),
        );

        assert!(matches!(result, Err(PlanError::PlanNotFound)));
        assert_eq!(store.history(None).unwrap().len(), 1);
    }

    #[test]
    fn test_apply_batch_rejects_create_inside_batch() {
        let store = InMemoryPlanStore::new();
        create_plan(&store);

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
    }

    #[test]
    fn test_apply_batch_enforces_node_limit_with_running_count() {
        let store = InMemoryPlanStore::with_config(config_with_max_nodes(1)).unwrap();
        create_plan(&store);

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
        assert!(store.current().unwrap().nodes.is_empty());
    }

    #[test]
    fn test_apply_batch_empty_is_noop() {
        let store = InMemoryPlanStore::new();
        create_plan(&store);

        let result = store
            .apply_batch(&PlanId::new("p-test"), Vec::new(), "c", store.revision())
            .unwrap();

        assert!(result.events.is_empty());
        assert_eq!(result.revision, RevisionId::new(1));
        assert_eq!(store.history(None).unwrap().len(), 1);
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
            Err(PlanError::InvalidId { field: "PlanId", .. })
        ));
        assert!(matches!(store.current(), Err(PlanError::PlanNotFound)));
    }

    #[test]
    fn test_bind_goal_sets_goal_id_through_store() {
        let store = InMemoryPlanStore::new();
        create_plan(&store);

        store
            .apply(
                PlanAction::BindGoal {
                    goal_id: "g-1".to_owned(),
                },
                "human:mia",
            )
            .unwrap();

        assert_eq!(store.current().unwrap().goal_id.as_deref(), Some("g-1"));
    }
}
