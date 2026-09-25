//! `harwness.agent-child/v1` — the JSON-lines protocol between a parent
//! runner and a child agent it runs as a separate process (plan
//! `docs/plans/r10-agent-compiler.md`, §3C).
//!
//! # Description
//! One JSON object per line, `\n`-terminated, UTF-8. Parent → child frames
//! ([`ParentToChild`]) travel on the child's **stdin**; child → parent frames
//! ([`ChildToParent`]) on the child's **stdout**. The child's **stderr** is
//! logs only — free text, never a protocol line, and never parsed as one.
//!
//! Every frame is a JSON object tagged by a `"type"` field naming the variant
//! (`#[serde(tag = "type")]`), so an unrecognized future variant fails to
//! parse loudly instead of silently matching the wrong arm.
//!
//! # Handshake
//! The child's first frame is always [`ChildToParent::Hello`], carrying the
//! protocol string it implements. [`verify_protocol`] is the version check
//! the parent (`harw-agent-runner`'s `JobChildBackend`) runs against it
//! before sending anything else: today it demands an exact match against
//! [`PROTOCOL_VERSION`], so a version skew between a runner binary and an
//! older/newer compiled child fails closed with a readable reason rather
//! than desyncing the stream.
//!
//! # Stability
//! This is a private wire format between one `harw-agent-runner` binary and
//! itself (parent and child are always built from the same executable, see
//! `crate::child::run_child`), not a public API — variants may gain fields
//! across versions as long as [`PROTOCOL_VERSION`] is bumped alongside any
//! change that isn't purely additive.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// The protocol this file implements. Sent by the child in
/// [`ChildToParent::Hello`] and checked by the parent via [`verify_protocol`].
pub const PROTOCOL_VERSION: &str = "harwness.agent-child/v1";

/// One frame sent from the parent to a child, over the child's stdin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ParentToChild {
    /// Starts (or resumes) the child's one turn.
    Task {
        /// The child's task text (its user turn).
        task: String,
        /// Structured context handed alongside `task`, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        context: Option<serde_json::Value>,
        /// Opaque continuation token from a prior budget-ended run of this
        /// same child, if this resumes one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        continue_from: Option<String>,
    },
    /// A follow-up message while the child's turn is already running
    /// (parent-to-child chat, distinct from the initial `Task`).
    Message {
        /// The message text.
        text: String,
    },
    /// The parent's reply to a [`ChildToParent::Question`] or
    /// [`ChildToParent::ApprovalRequest`], matched by id.
    Answer {
        /// The `id` of the question or approval request this answers.
        question_id: String,
        /// The reply text. For an approval request, `"approve"` approves and
        /// any other text denies with that text as the reason (the
        /// convention `JobChildBackend`/`run_child` use end to end).
        text: String,
    },
    /// Cancels the child's run. Terminal: the child does not resume after
    /// this.
    Cancel,
    /// Updates the child's budget mid-run.
    Budget {
        /// Model tokens over the whole child session, if capped.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_tokens: Option<u64>,
        /// Tool calls over the whole child session, if capped.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_tool_calls: Option<u64>,
        /// Wall-clock budget in milliseconds, if capped.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_wall_time_ms: Option<u64>,
    },
    /// Switches the child between plan and live mode.
    Mode {
        /// The mode to switch to.
        mode: ChildMode,
    },
    /// Narrows the child's rights mid-run (a backend only ever narrows;
    /// widening past the child's own manifest is never valid).
    Rights {
        /// The rights to apply.
        rights: ChildRights,
    },
}

/// A child's live mode, mirrored from the parent's own (Runde 9, E6:
/// `crate::live_mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildMode {
    /// The child plans but does not act.
    Plan,
    /// The child acts.
    Live,
}

/// Rights carried over the wire, mirroring
/// `harw_runtime::embedded::EffectiveRights`'s shape (this crate cannot name
/// that type directly in a protocol meant to also serialize cleanly and stay
/// stable independent of that crate's internals).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildRights {
    /// Exact tool names the child may call.
    #[serde(default)]
    pub tools: BTreeSet<String>,
    /// Hosts the child may reach over the network.
    #[serde(default)]
    pub network_hosts: BTreeSet<String>,
    /// Unrestricted network access (beyond `network_hosts`).
    #[serde(default)]
    pub network_open: bool,
    /// Write access to the workspace.
    #[serde(default)]
    pub write: bool,
    /// Shell execution.
    #[serde(default)]
    pub shell: bool,
    /// Host-level (unsandboxed) execution.
    #[serde(default)]
    pub host: bool,
    /// The manifest's full-access escape hatch.
    #[serde(default)]
    pub full_access: bool,
}

