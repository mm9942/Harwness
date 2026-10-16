//! Strukturierter Markdown-/JSON-Export einer Chat-Historie für den
//! `/export`-Befehl der TUI.
//!
//! Spec-Quelle: `docs/design/tui-command-contract.md` (Export).
//!
//! # Verantwortung
//! Dieses Modul rendert eine gegebene Menge von [`ExportEntry`]-Werten als
//! menschenlesbares Markdown-Dokument ([`render_markdown`]) oder versioniertes
//! JSON ([`render_json`]) und schreibt es atomar und ohne Überschreiben auf die
//! Platte ([`write_export`]). Es kennt
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
//!     cwd: Some("/home/user/projects/harwness".to_owned()),
//!     model: Some("groq/llama".to_owned()),
//! };
//! let entries = vec![ExportEntry::User("Hallo".to_owned())];
//! let markdown = render_markdown(&meta, &entries, &ExportOptions::default());
//! assert!(markdown.contains("## Du"));
//! ```

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde_json::{Map, Value, json};

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
#[derive(Debug, harw_macros::HarwError)]
pub enum ExportError {
    /// Ein-/Ausgabefehler beim atomaren Schreiben der Exportdatei.
    #[msg("Export konnte nicht geschrieben werden: {0}")]
    #[from]
    Io(std::io::Error),
    /// Keine Zwischenablage erreichbar (weder Systemwerkzeug noch OSC-52-Fallback).
    #[msg("Keine Zwischenablage verfügbar (weder wl-copy/xclip/xsel/pbcopy noch OSC-52 möglich)")]
    NoClipboard,
    /// Der übergebene Zielpfad wurde abgelehnt (Traversal oder erschöpfte Suffixe).
    #[msg("Zielpfad für den Export abgelehnt: {0}")]
    PathRejected(String),
}

// ---------------------------------------------------------------------------
// Öffentliche Datentypen
// ---------------------------------------------------------------------------

/// Auswahl der eingeschlossenen Inhalte und optionale Zeichenbegrenzung.
///
/// # Beschreibung
/// Steuert, ob Werkzeugaufrufe und Denkschritte im gerenderten Markdown
/// erscheinen, sowie eine optionale harte Obergrenze der Ausgabelänge.
/// Werkzeugdaten sind standardmäßig enthalten; Reasoning bleibt optional und
/// darf nur als explizite Zusammenfassung vorliegen.
///
/// # Felder
/// - `include_tool_calls` (`bool`): Werkzeugaufrufe als `- ⚙ <label>` einschließen.
/// - `include_reasoning` (`bool`): Denkschritte als Zitatblock einschließen.
/// - `max_chars` (`Option<usize>`): harte Obergrenze in Unicode-Zeichen für
///   das gesamte gerenderte Dokument.
/// - `max_chars_per_entry` (`Option<usize>`): Obergrenze in Unicode-Zeichen
///   je gerendertem JSON-/Text-Block innerhalb eines ToolCall-/
///   ToolResult-Eintrags (siehe [`render_json_block`]). Standard: `Some(4000)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportOptions {
    /// Werkzeugaufrufe und -ergebnisse im Export anzeigen (Standard: an).
    pub include_tool_calls: bool,
    /// Denkschritte (Reasoning) im Export anzeigen (Standard: aus).
    pub include_reasoning: bool,
    /// Harte Obergrenze der Ausgabelänge in Unicode-Zeichen.
    pub max_chars: Option<usize>,
    /// Obergrenze je gerendertem JSON-/Text-Block eines ToolCall-/
    /// ToolResult-Eintrags in Unicode-Zeichen (Standard: `Some(4000)`).
    pub max_chars_per_entry: Option<usize>,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self {
            // Strukturierte Werkzeuge sind Bestandteil des Exportvertrags;
            // ein Aufrufer kann sie weiterhin explizit ausblenden.
            include_tool_calls: true,
            include_reasoning: false,
            max_chars: None,
            // Verhindert, dass einzelne ToolCall-/ToolResult-Blöcke (bis zu
            // 70.000 Zeichen in einer einzigen Zeile) den Export dominieren.
            max_chars_per_entry: Some(4000),
        }
    }
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

/// Zusätzliche, vorwärtskompatible Sessionmetadaten.
///
/// Die bestehenden Felder von [`ExportMeta`] bleiben absichtlich unverändert,
/// damit alte Struct-Literale in `app.rs` weiter kompilieren. Neue Integrationen
/// können diese Erweiterungen an beide Renderer übergeben. Schlüssel mit
/// Systemprompt-Bezug werden verworfen, sensible Werte redigiert.
pub type ExportMetaExtensions = BTreeMap<String, Value>;

/// Herkunft eines Ereignisses aus einem Agenten- oder Kind-Agenten-Kontext.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportAgentRef {
    /// Stabile Agenten-ID, sofern vorhanden.
    pub agent_id: String,
    /// Rolle des Agenten, sofern bekannt.
    pub role: Option<String>,
    /// Eltern-Agent, sofern es sich um einen Kind-Agenten handelt.
    pub parent_id: Option<String>,
}

/// Ergebnisstatus eines strukturierten Werkzeugergebnisses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportStatus {
    /// Werkzeug wurde erfolgreich ausgeführt.
    Success,
    /// Werkzeug ist mit einem Fehler beendet worden.
    Error,
}

impl ExportStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
        }
    }
}

/// Typisierter Fehler-Eintrag im Exportereignisstrom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportErrorEntry {
    /// Maschinenlesbarer Fehlercode, sofern vorhanden.
    pub code: Option<String>,
    /// Sichere, menschenlesbare Fehlermeldung.
    pub message: String,
    /// Optionale strukturierte Zusatzdaten.
    pub details: Option<Value>,
    /// Optionale Agentenherkunft.
    pub agent: Option<ExportAgentRef>,
}

/// Typisierter Plan-Eintrag im Exportereignisstrom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportPlanEntry {
    /// Plan-ID, sofern vorhanden.
    pub plan_id: Option<String>,
    /// Planrevision, sofern vorhanden.
    pub revision: Option<u64>,
    /// Zusammenfassung des Plans, niemals opaque Modell-Reasoning.
    pub summary: String,
    /// Optionale, explizit dargestellte Schritte.
    pub steps: Vec<String>,
    /// Freier Status wie `pending`, `running` oder `completed`.
    pub status: Option<String>,
    /// Optionale Agentenherkunft.
    pub agent: Option<ExportAgentRef>,
}

/// Typisierter Agenten-Eintrag im Exportereignisstrom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportAgentEntry {
    /// Stabile Agenten-ID.
    pub agent_id: String,
    /// Agentenrolle, sofern vorhanden.
    pub role: Option<String>,
    /// Eltern-Agent, sofern vorhanden.
    pub parent_id: Option<String>,
    /// Freier Lebenszyklusstatus.
    pub status: Option<String>,
    /// Sichere Zusammenfassung des Agentenereignisses.
    pub summary: Option<String>,
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
/// - `Reasoning`: rückwärtskompatible Reasoning-Zusammenfassung.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportEntry {
    /// Nutzernachricht.
    User(String),
    /// Antwort von harw.
    Assistant(String),
    /// Werkzeugaufruf mit Label (z. B. `Shell(git status)`) und optionaler Zusammenfassung.
    Tool {
        /// Kurzes, einzeiliges Label des Werkzeugaufrufs.
        label: String,
        /// Optionale, einzeilige Zusammenfassung des Ergebnisses.
        summary: Option<String>,
    },
    /// Strukturierter Werkzeugaufruf mit vollständigen (redigierten) Argumenten.
    ToolCall {
        /// Eindeutige Zuordnung zum Ergebnis.
        call_id: String,
        /// Kanonischer Werkzeugname.
        tool_name: String,
        /// Vollständige Argumentstruktur.
        arguments: Value,
        /// Laufzeit in Millisekunden, sofern bekannt.
        duration_ms: Option<u64>,
        /// Vertrauens-/Freigabestufe, sofern bekannt.
        trust: Option<String>,
        /// Optionale Agentenherkunft.
        agent: Option<ExportAgentRef>,
    },
    /// Strukturiertes Werkzeugergebnis, getrennt vom Aufruf.
    ToolResult {
        /// ID des zugehörigen Aufrufs.
        call_id: String,
        /// Werkzeugname, sofern der Resultatlieferant ihn kennt.
        tool_name: Option<String>,
        /// Vollständige Resultatstruktur.
        result: Value,
        /// Ausführung erfolgreich oder fehlerhaft.
        status: ExportStatus,
        /// Optionale sichere Fehlerbeschreibung bei `Error`.
        error: Option<String>,
        /// Laufzeit in Millisekunden, sofern bekannt.
        duration_ms: Option<u64>,
        /// Vertrauens-/Freigabestufe, sofern bekannt.
        trust: Option<String>,
        /// Optionale Agentenherkunft.
        agent: Option<ExportAgentRef>,
    },
    /// Systemmeldung außerhalb eines normalen Turns.
    System(String),
    /// Vom System in den Verlauf eingespeiste Meldung (Ergebnis eines
    /// Hintergrund-Agenten, Nachricht/Frage eines Kindes, die Anweisung eines
    /// Auto-Turns). Steht in der Historie als Nutzernachricht, stammt aber
    /// nicht von der Nutzerin und wird deshalb unter `## Hintergrund-Agent`
    /// statt `## Du` gerendert.
    Notice(String),
    /// Legacy-Kompatibilität: darf ausschließlich eine Reasoning-Zusammenfassung
    /// enthalten, niemals opaque oder rohe Chain-of-Thought-Blöcke.
    Reasoning(String),
    /// Typisierter Fehler außerhalb eines Werkzeugresultats.
    Error(ExportErrorEntry),
    /// Explizite Plan-Zusammenfassung.
    Plan(ExportPlanEntry),
    /// Agentenlebenszyklus oder Agentenzusammenfassung.
    Agent(ExportAgentEntry),
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
    /// Zuletzt gerenderte Überschrift war `## Hintergrund-Agent`.
    Notice,
}

