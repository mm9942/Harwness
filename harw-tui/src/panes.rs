//! Seitenpanels der TUI: Aufteilung der Chat-Fläche und Fokusführung.
//!
//! ```text
//! ┌ Explorer (F2) ─┬──────── Chat ────────┬ Agenten (F3) ─┐
//! │                │                      │               │
//! └────────────────┴──────────────────────┴───────────────┘
//! ```
//!
//! Panels erscheinen nur, wenn die Fläche breit genug ist
//! ([`MIN_CHAT_WIDTH`] bleibt immer für den Chat reserviert). In der
//! Standard-Belegung wechselt `F4` den Fokus reihum; `Esc` gibt ihn an den
//! Chat zurück. Die Tasten sind über `[tui].keybindings_file` umbelegbar
//! (siehe [`crate::keybindings`]).

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};

use crate::keybindings::{KeyAction, KeyBindings};

/// Mindestbreite, die der Chat neben den Panels behält.
pub(crate) const MIN_CHAT_WIDTH: u16 = 48;
/// Breite des Agenten-Panels.
pub(crate) const AGENTS_WIDTH: u16 = 44;
/// Breite des Explorer-Panels.
pub(crate) const EXPLORER_WIDTH: u16 = 36;

/// Welche Fläche Tastatureingaben bekommt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PaneFocus {
    #[default]
    Chat,
    Explorer,
    Agents,
}

/// Sichtbarkeit und Fokus der Panels.
#[derive(Debug, Clone)]
pub(crate) struct PanelState {
    pub agents_visible: bool,
    pub explorer_visible: bool,
    /// Ein Panel im Vollbild (ersetzt Chat und das andere Panel).
    pub maximized: bool,
    pub focus: PaneFocus,
}

impl Default for PanelState {
    fn default() -> Self {
        Self {
            agents_visible: true,
            explorer_visible: false,
            maximized: false,
            focus: PaneFocus::Chat,
        }
    }
}

/// Ergebnis der Panel-Tastenbehandlung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelKey {
    /// Taste gehört nicht den Panels.
    Ignored,
    /// Sichtbarkeit/Fokus geändert; neu zeichnen.
    Changed,
    /// Taste soll an das fokussierte Panel gehen.
    ForFocused(KeyEvent),
}

impl PanelState {
    /// Globale Panel-Tasten gemäß `bindings`. In der Standard-Belegung
    /// ([`KeyBindings::default`]): `F2` Explorer, `F3` Agenten, `F4` Fokus,
    /// `Ctrl+E` Explorer fokussieren, `F11` Vollbild des fokussierten
    /// Panels. `Esc` (nicht umbelegbar) gibt den Fokus aus dem
    /// Agenten-Panel an den Chat zurück.
    pub(crate) fn handle_key(&mut self, key: KeyEvent, bindings: &KeyBindings) -> PanelKey {
        match bindings.action_for(&key) {
            Some(KeyAction::ToggleExplorer) => {
                self.explorer_visible = !self.explorer_visible;
                if !self.explorer_visible && self.focus == PaneFocus::Explorer {
                    self.focus = PaneFocus::Chat;
                    self.maximized = false;
                }
                return PanelKey::Changed;
            }
            Some(KeyAction::ToggleAgents) => {
                self.agents_visible = !self.agents_visible;
                if !self.agents_visible && self.focus == PaneFocus::Agents {
                    self.focus = PaneFocus::Chat;
                    self.maximized = false;
                }
                return PanelKey::Changed;
            }
            Some(KeyAction::CycleFocus) => {
                self.cycle_focus();
                return PanelKey::Changed;
            }
            Some(KeyAction::FocusExplorer) => {
                self.explorer_visible = true;
                self.focus = PaneFocus::Explorer;
                return PanelKey::Changed;
            }
            Some(KeyAction::MaximizePanel) if self.focus != PaneFocus::Chat => {
                self.maximized = !self.maximized;
                return PanelKey::Changed;
            }
            _ => {}
        }
        match key.code {
            // Im Explorer gehört Esc zuerst dem Panel (Filter/Vorschau
            // schließen); das Panel gibt den Fokus selbst zurück.
            KeyCode::Esc if self.focus == PaneFocus::Agents => {
                self.focus = PaneFocus::Chat;
                self.maximized = false;
                PanelKey::Changed
            }
            _ if self.focus != PaneFocus::Chat => PanelKey::ForFocused(key),
            _ => PanelKey::Ignored,
        }
    }

    fn cycle_focus(&mut self) {
        let order = [PaneFocus::Chat, PaneFocus::Explorer, PaneFocus::Agents];
        let start = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        for step in 1..=order.len() {
            let next = order[(start + step) % order.len()];
            let visible = match next {
                PaneFocus::Chat => true,
                PaneFocus::Explorer => self.explorer_visible,
                PaneFocus::Agents => self.agents_visible,
            };
            if visible {
                self.focus = next;
                if next == PaneFocus::Chat {
                    self.maximized = false;
                }
                return;
            }
        }
    }
}

