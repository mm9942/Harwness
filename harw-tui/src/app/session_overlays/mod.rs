//! ChatApp methods: session overlays.

use super::*;

impl ChatApp {
    /// Setzt den Anzeigetitel der Sitzung (Plan Schritt 7).
    ///
    /// # Beschreibung
    /// Öffentlicher Setter, damit eine spätere Verdrahtung (Titel-Job-Ergebnis,
    /// `/resume`-Wiederherstellung) den Titel unabhängig vom Konstruktor
    /// nachtragen kann. Erscheint danach in der Statuszeile ([`status_line`]).
    ///
    /// # Argumente
    /// - `title` (`impl Into<String>`): der neue Anzeigetitel.
    pub fn set_session_title(&mut self, title: impl Into<String>) {
        self.session_title = Some(title.into());
    }

    /// Übernimmt ein im Session-Lifecycle bekannt gewordenes Modell für den
    /// Exportkopf. Ein vorhandener, explizit gesetzter Wert bleibt erhalten.
    pub(super) fn set_export_session_model(&mut self, model: impl Into<String>) {
        let model = model.into();
        if !model.trim().is_empty() {
            self.export_session_model = Some(model);
        }
    }

    /// Gibt den aktuellen Anzeigetitel der Sitzung zurück, falls bekannt.
    #[must_use]
    pub(crate) fn session_title(&self) -> Option<&str> {
        self.session_title.as_deref()
    }

    /// Öffnet den Session-Picker als Vollflächen-Overlay (Plan Schritt 7).
    ///
    /// # Beschreibung
    /// Aufgerufen von der Composition-Root (`runtime_root::run_tui`), nachdem
    /// `/resume` ohne Argument den Loop mit
    /// `TuiRunOutcome::Resume { selector: None }` verlassen hat und die
    /// verfügbaren Sitzungen geladen wurden. Ein bereits offenes Overlay wird
    /// ersetzt.
    ///
    /// # Argumente
    /// - `entries` (`Vec<SessionEntry>`): die anzuzeigenden Sitzungen.
    pub(crate) fn open_session_picker(&mut self, entries: Vec<SessionEntry>) {
        self.overlay = Some(Overlay::SessionPicker(SessionPicker::new(
            entries,
            SystemTime::now(),
        )));
    }

    /// Öffnet die `/export`-Zielauswahl als Vollflächen-Overlay.
    ///
    /// # Spec
    /// Contract „Nachträgliche Entscheidungen" (Slice E1): „In die
    /// Zwischenablage kopieren" / „Als Datei speichern" / „Abbrechen".
    pub(super) fn open_export_choice(&mut self) {
        crate::i18n::with_language(self.language, || self.open_export_choice_scoped());
    }

    pub(super) fn open_export_choice_scoped(&mut self) {
        self.overlay = Some(Overlay::ExportChoice(export_choice_dialog()));
    }

    /// Liest die aufgelöste Konfiguration aus derselben Runtime-Montage, die
    /// auch die Slash-Dienste bestückt — öffnet niemals eine zweite
    /// Config-Discovery.
    ///
    /// # Beschreibung
    /// Liefert den Live-Stand der Montage
    /// ([`harw_runtime::RuntimeAssembly::current_config`]): den Stand des
    /// Starts plus alle seither gespeicherten Änderungen — derselbe `Arc`,
    /// den jede über [`crate::runtime_commands::slash_service_map`] gebaute
    /// `ServiceMap` trägt. `None` ohne Runtime-Montage.
    ///
    /// # Rückgabe
    /// `Some(Arc<ResolvedConfig>)`, sofern verfügbar; sonst `None`.
    #[must_use]
    pub(super) fn resolved_config(&self) -> Option<Arc<harw_config::ResolvedConfig>> {
        // Live-Stand der Montage (Start + alle seither gespeicherten
        // Änderungen) — derselbe `Arc`, den jede frisch gebaute Slash-
        // `ServiceMap` trägt. Nie der Start-Schnappschuss: sonst zeigten
        // F7/F8 und die Picker nach einer Änderung weiter den alten Wert.
        let rt = self.runtime.as_ref()?;
        Some(rt.current_config())
    }

