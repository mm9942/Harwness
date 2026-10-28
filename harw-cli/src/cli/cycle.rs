//! Grammatik von `harw cycle`: Intent-Cycle-Aufträge einsehen und beenden.

use clap::{Subcommand, ValueHint};

/// Aktionen des `harw cycle`-Subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum CycleAction {
    /// Listet Intent-Cycle-Aufträge des Workspaces mit Zustand, Objective,
    /// Sequenz und offenen Beobachtungen.
    List {
        /// Optionaler Zustandsfilter, z. B. `running` oder `failed`.
        #[arg(value_hint = ValueHint::Other)]
        state: Option<String>,
        /// Höchstzahl der Zeilen (Standard 50, Obergrenze 200). Ohne
        /// Zustandsfilter wird `-` („alle Zustände") übergeben.
        #[arg(long, value_hint = ValueHint::Other)]
        limit: Option<u32>,
    },
    /// Zeigt den Status eines Intent-Cycle-Auftrags.
    Status {
        /// Auftrags-ID.
        #[arg(value_hint = ValueHint::Other)]
        id: String,
    },
    /// Beendet einen laufenden Intent-Cycle-Auftrag.
    Stop {
        /// Auftrags-ID.
        #[arg(value_hint = ValueHint::Other)]
        id: String,
        /// Optionale Begründung des Abbruchs.
        #[arg(long, value_hint = ValueHint::Other)]
        reason: Option<String>,
    },
}
