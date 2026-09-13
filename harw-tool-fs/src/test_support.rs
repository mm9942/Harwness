//! Gemeinsame Test-Hilfen für die W1-02-Sicherheitstests (nur `cfg(test)`).
//!
//! [`Fixture`] legt einen Workspace `ws/` und daneben ein fremdes
//! Verzeichnis `outside/` mit Geheimnissen an; [`Fixture::plant_escapes`]
//! erzeugt echte Symlinks aus dem Workspace hinaus sowie Symlink-Schleifen.

use harw_sandbox::{
    Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
};
use harw_tools::{ToolCall, ToolExecutionContext, ToolName, ToolOutput};
use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use tempfile::TempDir;

/// Inhalt der Geheimdateien außerhalb des Workspace.
pub const SECRET: &str = "TOPSECRET";

/// Temporärer Workspace mit fremdem Nachbarverzeichnis.
pub struct Fixture {
    _dir: TempDir,
    /// Harness-Wurzel (Elternverzeichnis von `ws` und `outside`).
    base: PathBuf,
    /// Workspace-Wurzel.
    pub ws: PathBuf,
    /// Verzeichnis außerhalb des Workspace.
    pub outside: PathBuf,
}

impl Fixture {
    /// Legt `ws/`, `outside/secret.txt` und `outside/deep/more.txt` an.
    pub fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let base = dir.path().canonicalize().expect("canonicalize");
        let ws = base.join("ws");
        let outside = base.join("outside");
        fs::create_dir_all(&ws).expect("mkdir ws");
        fs::create_dir_all(outside.join("deep")).expect("mkdir outside");
        fs::write(outside.join("secret.txt"), SECRET).expect("write secret");
        fs::write(outside.join("deep/more.txt"), SECRET).expect("write secret");
        Self {
            _dir: dir,
            base,
            ws,
            outside,
        }
    }

    /// Symlinks nach außen (`link_dir`, `link_file`) und Schleifen (`loop`,
    /// `nested/up`).
    pub fn plant_escapes(&self) {
        fs::create_dir_all(self.ws.join("nested")).expect("mkdir nested");
        symlink(&self.outside, self.ws.join("link_dir")).expect("symlink dir");
        symlink(self.outside.join("secret.txt"), self.ws.join("link_file"))
            .expect("symlink file");
        symlink(".", self.ws.join("loop")).expect("symlink loop");
        symlink("..", self.ws.join("nested/up")).expect("symlink up");
    }

    /// Ausführungskontext mit den angegebenen Rechten.
    pub fn ctx(&self, permissions: Vec<Permission>) -> ToolExecutionContext {
        let registry = WorkspaceRegistry::build(
            &self.base,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("t"),
                workspace: WorkspaceId::from_str("w"),
                root: PathBuf::from("ws"),
            }],
        )
        .expect("registry");
        let binding = registry
            .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("w"))
            .expect("binding");
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::from_policy(permissions));
        ToolExecutionContext::new(SessionId::new(), TurnId::new(), sandbox)
    }
}

/// Baut einen Tool-Aufruf.
pub fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
    ToolCall {
        id: ToolCallId::new(),
        name: ToolName::new(name),
        arguments,
    }
}

/// Serialisiert ein Tool-Ergebnis vollständig (für „enthält nicht“-Prüfungen).
pub fn render(output: &ToolOutput) -> String {
    serde_json::to_string(output).expect("serialize output")
}
