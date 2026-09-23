//! Grammatik von `harw analyze`: Workspace-Abhängigkeitsanalyse.

/// Argumente des `harw analyze`-Subcommands.
#[derive(Debug, Clone, clap::Args)]
pub struct AnalyzeArgs {
    /// Zu analysierendes Crate; ohne Angabe der gesamte Workspace.
    pub crate_name: Option<String>,
    /// Erzwingt die Analyse des gesamten Workspace, auch wenn ein Crate genannt ist.
    #[arg(long)]
    pub workspace: bool,
    /// Analysiert von den Blatt-Crates aufwärts (Vorgabe).
    ///
    /// Schließt sich mit `--top-down` aus: werden beide Flags gemeinsam
    /// angegeben, bricht das Parsing mit einem Fehler ab — unabhängig von der
    /// Reihenfolge, in der sie stehen. Es gibt bewusst keinen stillen
    /// Vorrang eines Flags vor dem anderen.
    #[arg(long, default_value_t = true, conflicts_with = "top_down")]
    pub bottom_up: bool,
    /// Kehrt die Reihenfolge um: von den Wurzeln abwärts.
    ///
    /// Siehe `--bottom-up`: beide Flags zusammen sind ein Fehler, kein
    /// stiller Vorrang.
    #[arg(long)]
    pub top_down: bool,
    /// Legt den Plan an, startet aber keine Kind-Agenten.
    #[arg(long)]
    pub dry_run: bool,
    /// Obergrenze gleichzeitig laufender Kind-Agenten.
    #[arg(long, value_name = "N")]
    pub max_parallel: Option<usize>,
}
