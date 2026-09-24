//! Plan-Modus in der TUI (Runde 5, Teil F): Freigabefenster für `plan.exit`,
//! Vorschlagsfenster für `plan.enter`, Fragekanal-Zustand und `/plan …`.
//!
//! # Verantwortung
//! - [`PlanUi`]: hält den Plan-Fragekanal (`harw_tool_plan::PlanUiReceiver`),
//!   den geteilten [`PlanSession`]-Zustand der Montage und das gerade offene
//!   Fenster (Plan-Freigabe, Plan-Vorschlag oder `ask_user`,
//!   [`crate::ask_user_dialog`]). Weitere Fragen warten in einer Schlange.
//! - [`PlanExitDialog`]: zeigt den Plan als gerendertes Markdown mit Scrollen
//!   (über dem Verlauf) und drei Optionen (anstelle des Composers):
//!   1. Ja, umsetzen im Auto-Modus → `work` + Freigabe `auto`.
//!   2. Ja, umsetzen und Änderungen einzeln freigeben → `work` + `ask`.
//!   3. Nein, weiter planen — mit Freitext-Rückmeldung an den Agenten.
//! - [`PlanEnterDialog`]: der Agent schlägt den Plan-Modus vor; nur „Ja“
//!   wechselt.
//! - Runde 5, Teil P: dasselbe Freigabefenster in der Variante „Plan
//!   bestätigen“ ([`PlanExitDialog::new_confirm`]) für die `plan`-Operation:
//!   der gerenderte Plan-Graph über dem Verlauf, zwei Optionen
//!   (1 = bestätigen, 2 = ablehnen mit Rückmeldung). Ein „bearbeiten“ gibt
//!   es dort nicht — ein Plan-Graph ist kein Markdown-Dokument.
//! - [`PlanCommand`]: `/plan` (Plan-Modus an), `/plan show|edit|list|open`.
//! - Statuszeilen-Marke und Composer-Hinweis des Plan-Modus.
//!
//! Die Anwendung einer Entscheidung auf `ChatApp` (Freigabemodus,
//! Interaktionsmodus, Sperre, Folge-Nachricht) liegt in
//! `crate::app::plan_mode` — hier gibt es nur reine Zustände und Widgets.
//!
//! # Sicherheitsregeln
//! - **Moduswechsel nur nach Bestätigung:** ausschließlich die Optionen 1/2
//!   bzw. „Ja“ liefern eine wechselnde [`PlanDialogAction`]. Esc, Ctrl+C,
//!   Turn-Ende und Drop antworten nie zustimmend.
//! - **Scharf-Verzögerung** [`PLAN_ARMING_DELAY`]: vorher zählen nur
//!   Esc/Ctrl+C (und Scrollen).
//! - Planinhalt ist Modelltext: Rendering über [`crate::markdown`] (bereinigt
//!   jeden Baustein), Einzeiler über [`sanitize_inline`].
//!
//! # Nebenläufigkeit
//! Lebt exklusiv in der `ChatApp` (Renderer-Thread).

use std::collections::VecDeque;
use std::fmt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use harw_tool_plan::plan_file::display_path;
use harw_tool_plan::{
    PlanConfirmDecision, PlanConfirmKind, PlanConfirmPrompt, PlanEnterPrompt, PlanExitDecision,
    PlanExitPrompt, PlanSession, PlanUiReceiver, PlanUiRequest,
};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap},
};

use crate::ask_user_dialog::{AskStep, AskUserDialog};
use crate::sanitize::sanitize_inline;
use crate::style::{self, Theme};
use crate::tui_event::TuiEvent;

/// Scharf-Verzögerung der Plan-Fenster.
pub(crate) const PLAN_ARMING_DELAY: Duration = Duration::from_millis(600);

/// Höchstlänge der Rückmeldung (Option 3) in Zeichen.
pub(crate) const MAX_FEEDBACK_CHARS: usize = 4000;

/// Statuszeilen-Marke im Stil von Claude Code.
pub(crate) const PLAN_STATUS_MARKER: &str = "⏸ plan mode on (shift+tab to cycle) ";

/// Titel des Composers im Plan-Modus.
pub(crate) const PLAN_COMPOSER_TITLE: &str = " Plan-Modus – es wird nichts verändert ";

/// Platzhalter des leeren Composers im Plan-Modus.
pub(crate) const PLAN_COMPOSER_PLACEHOLDER: &str =
    "Plan-Modus – es wird nichts verändert · beschreibe die Aufgabe · Shift+Tab wechselt weiter";

// ── Farben, Statuszeile, Composer ────────────────────────────────────────────

/// Eigene Farbe des Plan-Modus (Petrol), abgesetzt von Akzent, Warnung und
/// Shell-Modus.
#[must_use]
pub(crate) fn plan_color(theme: Theme) -> Color {
    if style::is_light(theme) {
        Color::Rgb(0x0B, 0x6E, 0x69)
    } else {
        Color::Rgb(0x5E, 0xD6, 0xC8)
    }
}

/// Fetter Stil in der Plan-Farbe.
#[must_use]
pub(crate) fn plan_style(theme: Theme) -> Style {
    Style::default()
        .fg(plan_color(theme))
        .add_modifier(Modifier::BOLD)
}

/// Die Statuszeilen-Marke „⏸ plan mode on (shift+tab to cycle)“.
#[must_use]
pub(crate) fn plan_status_span(theme: Theme) -> Span<'static> {
    Span::styled(format!(" {PLAN_STATUS_MARKER}"), plan_style(theme))
}

/// Titel, Titelstil und Rahmenstil des Composers im Plan-Modus.
#[must_use]
pub(crate) fn plan_composer_chrome(theme: Theme) -> (&'static str, Style, Style) {
    (
        PLAN_COMPOSER_TITLE,
        plan_style(theme),
        Style::default().fg(plan_color(theme)),
    )
}

/// Die Platzhalterzeile des leeren Composers im Plan-Modus.
#[must_use]
pub(crate) fn plan_placeholder_line(theme: Theme) -> Line<'static> {
    Line::from(vec![
        Span::styled("› ", plan_style(theme)),
        Span::styled(PLAN_COMPOSER_PLACEHOLDER, style::dim_style(theme)),
    ])
}

// ── /plan … ──────────────────────────────────────────────────────────────────

