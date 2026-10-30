//! Die Wire-Hülle: [`WardenActionRequest`], [`WardenActionAudit`],
//! [`WardenResponse`].
//!
//! # Verantwortungsbereich
//! Drei Typen, die den vollständigen Umlauf einer Aktion über die Leitung
//! tragen: die Anfrage vom Eskalationsleiter zum Durchsetzer
//! ([`WardenActionRequest`]), der Audit-Eintrag für eine tatsächlich
//! ausgeführte Aktion ([`WardenActionAudit`]), und die Antwort vom
//! Durchsetzer zurück ([`WardenResponse`]) — Erfolg mit Audit-Eintrag oder
//! Ablehnung mit [`crate::denial::Denial`]. Alle drei tragen
//! `deny_unknown_fields` (K19): sie nehmen Daten entgegen, die von der
//! jeweils anderen Seite der Leitung kommen.
//!
//! # Proof v2 (C-WPROTO, F-088)
//! [`WardenRequest`] ist die v2-Anfragehülle (ersetzt `WardenActionRequest` und
//! das bisher nur im Binary definierte `WardenRequestEnvelope`): Version,
//! Aktion und [`crate::signed::SignedAuthorization`] — **ohne** separates
//! `finding`-Feld (die v1-Bindung daran war tautologisch). [`WardenReply`]
//! versioniert die Antwort. [`WardenRequest::peek_version`] liest nur die
//! Version, damit der Warden auch auf eine unbekannte Fassung mit
//! [`Denial::UnsupportedVersion`] antworten kann, obwohl die übrigen Felder
//! `deny_unknown_fields` verletzen würden.
//!
//! `WardenActionRequest` bleibt als v1-Altlast nur bis W5 (D-WARDEN/D-ESC)
//! bestehen, damit abhängige Crates bis dahin kompilieren; der Warden darf
//! ihn nicht mehr akzeptieren.
//!
//! # Warum `WardenResponse` ein eigener Typ ist, kein `Result<T, E>`
//! `std::result::Result` ist fremd — diese Crate kann ihm kein
//! `#[serde(deny_unknown_fields)]` mitgeben. `WardenResponse` ist die
//! eigens deklarierte, geprüfte Entsprechung.

use serde::{Deserialize, Serialize};

use crate::action::{ProposedAction, WardenAction};
use crate::denial::Denial;
use crate::error::ProofError;
use crate::proof::AuthorizationProof;
use crate::signed::{
    KeyRing, NonceLedger, ProofPolicy, SignedAuthorization, VerifiedAuthorization,
    WARDEN_PROTOCOL_VERSION,
};

/// v2-Anfrage vom Escalator an den Warden.
///
/// # Description
/// `version` muss [`WARDEN_PROTOCOL_VERSION`] sein; `authorization` bindet
/// `action` (siehe [`SignedAuthorization::verify`]).
///
/// # Wire-Format
/// `deny_unknown_fields`.
///
/// # Examples
/// ```rust
/// use harw_dod_warden_proto::{
///     EscalationStage, KeyId, ProofKey, SignedAuthorization, WardenAction, WardenRequest,
/// };
/// use harw_types::CgroupId;
///
/// let cgroup = CgroupId::try_from_str("harw.slice/job-1").unwrap();
/// let action = WardenAction::FreezeCgroup { cgroup: cgroup.clone() };
/// let auth = SignedAuthorization::sign(
///     &ProofKey::from_bytes([3; 32]), &KeyId::new("k1").unwrap(), &action,
///     EscalationStage::RuleTriggered, &cgroup, [0; 16], jiff::Timestamp::UNIX_EPOCH, 60,
/// );
/// let request = WardenRequest::new(action, auth);
/// let json = serde_json::to_vec(&request).unwrap();
/// assert_eq!(WardenRequest::peek_version(&json).unwrap(), 2);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WardenRequest {
    /// Protokollversion der Hülle.
    pub version: u16,
    /// Die angeforderte Aktion.
    pub action: WardenAction,
    /// Die signierte Autorisierung für `action`.
    pub authorization: SignedAuthorization,
}

// Liest nur `version`, ignoriert alle anderen Felder.
#[derive(Deserialize)]
struct VersionProbe {
    version: u16,
}

impl WardenRequest {
    /// Wraps an action and its signed authorization with the current version.
    #[must_use]
    pub fn new(action: WardenAction, authorization: SignedAuthorization) -> Self {
        Self {
            version: WARDEN_PROTOCOL_VERSION,
            action,
            authorization,
        }
    }

