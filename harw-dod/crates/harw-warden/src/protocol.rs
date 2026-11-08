//! Das Wire-Format dieses Binaries: Anfrage dekodieren, Antwort kodieren
//! (PL-100 WP 3.1, Invariante 3 „v1-Belege werden abgelehnt“).
//!
//! # Anfrage
//! Genau ein `SOCK_SEQPACKET`-Datagramm mit einer v2-Anfrage
//! [`WardenRequest`] (JSON, `deny_unknown_fields`):
//! `{"version":2,"action":{...},"authorization":{...}}`. Der Befund ist Teil
//! der signierten Autorisierung (`finding_id`); der frühere eigene Umschlag
//! `{finding, request}` mit dem fälschbaren v1-Beleg ist entfallen.
//! [`decode_request`] nutzt ausschließlich [`WardenRequest::decode`]: eine
//! v1-Hülle (ohne `version`, mit `request`/`proof`, oder `version == 1`)
//! wird ausdrücklich als `UnsupportedVersion` abgelehnt, bevor irgendein
//! Feld als Autorisierung gelesen wird.
//!
//! # Antwort
//! Genau ein Datagramm mit [`WardenReply`] (`{"version":2,"outcome":...}`),
//! inhaltsfrei begrenzt durch `harw-dod-warden-proto`. Auf **jedem** Pfad
//! entsteht eine Antwort: [`reply_for_denial`] für Ablehnungen vor dem
//! Warden (Peer, Dekodieren, Empfang), `WardenOutcome::to_reply` danach.
//!
//! # Nebenläufigkeit
//! Reine Funktionen.
//!
//! # Fehler
//! [`harw_dod_warden_proto::ProofError`] beim Dekodieren (nur lokal geloggt,
//! nach außen nur seine `Denial`), `serde_json::Error` beim Kodieren.

use harw_dod_warden_proto::{Denial, ProofError, WardenReply, WardenRequest, WardenResponse};

/// Decodes one received datagram as a v2 request (v1 is refused explicitly).
///
/// # Errors
/// Siehe [`WardenRequest::decode`]: `LegacyV1Refused`,
/// `UnsupportedVersion`, `MalformedRequest`.
pub fn decode_request(bytes: &[u8]) -> Result<WardenRequest, ProofError> {
    WardenRequest::decode(bytes)
}

/// Builds the versioned reply for a denial that happened before the warden ran.
#[must_use]
pub fn reply_for_denial(reason: Denial) -> WardenReply {
    WardenReply::new(WardenResponse::Denied { reason })
}

/// Encodes a reply as one JSON datagram.
///
/// # Errors
/// `serde_json::Error` (praktisch unerreichbar: nur feldlose Enums,
/// `harw-types`-Kennungen und Zahlen).
pub fn encode_reply(reply: &WardenReply) -> Result<Vec<u8>, serde_json::Error> {
    serde_json::to_vec(reply)
}

#[cfg(test)]
mod tests {
    use super::{decode_request, encode_reply, reply_for_denial};
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_dod_warden_proto::{
        AuthorizationBody, CgroupKernelId, CgroupTarget, Denial, EscalationStage, KeyId, ProofKey,
        SignedAuthorization, WardenAction, WardenRequest,
    };
    use harw_types::{ApprovalActor, CgroupId, FindingId};
    use serde_json::{Value, json};

