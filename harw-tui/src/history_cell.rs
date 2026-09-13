//! Typisierte Ausgabe-Zellen für die Chat-History.
//!
//! # Verantwortung
//! Dieses Modul definiert das [`HistoryCell`]-Trait sowie zehn konkrete
//! Implementierungen (`PlainHistoryCell`, `UserHistoryCell`,
//! `AssistantHistoryCell`, `ToolCallHistoryCell`, `ToolResultHistoryCell`,
//! `ReasoningHistoryCell`, `SubAgentCell`, `PlanGraphCell`,
//! `ApprovalPromptCell`, `GoalCell`) sowie die freien Hilfsfunktionen
//! [`wrap_plain`] (wortweises Umbruchverhalten), [`truncate_chars`]
//! (zeichensichere Kürzung) und [`sanitize_terminal_text`] (Entfernung von
//! Steuerzeichen/ANSI-Sequenzen aus nicht vertrauenswürdigem Modelltext).
//!
//! # Schlüsseltypen
//! - [`HistoryCell`]: Trait — jede Zelle kennt ihre eigene Render-Logik.
//! - [`PlainHistoryCell`]: Unveränderliche Zeilen (z.B. System-Nachrichten).
//! - [`UserHistoryCell`]: Nutzer-Eingabe mit `"> "`-Präfix und Wort-Wrapping.
//! - [`AssistantHistoryCell`]: Assistenten-Antwort, speichert Quelltext; bricht bei
//!   unterschiedlichem `width` unterschiedlich um (Re-Render-on-Resize).
//! - [`ToolCallHistoryCell`]: angeforderter Tool-Aufruf, magenta `"⚙ "`-Präfix.
//! - [`ToolResultHistoryCell`]: abgeschlossener Tool-Aufruf, grüner `"✓ "` /
//!   roter `"✗ "`-Präfix je nach Erfolg.
//! - [`ReasoningHistoryCell`]: Reasoning-Zusammenfassung, gedimmter `"· "`-Präfix.
//! - [`SubAgentCell`]: laufender/beendeter Kind-Agent (`TurnEvent::ChildSpawned`
//!   / `ChildProgress` / `ChildCompleted`); **aktualisierbar** über
//!   [`SubAgentCell::apply_progress`] / [`SubAgentCell::apply_completion`].
//! - [`PlanGraphCell`]: kompakte Übersicht eines `harw_plan::Plan`, kürzt bei
//!   vielen Knoten und nennt die Zahl der ausgelassenen.
//! - [`ApprovalPromptCell`]: P0-Freigabeabfrage; zeigt nach der Entscheidung
//!   das Ergebnis statt der Frage über [`ApprovalPromptCell::apply_decision`].
//! - [`GoalCell`]: Ziel-Statement gegen einen `harw_plan::goal::GoalReport`.
//!
//! # Nebenläufigkeit
//! Alle Typen implementieren [`Send`] + [`Sync`] (erzwungen durch den Trait-Bound).
//! `SubAgentCell` und `ApprovalPromptCell` sind intern veränderlich (`&mut self`-
//! Methoden), aber nicht selbst synchronisiert — geteilter Zugriff über Threads
//! erfordert wie bei jedem `&mut`-Typ eine äußere Synchronisation durch den Aufrufer.
//!
//! # Fehler
//! Dieses Modul produziert keine Fehler; ungültige `width`-Werte werden defensiv
//! behandelt (`width == 0` → 1 Spalte).
//!
//! # Beispiele
//! ```ignore
//! use harw_tui::history_cell::{AssistantHistoryCell, HistoryCell};
//! let cell = AssistantHistoryCell { source: "Hallo Welt".to_owned() };
//! let lines = cell.display_lines(40);
//! assert!(!lines.is_empty());
//! ```
//!
//! Spec-Quelle: `docs/design/codex-tui-study/00-harw-tui-redesign-spec.md` §2.10 / SLICE 7
//! und `docs/design/codex-tui-study/04-rendering-style-dynamic.md` §3 sowie
//! AP W5-01 / W5-10a.

use ratatui::text::{Line, Span};

use harw_plan::goal::GoalReport;
use harw_plan::{Plan, PlanNodeStatus};

use crate::style;

// ─── Trait ───────────────────────────────────────────────────────────────────

/// Trait für eine einzelne typisierte Zelle in der Chat-History.
///
/// # Beschreibung
/// Jede Zelle ist für ihre eigene Render-Logik verantwortlich. Das ermöglicht
/// korrektes Re-Rendering nach Terminal-Resize: Zellen mit textueller Quelle
/// (`AssistantHistoryCell`) können bei jedem `display_lines(width)`-Aufruf neu
/// umbrechen; Zellen mit bereits gerenderten Zeilen (`PlainHistoryCell`) geben
/// diese unverändert zurück.
///
/// # Argumente
/// - `width` (`u16`): Nutzbare Terminalbreite in Spalten. `0` wird wie `1` behandelt.
///
/// # Rückgabe
/// `Vec<Line<'static>>` — vollständig gerenderter Inhalt der Zelle.
///
/// # Nebenläufigkeit
/// Implementierungen müssen [`Send`] + [`Sync`] sein, da Zellen in einem
/// `Arc<Vec<Box<dyn HistoryCell>>>` über Threads geteilt werden können.
///
/// # Beispiele
/// ```ignore
/// use harw_tui::history_cell::{PlainHistoryCell, HistoryCell};
/// use ratatui::text::Line;
/// let cell = PlainHistoryCell { lines: vec![Line::from("Test")] };
/// assert_eq!(cell.desired_height(80), 1);
/// ```
pub(crate) trait HistoryCell: Send + Sync + std::fmt::Debug {
    /// Rendert den Zellinhalt für die gegebene Terminalbreite.
    ///
    /// # Argumente
    /// - `width` (`u16`): Verfügbare Spaltenbreite. `0` wird als `1` behandelt.
    ///
    /// # Rückgabe
    /// Vollständig formatierter Inhalt als Liste von [`Line`]-Werten.
    fn display_lines(&self, width: u16, theme: style::Theme) -> Vec<Line<'static>>;

    /// Gibt die gewünschte Anzahl von Zeilen für die gegebene Breite zurück.
    ///
    /// # Beschreibung
    /// Standardimplementierung delegiert an [`display_lines`](Self::display_lines)
    /// und zählt die zurückgegebenen Zeilen. Kann für performante Layout-Berechnung
    /// überschrieben werden, wenn die Höhe bekannt ist ohne den vollen Render auszuführen.
    ///
    /// # Argumente
    /// - `width` (`u16`): Verfügbare Spaltenbreite.
    ///
    /// # Rückgabe
    /// Anzahl Zeilen als `u16`.
    #[allow(dead_code)]
    fn desired_height(&self, width: u16, theme: style::Theme) -> u16 {
        self.display_lines(width, theme).len() as u16
    }
}

// ─── PlainHistoryCell ─────────────────────────────────────────────────────────

/// Unveränderliche Zelle mit vorgerenderten Zeilen.
///
/// # Beschreibung
/// Geeignet für System-Nachrichten, Trennlinien oder andere Ausgaben, deren
/// Darstellung nicht von der Terminalbreite abhängt. Die gespeicherten [`Line`]-Werte
/// werden bei `display_lines()` geklont und unverändert zurückgegeben.
///
/// # Felder
/// - `lines` (`Vec<Line<'static>>`): Vorgerenderte Zeilen.
///
/// # Spec-Referenz
/// `00-harw-tui-redesign-spec.md` SLICE 7 / `04-rendering-style-dynamic.md` §3.
#[derive(Debug)]
pub(crate) struct PlainHistoryCell {
    /// Vorgerenderte, breitenunabhängige Ausgabezeilen.
    pub lines: Vec<Line<'static>>,
}

impl HistoryCell for PlainHistoryCell {
    /// Gibt einen Klon der gespeicherten Zeilen zurück; ignoriert `width`.
    ///
    /// # Argumente
    /// - `width` (`u16`): Wird ignoriert, da die Zeilen bereits finalisiert sind.
    ///
    /// # Rückgabe
    /// Geklonte Liste der internen Zeilen.
    fn display_lines(&self, _width: u16, _theme: style::Theme) -> Vec<Line<'static>> {
        self.lines.clone()
    }
}

// ─── UserHistoryCell ──────────────────────────────────────────────────────────

/// Nutzer-Eingabe-Zelle mit `"> "`-Präfix und wortweisem Wrapping.
///
/// # Beschreibung
/// Speichert den Rohtext der Nutzereingabe. Bei `display_lines(width)` wird der Text
/// zunächst mit [`wrap_plain`] umbrochen; die erste Zeile erhält den Präfix `"> "`,
/// Folgezeilen werden mit entsprechendem Leerzeichen-Einzug versehen, um die
/// visuelle Ausrichtung zu erhalten.
///
/// # Felder
/// - `text` (`String`): Rohtext der Nutzereingabe.
///
/// # Spec-Referenz
/// `00-harw-tui-redesign-spec.md` SLICE 7 / `04-rendering-style-dynamic.md` §3.
#[derive(Debug)]
pub(crate) struct UserHistoryCell {
    /// Rohtext der Nutzereingabe vor der Darstellung.
    pub text: String,
}

impl HistoryCell for UserHistoryCell {
    /// Rendert den Nutzertext mit `"> "`-Präfix und wortweisem Wrapping.
    ///
    /// # Beschreibung
    /// Der effektive Textbereich ist `width.saturating_sub(2)` Spalten breit, um
    /// Platz für den `"> "`-Präfix zu schaffen. Folgezeilen erhalten zwei führende
    /// Leerzeichen als Einzug. Leerer Text ergibt genau eine Zeile mit `"> "`.
    ///
    /// # Argumente
    /// - `width` (`u16`): Gesamtbreite in Spalten (inklusive Präfix).
    ///
    /// # Rückgabe
    /// Liste der darstellbaren Zeilen beginnend mit `"> <erster Textabschnitt>"`.
    fn display_lines(&self, width: u16, theme: style::Theme) -> Vec<Line<'static>> {
        let prefix_len = 2_u16;
        let text_width = width.saturating_sub(prefix_len).max(1);
        let user_style = style::user_style(theme);

