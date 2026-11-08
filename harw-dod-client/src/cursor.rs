//! Incremental reads of the findings export: [`FindingsCursor`].
//!
//! # Purpose
//! A consumer that polls the export (a watcher, the dispatch stage, a
//! notification view) must see every new line exactly once and must know
//! when lines were lost in between. Export v3 positions every line by the
//! sentinel `instance` (UUIDv4, new per start) and `seq` (monotonic per
//! instance, from 1); a [`FindingsCursor`] is the `(instance, seq)` of the
//! last line a consumer has processed.
//!
//! # Semantics of [`lines_after`]
//! The export is append-only and written by one sentinel process at a time,
//! so file order is write order across restarts. Given a cursor:
//!
//! - **Continuous:** the cursor's instance is in the tail and its oldest line
//!   there has `seq <= cursor.seq + 1`. Every line after the cursor's
//!   position is returned (lines of a later instance included), `gap` is
//!   `false`.
//! - **Gap:** the cursor's instance is not in the tail at all (rotated away,
//!   or a restart was not observed), or its oldest line there is newer than
//!   `cursor.seq + 1` (lines between were rotated away). Every line of the
//!   tail is returned and `gap` is `true`; consumers deduplicate by
//!   `finding_id` / `dedup_key`.
//! - **No cursor:** every line, `gap == false`.
//!
//! The next cursor is the position of the last v3 line of the tail; a tail
//! without v3 lines (v1/v2 export) keeps the given cursor.
//!
//! # Concurrency and errors
//! [`lines_after`] is pure. [`read_findings_since`] reads the export like
//! [`read_findings_tail`] (bounded, `<path>.1` included, symlinks refused)
//! and fails with the same [`DodClientError`]s.
//!
//! # Examples
//! ```
//! use harw_dod_client::{FindingsCursor, FindingsTail, lines_after};
//!
//! // Nothing read yet: everything, no gap.
//! let first = lines_after(FindingsTail::default(), None);
//! assert!(!first.gap && first.lines.is_empty());
//!
//! // A cursor whose instance is not in the export: report the gap.
//! let cursor = FindingsCursor {
//!     instance: "3f2b8c1e-9d4a-4b7e-8a6f-1c2d3e4f5a6b".to_owned(),
//!     seq: 7,
//! };
//! let next = lines_after(FindingsTail::default(), Some(&cursor));
//! assert!(next.gap);
//! assert_eq!(next.cursor, Some(cursor));
//! ```

use std::path::Path;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::DodClientError;
use crate::findings::{FindingLine, FindingsTail, read_findings_tail};

/// Position in the findings export: the last line a consumer processed.
///
/// Built from a v3 line with [`FindingsCursor::of`]; consumers persist it
/// between polls (it is serializable).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FindingsCursor {
    /// `instance` of the sentinel process that wrote the line (UUIDv4).
    pub instance: String,
    /// `seq` of the line within that instance (from 1).
    pub seq: u64,
}

impl FindingsCursor {
    /// The position of `line`; `None` for a line without `instance` and
    /// `seq` (v1/v2 lines).
    #[must_use]
    pub fn of(line: &FindingLine) -> Option<Self> {
        Some(Self {
            instance: line.instance.clone()?,
            seq: line.seq?,
        })
    }

    /// `seq` of `line` when it was written by this cursor's instance.
    fn seq_in_instance(&self, line: &FindingLine) -> Option<u64> {
        if line.instance.as_deref() == Some(self.instance.as_str()) {
            line.seq
        } else {
            None
        }
    }
}

/// The lines after a cursor ([`lines_after`], [`read_findings_since`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingsSince {
    /// New lines in file order, oldest first.
    pub lines: Vec<FindingLine>,
    /// `true` when lines between the cursor and [`lines`](Self::lines) may
    /// have been lost (rotation, unobserved restart); `lines` then holds the
    /// whole tail, already processed lines included.
    pub gap: bool,
    /// Where to continue: the position of the last v3 line read, else the
    /// cursor that was given.
    pub cursor: Option<FindingsCursor>,
    /// [`FindingsTail::truncated`] of the underlying read.
    pub truncated: bool,
}