/// One frame sent from a child to the parent, over the child's stdout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChildToParent {
    /// Always the child's first frame.
    Hello {
        /// The child's own agent id inside the bundle.
        agent_id: String,
        /// The protocol the child implements (compare with
        /// [`PROTOCOL_VERSION`] via [`verify_protocol`]).
        protocol: String,
        /// The bundle's artifact digest, so the parent can confirm it started
        /// the binary it thinks it did.
        digest: String,
    },
    /// A translated live event of the child's session
    /// (`harwness_sdk::SdkEvent`, translated to JSON by
    /// `crate::child::translate_event` — this file names no SDK type so the
    /// wire format does not change every time that crate's event enum
    /// grows a field).
    Event {
        /// The translated event.
        sdk_event: serde_json::Value,
    },
    /// The child has a free-text question for the parent (e.g. an
    /// interactive-input tool); resolved by a matching
    /// [`ParentToChild::Answer`].
    Question {
        /// A fresh id, unique for this child's lifetime.
        id: String,
        /// The question text.
        text: String,
    },
    /// The child asks approval for a held tool call; resolved by a matching
    /// [`ParentToChild::Answer`] (`"approve"` or a deny reason).
    ApprovalRequest {
        /// A fresh id, unique for this child's lifetime.
        id: String,
        /// The tool name.
        tool: String,
        /// A short, human-readable summary of the call's arguments (never
        /// the raw arguments verbatim, to keep this frame small and to avoid
        /// echoing anything the parent should not print unredacted).
        args_summary: String,
    },
    /// The child's run ended.
    Result {
        /// How it ended.
        status: ChildResultStatus,
        /// The final (or last, for a budget end) assistant text, if any.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// Consumption of this run.
        #[serde(default)]
        usage: ChildUsage,
    },
    /// A standalone usage update, sent independently of [`Self::Result`]
    /// (e.g. after each model round) so the parent can show live
    /// consumption without waiting for the run to end.
    Usage {
        /// The usage snapshot.
        usage: ChildUsage,
    },
    /// The child hit an error it could not otherwise report (protocol
    /// decode failure, panic caught at the top level, ...). Distinct from
    /// [`ChildResultStatus::Failed`], which is a *result* — this is sent
    /// when the child cannot even produce one.
    Error {
        /// Human-readable message.
        message: String,
    },
}

/// How a child's run ended, carried in [`ChildToParent::Result`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildResultStatus {
    /// Regular completion.
    Completed,
    /// Cancelled (via [`ParentToChild::Cancel`] or the child's own budget).
    Cancelled,
    /// Any other non-successful end (provider/turn error, refusal, ...).
    Failed,
    /// The run's budget ended it; `text` is the last (or handed-off) answer.
    BudgetExhausted,
}

/// Token/tool/time consumption of a child run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildUsage {
    /// Fresh input tokens (uncached).
    #[serde(default)]
    pub input_tokens: u64,
    /// Output tokens.
    #[serde(default)]
    pub output_tokens: u64,
    /// Tokens read from a prompt cache, if the provider reports them.
    #[serde(default)]
    pub cached_input_tokens: u64,
    /// Tool calls executed.
    #[serde(default)]
    pub tool_calls: u64,
    /// Wall-clock time consumed, in milliseconds.
    #[serde(default)]
    pub wall_time_ms: u64,
}

impl ChildUsage {
    /// Sum of fresh input and output tokens (excludes cached reads),
    /// matching `harw_types::TokenUsage::fresh_tokens`'s accounting unit.
    #[must_use]
    pub fn fresh_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
}

/// A malformed line or a version mismatch at the Hello handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    /// The line was not valid JSON, or not a recognized frame shape.
    Malformed(String),
    /// The child's [`ChildToParent::Hello`] named a different protocol than
    /// [`PROTOCOL_VERSION`].
    VersionMismatch {
        /// What this binary implements.
        expected: String,
        /// What the child reported.
        got: String,
    },
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(reason) => write!(f, "malformed child protocol line: {reason}"),
            Self::VersionMismatch { expected, got } => write!(
                f,
                "child protocol version mismatch: expected {expected}, got {got}"
            ),
        }
    }
}

impl std::error::Error for ProtocolError {}

/// Encodes one frame as a single `\n`-terminated JSON line.
///
/// # Errors
/// Never, in practice — every field here is a plain, non-cyclic value — but
/// `serde_json` is fallible in general, so this stays a `Result` rather than
/// panicking on a future field that turns out not to serialize.
pub fn encode_line<T: Serialize>(frame: &T) -> Result<String, ProtocolError> {
    let mut line = serde_json::to_string(frame).map_err(|error| {
        ProtocolError::Malformed(format!("failed to encode protocol frame: {error}"))
    })?;
    line.push('\n');
    Ok(line)
}

/// Decodes one line (without its trailing newline) as a frame.
///
/// # Errors
/// [`ProtocolError::Malformed`] if `line` is not valid JSON or does not
/// match any tagged variant of `T`.
pub fn decode_line<T: for<'de> Deserialize<'de>>(line: &str) -> Result<T, ProtocolError> {
    serde_json::from_str(line.trim_end_matches(['\n', '\r']))
        .map_err(|error| ProtocolError::Malformed(error.to_string()))
}

