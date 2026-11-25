//! ChatApp methods: agent event state.

use super::*;

impl ChatApp {
    /// Stores the detected project root path for display in the chat header.
    ///
    /// # Description
    /// Called from the composition root (`crate::runtime_root`) with the
    /// project root of the runtime assembly.
    /// The string is built from `project.project_root.display()` — a
    /// platform-appropriate path string. An empty string means no root was
    /// detected (graceful degradation).
    ///
    /// # Arguments
    /// - `root` (`String`): display-form of the project root path.
    ///
    /// # Returns
    /// `Self` for builder-style chaining.
    #[must_use]
    pub fn with_project_root(mut self, root: String) -> Self {
        self.project_root = root;
        self
    }

    /// Returns the detected project root path string.
    ///
    /// # Returns
    /// The project root as set by [`with_project_root`][Self::with_project_root],
    /// or an empty string if none was detected.
    #[must_use]
    pub(crate) fn project_root(&self) -> &str {
        &self.project_root
    }

    /// Abonniert den agenten-übergreifenden Live-Bus des Laufs.
    pub(crate) fn attach_agent_events(&mut self, hub: &harw_core::AgentEventHub) {
        self.agent_rx = Some(hub.subscribe());
    }

    /// Übernimmt die bereits verbrauchte Nutzung einer fortgesetzten Sitzung
    /// in Statuszeile und Agenten-Panel (sonst begänne `/resume` bei 0).
    pub(crate) fn seed_session_usage(&mut self, agent: &str, usage: &TokenUsage) {
        if *usage == TokenUsage::default() {
            return;
        }
        self.total_usage = usage.clone();
        self.agent_monitor
            .seed_usage(agent, "assistant", usage.clone());
    }

    /// Leert den Live-Bus nicht-blockierend in den [`crate::agent_monitor::AgentMonitor`].
    /// Matrix-Spielereignisse gehen stattdessen an eine offene generische
    /// Ansicht ([`OverlayView::apply_event`]); ist das die Matrix-Ansicht,
    /// wird ihr Zustand nachgeladen. Wissensänderungen
    /// (`AgentEventKind::Knowledge`) markieren nur die betroffenen Panels als
    /// veraltet ([`Self::apply_knowledge_event`]).
    /// Liefert `true`, wenn sich Sichtbares geändert hat.
    pub(crate) fn drain_agent_events(&mut self) -> bool {
        crate::i18n::with_language(self.language, || self.drain_agent_events_scoped())
    }

