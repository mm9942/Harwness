//! ChatApp methods: composer.

use super::*;

impl ChatApp {
    /// Synchronisiert den Popup-Zustand mit dem aktuellen Eingabepuffer.
    ///
    /// # Beschreibung
    /// Öffnet das `/command`-Popup wenn `input` mit `/` beginnt und das Popup
    /// noch nicht geöffnet ist. Schließt es wenn kein `/`-Präfix vorhanden ist.
    /// Aktualisiert den Filtertext (ohne führendes `/`) wenn das Popup bereits
    /// offen ist. (Spec-Abschnitt 2.8 / SLICE 5)
    pub(super) fn sync_popup(&mut self) {
        // Runde 5, Teil G: beim Blättern durch die Eingabe-History öffnet
        // ein zurückgeholter `/befehl` kein Popup — Hoch/Runter bleiben bei
        // der History, bis getippt oder editiert wird.
        if self.input.is_browsing_history() {
            self.command_popup = None;
            self.mention_popup = None;
            return;
        }
        self.sync_command_popup();
        self.sync_mention_popup();
    }

    /// Befehls-Popup in zwei Stufen.
    ///
    /// # Beschreibung
    /// Stufe 1 (`/mo`): Autocomplete für den Befehlsnamen, solange kein
    /// Leerraum getippt ist. Stufe 2 (`/kanban mo`): Folgt auf einen
    /// vollständigen, bekannten Befehl genau ein Leerzeichen-getrenntes Wort,
    /// bietet [`CommandPopup::for_subcommands`] dessen Unterkommandos an.
    /// Sobald danach weiterer Leerraum folgt (Argument-Eingabe) oder nichts
    /// passt, schließt das Popup — sonst fingen Ziffern in Argumenten (z. B.
    /// Job-IDs `job-42`) die Eingabe ab.
    pub(super) fn sync_command_popup(&mut self) {
        let Some(query) = self.input.text().strip_prefix('/') else {
            self.command_popup = None;
            return;
        };
        if !query.contains(char::is_whitespace) {
            let reuse = matches!(
                self.command_popup.as_ref().map(CommandPopup::mode),
                Some(PopupMode::CommandName)
            );
            if !reuse {
                self.command_popup = Some(CommandPopup::new(&self.command_registry));
            }
            if let Some(popup) = self.command_popup.as_mut() {
                popup.on_query_change(query);
            }
            return;
        }
        let Some((name, rest)) = query.split_once(char::is_whitespace) else {
            self.command_popup = None;
            return;
        };
        let sub_query = rest.trim_start();
        if sub_query.contains(char::is_whitespace) {
            self.command_popup = None;
            return;
        }
        let Some(spec) = self.command_registry.find(name) else {
            self.command_popup = None;
            return;
        };
        let canonical = spec.name.as_str().to_owned();
        let reuse = matches!(
            self.command_popup.as_ref().map(CommandPopup::mode),
            Some(PopupMode::Subcommand { command }) if *command == canonical
        );
        if !reuse {
            self.command_popup = CommandPopup::for_subcommands(spec);
        }
        let sub_query = sub_query.to_owned();
        if let Some(popup) = self.command_popup.as_mut() {
            popup.on_query_change(&sub_query);
            if popup.is_empty() {
                self.command_popup = None;
            }
        }
    }

    /// Öffnet, filtert oder schließt das `@`-Erwähnungs-Popup passend zum
    /// Wort unter dem Cursor (nur außerhalb des Befehls-Popups).
    pub(super) fn sync_mention_popup(&mut self) {
        if self.command_popup.is_some() {
            self.mention_popup = None;
            return;
        }
        let cursor = editor_cursor_byte(&self.input);
        let query = current_mention_query(self.input.text(), cursor).map(|(_, q)| q.to_owned());
        match query {
            Some(query) => {
                if self.mention_popup.is_none() {
                    self.mention_popup = Some(MentionPopup::new(self.mention_candidates()));
                }
                if let Some(popup) = self.mention_popup.as_mut() {
                    popup.filter(&query);
                }
            }
            None => self.mention_popup = None,
        }
    }

