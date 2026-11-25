//! ChatApp methods: composition.

use super::*;

impl ChatApp {
    /// Baut einen leeren Chat-Zustand mit der übergebenen Operation-Adapter-
    /// Pipeline.
    ///
    /// # Beschreibung
    /// Initialisiert den Nachrichtenverlauf und die Eingabe auf Leerwerte und
    /// befüllt `command_registry` mit [`CommandRegistry::built_in()`]
    /// (ausschließlich für die Popup-Autocomplete-Anzeige, SLICE 5). Das Popup
    /// ist initial geschlossen. Das Theme wird via `style::detect_theme` aus
    /// der Prozessumgebung bestimmt (SLICE 8). `adapters`, `sandbox` und
    /// `session_id` werden von der Composition-Root (`crate::runtime_root`)
    /// übernommen und bei jedem `/command`-Dispatch verwendet.
    ///
    /// # Argumente
    /// - `adapters` (`Vec<CommandAdapter>`): Alle `/`-Command-Adapter, gebaut
    ///   aus der `OperationRegistry` (`CommandAdapter::from_operation` pro Op).
    /// - `sandbox` (`SandboxSpec`): Authority-Boundary für alle
    ///   Operation-Dispatches dieser Session.
    /// - `session_id` (`SessionId`): Stabile Session-ID für
    ///   [`harw_operations::OpContext`].
    ///
    /// # Rückgabe
    /// Neuer, leerer `ChatApp`-Zustand mit der übergebenen Command-Pipeline.
    ///
    /// # Beispiele
    /// ```ignore
    /// use harw_tui::app::ChatApp;
    /// // adapters/sandbox/session_id baut real `crate::runtime_root`.
    /// let app = ChatApp::new(Vec::new(), sandbox, session_id);
    /// assert!(app.input().is_empty());
    /// ```
    #[must_use]
    pub fn new(adapters: Vec<CommandAdapter>, sandbox: SandboxSpec, session_id: SessionId) -> Self {
        Self::with_memory(adapters, sandbox, session_id, None)
    }

