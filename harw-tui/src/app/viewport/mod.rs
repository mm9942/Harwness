//! Fullscreen viewport rendering.

use super::*;

/// Zeichnet die Fullscreen-Viewport: scrollbare History oben und Eingabebox unten.
///
/// # Beschreibung
/// Dreiteiliges Layout: History oben, eine dauerhafte Statuszeile und der
/// mehrzeilige Composer unten. Ein offenes `/command`-Popup überlagert den
/// unteren Teil der History.
///
/// # Fehler
/// [`TuiError::Io`] beim Zeichnen.
pub(super) fn draw_viewport(
    guard: &mut TerminalGuard,
    app: &ChatApp,
    spinner: &Spinner,
    quit_hint: Option<&str>,
) -> Result<(), TuiError> {
    if app.full_redraw.take() {
        guard.terminal().clear().map_err(TuiError::from)?;
    }
    guard
        .terminal()
        .draw(|frame| render_viewport(frame, app, spinner, quit_hint))
        .map_err(TuiError::from)?;
    Ok(())
}

/// Renders the same viewport on a real terminal or a test backend.
pub(super) fn render_viewport(
    frame: &mut ratatui::Frame<'_>,
    app: &ChatApp,
    spinner: &Spinner,
    quit_hint: Option<&str>,
) {
    crate::i18n::with_language(app.language, || {
        render_viewport_scoped(frame, app, spinner, quit_hint)
    })
}

