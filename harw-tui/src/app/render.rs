//! Body composition of the viewport (PL-97 C2/C3, §3.3.3 "Refactor boundary").
//!
//! [`compose_body`] classifies the terminal once per frame with
//! [`crate::panes::classify_mission`] (every orientation) and lays out and
//! draws everything between the overlays and the status line:
//!
//! - **Portrait stack** (C2): Active Agents | Active Jobs (one combined
//!   window below 68 columns), the Finished / Failed strip and the LIVE strip
//!   above the full-width chat.
//! - **Three-column landscape** (C3, PL-94 anchors): five separately
//!   bordered windows -- the chat left and widest, Finished / Failed
//!   top-centre with LIVE below it, Active Agents above Active Jobs in the far
//!   right column; the explorer left of the chat and the workbench below the
//!   jobs where the geometry places them. LIVE is focusable and scrollable
//!   there and shows the agent detail in place (`Alt+L`).
//! - **Shallow landscape** (C3): today's wide side column with the agents
//!   stacked above the jobs, a one-row Finished strip and a header-only LIVE
//!   strip above the chat.
//! - **Compact summary**, a maximized panel (`F11`, detail view) and hidden
//!   windows in the portrait stack: the side column / one-line summary as
//!   before; the summary line leads with the `Agents n · Jobs n · Done
//!   ok/✗failed` counters (C3).
//!
//! It returns the chat, status and composer areas so `render_viewport` reads
//! overlay → body → status → composer, and records the mouse regions of the
//! frame ([`scroll_routing::FrameRegions`]).

use std::time::Instant;

use harw_tui_layout::MissionPlacement;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget};

use super::{ChatApp, render_explorer_panel, scroll_routing};
use crate::agent_monitor::AgentLive;
use crate::mission::agents_window::{self, AgentsArea};
use crate::mission::projection::{self, TerminalRow};
use crate::mission::{WindowUi, finished_window, jobs_window, live_window};
use crate::panes::{MissionAreas, MissionPresence, MissionRows, PaneFocus, PanelState};
use crate::status_line::fit_width;
use crate::style::{self, Theme};

/// Status hint while the workbench is switched on (`F5`) but the current
/// placement has no room for it (PL-97 C3).
pub(super) const WORKBENCH_HIDDEN_HINT: &str = " · Werkbank: zu wenig Platz";

/// Title of the chat window in the three-column landscape.
const CHAT_TITLE: &str = " Chat ";

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
    /// Status hint of this frame ([`WORKBENCH_HIDDEN_HINT`] or empty).
    pub(super) workbench_hint: &'static str,
}

/// What the Mission Control windows show in this frame; computed once and
/// shared by the geometry (wanted rows) and the renderers.
struct MissionContent<'a> {
    /// LIVE source agent and whether it is pinned.
    source: Option<(&'a AgentLive, bool)>,
    /// Start of the source's current tool call.
    tool_started: Option<Instant>,
    /// LIVE content for a strip as wide as the terminal.
    live: live_window::LiveView,
    /// Finished / Failed aggregate rows.
    finished: Vec<TerminalRow>,
    /// Reference time of the frame.
    now: Instant,
}

impl<'a> MissionContent<'a> {
    fn new(app: &'a ChatApp, width: u16, now: Instant) -> Self {
        let source = app.mission.live_source(&app.agent_monitor, now);
        let tool_started = source.and_then(|(live, _)| app.mission.tool_started(live));
        let live = live_window::view(source, tool_started, &app.recency, width, app.theme, now);
        let finished = projection::terminal_aggregate(&app.agent_monitor, app.agent_monitor.jobs());
        Self {
            source,
            tool_started,
            live,
            finished,
            now,
        }
    }

    /// Geometry input: the reserved bottom rows plus what LIVE and
    /// Finished / Failed want (LIVE body lines only count where they decide,
    /// see [`revealed_presence`]).
    fn mission_rows(&self, app: &ChatApp, rows: BodyRows, live_body_lines: u16) -> MissionRows {
        MissionRows {
            status: rows.status_rows,
            composer: rows.input_height,
            live_body_lines,
            finished: finished_window::rows_wanted(&self.finished, app.mission.finished_expanded),
        }
    }
}

