//! `/diff` — read-only git diff operation.
//!
//! # Responsibility
//! Implements `/diff` as a channel-parity command and read-only model tool. The
//! operation delegates exclusively to `shell.exec` through `ShellToolProvider`;
//! the provider derives its Bubblewrap plan and process authority from the
//! immutable sandbox in [`OpContext`]. It never launches a host process itself.
//!
//! # Security boundary
//! `shell.exec` (`harw-tool-shell/src/exec.rs`) has no argv/env parameters of
//! its own — its `ShellExecArgs` accepts exactly one `command` string, run as
//! `/bin/sh -c <command>`. This operation therefore builds a fully-hardened
//! git invocation as a pure [`GitDiffPlan`] (argv + env), then renders it into
//! that single command string with every token POSIX single-quoted:
//!
//! - `git -c core.fsmonitor= -c core.hooksPath=/dev/null -c diff.external=
//!   -c core.pager=cat --no-pager diff --no-textconv --no-ext-diff --no-color`
//!   neutralises the config- and attribute-driven code-execution paths a bare
//!   `git diff` would otherwise honor from repo-local state the sandbox does
//!   not control: `core.fsmonitor`'s hook, `.git/hooks/*` via `hooksPath`,
//!   `diff.external`/`GIT_EXTERNAL_DIFF`, `diff.<driver>.textconv` from
//!   `.gitattributes`, and the pager.
//! - `GIT_CONFIG_NOSYSTEM=1` and `GIT_CONFIG_GLOBAL=/dev/null` stop the
//!   system and global git config — which live outside the sandboxed
//!   workspace and are not covered by the `-c` overrides above — from
//!   reintroducing any of the same hooks. `GIT_TERMINAL_PROMPT=0` stops an
//!   interactive credential prompt from blocking the call until its timeout.
//! - An optional workspace-relative path is validated (no leading `/`, no
//!   `..` component, no leading `:`) and bound as `:(literal)<path>` so git's
//!   pathspec "magic" (`:/`, `:(top)`, `:(glob)`, `:(exclude)`, …) cannot
//!   widen or redirect the diff.
//!
//! `ExecuteProcess` remains an executor-enforced sandbox permission: this
//! operation only clones the caller's sandbox and cannot grant capabilities.
//! Because it still starts a real process under sandbox-derived authority,
//! its `ModelTool` surface requires `approval = "always"` rather than
//! auto-approval — see the `#[operation]` attribute below.
//!
//! # Output
//! Successful shell JSON output is rendered deterministically from `stdout` and
//! `stderr`. Executor failures, tool errors, malformed output, and non-zero git
//! exit statuses (unless the process tree was killed by `shell.exec`'s output
//! limit — see below) are translated to [`OpError::Execution`].
//!
//! `harw-tool-shell` caps combined `stdout`/`stderr` at `max_output_bytes`
//! (64 KiB default) and reports that in the result JSON via `truncated` and,
//! when the cap was hit *while the process was still running* and its tree
//! was killed, `killed_by_output_limit` (with a synthetic `exit_code: -1`).
//! `render_shell_output` treats both as a **truncated success** — the
//! available partial diff plus a trailing "Diff gekürzt bei 64 KiB" note —
//! rather than an error; only a non-zero `exit_code` *without* a kill (a real
//! `git` failure) is still an error.

use harw_extension_api::contributors::ToolProvider;
use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};
use harw_tool_shell::ShellToolProvider;
use harw_tools::{ToolCall, ToolExecutionContext, ToolOutput, spec::ToolName};
use harw_types::ToolCallId;
use serde_json::{Value, json};

const SHELL_EXEC_TOOL: &str = "shell.exec";

/// Fixed argv prefix for the hardened `git diff` invocation. See the module
/// doc for why each `-c` and flag is present; none of them are optional.
const GIT_DIFF_BASE_ARGV: &[&str] = &[
    "git",
    "-c",
    "core.fsmonitor=",
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "diff.external=",
    "-c",
    "core.pager=cat",
    "--no-pager",
    "diff",
    "--no-textconv",
    "--no-ext-diff",
    "--no-color",
];

