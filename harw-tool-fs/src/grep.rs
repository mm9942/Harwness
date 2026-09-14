//! `fs.grep` — Tool-Executor für Regex-basierte Dateisuche mit Kontextzeilen.
//!
//! Spec-Referenz: AP W2-01..03, Abschnitt "2. `grep.rs` — `fs.grep`";
//! Sicherheits- und Grenzenüberarbeitung: W1-02 (F-059, F-118).
//!
//! # Verantwortung
//! Dieses Modul besitzt den `fs.grep`-Executor:
//! - [`GrepArgs`]: deserialisierte Aufrufargumente, per `#[derive(harw_macros::Tool)]`
//!   auch Quelle der JSON-Schema-Spezifikation.
//! - `fs_grep` (per `#[harw_macros::tool]` zu [`FsGrepTool`] erweitert): durchsucht
//!   Dateien unterhalb eines Start-Verzeichnisses zeilenweise mit einem
//!   `regex`-Muster. Ausgabe im GNU-grep-Stil (`pfad:zeile: inhalt` für
//!   Treffer, `pfad-zeile- inhalt` für Kontext).
//!
//! # Traversierung und Grenzen (W1-02)
//! Gewalkt wird über [`crate::tree::walk_tree`] (Basis
//! `harw_fsutil::walk_beneath`): Symlinks werden nie gefolgt, Dateien nur über
//! `open_beneath` relativ zum Eltern-Deskriptor geöffnet. `.gitignore`/
//! `.ignore` gelten weiterhin (nur innerhalb des Workspace), `target/` und
//! `.git/` sind harte Ausschlüsse. Das optionale `glob` wird gegen den Pfad
//! **relativ zur Workspace-Wurzel** geprüft. Grenzen: höchstens 1000 Treffer,
//! Tiefe 32, 50 000 Einträge, 10 s, Dateien über [`MAX_FILE_SIZE_BYTES`]
//! werden übersprungen, Zeilen auf 1024 Bytes und die Gesamtausgabe auf
//! 64 KiB gekürzt. Der Abbruchgrund erscheint als `# stopped: <grund>`.
//!
//! # Schlüsseltypen
//! - [`GrepArgs`]
//! - [`FsGrepTool`] (generiert durch `#[harw_macros::tool]`)
//!
//! # Nebenläufigkeit
//! [`FsGrepTool`] ist `Send + Sync` (Unit-Struct). `fs.grep` ist `parallel_safe`.
//! Der Walk läuft über `spawn_blocking`.
//!
//! # Fehler
//! Permission-Fehler (erzwungen durch den Makro-Prolog vor der
//! Deserialisierung), ungültige Regex- oder Glob-Muster und
//! Pfadauflösungsfehler münden alle in `Ok(ToolOutput::error(...))`. Kein Panic.

use crate::blocking::run_blocking;
use crate::tree::{
    HARD_MAX_RESULTS, MAX_OUTPUT_BYTES, MAX_SCAN_FILE_BYTES, StopReason, WalkOptions, Workspace,
    normalize_relative, open_file_in, read_bounded, truncate_line, walk_tree,
};
use globset::{GlobBuilder, GlobMatcher};
use harw_fsutil::EntryType;
use harw_macros::Tool;
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use std::ffi::OsStr;
use std::ops::ControlFlow;
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
pub const MAX_FILE_SIZE_BYTES: u64 = MAX_SCAN_FILE_BYTES;

/// Anzahl der Bytes am Dateianfang, die auf ein NUL-Byte geprüft werden, um
/// Binärdateien heuristisch zu erkennen.
const BINARY_SNIFF_BYTES: usize = 8192;

/// Geschätzter Overhead je Ausgabezeile (Pfad-Trenner, Zeilennummer).
const LINE_OVERHEAD_BYTES: usize = 16;

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
    /// Optionales Glob-Muster zur Dateiauswahl, relativ zur Workspace-Wurzel
    /// (Default: alle Textdateien).
    pub glob: Option<String>,
    /// Kontextzeilen vor und nach dem Treffer (Default 0, Maximum 10).
    #[tool(default = 0)]
    #[serde(default)]
    pub context_lines: Option<usize>,
    /// Obergrenze der Treffer (Default 100, Maximum 1000).
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

impl GrepHit {
    /// Geschätzte Ausgabelänge dieses Treffers in Bytes.
    fn output_cost(&self) -> usize {
        let context: usize = self
            .before
            .iter()
            .chain(self.after.iter())
            .map(|(_, text)| text.len() + self.path.len() + LINE_OVERHEAD_BYTES)
            .sum();
        context + self.text.len() + self.path.len() + LINE_OVERHEAD_BYTES
    }
}