        if self.text.is_empty() {
            return vec![Line::from(vec![Span::styled("> ", user_style)])];
        }

        let wrapped = wrap_plain(&self.text, text_width);
        wrapped
            .into_iter()
            .enumerate()
            .map(|(i, line)| {
                let raw_content: String = line
                    .spans
                    .into_iter()
                    .map(|s| s.content.into_owned())
                    .collect();
                let prefix_span = if i == 0 {
                    Span::styled("> ", user_style)
                } else {
                    Span::raw("  ")
                };
                Line::from(vec![prefix_span, Span::raw(raw_content)])
            })
            .collect()
    }
}

// ─── AssistantHistoryCell ─────────────────────────────────────────────────────

/// Finalisierte Assistenten-Antwort mit Re-Render-on-Resize.
///
/// # Beschreibung
/// Speichert den Quelltext der Antwort als `String`. Bei jedem `display_lines(width)`-
/// Aufruf wird der Text neu umbrochen — dadurch passt sich die Darstellung korrekt an
/// unterschiedliche Terminalbreiten an (z.B. nach einem Resize-Event). In dieser
/// Iteration wird kein Markdown-Renderer eingesetzt; stattdessen kommt [`wrap_plain`]
/// zum Einsatz.
///
/// # Felder
/// - `source` (`String`): Quelltext der Assistenten-Antwort.
///
/// # Nebenläufigkeit
/// Lesen auf `source` ist nebenläufig sicher; [`Send`] + [`Sync`] sind ableitbar,
/// da `String` beide Bounds erfüllt.
///
/// # Spec-Referenz
/// `00-harw-tui-redesign-spec.md` SLICE 7 / `04-rendering-style-dynamic.md` §3
/// ("`AgentMarkdownCell { source }` — Re-rendert bei jeder `display_lines(width)`-Anfrage").
#[derive(Debug)]
pub(crate) struct AssistantHistoryCell {
    /// Quelltext der Assistenten-Antwort; wird bei jedem Render neu umbrochen.
    pub source: String,
}

impl HistoryCell for AssistantHistoryCell {
    /// Bricht den Quelltext für die gegebene Breite neu um und setzt einen
    /// cyanfarbenen `» `-Präfix auf die erste Zeile (Folgezeilen: `  `), damit
    /// Assistenten-Antworten visuell klar von Nutzer-Zeilen (`> `-Präfix, grün)
    /// unterschieden werden können.
    fn display_lines(&self, width: u16, theme: style::Theme) -> Vec<Line<'static>> {
        let prefix_len = 2_u16;
        let text_width = width.saturating_sub(prefix_len).max(1);
        let source = if self.source.is_empty() {
            " "
        } else {
            &self.source
        };
        let wrapped = wrap_plain(source, text_width);
        let assistant_style = style::assistant_style(theme);
        wrapped
            .into_iter()
            .enumerate()
            .map(|(i, line)| {
                let raw: String = line
                    .spans
                    .into_iter()
                    .map(|s| s.content.into_owned())
                    .collect();
                let prefix_span = if i == 0 {
                    Span::styled("» ", assistant_style)
                } else {
                    Span::raw("  ")
                };
                Line::from(vec![prefix_span, Span::raw(raw)])
            })
            .collect()
    }
}

// ─── ToolCallHistoryCell ──────────────────────────────────────────────────────

/// Zeigt an, dass das Modell einen Tool-Aufruf angefordert hat.
///
/// # Beschreibung
/// Wird bei `TurnEvent::ToolCallRequested` erzeugt. Rendert einen magenta-
/// farbenen `"⚙ "`-Präfix auf der ersten Zeile (Folgezeilen: `"  "`-Einzug),
/// gefolgt vom Format `"{tool_name}({arguments_preview})"`. Die Akzentfarbe
/// (Magenta) unterscheidet sich bewusst von User-Grün und Assistant-Cyan.
///
/// # Felder
/// - `tool_name` (`String`): Name des angeforderten Tools.
/// - `arguments_preview` (`String`): Kompakte, bereits gekürzte JSON-Vorschau
///   der Argumente.
///
/// # Spec-Referenz
/// Welle 3 — Verdrahtung von `TurnEvent::ToolCallRequested` in `app.rs::run_loop`.
#[derive(Debug)]
pub(crate) struct ToolCallHistoryCell {
    /// Name des angeforderten Tools.
    pub tool_name: String,
    /// Kompakte JSON-Vorschau der Argumente (bereits auf eine sinnvolle Länge gekürzt).
    pub arguments_preview: String,
}

impl HistoryCell for ToolCallHistoryCell {
    /// Rendert `"{tool_name}({arguments_preview})"` mit magentafarbenem
    /// `"⚙ "`-Präfix auf der ersten Zeile und wortweisem Wrapping.
    ///
    /// # Argumente
    /// - `width` (`u16`): Gesamtbreite in Spalten (inklusive Präfix).
    ///
    /// # Rückgabe
    /// Liste der darstellbaren Zeilen beginnend mit `"⚙ <tool_name>(<preview>)"`.
    fn display_lines(&self, width: u16, theme: style::Theme) -> Vec<Line<'static>> {
        let prefix_len = 2_u16;
        let text_width = width.saturating_sub(prefix_len).max(1);
        let text = format!("{}({})", self.tool_name, self.arguments_preview);
        let wrapped = wrap_plain(&text, text_width);
        let tool_style = style::tool_style(theme);
        wrapped
            .into_iter()
            .enumerate()
            .map(|(i, line)| {
                let raw: String = line
                    .spans
                    .into_iter()
                    .map(|s| s.content.into_owned())
                    .collect();
                let prefix_span = if i == 0 {
                    Span::styled("⚙ ", tool_style)
                } else {
                    Span::raw("  ")
                };
                Line::from(vec![prefix_span, Span::raw(raw)])
            })
            .collect()
    }
}

// ─── ToolResultHistoryCell ────────────────────────────────────────────────────

/// Zeigt das Ergebnis eines abgeschlossenen Tool-Aufrufs an.
///
/// # Beschreibung
/// Wird bei `TurnEvent::ToolCallCompleted` erzeugt. Rendert einen grünen
/// `"✓ "`-Präfix bei Erfolg bzw. einen roten `"✗ "`-Präfix bei Fehlschlag,
/// gefolgt vom Format `"{tool_name} ({duration_ms}ms)"`.
///
/// # Felder
/// - `tool_name` (`String`): Name des abgeschlossenen Tools.
/// - `success` (`bool`): `true` bei Erfolg, `false` bei Fehlschlag.
/// - `duration_ms` (`u64`): Laufzeit des Tool-Aufrufs in Millisekunden.
///
/// # Spec-Referenz
/// Welle 3 — Verdrahtung von `TurnEvent::ToolCallCompleted` in `app.rs::run_loop`.
#[derive(Debug)]
pub(crate) struct ToolResultHistoryCell {
    /// Name des abgeschlossenen Tools.
    pub tool_name: String,
    /// `true` bei Erfolg, `false` bei Fehlschlag.
    pub success: bool,
    /// Laufzeit des Tool-Aufrufs in Millisekunden.
    pub duration_ms: u64,
}

impl HistoryCell for ToolResultHistoryCell {
    /// Rendert `"{tool_name} ({duration_ms}ms)"` mit grünem `"✓ "`-Präfix bei
    /// Erfolg bzw. rotem `"✗ "`-Präfix bei Fehlschlag.
    ///
    /// # Argumente
    /// - `width` (`u16`): Gesamtbreite in Spalten (inklusive Präfix).
    ///
    /// # Rückgabe
    /// Liste der darstellbaren Zeilen beginnend mit dem Erfolgs-/Fehler-Präfix.
    fn display_lines(&self, width: u16, theme: style::Theme) -> Vec<Line<'static>> {
        let prefix_len = 2_u16;
        let text_width = width.saturating_sub(prefix_len).max(1);
        let text = format!("{} ({}ms)", self.tool_name, self.duration_ms);
        let wrapped = wrap_plain(&text, text_width);
        let (glyph, result_style) = if self.success {
            ("✓ ", style::success_style(theme))
        } else {
            ("✗ ", style::error_style(theme))
        };
        wrapped
            .into_iter()
            .enumerate()
            .map(|(i, line)| {
                let raw: String = line
                    .spans
                    .into_iter()
                    .map(|s| s.content.into_owned())
                    .collect();
                let prefix_span = if i == 0 {
                    Span::styled(glyph, result_style)
                } else {
                    Span::raw("  ")
                };
                Line::from(vec![prefix_span, Span::raw(raw)])
            })
            .collect()
    }
}

// ─── ReasoningHistoryCell ─────────────────────────────────────────────────────

/// Zeigt eine Reasoning-Zusammenfassung des Modells an.
///
/// # Beschreibung
/// Wird bei `TurnEvent::ItemAdded { item: TurnItem::Reasoning(..), .. }`
/// erzeugt. Rendert einen gedimmten (`style::dim_style()`) `"· "`-Präfix auf
/// der ersten Zeile (Folgezeilen: `"  "`-Einzug).
///
/// # Felder
/// - `summary` (`String`): Zusammengefasster Denkprozess-Text (bereits aus
///   `ReasoningItem::summary_text` zusammengefügt).
///
/// # Spec-Referenz
/// Welle 3 — Verdrahtung von `TurnEvent::ItemAdded(Reasoning)` in
/// `app.rs::run_loop`.
#[derive(Debug)]
pub(crate) struct ReasoningHistoryCell {
    /// Zusammengefasster Denkprozess-Text.
    pub summary: String,
}

impl HistoryCell for ReasoningHistoryCell {
    /// Rendert die Reasoning-Zusammenfassung mit gedimmtem `"· "`-Präfix und
    /// wortweisem Wrapping.
    ///
    /// # Argumente
    /// - `width` (`u16`): Gesamtbreite in Spalten (inklusive Präfix).
    ///
    /// # Rückgabe
    /// Liste der darstellbaren Zeilen beginnend mit `"· <summary>"`.
    fn display_lines(&self, width: u16, theme: style::Theme) -> Vec<Line<'static>> {
        let prefix_len = 2_u16;
        let text_width = width.saturating_sub(prefix_len).max(1);
        let summary = if self.summary.is_empty() {
            " "
        } else {
            &self.summary
        };
        let wrapped = wrap_plain(summary, text_width);
        let dim = style::dim_style(theme);
        wrapped
            .into_iter()
            .enumerate()
            .map(|(i, line)| {
                let raw: String = line
                    .spans
                    .into_iter()
                    .map(|s| s.content.into_owned())
                    .collect();
                let prefix_span = if i == 0 {
                    Span::styled("· ", dim)
                } else {
                    Span::raw("  ")
                };
                Line::from(vec![prefix_span, Span::raw(raw)])
            })
            .collect()
    }
}

