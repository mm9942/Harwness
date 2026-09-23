//! Test-Fehlertyp dieses Crates: ersetzt panic!/unwrap/expect in Tests (Bible R087/R165/R182).

#![allow(dead_code)] // Gemeinsames Test-Gerüst: nicht jede Crate nutzt alle Varianten/Helfer.

use std::fmt;

use crate::error::{ImpactAssessmentError, InvalidId};
use crate::impact::ParseImpactSeverityError;
use crate::reasoning::ParseReasoningEffortError;

/// Fehler eines Tests; jeder Fehlschlag wird als `Err` zurückgegeben statt zu paniken.
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
    /// Eine ID-Konstruktion ist an einem leeren/nur-Whitespace-Wert gescheitert.
    Id(InvalidId),
    /// JSON-(De-)Serialisierung ist fehlgeschlagen.
    Json(serde_json::Error),
    /// Das Parsen einer [`crate::impact::ImpactSeverity`] ist fehlgeschlagen.
    Severity(ParseImpactSeverityError),
    /// Das Parsen eines [`crate::reasoning::ReasoningEffort`] ist fehlgeschlagen.
    Effort(ParseReasoningEffortError),
    /// Eine [`crate::impact::ImpactAssessment`]-Invariante wurde verletzt.
    Impact(ImpactAssessmentError),
}

/// Kurzform für `Result<T, TestError>` in Tests dieses Crates.
pub(crate) type TestResult<T = ()> = Result<T, TestError>;

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(what) => write!(f, "erwarteter Wert fehlte: {what}"),
            Self::Unexpected(msg) => write!(f, "unerwartetes Ergebnis: {msg}"),
            Self::Context { context, source } => write!(f, "{context}: {source}"),
            Self::Id(source) => write!(f, "ID-Konstruktion fehlgeschlagen: {source}"),
            Self::Json(source) => write!(f, "JSON-Fehler: {source}"),
            Self::Severity(source) => write!(f, "Severity-Parse-Fehler: {source}"),
            Self::Effort(source) => write!(f, "Effort-Parse-Fehler: {source}"),
            Self::Impact(source) => write!(f, "Impact-Assessment-Fehler: {source}"),
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
            Self::Id(source) => Some(source),
            Self::Json(source) => Some(source),
            Self::Severity(source) => Some(source),
            Self::Effort(source) => Some(source),
            Self::Impact(source) => Some(source),
            Self::Missing(_) | Self::Unexpected(_) | Self::Context { .. } => None,
        }
    }
}

impl From<InvalidId> for TestError {
    fn from(source: InvalidId) -> Self {
        Self::Id(source)
    }
}

impl From<serde_json::Error> for TestError {
    fn from(source: serde_json::Error) -> Self {
        Self::Json(source)
    }
}

impl From<ParseImpactSeverityError> for TestError {
    fn from(source: ParseImpactSeverityError) -> Self {
        Self::Severity(source)
    }
}

impl From<ParseReasoningEffortError> for TestError {
    fn from(source: ParseReasoningEffortError) -> Self {
        Self::Effort(source)
    }
}

impl From<ImpactAssessmentError> for TestError {
    fn from(source: ImpactAssessmentError) -> Self {
        Self::Impact(source)
    }
}

/// Wandelt einen Fremdfehler mit Kontext in einen [`TestError::Context`] um —
/// Ersatz für `.expect("…")` in Tests.
pub(crate) fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |source| TestError::Context {
        context,
        source: source.to_string(),
    }
}
