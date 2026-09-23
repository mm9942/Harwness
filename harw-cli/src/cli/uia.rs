//! Grammatik von `harw uia`: Benutzeroberflächen-Agentin verwalten.

use clap::Subcommand;

/// Aktionen des `harw uia`-Subcommands.
#[derive(Debug, Subcommand)]
pub enum UiaAction {
    /// Richtet interaktiv eine neue UIA ein (Name, Persönlichkeit,
    /// Nutzerkontext), zeigt eine Vorschau, fragt Bestätigung ab und
    /// aktiviert die neue UIA anschließend als
    /// `active_uia_definition` im aktiven Profil.
    New,
}