    /// Wie [`Self::new`], zusätzlich mit Memory-Backend.
    ///
    /// # Beschreibung
    /// Hinterlegt `memory` für die Korrektur-Erkennung im Chat-Pfad. Die
    /// Dienste der `/memory`-Kommandos kommen aus der Runtime-Montage
    /// (`Self::with_runtime`).
    #[must_use]
    pub fn with_memory(
        adapters: Vec<CommandAdapter>,
        sandbox: SandboxSpec,
        session_id: SessionId,
        memory: Option<std::sync::Arc<dyn harw_memory::Memory>>,
    ) -> Self {
        // Die persistente Historie wird beim Erzeugen in den Editor gespielt,
        // damit die Pfeiltasten sofort auch Eingaben früherer Sitzungen finden.
        let input_history = InputHistoryStore::open(INPUT_HISTORY_CAP);
        let mut input = InputEditor::new();
        for entry in input_history.load() {
            input.push_history(entry);
        }

        Self {
            cells: Vec::new(),
            input,
            deferred_input: std::collections::VecDeque::new(),
            child_infos: std::collections::HashMap::new(),
            recency: crate::recency::RecencySlots::new(),
            live_order: RefCell::new(live_order_glue::LiveOrder::new(Instant::now())),
            live_slots: RefCell::new(live_slots::LiveSlots::new(Instant::now())),
            mission: crate::mission::MissionUi::new(Instant::now()),
            status_expiry: harw_config::StatusExpiryMode::default(),
            reasoning_mode: ReasoningDisplayMode::default(),
            step_list: None,
            pending_turns: std::collections::VecDeque::new(),
            images: crate::image_attach::ImageAttachments::default(),
            busy_jobs: BusyJobs::new(),
            // Runde 5, Teil L.
            btw: btw::BtwState::new(),
            // Runde 6, Teil B.
            operator_shell: operator_shell::OperatorShellJobs::new(),
            active_cancel: None,
            cancel_requested_at: None,
            queue_kept_at: None,
            escape_armed: false,
            scroll: ChatScroll::new(),
            input_history,
            command_registry: CommandRegistry::from_command_adapters(&adapters)
                .with_local_specs(crate::command_catalog::local_command_specs()),
            command_popup: None,
            hidden_commands: Vec::new(),
            title_override: None,
            model_switch_allowlist: None,
            language: harw_config::UiLanguage::English,
            theme: style::detect_theme(),
            total_usage: TokenUsage::default(),
            agent_monitor: crate::agent_monitor::AgentMonitor::default(),
            child_cells: HashMap::new(),
            agent_rx: None,
            // Runde 5, Teil C: Kanban live.
            kanban_live: kanban_live::KanbanLiveWatch::default(),
            live_stream: String::new(),
            live_reasoning: String::new(),
            panels: crate::panes::PanelState::default(),
            agent_detail: None,
            key_bindings: KeyBindings::default(),
            explorer: None,
            adapters: adapters.into(),
            sandbox,
            session_id,
            memory,
            runtime: None,
            project_root: String::new(),
            session_controller: Arc::new(TuiSessionController::new()),
            last_history_total_lines: Cell::new(0),
            last_history_visible_rows: Cell::new(20),
            last_regions: Cell::new(scroll_routing::FrameRegions::default()),
            footer_counts_enabled: false,
            reasoning_feed: crate::mission::reasoning_feed::ReasoningFeed::new(),
            cell_aspect_permille: Cell::new(0),
            cell_aspect_override: None,
            full_redraw: Cell::new(false),
            managed_spawner: None,
            historic_agent_events: Vec::new(),
            active_mode: InteractionMode::default(),
            plan_services: None,
            goal_tracker: crate::goal_marker::GoalTracker::default(),
            dod_mark: crate::dod_view::DodMarkFeed::default(),
            dod_mark_waker: None,
            tool_verbosity: ToolVerbosity::Activity,
            tool_cells: Vec::new(),
            open_tool_group: None,
            activity: activity_glue::ActivityGlue::default(),
            last_shown_plan: None,
            // Runde 5, Teil I.
            child_stream: crate::child_stream::ChildStreamRegistry::default(),
            ctrl_o_expand_last_armed: false,
            pending_approval_dialog: None,
            pending_host_permit: None,
            pending_host_permit_dialog: None,
            sudo: crate::sudo_dialog::SudoUi::default(),
            // Runde 5, Teil F.
            plan_ui: crate::plan_dialog::PlanUi::default(),
            pending_permission_stage: None,
            mode_before_plan: None,
            overlay: None,
            export_entries: Vec::new(),
            session_title: None,
            title_job_context: None,
            session_store_root: None,
            export_started_at: Some(export_timestamp_now()),
            export_session_model: None,
            export_meta_extensions: ExportMetaExtensions::new(),
            pending_export_options: None,
            pending_export_format: ExportOutputFormat::Markdown,
            pending_quit: None,
            hard_quit_requested: false,
            jobs_ui: jobs_glue::JobsUi::default(),
            last_shell_command: None,
            pending_turn_user_cell_override: None,
            needs_terminal_reassert: false,
            workbench: WorkbenchPane::new(),
            mention_popup: None,
            pending_fetches: Vec::new(),
            pending_commands: std::collections::VecDeque::new(),
            overlay_generation: 0,
            background: background_agents::BackgroundUi::default(),
            // Runde 5, Teil O.
            esc_confirm: turn_safety::EscConfirm::default(),
            child_approvals: child_approvals::ChildApprovalUi::default(),
        }
    }

    /// Merkt sich einen Info-Zwischenstand eines Kind-Agenten für die
    /// Statuszeile.
    ///
    /// Der Zeitstempel ist `Instant::now()`; das Anzeigefenster (30 s,
    /// passend zur Drossel `PARENT_INFO_MIN_INTERVAL` in `harw-core`)
    /// prüft das Rendern über das verstrichene Alter. Ältere Einträge
    /// bleiben in der Map, werden aber nicht mehr prominent gezeigt.
    pub(crate) fn note_child_info(&mut self, child: String, info: String) {
        self.note_child_info_at(child, info, std::time::Instant::now());
    }

