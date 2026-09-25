//! `fs.grep` — Tool-Executor für Regex-basierte Dateisuche mit Kontextzeilen.
//!
//! Spec-Referenz: AP W2-01..03, Abschnitt "2. `grep.rs` — `fs.grep`";
//! Sicherheits- und Grenzenüberarbeitung: W1-02 (F-059, F-118); Datei-Ziele
//! und ripgrep-Integration: Nutzerentscheidung "ripgrep bevorzugt, interner
//! Fallback" (siehe unten).
//!
//! # Verantwortung
//! Dieses Modul besitzt den `fs.grep`-Executor:
//! - [`GrepArgs`]: deserialisierte Aufrufargumente, per `#[derive(harw_macros::Tool)]`
//!   auch Quelle der JSON-Schema-Spezifikation.
//! - `fs_grep` (per `#[harw_macros::tool]` zu [`FsGrepTool`] erweitert): durchsucht
//!   eine einzelne Datei oder alle Dateien unterhalb eines Start-Verzeichnisses
//!   zeilenweise mit einem `regex`-Muster. Ausgabe im GNU-grep-Stil
//!   (`pfad:zeile: inhalt` für Treffer, `pfad-zeile- inhalt` für Kontext).
//!
//! # Ziel-Validierung (Datei oder Verzeichnis)
//! `args.path` bezeichnet entweder eine einzelne Datei oder ein
//! Unterverzeichnis. Bevor irgendein Suchmotor läuft, löst
//! `crate::symlink::open_start` Symlinks im Pfad auf (nach innen frei, nach
//! außen nur mit Freigabe — dann ist das freigegebene Verzeichnis die
//! Wurzel), und [`resolve_target`] öffnet den aufgelösten Pfad symlinkfrei
//! über `Workspace::open_any` und entscheidet anhand der Metadaten zwischen
//! Datei- und Verzeichnis-Modus; jeder andere Typ (Socket, FIFO, …) mündet in
//! denselben Fehler. Der Startpfad liegt damit immer unter einer erlaubten
//! Wurzel — unabhängig vom Suchmotor.
//!
//! # ripgrep-Integration
//! Ist ein `rg`-Binary in `PATH` auffindbar (einmal je Prozess ermittelt,
//! siehe [`rg_binary`]; keine `which`-Crate — reine `PATH`-Suche über
//! `std::env::split_paths`) und wurden keine Kontextzeilen angefordert, läuft
//! `rg --json` synchron im bereits blockierenden Kontext (siehe
//! [`crate::blocking::run_blocking`]) mit `current_dir` = Workspace-Wurzel
//! und entferntem `RIPGREP_CONFIG_PATH`. Die `match`-Ereignisse der
//! `--json`-Ausgabe werden zeilenweise geparst (siehe
//! [`parse_rg_match_line`]) und in dieselbe Trefferstruktur überführt wie die
//! interne Suche — identisches Ausgabeformat, identische Zeilenkürzung,
//! identische Treffer-/Ausgabegrenzen. Der Kindprozess wird beim Erreichen
//! einer Grenze vorzeitig beendet. Ein Regex-Fehler von `rg` (Exit-Code 2)
//! ohne bereits gesammelte Treffer sowie jeder Spawn-Fehler fallen auf die
//! interne Suche zurück; Exit-Code 1 (keine Treffer) liefert ein leeres
//! Ergebnis. `rg` überspringt standardmäßig `.gitignore`-Einträge und
//! versteckte Dateien — das deckt sich mit dem internen Fallback (der
//! ebenfalls `.gitignore` beachtet) und ist akzeptiertes Verhalten.
//! Kontextzeilen (`context_lines > 0`) werden ausschließlich intern bedient,
//! da die verwendete `rg`-Argumentliste keine `-A`/`-B`/`-C`-Flags enthält.
//! In Testbuilds liefert [`rg_binary`] stets `None`, damit Tests unabhängig
//! vom Testrechner deterministisch bleiben; die ripgrep-Anbindung wird
//! stattdessen gezielt über [`grep_with_engine`] mit einem injizierten Pfad
//! getestet.
//!
//! # Traversierung und Grenzen (W1-02, interne Suche)
//! Im Verzeichnis-Modus wird über [`crate::tree::walk_tree`] gewalkt (Basis
//! `harw_fsutil::walk_beneath`): Symlinks werden nie gefolgt, Dateien nur über
//! `open_beneath` relativ zum Eltern-Deskriptor geöffnet. `.gitignore`/
//! `.ignore` gelten weiterhin (nur innerhalb des Workspace), `target/` und
//! `.git/` sind harte Ausschlüsse. Das optionale `glob` wird gegen den Pfad
//! **relativ zur Workspace-Wurzel** geprüft und gilt nur im Verzeichnis-Modus
//! (im Datei-Modus gibt es nichts zu filtern). Grenzen: höchstens 1000
//! Treffer, Tiefe 32, 50 000 Einträge, 10 s, Dateien über
//! [`MAX_FILE_SIZE_BYTES`] werden übersprungen, Zeilen auf 1024 Bytes und die
//! Gesamtausgabe auf 64 KiB gekürzt. Der Abbruchgrund erscheint als
//! `# stopped: <grund>`.
//!
//! # Schlüsseltypen
//! - [`GrepArgs`]
//! - [`FsGrepTool`] (generiert durch `#[harw_macros::tool]`)
//!
//! # Nebenläufigkeit
//! [`FsGrepTool`] ist `Send + Sync` (Unit-Struct). `fs.grep` ist `parallel_safe`.
//! Walk bzw. rg-Subprozess laufen über `spawn_blocking`.
//!
//! # Fehler
//! Permission-Fehler (erzwungen durch den Makro-Prolog vor der
//! Deserialisierung), ungültige Regex- oder Glob-Muster und
//! Pfadauflösungsfehler münden alle in `Ok(ToolOutput::error(...))`. Kein Panic.

