//! Die Triage-Einreichung an den Escalator: [`TriageSubmission`] (C-WPROTO, W3; F-023).
//!
//! # Verantwortungsbereich
//! Was ein modellnaher Triage-Prozess (W5 D-TRIAGE, `harw-cli/src/triage.rs`)
//! dem Escalator (W5 D-ESC, `harw-escalator`) über den Submission-Socket
//! schickt: **nur** ein Urteil ([`SecurityVerdict`]) plus die Kennung und den
//! Inhaltsdigest des Finding-Records, auf den sich das Urteil bezieht. Keine
//! Aktion, keine Stufe, keine cgroup, kein Schlüssel — die leitet der
//! Escalator ausschließlich aus dem Record ab, den er **selbst** aus dem Spool
//! liest (Plan Teil B, „Warden-Proof v2“). Damit kann ein Modell weder
//! Befundfelder nachträglich verändern (F-023) noch eine Aktion erfinden.
//!
//! # Bindung
//! Drei Werte müssen übereinstimmen, bevor der Escalator das Urteil verwendet:
//! 1. `finding`/`finding_digest` dieser Einreichung,
//! 2. `verdict.finding()`/`verdict.bound_evidence()` im Urteil,
//! 3. Kennung und selbst berechneter Digest des Records aus dem Spool.
//!
//! [`TriageSubmission::new`] erzwingt 1 == 2 beim Bauen,
//! [`TriageSubmission::validate`] prüft es erneut (und Version, Vertrag,
//! Pflichtfelder) nach dem Deserialisieren, [`TriageSubmission::binds`]
//! schließt 3 ein.
//!
//! # Wire-Format (eingefroren, siehe Ledger `docs/remediation/ledger/W3/C-WPROTO.md`)
//! JSON-Objekt, `deny_unknown_fields`:
//! `{"version": 1, "finding": "<FindingId>", "finding_digest": "<hex>", "verdict": <SecurityVerdict>}`.
//!
//! # Nebenläufigkeit
//! Reine Werttypen, `Send + Sync`.
//!
//! # Fehler
//! [`SubmissionError`] — inhaltsfreie Meldungen. D-ESC darf ihn in
//! `EscalateError` einfalten.
//!
//! # Examples
//! ```rust
//! use harw_dod_escalate::submission::TriageSubmission;
//! use harw_dod_signals::{SecurityVerdict, Severity, VerdictClassification};
//! use harw_types::{ContentDigest, FindingId};
//!
//! let finding = FindingId::try_from_str("finding-1").unwrap();
//! let digest = ContentDigest::of(b"record-bytes");
//! let verdict = SecurityVerdict::new(
//!     finding.clone(), digest, VerdictClassification::Confirmed, Severity::High,
//!     "bestätigt".to_owned(), None, "security-triage".to_owned(), jiff::Timestamp::UNIX_EPOCH,
//! );
//! let submission = TriageSubmission::new(finding.clone(), digest, verdict).unwrap();
//! assert!(submission.binds(&finding, digest).is_ok());
//! ```

use std::fmt;

use harw_dod_signals::{SecurityVerdict, validate_verdict};
use harw_types::{ContentDigest, FindingId};
use serde::{Deserialize, Serialize};

/// Aktuelle Fassung des Einreichungsformats.
pub const TRIAGE_SUBMISSION_VERSION: u16 = 1;

/// Fehler beim Bauen oder Prüfen einer [`TriageSubmission`].
#[derive(harw_macros::HarwError)]
pub enum SubmissionError {
    /// `version` ist nicht [`TRIAGE_SUBMISSION_VERSION`].
    #[msg("unsupported triage submission version")]
    UnsupportedVersion(u16),
    /// Einreichung, Urteil und/oder gelesener Record nennen verschiedene
    /// Befunde oder Digests.
    #[msg("triage submission is not bound to the finding record")]
    BindingMismatch,
    /// Das Urteil verletzt seinen Vertrag (falsche `contract`-Kennung oder
    /// leere Pflichtfelder).
    #[msg("triage verdict is not well-formed")]
    InvalidVerdict,
}

