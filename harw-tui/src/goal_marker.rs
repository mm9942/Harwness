//! Sichtbare Goal-Marke der TUI (Runde 5, Teil P).
//!
//! # Verantwortung
//! Solange ein Goal aktiv ist, steht in der Statuszeile eine feste, farbig
//! hervorgehobene Marke „◎ Goal: <Titel> · 2/5“ (Fortschritt aus dem
//! gebundenen Plan, ohne Plan nur der Titel). Ist ein Schritt blockiert,
//! erscheint sie in Warnfarbe mit „⚠ blockiert“. Bei schmalem Terminal wird
//! der Titel gekürzt.
//!
//! Zusätzlich meldet [`GoalTracker`] Übergänge als kurze Verlaufszeilen:
//! Goal gesetzt, Schritt erledigt (mit Beleg), Schritt blockiert, Goal
//! erreicht — etwa „◎ Schritt 2/5 erledigt: Secret-Key 0600 (cargo test -p x)“.
//!
//! # Aktualisierung
//! Die `ChatApp` ruft [`GoalTracker::poll`] im Spinner- bzw. Leerlauf-Takt
//! (`drain_agent_events`); höchstens einmal je [`GOAL_POLL_INTERVAL`] werden
//! Goal- und Plan-Store gelesen (beides RAM-Caches der Stores). Damit folgt
//! die Marke jeder Änderung — auch aus `/goal`, `/plan`, dem `plan`-Werkzeug
//! oder Jobs — ohne eigenes Ereignis und ohne Neuaufbau.
//!
//! # Nebenläufigkeit
//! Lebt exklusiv in der `ChatApp` (Renderer-Thread).

use std::time::{Duration, Instant};

use harw_plan::goal::{GoalStatus, GoalStore};
use harw_plan::types::{Plan, PlanNodeStatus};
use harw_plan::{PlanStore, has_blocked_step, plan_progress};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::sanitize::sanitize_inline;
use crate::style::{self, Theme};

/// Mindestabstand zwischen zwei Store-Abfragen.
pub(crate) const GOAL_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// Höchstlänge des Titels in Zeichen (vor der Breitenkürzung).
const TITLE_MAX_CHARS: usize = 40;

/// Die angezeigte Marke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GoalMarker {
    /// Kurzer Titel (Goal-Statement, sonst ID).
    pub(crate) title: String,
    /// `(erledigt, gesamt)` des gebundenen Plans, falls vorhanden.
    pub(crate) progress: Option<(usize, usize)>,
    /// Mindestens ein Schritt blockiert.
    pub(crate) blocked: bool,
}

/// Ein Schritt im Schnappschuss.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StepState {
    /// Knoten-ID.
    pub(crate) id: String,
    /// Status.
    pub(crate) status: PlanNodeStatus,
    /// Ziel des Schritts.
    pub(crate) objective: String,
    /// Lokator des jüngsten Belegs.
    pub(crate) evidence: Option<String>,
}

/// Goal- und Plan-Zustand zu einem Zeitpunkt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GoalSnapshot {
    /// Goal-ID.
    pub(crate) goal_id: String,
    /// Goal-Statement.
    pub(crate) statement: String,
    /// Goal-Status.
    pub(crate) status: GoalStatus,
    /// Schritte des gebundenen Plans (ohne abgelöste/invalidierte).
    pub(crate) steps: Vec<StepState>,
    /// `(erledigt, gesamt)`, falls ein Plan gebunden ist.
    pub(crate) progress: Option<(usize, usize)>,
    /// Mindestens ein Schritt blockiert.
    pub(crate) blocked: bool,
}

