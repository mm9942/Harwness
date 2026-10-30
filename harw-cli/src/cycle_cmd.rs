//! `harw cycle list|status|stop`: Intent-Cycle-Aufträge von der
//! Kommandozeile aus einsehen und beenden.
//!
//! Jede Aktion ruft dieselbe Operation auf wie der Chat-Befehl `/cycle`
//! (`/cycle list [<zustand>] [<limit>]`, `/cycle status <job>`,
//! `/cycle stop <job> [grund]`), damit Terminal und Chat identisch
//! reagieren. Das Beenden läuft hier wie im Chat auf der Operator-Stufe;
//! der modellseitige Weg bleibt das freigabepflichtige Werkzeug
//! `intent_cycle.stop`.

use crate::cli::{CycleAction, GlobalArgs};

/// Führt `harw cycle …` aus.
///
/// # Errors
/// Ein deutscher Fehlertext, wenn eine Angabe unstimmig ist (z. B. ein
/// Limit ohne Zustandsfilter), das Arbeitsverzeichnis nicht ermittelbar ist,
/// die Operation scheitert oder die Ausgabe fehlschlägt.
pub fn run(g: &GlobalArgs, a: CycleAction) -> Result<(), String> {
    let (path, tokens) = operation_for(a)?;
    crate::jobs_cmd::run_and_print(g, path, tokens)
}

/// Übersetzt eine `cycle`-Aktion in Operationspfad und Token.
///
/// Die Token entsprechen der Chat-Grammatik von `/cycle`: Das erste Token
/// ist das Verb, danach folgen die positionsgebundenen Argumente.
fn operation_for(a: CycleAction) -> Result<(&'static str, Vec<String>), String> {
    let tokens = match a {
        CycleAction::List { state, limit } => {
            let state = state.filter(|state| !state.trim().is_empty());
            let mut tokens = vec!["list".to_owned()];
            match (state, limit) {
                (Some(state), Some(limit)) => {
                    tokens.push(state);
                    tokens.push(limit.to_string());
                }
                (Some(state), None) => tokens.push(state),
                // Zustand und Limit sind in der Operation positionsgebunden;
                // `-` steht für „kein Zustandsfilter".
                (None, Some(limit)) => {
                    tokens.push("-".to_owned());
                    tokens.push(limit.to_string());
                }
                (None, None) => {}
            }
            tokens
        }
        CycleAction::Status { id } => vec!["status".to_owned(), id],
        CycleAction::Stop { id, reason } => {
            let mut tokens = vec!["stop".to_owned(), id];
            if let Some(reason) = reason.filter(|reason| !reason.trim().is_empty()) {
                tokens.push(reason);
            }
            tokens
        }
    };
    Ok(("/cycle", tokens))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_for_builds_cycle_tokens() -> Result<(), String> {
        let cases = [
            (
                CycleAction::List {
                    state: None,
                    limit: None,
                },
                vec!["list"],
            ),
            (
                CycleAction::List {
                    state: Some("running".to_owned()),
                    limit: None,
                },
                vec!["list", "running"],
            ),
            (
                CycleAction::List {
                    state: Some("failed".to_owned()),
                    limit: Some(20),
                },
                vec!["list", "failed", "20"],
            ),
            (
                CycleAction::Status {
                    id: "job-1".to_owned(),
                },
                vec!["status", "job-1"],
            ),
            (
                CycleAction::Stop {
                    id: "job-2".to_owned(),
                    reason: None,
                },
                vec!["stop", "job-2"],
            ),
            (
                CycleAction::Stop {
                    id: "job-2".to_owned(),
                    reason: Some("zu teuer".to_owned()),
                },
                vec!["stop", "job-2", "zu teuer"],
            ),
            (
                CycleAction::Stop {
                    id: "job-3".to_owned(),
                    reason: Some("   ".to_owned()),
                },
                vec!["stop", "job-3"],
            ),
        ];
        for (action, expected) in cases {
            let (path, tokens) = operation_for(action)?;
            assert_eq!(path, "/cycle");
            let expected: Vec<String> = expected.into_iter().map(str::to_owned).collect();
            assert_eq!(tokens, expected);
        }
        Ok(())
    }

    #[test]
    fn limit_without_state_uses_the_placeholder() -> Result<(), String> {
        let (path, tokens) = operation_for(CycleAction::List {
            state: None,
            limit: Some(5),
        })?;
        assert_eq!(path, "/cycle");
        assert_eq!(tokens, vec!["list", "-", "5"]);
        Ok(())
    }
}