/// Die lokalen `/plan`-Befehle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlanCommand {
    /// `/plan` — Plan-Modus einschalten (wie Shift+Tab bis `plan`).
    Enter,
    /// `/plan show` — aktuellen (oder angehefteten) Plan anzeigen.
    Show,
    /// `/plan edit` — aktuellen Plan im `$VISUAL`/`$EDITOR` öffnen.
    Edit,
    /// `/plan list` — Pläne des Projekts auflisten.
    List,
    /// `/plan open <slug>` — früheren Plan zum Weiterplanen laden.
    Open(String),
    /// Falsche Nutzung; trägt die Meldung.
    Usage(String),
}

impl PlanCommand {
    /// `true`, wenn der Befehl während eines laufenden Turns sofort laufen
    /// darf. `/plan edit` braucht das Terminal (externer Editor) und wartet
    /// bis zum Turn-Ende.
    #[must_use]
    pub(crate) fn busy_safe(&self) -> bool {
        !matches!(self, Self::Edit)
    }
}

/// Nutzungszeile der lokalen `/plan`-Befehle.
pub(crate) const PLAN_USAGE: &str = "/plan [show|edit|list|open <name>]";

/// Liest die Argumente nach `/plan`. `None` = kein lokaler Befehl (die
/// Zeile geht an die `plan`-Operation, z. B. `/plan inspect`).
#[must_use]
pub(crate) fn parse_plan_command(args: &str) -> Option<PlanCommand> {
    let mut words = args.split_whitespace();
    let command = match words.next() {
        None => PlanCommand::Enter,
        Some("show") => PlanCommand::Show,
        Some("edit") => PlanCommand::Edit,
        Some("list") => PlanCommand::List,
        Some("open") => match words.next() {
            Some(slug) => PlanCommand::Open(slug.to_owned()),
            None => PlanCommand::Usage("Nutzung: /plan open <name> — siehe /plan list".to_owned()),
        },
        Some(_) => return None,
    };
    if words.next().is_some() {
        return Some(PlanCommand::Usage(format!("Nutzung: {PLAN_USAGE}")));
    }
    Some(command)
}

/// Was die App nach einem `/plan`-Befehl tun soll.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlanCommandEffect {
    /// In die Stufe `plan` wechseln, dann diese Zeile anzeigen.
    EnterPlanStage(String),
    /// Nur diese Zeilen anzeigen.
    Text(String),
    /// Diese Datei im externen Editor öffnen (nach dem Turn).
    Edit {
        /// Slug des Plans.
        slug: String,
        /// Absoluter Pfad.
        path: PathBuf,
    },
}

/// Der Slug, auf den sich `/plan show|edit` beziehen: der aktuelle Plan der
/// Sitzung, sonst der angeheftete.
fn focused_slug(session: &PlanSession) -> Option<String> {
    session
        .current_slug()
        .or_else(|| session.pinned().get().map(|doc| doc.slug))
}

/// Führt einen `/plan`-Befehl gegen den geteilten Plan-Zustand aus.
///
/// # Arguments
/// - `command` ([`PlanCommand`]): der gelesene Befehl.
/// - `session` (`Option<&PlanSession>`): `None`, wenn die Montage keinen
///   Plan-Zustand hat (dann nur Hinweise).
#[must_use]
pub(crate) fn run_plan_command(
    command: &PlanCommand,
    session: Option<&PlanSession>,
) -> PlanCommandEffect {
    let Some(session) = session else {
        return match command {
            PlanCommand::Enter => PlanCommandEffect::EnterPlanStage(
                "Plan-Modus an — es wird nichts verändert (Shift+Tab wechselt weiter).".to_owned(),
            ),
            PlanCommand::Usage(text) => PlanCommandEffect::Text(text.clone()),
            _ => PlanCommandEffect::Text(
                "/plan: in dieser Sitzung gibt es keine Plan-Dateien (keine Montage).".to_owned(),
            ),
        };
    };
    match command {
        PlanCommand::Enter => PlanCommandEffect::EnterPlanStage(
            "Plan-Modus an — es wird nichts verändert. Der Agent erkundet, fragt nach und legt \
             einen Plan zur Freigabe vor (Shift+Tab wechselt weiter)."
                .to_owned(),
        ),
        PlanCommand::Usage(text) => PlanCommandEffect::Text(text.clone()),
        PlanCommand::Show => {
            let Some(slug) = focused_slug(session) else {
                return PlanCommandEffect::Text(
                    "Noch kein Plan in dieser Sitzung — /plan list zeigt frühere Pläne.".to_owned(),
                );
            };
            match session.dir().read(&slug) {
                Ok(content) => {
                    let pinned = session.pinned().get().is_some_and(|doc| doc.slug == slug);
                    let marker = if pinned { " (angeheftet)" } else { "" };
                    PlanCommandEffect::Text(format!(
                        "── Plan {}{marker} ──\n{}",
                        display_path(&slug),
                        content.trim_end()
                    ))
                }
                Err(error) => PlanCommandEffect::Text(format!("/plan show: {error}")),
            }
        }
        PlanCommand::Edit => {
            let Some(slug) = focused_slug(session) else {
                return PlanCommandEffect::Text(
                    "Kein aktueller Plan — erst /plan open <name> oder den Agenten planen lassen."
                        .to_owned(),
                );
            };
            match session.dir().path_for(&slug) {
                Ok(path) if path.exists() => PlanCommandEffect::Edit { slug, path },
                Ok(_) => PlanCommandEffect::Text(format!(
                    "/plan edit: {} existiert noch nicht",
                    display_path(&slug)
                )),
                Err(error) => PlanCommandEffect::Text(format!("/plan edit: {error}")),
            }
        }
        PlanCommand::List => match session.dir().list() {
            Ok(entries) if entries.is_empty() => PlanCommandEffect::Text(
                "Keine Pläne unter .harw/plans/ — im Plan-Modus schreibt der Agent sie mit \
                 plan.write."
                    .to_owned(),
            ),
            Ok(entries) => {
                let current = session.current_slug();
                let pinned = session.pinned().get().map(|doc| doc.slug);
                let mut lines = vec!["Pläne des Projekts (neueste zuerst):".to_owned()];
                for entry in entries {
                    let mut marks = Vec::new();
                    if current.as_deref() == Some(entry.slug.as_str()) {
                        marks.push("aktuell");
                    }
                    if pinned.as_deref() == Some(entry.slug.as_str()) {
                        marks.push("angeheftet");
                    }
                    let marks = if marks.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", marks.join(", "))
                    };
                    lines.push(format!("  {} · {} Bytes{marks}", entry.slug, entry.bytes));
                }
                lines.push("/plan open <name> lädt einen Plan zum Weiterplanen.".to_owned());
                PlanCommandEffect::Text(lines.join("\n"))
            }
            Err(error) => PlanCommandEffect::Text(format!("/plan list: {error}")),
        },
        PlanCommand::Open(raw) => {
            let slug = match session.dir().resolve(raw) {
                Ok(slug) => slug,
                Err(error) => return PlanCommandEffect::Text(format!("/plan open: {error}")),
            };
            match session.dir().read(&slug) {
                Ok(_) => {
                    session.set_current_slug(Some(slug.clone()));
                    PlanCommandEffect::EnterPlanStage(format!(
                        "Plan {} geöffnet — Plan-Modus an. Beschreibe, was sich ändern soll; \
                         plan.write überschreibt diesen Plan, plan.exit legt ihn erneut vor.",
                        display_path(&slug)
                    ))
                }
                Err(error) => PlanCommandEffect::Text(format!("/plan open: {error}")),
            }
        }
    }
}

