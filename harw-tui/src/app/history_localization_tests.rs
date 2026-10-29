use super::*;
use crate::test_support::TestResult;
use harw_config::UiLanguage;

#[test]
fn interleaved_apps_complete_in_creation_locale_after_language_switch() -> TestResult {
    let mut german = tests::test_chat_app()?;
    let mut japanese = tests::test_chat_app()?;
    german.language = UiLanguage::German;
    japanese.language = UiLanguage::Japanese;
    let mut gs = TurnEventState::default();
    let mut js = TurnEventState::default();
    let turn = harw_types::TurnId::new();
    let gcall = harw_types::ToolCallId::new();
    let jcall = harw_types::ToolCallId::new();
    for (app, state, id) in [
        (&mut german, &mut gs, &gcall),
        (&mut japanese, &mut js, &jcall),
    ] {
        handle_turn_event(
            app,
            state,
            TurnEvent::ToolCallRequested {
                turn_id: turn.clone(),
                call_id: id.clone(),
                tool_name: "fs.read".to_owned(),
                arguments: serde_json::json!({"path":"original/原文.rs"}),
            },
        );
    }
    german.language = UiLanguage::Hindi;
    japanese.language = UiLanguage::French;
    let payload = "original Zeilen {count}\nदूसरी पंक्ति";
    for (app, state, id) in [
        (&mut japanese, &mut js, &jcall),
        (&mut german, &mut gs, &gcall),
    ] {
        handle_turn_event(
            app,
            state,
            TurnEvent::ToolCallCompleted {
                turn_id: turn.clone(),
                call_id: id.clone(),
                result: ToolCallResult::success(serde_json::json!({"content":payload})),
                duration_ms: 42,
                placement: None,
            },
        );
    }
    for (app, expected_label, expected_summary) in [
        (&german, "Lesen(original/原文.rs)", "2 Zeilen"),
        (&japanese, "読み取り(original/原文.rs)", "2 行"),
    ] {
        let group = app
            .tool_cells
            .iter()
            .find_map(|handle| match handle {
                ToolCellHandle::Group(group) => Some(group),
                _ => None,
            })
            .expect("read group");
        let group = group.lock().expect("group");
        let cell = group.cells[0].lock().expect("cell");
        assert_eq!(cell.label, expected_label);
        assert_eq!(cell.summary.as_deref(), Some(expected_summary));
        assert_eq!(cell.full_output.join("\n"), payload);
        assert!(app.export_entries.iter().any(|entry| matches!(entry, ExportEntry::ToolResult { result, .. } if *result == export_tool_result_value(&ToolCallResult::success(serde_json::json!({"content":payload}))))));
    }
    Ok(())
}
