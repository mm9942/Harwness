//! Typisierte Ausgabe-Zellen für die Chat-History.
//!
//! # Verantwortung
//! Dieses Modul definiert das [`HistoryCell`]-Trait sowie neun konkrete
//! Implementierungen (`PlainHistoryCell`, `UserHistoryCell`,
//! `AssistantHistoryCell`, `ReasoningHistoryCell`, `SubAgentCell`,
//! `PlanGraphCell`, `GoalCell`, `ToolCell`, `ToolGroupCell`) sowie die freien
//! Hilfsfunktionen [`wrap_plain`] (wortweises Umbruchverhalten) und
//! [`truncate_chars`] (zeichensichere Kürzung).
//!
//! Die früheren Verlaufszellen `ToolCallHistoryCell`/`ToolResultHistoryCell`
//! (angeforderter bzw. abgeschlossener Werkzeugaufruf als zwei separate,
//! unveränderliche Items) und `ApprovalPromptCell`/`ApprovalPromptView`
//! (Freigabefrage als Verlaufszelle) sind entfallen: Werkzeugaufrufe laufen
//! seit Plan Schritt 2 vollständig über [`ToolCell`]/[`ToolGroupCell`]
//! (eine geteilte, über den Lebenszyklus fortgeschriebene Zelle statt zweier
//! Items), die Freigabe läuft seit der Panel-Umstellung ausschließlich über
//! [`crate::approval_dialog::ApprovalDialog`] (kein Verlaufseintrag mehr
//! während die Frage offen ist). `ApprovalArgument`/`ApprovalArgumentValue`
//! bleiben bestehen, weil `approval_dialog.rs` genau diese Zerlegung eines
//! `ToolCall` weiterverwendet.
//!
//! # Terminal-Sicherheit (W1-08, G-007/G-008)
//! **Jeder** Text, der nicht aus einem festen Literal dieses Moduls stammt
//! (Modell-, Werkzeug-, Plan-, Ziel- und Nutzertext), läuft vor dem Rendern
//! durch eine Funktion aus [`crate::sanitize`]: Fließtext über
//! `sanitize_display`, einzeilige Felder über `sanitize_inline` (die
//! Freigabefrage selbst rendert `approval_dialog.rs` und sanitisiert dort
//! über `sanitize_reveal`/`sanitize_reveal_inline` — nichts wird verschluckt,
//! damit sichtbar ist, was freigegeben wird). ESC-Sequenzen,
//! C0/C1-Steuerzeichen, Bidi- und Zero-Width-Zeichen erreichen damit nie den
//! ratatui-Buffer. Eine Kürzung (`truncate_chars`) erfolgt immer **vor** der
//! Bereinigung, damit keine Markierung `⟨U+XXXX⟩` zerschnitten wird.
//!
//! # Schlüsseltypen
//! - [`HistoryCell`]: Trait — jede Zelle kennt ihre eigene Render-Logik.
//! - [`PlainHistoryCell`]: Unveränderliche Zeilen (z.B. System-Nachrichten).
//! - [`UserHistoryCell`]: Nutzer-Eingabe mit `"> "`-Präfix und Wort-Wrapping.
//! - [`AssistantHistoryCell`]: Assistenten-Antwort, speichert Quelltext; bricht bei
//!   unterschiedlichem `width` unterschiedlich um (Re-Render-on-Resize).
//! - [`ReasoningHistoryCell`]: Reasoning-Zusammenfassung, gedimmt-kursiv mit
//!   `"∴ "`-Markierung und optionaler Agenten-Herkunft; standardmäßig
//!   eingeklappt, per Ctrl+O über [`SharedReasoningCell`] ausklappbar.
//! - [`SubAgentCell`]: laufender/beendeter Kind-Agent (`TurnEvent::ChildSpawned`
//!   / `ChildProgress` / `ChildCompleted`); **aktualisierbar** über
//!   [`SubAgentCell::apply_progress`] / [`SubAgentCell::apply_completion`].
//! - [`PlanGraphCell`]: kompakte Übersicht eines `harw_plan::Plan`, kürzt bei
//!   vielen Knoten und nennt die Zahl der ausgelassenen.
//! - [`GoalCell`]: Ziel-Statement gegen einen `harw_plan::goal::GoalReport`.
//! - [`ToolCell`]: Claude-Code-artige Darstellung **eines** Werkzeugaufrufs
//!   über seinen gesamten Lebenszyklus (angefordert → läuft → abgeschlossen),
//!   geteilt über [`SharedToolCell`] und aktualisiert über
//!   [`ToolCell::complete`] (Plan Schritt 2, Contract A5) — ersetzt
//!   vollständig die früheren Alttypen `ToolCallHistoryCell`/
//!   `ToolResultHistoryCell` (siehe Modul-Verantwortung oben).
//! - [`ToolGroupCell`]: fasst aufeinanderfolgende lesende `fs.*`-Aufrufe
//!   (`fs.read`, `fs.search`, `fs.grep`, `fs.list`, `fs.glob`) zu einer
//!   Sammelzeile zusammen.
//!
//! # Verbosity (Schritt 2)
//! [`ToolCell`] und [`ToolGroupCell`] rendern zusätzlich über
//! [`ToolCell::display_lines_with`] / [`ToolGroupCell::display_lines_with`]
//! mit einem [`ToolVerbosity`]-Parameter: `Verbose` verhält sich wie
//! ausgeklappt und zeigt zusätzlich die rohen Aufrufargumente (eingerückt
//! als JSON) unter dem Label.
//!
//! # Nebenläufigkeit
//! Alle Typen implementieren [`Send`] + [`Sync`] (erzwungen durch den Trait-Bound).
//! `SubAgentCell` und `ToolCell` sind intern veränderlich (`&mut self`-
//! Methoden), aber nicht selbst synchronisiert — geteilter Zugriff über Threads
//! erfordert wie bei jedem `&mut`-Typ eine äußere Synchronisation durch den Aufrufer
//! (bei `ToolCell` über [`SharedToolCell`] = `Arc<Mutex<ToolCell>>`).
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
//! Spec-Quelle: `docs/design/tui-architecture.md` §2.10 / SLICE 7
//! und `docs/design/tui-architecture.md` §3 sowie
//! AP W5-01 / W5-10a.

use std::sync::{Arc, Mutex};

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use harw_extension_api::ToolCall;
use harw_plan::goal::GoalReport;
use harw_plan::{Plan, PlanNode, PlanNodeStatus, TaskId};
use harw_protocol::items::{ToolCallResult, ToolPlacement};

use crate::sanitize::{sanitize_display, sanitize_inline};
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
/// werden bei `display_lines()` geklont; Stil und Ausrichtung bleiben erhalten,
/// der Inhalt jedes Spans läuft durch `sanitize_inline` (Command-Ausgaben wie
/// `/diff` können Werkzeug- oder Dateitext mit ESC-Sequenzen tragen).
///
/// # Felder
/// - `lines` (`Vec<Line<'static>>`): Vorgerenderte Zeilen.
///
/// # Spec-Referenz
/// `docs/design/tui-architecture.md`.
#[derive(Debug)]
pub(crate) struct PlainHistoryCell {
    /// Vorgerenderte, breitenunabhängige Ausgabezeilen.
    pub lines: Vec<Line<'static>>,
}

impl HistoryCell for PlainHistoryCell {
    /// Gibt einen bereinigten Klon der gespeicherten Zeilen zurück; ignoriert `width`.
    ///
    /// # Argumente
    /// - `width` (`u16`): Wird ignoriert, da die Zeilen bereits finalisiert sind.
    ///
    /// # Rückgabe
    /// Geklonte Liste der internen Zeilen, jeder Span-Inhalt terminal-sicher.
    fn display_lines(&self, _width: u16, _theme: style::Theme) -> Vec<Line<'static>> {
        let mut lines = self.lines.clone();
        for line in &mut lines {
            for span in &mut line.spans {
                span.content = sanitize_inline(&span.content).into();
            }
        }
        lines
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
/// `docs/design/tui-architecture.md`.
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
        let text = sanitize_display(&self.text);

        if text.is_empty() {
            return vec![Line::from(vec![Span::styled("> ", user_style)])];
        }

        let wrapped = wrap_plain(&text, text_width);
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
/// unterschiedliche Terminalbreiten an (z.B. nach einem Resize-Event). Der Text
/// wird als Markdown gerendert ([`crate::markdown::render_markdown`]) und
/// anschließend stilerhaltend wortweise umbrochen.
///
/// # Felder
/// - `source` (`String`): Quelltext der Assistenten-Antwort.
///
/// # Nebenläufigkeit
/// Lesen auf `source` ist nebenläufig sicher; [`Send`] + [`Sync`] sind ableitbar,
/// da `String` beide Bounds erfüllt.
///
/// # Spec-Referenz
/// `docs/design/tui-architecture.md`
/// ("`AgentMarkdownCell { source }` — Re-rendert bei jeder `display_lines(width)`-Anfrage").
#[derive(Debug)]
pub(crate) struct AssistantHistoryCell {
    /// Quelltext der Assistenten-Antwort; wird bei jedem Render neu umbrochen.
    pub source: String,
}

impl HistoryCell for AssistantHistoryCell {
    /// Rendert den Quelltext als Markdown (Überschriften, Listen, Code mit
    /// Hervorhebung, Tabellen, Links — siehe [`crate::markdown`]) und setzt
    /// einen `» `-Präfix auf die erste Zeile (Folgezeilen: `  `), damit
    /// Assistenten-Antworten visuell klar von Nutzer-Zeilen (`> `-Präfix, grün)
    /// unterschieden werden können. Das Umbrechen übernimmt der Verlauf
    /// (`Paragraph` mit `Wrap`).
    fn display_lines(&self, width: u16, theme: style::Theme) -> Vec<Line<'static>> {
        let assistant_style = style::assistant_style(theme);
        let text_width = usize::from(width.saturating_sub(2).max(1));
        let mut lines: Vec<Line<'static>> = crate::markdown::render_markdown(&self.source, theme)
            .into_iter()
            .flat_map(|line| wrap_styled(line, text_width))
            .collect();
        if lines.is_empty() {
            lines.push(Line::raw(" "));
        }
        lines
            .into_iter()
            .enumerate()
            .map(|(i, line)| {
                let prefix_span = if i == 0 {
                    Span::styled("» ", assistant_style)
                } else {
                    Span::raw("  ")
                };
                let mut spans = Vec::with_capacity(line.spans.len() + 1);
                spans.push(prefix_span);
                spans.extend(line.spans);
                Line::from(spans).style(line.style)
            })
            .collect()
    }
}

/// Bricht eine gestylte Zeile wortweise auf `width` Spalten um und erhält
/// dabei die Styles der einzelnen Spans. Wörter, die allein breiter sind als
/// `width`, werden hart geteilt.
fn wrap_styled(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    use unicode_width::UnicodeWidthChar;
    let width = width.max(1);
    let line_style = line.style;
    let total: usize = line
        .spans
        .iter()
        .map(|span| unicode_width::UnicodeWidthStr::width(span.content.as_ref()))
        .sum();
    if total <= width {
        return vec![line];
    }
    // Tokens: (Text, Style, ist Leerraum)
    let mut tokens: Vec<(String, ratatui::style::Style, bool)> = Vec::new();
    for span in &line.spans {
        let mut current = String::new();
        let mut current_ws: Option<bool> = None;
        for ch in span.content.chars() {
            let ws = ch == ' ';
            if current_ws.is_some_and(|prev| prev != ws) {
                tokens.push((
                    std::mem::take(&mut current),
                    span.style,
                    current_ws == Some(true),
                ));
            }
            current.push(ch);
            current_ws = Some(ws);
        }
        if !current.is_empty() {
            tokens.push((current, span.style, current_ws == Some(true)));
        }
    }
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    let flush = |spans: &mut Vec<Span<'static>>, out: &mut Vec<Line<'static>>, used: &mut usize| {
        out.push(Line::from(std::mem::take(spans)).style(line_style));
        *used = 0;
    };
    for (text, span_style, is_ws) in tokens {
        let token_width = unicode_width::UnicodeWidthStr::width(text.as_str());
        if is_ws {
            // Leerraum am Zeilenumbruch entfällt.
            if used > 0 && used + token_width <= width {
                spans.push(Span::styled(text, span_style));
                used += token_width;
            } else if used > 0 {
                flush(&mut spans, &mut out, &mut used);
            }
            continue;
        }
        if used + token_width <= width {
            spans.push(Span::styled(text, span_style));
            used += token_width;
            continue;
        }
        if used > 0 && token_width <= width {
            flush(&mut spans, &mut out, &mut used);
            spans.push(Span::styled(text, span_style));
            used = token_width;
            continue;
        }
        // Überlanges Wort: zeichenweise teilen.
        let mut chunk = String::new();
        for ch in text.chars() {
            let cw = ch.width().unwrap_or(0);
            if used + cw > width && used > 0 {
                if !chunk.is_empty() {
                    spans.push(Span::styled(std::mem::take(&mut chunk), span_style));
                }
                flush(&mut spans, &mut out, &mut used);
            }
            chunk.push(ch);
            used += cw;
        }
        if !chunk.is_empty() {
            spans.push(Span::styled(chunk, span_style));
        }
    }
    if !spans.is_empty() || out.is_empty() {
        out.push(Line::from(spans).style(line_style));
    }
    out
}

// ─── ReasoningHistoryCell ─────────────────────────────────────────────────────

/// Markierung der ersten Zeile einer [`ReasoningHistoryCell`].
const REASONING_MARKER: &str = "∴ ";

/// Obergrenze (in Zeichen) der Herkunftskennung einer [`ReasoningHistoryCell`]
/// (`[rolle] `), bevor char-sicher mit [`truncate_chars`] gekürzt wird.
const REASONING_ORIGIN_MAX_CHARS: usize = 24;

/// Zeigt eine Reasoning-Zusammenfassung des Modells an — eingeklappt oder
/// ausgeklappt.
///
/// # Beschreibung
/// Wird bei `TurnEvent::ItemAdded { item: TurnItem::Reasoning(..), .. }`
/// erzeugt. Der Text wird gedimmt-kursiv gerendert, die erste Zeile trägt die
/// gedimmte Markierung `"∴ "` (optional gefolgt von der Herkunft
/// `"[rolle] "`), Folgezeilen sind bündig darunter eingerückt.
///
/// Standardmäßig **eingeklappt**: sichtbar ist nur die erste umgebrochene
/// Zeile plus der Hinweis `"… (N Zeilen · Ctrl+O)"`, wobei `N` die Zahl der
/// Zeilen der ausgeklappten Darstellung bei der aktuellen Breite ist. Passt
/// der gesamte Text in eine Zeile, entfällt der Hinweis. Ausgeklappt wird
/// genau wie bei [`ToolCell`]: über das Feld `expanded` bzw.
/// [`ReasoningHistoryCell::set_expanded`] — geteilt über
/// [`SharedReasoningCell`], damit `app.rs` (Ctrl+O) den Zustand einer bereits
/// in den Verlauf geschobenen Zelle umschalten kann.
///
/// # Felder
/// - `summary` (`String`): Zusammengefasster Denkprozess-Text (bereits aus
///   `ReasoningItem::summary_text` zusammengefügt).
/// - `expanded` (`bool`): Nutzer-Umschalter (Ctrl+O); Standard `false`.
/// - `origin` (`Option<String>`): Rolle des Agenten, von dem das Reasoning
///   stammt (z. B. ein Kind-Agent); `None` für den Hauptagenten.
///
/// # Spec-Referenz
/// Welle 3 — Verdrahtung von `TurnEvent::ItemAdded(Reasoning)` in
/// `app.rs::run_loop`; Runde 2 / Welle 2 — einklappbares Reasoning.
#[derive(Debug, Clone, Default)]
pub(crate) struct ReasoningHistoryCell {
    /// Zusammengefasster Denkprozess-Text.
    pub summary: String,
    /// Nutzer-Umschalter (Ctrl+O): `true` zeigt den vollständigen Text.
    pub expanded: bool,
    /// Rolle des Ursprungs-Agenten (roh, unsanitisiert); `None` = Hauptagent.
    pub origin: Option<String>,
}

/// Geteilte Reasoning-Zelle (`Arc<Mutex<ReasoningHistoryCell>>`).
///
/// # Beschreibung
/// Analog zu [`SharedToolCell`]: `app.rs` schiebt `Box::new(Arc::clone(&cell))`
/// in den Verlauf (siehe `impl HistoryCell for SharedReasoningCell`) und hält
/// eine zweite Referenz, um den Ausklapp-Zustand per Ctrl+O zu schreiben.
pub(crate) type SharedReasoningCell = Arc<Mutex<ReasoningHistoryCell>>;

impl ReasoningHistoryCell {
    /// Erzeugt eine eingeklappte Reasoning-Zelle ohne Herkunftskennung.
    ///
    /// # Argumente
    /// - `summary` (`impl Into<String>`): Zusammengefasster Denkprozess-Text.
    pub(crate) fn new(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            expanded: false,
            origin: None,
        }
    }

    /// Versieht die Zelle mit der Rolle des Ursprungs-Agenten (Builder).
    ///
    /// # Argumente
    /// - `role` (`&str`): Rolle, z. B. `"explorer"`. Leer bzw. nur aus
    ///   Leerraum bestehend entfernt die Herkunft wieder.
    ///
    /// # Rückgabe
    /// Die Zelle mit gesetzter (bzw. entfernter) Herkunft; gerendert als
    /// gedimmter Präfix `"[rolle] "` hinter der Markierung `"∴ "`.
    #[must_use]
    pub(crate) fn with_origin(mut self, role: &str) -> Self {
        let trimmed = role.trim();
        self.origin = if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        };
        self
    }

    /// Setzt den Ausklapp-Zustand direkt (Ctrl+O) — dieselbe Semantik wie
    /// [`ToolCell::set_expanded`]: der Aufrufer liest den vorherigen Zustand
    /// über [`ReasoningHistoryCell::is_expanded`] und schreibt hier nur.
    pub(crate) fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
    }

    /// Liefert den aktuellen Ausklapp-Zustand.
    #[must_use]
    pub(crate) fn is_expanded(&self) -> bool {
        self.expanded
    }

    /// Erzeugt die geteilte Variante dieser Zelle (siehe [`SharedReasoningCell`]).
    #[must_use]
    pub(crate) fn into_shared(self) -> SharedReasoningCell {
        Arc::new(Mutex::new(self))
    }
}

impl HistoryCell for ReasoningHistoryCell {
    /// Rendert die Reasoning-Zusammenfassung gedimmt-kursiv mit `"∴ "`-Markierung,
    /// optionaler Herkunft `"[rolle] "` und wortweisem Wrapping; eingeklappt
    /// nur die erste Zeile plus `"… (N Zeilen · Ctrl+O)"`.
    ///
    /// # Argumente
    /// - `width` (`u16`): Gesamtbreite in Spalten (inklusive Präfix).
    ///
    /// # Rückgabe
    /// Liste der darstellbaren Zeilen beginnend mit `"∴ <summary>"`.
    fn display_lines(&self, width: u16, theme: style::Theme) -> Vec<Line<'static>> {
        let dim = style::dim_style(theme);
        let text_style = dim.add_modifier(Modifier::ITALIC);

        // Kürzung vor Bereinigung (siehe Modul-Doku, W1-08).
        let origin = self.origin.as_deref().map(|role| {
            format!(
                "[{}] ",
                sanitize_inline(&truncate_chars(role, REASONING_ORIGIN_MAX_CHARS))
            )
        });
        let lead_chars =
            REASONING_MARKER.chars().count() + origin.as_deref().map_or(0, |o| o.chars().count());
        let lead_width = u16::try_from(lead_chars).unwrap_or(u16::MAX);
        let text_width = width.saturating_sub(lead_width).max(1);
        let continuation = " ".repeat(lead_chars);

        let sanitized = sanitize_display(&self.summary);
        let body = sanitized.trim_matches('\n');
        let mut rows: Vec<String> = wrap_plain(body, text_width)
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|s| s.content.into_owned())
                    .collect()
            })
            .collect();
        if rows.is_empty() {
            rows.push(String::new());
        }
        let total = rows.len();

        let first_row = |text: String| -> Line<'static> {
            let mut spans = vec![Span::styled(REASONING_MARKER, dim)];
            if let Some(origin) = &origin {
                spans.push(Span::styled(origin.clone(), dim));
            }
            spans.push(Span::styled(text, text_style));
            Line::from(spans)
        };

        let mut rows = rows.into_iter();
        let mut out: Vec<Line<'static>> = Vec::new();
        if let Some(first) = rows.next() {
            out.push(first_row(first));
        }

        if self.expanded || total <= 1 {
            out.extend(rows.map(|text| {
                Line::from(vec![
                    Span::raw(continuation.clone()),
                    Span::styled(text, text_style),
                ])
            }));
        } else {
            let hint = format!("… ({total} Zeilen · Ctrl+O)");
            for piece in wrap_plain(&hint, text_width) {
                let raw: String = piece.spans.iter().map(|s| s.content.as_ref()).collect();
                out.push(Line::from(vec![
                    Span::raw(continuation.clone()),
                    Span::styled(raw, dim),
                ]));
            }
        }
        out
    }
}