// ── Entscheidungen ───────────────────────────────────────────────────────────

/// Was die App nach einer Entscheidung in einem Plan-Fenster anwenden muss.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlanDialogAction {
    /// Plan freigegeben: `work` + Freigabe `auto`.
    ImplementAuto {
        /// Anzeigepfad des Plans.
        display_path: String,
    },
    /// Plan freigegeben: `work` + Freigabe `ask`.
    ImplementAsk {
        /// Anzeigepfad des Plans.
        display_path: String,
    },
    /// Plan abgelehnt; Rückmeldung ging an den Agenten.
    KeepPlanning,
    /// Vorschlag angenommen: in die Stufe `plan` wechseln.
    EnterPlan,
    /// Vorschlag abgelehnt.
    EnterDeclined,
    /// `ask_user` beantwortet; trägt die Zusammenfassung.
    Answered(String),
    /// Runde 5, Teil P: Plan-Graph bestätigt (neu oder Änderung).
    PlanConfirmed {
        /// Bezeichner des Plans.
        plan_id: String,
        /// `true` für eine Änderung, `false` für einen neuen Plan.
        change: bool,
    },
    /// Runde 5, Teil P: Plan-Graph abgelehnt; die Rückmeldung ging an den
    /// Agenten.
    PlanRejected {
        /// Bezeichner des Plans.
        plan_id: String,
    },
    /// Fenster ohne Entscheidung geschlossen.
    Closed,
}

// ── plan.exit ────────────────────────────────────────────────────────────────

/// Die drei Optionen des Freigabefensters.
const EXIT_OPTIONS: [&str; 3] = [
    "Ja, umsetzen im Auto-Modus (work · Freigabe auto)",
    "Ja, umsetzen und Änderungen einzeln freigeben (work · Freigabe ask)",
    "Nein, weiter planen — Rückmeldung: ",
];

/// Runde 5, Teil P: die zwei Optionen der Variante „Plan bestätigen“.
const CONFIRM_OPTIONS: [&str; 2] = [
    "Ja, bestätigen — der Plan wird aktiv und als Goal verfolgt",
    "Nein — Rückmeldung an den Agenten: ",
];

/// Die wartende Frage hinter dem Freigabefenster.
enum DialogPrompt {
    /// `plan.exit` (Plan-Datei).
    Exit(PlanExitPrompt),
    /// Runde 5, Teil P: Plan-Graph der `plan`-Operation bestätigen.
    Confirm(PlanConfirmPrompt),
}

/// Freigabefenster eines Plans (`plan.exit` oder — Teil P — „Plan bestätigen“).
pub(crate) struct PlanExitDialog {
    prompt: Option<DialogPrompt>,
    /// `Some`, wenn das Fenster die Variante „Plan bestätigen“ zeigt.
    confirm: Option<PlanConfirmKind>,
    /// Einzeiler der Bestätigung (was geändert werden soll).
    summary: String,
    display_path: String,
    empty: bool,
    markdown: String,
    scroll: u16,
    cursor: usize,
    feedback: String,
    editing: bool,
    shown_at: Instant,
}

impl fmt::Debug for PlanExitDialog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlanExitDialog")
            .field("display_path", &self.display_path)
            .field("cursor", &self.cursor)
            .finish_non_exhaustive()
    }
}

/// Ergebnis einer Eingabe in einem Plan-Fenster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PlanStep {
    /// Offen bleiben; `true` = neu zeichnen.
    Stay(bool),
    /// Entschieden oder geschlossen.
    Done(PlanDialogAction),
}

impl PlanExitDialog {
    /// Öffnet das Fenster.
    pub(crate) fn new(prompt: PlanExitPrompt, now: Instant) -> Self {
        Self {
            display_path: prompt.display_path().to_owned(),
            empty: prompt.is_empty_plan(),
            markdown: prompt.content().to_owned(),
            prompt: Some(DialogPrompt::Exit(prompt)),
            confirm: None,
            summary: String::new(),
            scroll: 0,
            cursor: 0,
            feedback: String::new(),
            editing: false,
            shown_at: now,
        }
    }

    /// Runde 5, Teil P: öffnet das Fenster in der Variante „Plan bestätigen“.
    pub(crate) fn new_confirm(prompt: PlanConfirmPrompt, now: Instant) -> Self {
        Self {
            display_path: format!("Plan {}", prompt.plan_id()),
            empty: prompt.content().trim().is_empty(),
            markdown: prompt.content().to_owned(),
            confirm: Some(prompt.kind()),
            summary: prompt.summary().to_owned(),
            prompt: Some(DialogPrompt::Confirm(prompt)),
            scroll: 0,
            cursor: 0,
            feedback: String::new(),
            editing: false,
            shown_at: now,
        }
    }

