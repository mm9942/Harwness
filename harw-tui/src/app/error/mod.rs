//! TUI error contract.

use harw_extension_api::registry::ContextProviderRegistrationError;
use std::io;

/// Fehler des TUI-Chat-Renderers.
///
/// # Beschreibung
/// Handgeschriebener Fehler-Enum (kein `anyhow`/`thiserror`) mit `From`-Impl für
/// [`std::io::Error`], damit `?` an I/O-Grenzen funktioniert. Beide Varianten
/// tragen alle Informationen, die zum Verstehen des Fehlers nötig sind.
///
/// # Fehler-Varianten
/// - [`TuiError::Io`] — bei Terminal-Setup (Raw-Mode, Bracketed-Paste,
///   Inline-Viewport), beim Zeichnen oder bei der Event-Verarbeitung.
/// - [`TuiError::Core`] — wenn der Turn-Loop einen Fehler liefert oder keine
///   Assistant-Antwort in der Session-Historie vorhanden ist.
///
/// # Beispiele
/// ```ignore
/// use harw_tui::app::TuiError;
/// let err = TuiError::Core("Modell antwortet nicht".to_owned());
/// assert!(err.to_string().contains("chat turn failed"));
/// ```
pub enum TuiError {
    /// I/O-Fehler bei Terminal-Setup, Zeichnen oder Event-Verarbeitung.
    Io(io::Error),
    /// Vom Core/Turn-Loop gemeldeter Fehler (als Text übernommen).
    Core(String),
    /// Ein Kontextanbieter deklariert einen leeren oder bereits vergebenen
    /// Namensraum beim Zusammenbau einer Registry durch die Composition-Root.
    ContextProviderRegistration(ContextProviderRegistrationError),
}

/// Menschenlesbare Darstellung; enthält weder interne Feldnamen noch Rust-Typen.
impl std::fmt::Display for TuiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TuiError::Io(error) => write!(formatter, "terminal I/O failed: {error}"),
            TuiError::Core(message) => write!(formatter, "chat turn failed: {message}"),
            TuiError::ContextProviderRegistration(error) => {
                write!(formatter, "context provider registration failed: {error}")
            }
        }
    }
}

/// Debug delegiert an [`std::fmt::Display`], um eine einzige Formatierung zu halten.
impl std::fmt::Debug for TuiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Debug delegiert an Display, um eine einzige Formatierung zu halten.
        std::fmt::Display::fmt(self, formatter)
    }
}

/// Implementiert `std::error::Error`; `source()` liefert den eingebetteten I/O-Fehler.
impl std::error::Error for TuiError {
    /// Gibt den eingebetteten [`std::io::Error`] zurück, falls vorhanden.
    ///
    /// # Rückgabe
    /// - `Some(&io::Error)` für [`TuiError::Io`].
    /// - `None` für [`TuiError::Core`].
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TuiError::Io(error) => Some(error),
            TuiError::Core(_) => None,
            TuiError::ContextProviderRegistration(error) => Some(error),
        }
    }
}

/// Konvertiert [`std::io::Error`] in [`TuiError::Io`], sodass `?` an I/O-Grenzen
/// funktioniert.
impl From<io::Error> for TuiError {
    fn from(error: io::Error) -> Self {
        TuiError::Io(error)
    }
}

/// Konvertiert [`ContextProviderRegistrationError`] in
/// [`TuiError::ContextProviderRegistration`], sodass `?` beim Registry-Umbau
/// funktioniert.
impl From<ContextProviderRegistrationError> for TuiError {
    fn from(error: ContextProviderRegistrationError) -> Self {
        TuiError::ContextProviderRegistration(error)
    }
}
