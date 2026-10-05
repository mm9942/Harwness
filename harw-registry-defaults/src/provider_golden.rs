//! Golden-Tests der Oberfläche der handgeschriebenen Registry-Default-Provider
//! (Palace, Skill-Katalog, Kanban lesend, Workbench schreibend/lesend, Diary):
//! Namen, Spezifikations-JSON, Parallelitäts-Zusage und Berechtigungen müssen
//! exakt den vor der `tool_provider!`-Migration erfassten Werten entsprechen.
//! Siehe [`crate::test_support::golden`].

use std::path::Path;
use std::sync::Arc;

use harw_catalog::SkillIndex;
use harw_knowledge::{AgentId, KnowledgeStore};

use crate::diary_tools::DiaryToolProvider;
use crate::kanban_tools::KanbanReadToolProvider;
use crate::palace_tools::PalaceToolProvider;
use crate::skill_tools::SkillCatalogToolProvider;
use crate::test_support::TestResult;
use crate::test_support::golden::{assert_golden, surface};
use crate::workbench_tools::{WorkbenchReadToolProvider, WorkbenchToolProvider};

fn store() -> Arc<KnowledgeStore> {
    Arc::new(KnowledgeStore::new(Path::new("/x")))
}

#[test]
fn test_palace_provider_surface_matches_golden() -> TestResult {
    let provider = PalaceToolProvider::new(store());

    assert_golden(
        "palace.json",
        &surface(&provider, Some(PalaceToolProvider::TOOL_PERMISSIONS))?,
    )
}

#[test]
fn test_skill_catalog_provider_surface_matches_golden() -> TestResult {
    let provider = SkillCatalogToolProvider::new(Arc::new(SkillIndex::build(&[])));

    assert_golden(
        "skill_catalog.json",
        &surface(&provider, Some(SkillCatalogToolProvider::TOOL_PERMISSIONS))?,
    )
}

#[test]
fn test_kanban_read_provider_surface_matches_golden() -> TestResult {
    let provider = KanbanReadToolProvider::new(store());

    assert_golden(
        "kanban_read.json",
        &surface(&provider, Some(KanbanReadToolProvider::TOOL_PERMISSIONS))?,
    )
}

#[test]
fn test_workbench_provider_surface_matches_golden() -> TestResult {
    let provider = WorkbenchToolProvider::new(store());

    assert_golden(
        "workbench.json",
        &surface(&provider, Some(WorkbenchToolProvider::TOOL_PERMISSIONS))?,
    )
}

#[test]
fn test_workbench_read_provider_surface_matches_golden() -> TestResult {
    let provider = WorkbenchReadToolProvider::new(store());

    assert_golden(
        "workbench_read.json",
        &surface(&provider, Some(WorkbenchReadToolProvider::TOOL_PERMISSIONS))?,
    )
}

#[test]
fn test_diary_provider_surface_matches_golden() -> TestResult {
    let provider = DiaryToolProvider::new(store(), AgentId::new("explorer"));

    assert_golden(
        "diary.json",
        &surface(&provider, Some(DiaryToolProvider::TOOL_PERMISSIONS))?,
    )
}
