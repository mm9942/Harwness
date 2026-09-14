//! Verdrahtung zwischen der Eskalationsleiter und der [`FreezeStore`]-
//! Persistenz: [`authorize_freeze`], [`authorize_release`],
//! [`authorize_stage_gated`], [`reconcile_expired_freezes`].
//!
//! # Audit vor Fehlerpfad
//! **Was protokolliert werden soll, wird protokolliert, bevor die Aktion
//! versucht wird — nie danach.** [`authorize_freeze`] schreibt den
//! `Freeze`-Datensatz über [`FreezeStore::freeze`] als **ersten** durchbrechenden
//! Schritt, bevor [`crate::ladder::Ladder::stage_for`] oder
//! [`crate::action::Action::authorize`] überhaupt ausgewertet werden.
//! Schlägt einer der beiden nachfolgenden, fehlschlagbaren Schritte fehl —
//! der Befund rechtfertigt keine Stufe, oder die Aktion ist ab der
//! ermittelten Stufe nicht zulässig —, bleibt der bereits geschriebene
//! Freeze-Datensatz stehen: genau der Eintrag zur fehlgeschlagenen
//! Autorisierung, den ein Audit-Trail braucht, um die interessanteste Zeile
//! nicht zu verlieren (Brief, Abschnitt „Die Auflagen, die diese Crate
//! tragen"). [`authorize_release`] folgt derselben Reihenfolge:
//! [`FreezeStore::resolve`] läuft vor der Autorisierungsprüfung.
//! `authorize_stage_gated` (für `IsolateNetwork`/`KillProcessTree`) hat
//! keinen `FreezeStore`-Bezug und schreibt deshalb auch keinen Audit-Eintrag
//! — die Regel ist damit für diese beiden Aktionen leer erfüllt, nicht
//! verletzt.
//!
//! # Rekonziliationsdisziplin beim Start
//! [`reconcile_expired_freezes`] ist die von
//! `harw-session-store/src/freeze.rs`-Moduldoku (Abschnitt „Expiry")
//! angekündigte freie Funktion: sie wird **nicht** automatisch aus
//! [`authorize_freeze`]/[`authorize_release`] aufgerufen — genau wie
//! `ChildLeaseStore::claim_expired` ein separater, vom Aufrufer beim Start
//! auszulösender Schritt ist, nicht ein Hook innerhalb `admit`/`complete`.
//! **Der Aufrufer dieser Crate muss `reconcile_expired_freezes` einmal, vor
//! jeder anderen Freeze-Operation, beim Start aufrufen** — sonst kann ein
//! längst fälliges, zeitgebundenes Einfrieren fälschlich als aktiv
//! erscheinen. Ein Einfrieren ohne `expires_at` läuft dabei nie automatisch
//! ab, auch über einen Neustart hinweg nicht (siehe dortige Moduldoku,
//! Abschnitt „Expiry") — nur ein bewusster [`FreezeStore::resolve`]-Aufruf
//! hebt es auf.

use harw_dod_rules::{Finding, Triaged};
use harw_dod_warden_proto::ProposedAction;
use harw_session_store::{Freeze, FreezeResolution, FreezeStore};
use harw_types::ApprovalActor;
use jiff::Timestamp;

use crate::action::{Action, Authorized, Proposed};
use crate::error::{EscalateError, EscalateResult};
use crate::ladder::Ladder;

/// Führt die beim Start fällige Rekonziliation abgelaufener Freezes aus.
///
/// # Description
/// Dünner Aufruf von [`FreezeStore::reconcile_expired`] — siehe Moduldoku,
/// Abschnitt „Rekonziliationsdisziplin beim Start", für die Pflicht, sie vor
/// jeder anderen Freeze-Operation genau einmal aufzurufen.
///
/// # Arguments
/// - `store` (`&FreezeStore`): der zu rekonziliierende Store.
/// - `now` (`jiff::Timestamp`): der Zeitpunkt, gegen den rekonziliiert wird —
///   injiziert, nie von dieser Funktion selbst gelesen.
///
/// # Returns
/// Die Freezes, die durch diesen Aufruf nach `.resolved.json` befördert
/// wurden (leer, wenn keines fällig war).
///
/// # Errors
/// - [`EscalateError::Store`]: der zugrunde liegende `FreezeStore`-Zugriff
///   ist fehlgeschlagen.
///
/// # Examples
/// ```rust
/// use harw_dod_escalate::reconcile_expired_freezes;
/// use harw_session_store::FreezeStore;
/// use jiff::Timestamp;
///
/// let temp = tempfile::tempdir().unwrap();
/// let store = FreezeStore::new(temp.path());
/// let expired = reconcile_expired_freezes(&store, Timestamp::now()).unwrap();
/// assert!(expired.is_empty());
/// ```
pub fn reconcile_expired_freezes(store: &FreezeStore, now: Timestamp) -> EscalateResult<Vec<Freeze>> {
    Ok(store.reconcile_expired(now)?)
}

