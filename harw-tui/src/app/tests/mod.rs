use super::*;
use crate::mission::tests::screen_row;
use crate::test_support::{TestError, TestResult, ctx};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use harw_agent_dsl::ExecutableAgentIr;
use harw_agent_dsl::roles::AgentRoleId;
use harw_core::{
    ApprovalResolution, InMemoryStateStore, ModelFuture, ModelProvider, ModelRequest,
    ModelResponse, SpawnContext,
};
use harw_extension_api::approval_mode::ApprovalModeCell;
use harw_extension_api::{
    ExtensionRegistry, ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture,
    ToolName, ToolOutput, ToolProvider, ToolSpec,
};
use harw_operations::SessionController;
use harw_operations::registry::OperationRegistry;
use harw_registry_defaults::profile::role_names;
use harw_tools::serde_json::{Value, json};
use harw_tools::{FunctionToolSpec, JsonSchema};
use harw_types::{AgentRole, ApprovalActor, ItemId, ToolCallId, TurnId};

use crate::approval::TuiApprovalHandler;
use crate::command_exec::build_services;
use crate::events::harw_event_channel;

// ────────────────────────────────────────────────────────────────────
// W2d-2 / T2b (CONTRACTS-W2d2 §2 T2b): dieses Testmodul lief bis W2d-2
// gegen Montage-Helfer, die app.rs selbst besaß (`build_tui_agent_session`,
// `trusted_tui_spawn_context`, `assemble_tui_registry`,
// `registry_with_approval_handler`, `build_tui_managed_spawner`,
// `TuiChildRegistryFactory`, `tui_child_limits`, `TuiModelToolServices`,
// `LOCAL_TUI_OPERATION_PERMISSION`, die Projekt-Wurzel-Session-ID-Ableitung).
// app.rs montiert nichts mehr selbst (`crate::runtime_root`,
// `harw_runtime::RuntimeAssembly`). Tests, die ausschließlich das Verhalten
// dieser gelöschten Montage-Helfer prüften, sind entfernt — ihre Abdeckung
// steht jetzt in harw-runtime/harw-registry-defaults/harw-core. Tests, die echtes TUI-Verhalten
// prüfen (Popup, Tastatur, Freigabefluss, `/mode`, Zellen-Rendering),
// bleiben erhalten; wo sie eine Session brauchten, bauen sie sie jetzt
// direkt über `AgentSession::new_with_id(..).with_spawn_context(..)`.
// ────────────────────────────────────────────────────────────────────

/// Baut eine gültige Test-`SandboxSpec` gegen ein eindeutiges Temp-Verzeichnis
/// (Muster übernommen aus `harw-operations/src/adapter/command.rs`).
fn test_sandbox() -> TestResult<SandboxSpec> {
    use harw_authority::{Permission, PermissionSet, WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{TenantId, WorkspaceId};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let root =
        std::env::temp_dir().join(format!("harw-tui-app-test-{}-{}", std::process::id(), id));
    std::fs::create_dir_all(root.join("workspace")).map_err(ctx("temp workspace dir"))?;
    let registry = WorkspaceRegistry::build(
        &root,
        [WorkspaceRegistration {
            tenant: TenantId::from_str("tui-test"),
            workspace: WorkspaceId::from_str("workspace"),
            root: PathBuf::from("workspace"),
        }],
    )
    .map_err(ctx("workspace registry build"))?;
    let binding = registry
        .resolve(
            &TenantId::from_str("tui-test"),
            &WorkspaceId::from_str("workspace"),
        )
        .map_err(ctx("resolve workspace binding"))?;
    Ok(SandboxSpec::from_resolved(
        binding,
        PermissionSet::from_policy([Permission::ReadWorkspace, Permission::WriteWorkspace]),
    ))
}

/// Runde 5, Teil B: solange das sudo-Fenster offen ist, erreicht keine
/// Taste und kein Paste den Composer, die Busy-Warteschlange oder die
/// Eingabe-Historie; Verlauf und Export-Einträge enthalten das Passwort
/// nie, und nach Enter geht genau eine Antwort an die Ausführung.
#[tokio::test]
async fn sudo_dialog_swallows_every_key_and_paste_and_never_logs_the_password() -> TestResult {
    let mut app = ChatApp::new(Vec::new(), test_sandbox()?, SessionId::new());
    let (prompt, answer) = harw_tool_shell::SudoPrompt::new(
        "s1".to_owned(),
        "uia-shell-worker".to_owned(),
        vec!["apt-get".to_owned(), "update".to_owned()],
        PathBuf::from("/workspace"),
        "Paketlisten".to_owned(),
        false,
    );
    // Bereits scharf: das Fenster ist seit zwei Sekunden offen.
    let Some(shown) = Instant::now().checked_sub(Duration::from_secs(2)) else {
        return Ok(());
    };
    assert!(app.sudo.open(prompt, shown).is_none());
    // Die Historie kann aus einer persistierten Datei vorbelegt sein;
    // entscheidend ist, dass der Dialog nichts hinzufügt.
    let history_before = app.input.history().len();
    for c in "sehr-geheim".chars() {
        crate::sudo_dialog::route_event(
            &mut app,
            TuiEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
        );
    }
    crate::sudo_dialog::route_event(&mut app, TuiEvent::Paste("-paste".to_owned()));
    for code in [KeyCode::PageUp, KeyCode::F(6), KeyCode::Home, KeyCode::End] {
        crate::sudo_dialog::route_event(
            &mut app,
            TuiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)),
        );
    }
    assert!(app.input.text().is_empty(), "Composer bleibt leer");
    assert_eq!(
        app.input.history().len(),
        history_before,
        "keine neue Eingabe-Historie"
    );
    assert!(
        !app.input
            .history()
            .iter()
            .any(|entry| entry.contains("geheim")),
        "das Passwort steht nie in der Historie"
    );
    assert!(app.deferred_input.is_empty(), "keine Busy-Warteschlange");
    assert!(app.pending_turns.is_empty());
    assert!(app.overlay.is_none(), "keine Ansicht per Hotkey");
    assert!(app.sudo.is_open());

    crate::sudo_dialog::route_event(
        &mut app,
        TuiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
    );
    assert!(!app.sudo.is_open());
    assert!(matches!(
        answer.into_answer().await,
        Some(harw_tool_shell::SudoAnswer::Password {
            remember: false,
            ..
        })
    ));
    let exported = format!("{:?}", app.export_entries);
    assert!(!exported.contains("geheim"), "{exported}");
    assert!(exported.contains("apt-get update"), "{exported}");
    Ok(())
}

/// Baut einen `ChatApp`-Testzustand mit leerer Adapter-Pipeline und einer
/// frischen Test-Sandbox. Für Tests, die nur Verlauf/Popup/Scroll prüfen
/// (nicht die Command-Adapter-Pipeline selbst — dafür siehe
/// `command_exec.rs`).
///
/// # Beschreibung
/// `ChatApp::new` baut `command_registry` aus der (hier leeren)
/// Adapter-Pipeline (`CommandRegistry::from_command_adapters`) — für
/// Popup-/Tab-Tests wird die Registry deshalb im Anschluss durch
/// [`CommandRegistry::built_in()`] ersetzt, damit `/`-Präfixe echte
/// Treffer liefern statt eines leeren, sofort wieder geschlossenen Popups.
pub(super) fn test_chat_app() -> TestResult<ChatApp> {
    let mut app = ChatApp::new(Vec::new(), test_sandbox()?, SessionId::new());
    app.command_registry = CommandRegistry::built_in().map_err(ctx("built_in"))?;
    Ok(app)
}

/// R10 Welle 3B: [`ChatApp::with_hidden_commands`] entfernt die
/// benannten Befehle aus `command_registry` — nicht mehr per `find`
/// auffindbar, also weder im Popup noch in der Vervollständigung, und
/// die übrigen Einträge bleiben unverändert.
#[test]
fn with_hidden_commands_removes_specs_from_the_registry() -> TestResult {
    let mut app = test_chat_app()?;
    app.command_registry = CommandRegistry::built_in().map_err(ctx("built_in"))?;
    assert!(app.command_registry.find("model").is_some());
    assert!(app.command_registry.find("agent").is_some());
    let app = app.with_hidden_commands(vec!["Model".to_owned(), "agent".to_owned()]);
    assert!(app.command_registry.find("model").is_none());
    assert!(app.command_registry.find("agent").is_none());
    // Ein nicht versteckter Befehl bleibt erreichbar.
    assert!(app.command_registry.find("mode").is_some());
    Ok(())
}

/// Ein leerer Filter ist ein No-Op: die Registry bleibt unverändert.
#[test]
fn with_hidden_commands_empty_list_is_a_no_op() -> TestResult {
    let mut app = test_chat_app()?;
    app.command_registry = CommandRegistry::built_in().map_err(ctx("built_in"))?;
    let before = app.command_registry.specs().len();
    let app = app.with_hidden_commands(Vec::new());
    assert_eq!(app.command_registry.specs().len(), before);
    Ok(())
}

/// Ein versteckter Befehl verschwindet auch aus dem `/`-Popup: es liest
/// ausschließlich `command_registry.specs()`
/// ([`CommandPopup::new`]), das [`ChatApp::with_hidden_commands`] bereits
/// gefiltert hat.
#[test]
fn with_hidden_commands_removes_the_command_from_the_popup() -> TestResult {
    use crate::command::{CommandDomain, CommandScope, OutputSurface, PermissionTier};

    let solo_spec = |name: &str| -> TestResult<crate::CommandSpec> {
        crate::CommandSpec::new(
            name,
            Vec::<String>::new(),
            CommandScope::TuiOnly,
            PermissionTier::Operator,
            OutputSurface::Inline,
            CommandDomain::Misc,
        )
        .map_err(ctx("spec"))
    };
    let mut app = test_chat_app()?;
    app.command_registry = CommandRegistry::new(vec![solo_spec("model")?, solo_spec("diary")?]);
    app.input.insert_str("/mo");
    app.sync_popup();
    assert!(
        !app.command_popup
            .as_ref()
            .is_some_and(CommandPopup::is_empty),
        "model should still match /mo before hiding it"
    );

    let mut app = app.with_hidden_commands(vec!["model".to_owned()]);
    app.command_popup = None;
    app.input.clear();
    app.input.insert_str("/mo");
    app.sync_popup();
    assert!(
        app.command_popup
            .as_ref()
            .is_some_and(CommandPopup::is_empty),
        "hidden /model must no longer match /mo in the popup"
    );
    Ok(())
}

/// Enter im Agenten-Panel öffnet die Detailansicht (Panel maximiert),
/// Tasten scrollen/schalten dort, Esc kehrt zur Liste zurück, ohne den
/// Fokus an den Chat abzugeben, und stellt den Vollbild-Zustand wieder her.
#[test]
fn agents_panel_enter_opens_detail_and_esc_closes_it() -> TestResult {
    let mut app = test_chat_app()?;
    app.agent_monitor
        .seed_usage("agent-1", "explorer", TokenUsage::default());
    app.panels.agents_visible = true;
    app.panels.focus = crate::panes::PaneFocus::Agents;
    assert!(!app.panels.maximized);

    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    assert_eq!(handle_panel_key(&mut app, key(KeyCode::Enter)), Some(true));
    let detail = app
        .agent_detail
        .clone()
        .ok_or(TestError::Missing("agent detail"))?;
    assert_eq!(detail.agent.as_str(), "agent-1");
    assert_eq!(detail.scroll.offset(), 0);
    assert!(app.panels.maximized, "Detailansicht maximiert das Panel");

    assert_eq!(
        handle_panel_key(&mut app, key(KeyCode::Char('k'))),
        Some(true)
    );
    assert_eq!(handle_panel_key(&mut app, key(KeyCode::PageUp)), Some(true));
    assert_eq!(
        app.agent_detail.as_ref().map(|d| d.scroll.offset()),
        Some(11)
    );
    assert_eq!(
        handle_panel_key(&mut app, key(KeyCode::Char('j'))),
        Some(true)
    );
    assert_eq!(
        app.agent_detail.as_ref().map(|d| d.scroll.offset()),
        Some(10)
    );
    assert_eq!(handle_panel_key(&mut app, key(KeyCode::Home)), Some(true));
    // Vor dem ersten Zeichnen ist die Spurlänge unbekannt: „ganz oben"
    // ist unbegrenzt und wird beim nächsten Zeichnen gekappt.
    assert_eq!(
        app.agent_detail.as_ref().map(|d| d.scroll.offset()),
        Some(usize::MAX)
    );
    assert_eq!(handle_panel_key(&mut app, key(KeyCode::End)), Some(true));
    assert_eq!(
        app.agent_detail.as_ref().map(|d| d.scroll.offset()),
        Some(0)
    );
    let before = app.agent_detail.as_ref().map(|d| d.show_reasoning);
    assert_eq!(
        handle_panel_key(&mut app, key(KeyCode::Char('r'))),
        Some(true)
    );
    assert_ne!(app.agent_detail.as_ref().map(|d| d.show_reasoning), before);

    assert_eq!(handle_panel_key(&mut app, key(KeyCode::Esc)), Some(true));
    assert!(app.agent_detail.is_none());
    assert_eq!(app.panels.focus, crate::panes::PaneFocus::Agents);
    assert!(!app.panels.maximized, "vorheriger Vollbild-Zustand zurück");

    // Ein zweites Esc gibt den Fokus wie bisher an den Chat zurück.
    assert_eq!(handle_panel_key(&mut app, key(KeyCode::Esc)), Some(true));
    assert_eq!(app.panels.focus, crate::panes::PaneFocus::Chat);
    Ok(())
}

/// Runde 5: Ctrl+O wirkt auch während eines laufenden Turns sofort und
/// landet nicht im Composer bzw. in der Busy-Queue. PL-97 R1: in der
/// Vorgabe `message` startet die Reasoning-Zelle ausgeklappt, Ctrl+O
/// klappt sie ein.
#[test]
fn toggle_tool_cells_works_while_busy() -> TestResult {
    let mut app = test_chat_app()?;
    app.push_reasoning_cell("erste Zeile\nzweite Zeile".to_owned(), None);
    assert!(
        !app.has_collapsed_tool_cells(),
        "`message` startet ausgeklappt"
    );
    let ctrl_o = TuiEvent::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
    assert!(matches!(
        handle_busy_event(&mut app, ctrl_o),
        BusyKeyOutcome::Redraw
    ));
    assert!(app.has_collapsed_tool_cells());
    assert!(app.input.text().is_empty(), "Ctrl+O darf nichts eintippen");
    Ok(())
}

/// Ctrl+O klappt auch Reasoning-Zellen zu und wieder auf (Vorgabe
/// `message`: ausgeklappt → eingeklappt → ausgeklappt).
#[test]
fn toggle_tool_cells_expands_reasoning_cells() -> TestResult {
    let mut app = test_chat_app()?;
    app.push_reasoning_cell("erste Zeile\nzweite Zeile".to_owned(), None);
    assert!(!app.has_collapsed_tool_cells());
    assert!(app.toggle_tool_cells());
    assert!(app.has_collapsed_tool_cells());
    assert!(app.toggle_tool_cells());
    assert!(!app.has_collapsed_tool_cells());
    Ok(())
}

/// Testhilfe: die Zeilen des Verlaufs als Strings (PL-97 R1).
fn history_strings(app: &ChatApp, width: u16) -> Vec<String> {
    app.cells
        .iter()
        .flat_map(|cell| cell.display_lines(width, style::Theme::Dark))
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

/// PL-97 R1 (`message`): die Zelle startet ausgeklappt mit Kopfzeile und
/// Markdown-Körper; Ctrl+O reduziert sie auf Kopfzeile + erste Zeile +
/// Hinweis und ein zweites Ctrl+O klappt sie wieder aus.
#[test]
fn reasoning_message_mode_starts_expanded_and_ctrl_o_collapses() -> TestResult {
    let mut app = test_chat_app()?;
    app.push_reasoning_cell("Erste Zeile\n\nZweite Zeile\nDritte Zeile".to_owned(), None);
    let expanded = history_strings(&app, 80);
    assert_eq!(
        expanded,
        vec![
            "∴ Reasoning".to_owned(),
            "│ Erste Zeile".to_owned(),
            "│ ".to_owned(),
            "│ Zweite Zeile".to_owned(),
            "│ Dritte Zeile".to_owned(),
        ]
    );
    assert!(app.toggle_tool_cells());
    assert_eq!(
        history_strings(&app, 80),
        vec![
            "∴ Reasoning".to_owned(),
            "│ Erste Zeile".to_owned(),
            "│ … (4 lines · Ctrl+O)".to_owned(),
        ]
    );
    assert!(app.toggle_tool_cells());
    assert_eq!(history_strings(&app, 80), expanded);
    Ok(())
}

/// PL-97 R1 (`collapsed`): die Zelle startet eingeklappt, nutzt aber
/// ausgeklappt denselben Renderer.
#[test]
fn reasoning_collapsed_mode_starts_collapsed() -> TestResult {
    let mut app = test_chat_app()?.with_reasoning_mode(ReasoningDisplayMode::Collapsed);
    app.push_reasoning_cell("- erstens\n- zweitens".to_owned(), None);
    assert!(app.has_collapsed_tool_cells());
    assert_eq!(
        history_strings(&app, 80),
        vec![
            "∴ Reasoning".to_owned(),
            "│ • erstens".to_owned(),
            "│ … (2 lines · Ctrl+O)".to_owned(),
        ]
    );
    assert!(app.toggle_tool_cells());
    assert_eq!(
        history_strings(&app, 80),
        vec![
            "∴ Reasoning".to_owned(),
            "│ • erstens".to_owned(),
            "│ • zweitens".to_owned(),
        ]
    );
    Ok(())
}

/// PL-97 R1: `summary_text` aus reinem Leerraum (nach dem `"\n\n"`-Join
/// also etwa `"\n\n"`) ergibt weder Zelle noch Export-Eintrag — wie in
/// `hydrate_visible_history`.
#[test]
fn reasoning_whitespace_only_summary_pushes_nothing() -> TestResult {
    use harw_protocol::items::ReasoningItem;
    use harw_types::{ItemId, TurnId};

    let mut app = test_chat_app()?;
    let mut state = TurnEventState::default();
    let cells_before = app.cells_len();
    let exports_before = app.export_entries.len();
    let handled = handle_turn_event(
        &mut app,
        &mut state,
        TurnEvent::ItemAdded {
            turn_id: TurnId::new(),
            item: TurnItem::Reasoning(ReasoningItem {
                id: ItemId::new(),
                summary_text: vec![String::new(), " ".to_owned()],
                raw_content: Vec::new(),
            }),
        },
    );
    assert!(!handled);
    assert_eq!(app.cells_len(), cells_before);
    assert_eq!(app.export_entries.len(), exports_before);
    Ok(())
}

/// PL-97 R1 (`hidden`): `ItemAdded(Reasoning)` schiebt keine Zelle in den
/// Verlauf, zeichnet den Export-Eintrag aber weiterhin auf — mit den
/// `summary_text`-Einträgen als Absätze.
#[test]
fn reasoning_hidden_mode_pushes_no_cell_but_records_export() -> TestResult {
    use harw_protocol::items::ReasoningItem;
    use harw_types::{ItemId, TurnId};

    let mut app = test_chat_app()?.with_reasoning_mode(ReasoningDisplayMode::Hidden);
    let mut state = TurnEventState::default();
    let before = app.cells_len();
    let handled = handle_turn_event(
        &mut app,
        &mut state,
        TurnEvent::ItemAdded {
            turn_id: TurnId::new(),
            item: TurnItem::Reasoning(ReasoningItem {
                id: ItemId::new(),
                summary_text: vec!["erster Absatz".to_owned(), "zweiter Absatz".to_owned()],
                raw_content: Vec::new(),
            }),
        },
    );
    assert!(handled);
    assert_eq!(app.cells_len(), before, "hidden: keine Verlaufszelle");
    assert!(app.tool_cells.is_empty());
    assert_eq!(
        app.export_entries.last(),
        Some(&ExportEntry::Reasoning(
            "erster Absatz\n\nzweiter Absatz".to_owned()
        ))
    );
    assert!(
        live_reasoning_lines("x", 80, style::Theme::Dark, ReasoningDisplayMode::Hidden)
            .is_empty()
    );
    Ok(())
}

/// PL-97 R1 (PR #154, P2): Ende-zu-Ende je Modus — `ReasoningDelta` und
/// `ItemAdded(Reasoning)` laufen durch den echten Event-Handler, der
/// Bildschirm wird über `render_viewport` gezeichnet. `message` zeigt
/// Live-Kopfzeile und ausgeklappten Block, `collapsed` die gedimmte
/// Dreizeilen-Vorschau und den eingeklappten Block, `hidden` nichts —
/// der Export-Eintrag bleibt in allen drei Modi erhalten.
#[test]
fn reasoning_modes_end_to_end_through_event_handler() -> TestResult {
    use harw_protocol::items::ReasoningItem;
    use harw_types::{ItemId, TurnId};

    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 40))
        .map_err(ctx("test terminal"))?;
    let mut screen = |app: &ChatApp| -> TestResult<String> {
        terminal
            .draw(|frame| render_viewport(frame, app, &Spinner::new(), None))
            .map_err(ctx("draw"))?;
        let buffer = terminal.backend().buffer();
        Ok((0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n"))
    };

    let mut results = Vec::new();
    for mode in [
        ReasoningDisplayMode::Message,
        ReasoningDisplayMode::Collapsed,
        ReasoningDisplayMode::Hidden,
    ] {
        let mut app = test_chat_app()?.with_reasoning_mode(mode);
        let mut state = TurnEventState::default();
        let turn_id = TurnId::new();
        assert!(handle_turn_event(
            &mut app,
            &mut state,
            TurnEvent::ReasoningDelta {
                turn_id: turn_id.clone(),
                text: "Liveabsatz eins\n\nLiveabsatz zwei".to_owned(),
            },
        ));
        let live = screen(&app)?;
        // Dieselbe Live-Projektion, die `render_viewport` an den Verlauf hängt.
        let live_history: Vec<String> = live_reasoning_lines(
            &app.live_reasoning,
            80,
            style::Theme::Dark,
            app.reasoning_mode,
        )
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();
        assert!(handle_turn_event(
            &mut app,
            &mut state,
            TurnEvent::ItemAdded {
                turn_id,
                item: TurnItem::Reasoning(ReasoningItem {
                    id: ItemId::new(),
                    summary_text: vec![
                        "Erster Absatz".to_owned(),
                        "- Punkt eins\n- Punkt zwei".to_owned(),
                    ],
                    raw_content: Vec::new(),
                }),
            },
        ));
        assert_eq!(
            app.export_entries.last(),
            Some(&ExportEntry::Reasoning(
                "Erster Absatz\n\n- Punkt eins\n- Punkt zwei".to_owned()
            )),
            "{mode:?}: Export-Eintrag fehlt"
        );
        // Der echte Übergang: ItemAdded ersetzt die Live-Vorschau.
        assert!(app.live_reasoning.is_empty(), "{mode:?}");
        let after = screen(&app)?;
        assert!(!after.contains("∴ Reasoning …"), "{mode:?}: {after}");
        let headers = after.matches("∴ Reasoning").count();
        let expected_headers = usize::from(mode != ReasoningDisplayMode::Hidden);
        assert_eq!(headers, expected_headers, "{mode:?}: {after}");
        results.push((mode, live, live_history, history_strings(&app, 80)));
    }

    for (mode, live, live_history, history) in &results {
        match mode {
            ReasoningDisplayMode::Message => {
                assert!(live.contains("∴ Reasoning …"), "{live}");
                assert!(live.contains("│ Liveabsatz eins"), "{live}");
                assert!(live.contains("│ Liveabsatz zwei"), "{live}");
                assert_eq!(
                    live_history,
                    &vec![
                        "∴ Reasoning …".to_owned(),
                        "│ Liveabsatz eins".to_owned(),
                        "│ ".to_owned(),
                        "│ Liveabsatz zwei".to_owned(),
                    ]
                );
                assert_eq!(
                    history,
                    &vec![
                        "∴ Reasoning".to_owned(),
                        "│ Erster Absatz".to_owned(),
                        "│ ".to_owned(),
                        "│ • Punkt eins".to_owned(),
                        "│ • Punkt zwei".to_owned(),
                    ]
                );
            }
            ReasoningDisplayMode::Collapsed => {
                assert!(!live.contains("∴ Reasoning …"), "{live}");
                assert_eq!(
                    live_history,
                    &vec!["  ∴ Liveabsatz eins Liveabsatz zwei".to_owned()]
                );
                assert_eq!(
                    history,
                    &vec![
                        "∴ Reasoning".to_owned(),
                        "│ Erster Absatz".to_owned(),
                        "│ … (4 lines · Ctrl+O)".to_owned(),
                    ]
                );
            }
            ReasoningDisplayMode::Hidden => {
                // Der Agenten-Monitor spiegelt den Live-Strom weiterhin
                // (bewusst unverändert); der Verlauf zeigt nichts davon.
                assert!(!live.contains("∴ Reasoning"), "{live}");
                assert!(live_history.is_empty(), "{live_history:?}");
                assert!(
                    !history.iter().any(|line| line.contains("Reasoning")
                        || line.contains("Absatz")
                        || line.contains("Punkt")),
                    "{history:?}"
                );
            }
        }
    }
    Ok(())
}

/// Live-Reasoning in `collapsed` zeigt höchstens drei umbrochene Zeilen,
/// die neuesten.
#[test]
fn live_reasoning_shows_last_three_wrapped_lines() {
    let text = (0..40)
        .map(|index| format!("wort{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let lines = live_reasoning_lines(
        &text,
        24,
        style::Theme::Dark,
        ReasoningDisplayMode::Collapsed,
    );
    assert_eq!(lines.len(), 3);
    let rendered: Vec<String> = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();
    assert!(rendered[0].starts_with("  ∴ "));
    assert!(rendered[2].contains("wort39"));
}

/// PL-97 R1: Live-Reasoning in `message` zeigt die Kopfzeile
/// `∴ Reasoning …` plus höchstens acht umbrochene Zeilen (die neuesten),
/// Absatzgrenzen bleiben als Leerzeile erhalten; Breite 24 ohne Panik.
#[test]
fn live_reasoning_message_mode_shows_header_and_eight_lines() {
    let text = (0..40)
        .map(|index| format!("wort{index}"))
        .collect::<Vec<_>>()
        .join(" ");
    let lines =
        live_reasoning_lines(&text, 24, style::Theme::Dark, ReasoningDisplayMode::Message);
    assert_eq!(lines.len(), 1 + LIVE_REASONING_MESSAGE_MAX_LINES);
    let rendered: Vec<String> = lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();
    assert_eq!(rendered[0], "∴ Reasoning …");
    assert!(rendered[1].starts_with("│ "));
    assert!(rendered[8].contains("wort39"));

    let short = live_reasoning_lines(
        "a\n\nb",
        24,
        style::Theme::Dark,
        ReasoningDisplayMode::Message,
    );
    let short: Vec<String> = short
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        })
        .collect();
    assert_eq!(
        short,
        vec![
            "∴ Reasoning …".to_owned(),
            "│ a".to_owned(),
            "│ ".to_owned(),
            "│ b".to_owned(),
        ]
    );
    for width in [0u16, 1, 3, 24] {
        let _ = live_reasoning_lines(
            &text,
            width,
            style::Theme::Dark,
            ReasoningDisplayMode::Message,
        );
    }
}

/// Baut einen Session-Store-Sidecar mit festem `created_at`/`last_opened_at`
/// unter `root` für `session_id` (Aufgabe 2: Export-Datum bei Resume).
fn save_session_meta_with_created_at(
    root: &std::path::Path,
    session_id: &SessionId,
    created_at: jiff::Timestamp,
) -> TestResult {
    std::fs::create_dir_all(root).map_err(ctx("temp session store root"))?;
    let meta = harw_session_store::meta::SessionMeta {
        version: harw_session_store::meta::SESSION_META_VERSION,
        session_id: session_id.clone(),
        title: None,
        title_source: harw_session_store::meta::TitleSource::None,
        created_at,
        last_opened_at: created_at,
        cwd: None,
        project_root: None,
        project_key: None,
        first_user_message: None,
        turns: 0,
        usage_rounds: 0,
        total_usage: harw_types::TokenUsage::default(),
        drift_events: std::collections::BTreeMap::new(),
        archived: false,
        last_origin: None,
        last_activity_at: None,
    };
    harw_session_store::meta::save(root, &meta).map_err(ctx("save session meta sidecar"))?;
    Ok(())
}

/// Eine über [`ChatApp::with_session_store_root`] gesetzte Wurzel wird
/// auch OHNE jeden Titel-Job-Kontext ausgewertet — anders als zuvor, wo nur
/// `title_job_context.session_store_root` (nur bei aktiver
/// Titelerzeugung gesetzt) zur Verfügung stand.
#[test]
fn apply_session_store_started_at_uses_own_field_without_title_job_context() -> TestResult {
    let mut app = test_chat_app()?;
    let root =
        std::env::temp_dir().join(format!("harw-tui-app-test-started-at-{}", app.session_id()));
    let created_at =
        jiff::Timestamp::from_second(1_700_000_000).map_err(ctx("valid timestamp"))?;
    save_session_meta_with_created_at(&root, app.session_id(), created_at)?;

    app = app.with_session_store_root(root.clone());
    apply_session_store_started_at(&mut app);
    std::fs::remove_dir_all(&root).ok();

    assert!(app.title_job_context.is_none());
    assert_eq!(
        app.export_started_at,
        Some(created_at.as_second().to_string())
    );
    Ok(())
}

/// Ohne eigenes `session_store_root` fällt [`apply_session_store_started_at`]
/// weiterhin auf `title_job_context.session_store_root` zurück (Rückwärts-
/// kompatibilität mit dem Verhalten vor Aufgabe 2).
#[test]
fn apply_session_store_started_at_falls_back_to_title_job_context() -> TestResult {
    let mut app = test_chat_app()?;
    let root = std::env::temp_dir().join(format!(
        "harw-tui-app-test-started-at-fallback-{}",
        app.session_id()
    ));
    let created_at =
        jiff::Timestamp::from_second(1_650_000_000).map_err(ctx("valid timestamp"))?;
    save_session_meta_with_created_at(&root, app.session_id(), created_at)?;

    app = app.with_title_job_context(TitleJobContext {
        provider: Arc::new(ScriptedModel::new(Vec::new())),
        session_store_root: root.clone(),
        title_model: None,
        config: Arc::new(harw_config::ResolvedConfig::default()),
    });
    assert!(app.session_store_root.is_none());
    apply_session_store_started_at(&mut app);
    std::fs::remove_dir_all(&root).ok();

    assert_eq!(
        app.export_started_at,
        Some(created_at.as_second().to_string())
    );
    Ok(())
}

/// Ist weder das eigene Feld noch der Titel-Job-Kontext bekannt (kein
/// `/resume` in diesem Lauf konfiguriert), bleibt `export_started_at`
/// unverändert beim TUI-Startzeitpunkt aus `ChatApp::new`.
#[test]
fn apply_session_store_started_at_leaves_tui_start_when_nothing_known() -> TestResult {
    let mut app = test_chat_app()?;
    let original = app.export_started_at.clone();

    apply_session_store_started_at(&mut app);

    assert_eq!(app.export_started_at, original);
    Ok(())
}

// -----------------------------------------------------------------------
// Plan Teil F: `!`-Modus wie in Claude Code — Composer-Moduserkennung,
// Folge-Turn-Nachricht (inkl. Kappung) und Einreihung.
// -----------------------------------------------------------------------

#[test]
fn is_shell_mode_input_true_for_bang_prefix() -> TestResult {
    assert!(is_shell_mode_input("!ls -la"));
    Ok(())
}

#[test]
fn is_shell_mode_input_true_for_double_bang() -> TestResult {
    assert!(is_shell_mode_input("!!"));
    Ok(())
}

#[test]
fn is_shell_mode_input_false_for_plain_text() -> TestResult {
    assert!(!is_shell_mode_input("ls -la"));
    assert!(!is_shell_mode_input(""));
    assert!(!is_shell_mode_input("/status"));
    Ok(())
}

#[test]
fn truncate_chars_with_marker_leaves_short_text_unchanged() -> TestResult {
    assert_eq!(truncate_chars_with_marker("hallo", 8000), "hallo");
    assert_eq!(truncate_chars_with_marker("", 8000), "");
    Ok(())
}

/// Kappung ist zeichengrenzen-sicher: ein 8001 Zeichen langer,
/// mehrbytiger Text (Umlaute) darf nicht mitten in einem UTF-8-Codepunkt
/// getrennt werden — `.chars().take(n)` garantiert das strukturell.
#[test]
fn truncate_chars_with_marker_caps_long_multibyte_text_with_marker() -> TestResult {
    let text: String = "ä".repeat(8001);
    let truncated = truncate_chars_with_marker(&text, 8000);
    assert!(truncated.ends_with("\n[gekürzt]"));
    let body = truncated
        .strip_suffix("\n[gekürzt]")
        .ok_or(TestError::Missing("marker suffix"))?;
    assert_eq!(body.chars().count(), 8000);
    assert!(body.chars().all(|c| c == 'ä'));
    Ok(())
}

#[test]
fn build_shell_turn_message_includes_command_and_exit_code() -> TestResult {
    let message = build_shell_turn_message(&shell_outcome("ls -la", 0, "total 0\n"));
    // Runde 6, Teil B: die Folge-Nachricht nennt „Host“ und cwd.
    assert!(
        message.starts_with(
            "Ich habe `! ls -la` auf dem Host ausgeführt (cwd `/projekt`, Exit 0):"
        ),
        "{message}"
    );
    assert!(message.contains("Host"));
    assert!(message.contains("```text\ntotal 0\n\n```"));
    Ok(())
}

