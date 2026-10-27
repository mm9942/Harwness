//! Anzeigereihenfolge der Kind-Statusmeldungen in [`ChatApp`] (PL-94 W01 M2b).
//!
//! # Verantwortungsbereich
//! Die Statuszeile zeigt je Kind-Agent, der einen Info-Zwischenstand
//! (`parent.message {kind: "info"}`) gesendet hat, ein Segment
//! (`ChatApp::child_infos`). Die Reihenfolge dieser Segmente kam bisher aus
//! einer `HashMap` und war damit zufällig und von Bild zu Bild instabil; bei
//! Platzmangel kürzt `fit_segments` bei gleicher Priorität zuerst das weiter
//! rechts stehende Segment. Hier bestimmt der reine
//! [`crate::live_ranker::LiveActivityRanker`] die Reihenfolge: dringendste und
//! zuletzt bedeutsam aktive Kinder zuerst, ruhige und beendete zuletzt.
//!
//! Der Ranker ist reine Anzeige. Er ändert nichts an Ausführung, Scheduling,
//! Rechten oder Verlauf; er bekommt nur eine Momentaufnahme je Kind
//! ([`ChildActivity`]) aus dem Agenten-Monitor und den laufenden
//! Hintergrund-Läufen ([`super::background_agents::running`]).
//!
//! # Abbildung auf Stufen
//! - ungesehener Fehlschlag: `NeedsOperator` (W01 Stufe 1);
//! - beendet (fertig, abgebrochen, gesehener Fehlschlag): `Terminal`;
//! - frischer Info-Zwischenstand (im 30s-Fenster der Statuszeile): `Milestone`;
//! - Phase `Admitted`/`Thinking`/`Tool`: `Active`;
//! - Phase `Waiting` oder dem Monitor unbekannt: `Quiet` (läuft, Zustand
//!   unbekannt — nicht „fertig“).
//!
//! # Anzeige
//! Beendete Kinder verlassen die Rangliste nach der Nachlaufzeit des Rankers,
//! ihr (konsolidiertes) Segment bleibt aber wie bisher sichtbar: sie stehen
//! hinter den gerankten Kindern, untereinander nach Name sortiert. Der Ranker
//! entscheidet also nur über die Reihenfolge, nie darüber, was gezeigt wird.

use std::time::Instant;

use super::ChatApp;
use crate::agent_monitor::{AgentLive, AgentPhase};
use crate::live_ranker::{ActivityTier, LiveActivityRanker, LiveCandidate, RankerConfig};
use crate::panes::PaneFocus;

/// Momentaufnahme eines Kindes für die Anzeigereihenfolge (reine Daten, damit
/// die Abbildung auf Stufen ohne Monitor testbar bleibt).
#[derive(Debug, Clone)]
pub(super) struct ChildActivity {
    /// Kennung des Kindes (Sitzungs-ID).
    pub(super) id: String,
    /// Phase laut Agenten-Monitor; `None`, solange der Monitor das Kind nicht
    /// kennt.
    pub(super) phase: Option<AgentPhase>,
    /// Ein Fehlschlag, den die Bedienerin noch nicht gesehen hat.
    pub(super) unseen_failure: bool,
    /// Letzte bedeutsame Beobachtung des Monitors.
    pub(super) last_event: Option<Instant>,
    /// Ende des Laufs laut Monitor.
    pub(super) finished_at: Option<Instant>,
    /// Zeitpunkt des letzten Info-Zwischenstands (`parent.message`).
    pub(super) info_at: Option<Instant>,
}

/// Abbildung auf die Anzeige-Stufe des Rankers (siehe Moduldoku).
fn tier_of(child: &ChildActivity, now: Instant) -> ActivityTier {
    let fresh_info = child
        .info_at
        .is_some_and(|at| super::child_info_visible(now.saturating_duration_since(at).as_secs()));
    match child.phase {
        _ if child.unseen_failure => ActivityTier::NeedsOperator,
        Some(AgentPhase::Done | AgentPhase::Failed | AgentPhase::Cancelled) => {
            ActivityTier::Terminal
        }
        _ if fresh_info => ActivityTier::Milestone,
        Some(AgentPhase::Admitted | AgentPhase::Thinking | AgentPhase::Tool) => {
            ActivityTier::Active
        }
        Some(AgentPhase::Waiting) | None => ActivityTier::Quiet,
    }
}

