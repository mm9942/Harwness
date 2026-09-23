//! Der Durchsetzer selbst: [`Warden`], [`WardenOutcome`].
//!
//! # Verantwortungsbereich
//! [`Warden::handle`] ist die einzige Stelle dieser Crate, die eine
//! [`harw_dod_warden_proto::WardenActionRequest`] tatsächlich verarbeitet:
//! Beleg nachprüfen, Audit-Eintrag vor jedem Ausführungsversuch schreiben,
//! bei Zulässigkeit ausführen, Ergebnis aufzeichnen.
//!
//! # Die beiden Nachprüfungen — und warum sie nicht tautologisch sind
//! [`harw_dod_warden_proto::AuthorizationProof::verify`] übernimmt beide vom
//! Brief geforderten Prüfungen in einem Aufruf:
//!
//! 1. **Bindung**: `verify` bildet den Inhaltsdigest der **übergebenen**
//!    Aktion (`action.content_digest()`, aus `request.action`, also aus der
//!    tatsächlich **empfangenen** Wire-Nachricht) und vergleicht ihn mit
//!    `proof.bound_action()` (aus demselben Beleg). Der Vergleichswert für
//!    die Aktionsseite kommt damit nachweislich aus der empfangenen Aktion,
//!    nicht aus dem Beleg selbst — siehe
//!    `test_mismatched_proof_and_action_is_denied_via_public_path` unten,
//!    der über [`Warden::handle`] (den einzigen öffentlichen Weg) einen
//!    Beleg schickt, der auf eine andere Aktion gebunden ist, als tatsächlich
//!    in der Anfrage steht.
//! 2. **Zulässigkeit**: `verify` befragt
//!    `harw_dod_warden_proto::WardenAction::is_admissible_from` — die
//!    bereits vorhandene Matrix aus `harw-dod-warden-proto`, hier nicht
//!    nachgebaut.
//!
//! **Die K43-Falle, die dieser Knoten vermeidet:** Der `finding`-Wert, gegen
//! den [`AuthorizationProof::verify`] die Bindung prüft, kommt bei
//! [`Warden::handle`] **nicht** aus `request.proof.finding()` — das wäre
//! tautologisch (derselbe Wert gegen sich selbst verglichen, immer wahr).
//! `harw_dod_warden_proto::WardenActionRequest` trägt bewusst kein eigenes
//! `finding`-Feld (das ist Sache der Transportschicht/Session, nicht dieser
//! Wire-Nachricht — siehe `harw-dod-warden-proto`). [`Warden::handle`]
//! verlangt deshalb `finding` als eigenen Parameter, den der Aufrufer
//! (künftig: das Warden-Binary AW5-04b, das weiß, für welchen Befund die
//! Eskalationssitzung lief) unabhängig von der Anfrage mitbringt.
//!
//! # Was zusätzlich geprüft wird — und was bewusst nicht
//! - **Geprüft**: cgroup-Kennungen werden vor jedem Dateisystemzugriff auf
//!   Pfad-Traversal-Zeichen geprüft (`executor.rs`,
//!   `validate_path_segment`) — der Beleg autorisiert eine *Aktion*, nicht
//!   automatisch einen sicheren Dateisystempfad.
//! - **Bewusst nicht geprüft**: `authorized_at` gegen eine Systemuhr
//!   (Ablauf/Frische des Belegs). Diese Crate liest keine Systemuhr (Brief),
//!   und eine Ablauffrist wäre eine neue, hier nicht mandatierte
//!   Geschäftsregel — sie gehört, wenn gewünscht, zu
//!   `harw_dod_escalate::authorize` (die den Zeitpunkt bereits injiziert
//!   bekommt) oder zu einem künftigen Knoten, nicht in die reine
//!   Nachprüfung hier.
//! - **Bewusst nicht geprüft**: die Identität von `authorized_by` gegen eine
//!   Zugriffskontrollliste. Wer autorisieren durfte, ist eine Entscheidung
//!   von `harw_dod_escalate::authorize` — der Zeitpunkt, an dem
//!   `AuthorizationProof::new` aufgerufen wird (siehe
//!   `harw-dod-warden-proto`, `proof.rs`-Moduldoku, Abschnitt „Wer diesen
//!   Typ konstruieren darf"). Der Warden vertraut nicht der Aussage
//!   `authorized_by`, sondern der *Vertrauensgrenze*, über die der Beleg
//!   überhaupt eintraf (`SOCK_SEQPACKET` mit `SO_PEERCRED`, AW5-04b) — ein
//!   zweiter Identitätscheck hier wäre eine Prüfung gegen denselben Wert,
//!   den man prüfen wollte, sobald keine unabhängige Quelle für „wer darf"
//!   existiert.
//!
//! # Audit vor jedem Fehlerpfad
//! Siehe `audit.rs`-Moduldoku für die vollständige Reihenfolge. Kurz: ein
//! [`crate::audit::AuditEvent::Attempting`]-Eintrag entsteht, **bevor**
//! [`Warden::dispatch`] aufgerufen wird — nicht erst nach einem Erfolg.