/// Vorgabewert der Vertrauensstufe eines Werkzeugergebnisses. Im Markdown
/// wird nur eine davon **abweichende** Stufe gezeigt (sonst stünde unter
/// jedem Werkzeug `Trust: untrusted`); JSON bleibt vollständig.
const DEFAULT_TRUST: &str = "untrusted";

/// Rendert Metadaten und Verlaufseinträge als Markdown-Dokument.
///
/// # Beschreibung
/// Erzeugt zunächst einen Titel (`# <title>` bzw. `# harw-Session`) und eine
/// Metadaten-Aufzählung (Session-ID, Datum, Verzeichnis, Modell), danach die
/// Gesprächsabschnitte in der Reihenfolge von `entries`. Aufeinanderfolgende
/// `User`- bzw. `Assistant`-Einträge teilen sich eine `## Du`- bzw.
/// `## harw`-Überschrift; ein Rollenwechsel erzeugt eine neue Überschrift.
/// Eingespeiste `Notice`-Einträge (Hintergrund-Agenten) stehen unter
/// `## Hintergrund-Agent`. Die Vertrauensstufe eines Werkzeugs erscheint nur,
/// wenn sie vom Vorgabewert `untrusted` abweicht.
/// `ToolCall`-, `ToolResult`- und (bei `opts.include_reasoning`)
/// `Reasoning`-Einträge schalten stets auf die `## harw`-Überschrift um,
/// auch direkt nach einer Nutzernachricht — inhaltlich gehören sie zu harws
/// Turn. `Tool`-, `ToolCall`- und `ToolResult`-Einträge werden nur bei
/// `opts.include_tool_calls` gerendert, `Reasoning`-Einträge nur bei
/// `opts.include_reasoning` als Zitatblock. ATX-Überschriften (`#…######`)
/// in `User`- und `Assistant`-Text werden über [`demote_markdown_headings`]
/// um zwei Ebenen herabgestuft, damit sie nicht mit den Export-eigenen
/// `#`/`##`-Ebenen kollidieren; Fenced-Code-Blöcke bleiben dabei
/// unangetastet. ToolCall-Argumente und ToolResult-Ergebnisse werden über
/// [`render_json_block`] dargestellt: mehrzeilige String-Blätter (insb.
/// `value`) landen in einem eigenen ```text-Block mit echten
/// Zeilenumbrüchen statt als escapte JSON-Zeile, und jeder Block wird
/// unabhängig über `opts.max_chars_per_entry` gekappt. Fremdtext und
/// strukturierte Werte werden zentral redigiert und danach über
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
    render_markdown_with_extensions(meta, entries, opts, &ExportMetaExtensions::new())
}

/// Rendert Markdown mit zusätzlichen, vorwärtskompatiblen Metadaten.
#[must_use]
pub fn render_markdown_with_extensions(
    meta: &ExportMeta,
    entries: &[ExportEntry],
    opts: &ExportOptions,
    extensions: &ExportMetaExtensions,
) -> String {
    let mut out = String::new();

    let title = meta
        .title
        .as_deref()
        .map(redact_text)
        .map(|text| sanitize_inline(&text))
        .unwrap_or_else(|| DEFAULT_TITLE.to_owned());
    out.push_str("# ");
    out.push_str(&title);
    out.push_str("\n\n");

    out.push_str("- **Session-ID:** ");
    out.push_str(&sanitize_inline(&redact_text(&meta.session_id)));
    out.push('\n');
    out.push_str("- **Datum:** ");
    out.push_str(
        &meta
            .started_at
            .as_deref()
            .map(format_started_at_for_display)
            .map(|text| sanitize_inline(&redact_text(&text)))
            .unwrap_or_else(|| UNKNOWN_PLACEHOLDER.to_owned()),
    );
    out.push('\n');
    out.push_str("- **Verzeichnis:** ");
    out.push_str(
        &meta
            .cwd
            .as_deref()
            .map(redact_text)
            .map(|text| sanitize_inline(&text))
            .unwrap_or_else(|| UNKNOWN_PLACEHOLDER.to_owned()),
    );
    out.push('\n');
    out.push_str("- **Modell:** ");
    out.push_str(
        &meta
            .model
            .as_deref()
            .map(redact_text)
            .map(|text| sanitize_inline(&text))
            .unwrap_or_else(|| UNKNOWN_PLACEHOLDER.to_owned()),
    );
    for (key, value) in extensions {
        if is_system_prompt_key(key) {
            continue;
        }
        out.push('\n');
        out.push_str("- **");
        out.push_str(&sanitize_inline(&redact_text(key)));
        out.push_str(":** ");
        out.push_str(&sanitize_inline(&redact_text(&display_json(
            &redact_json_value(value),
        ))));
    }
    out.push_str("\n\n");

    let mut current: Option<Section> = None;

    for entry in entries {
        match entry {
            ExportEntry::User(text) => {
                if current != Some(Section::Du) {
                    out.push_str("## Du\n\n");
                    current = Some(Section::Du);
                }
                out.push_str(&sanitize_display(&redact_text(&demote_markdown_headings(
                    text,
                ))));
                out.push_str("\n\n");
            }
            ExportEntry::Assistant(text) => {
                if current != Some(Section::Harw) {
                    out.push_str("## harw\n\n");
                    current = Some(Section::Harw);
                }
                out.push_str(&sanitize_display(&redact_text(&demote_markdown_headings(
                    text,
                ))));
                out.push_str("\n\n");
            }
            ExportEntry::Notice(text) => {
                if current != Some(Section::Notice) {
                    out.push_str("## Hintergrund-Agent\n\n");
                    current = Some(Section::Notice);
                }
                out.push_str(&sanitize_display(&redact_text(&demote_markdown_headings(
                    text,
                ))));
                out.push_str("\n\n");
            }
            ExportEntry::System(text) => {
                if current != Some(Section::System) {
                    out.push_str("## System\n\n");
                    current = Some(Section::System);
                }
                // `System` ist die alte UI-Statuszeile. Systempromptdaten
                // werden nicht als eigenes Exportfeld modelliert oder erzeugt.
                out.push_str(&sanitize_display(&redact_text(text)));
                out.push_str("\n\n");
            }
            ExportEntry::Tool { label, summary } => {
                if !opts.include_tool_calls {
                    continue;
                }
                out.push_str("- ⚙ ");
                out.push_str(&sanitize_inline(&redact_text(label)));
                out.push('\n');
                if let Some(summary) = summary {
                    out.push_str("  ");
                    out.push_str(&sanitize_inline(&redact_text(summary)));
                    out.push('\n');
                }
                out.push('\n');
            }
            ExportEntry::ToolCall {
                call_id,
                tool_name,
                arguments,
                duration_ms,
                trust,
                agent,
            } => {
                if !opts.include_tool_calls {
                    continue;
                }
                // Ein Werkzeugaufruf gehört inhaltlich zu harws Turn, auch wenn
                // er unmittelbar nach einer Nutzernachricht rendert wird.
                if current != Some(Section::Harw) {
                    out.push_str("## harw\n\n");
                    current = Some(Section::Harw);
                }
                render_tool_call_markdown(
                    &mut out,
                    call_id,
                    tool_name,
                    arguments,
                    *duration_ms,
                    trust.as_deref(),
                    agent.as_ref(),
                    opts.max_chars_per_entry,
                );
            }
            ExportEntry::ToolResult {
                call_id,
                tool_name,
                result,
                status,
                error,
                duration_ms,
                trust,
                agent,
            } => {
                if !opts.include_tool_calls {
                    continue;
                }
                if current != Some(Section::Harw) {
                    out.push_str("## harw\n\n");
                    current = Some(Section::Harw);
                }
                render_tool_result_markdown(
                    &mut out,
                    call_id,
                    tool_name.as_deref(),
                    result,
                    *status,
                    error.as_deref(),
                    *duration_ms,
                    trust.as_deref(),
                    agent.as_ref(),
                    opts.max_chars_per_entry,
                );
            }
            ExportEntry::Reasoning(text) => {
                if !opts.include_reasoning {
                    continue;
                }
                if current != Some(Section::Harw) {
                    out.push_str("## harw\n\n");
                    current = Some(Section::Harw);
                }
                let sanitized = sanitize_display(&redact_text(text));
                for line in sanitized.lines() {
                    out.push_str("> ");
                    out.push_str(line);
                    out.push('\n');
                }
                out.push('\n');
            }
            ExportEntry::Error(error) => render_error_markdown(&mut out, error),
            ExportEntry::Plan(plan) => render_plan_markdown(&mut out, plan),
            ExportEntry::Agent(agent) => render_agent_markdown(&mut out, agent),
        }
    }

    let mut result = out.trim_end_matches('\n').to_owned();
    result.push('\n');

    if let Some(max_chars) = opts.max_chars {
        result = truncate_markdown(&result, max_chars);
    }

    result
}

