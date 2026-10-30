//! PL-97 C3: Mission Control im Querformat (drei Spalten, flach, kompakt).
//!
//! Zellgenaue Render-Tests an den Ankern aus §3.3.1/§3.3.2 des Plans (die
//! Rechtecke stammen aus `harw_tui_layout::classify_mission`), dazu
//! Mausrad- und Klick-Routing über den Querformat-Fenstern, `Alt+L` an Ort
//! und Stelle, `F11` und ein Wechsel zwischen Hoch- und Querformat, der
//! Auswahl, Scroll-Stand und laufende Jobs behält.

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use harw_protocol::TurnEvent;
use harw_types::{SessionId, ToolCallId, TurnId};
use ratatui::layout::Rect;

use harw_tool_job::{JobState, ManagedJobOrigin};

use super::scroll_routing::route_scroll_input;
use super::tests::test_chat_app;
use super::{ChatApp, Overlay, Role, handle_panel_key, render_viewport};
use crate::approval_dialog::{ApprovalDialog, ApprovalDialogRequest};
use crate::dialog_frame::WHEEL_LINES;
use crate::jobs_panel::JobRow;
use crate::mission::projection::tests::{job, job_in};
use crate::panes::PaneFocus;
use crate::spinner::Spinner;
use crate::test_support::{TestError, TestResult, ctx};
use crate::tui_event::TuiEvent;

/// Bus-Ereignis eines Kind-Agenten der Testsitzung.
fn agent_event(app: &mut ChatApp, id: &str, role: &str, turn: TurnEvent) {
    let event = harw_core::AgentEvent {
        agent: SessionId::from_str(id),
        parent: Some(app.session_id().clone()),
        role: role.to_owned(),
        kind: harw_core::AgentEventKind::Turn(turn),
    };
    app.agent_monitor.apply(&event);
}

/// Ein laufender Agent (Kontext-Stand genügt für „aktiv“).
fn running(app: &mut ChatApp, id: &str, role: &str) {
    agent_event(
        app,
        id,
        role,
        TurnEvent::ContextUpdated {
            turn_id: TurnId::new(),
            used_tokens: 1_000,
            window_tokens: 100_000,
            history_items_dropped: 0,
            estimated_next_tokens: None,
            threshold_tokens: None,
            reserve_tokens: None,
        },
    );
}

/// Ein fehlgeschlagener Agent (ungesehen: `NEW`).
fn failed(app: &mut ChatApp, id: &str, role: &str) {
    agent_event(
        app,
        id,
        role,
        TurnEvent::TurnFailed {
            turn_id: TurnId::new(),
            reason: "Zeitlimit".to_owned(),
            retryable: false,
        },
    );
}

/// Ein Werkzeugaufruf des Agenten `id` (füllt seine Spur).
fn tool_call(app: &mut ChatApp, id: &str, role: &str, path: &str) {
    agent_event(
        app,
        id,
        role,
        TurnEvent::ToolCallRequested {
            turn_id: TurnId::new(),
            call_id: ToolCallId::new(),
            tool_name: "fs.read".to_owned(),
            arguments: serde_json::json!({ "path": path }),
        },
    );
}

/// Root-Orchestrator denkt und liest (LIVE-Quelle), ein Worker läuft, ein
/// Explorer ist fehlgeschlagen, ein Job läuft; eine eindeutige Verlaufszeile.
fn mission_app() -> TestResult<ChatApp> {
    let mut app = test_chat_app()?;
    let orchestrator = harw_registry_defaults::profile::role_names::ROOT_ORCHESTRATOR;
    agent_event(
        &mut app,
        "o1",
        orchestrator,
        TurnEvent::ReasoningDelta {
            turn_id: TurnId::new(),
            text: "Prüfe Lens und interne Werkzeugverträge".to_owned(),
        },
    );
    tool_call(&mut app, "o1", orchestrator, "README.md");
    running(&mut app, "w1", "uia-worker");
    failed(&mut app, "e2", "uia-explorer");
    app.agent_monitor
        .set_jobs(vec![job("job-1", true, false, "läuft")]);
    app.push_line(Role::System, "einzigartige-verlaufszeile-quer".to_owned());
    Ok(app)
}

