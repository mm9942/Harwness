//! Globale Flags und die Argumente der Arbeitsbefehle `chat` und `exec`.
//!
//! [`GlobalArgs`] wird in [`super::Cli`] geflattet; alle Felder sind
//! `global`, gelten also auch hinter jedem Subcommand. Die Sitzungs-Flags
//! (`--mode`, `--approval`, `--model`, `--goal`, `--add-dir`) sind zwar
//! ebenfalls global, wirken aber nur bei `chat`, `exec` und `analyze`; welche
//! davon gesetzt sind, meldet [`GlobalArgs::session_flags_used`], damit der
//! Aufrufer sie bei anderen Befehlen als Fehler ablehnen kann.
//!
//! [`ChatArgs`] (Root und `harw chat`) und [`ExecArgs`] (`harw exec`) sind
//! nicht global.
//!
//! # Argument-IDs
//! Die Sitzungs-Flags `--mode`, `--approval`, `--model` und `--goal` tragen
//! eigene IDs (`session_mode`, `session_approval`, `session_model`,
//! `session_goal`). clap überträgt Werte globaler Argumente anhand der ID aus
//! Subcommands nach oben; ohne eigene ID würde etwa ein positionales `model`
//! eines Subcommands als globales `--model` erscheinen.

use std::path::PathBuf;

use clap::{Args, ValueHint};
use harw_extension_api::ApprovalMode;

use super::values::{ApprovalParser, LogFilterParser, ModeParser};

/// Globale Flags, die hinter jedem Befehl angegeben werden dürfen.
#[derive(Debug, Clone, Args)]
pub struct GlobalArgs {
    /// Verwendet dieses Verzeichnis statt `HARW_HOME` bzw. `~/.harw` als Harwness-Verzeichnis.
    #[arg(
        long,
        global = true,
        value_name = "DIR",
        value_hint = ValueHint::DirPath,
        help_heading = "Global"
    )]
    pub home: Option<PathBuf>,
    /// Verwendet das angegebene Profil statt des aktiven Profils.
    #[arg(
        long,
        global = true,
        value_name = "NAME",
        value_hint = ValueHint::Other,
        help_heading = "Global"
    )]
    pub profile: Option<String>,
    /// Arbeitet so, als wäre harw in diesem Verzeichnis gestartet worden.
    #[arg(
        short = 'C',
        long,
        global = true,
        value_name = "DIR",
        value_hint = ValueHint::DirPath,
        help_heading = "Global"
    )]
    pub cwd: Option<PathBuf>,
    /// Legt fest, wie ausführlich protokolliert wird (z. B. `info` oder `debug`).
    #[arg(
        long,
        global = true,
        value_name = "LEVEL",
        default_value = "info",
        value_parser = LogFilterParser,
        help_heading = "Global"
    )]
    pub log: String,
    /// Protokolliert auch Prompts, Werkzeugargumente und Antworten und ist nur zur Fehlersuche gedacht.
    #[arg(long, global = true, default_value_t = false, help_heading = "Global")]
    pub log_sensitive: bool,
    /// Zeigt jeden Werkzeugaufruf mit allen Argumenten statt einer kurzen Vorschau an.
    #[arg(
        short = 'v',
        long,
        global = true,
        default_value_t = false,
        help_heading = "Global"
    )]
    pub verbose: bool,
    /// Gibt das Ergebnis als JSON statt als Text aus, sofern der Befehl das unterstützt.
    #[arg(long, global = true, default_value_t = false, help_heading = "Global")]
    pub json: bool,
    /// Startet die Sitzung im angegebenen Interaktionsmodus statt im konfigurierten Standard.
    #[arg(
        id = "session_mode",
        long = "mode",
        global = true,
        value_name = "MODUS",
        value_parser = ModeParser,
        help_heading = "Sitzung"
    )]
    pub mode: Option<String>,
    /// Legt für diese Sitzung fest, wie viel Werkzeuge ohne Rückfrage tun dürfen.
    #[arg(
        id = "session_approval",
        long = "approval",
        global = true,
        value_name = "MODUS",
        value_parser = ApprovalParser,
        help_heading = "Sitzung"
    )]
    pub approval: Option<ApprovalMode>,
    /// Verwendet für diese Sitzung das angegebene Modell statt des Standardmodells.
    #[arg(
        id = "session_model",
        long = "model",
        global = true,
        value_name = "ID",
        value_hint = ValueHint::Other,
        help_heading = "Sitzung"
    )]
    pub model: Option<String>,
    /// Setzt beim Start ein Ziel, auf das die Sitzung hinarbeitet.
    #[arg(
        id = "session_goal",
        long = "goal",
        global = true,
        value_name = "TEXT",
        value_hint = ValueHint::Other,
        help_heading = "Sitzung"
    )]
    pub goal: Option<String>,
    /// Erlaubt Dateizugriffe ohne Rückfrage zusätzlich unter diesem Verzeichnis (mehrfach angebbar).
    #[arg(
        long = "add-dir",
        global = true,
        value_name = "PFAD",
        value_hint = ValueHint::DirPath,
        help_heading = "Sitzung"
    )]
    pub add_dir: Vec<PathBuf>,
}

