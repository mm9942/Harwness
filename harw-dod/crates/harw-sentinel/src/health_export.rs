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
//!   "ipc": {"probes_connected": 1, "events_total": 40, "dropped_total": 0,
//!           "peers_rejected_total": 0,
//!           "events_last_cycle": 3, "dropped_last_cycle": 0,
//!           "events_last_60s": 12, "dropped_last_60s": 0},
//!   "ring": {"overflow_samples_total": 0, "overflow_events_total": 0,
//!            "overflow_samples_last_cycle": 0, "overflow_events_last_cycle": 0,
//!            "overflow_samples_last_60s": 0, "overflow_events_last_60s": 0},
//!   "windows": {"last_cycle_ms": 5000, "last_60s_ms": 60000},
//!   "spool": {"state": "degraded", "degrade_reason": "spool_full",
//!             "enqueued_total": 9, "written_total": 8, "dropped_total": 0,
//!             "spool_full_total": 1, "too_large_total": 0, "failed_total": 0,
//!             "removed_total": 0, "dropped_last_60s": 0, "rejected_last_60s": 1}
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
//! - `ipc.peers_rejected_total` (PL-100 WP0.5, additive) counts probe
//!   socket connections closed because their `SO_PEERCRED` uid is not on
//!   the probe allowlist (`crate::peer_allow`). Expected: `0`.
//! - `ring` counts entries the evidence ring evicted **before** the rules
//!   evaluated them (`crate::findings::RuleCycle`): observations that were
//!   never checked. Expected: `0`.
//! - Windowed deltas (PL-100 WP0.13, additive): every cumulative counter of
//!   `ipc` and `ring` has a `*_last_cycle` delta (since the previous
//!   document) and a `*_last_60s` delta (since the newest cycle at least
//!   [`HEALTH_WINDOW_SECS`] seconds ago). `windows` reports the span each
//!   delta actually covers in milliseconds; `null` means "since process
//!   start" (the first cycle, or a process younger than the window). When
//!   the bounded history ([`MAX_WINDOW_SNAPSHOTS`]) no longer reaches back
//!   a full window, `last_60s_ms` is the shorter span actually covered.
//!   Counters that go backwards yield `0`, never a wrapped value.
//! - `spool` (PL-100 WP0.12, additive) reports the finding spool writer
//!   (`crate::spool_writer`); it is `null` when no spool is configured (no
//!   `--findings-export`). `state` is `ok` or `degraded`; `degraded` when the
//!   spool could not be opened (`unavailable`), the most recent write failed,
//!   or a record was dropped (full writer queue) or refused (`SpoolFull`,
//!   `RecordTooLarge`, another write failure) within the last
//!   [`HEALTH_WINDOW_SECS`] seconds. `degrade_reason` (present only when
//!   degraded) is the most recent cause: `unavailable`, `queue_overflow`,
//!   `spool_full`, `record_too_large` or `write_failed`. The counters are
//!   cumulative since process start; `dropped_last_60s` and
//!   `rejected_last_60s` are windowed like the `ipc`/`ring` deltas.
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

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_dod_sentinel::{DegradeReason, SensorHealth};
use harw_observe::{MetricValue, TelemetrySink};
use harw_types::SensorId;
use jiff::{SignedDuration, Timestamp};
use serde::Serialize;

use crate::spool_writer::{SpoolCounters, SpoolFailure, SpoolStats};

/// Schema version of the health document (`schema_version`).
pub const HEALTH_SCHEMA_VERSION: u32 = 1;

/// File name of the default health export next to `--findings-export`.
pub const DEFAULT_HEALTH_FILE_NAME: &str = "health.json";

/// Mode of a newly created health document: owner read/write, group read
/// (readers get access through the export directory's group), nothing else.
pub const HEALTH_FILE_MODE: u32 = 0o640;

/// Length of the sliding window behind the `*_last_60s` deltas, in seconds.
pub const HEALTH_WINDOW_SECS: i64 = 60;

/// [`HEALTH_WINDOW_SECS`] as a duration.
const HEALTH_WINDOW: SignedDuration = SignedDuration::from_secs(HEALTH_WINDOW_SECS);

