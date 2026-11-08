//! The findings export: line schema, bounded tail reader, windowed summary.
//!
//! # Line schema
//! v1 (current sentinel export, exactly six fields):
//!
//! ```json
//! {"finding_id":"f-1","host":"host-a","rule_id":"structure-drift","severity":"medium","summary":"...","observed_at":"2026-09-27T10:00:00Z"}
//! ```
//!
//! v2 adds two optional fields, additively: `schema_version` (`2`) and
//! `scope` (the cgroup path of the observed actor). Unknown fields are
//! ignored so later additive versions keep parsing.
//!
//! v3 (PL-100 WP0.12, `schema_version` `3`) is a strict superset of v2. Every
//! new field is optional for this reader, so v1 and v2 lines keep parsing:
//! `instance` (UUIDv4 of the sentinel process, new per start) and `seq`
//! (monotonic per instance, from 1) position a line for a
//! [`FindingsCursor`](crate::cursor::FindingsCursor); `record_digest`
//! (64 lowercase hex chars, the BLAKE3 digest of the `FindingRecord`),
//! `dedup_key` (32 lowercase hex chars), `occurrences` (≥ 1), `first_seen`
//! (RFC 3339), `kind` ([`FindingKind`]), `hardness` ([`FindingHardness`]),
//! `cgroup_kid` (kernel cgroup id), `scope_obfuscated` (the scope was
//! withheld because it could not be rendered safely) and `evidence_card`
//! ([`EvidenceCard`], at most 4 KiB). From v3 on `finding_id` must be the
//! canonical lowercase UUIDv4 the sentinel mints (`FindingId::new`); v1/v2
//! lines keep the older, looser id rule.
//!
//! Lines with a `schema_version` above [`EXPORT_SCHEMA_VERSION`] come from a
//! newer writer: an enum value or an `evidence_card` key this reader does not
//! know is then ignored instead of rejecting the line or the card.
//!
//! # Untrusted input
//! The export is written by the unprivileged sentinel from
//! attacker-influenced observations. The reader is bounded (tail of at most
//! [`MAX_TAIL_BYTES`], lines longer than [`MAX_LINE_BYTES`] skipped), refuses
//! symlinks, skips malformed lines instead of failing, sanitizes the free
//! text and rejects identifiers that violate the contract. Identifiers the
//! sentinel generates itself (`finding_id`, `instance`, `seq`,
//! `record_digest`, `dedup_key`) reject the line when malformed; values that
//! come from observations (`scope`, the `evidence_card` strings) are dropped
//! or escaped instead, so a hostile observation can never hide a finding. A
//! finding is evidence, never an instruction.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use harw_types::{FindingId, HostId, ImpactSeverity};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::DodClientError;
use crate::evidence::EvidenceCard;
use crate::fsio::{OpenedFile, is_clean, is_unsafe_char, open_regular_nofollow, sanitize};

/// Hard ceiling of the findings tail read, in bytes (256 KiB).
pub const MAX_TAIL_BYTES: u64 = 256 * 1024;

/// Longest export line that is parsed, in bytes (16 KiB).
pub const MAX_LINE_BYTES: usize = 16 * 1024;

/// Longest kept `summary`, in bytes (same limit as the sentinel export).
pub const MAX_SUMMARY_BYTES: usize = 512;

/// Longest accepted `finding_id` / `host`, in bytes.
pub const MAX_ID_BYTES: usize = 256;

/// Longest accepted `rule_id`, in bytes.
pub const MAX_RULE_ID_BYTES: usize = 128;

/// Longest kept `scope` (cgroup path), in bytes.
pub const MAX_SCOPE_BYTES: usize = 4096;

/// How far in the future (relative to the injected `now`) an
/// `observed_at` may lie before the line is treated as implausible.
pub const MAX_CLOCK_SKEW: Duration = Duration::from_secs(5 * 60);

/// Default summary window (24 h, the `[dod] findings_window_hours` default).
pub const DEFAULT_FINDINGS_WINDOW: Duration = Duration::from_secs(24 * 3600);

/// Most rules listed in a [`FindingsSummary`]; the rest is counted in
/// [`FindingsSummary::other_rules`].
pub const MAX_SUMMARY_RULES: usize = 32;

/// Highest export line schema version this reader knows.
///
/// Versions `1..=3` are parsed with every check of this module; a higher
/// version is still accepted, but unknown enum values and unknown
/// `evidence_card` keys are then ignored (see the module documentation).
pub const EXPORT_SCHEMA_VERSION: u32 = 3;

/// Length of a v3 `dedup_key`: lowercase hex of the first 16 bytes of a
/// BLAKE3 digest.
pub const DEDUP_KEY_HEX_LEN: usize = 32;

/// Length of a v3 `record_digest`: lowercase hex of a 32-byte BLAKE3 digest.
pub const RECORD_DIGEST_HEX_LEN: usize = 64;

/// v3 `kind`: why the finding exists.
///
/// Mirrors `harw_dod_rules::FindingKind`; the export spells it in
/// snake_case (`"rule_triggered"`, `"anomaly"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    /// A rule fired (may be escalated).
    RuleTriggered,
    /// A deviation from a baseline that is not established yet (notice only).
    Anomaly,
}

impl FindingKind {
    /// Parses the export spelling; `None` for anything else.
    ///
    /// # Examples
    /// ```
    /// use harw_dod_client::findings::FindingKind;
    ///
    /// assert_eq!(FindingKind::from_wire("anomaly"), Some(FindingKind::Anomaly));
    /// assert_eq!(FindingKind::from_wire("Anomaly"), None);
    /// ```
    #[must_use]
    pub fn from_wire(text: &str) -> Option<Self> {
        match text {
            "rule_triggered" => Some(Self::RuleTriggered),
            "anomaly" => Some(Self::Anomaly),
            _ => None,
        }
    }

    /// The export spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RuleTriggered => "rule_triggered",
            Self::Anomaly => "anomaly",
        }
    }
}

/// v3 `hardness`: how directly the finding is backed by observations.
///
/// Ordered like `harw_dod_signals::Hardness`: `Observed < Correlated <
/// Inferred` (the larger, the more indirect the evidence).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingHardness {
    /// Observed directly, without linking or inference.
    Observed,
    /// Linked from several observations.
    Correlated,
    /// Concluded beyond the observations.
    Inferred,
}

impl FindingHardness {
    /// Parses the export spelling; `None` for anything else.
    ///
    /// # Examples
    /// ```
    /// use harw_dod_client::findings::FindingHardness;
    ///
    /// assert_eq!(FindingHardness::from_wire("observed"), Some(FindingHardness::Observed));
    /// assert!(FindingHardness::Observed < FindingHardness::Inferred);
    /// ```
    #[must_use]
    pub fn from_wire(text: &str) -> Option<Self> {
        match text {
            "observed" => Some(Self::Observed),
            "correlated" => Some(Self::Correlated),
            "inferred" => Some(Self::Inferred),
            _ => None,
        }
    }

    /// The export spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Correlated => "correlated",
            Self::Inferred => "inferred",
        }
    }
}

/// `true` when `value` is a canonical UUIDv4 as `uuid::Uuid::new_v4()`
/// renders it (and therefore as `harw_types::FindingId::new` mints it):
/// 36 characters, lowercase hex, hyphens at 8/13/18/23, version nibble `4`,
/// RFC 4122 variant (`8`, `9`, `a` or `b`).
///
/// Uppercase, braced, URN and simple (hyphenless) spellings are refused, and
/// so is the nil UUID (its version nibble is `0`).
///
/// # Examples
/// ```
/// use harw_dod_client::findings::is_uuid_v4;
///
/// assert!(is_uuid_v4("3f2b8c1e-9d4a-4b7e-8a6f-1c2d3e4f5a6b"));
/// assert!(!is_uuid_v4("3F2B8C1E-9D4A-4B7E-8A6F-1C2D3E4F5A6B"));
/// assert!(!is_uuid_v4("00000000-0000-0000-0000-000000000000"));
/// assert!(!is_uuid_v4("f-1"));
/// ```
#[must_use]
pub fn is_uuid_v4(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 36
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            8 | 13 | 18 | 23 => *byte == b'-',
            14 => *byte == b'4',
            19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
            _ => matches!(byte, b'0'..=b'9' | b'a'..=b'f'),
        })
}

/// `true` when `value` is exactly `len` lowercase hex characters.
fn is_lower_hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// `skip_serializing_if` helper: `scope_obfuscated` is written only when set.
fn is_false(value: &bool) -> bool {
    !*value
}

