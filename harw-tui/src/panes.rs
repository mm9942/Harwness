//! Seitenpanels der TUI: Aufteilung der Chat-Fläche und Fokusführung.
//!
//! ```text
//! ┌ Explorer (F2) ─┬──────── Chat ────────┬ Agenten (F3) ─┐
//! │                │                      │               │
//! │                │                      ├ Workbench (F5)┤
//! │                │                      │               │
//! └────────────────┴──────────────────────┴───────────────┘
//! ```
//!
//! Panels erscheinen nur, wenn die Fläche breit genug ist
//! ([`MIN_CHAT_WIDTH`] bleibt immer für den Chat reserviert). Agenten und
//! Workbench teilen sich die rechte Spalte: sind beide sichtbar, bekommen
//! die Agenten oben 60 %, die Workbench unten 40 % (mindestens
//! [`WORKBENCH_MIN_ROWS`] Zeilen). In der Standard-Belegung wechselt `F4`
//! den Fokus reihum; `Esc` gibt ihn an den Chat zurück. Die Tasten sind über `[tui].keybindings_file` umbelegbar
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
/// Mindesthöhe der Workbench, wenn sie die rechte Spalte mit den Agenten
/// teilt.
pub(crate) const WORKBENCH_MIN_ROWS: u16 = 8;
/// Mindesthöhe der Agenten über der Workbench.
pub(crate) const AGENTS_MIN_ROWS: u16 = 3;

/// Welche Fläche Tastatureingaben bekommt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PaneFocus {
    #[default]
    Chat,
    Explorer,
    Agents,
    Workbench,
}

/// Sichtbarkeit und Fokus der Panels.
#[derive(Debug, Clone)]
pub(crate) struct PanelState {
    pub agents_visible: bool,
    pub explorer_visible: bool,
    /// Workbench (rechte Spalte, unter den Agenten).
    pub workbench_visible: bool,
    /// Ein Panel im Vollbild (ersetzt Chat und das andere Panel).
    pub maximized: bool,
    pub focus: PaneFocus,
}

