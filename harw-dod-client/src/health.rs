//! The sentinel health report (`health.json`).
//!
//! Written atomically by the sentinel every poll cycle (PL-97 D1):
//!
//! ```json
//! {"schema_version":1,"written_at":"2026-10-28T10:00:00Z","config_digest":"…","profile":"harw-jobs",
//!  "sensors":[{"id":"exec","health":"bound","last_ok_at":"2026-10-28T09:59:58Z","readings_total":12,"errors_total":0}],
//!  "ipc":{"probes_connected":1,"events_total":40,"dropped_total":0}}
//! ```
//!
//! The reader is bounded ([`MAX_HEALTH_BYTES`], at most [`MAX_SENSORS`]
//! sensors), refuses symlinks, tolerates unknown fields and unknown health
//! states (additive evolution), and sanitizes every string it keeps.

use std::io::Read as _;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::DodClientError;
use crate::fsio::{OpenedFile, open_regular_nofollow, sanitize};

/// Largest accepted health report, in bytes (64 KiB).
pub const MAX_HEALTH_BYTES: u64 = 64 * 1024;

/// Most sensors kept from one report.
pub const MAX_SENSORS: usize = 64;

/// Longest kept sensor id / profile / digest, in bytes.
const MAX_LABEL_BYTES: usize = 128;

/// Longest kept degrade reason, in bytes.
const MAX_REASON_BYTES: usize = 256;

/// Health of one sentinel sensor, as written by the sentinel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SensorHealthState {
    /// The sensor delivers readings.
    #[serde(alias = "Bound")]
    Bound,
    /// A transient error; the sentinel retries.
    #[serde(alias = "Retrying")]
    Retrying,
    /// Given up; the sensor is no longer polled.
    #[serde(alias = "Degraded")]
    Degraded,
    /// A state this reader does not know (newer sentinel).
    #[serde(other)]
    Unknown,
}

/// One sensor row of the health report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SensorHealthEntry {
    /// Sensor id.
    pub id: String,
    /// Current health.
    pub health: SensorHealthState,
    /// Why the sensor degraded, if it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degrade_reason: Option<String>,
    /// Last successful reading.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub last_ok_at: Option<OffsetDateTime>,
    /// Readings since sentinel start.
    #[serde(default)]
    pub readings_total: u64,
    /// Errors since sentinel start.
    #[serde(default)]
    pub errors_total: u64,
}

/// Probe IPC counters of the sentinel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IpcCounters {
    /// Probes currently connected.
    #[serde(default)]
    pub probes_connected: u64,
    /// Events received since start.
    #[serde(default)]
    pub events_total: u64,
    /// Events dropped since start.
    #[serde(default)]
    pub dropped_total: u64,
}

/// The parsed, sanitized health report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthReport {
    /// Report schema version (`1`).
    pub schema_version: u32,
    /// When the sentinel wrote the report.
    #[serde(with = "time::serde::rfc3339")]
    pub written_at: OffsetDateTime,
    /// Digest of the configuration the sentinel runs with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_digest: Option<String>,
    /// Active observation profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Sensor rows (at most [`MAX_SENSORS`]).
    #[serde(default)]
    pub sensors: Vec<SensorHealthEntry>,
    /// IPC counters, if reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipc: Option<IpcCounters>,
}

impl HealthReport {
    /// `true` when the report is older than `max_age` at `now` (the
    /// sentinel stopped writing it).
    #[must_use]
    pub fn is_stale(&self, now: OffsetDateTime, max_age: Duration) -> bool {
        time::Duration::try_from(max_age)
            .ok()
            .and_then(|age| now.checked_sub(age))
            .is_some_and(|bound| self.written_at < bound)
    }

    /// Number of sensors in [`SensorHealthState::Degraded`].
    #[must_use]
    pub fn degraded_sensors(&self) -> usize {
        self.sensors
            .iter()
            .filter(|sensor| sensor.health == SensorHealthState::Degraded)
            .count()
    }

    fn sanitized(mut self) -> Self {
        let label = |value: Option<String>| {
            value
                .map(|value| sanitize(&value, MAX_LABEL_BYTES, '?'))
                .filter(|value| !value.is_empty())
        };
        self.config_digest = label(self.config_digest);
        self.profile = label(self.profile);
        self.sensors.truncate(MAX_SENSORS);
        for sensor in &mut self.sensors {
            sensor.id = sanitize(&sensor.id, MAX_LABEL_BYTES, '?');
            sensor.degrade_reason = sensor
                .degrade_reason
                .take()
                .map(|reason| sanitize(&reason, MAX_REASON_BYTES, ' '));
        }
        self
    }
}

