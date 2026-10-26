//! Bounded validator for a child agent's return envelope (PL-90 W03d-3,
//! `05-error-learning-and-run-isolation.md` §2.2).
//!
//! A child returns a small, typed envelope to its parent: a status, a short
//! summary, evidence references, a token count and an optional failure code.
//! The parent never receives the child's transcript. [`sanitize`] is the one
//! gate between the child and the parent:
//!
//! - Structural violations are **refused**, never silently repaired: too many
//!   references, over-long or non-prompt-safe locators, locators outside the
//!   child's own namespace, control characters in the summary and malformed
//!   failure codes.
//! - The summary is **truncated** on a char boundary (and flagged), because an
//!   over-long summary is a size problem, not a trust problem.
//! - The trust of every reference is **clamped** to
//!   [`EvidenceTrust::Reported`]: a child can never claim that its own
//!   evidence was observed or verified; only the trusted runtime upgrades it.
//! - `deny_unknown_fields` makes an extra field (for example `transcript`) a
//!   deserialization error.

use std::fmt;

use harw_types::ContentDigest;
use serde::{Deserialize, Serialize};

use crate::intent_cycle::{EvidenceTrust, is_prompt_safe_char};

/// Maximum summary length in chars; longer summaries are truncated.
pub const MAX_SUMMARY_CHARS: usize = 1024;
/// Maximum number of evidence references per envelope.
pub const MAX_EVIDENCE_REFS: usize = 16;
/// Maximum locator length in chars.
pub const MAX_LOCATOR_CHARS: usize = 256;
/// Maximum failure code length in chars.
pub const MAX_FAILURE_CODE_CHARS: usize = 64;

/// Terminal status a child reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChildStatus {
    /// The child finished its task.
    Succeeded,
    /// The child failed; see the failure code.
    Failed,
    /// The child could not proceed.
    Blocked,
    /// The child was cancelled.
    Cancelled,
}

/// One evidence reference returned by a child.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    /// Where the evidence lives; must stay inside the child's namespace.
    pub locator: String,
    /// Digest of the referenced content.
    pub digest: ContentDigest,
    /// Claimed trust; always clamped to `Reported` by [`sanitize`].
    pub trust: EvidenceTrust,
}

/// The envelope a child returns to its parent. Carries no transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildReturnEnvelope {
    /// Terminal status.
    pub status: ChildStatus,
    /// Short human-readable summary.
    pub summary: String,
    /// Evidence references, at most [`MAX_EVIDENCE_REFS`].
    pub evidence_refs: Vec<EvidenceRef>,
    /// Tokens the child consumed.
    pub usage_tokens: u64,
    /// Machine-readable failure code, prompt-safe and bounded.
    pub failure_code: Option<String>,
}

/// A validated envelope plus a note on whether the summary was shortened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizedEnvelope {
    /// The validated envelope with clamped trust and bounded summary.
    pub envelope: ChildReturnEnvelope,
    /// True if the summary was truncated to [`MAX_SUMMARY_CHARS`].
    pub summary_truncated: bool,
}

/// Why an envelope was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvelopeRefusal {
    /// More than [`MAX_EVIDENCE_REFS`] references.
    TooManyRefs,
    /// A locator longer than [`MAX_LOCATOR_CHARS`].
    LocatorTooLong,
    /// A locator with a character outside the prompt-safe set.
    UnsafeLocator,
    /// A locator outside the expected child namespace.
    ForeignLocator,
    /// A control character other than newline or tab in the summary.
    SummaryControlChars,
    /// A failure code that is empty, too long or not prompt-safe.
    BadFailureCode,
}

impl fmt::Display for EnvelopeRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::TooManyRefs => "child return has too many evidence references",
            Self::LocatorTooLong => "evidence locator is too long",
            Self::UnsafeLocator => "evidence locator contains non-prompt-safe characters",
            Self::ForeignLocator => "evidence locator is outside the child namespace",
            Self::SummaryControlChars => "summary contains control characters",
            Self::BadFailureCode => "failure code is empty, too long or not prompt-safe",
        };
        f.write_str(text)
    }
}