/// Environment applied only to this git invocation (not the harness's own
/// process environment). See the module doc for rationale.
const GIT_DIFF_ENV: &[(&str, &str)] = &[
    ("GIT_CONFIG_NOSYSTEM", "1"),
    ("GIT_CONFIG_GLOBAL", "/dev/null"),
    ("GIT_TERMINAL_PROMPT", "0"),
];

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

/// A pure, side-effect-free plan for the single `/bin/sh -c` string that
/// `shell.exec` accepts. Kept as a struct — rather than folded straight into
/// a `String` — so tests can assert on the exact argv and env instead of
/// parsing shell-quoting back out of a rendered command line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct GitDiffPlan {
    /// argv in exec order; `argv[0] == "git"`.
    argv: Vec<String>,
    /// `(name, value)` pairs applied only to this invocation.
    env: Vec<(&'static str, &'static str)>,
}

/// Build the [`GitDiffPlan`] for one `/diff` call. Pure function: no I/O, no
/// sandbox access — everything needed comes from `args`.
fn build_git_diff_plan(args: &DiffArgs) -> Result<GitDiffPlan, OpError> {
    let mut argv: Vec<String> = GIT_DIFF_BASE_ARGV
        .iter()
        .map(|token| (*token).to_owned())
        .collect();
    if args.stat_only {
        argv.push("--stat".to_owned());
    }
    if let Some(path) = &args.path {
        let literal = validate_relative_pathspec(path)?;
        argv.push("--".to_owned());
        argv.push(format!(":(literal){literal}"));
    }
    Ok(GitDiffPlan {
        argv,
        env: GIT_DIFF_ENV.to_vec(),
    })
}

/// Validate a workspace-relative git pathspec before it is bound with
/// `:(literal)`. Rejects anything that is not a plain relative path:
/// - a leading `/` (absolute) or a `..` component (parent traversal);
/// - a leading `:` — git's pathspec "magic" prefix (`:/`, `:(top)`,
///   `:(glob)`, `:(exclude)`, the short form `::`, …). `:(literal)` is
///   applied once by [`build_git_diff_plan`], over the validated tail; a
///   caller-supplied leading `:` is rejected outright rather than trusted to
///   be neutralised by nesting inside that wrapper;
/// - an embedded NUL.
fn validate_relative_pathspec(path: &str) -> Result<&str, OpError> {
    if path.is_empty()
        || path.contains('\0')
        || path.starts_with('/')
        || path.starts_with(':')
        || path.split('/').any(|component| component == "..")
    {
        return Err(OpError::InvalidArguments(
            "/diff path must be a non-empty workspace-relative path without `..`, a leading `/`, \
             or a leading `:` (git pathspec magic)"
                .to_owned(),
        ));
    }
    Ok(path)
}

/// Render a [`GitDiffPlan`] into the one `command` string `shell.exec`
/// accepts. Every token — env assignment and argv element alike — is POSIX
/// single-quoted, so correctness never depends on which of the (currently
/// all-static) tokens do or do not contain shell metacharacters.
fn render_shell_command(plan: &GitDiffPlan) -> String {
    let mut parts = Vec::with_capacity(plan.env.len() + plan.argv.len());
    for (name, value) in &plan.env {
        parts.push(format!("{name}={}", shell_quote(value)));
    }
    for token in &plan.argv {
        parts.push(shell_quote(token));
    }
    parts.join(" ")
}

