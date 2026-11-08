//! Fehlertyp der Crate: [`DispatchError`].
//!
//! # Zweck
//! Ein handgeschriebenes Enum für die zwei Stellen, an denen der Dispatcher
//! eine Eingabe des Aufrufers ablehnen kann: eine ungültige
//! [`crate::DispatchConfig`] ([`crate::DispatchConfig::validate`],
//! [`crate::Dispatcher::new`]) und ein unbrauchbarer, persistierter
//! [`crate::DispatchState`] ([`crate::Dispatcher::restore`],
//! [`crate::DispatchState::from_json_bounded`],
//! [`crate::DispatchState::to_json`]).
//!
//! Befunde selbst erzeugen nie einen Fehler: ein ungültiger Befund wird
//! verworfen und gezählt ([`crate::DispatchAction::Dropped`]).
//!
//! # Inhalt
//! Inhaltsfrei: keine Variante trägt einen Wert aus Befund oder Zustand,
//! nur statische Feldnamen und Gründe. So kann ein manipulierter Zustand
//! nichts über die Fehlermeldung auf ein Terminal schmuggeln; auch ein
//! Fehler von `serde_json` (der Ausschnitte der Eingabe tragen kann) wird
//! nicht weitergereicht.
//!
//! # Nebenläufigkeit
//! `Send + Sync`, `Clone`; ohne inneren Zustand.
//!
//! # Examples
//! ```
//! use harw_dod_dispatch::{DispatchConfig, DispatchError};
//!
//! let config = DispatchConfig { max_parallel: 0, ..DispatchConfig::default() };
//! let error = config.validate().err();
//! assert!(matches!(error, Some(DispatchError::InvalidConfig { field: "max_parallel", .. })));
//! ```

use std::fmt;

/// Ein Fehler beim Konfigurieren oder Wiederherstellen eines
/// [`crate::Dispatcher`].
///
/// # Description
/// Inhaltsfrei (siehe Moduldoku). `Debug` ist abgeleitet, `Display` liefert
/// eine kurze englische Meldung.
///
/// # Errors
/// Entsteht in [`crate::DispatchConfig::validate`], [`crate::Dispatcher::new`],
/// [`crate::Dispatcher::restore`], [`crate::DispatchState::to_json`] und
/// [`crate::DispatchState::from_json_bounded`].
///
/// # Examples
/// ```
/// use harw_dod_dispatch::DispatchError;
///
/// let error = DispatchError::InvalidState { what: "queue" };
/// assert_eq!(error.to_string(), "dispatch state is invalid: queue");
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DispatchError {
    /// Ein Konfigurationsfeld liegt außerhalb seines zulässigen Bereichs.
    InvalidConfig {
        /// Name des Feldes in [`crate::DispatchConfig`] (bzw. der
        /// verschachtelten Budget-/Breaker-Konfiguration).
        field: &'static str,
        /// Was am Wert falsch ist.
        reason: &'static str,
    },
    /// Der Zustand stammt aus einer anderen Formatversion.
    UnsupportedStateVersion {
        /// Version im übergebenen Zustand.
        found: u32,
        /// Version, die diese Crate liest ([`crate::DispatchState::VERSION`]).
        supported: u32,
    },
    /// Der Zustand ist inkonsistent, enthält ungültige Kennungen oder ist
    /// größer, als irgendeine gültige Konfiguration ihn schreiben kann.
    InvalidState {
        /// Welcher Teil des Zustands die Prüfung nicht bestanden hat. Eine
        /// Größengrenze heißt „too many …" (etwa `"too many bytes"` vor dem
        /// Dekodieren, `"too many table entries"` beim Dekodieren,
        /// `"too many known keys"` in [`crate::Dispatcher::restore`]), eine
        /// sonst ungültige Kodierung `"json encoding"`, ein ungültiger
        /// Eintrag trägt den Namen seiner Tabelle (etwa `"known keys"`).
        what: &'static str,
    },
}

impl fmt::Display for DispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidConfig { field, reason } => {
                write!(f, "dispatch config field {field} is invalid: {reason}")
            }
            Self::UnsupportedStateVersion { found, supported } => write!(
                f,
                "dispatch state version {found} is not supported (expected {supported})"
            ),
            Self::InvalidState { what } => write!(f, "dispatch state is invalid: {what}"),
        }
    }
}

impl std::error::Error for DispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidConfig { .. }
            | Self::UnsupportedStateVersion { .. }
            | Self::InvalidState { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    #[test]
    fn test_display_names_field_and_reason() -> TestResult {
        let error = DispatchError::InvalidConfig {
            field: "queue_capacity",
            reason: "must be at least 1",
        };
        assert_eq!(
            error.to_string(),
            "dispatch config field queue_capacity is invalid: must be at least 1"
        );
        let version = DispatchError::UnsupportedStateVersion {
            found: 7,
            supported: 1,
        };
        assert_eq!(
            version.to_string(),
            "dispatch state version 7 is not supported (expected 1)"
        );
        assert!(std::error::Error::source(&version).is_none());
        Ok(())
    }
}