/// Zeichnet `app` und liefert die Bildschirmzeilen (eine Zelle je Zeichen;
/// die Tests verwenden nur einfach breite Zeichen).
fn rendered(app: &ChatApp, width: u16, height: u16) -> TestResult<Vec<String>> {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
        .map_err(ctx("test terminal"))?;
    terminal
        .draw(|frame| render_viewport(frame, app, &Spinner::new(), None))
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

/// `width` Zellen der Zeile `y` ab Spalte `x`.
fn cells(rows: &[String], x: u16, y: u16, width: u16) -> TestResult<String> {
    let row = rows
        .get(usize::from(y))
        .ok_or(TestError::Missing("screen row"))?;
    Ok(row
        .chars()
        .skip(usize::from(x))
        .take(usize::from(width))
        .collect())
}

/// Die Zelle `(x, y)`.
fn cell(rows: &[String], x: u16, y: u16) -> TestResult<char> {
    cells(rows, x, y, 1)?
        .chars()
        .next()
        .ok_or(TestError::Missing("screen cell"))
}

/// Prüft den eigenen Rahmen von `rect`: vier Ecken an ihren Zellen.
fn assert_framed(rows: &[String], rect: Rect, name: &str) -> TestResult {
    let right = rect.x + rect.width - 1;
    let bottom = rect.y + rect.height - 1;
    let shown = rows.join("\n");
    assert_eq!(
        cell(rows, rect.x, rect.y)?,
        '┌',
        "{name} top left:\n{shown}"
    );
    assert_eq!(
        cell(rows, right, rect.y)?,
        '┐',
        "{name} top right:\n{shown}"
    );
    assert_eq!(
        cell(rows, rect.x, bottom)?,
        '└',
        "{name} bottom left:\n{shown}"
    );
    assert_eq!(
        cell(rows, right, bottom)?,
        '┘',
        "{name} bottom right:\n{shown}"
    );
    Ok(())
}

fn alt(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> TuiEvent {
    TuiEvent::Mouse(MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    })
}

fn click(column: u16, row: u16) -> TuiEvent {
    mouse(MouseEventKind::Down(MouseButton::Left), column, row)
}

/// Prüft das Fünf-Fenster-Bild des dreispaltigen Querformats zellgenau:
/// `chat`, `finished`, `live`, `agents`, `jobs` sind die Anker aus
/// `classify_mission`; Statuszeile und Composer liegen darunter.
fn assert_five_windows(
    app: &ChatApp,
    rows: &[String],
    chat: Rect,
    finished: Rect,
    live: Rect,
    agents: Rect,
    jobs: Rect,
) -> TestResult {
    let shown = rows.join("\n");
    let regions = app.last_regions.get();
    assert_eq!(regions.agents, Some(agents), "{shown}");
    assert_eq!(regions.jobs, Some(jobs), "{shown}");
    assert_eq!(regions.finished, Some(finished), "{shown}");
    assert_eq!(regions.live, Some(live), "{shown}");
    assert!(regions.presence.live, "LIVE is a focus target in landscape");
    for (rect, name) in [
        (chat, "chat"),
        (finished, "finished"),
        (live, "live"),
        (agents, "agents"),
        (jobs, "jobs"),
    ] {
        assert_framed(rows, rect, name)?;
    }
    // Chat links und am breitesten; Finished oben in der Mitte, LIVE darunter;
    // Agenten über Jobs ganz rechts.
    assert_eq!((chat.x, chat.y), (0, 0));
    assert!(chat.width > finished.width && chat.width > agents.width);
    assert_eq!((finished.x, finished.y), (chat.width, 0));
    assert_eq!((live.x, live.y), (finished.x, finished.height));
    assert_eq!((agents.x, agents.y), (finished.x + finished.width, 0));
    assert_eq!((jobs.x, jobs.y), (agents.x, agents.height));
    assert_eq!(cells(rows, 1, 0, 6)?, " Chat ", "{shown}");
    // Der Titel ist die eigene Kopfzeile des Finished-Fensters auf der
    // oberen Rahmenzeile (dieselben Summen wie im Hochformat).
    assert!(
        cells(rows, finished.x + 1, 0, finished.width - 2)?
            .starts_with("─ FINISHED / FAILED · ✓ 0 · ✗ 1 "),
        "{shown}"
    );
    assert!(
        cells(rows, finished.x + 1, 1, finished.width - 2)?.contains("uia-explorer ×1 ✗1 NEW"),
        "{shown}"
    );
    assert!(
        cells(rows, live.x + 1, live.y, live.width - 2)?.starts_with(" LIVE · root-orchestrator "),
        "{shown}"
    );
    let live_body: String = (live.y + 1..live.y + 4)
        .map(|y| cells(rows, live.x + 1, y, live.width - 2))
        .collect::<TestResult<Vec<_>>>()?
        .join("\n");
    assert!(
        live_body.starts_with("∴ Prüfe Lens") && live_body.contains("● fs.read · running · "),
        "{shown}"
    );
    assert!(
        cells(rows, agents.x + 1, 0, agents.width - 2)?.starts_with(" Agents · 2 active"),
        "{shown}"
    );
    assert!(
        cells(rows, jobs.x + 1, jobs.y, jobs.width - 2)?.starts_with(" Jobs · 1 active "),
        "{shown}"
    );
    assert!(
        (jobs.y + 1..jobs.y + jobs.height - 1)
            .map(|y| cells(rows, jobs.x + 1, y, jobs.width - 2))
            .collect::<TestResult<Vec<_>>>()?
            .iter()
            .any(|row| row.contains("name-job-1")),
        "{shown}"
    );
    let history = rows
        .iter()
        .position(|row| row.contains("einzigartige-verlaufszeile-quer"))
        .ok_or(TestError::Missing("Verlaufszeile"))?;
    let history_y = u16::try_from(history).map_err(ctx("row index"))?;
    assert!(
        history_y > chat.y && history_y < chat.y + chat.height - 1,
        "der Verlauf steht im Chat-Fenster: {shown}"
    );
    let status = cells(rows, 0, chat.height, chat.width)?;
    assert!(
        status.contains("Ask"),
        "Statuszeile unter den Fenstern: {shown}"
    );
    assert!(
        cells(rows, 0, chat.height + 2, 6)?.contains("› "),
        "Composer: {shown}"
    );
    Ok(())
}

/// PL-97 C3 (§3.3.1): bei 160×45 zeichnet das volle Profil fünf getrennt
/// gerahmte Fenster an den Ankern 88 | 40 | 32.
#[test]
fn render_viewport_at_160x45_shows_five_windows_at_the_landscape_anchors() -> TestResult {
    let app = mission_app()?;
    let rows = rendered(&app, 160, 45)?;
    assert_five_windows(
        &app,
        &rows,
        Rect::new(0, 0, 88, 41),
        Rect::new(88, 0, 40, 5),
        Rect::new(88, 5, 40, 36),
        Rect::new(128, 0, 32, 21),
        Rect::new(128, 21, 32, 20),
    )
}

/// PL-97 C3: 200×50 — 110 | 50 | 40, dieselbe Anordnung.
#[test]
fn render_viewport_at_200x50_shows_five_windows_at_the_landscape_anchors() -> TestResult {
    let app = mission_app()?;
    let rows = rendered(&app, 200, 50)?;
    assert_five_windows(
        &app,
        &rows,
        Rect::new(0, 0, 110, 46),
        Rect::new(110, 0, 50, 5),
        Rect::new(110, 5, 50, 41),
        Rect::new(160, 0, 40, 23),
        Rect::new(160, 23, 40, 23),
    )
}

/// PL-97 C3: 200×50 mit Werkbank und Explorer — die Werkbank nimmt die
/// unteren 40 % der rechten Spalte unter den Jobs (Agenten und Jobs behalten
/// je mindestens fünf Zeilen), der Explorer (36 Spalten) steht links, weil der
/// Chat sein Minimum behält; Finished/LIVE bleiben in der Mitte. `F11`
/// maximiert das fokussierte Agenten-Fenster weiterhin.
#[test]
fn render_viewport_at_200x50_with_workbench_and_explorer_keeps_the_anchors() -> TestResult {
    let mut app = mission_app()?;
    app.panels.workbench_visible = true;
    app.panels.explorer_visible = true;
    let rows = rendered(&app, 200, 50)?;
    let shown = rows.join("\n");
    let regions = app.last_regions.get();
    let agents = Rect::new(160, 0, 40, 14);
    let jobs = Rect::new(160, 14, 40, 14);
    let workbench = Rect::new(160, 28, 40, 18);
    assert_eq!(regions.agents, Some(agents), "{shown}");
    assert_eq!(regions.jobs, Some(jobs), "{shown}");
    assert_eq!(regions.finished, Some(Rect::new(110, 0, 50, 5)), "{shown}");
    assert_eq!(regions.live, Some(Rect::new(110, 5, 50, 41)), "{shown}");
    assert!(agents.height >= 5 && jobs.height >= 5);
    assert_framed(&rows, workbench, "workbench")?;
    assert!(
        cells(&rows, 161, 28, 38)?.starts_with(" Werkbank"),
        "{shown}"
    );
    // Der Chat beginnt rechts der Explorer-Spalte.
    assert_framed(&rows, Rect::new(36, 0, 74, 46), "chat")?;
    assert_eq!(cells(&rows, 37, 0, 6)?, " Chat ", "{shown}");
    assert!(
        !rows
            .iter()
            .any(|row| row.contains("Workbench: not enough space")),
        "no hint while the workbench is drawn: {shown}"
    );

    // F11 im fokussierten Agenten-Fenster: Vollbild, danach zurück zu den
    // Ankern.
    app.panels.focus = PaneFocus::Agents;
    assert_eq!(handle_panel_key(&mut app, key(KeyCode::F(11))), Some(true));
    let rows = rendered(&app, 200, 50)?;
    let shown = rows.join("\n");
    assert_eq!(
        app.last_regions.get().agents,
        Some(Rect::new(0, 0, 200, 46)),
        "{shown}"
    );
    assert!(cells(&rows, 0, 0, 10)?.starts_with("┌ Agents"), "{shown}");
    assert_eq!(handle_panel_key(&mut app, key(KeyCode::F(11))), Some(true));
    let _ = rendered(&app, 200, 50)?;
    assert_eq!(app.last_regions.get().agents, Some(agents));
    Ok(())
}

/// PL-97 C3: 120×42 liegt deterministisch im dichten dreispaltigen Profil
/// (ab 108 Spalten, Körper ≥ 20 Zeilen): 60 | 34 | 26.
#[test]
fn render_viewport_at_120x42_is_the_dense_three_column_landscape() -> TestResult {
    let app = mission_app()?;
    let layout = crate::panes::classify_mission(
        Rect::new(0, 0, 120, 42),
        &app.panels,
        crate::panes::MissionRows {
            status: 1,
            composer: 3,
            live_body_lines: 2,
            finished: 1,
        },
    );
    assert_eq!(
        layout.placement,
        harw_tui_layout::MissionPlacement::LandscapeThreeColumns
    );
    let rows = rendered(&app, 120, 42)?;
    assert_five_windows(
        &app,
        &rows,
        Rect::new(0, 0, 60, 38),
        Rect::new(60, 0, 34, 5),
        Rect::new(60, 5, 34, 33),
        Rect::new(94, 0, 26, 19),
        Rect::new(94, 19, 26, 19),
    )
}

/// PL-97 C3: 100×30 ist das flache Querformat — die heutige Seitenspalte
/// (44 Spalten) mit Agenten über Jobs, darüber im Chat-Bereich ein
/// einzeiliger Finished-Streifen und ein LIVE-Kopfzeilenstreifen; der Chat
/// bleibt ohne Rahmen, LIVE ist dort kein Fokusziel.
#[test]
fn render_viewport_at_100x30_is_shallow_with_finished_and_live_strips() -> TestResult {
    let app = mission_app()?;
    let rows = rendered(&app, 100, 30)?;
    let shown = rows.join("\n");
    let regions = app.last_regions.get();
    let agents = Rect::new(56, 0, 44, 13);
    let jobs = Rect::new(56, 13, 44, 13);
    assert_eq!(regions.finished, Some(Rect::new(0, 0, 56, 1)), "{shown}");
    assert_eq!(regions.live, Some(Rect::new(0, 1, 56, 1)), "{shown}");
    assert_eq!(regions.agents, Some(agents), "{shown}");
    assert_eq!(regions.jobs, Some(jobs), "{shown}");
    assert!(!regions.presence.live, "the LIVE strip is not focusable");
    assert!(regions.presence.jobs && regions.presence.finished);
    assert!(
        cells(&rows, 0, 0, 56)?.starts_with("─ FINISHED / FAILED · uia-explorer ×1 ✗1 NEW"),
        "{shown}"
    );
    let live = cells(&rows, 0, 1, 56)?;
    assert!(
        live.starts_with("─ LIVE · root-orchestrator · ● fs.read"),
        "header-only LIVE carries the tool: {shown}"
    );
    assert_framed(&rows, agents, "agents")?;
    assert_framed(&rows, jobs, "jobs")?;
    assert!(cells(&rows, 57, 13, 10)?.starts_with(" Jobs"), "{shown}");
    let history = rows
        .iter()
        .position(|row| row.contains("einzigartige-verlaufszeile-quer"))
        .ok_or(TestError::Missing("Verlaufszeile"))?;
    assert!(history >= 2, "der Chat beginnt unter den Streifen: {shown}");
    assert_ne!(
        cell(&rows, 0, 2)?,
        '┌',
        "der Chat bleibt ohne Rahmen: {shown}"
    );
    Ok(())
}

/// PL-97 C3: kompakt (39×60, 48×19) bleibt die einzeilige Zusammenfassung;
/// sie trägt vorn die Zähler `Agents n · Jobs n · Done ok/✗failed`, es gibt
/// keine Fenster.
#[test]
fn render_viewport_at_39x60_and_48x19_shows_the_compact_counters() -> TestResult {
    let app = mission_app()?;
    for (width, height) in [(39u16, 60u16), (48, 19)] {
        let rows = rendered(&app, width, height)?;
        let shown = rows.join("\n");
        let regions = app.last_regions.get();
        assert_eq!(
            (regions.agents, regions.jobs, regions.finished, regions.live),
            (None, None, None, None),
            "{width}x{height}: {shown}"
        );
        let line = rows
            .iter()
            .position(|row| row.starts_with("Agents 2 · Jobs 1 · Done 0/✗1"))
            .ok_or(TestError::Unexpected(format!(
                "{width}x{height}: counters missing:\n{shown}"
            )))?;
        assert!(
            rows.get(line + 1).is_some_and(|row| row.contains("Ask")),
            "{width}x{height}: the summary sits right above the status line: {shown}"
        );
    }
    Ok(())
}

/// PL-97 C3: ein Wechsel Querformat → Hochformat → Querformat klassifiziert
/// nur neu — Agenten- und Jobs-Auswahl, Scroll-Stände und die laufenden Jobs
/// bleiben.
#[test]
fn render_viewport_resize_flip_keeps_selection_scroll_and_jobs() -> TestResult {
    let mut app = mission_app()?;
    app.agent_monitor.set_jobs(
        (0..40)
            .map(|index| job(&format!("j{index:02}"), true, false, "läuft"))
            .collect(),
    );
    for index in 0..100 {
        app.push_line(Role::System, format!("Zeile-{index:03}"));
    }
    let _ = rendered(&app, 160, 45)?;
    assert!(app.last_regions.get().presence.live, "landscape first");
    let keys_owned: Vec<String> = crate::mission::jobs_window::row_keys(app.agent_monitor.jobs())
        .into_iter()
        .map(str::to_owned)
        .collect();
    let keys: Vec<&str> = keys_owned.iter().map(String::as_str).collect();
    for _ in 0..3 {
        app.mission.jobs.select_step(true, &keys);
    }
    let _ = rendered(&app, 160, 45)?;
    app.mission.jobs.scroll_by(5);
    app.agent_monitor.selected = 1;
    app.panels.focus = PaneFocus::Jobs;
    let over_chat = mouse(MouseEventKind::ScrollUp, 5, 10);
    assert_eq!(route_scroll_input(&mut app, &over_chat), Some(true));
    let state = |app: &ChatApp| {
        (
            crate::mission::jobs_window::selected_job(app.agent_monitor.jobs(), &app.mission.jobs),
            app.agent_monitor.selected,
            app.agent_monitor
                .jobs()
                .iter()
                .filter(|row| row.active)
                .count(),
            app.panels.focus,
        )
    };
    let expected = (Some("j03".to_owned()), 1, 40, PaneFocus::Jobs);
    let _ = rendered(&app, 160, 45)?;
    assert_eq!(state(&app), expected);
    assert_eq!(app.mission.jobs.scroll_offset(), 5);

    // Hochformat mit eigenem Jobs-Fenster (ab 68 Spalten).
    let _ = rendered(&app, 70, 90)?;
    let regions = app.last_regions.get();
    assert_eq!(
        regions.agents.map(|rect| (rect.y, rect.height)),
        Some((0, 4))
    );
    assert!(!regions.presence.live, "portrait LIVE is a strip");
    assert_eq!(state(&app), expected);
    assert_eq!(app.mission.jobs.scroll_offset(), 5);
    assert!(!app.scroll.is_following(), "the chat stays scrolled up");

    let _ = rendered(&app, 160, 45)?;
    assert!(app.last_regions.get().presence.live, "landscape again");
    assert_eq!(state(&app), expected);
    assert_eq!(app.mission.jobs.scroll_offset(), 5);
    assert!(!app.scroll.is_following());
    Ok(())
}

/// Scroll-Stände aller Flächen: Agenten, Jobs, Finished, LIVE, Chat folgt.
fn offsets(app: &ChatApp) -> (usize, usize, usize, usize, bool) {
    (
        app.agent_monitor.panel_scroll_offset(),
        app.mission.jobs.scroll_offset(),
        app.mission.finished.scroll_offset(),
        app.mission.live_scroll.offset(),
        app.scroll.is_following(),
    )
}

/// PL-97 C3: das Mausrad über jedem Querformat-Fenster scrollt nur dieses
/// Fenster — LIVE (bzw. seine Details an Ort und Stelle) eingeschlossen —,
/// über dem Chat nur den Chat; ein offener Dialog behält den Vorrang.
#[test]
fn render_viewport_wheel_over_each_landscape_window_scrolls_only_that_window() -> TestResult {
    let mut app = mission_app()?;
    let orchestrator = harw_registry_defaults::profile::role_names::ROOT_ORCHESTRATOR;
    for index in 0..40 {
        running(
            &mut app,
            &format!("a{index:02}"),
            &format!("rolle-{index:02}"),
        );
        tool_call(
            &mut app,
            "o1",
            orchestrator,
            &format!("datei-{index:02}.md"),
        );
    }
    for index in 0..6 {
        failed(
            &mut app,
            &format!("f{index}"),
            &format!("fehlrolle-{index}"),
        );
    }
    app.agent_monitor.set_jobs(
        (0..40)
            .map(|index| job(&format!("j{index:02}"), true, false, "läuft"))
            .collect(),
    );
    for index in 0..100 {
        app.push_line(Role::System, format!("Zeile-{index:03}"));
    }
    let _ = rendered(&app, 160, 45)?;
    let regions = app.last_regions.get();
    let agents = regions.agents.ok_or(TestError::Missing("Agenten"))?;
    let jobs = regions.jobs.ok_or(TestError::Missing("Jobs"))?;
    let finished = regions.finished.ok_or(TestError::Missing("Finished"))?;
    let live = regions.live.ok_or(TestError::Missing("LIVE"))?;
    let down = |area: Rect| mouse(MouseEventKind::ScrollDown, area.x + 2, area.y + 2);
    assert_eq!(offsets(&app), (0, 0, 0, 0, true));

    assert_eq!(route_scroll_input(&mut app, &down(agents)), Some(true));
    assert_eq!(
        offsets(&app),
        (WHEEL_LINES, 0, 0, 0, true),
        "only the agents"
    );
    assert_eq!(route_scroll_input(&mut app, &down(jobs)), Some(true));
    assert_eq!(
        offsets(&app),
        (WHEEL_LINES, WHEEL_LINES, 0, 0, true),
        "only the jobs"
    );
    assert_eq!(route_scroll_input(&mut app, &down(finished)), Some(true));
    assert_eq!(
        offsets(&app),
        (WHEEL_LINES, WHEEL_LINES, WHEEL_LINES, 0, true),
        "the framed finished window scrolls without being expanded"
    );
    let up_live = mouse(MouseEventKind::ScrollUp, live.x + 2, live.y + 2);
    assert_eq!(route_scroll_input(&mut app, &up_live), Some(true));
    assert_eq!(
        offsets(&app),
        (WHEEL_LINES, WHEEL_LINES, WHEEL_LINES, 0, true),
        "the short LIVE body has nothing to scroll, nothing else moves"
    );

    // LIVE-Details an Ort und Stelle (lange Spur): das Rad scrollt sie.
    assert_eq!(handle_panel_key(&mut app, alt('l')), Some(true));
    let _ = rendered(&app, 160, 45)?;
    let before = offsets(&app);
    assert_eq!(route_scroll_input(&mut app, &up_live), Some(true));
    assert_eq!(
        offsets(&app),
        (before.0, before.1, before.2, WHEEL_LINES, true),
        "only the LIVE detail"
    );
    let ctrl_up = TuiEvent::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL));
    assert_eq!(app.panels.focus, PaneFocus::Live);
    assert_eq!(route_scroll_input(&mut app, &ctrl_up), Some(true));
    assert_eq!(
        app.mission.live_scroll.offset(),
        WHEEL_LINES + 1,
        "Ctrl+↑ follows the focus"
    );

    let over_chat = mouse(MouseEventKind::ScrollUp, 5, 10);
    let before = offsets(&app);
    assert_eq!(route_scroll_input(&mut app, &over_chat), Some(true));
    assert_eq!(
        offsets(&app),
        (before.0, before.1, before.2, before.3, false),
        "only the chat"
    );

    // Ein offener Dialog behält den Vorrang vor allen Fenstern.
    app.pending_approval_dialog = Some(approval_dialog());
    let _ = rendered(&app, 160, 45)?;
    let dialog = app
        .last_regions
        .get()
        .dialog
        .ok_or(TestError::Missing("Dialogfläche"))?;
    let before = offsets(&app);
    let over_dialog = mouse(MouseEventKind::ScrollDown, dialog.x + 2, dialog.y + 1);
    assert!(
        route_scroll_input(&mut app, &over_dialog).is_some(),
        "the dialog takes the wheel"
    );
    assert_eq!(offsets(&app), before, "no window scrolls behind the dialog");
    Ok(())
}

