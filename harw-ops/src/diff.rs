//! `/diff` — read-only git diff operation.
//!
//! # Responsibility
//! Implements `/diff` as a channel-parity command and read-only model tool. The
//! operation delegates exclusively to `shell.exec` through `ShellToolProvider`;
//! the provider derives its Bubblewrap plan and process authority from the
//! immutable sandbox in [`OpContext`]. It never launches a host process itself.
//!
//! # Security boundary
//! The command is fixed to `git diff --no-ext-diff --no-color`; `--stat` is the
//! only optional git flag. An optional workspace-relative path is single-quoted
//! before it is passed to `/bin/sh -c`, and invalid path syntax is rejected.
//! `ExecuteProcess` remains an executor-enforced sandbox permission: this
//! operation only clones the caller's sandbox and cannot grant capabilities.
//!
//! # Output
//! Successful shell JSON output is rendered deterministically from `stdout` and
//! `stderr`. Executor failures, tool errors, malformed output, and non-zero git
//! exit statuses are translated to [`OpError::Execution`].

use harw_extension_api::contributors::ToolProvider;
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_tool_shell::ShellToolProvider;
use harw_tools::{ToolCall, ToolExecutionContext, ToolOutput, spec::ToolName};
use harw_types::ToolCallId;
use serde_json::{Value, json};

const SHELL_EXEC_TOOL: &str = "shell.exec";

/// Argument container for the `/diff` operation.
#[derive(Default, serde::Deserialize)]
pub struct DiffArgs {
    /// Optional workspace-relative path filter.
    #[serde(default)]
    pub path: Option<String>,
    /// When true, request only `git diff --stat`.
    #[serde(default)]
    pub stat_only: bool,
}

impl harw_operations::FromRawArgs for DiffArgs {
    fn from_raw_args(tokens: &[String]) -> Result<Self, harw_operations::OpError> {
        let mut stat_only = false;
        let mut path = None;
        for token in tokens {
            match token.as_str() {
                "--stat" | "--stat-only" => stat_only = true,
                other if other.starts_with("--") => {
                    return Err(harw_operations::OpError::InvalidArguments(format!(
                        "unbekanntes Flag für /diff: {other}"
                    )));
                }
                other if path.is_none() => path = Some(other.to_owned()),
                other => {
                    return Err(harw_operations::OpError::InvalidArguments(format!(
                        "unerwartetes zusätzliches Argument für /diff: {other}"
                    )));
                }
            }
        }
        Ok(Self { stat_only, path })
    }
}

/// Build the fixed shell command used by `/diff`.
fn build_diff_command(args: &DiffArgs) -> Result<String, OpError> {
    let mut command = String::from("git diff --no-ext-diff --no-color");
    if args.stat_only {
        command.push_str(" --stat");
    }
    if let Some(path) = &args.path {
        command.push_str(" -- ");
        command.push_str(&shell_quote_path(path)?);
    }
    Ok(command)
}

/// Validate and POSIX-shell quote a workspace-relative git pathspec.
fn shell_quote_path(path: &str) -> Result<String, OpError> {
    if path.is_empty()
        || path.contains('\0')
        || path.starts_with('/')
        || path.split('/').any(|component| component == "..")
    {
        return Err(OpError::InvalidArguments(
            "/diff path must be a non-empty workspace-relative path without `..`".to_owned(),
        ));
    }

    Ok(format!("'{}'", path.replace('\'', "'\\''")))
}

fn shell_call(command: String) -> ToolCall {
    ToolCall {
        id: ToolCallId::new(),
        name: ToolName::new(SHELL_EXEC_TOOL),
        arguments: json!({ "command": command }),
    }
}

fn render_shell_output(content: Value) -> Result<String, OpError> {
    let object = content.as_object().ok_or_else(|| {
        OpError::Execution("shell.exec returned JSON that was not an object".to_owned())
    })?;
    let exit_code = object
        .get("exit_code")
        .and_then(Value::as_i64)
        .ok_or_else(|| {
            OpError::Execution("shell.exec JSON did not contain an integer exit_code".to_owned())
        })?;
    let stdout = object
        .get("stdout")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            OpError::Execution("shell.exec JSON did not contain string stdout".to_owned())
        })?;
    let stderr = object
        .get("stderr")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            OpError::Execution("shell.exec JSON did not contain string stderr".to_owned())
        })?;

    if exit_code != 0 {
        return Err(OpError::Execution(format!(
            "git diff exited with status {exit_code}"
        )));
    }

    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => Ok("git diff produced no output.".to_owned()),
        (false, true) => Ok(stdout.to_owned()),
        (true, false) => Ok(format!("stderr:\n{stderr}")),
        (false, false) => Ok(format!("{stdout}\nstderr:\n{stderr}")),
    }
}

