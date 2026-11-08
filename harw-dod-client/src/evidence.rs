//! The `evidence_card` of findings export v3 (PL-100 WP0.12).
//!
//! # Purpose
//! A v3 export line may carry a small, typed summary of the evidence behind
//! a finding: event counts per kind, the uid, the destination address, a
//! path and the acting process. It lets readers (operations, TUI, the
//! dispatch stage) show *what* was observed without the raw events.
//!
//! # Untrusted input
//! Every string in the card comes from attacker-influenced observations
//! (file names, process names, event kinds). The sentinel escapes them with
//! `harw-untrusted-text` before writing; this reader escapes them again with
//! [`harw_untrusted_text::escape_line`] (idempotent on already escaped text)
//! and bounds them, so a card from any writer is display-safe:
//!
//! - the card as a whole: at most [`MAX_EVIDENCE_CARD_BYTES`] serialized,
//!   otherwise it is dropped;
//! - `path`: at most [`MAX_EVIDENCE_PATH_CHARS`] characters;
//! - `process.comm`: at most [`MAX_EVIDENCE_COMM_CHARS`] characters;
//! - `event_counts`: at most [`MAX_EVENT_KINDS`] kinds of at most
//!   [`MAX_EVENT_KIND_CHARS`] characters each;
//! - `destination`: kept only when it parses as `ip:port`
//!   ([`std::net::SocketAddr`]).
//!
//! A card that is not an object, exceeds the size cap or has a value of the
//! wrong JSON type is dropped as a whole; the finding line itself is kept.
//! Unknown keys drop the card for `schema_version <= 3` and are ignored for
//! newer writers.
//!
//! # Concurrency
//! Plain data and pure functions; no shared state.
//!
//! # Examples
//! ```
//! use harw_dod_client::EvidenceCard;
//!
//! let card: EvidenceCard = serde_json::from_str(
//!     r#"{"event_counts":{"tcp_connect":3},"destination":"203.0.113.9:443",
//!         "process":{"pid":42,"comm":"curl\u202e"}}"#,
//! )?;
//! assert_eq!(card.event_counts.get("tcp_connect"), Some(&3));
//! assert_eq!(card.destination.map(|addr| addr.port()), Some(443));
//! assert_eq!(card.process.map(|p| p.comm).as_deref(), Some("curl\\u{202e}"));
//! # Ok::<(), serde_json::Error>(())
//! ```

use std::collections::BTreeMap;
use std::net::SocketAddr;

use harw_untrusted_text::escape_line;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Largest accepted `evidence_card`, serialized as compact JSON (4 KiB).
pub const MAX_EVIDENCE_CARD_BYTES: usize = 4 * 1024;

/// Longest kept `path`, in characters (escapes and `…` included).
pub const MAX_EVIDENCE_PATH_CHARS: usize = 256;

/// Longest kept `process.comm`, in characters (escapes and `…` included).
pub const MAX_EVIDENCE_COMM_CHARS: usize = 32;

/// Longest kept event kind in `event_counts`, in characters.
pub const MAX_EVENT_KIND_CHARS: usize = 64;

/// Most event kinds kept in `event_counts` (the first ones in key order).
pub const MAX_EVENT_KINDS: usize = 32;

// The keys a v3 card may carry.
const CARD_FIELDS: [&str; 5] = ["event_counts", "uid", "destination", "path", "process"];

// The keys a v3 `process` object may carry.
const PROCESS_FIELDS: [&str; 2] = ["pid", "comm"];

/// Bounded, escaped summary of the evidence behind one finding (export v3).
///
/// Deserializing always escapes and bounds the strings (see the module
/// documentation); the size cap and the unknown-key rule are applied by the
/// findings reader, which sees the raw JSON.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "RawEvidenceCard")]
pub struct EvidenceCard {
    /// Number of observed events per event kind (escaped kinds).
    pub event_counts: BTreeMap<String, u64>,
    /// uid of the observed actor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
    /// Remote address of a network event (`ip:port`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub destination: Option<SocketAddr>,
    /// Escaped path, at most [`MAX_EVIDENCE_PATH_CHARS`] characters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The acting process.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process: Option<EvidenceProcess>,
}

/// The acting process of an [`EvidenceCard`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "RawEvidenceProcess")]
pub struct EvidenceProcess {
    /// Process id.
    pub pid: u32,
    /// Escaped command name, at most [`MAX_EVIDENCE_COMM_CHARS`] characters.
    pub comm: String,
}

