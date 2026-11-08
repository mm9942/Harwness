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
//!
//! Since PL-100 WP0.7 the sentinel also reports its Landlock
//! self-restriction (additive object, schema version unchanged):
//!
//! ```json
//! "landlock": {"outcome": "partially_enforced", "requested_abi": 1, "effective_abi": 3}
//! ```
//!
//! [`HealthReport::landlock`] is `None` for a sentinel that predates the
//! object (or writes `null`); an outcome this reader does not know reads as
//! [`LandlockOutcome::Unknown`].

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
    /// IPC peers refused by the sentinel's `SO_PEERCRED` allowlist since
    /// start (PL-100 WP0.5); `0` for a sentinel that predates the field.
    #[serde(default)]
    pub peers_rejected_total: u64,
    /// Messages of admitted probes the sentinel discarded since start
    /// because they were malformed or oversized (PL-100 WP1); `0` for a
    /// sentinel that predates the field. Expected: `0`.
    #[serde(default)]
    pub decode_errors_total: u64,
}

/// Outcome of the sentinel's Landlock self-restriction (PL-100 WP0.7).
///
/// The default is [`Self::Unknown`]: a `landlock` object without an
/// `outcome` (or with `"outcome": null`) reads as unknown instead of
/// failing the whole report.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandlockOutcome {
    /// Every requested rule is enforced by the kernel.
    FullyEnforced,
    /// Only part of the requested rules is enforced (older kernel ABI).
    PartiallyEnforced,
    /// The kernel does not enforce Landlock; the sentinel runs without its
    /// filesystem sandbox.
    NotEnforced,
    /// Building or applying the ruleset failed; the sentinel runs without
    /// its filesystem sandbox.
    Failed,
    /// An outcome this reader does not know (newer sentinel), or none was
    /// reported.
    #[default]
    #[serde(other)]
    Unknown,
}

impl LandlockOutcome {
    /// `true` when the sentinel runs **without** its filesystem sandbox
    /// ([`Self::NotEnforced`], [`Self::Failed`]). [`Self::Unknown`] is not
    /// counted as degraded, since its meaning is unknown to this reader.
    #[must_use]
    pub const fn is_degraded(self) -> bool {
        matches!(self, Self::NotEnforced | Self::Failed)
    }
}

/// The sentinel's Landlock self-restriction as reported in `health.json`
/// (`landlock` object, PL-100 WP0.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandlockHealth {
    /// What the kernel enforced; [`LandlockOutcome::Unknown`] when the
    /// object lacks the field or carries `null` (additive tolerance: one
    /// malformed optional object must not cost the sensor rows).
    #[serde(default, deserialize_with = "outcome_or_unknown")]
    pub outcome: LandlockOutcome,
    /// ABI level whose access rights the sentinel requested; `None` if the
    /// sentinel did not report it.
    #[serde(default)]
    pub requested_abi: Option<u32>,
    /// ABI level the running kernel supports; `None` without Landlock
    /// support or when the attempt failed early.
    #[serde(default)]
    pub effective_abi: Option<u32>,
}

/// Reads `landlock.outcome`, mapping `null` to [`LandlockOutcome::Unknown`]
/// (a missing field is handled by `#[serde(default)]`).
fn outcome_or_unknown<'de, D>(deserializer: D) -> Result<LandlockOutcome, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<LandlockOutcome>::deserialize(deserializer)?.unwrap_or_default())
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
    /// The sentinel's Landlock self-restriction (PL-100 WP0.7); `None` for
    /// a sentinel that predates the field or did not report it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub landlock: Option<LandlockHealth>,
}

impl HealthReport {
    /// `true` when the report is older than `max_age` at `now`, or its
    /// timestamp is more than five seconds in the future. The bounded skew
    /// allowance tolerates small clock adjustments without trusting future data.
    #[must_use]
    pub fn is_stale(&self, now: OffsetDateTime, max_age: Duration) -> bool {
        let age = now - self.written_at;
        age < -time::Duration::seconds(5) || (age.is_positive() && age.unsigned_abs() > max_age)
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
    fn reads_rejected_peers_and_ignores_the_windowed_sentinel_objects() -> TestResult {
        // PL-100 WP0.5/WP0.12/WP0.13: newer sentinels add `peers_rejected_total`
        // and the `ring`, `windows` and `spool` objects (additive, schema 1).
        let newer = r#"{
            "schema_version": 1,
            "written_at": "2026-10-28T10:00:00Z",
            "ipc": {"probes_connected": 2, "events_total": 7, "dropped_total": 1,
                    "peers_rejected_total": 3, "events_last_cycle": 1},
            "ring": {"overflow_events_total": 4},
            "windows": {"last_cycle_ms": 5000, "last_60s_ms": null},
            "spool": {"state": "ok"}
        }"#;
        let report: HealthReport = serde_json::from_str(newer)?;
        assert_eq!(report.ipc.map(|ipc| ipc.peers_rejected_total), Some(3));
        assert_eq!(report.ipc.map(|ipc| ipc.events_total), Some(7));
        let older: HealthReport = serde_json::from_str(SAMPLE)?;
        assert_eq!(older.ipc.map(|ipc| ipc.peers_rejected_total), Some(0));
        Ok(())
    }