/// Lays out and draws the body (windows, queue block, explorer/workbench)
/// and records the mouse regions of this frame.
pub(super) fn compose_body(
    frame: &mut Frame<'_>,
    app: &ChatApp,
    queue_lines: Vec<Line<'static>>,
    rows: BodyRows,
) -> BodyAreas {
    // Klassifiziert die Terminalgröße einmal pro Bild über
    // `classify_mission` (jede Ausrichtung): Hochformat-Stapel,
    // dreispaltiges Querformat, flaches Querformat (breite Seitenspalte)
    // oder Zusammenfassungs-/Kompakt-Fallback. `status_rows` und
    // `input_height` sind exakt die Zeilen, die Statuszeile bzw. Composer
    // reservieren.
    let area = frame.area();
    let content = MissionContent::new(app, area.width, Instant::now());
    let areas = crate::panes::classify_mission(
        area,
        &app.panels,
        content.mission_rows(app, rows, content.live.body_lines_wanted()),
    );
    let windows_shown = app.panels.agents_visible && !app.panels.maximized;
    match areas.placement {
        MissionPlacement::PortraitStack if windows_shown => {
            compose_portrait(frame, app, queue_lines, rows, &areas, &content)
        }
        MissionPlacement::LandscapeThreeColumns | MissionPlacement::ShallowLandscape
            if !app.panels.maximized =>
        {
            compose_columns(frame, app, queue_lines, rows, &areas, &content)
        }
        MissionPlacement::PortraitStack
        | MissionPlacement::LandscapeThreeColumns
        | MissionPlacement::ShallowLandscape
        | MissionPlacement::CompactSummary => {
            // Ausgeblendete (F3) oder maximierte Fenster: merken, was die
            // Fenster-Lage nach dem Einblenden zeigt (`Alt+J`/`Alt+F`/`Alt+L`).
            let revealed = if areas.placement == MissionPlacement::CompactSummary {
                MissionPresence::default()
            } else {
                revealed_presence(app, area, rows, &content)
            };
            compose_side_or_summary(frame, app, queue_lines, rows, revealed, &content.finished)
        }
    }
}

/// Mission Control windows once `F3`/`F11` show them again (PL-97 C2/C3):
/// the same geometry with the windows visible, the LIVE body left out (it
/// never decides whether the card row or the Finished strip fit). LIVE is a
/// focus target only as the landscape window.
fn revealed_presence(
    app: &ChatApp,
    area: Rect,
    rows: BodyRows,
    content: &MissionContent<'_>,
) -> MissionPresence {
    let panels = PanelState {
        agents_visible: true,
        maximized: false,
        ..app.panels.clone()
    };
    let areas = crate::panes::classify_mission(area, &panels, content.mission_rows(app, rows, 0));
    MissionPresence {
        jobs: areas.jobs.is_some(),
        finished: areas.finished.is_some(),
        live: areas.placement == MissionPlacement::LandscapeThreeColumns && areas.live.is_some(),
        agents_hidden: areas.agents.is_none(),
    }
}

/// Status hint when the workbench is on but `drawn` says it got no area.
fn workbench_hint(app: &ChatApp, drawn: bool) -> &'static str {
    if app.panels.workbench_visible && !drawn {
        WORKBENCH_HIDDEN_HINT
    } else {
        ""
    }
}

