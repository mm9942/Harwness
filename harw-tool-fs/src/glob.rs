//! `fs.glob` — Tool-Executor für Glob-basierte Dateisuche im Workspace.
//!
//! Spec-Referenz: AP W2-01..03, Abschnitt "1. `glob.rs` — `fs.glob`";
//! Sicherheits- und Grenzenüberarbeitung: W1-02 (F-058, F-118).
//!
//! # Verantwortung
//! Dieses Modul besitzt den `fs.glob`-Executor:
//! - [`GlobArgs`]: deserialisierte Aufrufargumente, per `#[derive(harw_macros::Tool)]`
//!   auch Quelle der JSON-Schema-Spezifikation.
//! - `fs_glob` (per `#[harw_macros::tool]` zu [`FsGlobTool`] erweitert): findet
//!   Dateien unterhalb eines Start-Verzeichnisses, die auf ein Glob-Muster
//!   passen.
//!
//! # Traversierung (W1-02)
//! Gewalkt wird über [`crate::tree::walk_tree`] auf Basis von
//! `harw_fsutil::walk_beneath`: Symlinks werden nie gefolgt (Symlink-Einträge
//! gelten nicht als Datei), Unterverzeichnisse werden relativ zum
//! Eltern-Deskriptor geöffnet. `.gitignore`/`.ignore` werden weiterhin
//! beachtet, aber nur innerhalb des Workspace (siehe `tree`-Modul);
//! `target/` und `.git/` bleiben harte Ausschlüsse. Das Muster wird — wie
//! dokumentiert — gegen den Pfad **relativ zur Workspace-Wurzel** geprüft
//! (`globset` mit `literal_separator(true)`). Grenzen: höchstens 1000
//! Treffer, Tiefe 32, 50 000 Einträge, 10 s, Ausgabe höchstens 64 KiB; der
//! Abbruchgrund steht im Feld `stopped`.
//!
//! # Schlüsseltypen
//! - [`GlobArgs`]
//! - [`FsGlobTool`] (generiert durch `#[harw_macros::tool]`)
//!
//! # Nebenläufigkeit
//! [`FsGlobTool`] ist `Send + Sync` (Unit-Struct). `fs.glob` ist `parallel_safe`.
//! Der Walk läuft über `spawn_blocking`.
//!
//! # Fehler
//! Permission-Fehler (erzwungen durch den Makro-Prolog vor der
//! Deserialisierung), ungültige Glob-Muster und Pfadauflösungsfehler münden
//! alle in `Ok(ToolOutput::error(...))`. Kein Panic.

use crate::blocking::run_blocking;
use crate::symlink::open_start;
use crate::tree::{
    HARD_MAX_RESULTS, MAX_OUTPUT_BYTES, StopReason, WalkOptions, normalize_relative, walk_tree,
};
use globset::GlobBuilder;
use harw_fsutil::EntryType;
use harw_macros::Tool;
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::ops::ControlFlow;
use std::path::Path;

/// Standard-Obergrenze für `fs.glob`-Treffer.
pub const DEFAULT_MAX_RESULTS: usize = 200;

/// Geschätzter JSON-Overhead je Treffer (Quoting, Komma).
const MATCH_OVERHEAD_BYTES: usize = 4;

/// Verzeichnis- bzw. Dateinamen, die unabhängig von `.gitignore` immer
/// übersprungen werden.
const HARD_EXCLUDED_NAMES: &[&str] = &["target", ".git"];

/// Deserialisierte Argumente für `fs.glob`.
#[derive(Debug, Tool, Deserialize)]
#[tool(
    name = "fs.glob",
    description = "Findet Dateien im Workspace über ein Glob-Muster."
)]
pub struct GlobArgs {
    /// Glob-Muster relativ zur Workspace-Wurzel, z. B. `src/**/*.rs`.
    pub pattern: String,
    /// Optionales Unterverzeichnis, ab dem gesucht wird.
    pub path: Option<String>,
    /// Obergrenze der Treffer (Default 200, Maximum 1000).
    #[tool(default = 200)]
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub max_results: Option<usize>,
}

/// JSON-Ausgabestruktur für `fs.glob`.
#[derive(Debug, Serialize)]
struct GlobResult {
    /// Gefundene Pfade, relativ zur Workspace-Wurzel, alphabetisch sortiert.
    matches: Vec<String>,
    /// Hinweistext, wenn das Ergebnis gekappt wurde.
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    /// Abbruchgrund, falls das Ergebnis unvollständig ist.
    #[serde(skip_serializing_if = "Option::is_none")]
    stopped: Option<&'static str>,
}

