//! Actual app boundaries: live locale changes preserve drafts, historical cells,
//! action indices, prompt data and the delay used to arm host decisions.
use super::tests::test_chat_app;
use super::*;
use crate::test_support::{TestResult, ctx, display_buffer_text};
use harw_config::{TuiSection, UiLanguage};
use std::path::PathBuf;

fn language(app: &mut ChatApp, language: UiLanguage) {
    app.apply_display_preferences(&TuiSection {
        language,
        ..TuiSection::default()
    });
}

fn screen(app: &ChatApp, width: u16, height: u16) -> TestResult<String> {
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
        .map_err(ctx("terminal"))?;
    terminal
        .draw(|frame| render_viewport(frame, app, &Spinner::new(), None))
        .map_err(ctx("draw"))?;
    Ok(display_buffer_text(terminal.backend().buffer()))
}

#[test]
fn chrome_updates_next_frame_without_changing_draft_or_history() -> TestResult {
    let mut app = test_chat_app()?;
    app.push_line(Role::User, "verbatim user 漢字 नमस्ते");
    app.push_line(Role::System, "historical notice bleibt unverändert");
    app.input.insert_str("! echo 'payload 漢字 नमस्ते'");
    let draft = app.input.text().to_owned();
    let cursor = app.input.cursor();
    let history = format!("{:?}", app.export_entries);
    let count = app.cells_len();
    assert!(screen(&app, 120, 24)?.contains("Shell mode"));
    language(&mut app, UiLanguage::Japanese);
    let shown = screen(&app, 120, 24)?;
    assert!(shown.contains("シェルモード"), "{shown}");
    assert!(shown.contains("モード: chat"), "{shown}");
    assert!(shown.contains("verbatim user 漢字 नमस्ते"), "{shown}");
    assert_eq!(app.input.text(), draft);
    assert_eq!(app.input.cursor(), cursor);
    assert_eq!(app.cells_len(), count);
    assert_eq!(format!("{:?}", app.export_entries), history);
    // Every locale, tiny through normal terminals plus the mission placements.
    for locale in UiLanguage::ALL {
        language(&mut app, locale);
        for (width, height) in [
            (1, 1),
            (8, 6),
            (20, 8),
            (24, 12),
            (40, 20),
            (60, 24),
            (80, 40),
            (140, 14),
            (160, 45),
        ] {
            screen(&app, width, height)?;
        }
    }
    assert_eq!(app.input.text(), draft);
    assert_eq!(format!("{:?}", app.export_entries), history);
    Ok(())
}

