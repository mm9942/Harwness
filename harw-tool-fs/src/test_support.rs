//! Gemeinsame Test-Hilfen für die W1-02-Sicherheitstests (nur `cfg(test)`).
//!
//! [`Fixture`] legt einen Workspace `ws/` und daneben ein fremdes
//! Verzeichnis `outside/` mit Geheimnissen an; [`Fixture::plant_escapes`]
//! erzeugt echte Symlinks aus dem Workspace hinaus sowie Symlink-Schleifen.
//!
//! Test-Fehlertyp dieses Crates: ersetzt `panic!`/`.unwrap()`/`.expect(…)` in
//! Tests (Bible R087/R165/R182). Jeder Fehlschlag wird als `Err`
//! zurückgegeben statt zu paniken.

use harw_authority::{
    Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
};
use harw_tools::{ToolCall, ToolExecutionContext, ToolName, ToolOutput, ToolsError};
use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
use std::fmt;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use tempfile::TempDir;

/// Inhalt der Geheimdateien außerhalb des Workspace.
pub const SECRET: &str = "TOPSECRET";

/// Fehler eines Tests; jeder Fehlschlag wird als `Err` zurückgegeben statt zu
/// paniken (Bible R087/R165/R182).
pub(crate) enum TestError {
    /// Ein erwarteter Wert fehlte (`Option` war `None`).
    Missing(&'static str),
    /// Ein Ergebnis hatte eine unerwartete Form.
    Unexpected(String),
    /// Ein Fehler mit Kontext (ersetzt `.expect("…")`).
    Context {
        /// Kurzbeschreibung der fehlgeschlagenen Operation.
        context: &'static str,
        /// Textform des ursprünglichen Fehlers.
        source: String,
    },
    /// I/O-Fehler beim Aufbau der Testumgebung oder beim Prüfen von Dateien.
    Io(std::io::Error),
    /// JSON-(De-)Serialisierungsfehler.
    Json(serde_json::Error),
    /// Fehler aus der Tool-Ausführung (`harw-tools`).
    Tools(ToolsError),
}

/// Kurzform für `Result<T, TestError>` in Tests dieses Crates.
pub(crate) type TestResult<T = ()> = Result<T, TestError>;

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(what) => write!(f, "erwarteter Wert fehlte: {what}"),
            Self::Unexpected(msg) => write!(f, "unerwartetes Ergebnis: {msg}"),
            Self::Context { context, source } => write!(f, "{context}: {source}"),
            Self::Io(err) => write!(f, "I/O-Fehler: {err}"),
            Self::Json(err) => write!(f, "JSON-Fehler: {err}"),
            Self::Tools(err) => write!(f, "Tool-Fehler: {err}"),
        }
    }
}

impl fmt::Debug for TestError {
    /// Delegiert an [`fmt::Display`] (Bible: keine doppelte Fehlerdarstellung).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for TestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Json(err) => Some(err),
            Self::Tools(err) => Some(err),
            Self::Missing(_) | Self::Unexpected(_) | Self::Context { .. } => None,
        }
    }
}

impl From<std::io::Error> for TestError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<serde_json::Error> for TestError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

impl From<ToolsError> for TestError {
    fn from(err: ToolsError) -> Self {
        Self::Tools(err)
    }
}

/// Übersetzt einen Fremdfehler mit Kontext in einen [`TestError::Context`]
/// (ersetzt `.expect("…")` auf `Result`-Werten).
pub(crate) fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |source| TestError::Context {
        context,
        source: source.to_string(),
    }
}

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
    pub fn new() -> TestResult<Self> {
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

    /// Symlinks nach außen (`link_dir`, `link_file`) und Schleifen (`loop`,
    /// `nested/up`).
    pub fn plant_escapes(&self) -> TestResult {
        fs::create_dir_all(self.ws.join("nested"))?;
        symlink(&self.outside, self.ws.join("link_dir"))?;
        symlink(self.outside.join("secret.txt"), self.ws.join("link_file"))?;
        symlink(".", self.ws.join("loop"))?;
        symlink("..", self.ws.join("nested/up"))?;
        Ok(())
    }

    /// Ausführungskontext mit den angegebenen Rechten.
    pub fn ctx(&self, permissions: Vec<Permission>) -> TestResult<ToolExecutionContext> {
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
pub fn render(output: &ToolOutput) -> TestResult<String> {
    Ok(serde_json::to_string(output)?)
}
