//! Einbindung der Goal-Marke in die `ChatApp` (Runde 5, Teil P).
//!
//! # Beschreibung
//! Die reine Logik (Schnappschuss, Übergänge, Spans) liegt in
//! [`crate::goal_marker`]. Dieses Kindmodul von `app` liest im Spinner- bzw.
//! Leerlauf-Takt (`drain_agent_events`) Goal- und Plan-Store der Montage,
//! schreibt Übergänge als Systemzeilen in den Verlauf und liefert der
//! Statuszeile die Marke.
//!
//! # Nebenläufigkeit
//! Renderer-Thread.

use std::sync::Arc;
use std::time::Instant;

use ratatui::text::Span;

use super::{ChatApp, Role};
use crate::style::Theme;

impl ChatApp {
    /// Aktualisiert die Goal-Marke (höchstens einmal je
    /// [`crate::goal_marker::GOAL_POLL_INTERVAL`]).
    ///
    /// # Rückgabe
    /// `true`, wenn sich Sichtbares änderte.
    pub(super) fn poll_goal_marker(&mut self) -> bool {
        self.poll_goal_marker_at(Instant::now())
    }

    /// Wie [`Self::poll_goal_marker`], mit fester Uhr (Tests).
    pub(super) fn poll_goal_marker_at(&mut self, now: Instant) -> bool {
        let Some(services) = self.plan_services.as_ref() else {
            return self.goal_tracker.clear();
        };
        let plans = Arc::clone(&services.plan_store);
        let goals = Arc::clone(&services.goal_store);
        let Some(lines) = self.goal_tracker.poll(plans.as_ref(), goals.as_ref(), now) else {
            return false;
        };
        for line in lines {
            self.push_line(Role::System, line);
        }
        true
    }

    /// Die Statuszeilen-Spans der Goal-Marke (leer ohne aktives Goal).
    pub(super) fn goal_status_spans(&self, theme: Theme, width: u16) -> Vec<Span<'static>> {
        crate::goal_marker::status_spans(self.goal_tracker.marker(), theme, width)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Instant;

    use harw_plan::actions::PlanAction;
    use harw_plan::goal::{Goal, GoalAction, GoalId, GoalStatus, GoalStore};
    use harw_plan::ids::{PlanId, TaskId};
    use harw_plan::types::{EvidenceKind, EvidenceRef, PlanNodeStatus};
    use harw_plan::{InMemoryGoalStore, InMemoryPlanStore, PlanStore};
    use time::OffsetDateTime;

    use super::super::TuiPlanServices;
    use super::super::tests::test_chat_app;
    use crate::goal_marker::GOAL_POLL_INTERVAL;
    use crate::style::Theme;
    use crate::test_support::{TestResult, ctx};

    fn status(id: &str, status: PlanNodeStatus) -> PlanAction {
        PlanAction::SetStatus {
            id: TaskId::new(id),
            status,
            reason: None,
        }
    }

    /// „Marke erscheint bei aktivem Goal“ und „Verlaufszeile beim
    /// Schrittwechsel“ über die echten Stores.
    #[test]
    fn step_completion_updates_marker_and_adds_a_history_line() -> TestResult {
        let plans: Arc<dyn PlanStore> = Arc::new(InMemoryPlanStore::new());
        let goals: Arc<dyn GoalStore> = Arc::new(InMemoryGoalStore::new());
        plans
            .apply(
                PlanAction::Create {
                    plan_id: PlanId::new("p-key"),
                    goal: "Secret-Key 0600".to_owned(),
                },
                "human:test",
            )
            .map_err(ctx("create"))?;
        plans
            .apply(
                PlanAction::AddNode {
                    node: harw_plan::testing::base_node("t-1"),
                },
                "human:test",
            )
            .map_err(ctx("add"))?;
        goals
            .apply(
                GoalAction::Set {
                    goal: Goal {
                        id: GoalId::new("goal-p-key"),
                        revision: 0,
                        statement: "Secret-Key 0600".to_owned(),
                        non_goals: Vec::new(),
                        invariants: Vec::new(),
                        acceptance_criteria: Vec::new(),
                        constraints: Vec::new(),
                        open_questions: Vec::new(),
                        status: GoalStatus::Active,
                        plan_id: Some(PlanId::new("p-key")),
                        plan_revision: None,
                        evidence: Vec::new(),
                        created_at: OffsetDateTime::UNIX_EPOCH,
                        updated_at: OffsetDateTime::UNIX_EPOCH,
                    },
                },
                "human:test",
            )
            .map_err(ctx("goal"))?;

        let mut app = test_chat_app()?.with_plan_services(TuiPlanServices {
            plan_store: Arc::clone(&plans),
            goal_store: Arc::clone(&goals),
        });
        let start = Instant::now();
        assert!(
            app.poll_goal_marker_at(start),
            "erste Abfrage setzt die Marke"
        );
        let cells_before = app.cells.len();
        let marker: String = app
            .goal_status_spans(Theme::Dark, 120)
            .iter()
            .map(|span| span.content.to_string())
            .collect();
        assert!(marker.contains("◎ Goal: Secret-Key 0600 · 0/1"), "{marker}");

        plans
            .apply_batch(
                &PlanId::new("p-key"),
                vec![
                    status("t-1", PlanNodeStatus::Ready),
                    status("t-1", PlanNodeStatus::InProgress),
                    PlanAction::AttachEvidence {
                        id: TaskId::new("t-1"),
                        evidence: EvidenceRef {
                            kind: EvidenceKind::CargoTest,
                            locator: "cargo test -p key_control".to_owned(),
                            attached_at: OffsetDateTime::UNIX_EPOCH,
                            actor: "test".to_owned(),
                            digest: None,
                        },
                    },
                    status("t-1", PlanNodeStatus::Completed),
                ],
                "human:test",
                plans.revision(),
            )
            .map_err(ctx("batch"))?;

        assert!(
            !app.poll_goal_marker_at(start),
            "innerhalb des Intervalls wird nicht gelesen"
        );
        assert!(app.poll_goal_marker_at(start + GOAL_POLL_INTERVAL));
        assert_eq!(app.cells.len(), cells_before + 1, "eine Verlaufszeile");
        let marker: String = app
            .goal_status_spans(Theme::Dark, 120)
            .iter()
            .map(|span| span.content.to_string())
            .collect();
        assert!(marker.contains("1/1"), "{marker}");
        Ok(())
    }

    /// „verschwindet ohne Goal“: ohne Plan-Dienste keine Marke.
    #[test]
    fn no_marker_without_plan_services() -> TestResult {
        let mut app = test_chat_app()?;
        assert!(!app.poll_goal_marker_at(Instant::now()));
        assert!(app.goal_status_spans(Theme::Dark, 120).is_empty());
        Ok(())
    }
}