/// Formatiert `SubmissionError` über seine `Display`-Meldung.
impl fmt::Debug for SubmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// Eine Triage-Einreichung (Urteil + gebundener Finding-Digest).
///
/// # Description
/// Felder privat; lesen über Zugriffsmethoden. Deserialisierte Werte sind
/// **ungeprüft**, bis [`Self::validate`] bzw. [`Self::binds`] `Ok` liefert.
///
/// # Wire-Format
/// `deny_unknown_fields`; siehe Moduldoku.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TriageSubmission {
    version: u16,
    finding: FindingId,
    finding_digest: ContentDigest,
    verdict: SecurityVerdict,
}

impl TriageSubmission {
    /// Builds a submission and checks that `verdict` is bound to `finding`/`finding_digest`.
    ///
    /// # Arguments
    /// - `finding` (`FindingId`): Kennung des gelesenen Records.
    /// - `finding_digest` (`ContentDigest`): Digest des gelesenen Records.
    /// - `verdict` (`SecurityVerdict`): das bereits geparste Urteil.
    ///
    /// # Errors
    /// Siehe [`Self::validate`].
    pub fn new(
        finding: FindingId,
        finding_digest: ContentDigest,
        verdict: SecurityVerdict,
    ) -> Result<Self, SubmissionError> {
        let submission = Self {
            version: TRIAGE_SUBMISSION_VERSION,
            finding,
            finding_digest,
            verdict,
        };
        submission.validate()?;
        Ok(submission)
    }

    /// Checks version, verdict contract and internal binding (without the spool record).
    ///
    /// # Errors
    /// - [`SubmissionError::UnsupportedVersion`]: falsche Fassung.
    /// - [`SubmissionError::InvalidVerdict`]: `contract` ≠
    ///   [`SecurityVerdict::CONTRACT_ID`] oder leere Pflichtfelder.
    /// - [`SubmissionError::BindingMismatch`]: Urteil nennt anderen Befund/Digest.
    pub fn validate(&self) -> Result<(), SubmissionError> {
        if self.version != TRIAGE_SUBMISSION_VERSION {
            return Err(SubmissionError::UnsupportedVersion(self.version));
        }
        if self.verdict.contract() != SecurityVerdict::CONTRACT_ID {
            return Err(SubmissionError::InvalidVerdict);
        }
        validate_verdict(&self.verdict).map_err(|_| SubmissionError::InvalidVerdict)?;
        self.verdict
            .binds(&self.finding, self.finding_digest)
            .map_err(|_| SubmissionError::BindingMismatch)
    }

    /// Checks [`Self::validate`] and that the submission names exactly the record the escalator read.
    ///
    /// # Arguments
    /// - `record_finding` (`&FindingId`): Kennung des vom Escalator selbst gelesenen Records.
    /// - `record_digest` (`ContentDigest`): vom Escalator selbst berechneter Digest.
    ///
    /// # Errors
    /// Alles aus [`Self::validate`]; [`SubmissionError::BindingMismatch`], wenn
    /// Kennung oder Digest vom Record abweichen.
    pub fn binds(
        &self,
        record_finding: &FindingId,
        record_digest: ContentDigest,
    ) -> Result<(), SubmissionError> {
        self.validate()?;
        if &self.finding != record_finding || self.finding_digest != record_digest {
            return Err(SubmissionError::BindingMismatch);
        }
        Ok(())
    }

    /// Returns the submission format version.
    #[must_use]
    pub fn version(&self) -> u16 {
        self.version
    }

    /// Returns the referenced finding id.
    #[must_use]
    pub fn finding(&self) -> &FindingId {
        &self.finding
    }

    /// Returns the bound finding-record digest.
    #[must_use]
    pub fn finding_digest(&self) -> ContentDigest {
        self.finding_digest
    }

    /// Returns the verdict.
    #[must_use]
    pub fn verdict(&self) -> &SecurityVerdict {
        &self.verdict
    }
}

#[cfg(test)]
mod tests {
    use super::{SubmissionError, TRIAGE_SUBMISSION_VERSION, TriageSubmission};
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_dod_signals::{SecurityVerdict, Severity, VerdictClassification};
    use harw_types::{ContentDigest, FindingId};

    fn finding(id: &str) -> TestResult<FindingId> {
        FindingId::try_from_str(id).map_err(ctx("non-empty id"))
    }

    fn verdict_for(
        id: &str,
        digest: ContentDigest,
        rationale: &str,
    ) -> TestResult<SecurityVerdict> {
        Ok(SecurityVerdict::new(
            finding(id)?,
            digest,
            VerdictClassification::Confirmed,
            Severity::High,
            rationale.to_owned(),
            None,
            "security-triage".to_owned(),
            jiff::Timestamp::UNIX_EPOCH,
        ))
    }

