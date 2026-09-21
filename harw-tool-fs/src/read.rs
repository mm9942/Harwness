//! `fs.read` — Tool-Executor für lesenden Dateizugriff.
//!
//! # Verantwortung
//! Dieses Modul besitzt den `fs.read`-Executor:
//! - [`FsReadExecutor`]: implementiert [`ToolExecutor`]; liest eine Datei aus dem
//!   Workspace und erzwingt Permission-Check, symlinkfreies Öffnen unterhalb
//!   der Workspace-Wurzel (`open_beneath`), Typ-Check und Ausgabegrenze.
//!
//! # Kürzung statt Ablehnung (W1-02)
//! Dateien über dem Byte-Limit (höchstens 64 KiB) werden gekürzt geliefert.
//! Ein Hinweis am Ende nennt den gelieferten Bereich und den `offset` zum
//! Weiterlesen. Gekürzt wird an einer UTF-8-Zeichengrenze, sofern der Schnitt
//! ein Zeichen teilen würde. Gelesen wird über den geöffneten Deskriptor mit
//! `take(limit + 1)` — eine wachsende Datei sprengt den Speicher nicht.
//!
//! # Schlüsseltypen
//! - [`FsReadExecutor`]
//!
//! # Nebenläufigkeit
//! [`FsReadExecutor`] ist `Send + Sync` und über `Arc` teilbar.
//! `fs.read` ist `parallel_safe` (Reads sind commutative). Die Datei-IO läuft
//! über [`crate::blocking::run_blocking`] im Blocking-Pool.
//!
//! # Fehler
//! Permission-Fehler werden als `ToolOutput::error(...)` + `Ok(...)` signalisiert
//! (fail-closed ohne Panic). I/O- und Pfad-Fehler ebenfalls als Tool-Output;
//! Meldungen nennen nur den vom Modell übergebenen relativen Pfad.

use crate::error::FsToolError;
use crate::tree::{MAX_OUTPUT_BYTES, Workspace, normalize_relative};
use harw_authority::Permission;
use harw_tools::{
    ToolCall, ToolOutput,
    error::ToolsError,
    executor::{ToolExecutionContext, ToolExecutor, ToolExecutorFuture},
};
use serde::Deserialize;
use std::io::{Read, Seek, SeekFrom};

/// Deserialisierte Argumente für `fs.read`.
#[derive(Debug, Deserialize)]
struct FsReadArgs {
    /// Pfad relativ zum Workspace-Root.
    path: String,
    /// Optionales Byte-Limit für diesen Aufruf.
    #[serde(default, deserialize_with = "harw_extension_api::lenient::lenient_opt_u64")]
    max_bytes: Option<u64>,
    /// Optionaler Byte-Offset, ab dem gelesen wird (Default 0).
    #[serde(default, deserialize_with = "harw_extension_api::lenient::lenient_opt_u64")]
    offset: Option<u64>,
}

