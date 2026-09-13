//! `fs.read` — Tool-Executor für lesenden Dateizugriff.
//!
//! # Verantwortung
//! Dieses Modul besitzt den `fs.read`-Executor:
//! - [`FsReadExecutor`]: implementiert [`ToolExecutor`]; liest eine Datei aus dem
//!   Workspace und erzwingt Permission-Check, Path-Traversal-Schutz, Typ-Check
//!   und Größenlimit.
//!
//! # Schlüsseltypen
//! - [`FsReadExecutor`]
//!
//! # Nebenläufigkeit
//! [`FsReadExecutor`] ist `Send + Sync` und über `Arc` teilbar.
//! `fs.read` ist `parallel_safe` (Reads sind commutative).
//!
//! # Fehler
//! Permission-Fehler werden als `ToolOutput::error(...)` + `Ok(...)` signalisiert
//! (fail-closed ohne Panic). I/O- und Arg-Fehler ebenfalls als Tool-Output.

use crate::error::FsToolError;
use harw_sandbox::Permission;
use harw_tools::{
    ToolCall, ToolOutput,
    error::ToolsError,
    executor::{ToolExecutionContext, ToolExecutor, ToolExecutorFuture},
};
use serde::Deserialize;
use std::path::PathBuf;

/// Deserialisierte Argumente für `fs.read`.
#[derive(Debug, Deserialize)]
struct FsReadArgs {
    /// Pfad relativ zum Workspace-Root.
    path: String,
    /// Optionales Byte-Limit für diesen Aufruf.
    max_bytes: Option<u64>,
}

/// Führt `fs.read`-Aufrufe aus.
///
/// # Description
/// Liest eine Datei relativ zum Workspace-Root des Sandbox-Kontexts.
/// Prüft `ReadWorkspace`-Permission, delegiert Path-Resolution an
/// `ctx.sandbox().workspace().resolve_existing()`, prüft Typ und Größenlimit,
/// und gibt den Inhalt als [`ToolOutput::Text`] zurück.
///
/// # Arguments
/// - `max_bytes` (`u64`): konfiguriertes Byte-Maximum (Provider-Level).
///
/// # Concurrency
/// `Send + Sync`; über [`std::sync::Arc`] teilbar. Keine Mutation von Shared State.
///
/// # Errors
/// Permission-Fehler → `Ok(ToolOutput::error(...))`.
/// I/O/Arg-Fehler → `Ok(ToolOutput::error(...))`.
/// Executor-Panic → nie.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_fs::FsReadExecutor;
/// let _executor = FsReadExecutor { max_bytes: 65_536 };
/// ```
pub struct FsReadExecutor {
    /// Maximale Bytes, die pro Aufruf gelesen werden.
    pub max_bytes: u64,
}

impl FsReadExecutor {
    /// Führt den Lese-Vorgang synchron aus und gibt ein [`ToolOutput`] zurück.
    ///
    /// # Errors
    /// Gibt nur `Err` bei einem internen Deserialisierungs-Fehler zurück (via
    /// [`ToolsError::InvalidArguments`]). Alle anderen Fehler werden in
    /// `Ok(ToolOutput::error(...))` gewandelt.
    fn read_file(
        &self,
        ctx: &ToolExecutionContext,
        call: &ToolCall,
    ) -> Result<ToolOutput, ToolsError> {
        // Permission check — fail closed
        if let Some(err) =
            harw_tools::sandbox_guard::require_permission(ctx, Permission::ReadWorkspace, "fs.read")
        {
            return Ok(err);
        }

        // Parse arguments
        let args: FsReadArgs = match serde_json::from_value::<FsReadArgs>(call.arguments.clone()) {
            Ok(a) => a,
            Err(err) => {
                return Err(ToolsError::InvalidArguments {
                    name: "fs.read".to_owned(),
                    reason: err.to_string(),
                });
            }
        };

        // Safe path resolution via sandbox
        let relative = PathBuf::from(&args.path);
        let resolved = match ctx.sandbox().workspace().resolve_existing(&relative) {
            Ok(p) => p,
            Err(err) => {
                return Ok(ToolOutput::error(err.to_string()));
            }
        };

        // Verify it is a regular file
        let metadata = match resolved.metadata() {
            Ok(m) => m,
            Err(err) => return Ok(ToolOutput::error(FsToolError::Io(err).to_string())),
        };
        if !metadata.is_file() {
            return Ok(ToolOutput::error(
                FsToolError::NotAFile {
                    path: resolved.display().to_string(),
                }
                .to_string(),
            ));
        }

        // Apply byte limit
        let cap = args.max_bytes.unwrap_or(self.max_bytes).min(self.max_bytes);
        let file_size = metadata.len();
        if file_size > cap {
            return Ok(ToolOutput::error(
                FsToolError::TooLarge {
                    size: file_size,
                    max: cap,
                }
                .to_string(),
            ));
        }

        // Read contents
        let raw = match std::fs::read(&resolved) {
            Ok(b) => b,
            Err(err) => return Ok(ToolOutput::error(FsToolError::Io(err).to_string())),
        };
        let content = String::from_utf8_lossy(&raw).into_owned();
        Ok(ToolOutput::text(content))
    }
}

