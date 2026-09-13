//! Genehmigungs-Protokoll: `ApprovalRequest` (Agent → Client) und
//! `ApprovalResponse` (Client → Agent). `RiskLevel`/`ReviewDecision` stammen
//! aus `harw-types`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use harw_types::{ReviewDecision, RiskLevel, TurnId};

/// Eine genehmigungspflichtige Aktion, die auf Nutzerentscheidung wartet.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields, tag = "type", rename_all = "snake_case")]
pub enum ApprovalRequest {
    /// Exec-Befehl wartet auf Nutzergenehmigung.
    ExecApproval {
        id: String,
        turn_id: TurnId,
        command: Vec<String>,
        cwd: String,
        risk_level: RiskLevel,
        reasoning: Option<String>,
    },
    /// Patch-Anwendung wartet auf Genehmigung.
    PatchApproval {
        id: String,
        turn_id: TurnId,
        /// Dateiname → unified diff.
        changes: HashMap<String, String>,
    },
    /// Generische Tool-Genehmigung für dynamische Tools.
    DynamicToolApproval {
        id: String,
        turn_id: TurnId,
        tool_name: String,
        arguments: serde_json::Value,
    },
}

/// Antwort auf eine `ApprovalRequest`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalResponse {
    pub id: String,
    pub decision: ReviewDecision,
    pub comment: Option<String>,
}