// ─── SubAgentCell ─────────────────────────────────────────────────────────────

/// Obergrenze für die angezeigte Länge der Kind-Frage/des Kind-Auftrags in
/// [`SubAgentCell`], bevor char-sicher mit [`truncate_chars`] gekürzt wird.
const SUBAGENT_QUESTION_PREVIEW_CHARS: usize = 80;

/// Obergrenze für die angezeigte Kind-Kennung in [`SubAgentCell`].
///
/// Kurz genug, dass die Zeile nicht von der ID beherrscht wird, lang genug, um
/// gleichzeitig laufende Kinder derselben Rolle auseinanderzuhalten.
const SUBAGENT_CHILD_ID_PREVIEW_CHARS: usize = 12;

/// Kürzt eine Kind-Kennung auf ihr **Ende** statt auf ihren Anfang.
///
/// # Beschreibung
/// Kind-Kennungen sind typischerweise gleich lang und teilen sich einen
/// Präfix (`child-explorer-1`, `child-explorer-2` …). Vorne zu kürzen macht sie
/// deshalb ununterscheidbar — genau das, wozu die Kennung in der Zeile steht.
/// Der unterscheidende Teil sitzt am Ende, also bleibt das Ende stehen und der
/// Anfang wird durch `…` ersetzt.
///
/// # Argumente
/// - `id` (`&str`): die vollständige Kennung.
/// - `max_chars` (`usize`): Obergrenze in **Zeichen**, nicht Bytes.
///
/// # Rückgabe
/// Die Kennung, falls sie kurz genug ist; sonst `…` gefolgt von ihren letzten
/// `max_chars - 1` Zeichen. Die Zählung läuft über `char`, schneidet also nie
/// mitten in ein Mehrbyte-Zeichen.
fn truncate_id_tail(id: &str, max_chars: usize) -> String {
    let total = id.chars().count();
    if total <= max_chars || max_chars == 0 {
        return id.to_owned();
    }
    let keep = max_chars.saturating_sub(1);
    let tail: String = id.chars().skip(total - keep).collect();
    format!("…{tail}")
}

/// Laufzeitstatus eines Sub-Agenten innerhalb einer [`SubAgentCell`].
///
/// # Beschreibung
/// `Running` gilt ab `TurnEvent::ChildSpawned` bis zum Empfang des
/// zugehörigen `TurnEvent::ChildCompleted`. `outcome` in `Done` ist die
/// Kurzform aus `TurnEvent::ChildCompleted` (`"completed"`,
/// `"budget_exceeded"`, `"cancelled"`, `"failed"`); nur `"completed"` gilt
/// als Erfolg (grüner `"✓ "`-Präfix), jeder andere Wert als Fehlschlag
/// (roter `"✗ "`-Präfix).
///
/// # Spec-Referenz
/// AP W5-01 — `harw-protocol/src/events.rs::TurnEvent::{ChildSpawned,
/// ChildProgress, ChildCompleted}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SubAgentStatus {
    /// Kind läuft noch; `tool_calls`/`tokens` der besitzenden [`SubAgentCell`]
    /// werden über [`SubAgentCell::apply_progress`] laufend aktualisiert.
    Running,
    /// Kind ist terminiert.
    Done {
        /// Kurzform des Ergebnisses (siehe Typdokumentation oben).
        outcome: String,
        /// Laufzeit des Kindes in Millisekunden.
        duration_ms: u64,
    },
}

/// Zeigt einen laufenden oder beendeten Sub-Agenten (Kind-Session) an.
///
/// # Beschreibung
/// Wird bei `TurnEvent::ChildSpawned` erzeugt und für dasselbe Kind bei
/// jedem folgenden `TurnEvent::ChildProgress` / `TurnEvent::ChildCompleted`
/// **in derselben Zelleninstanz** über [`SubAgentCell::apply_progress`] bzw.
/// [`SubAgentCell::apply_completion`] aktualisiert — ein Kind erzeugt drei
/// Events nacheinander, nicht drei Zellen. Rendert Rolle, gekürzte
/// Frage/Auftrag, Status (laufend / fertig / gescheitert), Tool-Aufruf-Zahl
/// und verbrauchte Token in einer wortweise umgebrochenen Zeile.
///
/// # Felder
/// - `child_id` (`String`): Bezeichner des Kindes (`SessionId::as_str()`);
///   dient dem Aufrufer zur Zuordnung eingehender `ChildProgress`/
///   `ChildCompleted`-Events auf die richtige Zelleninstanz.
/// - `role` (`String`): Rolle des Kindes (z. B. `"explorer"`).
/// - `question` (`Option<String>`): Auftrag/Frage an das Kind, `None` wenn
///   keine mitgegeben wurde.
/// - `tool_calls` (`u32`): Bisherige Anzahl ausgeführter Tool-Aufrufe.
/// - `tokens` (`u64`): Bisher verbrauchte Token.
/// - `status` ([`SubAgentStatus`]): Laufzeitstatus.
///
/// # Aktualisierbarkeit (Abweichung vom Bestandsmuster)
/// `history_cell.rs` kannte vor diesem AP kein Muster für nachträglich
/// veränderte Zellen — alle bisherigen Zellen sind unveränderlich und werden
/// einmalig konstruiert. Diese Zelle bietet daher die inhärenten Methoden
/// [`SubAgentCell::apply_progress`] und [`SubAgentCell::apply_completion`]
/// an. Der Aufrufer muss die konkrete `SubAgentCell`-Instanz (nicht nur
/// `Box<dyn HistoryCell>`) referenzierbar halten, um sie aufzurufen (z. B.
/// über einen nach `child_id` indizierten Seitenkanal), da [`HistoryCell`]
/// kein Downcasting anbietet.
///
/// # Spec-Referenz
/// AP W5-01 — `harw-protocol/src/events.rs::TurnEvent::{ChildSpawned,
/// ChildProgress, ChildCompleted}`.
#[derive(Debug)]
pub(crate) struct SubAgentCell {
    /// Bezeichner des Kindes (`SessionId::as_str()`).
    pub child_id: String,
    /// Rolle des Kindes.
    pub role: String,
    /// Auftrag/Frage an das Kind, `None` wenn keine mitgegeben wurde.
    pub question: Option<String>,
    /// Bisherige Anzahl ausgeführter Tool-Aufrufe.
    pub tool_calls: u32,
    /// Bisher verbrauchte Token.
    pub tokens: u64,
    /// Laufzeitstatus.
    pub status: SubAgentStatus,
}

impl SubAgentCell {
    /// Aktualisiert Tool-Aufruf-Zahl und Token-Verbrauch bei
    /// `TurnEvent::ChildProgress`.
    ///
    /// # Argumente
    /// - `tool_calls` (`u32`): neue Gesamtzahl ausgeführter Tool-Aufrufe.
    /// - `tokens` (`u64`): neue Gesamtzahl verbrauchter Token.
    ///
    /// # Beispiele
    /// ```ignore
    /// use harw_tui::history_cell::{SubAgentCell, SubAgentStatus};
    /// let mut cell = SubAgentCell {
    ///     child_id: "child-1".to_owned(),
    ///     role: "explorer".to_owned(),
    ///     question: None,
    ///     tool_calls: 0,
    ///     tokens: 0,
    ///     status: SubAgentStatus::Running,
    /// };
    /// cell.apply_progress(3, 128);
    /// assert_eq!(cell.tool_calls, 3);
    /// ```
    pub(crate) fn apply_progress(&mut self, tool_calls: u32, tokens: u64) {
        self.tool_calls = tool_calls;
        self.tokens = tokens;
    }

    /// Markiert das Kind als terminiert bei `TurnEvent::ChildCompleted`.
    ///
    /// # Argumente
    /// - `outcome` (`impl Into<String>`): Kurzform des Ergebnisses
    ///   (`"completed"`, `"budget_exceeded"`, `"cancelled"`, `"failed"`).
    /// - `duration_ms` (`u64`): Laufzeit des Kindes in Millisekunden.
    ///
    /// # Beispiele
    /// ```ignore
    /// use harw_tui::history_cell::{SubAgentCell, SubAgentStatus};
    /// let mut cell = SubAgentCell {
    ///     child_id: "child-1".to_owned(),
    ///     role: "explorer".to_owned(),
    ///     question: None,
    ///     tool_calls: 2,
    ///     tokens: 64,
    ///     status: SubAgentStatus::Running,
    /// };
    /// cell.apply_completion("completed", 512);
    /// assert_eq!(cell.status, SubAgentStatus::Done { outcome: "completed".to_owned(), duration_ms: 512 });
    /// ```
    pub(crate) fn apply_completion(&mut self, outcome: impl Into<String>, duration_ms: u64) {
        self.status = SubAgentStatus::Done {
            outcome: outcome.into(),
            duration_ms,
        };
    }
}

impl HistoryCell for SubAgentCell {
    /// Rendert Rolle, gekürzte Frage, Status, Tool-Aufruf-Zahl und Token in
    /// einer Zeile mit statusabhängigem Präfix (`"▶ "` laufend, `"✓ "`
    /// fertig, `"✗ "` gescheitert) und wortweisem Wrapping.
    ///
    /// # Argumente
    /// - `width` (`u16`): Gesamtbreite in Spalten (inklusive Präfix).
    ///
    /// # Rückgabe
    /// Liste der darstellbaren Zeilen beginnend mit dem Status-Präfix.
    fn display_lines(&self, width: u16, theme: style::Theme) -> Vec<Line<'static>> {
        let prefix_len = 2_u16;
        let text_width = width.saturating_sub(prefix_len).max(1);

