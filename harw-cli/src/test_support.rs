//! Test-Fehlertyp dieses Crates: ersetzt panic!/unwrap/expect in Tests (Bible R087/R165/R182).
//!
//! Jede Testfunktion in `harw-cli` gibt statt `()` ein [`TestResult`] zurück; ein
//! Fehlschlag wird als `Err` propagiert statt eine Panik auszulösen. Fremdfehler
//! (aus `std`, `serde_json`, `toml` oder anderen `harw`-Crates) werden über
//! [`ctx`] bzw. [`some_or`] mit einem Kontextlabel in [`TestError`] übersetzt,
//! statt für jeden Fremdfehlertyp eine eigene `From`-Variante zu pflegen — der
//! Kontextlabel trägt dieselbe Information wie ein vormaliges `.expect("…")`.

#![allow(dead_code)] // Gemeinsames Test-Gerüst: nicht jede Crate nutzt alle Varianten/Helfer.

use std::fmt;

/// Fehler eines Tests; jeder Fehlschlag wird als `Err` zurückgegeben statt zu paniken.
pub(crate) enum TestError {
    /// Ein erwarteter Wert fehlte (`Option` war `None`).
    Missing(&'static str),
    /// Ein Ergebnis hatte eine unerwartete Form (z. B. `Ok` statt des erwarteten `Err`).
    Unexpected(String),
    /// Ein Fehler mit Kontext (ersetzt `.unwrap()`/`.expect("…")` auf `Result`).
    Context {
        /// Kurzbeschreibung, was fehlgeschlagen ist.
        context: &'static str,
        /// Textform des ursprünglichen Fremdfehlers.
        source: String,
    },
}

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TestError::Missing(what) => write!(f, "erwarteter Wert fehlt: {what}"),
            TestError::Unexpected(msg) => write!(f, "unerwartetes Ergebnis: {msg}"),
            TestError::Context { context, source } => write!(f, "{context}: {source}"),
        }
    }
}

impl fmt::Debug for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for TestError {}

/// Ergebnistyp aller Testfunktionen dieses Crates (ersetzt `()` + Panik).
pub(crate) type TestResult<T = ()> = Result<T, TestError>;

/// Übersetzt einen Fremdfehler mit Kontext in [`TestError::Context`]; ersetzt
/// `.expect("…")`/`.unwrap()` auf `Result<T, E>` per `.map_err(ctx("…"))?`.
pub(crate) fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |source| TestError::Context {
        context,
        source: source.to_string(),
    }
}

/// Übersetzt ein fehlendes `Option<T>` in [`TestError::Missing`]; ersetzt
/// `.expect("…")`/`.unwrap()` auf `Option<T>` per `some_or(opt, "…")?`.
pub(crate) fn some_or<T>(opt: Option<T>, what: &'static str) -> TestResult<T> {
    opt.ok_or(TestError::Missing(what))
}