/// Prüft, ob ein Datei- oder Verzeichnisname unabhängig von `.gitignore`
/// hart ausgeschlossen werden soll (`target`, `.git`).
fn is_hard_excluded(name: &OsStr, _is_dir: bool) -> bool {
    name.to_str()
        .is_some_and(|name| HARD_EXCLUDED_NAMES.contains(&name))
}

/// Führt die `fs.glob`-Suche aus: findet Dateien unterhalb eines
/// (optionalen) Start-Verzeichnisses, die auf `args.pattern` passen.
///
/// Berechtigungsprüfung (`ReadWorkspace`) und JSON-Deserialisierung laufen im
/// von `#[harw_macros::tool]` generierten Prolog von [`FsGlobTool`], bevor
/// diese Funktion aufgerufen wird. Die eigentliche Arbeit läuft blockierend
/// in `spawn_blocking`.
///
/// # Errors
/// Liefert nie `Err`; Pfad-, Muster- oder I/O-Fehler werden als
/// `Ok(ToolOutput::error(...))` zurückgegeben.
#[harw_macros::tool(
    name = "fs.glob",
    description = "Findet Dateien im Workspace über ein Glob-Muster.",
    permission = "read_workspace",
    parallel_safe
)]
async fn fs_glob(context: &ToolExecutionContext, args: GlobArgs) -> Result<ToolOutput, ToolsError> {
    let root = context.sandbox().workspace().canonical_root().to_path_buf();
    run_blocking("fs.glob", move || Ok(glob_blocking(&root, &args))).await
}

