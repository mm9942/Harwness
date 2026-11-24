//! Grammatik von `harw knowledge`: Wissensindex, Gedächtnis, Tagebuch und Vorschläge.

use clap::{Subcommand, ValueHint};

use super::lens::LensAction;

/// Aktionen des `harw knowledge`-Subcommands.
///
/// Leitet kein `Clone` ab, weil [`LensAction`] es nicht ableitet.
#[derive(Debug, Subcommand)]
pub enum KnowledgeAction {
    /// Baut den Wissensindex oder zeigt seinen Stand; ohne Unterbefehl wird
    /// der Stand gezeigt.
    Index {
        /// Auszuführende Index-Aktion.
        #[command(subcommand)]
        action: Option<LensAction>,
    },
    /// Verwaltet das Langzeitgedächtnis des aktiven Profils.
    Memory {
        /// Argumente für die Gedächtnis-Verwaltung (z. B. `list`).
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            num_args = 0..,
            value_name = "ARGS",
            value_hint = ValueHint::Other
        )]
        args: Vec<String>,
    },
    /// Liest und ergänzt das lokale Tagebuch des aktiven Profils.
    Diary {
        /// Auszuführende Tagebuch-Aktion.
        #[command(subcommand)]
        action: DiaryAction,
    },
    /// Zeigt und bearbeitet offene Kontext-Vorschläge.
    Proposals {
        /// Argumente für die Vorschlags-Verwaltung (z. B. `list`).
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            num_args = 0..,
            value_name = "ARGS",
            value_hint = ValueHint::Other
        )]
        args: Vec<String>,
    },
}

/// Typisierte lokale CLI-Oberfläche derselben Operation wie `/diary`.
#[derive(Debug, Subcommand)]
pub enum DiaryAction {
    /// Zeigt die eigenen Einträge des aktuellen UTC-Tags.
    Today,
    /// Zeigt sichtbare Einträge an einem UTC-Tag oder in einem Datumsbereich.
    Show {
        /// Agent-ID; ohne Angabe das eigene Tagebuch.
        #[arg(long, value_name = "ID")]
        agent: Option<String>,
        /// Ein einzelner UTC-Tag.
        #[arg(long, value_name = "YYYY-MM-DD", conflicts_with_all = ["from", "to"])]
        date: Option<String>,
        /// Erster Tag des Bereichs (inklusive).
        #[arg(long, value_name = "YYYY-MM-DD")]
        from: Option<String>,
        /// Letzter Tag des Bereichs (inklusive).
        #[arg(long, value_name = "YYYY-MM-DD")]
        to: Option<String>,
    },
    /// Sucht sichtbare Einträge per Volltext. Option-artigen Text nach `--` angeben.
    Search {
        /// Gesuchter Text, als ein Argument.
        #[arg(value_name = "TEXT")]
        text: String,
        /// Beschränkt die Suche auf eine genaue Agent-ID.
        #[arg(long, value_name = "ID")]
        agent: Option<String>,
        /// Erster Tag des Bereichs (inklusive).
        #[arg(long, value_name = "YYYY-MM-DD")]
        from: Option<String>,
        /// Letzter Tag des Bereichs (inklusive).
        #[arg(long, value_name = "YYYY-MM-DD")]
        to: Option<String>,
    },
    /// Listet Agenten mit sichtbaren Tagebüchern.
    Agents,
    /// Liest eine UTF-8-Notiz von stdin und ergänzt nur das eigene Tagebuch.
    Note,
}