/// Prüft, ob ein Datei- oder Verzeichnisname unabhängig von `.gitignore`
/// hart ausgeschlossen werden soll (`target`, `.git`).
fn is_hard_excluded(name: &OsStr, _is_dir: bool) -> bool {
    name.to_str()
        .is_some_and(|name| HARD_EXCLUDED_NAMES.contains(&name))
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

/// Zustand eines laufenden `fs.grep`-Durchlaufs.
struct GrepRun<'a> {
    regex: &'a Regex,
    file_matcher: Option<&'a GlobMatcher>,
    context_lines: usize,
    cap: usize,
    hits: Vec<GrepHit>,
    output_bytes: usize,
    skipped: usize,
}

impl GrepRun<'_> {
    /// Durchsucht eine bereits symlinkfrei geöffnete Datei.
    fn scan(&mut self, file: &std::fs::File, rel: &Path) -> ControlFlow<StopReason> {
        let bytes = match read_bounded(file, MAX_FILE_SIZE_BYTES) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                self.skipped += 1;
                return ControlFlow::Continue(());
            }
            Err(_) => return ControlFlow::Continue(()),
        };
        let sniff_len = bytes.len().min(BINARY_SNIFF_BYTES);
        if bytes[..sniff_len].contains(&0u8) {
            self.skipped += 1;
            return ControlFlow::Continue(());
        }

        // UTF-8-sicher: verlustbehaftet dekodieren statt nach Byte-Index zu schneiden.
        let text = String::from_utf8_lossy(&bytes);
        let lines: Vec<&str> = text.lines().collect();
        let output_path = rel.display().to_string();

        for (idx, line) in lines.iter().enumerate() {
            if !self.regex.is_match(line) {
                continue;
            }
            if self.hits.len() >= self.cap {
                return ControlFlow::Break(StopReason::ResultLimit);
            }

            let before_start = idx.saturating_sub(self.context_lines);
            let before = (before_start..idx)
                .map(|i| (i + 1, truncate_line(lines[i])))
                .collect::<Vec<_>>();

            let after_end = (idx + 1 + self.context_lines).min(lines.len());
            let after = ((idx + 1)..after_end)
                .map(|i| (i + 1, truncate_line(lines[i])))
                .collect::<Vec<_>>();

            let hit = GrepHit {
                path: output_path.clone(),
                line: idx + 1,
                before,
                text: truncate_line(line),
                after,
            };
            if self.output_bytes + hit.output_cost() > MAX_OUTPUT_BYTES {
                return ControlFlow::Break(StopReason::OutputLimit);
            }
            self.output_bytes += hit.output_cost();
            self.hits.push(hit);
        }
        ControlFlow::Continue(())
    }
}

/// Führt die `fs.grep`-Suche aus: durchsucht Dateien unterhalb eines
/// (optionalen) Start-Verzeichnisses zeilenweise mit `args.pattern`.
///
/// Berechtigungsprüfung (`ReadWorkspace`) und JSON-Deserialisierung laufen im
/// von `#[harw_macros::tool]` generierten Prolog von [`FsGrepTool`], bevor
/// diese Funktion aufgerufen wird. Die eigentliche Arbeit läuft blockierend
/// in `spawn_blocking`.
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
    let root = context.sandbox().workspace().canonical_root().to_path_buf();
    run_blocking("fs.grep", move || Ok(grep_blocking(&root, &args))).await
}

