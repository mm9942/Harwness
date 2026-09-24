//! `fs.list` — Tool-Executor für Verzeichnis-Listing.
//!
//! # Verantwortung
//! Dieses Modul besitzt den `fs.list`-Executor:
//! - [`FsListExecutor`]: implementiert [`ToolExecutor`]; listet den Inhalt eines
//!   Verzeichnisses als JSON-Objekt `{entries, stopped?}` auf. Prüft
//!   `ReadWorkspace`-Permission, öffnet das Verzeichnis symlinkfrei unterhalb
//!   der Workspace-Wurzel und liest es über `harw_fsutil::walk_beneath`
//!   (sortiert, Symlinks werden nie gefolgt und als `"other"` gemeldet).
//!
//! # Schlüsseltypen
//! - [`FsListExecutor`]
//!
//! # Nebenläufigkeit
//! [`FsListExecutor`] ist `Send + Sync`. `fs.list` ist `parallel_safe`. Die
//! Verzeichnis-IO läuft im Blocking-Pool.
//!
//! # Fehler
//! Permission-Fehler → `Ok(ToolOutput::error(...))`. Kein Panic.

use crate::tree::{
    HARD_MAX_ENTRIES, StopReason, WALK_DEADLINE, Workspace, normalize_relative, read_dir_limited,
};
use harw_authority::Permission;
use harw_fsutil::EntryType;
use harw_tools::{
    ToolCall, ToolOutput,
    error::ToolsError,
    executor::{ToolExecutionContext, ToolExecutor, ToolExecutorFuture},
};
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// Standard-Maximum für Verzeichnis-Einträge.
pub const DEFAULT_MAX_ENTRIES: usize = 200;

/// Deserialisierte Argumente für `fs.list`.
#[derive(Debug, Deserialize)]
struct FsListArgs {
    /// Pfad relativ zum Workspace-Root (Verzeichnis).
    path: String,
    /// Maximale Anzahl Einträge (Default: [`DEFAULT_MAX_ENTRIES`]).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    max_entries: Option<usize>,
}

/// Ein Eintrag im Verzeichnis-Listing.
#[derive(Debug, Serialize)]
struct DirEntry {
    /// Dateiname (ohne Pfad-Prefix).
    name: String,
    /// Typ: `"file"` oder `"dir"` (Symlinks und sonstige Typen: `"other"`).
    kind: String,
    /// Dateigröße in Bytes (0 für Verzeichnisse und `"other"`).
    size: u64,
}

/// JSON-Ergebnis von `fs.list`.
#[derive(Debug, Serialize)]
struct ListResult {
    /// Einträge, bytewise nach Namen sortiert.
    entries: Vec<DirEntry>,
    /// Abbruchgrund, falls das Listing unvollständig ist.
    #[serde(skip_serializing_if = "Option::is_none")]
    stopped: Option<&'static str>,
}