/// One validated export line (v1, v2 or v3).
///
/// Built by [`parse_finding_line`] / deserialization, which validate every
/// field; the v3 fields are `None` / `false` on v1 and v2 lines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawFindingLine")]
pub struct FindingLine {
    /// `schema_version`; absent on v1 lines.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u32>,
    /// Stable id of the finding.
    pub finding_id: FindingId,
    /// Host the finding was observed on.
    pub host: HostId,
    /// Rule that produced the finding (ASCII `[A-Za-z0-9._:-]`).
    pub rule_id: String,
    /// Severity on the shared impact scale.
    pub severity: ImpactSeverity,
    /// Sanitized, bounded free text. Evidence, never an instruction.
    pub summary: String,
    /// When the sentinel observed the finding.
    #[serde(serialize_with = "time::serde::rfc3339::serialize")]
    pub observed_at: OffsetDateTime,
    /// v2: cgroup path of the observed actor, if attributed. Kept only when
    /// absolute, at most [`MAX_SCOPE_BYTES`] long and free of control/bidi
    /// characters; otherwise the line is kept without `scope`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// v3: UUIDv4 of the sentinel process instance that wrote the line (new
    /// per start). Validated with [`is_uuid_v4`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// v3: position of the line within [`instance`](Self::instance),
    /// monotonic, starting at 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// v3: lowercase hex of the 32-byte BLAKE3 digest of the
    /// `FindingRecord` ([`RECORD_DIGEST_HEX_LEN`] chars); `None` when the
    /// sentinel produced no record.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record_digest: Option<String>,
    /// v3: throttling key over rule, uid, cgroup id and coarse subject
    /// ([`DEDUP_KEY_HEX_LEN`] lowercase hex chars). Lines with the same key
    /// report repeats of one finding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dedup_key: Option<String>,
    /// v3: how often the [`dedup_key`](Self::dedup_key) fired (≥ 1).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub occurrences: Option<u64>,
    /// v3: when the [`dedup_key`](Self::dedup_key) was first seen.
    #[serde(
        serialize_with = "time::serde::rfc3339::option::serialize",
        skip_serializing_if = "Option::is_none"
    )]
    pub first_seen: Option<OffsetDateTime>,
    /// v3: why the finding exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<FindingKind>,
    /// v3: how directly the finding is backed by observations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hardness: Option<FindingHardness>,
    /// v3: kernel cgroup id of the observed actor. Kernel ids are reused;
    /// pair it with [`scope`](Self::scope) and
    /// [`first_seen`](Self::first_seen).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cgroup_kid: Option<u64>,
    /// `true` when a scope existed but was withheld because it could not be
    /// rendered safely: set by the v3 writer, and by this reader whenever it
    /// drops a non-empty `scope` it does not accept (any version).
    #[serde(skip_serializing_if = "is_false")]
    pub scope_obfuscated: bool,
    /// v3: bounded, escaped evidence summary; `None` when absent, larger than
    /// [`MAX_EVIDENCE_CARD_BYTES`](crate::evidence::MAX_EVIDENCE_CARD_BYTES)
    /// or malformed (the line itself is kept).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_card: Option<EvidenceCard>,
}

impl FindingLine {
    /// `true` for `high` and `critical` findings.
    #[must_use]
    pub fn is_high_or_critical(&self) -> bool {
        self.severity >= ImpactSeverity::High
    }

    /// `true` when [`finding_id`](Self::finding_id) is a canonical UUIDv4
    /// ([`is_uuid_v4`]). Always `true` for v3 lines; v1/v2 lines may carry
    /// older ids. Consumers that bind decisions to an id (dispatch, verdicts)
    /// skip lines without one.
    #[must_use]
    pub fn has_uuid_v4_id(&self) -> bool {
        is_uuid_v4(self.finding_id.as_str())
    }
}

/// The wire form before validation.
#[derive(Debug, Deserialize)]
struct RawFindingLine {
    #[serde(default)]
    schema_version: Option<u32>,
    finding_id: String,
    host: String,
    rule_id: String,
    severity: ImpactSeverity,
    summary: String,
    observed_at: String,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    instance: Option<String>,
    #[serde(default)]
    seq: Option<u64>,
    #[serde(default)]
    record_digest: Option<String>,
    #[serde(default)]
    dedup_key: Option<String>,
    #[serde(default)]
    occurrences: Option<u64>,
    #[serde(default)]
    first_seen: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    hardness: Option<String>,
    #[serde(default)]
    cgroup_kid: Option<u64>,
    #[serde(default)]
    scope_obfuscated: bool,
    #[serde(default)]
    evidence_card: Option<serde_json::Value>,
}

/// Why a raw line was rejected.
#[derive(Debug)]
pub struct InvalidFindingLine(&'static str);

impl std::fmt::Display for InvalidFindingLine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid findings export line: {}", self.0)
    }
}

impl std::error::Error for InvalidFindingLine {}

impl TryFrom<RawFindingLine> for FindingLine {
    type Error = InvalidFindingLine;

    fn try_from(raw: RawFindingLine) -> Result<Self, Self::Error> {
        if raw.schema_version == Some(0) {
            return Err(InvalidFindingLine("schema_version 0"));
        }
        // From v3 on the id is the UUIDv4 `FindingId::new` mints; v1/v2 lines
        // keep the older rule (non-empty, clean, bounded).
        let strict_ids = raw.schema_version.is_some_and(|version| version >= 3);
        // Only a writer newer than this reader may use values it does not
        // know yet; up to `EXPORT_SCHEMA_VERSION` they are contract violations.
        let lenient = raw
            .schema_version
            .is_some_and(|version| version > EXPORT_SCHEMA_VERSION);
        if !is_clean(&raw.finding_id, MAX_ID_BYTES) || (strict_ids && !is_uuid_v4(&raw.finding_id))
        {
            return Err(InvalidFindingLine("finding_id"));
        }
        if !is_clean(&raw.host, MAX_ID_BYTES) {
            return Err(InvalidFindingLine("host"));
        }
        if raw.rule_id.is_empty()
            || raw.rule_id.len() > MAX_RULE_ID_BYTES
            || !raw
                .rule_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
        {
            return Err(InvalidFindingLine("rule_id"));
        }
        let observed_at = OffsetDateTime::parse(&raw.observed_at, &Rfc3339)
            .map_err(|_| InvalidFindingLine("observed_at"))?;
        let finding_id = FindingId::try_from_str(raw.finding_id)
            .map_err(|_| InvalidFindingLine("finding_id"))?;
        let host = HostId::try_from_str(raw.host).map_err(|_| InvalidFindingLine("host"))?;
        // v3 identifiers are generated by the sentinel itself: a malformed
        // one means the line was not written by it, so the line is refused.
        let instance = match raw.instance {
            Some(instance) if !is_uuid_v4(&instance) => {
                return Err(InvalidFindingLine("instance"));
            }
            instance => instance,
        };
        if raw.seq == Some(0) {
            return Err(InvalidFindingLine("seq"));
        }
        let record_digest =
            lower_hex_field(raw.record_digest, RECORD_DIGEST_HEX_LEN, "record_digest")?;
        let dedup_key = lower_hex_field(raw.dedup_key, DEDUP_KEY_HEX_LEN, "dedup_key")?;
        if raw.occurrences == Some(0) {
            return Err(InvalidFindingLine("occurrences"));
        }
        let first_seen = raw
            .first_seen
            .map(|text| OffsetDateTime::parse(&text, &Rfc3339))
            .transpose()
            .map_err(|_| InvalidFindingLine("first_seen"))?;
        let kind = enum_field(raw.kind.as_deref(), FindingKind::from_wire, lenient, "kind")?;
        let hardness = enum_field(
            raw.hardness.as_deref(),
            FindingHardness::from_wire,
            lenient,
            "hardness",
        )?;
        // Same acceptance rule as the sentinel writer: an unacceptable scope
        // is dropped, never rewritten (a rewritten path names another cgroup).
        // Dropping a non-empty scope is recorded as `scope_obfuscated`, so the
        // withheld attribution stays visible instead of silently vanishing.
        let scope_given = raw.scope.as_deref().is_some_and(|scope| !scope.is_empty());
        let scope = raw.scope.filter(|scope| {
            scope.starts_with('/')
                && scope.len() <= MAX_SCOPE_BYTES
                && !scope.chars().any(is_unsafe_char)
        });
        let scope_obfuscated = raw.scope_obfuscated || (scope_given && scope.is_none());
        // Observation content: a card that is too large or malformed is
        // dropped, the finding itself is kept.
        let evidence_card = raw
            .evidence_card
            .and_then(|value| EvidenceCard::from_export_value(value, lenient));
        Ok(Self {
            schema_version: raw.schema_version,
            finding_id,
            host,
            rule_id: raw.rule_id,
            severity: raw.severity,
            summary: sanitize(&raw.summary, MAX_SUMMARY_BYTES, ' '),
            observed_at,
            scope,
            instance,
            seq: raw.seq,
            record_digest,
            dedup_key,
            occurrences: raw.occurrences,
            first_seen,
            kind,
            hardness,
            cgroup_kid: raw.cgroup_kid,
            scope_obfuscated,
            evidence_card,
        })
    }
}

/// An optional lowercase-hex identifier of exactly `len` chars; any other
/// present value rejects the line, naming `field`.
fn lower_hex_field(
    value: Option<String>,
    len: usize,
    field: &'static str,
) -> Result<Option<String>, InvalidFindingLine> {
    match value {
        Some(text) if !is_lower_hex(&text, len) => Err(InvalidFindingLine(field)),
        value => Ok(value),
    }
}

