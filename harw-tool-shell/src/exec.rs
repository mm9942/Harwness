//! Shell-execution tool provider and executor for Harwness.
//!
//! **Security note**: This executor launches `/bin/sh -c` only through a Bubblewrap
//! plan derived from the per-call sandbox. Failure to build or spawn that plan rejects
//! execution rather than falling back to the host.
//!
//! # Responsibility scope
//! - Owns [`ShellToolProvider`] (implements [`harw_extension_api::contributors::ToolProvider`])
//! - Owns [`ShellExecutor`] (implements [`harw_tools::ToolExecutor`])
//! - Owns [`ShellExecError`] — internal error type (never crosses the `ToolExecutor` boundary;
//!   all failures are returned as [`harw_tools::ToolOutput::error`] or [`harw_tools::ToolsError`])
//!
//! # Key types
//! - [`ShellToolProvider`] — stateful provider holding timeout and output-size configuration
//! - [`ShellExecutor`] — stateless per-call executor (state driven by provider config)
//! - [`ShellExecError`] — typed internal errors for spawn/timeout/truncation scenarios
//!
//! # Concurrency model
//! [`ShellToolProvider`] and [`ShellExecutor`] are `Send + Sync`. Shell side-effects are
//! presumed non-commutative, so [`ShellToolProvider::parallel_safe`] always returns `false`.
//!
//! # Error types
//! [`ShellExecError`] — converted to [`harw_tools::ToolOutput::error`] before crossing the
//! public `ToolExecutor` boundary. Callers only ever see `Result<ToolOutput, ToolsError>`.

use harw_extension_api::contributors::ToolProvider;
use harw_sandbox::{BwrapLauncher, Permission, SandboxSpec};
use harw_tools::{
    schema::{AdditionalProperties, JsonSchema, JsonSchemaType},
    spec::{FunctionToolSpec, ToolName, ToolSpec},
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolOutput, ToolsError,
};
use serde::Deserialize;
use serde_json::json;
use std::{collections::BTreeMap, ffi::OsString, fmt, io, sync::Arc, time::Duration};
use tracing::{debug, info, warn};

// ── Constants ─────────────────────────────────────────────────────────────────

const TOOL_NAME: &str = "shell.exec";
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 64 * 1024;
const TRUNCATION_MARKER: &str = "\n[...truncated...]";

// ── ShellExecError ────────────────────────────────────────────────────────────

/// Internal error type for shell execution failures.
///
/// # Description
/// Represents the distinct failure modes of [`ShellExecutor`]. This type is
/// never returned across the [`ToolExecutor`] boundary — all variants are
/// converted to human-readable [`ToolOutput::error`] messages before returning.
///
/// # Concurrency
/// `ShellExecError` is `Send + Sync`.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_shell::ShellExecError;
///
/// let err = ShellExecError::InvalidArgs("command field missing".to_owned());
/// assert!(err.to_string().contains("invalid arguments"));
/// ```
#[derive(Debug)]
pub enum ShellExecError {
    /// The tool call arguments could not be parsed or are semantically invalid.
    InvalidArgs(String),
    /// The child process could not be spawned.
    Spawn(io::Error),
    /// The child process did not complete within the allowed time.
    Timeout {
        /// Effective timeout in seconds that was exceeded.
        secs: u64,
    },
    /// The child process exited with a non-zero status.
    ExitWithError {
        /// The exit code returned by the process.
        code: i32,
    },
    /// The combined output exceeded `max_output_bytes` and was truncated.
    TruncatedOutput,
}

impl fmt::Display for ShellExecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgs(msg) => write!(f, "shell.exec: invalid arguments: {msg}"),
            Self::Spawn(err) => write!(f, "shell.exec: failed to spawn Bubblewrap: {err}"),
            Self::Timeout { secs } => write!(f, "shell.exec timed out after {secs}s"),
            Self::ExitWithError { code } => {
                write!(f, "shell.exec: process exited with code {code}")
            }
            Self::TruncatedOutput => write!(f, "shell.exec: output truncated"),
        }
    }
}