    /// Kandidaten für das `@`-Popup: bekannte Rollen und bis zu
    /// [`MENTION_CANDIDATE_CAP`] Dateien unter der Projektwurzel.
    pub(super) fn mention_candidates(&self) -> Vec<MentionCandidate> {
        let mut candidates: Vec<MentionCandidate> = KNOWN_ROLES
            .iter()
            .map(|role| MentionCandidate::role(*role))
            .collect();
        if !self.project_root.is_empty() {
            candidates.extend(
                scan_mention_candidates(
                    std::path::Path::new(&self.project_root),
                    MENTION_CANDIDATE_CAP,
                )
                .into_iter()
                .map(MentionCandidate::file),
            );
        }
        candidates
    }

    /// Ersetzt das `@token` unter dem Cursor durch `@insert` und schließt das
    /// Popup. Ohne folgenden Text wird ein Leerzeichen angehängt.
    pub(super) fn accept_mention(&mut self, insert: &str) {
        let text = self.input.text().to_owned();
        let cursor = editor_cursor_byte(&self.input);
        self.mention_popup = None;
        let Some((start, query)) = current_mention_query(&text, cursor) else {
            return;
        };
        let end = start + 1 + query.len();
        let (Some(before), Some(tail)) = (text.get(..start), text.get(end..)) else {
            return;
        };
        let mut head = format!("{before}@{insert}");
        if tail.is_empty() {
            head.push(' ');
        }
        // Puffer ersetzen, ohne gemerkte Pastes zu verlieren: ein Platzhalter
        // vor oder nach der Erwähnung bleibt beim Absenden auflösbar.
        let cursor = head.len();
        self.input
            .replace_text_keeping_pastes(&format!("{head}{tail}"), cursor);
    }

    /// Gibt `true` zurück wenn das `/command`-Popup aktuell geöffnet ist.
    ///
    /// # Rückgabe
    /// `true` wenn ein aktives Popup vorhanden ist, sonst `false`.
    #[must_use]
    pub(super) fn has_popup(&self) -> bool {
        self.command_popup.is_some()
    }

    /// Aktiver Freigabemodus aus der geteilten Zelle der Runtime-Montage.
    pub(super) fn current_approval(&self) -> Option<ApprovalMode> {
        self.runtime.as_ref().map(|rt| rt.approval_mode().get())
    }

    /// Berechtigungsstufe des TUI-Nutzers; ohne Runtime-Montage die
    /// niedrigste Stufe.
    pub(super) fn caller_tier(&self) -> crate::PermissionTier {
        self.runtime
            .as_ref()
            .map_or(crate::PermissionTier::Observer, |rt| {
                runtime_commands::caller_tier(rt.principal())
            })
    }

    /// Live-Provider und -Modell der Sitzung: was die Wurzelsitzung
    /// tatsächlich anspricht (inkl. eines noch nicht angewandten Wechsels),
    /// als aufgelöste Kennungen — nie ein Alias wie `default`.
    pub(super) fn live_model(&self) -> (Option<String>, Option<String>) {
        live_model::resolved_root_route(self)
    }

    /// Öffnet eine generische Ansicht und reiht ihren Initial-Abruf ein.
    pub(super) fn open_overlay_view(&mut self, view: Box<dyn OverlayView>) {
        self.overlay_generation = self.overlay_generation.wrapping_add(1);
        self.overlay = Some(Overlay::View(view));
        self.queue_overlay_refresh();
    }

    /// Reiht den `refresh_command` der offenen generischen Ansicht ein.
    pub(super) fn queue_overlay_refresh(&mut self) {
        let command = match &self.overlay {
            Some(Overlay::View(view)) => view.refresh_command(),
            _ => None,
        };
        if let Some(command) = command {
            self.queue_overlay_fetch(command);
        }
    }