fn approval_dialog() -> ApprovalDialog {
    ApprovalDialog::new(ApprovalDialogRequest {
        call: harw_extension_api::ToolCall {
            id: ToolCallId::new(),
            name: harw_extension_api::ToolName::new("shell.exec"),
            arguments: serde_json::json!({ "command": "cargo build" }),
        },
        cwd: None,
        justification: None,
        risk: None,
        origin: None,
        remember_rule: None,
        deadline: Instant::now() + Duration::from_secs(300),
        reason_input_enabled: false,
    })
}

/// PL-97 C3: ein Linksklick fokussiert das Fenster unter dem Zeiger
/// (Agenten, Jobs, Finished/Failed, LIVE im Querformat); er schließt dabei
/// die LIVE-Details an Ort und Stelle, ändert nichts über dem Chat oder dem
/// nicht fokussierbaren LIVE-Streifen und nichts, solange ein Dialog offen
/// ist.
#[test]
fn render_viewport_click_focuses_the_window_under_the_pointer() -> TestResult {
    let mut app = mission_app()?;
    let _ = rendered(&app, 160, 45)?;
    let regions = app.last_regions.get();
    let agents = regions.agents.ok_or(TestError::Missing("Agenten"))?;
    let jobs = regions.jobs.ok_or(TestError::Missing("Jobs"))?;
    let finished = regions.finished.ok_or(TestError::Missing("Finished"))?;
    let live = regions.live.ok_or(TestError::Missing("LIVE"))?;
    let on = |area: Rect| click(area.x + 3, area.y + 1);

    for (area, focus) in [
        (jobs, PaneFocus::Jobs),
        (agents, PaneFocus::Agents),
        (finished, PaneFocus::Finished),
        (live, PaneFocus::Live),
    ] {
        assert_eq!(
            route_scroll_input(&mut app, &on(area)),
            Some(true),
            "{focus:?}"
        );
        assert_eq!(app.panels.focus, focus);
    }
    assert_eq!(
        route_scroll_input(&mut app, &on(live)),
        Some(false),
        "already focused"
    );
    assert_eq!(
        route_scroll_input(&mut app, &click(5, 10)),
        None,
        "chat: no window"
    );
    assert_eq!(app.panels.focus, PaneFocus::Live);

    // Ein Klick auf ein anderes Fenster schließt die LIVE-Details.
    assert_eq!(handle_panel_key(&mut app, alt('l')), Some(true));
    assert!(app.mission.live_detail_agent().is_some());
    assert_eq!(route_scroll_input(&mut app, &on(jobs)), Some(true));
    assert_eq!(app.panels.focus, PaneFocus::Jobs);
    assert!(app.mission.live_detail_agent().is_none());

    // Hochformat: der LIVE-Streifen nimmt den Klick, ohne zu fokussieren.
    let _ = rendered(&app, 70, 90)?;
    let strip = app
        .last_regions
        .get()
        .live
        .ok_or(TestError::Missing("LIVE-Streifen"))?;
    assert_eq!(route_scroll_input(&mut app, &on(strip)), Some(false));
    assert_eq!(app.panels.focus, PaneFocus::Jobs);

    // Offener Dialog: kein Fokuswechsel per Klick.
    app.pending_approval_dialog = Some(approval_dialog());
    let _ = rendered(&app, 160, 45)?;
    let agents = app
        .last_regions
        .get()
        .agents
        .ok_or(TestError::Missing("Agenten mit Dialog"))?;
    assert_eq!(route_scroll_input(&mut app, &on(agents)), None);
    assert_eq!(app.panels.focus, PaneFocus::Jobs);
    Ok(())
}