/// Führt `fs.read`-Aufrufe aus.
///
/// # Description
/// Liest eine Datei relativ zum Workspace-Root des Sandbox-Kontexts.
/// Prüft `ReadWorkspace`-Permission, öffnet die Datei über
/// `harw_fsutil::open_beneath` relativ zum Deskriptor der Workspace-Wurzel
/// (kein Pfadglied darf ein Symlink sein), prüft den Typ und liefert höchstens
/// `min(max_bytes, 64 KiB)` Bytes ab `offset` als [`ToolOutput::Text`].
///
/// # Arguments
/// - `max_bytes` (`u64`): konfiguriertes Byte-Maximum (Provider-Level); Werte
///   über 64 KiB werden auf 64 KiB begrenzt.
///
/// # Concurrency
/// `Send + Sync`; über [`std::sync::Arc`] teilbar. Keine Mutation von Shared State.
///
/// # Errors
/// Permission-Fehler → `Ok(ToolOutput::error(...))`.
/// I/O/Pfad-Fehler → `Ok(ToolOutput::error(...))`.
/// Ungültige JSON-Argumente → `Err(ToolsError::InvalidArguments)`.
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
    /// Führt den Lese-Vorgang synchron (blockierend) aus.
    ///
    /// # Errors
    /// Gibt nur `Err` bei einem Deserialisierungs-Fehler zurück (via
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

        let relative = match normalize_relative(&args.path) {
            Ok(rel) => rel,
            Err(reason) => return Ok(ToolOutput::error(format!("fs.read: {reason}"))),
        };

        // Symlinkfreies Öffnen relativ zum Wurzel-Deskriptor: kein TOCTOU
        // zwischen Prüfen und Öffnen, kein Folgen von Symlinks.
        let opened = Workspace::open(ctx.sandbox().workspace().canonical_root())
            .and_then(|workspace| workspace.open_any(&relative));
        let mut file = match opened {
            Ok(file) => file,
            Err(err) => {
                return Ok(ToolOutput::error(format!(
                    "fs.read: '{}' kann nicht geöffnet werden \
                     (Symlinks werden nicht verfolgt): {err}",
                    args.path
                )));
            }
        };
        let metadata = match file.metadata() {
            Ok(m) => m,
            Err(err) => return Ok(ToolOutput::error(FsToolError::Io(err).to_string())),
        };
        if !metadata.is_file() {
            return Ok(ToolOutput::error(
                FsToolError::NotAFile {
                    path: args.path.clone(),
                }
                .to_string(),
            ));
        }

        let output_cap = u64::try_from(MAX_OUTPUT_BYTES).unwrap_or(u64::MAX);
        let cap = args
            .max_bytes
            .unwrap_or(self.max_bytes)
            .min(self.max_bytes)
            .min(output_cap)
            .max(1);
        let size = metadata.len();
        let offset = args.offset.unwrap_or(0);
        if offset > size {
            return Ok(ToolOutput::error(format!(
                "fs.read: offset {offset} liegt hinter dem Dateiende ({size} Bytes)"
            )));
        }
        if let Err(err) = file.seek(SeekFrom::Start(offset)) {
            return Ok(ToolOutput::error(FsToolError::Io(err).to_string()));
        }

        // Ein Byte mehr lesen, um „es gibt weitere Bytes“ ohne `fstat`-Wettlauf
        // zu erkennen.
        let mut raw = Vec::new();
        if let Err(err) = (&file).take(cap.saturating_add(1)).read_to_end(&mut raw) {
            return Ok(ToolOutput::error(FsToolError::Io(err).to_string()));
        }
        let cap_len = usize::try_from(cap).unwrap_or(usize::MAX);
        let truncated = raw.len() > cap_len;
        if truncated {
            raw.truncate(cap_len);
            trim_partial_utf8(&mut raw);
        }

        let mut content = String::from_utf8_lossy(&raw).into_owned();
        if truncated {
            let end = offset.saturating_add(u64::try_from(raw.len()).unwrap_or(u64::MAX));
            content.push_str(&format!(
                "\n\n[fs.read: Ausgabe gekürzt auf {} Bytes (Bytes {offset}..{end} von {size}); \
                 weiterlesen mit offset={end}]",
                raw.len()
            ));
        }
        Ok(ToolOutput::text(content))
    }
}

