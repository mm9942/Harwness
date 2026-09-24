//! Tests der Kind-Freigaben im Freigabedialog (Runde 5, Teil O).

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use harw_core::child_approval::{ChildApprovalAnswer, ChildApprovalBroker, ChildApprovalRequest};
use harw_tools::{ToolCall, ToolName};
use harw_types::{SessionId, ToolCallId};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use tokio::sync::oneshot;

use super::super::tests::test_chat_app;
use super::*;
use crate::test_support::{TestError, TestResult};

fn request() -> ChildApprovalRequest {
    ChildApprovalRequest {
        child: SessionId::new(),
        role: "uia-worker".to_owned(),
        tree_path: vec!["root-orchestrator".to_owned(), "uia-worker".to_owned()],
        call: ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("fs.write"),
            arguments: serde_json::json!({ "path": "src/lib.rs", "content": "x" }),
        },
        timeout: Duration::from_secs(600),
    }
}

/// App mit angebundenem Kanal; liefert den Broker für das „Kind“.
fn app_with_channel() -> TestResult<(ChatApp, TuiChildApprovalBroker)> {
    let mut app = test_chat_app()?;
    let (ui, broker) = ChildApprovalUi::channel();
    app.child_approvals = ui;
    Ok((app, broker))
}

fn submit(broker: &TuiChildApprovalBroker) -> TestResult<oneshot::Receiver<ChildApprovalAnswer>> {
    broker
        .submit(request())
        .ok_or(TestError::Missing("die Frage wird zugestellt"))
}

fn dialog_text(app: &ChatApp) -> TestResult<String> {
    let dialog = app
        .pending_approval_dialog
        .as_ref()
        .ok_or(TestError::Missing("offener Freigabedialog"))?;
    let area = Rect::new(0, 0, 140, dialog.desired_height(140));
    let mut buf = Buffer::empty(area);
    dialog.render(area, &mut buf, &crate::style::Theme::Dark);
    let mut text = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            text.push_str(buf[(x, y)].symbol());
        }
        text.push('\n');
    }
    Ok(text)
}

fn key(code: KeyCode) -> TuiEvent {
    TuiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

#[test]
fn a_child_request_appears_in_the_approval_dialog_with_its_sender() -> TestResult {
    let (mut app, broker) = app_with_channel()?;
    let _answer = submit(&broker)?;

    assert!(poll(&mut app), "die Frage öffnet den Dialog");
    assert!(is_open(&app));
    let text = dialog_text(&app)?;
    assert!(
        text.contains("angefragt von: uia-worker (Wurzel › root-orchestrator › uia-worker)"),
        "{text}"
    );
    assert!(text.contains("src/lib.rs"), "{text}");
    Ok(())
}

#[test]
fn approving_answers_the_child_and_closes_the_dialog() -> TestResult {
    let (mut app, broker) = app_with_channel()?;
    let mut answer = submit(&broker)?;
    poll(&mut app);
    app.child_approvals.backdate_open(Duration::from_secs(2));

    assert!(matches!(
        route_event(&mut app, key(KeyCode::Char('y'))),
        Ok(true)
    ));

    assert_eq!(answer.try_recv(), Ok(ChildApprovalAnswer::Approve));
    assert!(!is_open(&app));
    assert!(app.pending_approval_dialog.is_none());
    Ok(())
}

#[test]
fn rejecting_answers_the_child_with_a_rejection() -> TestResult {
    let (mut app, broker) = app_with_channel()?;
    let mut answer = submit(&broker)?;
    poll(&mut app);
    app.child_approvals.backdate_open(Duration::from_secs(2));

    assert!(matches!(
        route_event(&mut app, key(KeyCode::Char('n'))),
        Ok(true)
    ));

    assert_eq!(
        answer.try_recv(),
        Ok(ChildApprovalAnswer::Reject { reason: None })
    );
    assert!(!is_open(&app));
    Ok(())
}

/// Vor dem Arming-Delay zählt keine Taste — ein gerade tippendes „y“ darf
/// nicht versehentlich freigeben.
#[test]
fn keys_before_the_arming_delay_do_not_answer() -> TestResult {
    let (mut app, broker) = app_with_channel()?;
    let mut answer = submit(&broker)?;
    poll(&mut app);

    assert!(matches!(
        route_event(&mut app, key(KeyCode::Char('y'))),
        Ok(true)
    ));
    assert!(answer.try_recv().is_err(), "noch keine Antwort");
    assert!(is_open(&app));
    Ok(())
}

/// Gibt das Kind auf (Zeitablauf im Kern, Abbruch), schließt die TUI die
/// Frage still und meldet es als Systemzeile.
#[test]
fn a_request_the_child_gave_up_on_is_closed() -> TestResult {
    let (mut app, broker) = app_with_channel()?;
    let answer = submit(&broker)?;
    poll(&mut app);
    assert!(is_open(&app));

    drop(answer);
    assert!(poll(&mut app));
    assert!(!is_open(&app));
    assert!(app.pending_approval_dialog.is_none());
    Ok(())
}

/// Eine Frage der Wurzel hat Vorrang; die Kind-Frage kommt danach wieder.
#[test]
fn a_root_prompt_takes_precedence_and_the_child_prompt_returns() -> TestResult {
    let (mut app, broker) = app_with_channel()?;
    let _answer = submit(&broker)?;
    poll(&mut app);
    assert!(is_open(&app));

    yield_to_root(&mut app);
    assert!(!is_open(&app));
    assert_eq!(app.child_approvals.queued(), 1);

    assert!(show_next(&mut app));
    assert!(is_open(&app));
    Ok(())
}

/// Jede Zustimmung gilt nur für diesen einen Aufruf: kein Moduswechsel,
/// keine Regel für das Kind.
#[test]
fn every_approval_variant_is_a_single_approval() {
    assert_eq!(
        answer_for(ApprovalChoice::ApproveAndAutoMode),
        ChildApprovalAnswer::Approve
    );
    assert_eq!(
        answer_for(ApprovalChoice::ApproveAndRemember("git status".to_owned())),
        ChildApprovalAnswer::Approve
    );
    assert_eq!(
        answer_for(ApprovalChoice::Reject {
            reason: Some("nein".to_owned())
        }),
        ChildApprovalAnswer::Reject {
            reason: Some("nein".to_owned())
        }
    );
}

/// Nutzerentscheidung 2026-09-24: unter „Full Access" beantwortet die TUI
/// eine bereits unterwegs gewesene Kind-Frage selbst — kein Dialog.
#[tokio::test]
async fn full_access_answers_relayed_child_requests_without_a_dialog() -> TestResult {
    let (mut app, broker) = app_with_channel()?;
    let answer = submit(&broker)?;
    let prompt = app
        .child_approvals
        .receiver
        .as_mut()
        .ok_or(TestError::Missing("Kanal der Kind-Fragen"))?
        .try_recv()
        .map_err(|_| TestError::Missing("zugestellte Kind-Frage"))?;
    app.child_approvals.queue.push_back(prompt);

    assert!(!approve_waiting_in_mode(
        &mut app,
        Some(ApprovalMode::Delegated)
    ));
    assert!(approve_waiting_in_mode(
        &mut app,
        Some(ApprovalMode::FullAccess)
    ));
    assert!(!is_open(&app));
    assert!(app.pending_approval_dialog.is_none());
    assert!(matches!(answer.await, Ok(ChildApprovalAnswer::Approve)));
    Ok(())
}