/// PL-97 C3: im Querformat ist LIVE ein Fokusziel (`F4` nach
/// Finished/Failed). `Alt+L` zeigt die bestehende Agenten-Detailansicht der
/// LIVE-Quelle im LIVE-Rechteck — ohne Vollbild, die übrigen Fenster
/// bleiben —; Enter schließt und öffnet sie im fokussierten Fenster, `Esc`
/// gibt den Fokus an den Chat zurück und schließt sie.
#[test]
fn render_viewport_alt_l_shows_the_live_detail_in_place_at_160x45() -> TestResult {
    let mut app = mission_app()?;
    let _ = rendered(&app, 160, 45)?;
    let mut seen = Vec::new();
    for _ in 0..5 {
        assert_eq!(handle_panel_key(&mut app, key(KeyCode::F(4))), Some(true));
        seen.push(app.panels.focus);
    }
    assert_eq!(
        seen,
        vec![
            PaneFocus::Agents,
            PaneFocus::Jobs,
            PaneFocus::Finished,
            PaneFocus::Live,
            PaneFocus::Chat,
        ]
    );

    assert_eq!(handle_panel_key(&mut app, alt('l')), Some(true));
    assert_eq!(app.panels.focus, PaneFocus::Live);
    assert_eq!(app.mission.live_detail_agent(), Some("o1"));
    assert!(app.agent_detail.is_none() && !app.panels.maximized);
    let rows = rendered(&app, 160, 45)?;
    let shown = rows.join("\n");
    let live = Rect::new(88, 5, 40, 36);
    assert_eq!(app.last_regions.get().live, Some(live), "{shown}");
    assert_framed(&rows, live, "live detail")?;
    assert!(
        cells(&rows, 89, 5, 38)?.starts_with(" Agent · root-orchestrator · Esc"),
        "{shown}"
    );
    assert!(
        cells(&rows, 129, 0, 30)?.starts_with(" Agents"),
        "the other windows stay: {shown}"
    );

    assert_eq!(
        handle_panel_key(&mut app, key(KeyCode::Char('r'))),
        Some(true)
    );
    assert!(
        !app.mission.live_detail_reasoning,
        "r toggles the reasoning"
    );
    assert_eq!(handle_panel_key(&mut app, key(KeyCode::Enter)), Some(true));
    assert!(app.mission.live_detail_agent().is_none(), "Enter closes");
    let rows = rendered(&app, 160, 45)?;
    assert!(
        cells(&rows, 89, 5, 38)?.starts_with(" LIVE · root-orchestrator"),
        "{}",
        rows.join("\n")
    );
    assert_eq!(handle_panel_key(&mut app, key(KeyCode::Enter)), Some(true));
    assert_eq!(app.mission.live_detail_agent(), Some("o1"), "Enter opens");
    assert_eq!(handle_panel_key(&mut app, key(KeyCode::Esc)), Some(true));
    assert_eq!(app.panels.focus, PaneFocus::Chat);
    assert!(app.mission.live_detail_agent().is_none(), "Esc closes");
    assert!(!app.mission.live_expanded);
    Ok(())
}