        let question_display = match &self.question {
            Some(q) if !q.is_empty() => truncate_chars(q, SUBAGENT_QUESTION_PREVIEW_CHARS),
            _ => "(kein Auftrag angegeben)".to_owned(),
        };

        let (glyph, cell_style, status_text) = match &self.status {
            SubAgentStatus::Running => ("▶ ", style::tool_style(theme), "läuft".to_owned()),
            SubAgentStatus::Done {
                outcome,
                duration_ms,
            } if outcome == "completed" => (
                "✓ ",
                style::success_style(theme),
                format!("fertig ({duration_ms}ms)"),
            ),
            SubAgentStatus::Done {
                outcome,
                duration_ms,
            } => (
                "✗ ",
                style::error_style(theme),
                format!("gescheitert: {outcome} ({duration_ms}ms)"),
            ),
        };

        // Die Kind-Kennung gehört sichtbar in die Zeile: bei einem Fan-out
        // laufen mehrere Kinder derselben Rolle gleichzeitig, und vier Zeilen
        // „explorer: …" wären nicht auseinanderzuhalten. Gekürzt wird am
        // **Anfang** (`truncate_id_tail`), weil Kennungen sich am Ende
        // unterscheiden — eine Kürzung von hinten machte sie wieder gleich.
        let text = format!(
            "{role} [{child}]: {question_display} — {status_text} · {tool_calls} Tools · {tokens} Tok",
            role = self.role,
            child = truncate_id_tail(&self.child_id, SUBAGENT_CHILD_ID_PREVIEW_CHARS),
            tool_calls = self.tool_calls,
            tokens = self.tokens,
        );

        let wrapped = wrap_plain(&text, text_width);
        wrapped
            .into_iter()
            .enumerate()
            .map(|(i, line)| {
                let raw: String = line
                    .spans
                    .into_iter()
                    .map(|s| s.content.into_owned())
                    .collect();
                let prefix_span = if i == 0 {
                    Span::styled(glyph, cell_style)
                } else {
                    Span::raw("  ")
                };
                Line::from(vec![prefix_span, Span::raw(raw)])
            })
            .collect()
    }
}

// ─── PlanGraphCell ────────────────────────────────────────────────────────────

/// Obergrenze der in [`PlanGraphCell`] gleichzeitig dargestellten
/// Plan-Knoten. Bei mehr Knoten werden die überzähligen abgeschnitten und
/// ihre Anzahl in einer Sammelzeile genannt (siehe
/// [`PlanGraphCell::display_lines`]).
const PLAN_GRAPH_MAX_NODES: usize = 25;

/// Obergrenze für die angezeigte Länge des `objective`-Texts je Plan-Knoten,
/// bevor char-sicher mit [`truncate_chars`] gekürzt wird.
const PLAN_GRAPH_OBJECTIVE_PREVIEW_CHARS: usize = 48;

/// Zeigt eine kompakte Übersicht eines `harw_plan::Plan` an.
///
/// # Beschreibung
/// Rendert je Knoten eine Zeile mit ID, Art (`PlanNodeKind`), Status
/// (`PlanNodeStatus`), Welle und gekürztem Ziel. Die Zeile ist vollständig
/// in einer statusabhängigen Farbe eingefärbt (nicht nur ein Präfix-Glyph
/// wie bei den übrigen Zellen dieses Moduls), damit fertige (grün),
/// blockierte (rot) und bereite (Akzentfarbe, fett) Knoten auf einen Blick
/// optisch unterscheidbar sind; laufende Knoten erhalten eine Warnfarbe,
/// Entwurfs-/terminale Knoten (`Draft`, `Superseded`, `Invalidated`) eine
/// gedimmte Farbe.
///
/// Enthält der Plan mehr als [`PLAN_GRAPH_MAX_NODES`] Knoten, werden nur die
/// ersten [`PLAN_GRAPH_MAX_NODES`] gerendert; eine abschließende, gedimmte
/// Sammelzeile nennt die Anzahl der ausgelassenen Knoten sowie die
/// Gesamtzahl — ein Terminal, das hunderte Zeilen ausspuckt, ist
/// unbrauchbar.
///
/// # Felder
/// - `plan` (`harw_plan::Plan`): der darzustellende Plan.
///
/// # Spec-Referenz
/// AP W5-01 — `harw-plan/src/types.rs::{Plan, PlanNode, PlanNodeStatus,
/// PlanNodeKind}`.
#[derive(Debug, Clone)]
pub(crate) struct PlanGraphCell {
    /// Der darzustellende Plan.
    pub plan: Plan,
}

impl HistoryCell for PlanGraphCell {
    /// Rendert je Knoten eine vollständig statusgefärbte, wortweise
    /// umgebrochene Zeile; kürzt bei Überlänge auf [`PLAN_GRAPH_MAX_NODES`]
    /// Knoten und nennt die Zahl der ausgelassenen Knoten.
    ///
    /// # Argumente
    /// - `width` (`u16`): Gesamtbreite in Spalten (inklusive Glyph-Präfix).
    ///
    /// # Rückgabe
    /// Liste der darstellbaren Zeilen; enthält bei einem leeren Plan genau
    /// eine gedimmte Platzhalterzeile statt einer leeren Liste.
    fn display_lines(&self, width: u16, theme: style::Theme) -> Vec<Line<'static>> {
        let prefix_len = 2_u16;
        let text_width = width.saturating_sub(prefix_len).max(1);
        let mut lines: Vec<Line<'static>> = Vec::new();

        let total = self.plan.nodes.len();
        let visible = total.min(PLAN_GRAPH_MAX_NODES);

        for node in self.plan.nodes.iter().take(visible) {
            let node_style = match node.status {
                PlanNodeStatus::Completed => style::success_style(theme),
                PlanNodeStatus::Blocked => style::error_style(theme),
                PlanNodeStatus::Ready => style::selected_style(theme),
                PlanNodeStatus::InProgress => style::warning_style(theme),
                PlanNodeStatus::Draft
                | PlanNodeStatus::Superseded
                | PlanNodeStatus::Invalidated => style::dim_style(theme),
            };

            let wave_display = node
                .wave
                .map(|w| w.to_string())
                .unwrap_or_else(|| "–".to_owned());
            let objective_preview =
                truncate_chars(&node.objective, PLAN_GRAPH_OBJECTIVE_PREVIEW_CHARS);
            let node_text = format!(
                "{id} · {kind:?} · {status:?} · Welle {wave_display} · {objective_preview}",
                id = node.id,
                kind = node.kind,
                status = node.status,
            );

            let wrapped = wrap_plain(&node_text, text_width);
            for (i, line) in wrapped.into_iter().enumerate() {
                let raw: String = line
                    .spans
                    .into_iter()
                    .map(|s| s.content.into_owned())
                    .collect();
                let content = if i == 0 {
                    format!("● {raw}")
                } else {
                    format!("  {raw}")
                };
                lines.push(Line::from(Span::styled(content, node_style)));
            }
        }

        if total > PLAN_GRAPH_MAX_NODES {
            let elided = total - PLAN_GRAPH_MAX_NODES;
            let note = format!("… {elided} weitere Knoten ausgeblendet (insgesamt {total})");
            lines.push(Line::from(Span::styled(note, style::dim_style(theme))));
        }

        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                "(keine Knoten im Plan)",
                style::dim_style(theme),
            )));
        }

        lines
    }
}

// ─── ApprovalPromptCell ───────────────────────────────────────────────────────

/// Obergrenze für die angezeigte Länge der (bereits sanitisierten)
/// Werkzeug-Argumente in [`ApprovalPromptCell`], bevor char-sicher mit
/// [`truncate_chars`] gekürzt wird.
const APPROVAL_ARGS_PREVIEW_CHARS: usize = 160;

/// Zeigt die P0-Freigabeabfrage für einen Werkzeugaufruf an.
///
/// # Beschreibung
/// Solange `decision == None`, rendert die Zelle Werkzeugname, die
/// **gekürzten und terminal-sicher gerenderten** Argumente (über
/// [`sanitize_terminal_text`] dann [`truncate_chars`] — Steuerzeichen und
/// ANSI-Sequenzen aus Modell-Ausgaben können damit die Darstellung nicht
/// kapern) sowie die Tastenbelegung (`[y] freigeben · [n] ablehnen`). Sobald
/// über [`ApprovalPromptCell::apply_decision`] eine Entscheidung gesetzt
/// wurde, zeigt **dieselbe Zelle** stattdessen nur noch das Ergebnis
/// (freigegeben/abgelehnt) — die Frage verschwindet vollständig.
///
/// # Felder
/// - `tool_name` (`String`): Name des zur Freigabe anstehenden Werkzeugs.
/// - `arguments_raw` (`String`): Roh-Argumente (unsanitisiert, z. B. eine
///   JSON-Serialisierung) — Sanitisierung/Kürzung erfolgt erst beim Rendern.
/// - `decision` (`Option<bool>`): `None` = Entscheidung steht aus, `Some(true)`
///   = freigegeben, `Some(false)` = abgelehnt.
///
/// # Spec-Referenz
/// AP W5-01 (P0) — Freigabeabfrage vor Werkzeugausführung.
#[derive(Debug)]
pub(crate) struct ApprovalPromptCell {
    /// Name des zur Freigabe anstehenden Werkzeugs.
    pub tool_name: String,
    /// Roh-Argumente (unsanitisiert); Sanitisierung/Kürzung erfolgt beim Rendern.
    pub arguments_raw: String,
    /// `None` = Entscheidung steht aus, `Some(true)` = freigegeben,
    /// `Some(false)` = abgelehnt.
    pub decision: Option<bool>,
}

impl ApprovalPromptCell {
    /// Setzt die Freigabeentscheidung; ab dem nächsten `display_lines`-Aufruf
    /// zeigt die Zelle das Ergebnis statt der Frage.
    ///
    /// # Argumente
    /// - `approved` (`bool`): `true` = freigegeben, `false` = abgelehnt.
    ///
    /// # Beispiele
    /// ```ignore
    /// use harw_tui::history_cell::ApprovalPromptCell;
    /// let mut cell = ApprovalPromptCell {
    ///     tool_name: "run_shell".to_owned(),
    ///     arguments_raw: "{}".to_owned(),
    ///     decision: None,
    /// };
    /// cell.apply_decision(true);
    /// assert_eq!(cell.decision, Some(true));
    /// ```
    pub(crate) fn apply_decision(&mut self, approved: bool) {
        self.decision = Some(approved);
    }
}

