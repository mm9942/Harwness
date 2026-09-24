//! Einhängepunkte des Kind-Live-Streams in [`ChatApp`] (Runde 5, Teil I).
//!
//! # Beschreibung
//! Die Logik lebt in [`crate::child_stream`]; hier stehen nur die dünnen
//! Brücken zu privaten Teilen von `app.rs`: Block hinter die Agent-Zeile
//! hängen, neue Kind-Zellen in die Ctrl+O-Liste (`tool_cells`) übernehmen
//! und `/agent stream <modus>` anwenden.

use ratatui::text::Line;

use super::{ChatApp, ToolCellHandle};
use crate::child_stream::{
    ChildCellHandle, ChildStreamMode, ChildStreamRegistry, OrchestratorRoles,
};
use crate::history_cell::ToolVerbosity;
use crate::sanitize::sanitize_inline;

impl ChatApp {
    /// Setzt Startmodus (`[tui] child_stream`) und Orchestrator-Einstufung
    /// des Kind-Live-Streams (Builder, Composition-Root).
    ///
    /// # Argumente
    /// - `mode` ([`ChildStreamMode`]): Startmodus.
    /// - `roles` ([`OrchestratorRoles`]): Einstufung aus den
    ///   Agentendefinitionen (eingebaut + lokal).
    #[must_use]
    pub(crate) fn with_child_stream(
        mut self,
        mode: ChildStreamMode,
        roles: OrchestratorRoles,
    ) -> Self {
        self.child_stream = ChildStreamRegistry::new(mode, roles);
        self
    }

    /// Hängt den Live-Block eines Kindes der Wurzel direkt hinter dessen
    /// gerade angehängte Agent-Zeile — sofern das Kind im aktuellen Modus
    /// streamt. Sonst bleibt es bei der Zusammenfassungszeile.
    ///
    /// # Argumente
    /// - `child_id` (`&str`): Session-Kennung des Kindes.
    /// - `role` (`&str`): Rollenname aus `TurnEvent::ChildSpawned`.
    /// - `question` (`Option<&str>`): Auftrag des Kindes (für `/agent`).
    pub(super) fn attach_child_stream_block(
        &mut self,
        child_id: &str,
        role: &str,
        question: Option<&str>,
    ) {
        if let Some(question) = question {
            self.child_stream.remember_task(child_id, question);
        }
        if let Some(block) = self.child_stream.attach_root_child(child_id, role) {
            self.push_shared_cell(block);
            self.absorb_child_stream_handles();
        }
    }

    /// Übernimmt neu entstandene Werkzeug-/Reasoning-Zellen der Kind-Blöcke
    /// in die Ctrl+O-Liste; bei aktiver ausführlicher Anzeige (`/verbose`)
    /// gleich ausgeklappt, wie neue Zellen der Wurzel.
    pub(super) fn absorb_child_stream_handles(&mut self) {
        let verbose = self.tool_verbosity == ToolVerbosity::Verbose;
        for handle in self.child_stream.take_new_handles() {
            let handle = match handle {
                ChildCellHandle::Tool(cell) => ToolCellHandle::Single(cell),
                ChildCellHandle::Group(group) => ToolCellHandle::Group(group),
                ChildCellHandle::Reasoning(cell) => ToolCellHandle::Reasoning(cell),
            };
            if verbose {
                handle.set_expanded(true);
            }
            self.tool_cells.push(handle);
        }
    }

    /// `/agent stream [<orchestrators|all|none>]`: zeigt bzw. setzt den
    /// Modus für diese Sitzung (sofort wirksam, auch während eines Turns).
    ///
    /// # Argumente
    /// - `args` (`&str`): Text hinter `stream` (leer = aktuellen Modus zeigen).
    pub(super) fn apply_child_stream_command(&mut self, args: &str) {
        let args = args.trim();
        let text = if args.is_empty() {
            format!(
                "Live-Stream der Kind-Agenten: {} (ändern: /agent stream <orchestrators|all|none>)",
                self.child_stream.mode().as_str()
            )
        } else if let Some(mode) = ChildStreamMode::parse(args) {
            self.child_stream.set_mode(mode);
            format!(
                "Live-Stream der Kind-Agenten für diese Sitzung: {}",
                mode.as_str()
            )
        } else {
            format!(
                "Unbekannter Modus „{}“ — erlaubt: orchestrators, all, none",
                sanitize_inline(args)
            )
        };
        self.push_lines(vec![Line::from(text)]);
    }
}

#[cfg(test)]
mod tests {
    use harw_protocol::events::TurnEvent;
    use harw_registry_defaults::profile::role_names;
    use harw_types::{SessionId, ToolCallId, TurnId};
    use serde_json::json;

