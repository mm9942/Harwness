//! Einhängepunkte der Live-Werte der Agentenbaum-Ansicht `/agent` in
//! [`ChatApp`] (Runde 5, Teil I).
//!
//! # Beschreibung
//! Die Logik lebt in [`crate::agent_tree_live`]; hier stehen nur die Brücken
//! zu privaten Teilen von `app.rs`: Wurzel-Beschriftung aus der Montage,
//! Anreicherung der Baumzeilen aus dem Agenten-Monitor und den gemerkten
//! Spawn-Aufträgen sowie der Leerlauf-Takt bei offener Ansicht.

use std::time::Instant;

use super::{ChatApp, Overlay};
use crate::agent_tree::AgentRow;
use crate::agent_tree_live::{enrich_rows, root_label};

impl ChatApp {
    /// Beschriftung der Wurzel: der explizit gestartete Wurzel-Agent, sonst
    /// `UIA · <aktive UIA-Definition>`, sonst `UIA`.
    pub(super) fn agent_tree_root_label(&self) -> String {
        let explicit = self
            .runtime
            .as_ref()
            .and_then(|rt| rt.spec().active_agent.clone());
        let uia = self
            .runtime
            .as_ref()
            .and_then(|rt| rt.config().harness.active_uia_definition.clone());
        root_label(explicit.as_deref(), uia.as_deref())
    }

    /// Ergänzt die Baumzeilen um Live-Werte (Monitor) und Aufträge
    /// (Spawn-Ereignisse). Der Zustand der Ansicht bleibt unberührt.
    pub(super) fn enrich_agent_tree_rows(&self, rows: &mut [AgentRow]) {
        let child_stream = &self.child_stream;
        enrich_rows(
            rows,
            &self.agent_monitor,
            |id| child_stream.task_for(id),
            Instant::now(),
        );
    }

    /// `true`, solange die Agentenbaum-Ansicht offen ist (Leerlauf-Takt in
    /// `run_loop`, [`crate::agent_tree_live::AGENT_TREE_LIVE_INTERVAL`]).
    pub(crate) fn agent_tree_live_active(&self) -> bool {
        matches!(self.overlay, Some(Overlay::AgentTree(_)))
    }
}

#[cfg(test)]
mod tests {
    use harw_core::{AgentEvent, AgentEventKind};
    use harw_protocol::events::TurnEvent;
    use harw_types::{SessionId, TokenUsage, TurnId};

    use super::super::tests::test_chat_app;
    use super::super::{TurnEventState, handle_turn_event};
    use super::*;
    use crate::test_support::{TestError, TestResult};

    /// Ohne Montage heißt die Wurzel `UIA`, nicht mehr
    /// „Wurzel-Orchestrator“; die Wurzel zeigt Live-Werte der Sitzung.
    #[test]
    fn root_row_is_labelled_uia_and_shows_live_values() -> TestResult {
        let mut app = test_chat_app()?;
        let root = app.session_id().clone();
        app.agent_monitor.apply(&AgentEvent {
            agent: root.clone(),
            parent: None,
            role: "assistant-ui".to_owned(),
            kind: AgentEventKind::Turn(TurnEvent::UsageUpdated {
                turn_id: TurnId::new(),
                round: TokenUsage::default(),
                turn_total: TokenUsage {
                    input_tokens: 1200,
                    output_tokens: 300,
                    ..TokenUsage::default()
                },
                final_round: false,
            }),
        });
        let rows = app.agent_tree_rows();
        let root_row = rows.first().ok_or(TestError::Missing("Wurzelzeile"))?;
        assert_eq!(root_row.role, "UIA");
        assert!(root_row.live.is_some(), "Live-Werte der Sitzung");
        assert_eq!(root_row.tokens, Some(1500));
        Ok(())
    }

    /// Der Auftrag aus dem Spawn-Ereignis ist in der Ansicht sichtbar, und
    /// eine offene Ansicht behält ihren Zustand, während neue Live-Werte
    /// eintreffen (kein Neuaufbau).
    #[test]
    fn spawn_task_visible_and_view_state_survives_updates() -> TestResult {
        let mut app = test_chat_app()?;
        let mut state = TurnEventState::default();
        let child = SessionId::from_str("child-root-orch-tree");
        handle_turn_event(
            &mut app,
            &mut state,
            TurnEvent::ChildSpawned {
                turn_id: TurnId::new(),
                child: child.clone(),
                role: "root-orchestrator".to_owned(),
                question: Some("Baue das Modul X".to_owned()),
            },
        );
        let mut rows = vec![AgentRow {
            id: child.as_str().to_owned(),
            parent: Some(app.session_id().as_str().to_owned()),
            role: "root-orchestrator".to_owned(),
            depth: 1,
            status: "running".to_owned(),
            ..AgentRow::default()
        }];
        app.enrich_agent_tree_rows(&mut rows);
        let row = rows.first().ok_or(TestError::Missing("Kindzeile"))?;
        assert_eq!(row.task.as_deref(), Some("Baue das Modul X"));

        // Ansicht öffnen, Details der Wurzel zeigen, dann treffen neue
        // Live-Werte ein: die Anzeige ändert sich, der Ansichtszustand nicht.
        let mut tree = crate::agent_tree::AgentTree::default();
        let enter = crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Enter,
            crossterm::event::KeyModifiers::NONE,
        );
        tree.handle_key(enter, &app.agent_tree_rows());
        let area = ratatui::layout::Rect::new(0, 0, 100, 20);
        let render = |app: &ChatApp, tree: &crate::agent_tree::AgentTree| {
            let mut buffer = ratatui::buffer::Buffer::empty(area);
            tree.render(
                area,
                &mut buffer,
                crate::style::Theme::Dark,
                &app.agent_tree_rows(),
            );
            buffer
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        let before_state = format!("{tree:?}");
        let before = render(&app, &tree);
        app.agent_monitor.apply(&AgentEvent {
            agent: app.session_id().clone(),
            parent: None,
            role: "assistant-ui".to_owned(),
            kind: AgentEventKind::Turn(TurnEvent::UsageUpdated {
                turn_id: TurnId::new(),
                round: TokenUsage::default(),
                turn_total: TokenUsage {
                    input_tokens: 5000,
                    output_tokens: 700,
                    ..TokenUsage::default()
                },
                final_round: false,
            }),
        });
        let after = render(&app, &tree);
        assert_ne!(before, after, "Live-Werte erscheinen ohne Neuöffnen");
        assert!(after.contains("5.0k"), "{after}");
        assert_eq!(
            format!("{tree:?}"),
            before_state,
            "kein Neuaufbau der Ansicht"
        );
        Ok(())
    }
}
