//! Grammatik von `harw service`: Hintergrunddienst verwalten.

use clap::Subcommand;

/// Aktionen des `harw service`-Subcommands.
#[derive(Debug, Subcommand)]
pub enum ServiceAction {
    /// Dienst-Unit rendern und installieren.
    Install {
        /// Nur die gerenderte Unit ausgeben, nichts installieren.
        #[arg(long)]
        dry_run: bool,
    },
    /// Dienststatus abfragen.
    Status,
    /// Dienst-Unit entfernen.
    Uninstall,
}