    /// Testbare Variante von [`Self::note_child_info`] mit vorgegebenem
    /// Zeitstempel, damit das 30s-Ausblendfenster (`PARENT_INFO_MIN_INTERVAL`
    /// in `harw-core::child_comms`) ohne Echtzeit-Warten geprüft werden kann.
    pub(crate) fn note_child_info_at(
        &mut self,
        child: String,
        info: String,
        at: std::time::Instant,
    ) {
        self.child_infos.insert(child, (info, at));
    }

    /// Reuses a runtime-owned controller for both slash commands and model
    /// tool operations in this chat session.
    #[must_use]
    pub(crate) fn with_session_controller(mut self, controller: Arc<TuiSessionController>) -> Self {
        self.session_controller = controller;
        self
    }

    /// Hinterlegt die Runtime-Montage dieses Laufs.
    ///
    /// # Beschreibung
    /// Aus der Montage bezieht `run_loop` für jede `/command`-Zeile die
    /// Berechtigungsstufe des Aufrufers
    /// ([`crate::runtime_commands::caller_tier`] über
    /// [`harw_runtime::RuntimeAssembly::principal`]) und die Slash-Dienste
    /// ([`crate::runtime_commands::slash_service_map`] über
    /// [`harw_runtime::RuntimeAssembly::services`]).
    ///
    /// # Argumente
    /// - `assembly` (`Arc<harw_runtime::RuntimeAssembly>`): geteilte Montage;
    ///   Besitz des `Arc` geht über.
    ///
    /// # Rückgabe
    /// `Self` für Builder-Verkettung.
    #[must_use]
    pub(crate) fn with_runtime(mut self, assembly: Arc<harw_runtime::RuntimeAssembly>) -> Self {
        self.runtime = Some(assembly);
        self
    }

    /// Installiert die beim Resume geladene, unveränderliche
    /// Orchestrierungshistorie für die Agentenbaum-Projektion.
    pub(crate) fn set_historic_agent_events(&mut self, events: Vec<AgentOrchestrationEvent>) {
        self.historic_agent_events = events;
    }

    /// Ersetzt die aktive Tastenbelegung (z. B. aus der Keybindings-Datei
    /// geladen); gilt ab der nächsten Taste für Panels und Composer.
    pub(crate) fn set_key_bindings(&mut self, bindings: KeyBindings) {
        self.key_bindings = bindings;
    }

    /// Rebinds persistent input history to the runtime-selected Harw home.
    ///
    /// `ChatApp::new` has a standalone fallback for tests and embedders. The
    /// composition root calls this builder before the event loop starts, so
    /// replacing the editor's initially loaded fallback history is safe and
    /// makes an explicit `--home` authoritative for both reads and writes.
    #[must_use]
    pub(crate) fn with_home(mut self, home: &std::path::Path) -> Self {
        let input_history = InputHistoryStore::at_home(home, INPUT_HISTORY_CAP);
        let mut input = InputEditor::new();
        for entry in input_history.load() {
            input.push_history(entry);
        }
        self.input_history = input_history;
        self.input = input;
        self
    }

    /// Gibt die hinterlegte Runtime-Montage zurück.
    ///
    /// # Rückgabe
    /// `Option<&Arc<harw_runtime::RuntimeAssembly>>` — `None`, solange
    /// [`Self::with_runtime`] nicht aufgerufen wurde.
    #[must_use]
    pub(crate) fn runtime(&self) -> Option<&Arc<harw_runtime::RuntimeAssembly>> {
        self.runtime.as_ref()
    }