impl Default for PanelState {
    fn default() -> Self {
        Self {
            agents_visible: true,
            explorer_visible: false,
            workbench_visible: false,
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
    /// ([`KeyBindings::default`]): `F2` Explorer, `F3` Agenten, `F5`
    /// Workbench, `F4` Fokus, `Ctrl+E` Explorer fokussieren, `F11` Vollbild
    /// des fokussierten Panels. `Esc` (nicht umbelegbar) gibt den Fokus aus
    /// dem Agenten- oder Workbench-Panel an den Chat zurück.
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
            Some(KeyAction::ToggleWorkbench) => {
                self.workbench_visible = !self.workbench_visible;
                if !self.workbench_visible && self.focus == PaneFocus::Workbench {
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
            KeyCode::Esc if matches!(self.focus, PaneFocus::Agents | PaneFocus::Workbench) => {
                self.focus = PaneFocus::Chat;
                self.maximized = false;
                PanelKey::Changed
            }
            _ if self.focus != PaneFocus::Chat => PanelKey::ForFocused(key),
            _ => PanelKey::Ignored,
        }
    }

    fn cycle_focus(&mut self) {
        let order = [
            PaneFocus::Chat,
            PaneFocus::Explorer,
            PaneFocus::Agents,
            PaneFocus::Workbench,
        ];
        let start = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        for step in 1..=order.len() {
            let next = order[(start + step) % order.len()];
            let visible = match next {
                PaneFocus::Chat => true,
                PaneFocus::Explorer => self.explorer_visible,
                PaneFocus::Agents => self.agents_visible,
                PaneFocus::Workbench => self.workbench_visible,
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
    pub workbench: Option<Rect>,
}

/// Teilt `area` gemäß `state` auf.
pub(crate) fn split(area: Rect, state: &PanelState) -> PaneAreas {
    if state.maximized {
        return match state.focus {
            PaneFocus::Explorer => PaneAreas {
                explorer: Some(area),
                chat: None,
                agents: None,
                workbench: None,
            },
            PaneFocus::Agents => PaneAreas {
                explorer: None,
                chat: None,
                agents: Some(area),
                workbench: None,
            },
            PaneFocus::Workbench => PaneAreas {
                explorer: None,
                chat: None,
                agents: None,
                workbench: Some(area),
            },
            PaneFocus::Chat => PaneAreas {
                explorer: None,
                chat: Some(area),
                agents: None,
                workbench: None,
            },
        };
    }
    let mut budget = area.width.saturating_sub(MIN_CHAT_WIDTH);
    let right_visible = state.agents_visible || state.workbench_visible;
    let agents_w = if right_visible && budget >= AGENTS_WIDTH {
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
    let (agents, workbench) = if agents_w > 0 {
        split_right_column(chunks[2], state)
    } else {
        (None, None)
    };
    PaneAreas {
        explorer: (explorer_w > 0).then_some(chunks[0]),
        chat: Some(chunks[1]),
        agents,
        workbench,
    }
}

/// Teilt die rechte Spalte zwischen Agenten (oben) und Workbench (unten).
///
/// Sind beide sichtbar, bekommt die Workbench 40 % der Höhe, mindestens
/// [`WORKBENCH_MIN_ROWS`], und die Agenten behalten mindestens
/// [`AGENTS_MIN_ROWS`]. Reicht die Höhe dafür nicht, erhält das fokussierte
/// der beiden Panels die ganze Spalte (sonst die Agenten).
fn split_right_column(column: Rect, state: &PanelState) -> (Option<Rect>, Option<Rect>) {
    match (state.agents_visible, state.workbench_visible) {
        (true, false) => (Some(column), None),
        (false, true) => (None, Some(column)),
        (false, false) => (None, None),
        (true, true) => {
            let height = column.height;
            if height < WORKBENCH_MIN_ROWS + AGENTS_MIN_ROWS {
                return if state.focus == PaneFocus::Workbench {
                    (None, Some(column))
                } else {
                    (Some(column), None)
                };
            }
            let proportional = u16::try_from(u32::from(height) * 2 / 5).unwrap_or(height);
            let workbench_h = proportional
                .max(WORKBENCH_MIN_ROWS)
                .min(height - AGENTS_MIN_ROWS);
            let agents_h = height - workbench_h;
            let agents = Rect {
                height: agents_h,
                ..column
            };
            let workbench = Rect {
                y: column.y + agents_h,
                height: workbench_h,
                ..column
            };
            (Some(agents), Some(workbench))
        }
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
            KeyBindings::from_toml_str("cycle_focus = \"F20\"\n", std::path::Path::new("kb.toml"))?;
        let mut state = PanelState::default();
        assert_eq!(
            state.handle_key(key(KeyCode::F(4)), &bindings),
            PanelKey::Ignored
        );
        assert_eq!(state.focus, PaneFocus::Chat);
        assert_eq!(
            state.handle_key(key(KeyCode::F(20)), &bindings),
            PanelKey::Changed
        );
        assert_eq!(state.focus, PaneFocus::Agents);
        Ok(())
    }

    #[test]
    fn workbench_is_hidden_by_default() {
        let areas = split(Rect::new(0, 0, 160, 40), &PanelState::default());
        assert!(areas.workbench.is_none());
        assert_eq!(areas.agents.map(|r| r.height), Some(40));
    }

    #[test]
    fn agents_and_workbench_share_the_right_column() {
        let state = PanelState {
            workbench_visible: true,
            ..PanelState::default()
        };
        let areas = split(Rect::new(0, 0, 160, 40), &state);
        let agents = areas.agents.unwrap_or_default();
        let workbench = areas.workbench.unwrap_or_default();
        assert_eq!(agents.height, 24);
        assert_eq!(workbench.height, 16);
        assert_eq!(workbench.y, agents.y + agents.height);
        assert_eq!(agents.x, workbench.x);
        assert_eq!(workbench.width, AGENTS_WIDTH);
        assert_eq!(areas.chat.map(|r| r.width), Some(160 - AGENTS_WIDTH));
    }

    #[test]
    fn workbench_keeps_minimum_rows() {
        let state = PanelState {
            workbench_visible: true,
            ..PanelState::default()
        };
        let areas = split(Rect::new(0, 0, 160, 12), &state);
        assert_eq!(areas.workbench.map(|r| r.height), Some(WORKBENCH_MIN_ROWS));
        assert_eq!(
            areas.agents.map(|r| r.height),
            Some(12 - WORKBENCH_MIN_ROWS)
        );

        // Zu niedrig für beide: das fokussierte Panel bekommt die Spalte.
        let mut tight = state.clone();
        let areas = split(Rect::new(0, 0, 160, 9), &tight);
        assert_eq!(areas.agents.map(|r| r.height), Some(9));
        assert!(areas.workbench.is_none());
        tight.focus = PaneFocus::Workbench;
        let areas = split(Rect::new(0, 0, 160, 9), &tight);
        assert_eq!(areas.workbench.map(|r| r.height), Some(9));
        assert!(areas.agents.is_none());
    }

    #[test]
    fn workbench_alone_takes_the_right_column() {
        let state = PanelState {
            agents_visible: false,
            workbench_visible: true,
            ..PanelState::default()
        };
        let areas = split(Rect::new(0, 0, 160, 30), &state);
        assert!(areas.agents.is_none());
        assert_eq!(
            areas.workbench,
            Some(Rect::new(160 - AGENTS_WIDTH, 0, AGENTS_WIDTH, 30))
        );
    }

    #[test]
    fn f5_toggles_workbench_and_focus_cycle_includes_it() {
        let bindings = KeyBindings::default();
        let mut state = PanelState::default();
        assert_eq!(
            state.handle_key(key(KeyCode::F(5)), &bindings),
            PanelKey::Changed
        );
        assert!(state.workbench_visible);
        state.handle_key(key(KeyCode::F(4)), &bindings);
        assert_eq!(state.focus, PaneFocus::Agents);
        state.handle_key(key(KeyCode::F(4)), &bindings);
        assert_eq!(state.focus, PaneFocus::Workbench);
        assert!(matches!(
            state.handle_key(key(KeyCode::Char('n')), &bindings),
            PanelKey::ForFocused(_)
        ));
        state.handle_key(key(KeyCode::F(11)), &bindings);
        let areas = split(Rect::new(0, 0, 100, 20), &state);
        assert_eq!(areas.workbench, Some(Rect::new(0, 0, 100, 20)));
        assert!(areas.chat.is_none() && areas.agents.is_none());
        assert_eq!(
            state.handle_key(key(KeyCode::Esc), &bindings),
            PanelKey::Changed
        );
        assert_eq!(state.focus, PaneFocus::Chat);
        assert!(!state.maximized);

        state.handle_key(key(KeyCode::F(4)), &bindings);
        state.handle_key(key(KeyCode::F(4)), &bindings);
        assert_eq!(state.focus, PaneFocus::Workbench);
        state.handle_key(key(KeyCode::F(4)), &bindings);
        assert_eq!(state.focus, PaneFocus::Chat, "cycle wraps to chat");

        state.focus = PaneFocus::Workbench;
        state.handle_key(key(KeyCode::F(5)), &bindings);
        assert!(!state.workbench_visible);
        assert_eq!(state.focus, PaneFocus::Chat, "hiding releases focus");
        state.handle_key(key(KeyCode::F(4)), &bindings);
        state.handle_key(key(KeyCode::F(4)), &bindings);
        assert_eq!(state.focus, PaneFocus::Chat, "hidden workbench skipped");
    }
}
