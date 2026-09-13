//! Tool-Invokationen, wie sie vom Modell angefordert werden.

use crate::spec::ToolName;
use harw_types::ToolCallId;
use serde::{Deserialize, Serialize};

/// Eine konkrete Tool-Invokation: ID, Name und JSON-Argumente.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: ToolCallId,
    pub name: ToolName,
    pub arguments: serde_json::Value,
}
