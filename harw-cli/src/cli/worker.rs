//! Grammatik von `harw worker`: Container-Worker-Bau (P2 Builder-API-Contract).
//!
//! Dünne Brücke zur Builder-Logik: keine eigene Grammatik-Doppelung — jede
//! Aktion wird in eine Slash-Operation übersetzt, dieselbe wie im Chat.

use clap::{Subcommand, ValueHint};

/// Aktionen des `harw worker`-Subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum WorkerAction {
    /// Plant und startet einen Build im Builder-Worker (Podman/Bwrap nach
    /// `[builder]`-Konfiguration).
    Build {
        /// Workspace-Pfad, der gebaut wird (Vorgabe: aktuelles Verzeichnis).
        #[arg(value_hint = ValueHint::DirPath)]
        path: Option<String>,
    },
    /// Plant und startet den Testlauf im Builder-Worker.
    Test {
        /// Workspace-Pfad, dessen Tests laufen (Vorgabe: aktuelles Verzeichnis).
        #[arg(value_hint = ValueHint::DirPath)]
        path: Option<String>,
    },
    /// Führt einen einzelnen Befehl im Builder-Worker aus (hermetisch).
    Exec {
        /// Der auszuführende Befehl mit Argumenten (kein Shell-Interpret).
        #[arg(value_hint = ValueHint::Other, trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Zeigt den Zustand des Builder-Workers (letzte Läufe, aktive Versuche).
    Status,
}
