use super::*;
use crate::i18n::{tr_args_for, tr_for, with_language};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

fn text(lines: Vec<Line<'static>>) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn call(name: &str, args: harw_tools::serde_json::Value) -> ToolCall {
    ToolCall {
        id: harw_types::ToolCallId::new(),
        name: harw_extension_api::ToolName::new(name),
        arguments: args,
    }
}

#[test]
fn captured_tool_completion_and_live_label_have_separate_locale_ownership() {
    let call = call(
        "fs.write",
        harw_tools::serde_json::json!({"path":"原文/फ़ाइल.rs","content":"DO NOT COPY THIS BODY"}),
    );
    for created in UiLanguage::ALL {
        let mut cell = with_language(created, || ToolCell::started(&call));
        assert!(
            matches!(cell.live_label.as_ref(), Some(ToolLabelPresentation::Template { args, .. }) if args.len() == 1 && args[0].1 == "原文/फ़ाइल.rs")
        );
        let stored = cell.label.clone();
        for live in UiLanguage::ALL {
            let shown = with_language(live, || text(cell.display_lines(200, style::Theme::Dark)));
            assert!(shown.contains(&tr_args_for(
                live,
                "tool_chrome.write",
                "Write({path})",
                &[("path", "原文/फ़ाइल.rs")]
            )));
            assert_eq!(cell.label, stored);
        }
        let result = ToolCallResult::success(
            harw_tools::serde_json::json!({"message":"原文\n{count} remains payload"}),
        );
        with_language(UiLanguage::Hindi, || cell.complete(&result, 7));
        assert_eq!(cell.full_output, ["message: 原文\n{count} remains payload"]);
        assert_eq!(cell.label, stored);
        assert!(cell.live_label.is_none());
    }
}

#[test]
fn reasoning_and_plain_history_keep_production_locale_and_raw_sources() {
    let source = "# 原文\n\n- फ़ाइल e\u{301}\n\n```rust\nlet x = \"{count}\";\n```";
    let plain = PlainHistoryCell {
        lines: vec![Line::raw("recorded Zeilen {count}")],
    };
    for created in UiLanguage::ALL {
        let cell = with_language(created, || {
            ReasoningHistoryCell::new(source).with_expanded(true)
        });
        let original = text(cell.display_lines(120, style::Theme::Dark));
        for live in UiLanguage::ALL {
            assert_eq!(
                with_language(live, || text(cell.display_lines(120, style::Theme::Dark))),
                original
            );
            assert_eq!(
                with_language(live, || text(plain.display_lines(120, style::Theme::Dark))),
                "recorded Zeilen {count}"
            );
            let live_header = with_language(live, || {
                crate::reasoning_cell::header_line(None, true, style::Theme::Dark)
            });
            assert_eq!(
                live_header.spans[0].content,
                tr_for(live, "history.reasoning", "∴ Reasoning")
            );
        }
        assert_eq!(cell.summary, source);
    }
}

#[test]
fn read_pdf_parser_and_unknown_error_shell_payloads_remain_literal() {
    let pdf = "report.pdf — Seiten 2–4 von 5 (lokal)\n原文 {count}\nफ़ाइल";
    let raw = "raw Zeilen: {count}\nstderr unverändert";
    for language in UiLanguage::ALL {
        with_language(language, || {
            let mut cell = ToolCell::started(&call(
                "doc.read_pdf",
                harw_tools::serde_json::json!({"path":"report.pdf"}),
            ));
            cell.complete(
                &ToolCallResult::success(harw_tools::serde_json::json!({"content":pdf})),
                1,
            );
            assert_eq!(cell.summary.as_deref(), Some("Seiten 2–4 von 5 (lokal)"));
            assert_eq!(cell.full_output.join("\n"), pdf);
            let mut unknown = ToolCell::started(&call(
                "extension.original",
                harw_tools::serde_json::json!({"key":"unübersetzt","nested":{"key":"value"}}),
            ));
            assert_eq!(unknown.label, "extension.original(key: \"unübersetzt\")");
            unknown.complete(&ToolCallResult::error(raw), 2);
            assert_eq!(unknown.full_output.join("\n"), raw);
            let mut shell = ToolCell::started(&call(
                "shell.exec",
                harw_tools::serde_json::json!({"command":"cat <<EOF\nDO NOT CACHE BODY\nEOF"}),
            ));
            assert!(
                matches!(shell.live_label.as_ref(), Some(ToolLabelPresentation::Shell { head, heredoc_lines: Some(1) }) if head == "cat <<EOF")
            );
            shell.complete(&ToolCallResult::success(harw_tools::serde_json::json!({"stdout":raw,"stderr":"ERROR 原文","exit_code":1})), 3);
            assert_eq!(
                shell.full_output,
                ["raw Zeilen: {count}", "stderr unverändert", "ERROR 原文"]
            );
        });
    }
}

