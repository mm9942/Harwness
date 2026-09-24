//! Grammatik von `harw sandbox`: Host-Freigaben prüfen und verwalten.

use clap::{Subcommand, ValueHint};

/// Aktionen des `harw sandbox`-Subcommands.
#[derive(Debug, Subcommand)]
pub enum SandboxAction {
    /// Zeigt, welche Werkzeuge direkt auf dem Host laufen dürfen und ob die
    /// Freigabe-Prüfung aktiv ist.
    Status,
    /// Listet aktive Host-Freigaben dieses Prozesses.
    #[command(hide = true)]
    Leases,
    /// Widerruft alle Host-Freigaben einer Sitzung in diesem Prozess.
    #[command(hide = true)]
    Revoke {
        /// Sitzungs-ID, deren Zustimmung und Permits widerrufen werden.
        #[arg(long, value_hint = ValueHint::Other)]
        session: String,
    },
}
