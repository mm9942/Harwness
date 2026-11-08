//! The SecurityHub posture document: `GET /v1/posture`
//! ([`crate::SecurityHubClient::posture`]).
//!
//! # Wire contract (JSON, `application/json`)
//!
//! What `harw_security_hub::server` answers today (protocol 1):
//!
//! ```text
//! 200 {"peer": {"uid": <u32>, "gid": <u32>, "pid": <u32>|null},
//!      "principal_id": "<id>"|null,
//!      "host": "<host id>"|null,
//!      "dod": {"status": "ok", "min_severity": "<severity>", "since": "<RFC 3339>",
//!              "findings": [<finding>…], "skipped_malformed": <n>,
//!              "skipped_oversize": <n>, "truncated": <bool>}
//!           | {"status": "unavailable", "findings": []}
//!           | {"status": "not_configured", "findings": []}}
//! <finding> = {"finding_id", "host", "rule_id", "severity", "summary",
//!              "observed_at": "<RFC 3339>",
//!              "schema_version"?, "dedup_key"?, "occurrences"?}
//! 403 {"error": "unknown_peer"}  → InfraClientError::Forbidden
//! ```
//!
//! `<severity>` is one of `info`, `low`, `medium`, `high`, `critical`.
//!
//! # Reader-first (PL-100 I11)
//! The hub may only ever *add* to this document. This reader therefore
//! ignores unknown fields, treats every field the hub omits in some state
//! (`min_severity`, `since`, the counters, the v3 finding fields) as
//! optional, and maps an unknown `status` or severity string to an
//! `Unknown` variant instead of failing. Only a missing `peer` or `dod`, a
//! missing `dod.status`, or a finding without its identifying fields (or
//! with one of the wrong JSON type) is a [`InfraClientError::Protocol`]
//! error. An id that fails its grammar is not an error (next section).
//!
//! # Untrusted text (PL-100 I4, THREAT-MODEL B3)
//! Three finding fields come straight from the DoD findings export, which
//! is written from attacker-influenced observations, and the hub does not
//! make them safe to show: it replaces only `Cc` control characters in
//! `summary` and `rule_id` (bidi overrides, zero-width and tag-block
//! characters pass) and passes `finding_id` through with nothing but a
//! non-blank check. This reader therefore never hands them out as plain
//! strings:
//! - [`PostureFinding::summary`] is an [`UntrustedText`]: no `Display`, no
//!   `Deref`, a `Debug` that shows only its length. Render it with
//!   [`UntrustedText::escaped_line`] (`harw_untrusted_text::escape_line`)
//!   and never use it as an instruction.
//! - [`PostureFinding::finding_id`] and [`PostureFinding::rule_id`] are a
//!   [`PostureId`]: [`PostureId::Valid`] only when the value passes the id
//!   grammar of the DoD export reader (`harw-dod-client`), otherwise
//!   [`PostureId::Invalid`] holding the value as [`UntrustedText`]. A
//!   finding with an invalid id is kept, never dropped (PL-100 I3).
//!   - `finding_id`: not blank, at most 256 bytes, and no character
//!     `harw_untrusted_text` would escape (the export reader's rule for
//!     v1/v2 lines);
//!   - `rule_id`: 1 to 128 bytes of ASCII alphanumerics, `-`, `_`, `.`,
//!     `:` (the export reader's and the node uplink's rule).
//! - [`PostureFinding::dedup_key`] is kept only as 32 lowercase hex
//!   characters (the hub filters the same way); anything else reads as
//!   `None`.
//!
//! A valid finding id is safe to display, but only a UUIDv4 belongs in a
//! model instruction template (I4); this reader does not check for that.
//! The remaining strings are produced by the hub itself, not by the
//! export: `observed_at` and `since` are its RFC 3339 rendering of parsed
//! timestamps, `host` is its configured host id (it only returns findings
//! whose `host` equals it), and `principal_id` comes from its policy.
//!
//! # Examples
//! ```no_run
//! # async fn show(client: &harw_infra_client::SecurityHubClient)
//! #     -> Result<Vec<String>, harw_infra_client::InfraClientError> {
//! let posture = client.posture().await?;
//! let mut lines = Vec::new();
//! for finding in &posture.dod.findings {
//!     // Ids and summaries only ever leave the client escaped.
//!     let summary = finding
//!         .summary
//!         .as_ref()
//!         .map_or_else(String::new, |text| text.escaped_line(200));
//!     lines.push(format!(
//!         "{} {} {summary}",
//!         finding.finding_id.escaped_line(64),
//!         finding.rule_id.escaped_line(64),
//!     ));
//! }
//! # Ok(lines)
//! # }
//! ```

