//! Grammatik von `harw agent`: Agentin, Skills und Plugins verwalten.

use clap::{Subcommand, ValueHint};

/// Aktionen des `harw agent`-Subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum AgentAction {
    /// Richtet interaktiv eine neue Benutzeroberflächen-Agentin ein.
    UiaNew,
    /// Listet die startbaren Agenten (eingebaut und benutzerdefiniert) mit
    /// Rolle, Herkunft und Beschreibung; optional gefiltert.
    List {
        /// Optionaler Suchbegriff (Name oder Beschreibung).
        #[arg(value_name = "SUCHE", value_hint = ValueHint::Other)]
        query: Option<String>,
    },
    /// Verwaltet die Skills des aktiven Profils.
    Skills {
        /// Argumente für die Skill-Verwaltung (z. B. `list`).
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            num_args = 0..,
            value_name = "ARGS",
            value_hint = ValueHint::Other
        )]
        args: Vec<String>,
    },
    /// Verwaltet die Plugins des aktiven Profils.
    Plugins {
        /// Argumente für die Plugin-Verwaltung (z. B. `list`).
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
