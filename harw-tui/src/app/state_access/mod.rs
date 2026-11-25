//! ChatApp methods: state access.

use super::*;

impl ChatApp {
    /// Setzt die read-only Step-/TODO-Ansicht (h22). Der Aufrufer (Step-
    /// Owner, z. B. ein Driver oder die `plan`-Brücke) publisht den
    /// Bibliothekszustand; die TUI rendert nur. `None` blendet die Ansicht
    /// aus.
    #[allow(
        dead_code,
        reason = "h22: the step owner that publishes into the TUI is not wired yet"
    )]
    pub(crate) fn set_step_list(&mut self, list: Option<harw_step_list::StepList>) {
        self.step_list = list;
    }

    /// Legt das Explorer-Panel beim ersten Einblenden an und startet die
    /// Hintergrund-Indizierung ab der Projektwurzel (sonst ab dem cwd).
    pub(super) fn ensure_explorer(&mut self) {
        if self.explorer.is_some() || !self.panels.explorer_visible {
            return;
        }
        let root = if self.project_root.is_empty() {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        } else {
            std::path::PathBuf::from(&self.project_root)
        };
        let mut panel = crate::explorer_panel::ExplorerPanel::new(root);
        panel.start_indexing();
        self.explorer = Some(panel);
    }

    /// Übernimmt einen fertigen Explorer-Index (nicht-blockierend).
    pub(super) fn poll_explorer(&mut self) -> bool {
        self.explorer
            .as_mut()
            .is_some_and(crate::explorer_panel::ExplorerPanel::poll)
    }

    /// Verwirft den transienten Streaming-Text (finale Zelle liegt vor).
    pub(crate) fn clear_live_stream(&mut self) {
        self.live_stream.clear();
        self.live_reasoning.clear();
    }

    /// Token-Summe für die Statuszeile: live über alle Agenten, sobald der
    /// Bus Daten liefert, sonst die Turn-Summen aus `SessionEvent`s.
    pub(super) fn display_usage(&self) -> TokenUsage {
        let live = self.agent_monitor.totals();
        if live.total() >= self.total_usage.total() {
            live
        } else {
            self.total_usage.clone()
        }
    }

    /// Gibt das optional registrierte Memory-Backend zurück.
    #[must_use]
    pub(crate) fn memory(&self) -> Option<&std::sync::Arc<dyn harw_memory::Memory>> {
        self.memory.as_ref()
    }

    /// Gibt die zuletzt in `draw_viewport` berechnete Gesamtzeilenzahl der
    /// History zurück (`0` vor dem ersten Draw).
    ///
    /// # Rückgabe
    /// `u16` — Anzahl aller (gewrappten) History-Zeilen beim letzten Zeichnen.
    #[must_use]
    pub(crate) fn last_history_total_lines(&self) -> u16 {
        self.last_history_total_lines.get()
    }

    /// Nimmt eine abgesendete Eingabe in die Historie auf — im Speicher und auf
    /// Platte.
    ///
    /// # Beschreibung
    /// Der [`InputEditor`] hält die Historie für die Pfeiltasten-Navigation der
    /// laufenden Sitzung, der [`InputHistoryStore`] schreibt sie für die
    /// nächste fort. Beide Seiten werden hier gemeinsam bedient, damit keine
    /// Absende-Stelle die eine ohne die andere aktualisiert.
    ///
    /// # Argumente
    /// - `line` (`&str`): die abgesendete Eingabe im Originaltext.
    ///
    /// # Nebenläufigkeit
    /// Schreibt synchron auf Platte. Der Anhänge-Vorgang ist ein einzelner,
    /// kurzer Schreibzugriff und läuft im Event-Loop-Thread.
    pub(crate) fn remember_input(&mut self, line: &str) {
        // Runde 5, Teil L: `/btw` ist bewusst flüchtig — nie in die Historie.
        if btw::is_btw_line(line) {
            return;
        }
        self.input.push_history(line.to_owned());
        self.input_history.append(line);
    }

    /// Gibt die zuletzt in `draw_viewport` gemessene sichtbare Zeilenzahl des
    /// History-Bereichs zurück (`20` vor dem ersten Draw als sicherer
    /// Platzhalter-Fallback).
    ///
    /// # Rückgabe
    /// `u16` — Höhe des History-Bereichs (`history_area.height`) beim
    /// letzten Zeichnen.
    #[must_use]
    pub(crate) fn last_history_visible_rows(&self) -> u16 {
        self.last_history_visible_rows.get()
    }

    /// Wendet ausstehende Controller-Mutationen auf `session` an.
    ///
    /// # Beschreibung
    /// Delegiert an [`TuiSessionController::apply_to_session_checked`]. Darf nur
    /// zwischen Turns aufgerufen werden (nie während ein Turn läuft). Ist ein
    /// billiger No-op, wenn seit dem letzten Apply keine Setter aufgerufen wurden.
    ///
    /// Eine ausstehende Modell-/Provider-Route wird **vor** dem Schreiben in
    /// die Wurzelsitzung gegen den Provider-Lebenszyklus geprüft
    /// ([`live_model::check_pending_route`]). Ist das Modell abgeschaltet
    /// (retired), bleibt die Sitzung auf ihrer bisherigen Route, der
    /// Controller wird darauf zurückgesetzt (damit auch die Kind-Route über
    /// [`live_model::sync_live_main`] unverändert bleibt) und eine
    /// Systemzeile meldet die Ablehnung. Die Wechsel-Operationen lehnen ein
    /// abgeschaltetes Modell schon vor der Controller-Mutation ab; diese
    /// Prüfung fängt Auswahlen ab, die den Controller auf anderem Weg
    /// erreicht haben (Start-Auswahl, fremde Schreiber).
    ///
    /// # Argumente
    /// - `session` (`&mut AgentSession`): Die lebende Session, auf die Mutationen
    ///   übertragen werden (z. B. `reasoning_effort`).
    ///
    /// # Rückgabe
    /// `true`, wenn die Session tatsächlich verändert wurde.
    ///
    /// # Spec
    /// harw-tui Design §session_controller — apply_pending_controller_state.
    /// AP W5-05 — `/mode` wirkt an der Turn-Grenze. Löst zusätzlich einen
    /// während des letzten Turns vorgemerkten Shift+Tab-Zyklus ein (Plan
    /// Schritt 5, [`Self::cycle_permission_stage`]).
    pub(crate) fn apply_pending_controller_state(&mut self, session: &mut AgentSession) -> bool {
        if let Some(stage) = self.pending_permission_stage.take() {
            self.apply_permission_stage(stage);
        }
        // Lebenszyklus vor dem Schreiben in die Wurzelsitzung: eine
        // abgeschaltete Route erreicht weder die Sitzung noch (über
        // `sync_live_main`) die Kinder.
        let outcome = self
            .session_controller
            .apply_to_session_checked(session, |provider, model| {
                live_model::check_pending_route(self, provider, model)
            });
        let applied = outcome.applied;
        if let Some(rejected) = outcome.rejected_route {
            live_model::report_rejected_route(self, &rejected);
        }
        // Runde 5, Teil G: „wie UIA“-Worker folgen der Live-Auswahl der UIA.
        uia_workers::sync_live_uia(self, session);
        // Live-Modellwechsel: neu gestartete Kinder des Wurzel-Baums folgen
        // der generischen Auswahl (`/model switch`) ohne Neustart.
        live_model::sync_live_main(self);
        // Immer nachziehen, nicht nur bei `applied`: der Anzeigezustand soll
        // auch dann stimmen, wenn der Modus beim Aufbau der Session gesetzt
        // wurde oder ein anderer Pfad ihn verändert hat.
        self.active_mode = session.mode();
        // Runde 5, Teil F: Sperre an der Turn-Grenze mit dem Kern abgleichen.
        self.sync_plan_lock();
        if applied {
            tracing::debug!(
                mode = session.mode().as_str(),
                "tui.session_controller.applied_at_turn_boundary"
            );
        }
        applied
    }

    /// Gibt die `/`-Command-Adapter-Pipeline zurück.
    ///
    /// # Rückgabe
    /// Alle in dieser Session verfügbaren [`CommandAdapter`]s, in
    /// Registrierungsreihenfolge.
    #[must_use]
    pub(crate) fn adapters(&self) -> &[CommandAdapter] {
        &self.adapters
    }

    /// Geklonte Dispatch-Daten für einen Busy-Task (Runde 4, Teil H).
    pub(super) fn busy_dispatch(&self) -> BusyDispatch {
        BusyDispatch {
            language: self.interface_language(),
            runtime: self.runtime.clone(),
            adapters: Arc::clone(&self.adapters),
            sandbox: self.sandbox.clone(),
            session_id: self.session_id.clone(),
            #[cfg(test)]
            test_controller: self
                .busy_jobs
                .dispatches_without_runtime()
                .then(|| Arc::clone(&self.session_controller)),
        }
    }

    /// Gibt die Authority-Boundary für Operation-Dispatches zurück.
    ///
    /// # Rückgabe
    /// Referenz auf die in dieser Session eingefrorene [`SandboxSpec`].
    #[must_use]
    pub(crate) fn sandbox(&self) -> &SandboxSpec {
        &self.sandbox
    }

    /// Gibt die stabile Session-ID zurück.
    ///
    /// # Rückgabe
    /// Referenz auf die [`SessionId`] dieser Chat-Session.
    #[must_use]
    pub(crate) fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    #[must_use]
    /// Schreibt die Live-Zähler eines Kindes aus dem Agenten-Monitor in seine
    /// Verlaufszelle fort. `TurnEvent::ChildProgress` erreicht die Zelle nur,
    /// solange der startende Turn läuft; ein Hintergrund-Orchestrator meldet
    /// danach nur noch über den Agenten-Hub.
    ///
    /// # Rückgabe
    /// `true`, wenn sich Zähler geändert haben (Neuzeichnen nötig).
    pub(super) fn sync_child_cell(&self, agent_id: &str) -> bool {
        let (Some(cell), Some(live)) = (
            self.child_cells.get(agent_id),
            self.agent_monitor.agent(agent_id),
        ) else {
            return false;
        };
        let Ok(mut cell) = cell.lock() else {
            return false;
        };
        let tokens = live.usage().total();
        // Nie rückwärts: `ChildProgress` des Turns kann höher liegen als ein
        // verspätetes Hub-Ereignis.
        let tool_calls = cell.tool_calls.max(live.tool_calls);
        let tokens = cell.tokens.max(tokens);
        let counters_changed = tool_calls != cell.tool_calls || tokens != cell.tokens;
        let outcome = match live.phase {
            crate::agent_monitor::AgentPhase::Done => Some("completed"),
            crate::agent_monitor::AgentPhase::Failed => Some("failed"),
            crate::agent_monitor::AgentPhase::Cancelled => Some("cancelled"),
            _ => None,
        };
        let status_changed = outcome.is_some() && matches!(cell.status, SubAgentStatus::Running);
        if counters_changed {
            cell.apply_progress(tool_calls, tokens);
        }
        if status_changed && let Some(outcome) = outcome {
            let duration_ms = live
                .finished
                .unwrap_or(live.last_meaningful)
                .saturating_duration_since(live.started)
                .as_millis();
            cell.apply_completion(
                outcome.to_owned(),
                u64::try_from(duration_ms).unwrap_or(u64::MAX),
            );
        }
        counters_changed || status_changed
    }
}
