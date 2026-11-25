//! Turn events and history-cell projection.

use super::*;

/// Findet oder erzeugt die eine geteilte [`ToolCell`] für `call_id` (Plan
/// Schritt 2/3, Contract-Slice B3).
///
/// # Beschreibung
/// Eine Freigabefrage für einen Aufruf trifft ein, **bevor** dessen
/// `ToolCallCompleted` feststeht, aber unter Umständen knapp **vor oder nach**
/// dem zugehörigen `TurnEvent::ToolCallRequested` — beide laufen über
/// getrennte Kanäle (`turn_event_rx` bzw. der Fragekanal des
/// [`ApprovalDriver`]), deren relative Ankunftsreihenfolge nicht garantiert
/// ist. Diese Funktion macht beide Aufrufer robust: existiert bereits eine
/// Zelle für `call_id` (`state.pending_tool_cells`), wird sie unverändert
/// zurückgegeben (keine zweite Zelle, kein zweiter Verlaufseintrag); sonst
/// wird sie über [`ToolCell::started`] neu angelegt, in `state` vermerkt und
/// über [`ChatApp::append_tool_cell`] an den Verlauf gehängt.
///
/// # Argumente
/// - `app` (`&mut ChatApp`): Renderer-Zustand, der die Zelle aufnimmt.
/// - `state` (`&mut TurnEventState`): hält `call_id → Zelle`.
/// - `call_id` (`harw_types::ToolCallId`): Kennung des Aufrufs.
/// - `call` (`&ToolCall`): vollständiger Aufruf (Name + Argumente).
/// - `turn` (`Option<&str>`): Turn-Kennung, wenn bekannt (PL-97 M3:
///   Turn-Grenze des Aktivitäts-Reducers); `None` auf dem Freigabepfad.
///
/// # Rückgabe
/// Die eine, geteilte [`SharedToolCell`] dieses Aufrufs.
pub(super) fn ensure_tool_cell(
    app: &mut ChatApp,
    state: &mut TurnEventState,
    call_id: harw_types::ToolCallId,
    call: &ToolCall,
    turn: Option<&str>,
) -> SharedToolCell {
    if let Some(existing) = state.pending_tool_cells.get(&call_id) {
        return Arc::clone(existing);
    }
    let cell: SharedToolCell =
        Arc::new(Mutex::new(crate::i18n::with_language(app.language, || {
            ToolCell::started(call)
        })));
    state.pending_tool_cells.insert(call_id, Arc::clone(&cell));
    app.append_tool_cell(call, turn, Arc::clone(&cell));
    cell
}

