//! `harw tool list|describe` (PL-97 B2, PL-88): der Werkzeug-Index von der
//! Kommandozeile aus.
//!
//! Beide Aktionen rendern über dieselben Ansichten wie das Modell-Werkzeug
//! `tool.index` (`harw_ops::tool_index_op::{index_view, describe_view}`):
//! Katalog, Profile, die ersten Sätze der echten Werkzeugbeschreibungen und
//! die Werkzeug→Skill-Verknüpfung. Die Operations-Werkzeuge beschreibt die
//! Grundausstattung samt der gegateten Flächen
//! (`harw_ops::tool_index_op::cli_operations`). Es braucht weder
//! Konfiguration noch Laufzeit noch ein Harwness-Home; nichts wird
//! geschrieben.
//!
//! Exit-Codes: 0 bei Erfolg, 1, wenn die Familie bzw. das Werkzeug unbekannt
//! ist, 2 bei einer falschen Bedienung (clap) oder nicht schreibbarer
//! Ausgabe.

use harw_operations::registry::OperationRegistry;
use harw_ops::tool_index_op::{IndexView, build_index, cli_operations, describe_view, index_view};

use crate::cli::{GlobalArgs, ToolAction};
use crate::output::Printer;

/// Exit-Code für eine unbekannte Familie bzw. ein unbekanntes Werkzeug.
const NOT_FOUND_EXIT: i32 = 1;

/// Führt `harw tool …` aus.
///
/// # Errors
/// Ein deutscher Fehlertext, wenn die Ausgabe nicht geschrieben werden kann
/// (Exit-Code 2). Eine unbekannte Familie bzw. ein unbekanntes Werkzeug
/// beendet den Prozess nach der Meldung mit Exit-Code 1.
pub fn run(g: &GlobalArgs, a: ToolAction) -> Result<(), String> {
    let printer = Printer::new(g.output());
    let operations = cli_operations();
    match view(&operations, &a) {
        Ok(view) => printer.value(&view.text, view.json),
        Err(error) => {
            eprintln!("harw: {error}");
            std::process::exit(NOT_FOUND_EXIT);
        }
    }
}

/// Die Ansicht einer Aktion.
///
/// # Errors
/// Der Fehlertext für eine unbekannte Familie (mit den bekannten Familien)
/// bzw. ein unbekanntes Werkzeug.
fn view(operations: &OperationRegistry, a: &ToolAction) -> Result<IndexView, String> {
    match a {
        ToolAction::List { family } => {
            index_view(&build_index(Some(operations)), family.as_deref())
        }
        ToolAction::Describe { name } => describe_view(name, Some(operations)).ok_or_else(|| {
            format!("unbekanntes Werkzeug '{name}'; `harw tool list` zeigt die Familien")
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clap::Parser as _;

    use super::*;
    use crate::cli::{Cli, Command};
    use crate::test_support::{TestError, TestResult, ctx};

    fn action(argv: &[&str]) -> TestResult<ToolAction> {
        let cli = Cli::try_parse_from(std::iter::once("harw").chain(argv.iter().copied()))
            .map_err(ctx("harw tool parses"))?;
        match cli.command {
            Some(Command::Tool { action }) => Ok(action),
            other => Err(TestError::Unexpected(format!(
                "expected Command::Tool, got {other:?}"
            ))),
        }
    }

    #[test]
    fn tool_subcommands_parse_with_and_without_family() -> TestResult {
        assert!(matches!(
            action(&["tool", "list"])?,
            ToolAction::List { family: None }
        ));
        assert!(matches!(
            action(&["tool", "list", "fs"])?,
            ToolAction::List { family: Some(family) } if family == "fs"
        ));
        assert!(matches!(
            action(&["tool", "describe", "fs.read"])?,
            ToolAction::Describe { name } if name == "fs.read"
        ));
        assert!(Cli::try_parse_from(["harw", "tool", "describe"]).is_err());
        assert!(Cli::try_parse_from(["harw", "tool", "call", "fs.read"]).is_err());
        Ok(())
    }

    /// Jede `harw tool`-Aktion nimmt `--json` an (vor und hinter dem
    /// Befehl), startet keine TUI und läuft ohne Config-Migration.
    #[test]
    fn tool_subcommands_accept_json_and_skip_startup_migrations() -> TestResult {
        for argv in [
            &["tool", "list"][..],
            &["tool", "list", "dod"],
            &["tool", "describe", "fs.read"],
        ] {
            for json_first in [false, true] {
                let mut full = vec!["harw"];
                if json_first {
                    full.push("--json");
                }
                full.extend_from_slice(argv);
                if !json_first {
                    full.push("--json");
                }
                let cli =
                    Cli::try_parse_from(full.iter().copied()).map_err(ctx("tool --json parses"))?;
                assert!(cli.global.json, "{full:?}");
                crate::reject_unsupported_json(cli.command.as_ref(), &cli.global)
                    .map_err(ctx("tool accepts --json"))?;
                assert!(!crate::starts_tui(&cli), "{full:?}");
                assert_eq!(crate::command_label(cli.command.as_ref()), "harw tool");
                crate::run_startup_migrations(
                    &cli.command,
                    Some(PathBuf::from("/nonexistent/harw")),
                )
                .map_err(ctx("tool skips migrations"))?;
            }
        }
        Ok(())
    }

    #[test]
    fn tool_list_shows_families_and_the_tools_of_one_family() -> TestResult {
        let operations = cli_operations();
        let overview =
            view(&operations, &ToolAction::List { family: None }).map_err(TestError::Unexpected)?;
        assert!(overview.text.contains("\n- fs ("), "{}", overview.text);
        assert!(overview.text.contains("\n- dod (3)"), "{}", overview.text);
        assert!(overview.json["families"].is_array());

        let dod = view(
            &operations,
            &ToolAction::List {
                family: Some("dod".to_owned()),
            },
        )
        .map_err(TestError::Unexpected)?;
        assert_eq!(dod.text.lines().count(), 3, "{}", dod.text);
        assert!(dod.text.starts_with("dod.findings [meta] "), "{}", dod.text);
        assert_eq!(dod.json["family"], "dod");

        let unknown = view(
            &operations,
            &ToolAction::List {
                family: Some("nope".to_owned()),
            },
        );
        assert!(
            unknown
                .as_ref()
                .is_err_and(|error| error.contains("known families")),
            "{unknown:?}"
        );
        Ok(())
    }

    #[test]
    fn tool_describe_shows_the_full_spec_or_reports_an_unknown_tool() -> TestResult {
        let operations = cli_operations();
        let read = view(
            &operations,
            &ToolAction::Describe {
                name: "fs.read".to_owned(),
            },
        )
        .map_err(TestError::Unexpected)?;
        assert!(read.text.starts_with("fs.read\n"), "{}", read.text);
        assert!(read.text.contains("Parameters:"), "{}", read.text);
        assert_eq!(read.json["name"], "fs.read");
        assert!(read.json["schema"].is_object());

        let sys = view(
            &operations,
            &ToolAction::Describe {
                name: "sys.ps".to_owned(),
            },
        )
        .map_err(TestError::Unexpected)?;
        assert!(sys.text.contains("skill:     sys-tools"), "{}", sys.text);

        let unknown = view(
            &operations,
            &ToolAction::Describe {
                name: "no.such_tool".to_owned(),
            },
        );
        assert!(
            unknown
                .as_ref()
                .is_err_and(|error| error.contains("no.such_tool")),
            "{unknown:?}"
        );
        Ok(())
    }
}
