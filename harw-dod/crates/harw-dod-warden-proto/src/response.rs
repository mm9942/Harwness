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
//! `WardenActionRequest` bleibt als `#[deprecated]` v1-Altlast nur bestehen,
//! bis `harw-dod-escalate` auf Proof v2 umgestellt ist; der Warden akzeptiert
//! ihn nicht. [`WardenRequest::decode`] ist der einzige Weg, Anfragebytes zu
//! lesen: er erkennt eine v1-Hülle (ohne `version`, mit `request`/`proof`, oder
//! `version == 1`) und lehnt sie ausdrücklich mit
//! [`ProofError::LegacyV1Refused`] ab, bevor ein Feld als Autorisierung
//! gelesen wird (PL-100 WP 3.1, Invariante 3).
//!
//! # Warum `WardenResponse` ein eigener Typ ist, kein `Result<T, E>`
//! `std::result::Result` ist fremd — diese Crate kann ihm kein
//! `#[serde(deny_unknown_fields)]` mitgeben. `WardenResponse` ist die
//! eigens deklarierte, geprüfte Entsprechung.

use serde::{Deserialize, Serialize};

use crate::action::{ProposedAction, WardenAction};
use crate::denial::Denial;
use crate::error::ProofError;
// Nur für die stillgelegte v1-Hülle `WardenActionRequest`.
#[allow(deprecated)]
use crate::proof::AuthorizationProof;
use crate::signed::{
    KeyRing, NonceLedger, ProofPolicy, SignedAuthorization, VerifiedAuthorization,
    WARDEN_PROTOCOL_VERSION,
};

// v1-Hüllen trugen kein `version`-Feld; eine ausdrückliche `1` gilt ebenfalls als v1.
const LEGACY_V1_VERSION: u64 = 1;

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
///     AuthorizationBody, CgroupKernelId, CgroupTarget, EscalationStage, KeyId, ProofKey,
///     SignedAuthorization, WardenAction, WardenRequest,
/// };
/// use harw_types::{ApprovalActor, CgroupId, FindingId};
///
/// let (Ok(cgroup), Ok(finding_id), Ok(kernel_id), Ok(key_id)) = (
///     CgroupId::try_from_str("harw.slice/job-1"),
///     FindingId::try_from_str("finding-1"),
///     CgroupKernelId::new(4242),
///     KeyId::new("k1"),
/// ) else {
///     return;
/// };
/// let now = jiff::Timestamp::UNIX_EPOCH;
/// let Ok(expires_at) = now.checked_add(jiff::SignedDuration::from_secs(60)) else {
///     return;
/// };
/// let action = WardenAction::FreezeCgroup { cgroup: cgroup.clone() };
/// let auth = SignedAuthorization::sign(
///     &ProofKey::from_bytes([3; 32]),
///     &key_id,
///     AuthorizationBody {
///         finding_id,
///         approver: ApprovalActor::Operator { id: "operator-1".to_owned() },
///         action: action.clone(),
///         stage: EscalationStage::RuleTriggered,
///         target: CgroupTarget::new(cgroup, kernel_id, now),
///         hold_secs: None,
///         irreversible_ack: false,
///         nonce: [5; 16],
///         issued_at: now,
///         expires_at,
///     },
/// );
/// let request = WardenRequest::new(action, auth);
/// let Ok(json) = serde_json::to_vec(&request) else {
///     return;
/// };
/// assert_eq!(WardenRequest::peek_version(&json).ok(), Some(2));
/// assert_eq!(WardenRequest::decode(&json).ok(), Some(request));
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

// Liest nur die Merkmale, an denen sich v1- und v2-Hüllen unterscheiden;
// alle übrigen Felder werden ignoriert.
#[derive(Deserialize)]
struct EnvelopeProbe {
    // v2: Pflicht; v1: fehlt.
    #[serde(default)]
    version: Option<u64>,
    // v1 `WardenActionRequest` trug den fälschbaren Beleg unter `proof`.
    #[serde(default)]
    proof: Option<serde::de::IgnoredAny>,
    // v1-Binary-Hülle `{finding, request}` (harw-warden `WardenRequestEnvelope`).
    #[serde(default)]
    request: Option<serde::de::IgnoredAny>,
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

