//! Test-Gerüst (nur `cfg(test)`): `TestError`/`TestResult`, ein
//! handgebautes Repository (lose Objekte, Refs, Index) ohne externes `git`,
//! Ausführungskontext und Aufruf-Helfer.

use crate::oid::{Oid, hash_object};
use flate2::Compression;
use flate2::write::ZlibEncoder;
use harw_authority::{
    Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
};
use harw_tools::{ToolCall, ToolExecutionContext, ToolExecutor, ToolName, ToolOutput};
use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
use serde_json::Value;
use sha1::{Digest, Sha1};
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

harw_test_support::define_test_error!(
    pub(crate),
    Io(std::io::Error) => "I/O error",
    Json(serde_json::Error) => "JSON error",
    Tools(harw_tools::ToolsError) => "tool error",
    Scope(harw_tool_fsread::scope::ScopeError) => "scope error"
);

impl From<String> for TestError {
    fn from(message: String) -> Self {
        Self::Unexpected(message)
    }
}

/// Handgebautes Repository im Workspace `ws/`.
pub(crate) struct TestRepo {
    _dir: TempDir,
    base: PathBuf,
    /// Workspace-Wurzel.
    pub(crate) ws: PathBuf,
    /// Nachbarverzeichnis außerhalb des Workspace.
    pub(crate) outside: PathBuf,
}

impl TestRepo {
    /// Workspace ohne `.git`.
    pub(crate) fn new_empty() -> TestResult<Self> {
        let dir = TempDir::new()?;
        let base = dir.path().canonicalize()?;
        let ws = base.join("ws");
        let outside = base.join("outside");
        std::fs::create_dir_all(&ws)?;
        std::fs::create_dir_all(&outside)?;
        Ok(Self {
            _dir: dir,
            base,
            ws,
            outside,
        })
    }

    /// Workspace mit leerem `.git/{objects,refs/heads,refs/tags}`.
    pub(crate) fn new() -> TestResult<Self> {
        let repo = Self::new_empty()?;
        for dir in ["objects", "refs/heads", "refs/tags", "refs/remotes"] {
            std::fs::create_dir_all(repo.ws.join(".git").join(dir))?;
        }
        Ok(repo)
    }

    /// Schreibt eine Datei im Git-Verzeichnis.
    pub(crate) fn write_git(&self, rel: &str, text: &str) -> TestResult {
        self.write_git_bytes(rel, text.as_bytes())
    }

