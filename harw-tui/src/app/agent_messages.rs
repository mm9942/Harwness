//! Kind-Meldungen an die UIA in der TUI (Runde 5, Teil M).
//!
//! # Verantwortungsbereich
//! Ein Kind der UIA-Wurzel (typisch der Root-Orchestrator im Hintergrund)
//! meldet sich über `parent.message`. Der Spawner legt die Meldung im
//! Eingang der Wurzel ab (`harw_core::ChildComms::take_parent_messages`);
//! diese Datei holt sie in der Ereignisschleife ab:
//! - eine Zeile im Verlauf („root-orchestrator › …", bei Fragen mit
//!   „Frage:"),
//! - die Meldung als Kontext für den nächsten UIA-Turn und — ist die UIA im
//!   Leerlauf — ein Auto-Turn über denselben Mechanismus wie die
//!   Hintergrund-Benachrichtigungen (`background_agents`). Bei einer Frage
//!   nennt der Kontext `agent.message {child_id, text}` als Antwortweg.
//!
//! - Runde 7, Teil B6: eine Meldung eines Kindes, dessen Lauf beim Abholen
//!   schon beendet ist (Journal geschlossen), ist veraltet — der Endbericht
//!   ist maßgeblich. Sie wird mit „(veraltet – Lauf bereits beendet)“
//!   gekennzeichnet, löst keinen Auto-Turn aus, und eine Frage verlangt
//!   keine Antwort mehr.
//!
//! # Nebenläufigkeit
//! Läuft auf dem Thread der Ereignisschleife; liest nur kurz den Eingang des
//! Spawners.

use harw_core::{ParentMessage, ParentMessageKind};

use super::background_agents::enqueue_background_notice;
use super::{ChatApp, Role};

/// Holt die Kind-Meldungen an die Wurzel ab, zeigt je Meldung eine Zeile
/// und reiht sie für den nächsten UIA-Turn ein (mit Auto-Turn im Leerlauf).
///
/// # Returns
/// `true`, wenn neue Meldungen abgeholt wurden (neu zeichnen).
pub(crate) fn collect_parent_messages(app: &mut ChatApp) -> bool {
    let Some(spawner) = app.managed_spawner().cloned() else {
        return false;
    };
    let comms = spawner.child_comms();
    let messages = comms.take_parent_messages(app.session_id());
    let any = !messages.is_empty();
    for message in messages {
        // Runde 7, Teil B6: Lauf schon beendet → veraltet. Ein bereits
        // verdrängtes Journal (unbekannt) gilt nicht als veraltet.
        let stale = comms
            .journal(&message.child)
            .is_some_and(|journal| !journal.is_running());
        tracing::info!(
            child = %message.child,
            kind = message.kind.as_str(),
            stale,
            "tui.parent_message.received"
        );
        if stale {
            app.push_line(
                Role::System,
                format!("{STALE_MARK} {}", message.display_line()),
            );
        } else {
            app.push_line(Role::System, message.display_line());
        }
        // Runde 6, Teil C: die volle Meldung (nicht nur der Anzeigeauszug)
        // als Agenten-Eintrag in den Export.
        super::export_capture::export_agent_event(
            app,
            crate::export::ExportAgentEntry {
                agent_id: message.child.as_str().to_owned(),
                role: Some(message.role.clone()),
                parent_id: Some(message.parent.as_str().to_owned()),
                status: Some(format!("parent.message:{}", message.kind.as_str())),
                summary: Some(message.text.clone()),
            },
        );
        if stale {
            enqueue_background_notice(app, stale_model_text(&message), false);
        } else {
            enqueue_background_notice(app, message.to_model_text(), true);
        }
        app.recency.push(crate::recency::RecencyEntry {
            child: message.child.as_str().to_owned(),
            text: message.text.clone(),
        });
        // Info-Zwischenstände des Kindes zusätzlich für die Statuszeile
        // merken (30s-Fenster, siehe `ChatApp::note_child_info`).
        if matches!(message.kind, ParentMessageKind::Info) {
            app.note_child_info(message.child.as_str().to_owned(), message.text.clone());
        }
    }
    any
}

/// Kennzeichnung veralteter Kind-Meldungen (Runde 7, Teil B6).
const STALE_MARK: &str = "(veraltet – Lauf bereits beendet)";