/// Execute the current workspace's read-only git diff through the sandboxed
/// shell tool. The cloned sandbox preserves, rather than expands, authority.
#[operation(
    name = "diff",
    summary = "Zeigt den aktuellen git-Diff des Workspaces.",
    domain = "knowledge",
    permission = "observer",
    command(path = "/diff", visibility = "channel_parity"),
    model_tool(readonly, approval = "none"),
    // Web-Fläche übernimmt dieselbe Achse wie das ModelTool: `git diff` ist
    // ein reiner Lesevorgang auf dem Workspace, keine Mutation.
    web(path = "/api/diff", readonly, approval = "none")
)]
async fn diff(ctx: &OpContext, args: DiffArgs) -> Result<OpOutput, OpError> {
    let command = build_diff_command(&args)?;
    let execution_context = ToolExecutionContext::new(
        ctx.session_id().clone(),
        ctx.turn_id().clone(),
        ctx.sandbox().clone(),
    );
    let provider = ShellToolProvider::new();
    let tool_name = ToolName::new(SHELL_EXEC_TOOL);
    let executor = provider
        .executor(&tool_name)
        .ok_or_else(|| OpError::Execution("shell.exec executor is unavailable".to_owned()))?;
    let call = shell_call(command);

    let output = executor
        .execute(&execution_context, &call)
        .await
        .map_err(|error| OpError::Execution(format!("shell.exec executor failed: {error}")))?;

    let text = match output {
        ToolOutput::Json { content } => render_shell_output(content)?,
        ToolOutput::Error { message } => {
            return Err(OpError::Execution(format!(
                "shell.exec rejected git diff: {message}"
            )));
        }
        ToolOutput::Text { .. } => {
            return Err(OpError::Execution(
                "shell.exec returned an unexpected text output".to_owned(),
            ));
        }
    };

    Ok(OpOutput { text })
}

#[cfg(test)]
mod tests {
    use super::{DiffArgs, build_diff_command, diff, render_shell_output, shell_quote_path};
    use crate::testutil::toks;
    use harw_operations::{FromRawArgs, OpContext, OpError, context::ServiceMap};
    use harw_sandbox::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    fn test_context() -> (OpContext, PathBuf) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("harw-diff-test-{}-{id}", std::process::id()));
        std::fs::create_dir_all(root.join("workspace")).expect("create test workspace");
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )
        .expect("build workspace registry");
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("workspace"),
            )
            .expect("resolve workspace binding");
        (
            OpContext::new(
                SessionId::new(),
                TurnId::new(),
                SandboxSpec::from_resolved(binding, PermissionSet::empty()),
                ServiceMap::new(),
            ),
            root,
        )
    }

    #[test]
    fn test_diff_args_from_raw_args_stat_flag_only() {
        let args = DiffArgs::from_raw_args(&toks(&["--stat"])).expect("parse args");
        assert!(args.stat_only);
        assert!(args.path.is_none());
    }

    #[test]
    fn test_diff_args_from_raw_args_stat_flag_and_path() {
        let args = DiffArgs::from_raw_args(&toks(&["--stat", "src"])).expect("parse args");
        assert!(args.stat_only);
        assert_eq!(args.path.as_deref(), Some("src"));
    }

    #[test]
    fn test_diff_args_from_raw_args_no_tokens_defaults() {
        let args = DiffArgs::from_raw_args(&toks(&[])).expect("parse args");
        assert!(!args.stat_only);
        assert!(args.path.is_none());
    }

    #[test]
    fn test_diff_args_from_raw_args_unknown_flag_returns_err() {
        assert!(matches!(
            DiffArgs::from_raw_args(&toks(&["--bogus"])),
            Err(OpError::InvalidArguments(_))
        ));
    }

    #[test]
    fn test_diff_args_from_raw_args_extra_token_after_path_returns_err() {
        assert!(matches!(
            DiffArgs::from_raw_args(&toks(&["src", "extra"])),
            Err(OpError::InvalidArguments(_))
        ));
    }

    #[test]
    fn test_build_diff_command_is_fixed_and_quotes_malicious_path() {
        let command = build_diff_command(&DiffArgs {
            path: Some("src/'; touch owned; echo '".to_owned()),
            stat_only: true,
        })
        .expect("build command");
        assert_eq!(
            command,
            "git diff --no-ext-diff --no-color --stat -- 'src/'\\''; touch owned; echo '\\'''"
        );
    }

    #[test]
    fn test_shell_quote_path_rejects_workspace_escape() {
        assert!(matches!(
            shell_quote_path("../secrets"),
            Err(OpError::InvalidArguments(_))
        ));
    }

    #[test]
    fn test_nonzero_git_status_does_not_echo_diff_output() {
        let error = render_shell_output(serde_json::json!({
            "exit_code": 1,
            "stdout": "sensitive diff content",
            "stderr": "sensitive diagnostic",
        }))
        .expect_err("nonzero git status must fail");

        match error {
            OpError::Execution(message) => {
                assert_eq!(message, "git diff exited with status 1");
                assert!(!message.contains("sensitive"));
            }
            other => panic!("expected execution error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_diff_without_execute_process_fails_before_process_invocation() {
        let (ctx, root) = test_context();
        let result = diff(&ctx, DiffArgs::default()).await;
        std::fs::remove_dir_all(root).expect("remove test workspace");

        assert!(
            matches!(result, Err(OpError::Execution(message)) if message.contains("ExecuteProcess permission missing"))
        );
    }
}