/// Verarbeitet genau ein [`TurnEvent`] in den Renderer-Zustand.
///
/// # Beschreibung
/// AP W5-10b. Als freie Funktion ausgelagert, damit sowohl der Event-Loop
/// ([`run_loop`]) als auch der laufende Turn ([`drive_turn_animated`]) dieselbe
/// Verarbeitung benutzen — und damit sie ohne Terminal testbar ist.
///
/// Zuordnung Ereignis → Zelle:
/// - `ToolCallRequested` → **eine** geteilte [`ToolCell`] je `call_id`, über
///   [`ensure_tool_cell`] angelegt bzw. wiedergefunden (Plan Schritt 2:
///   „eine Zelle statt zwei"); aufeinanderfolgende lesende `fs.*`-Aufrufe
///   werden über [`ChatApp::append_tool_cell`] zu einer [`ToolGroupCell`]
///   gebündelt,
/// - `ToolCallCompleted` → schreibt **dieselbe** Zelle über [`ToolCell::complete`]
///   fort (kein zweiter Verlaufseintrag),
/// - `ItemAdded(Reasoning)` → [`ReasoningHistoryCell`], nur bei nicht-leerer
///   Zusammenfassung; schließt eine offene Lese-Gruppe,
/// - `ItemAdded(AssistantMessage)` → nur bei `phase == Some(Commentary)` eine
///   sichtbare Assistant-Zeile über [`ChatApp::push_line`] (Live-Anzeige des
///   kurzen Ack-Texts vor Tool-Aufrufen); bei `FinalAnswer` oder fehlender
///   Phase bleibt die Anzeige `reveal_reply` am Turn-Ende vorbehalten, sonst
///   entstünde eine Doppelanzeige. Der live gepushte Text wird zusätzlich in
///   [`TurnEventState::last_commentary_text`] festgehalten — `reveal_reply`s
///   Aufrufer ([`drive_turn_animated`]) gleicht seine Turn-Antwort dagegen ab
///   ([`suppress_if_matches_commentary`]), als zweite Verteidigungslinie
///   gegen eine doppelte Anzeige derselben Nachricht,
/// - `ChildSpawned` → **eine** [`SubAgentCell`] je `child_id`, geteilt über
///   [`SharedHistoryCell`]; schließt eine offene Lese-Gruppe,
/// - `ChildProgress` / `ChildCompleted` → schreiben **dieselbe** Zelle fort
///   (drei Ereignisse, eine Zelle),
/// - `PlanUpdated` → [`PlanGraphCell`], wenn Plan-Dienste vorliegen und der
///   aktuelle Plan lesbar ist; sonst eine Systemzeile mit derselben Information,
/// - `ModeChanged` → Anzeigemodus der Statuszeile.
///
/// `TurnCompleted` wird hier bewusst **nicht** verarbeitet: die Token-Summary
/// läuft exklusiv über den `SessionEvent`-Pfad, sonst entstünden Doppelantworten.
///
/// # Argumente
/// - `app` (`&mut ChatApp`): Renderer-Zustand, der die Zellen aufnimmt.
/// - `state` (`&mut TurnEventState`): Seitenkanäle (Werkzeugzellen, Kind-Zellen).
/// - `event` ([`TurnEvent`]): das zu verarbeitende Ereignis; Besitz geht über.
///
/// # Rückgabe
/// `true`, wenn sich der sichtbare Zustand geändert hat und ein Redraw nötig ist.
/// Obergrenze (Zeichen) des vorgehaltenen Live-Reasoning-Schwanzes
/// (PL-97 R1: genug für acht umbrochene Zeilen samt Absätzen).
pub(super) const LIVE_REASONING_MAX_CHARS: usize = 4000;
/// Maximale Zahl umbrochener Live-Reasoning-Zeilen im Chat bei
/// `[tui] reasoning = "collapsed"`.
pub(super) const LIVE_REASONING_MAX_LINES: usize = 3;
/// Maximale Zahl umbrochener Live-Reasoning-Zeilen (nach der Kopfzeile) bei
/// `[tui] reasoning = "message"` (PL-97 R1).
pub(super) const LIVE_REASONING_MESSAGE_MAX_LINES: usize = 8;

/// Baut die transienten Live-Reasoning-Zeilen je nach `[tui] reasoning`
/// (PL-97 R1):
/// - `message`: Kopfzeile `∴ Reasoning …` plus die letzten (bis zu)
///   [`LIVE_REASONING_MESSAGE_MAX_LINES`] umbrochenen Zeilen mit erhaltenen
///   Absätzen und Randspalte (siehe [`crate::reasoning_cell::live_message_lines`]),
/// - `collapsed`: die letzten (bis zu) [`LIVE_REASONING_MAX_LINES`]
///   umbrochenen Zeilen, gedimmt, die erste mit `"∴ "`-Markierung,
/// - `hidden`: keine Zeilen.
///
/// # Argumente
/// - `text` (`&str`): Roher, gestreamter Reasoning-Text (wird sanitisiert).
/// - `width` (`u16`): Breite der Verlaufsfläche.
/// - `theme` ([`style::Theme`]): Farbschema.
/// - `mode` ([`ReasoningDisplayMode`]): konfigurierte Darstellung.
pub(super) fn live_reasoning_lines(
    text: &str,
    width: u16,
    theme: style::Theme,
    mode: ReasoningDisplayMode,
) -> Vec<Line<'static>> {
    match mode {
        ReasoningDisplayMode::Hidden => return Vec::new(),
        ReasoningDisplayMode::Message => {
            return crate::reasoning_cell::live_message_lines(
                text,
                width,
                theme,
                LIVE_REASONING_MESSAGE_MAX_LINES,
            );
        }
        ReasoningDisplayMode::Collapsed => {}
    }
    let sanitized = crate::sanitize::sanitize_inline(text);
    let wrapped = crate::history_cell::wrap_plain(sanitized.trim(), width.saturating_sub(4));
    let skip = wrapped.len().saturating_sub(LIVE_REASONING_MAX_LINES);
    wrapped
        .into_iter()
        .skip(skip)
        .enumerate()
        .map(|(index, line)| {
            let content: String = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            let prefix = if index == 0 { "  ∴ " } else { "    " };
            Line::styled(format!("{prefix}{content}"), style::dim_style(theme))
        })
        .collect()
}

