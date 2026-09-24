//! Fehlertyp der SDK (`SdkError`).
//!
//! # Beschreibung
//! Eine Variante je Fehlerklasse, die ein Einbettender unterscheiden will
//! (Eingabe, Home, Konfiguration, Provider, Montage, Sitzung, Turn,
//! Freigabe). `Display` und [`std::error::Error`] sind von Hand geschrieben,
//! damit kein Makro aus einem internen Crate die öffentliche Fläche prägt.
//! Interne Fehlertypen erscheinen nie in einer Signatur: sie werden an der
//! Grenze in ihre `Display`-Form übersetzt.

use std::fmt;

/// Ergebnistyp der SDK.
pub type Result<T, E = SdkError> = std::result::Result<T, E>;

/// Fehler der Harwness-SDK.
///
/// # Stabilität
/// `#[non_exhaustive]`: neue Varianten sind kein Major-Sprung. Ein `match`
/// braucht deshalb immer einen Auffangzweig.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SdkError {
    /// Eine Builder- oder Methodeneingabe ist ungültig.
    InvalidInput {
        /// Name der betroffenen Eingabe (z. B. `"cwd"`, `"tool.name"`).
        field: &'static str,
        /// Menschenlesbarer Grund.
        reason: String,
    },
    /// Der Harwness-Root-Space (`~/.harw` bzw. `HARW_HOME`) ist nicht
    /// auflösbar oder nicht anlegbar.
    Home {
        /// Menschenlesbare Angabe ohne Geheimnisse.
        detail: String,
    },
    /// Konfiguration nicht ladbar, nicht vertrauenswürdig oder unvollständig
    /// (z. B. keine aktive UIA).
    Config {
        /// Menschenlesbare Angabe ohne Geheimnisse.
        detail: String,
    },
    /// Kein nutzbarer Modell-Provider.
    Provider {
        /// Menschenlesbare Angabe ohne Geheimnisse.
        detail: String,
    },
    /// Die Runtime-Montage schlug fehl (Registry, Sandbox, Spawner, …).
    Setup {
        /// Menschenlesbare Angabe ohne Geheimnisse.
        detail: String,
    },
    /// Sitzung oder Verlaufsspeicher nicht verfügbar.
    Session {
        /// Menschenlesbare Angabe ohne Geheimnisse.
        detail: String,
    },
    /// Eine fortzusetzende Sitzung hat keinen gespeicherten Verlauf.
    SessionNotFound {
        /// Die angefragte Sitzungskennung.
        id: String,
    },
    /// Der Turn konnte nicht ausgeführt werden (Kernfehler, nicht ein
    /// regulärer Ausgang wie Abbruch oder Ablehnung — die meldet
    /// [`crate::TurnReport::status`]).
    Turn {
        /// Menschenlesbare Angabe ohne Geheimnisse.
        detail: String,
    },
    /// Eine Freigabe konnte nicht zugestellt oder aufgelöst werden.
    Approval {
        /// Menschenlesbare Angabe ohne Geheimnisse.
        detail: String,
    },
}

impl SdkError {
    /// Baut [`SdkError::InvalidInput`].
    pub(crate) fn invalid(field: &'static str, reason: impl Into<String>) -> Self {
        Self::InvalidInput {
            field,
            reason: reason.into(),
        }
    }

    /// Übersetzt einen Montagefehler der Runtime an der SDK-Grenze.
    ///
    /// # Beschreibung
    /// Konfiguration und Vertrauen → [`SdkError::Config`], Provider →
    /// [`SdkError::Provider`], Speicher → [`SdkError::Session`], alles Übrige
    /// → [`SdkError::Setup`]. Der Auffangzweig hält die Abbildung stabil, wenn
    /// die Runtime neue Phasen bekommt.
    pub(crate) fn from_runtime(error: harw_runtime::RuntimeError) -> Self {
        use harw_runtime::RuntimeError;
        match error {
            RuntimeError::Config { detail } | RuntimeError::Trust { detail } => {
                Self::Config { detail }
            }
            RuntimeError::Provider { detail } => Self::Provider { detail },
            RuntimeError::Store { detail } => Self::Session { detail },
            other => Self::Setup {
                detail: other.to_string(),
            },
        }
    }

    /// Übersetzt einen Kernfehler eines Turns.
    pub(crate) fn turn(error: impl fmt::Display) -> Self {
        Self::Turn {
            detail: error.to_string(),
        }
    }
}

impl fmt::Display for SdkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput { field, reason } => {
                write!(f, "harwness-sdk: invalid input '{field}': {reason}")
            }
            Self::Home { detail } => write!(f, "harwness-sdk: home error: {detail}"),
            Self::Config { detail } => write!(f, "harwness-sdk: config error: {detail}"),
            Self::Provider { detail } => write!(f, "harwness-sdk: provider error: {detail}"),
            Self::Setup { detail } => write!(f, "harwness-sdk: setup error: {detail}"),
            Self::Session { detail } => write!(f, "harwness-sdk: session error: {detail}"),
            Self::SessionNotFound { id } => {
                write!(f, "harwness-sdk: no stored session with id '{id}'")
            }
            Self::Turn { detail } => write!(f, "harwness-sdk: turn error: {detail}"),
            Self::Approval { detail } => write!(f, "harwness-sdk: approval error: {detail}"),
        }
    }
}

impl std::error::Error for SdkError {}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn display_names_the_class_and_detail() -> TestResult {
        let error = SdkError::invalid("cwd", "does not exist");
        assert_eq!(
            error.to_string(),
            "harwness-sdk: invalid input 'cwd': does not exist"
        );
        let missing = SdkError::SessionNotFound { id: "abc".into() };
        assert!(missing.to_string().contains("'abc'"));
        Ok(())
    }

    #[test]
    fn runtime_errors_map_to_their_sdk_class() -> TestResult {
        use harw_runtime::RuntimeError;
        let cases = [
            (
                RuntimeError::Config { detail: "c".into() },
                SdkError::Config { detail: "c".into() },
            ),
            (
                RuntimeError::Trust { detail: "t".into() },
                SdkError::Config { detail: "t".into() },
            ),
            (
                RuntimeError::Provider { detail: "p".into() },
                SdkError::Provider { detail: "p".into() },
            ),
            (
                RuntimeError::Store { detail: "s".into() },
                SdkError::Session { detail: "s".into() },
            ),
        ];
        for (runtime, expected) in cases {
            assert_eq!(SdkError::from_runtime(runtime), expected);
        }
        let setup = SdkError::from_runtime(RuntimeError::Registry { detail: "r".into() });
        let SdkError::Setup { detail } = setup else {
            return Err("registry errors must map to Setup".into());
        };
        assert!(detail.contains('r'));
        Ok(())
    }
}
