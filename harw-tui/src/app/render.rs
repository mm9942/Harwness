//! Body composition of the viewport (PL-97 C2, §3.3.3 "Refactor boundary").
//!
//! [`compose_body`] lays out and draws everything between the overlays and
//! the status line: in the portrait stack the Mission Control windows (Active
//! Agents, Active Jobs, Finished / Failed, LIVE) above the full-width chat,
//! otherwise the wide side column or the one-line summary exactly as before.
//! It returns the chat, status and composer areas so `render_viewport` reads
//! overlay → body → status → composer. The wide and summary placements stay
//! unchanged here; the landscape anchors are PL-97 C3.

use std::time::Instant;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::{ChatApp, render_explorer_panel, scroll_routing};
use crate::mission::agents_window::{self, AgentsArea};
use crate::mission::{finished_window, jobs_window, live_window, projection};
use crate::panes::{MissionPresence, MissionRows, PaneFocus, PanelState};
use crate::style;

/// Heights `render_viewport` reserves below the body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BodyRows {
    /// Status line rows.
    pub(super) status_rows: u16,
    /// Composer rows (or the height of the dialog replacing it).
    pub(super) input_height: u16,
    /// Rows the chat keeps at least next to the queue block.
    pub(super) history_min: u16,
    /// A dialog replaces the composer (mouse routing).
    pub(super) dialog_open: bool,
}

/// Areas the rest of `render_viewport` draws into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct BodyAreas {
    /// Chat history.
    pub(super) history: Rect,
    /// Status line(s).
    pub(super) status: Rect,
    /// Composer or the dialog replacing it.
    pub(super) input: Rect,
}

/// Lays out and draws the body (windows, queue block, explorer/workbench)
/// and records the mouse regions of this frame.
pub(super) fn compose_body(
    frame: &mut Frame<'_>,
    app: &ChatApp,
    queue_lines: Vec<Line<'static>>,
    rows: BodyRows,
) -> BodyAreas {
    // Klassifiziert die Terminalgröße einmal pro Bild (siehe
    // `panes::classify_screen`): breite Seitenspalte (`WideSide`), oben
    // angedockter Portrait-Dock (`PortraitDock`) oder Zusammenfassungs-/
    // Kompakt-Fallback. `status_rows` und `composer_rows` (`input_height`)
    // sind exakt die Zeilen, die Statuszeile bzw. Composer reservieren.
    let area = frame.area();
    let screen = crate::panes::classify_screen(area, rows.status_rows, rows.input_height);
    let portrait_screen = matches!(
        screen.placement,
        harw_tui_layout::Placement::PortraitDock { .. }
    );
    if portrait_screen && app.panels.agents_visible && !app.panels.maximized {
        compose_portrait(frame, app, queue_lines, rows)
    } else {
        // Ausgeblendete (F3) oder maximierte Fenster: merken, was das
        // Hochformat nach dem Einblenden zeigt (`Alt+J`/`Alt+F`).
        let revealed = if portrait_screen {
            revealed_presence(app, area, rows)
        } else {
            MissionPresence::default()
        };
        compose_side_or_summary(frame, app, queue_lines, rows, revealed)
    }
}

/// Mission Control windows of the portrait stack once `F3`/`F11` show it
/// again (PL-97 C2): the same geometry as [`compose_portrait`] with the
/// windows visible, the LIVE body left out (it never decides whether the
/// card row or the Finished strip fit).
fn revealed_presence(app: &ChatApp, area: Rect, rows: BodyRows) -> MissionPresence {
    let panels = PanelState {
        agents_visible: true,
        maximized: false,
        ..app.panels.clone()
    };
    let finished_rows =
        projection::terminal_aggregate(&app.agent_monitor, app.agent_monitor.jobs());
    let areas = crate::panes::classify_mission(
        area,
        &panels,
        MissionRows {
            status: rows.status_rows,
            composer: rows.input_height,
            live_body_lines: 0,
            finished: finished_window::rows_wanted(&finished_rows, app.mission.finished_expanded),
        },
    );
    MissionPresence {
        jobs: areas.jobs.is_some(),
        finished: areas.finished.is_some(),
        live: false,
        agents_hidden: areas.agents.is_none(),
    }
}

