//! # HARW SDK — canonical entry point
//!
//! `harw` is the small facade crate for building, configuring and running a
//! Harwness runtime. It re-exports the canonical types from the underlying
//! crates so external users depend on a single package.
//!
//! ## What lives where
//!
//! - Extension registry, contributors: [`extension`]
//! - Operations (built-ins + custom): [`ops`]
//! - Agent DSL, resolution, executable IR: [`agent`]
//! - Providers, models, catalog: [`provider`], [`model`]
//! - Core session and runtime: [`core`]
//! - Typed plan graph, goals, stores: [`plan`]
//! - Plan controller, cells, job bridge, finding store: [`plan_bridge`]
//! - Research questions, findings, return envelope: [`research`]
//! - Workspace graph, lockfile, registry sources: [`code_graph`]
//! - Permissions, sandbox spec, network scope: [`sandbox`]
//! - A convenience [`prelude`] with the most common imports
//!
//! ## Stability
//!
//! Version `0.x` — public API may change until `1.0.0`.

pub mod extension {
    //! Re-exports of `harw-extension-api`.
    pub use harw_extension_api::*;
}

pub mod ops {
    //! Re-exports of `harw-operations` and `harw-ops`.
    pub use harw_operations::*;
    pub mod builtins {
        //! Re-exports of `harw-ops` (built-in operation pack).
        pub use harw_ops::*;
    }
}

pub mod agent {
    //! Re-exports of `harw-agent-dsl`.
    pub use harw_agent_dsl::*;
}

pub mod provider {
    //! Re-exports of `harw-provider`.
    pub use harw_provider::*;
}

pub mod model {
    //! Re-exports of `harw-model-catalog`.
    pub use harw_model_catalog::*;
}

pub mod core {
    //! Re-exports of `harw-core`.
    pub use harw_core::*;
}

pub mod defaults {
    //! Re-exports of `harw-registry-defaults`.
    pub use harw_registry_defaults::*;
}

pub mod types {
    //! Re-exports of `harw-types`.
    pub use harw_types::*;
}

pub mod plan {
    //! Re-exports of `harw-plan` (typisierter Plan-Graph, Ziele, Stores).
    pub use harw_plan::*;
}

pub mod plan_bridge {
    //! Re-exports of `harw-plan-bridge` (Controller, Zellen, Job-Brücke, FindingStore).
    pub use harw_plan_bridge::*;
}

pub mod research {
    //! Re-exports of `harw-research` (Forschungsfragen, Findings, Rückgabe-Umschlag).
    pub use harw_research::*;
}

pub mod code_graph {
    //! Re-exports of `harw-code-graph` (Workspace-Graph, Lockfile, Registry-Quellen).
    pub use harw_code_graph::*;
}

pub mod sandbox {
    //! Re-exports of sandbox backends and their authority contract.
    pub use harw_authority::{Permission, SandboxSpec};
    pub use harw_sandbox::*;
}

pub mod prelude;

#[cfg(test)]
mod tests {
    //! Erreichbarkeitstests: ein Typ pro neuem Modul, benannt über den
    //! Fassadenpfad (`crate::<modul>::<Typ>`). Diese Tests sind billig und
    //! fangen genau den Fehler, den diese Datei machen kann — einen
    //! Modulpfad, der nicht existiert (AP W5-09).

    #[test]
    fn plan_module_is_reachable() {
        let _: Option<crate::plan::TaskId> = None;
    }

    #[test]
    fn plan_bridge_module_is_reachable() {
        let _: Option<crate::plan_bridge::PlanController> = None;
    }

    #[test]
    fn research_module_is_reachable() {
        let _: Option<crate::research::ResearchFinding> = None;
    }

    #[test]
    fn code_graph_module_is_reachable() {
        let _: Option<crate::code_graph::WorkspaceGraph> = None;
    }

    #[test]
    fn sandbox_module_is_reachable() {
        let _: Option<crate::sandbox::SandboxSpec> = None;
    }
}
