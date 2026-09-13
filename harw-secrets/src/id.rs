//! Opaque identifiers (spec §2.3). These are safe to log; the *record* types
//! in [`crate::record`] are not.

use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Identifies one stored secret across its lifetime, independent of key
/// rotation (the same `SecretId` persists through every `rotate`).
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct SecretId(Uuid);

impl SecretId {
    /// Parse a canonical UUID string into a secret id.
    pub fn parse(value: &str) -> Result<Self, SecretIdParseError> {
        Uuid::parse_str(value)
            .map(Self)
            .map_err(|_| SecretIdParseError)
    }

    /// Mint a fresh time-ordered (UUIDv7) secret id.
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    /// Borrow the raw 16-byte UUID representation.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }
}

impl fmt::Display for SecretId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, formatter)
    }
}

impl FromStr for SecretId {
    type Err = SecretIdParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

/// Error returned when configuration text is not a valid secret id.
///
/// This is intentionally a unit type: malformed configuration values are not
/// retained in the error and therefore cannot be accidentally exposed by
/// diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SecretIdParseError;

impl fmt::Display for SecretIdParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid secret id")
    }
}

impl std::error::Error for SecretIdParseError {}

impl Default for SecretId {
    fn default() -> Self {
        Self::new()
    }
}

/// Monotonic generation counter for a deployment's KEK. Bumped on every
/// rotation; every `SecretRecord` remembers which generation wrapped its DEK.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct KeyVersion(pub u32);

impl KeyVersion {
    /// The genesis generation (`0`) for a freshly initialized store.
    #[must_use]
    pub fn initial() -> Self {
        Self(0)
    }

    /// The next generation, produced when a rotation begins (§2.4).
    #[must_use]
    pub fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl Default for KeyVersion {
    fn default() -> Self {
        Self::initial()
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{KeyVersion, SecretId, SecretIdParseError};

    #[test]
    fn secret_id_display_from_str_round_trip_is_stable() {
        let id = SecretId::new();
        let text = id.to_string();

        assert_eq!(SecretId::from_str(&text).unwrap(), id);
        assert_eq!(SecretId::parse(&text).unwrap(), id);
    }

    #[test]
    fn secret_id_parse_rejects_invalid_text_without_retaining_it() {
        let malformed = "not-a-secret-id";
        let error = SecretId::from_str(malformed).unwrap_err();

        assert_eq!(error, SecretIdParseError);
        assert_eq!(error.to_string(), "invalid secret id");
        assert!(!format!("{error:?}").contains(malformed));
    }

    #[test]
    fn secret_id_serde_round_trip_is_stable() {
        let id = SecretId::new();
        let encoded = serde_json::to_string(&id).unwrap();
        let decoded: SecretId = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded, id);
        assert_eq!(serde_json::to_string(&decoded).unwrap(), encoded);
    }

    #[test]
    fn key_version_serde_round_trip_is_stable() {
        let version = KeyVersion::initial().next();
        let encoded = serde_json::to_string(&version).unwrap();
        let decoded: KeyVersion = serde_json::from_str(&encoded).unwrap();

        assert_eq!(decoded, version);
        assert_eq!(serde_json::to_string(&decoded).unwrap(), encoded);
    }
}
