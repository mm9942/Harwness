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

#[test]
fn sole_completed_job_start_keeps_live_projection_title_until_runtime_ends() -> TestResult {
    let mut app = tests::test_chat_app()?;
    app.language = UiLanguage::Japanese;
    let mut state = TurnEventState::default();
    let turn = harw_types::TurnId::new();
    let call_id = harw_types::ToolCallId::new();
    let result =
        ToolCallResult::success(serde_json::json!({"job_id":"job-sole","state":"running"}));
    handle_turn_event(
        &mut app,
        &mut state,
        TurnEvent::ToolCallRequested {
            turn_id: turn.clone(),
            call_id: call_id.clone(),
            tool_name: "job.start".to_owned(),
            arguments: serde_json::json!({"name":"build","command":"make original"}),
        },
    );
    handle_turn_event(
        &mut app,
        &mut state,
        TurnEvent::ToolCallCompleted {
            turn_id: turn,
            call_id,
            result: result.clone(),
            duration_ms: 3,
            placement: None,
        },
    );
    let group = app
        .tool_cells
        .iter()
        .find_map(|handle| match handle {
            ToolCellHandle::Group(group) => Some(Arc::clone(group)),
            _ => None,
        })
        .expect("job projection");
    let original_instance = {
        let group = group.lock().expect("group");
        assert_eq!(group.cells.len(), 1);
        let cell = group.cells[0].lock().expect("start cell");
        assert_eq!(cell.state, crate::history_cell::ToolState::Succeeded);
        assert_eq!(cell.label, "ジョブ(build)");
        let crate::history_cell::ToolGroupKind::Activity(projection) = &group.kind else {
            panic!("expected activity projection");
        };
        assert!(!projection.instance.is_terminal());
        assert_eq!(projection.instance.state.counts.running, 1);
        projection.instance.clone()
    };
    let export_count = app.export_entries.len();
    app.language = UiLanguage::French;
    let render = |app: &ChatApp, verbosity, expanded| {
        let mut group = group.lock().expect("group");
        group.expanded = expanded;
        crate::i18n::with_language(app.language, || {
            group.display_lines_with(120, style::Theme::Dark, verbosity)
        })
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|span| span.content.as_ref())
        .collect::<String>()
    };
    for expanded in [false, true] {
        for verbosity in [
            ToolVerbosity::Compact,
            ToolVerbosity::Verbose,
            ToolVerbosity::Activity,
        ] {
            let shown = render(&app, verbosity, expanded);
            assert!(shown.contains("Tâche(build)"), "{shown}");
            assert!(!shown.contains("ジョブ(build)"), "{shown}");
        }
    }
    {
        let group = group.lock().expect("group");
        let crate::history_cell::ToolGroupKind::Activity(projection) = &group.kind else {
            panic!("projection");
        };
        assert_eq!(projection.instance, original_instance);
        assert_eq!(
            group.cells[0].lock().expect("start cell").label,
            "ジョブ(build)"
        );
    }
    app.activity
        .sync_jobs(&[crate::mission::projection::tests::job_in(
            "job-sole",
            harw_tool_job::JobState::Succeeded,
        )]);
    for expanded in [false, true] {
        for verbosity in [
            ToolVerbosity::Compact,
            ToolVerbosity::Verbose,
            ToolVerbosity::Activity,
        ] {
            let shown = render(&app, verbosity, expanded);
            assert!(shown.contains("ジョブ(build)"), "{shown}");
            assert!(!shown.contains("Tâche(build)"), "{shown}");
        }
    }
    let group = group.lock().expect("group");
    let crate::history_cell::ToolGroupKind::Activity(projection) = &group.kind else {
        panic!("projection");
    };
    assert_eq!(projection.instance.id, original_instance.id);
    assert!(projection.instance.is_terminal());
    assert_eq!(group.cells.len(), 1);
    assert_eq!(app.export_entries.len(), export_count);
    assert!(app.export_entries.iter().any(|entry| matches!(entry, ExportEntry::ToolCall { arguments, .. } if *arguments == serde_json::json!({"name":"build","command":"make original"}))));
    assert!(app.export_entries.iter().any(|entry| matches!(entry, ExportEntry::ToolResult { result: exported, .. } if *exported == export_tool_result_value(&result))));
    Ok(())
}
