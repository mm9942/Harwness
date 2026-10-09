//! Device enrollment types for the Harwness cloud home (PL-65/68/94).
//!
//! This crate models the enrollment record kept per device, the short-lived
//! pairing code exchanged during enrollment, and the validation of pairing
//! codes.

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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingCode {
    /// The pairing code string presented to the device owner.
    pub code: String,
    /// ISO-8601 timestamp after which the code is no longer valid.
    pub expires_at: String,
}

/// Errors that can occur during pairing validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrollmentError {
    /// The presented code does not match the expected one.
    WrongCode,
    /// The expected code has expired.
    Expired,
}

impl fmt::Display for EnrollmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EnrollmentError::WrongCode => write!(f, "pairing code does not match"),
            EnrollmentError::Expired => write!(f, "pairing code has expired"),
        }
    }
}

impl std::error::Error for EnrollmentError {}

/// Validate a presented pairing code against the expected one.
///
/// Expiry is determined by comparing the expected `expires_at` timestamp
/// (ISO-8601) lexicographically with the current UTC time, which is
/// sufficient for consistently formatted timestamps.
pub fn validate_pairing(code: &str, expected: &PairingCode) -> Result<(), EnrollmentError> {
    let now = iso8601_now_utc();
    if expected.expires_at < now {
        return Err(EnrollmentError::Expired);
    }
    if !constant_time_eq(code.as_bytes(), expected.code.as_bytes()) {
        return Err(EnrollmentError::WrongCode);
    }
    Ok(())
}

/// Current UTC time formatted as ISO-8601 (`YYYY-MM-DDTHH:MM:SSZ`).
fn iso8601_now_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    iso8601_from_unix(secs)
}

/// Convert a UNIX timestamp to ISO-8601 UTC (proleptic Gregorian, no leap seconds).
fn iso8601_from_unix(secs: u64) -> String {
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, m, d) = civil_from_days(days as i64);
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Days since 1970-01-01 to (year, month, day); Howard Hinnant's algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// Length-checking, timing-attack-resistant byte comparison.
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
}