/// PL-97 C3: ist die Werkbank eingeschaltet, findet aber keinen Platz
/// (flaches Querformat mit zu niedriger Seitenspalte), nennt die
/// Statuszeile den Grund.
#[test]
fn render_viewport_names_a_workbench_without_room_in_the_status_line() -> TestResult {
    let mut app = test_chat_app()?;
    app.panels.workbench_visible = true;
    let rows = rendered(&app, 140, 14)?;
    let shown = rows.join("\n");
    assert!(
        rows.iter()
            .any(|row| row.contains("Workbench: not enough space")),
        "{shown}"
    );
    let rows = rendered(&app, 160, 45)?;
    assert!(
        !rows
            .iter()
            .any(|row| row.contains("Workbench: not enough space")),
        "{}",
        rows.join("\n")
    );
    Ok(())
}

/// PL-97 C3 (§3.3.3): die Zeile `Agent child (pending)` zählt nie mit —
/// weder im Titel des gerahmten Finished-Fensters (160×45) noch in den
/// kompakten Zählern (39×60); sie steht trotzdem als eigene Zeile da.
#[test]
fn render_viewport_never_counts_the_pending_child_row() -> TestResult {
    let mut app = test_chat_app()?;
    running(&mut app, "w1", "uia-worker");
    app.agent_monitor.set_jobs(vec![JobRow {
        origin: Some(ManagedJobOrigin::AgentChild {
            child_session_id: "child-9".to_owned(),
        }),
        ..job_in("job-9", JobState::Failed)
    }]);

    let rows = rendered(&app, 160, 45)?;
    let shown = rows.join("\n");
    let finished = app
        .last_regions
        .get()
        .finished
        .ok_or(TestError::Missing("Finished"))?;
    assert_framed(&rows, finished, "finished")?;
    assert!(
        cells(&rows, finished.x + 1, finished.y, finished.width - 2)?
            .starts_with("─ FINISHED / FAILED · ✓ 0 · ✗ 0 "),
        "{shown}"
    );
    assert!(
        cells(&rows, finished.x + 1, finished.y + 1, finished.width - 2)?
            .contains("Agent child (pending) ×1 ✗1"),
        "{shown}"
    );

    let rows = rendered(&app, 39, 60)?;
    assert!(
        rows.iter()
            .any(|row| row.starts_with("Agents 1 · Jobs 0 · Done 0/✗0")),
        "{}",
        rows.join("\n")
    );
    Ok(())
}