/// Selects the lines of `tail` after `cursor` (see the module
/// documentation for continuity and gaps).
///
/// # Arguments
/// - `tail`: a read of the export, oldest line first.
/// - `cursor`: the last processed position; `None` on the first read.
///
/// # Returns
/// The new lines, the gap flag, the next cursor and the coverage of the
/// read.
#[must_use]
pub fn lines_after(tail: FindingsTail, cursor: Option<&FindingsCursor>) -> FindingsSince {
    let FindingsTail { lines, truncated } = tail;
    let next = lines
        .iter()
        .rev()
        .find_map(FindingsCursor::of)
        .or_else(|| cursor.cloned());
    let Some(cursor) = cursor else {
        return FindingsSince {
            lines,
            gap: false,
            cursor: next,
            truncated,
        };
    };
    // First line of the cursor's instance (index, seq) and the last line of
    // that instance at or before the cursor (index).
    let mut first: Option<(usize, u64)> = None;
    let mut resume_after: Option<usize> = None;
    for (index, line) in lines.iter().enumerate() {
        let Some(seq) = cursor.seq_in_instance(line) else {
            continue;
        };
        if first.is_none() {
            first = Some((index, seq));
        }
        if seq <= cursor.seq {
            resume_after = Some(index);
        }
    }
    let start = match first {
        // Lines between the cursor and the oldest line of its instance were
        // rotated away.
        Some((_, oldest)) if cursor.seq.saturating_add(1) < oldest => None,
        Some((first_index, _)) => Some(
            resume_after.map_or(first_index, |index| index.saturating_add(1)),
        ),
        // The cursor's instance is not in the tail at all.
        None => None,
    };
    match start {
        Some(start) => FindingsSince {
            lines: lines.into_iter().skip(start).collect(),
            gap: false,
            cursor: next,
            truncated,
        },
        None => FindingsSince {
            lines,
            gap: true,
            cursor: next,
            truncated,
        },
    }
}

