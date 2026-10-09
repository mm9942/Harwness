//! Test-Gerüst (nur `cfg(test)`): `TestError`/`TestResult`, ein temporärer
//! Workspace samt fremdem Nachbarverzeichnis und Aufruf-Helfer.

use harw_authority::{
    Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
};
use harw_tools::{ToolCall, ToolExecutionContext, ToolExecutor, ToolName, ToolOutput};
use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

harw_test_support::define_test_error!(
    pub(crate),
    Io(std::io::Error) => "I/O error",
    Json(serde_json::Error) => "JSON error",
    Tools(harw_tools::ToolsError) => "tool error",
    Scope(crate::scope::ScopeError) => "scope error"
);

/// Inhalt der Geheimdatei außerhalb des Workspace.
pub(crate) const SECRET: &str = "TOPSECRET";

/// Temporärer Workspace `ws/` mit Nachbarverzeichnis `outside/`.
pub(crate) struct Fixture {
    _dir: TempDir,
    base: PathBuf,
    /// Kanonische Workspace-Wurzel.
    pub(crate) ws: PathBuf,
    /// Verzeichnis außerhalb des Workspace.
    pub(crate) outside: PathBuf,
}

impl Fixture {
    pub(crate) fn new() -> TestResult<Self> {
        let dir = TempDir::new()?;
        let base = dir.path().canonicalize()?;
        let ws = base.join("ws");
        let outside = base.join("outside");
        fs::create_dir_all(&ws)?;
        fs::create_dir_all(outside.join("deep"))?;
        fs::write(outside.join("secret.txt"), SECRET)?;
        fs::write(outside.join("deep/more.txt"), SECRET)?;
        Ok(Self {
            _dir: dir,
            base,
            ws,
            outside,
        })
    }

    /// Schreibt eine Datei (mit Elternverzeichnissen) in den Workspace.
    pub(crate) fn write(&self, rel: &str, contents: &[u8]) -> TestResult {
        let path = self.ws.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, contents)?;
        Ok(())
    }

    /// Symlinks nach außen und Schleifen.
    pub(crate) fn plant_escapes(&self) -> TestResult {
        use std::os::unix::fs::symlink;
        fs::create_dir_all(self.ws.join("nested"))?;
        symlink(&self.outside, self.ws.join("link_dir"))?;
        symlink(self.outside.join("secret.txt"), self.ws.join("link_file"))?;
        symlink(".", self.ws.join("loop"))?;
        symlink("..", self.ws.join("nested/up"))?;
        Ok(())
    }

    pub(crate) fn scope(&self) -> TestResult<crate::scope::Scope> {
        Ok(crate::scope::Scope::new(&self.ws)?)
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

    /// Kontext mit `ReadWorkspace`.
    pub(crate) fn read_ctx(&self) -> TestResult<ToolExecutionContext> {
        self.ctx(vec![Permission::ReadWorkspace])
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