impl ToolExecutor for FsReadExecutor {
    /// Führt eine `fs.read`-Invokation aus.
    ///
    /// # Arguments
    /// - `context` (`&ToolExecutionContext`): harness-etablierte Autorität mit
    ///   Sandbox-Spec (Permissions + Workspace).
    /// - `call` (`&ToolCall`): die ungeprüfte Invokation mit JSON-Argumenten.
    ///
    /// # Returns
    /// `Ok(ToolOutput::Text { content })` bei Erfolg.
    /// `Ok(ToolOutput::Error { message })` bei Permission- oder I/O-Fehler.
    ///
    /// # Errors
    /// - [`ToolsError::InvalidArguments`]: fehlende oder fehlerhafte JSON-Argumente.
    ///
    /// # Concurrency
    /// Sicher für parallele Aufrufe; keine Mutation von Shared State.
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        let result = self.read_file(context, call);
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
            name: harw_tools::spec::ToolName::new("fs.read"),
            arguments: args,
        }
    }

    #[test]
    fn test_fs_read_reads_file_successfully() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("hello.txt"), "hello world").unwrap();

        let executor = FsReadExecutor { max_bytes: 65_536 };
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace]);
        let ctx = make_ctx(sandbox);
        let call = make_call(serde_json::json!({ "path": "hello.txt" }));

        let result = executor.read_file(&ctx, &call).unwrap();
        match result {
            ToolOutput::Text { content } => assert_eq!(content, "hello world"),
            other => panic!("expected text output, got: {other:?}"),
        }
    }

    #[test]
    fn test_fs_read_denied_when_no_read_permission() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();

        let executor = FsReadExecutor { max_bytes: 65_536 };
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![]);
        let ctx = make_ctx(sandbox);
        let call = make_call(serde_json::json!({ "path": "any.txt" }));

        let result = executor.read_file(&ctx, &call).unwrap();
        match result {
            ToolOutput::Error { message } => {
                assert!(message.contains("ReadWorkspace"), "unexpected: {message}");
            }
            other => panic!("expected error output, got: {other:?}"),
        }
    }

    #[test]
    fn test_fs_read_invalid_args_returns_err() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();

        let executor = FsReadExecutor { max_bytes: 65_536 };
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace]);
        let ctx = make_ctx(sandbox);
        // Missing "path"
        let call = make_call(serde_json::json!({ "max_bytes": 100 }));

        let result = executor.read_file(&ctx, &call);
        assert!(
            matches!(result, Err(ToolsError::InvalidArguments { .. })),
            "expected InvalidArguments, got: {result:?}"
        );
    }

    #[test]
    fn test_fs_read_rejects_missing_file() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();

        let executor = FsReadExecutor { max_bytes: 65_536 };
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace]);
        let ctx = make_ctx(sandbox);
        let call = make_call(serde_json::json!({ "path": "ghost.txt" }));

        let result = executor.read_file(&ctx, &call).unwrap();
        assert!(matches!(result, ToolOutput::Error { .. }));
    }

    #[test]
    fn test_fs_read_rejects_directory() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(ws.join("subdir")).unwrap();

        let executor = FsReadExecutor { max_bytes: 65_536 };
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace]);
        let ctx = make_ctx(sandbox);
        let call = make_call(serde_json::json!({ "path": "subdir" }));

        let result = executor.read_file(&ctx, &call).unwrap();
        assert!(matches!(result, ToolOutput::Error { .. }));
    }

    #[test]
    fn test_fs_read_respects_max_bytes() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("big.txt"), "A".repeat(100)).unwrap();

        let executor = FsReadExecutor { max_bytes: 50 };
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace]);
        let ctx = make_ctx(sandbox);
        let call = make_call(serde_json::json!({ "path": "big.txt" }));

        let result = executor.read_file(&ctx, &call).unwrap();
        match result {
            ToolOutput::Error { message } => {
                assert!(message.contains("too large"), "unexpected: {message}");
            }
            other => panic!("expected error output, got: {other:?}"),
        }
    }
}
