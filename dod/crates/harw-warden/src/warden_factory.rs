//! Zusammenbau des produktiven [`harw_dod_warden::Warden`]: echte
//! cgroup-v2-Ausführung, echtes Audit, ehrlich fehlschlagende
//! Netzisolation.
//!
//! # Warum ein eigenes Modul, nicht direkt in `main.rs`
//! `main::run` soll nur die Startreihenfolge orchestrieren, nicht selbst
//! entscheiden, welche Ausführungs- und Audit-Implementierungen ein
//! produktiver Warden bekommt — dieselbe Trennung, die
//! `harw-sentinel::sensors::build_sensors` für seine Sensor-Registrierung
//! zieht.
//!
//! # Was zusammengebaut wird
//! - `FreezeCgroup`/`ReleaseCgroup`/`KillProcessTree` laufen über je eine
//!   eigene [`harw_dod_warden::CgroupV2Executor`]-Instanz (echte
//!   Dateisystemschreibzugriffe auf `cgroup_root`, siehe dessen
//!   Moduldoku).
//! - `IsolateNetwork` läuft über [`crate::isolation::NftNetworkIsolator`]
//!   (nftables-Tabelle `inet harw_warden`, je cgroup eigene Ein-/Ausgangs-
//!   Chains; `ReleaseCgroup` hebt die Isolation wieder auf). Braucht
//!   `CAP_NET_ADMIN` und `nft` ≥ 0.9.4.
//! - Das Audit-Ziel ist [`crate::audit::TracingAuditSink`] (siehe dessen
//!   Moduldoku für die Begründung, warum dieses Binary es liefern muss).

use std::path::Path;

use harw_dod_warden::{CgroupV2Executor, Warden};

use crate::audit::TracingAuditSink;
use crate::isolation::NftNetworkIsolator;

/// Baut den produktiven Warden für das gegebene cgroup-v2-Wurzelverzeichnis.
///
/// # Description
/// Siehe Moduldoku für die Zuordnung Aktion → Ausführer. Jede
/// `CgroupV2Executor`-Instanz erhält eine eigene Kopie von `cgroup_root`
/// (der Typ implementiert kein `Clone`, drei unabhängige Instanzen sind
/// deshalb nötig — sie teilen sich aber denselben Wurzelpfad und damit
/// dasselbe Dateisystemziel).
///
/// # Arguments
/// - `cgroup_root` (`&Path`): Wurzelverzeichnis der cgroup-v2-Hierarchie
///   (Produktion: `/sys/fs/cgroup`, siehe [`crate::cli::DEFAULT_CGROUP_ROOT`]).
///
/// # Returns
/// Einen einsatzbereiten [`Warden`].
#[must_use]
pub fn build_production_warden(cgroup_root: &Path) -> Warden {
    Warden::new(
        CgroupV2Executor::new(cgroup_root),
        CgroupV2Executor::new(cgroup_root),
        NftNetworkIsolator::from_cgroup_root(cgroup_root),
        CgroupV2Executor::new(cgroup_root),
        TracingAuditSink::new(),
    )
}

#[cfg(test)]
mod tests {
    use super::build_production_warden;
    use crate::test_support::{TestResult, ctx};
    use harw_dod_warden::WardenOutcome;
    use harw_dod_warden_proto::{
        AuthorizationProof, EscalationStage, ProposedAction, WardenAction, WardenActionRequest,
        WardenResponse,
    };
    use harw_types::{ApprovalActor, CgroupId, FindingId};

    #[test]
    fn test_production_warden_denies_inadmissible_action_without_touching_the_filesystem()
    -> TestResult {
        // `/nonexistent-harw-warden-mock-root` wird nie geöffnet: die Aktion
        // ist bereits an der Nachprüfung (KillProcessTree ist ab
        // `RuleTriggered` nicht zulässig) abgelehnt, bevor
        // `Warden::dispatch` je aufgerufen wird — dieser Test öffnet also
        // nie eine echte cgroup (harte Auflage dieses Knotens).
        let warden =
            build_production_warden(std::path::Path::new("/nonexistent-harw-warden-mock-root"));

        let action = WardenAction::KillProcessTree {
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
            ProposedAction::KillProcessTree {
                cgroup: CgroupId::try_from_str("cgroup-1").map_err(ctx("non-empty id"))?,
            },
            proof,
        );

        let outcome = warden.handle(&finding, &request);
        assert!(matches!(outcome, WardenOutcome::Denied(_)));
        Ok(())
    }

    /// Deckt den im Auftrag verlangten Test „eine Anfrage geht unverändert
    /// an `Warden::handle`; die Antwort ist inhaltsfrei" ab — ohne Socket,
    /// direkt gegen den produktiv verdrahteten Warden.
    #[test]
    fn test_request_reaches_warden_handle_unmodified_and_the_wire_response_is_content_free()
    -> TestResult {
        let warden =
            build_production_warden(std::path::Path::new("/nonexistent-harw-warden-mock-root"));

        let action = WardenAction::KillProcessTree {
            cgroup: CgroupId::try_from_str("very-identifiable-cgroup-name")
                .map_err(ctx("non-empty id"))?,
        };
        let finding = FindingId::try_from_str("very-identifiable-finding-name")
            .map_err(ctx("non-empty id"))?;
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
            ProposedAction::KillProcessTree {
                cgroup: CgroupId::try_from_str("very-identifiable-cgroup-name")
                    .map_err(ctx("non-empty id"))?,
            },
            proof,
        );

        // Dieselbe `request`, unverändert an `Warden::handle` weitergereicht
        // — kein eigener Vorabentscheid dieses Binaries (siehe
        // `harw_dod_warden::warden`-Moduldoku, „Audit vor jedem Fehlerpfad":
        // die Bibliothek setzt das durch, dieses Binary darf es nicht
        // umgehen).
        let outcome = warden.handle(&finding, &request);
        let response = outcome
            .to_wire()
            .map_err(ctx("Denied maps to WardenResponse"))?;
        let json = serde_json::to_string(&response).map_err(ctx("serializes"))?;

        assert!(!json.contains("very-identifiable"));
        assert!(matches!(response, WardenResponse::Denied { .. }));
        Ok(())
    }
}
