//! Optional health export (`--health-export`): one small JSON document that
//! describes the sentinel's own state, rewritten atomically every poll cycle.
//!
//! # Purpose
//! The findings export (`crate::export`) says *what* the rules found; it
//! cannot say whether the sentinel is still observing anything. This module
//! writes the complementary snapshot that harw (`harw-dod-client`,
//! `harw dod sensors`) reads: per-sensor health, the configuration the
//! sentinel resolved, and the IPC counters of the probe receive path. It
//! exists **only** when `--health-export <ABS-PATH>` is given or, as the
//! default, when `--findings-export` is set (sibling file
//! [`DEFAULT_HEALTH_FILE_NAME`]); without either flag no [`HealthExporter`]
//! exists and nothing is written.
//!
//! # Document (schema version 1)
//! ```json
//! {
//!   "schema_version": 1,
//!   "written_at": "2026-10-28T10:00:00Z",
//!   "config_digest": "3f…",
//!   "profile": "host",
//!   "sensors": [
//!     {"id": "cpu-0", "health": "bound", "last_ok_at": "2026-10-28T10:00:00Z",
//!      "readings_total": 12, "errors_total": 0},
//!     {"id": "gpu-0", "health": "degraded", "degrade_reason": "permanent",
//!      "readings_total": 0, "errors_total": 1}
//!   ],
//!   "ipc": {"probes_connected": 1, "events_total": 40, "dropped_total": 0}
//! }
//! ```
//!
//! - `config_digest`/`profile` are `null` when the sentinel runs on built-in
//!   defaults (no system configuration, see `crate::main`).
//! - `health` is one of [`health_label`]'s values (`bound`, `retrying`,
//!   `degraded`); `degrade_reason` (`permanent`, `retries_exhausted`) is
//!   present only for `degraded`; `last_ok_at` only after a successful poll.
//! - `readings_total`/`errors_total` count successful and failed polls of
//!   that sensor since process start ([`SensorTracker`]).
//! - The document carries no evidence, no raw events, no paths, no secrets.
//!
//! # Atomic replacement
//! Every write goes to a temporary sibling (`.<name>.tmp`, created with
//! `O_CREAT | O_EXCL`, so a pre-planted file or symlink is never followed),
//! is flushed with `sync_data`, and is then renamed over the target in the
//! same directory: a reader sees either the previous or the new document,
//! never a partial one. A target that exists but is not a regular file
//! (symlink, directory) is refused. New files get mode
//! [`HEALTH_FILE_MODE`] (`0640`), matching the findings export.
//!
//! # Errors
//! A failed write never stops the sentinel: it is counted
//! ([`HealthExporter::failures`], metric [`HEALTH_EXPORT_FAILURE_TOTAL`]),
//! logged with `tracing::warn!` once when the export starts failing (repeats
//! go to `debug`, so a missing directory does not flood the journal every
//! cycle), and retried on the next cycle.
//!
//! # Concurrency
//! Lives exclusively in the collection thread (`&mut self`), like
//! `crate::export::FindingsExporter`.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use harw_dod_sentinel::{DegradeReason, SensorHealth};
use harw_observe::{MetricValue, TelemetrySink};
use harw_types::SensorId;
use jiff::Timestamp;
use serde::Serialize;

/// Schema version of the health document (`schema_version`).
pub const HEALTH_SCHEMA_VERSION: u32 = 1;

/// File name of the default health export next to `--findings-export`.
pub const DEFAULT_HEALTH_FILE_NAME: &str = "health.json";

/// Mode of a newly created health document: owner read/write, group read
/// (readers get access through the export directory's group), nothing else.
pub const HEALTH_FILE_MODE: u32 = 0o640;

harw_macros::metrics! {
    /// How often the health document could not be written since process
    /// start (encode, temporary file, flush or rename). Expected: `0`.
    HEALTH_EXPORT_FAILURE_TOTAL: counter, unit = count, labels = [], cardinality = single,
        name = "harw_sentinel_health_export_failure_total";
}

/// Counters of the probe receive path (`ipc` object of the document).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct IpcCounters {
    /// Probe connections currently open.
    pub probes_connected: u64,
    /// Events received from probes since start (including evicted ones).
    pub events_total: u64,
    /// Events evicted from the full receive inbox since start.
    pub dropped_total: u64,
}

