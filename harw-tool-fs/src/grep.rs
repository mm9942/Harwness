//! `fs.grep` — Tool-Executor für Regex-basierte Dateisuche mit Kontextzeilen.
//!
//! Spec-Referenz: AP W2-01..03, Abschnitt "2. `grep.rs` — `fs.grep`".
//!
//! # Verantwortung
//! Dieses Modul besitzt den `fs.grep`-Executor:
//! - [`GrepArgs`]: deserialisierte Aufrufargumente, per `#[derive(harw_macros::Tool)]`
//!   auch Quelle der JSON-Schema-Spezifikation.
//! - `fs_grep` (per `#[harw_macros::tool]` zu [`FsGrepTool`] erweitert): durchsucht
//!   Dateien unterhalb eines Start-Verzeichnisses zeilenweise mit einem
//!   `regex`-Muster. Traversierung über die `ignore`-Crate (respektiert
//!   `.gitignore`; `target/` und `.git/` werden zusätzlich hart
//!   ausgeschlossen). Optionale Dateiauswahl über ein `globset`-Muster.
//!   Binärdateien (NUL-Byte in den ersten 8 KiB) und Dateien über dem
//!   Größenlimit werden übersprungen. Ausgabe im GNU-grep-Stil
//!   (`pfad:zeile: inhalt` für Treffer, `pfad-zeile- inhalt` für Kontext).
//!
//! # Schlüsseltypen
//! - [`GrepArgs`]
//! - [`FsGrepTool`] (generiert durch `#[harw_macros::tool]`)
//!
//! # Nebenläufigkeit
//! [`FsGrepTool`] ist `Send + Sync` (Unit-Struct). `fs.grep` ist `parallel_safe`.
//!
//! # Fehler
//! Permission-Fehler (erzwungen durch den Makro-Prolog vor der
//! Deserialisierung), ungültige Regex- oder Glob-Muster und
//! Pfadauflösungsfehler münden alle in `Ok(ToolOutput::error(...))`. Kein Panic.

use globset::GlobBuilder;
use harw_macros::Tool;
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use ignore::WalkBuilder;
use regex::RegexBuilder;
use serde::Deserialize;
use std::ffi::OsStr;
use std::path::Path;

/// Standard-Obergrenze für `fs.grep`-Treffer.
pub const DEFAULT_MAX_MATCHES: usize = 100;

/// Standard-Anzahl an Kontextzeilen vor/nach einem Treffer.
pub const DEFAULT_CONTEXT_LINES: usize = 0;

/// Maximal erlaubte Kontextzeilen (Vor- und Nachzeilen je), unabhängig vom
/// per Aufruf angeforderten Wert.
pub const MAX_CONTEXT_LINES: usize = 10;

/// Maximale Dateigröße, die noch durchsucht wird. Größere Dateien werden
/// übersprungen, um den Speicherbedarf einer einzelnen Suche zu begrenzen
/// (z. B. Log-Dumps oder Binär-Artefakte, die versehentlich im Workspace liegen).
pub const MAX_FILE_SIZE_BYTES: u64 = 2 * 1024 * 1024;

/// Anzahl der Bytes am Dateianfang, die auf ein NUL-Byte geprüft werden, um
/// Binärdateien heuristisch zu erkennen.
const BINARY_SNIFF_BYTES: usize = 8192;

/// Verzeichnis- bzw. Dateinamen, die unabhängig von `.gitignore` immer
/// übersprungen werden.
const HARD_EXCLUDED_NAMES: &[&str] = &["target", ".git"];

/// Deserialisierte Argumente für `fs.grep`.
#[derive(Debug, Tool, Deserialize)]
#[tool(
    name = "fs.grep",
    description = "Durchsucht Dateien im Workspace mit einem regulären Ausdruck."
)]
pub struct GrepArgs {
    /// Rust-Regex (crate `regex`).
    pub pattern: String,
    /// Optionales Unterverzeichnis.
    pub path: Option<String>,
    /// Optionales Glob-Muster zur Dateiauswahl (Default: alle Textdateien).
    pub glob: Option<String>,
    /// Kontextzeilen vor und nach dem Treffer (Default 0, Maximum 10).
    #[tool(default = 0)]
    #[serde(default)]
    pub context_lines: Option<usize>,
    /// Obergrenze der Treffer (Default 100).
    #[tool(default = 100)]
    #[serde(default)]
    pub max_matches: Option<usize>,
    /// Groß-/Kleinschreibung ignorieren.
    #[tool(default = false)]
    #[serde(default)]
    pub case_insensitive: Option<bool>,
}

