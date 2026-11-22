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
//! `ratatui::layout::Rect` um, [`classify_mission`] entsprechend die
//! Mission-Control-Geometrie über [`harw_tui_layout::classify_mission`]
//! (PL-97 C2/C3: Hochformat-Stapel, dreispaltiges Querformat, flaches
//! Querformat mit Agenten über Jobs in der Seitenspalte, Kompakt-Fallback).
//! [`split`] und `split_right_column` beschreiben nur noch die
//! Seitenspalten-Darstellung eines maximierten Panels (`F11`,
//! Detailansicht) und des Kompakt-Fallbacks; `F4` läuft in der Reihenfolge
//! [`crate::mission::FOCUS_ORDER`] über alle sichtbaren Flächen.

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
///
/// Seit PL-97 C3 klassifiziert das Bild über [`classify_mission`]; diese
/// Fassung bleibt als Gegenprobe der bisherigen Klassifikation in den Tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
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
/// `fallback`). Seit PL-97 C3 nur noch Gegenprobe in den Tests (siehe
/// [`ScreenAreas`]).
#[cfg_attr(not(test), allow(dead_code))]
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

/// Rows the caller reserves or wants for [`classify_mission`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MissionRows {
    /// Status line rows above the composer.
    pub status: u16,
    /// Composer rows (or the height of the dialog that replaces it).
    pub composer: u16,
    /// LIVE body lines the content can fill (0 = header only).
    pub live_body_lines: u16,
    /// Finished/Failed rows: 1 collapsed, more when expanded.
    pub finished: u16,
    /// Cell width/height x 1000 (0 = unknown -> layout default).
    pub cell_aspect_permille: u16,
}

/// Ratatui form of [`harw_tui_layout::MissionLayout`]: the same placement,
/// rects as `ratatui::layout::Rect` offset by the input area's origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MissionAreas {
    /// Chosen Mission Control arrangement.
    pub placement: harw_tui_layout::MissionPlacement,
    /// Chat area (always present, possibly empty).
    pub chat: Rect,
    /// Active Agents window; with `jobs == None` it holds the combined
    /// agents-and-jobs list.
    pub agents: Option<Rect>,
    /// Active Jobs window.
    pub jobs: Option<Rect>,
    /// Finished/Failed window or strip.
    pub finished: Option<Rect>,
    /// LIVE window or strip (header plus body lines).
    pub live: Option<Rect>,
    /// LIVE body lines below the header (0 for a header-only strip).
    pub live_body_lines: u16,
    /// Explorer column left of the chat.
    pub explorer: Option<Rect>,
    /// Workbench (side-column placements only; never in the portrait stack).
    pub workbench: Option<Rect>,
    /// Status row(s).
    pub status: Rect,
    /// Composer (or the dialog replacing it).
    pub composer: Rect,
}

/// Classifies `area` with [`harw_tui_layout::classify_mission`] for the
/// visible panels of `state` and converts the result into
/// [`MissionAreas`] (PL-97 §3.3.2/§3.3.3).
pub(crate) fn classify_mission(area: Rect, state: &PanelState, rows: MissionRows) -> MissionAreas {
    let layout = harw_tui_layout::classify_mission(harw_tui_layout::MissionInput {
        cols: area.width,
        rows: area.height,
        cell_aspect_permille: rows.cell_aspect_permille,
        status_rows: rows.status,
        composer_rows: rows.composer,
        live_body_lines_wanted: rows.live_body_lines,
        finished_rows_wanted: rows.finished,
        agents_visible: state.agents_visible,
        workbench_visible: state.workbench_visible,
        explorer_visible: state.explorer_visible,
    });
    let to_rect = |r: harw_tui_layout::Rect| Rect {
        x: area.x.saturating_add(r.x),
        y: area.y.saturating_add(r.y),
        width: r.width,
        height: r.height,
    };
    MissionAreas {
        placement: layout.placement,
        chat: to_rect(layout.chat),
        agents: layout.agents.map(to_rect),
        jobs: layout.jobs.map(to_rect),
        finished: layout.finished.map(to_rect),
        live: layout.live.map(to_rect),
        live_body_lines: match layout.live_mode {
            harw_tui_layout::LiveMode::Body { lines } => lines,
            harw_tui_layout::LiveMode::Header | harw_tui_layout::LiveMode::Hidden => 0,
        },
        explorer: layout.explorer.map(to_rect),
        workbench: layout.workbench.map(to_rect),
        status: to_rect(layout.status),
        composer: to_rect(layout.composer),
    }
}

