//! `/quit` — TUI-Beenden-Operation für Harwness.
//!
//! # Verantwortungsbereich
//! Implementiert die `/quit`-Operation, die ausschließlich als TUI-Command
//! exponiert wird (`tui_only`). Das Modell darf sich nicht selbst beenden —
//! daher **kein** `model_tool`-Surface.
//!
//! # Schlüsseltypen
//! - [`QuitArgs`] — leere Argumente-Struct (kein Parameter erforderlich)
//! - [`QuitOperation`] — generiertes Unit-Struct (via `#[operation]`-Makro)
//!
//! # Steuer-Sentinel
//! Die Operation gibt den Text `__QUIT__` zurück. Dieser Wert ist ein
//! Steuer-Sentinel, den die TUI-Renderer-Schicht erkennt, um einen geordneten
//! Shutdown einzuleiten. Er ist nicht für menschliche Lesbarkeit gedacht.
//!
//! # Nebenläufigkeit
//! `QuitOperation` ist `Send + Sync` (Unit-Struct, kein innerer Zustand).
//!
//! # Fehlertypen
//! Diese Operation produziert keine Fehler; sie gibt stets `Ok(OpOutput)` zurück.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_ops::quit::QuitOperation;
//! use harw_operations::Operation;
//!
//! let op = QuitOperation;
//! assert_eq!(op.meta().name, "quit");
//! ```

use harw_macros::operation;
use harw_operations::{OpContext, OpError, OpOutput};

/// Leere Argumente-Struct für die `/quit`-Operation.
///
/// # Beschreibung
/// `/quit` akzeptiert keine Parameter. Die Struct implementiert [`Default`] und
/// [`serde::Deserialize`], damit das `#[operation]`-Makro sie bei einem
/// JSON-`Null`-Input per `Default::default()` konstruieren kann.
///
/// # Beispiel
/// ```rust
/// use harw_ops::quit::QuitArgs;
///
/// let args = QuitArgs::default();
/// let _ = args; // keine Felder
/// ```
#[derive(Default, serde::Deserialize, harw_macros::FromRawArgs)]
pub struct QuitArgs {}

// Der Text `__QUIT__` ist ein Steuer-Sentinel; die TUI-Renderer-Schicht erkennt
// ihn und initiiert Shutdown. Nicht menschenlesbar rendern.
#[operation(
    name = "quit",
    summary = "Beendet die aktuelle TUI-Session.",
    domain = "session",
    permission = "operator",
    command(path = "/quit", visibility = "tui_only")
)]
async fn quit(_ctx: &OpContext, _args: QuitArgs) -> Result<OpOutput, OpError> {
    Ok(OpOutput {
        text: "__QUIT__".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::QuitArgs;
    use crate::testutil::toks;
    use harw_operations::FromRawArgs;

    #[test]
    fn test_quit_args_from_raw_args_empty_tokens_returns_ok() {
        let result = QuitArgs::from_raw_args(&toks(&[]));
        assert!(result.is_ok());
    }

    #[test]
    fn test_quit_args_from_raw_args_ignores_extra_tokens() {
        let result = QuitArgs::from_raw_args(&toks(&["ignored", "tokens"]));
        assert!(result.is_ok());
    }
}