#[test]
fn build_shell_turn_message_truncates_long_output() -> TestResult {
    let output: String = "x".repeat(SHELL_TURN_OUTPUT_MAX_CHARS + 500);
    let message = build_shell_turn_message(&shell_outcome("yes | head", 0, &output));
    assert!(message.contains("[gekürzt]"));
    // Nur die (gekürzte) Ausgabe zählt gegen die Obergrenze, nicht die
    // umgebende Nachricht (Header + Codeblock-Markierungen).
    let fence_body = message
        .split("```text\n")
        .nth(1)
        .and_then(|rest| rest.rsplit_once("\n```"))
        .map(|(body, _)| body)
        .ok_or(TestError::Missing("fenced code block"))?;
    assert_eq!(
        fence_body.chars().count(),
        SHELL_TURN_OUTPUT_MAX_CHARS + "\n[gekürzt]".chars().count()
    );
    Ok(())
}

fn shell_outcome(command: &str, exit_code: i64, combined_output: &str) -> ShellRunOutcome {
    ShellRunOutcome {
        command: command.to_owned(),
        exit_code,
        combined_output: combined_output.to_owned(),
        // Runde 6, Teil B.
        cwd: PathBuf::from("/projekt"),
        note: None,
    }
}

/// Ist gerade kein Turn unterwegs (`submitted == None`, der Normalfall
/// direkt nach einem `!`-Befehl im Idle-Pfad), wird der Folge-Turn sofort
/// zum nächsten zu treibenden Turn — derselbe Pfad wie `HarwEvent::Submit`.
#[test]
fn queue_shell_follow_up_turn_submits_immediately_when_idle() -> TestResult {
    let mut app = test_chat_app()?;
    let mut submitted: Option<String> = None;

    queue_shell_follow_up_turn(
        &mut app,
        &mut submitted,
        shell_outcome("echo hi", 0, "hi\n"),
    );

    let text = submitted.ok_or(TestError::Missing("turn queued immediately"))?;
    assert!(text.starts_with("Ich habe `! echo hi` auf dem Host ausgeführt"));
    assert!(text.contains("Exit 0"));
    assert!(app.pending_turns.is_empty());
    assert_eq!(app.last_shell_command.as_deref(), Some("echo hi"));
    assert_eq!(
        app.pending_turn_user_cell_override.as_deref(),
        Some("↳ Ausgabe von ! echo hi an den Agenten übergeben")
    );
    Ok(())
}

/// Läuft bereits ein Turn (`submitted.is_some()`), wird der Folge-Turn
/// stattdessen an `app.pending_turns` gehängt statt den belegten Platz zu
/// überschreiben.
#[test]
fn queue_shell_follow_up_turn_queues_when_turn_already_submitted() -> TestResult {
    let mut app = test_chat_app()?;
    let mut submitted: Option<String> = Some("bereits abgeschickte Nachricht".to_owned());

    queue_shell_follow_up_turn(&mut app, &mut submitted, shell_outcome("pwd", 1, "err\n"));

    assert_eq!(submitted.as_deref(), Some("bereits abgeschickte Nachricht"));
    assert_eq!(app.pending_turns.len(), 1);
    assert!(
        app.pending_turns
            .front()
            .ok_or(TestError::Missing("queued follow-up turn"))?
            .starts_with("Ich habe `! pwd` auf dem Host ausgeführt (cwd `/projekt`, Exit 1):")
    );
    Ok(())
}

#[test]
fn export_request_marker_reads_format_options_and_path() -> TestResult {
    let request = export_request_from_data(&json!({
        "kind": "export.request",
        "format": "json",
        "include_tool_calls": false,
        "include_reasoning_summary": true,
        "path": "exports/session with spaces.json",
    }))
    .ok_or(TestError::Missing("valid export marker"))?;

    assert_eq!(request.format, ExportOutputFormat::Json);
    assert!(!request.include_tool_calls);
    assert!(request.include_reasoning_summary);
    assert_eq!(
        request.path.as_deref(),
        Some("exports/session with spaces.json")
    );
    // Kein `max_chars`-Schlüssel im Marker → `None`, keine Begrenzung.
    assert_eq!(request.max_chars, None);
    Ok(())
}

/// `max_chars` im Marker (`--max-chars <n>` am `/export`-Command, siehe
/// `harw_ops::export`) wird als positive Zahl übernommen und landet
/// unverändert in `ExportOptions.max_chars`.
#[test]
fn export_request_marker_reads_max_chars() -> TestResult {
    let request = export_request_from_data(&json!({
        "kind": "export.request",
        "format": "markdown",
        "max_chars": 20000,
    }))
    .ok_or(TestError::Missing("valid export marker"))?;

    assert_eq!(request.max_chars, Some(20000));

    let opts = ExportOptions {
        include_tool_calls: request.include_tool_calls,
        include_reasoning: request.include_reasoning_summary,
        max_chars: request.max_chars,
        ..ExportOptions::default()
    };
    assert_eq!(opts.max_chars, Some(20000));
    Ok(())
}

#[test]
fn build_export_markdown_reflects_reasoning_and_tool_options() -> TestResult {
    let mut app = test_chat_app()?;
    app.push_line(Role::User, "Frage");
    app.export_entries
        .push(ExportEntry::Reasoning("sichere Zusammenfassung".to_owned()));
    app.export_entries.push(ExportEntry::Tool {
        label: "shell.exec".to_owned(),
        summary: Some("Ergebnis".to_owned()),
    });

    // Standard: Reasoning ausgeblendet, Werkzeuge eingeblendet.
    let default_markdown = build_export_markdown(&app, &ExportOptions::default());
    assert!(!default_markdown.contains("sichere Zusammenfassung"));
    assert!(default_markdown.contains("shell.exec"));

    // `--reasoning-summary` muss im Markdown tatsächlich wirken.
    let with_reasoning = build_export_markdown(
        &app,
        &ExportOptions {
            include_reasoning: true,
            ..ExportOptions::default()
        },
    );
    assert!(with_reasoning.contains("sichere Zusammenfassung"));

    // `build_export` mit `ExportOutputFormat::Markdown` muss identisch zu
    // `build_export_markdown` sein — beide teilen sich denselben Zweig.
    let via_build_export = build_export(
        &app,
        &ExportOptions {
            include_reasoning: true,
            ..ExportOptions::default()
        },
        ExportOutputFormat::Markdown,
    );
    assert_eq!(with_reasoning, via_build_export);
    Ok(())
}

#[test]
fn structured_export_request_feeds_json_renderer_options() -> TestResult {
    let mut app = test_chat_app()?;
    app.push_line(Role::User, "Frage");
    app.export_entries
        .push(ExportEntry::Reasoning("sichere Zusammenfassung".to_owned()));
    app.export_entries.push(ExportEntry::Tool {
        label: "shell.exec".to_owned(),
        summary: Some("Ergebnis".to_owned()),
    });

    let request = export_request_from_data(&json!({
        "kind": "export.request",
        "format": "json",
        "include_tool_calls": false,
        "include_reasoning_summary": true,
        "path": null,
    }))
    .ok_or(TestError::Missing("valid export marker"))?;
    let opts = ExportOptions {
        include_tool_calls: request.include_tool_calls,
        include_reasoning: request.include_reasoning_summary,
        ..ExportOptions::default()
    };
    let document: Value = serde_json::from_str(&build_export(&app, &opts, request.format))
        .map_err(ctx("valid JSON export"))?;
    let events = document["events"]
        .as_array()
        .ok_or(TestError::Missing("events array"))?;

    assert!(
        events
            .iter()
            .any(|event| event["type"] == "reasoning_summary")
    );
    assert!(!events.iter().any(|event| event["type"] == "tool"));
    Ok(())
}

#[test]
fn file_export_reports_the_actual_collision_suffix() -> TestResult {
    let mut app = test_chat_app()?;
    let path = std::env::temp_dir().join(format!(
        "harw-tui-export-suffix-{}-{}.md",
        std::process::id(),
        SessionId::new()
    ));
    let first =
        export::write_export_path(&path, "first\n").map_err(ctx("create first export"))?;

    resolve_export_request(
        &mut app,
        &ExportRequest {
            format: ExportOutputFormat::Markdown,
            include_tool_calls: true,
            include_reasoning_summary: false,
            path: Some(path.to_string_lossy().into_owned()),
            max_chars: None,
        },
    );

    let message = app
        .export_entries
        .iter()
        .rev()
        .find_map(|entry| match entry {
            ExportEntry::System(text) => Some(text.as_str()),
            _ => None,
        })
        .ok_or(TestError::Missing("export status message"))?;
    let expected_suffix = format!("{}-2.md", path.with_extension("").display());
    assert!(message.contains(&expected_suffix), "message: {message}");

    let second = path.with_file_name(format!(
        "{}-2.md",
        path.file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or(TestError::Missing("stem"))?
    ));
    std::fs::remove_file(first).ok();
    std::fs::remove_file(second).ok();
    Ok(())
}

/// A7-Fallback-Entscheid: schlägt beim reinen `/export`
/// (`resolve_export_choice`-Index `0`, „Zwischenablage") sowohl jedes
/// Systemwerkzeug als auch der OSC-52-Fallback fehl, wird stattdessen
/// eine Datei über [`export::default_export_path`] geschrieben und ihr
/// Pfad gemeldet — statt nur den `NoClipboard`-Fehler anzuzeigen. In der
/// Testumgebung ist ohnehin kein Zwischenablage-Werkzeug installiert;
/// ein absichtlich übergroßer Inhalt (> `OSC52_MAX_BYTES`) lässt
/// zusätzlich den OSC-52-Fallback selbst scheitern, sodass
/// `clipboard::copy_or_sequence` deterministisch `NoClipboard` liefert.
#[test]
fn export_choice_falls_back_to_a_file_when_no_clipboard_is_available() -> TestResult {
    let mut app = test_chat_app()?;
    // Größer als `clipboard::OSC52_MAX_BYTES` (100_000) — macht auch den
    // OSC-52-Fallback selbst unmöglich, nicht nur die Systemwerkzeuge.
    app.push_line(Role::User, "x".repeat(150_000));
    app.pending_export_options = Some(ExportOptions::default());
    app.pending_export_format = ExportOutputFormat::Markdown;
    let (bus, _receiver) = harw_event_channel();

    resolve_export_choice(&mut app, &bus, 0);

    let message = app
        .export_entries
        .iter()
        .rev()
        .find_map(|entry| match entry {
            ExportEntry::System(text) => Some(text.clone()),
            _ => None,
        })
        .ok_or(TestError::Missing("export status message"))?;
    let expected_prefix = "No clipboard — export saved to ";
    assert!(message.starts_with(expected_prefix), "message: {message}");

    let path = std::path::PathBuf::from(
        message
            .strip_prefix(expected_prefix)
            .ok_or(TestError::Missing("prefix checked above"))?,
    );
    assert!(
        path.exists(),
        "the fallback export file must actually be written: {path:?}"
    );
    std::fs::remove_file(&path).ok();
    Ok(())
}

/// Test-lokaler Ersatz für das gelöschte `trusted_tui_spawn_context`
/// (W2d-2/T2b, CONTRACTS-W2d2 §2 T2b): dieselben Feldwerte, die die
/// gelöschte Funktion für den lokalen TUI-Root vergab — ein
/// `ApprovalActor::Operator { id: "local-tui" }`, keine Vorschläge, kein
/// Capability-Snapshot, Organisationsrolle `RootOrchestrator`. Anders als
/// die Montage-Funktion trägt dieser Testwert keinen frischen Trace und
/// keine Kontext-Decke — beide sind für die hier verbliebenen Tests
/// (Freigabefluss, Modus-Wechsel, ausführbare Policy) ohne Bedeutung; ihre
/// jeweilige Erzeugung ist in `harw-runtime/src/trace.rs` und
/// `harw-runtime/src/ceiling.rs` eigenständig getestet.
fn test_spawn_context(sandbox: &SandboxSpec) -> SpawnContext {
    SpawnContext {
        sandbox: sandbox.clone(),
        suggestions: None,
        capability_snapshot: None,
        approval_actor: Some(ApprovalActor::Operator {
            id: "local-tui".to_owned(),
        }),
        organizational_role: AgentRoleId::RootOrchestrator,
        allowed_child_orchestrators: Vec::new(),
        trace: None,
        ceiling: None,
    }
}

#[test]
fn busy_turn_scrolls_immediately_and_preserves_typed_input_in_order() -> TestResult {
    // `queue_busy_key` (siehe Doku dort) wurde bewusst umgebaut: Tastatur-
    // Events werden während eines laufenden Turns nicht mehr roh in
    // `deferred_input` zwischengelagert, sondern live in `app.input`
    // editiert — der Composer bleibt beim Tippen sichtbar aktuell.
    // Fertige Chat-Zeilen landen direkt in `pending_turns`; nur ein
    // fertiges, zurückgestelltes Slash-Kommando wird für die autorisierte
    // Nach-Turn-Ausführung als Paste+Enter in `deferred_input` gelegt
    // (Runde 4, Teil H: `Immediate`/`Staged` meldet
    // `BusyKeyOutcome::Dispatch`, lokale Befehle `Local`, siehe die
    // eigenen Tests dafür unten). Diese Assertions prüfen jetzt genau das, statt die
    // alte Roh-Event-Warteschlange: Scrollen wirkt weiterhin sofort, und
    // Tippen + Einfügen bleiben in der Reihenfolge im Composer erhalten.
    let mut app = test_chat_app()?;
    app.last_history_total_lines.set(100);
    app.last_history_visible_rows.set(10);
    let typed = TuiEvent::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    let pasted = TuiEvent::Paste("next prompt".to_owned());
    assert_eq!(
        handle_busy_event(&mut app, typed.clone()),
        BusyKeyOutcome::Redraw
    );
    assert_eq!(
        handle_busy_event(
            &mut app,
            TuiEvent::Key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))
        ),
        BusyKeyOutcome::Redraw
    );
    assert!(app.scroll.offset() > 0);
    assert_eq!(
        handle_busy_event(&mut app, pasted.clone()),
        BusyKeyOutcome::Redraw
    );
    assert_eq!(app.input.text(), "xnext prompt");
    assert!(app.deferred_input.is_empty());
    Ok(())
}

/// Feldbericht: während eines laufenden Turns verschwand eine von
/// `classify_line` abgelehnte Zeile (hier: unterminiertes Quote)
/// kommentarlos — `queue_busy_key` behandelte `LineAction::System`
/// bisher genauso wie `Quit`/`Ignore`. Jetzt kommt die Zeile in den
/// Composer zurück, und eine Systemzeile erklärt die Ablehnung.
#[test]
fn busy_enter_keeps_rejected_line_and_shows_system_line() -> TestResult {
    let mut app = test_chat_app()?;
    app.input.insert_str("/x 'a");

    let outcome = handle_busy_event(
        &mut app,
        TuiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
    );
    assert_eq!(outcome, BusyKeyOutcome::Redraw);

    assert_eq!(app.input(), "/x 'a");
    assert!(app.pending_turns.is_empty());
    let message = app
        .export_entries
        .last()
        .and_then(|entry| match entry {
            ExportEntry::System(text) => Some(text.as_str()),
            _ => None,
        })
        .ok_or(TestError::Missing("export status message"))?;
    assert!(message.starts_with("Eingabe abgelehnt"), "{message}");
    Ok(())
}

/// `/status` trägt `BusyAvailability::Immediate` — `queue_busy_key`
/// reiht ihn in `busy_jobs` ein und meldet `Dispatch(Immediate)`, statt
/// ihn in `deferred_input` zurückzustellen; der Aufrufer startet ihn als
/// eigenen Task (`settle_busy_outcome`).
#[test]
fn busy_turn_immediate_command_is_dispatched_without_touching_deferred_input() -> TestResult {
    let mut app = test_chat_app()?;
    app.input.insert_str("/status");

    let outcome = queue_busy_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        outcome,
        BusyKeyOutcome::Dispatch(BusyAvailability::Immediate)
    );
    assert_eq!(
        app.busy_jobs.queued().front(),
        Some(&BusyCommand {
            raw: "/status".to_owned(),
            class: BusyAvailability::Immediate,
        })
    );
    assert!(app.input.is_empty());
    assert!(app.deferred_input.is_empty());
    Ok(())
}

/// `/compact` bleibt `DeferredUntilTurnEnd`: Paste+Enter in
/// `deferred_input`, für die autorisierte Ausführung nach Turn-Ende.
#[test]
fn busy_turn_queues_submitted_command_for_authorized_dispatch_after_turn() -> TestResult {
    let mut app = test_chat_app()?;
    app.input.insert_str("/compact");

    assert_eq!(
        queue_busy_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        BusyKeyOutcome::Redraw
    );
    assert!(app.input.is_empty());
    assert!(app.busy_jobs.queued().is_empty());
    assert_eq!(
        app.deferred_input.pop_front(),
        Some(TuiEvent::Paste("/compact".to_owned()))
    );
    assert_eq!(
        app.deferred_input.pop_front(),
        Some(TuiEvent::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))
    );
    assert!(app.deferred_input.is_empty());
    Ok(())
}

/// `/model switch x` und `/mode plan` sind `Staged`: sie laufen sofort,
/// merken ihre Änderung aber nur im Controller vor (gilt ab dem nächsten
/// Turn).
#[test]
fn busy_turn_staged_commands_are_dispatched_as_staged() -> TestResult {
    for raw in ["/model switch x", "/mode plan", "/effort high"] {
        let mut app = test_chat_app()?;
        app.input.insert_str(raw);
        assert_eq!(
            queue_busy_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            BusyKeyOutcome::Dispatch(BusyAvailability::Staged),
            "{raw}"
        );
        assert_eq!(
            app.busy_jobs
                .queued()
                .front()
                .map(|command| command.raw.as_str()),
            Some(raw)
        );
        assert!(app.deferred_input.is_empty(), "{raw}");
    }
    Ok(())
}

/// Ein unbekannter Befehl bleibt sicher eingereiht — `busy_availability_for`
/// liefert dafür `DeferredUntilTurnEnd`, der eigentliche „unbekannter
/// Befehl"-Fehler entsteht erst im späteren Dispatch.
#[test]
fn busy_turn_unknown_command_stays_deferred() -> TestResult {
    let mut app = test_chat_app()?;
    app.input.insert_str("/no-such-command");

    assert_eq!(
        queue_busy_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        BusyKeyOutcome::Redraw
    );
    assert_eq!(
        app.deferred_input.pop_front(),
        Some(TuiEvent::Paste("/no-such-command".to_owned()))
    );
    assert_eq!(
        app.deferred_input.pop_front(),
        Some(TuiEvent::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))
    );
    assert!(app.deferred_input.is_empty());
    Ok(())
}

/// Chattext (kein Slash-Befehl) landet weiterhin direkt in `pending_turns`,
/// unabhängig von `busy_availability_for`.
#[test]
fn busy_turn_chat_text_still_goes_to_pending_turns() -> TestResult {
    let mut app = test_chat_app()?;
    app.input.insert_str("hallo welt");

    assert_eq!(
        queue_busy_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        BusyKeyOutcome::Redraw
    );
    assert_eq!(app.pending_turns.pop_front(), Some("hallo welt".to_owned()));
    assert!(app.deferred_input.is_empty());
    Ok(())
}

/// Welle 4c: ein erster Ctrl+C während eines laufenden Turns bricht ihn
/// weiterhin kooperativ ab und scharft zusätzlich `app.pending_quit`; ein
/// zweiter Druck derselben Taste innerhalb von `QUIT_HINT_WINDOW` setzt
/// `app.hard_quit_requested` — derselbe Doppeldruck-Vertrag wie im
/// Idle-Pfad (`handle_key`/`double_ctrl_d_quits_regardless_of_...`), nur
/// mit zusätzlichem Cancel des laufenden Turns statt eines sofortigen
/// `HarwEvent::Quit`.
#[test]
fn double_ctrl_c_during_busy_quits_like_idle() -> TestResult {
    let mut app = test_chat_app()?;
    let cancel = CancelToken::new();
    app.active_cancel = Some(cancel.clone());
    let ctrl_c = || TuiEvent::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));

    assert_eq!(
        handle_busy_event(&mut app, ctrl_c()),
        BusyKeyOutcome::Redraw
    );
    assert!(
        cancel.is_cancelled(),
        "erster Ctrl+C muss den Turn kooperativ abbrechen"
    );
    assert!(matches!(
        app.pending_quit,
        Some(QuitArm {
            label: "Ctrl+C",
            ..
        })
    ));
    assert!(!app.hard_quit_requested);

    assert_eq!(
        handle_busy_event(&mut app, ctrl_c()),
        BusyKeyOutcome::Redraw
    );
    assert!(
        app.hard_quit_requested,
        "zweiter Ctrl+C-Druck binnen des Fensters muss hart beenden"
    );
    Ok(())
}

/// Runde 4 (Nutzerwunsch): ein Ctrl+C-Abbruch bricht nur den laufenden
/// Turn ab. Bereits abgeschickte, aber noch nicht ausgelieferte Eingaben
/// bleiben in `pending_turns`/`deferred_input` und werden von `run_loop`
/// an der nächsten Turn-Grenze ausgeliefert; die Statuszeile meldet das.
#[test]
fn ctrl_c_during_busy_keeps_queued_input_for_delivery() -> TestResult {
    let mut app = test_chat_app()?;
    let cancel = CancelToken::new();
    app.active_cancel = Some(cancel.clone());
    app.deferred_input.push_back(TuiEvent::Key(KeyEvent::new(
        KeyCode::Char('x'),
        KeyModifiers::NONE,
    )));
    app.pending_turns
        .push_back("noch nicht gesendet".to_owned());
    let ctrl_c = TuiEvent::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));

    assert_eq!(handle_busy_event(&mut app, ctrl_c), BusyKeyOutcome::Redraw);

    assert!(
        cancel.is_cancelled(),
        "Ctrl+C muss den laufenden Turn abbrechen"
    );
    assert_eq!(
        app.pending_turns.front().map(String::as_str),
        Some("noch nicht gesendet"),
        "abgeschickte Nachrichten müssen nach dem Abbruch ausgeliefert werden"
    );
    assert_eq!(app.deferred_input.len(), 1);
    assert!(
        app.queue_kept_at.is_some(),
        "eine nicht-leere Warteschlange muss den Statuszeilen-Hinweis setzen"
    );
    Ok(())
}

/// Runde 4: Esc unterbricht einen laufenden Turn wie Ctrl+C, behält die
/// Warteschlange und den Composer-Text, scharft aber kein Beenden.
#[test]
fn esc_during_busy_interrupts_and_keeps_queue_without_quit_arm() -> TestResult {
    let mut app = test_chat_app()?;
    let cancel = CancelToken::new();
    app.active_cancel = Some(cancel.clone());
    app.pending_turns.push_back("gleich danach".to_owned());
    app.input.insert_str("angefangen");
    let esc = TuiEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

    assert_eq!(handle_busy_event(&mut app, esc), BusyKeyOutcome::Redraw);

    assert!(
        cancel.is_cancelled(),
        "Esc muss den laufenden Turn abbrechen"
    );
    assert_eq!(
        app.pending_turns.front().map(String::as_str),
        Some("gleich danach")
    );
    assert!(app.queue_kept_at.is_some());
    assert_eq!(app.input(), "angefangen", "Composer-Text bleibt stehen");
    assert!(
        app.pending_quit.is_none(),
        "Esc darf nie das Beenden scharfstellen"
    );
    assert!(!app.hard_quit_requested);
    Ok(())
}

/// Leere Warteschlangen dürfen den „Warteschlange wird gesendet“-Hinweis
/// nicht fälschlich scharfstellen — sonst zeigte die Statuszeile bei
/// jedem Ctrl+C einen Hinweis, obwohl nichts verworfen wurde.
#[test]
fn ctrl_c_during_busy_with_empty_queues_does_not_arm_queue_cleared_hint() -> TestResult {
    let mut app = test_chat_app()?;
    app.active_cancel = Some(CancelToken::new());
    assert!(app.deferred_input.is_empty());
    assert!(app.pending_turns.is_empty());
    let ctrl_c = TuiEvent::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));

    assert_eq!(handle_busy_event(&mut app, ctrl_c), BusyKeyOutcome::Redraw);

    assert!(
        app.queue_kept_at.is_none(),
        "ohne eingereihte Eingaben gibt es nichts zu verwerfen"
    );
    Ok(())
}

/// Nach Ablauf von `QUIT_HINT_WINDOW` beendet ein erneuter Ctrl+C-Druck
/// nicht hart — die Scharfstellung ist verfallen und wird stattdessen neu
/// gesetzt, exakt wie die Ablaufprüfung im Idle-Pfad von `run_loop`
/// (Welle 4c, Punkt 5).
#[test]
fn ctrl_c_during_busy_after_window_expiry_does_not_hard_quit() -> TestResult {
    let mut app = test_chat_app()?;
    app.active_cancel = Some(CancelToken::new());
    app.pending_quit = Some(QuitArm {
        label: "Ctrl+C",
        at: Instant::now() - QUIT_HINT_WINDOW - Duration::from_millis(1),
    });
    let ctrl_c = TuiEvent::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));

    assert_eq!(handle_busy_event(&mut app, ctrl_c), BusyKeyOutcome::Redraw);
    assert!(
        !app.hard_quit_requested,
        "ein abgelaufener Hinweis darf keinen harten Abbruch auslösen"
    );
    assert!(matches!(
        app.pending_quit,
        Some(QuitArm {
            label: "Ctrl+C",
            ..
        })
    ));
    Ok(())
}

/// PL-97 C4b: die Statuszeile nennt nach dem ersten Ctrl+C die
/// tatsächliche Wirkung — während eines Turns beendet der zweite Druck
/// alles sofort (Not-Aus), im Leerlauf ohne Jobs nur die App.
#[test]
fn armed_ctrl_c_hint_names_the_hard_kill_only_while_work_runs() -> TestResult {
    let mut app = test_chat_app()?;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 20))
        .map_err(ctx("test terminal"))?;
    let mut screen = |app: &ChatApp, hint: Option<&str>| -> TestResult<String> {
        terminal
            .draw(|frame| render_viewport(frame, app, &Spinner::new(), hint))
            .map_err(ctx("draw"))?;
        let buffer = terminal.backend().buffer();
        Ok((0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n"))
    };

    let idle = screen(&app, Some("Ctrl+C"))?;
    assert!(idle.contains("Ctrl+C again to quit"), "{idle}");
    assert!(!idle.contains("stop everything immediately"), "{idle}");

    app.active_cancel = Some(CancelToken::new());
    let busy = screen(&app, Some("Ctrl+C"))?;
    assert!(
        busy.contains("Ctrl+C again: stop everything immediately"),
        "{busy}"
    );

    // Ctrl+D kennt keinen Not-Aus: sein Hinweis bleibt auch im Turn gleich.
    let ctrl_d = screen(&app, Some("Ctrl+D"))?;
    assert!(ctrl_d.contains("Ctrl+D again to quit"), "{ctrl_d}");
    Ok(())
}

#[test]
fn rendered_input_keeps_cursor_on_wrapped_text_and_long_input_visible() -> TestResult {
    let mut app = test_chat_app()?;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(20, 16))
        .map_err(ctx("test terminal"))?;
    app.input.insert_str("one two three four five");
    app.input.move_left();
    terminal
        .draw(|frame| render_viewport(frame, &app, &Spinner::new(), None))
        .map_err(ctx("draw"))?;
    let position = terminal.get_cursor_position().map_err(ctx("cursor"))?;
    assert_eq!(terminal.backend().buffer()[position].symbol(), "e");

    app.input.clear();
    app.input.insert_str("0\n1\n2\n3\n4\n5\n6\n7\n8\n9\nlast");
    terminal
        .draw(|frame| render_viewport(frame, &app, &Spinner::new(), None))
        .map_err(ctx("draw"))?;
    let position = terminal.get_cursor_position().map_err(ctx("cursor"))?;
    assert_eq!(
        terminal.backend().buffer()[(position.x - 1, position.y)].symbol(),
        "t"
    );
    // `render_viewport` legt die Statuszeile inzwischen ÜBER den Composer
    // statt darunter (siehe Kommentar „History | permanente Statuszeile |
    // Eingabe" dort): der Sicherheitsmodus soll nicht vom Verlauf
    // verdrängt werden können. Dadurch sitzt `input_area` jetzt eine Zeile
    // tiefer als zur Einführung dieses Tests (damals History | Eingabe |
    // Status), und die alte Grenze `< 14` war an die alte Reihenfolge
    // gebunden. Mit 20x16-Testterminal, 11 Composer-Zeilen (geclamped auf
    // 8, `input_height` = 10) und der Statuszeile jetzt bei y=5 liegt
    // `input_area` bei y=6..16; die eigene Bodenkante des Composers (Zeile
    // 15) bleibt die relevante Grenze, kein `status_area` mehr darunter.
    assert!(
        position.y < 15,
        "cursor remains inside input, above its own bottom border"
    );
    Ok(())
}