/// Portrait stack (PL-97 C2, W02): Active Agents | Active Jobs (one combined
/// window below 68 columns), the Finished / Failed strip, the LIVE strip and
/// the chat over the full width. Modal dialogs and approval questions are
/// drawn later into the status/composer areas, so they always lie over
/// (after) the windows, never inside them.
fn compose_portrait(
    frame: &mut Frame<'_>,
    app: &ChatApp,
    queue_lines: Vec<Line<'static>>,
    rows: BodyRows,
) -> BodyAreas {
    let theme = app.theme;
    let area = frame.area();
    let now = Instant::now();
    // Inhalt zuerst: Zeilenwunsch von LIVE und Finished/Failed bestimmen die
    // Geometrie mit (der Chat behält trotzdem immer zuerst seinen Platz).
    let source = app.mission.live_source(&app.agent_monitor, now);
    let tool_started = source.and_then(|(live, _)| app.mission.tool_started(live));
    let live = live_window::view(source, tool_started, &app.recency, area.width, theme, now);
    let jobs = app.agent_monitor.jobs();
    let finished_rows = projection::terminal_aggregate(&app.agent_monitor, jobs);
    let areas = crate::panes::classify_mission(
        area,
        &app.panels,
        MissionRows {
            status: rows.status_rows,
            composer: rows.input_height,
            live_body_lines: live.body_lines_wanted(),
            finished: finished_window::rows_wanted(&finished_rows, app.mission.finished_expanded),
        },
    );

    // Warteschlange zwischen Chat und Statuszeile; der Chat behält
    // `history_min` Zeilen.
    let queue_height = u16::try_from(queue_lines.len())
        .unwrap_or(u16::MAX)
        .min(areas.chat.height.saturating_sub(rows.history_min));
    let history = Rect {
        height: areas.chat.height.saturating_sub(queue_height),
        ..areas.chat
    };
    if queue_height > 0 {
        let queue_area = Rect {
            y: history.y.saturating_add(history.height),
            height: queue_height,
            ..areas.chat
        };
        frame.render_widget(
            Paragraph::new(queue_lines).style(Style::default().fg(style::border_color(theme))),
            queue_area,
        );
    }

    let focus = app.panels.focus;
    let buf = frame.buffer_mut();
    // Mausziele der Kartenzeile: eine offene Detailansicht verdeckt das
    // Jobs-Fenster, das Rad über ihr gehört dann ganz der Detailansicht.
    let mut agents_region = areas.agents;
    let mut jobs_region = areas.jobs;
    match (app.agent_detail.as_ref(), areas.agents) {
        // Detailansicht ohne Vollbild (F11 in der Detailansicht): sie
        // ersetzt die Kartenzeile, wie früher den Dock.
        (Some(detail), Some(agents)) => {
            let cards = areas.jobs.map_or(agents, |jobs| agents.union(jobs));
            agents_region = Some(cards);
            jobs_region = None;
            if detail.job.is_some() {
                crate::jobs_panel::render_job_detail(
                    detail.job_view.as_ref(),
                    cards,
                    buf,
                    &detail.scroll,
                    theme,
                );
            } else {
                app.agent_monitor.render_agent_detail(
                    &detail.agent,
                    cards,
                    buf,
                    &detail.scroll,
                    detail.show_reasoning,
                );
            }
        }
        (None, Some(agents)) => {
            let agents_area = if areas.jobs.is_some() {
                AgentsArea::AgentsOnly(agents)
            } else {
                AgentsArea::Combined(agents)
            };
            agents_window::render(
                &app.agent_monitor,
                agents_area,
                buf,
                theme,
                focus == PaneFocus::Agents,
            );
            if let Some(jobs_area) = areas.jobs {
                jobs_window::render(
                    jobs,
                    jobs_area,
                    buf,
                    theme,
                    &app.mission.jobs,
                    focus == PaneFocus::Jobs,
                );
            }
        }
        (_, None) => {}
    }
    if let Some(finished_area) = areas.finished {
        finished_window::render(
            &finished_rows,
            finished_area,
            buf,
            theme,
            &app.mission.finished,
            focus == PaneFocus::Finished,
        );
    }
    if let Some(live_area) = areas.live {
        live_window::render(&live, live_area, buf, theme);
    }
    if let Some(explorer_area) = areas.explorer {
        render_explorer_panel(app, explorer_area, buf, theme);
    }
    if let Some(workbench_area) = areas.workbench {
        app.workbench
            .render(workbench_area, buf, theme, focus == PaneFocus::Workbench);
    }

    let presence = MissionPresence {
        jobs: jobs_region.is_some(),
        finished: areas.finished.is_some(),
        // Der LIVE-Streifen des Hochformats ist nie fokussierbar.
        live: false,
        agents_hidden: areas.agents.is_none(),
    };
    app.last_regions.set(scroll_routing::FrameRegions {
        agents: agents_region,
        jobs: jobs_region,
        finished: areas.finished,
        live: areas.live,
        dialog: rows.dialog_open.then_some(areas.composer),
        plan: app.plan_ui.covers_history().then_some(history),
        presence,
        // Sichtbar: nach dem Einblenden gilt dieselbe Lage (ohne die
        // Detailansicht, die beim Schließen das Jobs-Fenster freigibt).
        revealed: MissionPresence {
            jobs: areas.jobs.is_some(),
            ..presence
        },
    });
    BodyAreas {
        history,
        status: areas.status,
        input: areas.composer,
    }
}