/// Checks a child's reported protocol string (from [`ChildToParent::Hello`])
/// against [`PROTOCOL_VERSION`].
///
/// # Errors
/// [`ProtocolError::VersionMismatch`] if `reported` is not exactly
/// [`PROTOCOL_VERSION`]. There is exactly one supported version today, so
/// this is a strict equality check rather than a compatibility range.
pub fn verify_protocol(reported: &str) -> Result<(), ProtocolError> {
    if reported == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(ProtocolError::VersionMismatch {
            expected: PROTOCOL_VERSION.to_owned(),
            got: reported.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(frame: T)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let line = encode_line(&frame).expect("encode");
        assert!(line.ends_with('\n'));
        let decoded: T = decode_line(&line).expect("decode");
        assert_eq!(decoded, frame);
    }

    #[test]
    fn parent_to_child_task_roundtrips() {
        roundtrip(ParentToChild::Task {
            task: "review the PR".to_owned(),
            context: Some(serde_json::json!({"pr": 42})),
            continue_from: None,
        });
    }

    #[test]
    fn parent_to_child_every_variant_roundtrips() {
        roundtrip(ParentToChild::Message {
            text: "hurry up".to_owned(),
        });
        roundtrip(ParentToChild::Answer {
            question_id: "appr-1".to_owned(),
            text: "approve".to_owned(),
        });
        roundtrip(ParentToChild::Cancel);
        roundtrip(ParentToChild::Budget {
            max_tokens: Some(1_000),
            max_tool_calls: None,
            max_wall_time_ms: Some(60_000),
        });
        roundtrip(ParentToChild::Mode {
            mode: ChildMode::Live,
        });
        roundtrip(ParentToChild::Rights {
            rights: ChildRights {
                tools: BTreeSet::from(["fs.read".to_owned()]),
                write: false,
                ..Default::default()
            },
        });
    }

    #[test]
    fn child_to_parent_hello_roundtrips() {
        roundtrip(ChildToParent::Hello {
            agent_id: "evidence-critic".to_owned(),
            protocol: PROTOCOL_VERSION.to_owned(),
            digest: "blake3:deadbeef".to_owned(),
        });
    }

    #[test]
    fn child_to_parent_every_variant_roundtrips() {
        roundtrip(ChildToParent::Event {
            sdk_event: serde_json::json!({"kind": "message", "text": "hi"}),
        });
        roundtrip(ChildToParent::Question {
            id: "q-1".to_owned(),
            text: "which file?".to_owned(),
        });
        roundtrip(ChildToParent::ApprovalRequest {
            id: "appr-1".to_owned(),
            tool: "fs.write".to_owned(),
            args_summary: "write 12 lines to src/lib.rs".to_owned(),
        });
        roundtrip(ChildToParent::Result {
            status: ChildResultStatus::Completed,
            text: Some("done".to_owned()),
            usage: ChildUsage {
                input_tokens: 10,
                output_tokens: 5,
                cached_input_tokens: 0,
                tool_calls: 2,
                wall_time_ms: 1_500,
            },
        });
        roundtrip(ChildToParent::Usage {
            usage: ChildUsage::default(),
        });
        roundtrip(ChildToParent::Error {
            message: "boom".to_owned(),
        });
    }

    #[test]
    fn decode_rejects_malformed_json() {
        let error = decode_line::<ChildToParent>("not json").unwrap_err();
        assert!(matches!(error, ProtocolError::Malformed(_)));
    }

    #[test]
    fn decode_rejects_unknown_tag() {
        let error = decode_line::<ChildToParent>(r#"{"type":"not_a_real_frame"}"#).unwrap_err();
        assert!(matches!(error, ProtocolError::Malformed(_)));
    }

    #[test]
    fn verify_protocol_accepts_exact_match() {
        assert!(verify_protocol(PROTOCOL_VERSION).is_ok());
    }

    #[test]
    fn verify_protocol_rejects_mismatch() {
        let error = verify_protocol("harwness.agent-child/v2").unwrap_err();
        match error {
            ProtocolError::VersionMismatch { expected, got } => {
                assert_eq!(expected, PROTOCOL_VERSION);
                assert_eq!(got, "harwness.agent-child/v2");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn hello_serializes_with_a_type_tag() {
        let line = encode_line(&ChildToParent::Hello {
            agent_id: "a".to_owned(),
            protocol: PROTOCOL_VERSION.to_owned(),
            digest: "d".to_owned(),
        })
        .expect("encode");
        let value: serde_json::Value = serde_json::from_str(line.trim_end()).expect("parse");
        assert_eq!(value["type"], "hello");
    }

    #[test]
    fn usage_fresh_tokens_excludes_cache_reads() {
        let usage = ChildUsage {
            input_tokens: 100,
            output_tokens: 50,
            cached_input_tokens: 80,
            tool_calls: 0,
            wall_time_ms: 0,
        };
        assert_eq!(usage.fresh_tokens(), 150);
    }
}
