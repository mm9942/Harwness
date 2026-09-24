//! Grammatik von `harw jobs`: Hintergrundaufträge einsehen und steuern.

use clap::{Subcommand, ValueHint};

/// Aktionen des `harw jobs`-Subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum JobsAction {
    /// Listet Aufträge auf, optional nach Status gefiltert.
    List {
        /// Optionaler Filter, z. B. ein Status wie `offen`.
        #[arg(value_hint = ValueHint::Other)]
        filter: Option<String>,
    },
    /// Zeigt Details zu einem Auftrag.
    Show {
        /// Auftrags-ID.
        #[arg(value_hint = ValueHint::Other)]
        id: String,
    },
    /// Gibt einen wartenden Auftrag frei.
    Approve {
        /// Auftrags-ID.
        #[arg(value_hint = ValueHint::Other)]
        id: String,
        /// Optionale Notiz zur Freigabe.
        #[arg(long, value_hint = ValueHint::Other)]
        note: Option<String>,
    },
    /// Lehnt einen wartenden Auftrag ab.
    Deny {
        /// Auftrags-ID.
        #[arg(value_hint = ValueHint::Other)]
        id: String,
        /// Optionale Begründung der Ablehnung.
        #[arg(long, value_hint = ValueHint::Other)]
        reason: Option<String>,
    },
    /// Bricht einen Auftrag ab.
    Cancel {
        /// Auftrags-ID.
        #[arg(value_hint = ValueHint::Other)]
        id: String,
    },
    /// Startet einen fehlgeschlagenen Auftrag erneut.
    Retry {
        /// Auftrags-ID.
        #[arg(value_hint = ValueHint::Other)]
        id: String,
    },
}