/// Modelltext einer veralteten Meldung: gekennzeichnet, ohne Antwortaufforderung.
///
/// # Arguments
/// - `message` (`&ParentMessage`): die Meldung eines schon beendeten Kindes.
///
/// # Returns
/// Den Kontexttext für den nächsten UIA-Turn.
fn stale_model_text(message: &ParentMessage) -> String {
    let what = match message.kind {
        ParentMessageKind::Info => "Nachricht",
        ParentMessageKind::Question => "Frage",
    };
    format!(
        "[{what} von {} ({}) {STALE_MARK}] {}\nDer Lauf ist bereits beendet; maßgeblich ist \
         sein Endbericht. Nicht beantworten.",
        message.role, message.child, message.text
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Instant;

    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_core::{ChildLimits, ManagedAgentSpawner, SessionManager};
    use harw_types::{SessionId, TenantId, WorkspaceId};

    use super::collect_parent_messages;
    use crate::app::ChatApp;
    use crate::app::background_agents::{attach_queued_notices, take_auto_turn};
    use crate::test_support::{TestError, TestResult, ctx};

    fn app_with_spawner(root: &SessionId) -> TestResult<(ChatApp, Arc<ManagedAgentSpawner>)> {
        let dir = std::env::temp_dir().join(format!(
            "harw-tui-parent-message-{}-{}",
            std::process::id(),
            SessionId::new()
        ));
        std::fs::create_dir_all(dir.join("workspace")).map_err(ctx("temp workspace dir"))?;
        let registry = WorkspaceRegistry::build(
            &dir,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("tui-parent-message"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )
        .map_err(ctx("workspace registry build"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("tui-parent-message"),
                &WorkspaceId::from_str("workspace"),
            )
            .map_err(ctx("resolve workspace binding"))?;
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        let (events, _receiver) = tokio::sync::mpsc::unbounded_channel();
        let spawner = Arc::new(ManagedAgentSpawner::new(
            Arc::new(Mutex::new(SessionManager::new(events))),
            ChildLimits::conservative(),
        ));
        let app = ChatApp::new(Vec::new(), sandbox, root.clone())
            .with_managed_spawner(Some(Arc::clone(&spawner)));
        Ok((app, spawner))
    }

    /// `parent.message {kind: "info"}` erscheint beim Elternteil (UIA) als
    /// Zeile und geht dem nächsten Turn als Kontext voran; im Leerlauf
    /// startet sie einen Auto-Turn.
    #[test]
    fn an_info_message_appears_at_the_uia_and_joins_the_next_turn() -> TestResult {
        let root = SessionId::new();
        let (mut app, spawner) = app_with_spawner(&root)?;
        let child = SessionId::new();
        let comms = spawner.child_comms();
        comms.open_journal(&child, &root, "root-orchestrator", Some("Umsetzen"));
        comms
            .post_info(
                &child,
                &root,
                "root-orchestrator",
                "Tests laufen, 3 von 5 Dateien fertig",
                Instant::now(),
            )
            .map_err(TestError::Unexpected)?;
        let cells_before = app.cells_len();
        assert!(collect_parent_messages(&mut app));
        assert_eq!(app.cells_len(), cells_before + 1, "eine Zeile im Verlauf");
        assert!(!collect_parent_messages(&mut app), "nur einmal zugestellt");
        let auto = take_auto_turn(&mut app).ok_or(TestError::Missing("Auto-Turn"))?;
        let turn = attach_queued_notices(&mut app, auto);
        assert!(
            turn.starts_with("[Nachricht von root-orchestrator"),
            "{turn}"
        );
        assert!(turn.contains("3 von 5 Dateien"));
        // Runde 6, Teil C: die Meldung steht als Agenten-Eintrag im Export.
        assert!(
            app.export_entries.iter().any(|entry| matches!(
                entry,
                crate::export::ExportEntry::Agent(agent)
                    if agent.summary.as_deref() == Some("Tests laufen, 3 von 5 Dateien fertig")
                        && agent.status.as_deref() == Some("parent.message:info")
            )),
            "parent.message fehlt im Export"
        );
        Ok(())
    }

    /// Eine Frage nennt `agent.message` als Antwortweg.
    #[test]
    fn a_question_names_the_answer_tool() -> TestResult {
        let root = SessionId::new();
        let (mut app, spawner) = app_with_spawner(&root)?;
        let child = SessionId::new();
        let comms = spawner.child_comms();
        comms.open_journal(&child, &root, "root-orchestrator", None);
        let _pending = comms
            .ask_parent(&child, &root, "root-orchestrator", "Tabelle A oder B?")
            .map_err(TestError::Unexpected)?;
        assert!(collect_parent_messages(&mut app));
        let turn = attach_queued_notices(&mut app, String::new());
        assert!(turn.contains("[Frage von root-orchestrator"), "{turn}");
        assert!(turn.contains("agent.message"), "{turn}");
        assert!(turn.contains(child.as_str()), "{turn}");
        Ok(())
    }

    /// Runde 7, Teil B6: eine Meldung, die erst nach dem Ende des Laufs
    /// abgeholt wird, ist als veraltet gekennzeichnet, weckt die UIA nicht
    /// und verlangt keine Antwort.
    #[test]
    fn a_message_of_a_finished_run_is_marked_stale() -> TestResult {
        let root = SessionId::new();
        let (mut app, spawner) = app_with_spawner(&root)?;
        let child = SessionId::new();
        let comms = spawner.child_comms();
        comms.open_journal(&child, &root, "root-orchestrator", Some("Umsetzen"));
        let _pending = comms
            .ask_parent(&child, &root, "root-orchestrator", "Tabelle A oder B?")
            .map_err(TestError::Unexpected)?;
        comms.close_journal(&child);
        assert!(collect_parent_messages(&mut app));
        assert!(
            take_auto_turn(&mut app).is_none(),
            "veraltete Meldung löst keinen Auto-Turn aus"
        );
        let turn = attach_queued_notices(&mut app, String::new());
        assert!(turn.contains("veraltet – Lauf bereits beendet"), "{turn}");
        assert!(turn.contains("Nicht beantworten"), "{turn}");
        assert!(!turn.contains("wartet auf eine Antwort"), "{turn}");
        Ok(())
    }
}
