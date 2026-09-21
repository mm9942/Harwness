//! Interactive provider/model picker used by `harw models add`.

use std::collections::BTreeSet;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::setup::TerminalGuard;
use crate::{TuiError, style};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelPickerProvider {
    pub id: String,
    pub models: Vec<String>,
    pub selected: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelPickerOutcome {
    pub provider: String,
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage { Providers, Models }

struct PickerApp {
    providers: Vec<ModelPickerProvider>,
    provider_index: usize,
    model_index: usize,
    stage: Stage,
}

impl PickerApp {
    fn new(mut providers: Vec<ModelPickerProvider>) -> Self {
        for provider in &mut providers { provider.models.sort(); provider.models.dedup(); }
        Self { providers, provider_index: 0, model_index: 0, stage: Stage::Providers }
    }

    fn on_key(&mut self, key: KeyEvent) -> Option<ModelPickerOutcome> {
        match self.stage {
            Stage::Providers => match key.code {
                KeyCode::Up => self.provider_index = self.provider_index.saturating_sub(1),
                KeyCode::Down => if self.provider_index + 1 < self.providers.len() { self.provider_index += 1; },
                KeyCode::Enter | KeyCode::Right if !self.providers.is_empty() => { self.stage = Stage::Models; self.model_index = 0; },
                _ => {}
            },
            Stage::Models => {
                let provider = self.providers.get_mut(self.provider_index)?;
                match key.code {
                    KeyCode::Up => self.model_index = self.model_index.saturating_sub(1),
                    KeyCode::Down => if self.model_index + 1 < provider.models.len() { self.model_index += 1; },
                    KeyCode::Char(' ') => if let Some(model) = provider.models.get(self.model_index) {
                        if !provider.selected.remove(model) { provider.selected.insert(model.clone()); }
                    },
                    KeyCode::Left => self.stage = Stage::Providers,
                    KeyCode::Enter => return Some(ModelPickerOutcome { provider: provider.id.clone(), models: provider.selected.iter().cloned().collect() }),
                    _ => {}
                }
            }
        }
        None
    }
}

pub fn run_model_picker(providers: Vec<ModelPickerProvider>) -> Result<Option<ModelPickerOutcome>, TuiError> {
    let mut app = PickerApp::new(providers);
    let mut terminal = TerminalGuard::enter()?;
    let result = loop {
        draw(&mut terminal, &app)?;
        if !event::poll(Duration::from_millis(100))? { continue; }
        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Release { continue; }
            if key.code == KeyCode::Esc { break Ok(None); }
            if let Some(outcome) = app.on_key(key) { break Ok(Some(outcome)); }
        }
    };
    drop(terminal);
    result
}

fn draw(terminal: &mut TerminalGuard, app: &PickerApp) -> Result<(), TuiError> {
    terminal.terminal().draw(|frame| {
        let chunks = Layout::default().direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(1)]).split(frame.area());
        let title = match app.stage { Stage::Providers => "Provider wählen", Stage::Models => "Modelle wählen" };
        frame.render_widget(Paragraph::new("↑/↓ navigieren · Enter/→ öffnen · Leertaste markieren · Enter bestätigen · Esc abbrechen")
            .block(Block::default().borders(Borders::ALL).title(title)), chunks[0]);
        let theme = style::detect_theme();
        let lines = match app.stage {
            Stage::Providers => app.providers.iter().enumerate().map(|(i, provider)| {
                let prefix = if i == app.provider_index { "› " } else { "  " };
                let count = provider.models.len();
                ratatui::text::Line::styled(format!("{prefix}{} ({count} Modelle)", provider.id), if i == app.provider_index { style::selected_style(theme) } else { ratatui::style::Style::default() })
            }).collect::<Vec<_>>(),
            Stage::Models => app.providers.get(app.provider_index).map(|provider| provider.models.iter().enumerate().map(|(i, model)| {
                let checked = if provider.selected.contains(model) { "☑" } else { "☐" };
                let prefix = if i == app.model_index { "›" } else { " " };
                ratatui::text::Line::styled(format!("{prefix} {checked} {model}"), if i == app.model_index { style::selected_style(theme) } else { ratatui::style::Style::default() })
            }).collect()).unwrap_or_default(),
        };
        frame.render_widget(Paragraph::new(lines).block(Block::default().borders(Borders::ALL)), chunks[1]);
    }).map_err(TuiError::from)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    fn key(code: KeyCode) -> KeyEvent { KeyEvent::new(code, KeyModifiers::NONE) }
    #[test]
    fn space_toggles_and_enter_confirms() {
        let mut app = PickerApp::new(vec![ModelPickerProvider { id: "p".into(), models: vec!["a".into()], selected: BTreeSet::new() }]);
        app.on_key(key(KeyCode::Enter));
        app.on_key(key(KeyCode::Char(' ')));
        let outcome = app.on_key(key(KeyCode::Enter)).expect("confirmed");
        assert_eq!(outcome.models, ["a"]);
    }
}
