//! Gemeinsame Runtime-Montage (`RuntimeAssembly`) für alle `harw`-Einstiege.
//!
//! Verträge (`spec`, `error`) und Montage folgen `docs/design/runtime-contracts.md` §runtime.
#![forbid(unsafe_code)]

// Runde 5, Teil K: Hintergrund-Agenten (`agent.status`/`agent.cancel`, `[agents]`).
pub mod agent_background_wiring;
// Async-by-default delegation: durable agent work lives in the shared job ledger.
pub mod agent_job_wiring;
// Runde 5, Teil M: Montage von `agent.message`/`parent.message`.
pub mod agent_messaging_wiring;
// Runde 5, Teil H: Montage von `agent.result`.
pub mod agent_result_wiring;
pub mod approval;
pub mod assembly;
// Runde 5, Teil E: Auto-Modus (Vorfilter, Klassifizierer, Deckel).
pub mod auto_classifier;
pub mod budget;
pub mod ceiling;
pub mod children;
pub mod config;
pub mod contributors;
pub mod diary_wiring;
pub mod dream_run;
// #22 Welle 3A: eingebettete Agenten-Artefakte (`EntryKind::CompiledAgent`).
pub mod embedded;
pub mod error;
pub mod guard_wiring;
pub mod handoff;
// Runde 5, Teil N: Host-Mode-Anfrage aus dem Orchestrator-Baum.
pub mod host_escalation_wiring;
// Crypto-Infrastruktur H4: Infrastruktur-Clients aus `[infrastructure]`.
pub mod infrastructure;
pub mod job_ledger;
// Plan R9, Teil F: Job-Verwaltung der Sitzung und Zustellung ihrer Ereignisse.
pub mod job_wiring;
// Live-Modellwechsel: Kinder, Rollenwahl und Provider-Neubau ohne Neustart.
pub mod live_model;
pub mod mcp_wiring;
pub mod memory_wiring;
pub mod model;
// Runde 5, Teil E: Lernen aus Freigaben.
pub mod container_wiring;
pub mod permission_rules;
pub mod sandbox;
pub mod security_signals;
pub mod services;
pub mod session_title;
pub mod spec;
pub mod task_context;
pub mod trace;
// Runde 5, Teil G: eigene Modellwahl je UIA-Worker-Rolle zur Laufzeit.
pub mod uia_worker_routing;

#[cfg(test)]
mod test_support;

pub use approval::ApprovalChain;
pub use assembly::{
    RootSession, RuntimeAssembly, RuntimeAssemblyBuilder, RuntimeNarrowing, RuntimeStores,
    SessionLifecycleHook, TurnLimits, default_approval_mode,
};
// Runde 5, Teil E.
pub use auto_classifier::{AutoModeHandle, ModelClassifierBackend, PrefilterContext};
pub use budget::child_limits;
pub use ceiling::root_ceiling;
pub use children::RuntimeChildRegistryFactory;
pub use config::{
    ConfigTrustReport, load_config, load_config_embedded, load_config_embedded_with_agents,
    load_config_with_agents,
};
pub use contributors::{
    AssemblyContributor, AssemblyInputs, AssemblyParts, GatewayContributor,
    GatewayDiagnosticsContributor, InfrastructureContributor, default_contributors,
};
pub use embedded::{EffectiveRights, EmbeddedAgent, RightsFlags};
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
// Runde 5, Teil E.
pub use permission_rules::{ApprovalLearner, LearnKey, LearnOffer};
pub use sandbox::{permissions_for_tier, plan_node_sandbox, root_sandbox};
pub use services::{PlanServices, RuntimeServices, RuntimeServicesParts, ServiceSurface};
pub use session_title::{
    SessionTitleError, TitleRequest, ensure_title, generate_title, spawn_title_job,
};
pub use spec::{
    AskResolution, CeilingPolicy, ChildBackendHandle, EntryKind, EntryProfile, OperationSurface,
    RightsSnapshot, RootBudget, RuntimeSpec, SpawnerPolicy,
};
pub use trace::new_root_trace;
