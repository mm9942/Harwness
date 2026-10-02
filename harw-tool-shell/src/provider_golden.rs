//! Golden-Tests der Provider-Oberfläche (`shell.exec`, `latex.*`,
//! `host.sudo_exec`): Namen, Spezifikations-JSON und Parallelitäts-Zusage
//! müssen exakt den vor der `tool_provider!`-Migration erfassten Werten
//! entsprechen. Siehe [`crate::test_support::golden`].

use crate::exec::ShellToolProvider;
use crate::latex::LatexToolProvider;
use crate::sudo::{SudoToolProvider, sudo_prompt_channel};
use crate::test_support::TestResult;
use crate::test_support::golden::{assert_golden, surface};

#[test]
fn test_shell_exec_provider_surface_matches_golden() -> TestResult {
    let provider = ShellToolProvider::new();

    assert_golden("shell_exec.json", &surface(&provider, None)?)
}

#[test]
fn test_latex_provider_surface_matches_golden() -> TestResult {
    let provider = LatexToolProvider::new();

    assert_golden("latex.json", &surface(&provider, None)?)
}

#[test]
fn test_sudo_provider_surface_matches_golden() -> TestResult {
    let (sender, _receiver) = sudo_prompt_channel();
    let provider = SudoToolProvider::new(sender, "uia-shell-worker");

    assert_golden("sudo.json", &surface(&provider, None)?)
}
