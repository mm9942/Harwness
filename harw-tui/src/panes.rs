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
//! ([`MIN_CHAT_WIDTH`] bleibt immer für den Chat reserviert). Die rechte
//! Spalte (Agenten/Workbench) braucht zusätzlich mindestens
//! [`AGENTS_PANEL_MIN_TERMINAL_WIDTH`] Spalten Terminalbreite; darunter
//! klappt das Agenten-Panel zu einer Zeile über der Statuszeile zusammen
//! (ab [`AGENTS_SUMMARY_MIN_HEIGHT`] Zeilen Höhe, darunter entfällt es ganz),
//! damit der Chat in kleinen Kacheln seine Breite behält. Im Vollbild
//! (`F11`, Detailansicht) gilt die Schwelle nicht. Agenten und
//! Workbench teilen sich die rechte Spalte: sind beide sichtbar, bekommen
//! die Agenten oben 60 %, die Workbench unten 40 % (mindestens
//! [`WORKBENCH_MIN_ROWS`] Zeilen). In der Standard-Belegung wechselt `F4`
//! den Fokus reihum; `Esc` gibt ihn an den Chat zurück. Die Tasten sind über `[tui].keybindings_file` umbelegbar
//! (siehe [`crate::keybindings`]).
//!
//! Dieses Modul ist außerdem die einzige Stelle, an der `harw-tui` in die
//! Geometrie-Crate `harw-tui-layout` hineinschaut: [`classify_screen`]
//! wandelt deren Klassifikation (breite Seitenspalte, oben angedockter
//! Portrait-Dock oder Zusammenfassungs-/Kompakt-Fallback) in
//! `ratatui::layout::Rect` um, [`split_dock_areas`] entsprechend die interne
//! Aufteilung des Docks. `PaneFocus`, `PanelState`, `PanelKey`, [`split`] und
//! `split_right_column` bleiben unverändert und beschreiben weiterhin nur
//! die breite Seitenspalten-Darstellung von oben.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};

use crate::keybindings::{KeyAction, KeyBindings};

/// Mindestbreite, die der Chat neben den Panels behält.
pub(crate) const MIN_CHAT_WIDTH: u16 = 48;
/// Kleinste Terminalbreite, ab der die rechte Spalte (Agenten-Panel,
/// Workbench) neben dem Chat erscheint. Darunter behält der Chat die ganze
/// Breite (100 = 44 Panel + 56 Chat; bei 80–99 Spalten wäre der Chat sonst
/// auf 36–55 Spalten gequetscht).
pub(crate) const AGENTS_PANEL_MIN_TERMINAL_WIDTH: u16 = 100;
/// Kleinste Terminalhöhe, ab der das zusammengeklappte Agenten-Panel als
/// eigene Zeile über der Statuszeile erscheint; darunter entfällt es ganz.
pub(crate) const AGENTS_SUMMARY_MIN_HEIGHT: u16 = 16;
/// Breite des Agenten-Panels.
pub(crate) const AGENTS_WIDTH: u16 = 44;
/// Breite des Explorer-Panels.
pub(crate) const EXPLORER_WIDTH: u16 = 36;
/// Mindesthöhe der Workbench, wenn sie die rechte Spalte mit den Agenten
/// teilt.
pub(crate) const WORKBENCH_MIN_ROWS: u16 = 8;
/// Mindesthöhe der Agenten über der Workbench.
pub(crate) const AGENTS_MIN_ROWS: u16 = 3;

/// Ratatui-Fassung von `harw_tui_layout::ScreenLayout`: dieselbe
/// Platzierung, aber Rechtecke als `ratatui::layout::Rect` — dies ist die
/// einzige Stelle, an der `harw-tui` den dritten Rect-Typ aus
/// `harw-tui-layout` in einen eigenen umwandelt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScreenAreas {
    pub placement: harw_tui_layout::Placement,
    pub chat: Rect,
    pub agents: Option<Rect>,
    pub status: Rect,
    pub composer: Rect,
}

/// Klassifiziert `area` (Terminalgröße) mithilfe von
/// [`harw_tui_layout::classify`] und wandelt die zurückgegebenen Rechtecke
/// in `ratatui::layout::Rect` um. `status_rows`/`composer_rows` sind exakt
/// die Zeilen, die die aufrufende Seite heute für Statuszeile
/// (`harw-tui/src/app.rs:9552`, immer 1) und Composer (die dort berechnete
/// `input_height`) reserviert, damit die Fallback-Formel byte-identisch zu
/// heute bleibt (siehe `harw-tui-layout/src/placement.rs`, Funktion
/// `fallback`).
pub(crate) fn classify_screen(area: Rect, status_rows: u16, composer_rows: u16) -> ScreenAreas {
    let out = harw_tui_layout::classify(harw_tui_layout::LayoutInput {
        cols: area.width,
        rows: area.height,
        status_rows,
        composer_rows,
    });
    let to_rect = |r: harw_tui_layout::Rect| Rect {
        x: area.x.saturating_add(r.x),
        y: area.y.saturating_add(r.y),
        width: r.width,
        height: r.height,
    };
    ScreenAreas {
        placement: out.placement,
        chat: to_rect(out.chat),
        agents: out.agents.map(to_rect),
        status: to_rect(out.status),
        composer: to_rect(out.composer),
    }
}

/// Zielflächen des Portrait-Docks, bereits als `ratatui::layout::Rect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DockAreas {
    /// Eine kombinierte Liste (schmaler Dock).
    Combined(Rect),
    /// Agenten links, Jobs rechts (breiter Dock).
    Split { agents: Rect, jobs: Rect },
}