#[test]
fn translated_tool_labels_and_controls_wrap_to_cells() {
    let call = call(
        "fs.edit",
        harw_tools::serde_json::json!({"path":"原文/é.rs"}),
    );
    for language in UiLanguage::ALL {
        with_language(language, || {
            let mut cell = ToolCell::started(&call);
            cell.complete(&ToolCallResult::success(harw_tools::serde_json::json!({"path":"原文/é.rs","replacements":2,"diff_excerpt":"-one\n+two\n-three\n+four\n-five\n+six"})), 1234);
            for width in [8, 12, 20, 40, 80] {
                for verbosity in [
                    ToolVerbosity::Compact,
                    ToolVerbosity::Verbose,
                    ToolVerbosity::Activity,
                ] {
                    for line in cell.display_lines_with(width, style::Theme::Dark, verbosity) {
                        assert!(
                            line.width() <= usize::from(width),
                            "{language:?}/{width}: {line:?}"
                        );
                    }
                }
            }
        });
    }
}

#[test]
fn wrapping_preserves_graphemes_and_width_clipping_preserves_char_limit_contract() {
    let source = "界面फ़ाइलé👩🏽‍💻/क्षि_家";
    let original = source.graphemes(true).collect::<Vec<_>>();
    for width in [2, 4, 8, 12] {
        let lines = wrap_plain(source, width);
        let rendered = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<String>();
        assert_eq!(rendered, source);
        for line in lines {
            let value = line
                .spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>();
            assert!(
                value.graphemes(true).all(|g| original.contains(&g)),
                "split grapheme: {value}"
            );
            assert!(
                UnicodeWidthStr::width(value.as_str()) <= usize::from(width)
                    || value.graphemes(true).count() == 1
            );
        }
        let styled = wrap_styled(Line::raw(source), usize::from(width));
        assert_eq!(
            styled
                .iter()
                .flat_map(|l| l.spans.iter())
                .map(|s| s.content.as_ref())
                .collect::<String>(),
            source
        );
    }
    // This remains a content-size helper for existing model-context consumers.
    assert_eq!(truncate_chars("界界界", 3), "界界界");
    assert_eq!(crate::status_line::fit_width("界界界", 3), "界…");
}

#[test]
fn goal_marker_uses_translated_display_cell_budget() {
    let marker = crate::goal_marker::GoalMarker {
        title: "界面 फ़ाइल é 👩🏽‍💻 long title".to_owned(),
        progress: Some((1, 4)),
        blocked: true,
    };
    for language in UiLanguage::ALL {
        with_language(language, || {
            for width in [0, 1, 8, 20, 40, 80, 120] {
                let spans =
                    crate::goal_marker::status_spans(Some(&marker), style::Theme::Dark, width);
                let shown = spans.iter().map(|s| s.content.as_ref()).collect::<String>();
                assert!(UnicodeWidthStr::width(shown.as_str()) <= usize::from(width));
            }
        });
    }
}

