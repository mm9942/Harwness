//! Keyboard and overlay routing.

use super::*;

/// Verarbeitet einen Tastendruck, während ein Vollflächen-Overlay
/// (Session-Picker, `/export`-, Modell/Provider-, Effort-Auswahl) den
/// normalen Eingabepfad ersetzt.
///
/// # Beschreibung
/// - [`Overlay::SessionPicker`]: delegiert an [`SessionPicker::handle_key`].
///   `PickerAction::Open(id)` (`session_picker`) schließt das Overlay und
///   synthetisiert eine `/resume <id>`-Befehlszeile über den bestehenden
///   [`HarwEvent::Command`]-Pfad (`resume_request` in `run_loop` erkennt sie
///   und beendet den Loop mit `TuiRunOutcome::Resume { selector: Some(id) }`,
///   genau wie bei einer getippten Zeile) — kein neuer Ereignistyp nötig.
/// - [`Overlay::ExportChoice`]: delegiert an [`ChoiceDialog::handle_key`].
///   Eine getroffene Wahl schließt das Overlay und wird über
///   [`resolve_export_choice`] eingelöst.
/// - [`Overlay::ModelSwitch`]: delegiert an [`ModelSwitchPicker::on_key`].
///   `ModelSwitchAction::Accept { provider, model }` schließt das Overlay und
///   synthetisiert je nach [`ModelSwitchPicker::target`] `/model switch
///   <model>`, `/uia-model switch <model>` bzw. `/uia-worker-model switch
///   <model>` über [`HarwEvent::Command`] (`provider` bleibt implizit — der
///   Ziel-Op wechselt Provider+Modell atomar).
/// - [`Overlay::EffortChoice`]: delegiert an [`ChoiceDialog::handle_key`].
///   Eine getroffene Wahl schließt das Overlay und synthetisiert je nach
///   `target` `/effort <level>`/`/effort clear` bzw. `/uia-effort
///   <level>`/`/uia-effort clear` (letzter Eintrag „Provider-Default
///   (zurücksetzen)" → `clear`).
///
/// # Rückgabe
/// `true` (jede Taste verändert entweder den Overlay-Zustand oder schließt
/// ihn — in beiden Fällen ist ein Redraw nötig).
pub(super) fn handle_overlay_key(app: &mut ChatApp, key: KeyEvent, bus: &HarwEventSender) -> bool {
    match app.overlay.as_mut() {
        // Der Arm bindet nichts aus dem Overlay-Inhalt, daher endet der
        // veränderliche Borrow von `app.overlay` schon vor diesem Aufruf und
        // `app` darf hier erneut voll geliehen werden (NLL) — kein separater
        // Vorab-Check mehr nötig, der Fall ist jetzt Teil dieses `match`.
        Some(Overlay::AgentTree(_)) => {
            handle_agent_tree_key(app, key);
        }
        Some(Overlay::SessionPicker(picker)) => match picker.handle_key(key) {
            PickerAction::Stay => {}
            PickerAction::Cancel => app.overlay = None,
            PickerAction::Open(id) => {
                app.overlay = None;
                bus.send(HarwEvent::Command(format!("/resume {id}")));
            }
        },
        Some(Overlay::ExportChoice(dialog)) => match dialog.handle_key(key) {
            ChoiceAction::Stay => {}
            ChoiceAction::Cancel => {
                app.overlay = None;
                app.pending_export_options = None;
            }
            ChoiceAction::Chosen(index) => {
                app.overlay = None;
                resolve_export_choice(app, bus, index);
            }
        },
        Some(Overlay::ModelSwitch(picker)) => match picker.on_key(key) {
            ModelSwitchAction::Stay => {}
            ModelSwitchAction::Cancel => app.overlay = None,
            ModelSwitchAction::Accept { provider, model } => {
                let command = picker.target().command_line(&provider, &model);
                // Runde 5, Teil G: Worker-Wahl live übernehmen, nach der
                // UIA-Wahl aus `/models` den Worker-Bereich öffnen.
                let reopen_workers = picker.opens_uia_workers_after();
                app.overlay = None;
                uia_workers::after_model_switch_accept(app, &command, reopen_workers);
                bus.send(HarwEvent::Command(command));
            }
        },
        Some(Overlay::View(view)) => {
            let outcome = view.on_key(key);
            apply_overlay_outcome(app, outcome, bus);
        }
        // Plan R9, Teil F: Beenden mit laufenden Jobs.
        Some(Overlay::JobsQuitChoice(dialog)) => match jobs_glue::handle_quit_choice(dialog, key) {
            None => {}
            Some(None) => app.overlay = None,
            Some(Some(choice)) => {
                app.overlay = None;
                app.jobs_ui.quit_choice = Some(choice);
                bus.send(HarwEvent::Quit);
            }
        },
        // PL-97 C4 (W20-E): Enter schickt die wörtliche Zeile `/jobs stop
        // <id>` über denselben Weg wie eine getippte Zeile (Zulassung,
        // Busy-Klasse, `OpContext`); keine Job-API von hier, kein zweiter Weg.
        Some(Overlay::StopJob(stop)) => match stop.handle_key(key) {
            crate::mission::stop_job::StopDecision::Stay => {}
            crate::mission::stop_job::StopDecision::Cancel => app.overlay = None,
            crate::mission::stop_job::StopDecision::Confirm { line, target } => {
                app.overlay = None;
                tracing::info!(
                    job_id = %target.job,
                    line = line.as_str(),
                    "tui.jobs.stop_confirmed"
                );
                jobs_glue::note_stop_requested(app, &target);
                bus.send(HarwEvent::Command(line.into_line()));
            }
        },
        Some(Overlay::EffortChoice { target, dialog }) => {
            let target = *target;
            match dialog.handle_key(key) {
                ChoiceAction::Stay => {}
                ChoiceAction::Cancel => app.overlay = None,
                ChoiceAction::Chosen(index) => {
                    app.overlay = None;
                    let level = EFFORT_LEVELS
                        .get(index)
                        .map(std::string::ToString::to_string);
                    let level = level.unwrap_or_else(|| "clear".to_owned());
                    let command = match target {
                        EffortTarget::Session => format!("/effort {level}"),
                        EffortTarget::Uia => format!("/uia-effort {level}"),
                    };
                    bus.send(HarwEvent::Command(command));
                }
            }
        }
        None => {}
    }
    true
}

