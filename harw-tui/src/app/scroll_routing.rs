//! Scroll-Routing: Mausrad nach Zeigerposition und die Scroll-Aktionen der
//! Tastenbelegung (`scroll_panel_up`/`scroll_panel_down`).
//!
//! # Verantwortung
//! Jede Stelle, an der die TUI Eingabeereignisse entgegennimmt (Leerlauf,
//! laufender Turn, offene Freigabe), fragt zuerst [`route_scroll_input`].
//! Das Mausrad scrollt, was unter dem Zeiger liegt:
//! 1. ein offener Dialog (Freigabe, sudo, Host-Permit, Plan, `ask_user`) —
//!    dessen Körper; bei der Plan-Freigabe auch über dem Plan im Verlauf;
//! 2. das Agenten-Panel (bzw. seine Detailansicht);
//! 3. die Mission-Control-Fenster (PL-97 C2/C3): Jobs und Finished/Failed
//!    scrollen je für sich ([`crate::mission::WindowUi`]), das LIVE-Fenster
//!    der Querformat-Mittelspalte mit eigenem [`crate::chat_scroll::ChatScroll`]
//!    (auch seine Details an Ort und Stelle); über dem LIVE-Streifen des
//!    Hochformats und des flachen Querformats scrollt nichts (er ist reine
//!    Anzeige);
//! 4. sonst der Chat-Verlauf (wie bisher).
//!
//! Die Scroll-Aktionen der Tastenbelegung wirken ohne Fokuswechsel: bei
//! sichtbarem Dialog auf dessen Körper, sonst auf das fokussierte Jobs-,
//! Finished/Failed- bzw. LIVE-Fenster, sonst auf das sichtbare
//! Agenten-Panel; ist nichts davon sichtbar, geht die Taste normal weiter.
//!
//! Ein Linksklick (PL-97 C3) fokussiert das Mission-Control-Fenster unter
//! dem Zeiger ([`FrameRegions::window_at`]); er wählt nichts aus und löst
//! nichts aus. Solange ein Dialog offen ist, ändert ein Klick den Fokus
//! nicht (Dialoge behalten den Vorrang).
//!
//! # Sicherheit
//! Scrollen ist reines Lesen: es trifft nie eine Entscheidung, ändert keine
//! Auswahl und erreicht weder Composer noch Warteschlange. Deshalb darf es
//! auch vor der Scharf-Verzögerung der Dialoge wirken.
//!
//! # Flächen
//! `render_viewport` legt die Flächen des letzten Frames in
//! [`ChatApp::last_regions`] ab ([`FrameRegions`]); Overlays setzen sie
//! zurück (dann scrollt das Rad wie bisher den Verlauf).

use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use super::ChatApp;
use crate::chat_scroll::ScrollAction;
use crate::dialog_frame::WHEEL_LINES;
use crate::keybindings::KeyAction;
use crate::mission::{MissionWindow, WindowUi};
use crate::panes::{MissionPresence, PaneFocus};
use crate::tui_event::TuiEvent;

/// Flächen des zuletzt gezeichneten Frames für das Maus-Routing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameRegions {
    /// Das sichtbare Agenten-Panel: entweder die breite Seitenspalte
    /// (bzw. deren Detailansicht) oder der oben angedockte Portrait-Dock
    /// schmaler, hoher Terminals. Für das Rad- und Tasten-Routing macht das
    /// keinen Unterschied — beide sind nur eine Fläche.
    pub(crate) agents: Option<Rect>,
    /// Active Jobs window (Mission Control, PL-97 C2).
    pub(crate) jobs: Option<Rect>,
    /// Finished/Failed window or strip.
    pub(crate) finished: Option<Rect>,
    /// LIVE window or strip.
    pub(crate) live: Option<Rect>,
    /// Ein offener Dialog anstelle des Composers.
    pub(crate) dialog: Option<Rect>,
    /// Der Plan über dem Verlauf, solange die Plan-Freigabe offen ist.
    pub(crate) plan: Option<Rect>,
    /// Mission Control windows of this frame as focus targets (PL-97 C2).
    pub(crate) presence: MissionPresence,
    /// Mission Control windows the portrait stack shows once `F3`/`F11`
    /// reveal it again (equal to `presence` while it is on screen).
    pub(crate) revealed: MissionPresence,
}

impl FrameRegions {
    /// The Mission Control window under `at`, if any (the agents panel
    /// counts as [`MissionWindow::Agents`]).
    #[must_use]
    pub(crate) fn window_at(&self, at: Position) -> Option<MissionWindow> {
        let hit = |area: Option<Rect>| area.is_some_and(|area| area.contains(at));
        if hit(self.agents) {
            Some(MissionWindow::Agents)
        } else if hit(self.jobs) {
            Some(MissionWindow::Jobs)
        } else if hit(self.finished) {
            Some(MissionWindow::Finished)
        } else if hit(self.live) {
            Some(MissionWindow::Live)
        } else {
            None
        }
    }
}

