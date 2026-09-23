//! Fehlertypen für `harw-operations`.
//!
//! Definiert [`OpError`], den zentralen Fehler-Enum dieser Crate.
//! Alle Varianten sind eigenständig und tragen ausreichend Kontext,
//! um die Fehlerursache ohne Quellcode-Blick zu verstehen.
//!
//! Kein `anyhow`, kein `thiserror` — handgeschriebene Implementierungen
//! gemäß Harness-Konventionen.

use std::fmt;

/// Fehler, der bei der Ausführung oder Vorbereitung einer [`crate::operation::Operation`]
/// auftreten kann.
///
/// # Varianten
/// - [`OpError::InvalidArguments`]: Eingabe ist syntaktisch oder semantisch ungültig.
/// - [`OpError::Execution`]: Kernausführung schlug fehl (Laufzeitfehler).
/// - [`OpError::NotAvailable`]: Die Operation ist in der aktuellen Konfiguration
///   nicht verfügbar (fehlende Berechtigung, deaktiviertes Feature u. Ä.).
///
/// # Beispiel
/// ```rust,no_run
/// use harw_operations::error::OpError;
///
/// fn validate(args: &[String]) -> Result<(), OpError> {
///     if args.is_empty() {
///         return Err(OpError::InvalidArguments("Mindestens ein Argument erwartet".to_owned()));
///     }
///     Ok(())
/// }
/// ```
#[derive(Clone, PartialEq, Eq)]
pub enum OpError {
    /// Die übergebenen Argumente sind ungültig (Syntaxfehler, fehlende Pflichtfelder,
    /// ungültige Wertebereichs-Kombination). Enthält eine menschenlesbare Beschreibung.
    InvalidArguments(String),

    /// Die Operationsausführung selbst schlug fehl (z. B. I/O-Fehler, Datenbankfehler,
    /// externer Dienst nicht erreichbar). Enthält eine menschenlesbare Fehlermeldung.
    Execution(String),

    /// Die Operation ist im aktuellen Kontext nicht verfügbar (unzureichende Berechtigungen,
    /// Feature deaktiviert, Abhängigkeit fehlt). Enthält eine menschenlesbare Begründung.
    NotAvailable(String),
}

impl fmt::Display for OpError {
    /// Gibt eine menschenlesbare Fehlerbeschreibung aus.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArguments(msg) => write!(f, "Ungültige Argumente: {msg}"),
            Self::Execution(msg) => write!(f, "Ausführungsfehler: {msg}"),
            Self::NotAvailable(msg) => write!(f, "Operation nicht verfügbar: {msg}"),
        }
    }
}

impl fmt::Debug for OpError {
    /// Delegiert an [`fmt::Display`], sodass Debug- und Display-Ausgabe identisch sind.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for OpError {
    /// Gibt die Ursache des Fehlers zurück.
    ///
    /// Da `OpError`-Varianten keine fremdtypigen Fehler einwickeln (Stand: initial),
    /// liefert diese Implementierung stets `None`. Ein Folge-Agent kann `From`-Impls
    /// ergänzen und `source()` entsprechend anpassen.
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::OpError;
    use crate::test_support::{TestError, TestResult};
    use std::error::Error;

    // ── helpers ───────────────────────────────────────────────────────────────

    fn invalid_args(msg: &str) -> OpError {
        OpError::InvalidArguments(msg.to_owned())
    }

    fn execution(msg: &str) -> OpError {
        OpError::Execution(msg.to_owned())
    }

    fn not_available(msg: &str) -> OpError {
        OpError::NotAvailable(msg.to_owned())
    }

    // ── Display ───────────────────────────────────────────────────────────────

    #[test]
    fn test_display_invalid_arguments_contains_prefix_and_message() {
        let err = invalid_args("fehlende Felder");
        let text = err.to_string();
        assert!(
            text.contains("Ungültige Argumente"),
            "Display sollte 'Ungültige Argumente' enthalten, war: {text}"
        );
        assert!(
            text.contains("fehlende Felder"),
            "Display sollte die Nutzlast enthalten, war: {text}"
        );
    }