/// Handles the tree's local navigation and delegates a selected stop request
/// to the shared controller. It is used for both idle and busy input, so the
/// tree remains operable while a parent turn is running.
pub(super) fn handle_agent_tree_key(app: &mut ChatApp, key: KeyEvent) {
    let rows = app.agent_tree_rows();
    let action = match app.overlay.as_mut() {
        Some(Overlay::AgentTree(tree)) => tree.handle_key(key, &rows),
        _ => AgentTreeAction::Stay,
    };
    match action {
        AgentTreeAction::Stay => {}
        AgentTreeAction::Close => app.overlay = None,
        AgentTreeAction::Stop(target) => {
            let child = SessionId::from_str(target.clone());
            let cancelled = app.managed_spawner().is_some_and(|spawner| {
                spawner.owns_descendant(app.session_id(), &child)
                    && spawner.request_cancellation(&child)
            });
            if !cancelled {
                app.push_line(
                    Role::System,
                    crate::i18n::tr_args_for(
                        app.language,
                        "agents.cancel_failed",
                        "Could not request cancellation for agent {target}.",
                        &[("target", &target)],
                    ),
                );
            }
        }
    }
}

/// Verarbeitet einen Tastendruck: mutiert den Eingabezustand und emittiert
/// [`HarwEvent`]s. Gibt `true` zurück, wenn ein Redraw nötig ist.
///
/// # Tastensteuerung (an codex/codex-rs orientiert)
/// - **Enter** — Zeile absenden (leer/whitespace: ignoriert).
/// - **Ctrl+J** (auch Shift/Alt+Enter) — neue Zeile im Eingabepuffer.
/// - **Ctrl+C** — 1×: „again to quit"-Hinweis; 2× innerhalb 2 s: [`HarwEvent::Quit`].
/// - **Ctrl+D** — 1×: Hinweis, 2× innerhalb 2 s: Quit, unabhängig von Composer, Popup oder Overlay.
/// - **Esc** — aktuelle Eingabe leeren.
/// - **Backspace** — letztes Zeichen löschen.
/// - **PageUp** — History nach oben scrollen (via [`ChatScroll::page_up`]).
/// - **PageDown** — History nach unten scrollen (via [`ChatScroll::page_down`]).
/// - **Strg+Pos1**/**Strg+Ende** — springt bei leerem Composer im Transkript
///   an Anfang/Ende ([`ChatScroll`]), sonst im Composer an Puffer-Anfang/-Ende
///   (siehe [`scroll_claims_key`] und `InputEditor::move_buffer_start`/
///   `move_buffer_end`).
/// - Hochgescrollt (Hinweis „↓ N neue Zeilen · Strg+Ende springt ans Ende"):
///   **Strg+Ende** springt immer ans Transkript-Ende, **Ende** bei leerem
///   Composer ebenfalls; danach folgt die Ansicht wieder neuem Inhalt.
/// - Bei offenem Popup: Pfeiltasten/Enter/Esc/Ziffern navigieren das Popup.
/// - Alle übrigen Composer-Tasten (Pos1/Ende, Strg+Links/Rechts,
///   Strg+Backspace/Strg+W, Strg+A/Strg+E …) siehe
///   `InputEditor::handle_key_at`-Doku; ein bar Esc kann dort zusätzlich der
///   Anfang einer zerstückelten CSI-Sequenz sein (dtach/zsh-Härtung).
///
/// # Argumente
/// - `app` (`&mut ChatApp`): Zustand, der mutiert wird — inklusive
///   [`ChatApp::pending_quit`] (Welle 4c: vormals ein separater Parameter,
///   jetzt ein Feld, damit auch [`handle_busy_event`] dieselbe Scharfstellung
///   lesen/setzen kann).
/// - `key` (`KeyEvent`): der bereits auf Press/Repeat gefilterte Tastendruck.
/// - `bus` (`&HarwEventSender`): Kanal, über den [`HarwEvent`]s emittiert werden.
///
/// # Rückgabe
/// `true` wenn ein Redraw angefordert werden soll, sonst `false`.
pub(super) fn handle_key(app: &mut ChatApp, key: KeyEvent, bus: &HarwEventSender) -> bool {
    crate::i18n::with_language(app.language, || handle_key_scoped(app, key, bus))
}

