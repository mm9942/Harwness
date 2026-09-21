//! `fs.search` — Tool-Executor für rekursive Textsuche in Dateien.
//!
//! # Verantwortung
//! Dieses Modul besitzt den `fs.search`-Executor:
//! - [`FsSearchExecutor`]: implementiert [`ToolExecutor`]; durchsucht Dateien
//!   rekursiv nach einem String-Query. Prüft `ReadWorkspace`-Permission.
//!
//! # Sicherheit (W1-02, F-015)
//! Der Walk läuft über [`crate::tree::walk_tree`] (Basis
//! `harw_fsutil::walk_beneath`): Symlinks werden **nie** gefolgt — weder als
//! Verzeichnis noch als Datei —, Dateien werden ausschließlich über
//! `open_beneath` relativ zum Deskriptor ihres Elternverzeichnisses geöffnet.
//! Harte Grenzen: höchstens 1000 Treffer, Tiefe 32, 50 000 Einträge, 10 s,
//! Dateien über 8 MiB werden übersprungen, Ausgabe höchstens 64 KiB, Zeilen
//! höchstens 1024 Bytes. Der Abbruchgrund steht im Feld `stopped`.
//!
//! # Schlüsseltypen
//! - [`FsSearchExecutor`]
//!
//! # Nebenläufigkeit
//! [`FsSearchExecutor`] ist `Send + Sync`. `fs.search` ist `parallel_safe`.
//! Walk und Datei-IO laufen im Blocking-Pool.
//!
//! # Fehler
//! Permission-Fehler → `Ok(ToolOutput::error(...))`. Kein Panic.

use crate::tree::{
    HARD_MAX_RESULTS, MAX_OUTPUT_BYTES, MAX_SCAN_FILE_BYTES, StopReason, WalkOptions, Workspace,
    normalize_relative, open_file_in, read_bounded, truncate_line, walk_tree,
};
use harw_fsutil::EntryType;
use harw_authority::Permission;
use harw_tools::{
    ToolCall, ToolOutput,
    error::ToolsError,
    executor::{ToolExecutionContext, ToolExecutor, ToolExecutorFuture},
};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::fs::File;
use std::ops::ControlFlow;
use std::path::Path;

/// Standard-Maximum für Suchergebnisse.
pub const DEFAULT_MAX_MATCHES: usize = 100;

/// Standard-Tiefe der rekursiven Suche.
pub const DEFAULT_MAX_DEPTH: usize = 8;

/// Verzeichnisnamen, die bei der Suche übersprungen werden.
const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules"];

/// Geschätzter JSON-Overhead je Treffer (Feldnamen, Zeilennummer, Quoting).
const MATCH_OVERHEAD_BYTES: usize = 32;

/// Deserialisierte Argumente für `fs.search`.
#[derive(Debug, Deserialize)]
struct FsSearchArgs {
    /// Suchbegriff (case-sensitive substring match).
    query: String,
    /// Start-Pfad relativ zum Workspace-Root (Default: `"."`).
    path: Option<String>,
    /// Maximale Anzahl Treffer (Default: [`DEFAULT_MAX_MATCHES`]).
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    max_matches: Option<usize>,
}

/// Ein Suchtreffer.
#[derive(Debug, Serialize)]
struct SearchMatch {
    /// Pfad der Datei relativ zum Workspace-Root.
    path: String,
    /// Zeilennummer (1-basiert).
    line: u32,
    /// Zeileninhalt (getrimmt, höchstens 1024 Bytes).
    text: String,
}

/// JSON-Ergebnis von `fs.search`.
#[derive(Debug, Serialize)]
struct SearchResult {
    /// Treffer in Walk-Reihenfolge (sortierte Tiefensuche).
    matches: Vec<SearchMatch>,
    /// Abbruchgrund, falls die Suche unvollständig ist.
    #[serde(skip_serializing_if = "Option::is_none")]
    stopped: Option<&'static str>,
    /// Anzahl übersprungener Dateien (größer als 8 MiB).
    #[serde(skip_serializing_if = "is_zero")]
    skipped_files: usize,
}

