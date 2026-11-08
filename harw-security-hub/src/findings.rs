//! Read-only DoD correlation: recent high-severity findings for this host.
//!
//! # Boundary (masterplan v2 §20)
//! DoD stays structurally separate. This module **never writes** to DoD,
//! never links a DoD crate and never talks to `sentinel.sock` or
//! `warden.sock`. It only reads an append-only JSON Lines export that an
//! operator (or a future DoD uplink, WP-15) places at a configured path.
//!
//! # Export format
//! The DoD workspace has no JSON findings export today: `harw-sentinel`
//! reports each `Finding<RuleChecked>` via `tracing` and a telemetry counter
//! (see `harw-dod/crates/harw-sentinel/src/findings.rs`, "Bruch 2"). This module
//! therefore defines the minimal line format a DoD-side exporter has to
//! write — one JSON object per line:
//!
//! ```json
//! {"finding_id":"f-1","host":"host-a","rule_id":"structure-drift","severity":"high","summary":"…","observed_at":"2026-09-27T10:00:00Z"}
//! ```
//!
//! `severity` uses the lower-case names shared by
//! `harw_dod_signals::Severity` (kebab-case) and [`ImpactSeverity`]
//! (snake_case) — for single-word variants they are identical. Unknown extra
//! fields are ignored so a richer exporter stays compatible; missing or
//! malformed required fields make the line count as malformed.
//!
//! ## Export versions
//! The sentinel export (`harw-dod/crates/harw-sentinel/src/export.rs`) is
//! read additively: v1 is exactly the six fields above; v2 adds
//! `schema_version` and `scope`; v3 (PL-100 WP0.12) adds `instance`, `seq`,
//! `record_digest`, `dedup_key`, `occurrences`, `first_seen`, `kind`,
//! `hardness`, `cgroup_kid`, `scope_obfuscated` and `evidence_card`. This
//! reader keeps the six fields plus three of the additions that help a
//! posture consumer correlate repeats: `schema_version`, `dedup_key` (only a
//! well-formed 32-char lowercase hex key) and `occurrences` (only `>= 1`).
//! An invalid value of the right JSON type (a non-hex key, `occurrences: 0`)
//! is dropped, never the line; a value of the wrong JSON type and
//! `schema_version: 0` make the line malformed. All other v3 fields are
//! ignored like any unknown field.
//!
//! # Limits (the file is attacker-influenced input)
//! - only the last [`FindingLimits::max_tail_bytes`] of the file are read;
//!   when reading starts mid-file the first (partial) line is discarded,
//! - lines longer than [`FindingLimits::max_line_bytes`] are skipped unparsed,
//! - at most [`FindingLimits::max_records`] matching findings are returned
//!   (the newest ones),
//! - lines whose `observed_at` lies more than [`MAX_CLOCK_SKEW`] after the
//!   evaluation time [`FindingFilter::now`] are counted as malformed and
//!   dropped (a future-dated line would otherwise stay "recent" indefinitely
//!   and pin every posture view); the bound mirrors the canonical
//!   `harw_dod_client::findings::MAX_CLOCK_SKEW` reader,
//! - summaries are truncated to [`FindingLimits::max_summary_chars`] and
//!   control characters are replaced.
//!
//! A missing file is not an error for the posture endpoint: it reports
//! `unavailable` and carries on.

use std::collections::VecDeque;
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use harw_types::{FindingId, HostId, ImpactSeverity};

/// How far `observed_at` may lie after the evaluation time
/// ([`FindingFilter::now`]) before a line is treated as implausible and
/// counted in [`FindingBatch::skipped_malformed`].
///
/// Same value as `harw_dod_client::findings::MAX_CLOCK_SKEW` (5 min). It is
/// mirrored, not imported, because this module never links a DoD crate (see
/// the module boundary) and uses `jiff` instead of `time`.
///
/// The sync is enforced by the unit test
/// `test_clock_skew_mirrors_the_dod_client_definition`: it embeds
/// `harw-dod-client/src/findings.rs` as text (`include_str!`, no Cargo
/// dependency) and fails as soon as the client's definition changes.
pub const MAX_CLOCK_SKEW: SignedDuration = SignedDuration::from_mins(5);

