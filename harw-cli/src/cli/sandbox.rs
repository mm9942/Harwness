//! Grammatik von `harw sandbox`: Host-Profil-Permit-Ledger.

use clap::{Subcommand, ValueHint};

/// Aktionen des `harw sandbox`-Subcommands.
#[derive(Debug, Subcommand)]
pub enum SandboxAction {
    /// Zeigt, welche Worker-Definitionen Host-Profil-Ausführung deklarieren
    /// und ob die Permit-Ledger-Kette dafür in diesem Prozess verdrahtet ist.
    Status,
    /// Listet aktive Host-Permit-Leases **dieses Prozesses** (siehe Hinweis
    /// bei [`super::Command::Sandbox`] zur fehlenden Persistenz über Prozesse hinweg).
    Leases,
    /// Widerruft alle Leases und gemerkten Permits einer Sitzung **in
    /// diesem Prozess**.
    Revoke {
        /// Sitzungs-ID, deren Zustimmung und Permits widerrufen werden.
        #[arg(long, value_hint = ValueHint::Other)]
        session: String,
    },
}
