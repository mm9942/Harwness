//! Golden-Test der Oberfläche des [`JobToolProvider`]: Namen, Spezifikations-
//! JSON und Parallelitäts-Zusagen (nur die vier lesenden Werkzeuge) müssen
//! exakt den vor der `tool_provider!`-Migration erfassten Werten entsprechen.
//! Siehe [`crate::test_support::golden`].

use crate::launcher::DirectLauncher;
use crate::test_support::golden::{assert_golden, surface};
use crate::test_support::{Env, TestResult};
use crate::tools::{JobToolProvider, NoLineage};
use std::sync::Arc;

#[test]
fn test_job_provider_surface_matches_golden() -> TestResult {
    let env = Env::new()?;
    let provider = JobToolProvider::new(Arc::clone(&env.manager), Arc::new(DirectLauncher))
        .with_lineage(Arc::new(NoLineage));

    assert_golden("job.json", &surface(&provider, None)?)
}