    fn armed(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.shown_at) >= PLAN_ARMING_DELAY
    }

    fn decide(&mut self, decision: PlanExitDecision) -> PlanStep {
        let action = match &decision {
            PlanExitDecision::ImplementAuto => PlanDialogAction::ImplementAuto {
                display_path: self.display_path.clone(),
            },
            PlanExitDecision::ImplementAsk => PlanDialogAction::ImplementAsk {
                display_path: self.display_path.clone(),
            },
            PlanExitDecision::KeepPlanning { .. } => PlanDialogAction::KeepPlanning,
        };
        // Runde 5, Teil P: ohne Move im Match-Guard (die Frage wird beim
        // Beantworten konsumiert).
        let delivered = match self.prompt.take() {
            Some(DialogPrompt::Exit(prompt)) => prompt.decide(decision),
            _ => false,
        };
        if delivered {
            PlanStep::Done(action)
        } else {
            PlanStep::Done(PlanDialogAction::Closed)
        }
    }

    /// Runde 5, Teil P: Entscheidung der Variante „Plan bestätigen“.
    fn decide_confirm(&mut self, decision: PlanConfirmDecision) -> PlanStep {
        let Some(DialogPrompt::Confirm(prompt)) = self.prompt.take() else {
            return PlanStep::Done(PlanDialogAction::Closed);
        };
        let plan_id = prompt.plan_id().to_owned();
        let action = match &decision {
            PlanConfirmDecision::Confirm => PlanDialogAction::PlanConfirmed {
                plan_id,
                change: prompt.kind() == PlanConfirmKind::Change,
            },
            PlanConfirmDecision::Reject { .. } => PlanDialogAction::PlanRejected { plan_id },
        };
        if prompt.decide(decision) {
            PlanStep::Done(action)
        } else {
            PlanStep::Done(PlanDialogAction::Closed)
        }
    }

    /// Die Optionen der gezeigten Variante; die letzte ist immer die
    /// Rückmeldung.
    fn options(&self) -> &'static [&'static str] {
        if self.confirm.is_some() {
            &CONFIRM_OPTIONS
        } else {
            &EXIT_OPTIONS
        }
    }

    fn choose(&mut self, index: usize) -> PlanStep {
        let last = self.options().len().saturating_sub(1);
        self.cursor = index.min(last);
        if self.cursor == last {
            self.editing = true;
            return PlanStep::Stay(true);
        }
        match (self.confirm.is_some(), self.cursor) {
            (true, _) => self.decide_confirm(PlanConfirmDecision::Confirm),
            (false, 0) => self.decide(PlanExitDecision::ImplementAuto),
            (false, _) => self.decide(PlanExitDecision::ImplementAsk),
        }
    }

    /// Sendet die Rückmeldung der letzten Option.
    fn send_feedback(&mut self, feedback: String) -> PlanStep {
        if self.confirm.is_some() {
            self.decide_confirm(PlanConfirmDecision::Reject { feedback })
        } else {
            self.decide(PlanExitDecision::KeepPlanning { feedback })
        }
    }

    fn scroll_by(&mut self, delta: i32) {
        let next = i32::from(self.scroll).saturating_add(delta).max(0);
        self.scroll = u16::try_from(next).unwrap_or(u16::MAX);
    }

    /// Verarbeitet ein Eingabeereignis.
    pub(crate) fn handle_event(&mut self, event: TuiEvent, now: Instant) -> PlanStep {
        let key = match event {
            TuiEvent::Key(key) => key,
            TuiEvent::Paste(text) if self.editing => {
                for c in text.chars() {
                    if self.feedback.chars().count() < MAX_FEEDBACK_CHARS {
                        self.feedback
                            .push(if c.is_control() && c != '\n' { ' ' } else { c });
                    }
                }
                return PlanStep::Stay(true);
            }
            TuiEvent::Resize(..) | TuiEvent::Draw => return PlanStep::Stay(true),
            TuiEvent::Paste(_) | TuiEvent::Mouse(_) => return PlanStep::Stay(false),
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && matches!(key.code, KeyCode::Char('c' | 'C')) {
            return self.close();
        }
        match key.code {
            KeyCode::PageDown => {
                self.scroll_by(10);
                return PlanStep::Stay(true);
            }
            KeyCode::PageUp => {
                self.scroll_by(-10);
                return PlanStep::Stay(true);
            }
            KeyCode::Char('d') if ctrl => {
                self.scroll_by(10);
                return PlanStep::Stay(true);
            }
            KeyCode::Char('u') if ctrl => {
                self.scroll_by(-10);
                return PlanStep::Stay(true);
            }
            _ => {}
        }
        if self.editing {
            return self.handle_feedback_key(key);
        }
        if key.code == KeyCode::Esc {
            return self.close();
        }
        if !self.armed(now) {
            return PlanStep::Stay(false);
        }
        let count = self.options().len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.cursor = (self.cursor + count - 1) % count;
                PlanStep::Stay(true)
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                self.cursor = (self.cursor + 1) % count;
                PlanStep::Stay(true)
            }
            KeyCode::Char('1') => self.choose(0),
            KeyCode::Char('2') => self.choose(1),
            KeyCode::Char('3') if count > 2 => self.choose(2),
            KeyCode::Enter => self.choose(self.cursor),
            _ => PlanStep::Stay(false),
        }
    }

    fn handle_feedback_key(&mut self, key: KeyEvent) -> PlanStep {
        match key.code {
            KeyCode::Esc => {
                self.editing = false;
                PlanStep::Stay(true)
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                if self.feedback.chars().count() < MAX_FEEDBACK_CHARS {
                    self.feedback.push('\n');
                }
                PlanStep::Stay(true)
            }
            KeyCode::Enter => {
                let feedback = std::mem::take(&mut self.feedback);
                self.send_feedback(feedback)
            }
            KeyCode::Backspace => {
                self.feedback.pop();
                PlanStep::Stay(true)
            }
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL) && !c.is_control() =>
            {
                if self.feedback.chars().count() < MAX_FEEDBACK_CHARS {
                    self.feedback.push(c);
                }
                PlanStep::Stay(true)
            }
            _ => PlanStep::Stay(false),
        }
    }

    /// Schließt ohne Entscheidung (Drop der Frage).
    fn close(&mut self) -> PlanStep {
        self.prompt = None;
        PlanStep::Done(PlanDialogAction::Closed)
    }

    fn option_lines(&self, theme: Theme, now: Instant) -> Vec<Line<'static>> {
        let dim = style::dim_style(theme);
        let accent = plan_style(theme);
        let title = match self.confirm {
            Some(PlanConfirmKind::NewPlan) => format!(
                "Plan bestätigen? · {} · {}",
                sanitize_inline(&self.display_path),
                sanitize_inline(&self.summary)
            ),
            Some(PlanConfirmKind::Change) => format!(
                "Änderung übernehmen? · {} · {}",
                sanitize_inline(&self.display_path),
                sanitize_inline(&self.summary)
            ),
            None => format!("Plan umsetzen? · {}", sanitize_inline(&self.display_path)),
        };
        let mut lines = vec![Line::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        )];
        if self.empty {
            lines.push(Line::styled(
                "⚠ Der Plan ist leer — eine Freigabe setzt nichts Konkretes um.",
                style::warning_style(theme),
            ));
        }
        let options = self.options();
        let last = options.len().saturating_sub(1);
        for (index, label) in options.iter().enumerate() {
            let selected = index == self.cursor;
            let pointer = if selected { "›" } else { " " };
            let mut spans = vec![Span::styled(
                format!("{pointer} {}. {label}", index + 1),
                if selected { accent } else { Style::default() },
            )];
            if index == last {
                let text = if self.editing {
                    format!("{}▏", sanitize_inline(&self.feedback))
                } else if self.feedback.is_empty() {
                    "(Enter, dann tippen)".to_owned()
                } else {
                    sanitize_inline(&self.feedback)
                };
                spans.push(Span::styled(
                    text,
                    if self.editing { Style::default() } else { dim },
                ));
            }
            lines.push(Line::from(spans));
        }
        let footer = if !self.armed(now) {
            "Fenster wird gleich scharf … · Bild↑/↓ scrollt · Esc schließt"
        } else if self.editing {
            "Rückmeldung tippen · Enter senden · Shift+Enter neue Zeile · Esc zurück"
        } else if self.confirm.is_some() {
            "↑↓ wählen · 1/2 direkt · Enter bestätigen · Bild↑/↓ scrollt den Plan · Esc schließt"
        } else {
            "↑↓ wählen · 1–3 direkt · Enter bestätigen · Bild↑/↓ scrollt den Plan · Esc schließt"
        };
        lines.push(Line::styled(footer, dim));
        lines
    }

    fn desired_height(&self, width: u16, theme: Theme) -> u16 {
        let inner = width.saturating_sub(2).max(1);
        let rows = Paragraph::new(self.option_lines(theme, Instant::now()))
            .wrap(Wrap { trim: false })
            .line_count(inner);
        u16::try_from(rows)
            .unwrap_or(u16::MAX)
            .saturating_add(2)
            .clamp(6, 14)
    }

    fn render(&self, input: Rect, history: Rect, buf: &mut Buffer, theme: Theme) {
        if history.height > 2 && history.width > 2 {
            Clear.render(history, buf);
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(plan_color(theme)))
                .title(Span::styled(
                    format!(
                        " {} · {} ",
                        if self.confirm.is_some() {
                            "Plan zur Bestätigung"
                        } else {
                            "Plan zur Freigabe"
                        },
                        sanitize_inline(&self.display_path)
                    ),
                    plan_style(theme),
                ));
            Paragraph::new(crate::markdown::render_markdown(&self.markdown, theme))
                .wrap(Wrap { trim: false })
                .scroll((self.scroll, 0))
                .block(block)
                .render(history, buf);
        }
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(plan_color(theme)))
            .title(Span::styled(
                if self.confirm.is_some() {
                    " Plan bestätigen · plan "
                } else {
                    " Plan-Freigabe · plan.exit "
                },
                plan_style(theme),
            ));
        Paragraph::new(self.option_lines(theme, Instant::now()))
            .wrap(Wrap { trim: false })
            .block(block)
            .render(input, buf);
    }
}

