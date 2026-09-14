//! Markdown-Export einer Chat-Historie für den `/export`-Befehl der TUI.
//!
//! Spec-Quelle: `harw-scopes-contract.md` Slice E1 und
//! `nope-permissions-gibt-es-wild-lobster.md` (UI-Stil der Dialoge).
//!
//! # Verantwortung
//! Dieses Modul rendert eine gegebene Menge von [`ExportEntry`]-Werten als
//! menschenlesbares Markdown-Dokument ([`render_markdown`]) und schreibt es
//! atomar und ohne Überschreiben auf die Platte ([`write_export`]). Es kennt
//! keine `HistoryCell`- oder `app.rs`-Typen — die spätere Verdrahtung (Slice
//! zur `app.rs`-Integration) übersetzt den Verlauf in [`ExportEntry`]-Werte.
//!
//! # Schlüsseltypen
//! - [`ExportOptions`] — Auswahl der eingeschlossenen Inhalte und optionale Kürzung.
//! - [`ExportEntry`] — minimale, quellcode-unabhängige Repräsentation eines Verlaufseintrags.
//! - [`ExportMeta`] — Kopfzeilen-Metadaten (Titel, Session-ID, Zeit, Verzeichnis, Modell).
//! - [`ExportError`] — handgeschriebener Fehlertyp für Ein-/Ausgabe- und Pfadfehler.
//!
//! # Nebenläufigkeit
//! Alle Funktionen sind rein synchron und zustandslos; kein Shared State.
//!
//! # Fehlertypen
//! [`ExportError`]: `Io` (Dateisystemfehler), `NoClipboard` (nur relevant für
//! `clipboard.rs`, hier re-exportiert für gemeinsame Fehlerbehandlung),
//! `PathRejected` (Pfadtraversal oder keine freie Datei gefunden).
//!
//! # Beispiele
//! ```ignore
//! use harw_tui::export::{ExportEntry, ExportMeta, ExportOptions, render_markdown};
//!
//! let meta = ExportMeta {
//!     title: Some("Testlauf".to_owned()),
//!     session_id: "abc123".to_owned(),
//!     started_at: Some("2026-09-14T05:30:00".to_owned()),
//!     cwd: Some("/home/mia/projects/harwness".to_owned()),
//!     model: Some("groq/llama".to_owned()),
//! };
//! let entries = vec![ExportEntry::User("Hallo".to_owned())];
//! let markdown = render_markdown(&meta, &entries, &ExportOptions::default());
//! assert!(markdown.contains("## Du"));
//! ```

use std::fmt;
use std::path::{Component, Path, PathBuf};

use crate::sanitize::{sanitize_display, sanitize_inline};

/// Textmarker, der ans Ende eines gekürzten Exports angehängt wird.
const TRUNCATION_MARKER: &str = "\n\n_[gekürzt]_\n";

/// Fallback-Titel, wenn [`ExportMeta::title`] `None` ist.
const DEFAULT_TITLE: &str = "harw-Session";

/// Fallback-Text für fehlende optionale Metadatenfelder.
const UNKNOWN_PLACEHOLDER: &str = "unbekannt";

/// Obergrenze für Nummerierungssuffixe beim Kollisionsschutz von [`write_export`].
const MAX_SUFFIX_ATTEMPTS: u32 = 99;

// ---------------------------------------------------------------------------
// Fehlertyp
// ---------------------------------------------------------------------------

/// Fehler beim Rendern, Schreiben oder Kopieren eines Exports.
///
/// # Beschreibung
/// Handgeschriebener Fehler-Enum (kein `anyhow`/`thiserror`) für alle
/// Fehlklassen, die beim `/export`-Fluss auftreten können. `NoClipboard`
/// wird von `clipboard.rs` erzeugt, gehört aber logisch zum selben
/// Nutzerfluss und wird deshalb hier zentral definiert.
///
/// # Varianten
/// - [`ExportError::Io`]: Dateisystemfehler beim atomaren Schreiben.
/// - [`ExportError::NoClipboard`]: Keine Zwischenablage erreichbar und OSC-52
///   entweder nicht verfügbar oder der Text ist dafür zu groß.
/// - [`ExportError::PathRejected`]: Der Zielpfad ist ein Traversal-Versuch
///   oder es wurde binnen 99 Versuchen kein freier Dateiname gefunden.
#[derive(Debug)]
pub enum ExportError {
    /// Ein-/Ausgabefehler beim atomaren Schreiben der Exportdatei.
    Io(std::io::Error),
    /// Keine Zwischenablage erreichbar (weder Systemwerkzeug noch OSC-52-Fallback).
    NoClipboard,
    /// Der übergebene Zielpfad wurde abgelehnt (Traversal oder erschöpfte Suffixe).
    PathRejected(String),
}