/// PL-97 C3: verschwindet das LIVE-Fenster (Querformat → Hochformat), gehen
/// mit dem Fokus auch seine Details an Ort und Stelle — eine beliebige Taste
/// räumt sie ab, und `Alt+L` öffnet im Hochformat gleich beim ersten Druck
/// die Detailansicht aus C2. Zurück im Querformat zeigt das LIVE-Rechteck
/// wieder LIVE.
#[test]
fn render_viewport_resize_closes_the_in_place_live_detail() -> TestResult {
    let mut app = mission_app()?;
    let live = Rect::new(88, 5, 40, 36);
    let live_title = |app: &ChatApp| -> TestResult<String> {
        let rows = rendered(app, 160, 45)?;
        cells(&rows, live.x + 1, live.y, live.width - 2)
    };

    // Eine beliebige Taste im Hochformat schließt die unsichtbaren Details.
    let _ = rendered(&app, 160, 45)?;
    assert_eq!(handle_panel_key(&mut app, alt('l')), Some(true));
    assert_eq!(app.mission.live_detail_agent(), Some("o1"));
    let _ = rendered(&app, 70, 90)?;
    let _ = handle_panel_key(&mut app, key(KeyCode::Char('j')));
    assert_eq!(app.panels.focus, PaneFocus::Chat);
    assert!(app.mission.live_detail_agent().is_none());
    assert!(!app.mission.live_expanded);
    let title = live_title(&app)?;
    assert!(title.starts_with(" LIVE · "), "{title}");

    // `Alt+L` im Hochformat: sofort die C2-Detailansicht.
    assert_eq!(handle_panel_key(&mut app, alt('l')), Some(true));
    assert_eq!(app.mission.live_detail_agent(), Some("o1"));
    let _ = rendered(&app, 70, 90)?;
    assert_eq!(handle_panel_key(&mut app, alt('l')), Some(true));
    assert!(
        app.mission.live_detail_agent().is_none(),
        "the in-place detail is gone"
    );
    assert_eq!(
        app.agent_detail.as_ref().map(|detail| detail.agent.clone()),
        Some(SessionId::from_str("o1")),
        "the first Alt+L opens the C2 detail"
    );
    assert_eq!(app.panels.focus, PaneFocus::Agents);
    // Das zweite `Alt+L` schließt sie; zurück im Querformat zeigt das
    // LIVE-Rechteck wieder LIVE.
    assert_eq!(handle_panel_key(&mut app, alt('l')), Some(true));
    assert!(app.agent_detail.is_none() && !app.panels.maximized);
    let title = live_title(&app)?;
    assert!(title.starts_with(" LIVE · "), "{title}");
    Ok(())
}