impl Default for GlobalArgs {
    /// Entspricht einem Aufruf ohne jedes globale Flag (`--log` also `info`).
    fn default() -> Self {
        Self {
            home: None,
            profile: None,
            cwd: None,
            log: "info".to_owned(),
            log_sensitive: false,
            verbose: false,
            json: false,
            mode: None,
            approval: None,
            model: None,
            goal: None,
            add_dir: Vec::new(),
        }
    }
}

impl GlobalArgs {
    /// Liefert die Namen der gesetzten Sitzungs-Flags.
    ///
    /// # Rückgabe
    /// Die Flag-Namen (z. B. `"--mode"`) in fester Reihenfolge
    /// `--mode`, `--approval`, `--model`, `--goal`, `--add-dir`; leer, wenn
    /// keines gesetzt ist.
    #[must_use]
    pub fn session_flags_used(&self) -> Vec<&'static str> {
        let mut used = Vec::new();
        if self.mode.is_some() {
            used.push("--mode");
        }
        if self.approval.is_some() {
            used.push("--approval");
        }
        if self.model.is_some() {
            used.push("--model");
        }
        if self.goal.is_some() {
            used.push("--goal");
        }
        if !self.add_dir.is_empty() {
            used.push("--add-dir");
        }
        used
    }

    /// Liefert das gewünschte Ausgabeformat.
    ///
    /// # Rückgabe
    /// `OutputFormat::Json` bei `--json`, sonst `OutputFormat::Text`.
    #[must_use]
    pub fn output(&self) -> crate::output::OutputFormat {
        if self.json {
            crate::output::OutputFormat::Json
        } else {
            crate::output::OutputFormat::Text
        }
    }
}

/// Argumente des interaktiven Chats (`harw [PROMPT]` und `harw chat`).
#[derive(Debug, Clone, Default, Args)]
#[command(next_help_heading = "Chat")]
pub struct ChatArgs {
    /// Startet den Chat mit dieser ersten Nachricht.
    #[arg(value_name = "PROMPT", value_hint = ValueHint::Other)]
    pub prompt: Option<String>,
    /// Setzt eine gespeicherte Sitzung fort und lässt ohne Angabe eine auswählen.
    #[arg(
        short = 'r',
        long,
        num_args = 0..=1,
        value_name = "SITZUNG",
        value_hint = ValueHint::Other
    )]
    pub resume: Option<Option<String>>,
    /// Bietet bei `--resume` die Sitzungen aller Projekte statt nur des aktuellen an.
    #[arg(long, default_value_t = false)]
    pub all: bool,
}

/// Argumente für eine einmalige Anfrage ohne interaktive Oberfläche (`harw exec`).
#[derive(Debug, Clone, Default, Args)]
pub struct ExecArgs {
    /// Die Anfrage; mehrere Wörter werden mit Leerzeichen zusammengesetzt.
    #[arg(
        value_name = "PROMPT",
        required = true,
        num_args = 1..,
        trailing_var_arg = true,
        value_hint = ValueHint::Other
    )]
    pub prompt: Vec<String>,
}
