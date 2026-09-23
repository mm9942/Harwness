//! `harw-plan` — First-Class Planning-Tool für das Harwness-Crate-Ökosystem.
//!
//! Implementiert den Vertical Slice aus Design-Doc `planning-tool-v1.md`:
//! typisierte Plan-Knoten, In-Memory- und File-Store, vollständige Validation
//! und eine minimale synchrone API.
//!
//! # Verantwortungsbereich
//! - Kern-Datentypen: [`types`], [`ids`]
//! - Aktionen und Events: [`actions`]
//! - Fehlertypen: [`error`]
//! - Konfiguration: [`config`]
//! - Validation: [`validate`], Scope-/Patch-Zulassung: [`admission`]
//! - Graph-Auswertung (ausführbare Knoten, topologische Wellen): [`graph`]
//! - Ziel-Verwaltung: [`goal`]
//! - Store-Trait: [`store`]
//! - Implementierungen: [`memory_store`], [`file_store`], [`goal_store`]
//!
//! # Architekturregel: eine Mutationsstelle
//! Der Plan wird ausschließlich über `PlanStore::apply` verändert. Die
//! eigentliche Zustandsänderung liegt im crate-privaten Modul `mutation`, das
//! beide Stores gemeinsam nutzen — es gibt keine zweite Kopie der
//! Mutationslogik und keinen `&mut PlanNode` in der öffentlichen API.
//!
//! # Nicht im Slice
//! Adapter (Command / Model-Tool / Channel), MCP-Bridge, TUI-Projektion —
//! kommen in Folge-Waves.
//!
//! # Concurrency
//! Alle Store-Implementierungen sind `Send + Sync`.
//!
//! # Examples
//! ```rust,no_run
//! use harw_plan::{InMemoryPlanStore, PlanAction, PlanId, PlanStore};
//!
//! let store = InMemoryPlanStore::new();
//! let created = store.apply(
//!     PlanAction::Create {
//!         plan_id: PlanId::new("p-1"),
//!         goal: "Ziel".to_owned(),
//!     },
//!     "orchestrator",
//! );
//! assert!(created.is_ok());
//!
//! // Default-Methode des Traits: ausführbare Knoten über `graph::ready_nodes`.
//! let ready = store.ready_nodes().unwrap_or_default();
//! assert!(ready.is_empty());
//! ```

#![forbid(unsafe_code)]

pub mod actions;
pub mod admission;
pub mod config;
pub mod error;
pub mod file_store;
pub mod goal;
pub mod goal_store;
pub mod graph;
pub mod ids;
pub mod memory_store;
pub mod store;
// Test-Fixtures für dieses Crate **und** seine Konsumenten. Bewusst
// unbedingt `pub` statt `#[cfg(test)]`: so gegattete Items sind für andere
// Crates unsichtbar, und genau deshalb haben `harw-plan-bridge`, `harw-ops`
// und `harw-tui` bisher dieselben Fixtures noch einmal geschrieben. Die
// Begründung steht ausführlich im Modulkopf.
pub mod testing;
pub mod types;
pub mod validate;

// Crate-privat: die einzige Mutationsstelle. Bewusst nicht `pub` — Mutationen
// laufen ausschließlich über `PlanStore::apply`.
mod mutation;

// Crate-interner Test-Fehlertyp (Bible R087/R165/R182). Ergänzt `testing`
// (siehe dortiger Modulkopf), das öffentlich bleibt und unverändert ist.
#[cfg(test)]
mod test_support;

// ── Re-Exports der Kerntypen ────────────────────────────────────────────────
// Damit Aufrufer `harw_plan::PlanNode` statt `harw_plan::types::PlanNode`
// schreiben können. Die Modulpfade bleiben zusätzlich öffentlich.

pub use crate::actions::{NodePatch, PlanAction, PlanEvent};
pub use crate::admission::ScopeMatcher;
pub use crate::config::PlanToolConfig;
pub use crate::error::{PlanError, PlanResult, PlanToolConfigError};
pub use crate::file_store::FilePlanStore;
pub use crate::ids::{ContractRef, PathOrSymbol, PlanId, RevisionId, TaskId};
pub use crate::memory_store::InMemoryPlanStore;
pub use crate::store::{PlanRevision, PlanStore};
pub use crate::types::{
    Assignment, Criterion, EvidenceKind, EvidenceRef, InvalidationCondition, Plan, PlanNode,
    PlanNodeKind, PlanNodeStatus, VerificationStep,
};