use crate::blocking::run_blocking;
use crate::symlink::open_start;
use crate::tree::{
    HARD_MAX_RESULTS, MAX_LINE_BYTES, MAX_OUTPUT_BYTES, MAX_SCAN_FILE_BYTES, StopReason,
    WalkOptions, Workspace, normalize_relative, open_file_in, read_bounded, truncate_line,
    walk_tree,
};
use globset::{GlobBuilder, GlobMatcher};
use harw_fsutil::EntryType;
use harw_macros::Tool;
use harw_tools::{ToolExecutionContext, ToolOutput, ToolsError};
use regex::{Regex, RegexBuilder};
use serde::Deserialize;
use std::ffi::OsStr;
use std::io::{BufRead, BufReader};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
#[cfg(not(test))]
use std::sync::OnceLock;

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
    description = "Durchsucht eine Datei oder Dateien im Workspace mit einem regulären \
                    Ausdruck; bevorzugt ripgrep, sonst interne Suche."
)]
pub struct GrepArgs {
    /// Rust-Regex (crate `regex`).
    pub pattern: String,
    /// Optionale Datei oder Unterverzeichnis (relativ zur Workspace-Wurzel).
    pub path: Option<String>,
    /// Optionales Glob-Muster zur Dateiauswahl, relativ zur Workspace-Wurzel
    /// (Default: alle Textdateien).
    pub glob: Option<String>,
    /// Kontextzeilen vor und nach dem Treffer (Default 0, Maximum 10).
    #[tool(default = 0)]
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
    pub context_lines: Option<usize>,
    /// Obergrenze der Treffer (Default 100, Maximum 1000).
    #[tool(default = 100)]
    #[serde(
        default,
        deserialize_with = "harw_extension_api::lenient::lenient_opt_usize"
    )]
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
    description = "Durchsucht eine Datei oder alle Dateien im Workspace mit einem regulären \
                    Ausdruck und liefert Treffer im GNU-grep-Stil. Bevorzugt ripgrep, wenn \
                    installiert (dann wird .gitignore beachtet); sonst interne symlinkfeste \
                    Suche.",
    permission = "read_workspace",
    parallel_safe
)]
async fn fs_grep(context: &ToolExecutionContext, args: GrepArgs) -> Result<ToolOutput, ToolsError> {
    let root = context.sandbox().workspace().canonical_root().to_path_buf();
    run_blocking("fs.grep", move || Ok(grep_blocking(&root, &args))).await
}

/// Synchroner Kern von [`fs_grep`]: ermittelt den prozessweiten `rg`-Pfad
/// (siehe [`rg_binary`]) und delegiert an [`grep_with_engine`].
fn grep_blocking(root: &Path, args: &GrepArgs) -> ToolOutput {
    grep_with_engine(root, args, rg_binary())
}