/// An optional enum spelled as in the export: unknown values reject the line
/// (naming `field`) unless the line comes from a newer writer (`lenient`),
/// where they are ignored.
fn enum_field<T>(
    value: Option<&str>,
    parse: fn(&str) -> Option<T>,
    lenient: bool,
    field: &'static str,
) -> Result<Option<T>, InvalidFindingLine> {
    let Some(text) = value else {
        return Ok(None);
    };
    match parse(text) {
        Some(parsed) => Ok(Some(parsed)),
        None if lenient => Ok(None),
        None => Err(InvalidFindingLine(field)),
    }
}

/// Parses one export line; `None` for anything that is not a valid line.
#[must_use]
pub fn parse_finding_line(line: &str) -> Option<FindingLine> {
    if line.len() > MAX_LINE_BYTES {
        return None;
    }
    serde_json::from_str(line).ok()
}

/// What [`read_findings_tail`] read, and whether it covers the export.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingsTail {
    /// Parsed lines in file order, oldest first: the newest rotated
    /// generation (`<path>.1`, if any of it was read) before the current
    /// file.
    pub lines: Vec<FindingLine>,
    /// `true` when export data exists that was not read: the read started
    /// inside the current file or inside `<path>.1`, an older generation
    /// (`<path>.2`, ...) exists, `<path>.1` could not be read, or the export
    /// kept rotating while it was read (a newer current file may then hold
    /// findings that are missing here). Counts over such a tail are lower
    /// bounds (see [`FindingsTail::summarize`]).
    pub truncated: bool,
}

impl FindingsTail {
    /// [`summarize`] over the tail, with [`FindingsSummary::complete`] set
    /// to `false` whenever the tail is [`truncated`](Self::truncated).
    ///
    /// The timestamps of the read lines are never taken as proof that the
    /// unread data lies outside the window: `observed_at` comes from the
    /// untrusted export and its append order is not validated, so an old
    /// timestamp in the read tail says nothing about the unread bytes before
    /// it. Only full, stable byte coverage of the export makes the counts
    /// exact.
    #[must_use]
    pub fn summarize(&self, window: Duration, now: OffsetDateTime) -> FindingsSummary {
        let mut summary = summarize(&self.lines, window, now);
        summary.complete = !self.truncated;
        summary
    }
}

fn window_start(window: Duration, now: OffsetDateTime) -> Option<OffsetDateTime> {
    time::Duration::try_from(window)
        .ok()
        .and_then(|window| now.checked_sub(window))
}

/// How often [`read_findings_tail`] reads the export when it rotates during
/// the read; the last attempt is returned as truncated.
const ROTATION_ATTEMPTS: u32 = 2;

/// Reads the tail of the findings export.
///
/// At most `max_bytes` (clamped to [`MAX_TAIL_BYTES`]) are read in total.
/// The budget is spent from the end of the current file first; when the
/// whole current file fits (typically right after the sentinel rotated it),
/// the rest of the budget is read from the end of the newest rotated
/// generation `<path>.1`. A line cut by a window start is dropped, lines
/// longer than [`MAX_LINE_BYTES`], non-UTF-8 lines and lines that do not
/// match the schema are skipped, and lines whose `observed_at` lies more
/// than [`MAX_CLOCK_SKEW`] after `now` are dropped as implausible (they
/// would otherwise pin every "latest" view). [`FindingsTail::truncated`]
/// tells whether export data was left unread.
///
/// When the export rotates during the read (`<path>.1` is the file just
/// read, or `<path>` names another file afterwards), the read is retried
/// once; if it rotates again the result is marked truncated, because the
/// new current file may hold newer findings. When the retry fails (for
/// example because the rotated export has no current file yet), the lines
/// of the earlier pass are returned as truncated instead of the error.
///
/// # Errors
/// [`DodClientError::NotFound`] when the export does not exist,
/// [`DodClientError::Symlink`] / [`DodClientError::NotRegularFile`] when
/// the path is refused, and [`DodClientError::Io`] for read failures of the
/// current file. A refused or unreadable `<path>.1` is not an error; the
/// tail is then marked truncated.
pub fn read_findings_tail(
    path: &Path,
    max_bytes: u64,
    now: OffsetDateTime,
) -> Result<FindingsTail, DodClientError> {
    read_findings_tail_with(path, max_bytes, now, |_| {})
}

/// Points inside one read pass where [`read_findings_tail_with`] runs its
/// hook (tests inject rotations there).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadStage {
    /// After the current file was read, before `<path>.1` is opened.
    BetweenOpens,
    /// After all reads of the pass, before the identity of `<path>` is
    /// checked again.
    BeforeStability,
}

/// [`read_findings_tail`] with a hook that runs at each [`ReadStage`].
fn read_findings_tail_with(
    path: &Path,
    max_bytes: u64,
    now: OffsetDateTime,
    mut hook: impl FnMut(ReadStage),
) -> Result<FindingsTail, DodClientError> {
    let limit = max_bytes.min(MAX_TAIL_BYTES);
    let mut attempt = 1;
    // The lines of the last rotated pass: a failing retry must not discard
    // them (nor report the export as missing and therefore empty).
    let mut previous: Option<FindingsTail> = None;
    loop {
        let pass = match read_once(path, limit, now, &mut hook) {
            Ok(pass) => pass,
            Err(error) => {
                return match previous {
                    Some(mut tail) => {
                        tail.truncated = true;
                        Ok(tail)
                    }
                    None => Err(error),
                };
            }
        };
        match pass {
            ReadOnce::Stable(tail) => return Ok(tail),
            ReadOnce::Rotated(mut tail) => {
                if attempt >= ROTATION_ATTEMPTS {
                    tail.truncated = true;
                    return Ok(tail);
                }
                previous = Some(tail);
                attempt += 1;
            }
        }
    }
}

/// One pass of [`read_findings_tail`].
enum ReadOnce {
    /// The export kept its identity across the pass.
    Stable(FindingsTail),
    /// The export rotated during the pass; newer data may be missing.
    Rotated(FindingsTail),
}

fn read_once(
    path: &Path,
    limit: u64,
    now: OffsetDateTime,
    hook: &mut impl FnMut(ReadStage),
) -> Result<ReadOnce, DodClientError> {
    let Some(current) = open_regular_nofollow(path)? else {
        return Err(DodClientError::NotFound {
            path: path.to_path_buf(),
        });
    };
    let current_identity = current.identity;
    let current_size = current.size;
    let newest = read_tail(path, current, limit)?;
    if newest.started_mid_file {
        let tail = FindingsTail {
            lines: parse_lines(&newest.body, now),
            truncated: true,
        };
        hook(ReadStage::BeforeStability);
        return Ok(stability(path, current_identity, tail));
    }
    hook(ReadStage::BetweenOpens);
    let rotated = generation_path(path, 1);
    let budget = limit.saturating_sub(current_size);
    let (mut lines, truncated) = match open_regular_nofollow(&rotated) {
        // Absent `<path>.1` is full coverage only when no older generation
        // exists: the sentinel shifts `.1` to `.2` before it renames `<path>`
        // to `.1`, so a read in between would otherwise skip a generation.
        Ok(None) => (
            Vec::new(),
            fs::symlink_metadata(generation_path(path, 2)).is_ok(),
        ),
        // The sentinel rotated between the two opens: `<path>.1` is the file
        // just read, and `<path>` now names a newer file that was not read.
        // Do not count the old file twice; the pass is retried.
        Ok(Some(older)) if older.identity == current_identity => {
            let tail = FindingsTail {
                lines: parse_lines(&newest.body, now),
                truncated: true,
            };
            return Ok(ReadOnce::Rotated(tail));
        }
        Ok(Some(older)) => match read_tail(&rotated, older, budget) {
            Ok(older) => (
                parse_lines(&older.body, now),
                older.started_mid_file || fs::symlink_metadata(generation_path(path, 2)).is_ok(),
            ),
            Err(_) => (Vec::new(), true),
        },
        Err(_) => (Vec::new(), true),
    };
    lines.extend(parse_lines(&newest.body, now));
    hook(ReadStage::BeforeStability);
    Ok(stability(
        path,
        current_identity,
        FindingsTail { lines, truncated },
    ))
}

/// [`ReadOnce::Stable`] when `<path>` still names the file read as current,
/// [`ReadOnce::Rotated`] otherwise (a rotation after the `<path>.1` open, or
/// the export vanished).
fn stability(path: &Path, identity: (u64, u64), tail: FindingsTail) -> ReadOnce {
    use std::os::unix::fs::MetadataExt as _;
    let unchanged = fs::symlink_metadata(path)
        .is_ok_and(|meta| meta.file_type().is_file() && (meta.dev(), meta.ino()) == identity);
    if unchanged {
        ReadOnce::Stable(tail)
    } else {
        ReadOnce::Rotated(tail)
    }
}

/// `<path>.<generation>`, the sentinel's rotation naming.
fn generation_path(path: &Path, generation: u32) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(format!(".{generation}"));
    PathBuf::from(name)
}