impl std::error::Error for ShellExecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for ShellExecError {
    /// Converts an [`io::Error`] into [`ShellExecError::Spawn`].
    ///
    /// # Description
    /// Used by `?` propagation when a process spawn fails.
    fn from(err: io::Error) -> Self {
        Self::Spawn(err)
    }
}

// ── Argument deserialization ──────────────────────────────────────────────────

/// Deserialized arguments for the `shell.exec` tool call.
///
/// # Description
/// Parsed from `call.arguments` (a [`serde_json::Value`]). Unknown fields are
/// rejected via `deny_unknown_fields` to prevent prompt-injection via surplus keys.
///
/// # Errors
/// [`ShellExecError::InvalidArgs`] when the JSON does not match this schema.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellExecArgs {
    /// The shell command to execute via `/bin/sh -c`.
    command: String,
    /// Optional per-call timeout override. Clamped to the provider's configured maximum.
    timeout_secs: Option<u64>,
}

// ── ShellExecutor ─────────────────────────────────────────────────────────────

/// Stateless executor for one `shell.exec` invocation.
///
/// # Description
/// Spawns `/bin/sh -c <command>` through Bubblewrap, captures stdout and stderr,
/// applies timeout and combined output-size limits, and returns a JSON `ToolOutput`.
///
/// The executor is stateless: all configuration is fixed at construction time and
/// comes from the owning [`ShellToolProvider`].
///
/// # Concurrency
/// `ShellExecutor` is `Send + Sync`. Multiple calls may run concurrently on different
/// Tokio tasks, but [`ShellToolProvider::parallel_safe`] returns `false` because shell
/// commands typically have side effects that are not safe to interleave.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_shell::ShellToolProvider;
/// use harw_extension_api::contributors::ToolProvider;
/// use harw_tools::spec::ToolName;
///
/// let provider = ShellToolProvider::new();
/// let name = ToolName::new("shell.exec");
/// let executor = provider.executor(&name);
/// assert!(executor.is_some());
/// ```
pub struct ShellExecutor {
    timeout_secs: u64,
    max_output_bytes: usize,
}

impl ShellExecutor {
    /// Validates caller-controlled arguments and computes the effective timeout.
    ///
    /// Explicit zero values are rejected instead of being passed to the timeout
    /// machinery, while positive caller values remain capped by the provider.
    fn effective_timeout(&self, args: &ShellExecArgs) -> Result<u64, ToolsError> {
        if args.command.trim().is_empty() {
            return Err(ToolsError::InvalidArguments {
                name: TOOL_NAME.to_owned(),
                reason: "command must not be blank".to_owned(),
            });
        }

        if args.timeout_secs == Some(0) {
            return Err(ToolsError::InvalidArguments {
                name: TOOL_NAME.to_owned(),
                reason: "timeout_secs must be greater than zero".to_owned(),
            });
        }

        Ok(args
            .timeout_secs
            .unwrap_or(self.timeout_secs)
            .min(self.timeout_secs))
    }

    /// Truncates `bytes` to at most `limit` bytes, appending a truncation marker if needed.
    ///
    /// # Description
    /// The limit applies to the returned string, including the marker. The retained
    /// prefix is adjusted to the last valid UTF-8 boundary before conversion, so a
    /// multibyte code point is never split. If the limit is too small for the marker,
    /// the marker is omitted and the largest valid prefix is returned within the limit.
    ///
    /// # Returns
    /// `(string, was_truncated)` — the (possibly clipped) string and whether clipping occurred.
    ///
    /// # Concurrency
    /// Pure function; no shared state.
    fn truncate_output(bytes: &[u8], limit: usize) -> (String, bool) {
        if bytes.len() <= limit {
            (String::from_utf8_lossy(bytes).into_owned(), false)
        } else {
            let marker_fits = limit >= TRUNCATION_MARKER.len();
            let prefix_limit = if marker_fits {
                limit - TRUNCATION_MARKER.len()
            } else {
                limit
            };
            let prefix_end = match std::str::from_utf8(&bytes[..prefix_limit]) {
                Ok(_) => prefix_limit,
                Err(error) => error.valid_up_to(),
            };
            let mut clipped = String::from_utf8_lossy(&bytes[..prefix_end]).into_owned();
            if marker_fits {
                clipped.push_str(TRUNCATION_MARKER);
            }
            (clipped, true)
        }
    }