impl HistoryCell for SharedReasoningCell {
    /// Delegiert an die geteilte [`ReasoningHistoryCell`]; ein vergifteter
    /// Lock ergibt eine sichtbare Hinweiszeile statt eines Panics.
    fn display_lines(&self, width: u16, theme: style::Theme) -> Vec<Line<'static>> {
        match self.lock() {
            Ok(guard) => guard.display_lines(width, theme),
            Err(_) => vec![Line::from(Span::styled(
                "⚠ Reasoning-Zelle nicht lesbar (Sperre vergiftet)".to_owned(),
                style::warning_style(theme),
            ))],
        }
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
            Some(q) if !q.is_empty() => {
                sanitize_inline(&truncate_chars(q, SUBAGENT_QUESTION_PREVIEW_CHARS))
            }
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
                format!(
                    "gescheitert: {} ({duration_ms}ms)",
                    sanitize_inline(outcome)
                ),
            ),
        };

        // Die Kind-Kennung gehört sichtbar in die Zeile: bei einem Fan-out
        // laufen mehrere Kinder derselben Rolle gleichzeitig, und vier Zeilen
        // „explorer: …" wären nicht auseinanderzuhalten. Gekürzt wird am
        // **Anfang** (`truncate_id_tail`), weil Kennungen sich am Ende
        // unterscheiden — eine Kürzung von hinten machte sie wieder gleich.
        let text = format!(
            "{role} [{child}]: {question_display} — {status_text} · {tool_calls} Tools · {tokens} Tok",
            role = sanitize_inline(&self.role),
            child = sanitize_inline(&truncate_id_tail(
                &self.child_id,
                SUBAGENT_CHILD_ID_PREVIEW_CHARS
            )),
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

/// Höchstzahl der in der Delta-Darstellung einzeln gezeigten geänderten
/// Knoten; darüber nennt eine Sammelzeile den Rest (R18 F4).
const PLAN_GRAPH_MAX_CHANGED: usize = 5;

/// Breite des Fortschrittsbalkens in Zeichen.
const PLAN_GRAPH_PROGRESS_BAR_CHARS: usize = 10;

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
/// Zwei Formen (R18 F4, vorher druckte jeder `plan step` alle 95 Knoten
/// neu):
/// - **Vollbild** ([`PlanGraphCell::full`], erster gezeigter Stand eines
///   Plans): Knotenliste, eingeklappt höchstens [`PLAN_GRAPH_MAX_NODES`]
///   Knoten plus Sammelzeile mit der Zahl der ausgelassenen Knoten.
/// - **Delta** ([`PlanGraphCell::delta`], jeder weitere Stand desselben
///   Plans): eingeklappt nur eine Fortschrittszeile und die gegenüber dem
///   vorigen Stand geänderten Knoten; die vollständige Liste erst
///   ausgeklappt (Ctrl+O, [`PlanGraphCell::set_expanded`]).
///
/// Ausgeklappt zeigen beide Formen alle Knoten.
///
/// # Felder
/// - `plan` (`harw_plan::Plan`): der darzustellende Plan.
/// - `changed` (`Option<Vec<TaskId>>`): `None` = Vollbild; `Some` = Delta mit
///   den geänderten bzw. neuen Knoten in Plan-Reihenfolge.
/// - `removed` (`usize`): im Delta entfernte Knoten.
/// - `expanded` (`bool`): Nutzer-Umschalter (Ctrl+O).
///
/// # Spec-Referenz
/// AP W5-01 — `harw-plan/src/types.rs::{Plan, PlanNode, PlanNodeStatus,
/// PlanNodeKind}`; R18 F4.
#[derive(Debug, Clone)]
pub(crate) struct PlanGraphCell {
    /// Der darzustellende Plan.
    pub plan: Plan,
    /// Geänderte Knoten gegenüber dem vorigen Stand; `None` = Vollbild.
    pub changed: Option<Vec<TaskId>>,
    /// Zahl der gegenüber dem vorigen Stand entfernten Knoten.
    pub removed: usize,
    /// Nutzer-Umschalter (Ctrl+O): `true` zeigt alle Knoten.
    pub expanded: bool,
}

/// Geteilte Plan-Zelle (`Arc<Mutex<PlanGraphCell>>`), damit `app.rs` sie
/// per Ctrl+O auf-/zuklappen kann (wie [`SharedReasoningCell`]).
pub(crate) type SharedPlanGraphCell = Arc<Mutex<PlanGraphCell>>;

impl PlanGraphCell {
    /// Vollbild eines erstmals gezeigten Plans.
    #[must_use]
    pub(crate) fn full(plan: Plan) -> Self {
        Self {
            plan,
            changed: None,
            removed: 0,
            expanded: false,
        }
    }

    /// Delta gegenüber `previous`.
    ///
    /// # Beschreibung
    /// Ist `previous` ein anderer Plan (andere ID), entsteht ein Vollbild.
    /// Sonst gilt ein Knoten als geändert, wenn er neu ist oder sich Status,
    /// Art, Welle, Ziel oder die Zahl der Belege unterscheiden.
    #[must_use]
    pub(crate) fn delta(previous: &Plan, plan: Plan) -> Self {
        if previous.id != plan.id {
            return Self::full(plan);
        }
        let changed: Vec<TaskId> = plan
            .nodes
            .iter()
            .filter(|node| {
                previous
                    .nodes
                    .iter()
                    .find(|old| old.id == node.id)
                    .is_none_or(|old| {
                        old.status != node.status
                            || old.kind != node.kind
                            || old.wave != node.wave
                            || old.objective != node.objective
                            || old.evidence.len() != node.evidence.len()
                    })
            })
            .map(|node| node.id.clone())
            .collect();
        let removed = previous
            .nodes
            .iter()
            .filter(|old| !plan.nodes.iter().any(|node| node.id == old.id))
            .count();
        Self {
            plan,
            changed: Some(changed),
            removed,
            expanded: false,
        }
    }

    /// Setzt den Ausklapp-Zustand (Ctrl+O).
    pub(crate) fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
    }

    /// Liefert den Ausklapp-Zustand.
    #[must_use]
    pub(crate) fn is_expanded(&self) -> bool {
        self.expanded
    }

    /// Erzeugt die geteilte Variante (siehe [`SharedPlanGraphCell`]).
    #[must_use]
    pub(crate) fn into_shared(self) -> SharedPlanGraphCell {
        Arc::new(Mutex::new(self))
    }

    /// Fortschrittszeile: `Plan <id> @<rev> · ▰▰▱▱ 3/10 erledigt (30 %)`.
    fn progress_text(&self) -> String {
        let total = self.plan.nodes.len();
        let done = self
            .plan
            .nodes
            .iter()
            .filter(|node| node.status == PlanNodeStatus::Completed)
            .count();
        let percent = (done * 100).checked_div(total).unwrap_or(0);
        let filled = (done * PLAN_GRAPH_PROGRESS_BAR_CHARS)
            .checked_div(total)
            .unwrap_or(0);
        let bar = format!(
            "{}{}",
            "▰".repeat(filled),
            "▱".repeat(PLAN_GRAPH_PROGRESS_BAR_CHARS.saturating_sub(filled))
        );
        format!(
            "Plan {} @{} · {bar} {done}/{total} erledigt ({percent} %)",
            sanitize_inline(self.plan.id.as_str()),
            self.plan.revision
        )
    }

    /// Hängt die Zeilen eines Knotens statusgefärbt und umgebrochen an.
    fn push_node(
        lines: &mut Vec<Line<'static>>,
        node: &PlanNode,
        text_width: u16,
        theme: style::Theme,
    ) {
        let node_style = match node.status {
            PlanNodeStatus::Completed => style::success_style(theme),
            PlanNodeStatus::Blocked => style::error_style(theme),
            PlanNodeStatus::Ready => style::selected_style(theme),
            PlanNodeStatus::InProgress => style::warning_style(theme),
            PlanNodeStatus::Draft | PlanNodeStatus::Superseded | PlanNodeStatus::Invalidated => {
                style::dim_style(theme)
            }
        };

        let wave_display = node
            .wave
            .map(|w| w.to_string())
            .unwrap_or_else(|| "–".to_owned());
        let objective_preview = sanitize_inline(&truncate_chars(
            &node.objective,
            PLAN_GRAPH_OBJECTIVE_PREVIEW_CHARS,
        ));
        let node_text = format!(
            "{id} · {kind:?} · {status:?} · Welle {wave_display} · {objective_preview}",
            id = sanitize_inline(node.id.as_ref()),
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
}

impl HistoryCell for PlanGraphCell {
    /// Rendert Vollbild bzw. Delta (siehe Typdokumentation).
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
        let dim = style::dim_style(theme);
        let mut lines: Vec<Line<'static>> = Vec::new();
        let total = self.plan.nodes.len();

        if let (Some(changed), false) = (&self.changed, self.expanded) {
            for piece in wrap_plain(&self.progress_text(), width.max(1)) {
                let raw: String = piece.spans.iter().map(|s| s.content.as_ref()).collect();
                lines.push(Line::from(Span::styled(raw, dim)));
            }
            let changed_nodes: Vec<&PlanNode> = self
                .plan
                .nodes
                .iter()
                .filter(|node| changed.contains(&node.id))
                .collect();
            for node in changed_nodes.iter().take(PLAN_GRAPH_MAX_CHANGED) {
                Self::push_node(&mut lines, node, text_width, theme);
            }
            let mut notes: Vec<String> = Vec::new();
            if changed_nodes.len() > PLAN_GRAPH_MAX_CHANGED {
                notes.push(format!(
                    "{} weitere geänderte Knoten",
                    changed_nodes.len() - PLAN_GRAPH_MAX_CHANGED
                ));
            }
            if changed_nodes.is_empty() {
                notes.push("keine Knotenänderung".to_owned());
            }
            if self.removed > 0 {
                notes.push(format!("{} Knoten entfernt", self.removed));
            }
            notes.push(format!("{total} Knoten · ctrl+o zum Ausklappen"));
            let note = format!("… {}", notes.join(" · "));
            for piece in wrap_plain(&note, width.max(1)) {
                let raw: String = piece.spans.iter().map(|s| s.content.as_ref()).collect();
                lines.push(Line::from(Span::styled(raw, dim)));
            }
            return lines;
        }

        if self.changed.is_some() {
            // Ausgeklapptes Delta: Fortschritt vor der vollständigen Liste.
            lines.push(Line::from(Span::styled(self.progress_text(), dim)));
        }
        let visible = if self.expanded {
            total
        } else {
            total.min(PLAN_GRAPH_MAX_NODES)
        };
        for node in self.plan.nodes.iter().take(visible) {
            Self::push_node(&mut lines, node, text_width, theme);
        }

        if total > visible {
            let elided = total - visible;
            let note = format!("… {elided} weitere Knoten ausgeblendet (insgesamt {total})");
            lines.push(Line::from(Span::styled(note, dim)));
        }

        if lines.is_empty() {
            lines.push(Line::from(Span::styled("(keine Knoten im Plan)", dim)));
        }

        lines
    }
}

impl HistoryCell for SharedPlanGraphCell {
    /// Delegiert an die geteilte [`PlanGraphCell`]; ein vergifteter Lock
    /// ergibt eine sichtbare Hinweiszeile statt eines Panics.
    fn display_lines(&self, width: u16, theme: style::Theme) -> Vec<Line<'static>> {
        match self.lock() {
            Ok(guard) => guard.display_lines(width, theme),
            Err(_) => vec![Line::from(Span::styled(
                "⚠ Plan-Zelle nicht lesbar (Sperre vergiftet)".to_owned(),
                style::warning_style(theme),
            ))],
        }
    }
}

// ─── ApprovalArgument ─────────────────────────────────────────────────────────
//
// Gemeinsame Zerlegung eines Werkzeugaufrufs in Schlüssel/Wert-Paare für die
// Freigabedarstellung. Die frühere Verlaufszelle, die diese Typen zusammen
// mit einer eigenen `display_lines`-Ansicht nutzte (`ApprovalPromptCell`/
// `ApprovalPromptView`), ist entfallen — die Freigabe läuft seit der
// Panel-Umstellung ausschließlich über [`crate::approval_dialog::ApprovalDialog`].
// `ApprovalArgument`/`ApprovalArgumentValue` bleiben bestehen, weil
// `approval_dialog.rs` genau diese Zerlegung weiterverwendet
// (`ApprovalArgument::from_call`), damit die Darstellung konsistent bleibt.

/// Höchstzahl umgebrochener Zeilen, die der **eingeklappte** Block der
/// übrigen Argumente in [`crate::approval_dialog::ApprovalDialog`] zeigt.
/// Darüber hinaus wird nie still gekürzt: eine Hinweiszeile nennt die Zahl
/// der ausgeblendeten Zeilen und die Taste `[v]`. Das Hauptargument (`path`
/// bei `fs.write`, `command` bei `shell.exec`) wird **nie** eingeklappt.
pub(crate) const APPROVAL_COLLAPSED_ARGUMENT_LINES: usize = 8;

/// Wert eines einzelnen Freigabe-Arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ApprovalArgumentValue {
    /// JSON-Zeichenkette, bereits dekodiert (Escapes aufgelöst, **roh**).
    Text(String),
    /// Jeder andere JSON-Wert in kompakter Serialisierung.
    Json(String),
}

/// Ein Argument der Freigabefrage, aus dem JSON-Objekt des Aufrufs gelöst.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ApprovalArgument {
    /// Schlüssel im Argument-Objekt (roh).
    pub key: String,
    /// Wert (roh).
    pub value: ApprovalArgumentValue,
}

impl ApprovalArgument {
    /// Zerlegt die Argumente eines Werkzeugaufrufs in Schlüssel/Wert-Paare.
    ///
    /// # Beschreibung
    /// Liest direkt aus dem vom Kern festgehaltenen `ToolCall::arguments`
    /// (`serde_json::Value`), nicht aus einer Zweit-Serialisierung. Die
    /// Reihenfolge ist die des JSON-Objekts (mit `serde_json/preserve_order`
    /// die Modellreihenfolge, sonst alphabetisch) — die Darstellung zieht das
    /// Hauptargument ohnehin nach vorn.
    ///
    /// # Argumente
    /// - `call` (`&ToolCall`): der anzuzeigende Aufruf.
    ///
    /// # Rückgabe
    /// `Some(Argumente)`, wenn die Argumente ein JSON-Objekt sind; sonst `None`
    /// (die Darstellung zeigt dann den Rohtext).
    pub(crate) fn from_call(call: &ToolCall) -> Option<Vec<Self>> {
        let object = call.arguments.as_object()?;
        Some(
            object
                .iter()
                .map(|(key, value)| Self {
                    key: key.clone(),
                    value: match value.as_str() {
                        Some(text) => ApprovalArgumentValue::Text(text.to_owned()),
                        None => ApprovalArgumentValue::Json(value.to_string()),
                    },
                })
                .collect(),
        )
    }
}

/// Setzt eine einzeilige Zeichenkette in Anführungszeichen (`"`/`\` escaped).
///
/// Genutzt von [`unknown_tool_label`] für das Klartext-Label unbekannter
/// Werkzeuge.
fn quote_approval_text(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
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

        let statement_preview = sanitize_inline(&truncate_chars(
            &self.statement,
            GOAL_STATEMENT_PREVIEW_CHARS,
        ));
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
                format!(
                    "  Invariante '{}' nicht belegt",
                    sanitize_inline(invariant_id)
                ),
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
/// Spec-Quelle: `docs/design/tui-architecture.md`.
pub(crate) fn wrap_plain(text: &str, width: u16) -> Vec<Line<'static>> {
    use unicode_width::UnicodeWidthStr;
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
            // B14: Breite in Anzeigespalten (UnicodeWidth), nicht Zeichenanzahl
            // — CJK/Emoji zählen doppelt bzw. breit. Überlange Wörter werden
            // zuerst an weichen Trennpunkten (`/`, `·`, `_`) zerlegt; nur
            // Segmente, die allein immer noch überlang sind, werden hart
            // (width-aware) geteilt. Ergebnis: kein Einzelwort-Überhang.
            let tokens: Vec<std::borrow::Cow<str>> =
                if UnicodeWidthStr::width(word) >= col_width {
                    split_at_soft_breaks(word, col_width)
                        .into_iter()
                        .map(std::borrow::Cow::Owned)
                        .collect()
                } else {
                    vec![std::borrow::Cow::Borrowed(word)]
                };

            for token in tokens {
                let token_len = UnicodeWidthStr::width(token.as_ref());

                // Token passt in eine Zeile
                if current_len == 0 {
                    // Erstes Token auf leerer Zeile
                    current_line.push_str(token.as_ref());
                    current_len = token_len;
                } else if current_len + 1 + token_len <= col_width {
                    // Token plus Leerzeichen passt noch auf die aktuelle Zeile
                    current_line.push(' ');
                    current_line.push_str(token.as_ref());
                    current_len += 1 + token_len;
                } else {
                    // Aktuelle Zeile fertig; neues Token auf nächster Zeile
                    result.push(Line::from(Span::raw(current_line.clone())));
                    current_line = token.to_string();
                    current_len = token_len;
                }
            }
        }

        if !current_line.is_empty() {
            result.push(Line::from(Span::raw(current_line)));
        }
    }

    result
}

/// Zerlegt ein überlanges Wort greedy an weichen Trennpunkten (`/`, `·`, `_`,
/// B14) in Segmente von höchstens `col_width` Anzeigespalten; Segmente, die
/// allein breiter sind, werden char-weise width-aware hart geteilt (CJK- und
/// Emoji-sicher, Vorbild `wrap_styled`). Der Trenner bleibt am Segmentende,
/// damit Pfade/URLs am Zeichen `x/y` und nicht `x/ /y` brechen.
fn split_at_soft_breaks(word: &str, col_width: usize) -> Vec<String> {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

    // Tokenisieren: jedes Token endet an einem Trennzeichen (Trenner inklusive).
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();
    for ch in word.chars() {
        current.push(ch);
        if ch == '/' || ch == '·' || ch == '_' {
            tokens.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }

    // Greifend zu Segmenten <= col_width packen.
    let mut segments: Vec<String> = Vec::new();
    let mut segment = String::new();
    let mut segment_width: usize = 0;
    for token in tokens {
        let token_width = UnicodeWidthStr::width(token.as_str());
        if token_width > col_width {
            // Hart-Split wie bisher, aber width-aware (Häppchen <= col_width).
            if !segment.is_empty() {
                segments.push(std::mem::take(&mut segment));
                segment_width = 0;
            }
            let mut chunk = String::new();
            let mut chunk_width: usize = 0;
            for ch in token.chars() {
                let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
                if chunk_width + char_width > col_width && !chunk.is_empty() {
                    segments.push(std::mem::take(&mut chunk));
                    chunk_width = 0;
                }
                chunk.push(ch);
                chunk_width += char_width;
            }
            if !chunk.is_empty() {
                segments.push(chunk);
            }
        } else if segment_width + token_width <= col_width {
            segment.push_str(&token);
            segment_width += token_width;
        } else {
            segments.push(std::mem::take(&mut segment));
            segment.push_str(&token);
            segment_width = token_width;
        }
    }
    if !segment.is_empty() {
        segments.push(segment);
    }
    segments
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

// ─── ToolCell (Plan Schritt 2 / Contract A5) ──────────────────────────────────

/// Höchstzahl der in eingeklappter Darstellung gezeigten Ergebniszeilen
/// (Zusammenfassungszeile ausgenommen) einer [`ToolCell`] — gezählt in
/// umgebrochenen Bildschirmzeilen, nicht in logischen Zeilen.
const TOOL_CELL_COLLAPSED_LINES: usize = 3;

/// Höchstzahl der in ausgeklappter Darstellung gezeigten Ergebniszeilen
/// (umgebrochene Bildschirmzeilen) einer [`ToolCell`], bevor auch dort eine Sammelzeile die Anzahl der
/// ausgelassenen Zeilen nennt — ein Terminal, das tausende Zeilen Rohausgabe
/// zeigt, ist unbrauchbar (dieselbe Erwägung wie bei [`PLAN_GRAPH_MAX_NODES`]).
const TOOL_CELL_EXPANDED_LINES: usize = 200;

/// Höchstzahl der Zeichen je Vorschauzeile einer [`ToolCell`] (siehe
/// [`ToolCell::complete`]); längere Zeilen werden über [`truncate_chars`]
/// mit `…` gekürzt.
const TOOL_CELL_PREVIEW_MAX_CHARS: usize = 500;

/// Höchstzahl der Zeilen der rohen Aufrufargumente in [`ToolVerbosity::Verbose`].
const TOOL_CELL_VERBOSE_ARGUMENT_LINES: usize = 20;

/// Einrückung der Fortsetzungszeilen des Ergebnisblocks (`"  ⎿  "` auf der
/// ersten Zeile, danach fünf Leerzeichen in derselben Breite).
const TOOL_RESULT_LEAD: &str = "  ⎿  ";
/// Fortsetzungs-Einzug für den Ergebnisblock (siehe [`TOOL_RESULT_LEAD`]).
const TOOL_RESULT_CONTINUATION: &str = "     ";

/// Laufzeitstatus eines Werkzeugaufrufs innerhalb einer [`ToolCell`].
///
/// # Beschreibung
/// `Running` gilt zwischen [`ToolCell::started`] und dem zugehörigen
/// [`ToolCell::complete`]-Aufruf. Danach entscheidet bei `shell.exec` der
/// Exit-Code, sonst `ToolCallResult::{Success, Error}`, ob `Succeeded` oder
/// `Failed` gilt.
///
/// # Spec-Referenz
/// Plan Schritt 2 — `history_cell.rs::ToolCell`, Contract-Slice A5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolState {
    /// Aufruf angefordert, Ergebnis steht noch aus.
    Running,
    /// Abgeschlossen ohne Fehler (bzw. `exit_code == 0` bei `shell.exec`).
    Succeeded,
    /// Abgeschlossen mit Fehler (bzw. `exit_code != 0` bei `shell.exec`).
    Failed,
}

/// Detailgrad der Darstellung einer [`ToolCell`]/[`ToolGroupCell`].
///
/// # Beschreibung
/// `Verbose` (Kommandozeilen-`--verbose`/Statuszeilen-Umschalter) verhält
/// sich wie ausgeklappt (`expanded == true`) und zeigt **zusätzlich** die
/// rohen Aufrufargumente als eingerücktes JSON unter dem Label — nützlich
/// zur Fehlersuche, wenn das kompakte Label ein wichtiges Argument verbirgt.
///
/// # Spec-Referenz
/// Plan Schritt 2, Contract-Slice A5 („Verbosity“).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolVerbosity {
    /// Einzeilige Aktivität: Statuspunkt + Label + Ergebnis-Zusammenfassung
    /// (B14-Verdichtung, Vorgabe) — keine Diff-/Ergebnis-Vorschauzeilen.
    Activity,
    /// Nur das an- bzw. ausgeklappte Ergebnis, keine rohen Argumente.
    Compact,
    /// Wie ausgeklappt, zusätzlich die rohen Aufrufargumente als JSON.
    Verbose,
}