/// Leitet Mausrad und Scroll-Tasten weiter.
///
/// # Rückgabe
/// - `Some(redraw)`: verbraucht;
/// - `None`: kein Scroll-Ereignis (bzw. nichts Scrollbares sichtbar) —
///   normal weiterverarbeiten.
pub(crate) fn route_scroll_input(app: &mut ChatApp, event: &TuiEvent) -> Option<bool> {
    match event {
        TuiEvent::Mouse(mouse) => route_wheel(app, *mouse),
        TuiEvent::Key(key) => route_scroll_key(app, key),
        _ => None,
    }
}

/// Mausrad nach Zeigerposition; ein Linksklick fokussiert das
/// Mission-Control-Fenster unter dem Zeiger ([`route_click`]).
///
/// # Rückgabe
/// `None` für übrige Ereignisse (Bewegungen, Klicks neben den Fenstern).
pub(crate) fn route_wheel(app: &mut ChatApp, mouse: MouseEvent) -> Option<bool> {
    let up = match mouse.kind {
        MouseEventKind::ScrollUp => true,
        MouseEventKind::ScrollDown => false,
        MouseEventKind::Down(MouseButton::Left) => return route_click(app, mouse),
        _ => return None,
    };
    let regions = app.last_regions.get();
    let at = Position::new(mouse.column, mouse.row);
    let hit = |area: Option<Rect>| area.is_some_and(|area| area.contains(at));
    if (hit(regions.dialog) || hit(regions.plan))
        && let Some(redraw) = scroll_open_dialog(app, up, WHEEL_LINES)
    {
        return Some(redraw);
    }
    match regions.window_at(at) {
        Some(MissionWindow::Agents) => return Some(scroll_agents(app, up, WHEEL_LINES)),
        Some(MissionWindow::Jobs) => {
            return Some(scroll_window(&app.mission.jobs, up, WHEEL_LINES));
        }
        // Eingeklappt ist Finished/Failed eine einzige Zeile ohne
        // Scroll-Inhalt: das Rad wird verbraucht, scrollt aber nichts.
        Some(MissionWindow::Finished) => {
            if !finished_scrolls(regions) {
                return Some(false);
            }
            return Some(scroll_window(&app.mission.finished, up, WHEEL_LINES));
        }
        // Das LIVE-Fenster der Querformat-Mittelspalte scrollt (auch seine
        // Details); der LIVE-Streifen scrollt nicht und gibt das Rad auch
        // nicht an den Chat weiter: unter dem Zeiger liegt kein Verlauf.
        Some(MissionWindow::Live) => {
            if !regions.presence.live {
                return Some(false);
            }
            return Some(scroll_live(app, up, WHEEL_LINES));
        }
        None => {}
    }
    Some(
        app.scroll.handle_mouse(
            mouse,
            usize::from(app.last_history_total_lines()),
            usize::from(app.last_history_visible_rows()),
        ) == ScrollAction::Redraw,
    )
}

/// `scroll_panel_up`/`scroll_panel_down` (Standard `Ctrl+↑`/`Ctrl+↓`).
fn route_scroll_key(app: &mut ChatApp, key: &KeyEvent) -> Option<bool> {
    let up = match app.key_bindings.action_for(key)? {
        KeyAction::ScrollPanelUp => true,
        KeyAction::ScrollPanelDown => false,
        _ => return None,
    };
    let regions = app.last_regions.get();
    if regions.dialog.is_some()
        && let Some(redraw) = scroll_open_dialog(app, up, 1)
    {
        return Some(redraw);
    }
    match app.panels.focus {
        PaneFocus::Jobs if regions.jobs.is_some() => {
            return Some(scroll_window(&app.mission.jobs, up, 1));
        }
        // Eingeklappt fällt die Taste wie bisher an das Agenten-Panel.
        PaneFocus::Finished if finished_scrolls(regions) => {
            return Some(scroll_window(&app.mission.finished, up, 1));
        }
        PaneFocus::Live if regions.presence.live => {
            return Some(scroll_live(app, up, 1));
        }
        _ => {}
    }
    if regions.agents.is_some() {
        return Some(scroll_agents(app, up, 1));
    }
    None
}

/// Scrollt den Körper des obersten offenen Dialogs (gleiche Rangfolge wie
/// beim Zeichnen: sudo, Plan/`ask_user`, Freigabe, Host-Permit).
///
/// # Rückgabe
/// `None`, wenn kein Dialog offen ist.
fn scroll_open_dialog(app: &mut ChatApp, up: bool, lines: usize) -> Option<bool> {
    if app.sudo.is_open() {
        return Some(app.sudo.scroll_body(up, lines));
    }
    if app.plan_ui.is_open() {
        return Some(app.plan_ui.scroll_body(up, lines));
    }
    if let Some(dialog) = app.pending_approval_dialog.as_ref() {
        return Some(dialog.scroll_body(up, lines));
    }
    if let Some(dialog) = app.pending_host_permit_dialog.as_ref() {
        return Some(dialog.scroll_body(up, lines));
    }
    None
}

