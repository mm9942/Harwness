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

/// Platzhalter, den `AgentSession::announce_configured` ohne gesetztes
/// Sitzungsmodell meldet — kein Modellname.
const UNRESOLVED_MODEL_PLACEHOLDER: &str = "default";

/// Modellangabe für den Exportkopf: `<provider>/<modell-id>` des Modells,
/// das die Wurzel (UIA) tatsächlich anspricht.
///
/// # Beschreibung
/// Früher stand dort `anthropic/default`: der Vorgabe-Provider plus der
/// Platzhalter `default` einer Sitzung ohne eigenes Modell — obwohl die UIA
/// über `uia_provider`/`uia_model` ein anderes Modell nutzte. Reihenfolge:
/// 1. ein live gewähltes Sitzungsmodell (`active_model`, mit
///    `active_provider`),
/// 2. die UIA-Auswahl des Controllers,
/// 3. die UIA-Auswahl der Konfiguration (je Achse mit `default_*` als
///    Rückfall, wie die Begrüßung),
/// 4. das von der Sitzung gemeldete Modell (ohne den Platzhalter).
///
/// Ein Alias aus `models/*.toml` wird zur kanonischen Modell-ID aufgelöst;
/// fehlt der Provider, kommt er aus dem Modelleintrag.
pub(super) fn export_model_label(
    snapshot: &harw_operations::session_control::SessionControlSnapshot,
    config: Option<&harw_config::ResolvedConfig>,
    session_model: Option<&str>,
) -> Option<String> {
    let usable = |model: Option<&str>| {
        model
            .map(str::trim)
            .filter(|model| !model.is_empty() && *model != UNRESOLVED_MODEL_PLACEHOLDER)
            .map(str::to_owned)
    };
    let config_selection = config.map(|config| {
        harw_operations::session_control::UiaSelection::from_config(
            config.harness.uia_provider.as_deref(),
            config.harness.uia_model.as_deref(),
            config.harness.default_provider.as_deref(),
            config.harness.default_model.as_deref(),
        )
    });
    let candidates = [
        (
            snapshot.active_provider.clone(),
            usable(snapshot.active_model.as_deref()),
        ),
        (
            snapshot.uia_selection.provider.clone(),
            usable(snapshot.uia_selection.model()),
        ),
        (
            config_selection
                .as_ref()
                .and_then(|selection| selection.provider.clone()),
            usable(
                config_selection
                    .as_ref()
                    .and_then(|selection| selection.model()),
            ),
        ),
        (None, usable(session_model)),
    ];
    let (provider, model) = candidates
        .into_iter()
        .find(|(_, model)| model.is_some())
        .unwrap_or((None, None));
    let Some(model) = model else {
        return provider;
    };
    // Alias → kanonische ID (bei bekanntem Provider nur dessen Modelle).
    let resolved = config.and_then(|config| {
        config.models.values().find(|entry| {
            provider
                .as_deref()
                .is_none_or(|provider| entry.provider == provider)
                && (entry.id == model || entry.aliases.contains(&model.to_owned()))
        })
    });
    let (provider, model) = match resolved {
        Some(entry) => (
            provider.or_else(|| Some(entry.provider.clone())),
            entry.id.clone(),
        ),
        None => (provider, model),
    };
    Some(match provider {
        Some(provider) => format!("{provider}/{model}"),
        None => model,
    })
}

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

    fn model_config(uia: Option<(&str, &str)>) -> TestResult<harw_config::ResolvedConfig> {
        let mut config = harw_config::ResolvedConfig::default();
        config.harness.default_provider = Some("anthropic".to_owned());
        config.harness.default_model = Some("sonnet".to_owned());
        if let Some((provider, model)) = uia {
            config.harness.uia_provider = Some(provider.to_owned());
            config.harness.uia_model = Some(model.to_owned());
        }
        let entry: harw_config::ModelToml = serde_json::from_value(serde_json::json!({
            "id": "claude-sonnet-5",
            "provider": "anthropic",
            "aliases": ["sonnet"],
        }))
        .map_err(ctx("model entry"))?;
        config.models.insert("claude-sonnet-5".to_owned(), entry);
        Ok(config)
    }

    /// Export 429: der Kopf zeigte `anthropic/default` (Vorgabe-Provider plus
    /// Sitzungs-Platzhalter), obwohl die UIA ein anderes Modell nutzte.
    #[test]
    fn export_model_label_prefers_the_uia_selection_over_the_placeholder() -> TestResult {
        let snapshot = harw_operations::session_control::SessionControlSnapshot::empty();
        let config = model_config(Some(("openai", "gpt-6-sol")))?;
        assert_eq!(
            export_model_label(&snapshot, Some(&config), Some("default")).as_deref(),
            Some("openai/gpt-6-sol")
        );
        Ok(())
    }

    /// Aliasse werden zur kanonischen Modell-ID aufgelöst; der Platzhalter
    /// `default` zählt nie als Modell.
    #[test]
    fn export_model_label_resolves_aliases_and_ignores_the_placeholder() -> TestResult {
        let snapshot = harw_operations::session_control::SessionControlSnapshot::empty();
        let config = model_config(None)?;
        assert_eq!(
            export_model_label(&snapshot, Some(&config), Some("default")).as_deref(),
            Some("anthropic/claude-sonnet-5")
        );
        assert_eq!(
            export_model_label(&snapshot, None, Some("default")).as_deref(),
            None
        );
        let mut live = harw_operations::session_control::SessionControlSnapshot::empty();
        live.active_model = Some("sonnet".to_owned());
        assert_eq!(
            export_model_label(&live, Some(&config), None).as_deref(),
            Some("anthropic/claude-sonnet-5")
        );
        Ok(())
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