/// Synchroner Kern von [`fs_grep`].
fn grep_blocking(root: &Path, args: &GrepArgs) -> ToolOutput {
    let start_input = args.path.as_deref().unwrap_or(".");
    let start_rel = match normalize_relative(start_input) {
        Ok(rel) => rel,
        Err(reason) => return ToolOutput::error(format!("fs.grep: {reason}")),
    };

    let regex = match RegexBuilder::new(&args.pattern)
        .case_insensitive(args.case_insensitive.unwrap_or(false))
        .build()
    {
        Ok(re) => re,
        Err(err) => {
            return ToolOutput::error(format!(
                "fs.grep: ungültiges Muster '{}': {err}",
                args.pattern
            ));
        }
    };

    let file_matcher = match args.glob.as_deref() {
        Some(pattern) => match GlobBuilder::new(pattern).literal_separator(true).build() {
            Ok(glob) => Some(glob.compile_matcher()),
            Err(err) => {
                return ToolOutput::error(format!(
                    "fs.grep: ungültiges Glob-Muster '{pattern}': {err}"
                ));
            }
        },
        None => None,
    };

    let workspace = match Workspace::open(root) {
        Ok(workspace) => workspace,
        Err(err) => return ToolOutput::error(format!("fs.grep: Workspace nicht lesbar: {err}")),
    };
    let context_lines = args
        .context_lines
        .unwrap_or(DEFAULT_CONTEXT_LINES)
        .min(MAX_CONTEXT_LINES);
    // `clamp(1, HARD_MAX_RESULTS)`: siehe `glob.rs` — gleiche Grenzen.
    let cap = args
        .max_matches
        .unwrap_or(DEFAULT_MAX_MATCHES)
        .clamp(1, HARD_MAX_RESULTS);

    let mut run = GrepRun {
        regex: &regex,
        file_matcher: file_matcher.as_ref(),
        context_lines,
        cap,
        hits: Vec::new(),
        output_bytes: 0,
        skipped: 0,
    };
    let walked = walk_tree(
        &workspace,
        &start_rel,
        WalkOptions::standard(true, is_hard_excluded),
        |entry| {
            if entry.entry_type != EntryType::File {
                return ControlFlow::Continue(());
            }
            if let Some(matcher) = run.file_matcher {
                if !matcher.is_match(entry.rel) {
                    return ControlFlow::Continue(());
                }
            }
            if entry.len > MAX_FILE_SIZE_BYTES {
                run.skipped += 1;
                return ControlFlow::Continue(());
            }
            match open_file_in(entry.dir, entry.name) {
                Ok(file) => run.scan(&file, entry.rel),
                Err(_) => ControlFlow::Continue(()),
            }
        },
    );
    let stop = match walked {
        Ok(stop) => stop,
        Err(err) => {
            return ToolOutput::error(format!(
                "fs.grep: '{start_input}' ist kein lesbares Verzeichnis \
                 (Symlinks werden nicht verfolgt): {err}"
            ));
        }
    };

    let mut out_lines = format_hits(&run.hits);
    let mut note_parts = Vec::new();
    if stop == Some(StopReason::ResultLimit) {
        note_parts.push(format!("Ergebnis auf {cap} Treffer begrenzt"));
    }
    if run.skipped > 0 {
        note_parts.push(format!(
            "{} Datei(en) übersprungen (Binärdatei oder Größenlimit überschritten)",
            run.skipped
        ));
    }
    if !note_parts.is_empty() {
        out_lines.push(format!("# Hinweis: {}.", note_parts.join("; ")));
    }
    if let Some(reason) = stop {
        out_lines.push(format!("# stopped: {}", reason.as_str()));
    }

    ToolOutput::text(out_lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, SECRET, render};
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

    #[tokio::test]
    async fn test_fs_grep_does_not_follow_symlinks() {
        let fixture = Fixture::new();
        fixture.plant_escapes();
        fs::write(fixture.ws.join("nested/own.txt"), "TOPSECRET-lookalike\n").unwrap();
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace]);

        let output = fs_grep(&ctx, grep_args(SECRET)).await.unwrap();
        let text = text_of(output);
        assert_eq!(text.lines().count(), 1, "nur die eigene Datei darf treffen: {text}");
        assert!(text.starts_with("nested/own.txt:1:"), "{text}");

        for path in ["link_dir", "loop", "nested/up", "../outside"] {
            let mut args = grep_args(SECRET);
            args.path = Some(path.to_owned());
            let output = fs_grep(&ctx, args).await.unwrap();
            assert!(matches!(output, ToolOutput::Error { .. }), "{path}: {output:?}");
            assert!(!render(&output).contains("secret.txt"));
        }
    }

    #[tokio::test]
    async fn test_fs_grep_skips_oversized_files_and_caps_output() {
        let fixture = Fixture::new();
        let mut big = fs::File::create(fixture.ws.join("big.log")).unwrap();
        std::io::Write::write_all(&mut big, b"MATCHME\n").unwrap();
        big.set_len(MAX_FILE_SIZE_BYTES + 1).unwrap();
        let wide = format!("MATCHME {}\n", "x".repeat(5_000));
        fs::write(fixture.ws.join("wide.txt"), wide.repeat(300)).unwrap();
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace]);

        let mut args = grep_args("MATCHME");
        args.max_matches = Some(usize::MAX);
        let text = text_of(fs_grep(&ctx, args).await.unwrap());
        assert!(text.contains("# stopped: output_limit"), "{}", &text[..text.len().min(400)]);
        assert!(text.contains("übersprungen"), "Größenlimit muss gemeldet werden");
        assert!(!text.contains("big.log:"), "übergroße Datei darf nicht durchsucht werden");
        for line in text.lines().filter(|line| !line.starts_with('#')) {
            let max_line = crate::tree::MAX_LINE_BYTES + 32;
            assert!(line.len() <= max_line, "Zeile zu lang: {}", line.len());
        }
        assert!(text.len() <= MAX_OUTPUT_BYTES + 4096, "Ausgabe zu groß: {}", text.len());
    }

    #[tokio::test]
    async fn test_fs_grep_caps_matches_at_hard_limit() {
        let fixture = Fixture::new();
        let lines: String = (0..(HARD_MAX_RESULTS + 20)).map(|i| format!("m{i}\n")).collect();
        fs::write(fixture.ws.join("many.txt"), lines).unwrap();
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace]);
        let mut args = grep_args("^m");
        args.max_matches = Some(usize::MAX);
        let text = text_of(fs_grep(&ctx, args).await.unwrap());
        let hits = text
            .lines()
            .filter(|line| !line.starts_with('#') && line.contains(':'))
            .count();
        assert_eq!(hits, HARD_MAX_RESULTS);
        assert!(text.contains("# stopped: result_limit"));
    }
}