/// `true`, wenn das Finished/Failed-Fenster mehr als seine Kopfzeile zeigt
/// (nur dann gibt es etwas zu scrollen): aufgeklappt im Hochformat, immer
/// als gerahmtes Fenster des dreispaltigen Querformats (PL-97 C3); der
/// einzeilige Streifen nie. Dasselbe Prädikat entscheidet, ob die Tasten des
/// fokussierten Fensters die sichtbare Kategorienliste bedienen
/// (`handle_finished_window_key`), damit Rad und Tasten übereinstimmen.
pub(super) fn finished_scrolls(regions: FrameRegions) -> bool {
    regions.finished.is_some_and(|area| area.height > 1)
}

/// Scrollt ein Mission-Fenster (Jobs, Finished/Failed), ohne Auswahl oder
/// Fokus zu ändern; das nächste Zeichnen begrenzt den Versatz.
fn scroll_window(ui: &WindowUi, up: bool, lines: usize) -> bool {
    let delta = isize::try_from(lines).unwrap_or(isize::MAX);
    ui.scroll_by(if up { -delta } else { delta });
    true
}

/// Scrollt das LIVE-Fenster der Querformat-Mittelspalte (bzw. seine Details
/// an Ort und Stelle) im ChatScroll-Stil: hoch zu älteren Zeilen, runter
/// zurück zur neuesten; der Fokus bleibt.
fn scroll_live(app: &mut ChatApp, up: bool, lines: usize) -> bool {
    if up {
        app.mission.live_scroll.scroll_up_measured(lines);
    } else {
        app.mission.live_scroll.scroll_down(lines);
    }
    true
}

/// Linksklick: fokussiert das Mission-Control-Fenster unter dem Zeiger
/// (Agenten, Jobs, Finished/Failed, LIVE als Querformat-Fenster). Ein
/// Fokuswechsel schließt wie `F4` eine Detailansicht, deren Fenster den
/// Fokus verliert.
///
/// # Rückgabe
/// - `Some(true)`: Fokus gewechselt (neu zeichnen);
/// - `Some(false)`: über einem Fenster, aber nichts zu tun (schon
///   fokussiert, nicht fokussierbarer LIVE-Streifen);
/// - `None`: offener Dialog oder kein Fenster unter dem Zeiger — normal
///   weiterverarbeiten.
fn route_click(app: &mut ChatApp, mouse: MouseEvent) -> Option<bool> {
    let regions = app.last_regions.get();
    // Dialoge behalten den Vorrang: kein Fokuswechsel hinter ihrem Rücken.
    if regions.dialog.is_some() {
        return None;
    }
    let at = Position::new(mouse.column, mouse.row);
    let focus = match regions.window_at(at)? {
        MissionWindow::Agents => PaneFocus::Agents,
        MissionWindow::Jobs => PaneFocus::Jobs,
        MissionWindow::Finished => PaneFocus::Finished,
        MissionWindow::Live if regions.presence.live => PaneFocus::Live,
        MissionWindow::Live => return Some(false),
    };
    if app.panels.focus == focus {
        return Some(false);
    }
    app.panels.mission = regions.presence;
    app.panels.revealed = regions.revealed;
    app.panels.focus = focus;
    app.sync_agent_detail();
    Some(true)
}