impl GoalSnapshot {
    /// Baut den Schnappschuss aus Goal und (optional) gebundenem Plan.
    #[must_use]
    pub(crate) fn new(goal: &harw_plan::goal::Goal, plan: Option<&Plan>) -> Self {
        let steps = plan
            .map(|plan| {
                plan.nodes
                    .iter()
                    .filter(|node| {
                        !matches!(
                            node.status,
                            PlanNodeStatus::Superseded | PlanNodeStatus::Invalidated
                        )
                    })
                    .map(|node| StepState {
                        id: node.id.as_str().to_owned(),
                        status: node.status,
                        objective: node.objective.clone(),
                        evidence: node
                            .evidence
                            .last()
                            .map(|evidence| evidence.locator.clone()),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            goal_id: goal.id.as_str().to_owned(),
            statement: goal.statement.clone(),
            status: goal.status,
            steps,
            progress: plan.map(plan_progress),
            blocked: plan.is_some_and(has_blocked_step),
        }
    }

    /// Liest Goal und gebundenen (sonst aktiven) Plan aus den Stores.
    ///
    /// # Returns
    /// `None` ohne Goal.
    #[must_use]
    pub(crate) fn read(plans: &dyn PlanStore, goals: &dyn GoalStore) -> Option<Self> {
        let goal = goals.current().ok()?;
        let plan = match &goal.plan_id {
            Some(id) => plans.plan_by_id(id).ok(),
            None => None,
        };
        Some(Self::new(&goal, plan.as_ref()))
    }

    /// `true`, solange das Goal verfolgt wird (Marke sichtbar).
    fn is_open(&self) -> bool {
        matches!(
            self.status,
            GoalStatus::Draft | GoalStatus::Active | GoalStatus::Blocked
        )
    }

    /// Kurzer Titel: Statement, sonst ID; einzeilig und gekürzt.
    fn title(&self) -> String {
        let raw = if self.statement.trim().is_empty() {
            self.goal_id.as_str()
        } else {
            self.statement.as_str()
        };
        truncate_chars(&sanitize_inline(raw), TITLE_MAX_CHARS)
    }

    fn marker(&self) -> Option<GoalMarker> {
        self.is_open().then(|| GoalMarker {
            title: self.title(),
            progress: self.progress,
            blocked: self.blocked || self.status == GoalStatus::Blocked,
        })
    }
}

/// Kürzt auf höchstens `max` Zeichen (mit „…“).
fn truncate_chars(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_owned();
    }
    let keep = max.saturating_sub(1);
    let mut out: String = text.chars().take(keep).collect();
    out.push('…');
    out
}

/// Verfolgt Goal und Plan und liefert Marke und Verlaufszeilen.
#[derive(Debug, Default)]
pub(crate) struct GoalTracker {
    last: Option<GoalSnapshot>,
    marker: Option<GoalMarker>,
    initialized: bool,
    last_poll: Option<Instant>,
}

impl GoalTracker {
    /// Die aktuelle Marke (`None` ohne aktives Goal).
    #[must_use]
    pub(crate) fn marker(&self) -> Option<&GoalMarker> {
        self.marker.as_ref()
    }

    /// Liest die Stores, höchstens einmal je [`GOAL_POLL_INTERVAL`].
    ///
    /// # Returns
    /// `None`, wenn nicht gelesen wurde oder sich nichts änderte; sonst die
    /// neuen Verlaufszeilen (kann leer sein, wenn sich nur die Marke änderte).
    pub(crate) fn poll(
        &mut self,
        plans: &dyn PlanStore,
        goals: &dyn GoalStore,
        now: Instant,
    ) -> Option<Vec<String>> {
        if self
            .last_poll
            .is_some_and(|last| now.saturating_duration_since(last) < GOAL_POLL_INTERVAL)
        {
            return None;
        }
        self.last_poll = Some(now);
        self.observe(GoalSnapshot::read(plans, goals))
    }

    /// Übernimmt einen Schnappschuss (rein, testbar).
    ///
    /// # Returns
    /// Wie [`Self::poll`]. Der allererste Schnappschuss erzeugt keine
    /// Verlaufszeilen (kein Nachspielen alter Zustände beim Start).
    pub(crate) fn observe(&mut self, snapshot: Option<GoalSnapshot>) -> Option<Vec<String>> {
        if self.initialized && snapshot == self.last {
            return None;
        }
        let lines = if self.initialized {
            transition_lines(self.last.as_ref(), snapshot.as_ref())
        } else {
            Vec::new()
        };
        self.initialized = true;
        self.marker = snapshot.as_ref().and_then(GoalSnapshot::marker);
        self.last = snapshot;
        Some(lines)
    }

    /// Vergisst alles (keine Plan-Dienste).
    ///
    /// # Returns
    /// `true`, wenn vorher eine Marke sichtbar war.
    pub(crate) fn clear(&mut self) -> bool {
        let had = self.marker.is_some();
        self.last = None;
        self.marker = None;
        had
    }
}

/// Die Verlaufszeilen für den Übergang `before` → `after`.
fn transition_lines(before: Option<&GoalSnapshot>, after: Option<&GoalSnapshot>) -> Vec<String> {
    let Some(after) = after else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    let same_goal = before.is_some_and(|before| before.goal_id == after.goal_id);
    if !same_goal && after.is_open() {
        let steps = after
            .progress
            .map(|(_, total)| format!(" · {total} Schritt(e)"))
            .unwrap_or_default();
        lines.push(format!("◎ Goal gesetzt: {}{steps}", after.title()));
    }
    if let Some(before) = before.filter(|_| same_goal) {
        let total = after.progress.map_or(after.steps.len(), |(_, total)| total);
        let mut done = before.progress.map_or(0, |(done, _)| done);
        for step in &after.steps {
            let previous = before
                .steps
                .iter()
                .find(|old| old.id == step.id)
                .map(|old| old.status);
            if previous == Some(step.status) {
                continue;
            }
            match step.status {
                PlanNodeStatus::Completed => {
                    done = done.saturating_add(1).min(total);
                    let evidence = step
                        .evidence
                        .as_deref()
                        .map(|locator| {
                            format!(" ({})", truncate_chars(&sanitize_inline(locator), 60))
                        })
                        .unwrap_or_default();
                    lines.push(format!(
                        "◎ Schritt {done}/{total} erledigt: {}{evidence}",
                        truncate_chars(&sanitize_inline(&step.objective), 60)
                    ));
                }
                PlanNodeStatus::Blocked => lines.push(format!(
                    "⚠ Schritt {} blockiert: {}",
                    step.id,
                    truncate_chars(&sanitize_inline(&step.objective), 60)
                )),
                _ => {}
            }
        }
        if before.status != GoalStatus::Achieved && after.status == GoalStatus::Achieved {
            lines.push(format!("◎ Goal erreicht: {}", after.title()));
        }
    }
    lines
}

/// Eigene Farbe der Goal-Marke (Violett), abgesetzt von Plan-Modus (Petrol),
/// Akzent und Warnung.
#[must_use]
pub(crate) fn goal_color(theme: Theme) -> Color {
    if style::is_light(theme) {
        Color::Rgb(0x6A, 0x3D, 0x9A)
    } else {
        Color::Rgb(0xC3, 0x9B, 0xF5)
    }
}

/// Die Statuszeilen-Spans der Marke (leer ohne Marke).
///
/// # Arguments
/// - `marker`: aus [`GoalTracker::marker`].
/// - `width`: Breite der Statuszeile; die Marke nimmt höchstens ein Drittel
///   (mindestens 18 Zeichen), der Titel wird entsprechend gekürzt.
#[must_use]
pub(crate) fn status_spans(
    marker: Option<&GoalMarker>,
    theme: Theme,
    width: u16,
) -> Vec<Span<'static>> {
    let Some(marker) = marker else {
        return Vec::new();
    };
    let progress = marker
        .progress
        .map(|(done, total)| format!(" · {done}/{total}"))
        .unwrap_or_default();
    let blocked = if marker.blocked {
        " · ⚠ blockiert"
    } else {
        ""
    };
    let budget = usize::from(width / 3).max(18);
    let fixed =
        " ◎ Goal: ".chars().count() + progress.chars().count() + blocked.chars().count() + 1;
    let title = truncate_chars(&marker.title, budget.saturating_sub(fixed).max(4));
    let color = if marker.blocked {
        style::warning_color(theme)
    } else {
        goal_color(theme)
    };
    vec![Span::styled(
        format!(" ◎ Goal: {title}{progress}{blocked} "),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_plan::goal::{Goal, GoalId};
    use harw_plan::ids::{PlanId, RevisionId};
    use time::OffsetDateTime;

    fn goal(status: GoalStatus) -> Goal {
        Goal {
            id: GoalId::new("goal-p"),
            revision: 1,
            statement: "Secret-Key 0600".to_owned(),
            non_goals: Vec::new(),
            invariants: Vec::new(),
            acceptance_criteria: Vec::new(),
            constraints: Vec::new(),
            open_questions: Vec::new(),
            status,
            plan_id: Some(PlanId::new("p")),
            plan_revision: None,
            evidence: Vec::new(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            tenant: None,
        }
    }

    fn plan(statuses: &[PlanNodeStatus]) -> Plan {
        let nodes = statuses
            .iter()
            .enumerate()
            .map(|(index, status)| {
                let mut node = harw_plan::testing::base_node(format!("t-{index}"));
                node.status = *status;
                node.objective = format!("Schritt {index}");
                if *status == PlanNodeStatus::Completed {
                    node.evidence.push(harw_plan::types::EvidenceRef {
                        kind: harw_plan::types::EvidenceKind::CargoTest,
                        locator: "cargo test -p x".to_owned(),
                        attached_at: OffsetDateTime::UNIX_EPOCH,
                        actor: "test".to_owned(),
                        digest: None,
                    });
                }
                node
            })
            .collect();
        Plan {
            id: PlanId::new("p"),
            revision: RevisionId::new(1),
            parent_revision: None,
            goal_statement: "Secret-Key 0600".to_owned(),
            goal_id: Some("goal-p".to_owned()),
            nodes,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            tenant: None,
        }
    }

    fn snapshot(status: GoalStatus, steps: &[PlanNodeStatus]) -> Option<GoalSnapshot> {
        Some(GoalSnapshot::new(&goal(status), Some(&plan(steps))))
    }

    fn text(spans: &[Span<'static>]) -> String {
        spans.iter().map(|span| span.content.to_string()).collect()
    }

    use PlanNodeStatus::{Blocked, Completed, Draft, InProgress};

    #[test]
    fn marker_appears_with_an_active_goal_and_shows_progress() {
        let mut tracker = GoalTracker::default();
        tracker.observe(snapshot(
            GoalStatus::Active,
            &[Completed, InProgress, Draft],
        ));
        let spans = status_spans(tracker.marker(), Theme::Dark, 120);
        let rendered = text(&spans);
        assert!(
            rendered.contains("◎ Goal: Secret-Key 0600 · 1/3"),
            "{rendered}"
        );
        assert_eq!(spans[0].style.fg, Some(goal_color(Theme::Dark)));
    }

    #[test]
    fn blocked_step_switches_to_warning_colour() {
        let mut tracker = GoalTracker::default();
        tracker.observe(snapshot(GoalStatus::Active, &[Completed, Blocked]));
        let spans = status_spans(tracker.marker(), Theme::Dark, 120);
        assert!(text(&spans).contains("⚠ blockiert"));
        assert_eq!(spans[0].style.fg, Some(style::warning_color(Theme::Dark)));
    }

    #[test]
    fn marker_disappears_without_goal_or_after_achievement() {
        let mut tracker = GoalTracker::default();
        tracker.observe(snapshot(GoalStatus::Active, &[Draft]));
        assert!(tracker.marker().is_some());
        tracker.observe(None);
        assert!(tracker.marker().is_none());
        assert!(status_spans(tracker.marker(), Theme::Dark, 120).is_empty());
        tracker.observe(snapshot(GoalStatus::Achieved, &[Completed]));
        assert!(
            tracker.marker().is_none(),
            "erreichtes Goal zeigt keine Marke"
        );
    }

    #[test]
    fn narrow_terminal_shortens_the_title() {
        let marker = GoalMarker {
            title: "Ein sehr langer Goal-Titel, der nicht passt".to_owned(),
            progress: Some((2, 5)),
            blocked: false,
        };
        let rendered = text(&status_spans(Some(&marker), Theme::Dark, 60));
        assert!(rendered.contains('…'), "{rendered}");
        assert!(rendered.contains("2/5"), "{rendered}");
        assert!(rendered.chars().count() <= 22, "{rendered}");
    }

    #[test]
    fn history_lines_on_goal_set_step_done_blocked_and_achieved() {
        let mut tracker = GoalTracker::default();
        // Start ohne Goal: keine Zeilen.
        assert_eq!(tracker.observe(None), Some(Vec::new()));
        let set = tracker
            .observe(snapshot(GoalStatus::Active, &[InProgress, Draft]))
            .unwrap_or_default();
        assert_eq!(
            set,
            vec!["◎ Goal gesetzt: Secret-Key 0600 · 2 Schritt(e)".to_owned()]
        );

        let done = tracker
            .observe(snapshot(GoalStatus::Active, &[Completed, Draft]))
            .unwrap_or_default();
        assert_eq!(
            done,
            vec!["◎ Schritt 1/2 erledigt: Schritt 0 (cargo test -p x)".to_owned()]
        );

        let blocked = tracker
            .observe(snapshot(GoalStatus::Active, &[Completed, Blocked]))
            .unwrap_or_default();
        assert_eq!(
            blocked,
            vec!["⚠ Schritt t-1 blockiert: Schritt 1".to_owned()]
        );

        assert_eq!(
            tracker.observe(snapshot(GoalStatus::Active, &[Completed, Blocked])),
            None,
            "unverändert → nichts"
        );

        let achieved = tracker
            .observe(snapshot(GoalStatus::Achieved, &[Completed, Blocked]))
            .unwrap_or_default();
        assert_eq!(
            achieved,
            vec!["◎ Goal erreicht: Secret-Key 0600".to_owned()]
        );
    }

    #[test]
    fn first_observation_replays_nothing() {
        let mut tracker = GoalTracker::default();
        assert_eq!(
            tracker.observe(snapshot(GoalStatus::Active, &[Completed])),
            Some(Vec::new())
        );
        assert!(tracker.marker().is_some());
    }
}
