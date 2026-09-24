//! Grammatik von `harw analyze`: Workspace-Abhängigkeitsanalyse.

use super::values::AnalyzeOrder;

/// Argumente des `harw analyze`-Subcommands.
#[derive(Debug, Clone, clap::Args)]
pub struct AnalyzeArgs {
    /// Zu analysierendes Crate; ohne Angabe der gesamte Workspace.
    pub crate_name: Option<String>,
    /// Erzwingt die Analyse des gesamten Workspace, auch wenn ein Crate genannt ist.
    #[arg(long)]
    pub workspace: bool,
    /// Reihenfolge der Analyse: von den Blatt-Crates aufwärts (`bottom-up`,
    /// Vorgabe) oder von den Wurzeln abwärts (`top-down`).
    #[arg(long, value_enum, value_name = "RICHTUNG", default_value_t = AnalyzeOrder::BottomUp)]
    pub order: AnalyzeOrder,
    /// Veraltete Schreibweise von `--order bottom-up`.
    ///
    /// Schließt sich mit `--top-down` und `--order` aus: ein gemeinsamer
    /// Aufruf bricht das Parsing mit einem Fehler ab, ohne stillen Vorrang.
    #[arg(long, hide = true, conflicts_with_all = ["top_down", "order"])]
    pub bottom_up: bool,
    /// Veraltete Schreibweise von `--order top-down`.
    #[arg(long, hide = true, conflicts_with = "order")]
    pub top_down: bool,
    /// Zeigt nur, was analysiert würde, und startet keine Hilfsagenten.
    #[arg(long)]
    pub dry_run: bool,
    /// Obergrenze gleichzeitig laufender Hilfsagenten.
    #[arg(long, value_name = "N")]
    pub max_parallel: Option<usize>,
}

impl AnalyzeArgs {
    /// Liefert die wirksame Reihenfolge unter Berücksichtigung der
    /// versteckten Alt-Flags `--bottom-up`/`--top-down`.
    ///
    /// # Returns
    /// [`AnalyzeOrder::TopDown`] bei `--top-down`, [`AnalyzeOrder::BottomUp`]
    /// bei `--bottom-up`, sonst den Wert von `--order`.
    #[must_use]
    pub fn effective_order(&self) -> AnalyzeOrder {
        if self.top_down {
            return AnalyzeOrder::TopDown;
        }
        if self.bottom_up {
            return AnalyzeOrder::BottomUp;
        }
        match self.order {
            AnalyzeOrder::BottomUp => AnalyzeOrder::BottomUp,
            AnalyzeOrder::TopDown => AnalyzeOrder::TopDown,
        }
    }
}