    /// Decodes request bytes as a v2 request and refuses every v1 envelope explicitly.
    ///
    /// # Description
    /// Liest zuerst nur die Unterscheidungsmerkmale (unbekannte Felder werden
    /// dabei toleriert) und entscheidet:
    /// 1. kein `version`, aber `request` oder `proof` vorhanden, oder
    ///    `version == 1`: v1-Auftrag (fälschbarer
    ///    [`crate::proof::AuthorizationProof`], F-001) →
    ///    [`ProofError::LegacyV1Refused`]. Kein Feld wird als Autorisierung
    ///    gelesen.
    /// 2. `version` ≠ [`WARDEN_PROTOCOL_VERSION`] →
    ///    [`ProofError::UnsupportedVersion`] (Werte über `u16::MAX` als
    ///    `u16::MAX`).
    /// 3. `version == 2`: vollständiges Dekodieren mit `deny_unknown_fields`.
    ///
    /// Danach muss der Warden noch [`Self::verify`] aufrufen; `decode` prüft
    /// keine Autorisierung.
    ///
    /// # Arguments
    /// - `json` (`&[u8]`): die empfangenen Bytes (Größe begrenzt der Aufrufer).
    ///
    /// # Errors
    /// - [`ProofError::LegacyV1Refused`]: v1-Hülle (Ablehnung
    ///   [`Denial::UnsupportedVersion`]).
    /// - [`ProofError::UnsupportedVersion`]: andere Version.
    /// - [`ProofError::MalformedRequest`]: kein gültiges JSON, `version` fehlt
    ///   ohne v1-Merkmale, falscher Typ oder ein unbekanntes Feld.
    ///
    /// # Concurrency
    /// Reine Funktion.
    pub fn decode(json: &[u8]) -> Result<Self, ProofError> {
        let probe: EnvelopeProbe =
            serde_json::from_slice(json).map_err(ProofError::MalformedRequest)?;
        match probe.version {
            None if probe.proof.is_some() || probe.request.is_some() => {
                Err(ProofError::LegacyV1Refused)
            }
            None => Err(ProofError::MalformedRequest(
                <serde_json::Error as serde::de::Error>::missing_field("version"),
            )),
            Some(LEGACY_V1_VERSION) => Err(ProofError::LegacyV1Refused),
            Some(version) if version == u64::from(WARDEN_PROTOCOL_VERSION) => {
                serde_json::from_slice(json).map_err(ProofError::MalformedRequest)
            }
            Some(version) => Err(ProofError::UnsupportedVersion(
                u16::try_from(version).unwrap_or(u16::MAX),
            )),
        }
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

/// **v1-Altlast** (vom Warden ausdrücklich abgelehnt, siehe
/// [`WardenRequest::decode`]; Entfernung nach der Umstellung von
/// `harw-dod-escalate`; Nachfolger: [`WardenRequest`]). Die frühere
/// Wire-Nachricht vom Eskalationsleiter zum Durchsetzer: eine Aktion plus
/// fälschbarer Autorisierungsbeleg (F-001).
///
/// # Wire-Format
/// `deny_unknown_fields` (K19).
#[deprecated(
    note = "v1 request with a forgeable proof (F-001), refused by the warden; send a WardenRequest with a SignedAuthorization instead"
)]
#[allow(deprecated)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WardenActionRequest {
    /// Die vorgeschlagene, nun autorisierte Aktion.
    pub action: WardenAction,
    /// Der Beleg, dass `action` autorisiert wurde.
    pub proof: AuthorizationProof,
}

#[allow(deprecated)]
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
    /// #![allow(deprecated)]
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