/// Aufgeteilte Flächen; `None` für ausgeblendete Panels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PaneAreas {
    pub explorer: Option<Rect>,
    pub chat: Option<Rect>,
    pub agents: Option<Rect>,
}

/// Teilt `area` gemäß `state` auf.
pub(crate) fn split(area: Rect, state: &PanelState) -> PaneAreas {
    if state.maximized {
        return match state.focus {
            PaneFocus::Explorer => PaneAreas {
                explorer: Some(area),
                chat: None,
                agents: None,
            },
            PaneFocus::Agents => PaneAreas {
                explorer: None,
                chat: None,
                agents: Some(area),
            },
            PaneFocus::Chat => PaneAreas {
                explorer: None,
                chat: Some(area),
                agents: None,
            },
        };
    }
    let mut budget = area.width.saturating_sub(MIN_CHAT_WIDTH);
    let agents_w = if state.agents_visible && budget >= AGENTS_WIDTH {
        budget -= AGENTS_WIDTH;
        AGENTS_WIDTH
    } else {
        0
    };
    let explorer_w = if state.explorer_visible && budget >= EXPLORER_WIDTH {
        EXPLORER_WIDTH
    } else {
        0
    };
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(explorer_w),
            Constraint::Min(MIN_CHAT_WIDTH.min(area.width)),
            Constraint::Length(agents_w),
        ])
        .split(area);
    PaneAreas {
        explorer: (explorer_w > 0).then_some(chunks[0]),
        chat: Some(chunks[1]),
        agents: (agents_w > 0).then_some(chunks[2]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn narrow_terminals_keep_only_the_chat() {
        let areas = split(Rect::new(0, 0, 80, 20), &PanelState::default());
        assert!(areas.agents.is_none());
        assert_eq!(areas.chat.map(|r| r.width), Some(80));
    }

    #[test]
    fn wide_terminals_show_agents_panel() {
        let areas = split(Rect::new(0, 0, 160, 20), &PanelState::default());
        assert_eq!(areas.agents.map(|r| r.width), Some(AGENTS_WIDTH));
        assert_eq!(areas.chat.map(|r| r.width), Some(160 - AGENTS_WIDTH));
    }

    #[test]
    fn focus_cycles_through_visible_panels_and_escape_returns() {
        let bindings = KeyBindings::default();
        let mut state = PanelState::default();
        assert_eq!(
            state.handle_key(key(KeyCode::F(4)), &bindings),
            PanelKey::Changed
        );
        assert_eq!(state.focus, PaneFocus::Agents, "explorer hidden → skipped");
        assert!(matches!(
            state.handle_key(key(KeyCode::Down), &bindings),
            PanelKey::ForFocused(_)
        ));
        state.handle_key(key(KeyCode::F(11)), &bindings);
        let areas = split(Rect::new(0, 0, 100, 20), &state);
        assert_eq!(areas.agents.map(|r| r.width), Some(100));
        assert!(areas.chat.is_none());
        state.handle_key(key(KeyCode::Esc), &bindings);
        assert_eq!(state.focus, PaneFocus::Chat);
        assert!(!state.maximized);
        assert_eq!(
            state.handle_key(key(KeyCode::Down), &bindings),
            PanelKey::Ignored
        );
    }

    #[test]
    fn ctrl_e_focuses_explorer_and_f2_hides_it() {
        let bindings = KeyBindings::default();
        let mut state = PanelState::default();
        let ctrl_e = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL);
        assert_eq!(state.handle_key(ctrl_e, &bindings), PanelKey::Changed);
        assert!(state.explorer_visible);
        assert_eq!(state.focus, PaneFocus::Explorer);
        assert_eq!(
            state.handle_key(key(KeyCode::F(2)), &bindings),
            PanelKey::Changed
        );
        assert!(!state.explorer_visible);
        assert_eq!(state.focus, PaneFocus::Chat);
        assert_eq!(
            state.handle_key(key(KeyCode::F(11)), &bindings),
            PanelKey::Ignored,
            "F11 im Chat bleibt wirkungslos"
        );
    }

    #[test]
    fn custom_bindings_replace_defaults() -> Result<(), Box<dyn std::error::Error>> {
        let bindings =
            KeyBindings::from_toml_str("cycle_focus = \"F6\"\n", std::path::Path::new("kb.toml"))?;
        let mut state = PanelState::default();
        assert_eq!(
            state.handle_key(key(KeyCode::F(4)), &bindings),
            PanelKey::Ignored
        );
        assert_eq!(state.focus, PaneFocus::Chat);
        assert_eq!(
            state.handle_key(key(KeyCode::F(6)), &bindings),
            PanelKey::Changed
        );
        assert_eq!(state.focus, PaneFocus::Agents);
        Ok(())
    }
}