    /// Schreibt Bytes in eine Datei im Git-Verzeichnis.
    pub(crate) fn write_git_bytes(&self, rel: &str, bytes: &[u8]) -> TestResult {
        let path = self.ws.join(".git").join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, bytes)?;
        Ok(())
    }

    /// Schreibt eine Arbeitsverzeichnisdatei.
    pub(crate) fn write(&self, rel: &str, bytes: &[u8]) -> TestResult {
        let path = self.ws.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, bytes)?;
        Ok(())
    }

    /// Legt ein loses Objekt an und liefert seine ID.
    pub(crate) fn write_loose(&self, kind: &str, data: &[u8]) -> TestResult<Oid> {
        let oid = hash_object(kind, data);
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        encoder.write_all(format!("{kind} {}\0", data.len()).as_bytes())?;
        encoder.write_all(data)?;
        let bytes = encoder.finish()?;
        let hex = oid.hex();
        self.write_git_bytes(&format!("objects/{}/{}", &hex[..2], &hex[2..]), &bytes)?;
        Ok(oid)
    }

    /// Blob.
    pub(crate) fn blob(&self, text: &str) -> TestResult<Oid> {
        self.write_loose("blob", text.as_bytes())
    }

    /// Baum aus `(modus, name, oid)`; die Reihenfolge entspricht Gits Sortierung
    /// (Verzeichnisse sortieren, als hätten sie ein `/` am Ende).
    pub(crate) fn tree(&self, entries: &[(u32, &str, Oid)]) -> TestResult<Oid> {
        let mut sorted: Vec<(u32, &str, Oid)> = entries.to_vec();
        sorted.sort_by_key(|(mode, name, _)| {
            let mut key = name.as_bytes().to_vec();
            if mode & 0o170_000 == 0o040_000 {
                key.push(b'/');
            }
            key
        });
        let mut data = Vec::new();
        for (mode, name, oid) in sorted {
            data.extend_from_slice(format!("{mode:o} {name}\0").as_bytes());
            data.extend_from_slice(oid.as_bytes());
        }
        self.write_loose("tree", &data)
    }

    /// Commit mit festen Signaturen; `when` in Unix-Sekunden.
    pub(crate) fn commit(
        &self,
        tree: Oid,
        parents: &[Oid],
        message: &str,
        when: i64,
    ) -> TestResult<Oid> {
        self.commit_as(tree, parents, message, when, "Ada", "ada@example.com")
    }

    /// Commit mit wählbarem Autor.
    pub(crate) fn commit_as(
        &self,
        tree: Oid,
        parents: &[Oid],
        message: &str,
        when: i64,
        name: &str,
        email: &str,
    ) -> TestResult<Oid> {
        let mut text = format!("tree {tree}\n");
        for parent in parents {
            text.push_str(&format!("parent {parent}\n"));
        }
        text.push_str(&format!("author {name} <{email}> {when} +0000\ncommitter {name} <{email}> {when} +0000\n\n{message}\n"));
        self.write_loose("commit", text.as_bytes())
    }

    /// Setzt eine Referenz (`refs/heads/main`).
    pub(crate) fn set_ref(&self, name: &str, oid: Oid) -> TestResult {
        self.write_git(name, &format!("{oid}\n"))
    }

    /// `HEAD` zeigt auf einen Branch.
    pub(crate) fn head_branch(&self, branch: &str) -> TestResult {
        self.write_git("HEAD", &format!("ref: refs/heads/{branch}\n"))
    }

    /// `HEAD` ist abgekoppelt.
    pub(crate) fn head_detached(&self, oid: Oid) -> TestResult {
        self.write_git("HEAD", &format!("{oid}\n"))
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

/// Baut einen Index (Version 2) aus `(Pfad, Modus, oid, Größe, mtime-Sekunden, mtime-Nanosekunden)`.
/// Die Einträge werden nach Pfad sortiert.
pub(crate) fn build_index(entries: &[(&str, u32, Oid, u32, u32, u32)]) -> Vec<u8> {
    let mut sorted: Vec<&(&str, u32, Oid, u32, u32, u32)> = entries.iter().collect();
    sorted.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    let mut out = Vec::new();
    out.extend_from_slice(b"DIRC");
    out.extend_from_slice(&2u32.to_be_bytes());
    out.extend_from_slice(&u32::try_from(sorted.len()).unwrap_or(0).to_be_bytes());
    for (path, mode, oid, size, mtime, mtime_nsec) in sorted {
        let start = out.len();
        out.extend_from_slice(&0u32.to_be_bytes()); // ctime sec
        out.extend_from_slice(&0u32.to_be_bytes()); // ctime nsec
        out.extend_from_slice(&mtime.to_be_bytes());
        out.extend_from_slice(&mtime_nsec.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes()); // dev
        out.extend_from_slice(&0u32.to_be_bytes()); // ino
        out.extend_from_slice(&mode.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes()); // uid
        out.extend_from_slice(&0u32.to_be_bytes()); // gid
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(oid.as_bytes());
        let name_len = u16::try_from(path.len().min(0xFFF)).unwrap_or(0xFFF);
        out.extend_from_slice(&name_len.to_be_bytes());
        out.extend_from_slice(path.as_bytes());
        // NUL-Auffüllung auf ein Vielfaches von 8 (mindestens ein NUL).
        let len = out.len() - start;
        let pad = 8 - len % 8;
        out.extend(std::iter::repeat_n(0u8, pad));
    }
    let checksum = Sha1::digest(&out);
    out.extend_from_slice(checksum.as_slice());
    out
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

/// Pfad eines Unterverzeichnisses (nur Lesehilfe für Tests).
pub(crate) fn join(base: &Path, rel: &str) -> PathBuf {
    base.join(rel)
}