/// One DoD finding as read from the export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DodFinding {
    /// Finding identity.
    pub finding_id: FindingId,
    /// Host the finding was observed on.
    pub host: HostId,
    /// Id of the rule that produced it.
    pub rule_id: String,
    /// Severity.
    pub severity: ImpactSeverity,
    /// Human-readable summary (sanitized, truncated).
    pub summary: String,
    /// Observation time.
    pub observed_at: Timestamp,
    /// Export schema version (`2`, `3`, ...); absent on v1 lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u32>,
    /// v3: throttling key of the finding (32 lowercase hex chars); lines
    /// with the same key report repeats of one finding. Dropped when
    /// malformed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedup_key: Option<String>,
    /// v3: how often the `dedup_key` fired (`>= 1`). Dropped when `0`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occurrences: Option<u64>,
}

/// Length of a v3 `dedup_key` (lowercase hex of 16 bytes).
const DEDUP_KEY_HEX_LEN: usize = 32;

/// `true` for a well-formed v3 `dedup_key`.
fn is_dedup_key(key: &str) -> bool {
    key.len() == DEDUP_KEY_HEX_LEN
        && key
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Which findings a caller wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingFilter {
    /// Only findings for this host.
    pub host: HostId,
    /// Only findings at or above this severity.
    pub min_severity: ImpactSeverity,
    /// Only findings observed at or after this time.
    pub since: Timestamp,
    /// Evaluation time (the caller's injected clock). Lines observed more
    /// than [`MAX_CLOCK_SKEW`] after it are implausible: they are counted in
    /// [`FindingBatch::skipped_malformed`] and never returned, whatever
    /// their host or severity.
    pub now: Timestamp,
}

impl FindingFilter {
    /// Latest plausible `observed_at`: `now + MAX_CLOCK_SKEW` (inclusive).
    /// Only near [`Timestamp::MAX`] does the addition overflow; the bound is
    /// then [`Timestamp::MAX`] itself, like the DoD client's reader keeps
    /// every line when its bound overflows.
    fn latest_plausible(&self) -> Timestamp {
        self.now
            .checked_add(MAX_CLOCK_SKEW)
            .unwrap_or(Timestamp::MAX)
    }

    // `true` when the finding belongs to the requested host, severity and
    // window start; the upper (skew) bound is checked separately so that
    // violators can be counted.
    fn matches(&self, finding: &DodFinding) -> bool {
        finding.host == self.host
            && finding.severity >= self.min_severity
            && finding.observed_at >= self.since
    }
}

/// Resource limits for reading the export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FindingLimits {
    /// Bytes read from the end of the file.
    pub max_tail_bytes: u64,
    /// Longest line that is parsed at all.
    pub max_line_bytes: usize,
    /// Most findings returned.
    pub max_records: usize,
    /// Longest summary kept (in chars).
    pub max_summary_chars: usize,
}

impl Default for FindingLimits {
    fn default() -> Self {
        Self {
            max_tail_bytes: 1024 * 1024,
            max_line_bytes: 16 * 1024,
            max_records: 64,
            max_summary_chars: 512,
        }
    }
}

/// Findings plus the bookkeeping a reader needs to judge completeness.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FindingBatch {
    /// Matching findings, newest first.
    pub findings: Vec<DodFinding>,
    /// Lines that were not valid finding records, including lines whose
    /// `observed_at` lies more than [`MAX_CLOCK_SKEW`] after
    /// [`FindingFilter::now`].
    pub skipped_malformed: usize,
    /// Lines longer than the line limit.
    pub skipped_oversize: usize,
    /// More findings matched than `max_records`; the oldest were dropped.
    pub truncated: bool,
}

/// Why a finding source could not deliver.
#[derive(Debug)]
#[non_exhaustive]
pub enum FindingSourceError {
    /// The export file does not exist (yet).
    NotFound(PathBuf),
    /// The path exists but is not a regular file.
    NotAFile(PathBuf),
    /// Reading failed.
    Io {
        /// The file.
        path: PathBuf,
        /// The I/O error.
        source: io::Error,
    },
}