/// Autorisiert eine `FreezeCgroup`-Aktion, nachdem der Freeze-Datensatz
/// bereits durabel geschrieben ist.
///
/// # Description
/// Siehe Moduldoku, Abschnitt „Audit vor Fehlerpfad", für die Reihenfolge:
/// [`FreezeStore::freeze`] läuft **vor** [`crate::ladder::Ladder::stage_for`]
/// und [`crate::action::Action::authorize`].
///
/// # Arguments
/// - `freeze_store` (`&FreezeStore`): wohin der Freeze-Datensatz geschrieben
///   wird.
/// - `finding` (`&Finding<Triaged>`): der auslösende, bereits triagierte
///   Befund.
/// - `proposed` (`Action<Proposed>`): muss `ProposedAction::FreezeCgroup`
///   sein.
/// - `authorized_by` (`ApprovalActor`): wer autorisiert.
/// - `now` (`jiff::Timestamp`): sowohl `frozen_at` für den Freeze-Datensatz
///   als auch `authorized_at` für den Beleg — injiziert, nie gelesen.
///
/// # Returns
/// Das autorisierte, versandbereite `Action<Authorized>`.
///
/// # Errors
/// - [`EscalateError::NotAFreezeAction`]: `proposed` ist keine
///   `FreezeCgroup`-Variante.
/// - [`EscalateError::Store`]: der Freeze-Datensatz existiert bereits
///   (aktiv oder aufgelöst) oder der `FreezeStore`-Zugriff ist
///   fehlgeschlagen.
/// - [`EscalateError::NotEscalatable`]: der Befund rechtfertigt keine Stufe.
/// - Alles, was [`crate::action::Action::authorize`] zurückgeben kann.
///
/// # Examples
/// ```rust,no_run
/// use harw_dod_escalate::{authorize_freeze, Action};
/// use harw_dod_rules::{Finding, Triaged};
/// use harw_dod_warden_proto::ProposedAction;
/// use harw_session_store::FreezeStore;
/// use harw_types::{ApprovalActor, CgroupId};
/// use jiff::Timestamp;
///
/// fn run(store: &FreezeStore, finding: &Finding<Triaged>) {
///     let proposed = Action::propose(ProposedAction::FreezeCgroup {
///         cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
///     });
///     let _ = authorize_freeze(
///         store,
///         finding,
///         proposed,
///         ApprovalActor::Operator { id: "operator-1".to_string() },
///         Timestamp::now(),
///     );
/// }
/// ```
pub fn authorize_freeze(
    freeze_store: &FreezeStore,
    finding: &Finding<Triaged>,
    proposed: Action<Proposed>,
    authorized_by: ApprovalActor,
    now: Timestamp,
) -> EscalateResult<Action<Authorized>> {
    let cgroup = match proposed.proposed() {
        ProposedAction::FreezeCgroup { cgroup } => cgroup.clone(),
        _ => return Err(EscalateError::NotAFreezeAction),
    };

    // Audit vor Fehlerpfad (siehe Moduldoku): der Freeze-Datensatz wird
    // geschrieben, BEVOR die fehlschlagbare Stufen- und
    // Autorisierungsprüfung läuft.
    freeze_store.freeze(&Freeze {
        cgroup,
        finding: finding.id().clone(),
        frozen_at: now,
        expires_at: None,
    })?;

    let stage = Ladder::stage_for(finding).ok_or(EscalateError::NotEscalatable)?;
    proposed.authorize(finding, stage, authorized_by, now)
}