// ── plan.enter ───────────────────────────────────────────────────────────────

/// Vorschlagsfenster: der Agent möchte in den Plan-Modus wechseln.
pub(crate) struct PlanEnterDialog {
    prompt: Option<PlanEnterPrompt>,
    reason: String,
    cursor: usize,
    shown_at: Instant,
}

impl fmt::Debug for PlanEnterDialog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlanEnterDialog")
            .field("cursor", &self.cursor)
            .finish_non_exhaustive()
    }
}

impl PlanEnterDialog {
    /// Öffnet das Fenster; der Cursor steht auf „Nein“ (sicherer Default).
    pub(crate) fn new(prompt: PlanEnterPrompt, now: Instant) -> Self {
        Self {
            reason: prompt.reason().to_owned(),
            prompt: Some(prompt),
            cursor: 1,
            shown_at: now,
        }
    }

    fn answer(&mut self, accept: bool) -> PlanStep {
        let delivered = self
            .prompt
            .take()
            .is_some_and(|prompt| prompt.answer(accept));
        PlanStep::Done(match (delivered, accept) {
            (true, true) => PlanDialogAction::EnterPlan,
            (true, false) => PlanDialogAction::EnterDeclined,
            (false, _) => PlanDialogAction::Closed,
        })
    }

    /// Verarbeitet ein Eingabeereignis.
    pub(crate) fn handle_event(&mut self, event: TuiEvent, now: Instant) -> PlanStep {
        let key = match event {
            TuiEvent::Key(key) => key,
            TuiEvent::Resize(..) | TuiEvent::Draw => return PlanStep::Stay(true),
            TuiEvent::Paste(_) | TuiEvent::Mouse(_) => return PlanStep::Stay(false),
        };
        let ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'C'));
        if ctrl_c || key.code == KeyCode::Esc {
            return self.answer(false);
        }
        if now.saturating_duration_since(self.shown_at) < PLAN_ARMING_DELAY {
            return PlanStep::Stay(false);
        }
        match key.code {
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down | KeyCode::Tab => {
                self.cursor = 1 - self.cursor.min(1);
                PlanStep::Stay(true)
            }
            KeyCode::Char('1' | 'y' | 'j') => self.answer(true),
            KeyCode::Char('2' | 'n') => self.answer(false),
            KeyCode::Enter => self.answer(self.cursor == 0),
            _ => PlanStep::Stay(false),
        }
    }

    fn lines(&self, theme: Theme) -> Vec<Line<'static>> {
        let dim = style::dim_style(theme);
        let accent = plan_style(theme);
        let option = |index: usize, label: &str| {
            let selected = self.cursor == index;
            Line::styled(
                format!(
                    "{} {}. {label}",
                    if selected { "›" } else { " " },
                    index + 1
                ),
                if selected { accent } else { Style::default() },
            )
        };
        vec![
            Line::styled(
                "Der Agent schlägt vor, zuerst zu planen (Plan-Modus: nichts wird verändert).",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Line::from(vec![
                Span::styled("Grund: ", dim),
                Span::raw(sanitize_inline(&self.reason)),
            ]),
            option(0, "Ja, in den Plan-Modus wechseln"),
            option(1, "Nein, weiter wie bisher"),
            Line::styled("←→ wählen · 1/2 · Enter bestätigen · Esc = Nein", dim),
        ]
    }

    fn render(&self, area: Rect, buf: &mut Buffer, theme: Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(plan_color(theme)))
            .title(Span::styled(
                " Plan-Modus vorgeschlagen · plan.enter ",
                plan_style(theme),
            ));
        Paragraph::new(self.lines(theme))
            .wrap(Wrap { trim: false })
            .block(block)
            .render(area, buf);
    }
}

impl Drop for PlanEnterDialog {
    /// Ohne Antwort geschlossen = abgelehnt (Drop der Frage).
    fn drop(&mut self) {
        self.prompt = None;
    }
}

// ── PlanUi ───────────────────────────────────────────────────────────────────