pub(super) fn render_viewport_scoped(
    frame: &mut ratatui::Frame<'_>,
    app: &ChatApp,
    spinner: &Spinner,
    quit_hint: Option<&str>,
) {
    let theme = app.theme;
    let area = frame.area();
    // Flächen für das Maus-Routing; Overlays lassen sie leer (dann scrollt
    // das Rad wie bisher den Verlauf).
    app.last_regions
        .set(scroll_routing::FrameRegions::default());

    // Vollflächige Overlays (Session-Picker, `/export`-, Modell/Provider-,
    // Effort-Auswahl) ersetzen die gesamte Viewport (Plan Schritt 6/7) —
    // `Clear` erst, sonst bliebe Chat-Text unter dem Overlay stehen
    // (dasselbe Muster wie beim `/command`-Popup weiter unten).
    // Runde 5, Teil B: ein offenes sudo-Fenster (fängt jede Eingabe ab) darf
    // nie unter einem Overlay verborgen sein — das Overlay pausiert solange.
    match app.overlay.as_ref().filter(|_| !app.sudo.is_open()) {
        Some(Overlay::AgentTree(tree)) => {
            frame.render_widget(Clear, area);
            tree.render(area, frame.buffer_mut(), theme, &app.agent_tree_rows());
            return;
        }
        Some(Overlay::SessionPicker(picker)) => {
            frame.render_widget(Clear, area);
            picker.render(area, frame.buffer_mut(), &theme);
            return;
        }
        Some(Overlay::ModelSwitch(picker)) => {
            frame.render_widget(Clear, area);
            picker.render(area, frame.buffer_mut(), theme);
            return;
        }
        Some(Overlay::ExportChoice(dialog))
        | Some(Overlay::EffortChoice { dialog, .. })
        | Some(Overlay::JobsQuitChoice(dialog))
        | Some(Overlay::StopJob(crate::mission::stop_job::StopJobDialog { dialog, .. })) => {
            frame.render_widget(Clear, area);
            dialog.render(area, frame.buffer_mut(), theme);
            return;
        }
        Some(Overlay::View(view)) => {
            frame.render_widget(Clear, area);
            view.render(area, frame.buffer_mut(), theme);
            return;
        }
        None => {}
    }

    // Eingabehöhe wächst mit der Zeilenanzahl, begrenzt auf 3–10 — außer eine
    // Freigabefrage ist offen (Plan Schritt 3): dann ersetzt das
    // [`ApprovalDialog`]-Panel den Composer, und seine eigene
    // `desired_height` bestimmt die Höhe dieser Layout-Zeile.
    // Rahmen (2 Spalten) und das `"› "`-Präfix (2 Spalten) gehen von der
    // nutzbaren Textbreite ab; eine weitere Spalte bleibt für den Cursor frei.
    // Höhe und Cursor-Position müssen mit
    // derselben Breite rechnen, sonst laufen sie auseinander.
    let input_width = (area.width.saturating_sub(5)) as usize;
    // Runde 5, Teil B: ein offenes sudo-Fenster hat Vorrang vor jeder Frage.
    let sudo_height = app.sudo.desired_height(area.width, theme);
    // Runde 5, Teil F: danach ein offenes Plan-Fenster.
    let plan_height = app.plan_ui.desired_height(area.width, theme);
    let input_height = match (
        &app.pending_approval_dialog,
        &app.pending_host_permit_dialog,
    ) {
        _ if sudo_height.is_some() => sudo_height.unwrap_or_default(),
        _ if plan_height.is_some() => plan_height.unwrap_or_default(),
        (Some(dialog), _) => dialog.desired_height(area.width),
        // Eine Host-Permit-Frage kann nur auftreten, wenn keine normale
        // Werkzeugfreigabe offen ist (siehe `drive_pauses_to_completion`:
        // `y`/`n` gehen zuerst an eine offene `ApprovalDialog`-Frage) — die
        // beiden Panels ersetzen den Composer deshalb nie gleichzeitig.
        (None, Some(dialog)) => dialog.desired_height_for(area.width),
        (None, None) => {
            let input_line_count = app.input.visible_lines(input_width).len().clamp(1, 8) as u16;
            input_line_count + 2
        }
    };
    // Ein Dialog ersetzt den Composer: er darf den Verlauf bis auf eine
    // Zeile verdrängen, aber nie über den Bildschirm hinauswachsen — passt
    // er nicht, scrollt sein Körper, Optionen und Hinweis bleiben sichtbar
    // (siehe `crate::dialog_frame`).
    let dialog_open = sudo_height.is_some()
        || plan_height.is_some()
        || app.pending_approval_dialog.is_some()
        || app.pending_host_permit_dialog.is_some();
    let history_min: u16 = if dialog_open { 1 } else { 3 };
    let input_height = if dialog_open {
        input_height.min(area.height.saturating_sub(1 + history_min))
    } else {
        input_height
    };

    // Runde 4, Teil H: während eines Turns abgeschickte Nachrichten und
    // zurückgestellte Befehle stehen sichtbar über dem Composer, bis sie
    // ausgeliefert werden (auch nach einem Abbruch). Diese Berechnung hängt
    // von nichts unten Stehendem ab und läuft deshalb vor der
    // Bildschirm-Klassifikation.
    let queue_lines = busy_queue::queue_block_lines(
        app.pending_turns.iter().map(String::as_str),
        app.deferred_input.iter().filter_map(|event| match event {
            TuiEvent::Paste(text) => Some(text.as_str()),
            _ => None,
        }),
        area.width,
    );

    // Schmale Terminals (Hochformat, Handy): Goal-, Plan- und Host-Marke
    // bekommen eine eigene Zeile über den Statussegmenten, statt deren Breite
    // aufzuzehren. Auf einer Zeile würde `fit_segments` sonst Modus, Modell
    // und Kontext still verwerfen (Befund 2026-09-28, Hochformat-TUI).
    let status_rows = status_rows_for(app, theme, area.width);
    // Fußzeile mit Agentenzahlen: nur bei ausreichend Platz für Verlauf,
    // Status und vollständigen Composer eine eigene Zeile unterhalb der
    // Eingabe reservieren. Dialoge ersetzen den Composer und erhalten keine
    // Agenten-Fußzeile.
    let footer_visible = app.footer_counts_enabled
        && !dialog_open
        && area.height
            > input_height
                .saturating_add(status_rows)
                .saturating_add(history_min);
    let footer_area = Rect::new(
        area.x,
        area.y + area.height.saturating_sub(1),
        area.width,
        1,
    );
    // PL-97 C2: Fenster, Warteschlange und Seitenpanels (`app/render.rs`);
    // danach folgen Statuszeile, Verlauf und Composer.
    let body = render::compose_body(
        frame,
        app,
        queue_lines,
        render::BodyRows {
            status_rows,
            input_height,
            history_min,
            dialog_open,
            footer_rows: u16::from(footer_visible),
        },
    );
    let (history_area, status_area, input_area) = (body.history, body.status, body.input);

    let permission = match app.current_permission_stage() {
        PermissionCycleStage::Ask => tr("chrome.permission_ask", "Ask"),
        PermissionCycleStage::Auto => tr("chrome.permission_auto", "Auto"),
        PermissionCycleStage::Full => tr("chrome.permission_full", "Full Access"),
        PermissionCycleStage::Plan => tr("chrome.permission_plan", "Plan"),
    };
    let usage = app.display_usage();
    let cached = usage.cached_tokens.unwrap_or(0);
    let cache_write = usage.cache_write_tokens.unwrap_or(0);
    let cache_suffix = if cached > 0 || cache_write > 0 {
        tr_args(
            "chrome.cache",
            ", cache {read} / new {write}",
            &[
                ("read", &crate::agent_monitor::human_tokens(cached)),
                ("write", &crate::agent_monitor::human_tokens(cache_write)),
            ],
        )
    } else {
        String::new()
    };
    // Kontextfenster der Wurzel als Balken (aus `ContextUpdated`).
    let context_percent = app
        .agent_monitor
        .agent(app.session_id().as_str())
        .and_then(crate::agent_monitor::AgentLive::context_percent);
    let context_suffix = app
        .agent_monitor
        .agent(app.session_id().as_str())
        .and_then(|root| Some((root.context_percent()?, root.context_window)))
        .map(|(pct, window)| {
            // Schwelle der nächsten Kompaktierung (aus `ContextUpdated`), falls bekannt.
            let threshold = SessionController::context_usage(app.session_controller.as_ref())
                .and_then(|usage| usage.threshold_tokens)
                .map(|tokens| {
                    tr_args(
                        "chrome.compact_threshold",
                        ", compacts at {tokens}",
                        &[("tokens", &crate::agent_monitor::human_tokens(tokens))],
                    )
                })
                .unwrap_or_default();
            tr_args(
                "chrome.context",
                " | ctx {gauge} {percent}% / {window}{threshold}",
                &[
                    ("gauge", &crate::agent_monitor::gauge(pct, 8)),
                    ("percent", &pct.to_string()),
                    ("window", &crate::agent_monitor::human_tokens(window)),
                    ("threshold", &threshold),
                ],
            )
        })
        .unwrap_or_default();
    let explorer_suffix = if app
        .explorer
        .as_ref()
        .is_some_and(crate::explorer_panel::ExplorerPanel::is_indexing)
    {
        tr("chrome.explorer_indexing", " | Explorer indexing…")
    } else {
        ""
    };
    // Runde 5, Teil K: „· N im Hintergrund".
    let agents_suffix = background_agents::status_suffix(app, app.agent_monitor.active_count());
    // Spinner-Präfix: solange ein Turn läuft, zeigt die Statuszeile das
    // animierte Glyph plus Label — vorher wurde `spinner`/`quit_hint` zwar
    // berechnet und weitergereicht, aber nie tatsächlich gerendert. Die
    // Statuszeile ist die einzige dauerhaft sichtbare Zeile außerhalb der
    // History, deshalb landet die Animation hier statt am Input-Präfix.
    let spinner_prefix = if spinner.is_active() {
        tr_args(
            "chrome.thinking",
            "{spinner} thinking… · ",
            &[("spinner", spinner.glyph())],
        )
    } else {
        String::new()
    };
    // Transienter "Abbruch angefordert …"-Hinweis (siehe `cancel_requested_at`
    // an [`ChatApp`]) — bewusst NICHT über `push_line`, damit er nie in der
    // permanenten Verlaufshistorie landet und automatisch verschwindet,
    // sobald der Turn beendet ist oder ein neuer Turn startet.
    let cancel_suffix = if app.cancel_requested_at.is_some() {
        tr("chrome.cancel_requested", " · Cancellation requested…")
    } else {
        ""
    };
    // Transienter Hinweis, dass nach einem Ctrl+C-Abbruch die nicht-leere
    // Eingabe-Warteschlange (`deferred_input`/`pending_turns`) ausgeliefert
    // wird — analog zu `cancel_suffix` oben, keine dauerhafte
    // Verlaufszeile, verschwindet an denselben Stellen wieder
    // (`queue_kept_at`-Reset an [`ChatApp`]).
    let queue_cleared_suffix = if app.queue_kept_at.is_some() {
        tr("chrome.queue_sending", " · Sending queued messages")
    } else {
        ""
    };
    // `quit_hint` (z. B. "Ctrl+C"/"Ctrl+D") signalisiert die Scharfstellung
    // des zweistufigen Beenden-Hinweises (`QuitArm`) und wurde bisher
    // stillschweigend verworfen.
    //
    // PL-97 C4b: läuft ein Turn oder ein Job, beendet der zweite Ctrl+C nicht
    // nur die App, sondern sofort alle KI-Arbeit samt Jobs und Prozessen
    // (Not-Aus, siehe [`crate::hard_kill`]) — das soll der Hinweis auch sagen.
    // Dieselbe Bedingung geht an den Not-Aus-Detektor im Eingabe-Thread: er
    // löst nur aus, wenn dieser Hinweis den Not-Aus ankündigen würde; im
    // Leerlauf bleibt 2× Ctrl+C das normale Beenden.
    let work_running = app.active_cancel.is_some() || jobs_glue::has_running(app);
    crate::hard_kill::publish_work_running(work_running);
    let quit_suffix = match quit_hint {
        Some(label) if label == "Ctrl+C" && work_running => tr_args(
            "chrome.quit_all",
            " · {key} again: stop everything immediately",
            &[("key", label)],
        ),
        Some(label) => tr_args("chrome.quit", " · {key} again to quit", &[("key", label)]),
        None => String::new(),
    };
    // Eingereihte Nachrichten zeigt der Warteschlangen-Block über dem
    // Composer (siehe oben); die Statuszeile trägt nur noch ihre Anzahl.
    let queue_suffix = match app.pending_turns.len() {
        0 => String::new(),
        n => tr_args(
            "chrome.queued",
            " · {count} queued",
            &[("count", &n.to_string())],
        ),
    };
    let tool_suffix = if app.has_collapsed_tool_cells() {
        tr("chrome.tool_hint", " · Ctrl+O: tool/reasoning details")
    } else {
        ""
    };
    // Kontexthinweis für das fokussierte Agenten-Panel.
    let agents_hint = if matches!(
        app.panels.focus,
        crate::panes::PaneFocus::Jobs | crate::panes::PaneFocus::Finished
    ) && app.agent_detail.is_none()
    {
        // PL-97 C2: Hinweise der Mission-Fenster.
        if app.panels.focus == crate::panes::PaneFocus::Jobs {
            tr(
                "chrome.jobs_hint",
                " · Jobs · Enter: details · j/k select · Esc: chat",
            )
            .to_owned()
        } else {
            tr(
                "chrome.finished_hint",
                " · Finished/Failed · Enter/f: expand/collapse · c: acknowledge errors",
            )
            .to_owned()
        }
    } else if matches!(
        app.panels.focus,
        crate::panes::PaneFocus::Agents | crate::panes::PaneFocus::Jobs
    ) {
        match app.agent_detail.as_ref() {
            Some(detail) if detail.job.is_some() => tr(
                "chrome.job_log_hint",
                " · Job log · Esc: list · j/k/PgUp/PgDn: scroll · g/G: start/end",
            )
            .to_owned(),
            Some(detail) => {
                let entries = app
                    .agent_monitor
                    .trace(&detail.agent)
                    .map_or(0, crate::agent_monitor::AgentTrace::len);
                tr_args(
                    "chrome.trace_hint",
                    " · {count} trace entries · Esc: list · j/k/PgUp/PgDn: scroll · g/G: start/end · r: reasoning",
                    &[("count", &entries.to_string())],
                )
            }
            None => tr(
                "chrome.agents_hint",
                " · Enter: details · w/s/j/k select · f: finished · c: acknowledge errors",
            )
            .to_owned(),
        }
    } else {
        String::new()
    };
    let pending_permission_suffix = app
        .pending_permission_stage()
        .map(|_| {
            tr(
                "chrome.permission_pending",
                " · Approval mode applies after this turn",
            )
        })
        .unwrap_or("");
    let mode_segment = status_line::mode_segment(
        app.active_mode(),
        app.current_approval(),
        app.pending_permission_stage().is_some(),
    );
    let (live_provider, live_model) = app.live_model();
    let model_segment = status_line::model_segment(
        live_provider.as_deref(),
        live_model.as_deref(),
        STATUS_MODEL_MAX_CHARS,
    );
    // Kurzformen für schmale Fenster: die Statuszeile kürzt nach Vorrang
    // statt am rechten Rand abgeschnitten zu werden — zuerst Token-Details,
    // dann Kontextbalken und Hinweise; Modus, Freigabe und Modell bleiben.
    let model_short = status_line::model_segment(
        live_provider.as_deref(),
        live_model.as_deref(),
        STATUS_MODEL_MAX_CHARS / 2,
    );
    let total_tokens = crate::agent_monitor::human_tokens(usage.total());
    let context_short = context_percent
        .map(|pct| {
            tr_args(
                "chrome.context_short",
                " | ctx {percent}%",
                &[("percent", &pct.to_string())],
            )
        })
        .unwrap_or_default();
    let agents_hint_short = if agents_hint.is_empty() {
        String::new()
    } else if app.agent_detail.is_some() {
        tr("chrome.list_hint", " · Esc: list").to_owned()
    } else {
        tr("chrome.details_hint", " · Enter: details").to_owned()
    };
    let mode_short = status_line::mode_segment_short(
        app.active_mode(),
        app.current_approval(),
        app.pending_permission_stage().is_some(),
    );
    use status_line::StatusSegment as Seg;
    // R10 Welle 3B: fixierter Agenten-Titel (Name + Kurz-Digest) ganz vorne,
    // solange die Sitzung eine Titel-Überschreibung trägt
    // ([`ChatApp::with_title_override`]); mit sehr hoher Priorität, damit er
    // erst ganz zuletzt gekürzt wird.
    let title_suffix = app
        .title_override()
        .map(|title| format!(" {title} │"))
        .unwrap_or_default();
    let mut segments = vec![
        Seg::optional(255, title_suffix),
        Seg::new(
            150,
            vec![
                format!(" {spinner_prefix}Shift+Tab: {permission}"),
                format!(" {spinner_prefix}{permission}"),
            ],
        ),
        Seg::new(
            180,
            vec![format!(" | {mode_segment}"), format!(" | {mode_short}")],
        ),
        Seg::new(
            200,
            vec![format!(" | {model_segment}"), format!(" | {model_short}")],
        ),
        Seg::new(
            10,
            vec![
                tr_args(
                    "chrome.tokens",
                    " | Σ Tokens: {total} (in {input}, out {output}{cache})",
                    &[
                        ("total", &total_tokens),
                        (
                            "input",
                            &crate::agent_monitor::human_tokens(usage.prompt_tokens()),
                        ),
                        (
                            "output",
                            &crate::agent_monitor::human_tokens(usage.output_tokens),
                        ),
                        ("cache", &cache_suffix),
                    ],
                ),
                format!(" | Σ {total_tokens}"),
                String::new(),
            ],
        ),
        Seg::new(20, vec![context_suffix, context_short, String::new()]),
        Seg::optional(60, agents_suffix),
        Seg::optional(30, explorer_suffix),
        // PL-97 C3: eingeschaltete Werkbank ohne Platz in dieser Lage.
        Seg::optional(25, body.workbench_hint),
        Seg::optional(240, cancel_suffix),
        Seg::optional(230, queue_cleared_suffix),
        Seg::optional(250, quit_suffix),
        Seg::optional(70, queue_suffix),
        Seg::optional(5, tool_suffix),
        Seg::new(15, vec![agents_hint, agents_hint_short, String::new()]),
        Seg::optional(220, pending_permission_suffix),
    ];
    // Info-Zwischenstände der Kind-Agenten (30s-Fenster, passend zur Drossel
    // `PARENT_INFO_MIN_INTERVAL`): Ältere Einträge werden nicht aufgenommen
    // (nur ausgeblendet, nicht gelöscht).
    // Die Reihenfolge bestimmt der Live-Aktivitäts-Ranker (PL-94 W01): zuletzt
    // bedeutsam aktive Kinder zuerst, ruhige und beendete zuletzt; bei Platzmangel
    // kürzt `fit_segments` das weiter rechts stehende Segment zuerst.
    for child in app.child_info_display_order(Instant::now()) {
        let Some((info, at)) = app.child_infos.get(&child) else {
            continue;
        };
        let age = at.elapsed().as_secs();
        if child_info_visible(age) {
            segments.push(Seg::optional(
                120,
                status_line::child_info_segment(&child, Some(info), age, 40),
            ));
        } else if app.status_expiry == harw_config::StatusExpiryMode::Consolidate {
            // `[tui].status_expiry = "consolidate"` (Default): abgelaufene
            // Meldungen nur noch kompakt (Name + Alter) und niedrig priorisiert.
            segments.push(Seg::optional(60, child_info_expired_segment(&child, age)));
        }
        // Modus "hide": abgelaufene Meldungen entfallen komplett.
    }
    // Solange eine Host-Arbeitsphase läuft, muss die Statuszeile das gemäß
    // `docs/design/mediated-process-execution.md` („permanent und
    // unübersehbar `HOST-MODUS AKTIV`") in einer eigenen, hervorgehobenen
    // Warnfarbe zeigen. Die Warnung steht deshalb **vorne** und ihr Platz ist
    // fest reserviert; Plan- und Goal-Marke (Runde 5, Teile F/P) teilen sich
    // nur den Rest (Review #60: dahinter fiel sie im Hochformat heraus).
    let two_rows = status_area.height >= 2;
    let host_text = host_mode_mark(status_area.width);
    let host_span = app.host_mode_active().then(|| {
        Span::styled(
            host_text,
            Style::default()
                .fg(style::warning_color(theme))
                .add_modifier(Modifier::BOLD),
        )
    });
    let plan_spans = (app.current_permission_stage() == PermissionCycleStage::Plan).then(|| {
        let full = crate::plan_dialog::plan_status_span(theme);
        let short = Span::styled(tr("chrome.plan_short", PLAN_MARK_SHORT), full.style);
        (full, short)
    });
    // `status_spans` teilt ihre Breite durch drei: auf zwei Zeilen darf die
    // Goal-Marke die ganze erste Zeile nutzen, einzeilig wie bisher.
    let goal_budget = if two_rows {
        status_area.width.saturating_mul(3)
    } else {
        status_area.width
    };
    let goal_spans = app.goal_status_spans(theme, goal_budget);
    let dod_spans = app.dod_mark.visible().map(|mark| {
        let (full, short) = status_line::dod_mark_text(&mark.count_text(), mark.verified);
        let style = crate::dod_view::mark_style(mark, theme);
        (Span::styled(full, style), Span::styled(short, style))
    });
    let (mut marks, marks_width) = status_mark_spans(
        host_span,
        dod_spans,
        plan_spans,
        goal_spans,
        usize::from(status_area.width),
    );
    let status_width = if two_rows {
        usize::from(status_area.width)
    } else {
        usize::from(status_area.width).saturating_sub(marks_width)
    };
    let status = status_line::fit_segments(&segments, status_width);
    let status_style = Style::default().fg(style::border_color(theme));
    if two_rows {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(status_area);
        frame.render_widget(Paragraph::new(Line::from(marks)), rows[0]);
        frame.render_widget(Paragraph::new(status).style(status_style), rows[1]);
    } else {
        marks.push(Span::styled(status, status_style));
        frame.render_widget(Paragraph::new(Line::from(marks)), status_area);
    }

    // ── History ──────────────────────────────────────────────────────
    // Alle Zellen zu einem flachen Zeilen-Vec zusammenführen. PL-97 C5: die
    // kanonischen Zellen unverändert, dazwischen die virtuellen Live-Zellen
    // der Hintergrund-Läufe (`app/live_slots.rs`).
    let width = history_area.width;
    let live_projection = app.live_slot_projection(Instant::now(), history_area.height);
    let segments = app.history_segments(&live_projection, width, theme, history_area.height);
    // Identitätsanker nur beim Hochscrollen (die Messung kostet einen Umbruch
    // je Segment).
    let geometry = (!app.scroll.is_following())
        .then(|| live_slots::SegmentGeometry::measure(&segments, width));
    let mut all_lines: Vec<Line<'static>> = segments
        .into_iter()
        .flat_map(|segment| segment.lines)
        .collect();
    // Transient: live gestreamtes Reasoning/Text der laufenden Runde.
    if !app.live_reasoning.is_empty() {
        all_lines.extend(live_reasoning_lines(
            &app.live_reasoning,
            width,
            theme,
            app.reasoning_mode,
        ));
    }
    if !app.live_stream.is_empty() {
        for (index, text) in app.live_stream.lines().enumerate() {
            let prefix = if index == 0 { "● " } else { "  " };
            all_lines.push(Line::styled(
                format!("{prefix}{}", crate::sanitize::sanitize_inline(text)),
                style::assistant_style(theme),
            ));
        }
        all_lines.push(Line::styled("  ▍", style::dim_style(theme)));
    }

    // Scroll-Offset: 0 = ganz unten; wächst nach oben.
    // Gesamtzeilen → sichtbaren Bereich berechnen → Paragraph.scroll() aufrufen.
    // Die gerenderte Zeilenzahl ist nicht die Zahl der `Line`-Objekte:
    // `Wrap` bricht lange Zeilen zusätzlich um. Würde der Scroll-Offset
    // gegen die ungewrappte Zahl geclamped, bliebe der obere Teil langer
    // Ausgaben (etwa `/help`) unerreichbar.
    let history_widget = Paragraph::new(all_lines).wrap(Wrap { trim: false });
    let total_lines = u16::try_from(history_widget.line_count(width)).unwrap_or(u16::MAX);
    let visible_rows = history_area.height;
    // Für PageUp/PageDown/Home/End außerhalb des Draw-Closures cachen
    // (`handle_key` hat keinen Zugriff auf `frame.area()`).
    app.last_history_total_lines.set(total_lines);
    app.last_history_visible_rows.set(visible_rows);
    // Rohes Offset (in Zeilen vom Anfang gesehen).
    // Folgen nur am Ende: hochgescrollt bleibt der sichtbare Text stehen,
    // auch wenn unten Zeilen dazukommen (Streaming, Werkzeugergebnisse,
    // Kind-Agenten, Systemzeilen) oder ein Freigabe-Dialog den Verlauf
    // verkleinert — `sync_layout` führt den Anker nach (siehe `chat_scroll`).
    let back = app
        .scroll
        .sync_layout(usize::from(total_lines), usize::from(visible_rows), width);
    let back = match geometry.as_ref() {
        Some(geometry) => app.scroll.sync_anchor(
            usize::from(total_lines),
            usize::from(visible_rows),
            |anchor| geometry.resolve(anchor),
            |top| geometry.locate(top),
        ),
        None => back,
    };
    let scroll_from_top: u16 = if total_lines > visible_rows {
        let max_offset = total_lines - visible_rows;
        // Offset zählt vom Ende → in „von oben" umrechnen.
        max_offset.saturating_sub(u16::try_from(back).unwrap_or(u16::MAX))
    } else {
        0
    };

    frame.render_widget(history_widget.scroll((scroll_from_top, 0)), history_area);

    // Hinweis, solange die Ansicht nicht folgt (unterste Verlaufszeile,
    // rechtsbündig; ein offenes Popup zeichnet danach darüber).
    if let Some(text) = scroll_indicator_text(&app.scroll, "Ctrl+End")
        && history_area.height > 0
    {
        let label = format!(" {text} ");
        let label_width = u16::try_from(status_line::display_width(&label))
            .unwrap_or(u16::MAX)
            .min(history_area.width);
        let indicator_area = Rect {
            x: history_area.x + history_area.width - label_width,
            y: history_area.y + history_area.height - 1,
            width: label_width,
            height: 1,
        };
        frame.render_widget(Clear, indicator_area);
        frame.render_widget(
            Paragraph::new(label).style(
                Style::default()
                    .fg(style::warning_color(theme))
                    .add_modifier(Modifier::REVERSED),
            ),
            indicator_area,
        );
    }
    // PL-97 C5: eingefrorene Live-Reihenfolge — was seitdem passierte, eine
    // Zeile über dem Hinweis.
    if let Some(text) = live_slots::new_activity_label(live_projection.new_activity)
        && history_area.height > 1
    {
        let label = format!(" {text} ");
        let label_width = u16::try_from(status_line::display_width(&label))
            .unwrap_or(u16::MAX)
            .min(history_area.width);
        let activity_area = Rect {
            x: history_area.x + history_area.width - label_width,
            y: history_area.y + history_area.height - 2,
            width: label_width,
            height: 1,
        };
        frame.render_widget(Clear, activity_area);
        frame.render_widget(
            Paragraph::new(label).style(
                Style::default()
                    .fg(style::accent_color(theme))
                    .add_modifier(Modifier::REVERSED),
            ),
            activity_area,
        );
    }

    // Popup überlagert den unteren Teil des History-Bereichs (falls offen).
    let popup_open = app
        .command_popup
        .as_ref()
        .is_some_and(|popup| !popup.is_empty());
    if popup_open && let Some(popup) = app.command_popup.as_ref() {
        let popup_height = popup
            .preferred_height(POPUP_MAX_ROWS)
            .min(history_area.height);
        if popup_height > 0 {
            let popup_area = Rect::new(
                history_area.x,
                history_area.y + history_area.height - popup_height,
                history_area.width,
                popup_height,
            );
            // Ohne `Clear` bleibt der Chat-Text unter dem Popup
            // stehen: die Zellen, die das Popup nicht selbst
            // überschreibt, behalten ihren alten Inhalt, und beide
            // Texte lesen sich ineinander verschachtelt.
            frame.render_widget(Clear, popup_area);
            popup.render(popup_area, frame.buffer_mut(), theme);
        }
    }

    // `@`-Erwähnungs-Popup (nur mit Treffern; schließt das Befehls-Popup
    // aus, siehe `ChatApp::sync_mention_popup`).
    if let Some(popup) = app.mention_popup.as_ref().filter(|popup| !popup.is_empty()) {
        let popup_height = POPUP_MAX_ROWS.min(history_area.height);
        if popup_height > 0 {
            let popup_area = Rect::new(
                history_area.x,
                history_area.y + history_area.height - popup_height,
                history_area.width,
                popup_height,
            );
            frame.render_widget(Clear, popup_area);
            popup.render(popup_area, frame.buffer_mut(), theme);
        }
    }

    // ── Eingabe / Freigabe-Panel ─────────────────────────────────────
    // Solange eine Freigabefrage offen ist, ersetzt das `ApprovalDialog` den
    // Composer vollständig (Plan Schritt 3); der Rest des Layouts (History,
    // Status) bleibt unverändert. Kein Text-Cursor in diesem Fall — das Panel
    // wird über Pfeiltasten/Ziffern bedient, nicht getippt.
    // Runde 5, Teil B: das sudo-Fenster ersetzt den Composer vor allen
    // anderen Fragen; kein Text-Cursor (das Passwort wird nie angezeigt).
    if app.sudo.render(input_area, frame.buffer_mut(), theme) {
        return;
    }
    // Runde 5, Teil F: Plan-Freigabe (Plan über dem Verlauf, Optionen statt
    // Composer), Plan-Vorschlag und `ask_user`-Auswahl.
    if app
        .plan_ui
        .render(input_area, history_area, frame.buffer_mut(), theme)
    {
        return;
    }
    if let Some(dialog) = &app.pending_approval_dialog {
        dialog.render(input_area, frame.buffer_mut(), &theme);
        return;
    }
    // Dieselbe Composer-Ersetzung für eine offene Host-Permit-Frage (Plan
    // „UIA-Shell-Worker und Shell-Modus", Schritt 2) — nur erreichbar, wenn
    // keine `ApprovalDialog`-Frage offen ist (siehe Kommentar bei
    // `input_height`).
    if let Some(dialog) = &app.pending_host_permit_dialog {
        dialog.render(input_area, frame.buffer_mut(), theme);
        return;
    }

    // Plan Teil F: beginnt der getippte Text mit `!`, rendert der Composer
    // im Shell-Modus (eigene Akzentfarbe für Rahmen und Prompt, eigener
    // Titel/Hinweis) — `is_shell_mode_input` ist eine reine Prädikatsfunktion
    // auf dem noch nicht abgeschickten Text, siehe dort.
    let shell_mode = is_shell_mode_input(app.input.text());
    let prompt_style = if shell_mode {
        style::shell_mode_style(theme)
    } else {
        Style::default()
    };
    let mut input_lines: Vec<Line<'static>> = Vec::new();
    for (index, segment) in app.input.visible_lines(input_width).iter().enumerate() {
        if index == 0 {
            input_lines.push(Line::from(vec![
                Span::styled("› ", prompt_style),
                Span::raw(segment.clone()),
            ]));
        } else {
            input_lines.push(Line::from(format!("  {segment}")));
        }
    }
    let (cursor_row, cursor_col) = app.input.cursor_position(input_width);
    let input_rows = input_area.height.saturating_sub(2) as usize;
    let input_top = cursor_row.saturating_sub(input_rows.saturating_sub(1));
    // Runde 5, Teil F: im Plan-Modus zeigt der leere Composer den Hinweis
    // „Plan-Modus – es wird nichts verändert“ (Rahmen/Titel in Plan-Farbe).
    let plan_mode_composer =
        !shell_mode && app.current_permission_stage() == PermissionCycleStage::Plan;
    if plan_mode_composer && app.input.text().is_empty() {
        input_lines = vec![crate::plan_dialog::plan_placeholder_line(theme)];
    }
    let (title_text, title_style, border_style) = if shell_mode {
        (
            tr(
                "chrome.shell_title",
                " Shell mode · Enter runs · Esc/Backspace at start exits ",
            ),
            style::shell_mode_style(theme),
            Style::default().fg(style::shell_mode_color(theme)),
        )
    } else if plan_mode_composer {
        crate::plan_dialog::plan_composer_chrome(theme)
    } else {
        (
            " harw ",
            style::selected_style(theme),
            Style::default().fg(style::border_color(theme)),
        )
    };
    let input_widget = Paragraph::new(input_lines.into_iter().skip(input_top).collect::<Vec<_>>())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(title_text, title_style))
                .border_style(border_style),
        );
    frame.render_widget(input_widget, input_area);
    if footer_visible && footer_area.width > 0 && footer_area.height > 0 {
        let (active, failed) = app.agent_monitor.footer_counts();
        frame.render_widget(
            Paragraph::new(format!("Agenten aktiv {active} · fehlgeschlagen {failed}")),
            footer_area,
        );
    }

    // Terminal-Cursor auf die Schreibposition setzen. Ohne diesen Aufruf
    // blendet ratatui den Cursor für den Frame aus, und der Schreibende
    // sieht nicht, wo das nächste Zeichen landet.
    let max_x = input_area.x + input_area.width.saturating_sub(2);
    let max_y = input_area.y + input_area.height.saturating_sub(2);
    let cursor_x = input_area
        .x
        .saturating_add(3)
        .saturating_add(u16::try_from(cursor_col).unwrap_or(u16::MAX))
        .min(max_x);
    let cursor_y = input_area
        .y
        .saturating_add(1)
        .saturating_add(u16::try_from(cursor_row.saturating_sub(input_top)).unwrap_or(u16::MAX))
        .min(max_y);
    frame.set_cursor_position((cursor_x, cursor_y));
}
