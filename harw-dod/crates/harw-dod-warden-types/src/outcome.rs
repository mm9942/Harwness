//! Das Ergebnis einer Warden-Aktion: [`WardenActionAudit`], [`WardenResponse`].
//!
//! # Verantwortungsbereich
//! Zwei reine Datentypen, die der Durchsetzer nach einer Anfrage zurückgibt:
//! der Audit-Eintrag für eine tatsächlich ausgeführte Aktion
//! ([`WardenActionAudit`]) und die Antwort selbst ([`WardenResponse`]) —
//! Erfolg mit Audit-Eintrag oder Ablehnung mit [`crate::denial::Denial`].
//! Beide tragen `deny_unknown_fields` (K19): sie nehmen Daten entgegen, die
//! von der jeweils anderen Seite der Leitung kommen.
//!
//! Bis PL-100 WP 3.0 lagen beide in `harw-dod-warden-proto/src/response.rs`;
//! sie enthalten keinen Signier-Code und sind deshalb hierher gewandert. Die
//! versionierte Hülle `WardenReply` und die Anfragehülle `WardenRequest`
//! bleiben im Signierteil, der beide Typen hier unter ihren bisherigen Pfaden
//! re-exportiert (`harw_dod_warden_proto::WardenResponse`,
//! `harw_dod_warden_proto::response::WardenResponse`).
//!
//! # Warum `WardenResponse` ein eigener Typ ist, kein `Result<T, E>`
//! `std::result::Result` ist fremd — diese Crate kann ihm kein
//! `#[serde(deny_unknown_fields)]` mitgeben. `WardenResponse` ist die
//! eigens deklarierte, geprüfte Entsprechung.
//!
//! # Nebenläufigkeit
//! Reine Werttypen, `Send + Sync`.
//!
//! # Examples
//! ```rust
//! use harw_dod_warden_types::{Denial, WardenResponse};
//!
//! let response = WardenResponse::Denied { reason: Denial::PolicyDenied };
//! let Ok(json) = serde_json::to_string(&response) else { return };
//! assert_eq!(json, r#"{"outcome":"denied","reason":"policy-denied"}"#);
//! ```

use serde::{Deserialize, Serialize};

use crate::action::WardenAction;
use crate::denial::Denial;

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
    /// use harw_dod_warden_types::{WardenAction, WardenActionAudit};
    /// use harw_types::CgroupId;
    ///
    /// let Ok(cgroup) = CgroupId::try_from_str("cgroup-1") else { return };
    /// let audit = WardenActionAudit::for_action(WardenAction::FreezeCgroup { cgroup });
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
    use super::{WardenActionAudit, WardenResponse};
    use crate::action::WardenAction;
    use crate::denial::Denial;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_types::CgroupId;

    fn cgroup(id: &str) -> TestResult<CgroupId> {
        CgroupId::try_from_str(id).map_err(ctx("non-empty id"))
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

    #[test]
    fn test_response_rejects_unknown_field() {
        let malformed = r#"{"outcome":"denied","reason":"proof-mismatch","extra":true}"#;
        let result: Result<WardenResponse, _> = serde_json::from_str(malformed);
        assert!(result.is_err());
    }
}