use core::fmt;

use http::Method;
use serde::{Deserialize, Deserializer};

use crate::error::InfraClientError;
use crate::info::JSON;
use crate::transport::{RequestBody, UdsTransport};

/// Request path of the posture document.
const POSTURE_PATH: &str = "/v1/posture";

/// Longest `finding_id` read as [`PostureId::Valid`], in bytes (the
/// `MAX_ID_BYTES` of the `harw-dod-client` export reader).
const MAX_FINDING_ID_BYTES: usize = 256;

/// Longest `rule_id` read as [`PostureId::Valid`], in bytes (the
/// `MAX_RULE_ID_BYTES` of the `harw-dod-client` export reader and of the
/// node uplink).
const MAX_RULE_ID_BYTES: usize = 128;

/// Length of a well-formed export v3 `dedup_key` (lowercase hex of 16
/// bytes).
const DEDUP_KEY_HEX_LEN: usize = 32;

/// `GET /v1/posture`: who the hub takes the caller for, plus the DoD
/// posture of this host.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Posture {
    /// The kernel-attested peer credentials the hub read for this
    /// connection.
    pub peer: PosturePeer,
    /// Principal the hub's policy maps the caller's uid to; `None` for a
    /// verifier without a policy rule.
    #[serde(default)]
    pub principal_id: Option<String>,
    /// This host's id (the DoD correlation key); `None` if the hub has none
    /// configured.
    #[serde(default)]
    pub host: Option<String>,
    /// DoD correlation state and recent findings for this host.
    pub dod: DodPosture,
}

/// The peer credentials (`SO_PEERCRED`) the hub attributes to the caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub struct PosturePeer {
    /// Effective uid.
    pub uid: u32,
    /// Effective gid.
    pub gid: u32,
    /// Process id at `connect()` time, if the platform reports it.
    #[serde(default)]
    pub pid: Option<u32>,
}

/// The `dod` part of the posture document.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct DodPosture {
    /// Whether the hub could read DoD findings.
    pub status: DodPostureStatus,
    /// Lowest severity included; only sent with [`DodPostureStatus::Ok`].
    #[serde(default)]
    pub min_severity: Option<PostureSeverity>,
    /// Start of the finding window (RFC 3339); only sent with
    /// [`DodPostureStatus::Ok`].
    #[serde(default)]
    pub since: Option<String>,
    /// Matching findings, newest first; empty unless
    /// [`DodPostureStatus::Ok`].
    #[serde(default)]
    pub findings: Vec<PostureFinding>,
    /// Export lines that were not valid finding records (`0` when not
    /// sent).
    #[serde(default)]
    pub skipped_malformed: u64,
    /// Export lines longer than the hub's line limit (`0` when not sent).
    #[serde(default)]
    pub skipped_oversize: u64,
    /// More findings matched than the hub returns; the oldest were dropped.
    #[serde(default)]
    pub truncated: bool,
}

/// `dod.status` of the posture document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DodPostureStatus {
    /// Findings were read; [`DodPosture::findings`] is current.
    Ok,
    /// The hub is wired to a findings export but could not read it (e.g.
    /// the file is missing). Not an error of the call.
    Unavailable,
    /// The hub has no findings export or no host id configured.
    NotConfigured,
    /// A status string this client does not know (a newer hub).
    Unknown,
}

impl DodPostureStatus {
    /// Maps the wire spelling; unknown strings become [`Self::Unknown`].
    fn from_wire(text: &str) -> Self {
        match text {
            "ok" => Self::Ok,
            "unavailable" => Self::Unavailable,
            "not_configured" => Self::NotConfigured,
            _ => Self::Unknown,
        }
    }
}

impl<'de> Deserialize<'de> for DodPostureStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Ok(Self::from_wire(&text))
    }
}

/// Severity of a posture finding (`harw_types::ImpactSeverity` on the hub).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PostureSeverity {
    /// Informational.
    Info,
    /// Low.
    Low,
    /// Medium.
    Medium,
    /// High.
    High,
    /// Critical.
    Critical,
    /// A severity string this client does not know (a newer hub).
    Unknown,
}

impl PostureSeverity {
    /// Maps the wire spelling; unknown strings become [`Self::Unknown`].
    fn from_wire(text: &str) -> Self {
        match text {
            "info" => Self::Info,
            "low" => Self::Low,
            "medium" => Self::Medium,
            "high" => Self::High,
            "critical" => Self::Critical,
            _ => Self::Unknown,
        }
    }
}

impl<'de> Deserialize<'de> for PostureSeverity {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Ok(Self::from_wire(&text))
    }
}