/// Rendert dieselbe geordnete Ereignisliste als versioniertes JSON.
///
/// Die `events`-Array-Reihenfolge ist der einzige maßgebliche Verlauf. Die
/// Markdown- und JSON-Renderer filtern lediglich über dieselben Optionen;
/// insbesondere werden ToolCall und ToolResult nie zusammengeführt.
#[must_use]
pub fn render_json(meta: &ExportMeta, entries: &[ExportEntry], opts: &ExportOptions) -> String {
    render_json_with_extensions(meta, entries, opts, &ExportMetaExtensions::new())
}

/// JSON-Export mit zusätzlichen Sessionmetadaten.
#[must_use]
pub fn render_json_with_extensions(
    meta: &ExportMeta,
    entries: &[ExportEntry],
    opts: &ExportOptions,
    extensions: &ExportMetaExtensions,
) -> String {
    let mut document = Map::new();
    document.insert("schema_version".to_owned(), json!(1));
    document.insert("meta".to_owned(), metadata_value(meta, extensions));
    let events = entries
        .iter()
        .filter_map(|entry| entry_to_json(entry, opts))
        .collect::<Vec<_>>();
    document.insert("events".to_owned(), Value::Array(events));
    // A JSON Value can always be serialized by serde_json. Keeping this
    // function infallible makes it symmetrical with render_markdown.
    serde_json::to_string_pretty(&Value::Object(document))
        .unwrap_or_else(|_| "{\"schema_version\":1,\"meta\":{},\"events\":[]}".to_owned())
}

fn display_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".to_owned())
}

/// Formatiert einen Unix-Sekunden-Zeitstempel als lesbares Datum mit
/// lokalem UTC-Offset.
///
/// # Beschreibung
/// Wandelt `unix_secs` über [`jiff::Timestamp::from_second`] und
/// [`jiff::Timestamp::to_zoned`] (Systemzeitzone,
/// [`jiff::tz::TimeZone::system`]) in ein lokal aufgeschlüsseltes Datum um
/// und formatiert es im Stil `%Y-%m-%d %H:%M:%S %:z`. Damit zeigt der Export
/// ein lesbares Datum statt der rohen Sekundenzahl (`app.rs`s
/// `export_timestamp_now()` liefert Unix-Sekunden). Schlägt die Umwandlung
/// fehl (Wert außerhalb des darstellbaren Bereichs), wird `unix_secs`
/// unverändert als Dezimalstring zurückgegeben, damit der Export nie
/// abbricht.
///
/// # Argumente
/// - `unix_secs` (`i64`): Unix-Zeitstempel in Sekunden.
///
/// # Rückgabe
/// Lesbares Datum, z. B. `2026-09-14 07:30:00 +02:00`, oder `unix_secs` als
/// Dezimalstring bei einem ungültigen Zeitstempel.
///
/// # Beispiele
/// ```ignore
/// use harw_tui::export::format_export_timestamp;
///
/// let formatted = format_export_timestamp(1_700_000_000);
/// assert!(formatted.contains("2023"));
/// ```
#[must_use]
pub fn format_export_timestamp(unix_secs: i64) -> String {
    match jiff::Timestamp::from_second(unix_secs) {
        Ok(timestamp) => {
            let zoned = timestamp.to_zoned(jiff::tz::TimeZone::system());
            zoned.strftime("%Y-%m-%d %H:%M:%S %:z").to_string()
        }
        Err(_) => unix_secs.to_string(),
    }
}

/// Formatiert `text` für die Kopfzeile: ist `text` rein numerisch (ein
/// Unix-Sekunden-Zeitstempel als Dezimalstring), wird er über
/// [`format_export_timestamp`] lesbar gemacht. Jeder andere Text (z. B.
/// bereits vorformatierte Daten aus `app.rs`) bleibt unverändert, damit der
/// Fix auch ohne eine parallele `app.rs`-Änderung wirkt.
fn format_started_at_for_display(text: &str) -> String {
    match text.parse::<i64>() {
        Ok(unix_secs) => format_export_timestamp(unix_secs),
        Err(_) => text.to_owned(),
    }
}

fn optional_string(value: Option<&str>) -> Value {
    value
        .map(|text| Value::String(redact_text(text)))
        .unwrap_or(Value::Null)
}

fn agent_value(agent: Option<&ExportAgentRef>) -> Value {
    let Some(agent) = agent else {
        return Value::Null;
    };
    json!({
        "agent_id": redact_text(&agent.agent_id),
        "role": optional_string(agent.role.as_deref()),
        "parent_id": optional_string(agent.parent_id.as_deref()),
    })
}

fn metadata_value(meta: &ExportMeta, extensions: &ExportMetaExtensions) -> Value {
    let mut metadata = Map::new();
    metadata.insert(
        "title".to_owned(),
        meta.title
            .as_deref()
            .map(redact_text)
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    metadata.insert(
        "session_id".to_owned(),
        Value::String(redact_text(&meta.session_id)),
    );
    metadata.insert(
        "started_at".to_owned(),
        optional_string(meta.started_at.as_deref()),
    );
    metadata.insert("cwd".to_owned(), optional_string(meta.cwd.as_deref()));
    metadata.insert("model".to_owned(), optional_string(meta.model.as_deref()));
    for (key, value) in extensions {
        if !is_system_prompt_key(key) {
            metadata.insert(redact_text(key), redact_json_value(value));
        }
    }
    Value::Object(metadata)
}

fn entry_to_json(entry: &ExportEntry, opts: &ExportOptions) -> Option<Value> {
    Some(match entry {
        ExportEntry::User(text) => json!({
            "type": "user",
            "text": redact_text(text),
        }),
        ExportEntry::Assistant(text) => json!({
            "type": "assistant",
            "text": redact_text(text),
        }),
        ExportEntry::System(text) => json!({
            "type": "system",
            "text": redact_text(text),
        }),
        ExportEntry::Notice(text) => json!({
            "type": "notice",
            "text": redact_text(text),
        }),
        ExportEntry::Tool { label, summary } => {
            if !opts.include_tool_calls {
                return None;
            }
            json!({
                "type": "tool",
                "label": redact_text(label),
                "summary": summary.as_deref().map(redact_text),
            })
        }
        ExportEntry::ToolCall {
            call_id,
            tool_name,
            arguments,
            duration_ms,
            trust,
            agent,
        } => {
            if !opts.include_tool_calls {
                return None;
            }
            json!({
                "type": "tool_call",
                "call_id": redact_text(call_id),
                "tool_name": redact_text(tool_name),
                "arguments": redact_json_value(arguments),
                "status": "requested",
                "duration_ms": duration_ms,
                "trust": optional_string(trust.as_deref()),
                "agent": agent_value(agent.as_ref()),
            })
        }
        ExportEntry::ToolResult {
            call_id,
            tool_name,
            result,
            status,
            error,
            duration_ms,
            trust,
            agent,
        } => {
            if !opts.include_tool_calls {
                return None;
            }
            json!({
                "type": "tool_result",
                "call_id": redact_text(call_id),
                "tool_name": optional_string(tool_name.as_deref()),
                "result": redact_json_value(result),
                "status": status.as_str(),
                "success": matches!(*status, ExportStatus::Success),
                "error": error.as_deref().map(redact_text),
                "duration_ms": duration_ms,
                "trust": optional_string(trust.as_deref()),
                "agent": agent_value(agent.as_ref()),
            })
        }
        ExportEntry::Reasoning(text) => {
            if !opts.include_reasoning {
                return None;
            }
            json!({
                "type": "reasoning_summary",
                "summary": redact_text(text),
            })
        }
        ExportEntry::Error(error) => json!({
            "type": "error",
            "code": optional_string(error.code.as_deref()),
            "message": redact_text(&error.message),
            "details": error.details.as_ref().map(redact_json_value),
            "agent": agent_value(error.agent.as_ref()),
        }),
        ExportEntry::Plan(plan) => json!({
            "type": "plan",
            "plan_id": optional_string(plan.plan_id.as_deref()),
            "revision": plan.revision,
            "summary": redact_text(&plan.summary),
            "steps": plan.steps.iter().map(|step| redact_text(step)).collect::<Vec<_>>(),
            "status": optional_string(plan.status.as_deref()),
            "agent": agent_value(plan.agent.as_ref()),
        }),
        ExportEntry::Agent(agent) => json!({
            "type": "agent",
            "agent_id": redact_text(&agent.agent_id),
            "role": optional_string(agent.role.as_deref()),
            "parent_id": optional_string(agent.parent_id.as_deref()),
            "status": optional_string(agent.status.as_deref()),
            "summary": agent.summary.as_deref().map(redact_text),
        }),
    })
}

/// Trennt einen (bereits redigierten) JSON-Wert in eine kompakte Struktur
/// ohne mehrzeilige String-Blätter und eine geordnete Liste ausgelagerter
/// Freitextblöcke.
///
/// # Beschreibung
/// Rekursiert durch Objekte und Arrays. Ein String-Blatt, das selbst
/// gültiges JSON ist, wird geparst und an derselben Stelle eingebettet, so
/// dass es beim finalen Pretty-Print verschachtelt erscheint. Ein
/// String-Blatt mit einem echten Zeilenumbruch (`\n`) wird aus der Struktur
/// entfernt (Objektschlüssel entfallen ersatzlos, Array-Positionen werden zu
/// `null`) und stattdessen unter seinem Feldpfad in `text_blocks` gesammelt
/// — genau der Fall, der zuvor als eine einzige, bis zu 70.000 Zeichen lange
/// escapte JSON-Zeile im Export landete (insbesondere ein Feld `value`).
/// Kurze, einzeilige Strings bleiben unverändert im JSON-Block.
///
/// # Argumente
/// - `value` (`&Value`): der zu trennende, bereits redigierte Wert.
/// - `path` (`&str`): Feldpfad des aktuellen Werts (leer an der Wurzel).
/// - `text_blocks` (`&mut Vec<(String, String)>`): Sammelziel für
///   `(Feldpfad, Text)`-Paare ausgelagerter mehrzeiliger Strings.
///
/// # Rückgabe
/// `Some(Value)` mit der reduzierten Struktur, oder `None`, wenn der Wert an
/// dieser Position vollständig in `text_blocks` ausgelagert wurde (nur bei
/// einem String-Blatt möglich).
fn split_json_for_block(
    value: &Value,
    path: &str,
    text_blocks: &mut Vec<(String, String)>,
) -> Option<Value> {
    match value {
        Value::Object(object) => {
            let mut reduced = Map::new();
            for (key, child) in object {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                if let Some(child_value) = split_json_for_block(child, &child_path, text_blocks) {
                    reduced.insert(key.clone(), child_value);
                }
            }
            Some(Value::Object(reduced))
        }
        Value::Array(items) => {
            let reduced = items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    let child_path = format!("{path}[{index}]");
                    // Arrays behalten ihre Indizes; ausgelagerte Einträge
                    // werden zu `null`, damit die Position erhalten bleibt.
                    split_json_for_block(item, &child_path, text_blocks).unwrap_or(Value::Null)
                })
                .collect();
            Some(Value::Array(reduced))
        }
        Value::String(text) => {
            if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                Some(parsed)
            } else if text.contains('\n') {
                let label = if path.is_empty() {
                    "value".to_owned()
                } else {
                    path.to_owned()
                };
                text_blocks.push((label, text.to_owned()));
                None
            } else {
                Some(Value::String(text.to_owned()))
            }
        }
        other => Some(other.clone()),
    }
}