/// The bytes of a tail read, starting at a line boundary.
struct TailBytes {
    body: Vec<u8>,
    /// `true` when bytes before the window exist in this file.
    started_mid_file: bool,
}

/// Reads the last `limit` bytes of `opened` and drops a line cut by the
/// window start.
fn read_tail(path: &Path, opened: OpenedFile, limit: u64) -> Result<TailBytes, DodClientError> {
    let OpenedFile { mut file, size, .. } = opened;
    let start = size.saturating_sub(limit);
    // One byte before the window tells whether `start` is a line boundary.
    let (seek_to, extra) = if start > 0 { (start - 1, 1) } else { (0, 0) };
    file.seek(SeekFrom::Start(seek_to))
        .map_err(|error| DodClientError::io(path, error))?;
    // Never past the size seen at open: lines appended meanwhile belong to
    // the next read, and the total stays within the budget.
    let mut buf = Vec::new();
    file.take(size - start + extra)
        .read_to_end(&mut buf)
        .map_err(|error| DodClientError::io(path, error))?;
    let skip = if start == 0 {
        0
    } else if buf.first() == Some(&b'\n') {
        1
    } else {
        buf.iter()
            .position(|byte| *byte == b'\n')
            .map_or(buf.len(), |index| index + 1)
    };
    buf.drain(..skip);
    Ok(TailBytes {
        body: buf,
        started_mid_file: start > 0,
    })
}

/// Parses export bytes line by line (see [`read_findings_tail`]).
#[must_use]
pub fn parse_lines(bytes: &[u8], now: OffsetDateTime) -> Vec<FindingLine> {
    let latest_plausible = latest_plausible(now);
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|raw| !raw.is_empty() && raw.len() <= MAX_LINE_BYTES)
        .filter_map(|raw| std::str::from_utf8(raw).ok())
        .filter_map(|text| parse_finding_line(text.strip_suffix('\r').unwrap_or(text)))
        .filter(|line| latest_plausible.is_none_or(|bound| line.observed_at <= bound))
        .collect()
}

fn latest_plausible(now: OffsetDateTime) -> Option<OffsetDateTime> {
    time::Duration::try_from(MAX_CLOCK_SKEW)
        .ok()
        .and_then(|skew| now.checked_add(skew))
}

/// Finding counts per severity.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeverityCounts {
    /// `info` findings.
    pub info: u32,
    /// `low` findings.
    pub low: u32,
    /// `medium` findings.
    pub medium: u32,
    /// `high` findings.
    pub high: u32,
    /// `critical` findings.
    pub critical: u32,
}

impl SeverityCounts {
    /// Counts one finding of `severity`.
    pub fn add(&mut self, severity: ImpactSeverity) {
        let slot = match severity {
            ImpactSeverity::Info => &mut self.info,
            ImpactSeverity::Low => &mut self.low,
            ImpactSeverity::Medium => &mut self.medium,
            ImpactSeverity::High => &mut self.high,
            ImpactSeverity::Critical => &mut self.critical,
        };
        *slot = slot.saturating_add(1);
    }

    /// `high + critical`.
    #[must_use]
    pub const fn high_or_critical(&self) -> u32 {
        self.high.saturating_add(self.critical)
    }

    /// All findings counted.
    #[must_use]
    pub const fn total(&self) -> u32 {
        self.info
            .saturating_add(self.low)
            .saturating_add(self.medium)
            .saturating_add(self.high_or_critical())
    }
}

/// Counts of one rule inside the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleCount {
    /// The rule id.
    pub rule_id: String,
    /// Counts per severity for this rule.
    pub by_severity: SeverityCounts,
    /// Highest severity seen for this rule.
    pub max_severity: ImpactSeverity,
}

/// Per-rule and per-severity counts of the findings inside a window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FindingsSummary {
    /// Window length in seconds.
    pub window_secs: u64,
    /// The injected "now" the window ends at.
    #[serde(with = "time::serde::rfc3339")]
    pub as_of: OffsetDateTime,
    /// Counts across all rules.
    pub by_severity: SeverityCounts,
    /// Rules ordered by `high + critical` desc, total desc, rule id asc;
    /// at most [`MAX_SUMMARY_RULES`].
    pub by_rule: Vec<RuleCount>,
    /// Number of further rules not listed in `by_rule`.
    pub other_rules: u32,
    /// Newest `observed_at` inside the window.
    #[serde(with = "time::serde::rfc3339::option")]
    pub latest_observed_at: Option<OffsetDateTime>,
    /// `false` when export data that may fall inside the window was not
    /// read (bounded tail, rotation, unreadable export): every count is then
    /// a lower bound and should be shown as such (`≥ n`, "partial").
    pub complete: bool,
}

impl FindingsSummary {
    /// An empty summary (no export, no findings).
    #[must_use]
    pub fn empty(window: Duration, now: OffsetDateTime) -> Self {
        Self {
            window_secs: window.as_secs(),
            as_of: now,
            by_severity: SeverityCounts::default(),
            by_rule: Vec::new(),
            other_rules: 0,
            latest_observed_at: None,
            complete: true,
        }
    }

    /// All findings inside the window.
    #[must_use]
    pub const fn total(&self) -> u32 {
        self.by_severity.total()
    }

    /// `high + critical` findings inside the window.
    #[must_use]
    pub const fn high_or_critical(&self) -> u32 {
        self.by_severity.high_or_critical()
    }
}