// Ziel-Verwaltung. `Invariant`, `Constraint`, `ConstraintKind` und `GoalReport`
// bleiben bewusst unter `harw_plan::goal::…` — ihre Namen sind ohne den
// Modulkontext zu allgemein. Die Graph-Funktionen bleiben aus demselben Grund
// unter `harw_plan::graph::…`, damit der Crate-Root nur Typen führt.
pub use crate::goal::{Goal, GoalAction, GoalEvent, GoalId, GoalPatch, GoalStatus, GoalStore};
pub use crate::goal_store::{FileGoalStore, InMemoryGoalStore};

#[cfg(test)]
mod tests {
    use super::actions::PlanAction;
    use super::error::PlanError;
    use super::ids::{PathOrSymbol, PlanId, RevisionId, TaskId};
    use super::types::{Plan, PlanNode, PlanNodeKind, PlanNodeStatus};
    use super::validate::validate;
    use time::OffsetDateTime;

    fn plan_with_nodes(nodes: Vec<PlanNode>) -> Plan {
        Plan {
            id: PlanId::new("p-invariants"),
            revision: RevisionId::new(1),
            parent_revision: None,
            goal_statement: "enforce plan invariants".to_owned(),
            goal_id: None,
            nodes,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn node(id: &str, status: PlanNodeStatus) -> PlanNode {
        PlanNode {
            id: TaskId::new(id),
            objective: "test objective".to_owned(),
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
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    /// Die Re-Exports im Crate-Root müssen auf dieselben Typen zeigen wie die
    /// Modulpfade — sonst zerfällt die Fassade in zwei inkompatible APIs.
    #[test]
    fn crate_root_reexports_point_at_the_module_types() {
        let root: crate::PlanNode = node("t-reexport", PlanNodeStatus::Draft);
        let via_module: crate::types::PlanNode = root;
        assert_eq!(via_module.id, crate::TaskId::new("t-reexport"));
        assert_eq!(via_module.kind, crate::PlanNodeKind::Coding);

        let patch: crate::NodePatch = crate::NodePatch::default();
        assert!(patch.is_empty());

        let error: crate::PlanError = crate::PlanError::PlanExists {
            id: crate::PlanId::new("p-1"),
        };
        assert!(error.to_string().contains("p-1"));
    }

    #[test]
    fn add_node_rejects_duplicate_id_with_typed_error() {
        let plan = plan_with_nodes(vec![node("task-1", PlanNodeStatus::Draft)]);
        let action = PlanAction::AddNode {
            node: node("task-1", PlanNodeStatus::Draft),
        };

        assert!(matches!(
            validate(&plan, &action),
            Err(PlanError::DuplicateNode { .. })
        ));
    }

    #[test]
    fn add_node_rejects_absent_dependency_with_typed_error() {
        let mut child = node("child", PlanNodeStatus::Draft);
        child.dependencies.push(TaskId::new("missing"));

        assert!(matches!(
            validate(&plan_with_nodes(Vec::new()), &PlanAction::AddNode { node: child }),
            Err(PlanError::NodeMissing { id }) if id == TaskId::new("missing")
        ));
    }

    #[test]
    fn status_change_rejects_unfinished_dependency_for_ready_and_in_progress() {
        for (current_status, requested_status) in [
            (PlanNodeStatus::Draft, PlanNodeStatus::Ready),
            (PlanNodeStatus::Ready, PlanNodeStatus::InProgress),
        ] {
            let dependency = node("dependency", PlanNodeStatus::InProgress);
            let mut child = node("child", current_status);
            child.dependencies.push(TaskId::new("dependency"));
            let action = PlanAction::SetStatus {
                id: child.id.clone(),
                status: requested_status,
                reason: None,
            };

            assert!(matches!(
                validate(&plan_with_nodes(vec![dependency, child]), &action),
                Err(PlanError::DependencyNotCompleted { .. })
            ));
        }
    }
}
