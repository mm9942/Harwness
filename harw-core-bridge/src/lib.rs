//! `harw-core-bridge` — Brücke zwischen `harw-operations` und `harw-core`.
//!
//! # Verantwortungsbereich
//! Dieses Crate hostet alle Symbole, die eine echte Kopplung zwischen dem
//! allgemeinen Operation-SDK (`harw-operations`) und der konkreten Core-Runtime
//! (`harw-core`) haben. Dadurch bleibt `harw-operations` core-frei.
//!
//! Enthalten:
//! - [`OpContextCoreExt`] — Extension-Trait mit `managed_spawner()` und
//!   `state_store()` (früher direkt auf `OpContext`).
//! - [`AgentToolAdapter`] — Adapter, der `Surface::AgentTool` gegen einen
//!   `ManagedAgentSpawner` fährt.
//! - [`AgentProductAdapter`] — die `/agent`-Produktfläche (list/stop/budget).
//! - [`ChildReturnContract`] — wie das Ergebnis eines Kind-Agenten ausgewertet
//!   wird (Freitext, `ResearchFinding`, `ReturnEnvelope`, seit AW6-02
//!   `SecurityVerdict`).
//! - [`parse_budget_hint`] / [`tighten_budget`] — Typisierung und monotone
//!   Verschärfung von Agent-Budgets.
//! - [`fanout_children`] — nebenläufiger Fan-out gleichartiger Kinder als
//!   Grundlage für `/analyze` und den Explore-Fan-out.
//! - [`DelegateWaveOperation`] / [`delegate_wave`] — die Fan-out-Operation
//!   der Orchestratoren (`delegate_wave`): eine Welle von Kind-Agenten mit
//!   Ziel-Admission ([`admit_targets`]) und Budget-Deckel
//!   ([`wave_budget_cap`]).
//! - [`AgentResultOperation`] — `agent.result`: der ungekürzte Antworttext
//!   eines eigenen, abgeschlossenen Kind-Laufs, optional seitenweise
//!   (Runde 5, Teil H).
//! - [`AgentStatusOperation`] / [`AgentCancelOperation`] — `agent.status`
//!   (lesend) und `agent.cancel` (immer mit Freigabe) für die eigenen
//!   Hintergrund-Agenten der TUI-Wurzel (Runde 5, Teil K).
//!
//! # Nebenläufigkeit
//! Alle exportierten Typen sind `Send + Sync`, sofern die Trait-Implementierung
//! dies zulässt.

// Runde 5, Teil K: `agent.status`/`agent.cancel` (Hintergrund-Agenten).
mod agent_background;
// Runde 5, Teil K: `agent.status` (eigene Erweiterungsstelle `status_entry`).
mod agent_status;
// Runde 5, Teil M: `agent.message`/`parent.message` und Journal-Felder für
// `agent.status`.
mod agent_messaging;
// Runde 5, Teil H: `agent.result` (ungekürzter Text eines eigenen Kind-Laufs).
mod agent_result;
mod agent_tool;
mod context_ext;
mod delegate_wave;
// #22 Welle 1B: `[return] validators` der Agent-IR v2.
pub mod return_validators;

pub use agent_tool::{
    AgentProductAdapter, AgentToolAdapter, ChildReturnContract, UnknownReturnContract,
    fanout_children, parse_budget_hint, tighten_budget,
};
// Runde 5, Teil K.
pub use agent_background::{
    AGENT_BACKGROUND_TOOLS, AGENT_CANCEL_TOOL, AGENT_STATUS_TOOL, AgentCancelOperation,
    agent_cancel,
};
pub use agent_status::{AgentStatusOperation, agent_status, status_entry, status_line};
// Runde 5, Teil M.
pub use agent_messaging::{
    AGENT_MESSAGING_TOOLS, AgentMessageOperation, AgentMessageRequest, ParentMessageOperation,
    ParentMessageRequest, STATUS_RECENT_ENTRIES, agent_message, journal_status_fields,
    journal_status_line, parent_message,
};
pub use harw_core::child_comms::{AGENT_MESSAGE_TOOL, PARENT_MESSAGE_TOOL};
// Runde 5, Teil H.
pub use agent_result::{
    AGENT_RESULT_MAX_PAGE_BYTES, AgentResultOperation, AgentResultPage, AgentResultPart,
    AgentResultRequest, agent_result, page_of,
};
pub use context_ext::OpContextCoreExt;
pub use delegate_wave::{
    DEFAULT_MAX_PARALLEL, DELEGATE_WAVE_TOOL, DeclaredTargets, DelegateWaveOperation,
    DelegateWavePolicy, DelegateWaveReport, DelegateWaveRequest, MAX_WAVE_TARGETS, ReducerForRole,
    TargetReport, TargetStatus, TargetsForCaller, WaveComplexity, WaveJoin, WaveTarget,
    admit_targets, delegable_roles, delegate_wave, wave_budget_cap,
};
pub use harw_core::child_controller::AGENT_RESULT_TOOL;

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