/// Aufgelöstes Suchziel (Abschnitt "Ziel-Validierung"): eine einzelne,
/// bereits symlinkfrei geöffnete Datei oder ein bestätigtes Verzeichnis.
enum Target {
    /// Einzelne reguläre Datei; wird direkt gescannt (kein Glob-Filter).
    File(std::fs::File),
    /// Verzeichnis; wird vom Walk erneut relativ zur Wurzel geöffnet.
    Dir,
}

/// Löst und validiert `start_rel` **vor** jedem Suchmotor (Abschnitt
/// "Ziel-Validierung"): öffnet den Pfad symlinkfrei unterhalb der
/// Workspace-Wurzel und unterscheidet Datei- von Verzeichnis-Modus.
///
/// # Errors
/// `Err(ToolOutput::error(...))` für Symlink-Glieder im Pfad, fehlende Pfade
/// und jeden anderen Eintragstyp (Socket, FIFO, …).
fn resolve_target(
    workspace: &Workspace,
    start_input: &str,
    start_rel: &Path,
) -> Result<Target, ToolOutput> {
    let opened = workspace
        .open_any(start_rel)
        .and_then(|file| file.metadata().map(|meta| (file, meta)));
    match opened {
        Ok((file, meta)) if meta.is_file() => Ok(Target::File(file)),
        Ok((_, meta)) if meta.is_dir() => Ok(Target::Dir),
        Ok(_) => Err(ToolOutput::error(format!(
            "fs.grep: '{start_input}' ist weder eine lesbare Datei noch ein lesbares \
             Verzeichnis"
        ))),
        Err(err) => Err(ToolOutput::error(format!(
            "fs.grep: '{start_input}' ist weder eine lesbare Datei noch ein lesbares \
             Verzeichnis: {err}"
        ))),
    }
}

