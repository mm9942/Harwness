//! Grammatik von `harw channel`: Nachrichtenkanäle anbinden.

use clap::{Subcommand, ValueHint};

use super::values::Channel;

/// Aktionen des `harw channel`-Subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum ChannelAction {
    /// Richtet einen Kanal ein oder schließt eine ausstehende Kopplung ab.
    Connect {
        /// Kanal-Art (derzeit nur `telegram`).
        #[arg(value_enum)]
        channel: Channel,
        /// Einmaligen Code nach einer `/pair CODE`-Nachricht an den Bot einlösen.
        #[arg(long, value_name = "CODE", value_hint = ValueHint::Other)]
        pair: Option<String>,
    },
}
