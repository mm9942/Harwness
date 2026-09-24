//! `fs.write` — Tool-Executor für schreibenden Dateizugriff.
//!
//! # Verantwortung
//! Dieses Modul besitzt den `fs.write`-Executor:
//! - [`FsWriteExecutor`]: implementiert [`ToolExecutor`]; schreibt eine Datei in
//!   den Workspace. Prüft `WriteWorkspace`-Permission, lehnt geschützte Pfade
//!   ab, öffnet das Elternverzeichnis symlinkfrei unterhalb der
//!   Workspace-Wurzel und schreibt atomar über [`harw_fsutil::write_atomic`].
//!
//! # Sicherheitsregeln (W1-02)
//! - **Geschützte Pfade:** jede Pfadkomponente `.git` oder `.harw`
//!   (ASCII-Groß-/Kleinschreibung egal) wird abgelehnt — Git-Hooks und
//!   `.git/config` führen sonst Code auf dem Host aus, `.harw/` ist die
//!   repo-lokale Konfigurationsebene.
//! - **Elternverzeichnis:** wird mit `open_beneath` geöffnet (kein Pfadglied
//!   darf ein Symlink sein) und `write_atomic` über `/proc/self/fd/<fd>`
//!   übergeben, sodass ein nachträglicher Tausch eines Verzeichnisses gegen
//!   einen Symlink den Schreibort nicht mehr umlenkt (Rückfall ohne `/proc`:
//!   Pfad unter der Wurzel mit dokumentiertem Restfenster, siehe
//!   `tree`-Modul).
//! - **Zielname:** ein Symlink am Ziel wird ersetzt, nie gefolgt. Rechte-Bits
//!   werden nur von einer vorhandenen **regulären** Datei übernommen
//!   (`lstat`, `& 0o777`), nie vom Ziel eines Symlinks; neue Dateien erhalten
//!   `0644`.
//! - Elternverzeichnisse werden nicht angelegt.
//!
//! # Schlüsseltypen
//! - [`FsWriteExecutor`]
//!
//! # Nebenläufigkeit
//! [`FsWriteExecutor`] ist `Send + Sync`. `fs.write` ist NICHT `parallel_safe`
//! (Writes sind nicht commutative). Die Datei-IO läuft im Blocking-Pool.
//!
//! # Fehler
//! Permission-Fehler → `Ok(ToolOutput::error(...))`. Kein Panic.

use crate::error::FsToolError;
use crate::symlink::resolve_for_write;
use crate::tree::{Workspace, normalize_relative};
use harw_authority::Permission;
use harw_fsutil::{AtomicWriteOptions, write_atomic};
use harw_tools::{
    ToolCall, ToolOutput,
    error::ToolsError,
    executor::{ToolExecutionContext, ToolExecutor, ToolExecutorFuture},
};
use serde::Deserialize;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path};

/// Rechte-Bits neu angelegter Dateien (unabhängig vom `umask`).
const NEW_FILE_MODE: u32 = 0o644;

/// Pfadkomponenten, unter die `fs.write` nie schreibt.
const PROTECTED_COMPONENTS: &[&str] = &[".git", ".harw"];

/// Liefert die erste geschützte Komponente von `relative`, falls vorhanden.
pub(crate) fn protected_component(relative: &Path) -> Option<&'static str> {
    relative.components().find_map(|component| {
        let Component::Normal(part) = component else {
            return None;
        };
        let part = part.to_str()?;
        PROTECTED_COMPONENTS
            .iter()
            .copied()
            .find(|protected| part.eq_ignore_ascii_case(protected))
    })
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
/// Schreibt `content` atomar in eine Datei relativ zum Workspace-Root des
/// Sandbox-Kontexts. Prüft `WriteWorkspace`-Permission, lehnt geschützte
/// Pfade (`.git`, `.harw`) ab und öffnet das Elternverzeichnis symlinkfrei.
///
/// # Concurrency
/// `Send + Sync`; über [`std::sync::Arc`] teilbar. Schreiboperationen sind
/// NICHT commutative — `fs.write` ist NICHT `parallel_safe`.
///
/// # Errors
/// Permission-Fehler → `Ok(ToolOutput::error(...))`.
/// I/O/Pfad-Fehler → `Ok(ToolOutput::error(...))`.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_fs::FsWriteExecutor;
/// let _executor = FsWriteExecutor;
/// ```
pub struct FsWriteExecutor;

impl FsWriteExecutor {
    /// Führt den Schreib-Vorgang synchron (blockierend) aus.
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