use harw_dod_warden_proto::{
    Denial, WardenAction, WardenActionAudit, WardenActionRequest, WardenResponse,
};
use harw_types::FindingId;

use crate::audit::{AuditEvent, AuditSink};
use crate::executor::{CgroupFreezer, CgroupReleaser, NetworkIsolator, ProcessTreeKiller};

/// Das Ergebnis einer verarbeiteten [`WardenActionRequest`].
///
/// # Description
/// Ein eigener, in-process-Typ dieser Crate — **kein** Ersatz für
/// [`WardenResponse`]. Grund: [`WardenResponse`] (aus
/// `harw-dod-warden-proto`, nicht Teil dieses Schreibbereichs) kennt nur
/// „ausgeführt" und „abgelehnt"; ein Ausführungsfehler *nach* erfolgreicher
/// Nachprüfung (z. B. ein fehlgeschlagener Dateisystemzugriff) ist weder das
/// eine noch das andere im Sinne der bestehenden
/// [`harw_dod_warden_proto::Denial`]-Kategorien. [`Self::to_wire`] bildet
/// die beiden Fälle ab, die sich verlustfrei auf [`WardenResponse`]
/// übertragen lassen; [`Self::ExecutionFailed`] und
/// [`Self::VerificationFailed`] bleiben ohne Wire-Entsprechung — das ist
/// eine dokumentierte Lücke, keine übersehene (siehe `to_wire`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WardenOutcome {
    /// Die Aktion wurde erfolgreich ausgeführt.
    Executed(WardenActionAudit),
    /// Die Nachprüfung (Bindung oder Zulässigkeit) hat die Anfrage
    /// abgelehnt.
    Denied(Denial),
    /// Die Nachprüfung war erfolgreich, aber die Ausführung selbst ist
    /// fehlgeschlagen.
    ExecutionFailed,
    /// Die Nachprüfung selbst konnte nicht durchgeführt werden (interne
    /// Kodierungspanne, kein geschäftslogischer Ablehnungsgrund).
    VerificationFailed,
}

impl WardenOutcome {
    /// Bildet dieses Ergebnis, wo möglich, auf die Wire-Antwort ab.
    ///
    /// # Description
    /// Siehe Typ-Dokumentation für die Begründung, warum
    /// [`Self::ExecutionFailed`] und [`Self::VerificationFailed`] `None`
    /// liefern.
    ///
    /// # Returns
    /// `Some(WardenResponse)` für [`Self::Executed`]/[`Self::Denied`],
    /// sonst `None`.
    #[must_use]
    pub fn to_wire(&self) -> Option<WardenResponse> {
        match self {
            Self::Executed(audit) => Some(WardenResponse::Executed {
                audit: audit.clone(),
            }),
            Self::Denied(reason) => Some(WardenResponse::Denied { reason: *reason }),
            Self::ExecutionFailed | Self::VerificationFailed => None,
        }
    }
}

/// Der Durchsetzer: prüft jeden Beleg nach, führt zulässige Aktionen über
/// ihre Ausführungs-Traits aus, protokolliert jeden Pfad.
///
/// # Description
/// Hält für jede der vier Aktionen genau eine Ausführungsimplementierung
/// (siehe `executor.rs`) sowie ein Audit-Ziel (siehe `audit.rs`) — jeweils
/// als Trait-Objekt, weil dies eine austauschbare, plug-in-artige
/// Konfiguration ist (Produktions- vs. Testimplementierung), kein
/// Leistungspfad.
pub struct Warden {
    freezer: Box<dyn CgroupFreezer + Send + Sync>,
    releaser: Box<dyn CgroupReleaser + Send + Sync>,
    isolator: Box<dyn NetworkIsolator + Send + Sync>,
    killer: Box<dyn ProcessTreeKiller + Send + Sync>,
    audit: Box<dyn AuditSink + Send + Sync>,
}