// Wire form of a card before escaping and bounding.
#[derive(Deserialize)]
struct RawEvidenceCard {
    #[serde(default)]
    event_counts: BTreeMap<String, u64>,
    #[serde(default)]
    uid: Option<u32>,
    #[serde(default)]
    destination: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    process: Option<EvidenceProcess>,
}

// Wire form of a process before escaping and bounding.
#[derive(Deserialize)]
struct RawEvidenceProcess {
    pid: u32,
    comm: String,
}

impl From<RawEvidenceCard> for EvidenceCard {
    fn from(raw: RawEvidenceCard) -> Self {
        let mut event_counts: BTreeMap<String, u64> = BTreeMap::new();
        for (kind, count) in raw.event_counts {
            let kind = escape_line(&kind, MAX_EVENT_KIND_CHARS);
            if kind.is_empty() {
                continue;
            }
            // Two raw kinds may escape/truncate to the same key: their counts
            // add up instead of one silently replacing the other.
            if let Some(slot) = event_counts.get_mut(&kind) {
                *slot = slot.saturating_add(count);
            } else if event_counts.len() < MAX_EVENT_KINDS {
                event_counts.insert(kind, count);
            }
        }
        Self {
            event_counts,
            uid: raw.uid,
            destination: raw
                .destination
                .and_then(|text| text.parse::<SocketAddr>().ok()),
            path: raw
                .path
                .map(|path| escape_line(&path, MAX_EVIDENCE_PATH_CHARS))
                .filter(|path| !path.is_empty()),
            process: raw.process,
        }
    }
}

impl From<RawEvidenceProcess> for EvidenceProcess {
    fn from(raw: RawEvidenceProcess) -> Self {
        Self {
            pid: raw.pid,
            comm: escape_line(&raw.comm, MAX_EVIDENCE_COMM_CHARS),
        }
    }
}

impl EvidenceCard {
    /// The card of an export line, from its raw JSON value.
    ///
    /// # Arguments
    /// - `value`: the `evidence_card` value of the line.
    /// - `lenient`: `true` for lines from a writer newer than this reader
    ///   (`schema_version > 3`): unknown keys are then ignored instead of
    ///   dropping the card.
    ///
    /// # Returns
    /// `None` when the card is larger than [`MAX_EVIDENCE_CARD_BYTES`], is
    /// not an object, has unknown keys (strict mode) or a value of the wrong
    /// type.
    pub(crate) fn from_export_value(value: Value, lenient: bool) -> Option<Self> {
        let encoded = serde_json::to_vec(&value).ok()?;
        if encoded.len() > MAX_EVIDENCE_CARD_BYTES {
            return None;
        }
        let Value::Object(mut card) = value else {
            return None;
        };
        if !restrict_keys(&mut card, &CARD_FIELDS, lenient) {
            return None;
        }
        if let Some(Value::Object(process)) = card.get_mut("process")
            && !restrict_keys(process, &PROCESS_FIELDS, lenient)
        {
            return None;
        }
        serde_json::from_value(Value::Object(card)).ok()
    }
}