    #[test]
    fn test_display_execution_contains_prefix_and_message() {
        let err = execution("Datenbankverbindung verloren");
        let text = err.to_string();
        assert!(
            text.contains("Ausführungsfehler"),
            "Display sollte 'Ausführungsfehler' enthalten, war: {text}"
        );
        assert!(
            text.contains("Datenbankverbindung verloren"),
            "Display sollte die Nutzlast enthalten, war: {text}"
        );
    }

    #[test]
    fn test_display_not_available_contains_prefix_and_message() {
        let err = not_available("Feature deaktiviert");
        let text = err.to_string();
        assert!(
            text.contains("Operation nicht verfügbar"),
            "Display sollte 'Operation nicht verfügbar' enthalten, war: {text}"
        );
        assert!(
            text.contains("Feature deaktiviert"),
            "Display sollte die Nutzlast enthalten, war: {text}"
        );
    }

    #[test]
    fn test_display_empty_message_still_formats() {
        let err = invalid_args("");
        let text = err.to_string();
        assert!(
            text.contains("Ungültige Argumente"),
            "Display sollte mit leerer Nutzlast noch funktionieren, war: {text}"
        );
    }

    // ── Debug delegates to Display ────────────────────────────────────────────

    #[test]
    fn test_debug_matches_display_for_invalid_arguments() {
        let err = invalid_args("ein Fehler");
        assert_eq!(format!("{err:?}"), format!("{err}"));
    }

    #[test]
    fn test_debug_matches_display_for_execution() {
        let err = execution("ein Fehler");
        assert_eq!(format!("{err:?}"), format!("{err}"));
    }

    #[test]
    fn test_debug_matches_display_for_not_available() {
        let err = not_available("ein Fehler");
        assert_eq!(format!("{err:?}"), format!("{err}"));
    }

    // ── std::error::Error ─────────────────────────────────────────────────────

    #[test]
    fn test_source_returns_none_for_invalid_arguments() {
        let err = invalid_args("x");
        assert!(err.source().is_none());
    }

    #[test]
    fn test_source_returns_none_for_execution() {
        let err = execution("x");
        assert!(err.source().is_none());
    }

    #[test]
    fn test_source_returns_none_for_not_available() {
        let err = not_available("x");
        assert!(err.source().is_none());
    }

    #[test]
    fn test_source_returns_none_for_all_variants_empty_message() {
        assert!(OpError::InvalidArguments(String::new()).source().is_none());
        assert!(OpError::Execution(String::new()).source().is_none());
        assert!(OpError::NotAvailable(String::new()).source().is_none());
    }

    // ── implements std::error::Error (trait object coercion) ──────────────────

    #[test]
    fn test_op_error_is_std_error() {
        let err: &dyn Error = &invalid_args("test");
        assert!(err.source().is_none());
        assert!(!err.to_string().is_empty());
    }

    // ── PartialEq / Clone ─────────────────────────────────────────────────────

    #[test]
    fn test_partial_eq_same_variant_and_message() {
        let a = execution("io error");
        let b = execution("io error");
        assert_eq!(a, b);
    }

    #[test]
    fn test_partial_eq_different_variants_not_equal() {
        let a = execution("msg");
        let b = not_available("msg");
        assert_ne!(a, b);
    }

    #[test]
    fn test_partial_eq_same_variant_different_messages_not_equal() {
        let a = invalid_args("alpha");
        let b = invalid_args("beta");
        assert_ne!(a, b);
    }

    #[test]
    fn test_clone_produces_equal_value() {
        let original = not_available("permission denied");
        let cloned = original.clone();
        assert_eq!(original, cloned);
    }

    // ── Result<_, OpError> ergonomics ─────────────────────────────────────────

    #[test]
    fn test_op_error_usable_as_result_err_variant() -> TestResult {
        fn always_fail() -> Result<(), OpError> {
            Err(OpError::Execution("simulated".to_owned()))
        }
        let result = always_fail();
        assert!(result.is_err());
        match result {
            Err(OpError::Execution(msg)) => assert_eq!(msg, "simulated"),
            other => {
                return Err(TestError::Unexpected(format!(
                    "unerwartetes Ergebnis: {other:?}"
                )));
            }
        }
        Ok(())
    }
}