/// Scrollt das Agenten-Panel (bzw. die Detailansicht), ohne die Auswahl
/// oder den Fokus zu ändern.
fn scroll_agents(app: &mut ChatApp, up: bool, lines: usize) -> bool {
    if let Some(detail) = app.agent_detail.as_mut() {
        if up {
            detail.scroll.scroll_up_measured(lines);
        } else {
            detail.scroll.scroll_down(lines);
        }
        return true;
    }
    let delta = isize::try_from(lines).unwrap_or(isize::MAX);
    app.agent_monitor
        .scroll_panel(if up { -delta } else { delta });
    true
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
    use harw_core::{AgentEvent, AgentEventKind};
    use harw_protocol::TurnEvent;
    use harw_types::{SessionId, TokenUsage, TurnId};

    use super::super::tests::test_chat_app;
    use super::super::{
        BusyKeyOutcome, ChatApp, Role, handle_busy_event, handle_panel_key, render_viewport,
    };
    use super::*;
    use crate::approval_dialog::{ApprovalDialog, ApprovalDialogRequest};
    use crate::spinner::Spinner;
    use crate::test_support::{TestError, TestResult, ctx};

    /// Bildschirmgrößen aus dem Auftrag (kleine Kachel bis großer Monitor).
    const SIZES: [(u16, u16); 5] = [
        (
            crate::dialog_frame::MIN_LAYOUT_WIDTH,
            crate::dialog_frame::MIN_LAYOUT_HEIGHT,
        ),
        (60, 20),
        (80, 24),
        (120, 40),
        (200, 50),
    ];

    fn screen(app: &ChatApp, width: u16, height: u16) -> TestResult<Vec<String>> {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
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

    /// Freigabefrage wie im Screenshot: langer Befehl, alle Info-Zeilen.
    fn crowded_dialog(steps: usize) -> ApprovalDialog {
        let command = (0..steps)
            .map(|i| format!("schritt-{i} --mit-langem-argument"))
            .collect::<Vec<_>>()
            .join(" && ");
        ApprovalDialog::new(ApprovalDialogRequest {
            call: harw_extension_api::ToolCall {
                id: harw_types::ToolCallId::new(),
                name: harw_extension_api::ToolName::new("shell.exec"),
                arguments: serde_json::json!({ "command": command }),
            },
            cwd: Some("/home/u/ein/ziemlich/langes/projekt/verzeichnis".to_owned()),
            justification: Some("baut und prüft das Projekt".to_owned()),
            risk: Some("mittel — schreibt in target/".to_owned()),
            origin: Some("uia-worker (uia › root-orchestrator › uia-worker)".to_owned()),
            remember_rule: Some("cargo build".to_owned()),
            deadline: Instant::now() + Duration::from_secs(300),
            reason_input_enabled: true,
        })
        .with_auto_reason(Some("shell – unbekannter Befehl".to_owned()))
    }

    fn child_event(app: &ChatApp, id: &str, role: &str, turn: TurnEvent) -> AgentEvent {
        AgentEvent {
            agent: SessionId::from_str(id),
            parent: Some(app.session_id().clone()),
            role: role.to_owned(),
            kind: AgentEventKind::Turn(turn),
        }
    }

    fn running(app: &mut ChatApp, id: &str, role: &str) {
        let event = child_event(
            app,
            id,
            role,
            TurnEvent::ContextUpdated {
                turn_id: TurnId::new(),
                used_tokens: 22_280,
                window_tokens: 202_800,
                history_items_dropped: 0,
                estimated_next_tokens: None,
                threshold_tokens: None,
                reserve_tokens: None,
            },
        );
        app.agent_monitor.apply(&event);
    }

    fn finished(app: &mut ChatApp, id: &str, role: &str) {
        let event = child_event(
            app,
            id,
            role,
            TurnEvent::TurnCompleted {
                turn_id: TurnId::new(),
                usage: Some(TokenUsage {
                    input_tokens: 30_000,
                    output_tokens: 1_000,
                    ..TokenUsage::default()
                }),
            },
        );
        app.agent_monitor.apply(&event);
    }

    fn with_agents(app: &mut ChatApp) {
        running(app, "w7", "uia-worker");
        for (id, role) in [
            ("w1", "uia-worker"),
            ("w2", "uia-worker"),
            ("w3", "uia-worker"),
            ("e1", "uia-explorer"),
            ("e2", "uia-explorer"),
            ("o1", "root-orchestrator"),
        ] {
            finished(app, id, role);
        }
    }

    fn wheel(kind: MouseEventKind, column: u16, row: u16) -> TuiEvent {
        TuiEvent::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    }

    /// Optionen und Tastenhinweis des Freigabe-Dialogs sind bei jeder Größe
    /// vollständig sichtbar; der Dialog bleibt im Terminal.
    #[test]
    fn approval_dialog_options_and_hint_fit_every_size() -> TestResult {
        let mut app = test_chat_app()?;
        with_agents(&mut app);
        app.pending_approval_dialog = Some(crowded_dialog(12));
        for (width, height) in SIZES {
            let rows = screen(&app, width, height)?;
            let shown = rows.join("\n");
            assert!(
                rows.iter().any(|row| row.contains("❯ 1. Yes")),
                "{width}x{height}: {shown}"
            );
            assert!(
                rows.iter().any(|row| row.contains("No (Esc)")),
                "{width}x{height}: {shown}"
            );
            assert!(
                rows.iter()
                    .any(|row| row.contains("↑↓") && row.contains("Enter") && row.contains("Esc")),
                "{width}x{height}: Hinweiszeile fehlt: {shown}"
            );
            assert!(
                rows.iter().any(|row| row.contains("Execute command?")),
                "{width}x{height}: Titel/oberer Rahmen fehlt: {shown}"
            );
            let last = rows.last().ok_or(TestError::Missing("letzte Zeile"))?;
            assert!(
                last.starts_with('└'),
                "{width}x{height}: unterer Rahmen liegt im Terminal: {shown}"
            );
            assert!(
                rows.iter()
                    .any(|row| row.contains("Ask") && row.contains("chat")),
                "{width}x{height}: Statuszeile (Freigabe, Modus) fehlt: {shown}"
            );
        }
        Ok(())
    }

    /// Unter 100 Spalten klappt das Agenten-Panel zu einer Zeile zusammen
    /// (ab 16 Zeilen Höhe), darüber erscheint das volle Panel. PL-97 C3:
    /// 120×40 und 200×50 sind dreispaltiges Querformat — die fertigen
    /// Agenten zählt dort das Finished/Failed-Fenster.
    #[test]
    fn agent_panel_collapses_below_the_width_threshold() -> TestResult {
        let mut app = test_chat_app()?;
        with_agents(&mut app);
        for (width, height) in SIZES {
            let rows = screen(&app, width, height)?;
            let shown = rows.join("\n");
            let panel = rows.iter().any(|row| row.contains("Agents · 1 active"));
            let summary = rows.iter().any(|row| row.contains("Agents: ● 1 active"));
            if width >= crate::panes::AGENTS_PANEL_MIN_TERMINAL_WIDTH {
                assert!(panel && !summary, "{width}x{height}: {shown}");
                assert!(
                    rows.iter()
                        .any(|row| row.contains("FINISHED / FAILED · ✓ 6 · ✗ 0")),
                    "{width}x{height}: {shown}"
                );
            } else if height >= crate::panes::AGENTS_SUMMARY_MIN_HEIGHT {
                assert!(!panel && summary, "{width}x{height}: {shown}");
            } else {
                assert!(!panel && !summary, "{width}x{height}: {shown}");
            }
        }
        Ok(())
    }

    /// Die Statuszeile kürzt nach Vorrang: Modus, Freigabe und Modell
    /// bleiben, Token-Details fallen zuerst.
    #[test]
    fn status_line_keeps_mode_and_approval_in_narrow_windows() -> TestResult {
        let app = test_chat_app()?;
        for (width, height) in [(60u16, 20u16), (80, 24)] {
            let rows = screen(&app, width, height)?;
            let status = rows
                .iter()
                .find(|row| row.contains("Shift+Tab"))
                .ok_or(TestError::Missing("Statuszeile"))?;
            assert!(status.contains("Approval:"), "{width}: {status}");
            assert!(!status.contains("(in "), "{width}: {status}");
        }
        let wide = screen(&app, 200, 50)?;
        assert!(wide.iter().any(|row| row.contains("Σ Tokens:")), "{wide:?}");
        Ok(())
    }

    /// Mausrad über dem Panel scrollt das Panel, nicht den Verlauf; über
    /// dem Verlauf den Verlauf, nicht das Panel.
    #[test]
    fn wheel_over_the_panel_scrolls_the_panel_not_the_chat() -> TestResult {
        let mut app = test_chat_app()?;
        for index in 0..60 {
            running(
                &mut app,
                &format!("a{index:02}"),
                &format!("rolle-{index:02}"),
            );
        }
        for index in 0..100 {
            app.push_line(Role::System, format!("Zeile-{index:03}"));
        }
        let _ = screen(&app, 120, 40)?;
        let panel = app
            .last_regions
            .get()
            .agents
            .ok_or(TestError::Missing("Agenten-Panel"))?;
        assert!(app.scroll.is_following());
        let over_panel = wheel(MouseEventKind::ScrollDown, panel.x + 2, panel.y + 3);
        assert_eq!(route_scroll_input(&mut app, &over_panel), Some(true));
        assert_eq!(app.agent_monitor.panel_scroll_offset(), WHEEL_LINES);
        assert!(app.scroll.is_following(), "der Verlauf bleibt unberührt");
        // PL-97 C3: LIVE (Mittelspalte) darf `rolle-00` als Quelle nennen;
        // das Agenten-Fenster selbst ist weitergescrollt.
        let rows = screen(&app, 120, 40)?;
        let panel_rows: Vec<String> = rows
            .iter()
            .skip(usize::from(panel.y))
            .take(usize::from(panel.height))
            .map(|row| {
                row.chars()
                    .skip(usize::from(panel.x))
                    .take(usize::from(panel.width))
                    .collect()
            })
            .collect();
        let shown = panel_rows.join("\n");
        assert!(!shown.contains("rolle-00"), "{shown}");
        assert!(shown.contains("rolle-03"), "{shown}");

        let over_chat = wheel(MouseEventKind::ScrollUp, 2, 5);
        assert_eq!(route_scroll_input(&mut app, &over_chat), Some(true));
        assert!(!app.scroll.is_following(), "der Verlauf scrollt");
        assert_eq!(app.agent_monitor.panel_scroll_offset(), WHEEL_LINES);
        Ok(())
    }

    /// Mausrad und `scroll_panel_down` über einem offenen Dialog scrollen
    /// dessen Körper; Optionen bleiben sichtbar, die Auswahl unverändert,
    /// der Verlauf bleibt stehen.
    #[test]
    fn wheel_and_scroll_keys_over_a_dialog_scroll_its_body() -> TestResult {
        let mut app = test_chat_app()?;
        for index in 0..100 {
            app.push_line(Role::System, format!("Zeile-{index:03}"));
        }
        // Mit „v Details“ steht der ganze Befehl im Körper — er läuft über.
        let mut dialog = crowded_dialog(60);
        dialog.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE), true);
        app.pending_approval_dialog = Some(dialog);
        let before = screen(&app, 80, 24)?;
        let dialog_area = app
            .last_regions
            .get()
            .dialog
            .ok_or(TestError::Missing("Dialogfläche"))?;
        let over_dialog = wheel(
            MouseEventKind::ScrollDown,
            dialog_area.x + 5,
            dialog_area.y + 2,
        );
        assert_eq!(route_scroll_input(&mut app, &over_dialog), Some(true));
        let offset = app
            .pending_approval_dialog
            .as_ref()
            .map(ApprovalDialog::body_offset);
        assert_eq!(offset, Some(WHEEL_LINES));
        assert!(app.scroll.is_following(), "der Verlauf bleibt stehen");
        let after = screen(&app, 80, 24)?;
        assert_ne!(before, after);
        for needle in ["❯ 1. Yes", "No (Esc)"] {
            assert!(
                after.iter().any(|row| row.contains(needle)),
                "{needle}: {after:?}"
            );
        }

        let ctrl_down = TuiEvent::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL));
        assert_eq!(route_scroll_input(&mut app, &ctrl_down), Some(true));
        let offset = app
            .pending_approval_dialog
            .as_ref()
            .map(ApprovalDialog::body_offset);
        assert_eq!(offset, Some(WHEEL_LINES + 1));
        let shown = screen(&app, 80, 24)?;
        assert!(
            shown.iter().any(|row| row.contains("❯ 1. Yes")),
            "Scrollen ändert die Auswahl nicht: {shown:?}"
        );
        Ok(())
    }

    /// Mausrad über dem oben angedockten Portrait-Dock (schmales, hohes
    /// Terminal) scrollt den Dock, nicht den Verlauf; darunter (über dem
    /// Chat) scrollt weiterhin der Verlauf. Gleiche Zusicherung wie
    /// `wheel_over_the_panel_scrolls_the_panel_not_the_chat`, nur mit dem
    /// Dock statt der breiten Seitenspalte.
    #[test]
    fn wheel_over_the_top_dock_scrolls_the_dock_not_the_chat() -> TestResult {
        let mut app = test_chat_app()?;
        for index in 0..60 {
            running(
                &mut app,
                &format!("a{index:02}"),
                &format!("rolle-{index:02}"),
            );
        }
        for index in 0..100 {
            app.push_line(Role::System, format!("Zeile-{index:03}"));
        }
        let _ = screen(&app, 80, 40)?;
        let regions = app.last_regions.get();
        let dock = regions.agents.ok_or(TestError::Missing("Portrait-Dock"))?;
        let jobs = regions.jobs.ok_or(TestError::Missing("Jobs-Fenster"))?;
        let live = regions.live.ok_or(TestError::Missing("LIVE-Streifen"))?;
        assert_eq!(dock.y, 0, "der Dock ist oben angedockt");
        // PL-97 C2: ab 68 Spalten teilen sich Agenten- und Jobs-Fenster die
        // volle Breite der Kartenzeile.
        assert_eq!(dock.x, 0);
        assert_eq!(
            dock.width + jobs.width,
            80,
            "die Kartenzeile nimmt die volle Breite ein"
        );
        assert!(app.scroll.is_following());

        let over_dock = wheel(MouseEventKind::ScrollDown, dock.x + 2, dock.y + 1);
        assert_eq!(route_scroll_input(&mut app, &over_dock), Some(true));
        assert_eq!(app.agent_monitor.panel_scroll_offset(), WHEEL_LINES);
        assert!(app.scroll.is_following(), "der Verlauf bleibt unberührt");

        let below_dock = live.y + live.height + 1;
        let over_chat = wheel(MouseEventKind::ScrollUp, 2, below_dock);
        assert_eq!(route_scroll_input(&mut app, &over_chat), Some(true));
        assert!(!app.scroll.is_following(), "der Verlauf scrollt");
        assert_eq!(app.agent_monitor.panel_scroll_offset(), WHEEL_LINES);
        Ok(())
    }

    /// `scroll_panel_down`/`scroll_panel_up` wirken auch dann auf den
    /// Agenten-Monitor, wenn nur der oben angedockte Portrait-Dock (statt
    /// der Seitenspalte) sichtbar ist; F3 (`agents_visible = false`) blendet
    /// beide gleich aus, die Taste geht dann normal an den Composer weiter.
    #[test]
    fn scroll_panel_keys_reach_the_dock_when_it_is_the_only_visible_agents_area() -> TestResult {
        let mut app = test_chat_app()?;
        for index in 0..3 {
            running(&mut app, &format!("a{index}"), &format!("rolle-{index}"));
        }
        let ctrl_down = TuiEvent::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL));

        let _ = screen(&app, 80, 40)?;
        let dock = app
            .last_regions
            .get()
            .agents
            .ok_or(TestError::Missing("Portrait-Dock"))?;
        assert_eq!(dock.y, 0, "der Dock ist oben angedockt");
        assert_eq!(route_scroll_input(&mut app, &ctrl_down), Some(true));
        assert_eq!(app.agent_monitor.panel_scroll_offset(), 1);

        // F3 blendet den Dock genau wie die Seitenspalte aus — die Taste
        // geht dann normal an den Composer weiter (`None`).
        app.panels.agents_visible = false;
        let _ = screen(&app, 80, 40)?;
        assert!(app.last_regions.get().agents.is_none(), "der Dock ist aus");
        assert_eq!(route_scroll_input(&mut app, &ctrl_down), None);
        assert_eq!(
            app.agent_monitor.panel_scroll_offset(),
            1,
            "ohne sichtbaren Dock ändert sich der Versatz nicht"
        );
        Ok(())
    }

    /// PL-97 C2: das Mausrad über jedem Fenster (Agenten, Jobs,
    /// Finished/Failed, LIVE) scrollt nur dieses Fenster — LIVE gar nichts —,
    /// über dem Chat nur den Chat; ein offener Dialog behält den Vorrang.
    #[test]
    fn wheel_over_each_mission_window_scrolls_only_that_window() -> TestResult {
        let mut app = test_chat_app()?;
        for index in 0..20 {
            running(
                &mut app,
                &format!("a{index:02}"),
                &format!("rolle-{index:02}"),
            );
        }
        app.agent_monitor.set_jobs(
            (0..12)
                .map(|index| {
                    crate::mission::projection::tests::job(
                        &format!("j{index}"),
                        true,
                        false,
                        "läuft",
                    )
                })
                .collect(),
        );
        for index in 0..100 {
            app.push_line(Role::System, format!("Zeile-{index:03}"));
        }
        let _ = screen(&app, 70, 90)?;
        let regions = app.last_regions.get();
        let agents = regions
            .agents
            .ok_or(TestError::Missing("Agenten-Fenster"))?;
        let jobs = regions.jobs.ok_or(TestError::Missing("Jobs-Fenster"))?;
        let finished = regions
            .finished
            .ok_or(TestError::Missing("Finished-Fenster"))?;
        let live = regions.live.ok_or(TestError::Missing("LIVE-Streifen"))?;
        let offsets = |app: &ChatApp| {
            (
                app.agent_monitor.panel_scroll_offset(),
                app.mission.jobs.scroll_offset(),
                app.mission.finished.scroll_offset(),
                app.scroll.is_following(),
            )
        };
        assert_eq!(offsets(&app), (0, 0, 0, true));

        let over = |area: Rect| wheel(MouseEventKind::ScrollDown, area.x + 1, area.y);
        assert_eq!(route_scroll_input(&mut app, &over(jobs)), Some(true));
        assert_eq!(offsets(&app), (0, WHEEL_LINES, 0, true), "only the jobs");
        assert_eq!(route_scroll_input(&mut app, &over(agents)), Some(true));
        assert_eq!(
            offsets(&app),
            (WHEEL_LINES, WHEEL_LINES, 0, true),
            "only the agents"
        );
        assert_eq!(
            route_scroll_input(&mut app, &over(finished)),
            Some(false),
            "the collapsed one-row strip has nothing to scroll"
        );
        assert_eq!(offsets(&app), (WHEEL_LINES, WHEEL_LINES, 0, true));
        assert_eq!(
            route_scroll_input(&mut app, &over(live)),
            Some(false),
            "LIVE consumes the wheel without scrolling"
        );
        assert_eq!(offsets(&app), (WHEEL_LINES, WHEEL_LINES, 0, true));
        let over_chat = wheel(MouseEventKind::ScrollUp, 2, live.y + live.height + 2);
        assert_eq!(route_scroll_input(&mut app, &over_chat), Some(true));
        assert_eq!(
            offsets(&app),
            (WHEEL_LINES, WHEEL_LINES, 0, false),
            "only the chat"
        );

        // Ctrl+↓ folgt dem Fokus: im Jobs-Fenster scrollen die Jobs.
        app.panels.focus = crate::panes::PaneFocus::Jobs;
        let ctrl_down = TuiEvent::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL));
        assert_eq!(route_scroll_input(&mut app, &ctrl_down), Some(true));
        assert_eq!(app.mission.jobs.scroll_offset(), WHEEL_LINES + 1);
        assert_eq!(app.agent_monitor.panel_scroll_offset(), WHEEL_LINES);
        // Eingeklappt fällt Ctrl+↓ im Finished-Fenster an das Agenten-Panel.
        app.panels.focus = crate::panes::PaneFocus::Finished;
        assert_eq!(route_scroll_input(&mut app, &ctrl_down), Some(true));
        assert_eq!(app.mission.finished.scroll_offset(), 0);
        assert_eq!(app.agent_monitor.panel_scroll_offset(), WHEEL_LINES + 1);

        // Aufgeklappt scrollt das Rad nur das Finished-Fenster.
        app.mission.finished_expanded = true;
        let _ = screen(&app, 70, 90)?;
        let finished = app
            .last_regions
            .get()
            .finished
            .ok_or(TestError::Missing("Finished-Fenster"))?;
        assert!(finished.height > 1, "{finished:?}");
        let before = offsets(&app);
        assert_eq!(route_scroll_input(&mut app, &over(finished)), Some(true));
        assert_eq!(
            offsets(&app),
            (before.0, before.1, before.2 + WHEEL_LINES, before.3),
            "only the expanded finished window"
        );

        // Ein offener Dialog behält den Vorrang vor allen Fenstern.
        app.pending_approval_dialog = Some(crowded_dialog(60));
        let _ = screen(&app, 70, 90)?;
        let dialog = app
            .last_regions
            .get()
            .dialog
            .ok_or(TestError::Missing("Dialogfläche"))?;
        let over_dialog = wheel(MouseEventKind::ScrollDown, dialog.x + 2, dialog.y + 2);
        let before = offsets(&app);
        assert_eq!(route_scroll_input(&mut app, &over_dialog), Some(true));
        assert_eq!(offsets(&app), before, "the dialog scrolls, no window does");
        Ok(())
    }

    /// PL-97 C2: eine nicht maximierte Detailansicht liegt über der ganzen
    /// Kartenzeile (Agenten ∪ Jobs). Das Rad über ihrer rechten Hälfte
    /// scrollt die Detailansicht, nie das verdeckte Jobs-Fenster.
    #[test]
    fn wheel_over_an_open_detail_in_the_card_row_scrolls_the_detail() -> TestResult {
        let mut app = test_chat_app()?;
        running(&mut app, "a1", "rolle-a");
        app.agent_monitor
            .set_jobs(vec![crate::mission::projection::tests::job(
                "j1", true, false, "läuft",
            )]);
        app.panels.focus = crate::panes::PaneFocus::Agents;
        assert!(app.open_agent_detail(), "detail of the selected agent");
        // F11 in der Detailansicht: sie bleibt in der Kartenzeile.
        app.panels.maximized = false;
        let _ = screen(&app, 70, 90)?;
        let regions = app.last_regions.get();
        let cards = regions.agents.ok_or(TestError::Missing("Kartenzeile"))?;
        assert_eq!(cards.width, 70, "the detail covers agents and jobs");
        assert_eq!(regions.jobs, None, "the hidden jobs window is no target");
        assert!(!regions.presence.jobs);

        let right_half = wheel(MouseEventKind::ScrollUp, cards.x + 50, cards.y + 1);
        assert_eq!(route_scroll_input(&mut app, &right_half), Some(true));
        assert_eq!(app.mission.jobs.scroll_offset(), 0, "jobs untouched");
        assert_eq!(
            app.agent_monitor.panel_scroll_offset(),
            0,
            "the detail scrolls, not the list behind it"
        );
        assert!(app.scroll.is_following(), "the chat stays put");
        Ok(())
    }

    /// `w`/`s` wählen im fokussierten Panel und bleiben im Composer
    /// Buchstaben; `scroll_panel_down` scrollt das Panel ohne Fokus.
    #[test]
    fn w_and_s_select_in_the_focused_panel_and_type_in_the_composer() -> TestResult {
        let mut app = test_chat_app()?;
        for index in 0..3 {
            running(&mut app, &format!("a{index}"), &format!("rolle-{index}"));
        }
        let key = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);

        app.panels.focus = crate::panes::PaneFocus::Agents;
        assert_eq!(handle_panel_key(&mut app, key('s')), Some(true));
        assert_eq!(app.agent_monitor.selected, 1);
        assert_eq!(
            handle_busy_event(&mut app, TuiEvent::Key(key('s'))),
            BusyKeyOutcome::Redraw
        );
        assert_eq!(app.agent_monitor.selected, 2);
        assert_eq!(handle_panel_key(&mut app, key('w')), Some(true));
        assert_eq!(app.agent_monitor.selected, 1);
        assert!(app.input.text().is_empty(), "nichts landet im Composer");

        app.panels.focus = crate::panes::PaneFocus::Chat;
        assert_eq!(handle_panel_key(&mut app, key('w')), None);
        handle_busy_event(&mut app, TuiEvent::Key(key('w')));
        handle_busy_event(&mut app, TuiEvent::Key(key('s')));
        assert_eq!(app.input.text(), "ws");
        assert_eq!(app.agent_monitor.selected, 1, "Auswahl unverändert");

        // `scroll_panel_down` (Ctrl+↓) wirkt ohne Fokus auf das sichtbare Panel.
        let _ = screen(&app, 120, 40)?;
        let ctrl_down = TuiEvent::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL));
        assert_eq!(route_scroll_input(&mut app, &ctrl_down), Some(true));
        assert_eq!(app.panels.focus, crate::panes::PaneFocus::Chat);
        // Ohne sichtbares Panel (schmal) geht die Taste normal weiter.
        let _ = screen(&app, 80, 24)?;
        assert_eq!(route_scroll_input(&mut app, &ctrl_down), None);
        Ok(())
    }
}