    /// Löst eine konfigurierte Provider-ID oder einen konfigurierten Namen zu
    /// ihrem kanonischen Namen auf (Spiegel von `harw_ops::provider`s
    /// gleichnamiger privater Hilfsfunktion, die von hier aus nicht
    /// referenzierbar ist).
    #[must_use]
    pub(super) fn canonical_provider_name<'a>(
        config: &'a harw_config::ResolvedConfig,
        requested: &str,
    ) -> Option<&'a str> {
        config.providers.iter().find_map(|(key, provider)| {
            (key == requested || provider.name == requested).then_some(provider.name.as_str())
        })
    }

    /// Liest den aktiven Provider (Snapshot des Controllers) oder — falls
    /// noch nicht explizit gewechselt — den Config-Default.
    ///
    /// # Beschreibung
    /// Dieselbe Präzedenz wie `harw_ops::provider::handle_show`: Laufzeit vor
    /// Konfiguration. Wird sowohl von [`Self::open_model_switch_picker`] (zur
    /// Vorauswahl) als auch vom `/uia-worker-model`-Trigger (zur Auflösung
    /// des effektiven UIA-Providers) verwendet.
    #[must_use]
    pub(super) fn active_or_default_provider(&self, config: &harw_config::ResolvedConfig) -> Option<String> {
        self.session_controller
            .snapshot()
            .active_provider
            .clone()
            .or_else(|| config.harness.default_provider.clone())
    }

    /// Öffnet den konsolidierten Provider/Modell-Umschalt-Picker (Welle 4a).
    ///
    /// # Beschreibung
    /// Ersetzt die vier vormaligen Öffner (`open_provider_choice`,
    /// `open_uia_provider_choice`, `open_model_choice`,
    /// `open_uia_model_choice`). Baut `providers` (nur aktivierte Provider,
    /// alphabetisch) und `models_by_provider` (alle konfigurierten Modelle,
    /// nach kanonischem Provider gruppiert) aus der aufgelösten Config; ohne
    /// Konfiguration oder ohne aktivierte Provider wird stattdessen eine
    /// klare Systemzeile angehängt — nie ein leerer Dialog (dieselbe
    /// Zurückhaltung wie die vormaligen Öffner).
    ///
    /// Die Vorauswahl unterscheidet sich je `target`:
    /// - Für [`PickerTarget::Orchestrator`]: Controller-Snapshot
    ///   (`active_provider`/`active_model`), sonst `default_provider`/
    ///   `default_model`.
    /// - Für [`PickerTarget::Uia`]: `config.harness.uia_provider`/`uia_model`,
    ///   sonst derselbe Fallback wie beim Orchestrator (aktiver/Standard-
    ///   Provider bzw. Controller-Snapshot-Modell).
    /// - [`PickerTarget::UiaWorker`]: `fixed_provider` ist bereits der
    ///   effektive UIA-Provider (vom Aufrufer aufgelöst); aktives Modell ist
    ///   `config.harness.uia_worker_model`.
    ///
    /// # Argumente
    /// - `target` (`PickerTarget`): Umschalt-Kontext (siehe oben).
    pub(crate) fn open_model_switch_picker(&mut self, target: PickerTarget) {
        crate::i18n::with_language(self.language, || {
            self.open_model_switch_picker_scoped(target)
        });
    }

    pub(super) fn open_model_switch_picker_scoped(&mut self, target: PickerTarget) {
        let context_label = target.context_label();

        let Some(config) = self.resolved_config() else {
            self.push_line(
                Role::System,
                tr_args(
                    "app_notices.no_config",
                    "{context} unavailable: no configuration loaded.",
                    &[("context", &context_label)],
                ),
            );
            return;
        };

        let mut providers: Vec<&harw_config::ProviderToml> = config
            .providers
            .values()
            .filter(|provider| provider.enabled)
            .collect();
        providers.sort_by(|left, right| left.name.cmp(&right.name));

        if providers.is_empty() {
            self.push_line(
                Role::System,
                tr_args(
                    "app_notices.no_providers",
                    "{context} unavailable: no enabled providers configured.",
                    &[("context", &context_label)],
                ),
            );
            return;
        }

        let provider_entries: Vec<ProviderEntry> = providers
            .iter()
            .map(|provider| ProviderEntry {
                id: provider.name.clone(),
                label: provider.name.clone(),
            })
            .collect();

        let mut models: Vec<&harw_config::ModelToml> = config.models.values().collect();
        models.sort_by(|left, right| left.id.cmp(&right.id));

        let mut models_by_provider: Vec<(String, Vec<ModelEntry>)> = Vec::new();
        for model in models {
            let Some(canonical) = Self::canonical_provider_name(&config, &model.provider) else {
                continue;
            };
            let label = model.name.as_deref().unwrap_or(model.id.as_str());
            let entry = ModelEntry {
                id: model.id.clone(),
                label: format!("{label} ({})", model.id),
            };
            match models_by_provider
                .iter_mut()
                .find(|(id, _)| id.as_str() == canonical)
            {
                Some((_, list)) => list.push(entry),
                None => models_by_provider.push((canonical.to_owned(), vec![entry])),
            }
        }

        let snap = self.session_controller.snapshot();
        let (active_provider, active_model): (Option<String>, Option<String>) = match &target {
            PickerTarget::Orchestrator => (
                snap.active_provider
                    .clone()
                    .or_else(|| config.harness.default_provider.clone()),
                snap.active_model
                    .clone()
                    .or_else(|| config.harness.default_model.clone()),
            ),
            // Live-Auswahl der UIA zuerst (`/uia-model switch` wirkt sofort),
            // dann der gespeicherte Stand.
            PickerTarget::Uia => (
                snap.uia_selection
                    .provider()
                    .map(str::to_owned)
                    .or_else(|| config.harness.uia_provider.clone())
                    .or_else(|| self.active_or_default_provider(&config)),
                snap.uia_selection
                    .model()
                    .map(str::to_owned)
                    .or_else(|| config.harness.uia_model.clone())
                    .or_else(|| snap.active_model.clone()),
            ),
            PickerTarget::UiaWorker { fixed_provider } => (
                Some(fixed_provider.clone()),
                config.harness.uia_worker_model.clone(),
            ),
            PickerTarget::Role { role } => {
                let row = harw_config::resolve_role_model(&config, *role);
                (row.provider, row.model)
            }
            // Runde 5, Teil G: Vorauswahl = feste Wahl der Rolle, sonst UIA.
            PickerTarget::UiaWorkerRole { role } => {
                match harw_config::resolve_uia_worker_model(&config, role).choice {
                    harw_config::UiaWorkerModelChoice::Fixed { provider, model } => {
                        (Some(provider), Some(model))
                    }
                    harw_config::UiaWorkerModelChoice::FollowUia => {
                        let row =
                            harw_config::resolve_role_model(&config, harw_config::ModelRole::Uia);
                        (row.provider, row.model)
                    }
                }
            }
        };

        let Some(picker) = ModelSwitchPicker::new(
            target,
            provider_entries,
            models_by_provider,
            active_provider.as_deref(),
            active_model.as_deref(),
        ) else {
            self.push_line(
                Role::System,
                tr_args(
                    "app_notices.no_providers",
                    "{context} unavailable: no enabled providers configured.",
                    &[("context", &context_label)],
                ),
            );
            return;
        };
        self.overlay = Some(Overlay::ModelSwitch(Box::new(picker)));
    }

    /// Öffnet die einstufige Effort-Auswahl (Welle 7b).
    ///
    /// # Beschreibung
    /// Listet alle sechs [`ReasoningEffort`]-Stufen (aufsteigend) plus einen
    /// abschließenden Eintrag „Provider-Default (zurücksetzen)" (emittiert
    /// `clear`). Die Vorauswahl unterscheidet sich je `target`:
    /// [`EffortTarget::Session`] liest den Controller-Snapshot
    /// (`reasoning_effort`), [`EffortTarget::Uia`] den persistierten
    /// `config.harness.reasoning.uia`-Wert (geparst über
    /// [`ReasoningEffort::from_str`]; ein unbekannter oder fehlender Wert
    /// fällt auf keine Vorauswahl zurück).
    ///
    /// # Argumente
    /// - `target` (`EffortTarget`): Session oder UIA.
    pub(crate) fn open_effort_choice(&mut self, target: EffortTarget) {
        crate::i18n::with_language(self.language, || self.open_effort_choice_scoped(target));
    }

    pub(super) fn open_effort_choice_scoped(&mut self, target: EffortTarget) {
        let active: Option<ReasoningEffort> = match target {
            EffortTarget::Session => self.session_controller.snapshot().reasoning_effort,
            EffortTarget::Uia => self
                .resolved_config()
                .and_then(|config| config.harness.reasoning.uia.clone())
                .and_then(|value| value.parse::<ReasoningEffort>().ok()),
        };

        let selected = active
            .and_then(|active| EFFORT_LEVELS.iter().position(|level| *level == active))
            .unwrap_or(0);

        let dialog = effort_choice_dialog(target).with_selected(selected);
        self.overlay = Some(Overlay::EffortChoice { target, dialog });
    }

    /// Gibt `true` zurück, wenn gerade ein Vollflächen-Overlay geöffnet ist.
    #[must_use]
    pub(super) fn has_overlay(&self) -> bool {
        self.overlay.is_some()
    }

    /// Öffnet die Agentenbaum-Ansicht für die aktuelle Wurzelsitzung.
    ///
    /// Der Snapshot wird bei jedem Rendern neu aus dem gemeinsamen Spawner
    /// gelesen, damit Start, Abschluss und ein Abbruch ohne separaten
    /// TUI-internen Lifecycle-Cache sichtbar werden.
    pub(super) fn open_agent_tree(&mut self) {
        self.overlay = Some(Overlay::AgentTree(AgentTree::default()));
    }

    /// Erzeugt einen topologisch sortierten, nicht-sensitiven Snapshot des
    /// eigenen Agenten-Teilbaums. Der Spawner besitzt die einzige Quelle für
    /// Admittierung und Status; unbekannte Telemetrie bleibt für die Anzeige
    /// bewusst `None` statt als Nullwert erfunden zu werden.
    pub(super) fn agent_tree_rows(&self) -> Vec<AgentRow> {
        crate::i18n::with_language(self.language, || self.agent_tree_rows_scoped())
    }

    pub(super) fn agent_tree_rows_scoped(&self) -> Vec<AgentRow> {
        let root = self.session_id().clone();
        let mut rows = vec![AgentRow {
            id: root.as_str().to_owned(),
            parent: None,
            // Runde 5, Teil I: tatsächliche Wurzelrolle (UIA bzw. expliziter
            // Wurzel-Agent) statt „Wurzel-Orchestrator“.
            role: self.agent_tree_root_label(),
            depth: 0,
            status: "running".to_owned(),
            task: None,
            tokens: None,
            tool_calls: None,
            duration_ms: None,
            budget: "—".to_owned(),
            result: None,
            can_stop: false,
            ..AgentRow::default()
        }];
        let mut seen = HashSet::from([root.as_str().to_owned()]);
        if let Some(spawner) = self.managed_spawner() {
            append_agent_tree_rows(spawner, &root, 1, &mut seen, &mut rows);
        }
        append_historic_agent_tree_rows(&self.historic_agent_events, &root, &mut seen, &mut rows);
        // Runde 5, Teil I: Live-Werte (Monitor) und Spawn-Aufträge ergänzen.
        self.enrich_agent_tree_rows(&mut rows);
        // Runde 5, Teil K: „läuft im Hintergrund" statt nur „running".
        background_agents::mark_rows(self, &mut rows);
        rows
    }
}
