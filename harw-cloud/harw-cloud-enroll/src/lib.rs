//! Device enrollment types for the Harwness cloud home (PL-65/68/94).
//!
//! This crate models the enrollment record kept per device, the short-lived
//! pairing code exchanged during enrollment, and the validation of pairing
//! codes. Validation does not consume an invitation or commit a device/key
//! binding; durable enrollment is a separate integration boundary.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Lifecycle status of an enrolled device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EnrollmentStatus {
    /// The device is enrolled and allowed to connect.
    Active,
    /// Enrollment was revoked; the device is no longer accepted.
    Revoked,
}

/// Persistent record of an enrolled cloud home device.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnrollmentRecord {
    /// Stable identifier of the device.
    pub device_id: String,
    /// Tenant the device belongs to.
    pub tenant: String,
    /// Service tier granted to the device.
    pub tier: String,
    /// Lifecycle status of the enrollment.
    pub status: EnrollmentStatus,
    /// Human-readable label for the device.
    pub label: String,
    /// ISO-8601 timestamp of enrollment.
    pub enrolled_at: String,
}

/// A short-lived pairing code issued for device enrollment.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingCode {
    /// The pairing code string presented to the device owner.
    pub code: String,
    /// ISO-8601 timestamp after which the code is no longer valid.
    pub expires_at: String,
}

impl fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairingCode")
            .field("code", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Errors that can occur during pairing validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrollmentError {
    /// The presented code does not match the expected one.
    WrongCode,
    /// The expected code has reached or passed its expiry.
    Expired,
    /// Expiry is not a valid absolute timestamp with a UTC offset.
    InvalidExpiry,
}

impl fmt::Display for EnrollmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EnrollmentError::WrongCode => write!(f, "pairing code does not match"),
            EnrollmentError::Expired => write!(f, "pairing code has expired"),
            EnrollmentError::InvalidExpiry => write!(f, "pairing code expiry is invalid"),
        }
    }
}

impl std::error::Error for EnrollmentError {}

/// Validate a presented code using the current clock.
///
/// See [`validate_pairing_at`] for deterministic validation and exact expiry
/// semantics. No code is consumed by this helper.
pub fn validate_pairing(code: &str, expected: &PairingCode) -> Result<(), EnrollmentError> {
    validate_pairing_at(code, expected, Timestamp::now())
}

/// Validate using an injected current instant.
///
/// Expiry is parsed as an absolute timestamp, so UTC offsets and fractional
/// seconds compare by instant. A code is invalid at its expiry, not just after
/// it. Invalid expiry fails closed before comparing the presented code.
///
/// # Errors
/// [`EnrollmentError::InvalidExpiry`], [`EnrollmentError::Expired`], or
/// [`EnrollmentError::WrongCode`].
pub fn validate_pairing_at(
    code: &str,
    expected: &PairingCode,
    now: Timestamp,
) -> Result<(), EnrollmentError> {
    let expires_at: Timestamp = expected
        .expires_at
        .parse()
        .map_err(|_| EnrollmentError::InvalidExpiry)?;
    if now >= expires_at {
        return Err(EnrollmentError::Expired);
    }
    if !constant_time_eq(code.as_bytes(), expected.code.as_bytes()) {
        return Err(EnrollmentError::WrongCode);
    }
    Ok(())
}

