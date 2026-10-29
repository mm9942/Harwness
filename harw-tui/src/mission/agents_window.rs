//! Active Agents window (PL-97 C2).
//!
//! The window keeps the legacy state of [`AgentMonitor`] -- its selection
//! (`selected`) and scroll offset (`panel_scroll`) -- and renders through the
//! legacy dock renderer, so every existing agent-list key (`j/k/w/s`,
//! PgUp/PgDn, Home/End, `f`, `c`, Enter) behaves exactly as before:
//!
//! - [`AgentsArea::Combined`]: one bordered list of agents **and** jobs
//!   (portrait stack below `PHONE_DOCK_SPLIT_MIN_COLS`, the legacy combined
//!   dock);
//! - [`AgentsArea::AgentsOnly`]: the agents column of the legacy split dock;
//!   the jobs are in their own window ([`super::jobs_window`]), so the
//!   selection is kept on agent entries ([`settle_on_agent_entry`]).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::agent_monitor::{AgentMonitor, PanelEntry, PanelScope};
use crate::panes::DockAreas;
use crate::style::Theme;

/// Where and how the Agents window is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentsArea {
    /// Agents and jobs in one list (no separate Jobs window).
    Combined(Rect),
    /// Agents only; the jobs have their own window.
    AgentsOnly(Rect),
}

/// Draws the Agents window with the legacy monitor state, in `scope`
/// (PL-97 C4: active runs only when a Finished / Failed window is drawn in
/// the same frame, see [`PanelScope::for_cards`]).
pub(crate) fn render(
    monitor: &AgentMonitor,
    area: AgentsArea,
    scope: PanelScope,
    buf: &mut Buffer,
    theme: Theme,
    focused: bool,
) {
    let areas = match area {
        AgentsArea::Combined(rect) => DockAreas::Combined(rect),
        // Die Agenten-Spalte des geteilten Docks: die Jobs-Spalte ist leer
        // (Breite 0), ihr Renderer zeichnet dann nichts. So bleiben
        // Auswahl, gemeinsamer Scroll-Versatz und das Nachführen der
        // Auswahl exakt die des bisherigen Docks.
        AgentsArea::AgentsOnly(rect) => DockAreas::Split {
            agents: rect,
            jobs: Rect::new(rect.x.saturating_add(rect.width), rect.y, 0, 0),
        },
    };
    crate::agent_monitor::render_dock(monitor, areas, scope, buf, theme, focused);
}

/// `true` for entries that belong to the jobs group of the legacy list.
fn is_job_entry(entry: &PanelEntry<'_>) -> bool {
    matches!(entry, PanelEntry::Job { .. } | PanelEntry::JobsFinished)
}

/// Keeps the legacy selection on an agent entry while the jobs have their own
/// window: the jobs group always follows the agent entries in
/// [`AgentMonitor::panel_entries`], so a move onto a job entry wraps to the
/// first agent entry (`forward`) or stops at the last one.
pub(crate) fn settle_on_agent_entry(monitor: &mut AgentMonitor, forward: bool) {
    let entries = monitor.panel_entries();
    let agent_entries = entries
        .iter()
        .take_while(|entry| !is_job_entry(entry))
        .count();
    if agent_entries == 0 || monitor.selected < agent_entries {
        return;
    }
    monitor.selected = if forward { 0 } else { agent_entries - 1 };
}

/// `Alt+J` without a separate Jobs window: selects the first job entry of
/// the combined list (if any).
pub(crate) fn select_first_job(monitor: &mut AgentMonitor) -> bool {
    let first = monitor.panel_entries().iter().position(is_job_entry);
    match first {
        Some(index) => {
            monitor.selected = index;
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mission::projection::tests::job;
    use crate::mission::tests::thinking;
    use crate::test_support::{TestResult, ctx};

    fn monitor_with_jobs() -> AgentMonitor {
        let mut monitor = AgentMonitor::default();
        thinking(&mut monitor, "a1", "rolle-a", "");
        thinking(&mut monitor, "a2", "rolle-b", "");
        monitor.set_jobs(vec![job("j1", true, false, "läuft")]);
        monitor
    }

    fn rows(monitor: &AgentMonitor, area: AgentsArea, width: u16) -> TestResult<Vec<String>> {
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 4))
            .map_err(ctx("test terminal"))?;
        terminal
            .draw(|frame| {
                render(
                    monitor,
                    area,
                    PanelScope::ActiveOnly,
                    frame.buffer_mut(),
                    Theme::Dark,
                    true,
                )
            })
            .map_err(ctx("draw"))?;
        let buffer = terminal.backend().buffer();
        Ok((0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect())
    }

    #[test]
    fn agents_only_area_draws_no_job_rows() -> TestResult {
        let monitor = monitor_with_jobs();
        let only = rows(&monitor, AgentsArea::AgentsOnly(Rect::new(0, 0, 30, 4)), 40)?;
        let shown = only.join("\n");
        assert!(shown.contains("Agenten"), "{shown}");
        assert!(!shown.contains("name-j1"), "{shown}");
        let combined = rows(&monitor, AgentsArea::Combined(Rect::new(0, 0, 40, 4)), 40)?;
        assert!(
            combined.iter().any(|row| row.contains("Agenten")),
            "{combined:?}"
        );
        Ok(())
    }

    #[test]
    fn selection_settles_on_agent_entries_and_alt_j_finds_the_first_job() {
        let mut monitor = monitor_with_jobs();
        monitor.select_next();
        monitor.select_next();
        assert_eq!(monitor.selected, 2, "legacy order: a1, a2, j1");
        settle_on_agent_entry(&mut monitor, true);
        assert_eq!(monitor.selected, 0, "forward wraps to the first agent");
        monitor.select_prev();
        settle_on_agent_entry(&mut monitor, false);
        assert_eq!(monitor.selected, 1, "backward stops at the last agent");
        assert!(select_first_job(&mut monitor));
        assert_eq!(monitor.selected, 2);
        let mut empty = AgentMonitor::default();
        assert!(!select_first_job(&mut empty));
        settle_on_agent_entry(&mut empty, true);
        assert_eq!(empty.selected, 0);
    }
}
