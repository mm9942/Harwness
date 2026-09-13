//! `fs.search` — Tool-Executor für rekursive Textsuche in Dateien.
//!
//! # Verantwortung
//! Dieses Modul besitzt den `fs.search`-Executor:
//! - [`FsSearchExecutor`]: implementiert [`ToolExecutor`]; durchsucht Dateien
//!   rekursiv (BFS, Depth-Limit) nach einem String-Query. Prüft
//!   `ReadWorkspace`-Permission und delegiert Path-Resolution an
//!   `ctx.sandbox().workspace().resolve_existing()`.
//!
//! # Schlüsseltypen
//! - [`FsSearchExecutor`]
//!
//! # Nebenläufigkeit
//! [`FsSearchExecutor`] ist `Send + Sync`. `fs.search` ist `parallel_safe`.
//!
//! # Fehler
//! Permission-Fehler → `Ok(ToolOutput::error(...))`. Kein Panic.

use harw_sandbox::Permission;
use harw_tools::{
    ToolCall, ToolOutput,
    error::ToolsError,
    executor::{ToolExecutionContext, ToolExecutor, ToolExecutorFuture},
};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;

/// Standard-Maximum für Suchergebnisse.
pub const DEFAULT_MAX_MATCHES: usize = 100;

/// Standard-Tiefe der rekursiven Suche.
pub const DEFAULT_MAX_DEPTH: usize = 8;

/// Verzeichnisnamen, die bei der Suche übersprungen werden.
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules"];

/// Deserialisierte Argumente für `fs.search`.
#[derive(Debug, Deserialize)]
struct FsSearchArgs {
    /// Suchbegriff (case-sensitive substring match).
    query: String,
    /// Start-Pfad relativ zum Workspace-Root (Default: `"."`).
    path: Option<String>,
    /// Maximale Anzahl Treffer (Default: [`DEFAULT_MAX_MATCHES`]).
    max_matches: Option<usize>,
}

/// Ein Suchtreffer.
#[derive(Debug, Serialize)]
struct SearchMatch {
    /// Pfad der Datei relativ zum Workspace-Root.
    path: String,
    /// Zeilennummer (1-basiert).
    line: u32,
    /// Zeileninhalt (getrimmt).
    text: String,
}

/// Führt `fs.search`-Aufrufe aus.
///
/// # Description
/// Durchsucht Dateien rekursiv (BFS) relativ zum Workspace-Root des
/// Sandbox-Kontexts nach einem String-Query. Überspringt `.git`, `target` und
/// `node_modules`. Beschränkt Tiefe auf `max_depth` und Treffer auf
/// `max_matches`.
///
/// # Arguments
/// - `max_matches` (`usize`): konfiguriertes Treffer-Maximum.
/// - `max_depth` (`usize`): maximale Rekursionstiefe.
///
/// # Concurrency
/// `Send + Sync`; über [`std::sync::Arc`] teilbar. Reads sind commutative →
/// `fs.search` ist `parallel_safe`.
///
/// # Errors
/// Permission-Fehler → `Ok(ToolOutput::error(...))`. Kein Panic.
///
/// # Examples
/// ```rust,no_run
/// use harw_tool_fs::FsSearchExecutor;
/// let _executor = FsSearchExecutor { max_matches: 100, max_depth: 8 };
/// ```
pub struct FsSearchExecutor {
    /// Maximale Anzahl zurückgegebener Treffer.
    pub max_matches: usize,
    /// Maximale Rekursionstiefe.
    pub max_depth: usize,
}