/// Kürzt `text` auf höchstens `max_chars` Unicode-Zeichen und hängt einen
/// Marker mit der Anzahl entfernter Zeichen an.
///
/// # Beschreibung
/// Schneidet wie [`truncate_markdown`] an einer Zeichen- (nicht Byte-)
/// Grenze, damit mehrbytige UTF-8-Sequenzen niemals mittendrin getrennt
/// werden. Anders als [`truncate_markdown`] gilt die Grenze pro Block
/// (JSON- oder Text-Block eines einzelnen ToolCall-/ToolResult-Eintrags),
/// nicht für das gesamte Dokument, und der Marker nennt die genaue Anzahl
/// gekürzter Zeichen.
///
/// # Argumente
/// - `text` (`&str`): der zu kürzende Blockinhalt.
/// - `max_chars` (`usize`): maximale Anzahl Unicode-Zeichen vor dem Marker.
///
/// # Rückgabe
/// Gekürzter Text mit angehängtem `_[gekürzt: N Zeichen]_`, oder der
/// unveränderte Text, wenn keine Kürzung nötig war.
fn truncate_block(text: &str, max_chars: usize) -> String {
    let total_chars = text.chars().count();
    if total_chars <= max_chars {
        return text.to_owned();
    }
    let cut_byte = text
        .char_indices()
        .nth(max_chars)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len());
    let removed = total_chars - max_chars;
    let mut truncated = text[..cut_byte].to_owned();
    truncated.push_str(&format!("\n\n_[gekürzt: {removed} Zeichen]_\n"));
    truncated
}

/// Wendet die Pro-Block-Kappung von [`ExportOptions::max_chars_per_entry`] an.
fn apply_block_cap(text: &str, max_chars_per_entry: Option<usize>) -> String {
    match max_chars_per_entry {
        Some(max_chars) => truncate_block(text, max_chars),
        None => text.to_owned(),
    }
}

/// Rendert einen (bereits redigierten) JSON-Wert lesbar: ein kompakter
/// ```` ```json ````-Block für die Struktur, gefolgt von je einem eigenen
/// ```` ```text ````-Block je mehrzeiligem String-Blatt.
///
/// # Beschreibung
/// Ersetzt `display_json` an den Stellen, an denen ToolCall-Argumente und
/// ToolResult-Ergebnisse gerendert werden. Nutzt [`split_json_for_block`],
/// um String-Blätter, die selbst JSON sind, verschachtelt einzubetten und
/// mehrzeilige Freitext-Strings (insbesondere ein Feld `value`) mit echten
/// Zeilenumbrüchen in einem separaten Block auszugeben, statt sie als eine
/// einzige escapte JSON-Zeile darzustellen. Jeder Block wird unabhängig über
/// `max_chars_per_entry` gekappt ([`apply_block_cap`]). Ist die gesamte
/// Wurzel ein einzelner mehrzeiliger String (kein Objekt/Array), entfällt
/// der JSON-Block, da er sonst nur `null` zeigen würde.
///
/// # Argumente
/// - `out` (`&mut String`): Ausgabepuffer.
/// - `value` (`&Value`): bereits redigierter JSON-Wert (siehe [`redact_json_value`]).
/// - `max_chars_per_entry` (`Option<usize>`): Pro-Block-Obergrenze, siehe
///   [`ExportOptions::max_chars_per_entry`].
fn render_json_block(out: &mut String, value: &Value, max_chars_per_entry: Option<usize>) {
    let mut text_blocks = Vec::new();
    let reduced = split_json_for_block(value, "", &mut text_blocks);

    if let Some(json_value) = reduced {
        out.push_str("```json\n");
        out.push_str(&apply_block_cap(
            &display_json(&json_value),
            max_chars_per_entry,
        ));
        out.push_str("\n```\n");
    }

    for (label, text) in &text_blocks {
        out.push_str("  - Text (");
        out.push_str(&sanitize_inline(&redact_text(label)));
        out.push_str("):\n\n```text\n");
        out.push_str(&apply_block_cap(
            &sanitize_display(text),
            max_chars_per_entry,
        ));
        out.push_str("\n```\n");
    }
}

// 8 bzw. 10 Parameter statt eines Parameter-Structs: beide Funktionen werden
// ausschließlich hier in `render_markdown_with_extensions` aufgerufen, eine
// Struct-Extraktion für einen einzigen Aufrufer bringt keinen Nutzen —
// daher gezielt unterdrückt statt umgebaut.
#[allow(clippy::too_many_arguments)]
fn render_tool_call_markdown(
    out: &mut String,
    call_id: &str,
    tool_name: &str,
    arguments: &Value,
    duration_ms: Option<u64>,
    trust: Option<&str>,
    agent: Option<&ExportAgentRef>,
    max_chars_per_entry: Option<usize>,
) {
    out.push_str("- ⚙ **ToolCall** ");
    out.push_str(&sanitize_inline(&redact_text(tool_name)));
    out.push_str(" (`");
    out.push_str(&sanitize_inline(&redact_text(call_id)));
    out.push_str("`)\n  - Status: angefordert\n  - Argumente:\n\n");
    render_json_block(out, &redact_json_value(arguments), max_chars_per_entry);
    render_tool_metadata_markdown(out, duration_ms, trust, agent);
    out.push('\n');
}

#[allow(clippy::too_many_arguments)]
fn render_tool_result_markdown(
    out: &mut String,
    call_id: &str,
    tool_name: Option<&str>,
    result: &Value,
    status: ExportStatus,
    error: Option<&str>,
    duration_ms: Option<u64>,
    trust: Option<&str>,
    agent: Option<&ExportAgentRef>,
    max_chars_per_entry: Option<usize>,
) {
    out.push_str("- ⚙ **ToolResult**");
    if let Some(tool_name) = tool_name {
        out.push(' ');
        out.push_str(&sanitize_inline(&redact_text(tool_name)));
    }
    out.push_str(" (`");
    out.push_str(&sanitize_inline(&redact_text(call_id)));
    out.push_str("`)\n  - Status: ");
    out.push_str(status.as_str());
    if let Some(error) = error {
        out.push_str(" (");
        out.push_str(&sanitize_inline(&redact_text(error)));
        out.push(')');
    }
    out.push_str("\n  - Resultat:\n\n");
    render_json_block(out, &redact_json_value(result), max_chars_per_entry);
    render_tool_metadata_markdown(out, duration_ms, trust, agent);
    out.push('\n');
}

fn render_tool_metadata_markdown(
    out: &mut String,
    duration_ms: Option<u64>,
    trust: Option<&str>,
    agent: Option<&ExportAgentRef>,
) {
    if let Some(duration_ms) = duration_ms {
        out.push_str("  - Dauer: ");
        out.push_str(&duration_ms.to_string());
        out.push_str(" ms\n");
    }
    if let Some(trust) = trust.filter(|trust| *trust != DEFAULT_TRUST) {
        out.push_str("  - Trust: ");
        out.push_str(&sanitize_inline(&redact_text(trust)));
        out.push('\n');
    }
    if let Some(agent) = agent {
        out.push_str("  - Agent: ");
        out.push_str(&sanitize_inline(&redact_text(&agent.agent_id)));
        if let Some(role) = &agent.role {
            out.push_str(" ( ");
            out.push_str(&sanitize_inline(&redact_text(role)));
            out.push_str(" )");
        }
        out.push('\n');
    }
}