#[test]
fn export_and_effort_choices_refresh_semantically_preserving_actions() -> TestResult {
    let mut app = test_chat_app()?;
    app.input.insert_str("draft to keep");
    app.open_export_choice();
    if let Some(Overlay::ExportChoice(dialog)) = &mut app.overlay {
        dialog.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    language(&mut app, UiLanguage::Japanese);
    let shown = screen(&app, 100, 16)?;
    assert!(shown.contains("ファイルに保存"), "{shown}");
    if let Some(Overlay::ExportChoice(dialog)) = &mut app.overlay {
        assert_eq!(
            dialog.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ChoiceAction::Chosen(1)
        );
    } else {
        panic!("export choice disappeared");
    }
    for target in [EffortTarget::Session, EffortTarget::Uia] {
        app.open_effort_choice(target);
        if let Some(Overlay::EffortChoice { dialog, .. }) = &mut app.overlay {
            for _ in 0..EFFORT_LEVELS.len() {
                dialog.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
            }
        }
        language(&mut app, UiLanguage::Hindi);
        let shown = screen(&app, 120, 16)?;
        assert!(shown.contains("रीसेट"), "{shown}");
        if let Some(Overlay::EffortChoice {
            dialog,
            target: actual,
        }) = &mut app.overlay
        {
            assert_eq!(*actual, target);
            assert_eq!(
                dialog.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                ChoiceAction::Chosen(EFFORT_LEVELS.len())
            );
        } else {
            panic!("effort choice disappeared");
        }
        language(&mut app, UiLanguage::Japanese);
    }
    assert_eq!(app.input.text(), "draft to keep");
    Ok(())
}

#[tokio::test]
async fn host_choice_refresh_keeps_prompt_selection_and_arming_time() -> TestResult {
    for worker in [
        "host-process-worker@1",
        harw_tool_shell::SANDBOX_LEASE_WORKER_DEFINITION,
    ] {
        let mut app = test_chat_app()?;
        language(&mut app, UiLanguage::Japanese);
        let (prompt, answer) = HostPermitPrompt::new(
            "session-unchanged".into(),
            worker.into(),
            "echo 'payload {command} 漢字'".into(),
            PathBuf::from("/workspace/unchanged"),
            HostPermitVariant::SessionLease,
        );
        let shown_at = open_host_permit_prompt(&mut app, prompt);
        let shown = screen(&app, 120, 24)?;
        assert!(shown.contains("ホスト作業フェーズ"), "{shown}");
        language(&mut app, UiLanguage::French);
        let shown = screen(&app, 120, 24)?;
        assert!(shown.contains("Phase de travail hôte"), "{shown}");
        assert!(
            shown.contains("echo 'payload") && shown.contains("{command} 漢字'"),
            "{shown}"
        );
        let prompt = app.pending_host_permit.take().expect("same pending prompt");
        assert_eq!(prompt.worker_definition(), worker);
        assert_eq!(prompt.command(), "echo 'payload {command} 漢字'");
        let mut dialog = app.pending_host_permit_dialog.take().expect("dialog");
        assert_eq!(
            dialog.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            ChoiceAction::Chosen(1)
        );
        assert!(!approval_dialog_key_is_armed(
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            Duration::ZERO
        ));
        assert!(shown_at <= Instant::now());
        apply_host_permit_decision(&mut app, prompt, 1);
        assert_eq!(
            answer.await.map_err(ctx("host answer"))?,
            Some(HostPermitVariant::SessionLease)
        );
        let history = format!("{:?}", app.export_entries);
        let cells = app.cells_len();
        language(&mut app, UiLanguage::German);
        assert_eq!(app.cells_len(), cells);
        assert_eq!(format!("{:?}", app.export_entries), history);
        assert!(screen(&app, 120, 24)?.contains("Phase de travail hôte autorisée"));
    }
    Ok(())
}

#[test]
fn missing_config_notice_uses_app_locale_at_constructor_boundary() -> TestResult {
    let mut app = test_chat_app()?;
    language(&mut app, UiLanguage::Japanese);
    // No separately installed TLS scope: the actual constructor owns it.
    app.open_model_switch_picker(PickerTarget::Uia);
    assert!(app.overlay.is_none());
    let shown = screen(&app, 120, 24)?;
    assert!(shown.contains("設定が読み込まれていません"), "{shown}");
    assert_eq!(crate::i18n::tr("settings.title", "Settings"), "Settings");
    Ok(())
}

#[test]
fn host_warning_remains_visible_and_cell_bounded_at_tiny_widths() {
    for locale in UiLanguage::ALL {
        crate::i18n::with_language(locale, || {
            for width in [0, 1, 8, 20, 24, 40, 60, 80] {
                let text = host_mode_mark(width);
                assert!(status_line::display_width(&text) <= usize::from(width));
                if width > 0 {
                    assert!(!text.trim().is_empty());
                }
                if width == 1 {
                    assert_eq!(text, "!");
                }
            }
        });
    }
}

#[test]
fn paste_projection_keeps_real_composer_and_popup_byte_coordinates() -> TestResult {
    let mut app = test_chat_app()?;
    app.input.insert_str("before ");
    let payload = "original 漢字 नमस्ते 👩‍💻\n".repeat(20);
    app.input.insert_paste(&payload);
    app.input.insert_str(" after");
    let raw = app.input.text().to_owned();
    let cursor = app.input.cursor();
    let id = app.input.pastes()[0].id;
    for locale in UiLanguage::ALL {
        language(&mut app, locale);
        // The same actual input/render locale boundary must still yield raw
        // byte offsets for completion, including a cursor inside the marker.
        crate::i18n::with_language(locale, || {
            let mut editor = app.input.clone();
            for _ in 0..raw.chars().count() {
                assert_eq!(editor_cursor_byte(&editor), editor.cursor(), "{locale:?}");
                editor.move_left();
            }
        });
        let shown = screen(&app, 120, 24)?;
        if locale == UiLanguage::Japanese {
            assert!(shown.contains("貼り付けテキスト"), "{shown}");
        }
        assert_eq!(app.input.text(), raw);
        assert_eq!(app.input.cursor(), cursor);
        assert_eq!(app.input.pastes()[0].id, id);
        assert_eq!(
            app.input.submission_text(),
            format!("before {payload} after")
        );
    }
    Ok(())
}