impl fmt::Display for FindingSourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(path) => write!(f, "finding export '{}' not found", path.display()),
            Self::NotAFile(path) => write!(
                f,
                "finding export '{}' is not a regular file",
                path.display()
            ),
            Self::Io { path, source } => {
                write!(
                    f,
                    "cannot read finding export '{}': {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for FindingSourceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// A read-only source of DoD findings.
///
/// Implementations must not write to DoD and must bound their own resource
/// use. Calls are synchronous; the server runs them on a blocking thread.
pub trait DodFindingSource: Send + Sync {
    /// Returns recent findings matching `filter`, newest first.
    ///
    /// Findings observed more than [`MAX_CLOCK_SKEW`] after
    /// [`FindingFilter::now`] are never returned; they are counted in
    /// [`FindingBatch::skipped_malformed`].
    ///
    /// # Errors
    /// A [`FindingSourceError`] if the source cannot be read.
    fn recent(&self, filter: &FindingFilter) -> Result<FindingBatch, FindingSourceError>;
}

/// Reads the tail of a JSON Lines export file.
#[derive(Debug, Clone)]
pub struct JsonlFindingSource {
    path: PathBuf,
    limits: FindingLimits,
}

impl JsonlFindingSource {
    /// A source reading `path` with `limits`.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>, limits: FindingLimits) -> Self {
        Self {
            path: path.into(),
            limits,
        }
    }

    /// The configured path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn read_tail(&self) -> Result<(Vec<u8>, bool), FindingSourceError> {
        let io_error = |source| FindingSourceError::Io {
            path: self.path.clone(),
            source,
        };
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(FindingSourceError::NotFound(self.path.clone()));
            }
            Err(error) => return Err(io_error(error)),
        };
        let metadata = file.metadata().map_err(io_error)?;
        if !metadata.is_file() {
            return Err(FindingSourceError::NotAFile(self.path.clone()));
        }
        let len = metadata.len();
        let start = len.saturating_sub(self.limits.max_tail_bytes);
        file.seek(SeekFrom::Start(start)).map_err(io_error)?;
        let mut buffer = Vec::new();
        file.take(self.limits.max_tail_bytes)
            .read_to_end(&mut buffer)
            .map_err(io_error)?;
        Ok((buffer, start > 0))
    }
}

impl DodFindingSource for JsonlFindingSource {
    fn recent(&self, filter: &FindingFilter) -> Result<FindingBatch, FindingSourceError> {
        let (bytes, started_mid_file) = self.read_tail()?;
        Ok(parse_findings(
            &bytes,
            started_mid_file,
            filter,
            &self.limits,
        ))
    }
}

/// Parses a JSON Lines buffer into a bounded [`FindingBatch`].
///
/// `started_mid_file` discards everything up to and including the first
/// newline, because that line is (probably) cut off. A line observed more
/// than [`MAX_CLOCK_SKEW`] after `filter.now` counts as malformed, before
/// the host/severity/window filter applies.
#[must_use]
pub fn parse_findings(
    bytes: &[u8],
    started_mid_file: bool,
    filter: &FindingFilter,
    limits: &FindingLimits,
) -> FindingBatch {
    let body = if started_mid_file {
        match bytes.iter().position(|&byte| byte == b'\n') {
            Some(newline) => &bytes[newline + 1..],
            None => &[],
        }
    } else {
        bytes
    };

    let mut batch = FindingBatch::default();
    let mut kept: VecDeque<DodFinding> = VecDeque::new();
    let max_records = limits.max_records.max(1);
    let latest_plausible = filter.latest_plausible();
    for raw_line in body.split(|&byte| byte == b'\n') {
        let line = raw_line.trim_ascii();
        if line.is_empty() {
            continue;
        }
        if line.len() > limits.max_line_bytes {
            batch.skipped_oversize += 1;
            continue;
        }
        let Ok(mut finding) = serde_json::from_slice::<DodFinding>(line) else {
            batch.skipped_malformed += 1;
            continue;
        };
        if finding.schema_version == Some(0) {
            batch.skipped_malformed += 1;
            continue;
        }
        // Future-dated beyond the skew: implausible, would stay "recent"
        // forever otherwise.
        if finding.observed_at > latest_plausible {
            batch.skipped_malformed = batch.skipped_malformed.saturating_add(1);
            continue;
        }
        if !filter.matches(&finding) {
            continue;
        }
        finding.summary = sanitize(&finding.summary, limits.max_summary_chars);
        finding.rule_id = sanitize(&finding.rule_id, limits.max_summary_chars);
        finding.dedup_key = finding.dedup_key.filter(|key| is_dedup_key(key));
        finding.occurrences = finding.occurrences.filter(|count| *count >= 1);
        if kept.len() == max_records {
            kept.pop_front();
            batch.truncated = true;
        }
        kept.push_back(finding);
    }
    batch.findings = kept.into_iter().rev().collect();
    batch
}

