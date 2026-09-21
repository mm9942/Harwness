//! Fehlertypen für `harw-core`.
//!
//! `CoreResult<T>` wird vom `HarwError`-Derive miterzeugt.

use harw_macros::HarwError;

#[derive(Debug, HarwError)]
pub enum CoreError {
    #[msg("session {session_id} is not idle (current state: {state})")]
    NotIdle { session_id: String, state: String },

    /// Ein Kind-Agent hat eine Budget-Dimension überschritten.
    ///
    /// # Auslöser
    /// `ManagedAgentSpawner::run_child_with_budget` nach Abschluss bzw. Ablauf
    /// des Kind-Turns. `dimension` ist `"tokens"`, `"tool_calls"` oder
    /// `"wall_time"`; `limit` und `used` tragen die Zahlen in der Einheit der
    /// Dimension (Tokens, Aufrufe, Millisekunden).
    #[msg("budget exceeded: {dimension} (limit={limit}, used={used})")]
    BudgetExceeded {
        dimension: String,
        limit: u64,
        used: u64,
    },

    /// Ein Kind hat pausiert, obwohl seine Lebenszyklus-Policy das verbietet.
    ///
    /// # Auslöser
    /// `allow_pause = false` in der Agent-IR (read-only Kinder dürfen nie auf
    /// eine Rückfrage warten), der Turn liefert aber `AwaitingApproval` oder
    /// `AwaitingChild`. Fail-closed: das Ergebnis wird verworfen.
    #[msg("child {child} paused ({outcome}) but its lifecycle forbids pausing")]
    ChildPauseForbidden { child: String, outcome: String },

    #[msg("session {0} not found")]
    SessionNotFound(String),

    #[msg("turn rejected: {0}")]
    TurnRejected(String),

    #[msg("handoff to '{role}' failed: {reason}")]
    HandoffFailed { role: String, reason: String },

    #[msg("MCP '{name}' launch rejected: {reason}")]
    McpLaunchRejected { name: String, reason: String },

    #[msg("tool execution failed: {0}")]
    ToolFailed(String),

    #[msg("tool catalog contains duplicate tool name '{name}'")]
    DuplicateTool { name: String },

    #[msg("session {session_id} attempted tool execution without a trusted sandbox context")]
    MissingToolExecutionContext { session_id: String },

    #[msg("session {session_id} requires a trusted approval actor before a tool can pause")]
    MissingApprovalActor { session_id: String },

    #[msg("approval actor does not match the pending approval for session {session_id}")]
    ApprovalActorMismatch { session_id: String },

    #[from]
    Extension(harw_extension_api::ExtensionError),

    #[from]
    Tools(harw_tools::ToolsError),

    #[from]
    SessionStore(harw_session_store::SessionStoreError),

    #[from]
    Model(crate::model::ModelError),

    #[from]
    SerdeJson(serde_json::Error),
}