    /// Runde 5, Teil B: hängt den sudo-Fragekanal (`host.sudo_exec`) und die
    /// Merkfrist für „Für diese Sitzung“ an.
    ///
    /// # Argumente
    /// - `receiver`: Empfänger aus `RuntimeAssembly::take_sudo_prompts`
    ///   (`None` außerhalb einer TUI-Montage — dann gibt es kein Fenster).
    /// - `session_ttl`: Merkfrist (`[host] sudo_session_minutes`);
    ///   `Duration::ZERO` bietet nur „Einmalig“ an.
    ///
    /// # Rückgabe
    /// `Self` für Builder-Verkettung.
    #[must_use]
    pub(crate) fn with_sudo_prompts(
        mut self,
        receiver: Option<harw_tool_shell::SudoPromptReceiver>,
        session_ttl: Duration,
    ) -> Self {
        self.sudo = crate::sudo_dialog::SudoUi::new(receiver, session_ttl);
        self
    }

    /// Hinterlegt die Kind-Spawn-Autorität dieser Session.
    ///
    /// # Beschreibung
    /// Der Renderer braucht denselben [`ManagedAgentSpawner`], den auch der
    /// Model-Tool-Pfad benutzt: [`ApprovalDriver::drive_to_completion`] erhält
    /// ihn als [`ChildTurnDriver`], um ein `TurnOutcome::AwaitingChild`
    /// weiterzutreiben. `None` ist zulässig — Handoffs werden dann mit einem
    /// Fehlerergebnis beantwortet und der Turn endet trotzdem regulär.
    ///
    /// # Argumente
    /// - `spawner` (`Option<Arc<ManagedAgentSpawner>>`): geteilte Autorität.
    ///
    /// # Rückgabe
    /// `Self` für Builder-Verkettung.
    #[must_use]
    pub(crate) fn with_managed_spawner(
        mut self,
        spawner: Option<Arc<ManagedAgentSpawner>>,
    ) -> Self {
        self.managed_spawner = spawner;
        self
    }

    /// Gibt die Kind-Spawn-Autorität dieser Session zurück.
    ///
    /// # Rückgabe
    /// `Option<&Arc<ManagedAgentSpawner>>` — `None`, wenn keine Kindrolle
    /// konfiguriert ist.
    #[must_use]
    pub(crate) fn managed_spawner(&self) -> Option<&Arc<ManagedAgentSpawner>> {
        self.managed_spawner.as_ref()
    }

    /// Hinterlegt die von der Composition-Root gebauten Plan-/Ziel-Dienste.
    ///
    /// # Beschreibung
    /// Ohne diese Dienste bleibt der Renderer voll funktionsfähig; er stellt
    /// Plan-Updates dann als Systemzeile statt als [`PlanGraphCell`] dar und
    /// zeigt nach `/goal check` keinen [`GoalCell`]-Zielstand.
    ///
    /// # Argumente
    /// - `services` ([`TuiPlanServices`]): geteilte Store-Handles.
    ///
    /// # Rückgabe
    /// `Self` für Builder-Verkettung.
    #[must_use]
    pub fn with_plan_services(mut self, services: TuiPlanServices) -> Self {
        self.plan_services = Some(services);
        self
    }

    /// Gibt die hinterlegten Plan-/Ziel-Dienste zurück.
    ///
    /// # Rückgabe
    /// `Option<&TuiPlanServices>` — `None`, wenn keine konfiguriert sind.
    #[must_use]
    pub(crate) fn plan_services(&self) -> Option<&TuiPlanServices> {
        self.plan_services.as_ref()
    }

    /// Setzt den Detailgrad, mit dem Werkzeugzellen gerendert werden (Plan
    /// Schritt 2 „Verbosity").
    ///
    /// # Beschreibung
    /// Öffentliche, von `--verbose` unabhängige API: der interne
    /// [`ToolVerbosity`]-Typ ist `pub(crate)` und kann deshalb nicht direkt in
    /// einer öffentlichen Signatur erscheinen. Die CLI (ein anderer Slice)
    /// reicht `--verbose` als `bool` durch; `true` entspricht
    /// `ToolVerbosity::Verbose` (zusätzlich immer ausgeklappt, rohe Argumente
    /// sichtbar), `false` dem Default `ToolVerbosity::Activity`
    /// (B8/B11c: einzeilige Aktivitätsdarstellung; `Compact` bleibt nur
    /// programmatisch).
    ///
    /// # Argumente
    /// - `verbose` (`bool`): `true` aktiviert die ausführliche Darstellung.
    ///
    /// # Rückgabe
    /// `Self` für Builder-Verkettung.
    #[must_use]
    pub fn with_verbose_tools(mut self, verbose: bool) -> Self {
        self.tool_verbosity = if verbose {
            ToolVerbosity::Verbose
        } else {
            ToolVerbosity::Activity
        };
        self
    }

