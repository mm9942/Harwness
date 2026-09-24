//! Vollständiger Export (Runde 6, Teil C).
//!
//! # Verantwortungsbereich
//! `ChatApp::push_lines` zeigt vorgerenderte Zeilen nur an; `/export` liest
//! dagegen ausschließlich `export_entries`. Alles, was bisher nur über
//! `push_lines` erschien (Befehlsausgaben, Bus-Systemzeilen, Busy-Ergebnisse,
//! Anhang-Hinweise, `/tools`), fehlte deshalb im Export. Diese Datei bündelt:
//! - [`ChatApp::push_system_lines_exported`] und Varianten: Anzeige und
//!   `ExportEntry::System` in einem Schritt;
//! - [`export_shell_result`]: ein eigener Eintrag für `!`-Befehle (Befehl,
//!   Exit-Code, gekappte Ausgabe);
//! - [`export_agent_event`]: Hintergrund-/Kind-Ereignisse als
//!   `ExportEntry::Agent`;
//! - Zielpfade für `/export --datei` und die Dateiauswahl
//!   ([`export_target_path`], [`default_export_target`]): `~`/`$HOME`
//!   expandiert, Dateiendung nach Format.
//!
//! Bewusst **nicht** exportiert bleiben die `/btw`-Zellen (flüchtig, siehe
//! `app/btw.rs`) und die Kind-Live-Blöcke (`child_stream_glue`).
//!
//! # Nebenläufigkeit
//! Läuft auf dem Thread der Ereignisschleife; kein eigener Zustand.

use std::path::{Path, PathBuf};

use ratatui::text::Line;

use super::{ChatApp, ExportOutputFormat};
use crate::export::{self, ExportAgentEntry, ExportEntry};

/// Obergrenze der exportierten `!`-Ausgabe in Zeichen (wie die Kappung des
/// Folge-Turns).
pub(crate) const SHELL_EXPORT_MAX_CHARS: usize = 8000;

impl ExportOutputFormat {
    /// Dateiendung des Formats (ohne Punkt).
    pub(super) fn file_extension(self) -> &'static str {
        match self {
            Self::Markdown => "md",
            Self::Json => "json",
        }
    }
}

impl ChatApp {
    /// Hängt vorgerenderte Systemzeilen an und schreibt `export_text` als
    /// `ExportEntry::System` in den Export.
    ///
    /// # Argumente
    /// - `lines` (`Vec<Line<'static>>`): die Anzeige (unverändert wie
    ///   [`ChatApp::push_lines`]).
    /// - `export_text` (`impl Into<String>`): der Text für den Export; ein
    ///   leerer Text erzeugt keinen Eintrag.
    pub(crate) fn push_system_lines_exported(
        &mut self,
        lines: Vec<Line<'static>>,
        export_text: impl Into<String>,
    ) {
        let text = export_text.into();
        if !text.trim().is_empty() {
            self.export_entries.push(ExportEntry::System(text));
        }
        self.push_lines(lines);
    }

    /// Wie [`Self::push_system_lines_exported`], der Exporttext ist der
    /// Klartext der Zeilen.
    pub(crate) fn push_lines_exported(&mut self, lines: Vec<Line<'static>>) {
        let text = lines_plain_text(&lines);
        self.push_system_lines_exported(lines, text);
    }

    /// Zeigt einen mehrzeiligen Text (an `\n` getrennt) und exportiert ihn
    /// als einen `ExportEntry::System`.
    pub(crate) fn push_system_text_exported(&mut self, text: &str) {
        self.push_system_lines_exported(split_lines(text), text);
    }
}

/// Teilt einen Text an `\n` in Anzeigezeilen.
pub(crate) fn split_lines(text: &str) -> Vec<Line<'static>> {
    text.split('\n')
        .map(|line| Line::from(line.to_owned()))
        .collect()
}