    /// Reads only the `version` field of a JSON request.
    ///
    /// # Description
    /// Toleriert unbekannte Felder, damit auch Anfragen anderer Fassungen
    /// (v3 mit neuen Feldern → Wert) eine definierte Antwort bekommen; eine
    /// v1-Hülle ohne `version`-Feld ergibt einen Fehler.
    ///
    /// # Errors
    /// `serde_json::Error`, wenn die Bytes kein JSON-Objekt mit numerischem
    /// `version` (u16) sind — der Warden antwortet dann [`Denial::Malformed`]
    /// bzw. [`Denial::UnsupportedVersion`] (Entscheidung D-WARDEN).
    pub fn peek_version(json: &[u8]) -> Result<u16, serde_json::Error> {
        serde_json::from_slice::<VersionProbe>(json).map(|probe| probe.version)
    }

    /// Checks the envelope version, then verifies the authorization for `self.action`.
    ///
    /// # Errors
    /// - [`ProofError::UnsupportedVersion`]: `self.version` ≠ [`WARDEN_PROTOCOL_VERSION`].
    /// - alles aus [`SignedAuthorization::verify`].
    pub fn verify(
        &self,
        ring: &KeyRing,
        now: jiff::Timestamp,
        policy: &ProofPolicy,
        ledger: &dyn NonceLedger,
    ) -> Result<VerifiedAuthorization, ProofError> {
        if self.version != WARDEN_PROTOCOL_VERSION {
            return Err(ProofError::UnsupportedVersion(self.version));
        }
        self.authorization
            .verify(ring, &self.action, now, policy, ledger)
    }
}

/// Versionierte v2-Antwort des Wardens (F-088).
///
/// # Wire-Format
/// `deny_unknown_fields`; `outcome` ist die bestehende [`WardenResponse`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WardenReply {
    /// Protokollversion der Antwort.
    pub version: u16,
    /// Ergebnis (ausgeführt oder inhaltsfreie Ablehnung).
    pub outcome: WardenResponse,
}

impl WardenReply {
    /// Wraps an outcome with the current protocol version.
    #[must_use]
    pub fn new(outcome: WardenResponse) -> Self {
        Self {
            version: WARDEN_PROTOCOL_VERSION,
            outcome,
        }
    }
}

/// **v1-Altlast** (nicht mehr akzeptiert, Entfernung nach W5 D-WARDEN/D-ESC;
/// Nachfolger: [`WardenRequest`]). Die frühere Wire-Nachricht vom
/// Eskalationsleiter zum Durchsetzer: eine Aktion plus fälschbarer
/// Autorisierungsbeleg (F-001).
///
/// # Wire-Format
/// `deny_unknown_fields` (K19).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WardenActionRequest {
    /// Die vorgeschlagene, nun autorisierte Aktion.
    pub action: WardenAction,
    /// Der Beleg, dass `action` autorisiert wurde.
    pub proof: AuthorizationProof,
}

impl WardenActionRequest {
    /// Verpackt eine vorgeschlagene Aktion mit ihrem Autorisierungsbeleg zur
    /// tatsächlichen Wire-Nachricht.
    ///
    /// # Description
    /// Reine Zusammensetzung — trifft keine Autorisierungsentscheidung
    /// (siehe `proof.rs`-Moduldoku, Abschnitt „Wer diesen Typ konstruieren
    /// darf": der `proof`-Parameter muss bereits vorliegen).
    ///
    /// # Arguments
    /// - `action` (`ProposedAction`): die vorgeschlagene Aktion.
    /// - `proof` (`AuthorizationProof`): ihr Autorisierungsbeleg.
    ///
    /// # Returns
    /// Die fertige Wire-Nachricht.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_warden_proto::{
    ///     AuthorizationProof, EscalationStage, ProposedAction, WardenAction, WardenActionRequest,
    /// };
    /// use harw_types::{ApprovalActor, CgroupId, FindingId};
    ///
    /// let proposed = ProposedAction::FreezeCgroup {
    ///     cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
    /// };
    /// let action: WardenAction = proposed.clone().into();
    /// let proof = AuthorizationProof::new(
    ///     FindingId::try_from_str("finding-1").unwrap(),
    ///     EscalationStage::RuleTriggered,
    ///     ApprovalActor::Operator { id: "operator-1".to_string() },
    ///     jiff::Timestamp::UNIX_EPOCH,
    ///     action.content_digest().unwrap(),
    /// );
    /// let request = WardenActionRequest::new(proposed, proof);
    /// assert_eq!(request.action, action);
    /// ```
    #[must_use]
    pub fn new(action: ProposedAction, proof: AuthorizationProof) -> Self {
        Self {
            action: action.into(),
            proof,
        }
    }
}