        let relative = match normalize_relative(&args.path) {
            Ok(rel) => rel,
            Err(reason) => return Ok(ToolOutput::error(format!("fs.write: {reason}"))),
        };
        // Symlinks im Pfad (auch als letztes Glied) nur folgen, wenn das Ziel
        // im Workspace bleibt (siehe `crate::symlink`); der Schutzbereich gilt
        // danach für das aufgelöste Ziel. Ein fehlendes Glied fällt unten in
        // die bisherige Meldung („Verzeichnisse werden nicht angelegt“).
        let relative =
            match resolve_for_write(ctx.sandbox().workspace().canonical_root(), &relative) {
                Ok(resolved) => resolved,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => relative,
                Err(err) => {
                    return Ok(ToolOutput::error(format!(
                        "fs.write: '{}': {err}",
                        args.path
                    )));
                }
            };
        if let Some(protected) = protected_component(&relative) {
            return Ok(ToolOutput::error(format!(
                "fs.write: '{}' liegt im geschützten Bereich '{protected}/' und wird nicht \
                 geschrieben",
                args.path
            )));
        }
        let (Some(name), Some(parent_rel)) = (relative.file_name(), relative.parent()) else {
            return Ok(ToolOutput::error(format!(
                "fs.write: '{}' enthält keinen Dateinamen",
                args.path
            )));
        };

        // Elternverzeichnis symlinkfrei öffnen (legt nichts an).
        let workspace = match Workspace::open(ctx.sandbox().workspace().canonical_root()) {
            Ok(workspace) => workspace,
            Err(err) => return Ok(ToolOutput::error(FsToolError::Io(err).to_string())),
        };
        let parent = match workspace.open_dir(parent_rel) {
            Ok(parent) => parent,
            Err(err) => {
                return Ok(ToolOutput::error(format!(
                    "fs.write: Elternverzeichnis von '{}' ist nicht nutzbar \
                     (Verzeichnisse werden nicht angelegt): {err}",
                    args.path
                )));
            }
        };
        // Solange `parent` offen ist, bezeichnet dieser Pfad genau das geöffnete
        // Verzeichnis (`/proc/self/fd/<fd>`).
        let target = workspace.dir_path(&parent, parent_rel).join(name);

        // Rechte nur von einer vorhandenen regulären Datei übernehmen (lstat).
        let mode = match std::fs::symlink_metadata(&target) {
            Ok(meta) if meta.file_type().is_file() => meta.permissions().mode() & 0o777,
            _ => NEW_FILE_MODE,
        };

        // Atomar über Tempdatei + rename; ein Ziel-Symlink wird ersetzt.
        let byte_count = args.content.len();
        match write_atomic(
            &target,
            args.content.as_bytes(),
            AtomicWriteOptions::with_mode(mode),
        ) {
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
    /// Nicht parallel-safe (Writes nicht commutative); läuft im Blocking-Pool.
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        let context = context.clone();
        let call = call.clone();
        Box::pin(crate::blocking::run_blocking("fs.write", move || {
            FsWriteExecutor.write_file(&context, &call)
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, SECRET, TestError, TestResult, call, ctx, render};
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
    use std::fs;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn make_sandbox_with_permissions(
        root: &Path,
        permissions: Vec<Permission>,
    ) -> TestResult<SandboxSpec> {
        let ws_dir = root.join("ws");
        fs::create_dir_all(&ws_dir)?;
        let registry = WorkspaceRegistry::build(
            root,
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
        Ok(SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy(permissions),
        ))
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
    fn test_fs_write_writes_file_successfully() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;

        let executor = FsWriteExecutor;
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::WriteWorkspace])?;
        let ctx = make_ctx(sandbox);
        let call = make_call(serde_json::json!({
            "path": "output.txt",
            "content": "hello from test"
        }));

        let result = executor.write_file(&ctx, &call)?;
        match &result {
            ToolOutput::Text { content } => {
                assert!(content.contains("15"), "expected byte count 15: {content}");
                assert!(content.contains("output.txt"), "unexpected: {content}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected text output, got: {other:?}"
                )));
            }
        }