impl fmt::Display for ExportError {
    /// Menschenlesbare, deutsche Fehlermeldung ohne interne Details.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ExportError::Io(err) => write!(f, "Export konnte nicht geschrieben werden: {err}"),
            ExportError::NoClipboard => write!(
                f,
                "Keine Zwischenablage verfügbar (weder wl-copy/xclip/xsel/pbcopy noch OSC-52 möglich)"
            ),
            ExportError::PathRejected(reason) => {
                write!(f, "Zielpfad für den Export abgelehnt: {reason}")
            }
        }
    }
}

impl std::error::Error for ExportError {
    /// Verlinkt auf den zugrunde liegenden `io::Error`, sofern vorhanden.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ExportError::Io(err) => Some(err),
            ExportError::NoClipboard | ExportError::PathRejected(_) => None,
        }
    }
}

impl From<std::io::Error> for ExportError {
    /// Erlaubt `?` auf `io::Result`-Rückgaben von `harw_fsutil::write_atomic`.
    fn from(err: std::io::Error) -> Self {
        ExportError::Io(err)
    }
}

// ---------------------------------------------------------------------------
// Öffentliche Datentypen
// ---------------------------------------------------------------------------

/// Auswahl der eingeschlossenen Inhalte und optionale Zeichenbegrenzung.
///
/// # Beschreibung
/// Steuert, ob Werkzeugaufrufe und Denkschritte im gerenderten Markdown
/// erscheinen, sowie eine optionale harte Obergrenze der Ausgabelänge.
/// Beide Inhaltsschalter sind standardmäßig aus, da sie interne
/// Implementierungsdetails offenlegen können.
///
/// # Felder
/// - `include_tool_calls` (`bool`): Werkzeugaufrufe als `- ⚙ <label>` einschließen.
/// - `include_reasoning` (`bool`): Denkschritte als Zitatblock einschließen.
/// - `max_chars` (`Option<usize>`): harte Obergrenze in Unicode-Zeichen.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExportOptions {
    /// Werkzeugaufrufe im Export anzeigen (Standard: aus).
    pub include_tool_calls: bool,
    /// Denkschritte (Reasoning) im Export anzeigen (Standard: aus).
    pub include_reasoning: bool,
    /// Harte Obergrenze der Ausgabelänge in Unicode-Zeichen.
    pub max_chars: Option<usize>,
}

/// Kopfzeilen-Metadaten für den Export.
///
/// # Beschreibung
/// Wird als YAML-ähnlicher Kopf über den Gesprächsabschnitten gerendert.
/// Alle Felder außer `session_id` sind optional und fallen auf
/// `"unbekannt"` zurück, wenn `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportMeta {
    /// Anzeigetitel der Session; fällt auf `"harw-Session"` zurück wenn `None`.
    pub title: Option<String>,
    /// Kanonische Session-ID.
    pub session_id: String,
    /// Startzeitpunkt als vorformatierter String (z. B. ISO 8601).
    pub started_at: Option<String>,
    /// Arbeitsverzeichnis der Session.
    pub cwd: Option<String>,
    /// Verwendetes Modell (Provider/Modell-ID).
    pub model: Option<String>,
}