/// Ein einzelner Regex-Treffer mit optionalen Kontextzeilen.
struct GrepHit {
    /// Pfad relativ zur Workspace-Wurzel.
    path: String,
    /// 1-basierte Zeilennummer der Treffer-Zeile.
    line: usize,
    /// Kontextzeilen vor dem Treffer als `(Zeilennummer, Text)`, aufsteigend sortiert.
    before: Vec<(usize, String)>,
    /// Text der Treffer-Zeile.
    text: String,
    /// Kontextzeilen nach dem Treffer als `(Zeilennummer, Text)`, aufsteigend sortiert.
    after: Vec<(usize, String)>,
}

/// Prüft, ob ein Datei- oder Verzeichnisname unabhängig von `.gitignore`
/// hart ausgeschlossen werden soll (`target`, `.git`).
fn is_hard_excluded(name: &OsStr) -> bool {
    name.to_str()
        .is_some_and(|s| HARD_EXCLUDED_NAMES.contains(&s))
}

/// Formatiert die gesammelten Treffer im GNU-grep-Stil: `pfad:zeile: inhalt`
/// für die Treffer-Zeile, `pfad-zeile- inhalt` für Kontextzeilen, mit einem
/// `--`-Trenner zwischen nicht zusammenhängenden Treffergruppen.
fn format_hits(hits: &[GrepHit]) -> Vec<String> {
    let mut out_lines: Vec<String> = Vec::new();
    let mut last: Option<(String, usize)> = None;

    for hit in hits {
        let first_line = hit.before.first().map_or(hit.line, |(n, _)| *n);
        let needs_separator = match &last {
            Some((path, line)) => *path != hit.path || first_line > line + 1,
            None => false,
        };
        if needs_separator {
            out_lines.push("--".to_owned());
        }

        for (n, text) in &hit.before {
            out_lines.push(format!("{}-{}- {}", hit.path, n, text));
        }
        out_lines.push(format!("{}:{}: {}", hit.path, hit.line, hit.text));
        for (n, text) in &hit.after {
            out_lines.push(format!("{}-{}- {}", hit.path, n, text));
        }

        let last_line = hit.after.last().map_or(hit.line, |(n, _)| *n);
        last = Some((hit.path.clone(), last_line));
    }

    out_lines
}

