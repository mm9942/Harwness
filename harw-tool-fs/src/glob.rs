//! `fs.glob` — Tool-Executor für Glob-basierte Dateisuche im Workspace.
//!
//! Spec-Referenz: AP W2-01..03, Abschnitt "1. `glob.rs` — `fs.glob`".
//!
//! # Verantwortung
//! Dieses Modul besitzt den `fs.glob`-Executor:
//! - [`GlobArgs`]: deserialisierte Aufrufargumente, per `#[derive(harw_macros::Tool)]`
//!   auch Quelle der JSON-Schema-Spezifikation.
//! - `fs_glob` (per `#[harw_macros::tool]` zu [`FsGlobTool`] erweitert): findet
//!   Dateien unterhalb eines Start-Verzeichnisses, die auf ein Glob-Muster
//!   passen. Traversierung über die `ignore`-Crate (respektiert `.gitignore`;
//!   `target/` und `.git/` werden zusätzlich hart ausgeschlossen). Matching
//!   über `globset` mit `literal_separator(true)`, damit `*` keine `/`
//!   überspringt. Pfadauflösung ausschließlich über
//!   `ctx.sandbox().workspace().resolve_existing()`.
//!
//! # Schlüsseltypen
//! - [`GlobArgs`]
//! - [`FsGlobTool`] (generiert durch `#[harw_macros::tool]`)
//!
//! # Nebenläufigkeit
//! [`FsGlobTool`] ist `Send + Sync` (Unit-Struct). `fs.glob` ist `parallel_safe`.
//!
//! # Fehler
//! Permission-Fehler (erzwungen durch den Makro-Prolog vor der
//! Deserialisierung), ungültige Glob-Muster und Pfadauflösungsfehler münden
//! alle in `Ok(ToolOutput::error(...))`. Kein Panic.

use globset::GlobBuilder;
use harw_macros::Tool;
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use ignore::WalkBuilder;
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::Path;

/// Standard-Obergrenze für `fs.glob`-Treffer.
pub const DEFAULT_MAX_RESULTS: usize = 200;

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
    /// Obergrenze der Treffer (Default 200).
    #[tool(default = 200)]
    #[serde(default)]
    pub max_results: Option<usize>,
}

/// JSON-Ausgabestruktur für `fs.glob`.
#[derive(Debug, Serialize)]
struct GlobResult {
    /// Gefundene Pfade, relativ zur Workspace-Wurzel, alphabetisch sortiert.
    matches: Vec<String>,
    /// Hinweistext, wenn das Ergebnis auf `max_results` gekappt wurde.
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

/// Prüft, ob ein Datei- oder Verzeichnisname unabhängig von `.gitignore`
/// hart ausgeschlossen werden soll (`target`, `.git`).
fn is_hard_excluded(name: &OsStr) -> bool {
    name.to_str()
        .is_some_and(|s| HARD_EXCLUDED_NAMES.contains(&s))
}

/// Führt die `fs.glob`-Suche aus: findet Dateien unterhalb eines
/// (optionalen) Start-Verzeichnisses, die auf `args.pattern` passen.
///
/// Berechtigungsprüfung (`ReadWorkspace`) und JSON-Deserialisierung laufen im
/// von `#[harw_macros::tool]` generierten Prolog von [`FsGlobTool`], bevor
/// diese Funktion aufgerufen wird.
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
    let workspace = context.sandbox().workspace();

    let start_relative = args.path.as_deref().unwrap_or(".");
    let start_resolved = match workspace.resolve_existing(Path::new(start_relative)) {
        Ok(path) => path,
        Err(err) => return Ok(ToolOutput::error(err.to_string())),
    };
    if !start_resolved.is_dir() {
        return Ok(ToolOutput::error(format!(
            "fs.glob: '{start_relative}' ist kein Verzeichnis"
        )));
    }

    let matcher = match GlobBuilder::new(&args.pattern).literal_separator(true).build() {
        Ok(glob) => glob.compile_matcher(),
        Err(err) => {
            return Ok(ToolOutput::error(format!(
                "fs.glob: ungültiges Muster '{}': {err}",
                args.pattern
            )));
        }
    };

    let workspace_root = workspace.canonical_root().to_path_buf();
    let cap = args.max_results.unwrap_or(DEFAULT_MAX_RESULTS).max(1);

    let mut walk_builder = WalkBuilder::new(&start_resolved);
    walk_builder
        .hidden(false)
        .parents(true)
        .git_ignore(true)
        .require_git(false)
        .filter_entry(|entry| entry.depth() == 0 || !is_hard_excluded(entry.file_name()));

    let mut matches: Vec<String> = Vec::new();

    for entry in walk_builder.build() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        let match_relative = entry
            .path()
            .strip_prefix(&start_resolved)
            .unwrap_or_else(|_| entry.path());
        if !matcher.is_match(match_relative) {
            continue;
        }