    use super::super::{TurnEventState, handle_turn_event, tests::test_chat_app};
    use super::*;
    use crate::style;
    use crate::test_support::{TestError, TestResult};

    fn rendered(app: &ChatApp) -> String {
        app.cells
            .iter()
            .flat_map(|cell| cell.display_lines(120, style::Theme::Dark))
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Ein `root-orchestrator`-Kind bekommt direkt hinter seiner Agent-Zeile
    /// einen Block; Bus-Ereignisse füllen ihn, ohne weitere Verlaufszellen
    /// anzulegen (kein Neuaufbau), und landen in der Ctrl+O-Liste.
    #[test]
    fn orchestrator_child_streams_under_its_agent_line() -> TestResult {
        let mut app = test_chat_app()?;
        let mut state = TurnEventState::default();
        let root = SessionId::from_str("root-uia");
        let orch = SessionId::from_str("child-root-orchestrator-9");
        let before = app.cells_len();
        assert!(handle_turn_event(
            &mut app,
            &mut state,
            TurnEvent::ChildSpawned {
                turn_id: TurnId::new(),
                child: orch.clone(),
                role: role_names::ROOT_ORCHESTRATOR.to_owned(),
                question: Some("Baue X".to_owned()),
            }
        ));
        assert_eq!(app.cells_len(), before + 2, "Agent-Zeile + Live-Block");

        let tool_cells_before = app.tool_cells.len();
        let call = ToolCallId::new();
        app.child_stream.apply(
            &orch,
            Some(&root),
            role_names::ROOT_ORCHESTRATOR,
            &TurnEvent::ToolCallRequested {
                turn_id: TurnId::new(),
                call_id: call,
                tool_name: "shell.exec".to_owned(),
                arguments: json!({"cmd": ["git", "log"]}),
            },
        );
        app.absorb_child_stream_handles();
        assert_eq!(app.cells_len(), before + 2, "kein neuer Verlaufseintrag");
        assert_eq!(app.tool_cells.len(), tool_cells_before + 1, "Ctrl+O");

        let text = rendered(&app);
        let agent_line = text
            .lines()
            .position(|line| line.contains(role_names::ROOT_ORCHESTRATOR))
            .ok_or(TestError::Missing("Agent-Zeile"))?;
        let next = text
            .lines()
            .nth(agent_line + 1)
            .ok_or(TestError::Missing("Blockzeile"))?;
        assert!(
            next.starts_with("  │ "),
            "eingerückt unter der Agent-Zeile: {text}"
        );
        Ok(())
    }

    /// Worker-Kinder der Wurzel erzeugen weiterhin nur ihre Agent-Zeile.
    #[test]
    fn worker_child_keeps_single_summary_line() -> TestResult {
        let mut app = test_chat_app()?;
        let mut state = TurnEventState::default();
        let before = app.cells_len();
        assert!(handle_turn_event(
            &mut app,
            &mut state,
            TurnEvent::ChildSpawned {
                turn_id: TurnId::new(),
                child: SessionId::from_str("child-explorer-9"),
                role: role_names::EXPLORER.to_owned(),
                question: None,
            }
        ));
        assert_eq!(app.cells_len(), before + 1);
        Ok(())
    }

    /// `/agent stream none` blendet einen bestehenden Block sofort aus.
    #[test]
    fn agents_stream_command_switches_mode() -> TestResult {
        let mut app = test_chat_app()?;
        let mut state = TurnEventState::default();
        let root = SessionId::from_str("root-uia");
        let orch = SessionId::from_str("child-coding-orch-9");
        handle_turn_event(
            &mut app,
            &mut state,
            TurnEvent::ChildSpawned {
                turn_id: TurnId::new(),
                child: orch.clone(),
                role: role_names::CODING_ORCHESTRATOR.to_owned(),
                question: None,
            },
        );
        app.child_stream.apply(
            &orch,
            Some(&root),
            role_names::CODING_ORCHESTRATOR,
            &TurnEvent::AssistantDelta {
                turn_id: TurnId::new(),
                text: "Plane Welle 1".to_owned(),
            },
        );
        assert!(rendered(&app).contains("Plane Welle 1"));
        app.apply_child_stream_command("none");
        assert_eq!(app.child_stream.mode(), ChildStreamMode::Off);
        assert!(!rendered(&app).contains("● Plane Welle 1"));
        app.apply_child_stream_command("bogus");
        assert_eq!(app.child_stream.mode(), ChildStreamMode::Off);
        app.apply_child_stream_command("orchestrators");
        assert!(rendered(&app).contains("Plane Welle 1"));
        Ok(())
    }
}
