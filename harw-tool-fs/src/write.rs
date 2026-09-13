//! `fs.write` — Tool-Executor für schreibenden Dateizugriff.
//!
//! # Verantwortung
//! Dieses Modul besitzt den `fs.write`-Executor:
//! - [`FsWriteExecutor`]: implementiert [`ToolExecutor`]; schreibt eine Datei in
//!   den Workspace. Prüft `WriteWorkspace`-Permission und delegiert Path-Resolution
//!   an `ctx.sandbox().workspace().resolve_for_create()`.
//!
//! # Schlüsseltypen
//! - [`FsWriteExecutor`]
//!
//! # Nebenläufigkeit
//! [`FsWriteExecutor`] ist `Send + Sync`. `fs.write` ist NICHT `parallel_safe`
//! (Writes sind nicht commutative).
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
use serde::Deserialize;
use std::{
    fs::{self as std_fs, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const TEMP_FILE_PREFIX: &str = ".harw-write-";

fn atomic_write(path: &Path, content: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "destination path has no parent directory",
        )
    })?;
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "destination path has no file name",
        )
    })?;
    let existing_permissions = std_fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());

    let (temp_path, mut temp_file) = create_temp_file(parent, file_name)?;
    let write_result = (|| {
        temp_file.write_all(content)?;
        temp_file.sync_all()?;

        if let Some(permissions) = existing_permissions {
            temp_file.set_permissions(permissions)?;
            temp_file.sync_all()?;
        }

        drop(temp_file);
        std_fs::rename(&temp_path, path)?;
        File::open(parent)?.sync_all()
    })();

    if write_result.is_err() {
        let _ = std_fs::remove_file(&temp_path);
    }

    write_result
}

fn create_temp_file(parent: &Path, file_name: &std::ffi::OsStr) -> io::Result<(PathBuf, File)> {
    for _ in 0..32 {
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temp_path = parent.join(format!(
            "{TEMP_FILE_PREFIX}{}-{sequence}-{}",
            std::process::id(),
            file_name.to_string_lossy()
        ));

        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique temporary file",
    ))
}

/// Deserialisierte Argumente für `fs.write`.
#[derive(Debug, Deserialize)]
struct FsWriteArgs {
    /// Pfad relativ zum Workspace-Root.
    path: String,
    /// Inhalt, der in die Datei geschrieben wird.
    content: String,
}

/// Führt `fs.write`-Aufrufe aus.
///
/// # Description
/// Schreibt `content` in eine Datei relativ zum Workspace-Root des
/// Sandbox-Kontexts. Prüft `WriteWorkspace`-Permission und delegiert
/// Path-Resolution an `ctx.sandbox().workspace().resolve_for_create()`.
///
/// # Concurrency
/// `Send + Sync`; über [`std::sync::Arc`] teilbar. Schreiboperationen sind
/// NICHT commutative — `fs.write` ist NICHT `parallel_safe`.
///
/// # Errors
/// Permission-Fehler → `Ok(ToolOutput::error(...))`.
/// I/O/Arg-Fehler → `Ok(ToolOutput::error(...))`.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_fs::FsWriteExecutor;
/// let _executor = FsWriteExecutor;
/// ```
pub struct FsWriteExecutor;

impl FsWriteExecutor {
    /// Führt den Schreib-Vorgang synchron aus und gibt ein [`ToolOutput`] zurück.
    ///
    /// # Errors
    /// Gibt `Err(ToolsError::InvalidArguments)` bei ungültigen JSON-Argumenten.
    /// Alle anderen Fehler → `Ok(ToolOutput::error(...))`.
    fn write_file(
        &self,
        ctx: &ToolExecutionContext,
        call: &ToolCall,
    ) -> Result<ToolOutput, ToolsError> {
        // Permission check — fail closed
        if let Some(err) = harw_tools::sandbox_guard::require_permission(
            ctx,
            Permission::WriteWorkspace,
            "fs.write",
        ) {
            return Ok(err);
        }

        // Parse arguments
        let args: FsWriteArgs = match serde_json::from_value::<FsWriteArgs>(call.arguments.clone())
        {
            Ok(a) => a,
            Err(err) => {
                return Err(ToolsError::InvalidArguments {
                    name: "fs.write".to_owned(),
                    reason: err.to_string(),
                });
            }
        };

        // Safe path resolution via sandbox (creates parent if needed via resolve_for_create)
        let relative = PathBuf::from(&args.path);
        let resolved = match ctx.sandbox().workspace().resolve_for_create(&relative) {
            Ok(p) => p,
            Err(err) => {
                return Ok(ToolOutput::error(err.to_string()));
            }
        };

        // Write via a durable same-directory temporary file and atomic rename.
        let byte_count = args.content.len();
        match atomic_write(&resolved, args.content.as_bytes()) {
            Ok(()) => Ok(ToolOutput::text(format!(
                "wrote {byte_count} bytes to {}",
                args.path
            ))),
            Err(err) => Ok(ToolOutput::error(FsToolError::Io(err).to_string())),
        }
    }
}