/// PL-97 C3: das gerahmte Finished-Fenster des Querformats zeigt seine
/// Kategorien immer — die Tasten bedienen diese Liste wie das Mausrad: `j`
/// wählt die nächste Kategorie, `Enter` öffnet deren Einzelläufe (statt erst
/// aufzuklappen).
#[test]
fn render_viewport_keys_of_the_framed_finished_window_select_and_open() -> TestResult {
    let mut app = mission_app()?;
    failed(&mut app, "f1", "uia-reviewer");
    failed(&mut app, "f2", "uia-tester");
    let _ = rendered(&app, 160, 45)?;
    for _ in 0..3 {
        assert_eq!(handle_panel_key(&mut app, key(KeyCode::F(4))), Some(true));
    }
    assert_eq!(app.panels.focus, PaneFocus::Finished);
    let _ = rendered(&app, 160, 45)?;
    let rows = crate::mission::projection::terminal_aggregate(
        &app.agent_monitor,
        app.agent_monitor.jobs(),
    );
    let keys = crate::mission::finished_window::row_keys(&rows);
    assert!(keys.len() >= 3, "{keys:?}");
    let first = app.mission.finished.selected_key(&keys);
    assert_eq!(
        handle_panel_key(&mut app, key(KeyCode::Char('j'))),
        Some(true)
    );
    let second = app.mission.finished.selected_key(&keys);
    assert_ne!(first, second, "j moves the selection");
    assert_eq!(second, keys.get(1).copied());
    let shown = rendered(&app, 160, 45)?.join("\n");
    // PL-97 C4: der Schlüssel ist typisiert, angezeigt wird die Bezeichnung.
    let second_label = rows.get(1).map(|row| row.label.as_str());
    assert!(
        shown.contains(&format!("▸ {}", second_label.unwrap_or_default())),
        "{shown}"
    );

    assert_eq!(handle_panel_key(&mut app, key(KeyCode::Enter)), Some(true));
    assert!(
        matches!(app.overlay, Some(Overlay::View(_))),
        "Enter opens the constituents"
    );
    assert!(!app.mission.finished_expanded, "nothing to expand first");
    Ok(())
}

