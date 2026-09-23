//! Test-Fehlertyp dieses Crates: ersetzt panic!/unwrap/expect in Tests (Bible R087/R165/R182).
//!
//! Jeder Testfehlschlag wird als `Err(TestError)` zurückgegeben statt zu paniken. Die
//! Helferfunktion [`ctx`] übersetzt einen beliebigen fremden Fehlertyp (`E: Display`)
//! generisch in [`TestError::Context`] — dadurch braucht dieses Modul keine eigene
//! `From`-Variante je Fremdfehlertyp (Tests in dieser Crate berühren u. a.
//! `harw_operations::OpError`, `std::io::Error`, `serde_json::Error`,
//! `harw_plan::error::PlanError`, `toml`/`toml_edit`-Fehler und mehrere
//! Store-eigene Fehlertypen; eine generische `Display`-Brücke ist hier robuster als
//! ein Dutzend einzelner `From`-Impls, siehe Abschlussbericht).

use std::fmt;

/// Fehler eines Tests; jeder Fehlschlag wird als `Err` zurückgegeben statt zu paniken.
#[derive(Debug)]
pub(crate) enum TestError {
    /// Ein erwarteter Wert fehlte (`Option` war `None`).
    Missing(&'static str),
    /// Ein Ergebnis hatte eine unerwartete Form.
    Unexpected(String),
    /// Ein Fehler mit Kontext (ersetzt `expect("…")` bzw. `unwrap()` auf `Result`).
    Context {
        context: &'static str,
        source: String,
    },
}

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(what) => write!(f, "erwarteter Wert fehlte: {what}"),
            Self::Unexpected(msg) => write!(f, "unerwartetes Ergebnis: {msg}"),
            Self::Context { context, source } => write!(f, "{context}: {source}"),
        }
    }
}

impl std::error::Error for TestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

/// Ergebnistyp für Tests dieser Crate; Standard-`Ok`-Wert ist `()`.
pub(crate) type TestResult<T = ()> = Result<T, TestError>;

/// Baut eine `map_err`-Closure, die einen beliebigen `Display`-Fehler mit Kontext
/// in [`TestError::Context`] übersetzt (ersetzt `.expect("msg")`/`.unwrap()` auf `Result`).
///
/// # Arguments
/// - `context` (`&'static str`): menschenlesbare Beschreibung der fehlgeschlagenen Aktion.
///
/// # Returns
/// `impl FnOnce(E) -> TestError`, geeignet für `.map_err(ctx("…"))?`.
pub(crate) fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |source| TestError::Context {
        context,
        source: source.to_string(),
    }
}