impl std::error::Error for EnvelopeRefusal {}

/// Validates a child's return envelope for the parent.
///
/// `expected_child_prefix` is supplied by the trusted runtime (for example the
/// child's session id namespace); every locator must start with it.
///
/// # Errors
/// An [`EnvelopeRefusal`] for the first violated bound; nothing is repaired.
pub fn sanitize(
    raw: ChildReturnEnvelope,
    expected_child_prefix: &str,
) -> Result<SanitizedEnvelope, EnvelopeRefusal> {
    if raw.evidence_refs.len() > MAX_EVIDENCE_REFS {
        return Err(EnvelopeRefusal::TooManyRefs);
    }
    for evidence in &raw.evidence_refs {
        if evidence.locator.chars().count() > MAX_LOCATOR_CHARS {
            return Err(EnvelopeRefusal::LocatorTooLong);
        }
        if !evidence.locator.chars().all(is_prompt_safe_char) {
            return Err(EnvelopeRefusal::UnsafeLocator);
        }
        if !evidence.locator.starts_with(expected_child_prefix) {
            return Err(EnvelopeRefusal::ForeignLocator);
        }
    }
    if raw
        .summary
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(EnvelopeRefusal::SummaryControlChars);
    }
    if let Some(code) = &raw.failure_code {
        if code.is_empty()
            || code.chars().count() > MAX_FAILURE_CODE_CHARS
            || !code.chars().all(is_prompt_safe_char)
        {
            return Err(EnvelopeRefusal::BadFailureCode);
        }
    }

    let ChildReturnEnvelope {
        status,
        summary,
        evidence_refs,
        usage_tokens,
        failure_code,
    } = raw;
    let summary_truncated = summary.chars().count() > MAX_SUMMARY_CHARS;
    let summary = if summary_truncated {
        summary.chars().take(MAX_SUMMARY_CHARS).collect()
    } else {
        summary
    };
    let evidence_refs = evidence_refs
        .into_iter()
        .map(|evidence| EvidenceRef {
            trust: EvidenceTrust::Reported,
            ..evidence
        })
        .collect();

    Ok(SanitizedEnvelope {
        envelope: ChildReturnEnvelope {
            status,
            summary,
            evidence_refs,
            usage_tokens,
            failure_code,
        },
        summary_truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    const PREFIX: &str = "child-1/";

    fn evidence(locator: &str, trust: EvidenceTrust) -> EvidenceRef {
        EvidenceRef {
            locator: locator.to_owned(),
            digest: ContentDigest::of(locator.as_bytes()),
            trust,
        }
    }

    fn envelope() -> ChildReturnEnvelope {
        ChildReturnEnvelope {
            status: ChildStatus::Succeeded,
            summary: "done".to_owned(),
            evidence_refs: vec![evidence("child-1/a.txt", EvidenceTrust::Reported)],
            usage_tokens: 42,
            failure_code: None,
        }
    }

    #[test]
    fn clamps_trust_of_every_ref_to_reported() -> TestResult {
        let mut raw = envelope();
        raw.evidence_refs = vec![
            evidence("child-1/a", EvidenceTrust::Verified),
            evidence("child-1/b", EvidenceTrust::Observed),
        ];
        let sanitized = sanitize(raw, PREFIX).map_err(ctx("valid envelope sanitizes"))?;
        assert_eq!(sanitized.envelope.evidence_refs.len(), 2);
        assert!(
            sanitized
                .envelope
                .evidence_refs
                .iter()
                .all(|e| e.trust == EvidenceTrust::Reported)
        );
        assert!(!sanitized.summary_truncated);
        Ok(())
    }

    #[test]
    fn refuses_foreign_locator() {
        let mut raw = envelope();
        raw.evidence_refs = vec![evidence("child-2/a.txt", EvidenceTrust::Reported)];
        assert_eq!(sanitize(raw, PREFIX), Err(EnvelopeRefusal::ForeignLocator));
    }

    #[test]
    fn refuses_unsafe_and_overlong_locators() {
        let mut raw = envelope();
        raw.evidence_refs = vec![evidence("child-1/a b", EvidenceTrust::Reported)];
        assert_eq!(sanitize(raw, PREFIX), Err(EnvelopeRefusal::UnsafeLocator));

        let mut raw = envelope();
        let long = format!("{PREFIX}{}", "a".repeat(MAX_LOCATOR_CHARS));
        raw.evidence_refs = vec![evidence(&long, EvidenceTrust::Reported)];
        assert_eq!(sanitize(raw, PREFIX), Err(EnvelopeRefusal::LocatorTooLong));
    }

    #[test]
    fn refuses_control_characters_but_allows_newline_and_tab() -> TestResult {
        let mut raw = envelope();
        raw.summary = "line one\n\tline two".to_owned();
        sanitize(raw, PREFIX).map_err(ctx("newline and tab are allowed"))?;

        for bad in ["esc\u{1b}[31m", "nul\0", "cr\r"] {
            let mut raw = envelope();
            raw.summary = bad.to_owned();
            assert_eq!(
                sanitize(raw, PREFIX),
                Err(EnvelopeRefusal::SummaryControlChars)
            );
        }
        Ok(())
    }

    #[test]
    fn truncates_multibyte_summary_on_char_boundary() -> TestResult {
        let mut raw = envelope();
        raw.summary = "ä".repeat(MAX_SUMMARY_CHARS + 5);
        let sanitized = sanitize(raw, PREFIX).map_err(ctx("long summary sanitizes"))?;
        assert!(sanitized.summary_truncated);
        assert_eq!(
            sanitized.envelope.summary.chars().count(),
            MAX_SUMMARY_CHARS
        );
        assert!(sanitized.envelope.summary.chars().all(|c| c == 'ä'));

        let mut raw = envelope();
        raw.summary = "ä".repeat(MAX_SUMMARY_CHARS);
        let exact = sanitize(raw, PREFIX).map_err(ctx("exact summary sanitizes"))?;
        assert!(!exact.summary_truncated);
        Ok(())
    }

    #[test]
    fn serde_rejects_extra_transcript_field() -> TestResult {
        let mut value = serde_json::to_value(envelope()).map_err(ctx("envelope serializes"))?;
        let object = value
            .as_object_mut()
            .ok_or(TestError::Missing("envelope is a JSON object"))?;
        object.insert(
            "transcript".to_owned(),
            serde_json::Value::String("secret reasoning".to_owned()),
        );
        let parsed = serde_json::from_value::<ChildReturnEnvelope>(value);
        assert!(parsed.is_err());
        Ok(())
    }

    #[test]
    fn enforces_the_ref_cap() -> TestResult {
        let refs = |n: usize| -> Vec<EvidenceRef> {
            (0..n)
                .map(|i| evidence(&format!("{PREFIX}{i}"), EvidenceTrust::Reported))
                .collect()
        };
        let mut raw = envelope();
        raw.evidence_refs = refs(MAX_EVIDENCE_REFS);
        sanitize(raw, PREFIX).map_err(ctx("cap-sized envelope sanitizes"))?;

        let mut raw = envelope();
        raw.evidence_refs = refs(MAX_EVIDENCE_REFS + 1);
        assert_eq!(sanitize(raw, PREFIX), Err(EnvelopeRefusal::TooManyRefs));
        Ok(())
    }

    #[test]
    fn refuses_bad_failure_codes() -> TestResult {
        let mut raw = envelope();
        raw.failure_code = Some("E_TIMEOUT".to_owned());
        sanitize(raw, PREFIX).map_err(ctx("good failure code sanitizes"))?;

        let too_long = "x".repeat(MAX_FAILURE_CODE_CHARS + 1);
        for bad in ["has space", "new\nline", "", too_long.as_str()] {
            let mut raw = envelope();
            raw.failure_code = Some(bad.to_owned());
            assert_eq!(sanitize(raw, PREFIX), Err(EnvelopeRefusal::BadFailureCode));
        }
        Ok(())
    }
}