/// Existing length-checking comparison; no cryptographic timing guarantee is claimed.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_serde_round_trip() {
        let record = EnrollmentRecord {
            device_id: "dev-1".into(),
            tenant: "home".into(),
            tier: "standard".into(),
            status: EnrollmentStatus::Active,
            label: "Living room sensor".into(),
            enrolled_at: "2026-02-27T12:00:00Z".into(),
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("\"deviceId\""));
        assert!(json.contains("\"enrolledAt\""));
        assert_eq!(json, serde_json::to_string(&record.clone()).unwrap());
        let back: EnrollmentRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back, record);
    }

    #[test]
    fn status_serde_round_trip() {
        for status in [EnrollmentStatus::Active, EnrollmentStatus::Revoked] {
            let json = serde_json::to_string(&status).unwrap();
            let back: EnrollmentStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(back, status);
        }
        assert_eq!(
            serde_json::to_string(&EnrollmentStatus::Revoked).unwrap(),
            "\"revoked\""
        );
    }

    #[test]
    fn validate_pairing_ok() {
        let expected = PairingCode {
            code: "abc123".into(),
            expires_at: "2999-01-01T00:00:00Z".into(),
        };
        assert!(validate_pairing("abc123", &expected).is_ok());
    }

    #[test]
    fn validate_pairing_wrong_code() {
        let expected = PairingCode {
            code: "abc123".into(),
            expires_at: "2999-01-01T00:00:00Z".into(),
        };
        assert_eq!(
            validate_pairing("nope", &expected),
            Err(EnrollmentError::WrongCode)
        );
    }

    #[test]
    fn validate_pairing_expired() {
        let expected = PairingCode {
            code: "abc123".into(),
            expires_at: "2000-01-01T00:00:00Z".into(),
        };
        assert_eq!(
            validate_pairing("abc123", &expected),
            Err(EnrollmentError::Expired)
        );
    }
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn pairing(expires_at: &str) -> PairingCode {
        PairingCode {
            code: "one-time-secret".into(),
            expires_at: expires_at.into(),
        }
    }

    #[test]
    fn expiry_equality_is_expired_and_fractional_seconds_are_preserved() -> TestResult {
        let expected = pairing("2026-10-29T12:00:00.500Z");
        assert_eq!(
            validate_pairing_at(
                "one-time-secret",
                &expected,
                "2026-10-29T12:00:00.500Z".parse()?
            ),
            Err(EnrollmentError::Expired)
        );
        assert!(
            validate_pairing_at(
                "one-time-secret",
                &expected,
                "2026-10-29T12:00:00.499999999Z".parse()?
            )
            .is_ok()
        );
        assert_eq!(
            validate_pairing_at(
                "one-time-secret",
                &expected,
                "2026-10-29T12:00:00.500000001Z".parse()?
            ),
            Err(EnrollmentError::Expired)
        );
        Ok(())
    }

    #[test]
    fn utc_offsets_compare_by_instant_in_both_directions() -> TestResult {
        let now: Timestamp = "2026-10-29T12:00:00Z".parse()?;
        assert_eq!(
            validate_pairing_at(
                "one-time-secret",
                &pairing("2026-10-29T13:00:00+01:00"),
                now
            ),
            Err(EnrollmentError::Expired)
        );
        assert!(
            validate_pairing_at(
                "one-time-secret",
                &pairing("2026-10-29T11:00:01-01:00"),
                now
            )
            .is_ok()
        );
        assert_eq!(
            validate_pairing_at(
                "one-time-secret",
                &pairing("2026-10-29T13:00:00+02:00"),
                now
            ),
            Err(EnrollmentError::Expired)
        );
        Ok(())
    }

    #[test]
    fn invalid_expiry_and_wrong_code_are_explicit_failures() -> TestResult {
        let now: Timestamp = "2026-10-29T12:00:00Z".parse()?;
        for expiry in [
            "",
            "not-a-timestamp",
            "2026-10-29T13:00:00",
            "2026-02-30T13:00:00Z",
        ] {
            assert_eq!(
                validate_pairing_at("one-time-secret", &pairing(expiry), now),
                Err(EnrollmentError::InvalidExpiry)
            );
        }
        assert_eq!(
            validate_pairing_at("wrong", &pairing("2026-10-29T12:00:01Z"), now),
            Err(EnrollmentError::WrongCode)
        );
        Ok(())
    }

    #[test]
    fn debug_redacts_secret_but_preserves_wire_representation() -> TestResult {
        let expected = pairing("2026-10-29T12:00:01Z");
        let debug = format!("{expected:?}");
        assert!(!debug.contains("one-time-secret"));
        assert!(debug.contains("REDACTED"));
        let wire = serde_json::to_string(&expected)?;
        let restored: PairingCode = serde_json::from_str(&wire)?;
        assert_eq!(restored.code, expected.code);
        assert_eq!(restored.expires_at, expected.expires_at);
        Ok(())
    }
}
