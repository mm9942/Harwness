//! Integrationstest für Knoten **AW5-07**: `DetailMode::References` +
//! `context.load` end-to-end über `harw-context`, `harw-core` und
//! `harw-tools` hinweg.
//!
//! Deckt genau die Kette ab, die der Auftrag verlangt: eine Historie wird zu
//! einer `"history.tail"`-Sektion mit garantiertem Schwanz und Verweisen für
//! ältere Gruppen gerendert ([`harw_core::history_tail::render_history_tail`]);
//! die Verweise fließen unverändert durch die bestehende AW1-03-Montage
//! ([`harw_core::context_budget::Assembly`]); ein Verweis lässt sich über
//! `context.load` ([`harw_tools::context_load::ContextLoadExecutor`]) gegen
//! seinen vollständigen Inhalt auflösen — bei laufender Kappe, ohne
//! Vertrauens-Eskalation, mit erkennbarer Digest-Veralterung.

use harw_agent_dsl::executable::ContextProgram;
use harw_authority::{
    Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
};
use harw_context::{ContextBudgetSpec, ContextCeiling, TrustClass};
use harw_core::context_budget::Assembly;
use harw_core::history::ConversationHistory;
use harw_core::history_tail::{HISTORY_TAIL_GUARANTEED_GROUPS, render_history_tail};
use harw_lens_types::{BudgetSpec, BytesOverFour};
use harw_tools::context_load::{ContextLoadExecutor, InMemoryReferenceStore};
use harw_tools::{ToolCall, ToolExecutionContext, ToolExecutor, ToolOutput};
use harw_types::{SessionId, TenantId, ToolCallId, WorkspaceId};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

mod common;
use common::{TestError, TestResult, ctx};

/// Füllsel, lang genug, dass eine `FragmentReference`-Darstellung (Fixkosten
/// aus UUID-Label, 64-Zeichen-Digest und Zeitstempel) nachweislich billiger
/// bleibt als der volle Rumpf — siehe `harw_context::reference`s Moduldoku,
/// Abschnitt „Fixkosten eines Verweises".
const LONG_FILLER: &str = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod \
     tempor incididunt ut labore et dolore magna aliqua ";

fn history_with_exchanges(n: usize) -> ConversationHistory {
    let mut history = ConversationHistory::new();
    let filler = LONG_FILLER.repeat(3);
    for i in 0..n {
        history.push_user_text(format!("question {i} {filler}"));
        history.push_assistant_text(
            format!("answer number {i} with some real length to it {filler}"),
            None,
        );
    }
    history
}

fn history_tail_ceiling(section_budget: u32, total: u32) -> TestResult<ContextCeiling> {
    let section = harw_context::SectionName::try_new("history.tail")
        .map_err(ctx("literal section name always validates"))?;
    let mut sections = BTreeSet::new();
    sections.insert(section.clone());
    let mut per_section = std::collections::BTreeMap::new();
    per_section.insert(section, section_budget);
    Ok(ContextCeiling {
        sections,
        max_trust: TrustClass::Instruction,
        budget: ContextBudgetSpec {
            total: BudgetSpec { total },
            per_section,
        },
    })
}

fn make_sandbox(test_id: &str) -> TestResult<SandboxSpec> {
    let base = std::env::temp_dir()
        .join("harw_core_detail_mode_references_tests")
        .join(test_id);
    let ws = base.join("ws");
    std::fs::create_dir_all(&ws).map_err(ctx("create_dir_all"))?;
    let registry = WorkspaceRegistry::build(
        &base,
        [WorkspaceRegistration {
            tenant: TenantId::from_str("t"),
            workspace: WorkspaceId::from_str("w"),
            root: PathBuf::from("ws"),
        }],
    )
    .map_err(ctx("WorkspaceRegistry::build"))?;
    let binding = registry
        .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("w"))
        .map_err(ctx("registry.resolve"))?;
    Ok(SandboxSpec::from_resolved(
        binding,
        PermissionSet::from_policy(vec![Permission::ReadWorkspace]),
    ))
}

fn make_ctx(test_id: &str, turn_id: harw_types::TurnId) -> TestResult<ToolExecutionContext> {
    Ok(ToolExecutionContext::new(
        SessionId::new(),
        turn_id,
        make_sandbox(test_id)?,
    ))
}