/// Most per-cycle counter snapshots kept for the sliding window (bounds the
/// memory even with a very short `--interval-secs`).
pub const MAX_WINDOW_SNAPSHOTS: usize = 4096;

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
    /// Connections closed since start because the peer uid is not on the
    /// probe allowlist (PL-100 WP0.5).
    pub peers_rejected_total: u64,
}

/// Cumulative counters of the evidence ring (`ring` object).
///
/// # Description
/// Entries the ring evicted before the rules evaluated them, since process
/// start (`crate::findings::RuleCycle::ring_counters`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RingCounters {
    /// Samples evicted before evaluation.
    pub overflow_samples_total: u64,
    /// Events evicted before evaluation.
    pub overflow_events_total: u64,
}

/// The counters that get windowed deltas, as one value.
///
/// # Description
/// Used both for a cumulative snapshot and for the difference of two
/// snapshots ([`Self::since`]).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HealthCounters {
    /// IPC events received (`ipc.events_*`).
    pub ipc_events: u64,
    /// IPC events evicted from the inbox (`ipc.dropped_*`).
    pub ipc_dropped: u64,
    /// Ring samples evicted before evaluation (`ring.overflow_samples_*`).
    pub ring_overflow_samples: u64,
    /// Ring events evicted before evaluation (`ring.overflow_events_*`).
    pub ring_overflow_events: u64,
    /// Spool records dropped at the full writer queue (`spool.dropped_*`).
    pub spool_dropped: u64,
    /// Spool records refused (`spool.rejected_*`: full spool, too large,
    /// other write failure).
    pub spool_rejected: u64,
}

impl HealthCounters {
    /// The cumulative snapshot of `ipc` and `ring` (spool counters `0`, see
    /// [`Self::with_spool`]).
    #[must_use]
    pub const fn of(ipc: IpcCounters, ring: RingCounters) -> Self {
        Self {
            ipc_events: ipc.events_total,
            ipc_dropped: ipc.dropped_total,
            ring_overflow_samples: ring.overflow_samples_total,
            ring_overflow_events: ring.overflow_events_total,
            spool_dropped: 0,
            spool_rejected: 0,
        }
    }

    /// The same snapshot with the cumulative counters of the finding spool
    /// writer (PL-100 WP0.12).
    #[must_use]
    pub const fn with_spool(self, spool: &SpoolCounters) -> Self {
        Self {
            spool_dropped: spool.dropped_total,
            spool_rejected: spool.rejected_total(),
            ..self
        }
    }

    /// The per-counter difference `self - base`, `0` where a counter went
    /// backwards.
    #[must_use]
    pub const fn since(self, base: Self) -> Self {
        Self {
            ipc_events: self.ipc_events.saturating_sub(base.ipc_events),
            ipc_dropped: self.ipc_dropped.saturating_sub(base.ipc_dropped),
            ring_overflow_samples: self
                .ring_overflow_samples
                .saturating_sub(base.ring_overflow_samples),
            ring_overflow_events: self
                .ring_overflow_events
                .saturating_sub(base.ring_overflow_events),
            spool_dropped: self.spool_dropped.saturating_sub(base.spool_dropped),
            spool_rejected: self.spool_rejected.saturating_sub(base.spool_rejected),
        }
    }
}

/// Deltas over the two health windows (see the module documentation).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowDeltas {
    /// Change since the previous document.
    pub last_cycle: HealthCounters,
    /// Span of `last_cycle` in milliseconds; `None` = since process start.
    pub last_cycle_ms: Option<u64>,
    /// Change over the last [`HEALTH_WINDOW_SECS`] seconds.
    pub last_60s: HealthCounters,
    /// Span of `last_60s` in milliseconds; `None` = since process start.
    pub last_60s_ms: Option<u64>,
}