    fn sample_request() -> TestResult<WardenRequest> {
        let cgroup = CgroupId::try_from_str("harw.slice/job-1").map_err(ctx("cgroup"))?;
        let now = jiff::Timestamp::from_second(1_800_000_000).map_err(ctx("now"))?;
        let expires_at = jiff::Timestamp::from_second(1_800_000_060).map_err(ctx("expiry"))?;
        let action = WardenAction::FreezeCgroup {
            cgroup: cgroup.clone(),
        };
        let body = AuthorizationBody {
            finding_id: FindingId::try_from_str("finding-1").map_err(ctx("finding"))?,
            approver: ApprovalActor::Operator {
                id: "operator-1".to_owned(),
            },
            action: action.clone(),
            stage: EscalationStage::RuleTriggered,
            target: CgroupTarget::new(
                cgroup,
                CgroupKernelId::new(4242).map_err(ctx("kernel id"))?,
                now,
            ),
            hold_secs: None,
            irreversible_ack: false,
            nonce: [3; 16],
            issued_at: now,
            expires_at,
        };
        let key_id = KeyId::new("k1").map_err(ctx("key id"))?;
        Ok(WardenRequest::new(
            action,
            SignedAuthorization::sign(&ProofKey::from_bytes([1; 32]), &key_id, body),
        ))
    }

    fn denial_of(bytes: &[u8]) -> TestResult<Denial> {
        match decode_request(bytes) {
            Ok(_) => Err(TestError::Unexpected("must not decode".into())),
            Err(error) => Ok(error.as_denial()),
        }
    }

    #[test]
    fn test_v2_request_roundtrips() -> TestResult {
        let request = sample_request()?;
        let bytes = serde_json::to_vec(&request).map_err(ctx("encode"))?;
        assert_eq!(decode_request(&bytes).map_err(ctx("decode"))?, request);
        Ok(())
    }

    #[test]
    fn test_v1_envelopes_are_refused_as_unsupported_version() -> TestResult {
        // Der frühere Umschlag dieses Binaries: `{finding, request}` mit v1-Beleg.
        let legacy_envelope = json!({
            "finding": "finding-1",
            "request": {
                "action": { "kind": "freeze-cgroup", "cgroup": "harw.slice/job-1" },
                "proof": {
                    "finding": "finding-1",
                    "stage": "rule-triggered",
                    "authorized_by": { "kind": "operator", "id": "operator-1" },
                    "authorized_at": "2027-01-15T08:00:00Z",
                    "bound_action": "00"
                }
            }
        });
        let bare_v1 = json!({
            "action": { "kind": "freeze-cgroup", "cgroup": "harw.slice/job-1" },
            "proof": {}
        });
        let mut explicit_v1: Value =
            serde_json::to_value(sample_request()?).map_err(ctx("encode"))?;
        explicit_v1
            .as_object_mut()
            .ok_or(TestError::Missing("request object"))?
            .insert("version".to_owned(), Value::from(1));
        for v1 in [legacy_envelope, bare_v1, explicit_v1] {
            let bytes = serde_json::to_vec(&v1).map_err(ctx("encode v1"))?;
            assert_eq!(denial_of(&bytes)?, Denial::UnsupportedVersion);
        }
        Ok(())
    }

    #[test]
    fn test_unknown_fields_and_garbage_are_malformed() -> TestResult {
        let mut extended = serde_json::to_value(sample_request()?).map_err(ctx("encode"))?;
        extended
            .as_object_mut()
            .ok_or(TestError::Missing("request object"))?
            .insert("extra".to_owned(), Value::Bool(true));
        let bytes = serde_json::to_vec(&extended).map_err(ctx("encode"))?;
        assert_eq!(denial_of(&bytes)?, Denial::Malformed);
        assert_eq!(denial_of(b"not json")?, Denial::Malformed);
        assert_eq!(denial_of(b"{\"version\":3}")?, Denial::UnsupportedVersion);
        Ok(())
    }

    #[test]
    fn test_denial_reply_is_versioned_and_content_free() -> TestResult {
        let bytes = encode_reply(&reply_for_denial(Denial::PeerRejected)).map_err(ctx("encode"))?;
        let value: Value = serde_json::from_slice(&bytes).map_err(ctx("decode"))?;
        assert_eq!(
            value,
            json!({ "version": 2, "outcome": { "outcome": "denied", "reason": "peer-rejected" } })
        );
        Ok(())
    }
}
