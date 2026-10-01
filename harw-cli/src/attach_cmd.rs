//! `harw attach`: an eine Host-Sitzung über `harw-session-remote` anhängen
//! (W00 W04/W06, Scope S09).
//!
//! Skelett: der Handler meldet „nicht implementiert“. S09 ersetzt den Rumpf
//! (Endpoint aus `--socket`/`--host`, `RemotePort`, TUI-Attach) und behält die
//! Signatur.

use crate::cli::AttachArgs;

/// Führt `harw attach` aus.
///
/// # Errors
/// Im Skelett immer ein deutscher Fehlertext „nicht implementiert“.
pub fn run(args: &AttachArgs) -> Result<(), String> {
    let _ = args;
    Err("`harw attach` ist noch nicht implementiert (Skelett, Scope S09)".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skeleton_reports_not_implemented() {
        let args = AttachArgs {
            session: None,
            socket: None,
            host: None,
        };
        let result = run(&args);
        assert!(result.is_err_and(|message| message.contains("nicht implementiert")));
    }
}