impl HistoryCell for ApprovalPromptCell {
    /// Rendert entweder die Freigabefrage (Werkzeugname, sanitisiert-gekürzte
    /// Argumente, Tastenbelegung) oder — nach gesetzter Entscheidung — nur
    /// das Ergebnis, jeweils mit wortweisem Wrapping.
    ///
    /// # Argumente
    /// - `width` (`u16`): Gesamtbreite in Spalten (inklusive Präfix).
    ///
    /// # Rückgabe
    /// Liste der darstellbaren Zeilen.
    fn display_lines(&self, width: u16, theme: style::Theme) -> Vec<Line<'static>> {
        let prefix_len = 2_u16;
        let text_width = width.saturating_sub(prefix_len).max(1);

        match self.decision {
            None => {
                let sanitized = sanitize_terminal_text(&self.arguments_raw);
                let args_preview = truncate_chars(&sanitized, APPROVAL_ARGS_PREVIEW_CHARS);
                let text = format!(
                    "Freigabe erforderlich: {tool_name}\nArgumente: {args_preview}\n[y] freigeben · [n] ablehnen",
                    tool_name = self.tool_name,
                );
                let wrapped = wrap_plain(&text, text_width);
                let warn_style = style::warning_style(theme);
                wrapped
                    .into_iter()
                    .enumerate()
                    .map(|(i, line)| {
                        let raw: String = line
                            .spans
                            .into_iter()
                            .map(|s| s.content.into_owned())
                            .collect();
                        let prefix_span = if i == 0 {
                            Span::styled("⚠ ", warn_style)
                        } else {
                            Span::raw("  ")
                        };
                        Line::from(vec![prefix_span, Span::raw(raw)])
                    })
                    .collect()
            }
            Some(approved) => {
                let (glyph, result_style, verdict) = if approved {
                    ("✓ ", style::success_style(theme), "freigegeben")
                } else {
                    ("✗ ", style::error_style(theme), "abgelehnt")
                };
                let text = format!("{} — {verdict}", self.tool_name);
                let wrapped = wrap_plain(&text, text_width);
                wrapped
                    .into_iter()
                    .enumerate()
                    .map(|(i, line)| {
                        let raw: String = line
                            .spans
                            .into_iter()
                            .map(|s| s.content.into_owned())
                            .collect();
                        let prefix_span = if i == 0 {
                            Span::styled(glyph, result_style)
                        } else {
                            Span::raw("  ")
                        };
                        Line::from(vec![prefix_span, Span::raw(raw)])
                    })
                    .collect()
            }
        }
    }
}

// ─── GoalCell ─────────────────────────────────────────────────────────────────

/// Obergrenze für die angezeigte Länge des Ziel-Statements in [`GoalCell`],
/// bevor char-sicher mit [`truncate_chars`] gekürzt wird.
const GOAL_STATEMENT_PREVIEW_CHARS: usize = 100;

/// Zeigt ein Goal gegen einen ausgewerteten `harw_plan::goal::GoalReport` an.
///
/// # Beschreibung
/// Rendert das gekürzte Ziel-Statement, danach eine Zusammenfassungszeile
/// (`"Kriterien: {erfüllt} von {gesamt} erfüllt ({prozent} %)"`), je eine
/// Zeile pro Akzeptanzkriterium (**1-basiert** nummeriert, erfüllt grün /
/// offen gedimmt) und — falls vorhanden — je eine rote Zeile pro verletzter
/// Invariante. Die Gesamtzahl der Kriterien ergibt sich aus
/// `criteria_met.len() + criteria_open.len()`, da `evaluate_goal` jedes
/// Kriterium genau einer der beiden Listen zuordnet.
///
/// Die Prozentanzeige rundet `coverage * 100.0` und castet nach `u32`; seit
/// Rust 1.45 sättigen `as`-Casts von Gleitkomma nach Ganzzahl `NaN` und
/// negative Werte auf `0` (kein Panic, kein `NaN` in der Anzeige) — das
/// deckt sowohl eine leere Kriterienliste (per `evaluate_goal`-Konvention
/// `coverage == 1.0`, also 100 %) als auch 0 von N erfüllten Kriterien
/// (`coverage == 0.0`, also 0 %) sicher ab.
///
/// # Felder
/// - `statement` (`String`): Ziel-Statement (`Goal::statement`).
/// - `report` (`harw_plan::goal::GoalReport`): Auswertungsergebnis aus
///   `evaluate_goal`.
///
/// # Spec-Referenz
/// AP W5-10a — `harw-plan/src/goal.rs::{GoalReport, evaluate_goal}`.
#[derive(Debug, Clone)]
pub(crate) struct GoalCell {
    /// Ziel-Statement (`Goal::statement`).
    pub statement: String,
    /// Auswertungsergebnis aus `evaluate_goal`.
    pub report: GoalReport,
}

impl HistoryCell for GoalCell {
    /// Rendert Ziel-Statement, Kriterien-Coverage, Einzelkriterien (1-basiert)
    /// und verletzte Invarianten.
    ///
    /// # Argumente
    /// - `width` (`u16`): Gesamtbreite in Spalten (inklusive Glyph-Präfix
    ///   der Ziel-Zeile).
    ///
    /// # Rückgabe
    /// Liste der darstellbaren Zeilen.
    fn display_lines(&self, width: u16, theme: style::Theme) -> Vec<Line<'static>> {
        let prefix_len = 2_u16;
        let text_width = width.saturating_sub(prefix_len).max(1);
        let mut lines: Vec<Line<'static>> = Vec::new();

        let statement_preview = truncate_chars(&self.statement, GOAL_STATEMENT_PREVIEW_CHARS);
        let header_text = format!("Ziel: {statement_preview}");
        let header_style = style::selected_style(theme);
        let wrapped_header = wrap_plain(&header_text, text_width);
        for (i, line) in wrapped_header.into_iter().enumerate() {
            let raw: String = line
                .spans
                .into_iter()
                .map(|s| s.content.into_owned())
                .collect();
            let prefix_span = if i == 0 {
                Span::styled("◎ ", header_style)
            } else {
                Span::raw("  ")
            };
            lines.push(Line::from(vec![prefix_span, Span::raw(raw)]));
        }

        let met_count = self.report.criteria_met.len();
        let total_count = met_count + self.report.criteria_open.len();
        // Sättigender Cast (siehe Typdokumentation): NaN/negativ → 0, kein Panic.
        let coverage_pct = (self.report.coverage * 100.0).round() as u32;
        lines.push(Line::from(Span::raw(format!(
            "Kriterien: {met_count} von {total_count} erfüllt ({coverage_pct} %)"
        ))));

        let mut criteria: Vec<(usize, bool)> = self
            .report
            .criteria_met
            .iter()
            .map(|&idx| (idx, true))
            .chain(self.report.criteria_open.iter().map(|&idx| (idx, false)))
            .collect();
        criteria.sort_by_key(|&(idx, _)| idx);
        for (idx, is_met) in criteria {
            let (label, crit_style) = if is_met {
                ("erfüllt", style::success_style(theme))
            } else {
                ("offen", style::dim_style(theme))
            };
            lines.push(Line::from(Span::styled(
                format!("  Kriterium {}: {label}", idx + 1),
                crit_style,
            )));
        }

        for invariant_id in &self.report.invariants_violated {
            lines.push(Line::from(Span::styled(
                format!("  Invariante '{invariant_id}' nicht belegt"),
                style::error_style(theme),
            )));
        }

        lines
    }
}

// ─── Hilfsfunktion ───────────────────────────────────────────────────────────

/// Bricht `text` wortweise auf Zeilen der Breite `width` um.
///
/// # Beschreibung
/// Wörter werden an Leerzeichen getrennt und auf Zeilen aufgeteilt. Passt ein
/// einzelnes Wort nicht in die Spaltenbreite, wird es hart an `width` Stellen
/// aufgeteilt (harte Trennung als Fallback). `width == 0` wird wie `1` behandelt.
///
/// Leerer `text` ergibt eine leere [`Vec`]. Zeilenumbrüche im Eingabetext (`\n`)
/// erzeugen explizite Zeilentrennungen.
///
/// # Argumente
/// - `text` (`&str`): Der zu brechende Text (borrowed; wird intern geklont).
/// - `width` (`u16`): Ziel-Spaltenbreite. `0` → 1 Spalte.
///
/// # Rückgabe
/// `Vec<Line<'static>>` — jede Zeile ist ein einzelner [`Span`] mit unveränderlichem
/// Inhalt.
///
/// # Panics
/// Keine.
///
/// # Beispiele
/// ```ignore
/// use harw_tui::history_cell::wrap_plain;
/// let lines = wrap_plain("Hallo schöne Welt", 10);
/// assert!(lines.len() >= 2);
/// ```
///
/// Spec-Quelle: `00-harw-tui-redesign-spec.md` SLICE 7.
pub(crate) fn wrap_plain(text: &str, width: u16) -> Vec<Line<'static>> {
    let col_width = (width as usize).max(1);
    let mut result: Vec<Line<'static>> = Vec::new();

    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            result.push(Line::from(""));
            continue;
        }

        let mut current_line = String::new();
        let mut current_len: usize = 0;

        for word in paragraph.split_ascii_whitespace() {
            let word_len = word.chars().count();

            if word_len >= col_width {
                // Harte Trennung: aktuell akkumulierte Zeile erst flushen
                if !current_line.is_empty() {
                    result.push(Line::from(Span::raw(current_line.clone())));
                    current_line.clear();
                    current_len = 0;
                }
                // Wort selbst in col_width-Häppchen aufteilen
                let chars: Vec<char> = word.chars().collect();
                let mut offset = 0;
                while offset < chars.len() {
                    let end = (offset + col_width).min(chars.len());
                    let chunk: String = chars[offset..end].iter().collect();
                    result.push(Line::from(Span::raw(chunk)));
                    offset = end;
                }
                continue;
            }

            // Wort passt in eine Zeile
            if current_len == 0 {
                // Erste Wort auf leerer Zeile
                current_line.push_str(word);
                current_len = word_len;
            } else if current_len + 1 + word_len <= col_width {
                // Wort plus Leerzeichen passt noch auf die aktuelle Zeile
                current_line.push(' ');
                current_line.push_str(word);
                current_len += 1 + word_len;
            } else {
                // Aktuelle Zeile fertig; neues Wort auf nächster Zeile
                result.push(Line::from(Span::raw(current_line.clone())));
                current_line = word.to_owned();
                current_len = word_len;
            }
        }

        if !current_line.is_empty() {
            result.push(Line::from(Span::raw(current_line)));
        }
    }

    result
}