pub(super) fn handle_key_scoped(app: &mut ChatApp, key: KeyEvent, bus: &HarwEventSender) -> bool {
    // Runde 5, Teil L: Esc bricht eine wartende `/btw`-Nebenfrage ab.
    if app.btw_esc_cancels(&key) {
        return true;
    }
    if let Some(redraw) = handle_view_hotkey(app, key) {
        return redraw;
    }
    if let Some(redraw) = handle_panel_key(app, key) {
        return redraw;
    }
    scroll_and_composer_key(app, key, bus)
}

/// Panel-Tasten (Standard F2/F3/F4/F11/Ctrl+E laut [`ChatApp::key_bindings`],
/// Esc und Navigation im fokussierten Panel).
/// `None`: Taste gehört dem Chat/Composer.
pub(super) fn handle_panel_key(app: &mut ChatApp, key: KeyEvent) -> Option<bool> {
    // Ein offenes Popup/Dialog behält Esc & Pfeile für sich. PL-97 C4: auch
    // ein Overlay, das ein fokussiertes Fenster öffnete (Stopp-Dialog der
    // Jobs, Einzelläufe von Finished/Failed) — wie im Busy-Pfad gehen seine
    // Tasten vor die Panel-Tasten.
    if app.command_popup.as_ref().is_some_and(|p| !p.is_empty())
        || app.mention_popup.as_ref().is_some_and(|p| !p.is_empty())
        || app.pending_approval_dialog.is_some()
        || app.pending_host_permit_dialog.is_some()
        || app.has_overlay()
    {
        return None;
    }
    sync_mission_focus(app);
    if composer_owns_mission_key(app, &key) {
        return None;
    }
    // Die Agenten-Detailansicht bekommt ihre Tasten (insbesondere Esc) vor
    // der Panel-Logik, die Esc sonst als „Fokus zurück an den Chat" deutet.
    if app.agent_detail.is_some()
        && matches!(
            app.panels.focus,
            crate::panes::PaneFocus::Agents | crate::panes::PaneFocus::Jobs
        )
        && let Some(redraw) = handle_agent_detail_key(app, key)
    {
        return Some(redraw);
    }
    // PL-97 C2: `Alt+L` öffnet/schließt die Details des LIVE-Agenten.
    if app.key_bindings.action_for(&key) == Some(KeyAction::ToggleLiveDetail) {
        return Some(app.toggle_live_detail());
    }
    match app.panels.handle_key(key, &app.key_bindings) {
        crate::panes::PanelKey::Ignored => None,
        crate::panes::PanelKey::Changed => {
            app.sync_agent_detail();
            app.ensure_explorer();
            // Ohne eigenes Jobs-Fenster wählt `Alt+J` den ersten Job der
            // kombinierten Liste (`PanelState` landet nur dann im
            // Agenten-Panel, wenn auch nach dem Einblenden kein Jobs-Fenster
            // existiert).
            if app.key_bindings.action_for(&key) == Some(KeyAction::FocusJobs)
                && app.panels.focus == crate::panes::PaneFocus::Agents
                && !app.panels.revealed.jobs
            {
                crate::mission::agents_window::select_first_job(&mut app.agent_monitor);
            }
            // Frisch eingeblendete Werkbank lädt neu (siehe
            // `process_pending_fetches`).
            if app.panels.workbench_visible
                && app.key_bindings.action_for(&key) == Some(KeyAction::ToggleWorkbench)
            {
                app.workbench.mark_stale();
            }
            Some(true)
        }
        // Übrige Tasten verschluckt die Detailansicht (kein Rückfall auf
        // die Listennavigation).
        crate::panes::PanelKey::ForFocused(_) if app.agent_detail.is_some() => Some(false),
        crate::panes::PanelKey::ForFocused(key) => Some(match app.panels.focus {
            crate::panes::PaneFocus::Agents => handle_agents_list_key(app, key),
            crate::panes::PaneFocus::Jobs => handle_jobs_window_key(app, key),
            crate::panes::PaneFocus::Finished => handle_finished_window_key(app, key),
            // PL-97 C3: LIVE scrollt; Enter zeigt/schließt die Details — in
            // der Querformat-Mittelspalte an Ort und Stelle, im Hochformat
            // (Kasten „Reasoning“) maximiert wie `Alt+L`.
            crate::panes::PaneFocus::Live
                if key.code == KeyCode::Enter && key.modifiers.is_empty() =>
            {
                app.toggle_live_detail()
            }
            crate::panes::PaneFocus::Live => {
                crate::mission::live_window::handle_key(&mut app.mission, key)
            }
            crate::panes::PaneFocus::Explorer => handle_explorer_key(app, key),
            crate::panes::PaneFocus::Workbench => app.handle_workbench_key(key),
            crate::panes::PaneFocus::Chat => false,
        }),
    }
}