pub(super) fn handle_turn_event(app: &mut ChatApp, state: &mut TurnEventState, event: TurnEvent) -> bool {
    crate::i18n::with_language(app.language, || handle_turn_event_scoped(app, state, event))
}

pub(super) fn handle_turn_event_scoped(
    app: &mut ChatApp,
    state: &mut TurnEventState,
    event: TurnEvent,
) -> bool {
    match event {
        TurnEvent::ToolCallRequested {
            turn_id,
            call_id,
            tool_name,
            arguments,
            ..
        } => {
            app.clear_live_stream();
            let surface_root_id = app.session_id().as_str().to_owned();
            app.agent_monitor
                .surface_root_tool(&surface_root_id, "assistant", Some(&tool_name));
            app.reasoning_feed
                .push_tool_start(&surface_root_id, "assistant", &tool_name);
            // PL-97 M3: Turn-Kennung für die Turn-Grenze des Reducers.
            let turn_key = turn_id.as_str().to_owned();
            // Runde 5, Teil O: Turn des Aufrufs für die Abbruch-Markierung.
            state.tool_call_turns.insert(call_id.clone(), turn_id);
            let already_known = state.pending_tool_cells.contains_key(&call_id);
            let call = ToolCall {
                id: call_id.clone(),
                name: ToolName::new(tool_name.clone()),
                arguments: arguments.clone(),
            };
            ensure_tool_cell(app, state, call_id.clone(), &call, Some(&turn_key));
            if let Some(index) = state.export_tool_calls.get(&call_id).copied() {
                if let Some(ExportEntry::ToolCall {
                    tool_name: exported_name,
                    arguments: exported_arguments,
                    ..
                }) = app.export_entries.get_mut(index)
                {
                    *exported_name = tool_name;
                    *exported_arguments = arguments;
                }
            } else {
                app.export_entries.push(export_tool_call_entry(
                    &call_id,
                    call.name.as_str(),
                    call.arguments.clone(),
                ));
                state
                    .export_tool_calls
                    .insert(call_id.clone(), app.export_entries.len().saturating_sub(1));
            }
            // Bereits während einer Freigabefrage angelegt (Wettlauf zwischen
            // den beiden Kanälen, siehe `ensure_tool_cell`-Doku): kein neuer
            // sichtbarer Zustand, kein Redraw nötig.
            !already_known
        }
        TurnEvent::ToolCallCompleted {
            call_id,
            result,
            duration_ms,
            placement,
            ..
        } => {
            let cell = match state.pending_tool_cells.get(&call_id).cloned() {
                Some(cell) => cell,
                None => {
                    tracing::warn!(call_id = %call_id, "tui.tool_cell.completed_without_request");
                    let orphan = ToolCall {
                        id: call_id.clone(),
                        name: ToolName::new("tool.result"),
                        arguments: serde_json::json!({
                            "call_id": call_id.to_string(),
                            "orphaned": true,
                        }),
                    };
                    let cell = Arc::new(Mutex::new(ToolCell::started(&orphan)));
                    state
                        .pending_tool_cells
                        .insert(call_id.clone(), Arc::clone(&cell));
                    app.append_tool_cell(&orphan, None, Arc::clone(&cell));
                    cell
                }
            };
            let tool_name = state
                .export_tool_calls
                .get(&call_id)
                .and_then(|index| app.export_entries.get(*index))
                .and_then(|entry| match entry {
                    ExportEntry::ToolCall { tool_name, .. } => Some(tool_name.clone()),
                    _ => None,
                });
            // Register „CSI-Sicherheitsnetz und Paste-Platzhalter", Punkt 7:
            // ein abgeschlossener `shell.exec`-Aufruf kann Terminal-Modi
            // (Raw-Mode/Alternate-Screen/Bracketed-Paste/Maus-Capture) beschädigt
            // zurücklassen — der Aufrufer (mit Zugriff auf den
            // `TerminalGuard`) reasserted sie best-effort, sobald diese
            // Vormerkung ansteht (siehe `ChatApp::take_needs_terminal_reassert`).
            if tool_name.as_deref() == Some("shell.exec") {
                app.request_terminal_reassert();
            }
            let surface_root_id = app.session_id().as_str().to_owned();
            app.agent_monitor
                .surface_root_tool(&surface_root_id, "assistant", None);
            let (feed_ok, feed_summary) = feed_tool_outcome(&result);
            app.reasoning_feed.push_tool_end(
                &surface_root_id,
                "assistant",
                tool_name.as_deref().unwrap_or("tool.result"),
                feed_ok,
                &feed_summary,
            );
            if !state.export_tool_calls.contains_key(&call_id) {
                app.export_entries.push(export_tool_call_entry(
                    &call_id,
                    tool_name.as_deref().unwrap_or("tool.result"),
                    serde_json::json!({ "call_id": call_id.to_string(), "orphaned": true }),
                ));
                state
                    .export_tool_calls
                    .insert(call_id.clone(), app.export_entries.len().saturating_sub(1));
            }
            // A6: die Dauer gehört zum `ExportEntry::ToolResult` unten
            // (`export_tool_result_entry`), nicht zum `ToolCall`-Eintrag —
            // kein Backfill von `duration_ms` hier mehr.
            let export_entry = match cell.lock() {
                Ok(mut guard) => {
                    // R18 D-D: der Ort kommt vom ausführenden Teil.
                    guard.complete_at(&result, duration_ms, placement.as_ref());
                    Some(export_tool_result_entry(
                        &call_id,
                        tool_name,
                        &result,
                        duration_ms,
                        None,
                    ))
                }
                Err(_) => {
                    tracing::error!(call_id = %call_id, "tui.tool_cell.lock_poisoned");
                    None
                }
            };
            if let Some(entry) = export_entry {
                app.export_entries.push(entry);
            }
            // PL-97 M3: Ende des Aufrufs in den Aktivitäts-Reducer (nach dem
            // Lösen der Sperre: der Adapter liest den Zellzustand selbst).
            activity_glue::tool_completed(app, call_id.as_str(), &cell, Some(&result));
            // Runde 5, Teil E: „auto ✓ <Grund>“ bzw. „Vom Auto-Modus
            // abgelehnt · <Kategorie>“ an der Zelle; löst der Deckel aus,
            // erscheint sein Hinweis einmal als Systemzeile.
            let auto_notice = app
                .runtime()
                .and_then(|rt| rt.auto_mode())
                .and_then(|auto| {
                    if let Some(note) = auto
                        .log()
                        .verdict_for(call_id.as_str())
                        .as_ref()
                        .and_then(auto_note_for)
                        && let Ok(mut guard) = cell.lock()
                    {
                        guard.set_approval_note(&note);
                    }
                    auto.log().take_notice()
                });
            if let Some(notice) = auto_notice {
                app.push_line(Role::System, notice);
            }
            true
        }
        TurnEvent::ItemAdded {
            item: TurnItem::Reasoning(reasoning),
            ..
        } => {
            // PL-97 R1: Leerzeilen zwischen den Einträgen erhalten die Absätze.
            let summary = reasoning.summary_text.join("\n\n");
            // Wie `hydrate_visible_history`: nur Leerraum ergibt keine Zelle.
            if summary.trim().is_empty() {
                return false;
            }
            app.close_tool_group();
            app.export_entries
                .push(ExportEntry::Reasoning(summary.clone()));
            app.push_reasoning_cell(summary, None);
            // Die fertige Zelle ersetzt die Live-Vorschau (in jedem Modus),
            // sonst stünde derselbe Text zweimal auf dem Schirm.
            app.live_reasoning.clear();
            let surface_root_id = app.session_id().as_str().to_owned();
            app.agent_monitor.sync_surface_root_stream(
                &surface_root_id,
                "assistant",
                &app.live_reasoning,
                &app.live_stream,
            );
            true
        }
        TurnEvent::AssistantDelta { text, .. } => {
            app.live_stream.push_str(&text);
            let surface_root_id = app.session_id().as_str().to_owned();
            app.agent_monitor.sync_surface_root_stream(
                &surface_root_id,
                "assistant",
                &app.live_reasoning,
                &app.live_stream,
            );
            true
        }
        TurnEvent::ReasoningDelta { text, .. } => {
            app.live_reasoning.push_str(&text);
            let surface_root_id = app.session_id().as_str().to_owned();
            app.reasoning_feed
                .push_reasoning(&surface_root_id, "assistant", &text);
            // Nur ein Schwanz bleibt vorgehalten — genug für die letzten
            // acht umbrochenen Zeilen auch auf breiten Terminals.
            let count = app.live_reasoning.chars().count();
            if count > LIVE_REASONING_MAX_CHARS {
                app.live_reasoning = app
                    .live_reasoning
                    .chars()
                    .skip(count - LIVE_REASONING_MAX_CHARS)
                    .collect();
            }
            app.agent_monitor.sync_surface_root_stream(
                &surface_root_id,
                "assistant",
                &app.live_reasoning,
                &app.live_stream,
            );
            true
        }
        TurnEvent::CompactionApplied {
            reason,
            items_before,
            items_after,
            tokens_before,
            tokens_after,
            summarized,
            elided_results,
            ..
        } => {
            // `turn_event_rx` trägt nur die Ereignisse der Root-Session
            // (Kind-Sessions melden über den Agenten-Bus) — die Momentaufnahme
            // für `/status` und `/usage` bleibt damit root-exklusiv.
            // Runde 6, Teil C: Verdichtung merken (Rückfall der Schlussantwort).
            state.final_reply.record_compaction();
            app.session_controller
                .record_compaction(harw_operations::LastCompaction {
                    reason: reason.clone(),
                    tokens_before,
                    tokens_after,
                    summarized,
                    elided_results,
                });
            app.push_line(
                Role::System,
                format!("⟲ Kontext verdichtet ({reason}): {items_before} → {items_after} Einträge"),
            );
            true
        }
        TurnEvent::ContextUpdated {
            used_tokens,
            window_tokens,
            estimated_next_tokens,
            threshold_tokens,
            reserve_tokens,
            ..
        } => {
            // Nur Root-Ereignisse erreichen `turn_event_rx` (siehe oben);
            // die Momentaufnahme speist `/status` und `/usage`.
            app.session_controller.record_context_updated(
                used_tokens,
                window_tokens,
                estimated_next_tokens,
                threshold_tokens,
                reserve_tokens,
            );
            // Die Werte der Statuszeile liest weiterhin der Agenten-Monitor
            // (Bus); hier nur ein Redraw-Anstoß.
            app.drain_agent_events();
            true
        }
        TurnEvent::UsageUpdated { .. } => {
            // Die Werte selbst liest die Statuszeile aus dem Agenten-Monitor
            // (Bus); hier nur ein Redraw-Anstoß.
            app.drain_agent_events();
            true
        }
        TurnEvent::ItemAdded {
            item: TurnItem::AssistantMessage(message),
            ..
        } => {
            // Die Runde ist fertig: der gestreamte Vorschautext wird durch die
            // echte Nachricht ersetzt (Commentary hier, Final via reveal_reply).
            app.clear_live_stream();
            // Nur der kurze Zwischentext vor einem Tool-Aufruf wird hier live
            // gerendert. Die finale Turn-Antwort bleibt exklusiv `reveal_reply`
            // am Turn-Ende vorbehalten (sonst erschiene sie doppelt: einmal
            // hier, einmal beim Abschluss).
            if message.phase != Some(harw_types::MessagePhase::Commentary) {
                // Runde 6, Teil C: die Schlussantwort aus dem Ereignis merken —
                // unabhängig davon, was eine Verdichtung mit dem Verlauf macht.
                state
                    .final_reply
                    .record_final(visible_message_text(&message.content));
                return false;
            }
            let text = visible_message_text(&message.content);
            app.push_line(Role::Assistant, text.clone());
            // Zweite Verteidigungslinie gegen eine Doppelanzeige: siehe
            // `TurnEventState::last_commentary_text`-Doku und
            // `drive_turn_animated`.
            state.last_commentary_text = Some(text);
            true
        }
        TurnEvent::ItemAdded {
            item: TurnItem::Error(error),
            ..
        } => {
            app.export_entries
                .push(ExportEntry::Error(ExportErrorEntry {
                    code: Some("item_error".to_owned()),
                    message: error.message.clone(),
                    details: Some(serde_json::json!({ "retryable": error.retryable })),
                    agent: None,
                }));
            app.push_line(Role::System, format!("⚠ {}", error.message));
            true
        }
        TurnEvent::ChildSpawned {
            child,
            role,
            question,
            ..
        } => {
            app.close_tool_group();
            let child_id = child.as_str().to_owned();
            let parent_id = exported_agent_parent_id(app, &child);
            let cell = Arc::new(Mutex::new(SubAgentCell {
                language: crate::i18n::current_language(),
                child_id: child_id.clone(),
                role: role.clone(),
                question: question.clone(),
                tool_calls: 0,
                tokens: 0,
                status: SubAgentStatus::Running,
            }));
            state
                .child_cells
                .insert(child_id.clone(), Arc::clone(&cell));
            app.child_cells.insert(child_id.clone(), Arc::clone(&cell));
            app.export_entries
                .push(ExportEntry::Agent(ExportAgentEntry {
                    agent_id: child_id.clone(),
                    role: Some(role.clone()),
                    parent_id,
                    status: Some("running".to_owned()),
                    summary: question.clone(),
                }));
            app.push_shared_cell(cell);
            // Runde 5, Teil I: Live-Block (Orchestrator-Kinder) direkt darunter.
            app.attach_child_stream_block(&child_id, &role, question.as_deref());
            tracing::debug!(child = %child_id, "tui.child_cell.created");
            true
        }
        TurnEvent::ChildProgress {
            child,
            tool_calls,
            tokens,
            ..
        } => {
            let parent_id = exported_agent_parent_id(app, &child);
            let updated = state.update_child(child.as_str(), |cell| {
                cell.apply_progress(tool_calls, tokens);
            });
            if updated {
                app.export_entries
                    .push(ExportEntry::Agent(ExportAgentEntry {
                        agent_id: child.as_str().to_owned(),
                        role: None,
                        parent_id,
                        status: Some("running".to_owned()),
                        summary: Some(format!("{tool_calls} Tool-Aufrufe, {tokens} Tokens")),
                    }));
            }
            updated
        }
        TurnEvent::ChildCompleted {
            child,
            outcome,
            duration_ms,
            ..
        } => {
            let parent_id = exported_agent_parent_id(app, &child);
            let updated = state.update_child(child.as_str(), |cell| {
                cell.apply_completion(outcome.clone(), duration_ms);
            });
            if updated {
                app.export_entries
                    .push(ExportEntry::Agent(ExportAgentEntry {
                        agent_id: child.as_str().to_owned(),
                        role: None,
                        parent_id,
                        status: Some(outcome),
                        summary: Some(format!("Dauer: {duration_ms} ms")),
                    }));
            }
            updated
        }
        TurnEvent::PlanUpdated {
            plan_id,
            revision,
            summary,
        } => {
            // Bewusst als `let`-Bindung: eine Scrutinee-Temporäre würde die
            // gemeinsame Leihe auf `app` über den ganzen `match` halten und
            // damit `push_cell`/`push_line` blockieren.
            let current_plan = app
                .plan_services()
                .map(|services| services.plan_store.current());
            match current_plan {
                Some(Ok(plan)) => {
                    app.close_tool_group();
                    // Der Graph wird sichtbar über `push_cell` gezeigt, aber
                    // `/export` liest ausschließlich aus `export_entries` mit —
                    // ohne diesen Push würde der häufigste Fall (Plan-Dienste
                    // vorhanden, Plan lesbar) beim Export komplett fehlen.
                    app.export_entries.push(ExportEntry::Plan(ExportPlanEntry {
                        plan_id: Some(plan_id.clone()),
                        revision: Some(revision),
                        summary: summary.clone(),
                        steps: Vec::new(),
                        status: None,
                        agent: None,
                    }));
                    // R18 F4: jeder weitere Stand desselben Plans zeigt nur
                    // Fortschritt und geänderte Knoten; Ctrl+O klappt die
                    // vollständige Liste auf.
                    app.push_plan_cell(plan);
                }
                Some(Err(error)) => {
                    tracing::warn!(
                        plan_id = %plan_id,
                        revision,
                        error = %error,
                        "tui.plan_cell.plan_unreadable"
                    );
                    app.push_line(
                        Role::System,
                        format!("Plan {plan_id} @{revision}: {summary} (Plan nicht lesbar)"),
                    );
                    app.export_entries.push(ExportEntry::Plan(ExportPlanEntry {
                        plan_id: Some(plan_id.clone()),
                        revision: Some(revision),
                        summary: format!("{summary} (Plan nicht lesbar: {error})"),
                        steps: Vec::new(),
                        status: None,
                        agent: None,
                    }));
                }
                // Keine Plan-Dienste durchgereicht: die Information geht nicht
                // verloren, sie wird nur einzeilig statt als Graph gezeigt.
                None => {
                    app.push_line(
                        Role::System,
                        format!("Plan {plan_id} @{revision}: {summary}"),
                    );
                    app.export_entries.push(ExportEntry::Plan(ExportPlanEntry {
                        plan_id: Some(plan_id),
                        revision: Some(revision),
                        summary,
                        steps: Vec::new(),
                        status: None,
                        agent: None,
                    }));
                }
            }
            true
        }
        TurnEvent::ModeChanged { mode } => match InteractionMode::parse(&mode) {
            Some(parsed) => {
                app.set_active_mode(parsed);
                true
            }
            None => {
                tracing::warn!(mode = %mode, "tui.mode.unknown_name_ignored");
                false
            }
        },
        // Runde 5, Teil O: nur offene Aufrufe des beendeten Turns; beim
        // Abbruch mit echtem Grund (Nutzerin vs. Turn-Grenze).
        TurnEvent::TurnFailed {
            turn_id, reason, ..
        } => {
            let surface_root_id = app.session_id().as_str().to_owned();
            app.agent_monitor
                .finish_surface_root(&surface_root_id, false, true);
            let _ = mark_incomplete_tool_exports(
                app,
                state,
                &turn_id,
                &format!("unvollständig ({reason})"),
            );
            true
        }
        TurnEvent::TurnAborted { turn_id } => {
            let surface_root_id = app.session_id().as_str().to_owned();
            app.agent_monitor
                .finish_surface_root(&surface_root_id, true, false);
            let label = turn_safety::aborted_label(app);
            let _ = mark_incomplete_tool_exports(app, state, &turn_id, &label);
            true
        }
        // TurnCompleted läuft exklusiv über den SessionEvent-Pfad, damit die
        // Token-Summary nicht doppelt erscheint. TurnStarted trägt keinen
        // zusätzlichen sichtbaren Zustand.
        _ => false,
    }
}