/// Text from the DoD findings export that is not safe to show as is
/// (PL-100 I4, THREAT-MODEL B3).
///
/// # Description
/// Deliberately without `Display`, `Deref`, `AsRef<str>` or a conversion
/// into `String`, so a `format!("{}", …)` cannot put raw sensor text into a
/// terminal line, a TUI or a model prompt. The one way to show it is
/// [`Self::escaped_line`]. [`Self::as_untrusted_str`] exists for callers
/// that compare the text or hand it to their own escaping layer, and says
/// so in its name. `Debug` shows only the length in bytes.
///
/// # Concurrency
/// Plain owned data; `Send + Sync`.
///
/// # Examples
/// ```
/// use harw_infra_client::UntrustedText;
///
/// let text = UntrustedText::new("evil\u{202E}txt.exe".to_owned());
/// assert_eq!(text.escaped_line(64), "evil\\u{202e}txt.exe");
/// assert_eq!(format!("{text:?}"), "UntrustedText(<14 bytes>)");
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct UntrustedText(String);

impl UntrustedText {
    /// Wraps `text` as untrusted.
    ///
    /// # Arguments
    /// - `text` (`String`): the raw text; it is kept unchanged.
    ///
    /// # Returns
    /// The wrapper.
    #[must_use]
    pub fn new(text: String) -> Self {
        Self(text)
    }

    /// The text made safe for one line of a terminal, a TUI or a model
    /// prompt.
    ///
    /// # Description
    /// `harw_untrusted_text::escape_line`: every control, format/bidi,
    /// separator, private-use, variation-selector and tag-block character,
    /// line breaks included, becomes a visible `\u{…}` escape, and the
    /// result is cut to at most `max_chars` characters (escapes and the
    /// truncation mark `…` included).
    ///
    /// # Arguments
    /// - `max_chars` (`usize`): upper bound of the output in characters.
    ///
    /// # Returns
    /// The escaped single line.
    #[must_use]
    pub fn escaped_line(&self, max_chars: usize) -> String {
        harw_untrusted_text::escape_line(&self.0, max_chars)
    }

    /// The raw, unescaped text.
    ///
    /// # Description
    /// Only for a caller that compares or hashes the text, or escapes it
    /// itself. Never print it, log it or put it into a prompt; use
    /// [`Self::escaped_line`] for that.
    ///
    /// # Returns
    /// The text exactly as the hub sent it.
    #[must_use]
    pub fn as_untrusted_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for UntrustedText {
    /// Shows only the length, so a debug log cannot carry the text.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "UntrustedText(<{} bytes>)", self.0.len())
    }
}

impl<'de> Deserialize<'de> for UntrustedText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self)
    }
}

/// A `finding_id` or `rule_id` of a [`PostureFinding`], checked against its
/// grammar on decode (module docs, "Untrusted text").
///
/// # Description
/// [`Self::Valid`] holds a value that passed the grammar and may be shown
/// as is. [`Self::Invalid`] holds one that did not: the finding stays
/// visible (PL-100 I3), but its id is only available as [`UntrustedText`].
/// `Debug` never shows an invalid value.
///
/// # Examples
/// ```
/// use harw_infra_client::{PostureId, UntrustedText};
///
/// let valid = PostureId::Valid("egress-flow".to_owned());
/// assert_eq!(valid.as_valid(), Some("egress-flow"));
/// let invalid = PostureId::Invalid(UntrustedText::new("egress\u{202E}wolf".to_owned()));
/// assert_eq!(invalid.as_valid(), None);
/// assert_eq!(invalid.escaped_line(64), "egress\\u{202e}wolf");
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PostureId {
    /// The value passed its grammar.
    Valid(String),
    /// The value failed its grammar; untrusted.
    Invalid(UntrustedText),
}

impl PostureId {
    /// Sorts `text` into [`Self::Valid`] or [`Self::Invalid`] by `grammar`.
    fn checked(text: String, grammar: fn(&str) -> bool) -> Self {
        if grammar(&text) {
            Self::Valid(text)
        } else {
            Self::Invalid(UntrustedText(text))
        }
    }

    /// The id, if it passed its grammar.
    ///
    /// # Returns
    /// `Some` for [`Self::Valid`], `None` for [`Self::Invalid`].
    #[must_use]
    pub fn as_valid(&self) -> Option<&str> {
        match self {
            Self::Valid(id) => Some(id),
            Self::Invalid(_) => None,
        }
    }