/// Remembers per-cycle cumulative snapshots for the windowed deltas.
///
/// # Description
/// Keeps, oldest first, the newest snapshot at least one window old (the
/// baseline of the next window) plus every younger one, at most
/// [`MAX_WINDOW_SNAPSHOTS`].
#[derive(Debug, Default)]
struct WindowTracker {
    // `(now, cumulative counters)` per recorded cycle, oldest first.
    history: VecDeque<(Timestamp, HealthCounters)>,
    // Whether a snapshot was ever discarded, i.e. the process-start baseline
    // (all zero) is no longer the implicit oldest one.
    truncated: bool,
}

impl WindowTracker {
    /// Records the cumulative counters of the cycle at `now` and returns its
    /// windowed deltas.
    fn record(&mut self, now: Timestamp, current: HealthCounters) -> WindowDeltas {
        let (last_cycle, last_cycle_ms) = match self.history.back() {
            Some((at, base)) => (current.since(*base), span_ms(now, *at)),
            None => (current, None),
        };
        let window_start = now.checked_sub(HEALTH_WINDOW).ok();
        let baseline = window_start.and_then(|start| {
            self.history
                .iter()
                .rev()
                .find(|(at, _)| *at <= start)
                .copied()
        });
        let (last_60s, last_60s_ms) = match (baseline, self.truncated, self.history.front()) {
            (Some((at, base)), _, _) | (None, true, Some(&(at, base))) => {
                (current.since(base), span_ms(now, at))
            }
            (None, _, _) => (current, None),
        };

        self.history.push_back((now, current));
        if let Some(start) = window_start {
            // Ältere Stände als die jüngste Basis vor dem Fenster werden nie
            // mehr gebraucht.
            while self.history.get(1).is_some_and(|(at, _)| *at <= start) {
                self.history.pop_front();
                self.truncated = true;
            }
        }
        while self.history.len() > MAX_WINDOW_SNAPSHOTS {
            self.history.pop_front();
            self.truncated = true;
        }
        WindowDeltas {
            last_cycle,
            last_cycle_ms,
            last_60s,
            last_60s_ms,
        }
    }
}

