//! Auswahlfenster für `ask_user` (Runde 5, Teil F, Punkt 4).
//!
//! # Verantwortung
//! Zeigt die 1–4 Fragen einer [`AskUserPrompt`] anstelle des Composers, mit
//! Pfeiltasten und Zahlen, Einzel- oder Mehrfachauswahl und der immer
//! vorhandenen Freitext-Option „Andere“. Die Antwort geht als
//! [`AskUserAnswer`] an das wartende Werkzeug zurück; Esc/Ctrl+C brechen ab
//! (das Werkzeug meldet dem Modell dann „keine Antwort“).
//!
//! # Sicherheitsregeln
//! - Modelltext (Fragen, Optionen) läuft durch [`sanitize_inline`].
//! - **Scharf-Verzögerung:** in den ersten [`ASK_ARMING_DELAY`] zählt nur
//!   Esc/Ctrl+C — ein gerade getipptes Enter wählt nichts versehentlich.
//! - Ein Drop ohne Antwort ist ein Abbruch, nie eine Auswahl.
//!
//! # Nebenläufigkeit
//! Lebt exklusiv in `crate::plan_dialog::PlanUi` (Renderer-Thread).

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use harw_tool_plan::ask_user::MAX_OTHER_CHARS;
use harw_tool_plan::{AskQuestion, AskUserAnswer, AskUserPrompt, QuestionAnswer};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Widget, Wrap},
};

use crate::sanitize::sanitize_inline;
use crate::style;
use crate::tui_event::TuiEvent;

/// Scharf-Verzögerung (wie beim Freigabe-Panel).
pub(crate) const ASK_ARMING_DELAY: Duration = Duration::from_millis(600);

/// Höchsthöhe des Fensters in Zeilen.
const MAX_DIALOG_HEIGHT: u16 = 22;
/// Mindesthöhe des Fensters in Zeilen.
const MIN_DIALOG_HEIGHT: u16 = 7;

/// Zustand einer Frage im Fenster.
#[derive(Debug, Clone, Default)]
struct QuestionState {
    cursor: usize,
    selected: Vec<bool>,
    other: String,
    other_chosen: bool,
    editing: bool,
    answered: bool,
}

/// Ergebnis einer Eingabe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AskStep {
    /// Fenster bleibt offen; `true` = neu zeichnen.
    Stay(bool),
    /// Alle Fragen beantwortet, Antwort zugestellt. Trägt eine kurze
    /// Zusammenfassung für die Verlaufszeile.
    Submitted(String),
    /// Abgebrochen (Esc/Ctrl+C); das Werkzeug meldet „keine Antwort“.
    Cancelled,
}

/// Das offene Auswahlfenster.
pub(crate) struct AskUserDialog {
    prompt: Option<AskUserPrompt>,
    questions: Vec<AskQuestion>,
    states: Vec<QuestionState>,
    index: usize,
    shown_at: Instant,
}

impl std::fmt::Debug for AskUserDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AskUserDialog")
            .field("questions", &self.questions.len())
            .field("index", &self.index)
            .finish_non_exhaustive()
    }
}

impl AskUserDialog {
    /// Öffnet das Fenster für `prompt`.
    pub(crate) fn new(prompt: AskUserPrompt, now: Instant) -> Self {
        let questions = prompt.questions().to_vec();
        let states = questions
            .iter()
            .map(|question| QuestionState {
                selected: vec![false; question.options.len()],
                ..QuestionState::default()
            })
            .collect();
        Self {
            prompt: Some(prompt),
            questions,
            states,
            index: 0,
            shown_at: now,
        }
    }

    /// Anzahl der Einträge der aktuellen Frage inklusive „Andere“.
    fn entries(&self) -> usize {
        self.questions
            .get(self.index)
            .map_or(1, |question| question.options.len() + 1)
    }

    fn is_multi(&self) -> bool {
        self.questions
            .get(self.index)
            .is_some_and(|question| question.multi_select)
    }

