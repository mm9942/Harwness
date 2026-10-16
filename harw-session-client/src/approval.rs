//! An approval request as a client shows it: a tool and its arguments.
//!
//! The wire form ([`ApprovalRequest`]) distinguishes an exec command, a patch
//! and a dynamic tool call. A client wants one shape. [`describe`] gives it,
//! with stable tool names for the first two (`shell.exec`, `fs.patch`), and
//! never passes on the **bodies** of a patch: only the file names. A diff can
//! be large and carry secrets, and approving does not need it in a prompt.

use harw_protocol::{ApprovalKind, ApprovalRequest};

/// What an approval request is about.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalSummary {
    /// The request id (stable for this pause).
    pub request_id: String,
    /// The governance handle of the request. The host does not know the
    /// runtime's call id, so this carries the work id.
    pub call_id: String,
    /// Tool name: the dynamic tool's own, `shell.exec` or `fs.patch`.
    pub tool: String,
    /// Arguments as JSON.
    pub arguments: serde_json::Value,
}

/// Describes `request`.
#[must_use]
pub fn describe(request: &ApprovalRequest) -> ApprovalSummary {
    let (tool, arguments) = match &request.kind {
        ApprovalKind::DynamicTool {
            tool_name,
            arguments,
            ..
        } => (tool_name.clone(), arguments.clone()),
        ApprovalKind::Exec {
            command,
            cwd,
            reasoning,
            ..
        } => (
            "shell.exec".to_owned(),
            serde_json::json!({ "command": command, "cwd": cwd, "reasoning": reasoning }),
        ),
        ApprovalKind::Patch { changes, .. } => {
            let mut files: Vec<&String> = changes.keys().collect();
            files.sort();
            ("fs.patch".to_owned(), serde_json::json!({ "files": files }))
        }
    };
    ApprovalSummary {
        request_id: request.id.as_str().to_owned(),
        call_id: request.work_id.as_str().to_owned(),
        tool,
        arguments,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ensure};
    use harw_types::{ApprovalId, ReviewDecision, RiskLevel, TurnId, WorkId};

    fn wire(kind: ApprovalKind) -> ApprovalRequest {
        let now = jiff::Timestamp::UNIX_EPOCH;
        ApprovalRequest {
            id: ApprovalId::from_str("a1"),
            work_id: WorkId::from_str("w1"),
            kind,
            summary: "x".to_owned(),
            risk: RiskLevel::Low,
            requested_at: now,
            timeout_at: ApprovalRequest::default_timeout_at(now),
            decisions: vec![ReviewDecision::Approved, ReviewDecision::Rejected],
        }
    }

    #[test]
    fn a_dynamic_tool_keeps_its_name_and_arguments() -> TestResult {
        let s = describe(&wire(ApprovalKind::DynamicTool {
            turn_id: TurnId::from_str("t"),
            tool_name: "fs.write".to_owned(),
            arguments: serde_json::json!({"path": "a"}),
        }));
        ensure(s.tool == "fs.write", "name")?;
        ensure(s.arguments == serde_json::json!({"path": "a"}), "arguments")?;
        ensure(
            (s.request_id.as_str(), s.call_id.as_str()) == ("a1", "w1"),
            "ids",
        )
    }

    #[test]
    fn exec_gets_a_stable_tool_name() -> TestResult {
        let s = describe(&wire(ApprovalKind::Exec {
            turn_id: TurnId::from_str("t"),
            command: vec!["ls".to_owned(), "-l".to_owned()],
            cwd: "/w".to_owned(),
            reasoning: None,
        }));
        ensure(s.tool == "shell.exec", "name")?;
        ensure(
            s.arguments["command"] == serde_json::json!(["ls", "-l"]),
            "command",
        )
    }

    #[test]
    fn a_patch_shows_file_names_never_the_diffs() -> TestResult {
        let mut changes = std::collections::HashMap::new();
        changes.insert("b.rs".to_owned(), "SECRET DIFF".to_owned());
        changes.insert("a.rs".to_owned(), "SECRET DIFF".to_owned());
        let s = describe(&wire(ApprovalKind::Patch {
            turn_id: TurnId::from_str("t"),
            changes,
        }));
        ensure(s.tool == "fs.patch", "name")?;
        ensure(
            s.arguments == serde_json::json!({"files": ["a.rs", "b.rs"]}),
            "sorted names",
        )?;
        ensure(!s.arguments.to_string().contains("SECRET"), "no diff body")
    }
}