/// Milliseconds from `earlier` to `now`; `None` if the clock went backwards.
fn span_ms(now: Timestamp, earlier: Timestamp) -> Option<u64> {
    u64::try_from(now.duration_since(earlier).as_millis()).ok()
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

/// The `ipc` object: cumulative counters plus their windowed deltas.
#[derive(Debug, Serialize)]
struct IpcLine {
    probes_connected: u64,
    events_total: u64,
    dropped_total: u64,
    peers_rejected_total: u64,
    events_last_cycle: u64,
    dropped_last_cycle: u64,
    events_last_60s: u64,
    dropped_last_60s: u64,
}

/// The `ring` object: cumulative overflow counters plus windowed deltas.
#[derive(Debug, Serialize)]
struct RingLine {
    overflow_samples_total: u64,
    overflow_events_total: u64,
    overflow_samples_last_cycle: u64,
    overflow_events_last_cycle: u64,
    overflow_samples_last_60s: u64,
    overflow_events_last_60s: u64,
}

/// The `windows` object: the spans the deltas cover.
#[derive(Debug, Serialize)]
struct WindowLine {
    last_cycle_ms: Option<u64>,
    last_60s_ms: Option<u64>,
}

/// The `spool` object (PL-100 WP0.12): state of the finding spool writer.
#[derive(Debug, Serialize)]
struct SpoolLine {
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    degrade_reason: Option<&'static str>,
    enqueued_total: u64,
    written_total: u64,
    dropped_total: u64,
    spool_full_total: u64,
    too_large_total: u64,
    failed_total: u64,
    removed_total: u64,
    dropped_last_60s: u64,
    rejected_last_60s: u64,
}

impl SpoolLine {
    /// The line for `spool`, judged against the windowed deltas.
    fn of(spool: &SpoolCounters, windows: &WindowDeltas) -> Self {
        let (state, degrade_reason) = spool_state(spool, windows);
        Self {
            state,
            degrade_reason,
            enqueued_total: spool.enqueued_total,
            written_total: spool.written_total,
            dropped_total: spool.dropped_total,
            spool_full_total: spool.spool_full_total,
            too_large_total: spool.too_large_total,
            failed_total: spool.failed_total,
            removed_total: spool.removed_total,
            dropped_last_60s: windows.last_60s.spool_dropped,
            rejected_last_60s: windows.last_60s.spool_rejected,
        }
    }
}

/// `state` and `degrade_reason` of the `spool` object.
///
/// # Description
/// `degraded` when the spool is unavailable, the most recent write failed,
/// or a record was dropped or refused within the last window (see the
/// module documentation); the reason is the most recent cause.
///
/// # Returns
/// `("ok", None)` or `("degraded", Some(reason))`.
#[must_use]
pub fn spool_state(
    spool: &SpoolCounters,
    windows: &WindowDeltas,
) -> (&'static str, Option<&'static str>) {
    let recent_loss = windows.last_60s.spool_dropped > 0 || windows.last_60s.spool_rejected > 0;
    if !(spool.unavailable || spool.last_put_failed || recent_loss) {
        return ("ok", None);
    }
    let reason = if spool.unavailable {
        SpoolFailure::Unavailable
    } else {
        spool.last_failure.unwrap_or(SpoolFailure::WriteFailed)
    };
    ("degraded", Some(reason.label()))
}

/// The whole document.
#[derive(Debug, Serialize)]
struct HealthDocument<'a> {
    schema_version: u32,
    written_at: Timestamp,
    config_digest: Option<&'a str>,
    profile: Option<&'a str>,
    sensors: Vec<SensorLine<'a>>,
    ipc: IpcLine,
    ring: RingLine,
    windows: WindowLine,
    spool: Option<SpoolLine>,
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
/// # Arguments
/// - `identity`, `snapshot`, `tracker`: configuration identity, sensor
///   health and per-sensor counters.
/// - `ipc`, `ring`: the cumulative counters.
/// - `spool`: the finding spool writer's counters, `None` without a spool
///   (written as `"spool": null`).
/// - `windows`: their windowed deltas ([`WindowDeltas`]).
/// - `now`: reported as `written_at`.
///
/// # Errors
/// `serde_json::Error` if serialization fails (practically unreachable for
/// these field types, but not treated as a panic).
// Acht Eingänge eines Dokuments, keiner davon ableitbar; ein Bündel-Typ nur
// für diesen einen Aufrufer brächte nichts (wie `report_findings`).
#[allow(clippy::too_many_arguments)]
pub fn encode_document(
    identity: &ConfigIdentity,
    snapshot: &[(SensorId, SensorHealth)],
    tracker: &SensorTracker,
    ipc: IpcCounters,
    ring: RingCounters,
    spool: Option<&SpoolCounters>,
    windows: &WindowDeltas,
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
        ipc: IpcLine {
            probes_connected: ipc.probes_connected,
            events_total: ipc.events_total,
            dropped_total: ipc.dropped_total,
            peers_rejected_total: ipc.peers_rejected_total,
            events_last_cycle: windows.last_cycle.ipc_events,
            dropped_last_cycle: windows.last_cycle.ipc_dropped,
            events_last_60s: windows.last_60s.ipc_events,
            dropped_last_60s: windows.last_60s.ipc_dropped,
        },
        ring: RingLine {
            overflow_samples_total: ring.overflow_samples_total,
            overflow_events_total: ring.overflow_events_total,
            overflow_samples_last_cycle: windows.last_cycle.ring_overflow_samples,
            overflow_events_last_cycle: windows.last_cycle.ring_overflow_events,
            overflow_samples_last_60s: windows.last_60s.ring_overflow_samples,
            overflow_events_last_60s: windows.last_60s.ring_overflow_events,
        },
        windows: WindowLine {
            last_cycle_ms: windows.last_cycle_ms,
            last_60s_ms: windows.last_60s_ms,
        },
        spool: spool.map(|spool| SpoolLine::of(spool, windows)),
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

/// Temporary sibling `.<name>.tmp` used for the atomic replacement (also by
/// `crate::throttle` for its state file).
pub(crate) fn temporary_path(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?;
    let mut temporary = OsString::from(".");
    temporary.push(name);
    temporary.push(".tmp");
    Some(path.with_file_name(temporary))
}

/// Writes `bytes` to `path` atomically (temporary sibling, flush, rename).
///
/// # Description
/// A target that exists but is not a regular file (symlink, directory) is
/// refused; a leftover temporary is removed (never followed); the new file
/// is created with `O_EXCL` and `mode`. Shared with `crate::throttle`.
///
/// # Errors
/// Every I/O error of these steps; the temporary is removed on failure.
pub(crate) fn write_atomically(
    path: &Path,
    temporary: &Path,
    bytes: &[u8],
    mode: u32,
) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.file_type().is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "atomic write target exists but is not a regular file",
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
        options.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = mode; // Ohne Unix-Modusbits gibt es nichts zu setzen.
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
/// document, the [`SensorTracker`], the optional counters of the finding
/// spool writer, and counters for written and failed documents. See the
/// module documentation for format and failure handling.
#[derive(Debug)]
pub struct HealthExporter {
    path: PathBuf,
    identity: ConfigIdentity,
    tracker: SensorTracker,
    /// Per-cycle snapshots behind the windowed deltas.
    windows: WindowTracker,
    /// Counters of the finding spool writer (`spool`), if one runs.
    spool: Option<Arc<SpoolStats>>,
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
            windows: WindowTracker::default(),
            spool: None,
            written: 0,
            failures: 0,
            failing: false,
        }
    }

    /// Reports the finding spool writer from now on (`spool` object, PL-100
    /// WP0.12).
    ///
    /// # Arguments
    /// - `stats` (`Arc<SpoolStats>`): the writer's shared counters
    ///   (`crate::spool_writer::SpoolWriter::stats`).
    #[must_use]
    pub fn with_spool(mut self, stats: Arc<SpoolStats>) -> Self {
        self.spool = Some(stats);
        self
    }

    /// Target path of the health document.
    #[cfg(test)]
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
    #[cfg(test)]
    #[must_use]
    pub const fn written(&self) -> u64 {
        self.written
    }

    /// Number of failed writes since process start.
    #[cfg(test)]
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
    /// - `ring`: current evidence-ring overflow counters.
    /// - `now`: the cycle's timestamp, reported as `written_at`; also the
    ///   time base of the windowed deltas.
    ///
    /// The spool counters, if [`Self::with_spool`] was set, are read here
    /// once per cycle.
    ///
    /// # Returns
    /// `true` if the document was replaced; `false` after a counted and
    /// logged failure. The windowed deltas advance either way.
    pub fn record_cycle(
        &mut self,
        sink: &dyn TelemetrySink,
        before: &[(SensorId, SensorHealth)],
        after: &[(SensorId, SensorHealth)],
        ipc: IpcCounters,
        ring: RingCounters,
        now: Timestamp,
    ) -> bool {
        self.tracker.observe(before, after, now);
        let spool = self.spool.as_ref().map(|stats| stats.snapshot());
        let mut current = HealthCounters::of(ipc, ring);
        if let Some(spool) = spool.as_ref() {
            current = current.with_spool(spool);
        }
        let windows = self.windows.record(now, current);
        let result = encode_document(
            &self.identity,
            after,
            &self.tracker,
            ipc,
            ring,
            spool.as_ref(),
            &windows,
            now,
        )
        .map_err(io::Error::other)
        .and_then(|encoded| {
            let temporary = temporary_path(&self.path).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "health export path has no file name",
                )
            })?;
            write_atomically(&self.path, &temporary, encoded.as_bytes(), HEALTH_FILE_MODE)
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
        ring: Ring,
        windows: Windows,
        spool: Option<Spool>,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Spool {
        state: String,
        #[serde(default)]
        degrade_reason: Option<String>,
        enqueued_total: u64,
        written_total: u64,
        dropped_total: u64,
        spool_full_total: u64,
        too_large_total: u64,
        failed_total: u64,
        removed_total: u64,
        dropped_last_60s: u64,
        rejected_last_60s: u64,
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
        peers_rejected_total: u64,
        events_last_cycle: u64,
        dropped_last_cycle: u64,
        events_last_60s: u64,
        dropped_last_60s: u64,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Ring {
        overflow_samples_total: u64,
        overflow_events_total: u64,
        overflow_samples_last_cycle: u64,
        overflow_events_last_cycle: u64,
        overflow_samples_last_60s: u64,
        overflow_events_last_60s: u64,
    }

    #[derive(Debug, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Windows {
        last_cycle_ms: Option<u64>,
        last_60s_ms: Option<u64>,
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
            peers_rejected_total: 5,
        };
        let ring = RingCounters {
            overflow_samples_total: 3,
            overflow_events_total: 4,
        };
        assert!(exporter.record_cycle(&NullSink, &before, &after, ipc, ring, now));
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
        assert_eq!(document.ipc.peers_rejected_total, 5);
        // Erster Zyklus: die Fenster reichen bis zum Prozessstart zurück.
        assert_eq!(document.ipc.events_last_cycle, 40);
        assert_eq!(document.ipc.dropped_last_cycle, 2);
        assert_eq!(document.ipc.events_last_60s, 40);
        assert_eq!(document.ipc.dropped_last_60s, 2);
        assert_eq!(document.ring.overflow_samples_total, 3);
        assert_eq!(document.ring.overflow_events_total, 4);
        assert_eq!(document.ring.overflow_samples_last_cycle, 3);
        assert_eq!(document.ring.overflow_events_last_cycle, 4);
        assert_eq!(document.ring.overflow_samples_last_60s, 3);
        assert_eq!(document.ring.overflow_events_last_60s, 4);
        assert_eq!(document.windows.last_cycle_ms, None);
        assert_eq!(document.windows.last_60s_ms, None);
        assert!(document.spool.is_none(), "no spool configured");
        Ok(())
    }

    /// PL-100 WP0.12: ein nicht geöffneter Spool erscheint als `degraded`.
    #[test]
    fn test_an_unavailable_spool_is_reported_degraded() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join(DEFAULT_HEALTH_FILE_NAME);
        let writer = crate::spool_writer::SpoolWriter::unavailable();
        let mut exporter = HealthExporter::new(path.clone(), identity()).with_spool(writer.stats());
        assert!(exporter.record_cycle(
            &NullSink,
            &[],
            &[],
            IpcCounters::default(),
            RingCounters::default(),
            at(0)?,
        ));
        let spool = read_document(&path)?
            .spool
            .ok_or(TestError::Missing("spool object"))?;
        assert_eq!(spool.state, "degraded");
        assert_eq!(spool.degrade_reason.as_deref(), Some("unavailable"));
        Ok(())
    }

    /// PL-100 WP0.12: ein abgewiesener Record (`RecordTooLarge`) macht den
    /// Spool für ein Fenster `degraded`; danach ist er wieder `ok`.
    #[test]
    fn test_a_refused_record_degrades_the_spool_for_one_window() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let spool_dir = dir.path().join(crate::spool_writer::SPOOL_DIR_NAME);
        crate::spool_writer::prepare_spool_dir(&spool_dir).map_err(ctx("spool dir"))?;
        let spool = harw_dod_sentinel::spool::FindingSpool::open_with_limits(
            &spool_dir,
            crate::spool_writer::sentinel_limits(),
        )
        .map_err(ctx("open spool"))?;
        let writer = crate::spool_writer::SpoolWriter::spawn(spool, 4).map_err(ctx("spawn"))?;
        let path = dir.path().join(DEFAULT_HEALTH_FILE_NAME);
        let mut exporter = HealthExporter::new(path.clone(), identity()).with_spool(writer.stats());
        writer.note_rejected(&harw_dod_sentinel::spool::SpoolError::RecordTooLarge {
            len: 70_000,
            limit: 65_536,
        });

        assert!(exporter.record_cycle(
            &NullSink,
            &[],
            &[],
            IpcCounters::default(),
            RingCounters::default(),
            at(0)?,
        ));
        let spool = read_document(&path)?
            .spool
            .ok_or(TestError::Missing("spool object"))?;
        assert_eq!(spool.state, "degraded");
        assert_eq!(spool.degrade_reason.as_deref(), Some("record_too_large"));
        assert_eq!(
            (
                spool.enqueued_total,
                spool.written_total,
                spool.dropped_total,
                spool.spool_full_total,
                spool.too_large_total,
                spool.failed_total,
                spool.removed_total,
            ),
            (0, 0, 0, 0, 1, 0, 0)
        );
        assert_eq!((spool.dropped_last_60s, spool.rejected_last_60s), (0, 1));

        assert!(exporter.record_cycle(
            &NullSink,
            &[],
            &[],
            IpcCounters::default(),
            RingCounters::default(),
            at(120)?,
        ));
        let later = read_document(&path)?
            .spool
            .ok_or(TestError::Missing("spool object"))?;
        assert_eq!(later.state, "ok", "the refusal left the window");
        assert_eq!(later.degrade_reason, None);
        assert_eq!(later.too_large_total, 1, "cumulative counters stay");
        assert_eq!(later.rejected_last_60s, 0);
        Ok(())
    }

    #[test]
    fn test_spool_state_is_degraded_only_for_recent_or_current_failures() {
        let quiet = WindowDeltas::default();
        let refused_now = WindowDeltas {
            last_60s: HealthCounters {
                spool_rejected: 1,
                ..HealthCounters::default()
            },
            ..WindowDeltas::default()
        };
        let dropped_now = WindowDeltas {
            last_60s: HealthCounters {
                spool_dropped: 2,
                ..HealthCounters::default()
            },
            ..WindowDeltas::default()
        };
        let full = SpoolCounters {
            spool_full_total: 1,
            last_failure: Some(SpoolFailure::SpoolFull),
            ..SpoolCounters::default()
        };
        assert_eq!(spool_state(&SpoolCounters::default(), &quiet), ("ok", None));
        assert_eq!(
            spool_state(&full, &refused_now),
            ("degraded", Some("spool_full"))
        );
        assert_eq!(spool_state(&full, &quiet), ("ok", None), "aged out");
        let still_failing = SpoolCounters {
            last_put_failed: true,
            ..full
        };
        assert_eq!(
            spool_state(&still_failing, &quiet),
            ("degraded", Some("spool_full"))
        );
        let overflow = SpoolCounters {
            dropped_total: 2,
            last_failure: Some(SpoolFailure::QueueOverflow),
            ..SpoolCounters::default()
        };
        assert_eq!(
            spool_state(&overflow, &dropped_now),
            ("degraded", Some("queue_overflow"))
        );
        let gone = SpoolCounters {
            unavailable: true,
            last_failure: Some(SpoolFailure::QueueOverflow),
            ..SpoolCounters::default()
        };
        assert_eq!(
            spool_state(&gone, &quiet),
            ("degraded", Some("unavailable"))
        );
    }

    fn counters(dropped: u64, overflow_events: u64) -> (IpcCounters, RingCounters) {
        (
            IpcCounters {
                probes_connected: 1,
                events_total: dropped.saturating_mul(10),
                dropped_total: dropped,
                peers_rejected_total: 0,
            },
            RingCounters {
                overflow_samples_total: 0,
                overflow_events_total: overflow_events,
            },
        )
    }

    /// PL-100 WP0.13: Deltas des letzten Zyklus und der letzten 60 s neben
    /// den kumulierten Zählern.
    #[test]
    fn test_windowed_deltas_cover_the_last_cycle_and_the_last_minute() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join(DEFAULT_HEALTH_FILE_NAME);
        let mut exporter = HealthExporter::new(path.clone(), ConfigIdentity::default());
        // (Sekunde, kumuliert verworfen, kumulierter Ring-Überlauf)
        for (second, dropped, overflow) in [(100, 0, 0), (130, 4, 1), (160, 10, 1), (190, 12, 5)] {
            let (ipc, ring) = counters(dropped, overflow);
            assert!(exporter.record_cycle(&NullSink, &[], &[], ipc, ring, at(second)?));
        }
        let document = read_document(&path)?;
        assert_eq!(document.ipc.dropped_total, 12, "cumulative counters stay");
        assert_eq!(document.ipc.dropped_last_cycle, 2, "190 vs 160");
        assert_eq!(document.ipc.dropped_last_60s, 8, "190 vs 130");
        assert_eq!(document.ipc.events_last_cycle, 20);
        assert_eq!(document.ipc.events_last_60s, 80);
        assert_eq!(document.ring.overflow_events_total, 5);
        assert_eq!(document.ring.overflow_events_last_cycle, 4);
        assert_eq!(document.ring.overflow_events_last_60s, 4);
        assert_eq!(document.ring.overflow_samples_last_60s, 0);
        assert_eq!(document.windows.last_cycle_ms, Some(30_000));
        assert_eq!(document.windows.last_60s_ms, Some(60_000));
        Ok(())
    }

    #[test]
    fn test_a_young_process_reports_the_minute_since_start() -> TestResult {
        let mut tracker = WindowTracker::default();
        let first = tracker.record(
            at(10)?,
            HealthCounters {
                ipc_dropped: 2,
                ..HealthCounters::default()
            },
        );
        assert_eq!(first.last_cycle.ipc_dropped, 2);
        assert_eq!(first.last_cycle_ms, None);
        let second = tracker.record(
            at(15)?,
            HealthCounters {
                ipc_dropped: 3,
                ..HealthCounters::default()
            },
        );
        assert_eq!(second.last_cycle.ipc_dropped, 1);
        assert_eq!(second.last_cycle_ms, Some(5_000));
        assert_eq!(
            second.last_60s.ipc_dropped, 3,
            "younger than the window: everything since start"
        );
        assert_eq!(second.last_60s_ms, None);
        Ok(())
    }

    #[test]
    fn test_counters_going_backwards_yield_zero_not_a_wrap() -> TestResult {
        let mut tracker = WindowTracker::default();
        tracker.record(
            at(10)?,
            HealthCounters {
                ipc_events: 50,
                ..HealthCounters::default()
            },
        );
        let after_reset = tracker.record(
            at(15)?,
            HealthCounters {
                ipc_events: 5,
                ..HealthCounters::default()
            },
        );
        assert_eq!(after_reset.last_cycle.ipc_events, 0);
        let back = tracker.record(at(1)?, HealthCounters::default());
        assert_eq!(back.last_cycle_ms, None, "a clock jump back has no span");
        Ok(())
    }

    #[test]
    fn test_window_history_stays_bounded() -> TestResult {
        let mut tracker = WindowTracker::default();
        let mut last = WindowDeltas::default();
        for millis in 0..6_000_i64 {
            let now = Timestamp::from_millisecond(millis).map_err(ctx("timestamp"))?;
            let total = u64::try_from(millis).map_err(ctx("non-negative"))?;
            last = tracker.record(
                now,
                HealthCounters {
                    ipc_events: total,
                    ..HealthCounters::default()
                },
            );
            assert!(tracker.history.len() <= MAX_WINDOW_SNAPSHOTS);
        }
        // Die gekappte Historie reicht nicht mehr eine ganze Minute zurück:
        // gemeldet wird die tatsächlich abgedeckte Spanne.
        let covered = u64::try_from(MAX_WINDOW_SNAPSHOTS).map_err(ctx("snapshot bound"))?;
        assert_eq!(last.last_60s_ms, Some(covered));
        assert_eq!(last.last_60s.ipc_events, covered);
        Ok(())
    }

    #[test]
    fn test_builtin_defaults_report_null_profile_and_digest() -> TestResult {
        let encoded = encode_document(
            &ConfigIdentity::default(),
            &[],
            &SensorTracker::default(),
            IpcCounters::default(),
            RingCounters::default(),
            None,
            &WindowDeltas::default(),
            Timestamp::UNIX_EPOCH,
        )
        .map_err(ctx("encode"))?;
        let value: serde_json::Value = serde_json::from_str(&encoded).map_err(ctx("parse"))?;
        assert!(value["config_digest"].is_null());
        assert!(value["profile"].is_null());
        assert!(value["spool"].is_null(), "no spool is written as null");
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
                RingCounters::default(),
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
            RingCounters::default(),
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
            RingCounters::default(),
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
            RingCounters::default(),
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
                RingCounters::default(),
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
            RingCounters::default(),
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
            RingCounters::default(),
            None,
            &WindowDeltas::default(),
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