/// Summarizes the findings observed in `[now - window, now + MAX_CLOCK_SKEW]`.
///
/// `lines` is taken as every finding there is ([`FindingsSummary::complete`]
/// is `true`); summarize a bounded read with [`FindingsTail::summarize`]
/// instead, which reports missing coverage.
#[must_use]
pub fn summarize(lines: &[FindingLine], window: Duration, now: OffsetDateTime) -> FindingsSummary {
    let earliest = window_start(window, now);
    let latest = latest_plausible(now);
    let mut summary = FindingsSummary::empty(window, now);
    let mut rules: BTreeMap<&str, RuleCount> = BTreeMap::new();
    for line in lines {
        if earliest.is_some_and(|bound| line.observed_at < bound)
            || latest.is_some_and(|bound| line.observed_at > bound)
        {
            continue;
        }
        summary.by_severity.add(line.severity);
        summary.latest_observed_at = Some(
            summary
                .latest_observed_at
                .map_or(line.observed_at, |seen| seen.max(line.observed_at)),
        );
        let rule = rules
            .entry(line.rule_id.as_str())
            .or_insert_with(|| RuleCount {
                rule_id: line.rule_id.clone(),
                by_severity: SeverityCounts::default(),
                max_severity: line.severity,
            });
        rule.by_severity.add(line.severity);
        rule.max_severity = rule.max_severity.max(line.severity);
    }
    let mut ordered: Vec<RuleCount> = rules.into_values().collect();
    ordered.sort_by(|a, b| {
        b.by_severity
            .high_or_critical()
            .cmp(&a.by_severity.high_or_critical())
            .then_with(|| b.by_severity.total().cmp(&a.by_severity.total()))
            .then_with(|| a.rule_id.cmp(&b.rule_id))
    });
    if ordered.len() > MAX_SUMMARY_RULES {
        summary.other_rules = u32::try_from(ordered.len() - MAX_SUMMARY_RULES).unwrap_or(u32::MAX);
        ordered.truncate(MAX_SUMMARY_RULES);
    }
    summary.by_rule = ordered;
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::fs;
    use std::io::Write as _;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn at(text: &str) -> Result<OffsetDateTime, time::error::Parse> {
        OffsetDateTime::parse(text, &Rfc3339)
    }

    fn v1(id: &str, rule: &str, severity: &str, observed_at: &str) -> String {
        format!(
            r#"{{"finding_id":"{id}","host":"host-a","rule_id":"{rule}","severity":"{severity}","summary":"s","observed_at":"{observed_at}"}}"#
        )
    }

    #[test]
    fn accepts_v1_and_v2_lines_and_ignores_unknown_fields() -> TestResult {
        let v1_line = r#"{"finding_id":"f-1","host":"host-a","rule_id":"structure-drift","severity":"medium","summary":"drift","observed_at":"2026-09-27T10:00:00Z"}"#;
        let parsed = parse_finding_line(v1_line).ok_or("v1 line must parse")?;
        assert_eq!(parsed.schema_version, None);
        assert_eq!(parsed.scope, None);
        assert_eq!(parsed.rule_id, "structure-drift");
        assert_eq!(parsed.severity, ImpactSeverity::Medium);
        assert_eq!(parsed.observed_at, at("2026-09-27T10:00:00Z")?);

        let v2_line = r#"{"schema_version":2,"finding_id":"f-2","host":"host-a","rule_id":"egress-unexpected","severity":"high","summary":"tcp","observed_at":"2026-09-27T10:00:00Z","scope":"/harw.slice/harw-job-1","future_field":true}"#;
        let parsed = parse_finding_line(v2_line).ok_or("v2 line must parse")?;
        assert_eq!(parsed.schema_version, Some(2));
        assert_eq!(parsed.scope.as_deref(), Some("/harw.slice/harw-job-1"));
        assert!(parsed.is_high_or_critical());

        // Round trip through the serialized form keeps the line identical.
        let json = serde_json::to_string(&parsed)?;
        let again: FindingLine = serde_json::from_str(&json)?;
        assert_eq!(again, parsed);
        let v1_json = serde_json::to_string(&parse_finding_line(v1_line).ok_or("v1")?)?;
        assert!(!v1_json.contains("schema_version") && !v1_json.contains("scope"));
        Ok(())
    }

    #[test]
    fn rejects_contract_violations_and_sanitizes_free_text() {
        for bad in [
            r#"{"finding_id":" ","host":"h","rule_id":"r","severity":"low","summary":"s","observed_at":"2026-09-27T10:00:00Z"}"#,
            r#"{"finding_id":"f","host":"h\u0007","rule_id":"r","severity":"low","summary":"s","observed_at":"2026-09-27T10:00:00Z"}"#,
            r#"{"finding_id":"f","host":"h","rule_id":"a b","severity":"low","summary":"s","observed_at":"2026-09-27T10:00:00Z"}"#,
            r#"{"finding_id":"f","host":"h","rule_id":"r","severity":"severe","summary":"s","observed_at":"2026-09-27T10:00:00Z"}"#,
            r#"{"finding_id":"f","host":"h","rule_id":"r","severity":"low","summary":"s","observed_at":"yesterday"}"#,
            r#"{"finding_id":"f","host":"h","rule_id":"r","severity":"low","observed_at":"2026-09-27T10:00:00Z"}"#,
            r#"{"schema_version":0,"finding_id":"f","host":"h","rule_id":"r","severity":"low","summary":"s","observed_at":"2026-09-27T10:00:00Z"}"#,
            "not json",
        ] {
            assert!(parse_finding_line(bad).is_none(), "accepted: {bad}");
        }
        let hostile = r#"{"finding_id":"f","host":"h","rule_id":"r","severity":"low","summary":"IGNORE\nPREVIOUS\u202Einstructions","observed_at":"2026-09-27T10:00:00Z","scope":"/harw.slice/harw-job-1/\u001b[31m"}"#;
        let line = parse_finding_line(hostile);
        assert_eq!(
            line.as_ref().map(|l| l.summary.as_str()),
            Some("IGNORE PREVIOUS instructions")
        );
        // An unsafe or relative scope is dropped, the line itself is kept.
        assert_eq!(line.map(|l| l.scope), Some(None));
        let relative = r#"{"finding_id":"f","host":"h","rule_id":"r","severity":"low","summary":"s","observed_at":"2026-09-27T10:00:00Z","scope":"harw.slice/harw-job-1"}"#;
        assert_eq!(parse_finding_line(relative).map(|l| l.scope), Some(None));
        let long_summary = format!(
            r#"{{"finding_id":"f","host":"h","rule_id":"r","severity":"low","summary":"{}","observed_at":"2026-09-27T10:00:00Z"}}"#,
            "x".repeat(2000)
        );
        assert_eq!(
            parse_finding_line(&long_summary).map(|l| l.summary.len()),
            Some(MAX_SUMMARY_BYTES)
        );
    }

    #[test]
    fn tail_is_bounded_and_drops_the_cut_line() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("findings.jsonl");
        let now = at("2026-10-01T12:00:00Z")?;
        let mut file = fs::File::create(&path)?;
        let mut written = 0_u64;
        let mut count = 0_u32;
        while written < 2 * MAX_TAIL_BYTES {
            // Fixed-width ids keep every line the same length.
            let line = v1(&format!("f-{count:06}"), "r", "low", "2026-10-01T11:00:00Z");
            writeln!(file, "{line}")?;
            written += line.len() as u64 + 1;
            count += 1;
        }
        drop(file);
        let line_len = v1("f-000000", "r", "low", "2026-10-01T11:00:00Z").len() as u64 + 1;

        // Asking for more than the ceiling is clamped to MAX_TAIL_BYTES.
        let tail = read_findings_tail(&path, u64::MAX, now)?;
        assert!(tail.truncated);
        let lines = tail.lines;
        let max_lines = MAX_TAIL_BYTES / line_len + 1;
        assert!(!lines.is_empty());
        assert!(lines.len() as u64 <= max_lines, "{} lines", lines.len());
        assert_eq!(
            lines.last().map(|l| l.finding_id.as_str().to_owned()),
            Some(format!("f-{:06}", count - 1))
        );

        // A small window that starts mid-line drops the partial line.
        let small = read_findings_tail(&path, line_len + line_len / 2, now)?;
        assert_eq!(small.lines.len(), 1);
        // A window that starts exactly on a line boundary keeps that line.
        let exact = read_findings_tail(&path, 2 * line_len, now)?;
        assert_eq!(exact.lines.len(), 2);
        assert!(exact.truncated);
        Ok(())
    }

    #[test]
    fn overlong_lines_future_lines_and_garbage_are_skipped() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("findings.jsonl");
        let now = at("2026-10-01T12:00:00Z")?;
        let huge = format!(
            r#"{{"finding_id":"f-huge","host":"h","rule_id":"r","severity":"high","summary":"{}","observed_at":"2026-10-01T11:00:00Z"}}"#,
            "x".repeat(MAX_LINE_BYTES)
        );
        let mut content = Vec::new();
        for line in [
            v1("f-1", "r", "high", "2026-10-01T11:00:00Z"),
            huge,
            v1("f-future", "r", "critical", "2026-10-01T12:10:00Z"),
            v1("f-skew-ok", "r", "low", "2026-10-01T12:04:00Z"),
            "garbage".to_owned(),
        ] {
            content.extend_from_slice(line.as_bytes());
            content.push(b'\n');
        }
        content.extend_from_slice(&[0xff, 0xfe, b'\n']);
        content.extend_from_slice(v1("f-crlf", "r", "low", "2026-10-01T11:00:00Z").as_bytes());
        content.extend_from_slice(b"\r\n");
        // A trailing partial line (writer mid-append) is skipped.
        content.extend_from_slice(br#"{"finding_id":"f-partial","host":"#);
        fs::write(&path, content)?;
        let tail = read_findings_tail(&path, MAX_TAIL_BYTES, now)?;
        assert!(!tail.truncated);
        let ids: Vec<String> = tail
            .lines
            .into_iter()
            .map(|l| l.finding_id.as_str().to_owned())
            .collect();
        assert_eq!(ids, ["f-1", "f-skew-ok", "f-crlf"]);
        Ok(())
    }

    #[test]
    fn tail_refuses_symlinks_and_reports_missing() -> TestResult {
        let dir = tempfile::tempdir()?;
        let now = at("2026-10-01T12:00:00Z")?;
        let target = dir.path().join("real.jsonl");
        fs::write(&target, v1("f-1", "r", "low", "2026-10-01T11:00:00Z"))?;
        let link = dir.path().join("findings.jsonl");
        std::os::unix::fs::symlink(&target, &link)?;
        assert!(matches!(
            read_findings_tail(&link, MAX_TAIL_BYTES, now),
            Err(DodClientError::Symlink { .. })
        ));
        let missing = read_findings_tail(&dir.path().join("absent"), MAX_TAIL_BYTES, now);
        assert!(missing.is_err_and(|error| error.is_not_found()));
        assert_eq!(
            read_findings_tail(&target, MAX_TAIL_BYTES, now)?
                .lines
                .len(),
            1
        );
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

    fn ids(tail: &FindingsTail) -> Vec<&str> {
        tail.lines.iter().map(|l| l.finding_id.as_str()).collect()
    }

    #[test]
    fn tail_reads_the_rotated_generation_and_reports_coverage() -> TestResult {
        let dir = tempfile::tempdir()?;
        let now = at("2026-10-01T12:00:00Z")?;
        let path = dir.path().join("findings.jsonl");
        let rotated = dir.path().join("findings.jsonl.1");
        write_lines(&path, &[v1("new-1", "r", "low", "2026-10-01T11:30:00Z")])?;

        // No rotated generation: the whole export was read.
        let tail = read_findings_tail(&path, MAX_TAIL_BYTES, now)?;
        assert_eq!(ids(&tail), ["new-1"]);
        assert!(!tail.truncated);

        // Right after a rotation the older lines come from `<path>.1`,
        // oldest first.
        write_lines(
            &rotated,
            &[
                v1("old-1", "r", "high", "2026-10-01T10:00:00Z"),
                v1("old-2", "r", "critical", "2026-10-01T10:30:00Z"),
            ],
        )?;
        let tail = read_findings_tail(&path, MAX_TAIL_BYTES, now)?;
        assert_eq!(ids(&tail), ["old-1", "old-2", "new-1"]);
        assert!(!tail.truncated);
        assert_eq!(
            tail.summarize(DEFAULT_FINDINGS_WINDOW, now)
                .high_or_critical(),
            2
        );

        // A budget that ends inside `<path>.1` keeps its newest line only.
        let current_len = fs::metadata(&path)?.len();
        let old_len = v1("old-2", "r", "critical", "2026-10-01T10:30:00Z").len() as u64 + 1;
        let tail = read_findings_tail(&path, current_len + old_len, now)?;
        assert_eq!(ids(&tail), ["old-2", "new-1"]);
        assert!(tail.truncated);
        // No budget left for `<path>.1` at all.
        let tail = read_findings_tail(&path, current_len, now)?;
        assert_eq!(ids(&tail), ["new-1"]);
        assert!(tail.truncated);

        // An older generation beyond `<path>.1` is unread data as well.
        fs::write(dir.path().join("findings.jsonl.2"), "")?;
        assert!(read_findings_tail(&path, MAX_TAIL_BYTES, now)?.truncated);
        fs::remove_file(dir.path().join("findings.jsonl.2"))?;

        // A refused `<path>.1` does not fail the read; it is unread data.
        fs::remove_file(&rotated)?;
        std::os::unix::fs::symlink(&path, &rotated)?;
        let tail = read_findings_tail(&path, MAX_TAIL_BYTES, now)?;
        assert_eq!(ids(&tail), ["new-1"]);
        assert!(tail.truncated);
        Ok(())
    }

    #[test]
    fn tail_summary_is_partial_only_when_unread_data_may_be_in_the_window() -> TestResult {
        let now = at("2026-10-02T12:00:00Z")?;
        let line = |id: &str, observed_at: &str| {
            parse_finding_line(&v1(id, "r", "high", observed_at)).ok_or("fixture must parse")
        };
        let recent = line("recent", "2026-10-02T11:00:00Z")?;
        let old = line("old", "2026-10-01T06:00:00Z")?;

        let whole = FindingsTail {
            lines: vec![recent.clone()],
            truncated: false,
        };
        assert!(whole.summarize(DEFAULT_FINDINGS_WINDOW, now).complete);

        // Truncated and every read line is inside the window: older unread
        // lines may be inside it too, so the count is a lower bound.
        let partial = FindingsTail {
            lines: vec![recent.clone()],
            truncated: true,
        };
        let summary = partial.summarize(DEFAULT_FINDINGS_WINDOW, now);
        assert!(!summary.complete);
        assert_eq!(summary.high_or_critical(), 1);
        let empty = FindingsTail {
            lines: Vec::new(),
            truncated: true,
        };
        assert!(!empty.summarize(DEFAULT_FINDINGS_WINDOW, now).complete);

        // Truncated, even though a read line lies before the window start:
        // export timestamps are untrusted and unordered, so an old line in
        // the tail proves nothing about the unread bytes before it.
        let covered = FindingsTail {
            lines: vec![old, recent.clone()],
            truncated: true,
        };
        let summary = covered.summarize(DEFAULT_FINDINGS_WINDOW, now);
        assert!(!summary.complete);
        assert_eq!(summary.high_or_critical(), 1);

        // A bare slice is taken as complete.
        assert!(summarize(&[recent], DEFAULT_FINDINGS_WINDOW, now).complete);
        Ok(())
    }

    #[test]
    fn out_of_order_old_timestamp_after_unread_recent_findings_stays_partial() -> TestResult {
        let dir = tempfile::tempdir()?;
        let now = at("2026-10-02T12:00:00Z")?;
        let path = dir.path().join("findings.jsonl");
        let unread = v1("unread-recent", "r", "critical", "2026-10-02T11:00:00Z");
        let old = v1("old-stamp", "r", "low", "2026-09-01T00:00:00Z");
        let recent = v1("recent", "r", "high", "2026-10-02T11:30:00Z");
        write_lines(&path, &[unread, old.clone(), recent.clone()])?;
        // The budget covers the last two lines only.
        let budget = (old.len() + recent.len() + 2) as u64;
        let tail = read_findings_tail(&path, budget, now)?;
        assert_eq!(ids(&tail), ["old-stamp", "recent"]);
        assert!(tail.truncated);
        let summary = tail.summarize(DEFAULT_FINDINGS_WINDOW, now);
        assert!(!summary.complete, "a lower bound must not read as exact");
        assert_eq!(summary.high_or_critical(), 1);
        Ok(())
    }

    /// The sentinel's rotation: `<path>` becomes `<path>.1`, a new current
    /// file is started with `fresh`.
    fn rotate(path: &Path, fresh: &[String]) -> std::io::Result<()> {
        fs::rename(path, generation_path(path, 1))?;
        write_lines(path, fresh)
    }

    #[test]
    fn rotation_between_the_two_opens_is_retried_or_stays_partial() -> TestResult {
        let dir = tempfile::tempdir()?;
        let now = at("2026-10-02T12:00:00Z")?;
        let path = dir.path().join("findings.jsonl");
        write_lines(&path, &[v1("a", "r", "low", "2026-10-02T10:00:00Z")])?;

        // One rotation between the opens: the retry reads the new
        // generation pair and covers the whole export.
        let mut rotations = 0_u32;
        let mut result = Ok(());
        let tail = read_findings_tail_with(&path, MAX_TAIL_BYTES, now, |stage| {
            if stage != ReadStage::BetweenOpens {
                return;
            }
            if rotations == 0 {
                result = rotate(&path, &[v1("b", "r", "critical", "2026-10-02T11:00:00Z")]);
            }
            rotations += 1;
        })?;
        result?;
        assert_eq!(ids(&tail), ["a", "b"]);
        assert!(!tail.truncated);
        assert!(tail.summarize(DEFAULT_FINDINGS_WINDOW, now).complete);

        // The export keeps rotating: the newer current file was never read,
        // so the summary stays partial.
        let mut generation = 0_u32;
        let mut result = Ok(());
        let tail = read_findings_tail_with(&path, MAX_TAIL_BYTES, now, |stage| {
            if stage != ReadStage::BetweenOpens {
                return;
            }
            generation += 1;
            let id = format!("c-{generation}");
            if result.is_ok() {
                result = rotate(&path, &[v1(&id, "r", "critical", "2026-10-02T11:30:00Z")]);
            }
        })?;
        result?;
        assert!(tail.truncated);
        let summary = tail.summarize(DEFAULT_FINDINGS_WINDOW, now);
        assert!(!summary.complete);
        // The file read as current is not counted twice.
        let mut seen = ids(&tail);
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), tail.lines.len());
        Ok(())
    }

    /// Asserts that no finding id appears twice in `tail`.
    fn assert_no_duplicates(tail: &FindingsTail) {
        let mut seen = ids(tail);
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), tail.lines.len());
    }

    #[test]
    fn rotation_before_the_stability_check_is_retried_or_stays_partial() -> TestResult {
        let now = at("2026-10-02T12:00:00Z")?;
        // Whole-file path (`<path>.1` is read) and mid-file path (the budget
        // ends inside the current file).
        for mid_file in [false, true] {
            let dir = tempfile::tempdir()?;
            let path = dir.path().join("findings.jsonl");
            let first = [
                v1("a-1", "r", "low", "2026-10-02T10:00:00Z"),
                v1("a-2", "r", "low", "2026-10-02T10:10:00Z"),
            ];
            write_lines(&path, &first)?;
            // Mid-file: more than one line, less than the whole file.
            let budget = if mid_file {
                first[1].len() as u64 + 12
            } else {
                MAX_TAIL_BYTES
            };

            // One rotation after the reads: the retry reads the new pair.
            let mut rotations = 0_u32;
            let mut result = Ok(());
            let tail = read_findings_tail_with(&path, budget, now, |stage| {
                if stage == ReadStage::BeforeStability && rotations == 0 {
                    rotations += 1;
                    result = rotate(&path, &[v1("b", "r", "critical", "2026-10-02T11:00:00Z")]);
                }
            })?;
            result?;
            assert_eq!(rotations, 1);
            assert_no_duplicates(&tail);
            if mid_file {
                // The new current file fits, `<path>.1` is then cut.
                assert!(ids(&tail).contains(&"b"));
                assert!(tail.truncated);
            } else {
                assert_eq!(ids(&tail), ["a-1", "a-2", "b"]);
                assert!(!tail.truncated);
                assert!(tail.summarize(DEFAULT_FINDINGS_WINDOW, now).complete);
            }

            // Rotation on every pass: newer data may be missing.
            let mut generation = 0_u32;
            let mut result = Ok(());
            let tail = read_findings_tail_with(&path, budget, now, |stage| {
                if stage == ReadStage::BeforeStability && result.is_ok() {
                    generation += 1;
                    let id = format!("c-{generation}");
                    result = rotate(&path, &[v1(&id, "r", "critical", "2026-10-02T11:30:00Z")]);
                }
            })?;
            result?;
            assert_eq!(generation, ROTATION_ATTEMPTS);
            assert!(tail.truncated);
            assert!(!tail.summarize(DEFAULT_FINDINGS_WINDOW, now).complete);
            assert_no_duplicates(&tail);
        }
        Ok(())
    }

    #[test]
    fn export_vanishing_during_the_read_keeps_the_earlier_tail_partial() -> TestResult {
        let now = at("2026-10-02T12:00:00Z")?;
        for stage_to_hit in [ReadStage::BetweenOpens, ReadStage::BeforeStability] {
            let dir = tempfile::tempdir()?;
            let path = dir.path().join("findings.jsonl");
            write_lines(&path, &[v1("a", "r", "critical", "2026-10-02T11:00:00Z")])?;
            // The sentinel's rotate() renames `<path>` to `.1` and creates the
            // new current file only on the next append.
            let mut result = Ok(());
            let mut done = false;
            let tail = read_findings_tail_with(&path, MAX_TAIL_BYTES, now, |stage| {
                if stage == stage_to_hit && !done {
                    done = true;
                    result = fs::rename(&path, generation_path(&path, 1));
                }
            })?;
            result?;
            assert!(done);
            assert_eq!(ids(&tail), ["a"]);
            assert!(tail.truncated);
            let summary = tail.summarize(DEFAULT_FINDINGS_WINDOW, now);
            assert!(!summary.complete);
            assert_eq!(summary.high_or_critical(), 1);
        }

        // Plain removal of the export during the read: also partial, not an
        // exact empty summary.
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("findings.jsonl");
        write_lines(&path, &[v1("a", "r", "critical", "2026-10-02T11:00:00Z")])?;
        let mut result = Ok(());
        let tail = read_findings_tail_with(&path, MAX_TAIL_BYTES, now, |stage| {
            if stage == ReadStage::BeforeStability && path.exists() {
                result = fs::remove_file(&path);
            }
        })?;
        result?;
        assert!(tail.truncated);
        assert!(!tail.summarize(DEFAULT_FINDINGS_WINDOW, now).complete);
        Ok(())
    }

    #[test]
    fn generation_shift_between_the_opens_stays_partial() -> TestResult {
        let dir = tempfile::tempdir()?;
        let now = at("2026-10-02T12:00:00Z")?;
        let path = dir.path().join("findings.jsonl");
        write_lines(&path, &[v1("new", "r", "low", "2026-10-02T11:00:00Z")])?;
        write_lines(
            &generation_path(&path, 1),
            &[v1("older", "r", "critical", "2026-10-02T10:00:00Z")],
        )?;
        // The sentinel shifted `.1` to `.2` but has not renamed `<path>` yet.
        let mut result = Ok(());
        let tail = read_findings_tail_with(&path, MAX_TAIL_BYTES, now, |stage| {
            if stage == ReadStage::BetweenOpens && result.is_ok() {
                result = fs::rename(generation_path(&path, 1), generation_path(&path, 2));
            }
        })?;
        result?;
        assert_eq!(ids(&tail), ["new"]);
        assert!(tail.truncated);
        assert!(!tail.summarize(DEFAULT_FINDINGS_WINDOW, now).complete);
        Ok(())
    }

    #[test]
    fn summary_windows_with_the_injected_now_and_orders_rules() -> TestResult {
        let now = at("2026-10-02T12:00:00Z")?;
        let lines: Vec<FindingLine> = [
            v1("a", "structure-drift", "high", "2026-10-02T10:00:00Z"),
            v1("b", "structure-drift", "critical", "2026-10-02T11:00:00Z"),
            v1("c", "structure-drift", "low", "2026-10-02T11:30:00Z"),
            v1("d", "egress", "medium", "2026-10-02T11:59:00Z"),
            v1("e", "egress", "medium", "2026-10-02T11:58:00Z"),
            v1("f", "egress", "medium", "2026-10-02T11:57:00Z"),
            v1("g", "old-rule", "critical", "2026-09-01T11:00:00Z"),
            v1("h", "edge", "info", "2026-10-01T12:00:00Z"),
            v1("i", "future", "critical", "2026-10-02T13:00:00Z"),
        ]
        .iter()
        .filter_map(|line| parse_finding_line(line))
        .collect();
        assert_eq!(lines.len(), 9);
        let summary = summarize(&lines, DEFAULT_FINDINGS_WINDOW, now);
        assert_eq!(summary.window_secs, 24 * 3600);
        assert_eq!(summary.as_of, now);
        assert_eq!(summary.total(), 7);
        assert_eq!(summary.high_or_critical(), 2);
        assert_eq!(
            summary.by_severity,
            SeverityCounts {
                info: 1,
                low: 1,
                medium: 3,
                high: 1,
                critical: 1
            }
        );
        let order: Vec<&str> = summary.by_rule.iter().map(|r| r.rule_id.as_str()).collect();
        assert_eq!(order, ["structure-drift", "egress", "edge"]);
        assert_eq!(
            summary.by_rule.first().map(|r| r.max_severity),
            Some(ImpactSeverity::Critical)
        );
        assert_eq!(
            summary.latest_observed_at,
            Some(at("2026-10-02T11:59:00Z")?)
        );

        // A one-hour window keeps only the last hour.
        let hour = summarize(&lines, Duration::from_secs(3600), now);
        assert_eq!(hour.total(), 5);
        assert_eq!(hour.high_or_critical(), 1);
        // Moving `now` moves the window: nothing is recent a week later.
        let later = summarize(&lines, DEFAULT_FINDINGS_WINDOW, at("2026-10-09T12:00:00Z")?);
        assert_eq!(later.total(), 0);
        assert_eq!(later.latest_observed_at, None);
        Ok(())
    }

    #[test]
    fn summary_caps_the_rule_list() -> TestResult {
        let now = at("2026-10-02T12:00:00Z")?;
        let lines: Vec<FindingLine> = (0..MAX_SUMMARY_RULES + 3)
            .filter_map(|i| {
                parse_finding_line(&v1(
                    "f",
                    &format!("rule-{i:02}"),
                    "low",
                    "2026-10-02T11:00:00Z",
                ))
            })
            .collect();
        let summary = summarize(&lines, DEFAULT_FINDINGS_WINDOW, now);
        assert_eq!(summary.by_rule.len(), MAX_SUMMARY_RULES);
        assert_eq!(summary.other_rules, 3);
        assert_eq!(summary.total(), u32::try_from(MAX_SUMMARY_RULES + 3)?);
        Ok(())
    }

    // ── PL-100 WP0.12: export v3 ────────────────────────────────────────────

    const FINDING: &str = "3f2b8c1e-9d4a-4b7e-8a6f-1c2d3e4f5a6b";
    const INSTANCE: &str = "a1b2c3d4-e5f6-4a7b-9c8d-0e1f2a3b4c5d";
    const DEDUP: &str = "0123456789abcdef0123456789abcdef";
    const DIGEST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// The v3-only keys (all optional for the reader).
    const V3_KEYS: [&str; 11] = [
        "instance",
        "seq",
        "record_digest",
        "dedup_key",
        "occurrences",
        "first_seen",
        "kind",
        "hardness",
        "cgroup_kid",
        "scope_obfuscated",
        "evidence_card",
    ];

    /// A complete v3 line, as the sentinel writes it.
    fn v3_value() -> Value {
        json!({
            "schema_version": 3,
            "finding_id": FINDING,
            "host": "host-a",
            "rule_id": "egress-flow",
            "severity": "high",
            "summary": "tcp connect",
            "observed_at": "2026-10-02T11:00:00Z",
            "scope": "/sys/fs/cgroup/harw.slice/harw-job-1.scope",
            "instance": INSTANCE,
            "seq": 7,
            "record_digest": DIGEST,
            "dedup_key": DEDUP,
            "occurrences": 3,
            "first_seen": "2026-10-02T10:00:00Z",
            "kind": "rule_triggered",
            "hardness": "observed",
            "cgroup_kid": 4242,
            "scope_obfuscated": false,
            "evidence_card": {
                "event_counts": {"tcp_connect": 3},
                "uid": 1000,
                "destination": "203.0.113.9:443",
                "path": "/usr/bin/curl",
                "process": {"pid": 4711, "comm": "curl"}
            }
        })
    }

    /// `value` with `key` set to `field`.
    fn with(mut value: Value, key: &str, field: Value) -> Value {
        if let Some(object) = value.as_object_mut() {
            object.insert(key.to_owned(), field);
        }
        value
    }

    /// `value` without `key`.
    fn without(mut value: Value, key: &str) -> Value {
        if let Some(object) = value.as_object_mut() {
            object.remove(key);
        }
        value
    }

    #[test]
    fn accepts_a_complete_v3_line_and_round_trips_it() -> TestResult {
        let line = parse_finding_line(&v3_value().to_string()).ok_or("v3 line must parse")?;
        assert_eq!(line.schema_version, Some(3));
        assert!(line.has_uuid_v4_id());
        assert_eq!(line.finding_id.as_str(), FINDING);
        assert_eq!(line.instance.as_deref(), Some(INSTANCE));
        assert_eq!(line.seq, Some(7));
        assert_eq!(line.record_digest.as_deref(), Some(DIGEST));
        assert_eq!(line.dedup_key.as_deref(), Some(DEDUP));
        assert_eq!(line.occurrences, Some(3));
        assert_eq!(line.first_seen, Some(at("2026-10-02T10:00:00Z")?));
        assert_eq!(line.kind, Some(FindingKind::RuleTriggered));
        assert_eq!(line.hardness, Some(FindingHardness::Observed));
        assert_eq!(line.cgroup_kid, Some(4242));
        assert!(!line.scope_obfuscated);
        assert_eq!(
            line.scope.as_deref(),
            Some("/sys/fs/cgroup/harw.slice/harw-job-1.scope")
        );
        let card = line.evidence_card.as_ref().ok_or("card kept")?;
        assert_eq!(card.event_counts.get("tcp_connect"), Some(&3));
        assert_eq!(card.uid, Some(1000));
        assert_eq!(card.process.as_ref().map(|p| p.pid), Some(4711));

        // Round trip through the serialized form keeps the line identical.
        let json = serde_json::to_string(&line)?;
        let again: FindingLine = serde_json::from_str(&json)?;
        assert_eq!(again, line);

        // Every v3 field is optional: a v3 line with only the v1/v2 fields
        // parses, and its v3 fields stay empty.
        let minimal = V3_KEYS
            .iter()
            .fold(v3_value(), |value, key| without(value, key));
        let parsed = parse_finding_line(&minimal.to_string()).ok_or("minimal v3 line")?;
        assert_eq!(parsed.instance, None);
        assert_eq!(parsed.seq, None);
        assert_eq!(parsed.record_digest, None);
        assert_eq!(parsed.kind, None);
        assert_eq!(parsed.evidence_card, None);
        assert!(!parsed.scope_obfuscated);
        // The v2-shaped serialization of such a line carries no v3 keys.
        let json = serde_json::to_string(&parsed)?;
        for key in V3_KEYS {
            assert!(!json.contains(&format!("\"{key}\"")), "{key} in {json}");
        }
        Ok(())
    }

    #[test]
    fn schema_versions_one_to_three_parse_and_v3_requires_uuid_v4_ids() -> TestResult {
        for version in [1_u32, 2, 3] {
            let line = with(v3_value(), "schema_version", json!(version));
            assert_eq!(
                parse_finding_line(&line.to_string()).map(|l| l.schema_version),
                Some(Some(version))
            );
        }
        // v1/v2 keep the older id rule; such ids are not UUIDv4.
        let legacy = with(
            with(v3_value(), "schema_version", json!(2)),
            "finding_id",
            json!("f-1"),
        );
        let parsed = parse_finding_line(&legacy.to_string()).ok_or("legacy id on v2")?;
        assert!(!parsed.has_uuid_v4_id());
        let unversioned = with(
            without(v3_value(), "schema_version"),
            "finding_id",
            json!("f-1"),
        );
        assert!(parse_finding_line(&unversioned.to_string()).is_some());

        // From v3 on only the canonical UUIDv4 the sentinel mints.
        let upper = FINDING.to_uppercase();
        for bad in [
            "f-1",
            upper.as_str(),
            "{3f2b8c1e-9d4a-4b7e-8a6f-1c2d3e4f5a6b}",
            "3f2b8c1e9d4a4b7e8a6f1c2d3e4f5a6b",
            "00000000-0000-0000-0000-000000000000",
            "3f2b8c1e-9d4a-1b7e-8a6f-1c2d3e4f5a6b",
            "3f2b8c1e-9d4a-4b7e-7a6f-1c2d3e4f5a6b",
            "3f2b8c1e-9d4a-4b7e-8a6f-1c2d3e4f5a6bb",
        ] {
            let line = with(v3_value(), "finding_id", json!(bad));
            assert!(parse_finding_line(&line.to_string()).is_none(), "{bad}");
            assert!(!is_uuid_v4(bad), "{bad}");
        }
        assert!(is_uuid_v4(FINDING));
        Ok(())
    }

    #[test]
    fn malformed_v3_identifiers_reject_the_line() {
        for (key, value) in [
            ("instance", json!("not-a-uuid")),
            ("instance", json!(INSTANCE.to_uppercase())),
            ("seq", json!(0)),
            ("seq", json!(-1)),
            (
                "record_digest",
                json!("0".repeat(RECORD_DIGEST_HEX_LEN - 1)),
            ),
            ("record_digest", json!(DIGEST.to_uppercase())),
            ("dedup_key", json!(DIGEST)),
            ("dedup_key", json!("g".repeat(DEDUP_KEY_HEX_LEN))),
            ("occurrences", json!(0)),
            ("first_seen", json!("yesterday")),
            ("kind", json!("rule-triggered")),
            ("hardness", json!("certain")),
            ("cgroup_kid", json!(-5)),
            ("scope_obfuscated", json!("yes")),
        ] {
            let text = with(v3_value(), key, value).to_string();
            assert!(parse_finding_line(&text).is_none(), "{key}: {text}");
        }
    }

    #[test]
    fn newer_writers_may_add_fields_and_enum_values() -> TestResult {
        let future = [
            ("schema_version", json!(4)),
            ("kind", json!("verdict")),
            ("hardness", json!("sensed")),
            ("future_field", json!({"x": 1})),
            ("evidence_card", json!({"uid": 5, "future": [1]})),
        ]
        .into_iter()
        .fold(v3_value(), |value, (key, field)| with(value, key, field));
        let line = parse_finding_line(&future.to_string()).ok_or("v4 line must parse")?;
        assert_eq!(line.kind, None);
        assert_eq!(line.hardness, None);
        assert_eq!(line.evidence_card.and_then(|card| card.uid), Some(5));

        // Up to v3 an unknown card key drops the card, never the line.
        let strict = with(
            v3_value(),
            "evidence_card",
            json!({"uid": 5, "future": [1]}),
        );
        let line = parse_finding_line(&strict.to_string()).ok_or("v3 line keeps parsing")?;
        assert_eq!(line.evidence_card, None);
        assert_eq!(line.seq, Some(7));

        // Malformed identifiers stay fatal for newer writers as well.
        let bad = with(
            with(v3_value(), "schema_version", json!(4)),
            "seq",
            json!(0),
        );
        assert!(parse_finding_line(&bad.to_string()).is_none());
        Ok(())
    }

    #[test]
    fn unsafe_scope_is_marked_obfuscated_not_silently_dropped() -> TestResult {
        let zero_width = with(
            v3_value(),
            "scope",
            json!("/harw.slice/harw-job\u{200b}-1.scope"),
        );
        let line = parse_finding_line(&zero_width.to_string()).ok_or("line kept")?;
        assert_eq!(line.scope, None);
        assert!(line.scope_obfuscated);

        // The writer's own flag is kept.
        let withheld = with(
            without(v3_value(), "scope"),
            "scope_obfuscated",
            json!(true),
        );
        let line = parse_finding_line(&withheld.to_string()).ok_or("line kept")?;
        assert!(line.scope_obfuscated);
        assert_eq!(line.scope, None);

        // No scope or an empty scope is not an obfuscated one.
        let empty = with(v3_value(), "scope", json!(""));
        let line = parse_finding_line(&empty.to_string()).ok_or("line kept")?;
        assert!(!line.scope_obfuscated);
        assert_eq!(line.scope, None);
        let absent = without(v3_value(), "scope");
        let line = parse_finding_line(&absent.to_string()).ok_or("line kept")?;
        assert!(!line.scope_obfuscated);

        // The reader marks dropped scopes on v1/v2 lines too.
        let v2 = with(
            with(zero_width, "schema_version", json!(2)),
            "finding_id",
            json!("f-2"),
        );
        assert_eq!(
            parse_finding_line(&v2.to_string()).map(|l| l.scope_obfuscated),
            Some(true)
        );
        Ok(())
    }

    #[test]
    fn evidence_card_is_escaped_and_an_oversized_card_is_dropped() -> TestResult {
        let hostile = with(
            v3_value(),
            "evidence_card",
            json!({
                "path": "/tmp/\u{202e}fdp.exe",
                "process": {"pid": 1, "comm": "\u{1b}]0;pwned\u{7}"}
            }),
        );
        let line = parse_finding_line(&hostile.to_string()).ok_or("line kept")?;
        let card = line.evidence_card.ok_or("card kept")?;
        assert_eq!(card.path.as_deref(), Some("/tmp/\\u{202e}fdp.exe"));
        assert!(
            card.process
                .is_some_and(|process| harw_untrusted_text::is_clean(&process.comm))
        );

        let big = with(
            v3_value(),
            "evidence_card",
            json!({"path": "p".repeat(crate::evidence::MAX_EVIDENCE_CARD_BYTES)}),
        );
        let line = parse_finding_line(&big.to_string()).ok_or("line kept")?;
        assert_eq!(line.evidence_card, None);
        assert_eq!(line.dedup_key.as_deref(), Some(DEDUP));
        Ok(())
    }

    #[test]
    fn future_dated_v3_lines_are_dropped_by_the_line_parser() -> TestResult {
        let now = at("2026-10-02T12:00:00Z")?;
        let future = with(v3_value(), "observed_at", json!("2026-10-02T12:30:00Z"));
        let mut bytes = v3_value().to_string().into_bytes();
        bytes.push(b'\n');
        bytes.extend_from_slice(future.to_string().as_bytes());
        bytes.push(b'\n');
        let lines = parse_lines(&bytes, now);
        assert_eq!(lines.len(), 1);
        assert_eq!(
            lines.first().map(|l| l.observed_at),
            Some(at("2026-10-02T11:00:00Z")?)
        );
        Ok(())
    }
}