fn test_executable_agent_ir(
    admitted: &[&str],
    forbidden: &[&str],
) -> TestResult<ExecutableAgentIr> {
    let admitted = admitted
        .iter()
        .map(|name| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let forbidden = forbidden
        .iter()
        .map(|name| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let raw = harw_agent_dsl::parse::parse_toml(&format!(
        r#"
schema = "harwness.agent/v1"
id = "harwness.agent.tui-policy-test@1"
version = "1.0.0"
role = "worker"
specialization = "tui-policy-test"

[tools]
admitted = [{admitted}]
forbidden = [{forbidden}]
"#
    ))
    .map_err(ctx("test executable policy TOML"))?;
    let resolved = harw_agent_dsl::resolved::ResolvedAgentDefinition {
        id: raw.id,
        version: raw.version,
        role: raw.role,
        specialization: raw.specialization,
        name: raw.name,
        description: raw.description,
        authority: harw_agent_dsl::authority::AuthorityCeiling::default(),
        trace: harw_agent_dsl::resolved::ResolutionTrace { steps: Vec::new() },
        config: raw.tables,
        reasoning_effort: raw.reasoning_effort,
    };

    harw_agent_dsl::lower(&resolved).map_err(ctx("lower test executable policy"))
}

/// W2d-2/T2b: `build_tui_agent_session` ist entfallen (Montage lebt jetzt in
/// `harw_runtime::RuntimeAssembly`); die Session wird direkt über
/// `AgentSession::new_with_id(..).with_spawn_context(..).with_turn_event_sink(..)`
/// gebaut (CONTRACTS-W2d2 §2 T2b). Das geprüfte Verhalten
/// (`with_executable_agent_ir` schneidet die Werkzeugfläche) ist unverändert.
#[test]
fn selected_executable_policy_limits_tui_session_tools_and_retains_snapshot() -> TestResult {
    let policy = test_executable_agent_ir(&["stop"], &[])?;
    let snapshot_id = policy.snapshot_id();
    let sandbox = test_sandbox()?;
    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
    let (turn_event_tx, _turn_event_rx) = tokio::sync::mpsc::unbounded_channel();

    let session = AgentSession::new_with_id(
        SessionId::new(),
        AgentRole::Assistant,
        None,
        ExtensionRegistry::builder().build(),
        event_tx,
    )
    .with_spawn_context(test_spawn_context(&sandbox))
    .with_turn_event_sink(turn_event_tx)
    .with_executable_agent_ir(&policy);

    assert!(
        session
            .activation()
            .is_tool_enabled(&harw_extension_api::ToolName::new("stop"))
    );
    assert!(
        !session
            .activation()
            .is_tool_enabled(&harw_extension_api::ToolName::new("shell.exec")),
        "selected executable policy must be deny-by-default"
    );
    assert_eq!(session.executable_snapshot_id(), Some(&snapshot_id));
    Ok(())
}

/// W2d-2/T2b: siehe oben — ohne `with_executable_agent_ir` bleibt die
/// volle Werkzeugfläche sichtbar.
#[test]
fn absent_executable_policy_preserves_full_tui_session_visibility() -> TestResult {
    let sandbox = test_sandbox()?;
    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
    let (turn_event_tx, _turn_event_rx) = tokio::sync::mpsc::unbounded_channel();

    let session = AgentSession::new_with_id(
        SessionId::new(),
        AgentRole::Assistant,
        None,
        ExtensionRegistry::builder().build(),
        event_tx,
    )
    .with_spawn_context(test_spawn_context(&sandbox))
    .with_turn_event_sink(turn_event_tx);

    assert!(
        session
            .activation()
            .is_tool_enabled(&harw_extension_api::ToolName::new("shell.exec"))
    );
    assert!(session.executable_snapshot_id().is_none());
    Ok(())
}

#[test]
fn rate_limit_retry_does_not_append_the_user_message_again() -> TestResult {
    let retry = rate_limit_retry_input();

    assert!(
        retry.user_text.is_none(),
        "core already persisted the failed turn's user message"
    );
    Ok(())
}

fn test_operations() -> Vec<Arc<dyn harw_operations::Operation>> {
    let mut registry = OperationRegistry::new();
    harw_ops::register_all(&mut registry);
    registry.iter().map(Arc::clone).collect()
}

#[test]
fn test_classify_line_ignore_on_empty() -> TestResult {
    // Leere Zeilen beenden nicht mehr — sie werden ignoriert.
    assert_eq!(classify_line(""), LineAction::Ignore);
    assert_eq!(classify_line("   "), LineAction::Ignore);
    Ok(())
}

#[test]
fn test_classify_line_quit_on_slash_quit() -> TestResult {
    assert_eq!(classify_line("/quit"), LineAction::Quit);
    assert_eq!(classify_line("/exit"), LineAction::Quit);
    Ok(())
}

#[test]
fn test_classify_line_chat_passes_text() -> TestResult {
    assert_eq!(
        classify_line("hallo welt"),
        LineAction::Chat("hallo welt".to_owned())
    );
    Ok(())
}

#[test]
fn test_classify_line_command_is_dispatched() -> TestResult {
    // `/command`-Zeilen werden nun zur Ausführung durchgereicht, nicht mehr
    // als „noch nicht unterstützt"-Hinweis abgewiesen.
    assert_eq!(
        classify_line("/status"),
        LineAction::Command("/status".to_owned())
    );
    Ok(())
}

#[test]
fn test_chatapp_push_and_read() -> TestResult {
    let mut app = test_chat_app()?;
    app.push_line(Role::User, "hi");
    // Nach einem push_line muss genau eine Zelle vorhanden sein.
    assert_eq!(app.cells_len(), 1);
    assert!(app.input().is_empty());
    Ok(())
}

/// Prüft, dass nach je einem User- und Assistant-Push zwei Zellen vorhanden sind.
#[test]
fn test_push_user_and_assistant_cells_count() -> TestResult {
    let mut app = test_chat_app()?;
    app.push_line(Role::User, "Frage");
    app.push_line(Role::Assistant, "Antwort");
    assert_eq!(
        app.cells_len(),
        2,
        "Zwei Zellen erwartet nach User + Assistant push"
    );
    // UserHistoryCell hat "> "-Präfix auf der ersten Zeile.
    let user_lines = app.cells[0].display_lines(80, style::Theme::Dark);
    assert!(
        !user_lines.is_empty(),
        "User-Zelle muss mindestens eine Zeile liefern"
    );
    let first_content: String = user_lines[0]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert!(
        first_content.starts_with("> "),
        "User-Zelle muss mit '> ' beginnen, war: {first_content:?}"
    );
    // AssistantHistoryCell liefert den Quelltext umbrochen.
    let asst_lines = app.cells[1].display_lines(80, style::Theme::Dark);
    assert!(
        !asst_lines.is_empty(),
        "Assistant-Zelle muss mindestens eine Zeile liefern"
    );
    Ok(())
}

/// Prüft, dass nach einem System-Push die PlainHistoryCell den Text enthält.
#[test]
fn test_push_system_cell_plain_content() -> TestResult {
    let mut app = test_chat_app()?;
    app.push_line(Role::System, "Willkommen");
    assert_eq!(app.cells_len(), 1);
    assert_eq!(
        app.cells[0].desired_height(200, style::Theme::Dark),
        1,
        "Kurze System-Zeile muss height=1 haben"
    );
    let lines = app.cells[0].display_lines(200, style::Theme::Dark);
    let content: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
    assert_eq!(content, "Willkommen");
    Ok(())
}

/// Popup öffnet sich wenn der Eingabepuffer mit `/` beginnt.
#[test]
fn test_popup_opens_on_slash_prefix() -> TestResult {
    let mut app = test_chat_app()?;
    assert!(app.command_popup.is_none(), "initial kein Popup");

    app.input.insert_char('/');
    app.sync_popup();

    assert!(
        app.command_popup.is_some(),
        "nach '/' muss Popup geöffnet sein"
    );
    assert!(
        !app.command_popup
            .as_ref()
            .ok_or(TestError::Missing("command_popup"))?
            .is_empty(),
        "ungefiltertes Popup darf nicht leer sein"
    );
    Ok(())
}

/// Popup schließt sich wenn der Eingabepuffer das `/`-Präfix verliert.
#[test]
fn test_popup_closes_without_slash_prefix() -> TestResult {
    let mut app = test_chat_app()?;

    app.input.insert_char('/');
    app.sync_popup();
    assert!(app.command_popup.is_some());

    app.input.backspace();
    app.sync_popup();

    assert!(
        app.command_popup.is_none(),
        "nach Entfernen des '/' muss Popup geschlossen sein"
    );
    Ok(())
}

// ────────────────────────────────────────────────────────────────────
// Regressionstests: Bug 1 — Popup verschluckte Enter.
//
// Vorher leitete `handle_key` Enter bei offenem Popup in den
// Popup-Zweig; ein arg-behafteter Command (`/status`) matchte dort
// keinen Command-Namen (`PopupAction::Stay`) und die Zeile wurde nie
// abgeschickt. Fix: Enter wird jetzt immer VOR dem Popup-Zweig
// behandelt.
// ────────────────────────────────────────────────────────────────────

/// Zwei Ctrl+D beenden auch bei nicht leerem Composer und offenem Popup.
#[test]
fn double_ctrl_d_quits_regardless_of_composer_or_popup_state() -> TestResult {
    let mut app = test_chat_app()?;
    // `sync_popup` schließt das Popup, sobald der Query-Teil nach dem
    // `/` ein Leerzeichen enthält (Argument-Eingabe hat begonnen, siehe
    // Doku an `sync_popup`) — "/status mit Entwurf" erfüllt die
    // Vorbedingung deshalb nicht mehr. Ein reiner Kommandoname ohne
    // Leerzeichen hält das Popup dagegen offen und deckt denselben Fall
    // ab (nicht-leerer Composer + offenes Popup); die geprüfte Aussage
    // (doppeltes Ctrl-D beendet immer) bleibt unverändert.
    app.input.insert_str("/status");
    app.sync_popup();
    assert!(app.command_popup.is_some(), "Vorbedingung: Popup ist offen");

    let (bus, mut receiver) = harw_event_channel();
    let ctrl_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);

    assert!(handle_key(&mut app, ctrl_d, &bus));
    assert!(matches!(
        app.pending_quit,
        Some(QuitArm {
            label: "Ctrl+D",
            ..
        })
    ));
    assert!(matches!(
        receiver.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));

    assert!(!handle_key(&mut app, ctrl_d, &bus));
    assert!(matches!(receiver.try_recv(), Ok(HarwEvent::Quit)));
    Ok(())
}

/// Enter bei einem offenen Popup übernimmt den markierten Befehl und
/// sendet den unvollständigen Präfix nicht als unbekannten Command ab.
#[test]
fn test_handle_key_enter_accepts_popup_selection_without_submit() -> TestResult {
    let mut app = test_chat_app()?;
    app.input.clear();
    app.input.insert_str("/co");
    app.sync_popup();
    assert_eq!(
        app.command_popup
            .as_ref()
            .and_then(CommandPopup::selected_name),
        Some("compact"),
        "`/co` muss `/compact` vorauswählen"
    );

    let (bus, mut receiver) = harw_event_channel();
    let redraw = handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &bus,
    );

    assert!(redraw, "Enter muss einen Redraw anfordern");
    assert_eq!(app.input(), "/compact ");
    assert!(
        app.command_popup.is_none(),
        "Auswahl muss das Popup schließen"
    );
    assert!(
        receiver.try_recv().is_err(),
        "Autocomplete darf noch keinen Command absenden"
    );
    Ok(())
}

#[test]
fn busy_submit_is_queued_in_fifo_order() -> TestResult {
    let mut app = test_chat_app()?;
    app.input.insert_str("erste Nachricht");
    assert_eq!(
        queue_busy_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        BusyKeyOutcome::Redraw
    );
    app.input.insert_str("zweite Nachricht");
    assert_eq!(
        queue_busy_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        BusyKeyOutcome::Redraw
    );

    assert_eq!(
        app.pending_turns.pop_front().as_deref(),
        Some("erste Nachricht")
    );
    assert_eq!(
        app.pending_turns.pop_front().as_deref(),
        Some("zweite Nachricht")
    );
    assert!(app.pending_turns.is_empty());
    Ok(())
}

/// Der Composer bleibt beim ersten Escape erhalten und wird beim zweiten
/// unmittelbaren Escape geleert.
#[test]
fn two_escapes_clear_the_input_box() -> TestResult {
    let mut app = test_chat_app()?;
    app.input.insert_str("nicht verlieren beim ersten Escape");
    let (bus, _receiver) = harw_event_channel();
    let escape = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);

    assert!(handle_key(&mut app, escape, &bus));
    assert_eq!(app.input(), "nicht verlieren beim ersten Escape");
    assert!(app.escape_armed);

    assert!(handle_key(&mut app, escape, &bus));
    assert!(app.input().is_empty());
    assert!(!app.escape_armed);
    Ok(())
}

/// `KeyCode::Tab` bei offenem Popup akzeptiert die markierte Auswahl,
/// setzt `app.input` auf `"/<name> "` und sendet dabei NICHTS über den
/// Bus (kein Absenden, nur Autocomplete).
#[test]
fn test_handle_key_tab_accepts_popup_selection_without_submit() -> TestResult {
    let mut app = test_chat_app()?;
    app.input.clear();
    app.input.insert_str("/hel");
    app.sync_popup();
    match app
        .command_popup
        .as_ref()
        .and_then(CommandPopup::selected_name)
    {
        Some(name) => assert_eq!(name, "help", "einzige Übereinstimmung für 'hel'"),
        None => {
            return Err(TestError::Unexpected(
                "Popup muss eine Auswahl für 'hel' markieren".into(),
            ));
        }
    }

    let (bus, mut receiver) = harw_event_channel();
    let key = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);

    let redraw = handle_key(&mut app, key, &bus);

    assert!(redraw, "Tab muss einen Redraw anfordern");
    assert_eq!(
        app.input(),
        "/help ",
        "Tab muss die Auswahl inkl. trailing Space übernehmen"
    );
    assert!(
        app.command_popup.is_none(),
        "Tab muss das Popup nach der Auswahl schließen"
    );
    match receiver.try_recv() {
        Err(_) => {}
        Ok(event) => {
            return Err(TestError::Unexpected(format!(
                "Tab darf kein HarwEvent senden, war: {event:?}"
            )));
        }
    }
    Ok(())
}

/// Regression: `/mo` listet zwei Präfix-Treffer (`/mode`, `/model`) UND
/// einen reinen Teilstring-Treffer (`/memory`, enthält „mo", ist aber
/// kein Präfix-Treffer). Ohne bewegte Markierung darf Tab weder den
/// Teilstring-Treffer noch irgendeinen einzelnen Präfix-Treffer sofort
/// übernehmen, solange mehrere Präfix-Treffer uneindeutig sind —
/// stattdessen wird die Eingabe auf deren längstes gemeinsames Präfix
/// erweitert (`/mode`) und das Popup bleibt offen.
#[test]
fn test_handle_key_tab_extends_query_to_common_prefix_for_ambiguous_matches() -> TestResult {
    let mut app = test_chat_app()?;
    app.input.clear();
    app.input.insert_str("/mo");
    app.sync_popup();

    let (bus, mut receiver) = harw_event_channel();
    let key = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);

    let redraw = handle_key(&mut app, key, &bus);

    assert!(redraw, "Tab muss einen Redraw anfordern");
    assert_eq!(
        app.input(),
        "/mode",
        "Tab muss auf das gemeinsame Präfix von /mode und /model erweitern"
    );
    assert_ne!(
        app.input(),
        "/memory ",
        "Tab darf niemals den reinen Teilstring-Treffer /memory übernehmen"
    );
    assert!(
        app.command_popup.is_some(),
        "Popup muss offen bleiben, solange die Erweiterung mehrdeutig bleibt"
    );
    assert!(
        receiver.try_recv().is_err(),
        "Query-Erweiterung darf keinen Command absenden"
    );
    Ok(())
}

// ────────────────────────────────────────────────────────────────────
// Regressionstests: Bug 2 — Ziffern in Argumenten vom Popup abgefangen.
//
// Vorher fing der Popup-Zweig `KeyCode::Char('1'..='9')` als
// Auswahl-Index ab, auch während der Argument-Eingabe nach einem
// Leerzeichen — z. B. wurde aus `job-42` `job-`. Fix: `sync_popup`
// schließt das Popup, sobald die Eingabe nach `/` Whitespace enthält.
// ────────────────────────────────────────────────────────────────────

/// Nach `/stop job-` (Leerzeichen vorhanden) muss `sync_popup` das
/// Popup schließen — die Eingabe befindet sich in der Argument-Phase.
#[test]
fn test_sync_popup_closes_when_argument_has_whitespace() -> TestResult {
    let mut app = test_chat_app()?;
    app.input.clear();
    app.input.insert_str("/stop job-");

    app.sync_popup();

    assert!(
        app.command_popup.is_none(),
        "Popup muss bei Whitespace nach '/' geschlossen sein"
    );
    Ok(())
}

/// Kontrast: `/mod` (kein Leerzeichen) hält das Popup offen — reine
/// Command-Namen-Eingabe ohne Argument-Phase.
#[test]
fn test_sync_popup_stays_open_without_whitespace() -> TestResult {
    let mut app = test_chat_app()?;
    app.input.clear();
    app.input.insert_str("/mod");

    app.sync_popup();

    assert!(
        app.command_popup.is_some(),
        "Popup muss ohne Whitespace nach '/' offen bleiben"
    );
    Ok(())
}

/// Kern-Regressionstest: Zeichen für Zeichen `/stop job-42` eintippen —
/// die Ziffern `4` und `2` dürfen NICHT vom Popup als Auswahl-Index
/// verschluckt werden, sobald die Argument-Phase (nach dem Leerzeichen)
/// erreicht ist.
#[test]
fn test_handle_key_types_digits_in_argument_not_swallowed() -> TestResult {
    let mut app = test_chat_app()?;
    let (bus, mut receiver) = harw_event_channel();

    for character in "/stop job-42".chars() {
        let key = KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE);
        handle_key(&mut app, key, &bus);
    }

    assert_eq!(
        app.input(),
        "/stop job-42",
        "alle getippten Zeichen inkl. Ziffern müssen in der Eingabe landen"
    );
    // Kein Enter gedrückt — es darf noch nichts abgeschickt worden sein.
    assert!(
        receiver.try_recv().is_err(),
        "ohne Enter darf kein HarwEvent gesendet werden"
    );
    Ok(())
}

#[test]
fn resume_command_becomes_a_runtime_request_only_for_valid_shapes() -> TestResult {
    assert_eq!(
        resume_request("/resume"),
        Some(TuiRunOutcome::Resume { selector: None })
    );
    assert_eq!(
        resume_request("  /resume session-prefix  "),
        Some(TuiRunOutcome::Resume {
            selector: Some("session-prefix".to_owned()),
        }),
    );
    assert_eq!(resume_request("/resume too many"), None);
    assert_eq!(resume_request("/resume-other"), None);
    assert_eq!(resume_request("/new"), Some(TuiRunOutcome::NewSession));
    assert_eq!(resume_request("/news"), None);
    Ok(())
}

#[test]
fn durable_history_hydrates_core_and_redacts_non_text_visible_content() -> TestResult {
    use harw_protocol::items::{
        AssistantMessageItem, ErrorItem, ReasoningItem, ResultTrust, ToolCallItem,
        ToolCallResult, ToolResultItem, UserMessageItem,
    };
    use harw_types::{ItemId, ToolCallId};

    let call_id = ToolCallId::new();
    let mut history = ConversationHistory::new();
    history.push(TurnItem::UserMessage(UserMessageItem {
        id: ItemId::new(),
        content: vec![
            ContentPart::Text {
                text: "look ".to_owned(),
            },
            ContentPart::ImageUrl {
                url: "https://secret.example/token".to_owned(),
                detail: None,
            },
        ],
    }));
    history.push(TurnItem::AssistantMessage(AssistantMessageItem {
        id: ItemId::new(),
        content: vec![ContentPart::Text {
            text: "persisted answer".to_owned(),
        }],
        phase: None,
    }));
    history.push(TurnItem::ToolCall(ToolCallItem {
        id: ItemId::new(),
        call_id: call_id.clone(),
        tool_name: "fs.read".to_owned(),
        arguments: json!({ "path": "/tmp/visible.txt" }),
    }));
    history.push(TurnItem::ToolResult(ToolResultItem {
        id: ItemId::new(),
        call_id,
        result: ToolCallResult::error("not found"),
        duration_ms: 3,
        trust: ResultTrust::Untrusted,
    }));
    history.push(TurnItem::Reasoning(ReasoningItem {
        id: ItemId::new(),
        summary_text: vec!["private reasoning".to_owned()],
        raw_content: vec!["raw secret".to_owned()],
    }));
    history.push(TurnItem::Error(ErrorItem {
        id: ItemId::new(),
        message: "sensitive backend detail".to_owned(),
        retryable: false,
    }));

    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut session = AgentSession::new_with_id(
        SessionId::from_str("resumed-session"),
        AgentRole::Assistant,
        None,
        ExtensionRegistry::builder().build(),
        event_tx,
    );
    let mut app = test_chat_app()?;
    install_loaded_history(&mut session, &mut app, history);

    assert_eq!(session.history().len(), 6);
    let visible = app
        .cells
        .iter()
        .flat_map(|cell| cell.display_lines(200, style::Theme::Dark))
        .flat_map(|line| line.spans)
        .map(|span| span.content.into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(visible.contains("look [non-text content]"));
    assert!(visible.contains("persisted answer"));
    assert!(visible.contains("1 file read"));
    assert!(visible.contains("1 failed"));
    assert!(visible.contains("private reasoning"));
    assert!(visible.contains("sensitive backend detail"));
    for secret in ["secret.example", "raw secret"] {
        assert!(!visible.contains(secret));
    }
    Ok(())
}

#[test]
fn resume_hydration_pairs_orphan_result_and_marks_open_call() -> TestResult {
    use harw_protocol::items::{ResultTrust, ToolCallItem, ToolCallResult, ToolResultItem};
    use harw_types::{ItemId, ToolCallId};

    let orphan_id = ToolCallId::new();
    let open_id = ToolCallId::new();
    let mut history = ConversationHistory::new();
    history.push(TurnItem::ToolResult(ToolResultItem {
        id: ItemId::new(),
        call_id: orphan_id,
        result: ToolCallResult::error("orphan result"),
        duration_ms: 9,
        trust: ResultTrust::Runtime,
    }));
    history.push(TurnItem::ToolCall(ToolCallItem {
        id: ItemId::new(),
        call_id: open_id,
        tool_name: "shell.exec".to_owned(),
        arguments: json!({ "command": "echo pending" }),
    }));

    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut session = AgentSession::new_with_id(
        SessionId::from_str("resume-tool-pairs"),
        AgentRole::Assistant,
        None,
        ExtensionRegistry::builder().build(),
        event_tx,
    );
    let mut app = test_chat_app()?;
    // Test prüft die kompakte Darstellung; Default ist seit B8 `Activity`.
    app.tool_verbosity = ToolVerbosity::Compact;
    install_loaded_history(&mut session, &mut app, history);

    let visible = app
        .cells
        .iter()
        .flat_map(|cell| cell.display_lines(200, style::Theme::Dark))
        .flat_map(|line| line.spans)
        .map(|span| span.content.into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(visible.contains("tool.result"));
    assert!(visible.contains("orphan result"));
    assert!(visible.contains("incomplete (resume interrupted)"));
    let tool_entries = app
        .export_entries
        .iter()
        .filter(|entry| {
            matches!(
                entry,
                ExportEntry::ToolCall { .. } | ExportEntry::ToolResult { .. }
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(tool_entries.len(), 4);
    assert!(matches!(
        tool_entries[0],
        ExportEntry::ToolCall { tool_name, .. } if tool_name == "tool.result"
    ));
    assert!(matches!(tool_entries[1], ExportEntry::ToolResult { .. }));
    assert!(matches!(
        tool_entries[2],
        ExportEntry::ToolCall { tool_name, .. } if tool_name == "shell.exec"
    ));
    assert!(matches!(
        tool_entries[3],
        ExportEntry::ToolResult { error: Some(error), .. }
            if error == "unvollständig (Resume-Abbruch)"
    ));
    Ok(())
}

/// B8: the production default comes from `with_verbose_tools`, not
/// `ChatApp::new`; `/verbose` toggles Activity <-> Verbose.
#[test]
fn with_verbose_tools_sets_activity_default_and_verbose() -> TestResult {
    let app = test_chat_app()?.with_verbose_tools(false);
    assert_eq!(app.tool_verbosity, ToolVerbosity::Activity);
    let app = test_chat_app()?.with_verbose_tools(true);
    assert_eq!(app.tool_verbosity, ToolVerbosity::Verbose);
    Ok(())
}

/// B8: in the default Activity view a resumed orphan error result and an
/// interrupted open call are still visibly marked as failed.
#[test]
fn resume_hydration_marks_failures_in_default_activity_view() -> TestResult {
    use harw_protocol::items::{ResultTrust, ToolCallItem, ToolCallResult, ToolResultItem};
    use harw_types::{ItemId, ToolCallId};

    let mut history = ConversationHistory::new();
    history.push(TurnItem::ToolResult(ToolResultItem {
        id: ItemId::new(),
        call_id: ToolCallId::new(),
        result: ToolCallResult::error("orphan result"),
        duration_ms: 9,
        trust: ResultTrust::Runtime,
    }));
    history.push(TurnItem::ToolCall(ToolCallItem {
        id: ItemId::new(),
        call_id: ToolCallId::new(),
        tool_name: "shell.exec".to_owned(),
        arguments: json!({ "command": "echo pending" }),
    }));
    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut session = AgentSession::new_with_id(
        SessionId::from_str("resume-tool-pairs-activity"),
        AgentRole::Assistant,
        None,
        ExtensionRegistry::builder().build(),
        event_tx,
    );
    let mut app = test_chat_app()?.with_verbose_tools(false);
    install_loaded_history(&mut session, &mut app, history);
    let lines = app
        .cells
        .iter()
        .flat_map(|cell| cell.display_lines(200, style::Theme::Dark))
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    let failed = lines.iter().filter(|line| line.starts_with("✗ ")).count();
    assert_eq!(failed, 2, "war: {lines:?}");
    Ok(())
}

#[test]
fn incident_hint_is_small_and_only_suggests_bug_report_on_known_signals() -> TestResult {
    assert!(
        incident_hint(&TuiError::Core("provider timeout".to_owned()), 2)
            .is_some_and(|hint| hint.contains("/bug-report"))
    );
    assert!(
        incident_hint(&TuiError::Core("agent killed".to_owned()), 0)
            .is_some_and(|hint| hint.contains("/bug-report"))
    );
    assert!(
        incident_hint(&TuiError::Core("ordinary validation error".to_owned()), 9).is_none()
    );
    Ok(())
}

// ────────────────────────────────────────────────────────────────────
// AP W5-03 — Freigabe-Treiber (P0). Alle Tests laufen headless über den
// Fragekanal; kein Terminal, kein Netzwerk.
// ────────────────────────────────────────────────────────────────────

/// Name des Werkzeugs, das die Freigabetests anhalten lassen.
const APPROVAL_TEST_TOOL: &str = "fs.write";

/// Modell, das eine vorher festgelegte Folge von Antworten liefert.
struct ScriptedModel {
    responses: Mutex<VecDeque<ModelResponse>>,
}

impl ScriptedModel {
    fn new(responses: Vec<ModelResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
        }
    }

    fn tool_response(name: &str, arguments: Value) -> ModelResponse {
        ModelResponse {
            message: None,
            tool_calls: vec![ToolCall {
                id: ToolCallId::new(),
                name: ToolName::new(name),
                arguments,
            }],
            usage: TokenUsage::default(),
            // `reasoning`/`stop` sind für die Freigabetests irrelevant —
            // Default liefert `None` bzw. `StopReason::EndTurn`.
            ..Default::default()
        }
    }
}

impl ModelProvider for ScriptedModel {
    fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
        let next = match self.responses.lock() {
            Ok(mut responses) => responses.pop_front(),
            Err(_) => None,
        };
        Box::pin(async move {
            Ok(next.unwrap_or_else(|| ModelResponse::text("scripted model exhausted")))
        })
    }
}

/// Werkzeug, das ausschließlich zählt, wie oft es ausgeführt wurde.
struct CountingTool {
    executions: Arc<AtomicUsize>,
}

impl ToolExecutor for CountingTool {
    fn execute<'a>(
        &'a self,
        _context: &'a ToolExecutionContext,
        _call: &'a ToolCall,
    ) -> ToolExecutorFuture<'a> {
        let executions = Arc::clone(&self.executions);
        Box::pin(async move {
            executions.fetch_add(1, Ordering::SeqCst);
            Ok(ToolOutput::text("written"))
        })
    }
}

struct CountingToolProvider {
    executions: Arc<AtomicUsize>,
}

impl ToolProvider for CountingToolProvider {
    fn tools(&self) -> Vec<ToolSpec> {
        vec![ToolSpec::Function(FunctionToolSpec {
            name: ToolName::new(APPROVAL_TEST_TOOL),
            description: "test-only write tool".to_owned(),
            parameters: JsonSchema::default(),
            strict: false,
        })]
    }

    fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
        if name.as_str() == APPROVAL_TEST_TOOL {
            Some(Arc::new(CountingTool {
                executions: Arc::clone(&self.executions),
            }))
        } else {
            None
        }
    }
}

/// Baut eine Session mit derselben Freigabekette wie die Laufzeit-Montage:
/// `DefaultApprovalPolicy` entscheidet zuerst, der TUI-Handler hängt direkt
/// dahinter (W2d-2/T2b — Test-lokaler Ersatz für das gelöschte
/// `registry_with_approval_handler`: beide Handler werden in einem
/// Builder-Aufruf in genau dieser Reihenfolge registriert, statt eine
/// Basis-Registry nachträglich um den TUI-Handler zu erweitern). Der
/// Spawn-Kontext kommt aus [`test_spawn_context`] und trägt damit den
/// Approval-Actor.
fn approval_test_session(
    handler: &Arc<TuiApprovalHandler>,
    executions: &Arc<AtomicUsize>,
    sandbox: &SandboxSpec,
) -> AgentSession {
    let registered = as_dyn_approval_handler(handler);
    let registry = ExtensionRegistry::builder()
        .tool_provider(Arc::new(CountingToolProvider {
            executions: Arc::clone(executions),
        }))
        .approval_handler(Arc::new(
            harw_registry_defaults::DefaultApprovalPolicy::new(ApprovalModeCell::default()),
        ))
        .approval_handler(registered)
        .build();
    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
    AgentSession::new(AgentRole::Assistant, None, registry, event_tx)
        .with_spawn_context(test_spawn_context(sandbox))
}

/// Ergebnis eines headless getriebenen Freigabeturns.
struct DrivenTurn {
    outcome: Result<TurnOutcome, String>,
    executions: usize,
    prompts_seen: usize,
}

/// Treibt einen Turn, der an `fs.write` pausiert, und beantwortet jede
/// eintreffende Frage mit `approve`.
async fn drive_turn_answering(approve: bool) -> TestResult<DrivenTurn> {
    let executions = Arc::new(AtomicUsize::new(0));
    let (handler, mut prompts) = TuiApprovalHandler::new();
    let driver = ApprovalDriver::new(Arc::clone(&handler));
    let sandbox = test_sandbox()?;
    let mut session = approval_test_session(&handler, &executions, &sandbox);
    let model = ScriptedModel::new(vec![
        ScriptedModel::tool_response(APPROVAL_TEST_TOOL, json!({ "path": "note.txt" })),
        ModelResponse::text("fertig"),
    ]);
    let store = InMemoryStateStore::new();

    let first = run_turn(
        &mut session,
        &model,
        &store,
        TurnInput::user("schreib etwas"),
    )
    .await;
    let outcome = match first {
        Ok(outcome) => outcome,
        Err(error) => {
            return Ok(DrivenTurn {
                outcome: Err(error.to_string()),
                executions: executions.load(Ordering::SeqCst),
                prompts_seen: 0,
            });
        }
    };
    assert!(
        matches!(outcome, TurnOutcome::AwaitingApproval { .. }),
        "the default policy must pause an unapproved write"
    );

    let mut prompts_seen = 0usize;
    let drive = driver.drive_to_completion(&mut session, &model, &store, None, outcome);
    tokio::pin!(drive);
    let result = loop {
        tokio::select! {
            result = &mut drive => break result,
            maybe_prompt = prompts.recv() => {
                if let Some(prompt) = maybe_prompt {
                    prompts_seen += 1;
                    if approve {
                        prompt.approve();
                    } else {
                        prompt.reject("operator rejected the tool call in the test");
                    }
                }
            }
        }
    };

    Ok(DrivenTurn {
        outcome: result.map_err(|error| tui_error_from_approval_driver(error).to_string()),
        executions: executions.load(Ordering::SeqCst),
        prompts_seen,
    })
}

/// Bedingung 1 + 5: der Spawn-Kontext trägt einen Approval-Actor, und eine
/// freigegebene Pause endet in `Completed` — das Werkzeug lief genau einmal.
#[tokio::test]
async fn approved_pause_completes_the_turn_and_runs_the_tool() -> TestResult {
    let driven = drive_turn_answering(true).await?;

    match driven.outcome {
        Ok(TurnOutcome::Completed) => {}
        Ok(other) => {
            return Err(TestError::Unexpected(format!(
                "an approved pause must complete the turn, was: {other:?}"
            )));
        }
        Err(error) => {
            return Err(TestError::Unexpected(format!(
                "an approved pause must not fail the turn: {error}"
            )));
        }
    }
    assert_eq!(driven.prompts_seen, 1, "genau eine Frage erwartet");
    assert_eq!(
        driven.executions, 1,
        "eine Freigabe muss das Werkzeug genau einmal ausführen"
    );
    Ok(())
}

/// Ablehnung: der Turn endet trotzdem sauber, das Werkzeug läuft **nicht**.
#[tokio::test]
async fn rejected_pause_completes_the_turn_without_running_the_tool() -> TestResult {
    let driven = drive_turn_answering(false).await?;

    match driven.outcome {
        Ok(TurnOutcome::Completed) => {}
        Ok(other) => {
            return Err(TestError::Unexpected(format!(
                "a rejected pause must still complete the turn, was: {other:?}"
            )));
        }
        Err(error) => {
            return Err(TestError::Unexpected(format!(
                "a rejected pause must not fail the turn: {error}"
            )));
        }
    }
    assert_eq!(driven.prompts_seen, 1, "genau eine Frage erwartet");
    assert_eq!(
        driven.executions, 0,
        "eine Ablehnung darf das Werkzeug nicht ausführen"
    );
    Ok(())
}

/// Bedingung 3: nur ein ausdrückliches `y` gibt frei.
#[test]
fn only_an_explicit_y_approves_a_pending_prompt() -> TestResult {
    let approve = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
    let reject = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE);
    let escape = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
    let cancel = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    let other = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);

    assert_eq!(classify_approval_key(approve), ApprovalKeyAction::Approve);
    assert_eq!(
        classify_approval_key(reject),
        ApprovalKeyAction::Reject(REASON_OPERATOR_REJECTED)
    );
    assert_eq!(
        classify_approval_key(escape),
        ApprovalKeyAction::Reject(REASON_OPERATOR_CANCELLED)
    );
    assert_eq!(
        classify_approval_key(cancel),
        ApprovalKeyAction::Reject(REASON_OPERATOR_CANCELLED)
    );
    assert_eq!(classify_approval_key(other), ApprovalKeyAction::Ignore);
    Ok(())
}

/// Die Antwort landet beim wartenden Treiber **und** an derselben
/// Werkzeugzelle.
///
/// Seit der Umstellung auf das Freigabe-Panel (`ApprovalDialog`) trägt
/// nicht mehr eine eigene `ApprovalPromptCell` die Entscheidung, sondern
/// dieselbe [`SharedToolCell`], die auch `TurnEvent::ToolCallRequested`
/// über `ensure_tool_cell` anlegt (siehe [`PendingApprovalPrompt`]). Die
/// geprüfte Eigenschaft bleibt dieselbe: die Freigabe erreicht den
/// Treiber, und dieselbe Zelle trägt danach das Ergebnis.
#[tokio::test]
async fn answering_a_prompt_updates_the_same_cell() -> TestResult {
    let (handler, mut prompts) = TuiApprovalHandler::new();
    let request = ItemId::new();
    let call = ToolCall {
        id: ToolCallId::new(),
        name: ToolName::new(APPROVAL_TEST_TOOL),
        arguments: json!({ "path": "note.txt" }),
    };
    assert!(handler.open_prompt(&request, &call));

    let prompt = match prompts.try_recv() {
        Ok(prompt) => prompt,
        Err(error) => {
            return Err(TestError::Unexpected(format!(
                "die Frage muss den Renderer erreichen: {error}"
            )));
        }
    };
    let tool_cell: SharedToolCell = Arc::new(Mutex::new(ToolCell::started(&call)));
    let mut app = ChatApp::new(Vec::new(), test_sandbox()?, SessionId::new());

    apply_approval_decision(
        &mut app,
        PendingApprovalPrompt {
            prompt,
            tool_cell: Arc::clone(&tool_cell),
        },
        ApprovalChoice::Approve,
    )
    .await;

    match handler.await_resolution(&request).await {
        ApprovalResolution::Approve => {}
        other => {
            return Err(TestError::Unexpected(format!(
                "eine Freigabe muss den Treiber erreichen, war: {other:?}"
            )));
        }
    }
    match tool_cell.lock() {
        Ok(cell) => assert_eq!(cell.approval_note.as_deref(), Some("✓ freigegeben")),
        Err(_) => {
            return Err(TestError::Unexpected(
                "die Zelle muss nach der Antwort lesbar bleiben".into(),
            ));
        }
    }
    Ok(())
}

/// Eine Ablehnung schreibt dieselbe Werkzeugzelle mit der Ablehnungsnotiz
/// fort — die Nachfolgerin des früheren `Some(false)` an der
/// `ApprovalPromptCell` (siehe Kommentar oben).
#[tokio::test]
async fn rejecting_a_prompt_marks_the_same_cell_as_denied() -> TestResult {
    let (handler, mut prompts) = TuiApprovalHandler::new();
    let request = ItemId::new();
    let call = ToolCall {
        id: ToolCallId::new(),
        name: ToolName::new(APPROVAL_TEST_TOOL),
        arguments: json!({ "path": "note.txt" }),
    };
    assert!(handler.open_prompt(&request, &call));
    let prompt = match prompts.try_recv() {
        Ok(prompt) => prompt,
        Err(error) => {
            return Err(TestError::Unexpected(format!(
                "die Frage muss den Renderer erreichen: {error}"
            )));
        }
    };
    let tool_cell: SharedToolCell = Arc::new(Mutex::new(ToolCell::started(&call)));
    let mut app = ChatApp::new(Vec::new(), test_sandbox()?, SessionId::new());

    apply_approval_decision(
        &mut app,
        PendingApprovalPrompt {
            prompt,
            tool_cell: Arc::clone(&tool_cell),
        },
        ApprovalChoice::Reject {
            reason: Some(REASON_OPERATOR_REJECTED.to_owned()),
        },
    )
    .await;

    match handler.await_resolution(&request).await {
        ApprovalResolution::Reject { reason } => {
            assert_eq!(reason, REASON_OPERATOR_REJECTED);
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "eine Ablehnung muss den Treiber erreichen, war: {other:?}"
            )));
        }
    }
    match tool_cell.lock() {
        Ok(cell) => assert_eq!(cell.approval_note.as_deref(), Some("✗ abgelehnt")),
        Err(_) => {
            return Err(TestError::Unexpected(
                "die Zelle muss nach der Antwort lesbar bleiben".into(),
            ));
        }
    }
    Ok(())
}