// Die v1-Tests bauen die stillgelegten Typen absichtlich, um ihre Ablehnung zu prüfen.
#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::{
        WardenActionAudit, WardenActionRequest, WardenReply, WardenRequest, WardenResponse,
    };
    use crate::action::{ProposedAction, WardenAction};
    use crate::denial::Denial;
    use crate::error::ProofError;
    use crate::proof::AuthorizationProof;
    use crate::signed::{
        AuthorizationBody, KeyId, KeyRing, MemoryNonceLedger, ProofKey, ProofPolicy,
        SignedAuthorization,
    };
    use crate::stage::EscalationStage;
    use crate::target::{CgroupKernelId, CgroupTarget};
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_types::{ApprovalActor, CgroupId, FindingId};
    use serde_json::Value;

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
        let audit = WardenActionAudit::for_action(WardenAction::ThawCgroup {
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
        let cgroup = cgroup("harw.slice/job-1")?;
        let action = WardenAction::FreezeCgroup {
            cgroup: cgroup.clone(),
        };
        let issued_at = jiff::Timestamp::from_second(1_800_000_000).map_err(ctx("timestamp"))?;
        let auth = SignedAuthorization::sign(
            &ProofKey::from_bytes([3; 32]),
            &KeyId::new("k1").map_err(ctx("key id k1"))?,
            AuthorizationBody {
                finding_id: FindingId::try_from_str("finding-1").map_err(ctx("finding id"))?,
                approver: ApprovalActor::Operator {
                    id: "operator-1".to_owned(),
                },
                action: action.clone(),
                stage: EscalationStage::RuleTriggered,
                target: CgroupTarget::new(
                    cgroup,
                    CgroupKernelId::new(4242).map_err(ctx("kernel id"))?,
                    jiff::Timestamp::from_second(1_799_990_000).map_err(ctx("timestamp"))?,
                ),
                hold_secs: None,
                irreversible_ack: false,
                nonce: [9; 16],
                issued_at,
                expires_at: jiff::Timestamp::from_second(1_800_000_060)
                    .map_err(ctx("timestamp"))?,
            },
        );
        Ok(WardenRequest::new(action, auth))
    }

    // Erwartet einen Dekodierfehler und gibt ihn zurück.
    fn decode_err(json: &[u8]) -> TestResult<ProofError> {
        match WardenRequest::decode(json) {
            Ok(_) => Err(TestError::Unexpected(
                "decode erfolgreich, Fehler erwartet".into(),
            )),
            Err(err) => Ok(err),
        }
    }

    // -- v2: decode / v1-Ablehnung ---------------------------------------------

    #[test]
    fn test_decode_accepts_v2_request() -> TestResult {
        let request = v2_request()?;
        let json = serde_json::to_vec(&request).map_err(ctx("serializes"))?;
        let decoded = WardenRequest::decode(&json).map_err(ctx("decodes"))?;
        assert_eq!(decoded, request);
        Ok(())
    }

    #[test]
    fn test_decode_refuses_every_v1_envelope_explicitly() -> TestResult {
        let proposed = ProposedAction::FreezeCgroup {
            cgroup: cgroup("harw.slice/job-1")?,
        };
        let action: WardenAction = proposed.clone().into();
        let v1 = WardenActionRequest::new(proposed, sample_proof(&action)?);
        let bare = serde_json::to_value(v1).map_err(ctx("v1 to_value"))?;

        // Hülle des Warden-Binaries: `{finding, request: {action, proof}}`.
        let mut binary = serde_json::Map::new();
        binary.insert("finding".to_owned(), Value::from("finding-1"));
        binary.insert("request".to_owned(), bare.clone());
        let binary = Value::Object(binary);

        // Ausdrückliche Version 1 neben gültigen v2-Feldern.
        let mut versioned = serde_json::to_value(v2_request()?).map_err(ctx("v2 to_value"))?;
        versioned
            .as_object_mut()
            .ok_or(TestError::Missing("json object"))?
            .insert("version".to_owned(), Value::from(1_u16));

        for envelope in [bare, binary, versioned] {
            let json = serde_json::to_vec(&envelope).map_err(ctx("serializes"))?;
            let err = decode_err(&json)?;
            assert!(matches!(err, ProofError::LegacyV1Refused), "{err}");
            assert_eq!(err.as_denial(), Denial::UnsupportedVersion);
        }
        Ok(())
    }

    #[test]
    fn test_decode_rejects_other_versions_and_malformed_bytes() -> TestResult {
        for (version, expected) in [(3_u64, 3_u16), (70_000, u16::MAX)] {
            let mut value = serde_json::to_value(v2_request()?).map_err(ctx("to_value"))?;
            value
                .as_object_mut()
                .ok_or(TestError::Missing("json object"))?
                .insert("version".to_owned(), Value::from(version));
            let json = serde_json::to_vec(&value).map_err(ctx("serializes"))?;
            let err = decode_err(&json)?;
            assert!(matches!(err, ProofError::UnsupportedVersion(v) if v == expected));
        }

        let mut unknown = serde_json::to_value(v2_request()?).map_err(ctx("to_value"))?;
        unknown
            .as_object_mut()
            .ok_or(TestError::Missing("json object"))?
            .insert("finding".to_owned(), Value::from("f-1"));
        let unknown = serde_json::to_vec(&unknown).map_err(ctx("serializes"))?;

        for bytes in [
            b"not json".to_vec(),
            b"{}".to_vec(),
            br#"{"version":"2"}"#.to_vec(),
            unknown,
        ] {
            let err = decode_err(&bytes)?;
            assert!(matches!(err, ProofError::MalformedRequest(_)), "{err}");
            assert_eq!(err.as_denial(), Denial::Malformed);
        }
        Ok(())
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
