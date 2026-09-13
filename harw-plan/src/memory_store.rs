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
use crate::ids::RevisionId;
use crate::mutation::apply_mutation;
use crate::store::PlanStore;
use crate::types::Plan;
use crate::validate::validate_with;

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
        Self::with_config(PlanToolConfig::enabled_defaults())
            .expect("enabled defaults must construct an in-memory plan store")
    }

    /// Erstellt einen neuen, leeren `InMemoryPlanStore` und erzwingt die
    /// Plan-Tool-Konfiguration.
    ///
    /// # Errors
    /// - [`PlanError::Config`] wenn die Konfiguration deaktiviert oder ungültig ist.
    pub fn with_config(config: PlanToolConfig) -> PlanResult<Self> {
        config.require_enabled().map_err(PlanError::Config)?;

        Ok(Self {
            inner: RwLock::new(Inner {
                plan: None,
                history: Vec::new(),
                next_revision: RevisionId::new(1),
            }),
            config,
        })
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

        let current_node_count = inner.plan.as_ref().map_or(0, |plan| plan.nodes.len());
        self.config
            .validate_action(&action, current_node_count)
            .map_err(PlanError::Config)?;

        let now = OffsetDateTime::now_utc();

        // Für Create brauchen wir keinen bestehenden Plan — aber es darf auch
        // noch keiner existieren: `Create` legt an, es überschreibt nicht.
        if let PlanAction::Create {
            ref plan_id,
            ref goal,
        } = action
        {
            if let Some(existing) = inner.plan.as_ref() {
                return Err(PlanError::PlanExists {
                    id: existing.id.clone(),
                });
            }

            info!(plan_id = %plan_id, "Neuen Plan erstellen");
            let revision = inner.next_revision;
            inner.next_revision = revision.next();
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

        // Für alle anderen Aktionen brauchen wir einen Plan. Die Revision wird
        // vor dem exklusiven Borrow gelesen und erst nach dessen Ende
        // fortgeschrieben, damit weder `expect()` noch ein zweiter Lookup nötig ist.
        let revision = inner.next_revision;
        let Some(plan) = inner.plan.as_mut() else {
            return Err(PlanError::PlanNotFound);
        };

        // Validierung
        validate_with(plan, &action, &self.config, now)?;

        // Mutation anwenden — einzige Mutationsstelle, geteilt mit `FilePlanStore`.
        apply_mutation(plan, &action, actor, now);
        plan.updated_at = now;
        plan.revision = revision;

        inner.next_revision = revision.next();

        debug!(revision = %revision, actor = actor, "Aktion angewendet");

        let event = PlanEvent {
            revision,
            action,
            actor: actor.to_owned(),
            applied_at: now,
        };
        inner.history.push(event.clone());
        Ok(event)
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
}
