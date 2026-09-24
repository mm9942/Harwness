//! Stabile Kennungen der SDK.

use std::fmt;
use std::str::FromStr;

use crate::error::SdkError;

/// Kennung einer Harwness-Sitzung (Wurzel oder Kind-Agent).
///
/// # Beschreibung
/// Ein undurchsichtiger, nicht-leerer String. Er ist stabil über
/// Prozessneustarts: dieselbe Kennung setzt über
/// [`crate::Harwness::resume`] denselben gespeicherten Verlauf fort.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionId(String);

impl SessionId {
    /// Prüft und übernimmt eine Kennung.
    ///
    /// # Fehler
    /// [`SdkError::InvalidInput`], wenn `value` nach dem Trimmen leer ist.
    pub fn new(value: impl Into<String>) -> Result<Self, SdkError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(SdkError::invalid("session_id", "must not be empty"));
        }
        Ok(Self(value))
    }

    /// Die Kennung als String-Slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Übernimmt eine interne Kennung (nie leer, weil intern validiert).
    pub(crate) fn from_core(id: &harw_types::SessionId) -> Self {
        Self(id.as_str().to_owned())
    }

    /// Die interne Form für die Runtime.
    pub(crate) fn to_core(&self) -> Result<harw_types::SessionId, SdkError> {
        harw_types::SessionId::try_from_str(self.0.clone())
            .map_err(|error| SdkError::invalid("session_id", error.to_string()))
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for SessionId {
    type Err = SdkError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn blank_ids_are_rejected() {
        assert!(SessionId::new("").is_err());
        assert!(SessionId::new("  \t").is_err());
    }

    #[test]
    fn ids_roundtrip_through_the_core_form() -> TestResult {
        let id: SessionId = "session-1".parse()?;
        let core = id.to_core()?;
        assert_eq!(SessionId::from_core(&core), id);
        assert_eq!(id.to_string(), "session-1");
        Ok(())
    }
}