/// Klartext vorgerenderter Zeilen (Spans aneinander, Zeilen mit `\n`).
pub(crate) fn lines_plain_text(lines: &[Line<'_>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Exporttext einer `/befehl`-Ausgabe: die Befehlszeile, dann die Ausgabe.
///
/// # Argumente
/// - `raw` (`&str`): die eingegebene Befehlszeile.
/// - `output` (`&str`): die angezeigte Ausgabe.
pub(crate) fn command_export_text(raw: &str, output: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        output.to_owned()
    } else if output.trim().is_empty() {
        format!("› {raw}")
    } else {
        format!("› {raw}\n{output}")
    }
}

/// Exporttext eines `!`-Befehls: Befehl, Exit-Code und gekappte Ausgabe in
/// einem Codeblock.
///
/// # Argumente
/// - `command` (`&str`): der Befehl ohne führendes `!`.
/// - `exit_code` (`Option<i64>`): Exit-Code, sofern bekannt.
/// - `output` (`&str`): `stdout`/`stderr` zusammengeführt.
pub(crate) fn shell_export_text(command: &str, exit_code: Option<i64>, output: &str) -> String {
    let exit = exit_code.map_or_else(|| "unbekannt".to_owned(), |code| code.to_string());
    let (body, cut) = cap_chars(output, SHELL_EXPORT_MAX_CHARS);
    let body = body.trim_end_matches('\n');
    let fence = code_fence_for(body);
    let mut text = format!("! {command} (Exit {exit})");
    if !body.is_empty() {
        text.push_str(&format!("\n{fence}\n{body}\n{fence}"));
    }
    if cut {
        text.push_str(&format!(
            "\n… Ausgabe nach {SHELL_EXPORT_MAX_CHARS} Zeichen gekappt"
        ));
    }
    text
}

/// Schreibt das Ergebnis eines `!`-Befehls als eigenen Exporteintrag.
///
/// # Beschreibung
/// Nur der Export; die Anzeige macht der Aufrufer (`!`-Pfad). Genau einmal
/// je ausgeführtem Befehl aufrufen.
pub(crate) fn export_shell_result(
    app: &mut ChatApp,
    command: &str,
    exit_code: Option<i64>,
    output: &str,
) {
    app.export_entries
        .push(ExportEntry::System(shell_export_text(
            command, exit_code, output,
        )));
}

/// Schreibt ein Agenten-Ereignis (Hintergrund-Ende, `parent.message`) als
/// `ExportEntry::Agent`.
pub(crate) fn export_agent_event(app: &mut ChatApp, entry: ExportAgentEntry) {
    app.export_entries.push(ExportEntry::Agent(entry));
}

/// Kappt `text` auf höchstens `max` Zeichen.
///
/// # Rückgabe
/// Der (ggf. gekürzte) Text und ob gekappt wurde.
fn cap_chars(text: &str, max: usize) -> (&str, bool) {
    match text.char_indices().nth(max) {
        Some((cut, _)) => (&text[..cut], true),
        None => (text, false),
    }
}

/// Ein Code-Zaun, der länger ist als jede Backtick-Folge im Text.
fn code_fence_for(body: &str) -> String {
    let mut longest = 0usize;
    let mut run = 0usize;
    for ch in body.chars() {
        if ch == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat(longest.max(2) + 1)
}

/// Das Home-Verzeichnis aus `$HOME` (leer gilt als fehlend).
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// Zielpfad für `/export --datei <pfad>`.
///
/// # Beschreibung
/// `~` und `$HOME` am Anfang werden expandiert
/// ([`export::expand_home_prefix`]). Endet der Pfad auf `/` oder ist er ein
/// vorhandenes Verzeichnis, landet der Export dort unter dem Standardnamen
/// mit der Endung des Formats. Die Traversal-Prüfung macht weiterhin
/// [`export::write_export_path`].
pub(super) fn export_target_path(raw: &str, format: ExportOutputFormat) -> PathBuf {
    export_target_path_with(
        raw,
        format,
        home_dir().as_deref(),
        &super::export_timestamp_now(),
    )
}

/// Reine Form von [`export_target_path`] (für Tests).
pub(super) fn export_target_path_with(
    raw: &str,
    format: ExportOutputFormat,
    home: Option<&Path>,
    now: &str,
) -> PathBuf {
    let expanded = export::expand_home_prefix(raw, home);
    if raw.ends_with('/') || expanded.is_dir() {
        export::default_export_path_with_extension(now, &expanded, format.file_extension())
    } else {
        expanded
    }
}

/// Standard-Zielpfad der Dateiauswahl im aktuellen Arbeitsverzeichnis, mit
/// der Endung des Formats.
pub(super) fn default_export_target(format: ExportOutputFormat) -> PathBuf {
    let now = super::export_timestamp_now();
    let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    export::default_export_path_with_extension(&now, &dir, format.file_extension())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::super::tests::test_chat_app;
    use super::super::{ExportOutputFormat, build_export};
    use super::*;
    use crate::export::ExportOptions;
    use crate::test_support::{TestResult, ctx};

    fn markdown(app: &ChatApp) -> String {
        build_export(app, &ExportOptions::default(), ExportOutputFormat::Markdown)
    }

    /// Befehlsausgabe und Systemzeile landen im Export, die Anzeige bleibt
    /// unverändert eine Zelle.
    #[test]
    fn command_output_and_system_lines_are_exported() -> TestResult {
        let mut app = test_chat_app()?;
        let cells = app.cells_len();
        app.push_system_lines_exported(
            split_lines("Modus: auto\nZweite Zeile"),
            command_export_text("/mode auto", "Modus: auto\nZweite Zeile"),
        );
        app.push_system_text_exported("Bus-Systemzeile");
        app.push_lines_exported(vec![Line::from("Busy-Ergebnis (während Turn)")]);
        assert_eq!(app.cells_len(), cells + 3);

        let out = markdown(&app);
        assert!(out.contains("› /mode auto"), "{out}");
        assert!(out.contains("Zweite Zeile"), "{out}");
        assert!(out.contains("Bus-Systemzeile"), "{out}");
        assert!(out.contains("Busy-Ergebnis (während Turn)"), "{out}");
        Ok(())
    }

    /// Zwei Exporte nacheinander unterscheiden sich um die neuen Einträge.
    #[test]
    fn two_exports_differ_by_the_new_entries() -> TestResult {
        let mut app = test_chat_app()?;
        app.push_system_text_exported("Erste Ausgabe");
        let first = markdown(&app);
        app.push_system_lines_exported(
            split_lines("Export angefordert"),
            command_export_text("/export", "Export angefordert"),
        );
        export_shell_result(&mut app, "ls", Some(0), "a.txt\n");
        let second = markdown(&app);

        assert_ne!(first, second);
        assert!(!first.contains("Export angefordert"));
        assert!(second.contains("Export angefordert"), "{second}");
        assert!(second.contains("! ls (Exit 0)"), "{second}");
        assert!(second.contains("Erste Ausgabe"));
        Ok(())
    }

    /// `!`-Ergebnis: Befehl, Exit-Code, Ausgabe; lange Ausgaben gekappt.
    #[test]
    fn shell_results_carry_command_exit_and_capped_output() -> TestResult {
        let text = shell_export_text("cat x", Some(2), "zeile ``` mit zaun\n");
        assert!(text.starts_with("! cat x (Exit 2)\n````\n"), "{text}");
        assert!(text.contains("zeile ``` mit zaun"));

        let long = "x".repeat(SHELL_EXPORT_MAX_CHARS + 10);
        let capped = shell_export_text("yes", None, &long);
        assert!(capped.contains("(Exit unbekannt)"));
        assert!(capped.contains("gekappt"));
        assert!(!capped.contains(&long));

        let mut app = test_chat_app()?;
        export_shell_result(&mut app, "echo hi", Some(0), "hi\n");
        let out = markdown(&app);
        assert!(out.contains("! echo hi (Exit 0)"), "{out}");
        assert!(out.contains("hi"), "{out}");
        Ok(())
    }

    /// Leerer Exporttext erzeugt keinen Eintrag.
    #[test]
    fn empty_export_text_adds_no_entry() -> TestResult {
        let mut app = test_chat_app()?;
        let before = app.export_entries.len();
        app.push_system_lines_exported(vec![Line::from("")], "  ");
        assert_eq!(app.export_entries.len(), before);
        Ok(())
    }

    /// `~` und `$HOME` werden expandiert, die JSON-Endung stimmt.
    #[test]
    fn tilde_and_home_expand_and_json_gets_its_extension() -> TestResult {
        let home = Path::new("/home/user");
        assert_eq!(
            export_target_path_with("~/x.md", ExportOutputFormat::Markdown, Some(home), "1"),
            PathBuf::from("/home/user/x.md")
        );
        assert_eq!(
            export_target_path_with("$HOME/d/x.json", ExportOutputFormat::Json, Some(home), "1"),
            PathBuf::from("/home/user/d/x.json")
        );
        assert_eq!(
            export_target_path_with("~/exporte/", ExportOutputFormat::Json, Some(home), "42"),
            PathBuf::from("/home/user/exporte/harw-export-42.json")
        );
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let raw = dir.path().to_string_lossy().into_owned();
        assert_eq!(
            export_target_path_with(&raw, ExportOutputFormat::Json, Some(home), "7"),
            dir.path().join("harw-export-7.json")
        );
        assert_eq!(ExportOutputFormat::Markdown.file_extension(), "md");
        Ok(())
    }
}
