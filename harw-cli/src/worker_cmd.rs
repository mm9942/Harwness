//! `harw worker build|test|exec|status`: Container-Worker von der
//! Kommandozeile aus steuern (P2 Builder-API-Contract).
//!
//! Dünne Brücke: jede Aktion ruft dieselbe Slash-Operation auf wie der
//! Chat-Befehl (`/worker`), damit Terminal und Chat identisch reagieren.

use crate::cli::{GlobalArgs, WorkerAction};
use crate::jobs_cmd::run_and_print;

/// Führt `harw worker …` aus.
///
/// # Errors
/// Ein deutscher Fehlertext, wenn das Arbeitsverzeichnis nicht ermittelbar
/// ist oder die Operation scheitert.
pub fn run(g: &GlobalArgs, a: WorkerAction) -> Result<(), String> {
    let (path, args) = operation_for(a);
    run_and_print(g, path, args)
}

/// Übersetzt eine `worker`-Aktion in Operationspfad und Argumente.
fn operation_for(a: WorkerAction) -> (&'static str, Vec<String>) {
    match a {
        WorkerAction::Build { path } => (
            "/worker",
            std::iter::once("build".to_owned())
                .chain(path)
                .collect(),
        ),
        WorkerAction::Test { path } => (
            "/worker",
            std::iter::once("test".to_owned())
                .chain(path)
                .collect(),
        ),
        WorkerAction::Exec { command } => {
            let mut args = vec!["exec".to_owned()];
            args.extend(command);
            ("/worker", args)
        }
        WorkerAction::Status => ("/worker", vec!["status".to_owned()]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_without_path_has_only_verb() {
        let (path, args) = operation_for(WorkerAction::Build { path: None });
        assert_eq!(path, "/worker");
        assert_eq!(args, vec!["build".to_owned()]);
    }

    #[test]
    fn build_with_path_appends_it() {
        let (path, args) = operation_for(WorkerAction::Build {
            path: Some("/srv/w".to_owned()),
        });
        assert_eq!(path, "/worker");
        assert_eq!(args, vec!["build".to_owned(), "/srv/w".to_owned()]);
    }

    #[test]
    fn exec_passes_command_verbatim() {
        let (path, args) = operation_for(WorkerAction::Exec {
            command: vec![
                "cargo".to_owned(),
                "build".to_owned(),
                "--release".to_owned(),
            ],
        });
        assert_eq!(path, "/worker");
        assert_eq!(
            args,
            vec![
                "exec".to_owned(),
                "cargo".to_owned(),
                "build".to_owned(),
                "--release".to_owned()
            ]
        );
    }

    #[test]
    fn status_has_only_verb() {
        let (path, args) = operation_for(WorkerAction::Status);
        assert_eq!(path, "/worker");
        assert_eq!(args, vec!["status".to_owned()]);
    }
}