impl Warden {
    /// Baut einen Warden aus den vier Ausführungsimplementierungen und einem
    /// Audit-Ziel.
    ///
    /// # Arguments
    /// - `freezer` (`impl CgroupFreezer + Send + Sync + 'static`): Ausführer
    ///   für [`WardenAction::FreezeCgroup`].
    /// - `releaser` (`impl CgroupReleaser + Send + Sync + 'static`):
    ///   Ausführer für [`WardenAction::ReleaseCgroup`].
    /// - `isolator` (`impl NetworkIsolator + Send + Sync + 'static`):
    ///   Ausführer für [`WardenAction::IsolateNetwork`].
    /// - `killer` (`impl ProcessTreeKiller + Send + Sync + 'static`):
    ///   Ausführer für [`WardenAction::KillProcessTree`].
    /// - `audit` (`impl AuditSink + Send + Sync + 'static`): Ziel für
    ///   Audit-Einträge.
    ///
    /// # Returns
    /// Einen neuen [`Warden`].
    #[must_use]
    pub fn new(
        freezer: impl CgroupFreezer + Send + Sync + 'static,
        releaser: impl CgroupReleaser + Send + Sync + 'static,
        isolator: impl NetworkIsolator + Send + Sync + 'static,
        killer: impl ProcessTreeKiller + Send + Sync + 'static,
        audit: impl AuditSink + Send + Sync + 'static,
    ) -> Self {
        Self {
            freezer: Box::new(freezer),
            releaser: Box::new(releaser),
            isolator: Box::new(isolator),
            killer: Box::new(killer),
            audit: Box::new(audit),
        }
    }

    /// Verarbeitet eine Anfrage vollständig: nachprüfen, protokollieren,
    /// gegebenenfalls ausführen.
    ///
    /// # Description
    /// Siehe Moduldoku für die Begründung jeder Prüfung und die
    /// Audit-Reihenfolge. `finding` kommt bewusst **nicht** aus
    /// `request.proof` — siehe Moduldoku, Abschnitt „Die K43-Falle".
    ///
    /// # Arguments
    /// - `finding` (`&FindingId`): der Befund, für den diese Anfrage laut
    ///   der aufrufenden Sitzung gilt — unabhängig von `request` ermittelt.
    /// - `request` (`&WardenActionRequest`): die empfangene Wire-Nachricht.
    ///
    /// # Returns
    /// Das Ergebnis der Verarbeitung.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_warden::audit::RecordingAuditSink;
    /// use harw_dod_warden::executor::RecordingExecutor;
    /// use harw_dod_warden::warden::{Warden, WardenOutcome};
    /// use harw_dod_warden_proto::{
    ///     AuthorizationProof, EscalationStage, ProposedAction, WardenAction, WardenActionRequest,
    /// };
    /// use harw_types::{ApprovalActor, CgroupId, FindingId};
    ///
    /// let proposed = ProposedAction::FreezeCgroup {
    ///     cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
    /// };
    /// let action: WardenAction = proposed.clone().into();
    /// let finding = FindingId::try_from_str("finding-1").unwrap();
    /// let proof = AuthorizationProof::new(
    ///     finding.clone(),
    ///     EscalationStage::RuleTriggered,
    ///     ApprovalActor::Operator { id: "operator-1".to_string() },
    ///     jiff::Timestamp::UNIX_EPOCH,
    ///     action.content_digest().unwrap(),
    /// );
    /// let request = WardenActionRequest::new(proposed, proof);
    ///
    /// let warden = Warden::new(
    ///     RecordingExecutor::new(),
    ///     RecordingExecutor::new(),
    ///     RecordingExecutor::new(),
    ///     RecordingExecutor::new(),
    ///     RecordingAuditSink::new(),
    /// );
    /// let outcome = warden.handle(&finding, &request);
    /// assert!(matches!(outcome, WardenOutcome::Executed(_)));
    /// ```
    pub fn handle(&self, finding: &FindingId, request: &WardenActionRequest) -> WardenOutcome {
        match request.proof.verify(finding, &request.action) {
            Ok(()) => self.execute(request.action.clone()),
            Err(err) => match err.as_denial() {
                Some(reason) => {
                    self.audit.record(AuditEvent::Denied {
                        action: request.action.clone(),
                        reason,
                    });
                    WardenOutcome::Denied(reason)
                }
                None => {
                    self.audit.record(AuditEvent::VerificationFailed {
                        action: request.action.clone(),
                    });
                    WardenOutcome::VerificationFailed
                }
            },
        }
    }

