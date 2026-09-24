//! `harw jobs list|show|approve|deny|cancel|retry`: Hintergrund-Jobs von
//! der Kommandozeile aus verwalten.
//!
//! Jede Aktion ruft dieselbe Operation auf wie der entsprechende
//! Chat-Befehl (`/ps`, `/review`, `/approve`, `/deny`, `/cancel`,
//! `/retry`), damit Terminal und Chat identisch reagieren.

use std::path::PathBuf;

use crate::cli::{GlobalArgs, JobsAction};
use crate::op_bridge::{OpTarget, run_operation};
use crate::output::Printer;

/// Führt `harw jobs …` aus.
///
/// # Errors
/// Ein deutscher Fehlertext, wenn das Arbeitsverzeichnis nicht ermittelbar
/// ist, Pflichtangaben fehlen (z. B. die Begründung bei `deny`), die
/// Operation scheitert oder die Ausgabe fehlschlägt.
pub fn run(g: &GlobalArgs, a: JobsAction) -> Result<(), String> {
    let (path, args) = operation_for(a)?;
    run_and_print(g, path, args)
}

/// Übersetzt eine `jobs`-Aktion in Operationspfad und Argumente.
fn operation_for(a: JobsAction) -> Result<(&'static str, Vec<String>), String> {
    let result = match a {
        JobsAction::List { filter } => ("/ps", filter.into_iter().collect()),
        JobsAction::Show { id } => ("/review", vec![id]),
        JobsAction::Approve { id, note } => ("/approve", with_optional_text(id, note)),
        JobsAction::Deny { id, reason } => {
            let reason = reason
                .filter(|reason| !reason.trim().is_empty())
                .ok_or_else(|| {
                    "Zum Ablehnen ist eine Begründung nötig: harw jobs deny <ID> --reason <TEXT>"
                        .to_owned()
                })?;
            ("/deny", vec![id, reason])
        }
        JobsAction::Cancel { id } => ("/cancel", vec![id]),
        JobsAction::Retry { id } => ("/retry", vec![id]),
    };
    Ok(result)
}

fn with_optional_text(id: String, text: Option<String>) -> Vec<String> {
    let mut args = vec![id];
    if let Some(text) = text.filter(|text| !text.trim().is_empty()) {
        args.push(text);
    }
    args
}

/// Arbeitsverzeichnis für Operationen: `-C/--cwd`, sonst das aktuelle
/// Prozessverzeichnis. Wird auch von den übrigen Befehlsmodulen genutzt.
pub(crate) fn working_dir(g: &GlobalArgs) -> Result<PathBuf, String> {
    match &g.cwd {
        Some(dir) => Ok(dir.clone()),
        None => std::env::current_dir()
            .map_err(|error| format!("Aktuelles Arbeitsverzeichnis nicht ermittelbar: {error}")),
    }
}

/// Führt eine Operation mit den globalen Optionen aus und gibt ihr Ergebnis
/// über den [`Printer`] aus. Gemeinsamer Weg für `jobs`, `knowledge` und
/// `agent`.
pub(crate) fn run_and_print(g: &GlobalArgs, path: &str, args: Vec<String>) -> Result<(), String> {
    let cwd = working_dir(g)?;
    let output = run_operation(
        OpTarget {
            home: g.home.clone(),
            cwd: &cwd,
        },
        path,
        args,
    )?;
    Printer::new(g.output()).op_output(&output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_without_reason_is_rejected() {
        let result = operation_for(JobsAction::Deny {
            id: "w1".to_owned(),
            reason: None,
        });
        assert!(result.is_err());
    }

    #[test]
    fn approve_passes_note_as_tail() -> Result<(), String> {
        let (path, args) = operation_for(JobsAction::Approve {
            id: "w1".to_owned(),
            note: Some("passt so".to_owned()),
        })?;
        assert_eq!(path, "/approve");
        assert_eq!(args, vec!["w1".to_owned(), "passt so".to_owned()]);
        Ok(())
    }

    #[test]
    fn list_without_filter_has_no_args() -> Result<(), String> {
        let (path, args) = operation_for(JobsAction::List { filter: None })?;
        assert_eq!(path, "/ps");
        assert!(args.is_empty());
        Ok(())
    }
}