/// Fix E (Teil 1b): drückt der Nutzer Ctrl+C, während
/// `drive_pauses_to_completion` eine Freigabefrage anzeigt, bricht das
/// jetzt zusätzlich zum bestehenden Ablehnen des Dialogs auch den
/// laufenden Turn kooperativ ab und scharft den zweistufigen
/// Beenden-Hinweis. Vor diesem Fix blieben `active_cancel`/`pending_quit`
/// unberührt — nur der Dialog wurde abgelehnt, der Turn und alle
/// laufenden Kind-Agenten liefen weiter. Getestet direkt an der
/// extrahierten Kernlogik [`cancel_turn_and_reject_open_dialogs`], ohne
/// den vollen `drive_pauses_to_completion`-Ereignis-Loop aufzuziehen.
#[tokio::test]
async fn ctrl_c_with_open_approval_dialog_cancels_turn_and_still_rejects_dialog() -> TestResult
{
    let (handler, mut prompts) = TuiApprovalHandler::new();
    let request = ItemId::new();
    let call = ToolCall {
        id: ToolCallId::new(),
        name: ToolName::new(APPROVAL_TEST_TOOL),
        arguments: json!({ "path": "note.txt" }),
    };
    assert!(handler.open_prompt(&request, &call));
    let prompt = match prompts.try_recv() {
        Ok(prompt) => prompt,
        Err(error) => {
            return Err(TestError::Unexpected(format!(
                "die Frage muss den Renderer erreichen: {error}"
            )));
        }
    };
    let tool_cell: SharedToolCell = Arc::new(Mutex::new(ToolCell::started(&call)));
    let mut app = ChatApp::new(Vec::new(), test_sandbox()?, SessionId::new());
    let cancel = CancelToken::new();
    app.active_cancel = Some(cancel.clone());
    app.pending_turns
        .push_back("noch nicht gesendet".to_owned());
    let mut pending = Some(PendingApprovalPrompt {
        prompt,
        tool_cell: Arc::clone(&tool_cell),
    });
    let mut dialog_shown_at = Some(Instant::now());
    let mut host_permit_shown_at: Option<Instant> = None;

    cancel_turn_and_reject_open_dialogs(
        &mut app,
        &mut pending,
        &mut dialog_shown_at,
        &mut host_permit_shown_at,
    )
    .await;

    assert!(
        cancel.is_cancelled(),
        "Ctrl+C muss den Turn auch bei offenem Freigabe-Dialog kooperativ abbrechen"
    );
    assert!(
        matches!(
            app.pending_quit,
            Some(QuitArm {
                label: "Ctrl+C",
                ..
            })
        ),
        "Ctrl+C muss den zweistufigen Beenden-Hinweis scharfstellen"
    );
    assert_eq!(
        app.pending_turns.front().map(String::as_str),
        Some("noch nicht gesendet"),
        "abgeschickte Nachrichten bleiben für die Auslieferung nach dem Abbruch"
    );
    assert!(app.queue_kept_at.is_some());
    assert!(pending.is_none(), "die Frage muss konsumiert sein");
    assert!(dialog_shown_at.is_none());
    assert!(app.pending_approval_dialog.is_none());

    // Bestehendes Ablehnen bleibt zusätzlich bestehen, wird nicht ersetzt.
    match handler.await_resolution(&request).await {
        ApprovalResolution::Reject { reason } => {
            assert_eq!(reason, REASON_OPERATOR_CANCELLED);
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "Ctrl+C muss den Dialog weiterhin ablehnen, war: {other:?}"
            )));
        }
    }
    match tool_cell.lock() {
        Ok(cell) => assert_eq!(cell.approval_note.as_deref(), Some("✗ abgelehnt")),
        Err(_) => {
            return Err(TestError::Unexpected(
                "die Zelle muss nach der Antwort lesbar bleiben".into(),
            ));
        }
    }
    Ok(())
}

/// Bedingung 7: die Kern-Ursache bleibt in der Fehlermeldung erhalten.
#[test]
fn approval_driver_errors_keep_their_cause() -> TestResult {
    let error = ApprovalDriverError::MissingPendingApproval {
        session: "session-42".to_owned(),
    };
    let expected = error.to_string();

    match tui_error_from_approval_driver(error) {
        TuiError::Core(message) => {
            assert!(message.contains("session-42"));
            assert!(message.contains(&expected));
        }
        TuiError::Io(_) => {
            return Err(TestError::Unexpected(
                "ein Treiberfehler ist kein Terminal-I/O-Fehler".into(),
            ));
        }
        TuiError::ContextProviderRegistration(_) => {
            return Err(TestError::Unexpected(
                "ein Treiberfehler ist kein Namensraum-Konflikt".into(),
            ));
        }
    }
    Ok(())
}

/// TUI-Berechtigungen (E4: lokaler Principal-Tier `Operator`, siehe
/// CONTRACTS-W2d2 §4). W2d-2/T2b: `LOCAL_TUI_OPERATION_PERMISSION` ist mit
/// der Montage entfallen — die Produktionsfläche löst die Stufe jetzt über
/// `runtime_commands::caller_tier(rt.principal())` auf, deren Ergebnis für
/// den lokalen TUI-Principal laut E4 `PermissionTier::Operator` ist; dieser
/// Test benutzt denselben Wert direkt. `CommandServices` ist durch die
/// Closure `F: FnOnce() -> ServiceMap` ersetzt (CE, CONTRACTS-W2d2 §1.2).
#[tokio::test]
async fn local_tui_permissions_are_accessible_but_maintainer_commands_are_blocked() -> TestResult
{
    let sandbox = test_sandbox()?;
    let operations = test_operations();
    let adapters = operations
        .iter()
        .flat_map(|operation| CommandAdapter::from_operation(Arc::clone(operation)))
        .collect::<Vec<_>>();
    let controller = Arc::new(TuiSessionController::new());

    // `build_services` (command_exec.rs) spiegelt die Produktionsfläche
    // `RuntimeServices::service_map` bewusst nicht 1:1 — es fehlt dort die
    // `ApprovalModeCell` (siehe `harw-runtime/src/services.rs::assemble`,
    // wo `self.parts.approval_mode.clone()` unbedingt auf jeder Fläche
    // eingetragen wird). `/permissions` liest/schreibt den Freigabemodus
    // ausschließlich über `ctx.service::<ApprovalModeCell>()`
    // (`harw-ops/src/permissions.rs`), also braucht dieser Test dieselbe
    // Zelle wie die echte Laufzeit — hier direkt nach `build_services`
    // nachgetragen, ohne `command_exec.rs` selbst zu ändern.
    let permissions = execute_command_as(
        &adapters,
        &sandbox,
        &SessionId::new(),
        harw_operations::PermissionTier::Operator,
        "/permissions",
        || {
            let mut services = build_services(&adapters, None, None, &controller, None, None);
            services.insert(ApprovalModeCell::default());
            services
        },
    )
    .await;
    assert!(permissions.contains("Freigabemodus"), "{permissions}");
    assert!(permissions.contains("ask|auto|full"), "{permissions}");

    let output = execute_command_as(
        &adapters,
        &sandbox,
        &SessionId::new(),
        harw_operations::PermissionTier::Operator,
        "/plugins",
        || build_services(&adapters, None, None, &controller, None, None),
    )
    .await;

    assert_eq!(
        output,
        "Berechtigung verweigert: /plugins erfordert Maintainer; aktuelle Stufe ist Operator"
    );
    Ok(())
}

/// `/mode` an der Turn-Grenze (AP W5-05). W2d-2/T2b: die Session wird über
/// `AgentSession::new_with_id(..).with_spawn_context(..).with_turn_event_sink(..)`
/// gebaut statt über das gelöschte `build_tui_agent_session`.
#[test]
fn mode_request_is_applied_at_the_turn_boundary() -> TestResult {
    let sandbox = test_sandbox()?;
    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
    let (turn_event_tx, _turn_event_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut session = AgentSession::new_with_id(
        SessionId::new(),
        AgentRole::Assistant,
        None,
        ExtensionRegistry::builder().build(),
        event_tx,
    )
    .with_spawn_context(test_spawn_context(&sandbox))
    .with_turn_event_sink(turn_event_tx);
    let controller = Arc::new(TuiSessionController::new());
    let mut app = ChatApp::new(Vec::new(), sandbox, SessionId::new())
        .with_session_controller(Arc::clone(&controller));

    assert_eq!(session.mode(), InteractionMode::Chat);
    if let Err(error) = SessionController::request_mode(controller.as_ref(), "explore") {
        return Err(TestError::Unexpected(format!(
            "`/mode explore` muss angenommen werden: {error}"
        )));
    }
    assert_eq!(
        session.mode(),
        InteractionMode::Chat,
        "ein laufender Turn darf seinen Modus nicht unter sich wechseln"
    );

    assert!(app.apply_pending_controller_state(&mut session));

    assert_eq!(session.mode(), InteractionMode::Explore);
    assert_eq!(app.active_mode(), InteractionMode::Explore);
    Ok(())
}

/// Ein unbekannter Modusname wird abgewiesen statt still auf den Default zu
/// fallen; die Session bleibt unangetastet.
#[test]
fn unknown_mode_names_never_change_the_session() -> TestResult {
    let sandbox = test_sandbox()?;
    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
    let (turn_event_tx, _turn_event_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut session = AgentSession::new_with_id(
        SessionId::new(),
        AgentRole::Assistant,
        None,
        ExtensionRegistry::builder().build(),
        event_tx,
    )
    .with_spawn_context(test_spawn_context(&sandbox))
    .with_turn_event_sink(turn_event_tx);
    let controller = Arc::new(TuiSessionController::new());
    let mut app = ChatApp::new(Vec::new(), sandbox, SessionId::new())
        .with_session_controller(Arc::clone(&controller));

    assert!(SessionController::request_mode(controller.as_ref(), "yolo").is_err());
    assert!(!app.apply_pending_controller_state(&mut session));
    assert_eq!(session.mode(), InteractionMode::Chat);
    Ok(())
}

// ────────────────────────────────────────────────────────────────────
// AP W5-10b — Zellen-Verdrahtung.
// ────────────────────────────────────────────────────────────────────

fn rendered_cells(app: &ChatApp) -> String {
    app.cells
        .iter()
        .flat_map(|cell| cell.display_lines(120, style::Theme::Dark))
        .flat_map(|line| line.spans)
        .map(|span| span.content.into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Kernanforderung W5-10b: drei Kind-Ereignisse erzeugen **eine** Zelle, die
/// fortgeschrieben wird — nicht drei.
#[test]
fn three_child_events_produce_a_single_sub_agent_cell() -> TestResult {
    let mut app = test_chat_app()?;
    let mut state = TurnEventState::default();
    let turn_id = TurnId::new();
    let child = SessionId::from_str("child-explorer-1");
    let before = app.cells_len();

    assert!(handle_turn_event(
        &mut app,
        &mut state,
        TurnEvent::ChildSpawned {
            turn_id: turn_id.clone(),
            child: child.clone(),
            role: role_names::EXPLORER.to_owned(),
            question: Some("Wo liegt der Turn-Loop?".to_owned()),
        }
    ));
    assert_eq!(app.cells_len(), before + 1);

    assert!(handle_turn_event(
        &mut app,
        &mut state,
        TurnEvent::ChildProgress {
            turn_id: turn_id.clone(),
            child: child.clone(),
            tool_calls: 7,
            tokens: 4242,
        }
    ));
    assert!(handle_turn_event(
        &mut app,
        &mut state,
        TurnEvent::ChildCompleted {
            turn_id,
            child,
            outcome: "completed".to_owned(),
            duration_ms: 512,
        }
    ));

    assert_eq!(
        app.cells_len(),
        before + 1,
        "drei Ereignisse desselben Kindes dürfen nur eine Zelle erzeugen"
    );
    let rendered = rendered_cells(&app);
    assert!(rendered.contains(role_names::EXPLORER));
    assert!(rendered.contains('7'), "Tool-Aufrufe fehlen: {rendered:?}");
    assert!(
        rendered.contains("done"),
        "die Zelle muss den Abschluss zeigen: {rendered:?}"
    );
    Ok(())
}

/// Der kurze Ack-Text vor einem
/// Tool-Aufruf (`MessagePhase::Commentary`) muss sofort als sichtbare
/// Assistant-Zelle erscheinen, statt erst am Turn-Ende über
/// `reveal_reply`.
#[test]
fn commentary_assistant_message_renders_immediately() -> TestResult {
    use harw_protocol::items::AssistantMessageItem;

    let mut app = test_chat_app()?;
    let mut state = TurnEventState::default();
    let before = app.cells_len();

    let handled = handle_turn_event(
        &mut app,
        &mut state,
        TurnEvent::ItemAdded {
            turn_id: TurnId::new(),
            item: TurnItem::AssistantMessage(AssistantMessageItem {
                id: ItemId::new(),
                content: vec![ContentPart::Text {
                    text: "Ich prüfe die Konfiguration …".to_owned(),
                }],
                phase: Some(harw_types::MessagePhase::Commentary),
            }),
        },
    );

    assert!(handled, "Commentary-Nachrichten müssen ein Redraw auslösen");
    assert_eq!(
        app.cells_len(),
        before + 1,
        "Commentary-Nachricht muss eine neue Zelle anlegen"
    );
    let rendered = rendered_cells(&app);
    assert!(
        rendered.contains("Ich prüfe die Konfiguration …"),
        "Commentary-Text fehlt im gerenderten Verlauf: {rendered:?}"
    );
    Ok(())
}

/// Gegenprobe zu `commentary_assistant_message_renders_immediately`:
/// `phase == FinalAnswer` darf **keine** zusätzliche Zelle erzeugen — die
/// finale Antwort bleibt exklusiv `reveal_reply` am Turn-Ende vorbehalten,
/// sonst entstünde eine Doppelanzeige.
#[test]
fn final_answer_assistant_message_is_not_rendered_live() -> TestResult {
    use harw_protocol::items::AssistantMessageItem;

    let mut app = test_chat_app()?;
    let mut state = TurnEventState::default();
    let before = app.cells_len();

    let handled = handle_turn_event(
        &mut app,
        &mut state,
        TurnEvent::ItemAdded {
            turn_id: TurnId::new(),
            item: TurnItem::AssistantMessage(AssistantMessageItem {
                id: ItemId::new(),
                content: vec![ContentPart::Text {
                    text: "Das ist die finale Antwort.".to_owned(),
                }],
                phase: Some(harw_types::MessagePhase::FinalAnswer),
            }),
        },
    );

    assert!(
        !handled,
        "FinalAnswer darf keinen Redraw über handle_turn_event auslösen"
    );
    assert_eq!(
        app.cells_len(),
        before,
        "FinalAnswer darf keine zusätzliche Zelle anlegen (bleibt reveal_reply vorbehalten)"
    );
    Ok(())
}

// ------------------------------------------------------------------
// Bugfix: doppelte/verwaiste Antwortanzeige (`latest_final_reply`,
// `suppress_if_matches_commentary`) — siehe Bugfix-Abschnitt der
// `drive_turn_animated`-Doku.
// ------------------------------------------------------------------

fn tool_result_pair(history: &mut ConversationHistory) {
    let call_id = ToolCallId::new();
    history.push_tool_call(call_id.clone(), "fs.read", json!({ "path": "/tmp/x" }));
    history.push_tool_result(call_id, ToolCallResult::success(json!({ "ok": true })), 5);
}

/// Fall (a): endet der Turn mit einer bereits live gezeigten
/// `Commentary`-Nachricht, gefolgt nur von Tool-Aufrufen, darf
/// `latest_final_reply` sie nicht ein zweites Mal liefern.
#[test]
fn latest_final_reply_commentary_then_tool_calls_returns_none() -> TestResult {
    let mut history = ConversationHistory::new();
    history.push_assistant_text(
        "Ich prüfe die Konfiguration …",
        Some(harw_types::MessagePhase::Commentary),
    );
    tool_result_pair(&mut history);

    assert_eq!(latest_final_reply(&history, 0), None);
    Ok(())
}

/// Fall (b): eine finale (nicht-`Commentary`) Antwort wird genau einmal
/// geliefert — unabhängig von einer vorherigen `Commentary`-Nachricht
/// desselben Turns.
#[test]
fn latest_final_reply_final_answer_is_returned_once() -> TestResult {
    let mut history = ConversationHistory::new();
    history.push_assistant_text(
        "Ich prüfe die Konfiguration …",
        Some(harw_types::MessagePhase::Commentary),
    );
    history.push_assistant_text(
        "Die finale Antwort.",
        Some(harw_types::MessagePhase::FinalAnswer),
    );

    assert_eq!(
        latest_final_reply(&history, 0),
        Some("Die finale Antwort.".to_owned())
    );
    Ok(())
}

/// Eine Assistant-Nachricht ganz ohne `phase` gilt — wie `FinalAnswer` —
/// als enthüllbar (siehe `handle_turn_event`s Doku: „bei `FinalAnswer`
/// oder fehlender Phase bleibt die Anzeige `reveal_reply` vorbehalten").
#[test]
fn latest_final_reply_missing_phase_is_returned() -> TestResult {
    let mut history = ConversationHistory::new();
    history.push_assistant_text("Antwort ohne Phasen-Metadaten.", None);

    assert_eq!(
        latest_final_reply(&history, 0),
        Some("Antwort ohne Phasen-Metadaten.".to_owned())
    );
    Ok(())
}

/// Fall (c): fügt der aktuelle Turn gar keine neue Assistant-Nachricht
/// hinzu (z. B. ein zusammengefasster Warteschlangen-Turn), darf die
/// Antwort des VORHERIGEN Turns nicht erneut geliefert werden —
/// `since_len` grenzt die Suche strikt auf die neuen Items ein.
#[test]
fn latest_final_reply_no_new_assistant_item_returns_none() -> TestResult {
    let mut history = ConversationHistory::new();
    history.push_assistant_text(
        "Antwort des vorherigen Turns.",
        Some(harw_types::MessagePhase::FinalAnswer),
    );
    let since_len = history.len();
    tool_result_pair(&mut history);

    assert_eq!(latest_final_reply(&history, since_len), None);
    Ok(())
}

/// Zweite Verteidigungslinie: ein `reply`, der exakt dem zuletzt live
/// gepushten Commentary-Text entspricht, wird unterdrückt.
#[test]
fn suppress_if_matches_commentary_dedupes_identical_text() -> TestResult {
    assert_eq!(
        suppress_if_matches_commentary(Some("gleicher Text".to_owned()), Some("gleicher Text")),
        None
    );
    Ok(())
}

/// Unterschiedlicher Text, kein Commentary-Wächter gesetzt, oder gar
/// keine Antwort — in allen drei Fällen bleibt `reply` unverändert.
#[test]
fn suppress_if_matches_commentary_keeps_unrelated_values() -> TestResult {
    assert_eq!(
        suppress_if_matches_commentary(Some("neuer Text".to_owned()), Some("alter Text")),
        Some("neuer Text".to_owned())
    );
    assert_eq!(
        suppress_if_matches_commentary(Some("neuer Text".to_owned()), None),
        Some("neuer Text".to_owned())
    );
    assert_eq!(
        suppress_if_matches_commentary(None, Some("alter Text")),
        None
    );
    Ok(())
}

/// A6: eine abgeschlossene Werkzeugausführung schreibt die Dauer nur in
/// den `ExportEntry::ToolResult`-Eintrag; der zugehörige
/// `ToolCall`-Eintrag bleibt ohne `duration_ms` (kein Backfill mehr).
#[test]
fn tool_call_completion_leaves_the_call_entry_without_a_duration() -> TestResult {
    let mut app = test_chat_app()?;
    let mut state = TurnEventState::default();
    let turn_id = TurnId::new();
    let call_id = ToolCallId::new();

    assert!(handle_turn_event(
        &mut app,
        &mut state,
        TurnEvent::ToolCallRequested {
            turn_id: turn_id.clone(),
            call_id: call_id.clone(),
            tool_name: "fs.read".to_owned(),
            arguments: json!({ "path": "/tmp/x" }),
        }
    ));
    assert!(handle_turn_event(
        &mut app,
        &mut state,
        TurnEvent::ToolCallCompleted {
            turn_id,
            call_id: call_id.clone(),
            result: ToolCallResult::error("nicht gefunden"),
            duration_ms: 42,
            placement: None,
        }
    ));

    let call_duration = app.export_entries.iter().find_map(|entry| match entry {
        ExportEntry::ToolCall {
            call_id: id,
            duration_ms,
            ..
        } if *id == call_id.to_string() => Some(*duration_ms),
        _ => None,
    });
    assert_eq!(
        call_duration,
        Some(None),
        "ToolCall-Eintrag muss existieren, aber ohne Dauer"
    );

    let result_duration = app.export_entries.iter().find_map(|entry| match entry {
        ExportEntry::ToolResult {
            call_id: id,
            duration_ms,
            ..
        } if *id == call_id.to_string() => Some(*duration_ms),
        _ => None,
    });
    assert_eq!(
        result_duration,
        Some(Some(42)),
        "ToolResult-Eintrag muss die Dauer tragen"
    );
    Ok(())
}

/// A6 (Hydrate-Pfad): beim Laden einer durablen Historie bekommt der
/// `ExportEntry::ToolCall`-Eintrag `trust` nachgetragen, aber keine Dauer
/// mehr — die Dauer bleibt exklusiv am `ExportEntry::ToolResult`.
#[test]
fn hydrated_tool_result_leaves_the_call_entry_without_a_duration() -> TestResult {
    use harw_protocol::items::{ResultTrust, ToolCallItem, ToolCallResult, ToolResultItem};
    use harw_types::{ItemId, ToolCallId};

    let call_id = ToolCallId::new();
    let mut history = ConversationHistory::new();
    history.push(TurnItem::ToolCall(ToolCallItem {
        id: ItemId::new(),
        call_id: call_id.clone(),
        tool_name: "fs.read".to_owned(),
        arguments: json!({ "path": "/tmp/x" }),
    }));
    history.push(TurnItem::ToolResult(ToolResultItem {
        id: ItemId::new(),
        call_id: call_id.clone(),
        result: ToolCallResult::error("nicht gefunden"),
        duration_ms: 17,
        trust: ResultTrust::Runtime,
    }));

    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut session = AgentSession::new_with_id(
        SessionId::from_str("hydrate-duration"),
        AgentRole::Assistant,
        None,
        ExtensionRegistry::builder().build(),
        event_tx,
    );
    let mut app = test_chat_app()?;
    install_loaded_history(&mut session, &mut app, history);

    let call_entry = app.export_entries.iter().find_map(|entry| match entry {
        ExportEntry::ToolCall {
            call_id: id,
            duration_ms,
            trust,
            ..
        } if *id == call_id.to_string() => Some((*duration_ms, trust.clone())),
        _ => None,
    });
    assert_eq!(
        call_entry,
        Some((None, Some(export_trust(ResultTrust::Runtime)))),
        "ToolCall-Eintrag: keine Dauer, aber nachgetragenes Vertrauen"
    );

    let result_duration = app.export_entries.iter().find_map(|entry| match entry {
        ExportEntry::ToolResult {
            call_id: id,
            duration_ms,
            ..
        } if *id == call_id.to_string() => Some(*duration_ms),
        _ => None,
    });
    assert_eq!(result_duration, Some(Some(17)));
    Ok(())
}

/// Ein Fortschritt für ein unbekanntes Kind erzeugt **keine** Zelle.
#[test]
fn progress_for_an_unknown_child_creates_no_cell() -> TestResult {
    let mut app = test_chat_app()?;
    let mut state = TurnEventState::default();
    let before = app.cells_len();

    let handled = handle_turn_event(
        &mut app,
        &mut state,
        TurnEvent::ChildProgress {
            turn_id: TurnId::new(),
            child: SessionId::from_str("never-spawned"),
            tool_calls: 1,
            tokens: 1,
        },
    );

    assert!(!handled);
    assert_eq!(app.cells_len(), before);
    Ok(())
}

/// Ohne Plan-Dienste geht die Plan-Information nicht verloren — sie wird nur
/// einzeilig statt als Graph gezeigt.
#[test]
fn plan_updates_degrade_to_a_system_line_without_plan_services() -> TestResult {
    let mut app = test_chat_app()?;
    let mut state = TurnEventState::default();

    assert!(handle_turn_event(
        &mut app,
        &mut state,
        TurnEvent::PlanUpdated {
            plan_id: "plan-7".to_owned(),
            revision: 3,
            summary: "Knoten ergänzt".to_owned(),
        }
    ));

    let rendered = rendered_cells(&app);
    assert!(rendered.contains("plan-7"));
    assert!(rendered.contains("Knoten ergänzt"));
    Ok(())
}

/// TUI-03 (R18 F4): der erste Stand eines Plans kommt als Vollbild, jeder
/// weitere als Delta, das Ctrl+O aufklappt; `/clear` setzt zurück.
#[test]
fn plan_cells_render_a_delta_after_the_first_state() -> TestResult {
    let mut app = test_chat_app()?;
    let plan = || harw_plan::Plan {
        id: harw_plan::PlanId::new("p-delta"),
        revision: harw_plan::RevisionId::new(1),
        parent_revision: None,
        goal_statement: "Ziel".to_owned(),
        goal_id: None,
        nodes: Vec::new(),
        created_at: time::OffsetDateTime::UNIX_EPOCH,
        updated_at: time::OffsetDateTime::UNIX_EPOCH,
        tenant: None,
    };

    app.push_plan_cell(plan());
    let first = rendered_cells(&app);
    assert!(first.contains("(no nodes in plan)"), "{first}");
    assert!(!first.contains("no node changes"), "{first}");

    app.push_plan_cell(plan());
    let second = rendered_cells(&app);
    assert!(
        second.contains("no node changes · 0 nodes · ctrl+o to expand"),
        "{second}"
    );
    // Ctrl+O klappt die letzte (Delta-)Zelle auf: Fortschritt statt Hinweis.
    assert!(app.toggle_tool_cells());
    let expanded = rendered_cells(&app);
    assert!(!expanded.contains("no node changes"), "{expanded}");
    assert!(expanded.contains("0/0 done (0 %)"), "{expanded}");

    app.clear_transcript();
    app.push_plan_cell(plan());
    let after_clear = rendered_cells(&app);
    assert!(!after_clear.contains("no node changes"), "{after_clear}");
    Ok(())
}

/// `ModeChanged` zieht den internen Modus nach; ein unbekannter Name nicht.
#[test]
fn mode_changed_events_update_the_active_mode() -> TestResult {
    let mut app = test_chat_app()?;
    let mut state = TurnEventState::default();

    assert!(handle_turn_event(
        &mut app,
        &mut state,
        TurnEvent::ModeChanged {
            mode: "explore".to_owned(),
        }
    ));
    assert_eq!(app.active_mode(), InteractionMode::Explore);

    assert!(!handle_turn_event(
        &mut app,
        &mut state,
        TurnEvent::ModeChanged {
            mode: "teleport".to_owned(),
        }
    ));
    assert_eq!(app.active_mode(), InteractionMode::Explore);
    Ok(())
}

/// Nur die exakte Zeile `/goal check` löst eine [`GoalCell`] aus.
#[test]
fn only_goal_check_requests_a_goal_cell() -> TestResult {
    assert!(is_goal_check_command("/goal check"));
    assert!(is_goal_check_command("  /goal   check  "));
    assert!(!is_goal_check_command("/goal set"));
    assert!(!is_goal_check_command("/goal"));
    assert!(!is_goal_check_command("/plan check"));
    Ok(())
}

/// Ohne durchgereichte Plan-Dienste gibt es keinen Zielstand — und keinen
/// Fehler.
#[test]
fn goal_cell_is_absent_without_plan_services() -> TestResult {
    let app = test_chat_app()?;

    assert!(goal_cell_for_command(&app, "/goal check").is_none());
    Ok(())
}

// ── Welle 4a/7b: Picker-Konsolidierung (`/model`, `/uia-model`,
// `/uia-worker-model`, `/effort`, `/uia-effort`) ──────────────────────

/// Ohne Konfiguration (Test-`ChatApp` ohne Runtime-Montage) wird kein
/// leerer Dialog geöffnet, sondern eine klare Systemzeile angehängt —
/// für jedes der drei `PickerTarget`-Ziele.
#[test]
fn open_model_switch_picker_without_config_pushes_system_line_not_overlay() -> TestResult {
    for target in [
        PickerTarget::Orchestrator,
        PickerTarget::Uia,
        PickerTarget::UiaWorker {
            fixed_provider: "anthropic".to_owned(),
        },
    ] {
        let mut app = test_chat_app()?;
        app.open_model_switch_picker(target);
        assert!(app.overlay.is_none());
    }
    Ok(())
}

/// `Accept` auf [`Overlay::ModelSwitch`] synthetisiert je nach `target`
/// die richtige Befehlszeile — `/model switch`, `/uia-model switch` bzw.
/// `/uia-worker-model switch` — und schließt das Overlay.
#[test]
fn model_switch_accept_emits_the_command_line_for_each_target() -> TestResult {
    let providers = vec![ProviderEntry {
        id: "anthropic".to_owned(),
        label: "Anthropic".to_owned(),
    }];
    let models = vec![(
        "anthropic".to_owned(),
        vec![ModelEntry {
            id: "claude-sonnet".to_owned(),
            label: "Claude Sonnet".to_owned(),
        }],
    )];

    let cases = [
        (PickerTarget::Orchestrator, "/model switch claude-sonnet"),
        (PickerTarget::Uia, "/uia-model switch claude-sonnet"),
        (
            PickerTarget::UiaWorker {
                fixed_provider: "anthropic".to_owned(),
            },
            "/uia-worker-model switch claude-sonnet",
        ),
    ];

    for (target, expected) in cases {
        let mut app = test_chat_app()?;
        let picker =
            ModelSwitchPicker::new(target, providers.clone(), models.clone(), None, None)
                .ok_or(TestError::Missing("providers fixture ist nicht leer"))?;
        app.overlay = Some(Overlay::ModelSwitch(Box::new(picker)));

        let (bus, mut receiver) = harw_event_channel();
        // Erster Enter wählt (bzw. bestätigt) den einzigen Provider und
        // wechselt in die Modell-Stufe (bei `UiaWorker` ist die
        // Provider-Stufe bereits übersprungen, ein Enter genügt dort
        // direkt für das Modell).
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &bus,
        );
        if app.overlay.is_some() {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &bus,
            );
        }

        assert!(
            app.overlay.is_none(),
            "Overlay muss nach Accept geschlossen sein"
        );
        match receiver.try_recv() {
            Ok(HarwEvent::Command(command)) => assert_eq!(command, expected),
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartete HarwEvent::Command({expected:?}), bekam {other:?}"
                )));
            }
        }
    }
    Ok(())
}

/// `PickerTarget::Role` synthetisiert `/models set <rolle> <provider>/<modell>`.
#[test]
fn model_switch_accept_for_role_target_emits_models_set() -> TestResult {
    let providers = vec![ProviderEntry {
        id: "anthropic".to_owned(),
        label: "Anthropic".to_owned(),
    }];
    let models = vec![(
        "anthropic".to_owned(),
        vec![ModelEntry {
            id: "claude-sonnet".to_owned(),
            label: "Claude Sonnet".to_owned(),
        }],
    )];
    let target = PickerTarget::Role {
        role: harw_config::ModelRole::Orchestrator,
    };
    let expected = target.command_line("anthropic", "claude-sonnet");
    let picker = ModelSwitchPicker::new(target, providers, models, None, None)
        .ok_or(TestError::Missing("providers fixture ist nicht leer"))?;
    let mut app = test_chat_app()?;
    app.overlay = Some(Overlay::ModelSwitch(Box::new(picker)));
    let (bus, mut receiver) = harw_event_channel();
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    handle_key(&mut app, enter, &bus);
    if app.overlay.is_some() {
        handle_key(&mut app, enter, &bus);
    }
    assert!(app.overlay.is_none());
    match receiver.try_recv() {
        Ok(HarwEvent::Command(command)) => {
            assert_eq!(command, expected);
            assert!(command.starts_with("/models set "), "{command}");
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "erwartete /models set, bekam {other:?}"
            )));
        }
    }
    Ok(())
}

// ── T21: lokale Befehle, Ansichten, Werkbank, Erwähnungen ──────────

/// `/help` öffnet die generische Hilfe-Ansicht, `/workbench` blendet die
/// Werkbank ein und markiert sie zum Laden.
#[test]
fn local_intercept_opens_views_and_toggles_workbench() -> TestResult {
    let mut app = test_chat_app()?;
    let (bus, _receiver) = harw_event_channel();

    let help = local_intercept_for(&app, "/help").ok_or(TestError::Missing("help"))?;
    assert!(apply_local_intercept(&mut app, help, &bus).is_none());
    assert!(matches!(app.overlay, Some(Overlay::View(_))));

    app.overlay = None;
    let workbench =
        local_intercept_for(&app, "/workbench").ok_or(TestError::Missing("workbench"))?;
    assert!(apply_local_intercept(&mut app, workbench, &bus).is_none());
    assert!(app.panels.workbench_visible);
    assert!(app.workbench_needs_refresh());

    // Befehle mit Argumenten bleiben beim regulären Dispatch.
    assert!(local_intercept_for(&app, "/model switch x").is_none());
    assert!(local_intercept_for(&app, "/status").is_none());
    Ok(())
}