    /// Applies the configured output budget across stdout and stderr together.
    ///
    /// Stdout is retained first because it is normally the primary command result;
    /// stderr receives the unused portion of the same raw-byte budget.
    fn truncate_combined_output(
        stdout: &[u8],
        stderr: &[u8],
        limit: usize,
    ) -> (String, String, bool) {
        let stdout_limit = stdout.len().min(limit);
        let stderr_limit = limit.saturating_sub(stdout_limit);
        let (stdout, stdout_truncated) = Self::truncate_output(stdout, stdout_limit);
        let (stderr, stderr_truncated) = Self::truncate_output(stderr, stderr_limit);

        (stdout, stderr, stdout_truncated || stderr_truncated)
    }

    /// Executes the shell command described by `args` in the given sandbox.
    ///
    /// # Description
    /// Core async logic extracted for readability. Spawns the subprocess, applies the
    /// timeout, collects output, and builds the final [`ToolOutput`].
    ///
    /// # Errors
    /// Returns `Ok(ToolOutput::error(...))` for denied-permission, timeout, or spawn
    /// failure — callers are not expected to match on `Err` for these cases.
    /// Returns `Err(ToolsError)` only for argument parsing failures.
    ///
    /// # Concurrency
    /// Safe to call from any async context. Uses `kill_on_drop(true)` so the child
    /// process is reaped if the future is dropped before completion.
    ///
    /// # Panics
    /// None in production paths.
    async fn run_command(
        &self,
        args: &ShellExecArgs,
        sandbox: &SandboxSpec,
    ) -> Result<ToolOutput, ToolsError> {
        let effective_timeout = self.effective_timeout(args)?;

        debug!(
            command_len = args.command.len(),
            cwd = %sandbox.workspace().canonical_root().display(),
            timeout_secs = effective_timeout,
            "shell.exec preparing isolated process"
        );

        use std::process::Stdio;
        use tokio::process::Command as TokioCommand;

        let launcher = BwrapLauncher::default();
        let shell_command = [
            OsString::from("/bin/sh"),
            OsString::from("-c"),
            OsString::from(&args.command),
        ];
        let plan = match launcher.plan(sandbox, &shell_command) {
            Ok(plan) => plan,
            Err(err) => {
                warn!(error = %err, "shell.exec sandbox plan rejected");
                return Ok(ToolOutput::error(format!(
                    "shell.exec: sandbox setup failed: {err}"
                )));
            }
        };

        // `BwrapLauncher` deliberately exposes an inspectable plan but its synchronous
        // spawn API cannot pipe output. Execute that validated plan with Tokio so this
        // boundary can retain timeout, kill-on-drop, and output-capture guarantees.
        let child = match TokioCommand::new("bwrap")
            .args(plan.args())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(c) => c,
            Err(err) => {
                warn!(error = %err, "shell.exec spawn failed");
                return Ok(ToolOutput::error(format!(
                    "shell.exec: failed to spawn Bubblewrap: {err}"
                )));
            }
        };

        let timeout_result = tokio::time::timeout(
            Duration::from_secs(effective_timeout),
            child.wait_with_output(),
        )
        .await;

        match timeout_result {
            Err(_elapsed) => {
                warn!(timeout_secs = effective_timeout, "shell.exec timed out");
                Ok(ToolOutput::error(format!(
                    "shell.exec timed out after {effective_timeout}s"
                )))
            }
            Ok(Err(err)) => {
                warn!(error = %err, "shell.exec wait_with_output failed");
                Ok(ToolOutput::error(format!(
                    "shell.exec: process I/O error: {err}"
                )))
            }
            Ok(Ok(output)) => {
                let exit_code = output.status.code().unwrap_or(-1);

                let (stdout_str, stderr_str, truncated) = Self::truncate_combined_output(
                    &output.stdout,
                    &output.stderr,
                    self.max_output_bytes,
                );

                info!(exit_code, truncated, "shell.exec completed");

                Ok(ToolOutput::json(json!({
                    "exit_code": exit_code,
                    "stdout": stdout_str,
                    "stderr": stderr_str,
                    "truncated": truncated,
                })))
            }
        }
    }
}