    fn armed(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.shown_at) >= ASK_ARMING_DELAY
    }

    /// Verarbeitet ein Eingabeereignis.
    pub(crate) fn handle_event(&mut self, event: TuiEvent, now: Instant) -> AskStep {
        match event {
            TuiEvent::Key(key) => self.handle_key(key, now),
            TuiEvent::Paste(text) => {
                let Some(state) = self.states.get_mut(self.index) else {
                    return AskStep::Stay(false);
                };
                if state.editing {
                    for c in text.chars() {
                        let c = if c.is_control() { ' ' } else { c };
                        if state.other.chars().count() < MAX_OTHER_CHARS {
                            state.other.push(c);
                        }
                    }
                    return AskStep::Stay(true);
                }
                AskStep::Stay(false)
            }
            TuiEvent::Resize(..) | TuiEvent::Draw => AskStep::Stay(true),
            TuiEvent::Mouse(_) => AskStep::Stay(false),
        }
    }

    fn handle_key(&mut self, key: KeyEvent, now: Instant) -> AskStep {
        let ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'C'));
        let editing = self
            .states
            .get(self.index)
            .is_some_and(|state| state.editing);
        if ctrl_c || (key.code == KeyCode::Esc && !editing) {
            self.cancel();
            return AskStep::Cancelled;
        }
        if !self.armed(now) {
            return AskStep::Stay(false);
        }
        if editing {
            return self.handle_editing_key(key);
        }
        let entries = self.entries();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(state) = self.states.get_mut(self.index) {
                    state.cursor = (state.cursor + entries - 1) % entries;
                }
                AskStep::Stay(true)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(state) = self.states.get_mut(self.index) {
                    state.cursor = (state.cursor + 1) % entries;
                }
                AskStep::Stay(true)
            }
            KeyCode::Tab | KeyCode::Right => {
                if self.index + 1 < self.questions.len() {
                    self.index += 1;
                }
                AskStep::Stay(true)
            }
            KeyCode::BackTab | KeyCode::Left => {
                self.index = self.index.saturating_sub(1);
                AskStep::Stay(true)
            }
            KeyCode::Char(c) if c.is_ascii_digit() => {
                let Some(number) = c.to_digit(10).and_then(|d| usize::try_from(d).ok()) else {
                    return AskStep::Stay(false);
                };
                if number == 0 || number > entries {
                    return AskStep::Stay(false);
                }
                if let Some(state) = self.states.get_mut(self.index) {
                    state.cursor = number - 1;
                }
                self.activate(true)
            }
            KeyCode::Char(' ') => self.activate(true),
            KeyCode::Enter => self.activate(false),
            _ => AskStep::Stay(false),
        }
    }

    fn handle_editing_key(&mut self, key: KeyEvent) -> AskStep {
        let multi = self.is_multi();
        let Some(state) = self.states.get_mut(self.index) else {
            return AskStep::Stay(false);
        };
        match key.code {
            KeyCode::Esc => {
                state.editing = false;
                AskStep::Stay(true)
            }
            KeyCode::Backspace => {
                state.other.pop();
                AskStep::Stay(true)
            }
            KeyCode::Enter => {
                state.editing = false;
                state.other_chosen = !state.other.trim().is_empty();
                if state.other_chosen && !multi {
                    state.selected.iter_mut().for_each(|flag| *flag = false);
                    return self.commit();
                }
                AskStep::Stay(true)
            }
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL) && !c.is_control() =>
            {
                if state.other.chars().count() < MAX_OTHER_CHARS {
                    state.other.push(c);
                }
                AskStep::Stay(true)
            }
            _ => AskStep::Stay(false),
        }
    }

    /// Wählt/markiert den Eintrag unter dem Cursor. `toggle` (Leertaste,
    /// Ziffer) markiert bei Mehrfachauswahl; Enter bestätigt die Frage.
    fn activate(&mut self, toggle: bool) -> AskStep {
        let multi = self.is_multi();
        let entries = self.entries();
        let Some(state) = self.states.get_mut(self.index) else {
            return AskStep::Stay(false);
        };
        let on_other = state.cursor + 1 == entries;
        if on_other {
            if multi && !toggle && (state.selected.iter().any(|flag| *flag) || state.other_chosen) {
                return self.commit();
            }
            if multi && toggle && state.other_chosen {
                state.other_chosen = false;
                return AskStep::Stay(true);
            }
            state.editing = true;
            return AskStep::Stay(true);
        }
        if multi {
            if toggle {
                if let Some(flag) = state.selected.get_mut(state.cursor) {
                    *flag = !*flag;
                }
                return AskStep::Stay(true);
            }
            if state.selected.iter().any(|flag| *flag) || state.other_chosen {
                return self.commit();
            }
            if let Some(flag) = state.selected.get_mut(state.cursor) {
                *flag = true;
            }
            return AskStep::Stay(true);
        }
        state.selected.iter_mut().for_each(|flag| *flag = false);
        if let Some(flag) = state.selected.get_mut(state.cursor) {
            *flag = true;
        }
        state.other_chosen = false;
        self.commit()
    }

    /// Schließt die aktuelle Frage ab; sind alle beantwortet, wird die
    /// Antwort zugestellt.
    fn commit(&mut self) -> AskStep {
        if let Some(state) = self.states.get_mut(self.index) {
            state.answered = true;
        }
        match self.states.iter().position(|state| !state.answered) {
            Some(next) => {
                self.index = next;
                AskStep::Stay(true)
            }
            None => self.submit(),
        }
    }

    fn answer(&self) -> AskUserAnswer {
        let answers = self
            .questions
            .iter()
            .zip(&self.states)
            .map(|(question, state)| QuestionAnswer {
                question: question.question.clone(),
                selected: question
                    .options
                    .iter()
                    .zip(&state.selected)
                    .filter(|(_, chosen)| **chosen)
                    .map(|(option, _)| option.label.clone())
                    .collect(),
                other: state
                    .other_chosen
                    .then(|| state.other.trim().to_owned())
                    .filter(|text| !text.is_empty()),
            })
            .collect();
        AskUserAnswer { answers }
    }

    fn submit(&mut self) -> AskStep {
        let answer = self.answer();
        let summary = answer
            .answers
            .iter()
            .map(|entry| {
                let mut parts = entry.selected.clone();
                if let Some(other) = &entry.other {
                    parts.push(format!("„{other}“"));
                }
                sanitize_inline(&parts.join(", "))
            })
            .collect::<Vec<_>>()
            .join(" · ");
        match self.prompt.take() {
            Some(prompt) => {
                if prompt.answer(answer) {
                    AskStep::Submitted(summary)
                } else {
                    AskStep::Cancelled
                }
            }
            None => AskStep::Cancelled,
        }
    }

    /// Bricht ab (Drop der Frage).
    pub(crate) fn cancel(&mut self) {
        if let Some(prompt) = self.prompt.take() {
            prompt.cancel();
        }
    }

    fn lines(&self, theme: style::Theme, now: Instant) -> Vec<Line<'static>> {
        let dim = style::dim_style(theme);
        let accent = style::selected_style(theme);
        let mut lines = Vec::new();
        let total = self.questions.len();
        let mut tabs: Vec<Span<'static>> = vec![Span::styled(
            format!("Frage {}/{total}  ", self.index + 1),
            dim,
        )];
        for (i, (question, state)) in self.questions.iter().zip(&self.states).enumerate() {
            let label = question
                .header
                .as_deref()
                .map_or_else(|| (i + 1).to_string(), sanitize_inline);
            let mark = if state.answered { "✓ " } else { "" };
            let style = if i == self.index { accent } else { dim };
            tabs.push(Span::styled(format!("[{mark}{label}] "), style));
        }
        lines.push(Line::from(tabs));
        let (Some(question), Some(state)) =
            (self.questions.get(self.index), self.states.get(self.index))
        else {
            return lines;
        };
        lines.push(Line::styled(
            sanitize_inline(&question.question),
            Style::default().add_modifier(Modifier::BOLD),
        ));
        if question.multi_select {
            lines.push(Line::styled("(Mehrfachauswahl)", dim));
        }
        for (i, option) in question.options.iter().enumerate() {
            let chosen = state.selected.get(i).copied().unwrap_or(false);
            let marker = match (question.multi_select, chosen) {
                (true, true) => "[x]",
                (true, false) => "[ ]",
                (false, true) => "(•)",
                (false, false) => "( )",
            };
            let pointer = if state.cursor == i { "›" } else { " " };
            let style = if state.cursor == i {
                accent
            } else {
                Style::default()
            };
            let mut spans = vec![Span::styled(
                format!(
                    "{pointer} {}. {marker} {}",
                    i + 1,
                    sanitize_inline(&option.label)
                ),
                style,
            )];
            if let Some(description) = &option.description {
                spans.push(Span::styled(
                    format!(" — {}", sanitize_inline(description)),
                    dim,
                ));
            }
            lines.push(Line::from(spans));
        }
        let other_index = question.options.len();
        let pointer = if state.cursor == other_index {
            "›"
        } else {
            " "
        };
        let marker = match (question.multi_select, state.other_chosen) {
            (true, true) => "[x]",
            (true, false) => "[ ]",
            (false, true) => "(•)",
            (false, false) => "( )",
        };
        let text = if state.editing {
            format!("{}▏", sanitize_inline(&state.other))
        } else if state.other.is_empty() {
            "eigene Antwort eingeben …".to_owned()
        } else {
            sanitize_inline(&state.other)
        };
        let style = if state.cursor == other_index {
            accent
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{pointer} {}. {marker} Andere: ", other_index + 1),
                style,
            ),
            Span::styled(text, if state.editing { Style::default() } else { dim }),
        ]));
        let footer = if !self.armed(now) {
            "Fenster wird gleich scharf … · Esc bricht ab"
        } else if state.editing {
            "Tippen · Enter übernehmen · Esc zurück"
        } else if question.multi_select {
            "↑↓ wählen · Leertaste/Ziffer markieren · Enter bestätigen · Tab nächste Frage · Esc abbrechen"
        } else {
            "↑↓ wählen · Ziffer/Enter übernehmen · Tab nächste Frage · Esc abbrechen"
        };
        lines.push(Line::styled(footer, dim));
        lines
    }

    /// Benötigte Höhe (inkl. Rahmen).
    pub(crate) fn desired_height(&self, width: u16, theme: style::Theme) -> u16 {
        let inner_width = width.saturating_sub(2).max(1);
        let rows = Paragraph::new(self.lines(theme, Instant::now()))
            .wrap(Wrap { trim: false })
            .line_count(inner_width);
        u16::try_from(rows)
            .unwrap_or(u16::MAX)
            .saturating_add(2)
            .clamp(MIN_DIALOG_HEIGHT, MAX_DIALOG_HEIGHT)
    }

    /// Zeichnet das Fenster anstelle des Composers.
    pub(crate) fn render(&self, area: Rect, buf: &mut Buffer, theme: style::Theme) {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(style::accent_color(theme)))
            .title(" Rückfrage des Agenten · ask_user ");
        Paragraph::new(self.lines(theme, Instant::now()))
            .wrap(Wrap { trim: false })
            .block(block)
            .render(area, buf);
    }
}