fn render_error_markdown(out: &mut String, error: &ExportErrorEntry) {
    out.push_str("- ⚠ **Error**");
    if let Some(code) = &error.code {
        out.push_str(" [");
        out.push_str(&sanitize_inline(&redact_text(code)));
        out.push(']');
    }
    out.push_str(": ");
    out.push_str(&sanitize_inline(&redact_text(&error.message)));
    if let Some(details) = &error.details {
        out.push_str("\n  - Details: ");
        out.push_str(&sanitize_inline(&display_json(&redact_json_value(details))));
    }
    out.push_str("\n\n");
}

fn render_plan_markdown(out: &mut String, plan: &ExportPlanEntry) {
    out.push_str("- **Plan**");
    if let Some(plan_id) = &plan.plan_id {
        out.push(' ');
        out.push_str(&sanitize_inline(&redact_text(plan_id)));
    }
    out.push_str(": ");
    out.push_str(&sanitize_inline(&redact_text(&plan.summary)));
    if let Some(status) = &plan.status {
        out.push_str(" ( ");
        out.push_str(&sanitize_inline(&redact_text(status)));
        out.push_str(" )");
    }
    for (index, step) in plan.steps.iter().enumerate() {
        out.push_str("\n  ");
        out.push_str(&(index + 1).to_string());
        out.push_str(". ");
        out.push_str(&sanitize_inline(&redact_text(step)));
    }
    out.push_str("\n\n");
}

fn render_agent_markdown(out: &mut String, agent: &ExportAgentEntry) {
    out.push_str("- **Agent** ");
    out.push_str(&sanitize_inline(&redact_text(&agent.agent_id)));
    if let Some(role) = &agent.role {
        out.push_str(" ( ");
        out.push_str(&sanitize_inline(&redact_text(role)));
        out.push_str(" )");
    }
    if let Some(status) = &agent.status {
        out.push_str(" — ");
        out.push_str(&sanitize_inline(&redact_text(status)));
    }
    if let Some(summary) = &agent.summary {
        out.push_str(": ");
        out.push_str(&sanitize_inline(&redact_text(summary)));
    }
    out.push_str("\n\n");
}

const REDACTED: &str = "[REDACTED]";

fn normalized_key(key: &str) -> String {
    key.chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_system_prompt_key(key: &str) -> bool {
    let key = normalized_key(key);
    key == "systemprompt" || key == "systemmessage" || key == "systeminstructions"
}

fn is_sensitive_key(key: &str) -> bool {
    let key = normalized_key(key);
    matches!(
        key.as_str(),
        "authorization"
            | "proxyauthorization"
            | "apikey"
            | "accesskey"
            | "accesskeyid"
            | "password"
            | "passwd"
            | "passwort"
            | "secret"
            | "clientsecret"
            | "privatekey"
            | "accesstoken"
            | "refreshtoken"
            | "idtoken"
            | "authtoken"
            | "bearertoken"
            | "sessiontoken"
            | "token"
    ) || key.ends_with("token")
}

/// Redigiert strukturierte Werte zentral, bevor sie irgendeinen Renderer
/// erreichen. Systemprompt-Felder werden vollständig ausgelassen.
/// Runde 5, Teil I: auch vom Kind-Live-Stream (`crate::child_stream`) genutzt.
pub(crate) fn redact_json_value(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut redacted = Map::new();
            for (key, value) in object {
                if is_system_prompt_key(key) {
                    continue;
                }
                redacted.insert(
                    redact_text(key),
                    if is_sensitive_key(key) {
                        Value::String(REDACTED.to_owned())
                    } else {
                        redact_json_value(value)
                    },
                );
            }
            Value::Object(redacted)
        }
        Value::Array(values) => Value::Array(values.iter().map(redact_json_value).collect()),
        Value::String(text) => Value::String(redact_text(text)),
        _ => value.clone(),
    }
}

/// Redigiert typische `key=value`, `key: value` und Authorization-Felder in
/// Freitext. Das deckt auch Legacy-Toollabels und normale Chattexte ab.
/// Runde 5, Teil I: auch vom Kind-Live-Stream (`crate::child_stream`) genutzt.
pub(crate) fn redact_text(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let markers = [
        "authorization",
        "proxy-authorization",
        "api_key",
        "api-key",
        "apikey",
        "access_token",
        "refresh_token",
        "id_token",
        "auth_token",
        "bearer_token",
        "session_token",
        "client_secret",
        "private_key",
        "access-token",
        "refresh-token",
        "token",
        "password",
        "passwd",
        "passwort",
        "secret",
    ];
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    while cursor < text.len() {
        let Some((start, marker)) = next_sensitive_marker(&lower, cursor, &markers) else {
            output.push_str(&text[cursor..]);
            break;
        };
        let Some((value_start, value_end)) = sensitive_value_range(text, start, marker.len())
        else {
            output.push_str(&text[cursor..start + marker.len()]);
            cursor = start + marker.len();
            continue;
        };
        output.push_str(&text[cursor..value_start]);
        output.push_str(REDACTED);
        cursor = value_end;
    }
    output
}

fn next_sensitive_marker<'a>(
    lower: &str,
    from: usize,
    markers: &'a [&'a str],
) -> Option<(usize, &'a str)> {
    markers
        .iter()
        .filter_map(|marker| {
            let start = lower[from..].find(marker).map(|offset| from + offset)?;
            let before_ok = start == 0
                || !lower.as_bytes()[start - 1].is_ascii_alphanumeric()
                    && lower.as_bytes()[start - 1] != b'_';
            Some((start, *marker, before_ok))
        })
        .filter(|(_, _, before_ok)| *before_ok)
        .min_by_key(|(start, _, _)| *start)
        .map(|(start, marker, _)| (start, marker))
}

fn sensitive_value_range(text: &str, start: usize, marker_len: usize) -> Option<(usize, usize)> {
    let bytes = text.as_bytes();
    let mut index = start + marker_len;
    while bytes
        .get(index)
        .is_some_and(|byte| byte.is_ascii_whitespace())
    {
        index += 1;
    }
    if bytes.get(index) == Some(&b'"') || bytes.get(index) == Some(&b'\'') {
        index += 1;
        while bytes
            .get(index)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            index += 1;
        }
    }
    if !matches!(bytes.get(index), Some(b':' | b'=')) {
        return None;
    }
    index += 1;
    while bytes
        .get(index)
        .is_some_and(|byte| byte.is_ascii_whitespace())
    {
        index += 1;
    }
    if bytes.get(index) == Some(&b'"') || bytes.get(index) == Some(&b'\'') {
        let quote = bytes[index];
        let value_start = index + 1;
        let value_end = bytes[value_start..]
            .iter()
            .position(|byte| *byte == quote)
            .map(|offset| value_start + offset)
            .unwrap_or(text.len());
        return Some((value_start, value_end));
    }
    let value_start = index;
    // Authorization-Header enthalten häufig ein Schema (`Bearer`/`Basic`)
    // vor dem eigentlichen Geheimnis; beides muss gemeinsam verschwinden.
    let scan_start = ["bearer ", "basic "]
        .iter()
        .find(|prefix| text[index..].to_ascii_lowercase().starts_with(*prefix))
        .map(|prefix| index + prefix.len())
        .unwrap_or(index);
    let value_end = text[scan_start..]
        .bytes()
        .position(|byte| byte.is_ascii_whitespace() || matches!(byte, b',' | b';' | b']' | b'}'))
        .map(|offset| scan_start + offset)
        .unwrap_or(text.len());
    Some((value_start, value_end))
}

/// Stuft ATX-Überschriften (`#` bis `######`) in `text` um zwei Ebenen
/// herab, gekappt auf höchstens `######`. Zeilen innerhalb eines
/// Fenced-Code-Blocks (```` ``` ```` oder `~~~`) bleiben unverändert.
///
/// # Beschreibung
/// Nutzer- und Assistant-Text dürfen eigene Markdown-Überschriften
/// enthalten (z. B. `# Ergebnis`); ohne Herabstufung würden diese mit den
/// Export-eigenen Ebenen `#`/`##` kollidieren und die Dokumentgliederung
/// verfälschen. Jede erkannte ATX-Überschriftszeile außerhalb eines
/// Fenced-Code-Blocks bekommt zwei zusätzliche `#`; eine bereits auf
/// `######` stehende Überschrift bleibt unverändert (Ebene 6 ist das
/// Markdown-Maximum). Ein Fenced-Code-Block wird an seiner öffnenden
/// Markierung (```` ``` ```` oder `~~~`, führende Leerzeichen erlaubt)
/// erkannt und endet an der nächsten Zeile mit derselben Markierung;
/// innerhalb bleibt jede Zeile — auch eine mit `#` beginnende
/// Shell-Kommentarzeile — unangetastet.
///
/// # Argumente
/// - `text` (`&str`): der zu transformierende Rohtext.
///
/// # Rückgabe
/// Text mit herabgestuften Überschriften; alle sonstigen Zeilen und
/// Zeilenumbrüche bleiben unverändert.
fn demote_markdown_headings(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut fence_marker: Option<&'static str> = None;
    let mut first_line = true;

    for line in text.split('\n') {
        if !first_line {
            out.push('\n');
        }
        first_line = false;

        let trimmed_start = line.trim_start();
        let line_fence = if trimmed_start.starts_with("```") {
            Some("```")
        } else if trimmed_start.starts_with("~~~") {
            Some("~~~")
        } else {
            None
        };

        if let Some(marker) = line_fence {
            match fence_marker {
                Some(active) if active == marker => fence_marker = None,
                Some(_) => {}
                None => fence_marker = Some(marker),
            }
            out.push_str(line);
            continue;
        }

        if fence_marker.is_some() {
            out.push_str(line);
            continue;
        }

        match demote_heading_line(line) {
            Some(demoted) => out.push_str(&demoted),
            None => out.push_str(line),
        }
    }

    out
}