/// Der Audit-Eintrag für eine tatsächlich ausgeführte Aktion.
///
/// # Wire-Format
/// `deny_unknown_fields` (K19).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WardenActionAudit {
    /// Der deklarierte Audit-Name der ausgeführten Aktion (siehe
    /// [`WardenAction::audit_name`]). Ein fester, zur Compile-Zeit gewählter
    /// Wert — kein vom Modell beeinflusster `String` (siehe
    /// `lib.rs`-Moduldoku, Abschnitt „Keine freien Argumente"); er wird hier
    /// nur zur Laufzeit aus der Aktion abgelesen, nicht von außen entgegengenommen.
    pub audit_name: String,
    /// Die ausgeführte Aktion.
    pub action: WardenAction,
}

impl WardenActionAudit {
    /// Baut den Audit-Eintrag für eine bereits ausgeführte Aktion.
    ///
    /// # Arguments
    /// - `action` (`WardenAction`): die ausgeführte Aktion.
    ///
    /// # Returns
    /// Den Audit-Eintrag mit [`WardenAction::audit_name`] als `audit_name`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_warden_proto::{WardenAction, WardenActionAudit};
    /// use harw_types::CgroupId;
    ///
    /// let audit = WardenActionAudit::for_action(WardenAction::FreezeCgroup {
    ///     cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
    /// });
    /// assert_eq!(audit.audit_name, "warden.freeze_cgroup");
    /// ```
    #[must_use]
    pub fn for_action(action: WardenAction) -> Self {
        let audit_name = action.audit_name().to_string();
        Self { audit_name, action }
    }
}

/// Die Wire-Nachricht vom Durchsetzer zurück zum Eskalationsleiter: Erfolg
/// mit Audit-Eintrag, oder inhaltsfreie Ablehnung.
///
/// # Wire-Format
/// Intern getaggt (`"outcome"`), kebab-case, `deny_unknown_fields` (K19).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "kebab-case", deny_unknown_fields)]
pub enum WardenResponse {
    /// Die Aktion wurde ausgeführt.
    Executed {
        /// Der Audit-Eintrag der ausgeführten Aktion.
        audit: WardenActionAudit,
    },
    /// Die Aktion wurde abgelehnt.
    Denied {
        /// Die inhaltsfreie Ablehnungskategorie.
        reason: Denial,
    },
}

#[cfg(test)]
mod tests {
    use super::{
        WardenActionAudit, WardenActionRequest, WardenReply, WardenRequest, WardenResponse,
    };
    use crate::action::{ProposedAction, WardenAction};
    use crate::denial::Denial;
    use crate::error::ProofError;
    use crate::proof::AuthorizationProof;
    use crate::signed::{
        KeyId, KeyRing, MemoryNonceLedger, ProofKey, ProofPolicy, SignedAuthorization,
    };
    use crate::stage::EscalationStage;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_types::{ApprovalActor, CgroupId, FindingId};

    fn cgroup(id: &str) -> TestResult<CgroupId> {
        CgroupId::try_from_str(id).map_err(ctx("non-empty id"))
    }

    fn sample_proof(action: &WardenAction) -> TestResult<AuthorizationProof> {
        Ok(AuthorizationProof::new(
            FindingId::try_from_str("finding-1").map_err(ctx("finding-1 id"))?,
            EscalationStage::RuleTriggered,
            ApprovalActor::Operator {
                id: "operator-1".to_string(),
            },
            jiff::Timestamp::UNIX_EPOCH,
            action.content_digest().map_err(ctx("content digest"))?,
        ))
    }

    // -- WardenActionRequest --------------------------------------------------

    #[test]
    fn test_request_new_converts_proposed_action_and_keeps_proof() -> TestResult {
        let proposed = ProposedAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let action: WardenAction = proposed.clone().into();
        let proof = sample_proof(&action)?;

        let request = WardenActionRequest::new(proposed, proof.clone());
        assert_eq!(request.action, action);
        assert_eq!(request.proof, proof);
        Ok(())
    }