/// Teilt den Dock-Bereich gemäß [`harw_tui_layout::split_dock`] und wandelt
/// das Ergebnis in [`DockAreas`] um.
pub(crate) fn split_dock_areas(dock: Rect) -> DockAreas {
    let to_layout_rect = |r: Rect| harw_tui_layout::Rect {
        x: r.x,
        y: r.y,
        width: r.width,
        height: r.height,
    };
    let to_rect = |r: harw_tui_layout::Rect| Rect {
        x: r.x,
        y: r.y,
        width: r.width,
        height: r.height,
    };
    match harw_tui_layout::split_dock(to_layout_rect(dock)) {
        harw_tui_layout::DockSplit::Combined(r) => DockAreas::Combined(to_rect(r)),
        harw_tui_layout::DockSplit::Split { agents, jobs } => DockAreas::Split {
            agents: to_rect(agents),
            jobs: to_rect(jobs),
        },
    }
}

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
    let right_visible = (state.agents_visible || state.workbench_visible)
        && area.width >= AGENTS_PANEL_MIN_TERMINAL_WIDTH;
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

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    /// Cross-check gegen die heutige `split`-Geometrie: `classify_screen`
    /// darf für breite Terminals keine andere Aufteilung liefern.
    #[test]
    fn classify_screen_reproduces_wide_side_geometry_at_160x40_120x30_100x30() -> TestResult {
        for &(cols, rows) in &[(160u16, 40u16), (120, 30), (100, 30)] {
            let out = classify_screen(Rect::new(0, 0, cols, rows), 1, 3);
            assert_eq!(out.placement, harw_tui_layout::Placement::WideSide);
            let body_rows = rows - (1 + 3);
            let expected = split(Rect::new(0, 0, cols, body_rows), &PanelState::default());
            assert_eq!(
                out.agents.map(|r| r.width),
                expected.agents.map(|r| r.width)
            );
            assert_eq!(out.chat.width, expected.chat.ok_or("expected chat")?.width);
            let chat = out.chat;
            assert_eq!(chat.width, cols - AGENTS_WIDTH);
        }
        Ok(())
    }

    #[test]
    fn classify_screen_places_the_dock_for_80x40_and_wide_side_for_160x40() -> TestResult {
        let dock = classify_screen(Rect::new(0, 0, 80, 40), 1, 3);
        assert!(matches!(
            dock.placement,
            harw_tui_layout::Placement::PortraitDock { .. }
        ));
        let agents = dock.agents.ok_or("PortraitDock must have an agents rect")?;
        assert!(agents.height > 0);

        let wide = classify_screen(Rect::new(0, 0, 160, 40), 1, 3);
        assert_eq!(wide.placement, harw_tui_layout::Placement::WideSide);
        Ok(())
    }

    #[test]
    fn classify_screen_offsets_rects_by_the_input_area_origin() -> TestResult {
        let origin = classify_screen(Rect::new(0, 0, 80, 40), 1, 3);
        let shifted = classify_screen(Rect::new(5, 2, 80, 40), 1, 3);
        assert_eq!(shifted.chat.x, origin.chat.x + 5);
        assert_eq!(shifted.chat.y, origin.chat.y + 2);
        assert_eq!(shifted.chat.width, origin.chat.width);
        assert_eq!(shifted.chat.height, origin.chat.height);
        assert_eq!(shifted.status.x, origin.status.x + 5);
        assert_eq!(shifted.status.y, origin.status.y + 2);
        assert_eq!(shifted.composer.x, origin.composer.x + 5);
        assert_eq!(shifted.composer.y, origin.composer.y + 2);
        match (shifted.agents, origin.agents) {
            (Some(s), Some(o)) => {
                assert_eq!(s.x, o.x + 5);
                assert_eq!(s.y, o.y + 2);
                assert_eq!(s.width, o.width);
                assert_eq!(s.height, o.height);
            }
            (None, None) => {}
            _ => return Err("agents presence must not depend on the origin".into()),
        }
        Ok(())
    }

    #[test]
    fn split_dock_areas_splits_at_68_and_combines_below_it() -> TestResult {
        let dock = Rect::new(0, 0, 68, 12);
        match split_dock_areas(dock) {
            DockAreas::Split { agents, jobs } => {
                assert_eq!(agents.width + jobs.width, dock.width);
                assert_eq!(jobs.x, dock.x + agents.width);
            }
            other => return Err(format!("expected Split, got {other:?}").into()),
        }

        let dock = Rect::new(0, 0, 67, 12);
        match split_dock_areas(dock) {
            DockAreas::Combined(r) => assert_eq!(r, dock),
            other => return Err(format!("expected Combined, got {other:?}").into()),
        }
        Ok(())
    }

    #[test]
    fn narrow_terminals_keep_only_the_chat() {
        let areas = split(Rect::new(0, 0, 80, 20), &PanelState::default());
        assert!(areas.agents.is_none());
        assert_eq!(areas.chat.map(|r| r.width), Some(80));
    }

    /// Unter der Schwelle klappt die rechte Spalte weg, auch wenn das alte
    /// Budget (Chat 48 + Panel 44 = 92) noch gereicht hätte.
    #[test]
    fn right_column_collapses_below_the_width_threshold() {
        for width in [60u16, 80, 92, AGENTS_PANEL_MIN_TERMINAL_WIDTH - 1] {
            let areas = split(Rect::new(0, 0, width, 30), &PanelState::default());
            assert!(areas.agents.is_none(), "{width}");
            assert_eq!(areas.chat.map(|r| r.width), Some(width));
        }
        let areas = split(
            Rect::new(0, 0, AGENTS_PANEL_MIN_TERMINAL_WIDTH, 30),
            &PanelState::default(),
        );
        assert_eq!(areas.agents.map(|r| r.width), Some(AGENTS_WIDTH));
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