/// Which Mission Control windows were on screen in the last frame (from
/// `FrameRegions`); [`PanelState`] only focuses visible windows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct MissionPresence {
    /// A separate Active Jobs window exists.
    pub jobs: bool,
    /// The Finished/Failed window exists.
    pub finished: bool,
    /// The LIVE window is focusable (expanded, outside the portrait stack).
    pub live: bool,
    /// The portrait stack is drawn without its card row (the chat keeps its
    /// floor on a short body): the agents list is not on screen.
    pub agents_hidden: bool,
}

/// Welche Fläche Tastatureingaben bekommt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum PaneFocus {
    #[default]
    Chat,
    Explorer,
    Agents,
    Workbench,
    /// The Active Jobs window (Mission Control, PL-97 C2).
    Jobs,
    /// The Finished/Failed window.
    Finished,
    /// The LIVE window (focusable only while expanded outside the portrait
    /// stack).
    Live,
}

impl PaneFocus {
    /// `true` for the agents panel and every Mission Control window: they
    /// disappear together with `F3`.
    #[must_use]
    pub(crate) fn is_mission(self) -> bool {
        matches!(
            self,
            Self::Agents | Self::Jobs | Self::Finished | Self::Live
        )
    }
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
    /// Mission Control windows of the last frame (focus targets).
    pub mission: MissionPresence,
    /// Mission Control windows the portrait stack shows with the windows on
    /// screen and no detail view covering the card row -- also after `F3`
    /// hid them or while a panel is maximized. `Alt+J`/`Alt+F` decide by it,
    /// so they never act on the stale presence of a frame without windows.
    pub revealed: MissionPresence,
}

