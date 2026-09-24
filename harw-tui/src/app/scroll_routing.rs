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
//! 3. sonst der Chat-Verlauf (wie bisher).
//!
//! Die Scroll-Aktionen der Tastenbelegung wirken ohne Fokuswechsel: bei
//! sichtbarem Dialog auf dessen Körper, sonst auf das sichtbare
//! Agenten-Panel; ist beides nicht sichtbar, geht die Taste normal weiter.
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

use crossterm::event::{KeyEvent, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use super::ChatApp;
use crate::chat_scroll::ScrollAction;
use crate::dialog_frame::WHEEL_LINES;
use crate::keybindings::KeyAction;
use crate::tui_event::TuiEvent;

/// Flächen des zuletzt gezeichneten Frames für das Maus-Routing.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameRegions {
    /// Das sichtbare Agenten-Panel (Liste oder Detailansicht).
    pub(crate) agents: Option<Rect>,
    /// Ein offener Dialog anstelle des Composers.
    pub(crate) dialog: Option<Rect>,
    /// Der Plan über dem Verlauf, solange die Plan-Freigabe offen ist.
    pub(crate) plan: Option<Rect>,
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

/// Mausrad nach Zeigerposition.
///
/// # Rückgabe
/// `None` für Nicht-Rad-Ereignisse (Klicks bleiben unbeachtet).
pub(crate) fn route_wheel(app: &mut ChatApp, mouse: MouseEvent) -> Option<bool> {
    let up = match mouse.kind {
        MouseEventKind::ScrollUp => true,
        MouseEventKind::ScrollDown => false,
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
    if hit(regions.agents) {
        return Some(scroll_agents(app, up, WHEEL_LINES));
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
                rows.iter().any(|row| row.contains("❯ 1. Ja")),
                "{width}x{height}: {shown}"
            );
            assert!(
                rows.iter().any(|row| row.contains("Nein (Esc)")),
                "{width}x{height}: {shown}"
            );
            assert!(
                rows.iter()
                    .any(|row| row.contains("↑↓") && row.contains("Enter") && row.contains("Esc")),
                "{width}x{height}: Hinweiszeile fehlt: {shown}"
            );
            assert!(
                rows.iter().any(|row| row.contains("Befehl ausführen?")),
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
    /// (ab 16 Zeilen Höhe), darüber erscheint das volle Panel.
    #[test]
    fn agent_panel_collapses_below_the_width_threshold() -> TestResult {
        let mut app = test_chat_app()?;
        with_agents(&mut app);
        for (width, height) in SIZES {
            let rows = screen(&app, width, height)?;
            let shown = rows.join("\n");
            let panel = rows.iter().any(|row| row.contains("Agenten · 1 aktiv"));
            let summary = rows.iter().any(|row| row.contains("Agenten: ● 1 aktiv"));
            if width >= crate::panes::AGENTS_PANEL_MIN_TERMINAL_WIDTH {
                assert!(panel && !summary, "{width}x{height}: {shown}");
                assert!(
                    rows.iter().any(|row| row.contains("✓ 6 fertig")),
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
            assert!(status.contains("Freigabe:"), "{width}: {status}");
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
        let shown = screen(&app, 120, 40)?.join("\n");
        assert!(!shown.contains("rolle-00"), "{shown}");

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
        for needle in ["❯ 1. Ja", "Nein (Esc)"] {
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
            shown.iter().any(|row| row.contains("❯ 1. Ja")),
            "Scrollen ändert die Auswahl nicht: {shown:?}"
        );
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