impl ToolExecutor for ShellExecutor {
    /// Executes the `shell.exec` tool call.
    ///
    /// # Description
    /// 1. Parses `call.arguments` into [`ShellExecArgs`].
    /// 2. Rejects blank commands and caller-provided zero timeouts.
    /// 3. Checks that [`Permission::ExecuteProcess`] is granted in `context.sandbox()`.
    /// 4. Builds a Bubblewrap plan from `context.sandbox()`.
    /// 5. Executes `/bin/sh -c <command>` inside that plan with the effective timeout.
    /// 6. Returns a JSON `ToolOutput` with `exit_code`, `stdout`, `stderr`, and `truncated`.
    ///
    /// # Arguments
    /// - `context` (`&ToolExecutionContext`): harness-established sandbox authority.
    /// - `call` (`&ToolCall`): untrusted invocation; `arguments` must match [`ShellExecArgs`].
    ///
    /// # Returns
    /// `Ok(ToolOutput::json(...))` on success, `Ok(ToolOutput::error(...))` on permission
    /// denial or runtime failure, `Err(ToolsError::InvalidArguments)` if arguments cannot
    /// be parsed.
    ///
    /// # Errors
    /// - [`ToolsError::InvalidArguments`]: arguments JSON does not conform to the tool schema.
    ///
    /// # Concurrency
    /// `Send + Sync`. The returned future is `Send`.
    ///
    /// # Panics
    /// None.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_tool_shell::ShellToolProvider;
    /// use harw_extension_api::contributors::ToolProvider;
    /// use harw_tools::spec::ToolName;
    ///
    /// let provider = ShellToolProvider::new();
    /// let executor = provider.executor(&ToolName::new("shell.exec")).unwrap();
    /// // executor.execute(&ctx, &call).await
    /// ```
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            // 1. Parse arguments
            let args: ShellExecArgs =
                serde_json::from_value(call.arguments.clone()).map_err(|err| {
                    ToolsError::InvalidArguments {
                        name: TOOL_NAME.to_owned(),
                        reason: err.to_string(),
                    }
                })?;

            // 2. Reject invalid caller-controlled values before sandbox planning or spawn.
            self.effective_timeout(&args)?;

            // 3. Permission check
            if let Some(err) = harw_tools::sandbox_guard::require_permission(
                context,
                Permission::ExecuteProcess,
                TOOL_NAME,
            ) {
                warn!("shell.exec denied: ExecuteProcess permission missing");
                return Ok(err);
            }

            // 4 + 5 + 6. Build an isolated launch plan, spawn, and collect.
            self.run_command(&args, context.sandbox()).await
        })
    }
}

// ── ShellToolProvider ─────────────────────────────────────────────────────────

/// Tool provider that registers the `shell.exec` function-tool.
///
/// # Description
/// Holds the shared configuration (timeout, max output size) and manufactures
/// [`ShellExecutor`] instances on demand via [`ToolProvider::executor`].
///
/// Defaults: `timeout_secs = 30`, `max_output_bytes = 65536` (64 KiB).
///
/// # Concurrency
/// `Send + Sync`. Multiple threads may call [`tools`][ShellToolProvider::tools] and
/// [`executor`][ShellToolProvider::executor] concurrently without synchronisation.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_shell::ShellToolProvider;
/// use harw_extension_api::contributors::ToolProvider;
/// use harw_tools::spec::ToolName;
///
/// let provider = ShellToolProvider::new();
/// assert_eq!(provider.tools().len(), 1);
/// assert!(provider.executor(&ToolName::new("shell.exec")).is_some());
/// assert!(provider.executor(&ToolName::new("other.tool")).is_none());
/// ```
pub struct ShellToolProvider {
    /// Maximum time in seconds a single shell command may run.
    pub timeout_secs: u64,
    /// Maximum number of raw stdout and stderr bytes returned together.
    pub max_output_bytes: usize,
}