/// Autorisiert eine `ReleaseCgroup`-Aktion, nachdem der zugehörige Freeze
/// bereits durabel aufgelöst ist.
///
/// # Description
/// Siehe Moduldoku, Abschnitt „Audit vor Fehlerpfad": [`FreezeStore::resolve`]
/// läuft vor der Autorisierungsprüfung.
///
/// # Arguments
/// - `freeze_store` (`&FreezeStore`): wo der aufzulösende Freeze liegt.
/// - `finding` (`&Finding<Triaged>`): der auslösende, bereits triagierte
///   Befund.
/// - `proposed` (`Action<Proposed>`): muss `ProposedAction::ReleaseCgroup`
///   sein.
/// - `authorized_by` (`ApprovalActor`): wer autorisiert.
/// - `frozen_at` (`jiff::Timestamp`): der ursprüngliche Freeze-Zeitpunkt —
///   Teil des `FreezeStore`-Schlüssels.
/// - `now` (`jiff::Timestamp`): sowohl `resolved_at` für die Auflösung als
///   auch `authorized_at` für den Beleg.
///
/// # Returns
/// Das autorisierte `Action<Authorized>` zusammen mit der durabel
/// geschriebenen [`FreezeResolution`].
///
/// # Errors
/// - [`EscalateError::NotAReleaseAction`]: `proposed` ist keine
///   `ReleaseCgroup`-Variante.
/// - [`EscalateError::Store`]: kein aktiver Freeze für diesen Schlüssel, oder
///   der `FreezeStore`-Zugriff ist fehlgeschlagen.
/// - [`EscalateError::NotEscalatable`]: der Befund rechtfertigt keine Stufe.
/// - Alles, was [`crate::action::Action::authorize`] zurückgeben kann.
pub fn authorize_release(
    freeze_store: &FreezeStore,
    finding: &Finding<Triaged>,
    proposed: Action<Proposed>,
    authorized_by: ApprovalActor,
    frozen_at: Timestamp,
    now: Timestamp,
) -> EscalateResult<(Action<Authorized>, FreezeResolution)> {
    let cgroup = match proposed.proposed() {
        ProposedAction::ReleaseCgroup { cgroup } => cgroup.clone(),
        _ => return Err(EscalateError::NotAReleaseAction),
    };

    // Audit vor Fehlerpfad: die Auflösung wird durabel geschrieben, BEVOR
    // die fehlschlagbare Stufen- und Autorisierungsprüfung läuft.
    let resolution = freeze_store.resolve(&cgroup, finding.id(), frozen_at, now)?;

    let stage = Ladder::stage_for(finding).ok_or(EscalateError::NotEscalatable)?;
    let action = proposed.authorize(finding, stage, authorized_by, now)?;
    Ok((action, resolution))
}