        // Verify the file was actually written
        let written = fs::read_to_string(ws.join("output.txt"))?;
        assert_eq!(written, "hello from test");
        let names = fs::read_dir(&ws)?
            .map(|entry| entry.map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect::<std::io::Result<Vec<String>>>()?;
        assert_eq!(
            names,
            vec!["output.txt".to_owned()],
            "temporary write file should be renamed or cleaned up"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_fs_write_preserves_existing_file_permissions() -> TestResult {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;
        let target = ws.join("private.txt");
        fs::write(&target, "old content")?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600))?;

        let executor = FsWriteExecutor;
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::WriteWorkspace])?;
        let ctx = make_ctx(sandbox);
        let call = make_call(serde_json::json!({
            "path": "private.txt",
            "content": "replacement"
        }));

        let result = executor.write_file(&ctx, &call)?;
        assert!(matches!(result, ToolOutput::Text { .. }));
        assert_eq!(fs::read_to_string(&target)?, "replacement");
        assert_eq!(fs::metadata(&target)?.permissions().mode() & 0o777, 0o600);
        Ok(())
    }

    #[test]
    fn test_fs_write_denied_when_no_write_permission() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;

        let executor = FsWriteExecutor;
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![])?;
        let ctx = make_ctx(sandbox);
        let call = make_call(serde_json::json!({
            "path": "any.txt",
            "content": "data"
        }));

        let result = executor.write_file(&ctx, &call)?;
        match result {
            ToolOutput::Error { message } => {
                assert!(message.contains("WriteWorkspace"), "unexpected: {message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected error output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_fs_write_invalid_args_returns_err() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;

        let executor = FsWriteExecutor;
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::WriteWorkspace])?;
        let ctx = make_ctx(sandbox);
        // Missing "content"
        let call = make_call(serde_json::json!({ "path": "out.txt" }));

        let result = executor.write_file(&ctx, &call);
        assert!(
            matches!(result, Err(ToolsError::InvalidArguments { .. })),
            "expected InvalidArguments, got: {result:?}"
        );
        Ok(())
    }

    #[test]
    fn test_fs_write_rejects_protected_paths() -> TestResult {
        let fixture = Fixture::new()?;
        fs::create_dir_all(fixture.ws.join(".git/hooks"))?;
        fs::create_dir_all(fixture.ws.join(".harw"))?;
        fs::create_dir_all(fixture.ws.join("vendor/lib/.GIT"))?;
        let ctx = fixture.ctx(vec![Permission::WriteWorkspace])?;

        for path in [
            ".git/hooks/pre-commit",
            ".git/config",
            "./.git/hooks/post-checkout",
            ".git",
            ".harw/config.toml",
            "vendor/lib/.GIT/config",
        ] {
            let args = serde_json::json!({ "path": path, "content": "#!/bin/sh" });
            let call = call("fs.write", args);
            match FsWriteExecutor.write_file(&ctx, &call)? {
                ToolOutput::Error { message } => {
                    assert!(message.contains("geschützt"), "{path}: {message}");
                }
                other => {
                    return Err(TestError::Unexpected(format!(
                        "{path}: expected error, got {other:?}"
                    )));
                }
            }
        }
        assert!(!fixture.ws.join(".git/hooks/pre-commit").exists());
        assert!(!fixture.ws.join(".git/config").exists());
        assert!(!fixture.ws.join(".harw/config.toml").exists());
        assert!(!fixture.ws.join("vendor/lib/.GIT/config").exists());
        // Ähnliche, aber ungeschützte Namen bleiben schreibbar.
        let call = call(
            "fs.write",
            serde_json::json!({ "path": ".gitignore", "content": "x" }),
        );
        assert!(matches!(
            FsWriteExecutor.write_file(&ctx, &call)?,
            ToolOutput::Text { .. }
        ));
        Ok(())
    }

    #[test]
    fn test_fs_write_refuses_target_symlink_pointing_outside() -> TestResult {
        let fixture = Fixture::new()?;
        let secret = fixture.outside.join("secret.txt");
        fixture.plant_escapes()?;
        let ctx = fixture.ctx(vec![Permission::WriteWorkspace])?;

        let call = call(
            "fs.write",
            serde_json::json!({ "path": "link_file", "content": "neu" }),
        );
        let output = FsWriteExecutor.write_file(&ctx, &call)?;
        assert!(matches!(output, ToolOutput::Error { .. }), "{output:?}");
        let rendered = render(&output)?;
        assert!(
            rendered.contains("Symlink zeigt außerhalb des Arbeitsbereichs"),
            "{rendered}"
        );
        assert!(
            rendered.contains(&secret.display().to_string()),
            "die Meldung nennt das Ziel: {rendered}"
        );
        assert_eq!(
            fs::read_to_string(&secret)?,
            SECRET,
            "Ziel darf unberührt bleiben"
        );
        assert!(
            fs::symlink_metadata(fixture.ws.join("link_file"))?
                .file_type()
                .is_symlink(),
            "der Symlink bleibt unangetastet"
        );
        Ok(())
    }

    #[test]
    fn test_fs_write_follows_symlink_inside_workspace() -> TestResult {
        let fixture = Fixture::new()?;
        fs::create_dir_all(fixture.ws.join("real"))?;
        fs::write(fixture.ws.join("real/target.txt"), "alt")?;
        std::os::unix::fs::symlink("real/target.txt", fixture.ws.join("file_link"))?;
        std::os::unix::fs::symlink("real", fixture.ws.join("dir_link"))?;
        let ctx = fixture.ctx(vec![Permission::WriteWorkspace])?;

        for (path, file) in [("file_link", "target.txt"), ("dir_link/new.txt", "new.txt")] {
            let call = call(
                "fs.write",
                serde_json::json!({ "path": path, "content": "neu" }),
            );
            let output = FsWriteExecutor.write_file(&ctx, &call)?;
            assert!(
                matches!(output, ToolOutput::Text { .. }),
                "{path}: {output:?}"
            );
            assert_eq!(
                fs::read_to_string(fixture.ws.join("real").join(file))?,
                "neu"
            );
        }
        assert!(
            fs::symlink_metadata(fixture.ws.join("file_link"))?
                .file_type()
                .is_symlink(),
            "der Symlink selbst bleibt ein Symlink"
        );
        Ok(())
    }

    #[test]
    fn test_fs_write_through_symlink_into_protected_area_is_refused() -> TestResult {
        let fixture = Fixture::new()?;
        fs::create_dir_all(fixture.ws.join(".git"))?;
        std::os::unix::fs::symlink(".git", fixture.ws.join("gitlink"))?;
        let ctx = fixture.ctx(vec![Permission::WriteWorkspace])?;
        let call = call(
            "fs.write",
            serde_json::json!({ "path": "gitlink/config", "content": "x" }),
        );
        let output = FsWriteExecutor.write_file(&ctx, &call)?;
        assert!(matches!(output, ToolOutput::Error { .. }), "{output:?}");
        assert!(!fixture.ws.join(".git/config").exists());
        Ok(())
    }

    #[test]
    fn test_fs_write_rejects_symlinked_parent_and_traversal() -> TestResult {
        let fixture = Fixture::new()?;
        fixture.plant_escapes()?;
        let ctx = fixture.ctx(vec![Permission::WriteWorkspace])?;

        // `loop` (-> `.`) und `nested/up` (-> `..`) bleiben im Workspace und
        // werden gefolgt; alles andere führt hinaus oder fehlt.
        let escapes = [
            "link_dir/planted.txt",
            "../planted.txt",
            "/tmp/planted.txt",
            "missing/planted.txt",
        ];
        for path in escapes {
            let call = call(
                "fs.write",
                serde_json::json!({ "path": path, "content": "x" }),
            );
            let output = FsWriteExecutor.write_file(&ctx, &call)?;
            assert!(
                matches!(output, ToolOutput::Error { .. }),
                "{path}: {output:?}"
            );
            assert!(!render(&output)?.contains(SECRET));
        }
        assert!(!fixture.outside.join("planted.txt").exists());
        assert!(!fixture.ws.join("planted.txt").exists());
        for path in ["loop/planted.txt", "nested/up/planted.txt"] {
            let call = call(
                "fs.write",
                serde_json::json!({ "path": path, "content": "x" }),
            );
            let output = FsWriteExecutor.write_file(&ctx, &call)?;
            assert!(
                matches!(output, ToolOutput::Text { .. }),
                "{path}: {output:?}"
            );
        }
        assert!(fixture.ws.join("planted.txt").exists(), "innen gefolgt");
        assert!(!fixture.outside.join("planted.txt").exists());
        assert!(
            !fixture.ws.join("missing").exists(),
            "Elternverzeichnisse werden nicht angelegt"
        );
        Ok(())
    }

    #[test]
    fn test_fs_write_race_parent_replaced_by_symlink() -> TestResult {
        let fixture = Fixture::new()?;
        fs::create_dir_all(fixture.ws.join("out"))?;
        let ctx = fixture.ctx(vec![Permission::WriteWorkspace])?;
        let call = call(
            "fs.write",
            serde_json::json!({ "path": "out/file.txt", "content": "x" }),
        );
        let output = FsWriteExecutor.write_file(&ctx, &call)?;
        assert!(matches!(output, ToolOutput::Text { .. }), "{output:?}");

        // Wettlauf-Surrogat: Elternverzeichnis gegen Symlink nach außen getauscht.
        fs::rename(fixture.ws.join("out"), fixture.ws.join("out_old"))?;
        std::os::unix::fs::symlink(&fixture.outside, fixture.ws.join("out"))?;
        let output = FsWriteExecutor.write_file(&ctx, &call)?;
        assert!(matches!(output, ToolOutput::Error { .. }), "{output:?}");
        assert!(!fixture.outside.join("file.txt").exists());
        Ok(())
    }
}