/// `lenient`: removes keys not in `known` and returns `true`. Strict:
/// returns whether every key is in `known`.
fn restrict_keys(object: &mut Map<String, Value>, known: &[&str], lenient: bool) -> bool {
    if lenient {
        object.retain(|key, _| known.contains(&key.as_str()));
        true
    } else {
        object.keys().all(|key| known.contains(&key.as_str()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn card_strings_are_escaped_and_bounded() -> TestResult {
        let value = json!({
            "event_counts": {"tcp_connect": 3, "exec\u{1b}[31m": 1},
            "uid": 1000,
            "destination": "203.0.113.9:443",
            "path": format!("/tmp/{}\u{202e}gpj.exe", "a".repeat(400)),
            "process": {"pid": 42, "comm": "a-very-long-command-name-beyond-32-chars\u{200b}"},
        });
        let card = EvidenceCard::from_export_value(value, false).ok_or("card must parse")?;
        assert_eq!(card.uid, Some(1000));
        assert_eq!(card.event_counts.get("tcp_connect"), Some(&3));
        assert_eq!(card.event_counts.get("exec\\u{001b}[31m"), Some(&1));
        assert_eq!(
            card.destination,
            Some("203.0.113.9:443".parse::<SocketAddr>()?)
        );
        let path = card.path.as_deref().ok_or("path kept")?;
        assert_eq!(path.chars().count(), MAX_EVIDENCE_PATH_CHARS);
        assert!(path.ends_with('…'));
        assert!(harw_untrusted_text::is_clean(path));
        let process = card.process.as_ref().ok_or("process kept")?;
        assert_eq!(process.pid, 42);
        assert!(process.comm.chars().count() <= MAX_EVIDENCE_COMM_CHARS);
        assert!(harw_untrusted_text::is_clean(&process.comm));
        Ok(())
    }

    #[test]
    fn escaping_is_idempotent_so_the_card_round_trips() -> TestResult {
        let value = json!({
            "event_counts": {"open\u{2066}": 2},
            "destination": "[2001:db8::1]:8443",
            "path": "/srv/\u{e0041}x",
            "process": {"pid": 7, "comm": "sh\n"},
        });
        let card = EvidenceCard::from_export_value(value, false).ok_or("card must parse")?;
        assert_eq!(card.path.as_deref(), Some("/srv/\\u{e0041}x"));
        assert_eq!(
            card.process.as_ref().map(|p| p.comm.as_str()),
            Some("sh\\u{000a}")
        );
        let json = serde_json::to_string(&card)?;
        let again: EvidenceCard = serde_json::from_str(&json)?;
        assert_eq!(again, card);
        let again = EvidenceCard::from_export_value(serde_json::from_str(&json)?, false)
            .ok_or("re-read card must parse")?;
        assert_eq!(again, card);
        Ok(())
    }

    #[test]
    fn oversized_malformed_and_unknown_cards_are_dropped() {
        let big = json!({"path": "x".repeat(MAX_EVIDENCE_CARD_BYTES)});
        assert!(EvidenceCard::from_export_value(big, false).is_none());
        assert!(EvidenceCard::from_export_value(json!("card"), false).is_none());
        assert!(EvidenceCard::from_export_value(json!({"uid": -1}), false).is_none());
        assert!(EvidenceCard::from_export_value(json!({"uid": "root"}), false).is_none());
        assert!(
            EvidenceCard::from_export_value(json!({"event_counts": {"a": -2}}), false).is_none()
        );
        assert!(
            EvidenceCard::from_export_value(json!({"process": {"comm": "sh"}}), false).is_none()
        );
        // Unknown keys: refused up to v3, ignored for a newer writer.
        let unknown =
            json!({"uid": 5, "future": true, "process": {"pid": 1, "comm": "x", "tid": 2}});
        assert!(EvidenceCard::from_export_value(unknown.clone(), false).is_none());
        let lenient = EvidenceCard::from_export_value(unknown, true);
        assert_eq!(lenient.as_ref().and_then(|card| card.uid), Some(5));
        assert_eq!(
            lenient.and_then(|card| card.process).map(|p| p.pid),
            Some(1)
        );
        let nested = json!({"process": {"pid": 1, "comm": "x", "tid": 2}});
        assert!(EvidenceCard::from_export_value(nested, false).is_none());
    }

    #[test]
    fn invalid_destination_and_empty_strings_are_dropped_not_the_card() -> TestResult {
        for destination in ["203.0.113.9", "::1:443", "example.org:443", "1.2.3.4:99999"] {
            let card = EvidenceCard::from_export_value(
                json!({"destination": destination, "path": "", "event_counts": {"": 4}}),
                false,
            )
            .ok_or("card must parse")?;
            assert_eq!(card.destination, None, "{destination}");
            assert_eq!(card.path, None);
            assert!(card.event_counts.is_empty());
        }
        Ok(())
    }

    #[test]
    fn event_kinds_are_capped_and_colliding_kinds_add_up() -> TestResult {
        let mut counts = Map::new();
        for index in 0..(MAX_EVENT_KINDS + 8) {
            counts.insert(format!("kind-{index:03}"), json!(1));
        }
        let card = EvidenceCard::from_export_value(json!({"event_counts": counts}), false)
            .ok_or("card must parse")?;
        assert_eq!(card.event_counts.len(), MAX_EVENT_KINDS);
        assert!(card.event_counts.contains_key("kind-000"));

        // Both kinds truncate to the same escaped key.
        let long = "k".repeat(MAX_EVENT_KIND_CHARS + 4);
        let card = EvidenceCard::from_export_value(
            json!({"event_counts": {format!("{long}a"): u64::MAX, format!("{long}b"): 5}}),
            false,
        )
        .ok_or("card must parse")?;
        assert_eq!(card.event_counts.len(), 1);
        assert_eq!(card.event_counts.values().next(), Some(&u64::MAX));
        Ok(())
    }
}