/// Minimale, quellcode-unabhängige Repräsentation eines Verlaufseintrags.
///
/// # Beschreibung
/// `HistoryCell` besitzt keinen Text-Accessor; dieser Typ ist die
/// Eingabeschnittstelle für [`render_markdown`], die eine spätere
/// `app.rs`-Verdrahtung aus dem tatsächlichen Verlauf befüllt.
///
/// # Varianten
/// - `User`: Nutzernachricht (mehrzeilig erlaubt).
/// - `Assistant`: Antwort von harw (mehrzeilig erlaubt).
/// - `Tool`: Werkzeugaufruf mit Label und optionaler Ergebniszusammenfassung.
/// - `System`: Systemmeldung (z. B. Hinweise, Fehler außerhalb eines Turns).
/// - `Reasoning`: Denkschritt/Begründung des Modells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportEntry {
    /// Nutzernachricht.
    User(String),
    /// Antwort von harw.
    Assistant(String),
    /// Werkzeugaufruf mit Label (z. B. `Bash(git status)`) und optionaler Zusammenfassung.
    Tool {
        /// Kurzes, einzeiliges Label des Werkzeugaufrufs.
        label: String,
        /// Optionale, einzeilige Zusammenfassung des Ergebnisses.
        summary: Option<String>,
    },
    /// Systemmeldung außerhalb eines normalen Turns.
    System(String),
    /// Denkschritt/Begründung des Modells.
    Reasoning(String),
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Interner Diskriminator für die zuletzt gerenderte Überschrift, damit
/// aufeinanderfolgende Einträge derselben Rolle nicht mehrfach überschrieben werden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    /// Zuletzt gerenderte Überschrift war `## Du`.
    Du,
    /// Zuletzt gerenderte Überschrift war `## harw`.
    Harw,
    /// Zuletzt gerenderte Überschrift war `## System`.
    System,
}