/// Formatiert die gesammelten Treffer und hängt Hinweis-/Abbruchzeilen an
/// (gemeinsame Ausgabeformatierung für beide Suchmotoren).
fn finalize(hits: &[GrepHit], stop: Option<StopReason>, cap: usize, skipped: usize) -> ToolOutput {
    let mut out_lines = format_hits(hits);
    let mut note_parts = Vec::new();
    if stop == Some(StopReason::ResultLimit) {
        note_parts.push(format!("Ergebnis auf {cap} Treffer begrenzt"));
    }
    if skipped > 0 {
        note_parts.push(format!(
            "{skipped} Datei(en) übersprungen (Binärdatei oder Größenlimit überschritten)"
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

/// Prozessweit gecachter Pfad zum `rg`-Binary (`None`, wenn nicht gefunden).
/// Nur in Nicht-Testbuilds referenziert (siehe [`rg_binary`]).
#[cfg(not(test))]
static RG_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Sucht `rg` in `PATH` (manuelle Suche über [`std::env::split_paths`], keine
/// `which`-Crate — siehe Workspace-Regel gegen neue Abhängigkeiten).
fn find_rg() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var).find_map(|dir| {
        let candidate = dir.join("rg");
        candidate.is_file().then_some(candidate)
    })
}

/// Liefert den gecachten `rg`-Pfad, falls vorhanden (einmal je Prozess
/// ermittelt). In Testbuilds wird `rg` nie automatisch verwendet, damit Tests
/// unabhängig vom Testrechner deterministisch bleiben; die ripgrep-Anbindung
/// wird stattdessen gezielt über [`grep_with_engine`] mit einem injizierten
/// Pfad getestet (siehe Modul-Doku "ripgrep-Integration").
#[cfg(not(test))]
fn rg_binary() -> Option<&'static Path> {
    RG_PATH.get_or_init(find_rg).as_deref()
}

/// Testvariante von [`rg_binary`]: liefert immer `None`.
#[cfg(test)]
fn rg_binary() -> Option<&'static Path> {
    None
}

/// Ergebnis eines `rg`-Laufs.
enum RgOutcome {
    /// Fertige Trefferliste inklusive Abbruchgrund.
    Hits(Vec<GrepHit>, Option<StopReason>),
    /// `rg` konnte nicht sinnvoll verwendet werden — die interne Suche
    /// übernimmt (Spawn-Fehler oder Regex-Fehler ohne bereits gesammelte
    /// Treffer).
    Fallback,
}

/// Eine aus einem `rg --json`-`match`-Ereignis extrahierte Rohtrefferzeile.
struct RgMatchLine {
    /// Pfad relativ zum `current_dir` des `rg`-Aufrufs (== Workspace-Wurzel).
    path: String,
    /// 1-basierte Zeilennummer.
    line_number: usize,
    /// Zeileninhalt inklusive Zeilenumbruch.
    text: String,
}

/// Eine Zeile aus `rg --json` (`{"type": "...", "data": {...}}`).
#[derive(Deserialize)]
struct RgEvent {
    /// `"begin"`, `"match"`, `"end"` oder `"summary"`.
    #[serde(rename = "type")]
    event_type: String,
    /// Nutzlast; Form hängt von `event_type` ab (siehe [`RgEventData`]).
    data: Option<RgEventData>,
}

/// Die für `match`-Ereignisse relevanten Felder von `data`. Andere
/// Ereignistypen (`begin`, `end`, `summary`) haben ein abweichendes
/// `data`-Objekt; dessen unbekannte Felder werden von `serde_json` ignoriert,
/// die hier fehlenden Schlüssel landen einfach als `None`.
#[derive(Deserialize)]
struct RgEventData {
    /// `{"text": "<pfad>"}`.
    path: Option<RgText>,
    /// `{"text": "<zeileninhalt inkl. Zeilenumbruch>"}`.
    lines: Option<RgText>,
    /// 1-basierte Zeilennummer des Treffers.
    line_number: Option<usize>,
}

/// `{"text": "..."}` — von `rg --json` für Pfad- und Zeilenfelder verwendet.
#[derive(Deserialize)]
struct RgText {
    /// Der UTF-8-Text (fehlt bei nicht-UTF-8-Inhalt, dann Base64 in `bytes`,
    /// was hier nicht ausgewertet wird).
    text: Option<String>,
}

/// Parst eine Zeile aus `rg --json`-Ausgabe. Liefert `None` für
/// Nicht-`match`-Ereignisse (`begin`, `end`, `summary`) und für unparsebare
/// oder unvollständige Zeilen.
fn parse_rg_match_line(line: &str) -> Option<RgMatchLine> {
    let event: RgEvent = serde_json::from_str(line).ok()?;
    if event.event_type != "match" {
        return None;
    }
    let data = event.data?;
    Some(RgMatchLine {
        path: data.path?.text?,
        line_number: data.line_number?,
        text: data.lines?.text?,
    })
}

/// Führt `rg --json` im Verzeichnis `root` aus und übersetzt die
/// `match`-Ereignisse in [`GrepHit`]s (Abschnitt "ripgrep-Integration").
///
/// Bricht beim Erreichen von `cap` Treffern oder [`MAX_OUTPUT_BYTES`] ab und
/// beendet den Kindprozess vorzeitig (`StopReason::ResultLimit` /
/// `OutputLimit`). Exit-Code 2 (z. B. Regex-Fehler) ohne bereits gesammelte
/// Treffer sowie jeder Spawn-Fehler liefern [`RgOutcome::Fallback`];
/// Exit-Code 1 (keine Treffer) liefert eine leere Trefferliste.
fn run_rg(rg: &Path, root: &Path, args: &GrepArgs, start_rel: &Path, cap: usize) -> RgOutcome {
    let target_arg = if start_rel.as_os_str().is_empty() {
        Path::new(".")
    } else {
        start_rel
    };

    let mut cmd = Command::new(rg);
    cmd.current_dir(root)
        .env_remove("RIPGREP_CONFIG_PATH")
        .arg("--json")
        .arg("--no-config")
        .arg("--no-follow")
        .arg("--no-messages")
        .arg("--color")
        .arg("never")
        .arg("--max-columns")
        .arg(MAX_LINE_BYTES.to_string());
    if args.case_insensitive.unwrap_or(false) {
        cmd.arg("--ignore-case");
    }
    if let Some(glob) = args.glob.as_deref().filter(|glob| !glob.is_empty()) {
        cmd.arg("--glob").arg(glob);
    }
    cmd.arg("-e")
        .arg(&args.pattern)
        .arg("--")
        .arg(target_arg)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(_) => return RgOutcome::Fallback,
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return RgOutcome::Fallback;
    };

    let mut hits: Vec<GrepHit> = Vec::new();
    let mut output_bytes = 0usize;
    let mut stop: Option<StopReason> = None;

    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        let Some(matched) = parse_rg_match_line(&line) else {
            continue;
        };
        if hits.len() >= cap {
            stop = Some(StopReason::ResultLimit);
            break;
        }
        let hit = GrepHit {
            path: matched.path,
            line: matched.line_number,
            before: Vec::new(),
            text: truncate_line(matched.text.trim_end_matches(['\n', '\r'])),
            after: Vec::new(),
        };
        let cost = hit.output_cost();
        if output_bytes + cost > MAX_OUTPUT_BYTES {
            stop = Some(StopReason::OutputLimit);
            break;
        }
        output_bytes += cost;
        hits.push(hit);
    }

    let exit_code = if stop.is_some() {
        let _ = child.kill();
        let _ = child.wait();
        None
    } else {
        child.wait().ok().and_then(|status| status.code())
    };

    if hits.is_empty() && exit_code == Some(2) {
        return RgOutcome::Fallback;
    }
    RgOutcome::Hits(hits, stop)
}

