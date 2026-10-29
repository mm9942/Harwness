//! Grammatik von `harw tool` (PL-97 B2, PL-88): der Werkzeug-Index von der
//! Kommandozeile aus — dieselben Daten wie das Modell-Werkzeug `tool.index`.

use clap::{Subcommand, ValueHint};

/// Aktionen des `harw tool`-Subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum ToolAction {
    /// Listet die Werkzeugfamilien mit ihrer Werkzeuganzahl; mit FAMILY je
    /// Werkzeug der Familie eine Zeile mit Fähigkeitsklasse, erstem Satz der
    /// Beschreibung, verknüpftem Skill und den Profilen, die es registrieren.
    List {
        /// Werkzeugfamilie: der Namenspräfix vor dem ersten `.` (z. B. `fs`,
        /// `git`, `dod`); Namen ohne `.` gehören zu `core`.
        #[arg(value_name = "FAMILY", value_hint = ValueHint::Other)]
        family: Option<String>,
    },
    /// Zeigt ein Werkzeug: Familie, Provider, Fähigkeitsklasse, Profile,
    /// verknüpften Skill, Freigabeverhalten, die vollständige Beschreibung und
    /// die Parameter.
    Describe {
        /// Vollständiger Werkzeugname, z. B. `fs.read` oder `dod.findings`.
        #[arg(value_name = "NAME", value_hint = ValueHint::Other)]
        name: String,
    },
}