/// `serde`-Hilfe: Zähler 0 nicht ausgeben.
fn is_zero(count: &usize) -> bool {
    *count == 0
}

/// Überspringt die Verzeichnisse aus [`SKIP_DIRS`] (Dateien gleichen Namens nicht).
fn skip_dir(name: &OsStr, is_dir: bool) -> bool {
    is_dir && name.to_str().is_some_and(|name| SKIP_DIRS.contains(&name))
}

/// Sammelt Treffer unter Einhaltung von Treffer- und Ausgabegrenze.
struct Scanner<'q> {
    query: &'q str,
    cap: usize,
    matches: Vec<SearchMatch>,
    output_bytes: usize,
    skipped_files: usize,
}

impl Scanner<'_> {
    /// Durchsucht eine bereits symlinkfrei geöffnete Datei.
    fn scan(&mut self, file: &File, rel: &Path) -> ControlFlow<StopReason> {
        let bytes = match read_bounded(file, MAX_SCAN_FILE_BYTES) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                self.skipped_files += 1;
                return ControlFlow::Continue(());
            }
            Err(_) => return ControlFlow::Continue(()),
        };
        let text = String::from_utf8_lossy(&bytes);
        let path = rel.display().to_string();
        for (line_idx, line) in text.lines().enumerate() {
            if !line.contains(self.query) {
                continue;
            }
            if self.matches.len() >= self.cap {
                return ControlFlow::Break(StopReason::ResultLimit);
            }
            let text = truncate_line(line.trim());
            let cost = path.len() + text.len() + MATCH_OVERHEAD_BYTES;
            if self.output_bytes + cost > MAX_OUTPUT_BYTES {
                return ControlFlow::Break(StopReason::OutputLimit);
            }
            self.output_bytes += cost;
            self.matches.push(SearchMatch {
                path: path.clone(),
                line: u32::try_from(line_idx + 1).unwrap_or(u32::MAX),
                text,
            });
        }
        ControlFlow::Continue(())
    }
}