impl ShellToolProvider {
    /// Creates a [`ShellToolProvider`] with default configuration.
    ///
    /// # Description
    /// Equivalent to [`Default::default`]. Provides sane defaults suitable for
    /// interactive coding-agent use: 30-second timeout, 64 KiB output cap.
    ///
    /// # Returns
    /// A new [`ShellToolProvider`] with `timeout_secs = 30` and `max_output_bytes = 65536`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_tool_shell::ShellToolProvider;
    ///
    /// let provider = ShellToolProvider::new();
    /// assert_eq!(provider.timeout_secs, 30);
    /// assert_eq!(provider.max_output_bytes, 64 * 1024);
    /// ```
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds the [`JsonSchema`] for the `shell.exec` parameters.
    ///
    /// # Description
    /// Returns an `object` schema with two properties — `command` (required) and
    /// `timeout_secs` (optional integer). Constructed once per [`tools`][ShellToolProvider::tools]
    /// call; cheap enough not to cache.
    ///
    /// # Returns
    /// A [`JsonSchema`] matching the tool's parameter contract.
    ///
    /// # Concurrency
    /// Pure function; no shared state.
    fn parameter_schema() -> JsonSchema {
        let mut properties = BTreeMap::new();

        properties.insert(
            "command".to_owned(),
            JsonSchema {
                schema_type: Some(JsonSchemaType::String),
                description: Some(
                    "Shell command to execute in the isolated project sandbox. Uses /bin/sh -c."
                        .to_owned(),
                ),
                ..Default::default()
            },
        );

        properties.insert(
            "timeout_secs".to_owned(),
            JsonSchema {
                schema_type: Some(JsonSchemaType::Integer),
                description: Some(
                    "Optional per-call timeout override. Cannot exceed provider default."
                        .to_owned(),
                ),
                ..Default::default()
            },
        );

        JsonSchema {
            schema_type: Some(JsonSchemaType::Object),
            properties: Some(properties),
            required: Some(vec!["command".to_owned()]),
            additional_properties: Some(Box::new(AdditionalProperties::Bool(false))),
            ..Default::default()
        }
    }
}

impl Default for ShellToolProvider {
    /// Returns a [`ShellToolProvider`] with default timeout (30 s) and output cap (64 KiB).
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_tool_shell::ShellToolProvider;
    ///
    /// let provider = ShellToolProvider::default();
    /// assert_eq!(provider.timeout_secs, 30);
    /// ```
    fn default() -> Self {
        Self {
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }
}

impl ToolProvider for ShellToolProvider {
    /// Returns the single tool specification for `shell.exec`.
    ///
    /// # Description
    /// Constructs a [`ToolSpec::Function`] with the JSON schema, description, and
    /// `strict = true` so the model cannot inject extra fields.
    ///
    /// # Returns
    /// A one-element `Vec<ToolSpec>`.
    ///
    /// # Concurrency
    /// Safe to call from multiple threads.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_tool_shell::ShellToolProvider;
    /// use harw_extension_api::contributors::ToolProvider;
    ///
    /// let specs = ShellToolProvider::new().tools();
    /// assert_eq!(specs.len(), 1);
    /// assert_eq!(specs[0].name(), "shell.exec");
    /// ```
    fn tools(&self) -> Vec<ToolSpec> {
        vec![ToolSpec::Function(FunctionToolSpec {
            name: ToolName::new(TOOL_NAME),
            description: "Execute a shell command inside the isolated project sandbox. \
                Runs under Bubblewrap via /bin/sh -c and captures stdout+stderr. \
                Requires ExecuteProcess permission."
                .to_owned(),
            parameters: Self::parameter_schema(),
            strict: true,
        })]
    }