/// PL-97 C2: übernimmt, welche Mission-Fenster das letzte Bild zeigte (und
/// welche das Hochformat nach dem Einblenden zeigt), als Fokusziele (`F4`,
/// `Alt+J`, `Alt+F`). Verschwindet ein fokussiertes Fenster
/// (Größenänderung), geht der Fokus dorthin, wo sein Inhalt jetzt steht:
/// Jobs und Finished/Failed in das Agenten-Panel, ohne sichtbare
/// Kartenzeile (kurzer Körper) wie LIVE an den Chat. Eine offene
/// Detailansicht behält ihren Fokus; fehlt die Kartenzeile, in der sie
/// steht, wird sie maximiert statt unsichtbar zu bleiben.
pub(super) fn sync_mission_focus(app: &mut ChatApp) {
    use crate::panes::PaneFocus;
    let regions = app.last_regions.get();
    let presence = regions.presence;
    app.panels.mission = presence;
    app.panels.revealed = regions.revealed;
    if app.agent_detail.is_some() || app.panels.maximized {
        if presence.agents_hidden && app.agent_detail.is_some() {
            app.panels.maximized = true;
        }
        return;
    }
    let fallback = if presence.agents_hidden {
        PaneFocus::Chat
    } else {
        PaneFocus::Agents
    };
    app.panels.focus = match app.panels.focus {
        PaneFocus::Jobs if !presence.jobs => fallback,
        PaneFocus::Finished if !presence.finished => fallback,
        PaneFocus::Agents if presence.agents_hidden => PaneFocus::Chat,
        PaneFocus::Live if !presence.live => PaneFocus::Chat,
        focus => focus,
    };
    // PL-97 C3: verliert LIVE so den Fokus (das Querformat-Fenster ist
    // verschwunden), schließen auch seine Details an Ort und Stelle — sonst
    // bliebe ein unsichtbarer Zustand, den das nächste `Alt+L` nur schließt.
    app.mission.sync_live_focus(app.panels.focus);
}

/// PL-97 C2: beim Tippen gewinnt der Composer. Die Voreinstellungen der
/// Mission-Aktionen (`Alt+J`, `Alt+F`, `Alt+L`) dürfen dessen
/// Emacs-Wortsprünge (`Alt+B`/`Alt+F`) nicht schlucken, solange der Chat
/// fokussiert ist und der Composer Text enthält; mit leerem Composer (oder
/// umbelegt) gelten sie wie jede Panel-Taste.
pub(super) fn composer_owns_mission_key(app: &ChatApp, key: &KeyEvent) -> bool {
    let mission_action = matches!(
        app.key_bindings.action_for(key),
        Some(KeyAction::FocusJobs | KeyAction::FocusFinished | KeyAction::ToggleLiveDetail)
    );
    let word_motion = key.modifiers.contains(KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('b' | 'B' | 'f' | 'F'));
    mission_action
        && word_motion
        && app.panels.focus == crate::panes::PaneFocus::Chat
        && !app.input.is_empty()
}

/// PL-97 C2: Tasten des fokussierten Jobs-Fensters (fest, nicht
/// umbelegbar): `↑`/`↓`, `k`/`j` wählen, `Bild↑`/`Bild↓` blättern,
/// `Pos1`/`Ende` springen, `Enter` öffnet die Job-Details. Die Auswahl gehört
/// dem Fenster ([`crate::mission::MissionUi::jobs`]), nicht der Agentenliste.
/// PL-97 C4 (W20-E): `x` fragt, ob der gewählte laufende Job gestoppt
/// werden soll (`k` bleibt „hoch“); bestätigt läuft `/jobs stop <id>` über
/// den Befehlsweg, nie direkt über die Job-Verwaltung.
pub(super) fn handle_jobs_window_key(app: &mut ChatApp, key: KeyEvent) -> bool {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return false;
    }
    if key.code == KeyCode::Enter {
        return app.open_job_window_detail();
    }
    if key.code == KeyCode::Char('x') {
        let dialog =
            crate::mission::jobs_window::selected_row(app.agent_monitor.jobs(), &app.mission.jobs)
                .and_then(crate::mission::stop_job::StopJobDialog::for_row);
        return match dialog {
            Some(dialog) => {
                app.overlay = Some(Overlay::StopJob(dialog));
                true
            }
            None => false,
        };
    }
    // Die Auswahl hängt an der Job-Kennung (bleibt beim Umsortieren).
    let keys = crate::mission::jobs_window::row_keys(app.agent_monitor.jobs());
    let ui = &mut app.mission.jobs;
    match key.code {
        KeyCode::Down | KeyCode::Char('j') => ui.select_step(true, &keys),
        KeyCode::Up | KeyCode::Char('k') => ui.select_step(false, &keys),
        KeyCode::PageDown => ui.select_page(true, &keys),
        KeyCode::PageUp => ui.select_page(false, &keys),
        KeyCode::Home => ui.select_edge(false, &keys),
        KeyCode::End => ui.select_edge(true, &keys),
        _ => return false,
    }
    true
}