/// Wide side column (agents/workbench right, explorer left) or, on narrow
/// terminals, the one-line agents summary above the status line -- the
/// pre-C2 composition, unchanged.
fn compose_side_or_summary(
    frame: &mut Frame<'_>,
    app: &ChatApp,
    queue_lines: Vec<Line<'static>>,
    rows: BodyRows,
    revealed: MissionPresence,
) -> BodyAreas {
    let theme = app.theme;
    let area = frame.area();
    let BodyRows {
        status_rows,
        input_height,
        history_min,
        dialog_open,
    } = rows;
    // Schmale Terminals: das Agenten-Panel klappt zu einer Zeile über der
    // Statuszeile zusammen (ab `AGENTS_SUMMARY_MIN_HEIGHT` Zeilen, darunter
    // entfällt es ganz), damit der Chat seine Breite behält.
    let agents_collapsed = app.panels.agents_visible
        && !app.panels.maximized
        && area.width < crate::panes::AGENTS_PANEL_MIN_TERMINAL_WIDTH;
    let agents_summary = if agents_collapsed
        && area.height >= crate::panes::AGENTS_SUMMARY_MIN_HEIGHT
        && area.height > input_height + history_min + 2
    {
        crate::agent_monitor::render_collapsed_summary(
            &app.agent_monitor,
            area.width,
            theme,
            app.panels.focus == PaneFocus::Agents,
        )
    } else {
        None
    };
    let summary_height = u16::from(agents_summary.is_some());

    // Der Block darf den Verlauf nie ganz verdrängen.
    let queue_height = u16::try_from(queue_lines.len()).unwrap_or(u16::MAX).min(
        area.height
            .saturating_sub(input_height + summary_height + 4),
    );

    // History | Warteschlange | permanente Statuszeile | Eingabe. Der
    // Sicherheitsmodus muss sichtbar bleiben und darf nicht vom Verlauf
    // verdrängt werden.
    let [body_area, queue_area, summary_area, status_area, input_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(history_min),
            Constraint::Length(queue_height),
            Constraint::Length(summary_height),
            Constraint::Length(status_rows),
            Constraint::Length(input_height),
        ])
        .areas(area);
    if let Some(summary) = agents_summary {
        frame.render_widget(Paragraph::new(summary), summary_area);
    }
    if queue_height > 0 {
        frame.render_widget(
            Paragraph::new(queue_lines).style(Style::default().fg(style::border_color(theme))),
            queue_area,
        );
    }
    // Seitenpanels (Explorer links, Agenten rechts) teilen sich die obere
    // Fläche mit dem Verlauf; auf schmalen Terminals bleibt nur der Chat.
    let pane_areas = crate::panes::split(body_area, &app.panels);
    let history_area = pane_areas
        .chat
        .unwrap_or(Rect::new(body_area.x, body_area.y, 0, 0));
    app.last_regions.set(scroll_routing::FrameRegions {
        agents: pane_areas.agents,
        dialog: dialog_open.then_some(input_area),
        plan: app.plan_ui.covers_history().then_some(history_area),
        revealed,
        ..scroll_routing::FrameRegions::default()
    });
    if let Some(agents_area) = pane_areas.agents {
        if let Some(detail) = app.agent_detail.as_ref() {
            if detail.job.is_some() {
                // Plan R9, Teil F: Job-Detailansicht (Kopf und Log-Ende).
                crate::jobs_panel::render_job_detail(
                    detail.job_view.as_ref(),
                    agents_area,
                    frame.buffer_mut(),
                    &detail.scroll,
                    theme,
                );
            } else {
                app.agent_monitor.render_agent_detail(
                    &detail.agent,
                    agents_area,
                    frame.buffer_mut(),
                    &detail.scroll,
                    detail.show_reasoning,
                );
            }
        } else {
            crate::agent_monitor::render_agents_panel(
                &app.agent_monitor,
                agents_area,
                frame.buffer_mut(),
                theme,
                app.panels.focus == PaneFocus::Agents,
            );
        }
    }
    if let Some(explorer_area) = pane_areas.explorer {
        render_explorer_panel(app, explorer_area, frame.buffer_mut(), theme);
    }
    if let Some(workbench_area) = pane_areas.workbench {
        app.workbench.render(
            workbench_area,
            frame.buffer_mut(),
            theme,
            app.panels.focus == PaneFocus::Workbench,
        );
    }
    BodyAreas {
        history: history_area,
        status: status_area,
        input: input_area,
    }
}
