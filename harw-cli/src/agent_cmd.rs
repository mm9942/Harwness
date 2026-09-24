//! `harw agent uia-new|skills|plugins`: Agenten, Skills und Plugins
//! verwalten.
//!
//! `uia-new` startet den Einrichtungsdialog für einen weiteren
//! Benutzeroberflächen-Agenten; `skills` und `plugins` rufen dieselben
//! Operationen auf wie die Chat-Befehle `/skills` und `/plugins`.

use crate::cli::{AgentAction, GlobalArgs};
use crate::jobs_cmd::run_and_print;
use crate::output::Printer;

/// Führt `harw agent …` aus.
///
/// # Errors
/// Ein deutscher Fehlertext, wenn `--json` für den interaktiven
/// `uia-new`-Dialog verlangt wird, der Dialog bzw. die Operation scheitert
/// oder die Ausgabe fehlschlägt.
pub fn run(g: &GlobalArgs, a: AgentAction) -> Result<(), String> {
    match a {
        AgentAction::UiaNew => {
            Printer::new(g.output()).require_text("harw agent uia-new")?;
            crate::uia_bootstrap::run_new_uia_command(g.home.clone())
        }
        AgentAction::Skills { args } => run_and_print(g, "/skills", args),
        AgentAction::Plugins { args } => run_and_print(g, "/plugins", args),
    }
}