/// PL-97 C2/C4: Tasten des fokussierten Finished/Failed-Fensters (fest):
/// `f` klappt die Kategorien auf/zu, `Enter` klappt auf; aufgeklappt
/// quittiert `Enter` die gewählte Kategorie und öffnet ihre Einzelläufe
/// (neueste zuerst), `a` quittiert die gewählte Kategorie (eingeklappt:
/// alle), `c` alle; gewählt wird mit `↑`/`↓`/`k`/`j`, `Bild↑`/`Bild↓`,
/// `Pos1`/`Ende`. Quittieren löscht nichts.
pub(super) fn handle_finished_window_key(app: &mut ChatApp, key: KeyEvent) -> bool {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return false;
    }
    let expanded = app.mission.finished_expanded;
    // PL-97 C3: das gerahmte Fenster des dreispaltigen Querformats zeigt
    // seine Kategorien immer; die Tasten bedienen dieselbe sichtbare Liste
    // wie das Mausrad (`scroll_routing::finished_scrolls`).
    let listed = expanded || scroll_routing::finished_scrolls(app.last_regions.get());
    match key.code {
        KeyCode::Char('f') => {
            app.mission.finished_expanded = !expanded;
            return true;
        }
        KeyCode::Enter if !listed => {
            app.mission.finished_expanded = true;
            return true;
        }
        KeyCode::Char('c') => return app.agent_monitor.acknowledge_failures(),
        KeyCode::Char('a') if !listed => return app.agent_monitor.acknowledge_failures(),
        _ => {}
    }
    if !listed {
        return false;
    }
    let rows = crate::mission::projection::terminal_aggregate(
        &app.agent_monitor,
        app.agent_monitor.jobs(),
    );
    if matches!(key.code, KeyCode::Enter | KeyCode::Char('a')) {
        let Some(row) = crate::mission::finished_window::selected_row(&rows, &app.mission.finished)
        else {
            return false;
        };
        let category = row.category.clone();
        let acknowledged =
            crate::mission::projection::acknowledge(&mut app.agent_monitor, &category);
        if key.code == KeyCode::Char('a') {
            return acknowledged;
        }
        // Die Einzelläufe aus dem frisch quittierten Stand: die eben
        // quittierten Fehlschläge tragen kein `NEW` mehr.
        let fresh = crate::mission::projection::terminal_aggregate(
            &app.agent_monitor,
            app.agent_monitor.jobs(),
        );
        let Some(row) = fresh.iter().find(|row| row.category == category) else {
            return acknowledged;
        };
        let view = crate::mission::finished_window::ConstituentsView::new(row);
        app.open_overlay_view(Box::new(view));
        return true;
    }
    // Die Auswahl hängt an der Kategorie (bleibt beim Umsortieren).
    let keys = crate::mission::finished_window::row_keys(&rows);
    let ui = &mut app.mission.finished;
    match key.code {
        KeyCode::Down | KeyCode::Char('j') => ui.select_step(true, &keys),
        KeyCode::Up | KeyCode::Char('k') => ui.select_step(false, &keys),
        KeyCode::PageDown => ui.select_page(true, &keys),
        KeyCode::PageUp => ui.select_page(false, &keys),
        KeyCode::Home => ui.select_edge(false, &keys),
        KeyCode::End => ui.select_edge(true, &keys),
        _ => return false,
    }
    true
}

/// Tasten der Agenten-Liste bei fokussiertem Panel (fest, nicht umbelegbar).
///
/// # Beschreibung
/// `↑`/`↓`, `k`/`j` und — spieletypisch — `w`/`s` wählen; `Bild↑`/`Bild↓`
/// blättern seitenweise, `Pos1`/`Ende` springen an Anfang/Ende; `Enter`
/// öffnet die Detailansicht bzw. klappt die Sammelzeile der fertigen
/// Agenten auf; `f` klappt sie auf/zu; `c` quittiert Fehlschläge; `p` heftet
/// den gewählten Agenten an das LIVE-Fenster. `w`/`s` gelten nur hier — im
/// Composer bleiben es Buchstaben (der Chat-Fokus erreicht diese Funktion
/// nie).
///
/// # Rückgabe
/// `true`, wenn neu gezeichnet werden soll.
pub(super) fn handle_agents_list_key(app: &mut ChatApp, key: KeyEvent) -> bool {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return false;
    }
    // PL-97 C2: mit eigenem Jobs-Fenster bleibt die Auswahl der
    // Agentenliste auf Agenten (die Jobs wählt das Jobs-Fenster).
    let jobs_window = app.last_regions.get().jobs.is_some();
    let settle = |app: &mut ChatApp, forward: bool| {
        if jobs_window {
            crate::mission::agents_window::settle_on_agent_entry(&mut app.agent_monitor, forward);
        }
    };
    match key.code {
        KeyCode::Down | KeyCode::Char('j' | 's') => {
            app.agent_monitor.select_next();
            settle(app, true);
            true
        }
        KeyCode::Up | KeyCode::Char('k' | 'w') => {
            app.agent_monitor.select_prev();
            settle(app, false);
            true
        }
        KeyCode::PageDown => {
            app.agent_monitor.select_page(true);
            settle(app, false);
            true
        }
        KeyCode::PageUp => {
            app.agent_monitor.select_page(false);
            settle(app, false);
            true
        }
        KeyCode::Home => {
            app.agent_monitor.select_edge(false);
            true
        }
        KeyCode::End => {
            app.agent_monitor.select_edge(true);
            settle(app, false);
            true
        }
        // PL-97 C2: `p` heftet den gewählten Agenten an das LIVE-Fenster
        // (erneut `p`: lösen).
        KeyCode::Char('p') => match app.agent_monitor.selected_agent() {
            Some(agent) => {
                app.mission.toggle_pin(agent.as_str());
                true
            }
            None => false,
        },
        KeyCode::Char('f') => {
            app.agent_monitor.toggle_finished();
            true
        }
        KeyCode::Char('c') => app.agent_monitor.acknowledge_failures(),
        KeyCode::Enter if app.agent_monitor.selected_is_summary() => {
            app.agent_monitor.toggle_finished();
            true
        }
        KeyCode::Enter => app.open_agent_detail(),
        _ => false,
    }
}