    /// Returns a [`ShellExecutor`] for `shell.exec`, or `None` for any other name.
    ///
    /// # Description
    /// Constructs an executor carrying the provider's timeout and output-cap configuration.
    /// Each call allocates a new `Arc<ShellExecutor>`; the executor itself is stateless.
    ///
    /// # Arguments
    /// - `name` (`&ToolName`): the requested tool name.
    ///
    /// # Returns
    /// `Some(Arc<ShellExecutor>)` when `name == "shell.exec"`, `None` otherwise.
    ///
    /// # Concurrency
    /// Safe to call from multiple threads.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_tool_shell::ShellToolProvider;
    /// use harw_extension_api::contributors::ToolProvider;
    /// use harw_tools::spec::ToolName;
    ///
    /// let provider = ShellToolProvider::new();
    /// assert!(provider.executor(&ToolName::new("shell.exec")).is_some());
    /// assert!(provider.executor(&ToolName::new("other")).is_none());
    /// ```
    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        if name.as_str() == TOOL_NAME {
            Some(Arc::new(ShellExecutor {
                timeout_secs: self.timeout_secs,
                max_output_bytes: self.max_output_bytes,
            }))
        } else {
            None
        }
    }

    /// Returns `false` — shell side-effects are presumed non-commutative.
    ///
    /// # Description
    /// Shell commands modify filesystem state, environment variables, and process
    /// tables. Running them in parallel without coordination risks data races.
    /// Callers must serialize `shell.exec` invocations.
    ///
    /// # Returns
    /// Always `false`.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_tool_shell::ShellToolProvider;
    /// use harw_extension_api::contributors::ToolProvider;
    /// use harw_tools::spec::ToolName;
    ///
    /// let provider = ShellToolProvider::new();
    /// assert!(!provider.parallel_safe(&ToolName::new("shell.exec")));
    /// ```
    fn parallel_safe(&self, _name: &ToolName) -> bool {
        false
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use harw_sandbox::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_tools::{ToolCall, ToolExecutionContext};
    use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
    use std::fs;
    use tempfile::TempDir;

    // ── Test helpers ───────────────────────────────────────────────────────────

    fn make_temp_workspace() -> TempDir {
        tempfile::tempdir().expect("tempdir creation must succeed in tests")
    }

    fn make_sandbox(dir: &TempDir, permissions: Vec<Permission>) -> SandboxSpec {
        let harness_root = dir.path().to_path_buf();
        let ws_subdir = harness_root.join("project");
        fs::create_dir_all(&ws_subdir).expect("project subdir must be created");

        let registry = WorkspaceRegistry::build(
            &harness_root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("project"),
                root: ws_subdir,
            }],
        )
        .expect("registry build must succeed");

        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("project"),
            )
            .expect("resolve must succeed");

        SandboxSpec::from_resolved(binding, PermissionSet::from_policy(permissions))
    }

    fn make_ctx(sandbox: SandboxSpec) -> ToolExecutionContext {
        ToolExecutionContext::new(SessionId::new(), TurnId::new(), sandbox)
    }

    fn make_call(command: &str) -> ToolCall {
        ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(TOOL_NAME),
            arguments: serde_json::json!({ "command": command }),
        }
    }

    fn make_call_with_timeout(command: &str, timeout_secs: u64) -> ToolCall {
        ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(TOOL_NAME),
            arguments: serde_json::json!({ "command": command, "timeout_secs": timeout_secs }),
        }
    }

    // ── Tests ──────────────────────────────────────────────────────────────────

    #[test]
    fn test_provider_lists_one_tool() {
        let provider = ShellToolProvider::new();
        let tools = provider.tools();
        assert_eq!(tools.len(), 1, "provider must expose exactly one tool");
        assert_eq!(tools[0].name(), TOOL_NAME);
    }

    #[test]
    fn test_provider_not_parallel_safe() {
        let provider = ShellToolProvider::new();
        let name = ToolName::new(TOOL_NAME);
        assert!(
            !provider.parallel_safe(&name),
            "shell.exec must never be parallel-safe"
        );
    }

    #[test]
    fn test_effective_timeout_rejects_blank_command_without_planning() {
        let executor = ShellExecutor {
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        };

        for command in ["", " ", "\t\n"] {
            let args = ShellExecArgs {
                command: command.to_owned(),
                timeout_secs: None,
            };
            let result = executor.effective_timeout(&args);

            assert!(
                matches!(result, Err(ToolsError::InvalidArguments { .. })),
                "blank command must be rejected before planning"
            );
        }
    }

    #[test]
    fn test_effective_timeout_rejects_caller_zero_without_planning() {
        let executor = ShellExecutor {
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        };
        let args = ShellExecArgs {
            command: "echo valid".to_owned(),
            timeout_secs: Some(0),
        };

        let result = executor.effective_timeout(&args);

        assert!(
            matches!(result, Err(ToolsError::InvalidArguments { .. })),
            "caller-provided zero timeout must be rejected before planning"
        );
    }

    #[test]
    fn test_effective_timeout_preserves_provider_cap() {
        let executor = ShellExecutor {
            timeout_secs: 5,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        };
        let args = ShellExecArgs {
            command: "echo valid".to_owned(),
            timeout_secs: Some(30),
        };

        assert_eq!(executor.effective_timeout(&args).unwrap_or(0), 5);
    }

    #[test]
    fn test_truncate_output_handles_multibyte_utf8_boundary() {
        let limit = 3 + TRUNCATION_MARKER.len();
        let (output, truncated) =
            ShellExecutor::truncate_output("abc€defghijklmnopqrs".as_bytes(), limit);

        assert!(
            truncated,
            "output over the byte cap must be marked truncated"
        );
        assert!(
            output.starts_with("abc"),
            "truncation must retain only complete UTF-8 characters, got: {output:?}"
        );
        assert!(
            !output.contains('�'),
            "truncation must not split a code point"
        );
        assert!(
            output.len() <= limit,
            "returned output must respect its byte cap"
        );
        assert!(
            output.ends_with(TRUNCATION_MARKER),
            "truncated output must include its marker, got: {output:?}"
        );
    }

    #[test]
    fn test_truncate_combined_output_uses_one_budget() {
        let limit = 6 + 4 + TRUNCATION_MARKER.len();
        let (stdout, stderr, truncated) = ShellExecutor::truncate_combined_output(
            b"stdout",
            b"stderr-output-that-is-long",
            limit,
        );

        assert_eq!(stdout, "stdout");
        assert_eq!(stderr, format!("stde{TRUNCATION_MARKER}"));
        assert!(
            truncated,
            "combined output over the cap must be marked truncated"
        );
        assert!(
            stdout.len() + stderr.len() <= limit,
            "stdout and stderr together must respect one byte budget"
        );
    }

    #[test]
    fn test_truncate_combined_output_prioritizes_over_budget_stdout() {
        let limit = 4 + TRUNCATION_MARKER.len();
        let (stdout, stderr, truncated) = ShellExecutor::truncate_combined_output(
            b"stdout-output-that-is-long",
            b"stderr-output",
            limit,
        );

        assert_eq!(stdout, format!("stdo{TRUNCATION_MARKER}"));
        assert_eq!(stderr, "");
        assert!(truncated, "over-budget stdout must be marked truncated");
        assert!(
            stdout.len() + stderr.len() <= limit,
            "stdout and stderr together must respect one byte budget"
        );
    }

    #[test]
    fn test_truncate_output_with_small_budget_keeps_valid_prefix() {
        let (output, truncated) = ShellExecutor::truncate_output("abc€def".as_bytes(), 4);

        assert_eq!(output, "abc");
        assert!(
            truncated,
            "output over the byte cap must be marked truncated"
        );
        assert!(
            output.len() <= 4,
            "returned output must respect its byte cap"
        );
    }

    #[tokio::test]
    async fn test_exec_echo_returns_stdout() {
        let tmp = make_temp_workspace();
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess]);
        let ctx = make_ctx(sandbox);
        let call = make_call("echo hello_from_shell");

        let provider = ShellToolProvider::new();
        let executor = provider
            .executor(&ToolName::new(TOOL_NAME))
            .expect("executor must be returned for shell.exec");

        let output = executor
            .execute(&ctx, &call)
            .await
            .expect("execute must not return Err");

        match output {
            ToolOutput::Json { content } => {
                assert_eq!(content["exit_code"], 0, "echo must exit with 0");
                let stdout = content["stdout"].as_str().unwrap_or("");
                assert!(
                    stdout.contains("hello_from_shell"),
                    "stdout must contain echoed string, got: {stdout:?}"
                );
                assert_eq!(
                    content["truncated"], false,
                    "echo output must not be truncated"
                );
            }
            other => panic!("expected Json output, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_exec_permission_denied() {
        let tmp = make_temp_workspace();
        // No ExecuteProcess permission
        let sandbox = make_sandbox(&tmp, vec![Permission::ReadWorkspace]);
        let ctx = make_ctx(sandbox);
        let call = make_call("echo should_not_run");

        let provider = ShellToolProvider::new();
        let executor = provider
            .executor(&ToolName::new(TOOL_NAME))
            .expect("executor must be returned");

        let output = executor
            .execute(&ctx, &call)
            .await
            .expect("execute must not return Err even when denied");

        match output {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("ExecuteProcess permission missing"),
                    "denial message must mention the missing permission, got: {message:?}"
                );
            }
            other => panic!("expected Error output for denied permission, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_exec_timeout() {
        let tmp = make_temp_workspace();
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess]);
        let ctx = make_ctx(sandbox);

        // Provider with 1-second timeout; per-call override also 1s
        let provider = ShellToolProvider {
            timeout_secs: 1,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        };
        let call = make_call_with_timeout("sleep 5", 1);

        let executor = provider
            .executor(&ToolName::new(TOOL_NAME))
            .expect("executor must be returned");

        let output = executor
            .execute(&ctx, &call)
            .await
            .expect("execute must not return Err on timeout");

        match output {
            ToolOutput::Error { message } => {
                assert!(
                    message.contains("timed out"),
                    "timeout message must contain 'timed out', got: {message:?}"
                );
            }
            other => panic!("expected Error output for timeout, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_exec_nonzero_exit() {
        let tmp = make_temp_workspace();
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess]);
        let ctx = make_ctx(sandbox);
        let call = make_call("false");

        let provider = ShellToolProvider::new();
        let executor = provider
            .executor(&ToolName::new(TOOL_NAME))
            .expect("executor must be returned");

        let output = executor
            .execute(&ctx, &call)
            .await
            .expect("execute must not return Err on nonzero exit");

        match output {
            ToolOutput::Json { content } => {
                let exit_code = content["exit_code"].as_i64().unwrap_or(0);
                assert_ne!(exit_code, 0, "false must return a nonzero exit code");
            }
            other => panic!("expected Json output even for nonzero exit, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_exec_stderr_captured() {
        let tmp = make_temp_workspace();
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess]);
        let ctx = make_ctx(sandbox);
        let call = make_call("echo error_output >&2");

        let provider = ShellToolProvider::new();
        let executor = provider
            .executor(&ToolName::new(TOOL_NAME))
            .expect("executor must be returned");

        let output = executor
            .execute(&ctx, &call)
            .await
            .expect("execute must not return Err");

        match output {
            ToolOutput::Json { content } => {
                let stderr = content["stderr"].as_str().unwrap_or("");
                assert!(
                    stderr.contains("error_output"),
                    "stderr must be captured, got: {stderr:?}"
                );
            }
            other => panic!("expected Json output, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_exec_output_truncation() {
        let tmp = make_temp_workspace();
        let sandbox = make_sandbox(&tmp, vec![Permission::ExecuteProcess]);
        let ctx = make_ctx(sandbox);

        // The cap leaves room for a marker plus a truncated stdout prefix.
        let provider = ShellToolProvider {
            timeout_secs: DEFAULT_TIMEOUT_SECS,
            max_output_bytes: TRUNCATION_MARKER.len() + 10,
        };
        // Generate more than the configured output cap.
        let call = make_call("echo 'this_is_a_longer_string_than_ten_bytes'");
        let executor = provider
            .executor(&ToolName::new(TOOL_NAME))
            .expect("executor must be returned");

        let output = executor
            .execute(&ctx, &call)
            .await
            .expect("execute must not return Err");

        match output {
            ToolOutput::Json { content } => {
                assert_eq!(
                    content["truncated"], true,
                    "output must be reported as truncated when cap is exceeded"
                );
                let stdout = content["stdout"].as_str().unwrap_or("");
                assert!(
                    stdout.contains("[...truncated...]"),
                    "truncation marker must appear in stdout, got: {stdout:?}"
                );
            }
            other => panic!("expected Json output, got: {other:?}"),
        }
    }
}