/// PL-97 C3: der LIVE-Kopfzeilenstreifen des flachen Querformats ist nur so
/// breit wie die Chat-Spalte; eine lange Begründung ohne Werkzeug endet dort
/// trotzdem mit ihren neuesten Worten.
#[test]
fn render_viewport_at_100x30_live_strip_keeps_the_newest_reasoning() -> TestResult {
    let mut app = test_chat_app()?;
    let orchestrator = harw_registry_defaults::profile::role_names::ROOT_ORCHESTRATOR;
    agent_event(
        &mut app,
        "o1",
        orchestrator,
        TurnEvent::ReasoningDelta {
            turn_id: TurnId::new(),
            text: "Beginne mit der Analyse der Konfiguration und prüfe danach jeden \
                   einzelnen Vertrag bis zum letzten neuen Wort"
                .to_owned(),
        },
    );
    running(&mut app, "w1", "uia-worker");
    app.agent_monitor
        .set_jobs(vec![job("job-1", true, false, "läuft")]);
    let rows = rendered(&app, 100, 30)?;
    let shown = rows.join("\n");
    assert_eq!(
        app.last_regions.get().live,
        Some(Rect::new(0, 1, 56, 1)),
        "{shown}"
    );
    let strip = cells(&rows, 0, 1, 56)?;
    assert!(
        strip.starts_with("─ LIVE · root-orchestrator · ∴ …"),
        "{shown}"
    );
    assert!(
        strip.trim_end().ends_with("zum letzten neuen Wort"),
        "the strip keeps the newest words: {shown}"
    );
    Ok(())
}