/// Synchroner Kern von [`fs_glob`].
fn glob_blocking(root: &Path, args: &GlobArgs) -> ToolOutput {
    // Strict-Schema: `null` und `""` gelten als „nicht gesetzt“ (Wurzel).
    let start_input = args
        .path
        .as_deref()
        .filter(|path| !path.is_empty())
        .unwrap_or(".");
    let start_rel = match normalize_relative(start_input) {
        Ok(rel) => rel,
        Err(reason) => return ToolOutput::error(format!("fs.glob: {reason}")),
    };

    let matcher = match GlobBuilder::new(&args.pattern)
        .literal_separator(true)
        .build()
    {
        Ok(glob) => glob.compile_matcher(),
        Err(err) => {
            return ToolOutput::error(format!(
                "fs.glob: ungültiges Muster '{}': {err}",
                args.pattern
            ));
        }
    };

    // Symlinks im Startpfad: nach innen frei, nach außen nur mit Freigabe
    // (siehe `crate::symlink`); der Walk startet am aufgelösten Pfad.
    let (workspace, start_rel) = match open_start(root, &start_rel, "fs.glob", start_input) {
        Ok(start) => (start.workspace, start.rel),
        Err(err) => {
            return ToolOutput::error(format!(
                "fs.glob: '{start_input}' ist kein lesbares Verzeichnis: {err}"
            ));
        }
    };
    // `clamp(1, HARD_MAX_RESULTS)`: mindestens ein Treffer, höchstens die
    // harte Obergrenze — auch wenn der Aufrufer etwas Riesiges angibt.
    // `0` gilt wie `null` als „nicht gesetzt“ (Vorgabe), nicht als „ein Treffer“.
    let cap = args
        .max_results
        .filter(|&results| results > 0)
        .unwrap_or(DEFAULT_MAX_RESULTS)
        .clamp(1, HARD_MAX_RESULTS);

    let mut matches: Vec<String> = Vec::new();
    let mut output_bytes = 0usize;
    let walked = walk_tree(
        &workspace,
        &start_rel,
        WalkOptions::standard(true, is_hard_excluded),
        |entry| {
            if entry.entry_type != EntryType::File || !matcher.is_match(entry.rel) {
                return ControlFlow::Continue(());
            }
            if matches.len() >= cap {
                return ControlFlow::Break(StopReason::ResultLimit);
            }
            let found = entry.rel.display().to_string();
            let cost = found.len() + MATCH_OVERHEAD_BYTES;
            if output_bytes + cost > MAX_OUTPUT_BYTES {
                return ControlFlow::Break(StopReason::OutputLimit);
            }
            output_bytes += cost;
            matches.push(found);
            ControlFlow::Continue(())
        },
    );
    let stop = match walked {
        Ok(stop) => stop,
        Err(err) => {
            return ToolOutput::error(format!(
                "fs.glob: '{start_input}' ist kein lesbares Verzeichnis: {err}; erwartet ein \
                 Verzeichnis; für einzelne Dateien fs.read oder fs.grep verwenden"
            ));
        }
    };

    matches.sort();
    let note = match stop {
        Some(StopReason::ResultLimit) => Some(format!(
            "Ergebnis auf {cap} Treffer begrenzt; es gibt weitere Treffer, die nicht angezeigt \
             werden."
        )),
        Some(reason) => Some(format!(
            "Ergebnis unvollständig (Grenze '{}' erreicht).",
            reason.as_str()
        )),
        None => None,
    };

    let payload = serde_json::to_value(GlobResult {
        matches,
        note,
        stopped: stop.map(StopReason::as_str),
    })
    .unwrap_or(serde_json::Value::Array(vec![]));
    ToolOutput::json(payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        Fixture, SECRET, TestError, TestResult, call as tool_call, ctx, render,
    };
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_tools::ToolExecutor;
    use harw_types::{SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};
    use std::fs;
    use std::path::{Path as StdPath, PathBuf};
    use tempfile::TempDir;

    fn make_sandbox_with_permissions(
        root: &StdPath,
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

    fn glob_args(pattern: &str, path: Option<&str>, max_results: Option<usize>) -> GlobArgs {
        GlobArgs {
            pattern: pattern.to_owned(),
            path: path.map(str::to_owned),
            max_results,
        }
    }

    fn extract_matches(output: ToolOutput) -> TestResult<Vec<String>> {
        match output {
            ToolOutput::Json { content } => content["matches"]
                .as_array()
                .ok_or(TestError::Missing("matches"))?
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .ok_or(TestError::Missing("match"))
                })
                .collect(),
            other => Err(TestError::Unexpected(format!(
                "expected json output, got: {other:?}"
            ))),
        }
    }

    #[tokio::test]
    async fn test_fs_glob_finds_rust_files_recursively() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(ws.join("src/nested"))?;
        fs::write(ws.join("src/main.rs"), "fn main() {}")?;
        fs::write(ws.join("src/nested/lib.rs"), "pub fn f() {}")?;
        fs::write(ws.join("readme.md"), "not rust")?;

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace])?;
        let ctx = make_ctx(sandbox);
        let args = glob_args("**/*.rs", None, None);

        let output = fs_glob(&ctx, args).await?;
        let matches = extract_matches(output)?;

        assert_eq!(
            matches,
            vec!["src/main.rs".to_owned(), "src/nested/lib.rs".to_owned()]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_fs_glob_ignores_target_dir() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(ws.join("target"))?;
        fs::create_dir_all(ws.join("src"))?;
        fs::write(ws.join("target/generated.rs"), "// generated")?;
        fs::write(ws.join("src/main.rs"), "fn main() {}")?;

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace])?;
        let ctx = make_ctx(sandbox);
        let args = glob_args("**/*.rs", None, None);

        let output = fs_glob(&ctx, args).await?;
        let matches = extract_matches(output)?;

        assert_eq!(matches, vec!["src/main.rs".to_owned()]);
        Ok(())
    }

    #[tokio::test]
    async fn test_fs_glob_respects_gitignore() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;
        fs::write(ws.join(".gitignore"), "ignored.rs\n")?;
        fs::write(ws.join("ignored.rs"), "// ignored")?;
        fs::write(ws.join("kept.rs"), "// kept")?;

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace])?;
        let ctx = make_ctx(sandbox);
        let args = glob_args("*.rs", None, None);

        let output = fs_glob(&ctx, args).await?;
        let matches = extract_matches(output)?;

        assert_eq!(matches, vec!["kept.rs".to_owned()]);
        Ok(())
    }

    #[tokio::test]
    async fn test_fs_glob_caps_at_max_results() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;
        for i in 0..5 {
            fs::write(ws.join(format!("file{i}.rs")), "// f")?;
        }

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace])?;
        let ctx = make_ctx(sandbox);
        let args = glob_args("*.rs", None, Some(2));

        let output = fs_glob(&ctx, args).await?;
        match output {
            ToolOutput::Json { content } => {
                let matches = content["matches"]
                    .as_array()
                    .ok_or(TestError::Missing("matches"))?;
                assert_eq!(matches.len(), 2, "expected result capped at 2");
                assert!(
                    content["note"].as_str().is_some(),
                    "expected a truncation note"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected json output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_fs_glob_empty_path_and_zero_limit_mean_not_set() -> TestResult {
        // Strict-Schema: Modelle senden für ungenutzte Felder `""` bzw. `0`.
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;
        for i in 0..5 {
            fs::write(ws.join(format!("file{i}.rs")), "// f")?;
        }

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace])?;
        let ctx = make_ctx(sandbox);
        let output = fs_glob(&ctx, glob_args("*.rs", Some(""), Some(0))).await?;
        match output {
            ToolOutput::Json { content } => {
                let matches = content["matches"]
                    .as_array()
                    .ok_or(TestError::Missing("matches"))?;
                assert_eq!(matches.len(), 5, "0 darf nicht auf einen Treffer kappen");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected json output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_fs_glob_invalid_pattern_returns_error() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace])?;
        let ctx = make_ctx(sandbox);
        let args = glob_args("[", None, None);

        let output = fs_glob(&ctx, args).await?;
        match output {
            ToolOutput::Error { message } => {
                assert!(message.contains("ungültiges Muster"), "got: {message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected error output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_fs_glob_denied_when_no_read_permission() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![])?;
        let ctx = make_ctx(sandbox);
        let call = harw_tools::ToolCall {
            id: ToolCallId::new(),
            name: harw_tools::spec::ToolName::new("fs.glob"),
            arguments: serde_json::json!({ "pattern": "*.rs" }),
        };

        let tool = FsGlobTool;
        let result = tool.execute(&ctx, &call).await?;
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

    #[tokio::test]
    async fn test_fs_glob_does_not_follow_symlinks() -> TestResult {
        let fixture = Fixture::new()?;
        fixture.plant_escapes()?;
        fs::write(fixture.ws.join("nested/own.rs"), "// own")?;
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;

        let output = fs_glob(&ctx, glob_args("**/*", None, None)).await?;
        let matches = extract_matches(output)?;
        assert_eq!(matches, vec!["nested/own.rs".to_owned()]);

        for path in ["link_dir", "../outside"] {
            let args = glob_args("**/*", Some(path), None);
            let output = fs_glob(&ctx, args).await?;
            assert!(
                matches!(output, ToolOutput::Error { .. }),
                "{path}: {output:?}"
            );
            assert!(!render(&output)?.contains(SECRET));
        }
        // Startpfade, deren Symlink-Ziel im Workspace bleibt (`loop` -> `.`,
        // `nested/up` -> `..` = Wurzel), werden aufgelöst; der Walk selbst
        // folgt weiterhin keinem Symlink.
        for path in ["loop", "nested/up"] {
            let args = glob_args("**/*", Some(path), None);
            let matches = extract_matches(fs_glob(&ctx, args).await?)?;
            assert_eq!(matches, vec!["nested/own.rs".to_owned()], "{path}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_fs_glob_pattern_is_relative_to_workspace_root() -> TestResult {
        let fixture = Fixture::new()?;
        fs::create_dir_all(fixture.ws.join("src/nested"))?;
        fs::write(fixture.ws.join("src/nested/lib.rs"), "")?;
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;

        let args = glob_args("src/**/*.rs", Some("src"), None);
        let output = fs_glob(&ctx, args).await?;
        assert_eq!(
            extract_matches(output)?,
            vec!["src/nested/lib.rs".to_owned()]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_fs_glob_caps_results_at_hard_limit() -> TestResult {
        let fixture = Fixture::new()?;
        for i in 0..(HARD_MAX_RESULTS + 5) {
            fs::write(fixture.ws.join(format!("f{i:05}.rs")), "")?;
        }
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let args = glob_args("*.rs", None, Some(usize::MAX));
        let output = fs_glob(&ctx, args).await?;
        match output {
            ToolOutput::Json { content } => {
                assert_eq!(
                    content["matches"]
                        .as_array()
                        .ok_or(TestError::Missing("matches"))?
                        .len(),
                    HARD_MAX_RESULTS
                );
                assert_eq!(content["stopped"], "result_limit");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected json output, got: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_fs_glob_executor_runs_blocking_work() -> TestResult {
        let fixture = Fixture::new()?;
        fs::write(fixture.ws.join("a.rs"), "")?;
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let call = tool_call("fs.glob", serde_json::json!({ "pattern": "*.rs" }));
        let output = FsGlobTool.execute(&ctx, &call).await?;
        assert_eq!(extract_matches(output)?, vec!["a.rs".to_owned()]);
        Ok(())
    }
}
