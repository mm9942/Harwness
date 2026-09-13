//! Geteilte Typen für die Extension-API.

use harw_types::{SessionId, TurnId};
use serde::{Deserialize, Serialize};

/// Kontext der dem Turn mitgegeben wird.
#[derive(Debug, Clone, Default)]
pub struct TurnInputContext {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub metadata: serde_json::Value,
}

/// Fragment das ein ContextProvider liefert.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextFragment {
    pub label: String,
    pub content: String,
}

/// Geladene Instructions.
#[derive(Debug, Clone, Default)]
pub struct LoadedInstructions {
    pub system_prompt: String,
    pub fragments: Vec<String>,
}

/// Input für Turn-Observer.
#[derive(Debug, Clone)]
pub struct TurnStartInput {
    pub session_id: SessionId,
    pub turn_id: TurnId,
}

#[derive(Debug, Clone)]
pub struct TurnStopInput {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub token_usage: harw_types::TokenUsage,
}