/// Führt `fs.list`-Aufrufe aus.
///
/// # Description
/// Listet den Inhalt eines Verzeichnisses relativ zum Workspace-Root des
/// Sandbox-Kontexts. Prüft `ReadWorkspace`-Permission; das Verzeichnis wird
/// symlinkfrei relativ zum Wurzel-Deskriptor geöffnet.
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
    /// Führt das Listing synchron (blockierend) aus.
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

        let relative = match normalize_relative(&args.path) {
            Ok(rel) => rel,
            Err(reason) => return Ok(ToolOutput::error(format!("fs.list: {reason}"))),
        };
        // `null` und `0` gelten als „nicht gesetzt“ (Strict-Schema: Modelle
        // senden für ungenutzte Felder oft `0`, das sonst ein leeres Listing
        // ergäbe).
        let cap = args
            .max_entries
            .filter(|&entries| entries > 0)
            .unwrap_or(self.max_entries)
            .min(self.max_entries)
            .min(HARD_MAX_ENTRIES);

        let listed =
            Workspace::open(ctx.sandbox().workspace().canonical_root()).and_then(|workspace| {
                let dir = workspace.open_dir(&relative)?;
                let deadline = Instant::now() + WALK_DEADLINE;
                read_dir_limited(&workspace, &dir, &relative, cap, deadline)
            });
        let (children, stop) = match listed {
            Ok(listed) => listed,
            Err(err) => {
                return Ok(ToolOutput::error(format!(
                    "fs.list: '{}' ist kein lesbares Verzeichnis \
                     (Symlinks werden nicht verfolgt): {err}; erwartet ein Verzeichnis; für \
                     einzelne Dateien fs.read oder fs.grep verwenden",
                    args.path
                )));
            }
        };

        let entries = children
            .into_iter()
            .map(|child| {
                let (kind, size) = match child.entry_type {
                    EntryType::File => ("file", child.len),
                    EntryType::Dir => ("dir", 0),
                    EntryType::Symlink | EntryType::Other => ("other", 0),
                };
                DirEntry {
                    name: child.rel_path.to_string_lossy().into_owned(),
                    kind: kind.to_owned(),
                    size,
                }
            })
            .collect();
        let result = ListResult {
            entries,
            stopped: stop.map(StopReason::as_str),
        };
        let json = serde_json::to_value(result).unwrap_or(serde_json::Value::Array(vec![]));
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
    /// `Ok(ToolOutput::Json { content: {entries, stopped?} })` bei Erfolg.
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
        let executor = Self {
            max_entries: self.max_entries,
        };
        let context = context.clone();
        let call = call.clone();
        Box::pin(crate::blocking::run_blocking("fs.list", move || {
            executor.list_dir(&context, &call)
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
            name: harw_tools::spec::ToolName::new("fs.list"),
            arguments: args,
        }
    }

    #[test]
    fn test_fs_list_lists_directory_successfully() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;
        fs::write(ws.join("a.txt"), "aaa")?;
        fs::write(ws.join("b.txt"), "bb")?;
        fs::create_dir_all(ws.join("subdir"))?;

        let executor = FsListExecutor {
            max_entries: DEFAULT_MAX_ENTRIES,
        };
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace])?;
        let ctx = make_ctx(sandbox);
        let call = make_call(serde_json::json!({ "path": "." }));

        let result = executor.list_dir(&ctx, &call)?;
        match result {
            ToolOutput::Json { content } => {
                let arr = content["entries"]
                    .as_array()
                    .ok_or(TestError::Missing("entries"))?;
                let names = arr
                    .iter()
                    .map(|e| e["name"].as_str().ok_or(TestError::Missing("name")))
                    .collect::<TestResult<Vec<&str>>>()?;
                assert_eq!(names, vec!["a.txt", "b.txt", "subdir"]);
                assert_eq!(arr[0]["kind"], "file");
                assert_eq!(arr[0]["size"], 3);
                assert_eq!(arr[2]["kind"], "dir");
                assert!(content.get("stopped").is_none());
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected json output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_fs_list_denied_when_no_read_permission() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;

        let executor = FsListExecutor {
            max_entries: DEFAULT_MAX_ENTRIES,
        };
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![])?;
        let ctx = make_ctx(sandbox);
        let call = make_call(serde_json::json!({ "path": "." }));

        let result = executor.list_dir(&ctx, &call)?;
        match result {
            ToolOutput::Error { message } => {
                assert!(message.contains("ReadWorkspace"), "unexpected: {message}");
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
    fn test_fs_list_invalid_args_returns_err() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;

        let executor = FsListExecutor {
            max_entries: DEFAULT_MAX_ENTRIES,
        };
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace])?;
        let ctx = make_ctx(sandbox);
        // Missing "path"
        let call = make_call(serde_json::json!({ "max_entries": 5 }));

        let result = executor.list_dir(&ctx, &call);
        assert!(
            matches!(result, Err(ToolsError::InvalidArguments { .. })),
            "expected InvalidArguments, got: {result:?}"
        );
        Ok(())
    }

    #[test]
    fn test_fs_list_does_not_follow_symlinks() -> TestResult {
        let fixture = Fixture::new()?;
        fixture.plant_escapes()?;
        let executor = FsListExecutor {
            max_entries: DEFAULT_MAX_ENTRIES,
        };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;

        let output =
            executor.list_dir(&ctx, &call("fs.list", serde_json::json!({ "path": "." })))?;
        let ToolOutput::Json { content } = &output else {
            return Err(TestError::Unexpected(format!(
                "expected json output, got: {output:?}"
            )));
        };
        let entries = content["entries"]
            .as_array()
            .ok_or(TestError::Missing("entries"))?;
        for entry in entries {
            let name = entry["name"].as_str().ok_or(TestError::Missing("name"))?;
            if ["link_dir", "link_file", "loop"].contains(&name) {
                assert_eq!(entry["kind"], "other", "{name}");
                assert_eq!(entry["size"], 0, "{name}");
            }
        }
        assert!(!render(&output)?.contains("secret.txt"));

        for path in [
            "link_dir",
            "loop",
            "nested/up",
            "link_dir/deep",
            "../outside",
        ] {
            let output =
                executor.list_dir(&ctx, &call("fs.list", serde_json::json!({ "path": path })))?;
            assert!(
                matches!(output, ToolOutput::Error { .. }),
                "{path}: {output:?}"
            );
            let rendered = render(&output)?;
            assert!(
                !rendered.contains("secret.txt") && !rendered.contains(SECRET),
                "{rendered}"
            );
        }
        Ok(())
    }

    #[test]
    fn test_fs_list_reports_entry_limit() -> TestResult {
        let fixture = Fixture::new()?;
        for i in 0..5 {
            fs::write(fixture.ws.join(format!("f{i}")), "x")?;
        }
        let executor = FsListExecutor { max_entries: 3 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let output =
            executor.list_dir(&ctx, &call("fs.list", serde_json::json!({ "path": "." })))?;
        let ToolOutput::Json { content } = output else {
            return Err(TestError::Unexpected("expected json output".to_string()));
        };
        assert_eq!(
            content["entries"]
                .as_array()
                .ok_or(TestError::Missing("entries"))?
                .len(),
            3
        );
        assert_eq!(content["stopped"], "entry_limit");
        Ok(())
    }

    #[test]
    fn test_fs_list_zero_or_null_max_entries_means_default() -> TestResult {
        let fixture = Fixture::new()?;
        for i in 0..2 {
            fs::write(fixture.ws.join(format!("f{i}")), "x")?;
        }
        let executor = FsListExecutor { max_entries: 10 };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        for max_entries in [serde_json::json!(0), serde_json::Value::Null] {
            let output = executor.list_dir(
                &ctx,
                &call(
                    "fs.list",
                    serde_json::json!({ "path": ".", "max_entries": max_entries }),
                ),
            )?;
            let ToolOutput::Json { content } = output else {
                return Err(TestError::Unexpected("expected json output".to_string()));
            };
            assert_eq!(
                content["entries"]
                    .as_array()
                    .ok_or(TestError::Missing("entries"))?
                    .len(),
                2,
                "max_entries={max_entries}"
            );
        }
        Ok(())
    }

    #[test]
    fn test_fs_list_race_directory_replaced_by_symlink() -> TestResult {
        let fixture = Fixture::new()?;
        fs::create_dir_all(fixture.ws.join("sub"))?;
        let executor = FsListExecutor {
            max_entries: DEFAULT_MAX_ENTRIES,
        };
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let call = call("fs.list", serde_json::json!({ "path": "sub" }));
        let first = executor.list_dir(&ctx, &call)?;
        assert!(matches!(first, ToolOutput::Json { .. }));

        fs::rename(fixture.ws.join("sub"), fixture.ws.join("sub_old"))?;
        std::os::unix::fs::symlink(&fixture.outside, fixture.ws.join("sub"))?;
        let output = executor.list_dir(&ctx, &call)?;
        assert!(matches!(output, ToolOutput::Error { .. }), "{output:?}");
        assert!(!render(&output)?.contains("secret.txt"));
        Ok(())
    }
}