/// Tasten der Agenten-Detailansicht.
///
/// # Beschreibung
/// Esc/q zurück zur Liste, j/↓ Richtung neuester Einträge, k/↑ ältere,
/// PageUp/PageDown um [`AGENT_DETAIL_PAGE`] Zeilen, Home/g an den Anfang,
/// End/G ans Ende (folgt dem Neuesten), `r` schaltet Reasoning um.
///
/// # Rückgabe
/// `Some(redraw)`, wenn die Taste verarbeitet wurde; `None` für Tasten, die
/// an die Panel-Logik weitergehen (z. B. F2/F3/F4/F11).
pub(super) fn handle_agent_detail_key(app: &mut ChatApp, key: KeyEvent) -> Option<bool> {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }
    if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
        app.close_agent_detail();
        return Some(true);
    }
    let detail = app.agent_detail.as_mut()?;
    match key.code {
        KeyCode::Down | KeyCode::Char('j') => detail.scroll.scroll_down(1),
        KeyCode::Up | KeyCode::Char('k') => detail.scroll.scroll_up_measured(1),
        KeyCode::PageDown => detail.scroll.scroll_down(AGENT_DETAIL_PAGE),
        KeyCode::PageUp => detail.scroll.scroll_up_measured(AGENT_DETAIL_PAGE),
        KeyCode::Home | KeyCode::Char('g') => detail.scroll.jump_to_top_measured(),
        KeyCode::End | KeyCode::Char('G') => detail.scroll.jump_to_bottom(),
        KeyCode::Char('r') => detail.show_reasoning = !detail.show_reasoning,
        _ => return None,
    }
    Some(true)
}

/// Entscheidet, ob eine Taste zuerst dem Transkript-Scroll ([`ChatScroll`])
/// angeboten werden soll, bevor sie den Composer erreicht (Register
/// „CSI-Sicherheitsnetz und Paste-Platzhalter", Punkt 5).
///
/// # Beschreibung
/// Strg+Pos1/Strg+Ende sind doppelt belegt: [`ChatScroll`] springt damit an
/// Transkript-Anfang/-Ende, [`InputEditor`] an Composer-Anfang/-Ende
/// ([`InputEditor::move_buffer_start`]/[`InputEditor::move_buffer_end`]).
/// Bei nicht-leerer Eingabe gewinnt der Composer — ein Strg+Pos1 während des
/// Tippens soll den Cursor bewegen, nicht wortlos das Transkript
/// verschieben; bei leerem Composer bleibt das bisherige Verhalten
/// (Transkript-Sprung) unverändert. Alle anderen von
/// [`ChatScroll::handle_key`] behandelten Tasten (PageUp/PageDown,
/// Shift+Up/Down) sind davon nicht betroffen.
///
/// Folgen nur am Ende: solange hochgescrollt ist, springt Strg+Ende immer
/// ans Transkript-Ende (der Hinweis „… Strg+Ende springt ans Ende" muss
/// stimmen), und ein bares Ende tut das bei leerem Composer ebenfalls. Bei
/// nicht-leerer Eingabe gehören Pos1/Ende ohne Modifier dem Composer.
pub(super) fn scroll_claims_key(app: &ChatApp, key: &KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::End if ctrl && !app.scroll.is_following() => true,
        KeyCode::Home | KeyCode::End if ctrl || key.modifiers.is_empty() => app.input.is_empty(),
        _ => true,
    }
}