/// `#notiz` und `/sessions` werden zu Slash-Zeilen umgeschrieben und
/// erneut über den Command-Kanal geschickt; `@rolle` wird Chat.
#[test]
fn local_intercept_rewrites_notes_and_routes_mentions_to_chat() -> TestResult {
    let mut app = test_chat_app()?;
    let (bus, mut receiver) = harw_event_channel();

    let note = local_intercept_for(&app, "#Merken").ok_or(TestError::Missing("note"))?;
    assert!(apply_local_intercept(&mut app, note, &bus).is_none());
    match receiver.try_recv() {
        Ok(HarwEvent::Command(command)) => assert!(
            command == "/diary note Merken" || command == "/memory record Merken",
            "{command}"
        ),
        other => {
            return Err(TestError::Unexpected(format!(
                "erwartete Rewrite, bekam {other:?}"
            )));
        }
    }

    let sessions =
        local_intercept_for(&app, "/sessions").ok_or(TestError::Missing("sessions"))?;
    assert!(apply_local_intercept(&mut app, sessions, &bus).is_none());
    assert!(matches!(
        receiver.try_recv(),
        Ok(HarwEvent::Command(command)) if command == "/resume"
    ));

    let role = KNOWN_ROLES
        .first()
        .ok_or(TestError::Missing("bekannte Rolle"))?;
    let mention = local_intercept_for(&app, &format!("@{role} bitte prüfen"))
        .ok_or(TestError::Missing("mention"))?;
    assert!(apply_local_intercept(&mut app, mention, &bus).is_none());
    let queued = app
        .pending_turns
        .pop_front()
        .ok_or(TestError::Missing("chat turn"))?;
    assert!(queued.contains(role), "{queued}");
    assert!(queued.contains("bitte prüfen"), "{queued}");
    Ok(())
}

/// `/clear` leert nur die Anzeige; `/verbose` schaltet die Werkzeug-
/// Ausführlichkeit um.
#[test]
fn local_clear_and_verbose_change_display_state() -> TestResult {
    let mut app = test_chat_app()?;
    let (bus, _receiver) = harw_event_channel();
    app.push_line(Role::User, "hallo");
    app.push_line(Role::Assistant, "welt");
    let exported = app.export_entries.len();

    let clear = local_intercept_for(&app, "/clear").ok_or(TestError::Missing("clear"))?;
    apply_local_intercept(&mut app, clear, &bus);
    // Nur die Hinweiszeile bleibt; der Export-Verlauf ist unverändert.
    assert_eq!(app.cells_len(), 1);
    assert_eq!(app.export_entries.len(), exported);

    let before = app.tool_verbosity;
    let verbose = local_intercept_for(&app, "/verbose").ok_or(TestError::Missing("verbose"))?;
    apply_local_intercept(&mut app, verbose, &bus);
    assert_ne!(app.tool_verbosity, before);
    Ok(())
}

/// Toggle-Zyklus (B11c): `/verbose` zweimal bringt den Detailgrad vom
/// Default `Activity` über `Verbose` zurück zu `Activity`.
#[test]
fn verbose_toggle_cycles_activity_to_verbose_and_back() -> TestResult {
    let mut app = test_chat_app()?;
    let (bus, _receiver) = harw_event_channel();
    assert!(matches!(app.tool_verbosity, ToolVerbosity::Activity));

    let first = local_intercept_for(&app, "/verbose").ok_or(TestError::Missing("verbose"))?;
    apply_local_intercept(&mut app, first, &bus);
    assert!(matches!(app.tool_verbosity, ToolVerbosity::Verbose));

    let second = local_intercept_for(&app, "/verbose").ok_or(TestError::Missing("verbose"))?;
    apply_local_intercept(&mut app, second, &bus);
    assert!(matches!(app.tool_verbosity, ToolVerbosity::Activity));
    Ok(())
}

/// F6 öffnet das Kanban-Board als generische Ansicht und reiht dessen
/// Initial-Abruf ein; ohne Runtime landet ein Fehler in der Ansicht,
/// nicht im Chat.
#[tokio::test]
async fn view_hotkey_opens_kanban_and_fetch_does_not_touch_chat() -> TestResult {
    let mut app = test_chat_app()?;
    let (bus, _receiver) = harw_event_channel();
    assert!(handle_key(
        &mut app,
        KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE),
        &bus
    ));
    assert!(matches!(app.overlay, Some(Overlay::View(_))));
    assert!(app.pending_fetches.contains(&DataFetch::Overlay {
        command: crate::kanban_board::REFRESH_COMMAND.to_owned(),
        generation: app.overlay_generation,
    }));

    let cells = app.cells_len();
    assert!(process_pending_fetches(&mut app).await);
    assert_eq!(
        app.cells_len(),
        cells,
        "Datenabruf schreibt nie in den Chat"
    );
    assert!(app.pending_fetches.is_empty());
    assert!(!process_pending_fetches(&mut app).await);
    Ok(())
}

/// F9 öffnet das Matrix-Panel; Matrix-Ereignisse vom Bus gehen an die
/// Ansicht (nicht an den Agenten-Monitor) und reihen ihr Nachladen ein.
#[test]
fn matrix_hotkey_and_live_events_queue_refresh() -> TestResult {
    let mut app = test_chat_app()?;
    let (bus, _receiver) = harw_event_channel();
    let hub = harw_core::AgentEventHub::default();
    app.attach_agent_events(&hub);

    // Ohne offene Ansicht: kein Abruf, kein Monitor-Eintrag.
    hub.publish_matrix(
        app.session_id.clone(),
        "run-1",
        serde_json::json!({"round": 1, "text": "x"}),
    );
    app.drain_agent_events();
    assert!(app.pending_fetches.is_empty());

    assert!(handle_key(
        &mut app,
        KeyEvent::new(KeyCode::F(9), KeyModifiers::NONE),
        &bus
    ));
    assert!(matches!(app.overlay, Some(Overlay::View(_))));
    let fetch = DataFetch::Overlay {
        command: crate::matrix_view::REFRESH_COMMAND.to_owned(),
        generation: app.overlay_generation,
    };
    assert!(app.pending_fetches.contains(&fetch));
    app.pending_fetches.clear();

    hub.publish_matrix(
        app.session_id.clone(),
        "run-1",
        serde_json::json!({"round": 2, "kind": "roll", "text": "Würfel"}),
    );
    assert!(app.drain_agent_events());
    assert_eq!(app.pending_fetches, vec![fetch]);

    // Andere Ansichten erhalten das Ereignis, laden aber nicht nach.
    app.open_overlay_view(Box::new(KanbanBoard::new()));
    app.pending_fetches.clear();
    hub.publish_matrix(app.session_id.clone(), "run-1", serde_json::json!({}));
    app.drain_agent_events();
    assert!(app.pending_fetches.is_empty());
    Ok(())
}

/// Knowledge-Ereignisse markieren nur die passenden Panels als veraltet:
/// `kanban` lädt ein offenes Board nach, `workbench` das Werkbank-Panel,
/// fremde Flächen lassen die offene Ansicht in Ruhe.
#[test]
fn knowledge_events_mark_the_matching_panels_stale() -> TestResult {
    let mut app = test_chat_app()?;
    let hub = harw_core::AgentEventHub::default();
    app.attach_agent_events(&hub);
    app.open_overlay_view(Box::new(KanbanBoard::new()));
    app.pending_fetches.clear();

    hub.publish_knowledge(app.session_id.clone(), "palace", None);
    assert!(!app.drain_agent_events());
    assert!(app.pending_fetches.is_empty());

    hub.publish_knowledge(
        app.session_id.clone(),
        "kanban",
        Some("default/card-1".to_owned()),
    );
    assert!(app.drain_agent_events());
    assert_eq!(
        app.pending_fetches,
        vec![DataFetch::Overlay {
            command: crate::kanban_board::REFRESH_COMMAND.to_owned(),
            generation: app.overlay_generation,
        }]
    );

    app.workbench
        .apply_data(&serde_json::json!({"scope": "session:s"}));
    assert!(!app.workbench.is_stale());
    hub.publish_knowledge(app.session_id.clone(), "workbench", None);
    assert!(app.drain_agent_events());
    assert!(app.workbench.is_stale());
    Ok(())
}

/// Ein Abruf für eine inzwischen ersetzte Ansicht wird verworfen.
#[tokio::test]
async fn stale_overlay_fetch_is_dropped_after_view_replaced() -> TestResult {
    let mut app = test_chat_app()?;
    app.open_overlay_view(Box::new(KanbanBoard::new()));
    let stale_generation = app.overlay_generation;
    app.open_overlay_view(Box::new(HelpOverlay::new(
        &app.command_registry,
        &app.key_bindings,
        HelpTab::Keys,
    )));
    assert_ne!(stale_generation, app.overlay_generation);
    assert!(app.pending_fetches.iter().all(|fetch| matches!(
        fetch,
        DataFetch::Overlay { generation, .. } if *generation == stale_generation
    )));
    // Läuft ohne Wirkung auf die neue Ansicht durch.
    process_pending_fetches(&mut app).await;
    assert!(matches!(app.overlay, Some(Overlay::View(_))));
    Ok(())
}

/// Generische Ansicht: `Prefill` schließt und füllt den Composer, `Run`
/// schickt die Zeile über den Bus und lässt die Ansicht offen.
#[test]
fn overlay_outcomes_prefill_and_run() -> TestResult {
    let mut app = test_chat_app()?;
    let (bus, mut receiver) = harw_event_channel();
    app.open_overlay_view(Box::new(KanbanBoard::new()));

    apply_overlay_outcome(
        &mut app,
        OverlayOutcome::Run("/kanban show".to_owned()),
        &bus,
    );
    assert!(matches!(app.overlay, Some(Overlay::View(_))));
    assert!(matches!(
        receiver.try_recv(),
        Ok(HarwEvent::Command(command)) if command == "/kanban show"
    ));

    apply_overlay_outcome(
        &mut app,
        OverlayOutcome::Prefill("/kanban add ".to_owned()),
        &bus,
    );
    assert!(app.overlay.is_none());
    assert_eq!(app.input(), "/kanban add ");
    Ok(())
}

/// Werkbank-Tasten: `n` füllt den Composer und gibt den Fokus ab, `R`
/// lädt still neu (kein Chat-Befehl).
#[test]
fn workbench_focus_keys_prefill_and_refresh() -> TestResult {
    let mut app = test_chat_app()?;
    app.panels.workbench_visible = true;
    app.panels.focus = crate::panes::PaneFocus::Workbench;
    app.workbench.apply_error("noch nicht geladen".to_owned());
    assert!(!app.workbench.is_stale());

    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    assert_eq!(
        handle_panel_key(&mut app, key(KeyCode::Char('R'))),
        Some(true)
    );
    assert!(app.workbench.is_stale());
    assert!(app.pending_commands.is_empty());

    assert_eq!(
        handle_panel_key(&mut app, key(KeyCode::Char('n'))),
        Some(true)
    );
    assert_eq!(app.input(), crate::workbench_pane::NOTE_PREFILL);
    assert_eq!(app.panels.focus, crate::panes::PaneFocus::Chat);
    Ok(())
}

/// Stufe 2 des Befehls-Popups: nach `/model ` erscheinen die
/// Unterkommandos; Enter ohne Suchtext sendet ab, mit Suchtext wird
/// vervollständigt.
#[test]
fn subcommand_popup_after_complete_command() -> TestResult {
    let mut app = test_chat_app()?;
    let Some(spec) = app.command_registry.find("model") else {
        return Ok(());
    };
    if spec.subcommands.is_empty() {
        return Ok(());
    }

    app.input.insert_str("/model ");
    app.sync_popup();
    assert!(matches!(
        app.command_popup.as_ref().map(CommandPopup::mode),
        Some(PopupMode::Subcommand { .. })
    ));
    let (bus, mut receiver) = harw_event_channel();
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    assert!(handle_key(&mut app, enter, &bus));
    assert!(matches!(
        receiver.try_recv(),
        Ok(HarwEvent::Command(command)) if command.trim() == "/model"
    ));

    app.input.insert_str("/model sh");
    app.sync_popup();
    assert!(handle_key(&mut app, enter, &bus));
    assert_eq!(app.input(), "/model show ");
    assert!(receiver.try_recv().is_err());

    // Argumente nach dem Unterkommando schließen das Popup.
    app.input.clear();
    app.input.insert_str("/model switch x");
    app.sync_popup();
    assert!(app.command_popup.is_none());
    Ok(())
}

/// Feldbericht: eine von `classify_line` abgelehnte Zeile (hier der
/// reservierte `$`-Präfix) darf nicht verschwinden — plain Enter im
/// Leerlauf lässt sie im Composer stehen und legt sie in die History,
/// statt sie zusammen mit dem Puffer wegzuwerfen.
#[test]
fn idle_enter_keeps_rejected_line_in_composer_and_history() -> TestResult {
    let mut app = test_chat_app()?;
    app.input.insert_str("$nope");
    let (bus, mut receiver) = harw_event_channel();
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);

    assert!(handle_key(&mut app, enter, &bus));

    assert_eq!(app.input(), "$nope");
    match receiver.try_recv() {
        Ok(HarwEvent::SystemMessage(message)) => {
            assert!(message.starts_with("Eingabe abgelehnt"), "{message}");
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "erwartete SystemMessage, bekam {other:?}"
            )));
        }
    }
    assert!(receiver.try_recv().is_err());

    // Up-Pfeil findet die abgelehnte Zeile trotzdem in der History.
    app.input.clear();
    assert!(app.input.history_back());
    assert_eq!(app.input(), "$nope");
    Ok(())
}

#[test]
fn subcommand_query_is_empty_detects_bare_command_with_space() {
    assert!(subcommand_query_is_empty("/model "));
    assert!(subcommand_query_is_empty("/model"));
    assert!(!subcommand_query_is_empty("/model s"));
}

/// Busy-sichere lokale Befehle wirken während eines Turns sofort
/// (`Local`); Sitzungswechsel, Transkript-Leeren und Befehle ohne
/// busy-sichere Klasse werden zurückgestellt (Runde 4, Teil H).
#[test]
fn busy_local_commands_run_locally_and_session_commands_are_deferred() -> TestResult {
    let local_app = || -> TestResult<ChatApp> {
        let mut app = test_chat_app()?;
        app.command_registry = CommandRegistry::built_in()
            .map_err(ctx("built_in"))?
            .with_local_specs(crate::command_catalog::local_command_specs());
        Ok(app)
    };
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    for raw in [
        "/help",
        "/models",
        "/workbench",
        "/keys",
        "/whoami",
        "/agent",
    ] {
        let mut app = local_app()?;
        app.input.insert_str(raw);
        assert_eq!(
            queue_busy_key(&mut app, enter),
            BusyKeyOutcome::Local,
            "{raw}"
        );
        assert!(app.deferred_input.is_empty(), "{raw}");
        assert!(app.busy_jobs.queued().is_empty(), "{raw}");
    }
    // `/whoami` schreibt seine Systemzeile sofort, `/help` öffnet das Overlay.
    let mut app = local_app()?;
    app.input.insert_str("/help");
    queue_busy_key(&mut app, enter);
    assert!(app.overlay.is_some());

    for raw in [
        "/clear",
        "/resume",
        "/compact",
        "/tools",
        "/new",
        "/sessions",
    ] {
        let mut app = local_app()?;
        app.input.insert_str(raw);
        assert_eq!(
            queue_busy_key(&mut app, enter),
            BusyKeyOutcome::Redraw,
            "{raw}"
        );
        assert_eq!(
            app.deferred_input.pop_front(),
            Some(TuiEvent::Paste(raw.to_owned())),
            "{raw}"
        );
        assert!(app.busy_jobs.queued().is_empty(), "{raw}");
    }
    Ok(())
}

/// Klartext aller Verlaufszellen (Testhilfe).
fn history_plain(app: &ChatApp) -> String {
    app.cells
        .iter()
        .flat_map(|cell| cell.display_lines(200, style::Theme::Dark))
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Ein offenes Overlay bekommt während eines Turns seine Tasten: Esc
/// schließt die Hilfe, statt den Turn zu unterbrechen.
#[test]
fn busy_keys_reach_open_overlay_and_esc_closes_it_without_interrupt() -> TestResult {
    let mut app = test_chat_app()?;
    app.command_registry = CommandRegistry::built_in()
        .map_err(ctx("built_in"))?
        .with_local_specs(crate::command_catalog::local_command_specs());
    let cancel = CancelToken::new();
    app.active_cancel = Some(cancel.clone());
    app.input.insert_str("/help");
    assert_eq!(
        handle_busy_event(
            &mut app,
            TuiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
        ),
        BusyKeyOutcome::Local
    );
    assert!(matches!(app.overlay, Some(Overlay::View(_))));

    // Tippen landet im Overlay, nicht im Composer.
    handle_busy_event(
        &mut app,
        TuiEvent::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
    );
    assert!(app.input.is_empty());

    handle_busy_event(
        &mut app,
        TuiEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
    );
    assert!(app.overlay.is_none(), "Esc schließt das Overlay");
    assert!(
        !cancel.is_cancelled(),
        "Esc im Overlay unterbricht den Turn nicht"
    );
    assert!(app.cancel_requested_at.is_none());
    Ok(())
}

/// Alt+↑ holt die zuletzt eingereihte Nachricht zurück in den leeren
/// Composer.
#[test]
fn busy_alt_up_recalls_last_pending_turn() -> TestResult {
    let mut app = test_chat_app()?;
    app.pending_turns.push_back("erste".to_owned());
    app.pending_turns.push_back("zweite".to_owned());
    let alt_up = TuiEvent::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));
    assert_eq!(
        handle_busy_event(&mut app, alt_up.clone()),
        BusyKeyOutcome::Redraw
    );
    assert_eq!(app.input.submission_text(), "zweite");
    assert_eq!(app.pending_turns.len(), 1);
    // Mit Text im Composer holt Alt+↑ nichts zurück.
    handle_busy_event(&mut app, alt_up);
    assert_eq!(app.pending_turns.len(), 1);
    Ok(())
}

/// App mit allen `harw-ops`-Adaptern und Dispatch ohne Montage.
fn ops_chat_app() -> TestResult<ChatApp> {
    let mut ops = OperationRegistry::new();
    harw_ops::register_all(&mut ops);
    let adapters: Vec<CommandAdapter> = ops
        .iter()
        .flat_map(|op| CommandAdapter::from_operation(Arc::clone(op)))
        .collect();
    let mut app = ChatApp::new(adapters, test_sandbox()?, SessionId::new());
    app.busy_jobs.enable_dispatch_without_runtime();
    Ok(app)
}

#[test]
fn settings_refresh_updates_consumers_without_changing_history_or_draft() -> TestResult {
    let mut app = ops_chat_app()?.with_hidden_commands(vec!["model".to_owned()]);
    app.push_line(Role::User, "Deutsch 日本語 नमस्ते /model".to_owned());
    let original_history = history_plain(&app);
    app.input.insert_str("/settings");
    app.sync_command_popup();
    let selected = app
        .command_popup
        .as_ref()
        .and_then(CommandPopup::selected_name)
        .map(str::to_owned);
    let tui = harw_config::TuiSection {
        language: harw_config::UiLanguage::Japanese,
        theme: "light".to_owned(),
        reasoning: ReasoningDisplayMode::Hidden,
        child_stream: harw_config::harness_config::ChildStreamModeToml::Off,
        status_expiry: harw_config::StatusExpiryMode::Hide,
        ..Default::default()
    };
    app.apply_display_preferences(&tui);
    assert_eq!(app.language, harw_config::UiLanguage::Japanese);
    assert_eq!(app.theme, style::Theme::Light);
    assert_eq!(app.reasoning_mode, ReasoningDisplayMode::Hidden);
    assert_eq!(app.child_stream.mode().as_str(), "none");
    assert_eq!(app.status_expiry, harw_config::StatusExpiryMode::Hide);
    assert_eq!(app.input.submission_text(), "/settings");
    assert_eq!(history_plain(&app), original_history);
    assert!(app.command_registry.find("model").is_none());
    assert_eq!(
        app.command_registry
            .find("settings")
            .map(|spec| spec.summary.as_str()),
        Some("設定")
    );
    assert_eq!(
        app.command_popup
            .as_ref()
            .and_then(CommandPopup::selected_name),
        selected.as_deref()
    );
    assert_eq!(crate::i18n::tr("settings.title", "Settings"), "Settings");
    Ok(())
}

#[tokio::test]
async fn settings_panel_keeps_failed_save_and_committed_warning_visible_after_refresh()
-> TestResult {
    fn screen(app: &ChatApp, width: u16) -> TestResult<String> {
        let backend = ratatui::backend::TestBackend::new(width, 18);
        let mut terminal = ratatui::Terminal::new(backend).map_err(ctx("settings terminal"))?;
        terminal
            .draw(|frame| render_viewport(frame, app, &Spinner::new(), None))
            .map_err(ctx("settings render"))?;
        Ok(terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" "))
    }
    fn complete(app: &mut ChatApp, busy: bool, raw: &str, result: Result<OpOutput, String>) {
        let language = app.language;
        if busy {
            let (succeeded, output) = match result {
                Ok(output) => (true, output),
                Err(text) => (false, OpOutput { text, data: None }),
            };
            apply_busy_job_done(
                app,
                BusyJobDone::Command {
                    command: BusyCommand {
                        raw: raw.to_owned(),
                        class: BusyAvailability::Immediate,
                    },
                    text: output.text,
                    data: output.data,
                    succeeded,
                    language,
                },
            );
        } else {
            app.apply_settings_completion(
                raw,
                language,
                result.as_ref().map_err(String::as_str),
            );
            app.queue_overlay_refresh();
        }
    }
    for busy in [false, true] {
        let mut fixture = crate::runtime_root::tests::runtime_backed_app()?;
        let app = &mut fixture.app;
        let before = app.theme;
        app.open_overlay_view(Box::new(crate::settings_view::SettingsView::new(&[])));
        process_pending_fetches(app).await;
        let path = fixture
            .assembly
            .spec()
            .home
            .join("profiles/default/config.toml");
        let lock =
            harw_config::ConfigWriter::open_locked(&path).map_err(ctx("hold config lock"))?;
        let raw = "/settings set tui.theme light";
        let failed = crate::command_exec::dispatch_slash_command_for(
            app.language,
            app.runtime(),
            app.adapters(),
            app.sandbox(),
            app.session_id(),
            raw,
        )
        .await;
        assert!(
            failed.is_err(),
            "held writer lock must reject save before commit"
        );
        drop(lock);
        complete(app, busy, raw, failed);
        process_pending_fetches(app).await;
        assert_eq!(app.theme, before, "failed save must not update live theme");
        for width in [24, 80] {
            assert!(
                screen(app, width)?.contains("configuration busy"),
                "busy={busy}, width={width}"
            );
        }

        let mut committed = crate::command_exec::dispatch_slash_command_for(
            app.language,
            app.runtime(),
            app.adapters(),
            app.sandbox(),
            app.session_id(),
            raw,
        )
        .await
        .map_err(ctx("commit settings"))?;
        // Deterministic committed-result fixture; the ops suite exercises the
        // actual durability-uncertain save outcome and live publication.
        committed
            .data
            .as_mut()
            .ok_or(TestError::Missing("settings data"))?["warning"] =
            serde_json::json!("durability_uncertain");
        assert_eq!(app.theme, before, "consumer waits for typed completion");
        complete(app, busy, raw, Ok(committed));
        process_pending_fetches(app).await;
        assert_eq!(app.theme, style::Theme::Light);
        assert!(matches!(app.overlay, Some(Overlay::View(_))));
        // Feedback is pinned even when navigating into the last enum editor.
        if let Some(Overlay::View(view)) = app.overlay.as_mut() {
            for _ in 0..4 {
                view.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
            }
            view.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        }
        for width in [24, 80] {
            let shown = screen(app, width)?;
            assert!(shown.contains("saved and applied"), "{shown}");
            assert!(shown.contains("uncertain"), "{shown}");
            assert!(
                !shown.contains("configuration busy"),
                "later completion replaces old status"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn settings_changes_preserve_unrelated_session_stream_override() -> TestResult {
    let mut fixture = crate::runtime_root::tests::runtime_backed_app()?;
    let app = &mut fixture.app;
    app.apply_child_stream_command("all");
    for command in [
        "/settings set tui.theme light",
        "/settings: set 'tui.language' fr",
    ] {
        let language = app.language;
        let output = crate::command_exec::dispatch_slash_command_for(
            language,
            app.runtime(),
            app.adapters(),
            app.sandbox(),
            app.session_id(),
            command,
        )
        .await
        .map_err(ctx("settings mutation"))?;
        apply_busy_job_done(
            app,
            BusyJobDone::Command {
                command: BusyCommand {
                    raw: command.to_owned(),
                    class: BusyAvailability::Immediate,
                },
                text: output.text,
                data: output.data,
                succeeded: true,
                language,
            },
        );
        assert_eq!(app.child_stream.mode().as_str(), "all");
    }
    assert_eq!(app.language, harw_config::UiLanguage::French);
    assert_eq!(app.theme, style::Theme::Light);
    // A failed command must not reload even the named setting.
    apply_busy_job_done(
        app,
        BusyJobDone::Command {
            command: BusyCommand {
                raw: "/settings set tui.child_stream invalid".to_owned(),
                class: BusyAvailability::Immediate,
            },
            text: "Erreur".to_owned(),
            data: None,
            succeeded: false,
            language: app.language,
        },
    );
    assert_eq!(app.child_stream.mode().as_str(), "all");
    Ok(())
}

#[tokio::test]
async fn settings_busy_failures_use_captured_locale_and_typed_success() -> TestResult {
    let failing_app = |delay| -> TestResult<ChatApp> {
        let mut operation = SlowOperation::new(delay);
        operation.fail = true;
        let adapters = CommandAdapter::from_operation(Arc::new(operation));
        let mut app = ChatApp::new(adapters, test_sandbox()?, SessionId::new());
        app.busy_jobs.enable_dispatch_without_runtime();
        Ok(app)
    };
    let mut japanese = failing_app(Duration::from_millis(60))?;
    let mut french = failing_app(Duration::from_millis(5))?;
    japanese.language = harw_config::UiLanguage::Japanese;
    french.language = harw_config::UiLanguage::French;
    for app in [&mut japanese, &mut french] {
        app.busy_jobs.queue(BusyCommand {
            raw: "/slow".to_owned(),
            class: BusyAvailability::Staged,
        });
        start_busy_work(app);
    }
    japanese.language = harw_config::UiLanguage::Hindi;
    let (fr, ja) = tokio::join!(french.busy_jobs.recv(), japanese.busy_jobs.recv());
    for (app, result, prefix) in [
        (&mut french, fr, "Erreur :"),
        (&mut japanese, ja, "エラー:"),
    ] {
        let done = result.ok_or(TestError::Missing("busy result"))?;
        match &done {
            BusyJobDone::Command {
                succeeded, text, ..
            } => {
                assert!(!succeeded, "failure must remain typed");
                assert!(text.starts_with(prefix), "{text}");
            }
            _ => return Err(TestError::Missing("command result")),
        }
        apply_busy_job_done(app, done);
        assert!(!history_plain(app).contains("gilt ab nächstem Turn"));
    }
    assert_eq!(crate::i18n::tr("settings.title", "Settings"), "Settings");
    Ok(())
}

#[test]
fn settings_open_busy_and_delegation_preserves_fixed_model_rules() -> TestResult {
    let fixture = crate::runtime_root::tests::runtime_backed_app()?;
    let mut app = fixture
        .app
        .with_hidden_commands(vec!["model".to_owned(), "models".to_owned()]);
    app.active_cancel = Some(CancelToken::new());
    app.input.insert_str("/settings");
    let enter = TuiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(handle_busy_event(&mut app, enter), BusyKeyOutcome::Local);
    assert!(matches!(app.overlay, Some(Overlay::View(_))));
    assert!(matches!(
        local_intercept_for(&app, "/model"),
        Some(LocalIntercept::System(_))
    ));
    Ok(())
}

#[tokio::test]
async fn busy_uia_controls_keep_next_session_timing_and_payloads() -> TestResult {
    use harw_config::UiLanguage::{English, German, Japanese};
    for language in [English, German, Japanese] {
        let fixture = crate::runtime_root::tests::runtime_backed_app()?;
        let services = fixture
            .assembly
            .services()
            .service_map(harw_runtime::ServiceSurface::Slash);
        let live = services
            .get::<harw_ops::live_config::SharedLiveConfig>()
            .ok_or(TestError::Missing("live config"))?;
        live.update(|config| {
            config.models.insert(
                "claude-mythos-preview".to_owned(),
                serde_json::from_value(serde_json::json!({
                    "id": "claude-mythos-preview", "provider": "anthropic"
                }))
                .expect("model fixture"),
            );
        });
        let mut app = fixture.app;
        app.language = language;
        app.active_cancel = Some(CancelToken::new());
        let before = app.session_controller.snapshot();
        for raw in [
            "/uia-effort high",
            "/uia-worker-model switch claude-mythos-preview",
        ] {
            app.input.insert_str(raw);
            assert_eq!(
                queue_busy_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                BusyKeyOutcome::Dispatch(BusyAvailability::Staged)
            );
            assert!(app.deferred_input.is_empty());
            start_busy_work(&mut app);
            let done = tokio::time::timeout(Duration::from_secs(5), app.busy_jobs.recv())
                .await
                .map_err(ctx("busy control completion"))?
                .ok_or(TestError::Missing("busy control result"))?;
            let BusyJobDone::Command {
                command,
                text,
                data,
                succeeded,
                language: captured,
            } = &done
            else {
                return Err(TestError::Missing("command completion"));
            };
            assert!(*succeeded, "{raw}: {text}");
            assert_eq!(*captured, language);
            assert_eq!(command.raw, raw);
            assert_eq!(command.class, BusyAvailability::Staged);
            let payload = data.as_ref().ok_or(TestError::Missing("control payload"))?;
            assert_eq!(payload["ui_feedback"]["effective"], "next_session");
            assert!(text.contains(crate::i18n::tr_for(
                language,
                "operation_feedback.next_session",
                "Applies to the next session if the configuration is saved; see any save note below.",
            )), "{raw}: {text}");
            if raw.starts_with("/uia-worker-model") {
                assert_eq!(payload["ui_feedback"]["model"], "claude-mythos-preview");
                let warning = payload["ui_feedback"]["warning"]
                    .as_str()
                    .ok_or(TestError::Missing("lifecycle warning"))?;
                assert!(text.contains(warning), "{text}");
            } else {
                assert_eq!(payload["ui_feedback"]["level"], "high");
            }
            if let Some(note) = payload["ui_feedback"]["note"].as_str() {
                assert!(text.ends_with(note), "{text}");
            }
            let output_text = text.clone();
            let lines = crate::i18n::with_language(language, || {
                busy_queue::command_result_lines(command, text)
            });
            let expected_header = crate::i18n::tr_args_for(
                language,
                "dispatch.during_turn",
                "{command} (during turn)",
                &[("command", raw)],
            );
            assert_eq!(lines[0].to_string(), expected_header);
            assert_eq!(
                lines[1..]
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n"),
                output_text
            );
            app.language = harw_config::UiLanguage::Hindi;
            apply_busy_job_done(&mut app, done);
            let history = history_plain(&app);
            assert!(
                history.contains(&output_text),
                "history preserves completion text"
            );
            assert!(
                history.contains(&expected_header),
                "history preserves captured locale: {history}"
            );
            app.language = language;
        }
        assert_eq!(
            app.session_controller.snapshot(),
            before,
            "config-only controls must not stage live session state"
        );
    }
    Ok(())
}

/// Ein Picker-Accept während eines Turns (Effort-Auswahl) wird als
/// `Staged` eingestuft, läuft sofort und merkt die Änderung nur im
/// Controller vor — die Session selbst ändert sich erst an der
/// Turn-Grenze (`apply_pending_controller_state`).
#[tokio::test]
async fn busy_picker_accept_stages_controller_without_touching_session() -> TestResult {
    let sandbox = test_sandbox()?;
    let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
    let (turn_event_tx, _turn_event_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut session = AgentSession::new_with_id(
        SessionId::new(),
        AgentRole::Assistant,
        None,
        ExtensionRegistry::builder().build(),
        event_tx,
    )
    .with_spawn_context(test_spawn_context(&sandbox))
    .with_turn_event_sink(turn_event_tx);
    let mut app = ops_chat_app()?;
    app.active_cancel = Some(CancelToken::new());

    app.input.insert_str("/effort");
    let enter = TuiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(
        handle_busy_event(&mut app, enter.clone()),
        BusyKeyOutcome::Local
    );
    assert!(matches!(app.overlay, Some(Overlay::EffortChoice { .. })));

    assert_eq!(
        handle_busy_event(&mut app, enter),
        BusyKeyOutcome::Dispatch(BusyAvailability::Staged)
    );
    assert!(app.overlay.is_none());
    assert!(app.deferred_input.is_empty());
    let queued = app
        .busy_jobs
        .queued()
        .front()
        .cloned()
        .ok_or(TestError::Missing("eingereihter /effort-Befehl"))?;
    assert!(queued.raw.starts_with("/effort "), "{queued:?}");

    start_busy_work(&mut app);
    let done = tokio::time::timeout(Duration::from_secs(5), app.busy_jobs.recv())
        .await
        .map_err(ctx("Busy-Ergebnis"))?
        .ok_or(TestError::Missing("Busy-Ergebnis"))?;
    apply_busy_job_done(&mut app, done);

    let history = history_plain(&app);
    assert!(history.contains("(during turn)"), "{history}");
    assert!(
        history.contains("Reasoning effort (session): minimal."),
        "{history}"
    );
    assert!(app.session_controller.snapshot().reasoning_effort.is_some());
    // Die Session selbst ist unberührt, bis die Turn-Grenze anwendet.
    assert!(app.apply_pending_controller_state(&mut session));
    Ok(())
}

/// Testoperation, die vor ihrer Antwort wartet.
struct SlowOperation {
    meta: harw_operations::OperationMeta,
    delay: Duration,
    fail: bool,
}

impl SlowOperation {
    fn new(delay: Duration) -> Self {
        Self {
            meta: harw_operations::OperationMeta {
                name: "slow",
                summary: "Wartet und antwortet dann.",
                permission: harw_operations::PermissionTier::Observer,
                surfaces: vec![harw_operations::Surface::Command {
                    path: "/slow",
                    visibility: harw_operations::CommandVisibility::TuiOnly,
                }],
                busy: BusyAvailability::Immediate,
                ..harw_operations::OperationMeta::default()
            },
            delay,
            fail: false,
        }
    }
}

impl harw_operations::Operation for SlowOperation {
    fn meta(&self) -> &harw_operations::OperationMeta {
        &self.meta
    }

    fn run<'a>(
        &'a self,
        _ctx: &'a harw_operations::OpContext,
        _input: harw_operations::OpInput,
    ) -> harw_operations::OpFuture<'a> {
        Box::pin(async move {
            tokio::time::sleep(self.delay).await;
            if self.fail {
                return Err(harw_operations::OpError::Execution(
                    "invalid test choice".to_owned(),
                ));
            }
            Ok(OpOutput::from("langsam fertig".to_owned()))
        })
    }
}

fn slow_chat_app(delay: Duration) -> TestResult<ChatApp> {
    let adapters = CommandAdapter::from_operation(Arc::new(SlowOperation::new(delay)));
    let mut app = ChatApp::new(adapters, test_sandbox()?, SessionId::new());
    app.busy_jobs.enable_dispatch_without_runtime();
    Ok(app)
}

/// Ein langsamer Sofortbefehl blockiert die Busy-Schleife nicht: das
/// Starten kehrt sofort zurück, und ein Turn-Ereignis, das während der
/// Ausführung eintrifft, wird vor dem Befehlsergebnis bedient.
#[tokio::test]
async fn slow_immediate_command_does_not_stop_turn_events() -> TestResult {
    let mut app = slow_chat_app(Duration::from_millis(400))?;
    app.input.insert_str("/slow");
    assert_eq!(
        queue_busy_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        BusyKeyOutcome::Dispatch(BusyAvailability::Immediate)
    );
    let started = Instant::now();
    start_busy_work(&mut app);
    assert!(started.elapsed() < Duration::from_millis(200));
    assert_eq!(app.busy_jobs.running(), 1);

    let (turn_tx, mut turn_rx) = tokio::sync::mpsc::unbounded_channel::<u32>();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        let _ = turn_tx.send(7);
    });
    tokio::select! {
        event = turn_rx.recv() => assert_eq!(event, Some(7)),
        _ = app.busy_jobs.recv() => {
            return Err(TestError::Unexpected(
                "der langsame Befehl darf das Turn-Ereignis nicht überholen".to_owned(),
            ));
        }
    }

    let done = tokio::time::timeout(Duration::from_secs(5), app.busy_jobs.recv())
        .await
        .map_err(ctx("Busy-Ergebnis"))?
        .ok_or(TestError::Missing("Busy-Ergebnis"))?;
    apply_busy_job_done(&mut app, done);
    assert_eq!(app.busy_jobs.running(), 0);
    let history = history_plain(&app);
    assert!(history.contains("/slow (during turn)"), "{history}");
    assert!(history.contains("langsam fertig"), "{history}");
    Ok(())
}