/// Reads the findings export and returns the lines after `cursor`.
///
/// [`read_findings_tail`] (at most `max_bytes`, clamped to
/// [`MAX_TAIL_BYTES`](crate::findings::MAX_TAIL_BYTES), continuing into the
/// rotated `<path>.1`) followed by [`lines_after`]. A cursor whose lines were
/// rotated out of the readable tail yields the whole tail with
/// [`FindingsSince::gap`] set.
///
/// # Errors
/// The errors of [`read_findings_tail`]: [`DodClientError::NotFound`],
/// [`DodClientError::Symlink`], [`DodClientError::NotRegularFile`] and
/// [`DodClientError::Io`].
pub fn read_findings_since(
    path: &Path,
    cursor: Option<&FindingsCursor>,
    max_bytes: u64,
    now: OffsetDateTime,
) -> Result<FindingsSince, DodClientError> {
    read_findings_tail(path, max_bytes, now).map(|tail| lines_after(tail, cursor))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::findings::{MAX_TAIL_BYTES, parse_finding_line};
    use serde_json::json;
    use std::fs;
    use time::format_description::well_known::Rfc3339;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const OLD: &str = "a1b2c3d4-e5f6-4a7b-9c8d-0e1f2a3b4c5d";
    const NEW: &str = "b7e6d5c4-b3a2-4910-a8f7-e6d5c4b3a291";

    /// A v3 export line of `instance` at `seq`; the finding id encodes both.
    fn v3_text(instance: &str, seq: u64) -> String {
        let marker = if instance == OLD { 1_u64 } else { 2 };
        json!({
            "schema_version": 3,
            "finding_id": format!("5e1f0c2a-7b3d-4c9e-8f10-{marker:04x}{seq:08x}"),
            "host": "host-a",
            "rule_id": "egress-flow",
            "severity": "high",
            "summary": "s",
            "observed_at": "2026-10-02T11:00:00Z",
            "instance": instance,
            "seq": seq,
        })
        .to_string()
    }

    fn v3(instance: &str, seq: u64) -> Result<FindingLine, String> {
        parse_finding_line(&v3_text(instance, seq))
            .ok_or_else(|| format!("fixture {instance}/{seq} must parse"))
    }

    fn v1(id: &str) -> Result<FindingLine, String> {
        parse_finding_line(&format!(
            r#"{{"finding_id":"{id}","host":"host-a","rule_id":"r","severity":"low","summary":"s","observed_at":"2026-10-02T11:00:00Z"}}"#
        ))
        .ok_or_else(|| format!("fixture {id} must parse"))
    }

    fn tail(lines: Vec<FindingLine>) -> FindingsTail {
        FindingsTail {
            lines,
            truncated: false,
        }
    }

    fn cursor(instance: &str, seq: u64) -> FindingsCursor {
        FindingsCursor {
            instance: instance.to_owned(),
            seq,
        }
    }

    fn positions(since: &FindingsSince) -> Vec<(String, u64)> {
        since
            .lines
            .iter()
            .filter_map(FindingsCursor::of)
            .map(|c| (c.instance, c.seq))
            .collect()
    }

    fn pos(instance: &str, seq: u64) -> (String, u64) {
        (instance.to_owned(), seq)
    }

    #[test]
    fn without_cursor_everything_is_new_and_the_cursor_is_the_last_v3_line() -> TestResult {
        let since = lines_after(tail(vec![v3(OLD, 1)?, v3(OLD, 2)?, v1("f-1")?]), None);
        assert!(!since.gap);
        assert_eq!(since.lines.len(), 3);
        assert_eq!(since.cursor, Some(cursor(OLD, 2)));
        assert!(!since.truncated);
        Ok(())
    }

    #[test]
    fn continuous_cursor_returns_only_later_lines_including_a_new_instance() -> TestResult {
        let lines = vec![v3(OLD, 4)?, v3(OLD, 5)?, v3(OLD, 6)?, v3(NEW, 1)?, v3(NEW, 2)?];
        let since = lines_after(tail(lines.clone()), Some(&cursor(OLD, 5)));
        assert!(!since.gap);
        assert_eq!(
            positions(&since),
            [pos(OLD, 6), pos(NEW, 1), pos(NEW, 2)]
        );
        assert_eq!(since.cursor, Some(cursor(NEW, 2)));

        // Caught up: nothing new, the cursor stays.
        let since = lines_after(tail(lines.clone()), Some(&cursor(NEW, 2)));
        assert!(!since.gap);
        assert!(since.lines.is_empty());
        assert_eq!(since.cursor, Some(cursor(NEW, 2)));

        // The cursor sits right before the oldest line of its instance:
        // continuous, and the lines of an older instance before it are not
        // repeated.
        let lines = vec![v3(OLD, 9)?, v3(NEW, 3)?, v3(NEW, 4)?];
        let since = lines_after(tail(lines), Some(&cursor(NEW, 2)));
        assert!(!since.gap);
        assert_eq!(positions(&since), [pos(NEW, 3), pos(NEW, 4)]);
        Ok(())
    }

    #[test]
    fn rotated_away_cursor_returns_everything_with_a_gap() -> TestResult {
        let lines = vec![v3(OLD, 7)?, v3(OLD, 8)?, v3(NEW, 1)?];
        // Lines 4..=6 of the cursor's instance are gone.
        let since = lines_after(tail(lines.clone()), Some(&cursor(OLD, 3)));
        assert!(since.gap);
        assert_eq!(since.lines.len(), 3);
        assert_eq!(since.cursor, Some(cursor(NEW, 1)));

        // The cursor's instance is not in the tail at all.
        let gone = "c0ffee00-1234-4abc-9def-0123456789ab";
        let since = lines_after(tail(lines), Some(&cursor(gone, 40)));
        assert!(since.gap);
        assert_eq!(since.lines.len(), 3);
        assert_eq!(since.cursor, Some(cursor(NEW, 1)));
        Ok(())
    }

    #[test]
    fn v1_only_export_keeps_the_given_cursor_and_reports_the_gap() -> TestResult {
        let given = cursor(OLD, 3);
        let since = lines_after(tail(vec![v1("f-1")?, v1("f-2")?]), Some(&given));
        assert!(since.gap);
        assert_eq!(since.lines.len(), 2);
        assert_eq!(since.cursor, Some(given));
        // Truncation of the read is passed through.
        let since = lines_after(
            FindingsTail {
                lines: Vec::new(),
                truncated: true,
            },
            None,
        );
        assert!(since.truncated && !since.gap);
        assert_eq!(since.cursor, None);
        Ok(())
    }

    fn write_lines(path: &Path, lines: &[String]) -> std::io::Result<()> {
        let mut content = String::new();
        for line in lines {
            content.push_str(line);
            content.push('\n');
        }
        fs::write(path, content)
    }

    #[test]
    fn read_since_follows_the_cursor_across_a_rotation() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("findings.jsonl");
        let now = OffsetDateTime::parse("2026-10-02T12:00:00Z", &Rfc3339)?;
        write_lines(&path, &[v3_text(OLD, 1), v3_text(OLD, 2)])?;
        let first = read_findings_since(&path, None, MAX_TAIL_BYTES, now)?;
        assert_eq!(first.lines.len(), 2);
        let resume = first.cursor.ok_or("cursor after the first read")?;
        assert_eq!(resume, cursor(OLD, 2));

        // The sentinel rotates and keeps writing: the cursor's line is now in
        // `<path>.1`, the new lines are in the current file.
        fs::rename(&path, dir.path().join("findings.jsonl.1"))?;
        write_lines(&path, &[v3_text(OLD, 3), v3_text(NEW, 1)])?;
        let next = read_findings_since(&path, Some(&resume), MAX_TAIL_BYTES, now)?;
        assert!(!next.gap);
        assert!(!next.truncated);
        assert_eq!(positions(&next), [pos(OLD, 3), pos(NEW, 1)]);
        assert_eq!(next.cursor, Some(cursor(NEW, 1)));

        // A second rotation drops the generation holding the cursor's
        // instance: the gap is reported and the whole tail returned.
        fs::rename(dir.path().join("findings.jsonl.1"), dir.path().join("old"))?;
        fs::rename(&path, dir.path().join("findings.jsonl.1"))?;
        write_lines(&path, &[v3_text(NEW, 2)])?;
        let stale = read_findings_since(&path, Some(&cursor(OLD, 1)), MAX_TAIL_BYTES, now)?;
        assert!(stale.gap);
        assert_eq!(
            positions(&stale),
            [pos(OLD, 3), pos(NEW, 1), pos(NEW, 2)]
        );

        let missing = read_findings_since(&dir.path().join("absent"), None, MAX_TAIL_BYTES, now);
        assert!(missing.is_err_and(|error| error.is_not_found()));
        Ok(())
    }
}
