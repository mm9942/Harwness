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
//! (see `dod/crates/harw-sentinel/src/findings.rs`, "Bruch 2"). This module
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
//! # Limits (the file is attacker-influenced input)
//! - only the last [`FindingLimits::max_tail_bytes`] of the file are read;
//!   when reading starts mid-file the first (partial) line is discarded,
//! - lines longer than [`FindingLimits::max_line_bytes`] are skipped unparsed,
//! - at most [`FindingLimits::max_records`] matching findings are returned
//!   (the newest ones),
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

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use harw_types::{FindingId, HostId, ImpactSeverity};

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
}

impl FindingFilter {
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
    /// Lines that were not valid finding records.
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
/// newline, because that line is (probably) cut off.
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
        if !filter.matches(&finding) {
            continue;
        }
        finding.summary = sanitize(&finding.summary, limits.max_summary_chars);
        finding.rule_id = sanitize(&finding.rule_id, limits.max_summary_chars);
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
        parse_findings,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_types::{HostId, ImpactSeverity};
    use jiff::Timestamp;

    fn filter() -> TestResult<FindingFilter> {
        Ok(FindingFilter {
            host: HostId::try_from_str("host-a").map_err(ctx("host id"))?,
            min_severity: ImpactSeverity::High,
            since: "2026-09-27T00:00:00Z"
                .parse::<Timestamp>()
                .map_err(ctx("since"))?,
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
}