/// Zustand der Anzeigereihenfolge: der Ranker samt Bezugszeitpunkt für seine
/// Millisekundenuhr. Der Ranker selbst hat keine Uhr; alle Zeitstempel werden
/// relativ zu `epoch` (Start der App) in Millisekunden umgerechnet. Er liegt
/// in [`ChatApp`] hinter einer `RefCell`, weil die Statuszeile beim Zeichnen
/// nur `&ChatApp` hat, der Ranker aber Coalescing und Lead-Haltezeit
/// fortschreibt.
#[derive(Debug)]
pub(super) struct LiveOrder {
    ranker: LiveActivityRanker,
    epoch: Instant,
}

impl LiveOrder {
    /// Erzeugt den Zustand mit den vorgeschlagenen Standardparametern des
    /// Rankers.
    pub(super) fn new(epoch: Instant) -> Self {
        Self {
            ranker: LiveActivityRanker::new(RankerConfig::default()),
            epoch,
        }
    }

    /// Millisekunden seit `epoch`; Zeitpunkte davor zählen als 0.
    fn millis_since_epoch(&self, at: Instant) -> u64 {
        u64::try_from(at.saturating_duration_since(self.epoch).as_millis()).unwrap_or(u64::MAX)
    }

    /// Ranker-Kandidat zu einem Kind. Alle Kinder stammen aus dem Eingang
    /// bzw. den Hintergrund-Läufen dieser Sitzung und sind damit sichtbar.
    fn candidate(&self, child: &ChildActivity, now: Instant) -> LiveCandidate {
        let tier = tier_of(child, now);
        let last_meaningful = [child.last_event, child.info_at]
            .into_iter()
            .flatten()
            .max();
        let terminal_since_ms = if tier == ActivityTier::Terminal {
            child.finished_at.map(|at| self.millis_since_epoch(at))
        } else {
            None
        };
        LiveCandidate {
            run_id: child.id.clone(),
            session_visible: true,
            tier,
            last_meaningful_ms: last_meaningful.map_or(0, |at| self.millis_since_epoch(at)),
            reasoning_deltas_since_last_eval: 0,
            terminal_since_ms,
        }
    }

    /// Reihenfolge der Kinder zum Zeitpunkt `now`. `pin` hebt ein Kind an die
    /// Spitze (solange es nicht beendet ist).
    pub(super) fn order(
        &mut self,
        now: Instant,
        pin: Option<&str>,
        children: &[ChildActivity],
    ) -> Vec<String> {
        self.ranker.pin(pin.map(str::to_owned));
        let candidates: Vec<LiveCandidate> = children
            .iter()
            .map(|child| self.candidate(child, now))
            .collect();
        let now_ms = self.millis_since_epoch(now);
        self.ranker.rank(now_ms, &candidates)
    }
}

impl ChatApp {
    /// Momentaufnahme eines Kindes aus Monitor und Info-Zwischenständen.
    fn child_activity(&self, id: String) -> ChildActivity {
        let live = self.agent_monitor.agent(&id);
        let info_at = self.child_infos.get(&id).map(|(_, at)| *at);
        ChildActivity {
            phase: live.map(|live| live.phase),
            unseen_failure: live.is_some_and(AgentLive::is_unseen_failure),
            last_event: live.map(|live| live.last_meaningful),
            finished_at: live.and_then(|live| live.finished),
            info_at,
            id,
        }
    }

