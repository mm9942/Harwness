//! Grammatik von `harw project`: Projekt-Freigabe (Trust).

use std::path::PathBuf;

use clap::{Subcommand, ValueHint};

/// Aktionen des `harw project`-Subcommands.
///
/// Ohne `path` wirkt jede Aktion auf das aktuelle Arbeitsverzeichnis
/// (siehe `crate::project_trust`).
#[derive(Debug, Subcommand)]
pub enum ProjectAction {
    /// Gibt das Projekt frei (bzw. erneuert die Freigabe).
    Trust {
        /// Projekt-Root; ohne Angabe das aktuelle Arbeitsverzeichnis.
        #[arg(value_name = "DIR", value_hint = ValueHint::DirPath)]
        path: Option<PathBuf>,
    },
    /// Entzieht die Freigabe für das Projekt.
    Untrust {
        /// Projekt-Root; ohne Angabe das aktuelle Arbeitsverzeichnis.
        #[arg(value_name = "DIR", value_hint = ValueHint::DirPath)]
        path: Option<PathBuf>,
    },
    /// Zeigt den Vertrauensstatus des Projekts.
    Status {
        /// Projekt-Root; ohne Angabe das aktuelle Arbeitsverzeichnis.
        #[arg(value_name = "DIR", value_hint = ValueHint::DirPath)]
        path: Option<PathBuf>,
    },
}
