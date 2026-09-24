//! Grammatik von `harw debug`: Diagnosewerkzeuge für Entwickler.

use clap::{Subcommand, ValueHint};

/// Aktionen des `harw debug`-Subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum DebugAction {
    /// Führt einen einzelnen Durchlauf gegen einen Test-Anbieter aus, der die Eingabe zurückgibt.
    Echo {
        /// Die Eingabezeile.
        #[arg(required = true, num_args = 1.., value_hint = ValueHint::Other)]
        input: Vec<String>,
    },
    /// Zeigt, wie eine Eingabezeile eingeordnet wird.
    Classify {
        /// Die einzuordnende Eingabe.
        #[arg(required = true, num_args = 1.., value_hint = ValueHint::Other)]
        input: Vec<String>,
    },
}
