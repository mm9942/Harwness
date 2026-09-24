//! Grammatik von `harw knowledge`: Wissensindex, Gedächtnis und Vorschläge.

use clap::{Subcommand, ValueHint};

use super::lens::LensAction;

/// Aktionen des `harw knowledge`-Subcommands.
///
/// Leitet kein `Clone` ab, weil [`LensAction`] es nicht ableitet.
#[derive(Debug, Subcommand)]
pub enum KnowledgeAction {
    /// Baut den Wissensindex oder zeigt seinen Stand; ohne Unterbefehl wird
    /// der Stand gezeigt.
    Index {
        /// Auszuführende Index-Aktion.
        #[command(subcommand)]
        action: Option<LensAction>,
    },
    /// Verwaltet das Langzeitgedächtnis des aktiven Profils.
    Memory {
        /// Argumente für die Gedächtnis-Verwaltung (z. B. `list`).
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            num_args = 0..,
            value_name = "ARGS",
            value_hint = ValueHint::Other
        )]
        args: Vec<String>,
    },
    /// Zeigt und bearbeitet offene Kontext-Vorschläge.
    Proposals {
        /// Argumente für die Vorschlags-Verwaltung (z. B. `list`).
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            num_args = 0..,
            value_name = "ARGS",
            value_hint = ValueHint::Other
        )]
        args: Vec<String>,
    },
}