/// Geteilte, aktualisierbare Zelle **eines** Werkzeugaufrufs.
///
/// # Beschreibung
/// Analog zu [`SubAgentCell`]: eine Instanz pro `call_id`, erzeugt bei
/// `TurnEvent::ToolCallRequested` über [`ToolCell::started`] und in
/// derselben Instanz bei `TurnEvent::ToolCallCompleted` über
/// [`ToolCell::complete`] fortgeschrieben — kein zweites, separates
/// Ergebnis-Item wie bei den entfallenen Alttypen `ToolCallHistoryCell`/
/// `ToolResultHistoryCell`. Der Aufrufer hält die Zelle in einer
/// `HashMap<ToolCallId, SharedToolCell>` (`call_id` als Schlüssel dieser
/// Map, siehe `app.rs::TurnEventState::pending_tool_cells`), um eingehende
/// `ToolCallCompleted`-Events der richtigen Instanz zuzuordnen — die Zelle
/// selbst trägt ihre `call_id` deshalb **nicht** zusätzlich als eigenes
/// Feld (wäre nie gelesen worden).
///
/// # Felder
/// - `tool_name` (`String`): roher Werkzeugname (z. B. `"shell.exec"`).
/// - `label` (`String`): vorab über [`tool_label`] berechnetes Klartext-Label
///   (z. B. `"Shell(git status)"`), niemals rohes JSON.
/// - `state` ([`ToolState`]): Laufzeitstatus.
/// - `duration_ms` (`Option<u64>`): Laufzeit in Millisekunden, `None` solange
///   `state == Running`.
/// - `preview` (`Vec<String>`): eingeklappt gezeigte Ergebniszeilen (siehe
///   [`ToolCell::complete`] für die Herleitung je Werkzeug).
/// - `hidden_lines` (`usize`): Zahl der bei `preview` ausgelassenen Zeilen.
/// - `summary` (`Option<String>`): kurze Zusammenfassung (`"N Zeilen"`,
///   `"N Treffer"`, `"exit 1"`, …), `None` wenn keine sinnvolle Kurzform
///   existiert.
/// - `expanded` (`bool`): Nutzer-Umschalter (Ctrl+O), zeigt bei `true`
///   [`ToolCell::full_output`] statt `preview`.
/// - `approval_note` (`Option<String>`): kompakte Freigabe-Notiz
///   (`"✓ freigegeben"` / `"✗ abgelehnt"`), gesetzt über
///   [`ToolCell::set_approval_note`].
/// - `full_output` (`Vec<String>`): vollständige Ausgabezeilen für die
///   ausgeklappte Darstellung.
/// - `placement` (`Option<ToolPlacement>`): Ausführungsort (R18 D-D), in der
///   Kopfzeile als ` · host`/` · sandbox`/` · gateway` gezeigt.
///
/// Zusätzlich hält die Zelle die rohe, kompakte JSON-Serialisierung der
/// Aufrufargumente (`arguments_json`) — **nicht** Teil der oben zitierten
/// Kernfelder der Spec, aber ohne sie ließe sich
/// [`ToolVerbosity::Verbose`] (rohe Argumente unter dem Label) nicht
/// abbilden. Das Feld ist privat und wird nur von
/// [`ToolCell::display_lines_with`] gelesen.
///
/// # Nebenläufigkeit
/// Wie [`SubAgentCell`]: intern veränderlich, aber nicht selbst
/// synchronisiert. Geteilter Zugriff läuft über [`SharedToolCell`]
/// (`Arc<Mutex<ToolCell>>`).
///
/// # Spec-Referenz
/// Plan Schritt 2 — `history_cell.rs::ToolCell`, Contract-Slice A5.
#[derive(Debug)]
pub(crate) struct ToolCell {
    /// Roher Werkzeugname (z. B. `"shell.exec"`).
    pub tool_name: String,
    /// Vorab berechnetes Klartext-Label (siehe [`tool_label`]).
    pub label: String,
    /// Laufzeitstatus.
    pub state: ToolState,
    /// Laufzeit in Millisekunden, `None` solange `state == Running`.
    pub duration_ms: Option<u64>,
    /// Eingeklappt gezeigte Ergebniszeilen.
    pub preview: Vec<String>,
    /// Zahl der bei `preview` ausgelassenen Zeilen.
    pub hidden_lines: usize,
    /// Kurze Zusammenfassung (`"N Zeilen"`, `"exit 1"`, …).
    pub summary: Option<String>,
    /// Nutzer-Umschalter (Ctrl+O): `true` zeigt `full_output` statt `preview`.
    pub expanded: bool,
    /// Kompakte Freigabe-Notiz (`"✓ freigegeben"` / `"✗ abgelehnt"`).
    pub approval_note: Option<String>,
    /// Vollständige Ausgabezeilen für die ausgeklappte Darstellung.
    pub full_output: Vec<String>,
    /// Rohe, kompakte JSON-Serialisierung der Aufrufargumente (nur für
    /// [`ToolVerbosity::Verbose`], siehe Typdokumentation oben).
    arguments_json: String,
    /// Wo der Aufruf lief (R18 D-D): aus `ToolCallCompleted::placement`,
    /// ersatzweise aus `executed_on` im Ergebnis. `None` = unbekannt; die
    /// Kopfzeile zeigt dann keinen Ort statt „host“ zu raten.
    pub placement: Option<ToolPlacement>,
}

/// Geteilte Zelle eines Werkzeugaufrufs (`Arc<Mutex<ToolCell>>`).
///
/// Analog zu den geteilten Zellen der Freigabeansicht (`Arc<Mutex<…>>` im
/// Stil von [`SubAgentCell`]); der Aufrufer hält typischerweise eine
/// `HashMap<ToolCallId, SharedToolCell>`, um `TurnEvent::ToolCallCompleted`
/// der richtigen Instanz zuzuordnen.
pub(crate) type SharedToolCell = Arc<Mutex<ToolCell>>;

/// Baut das kompakte Klartext-Label eines Werkzeugaufrufs (Claude-Code-Stil),
/// niemals rohes JSON.
///
/// # Beschreibung
/// - `shell.exec` → `Shell(<Befehl, erste Zeile, max. 120 Zeichen>)` —
///   bewusst nicht `Bash(…)`: `shell.exec` läuft unter `/bin/sh` (POSIX sh,
///   R18 F3). Beginnt in der ersten Zeile ein Heredoc (`<<'EOF'`), wird sein
///   Rumpf zu `… heredoc N Zeilen` eingeklappt (R18 F7), siehe
///   [`shell_label`].
/// - `job.start` → `Job(<Name>)`; der Befehl erscheint nur ausgeklappt im
///   Ergebnis. `job.wait` → `job.wait(<id>, ≤Ns)`, `job.status`/`job.logs`/
///   `job.stop` → `job.<aktion>(<id>)`, `job.list` → `job.list(<art>)`.
/// - `plan {action: …}` → `plan(<aktion> <knoten> → <zustand>)`, siehe
///   [`plan_tool_label`].
/// - `fs.read` → `Read(<Pfad>)`.
/// - `fs.search`/`fs.grep` → `Search("<Muster>" in <Pfad oder .>)`.
/// - `fs.list`/`fs.glob` → `List(<Pfad>)`.
/// - `fs.write` → `Write(<Pfad>)`.
/// - `fs.edit` → `Edit(<Pfad>)`.
/// - `doc.read_pdf` → `ReadPdf(<Pfad>)`, bzw. `ReadPdf(<Pfad>, Seiten
///   <Bereich>)` wenn das Argument `pages` gesetzt ist (dieselbe
///   Darstellungsform wie `fs.read`, nur mit optionalem Seitenbereich);
///   geht die Datei an einen Remote-OCR-Dienst, folgt
///   `→ remote OCR (<Host>)`.
/// - `transfer_to_<rolle>` → `Agent(<rolle>)`, ebenso
///   `agents.delegate {agent: <rolle>}` (Plan R9, Teil C).
/// - alles andere → `name(schlüssel: wert, …)` mit den ersten bis zu drei
///   **skalaren** Argumenten (Zeichenketten in Anführungszeichen, auf 40
///   Zeichen gekürzt); verschachtelte Objekte/Arrays werden übersprungen,
///   damit nie eine rohe JSON-Klammer erscheint.
///
/// Das Ergebnis ist **unsanitisiert** — die Terminal-Bereinigung erfolgt
/// erst beim Rendern ([`ToolCell::display_lines_with`]).
///
/// # Argumente
/// - `call` (`&ToolCall`): Name und Argumente, wie vom Kern festgehalten.
///   Diese Funktion nimmt bewusst `&ToolCall` statt getrennter
///   `tool_name`/`arguments`-Parameter entgegen: `harw-tui` führt
///   `serde_json` bewusst nicht als direkte Abhängigkeit (siehe
///   `approval.rs`), und `ToolCall` trägt beides bereits zusammen — genau
///   das Muster, das [`ApprovalArgument::from_call`] in dieser Datei schon
///   nutzt.
///
/// # Rückgabe
/// `String`, z. B. `"Shell(git status --short)"`, `"Read(src/app.rs)"`,
/// `"Search(\"TODO\" in src)"`, `"Agent(explorer)"`.
pub(crate) fn tool_label(call: &ToolCall) -> String {
    let tool_name = call.name.as_str();
    let object = call.arguments.as_object();
    let str_arg = |key: &str| -> Option<&str> {
        object
            .and_then(|entries| entries.get(key))
            .and_then(|v| v.as_str())
    };

    match tool_name {
        "shell.exec" => shell_label(str_arg("command").unwrap_or("")),
        "job.start" => {
            let name = str_arg("name").map(str::trim).unwrap_or("");
            if name.is_empty() {
                "Job(unbenannt)".to_owned()
            } else {
                format!("Job({})", truncate_chars(name, 60))
            }
        }
        "job.wait" => {
            let id = str_arg("job_id").unwrap_or("?");
            match object
                .and_then(|entries| entries.get("timeout_secs"))
                .and_then(|v| v.as_u64())
            {
                Some(secs) => format!("job.wait({id}, ≤{secs}s)"),
                None => format!("job.wait({id})"),
            }
        }
        "job.status" | "job.logs" => format!("{tool_name}({})", str_arg("job_id").unwrap_or("?")),
        "job.stop" => match str_arg("signal") {
            Some(signal) if !signal.is_empty() => {
                format!("job.stop({}, {signal})", str_arg("job_id").unwrap_or("?"))
            }
            _ => format!("job.stop({})", str_arg("job_id").unwrap_or("?")),
        },
        "job.list" => format!("job.list({})", str_arg("kind").unwrap_or("")),
        "plan" if str_arg("action").is_some() => plan_tool_label(call),
        "fs.read" => format!("Read({})", str_arg("path").unwrap_or("")),
        "fs.search" | "fs.grep" => format!(
            "Search(\"{}\" in {})",
            str_arg("pattern").unwrap_or(""),
            str_arg("path").unwrap_or(".")
        ),
        "fs.list" | "fs.glob" => format!("List({})", str_arg("path").unwrap_or("")),
        "fs.write" => format!("Write({})", str_arg("path").unwrap_or("")),
        "fs.edit" => format!("Edit({})", str_arg("path").unwrap_or("")),
        "doc.read_pdf" => {
            let target = harw_registry_defaults::remote_ocr_target();
            read_pdf_label(
                str_arg("path").unwrap_or(""),
                str_arg("pages"),
                harw_registry_defaults::remote_ocr_host(call, target.as_ref()),
            )
        }
        other if other.len() > "transfer_to_".len() && other.starts_with("transfer_to_") => {
            format!("Agent({})", &other["transfer_to_".len()..])
        }
        // Plan R9, Teil C: `agents.delegate` ist derselbe Handoff.
        "agents.delegate" if str_arg("agent").is_some() => {
            format!("Agent({})", str_arg("agent").unwrap_or_default())
        }
        _ => unknown_tool_label(call),
    }
}

/// Höchstlänge der ersten Befehlszeile im `Shell(…)`-Label.
const SHELL_LABEL_MAX_CHARS: usize = 120;

/// Label für `shell.exec`: `Shell(<erste Zeile>)`, bei einem Heredoc mit
/// eingeklapptem Rumpf.
///
/// # Beschreibung
/// `shell.exec` läuft unter `/bin/sh -c` (POSIX sh, nicht bash; R18 F3),
/// darum `Shell(…)`. Öffnet die erste Zeile ein Heredoc (`<<EOF`,
/// `<<'EOF'`, `<<-"EOF"`), zeigt das Label nur diese Zeile und hängt
/// `… heredoc N Zeilen` an, statt den Rumpf zu verschlucken oder über
/// mehrere Zeilen zu drucken (R18 F7, `python3 - <<'EOF'`). Andere
/// mehrzeilige Befehle zeigen wie bisher nur die erste Zeile.
///
/// # Argumente
/// - `command` (`&str`): der rohe Befehlstext.
///
/// # Rückgabe
/// Das unsanitisierte Label, z. B. `"Shell(python3 - <<'EOF' … heredoc 12 Zeilen)"`.
fn shell_label(command: &str) -> String {
    let first_line = command.lines().next().unwrap_or("");
    let head = truncate_chars(first_line, SHELL_LABEL_MAX_CHARS);
    match heredoc_body_lines(command) {
        Some(count) => format!("Shell({head} … heredoc {count} Zeilen)"),
        None => format!("Shell({head})"),
    }
}

/// Zählt die Rumpfzeilen eines in der ersten Befehlszeile geöffneten
/// Heredocs.
///
/// # Beschreibung
/// Rein zeichenbasiert, kein Shell-Parser: sucht `<<` in der ersten Zeile,
/// überspringt ein optionales `-` (`<<-`), Leerraum und ein Anführungszeichen
/// und liest das Trennwort (`[A-Za-z0-9_]`). Gezählt werden die Zeilen nach
/// der ersten bis vor die Zeile, die (ohne führende Tabs/Leerzeichen) genau
/// das Trennwort ist; fehlt diese, zählen alle übrigen Zeilen. `<<<`
/// (Here-String) und ein mit einer Ziffer beginnendes Wort (`1<<2`) sind
/// kein Heredoc.
///
/// # Rückgabe
/// `Some(N)` bei einem Heredoc (auch `N = 0`), sonst `None`.
fn heredoc_body_lines(command: &str) -> Option<usize> {
    let mut lines = command.lines();
    let first = lines.next()?;
    let start = first.find("<<")?;
    let rest = &first[start + 2..];
    if rest.starts_with('<') {
        return None;
    }
    let rest = rest.strip_prefix('-').unwrap_or(rest).trim_start();
    let rest = rest.trim_start_matches(['\'', '"', '\\']);
    let delimiter: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    // Leeres oder mit Ziffer beginnendes Wort: kein Heredoc (z. B. die
    // Arithmetik `$((1<<2))`).
    if delimiter.is_empty() || delimiter.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    let count = lines.take_while(|line| line.trim() != delimiter).count();
    Some(count)
}

/// Label für das Plan-Werkzeug (`plan {action: …}`), kompakt statt aller
/// Argumente (R18 F4).
///
/// # Beschreibung
/// - `step`/`status` → `plan(step <knoten> → <zustand>)`,
/// - Aktionen mit Knoten-ID → `plan(<aktion> <knoten>)`,
/// - sonst `plan(<aktion>)`.
///
/// Der Beleg (`evidence`) und das Ziel erscheinen nicht im Label; sie stehen
/// im (aufklappbaren) Ergebnis.
fn plan_tool_label(call: &ToolCall) -> String {
    let object = call.arguments.as_object();
    let str_arg = |key: &str| -> Option<&str> {
        object
            .and_then(|entries| entries.get(key))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|text| !text.is_empty())
    };
    let action = str_arg("action").unwrap_or("?");
    let target = str_arg("state").or_else(|| str_arg("status"));
    match (str_arg("id"), target) {
        (Some(id), Some(state)) => format!(
            "plan({action} {} → {})",
            truncate_chars(id, 40),
            truncate_chars(state, 20)
        ),
        (Some(id), None) => format!("plan({action} {})", truncate_chars(id, 40)),
        _ => format!("plan({action})"),
    }
}

/// Label für `doc.read_pdf`: `ReadPdf(<Pfad>[, Seiten <Bereich>])`, mit
/// Zusatz `→ remote OCR (<Host>)`, wenn die Datei an einen Remote-OCR-Dienst
/// geht (`remote_host` ist `Some`).
fn read_pdf_label(path: &str, pages: Option<&str>, remote_host: Option<&str>) -> String {
    let base = match pages {
        Some(pages) if !pages.is_empty() => format!("ReadPdf({path}, Seiten {pages})"),
        _ => format!("ReadPdf({path})"),
    };
    match remote_host {
        Some(host) => format!("{base} → remote OCR ({host})"),
        None => base,
    }
}

/// Fallback-Label für unbekannte Werkzeuge: `name(schlüssel: wert, …)`.
///
/// # Beschreibung
/// Siehe [`tool_label`]. Zeigt die ersten bis zu drei **skalaren**
/// Argumente (Zeichenkette, Zahl, Bool, `null`) in Objekt-Reihenfolge;
/// verschachtelte Objekte/Arrays werden übersprungen, damit nie eine rohe
/// JSON-Klammer im Label erscheint. Zeichenketten werden über
/// [`quote_approval_text`] in Anführungszeichen gesetzt und vorher auf 40
/// Zeichen gekürzt.
fn unknown_tool_label(call: &ToolCall) -> String {
    let tool_name = call.name.as_str();
    let Some(object) = call.arguments.as_object() else {
        return format!("{tool_name}()");
    };

    let mut parts: Vec<String> = Vec::new();
    for (key, value) in object.iter() {
        if parts.len() >= 3 {
            break;
        }
        let rendered = if let Some(text) = value.as_str() {
            quote_approval_text(&truncate_chars(text, 40))
        } else if value.is_number() || value.is_boolean() || value.is_null() {
            value.to_string()
        } else {
            // Objekt oder Array: übersprungen statt roher JSON-Klammern.
            continue;
        };
        parts.push(format!("{key}: {rendered}"));
    }
    format!("{tool_name}({})", parts.join(", "))
}

/// Kurzform des Ausführungsorts für die Kopfzeile einer [`ToolCell`]
/// (R18 D-D): `host`, `sandbox`, `gateway` bzw. `gateway <knoten>`,
/// `unknown` für eine unbekannte Ortsart (nie als `host` geraten).
pub(crate) fn placement_badge(placement: &ToolPlacement) -> String {
    match placement {
        ToolPlacement::Gateway { node: Some(node) } if !node.trim().is_empty() => {
            format!("gateway {}", truncate_chars(node.trim(), 32))
        }
        other => other.label().to_owned(),
    }
}

/// Ersatz-Ort aus dem Ergebnis: `executed_on` (`"host"`, `"sandbox"`,
/// `"gateway"`) auf oberster Ebene oder unter `status` (`job.wait`).
/// Andere Werte ergeben `None` — geraten wird nicht.
fn placement_from_result(result: &ToolCallResult) -> Option<ToolPlacement> {
    let ToolCallResult::Success { value } = result else {
        return None;
    };
    let executed_on = value
        .get("executed_on")
        .or_else(|| {
            value
                .get("status")
                .and_then(|status| status.get("executed_on"))
        })
        .and_then(|v| v.as_str())?;
    match executed_on {
        "host" => Some(ToolPlacement::Host),
        "sandbox" => Some(ToolPlacement::Sandbox),
        "gateway" => Some(ToolPlacement::Gateway { node: None }),
        _ => None,
    }
}

/// Extrahiert den Seitenangaben-Teil aus der Kopfzeile eines
/// `doc.read_pdf`-Ergebnisses (`"{path} — Seiten X–Y von N
/// (Mistral OCR|lokal)"`), zur Nutzung als Erfolgs-Zusammenfassung anstelle
/// von „N Zeilen" (siehe [`ToolCell::apply_success`]). Rein
/// zeichenbasiert über [`str::find`], kein Parser.
///
/// # Argumente
/// - `header` (`&str`): die erste Zeile des `doc.read_pdf`-Ergebnistexts.
///
/// # Rückgabe
/// `Some("Seiten X–Y von N (…)")` wenn die Kopfzeile das Teilwort
/// `"Seiten "` enthält, sonst `None` (dann greift die generische Vorschau).
fn parse_read_pdf_page_summary(header: &str) -> Option<String> {
    let idx = header.find("Seiten ")?;
    let tail = header[idx..].trim();
    if tail.is_empty() {
        None
    } else {
        Some(tail.to_owned())
    }
}

