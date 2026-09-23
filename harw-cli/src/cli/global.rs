//! Globale Chat-Flags des Root-Parsers (`harw [PROMPT]`).
//!
//! [`ChatArgs`] wird in [`super::Cli`] geflattet; die meisten Flags sind
//! `global`, gelten also auch hinter jedem Subcommand.

use std::path::PathBuf;

use clap::{Args, ValueHint};

use super::values::LogFilterParser;

/// Flags, die für den Default-Chat-Pfad gelten.
#[derive(Debug, Args)]
#[command(next_help_heading = "Chat")]
pub struct ChatArgs {
    /// Optionaler erster Prompt. Ohne Prompt startet der interaktive Chat.
    #[arg(value_hint = ValueHint::Other)]
    pub prompt: Option<String>,
    /// Bestehende Sitzung fortsetzen; ohne Wert wird interaktiv ausgewählt.
    #[arg(
        short = 'r',
        long,
        num_args = 0..=1,
        value_name = "SESSION",
        value_hint = ValueHint::Other
    )]
    pub resume: Option<Option<String>>,
    /// Root-Space überschreiben (Vorrang vor `HARW_HOME`/`$HOME/.harw`).
    #[arg(long, global = true, value_name = "DIR", value_hint = ValueHint::DirPath)]
    pub home: Option<PathBuf>,
    /// Log-Level für das Tracing-Framework.
    ///
    /// Gültige Werte: `trace`, `debug`, `info`, `warn`, `error`.
    /// Entspricht der `RUST_LOG`-Syntax von `tracing-subscriber`.
    #[arg(
        long,
        global = true,
        value_name = "LEVEL",
        default_value = "info",
        value_parser = LogFilterParser,
        help_heading = "Logging"
    )]
    pub log: String,
    /// Aktiviert das Loggen sensibler Daten (Prompts, Tool-Args, Responses).
    ///
    /// **Achtung – Redaction-by-Default**: Ohne dieses Flag werden Prompts,
    /// Tool-Argumente und Modell-Responses **nicht** geloggt. Das Flag öffnet
    /// den Debug-Kanal; es darf **nicht** in Produktionsumgebungen gesetzt
    /// werden.
    #[arg(long, global = true, default_value_t = false, help_heading = "Logging")]
    pub log_sensitive: bool,
    /// Zeigt jeden Tool-Aufruf samt Argumenten in der TUI/Konsole an, statt
    /// nur die verdichtete `ToolCell`-Vorschau.
    #[arg(long, global = true, default_value_t = false)]
    pub verbose: bool,
    /// Zusätzliche Arbeitswurzel, unter der Datei-Werkzeuge ohne erneute
    /// Rückfrage lesen/schreiben dürfen (mehrfach angebbar). Nur für diesen
    /// Prozess gültig — dauerhaftes Merken läuft über `/add-workdir merken`
    /// bzw. `harw settings`.
    #[arg(
        long = "add-dir",
        global = true,
        value_name = "PFAD",
        value_hint = ValueHint::DirPath
    )]
    pub add_dir: Vec<PathBuf>,
    /// Zeigt bei `-r` die Sessions **aller** Projekte statt nur die des
    /// aktuellen Projekts (Projekt = nächster Ordner mit `.git`, sonst das
    /// Arbeitsverzeichnis selbst).
    #[arg(long, global = true, default_value_t = false)]
    pub all: bool,
}
