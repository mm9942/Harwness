//! ChatApp methods: monitoring.

use super::*;

impl ChatApp {
    /// Öffnet die Detailansicht für den im Agenten-Panel ausgewählten Agenten
    /// und maximiert das Panel dafür.
    ///
    /// # Rückgabe
    /// `true`, wenn ein Agent ausgewählt war (Redraw nötig).
    pub(super) fn open_agent_detail(&mut self) -> bool {
        // Plan R9, Teil F: ein gewählter Job öffnet seine Detailansicht
        // (Kopf und Log-Ende).
        if self.agent_monitor.selected_job().is_some() {
            return jobs_glue::open_detail(self);
        }
        let Some(agent) = self.agent_monitor.selected_agent() else {
            return false;
        };
        self.open_agent_detail_for(agent)
    }

    /// Opens the detail view of `agent` (Enter in the agents list, `Alt+L`
    /// for the LIVE source) and maximizes the panel for it.
    pub(super) fn open_agent_detail_for(&mut self, agent: SessionId) -> bool {
        // Ein angesehener Fehlschlag wandert in die Sammelzeile.
        self.agent_monitor.mark_seen(agent.as_str());
        let prev_maximized = self
            .agent_detail
            .as_ref()
            .map_or(self.panels.maximized, |detail| detail.prev_maximized);
        self.agent_detail = Some(AgentDetailState {
            agent,
            scroll: ChatScroll::new(),
            show_reasoning: true,
            prev_maximized,
            job: None,
            job_view: None,
        });
        self.panels.maximized = true;
        true
    }

    /// Schließt die Detailansicht und stellt den vorherigen Vollbild-Zustand
    /// des Agenten-Panels wieder her.
    pub(super) fn close_agent_detail(&mut self) {
        if let Some(detail) = self.agent_detail.take() {
            self.panels.maximized = detail.prev_maximized;
        }
        // PL-97 C2: eine per `Alt+L` geöffnete LIVE-Detailansicht ist zu.
        self.mission.live_expanded = false;
    }

    /// PL-97 C2: Enter im Jobs-Fenster öffnet die Detailansicht (Kopf und
    /// Log-Ende) des dort gewählten Jobs; sie lädt über
    /// [`jobs_glue::refresh`] wie die Detailansicht aus der Agentenliste.
    pub(super) fn open_job_window_detail(&mut self) -> bool {
        let Some(job) = crate::mission::jobs_window::selected_job(
            self.agent_monitor.jobs(),
            &self.mission.jobs,
        ) else {
            return false;
        };
        let prev_maximized = self
            .agent_detail
            .as_ref()
            .map_or(self.panels.maximized, |detail| detail.prev_maximized);
        self.agent_detail = Some(AgentDetailState {
            agent: SessionId(job.clone()),
            scroll: ChatScroll::new(),
            show_reasoning: true,
            prev_maximized,
            job: Some(job),
            job_view: None,
        });
        self.panels.maximized = true;
        let _ = jobs_glue::refresh(self, true);
        true
    }

    /// PL-97 C2 (`Alt+L`): öffnet die Detailansicht des Agenten, den das
    /// LIVE-Fenster gerade zeigt (Fokus: Agenten-Panel), bzw. schließt sie
    /// wieder. Im Hochformat (Kasten „Reasoning“) immer maximiert.
    /// PL-97 C3: mit dem LIVE-Fenster der Querformat-Mittelspalte erscheinen
    /// die Details dort an Ort und Stelle, LIVE bekommt den Fokus.
    pub(super) fn toggle_live_detail(&mut self) -> bool {
        if self.agent_detail.is_none()
            && (self.panels.revealed.live || self.mission.live_detail_agent().is_some())
        {
            return self.toggle_live_detail_in_place();
        }
        if self.mission.live_expanded {
            if self
                .agent_detail
                .as_ref()
                .is_some_and(|detail| detail.job.is_none())
            {
                self.close_agent_detail();
            }
            self.mission.live_expanded = false;
            return true;
        }
        let Some(agent) = self
            .mission
            .live_source(&self.agent_monitor, Instant::now())
            .map(|(live, _)| SessionId(live.id.clone()))
        else {
            return false;
        };
        self.panels.agents_visible = true;
        self.panels.focus = crate::panes::PaneFocus::Agents;
        let opened = self.open_agent_detail_for(agent);
        self.mission.live_expanded = opened;
        opened
    }

    /// PL-97 C3: `Alt+L`/Enter am LIVE-Fenster der Querformat-Mittelspalte —
    /// zeigt die bestehende Agenten-Detailansicht der LIVE-Quelle im selben
    /// Rechteck (LIVE fokussiert, ohne Vollbild) bzw. schließt sie wieder.
    pub(super) fn toggle_live_detail_in_place(&mut self) -> bool {
        if self.mission.live_detail_agent().is_some() {
            self.mission.close_live_detail();
            return true;
        }
        let Some(agent) = self
            .mission
            .live_source(&self.agent_monitor, Instant::now())
            .map(|(live, _)| live.id.clone())
        else {
            return false;
        };
        // Ein angesehener Fehlschlag wandert in die Sammelzeile (wie Enter in
        // der Agentenliste).
        self.agent_monitor.mark_seen(&agent);
        self.panels.agents_visible = true;
        self.panels.maximized = false;
        self.panels.focus = crate::panes::PaneFocus::Live;
        self.mission.open_live_detail(&agent);
        true
    }

    /// Schließt die Detailansicht, wenn das Agenten-Panel Fokus oder
    /// Sichtbarkeit verloren hat (F3/F4/Esc über die Panel-Logik). Eine
    /// Job-Detailansicht aus dem Jobs-Fenster bleibt dort offen.
    /// PL-97 C3: ebenso die LIVE-Details an Ort und Stelle, sobald LIVE den
    /// Fokus verliert.
    pub(super) fn sync_agent_detail(&mut self) {
        self.mission.sync_live_focus(self.panels.focus);
        let agents_active = self.panels.agents_visible
            && match self.panels.focus {
                crate::panes::PaneFocus::Agents => true,
                crate::panes::PaneFocus::Jobs => self
                    .agent_detail
                    .as_ref()
                    .is_some_and(|detail| detail.job.is_some()),
                _ => false,
            };
        if agents_active || self.agent_detail.is_none() {
            return;
        }
        let keep_maximized = self.panels.maximized;
        self.close_agent_detail();
        // Ein anderes Panel behält einen gerade gewählten Vollbild-Zustand;
        // der Chat braucht keinen.
        self.panels.maximized = match self.panels.focus {
            crate::panes::PaneFocus::Chat => false,
            _ => keep_maximized && self.panels.maximized,
        };
    }
}
