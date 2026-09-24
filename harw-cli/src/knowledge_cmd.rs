//! `harw knowledge index|memory|proposals`: Wissensindex, Gedächtnis und
//! Kontextvorschläge von der Kommandozeile aus.
//!
//! `index` baut bzw. prüft den Wissensindex; `memory` und `proposals`
//! rufen dieselben Operationen auf wie die Chat-Befehle `/memory` und
//! `/context-proposal`.

use crate::cli::{GlobalArgs, KnowledgeAction};
use crate::jobs_cmd::run_and_print;
use crate::output::Printer;

/// Führt `harw knowledge …` aus.
///
/// # Errors
/// Ein deutscher Fehlertext, wenn `--json` für `index` verlangt wird (nur
/// Textausgabe), der Indexbau bzw. die Operation scheitert oder die Ausgabe
/// fehlschlägt.
pub fn run(g: &GlobalArgs, a: KnowledgeAction) -> Result<(), String> {
    match a {
        KnowledgeAction::Index { action } => {
            Printer::new(g.output()).require_text("harw knowledge index")?;
            crate::lens::run(g.home.clone(), action)
        }
        KnowledgeAction::Memory { args } => run_and_print(g, "/memory", args),
        KnowledgeAction::Proposals { args } => run_and_print(g, "/context-proposal", args),
    }
}