    pub(super) fn drain_agent_events_scoped(&mut self) -> bool {
        let mut changed = self.poll_explorer();
        // Runde 5, Teil C: offenes Kanban-Board alle 2 s gegen die Platte
        // prüfen (Spinner-Takt im Busy-Pfad, Leerlauf-Takt in `run_loop`).
        changed |= self.poll_kanban_live();
        // Runde 5, Teil L: Abschluss einer `/btw`-Nebenfrage im Spinner-Takt.
        changed |= self.poll_btw();
        // Runde 6, Teil B: fertige `!`-Befehle während eines Turns anzeigen
        // und als Kontext des nächsten Turns einreihen (im Leerlauf holt sie
        // `run_loop` ab und startet den Folge-Turn).
        if self.active_cancel.is_some() {
            changed |= operator_shell::poll_busy(self);
        }
        // Runde 5, Teil P: Goal-Marke und Schritt-Verlaufszeilen (max. 1×/s).
        changed |= self.poll_goal_marker();
        // Plan R9, Teil F: Job-Ereignisse und Jobs-Gruppe (gedrosselt).
        changed |= jobs_glue::poll(self);
        // PL-97 D5: DoD-Marke auch während eines Turns (nebenläufig).
        changed |= poll_dod_mark(self);
        // PL-97 M3: beendete Jobs beenden ihren Lebenszyklus (Regel 3).
        self.activity.sync_jobs(self.agent_monitor.jobs());
        let mut touched_agents: Vec<String> = Vec::new();
        let Some(rx) = self.agent_rx.as_mut() else {
            return changed;
        };
        let mut matrix_events: Vec<serde_json::Value> = Vec::new();
        let mut knowledge_areas: Vec<String> = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(event) => match &event.kind {
                    harw_core::AgentEventKind::Matrix { run_id, event } => {
                        matrix_events.push(serde_json::json!({
                            "run_id": run_id,
                            "event": event,
                        }));
                    }
                    harw_core::AgentEventKind::Knowledge { area, .. } => {
                        knowledge_areas.push(area.clone());
                    }
                    _ => {
                        // Runde 5, Teil I: Turn-Ereignisse der Kinder als
                        // Delta in ihren Live-Block (sofern vorhanden).
                        if let harw_core::AgentEventKind::Turn(turn) = &event.kind {
                            changed |= self.child_stream.apply(
                                &event.agent,
                                event.parent.as_ref(),
                                &event.role,
                                turn,
                            );
                        }
                        // Vor `apply`: der Monitor kennt so noch das laufende
                        // Werkzeug und die Phase vor dem Ereignis.
                        feed_agent_event(
                            &mut self.reasoning_feed,
                            &self.agent_monitor,
                            &self.session_id,
                            &event,
                        );
                        changed |= self.agent_monitor.apply(&event);
                        touched_agents.push(event.agent.as_str().to_owned());
                    }
                },
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(skipped)) => {
                    tracing::debug!(skipped, "tui.agent_events.lagged");
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    self.agent_rx = None;
                    break;
                }
            }
        }
        // Hintergrund-Kinder: Live-Zähler in ihre Verlaufszelle fortschreiben.
        touched_agents.sort_unstable();
        touched_agents.dedup();
        // PL-97 M3: Läufe der Agenten in den Aktivitäts-Reducer
        // (Fehlschlag-Bursts für Finished / Failed).
        self.activity
            .sync_agents(&self.agent_monitor, &touched_agents);
        for agent in &touched_agents {
            changed |= self.sync_child_cell(agent);
        }
        // Runde 5, Teil I: neue Kind-Zellen in die Ctrl+O-Liste.
        self.absorb_child_stream_handles();
        if !matrix_events.is_empty() {
            changed |= self.apply_matrix_events(&matrix_events);
        }
        for area in &knowledge_areas {
            changed |= self.apply_knowledge_event(area);
        }
        changed
    }

    /// Markiert nach einer Wissensänderung die betroffenen Panels als
    /// veraltet: das Werkbank-Panel bei `workbench`, eine offene generische
    /// Ansicht (Kanban-Board, KnowledgeBrowser), deren Nachlade-Befehl mit
    /// `/<area>` beginnt. Nachgeladen wird über den normalen Datenabruf
    /// (sichtbarkeitsgeprüfte Op), nicht aus dem Event.
    ///
    /// # Rückgabe
    /// `true`, wenn ein Panel markiert wurde.
    pub(super) fn apply_knowledge_event(&mut self, area: &str) -> bool {
        let mut marked = false;
        if area == "workbench" {
            self.workbench.mark_stale();
            marked = true;
        }
        let command = format!("/{area}");
        let matches_view = match &self.overlay {
            Some(Overlay::View(view)) => view.refresh_command().is_some_and(|refresh| {
                refresh
                    .split_whitespace()
                    .next()
                    .is_some_and(|head| head == command)
            }),
            _ => false,
        };
        if matches_view {
            self.queue_overlay_refresh();
            marked = true;
        }
        marked
    }

    /// Reicht Matrix-Ereignisse an die offene generische Ansicht weiter und
    /// reiht bei der Matrix-Ansicht deren Nachladen ein.
    ///
    /// # Rückgabe
    /// `true`, wenn eine Ansicht die Ereignisse erhielt (Redraw nötig).
    pub(super) fn apply_matrix_events(&mut self, events: &[serde_json::Value]) -> bool {
        let Some(Overlay::View(view)) = self.overlay.as_mut() else {
            return false;
        };
        for event in events {
            view.apply_event(event);
        }
        let is_matrix =
            view.refresh_command().as_deref() == Some(crate::matrix_view::REFRESH_COMMAND);
        if is_matrix {
            self.queue_overlay_refresh();
        }
        true
    }
}