/// Überschreitet ein Befehl das Zeitlimit, meldet der Busy-Pfad das
/// statt ewig zu warten.
#[tokio::test]
async fn busy_command_timeout_is_reported() -> TestResult {
    let mut app = slow_chat_app(Duration::from_secs(5))?;
    app.busy_jobs.set_timeout(Duration::from_millis(50));
    app.busy_jobs.queue(BusyCommand {
        raw: "/slow".to_owned(),
        class: BusyAvailability::Immediate,
    });
    start_busy_work(&mut app);
    let done = tokio::time::timeout(Duration::from_secs(5), app.busy_jobs.recv())
        .await
        .map_err(ctx("Busy-Ergebnis"))?
        .ok_or(TestError::Missing("Busy-Ergebnis"))?;
    apply_busy_job_done(&mut app, done);
    assert!(history_plain(&app).contains("Time limit exceeded"));
    Ok(())
}

/// Befund 2026-09-28 (Redraw-Reste in tmux/Handy-Terminals): `Ctrl+L`
/// merkt ein vollständiges Neuzeichnen vor, im Leerlauf wie während eines
/// Turns; `draw_viewport` verbraucht die Markierung genau einmal.
#[test]
fn ctrl_l_requests_a_full_redraw() -> TestResult {
    let mut app = test_chat_app()?;
    let (bus, _receiver) = harw_event_channel();
    assert!(!app.full_redraw.get());
    let ctrl_l = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL);
    assert!(handle_key(&mut app, ctrl_l, &bus));
    assert!(app.full_redraw.take(), "idle Ctrl+L marks a full redraw");
    assert!(!app.full_redraw.get(), "the mark is consumed once");

    assert!(matches!(
        handle_busy_event(&mut app, TuiEvent::Key(ctrl_l)),
        BusyKeyOutcome::Redraw
    ));
    assert!(app.full_redraw.get(), "busy Ctrl+L marks a full redraw too");
    Ok(())
}

/// Review #60 (P1): die Host-Warnung steht vorne mit fest reserviertem
/// Platz und bleibt neben Plan- oder Goal-Marke bei 40, 45 und 55 Spalten
/// vollständig im gerenderten Buffer; Plan/Goal werden gekürzt.
#[test]
fn host_warning_stays_complete_next_to_plan_and_goal_marks() -> TestResult {
    let theme = crate::style::Theme::Dark;
    let host_text = |width: u16| {
        if width < STATUS_HOST_HINT_MIN_COLS {
            HOST_MODE_MARK_SHORT
        } else {
            HOST_MODE_MARK
        }
    };
    let plan = || {
        let full = crate::plan_dialog::plan_status_span(theme);
        let short = Span::styled(tr("chrome.plan_short", PLAN_MARK_SHORT), full.style);
        (full, short)
    };
    let goal = || {
        vec![Span::raw(
            " ◎ Goal: Auf aktueller dev-Basis einen sicheren Reviewer bauen · 1/5 ",
        )]
    };
    for width in [40u16, 45, 55] {
        for (label, with_plan, with_goal) in [
            ("host+plan", true, false),
            ("host+goal", false, true),
            ("host+plan+goal", true, true),
        ] {
            let (spans, used) = status_mark_spans(
                Some(Span::raw(host_text(width))),
                None,
                with_plan.then(plan),
                if with_goal { goal() } else { Vec::new() },
                usize::from(width),
            );
            assert!(used <= usize::from(width), "{label} at {width}: {used}");
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 1))
                    .map_err(ctx("test terminal"))?;
            terminal
                .draw(|frame| {
                    frame
                        .render_widget(Paragraph::new(Line::from(spans.clone())), frame.area());
                })
                .map_err(ctx("draw"))?;
            let buffer = terminal.backend().buffer();
            let row: String = (0..width).map(|x| buffer[(x, 0)].symbol()).collect();
            assert!(
                row.contains("HOST MODE ACTIVE"),
                "{label} at {width} columns lost the host warning: {row:?}"
            );
            if with_plan {
                assert!(
                    row.contains('⏸'),
                    "{label} at {width}: plan mark gone: {row:?}"
                );
            }
        }
    }
    Ok(())
}

/// PL-97 D5: die DoD-Marke steht direkt hinter der Host-Warnung und
/// überlebt 40, 45 und 55 Spalten (notfalls in Kurzform), auch neben
/// Plan- und Goal-Marke; die Host-Warnung bleibt vollständig.
#[test]
fn dod_mark_survives_narrow_widths_after_the_host_warning() -> TestResult {
    let theme = crate::style::Theme::Dark;
    let dod = || {
        let (full, short) = status_line::dod_mark_text("≥ 12", false);
        (Span::raw(full), Span::raw(short))
    };
    let plan = || {
        let full = crate::plan_dialog::plan_status_span(theme);
        let short = Span::styled(tr("chrome.plan_short", PLAN_MARK_SHORT), full.style);
        (full, short)
    };
    let goal = || {
        vec![Span::raw(
            " ◎ Goal: Auf aktueller dev-Basis einen sicheren Reviewer bauen · 1/5 ",
        )]
    };
    for width in [40u16, 45, 55] {
        let host = if width < STATUS_HOST_HINT_MIN_COLS {
            HOST_MODE_MARK_SHORT
        } else {
            HOST_MODE_MARK
        };
        for (with_plan, with_goal) in [(false, false), (true, false), (true, true)] {
            let (spans, used) = status_mark_spans(
                Some(Span::raw(host)),
                Some(dod()),
                with_plan.then(plan),
                if with_goal { goal() } else { Vec::new() },
                usize::from(width),
            );
            assert!(used <= usize::from(width), "{width}: {used}");
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 1))
                    .map_err(ctx("test terminal"))?;
            terminal
                .draw(|frame| {
                    frame
                        .render_widget(Paragraph::new(Line::from(spans.clone())), frame.area());
                })
                .map_err(ctx("draw"))?;
            let buffer = terminal.backend().buffer();
            let row: String = (0..width).map(|x| buffer[(x, 0)].symbol()).collect();
            assert!(row.contains("HOST MODE ACTIVE"), "{width}: {row:?}");
            let after_host = row
                .split_once("HOST MODE ACTIVE")
                .map_or("", |(_, rest)| rest);
            assert!(
                after_host.contains("⛨ ≥ 12"),
                "{width} plan={with_plan} goal={with_goal}: DoD mark lost: {row:?}"
            );
        }
    }
    Ok(())
}

/// PL-97 D5: Vorrang in der Markenzeile (`status_mark_spans`, ersetzt
/// das `fit_segments`-Segment): wird es eng, fällt zuerst das Goal, dann
/// kürzt bzw. fällt die Plan-Marke, die DoD-Marke behält mindestens ihre
/// Kurzform; die Host-Warnung bleibt immer vollständig.
#[test]
fn status_mark_spans_cut_goal_then_plan_before_the_dod_mark() {
    let theme = crate::style::Theme::Dark;
    let (dod_full, dod_short) = status_line::dod_mark_text("3", true);
    let plan_full = crate::plan_dialog::plan_status_span(theme);
    let plan_short = Span::styled(PLAN_MARK_SHORT, plan_full.style);
    let goal = " ◎ Goal: Auf aktueller dev-Basis einen sicheren Reviewer bauen · 1/5 ";
    let host = HOST_MODE_MARK_SHORT;
    let w = |text: &str| status_line::display_width(text);
    let (hw, dfw, dsw) = (w(host), w(&dod_full), w(&dod_short));
    let (pfw, psw) = (plan_full.width(), plan_short.width());
    assert!(psw > 0 && psw < pfw && dsw < dfw);
    let row = |width: usize| {
        let (spans, used) = status_mark_spans(
            Some(Span::raw(host)),
            Some((Span::raw(dod_full.clone()), Span::raw(dod_short.clone()))),
            Some((plan_full.clone(), plan_short.clone())),
            vec![Span::raw(goal)],
            width,
        );
        assert!(used <= width, "{width}: {used}");
        let texts: Vec<String> = spans.iter().map(|span| span.content.to_string()).collect();
        texts
    };
    let plan_full_text = plan_full.content.to_string();
    // Alles passt bis auf das Goal: es wird gekürzt, nichts sonst.
    let texts = row(hw + dfw + pfw + 5);
    assert_eq!(
        texts[..3],
        [host.to_owned(), dod_full.clone(), plan_full_text.clone()]
    );
    assert_eq!(texts.len(), 4, "{texts:?}");
    assert!(w(&texts[3]) <= 5 && texts[3] != goal, "{texts:?}");
    // Kein Platz fürs Goal und die volle Plan-Marke: Plan in Kurzform.
    let texts = row(hw + dfw + psw);
    assert_eq!(
        texts,
        [
            host.to_owned(),
            dod_full.clone(),
            PLAN_MARK_SHORT.to_owned()
        ]
    );
    // Noch enger: die Plan-Marke fällt (der Rest bleibt dem gekürzten
    // Goal), die DoD-Marke bleibt voll.
    let texts = row(hw + dfw + psw - 1);
    assert_eq!(texts[..2], [host.to_owned(), dod_full.clone()]);
    assert!(!texts.iter().any(|text| text.contains("plan")), "{texts:?}");
    // Am engsten: die DoD-Marke in Kurzform.
    let texts = row(hw + dsw);
    assert_eq!(texts, [host.to_owned(), dod_short.clone()]);
}

/// PL-97 D5: ohne installiertes Harw-DoD (oder mit unlesbarem Export)
/// bleibt die Marke weg; mit hohen Befunden steht sie in der Statuszeile
/// und im Titel des Finished-/Failed-Fensters.
#[test]
fn dod_mark_is_hidden_when_dod_is_not_installed() -> TestResult {
    let mut app = test_chat_app()?;
    let ok = |data: serde_json::Value| -> Result<OpOutput, String> {
        Ok(OpOutput {
            text: String::new(),
            data: Some(data),
        })
    };
    let draw = |app: &ChatApp| -> TestResult<String> {
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30))
            .map_err(ctx("test terminal"))?;
        terminal
            .draw(|frame| render_viewport(frame, app, &Spinner::new(), None))
            .map_err(ctx("draw"))?;
        Ok(crate::overlay_view::buffer_text(
            terminal.backend().buffer(),
        ))
    };
    let mut not_installed = crate::dod_view::tests::status_fixture(4, true);
    not_installed["installed"] = serde_json::json!(false);
    not_installed["attention"] = serde_json::Value::Null;
    app.dod_mark.apply(&ok(not_installed));
    assert_eq!(app.dod_mark.visible(), None);
    assert!(!draw(&app)?.contains('⛨'));
    app.dod_mark
        .apply(&Err("Unbekannter Command: /dod".to_owned()));
    assert!(!draw(&app)?.contains('⛨'));

    assert!(
        app.dod_mark
            .apply(&ok(crate::dod_view::tests::status_fixture(4, true)))
    );
    let shown = draw(&app)?;
    assert!(shown.contains("⛨ 4 high"), "{shown}");
    Ok(())
}

/// PL-97 D5: `/dod view` öffnet die DoD-Ansicht, reiht ihre drei Abrufe
/// über den regulären Datenpfad ein und zeigt Units, Sensoren und Befunde
/// aus den Ergebnissen — nichts davon landet im Chat.
#[tokio::test]
async fn dod_view_command_opens_the_overlay_and_renders_fixture_data() -> TestResult {
    let mut app = test_chat_app()?;
    let (bus, _receiver) = harw_event_channel();
    let intercepted =
        local_intercept_for(&app, "/dod view").ok_or(TestError::Missing("dod view"))?;
    assert!(local_intercept_for(&app, "/dod status").is_none());
    apply_local_intercept(&mut app, intercepted, &bus);
    assert!(matches!(app.overlay, Some(Overlay::View(_))));
    let generation = app.overlay_generation;
    let queued: Vec<String> = app
        .pending_fetches
        .iter()
        .filter_map(|fetch| match fetch {
            DataFetch::Overlay {
                command,
                generation: queued,
            } if *queued == generation => Some(command.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        queued,
        vec![
            crate::dod_view::REFRESH_COMMAND.to_owned(),
            crate::dod_view::SENSORS_COMMAND.to_owned(),
            crate::dod_view::findings_command(),
        ]
    );
    let cells = app.cells_len();
    for data in [
        crate::dod_view::tests::status_fixture(1, true),
        crate::dod_view::tests::sensors_fixture(true),
        crate::dod_view::tests::findings_fixture(false),
    ] {
        apply_fetch_result(
            &mut app,
            FetchTarget::Overlay { generation },
            Ok(OpOutput {
                text: String::new(),
                data: Some(data),
            }),
        );
    }
    assert_eq!(
        app.cells_len(),
        cells,
        "Datenabruf schreibt nie in den Chat"
    );
    let Some(Overlay::View(view)) = app.overlay.as_ref() else {
        return Err(TestError::Missing("dod overlay"));
    };
    let area = Rect::new(0, 0, 110, 40);
    let mut buf = ratatui::buffer::Buffer::empty(area);
    view.render(area, &mut buf, app.theme);
    let text = crate::overlay_view::buffer_text(&buf);
    for expected in [
        "Harw-DoD",
        "UNITS",
        "harw-sentinel.service",
        "SENSORS",
        "tcp-connect",
        "FINDINGS",
        "UNVERIFIED source (home)",
        "egress.unexpected",
    ] {
        assert!(text.contains(expected), "missing {expected:?}: {text}");
    }
    Ok(())
}

/// Befund 2026-09-28 (Hochformat): auf schmalen Terminals bekommt eine
/// Marke (hier die Plan-Marke) eine eigene Statuszeile, die Segmente
/// darunter behalten die volle Breite; breit bleibt es eine Zeile.
#[test]
fn narrow_status_line_moves_marks_to_their_own_row() -> TestResult {
    let mut app = test_chat_app()?;
    app.active_mode = InteractionMode::Plan;
    assert_eq!(status_rows_for(&app, app.theme, 50), 2);
    assert_eq!(status_rows_for(&app, app.theme, 120), 1);
    app.active_mode = InteractionMode::Chat;
    assert_eq!(status_rows_for(&app, app.theme, 50), 1);
    app.active_mode = InteractionMode::Plan;

    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(50, 30))
        .map_err(ctx("test terminal"))?;
    terminal
        .draw(|frame| render_viewport(frame, &app, &Spinner::new(), None))
        .map_err(ctx("draw"))?;
    let buffer = terminal.backend().buffer();
    let rows: Vec<String> = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect();
    let marker = crate::plan_dialog::PLAN_STATUS_MARKER;
    let mark_row = rows
        .iter()
        .position(|row| row.contains(marker))
        .ok_or_else(|| rows.join("\n"))
        .map_err(ctx("plan marker missing"))?;
    let segments = rows
        .get(mark_row + 1)
        .ok_or("no row")
        .map_err(ctx("row below the marks"))?;
    assert!(
        segments.contains("Plan") && !segments.contains(marker),
        "segments must get their own full-width row:\n{}",
        rows.join("\n")
    );
    Ok(())
}

/// Der Warteschlangen-Block steht über dem Composer, solange Nachrichten
/// warten, und verschwindet, sobald sie ausgeliefert sind.
#[test]
fn queue_block_renders_until_delivery() -> TestResult {
    let mut app = test_chat_app()?;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 20))
        .map_err(ctx("test terminal"))?;
    let screen = |terminal: &ratatui::Terminal<ratatui::backend::TestBackend>| {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    app.pending_turns
        .push_back("bitte danach prüfen".to_owned());
    terminal
        .draw(|frame| render_viewport(frame, &app, &Spinner::new(), None))
        .map_err(ctx("draw"))?;
    let with_queue = screen(&terminal);
    assert!(
        with_queue.contains("Wartet auf den nächsten Turn"),
        "{with_queue}"
    );
    assert!(with_queue.contains("bitte danach prüfen"), "{with_queue}");

    // Auslieferung an der Turn-Grenze (`run_loop` nimmt sie aus der FIFO).
    let delivered = app.pending_turns.pop_front();
    assert_eq!(delivered.as_deref(), Some("bitte danach prüfen"));
    terminal
        .draw(|frame| render_viewport(frame, &app, &Spinner::new(), None))
        .map_err(ctx("draw"))?;
    let after = screen(&terminal);
    assert!(!after.contains("Wartet auf den nächsten Turn"), "{after}");
    Ok(())
}

/// Folgen nur am Ende: hochgescrollt verschiebt neuer Inhalt den
/// sichtbaren Verlauf nicht (Hinweis zählt die neuen Zeilen), eine eigene
/// Nachricht springt ans Ende und folgt wieder.
#[test]
fn scrolled_up_history_stays_put_and_own_message_resumes_follow() -> TestResult {
    let mut app = test_chat_app()?;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 20))
        .map_err(ctx("test terminal"))?;
    let screen = |terminal: &ratatui::Terminal<ratatui::backend::TestBackend>| {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let first_line = |shown: &str| -> Option<String> {
        shown.find("Zeile-").map(|at| shown[at..at + 8].to_owned())
    };
    for index in 0..60 {
        app.push_line(Role::System, format!("Zeile-{index:02}"));
    }
    terminal
        .draw(|frame| render_viewport(frame, &app, &Spinner::new(), None))
        .map_err(ctx("draw"))?;
    assert!(app.scroll.is_following());
    assert!(screen(&terminal).contains("Zeile-59"));

    assert_eq!(
        handle_busy_event(
            &mut app,
            TuiEvent::Key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))
        ),
        BusyKeyOutcome::Redraw
    );
    terminal
        .draw(|frame| render_viewport(frame, &app, &Spinner::new(), None))
        .map_err(ctx("draw"))?;
    let before = screen(&terminal);
    let first_before = first_line(&before).ok_or(TestError::Missing("Zeile"))?;

    for index in 60..65 {
        app.push_line(Role::System, format!("Zeile-{index:02}"));
    }
    terminal
        .draw(|frame| render_viewport(frame, &app, &Spinner::new(), None))
        .map_err(ctx("draw"))?;
    let after = screen(&terminal);
    assert_eq!(first_line(&after), Some(first_before), "{after}");
    assert!(!after.contains("Zeile-64"), "{after}");
    assert!(
        after.contains("5 new lines · Ctrl+End jumps to end"),
        "{after}"
    );

    // Eigene Nachricht während des Turns: ans Ende, wieder folgen.
    app.input.insert_str("weiter");
    assert_eq!(
        queue_busy_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        BusyKeyOutcome::Redraw
    );
    assert!(app.scroll.is_following());
    terminal
        .draw(|frame| render_viewport(frame, &app, &Spinner::new(), None))
        .map_err(ctx("draw"))?;
    let tail = screen(&terminal);
    assert!(tail.contains("Zeile-64"), "{tail}");
    assert!(!tail.contains("jumps to end"), "{tail}");
    Ok(())
}

/// Strg+Ende springt hochgescrollt auch bei nicht-leerem Composer ans
/// Ende; bares Ende nur bei leerem Composer.
#[test]
fn end_keys_jump_to_bottom_while_scrolled_up() -> TestResult {
    let mut app = test_chat_app()?;
    app.last_history_total_lines.set(100);
    app.last_history_visible_rows.set(10);
    let ctrl_end = KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL);
    let end = KeyEvent::new(KeyCode::End, KeyModifiers::NONE);

    app.input.insert_str("halb getippt");
    assert!(
        !scroll_claims_key(&app, &ctrl_end),
        "am Ende bewegt Strg+Ende bei Text weiter den Composer-Cursor"
    );
    app.scroll.scroll_up(5, 100, 10);
    assert!(scroll_claims_key(&app, &ctrl_end));
    assert!(
        !scroll_claims_key(&app, &end),
        "Ende gehört bei Text dem Composer"
    );

    app.input.clear();
    assert!(scroll_claims_key(&app, &end));
    assert_eq!(app.scroll.handle_key(end, 100, 10), ScrollAction::Redraw);
    assert!(app.scroll.is_following());
    Ok(())
}

#[test]
fn workbench_command_detection() {
    assert!(is_workbench_command("/workbench"));
    assert!(is_workbench_command("  /workbench note x"));
    assert!(!is_workbench_command("/workbenches"));
    assert!(!is_workbench_command("workbench"));
}

/// Der rückgerechnete Cursor-Offset stimmt mit dem echten überein.
#[test]
fn editor_cursor_byte_matches_editor_cursor() {
    let mut editor = InputEditor::new();
    editor.insert_str("ab\nc@dé x");
    assert_eq!(editor_cursor_byte(&editor), editor.cursor());
    for _ in 0..4 {
        editor.move_left();
        assert_eq!(editor_cursor_byte(&editor), editor.cursor());
    }
}

/// `@`-Popup öffnet mit Rollen; die Übernahme ersetzt das Token unter dem
/// Cursor und erhält den Rest der Zeile.
#[test]
fn mention_popup_opens_and_accept_replaces_token() -> TestResult {
    let mut app = test_chat_app()?;
    let role = KNOWN_ROLES
        .first()
        .ok_or(TestError::Missing("bekannte Rolle"))?;
    app.input.insert_str("@");
    app.sync_popup();
    assert!(
        app.mention_popup
            .as_ref()
            .is_some_and(|popup| !popup.is_empty())
    );

    app.input.clear();
    app.input.insert_str("a @ex b");
    app.input.move_left();
    app.input.move_left();
    app.accept_mention(role);
    assert_eq!(app.input(), format!("a @{role} b"));
    assert_eq!(app.input.cursor(), format!("a @{role}").len());
    assert!(app.mention_popup.is_none());

    app.input.clear();
    app.input.insert_str("schau @src/ma");
    app.accept_mention("src/main.rs");
    assert_eq!(app.input(), "schau @src/main.rs ");
    Ok(())
}

#[test]
fn expand_mentions_without_root_keeps_text() {
    let (text, note) = expand_mentions_for_turn("", "hallo @datei.rs");
    assert_eq!(text, "hallo @datei.rs");
    assert!(note.is_none());
}

/// `Cancel` (Esc) auf [`Overlay::ModelSwitch`] schließt das Overlay ohne
/// eine Befehlszeile zu emittieren.
#[test]
fn model_switch_cancel_closes_overlay_without_emitting_a_command() -> TestResult {
    let providers = vec![ProviderEntry {
        id: "anthropic".to_owned(),
        label: "Anthropic".to_owned(),
    }];
    let models = vec![(
        "anthropic".to_owned(),
        vec![ModelEntry {
            id: "claude-sonnet".to_owned(),
            label: "Claude Sonnet".to_owned(),
        }],
    )];
    let picker =
        ModelSwitchPicker::new(PickerTarget::Orchestrator, providers, models, None, None)
            .ok_or(TestError::Missing("providers fixture ist nicht leer"))?;

    let mut app = test_chat_app()?;
    app.overlay = Some(Overlay::ModelSwitch(Box::new(picker)));
    let (bus, mut receiver) = harw_event_channel();

    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &bus,
    );

    assert!(app.overlay.is_none());
    assert!(matches!(
        receiver.try_recv(),
        Err(tokio::sync::mpsc::error::TryRecvError::Empty)
    ));
    Ok(())
}

/// Bare `/effort`/`/uia-effort` öffnet [`Overlay::EffortChoice`] mit
/// allen sechs Stufen plus dem Reset-Eintrag; die Vorauswahl folgt dem
/// Controller-Snapshot (`Session`) bzw. bleibt ohne Konfiguration auf
/// Index 0 (`Uia`, kein persistierter Wert im Test-`ChatApp`).
#[test]
fn open_effort_choice_lists_all_levels_with_reset_entry() -> TestResult {
    let mut app = test_chat_app()?;
    app.session_controller
        .set_reasoning_effort(Some(ReasoningEffort::High))
        .map_err(ctx("set_reasoning_effort muss gelingen"))?;

    app.open_effort_choice(EffortTarget::Session);
    match &app.overlay {
        Some(Overlay::EffortChoice { target, .. }) => {
            assert_eq!(*target, EffortTarget::Session);
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "erwartete Overlay::EffortChoice, bekam {other:?}"
            )));
        }
    }

    app.open_effort_choice(EffortTarget::Uia);
    assert!(matches!(
        app.overlay,
        Some(Overlay::EffortChoice {
            target: EffortTarget::Uia,
            ..
        })
    ));
    Ok(())
}

/// Eine getroffene Effort-Wahl sendet `/effort <level>` bzw.
/// `/uia-effort <level>`; der letzte Eintrag („Provider-Default
/// (zurücksetzen)") sendet `clear` statt einer Stufe.
#[test]
fn effort_choice_accept_emits_level_or_clear_per_target() -> TestResult {
    // Session: dritte Stufe (Index 2 = "medium") direkt bestätigen.
    let mut app = test_chat_app()?;
    app.open_effort_choice(EffortTarget::Session);
    let (bus, mut receiver) = harw_event_channel();
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &bus,
    );
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        &bus,
    );
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &bus,
    );
    assert!(app.overlay.is_none());
    match receiver.try_recv() {
        Ok(HarwEvent::Command(command)) => assert_eq!(command, "/effort medium"),
        other => {
            return Err(TestError::Unexpected(format!(
                "erwartete HarwEvent::Command(\"/effort medium\"), bekam {other:?}"
            )));
        }
    }

    // Uia: letzten Eintrag (Reset) wählen → `clear`.
    let mut app = test_chat_app()?;
    app.open_effort_choice(EffortTarget::Uia);
    let (bus, mut receiver) = harw_event_channel();
    for _ in 0..EFFORT_LEVELS.len() {
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &bus,
        );
    }
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &bus,
    );
    assert!(app.overlay.is_none());
    match receiver.try_recv() {
        Ok(HarwEvent::Command(command)) => assert_eq!(command, "/uia-effort clear"),
        other => {
            return Err(TestError::Unexpected(format!(
                "erwartete HarwEvent::Command(\"/uia-effort clear\"), bekam {other:?}"
            )));
        }
    }
    Ok(())
}

/// Befund 2026-09-28: ein in den Hintergrund gegebener Orchestrator
/// meldet nach dem Ende des startenden Turns nur noch über den Agenten-Hub.
/// Seine Verlaufszelle muss trotzdem mitzählen statt bei „0 Tools" zu
/// stehen; ein verspätetes niedrigeres Ereignis zählt nie rückwärts.
#[test]
fn background_child_cell_follows_hub_progress() -> TestResult {
    let mut app = test_chat_app()?;
    let child = "child-bg-1";
    let cell = Arc::new(Mutex::new(SubAgentCell {
        language: crate::i18n::current_language(),
        child_id: child.to_owned(),
        role: "root-orchestrator".to_owned(),
        question: Some("Review".to_owned()),
        tool_calls: 0,
        tokens: 0,
        status: SubAgentStatus::Running,
    }));
    app.child_cells.insert(child.to_owned(), Arc::clone(&cell));
    assert!(!app.sync_child_cell(child), "no monitor entry yet");

    dock_test_running_agent(&mut app, child, "root-orchestrator");
    let completed = || harw_core::AgentEvent {
        agent: SessionId::from_str(child),
        parent: None,
        role: "root-orchestrator".to_owned(),
        kind: harw_core::AgentEventKind::Turn(TurnEvent::ToolCallCompleted {
            turn_id: harw_types::TurnId::new(),
            call_id: harw_types::ToolCallId::new(),
            result: harw_protocol::items::ToolCallResult::success(serde_json::json!({})),
            duration_ms: 3,
            placement: None,
        }),
    };
    app.agent_monitor.apply(&completed());
    app.agent_monitor.apply(&completed());
    assert!(app.sync_child_cell(child));
    let calls = cell.lock().map_err(ctx("cell lock"))?.tool_calls;
    assert_eq!(calls, 2);

    // Höherer Stand aus `ChildProgress` bleibt erhalten.
    cell.lock().map_err(ctx("cell lock"))?.apply_progress(5, 0);
    assert!(!app.sync_child_cell(child), "never counts backwards");
    let calls = cell.lock().map_err(ctx("cell lock"))?.tool_calls;
    assert_eq!(calls, 5);
    app.agent_monitor.apply(&harw_core::AgentEvent {
        agent: SessionId::from_str(child),
        parent: None,
        role: "root-orchestrator".to_owned(),
        kind: harw_core::AgentEventKind::Turn(TurnEvent::TurnFailed {
            turn_id: harw_types::TurnId::new(),
            reason: "provider failure".to_owned(),
            retryable: false,
        }),
    });
    assert!(
        app.sync_child_cell(child),
        "terminal status changes without new counters"
    );
    assert!(matches!(&cell.lock().map_err(ctx("cell lock"))?.status,
        SubAgentStatus::Done { outcome, .. } if outcome == "failed"));
    assert!(
        !app.sync_child_cell(child),
        "terminal synchronization is idempotent"
    );
    Ok(())
}

/// Baut ein `TurnEvent::ContextUpdated` für einen laufenden Kind-Agenten
/// (gleiches Muster wie `scroll_routing::tests::running`), damit der Dock
/// mindestens einen aktiven Agenten anzeigt.
fn dock_test_running_agent(app: &mut ChatApp, id: &str, role: &str) {
    let event = harw_core::AgentEvent {
        agent: SessionId::from_str(id),
        parent: Some(app.session_id().clone()),
        role: role.to_owned(),
        kind: harw_core::AgentEventKind::Turn(TurnEvent::ContextUpdated {
            turn_id: harw_types::TurnId::new(),
            used_tokens: 22_280,
            window_tokens: 202_800,
            history_items_dropped: 0,
            estimated_next_tokens: None,
            threshold_tokens: None,
            reserve_tokens: None,
        }),
    };
    app.agent_monitor.apply(&event);
}

/// Legt einen einzelnen Job im Agenten-Monitor ab, damit die geteilte
/// Dock-Spalte (Agenten links, Jobs rechts) auch eine Job-Zeile zeigt.
fn dock_test_job() -> crate::jobs_panel::JobRow {
    crate::jobs_panel::JobRow {
        id: "job-1".to_owned(),
        name: "build".to_owned(),
        state: harw_tool_job::JobState::Running,
        suspended: false,
        suspension_unresolved: false,
        progress: "13%".to_owned(),
        runtime: "1m15s".to_owned(),
        active: true,
        failed: false,
        reason: None,
        reason_source: None,
        origin: None,
        cgroup: None,
        ended_at: None,
        child_link: crate::jobs_panel::ChildRunLink::NotAChild,
    }
}