/// Führt `fs.search`-Aufrufe aus.
///
/// # Description
/// Durchsucht Dateien rekursiv relativ zum Workspace-Root des
/// Sandbox-Kontexts nach einem String-Query. Überspringt `.git`, `target` und
/// `node_modules`. Beschränkt Tiefe auf `max_depth` (höchstens 32) und Treffer
/// auf `max_matches` (höchstens 1000). Symlinks werden nie gefolgt.
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
    /// Führt die rekursive Suche synchron (blockierend) aus.
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

        let start_input = args.path.as_deref().unwrap_or(".");
        let start_rel = match normalize_relative(start_input) {
            Ok(rel) => rel,
            Err(reason) => return Ok(ToolOutput::error(format!("fs.search: {reason}"))),
        };
        // Erst gegen die Obergrenze dieses Werkzeugs kappen, dann in die
        // harten Grenzen klemmen (mindestens ein Treffer).
        let cap = args
            .max_matches
            .unwrap_or(self.max_matches)
            .min(self.max_matches)
            .clamp(1, HARD_MAX_RESULTS);

        let opened = Workspace::open(ctx.sandbox().workspace().canonical_root()).and_then(
            |workspace| {
                let start = workspace.open_any(&start_rel)?;
                let is_dir = start.metadata()?.is_dir();
                Ok((workspace, start, is_dir))
            },
        );
        let (workspace, start, is_dir) = match opened {
            Ok(opened) => opened,
            Err(err) => {
                return Ok(ToolOutput::error(format!(
                    "fs.search: '{start_input}' kann nicht geöffnet werden \
                     (Symlinks werden nicht verfolgt): {err}"
                )));
            }
        };

        let mut scanner = Scanner {
            query: &args.query,
            cap,
            matches: Vec::new(),
            output_bytes: 0,
            skipped_files: 0,
        };
        let stop = if is_dir {
            drop(start);
            let opts = WalkOptions {
                max_depth: self.max_depth,
                ..WalkOptions::standard(false, skip_dir)
            };
            let walked = walk_tree(&workspace, &start_rel, opts, |entry| {
                if entry.entry_type != EntryType::File {
                    return ControlFlow::Continue(());
                }
                if entry.len > MAX_SCAN_FILE_BYTES {
                    scanner.skipped_files += 1;
                    return ControlFlow::Continue(());
                }
                match open_file_in(entry.dir, entry.name) {
                    Ok(file) => scanner.scan(&file, entry.rel),
                    Err(_) => ControlFlow::Continue(()),
                }
            });
            match walked {
                Ok(stop) => stop,
                Err(err) => {
                    return Ok(ToolOutput::error(format!(
                        "fs.search: '{start_input}' kann nicht gelesen werden: {err}"
                    )));
                }
            }
        } else {
            match scanner.scan(&start, &start_rel) {
                ControlFlow::Break(reason) => Some(reason),
                ControlFlow::Continue(()) => None,
            }
        };

        let result = SearchResult {
            matches: scanner.matches,
            stopped: stop.map(StopReason::as_str),
            skipped_files: scanner.skipped_files,
        };
        let json = serde_json::to_value(result).unwrap_or(serde_json::Value::Array(vec![]));
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
    /// `Ok(ToolOutput::Json { content: {matches, stopped?, skipped_files?} })`
    /// bei Erfolg. `Ok(ToolOutput::Error { message })` bei Permission- oder
    /// I/O-Fehler.
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
        let executor = Self {
            max_matches: self.max_matches,
            max_depth: self.max_depth,
        };
        let context = context.clone();
        let call = call.clone();
        Box::pin(crate::blocking::run_blocking("fs.search", move || {
            executor.search_files(&context, &call)
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
                let arr = content["matches"].as_array().expect("should be array");
                assert!(!arr.is_empty(), "expected at least one match");
                assert_eq!(first_path(&content), "code.rs");
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
                let arr = content["matches"].as_array().expect("should be array");
                assert!(arr.is_empty(), "expected no matches");
                assert!(content.get("stopped").is_none());
            }
            other => panic!("expected json output, got: {other:?}"),
        }
    }

    fn first_path(content: &serde_json::Value) -> &str {
        content["matches"][0]["path"].as_str().expect("path")
    }

    fn search(
        fixture: &Fixture,
        executor: &FsSearchExecutor,
        args: serde_json::Value,
    ) -> ToolOutput {
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace]);
        executor.search_files(&ctx, &call("fs.search", args)).unwrap()
    }

    const DEFAULT_EXECUTOR: FsSearchExecutor = FsSearchExecutor {
        max_matches: DEFAULT_MAX_MATCHES,
        max_depth: DEFAULT_MAX_DEPTH,
    };

    #[test]
    fn test_fs_search_does_not_follow_symlinks_out_of_workspace() {
        let fixture = Fixture::new();
        fixture.plant_escapes();
        fs::write(fixture.ws.join("nested/own.txt"), "TOPSECRET-lookalike").unwrap();

        // Vorher: `link_dir` wurde betreten und `outside/secret.txt` gelesen.
        let output = search(&fixture, &DEFAULT_EXECUTOR, serde_json::json!({ "query": SECRET }));
        let ToolOutput::Json { content } = &output else {
            panic!("expected json output, got: {output:?}");
        };
        let paths: Vec<&str> = content["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["path"].as_str().unwrap())
            .collect();
        assert_eq!(paths, vec!["nested/own.txt"], "Symlink-Ziele dürfen nicht gelesen werden");
        assert!(content.get("stopped").is_none(), "Schleifen müssen terminieren: {content}");

        for path in ["link_dir", "link_file", "loop", "nested/up", "link_dir/deep", "../outside"] {
            let output = search(
                &fixture,
                &DEFAULT_EXECUTOR,
                serde_json::json!({ "query": "", "path": path }),
            );
            assert!(matches!(output, ToolOutput::Error { .. }), "{path}: {output:?}");
            assert!(!render(&output).contains(SECRET), "{path}");
        }
    }

    #[test]
    fn test_fs_search_single_file_start_and_hard_result_limit() {
        let fixture = Fixture::new();
        let lines: String = (0..1200).map(|i| format!("hit {i}\n")).collect();
        fs::write(fixture.ws.join("many.txt"), lines).unwrap();
        let executor = FsSearchExecutor {
            max_matches: usize::MAX,
            max_depth: usize::MAX,
        };

        let output = search(
            &fixture,
            &executor,
            serde_json::json!({ "query": "hit", "path": "many.txt", "max_matches": 5000 }),
        );
        let ToolOutput::Json { content } = output else {
            panic!("expected json output");
        };
        assert_eq!(content["matches"].as_array().unwrap().len(), HARD_MAX_RESULTS);
        assert_eq!(content["stopped"], "result_limit");
    }

    #[test]
    fn test_fs_search_output_limit_and_line_truncation() {
        let fixture = Fixture::new();
        let long_line = format!("needle {}\n", "x".repeat(10_000));
        fs::write(fixture.ws.join("wide.txt"), long_line.repeat(200)).unwrap();
        let executor = FsSearchExecutor {
            max_matches: HARD_MAX_RESULTS,
            max_depth: DEFAULT_MAX_DEPTH,
        };
        let output = search(&fixture, &executor, serde_json::json!({ "query": "needle" }));
        let ToolOutput::Json { content } = output else {
            panic!("expected json output");
        };
        assert_eq!(content["stopped"], "output_limit");
        let matches = content["matches"].as_array().unwrap();
        assert!(!matches.is_empty());
        for m in matches {
            assert!(m["text"].as_str().unwrap().len() <= crate::tree::MAX_LINE_BYTES + 3);
        }
        assert!(serde_json::to_string(&content).unwrap().len() <= MAX_OUTPUT_BYTES + 1024);
    }

    #[test]
    fn test_fs_search_skips_oversized_files() {
        let fixture = Fixture::new();
        fs::write(fixture.ws.join("small.txt"), "needle\n").unwrap();
        let mut big = fs::File::create(fixture.ws.join("big.log")).unwrap();
        std::io::Write::write_all(&mut big, b"needle\n").unwrap();
        big.set_len(MAX_SCAN_FILE_BYTES + 1).unwrap();

        let output = search(&fixture, &DEFAULT_EXECUTOR, serde_json::json!({ "query": "needle" }));
        let ToolOutput::Json { content } = output else {
            panic!("expected json output");
        };
        assert_eq!(content["matches"].as_array().unwrap().len(), 1);
        assert_eq!(first_path(&content), "small.txt");
        assert_eq!(content["skipped_files"], 1);
    }

    #[test]
    fn test_fs_search_skips_hard_excluded_dirs_only() {
        let fixture = Fixture::new();
        fs::create_dir_all(fixture.ws.join("target")).unwrap();
        fs::create_dir_all(fixture.ws.join("node_modules/pkg")).unwrap();
        fs::write(fixture.ws.join("target/out.txt"), "needle").unwrap();
        fs::write(fixture.ws.join("node_modules/pkg/i.js"), "needle").unwrap();
        fs::write(fixture.ws.join("src.txt"), "needle").unwrap();
        let output = search(&fixture, &DEFAULT_EXECUTOR, serde_json::json!({ "query": "needle" }));
        let ToolOutput::Json { content } = output else {
            panic!("expected json output");
        };
        assert_eq!(content["matches"].as_array().unwrap().len(), 1);
        assert_eq!(first_path(&content), "src.txt");
    }
}