/// Das gerade offene Plan-Fenster.
#[derive(Debug)]
enum OpenDialog {
    /// Geboxt: deutlich größer als die übrigen Varianten.
    Exit(Box<PlanExitDialog>),
    Enter(PlanEnterDialog),
    Ask(AskUserDialog),
}

/// Ergebnis von [`PlanUi::handle_event`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PlanUiOutcome {
    /// Neu zeichnen.
    pub(crate) redraw: bool,
    /// Anzuwendende Entscheidung, falls ein Fenster sich geschlossen hat.
    pub(crate) action: Option<PlanDialogAction>,
}

/// Zustand der Plan-Fenster in der `ChatApp`.
#[derive(Default)]
pub(crate) struct PlanUi {
    receiver: Option<PlanUiReceiver>,
    session: Option<PlanSession>,
    open: Option<OpenDialog>,
    queue: VecDeque<PlanUiRequest>,
    pending_editor: Option<(String, PathBuf)>,
}

impl fmt::Debug for PlanUi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PlanUi")
            .field("listening", &self.receiver.is_some())
            .field("session", &self.session.is_some())
            .field("open", &self.open.is_some())
            .field("queued", &self.queue.len())
            .finish_non_exhaustive()
    }
}

impl PlanUi {
    /// Baut den Zustand.
    ///
    /// # Arguments
    /// - `receiver`: Plan-Fragekanal (nur TUI-Montage).
    /// - `session`: geteilter Plan-Zustand der Montage.
    pub(crate) fn new(receiver: Option<PlanUiReceiver>, session: Option<PlanSession>) -> Self {
        Self {
            receiver,
            session,
            ..Self::default()
        }
    }

    /// Der geteilte Plan-Zustand, falls vorhanden.
    pub(crate) fn session(&self) -> Option<&PlanSession> {
        self.session.as_ref()
    }

    /// `true`, solange der Fragekanal gepollt werden soll.
    pub(crate) fn is_listening(&self) -> bool {
        self.receiver.is_some()
    }

    /// Wartet auf die nächste Frage; ohne Kanal nie fertig.
    pub(crate) async fn recv(&mut self) -> Option<PlanUiRequest> {
        match self.receiver.as_mut() {
            Some(receiver) => receiver.recv().await,
            None => std::future::pending().await,
        }
    }

    /// Der Kanal ist zu; nicht mehr pollen.
    pub(crate) fn channel_closed(&mut self) {
        self.receiver = None;
    }

    /// `true`, solange ein Fenster offen ist (dann gehört ihm jede Eingabe).
    pub(crate) fn is_open(&self) -> bool {
        self.open.is_some()
    }

    /// Nimmt eine Frage an: öffnet sie oder reiht sie ein.
    pub(crate) fn accept(&mut self, request: PlanUiRequest, now: Instant) {
        if self.open.is_some() {
            self.queue.push_back(request);
        } else {
            self.open = Some(Self::dialog_for(request, now));
        }
    }

    fn dialog_for(request: PlanUiRequest, now: Instant) -> OpenDialog {
        match request {
            PlanUiRequest::AskUser(prompt) => OpenDialog::Ask(AskUserDialog::new(prompt, now)),
            PlanUiRequest::ExitPlan(prompt) => {
                OpenDialog::Exit(Box::new(PlanExitDialog::new(prompt, now)))
            }
            PlanUiRequest::EnterPlan(prompt) => {
                OpenDialog::Enter(PlanEnterDialog::new(prompt, now))
            }
            // Runde 5, Teil P: dasselbe Freigabefenster, Variante „bestätigen“.
            PlanUiRequest::ConfirmPlan(prompt) => {
                OpenDialog::Exit(Box::new(PlanExitDialog::new_confirm(prompt, now)))
            }
        }
    }

    fn open_next(&mut self, now: Instant) {
        self.open = self
            .queue
            .pop_front()
            .map(|request| Self::dialog_for(request, now));
    }

    /// Leitet ein Eingabeereignis an das offene Fenster.
    pub(crate) fn handle_event(&mut self, event: TuiEvent, now: Instant) -> PlanUiOutcome {
        let step = match self.open.as_mut() {
            None => return PlanUiOutcome::default(),
            Some(OpenDialog::Exit(dialog)) => dialog.handle_event(event, now),
            Some(OpenDialog::Enter(dialog)) => dialog.handle_event(event, now),
            Some(OpenDialog::Ask(dialog)) => match dialog.handle_event(event, now) {
                AskStep::Stay(redraw) => PlanStep::Stay(redraw),
                AskStep::Submitted(summary) => PlanStep::Done(PlanDialogAction::Answered(summary)),
                AskStep::Cancelled => PlanStep::Done(PlanDialogAction::Closed),
            },
        };
        match step {
            PlanStep::Stay(redraw) => PlanUiOutcome {
                redraw,
                action: None,
            },
            PlanStep::Done(action) => {
                self.open = None;
                self.open_next(now);
                PlanUiOutcome {
                    redraw: true,
                    action: Some(action),
                }
            }
        }
    }

    /// Schließt jedes offene und wartende Fenster ohne Entscheidung
    /// (Turn-Ende, Leerlauf).
    ///
    /// # Returns
    /// `true`, wenn etwas offen war.
    pub(crate) fn close_all(&mut self) -> bool {
        let had = self.open.is_some() || !self.queue.is_empty();
        self.open = None;
        self.queue.clear();
        had
    }

    /// Merkt eine Datei für den externen Editor vor (nach dem Turn).
    pub(crate) fn set_pending_editor(&mut self, slug: String, path: PathBuf) {
        self.pending_editor = Some((slug, path));
    }

    /// Holt die vorgemerkte Datei ab.
    pub(crate) fn take_pending_editor(&mut self) -> Option<(String, PathBuf)> {
        self.pending_editor.take()
    }

    /// Benötigte Höhe des Composer-Ersatzes oder `None` ohne offenes Fenster.
    pub(crate) fn desired_height(&self, width: u16, theme: Theme) -> Option<u16> {
        match self.open.as_ref()? {
            OpenDialog::Exit(dialog) => Some(dialog.desired_height(width, theme)),
            OpenDialog::Enter(_) => Some(7),
            OpenDialog::Ask(dialog) => Some(dialog.desired_height(width, theme)),
        }
    }

