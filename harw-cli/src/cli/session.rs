//! Grammatik von `harw session`: gespeicherte Sitzungen auflisten, anzeigen
//! und fortsetzen.

use clap::{Subcommand, ValueHint};

/// Aktionen des `harw session`-Subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum SessionAction {
    /// Listet die gespeicherten Sitzungen des aktuellen Projekts auf.
    List {
        /// Sitzungen aller Projekte statt nur des aktuellen anzeigen.
        #[arg(long)]
        all: bool,
    },
    /// Ordnet alten Sitzungen einmalig ein Projekt zu (nur Server, offline).
    BackfillProjects(super::BackfillProjectsArgs),
    /// Zeigt Details zu einer Sitzung an.
    Show {
        /// Sitzungs-ID.
        #[arg(value_hint = ValueHint::Other)]
        id: String,
    },
    /// Setzt eine Sitzung im interaktiven Chat fort.
    Resume {
        /// Sitzungs-ID.
        #[arg(value_hint = ValueHint::Other)]
        id: String,
    },
}