/// Entfernt ein am Ende abgeschnittenes, unvollständiges UTF-8-Zeichen.
///
/// Nur wenn davor gültiger Text steht; sonst bleibt der Puffer unverändert
/// (verlustbehaftete Dekodierung, garantiert Fortschritt beim Weiterlesen).
fn trim_partial_utf8(raw: &mut Vec<u8>) {
    if let Err(err) = std::str::from_utf8(raw) {
        if err.error_len().is_none() && err.valid_up_to() > 0 {
            raw.truncate(err.valid_up_to());
        }
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
    /// Sicher für parallele Aufrufe; die Arbeit läuft im Blocking-Pool.
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        let executor = Self {
            max_bytes: self.max_bytes,
        };
        let context = context.clone();
        let call = call.clone();
        Box::pin(crate::blocking::run_blocking("fs.read", move || {
            executor.read_file(&context, &call)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, SECRET, call, render};
    use harw_authority::{Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
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
    fn test_fs_read_truncates_instead_of_rejecting() {
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
            ToolOutput::Text { content } => {
                assert!(content.starts_with(&"A".repeat(50)), "unexpected: {content}");
                assert!(!content.starts_with(&"A".repeat(51)), "unexpected: {content}");
                assert!(content.contains("gekürzt"), "missing hint: {content}");
                assert!(content.contains("offset=50"), "missing offset hint: {content}");
            }
            other => panic!("expected text output, got: {other:?}"),
        }

        // Weiterlesen ab dem gemeldeten Offset liefert den Rest ohne Hinweis.
        let call = make_call(serde_json::json!({ "path": "big.txt", "offset": 50 }));
        match executor.read_file(&ctx, &call).unwrap() {
            ToolOutput::Text { content } => assert_eq!(content, "A".repeat(50)),
            other => panic!("expected text output, got: {other:?}"),
        }
    }

    #[test]
    fn test_fs_read_caps_provider_limit_at_64_kib() {
        let fixture = Fixture::new();
        fs::write(fixture.ws.join("huge.txt"), "B".repeat(200_000)).unwrap();
        let executor = FsReadExecutor { max_bytes: u64::MAX };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace]);
        let call = call("fs.read", serde_json::json!({ "path": "huge.txt" }));
        match executor.read_file(&ctx, &call).unwrap() {
            ToolOutput::Text { content } => {
                assert!(content.starts_with(&"B".repeat(MAX_OUTPUT_BYTES)));
                assert!(!content.starts_with(&"B".repeat(MAX_OUTPUT_BYTES + 1)));
                assert!(content.contains(&format!("offset={MAX_OUTPUT_BYTES}")));
            }
            other => panic!("expected text output, got: {other:?}"),
        }
    }

    #[test]
    fn test_fs_read_truncation_respects_utf8_boundary() {
        let fixture = Fixture::new();
        fs::write(fixture.ws.join("umlaut.txt"), "aäb").unwrap(); // a, ä (2 Bytes), b
        let executor = FsReadExecutor { max_bytes: 2 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace]);
        let call = call("fs.read", serde_json::json!({ "path": "umlaut.txt" }));
        match executor.read_file(&ctx, &call).unwrap() {
            ToolOutput::Text { content } => {
                assert!(content.starts_with("a\n"), "unexpected: {content}");
                assert!(content.contains("offset=1"), "unexpected: {content}");
                assert!(!content.contains('\u{FFFD}'), "unexpected: {content}");
            }
            other => panic!("expected text output, got: {other:?}"),
        }
    }

    #[test]
    fn test_fs_read_offset_beyond_end_is_error() {
        let fixture = Fixture::new();
        fs::write(fixture.ws.join("short.txt"), "abc").unwrap();
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace]);
        let call = call("fs.read", serde_json::json!({ "path": "short.txt", "offset": 4 }));
        assert!(matches!(executor.read_file(&ctx, &call).unwrap(), ToolOutput::Error { .. }));
    }

    #[test]
    fn test_fs_read_rejects_symlinks_out_of_workspace() {
        let fixture = Fixture::new();
        fixture.plant_escapes();
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace]);

        let escapes = [
            "link_file",
            "link_dir/secret.txt",
            "loop/link_file",
            "nested/up/x",
            "../outside/secret.txt",
        ];
        for path in escapes {
            let call = call("fs.read", serde_json::json!({ "path": path }));
            let output = executor.read_file(&ctx, &call).unwrap();
            assert!(matches!(output, ToolOutput::Error { .. }), "{path}: {output:?}");
            let rendered = render(&output);
            assert!(!rendered.contains(SECRET), "{path}: {rendered}");
            assert!(!rendered.contains(fixture.outside.to_str().unwrap()), "{rendered}");
        }
    }

    #[test]
    fn test_fs_read_race_directory_replaced_by_symlink() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.ws.join("docs")).unwrap();
        fs::write(fixture.ws.join("docs/secret.txt"), "harmless").unwrap();
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace]);
        let call = call("fs.read", serde_json::json!({ "path": "docs/secret.txt" }));
        assert!(matches!(executor.read_file(&ctx, &call).unwrap(), ToolOutput::Text { .. }));

        // Wettlauf-Surrogat: das geprüfte Verzeichnis wird durch einen Symlink
        // nach außen ersetzt; derselbe Aufruf muss jetzt scheitern.
        fs::rename(fixture.ws.join("docs"), fixture.ws.join("docs_old")).unwrap();
        std::os::unix::fs::symlink(&fixture.outside, fixture.ws.join("docs")).unwrap();
        let output = executor.read_file(&ctx, &call).unwrap();
        assert!(matches!(output, ToolOutput::Error { .. }), "{output:?}");
        assert!(!render(&output).contains(SECRET));
    }

    #[test]
    fn test_fs_read_execute_runs_on_runtime() {
        let fixture = Fixture::new();
        fs::write(fixture.ws.join("async.txt"), "via spawn_blocking").unwrap();
        let executor = FsReadExecutor { max_bytes: 65_536 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace]);
        let call = call("fs.read", serde_json::json!({ "path": "async.txt" }));
        let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
        match runtime.block_on(executor.execute(&ctx, &call)).unwrap() {
            ToolOutput::Text { content } => assert_eq!(content, "via spawn_blocking"),
            other => panic!("expected text output, got: {other:?}"),
        }
    }
}