impl FsSearchExecutor {
    /// Führt die rekursive Suche synchron aus.
    ///
    /// # Errors
    /// `Err(ToolsError::InvalidArguments)` bei fehlerhaften JSON-Argumenten.
    /// Alle anderen Fehler → `Ok(ToolOutput::error(...))`.
    fn search_files(
        &self,
        ctx: &ToolExecutionContext,
        call: &ToolCall,
    ) -> Result<ToolOutput, ToolsError> {
        // Permission check
        if let Some(err) = harw_tools::sandbox_guard::require_permission(
            ctx,
            Permission::ReadWorkspace,
            "fs.search",
        ) {
            return Ok(err);
        }

        // Parse arguments
        let args: FsSearchArgs =
            match serde_json::from_value::<FsSearchArgs>(call.arguments.clone()) {
                Ok(a) => a,
                Err(err) => {
                    return Err(ToolsError::InvalidArguments {
                        name: "fs.search".to_owned(),
                        reason: err.to_string(),
                    });
                }
            };

        // Resolve start path
        let start_path = args.path.as_deref().unwrap_or(".");
        let relative = PathBuf::from(start_path);
        let start_resolved = match ctx.sandbox().workspace().resolve_existing(&relative) {
            Ok(p) => p,
            Err(err) => return Ok(ToolOutput::error(err.to_string())),
        };

        let workspace_root = ctx.sandbox().workspace().canonical_root().to_path_buf();
        let cap = args
            .max_matches
            .unwrap_or(self.max_matches)
            .min(self.max_matches);
        let mut matches: Vec<SearchMatch> = Vec::new();

        // BFS traversal with depth tracking
        // Queue entries: (path, depth)
        let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::new();
        queue.push_back((start_resolved, 0));

        'bfs: while let Some((current, depth)) = queue.pop_front() {
            if current.is_file() {
                // Scan file line by line
                let raw = match std::fs::read(&current) {
                    Ok(b) => b,
                    Err(_) => continue,
                };
                let text = String::from_utf8_lossy(&raw);
                for (line_idx, line) in text.lines().enumerate() {
                    if line.contains(args.query.as_str()) {
                        let rel_path = current
                            .strip_prefix(&workspace_root)
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|_| current.display().to_string());
                        matches.push(SearchMatch {
                            path: rel_path,
                            line: (line_idx + 1) as u32,
                            text: line.trim().to_owned(),
                        });
                        if matches.len() >= cap {
                            break 'bfs;
                        }
                    }
                }
            } else if current.is_dir() && depth < self.max_depth {
                let read_dir = match std::fs::read_dir(&current) {
                    Ok(rd) => rd,
                    Err(_) => continue,
                };
                for entry_result in read_dir {
                    let entry = match entry_result {
                        Ok(e) => e,
                        Err(_) => continue,
                    };
                    let entry_path = entry.path();
                    // Skip ignored directories
                    if entry_path.is_dir() {
                        let dir_name = entry_path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("");
                        if SKIP_DIRS.contains(&dir_name) {
                            continue;
                        }
                    }
                    queue.push_back((entry_path, depth + 1));
                }
            }
        }

        let json = serde_json::to_value(matches).unwrap_or(serde_json::Value::Array(vec![]));
        Ok(ToolOutput::json(json))
    }
}

impl ToolExecutor for FsSearchExecutor {
    /// Führt eine `fs.search`-Invokation aus.
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
    /// Sicher für parallele Aufrufe; `fs.search` ist `parallel_safe`.
    fn execute<'a>(
        &'a self,
        context: &'a ToolExecutionContext,
        call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        let result = self.search_files(context, call);
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
            name: harw_tools::spec::ToolName::new("fs.search"),
            arguments: args,
        }
    }

    #[test]
    fn test_fs_search_finds_matches() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("code.rs"), "fn hello() {}\nfn world() {}\n").unwrap();
        fs::write(ws.join("other.txt"), "no match here\n").unwrap();

        let executor = FsSearchExecutor {
            max_matches: DEFAULT_MAX_MATCHES,
            max_depth: DEFAULT_MAX_DEPTH,
        };
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace]);
        let ctx = make_ctx(sandbox);
        let call = make_call(serde_json::json!({ "query": "hello" }));

        let result = executor.search_files(&ctx, &call).unwrap();
        match result {
            ToolOutput::Json { content } => {
                let arr = content.as_array().expect("should be array");
                assert!(!arr.is_empty(), "expected at least one match");
                let first = &arr[0];
                assert_eq!(first["line"].as_u64().unwrap(), 1);
                assert!(first["text"].as_str().unwrap().contains("hello"));
            }
            other => panic!("expected json output, got: {other:?}"),
        }
    }

    #[test]
    fn test_fs_search_denied_when_no_read_permission() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();

        let executor = FsSearchExecutor {
            max_matches: DEFAULT_MAX_MATCHES,
            max_depth: DEFAULT_MAX_DEPTH,
        };
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![]);
        let ctx = make_ctx(sandbox);
        let call = make_call(serde_json::json!({ "query": "anything" }));

        let result = executor.search_files(&ctx, &call).unwrap();
        match result {
            ToolOutput::Error { message } => {
                assert!(message.contains("ReadWorkspace"), "unexpected: {message}");
            }
            other => panic!("expected error output, got: {other:?}"),
        }
    }

    #[test]
    fn test_fs_search_invalid_args_returns_err() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();

        let executor = FsSearchExecutor {
            max_matches: DEFAULT_MAX_MATCHES,
            max_depth: DEFAULT_MAX_DEPTH,
        };
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace]);
        let ctx = make_ctx(sandbox);
        // Missing "query"
        let call = make_call(serde_json::json!({ "path": "." }));

        let result = executor.search_files(&ctx, &call);
        assert!(
            matches!(result, Err(ToolsError::InvalidArguments { .. })),
            "expected InvalidArguments, got: {result:?}"
        );
    }

    #[test]
    fn test_fs_search_empty_result_when_no_match() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("plain.txt"), "no match here at all\n").unwrap();

        let executor = FsSearchExecutor {
            max_matches: DEFAULT_MAX_MATCHES,
            max_depth: DEFAULT_MAX_DEPTH,
        };
        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace]);
        let ctx = make_ctx(sandbox);
        let call = make_call(serde_json::json!({ "query": "xyzzy_not_present" }));

        let result = executor.search_files(&ctx, &call).unwrap();
        match result {
            ToolOutput::Json { content } => {
                let arr = content.as_array().expect("should be array");
                assert!(arr.is_empty(), "expected no matches");
            }
            other => panic!("expected json output, got: {other:?}"),
        }
    }
}