/// 80×40 (schmal, hoch): `render_viewport` pinnt den Portrait-Dock oben
/// mit vollem Rahmen und Titel, der Chat läuft darunter über die volle
/// Breite, und Statuszeile sowie Composer bleiben am unteren Rand — die
/// gleiche Reihenfolge, die `harw-tui-layout::classify` für dieses Format
/// vorschreibt (`Placement::PortraitDock`).
/// Copilot-Review #48: eine lange Warteschlange darf im Portrait-Dock
/// Statuszeile und Composer nicht aus dem Bild schieben, denn die
/// Dock-Höhe ist schon fest vergeben.
/// Befund 2026-09-28: eine schmale, hohe tmux-Pane (hier 45×60, gemessen
/// am Hochkant-Monitor ≈ 45–55 Spalten) zeigt den Agenten-Dock oben mit
/// dem laufenden Agenten statt nur der einzeiligen Zusammenfassung.
#[test]
fn narrow_tall_pane_shows_the_agent_dock() -> TestResult {
    let mut app = test_chat_app()?;
    dock_test_running_agent(&mut app, "w1", "root-orchestrator");
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(45, 60))
        .map_err(ctx("test terminal"))?;
    terminal
        .draw(|frame| render_viewport(frame, &app, &Spinner::new(), None))
        .map_err(ctx("draw"))?;
    let buffer = terminal.backend().buffer();
    let shown: String = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n");
    let dock = app.last_regions.get().agents;
    assert!(
        dock.is_some_and(|dock| dock.y == 0 && dock.width == 22),
        "portrait dock expected at the top:\n{shown}"
    );
    // Die Rolle muss im Dock selbst stehen (im halbbreiten Fenster
    // gekürzt), nicht nur im Reasoning-Fenster darunter.
    let dock_height = dock.map_or(0, |dock| usize::from(dock.height));
    assert!(
        shown
            .lines()
            .take(dock_height)
            .any(|row| row.contains("root-or")),
        "{shown}"
    );
    Ok(())
}

/// Zeichnet `app` auf einem Testterminal und liefert die Zeilen als Text.
fn rendered_rows(app: &ChatApp, width: u16, height: u16) -> TestResult<Vec<String>> {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
            .map_err(ctx("test terminal"))?;
    terminal
        .draw(|frame| render_viewport(frame, app, &Spinner::new(), None))
        .map_err(ctx("draw"))?;
    let buffer = terminal.backend().buffer();
    Ok((0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect())
}

#[test]
fn footer_counts_row_is_opt_in_and_sits_below_the_composer() -> TestResult {
    let mut app = test_chat_app()?;
    let off = rendered_rows(&app, 80, 24)?;
    assert!(
        off.iter().all(|row| !row.contains("Agenten aktiv")),
        "Fußzeile ist standardmäßig aus"
    );
    app = app.with_footer_counts(true);
    let on = rendered_rows(&app, 80, 24)?;
    let last = on.last().map(String::as_str).unwrap_or_default();
    assert!(
        last.contains("Agenten aktiv 0 · fehlgeschlagen 0"),
        "{last}"
    );
    Ok(())
}

/// Portrait-Dock (46×78): unter Agenten/Jobs und dem Finished/Failed-
/// Streifen steht das gerahmte Reasoning-Fenster (10 Zeilen) mit dem
/// gemischten Strom aller Agenten, neueste Zeile unten; die Recency-Slots
/// des alten LIVE-Streifens gibt es dort nicht mehr. Der Chat beginnt
/// erst darunter.
#[test]
fn portrait_dock_shows_the_reasoning_window_below_the_dock_at_46x78() -> TestResult {
    let mut app = test_chat_app()?;
    dock_test_running_agent(&mut app, "w1", "uia-worker");
    app.reasoning_feed
        .push_reasoning("w1", "uia-worker", "erste Meldung");
    app.reasoning_feed
        .push_tool_start("w1", "uia-worker", "fs.read");
    app.push_line(
        Role::System,
        "einzigartige-verlaufszeile-im-chat".to_owned(),
    );
    let rows = rendered_rows(&app, 46, 78)?;
    let shown = rows.join("\n");
    let row_at = |index: usize| rows.get(index).map(String::as_str).unwrap_or_default();

    let live = app
        .last_regions
        .get()
        .live
        .ok_or(TestError::Missing("Reasoning-Fenster"))?;
    assert_eq!(live, Rect::new(0, 12, 46, 10), "{shown}");
    let header = usize::from(live.y);
    assert!(
        row_at(header).contains("Reasoning · 1 active"),
        "Kopfzeile fehlt unter dem Dock:\n{shown}"
    );
    assert!(
        row_at(header + 1).contains("uia-worker")
            && row_at(header + 1).contains("∴ erste Meldung"),
        "Reasoning-Zeile fehlt:\n{shown}"
    );
    assert!(
        row_at(header + 2).contains("● fs.read · running"),
        "Werkzeugzeile fehlt:\n{shown}"
    );
    let history_row = rows
        .iter()
        .position(|row| row.contains("einzigartige-verlaufszeile"))
        .ok_or(TestError::Missing("Verlaufszeile"))?;
    assert!(
        history_row > header + 10,
        "der Chat beginnt erst unter Fenster und Finished-Streifen:\n{shown}"
    );
    Ok(())
}

/// Befund P2 (Review): Im Betrieb ist die Agentenkennung eine 36 Zeichen
/// lange Sitzungs-UUID. Das Reasoning-Fenster zeigt die Rolle (bis 12
/// Spalten) statt der UUID, und der Meldung bleibt der Rest der Zeile.
#[test]
fn portrait_reasoning_shows_the_role_not_the_session_id_at_40_cols() -> TestResult {
    const SESSION_ID: &str = "0196f3a2-7b1e-4c8d-9a35-5e2f8d41b6c7";
    let mut app = test_chat_app()?;
    dock_test_running_agent(&mut app, "w1", "uia-worker");
    app.reasoning_feed.push_reasoning(
        SESSION_ID,
        "uia-writer",
        "Datei geschrieben und alle Tests gruen",
    );
    let rows = rendered_rows(&app, 40, 60)?;
    let shown = rows.join("\n");
    let dock = app
        .last_regions
        .get()
        .agents
        .ok_or(TestError::Missing("Portrait-Dock"))?;
    assert_eq!(dock.width, 20, "Agentenfenster = halbe Breite:\n{shown}");
    assert!(
        !shown.contains(SESSION_ID) && !shown.contains("0196f3a2"),
        "die Sitzungs-ID gehört nicht ins Bild:\n{shown}"
    );
    let line = rows
        .iter()
        .find(|row| row.contains("uia-writer"))
        .ok_or(TestError::Missing("Reasoning-Zeile mit der Rolle"))?;
    assert!(
        line.contains("uia-writer") && line.contains("∴ Datei geschrieben und"),
        "Zeile unerwartet: {line:?}"
    );
    Ok(())
}

/// Ohne Agentenmeldungen erfindet das Reasoning-Fenster nichts: es zeigt
/// seinen Rahmen mit „0 active“ und den Hinweis „no visible events yet“,
/// der Chat folgt unter dem Finished/Failed-Streifen.
#[test]
fn portrait_reasoning_window_without_events_says_so() -> TestResult {
    let mut app = test_chat_app()?;
    dock_test_running_agent(&mut app, "w1", "uia-worker");
    let rows = rendered_rows(&app, 46, 78)?;
    let regions = app.last_regions.get();
    let live = regions
        .live
        .ok_or(TestError::Missing("Reasoning-Fenster"))?;
    assert_eq!(live.height, 10, "8 Inhaltszeilen plus Rahmen");
    let header = rows
        .get(usize::from(live.y))
        .ok_or(TestError::Missing("Reasoning-Kopfzeile"))?;
    assert!(
        header.contains("Reasoning · 0 active"),
        "{}",
        rows.join("\n")
    );
    let hint = rows
        .get(usize::from(live.y) + 1)
        .ok_or(TestError::Missing("Hinweiszeile"))?;
    assert!(
        hint.contains("no visible events yet"),
        "{}",
        rows.join("\n")
    );
    Ok(())
}

/// Knapper Platz: der Chat behält seine Mindesthöhe von 8 Zeilen, die
/// Quadrate ihre 5 Zeilen, und das Reasoning-Fenster schrumpft auf seine
/// 3 Zeilen (Rahmen plus eine Inhaltszeile mit dem neuesten Eintrag).
/// Ein 10-zeiliger Composer lässt dem Körper bei 46×28 genau 17 Zeilen
/// (Quadrate 5, Reasoning 3, Finished 1, Chat 8).
#[test]
fn portrait_reasoning_window_shrinks_to_three_rows_when_space_is_short() -> TestResult {
    let mut app = test_chat_app()?;
    dock_test_running_agent(&mut app, "w1", "uia-worker");
    app.reasoning_feed
        .push_reasoning("w1", "uia-worker", "erste Meldung");
    app.reasoning_feed
        .push_tool_start("w1", "uia-worker", "fs.read");
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(46, 28))
        .map_err(ctx("test terminal"))?;
    terminal
        .draw(|frame| {
            let body = render::compose_body(
                frame,
                &app,
                Vec::new(),
                render::BodyRows {
                    status_rows: 1,
                    input_height: 10,
                    history_min: 1,
                    footer_rows: 0,
                    dialog_open: true,
                },
            );
            assert_eq!(body.history.height, 8, "the chat keeps its floor");
        })
        .map_err(ctx("draw"))?;
    let buffer = terminal.backend().buffer();
    let rows: Vec<String> = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect();
    let shown = rows.join("\n");
    let regions = app.last_regions.get();
    let live = regions
        .live
        .ok_or(TestError::Missing("Reasoning-Fenster"))?;
    assert_eq!(live.height, 3, "Rahmen plus eine Zeile:\n{shown}");
    assert_eq!(regions.agents.map(|a| a.height), Some(5), "{shown}");
    let body_row = rows
        .get(usize::from(live.y) + 1)
        .ok_or(TestError::Missing("Reasoning-Inhaltszeile"))?;
    assert!(
        body_row.contains("● fs.read · running"),
        "die Zeile trägt den neuesten Eintrag:\n{shown}"
    );
    assert!(
        !shown.contains("erste Meldung"),
        "ältere Zeile entfällt:\n{shown}"
    );
    Ok(())
}

/// Breite Terminals (Landschaft) behalten ihre Aufteilung: die Recency-Slots
/// ändern das Bild bei 120×42 nur im LIVE-Fenster der Mittelspalte
/// (PL-97 C3: dort stehen sie wie im Streifen des Portrait-Docks); Chat,
/// Finished/Failed, Agenten und Jobs bleiben Zelle für Zelle gleich.
#[test]
fn wide_layout_is_unchanged_by_the_recency_slots() -> TestResult {
    let mut app = test_chat_app()?;
    let before = rendered_rows(&app, 120, 42)?;
    app.recency.push(crate::recency::RecencyEntry {
        child: "neu".to_owned(),
        role: String::new(),
        text: "zweite Meldung".to_owned(),
    });
    let after = rendered_rows(&app, 120, 42)?;
    let live = app
        .last_regions
        .get()
        .live
        .ok_or(TestError::Missing("LIVE-Fenster"))?;
    let outside_live = |y: usize, row: &str| -> String {
        row.chars()
            .enumerate()
            .filter(|(x, _)| {
                let at = (u16::try_from(*x), u16::try_from(y));
                !matches!(at, (Ok(x), Ok(y)) if live.contains(ratatui::layout::Position::new(x, y)))
            })
            .map(|(_, cell)| cell)
            .collect()
    };
    for (y, (old, new)) in before.iter().zip(&after).enumerate() {
        assert_eq!(outside_live(y, old), outside_live(y, new), "row {y}");
    }
    assert!(
        after.iter().any(|row| row.contains("zweite Meldung")),
        "{}",
        after.join("\n")
    );
    Ok(())
}

/// PL-97 C2: Bus-Ereignis eines Kind-Agenten der Testsitzung.
fn mission_event(app: &mut ChatApp, id: &str, role: &str, turn: TurnEvent) {
    let event = harw_core::AgentEvent {
        agent: SessionId::from_str(id),
        parent: Some(app.session_id().clone()),
        role: role.to_owned(),
        kind: harw_core::AgentEventKind::Turn(turn),
    };
    feed_agent_event(
        &mut app.reasoning_feed,
        &app.agent_monitor,
        &app.session_id,
        &event,
    );
    app.agent_monitor.apply(&event);
}

/// PL-97 C2: ein denkender Root-Orchestrator mit laufendem Werkzeug, ein
/// Worker, ein fehlgeschlagener Explorer und ein Job.
fn mission_test_app() -> TestResult<ChatApp> {
    let mut app = test_chat_app()?;
    let orchestrator = harw_registry_defaults::profile::role_names::ROOT_ORCHESTRATOR;
    mission_event(
        &mut app,
        "o1",
        orchestrator,
        TurnEvent::ReasoningDelta {
            turn_id: harw_types::TurnId::new(),
            text: "Prüfe Lens und interne Werkzeugverträge".to_owned(),
        },
    );
    mission_event(
        &mut app,
        "o1",
        orchestrator,
        TurnEvent::ToolCallRequested {
            turn_id: harw_types::TurnId::new(),
            call_id: harw_types::ToolCallId::new(),
            tool_name: "fs.read".to_owned(),
            arguments: serde_json::json!({ "path": "README.md" }),
        },
    );
    dock_test_running_agent(&mut app, "w1", "uia-worker");
    mission_event(
        &mut app,
        "e2",
        "uia-explorer",
        TurnEvent::TurnFailed {
            turn_id: harw_types::TurnId::new(),
            reason: "Zeitlimit".to_owned(),
            retryable: false,
        },
    );
    app.agent_monitor.set_jobs(vec![dock_test_job()]);
    app.push_line(
        Role::System,
        "einzigartige-verlaufszeile-mission".to_owned(),
    );
    Ok(app)
}

/// Zeilenindex der einzigartigen Verlaufszeile aus [`mission_test_app`].
fn mission_history_row(rows: &[String]) -> TestResult<usize> {
    rows.iter()
        .position(|row| row.contains("einzigartige-verlaufszeile-mission"))
        .ok_or(TestError::Missing("Verlaufszeile"))
}

/// Portrait-Stapel bei 46×78: Agenten und Jobs als zwei gleich hohe,
/// getrennt gerahmte Fenster (Breite je 23, Höhe 12), darunter das
/// Reasoning-Fenster (10 Zeilen), der Finished/Failed-Streifen und der
/// Chat, der die größte Fläche bleibt; Statuszeile und Composer unten.
#[test]
fn render_viewport_at_46x78_shows_two_windows_reasoning_and_chat() -> TestResult {
    let app = mission_test_app()?;
    let rows = rendered_rows(&app, 46, 78)?;
    let shown = rows.join("\n");
    let regions = app.last_regions.get();
    assert_eq!(regions.agents, Some(Rect::new(0, 0, 23, 12)), "{shown}");
    assert_eq!(regions.jobs, Some(Rect::new(23, 0, 23, 12)), "{shown}");
    assert_eq!(regions.finished, Some(Rect::new(0, 22, 46, 1)), "{shown}");
    let live = regions
        .live
        .ok_or(TestError::Missing("Reasoning-Fenster"))?;
    assert_eq!(live, Rect::new(0, 12, 46, 10), "{shown}");
    assert!(
        screen_row(&rows, 0)?.starts_with('┌')
            && screen_row(&rows, 0)?.contains("Agents")
            && screen_row(&rows, 0)?.contains("┐┌ Jobs"),
        "{shown}"
    );
    assert!(
        screen_row(&rows, 11)?.starts_with('└'),
        "eigener Rahmen: {shown}"
    );
    assert!(
        screen_row(&rows, 12)?.starts_with("┌ Reasoning · 1 active"),
        "{shown}"
    );
    assert!(
        screen_row(&rows, 13)?.starts_with("│root-orches… ∴ Prüfe Lens"),
        "{shown}"
    );
    assert!(
        screen_row(&rows, 15)?.starts_with("│root-orches… ● fs.read · running"),
        "{shown}"
    );
    assert!(
        screen_row(&rows, 22)?.starts_with("─ FINISHED / FAILED · uia-explorer ×1 ✗1 NEW"),
        "{shown}"
    );
    let history_row = mission_history_row(&rows)?;
    assert!(
        history_row >= 23,
        "der Chat beginnt unter dem Finished-Streifen: {shown}"
    );
    let input_height = usize::from(input_height_for_test(&app, 46));
    assert!(
        screen_row(&rows, rows.len() - input_height - 1)?.contains("Ask"),
        "Statuszeile über dem Composer: {shown}"
    );
    assert!(
        rows.iter().any(|row| row.contains("› ")),
        "Composer: {shown}"
    );
    let chat_rows = 78 - input_height - 1 - 23;
    assert!(
        chat_rows > 78 / 2,
        "der Chat ist die größte Fläche ({chat_rows} Zeilen): {shown}"
    );
    Ok(())
}

/// 48×65 — dieselbe Reihenfolge, nichts überlappt, der Chat bleibt
/// dominant.
#[test]
fn render_viewport_at_48x65_keeps_windows_above_a_dominant_chat() -> TestResult {
    let app = mission_test_app()?;
    let rows = rendered_rows(&app, 48, 65)?;
    let shown = rows.join("\n");
    let regions = app.last_regions.get();
    let agents = regions
        .agents
        .ok_or(TestError::Missing("Agenten-Fenster"))?;
    let jobs = regions.jobs.ok_or(TestError::Missing("Jobs-Fenster"))?;
    let finished = regions.finished.ok_or(TestError::Missing("Finished"))?;
    let live = regions
        .live
        .ok_or(TestError::Missing("Reasoning-Fenster"))?;
    assert_eq!(agents, Rect::new(0, 0, 24, 12), "{shown}");
    assert_eq!(jobs, Rect::new(24, 0, 24, 12), "{shown}");
    assert_eq!(live, Rect::new(0, 12, 48, 10), "{shown}");
    assert_eq!(finished, Rect::new(0, 22, 48, 1), "{shown}");
    for (a, b) in [
        (agents, jobs),
        (agents, live),
        (jobs, live),
        (live, finished),
    ] {
        assert!(a.intersection(b).is_empty(), "{a:?} overlaps {b:?}");
    }
    assert!(
        screen_row(&rows, 12)?.contains("Reasoning · 1 active"),
        "{shown}"
    );
    assert!(
        screen_row(&rows, 22)?.contains("FINISHED / FAILED"),
        "{shown}"
    );
    let history_row = mission_history_row(&rows)?;
    assert!(history_row >= 23, "{shown}");
    let chat_rows = 65 - usize::from(input_height_for_test(&app, 48)) - 1 - 23;
    assert!(chat_rows > 65 / 2, "{chat_rows}: {shown}");
    Ok(())
}

/// Agenten und Jobs sind bei 70×90 zwei getrennt gerahmte, gleich hohe
/// Fenster nebeneinander (je 35×18); die Jobs zeigen den laufenden Job,
/// darunter folgen Reasoning-Fenster und Finished/Failed-Streifen.
#[test]
fn render_viewport_at_70x90_splits_agents_and_jobs_into_two_windows() -> TestResult {
    let app = mission_test_app()?;
    let rows = rendered_rows(&app, 70, 90)?;
    let shown = rows.join("\n");
    let regions = app.last_regions.get();
    assert_eq!(regions.agents, Some(Rect::new(0, 0, 35, 18)), "{shown}");
    assert_eq!(regions.jobs, Some(Rect::new(35, 0, 35, 18)), "{shown}");
    let top: String = screen_row(&rows, 0)?.chars().collect();
    assert!(
        top.starts_with("┌ Agents") && top.contains("┐┌ Jobs · 1 active"),
        "zwei eigene Rahmen: {shown}"
    );
    let jobs_rows: Vec<String> = rows
        .get(1..3)
        .ok_or(TestError::Missing("Kartenzeilen"))?
        .iter()
        .map(|row| row.chars().skip(35).collect())
        .collect();
    assert!(jobs_rows.iter().any(|row| row.contains("build")), "{shown}");
    let agents_rows: Vec<String> = rows
        .get(1..3)
        .ok_or(TestError::Missing("Kartenzeilen"))?
        .iter()
        .map(|row| row.chars().take(35).collect())
        .collect();
    assert!(
        !agents_rows.iter().any(|row| row.contains("build")),
        "keine Jobs im Agenten-Fenster: {shown}"
    );
    assert!(
        screen_row(&rows, 18)?.starts_with("┌ Reasoning · 1 active"),
        "{shown}"
    );
    assert!(
        screen_row(&rows, 28)?.starts_with("─ FINISHED / FAILED"),
        "{shown}"
    );
    let history_row = mission_history_row(&rows)?;
    assert!(history_row >= 29, "{shown}");
    Ok(())
}

/// `F4` läuft Chat → Agenten → Jobs → Finished/Failed → Reasoning (Live)
/// → Chat; im Hochformat ist das Reasoning-Fenster fokussierbar.
/// `Esc` gibt an den Chat zurück, `Alt+J`/`Alt+F` springen direkt. Auch
/// bei schmalen 46 Spalten bleibt das Jobs-Fenster ein eigenes Fenster:
/// `Alt+J` fokussiert es, die Agentenliste behält einen Agenten.
#[test]
fn focus_cycle_walks_the_mission_windows_including_portrait_live() -> TestResult {
    let mut app = mission_test_app()?;
    let _ = rendered_rows(&app, 70, 90)?;
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    let alt = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT);
    let mut seen = Vec::new();
    for _ in 0..5 {
        assert_eq!(handle_panel_key(&mut app, key(KeyCode::F(4))), Some(true));
        seen.push(app.panels.focus);
    }
    use crate::panes::PaneFocus;
    assert_eq!(
        seen,
        vec![
            PaneFocus::Agents,
            PaneFocus::Jobs,
            PaneFocus::Finished,
            PaneFocus::Live,
            PaneFocus::Chat
        ]
    );
    assert_eq!(handle_panel_key(&mut app, alt('j')), Some(true));
    assert_eq!(app.panels.focus, PaneFocus::Jobs);
    assert_eq!(handle_panel_key(&mut app, key(KeyCode::Esc)), Some(true));
    assert_eq!(app.panels.focus, PaneFocus::Chat);
    assert_eq!(handle_panel_key(&mut app, alt('f')), Some(true));
    assert_eq!(app.panels.focus, PaneFocus::Finished);
    // Enter klappt Finished/Failed auf; das nächste Bild gibt mehr Zeilen.
    assert_eq!(handle_panel_key(&mut app, key(KeyCode::Enter)), Some(true));
    assert!(app.mission.finished_expanded);
    let _ = rendered_rows(&app, 70, 90)?;
    let finished = app
        .last_regions
        .get()
        .finished
        .ok_or(TestError::Missing("Finished"))?;
    assert!(finished.height >= 3, "{finished:?}");

    // Schmal (46 Spalten): das Jobs-Fenster bleibt ein eigenes Fenster,
    // `Alt+J` fokussiert es und die Agentenliste wählt keinen Job.
    app.panels.focus = PaneFocus::Jobs;
    let _ = rendered_rows(&app, 46, 78)?;
    assert!(app.last_regions.get().jobs.is_some());
    assert_eq!(handle_panel_key(&mut app, alt('j')), Some(true));
    assert_eq!(app.panels.focus, PaneFocus::Jobs);
    assert!(app.agent_monitor.selected_job().is_none());
    Ok(())
}

/// PL-97 C2 / W02 Abnahme 4: das Jobs-Fenster hat eine eigene Auswahl;
/// im Agenten-Fenster bleibt die Auswahl auf Agenten, `p` heftet den
/// Agenten an (das Reasoning-Fenster zeigt dann nur ihn).
#[test]
fn jobs_window_selection_is_independent_and_p_pins_live() -> TestResult {
    let mut app = mission_test_app()?;
    app.reasoning_feed
        .push_reasoning("w1", "uia-worker", "fremder Text");
    let unpinned = rendered_rows(&app, 70, 90)?.join("\n");
    assert!(
        unpinned.contains("fremder Text"),
        "ungeheftet zeigt das Reasoning-Fenster alle Agenten: {unpinned}"
    );
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    app.panels.focus = crate::panes::PaneFocus::Agents;
    for _ in 0..5 {
        handle_panel_key(&mut app, key(KeyCode::Char('j')));
        assert!(
            app.agent_monitor.selected_job().is_none(),
            "die Agentenliste wählt keine Jobs"
        );
    }
    app.agent_monitor.selected = 0;
    let pinned = app
        .agent_monitor
        .selected_agent()
        .ok_or(TestError::Missing("gewählter Agent"))?;
    assert_eq!(
        handle_panel_key(&mut app, key(KeyCode::Char('p'))),
        Some(true)
    );
    assert_eq!(app.mission.pinned_agent.as_deref(), Some(pinned.as_str()));
    let rows = rendered_rows(&app, 70, 90)?;
    let shown = rows.join("\n");
    assert_eq!(
        app.mission.pinned_agent.as_deref(),
        Some(pinned.as_str()),
        "{shown}"
    );
    let reasoning: Vec<&String> = rows.iter().skip(18).take(10).collect();
    assert!(
        reasoning.iter().any(|row| row.contains("root-orches")),
        "{shown}"
    );
    assert!(
        !shown.contains("fremder Text"),
        "das Reasoning-Fenster zeigt nur den angehefteten Agenten: {shown}"
    );

    app.panels.focus = crate::panes::PaneFocus::Jobs;
    assert_eq!(handle_panel_key(&mut app, key(KeyCode::Down)), Some(true));
    assert_eq!(app.agent_monitor.selected, 0, "Agentenauswahl unverändert");
    assert_eq!(handle_panel_key(&mut app, key(KeyCode::Enter)), Some(true));
    let detail = app
        .agent_detail
        .as_ref()
        .ok_or(TestError::Missing("Job-Detailansicht"))?;
    assert_eq!(detail.job.as_deref(), Some("job-1"));
    assert_eq!(app.panels.focus, crate::panes::PaneFocus::Jobs);
    assert_eq!(handle_panel_key(&mut app, key(KeyCode::Esc)), Some(true));
    assert!(app.agent_detail.is_none(), "Esc schließt die Detailansicht");
    assert_eq!(app.panels.focus, crate::panes::PaneFocus::Jobs);
    Ok(())
}

/// PL-97 C2 / W02 Abnahme 4 + §3.3.3: LIVE nimmt nie Composer-Tasten —
/// getippter Text landet im Composer, `F4` tippt nichts, `Alt+L` öffnet
/// nur die Details des LIVE-Agenten (Esc schließt sie wieder).
#[test]
fn live_never_takes_composer_keys() -> TestResult {
    let mut app = mission_test_app()?;
    let (bus, _receiver) = harw_event_channel();
    let _ = rendered_rows(&app, 46, 78)?;
    assert!(app.last_regions.get().live.is_some());
    for c in ['h', 'a', 'l', 'l', 'o'] {
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            &bus,
        );
    }
    assert_eq!(app.input.text(), "hallo");
    let mut reached_live = false;
    for _ in 0..6 {
        handle_panel_key(&mut app, KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE));
        reached_live |= app.panels.focus == crate::panes::PaneFocus::Live;
    }
    assert!(reached_live, "F4 erreicht das Reasoning-Fenster");
    assert_eq!(app.input.text(), "hallo", "F4 tippt nichts");
    app.panels.focus = crate::panes::PaneFocus::Chat;

    let alt_l = KeyEvent::new(KeyCode::Char('l'), KeyModifiers::ALT);
    assert!(handle_key(&mut app, alt_l, &bus));
    let detail = app
        .agent_detail
        .as_ref()
        .ok_or(TestError::Missing("LIVE-Detailansicht"))?;
    assert_eq!(detail.agent.as_str(), "o1", "the LIVE source");
    assert_eq!(app.input.text(), "hallo", "Alt+L tippt nichts");
    assert!(handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &bus
    ));
    assert!(app.agent_detail.is_none());
    assert!(!app.mission.live_expanded);
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        &bus,
    );
    assert_eq!(app.panels.focus, crate::panes::PaneFocus::Chat);
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE),
        &bus,
    );
    assert_eq!(app.input.text(), "hallo!");
    Ok(())
}

/// PL-97 C2 / W02 Abnahme 3 + 9: mehrteilige Agenten-Prosa mit
/// Steuerzeichen erscheint im Reasoning-Fenster bereinigt (Absätze zu
/// einem umbrochenen Eintrag zusammengezogen, ESC/BEL als sichtbare
/// Marker); kein rohes ESC/BEL erreicht den Puffer.
#[test]
fn multi_paragraph_agent_prose_renders_sanitized_in_the_reasoning_window() -> TestResult {
    let mut app = mission_test_app()?;
    let orchestrator = harw_registry_defaults::profile::role_names::ROOT_ORCHESTRATOR;
    mission_event(
        &mut app,
        "o1",
        orchestrator,
        TurnEvent::ReasoningDelta {
            turn_id: harw_types::TurnId::new(),
            text:
                "\n\nAbsatz zwei mit \u{1b}[31mFarbe\u{1b}[0m.\n\nAbsatz drei\u{7} endet hier."
                    .to_owned(),
        },
    );
    mission_event(
        &mut app,
        "o1",
        orchestrator,
        TurnEvent::AssistantDelta {
            turn_id: harw_types::TurnId::new(),
            text: "Erste Antwortzeile\n\nZweite Antwortzeile\n\nDritte".to_owned(),
        },
    );
    // Deltas setzen die Phase auf „denkt“; der nächste Aufruf läuft.
    mission_event(
        &mut app,
        "o1",
        orchestrator,
        TurnEvent::ToolCallRequested {
            turn_id: harw_types::TurnId::new(),
            call_id: harw_types::ToolCallId::new(),
            tool_name: "fs.read".to_owned(),
            arguments: serde_json::json!({ "path": "docs/README.md" }),
        },
    );
    let rows = rendered_rows(&app, 46, 78)?;
    let shown = rows.join("\n");
    let live = app
        .last_regions
        .get()
        .live
        .ok_or(TestError::Missing("Reasoning-Fenster"))?;
    assert_eq!(live.height, 10, "{shown}");
    let live_rows = rows
        .get(usize::from(live.y)..usize::from(live.y + live.height))
        .ok_or(TestError::Missing("Reasoning-Zeilen"))?;
    for row in live_rows {
        assert!(
            !row.chars().any(|c| c.is_control()),
            "Steuerzeichen im Reasoning-Fenster: {row:?}"
        );
        assert!(
            !row.contains("[31m") && !row.contains("[0m") && !row.contains("]0;"),
            "CSI-/OSC-Nutzlast im Reasoning-Fenster: {row:?}"
        );
    }
    let flowed = live_rows
        .iter()
        .map(|row| row.trim_matches(|c: char| c == '│' || c.is_whitespace()))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        flowed.contains("∴ Absatz zwei mit ⟨ESC⟩Farbe⟨ESC⟩. Absatz drei⟨U+0007⟩ endet hier."),
        "die Absätze sind zu einem Eintrag zusammengezogen: {shown}"
    );
    assert!(
        live_rows.iter().any(|row| row.contains("● fs.read")),
        "{shown}"
    );
    assert!(
        !shown.contains('\u{1b}') && !shown.contains('\u{7}'),
        "kein rohes ESC/BEL im Bild"
    );
    assert!(
        !shown.contains("[31m") && !shown.contains("]0;titel"),
        "keine Sequenz-Nutzlast im Bild: {shown}"
    );
    Ok(())
}

/// PL-97 C2: solange der Chat fokussiert ist und der Composer Text
/// enthält, bleibt `Alt+F` dort „Wort vorwärts“: der Fokus wandert nicht
/// nach Finished/Failed, das nächste `c` wird getippt und quittiert keinen
/// Fehlschlag. Mit leerem Composer fokussiert `Alt+F` wie vorgesehen.
#[test]
fn alt_f_while_typing_moves_by_word_and_never_acknowledges_failures() -> TestResult {
    let mut app = mission_test_app()?;
    let (bus, _receiver) = harw_event_channel();
    let _ = rendered_rows(&app, 70, 90)?;
    let plain = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
    let alt = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT);
    for c in ['a', 'b', ' ', 'c', 'd'] {
        handle_key(&mut app, plain(c), &bus);
    }
    handle_key(&mut app, alt('b'), &bus);
    handle_key(&mut app, alt('b'), &bus);
    assert!(handle_key(&mut app, alt('f'), &bus), "word forward redraws");
    assert_eq!(app.panels.focus, crate::panes::PaneFocus::Chat);
    handle_key(&mut app, plain('c'), &bus);
    let text = app.input.text().to_owned();
    assert_eq!(text.chars().count(), 6, "c is typed: {text:?}");
    assert_ne!(text, "cab cd", "Alt+F moved the cursor off the start");
    assert!(
        app.agent_monitor
            .agent("e2")
            .is_some_and(crate::agent_monitor::AgentLive::is_unseen_failure),
        "no failure is acknowledged"
    );

    app.input.clear();
    assert!(handle_key(&mut app, alt('f'), &bus));
    assert_eq!(app.panels.focus, crate::panes::PaneFocus::Finished);
    Ok(())
}

/// PL-97 C2: `Alt+J`/`Alt+F` nach `F3` (oder aus einem maximierten
/// Panel) richten sich nach der Lage mit eingeblendeten Fenstern, nicht
/// nach dem letzten Bild ohne Fenster: bei 70×90 landet `Alt+J` im
/// Jobs-Fenster, und die Agentenliste bleibt auf einem Agenten.
#[test]
fn alt_j_after_hiding_the_windows_focuses_the_jobs_window() -> TestResult {
    use crate::panes::PaneFocus;
    let mut app = mission_test_app()?;
    let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
    let alt = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT);
    let _ = rendered_rows(&app, 70, 90)?;
    assert_eq!(handle_panel_key(&mut app, key(KeyCode::F(3))), Some(true));
    assert!(!app.panels.agents_visible);
    let _ = rendered_rows(&app, 70, 90)?;
    assert_eq!(app.last_regions.get().jobs, None, "no windows on screen");
    let selected = app.agent_monitor.selected;
    assert_eq!(handle_panel_key(&mut app, alt('j')), Some(true));
    assert!(app.panels.agents_visible, "Alt+J shows the windows");
    assert_eq!(app.panels.focus, PaneFocus::Jobs);
    assert_eq!(app.agent_monitor.selected, selected);
    assert!(
        app.agent_monitor.selected_job().is_none(),
        "the agents list keeps an agent selected"
    );

    // Aus dem maximierten Agenten-Panel: Alt+F öffnet Finished/Failed.
    let _ = rendered_rows(&app, 70, 90)?;
    app.panels.focus = PaneFocus::Agents;
    assert_eq!(handle_panel_key(&mut app, key(KeyCode::F(11))), Some(true));
    assert!(app.panels.maximized);
    let _ = rendered_rows(&app, 70, 90)?;
    assert_eq!(handle_panel_key(&mut app, alt('f')), Some(true));
    assert!(!app.panels.maximized);
    assert_eq!(app.panels.focus, PaneFocus::Finished);
    Ok(())
}