    /// Zeichnet das offene Fenster: anstelle des Composers (`input`) und —
    /// nur bei der Plan-Freigabe — den Plan über dem Verlauf (`history`).
    ///
    /// # Returns
    /// `true`, wenn ein Fenster gezeichnet wurde.
    pub(crate) fn render(
        &self,
        input: Rect,
        history: Rect,
        buf: &mut Buffer,
        theme: Theme,
    ) -> bool {
        match self.open.as_ref() {
            None => false,
            Some(OpenDialog::Exit(dialog)) => {
                dialog.render(input, history, buf, theme);
                true
            }
            Some(OpenDialog::Enter(dialog)) => {
                dialog.render(input, buf, theme);
                true
            }
            Some(OpenDialog::Ask(dialog)) => {
                dialog.render(input, buf, theme);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use harw_tool_plan::{PlanDir, PlanExitDecision};

    fn key(code: KeyCode) -> TuiEvent {
        TuiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn armed(shown: Instant) -> Instant {
        shown + PLAN_ARMING_DELAY + Duration::from_millis(1)
    }

    fn exit_prompt(
        content: &str,
    ) -> (
        PlanExitPrompt,
        tokio::sync::oneshot::Receiver<PlanExitDecision>,
    ) {
        PlanExitPrompt::new(
            "auth".to_owned(),
            PathBuf::from("/p/.harw/plans/auth.md"),
            content.to_owned(),
        )
    }

    #[test]
    fn parse_plan_command_covers_the_local_subcommands_only() {
        assert_eq!(parse_plan_command(""), Some(PlanCommand::Enter));
        assert_eq!(parse_plan_command("show"), Some(PlanCommand::Show));
        assert_eq!(parse_plan_command("edit"), Some(PlanCommand::Edit));
        assert_eq!(parse_plan_command("list"), Some(PlanCommand::List));
        assert_eq!(
            parse_plan_command("open auth"),
            Some(PlanCommand::Open("auth".to_owned()))
        );
        assert!(matches!(
            parse_plan_command("open"),
            Some(PlanCommand::Usage(_))
        ));
        assert!(matches!(
            parse_plan_command("show extra"),
            Some(PlanCommand::Usage(_))
        ));
        // Alles andere gehört der `plan`-Operation.
        assert_eq!(parse_plan_command("inspect"), None);
        assert_eq!(parse_plan_command("add t1 coding x"), None);
        assert!(!PlanCommand::Edit.busy_safe());
        assert!(PlanCommand::Show.busy_safe());
    }

    #[tokio::test]
    async fn option_one_approves_auto_only_after_arming() -> TestResult {
        let (prompt, decision) = exit_prompt("# Plan");
        let shown = Instant::now();
        let mut dialog = PlanExitDialog::new(prompt, shown);
        assert_eq!(
            dialog.handle_event(key(KeyCode::Char('1')), shown),
            PlanStep::Stay(false),
            "vor der Scharf-Verzögerung wechselt nichts"
        );
        let step = dialog.handle_event(key(KeyCode::Char('1')), armed(shown));
        assert_eq!(
            step,
            PlanStep::Done(PlanDialogAction::ImplementAuto {
                display_path: ".harw/plans/auth.md".to_owned()
            })
        );
        let received = decision
            .await
            .map_err(|_| TestError::Missing("Entscheidung"))?;
        assert_eq!(received, PlanExitDecision::ImplementAuto);
        Ok(())
    }

    #[tokio::test]
    async fn option_three_sends_the_typed_feedback() -> TestResult {
        let (prompt, decision) = exit_prompt("# Plan");
        let shown = Instant::now();
        let now = armed(shown);
        let mut dialog = PlanExitDialog::new(prompt, shown);
        dialog.handle_event(key(KeyCode::Down), now);
        dialog.handle_event(key(KeyCode::Down), now);
        assert_eq!(
            dialog.handle_event(key(KeyCode::Enter), now),
            PlanStep::Stay(true)
        );
        for c in "Tests zuerst".chars() {
            dialog.handle_event(key(KeyCode::Char(c)), now);
        }
        assert_eq!(
            dialog.handle_event(key(KeyCode::Enter), now),
            PlanStep::Done(PlanDialogAction::KeepPlanning)
        );
        let received = decision
            .await
            .map_err(|_| TestError::Missing("Entscheidung"))?;
        assert_eq!(
            received,
            PlanExitDecision::KeepPlanning {
                feedback: "Tests zuerst".to_owned()
            }
        );
        Ok(())
    }

    // ── Runde 5, Teil P: Variante „Plan bestätigen“ ─────────────────────────

    fn confirm_prompt(
        kind: PlanConfirmKind,
    ) -> (
        PlanConfirmPrompt,
        tokio::sync::oneshot::Receiver<PlanConfirmDecision>,
    ) {
        PlanConfirmPrompt::new(
            "crypt-guard-hardening-v1".to_owned(),
            kind,
            "Neuer Plan mit 2 Schritten".to_owned(),
            "# Plan crypt-guard-hardening-v1\n1. key-perms".to_owned(),
        )
    }

    /// „create zeigt den Plan und wartet auf Bestätigung“ (TUI-Seite): der
    /// Plan steht im Fenster, erst Option 1 nach der Scharf-Verzögerung
    /// bestätigt.
    #[tokio::test]
    async fn confirm_variant_shows_the_plan_and_confirms_with_option_one() -> TestResult {
        let (prompt, decision) = confirm_prompt(PlanConfirmKind::NewPlan);
        let shown = Instant::now();
        let mut ui = PlanUi::default();
        ui.accept(PlanUiRequest::ConfirmPlan(prompt), shown);
        assert!(ui.is_open());
        let Some(OpenDialog::Exit(dialog)) = ui.open.as_ref() else {
            return Err(TestError::Missing(
                "Freigabefenster in der Variante bestätigen",
            ));
        };
        assert!(
            dialog.markdown.contains("key-perms"),
            "der Plan wird gezeigt"
        );
        let header: String = dialog
            .option_lines(Theme::Dark, shown)
            .iter()
            .flat_map(|line| line.spans.iter().map(|span| span.content.to_string()))
            .collect();
        assert!(header.contains("Plan bestätigen?"), "{header}");
        assert!(header.contains("2. Nein"), "{header}");
        assert!(!header.contains("3."), "keine dritte Option: {header}");

        assert_eq!(
            ui.handle_event(key(KeyCode::Char('1')), shown).action,
            None,
            "vor der Scharf-Verzögerung wird nichts bestätigt"
        );
        let outcome = ui.handle_event(key(KeyCode::Char('1')), armed(shown));
        assert_eq!(
            outcome.action,
            Some(PlanDialogAction::PlanConfirmed {
                plan_id: "crypt-guard-hardening-v1".to_owned(),
                change: false,
            })
        );
        let received = decision
            .await
            .map_err(|_| TestError::Missing("Entscheidung"))?;
        assert_eq!(received, PlanConfirmDecision::Confirm);
        Ok(())
    }

    /// „Ablehnung gibt die Rückmeldung weiter“.
    #[tokio::test]
    async fn confirm_variant_rejection_carries_the_feedback() -> TestResult {
        let (prompt, decision) = confirm_prompt(PlanConfirmKind::Change);
        let shown = Instant::now();
        let now = armed(shown);
        let mut dialog = PlanExitDialog::new_confirm(prompt, shown);
        assert_eq!(
            dialog.handle_event(key(KeyCode::Char('2')), now),
            PlanStep::Stay(true),
            "Option 2 öffnet die Rückmeldung"
        );
        for c in "erst Tests".chars() {
            dialog.handle_event(key(KeyCode::Char(c)), now);
        }
        assert_eq!(
            dialog.handle_event(key(KeyCode::Enter), now),
            PlanStep::Done(PlanDialogAction::PlanRejected {
                plan_id: "crypt-guard-hardening-v1".to_owned()
            })
        );
        let received = decision
            .await
            .map_err(|_| TestError::Missing("Entscheidung"))?;
        assert_eq!(
            received,
            PlanConfirmDecision::Reject {
                feedback: "erst Tests".to_owned()
            }
        );
        Ok(())
    }

    #[tokio::test]
    async fn confirm_variant_esc_is_never_consent() -> TestResult {
        let (prompt, decision) = confirm_prompt(PlanConfirmKind::NewPlan);
        let mut dialog = PlanExitDialog::new_confirm(prompt, Instant::now());
        assert_eq!(
            dialog.handle_event(key(KeyCode::Esc), Instant::now()),
            PlanStep::Done(PlanDialogAction::Closed)
        );
        assert!(decision.await.is_err(), "Drop ist keine Zustimmung");
        Ok(())
    }

    #[tokio::test]
    async fn esc_closes_without_any_decision() -> TestResult {
        let (prompt, decision) = exit_prompt("# Plan");
        let mut dialog = PlanExitDialog::new(prompt, Instant::now());
        assert_eq!(
            dialog.handle_event(key(KeyCode::Esc), Instant::now()),
            PlanStep::Done(PlanDialogAction::Closed)
        );
        assert!(decision.await.is_err(), "Esc ist keine Zustimmung");
        Ok(())
    }

    #[tokio::test]
    async fn plan_enter_defaults_to_no_and_esc_declines() -> TestResult {
        let (prompt, answer) = PlanEnterPrompt::new("großer Umbau".to_owned());
        let shown = Instant::now();
        let mut dialog = PlanEnterDialog::new(prompt, shown);
        assert_eq!(
            dialog.handle_event(key(KeyCode::Enter), armed(shown)),
            PlanStep::Done(PlanDialogAction::EnterDeclined),
            "Cursor steht auf Nein"
        );
        assert_eq!(answer.await.ok(), Some(false));

        let (prompt, answer) = PlanEnterPrompt::new("großer Umbau".to_owned());
        let mut dialog = PlanEnterDialog::new(prompt, shown);
        assert_eq!(
            dialog.handle_event(key(KeyCode::Char('1')), armed(shown)),
            PlanStep::Done(PlanDialogAction::EnterPlan)
        );
        assert_eq!(answer.await.ok(), Some(true));
        Ok(())
    }

    #[tokio::test]
    async fn plan_ui_queues_requests_and_closes_all_without_consent() -> TestResult {
        let mut ui = PlanUi::new(None, None);
        let (first, first_rx) = exit_prompt("# A");
        let (second, second_rx) = PlanEnterPrompt::new("b".to_owned());
        let now = Instant::now();
        ui.accept(PlanUiRequest::ExitPlan(first), now);
        ui.accept(PlanUiRequest::EnterPlan(second), now);
        assert!(ui.is_open());
        let outcome = ui.handle_event(key(KeyCode::Esc), now);
        assert_eq!(outcome.action, Some(PlanDialogAction::Closed));
        assert!(ui.is_open(), "die zweite Frage öffnet sich danach");
        assert!(ui.close_all());
        assert!(!ui.is_open());
        assert!(first_rx.await.is_err());
        assert!(second_rx.await.is_err());
        Ok(())
    }

    #[test]
    fn plan_commands_show_list_open_and_edit() -> TestResult {
        let temp = tempfile::tempdir().map_err(|e| TestError::Unexpected(e.to_string()))?;
        let session = PlanSession::new(PlanDir::new(temp.path().join(".harw/plans")), false);
        assert!(matches!(
            run_plan_command(&PlanCommand::Show, Some(&session)),
            PlanCommandEffect::Text(text) if text.contains("Noch kein Plan")
        ));
        session
            .dir()
            .write("auth", "# Auth\n1. Schritt")
            .map_err(|e| TestError::Unexpected(e.to_string()))?;
        assert!(matches!(
            run_plan_command(&PlanCommand::List, Some(&session)),
            PlanCommandEffect::Text(text) if text.contains("auth")
        ));
        assert!(matches!(
            run_plan_command(&PlanCommand::Open("auth".to_owned()), Some(&session)),
            PlanCommandEffect::EnterPlanStage(_)
        ));
        assert_eq!(session.current_slug().as_deref(), Some("auth"));
        assert!(matches!(
            run_plan_command(&PlanCommand::Show, Some(&session)),
            PlanCommandEffect::Text(text) if text.contains("1. Schritt")
        ));
        assert!(matches!(
            run_plan_command(&PlanCommand::Edit, Some(&session)),
            PlanCommandEffect::Edit { slug, .. } if slug == "auth"
        ));
        assert!(matches!(
            run_plan_command(&PlanCommand::Open("../x".to_owned()), Some(&session)),
            PlanCommandEffect::Text(_)
        ));
        assert!(matches!(
            run_plan_command(&PlanCommand::Enter, Some(&session)),
            PlanCommandEffect::EnterPlanStage(_)
        ));
        Ok(())
    }

    #[test]
    fn status_marker_and_composer_hint_name_plan_mode() {
        let span = plan_status_span(Theme::Dark);
        assert!(span.content.contains("plan mode on (shift+tab to cycle)"));
        assert_eq!(span.style.fg, Some(plan_color(Theme::Dark)));
        let (title, _, _) = plan_composer_chrome(Theme::Light);
        assert!(title.contains("es wird nichts verändert"));
        let line = plan_placeholder_line(Theme::Dark);
        assert!(
            line.spans
                .iter()
                .any(|span| span.content.contains("es wird nichts verändert"))
        );
    }
}