/// Reads the sentinel health report.
///
/// Returns `Ok(None)` when the file does not exist.
///
/// # Errors
/// [`DodClientError::Symlink`] / [`DodClientError::NotRegularFile`] when
/// the path is refused,
/// [`DodClientError::TooLarge`] beyond [`MAX_HEALTH_BYTES`],
/// [`DodClientError::Parse`] when the content does not match the schema and
/// [`DodClientError::Io`] for read failures.
pub fn read_health(path: &Path) -> Result<Option<HealthReport>, DodClientError> {
    let Some(OpenedFile { file, size, .. }) = open_regular_nofollow(path)? else {
        return Ok(None);
    };
    let too_large = || DodClientError::TooLarge {
        path: path.to_path_buf(),
        limit: MAX_HEALTH_BYTES,
    };
    if size > MAX_HEALTH_BYTES {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    file.take(MAX_HEALTH_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| DodClientError::io(path, error))?;
    if bytes.len() as u64 > MAX_HEALTH_BYTES {
        return Err(too_large());
    }
    let report: HealthReport =
        serde_json::from_slice(&bytes).map_err(|error| DodClientError::Parse {
            path: path.to_path_buf(),
            reason: sanitize(&error.to_string(), MAX_REASON_BYTES, ' '),
        })?;
    if report.schema_version == 0 {
        return Err(DodClientError::Parse {
            path: path.to_path_buf(),
            reason: "unsupported schema_version 0".to_owned(),
        });
    }
    Ok(Some(report.sanitized()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use time::format_description::well_known::Rfc3339;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const SAMPLE: &str = r#"{
        "schema_version": 1,
        "written_at": "2026-10-28T10:00:00Z",
        "config_digest": "ab12",
        "profile": "harw-jobs",
        "sensors": [
            {"id": "exec", "health": "bound", "last_ok_at": "2026-10-28T09:59:58Z", "readings_total": 12, "errors_total": 0},
            {"id": "cgroup\u001b", "health": "degraded", "degrade_reason": "retry budget\nexhausted", "readings_total": 3, "errors_total": 9},
            {"id": "tcp", "health": "hibernating"}
        ],
        "ipc": {"probes_connected": 1, "events_total": 40, "dropped_total": 2},
        "added_later": {"ignored": true}
    }"#;

    #[test]
    fn parses_sanitizes_and_tolerates_unknown_states() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("health.json");
        fs::write(&path, SAMPLE)?;
        let report = read_health(&path)?.ok_or("report expected")?;
        assert_eq!(report.schema_version, 1);
        assert_eq!(report.profile.as_deref(), Some("harw-jobs"));
        assert_eq!(report.sensors.len(), 3);
        assert_eq!(report.sensors[0].health, SensorHealthState::Bound);
        assert_eq!(
            report.sensors[0].last_ok_at,
            Some(OffsetDateTime::parse("2026-10-28T09:59:58Z", &Rfc3339)?)
        );
        assert_eq!(report.sensors[1].id, "cgroup?");
        assert_eq!(
            report.sensors[1].degrade_reason.as_deref(),
            Some("retry budget exhausted")
        );
        assert_eq!(report.sensors[2].health, SensorHealthState::Unknown);
        assert_eq!(report.sensors[2].readings_total, 0);
        assert_eq!(report.degraded_sensors(), 1);
        assert_eq!(report.ipc.map(|ipc| ipc.dropped_total), Some(2));

        let now = OffsetDateTime::parse("2026-10-28T10:00:30Z", &Rfc3339)?;
        assert!(!report.is_stale(now, Duration::from_secs(60)));
        assert!(report.is_stale(now, Duration::from_secs(10)));

        let json = serde_json::to_string(&report)?;
        let again: HealthReport = serde_json::from_str(&json)?;
        assert_eq!(again, report);
        Ok(())
    }

    #[test]
    fn missing_is_none_and_hostile_files_are_refused() -> TestResult {
        let dir = tempfile::tempdir()?;
        assert!(read_health(&dir.path().join("health.json"))?.is_none());

        let target = dir.path().join("real.json");
        fs::write(&target, SAMPLE)?;
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&target, &link)?;
        assert!(matches!(
            read_health(&link),
            Err(DodClientError::Symlink { .. })
        ));

        let big = dir.path().join("big.json");
        fs::write(&big, vec![b' '; usize::try_from(MAX_HEALTH_BYTES)? + 1])?;
        assert!(matches!(
            read_health(&big),
            Err(DodClientError::TooLarge { .. })
        ));

        let broken = dir.path().join("broken.json");
        fs::write(&broken, "{\"schema_version\": 1}")?;
        assert!(matches!(
            read_health(&broken),
            Err(DodClientError::Parse { .. })
        ));
        let zero = dir.path().join("zero.json");
        fs::write(
            &zero,
            r#"{"schema_version":0,"written_at":"2026-10-28T10:00:00Z"}"#,
        )?;
        assert!(matches!(
            read_health(&zero),
            Err(DodClientError::Parse { .. })
        ));
        Ok(())
    }

    #[test]
    fn sensor_list_is_capped() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("health.json");
        let sensors: Vec<String> = (0..MAX_SENSORS + 5)
            .map(|i| format!(r#"{{"id":"s{i}","health":"bound"}}"#))
            .collect();
        fs::write(
            &path,
            format!(
                r#"{{"schema_version":1,"written_at":"2026-10-28T10:00:00Z","sensors":[{}]}}"#,
                sensors.join(",")
            ),
        )?;
        let report = read_health(&path)?.ok_or("report expected")?;
        assert_eq!(report.sensors.len(), MAX_SENSORS);
        Ok(())
    }
}
