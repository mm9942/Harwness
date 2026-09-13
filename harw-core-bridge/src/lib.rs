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
//!
//! # Nebenläufigkeit
//! Alle exportierten Typen sind `Send + Sync`, sofern die Trait-Implementierung
//! dies zulässt.

mod agent_tool;
mod context_ext;

pub use agent_tool::{
    AgentProductAdapter, AgentToolAdapter, ChildReturnContract, fanout_children, parse_budget_hint,
    tighten_budget,
};
pub use context_ext::OpContextCoreExt;