/// Stable string form of a [`SensorHealth`] state.
///
/// Exhaustive `match` without a wildcard: a new state breaks the build
/// instead of being exported under a wrong name.
#[must_use]
pub const fn health_label(health: &SensorHealth) -> &'static str {
    match health {
        SensorHealth::Bound => "bound",
        SensorHealth::Retrying { .. } => "retrying",
        SensorHealth::Degraded { .. } => "degraded",
    }
}

/// Stable string form of a [`DegradeReason`].
#[must_use]
pub const fn degrade_reason_label(reason: DegradeReason) -> &'static str {
    match reason {
        DegradeReason::Permanent => "permanent",
        DegradeReason::RetriesExhausted { .. } => "retries_exhausted",
    }
}

/// Per-sensor poll counters accumulated by [`SensorTracker`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SensorCounters {
    readings_total: u64,
    errors_total: u64,
    last_ok_at: Option<Timestamp>,
}

/// Derives per-sensor success/failure counts from two health snapshots.
///
/// # Description
/// `harw_dod_sentinel::Sentinel` keeps only the current [`SensorHealth`] of
/// each sensor, not how often it was polled. This tracker reconstructs that
/// exactly from `Sentinel::health_snapshot` taken immediately before and
/// after `Sentinel::poll_all(now)`: a sensor was polled in that cycle iff its
/// previous state [`SensorHealth::is_due`] at `now` (the very check
/// `poll_all` makes), and the poll succeeded iff the new state is
/// [`SensorHealth::Bound`] (`SensorHealth::advance` maps `Ok` to `Bound` and
/// every `Err` to `Retrying`/`Degraded`). Snapshots are positional
/// (registration order, fixed after `Sentinel::new`).
#[derive(Debug, Default)]
pub struct SensorTracker {
    counters: Vec<SensorCounters>,
}

impl SensorTracker {
    /// Accounts one poll cycle.
    ///
    /// # Arguments
    /// - `before`: `health_snapshot()` taken right before `poll_all(now)`.
    /// - `after`: `health_snapshot()` taken right after it.
    /// - `now`: the timestamp passed to `poll_all`.
    pub fn observe(
        &mut self,
        before: &[(SensorId, SensorHealth)],
        after: &[(SensorId, SensorHealth)],
        now: Timestamp,
    ) {
        if self.counters.len() < after.len() {
            self.counters
                .resize_with(after.len(), SensorCounters::default);
        }
        for ((_, previous), (counters, (_, next))) in before
            .iter()
            .zip(self.counters.iter_mut().zip(after.iter()))
        {
            if !previous.is_due(now) {
                continue;
            }
            if matches!(next, SensorHealth::Bound) {
                counters.readings_total = counters.readings_total.saturating_add(1);
                counters.last_ok_at = Some(now);
            } else {
                counters.errors_total = counters.errors_total.saturating_add(1);
            }
        }
    }

    fn counters(&self, index: usize) -> SensorCounters {
        self.counters.get(index).cloned().unwrap_or_default()
    }
}

/// One entry of the `sensors` array.
#[derive(Debug, Serialize)]
struct SensorLine<'a> {
    id: &'a str,
    health: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    degrade_reason: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_ok_at: Option<Timestamp>,
    readings_total: u64,
    errors_total: u64,
}

/// The whole document.
#[derive(Debug, Serialize)]
struct HealthDocument<'a> {
    schema_version: u32,
    written_at: Timestamp,
    config_digest: Option<&'a str>,
    profile: Option<&'a str>,
    sensors: Vec<SensorLine<'a>>,
    ipc: IpcCounters,
}

/// Identity of the resolved DoD configuration, as reported in the document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigIdentity {
    /// Hex BLAKE3 digest of the configuration bytes, or `None` on built-in
    /// defaults.
    pub config_digest: Option<String>,
    /// Id of the active observation profile, or `None` on built-in
    /// defaults.
    pub profile: Option<String>,
}