/// Rendert Metadaten und Verlaufseinträge als Markdown-Dokument.
///
/// # Beschreibung
/// Erzeugt zunächst einen Titel (`# <title>` bzw. `# harw-Session`) und eine
/// Metadaten-Aufzählung (Session-ID, Datum, Verzeichnis, Modell), danach die
/// Gesprächsabschnitte in der Reihenfolge von `entries`. Aufeinanderfolgende
/// `User`- bzw. `Assistant`-Einträge teilen sich eine `## Du`- bzw.
/// `## harw`-Überschrift; ein Rollenwechsel erzeugt eine neue Überschrift.
/// `Tool`-Einträge werden nur bei `opts.include_tool_calls` als
/// `- ⚙ <label>` gerendert, `Reasoning`-Einträge nur bei
/// `opts.include_reasoning` als Zitatblock. Fremdtext wird über
/// [`crate::sanitize::sanitize_display`] bzw.
/// [`crate::sanitize::sanitize_inline`] terminal- und markdown-sicher
/// aufbereitet; Fenced-Code-Blöcke (dreifache Backticks) bleiben dabei
/// erhalten, da Sanitize keine druckbaren Zeichen entfernt.
///
/// Ist `opts.max_chars` gesetzt und das Ergebnis länger, wird an einer
/// Unicode-Zeichengrenze gekürzt und `\n\n_[gekürzt]_\n` angehängt.
///
/// # Argumente
/// - `meta` (`&ExportMeta`): Kopfzeilen-Metadaten.
/// - `entries` (`&[ExportEntry]`): Verlaufseinträge in chronologischer Reihenfolge.
/// - `opts` (`&ExportOptions`): Inhaltsauswahl und optionale Kürzung.
///
/// # Rückgabe
/// Vollständiges Markdown-Dokument mit genau einem abschließenden Zeilenumbruch.
///
/// # Beispiele
/// ```ignore
/// use harw_tui::export::{ExportEntry, ExportMeta, ExportOptions, render_markdown};
///
/// let meta = ExportMeta {
///     title: None,
///     session_id: "s1".to_owned(),
///     started_at: None,
///     cwd: None,
///     model: None,
/// };
/// let entries = vec![ExportEntry::User("Hi".to_owned())];
/// let out = render_markdown(&meta, &entries, &ExportOptions::default());
/// assert!(out.ends_with('\n'));
/// ```
#[must_use]
pub fn render_markdown(meta: &ExportMeta, entries: &[ExportEntry], opts: &ExportOptions) -> String {
    let mut out = String::new();

    let title = meta
        .title
        .as_deref()
        .map(sanitize_inline)
        .unwrap_or_else(|| DEFAULT_TITLE.to_owned());
    out.push_str("# ");
    out.push_str(&title);
    out.push_str("\n\n");

    out.push_str("- **Session-ID:** ");
    out.push_str(&sanitize_inline(&meta.session_id));
    out.push('\n');
    out.push_str("- **Datum:** ");
    out.push_str(&meta.started_at.as_deref().map(sanitize_inline).unwrap_or_else(|| UNKNOWN_PLACEHOLDER.to_owned()));
    out.push('\n');
    out.push_str("- **Verzeichnis:** ");
    out.push_str(&meta.cwd.as_deref().map(sanitize_inline).unwrap_or_else(|| UNKNOWN_PLACEHOLDER.to_owned()));
    out.push('\n');
    out.push_str("- **Modell:** ");
    out.push_str(&meta.model.as_deref().map(sanitize_inline).unwrap_or_else(|| UNKNOWN_PLACEHOLDER.to_owned()));
    out.push_str("\n\n");

    let mut current: Option<Section> = None;

    for entry in entries {
        match entry {
            ExportEntry::User(text) => {
                if current != Some(Section::Du) {
                    out.push_str("## Du\n\n");
                    current = Some(Section::Du);
                }
                out.push_str(&sanitize_display(text));
                out.push_str("\n\n");
            }
            ExportEntry::Assistant(text) => {
                if current != Some(Section::Harw) {
                    out.push_str("## harw\n\n");
                    current = Some(Section::Harw);
                }
                out.push_str(&sanitize_display(text));
                out.push_str("\n\n");
            }
            ExportEntry::System(text) => {
                if current != Some(Section::System) {
                    out.push_str("## System\n\n");
                    current = Some(Section::System);
                }
                out.push_str(&sanitize_display(text));
                out.push_str("\n\n");
            }
            ExportEntry::Tool { label, summary } => {
                if !opts.include_tool_calls {
                    continue;
                }
                out.push_str("- ⚙ ");
                out.push_str(&sanitize_inline(label));
                out.push('\n');
                if let Some(summary) = summary {
                    out.push_str("  ");
                    out.push_str(&sanitize_inline(summary));
                    out.push('\n');
                }
                out.push('\n');
            }
            ExportEntry::Reasoning(text) => {
                if !opts.include_reasoning {
                    continue;
                }
                let sanitized = sanitize_display(text);
                for line in sanitized.lines() {
                    out.push_str("> ");
                    out.push_str(line);
                    out.push('\n');
                }
                out.push('\n');
            }
        }
    }

    let mut result = out.trim_end_matches('\n').to_owned();
    result.push('\n');

    if let Some(max_chars) = opts.max_chars {
        result = truncate_markdown(&result, max_chars);
    }

    result
}

/// Kürzt `text` auf höchstens `max_chars` Unicode-Zeichen und hängt den
/// Kürzungsmarker an.
///
/// # Beschreibung
/// Schneidet an einer Zeichen- (nicht Byte-)Grenze, damit mehrbytige
/// UTF-8-Sequenzen niemals mittendrin getrennt werden. Ist `text` bereits
/// kurz genug, wird es unverändert zurückgegeben.
///
/// # Argumente
/// - `text` (`&str`): der zu kürzende, bereits fertig gerenderte Text.
/// - `max_chars` (`usize`): maximale Anzahl Unicode-Zeichen vor dem Marker.
///
/// # Rückgabe
/// Gekürzter Text mit angehängtem `\n\n_[gekürzt]_\n`, oder der unveränderte
/// Text, wenn keine Kürzung nötig war.
fn truncate_markdown(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let cut_byte = text
        .char_indices()
        .nth(max_chars)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len());
    let mut truncated = text[..cut_byte].to_owned();
    truncated.push_str(TRUNCATION_MARKER);
    truncated
}

// ---------------------------------------------------------------------------
// Dateisystem
// ---------------------------------------------------------------------------