    /// Reihenfolge, in der die Info-Zwischenstände der Kinder in der
    /// Statuszeile stehen (siehe Moduldoku).
    ///
    /// # Beschreibung
    /// Kandidaten sind alle Kinder mit Info-Zwischenstand und alle laufenden
    /// Hintergrund-Läufe dieser Sitzung. Ist das Agenten-Panel fokussiert,
    /// pinnt die dort gewählte Zeile ihr Kind an die Spitze (W01: manuelle
    /// Auswahl schlägt die automatische Rangfolge, bis sie aufgehoben wird;
    /// `Esc` gibt den Fokus zurück).
    ///
    /// # Returns
    /// Genau die Schlüssel von `ChatApp::child_infos`, jeden einmal.
    pub(super) fn child_info_display_order(&self, now: Instant) -> Vec<String> {
        let mut ids: Vec<String> = self.child_infos.keys().cloned().collect();
        for run in super::background_agents::running(self) {
            let id = run.child.as_str().to_owned();
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        // Sortierte Eingabe: gleiche Lage ergibt immer dieselbe Reihenfolge,
        // unabhängig von der Iterationsreihenfolge der `HashMap`.
        ids.sort();
        let children: Vec<ChildActivity> =
            ids.into_iter().map(|id| self.child_activity(id)).collect();
        let pin = (self.panels.focus == PaneFocus::Agents)
            .then(|| self.agent_monitor.selected_agent())
            .flatten()
            .map(|id| id.as_str().to_owned());
        let ranked = match self.live_order.try_borrow_mut() {
            Ok(mut order) => order.order(now, pin.as_deref(), &children),
            Err(_) => Vec::new(),
        };
        let mut shown: Vec<String> = ranked
            .into_iter()
            .filter(|id| self.child_infos.contains_key(id))
            .collect();
        let mut rest: Vec<String> = self
            .child_infos
            .keys()
            .filter(|id| !shown.contains(id))
            .cloned()
            .collect();
        rest.sort();
        shown.extend(rest);
        shown
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use harw_core::{AgentEvent, AgentEventKind};
    use harw_protocol::events::TurnEvent;
    use harw_types::{SessionId, TurnId};

    use super::super::tests::test_chat_app;
    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn child(id: &str, phase: Option<AgentPhase>) -> ChildActivity {
        ChildActivity {
            id: id.to_owned(),
            phase,
            unseen_failure: false,
            last_event: None,
            finished_at: None,
            info_at: None,
        }
    }

    fn after(base: Instant, secs: u64) -> TestResult<Instant> {
        base.checked_add(Duration::from_secs(secs))
            .ok_or(TestError::Missing("Zeitpunkt nach der Basis"))
    }

    /// Die Abbildung folgt nur vertrauenswürdigem Zustand: Fehlschlag vor
    /// allem, beendet bleibt beendet (auch mit frischer Info), frische Info
    /// ist ein Meilenstein, sonst entscheidet die Phase.
    #[test]
    fn tiers_follow_the_monitor_state() -> TestResult {
        let base = Instant::now();
        let fresh = |phase| ChildActivity {
            info_at: Some(base),
            ..child("c", Some(phase))
        };
        assert_eq!(
            tier_of(
                &ChildActivity {
                    unseen_failure: true,
                    ..child("c", Some(AgentPhase::Failed))
                },
                base
            ),
            ActivityTier::NeedsOperator
        );
        assert_eq!(
            tier_of(&fresh(AgentPhase::Done), base),
            ActivityTier::Terminal
        );
        assert_eq!(
            tier_of(&fresh(AgentPhase::Thinking), base),
            ActivityTier::Milestone
        );
        assert_eq!(
            tier_of(&child("c", Some(AgentPhase::Tool)), base),
            ActivityTier::Active
        );
        assert_eq!(
            tier_of(&child("c", Some(AgentPhase::Waiting)), base),
            ActivityTier::Quiet
        );
        assert_eq!(tier_of(&child("c", None), base), ActivityTier::Quiet);
        // Nach dem 30s-Fenster ist aus dem Meilenstein wieder `Active` geworden.
        assert_eq!(
            tier_of(&fresh(AgentPhase::Thinking), after(base, 31)?),
            ActivityTier::Active
        );
        Ok(())
    }

    /// Innerhalb einer Stufe führt das zuletzt bedeutsam aktive Kind; die
    /// Stufe schlägt die Aktualität, die Namen spielen keine Rolle.
    #[test]
    fn order_prefers_tier_then_recency_not_the_name() -> TestResult {
        let epoch = Instant::now();
        let mut order = LiveOrder::new(epoch);
        let active = |id: &str, secs: u64| -> TestResult<ChildActivity> {
            Ok(ChildActivity {
                last_event: Some(after(epoch, secs)?),
                ..child(id, Some(AgentPhase::Thinking))
            })
        };
        let children = [
            active("a-old", 2)?,
            active("b-new", 8)?,
            ChildActivity {
                last_event: Some(after(epoch, 9)?),
                ..child("c-wait", Some(AgentPhase::Waiting))
            },
        ];
        let ranked = order.order(after(epoch, 10)?, None, &children);
        assert_eq!(ranked, ["b-new", "a-old", "c-wait"]);
        Ok(())
    }

    /// Ein Kind je Stufe, absichtlich so benannt, dass die alphabetische
    /// Reihenfolge der gewünschten genau entgegenläuft.
    fn app_with_three_children() -> TestResult<ChatApp> {
        let mut app = test_chat_app()?;
        let parent = app.session_id().clone();
        let event = |id: &str, kind: TurnEvent| AgentEvent {
            agent: SessionId::from_str(id),
            parent: Some(parent.clone()),
            role: "uia-worker".to_owned(),
            kind: AgentEventKind::Turn(kind),
        };
        app.agent_monitor.apply(&event(
            "z-fail",
            TurnEvent::TurnFailed {
                turn_id: TurnId::new(),
                reason: "kaputt".to_owned(),
                retryable: false,
            },
        ));
        app.agent_monitor.apply(&event(
            "m-run",
            TurnEvent::ContextUpdated {
                turn_id: TurnId::new(),
                used_tokens: 10,
                window_tokens: 100,
                history_items_dropped: 0,
                estimated_next_tokens: None,
                threshold_tokens: None,
                reserve_tokens: None,
            },
        ));
        app.agent_monitor.apply(&event(
            "a-done",
            TurnEvent::TurnAborted {
                turn_id: TurnId::new(),
            },
        ));
        for id in ["a-done", "m-run", "z-fail"] {
            app.note_child_info(id.to_owned(), "Zwischenstand".to_owned());
        }
        Ok(app)
    }

    /// Über den Helfer der App: Fehlschlag (`NeedsOperator`) vor frischer Info
    /// (`Milestone`) vor beendet (`Terminal`); das beendete Kind bleibt in der
    /// Anzeige, es verliert nur seinen Platz vorn.
    #[test]
    fn app_helper_orders_by_tier_and_keeps_every_entry() -> TestResult {
        let app = app_with_three_children()?;
        let order = app.child_info_display_order(Instant::now());
        assert_eq!(order, ["z-fail", "m-run", "a-done"]);
        Ok(())
    }

    /// Hat das Agenten-Panel den Fokus, pinnt die gewählte Zeile (hier die
    /// erste: das laufende Kind) ihr Kind an die Spitze, sogar vor den
    /// Fehlschlag.
    #[test]
    fn focused_agents_panel_pins_the_selected_child() -> TestResult {
        let mut app = app_with_three_children()?;
        app.panels.focus = PaneFocus::Agents;
        let order = app.child_info_display_order(Instant::now());
        assert_eq!(order, ["m-run", "z-fail", "a-done"]);
        Ok(())
    }

    /// Die Anzeige listet genau die Kinder mit Info-Zwischenstand; ein dem
    /// Monitor bekanntes Kind ohne Zwischenstand taucht nicht auf.
    #[test]
    fn display_order_lists_exactly_the_info_entries() -> TestResult {
        let mut app = app_with_three_children()?;
        app.agent_monitor.apply(&AgentEvent {
            agent: SessionId::from_str("q-ohne-info"),
            parent: Some(app.session_id().clone()),
            role: "uia-worker".to_owned(),
            kind: AgentEventKind::Turn(TurnEvent::TurnStarted {
                turn_id: TurnId::new(),
                thread_id: harw_types::ThreadId::new(),
            }),
        });
        let order = app.child_info_display_order(Instant::now());
        assert_eq!(order, ["z-fail", "m-run", "a-done"]);
        Ok(())
    }
}
