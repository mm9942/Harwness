//! Gemeinsame Runtime-Montage (`RuntimeAssembly`) für alle `harw`-Einstiege.
//!
//! Gerüst aus Welle W0a. Verträge (`spec`, `error`) liefert Agent W0B-05,
//! die Montage die Wellen W2b/W2c gemäß `docs/remediation/CONTRACTS.md` §runtime.
#![forbid(unsafe_code)]

pub mod approval;
pub mod assembly;
pub mod budget;
pub mod ceiling;
pub mod children;
pub mod config;
pub mod contributors;
pub mod error;
pub mod guard_wiring;
pub mod handoff;
pub mod mcp_wiring;
pub mod memory_wiring;
pub mod model;
pub mod sandbox;
pub mod services;
pub mod session_title;
pub mod spec;
pub mod trace;

#[cfg(test)]
mod test_support;

pub use approval::ApprovalChain;
pub use assembly::{
    RootSession, RuntimeAssembly, RuntimeAssemblyBuilder, RuntimeNarrowing, RuntimeStores,
    SessionLifecycleHook, TurnLimits, default_approval_mode,
};
pub use budget::child_limits;
pub use ceiling::root_ceiling;
pub use children::RuntimeChildRegistryFactory;
pub use config::{ConfigTrustReport, load_config};
pub use contributors::{AssemblyContributor, AssemblyInputs, AssemblyParts, default_contributors};
pub use error::{RuntimeError, RuntimeResult};
pub use guard_wiring::{
    DriftTracer, MemoryPitfallAdvisor, guard_policy_from_config, role_effort_weights_from_config,
    spawn_child_reaper,
};
pub use handoff::{
    HANDOFF_MAX_BYTES, HandoffContextProvider, HandoffWriter, SessionHandoff, handoff_path,
    read_handoff,
};
pub use memory_wiring::{MemoryCaptureObserver, MemoryConsolidationHook, spawn_startup_sweep};
pub use model::{ModelSource, build_root_model, build_root_model_with_resolver};
pub use sandbox::{permissions_for_tier, plan_node_sandbox, root_sandbox};
pub use services::{PlanServices, RuntimeServices, RuntimeServicesParts, ServiceSurface};
pub use session_title::{
    SessionTitleError, TitleRequest, ensure_title, generate_title, spawn_title_job,
};
pub use spec::{
    AskResolution, CeilingPolicy, EntryKind, EntryProfile, OperationSurface, RightsSnapshot,
    RootBudget, RuntimeSpec, SpawnerPolicy,
};
pub use trace::new_root_trace;