impl ToolExecutor for FsWriteExecutor {
    /// Führt eine `fs.write`-Invokation aus.
    ///
    /// # Arguments
    /// - `context` (`&ToolExecutionContext`): harness-etablierte Autorität mit
    ///   Sandbox-Spec (Permissions + Workspace).
    /// - `call` (`&ToolCall`): die ungeprüfte Invokation mit JSON-Argumenten.
    ///
    /// # Returns
    /// `Ok(ToolOutput::Text { content: "wrote N bytes to path" })` bei Erfolg.
    /// `Ok(ToolOutput::Error { message })` bei Permission- oder I/O-Fehler.
    ///
    /// # Errors
    /// - [`ToolsError::InvalidArguments`]: fehlende oder fehlerhafte JSON-Argumente.
    ///
    /// # Concurrency
    /// Nicht parallel-safe (Writes nicht commutative).
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        let result = self.write_file(context, call);
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
            name: harw_tools::spec::ToolName::new("fs.write"),
            arguments: args,
        }
    }

    #[test]
    fn test_fs_write_writes_file_successfully() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();

        let executor = FsWriteExecutor;
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::WriteWorkspace]);
        let ctx = make_ctx(sandbox);
        let call = make_call(serde_json::json!({
            "path": "output.txt",
            "content": "hello from test"
        }));

        let result = executor.write_file(&ctx, &call).unwrap();
        match &result {
            ToolOutput::Text { content } => {
                assert!(content.contains("15"), "expected byte count 15: {content}");
                assert!(content.contains("output.txt"), "unexpected: {content}");
            }
            other => panic!("expected text output, got: {other:?}"),
        }

        // Verify the file was actually written
        let written = fs::read_to_string(ws.join("output.txt")).unwrap();
        assert_eq!(written, "hello from test");
        assert!(
            fs::read_dir(&ws).unwrap().all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(TEMP_FILE_PREFIX)),
            "temporary write file should be renamed or cleaned up"
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_fs_write_preserves_existing_file_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        let target = ws.join("private.txt");
        fs::write(&target, "old content").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();

        let executor = FsWriteExecutor;
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::WriteWorkspace]);
        let ctx = make_ctx(sandbox);
        let call = make_call(serde_json::json!({
            "path": "private.txt",
            "content": "replacement"
        }));

        let result = executor.write_file(&ctx, &call).unwrap();
        assert!(matches!(result, ToolOutput::Text { .. }));
        assert_eq!(fs::read_to_string(&target).unwrap(), "replacement");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn test_fs_write_denied_when_no_write_permission() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();

        let executor = FsWriteExecutor;
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![]);
        let ctx = make_ctx(sandbox);
        let call = make_call(serde_json::json!({
            "path": "any.txt",
            "content": "data"
        }));

        let result = executor.write_file(&ctx, &call).unwrap();
        match result {
            ToolOutput::Error { message } => {
                assert!(message.contains("WriteWorkspace"), "unexpected: {message}");
            }
            other => panic!("expected error output, got: {other:?}"),
        }
    }

    #[test]
    fn test_fs_write_invalid_args_returns_err() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();

        let executor = FsWriteExecutor;
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::WriteWorkspace]);
        let ctx = make_ctx(sandbox);
        // Missing "content"
        let call = make_call(serde_json::json!({ "path": "out.txt" }));

        let result = executor.write_file(&ctx, &call);
        assert!(
            matches!(result, Err(ToolsError::InvalidArguments { .. })),
            "expected InvalidArguments, got: {result:?}"
        );
    }
}