/// Baut den Standard-Exportpfad `<dir>/harw-export-<now>.md`.
///
/// # Beschreibung
/// Reine Pfadkonstruktion ohne Dateisystemzugriff. `now` wird unverändert
/// in den Dateinamen übernommen; der Aufrufer ist für ein dateinamensicheres
/// Format verantwortlich (z. B. `2026-09-14T05-30-00`).
///
/// # Argumente
/// - `now` (`&str`): Zeitstempel für den Dateinamen.
/// - `dir` (`&Path`): Zielverzeichnis.
///
/// # Rückgabe
/// `<dir>/harw-export-<now>.md`.
///
/// # Beispiele
/// ```ignore
/// use harw_tui::export::default_export_path;
/// use std::path::Path;
///
/// let path = default_export_path("2026-09-14T05-30-00", Path::new("/tmp"));
/// assert_eq!(path, Path::new("/tmp/harw-export-2026-09-14T05-30-00.md"));
/// ```
#[must_use]
pub fn default_export_path(now: &str, dir: &Path) -> PathBuf {
    dir.join(format!("harw-export-{now}.md"))
}

/// Prüft `path` auf Traversal-Komponenten (`..`).
///
/// # Beschreibung
/// Ein Export darf niemals über einen Vorfahren-Komponenten auf ein
/// Verzeichnis außerhalb des vom Aufrufer beabsichtigten Ziels ausweichen.
///
/// # Argumente
/// - `path` (`&Path`): der zu prüfende Zielpfad.
///
/// # Fehler
/// - [`ExportError::PathRejected`]: `path` enthält eine `..`-Komponente.
fn reject_traversal(path: &Path) -> Result<(), ExportError> {
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(ExportError::PathRejected(format!(
            "Pfad-Traversal nicht erlaubt: {}",
            path.display()
        )));
    }
    Ok(())
}

/// Findet den ersten freien Pfad, indem bei Kollision `-2`, `-3`, … an den
/// Dateistamm gehängt wird.
///
/// # Beschreibung
/// Existiert `path` nicht, wird `path` selbst zurückgegeben. Andernfalls
/// wird die Dateiendung erhalten und vor die Endung ein Zähler ab `2`
/// eingefügt, bis ein nicht existierender Pfad gefunden wird oder
/// [`MAX_SUFFIX_ATTEMPTS`] erreicht ist.
///
/// # Argumente
/// - `path` (`&Path`): gewünschter Zielpfad.
///
/// # Fehler
/// - [`ExportError::PathRejected`]: binnen `MAX_SUFFIX_ATTEMPTS` Versuchen
///   wurde kein freier Name gefunden.
fn next_available_path(path: &Path) -> Result<PathBuf, ExportError> {
    if !path.exists() {
        return Ok(path.to_owned());
    }

    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("export")
        .to_owned();
    let extension = path.extension().and_then(|s| s.to_str()).map(str::to_owned);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));

    for n in 2..=MAX_SUFFIX_ATTEMPTS {
        let file_name = match &extension {
            Some(ext) => format!("{stem}-{n}.{ext}"),
            None => format!("{stem}-{n}"),
        };
        let candidate = parent.join(file_name);
        if !candidate.exists() {
            return Ok(candidate);
        }
    }

    Err(ExportError::PathRejected(format!(
        "keine freie Datei für {} gefunden (bis -{MAX_SUFFIX_ATTEMPTS})",
        path.display()
    )))
}