    #[test]
    fn test_new_accepts_bound_verdict_and_sets_version() -> TestResult {
        let digest = ContentDigest::of(b"record");
        let submission =
            TriageSubmission::new(finding("f-1")?, digest, verdict_for("f-1", digest, "ok")?)
                .map_err(ctx("submission builds"))?;
        assert_eq!(submission.version(), TRIAGE_SUBMISSION_VERSION);
        assert_eq!(submission.finding(), &finding("f-1")?);
        assert_eq!(submission.finding_digest(), digest);
        assert_eq!(submission.verdict().finding(), &finding("f-1")?);
        Ok(())
    }

    #[test]
    fn test_new_rejects_verdict_for_other_finding_or_digest() -> TestResult {
        let digest = ContentDigest::of(b"record");
        let other = ContentDigest::of(b"other");
        assert!(matches!(
            TriageSubmission::new(finding("f-1")?, digest, verdict_for("f-2", digest, "ok")?),
            Err(SubmissionError::BindingMismatch)
        ));
        assert!(matches!(
            TriageSubmission::new(finding("f-1")?, digest, verdict_for("f-1", other, "ok")?),
            Err(SubmissionError::BindingMismatch)
        ));
        Ok(())
    }

    #[test]
    fn test_validate_rejects_empty_rationale() -> TestResult {
        let digest = ContentDigest::of(b"record");
        assert!(matches!(
            TriageSubmission::new(finding("f-1")?, digest, verdict_for("f-1", digest, "  ")?),
            Err(SubmissionError::InvalidVerdict)
        ));
        Ok(())
    }

    #[test]
    fn test_binds_requires_spool_record_identity_and_digest() -> TestResult {
        let digest = ContentDigest::of(b"record");
        let submission =
            TriageSubmission::new(finding("f-1")?, digest, verdict_for("f-1", digest, "ok")?)
                .map_err(ctx("submission builds"))?;
        assert!(submission.binds(&finding("f-1")?, digest).is_ok());
        assert!(matches!(
            submission.binds(&finding("f-1")?, ContentDigest::of(b"tampered record")),
            Err(SubmissionError::BindingMismatch)
        ));
        assert!(matches!(
            submission.binds(&finding("f-9")?, digest),
            Err(SubmissionError::BindingMismatch)
        ));
        Ok(())
    }

    #[test]
    fn test_serde_roundtrip_and_deserialized_values_are_revalidated() -> TestResult {
        let digest = ContentDigest::of(b"record");
        let submission =
            TriageSubmission::new(finding("f-1")?, digest, verdict_for("f-1", digest, "ok")?)
                .map_err(ctx("submission builds"))?;
        let json = serde_json::to_value(&submission).map_err(ctx("submission serializes"))?;
        let back: TriageSubmission =
            serde_json::from_value(json.clone()).map_err(ctx("submission roundtrips"))?;
        assert_eq!(back, submission);

        let mut wrong_version = json.clone();
        wrong_version["version"] = serde_json::Value::from(2);
        let back: TriageSubmission = serde_json::from_value(wrong_version)
            .map_err(ctx("wrong-version value deserializes"))?;
        assert!(matches!(
            back.validate(),
            Err(SubmissionError::UnsupportedVersion(2))
        ));

        let mut swapped = json.clone();
        swapped["finding"] = serde_json::Value::from("f-2");
        let back: TriageSubmission =
            serde_json::from_value(swapped).map_err(ctx("swapped-finding value deserializes"))?;
        assert!(matches!(
            back.validate(),
            Err(SubmissionError::BindingMismatch)
        ));

        let mut extra = json;
        extra
            .as_object_mut()
            .ok_or(TestError::Missing("json object"))?
            .insert("action".to_owned(), serde_json::Value::from("kill"));
        assert!(serde_json::from_value::<TriageSubmission>(extra).is_err());
        Ok(())
    }

    #[test]
    fn test_submission_error_display_is_content_free() {
        assert_eq!(
            SubmissionError::UnsupportedVersion(7).to_string(),
            "unsupported triage submission version"
        );
        assert_eq!(
            format!("{:?}", SubmissionError::BindingMismatch),
            SubmissionError::BindingMismatch.to_string()
        );
    }
}