    #[test]
    fn test_request_serde_roundtrip() -> TestResult {
        let proposed = ProposedAction::IsolateNetwork {
            cgroup: cgroup("cgroup-1")?,
        };
        let action: WardenAction = proposed.clone().into();
        let proof = sample_proof(&action)?;
        let request = WardenActionRequest::new(proposed, proof);

        let json = serde_json::to_string(&request).map_err(ctx("serializes"))?;
        let round_tripped: WardenActionRequest =
            serde_json::from_str(&json).map_err(ctx("deserializes"))?;
        assert_eq!(round_tripped.action, request.action);
        assert_eq!(round_tripped.proof, request.proof);
        Ok(())
    }

    #[test]
    fn test_request_rejects_unknown_field() -> TestResult {
        let proposed = ProposedAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let action: WardenAction = proposed.clone().into();
        let proof = sample_proof(&action)?;
        let request = WardenActionRequest::new(proposed, proof);

        let mut value = serde_json::to_value(&request).map_err(ctx("to_value"))?;
        value
            .as_object_mut()
            .ok_or(TestError::Missing("json object"))?
            .insert("extra".to_string(), serde_json::Value::Bool(true));
        let result: Result<WardenActionRequest, _> = serde_json::from_value(value);
        assert!(result.is_err());
        Ok(())
    }

    // -- WardenActionAudit ------------------------------------------------------

    #[test]
    fn test_audit_records_declared_name_per_action() -> TestResult {
        let freeze = WardenActionAudit::for_action(WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        });
        assert_eq!(freeze.audit_name, "warden.freeze_cgroup");

