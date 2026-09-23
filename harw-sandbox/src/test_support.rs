//! Test-Fehlertyp dieses Crates: ersetzt panic!/unwrap/expect in Tests (Bible R087/R165/R182).

use std::fmt;

use crate::{CargoProfileError, SandboxError, TmuxProfileError};
use harw_authority::AuthorityError;

/// Fehler eines Tests; jeder Fehlschlag wird als `Err` zurückgegeben statt zu paniken.
#[allow(dead_code)]
pub(crate) enum TestError {
    /// Ein erwarteter Wert fehlte (`Option` war `None`).
    Missing(&'static str),
    /// Ein Ergebnis hatte eine unerwartete Form.
    Unexpected(String),
    /// Ein Fehler mit Kontext (ersetzt `expect("…")`).
    Context {
        context: &'static str,
        source: String,
    },
    /// Ein Fehler aus diesem Crate selbst (`SandboxError`).
    Sandbox(SandboxError),
    /// Ein I/O-Fehler aus einer Test-Fixture.
    Io(std::io::Error),
    /// Ein Fehler aus `harw_authority`, z. B. beim Aufbau einer Test-Workspace-Registry.
    Authority(AuthorityError),
    /// Ein Fehler beim Aufbau eines Test-`CargoSandboxProfile`.
    CargoProfile(CargoProfileError),
    /// Ein Fehler beim Aufbau eines Test-`TmuxSandboxProfile`.
    TmuxProfile(TmuxProfileError),
}

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TestError::Missing(what) => write!(f, "erwarteter Wert fehlt: {what}"),
            TestError::Unexpected(msg) => write!(f, "unerwartetes Ergebnis: {msg}"),
            TestError::Context { context, source } => write!(f, "{context}: {source}"),
            TestError::Sandbox(err) => write!(f, "Sandbox-Fehler: {err}"),
            TestError::Io(err) => write!(f, "I/O-Fehler: {err}"),
            TestError::Authority(err) => write!(f, "Authority-Fehler: {err}"),
            TestError::CargoProfile(err) => write!(f, "Cargo-Profil-Fehler: {err}"),
            TestError::TmuxProfile(err) => write!(f, "Tmux-Profil-Fehler: {err}"),
        }
    }
}

impl fmt::Debug for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for TestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TestError::Sandbox(err) => Some(err),
            TestError::Io(err) => Some(err),
            TestError::Authority(err) => Some(err),
            TestError::CargoProfile(err) => Some(err),
            TestError::TmuxProfile(err) => Some(err),
            _ => None,
        }
    }
}

impl From<SandboxError> for TestError {
    fn from(err: SandboxError) -> Self {
        TestError::Sandbox(err)
    }
}

impl From<std::io::Error> for TestError {
    fn from(err: std::io::Error) -> Self {
        TestError::Io(err)
    }
}

impl From<AuthorityError> for TestError {
    fn from(err: AuthorityError) -> Self {
        TestError::Authority(err)
    }
}

impl From<CargoProfileError> for TestError {
    fn from(err: CargoProfileError) -> Self {
        TestError::CargoProfile(err)
    }
}

impl From<TmuxProfileError> for TestError {
    fn from(err: TmuxProfileError) -> Self {
        TestError::TmuxProfile(err)
    }
}

/// Ergebnistyp für Tests dieses Crates; ersetzt Panics durch `Err`-Rückgaben.
pub(crate) type TestResult<T = ()> = Result<T, TestError>;

/// Baut aus einem Kontext-String eine Funktion, die einen Fremdfehler in
/// `TestError::Context` überführt (ersetzt `.expect("…")` auf `Result`).
#[allow(dead_code)]
pub(crate) fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |source| TestError::Context {
        context,
        source: source.to_string(),
    }
}