/// PL-97 C2: auf einem kurzen Körper entfällt die Kartenzeile (der Chat
/// behält seine Mindesthöhe). Dann ist die Agentenliste kein Fokusziel:
/// ein Agenten-Fokus geht an den Chat, eine offene Detailansicht wird
/// maximiert statt unsichtbar zu bleiben.
#[test]
fn a_dropped_card_row_releases_the_agents_focus() -> TestResult {
    use crate::panes::PaneFocus;
    let mut app = mission_test_app()?;
    let draw_short = |app: &ChatApp| -> TestResult {
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(46, 28))
            .map_err(ctx("test terminal"))?;
        terminal
            .draw(|frame| {
                let _ = render::compose_body(
                    frame,
                    app,
                    Vec::new(),
                    render::BodyRows {
                        status_rows: 1,
                        input_height: 17,
                        history_min: 1,
                        footer_rows: 0,
                        dialog_open: true,
                    },
                );
            })
            .map_err(ctx("draw"))?;
        Ok(())
    };
    app.panels.focus = PaneFocus::Agents;
    draw_short(&app)?;
    let regions = app.last_regions.get();
    assert_eq!(regions.agents, None, "no card row on a 10-row body");
    assert!(regions.presence.agents_hidden);
    sync_mission_focus(&mut app);
    assert_eq!(app.panels.focus, PaneFocus::Chat);
    assert!(!app.panels.can_focus(PaneFocus::Agents));

    app.panels.focus = PaneFocus::Agents;
    assert!(app.open_agent_detail());
    app.panels.maximized = false;
    draw_short(&app)?;
    sync_mission_focus(&mut app);
    assert!(app.panels.maximized, "the detail is shown full screen");
    assert!(app.agent_detail.is_some());
    Ok(())
}

/// PL-97 C2 / W02 acceptance 2: `render_viewport` over the W02 size
/// matrix never panics, and every window region of the frame lies inside
/// the screen and is disjoint from the others.
#[test]
fn render_viewport_size_matrix_keeps_window_regions_in_bounds_and_disjoint() -> TestResult {
    let app = mission_test_app()?;
    for (width, height) in [
        (46, 78),
        (48, 65),
        (70, 90),
        (39, 60),
        (48, 19),
        (60, 60),
        (80, 40),
        (100, 30),
        (120, 42),
        (160, 45),
        (200, 50),
        (1, 1),
        (0, 0),
    ] {
        let _ = rendered_rows(&app, width, height)?;
        let regions = app.last_regions.get();
        let windows: Vec<Rect> = [regions.agents, regions.jobs, regions.finished, regions.live]
            .into_iter()
            .flatten()
            .collect();
        for rect in &windows {
            assert!(
                rect.x.saturating_add(rect.width) <= width
                    && rect.y.saturating_add(rect.height) <= height,
                "{width}x{height}: {rect:?} leaves the screen"
            );
        }
        for (index, a) in windows.iter().enumerate() {
            for b in windows.iter().skip(index + 1) {
                assert!(
                    a.intersection(*b).is_empty(),
                    "{width}x{height}: {a:?} overlaps {b:?}"
                );
            }
        }
    }
    Ok(())
}

#[test]
fn long_queue_under_portrait_dock_keeps_status_and_composer_visible() -> TestResult {
    let mut app = test_chat_app()?;
    dock_test_running_agent(&mut app, "w1", "uia-worker");
    app.agent_monitor.set_jobs(vec![dock_test_job()]);
    for index in 0..60 {
        app.pending_turns
            .push_back(format!("wartende-nachricht-{index}"));
    }
    // Portrait-Minimum (28 Zeilen) mit mehrzeiligem Composer: Dock,
    // volle Warteschlange, Status und Composer überschreiten zusammen
    // die Höhe, wenn die Dock-Höhe nicht abgezogen wird.
    app.input
        .insert_str("zeile 1\nzeile 2\nzeile 3\nzeile 4\nzeile 5\nletzte-zeile");
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(64, 28))
        .map_err(ctx("test terminal"))?;
    terminal
        .draw(|frame| render_viewport(frame, &app, &Spinner::new(), None))
        .map_err(ctx("draw"))?;
    let buffer = terminal.backend().buffer();
    let rows: Vec<String> = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect();
    let shown = rows.join("\n");
    assert!(
        app.last_regions
            .get()
            .agents
            .is_some_and(|dock| dock.y == 0),
        "Portrait-Dock oben erwartet: {shown}"
    );
    let last_rows = &rows[rows
        .len()
        .saturating_sub(usize::from(input_height_for_test(&app, 64)) + 1)..];
    assert!(
        last_rows.iter().any(|row| row.contains("Ask")),
        "Statuszeile muss trotz langer Warteschlange unten sichtbar bleiben: {shown}"
    );
    assert!(
        shown.contains("letzte-zeile"),
        "die letzte Composer-Zeile muss sichtbar bleiben: {shown}"
    );
    Ok(())
}

#[test]
fn render_viewport_at_80x40_pins_a_top_dock_full_width_chat_below_and_status_composer_at_bottom()
-> TestResult {
    let mut app = test_chat_app()?;
    dock_test_running_agent(&mut app, "w1", "uia-worker");
    app.agent_monitor.set_jobs(vec![dock_test_job()]);
    app.push_line(
        Role::System,
        "einzigartige-verlaufszeile-fuer-den-dock-test".to_owned(),
    );
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 40))
        .map_err(ctx("test terminal"))?;
    terminal
        .draw(|frame| render_viewport(frame, &app, &Spinner::new(), None))
        .map_err(ctx("draw"))?;
    let buffer = terminal.backend().buffer();
    let rows: Vec<String> = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect();
    let shown = rows.join("\n");

    let regions = app.last_regions.get();
    let dock = regions.agents.ok_or(TestError::Missing("Portrait-Dock"))?;
    let jobs = regions.jobs.ok_or(TestError::Missing("Jobs-Fenster"))?;
    assert_eq!(dock.y, 0, "der Dock ist oben angedockt: {shown}");
    // PL-97 C2: ab 68 Spalten teilen sich Agenten- und Jobs-Fenster die
    // Kartenzeile über die volle Breite.
    assert_eq!(
        (dock.x, dock.width + jobs.width, jobs.y),
        (0, 80, 0),
        "die Kartenzeile nimmt die volle Breite ein: {shown}"
    );
    assert!(
        rows[..usize::from(dock.height)]
            .iter()
            .any(|row| row.contains("Agents")),
        "Dock-Titel fehlt in den oberen Zeilen: {shown}"
    );

    // Der Chat beginnt erst unterhalb des Docks: die einzigartige
    // Verlaufszeile taucht nie innerhalb der Dock-Höhe auf.
    let history_row = rows
        .iter()
        .position(|row| row.contains("einzigartige-verlaufszeile"))
        .ok_or(TestError::Missing("Verlaufszeile"))?;
    assert!(
        history_row >= usize::from(dock.height),
        "der Chat darf nicht in den Dock hineinragen: history_row={history_row}, dock.height={}: {shown}",
        dock.height
    );

    let last_rows = &rows[rows
        .len()
        .saturating_sub(usize::from(input_height_for_test(&app, 80)) + 1)..];
    assert!(
        last_rows.iter().any(|row| row.contains("Ask")),
        "Statuszeile (Modus) fehlt am unteren Rand: {shown}"
    );
    assert!(
        rows.iter().any(|row| row.contains("› ")),
        "Composer-Prompt fehlt: {shown}"
    );
    Ok(())
}

/// Hilfsberechnung, um die letzten Zeilen (Statuszeile/Composer) grob
/// einzugrenzen — orientiert sich an derselben Formel wie
/// `render_viewport` (Eingabezeilen geklemmt auf 1–8, plus Rahmen).
fn input_height_for_test(app: &ChatApp, width: u16) -> u16 {
    let input_width = width.saturating_sub(5) as usize;
    let input_line_count = app.input.visible_lines(input_width).len().clamp(1, 8) as u16;
    input_line_count + 2
}

/// Ein offener Freigabe-Dialog zeichnet weiterhin über (zeitlich nach)
/// dem Dock, nie hinein: bei 80×40 mit einem aktiven Agenten liegen
/// Dialog-Zeilen (Optionen, Hinweis) ausschließlich unterhalb der
/// Dock-Höhe, während der Dock-Titel weiterhin sichtbar bleibt.
#[test]
fn modal_and_approval_dialogs_render_over_the_dock_not_inside_it_at_80x40() -> TestResult {
    let mut app = test_chat_app()?;
    dock_test_running_agent(&mut app, "w1", "uia-worker");
    app.agent_monitor.set_jobs(vec![dock_test_job()]);
    app.pending_approval_dialog = Some(ApprovalDialog::new(ApprovalDialogRequest {
        call: harw_extension_api::ToolCall {
            id: harw_types::ToolCallId::new(),
            name: harw_extension_api::ToolName::new("shell.exec"),
            arguments: serde_json::json!({ "command": "echo hi" }),
        },
        cwd: None,
        justification: None,
        risk: None,
        origin: None,
        remember_rule: None,
        deadline: Instant::now() + Duration::from_secs(300),
        reason_input_enabled: true,
    }));

    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 40))
        .map_err(ctx("test terminal"))?;
    terminal
        .draw(|frame| render_viewport(frame, &app, &Spinner::new(), None))
        .map_err(ctx("draw"))?;
    let buffer = terminal.backend().buffer();
    let rows: Vec<String> = (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect();
    let shown = rows.join("\n");

    let dock = app
        .last_regions
        .get()
        .agents
        .ok_or(TestError::Missing("Portrait-Dock"))?;
    assert!(
        rows[..usize::from(dock.height)]
            .iter()
            .any(|row| row.contains("Agents")),
        "Dock-Titel fehlt weiterhin: {shown}"
    );

    let dialog_rows: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.contains("Yes") || row.contains("No (Esc)"))
        .map(|(y, _)| y)
        .collect();
    assert!(!dialog_rows.is_empty(), "Dialogzeilen fehlen: {shown}");
    assert!(
        dialog_rows.iter().all(|&y| y >= usize::from(dock.height)),
        "Dialogzeilen liegen im Dock statt darunter: {dialog_rows:?}, dock.height={}: {shown}",
        dock.height
    );
    Ok(())
}
}

// ── Tests zur Scharfschaltung der Freigabefrage (W1-08) ──────────────────────

#[cfg(test)]
mod approval_arming_tests {
use super::tests::test_chat_app;
use super::*;
use crate::test_support::{TestResult, ctx};
use crossterm::event::KeyEventKind;
use ratatui::buffer::Buffer;

/// Tastendruck ohne Modifier.
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Sichtbare Frage, Totzeit abgelaufen.
const ARMED: Duration = Duration::from_millis(900);

/// Pflichtfall G-008: ein `y` innerhalb der Totzeit gibt **nichts** frei.
#[test]
fn an_early_y_is_ignored_and_a_late_one_approves() -> TestResult {
    assert_eq!(
        classify_armed_approval_key(key(KeyCode::Char('y')), Duration::ZERO, true),
        ApprovalKeyAction::NotArmed
    );
    assert_eq!(
        classify_armed_approval_key(
            key(KeyCode::Char('y')),
            APPROVAL_ARMING_DELAY - Duration::from_millis(1),
            true
        ),
        ApprovalKeyAction::NotArmed
    );
    assert_eq!(
        classify_armed_approval_key(key(KeyCode::Char('y')), APPROVAL_ARMING_DELAY, true),
        ApprovalKeyAction::Approve
    );
    assert_eq!(
        classify_armed_approval_key(key(KeyCode::Char('Y')), ARMED, true),
        ApprovalKeyAction::Approve
    );
    Ok(())
}

/// Auch die Ablehnung per `n` ist der Tipp-Falle entzogen — sonst
/// beantwortete ein getipptes „nein" die Frage, bevor sie gelesen ist.
#[test]
fn an_early_n_is_ignored_and_a_late_one_rejects() -> TestResult {
    assert_eq!(
        classify_armed_approval_key(key(KeyCode::Char('n')), Duration::from_millis(10), true),
        ApprovalKeyAction::NotArmed
    );
    assert_eq!(
        classify_armed_approval_key(key(KeyCode::Char('n')), ARMED, true),
        ApprovalKeyAction::Reject(REASON_OPERATOR_REJECTED)
    );
    Ok(())
}

/// Eine nicht sichtbare Frage (hochgescrollt) wird nie beantwortet.
#[test]
fn an_invisible_question_accepts_no_answer() -> TestResult {
    assert_eq!(
        classify_armed_approval_key(key(KeyCode::Char('y')), ARMED, false),
        ApprovalKeyAction::NotArmed
    );
    assert_eq!(
        classify_armed_approval_key(key(KeyCode::Char('n')), ARMED, false),
        ApprovalKeyAction::NotArmed
    );
    Ok(())
}

/// Eine gedrückt gehaltene Taste ist keine Entscheidung.
#[test]
fn a_repeated_key_never_answers() -> TestResult {
    let repeat =
        KeyEvent::new_with_kind(KeyCode::Char('y'), KeyModifiers::NONE, KeyEventKind::Repeat);
    assert_eq!(
        classify_armed_approval_key(repeat, ARMED, true),
        ApprovalKeyAction::NotArmed
    );
    Ok(())
}

/// Abbruchtasten bleiben immer wirksam: sie lehnen ab (fail-safe).
#[test]
fn cancel_keys_stay_armed_because_they_reject() -> TestResult {
    assert_eq!(
        classify_armed_approval_key(key(KeyCode::Esc), Duration::ZERO, false),
        ApprovalKeyAction::Reject(REASON_OPERATOR_CANCELLED)
    );
    assert_eq!(
        classify_armed_approval_key(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Duration::ZERO,
            false
        ),
        ApprovalKeyAction::Reject(REASON_OPERATOR_CANCELLED)
    );
    Ok(())
}

/// `v` klappt jederzeit auf, andere Tasten bleiben bedeutungslos.
#[test]
fn v_toggles_details_and_other_keys_are_ignored() -> TestResult {
    assert_eq!(
        classify_armed_approval_key(key(KeyCode::Char('v')), Duration::ZERO, true),
        ApprovalKeyAction::ToggleDetails
    );
    assert_eq!(
        classify_armed_approval_key(key(KeyCode::Char('V')), ARMED, false),
        ApprovalKeyAction::ToggleDetails
    );
    assert_eq!(
        classify_armed_approval_key(key(KeyCode::Char('x')), ARMED, true),
        ApprovalKeyAction::Ignore
    );
    assert_eq!(
        classify_armed_approval_key(
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
            ARMED,
            true
        ),
        ApprovalKeyAction::Ignore
    );
    Ok(())
}

/// Der Anzeigezustand schaltet nach dem erneuten Anzeigen einer Frage
/// (Rearm) wieder scharf.
///
/// Seit der Umstellung auf das Freigabe-Panel gibt es keinen eigenen
/// `ApprovalPresentation`-Wrapper mit `since_shown`/`rearm` mehr — die
/// Schleife in `drive_pauses_to_completion` hält nur noch ein rohes
/// `Option<Instant>` (`dialog_shown_at`) und setzt es bei jeder neuen
/// Frage auf `Instant::now()` zurück. Die geprüfte Eigenschaft bleibt
/// dieselbe — nur direkt an der Bedingungsfunktion des Panels
/// ([`approval_dialog_key_is_armed`]) statt am inzwischen entfernten
/// Wrapper: vor dem Rearm (Anzeigedauer erreicht die Verzögerung) ist die
/// Taste scharf, unmittelbar danach (Anzeigedauer zurück auf null) wieder
/// nicht.
#[test]
fn rearm_restarts_the_arming_delay() -> TestResult {
    let answer_key = key(KeyCode::Char('y'));

    assert!(approval_dialog_key_is_armed(answer_key, ARMED));
    // Rearm: eine neu eingetroffene Frage setzt die seit dem Anzeigen
    // verstrichene Zeit auf null zurück.
    assert!(!approval_dialog_key_is_armed(answer_key, Duration::ZERO));
    Ok(())
}

// ── Host-Permit-Dialog (Plan „UIA-Shell-Worker und Shell-Modus", Schritt 2) ──
//
// Der Frage-/Antwortvertrag (`HostPermitPrompt`) und die
// Ausstellungslogik (Ledger/Registry) leben inzwischen in
// `harw_tool_shell` (Vertrag) bzw. `harw_tool_shell::exec::ShellExecutor`
// (Ausstellung, eigenständig dort getestet). Die folgenden Tests prüfen
// deshalb nur noch, was `app.rs` selbst besitzt: welche Vorauswahl der
// Dialog anzeigt und welche Entscheidung der App-seitige Dispatch über
// den Antwortkanal der Frage zurücksendet.

/// Baut eine echte [`HostPermitPrompt`] über ihren öffentlichen
/// Konstruktor (`harw_tool_shell::host_permit_prompt::HostPermitPrompt::new`)
/// — denselben Vertrag, den
/// `harw_tool_shell::exec::ShellExecutor::authorize_host_command`
/// tatsächlich verwendet — statt die privaten Felder des Typs zu erraten.
fn build_host_permit_prompt(
    preselected: HostPermitVariant,
) -> (
    HostPermitPrompt,
    tokio::sync::oneshot::Receiver<Option<HostPermitVariant>>,
) {
    build_host_permit_prompt_for("host-process-worker@1", "echo hi", preselected)
}

/// Wie [`build_host_permit_prompt`], aber mit einstellbarer
/// `worker_definition`/`command` — für B6-Tests, die zwischen einer
/// Sandbox-Lease-Anfrage
/// ([`harw_tool_shell::SANDBOX_LEASE_WORKER_DEFINITION`]) und einer
/// gewöhnlichen Host-Befehlsanfrage unterscheiden müssen.
fn build_host_permit_prompt_for(
    worker_definition: &str,
    command: &str,
    preselected: HostPermitVariant,
) -> (
    HostPermitPrompt,
    tokio::sync::oneshot::Receiver<Option<HostPermitVariant>>,
) {
    HostPermitPrompt::new(
        "s1".to_owned(),
        worker_definition.to_owned(),
        command.to_owned(),
        std::path::PathBuf::from("/workspace"),
        preselected,
    )
}

/// `build_host_permit_dialog` wählt Option 0 (Einmalig) vor, wenn der
/// Aufrufer [`HostPermitVariant::SingleExecution`] vorgeschlagen hat
/// (Arbeitsmodus außerhalb von `shell`).
#[tokio::test]
async fn build_host_permit_dialog_preselects_single_execution() -> TestResult {
    let (prompt, answer) = build_host_permit_prompt(HostPermitVariant::SingleExecution);
    let mut dialog = build_host_permit_dialog(&prompt);
    let action = dialog.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(action, ChoiceAction::Chosen(0));
    assert!(prompt.deny());
    assert_eq!(
        answer
            .await
            .map_err(ctx("responder must deliver an answer"))?,
        None
    );
    Ok(())
}

/// `build_host_permit_dialog` wählt Option 1 (Host-Arbeitsphase) vor,
/// wenn der Aufrufer [`HostPermitVariant::SessionLease`] vorgeschlagen hat
/// (Shell-Modus).
#[tokio::test]
async fn build_host_permit_dialog_preselects_session_lease() -> TestResult {
    let (prompt, answer) = build_host_permit_prompt(HostPermitVariant::SessionLease);
    let mut dialog = build_host_permit_dialog(&prompt);
    let action = dialog.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(action, ChoiceAction::Chosen(1));
    assert!(prompt.deny());
    assert_eq!(
        answer
            .await
            .map_err(ctx("responder must deliver an answer"))?,
        None
    );
    Ok(())
}

/// Fix E (Teil 1b): dieselbe Erwartung wie beim Freigabe-Dialog
/// ([`ctrl_c_with_open_approval_dialog_cancels_turn_and_still_rejects_dialog`]),
/// hier für die Host-Permit-Frage — Ctrl+C bricht den Turn zusätzlich zum
/// bestehenden Verweigern der Frage kooperativ ab, statt nur die Frage
/// abzulehnen.
#[tokio::test]
async fn ctrl_c_with_open_host_permit_dialog_cancels_turn_and_still_denies_prompt() -> TestResult
{
    let (prompt, answer) = build_host_permit_prompt(HostPermitVariant::SingleExecution);
    let dialog = build_host_permit_dialog(&prompt);
    let mut app = test_chat_app()?;
    let cancel = CancelToken::new();
    app.active_cancel = Some(cancel.clone());
    app.deferred_input.push_back(TuiEvent::Key(KeyEvent::new(
        KeyCode::Char('x'),
        KeyModifiers::NONE,
    )));
    app.pending_host_permit_dialog = Some(dialog);
    app.pending_host_permit = Some(prompt);
    let mut pending: Option<PendingApprovalPrompt> = None;
    let mut dialog_shown_at: Option<Instant> = None;
    let mut host_permit_shown_at = Some(Instant::now());

    cancel_turn_and_reject_open_dialogs(
        &mut app,
        &mut pending,
        &mut dialog_shown_at,
        &mut host_permit_shown_at,
    )
    .await;

    assert!(
        cancel.is_cancelled(),
        "Ctrl+C muss den Turn auch bei offenem Host-Permit-Dialog kooperativ abbrechen"
    );
    assert!(
        matches!(
            app.pending_quit,
            Some(QuitArm {
                label: "Ctrl+C",
                ..
            })
        ),
        "Ctrl+C muss den zweistufigen Beenden-Hinweis scharfstellen"
    );
    assert_eq!(
        app.deferred_input.len(),
        1,
        "eingereihte Eingaben bleiben für die Auslieferung nach dem Abbruch"
    );
    assert!(app.queue_kept_at.is_some());
    assert!(app.pending_host_permit.is_none());
    assert!(app.pending_host_permit_dialog.is_none());
    assert!(host_permit_shown_at.is_none());

    // Bestehendes Verweigern bleibt zusätzlich bestehen, wird nicht ersetzt.
    assert_eq!(
        answer
            .await
            .map_err(ctx("responder must deliver an answer"))?,
        None,
        "Ctrl+C muss die Host-Permit-Frage weiterhin verweigern"
    );
    Ok(())
}

/// Rendert einen [`ChoiceDialog`] in einen ausreichend breiten Puffer und
/// gibt seinen sichtbaren Text als flachen String zurück — dasselbe
/// Muster wie `choice_dialog.rs`s eigene `test_render_contains_option_labels`.
fn rendered_choice_dialog(dialog: &ChoiceDialog) -> String {
    let area = Rect::new(0, 0, 90, 10);
    let mut buf = Buffer::empty(area);
    dialog.render(area, &mut buf, style::Theme::Dark);
    buf.content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<Vec<_>>()
        .join("")
}

/// B6: eine Sandbox-Lease-Anfrage
/// ([`harw_tool_shell::SANDBOX_LEASE_WORKER_DEFINITION`]) zeigt einen
/// eigenen Titel und rahmt `prompt.command()` als „Grund" statt als
/// auszuführenden Befehl.
#[test]
fn build_host_permit_dialog_uses_sandbox_lease_wording_for_the_lease_worker() -> TestResult {
    let (prompt, _answer) = build_host_permit_prompt_for(
        harw_tool_shell::SANDBOX_LEASE_WORKER_DEFINITION,
        "brauche Host-PATH für cargo",
        HostPermitVariant::SessionLease,
    );
    let dialog = build_host_permit_dialog(&prompt);
    let rendered = rendered_choice_dialog(&dialog);

    assert!(
        rendered.contains("Sandbox lease requested"),
        "rendered: {rendered}"
    );
    assert!(
        rendered.contains("Reason: brauche Host-PATH für cargo"),
        "rendered: {rendered}"
    );
    assert!(
        !rendered.contains("Allow host execution?"),
        "rendered: {rendered}"
    );
    Ok(())
}

/// Ein gewöhnlicher Host-Befehl (nicht die Sandbox-Lease-Worker-
/// Definition) behält die bisherige Formulierung bei.
#[test]
fn build_host_permit_dialog_keeps_the_command_wording_for_other_workers() -> TestResult {
    let (prompt, _answer) = build_host_permit_prompt(HostPermitVariant::SingleExecution);
    let dialog = build_host_permit_dialog(&prompt);
    let rendered = rendered_choice_dialog(&dialog);

    assert!(
        rendered.contains("Allow host execution?"),
        "rendered: {rendered}"
    );
    assert!(
        rendered.contains("host-process-worker@1"),
        "rendered: {rendered}"
    );
    assert!(
        !rendered.contains("Sandbox lease requested"),
        "rendered: {rendered}"
    );
    Ok(())
}

/// B6: `open_host_permit_prompt` — die Funktion, die
/// `drive_turn_animated`s neuer `host_permit_prompts.recv()`-Arm bei
/// einer während des laufenden Turns eintreffenden Frage aufruft —
/// öffnet sofort Dialog und Frage. Ein vollständiger Test des
/// `select!`-Loops selbst existiert nicht (kein Test-Terminal/-Gateway
/// für `drive_turn_animated` vorhanden); dieser Test deckt die
/// tatsächliche Zustandsänderung ab, die den Dialog sichtbar macht.
#[test]
fn host_permit_prompt_arrival_opens_the_dialog_during_a_running_turn() -> TestResult {
    let mut app = test_chat_app()?;
    assert!(app.pending_host_permit.is_none());
    assert!(app.pending_host_permit_dialog.is_none());

    let (prompt, _answer) = build_host_permit_prompt(HostPermitVariant::SingleExecution);
    let shown_at = open_host_permit_prompt(&mut app, prompt);

    assert!(app.pending_host_permit.is_some());
    assert!(app.pending_host_permit_dialog.is_some());
    assert!(shown_at.elapsed() < Duration::from_secs(1));
    Ok(())
}

/// `open_host_permit_prompt` lehnt eine noch offene ältere Frage ab,
/// statt sie still zu überschreiben (dieselbe K3-Regel wie beim
/// normalen Freigabe-Panel).
#[tokio::test]
async fn open_host_permit_prompt_denies_a_stale_open_prompt() -> TestResult {
    let mut app = test_chat_app()?;
    let (stale_prompt, stale_answer) =
        build_host_permit_prompt(HostPermitVariant::SingleExecution);
    open_host_permit_prompt(&mut app, stale_prompt);
    assert!(app.pending_host_permit.is_some());

    let (fresh_prompt, _fresh_answer) =
        build_host_permit_prompt(HostPermitVariant::SessionLease);
    open_host_permit_prompt(&mut app, fresh_prompt);

    assert_eq!(
        stale_answer
            .await
            .map_err(ctx("responder must deliver an answer"))?,
        None,
        "the stale prompt must be denied, not silently dropped"
    );
    assert!(app.pending_host_permit.is_some());
    Ok(())
}

/// `apply_host_permit_decision` mit Options-Index 0 genehmigt genau die
/// Einzelfreigabe — der eigentliche Ledger-/Registry-Pfad ist bereits in
/// `harw_tool_shell::exec`'s eigenen Tests abgedeckt; hier wird nur
/// geprüft, dass der App-seitige Dispatch die richtige Variante über den
/// Antwortkanal sendet und die erwartete Systemzeile anhängt.
#[tokio::test]
async fn apply_host_permit_decision_index_zero_approves_single_execution() -> TestResult {
    let (prompt, answer) = build_host_permit_prompt(HostPermitVariant::SingleExecution);
    let mut app = test_chat_app()?;
    apply_host_permit_decision(&mut app, prompt, 0);
    assert_eq!(
        answer
            .await
            .map_err(ctx("responder must deliver an answer"))?,
        Some(HostPermitVariant::SingleExecution)
    );
    assert!(
        app.cells.iter().any(|cell| cell
            .display_lines(80, app.theme)
            .iter()
            .any(|line| line_contains(line, "approved once"))),
        "a system line must confirm the single-execution approval"
    );
    Ok(())
}

/// `apply_host_permit_decision` mit Options-Index 1 genehmigt die
/// Host-Arbeitsphase.
#[tokio::test]
async fn apply_host_permit_decision_index_one_approves_session_lease() -> TestResult {
    let (prompt, answer) = build_host_permit_prompt(HostPermitVariant::SessionLease);
    let mut app = test_chat_app()?;
    apply_host_permit_decision(&mut app, prompt, 1);
    assert_eq!(
        answer
            .await
            .map_err(ctx("responder must deliver an answer"))?,
        Some(HostPermitVariant::SessionLease)
    );
    assert!(
        app.cells.iter().any(|cell| cell
            .display_lines(80, app.theme)
            .iter()
            .any(|line| line_contains(line, "HOST MODE ACTIVE"))),
        "a system line must confirm the session-lease approval"
    );
    Ok(())
}

/// Nutzerentscheidung 2026-09-24: unter „Full Access" wird eine
/// Host-Permit-Frage (`request_host`, Sandbox-Lease) ohne Dialog mit einer
/// Sitzungs-Lease beantwortet; unter `ask`/`auto` kommt sie zurück und der
/// Dialog öffnet wie bisher.
#[tokio::test]
async fn full_access_grants_a_host_session_lease_without_a_dialog() -> TestResult {
    let (prompt, answer) = build_host_permit_prompt(HostPermitVariant::SingleExecution);
    let mut app = test_chat_app()?;
    let handed_back =
        auto_grant_host_permit_in_mode(&mut app, prompt, Some(ApprovalMode::FullAccess));
    assert!(handed_back.is_none(), "Full Access beantwortet selbst");
    assert!(app.pending_host_permit.is_none());
    assert!(app.pending_host_permit_dialog.is_none());
    assert_eq!(
        answer
            .await
            .map_err(ctx("responder must deliver an answer"))?,
        Some(HostPermitVariant::SessionLease)
    );
    assert!(
        app.cells.iter().any(|cell| cell
            .display_lines(80, app.theme)
            .iter()
            .any(|line| line_contains(line, "HOST MODE ACTIVE"))),
        "a system line must confirm the automatic session lease"
    );

    for mode in [
        None,
        Some(ApprovalMode::AlwaysAsk),
        Some(ApprovalMode::Delegated),
    ] {
        let (prompt, _answer) = build_host_permit_prompt(HostPermitVariant::SingleExecution);
        assert!(
            auto_grant_host_permit_in_mode(&mut app, prompt, mode).is_some(),
            "{mode:?} muss den Dialog zeigen"
        );
    }
    Ok(())
}

/// Jeder andere Options-Index (hier: die „Nein"-Option, Index 2) lehnt ab
/// — fail-closed statt eines Panics bei einem unerwarteten Index.
#[tokio::test]
async fn apply_host_permit_decision_any_other_index_denies() -> TestResult {
    let (prompt, answer) = build_host_permit_prompt(HostPermitVariant::SingleExecution);
    let mut app = test_chat_app()?;
    apply_host_permit_decision(&mut app, prompt, 2);
    assert_eq!(
        answer
            .await
            .map_err(ctx("responder must deliver an answer"))?,
        None
    );
    Ok(())
}

/// Fällt der Fragekanal weg, ohne dass je geantwortet wurde (z. B. der
/// Dialog wird verworfen, ohne `approve`/`deny` aufzurufen), schließt
/// [`HostPermitPrompt`]s `Drop` den Antwortkanal — der Empfänger sieht
/// `Err`, nie eine stillschweigende Zustimmung. Dieselbe Sicherheitsregel
/// wie beim normalen Freigabe-Panel: Ablehnung ist der Default.
#[tokio::test]
async fn dropped_host_permit_prompt_fails_closed() -> TestResult {
    let (prompt, answer) = build_host_permit_prompt(HostPermitVariant::SingleExecution);
    drop(prompt);
    assert!(
        answer.await.is_err(),
        "a dropped prompt must close the answer channel instead of implicitly approving"
    );
    Ok(())
}

/// Kleiner Helfer, der eine gerenderte [`Line`] auf enthaltenen Text prüft
/// (Spans zu einem String zusammengefügt), ohne von der genauen
/// Span-Aufteilung abzuhängen.
fn line_contains(line: &Line<'static>, needle: &str) -> bool {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
        .contains(needle)
}
}

#[cfg(test)]
mod child_info_render_tests {
use super::tests::test_chat_app;
use super::*;
use crate::test_support::TestResult;

/// `note_child_info_at` füllt `child_infos`; ein zurückliegender
/// Zeitstempel liegt außerhalb des 30s-Fensters und wird beim Rendern
/// nicht prominent aufgenommen (`child_info_visible` auf dem
/// verstrichenen Alter).
#[test]
fn note_child_info_at_fills_map_and_old_entries_are_not_visible() -> TestResult {
    let mut app = test_chat_app()?;
    let at = std::time::Instant::now();
    app.note_child_info_at("uia-writer".to_owned(), "schreibe Abschnitt".to_owned(), at);
    assert!(app.child_infos.contains_key("uia-writer"));
    // Ein vor 60 s gestempelter Eintrag ist älter als das 30s-Fenster.
    let age_old = 60_u64;
    assert!(!child_info_visible(age_old));
    Ok(())
}

/// Das 30s-Sichtbarkeitsfenster: frisch sichtbar, ab 30 s nicht mehr.
#[test]
fn child_info_visible_window() -> TestResult {
    assert!(child_info_visible(0));
    assert!(child_info_visible(29));
    assert!(!child_info_visible(30));
    assert!(!child_info_visible(120));
    Ok(())
}

/// Kompaktformat abgelaufener Meldungen im `consolidate`-Modus:
/// nur Name und Alter.
#[test]
fn expired_consolidate_shows_compact_segment() {
    assert_eq!(child_info_expired_segment("writer", 60), "writer: 60s");
    assert_eq!(child_info_expired_segment("root", 123), "root: 123s");
}

/// Die beiden TOML-Modi sind unterscheidbar; `hide` erzeugt im
/// Renderzweig kein Segment (else-if greift nur bei `consolidate`).
#[test]
fn status_expiry_modes_distinct() {
    assert_ne!(
        harw_config::StatusExpiryMode::Hide,
        harw_config::StatusExpiryMode::Consolidate
    );
}