impl Default for PanelState {
    fn default() -> Self {
        Self {
            agents_visible: true,
            explorer_visible: false,
            workbench_visible: false,
            maximized: false,
            focus: PaneFocus::Chat,
            mission: MissionPresence::default(),
            revealed: MissionPresence::default(),
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
    /// Workbench, `F4` Fokus, `Ctrl+E` Explorer fokussieren, `Alt+J` Jobs,
    /// `Alt+F` Finished/Failed, `F11` Vollbild des fokussierten Panels.
    /// `Esc` (nicht umbelegbar) gibt den Fokus aus dem Agenten-, einem
    /// Mission- oder dem Workbench-Fenster an den Chat zurück.
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
                if !self.agents_visible && self.focus.is_mission() {
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
            // PL-97 C2: ohne eigenes Fenster (kombinierte Liste, breite
            // Seitenspalte) landen Jobs und Finished/Failed im Agenten-Panel.
            Some(KeyAction::FocusJobs) => {
                let presence = self.revealed;
                self.agents_visible = true;
                self.maximized = false;
                self.focus = if presence.jobs {
                    PaneFocus::Jobs
                } else {
                    Self::agents_or_chat(presence)
                };
                return PanelKey::Changed;
            }
            Some(KeyAction::FocusFinished) => {
                let presence = self.revealed;
                self.agents_visible = true;
                self.maximized = false;
                self.focus = if presence.finished {
                    PaneFocus::Finished
                } else {
                    Self::agents_or_chat(presence)
                };
                return PanelKey::Changed;
            }
            // Vollbild nur für die klassischen Panels; die Mission-Fenster
            // öffnen ihre Details selbst (Enter).
            Some(KeyAction::MaximizePanel)
                if matches!(
                    self.focus,
                    PaneFocus::Explorer | PaneFocus::Agents | PaneFocus::Workbench
                ) =>
            {
                self.maximized = !self.maximized;
                return PanelKey::Changed;
            }
            _ => {}
        }
        match key.code {
            // Im Explorer gehört Esc zuerst dem Panel (Filter/Vorschau
            // schließen); das Panel gibt den Fokus selbst zurück.
            KeyCode::Esc if self.focus.is_mission() || self.focus == PaneFocus::Workbench => {
                self.focus = PaneFocus::Chat;
                self.maximized = false;
                PanelKey::Changed
            }
            _ if self.focus != PaneFocus::Chat => PanelKey::ForFocused(key),
            _ => PanelKey::Ignored,
        }
    }

    /// Ersatzziel ohne eigenes Jobs- bzw. Finished-Fenster: das
    /// Agenten-Panel (kombinierte Liste, Seitenspalte), ohne sichtbare
    /// Kartenzeile der Chat.
    fn agents_or_chat(presence: MissionPresence) -> PaneFocus {
        if presence.agents_hidden {
            PaneFocus::Chat
        } else {
            PaneFocus::Agents
        }
    }

    /// Ob `focus` gerade ein sichtbares Ziel ist (der Chat immer).
    pub(crate) fn can_focus(&self, focus: PaneFocus) -> bool {
        match focus {
            PaneFocus::Chat => true,
            PaneFocus::Explorer => self.explorer_visible,
            PaneFocus::Agents => self.agents_visible && !self.mission.agents_hidden,
            PaneFocus::Workbench => self.workbench_visible,
            PaneFocus::Jobs => self.agents_visible && self.mission.jobs,
            PaneFocus::Finished => self.agents_visible && self.mission.finished,
            PaneFocus::Live => self.agents_visible && self.mission.live,
        }
    }

    fn cycle_focus(&mut self) {
        let order = crate::mission::FOCUS_ORDER;
        let start = order.iter().position(|f| *f == self.focus).unwrap_or(0);
        for step in 1..=order.len() {
            let Some(next) = order.get((start + step) % order.len()).copied() else {
                continue;
            };
            if self.can_focus(next) {
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
            // Mission-Fenster zeigen im Vollbild (Job-Detailansicht) die
            // Fläche des Agenten-Panels.
            PaneFocus::Agents | PaneFocus::Jobs | PaneFocus::Finished | PaneFocus::Live => {
                PaneAreas {
                    explorer: None,
                    chat: None,
                    agents: Some(area),
                    workbench: None,
                }
            }
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

    /// Reference rows: one status row, three composer rows, finished strip
    /// collapsed.
    fn mission_rows(live_body_lines: u16) -> MissionRows {
        MissionRows {
            status: 1,
            composer: 3,
            live_body_lines,
            finished: 1,
            cell_aspect_permille: 0,
        }
    }

    /// Portrait stack: the card row is always two bordered windows (agents
    /// | jobs) of equal height, also below 68 columns (never a combined list).
    #[test]
    fn split_dock_areas_always_split_into_two_windows_in_portrait() -> TestResult {
        let state = PanelState::default();
        for (cols, agents_width) in [(68u16, 34u16), (67, 33), (46, 23)] {
            let out = classify_mission(Rect::new(0, 0, cols, 40), &state, mission_rows(0));
            let agents = out.agents.ok_or("agents window")?;
            let jobs = out.jobs.ok_or("jobs window")?;
            assert_eq!(agents.width, agents_width, "cols={cols}");
            assert_eq!(agents.width + jobs.width, cols, "cols={cols}");
            assert_eq!(jobs.x, agents.x + agents.width, "cols={cols}");
            assert_eq!(
                (agents.y, agents.height),
                (jobs.y, jobs.height),
                "cols={cols}"
            );
        }
        let narrow = classify_mission(Rect::new(0, 0, 67, 40), &state, mission_rows(0));
        assert_eq!(narrow.agents, Some(Rect::new(0, 0, 33, 17)));
        assert_eq!(narrow.jobs, Some(Rect::new(33, 0, 34, 17)));
        Ok(())
    }

    /// 46x78 (reference portrait): squares (agents | jobs, 12 rows), the
    /// bordered Reasoning window (10 rows), the finished strip, then the chat
    /// with the rest; all rects follow the origin of the input area. The wanted
    /// body lines no longer influence the geometry.
    #[test]
    fn portrait_stack_at_46x78_places_squares_reasoning_finished_then_chat() -> TestResult {
        let state = PanelState::default();
        let max = harw_tui_layout::MINI_LIVE_MAX_BODY_LINES;
        for wanted in [0u16, 2, 99] {
            let out = classify_mission(Rect::new(0, 0, 46, 78), &state, mission_rows(wanted));
            assert_eq!(
                out.placement,
                harw_tui_layout::MissionPlacement::PortraitStack
            );
            assert_eq!(out.agents, Some(Rect::new(0, 0, 23, 12)), "w={wanted}");
            assert_eq!(out.jobs, Some(Rect::new(23, 0, 23, 12)), "w={wanted}");
            assert_eq!(out.live, Some(Rect::new(0, 12, 46, 10)), "w={wanted}");
            assert_eq!(out.live_body_lines, max, "w={wanted}");
            assert_eq!(out.finished, Some(Rect::new(0, 22, 46, 1)), "w={wanted}");
            assert_eq!(out.chat, Rect::new(0, 23, 46, 51), "w={wanted}");
            assert_eq!(out.status, Rect::new(0, 74, 46, 1), "w={wanted}");
            assert_eq!(out.composer, Rect::new(0, 75, 46, 3), "w={wanted}");
        }

        let shifted = classify_mission(Rect::new(3, 2, 46, 78), &state, mission_rows(2));
        assert_eq!(shifted.agents, Some(Rect::new(3, 2, 23, 12)));
        assert_eq!(shifted.live, Some(Rect::new(3, 14, 46, 10)));
        assert_eq!(shifted.finished, Some(Rect::new(3, 24, 46, 1)));
        assert_eq!(shifted.chat, Rect::new(3, 25, 46, 51));
        Ok(())
    }

    /// PL-97 C3: im Querformat liefert [`classify_mission`] die Anker der
    /// drei Spalten (160×45: Chat 88 | Finished über LIVE 40 | Agenten über
    /// Jobs 32) bzw. das flache Querformat (100×30: Seitenspalte 44 mit
    /// Agenten über Jobs, Finished- und LIVE-Streifen über dem Chat), alles
    /// um den Ursprung der Eingabefläche verschoben.
    #[test]
    fn classify_mission_converts_the_landscape_anchors_and_the_shallow_stack() -> TestResult {
        let state = PanelState::default();
        let three = classify_mission(Rect::new(0, 0, 160, 45), &state, mission_rows(2));
        assert_eq!(
            three.placement,
            harw_tui_layout::MissionPlacement::LandscapeThreeColumns
        );
        assert_eq!(three.chat, Rect::new(0, 0, 88, 41));
        assert_eq!(three.finished, Some(Rect::new(88, 0, 40, 5)));
        assert_eq!(three.live, Some(Rect::new(88, 5, 40, 36)));
        assert_eq!(three.agents, Some(Rect::new(128, 0, 32, 21)));
        assert_eq!(three.jobs, Some(Rect::new(128, 21, 32, 20)));
        assert_eq!(three.status, Rect::new(0, 41, 160, 1));
        assert_eq!(three.composer, Rect::new(0, 42, 160, 3));

        let shifted = classify_mission(Rect::new(4, 1, 160, 45), &state, mission_rows(2));
        assert_eq!(shifted.live, Some(Rect::new(92, 6, 40, 36)));
        assert_eq!(shifted.jobs, Some(Rect::new(132, 22, 32, 20)));

        let shallow = classify_mission(Rect::new(0, 0, 100, 30), &state, mission_rows(2));
        assert_eq!(
            shallow.placement,
            harw_tui_layout::MissionPlacement::ShallowLandscape
        );
        let agents = shallow.agents.ok_or("shallow agents")?;
        let jobs = shallow.jobs.ok_or("shallow jobs")?;
        assert_eq!((agents.x, agents.width), (100 - AGENTS_WIDTH, AGENTS_WIDTH));
        assert_eq!((jobs.x, jobs.y), (agents.x, agents.y + agents.height));
        assert_eq!(shallow.finished, Some(Rect::new(0, 0, 56, 1)));
        assert_eq!(shallow.live, Some(Rect::new(0, 1, 56, 1)));
        assert_eq!(shallow.live_body_lines, 0, "header-only LIVE strip");
        assert_eq!(shallow.chat, Rect::new(0, 2, 56, 24));
        Ok(())
    }

    /// The chat keeps its floor: when rows are short the squares (min 5) win
    /// first, then the Reasoning window (min 3), then the finished strip; none
    /// of them ever takes a row below the chat floor.
    #[test]
    fn portrait_stack_gives_the_chat_its_floor_before_squares_reasoning_and_finished() -> TestResult
    {
        let floor = harw_tui_layout::MINI_LIVE_MIN_CHAT_ROWS;
        let max = harw_tui_layout::MINI_LIVE_MAX_BODY_LINES;
        let state = PanelState::default();
        for height in 28..=60u16 {
            for composer in 3..=24u16 {
                let rows = MissionRows {
                    status: 1,
                    composer,
                    live_body_lines: 0,
                    finished: 1,
                    cell_aspect_permille: 0,
                };
                let out = classify_mission(Rect::new(0, 0, 46, height), &state, rows);
                let ctx = format!("h={height} c={composer}");
                assert_eq!(out.composer.height, composer.min(height), "{ctx}");
                let Some(strip) = out.live else {
                    continue;
                };
                assert!((3..=10).contains(&strip.height), "{ctx}");
                assert_eq!(out.live_body_lines, (strip.height - 2).min(max), "{ctx}");
                let agents = out.agents.ok_or("squares come before the live window")?;
                assert_eq!(strip.y, agents.y + agents.height, "{ctx}");
                assert!(agents.height >= 5, "{ctx}");
                let mut below = strip.y + strip.height;
                if let Some(finished) = out.finished {
                    assert_eq!(finished.y, below, "{ctx}");
                    below += finished.height;
                }
                assert_eq!(out.chat.y, below, "{ctx}");
                assert!(out.chat.height >= floor, "{ctx}");
                if strip.height < 10 {
                    assert_eq!(out.chat.height, floor, "live grows before the chat: {ctx}");
                }
            }
        }
        // Exactly at the edges (chat 8 + squares 5 = 13 body rows): 13 rows
        // carry only the squares, 16 add the Reasoning window (3 rows), 17 the
        // finished strip.
        let at = |body: u16| {
            let rows = MissionRows {
                status: 1,
                composer: 28 - 1 - body,
                live_body_lines: 2,
                finished: 1,
                cell_aspect_permille: 0,
            };
            classify_mission(Rect::new(0, 0, 46, 28), &state, rows)
        };
        assert_eq!(at(12).agents, None);
        assert_eq!(at(13).agents.map(|r| r.height), Some(5));
        assert_eq!(at(13).live, None);
        assert_eq!(at(15).live, None);
        assert_eq!(at(16).live.map(|r| r.height), Some(3));
        assert_eq!(at(16).finished, None);
        assert_eq!(at(17).finished.map(|r| r.height), Some(1));
        Ok(())
    }

    /// PL-97 §3.3.3: `F4` läuft Chat → Agenten → Jobs → Finished → (LIVE nur
    /// fokussierbar) → Explorer → Workbench → Chat; nicht sichtbare
    /// Mission-Fenster werden übersprungen; `Esc` gibt an den Chat zurück.
    #[test]
    fn focus_cycle_visits_the_mission_windows_in_plan_order() {
        let bindings = KeyBindings::default();
        let mut state = PanelState {
            explorer_visible: true,
            workbench_visible: true,
            mission: MissionPresence {
                jobs: true,
                finished: true,
                ..MissionPresence::default()
            },
            ..PanelState::default()
        };
        let mut seen = Vec::new();
        for _ in 0..6 {
            state.handle_key(key(KeyCode::F(4)), &bindings);
            seen.push(state.focus);
        }
        assert_eq!(
            seen,
            vec![
                PaneFocus::Agents,
                PaneFocus::Jobs,
                PaneFocus::Finished,
                PaneFocus::Explorer,
                PaneFocus::Workbench,
                PaneFocus::Chat,
            ]
        );
        state.mission.live = true;
        state.focus = PaneFocus::Finished;
        state.handle_key(key(KeyCode::F(4)), &bindings);
        assert_eq!(
            state.focus,
            PaneFocus::Live,
            "focusable LIVE joins the cycle"
        );

        state.focus = PaneFocus::Jobs;
        assert!(matches!(
            state.handle_key(key(KeyCode::F(11)), &bindings),
            PanelKey::ForFocused(_)
        ));
        assert!(!state.maximized, "no full screen for mission windows");
        assert_eq!(
            state.handle_key(key(KeyCode::Esc), &bindings),
            PanelKey::Changed
        );
        assert_eq!(state.focus, PaneFocus::Chat);
    }

    /// `Alt+J`/`Alt+F` fokussieren Jobs bzw. Finished/Failed; ohne eigenes
    /// Fenster landen sie im Agenten-Panel. `F3` gibt den Fokus zurück; sind
    /// die Fenster ausgeblendet, entscheidet die Lage nach dem Einblenden
    /// ([`PanelState::revealed`]), nicht das letzte Bild ohne Fenster.
    #[test]
    fn alt_j_and_alt_f_focus_jobs_and_finished() {
        let bindings = KeyBindings::default();
        let alt = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT);
        let windows = MissionPresence {
            jobs: true,
            finished: true,
            ..MissionPresence::default()
        };
        let mut state = PanelState::default();
        assert_eq!(state.handle_key(alt('j'), &bindings), PanelKey::Changed);
        assert_eq!(
            state.focus,
            PaneFocus::Agents,
            "no jobs window: agents panel"
        );
        state.mission = windows;
        state.revealed = windows;
        state.handle_key(alt('j'), &bindings);
        assert_eq!(state.focus, PaneFocus::Jobs);
        state.handle_key(alt('f'), &bindings);
        assert_eq!(state.focus, PaneFocus::Finished);
        state.handle_key(key(KeyCode::F(3)), &bindings);
        assert!(!state.agents_visible);
        assert_eq!(state.focus, PaneFocus::Chat, "hiding releases the focus");

        // Das Bild ohne Fenster kennt keine Mission-Fenster mehr; das
        // Einblenden richtet sich nach `revealed`.
        state.mission = MissionPresence::default();
        state.revealed = windows;
        state.handle_key(alt('f'), &bindings);
        assert!(state.agents_visible, "Alt+F shows the windows again");
        assert_eq!(state.focus, PaneFocus::Finished);
        state.handle_key(key(KeyCode::F(3)), &bindings);
        state.handle_key(alt('j'), &bindings);
        assert_eq!(state.focus, PaneFocus::Jobs, "not the stale agents panel");

        // Ohne Kartenzeile (kurzer Körper) gibt es kein Agenten-Ziel.
        state.mission = MissionPresence {
            finished: true,
            agents_hidden: true,
            ..MissionPresence::default()
        };
        state.revealed = state.mission;
        state.focus = PaneFocus::Chat;
        state.handle_key(alt('j'), &bindings);
        assert_eq!(state.focus, PaneFocus::Chat, "no visible agents list");
        assert!(!state.can_focus(PaneFocus::Agents));
        state.handle_key(key(KeyCode::F(4)), &bindings);
        assert_eq!(state.focus, PaneFocus::Finished, "F4 skips the hidden list");
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