    /// Führt eine bereits nachgeprüfte, zulässige Aktion aus.
    ///
    /// # Description
    /// Schreibt [`AuditEvent::Attempting`] **vor** dem Ausführungsversuch
    /// (siehe `audit.rs`-Moduldoku) und anschließend
    /// [`AuditEvent::Executed`] oder [`AuditEvent::ExecutionFailed`], je
    /// nach Ergebnis von [`Self::dispatch`].
    fn execute(&self, action: WardenAction) -> WardenOutcome {
        self.audit.record(AuditEvent::Attempting {
            action: action.clone(),
        });
        match self.dispatch(&action) {
            Ok(()) => {
                let audit = WardenActionAudit::for_action(action);
                self.audit.record(AuditEvent::Executed {
                    audit: audit.clone(),
                });
                WardenOutcome::Executed(audit)
            }
            Err(_execution_error) => {
                self.audit.record(AuditEvent::ExecutionFailed { action });
                WardenOutcome::ExecutionFailed
            }
        }
    }

    /// Ruft die zur Aktion passende Ausführungs-Trait-Methode auf.
    fn dispatch(&self, action: &WardenAction) -> crate::error::WardenResult<()> {
        match action {
            WardenAction::FreezeCgroup { cgroup } => self.freezer.freeze(cgroup),
            WardenAction::ReleaseCgroup { cgroup } => self.releaser.release(cgroup),
            WardenAction::IsolateNetwork { cgroup } => self.isolator.isolate(cgroup),
            WardenAction::KillProcessTree { cgroup } => self.killer.kill(cgroup),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Warden, WardenOutcome};
    use crate::audit::{AuditEvent, RecordingAuditSink};
    use crate::executor::RecordingExecutor;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_dod_warden_proto::{
        AuthorizationProof, Denial, EscalationStage, ProposedAction, WardenAction,
        WardenActionRequest, WardenResponse,
    };
    use harw_types::{ApprovalActor, CgroupId, FindingId};

    fn cgroup(id: &str) -> TestResult<CgroupId> {
        CgroupId::try_from_str(id).map_err(ctx("non-empty id"))
    }

    fn finding(id: &str) -> TestResult<FindingId> {
        FindingId::try_from_str(id).map_err(ctx("non-empty id"))
    }

    fn actor() -> ApprovalActor {
        ApprovalActor::Operator {
            id: "operator-1".to_string(),
        }
    }

    fn proof_for(
        finding_id: FindingId,
        action: &WardenAction,
        stage: EscalationStage,
    ) -> TestResult<AuthorizationProof> {
        Ok(AuthorizationProof::new(
            finding_id,
            stage,
            actor(),
            jiff::Timestamp::UNIX_EPOCH,
            action.content_digest().map_err(ctx("action encodes"))?,
        ))
    }

    // -- Der wichtigste Test: Beleg, der an eine andere Aktion gebunden ist ---

    #[test]
    fn test_mismatched_proof_and_action_is_denied_via_public_path() -> TestResult {
        // Der Beleg wird an FreezeCgroup(cgroup-1) gebunden ausgestellt ...
        let bound_action = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let f = finding("finding-1")?;
        let proof = proof_for(f.clone(), &bound_action, EscalationStage::Escalated)?;

        // ... aber über den öffentlichen Weg (WardenActionRequest::new) wird
        // eine ANDERE Aktion mit diesem Beleg verschickt. Kein handgebauter
        // interner Zustand: der Beleg und die Aktion durchlaufen exakt den
        // Weg, den ein realer Absender nutzen würde.
        let different_action = ProposedAction::FreezeCgroup {
            cgroup: cgroup("cgroup-2")?,
        };
        let request = WardenActionRequest::new(different_action, proof);

        let warden = Warden::new(
            RecordingExecutor::new(),
            RecordingExecutor::new(),
            RecordingExecutor::new(),
            RecordingExecutor::new(),
            RecordingAuditSink::new(),
        );

        let outcome = warden.handle(&f, &request);
        assert_eq!(outcome, WardenOutcome::Denied(Denial::ProofMismatch));
        Ok(())
    }

    #[test]
    fn test_proof_bound_to_different_finding_is_denied() -> TestResult {
        let action = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let proof = proof_for(finding("finding-1")?, &action, EscalationStage::Escalated)?;
        let request = WardenActionRequest::new(
            ProposedAction::FreezeCgroup {
                cgroup: cgroup("cgroup-1")?,
            },
            proof,
        );

        let warden = Warden::new(
            RecordingExecutor::new(),
            RecordingExecutor::new(),
            RecordingExecutor::new(),
            RecordingExecutor::new(),
            RecordingAuditSink::new(),
        );

        // `finding` kommt hier unabhängig vom Beleg herein (Session-Kontext,
        // der laut Test einen ANDEREN Befund erwartet) - siehe Moduldoku,
        // Abschnitt „Die K43-Falle".
        let other_finding = finding("finding-2")?;
        let outcome = warden.handle(&other_finding, &request);
        assert_eq!(outcome, WardenOutcome::Denied(Denial::ProofMismatch));
        Ok(())
    }

    // -- Beleg für eine unzulässige Stufe --------------------------------------

    #[test]
    fn test_proof_for_inadmissible_stage_is_denied() -> TestResult {
        let action = WardenAction::KillProcessTree {
            cgroup: cgroup("cgroup-1")?,
        };
        let f = finding("finding-1")?;
        // RuleTriggered erlaubt kein KillProcessTree (siehe
        // `harw_dod_warden_proto::action`).
        let proof = proof_for(f.clone(), &action, EscalationStage::RuleTriggered)?;
        let request = WardenActionRequest::new(
            ProposedAction::KillProcessTree {
                cgroup: cgroup("cgroup-1")?,
            },
            proof,
        );

        let warden = Warden::new(
            RecordingExecutor::new(),
            RecordingExecutor::new(),
            RecordingExecutor::new(),
            RecordingExecutor::new(),
            RecordingAuditSink::new(),
        );

        let outcome = warden.handle(&f, &request);
        assert_eq!(outcome, WardenOutcome::Denied(Denial::NotAdmissibleAtStage));
        Ok(())
    }

    // -- Audit vor jedem Fehlerpfad ---------------------------------------------

    #[test]
    fn test_denied_request_audit_entry_is_observable() -> TestResult {
        use std::sync::Arc;

        struct SharedSink(Arc<RecordingAuditSink>);
        impl crate::audit::AuditSink for SharedSink {
            fn record(&self, event: AuditEvent) {
                self.0.record(event);
            }
        }

        let action = WardenAction::KillProcessTree {
            cgroup: cgroup("cgroup-1")?,
        };
        let f = finding("finding-1")?;
        let proof = proof_for(f.clone(), &action, EscalationStage::RuleTriggered)?;
        let request = WardenActionRequest::new(
            ProposedAction::KillProcessTree {
                cgroup: cgroup("cgroup-1")?,
            },
            proof,
        );

        let sink = Arc::new(RecordingAuditSink::new());
        let warden = Warden::new(
            RecordingExecutor::new(),
            RecordingExecutor::new(),
            RecordingExecutor::new(),
            RecordingExecutor::new(),
            SharedSink(Arc::clone(&sink)),
        );

        let outcome = warden.handle(&f, &request);
        assert_eq!(outcome, WardenOutcome::Denied(Denial::NotAdmissibleAtStage));

        let events = sink.events();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            AuditEvent::Denied {
                reason: Denial::NotAdmissibleAtStage,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn test_failed_execution_leaves_audit_entry() -> TestResult {
        use std::sync::Arc;

        struct SharedSink(Arc<RecordingAuditSink>);
        impl crate::audit::AuditSink for SharedSink {
            fn record(&self, event: AuditEvent) {
                self.0.record(event);
            }
        }

        let action = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let f = finding("finding-1")?;
        let proof = proof_for(f.clone(), &action, EscalationStage::Escalated)?;
        let request = WardenActionRequest::new(
            ProposedAction::FreezeCgroup {
                cgroup: cgroup("cgroup-1")?,
            },
            proof,
        );

        let sink = Arc::new(RecordingAuditSink::new());
        let warden = Warden::new(
            RecordingExecutor::new_failing(),
            RecordingExecutor::new(),
            RecordingExecutor::new(),
            RecordingExecutor::new(),
            SharedSink(Arc::clone(&sink)),
        );

        let outcome = warden.handle(&f, &request);
        assert_eq!(outcome, WardenOutcome::ExecutionFailed);

        let events = sink.events();
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], AuditEvent::Attempting { .. }));
        assert!(matches!(events[1], AuditEvent::ExecutionFailed { .. }));
        Ok(())
    }