pub(super) fn scroll_and_composer_key(app: &mut ChatApp, key: KeyEvent, bus: &HarwEventSender) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // ── Globale Steuer-Keys (unabhängig von Popup/Editor-Zustand) ────────

    // Ctrl+C — Doppeldruck beendet (unabhängig vom Eingabeinhalt).
    if ctrl && matches!(key.code, KeyCode::Char('c' | 'C')) {
        // Runde 6, Teil B: Laufende `!`-Befehle bricht Ctrl+C zuerst ab —
        // ohne Beenden-Scharfstellung.
        if operator_shell::cancel_on_ctrl_c(app) {
            app.pending_quit = None;
            return true;
        }
        if matches!(
            app.pending_quit,
            Some(QuitArm {
                label: "Ctrl+C",
                ..
            })
        ) {
            bus.send(HarwEvent::Quit);
            return false;
        }
        app.pending_quit = Some(QuitArm {
            label: "Ctrl+C",
            at: Instant::now(),
        });
        return true;
    }

    // Ctrl+D — Doppeldruck beendet unabhängig vom aktuellen UI-Zustand.
    // Der Check liegt vor Overlay, Popup und Composer, damit ein verlässlicher
    // Notausstieg auch bei offenem Dialog oder nicht leerer Eingabe funktioniert.
    if ctrl && matches!(key.code, KeyCode::Char('d' | 'D')) {
        if matches!(
            app.pending_quit,
            Some(QuitArm {
                label: "Ctrl+D",
                ..
            })
        ) {
            bus.send(HarwEvent::Quit);
            return false;
        }
        app.pending_quit = Some(QuitArm {
            label: "Ctrl+D",
            at: Instant::now(),
        });
        return true;
    }

    // Jede andere Taste macht eine Scharfstellung rückgängig.
    app.pending_quit = None;

    // Konfigurierbare Aktionen (Standardbelegung siehe `keybindings.rs`).
    let action = app.key_bindings.action_for(&key);
    if !matches!(key.code, KeyCode::Esc) {
        app.escape_armed = false;
    }

    // ── Vollflächige Overlays (Session-Picker, `/export`-, Modell/Provider-,
    // Effort-Auswahl) ─────────────────────────────────────────────────────
    // Exklusiv: solange eines offen ist, geht keine Taste an Popup, Editor
    // oder ChatScroll (Plan Schritt 6/7).
    if app.has_overlay() {
        return handle_overlay_key(app, key, bus);
    }

    // Runde 5, Teil G: `@`- und `/command`-Popup (Hoch/Runter/Tab/Esc/
    // Enter-Übernahme) gemeinsam mit dem Busy-Pfad (`popup_keys`).
    if let Some(redraw) = popup_keys::handle_popup_key(app, key) {
        return redraw;
    }

    // CyclePermissionMode (Standard Shift+Tab) — Zyklus ask → auto → full →
    // plan (Plan Schritt 5). Nur außerhalb eines offenen `/command`-Popups:
    // sonst hätte dieselbe Taste zwei Bedeutungen (Popup-Navigation vs.
    // Moduszyklus).
    if action == Some(KeyAction::CyclePermissionMode) && !app.has_popup() {
        // `handle_key` läuft ausschließlich außerhalb eines laufenden Turns
        // (während eines Turns übernimmt `handle_busy_event`) — der Wechsel
        // wirkt hier also sofort, nicht vorgemerkt.
        app.cycle_permission_stage(false);
        return true;
    }

    // DeleteLine (Standard Ctrl+K) löscht die komplette aktuelle Zeile (nicht
    // nur bis Zeilenende), auch bei offenem Command-Popup — nachfolgende
    // Zeilen rücken nach oben.
    if action == Some(KeyAction::DeleteLine) {
        app.input.delete_current_line();
        app.sync_popup();
        return true;
    }

    // ToggleToolCells (Standard Ctrl+O) — klappt die letzte bzw. bei erneutem
    // Druck alle Werkzeugzellen auf/zu (Plan Schritt 2).
    if action == Some(KeyAction::ToggleToolCells) {
        return app.toggle_tool_cells();
    }

    // EndHostMode (Standard Ctrl+H) — beendet eine laufende Host-Arbeitsphase
    // sofort (Plan „UIA-Shell-Worker und Shell-Modus", Schritt 2: „eine
    // Möglichkeit, sie zu beenden"). Ohne aktive Phase tut die Taste nichts (kein Redraw).
    if action == Some(KeyAction::EndHostMode) {
        return app.end_host_mode();
    }

    // RedrawScreen (Standard Ctrl+L) — nächstes Bild zeichnet alles neu.
    if action == Some(KeyAction::RedrawScreen) {
        app.full_redraw.set(true);
        return true;
    }

    // ── ChatScroll konsultieren (PageUp/PageDown/Shift+Up/Shift+Down etc.) ──
    // Echte Werte aus dem letzten `draw_viewport`-Aufruf (vor dem ersten Draw:
    // 0 Zeilen / 20 sichtbar als sicherer Platzhalter) — ChatScroll clamped selbst.
    // Strg+Pos1/Strg+Ende gehen bei nicht-leerer Eingabe stattdessen an den
    // Composer (siehe `scroll_claims_key`).
    if scroll_claims_key(app, &key) {
        match app.scroll.handle_key(
            key,
            app.last_history_total_lines() as usize,
            app.last_history_visible_rows() as usize,
        ) {
            ScrollAction::Redraw => return true,
            ScrollAction::Passthrough => {}
        }
    }

    // InsertNewline (Standard Ctrl+J) — neue Zeile einfügen (schließt ein
    // offenes Popup).
    if action == Some(KeyAction::InsertNewline) {
        app.command_popup = None;
        app.input.insert_newline();
        return true;
    }

    // Enter übernimmt bei sichtbaren Command-Vorschlägen zuerst die markierte
    // Auswahl. Dadurch wird `/co` nie als unbekannter Command abgesendet,
    // obwohl `/compact` sichtbar gewählt werden kann. Argumentbehaftete
    // Commands sind davon nicht betroffen: `sync_popup` schließt das Popup,
    // sobald nach dem Namen Whitespace steht.
    if matches!(key.code, KeyCode::Enter) {
        // Shift/Alt+Enter fügt (wie in vielen TUIs) eine neue Zeile ein.
        if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT)
        {
            app.input.insert_newline();
            app.sync_popup();
            return true;
        }
        // Runde 5, Teil G: die Übernahme einer markierten Popup-Auswahl
        // (auch der Unterkommando-Sonderfall `/kanban `) liegt jetzt in
        // `popup_keys::handle_popup_key`, das oben bereits lief.
        // Plain Enter ohne Auswahl: getippte Zeile absenden — mit aufgelösten
        // Paste-Platzhaltern, sonst bekäme das Modell nur
        // `[Pasted text #<id> +<n> lines]` statt des eingefügten Texts.
        // Erst klassifizieren, dann leeren: eine von `classify_line`
        // abgelehnte Zeile (unterminiertes Quote, ungültiges Escape, ...)
        // bleibt sonst weder im Composer noch in der History erhalten —
        // genau das gemeldete "Nachricht war weg, musste neu getippt
        // werden". Der Composer wird deshalb nur in den akzeptierten
        // Zweigen (Quit/Ignore/Chat/Command) geleert.
        let line = app.input.submission_text();
        match classify_line(&line) {
            LineAction::Quit => {
                app.input.clear();
                app.command_popup = None;
                app.mention_popup = None;
                bus.send(HarwEvent::Quit);
            }
            LineAction::Ignore => {
                app.input.clear();
                app.command_popup = None;
                app.mention_popup = None;
            }
            LineAction::System(text) => {
                // Composer bleibt unangetastet — inklusive Paste-Platzhalter
                // —, damit die Zeile korrigiert oder erneut abgeschickt
                // werden kann; Popups schließen hier bewusst nicht, ein
                // offener `/`-Vorschlag bleibt also sichtbar. Trotzdem in
                // die History aufnehmen, damit Pfeil-Hoch die abgelehnte
                // Zeile ebenfalls findet.
                app.remember_input(&line);
                bus.send(HarwEvent::SystemMessage(text));
            }
            LineAction::Chat(text) => {
                app.input.clear();
                app.command_popup = None;
                app.mention_popup = None;
                app.remember_input(&line);
                app.scroll.force_follow();
                bus.send(HarwEvent::Submit(text));
            }
            LineAction::Command(raw) => {
                app.input.clear();
                app.command_popup = None;
                app.mention_popup = None;
                app.remember_input(&line);
                app.scroll.force_follow();
                bus.send(HarwEvent::Command(raw));
            }
        }
        return true;
    }

    // ── Popup-Navigations-Pfad ────────────────────────────────────────────
    if app.has_popup() {
        match key.code {
            // Runde 5, Teil G: Tab/Shift+Tab/Esc und die Enter-Übernahme
            // behandelt `popup_keys::handle_popup_key` (oben, gemeinsam mit
            // dem Busy-Pfad); hier verbleiben Randfälle mit Modifikatoren.
            // Im Unterkommando-Modus sind Ziffern normale Eingabe.
            KeyCode::Char(character @ '1'..='9')
                if !app
                    .command_popup
                    .as_ref()
                    .is_some_and(CommandPopup::digits_select) =>
            {
                app.input.insert_char(character);
                app.sync_popup();
                true
            }
            KeyCode::Up | KeyCode::Down | KeyCode::Esc | KeyCode::Char('1'..='9') => {
                if let Some(popup) = app.command_popup.as_mut() {
                    match popup.on_key(key) {
                        PopupAction::Stay => {}
                        PopupAction::Cancel => {
                            app.command_popup = None;
                        }
                        PopupAction::Accept(name) => {
                            let line = popup.completion_line(&name);
                            app.input.clear();
                            app.input.insert_str(&line);
                            app.command_popup = None;
                        }
                    }
                }
                true
            }
            KeyCode::Backspace => {
                app.input.backspace();
                app.sync_popup();
                true
            }
            KeyCode::Char(character) => {
                app.input.insert_char(character);
                app.sync_popup();
                true
            }
            _ => false,
        }
    } else {
        // ── Normaler Eingabe-Pfad (kein Popup aktiv) — InputEditor konsultieren ──
        // Zwei Escape-Tasten leeren den Composer. Das erste Escape ist ein
        // harmloser Rücksprung (und kann damit auch die History-Navigation
        // beenden); erst der zweite unmittelbare Druck verwirft den Text.
        if matches!(key.code, KeyCode::Esc) && !app.input.is_empty() {
            if app.escape_armed {
                app.input.clear();
                app.command_popup = None;
                app.mention_popup = None;
                app.escape_armed = false;
            } else {
                app.escape_armed = true;
                let _ = app.input.handle_key(key);
            }
            return true;
        }

        let action = app.input.handle_key(key);
        // CSI-Sicherheitsnetz (siehe `input_editor.rs`): der Esc, der eben
        // erst `escape_armed` scharfgestellt hat, war in Wahrheit der
        // Anfang einer zerstückelten CSI-Sequenz (z. B. Pos1/Ende/Maus-
        // Report nach einem host `shell.exec`, siehe dortige Doku) — kein
        // bewusster Tastendruck. Die Scharfstellung wird deshalb rückgängig
        // gemacht, damit ein später wirklich gedrücktes einzelnes Esc den
        // Composer weiterhin nicht sofort leert, sondern erst beim zweiten
        // bewussten Druck.
        if app.input.take_swallowed_escape() {
            app.escape_armed = false;
        }
        match action {
            InputAction::Submit(text) => {
                // InputEditor hat den Puffer bereits geleert.
                app.remember_input(&text);
                app.scroll.force_follow();
                app.command_popup = None;
                app.mention_popup = None;
                match classify_line(&text) {
                    LineAction::Quit => bus.send(HarwEvent::Quit),
                    LineAction::Ignore => {}
                    LineAction::System(msg) => {
                        // Puffer wurde vom InputEditor schon geleert — die
                        // abgelehnte Zeile geht sonst verloren. Wie
                        // `recall_last_pending_turn` per `insert_paste`
                        // zurückholen, damit sie korrigiert werden kann.
                        app.input.insert_paste(&text);
                        app.sync_popup();
                        bus.send(HarwEvent::SystemMessage(msg));
                    }
                    LineAction::Chat(chat_text) => bus.send(HarwEvent::Submit(chat_text)),
                    LineAction::Command(raw) => bus.send(HarwEvent::Command(raw)),
                }
                true
            }
            InputAction::Redraw => {
                app.sync_popup();
                true
            }
            InputAction::Passthrough => false,
        }
    }
}
