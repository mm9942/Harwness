//! Common re-exports. `use harw::prelude::*;` should be enough for typical
//! consumers to start building a harness.
//!
//! Every name below is imported individually rather than via `pub use
//! module::*;`. Several source crates converge into this single flat
//! namespace, so a glob import would silently hide a name collision (e.g. two
//! crates exporting `Error` or `Config`) behind an ambiguity error at the
//! call site instead of here. Naming each item lets the compiler reject a
//! collision exactly where it originates — see the `#[cfg(test)] mod tests`
//! below for a test that names every entry.

pub use crate::agent::ExecutableAgentIr;
pub use crate::core::{AgentSession, InteractionMode};
pub use crate::extension::ExtensionRegistry;
pub use crate::ops::registry::OperationRegistry;
pub use crate::types::{AgentRole, ModelId, ModelName, ProviderId, ProviderName};

// Planning-Laufzeit: typisierter Plan-Graph und Ziele (`harw-plan`).
pub use crate::plan::{
    Goal, GoalAction, GoalStatus, GoalStore, Plan, PlanAction, PlanId, PlanNode, PlanNodeKind,
    PlanStore, TaskId,
};

// Recherche-Ergebnisse für read-only Sub-Agenten (`harw-research`).
pub use crate::research::{ResearchFinding, ResearchQuestion};

// Workspace-Abhängigkeitsgraph (`harw-code-graph`).
pub use crate::code_graph::WorkspaceGraph;

// Berechtigungen und eingefrorene Sandbox-Spezifikation (`harw-sandbox`).
pub use crate::sandbox::{Permission, SandboxSpec};

#[cfg(test)]
mod tests {
    //! Ein Test, der jeden Prelude-Namen einzeln benennt. Exportieren zwei
    //! re-exportierte Crates künftig denselben Namen, schlägt bereits der
    //! `pub use`-Block oben mit "defined multiple times" fehl; dieser Test
    //! ist die zweite Absicherung — er schlägt fehl, sobald ein Name aus dem
    //! Prelude verschwindet oder umbenannt wird, ohne dass hier nachgezogen
    //! wurde (AP W5-09).
    use super::{
        AgentRole, AgentSession, ExecutableAgentIr, ExtensionRegistry, Goal, GoalAction,
        GoalStatus, GoalStore, InteractionMode, ModelId, ModelName, OperationRegistry, Permission,
        Plan, PlanAction, PlanId, PlanNode, PlanNodeKind, PlanStore, ProviderId, ProviderName,
        ResearchFinding, ResearchQuestion, SandboxSpec, TaskId, WorkspaceGraph,
    };

    #[test]
    fn every_prelude_name_is_reachable_and_distinct() {
        let _: Option<AgentRole> = None;
        let _: Option<AgentSession> = None;
        let _: Option<ExecutableAgentIr> = None;
        let _: Option<ExtensionRegistry> = None;
        let _: Option<Goal> = None;
        let _: Option<GoalAction> = None;
        let _: Option<GoalStatus> = None;
        let _: Option<InteractionMode> = None;
        let _: Option<ModelId> = None;
        let _: Option<ModelName> = None;
        let _: Option<OperationRegistry> = None;
        let _: Option<Permission> = None;
        let _: Option<Plan> = None;
        let _: Option<PlanAction> = None;
        let _: Option<PlanId> = None;
        let _: Option<PlanNode> = None;
        let _: Option<PlanNodeKind> = None;
        let _: Option<ProviderId> = None;
        let _: Option<ProviderName> = None;
        let _: Option<ResearchFinding> = None;
        let _: Option<ResearchQuestion> = None;
        let _: Option<SandboxSpec> = None;
        let _: Option<TaskId> = None;
        let _: Option<WorkspaceGraph> = None;

        // `GoalStore` und `PlanStore` sind Traits, keine instanzierbaren
        // Typen — ihre Erreichbarkeit wird per Trait-Bound geprüft statt per
        // `Option<T>`.
        #[allow(dead_code)]
        fn requires_plan_store<T: PlanStore>() {}
        #[allow(dead_code)]
        fn requires_goal_store<T: GoalStore>() {}
    }
}