/// Encodes the health document (pretty-printed, trailing newline).
///
/// # Errors
/// `serde_json::Error` if serialization fails (practically unreachable for
/// these field types, but not treated as a panic).
pub fn encode_document(
    identity: &ConfigIdentity,
    snapshot: &[(SensorId, SensorHealth)],
    tracker: &SensorTracker,
    ipc: IpcCounters,
    now: Timestamp,
) -> Result<String, serde_json::Error> {
    let sensors = snapshot
        .iter()
        .enumerate()
        .map(|(index, (id, health))| {
            let counters = tracker.counters(index);
            let degrade_reason = match health {
                SensorHealth::Degraded { reason } => Some(degrade_reason_label(*reason)),
                SensorHealth::Bound | SensorHealth::Retrying { .. } => None,
            };
            SensorLine {
                id: id.as_str(),
                health: health_label(health),
                degrade_reason,
                last_ok_at: counters.last_ok_at,
                readings_total: counters.readings_total,
                errors_total: counters.errors_total,
            }
        })
        .collect();
    let document = HealthDocument {
        schema_version: HEALTH_SCHEMA_VERSION,
        written_at: now,
        config_digest: identity.config_digest.as_deref(),
        profile: identity.profile.as_deref(),
        sensors,
        ipc,
    };
    let mut encoded = serde_json::to_string_pretty(&document)?;
    encoded.push('\n');
    Ok(encoded)
}

/// Default health export path for a findings export path: the sibling file
/// [`DEFAULT_HEALTH_FILE_NAME`] in the same directory.
#[must_use]
pub fn default_path_for(findings_export: &Path) -> Option<PathBuf> {
    findings_export
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.join(DEFAULT_HEALTH_FILE_NAME))
}

/// Temporary sibling `.<name>.tmp` used for the atomic replacement.
fn temporary_path(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?;
    let mut temporary = OsString::from(".");
    temporary.push(name);
    temporary.push(".tmp");
    Some(path.with_file_name(temporary))
}

/// Writes `bytes` to `path` atomically (temporary sibling, flush, rename).
fn write_atomically(path: &Path, temporary: &Path, bytes: &[u8]) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.file_type().is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "health export path exists but is not a regular file",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    // Ein Rest aus einem abgebrochenen Lauf: entfernen (entfernt bei einem
    // Symlink nur den Link selbst, nie sein Ziel).
    match fs::remove_file(temporary) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(HEALTH_FILE_MODE);
    }
    let written = options.open(temporary).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_data()
    });
    let renamed = written.and_then(|()| fs::rename(temporary, path));
    if renamed.is_err() {
        // Best effort: keinen halben Rest liegen lassen.
        let _ = fs::remove_file(temporary);
    }
    renamed
}

/// Atomic writer of the health document.
///
/// # Description
/// Holds the target path, the configuration identity reported in every
/// document, the [`SensorTracker`], and counters for written and failed
/// documents. See the module documentation for format and failure handling.
#[derive(Debug)]
pub struct HealthExporter {
    path: PathBuf,
    identity: ConfigIdentity,
    tracker: SensorTracker,
    written: u64,
    failures: u64,
    /// Whether the previous write failed (rate-limits the warning).
    failing: bool,
}

impl HealthExporter {
    /// Builds an exporter for `path`. Writes nothing yet.
    #[must_use]
    pub fn new(path: PathBuf, identity: ConfigIdentity) -> Self {
        Self {
            path,
            identity,
            tracker: SensorTracker::default(),
            written: 0,
            failures: 0,
            failing: false,
        }
    }

    /// Target path of the health document.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Directory of the document — must stay writable for the temporary
    /// file and the rename (Landlock rule in `crate::run`).
    #[must_use]
    pub fn directory(&self) -> Option<&Path> {
        self.path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
    }

    /// Number of documents written successfully since process start.
    #[must_use]
    pub const fn written(&self) -> u64 {
        self.written
    }

    /// Number of failed writes since process start.
    #[must_use]
    pub const fn failures(&self) -> u64 {
        self.failures
    }