/// Marks outstanding tools as incomplete when a turn terminates early.
///
/// Runde 5, Teil O: nur Aufrufe, die in `turn_id` angefragt wurden (oder deren
/// Turn unbekannt ist, etwa eine vorab gezeigte Freigabefrage). Zellen früherer
/// Turns bleiben unberührt — ein längst beendeter Aufruf wird nie nachträglich
/// als „abgebrochen“ exportiert.
pub(super) fn mark_incomplete_tool_exports(
    app: &mut ChatApp,
    state: &mut TurnEventState,
    turn_id: &harw_types::TurnId,
    reason: &str,
) -> bool {
    let call_ids: Vec<_> = state
        .pending_tool_cells
        .keys()
        .filter(|call_id| {
            state
                .tool_call_turns
                .get(*call_id)
                .is_none_or(|requested_in| requested_in == turn_id)
        })
        .cloned()
        .collect();
    let mut changed = false;
    let mut marked: Vec<harw_types::ToolCallId> = Vec::new();
    for call_id in call_ids {
        let Some(cell) = state.pending_tool_cells.get(&call_id) else {
            continue;
        };
        let Ok(mut cell) = cell.lock() else {
            tracing::error!(call_id = %call_id, "tui.tool_cell.lock_poisoned");
            continue;
        };
        if cell.state != ToolState::Running || state.export_incomplete_tools.contains_key(&call_id)
        {
            continue;
        }
        // Runde 5, Teil M: der echte Grund des Turn-Endes, nicht
        // „Resume-Abbruch" (das gilt nur für den Resume-Pfad).
        cell.mark_incomplete_with(reason);
        let tool_name = state
            .export_tool_calls
            .get(&call_id)
            .and_then(|index| app.export_entries.get(*index))
            .and_then(|entry| match entry {
                ExportEntry::ToolCall { tool_name, .. } => Some(tool_name.clone()),
                _ => None,
            });
        app.export_entries.push(export_tool_result_entry(
            &call_id,
            tool_name,
            &ToolCallResult::error(reason),
            0,
            Some(ResultTrust::Runtime),
        ));
        state.export_incomplete_tools.insert(call_id.clone(), ());
        changed = true;
        marked.push(call_id);
    }
    // PL-97 M3: die abgebrochenen Aufrufe enden auch im Reducer
    // (fehlgeschlagen), nachdem die Sperren gelöst sind.
    for call_id in marked {
        if let Some(cell) = state.pending_tool_cells.get(&call_id).cloned() {
            activity_glue::tool_completed(app, call_id.as_str(), &cell, None);
        }
    }
    changed
}
