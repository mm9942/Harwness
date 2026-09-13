//! Shell-Completion-Generierung via `clap_complete`.

use std::io;

use clap::CommandFactory;
use clap_complete::{Shell, generate};

use crate::cli::Cli;

/// Schreibt das Completion-Skript für `shell` nach stdout.
///
/// # Arguments
/// - `shell` (`Shell`): Ziel-Shell (bash/zsh/fish/…).
///
/// # Errors
/// Aktuell infallibel; die Signatur bleibt `Result` für künftige I/O-Fehler.
pub fn print_completion(shell: Shell) -> Result<(), String> {
    let mut command = Cli::command();
    let bin_name = command.get_name().to_owned();
    generate(shell, &mut command, bin_name, &mut io::stdout());
    Ok(())
}