/// Splits the queue block off the bottom of `chat` (the chat keeps
/// `history_min` rows), draws it and returns the history area above it.
fn split_queue(
    frame: &mut Frame<'_>,
    chat: Rect,
    queue_lines: Vec<Line<'static>>,
    history_min: u16,
    theme: Theme,
) -> Rect {
    // Warteschlange zwischen Chat und Statuszeile; der Chat behält
    // `history_min` Zeilen.
    let queue_height = u16::try_from(queue_lines.len())
        .unwrap_or(u16::MAX)
        .min(chat.height.saturating_sub(history_min));
    let history = Rect {
        height: chat.height.saturating_sub(queue_height),
        ..chat
    };
    if queue_height > 0 {
        let queue_area = Rect {
            y: history.y.saturating_add(history.height),
            height: queue_height,
            ..chat
        };
        frame.render_widget(
            Paragraph::new(queue_lines).style(Style::default().fg(style::border_color(theme))),
            queue_area,
        );
    }
    history
}

/// Draws the Active Agents and Active Jobs windows -- or an open detail view
/// over both of them -- and returns their mouse regions `(agents, jobs)`.
fn draw_cards(
    app: &ChatApp,
    agents: Option<Rect>,
    jobs: Option<Rect>,
    buf: &mut Buffer,
    theme: Theme,
) -> (Option<Rect>, Option<Rect>) {
    let focus = app.panels.focus;
    match (app.agent_detail.as_ref(), agents) {
        // Detailansicht ohne Vollbild (F11 in der Detailansicht): sie
        // ersetzt Agenten- und Jobs-Fenster; das Rad über ihr gehört ganz
        // der Detailansicht.
        (Some(detail), Some(agents)) => {
            let cards = jobs.map_or(agents, |jobs| agents.union(jobs));
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
            (Some(cards), None)
        }
        (None, Some(agents)) => {
            let agents_area = if jobs.is_some() {
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
            if let Some(jobs_area) = jobs {
                jobs_window::render(
                    app.agent_monitor.jobs(),
                    jobs_area,
                    buf,
                    theme,
                    &app.mission.jobs,
                    focus == PaneFocus::Jobs,
                );
            }
            (Some(agents), jobs)
        }
        (_, None) => (None, None),
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
    areas: &MissionAreas,
    content: &MissionContent<'_>,
) -> BodyAreas {
    let theme = app.theme;
    let history = split_queue(frame, areas.chat, queue_lines, rows.history_min, theme);

    let focus = app.panels.focus;
    let buf = frame.buffer_mut();
    let (agents_region, jobs_region) = draw_cards(app, areas.agents, areas.jobs, buf, theme);
    if let Some(finished_area) = areas.finished {
        finished_window::render(
            &content.finished,
            finished_area,
            buf,
            theme,
            &app.mission.finished,
            focus == PaneFocus::Finished,
        );
    }
    if let Some(live_area) = areas.live {
        live_window::render(&content.live, live_area, buf, theme);
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
        workbench_hint: workbench_hint(app, areas.workbench.is_some()),
    }
}

/// Three-column landscape and shallow landscape (PL-97 C3).
///
/// Three columns: five separately bordered windows at the PL-94 anchors
/// (chat left and widest; Finished / Failed top-centre, LIVE below it;
/// Active Agents above Active Jobs far right), the explorer left of the chat
/// and the workbench below the jobs when the geometry places them. LIVE is
/// a focusable window there; with its detail open (`Alt+L`) it shows the
/// existing agent detail of the LIVE source in the same rect.
///
/// Shallow: today's side column with agents stacked above jobs, a one-row
/// Finished strip and a header-only LIVE strip above the (unframed) chat.
fn compose_columns(
    frame: &mut Frame<'_>,
    app: &ChatApp,
    queue_lines: Vec<Line<'static>>,
    rows: BodyRows,
    areas: &MissionAreas,
    content: &MissionContent<'_>,
) -> BodyAreas {
    let theme = app.theme;
    let area = frame.area();
    let focus = app.panels.focus;
    let three_columns = areas.placement == MissionPlacement::LandscapeThreeColumns;
    // Im dreispaltigen Querformat ist der Chat ein eigenes gerahmtes
    // Fenster; im flachen Querformat bleibt er wie bisher ohne Rahmen.
    let chat = if three_columns {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(window_border(theme, focus == PaneFocus::Chat))
            .title(fit_width(
                CHAT_TITLE,
                usize::from(areas.chat.width.saturating_sub(2)),
            ));
        let inner = block.inner(areas.chat);
        frame.render_widget(block, areas.chat);
        inner
    } else {
        areas.chat
    };
    let history = split_queue(frame, chat, queue_lines, rows.history_min, theme);

    let buf = frame.buffer_mut();
    let (agents_region, jobs_region) = draw_cards(app, areas.agents, areas.jobs, buf, theme);
    if let Some(finished_area) = areas.finished {
        if three_columns {
            render_finished_framed(
                &content.finished,
                finished_area,
                buf,
                theme,
                &app.mission.finished,
                focus == PaneFocus::Finished,
            );
        } else {
            finished_window::render(
                &content.finished,
                finished_area,
                buf,
                theme,
                &app.mission.finished,
                focus == PaneFocus::Finished,
            );
        }
    }
    if let Some(live_area) = areas.live {
        if three_columns {
            render_live_window(app, content, live_area, buf, focus == PaneFocus::Live);
        } else {
            live_window::render(&content.live, live_area, buf, theme);
        }
    }
    if let Some(explorer_area) = areas.explorer {
        render_explorer_panel(app, explorer_area, buf, theme);
    }
    if let Some(workbench_area) = areas.workbench {
        app.workbench
            .render(workbench_area, buf, theme, focus == PaneFocus::Workbench);
    }

    let (presence, revealed) = if app.panels.agents_visible {
        let presence = MissionPresence {
            jobs: jobs_region.is_some(),
            finished: areas.finished.is_some(),
            // Nur das gerahmte LIVE-Fenster der Mittelspalte ist ein
            // Fokusziel; der Kopfzeilen-Streifen des flachen Querformats
            // nicht.
            live: three_columns && areas.live.is_some(),
            agents_hidden: areas.agents.is_none(),
        };
        (
            presence,
            MissionPresence {
                jobs: areas.jobs.is_some(),
                ..presence
            },
        )
    } else {
        (
            MissionPresence::default(),
            revealed_presence(app, area, rows, content),
        )
    };
    app.last_regions.set(scroll_routing::FrameRegions {
        agents: agents_region,
        jobs: jobs_region,
        finished: areas.finished,
        live: areas.live,
        dialog: rows.dialog_open.then_some(areas.composer),
        plan: app.plan_ui.covers_history().then_some(history),
        presence,
        revealed,
    });
    BodyAreas {
        history,
        status: areas.status,
        input: areas.composer,
        workbench_hint: workbench_hint(app, areas.workbench.is_some()),
    }
}

/// Border style of a window: accent while focused.
fn window_border(theme: Theme, focused: bool) -> Style {
    if focused {
        Style::default().fg(style::accent_color(theme))
    } else {
        Style::default().fg(style::border_color(theme))
    }
}

/// Bordered Finished / Failed window of the three-column landscape: the
/// category rows of [`finished_window::render`] inside a frame whose title
/// carries the totals. The window's own header row lies under the top
/// border, which is drawn after it.
fn render_finished_framed(
    rows: &[TerminalRow],
    area: Rect,
    buf: &mut Buffer,
    theme: Theme,
    ui: &WindowUi,
    focused: bool,
) {
    let area = area.intersection(buf.area);
    if area.width < 3 || area.height < 3 {
        return;
    }
    // Erst der Inhalt (seine Kopfzeile liegt auf der oberen Rahmenzeile),
    // dann der Rahmen darüber.
    let content = Rect {
        x: area.x.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(1),
        ..area
    };
    finished_window::render(rows, content, buf, theme, ui, focused);
    let (ok, failed) = rows.iter().fold((0usize, 0usize), |(ok, failed), row| {
        (ok + row.ok, failed + row.failed)
    });
    let title = format!(
        " {} · ✓ {ok} · ✗ {failed} ",
        finished_window::FINISHED_TITLE
    );
    Block::default()
        .borders(Borders::ALL)
        .border_style(window_border(theme, focused))
        .title(fit_width(&title, usize::from(area.width.saturating_sub(2))))
        .render(area, buf);
}

/// LIVE in the landscape centre column: the bordered, scrollable LIVE
/// window, or -- with its detail open (`Alt+L`) -- the existing agent detail
/// of the LIVE source in the same rect.
fn render_live_window(
    app: &ChatApp,
    content: &MissionContent<'_>,
    area: Rect,
    buf: &mut Buffer,
    focused: bool,
) {
    if let Some(agent) = app.mission.live_detail_agent() {
        app.agent_monitor.render_agent_detail(
            &harw_types::SessionId(agent.to_owned()),
            area,
            buf,
            &app.mission.live_scroll,
            app.mission.live_detail_reasoning,
        );
        return;
    }
    // Die Zeilen für die Innenbreite des Fensters aufbereiten (der Rahmen
    // ersetzt den Einzug der Streifen).
    let width = area
        .width
        .saturating_sub(2)
        .saturating_add(live_window::LIVE_INDENT);
    let view = live_window::view(
        content.source,
        content.tool_started,
        &app.recency,
        width,
        app.theme,
        content.now,
    );
    live_window::render_window(
        &view,
        area,
        buf,
        app.theme,
        &app.mission.live_scroll,
        focused,
    );
}

/// One-line summary of the compact placement (PL-97 C3): the three window
/// identities as counters `Agents n · Jobs n · Done ok/✗failed`, then
/// today's agents summary. `None` while there is nothing to count (no
/// child agent and no job), so the line costs nothing.
fn compact_summary_line(
    app: &ChatApp,
    finished: &[TerminalRow],
    width: u16,
    focused: bool,
) -> Option<Line<'static>> {
    let theme = app.theme;
    let monitor = &app.agent_monitor;
    let jobs = monitor.jobs();
    // Die heutige Zusammenfassung ohne eigene Fokusmarke (die steht vorn).
    let legacy = crate::agent_monitor::render_collapsed_summary(monitor, width, theme, false);
    if legacy.is_none() && jobs.is_empty() {
        return None;
    }
    let running = jobs.iter().filter(|row| row.active).count();
    let (ok, failed) = finished.iter().fold((0usize, 0usize), |(ok, failed), row| {
        (ok + row.ok, failed + row.failed)
    });
    let marker = if focused { "▸ " } else { "" };
    let failed_style = if failed > 0 {
        style::error_style(theme)
    } else {
        style::dim_style(theme)
    };
    let mut spans = vec![
        Span::styled(
            format!(
                "{marker}Agents {} · Jobs {running} · Done {ok}/",
                monitor.active_count()
            ),
            Style::default().fg(style::accent_color(theme)),
        ),
        Span::styled(format!("✗{failed}"), failed_style),
    ];
    if let Some(legacy) = legacy {
        spans.push(Span::styled(" │ ", style::dim_style(theme)));
        spans.extend(legacy.spans);
    }
    Some(Line::from(spans))
}

/// Wide side column (agents/workbench right, explorer left) of a maximized
/// panel or, on narrow terminals, the one-line agents summary above the
/// status line -- the pre-C2 composition; the summary line leads with the
/// Mission Control counters (C3).
fn compose_side_or_summary(
    frame: &mut Frame<'_>,
    app: &ChatApp,
    queue_lines: Vec<Line<'static>>,
    rows: BodyRows,
    revealed: MissionPresence,
    finished: &[TerminalRow],
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
        compact_summary_line(
            app,
            finished,
            area.width,
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
    // Vollbild eines anderen Panels blendet die Werkbank absichtlich aus.
    let workbench_drawn = pane_areas.workbench.is_some() || app.panels.maximized;
    BodyAreas {
        history: history_area,
        status: status_area,
        input: input_area,
        workbench_hint: workbench_hint(app, workbench_drawn),
    }
}