        let output_relative = entry
            .path()
            .strip_prefix(&workspace_root)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| entry.path().display().to_string());
        matches.push(output_relative);
    }

    matches.sort();

    let truncated = matches.len() > cap;
    if truncated {
        matches.truncate(cap);
    }

    let note = truncated.then(|| {
        format!(
            "Ergebnis auf {cap} Treffer begrenzt; es gibt weitere Treffer, die nicht angezeigt werden."
        )
    });

    let payload = serde_json::to_value(GlobResult { matches, note })
        .unwrap_or(serde_json::Value::Array(vec![]));
    Ok(ToolOutput::json(payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_sandbox::{
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
    ) -> SandboxSpec {
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

    fn glob_args(pattern: &str, path: Option<&str>, max_results: Option<usize>) -> GlobArgs {
        GlobArgs {
            pattern: pattern.to_owned(),
            path: path.map(str::to_owned),
            max_results,
        }
    }

    fn extract_matches(output: ToolOutput) -> Vec<String> {
        match output {
            ToolOutput::Json { content } => content["matches"]
                .as_array()
                .expect("matches must be an array")
                .iter()
                .map(|v| v.as_str().expect("match must be a string").to_owned())
                .collect(),
            other => panic!("expected json output, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_fs_glob_finds_rust_files_recursively() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(ws.join("src/nested")).unwrap();
        fs::write(ws.join("src/main.rs"), "fn main() {}").unwrap();
        fs::write(ws.join("src/nested/lib.rs"), "pub fn f() {}").unwrap();
        fs::write(ws.join("readme.md"), "not rust").unwrap();

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace]);
        let ctx = make_ctx(sandbox);
        let args = glob_args("**/*.rs", None, None);

        let output = fs_glob(&ctx, args).await.unwrap();
        let matches = extract_matches(output);

        assert_eq!(
            matches,
            vec!["src/main.rs".to_owned(), "src/nested/lib.rs".to_owned()]
        );
    }

    #[tokio::test]
    async fn test_fs_glob_ignores_target_dir() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(ws.join("target")).unwrap();
        fs::create_dir_all(ws.join("src")).unwrap();
        fs::write(ws.join("target/generated.rs"), "// generated").unwrap();
        fs::write(ws.join("src/main.rs"), "fn main() {}").unwrap();

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace]);
        let ctx = make_ctx(sandbox);
        let args = glob_args("**/*.rs", None, None);

        let output = fs_glob(&ctx, args).await.unwrap();
        let matches = extract_matches(output);

        assert_eq!(matches, vec!["src/main.rs".to_owned()]);
    }

    #[tokio::test]
    async fn test_fs_glob_respects_gitignore() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join(".gitignore"), "ignored.rs\n").unwrap();
        fs::write(ws.join("ignored.rs"), "// ignored").unwrap();
        fs::write(ws.join("kept.rs"), "// kept").unwrap();

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace]);
        let ctx = make_ctx(sandbox);
        let args = glob_args("*.rs", None, None);

        let output = fs_glob(&ctx, args).await.unwrap();
        let matches = extract_matches(output);

        assert_eq!(matches, vec!["kept.rs".to_owned()]);
    }

    #[tokio::test]
    async fn test_fs_glob_caps_at_max_results() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        for i in 0..5 {
            fs::write(ws.join(format!("file{i}.rs")), "// f").unwrap();
        }

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace]);
        let ctx = make_ctx(sandbox);
        let args = glob_args("*.rs", None, Some(2));

        let output = fs_glob(&ctx, args).await.unwrap();
        match output {
            ToolOutput::Json { content } => {
                let matches = content["matches"].as_array().unwrap();
                assert_eq!(matches.len(), 2, "expected result capped at 2");
                assert!(
                    content["note"].as_str().is_some(),
                    "expected a truncation note"
                );
            }
            other => panic!("expected json output, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_fs_glob_invalid_pattern_returns_error() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace]);
        let ctx = make_ctx(sandbox);
        let args = glob_args("[", None, None);

        let output = fs_glob(&ctx, args).await.unwrap();
        match output {
            ToolOutput::Error { message } => {
                assert!(message.contains("ungültiges Muster"), "got: {message}");
            }
            other => panic!("expected error output, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_fs_glob_denied_when_no_read_permission() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![]);
        let ctx = make_ctx(sandbox);
        let call = harw_tools::ToolCall {
            id: ToolCallId::new(),
            name: harw_tools::spec::ToolName::new("fs.glob"),
            arguments: serde_json::json!({ "pattern": "*.rs" }),
        };

        let tool = FsGlobTool;
        let result = tool.execute(&ctx, &call).await.unwrap();
        match result {
            ToolOutput::Error { message } => {
                assert!(message.contains("ReadWorkspace"), "unexpected: {message}");
            }
            other => panic!("expected error output, got: {other:?}"),
        }
    }
}