    /// Schaltet die Agentenzahlen-Fußzeile unter dem Composer ein
    /// (`[tui] footer_counts`, Vorgabe: aus).
    #[must_use]
    pub fn with_footer_counts(mut self, enabled: bool) -> Self {
        self.footer_counts_enabled = enabled;
        self
    }

    /// Feste Zellproportion aus `[harness.tui] cell_aspect` (Breite/Höhe,
    /// z. B. `0.48`); `None` lässt [`Self::refresh_cell_aspect`] messen.
    #[must_use]
    pub fn with_cell_aspect(mut self, cell_aspect: Option<f32>) -> Self {
        self.cell_aspect_override = cell_aspect
            .filter(|value| value.is_finite() && *value > 0.0)
            // Nach dem Klemmen liegt der Wert sicher in `u16`.
            .map(|value| (f64::from(value) * 1000.0).round().clamp(200.0, 1500.0) as u16);
        if let Some(permille) = self.cell_aspect_override {
            self.cell_aspect_permille.set(permille);
        }
        self
    }

    /// Misst die Zellproportion über `TIOCGWINSZ` (Pixel- und Zellmaße),
    /// sofern keine Vorgabe gesetzt ist. Liefert das Terminal keine
    /// Pixelmaße (oder gar nichts, etwa unter `TestBackend`), bleibt `0`.
    pub(super) fn refresh_cell_aspect(&self) {
        if let Some(permille) = self.cell_aspect_override {
            self.cell_aspect_permille.set(permille);
            return;
        }
        let permille = crossterm::terminal::window_size()
            .ok()
            .filter(|size| size.columns > 0 && size.rows > 0 && size.width > 0 && size.height > 0)
            .map_or(0, |size| {
                let num = u64::from(size.width) * u64::from(size.rows) * 1000;
                let den = u64::from(size.height) * u64::from(size.columns);
                u16::try_from((num / den).clamp(200, 1500)).unwrap_or(0)
            });
        self.cell_aspect_permille.set(permille);
    }

    /// Hides the named commands from this session's palette, tab-completion
    /// and dispatch (R10 wave 3B: the fixed-agent mini-TUI,
    /// [`crate::fixed_agent`]).
    ///
    /// # Beschreibung
    /// Names are matched case-insensitively against each [`CommandSpec`]'s
    /// canonical name (not its aliases — see
    /// [`crate::fixed_agent::filter_command_specs`]'s own doc for why that is
    /// sufficient). Rebuilds `command_registry` immediately from its current
    /// specs with the hidden ones removed, so the `/`-popup
    /// ([`CommandPopup::new`]) and subcommand completion
    /// ([`Self::sync_command_popup`], both read `self.command_registry`)
    /// never offer a hidden command. [`local_intercept_for`] additionally
    /// consults [`Self::hidden_commands`] before any other local
    /// interception, so a hidden command is refused with a short message
    /// instead of running — even for a form (e.g. a bare `/agent`) that a
    /// local intercept would otherwise open unconditionally, and even for a
    /// form the local intercept does not know about at all (falls through to
    /// the operation-adapter dispatch, which no longer finds the command
    /// either once it is gone from `command_registry`).
    ///
    /// Calling this with an empty `hidden` list is a no-op (normal,
    /// unrestricted TUI).
    ///
    /// # Argumente
    /// - `hidden` (`Vec<String>`): canonical command names to hide, e.g.
    ///   `crate::fixed_agent::hidden_command_names(&opts)` mapped to owned
    ///   `String`s.
    ///
    /// # Rückgabe
    /// `Self` für Builder-Verkettung.
    #[must_use]
    pub fn with_hidden_commands(mut self, hidden: Vec<String>) -> Self {
        let hidden: Vec<String> = hidden
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect();
        if !hidden.is_empty() {
            let specs: Vec<crate::CommandSpec> = self
                .command_registry
                .specs()
                .iter()
                .filter(|spec| !hidden.iter().any(|name| name == spec.name.as_str()))
                .cloned()
                .collect();
            self.command_registry = CommandRegistry::new(specs);
        }
        self.hidden_commands = hidden;
        self
    }