        let kill = WardenActionAudit::for_action(WardenAction::KillProcessTree {
            cgroup: cgroup("cgroup-1")?,
        });
        assert_eq!(kill.audit_name, "warden.kill_process_tree");
        Ok(())
    }

    #[test]
    fn test_audit_serde_roundtrip() -> TestResult {
        let audit = WardenActionAudit::for_action(WardenAction::ReleaseCgroup {
            cgroup: cgroup("cgroup-1")?,
        });
        let json = serde_json::to_string(&audit).map_err(ctx("serializes"))?;
        let round_tripped: WardenActionAudit =
            serde_json::from_str(&json).map_err(ctx("deserializes"))?;
        assert_eq!(round_tripped, audit);
        Ok(())
    }

    #[test]
    fn test_audit_rejects_unknown_field() {
        let malformed = r#"{"audit_name":"warden.freeze_cgroup","action":{"kind":"freeze-cgroup","cgroup":"cgroup-1"},"extra":1}"#;
        let result: Result<WardenActionAudit, _> = serde_json::from_str(malformed);
        assert!(result.is_err());
    }

    // -- WardenResponse -----------------------------------------------------------

    #[test]
    fn test_response_executed_serde_roundtrip() -> TestResult {
        let audit = WardenActionAudit::for_action(WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        });
        let response = WardenResponse::Executed { audit };
        let json = serde_json::to_string(&response).map_err(ctx("serializes"))?;
        assert!(json.contains("\"outcome\":\"executed\""));
        let round_tripped: WardenResponse =
            serde_json::from_str(&json).map_err(ctx("deserializes"))?;
        match round_tripped {
            WardenResponse::Executed { audit } => {
                assert_eq!(audit.audit_name, "warden.freeze_cgroup");
            }
            WardenResponse::Denied { .. } => {
                return Err(TestError::Unexpected("expected Executed".into()));
            }
        }
        Ok(())
    }

    #[test]
    fn test_response_denied_serde_roundtrip() -> TestResult {
        let response = WardenResponse::Denied {
            reason: Denial::NotAdmissibleAtStage,
        };
        let json = serde_json::to_string(&response).map_err(ctx("serializes"))?;
        assert!(json.contains("\"outcome\":\"denied\""));
        assert!(json.contains("\"not-admissible-at-stage\""));
        let round_tripped: WardenResponse =
            serde_json::from_str(&json).map_err(ctx("deserializes"))?;
        match round_tripped {
            WardenResponse::Denied { reason } => assert_eq!(reason, Denial::NotAdmissibleAtStage),
            WardenResponse::Executed { .. } => {
                return Err(TestError::Unexpected("expected Denied".into()));
            }
        }
        Ok(())
    }

    // -- v2: WardenRequest / WardenReply ---------------------------------------

    fn v2_request() -> TestResult<WardenRequest> {
        let action = WardenAction::FreezeCgroup {
            cgroup: cgroup("harw.slice/job-1")?,
        };
        let auth = SignedAuthorization::sign(
            &ProofKey::from_bytes([3; 32]),
            &KeyId::new("k1").map_err(ctx("key id k1"))?,
            &action,
            EscalationStage::RuleTriggered,
            action.cgroup(),
            [9; 16],
            jiff::Timestamp::from_second(1_800_000_000).map_err(ctx("timestamp"))?,
            60,
        );
        Ok(WardenRequest::new(action, auth))
    }

    fn v2_ring() -> TestResult<KeyRing> {
        let mut ring = KeyRing::new();
        ring.insert(
            KeyId::new("k1").map_err(ctx("key id k1"))?,
            ProofKey::from_bytes([3; 32]),
        )
        .map_err(ctx("insert key"))?;
        Ok(ring)
    }

    fn v2_policy() -> TestResult<ProofPolicy> {
        ProofPolicy::new(60, 5, vec!["harw.slice".to_owned()]).map_err(ctx("proof policy"))
    }

    #[test]
    fn test_warden_request_serde_roundtrip_and_verify() -> TestResult {
        let request = v2_request()?;
        let json = serde_json::to_vec(&request).map_err(ctx("serializes"))?;
        let back: WardenRequest = serde_json::from_slice(&json).map_err(ctx("deserializes"))?;
        assert_eq!(back, request);
        let verified = back
            .verify(
                &v2_ring()?,
                jiff::Timestamp::from_second(1_800_000_010).map_err(ctx("timestamp"))?,
                &v2_policy()?,
                &MemoryNonceLedger::new(),
            )
            .map_err(ctx("verifies"))?;
        assert_eq!(verified.stage(), EscalationStage::RuleTriggered);
        Ok(())
    }

    #[test]
    fn test_warden_request_verify_rejects_envelope_version() -> TestResult {
        let mut request = v2_request()?;
        request.version = 1;
        let result = request.verify(
            &v2_ring()?,
            jiff::Timestamp::from_second(1_800_000_010).map_err(ctx("timestamp"))?,
            &v2_policy()?,
            &MemoryNonceLedger::new(),
        );
        let Err(err) = result else {
            return Err(TestError::Unexpected(
                "verify erfolgreich, Fehler erwartet".into(),
            ));
        };
        assert!(matches!(err, ProofError::UnsupportedVersion(1)));
        Ok(())
    }

    #[test]
    fn test_warden_request_peek_version_tolerates_unknown_fields() -> TestResult {
        assert_eq!(
            WardenRequest::peek_version(br#"{"version":3,"future":true}"#)
                .map_err(ctx("peek_version"))?,
            3
        );
        // v1-Hülle ohne `version`-Feld ist nicht lesbar.
        assert!(WardenRequest::peek_version(br#"{"finding":"f","request":{}}"#).is_err());
        assert!(WardenRequest::peek_version(b"not json").is_err());
        Ok(())
    }

    #[test]
    fn test_warden_request_rejects_unknown_field() -> TestResult {
        let request = v2_request()?;
        let mut value = serde_json::to_value(&request).map_err(ctx("to_value"))?;
        value
            .as_object_mut()
            .ok_or(TestError::Missing("json object"))?
            .insert("finding".to_owned(), serde_json::Value::from("f-1"));
        assert!(serde_json::from_value::<WardenRequest>(value).is_err());
        Ok(())
    }

    #[test]
    fn test_warden_reply_new_sets_version_and_roundtrips() -> TestResult {
        let reply = WardenReply::new(WardenResponse::Denied {
            reason: Denial::UnsupportedVersion,
        });
        assert_eq!(reply.version, 2);
        let json = serde_json::to_string(&reply).map_err(ctx("serializes"))?;
        let back: WardenReply = serde_json::from_str(&json).map_err(ctx("deserializes"))?;
        assert_eq!(back.version, 2);
        assert!(matches!(
            back.outcome,
            WardenResponse::Denied {
                reason: Denial::UnsupportedVersion
            }
        ));
        Ok(())
    }

    #[test]
    fn test_response_rejects_unknown_field() {
        let malformed = r#"{"outcome":"denied","reason":"proof-mismatch","extra":true}"#;
        let result: Result<WardenResponse, _> = serde_json::from_str(malformed);
        assert!(result.is_err());
    }
}