    /// Reiht einen Datenabruf für die offene generische Ansicht ein
    /// (doppelte Einträge werden zusammengefasst).
    ///
    /// PL-97 D5: der Abruf der DoD-Ansicht ([`crate::dod_view::REFRESH_COMMAND`])
    /// steht für ihre drei Abschnitte und wird in drei Abrufe derselben
    /// Generation zerlegt ([`crate::dod_view::fetch_commands`]).
    pub(super) fn queue_overlay_fetch(&mut self, command: String) {
        for command in crate::dod_view::fetch_commands(command) {
            let fetch = DataFetch::Overlay {
                command,
                generation: self.overlay_generation,
            };
            if !self.pending_fetches.contains(&fetch) {
                self.pending_fetches.push(fetch);
            }
        }
    }

    /// `true`, wenn das Werkbank-Panel sichtbar ist und neu laden sollte.
    pub(super) fn workbench_needs_refresh(&self) -> bool {
        self.panels.workbench_visible && self.workbench.is_stale()
    }

    /// Schaltet das Werkbank-Panel um (wie `F5`); beim Einblenden wird es
    /// als veraltet markiert und damit neu geladen.
    pub(super) fn toggle_workbench(&mut self) {
        self.panels.workbench_visible = !self.panels.workbench_visible;
        if self.panels.workbench_visible {
            self.workbench.mark_stale();
        } else if self.panels.focus == crate::panes::PaneFocus::Workbench {
            self.panels.focus = crate::panes::PaneFocus::Chat;
            self.panels.maximized = false;
        }
    }

    /// Setzt die Composer-Zeile auf `text` (Cursor am Ende) und gibt den
    /// Fokus an den Chat.
    pub(super) fn prefill_composer(&mut self, text: &str) {
        self.input.clear();
        self.input.insert_str(text);
        self.panels.focus = crate::panes::PaneFocus::Chat;
        self.panels.maximized = false;
        self.sync_popup();
    }

    /// Tasten des fokussierten Werkbank-Panels.
    ///
    /// # Rückgabe
    /// `true`, wenn neu gezeichnet werden soll.
    pub(super) fn handle_workbench_key(&mut self, key: KeyEvent) -> bool {
        match self.workbench.handle_key(key) {
            PaneCommand::None => false,
            PaneCommand::Redraw => true,
            PaneCommand::Run(command) => {
                if command == self.workbench.refresh_command() {
                    // Neu laden ohne Chat-Ausgabe.
                    self.workbench.mark_stale();
                } else {
                    self.pending_commands.push_back(command);
                }
                true
            }
            PaneCommand::Prefill(text) => {
                self.prefill_composer(&text);
                true
            }
            PaneCommand::ReleaseFocus => {
                self.panels.focus = crate::panes::PaneFocus::Chat;
                self.panels.maximized = false;
                true
            }
        }
    }

    /// Leert die sichtbaren Verlaufszellen (`/clear`). Sitzungsverlauf,
    /// Modellkontext und Exporteinträge bleiben unverändert.
    pub(super) fn clear_transcript(&mut self) {
        self.cells.clear();
        self.clear_live_slot_cells();
        self.tool_cells.clear();
        self.open_tool_group = None;
        // PL-97 M3: ohne Zellen kein Gruppenbezug mehr.
        self.activity = activity_glue::ActivityGlue::default();
        // Nach `/clear` ist kein Plan mehr sichtbar: der nächste Stand kommt
        // wieder als Vollbild.
        self.last_shown_plan = None;
        self.ctrl_o_expand_last_armed = false;
        self.live_stream.clear();
        self.live_reasoning.clear();
        self.reasoning_feed.clear();
        self.scroll.force_follow();
    }

