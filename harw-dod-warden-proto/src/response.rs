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
//! # Warum `WardenResponse` ein eigener Typ ist, kein `Result<T, E>`
//! `std::result::Result` ist fremd — diese Crate kann ihm kein
//! `#[serde(deny_unknown_fields)]` mitgeben. `WardenResponse` ist die
//! eigens deklarierte, geprüfte Entsprechung.

use serde::{Deserialize, Serialize};

use crate::action::{ProposedAction, WardenAction};
use crate::denial::Denial;
use crate::proof::AuthorizationProof;

/// Die tatsächliche Wire-Nachricht vom Eskalationsleiter zum Durchsetzer:
/// eine Aktion plus der Autorisierungsbeleg, ohne den der Durchsetzer sie
/// ablehnt.
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
    use super::{WardenActionAudit, WardenActionRequest, WardenResponse};
    use crate::action::{ProposedAction, WardenAction};
    use crate::denial::Denial;
    use crate::proof::AuthorizationProof;
    use crate::stage::EscalationStage;
    use harw_types::{ApprovalActor, CgroupId, FindingId};

    fn cgroup(id: &str) -> CgroupId {
        CgroupId::try_from_str(id).expect("non-empty id")
    }

    fn sample_proof(action: &WardenAction) -> AuthorizationProof {
        AuthorizationProof::new(
            FindingId::try_from_str("finding-1").unwrap(),
            EscalationStage::RuleTriggered,
            ApprovalActor::Operator {
                id: "operator-1".to_string(),
            },
            jiff::Timestamp::UNIX_EPOCH,
            action.content_digest().unwrap(),
        )
    }

    // -- WardenActionRequest --------------------------------------------------

    #[test]
    fn test_request_new_converts_proposed_action_and_keeps_proof() {
        let proposed = ProposedAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1"),
        };
        let action: WardenAction = proposed.clone().into();
        let proof = sample_proof(&action);

        let request = WardenActionRequest::new(proposed, proof.clone());
        assert_eq!(request.action, action);
        assert_eq!(request.proof, proof);
    }

    #[test]
    fn test_request_serde_roundtrip() {
        let proposed = ProposedAction::IsolateNetwork {
            cgroup: cgroup("cgroup-1"),
        };
        let action: WardenAction = proposed.clone().into();
        let proof = sample_proof(&action);
        let request = WardenActionRequest::new(proposed, proof);

        let json = serde_json::to_string(&request).expect("serializes");
        let round_tripped: WardenActionRequest =
            serde_json::from_str(&json).expect("deserializes");
        assert_eq!(round_tripped.action, request.action);
        assert_eq!(round_tripped.proof, request.proof);
    }

    #[test]
    fn test_request_rejects_unknown_field() {
        let proposed = ProposedAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1"),
        };
        let action: WardenAction = proposed.clone().into();
        let proof = sample_proof(&action);
        let request = WardenActionRequest::new(proposed, proof);

        let mut value = serde_json::to_value(&request).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("extra".to_string(), serde_json::Value::Bool(true));
        let result: Result<WardenActionRequest, _> = serde_json::from_value(value);
        assert!(result.is_err());
    }

    // -- WardenActionAudit ------------------------------------------------------

    #[test]
    fn test_audit_records_declared_name_per_action() {
        let freeze = WardenActionAudit::for_action(WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1"),
        });
        assert_eq!(freeze.audit_name, "warden.freeze_cgroup");

        let kill = WardenActionAudit::for_action(WardenAction::KillProcessTree {
            cgroup: cgroup("cgroup-1"),
        });
        assert_eq!(kill.audit_name, "warden.kill_process_tree");
    }

    #[test]
    fn test_audit_serde_roundtrip() {
        let audit = WardenActionAudit::for_action(WardenAction::ReleaseCgroup {
            cgroup: cgroup("cgroup-1"),
        });
        let json = serde_json::to_string(&audit).expect("serializes");
        let round_tripped: WardenActionAudit = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(round_tripped, audit);
    }

    #[test]
    fn test_audit_rejects_unknown_field() {
        let malformed = r#"{"audit_name":"warden.freeze_cgroup","action":{"kind":"freeze-cgroup","cgroup":"cgroup-1"},"extra":1}"#;
        let result: Result<WardenActionAudit, _> = serde_json::from_str(malformed);
        assert!(result.is_err());
    }

    // -- WardenResponse -----------------------------------------------------------

    #[test]
    fn test_response_executed_serde_roundtrip() {
        let audit = WardenActionAudit::for_action(WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1"),
        });
        let response = WardenResponse::Executed { audit };
        let json = serde_json::to_string(&response).expect("serializes");
        assert!(json.contains("\"outcome\":\"executed\""));
        let round_tripped: WardenResponse = serde_json::from_str(&json).expect("deserializes");
        match round_tripped {
            WardenResponse::Executed { audit } => {
                assert_eq!(audit.audit_name, "warden.freeze_cgroup");
            }
            WardenResponse::Denied { .. } => panic!("expected Executed"),
        }
    }

    #[test]
    fn test_response_denied_serde_roundtrip() {
        let response = WardenResponse::Denied {
            reason: Denial::NotAdmissibleAtStage,
        };
        let json = serde_json::to_string(&response).expect("serializes");
        assert!(json.contains("\"outcome\":\"denied\""));
        assert!(json.contains("\"not-admissible-at-stage\""));
        let round_tripped: WardenResponse = serde_json::from_str(&json).expect("deserializes");
        match round_tripped {
            WardenResponse::Denied { reason } => assert_eq!(reason, Denial::NotAdmissibleAtStage),
            WardenResponse::Executed { .. } => panic!("expected Denied"),
        }
    }

    #[test]
    fn test_response_rejects_unknown_field() {
        let malformed = r#"{"outcome":"denied","reason":"proof-mismatch","extra":true}"#;
        let result: Result<WardenResponse, _> = serde_json::from_str(malformed);
        assert!(result.is_err());
    }
}