    /// Overrides the title shown in the status line with the compiled
    /// agent's name and short digest (R10 wave 3B,
    /// [`crate::fixed_agent::fixed_agent_title`]).
    ///
    /// # Rückgabe
    /// `Self` für Builder-Verkettung.
    #[must_use]
    pub fn with_title_override(mut self, title: String) -> Self {
        self.title_override = Some(title);
        self
    }

    /// Restricts `/model switch` targets to this allowlist (R10 wave 3B).
    ///
    /// # Beschreibung
    /// `Some(allowlist)` makes [`local_intercept_for`] refuse any
    /// `/model switch <target>` whose target is not one of the `(provider,
    /// model)` pairs in `allowlist` (matched the same way as
    /// [`crate::fixed_agent::is_model_switch_target_allowed`]: bare model id
    /// or qualified `provider/model`, case-insensitive) with a short
    /// message, before the operation dispatch ever sees it. `None` (the
    /// default) leaves `/model switch` unrestricted.
    ///
    /// # Rückgabe
    /// `Self` für Builder-Verkettung.
    #[must_use]
    pub fn with_model_switch_allowlist(mut self, allowlist: Option<Vec<(String, String)>>) -> Self {
        self.model_switch_allowlist = allowlist;
        self
    }

    /// Hinterlegt den Kontext für die einmalige Titel-Job-Anstoßung nach dem
    /// ersten abgeschlossenen Turn (Plan Schritt 7).
    ///
    /// # Beschreibung
    /// Wird von der Composition-Root (`crate::runtime_root::build_root_runtime`)
    /// gesetzt, wenn Titel-Generierung konfiguriert und eine
    /// Session-Store-Wurzel bekannt ist. `run_loop` löst den Job beim ersten
    /// `SessionEvent::TurnCompleted` ein und verwirft den Kontext danach
    /// (`Option::take`) — er wird also höchstens einmal je Sitzung verwendet.
    ///
    /// # Rückgabe
    /// `Self` für Builder-Verkettung.
    #[must_use]
    pub(crate) fn with_title_job_context(mut self, ctx: TitleJobContext) -> Self {
        self.title_job_context = Some(ctx);
        self
    }

    /// Hinterlegt die Wurzel des Session-Stores für jede Sitzung, bei der sie
    /// bekannt ist (Export-Datum bei Resume für jede Session).
    ///
    /// # Beschreibung
    /// Wird von der Composition-Root (`crate::runtime_root::build_root_runtime`)
    /// **immer** gesetzt, wenn `/resume` in diesem Lauf konfiguriert ist —
    /// unabhängig davon, ob `[session] title_generation` aktiv ist. Anders
    /// als [`Self::with_title_job_context`] (nur bei aktiver Titelerzeugung)
    /// ist dies der zuverlässige Weg, mit dem
    /// [`apply_session_store_started_at`] für jede fortgesetzte Sitzung das
    /// tatsächliche Sitzungsstart-Datum statt des TUI-Startzeitpunkts
    /// auflösen kann.
    ///
    /// # Rückgabe
    /// `Self` für Builder-Verkettung.
    #[must_use]
    pub(crate) fn with_session_store_root(mut self, root: std::path::PathBuf) -> Self {
        self.session_store_root = Some(root);
        self
    }
}
