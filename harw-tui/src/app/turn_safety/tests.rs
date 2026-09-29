//! Tests des Abbruchschutzes (Runde 5, Teil O).

use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use harw_core::cancel::{CancelReason, CancelToken};
use harw_core::child_controller::ChildStatus;
use harw_types::SessionId;

use super::super::tests::test_chat_app;
use super::super::{BusyKeyOutcome, handle_busy_event};
use super::*;
use crate::test_support::TestResult;
use crate::tui_event::TuiEvent;

fn key(code: KeyCode) -> TuiEvent {
    TuiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

/// Sichtbarer Verlauf, eine Zeile je gerenderter Zeile.
fn visible(app: &ChatApp) -> String {
    app.cells
        .iter()
        .flat_map(|cell| cell.display_lines(400, crate::style::Theme::Dark))
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Enter während der Arbeit reiht die Nachricht ein und bricht nie ab —
/// auch nicht, wenn Kinder laufen.
#[test]
fn enter_during_a_running_turn_queues_and_never_cancels() -> TestResult {
    let mut app = test_chat_app()?;
    let cancel = CancelToken::new();
    app.active_cancel = Some(cancel.clone());
    app.esc_confirm.children_override = Some(2);
    for text in ["check aus wie weit er kam", "plane zuerst!"] {
        app.input.insert_str(text);
        let outcome = handle_busy_event(&mut app, key(KeyCode::Enter));
        assert_eq!(outcome, BusyKeyOutcome::Redraw);
    }
    assert!(!cancel.is_cancelled(), "Enter darf den Turn nie abbrechen");
    assert_eq!(
        app.pending_turns
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["check aus wie weit er kam", "plane zuerst!"],
        "die Nachrichten gehen im nächsten Turn raus"
    );
    Ok(())
}

/// Mit laufenden Kindern scharft das erste Esc nur die Nachfrage; erst das
/// zweite bricht Turn und Kinder ab.
#[test]
fn a_first_esc_with_running_children_only_asks_for_confirmation() -> TestResult {
    let mut app = test_chat_app()?;
    let cancel = CancelToken::new();
    app.active_cancel = Some(cancel.clone());
    app.esc_confirm.children_override = Some(2);

    assert_eq!(
        handle_busy_event(&mut app, key(KeyCode::Esc)),
        BusyKeyOutcome::Redraw
    );
    assert!(!cancel.is_cancelled(), "das erste Esc darf nicht abbrechen");
    assert!(app.esc_confirm.is_armed());
    let shown = visible(&app);
    assert!(
        shown
            .contains("Esc bricht den Turn und 2 laufende Agenten ab – nochmal Esc zum Bestätigen"),
        "{shown}"
    );

    assert_eq!(
        handle_busy_event(&mut app, key(KeyCode::Esc)),
        BusyKeyOutcome::Redraw
    );
    assert!(
        cancel.is_cancelled(),
        "das zweite Esc bestätigt den Abbruch"
    );
    assert_eq!(cancel.reason(), Some(CancelReason::User));
    assert!(!app.esc_confirm.is_armed());
    Ok(())
}

/// Ohne laufende Kinder bleibt Esc der sofortige Abbruch (Runde 4).
#[test]
fn esc_without_children_still_interrupts_immediately() -> TestResult {
    let mut app = test_chat_app()?;
    let cancel = CancelToken::new();
    app.active_cancel = Some(cancel.clone());
    app.esc_confirm.children_override = Some(0);

    handle_busy_event(&mut app, key(KeyCode::Esc));
    assert!(cancel.is_cancelled());
    Ok(())
}

/// Hintergrund-Kinder und ihre Nachkommen zählen nicht (eigener Token);
/// beendete Kinder ebenso wenig.
#[test]
fn background_and_finished_children_are_not_counted() {
    let root = SessionId::new();
    let sync_child = SessionId::new();
    let background = SessionId::new();
    let grandchild = SessionId::new();
    let finished = SessionId::new();
    let nodes = vec![
        (sync_child.clone(), root.clone(), ChildStatus::Running),
        (background.clone(), root.clone(), ChildStatus::Running),
        (grandchild, background.clone(), ChildStatus::Running),
        (finished, root, ChildStatus::Completed),
    ];
    let detached: HashSet<String> = [background.as_str().to_owned()].into_iter().collect();
    assert_eq!(count_cancellable(&nodes, &detached), 1);
}

#[test]
fn the_hint_uses_the_singular_for_one_agent() {
    assert_eq!(
        esc_confirm_hint(1),
        "Esc bricht den Turn und 1 laufenden Agenten ab – nochmal Esc zum Bestätigen"
    );
}

/// Ein Turn-Ende an einer Grenze heißt nicht „vom Nutzer abgebrochen“.
#[test]
fn the_aborted_label_names_the_real_reason() {
    assert_eq!(
        aborted_label_for(Some(CancelReason::User)),
        "unvollständig (abgebrochen)"
    );
    assert_eq!(
        aborted_label_for(None),
        "unvollständig (abgebrochen: Turn-Grenze erreicht)"
    );
    assert!(aborted_label_for(Some(CancelReason::LeaseLost)).contains("Lease"));
}

// ── Reparatur offener Aufrufe (Runde 5, Teil O) ──────────────────────────────

fn requested(turn: &harw_types::TurnId, call: &harw_types::ToolCallId) -> harw_protocol::TurnEvent {
    harw_protocol::TurnEvent::ToolCallRequested {
        turn_id: turn.clone(),
        call_id: call.clone(),
        tool_name: "transfer_to_root-orchestrator".to_owned(),
        arguments: serde_json::json!({ "task": "Exploration" }),
    }
}

/// Die Export-Ergebnisse zu `call`.
fn exported_results(app: &ChatApp, call: &harw_types::ToolCallId) -> Vec<(bool, Option<u64>)> {
    app.export_entries
        .iter()
        .filter_map(|entry| match entry {
            crate::export::ExportEntry::ToolResult {
                call_id,
                status,
                duration_ms,
                ..
            } if call_id == call.as_str() => Some((
                matches!(status, crate::export::ExportStatus::Success),
                *duration_ms,
            )),
            _ => None,
        })
        .collect()
}

/// Praxis-Transkript: der erste, längst erfolgreiche Handoff behält
/// sein echtes Ergebnis mit echter Dauer; ein späterer Abbruch des nächsten
/// Turns markiert nur dessen offenen Handoff.
#[test]
fn a_finished_transfer_keeps_its_result_when_a_later_turn_is_aborted() -> TestResult {
    use super::super::{TurnEventState, handle_turn_event};

    let mut app = test_chat_app()?;
    let mut state = TurnEventState::default();
    let first_turn = harw_types::TurnId::new();
    let exploration = harw_types::ToolCallId::new();
    handle_turn_event(&mut app, &mut state, requested(&first_turn, &exploration));
    handle_turn_event(
        &mut app,
        &mut state,
        harw_protocol::TurnEvent::ToolCallCompleted {
            turn_id: first_turn,
            call_id: exploration.clone(),
            result: harw_protocol::items::ToolCallResult::success(serde_json::json!(
                "Exploration fertig"
            )),
            duration_ms: 723_000,
            placement: None,
        },
    );

    let second_turn = harw_types::TurnId::new();
    let implementation = harw_types::ToolCallId::new();
    handle_turn_event(
        &mut app,
        &mut state,
        requested(&second_turn, &implementation),
    );
    handle_turn_event(
        &mut app,
        &mut state,
        harw_protocol::TurnEvent::TurnAborted {
            turn_id: second_turn,
        },
    );

    assert_eq!(
        exported_results(&app, &exploration),
        vec![(true, Some(723_000))],
        "genau ein Ergebnis: das echte, erfolgreiche mit echter Dauer"
    );
    assert_eq!(
        exported_results(&app, &implementation),
        vec![(false, Some(0))],
        "nur der wirklich offene Aufruf des abgebrochenen Turns"
    );
    Ok(())
}

/// Auch eine (etwa durch ein verlorenes Ereignis) hängengebliebene Zelle
/// eines **früheren** Turns wird von einem späteren Abbruch nicht als
/// „abgebrochen“ exportiert.
#[test]
fn an_abort_never_marks_calls_of_an_earlier_turn() -> TestResult {
    use super::super::{TurnEventState, handle_turn_event};

    let mut app = test_chat_app()?;
    let mut state = TurnEventState::default();
    let earlier = harw_types::TurnId::new();
    let stale = harw_types::ToolCallId::new();
    handle_turn_event(&mut app, &mut state, requested(&earlier, &stale));

    let later = harw_types::TurnId::new();
    handle_turn_event(
        &mut app,
        &mut state,
        harw_protocol::TurnEvent::TurnAborted { turn_id: later },
    );

    assert!(exported_results(&app, &stale).is_empty());
    Ok(())
}