/// Schreibt `content` atomar nach `path`, ohne eine bestehende Datei zu überschreiben.
///
/// # Beschreibung
/// Lehnt Pfad-Traversal ab ([`reject_traversal`]), sucht dann über
/// [`next_available_path`] einen freien Dateinamen (Suffixe `-2` … `-99`)
/// und schreibt über [`harw_fsutil::write_atomic`] mit
/// `AtomicWriteOptions::private()` (Rechte `0600`, Verzeichnis-`fsync`).
///
/// # Argumente
/// - `path` (`&Path`): gewünschter Zielpfad (kann `Datum` bereits enthalten).
/// - `content` (`&str`): vollständiger Dateiinhalt.
///
/// # Fehler
/// - [`ExportError::PathRejected`]: Traversal-Versuch oder erschöpfte Suffixe.
/// - [`ExportError::Io`]: Dateisystemfehler beim atomaren Schreiben.
///
/// # Beispiele
/// ```ignore
/// use harw_tui::export::write_export;
/// use std::path::Path;
///
/// write_export(Path::new("/tmp/harw-export-test.md"), "# Test\n")?;
/// # Ok::<(), harw_tui::export::ExportError>(())
/// ```
pub fn write_export(path: &Path, content: &str) -> Result<(), ExportError> {
    reject_traversal(path)?;
    let target = next_available_path(path)?;
    harw_fsutil::write_atomic(
        &target,
        content.as_bytes(),
        harw_fsutil::AtomicWriteOptions::private(),
    )
    .map_err(ExportError::from)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn meta_minimal() -> ExportMeta {
        ExportMeta {
            title: None,
            session_id: "sess-1".to_owned(),
            started_at: None,
            cwd: None,
            model: None,
        }
    }

    /// Titel, Metadaten und Rollenüberschriften erscheinen in der Eingabereihenfolge.
    #[test]
    fn test_render_markdown_order_and_headers() {
        let meta = ExportMeta {
            title: Some("Mein Export".to_owned()),
            session_id: "sess-42".to_owned(),
            started_at: Some("2026-09-14T05:30:00".to_owned()),
            cwd: Some("/home/mia/projects/harwness".to_owned()),
            model: Some("groq/llama".to_owned()),
        };
        let entries = vec![
            ExportEntry::User("Frage 1".to_owned()),
            ExportEntry::Assistant("Antwort 1".to_owned()),
            ExportEntry::User("Frage 2".to_owned()),
        ];
        let out = render_markdown(&meta, &entries, &ExportOptions::default());

        assert!(out.starts_with("# Mein Export\n\n"));
        assert!(out.contains("- **Session-ID:** sess-42"));
        assert!(out.contains("- **Datum:** 2026-09-14T05:30:00"));
        assert!(out.contains("- **Verzeichnis:** /home/mia/projects/harwness"));
        assert!(out.contains("- **Modell:** groq/llama"));

        let du_first = out.find("## Du").expect("erste Du-Überschrift fehlt");
        let harw_pos = out.find("## harw").expect("harw-Überschrift fehlt");
        let du_second = out.rfind("## Du").expect("zweite Du-Überschrift fehlt");
        assert!(du_first < harw_pos, "Frage 1 muss vor der Antwort stehen");
        assert!(harw_pos < du_second, "Antwort muss vor Frage 2 stehen");
        assert!(out.contains("Frage 1"));
        assert!(out.contains("Antwort 1"));
        assert!(out.contains("Frage 2"));
        assert!(out.ends_with('\n'));
    }

    /// Fehlende optionale Metadaten fallen auf `"unbekannt"` zurück.
    #[test]
    fn test_render_markdown_missing_meta_falls_back_to_unknown() {
        let out = render_markdown(&meta_minimal(), &[], &ExportOptions::default());
        assert!(out.starts_with("# harw-Session\n\n"));
        assert!(out.contains("- **Datum:** unbekannt"));
        assert!(out.contains("- **Verzeichnis:** unbekannt"));
        assert!(out.contains("- **Modell:** unbekannt"));
    }

    /// Werkzeugaufrufe erscheinen nur, wenn `include_tool_calls` gesetzt ist.
    #[test]
    fn test_render_markdown_tool_entries_only_when_enabled() {
        let entries = vec![ExportEntry::Tool {
            label: "Bash(git status)".to_owned(),
            summary: Some("0 Dateien geändert".to_owned()),
        }];

        let without = render_markdown(&meta_minimal(), &entries, &ExportOptions::default());
        assert!(!without.contains("⚙"));

        let opts = ExportOptions {
            include_tool_calls: true,
            ..ExportOptions::default()
        };
        let with = render_markdown(&meta_minimal(), &entries, &opts);
        assert!(with.contains("- ⚙ Bash(git status)"));
        assert!(with.contains("0 Dateien geändert"));
    }

    /// Denkschritte erscheinen nur, wenn `include_reasoning` gesetzt ist.
    #[test]
    fn test_render_markdown_reasoning_only_when_enabled() {
        let entries = vec![ExportEntry::Reasoning("weil X gilt".to_owned())];

        let without = render_markdown(&meta_minimal(), &entries, &ExportOptions::default());
        assert!(!without.contains("weil X gilt"));

        let opts = ExportOptions {
            include_reasoning: true,
            ..ExportOptions::default()
        };
        let with = render_markdown(&meta_minimal(), &entries, &opts);
        assert!(with.contains("> weil X gilt"));
    }

    /// Fenced-Code-Blöcke bleiben beim Rendern unverändert erhalten.
    #[test]
    fn test_render_markdown_preserves_fenced_code_blocks() {
        let entries = vec![ExportEntry::Assistant(
            "Hier:\n```rust\nfn main() {}\n```".to_owned(),
        )];
        let out = render_markdown(&meta_minimal(), &entries, &ExportOptions::default());
        assert!(out.contains("```rust\nfn main() {}\n```"));
    }

    /// Kürzung schneidet an einer Zeichen-, nicht Byte-Grenze und hängt den Marker an.
    #[test]
    fn test_render_markdown_truncates_at_char_boundary_with_multibyte() {
        // "ü" ist zwei Bytes (0xC3 0xBC) in UTF-8; ein Byte-Schnitt würde hier panicen.
        let text = "üüüüüüüüüü";
        let entries = vec![ExportEntry::Assistant(text.to_owned())];
        let opts = ExportOptions {
            max_chars: Some(20),
            ..ExportOptions::default()
        };

        let out = render_markdown(&meta_minimal(), &entries, &opts);
        assert!(out.contains("_[gekürzt]_"));
        // Darf nicht mitten in einem Mehrbyte-Zeichen enden (kein Ersatzzeichen U+FFFD).
        assert!(!out.contains('\u{FFFD}'));
    }

    /// Ohne `max_chars` bleibt der Text unverändert und unabgeschnitten.
    #[test]
    fn test_render_markdown_no_truncation_without_max_chars() {
        let entries = vec![ExportEntry::User("kurzer Text".to_owned())];
        let out = render_markdown(&meta_minimal(), &entries, &ExportOptions::default());
        assert!(!out.contains("_[gekürzt]_"));
    }

    /// `default_export_path` baut `<dir>/harw-export-<now>.md`.
    #[test]
    fn test_default_export_path_builds_expected_name() {
        let dir = Path::new("/tmp/harw-export-test");
        let path = default_export_path("2026-09-14T05-30-00", dir);
        assert_eq!(
            path,
            PathBuf::from("/tmp/harw-export-test/harw-export-2026-09-14T05-30-00.md")
        );
    }

    /// Existiert die Zieldatei bereits, wird bei erneutem `write_export` ein
    /// Suffix `-2` angehängt statt zu überschreiben.
    #[test]
    fn test_write_export_suffixes_when_file_exists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("harw-export-2026-09-14T05-30-00.md");

        write_export(&path, "erster Inhalt\n").expect("erster Export sollte klappen");
        assert!(path.exists());
        assert_eq!(
            std::fs::read_to_string(&path).expect("lesen"),
            "erster Inhalt\n"
        );

        write_export(&path, "zweiter Inhalt\n").expect("zweiter Export sollte klappen");
        let suffixed = dir.path().join("harw-export-2026-09-14T05-30-00-2.md");
        assert!(suffixed.exists(), "Suffix -2 muss angelegt werden");
        assert_eq!(
            std::fs::read_to_string(&suffixed).expect("lesen"),
            "zweiter Inhalt\n"
        );
        // Die ursprüngliche Datei bleibt unverändert.
        assert_eq!(
            std::fs::read_to_string(&path).expect("lesen"),
            "erster Inhalt\n"
        );
    }

    /// Ein Zielpfad mit `..`-Komponente wird abgelehnt.
    #[test]
    fn test_write_export_refuses_traversal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("../escape.md");

        let err = write_export(&path, "inhalt\n").expect_err("Traversal muss abgelehnt werden");
        assert!(matches!(err, ExportError::PathRejected(_)));
    }
}