/// Formatiert eine kompakte JSON-Zeichenkette mit Einzügen.
///
/// # Beschreibung
/// Rein zeichenbasiert (kein zweiter JSON-Parser): verschiebt nur
/// Whitespace um `{`/`}`/`[`/`]`/`,`/`:`, ohne den Werteinhalt zu verändern.
/// Zeichen innerhalb einer Zeichenkette (erkannt an unescaped `"`) werden
/// unverändert durchgereicht, damit ein `,` oder `{` im Wert selbst keinen
/// Zeilenumbruch auslöst. Genutzt für [`ToolVerbosity::Verbose`] und die
/// ausgeklappte Darstellung der `explore.*`-Ergebnisse — `harw-tui` führt `serde_json` bewusst nicht als direkte
/// Abhängigkeit (siehe `approval.rs`), ein `serde_json::to_string_pretty`
/// steht deshalb hier nicht zur Verfügung.
///
/// # Argumente
/// - `compact` (`&str`): kompakte JSON-Zeichenkette (z. B. `Value::to_string()`).
///
/// # Rückgabe
/// `String` mit zweispaltigem Einzug je Verschachtelungsebene.
fn pretty_print_json(compact: &str) -> String {
    let mut out = String::new();
    let mut indent: usize = 0;
    let mut in_string = false;
    let mut escaped = false;

    for c in compact.chars() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '{' | '[' => {
                indent += 1;
                out.push(c);
                out.push('\n');
                out.push_str(&"  ".repeat(indent));
            }
            '}' | ']' => {
                indent = indent.saturating_sub(1);
                out.push('\n');
                out.push_str(&"  ".repeat(indent));
                out.push(c);
            }
            ',' => {
                out.push(c);
                out.push('\n');
                out.push_str(&"  ".repeat(indent));
            }
            ':' => {
                out.push(c);
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    out
}

impl ToolCell {
    /// Erzeugt eine neue Zelle für einen soeben angeforderten Werkzeugaufruf.
    ///
    /// # Beschreibung
    /// Entspricht `TurnEvent::ToolCallRequested`. `state` startet als
    /// `ToolState::Running`, `label` wird sofort über [`tool_label`]
    /// berechnet (nicht erst beim Rendern), damit ein `display_lines`-Aufruf
    /// während des Laufs bereits das fertige Label zeigt.
    ///
    /// # Argumente
    /// - `call` (`&ToolCall`): vollständiger Aufruf (Name + Argumente). Die
    ///   `call_id` reicht der Aufrufer separat als Schlüssel in seine eigene
    ///   `HashMap<ToolCallId, SharedToolCell>` ein (siehe Typdokumentation) —
    ///   diese Konstruktionsfunktion braucht sie nicht.
    ///
    /// # Rückgabe
    /// Neue [`ToolCell`] im Zustand `Running`.
    pub(crate) fn started(call: &ToolCall) -> Self {
        Self {
            tool_name: call.name.as_str().to_owned(),
            label: tool_label(call),
            state: ToolState::Running,
            duration_ms: None,
            preview: Vec::new(),
            hidden_lines: 0,
            summary: None,
            expanded: false,
            approval_note: None,
            full_output: Vec::new(),
            arguments_json: call.arguments.to_string(),
            placement: None,
        }
    }

    /// Erzeugt eine laufende Zelle aus Werkzeugname und fertigem Label, ohne
    /// die Argumente zu halten.
    ///
    /// # Beschreibung
    /// Für die Spur der Agenten-Detailansicht (`agent_monitor.rs`): sie merkt
    /// sich je Aufruf nur Name und [`tool_label`] und wertet das Ergebnis
    /// erst bei `ToolCallCompleted` über [`ToolCell::complete_at`] aus — die
    /// (möglicherweise großen) Argumente bleiben so nicht im Speicher.
    /// [`ToolVerbosity::Verbose`] zeigt für eine solche Zelle keine Argumente.
    pub(crate) fn labelled(tool_name: &str, label: String) -> Self {
        Self {
            tool_name: tool_name.to_owned(),
            label,
            state: ToolState::Running,
            duration_ms: None,
            preview: Vec::new(),
            hidden_lines: 0,
            summary: None,
            expanded: false,
            approval_note: None,
            full_output: Vec::new(),
            arguments_json: String::new(),
            placement: None,
        }
    }

    /// Einzeiliger Ausgang einer abgeschlossenen Zelle (roh, unsanitisiert):
    /// Dauer, Ort (falls bekannt) und Zusammenfassung bzw. erste
    /// Vorschauzeile, getrennt durch ` · `, z. B. `"42ms · sandbox · exit 1"`.
    ///
    /// # Rückgabe
    /// Leer, solange der Aufruf läuft und nichts bekannt ist.
    #[must_use]
    pub(crate) fn compact_outcome(&self) -> String {
        let tail = self.header_tail();
        let mut parts: Vec<String> = Vec::new();
        let head = tail.trim_start_matches(" · ");
        if !head.is_empty() {
            parts.push(head.to_owned());
        }
        let detail = self
            .summary
            .clone()
            .or_else(|| self.preview.first().cloned());
        if let Some(detail) = detail {
            let detail = detail.trim().to_owned();
            if !detail.is_empty() {
                parts.push(detail);
            }
        }
        parts.join(" · ")
    }

    /// Übernimmt die echten Aufrufdaten für eine beim Resume zunächst
    /// synthetisch angelegte Result-Zelle.
    pub(crate) fn resume_call(&mut self, call: &ToolCall) {
        self.tool_name = call.name.as_str().to_owned();
        self.label = tool_label(call);
        self.arguments_json = call.arguments.to_string();
    }

    /// Markiert einen beim Resume offenen Aufruf sichtbar als unvollständig.
    ///
    /// Nur für den echten Resume-Pfad (Wiederaufnahme eines persistierten
    /// Verlaufs); ein Turn-Ende nennt seinen echten Grund über
    /// [`Self::mark_incomplete_with`] (Runde 5, Teil M).
    pub(crate) fn mark_incomplete(&mut self) {
        self.mark_incomplete_with("unvollständig (Resume-Abbruch)");
    }

    /// Runde 5, Teil M: markiert einen offenen Aufruf als unvollständig und
    /// nennt den echten Grund (z. B. „unvollständig (abgebrochen)").
    pub(crate) fn mark_incomplete_with(&mut self, reason: &str) {
        if self.state == ToolState::Running {
            self.state = ToolState::Failed;
            self.summary = Some(reason.to_owned());
        }
    }

    /// Schreibt das Ergebnis eines abgeschlossenen Werkzeugaufrufs fort.
    ///
    /// # Beschreibung
    /// Entspricht `TurnEvent::ToolCallCompleted`. Die Herleitung von
    /// `state`/`summary`/`preview`/`full_output`/`hidden_lines` hängt vom
    /// Werkzeugnamen und dem Ergebnistyp ab:
    /// - `ToolCallResult::Error { message }`: `state = Failed`, `preview`
    ///   ist die erste Zeile von `message`, `full_output` alle Zeilen.
    /// - `ToolCallResult::Success { value }` bei `shell.exec`: liest
    ///   `exit_code`/`stdout`/`stderr`; `state = Failed` bei `exit_code != 0`
    ///   (dann `summary = Some("exit N")`), `full_output` = Zeilen von
    ///   `stdout` gefolgt von `stderr`, `preview` die ersten drei
    ///   **nicht-leeren** Zeilen, `hidden_lines` der Rest.
    /// - `fs.read`: `summary = Some("N Zeilen")` (Zeilen von `content`/`text`,
    ///   sonst der JSON-Form), keine `preview`-Zeilen.
    /// - `fs.search`/`fs.grep`: `summary = Some("N Treffer")` (Länge von
    ///   `matches`/`results`, sonst `0`).
    /// - `fs.edit`: `summary = Some("N Ersetzung(en) in <pfad>")` aus
    ///   `replacements`/`path`; `preview` die ersten drei `-`/`+`-Zeilen aus
    ///   `diff_excerpt`, `full_output` der ganze Diff-Ausschnitt.
    /// - `doc.read_pdf`: `summary` aus dem Seitenangaben-Teil der Kopfzeile
    ///   (`"Seiten X–Y von N (…)"`), wenn diese das erwartete Muster enthält
    ///   (siehe [`parse_read_pdf_page_summary`]) — dann keine `preview`-Zeilen,
    ///   analog zu `fs.read`. Sonst (Kopfzeile nicht parsebar) `summary =
    ///   None` und eine generische Vorschau wie im Fallback-Zweig unten.
    /// - `explore.projects`/`explore.find`/`explore.relations`: `summary`
    ///   `"N Projekte"`/`"N Treffer"`/`"N Beziehungen"` (bzw. ein vorhandenes
    ///   `summary`-Feld, außer bei `explore.projects`), `preview` je Eintrag
    ///   eine Zeile (`root (kind)`, `path (kind)`, `from → to (kind)`),
    ///   `full_output` das formatierte JSON ([`pretty_print_json`]).
    /// - `job.start`/`job.status`/`job.wait`: `summary` aus Ausgang
    ///   (`job.wait`), Job-ID, Zustand und Exit-Code; keine `preview`, der
    ///   Befehl (`Befehl: …`), `cwd` und die letzten Zeilen nur ausgeklappt.
    /// - alle anderen Werkzeuge: `summary = None`, `preview` die ersten drei
    ///   Zeilen einer kompakten Klartext-Darstellung (Zeichenketten
    ///   unverändert, Objekte als `schlüssel: wert`-Zeilen).
    ///
    ///
    /// Jede Vorschauzeile wird danach auf [`TOOL_CELL_PREVIEW_MAX_CHARS`]
    /// Zeichen gekappt. Die Anzeige begrenzt zusätzlich in umgebrochenen
    /// Bildschirmzeilen (siehe [`ToolCell::display_lines_with`]).
    ///
    /// # Argumente
    /// - `result` (`&ToolCallResult`): das vom Kern festgehaltene Ergebnis.
    /// - `duration_ms` (`u64`): Laufzeit des Aufrufs in Millisekunden.
    pub(crate) fn complete(&mut self, result: &ToolCallResult, duration_ms: u64) {
        self.complete_at(result, duration_ms, None);
    }

    /// Wie [`ToolCell::complete`], zusätzlich mit dem Ausführungsort aus
    /// `TurnEvent::ToolCallCompleted::placement` (R18 D-D).
    ///
    /// # Beschreibung
    /// `placement` hat Vorrang; fehlt es, gilt `executed_on` im Ergebnis
    /// (`job.*` liefert `"host"`/`"sandbox"`, bei `job.wait` unter `status`).
    /// Fehlt beides, bleibt der Ort unbekannt (`None`).
    ///
    /// # Argumente
    /// - `result`, `duration_ms`: wie bei [`ToolCell::complete`].
    /// - `placement` (`Option<&ToolPlacement>`): vom ausführenden Teil
    ///   gesetzter Ort, `None` bei älteren Sendern.
    pub(crate) fn complete_at(
        &mut self,
        result: &ToolCallResult,
        duration_ms: u64,
        placement: Option<&ToolPlacement>,
    ) {
        self.placement = placement.cloned().or_else(|| placement_from_result(result));
        self.duration_ms = Some(duration_ms);
        match result {
            ToolCallResult::Error { message } => self.apply_error(message),
            ToolCallResult::Success { .. } => self.apply_success(result),
        }
        // Jede Vorschauzeile zusätzlich hart kappen: auch umgebrochen und
        // zeilenbegrenzt soll eine 64-KiB-Zeile nicht bei jedem Rendern
        // vollständig umgebrochen werden.
        for line in &mut self.preview {
            if line.chars().count() > TOOL_CELL_PREVIEW_MAX_CHARS {
                *line = truncate_chars(line, TOOL_CELL_PREVIEW_MAX_CHARS);
            }
        }
    }

    /// Teil von [`ToolCell::complete`]: Fehlerzweig.
    fn apply_error(&mut self, message: &str) {
        self.state = ToolState::Failed;
        // Runde 5, Teil M: der Endbericht eines Kind-Agenten nennt den
        // echten Grund („abgebrochen: Zeitbudget 15 min erreicht · Übergabe
        // verfügbar") statt eines generischen Fehlers.
        self.summary =
            harw_core::parse_child_end(message).map(|header| harw_core::child_end_label(&header));
        let first_line = message.lines().next().unwrap_or(message).to_owned();
        self.full_output = message.lines().map(str::to_owned).collect();
        self.preview = vec![first_line];
        self.hidden_lines = self.full_output.len().saturating_sub(1);
    }

    /// Teil von [`ToolCell::complete`]: Erfolgszweig, verzweigt nach
    /// Werkzeugname. Nimmt bewusst `&ToolCallResult` (statt `&Value`)
    /// entgegen und entpackt `value` erst intern — derselbe Grund wie bei
    /// [`tool_label`]: `harw-tui` kann `serde_json::Value` nicht als
    /// Parametertyp benennen, ohne die Crate als direkte Abhängigkeit zu
    /// führen.
    fn apply_success(&mut self, result: &ToolCallResult) {
        let ToolCallResult::Success { value } = result else {
            return;
        };

        match self.tool_name.as_str() {
            "shell.exec" => {
                let exit_code = value.get("exit_code").and_then(|v| v.as_i64()).unwrap_or(0);
                let stdout = value.get("stdout").and_then(|v| v.as_str()).unwrap_or("");
                let stderr = value.get("stderr").and_then(|v| v.as_str()).unwrap_or("");

                let all_lines: Vec<String> = stdout
                    .lines()
                    .chain(stderr.lines())
                    .map(str::to_owned)
                    .collect();
                let non_empty: Vec<String> = all_lines
                    .iter()
                    .filter(|line| !line.trim().is_empty())
                    .cloned()
                    .collect();

                self.state = if exit_code == 0 {
                    ToolState::Succeeded
                } else {
                    ToolState::Failed
                };
                self.summary = if exit_code == 0 {
                    None
                } else {
                    Some(format!("exit {exit_code}"))
                };
                self.preview = non_empty
                    .iter()
                    .take(TOOL_CELL_COLLAPSED_LINES)
                    .cloned()
                    .collect();
                self.hidden_lines = non_empty.len().saturating_sub(self.preview.len());
                self.full_output = all_lines;
            }
            "fs.read" => {
                let text = value
                    .get("content")
                    .and_then(|v| v.as_str())
                    .or_else(|| value.get("text").and_then(|v| v.as_str()));
                let (line_count, lines) = match text {
                    Some(t) => (
                        t.lines().count(),
                        t.lines().map(str::to_owned).collect::<Vec<_>>(),
                    ),
                    None => {
                        let rendered = value.to_string();
                        let lines: Vec<String> = rendered.lines().map(str::to_owned).collect();
                        (lines.len(), lines)
                    }
                };
                self.state = ToolState::Succeeded;
                self.summary = Some(format!("{line_count} Zeilen"));
                self.preview = Vec::new();
                self.hidden_lines = 0;
                self.full_output = lines;
            }
            "doc.read_pdf" => {
                let text = value
                    .get("content")
                    .and_then(|v| v.as_str())
                    .or_else(|| value.get("text").and_then(|v| v.as_str()));
                let lines: Vec<String> = match text {
                    Some(t) => t.lines().map(str::to_owned).collect(),
                    None => value.to_string().lines().map(str::to_owned).collect(),
                };
                self.state = ToolState::Succeeded;
                self.summary = lines
                    .first()
                    .and_then(|header| parse_read_pdf_page_summary(header));
                if self.summary.is_some() {
                    self.preview = Vec::new();
                    self.hidden_lines = 0;
                } else {
                    self.preview = lines
                        .iter()
                        .take(TOOL_CELL_COLLAPSED_LINES)
                        .cloned()
                        .collect();
                    self.hidden_lines = lines.len().saturating_sub(self.preview.len());
                }
                self.full_output = lines;
            }
            "explore.projects" | "explore.find" | "explore.relations" => {
                // Eigene Darstellung statt des generischen `schlüssel: wert`-
                // Fallbacks, der das ganze Array zu einer Riesenzeile machte.
                let (array_key, noun) = match self.tool_name.as_str() {
                    "explore.projects" => ("projects", "Projekte"),
                    "explore.find" => ("matches", "Treffer"),
                    _ => ("relations", "Beziehungen"),
                };
                let is_relations = self.tool_name == "explore.relations";
                let entries = value.get(array_key).and_then(|v| v.as_array());
                self.state = ToolState::Succeeded;
                self.preview = entries
                    .map(|entries| {
                        entries
                            .iter()
                            .map(|entry| {
                                let field = |key: &str| {
                                    entry.get(key).and_then(|v| v.as_str()).unwrap_or("?")
                                };
                                if is_relations {
                                    format!(
                                        "{} → {} ({})",
                                        field("from"),
                                        field("to"),
                                        field("kind")
                                    )
                                } else if array_key == "projects" {
                                    format!("{} ({})", field("root"), field("kind"))
                                } else {
                                    format!("{} ({})", field("path"), field("kind"))
                                }
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let total = value
                    .get("total")
                    .or_else(|| value.get("count"))
                    .and_then(|v| v.as_u64())
                    .map_or(self.preview.len(), |n| {
                        usize::try_from(n).unwrap_or(usize::MAX)
                    });
                let truncated = value
                    .get("truncated")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let mut summary = match value.get("summary").and_then(|v| v.as_str()) {
                    Some(text) if array_key != "projects" => text.to_owned(),
                    _ => format!("{total} {noun}"),
                };
                if truncated {
                    summary.push_str(" (gekürzt)");
                }
                self.summary = Some(summary);
                self.hidden_lines = 0;
                self.full_output = pretty_print_json(&value.to_string())
                    .lines()
                    .map(str::to_owned)
                    .collect();
            }
            // Runde 4, Teil E: fehlt LaTeX, muss die Nutzerin das sehen —
            // Zusammenfassung mit den fehlenden Programmen, `user_message`
            // (je Hinweis eine Zeile) aufgeklappt statt verschluckt. Übrige
            // `latex.build`-Ergebnisse laufen über den generischen Arm.
            "latex.build"
                if value.get("status").and_then(|v| v.as_str()) == Some("not_installed") =>
            {
                let missing: Vec<&str> = value
                    .get("missing")
                    .and_then(|v| v.as_array())
                    .map(|items| items.iter().filter_map(|item| item.as_str()).collect())
                    .unwrap_or_default();
                let message = value
                    .get("user_message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("LaTeX ist nicht installiert.");
                let lines: Vec<String> = message
                    .split(" · ")
                    .map(|part| part.trim().to_owned())
                    .filter(|part| !part.is_empty())
                    .collect();
                self.state = ToolState::Failed;
                self.summary = Some(format!("LaTeX nicht installiert: {}", missing.join(", ")));
                self.preview = lines.clone();
                self.hidden_lines = 0;
                self.full_output = lines;
                self.expanded = true;
            }
            // Runde 5, Teil D: `fs.edit` liefert `{path, replacements,
            // diff_excerpt}`; die Zelle zeigt die Zahl der Ersetzungen und
            // den Diff-Ausschnitt als Vorschau.
            "fs.edit" => {
                let replacements = value
                    .get("replacements")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let path = value.get("path").and_then(|v| v.as_str()).unwrap_or("?");
                let noun = if replacements == 1 {
                    "Ersetzung"
                } else {
                    "Ersetzungen"
                };
                let diff: Vec<String> = value
                    .get("diff_excerpt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .lines()
                    .map(str::to_owned)
                    .collect();
                let changed: Vec<String> = diff
                    .iter()
                    .filter(|line| line.starts_with('-') || line.starts_with('+'))
                    .cloned()
                    .collect();
                self.state = ToolState::Succeeded;
                self.summary = Some(format!("{replacements} {noun} in {path}"));
                self.preview = changed
                    .iter()
                    .take(TOOL_CELL_COLLAPSED_LINES)
                    .cloned()
                    .collect();
                self.hidden_lines = changed.len().saturating_sub(self.preview.len());
                self.full_output = diff;
            }
            // R18 D-D: `job.*` zeigt Job, Zustand und Ausgang; der Befehl
            // steht nur in der ausgeklappten Darstellung, nie in der
            // eingeklappten Zelle.
            "job.start" | "job.status" | "job.wait" if value.is_object() => {
                let status = value
                    .get("status")
                    .filter(|status| status.is_object())
                    .unwrap_or(value);
                let field = |key: &str| status.get(key).and_then(|v| v.as_str());
                let mut parts: Vec<String> = Vec::new();
                if let Some(outcome) = value.get("outcome").and_then(|v| v.as_str()) {
                    parts.push(outcome.to_owned());
                }
                parts.extend(field("job_id").map(str::to_owned));
                parts.extend(field("state").map(str::to_owned));
                if let Some(code) = status.get("exit_code").and_then(|v| v.as_i64()) {
                    parts.push(format!("exit {code}"));
                }
                let mut lines: Vec<String> = Vec::new();
                if let Some(command) = field("command") {
                    lines.push(format!("Befehl: {command}"));
                }
                if let Some(cwd) = field("cwd") {
                    lines.push(format!("cwd: {cwd}"));
                }
                if let Some(last) = status.get("last_lines").and_then(|v| v.as_array()) {
                    lines.extend(
                        last.iter()
                            .filter_map(|line| line.as_str())
                            .map(str::to_owned),
                    );
                }
                self.state = ToolState::Succeeded;
                self.summary = if parts.is_empty() {
                    None
                } else {
                    Some(parts.join(" · "))
                };
                self.preview = Vec::new();
                self.hidden_lines = lines.len();
                self.full_output = lines;
            }
            "fs.search" | "fs.grep" => {
                let count = value
                    .get("matches")
                    .or_else(|| value.get("results"))
                    .and_then(|v| v.as_array())
                    .map(|matches| matches.len())
                    .unwrap_or(0);
                self.state = ToolState::Succeeded;
                self.summary = Some(format!("{count} Treffer"));
                self.preview = Vec::new();
                self.hidden_lines = 0;
                self.full_output = Vec::new();
            }
            _ => {
                self.state = ToolState::Succeeded;
                self.summary = None;
                let mut lines: Vec<String> = Vec::new();
                if let Some(text) = value.as_str() {
                    lines.extend(text.lines().map(str::to_owned));
                } else if let Some(object) = value.as_object() {
                    for (key, entry) in object.iter() {
                        let rendered = match entry.as_str() {
                            Some(text) => text.to_owned(),
                            None => entry.to_string(),
                        };
                        lines.push(format!("{key}: {rendered}"));
                    }
                } else {
                    lines.push(value.to_string());
                }
                self.preview = lines
                    .iter()
                    .take(TOOL_CELL_COLLAPSED_LINES)
                    .cloned()
                    .collect();
                self.hidden_lines = lines.len().saturating_sub(self.preview.len());
                self.full_output = lines;
            }
        }
    }

    /// Setzt eine kompakte Freigabe-Notiz nach einer Freigabe-Entscheidung.
    ///
    /// # Argumente
    /// - `note` (`&str`): z. B. `"✓ freigegeben"` oder `"✗ abgelehnt"`.
    pub(crate) fn set_approval_note(&mut self, note: &str) {
        self.approval_note = Some(note.to_owned());
    }

    /// Setzt den Ausklapp-Zustand direkt (Ctrl+O, sowohl „letzte" als auch
    /// „alle" — siehe `app.rs::ToolCellHandle::{toggle_expanded, set_expanded}`,
    /// die den vorherigen Zustand selbst über `is_expanded`/`expanded` liest
    /// und hier nur noch schreibt; ein eigenes `toggle_expanded` auf dieser
    /// Zelle wäre daher nie aufgerufen worden).
    pub(crate) fn set_expanded(&mut self, expanded: bool) {
        self.expanded = expanded;
    }

    /// Baut den Dauer-/Ort-/Freigabe-Anhang der Kopfzeile (roh,
    /// unsanitisiert): ` · {duration}ms`/`s` wenn abgeschlossen, danach
    /// ` · {ort}` (R18 D-D, nur wenn bekannt) und ` · {approval_note}`.
    /// Leer, solange der Aufruf noch läuft und keine Notiz gesetzt ist.
    fn header_tail(&self) -> String {
        let mut text = String::new();
        if let Some(duration_ms) = self.duration_ms {
            if duration_ms < 1000 {
                text.push_str(&format!(" · {duration_ms}ms"));
            } else {
                text.push_str(&format!(" · {:.1}s", duration_ms as f64 / 1000.0));
            }
        }
        if let Some(placement) = &self.placement {
            text.push_str(" · ");
            text.push_str(&placement_badge(placement));
        }
        if let Some(note) = &self.approval_note {
            text.push_str(&format!(" · {note}"));
        }
        text
    }

    /// Rendert die Zelle mit explizitem [`ToolVerbosity`], unabhängig vom
    /// Ctrl+O-Zustand (`Verbose` verhält sich zusätzlich wie ausgeklappt).
    ///
    /// # Argumente
    /// - `width` (`u16`): Gesamtbreite in Spalten.
    /// - `theme` ([`style::Theme`]): aktives Farbschema.
    /// - `verbosity` ([`ToolVerbosity`]): `Compact` folgt `self.expanded`,
    ///   `Verbose` erzwingt Ausklappen und zeigt zusätzlich die rohen
    ///   Argumente.
    ///
    /// # Rückgabe
    /// Vollständig gerenderte Zeilen dieser Zelle.
    pub(crate) fn display_lines_with(
        &self,
        width: u16,
        theme: style::Theme,
        verbosity: ToolVerbosity,
    ) -> Vec<Line<'static>> {
        let verbose = matches!(verbosity, ToolVerbosity::Verbose);
        let expanded = self.expanded || verbose;

        let (glyph_style, dot_glyph) = match self.state {
            ToolState::Running => (style::dim_style(theme), "● "),
            ToolState::Succeeded => (style::success_style(theme), "● "),
            ToolState::Failed => (style::error_style(theme), "● "),
        };
        let label_style = Style::default().add_modifier(Modifier::BOLD);
        let dim = style::dim_style(theme);

        // Aktivitätsmodus (B11a): genau eine Zeile — Statuspunkt, Label,
        // Ergebnis-Zusammenfassung und Dauer; keine rohen Argumente, kein
        // Ergebnis-Rendering. Ausgeklappt bleibt opt-in über `self.expanded`.
        if matches!(verbosity, ToolVerbosity::Activity) && !self.expanded {
            let tail = match &self.summary {
                Some(summary) => format!("{summary}{}", self.header_tail()),
                None => self.header_tail(),
            };
            let mut lines: Vec<Line<'static>> = Vec::new();
            push_header_line(
                &mut lines,
                HeaderPart {
                    text: dot_glyph,
                    style: glyph_style,
                },
                HeaderPart {
                    text: &self.label,
                    style: label_style,
                },
                HeaderPart {
                    text: &tail,
                    style: dim,
                },
                width,
            );
            return lines;
        }

        let mut lines: Vec<Line<'static>> = Vec::new();

        // Kopfzeile: Statuspunkt + fettes Label + gedimmte Dauer/Notiz,
        // wortweise umgebrochen wie die übrigen Zellen dieser Datei.
        push_header_line(
            &mut lines,
            HeaderPart {
                text: dot_glyph,
                style: glyph_style,
            },
            HeaderPart {
                text: &self.label,
                style: label_style,
            },
            HeaderPart {
                text: &self.header_tail(),
                style: dim,
            },
            width,
        );

        if verbose {
            let pretty = pretty_print_json(&self.arguments_json);
            let arg_lines: Vec<&str> = pretty.lines().collect();
            for raw in arg_lines.iter().take(TOOL_CELL_VERBOSE_ARGUMENT_LINES) {
                push_indented_wrapped(&mut lines, raw, width, TOOL_RESULT_CONTINUATION, dim);
            }
            if arg_lines.len() > TOOL_CELL_VERBOSE_ARGUMENT_LINES {
                let elided = arg_lines.len() - TOOL_CELL_VERBOSE_ARGUMENT_LINES;
                let note = format!("… +{elided} Zeilen Argumente");
                push_indented_wrapped(&mut lines, &note, width, TOOL_RESULT_CONTINUATION, dim);
            }
        }

        let mut body_first = true;
        let push_body = |content: &str, lines: &mut Vec<Line<'static>>, body_first: &mut bool| {
            let lead = if *body_first {
                *body_first = false;
                TOOL_RESULT_LEAD
            } else {
                TOOL_RESULT_CONTINUATION
            };
            push_indented_wrapped(lines, content, width, lead, dim);
        };

        if let Some(summary) = &self.summary {
            push_body(summary, &mut lines, &mut body_first);
        }

        // Beide Zweige rechnen in umgebrochenen Bildschirmzeilen (Vorbild
        // `ReasoningHistoryCell`): eine einzelne 64-KiB-Zeile darf weder
        // eingeklappt noch ausgeklappt den Bildschirm füllen.
        if expanded {
            let hidden = self.push_body_rows(
                &mut lines,
                &mut body_first,
                self.full_output.iter(),
                TOOL_CELL_EXPANDED_LINES,
                width,
                dim,
            );
            if hidden > 0 {
                push_body(&format!("… +{hidden} Zeilen"), &mut lines, &mut body_first);
            }
        } else {
            let mut hidden = self.push_body_rows(
                &mut lines,
                &mut body_first,
                self.preview.iter(),
                TOOL_CELL_COLLAPSED_LINES,
                width,
                dim,
            );
            hidden += self
                .hidden_tail()
                .iter()
                .map(|row| wrapped_row_count(row, width, TOOL_RESULT_CONTINUATION))
                .sum::<usize>();
            if hidden > 0 {
                let note = format!("… +{hidden} Zeilen (ctrl+o zum Ausklappen)");
                push_body(&note, &mut lines, &mut body_first);
            }
        }

        lines
    }

    /// Rendert `rows` als Ergebniszeilen mit einem Budget von höchstens
    /// `max_rows` **umgebrochenen Bildschirmzeilen** (siehe
    /// [`push_indented_wrapped_capped`]).
    ///
    /// # Argumente
    /// - `lines`: Ziel-Vektor.
    /// - `body_first`: `true`, solange noch keine Ergebniszeile gerendert
    ///   wurde (dann erhält die nächste Zeile [`TOOL_RESULT_LEAD`]).
    /// - `rows`: die logischen Zeilen.
    /// - `max_rows`: Budget in Bildschirmzeilen.
    /// - `width`: Gesamtbreite in Spalten.
    /// - `style`: Stil der Zeilen.
    ///
    /// # Rückgabe
    /// Zahl der wegen des Budgets nicht gezeigten Bildschirmzeilen (Rest der
    /// zuletzt gekürzten Zeile plus alle folgenden Zeilen, umgebrochen).
    fn push_body_rows<'a>(
        &self,
        lines: &mut Vec<Line<'static>>,
        body_first: &mut bool,
        rows: impl Iterator<Item = &'a String>,
        max_rows: usize,
        width: u16,
        style: Style,
    ) -> usize {
        let mut remaining = max_rows;
        let mut hidden = 0usize;
        for row in rows {
            if remaining == 0 {
                hidden += wrapped_row_count(row, width, TOOL_RESULT_CONTINUATION);
                continue;
            }
            let lead = if *body_first {
                TOOL_RESULT_LEAD
            } else {
                TOOL_RESULT_CONTINUATION
            };
            let before = lines.len();
            hidden += push_indented_wrapped_capped(lines, row, width, lead, style, remaining);
            *body_first = false;
            remaining = remaining.saturating_sub(lines.len() - before);
        }
        hidden
    }

    /// Die bei eingeklappter Darstellung zusätzlich zur Vorschau
    /// verborgenen logischen Zeilen: die letzten `hidden_lines` Einträge
    /// von `full_output`.
    ///
    /// # Beschreibung
    /// In allen Zweigen von [`ToolCell::apply_success`]/`apply_error`, die
    /// `hidden_lines > 0` setzen, folgen die verborgenen Zeilen in
    /// `full_output` auf die Vorschau. Bei `shell.exec` (Vorschau aus
    /// nicht-leeren Zeilen) ist das eine Näherung: gezählt werden die
    /// letzten `hidden_lines` Zeilen, leere eingeschlossen — sie überlappen
    /// die Vorschau nie.
    fn hidden_tail(&self) -> &[String] {
        let start = self.full_output.len().saturating_sub(self.hidden_lines);
        &self.full_output[start..]
    }
}

impl HistoryCell for ToolCell {
    /// Rendert mit `verbosity = Compact`, d. h. gesteuert allein über
    /// `self.expanded`. Siehe [`ToolCell::display_lines_with`].
    fn display_lines(&self, width: u16, theme: style::Theme) -> Vec<Line<'static>> {
        self.display_lines_with(width, theme, ToolVerbosity::Compact)
    }
}

/// Teilt `text` an der `char_index`-ten Zeichengrenze (nicht Byte-Index) in
/// zwei Teile. Hilfsfunktion für [`push_header_line`], die Label (fett) und
/// Dauer/Notiz-Anhang (gedimmt) getrennt stylen muss, auch wenn beide in
/// derselben umgebrochenen Zeile landen.
fn split_at_char(text: &str, char_index: usize) -> (&str, &str) {
    match text.char_indices().nth(char_index) {
        Some((byte_index, _)) => (&text[..byte_index], &text[byte_index..]),
        None => (text, ""),
    }
}

/// Text und Stil eines Kopfzeilen-Teils für [`push_header_line`].
///
/// Bündelt die drei zusammengehörigen Text/Stil-Paare (Statuspunkt, Label,
/// Anhang) zu je einem Argument, damit `push_header_line` nicht mehr als
/// die von Clippys `too_many_arguments` erlaubte Parameterzahl braucht.
struct HeaderPart<'a> {
    /// Anzuzeigender Text (unsanitisiert bei `label`/`tail`, siehe Aufrufer).
    text: &'a str,
    /// Stil, mit dem `text` gerendert wird.
    style: Style,
}

/// Rendert die Kopfzeile einer [`ToolCell`]: Statuspunkt (`glyph`) auf der
/// ersten Zeile, gefolgt vom fett gestylten `label` und dem gedimmt
/// gestylten `tail` (Dauer/Freigabe-Notiz) — wortweise auf `width`
/// umgebrochen wie jede andere Zelle dieser Datei. Anders als
/// [`push_indented_wrapped`] trägt diese Funktion **zwei** Stile in
/// derselben logischen Zeile; bricht der Umbruch mitten im Übergang von
/// Label zu Anhang, wird genau diese eine Teilzeile an der Zeichengrenze in
/// zwei Spans aufgeteilt (siehe [`split_at_char`]).
///
/// # Argumente
/// - `lines`: Ziel-Vektor, an das die Kopfzeile(n) angehängt werden.
/// - `glyph`: Präfix der ersten Zeile (Statuspunkt) samt Stil.
/// - `label`: fett gestylter Werkzeugname/Label samt Stil.
/// - `tail`: gedimmt gestylter Anhang (Dauer, Freigabe-Notiz) samt Stil.
/// - `width`: Gesamtbreite in Spalten (inklusive Präfix).
fn push_header_line(
    lines: &mut Vec<Line<'static>>,
    glyph: HeaderPart<'_>,
    label: HeaderPart<'_>,
    tail: HeaderPart<'_>,
    width: u16,
) {
    let prefix_len = 2_u16;
    let text_width = width.saturating_sub(prefix_len).max(1);
    let sanitized_label = sanitize_inline(label.text);
    let sanitized_tail = sanitize_inline(tail.text);
    let label_chars = sanitized_label.chars().count();
    let combined = format!("{sanitized_label}{sanitized_tail}");
    let wrapped = wrap_plain(&combined, text_width);

    if wrapped.is_empty() {
        lines.push(Line::from(Span::styled(glyph.text.to_owned(), glyph.style)));
        return;
    }

    let mut consumed = 0usize;
    for (i, piece) in wrapped.into_iter().enumerate() {
        let raw: String = piece.spans.iter().map(|s| s.content.as_ref()).collect();
        let line_chars = raw.chars().count();
        let prefix_span = if i == 0 {
            Span::styled(glyph.text.to_owned(), glyph.style)
        } else {
            Span::raw("  ")
        };
        let mut spans = vec![prefix_span];
        if consumed >= label_chars {
            spans.push(Span::styled(raw, tail.style));
        } else if consumed + line_chars <= label_chars {
            spans.push(Span::styled(raw, label.style));
        } else {
            let split_at = label_chars - consumed;
            let (head, tail_piece) = split_at_char(&raw, split_at);
            spans.push(Span::styled(head.to_owned(), label.style));
            spans.push(Span::styled(tail_piece.to_owned(), tail.style));
        }
        lines.push(Line::from(spans));
        consumed += line_chars;
    }
}

/// Bricht `content` auf `width` um und hängt jede Teilzeile mit `lead` (erste
/// Teilzeile) bzw. gleich breiten Leerzeichen (Folgezeilen) versehen an
/// `lines` an — Fortsetzungszeilen bleiben so bündig unter dem Text der
/// ersten Zeile. Jede Zeile wird über [`sanitize_inline`] terminal-sicher
/// gemacht, bevor sie den `ratatui`-Buffer erreicht (W1-08).
fn push_indented_wrapped(
    lines: &mut Vec<Line<'static>>,
    content: &str,
    width: u16,
    lead: &str,
    style: Style,
) {
    let lead_width = u16::try_from(lead.chars().count()).unwrap_or(u16::MAX);
    let inner_width = width.saturating_sub(lead_width).max(1);
    let sanitized = sanitize_inline(content);
    let wrapped = wrap_plain(&sanitized, inner_width);
    let continuation = " ".repeat(lead.chars().count());
    if wrapped.is_empty() {
        lines.push(Line::from(Span::styled(lead.to_owned(), style)));
        return;
    }
    for (i, piece) in wrapped.into_iter().enumerate() {
        let raw: String = piece.spans.iter().map(|s| s.content.as_ref()).collect();
        let prefix = if i == 0 { lead } else { continuation.as_str() };
        lines.push(Line::from(Span::styled(format!("{prefix}{raw}"), style)));
    }
}

/// Zahl der Bildschirmzeilen, die [`push_indented_wrapped`] für `content`
/// bei Breite `width` und Präfix `lead` erzeugen würde (mindestens 1).
fn wrapped_row_count(content: &str, width: u16, lead: &str) -> usize {
    let lead_width = u16::try_from(lead.chars().count()).unwrap_or(u16::MAX);
    let inner_width = width.saturating_sub(lead_width).max(1);
    wrap_plain(&sanitize_inline(content), inner_width)
        .len()
        .max(1)
}

/// Wie [`push_indented_wrapped`], aber mit höchstens `max_rows`
/// Bildschirmzeilen.
///
/// # Beschreibung
/// Passt `content` umgebrochen nicht in `max_rows` Zeilen, endet die letzte
/// gezeigte Zeile mit `…` (bei voller Breite ersetzt `…` das letzte
/// Zeichen). Bei `max_rows == 0` wird nichts angehängt.
///
/// # Argumente
/// - `lines`: Ziel-Vektor.
/// - `content`: die (unsanitisierte) logische Zeile.
/// - `width`: Gesamtbreite in Spalten (inklusive Präfix).
/// - `lead`: Präfix der ersten Teilzeile (Folgezeilen gleich breit eingerückt).
/// - `style`: Stil aller Teilzeilen.
/// - `max_rows`: Höchstzahl der angehängten Bildschirmzeilen.
///
/// # Rückgabe
/// Zahl der nicht gezeigten Bildschirmzeilen dieser logischen Zeile.
fn push_indented_wrapped_capped(
    lines: &mut Vec<Line<'static>>,
    content: &str,
    width: u16,
    lead: &str,
    style: Style,
    max_rows: usize,
) -> usize {
    let lead_chars = lead.chars().count();
    let lead_width = u16::try_from(lead_chars).unwrap_or(u16::MAX);
    let inner_width = width.saturating_sub(lead_width).max(1);
    let sanitized = sanitize_inline(content);
    let wrapped: Vec<String> = wrap_plain(&sanitized, inner_width)
        .into_iter()
        .map(|piece| piece.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect();
    let total = wrapped.len().max(1);
    if max_rows == 0 {
        return total;
    }
    if wrapped.is_empty() {
        lines.push(Line::from(Span::styled(lead.to_owned(), style)));
        return 0;
    }
    let shown = wrapped.len().min(max_rows);
    let hidden = wrapped.len() - shown;
    let continuation = " ".repeat(lead_chars);
    for (i, raw) in wrapped.into_iter().take(shown).enumerate() {
        let prefix = if i == 0 { lead } else { continuation.as_str() };
        let text = if hidden > 0 && i + 1 == shown {
            let keep = usize::from(inner_width).saturating_sub(1);
            let mut cut: String = raw.chars().take(keep).collect();
            cut.push('…');
            cut
        } else {
            raw
        };
        lines.push(Line::from(Span::styled(format!("{prefix}{text}"), style)));
    }
    hidden
}

/// Sammelzelle für aufeinanderfolgende, lesende `fs.*`-Aufrufe desselben
/// Turns (`fs.read`, `fs.search`, `fs.grep`, `fs.list`, `fs.glob`).
///
/// # Beschreibung
/// Eingeklappt zeigt sie eine Sammelzeile mit Zählern je Kategorie
/// (`"N Dateien gelesen, N Muster gesucht, N Verzeichnisse gelistet"`,
/// nur nicht-leere Kategorien); ausgeklappt rendert sie jede enthaltene
/// [`ToolCell`] einzeln über deren eigene [`HistoryCell::display_lines`].
/// Der Aufrufer entscheidet über [`ToolGroupCell::accepts`], ob ein neu
/// eingetroffener Aufruf noch in die laufende Gruppe passt, oder ob eine neue
/// Gruppe (bzw. eine einzelne [`ToolCell`]) beginnt.
///
/// # Felder
/// - `cells` (`Vec<SharedToolCell>`): die gruppierten Zellen in
///   Ankunftsreihenfolge.
/// - `expanded` (`bool`): Nutzer-Umschalter (Ctrl+O).
///
/// # Spec-Referenz
/// Plan Schritt 2 („Gruppierung“), Contract-Slice A5.
///
/// # Semantische Grenze und geplanter Ausbau
/// Diese Struktur ist der **heutige TUI-lokale** Aggregator für genau eine
/// bekannte Darstellung: aufeinanderfolgende, lesende Werkzeugaufrufe. Sie
/// ist damit zugleich der konkrete Migrationsanker für
/// `docs/planning/71-semantic-activity-patterns/`.
///
/// Wichtig für spätere Umbauten:
///
/// - die enthaltenen `ToolCell`s und ihre `ToolCallId`s bleiben die
///   konkreten Laufzeitbeobachtungen;
/// - eine kompakte Sammelzeile ist nur eine abgeleitete Projektion und darf
///   die zugrunde liegenden Aufrufe nicht aus Verlauf/Audit entfernen;
/// - künftige ressourcenbezogene Muster (zum Beispiel
///   `fs.edit(path=X) -> fs.read(path=X) *`) sollen nicht als weitere
///   Namens-Sonderfälle direkt in diesen Renderer wachsen, sondern über eine
///   gemeinsame Pattern-/Reducer-Schicht eingespeist werden;
/// - `ToolGroupCell` darf während dieser Migration als kompatible
///   Darstellungsoberfläche bestehen bleiben.
///
/// Die geplante Pattern-Schicht ist ausdrücklich **keine Authority-Schicht**:
/// Gruppierung oder Mustererkennung darf weder Tool-Berechtigungen noch
/// Approval-Entscheidungen verändern.
#[derive(Debug)]
pub(crate) struct ToolGroupCell {
    /// Die gruppierten Zellen in Ankunftsreihenfolge.
    pub cells: Vec<SharedToolCell>,
    /// Nutzer-Umschalter (Ctrl+O).
    pub expanded: bool,
}

impl Default for ToolGroupCell {
    /// Entspricht [`ToolGroupCell::new`] (leer, eingeklappt).
    fn default() -> Self {
        Self::new()
    }
}

impl ToolGroupCell {
    /// Erzeugt eine leere Gruppe (eingeklappt, ohne Zellen).
    pub(crate) fn new() -> Self {
        Self {
            cells: Vec::new(),
            expanded: false,
        }
    }

    /// Hängt eine weitere Zelle an die Gruppe an.
    ///
    /// # Argumente
    /// - `cell` ([`SharedToolCell`]): die anzuhängende, bereits geteilte
    ///   Zelle (dieselbe Instanz, die auch `TurnEvent::ToolCallCompleted`
    ///   fortschreibt).
    pub(crate) fn push(&mut self, cell: SharedToolCell) {
        self.cells.push(cell);
    }

    /// Prüft, ob ein Werkzeugname noch in eine Lese-Gruppe passt.
    ///
    /// # Argumente
    /// - `tool_name` (`&str`): der zu prüfende Werkzeugname.
    ///
    /// # Rückgabe
    /// `true` für `fs.read`, `fs.search`, `fs.grep`, `fs.list`, `fs.glob`,
    /// `doc.read_pdf` (ebenfalls ein reines Lesewerkzeug); sonst `false`
    /// (z. B. `shell.exec`, `fs.write`, unbekannte Werkzeuge — jeder Aufruf
    /// mit Seiteneffekten oder unbekanntem Verhalten bleibt eine eigene,
    /// einzeln sichtbare Zelle).
    pub(crate) fn accepts(tool_name: &str) -> bool {
        matches!(
            tool_name,
            "fs.read" | "fs.search" | "fs.grep" | "fs.list" | "fs.glob" | "doc.read_pdf"
        )
    }

    /// Zählt Kategorie- und Fehlschlags-Vorkommen über alle enthaltenen
    /// Zellen. Ein vergifteter Lock zählt als `Running` (konservativ: eher
    /// zu viel „läuft noch“ anzeigen als einen Fehler zu verschlucken).
    fn tally(&self) -> ToolGroupTally {
        let mut tally = ToolGroupTally::default();
        for cell in &self.cells {
            let Ok(guard) = cell.lock() else {
                tally.running += 1;
                continue;
            };
            match guard.tool_name.as_str() {
                "fs.read" | "doc.read_pdf" => tally.read += 1,
                "fs.search" | "fs.grep" => tally.search += 1,
                "fs.list" | "fs.glob" => tally.list += 1,
                _ => {}
            }
            match guard.state {
                ToolState::Running => tally.running += 1,
                ToolState::Failed => tally.failed += 1,
                ToolState::Succeeded => {}
            }
        }
        tally
    }

    /// Rendert die Gruppe mit explizitem [`ToolVerbosity`]: `Activity` nutzt
    /// die kompakte Sammelzeile (Gruppensummen), `Verbose` verhält sich wie
    /// ausgeklappt; siehe [`ToolCell::display_lines_with`].
    pub(crate) fn display_lines_with(
        &self,
        width: u16,
        theme: style::Theme,
        verbosity: ToolVerbosity,
    ) -> Vec<Line<'static>> {
        let expanded = self.expanded || matches!(verbosity, ToolVerbosity::Verbose);

        if expanded {
            // B11a: ausgeklappt bleibt die Detailansicht wie unter `Compact`
            // („expanded wie bisher“) — nur `Verbose` reicht die rohen
            // Argumente an die Kindzellen durch, `Activity` nicht.
            let child_verbosity = match verbosity {
                ToolVerbosity::Verbose => ToolVerbosity::Verbose,
                ToolVerbosity::Activity | ToolVerbosity::Compact => ToolVerbosity::Compact,
            };
            let mut lines: Vec<Line<'static>> = Vec::new();
            for cell in &self.cells {
                let Ok(guard) = cell.lock() else {
                    lines.push(Line::from(Span::styled(
                        "⚠ Werkzeugzelle nicht lesbar (Sperre vergiftet)",
                        style::warning_style(theme),
                    )));
                    continue;
                };
                lines.extend(guard.display_lines_with(width, theme, child_verbosity));
            }
            return lines;
        }

        let tally = self.tally();
        let mut parts: Vec<String> = Vec::new();
        if tally.read > 0 {
            let noun = if tally.read == 1 { "Datei" } else { "Dateien" };
            parts.push(format!("{} {noun} gelesen", tally.read));
        }
        if tally.search > 0 {
            parts.push(format!("{} Muster gesucht", tally.search));
        }
        if tally.list > 0 {
            let noun = if tally.list == 1 {
                "Verzeichnis"
            } else {
                "Verzeichnisse"
            };
            parts.push(format!("{} {noun} gelistet", tally.list));
        }
        let summary = if parts.is_empty() {
            "0 Werkzeuge".to_owned()
        } else {
            parts.join(", ")
        };

        let glyph_style = if tally.failed > 0 {
            style::error_style(theme)
        } else if tally.running > 0 {
            style::dim_style(theme)
        } else {
            style::success_style(theme)
        };

        let mut lines: Vec<Line<'static>> = Vec::new();
        push_header_line(
            &mut lines,
            HeaderPart {
                text: "● ",
                style: glyph_style,
            },
            HeaderPart {
                text: &summary,
                style: Style::default().add_modifier(Modifier::BOLD),
            },
            HeaderPart {
                text: "",
                style: style::dim_style(theme),
            },
            width,
        );

        if tally.failed > 0 {
            let note = format!("{} fehlgeschlagen", tally.failed);
            push_indented_wrapped(
                &mut lines,
                &note,
                width,
                TOOL_RESULT_LEAD,
                style::error_style(theme),
            );
        }

        lines
    }
}

/// Ergebnis von [`ToolGroupCell::tally`]: Zählung je Kategorie und Status.
#[derive(Debug, Default, Clone, Copy)]
struct ToolGroupTally {
    /// Zahl der `fs.read`-/`doc.read_pdf`-Zellen.
    read: usize,
    /// Zahl der `fs.search`/`fs.grep`-Zellen.
    search: usize,
    /// Zahl der `fs.list`/`fs.glob`-Zellen.
    list: usize,
    /// Zahl der Zellen mit `state == Failed`.
    failed: usize,
    /// Zahl der Zellen mit `state == Running` (plus vergiftete Locks).
    running: usize,
}

impl HistoryCell for ToolGroupCell {
    /// Rendert mit `verbosity = Compact`, d. h. gesteuert allein über
    /// `self.expanded`. Siehe [`ToolGroupCell::display_lines_with`].
    fn display_lines(&self, width: u16, theme: style::Theme) -> Vec<Line<'static>> {
        self.display_lines_with(width, theme, ToolVerbosity::Compact)
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
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
            tenant: None,
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

    /// B14: Keine Zeile breiter als `width`, auch bei CJK (Breite 2 je
    /// Zeichen) — Metrik ist unicode-width, nicht Zeichenanzahl.
    #[test]
    fn test_wrap_plain_cjk_never_exceeds_width() {
        use unicode_width::UnicodeWidthStr;
        let text = "你好世界 これは 日本語の テキストです longer_ascii_word_here";
        let lines = lines_to_strings(&wrap_plain(text, 10));
        assert!(lines.len() >= 2, "war: {lines:?}");
        for line in &lines {
            let width = UnicodeWidthStr::width(line.as_str());
            assert!(width <= 10, "Zeile '{line}' ist {width} Spalten breit");
        }
    }

    /// B14: Überlange Pfade/URLs brechen an weichen Trennpunkten (`/`, `·`,
    /// `_`), der Trenner bleibt am Segmentende; kein Einzelwort-Überhang.
    #[test]
    fn test_wrap_plain_breaks_paths_at_soft_break_points() {
        use unicode_width::UnicodeWidthStr;
        let text = "crates/harw-tui/src/history_cell.rs und a_very_long_section_name_here";
        let lines = lines_to_strings(&wrap_plain(text, 12));
        assert!(lines.len() >= 2, "war: {lines:?}");
        for line in &lines {
            let width = UnicodeWidthStr::width(line.as_str());
            assert!(width <= 12, "Zeile '{line}' ist {width} Spalten breit");
        }
        // Bruch an '/': jedes Segment endet mit '/' oder ist der Rest.
        let path_line = lines
            .iter()
            .find(|l| l.contains("crates/"))
            .ok_or(TestError::Missing("Pfadsegment"))?;
        assert!(path_line.ends_with('/'), "war: {path_line:?}");
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

    /// Prüft, dass `ReasoningHistoryCell::display_lines` die `"∴ "`-Markierung
    /// gefolgt vom Zusammenfassungstext liefert und eine einzeilige
    /// Zusammenfassung keinen Ausklapp-Hinweis bekommt.
    #[test]
    fn test_reasoning_cell_has_marker_prefix() -> TestResult {
        let cell = ReasoningHistoryCell::new("Denke über die Lösung nach");
        let lines = lines_to_strings(&cell.display_lines(80, style::Theme::Dark));
        let first = lines.first().ok_or(TestError::Missing("erste Zeile"))?;
        if first != "∴ Denke über die Lösung nach" || lines.len() != 1 {
            return Err(TestError::Unexpected(format!("war: {lines:?}")));
        }
        Ok(())
    }

    /// Eingeklappt (Standard): nur die erste Zeile plus Hinweis mit der
    /// Gesamtzahl der Zeilen der ausgeklappten Darstellung.
    #[test]
    fn test_reasoning_cell_collapsed_by_default_shows_first_line_and_hint() -> TestResult {
        let cell = ReasoningHistoryCell::new("Erste Zeile\nZweite Zeile\nDritte Zeile");
        if cell.is_expanded() {
            return Err(TestError::Unexpected(
                "Standard muss eingeklappt sein".to_owned(),
            ));
        }
        let lines = lines_to_strings(&cell.display_lines(80, style::Theme::Dark));
        if lines
            != vec![
                "∴ Erste Zeile".to_owned(),
                "  … (3 Zeilen · Ctrl+O)".to_owned(),
            ]
        {
            return Err(TestError::Unexpected(format!("war: {lines:?}")));
        }
        Ok(())
    }

    /// Ausgeklappt: vollständiger Text, bündig eingerückt, ohne Hinweis.
    #[test]
    fn test_reasoning_cell_expanded_shows_full_text() -> TestResult {
        let mut cell = ReasoningHistoryCell::new("Erste Zeile\nZweite Zeile\nDritte Zeile");
        cell.set_expanded(true);
        let lines = lines_to_strings(&cell.display_lines(80, style::Theme::Dark));
        let expected = vec![
            "∴ Erste Zeile".to_owned(),
            "  Zweite Zeile".to_owned(),
            "  Dritte Zeile".to_owned(),
        ];
        if lines != expected {
            return Err(TestError::Unexpected(format!("war: {lines:?}")));
        }
        Ok(())
    }

    /// Ein langer Absatz ohne Zeilenumbruch wird eingeklappt ebenfalls auf eine
    /// Zeile reduziert; `N` entspricht der ausgeklappten Zeilenzahl.
    #[test]
    fn test_reasoning_cell_long_paragraph_collapses_to_one_row() -> TestResult {
        let text = "wort ".repeat(40);
        let collapsed = ReasoningHistoryCell::new(text.clone());
        let expanded = ReasoningHistoryCell {
            expanded: true,
            ..ReasoningHistoryCell::new(text)
        };
        let full = expanded.display_lines(30, style::Theme::Dark);
        let short = lines_to_strings(&collapsed.display_lines(30, style::Theme::Dark));
        let hint = format!("… ({} Zeilen · Ctrl+O)", full.len());
        if full.len() < 2 || short.len() != 2 || !short[1].contains(&hint) {
            return Err(TestError::Unexpected(format!(
                "voll: {} Zeilen, kurz: {short:?}",
                full.len()
            )));
        }
        Ok(())
    }

    /// Der Text ist gedimmt-kursiv gestylt.
    #[test]
    fn test_reasoning_cell_text_is_italic() -> TestResult {
        let cell = ReasoningHistoryCell::new("Kursiv");
        let lines = cell.display_lines(80, style::Theme::Dark);
        let first = lines.first().ok_or(TestError::Missing("erste Zeile"))?;
        let text_span = first.spans.last().ok_or(TestError::Missing("Text-Span"))?;
        if !text_span.style.add_modifier.contains(Modifier::ITALIC) {
            return Err(TestError::Unexpected(format!(
                "Stil: {:?}",
                text_span.style
            )));
        }
        Ok(())
    }

    /// `with_origin` rendert `"[rolle] "` hinter der Markierung; Folgezeilen
    /// sind bündig unter dem Text eingerückt; leere Rolle entfernt die Herkunft.
    #[test]
    fn test_reasoning_cell_with_origin_renders_role_prefix() -> TestResult {
        let mut cell =
            ReasoningHistoryCell::new("Plan prüfen\nDann lesen").with_origin(" explorer ");
        cell.set_expanded(true);
        let lines = lines_to_strings(&cell.display_lines(80, style::Theme::Dark));
        let expected = vec![
            "∴ [explorer] Plan prüfen".to_owned(),
            "             Dann lesen".to_owned(),
        ];
        if lines != expected {
            return Err(TestError::Unexpected(format!("war: {lines:?}")));
        }
        let cleared = ReasoningHistoryCell::new("x").with_origin("   ");
        if cleared.origin.is_some() {
            return Err(TestError::Unexpected(
                "leere Rolle muss None ergeben".to_owned(),
            ));
        }
        Ok(())
    }

    /// Die geteilte Variante rendert wie die Zelle selbst und folgt dem über
    /// eine zweite Referenz gesetzten Ausklapp-Zustand (Ctrl+O-Pfad in `app.rs`).
    #[test]
    fn test_shared_reasoning_cell_follows_expansion_toggle() -> TestResult {
        let shared = ReasoningHistoryCell::new("a\nb").into_shared();
        let boxed: Box<dyn HistoryCell> = Box::new(Arc::clone(&shared));
        if boxed.display_lines(80, style::Theme::Dark).len() != 2 {
            return Err(TestError::Unexpected(
                "eingeklappt: 2 Zeilen erwartet".to_owned(),
            ));
        }
        {
            let mut guard = shared.lock().map_err(ctx("Mutex vergiftet"))?;
            let next = !guard.is_expanded();
            guard.set_expanded(next);
        }
        let lines = lines_to_strings(&boxed.display_lines(80, style::Theme::Dark));
        if lines != vec!["∴ a".to_owned(), "  b".to_owned()] {
            return Err(TestError::Unexpected(format!("war: {lines:?}")));
        }
        Ok(())
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
        let cell = PlanGraphCell::full(plan);
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
        let cell = PlanGraphCell::full(plan);
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
        let cell = PlanGraphCell::full(plan);
        let lines = cell.display_lines(80, style::Theme::Dark);
        let rendered = lines_to_strings(&lines);
        assert_eq!(rendered, vec!["(keine Knoten im Plan)".to_owned()]);
    }

    /// TUI-03: ein weiterer Stand desselben Plans zeigt eingeklappt nur
    /// Fortschritt und den geänderten Knoten; ausgeklappt alle Knoten.
    #[test]
    fn test_plan_graph_cell_delta_shows_only_changed_node() {
        let total_nodes = 95;
        let before: Vec<PlanNode> = (0..total_nodes)
            .map(|i| make_plan_node(&format!("t-{i}"), PlanNodeStatus::Ready, None, "Ziel"))
            .collect();
        let mut after = before.clone();
        after[3].status = PlanNodeStatus::Completed;
        after[3].objective = "Schritt drei erledigt".to_owned();
        let previous = make_plan(before);
        let mut cell = PlanGraphCell::delta(&previous, make_plan(after));
        assert_eq!(
            cell.changed.as_ref().map(Vec::len),
            Some(1),
            "genau ein Knoten geändert"
        );

        let collapsed = lines_to_strings(&cell.display_lines(120, style::Theme::Dark));
        assert_eq!(
            collapsed.len(),
            3,
            "Fortschritt + Knoten + Hinweis: {collapsed:?}"
        );
        assert!(
            collapsed[0].contains("1/95 erledigt (1 %)"),
            "war: {collapsed:?}"
        );
        assert!(
            collapsed[1].starts_with("● t-3 ·") && collapsed[1].contains("Schritt drei erledigt"),
            "war: {collapsed:?}"
        );
        assert_eq!(
            collapsed[2], "… 95 Knoten · ctrl+o zum Ausklappen",
            "war: {collapsed:?}"
        );
        assert!(
            !collapsed
                .iter()
                .any(|l| l.contains("weitere Knoten ausgeblendet")),
            "war: {collapsed:?}"
        );

        cell.set_expanded(true);
        let expanded = lines_to_strings(&cell.display_lines(120, style::Theme::Dark));
        assert_eq!(expanded.len(), total_nodes + 1, "Fortschritt + alle Knoten");
        assert!(expanded[0].contains("1/95 erledigt"), "war: {expanded:?}");
    }

    /// Delta ohne Knotenänderung und mit entfernten Knoten nennt beides; ein
    /// anderer Plan ergibt ein Vollbild.
    #[test]
    fn test_plan_graph_cell_delta_notes_and_plan_switch() {
        let previous = make_plan(vec![
            make_plan_node("t-1", PlanNodeStatus::Ready, None, "a"),
            make_plan_node("t-2", PlanNodeStatus::Ready, None, "b"),
        ]);
        let same = make_plan(vec![make_plan_node(
            "t-1",
            PlanNodeStatus::Ready,
            None,
            "a",
        )]);
        let cell = PlanGraphCell::delta(&previous, same);
        let rendered = lines_to_strings(&cell.display_lines(120, style::Theme::Dark));
        assert_eq!(
            rendered.last().map(String::as_str),
            Some("… keine Knotenänderung · 1 Knoten entfernt · 1 Knoten · ctrl+o zum Ausklappen"),
            "war: {rendered:?}"
        );

        let mut other = make_plan(vec![make_plan_node(
            "x-1",
            PlanNodeStatus::Ready,
            None,
            "c",
        )]);
        other.id = PlanId::new("p-anders");
        let switched = PlanGraphCell::delta(&previous, other);
        assert!(switched.changed.is_none(), "anderer Plan → Vollbild");
    }

    // ── Sanitisierung aller Zelltypen (W1-08) ───────────────────────────

    /// Nutzlast mit ESC-Sequenz, OSC 52, C1, Bidi und Zero-Width.
    const HOSTILE: &str = "a\u{1b}[2J\u{1b}]52;c;ZXZpbA==\u{07}\u{9b}\u{202e}\u{200b}z";

    /// Prüft, dass keine Zeile ein Steuer- oder unsichtbares Formatzeichen trägt.
    fn assert_lines_terminal_safe(lines: &[Line<'static>]) {
        for row in lines_to_strings(lines) {
            for c in row.chars() {
                assert!(
                    !c.is_control() && !crate::sanitize::is_invisible_format_char(c),
                    "unsicheres Zeichen U+{:04X} in {row:?}",
                    u32::from(c)
                );
            }
        }
    }

    #[test]
    fn test_all_cell_types_render_hostile_text_safely() {
        let theme = style::Theme::Dark;
        let plan_node = make_plan_node(HOSTILE, PlanNodeStatus::Ready, Some(1), HOSTILE);
        let cells: Vec<Box<dyn HistoryCell>> = vec![
            Box::new(PlainHistoryCell {
                lines: vec![Line::from(HOSTILE)],
            }),
            Box::new(UserHistoryCell {
                text: HOSTILE.to_owned(),
            }),
            Box::new(AssistantHistoryCell {
                source: format!("{HOSTILE}\n{HOSTILE}"),
            }),
            Box::new(ReasoningHistoryCell::new(HOSTILE)),
            Box::new(ReasoningHistoryCell {
                expanded: true,
                ..ReasoningHistoryCell::new(format!("{HOSTILE}\n{HOSTILE}")).with_origin(HOSTILE)
            }),
            Box::new(SubAgentCell {
                child_id: HOSTILE.to_owned(),
                role: HOSTILE.to_owned(),
                question: Some(HOSTILE.to_owned()),
                tool_calls: 0,
                tokens: 0,
                status: SubAgentStatus::Done {
                    outcome: HOSTILE.to_owned(),
                    duration_ms: 1,
                },
            }),
            Box::new(PlanGraphCell::full(make_plan(vec![plan_node]))),
            Box::new(GoalCell {
                statement: HOSTILE.to_owned(),
                report: GoalReport {
                    criteria_met: vec![],
                    criteria_open: vec![],
                    invariants_violated: vec![HOSTILE.to_owned()],
                    coverage: 1.0,
                    blocking_nodes: vec![],
                    next_actions: vec![],
                },
            }),
        ];
        for cell in &cells {
            for width in [8_u16, 40, 120] {
                let lines = cell.display_lines(width, theme);
                assert!(!lines.is_empty(), "{cell:?}");
                assert_lines_terminal_safe(&lines);
            }
        }
        // `ToolCell`/`ToolGroupCell` (Werkzeugaufruf-Lebenszyklus) und die
        // Freigabefrage (`approval_dialog::ApprovalDialog`) haben eigene
        // Sanitisierungs-Tests (siehe `test_tool_cell_sanitizes_hostile_stdout`
        // unten bzw. `approval_dialog::tests::test_hostile_argument_text_is_sanitized`).
    }

    /// Plain-Zellen behalten Stil, verlieren aber die ESC-Sequenz.
    #[test]
    fn test_plain_cell_keeps_style_and_strips_escape() {
        let styled = style::warning_style(style::Theme::Dark);
        let cell = PlainHistoryCell {
            lines: vec![Line::from(Span::styled("\u{1b}[31mdiff\tzeile", styled))],
        };
        let lines = cell.display_lines(80, style::Theme::Dark);
        assert_eq!(lines[0].spans[0].style, styled);
        assert_eq!(
            lines_to_strings(&lines),
            vec!["⟨ESC⟩diff    zeile".to_owned()]
        );
    }

    /// Assistenten-Text: sichtbarer Text bleibt, OSC-52-Nutzlast verschwindet.
    #[test]
    fn test_assistant_cell_strips_osc52_payload() {
        let cell = AssistantHistoryCell {
            source: "Hallo \u{1b}]52;c;ZXZpbA==\u{07}Welt".to_owned(),
        };
        let rendered = lines_to_strings(&cell.display_lines(80, style::Theme::Dark));
        assert_eq!(rendered, vec!["» Hallo ⟨ESC⟩Welt".to_owned()]);
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

    // ── truncate_chars ─────────────────────────────────────────────────

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

    // ── ToolCell (Plan Schritt 2 / Contract A5) ──────────────────────────

    /// Baut einen `ToolCall` mit zufälliger ID für die Tests unten.
    fn make_tool_call(tool: &str, arguments: harw_tools::serde_json::Value) -> ToolCall {
        ToolCall {
            id: harw_types::ToolCallId::new(),
            name: harw_extension_api::ToolName::new(tool),
            arguments,
        }
    }

    /// TUI-02: `shell.exec` wird zu `Shell(<erste Zeile>)` (läuft unter
    /// `/bin/sh`, nicht bash), nicht zu rohem JSON.
    #[test]
    fn test_tool_label_shell_exec_is_shell_first_line() {
        let call = make_tool_call(
            "shell.exec",
            harw_tools::serde_json::json!({ "command": "git status --short\necho done" }),
        );
        assert_eq!(tool_label(&call), "Shell(git status --short)");
    }

    /// `shell.exec` kürzt die erste Zeile auf 120 Zeichen.
    #[test]
    fn test_tool_label_shell_exec_truncates_long_first_line() {
        let long_command = "x".repeat(200);
        let call = make_tool_call(
            "shell.exec",
            harw_tools::serde_json::json!({ "command": long_command }),
        );
        let label = tool_label(&call);
        assert!(label.starts_with("Shell("), "war: {label:?}");
        // "Shell(" + 120 Zeichen (119 'x' + Ellipse) + ")".
        assert_eq!(label.chars().count(), "Shell(".len() + 120 + 1);
    }

    /// TUI-04: ein Heredoc-Rumpf wird im Label zu `… heredoc N Zeilen`
    /// eingeklappt; nur die öffnende Zeile bleibt sichtbar.
    #[test]
    fn test_tool_label_shell_exec_collapses_heredoc_body() {
        let call = make_tool_call(
            "shell.exec",
            harw_tools::serde_json::json!({
                "command": "cd crate && python3 - <<'EOF'\nimport sys\nprint(1)\nprint(2)\nEOF\necho fertig"
            }),
        );
        assert_eq!(
            tool_label(&call),
            "Shell(cd crate && python3 - <<'EOF' … heredoc 3 Zeilen)"
        );

        // `<<-"END"` mit eingerücktem Trenner; danach ohne Trenner: alle übrigen Zeilen.
        let dashed = make_tool_call(
            "shell.exec",
            harw_tools::serde_json::json!({ "command": "cat <<-\"END\" > f\n\ta\n\tEND" }),
        );
        assert_eq!(
            tool_label(&dashed),
            "Shell(cat <<-\"END\" > f … heredoc 1 Zeilen)"
        );
        let unterminated = make_tool_call(
            "shell.exec",
            harw_tools::serde_json::json!({ "command": "cat <<EOF\na\nb" }),
        );
        assert_eq!(
            tool_label(&unterminated),
            "Shell(cat <<EOF … heredoc 2 Zeilen)"
        );

        // Ein Here-String ist kein Heredoc.
        let here_string = make_tool_call(
            "shell.exec",
            harw_tools::serde_json::json!({ "command": "grep x <<< \"$v\"\nzwei" }),
        );
        assert_eq!(tool_label(&here_string), "Shell(grep x <<< \"$v\")");
    }

    /// TUI-04: die Werkzeugzelle zeigt den Heredoc-Rumpf nicht, auch nicht
    /// ausgeklappt in der Kopfzeile.
    #[test]
    fn test_tool_cell_shell_heredoc_body_not_rendered() {
        let call = make_tool_call(
            "shell.exec",
            harw_tools::serde_json::json!({
                "command": "python3 - <<'EOF'\ngeheimer_rumpf = 1\nEOF"
            }),
        );
        let mut cell = ToolCell::started(&call);
        cell.complete(
            &harw_protocol::items::ToolCallResult::success(harw_tools::serde_json::json!({
                "exit_code": 0, "stdout": "ok", "stderr": ""
            })),
            7,
        );
        cell.set_expanded(true);
        let lines = lines_to_strings(&cell.display_lines(120, style::Theme::Dark));
        assert!(
            lines[0].starts_with("● Shell(python3 - <<'EOF' … heredoc 1 Zeilen) · 7ms"),
            "war: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("geheimer_rumpf")),
            "war: {lines:?}"
        );
    }

    /// `job.*` erhalten eigene Label; der Befehl steht nicht im Label.
    #[test]
    fn test_tool_label_job_tools() {
        let start = make_tool_call(
            "job.start",
            harw_tools::serde_json::json!({ "name": "ladybird build", "command": "cmake --build out" }),
        );
        assert_eq!(tool_label(&start), "Job(ladybird build)");
        let unnamed = make_tool_call(
            "job.start",
            harw_tools::serde_json::json!({ "command": "make" }),
        );
        assert_eq!(tool_label(&unnamed), "Job(unbenannt)");
        let wait = make_tool_call(
            "job.wait",
            harw_tools::serde_json::json!({ "job_id": "job-1", "timeout_secs": 60 }),
        );
        assert_eq!(tool_label(&wait), "job.wait(job-1, ≤60s)");
        let status = make_tool_call(
            "job.status",
            harw_tools::serde_json::json!({ "job_id": "job-1" }),
        );
        assert_eq!(tool_label(&status), "job.status(job-1)");
        let logs = make_tool_call(
            "job.logs",
            harw_tools::serde_json::json!({ "job_id": "job-1", "tail": 50 }),
        );
        assert_eq!(tool_label(&logs), "job.logs(job-1)");
        let stop = make_tool_call(
            "job.stop",
            harw_tools::serde_json::json!({ "job_id": "job-1", "signal": "KILL" }),
        );
        assert_eq!(tool_label(&stop), "job.stop(job-1, KILL)");
        let list = make_tool_call(
            "job.list",
            harw_tools::serde_json::json!({ "kind": "process" }),
        );
        assert_eq!(tool_label(&list), "job.list(process)");
    }

    /// `job.start`: eingeklappt Job-ID/Zustand und Ort, der Befehl erst
    /// ausgeklappt; der Ort kommt ersatzweise aus `executed_on`.
    #[test]
    fn test_tool_cell_job_start_hides_command_until_expanded() {
        let call = make_tool_call(
            "job.start",
            harw_tools::serde_json::json!({ "name": "build", "command": "cargo build --release" }),
        );
        let mut cell = ToolCell::started(&call);
        cell.complete(
            &harw_protocol::items::ToolCallResult::success(harw_tools::serde_json::json!({
                "job_id": "job-7",
                "state": "running",
                "command": "cargo build --release",
                "executed_on": "sandbox",
            })),
            3,
        );
        assert_eq!(cell.placement, Some(ToolPlacement::Sandbox));
        let collapsed = lines_to_strings(&cell.display_lines(120, style::Theme::Dark));
        assert!(
            collapsed[0].starts_with("● Job(build) · 3ms · sandbox"),
            "war: {collapsed:?}"
        );
        assert!(
            collapsed.iter().any(|l| l.contains("job-7 · running")),
            "war: {collapsed:?}"
        );
        assert!(
            !collapsed.iter().any(|l| l.contains("cargo build")),
            "Befehl darf eingeklappt nicht erscheinen: {collapsed:?}"
        );
        cell.set_expanded(true);
        let expanded = lines_to_strings(&cell.display_lines(120, style::Theme::Dark));
        assert!(
            expanded
                .iter()
                .any(|l| l.contains("Befehl: cargo build --release")),
            "war: {expanded:?}"
        );
    }

    /// TUI-01: der Ort aus dem Ereignis erscheint in der Kopfzeile und hat
    /// Vorrang vor `executed_on`; ohne beides erscheint kein Ort.
    #[test]
    fn test_tool_cell_placement_badge() {
        let call = make_tool_call(
            "fs.write",
            harw_tools::serde_json::json!({ "path": "a.txt" }),
        );
        let ok = harw_protocol::items::ToolCallResult::success(harw_tools::serde_json::json!({
            "executed_on": "host"
        }));

        let mut gateway = ToolCell::started(&call);
        gateway.complete_at(
            &ok,
            12,
            Some(&ToolPlacement::Gateway {
                node: Some("gw-1".to_owned()),
            }),
        );
        let lines = lines_to_strings(&gateway.display_lines(80, style::Theme::Dark));
        assert!(
            lines[0].starts_with("● Write(a.txt) · 12ms · gateway gw-1"),
            "war: {lines:?}"
        );

        let mut sandbox = ToolCell::started(&call);
        sandbox.complete_at(&ok, 12, Some(&ToolPlacement::Sandbox));
        let lines = lines_to_strings(&sandbox.display_lines(80, style::Theme::Dark));
        assert!(lines[0].contains("· sandbox"), "war: {lines:?}");
        assert!(!lines[0].contains("host"), "war: {lines:?}");

        let mut none = ToolCell::started(&call);
        none.complete_at(
            &harw_protocol::items::ToolCallResult::success(harw_tools::serde_json::json!({
                "ok": true
            })),
            12,
            None,
        );
        assert_eq!(none.placement, None);
        let lines = lines_to_strings(&none.display_lines(80, style::Theme::Dark));
        for badge in ["host", "sandbox", "gateway", "unknown"] {
            assert!(!lines[0].contains(badge), "war: {lines:?}");
        }

        let mut unknown = ToolCell::started(&call);
        unknown.complete_at(&ok, 12, Some(&ToolPlacement::Unknown));
        let lines = lines_to_strings(&unknown.display_lines(80, style::Theme::Dark));
        assert!(lines[0].ends_with("· unknown"), "war: {lines:?}");
    }

    /// `plan {action: step}` erhält ein kompaktes Label mit Knoten und Zustand.
    #[test]
    fn test_tool_label_plan_step() {
        let call = make_tool_call(
            "plan",
            harw_tools::serde_json::json!({
                "action": "step", "id": "t-12", "state": "done", "evidence": "cargo_test:cargo test"
            }),
        );
        assert_eq!(tool_label(&call), "plan(step t-12 → done)");
        let inspect = make_tool_call(
            "plan",
            harw_tools::serde_json::json!({ "action": "inspect" }),
        );
        assert_eq!(tool_label(&inspect), "plan(inspect)");
    }

    /// `fs.read`, `fs.write`, `fs.list`/`fs.glob` und `fs.search`/`fs.grep`
    /// ergeben die vorgesehenen Klartext-Label, niemals rohes JSON.
    #[test]
    fn test_tool_label_fs_tools() {
        let read = make_tool_call(
            "fs.read",
            harw_tools::serde_json::json!({ "path": "src/app.rs" }),
        );
        assert_eq!(tool_label(&read), "Read(src/app.rs)");

        let write = make_tool_call(
            "fs.write",
            harw_tools::serde_json::json!({ "path": "src/app.rs", "content": "x" }),
        );
        assert_eq!(tool_label(&write), "Write(src/app.rs)");

        let list = make_tool_call("fs.list", harw_tools::serde_json::json!({ "path": "src" }));
        assert_eq!(tool_label(&list), "List(src)");

        let glob = make_tool_call("fs.glob", harw_tools::serde_json::json!({ "path": "." }));
        assert_eq!(tool_label(&glob), "List(.)");

        let search = make_tool_call(
            "fs.search",
            harw_tools::serde_json::json!({ "pattern": "TODO", "path": "src" }),
        );
        assert_eq!(tool_label(&search), "Search(\"TODO\" in src)");

        let grep = make_tool_call(
            "fs.grep",
            harw_tools::serde_json::json!({ "pattern": "TODO" }),
        );
        assert_eq!(tool_label(&grep), "Search(\"TODO\" in .)");
    }

    /// `doc.read_pdf` ergibt `ReadPdf(<Pfad>)` ohne `pages`-Argument bzw.
    /// `ReadPdf(<Pfad>, Seiten <Bereich>)` mit gesetztem `pages`-Argument —
    /// dieselbe Darstellungsform wie `fs.read`, ergänzt um den Seitenbereich.
    #[test]
    fn test_tool_label_doc_read_pdf() {
        let without_pages = make_tool_call(
            "doc.read_pdf",
            harw_tools::serde_json::json!({ "path": "docs/report.pdf" }),
        );
        assert_eq!(tool_label(&without_pages), "ReadPdf(docs/report.pdf)");

        let with_pages = make_tool_call(
            "doc.read_pdf",
            harw_tools::serde_json::json!({ "path": "docs/report.pdf", "pages": "2-5" }),
        );
        assert_eq!(
            tool_label(&with_pages),
            "ReadPdf(docs/report.pdf, Seiten 2-5)"
        );
    }

    /// Geht die Datei an einen Remote-OCR-Dienst, nennt das Label den Host.
    #[test]
    fn test_read_pdf_label_names_remote_ocr_host() {
        assert_eq!(
            read_pdf_label("a.pdf", Some("1-2"), Some("api.mistral.ai")),
            "ReadPdf(a.pdf, Seiten 1-2) → remote OCR (api.mistral.ai)"
        );
        assert_eq!(read_pdf_label("a.pdf", None, None), "ReadPdf(a.pdf)");
    }

    /// `transfer_to_<rolle>` ergibt `Agent(<rolle>)`.
    #[test]
    fn test_tool_label_transfer_to_role_is_agent() {
        let call = make_tool_call("transfer_to_explorer", harw_tools::serde_json::json!({}));
        assert_eq!(tool_label(&call), "Agent(explorer)");
        // Plan R9, Teil C: `agents.delegate` zeigt denselben Handoff.
        let call = make_tool_call(
            "agents.delegate",
            harw_tools::serde_json::json!({ "agent": "evidence-critic", "task": "prüfe" }),
        );
        assert_eq!(tool_label(&call), "Agent(evidence-critic)");
    }

    /// Unbekannte Werkzeuge zeigen niemals eine rohe JSON-Klammer — auch
    /// nicht, wenn ein Argument selbst ein verschachteltes Objekt ist.
    #[test]
    fn test_tool_label_unknown_tool_never_contains_json_braces() {
        let call = make_tool_call(
            "custom.frobnicate",
            harw_tools::serde_json::json!({
                "target": "widget",
                "count": 3,
                "nested": { "a": 1 },
            }),
        );
        let label = tool_label(&call);
        assert!(!label.contains('{'), "war: {label:?}");
        assert!(!label.contains('}'), "war: {label:?}");
        assert!(label.starts_with("custom.frobnicate("), "war: {label:?}");
        assert!(label.contains("target: \"widget\""), "war: {label:?}");
        assert!(label.contains("count: 3"), "war: {label:?}");
    }

    /// Unbekannte Werkzeuge ohne Argumentobjekt ergeben `name()`.
    #[test]
    fn test_tool_label_unknown_tool_without_object_arguments() {
        let call = make_tool_call("custom.ping", harw_tools::serde_json::json!("raw"));
        assert_eq!(tool_label(&call), "custom.ping()");
    }

    /// `shell.exec`-Erfolg mit `exit_code == 0`: `Succeeded`, keine
    /// Zusammenfassung, höchstens drei Vorschauzeilen, Rest gezählt.
    #[test]
    fn test_tool_cell_complete_shell_success_previews_three_lines() {
        let call = make_tool_call(
            "shell.exec",
            harw_tools::serde_json::json!({ "command": "ls" }),
        );
        let mut cell = ToolCell::started(&call);
        let result = harw_protocol::items::ToolCallResult::success(harw_tools::serde_json::json!({
            "exit_code": 0,
            "stdout": "a.txt\nb.txt\nc.txt\nd.txt\n",
            "stderr": "",
        }));
        cell.complete(&result, 42);

        assert_eq!(cell.state, ToolState::Succeeded);
        assert_eq!(cell.summary, None);
        assert_eq!(cell.preview, vec!["a.txt", "b.txt", "c.txt"]);
        assert_eq!(cell.hidden_lines, 1);

        let lines = lines_to_strings(&cell.display_lines(80, style::Theme::Dark));
        assert!(lines[0].starts_with("● Shell(ls) · 42ms"), "war: {lines:?}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("+1 Zeilen (ctrl+o zum Ausklappen)")),
            "war: {lines:?}"
        );
    }

    /// `shell.exec` mit `exit_code != 0`: `Failed` und `"exit N"`-Zusammenfassung.
    #[test]
    fn test_tool_cell_complete_shell_failure_sets_failed_state_and_exit_summary() {
        let call = make_tool_call(
            "shell.exec",
            harw_tools::serde_json::json!({ "command": "false" }),
        );
        let mut cell = ToolCell::started(&call);
        let result = harw_protocol::items::ToolCallResult::success(harw_tools::serde_json::json!({
            "exit_code": 1,
            "stdout": "",
            "stderr": "boom",
        }));
        cell.complete(&result, 5);

        assert_eq!(cell.state, ToolState::Failed);
        assert_eq!(cell.summary.as_deref(), Some("exit 1"));

        let lines = lines_to_strings(&cell.display_lines(80, style::Theme::Dark));
        assert!(lines[0].contains("Shell(false)"), "war: {lines:?}");
        assert!(lines.iter().any(|l| l.contains("exit 1")), "war: {lines:?}");
    }

    /// `ToolCallResult::Error` markiert die Zelle als `Failed`; die
    /// Vorschau ist genau die erste Zeile der Fehlermeldung.
    #[test]
    fn test_tool_cell_complete_error_result_previews_first_line() {
        let call = make_tool_call("fs.read", harw_tools::serde_json::json!({ "path": "x" }));
        let mut cell = ToolCell::started(&call);
        let result =
            harw_protocol::items::ToolCallResult::error("Datei nicht gefunden\nDetails: ENOENT");
        cell.complete(&result, 3);

        assert_eq!(cell.state, ToolState::Failed);
        assert_eq!(cell.preview, vec!["Datei nicht gefunden".to_owned()]);

        let lines = lines_to_strings(&cell.display_lines(80, style::Theme::Dark));
        assert!(lines[0].starts_with("● Read(x)"), "war: {lines:?}");
        assert!(
            lines.iter().any(|l| l.contains("Datei nicht gefunden")),
            "war: {lines:?}"
        );
    }

    /// Ausgeklappt zeigt die Zelle die vollständige Ausgabe statt nur der
    /// Vorschau — kein „ausgeblendet“-Hinweis mehr.
    #[test]
    fn test_tool_cell_expanded_shows_all_output_lines() {
        let call = make_tool_call(
            "shell.exec",
            harw_tools::serde_json::json!({ "command": "ls" }),
        );
        let mut cell = ToolCell::started(&call);
        let result = harw_protocol::items::ToolCallResult::success(harw_tools::serde_json::json!({
            "exit_code": 0,
            "stdout": "eins\nzwei\ndrei\nvier\nfuenf\n",
            "stderr": "",
        }));
        cell.complete(&result, 9);
        cell.set_expanded(true);

        let joined = lines_to_strings(&cell.display_lines(80, style::Theme::Dark)).join("\n");
        for line in ["eins", "zwei", "drei", "vier", "fuenf"] {
            assert!(joined.contains(line), "Zeile {line} fehlt: {joined}");
        }
        assert!(!joined.contains("ausgeblendet"), "war: {joined}");
        assert!(!joined.contains("ctrl+o"), "war: {joined}");
    }

    /// `fs.read`-Erfolg zählt die Zeilen im Feld `content` und zeigt keine
    /// Vorschauzeilen (nur die Zusammenfassung).
    #[test]
    fn test_tool_cell_complete_fs_read_counts_lines() {
        let call = make_tool_call(
            "fs.read",
            harw_tools::serde_json::json!({ "path": "a.txt" }),
        );
        let mut cell = ToolCell::started(&call);
        let result = harw_protocol::items::ToolCallResult::success(harw_tools::serde_json::json!({
            "content": "zeile1\nzeile2\nzeile3",
        }));
        cell.complete(&result, 1);

        assert_eq!(cell.summary.as_deref(), Some("3 Zeilen"));
        assert!(cell.preview.is_empty());
    }

    /// `latex.build` mit `status = not_installed`: Zusammenfassung nennt die
    /// fehlenden Programme, der Hinweis für die Nutzerin steht vollständig
    /// und aufgeklappt in der Zelle.
    #[test]
    fn test_tool_cell_latex_build_not_installed_shows_user_message() {
        let call = make_tool_call(
            "latex.build",
            harw_tools::serde_json::json!({ "file": "main.tex" }),
        );
        let mut cell = ToolCell::started(&call);
        let result = harw_protocol::items::ToolCallResult::success(harw_tools::serde_json::json!({
            "status": "not_installed",
            "missing": ["latexmk", "xelatex"],
            "user_message": "LaTeX fehlt: latexmk, xelatex. · Debian/Ubuntu: sudo apt install latexmk",
        }));
        cell.complete(&result, 1);

        assert_eq!(
            cell.summary.as_deref(),
            Some("LaTeX nicht installiert: latexmk, xelatex")
        );
        assert!(cell.expanded, "Hinweis darf nicht eingeklappt sein");
        assert_eq!(
            cell.full_output,
            vec![
                "LaTeX fehlt: latexmk, xelatex.".to_owned(),
                "Debian/Ubuntu: sudo apt install latexmk".to_owned(),
            ]
        );
    }

    /// `fs.edit`-Erfolg: Zusammenfassung „N Ersetzung(en) in <pfad>“, die
    /// Vorschau zeigt die `-`/`+`-Zeilen des Diff-Ausschnitts.
    #[test]
    fn test_tool_cell_complete_fs_edit_summarizes_replacements() {
        let call = make_tool_call(
            "fs.edit",
            harw_tools::serde_json::json!({
                "path": "src/main.rs",
                "old_string": "a",
                "new_string": "b",
            }),
        );
        assert_eq!(tool_label(&call), "Edit(src/main.rs)");
        let mut cell = ToolCell::started(&call);
        let result = harw_protocol::items::ToolCallResult::success(harw_tools::serde_json::json!({
            "path": "src/main.rs",
            "replacements": 1,
            "diff_excerpt": "@@ Zeile 2 @@\n-    let x = 1;\n+    let x = 2;",
        }));
        cell.complete(&result, 1);

        assert_eq!(cell.summary.as_deref(), Some("1 Ersetzung in src/main.rs"));
        assert_eq!(
            cell.preview,
            vec!["-    let x = 1;".to_owned(), "+    let x = 2;".to_owned()]
        );
        assert_eq!(cell.hidden_lines, 0);
        assert_eq!(cell.full_output.len(), 3);

        let mut many = ToolCell::started(&call);
        let result = harw_protocol::items::ToolCallResult::success(harw_tools::serde_json::json!({
            "path": "a.txt",
            "replacements": 3,
            "diff_excerpt": "@@ Zeile 1 @@\n-foo\n+bar\n@@ Zeile 2 @@\n-foo\n+bar",
        }));
        many.complete(&result, 1);
        assert_eq!(many.summary.as_deref(), Some("3 Ersetzungen in a.txt"));
        assert_eq!(many.preview.len(), TOOL_CELL_COLLAPSED_LINES);
        assert_eq!(many.hidden_lines, 1);
    }

    /// `fs.search`-Erfolg zählt die Treffer im Feld `matches`.
    #[test]
    fn test_tool_cell_complete_fs_search_counts_matches() {
        let call = make_tool_call(
            "fs.search",
            harw_tools::serde_json::json!({ "pattern": "TODO" }),
        );
        let mut cell = ToolCell::started(&call);
        let result = harw_protocol::items::ToolCallResult::success(harw_tools::serde_json::json!({
            "matches": ["a", "b"],
        }));
        cell.complete(&result, 1);

        assert_eq!(cell.summary.as_deref(), Some("2 Treffer"));
    }

    /// Setzt eine Freigabe-Notiz; sie erscheint gedimmt in der Kopfzeile.
    #[test]
    fn test_tool_cell_set_approval_note_appears_in_header() {
        let call = make_tool_call(
            "shell.exec",
            harw_tools::serde_json::json!({ "command": "ls" }),
        );
        let mut cell = ToolCell::started(&call);
        cell.set_approval_note("✓ freigegeben");

        let lines = lines_to_strings(&cell.display_lines(80, style::Theme::Dark));
        assert!(lines[0].contains("✓ freigegeben"), "war: {lines:?}");
    }

    /// Ein ANSI-Escape in `stdout` erreicht nie den gerenderten Buffer.
    #[test]
    fn test_tool_cell_sanitizes_hostile_stdout() {
        let call = make_tool_call(
            "shell.exec",
            harw_tools::serde_json::json!({ "command": "ls" }),
        );
        let mut cell = ToolCell::started(&call);
        let hostile = "\u{1b}[31mROT\u{1b}[0m";
        let result = harw_protocol::items::ToolCallResult::success(harw_tools::serde_json::json!({
            "exit_code": 0,
            "stdout": hostile,
            "stderr": "",
        }));
        cell.complete(&result, 1);

        let joined = lines_to_strings(&cell.display_lines(80, style::Theme::Dark)).join("\n");
        assert!(!joined.contains('\u{1b}'), "war: {joined:?}");
        assert!(joined.contains("⟨ESC⟩ROT⟨ESC⟩"), "war: {joined:?}");
    }

    /// Läuft der Aufruf noch (`Running`), zeigt die Zelle keine Dauer an.
    #[test]
    fn test_tool_cell_running_state_has_no_duration() {
        let call = make_tool_call(
            "shell.exec",
            harw_tools::serde_json::json!({ "command": "ls" }),
        );
        let cell = ToolCell::started(&call);
        assert_eq!(cell.state, ToolState::Running);
        assert_eq!(cell.duration_ms, None);

        let lines = lines_to_strings(&cell.display_lines(80, style::Theme::Dark));
        assert!(!lines[0].contains("ms"), "war: {lines:?}");
    }

    /// `ToolVerbosity::Verbose` verhält sich wie ausgeklappt und zeigt
    /// zusätzlich die rohen Aufrufargumente unter dem Label.
    #[test]
    fn test_tool_cell_verbose_shows_raw_arguments() {
        let call = make_tool_call(
            "shell.exec",
            harw_tools::serde_json::json!({ "command": "ls", "timeout_secs": 30 }),
        );
        let mut cell = ToolCell::started(&call);
        let result = harw_protocol::items::ToolCallResult::success(harw_tools::serde_json::json!({
            "exit_code": 0,
            "stdout": "eins\nzwei\ndrei\nvier\n",
            "stderr": "",
        }));
        cell.complete(&result, 2);

        let compact = lines_to_strings(&cell.display_lines_with(
            80,
            style::Theme::Dark,
            ToolVerbosity::Compact,
        ));
        let verbose = lines_to_strings(&cell.display_lines_with(
            80,
            style::Theme::Dark,
            ToolVerbosity::Verbose,
        ));

        assert!(
            !compact.join("\n").contains("timeout_secs"),
            "war: {compact:?}"
        );
        assert!(
            verbose.join("\n").contains("timeout_secs"),
            "war: {verbose:?}"
        );
        assert!(verbose.join("\n").contains("30"), "war: {verbose:?}");
        // Verbose verhält sich wie ausgeklappt: alle vier Ausgabezeilen da.
        for line in ["eins", "zwei", "drei", "vier"] {
            assert!(
                verbose.iter().any(|l| l.contains(line)),
                "Zeile {line} fehlt: {verbose:?}"
            );
        }
    }

    /// Aktivitätsmodus (B11a): `ToolVerbosity::Activity` rendert genau eine
    /// Zeile — Statuspunkt, Label, Ergebnis-Zusammenfassung und Dauer; weder
    /// rohe Argumente noch Ergebnis-Text.
    #[test]
    fn test_tool_cell_activity_is_single_line() {
        let call = make_tool_call(
            "shell.exec",
            harw_tools::serde_json::json!({ "command": "ls", "timeout_secs": 30 }),
        );
        let mut cell = ToolCell::started(&call);
        let result = harw_protocol::items::ToolCallResult::success(harw_tools::serde_json::json!({
            "exit_code": 0,
            "stdout": "eins\nzwei\n",
            "stderr": "",
        }));
        cell.complete(&result, 2);

        let activity = lines_to_strings(&cell.display_lines_with(
            80,
            style::Theme::Dark,
            ToolVerbosity::Activity,
        ));
        assert_eq!(activity.len(), 1, "war: {activity:?}");
        assert!(activity[0].contains("Shell("), "war: {activity:?}");
        assert!(activity[0].contains("2.0s"), "war: {activity:?}");
        assert!(
            !activity.join("\n").contains("timeout_secs"),
            "rohe Argumente im Aktivitätsmodus: {activity:?}"
        );
        assert!(
            !activity.join("\n").contains("eins"),
            "Ergebnis-Text im Aktivitätsmodus: {activity:?}"
        );
    }

    /// Ausgeklappt zeigt `Activity` die Detailansicht wie Compact-expanded —
    /// rohe Argumente bleiben `Verbose` vorbehalten.
    #[test]
    fn test_tool_cell_activity_expanded_shows_details() {
        let call = make_tool_call(
            "shell.exec",
            harw_tools::serde_json::json!({ "command": "ls", "timeout_secs": 30 }),
        );
        let mut cell = ToolCell::started(&call);
        let result = harw_protocol::items::ToolCallResult::success(harw_tools::serde_json::json!({
            "exit_code": 0,
            "stdout": "eins\nzwei\n",
            "stderr": "",
        }));
        cell.complete(&result, 2);
        cell.set_expanded(true);

        let expanded = lines_to_strings(&cell.display_lines_with(
            80,
            style::Theme::Dark,
            ToolVerbosity::Activity,
        ));
        assert!(expanded.len() > 1, "war: {expanded:?}");
        for line in ["eins", "zwei"] {
            assert!(
                expanded.iter().any(|l| l.contains(line)),
                "Zeile {line} fehlt: {expanded:?}"
            );
        }
        assert!(
            !expanded.join("\n").contains("timeout_secs"),
            "rohe Argumente ohne Verbose: {expanded:?}"
        );
    }

    /// Aktivitätsmodus in der Gruppe: kompakte Sammelzeile mit
    /// Gruppensummen; ausgeklappt wie bisher die Detailzeilen.
    #[test]
    fn test_tool_group_cell_activity_summarizes_and_expands() -> TestResult {
        let mut group = ToolGroupCell::new();
        for path in ["a", "b"] {
            let call = make_tool_call("fs.read", harw_tools::serde_json::json!({ "path": path }));
            let cell = Arc::new(Mutex::new(ToolCell::started(&call)));
            cell.lock().map_err(ctx("Mutex vergiftet"))?.complete(
                &harw_protocol::items::ToolCallResult::success(
                    harw_tools::serde_json::json!({ "content": "x" }),
                ),
                1,
            );
            group.push(cell);
        }

        let activity = lines_to_strings(&group.display_lines_with(
            80,
            style::Theme::Dark,
            ToolVerbosity::Activity,
        ));
        assert!(activity[0].contains("2 Dateien gelesen"), "war: {activity:?}");
        assert!(
            !activity.join("\n").contains("Read(a)"),
            "Detailzeile im Aktivitätsmodus: {activity:?}"
        );

        group.expanded = true;
        let expanded = lines_to_strings(&group.display_lines_with(
            80,
            style::Theme::Dark,
            ToolVerbosity::Activity,
        ))
        .join("\n");
        assert!(expanded.contains("Read(a)"), "war: {expanded:?}");
        assert!(expanded.contains("Read(b)"), "war: {expanded:?}");
        Ok(())
    }

    /// Eine einzelne sehr lange Zeile (generischer Fallback) bleibt bei
    /// Breite 80 eingeklappt auf Kopfzeile + 3 Zeilen + Hinweis begrenzt;
    /// der Hinweis zählt die versteckten Bildschirmzeilen.
    #[test]
    fn test_tool_cell_long_single_line_collapses_to_three_rows() -> TestResult {
        let call = make_tool_call("custom.tool", harw_tools::serde_json::json!({}));
        let mut cell = ToolCell::started(&call);
        let long = "wort ".repeat(4000);
        let result = harw_protocol::items::ToolCallResult::success(
            harw_tools::serde_json::Value::String(long.clone()),
        );
        cell.complete(&result, 1);
        assert!(cell.preview.iter().all(|l| l.chars().count() <= 500));

        let collapsed = lines_to_strings(&cell.display_lines(80, style::Theme::Dark));
        assert!(collapsed.len() <= 1 + 3 + 1, "war: {collapsed:?}");
        let hint = collapsed.last().ok_or(TestError::Missing("letzte Zeile"))?;
        assert!(hint.contains("ctrl+o zum Ausklappen"), "war: {collapsed:?}");
        assert!(collapsed[3].trim_end().ends_with('…'), "war: {collapsed:?}");
        // Versteckt: Rest der gekappten Vorschauzeile (500 Zeichen ≈ 7
        // Bildschirmzeilen, davon 3 gezeigt).
        assert!(!hint.contains("+0 "), "war: {hint}");

        cell.set_expanded(true);
        let expanded = lines_to_strings(&cell.display_lines(80, style::Theme::Dark));
        assert!(expanded.len() > collapsed.len(), "war: {expanded:?}");
        assert!(expanded.len() <= 1 + TOOL_CELL_EXPANDED_LINES + 1);
        assert!(
            expanded.join(" ").contains("wort wort"),
            "war: {expanded:?}"
        );
        assert!(!expanded.join("\n").contains("ctrl+o"));
        Ok(())
    }

    /// Ausgeklappt gilt das Limit in Bildschirmzeilen: eine einzige Zeile,
    /// die umgebrochen weit über [`TOOL_CELL_EXPANDED_LINES`] Zeilen hätte,
    /// wird gekappt und der Rest gezählt.
    #[test]
    fn test_tool_cell_expanded_caps_wrapped_rows() -> TestResult {
        let call = make_tool_call("custom.tool", harw_tools::serde_json::json!({}));
        let mut cell = ToolCell::started(&call);
        let long = "x".repeat(64 * 1024);
        let result = harw_protocol::items::ToolCallResult::success(
            harw_tools::serde_json::Value::String(long),
        );
        cell.complete(&result, 1);
        cell.set_expanded(true);

        let lines = lines_to_strings(&cell.display_lines(80, style::Theme::Dark));
        assert_eq!(
            lines.len(),
            1 + TOOL_CELL_EXPANDED_LINES + 1,
            "{}",
            lines.len()
        );
        let last = lines.last().ok_or(TestError::Missing("letzte Zeile"))?;
        assert!(
            last.contains("Zeilen") && last.contains("… +"),
            "war: {last}"
        );
        Ok(())
    }

    /// `explore.projects`: Zusammenfassung „N Projekte“, je Projekt eine
    /// Vorschauzeile `root (kind)`, ausgeklappt formatiertes JSON.
    #[test]
    fn test_tool_cell_explore_projects_previews_one_line_per_project() -> TestResult {
        let call = make_tool_call("explore.projects", harw_tools::serde_json::json!({}));
        let mut cell = ToolCell::started(&call);
        let result = harw_protocol::items::ToolCallResult::success(harw_tools::serde_json::json!({
            "projects": [
                { "root": ".", "kind": "cargo-workspace", "name": "ws", "manifest": "Cargo.toml", "members": ["a", "b"] },
                { "root": "a", "kind": "cargo-crate", "name": "a", "manifest": "a/Cargo.toml", "members": [] },
            ],
            "total": 2,
            "summary": "13 Einträge (dir 5) · 2 Projekte",
            "truncated": false,
        }));
        cell.complete(&result, 1);

        assert_eq!(cell.summary.as_deref(), Some("2 Projekte"));
        assert_eq!(cell.preview, vec![". (cargo-workspace)", "a (cargo-crate)"]);
        assert_eq!(cell.hidden_lines, 0);

        let lines = lines_to_strings(&cell.display_lines(80, style::Theme::Dark));
        assert_eq!(lines.len(), 1 + 1 + 2, "war: {lines:?}");
        assert!(lines[2].contains(". (cargo-workspace)"), "war: {lines:?}");
        assert!(lines[3].contains("a (cargo-crate)"), "war: {lines:?}");

        cell.set_expanded(true);
        let expanded = lines_to_strings(&cell.display_lines(80, style::Theme::Dark));
        assert!(
            expanded.iter().any(|l| l.trim() == "\"members\": ["),
            "war: {expanded:?}"
        );
        Ok(())
    }

    /// Mehr Projekte als das Budget: drei Zeilen, der Rest im Hinweis.
    #[test]
    fn test_tool_cell_explore_projects_many_projects_hint_counts_rest() -> TestResult {
        let call = make_tool_call("explore.projects", harw_tools::serde_json::json!({}));
        let mut cell = ToolCell::started(&call);
        let projects: Vec<_> = (0..10)
            .map(|i| harw_tools::serde_json::json!({ "root": format!("p{i}"), "kind": "git" }))
            .collect();
        let result = harw_protocol::items::ToolCallResult::success(harw_tools::serde_json::json!({
            "projects": projects,
            "total": 10,
            "truncated": false,
        }));
        cell.complete(&result, 1);

        let lines = lines_to_strings(&cell.display_lines(80, style::Theme::Dark));
        assert_eq!(lines.len(), 1 + 1 + 3 + 1, "war: {lines:?}");
        let hint = lines.last().ok_or(TestError::Missing("letzte Zeile"))?;
        assert!(
            hint.contains("+7 Zeilen (ctrl+o zum Ausklappen)"),
            "war: {hint}"
        );
        Ok(())
    }

    /// `explore.find` und `explore.relations`: je Eintrag eine Zeile.
    #[test]
    fn test_tool_cell_explore_find_and_relations_preview() {
        let call = make_tool_call(
            "explore.find",
            harw_tools::serde_json::json!({ "query": "lib" }),
        );
        let mut cell = ToolCell::started(&call);
        let result = harw_protocol::items::ToolCallResult::success(harw_tools::serde_json::json!({
            "count": 1,
            "matches": [{ "path": "src/lib.rs", "kind": "rust", "size": 10 }],
            "truncated": false,
        }));
        cell.complete(&result, 1);
        assert_eq!(cell.summary.as_deref(), Some("1 Treffer"));
        assert_eq!(cell.preview, vec!["src/lib.rs (rust)"]);

        let call = make_tool_call("explore.relations", harw_tools::serde_json::json!({}));
        let mut cell = ToolCell::started(&call);
        let result = harw_protocol::items::ToolCallResult::success(harw_tools::serde_json::json!({
            "relations": [{ "from": ".", "to": "a", "kind": "member", "label": null }],
            "total": 1,
            "truncated": true,
        }));
        cell.complete(&result, 1);
        assert_eq!(cell.summary.as_deref(), Some("1 Beziehungen (gekürzt)"));
        assert_eq!(cell.preview, vec![". → a (member)"]);
    }

    /// `ToolGroupCell::accepts` grenzt lesende `fs.*`-Aufrufe von
    /// Seiteneffekt-Aufrufen (`shell.exec`, `fs.write`) und unbekannten
    /// Werkzeugen ab.
    #[test]
    fn test_tool_group_cell_accepts_only_read_only_fs_tools() {
        for tool in ["fs.read", "fs.search", "fs.grep", "fs.list", "fs.glob"] {
            assert!(ToolGroupCell::accepts(tool), "sollte akzeptieren: {tool}");
        }
        for tool in ["shell.exec", "fs.write", "transfer_to_explorer", "custom.x"] {
            assert!(!ToolGroupCell::accepts(tool), "sollte ablehnen: {tool}");
        }
    }

    /// Die eingeklappte Gruppen-Sammelzeile nennt die Zahl je Kategorie.
    #[test]
    fn test_tool_group_cell_collapsed_summary_counts_categories() -> TestResult {
        let mut group = ToolGroupCell::new();

        let read_call = make_tool_call("fs.read", harw_tools::serde_json::json!({ "path": "a" }));
        let read_cell = Arc::new(Mutex::new(ToolCell::started(&read_call)));
        read_cell.lock().map_err(ctx("Mutex vergiftet"))?.complete(
            &harw_protocol::items::ToolCallResult::success(
                harw_tools::serde_json::json!({ "content": "x" }),
            ),
            1,
        );
        group.push(Arc::clone(&read_cell));

        let read_call_2 = make_tool_call("fs.read", harw_tools::serde_json::json!({ "path": "b" }));
        let read_cell_2 = Arc::new(Mutex::new(ToolCell::started(&read_call_2)));
        read_cell_2
            .lock()
            .map_err(ctx("Mutex vergiftet"))?
            .complete(
                &harw_protocol::items::ToolCallResult::success(
                    harw_tools::serde_json::json!({ "content": "y" }),
                ),
                1,
            );
        group.push(Arc::clone(&read_cell_2));

        let search_call = make_tool_call(
            "fs.search",
            harw_tools::serde_json::json!({ "pattern": "TODO" }),
        );
        let search_cell = Arc::new(Mutex::new(ToolCell::started(&search_call)));
        search_cell
            .lock()
            .map_err(ctx("Mutex vergiftet"))?
            .complete(
                &harw_protocol::items::ToolCallResult::success(
                    harw_tools::serde_json::json!({ "matches": [] }),
                ),
                1,
            );
        group.push(search_cell);

        let lines = lines_to_strings(&group.display_lines(80, style::Theme::Dark));
        assert!(
            lines[0].contains("2 Dateien gelesen") && lines[0].contains("1 Muster gesucht"),
            "war: {lines:?}"
        );

        // Ausklappen läuft in Produktionscode über den direkten Feldzugriff
        // (`app.rs::ToolCellHandle::set_expanded`), nicht über eine eigene
        // `toggle_expanded`-Methode dieser Zelle — dieselbe Zuweisung hier.
        group.expanded = true;
        let expanded = lines_to_strings(&group.display_lines(80, style::Theme::Dark)).join("\n");
        assert!(expanded.contains("Read(a)"), "war: {expanded:?}");
        assert!(expanded.contains("Read(b)"), "war: {expanded:?}");
        assert!(
            expanded.contains("Search(\"TODO\" in .)"),
            "war: {expanded:?}"
        );
        Ok(())
    }

    /// Ist eine der gruppierten Zellen fehlgeschlagen, färbt sich der
    /// Statuspunkt rot und die Sammelzeile nennt die Fehlerzahl.
    #[test]
    fn test_tool_group_cell_reports_failure_count() -> TestResult {
        let mut group = ToolGroupCell::new();
        let ok_call = make_tool_call("fs.read", harw_tools::serde_json::json!({ "path": "a" }));
        let ok_cell = Arc::new(Mutex::new(ToolCell::started(&ok_call)));
        ok_cell.lock().map_err(ctx("Mutex vergiftet"))?.complete(
            &harw_protocol::items::ToolCallResult::success(
                harw_tools::serde_json::json!({ "content": "x" }),
            ),
            1,
        );
        group.push(ok_cell);

        let bad_call = make_tool_call("fs.read", harw_tools::serde_json::json!({ "path": "b" }));
        let bad_cell = Arc::new(Mutex::new(ToolCell::started(&bad_call)));
        bad_cell.lock().map_err(ctx("Mutex vergiftet"))?.complete(
            &harw_protocol::items::ToolCallResult::error("nicht gefunden"),
            1,
        );
        group.push(bad_cell);

        let joined = lines_to_strings(&group.display_lines(80, style::Theme::Dark)).join("\n");
        assert!(joined.contains("1 fehlgeschlagen"), "war: {joined:?}");
        Ok(())
    }

    /// Runde 5, Teil M: ein Turn-Ende nennt seinen echten Grund; nur der
    /// Resume-Pfad sagt „Resume-Abbruch".
    #[test]
    fn test_incomplete_label_names_the_real_reason() {
        let call = make_tool_call(
            "transfer_to_root-orchestrator",
            harw_tools::serde_json::json!({ "task": "Umsetzen" }),
        );
        let mut cell = ToolCell::started(&call);
        cell.mark_incomplete_with("unvollständig (abgebrochen)");
        assert_eq!(cell.state, ToolState::Failed);
        assert_eq!(cell.summary.as_deref(), Some("unvollständig (abgebrochen)"));

        let mut resumed = ToolCell::started(&call);
        resumed.mark_incomplete();
        assert_eq!(
            resumed.summary.as_deref(),
            Some("unvollständig (Resume-Abbruch)")
        );
    }

    /// Runde 5, Teil M: der Endbericht eines Kind-Agenten erscheint als
    /// echter Grund in der Werkzeugzelle.
    #[test]
    fn test_child_end_report_shows_the_real_reason() {
        let call = make_tool_call(
            "transfer_to_root-orchestrator",
            harw_tools::serde_json::json!({ "task": "Umsetzen" }),
        );
        let mut cell = ToolCell::started(&call);
        cell.complete(
            &harw_protocol::items::ToolCallResult::error(
                "[child_end status=timeout handoff=yes] Zeitbudget 15 min erreicht\nKind-Agent …",
            ),
            1_224_814,
        );
        assert_eq!(cell.state, ToolState::Failed);
        assert_eq!(
            cell.summary.as_deref(),
            Some("abgebrochen: Zeitbudget 15 min erreicht · Übergabe verfügbar")
        );
        // Ein gewöhnlicher Fehler bleibt ohne Zusammenfassung.
        let mut plain = ToolCell::started(&call);
        plain.complete(&harw_protocol::items::ToolCallResult::error("kaputt"), 1);
        assert_eq!(plain.summary, None);
    }
}