    /// PL-100 WP1: the sentinel reports discarded probe messages as
    /// `ipc.decode_errors_total`; older files without the field read as `0`.
    #[test]
    fn reads_the_ipc_decode_error_total_and_defaults_it() -> TestResult {
        let newer = r#"{
            "schema_version": 1,
            "written_at": "2026-10-28T10:00:00Z",
            "ipc": {"probes_connected": 1, "events_total": 9, "dropped_total": 0,
                    "peers_rejected_total": 0, "decode_errors_total": 4}
        }"#;
        let report: HealthReport = serde_json::from_str(newer)?;
        assert_eq!(report.ipc.map(|ipc| ipc.decode_errors_total), Some(4));
        let older: HealthReport = serde_json::from_str(SAMPLE)?;
        assert_eq!(older.ipc.map(|ipc| ipc.decode_errors_total), Some(0));
        let json = serde_json::to_string(&report)?;
        let again: HealthReport = serde_json::from_str(&json)?;
        assert_eq!(again.ipc.map(|ipc| ipc.decode_errors_total), Some(4));
        Ok(())
    }

    /// PL-100 WP0.7: the Landlock object is read with its outcome and ABI
    /// levels; older files without it and unknown outcomes stay readable.
    #[test]
    fn reads_the_landlock_outcome_and_tolerates_its_absence() -> TestResult {
        let newer = r#"{
            "schema_version": 1,
            "written_at": "2026-10-28T10:00:00Z",
            "landlock": {"outcome": "partially_enforced", "requested_abi": 1, "effective_abi": 3}
        }"#;
        let report: HealthReport = serde_json::from_str(newer)?;
        let landlock = report.landlock.ok_or("landlock expected")?;
        assert_eq!(landlock.outcome, LandlockOutcome::PartiallyEnforced);
        assert_eq!(landlock.requested_abi, Some(1));
        assert_eq!(landlock.effective_abi, Some(3));
        assert!(!landlock.outcome.is_degraded());

        let degraded = r#"{
            "schema_version": 1,
            "written_at": "2026-10-28T10:00:00Z",
            "landlock": {"outcome": "not_enforced", "requested_abi": 1, "effective_abi": null}
        }"#;
        let report: HealthReport = serde_json::from_str(degraded)?;
        let landlock = report.landlock.ok_or("landlock expected")?;
        assert!(landlock.outcome.is_degraded());
        assert_eq!(landlock.effective_abi, None);

        let future = r#"{
            "schema_version": 1,
            "written_at": "2026-10-28T10:00:00Z",
            "landlock": {"outcome": "enforced_with_audit"}
        }"#;
        let report: HealthReport = serde_json::from_str(future)?;
        let landlock = report.landlock.ok_or("landlock expected")?;
        assert_eq!(landlock.outcome, LandlockOutcome::Unknown);
        assert!(!landlock.outcome.is_degraded());
        assert_eq!(landlock.requested_abi, None);

        // A `landlock` object without an outcome, or with a null one, reads
        // as unknown instead of failing the whole report.
        for partial in [
            r#"{"schema_version": 1, "written_at": "2026-10-28T10:00:00Z", "landlock": {}}"#,
            r#"{"schema_version": 1, "written_at": "2026-10-28T10:00:00Z",
                "landlock": {"outcome": null, "requested_abi": 2}}"#,
        ] {
            let report: HealthReport = serde_json::from_str(partial)?;
            let landlock = report.landlock.ok_or("landlock expected")?;
            assert_eq!(landlock.outcome, LandlockOutcome::Unknown);
            assert!(!landlock.outcome.is_degraded());
        }
        assert_eq!(LandlockOutcome::default(), LandlockOutcome::Unknown);

        let explicit_null =
            r#"{"schema_version": 1, "written_at": "2026-10-28T10:00:00Z", "landlock": null}"#;
        let report: HealthReport = serde_json::from_str(explicit_null)?;
        assert_eq!(report.landlock, None);
        let older: HealthReport = serde_json::from_str(SAMPLE)?;
        assert_eq!(older.landlock, None);

        let json = serde_json::to_string(&serde_json::from_str::<HealthReport>(newer)?)?;
        let again: HealthReport = serde_json::from_str(&json)?;
        assert_eq!(
            again.landlock.map(|landlock| landlock.outcome),
            Some(LandlockOutcome::PartiallyEnforced)
        );
        Ok(())
    }

    #[test]
    fn health_freshness_bounds_past_age_and_future_clock_skew() -> TestResult {
        let report: HealthReport = serde_json::from_str(SAMPLE)?;
        let max_age = Duration::from_secs(60);
        for (offset_seconds, stale) in [
            (-3600, true),
            (-6, true),
            (-5, false),
            (0, false),
            (60, false),
            (61, true),
        ] {
            let now = report.written_at + time::Duration::seconds(offset_seconds);
            assert_eq!(
                report.is_stale(now, max_age),
                stale,
                "offset {offset_seconds}"
            );
        }
        // An enormous age allowance must still reject future-dated reports.
        assert!(report.is_stale(report.written_at - time::Duration::hours(1), Duration::MAX));
        assert!(!report.is_stale(report.written_at + time::Duration::hours(1), Duration::MAX));
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
