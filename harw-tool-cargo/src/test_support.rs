//! Test-Gerüst (nur `cfg(test)`): `TestError`/`TestResult`, Ausführungskontext,
//! ein skriptbarer `shell.exec`-Ersatz und Aufruf-Helfer.

use harw_authority::{
    Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
};
use harw_tools::{
    ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolName, ToolOutput,
};
use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use tempfile::TempDir;

harw_test_support::define_test_error!(
    pub(crate),
    Io(std::io::Error) => "I/O error",
    Json(serde_json::Error) => "JSON error",
    Tools(harw_tools::ToolsError) => "tool error"
);

/// Temporärer Workspace `ws/`.
pub(crate) struct Fixture {
    _dir: TempDir,
    base: PathBuf,
    /// Kanonische Workspace-Wurzel.
    pub(crate) ws: PathBuf,
}

impl Fixture {
    pub(crate) fn new() -> TestResult<Self> {
        let dir = TempDir::new()?;
        let base = dir.path().canonicalize()?;
        let ws = base.join("ws");
        std::fs::create_dir_all(&ws)?;
        Ok(Self {
            _dir: dir,
            base,
            ws,
        })
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

type Reply = Box<dyn Fn(&str) -> ToolOutput + Send + Sync>;

/// Skriptbarer Ersatz für den `shell.exec`-Ausführer: zeichnet
/// `(command, timeout_secs)` auf und antwortet nach Vorgabe.
pub(crate) struct ScriptedShell {
    calls: Mutex<Vec<(String, u64)>>,
    reply: Reply,
}

impl ScriptedShell {
    pub(crate) fn with(reply: impl Fn(&str) -> ToolOutput + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            reply: Box::new(reply),
        })
    }

    pub(crate) fn json(content: Value) -> Arc<Self> {
        Self::with(move |_| ToolOutput::json(content.clone()))
    }

    pub(crate) fn error(message: &'static str) -> Arc<Self> {
        Self::with(move |_| ToolOutput::error(message))
    }

    pub(crate) fn text(text: &'static str) -> Arc<Self> {
        Self::with(move |_| ToolOutput::text(text))
    }

    /// Alle bisherigen Aufrufe.
    pub(crate) fn calls(&self) -> Vec<(String, u64)> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl ToolExecutor for ScriptedShell {
    fn execute<'a>(
        &'a self,
        _context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        Box::pin(async move {
            let command = call
                .arguments
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let timeout = call
                .arguments
                .get("timeout_secs")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            self.calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((command.clone(), timeout));
            Ok((self.reply)(&command))
        })
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