/// Autorisiert eine stufengebundene Aktion ohne `FreezeStore`-Bezug
/// (`IsolateNetwork`, `KillProcessTree`).
///
/// # Description
/// Kein Audit-Datensatz zu schreiben — diese beiden Aktionen haben keinen
/// Bezug zu [`FreezeStore`] (siehe Moduldoku, Abschnitt „Audit vor
/// Fehlerpfad"). Reiner Aufruf von [`crate::ladder::Ladder::stage_for`]
/// gefolgt von [`crate::action::Action::authorize`].
///
/// # Arguments
/// - `finding` (`&Finding<Triaged>`): der auslösende, bereits triagierte
///   Befund.
/// - `proposed` (`Action<Proposed>`): die vorgeschlagene Aktion.
/// - `authorized_by` (`ApprovalActor`): wer autorisiert.
/// - `now` (`jiff::Timestamp`): `authorized_at` für den Beleg.
///
/// # Returns
/// Das autorisierte `Action<Authorized>`.
///
/// # Errors
/// - [`EscalateError::NotEscalatable`]: der Befund rechtfertigt keine Stufe.
/// - Alles, was [`crate::action::Action::authorize`] zurückgeben kann —
///   insbesondere [`EscalateError::NotAdmissibleAtStage`], wenn `proposed`
///   `IsolateNetwork`/`KillProcessTree` ist und die ermittelte Stufe nur
///   `RuleTriggered` erreicht.
pub fn authorize_stage_gated(
    finding: &Finding<Triaged>,
    proposed: Action<Proposed>,
    authorized_by: ApprovalActor,
    now: Timestamp,
) -> EscalateResult<Action<Authorized>> {
    let stage = Ladder::stage_for(finding).ok_or(EscalateError::NotEscalatable)?;
    proposed.authorize(finding, stage, authorized_by, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_dod_rules::{FindingKind, Verdict, triaged_finding_for_test};
    use harw_dod_signals::{Hardness, Severity};
    use harw_dod_warden_proto::EscalationStage;
    use harw_types::{CgroupId, FindingId};

    // `Severity::Critical` wird derzeit von keiner echten Regel erzeugt
    // (siehe `harw-dod-rules/src/rules/*`); die Fixtur muss sie trotzdem
    // direkt setzen können, um `Ladder::stage_for`s `Escalated`-Schwelle zu
    // erreichen. `triaged_finding_for_test` (Feature `test-support`, siehe
    // `harw-dod-rules/src/finding.rs`-Moduldoku) ist genau dafür da.
    fn finding_with(severity: Severity, hardness: Hardness, verdict: Verdict) -> Finding<Triaged> {
        triaged_finding_for_test(
            "egress-flow",
            FindingKind::RuleTriggered,
            severity,
            hardness,
            "egress flow to evil.example.com:443 is outside the allowed network scope",
            jiff::Timestamp::UNIX_EPOCH,
            FindingId::try_from_str("freeze-ops-test-finding").expect("non-empty id"),
            verdict,
        )
    }

    fn actor() -> ApprovalActor {
        ApprovalActor::Operator {
            id: "operator-1".to_string(),
        }
    }

    #[test]
    fn test_authorize_freeze_rejects_non_freeze_action() {
        let temp = tempfile::tempdir().unwrap();
        let store = FreezeStore::new(temp.path());
        let finding = finding_with(Severity::High, Hardness::Observed, Verdict::Confirmed);
        let proposed = Action::propose(ProposedAction::ReleaseCgroup {
            cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
        });

        let err = authorize_freeze(&store, &finding, proposed, actor(), Timestamp::UNIX_EPOCH)
            .unwrap_err();
        assert!(matches!(err, EscalateError::NotAFreezeAction));
        assert!(store.active().unwrap().is_empty());
    }

    #[test]
    fn test_authorize_freeze_succeeds_and_persists_the_record() {
        let temp = tempfile::tempdir().unwrap();
        let store = FreezeStore::new(temp.path());
        let finding = finding_with(Severity::High, Hardness::Observed, Verdict::Confirmed);
        let proposed = Action::propose(ProposedAction::FreezeCgroup {
            cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
        });

        let authorized =
            authorize_freeze(&store, &finding, proposed, actor(), Timestamp::UNIX_EPOCH)
                .expect("confirmed, rule-triggered finding authorizes a freeze");

        assert_eq!(store.active().unwrap().len(), 1);
        assert!(authorized
            .request()
            .proof
            .verify(finding.id(), &authorized.request().action)
            .is_ok());
    }

    /// Der Audit-Eintrag steht vor dem Fehlerpfad: eine Autorisierung, die
    /// scheitert, weil der Befund keine Stufe rechtfertigt, hinterlässt
    /// trotzdem ihren Freeze-Datensatz.
    #[test]
    fn test_a_failing_authorization_still_leaves_its_audit_entry() {
        let temp = tempfile::tempdir().unwrap();
        let store = FreezeStore::new(temp.path());
        // `FalsePositive` sorgt dafür, dass `Ladder::stage_for` `None`
        // liefert und `authorize_freeze` mit `NotEscalatable` scheitert —
        // aber erst NACHDEM der Freeze-Datensatz bereits geschrieben wurde.
        let finding = finding_with(Severity::Critical, Hardness::Observed, Verdict::FalsePositive);
        let proposed = Action::propose(ProposedAction::FreezeCgroup {
            cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
        });

        let err = authorize_freeze(&store, &finding, proposed, actor(), Timestamp::UNIX_EPOCH)
            .unwrap_err();

        assert!(matches!(err, EscalateError::NotEscalatable));
        assert_eq!(
            store.active().unwrap().len(),
            1,
            "the freeze record must survive the downstream authorization failure"
        );
    }

    #[test]
    fn test_authorize_release_rejects_non_release_action() {
        let temp = tempfile::tempdir().unwrap();
        let store = FreezeStore::new(temp.path());
        let finding = finding_with(Severity::High, Hardness::Observed, Verdict::Confirmed);
        let proposed = Action::propose(ProposedAction::FreezeCgroup {
            cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
        });

        let err = authorize_release(
            &store,
            &finding,
            proposed,
            actor(),
            Timestamp::UNIX_EPOCH,
            Timestamp::UNIX_EPOCH,
        )
        .unwrap_err();
        assert!(matches!(err, EscalateError::NotAReleaseAction));
    }

    #[test]
    fn test_authorize_release_resolves_an_active_freeze_and_authorizes() {
        let temp = tempfile::tempdir().unwrap();
        let store = FreezeStore::new(temp.path());
        let cgroup = CgroupId::try_from_str("cgroup-1").unwrap();
        let finding = finding_with(Severity::High, Hardness::Observed, Verdict::Confirmed);
        store
            .freeze(&Freeze {
                cgroup: cgroup.clone(),
                finding: finding.id().clone(),
                frozen_at: Timestamp::UNIX_EPOCH,
                expires_at: None,
            })
            .unwrap();

        let proposed = Action::propose(ProposedAction::ReleaseCgroup { cgroup });
        let (authorized, resolution) = authorize_release(
            &store,
            &finding,
            proposed,
            actor(),
            Timestamp::UNIX_EPOCH,
            Timestamp::UNIX_EPOCH,
        )
        .expect("an active freeze resolves and the release authorizes");

        assert!(store.active().unwrap().is_empty());
        assert_eq!(resolution.freeze.finding, *finding.id());
        assert!(authorized
            .request()
            .proof
            .verify(finding.id(), &authorized.request().action)
            .is_ok());
    }

    #[test]
    fn test_authorize_stage_gated_requires_escalated_for_kill() {
        let finding = finding_with(Severity::High, Hardness::Observed, Verdict::Confirmed);
        let proposed = Action::propose(ProposedAction::KillProcessTree {
            cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
        });

        let err = authorize_stage_gated(&finding, proposed, actor(), Timestamp::UNIX_EPOCH)
            .unwrap_err();
        assert!(matches!(err, EscalateError::NotAdmissibleAtStage));
    }

    #[test]
    fn test_authorize_stage_gated_succeeds_for_kill_when_escalated() {
        let finding = finding_with(Severity::Critical, Hardness::Observed, Verdict::Confirmed);
        assert_eq!(Ladder::stage_for(&finding), Some(EscalationStage::Escalated));
        let proposed = Action::propose(ProposedAction::KillProcessTree {
            cgroup: CgroupId::try_from_str("cgroup-1").unwrap(),
        });

        let authorized = authorize_stage_gated(&finding, proposed, actor(), Timestamp::UNIX_EPOCH)
            .expect("critical + observed reaches Escalated, which admits KillProcessTree");
        assert!(authorized
            .request()
            .proof
            .verify(finding.id(), &authorized.request().action)
            .is_ok());
    }

    // -- Rekonziliation ---------------------------------------------------------

    #[test]
    fn test_reconcile_expired_freezes_lifts_a_due_freeze_and_spares_an_indefinite_one() {
        let temp = tempfile::tempdir().unwrap();
        let store = FreezeStore::new(temp.path());
        let now = Timestamp::UNIX_EPOCH
            .checked_add(jiff::SignedDuration::from_secs(3600))
            .unwrap();

        let due = Freeze {
            cgroup: CgroupId::try_from_str("cgroup-due").unwrap(),
            finding: harw_types::FindingId::try_from_str("finding-1").unwrap(),
            frozen_at: Timestamp::UNIX_EPOCH,
            expires_at: Some(now.checked_sub(jiff::SignedDuration::from_secs(1)).unwrap()),
        };
        let indefinite = Freeze {
            cgroup: CgroupId::try_from_str("cgroup-indefinite").unwrap(),
            finding: harw_types::FindingId::try_from_str("finding-2").unwrap(),
            frozen_at: Timestamp::UNIX_EPOCH,
            expires_at: None,
        };
        store.freeze(&due).unwrap();
        store.freeze(&indefinite).unwrap();

        let expired = reconcile_expired_freezes(&store, now).unwrap();

        assert_eq!(expired, vec![due]);
        assert_eq!(store.active().unwrap(), vec![indefinite]);
    }
}