    /// Schaltet die Werkzeuganzeige weiter (`/verbose`).
    ///
    /// # Beschreibung
    /// B8/B11c (aus archive/tui-b11c-67adce7a): Zyklus `Activity ↔ Verbose`;
    /// `Activity` ist der Default, `Compact` bleibt nur programmatisch
    /// (ein verbleibendes `Compact` wird zu `Verbose`). Neue Werkzeugzellen erhalten
    /// die neue [`ToolVerbosity`]; bestehende Zellen werden bei `Verbose`
    /// aufgeklappt, sonst zugeklappt.
    ///
    /// # Rückgabe
    /// Die jetzt aktive [`ToolVerbosity`].
    pub(super) fn toggle_verbose(&mut self) -> ToolVerbosity {
        self.tool_verbosity = match self.tool_verbosity {
            ToolVerbosity::Compact | ToolVerbosity::Activity => ToolVerbosity::Verbose,
            ToolVerbosity::Verbose => ToolVerbosity::Activity,
        };
        let verbose = self.tool_verbosity == ToolVerbosity::Verbose;
        for handle in &self.tool_cells {
            handle.set_expanded(verbose);
        }
        self.ctrl_o_expand_last_armed = false;
        self.tool_verbosity
    }

    /// `/uia-worker-model` bare: Picker für den an den effektiven
    /// UIA-Provider gebundenen Worker.
    ///
    /// # Beschreibung
    /// Der Worker ist zwingend an den effektiven UIA-Provider gebunden
    /// (UIA-Pin, sonst der aktive/Standard-Provider) — keine eigene
    /// `uia_worker_provider`-Konzeption.
    pub(super) fn open_uia_worker_picker(&mut self) {
        crate::i18n::with_language(self.language, || self.open_uia_worker_picker_scoped());
    }

    pub(super) fn open_uia_worker_picker_scoped(&mut self) {
        match self.resolved_config() {
            Some(config) => {
                // Live-UIA-Auswahl vor dem gespeicherten Pin.
                let provider = self
                    .session_controller
                    .snapshot()
                    .uia_selection
                    .provider()
                    .map(str::to_owned)
                    .or_else(|| config.harness.uia_provider.clone())
                    .or_else(|| self.active_or_default_provider(&config));
                match provider {
                    Some(fixed_provider) => {
                        self.open_model_switch_picker(PickerTarget::UiaWorker { fixed_provider });
                    }
                    None => self.push_line(
                        Role::System,
                        tr("app_notices.worker_no_provider", "UIA worker model selection unavailable: no UIA-pinned, active or default provider known."),
                    ),
                }
            }
            None => self.push_line(
                Role::System,
                tr_args(
                    "app_notices.no_config",
                    "{context} unavailable: no configuration loaded.",
                    &[(
                        "context",
                        &PickerTarget::UiaWorker {
                            fixed_provider: String::new(),
                        }
                        .context_label(),
                    )],
                ),
            ),
        }
    }

    /// Modus-/Freigabe-Auswahl mit aktuellem Zustand (`F7`, `/mode`).
    pub(super) fn mode_picker_view(&self) -> ModePicker {
        let config = self.resolved_config();
        ModePicker::new(
            self.active_mode,
            config
                .as_deref()
                .map(|config| config.harness.mode.default.as_str()),
            self.current_approval(),
        )
    }

    /// Modelle je Rolle (`F8`, `/models`).
    ///
    /// Bei jedem Öffnen neu aus dem Live-Stand gebaut: Rollen aus der
    /// Live-Konfiguration, die Live-Zeile aus der aufgelösten Wurzel-Route
    /// (wie die Statuszeile). `Enter` auf der Live-Zeile öffnet den Picker,
    /// der die Wurzel tatsächlich wechselt — bei einer UIA-Wurzel
    /// `/uia-model`, sonst `/model`.
    pub(super) fn model_roles_view(&self) -> ModelRolesView {
        let (provider, model) = self.live_model();
        let view = match self.resolved_config() {
            Some(config) => {
                ModelRolesView::from_config(&config, provider.as_deref(), model.as_deref())
            }
            None => ModelRolesView::empty(provider.as_deref(), model.as_deref()),
        };
        view.with_resolved_live(self.live_model_command())
    }

    /// The current session's model picker must follow the mounted root role.
    pub(super) fn live_model_command(&self) -> &'static str {
        if self.runtime().is_some_and(|rt| rt.root_is_uia()) {
            "/uia-model"
        } else {
            "/model"
        }
    }
}