/// POSIX single-quote one shell word: wraps in `'…'`, escaping an embedded
/// `'` as `'\''` (close quote, escaped literal quote, reopen quote).
fn shell_quote(token: &str) -> String {
    format!("'{}'", token.replace('\'', "'\\''"))
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
    // `harw-tool-shell` (W1-03) kappt die kombinierte Ausgabe bei `max_output_bytes`
    // (64 KiB Default, `ShellToolProvider::new()`, siehe `exec.rs` `DEFAULT_MAX_OUTPUT_BYTES`)
    // und tötet den Prozessbaum, wenn die Grenze *während* des Laufs überschritten wird:
    // `killed_by_output_limit == true` dann zusammen mit `exit_code == -1` (kein echter
    // git-Exitstatus, siehe `exec.rs::completed_output`). Ein durch das Byte-Budget nur
    // *nachträglich* gekürztes, aber vollständig gelaufenes `git diff` setzt stattdessen
    // `truncated == true` bei einem echten `exit_code`. Beide Fälle sind ein gekürzter
    // Erfolg, kein Fehler (R1-01/R1-02) — nur ein `exit_code != 0` ohne Kill bleibt ein
    // echter git-Fehler.
    let truncated = object
        .get("truncated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let killed_by_output_limit = object
        .get("killed_by_output_limit")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if exit_code != 0 && !killed_by_output_limit {
        return Err(OpError::Execution(format!(
            "git diff exited with status {exit_code}"
        )));
    }

    let body = match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => "git diff produced no output.".to_owned(),
        (false, true) => stdout.to_owned(),
        (true, false) => format!("stderr:\n{stderr}"),
        (false, false) => format!("{stdout}\nstderr:\n{stderr}"),
    };

    if truncated || killed_by_output_limit {
        Ok(format!(
            "{body}\n[Diff gekürzt bei 64 KiB: shell.exec-Ausgabegrenze erreicht]"
        ))
    } else {
        Ok(body)
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
    // `diff` starts a real process (git, via shell.exec) under the caller's
    // sandbox. Auto-approval let repo-controlled git config/attributes run
    // code without a prompt (F-022/F-191/G-027); every model-initiated call
    // now requires explicit approval even though the git invocation itself
    // is hardened above.
    model_tool(readonly, approval = "always"),
    // Web-Fläche übernimmt dieselbe Achse wie das ModelTool: `git diff` ist
    // ein reiner Lesevorgang auf dem Workspace, keine Mutation.
    web(path = "/api/diff", readonly, approval = "none")
)]
async fn diff(ctx: &OpContext, args: DiffArgs) -> Result<OpOutput, OpError> {
    let plan = build_git_diff_plan(&args)?;
    let command = render_shell_command(&plan);
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
    use super::{
        DiffArgs, DiffOperation, GIT_DIFF_BASE_ARGV, GIT_DIFF_ENV, build_git_diff_plan, diff,
        render_shell_command, render_shell_output, validate_relative_pathspec,
    };
    use crate::testutil::toks;
    use harw_operations::{
        ApprovalPolicy, FromRawArgs, OpContext, OpError, Operation, Surface,
        context::ServiceMap,
    };
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
    fn test_build_git_diff_plan_defaults_have_hardening_argv_and_env() {
        let plan = build_git_diff_plan(&DiffArgs::default()).expect("build plan");

        // Exact argv for the default (no `--stat`, no path filter) case: this
        // pins every hardening flag from the module doc — fsmonitor hook,
        // hooksPath, diff.external, pager (twice), and the textconv/ext-diff
        // disable flags — so a future edit cannot silently drop one.
        let expected_argv: Vec<String> = GIT_DIFF_BASE_ARGV
            .iter()
            .map(|token| (*token).to_owned())
            .collect();
        assert_eq!(plan.argv, expected_argv);
        assert_eq!(
            plan.env,
            vec![
                ("GIT_CONFIG_NOSYSTEM", "1"),
                ("GIT_CONFIG_GLOBAL", "/dev/null"),
                ("GIT_TERMINAL_PROMPT", "0"),
            ]
        );
        assert_eq!(plan.env, GIT_DIFF_ENV.to_vec());
    }

    #[test]
    fn test_build_git_diff_plan_with_stat_and_path_appends_literal_pathspec() {
        let plan = build_git_diff_plan(&DiffArgs {
            path: Some("src".to_owned()),
            stat_only: true,
        })
        .expect("build plan");

        let tail = &plan.argv[plan.argv.len() - 3..];
        assert_eq!(tail, ["--stat", "--", ":(literal)src"]);
    }

    #[test]
    fn test_build_git_diff_plan_rejects_pathspec_magic() {
        for magic_path in [":/", ":(top)src", "::src", ":src"] {
            let error = build_git_diff_plan(&DiffArgs {
                path: Some(magic_path.to_owned()),
                stat_only: false,
            })
            .expect_err("pathspec magic must be rejected");
            assert!(matches!(error, OpError::InvalidArguments(_)));
        }
    }

    #[test]
    fn test_validate_relative_pathspec_rejects_workspace_escape() {
        for bad in ["../secrets", "/etc/passwd", "a/../b", ""] {
            assert!(matches!(
                validate_relative_pathspec(bad),
                Err(OpError::InvalidArguments(_))
            ));
        }
    }

    #[test]
    fn test_render_shell_command_quotes_every_token_for_default_plan() {
        let plan = build_git_diff_plan(&DiffArgs::default()).expect("build plan");
        let command = render_shell_command(&plan);
        assert_eq!(
            command,
            "GIT_CONFIG_NOSYSTEM='1' GIT_CONFIG_GLOBAL='/dev/null' GIT_TERMINAL_PROMPT='0' \
             'git' '-c' 'core.fsmonitor=' '-c' 'core.hooksPath=/dev/null' '-c' \
             'diff.external=' '-c' 'core.pager=cat' '--no-pager' 'diff' '--no-textconv' \
             '--no-ext-diff' '--no-color'"
        );
    }

    #[test]
    fn test_render_shell_command_escapes_quotes_in_malicious_literal_path() {
        let plan = build_git_diff_plan(&DiffArgs {
            path: Some("src/'; touch owned; echo '".to_owned()),
            stat_only: true,
        })
        .expect("build plan");
        let command = render_shell_command(&plan);

        // Exact tail: `--` then the fully quoted, `'\''`-escaped literal
        // pathspec — no unescaped `'` reaches the shell.
        let expected_tail = concat!("'--' ", r"':(literal)src/'\''; touch owned; echo '\'''");
        assert!(
            command.ends_with(expected_tail),
            "unexpected quoting: {command}"
        );
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

    #[test]
    fn test_killed_by_output_limit_returns_truncated_ok_not_error() {
        let text = render_shell_output(serde_json::json!({
            "exit_code": -1,
            "stdout": "diff --git a/x b/x\n+partial",
            "stderr": "",
            "truncated": true,
            "killed_by_output_limit": true,
        }))
        .expect("killed-by-output-limit must be a truncated success, not an error");

        assert!(text.starts_with("diff --git a/x b/x\n+partial"));
        assert!(
            text.contains("Diff gekürzt bei 64 KiB"),
            "missing truncation notice: {text}"
        );
    }

    #[test]
    fn test_truncated_without_kill_returns_ok_with_notice() {
        let text = render_shell_output(serde_json::json!({
            "exit_code": 0,
            "stdout": "diff --git a/x b/x\n+full run, output just capped",
            "stderr": "",
            "truncated": true,
            "killed_by_output_limit": false,
        }))
        .expect("truncated-but-completed output must be Ok");

        assert!(text.starts_with("diff --git a/x b/x\n+full run, output just capped"));
        assert!(
            text.contains("Diff gekürzt bei 64 KiB"),
            "missing truncation notice: {text}"
        );
    }

    #[test]
    fn test_real_git_failure_without_kill_is_still_an_error() {
        let error = render_shell_output(serde_json::json!({
            "exit_code": 1,
            "stdout": "",
            "stderr": "fatal: not a git repository",
            "truncated": false,
            "killed_by_output_limit": false,
        }))
        .expect_err("a genuine non-zero git exit without a kill must stay an error");

        match error {
            OpError::Execution(message) => {
                assert_eq!(message, "git diff exited with status 1");
            }
            other => panic!("expected execution error, got {other:?}"),
        }
    }

    #[test]
    fn test_missing_truncation_fields_default_to_false() {
        // Older/foreign JSON without `truncated`/`killed_by_output_limit` must
        // behave exactly as before: success without a notice, error on
        // nonzero exit.
        let text = render_shell_output(serde_json::json!({
            "exit_code": 0,
            "stdout": "diff --git a/x b/x\n+ok",
            "stderr": "",
        }))
        .expect("missing optional fields must default to false, not fail parsing");
        assert_eq!(text, "diff --git a/x b/x\n+ok");
    }

    #[test]
    fn test_diff_declares_always_approval_model_tool_surface() {
        let meta = DiffOperation.meta();
        assert_eq!(meta.name, "diff");
        assert!(meta.surfaces.iter().any(|surface| matches!(
            surface,
            Surface::ModelTool {
                readonly: true,
                approval: ApprovalPolicy::Always,
            }
        )));
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
