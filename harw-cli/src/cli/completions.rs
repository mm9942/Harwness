//! Grammatik von `harw completions` (Alias `completion`).
//!
//! Die eigentlichen Argumente (`shell`, `--install`, `--uninstall`,
//! `--dry-run`) stammen aus [`harw_completions::CompletionsArgs`]; hier kommt
//! nur `--all-binaries` für die harw-DoD-Binaries hinzu.

use clap::Args;

/// Argumente des `harw completions`-Subcommands.
#[derive(Debug, Clone, Args)]
pub struct CompletionsCommand {
    /// Gemeinsame Completion-Argumente (Shell, Installieren/Entfernen, Dry-Run).
    #[command(flatten)]
    pub args: harw_completions::CompletionsArgs,
    /// Also run `<bin> completions <shell> --install|--uninstall` for every harw DoD binary found on $PATH
    /// (harw-sentinel, harw-warden, harw-probe-fs, harw-probe-bpf); missing ones are skipped.
    #[arg(long, requires = "completions_action")]
    pub all_binaries: bool,
}
