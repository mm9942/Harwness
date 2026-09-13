//! Tool-Ausgaben, wie sie an das Modell zurückgereicht werden.

use serde::Serialize;

/// Ergebnis einer Tool-Ausführung.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum ToolOutput {
    #[serde(rename = "text")]
    Text { content: String },
    #[serde(rename = "json")]
    Json { content: serde_json::Value },
    #[serde(rename = "error")]
    Error { message: String },
}

impl ToolOutput {
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text { content: s.into() }
    }

    pub fn json(v: serde_json::Value) -> Self {
        Self::Json { content: v }
    }

    pub fn error(s: impl Into<String>) -> Self {
        Self::Error { message: s.into() }
    }
}