/// Führt die `fs.grep`-Suche aus: durchsucht Dateien unterhalb eines
/// (optionalen) Start-Verzeichnisses zeilenweise mit `args.pattern`.
///
/// Berechtigungsprüfung (`ReadWorkspace`) und JSON-Deserialisierung laufen im
/// von `#[harw_macros::tool]` generierten Prolog von [`FsGrepTool`], bevor
/// diese Funktion aufgerufen wird.
///
/// # Errors
/// Liefert nie `Err`; Pfad-, Muster- oder I/O-Fehler werden als
/// `Ok(ToolOutput::error(...))` zurückgegeben.
#[harw_macros::tool(
    name = "fs.grep",
    description = "Durchsucht Dateien im Workspace mit einem regulären Ausdruck und liefert Treffer im GNU-grep-Stil.",
    permission = "read_workspace",
    parallel_safe
)]
async fn fs_grep(context: &ToolExecutionContext, args: GrepArgs) -> Result<ToolOutput, ToolsError> {
    let workspace = context.sandbox().workspace();

    let start_relative = args.path.as_deref().unwrap_or(".");
    let start_resolved = match workspace.resolve_existing(Path::new(start_relative)) {
        Ok(path) => path,
        Err(err) => return Ok(ToolOutput::error(err.to_string())),
    };
    if !start_resolved.is_dir() {
        return Ok(ToolOutput::error(format!(
            "fs.grep: '{start_relative}' ist kein Verzeichnis"
        )));
    }

    let regex = match RegexBuilder::new(&args.pattern)
        .case_insensitive(args.case_insensitive.unwrap_or(false))
        .build()
    {
        Ok(re) => re,
        Err(err) => {
            return Ok(ToolOutput::error(format!(
                "fs.grep: ungültiges Muster '{}': {err}",
                args.pattern
            )));
        }
    };

    let file_matcher = match args.glob.as_deref() {
        Some(pattern) => match GlobBuilder::new(pattern).literal_separator(true).build() {
            Ok(glob) => Some(glob.compile_matcher()),
            Err(err) => {
                return Ok(ToolOutput::error(format!(
                    "fs.grep: ungültiges Glob-Muster '{pattern}': {err}"
                )));
            }
        },
        None => None,
    };

    let workspace_root = workspace.canonical_root().to_path_buf();
    let context_lines = args
        .context_lines
        .unwrap_or(DEFAULT_CONTEXT_LINES)
        .min(MAX_CONTEXT_LINES);
    let cap = args.max_matches.unwrap_or(DEFAULT_MAX_MATCHES).max(1);

    let mut walk_builder = WalkBuilder::new(&start_resolved);
    walk_builder
        .hidden(false)
        .parents(true)
        .git_ignore(true)
        .require_git(false)
        .filter_entry(|entry| entry.depth() == 0 || !is_hard_excluded(entry.file_name()));

    let mut hits: Vec<GrepHit> = Vec::new();
    let mut skipped = 0usize;
    let mut truncated = false;

    'walk: for entry in walk_builder.build() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }

        if let Some(matcher) = &file_matcher {
            let relative = entry
                .path()
                .strip_prefix(&start_resolved)
                .unwrap_or_else(|_| entry.path());
            if !matcher.is_match(relative) {
                continue;
            }
        }

        let metadata = match std::fs::metadata(entry.path()) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if metadata.len() > MAX_FILE_SIZE_BYTES {
            skipped += 1;
            continue;
        }

        let raw = match std::fs::read(entry.path()) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let sniff_len = raw.len().min(BINARY_SNIFF_BYTES);
        if raw[..sniff_len].contains(&0u8) {
            skipped += 1;
            continue;
        }

        // UTF-8-sicher: verlustbehaftet dekodieren statt nach Byte-Index zu schneiden.
        let text = String::from_utf8_lossy(&raw);
        let lines: Vec<&str> = text.lines().collect();
        let output_path = entry
            .path()
            .strip_prefix(&workspace_root)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| entry.path().display().to_string());

        for (idx, line) in lines.iter().enumerate() {
            if !regex.is_match(line) {
                continue;
            }

            let before_start = idx.saturating_sub(context_lines);
            let before = (before_start..idx)
                .map(|i| (i + 1, lines[i].to_owned()))
                .collect::<Vec<_>>();

            let after_end = (idx + 1 + context_lines).min(lines.len());
            let after = ((idx + 1)..after_end)
                .map(|i| (i + 1, lines[i].to_owned()))
                .collect::<Vec<_>>();

            hits.push(GrepHit {
                path: output_path.clone(),
                line: idx + 1,
                before,
                text: (*line).to_owned(),
                after,
            });

            if hits.len() >= cap {
                truncated = true;
                break 'walk;
            }
        }
    }

    let mut out_lines = format_hits(&hits);

    if truncated || skipped > 0 {
        let mut note_parts = Vec::new();
        if truncated {
            note_parts.push(format!("Ergebnis auf {cap} Treffer begrenzt"));
        }
        if skipped > 0 {
            note_parts.push(format!(
                "{skipped} Datei(en) übersprungen (Binärdatei oder Größenlimit überschritten)"
            ));
        }
        out_lines.push(format!("# Hinweis: {}.", note_parts.join("; ")));
    }

    Ok(ToolOutput::text(out_lines.join("\n")))
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

    fn grep_args(pattern: &str) -> GrepArgs {
        GrepArgs {
            pattern: pattern.to_owned(),
            path: None,
            glob: None,
            context_lines: None,
            max_matches: None,
            case_insensitive: None,
        }
    }

    fn text_of(output: ToolOutput) -> String {
        match output {
            ToolOutput::Text { content } => content,
            other => panic!("expected text output, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_fs_grep_finds_matches_with_context_lines() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        fs::write(
            ws.join("file.txt"),
            "alpha\nbeta\nGAMMA_MATCH\ndelta\nepsilon\n",
        )
        .unwrap();

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace]);
        let ctx = make_ctx(sandbox);
        let mut args = grep_args("GAMMA_MATCH");
        args.context_lines = Some(1);

        let output = fs_grep(&ctx, args).await.unwrap();
        let text = text_of(output);
        let lines: Vec<&str> = text.lines().collect();

        assert_eq!(
            lines,
            vec![
                "file.txt-2- beta",
                "file.txt:3: GAMMA_MATCH",
                "file.txt-4- delta",
            ]
        );
    }

    #[tokio::test]
    async fn test_fs_grep_case_insensitive() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("file.txt"), "some GAMMA_MATCH here\n").unwrap();

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace]);
        let ctx = make_ctx(sandbox);

        let mut insensitive_args = grep_args("gamma_match");
        insensitive_args.case_insensitive = Some(true);
        let output = fs_grep(&ctx, insensitive_args).await.unwrap();
        assert!(
            text_of(output).contains("GAMMA_MATCH"),
            "case-insensitive search must find the uppercase line"
        );

        let sensitive_args = grep_args("gamma_match");
        let output = fs_grep(&ctx, sensitive_args).await.unwrap();
        assert!(
            text_of(output).is_empty(),
            "case-sensitive search must not find the uppercase line"
        );
    }

    #[tokio::test]
    async fn test_fs_grep_skips_binary_files() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        let mut binary_content = b"MATCHME".to_vec();
        binary_content.push(0u8);
        binary_content.extend_from_slice(b"more MATCHME bytes");
        fs::write(ws.join("binary.bin"), &binary_content).unwrap();
        fs::write(ws.join("plain.txt"), "MATCHME in plain text\n").unwrap();

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace]);
        let ctx = make_ctx(sandbox);
        let args = grep_args("MATCHME");

        let output = fs_grep(&ctx, args).await.unwrap();
        let text = text_of(output);

        assert!(text.contains("plain.txt"), "plain text file must match");
        assert!(
            !text.contains("binary.bin"),
            "binary file must be skipped: {text}"
        );
    }

    #[tokio::test]
    async fn test_fs_grep_caps_at_max_matches() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();
        let content = (0..10)
            .map(|i| format!("line MATCHME {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(ws.join("many.txt"), content).unwrap();

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace]);
        let ctx = make_ctx(sandbox);
        let mut args = grep_args("MATCHME");
        args.max_matches = Some(3);

        let output = fs_grep(&ctx, args).await.unwrap();
        let text = text_of(output);
        let match_line_count = text
            .lines()
            .filter(|l| !l.starts_with('#') && l.contains(':'))
            .count();

        assert_eq!(match_line_count, 3, "expected exactly 3 match lines: {text}");
        assert!(
            text.contains("# Hinweis") && text.contains("begrenzt"),
            "expected a truncation hint: {text}"
        );
    }

    #[tokio::test]
    async fn test_fs_grep_invalid_pattern_returns_error() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace]);
        let ctx = make_ctx(sandbox);
        let args = grep_args("(unclosed");

        let output = fs_grep(&ctx, args).await.unwrap();
        match output {
            ToolOutput::Error { message } => {
                assert!(message.contains("ungültiges Muster"), "got: {message}");
            }
            other => panic!("expected error output, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_fs_grep_denied_when_no_read_permission() {
        let dir = TempDir::new().unwrap();
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws).unwrap();

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![]);
        let ctx = make_ctx(sandbox);
        let call = harw_tools::ToolCall {
            id: ToolCallId::new(),
            name: harw_tools::spec::ToolName::new("fs.grep"),
            arguments: serde_json::json!({ "pattern": "anything" }),
        };

        let tool = FsGrepTool;
        let result = tool.execute(&ctx, &call).await.unwrap();
        match result {
            ToolOutput::Error { message } => {
                assert!(message.contains("ReadWorkspace"), "unexpected: {message}");
            }
            other => panic!("expected error output, got: {other:?}"),
        }
    }
}
