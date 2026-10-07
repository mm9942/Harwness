//! Test-Gerüst (nur `cfg(test)`): `TestError`/`TestResult`, Ausführungskontext
//! mit wählbaren Rechten und Aufruf-Helfer.

use harw_authority::{
    Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
};
use harw_tools::{ToolCall, ToolExecutionContext, ToolExecutor, ToolName, ToolOutput};
use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
use serde_json::Value;
use std::path::PathBuf;
use tempfile::TempDir;

harw_test_support::define_test_error!(
    pub(crate),
    Io(std::io::Error) => "I/O error",
    Json(serde_json::Error) => "JSON error",
    Tools(harw_tools::ToolsError) => "tool error"
);

/// Temporärer Workspace (die `sys.*`-Werkzeuge brauchen ihn nur für den Kontext).
pub(crate) struct Fixture {
    _dir: TempDir,
    base: PathBuf,
}

impl Fixture {
    pub(crate) fn new() -> TestResult<Self> {
        let dir = TempDir::new()?;
        let base = dir.path().canonicalize()?;
        std::fs::create_dir_all(base.join("ws"))?;
        Ok(Self { _dir: dir, base })
    }

    /// Ausführungskontext mit den angegebenen Rechten.
    pub(crate) fn ctx(&self, permissions: Vec<Permission>) -> TestResult<ToolExecutionContext> {
        let registry = WorkspaceRegistry::build(
            &self.base,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("t"),
                workspace: WorkspaceId::from_str("w"),
                root: PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("registry"))?;
        let binding = registry
            .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("w"))
            .map_err(ctx("binding"))?;
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::from_policy(permissions));
        Ok(ToolExecutionContext::new(
            SessionId::new(),
            TurnId::new(),
            sandbox,
        ))
    }

    /// Kontext mit `ExecuteProcess`.
    pub(crate) fn exec_ctx(&self) -> TestResult<ToolExecutionContext> {
        self.ctx(vec![Permission::ExecuteProcess])
    }
}

/// Führt ein Werkzeug über seinen `ToolExecutor` aus.
pub(crate) async fn run(
    tool: &dyn ToolExecutor,
    context: &ToolExecutionContext,
    name: &str,
    arguments: Value,
) -> TestResult<ToolOutput> {
    let call = ToolCall {
        id: ToolCallId::new(),
        name: ToolName::new(name),
        arguments,
    };
    Ok(tool.execute(context, &call).await?)
}

/// JSON-Inhalt einer erfolgreichen Ausgabe.
pub(crate) fn json_of(output: ToolOutput) -> TestResult<Value> {
    match output {
        ToolOutput::Json { content } => Ok(content),
        other => Err(TestError::Unexpected(format!("kein JSON: {other:?}"))),
    }
}

/// Fehlermeldung einer Fehlerausgabe.
pub(crate) fn error_of(output: ToolOutput) -> TestResult<String> {
    match output {
        ToolOutput::Error { message } => Ok(message),
        other => Err(TestError::Unexpected(format!("kein Fehler: {other:?}"))),
    }
}
