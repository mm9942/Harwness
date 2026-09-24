//! Versioned, user-safe lifecycle records for the agent orchestration tree.

use harw_types::{SessionId, TokenUsage, TurnId};
use serde::{Deserialize, Serialize};

/// Stable lifecycle states exposed to UI and durable consumers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentOrchestrationStatus {
    Admitted,
    Running,
    Progress,
    Paused,
    Completed,
    Failed,
    Cancelled,
}

/// One append-only, versioned observation of a parent/child orchestration.
///
/// This is deliberately separate from child chat/turn events: consumers get
/// topology and bounded status without receiving the child's private history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentOrchestrationEvent {
    pub schema_version: u16,
    pub event_id: String,
    pub root_session_id: SessionId,
    pub parent_session_id: SessionId,
    pub child_session_id: SessionId,
    pub turn_id: Option<TurnId>,
    pub role: String,
    pub depth: u32,
    pub task: Option<String>,
    pub status: AgentOrchestrationStatus,
    pub usage: Option<TokenUsage>,
    pub duration_ms: Option<u64>,
    pub progress: Option<u8>,
    pub detail: Option<String>,
    /// Bisher abgeschlossene Tool-Aufrufe des Kindes (Live-Fortschritt).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<u32>,
    /// Das vom Kind angesprochene Modell (gepinntes Kind-Modell, sonst das
    /// `active_model` der Kind-Session); `None`, wenn unbekannt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Der vom Kind angesprochene Provider (Anzeige `<provider>/<modell>`);
    /// `None`, wenn unbekannt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

impl AgentOrchestrationEvent {
    pub const CURRENT_SCHEMA_VERSION: u16 = 1;
    pub const MAX_DETAIL_CHARS: usize = 512;

    #[must_use]
    pub fn bounded_detail(detail: Option<String>) -> Option<String> {
        detail.map(|value| value.chars().take(Self::MAX_DETAIL_CHARS).collect())
    }
}