impl Drop for AskUserDialog {
    /// Ein geschlossenes Fenster ohne Antwort ist ein Abbruch.
    fn drop(&mut self) {
        self.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use harw_tool_plan::AskOption;

    fn question(text: &str, multi: bool) -> AskQuestion {
        AskQuestion {
            question: text.to_owned(),
            header: None,
            options: vec![
                AskOption {
                    label: "Alpha".to_owned(),
                    description: Some("erste".to_owned()),
                },
                AskOption {
                    label: "Beta".to_owned(),
                    description: None,
                },
            ],
            multi_select: multi,
        }
    }

    fn key(code: KeyCode) -> TuiEvent {
        TuiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn armed(shown: Instant) -> Instant {
        shown + ASK_ARMING_DELAY + Duration::from_millis(1)
    }

    #[tokio::test]
    async fn arrows_numbers_and_other_text_produce_the_answer() -> TestResult {
        let (prompt, answer) = AskUserPrompt::new(vec![
            question("Welche?", false),
            question("Welche noch?", true),
        ]);
        let shown = Instant::now();
        let mut dialog = AskUserDialog::new(prompt, shown);
        let now = armed(shown);
        // Frage 1: ↓ auf Beta, Enter.
        assert_eq!(
            dialog.handle_event(key(KeyCode::Down), now),
            AskStep::Stay(true)
        );
        assert_eq!(
            dialog.handle_event(key(KeyCode::Enter), now),
            AskStep::Stay(true)
        );
        // Frage 2 (Mehrfach): 1 markiert Alpha, 3 öffnet „Andere“.
        dialog.handle_event(key(KeyCode::Char('1')), now);
        dialog.handle_event(key(KeyCode::Char('3')), now);
        for c in "Gamma".chars() {
            dialog.handle_event(key(KeyCode::Char(c)), now);
        }
        dialog.handle_event(key(KeyCode::Enter), now);
        let step = dialog.handle_event(key(KeyCode::Enter), now);
        assert!(matches!(step, AskStep::Submitted(_)), "{step:?}");
        let received = answer.await.map_err(|_| TestError::Missing("Antwort"))?;
        assert_eq!(received.answers[0].selected, vec!["Beta".to_owned()]);
        assert_eq!(received.answers[1].selected, vec!["Alpha".to_owned()]);
        assert_eq!(received.answers[1].other.as_deref(), Some("Gamma"));
        Ok(())
    }

    #[tokio::test]
    async fn keys_before_arming_select_nothing_and_esc_cancels() -> TestResult {
        let (prompt, answer) = AskUserPrompt::new(vec![question("Welche?", false)]);
        let shown = Instant::now();
        let mut dialog = AskUserDialog::new(prompt, shown);
        assert_eq!(
            dialog.handle_event(key(KeyCode::Enter), shown),
            AskStep::Stay(false),
            "vor der Scharf-Verzögerung zählt Enter nicht"
        );
        assert_eq!(
            dialog.handle_event(key(KeyCode::Esc), shown),
            AskStep::Cancelled
        );
        assert!(answer.await.is_err(), "Abbruch ist keine Antwort");
        Ok(())
    }

    #[tokio::test]
    async fn dropping_the_dialog_cancels_the_question() -> TestResult {
        let (prompt, answer) = AskUserPrompt::new(vec![question("Welche?", false)]);
        drop(AskUserDialog::new(prompt, Instant::now()));
        assert!(answer.await.is_err());
        Ok(())
    }

    #[test]
    fn renders_every_option_plus_other() {
        let (prompt, _answer) = AskUserPrompt::new(vec![question("Welche?", false)]);
        let dialog = AskUserDialog::new(prompt, Instant::now());
        let text: String = dialog
            .lines(style::Theme::Dark, Instant::now())
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("Welche?"));
        assert!(text.contains("1. ( ) Alpha — erste"));
        assert!(text.contains("2. ( ) Beta"));
        assert!(text.contains("3. ( ) Andere"));
        assert!(dialog.desired_height(80, style::Theme::Dark) >= MIN_DIALOG_HEIGHT);
    }
}