/// Erkennt eine ATX-Überschriftszeile (`^\s*#{1,6}(\s|$)`) und liefert sie
/// mit zwei zusätzlichen `#` zurück, gekappt auf `######`. Liefert `None`,
/// wenn `line` keine ATX-Überschrift ist.
fn demote_heading_line(line: &str) -> Option<String> {
    let leading_ws_len = line.len() - line.trim_start().len();
    let (leading_ws, rest) = line.split_at(leading_ws_len);
    let hashes = rest.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let after_hashes = &rest[hashes..];
    // Eine ATX-Überschrift braucht ein Leerzeichen/Tab (oder Zeilenende)
    // direkt nach den Rauten; sonst ist es z. B. ein `#tag` im Fließtext.
    if !after_hashes.is_empty() && !after_hashes.starts_with(' ') && !after_hashes.starts_with('\t')
    {
        return None;
    }
    let new_level = (hashes + 2).min(6);
    let mut result = String::with_capacity(line.len() + 2);
    result.push_str(leading_ws);
    result.push_str(&"#".repeat(new_level));
    result.push_str(after_hashes);
    Some(result)
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
    default_export_path_with_extension(now, dir, "md")
}

/// Baut den Standard-Exportpfad `<dir>/harw-export-<now>.<extension>`
/// (Runde 6, Teil C: die Endung folgt dem Format, `.json` bei JSON).
///
/// # Argumente
/// - `now` (`&str`): Zeitstempel für den Dateinamen.
/// - `dir` (`&Path`): Zielverzeichnis.
/// - `extension` (`&str`): Dateiendung ohne Punkt, z. B. `md` oder `json`.
///
/// # Rückgabe
/// `<dir>/harw-export-<now>.<extension>`.
#[must_use]
pub fn default_export_path_with_extension(now: &str, dir: &Path, extension: &str) -> PathBuf {
    dir.join(format!("harw-export-{now}.{extension}"))
}

/// Expandiert `~` bzw. `$HOME`/`${HOME}` am Anfang eines Exportpfads
/// (Runde 6, Teil C).
///
/// # Beschreibung
/// Reine Pfadkonstruktion ohne Dateisystemzugriff. Nur ein Präfix, das für
/// sich steht (`~`, `$HOME`) oder von `/` gefolgt wird, zählt; `~nutzer`
/// oder `$HOMEX` bleiben unverändert. Ohne bekanntes Home bleibt der Pfad
/// unverändert. Die Traversal-Prüfung ([`write_export_path`]) gilt danach
/// unverändert, `~/../x` wird also weiterhin abgelehnt.
///
/// # Argumente
/// - `raw` (`&str`): der eingegebene Pfad.
/// - `home` (`Option<&Path>`): das Home-Verzeichnis.
///
/// # Rückgabe
/// Der expandierte Pfad.
#[must_use]
pub fn expand_home_prefix(raw: &str, home: Option<&Path>) -> PathBuf {
    let Some(home) = home else {
        return PathBuf::from(raw);
    };
    for prefix in ["~", "${HOME}", "$HOME"] {
        let Some(rest) = raw.strip_prefix(prefix) else {
            continue;
        };
        if rest.is_empty() {
            return home.to_path_buf();
        }
        if rest.starts_with('/') {
            return home.join(rest.trim_start_matches('/'));
        }
    }
    PathBuf::from(raw)
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
    write_export_path(path, content).map(|_| ())
}

