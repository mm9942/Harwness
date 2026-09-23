//! Der Umschlag dieses Binaries um `harw_dod_warden_proto::WardenActionRequest`:
//! [`WardenRequestEnvelope`].
//!
//! # Warum ein eigener Umschlag nötig ist
//! `harw_dod_warden_proto::WardenActionRequest` trägt bewusst kein eigenes
//! `finding`-Feld — siehe `harw_dod_warden::warden`-Moduldoku, Abschnitt
//! „Die K43-Falle": `Warden::handle` verlangt `finding` als von der Anfrage
//! **unabhängigen** Parameter, den „der Aufrufer (künftig: das
//! Warden-Binary AW5-04b, das weiß, für welchen Befund die
//! Eskalationssitzung lief) unabhängig von der Anfrage mitbringt". Dieses
//! Binary ist genau dieser Aufrufer — es muss `finding` deshalb selbst über
//! die Leitung transportieren, getrennt von, aber neben, der eigentlichen
//! `WardenActionRequest`. Kein bestehender Wire-Typ aus
//! `harw-dod-warden-proto` trägt beides zusammen (das wäre dort
//! tautologisch, siehe zitierte Moduldoku); [`WardenRequestEnvelope`] ist
//! die für diesen Transport nötige, minimal zusätzliche Hülle, die dieser
//! Knoten selbst definiert.
//!
//! # Wire-Format
//! `deny_unknown_fields` (K19, wie jeder Wire-Typ von
//! `harw-dod-warden-proto`) — dieselbe Disziplin fortgeführt für die eine
//! zusätzliche Schicht, die dieser Knoten selbst einführt.
//!
//! # Inhaltsfreiheit gilt hier nicht
//! Diese Nachricht geht in Empfangsrichtung (Eskalationsleiter → Warden),
//! nicht in Antwortrichtung — die „inhaltsfreie Antworten"-Auflage betrifft
//! nur das, was den Warden **verlässt** ([`harw_dod_warden_proto::WardenResponse`]
//! über [`harw_dod_warden::warden::WardenOutcome::to_wire`], siehe
//! `crate::ipc`). Eine Anfrage darf und muss Inhalt tragen, sonst könnte der
//! Warden nichts nachprüfen.

use harw_dod_warden_proto::WardenActionRequest;
use harw_types::FindingId;
use serde::{Deserialize, Serialize};

/// Die tatsächliche Wire-Nachricht, die über den `SOCK_SEQPACKET`-Socket
/// dieses Binaries ankommt: eine `WardenActionRequest` plus der Befund, für
/// den sie laut aufrufender Eskalationssitzung gilt.
///
/// # Wire-Format
/// `deny_unknown_fields` (K19).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WardenRequestEnvelope {
    /// Der Befund, für den die aufrufende Eskalationssitzung diese Anfrage
    /// stellt — unabhängig von `request.proof`, siehe Moduldoku.
    pub finding: FindingId,
    /// Die eigentliche, an `harw-dod-warden-proto` bereits vollständig
    /// definierte Anfrage.
    pub request: WardenActionRequest,
}

#[cfg(test)]
mod tests {
    use super::WardenRequestEnvelope;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_dod_warden_proto::{
        AuthorizationProof, EscalationStage, ProposedAction, WardenAction, WardenActionRequest,
    };
    use harw_types::{ApprovalActor, CgroupId, FindingId};

    fn sample_envelope() -> TestResult<WardenRequestEnvelope> {
        let action = WardenAction::FreezeCgroup {
            cgroup: CgroupId::try_from_str("cgroup-1").map_err(ctx("non-empty id"))?,
        };
        let finding = FindingId::try_from_str("finding-1").map_err(ctx("non-empty id"))?;
        let proof = AuthorizationProof::new(
            finding.clone(),
            EscalationStage::RuleTriggered,
            ApprovalActor::Operator {
                id: "operator-1".to_string(),
            },
            jiff::Timestamp::UNIX_EPOCH,
            action.content_digest().map_err(ctx("action encodes"))?,
        );
        let request = WardenActionRequest::new(
            ProposedAction::FreezeCgroup {
                cgroup: CgroupId::try_from_str("cgroup-1").map_err(ctx("non-empty id"))?,
            },
            proof,
        );
        Ok(WardenRequestEnvelope { finding, request })
    }

    #[test]
    fn test_serde_roundtrip() -> TestResult {
        let envelope = sample_envelope()?;
        let json = serde_json::to_string(&envelope).map_err(ctx("serializes"))?;
        let round_tripped: WardenRequestEnvelope =
            serde_json::from_str(&json).map_err(ctx("deserializes"))?;
        assert_eq!(round_tripped.finding, envelope.finding);
        assert_eq!(round_tripped.request.action, envelope.request.action);
        Ok(())
    }

    #[test]
    fn test_rejects_unknown_field() -> TestResult {
        let envelope = sample_envelope()?;
        let mut value = serde_json::to_value(&envelope).map_err(ctx("serializes"))?;
        value
            .as_object_mut()
            .ok_or(TestError::Unexpected(
                "envelope serializes to an object".to_string(),
            ))?
            .insert("extra".to_string(), serde_json::Value::Bool(true));
        let result: Result<WardenRequestEnvelope, _> = serde_json::from_value(value);
        assert!(result.is_err());
        Ok(())
    }
}
