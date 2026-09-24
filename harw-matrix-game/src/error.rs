//! Zentraler Fehlertyp des Matrix-Game-Kerns.

use std::fmt;

/// Fehler des deterministischen Matrix-Game-Kerns.
///
/// Validierungsfehler sammeln alle Befunde statt beim ersten abzubrechen,
/// damit ein Reprompt (Agent) oder eine Szenario-Korrektur (Mensch) alle
/// Probleme auf einmal sieht.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatrixError {
    /// Das Szenario-TOML ist syntaktisch oder strukturell nicht lesbar.
    ScenarioParse(String),
    /// Das Szenario ist lesbar, verletzt aber Validierungsregeln.
    ScenarioInvalid(Vec<String>),
    /// Eine Agenten-Antwort verletzt ihren JSON-Contract.
    Contract(Vec<String>),
    /// JSON-(De-)Serialisierung schlug fehl.
    Json(String),
    /// Ein-/Ausgabefehler beim Journal.
    Io(String),
    /// Eine Journalzeile ist nicht lesbar.
    Journal {
        /// 1-basierte Zeilennummer in der JSONL-Datei.
        line: usize,
        /// Ursache.
        reason: String,
    },
    /// Unzulässiger Phasenübergang oder Aufruf in falscher Phase.
    Phase(String),
    /// Ein Journal-Eintrag passt nicht zum Spielzustand.
    State(String),
    /// Replay weicht vom Journal ab (`ReplayDivergence`).
    Replay(String),
    /// Commitment oder Offenlegung ist inkonsistent.
    Commitment(String),
}

impl fmt::Display for MatrixError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ScenarioParse(msg) => write!(f, "Szenario nicht lesbar: {msg}"),
            Self::ScenarioInvalid(errs) => {
                write!(f, "Szenario ungültig: {}", errs.join("; "))
            }
            Self::Contract(errs) => write!(f, "Contract verletzt: {}", errs.join("; ")),
            Self::Json(msg) => write!(f, "JSON-Fehler: {msg}"),
            Self::Io(msg) => write!(f, "E/A-Fehler: {msg}"),
            Self::Journal { line, reason } => {
                write!(f, "Journalzeile {line} nicht lesbar: {reason}")
            }
            Self::Phase(msg) => write!(f, "Phasenfehler: {msg}"),
            Self::State(msg) => write!(f, "Zustandsfehler: {msg}"),
            Self::Replay(msg) => write!(f, "ReplayDivergence: {msg}"),
            Self::Commitment(msg) => write!(f, "Commitment-Fehler: {msg}"),
        }
    }
}

impl std::error::Error for MatrixError {}

impl From<serde_json::Error> for MatrixError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err.to_string())
    }
}

impl From<std::io::Error> for MatrixError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

/// Ergebnis-Alias des Crates.
pub type MatrixResult<T> = Result<T, MatrixError>;