    /// Whether the id passed its grammar.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        matches!(self, Self::Valid(_))
    }

    /// The id for one display line.
    ///
    /// # Description
    /// Both variants go through `harw_untrusted_text::escape_line`: a valid
    /// id comes out unchanged (only cut to `max_chars`), an invalid one with
    /// every unsafe character escaped.
    ///
    /// # Arguments
    /// - `max_chars` (`usize`): upper bound of the output in characters.
    ///
    /// # Returns
    /// The escaped single line.
    #[must_use]
    pub fn escaped_line(&self, max_chars: usize) -> String {
        match self {
            Self::Valid(id) => harw_untrusted_text::escape_line(id, max_chars),
            Self::Invalid(text) => text.escaped_line(max_chars),
        }
    }
}

/// `finding_id` grammar: not blank, at most [`MAX_FINDING_ID_BYTES`] bytes
/// and no character `harw_untrusted_text` would escape (the
/// `harw-dod-client` export reader's rule for v1/v2 lines).
fn is_finding_id(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_FINDING_ID_BYTES
        && harw_untrusted_text::is_clean(value)
}

/// `rule_id` grammar: 1..=[`MAX_RULE_ID_BYTES`] bytes of ASCII
/// alphanumerics, `-`, `_`, `.`, `:` (the `harw-dod-client` export reader
/// and the node uplink).
fn is_rule_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_RULE_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

/// `true` for exactly [`DEDUP_KEY_HEX_LEN`] lowercase hex characters.
fn is_dedup_key(value: &str) -> bool {
    value.len() == DEDUP_KEY_HEX_LEN
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Decodes a `finding_id`; a value outside [`is_finding_id`] becomes
/// [`PostureId::Invalid`]. A non-string is a decode error.
fn finding_id_field<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PostureId, D::Error> {
    String::deserialize(deserializer).map(|text| PostureId::checked(text, is_finding_id))
}

/// Decodes a `rule_id`; a value outside [`is_rule_id`] becomes
/// [`PostureId::Invalid`]. A non-string is a decode error.
fn rule_id_field<'de, D: Deserializer<'de>>(deserializer: D) -> Result<PostureId, D::Error> {
    String::deserialize(deserializer).map(|text| PostureId::checked(text, is_rule_id))
}

/// Decodes an optional `dedup_key`; a malformed key reads as `None`, the
/// way the hub drops it.
fn dedup_key_field<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(deserializer).map(|key| key.filter(|key| is_dedup_key(key)))
}

/// One recent DoD finding for this host, as the hub reports it.
///
/// `Debug` is safe to log: it shows a valid id, never an invalid one or the
/// summary (only their lengths).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PostureFinding {
    /// Finding id, checked against the finding id grammar (module docs);
    /// [`PostureId::Invalid`] if the export carried anything else.
    #[serde(deserialize_with = "finding_id_field")]
    pub finding_id: PostureId,
    /// Host the finding was observed on: the hub's own configured host id
    /// (it only returns findings whose host equals it).
    pub host: String,
    /// Id of the rule that produced it, checked against the rule id grammar
    /// (module docs); [`PostureId::Invalid`] if the export carried anything
    /// else.
    #[serde(deserialize_with = "rule_id_field")]
    pub rule_id: PostureId,
    /// Severity.
    pub severity: PostureSeverity,
    /// Human-readable summary from the DoD export: sensor-derived and
    /// **untrusted**, only shown through [`UntrustedText::escaped_line`];
    /// `None` if the hub stops sending it.
    #[serde(default)]
    pub summary: Option<UntrustedText>,
    /// Observation time (RFC 3339, rendered by the hub).
    pub observed_at: String,
    /// Export schema version; absent on v1 export lines.
    #[serde(default)]
    pub schema_version: Option<u32>,
    /// Export v3 throttling key (32 lowercase hex chars): findings with the
    /// same key are repeats of one finding. `None` when absent or not in
    /// that form.
    #[serde(default, deserialize_with = "dedup_key_field")]
    pub dedup_key: Option<String>,
    /// Export v3: how often `dedup_key` fired (`>= 1`).
    #[serde(default)]
    pub occurrences: Option<u64>,
}

/// `GET /v1/posture` on `transport`.
pub(crate) async fn fetch(transport: &UdsTransport) -> Result<Posture, InfraClientError> {
    let response = transport
        .call(Method::GET, POSTURE_PATH, RequestBody::Empty, None)
        .await?;
    response.expect_content_type(JSON)?;
    parse(&response.body)
}

