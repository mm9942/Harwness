//! Golden-Test der Oberfläche des [`PlanToolProvider`]: Namen, Spezifikations-
//! JSON, Parallelitäts-Zusage und Berechtigungen müssen exakt den vor der
//! `tool_provider!`-Migration erfassten Werten entsprechen. Siehe
//! [`crate::test_support::golden`].

use crate::plan_file::PlanDir;
use crate::provider::PlanToolProvider;
use crate::session::PlanSession;
use crate::test_support::TestResult;
use crate::test_support::golden::{assert_golden, surface};

#[test]
fn test_plan_provider_surface_matches_golden() -> TestResult {
    let provider = PlanToolProvider::new(
        PlanSession::new(PlanDir::new("/p/.harw/plans"), false),
        None,
    );

    assert_golden(
        "plan.json",
        &surface(&provider, Some(PlanToolProvider::TOOL_PERMISSIONS))?,
    )
}