#[test]
fn resumed_completed_label_is_historical_and_group_summary_freezes_on_completion() {
    let call = call(
        "fs.read",
        harw_tools::serde_json::json!({"path":"unchanged.rs"}),
    );
    let (shared, mut group) = with_language(UiLanguage::German, || {
        let cell = Arc::new(Mutex::new(ToolCell::started(&call)));
        let mut group = ToolGroupCell::new();
        group.push(Arc::clone(&cell));
        (cell, group)
    });
    let running = with_language(UiLanguage::French, || {
        text(group.display_lines(120, style::Theme::Dark))
    });
    assert!(running.contains("1 fichier lu"));
    {
        let mut cell = shared.lock().expect("cell");
        with_language(UiLanguage::Hindi, || {
            cell.complete(
                &ToolCallResult::success(harw_tools::serde_json::json!({"content":"raw"})),
                1,
            )
        });
        cell.resume_call(&call);
        assert!(cell.live_label.is_none());
    }
    let completed = with_language(UiLanguage::French, || {
        text(group.display_lines(120, style::Theme::Dark))
    });
    assert!(completed.contains("1 Datei gelesen"));
    group.expanded = true;
    let expanded = with_language(UiLanguage::Japanese, || {
        text(group.display_lines(120, style::Theme::Dark))
    });
    assert!(expanded.contains("Lesen(unchanged.rs)"));
    assert!(expanded.contains("1 Zeilen"));
}

#[test]
fn plan_and_goal_owned_labels_keep_locale_and_fit_translated_widths() {
    for language in UiLanguage::ALL {
        let mut plan = harw_plan::testing::base_plan();
        plan.nodes = vec![harw_plan::testing::base_node("node-original")];
        plan.nodes[0].objective = "原文 फ़ाइल é".to_owned();
        let mut changed = plan.clone();
        changed.nodes[0].status = PlanNodeStatus::Completed;
        let mut cell = with_language(language, || PlanGraphCell::delta(&plan, changed));
        let goal = GoalCell {
            language,
            statement: "原文 फ़ाइल é".to_owned(),
            report: GoalReport {
                criteria_met: vec![0],
                criteria_open: vec![1],
                invariants_violated: vec!["id-original".to_owned()],
                coverage: 0.5,
                blocking_nodes: vec![],
                next_actions: vec![],
            },
        };
        let original = text(goal.display_lines(120, style::Theme::Dark));
        assert_eq!(
            with_language(UiLanguage::English, || text(
                goal.display_lines(120, style::Theme::Dark)
            )),
            original
        );
        assert_eq!(goal.statement, "原文 फ़ाइल é");
        for expanded in [false, true] {
            cell.expanded = expanded;
            for width in [8, 12, 20, 40, 80] {
                for line in cell
                    .display_lines(width, style::Theme::Dark)
                    .into_iter()
                    .chain(goal.display_lines(width, style::Theme::Dark))
                {
                    assert!(
                        line.width() <= usize::from(width),
                        "{language:?}/{width}: {line:?}"
                    );
                }
            }
        }
        assert_eq!(plan.nodes[0].status, PlanNodeStatus::Draft);
    }
}

#[test]
fn soft_break_separators_keep_attached_combining_marks_on_original_graphemes() {
    for separator in ['/', '·', '_'] {
        let source = format!("abcdefg{separator}\u{301}z");
        let boundaries = source
            .grapheme_indices(true)
            .map(|(index, _)| index)
            .chain(std::iter::once(source.len()))
            .collect::<Vec<_>>();
        for width in [1, 2, 4, 8] {
            let mut offset = 0;
            for line in wrap_plain(&source, width) {
                let value = line
                    .spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>();
                assert!(
                    boundaries.contains(&offset),
                    "start inside grapheme: {source:?}/{width}/{offset}"
                );
                assert_eq!(
                    source.get(offset..offset + value.len()),
                    Some(value.as_str())
                );
                offset += value.len();
                assert!(
                    boundaries.contains(&offset),
                    "end inside grapheme: {source:?}/{width}/{offset}"
                );
                assert!(UnicodeWidthStr::width(value.as_str()) <= usize::from(width));
            }
            assert_eq!(offset, source.len());
        }
    }
}