/// Decodes a posture document.
fn parse(body: &[u8]) -> Result<Posture, InfraClientError> {
    serde_json::from_slice(body)
        .map_err(|_| InfraClientError::Protocol("malformed posture document"))
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use bytes::Bytes;
    use http::header::{CONTENT_TYPE, HeaderValue};
    use http::{Method, Request, Response, StatusCode};
    use http_body_util::Full;
    use hyper::body::Incoming;
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use tokio::net::UnixListener;

    use super::{
        DodPostureStatus, PostureId, PostureSeverity, UntrustedText, is_dedup_key, is_finding_id,
        is_rule_id, parse,
    };
    use crate::error::InfraClientError;
    use crate::info::JSON;
    use crate::security::SecurityHubClient;
    use crate::test_support::{TestError, TestResult};
    use crate::transport::ClientOptions;

    /// What the hub sends with DoD correlation wired (shape of
    /// `harw_security_hub::server::posture`), plus fields a newer hub might
    /// add (`schema`, `scope`, `kind`): they must be ignored.
    const OK_DOCUMENT: &str = r#"{
        "peer": {"uid": 1000, "gid": 1000, "pid": 4242},
        "principal_id": "alice",
        "host": "host-a",
        "schema": 2,
        "dod": {
            "status": "ok",
            "min_severity": "high",
            "since": "2026-09-26T10:00:00Z",
            "findings": [
                {"finding_id": "3f2b8c1e-9d4a-4b7e-8a6f-1c2d3e4f5a6b", "host": "host-a",
                 "rule_id": "egress-flow", "severity": "critical",
                 "summary": "connect to 203.0.113.7:443",
                 "observed_at": "2026-09-27T09:55:00Z", "schema_version": 3,
                 "dedup_key": "0123456789abcdef0123456789abcdef", "occurrences": 4,
                 "kind": "rule_triggered", "scope": "/sys/fs/cgroup/x"},
                {"finding_id": "f-1", "host": "host-a", "rule_id": "structure-drift",
                 "severity": "high", "summary": "s", "observed_at": "2026-09-27T09:50:00Z"}
            ],
            "skipped_malformed": 1,
            "skipped_oversize": 2,
            "truncated": true
        }
    }"#;

    /// Findings whose ids the hub passed through but that fail the id
    /// grammars: an ANSI escape and a zero-width space in `finding_id`, a
    /// bidi override, a tag-block character (as a surrogate pair), the
    /// hub's `U+FFFD` replacement and an overlong value in `rule_id`, plus
    /// an upper-case `dedup_key`. Every character is a JSON escape, so the
    /// source holds none of them literally.
    const INVALID_IDS_DOCUMENT: &str = r#"{
        "peer": {"uid": 1000, "gid": 1000},
        "dod": {"status": "ok", "findings": [
            {"finding_id": "\u001b[2Jf-1", "host": "host-a",
             "rule_id": "egress\u202eflow", "severity": "critical",
             "summary": "x", "observed_at": "2026-09-27T09:55:00Z",
             "dedup_key": "0123456789ABCDEF0123456789ABCDEF"},
            {"finding_id": "f\u200b-2", "host": "host-a",
             "rule_id": "rule\udb40\udc41", "severity": "high",
             "observed_at": "2026-09-27T09:54:00Z"},
            {"finding_id": "   ", "host": "host-a",
             "rule_id": "a\ufffdb", "severity": "high",
             "observed_at": "2026-09-27T09:53:00Z"}
        ]}
    }"#;

    fn answer(
        status: StatusCode,
        content_type: &'static str,
        body: &'static str,
    ) -> Response<Full<Bytes>> {
        let mut response = Response::new(Full::new(Bytes::from_static(body.as_bytes())));
        *response.status_mut() = status;
        response
            .headers_mut()
            .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
        response
    }

    /// A mock hub that answers `GET /v1/posture` with the given response
    /// (and `404` for anything else); returns the dir (keeps the socket
    /// alive), the client and the server task.
    async fn fixed_hub(
        status: StatusCode,
        content_type: &'static str,
        body: &'static str,
    ) -> TestResult<(
        tempfile::TempDir,
        SecurityHubClient,
        tokio::task::JoinHandle<()>,
    )> {
        let dir = tempfile::tempdir()?;
        let socket = dir.path().join("security.sock");
        let listener = UnixListener::bind(&socket)?;
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let service = service_fn(move |request: Request<Incoming>| async move {
                        let routed = *request.method() == Method::GET
                            && request.uri().path() == "/v1/posture";
                        let response = if routed {
                            answer(status, content_type, body)
                        } else {
                            answer(StatusCode::NOT_FOUND, JSON, r#"{"error":"not_found"}"#)
                        };
                        Ok::<_, Infallible>(response)
                    });
                    // A broken connection surfaces as a client error in the
                    // test that made the call; nothing to report here.
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        let client = SecurityHubClient::new(socket, ClientOptions::default());
        Ok((dir, client, task))
    }

    #[test]
    fn test_parse_ok_document_types_every_field_and_ignores_unknown_ones() -> TestResult {
        let posture = parse(OK_DOCUMENT.as_bytes())?;
        assert_eq!(posture.peer.uid, 1000);
        assert_eq!(posture.peer.gid, 1000);
        assert_eq!(posture.peer.pid, Some(4242));
        assert_eq!(posture.principal_id.as_deref(), Some("alice"));
        assert_eq!(posture.host.as_deref(), Some("host-a"));
        let dod = &posture.dod;
        assert_eq!(dod.status, DodPostureStatus::Ok);
        assert_eq!(dod.min_severity, Some(PostureSeverity::High));
        assert_eq!(dod.since.as_deref(), Some("2026-09-26T10:00:00Z"));
        assert_eq!(dod.skipped_malformed, 1);
        assert_eq!(dod.skipped_oversize, 2);
        assert!(dod.truncated);
        assert_eq!(dod.findings.len(), 2);

        let first = dod.findings.first().ok_or(TestError::Missing("first"))?;
        assert_eq!(
            first.finding_id.as_valid(),
            Some("3f2b8c1e-9d4a-4b7e-8a6f-1c2d3e4f5a6b")
        );
        assert_eq!(first.host, "host-a");
        assert_eq!(first.rule_id.as_valid(), Some("egress-flow"));
        assert_eq!(first.severity, PostureSeverity::Critical);
        assert_eq!(
            first.summary.as_ref().map(|text| text.escaped_line(256)),
            Some("connect to 203.0.113.7:443".to_owned())
        );
        assert_eq!(first.observed_at, "2026-09-27T09:55:00Z");
        assert_eq!(first.schema_version, Some(3));
        assert_eq!(
            first.dedup_key.as_deref(),
            Some("0123456789abcdef0123456789abcdef")
        );
        assert_eq!(first.occurrences, Some(4));

        // A v1 export line: no v3 fields.
        let second = dod.findings.get(1).ok_or(TestError::Missing("second"))?;
        assert_eq!(second.finding_id.as_valid(), Some("f-1"));
        assert_eq!(second.rule_id.as_valid(), Some("structure-drift"));
        assert_eq!(second.schema_version, None);
        assert_eq!(second.dedup_key, None);
        assert_eq!(second.occurrences, None);
        Ok(())
    }

    #[test]
    fn test_parse_degraded_states_without_optional_fields() -> TestResult {
        for (body, expected) in [
            (
                r#"{"peer":{"uid":1,"gid":2,"pid":null},"principal_id":null,"host":"host-a",
                    "dod":{"status":"unavailable","findings":[]}}"#,
                DodPostureStatus::Unavailable,
            ),
            (
                r#"{"peer":{"uid":1,"gid":2,"pid":null},"principal_id":"bob","host":null,
                    "dod":{"status":"not_configured","findings":[]}}"#,
                DodPostureStatus::NotConfigured,
            ),
        ] {
            let posture = parse(body.as_bytes())?;
            assert_eq!(posture.dod.status, expected);
            assert_eq!(posture.peer.pid, None);
            assert_eq!(posture.dod.min_severity, None);
            assert_eq!(posture.dod.since, None);
            assert!(posture.dod.findings.is_empty());
            assert_eq!(posture.dod.skipped_malformed, 0);
            assert_eq!(posture.dod.skipped_oversize, 0);
            assert!(!posture.dod.truncated);
        }
        Ok(())
    }

    #[test]
    fn test_parse_maps_unknown_status_and_severity_instead_of_failing() -> TestResult {
        let posture = parse(
            br#"{"peer":{"uid":1,"gid":1},"dod":{"status":"paused","min_severity":"urgent",
                 "findings":[{"finding_id":"f","host":"h","rule_id":"r","severity":"urgent",
                 "observed_at":"2026-09-27T09:55:00Z","dedup_key":null}]}}"#,
        )?;
        assert_eq!(posture.dod.status, DodPostureStatus::Unknown);
        assert_eq!(posture.dod.min_severity, Some(PostureSeverity::Unknown));
        let finding = posture
            .dod
            .findings
            .first()
            .ok_or(TestError::Missing("finding"))?;
        assert_eq!(finding.severity, PostureSeverity::Unknown);
        // A hub that stops sending the free-text summary still parses.
        assert!(finding.summary.is_none());
        // An explicit `null` key reads like a missing one.
        assert_eq!(finding.dedup_key, None);
        // Optional top-level fields may be missing entirely.
        assert_eq!(posture.principal_id, None);
        assert_eq!(posture.host, None);
        assert_eq!(posture.peer.pid, None);
        Ok(())
    }

    #[test]
    fn test_parse_rejects_documents_without_their_core() -> TestResult {
        for body in [
            &b""[..],
            &b"[]"[..],
            &b"{\"peer\":{\"uid\":1,\"gid\":1}}"[..],
            &b"{\"dod\":{\"status\":\"ok\"}}"[..],
            &b"{\"peer\":{\"uid\":1,\"gid\":1},\"dod\":{\"findings\":[]}}"[..],
            &b"{\"peer\":{\"uid\":\"root\",\"gid\":1},\"dod\":{\"status\":\"ok\"}}"[..],
            &b"{\"peer\":{\"uid\":1,\"gid\":1},\"dod\":{\"status\":\"ok\",\"findings\":[{\"finding_id\":\"f\"}]}}"[..],
            // An id of the wrong JSON type is malformed, not "invalid".
            &b"{\"peer\":{\"uid\":1,\"gid\":1},\"dod\":{\"status\":\"ok\",\"findings\":[{\"finding_id\":7,\"host\":\"h\",\"rule_id\":\"r\",\"severity\":\"high\",\"observed_at\":\"t\"}]}}"[..],
        ] {
            assert_eq!(
                parse(body),
                Err(InfraClientError::Protocol("malformed posture document")),
                "{}",
                String::from_utf8_lossy(body)
            );
        }
        Ok(())
    }

    #[test]
    fn test_parse_keeps_findings_with_invalid_ids_as_untrusted() -> TestResult {
        let posture = parse(INVALID_IDS_DOCUMENT.as_bytes())?;
        let findings = &posture.dod.findings;
        // Nothing is dropped (I3): every finding stays visible.
        assert_eq!(findings.len(), 3);
        for finding in findings {
            assert!(!finding.finding_id.is_valid(), "{finding:?}");
            assert!(!finding.rule_id.is_valid(), "{finding:?}");
            assert_eq!(finding.finding_id.as_valid(), None);
            assert_eq!(finding.rule_id.as_valid(), None);
            // Rendered, no raw control or format character survives.
            for rendered in [
                finding.finding_id.escaped_line(256),
                finding.rule_id.escaped_line(256),
            ] {
                if !harw_untrusted_text::is_clean(&rendered) {
                    return Err(TestError::Unexpected(format!(
                        "raw character survived escaping: {rendered:?}"
                    )));
                }
            }
            // Debug never shows an invalid id, neither its text nor its
            // raw characters.
            let debug = format!("{finding:?}");
            for leak in ["2J", "egress", "f-1"] {
                assert!(!debug.contains(leak), "{leak:?} in {debug:?}");
            }
            for raw in ['\u{1B}', '\u{202E}', '\u{200B}', '\u{E0041}', '\u{FFFD}'] {
                assert!(!debug.contains(raw), "{raw:?} in {debug:?}");
            }
        }

        let first = findings.first().ok_or(TestError::Missing("first"))?;
        assert_eq!(first.finding_id.escaped_line(256), "\\u{001b}[2Jf-1");
        assert_eq!(first.rule_id.escaped_line(256), "egress\\u{202e}flow");
        // The raw value stays reachable under an explicit name.
        match &first.rule_id {
            PostureId::Invalid(text) => assert_eq!(text.as_untrusted_str(), "egress\u{202E}flow"),
            PostureId::Valid(id) => {
                return Err(TestError::Unexpected(format!("accepted rule id {id:?}")));
            }
        }
        // An upper-case dedup key is dropped like the hub drops it.
        assert_eq!(first.dedup_key, None);

        let second = findings.get(1).ok_or(TestError::Missing("second"))?;
        assert_eq!(second.finding_id.escaped_line(256), "f\\u{200b}-2");
        assert_eq!(second.rule_id.escaped_line(256), "rule\\u{e0041}");

        let third = findings.get(2).ok_or(TestError::Missing("third"))?;
        assert_eq!(third.finding_id.escaped_line(256), "   ");
        assert_eq!(third.rule_id.escaped_line(256), "a\u{FFFD}b");
        Ok(())
    }

    #[test]
    fn test_id_grammars_match_the_export_reader() -> TestResult {
        let longest_finding_id = "f".repeat(256);
        for good in [
            "3f2b8c1e-9d4a-4b7e-8a6f-1c2d3e4f5a6b",
            "f-1",
            "finding 42",
            longest_finding_id.as_str(),
        ] {
            assert!(is_finding_id(good), "{good:?}");
        }
        let overlong_finding_id = "f".repeat(257);
        for bad in [
            "",
            "   ",
            "f\u{1B}[2J",
            "f\u{202E}1",
            "f\u{200B}1",
            "f\u{E0041}",
            "f\n1",
            overlong_finding_id.as_str(),
        ] {
            assert!(!is_finding_id(bad), "{bad:?}");
        }

        let longest_rule_id = "r".repeat(128);
        for good in [
            "egress-flow",
            "dod.proc.unexpected_exec",
            "ns:rule_1",
            longest_rule_id.as_str(),
        ] {
            assert!(is_rule_id(good), "{good:?}");
        }
        let overlong_rule_id = "r".repeat(129);
        for bad in [
            "",
            "egress flow",
            "egress\u{202E}wolf",
            "r\u{E4}te",
            "a\u{FFFD}b",
            "a/b",
            overlong_rule_id.as_str(),
        ] {
            assert!(!is_rule_id(bad), "{bad:?}");
        }

        assert!(is_dedup_key("0123456789abcdef0123456789abcdef"));
        for bad in [
            "",
            "0123456789ABCDEF0123456789ABCDEF",
            "0123456789abcdef0123456789abcde",
            "0123456789abcdef0123456789abcdef0",
        ] {
            assert!(!is_dedup_key(bad), "{bad:?}");
        }
        Ok(())
    }

    #[test]
    fn test_untrusted_text_renders_only_escaped() -> TestResult {
        let text = UntrustedText::new("ok\u{1B}[31m\u{202E}\u{200B}\u{E0041}\nFAKE".to_owned());
        let line = text.escaped_line(256);
        assert_eq!(
            line,
            "ok\\u{001b}[31m\\u{202e}\\u{200b}\\u{e0041}\\u{000a}FAKE"
        );
        if !harw_untrusted_text::is_clean(&line) || line.contains('\n') {
            return Err(TestError::Unexpected(format!(
                "raw character survived: {line:?}"
            )));
        }
        // Bounded in characters, escapes and the mark included.
        assert!(text.escaped_line(8).chars().count() <= 8);
        // Debug shows the length only.
        let debug = format!("{text:?}");
        assert!(!debug.contains("FAKE"), "{debug}");
        assert!(!debug.contains('\u{1B}'), "{debug}");
        // The raw text is unchanged behind its explicit accessor.
        assert!(text.as_untrusted_str().contains('\u{202E}'));
        Ok(())
    }

    #[test]
    fn test_debug_of_a_finding_leaves_the_summary_out() -> TestResult {
        let posture = parse(OK_DOCUMENT.as_bytes())?;
        let first = posture
            .dod
            .findings
            .first()
            .ok_or(TestError::Missing("first"))?;
        let debug = format!("{first:?}");
        assert!(!debug.contains("203.0.113.7"), "{debug}");
        assert!(debug.contains("UntrustedText(<26 bytes>)"), "{debug}");
        // Valid ids are shown.
        assert!(debug.contains("egress-flow"), "{debug}");
        Ok(())
    }

    #[tokio::test]
    async fn test_posture_round_trip_through_the_client() -> TestResult {
        let (_dir, client, task) = fixed_hub(StatusCode::OK, JSON, OK_DOCUMENT).await?;
        let posture = client.posture().await?;
        assert_eq!(posture.dod.status, DodPostureStatus::Ok);
        assert_eq!(posture.dod.findings.len(), 2);
        assert_eq!(posture.principal_id.as_deref(), Some("alice"));
        task.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_posture_unknown_peer_is_forbidden() -> TestResult {
        let (_dir, client, task) =
            fixed_hub(StatusCode::FORBIDDEN, JSON, r#"{"error":"unknown_peer"}"#).await?;
        assert_eq!(client.posture().await, Err(InfraClientError::Forbidden));
        task.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_posture_rejects_wrong_content_type_and_malformed_json() -> TestResult {
        let (_dir, client, task) = fixed_hub(StatusCode::OK, "text/plain", OK_DOCUMENT).await?;
        let result = client.posture().await;
        if !matches!(result, Err(InfraClientError::Protocol(_))) {
            return Err(TestError::Unexpected(format!(
                "text/plain accepted: {result:?}"
            )));
        }
        task.abort();

        let (_dir, client, task) = fixed_hub(StatusCode::OK, JSON, r#"{"status":"ok"}"#).await?;
        assert_eq!(
            client.posture().await,
            Err(InfraClientError::Protocol("malformed posture document"))
        );
        task.abort();
        Ok(())
    }

    #[tokio::test]
    async fn test_posture_unreachable_hub_is_unavailable() -> TestResult {
        let dir = tempfile::tempdir()?;
        let client =
            SecurityHubClient::new(dir.path().join("absent.sock"), ClientOptions::default());
        assert_eq!(client.posture().await, Err(InfraClientError::Unavailable));
        Ok(())
    }
}