fn sanitize(text: &str, max_chars: usize) -> String {
    text.chars()
        .take(max_chars)
        .map(|character| {
            if character.is_control() {
                '\u{FFFD}'
            } else {
                character
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        DodFindingSource, FindingFilter, FindingLimits, FindingSourceError, JsonlFindingSource,
        MAX_CLOCK_SKEW, parse_findings,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_types::{HostId, ImpactSeverity};
    use jiff::{SignedDuration, Timestamp};

    // Evaluation time of `filter()`: after every fixture line of the
    // existing tests (the newest is `V3_FIXTURE_LINE` at 11:00Z).
    const NOW: &str = "2026-09-27T12:00:00Z";

    fn filter() -> TestResult<FindingFilter> {
        Ok(FindingFilter {
            host: HostId::try_from_str("host-a").map_err(ctx("host id"))?,
            min_severity: ImpactSeverity::High,
            since: "2026-09-27T00:00:00Z"
                .parse::<Timestamp>()
                .map_err(ctx("since"))?,
            now: NOW.parse::<Timestamp>().map_err(ctx("now"))?,
        })
    }

    fn line(id: &str, host: &str, severity: &str, at: &str) -> String {
        format!(
            r#"{{"finding_id":"{id}","host":"{host}","rule_id":"structure-drift","severity":"{severity}","summary":"s {id}","observed_at":"{at}"}}"#
        )
    }

    #[test]
    fn test_filters_by_host_severity_and_time_newest_first() -> TestResult {
        let text = [
            line("f1", "host-a", "high", "2026-09-27T01:00:00Z"),
            line("f2", "host-b", "critical", "2026-09-27T02:00:00Z"),
            line("f3", "host-a", "medium", "2026-09-27T03:00:00Z"),
            line("f4", "host-a", "critical", "2026-09-26T23:59:59Z"),
            line("f5", "host-a", "critical", "2026-09-27T04:00:00Z"),
        ]
        .join("\n");
        let batch = parse_findings(
            text.as_bytes(),
            false,
            &filter()?,
            &FindingLimits::default(),
        );
        let ids: Vec<&str> = batch
            .findings
            .iter()
            .map(|f| f.finding_id.as_str())
            .collect();
        assert_eq!(ids, ["f5", "f1"]);
        assert_eq!(batch.skipped_malformed, 0);
        assert!(!batch.truncated);
        Ok(())
    }

    #[test]
    fn test_malformed_and_oversize_lines_are_counted_not_fatal() -> TestResult {
        let long_summary = "x".repeat(200);
        let oversize = format!(
            r#"{{"finding_id":"big","host":"host-a","rule_id":"r","severity":"high","summary":"{long_summary}","observed_at":"2026-09-27T01:00:00Z"}}"#
        );
        let text = [
            "not json".to_owned(),
            r#"{"finding_id":"","host":"host-a","rule_id":"r","severity":"high","summary":"","observed_at":"2026-09-27T01:00:00Z"}"#.to_owned(),
            r#"{"finding_id":"f1","host":"host-a","rule_id":"r","severity":"apocalyptic","summary":"","observed_at":"2026-09-27T01:00:00Z"}"#.to_owned(),
            oversize,
            String::new(),
            line("ok", "host-a", "critical", "2026-09-27T01:00:00Z"),
        ]
        .join("\n");
        let limits = FindingLimits {
            max_line_bytes: 150,
            ..FindingLimits::default()
        };
        let batch = parse_findings(text.as_bytes(), false, &filter()?, &limits);
        assert_eq!(batch.skipped_malformed, 3);
        assert_eq!(batch.skipped_oversize, 1);
        assert_eq!(batch.findings.len(), 1);
        Ok(())
    }

    #[test]
    fn test_record_limit_keeps_newest_and_marks_truncated() -> TestResult {
        let text: Vec<String> = (0..10)
            .map(|index| {
                line(
                    &format!("f{index}"),
                    "host-a",
                    "high",
                    "2026-09-27T01:00:00Z",
                )
            })
            .collect();
        let limits = FindingLimits {
            max_records: 3,
            ..FindingLimits::default()
        };
        let batch = parse_findings(text.join("\n").as_bytes(), false, &filter()?, &limits);
        let ids: Vec<&str> = batch
            .findings
            .iter()
            .map(|f| f.finding_id.as_str())
            .collect();
        assert_eq!(ids, ["f9", "f8", "f7"]);
        assert!(batch.truncated);
        Ok(())
    }

    #[test]
    fn test_mid_file_start_drops_partial_first_line() -> TestResult {
        let text = format!(
            "e\":\"host-a\"}}\n{}\n",
            line("f1", "host-a", "high", "2026-09-27T01:00:00Z")
        );
        let batch = parse_findings(text.as_bytes(), true, &filter()?, &FindingLimits::default());
        assert_eq!(batch.findings.len(), 1);
        assert_eq!(batch.skipped_malformed, 0);
        Ok(())
    }

    #[test]
    fn test_summary_is_sanitized_and_truncated() -> TestResult {
        let text = r#"{"finding_id":"f1","host":"host-a","rule_id":"r","severity":"critical","summary":"ab\u001b[31mcdef","observed_at":"2026-09-27T01:00:00Z","extra":"ignored"}"#;
        let limits = FindingLimits {
            max_summary_chars: 4,
            ..FindingLimits::default()
        };
        let batch = parse_findings(text.as_bytes(), false, &filter()?, &limits);
        let finding = batch
            .findings
            .first()
            .ok_or(TestError::Missing("finding"))?;
        assert_eq!(finding.summary, "ab\u{FFFD}[");
        Ok(())
    }

    #[test]
    fn test_file_source_reads_only_the_tail() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("findings.jsonl");
        let mut text = String::new();
        for index in 0..50 {
            text.push_str(&line(
                &format!("f{index}"),
                "host-a",
                "high",
                "2026-09-27T01:00:00Z",
            ));
            text.push('\n');
        }
        std::fs::write(&path, &text).map_err(ctx("write export"))?;
        let one_line = line("f00", "host-a", "high", "2026-09-27T01:00:00Z").len() as u64;
        let source = JsonlFindingSource::new(
            &path,
            FindingLimits {
                // Roughly three lines: the first is cut and discarded.
                max_tail_bytes: one_line * 3,
                ..FindingLimits::default()
            },
        );
        let batch = source.recent(&filter()?).map_err(ctx("recent"))?;
        assert!(batch.findings.len() <= 3, "{}", batch.findings.len());
        assert!(!batch.findings.is_empty());
        let newest = batch.findings.first().ok_or(TestError::Missing("newest"))?;
        assert_eq!(newest.finding_id.as_str(), "f49");
        assert_eq!(batch.skipped_malformed, 0);
        Ok(())
    }

    /// The canonical v2 export line (`V2_FIXTURE_LINE` in
    /// `harw-dod/crates/harw-sentinel/src/export.rs`, PL-97 D1/E4): additive
    /// `schema_version` and `scope` must not break the hub reader.
    const V2_FIXTURE_LINE: &str = r#"{"schema_version":2,"finding_id":"f-1","host":"host-a","rule_id":"structure-drift","severity":"high","summary":"s","observed_at":"2026-09-27T10:00:00Z","scope":"/sys/fs/cgroup/harw.slice"}"#;

    #[test]
    fn test_v2_fixture_line_parses_through_the_finding_and_the_tail_reader() -> TestResult {
        let finding: super::DodFinding =
            serde_json::from_str(V2_FIXTURE_LINE).map_err(ctx("v2 line deserializes"))?;
        assert_eq!(finding.finding_id.as_str(), "f-1");
        assert_eq!(finding.rule_id, "structure-drift");
        assert_eq!(finding.severity, ImpactSeverity::High);

        let text = format!(
            "{}\n{V2_FIXTURE_LINE}\n",
            line("f0", "host-a", "critical", "2026-09-27T09:00:00Z")
        );
        let batch = parse_findings(
            text.as_bytes(),
            false,
            &filter()?,
            &FindingLimits::default(),
        );
        assert_eq!(batch.skipped_malformed, 0);
        let ids: Vec<&str> = batch
            .findings
            .iter()
            .map(|f| f.finding_id.as_str())
            .collect();
        assert_eq!(ids, ["f-1", "f0"], "v1 and v2 lines, newest first");

        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("findings.jsonl");
        std::fs::write(&path, format!("{V2_FIXTURE_LINE}\n")).map_err(ctx("write export"))?;
        let batch = JsonlFindingSource::new(&path, FindingLimits::default())
            .recent(&filter()?)
            .map_err(ctx("recent"))?;
        assert_eq!(batch.findings.len(), 1);
        assert_eq!(
            batch.findings.first().map(|f| f.summary.as_str()),
            Some("s")
        );
        Ok(())
    }

    /// A v3 export line (PL-100 WP0.12) as the sentinel writes it: a strict
    /// superset of [`V2_FIXTURE_LINE`].
    const V3_FIXTURE_LINE: &str = r#"{"schema_version":3,"finding_id":"3f2b8c1e-9d4a-4b7e-8a6f-1c2d3e4f5a6b","host":"host-a","rule_id":"egress-flow","severity":"critical","summary":"tcp connect","observed_at":"2026-09-27T11:00:00Z","scope":"/sys/fs/cgroup/harw.slice","instance":"a1b2c3d4-e5f6-4a7b-9c8d-0e1f2a3b4c5d","seq":7,"record_digest":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","dedup_key":"0123456789abcdef0123456789abcdef","occurrences":3,"first_seen":"2026-09-27T10:00:00Z","kind":"rule_triggered","hardness":"observed","cgroup_kid":4242,"scope_obfuscated":false,"evidence_card":{"event_counts":{"tcp_connect":3},"uid":1000,"destination":"203.0.113.9:443","path":"/usr/bin/curl","process":{"pid":4711,"comm":"curl"}}}"#;

    #[test]
    fn test_v3_fixture_line_parses_additively_through_the_tail_reader() -> TestResult {
        let finding: super::DodFinding =
            serde_json::from_str(V3_FIXTURE_LINE).map_err(ctx("v3 line deserializes"))?;
        assert_eq!(
            finding.finding_id.as_str(),
            "3f2b8c1e-9d4a-4b7e-8a6f-1c2d3e4f5a6b"
        );
        assert_eq!(finding.severity, ImpactSeverity::Critical);
        assert_eq!(finding.schema_version, Some(3));
        assert_eq!(
            finding.dedup_key.as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        assert_eq!(finding.occurrences, Some(3));

        // v1, v2 and v3 lines side by side: nothing is malformed.
        let text = format!(
            "{}\n{V2_FIXTURE_LINE}\n{V3_FIXTURE_LINE}\n",
            line("f0", "host-a", "critical", "2026-09-27T09:00:00Z")
        );
        let batch = parse_findings(
            text.as_bytes(),
            false,
            &filter()?,
            &FindingLimits::default(),
        );
        assert_eq!(batch.skipped_malformed, 0);
        let ids: Vec<&str> = batch
            .findings
            .iter()
            .map(|f| f.finding_id.as_str())
            .collect();
        assert_eq!(
            ids,
            ["3f2b8c1e-9d4a-4b7e-8a6f-1c2d3e4f5a6b", "f-1", "f0"],
            "newest first"
        );
        let v1 = batch
            .findings
            .last()
            .ok_or(TestError::Missing("v1 finding"))?;
        assert_eq!(v1.schema_version, None);
        assert_eq!(v1.dedup_key, None);

        // The posture view stays v2-shaped for v1/v2 lines and carries the
        // three kept v3 fields for v3 lines.
        let v1_json = serde_json::to_string(v1).map_err(ctx("serialize v1"))?;
        assert!(!v1_json.contains("dedup_key") && !v1_json.contains("schema_version"));
        let newest = batch
            .findings
            .first()
            .ok_or(TestError::Missing("v3 finding"))?;
        let v3_json = serde_json::to_value(newest).map_err(ctx("serialize v3"))?;
        assert_eq!(
            v3_json
                .get("occurrences")
                .and_then(serde_json::Value::as_u64),
            Some(3)
        );
        assert!(v3_json.get("evidence_card").is_none());

        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("findings.jsonl");
        std::fs::write(&path, format!("{V3_FIXTURE_LINE}\n")).map_err(ctx("write export"))?;
        let batch = JsonlFindingSource::new(&path, FindingLimits::default())
            .recent(&filter()?)
            .map_err(ctx("recent"))?;
        assert_eq!(batch.findings.len(), 1);
        assert_eq!(batch.skipped_malformed, 0);
        Ok(())
    }

    #[test]
    fn test_malformed_optional_v3_values_are_dropped_not_the_line() -> TestResult {
        let text = [
            line("f1", "host-a", "high", "2026-09-27T01:00:00Z").replace(
                '}',
                r#","schema_version":3,"dedup_key":"NOT-HEX","occurrences":0}"#,
            ),
            line("f2", "host-a", "high", "2026-09-27T02:00:00Z")
                .replace('}', r#","schema_version":0}"#),
        ]
        .join("\n");
        let batch = parse_findings(
            text.as_bytes(),
            false,
            &filter()?,
            &FindingLimits::default(),
        );
        assert_eq!(batch.skipped_malformed, 1, "schema_version 0 is malformed");
        let finding = batch.findings.first().ok_or(TestError::Missing("f1"))?;
        assert_eq!(finding.finding_id.as_str(), "f1");
        assert_eq!(finding.schema_version, Some(3));
        assert_eq!(finding.dedup_key, None);
        assert_eq!(finding.occurrences, None);
        Ok(())
    }

    #[test]
    fn test_missing_file_and_directory_are_distinct_errors() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let missing = JsonlFindingSource::new(dir.path().join("absent"), FindingLimits::default());
        match missing.recent(&filter()?) {
            Err(FindingSourceError::NotFound(_)) => {}
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        let directory = JsonlFindingSource::new(dir.path(), FindingLimits::default());
        match directory.recent(&filter()?) {
            Err(FindingSourceError::NotAFile(_) | FindingSourceError::Io { .. }) => {}
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        Ok(())
    }

    /// Source of the canonical reader, embedded as text only: no Cargo
    /// dependency on and no link of the DoD client (module boundary), the
    /// same pattern `harw-agent-dsl` uses for `harw-registry-defaults` files.
    const DOD_CLIENT_FINDINGS_RS: &str = include_str!("../../harw-dod-client/src/findings.rs");

    /// The client's skew definition, exactly as `harw-dod-client` declares it
    /// (`Duration` is `std::time::Duration` there).
    const DOD_CLIENT_SKEW_DEFINITION: &str =
        "pub const MAX_CLOCK_SKEW: Duration = Duration::from_secs(5 * 60);";

    /// Drift guard for [`MAX_CLOCK_SKEW`]: the hub value is 5 min and the
    /// client source still declares exactly that bound. A change on either
    /// side fails here until both readers agree again.
    #[test]
    fn test_clock_skew_mirrors_the_dod_client_definition() -> TestResult {
        assert_eq!(MAX_CLOCK_SKEW, SignedDuration::from_secs(5 * 60));
        assert!(
            DOD_CLIENT_FINDINGS_RS.contains("use std::time::Duration;"),
            "harw-dod-client findings.rs no longer uses std::time::Duration"
        );
        assert!(
            DOD_CLIENT_FINDINGS_RS
                .lines()
                .any(|line| line.trim() == DOD_CLIENT_SKEW_DEFINITION),
            "harw_dod_client::findings::MAX_CLOCK_SKEW changed; update MAX_CLOCK_SKEW in harw-security-hub/src/findings.rs to the same bound"
        );
        Ok(())
    }

    /// Owner review PR #168: a line dated beyond `now + MAX_CLOCK_SKEW` is
    /// counted as malformed and dropped (also for other hosts and years
    /// ahead), while an in-skew line and a past line stay visible.
    #[test]
    fn test_record_beyond_clock_skew_is_counted_malformed_and_dropped() -> TestResult {
        let text = [
            line("past", "host-a", "high", "2026-09-27T11:00:00Z"),
            line("future", "host-a", "critical", "2026-09-27T12:05:01Z"),
            line("in-skew", "host-a", "high", "2026-09-27T12:02:00Z"),
            line("years-ahead", "host-a", "critical", "2029-09-27T12:00:00Z"),
            line(
                "other-host-future",
                "host-b",
                "critical",
                "2030-01-01T00:00:00Z",
            ),
        ]
        .join("\n");
        let batch = parse_findings(
            text.as_bytes(),
            false,
            &filter()?,
            &FindingLimits::default(),
        );
        let ids: Vec<&str> = batch
            .findings
            .iter()
            .map(|f| f.finding_id.as_str())
            .collect();
        assert_eq!(ids, ["in-skew", "past"]);
        assert_eq!(batch.skipped_malformed, 3, "three lines beyond the skew");
        assert_eq!(batch.skipped_oversize, 0);
        assert!(!batch.truncated);
        Ok(())
    }

    #[test]
    fn test_record_at_exact_skew_bound_is_kept() -> TestResult {
        let text = [
            line("at-bound", "host-a", "high", "2026-09-27T12:05:00Z"),
            line(
                "just-beyond",
                "host-a",
                "high",
                "2026-09-27T12:05:00.000000001Z",
            ),
        ]
        .join("\n");
        let batch = parse_findings(
            text.as_bytes(),
            false,
            &filter()?,
            &FindingLimits::default(),
        );
        let ids: Vec<&str> = batch
            .findings
            .iter()
            .map(|f| f.finding_id.as_str())
            .collect();
        assert_eq!(ids, ["at-bound"], "the bound is inclusive");
        assert_eq!(batch.skipped_malformed, 1);
        Ok(())
    }

    #[test]
    fn test_file_source_counts_future_dated_lines_as_malformed() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("findings.jsonl");
        let text = format!(
            "{}\n{}\n",
            line("in-skew", "host-a", "critical", "2026-09-27T12:04:59Z"),
            line("future", "host-a", "critical", "2027-09-27T12:00:00Z"),
        );
        std::fs::write(&path, text).map_err(ctx("write export"))?;
        let batch = JsonlFindingSource::new(&path, FindingLimits::default())
            .recent(&filter()?)
            .map_err(ctx("recent"))?;
        assert_eq!(batch.findings.len(), 1);
        assert_eq!(
            batch.findings.first().map(|f| f.finding_id.as_str()),
            Some("in-skew")
        );
        assert_eq!(batch.skipped_malformed, 1);
        Ok(())
    }

    #[test]
    fn test_latest_plausible_is_now_plus_skew_and_saturates() -> TestResult {
        let normal = filter()?;
        let expected = "2026-09-27T12:05:00Z"
            .parse::<Timestamp>()
            .map_err(ctx("bound"))?;
        assert_eq!(normal.latest_plausible(), expected);

        // At the end of the representable range the addition overflows;
        // the bound saturates instead of failing (no line is dropped for
        // being "too new" then).
        let at_max = FindingFilter {
            now: Timestamp::MAX,
            ..filter()?
        };
        assert_eq!(at_max.latest_plausible(), Timestamp::MAX);
        let text = line("late", "host-a", "high", "2026-09-27T13:00:00Z");
        let batch = parse_findings(text.as_bytes(), false, &at_max, &FindingLimits::default());
        assert_eq!(batch.findings.len(), 1);
        assert_eq!(batch.skipped_malformed, 0);
        Ok(())
    }
}