/// Schreibt einen Export und gibt den tatsächlich verwendeten Pfad zurück.
///
/// Das ist die additive Variante zu [`write_export`]. Bei einer Kollision kann
/// der Rückgabepfad daher `-2`, `-3`, … enthalten, ohne die alte
/// `Result<(), _>`-API zu brechen. Die bestehende Traversalprüfung, private
/// atomare Ablage und
/// der Suffix-Kollisionsschutz bleiben unverändert.
pub fn write_export_path(path: &Path, content: &str) -> Result<PathBuf, ExportError> {
    reject_traversal(path)?;
    let target = next_available_path(path)?;
    harw_fsutil::write_atomic(
        &target,
        content.as_bytes(),
        harw_fsutil::AtomicWriteOptions::private(),
    )
    .map_err(ExportError::from)
    .map(|()| target)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

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
    fn test_render_markdown_order_and_headers() -> TestResult {
        let meta = ExportMeta {
            title: Some("Mein Export".to_owned()),
            session_id: "sess-42".to_owned(),
            started_at: Some("2026-09-14T05:30:00".to_owned()),
            cwd: Some("/home/user/projects/harwness".to_owned()),
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
        assert!(out.contains("- **Verzeichnis:** /home/user/projects/harwness"));
        assert!(out.contains("- **Modell:** groq/llama"));

        let du_first = out
            .find("## Du")
            .ok_or(TestError::Missing("erste Du-Überschrift"))?;
        let harw_pos = out
            .find("## harw")
            .ok_or(TestError::Missing("harw-Überschrift"))?;
        let du_second = out
            .rfind("## Du")
            .ok_or(TestError::Missing("zweite Du-Überschrift"))?;
        assert!(du_first < harw_pos, "Frage 1 muss vor der Antwort stehen");
        assert!(harw_pos < du_second, "Antwort muss vor Frage 2 stehen");
        assert!(out.contains("Frage 1"));
        assert!(out.contains("Antwort 1"));
        assert!(out.contains("Frage 2"));
        assert!(out.ends_with('\n'));
        Ok(())
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

    /// Werkzeugaufrufe sind standardmäßig enthalten und können explizit
    /// ausgeblendet werden.
    #[test]
    fn test_render_markdown_tool_entries_default_to_enabled() {
        let entries = vec![ExportEntry::Tool {
            label: "Shell(git status)".to_owned(),
            summary: Some("0 Dateien geändert".to_owned()),
        }];

        let default = render_markdown(&meta_minimal(), &entries, &ExportOptions::default());
        assert!(default.contains("- ⚙ Shell(git status)"));
        assert!(default.contains("0 Dateien geändert"));

        let opts = ExportOptions {
            include_tool_calls: false,
            ..ExportOptions::default()
        };
        let hidden = render_markdown(&meta_minimal(), &entries, &opts);
        assert!(!hidden.contains("⚙"));
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

    /// Strukturierte ToolCall-/ToolResult-Daten behalten ihre Reihenfolge und
    /// werden in Markdown vollständig, aber redigiert dargestellt.
    #[test]
    fn test_structured_tools_are_ordered_and_complete() -> TestResult {
        let entries = vec![
            ExportEntry::ToolCall {
                call_id: "call-7".to_owned(),
                tool_name: "http.request".to_owned(),
                arguments: json!({
                    "url": "https://example.test",
                    "headers": {"Authorization": "Bearer should-not-leak"},
                    "payload": {"answer": 42},
                }),
                duration_ms: None,
                trust: Some("sandbox".to_owned()),
                agent: Some(ExportAgentRef {
                    agent_id: "child-1".to_owned(),
                    role: Some("researcher".to_owned()),
                    parent_id: Some("root".to_owned()),
                }),
            },
            ExportEntry::ToolResult {
                call_id: "call-7".to_owned(),
                tool_name: Some("http.request".to_owned()),
                result: json!({"status": 200, "body": "ok"}),
                status: ExportStatus::Success,
                error: None,
                duration_ms: Some(12),
                trust: Some("sandbox".to_owned()),
                agent: None,
            },
        ];
        let out = render_markdown(&meta_minimal(), &entries, &ExportOptions::default());
        assert!(
            out.find("ToolCall").ok_or(TestError::Missing("ToolCall"))?
                < out
                    .find("ToolResult")
                    .ok_or(TestError::Missing("ToolResult"))?
        );
        assert!(out.contains("call-7"));
        assert!(out.contains("http.request"));
        assert!(out.contains("\"answer\": 42"));
        assert!(out.contains("Dauer: 12 ms"));
        assert!(!out.contains("should-not-leak"));
        Ok(())
    }

    /// JSON enthält versionierte Metadaten sowie getrennte Ereignisse in der
    /// Eingabereihenfolge und exportiert keine Systemprompt-Felder.
    #[test]
    fn test_render_json_is_structured_ordered_and_redacted() -> TestResult {
        let entries = vec![
            ExportEntry::Plan(ExportPlanEntry {
                plan_id: Some("plan-1".to_owned()),
                revision: Some(2),
                summary: "Zwei Schritte".to_owned(),
                steps: vec!["Lesen".to_owned(), "Prüfen".to_owned()],
                status: Some("completed".to_owned()),
                agent: None,
            }),
            ExportEntry::Error(ExportErrorEntry {
                code: Some("E_TOOL".to_owned()),
                message: "Werkzeugfehler".to_owned(),
                details: Some(json!({
                    "api_key": "json-secret",
                    "system_prompt": "do not export",
                })),
                agent: None,
            }),
            ExportEntry::ToolResult {
                call_id: "call-1".to_owned(),
                tool_name: Some("demo".to_owned()),
                result: json!({"password": "secret", "value": true}),
                status: ExportStatus::Error,
                error: Some("Authorization: Bearer text-secret".to_owned()),
                duration_ms: Some(9),
                trust: None,
                agent: None,
            },
        ];
        let value: Value = serde_json::from_str(&render_json(
            &meta_minimal(),
            &entries,
            &ExportOptions::default(),
        ))
        .map_err(ctx("JSON muss gültig sein"))?;
        let events = value["events"]
            .as_array()
            .ok_or(TestError::Missing("events array"))?;
        assert_eq!(events[0]["type"], "plan");
        assert_eq!(events[1]["type"], "error");
        assert_eq!(events[2]["type"], "tool_result");
        assert_eq!(events[2]["status"], "error");
        assert_eq!(events[2]["success"], false);
        let serialized = value.to_string();
        assert!(!serialized.contains("json-secret"));
        assert!(!serialized.contains("text-secret"));
        assert!(!serialized.contains("do not export"));
        assert_eq!(events[2]["result"]["password"], REDACTED);
        Ok(())
    }

    /// Redaction gilt auch für Legacy-Freitext und typische Header-/Env-Formate.
    #[test]
    fn test_redaction_covers_typical_secret_fields() {
        let text = "Authorization: Bearer abc API_KEY=def password='ghi'";
        let redacted = redact_text(text);
        assert!(!redacted.contains("abc"));
        assert!(!redacted.contains("def"));
        assert!(!redacted.contains("ghi"));
        assert!(redacted.contains("[REDACTED]"));
        let value = redact_json_value(&json!({
            "token": "one",
            "nested": {"api-key": "two", "normal": "three"},
            "system_instructions": "omit",
        }));
        assert_eq!(value["token"], REDACTED);
        assert_eq!(value["nested"]["api-key"], REDACTED);
        assert!(value.get("system_instructions").is_none());
        assert_eq!(value["nested"]["normal"], "three");
    }

    /// Runde 6, Teil C: `~` und `$HOME` werden nur als eigenständiges
    /// Präfix expandiert; Traversal bleibt abgelehnt.
    #[test]
    fn test_expand_home_prefix() -> TestResult {
        let home = Path::new("/home/user");
        assert_eq!(
            expand_home_prefix("~", Some(home)),
            PathBuf::from("/home/user")
        );
        assert_eq!(
            expand_home_prefix("~/a/b.md", Some(home)),
            PathBuf::from("/home/user/a/b.md")
        );
        assert_eq!(
            expand_home_prefix("$HOME/x.json", Some(home)),
            PathBuf::from("/home/user/x.json")
        );
        assert_eq!(
            expand_home_prefix("${HOME}/x.md", Some(home)),
            PathBuf::from("/home/user/x.md")
        );
        assert_eq!(
            expand_home_prefix("~bob/x", Some(home)),
            PathBuf::from("~bob/x")
        );
        assert_eq!(
            expand_home_prefix("$HOMEX/x", Some(home)),
            PathBuf::from("$HOMEX/x")
        );
        assert_eq!(
            expand_home_prefix("rel/x.md", Some(home)),
            PathBuf::from("rel/x.md")
        );
        assert_eq!(expand_home_prefix("~/x.md", None), PathBuf::from("~/x.md"));
        let escaped = expand_home_prefix("~/../etc/x.md", Some(home));
        assert!(matches!(
            write_export_path(&escaped, "x"),
            Err(ExportError::PathRejected(_))
        ));
        Ok(())
    }

    /// Runde 6, Teil C: die Standardendung folgt dem Format.
    #[test]
    fn test_default_export_path_with_json_extension() {
        let path = default_export_path_with_extension("5", Path::new("/tmp"), "json");
        assert_eq!(path, PathBuf::from("/tmp/harw-export-5.json"));
        assert_eq!(
            default_export_path("5", Path::new("/tmp")),
            PathBuf::from("/tmp/harw-export-5.md")
        );
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
    fn test_write_export_suffixes_when_file_exists() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("harw-export-2026-09-14T05-30-00.md");

        write_export(&path, "erster Inhalt\n").map_err(ctx("erster Export sollte klappen"))?;
        assert!(path.exists());
        assert_eq!(
            std::fs::read_to_string(&path).map_err(ctx("lesen"))?,
            "erster Inhalt\n"
        );

        write_export(&path, "zweiter Inhalt\n").map_err(ctx("zweiter Export sollte klappen"))?;
        let suffixed = dir.path().join("harw-export-2026-09-14T05-30-00-2.md");
        assert!(suffixed.exists(), "Suffix -2 muss angelegt werden");
        assert_eq!(
            std::fs::read_to_string(&suffixed).map_err(ctx("lesen"))?,
            "zweiter Inhalt\n"
        );
        // Die ursprüngliche Datei bleibt unverändert.
        assert_eq!(
            std::fs::read_to_string(&path).map_err(ctx("lesen"))?,
            "erster Inhalt\n"
        );
        Ok(())
    }

    /// Ein Zielpfad mit `..`-Komponente wird abgelehnt.
    #[test]
    fn test_write_export_refuses_traversal() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("../escape.md");

        let Err(err) = write_export(&path, "inhalt\n") else {
            return Err(TestError::Unexpected(
                "Traversal muss abgelehnt werden: Err erwartet".into(),
            ));
        };
        assert!(matches!(err, ExportError::PathRejected(_)));
        Ok(())
    }

    // -----------------------------------------------------------------
    // A2: schließender Backtick der ToolResult-Zeile
    // -----------------------------------------------------------------

    /// A2-Regression: Die Call-ID der ToolResult-Zeile ist beidseitig in
    /// Backticks eingeschlossen, exakt wie bei ToolCall.
    #[test]
    fn test_render_tool_result_closes_call_id_backtick() {
        let entries = vec![ExportEntry::ToolResult {
            call_id: "call-7".to_owned(),
            tool_name: Some("demo".to_owned()),
            result: json!({"ok": true}),
            status: ExportStatus::Success,
            error: None,
            duration_ms: None,
            trust: None,
            agent: None,
        }];
        let out = render_markdown(&meta_minimal(), &entries, &ExportOptions::default());
        assert!(out.contains("(`call-7`)\n  - Status: success"));
    }

    // -----------------------------------------------------------------
    // A3: render_json_block
    // -----------------------------------------------------------------

    /// Ein String-Blatt, das selbst gültiges JSON ist, wird verschachtelt
    /// pretty ausgegeben statt als escapte Zeile.
    #[test]
    fn test_render_json_block_parses_json_string_leaf_as_nested_pretty_json() {
        let entries = vec![ExportEntry::ToolResult {
            call_id: "call-9".to_owned(),
            tool_name: Some("demo".to_owned()),
            result: json!({"value": "{\"inner\":42}"}),
            status: ExportStatus::Success,
            error: None,
            duration_ms: None,
            trust: None,
            agent: None,
        }];
        let out = render_markdown(&meta_minimal(), &entries, &ExportOptions::default());
        assert!(out.contains("\"inner\": 42"));
        assert!(!out.contains("\\\"inner\\\""));
        assert!(!out.contains("```text"));
    }

    /// Ein String-Blatt mit echten Zeilenumbrüchen (insb. `value`) landet in
    /// einem eigenen ```text-Block mit erhaltenen Zeilenumbrüchen; der
    /// json-Block enthält den großen String nicht mehr.
    #[test]
    fn test_render_json_block_extracts_multiline_string_into_text_block() -> TestResult {
        let entries = vec![ExportEntry::ToolResult {
            call_id: "call-10".to_owned(),
            tool_name: Some("demo".to_owned()),
            result: json!({"status": "ok", "value": "Zeile 1\nZeile 2\nZeile 3"}),
            status: ExportStatus::Success,
            error: None,
            duration_ms: None,
            trust: None,
            agent: None,
        }];
        let out = render_markdown(&meta_minimal(), &entries, &ExportOptions::default());
        assert!(out.contains("  - Text (value):\n\n```text\nZeile 1\nZeile 2\nZeile 3"));

        let json_start = out
            .find("```json\n")
            .ok_or(TestError::Missing("json-Block"))?;
        let json_end = out[json_start..]
            .find("\n```\n")
            .map(|offset| json_start + offset)
            .ok_or(TestError::Missing("json-Block-Ende"))?;
        let json_block = &out[json_start..json_end];
        assert!(!json_block.contains("Zeile 1"));
        assert!(json_block.contains("\"status\""));
        Ok(())
    }

    /// Jeder Block wird unabhängig über `max_chars_per_entry` gekappt, schneidet
    /// an einer Zeichen- (nicht Byte-)Grenze und hängt den Marker mit der
    /// genauen Anzahl gekürzter Zeichen an.
    #[test]
    fn test_render_json_block_truncates_per_entry_with_marker_and_multibyte_boundary() {
        // 20 mehrbytige "ü"-Zeichen (je 2 Bytes UTF-8): ein Byte-Schnitt würde panicen.
        let long_text = format!("Start\n{}\nEnde", "ü".repeat(20));
        assert_eq!(long_text.chars().count(), 31);

        let entries = vec![ExportEntry::ToolResult {
            call_id: "call-11".to_owned(),
            tool_name: Some("demo".to_owned()),
            result: json!({"value": long_text}),
            status: ExportStatus::Success,
            error: None,
            duration_ms: None,
            trust: None,
            agent: None,
        }];
        let opts = ExportOptions {
            max_chars_per_entry: Some(10),
            ..ExportOptions::default()
        };
        let out = render_markdown(&meta_minimal(), &entries, &opts);

        assert!(out.contains("_[gekürzt: 21 Zeichen]_"));
        assert!(!out.contains('\u{FFFD}'));
    }

    /// `ExportOptions::default()` kappt Blöcke standardmäßig bei 4000 Zeichen.
    #[test]
    fn test_export_options_default_max_chars_per_entry_is_4000() {
        assert_eq!(ExportOptions::default().max_chars_per_entry, Some(4000));
    }

    // -----------------------------------------------------------------
    // A4: demote_markdown_headings
    // -----------------------------------------------------------------

    /// ATX-Überschriften in Nutzer-/Assistant-Text werden um zwei Ebenen
    /// herabgestuft, gekappt auf `######`.
    #[test]
    fn test_demote_markdown_headings_shifts_atx_headings_by_two_levels() {
        let entries = vec![ExportEntry::Assistant(
            "# Titel\n## Unterabschnitt\n###### Bereits Maximal".to_owned(),
        )];
        let out = render_markdown(&meta_minimal(), &entries, &ExportOptions::default());
        assert!(out.contains("### Titel\n#### Unterabschnitt\n###### Bereits Maximal"));
    }

    /// Fenced-Code-Blöcke (inkl. `#`-Zeilen darin, z. B. Shell-Kommentare)
    /// bleiben beim Herabstufen unangetastet.
    #[test]
    fn test_demote_markdown_headings_skips_fenced_code_blocks() {
        let entries = vec![ExportEntry::Assistant(
            "# Titel\n```bash\n# das ist ein Shell-Kommentar\necho hi\n```\n# Nach dem Block"
                .to_owned(),
        )];
        let out = render_markdown(&meta_minimal(), &entries, &ExportOptions::default());
        assert!(out.contains("```bash\n# das ist ein Shell-Kommentar\necho hi\n```"));
        assert!(out.contains("### Titel"));
        assert!(out.contains("### Nach dem Block"));
    }

    // -----------------------------------------------------------------
    // A5: Abschnittszuordnung für ToolCall/ToolResult/Reasoning
    // -----------------------------------------------------------------

    /// Export 429: eingespeiste Hintergrund-Meldungen stehen nicht unter
    /// `## Du`, sondern unter einer eigenen Überschrift; JSON typisiert sie.
    #[test]
    fn test_notice_entries_render_under_their_own_heading() -> TestResult {
        let entries = vec![
            ExportEntry::User("Frage".to_owned()),
            ExportEntry::Assistant("Gestartet.".to_owned()),
            ExportEntry::Notice("[Hintergrund-Agent x y fehlgeschlagen nach 81 s]".to_owned()),
            ExportEntry::Assistant("Zusammenfassung".to_owned()),
        ];
        let out = render_markdown(&meta_minimal(), &entries, &ExportOptions::default());
        assert_eq!(out.matches("## Du\n\n").count(), 1, "{out}");
        let notice_heading = out
            .find("## Hintergrund-Agent\n\n[Hintergrund-Agent x y")
            .ok_or(TestError::Missing("Hintergrund-Agent-Überschrift"))?;
        let du = out.find("## Du").ok_or(TestError::Missing("Du"))?;
        assert!(du < notice_heading);
        let json = render_json(&meta_minimal(), &entries, &ExportOptions::default());
        assert!(json.contains("\"type\": \"notice\""), "{json}");
        Ok(())
    }

    /// Die Vorgabe `untrusted` wird im Markdown nicht bei jedem Werkzeug
    /// wiederholt; abweichende Stufen und das JSON bleiben vollständig.
    #[test]
    fn test_markdown_omits_default_trust_but_keeps_other_levels() {
        let result = |trust: &str| ExportEntry::ToolResult {
            call_id: "call-1".to_owned(),
            tool_name: Some("demo".to_owned()),
            result: json!({}),
            status: ExportStatus::Success,
            error: None,
            duration_ms: Some(1),
            trust: Some(trust.to_owned()),
            agent: None,
        };
        let default = render_markdown(
            &meta_minimal(),
            &[result("untrusted")],
            &ExportOptions::default(),
        );
        assert!(!default.contains("Trust:"), "{default}");
        let runtime = render_markdown(
            &meta_minimal(),
            &[result("runtime")],
            &ExportOptions::default(),
        );
        assert!(runtime.contains("  - Trust: runtime"), "{runtime}");
        let json = render_json(
            &meta_minimal(),
            &[result("untrusted")],
            &ExportOptions::default(),
        );
        assert!(json.contains("\"trust\": \"untrusted\""), "{json}");
    }

    /// ToolCall und ToolResult nach einer Nutzernachricht schalten auf die
    /// `## harw`-Überschrift um, statt unter `## Du` zu bleiben.
    #[test]
    fn test_tool_entries_after_user_message_switch_to_harw_section() -> TestResult {
        let entries = vec![
            ExportEntry::User("Frage".to_owned()),
            ExportEntry::ToolCall {
                call_id: "call-1".to_owned(),
                tool_name: "demo".to_owned(),
                arguments: json!({}),
                duration_ms: None,
                trust: None,
                agent: None,
            },
            ExportEntry::ToolResult {
                call_id: "call-1".to_owned(),
                tool_name: Some("demo".to_owned()),
                result: json!({}),
                status: ExportStatus::Success,
                error: None,
                duration_ms: None,
                trust: None,
                agent: None,
            },
        ];
        let out = render_markdown(&meta_minimal(), &entries, &ExportOptions::default());

        let du_pos = out
            .find("## Du\n\n")
            .ok_or(TestError::Missing("Du-Überschrift"))?;
        let harw_pos = out
            .find("## harw\n\n")
            .ok_or(TestError::Missing("harw-Überschrift"))?;
        let tool_call_pos = out.find("ToolCall").ok_or(TestError::Missing("ToolCall"))?;
        assert!(
            du_pos < harw_pos,
            "Frage muss vor der harw-Überschrift stehen"
        );
        assert!(harw_pos < tool_call_pos, "ToolCall muss unter harw stehen");
        // ToolCall und ToolResult teilen sich dieselbe Überschrift.
        assert_eq!(out.matches("## harw\n\n").count(), 1);
        Ok(())
    }

    /// Ein Reasoning-Eintrag nach einer Nutzernachricht schaltet ebenfalls auf
    /// `## harw` um.
    #[test]
    fn test_reasoning_entry_after_user_message_switches_to_harw_section() -> TestResult {
        let entries = vec![
            ExportEntry::User("Frage".to_owned()),
            ExportEntry::Reasoning("weil X gilt".to_owned()),
        ];
        let opts = ExportOptions {
            include_reasoning: true,
            ..ExportOptions::default()
        };
        let out = render_markdown(&meta_minimal(), &entries, &opts);

        let du_pos = out
            .find("## Du")
            .ok_or(TestError::Missing("Du-Überschrift"))?;
        let harw_pos = out
            .find("## harw")
            .ok_or(TestError::Missing("harw-Überschrift"))?;
        let reasoning_pos = out
            .find("weil X gilt")
            .ok_or(TestError::Missing("Reasoning-Text"))?;
        assert!(du_pos < harw_pos);
        assert!(harw_pos < reasoning_pos);
        Ok(())
    }

    // -----------------------------------------------------------------
    // Datum-Helfer
    // -----------------------------------------------------------------

    /// Ein bekannter Unix-Zeitstempel wird lesbar mit Uhrzeit und Offset
    /// formatiert, nicht als rohe Sekundenzahl.
    #[test]
    fn test_format_export_timestamp_formats_known_unix_seconds() {
        // 1_700_000_000 = 2023-11-14T22:13:20Z; das Jahr bleibt in jeder
        // Zeitzone (±14h) unverändert 2023.
        let formatted = format_export_timestamp(1_700_000_000);
        assert!(formatted.contains("2023"));
        assert!(formatted.contains(':'));
        assert_ne!(formatted, "1700000000");
    }

    /// Ein Zeitstempel außerhalb des darstellbaren Bereichs fällt auf die
    /// unveränderte Zahl zurück, statt abzubrechen.
    #[test]
    fn test_format_export_timestamp_falls_back_to_number_on_invalid_input() {
        let formatted = format_export_timestamp(i64::MAX);
        assert_eq!(formatted, i64::MAX.to_string());
    }

    /// Ist `meta.started_at` rein numerisch, formatiert der Renderer es über
    /// [`format_export_timestamp`] lesbar — auch ohne eine `app.rs`-Änderung.
    #[test]
    fn test_render_markdown_formats_numeric_started_at_as_readable_date() {
        let meta = ExportMeta {
            title: None,
            session_id: "sess-1".to_owned(),
            started_at: Some("1700000000".to_owned()),
            cwd: None,
            model: None,
        };
        let out = render_markdown(&meta, &[], &ExportOptions::default());
        assert!(!out.contains("- **Datum:** 1700000000"));
        assert!(out.contains("2023"));
    }
}

#[cfg(test)]
mod error_display_tests {
    use super::ExportError;
    use std::error::Error as _;

    #[test]
    fn display_and_source_are_stable() {
        let io = ExportError::from(std::io::Error::other("boom"));
        assert_eq!(
            io.to_string(),
            "Export konnte nicht geschrieben werden: boom"
        );
        assert!(io.source().is_some());
        assert_eq!(
            ExportError::PathRejected("r".to_owned()).to_string(),
            "Zielpfad für den Export abgelehnt: r"
        );
        assert!(ExportError::NoClipboard.source().is_none());
    }
}