    // -- Erfolgreiche Ausführung --------------------------------------------------

    #[test]
    fn test_admissible_and_bound_request_is_executed() -> TestResult {
        let action = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let f = finding("finding-1")?;
        let proof = proof_for(f.clone(), &action, EscalationStage::RuleTriggered)?;
        let request = WardenActionRequest::new(
            ProposedAction::FreezeCgroup {
                cgroup: cgroup("cgroup-1")?,
            },
            proof,
        );

        let warden = Warden::new(
            RecordingExecutor::new(),
            RecordingExecutor::new(),
            RecordingExecutor::new(),
            RecordingExecutor::new(),
            RecordingAuditSink::new(),
        );

        let outcome = warden.handle(&f, &request);
        match outcome {
            WardenOutcome::Executed(audit) => {
                assert_eq!(audit.audit_name, "warden.freeze_cgroup");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected Executed, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    // -- Antwort enthält keinen Inhalt -------------------------------------------

    #[test]
    fn test_denied_wire_response_serialized_text_is_content_free() -> TestResult {
        let action = WardenAction::KillProcessTree {
            cgroup: cgroup("very-identifiable-cgroup-name")?,
        };
        let f = finding("very-identifiable-finding-name")?;
        let proof = proof_for(f.clone(), &action, EscalationStage::RuleTriggered)?;
        let request = WardenActionRequest::new(
            ProposedAction::KillProcessTree {
                cgroup: cgroup("very-identifiable-cgroup-name")?,
            },
            proof,
        );

        let warden = Warden::new(
            RecordingExecutor::new(),
            RecordingExecutor::new(),
            RecordingExecutor::new(),
            RecordingExecutor::new(),
            RecordingAuditSink::new(),
        );

        let outcome = warden.handle(&f, &request);
        let response = outcome
            .to_wire()
            .ok_or(TestError::Missing("Denied maps to WardenResponse"))?;
        let json = serde_json::to_string(&response).map_err(ctx("serializes"))?;

        assert!(!json.contains("very-identifiable"));
        assert!(matches!(response, WardenResponse::Denied { .. }));
        Ok(())
    }

    #[test]
    fn test_execution_failed_has_no_wire_representation() -> TestResult {
        let action = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let f = finding("finding-1")?;
        let proof = proof_for(f.clone(), &action, EscalationStage::RuleTriggered)?;
        let request = WardenActionRequest::new(
            ProposedAction::FreezeCgroup {
                cgroup: cgroup("cgroup-1")?,
            },
            proof,
        );

        let warden = Warden::new(
            RecordingExecutor::new_failing(),
            RecordingExecutor::new(),
            RecordingExecutor::new(),
            RecordingExecutor::new(),
            RecordingAuditSink::new(),
        );

        let outcome = warden.handle(&f, &request);
        assert_eq!(outcome, WardenOutcome::ExecutionFailed);
        assert!(outcome.to_wire().is_none());
        Ok(())
    }

    // -- unbekanntes Feld in der Anfrage -----------------------------------------

    #[test]
    fn test_request_with_unknown_field_is_rejected_at_deserialization() -> TestResult {
        let action = WardenAction::FreezeCgroup {
            cgroup: cgroup("cgroup-1")?,
        };
        let f = finding("finding-1")?;
        let proof = proof_for(f, &action, EscalationStage::RuleTriggered)?;
        let request = WardenActionRequest::new(
            ProposedAction::FreezeCgroup {
                cgroup: cgroup("cgroup-1")?,
            },
            proof,
        );

        let mut value = serde_json::to_value(&request).map_err(ctx("serializes"))?;
        value
            .as_object_mut()
            .ok_or(TestError::Unexpected(
                "serialized request is not a JSON object".to_string(),
            ))?
            .insert("extra".to_string(), serde_json::Value::Bool(true));

        let result: Result<WardenActionRequest, _> = serde_json::from_value(value);
        assert!(result.is_err());
        Ok(())
    }
}