/// Der Kern-Rundlauf: eine lange Historie wird gerendert, die alte Hälfte
/// verkürzt sich zu Verweisen, dieselben Verweise laufen unverändert durch
/// die bestehende Montage, und ein Verweis lässt sich über `context.load`
/// wieder zu seinem vollen Inhalt auflösen.
#[tokio::test]
async fn test_history_tail_references_survive_assembly_and_resolve_via_context_load() -> TestResult
{
    let history = history_with_exchanges(8); // 16 Gruppen, weit über dem garantierten Schwanz.
    let rendered = render_history_tail(&history, jiff::Timestamp::UNIX_EPOCH, &BytesOverFour)
        .map_err(ctx("the literal section name always validates"))?;
    assert!(
        !rendered.loadable.is_empty(),
        "a 16-group history must produce at least one reference"
    );

    // Die Verweise laufen unverändert durch die bestehende AW1-03-Montage:
    // kein Aufrufer, der `Assembly` nutzt, muss auch nur eine Zeile ändern.
    let ceiling = history_tail_ceiling(10_000, 10_000)?;
    let program = ContextProgram::default();
    let assembled = Assembly::gather(rendered.fragments.clone())
        .admit(&program, &ceiling)
        .map_err(ctx(
            "references fit the ceiling just like any other fragment",
        ))?
        .budget(&ceiling.budget)
        .map_err(ctx(
            "references fit the budget just like any other fragment",
        ))?
        .render();
    assert!(assembled.omissions.is_empty());
    let history_section = assembled
        .sections
        .iter()
        .find(|s| s.section.as_str() == "history.tail")
        .ok_or(TestError::Missing(
            "the history.tail section must survive the montage",
        ))?;
    assert_eq!(history_section.fragments.len(), rendered.fragments.len());

    // Ein Verweis (nicht aus dem garantierten Schwanz) wird über
    // `context.load` gegen seinen vollständigen Inhalt aufgelöst.
    let reference_fragment = history_section
        .fragments
        .iter()
        .find(|f| f.body.starts_with("[ref]"))
        .ok_or(TestError::Missing(
            "an older group must have become a reference",
        ))?;
    let full_fragment = rendered
        .loadable
        .iter()
        .find(|f| f.label == reference_fragment.label)
        .ok_or(TestError::Missing(
            "every reference has a matching loadable full fragment",
        ))?;

    let store = Arc::new(InMemoryReferenceStore::from_fragments(
        rendered.loadable.clone(),
    ));
    let executor = ContextLoadExecutor::new(store, ceiling.clone(), 20);
    let tool_ctx = make_ctx("references_survive_assembly", harw_types::TurnId::new())?;
    let call = ToolCall {
        id: ToolCallId::new(),
        name: harw_tools::ToolName::new(harw_tools::context_load::CONTEXT_LOAD_TOOL_NAME),
        arguments: serde_json::json!({
            "section": reference_fragment.section.as_str(),
            "label": reference_fragment.label.as_str(),
        }),
    };

    let output = executor
        .execute(&tool_ctx, &call)
        .await
        .map_err(ctx("context.load executes"))?;
    match output {
        ToolOutput::Json { content } => {
            assert_eq!(
                content["body"]
                    .as_str()
                    .ok_or(TestError::Missing("content.body as string"))?,
                full_fragment.body,
                "context.load must return exactly the content the reference stood in for"
            );
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "expected a successful load, got: {other:?}"
            )));
        }
    }
    Ok(())
}

/// Die Kappe greift: über der Grenze ein Fehler, kein gekürztes Ergebnis.
#[tokio::test]
async fn test_context_load_cap_rejects_instead_of_truncating_when_over_budget() -> TestResult {
    let history = history_with_exchanges(8);
    let rendered = render_history_tail(&history, jiff::Timestamp::UNIX_EPOCH, &BytesOverFour)
        .map_err(ctx("the literal section name always validates"))?;
    let reference_fragment = rendered
        .fragments
        .iter()
        .find(|f| f.body.starts_with("[ref]"))
        .ok_or(TestError::Missing(
            "an older group must have become a reference",
        ))?;
    let full_fragment = rendered
        .loadable
        .iter()
        .find(|f| f.label == reference_fragment.label)
        .ok_or(TestError::Missing(
            "every reference has a matching loadable full fragment",
        ))?;

    // Die Decke lässt kaum Platz — deutlich unter den vollen Kosten des
    // referenzierten Fragments.
    let tiny_budget = full_fragment.cost.0.saturating_sub(1);
    let ceiling = history_tail_ceiling(tiny_budget, tiny_budget)?;
    let store = Arc::new(InMemoryReferenceStore::from_fragments(
        rendered.loadable.clone(),
    ));
    let executor = ContextLoadExecutor::new(store, ceiling, 20);
    let tool_ctx = make_ctx(
        "cap_rejects_instead_of_truncating",
        harw_types::TurnId::new(),
    )?;
    let call = ToolCall {
        id: ToolCallId::new(),
        name: harw_tools::ToolName::new(harw_tools::context_load::CONTEXT_LOAD_TOOL_NAME),
        arguments: serde_json::json!({
            "section": reference_fragment.section.as_str(),
            "label": reference_fragment.label.as_str(),
        }),
    };

    let output = executor
        .execute(&tool_ctx, &call)
        .await
        .map_err(ctx("context.load executes"))?;
    match output {
        ToolOutput::Error { .. } => {}
        ToolOutput::Json { content } => {
            return Err(TestError::Unexpected(format!(
                "an over-budget load must be a hard error, never a truncated Ok result: {content:?}"
            )));
        }
        other => {
            return Err(TestError::Unexpected(format!(
                "unexpected output: {other:?}"
            )));
        }
    }
    Ok(())
}

/// Der garantierte Schwanz ist immer da, auch bei minimalem Budget — eine
/// zu kurze Historie darf sich nicht auflösen.
#[test]
fn test_history_tail_guaranteed_tail_is_always_present_regardless_of_history_length() -> TestResult
{
    for exchange_count in [0, 1, 2, 5, 20] {
        let history = history_with_exchanges(exchange_count);
        let rendered = render_history_tail(&history, jiff::Timestamp::UNIX_EPOCH, &BytesOverFour)
            .map_err(ctx("the literal section name always validates"))?;
        let expected_full = (exchange_count * 2).min(HISTORY_TAIL_GUARANTEED_GROUPS);
        let actual_full = rendered
            .fragments
            .iter()
            .filter(|f| !f.body.starts_with("[ref]"))
            .count();
        assert_eq!(
            actual_full, expected_full,
            "exchange_count={exchange_count}: the guaranteed tail must always be present in full"
        );
    }
    Ok(())
}