/// Kürzt `text` char-sicher auf höchstens `max_chars` Zeichen.
///
/// # Beschreibung
/// Zählt und iteriert über [`char`]s statt über Bytes — ein mehrbyte-
/// UTF-8-Zeichen (Umlaut, Emoji, …) wird daher nie mitten im Zeichen
/// zerschnitten (das wäre bei einer byteweisen Kürzung `&text[..n]` ein
/// möglicher Panic, da `n` keine Zeichengrenze träfe). Ist `text` nicht
/// länger als `max_chars`, wird es unverändert zurückgegeben. Andernfalls
/// werden `max_chars - 1` Zeichen behalten und eine Ellipse (`"…"`)
/// angehängt, sodass das Ergebnis insgesamt höchstens `max_chars` Zeichen
/// umfasst. `max_chars == 0` liefert einen leeren String.
///
/// # Argumente
/// - `text` (`&str`): der zu kürzende Text (borrowed).
/// - `max_chars` (`usize`): maximale Zeichenzahl des Ergebnisses.
///
/// # Rückgabe
/// `String` mit höchstens `max_chars` Zeichen.
///
/// # Panics
/// Keine.
///
/// # Beispiele
/// ```ignore
/// use harw_tui::history_cell::truncate_chars;
/// assert_eq!(truncate_chars("Hallo", 10), "Hallo");
/// assert_eq!(truncate_chars("Hallo Welt", 5), "Hall…");
/// ```
pub(crate) fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    if max_chars == 0 {
        return String::new();
    }
    let keep = max_chars.saturating_sub(1);
    let mut truncated: String = text.chars().take(keep).collect();
    truncated.push('…');
    truncated
}