/// Kern von [`grep_blocking`] mit injizierbarem `rg`-Pfad (siehe Modul-Doku
/// "ripgrep-Integration"). `rg` wird nur ohne angeforderte Kontextzeilen
/// versucht; bei `RgOutcome::Fallback` oder `rg.is_none()` übernimmt die
/// interne, symlinkfeste Suche.
///
/// # Errors
/// Liefert nie `Err`; Pfad-, Muster- oder I/O-Fehler werden als
/// `Ok(ToolOutput::error(...))` zurückgegeben.
fn grep_with_engine(root: &Path, args: &GrepArgs, rg: Option<&Path>) -> ToolOutput {
    // Strict-Schema: `null` und `""` gelten als „nicht gesetzt“ (Wurzel).
    let start_input = args
        .path
        .as_deref()
        .filter(|path| !path.is_empty())
        .unwrap_or(".");
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

    let file_matcher = match args.glob.as_deref().filter(|glob| !glob.is_empty()) {
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

    // Symlinks im Startpfad: nach innen frei, nach außen nur mit Freigabe
    // (siehe `crate::symlink`). Interne Suche und ripgrep arbeiten danach auf
    // dem aufgelösten Pfad unter der passenden Wurzel.
    let start = match open_start(root, &start_rel, "fs.grep", start_input) {
        Ok(start) => start,
        Err(err) => {
            return ToolOutput::error(format!(
                "fs.grep: '{start_input}' ist weder eine lesbare Datei noch ein lesbares \
                 Verzeichnis: {err}"
            ));
        }
    };
    let search_root = start.root().to_path_buf();
    let root = search_root.as_path();
    let (workspace, start_rel) = (start.workspace, start.rel);

    let target = match resolve_target(&workspace, start_input, &start_rel) {
        Ok(target) => target,
        Err(output) => return output,
    };

    let context_lines = args
        .context_lines
        .unwrap_or(DEFAULT_CONTEXT_LINES)
        .min(MAX_CONTEXT_LINES);
    // `clamp(1, HARD_MAX_RESULTS)`: siehe `glob.rs` — gleiche Grenzen.
    // `0` gilt wie `null` als „nicht gesetzt“ (Vorgabe), nicht als „ein Treffer“.
    let cap = args
        .max_matches
        .filter(|&matches| matches > 0)
        .unwrap_or(DEFAULT_MAX_MATCHES)
        .clamp(1, HARD_MAX_RESULTS);

    // ripgrep nur ohne Kontextzeilen versuchen: die dokumentierte
    // rg-Argumentliste (Abschnitt "ripgrep-Integration") enthält keine
    // -A/-B/-C-Flags, mit Kontextzeilen läuft ausschließlich die interne Suche.
    if context_lines == 0 {
        if let Some(rg) = rg {
            match run_rg(rg, root, args, &start_rel, cap) {
                RgOutcome::Hits(hits, stop) => return finalize(&hits, stop, cap, 0),
                RgOutcome::Fallback => {}
            }
        }
    }

    let mut run = GrepRun {
        regex: &regex,
        file_matcher: file_matcher.as_ref(),
        context_lines,
        cap,
        hits: Vec::new(),
        output_bytes: 0,
        skipped: 0,
    };

    let stop = match target {
        Target::File(file) => match run.scan(&file, &start_rel) {
            ControlFlow::Continue(()) => None,
            ControlFlow::Break(reason) => Some(reason),
        },
        Target::Dir => {
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
            match walked {
                Ok(stop) => stop,
                Err(err) => {
                    return ToolOutput::error(format!(
                        "fs.grep: '{start_input}' ist kein lesbares Verzeichnis: {err}"
                    ));
                }
            }
        }
    };

    finalize(&run.hits, stop, cap, run.skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{Fixture, SECRET, TestError, TestResult, ctx, render};
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

    fn text_of(output: ToolOutput) -> TestResult<String> {
        match output {
            ToolOutput::Text { content } => Ok(content),
            other => Err(TestError::Unexpected(format!(
                "expected text output, got: {other:?}"
            ))),
        }
    }

    #[tokio::test]
    async fn test_fs_grep_finds_matches_with_context_lines() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;
        fs::write(
            ws.join("file.txt"),
            "alpha\nbeta\nGAMMA_MATCH\ndelta\nepsilon\n",
        )?;

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace])?;
        let ctx = make_ctx(sandbox);
        let mut args = grep_args("GAMMA_MATCH");
        args.context_lines = Some(1);

        let output = fs_grep(&ctx, args).await?;
        let text = text_of(output)?;
        let lines: Vec<&str> = text.lines().collect();

        assert_eq!(
            lines,
            vec![
                "file.txt-2- beta",
                "file.txt:3: GAMMA_MATCH",
                "file.txt-4- delta",
            ]
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_fs_grep_case_insensitive() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;
        fs::write(ws.join("file.txt"), "some GAMMA_MATCH here\n")?;

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace])?;
        let ctx = make_ctx(sandbox);

        let mut insensitive_args = grep_args("gamma_match");
        insensitive_args.case_insensitive = Some(true);
        let output = fs_grep(&ctx, insensitive_args).await?;
        assert!(
            text_of(output)?.contains("GAMMA_MATCH"),
            "case-insensitive search must find the uppercase line"
        );

        let sensitive_args = grep_args("gamma_match");
        let output = fs_grep(&ctx, sensitive_args).await?;
        assert!(
            text_of(output)?.is_empty(),
            "case-sensitive search must not find the uppercase line"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_fs_grep_skips_binary_files() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;
        let mut binary_content = b"MATCHME".to_vec();
        binary_content.push(0u8);
        binary_content.extend_from_slice(b"more MATCHME bytes");
        fs::write(ws.join("binary.bin"), &binary_content)?;
        fs::write(ws.join("plain.txt"), "MATCHME in plain text\n")?;

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace])?;
        let ctx = make_ctx(sandbox);
        let args = grep_args("MATCHME");

        let output = fs_grep(&ctx, args).await?;
        let text = text_of(output)?;

        assert!(text.contains("plain.txt"), "plain text file must match");
        assert!(
            !text.contains("binary.bin"),
            "binary file must be skipped: {text}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_fs_grep_caps_at_max_matches() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;
        let content = (0..10)
            .map(|i| format!("line MATCHME {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(ws.join("many.txt"), content)?;

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace])?;
        let ctx = make_ctx(sandbox);
        let mut args = grep_args("MATCHME");
        args.max_matches = Some(3);

        let output = fs_grep(&ctx, args).await?;
        let text = text_of(output)?;
        let match_line_count = text
            .lines()
            .filter(|l| !l.starts_with('#') && l.contains(':'))
            .count();

        assert_eq!(
            match_line_count, 3,
            "expected exactly 3 match lines: {text}"
        );
        assert!(
            text.contains("# Hinweis") && text.contains("begrenzt"),
            "expected a truncation hint: {text}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_fs_grep_empty_path_glob_and_zero_limit_mean_not_set() -> TestResult {
        // Strict-Schema: Modelle senden für ungenutzte Felder `""` bzw. `0`.
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;
        let content = (0..10)
            .map(|i| format!("line MATCHME {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(ws.join("many.txt"), content)?;

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace])?;
        let ctx = make_ctx(sandbox);
        let mut args = grep_args("MATCHME");
        args.path = Some(String::new());
        args.glob = Some(String::new());
        args.max_matches = Some(0);

        let text = text_of(fs_grep(&ctx, args).await?)?;
        let match_line_count = text
            .lines()
            .filter(|l| !l.starts_with('#') && l.contains(':'))
            .count();
        assert_eq!(match_line_count, 10, "alle Treffer erwartet: {text}");
        Ok(())
    }

    #[tokio::test]
    async fn test_fs_grep_invalid_pattern_returns_error() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![Permission::ReadWorkspace])?;
        let ctx = make_ctx(sandbox);
        let args = grep_args("(unclosed");

        let output = fs_grep(&ctx, args).await?;
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
    async fn test_fs_grep_denied_when_no_read_permission() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;

        let sandbox = make_sandbox_with_permissions(dir.path(), vec![])?;
        let ctx = make_ctx(sandbox);
        let call = harw_tools::ToolCall {
            id: ToolCallId::new(),
            name: harw_tools::spec::ToolName::new("fs.grep"),
            arguments: serde_json::json!({ "pattern": "anything" }),
        };

        let tool = FsGrepTool;
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
    async fn test_fs_grep_does_not_follow_symlinks() -> TestResult {
        let fixture = Fixture::new()?;
        fixture.plant_escapes()?;
        fs::write(fixture.ws.join("nested/own.txt"), "TOPSECRET-lookalike\n")?;
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;

        let output = fs_grep(&ctx, grep_args(SECRET)).await?;
        let text = text_of(output)?;
        assert_eq!(
            text.lines().count(),
            1,
            "nur die eigene Datei darf treffen: {text}"
        );
        assert!(text.starts_with("nested/own.txt:1:"), "{text}");

        for path in ["link_dir", "../outside"] {
            let mut args = grep_args(SECRET);
            args.path = Some(path.to_owned());
            let output = fs_grep(&ctx, args).await?;
            assert!(
                matches!(output, ToolOutput::Error { .. }),
                "{path}: {output:?}"
            );
            assert!(!render(&output)?.contains("secret.txt"));
        }
        // `loop` -> `.` bleibt im Workspace und wird aufgelöst; der Walk
        // folgt trotzdem keinem Symlink nach außen.
        let mut args = grep_args(SECRET);
        args.path = Some("loop".to_owned());
        let text = text_of(fs_grep(&ctx, args).await?)?;
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(text.starts_with("nested/own.txt:1:"), "{text}");
        Ok(())
    }

    #[tokio::test]
    async fn test_fs_grep_skips_oversized_files_and_caps_output() -> TestResult {
        let fixture = Fixture::new()?;
        let mut big = fs::File::create(fixture.ws.join("big.log"))?;
        std::io::Write::write_all(&mut big, b"MATCHME\n")?;
        big.set_len(MAX_FILE_SIZE_BYTES + 1)?;
        let wide = format!("MATCHME {}\n", "x".repeat(5_000));
        fs::write(fixture.ws.join("wide.txt"), wide.repeat(300))?;
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;

        let mut args = grep_args("MATCHME");
        args.max_matches = Some(usize::MAX);
        let text = text_of(fs_grep(&ctx, args).await?)?;
        assert!(
            text.contains("# stopped: output_limit"),
            "{}",
            &text[..text.len().min(400)]
        );
        assert!(
            text.contains("übersprungen"),
            "Größenlimit muss gemeldet werden"
        );
        assert!(
            !text.contains("big.log:"),
            "übergroße Datei darf nicht durchsucht werden"
        );
        for line in text.lines().filter(|line| !line.starts_with('#')) {
            let max_line = crate::tree::MAX_LINE_BYTES + 32;
            assert!(line.len() <= max_line, "Zeile zu lang: {}", line.len());
        }
        assert!(
            text.len() <= MAX_OUTPUT_BYTES + 4096,
            "Ausgabe zu groß: {}",
            text.len()
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_fs_grep_caps_matches_at_hard_limit() -> TestResult {
        let fixture = Fixture::new()?;
        let lines: String = (0..(HARD_MAX_RESULTS + 20))
            .map(|i| format!("m{i}\n"))
            .collect();
        fs::write(fixture.ws.join("many.txt"), lines)?;
        let ctx = fixture.ctx(vec![Permission::ReadWorkspace])?;
        let mut args = grep_args("^m");
        args.max_matches = Some(usize::MAX);
        let text = text_of(fs_grep(&ctx, args).await?)?;
        let hits = text
            .lines()
            .filter(|line| !line.starts_with('#') && line.contains(':'))
            .count();
        assert_eq!(hits, HARD_MAX_RESULTS);
        assert!(text.contains("# stopped: result_limit"));
        Ok(())
    }

    // -- Datei-Ziel und ripgrep-Integration (Abschnitte "Ziel-Validierung",
    // "ripgrep-Integration") -------------------------------------------------

    #[test]
    fn test_grep_with_engine_file_mode_uses_internal_engine_without_rg() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;
        fs::write(ws.join("a.txt"), "one\nMATCHME here\nthree\n")?;
        fs::write(ws.join("b.txt"), "MATCHME should not appear\n")?;

        let mut args = grep_args("MATCHME");
        args.path = Some("a.txt".to_owned());
        let text = text_of(grep_with_engine(&ws, &args, None))?;

        assert_eq!(text, "a.txt:2: MATCHME here");
        Ok(())
    }

    #[test]
    fn test_grep_with_engine_dir_mode_uses_internal_engine_without_rg() -> TestResult {
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(ws.join("sub"))?;
        fs::write(ws.join("sub/a.txt"), "MATCHME\n")?;

        let args = grep_args("MATCHME");
        let text = text_of(grep_with_engine(&ws, &args, None))?;

        assert_eq!(text, "sub/a.txt:1: MATCHME");
        Ok(())
    }

    #[test]
    fn test_parse_rg_match_line_extracts_match_events_only() -> TestResult {
        let match_line = r#"{"type":"match","data":{"path":{"text":"src/lib.rs"},"lines":{"text":"fn matchme() {}\n"},"line_number":42,"absolute_offset":100,"submatches":[{"match":{"text":"matchme"},"start":3,"end":10}]}}"#;
        let begin_line = r#"{"type":"begin","data":{"path":{"text":"src/lib.rs"}}}"#;
        let end_line = r#"{"type":"end","data":{"path":{"text":"src/lib.rs"},"binary_offset":null,"stats":{"elapsed":{"secs":0,"nanos":100,"human":"0.000000s"},"searches":1,"searches_with_match":1,"bytes_searched":50,"bytes_printed":30,"matched_lines":1,"matches":1}}}"#;
        let summary_line = r#"{"data":{"elapsed_total":{"human":"0.000100s","nanos":100000,"secs":0},"stats":{"bytes_printed":30,"bytes_searched":50,"elapsed":{"human":"0.000100s","nanos":100000,"secs":0},"matched_lines":1,"matches":1,"searches":1,"searches_with_match":1}},"type":"summary"}"#;

        let matched = parse_rg_match_line(match_line).ok_or(TestError::Missing("match event"))?;
        assert_eq!(matched.path, "src/lib.rs");
        assert_eq!(matched.line_number, 42);
        assert_eq!(matched.text, "fn matchme() {}\n");

        assert!(
            parse_rg_match_line(begin_line).is_none(),
            "begin event must not parse as match"
        );
        assert!(
            parse_rg_match_line(end_line).is_none(),
            "end event must not parse as match"
        );
        assert!(
            parse_rg_match_line(summary_line).is_none(),
            "summary event must not parse as match"
        );
        assert!(
            parse_rg_match_line("not json").is_none(),
            "garbage line must not parse"
        );
        Ok(())
    }

    #[test]
    #[ignore = "erfordert ein installiertes rg-Binary auf PATH; nicht Teil der Standard-Testsuite"]
    fn test_grep_with_engine_spawns_real_ripgrep_when_present() -> TestResult {
        let Some(rg) = find_rg() else {
            return Ok(());
        };
        let dir = TempDir::new()?;
        let ws = dir.path().join("ws");
        fs::create_dir_all(&ws)?;
        fs::write(ws.join("a.txt"), "one\nMATCHME here\nthree\n")?;

        let args = grep_args("MATCHME");
        let text = text_of(grep_with_engine(&ws, &args, Some(&rg)))?;

        assert!(text.contains("a.txt:2: MATCHME here"), "{text}");
        Ok(())
    }
}
