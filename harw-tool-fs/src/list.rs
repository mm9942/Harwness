//! `fs.list` — Tool-Executor für Verzeichnis-Listing.
//!
//! # Verantwortung
//! Dieses Modul besitzt den `fs.list`-Executor:
//! - [`FsListExecutor`]: implementiert [`ToolExecutor`]; listet den Inhalt eines
//!   Verzeichnisses als JSON-Array auf. Prüft `ReadWorkspace`-Permission und
//!   delegiert Path-Resolution an `ctx.sandbox().workspace().resolve_existing()`.
//!
//! # Schlüsseltypen
//! - [`FsListExecutor`]
//!
//! # Nebenläufigkeit
//! [`FsListExecutor`] ist `Send + Sync`. `fs.list` ist `parallel_safe`.
//!
//! # Fehler
//! Permission-Fehler → `Ok(ToolOutput::error(...))`. Kein Panic.

use crate::error::FsToolError;
use harw_sandbox::Permission;
use harw_tools::{
    ToolCall, ToolOutput,
    error::ToolsError,
    executor::{ToolExecutionContext, ToolExecutor, ToolExecutorFuture},
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Standard-Maximum für Verzeichnis-Einträge.
pub const DEFAULT_MAX_ENTRIES: usize = 200;

/// Deserialisierte Argumente für `fs.list`.
#[derive(Debug, Deserialize)]
struct FsListArgs {
    /// Pfad relativ zum Workspace-Root (Verzeichnis).
    path: String,
    /// Maximale Anzahl Einträge (Default: [`DEFAULT_MAX_ENTRIES`]).
    max_entries: Option<usize>,
}

/// Ein Eintrag im Verzeichnis-Listing.
#[derive(Debug, Serialize)]
struct DirEntry {
    /// Dateiname (ohne Pfad-Prefix).
    name: String,
    /// Typ: `"file"` oder `"dir"` (unbekannte Typen werden als `"other"` gemeldet).
    kind: String,
    /// Dateigröße in Bytes (0 für Verzeichnisse).
    size: u64,
}

/// Führt `fs.list`-Aufrufe aus.
///
/// # Description
/// Listet den Inhalt eines Verzeichnisses relativ zum Workspace-Root des
/// Sandbox-Kontexts. Prüft `ReadWorkspace`-Permission und delegiert
/// Path-Resolution an `ctx.sandbox().workspace().resolve_existing()`.
///
/// # Arguments
/// - `max_entries` (`usize`): konfiguriertes Maximum für Verzeichnis-Einträge.
///
/// # Concurrency
/// `Send + Sync`; über [`std::sync::Arc`] teilbar. Reads sind commutative →
/// `fs.list` ist `parallel_safe`.
///
/// # Errors
/// Permission-Fehler → `Ok(ToolOutput::error(...))`. Kein Panic.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_fs::FsListExecutor;
/// let _executor = FsListExecutor { max_entries: 200 };
/// ```
pub struct FsListExecutor {
    /// Maximale Anzahl zurückgegebener Einträge.
    pub max_entries: usize,
}

impl FsListExecutor {
    /// Führt das Listing synchron aus.
    ///
    /// # Errors
    /// `Err(ToolsError::InvalidArguments)` bei fehlerhaften JSON-Argumenten.
    /// Alle anderen Fehler → `Ok(ToolOutput::error(...))`.
    fn list_dir(
        &self,
        ctx: &ToolExecutionContext,
        call: &ToolCall,
    ) -> Result<ToolOutput, ToolsError> {
        // Permission check
        if let Some(err) =
            harw_tools::sandbox_guard::require_permission(ctx, Permission::ReadWorkspace, "fs.list")
        {
            return Ok(err);
        }

        // Parse arguments
        let args: FsListArgs = match serde_json::from_value::<FsListArgs>(call.arguments.clone()) {
            Ok(a) => a,
            Err(err) => {
                return Err(ToolsError::InvalidArguments {
                    name: "fs.list".to_owned(),
                    reason: err.to_string(),
                });
            }
        };

        // Resolve path
        let relative = PathBuf::from(&args.path);
        let resolved = match ctx.sandbox().workspace().resolve_existing(&relative) {
            Ok(p) => p,
            Err(err) => return Ok(ToolOutput::error(err.to_string())),
        };

        // Verify it is a directory
        if !resolved.is_dir() {
            return Ok(ToolOutput::error(
                FsToolError::NotADirectory {
                    path: resolved.display().to_string(),
                }
                .to_string(),
            ));
        }

        // Read directory entries
        let cap = args
            .max_entries
            .unwrap_or(self.max_entries)
            .min(self.max_entries);
        let mut entries = Vec::new();

        let read_dir = match std::fs::read_dir(&resolved) {
            Ok(rd) => rd,
            Err(err) => return Ok(ToolOutput::error(FsToolError::Io(err).to_string())),
        };

        for entry_result in read_dir.take(cap) {
            let entry = match entry_result {
                Ok(e) => e,
                Err(err) => return Ok(ToolOutput::error(FsToolError::Io(err).to_string())),
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => {
                    entries.push(DirEntry {
                        name,
                        kind: "other".to_owned(),
                        size: 0,
                    });
                    continue;
                }
            };
            let kind = if meta.is_file() {
                "file"
            } else if meta.is_dir() {
                "dir"
            } else {
                "other"
            };
            entries.push(DirEntry {
                name,
                kind: kind.to_owned(),
                size: if meta.is_file() { meta.len() } else { 0 },
            });
        }

        let json = serde_json::to_value(entries).unwrap_or(serde_json::Value::Array(vec![]));
        Ok(ToolOutput::json(json))
    }
}

impl ToolExecutor for FsListExecutor {
    /// Führt eine `fs.list`-Invokation aus.
    ///
    /// # Arguments
    /// - `context` (`&ToolExecutionContext`): harness-etablierte Autorität mit
    ///   Sandbox-Spec.
    /// - `call` (`&ToolCall`): die ungeprüfte Invokation mit JSON-Argumenten.
    ///
    /// # Returns
    /// `Ok(ToolOutput::Json { content: [...] })` bei Erfolg.
    /// `Ok(ToolOutput::Error { message })` bei Permission- oder I/O-Fehler.
    ///
    /// # Errors
    /// - [`ToolsError::InvalidArguments`]: fehlende oder fehlerhafte JSON-Argumente.
    ///
    /// # Concurrency
    /// Sicher für parallele Aufrufe; `fs.list` ist `parallel_safe`.
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        let result = self.list_dir(context, call);
        Box::pin(async move { result })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_sandbox::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn make_sandbox_with_permissions(root: &Path, permissions: Vec<Permission>) -> SandboxSpec {
        let ws_dir = root.join("ws");
        fs::create_dir_all(&ws_dir).unwrap();
        let registry = WorkspaceRegistry::build(
            root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("t"),
                workspace: WorkspaceId::from_str("w"),
                root: PathBuf::from("ws"),
            }],
        )
        .unwrap();
        let binding = registry
            .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("w"))
            .unwrap();
        SandboxSpec::from_resolved(binding, PermissionSet::from_policy(permissions))
    }

    fn make_ctx(sandbox: SandboxSpec) -> ToolExecutionContext {
        ToolExecutionContext::new(SessionId::new(), TurnId::new(), sandbox)
    }

    fn make_call(args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: ToolCallId::new(),
            name: harw_tools::spec::ToolName::new("fs.list"),
            arguments: args,
        }
    }

    #[test]
    fn test_fs_list_lists_directory_successfully() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("a.txt"), "aaa").unwrap();
        fs::write(ws.join("b.txt"), "bb").unwrap();
        fs::create_dir_all(ws.join("subdir")).unwrap();

        let executor = FsListExecutor {
            max_entries: DEFAULT_MAX_ENTRIES,
        };
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace]);
        let ctx = make_ctx(sandbox);
        let call = make_call(serde_json::json!({ "path": "." }));

        let result = executor.list_dir(&ctx, &call).unwrap();
        match result {
            ToolOutput::Json { content } => {
                let arr = content.as_array().expect("should be array");
                assert!(!arr.is_empty(), "expected entries");
            }
            other => panic!("expected json output, got: {other:?}"),
        }
    }

    #[test]
    fn test_fs_list_denied_when_no_read_permission() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();

        let executor = FsListExecutor {
            max_entries: DEFAULT_MAX_ENTRIES,
        };
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![]);
        let ctx = make_ctx(sandbox);
        let call = make_call(serde_json::json!({ "path": "." }));

        let result = executor.list_dir(&ctx, &call).unwrap();
        match result {
            ToolOutput::Error { message } => {
                assert!(message.contains("ReadWorkspace"), "unexpected: {message}");
            }
            other => panic!("expected error output, got: {other:?}"),
        }
    }

    #[test]
    fn test_fs_list_invalid_args_returns_err() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();

        let executor = FsListExecutor {
            max_entries: DEFAULT_MAX_ENTRIES,
        };
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace]);
        let ctx = make_ctx(sandbox);
        // Missing "path"
        let call = make_call(serde_json::json!({ "max_entries": 5 }));

        let result = executor.list_dir(&ctx, &call);
        assert!(
            matches!(result, Err(ToolsError::InvalidArguments { .. })),
            "expected InvalidArguments, got: {result:?}"
        );
    }
}