/// Entfernt Steuerzeichen und ANSI-Escape-Sequenzen aus `text`.
///
/// # Beschreibung
/// Vom Modell gelieferte Werkzeug-Argumente sind nicht vertrauenswürdig und
/// dürfen die Terminal-Darstellung nicht kapern können (z. B. Cursor-
/// Bewegung, Farbwechsel, Bildschirm-Löschung über ANSI-Sequenzen). Ein
/// Escape-Zeichen (`\u{1b}`) leitet eine Sequenz ein:
/// - CSI (`ESC '[' … Abschlussbyte 0x40..=0x7E`): vollständig verschluckt.
/// - OSC (`ESC ']' … BEL`): vollständig verschluckt.
/// - jede andere auf `ESC` folgende Form: nur das `ESC`-Zeichen selbst wird
///   verschluckt (defensiver Fallback statt unbegrenztem Vorgriff).
///
/// `\n`/`\r` werden zu einem Leerzeichen normalisiert (einzeilige Vorschau);
/// alle übrigen [`char::is_control`]-Zeichen (z. B. Tab, BEL außerhalb einer
/// OSC-Sequenz) werden ebenfalls durch ein Leerzeichen ersetzt. Alle
/// übrigen Zeichen bleiben unverändert erhalten.
///
/// # Argumente
/// - `text` (`&str`): der zu bereinigende, nicht vertrauenswürdige Text.
///
/// # Rückgabe
/// `String` ohne Steuerzeichen oder ANSI-Escape-Sequenzen.
///
/// # Panics
/// Keine.
///
/// # Beispiele
/// ```ignore
/// use harw_tui::history_cell::sanitize_terminal_text;
/// let raw = "\u{1b}[31mDANGER\u{1b}[0m";
/// let clean = sanitize_terminal_text(raw);
/// assert_eq!(clean, "DANGER");
/// ```
pub(crate) fn sanitize_terminal_text(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    for nc in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&nc) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    for nc in chars.by_ref() {
                        if nc == '\u{07}' {
                            break;
                        }
                    }
                }
                _ => {
                    // Unbekannte/einfache Escape-Form: nur ESC selbst verschlucken.
                }
            }
            continue;
        }

        if c == '\n' || c == '\r' {
            result.push(' ');
            continue;
        }

        if c.is_control() {
            result.push(' ');
            continue;
        }

        result.push(c);
    }

    result
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use harw_plan::ids::{PathOrSymbol, PlanId, RevisionId, TaskId};
    use harw_plan::{PlanNode, PlanNodeKind};
    use time::OffsetDateTime;

    /// Testhilfe: konkateniert die Spans jeder Zeile zu je einem String (ein
    /// Eintrag pro Zeile), ohne die Zeilen selbst zu verbinden.
    fn lines_to_strings(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    /// Baut einen minimalen `PlanNode` mit den für `PlanGraphCell`-Tests
    /// relevanten Kernfeldern; `kind` ist immer `PlanNodeKind::Coding`.
    fn make_plan_node(
        id: &str,
        status: PlanNodeStatus,
        wave: Option<u32>,
        objective: &str,
    ) -> PlanNode {
        PlanNode {
            id: TaskId::new(id),
            objective: objective.to_owned(),
            dependencies: vec![],
            input_contracts: vec![],
            output_contracts: vec![],
            read_scope: vec![],
            write_scope: vec![PathOrSymbol::new(format!("src/{id}.rs"))],
            forbidden_scope: vec![],
            acceptance_criteria: vec![],
            invalidation_conditions: vec![],
            status,
            evidence: vec![],
            kind: PlanNodeKind::Coding,
            wave,
            assignment: None,
            parent: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    /// Baut einen minimalen `Plan` mit den übergebenen Knoten für
    /// `PlanGraphCell`-Tests.
    fn make_plan(nodes: Vec<PlanNode>) -> Plan {
        Plan {
            id: PlanId::new("p-test"),
            revision: RevisionId::new(1),
            parent_revision: None,
            goal_statement: "Testplan".to_owned(),
            goal_id: None,
            nodes,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    /// Prüft, dass `wrap_plain` einen langen Text bei kleinem `width` in mehrere Zeilen bricht.
    #[test]
    fn test_wrap_plain_breaks_long_line_at_small_width() {
        let text = "Dies ist ein langer Satz der definitiv umgebrochen werden muss";
        let lines = wrap_plain(text, 10);
        assert!(
            lines.len() >= 2,
            "Erwarte mindestens 2 Zeilen bei width=10, erhielt: {}",
            lines.len()
        );
    }

    /// Prüft, dass `AssistantHistoryCell` bei `width=10` mehr Zeilen liefert als bei `width=100`.
    #[test]
    fn test_assistant_cell_more_lines_at_narrow_width() {
        let long_text = "Ein sehr langer Assistenten-Text der bei schmaler Breite \
                         auf viele Zeilen verteilt wird aber bei breiten Terminals \
                         weniger Zeilen benötigt weil mehr Text pro Zeile passt.";
        let cell = AssistantHistoryCell {
            source: long_text.to_owned(),
        };
        let narrow = cell.display_lines(10, style::Theme::Dark);
        let wide = cell.display_lines(100, style::Theme::Dark);
        assert!(
            narrow.len() > wide.len(),
            "Schmal ({}) sollte mehr Zeilen haben als breit ({})",
            narrow.len(),
            wide.len()
        );
    }

    /// Prüft, dass `PlainHistoryCell::desired_height` exakt der Anzahl der gespeicherten Zeilen entspricht.
    #[test]
    fn test_plain_cell_desired_height_equals_line_count() {
        use ratatui::text::Line;
        let lines = vec![
            Line::from("Zeile 1"),
            Line::from("Zeile 2"),
            Line::from("Zeile 3"),
        ];
        let count = lines.len() as u16;
        let cell = PlainHistoryCell { lines };
        assert_eq!(
            cell.desired_height(80, style::Theme::Dark),
            count,
            "desired_height muss der Zeilenanzahl entsprechen"
        );
    }

    /// Prüft, dass `UserHistoryCell::display_lines` den `"> "`-Präfix auf der ersten Zeile enthält.
    #[test]
    fn test_user_cell_has_gt_prefix() {
        let cell = UserHistoryCell {
            text: "Hallo Welt".to_owned(),
        };
        let lines = cell.display_lines(80, style::Theme::Dark);
        assert!(!lines.is_empty(), "Mindestens eine Zeile erwartet");
        let first_content: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            first_content.starts_with("> "),
            "Erste Zeile muss mit '> ' beginnen, war: {:?}",
            first_content
        );
    }

    /// Prüft, dass `ToolCallHistoryCell::display_lines` den `"⚙ "`-Präfix und
    /// das `"{tool_name}({arguments_preview})"`-Format enthält.
    #[test]
    fn test_tool_call_cell_has_gear_prefix_and_format() {
        let cell = ToolCallHistoryCell {
            tool_name: "search".to_owned(),
            arguments_preview: "{\"q\":\"rust\"}".to_owned(),
        };
        let lines = cell.display_lines(80, style::Theme::Dark);
        assert!(!lines.is_empty(), "Mindestens eine Zeile erwartet");
        let first_content: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            first_content, "⚙ search({\"q\":\"rust\"})",
            "Erste Zeile muss Präfix und Format enthalten, war: {:?}",
            first_content
        );
    }

    /// Prüft, dass `ToolResultHistoryCell::display_lines` bei Erfolg den
    /// `"✓ "`-Präfix und das `"{tool_name} ({duration_ms}ms)"`-Format liefert.
    #[test]
    fn test_tool_result_cell_success_has_check_prefix() {
        let cell = ToolResultHistoryCell {
            tool_name: "search".to_owned(),
            success: true,
            duration_ms: 42,
        };
        let lines = cell.display_lines(80, style::Theme::Dark);
        assert!(!lines.is_empty(), "Mindestens eine Zeile erwartet");
        let first_content: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            first_content, "✓ search (42ms)",
            "Erste Zeile muss Erfolgs-Präfix und Format enthalten, war: {:?}",
            first_content
        );
    }

    /// Prüft, dass `ToolResultHistoryCell::display_lines` bei Fehlschlag den
    /// `"✗ "`-Präfix liefert.
    #[test]
    fn test_tool_result_cell_failure_has_cross_prefix() {
        let cell = ToolResultHistoryCell {
            tool_name: "search".to_owned(),
            success: false,
            duration_ms: 7,
        };
        let lines = cell.display_lines(80, style::Theme::Dark);
        assert!(!lines.is_empty(), "Mindestens eine Zeile erwartet");
        let first_content: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            first_content, "✗ search (7ms)",
            "Erste Zeile muss Fehler-Präfix und Format enthalten, war: {:?}",
            first_content
        );
    }

    /// Prüft, dass `ReasoningHistoryCell::display_lines` den `"· "`-Präfix
    /// gefolgt vom Zusammenfassungstext liefert.
    #[test]
    fn test_reasoning_cell_has_dot_prefix() {
        let cell = ReasoningHistoryCell {
            summary: "Denke über die Lösung nach".to_owned(),
        };
        let lines = cell.display_lines(80, style::Theme::Dark);
        assert!(!lines.is_empty(), "Mindestens eine Zeile erwartet");
        let first_content: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            first_content, "· Denke über die Lösung nach",
            "Erste Zeile muss mit '· ' beginnen, war: {:?}",
            first_content
        );
    }

    // ── SubAgentCell ────────────────────────────────────────────────────

    /// Prüft, dass `SubAgentCell::display_lines` Rolle, gekürzte Frage,
    /// Laufzeit-Status und Zähler in der ersten Zeile enthält.
    #[test]
    fn test_subagent_cell_renders_running_state_with_role_question_and_counts() {
        let cell = SubAgentCell {
            child_id: "child-1".to_owned(),
            role: "explorer".to_owned(),
            question: Some("Wie sieht X aus?".to_owned()),
            tool_calls: 3,
            tokens: 128,
            status: SubAgentStatus::Running,
        };
        let lines = cell.display_lines(120, style::Theme::Dark);
        let rendered = lines_to_strings(&lines);
        // Die Kind-Kennung steht in der Zeile, weil bei einem Fan-out mehrere
        // Kinder derselben Rolle laufen und sonst nicht unterscheidbar wären.
        assert_eq!(
            rendered[0], "▶ explorer [child-1]: Wie sieht X aus? — läuft · 3 Tools · 128 Tok",
            "war: {:?}",
            rendered[0]
        );
    }

    /// Zwei gleichzeitig laufende Kinder derselben Rolle bleiben in der
    /// Darstellung unterscheidbar — der eigentliche Zweck der Kind-Kennung.
    #[test]
    fn test_subagent_cells_of_the_same_role_stay_distinguishable() {
        let render = |child: &str| {
            let cell = SubAgentCell {
                child_id: child.to_owned(),
                role: "explorer".to_owned(),
                question: Some("Wie sieht X aus?".to_owned()),
                tool_calls: 0,
                tokens: 0,
                status: SubAgentStatus::Running,
            };
            lines_to_strings(&cell.display_lines(120, style::Theme::Dark))[0].clone()
        };

        // Gemeinsamer Präfix, Unterschied am Ende — der realistische Fall bei
        // Session-Kennungen und genau der, an dem eine Kürzung von hinten
        // scheitern würde.
        let first = render("child-explorer-1");
        let second = render("child-explorer-2");
        assert_ne!(
            first, second,
            "zwei Kinder derselben Rolle dürfen nicht identisch gerendert werden"
        );
        assert!(first.ends_with("· 0 Tok"), "war: {first:?}");
        assert!(
            first.contains("orer-1]"),
            "das unterscheidende Ende muss sichtbar bleiben, war: {first:?}"
        );
    }

    /// `truncate_id_tail` behält das Ende und schneidet nie in ein Zeichen.
    #[test]
    fn test_truncate_id_tail_keeps_the_distinguishing_end() {
        assert_eq!(truncate_id_tail("kurz", 12), "kurz");
        assert_eq!(truncate_id_tail("child-explorer-1", 12), "…-explorer-1");
        assert_eq!(truncate_id_tail("child-explorer-2", 12), "…-explorer-2");
        // Mehrbyte-Zeichen an der Schnittstelle: die Zählung läuft über `char`,
        // nicht über Bytes — sonst entstünde ungültiges UTF-8. "aaaaaaaaaaüber"
        // sind 14 Zeichen (aber 15 Bytes); bei `max = 6` bleiben die letzten 5.
        assert_eq!(truncate_id_tail("aaaaaaaaaaüber", 6), "…aüber");
        assert_eq!(truncate_id_tail("egal", 0), "egal");
    }

    /// Prüft, dass ein fehlender Auftrag als Platzhaltertext gerendert wird.
    #[test]
    fn test_subagent_cell_no_question_shows_placeholder() {
        let cell = SubAgentCell {
            child_id: "child-2".to_owned(),
            role: "worker".to_owned(),
            question: None,
            tool_calls: 0,
            tokens: 0,
            status: SubAgentStatus::Running,
        };
        let lines = cell.display_lines(120, style::Theme::Dark);
        let rendered = lines_to_strings(&lines);
        assert!(
            rendered[0].contains("(kein Auftrag angegeben)"),
            "war: {:?}",
            rendered[0]
        );
    }

    /// Prüft, dass `apply_progress` Tool-Aufruf-Zahl und Token aktualisiert
    /// und die Änderung im Render sichtbar wird.
    #[test]
    fn test_subagent_cell_apply_progress_updates_counts() {
        let mut cell = SubAgentCell {
            child_id: "child-3".to_owned(),
            role: "explorer".to_owned(),
            question: None,
            tool_calls: 0,
            tokens: 0,
            status: SubAgentStatus::Running,
        };
        cell.apply_progress(5, 999);
        assert_eq!(cell.tool_calls, 5);
        assert_eq!(cell.tokens, 999);
        let lines = cell.display_lines(120, style::Theme::Dark);
        let rendered = lines_to_strings(&lines);
        assert!(
            rendered[0].contains("5 Tools · 999 Tok"),
            "war: {:?}",
            rendered[0]
        );
    }

    /// Prüft, dass `apply_completion` bei Erfolg (`outcome == "completed"`)
    /// den Erfolgstext `"fertig"` mit grünem Präfix rendert.
    #[test]
    fn test_subagent_cell_apply_completion_success_shows_fertig() {
        let mut cell = SubAgentCell {
            child_id: "child-4".to_owned(),
            role: "explorer".to_owned(),
            question: None,
            tool_calls: 2,
            tokens: 64,
            status: SubAgentStatus::Running,
        };
        cell.apply_completion("completed", 512);
        assert_eq!(
            cell.status,
            SubAgentStatus::Done {
                outcome: "completed".to_owned(),
                duration_ms: 512
            }
        );
        let lines = cell.display_lines(120, style::Theme::Dark);
        let rendered = lines_to_strings(&lines);
        assert!(
            rendered[0].starts_with("✓ ") && rendered[0].contains("fertig (512ms)"),
            "war: {:?}",
            rendered[0]
        );
    }

    /// Prüft, dass `apply_completion` bei Fehlschlag (`outcome != "completed"`)
    /// den Fehlschlagstext `"gescheitert"` inklusive Outcome rendert.
    #[test]
    fn test_subagent_cell_apply_completion_failure_shows_gescheitert() {
        let mut cell = SubAgentCell {
            child_id: "child-5".to_owned(),
            role: "explorer".to_owned(),
            question: None,
            tool_calls: 1,
            tokens: 10,
            status: SubAgentStatus::Running,
        };
        cell.apply_completion("budget_exceeded", 900);
        let lines = cell.display_lines(120, style::Theme::Dark);
        let rendered = lines_to_strings(&lines);
        assert!(
            rendered[0].starts_with("✗ ")
                && rendered[0].contains("gescheitert: budget_exceeded (900ms)"),
            "war: {:?}",
            rendered[0]
        );
    }

    // ── PlanGraphCell ───────────────────────────────────────────────────

    /// Prüft, dass `PlanGraphCell::display_lines` ID, Art, Status, Welle und
    /// gekürztes Ziel je Knoten enthält.
    #[test]
    fn test_plan_graph_cell_renders_node_fields() {
        let node = make_plan_node(
            "t-1",
            PlanNodeStatus::Ready,
            Some(2),
            "Auth-Modul refactorn",
        );
        let plan = make_plan(vec![node]);
        let cell = PlanGraphCell { plan };
        let lines = cell.display_lines(120, style::Theme::Dark);
        let rendered = lines_to_strings(&lines);
        let joined = rendered.join("\n");
        assert!(joined.contains("t-1"), "war: {joined:?}");
        assert!(joined.contains("Coding"), "war: {joined:?}");
        assert!(joined.contains("Ready"), "war: {joined:?}");
        assert!(joined.contains("Welle 2"), "war: {joined:?}");
        assert!(joined.contains("Auth-Modul refactorn"), "war: {joined:?}");
    }

    /// Prüft, dass `PlanGraphCell` bei mehr als `PLAN_GRAPH_MAX_NODES`
    /// Knoten kürzt und die Zahl der ausgelassenen Knoten in einer eigenen
    /// Sammelzeile nennt.
    #[test]
    fn test_plan_graph_cell_truncates_and_reports_elided_count() {
        let total_nodes = PLAN_GRAPH_MAX_NODES + 5;
        let nodes: Vec<PlanNode> = (0..total_nodes)
            .map(|i| make_plan_node(&format!("t-{i}"), PlanNodeStatus::Draft, None, "Ziel"))
            .collect();
        let plan = make_plan(nodes);
        let cell = PlanGraphCell { plan };
        let lines = cell.display_lines(120, style::Theme::Dark);

        assert_eq!(
            lines.len(),
            PLAN_GRAPH_MAX_NODES + 1,
            "erwartet {} Knotenzeilen + 1 Sammelzeile, erhalten {}",
            PLAN_GRAPH_MAX_NODES,
            lines.len()
        );

        let rendered = lines_to_strings(&lines);
        let last_content = &rendered[rendered.len() - 1];
        assert_eq!(
            last_content,
            &format!("… 5 weitere Knoten ausgeblendet (insgesamt {total_nodes})"),
            "war: {last_content:?}"
        );
    }

    /// Prüft, dass ein Plan ohne Knoten eine einzelne Platzhalterzeile
    /// rendert statt einer leeren Zeilenliste.
    #[test]
    fn test_plan_graph_cell_empty_plan_shows_placeholder() {
        let plan = make_plan(vec![]);
        let cell = PlanGraphCell { plan };
        let lines = cell.display_lines(80, style::Theme::Dark);
        let rendered = lines_to_strings(&lines);
        assert_eq!(rendered, vec!["(keine Knoten im Plan)".to_owned()]);
    }

    // ── ApprovalPromptCell ──────────────────────────────────────────────

    /// Prüft, dass die ausstehende Freigabefrage Werkzeugname, Argumente und
    /// Tastenbelegung enthält.
    #[test]
    fn test_approval_prompt_cell_shows_pending_question_and_keybindings() {
        let cell = ApprovalPromptCell {
            tool_name: "run_shell".to_owned(),
            arguments_raw: "{\"cmd\":\"ls\"}".to_owned(),
            decision: None,
        };
        let lines = cell.display_lines(80, style::Theme::Dark);
        let rendered = lines_to_strings(&lines);
        assert_eq!(
            rendered,
            vec![
                "⚠ Freigabe erforderlich: run_shell".to_owned(),
                "  Argumente: {\"cmd\":\"ls\"}".to_owned(),
                "  [y] freigeben · [n] ablehnen".to_owned(),
            ]
        );
    }

    /// Prüft, dass ANSI-Escape-Sequenzen und Steuerzeichen aus den
    /// dargestellten Argumenten entfernt werden, sichtbarer Text aber
    /// erhalten bleibt.
    #[test]
    fn test_approval_prompt_cell_sanitizes_ansi_and_control_chars() {
        let raw_args = "\u{1b}[31mDANGER\u{1b}[0m\u{07} und \ttab".to_owned();
        let cell = ApprovalPromptCell {
            tool_name: "x".to_owned(),
            arguments_raw: raw_args,
            decision: None,
        };
        let lines = cell.display_lines(120, style::Theme::Dark);
        let joined = lines_to_strings(&lines).join("\n");
        assert!(
            !joined.contains('\u{1b}'),
            "ESC-Zeichen darf nicht mehr vorkommen: {joined:?}"
        );
        assert!(
            !joined.contains('\u{07}'),
            "BEL-Zeichen darf nicht mehr vorkommen: {joined:?}"
        );
        assert!(
            joined.contains("DANGER"),
            "sichtbarer Text muss erhalten bleiben: {joined:?}"
        );
        assert!(
            joined.contains("tab"),
            "sichtbarer Text muss erhalten bleiben: {joined:?}"
        );
    }

    /// Prüft, dass nach `apply_decision(true)` dieselbe Zelle das
    /// Freigabe-Ergebnis statt der Frage zeigt.
    #[test]
    fn test_approval_prompt_cell_shows_result_after_approval() {
        let mut cell = ApprovalPromptCell {
            tool_name: "run_shell".to_owned(),
            arguments_raw: "{}".to_owned(),
            decision: None,
        };
        cell.apply_decision(true);
        assert_eq!(cell.decision, Some(true));

        let lines = cell.display_lines(80, style::Theme::Dark);
        let rendered = lines_to_strings(&lines);
        assert_eq!(rendered, vec!["✓ run_shell — freigegeben".to_owned()]);
    }

    /// Prüft, dass nach `apply_decision(false)` dieselbe Zelle das
    /// Ablehnungs-Ergebnis statt der Frage zeigt.
    #[test]
    fn test_approval_prompt_cell_shows_result_after_rejection() {
        let mut cell = ApprovalPromptCell {
            tool_name: "run_shell".to_owned(),
            arguments_raw: "{}".to_owned(),
            decision: None,
        };
        cell.apply_decision(false);

        let lines = cell.display_lines(80, style::Theme::Dark);
        let rendered = lines_to_strings(&lines);
        assert_eq!(rendered, vec!["✗ run_shell — abgelehnt".to_owned()]);
    }

    // ── GoalCell ────────────────────────────────────────────────────────

    /// Prüft, dass `GoalCell` Ziel-Statement, Coverage, Einzelkriterien
    /// (1-basiert) und verletzte Invarianten rendert.
    #[test]
    fn test_goal_cell_renders_statement_and_criteria() {
        let report = GoalReport {
            criteria_met: vec![0],
            criteria_open: vec![1],
            invariants_violated: vec!["inv-1".to_owned()],
            coverage: 0.5,
            blocking_nodes: vec![],
            next_actions: vec![],
        };
        let cell = GoalCell {
            statement: "Testziel".to_owned(),
            report,
        };
        let lines = cell.display_lines(120, style::Theme::Dark);
        let joined = lines_to_strings(&lines).join("\n");

        assert!(joined.contains("◎ Ziel: Testziel"), "war: {joined:?}");
        assert!(
            joined.contains("Kriterien: 1 von 2 erfüllt (50 %)"),
            "war: {joined:?}"
        );
        assert!(joined.contains("Kriterium 1: erfüllt"), "war: {joined:?}");
        assert!(joined.contains("Kriterium 2: offen"), "war: {joined:?}");
        assert!(
            joined.contains("Invariante 'inv-1' nicht belegt"),
            "war: {joined:?}"
        );
    }

    /// Pflichtfall: 0 von 3 erfüllten Kriterien zeigt `0 %`, niemals `NaN`.
    #[test]
    fn test_goal_cell_zero_of_three_shows_zero_percent_not_nan() {
        let report = GoalReport {
            criteria_met: vec![],
            criteria_open: vec![0, 1, 2],
            invariants_violated: vec![],
            coverage: 0.0,
            blocking_nodes: vec![],
            next_actions: vec![],
        };
        let cell = GoalCell {
            statement: "Ziel".to_owned(),
            report,
        };
        let lines = cell.display_lines(120, style::Theme::Dark);
        let joined = lines_to_strings(&lines).join("\n");
        assert!(
            joined.contains("Kriterien: 0 von 3 erfüllt (0 %)"),
            "war: {joined:?}"
        );
        assert!(
            !joined.contains("NaN"),
            "darf niemals NaN anzeigen: {joined:?}"
        );
    }

    /// Pflichtfall: eine leere Kriterienliste (Division durch null bei
    /// `criteria_met.len() / acceptance_criteria.len()` in `evaluate_goal`)
    /// zeigt `100 %` (per `evaluate_goal`-Konvention), niemals `NaN`.
    #[test]
    fn test_goal_cell_empty_criteria_list_shows_hundred_percent_not_nan() {
        let report = GoalReport {
            criteria_met: vec![],
            criteria_open: vec![],
            invariants_violated: vec![],
            coverage: 1.0,
            blocking_nodes: vec![],
            next_actions: vec![],
        };
        let cell = GoalCell {
            statement: "Ziel".to_owned(),
            report,
        };
        let lines = cell.display_lines(120, style::Theme::Dark);
        let joined = lines_to_strings(&lines).join("\n");
        assert!(
            joined.contains("Kriterien: 0 von 0 erfüllt (100 %)"),
            "war: {joined:?}"
        );
        assert!(
            !joined.contains("NaN"),
            "darf niemals NaN anzeigen: {joined:?}"
        );
    }

    /// Verteidigungstest unabhängig von `evaluate_goal`: selbst eine
    /// pathologische `NaN`-Coverage darf `GoalCell` nie als `"NaN"`
    /// rendern — der sättigende `as u32`-Cast (seit Rust 1.45) muss auf
    /// `0 %` abbilden.
    #[test]
    fn test_goal_cell_nan_coverage_saturates_to_zero_percent() {
        let report = GoalReport {
            criteria_met: vec![],
            criteria_open: vec![],
            invariants_violated: vec![],
            coverage: f32::NAN,
            blocking_nodes: vec![],
            next_actions: vec![],
        };
        let cell = GoalCell {
            statement: "Ziel".to_owned(),
            report,
        };
        let lines = cell.display_lines(120, style::Theme::Dark);
        let joined = lines_to_strings(&lines).join("\n");
        assert!(
            joined.contains("(0 %)"),
            "NaN-Coverage muss auf 0 % sättigen: {joined:?}"
        );
        assert!(
            !joined.contains("NaN"),
            "darf niemals NaN anzeigen: {joined:?}"
        );
    }

    // ── truncate_chars / sanitize_terminal_text ────────────────────────

    /// Prüft, dass `truncate_chars` einen Text innerhalb des Limits
    /// unverändert zurückgibt.
    #[test]
    fn test_truncate_chars_returns_unchanged_when_within_limit() {
        assert_eq!(truncate_chars("Hallo", 10), "Hallo");
    }

    /// Pflichtfall: Kürzung an einem mehrbyte-Zeichen (hier: Emoji) panikt
    /// nicht und schneidet nicht mitten im Zeichen.
    #[test]
    fn test_truncate_chars_does_not_split_multibyte_emoji() {
        let text = "🎉🎉🎉🎉🎉";
        let truncated = truncate_chars(text, 3);
        assert_eq!(truncated, "🎉🎉…");
        assert_eq!(truncated.chars().count(), 3);
    }

    /// Pflichtfall: Kürzung eines Texts mit deutschen Umlauten (mehrbyte in
    /// UTF-8) panikt nicht und liefert eine gültige, exakt begrenzte Kürzung.
    #[test]
    fn test_truncate_chars_umlaut_source_text_stays_valid_utf8() {
        let text = "Ziel: Änderung prüfen und abschließen";
        let truncated = truncate_chars(text, 12);
        assert_eq!(truncated.chars().count(), 12);
        assert!(truncated.ends_with('…'), "war: {truncated:?}");
    }

    /// Prüft, dass `sanitize_terminal_text` ANSI-CSI-Sequenzen vollständig
    /// entfernt, sichtbaren Text aber erhält.
    #[test]
    fn test_sanitize_terminal_text_strips_ansi_and_control_chars() {
        let raw = "\u{1b}[31mRED\u{1b}[0m normal";
        let clean = sanitize_terminal_text(raw);
        assert_eq!(clean, "RED normal");
    }
}