    /// Accounts one poll cycle and rewrites the document.
    ///
    /// # Arguments
    /// - `sink`: receives [`HEALTH_EXPORT_FAILURE_TOTAL`] on failure.
    /// - `before`/`after`: health snapshots around `poll_all(now)` (see
    ///   [`SensorTracker::observe`]); `after` is also what the document
    ///   reports.
    /// - `ipc`: current receive-path counters.
    /// - `now`: the cycle's timestamp, reported as `written_at`.
    ///
    /// # Returns
    /// `true` if the document was replaced; `false` after a counted and
    /// logged failure.
    pub fn record_cycle(
        &mut self,
        sink: &dyn TelemetrySink,
        before: &[(SensorId, SensorHealth)],
        after: &[(SensorId, SensorHealth)],
        ipc: IpcCounters,
        now: Timestamp,
    ) -> bool {
        self.tracker.observe(before, after, now);
        let result = encode_document(&self.identity, after, &self.tracker, ipc, now)
            .map_err(io::Error::other)
            .and_then(|encoded| {
                let temporary = temporary_path(&self.path).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "health export path has no file name",
                    )
                })?;
                write_atomically(&self.path, &temporary, encoded.as_bytes())
            });
        match result {
            Ok(()) => {
                self.written = self.written.saturating_add(1);
                if self.failing {
                    tracing::info!(path = %self.path.display(), "health export recovered");
                }
                self.failing = false;
                true
            }
            Err(error) => {
                self.record_failure(sink, &error);
                false
            }
        }
    }

    fn record_failure(&mut self, sink: &dyn TelemetrySink, error: &io::Error) {
        self.failures = self.failures.saturating_add(1);
        sink.record(&HEALTH_EXPORT_FAILURE_TOTAL, MetricValue::Count(1), &[]);
        if self.failing {
            tracing::debug!(
                error = %error,
                path = %self.path.display(),
                failures = self.failures,
                "health export still failing"
            );
        } else {
            tracing::warn!(
                error = %error,
                path = %self.path.display(),
                failures = self.failures,
                "health export failed; retrying every poll cycle"
            );
        }
        self.failing = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_observe::NullSink;
    use jiff::SignedDuration;
    use serde::Deserialize;

    /// Strict mirror of the document: exactly these fields, nothing more.
    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Document {
        schema_version: u32,
        written_at: Timestamp,
        config_digest: Option<String>,
        profile: Option<String>,
        sensors: Vec<Sensor>,
        ipc: Ipc,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Sensor {
        id: String,
        health: String,
        #[serde(default)]
        degrade_reason: Option<String>,
        #[serde(default)]
        last_ok_at: Option<Timestamp>,
        readings_total: u64,
        errors_total: u64,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Ipc {
        probes_connected: u64,
        events_total: u64,
        dropped_total: u64,
    }

    fn at(secs: i64) -> TestResult<Timestamp> {
        Timestamp::from_second(secs).map_err(ctx("timestamp"))
    }

    fn id(name: &str) -> SensorId {
        SensorId::from_str(name)
    }

    fn identity() -> ConfigIdentity {
        ConfigIdentity {
            config_digest: Some("ab".repeat(32)),
            profile: Some("host".to_owned()),
        }
    }

    fn read_document(path: &Path) -> TestResult<Document> {
        let text = fs::read_to_string(path).map_err(ctx("read health export"))?;
        serde_json::from_str(&text).map_err(ctx("parse health export"))
    }

    #[test]
    fn test_health_export_round_trips_through_a_tempdir() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join(DEFAULT_HEALTH_FILE_NAME);
        let mut exporter = HealthExporter::new(path.clone(), identity());
        let now = at(1_800_000_000)?;
        let before = vec![
            (id("cpu-0"), SensorHealth::Bound),
            (id("gpu-0"), SensorHealth::Bound),
        ];
        let after = vec![
            (id("cpu-0"), SensorHealth::Bound),
            (
                id("gpu-0"),
                SensorHealth::Degraded {
                    reason: DegradeReason::Permanent,
                },
            ),
        ];
        let ipc = IpcCounters {
            probes_connected: 1,
            events_total: 40,
            dropped_total: 2,
        };
        assert!(exporter.record_cycle(&NullSink, &before, &after, ipc, now));
        assert_eq!(exporter.written(), 1);
        assert_eq!(exporter.failures(), 0);

        let document = read_document(&path)?;
        assert_eq!(document.schema_version, HEALTH_SCHEMA_VERSION);
        assert_eq!(document.written_at, now);
        assert_eq!(document.config_digest, Some("ab".repeat(32)));
        assert_eq!(document.profile.as_deref(), Some("host"));
        assert_eq!(document.sensors.len(), 2);
        let cpu = &document.sensors[0];
        assert_eq!((cpu.id.as_str(), cpu.health.as_str()), ("cpu-0", "bound"));
        assert_eq!(cpu.degrade_reason, None);
        assert_eq!(cpu.last_ok_at, Some(now));
        assert_eq!((cpu.readings_total, cpu.errors_total), (1, 0));
        let gpu = &document.sensors[1];
        assert_eq!(gpu.health, "degraded");
        assert_eq!(gpu.degrade_reason.as_deref(), Some("permanent"));
        assert_eq!(gpu.last_ok_at, None);
        assert_eq!((gpu.readings_total, gpu.errors_total), (0, 1));
        assert_eq!(document.ipc.probes_connected, 1);
        assert_eq!(document.ipc.events_total, 40);
        assert_eq!(document.ipc.dropped_total, 2);
        Ok(())
    }

    #[test]
    fn test_builtin_defaults_report_null_profile_and_digest() -> TestResult {
        let encoded = encode_document(
            &ConfigIdentity::default(),
            &[],
            &SensorTracker::default(),
            IpcCounters::default(),
            Timestamp::UNIX_EPOCH,
        )
        .map_err(ctx("encode"))?;
        let value: serde_json::Value = serde_json::from_str(&encoded).map_err(ctx("parse"))?;
        assert!(value["config_digest"].is_null());
        assert!(value["profile"].is_null());
        assert_eq!(value["sensors"], serde_json::json!([]));
        assert!(encoded.ends_with('\n'));
        Ok(())
    }

    #[test]
    fn test_atomic_write_replaces_the_document_and_leaves_no_temporary() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("health.json");
        let mut exporter = HealthExporter::new(path.clone(), ConfigIdentity::default());
        let snapshot = vec![(id("cpu-0"), SensorHealth::Bound)];
        for second in 1..=3 {
            assert!(exporter.record_cycle(
                &NullSink,
                &snapshot,
                &snapshot,
                IpcCounters::default(),
                at(second)?,
            ));
        }
        let document = read_document(&path)?;
        assert_eq!(document.written_at, at(3)?);
        assert_eq!(document.sensors[0].readings_total, 3);
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .map_err(ctx("read dir"))?
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .filter(|name| name != "health.json")
            .collect();
        assert!(leftovers.is_empty(), "unexpected files: {leftovers:?}");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_health_document_is_created_with_mode_0640() -> TestResult {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("health.json");
        let mut exporter = HealthExporter::new(path.clone(), ConfigIdentity::default());
        assert!(exporter.record_cycle(
            &NullSink,
            &[],
            &[],
            IpcCounters::default(),
            Timestamp::UNIX_EPOCH
        ));
        let mode = fs::metadata(&path)
            .map_err(ctx("metadata"))?
            .permissions()
            .mode()
            & 0o777;
        // Die Prozess-umask kann höchstens Bits entfernen, nie hinzufügen.
        assert_eq!(mode & !HEALTH_FILE_MODE, 0, "mode {mode:o} exceeds 0640");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_symlink_at_the_target_is_refused_and_its_target_untouched() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let victim = dir.path().join("victim");
        fs::write(&victim, b"keep").map_err(ctx("seed victim"))?;
        let path = dir.path().join("health.json");
        std::os::unix::fs::symlink(&victim, &path).map_err(ctx("symlink"))?;
        let mut exporter = HealthExporter::new(path.clone(), ConfigIdentity::default());
        let ok = exporter.record_cycle(
            &NullSink,
            &[],
            &[],
            IpcCounters::default(),
            Timestamp::UNIX_EPOCH,
        );
        assert!(!ok);
        assert_eq!(exporter.failures(), 1);
        assert_eq!(fs::read(&victim).map_err(ctx("read victim"))?, b"keep");
        assert!(
            fs::symlink_metadata(&path)
                .map_err(ctx("lstat"))?
                .file_type()
                .is_symlink()
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_a_planted_temporary_symlink_is_removed_not_followed() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let victim = dir.path().join("victim");
        fs::write(&victim, b"keep").map_err(ctx("seed victim"))?;
        let path = dir.path().join("health.json");
        let temporary = temporary_path(&path).ok_or(TestError::Missing("temporary path"))?;
        std::os::unix::fs::symlink(&victim, &temporary).map_err(ctx("symlink"))?;
        let mut exporter = HealthExporter::new(path.clone(), ConfigIdentity::default());
        assert!(exporter.record_cycle(
            &NullSink,
            &[],
            &[],
            IpcCounters::default(),
            Timestamp::UNIX_EPOCH
        ));
        assert_eq!(fs::read(&victim).map_err(ctx("read victim"))?, b"keep");
        let document = read_document(&path)?;
        assert_eq!(document.schema_version, 1);
        Ok(())
    }

    #[test]
    fn test_missing_directory_is_counted_and_retried_not_fatal() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("missing").join("health.json");
        let mut exporter = HealthExporter::new(path.clone(), ConfigIdentity::default());
        for _ in 0..2 {
            assert!(!exporter.record_cycle(
                &NullSink,
                &[],
                &[],
                IpcCounters::default(),
                Timestamp::UNIX_EPOCH
            ));
        }
        assert_eq!(exporter.failures(), 2);
        fs::create_dir(dir.path().join("missing")).map_err(ctx("mkdir"))?;
        assert!(exporter.record_cycle(
            &NullSink,
            &[],
            &[],
            IpcCounters::default(),
            Timestamp::UNIX_EPOCH
        ));
        assert_eq!(exporter.written(), 1);
        assert!(path.is_file());
        Ok(())
    }

    #[test]
    fn test_tracker_counts_only_sensors_that_were_due() -> TestResult {
        let now = at(100)?;
        let retry_later = SensorHealth::Retrying {
            failures: 1,
            next_attempt: now
                .checked_add(SignedDuration::from_secs(30))
                .map_err(ctx("next attempt"))?,
        };
        let retry_now = SensorHealth::Retrying {
            failures: 1,
            next_attempt: now,
        };
        let degraded = SensorHealth::Degraded {
            reason: DegradeReason::RetriesExhausted { attempts: 5 },
        };
        let before = vec![
            (id("waiting"), retry_later.clone()),
            (id("recovering"), retry_now.clone()),
            (id("failing"), retry_now),
            (id("gone"), degraded.clone()),
        ];
        let after = vec![
            (id("waiting"), retry_later),
            (id("recovering"), SensorHealth::Bound),
            (id("failing"), degraded.clone()),
            (id("gone"), degraded),
        ];
        let mut tracker = SensorTracker::default();
        tracker.observe(&before, &after, now);

        let waiting = tracker.counters(0);
        assert_eq!((waiting.readings_total, waiting.errors_total), (0, 0));
        let recovering = tracker.counters(1);
        assert_eq!((recovering.readings_total, recovering.errors_total), (1, 0));
        assert_eq!(recovering.last_ok_at, Some(now));
        let failing = tracker.counters(2);
        assert_eq!((failing.readings_total, failing.errors_total), (0, 1));
        let gone = tracker.counters(3);
        assert_eq!((gone.readings_total, gone.errors_total), (0, 0));

        let encoded = encode_document(
            &ConfigIdentity::default(),
            &after,
            &tracker,
            IpcCounters::default(),
            now,
        )
        .map_err(ctx("encode"))?;
        let value: serde_json::Value = serde_json::from_str(&encoded).map_err(ctx("parse"))?;
        assert_eq!(value["sensors"][0]["health"], "retrying");
        assert_eq!(value["sensors"][2]["degrade_reason"], "retries_exhausted");
        assert!(value["sensors"][0].get("degrade_reason").is_none());
        assert!(value["sensors"][0].get("last_ok_at").is_none());
        Ok(())
    }

    #[test]
    fn test_default_path_is_the_health_json_sibling() {
        assert_eq!(
            default_path_for(Path::new("/var/lib/harw-dod/export/findings.jsonl")),
            Some(PathBuf::from("/var/lib/harw-dod/export/health.json"))
        );
        assert_eq!(default_path_for(Path::new("findings.jsonl")), None);
    }

    #[test]
    fn test_temporary_path_is_a_hidden_sibling() {
        assert_eq!(
            temporary_path(Path::new("/var/lib/harw-dod/export/health.json")),
            Some(PathBuf::from("/var/lib/harw-dod/export/.health.json.tmp"))
        );
        assert_eq!(temporary_path(Path::new("/")), None);
    }
}
