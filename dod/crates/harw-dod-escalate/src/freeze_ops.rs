//! Verdrahtung zwischen der Eskalationsleiter und der [`FreezeStore`]-
//! Persistenz: [`authorize_freeze`], [`authorize_release`],
//! [`authorize_stage_gated`], [`reconcile_expired_freezes`].
//!
//! # Autorisierung vor Zustand
//! **[`FreezeStore`] ist Zustand, kein Audit-Log:** er hält fest, ob ein
//! Freeze noch in Kraft ist (siehe Moduldoku von
//! `harw-session-store/src/freeze.rs`). [`authorize_freeze`] und
//! [`authorize_release`] führen deshalb **zuerst** die nebenwirkungsfreien
//! Prüfungen aus — [`crate::ladder::Ladder::stage_for`], dann
//! [`crate::action::Action::authorize`]; keine der beiden berührt das
//! Dateisystem. Erst wenn beide gelingen, wird [`FreezeStore::freeze`] bzw.
//! [`FreezeStore::resolve`] aufgerufen — immer noch bevor das
//! `Action<Authorized>` die Funktion verlässt, der Datensatz geht also jeder
//! Warden-Ausführung stets voraus. Eine abgelehnte Autorisierung lässt den
//! Store unverändert; andernfalls hielte er einen Phantom-`.active`-Datensatz
//! für eine nie eingefrorene cgroup oder einen `Lifted`-Datensatz für eine
//! weiterhin eingefrorene. Die Ablehnung wird ausschließlich über das
//! inhaltsfreie `Err` gemeldet; sie durabel zu protokollieren ist Sache des
//! Aufrufers. Scheitert der Store-Schreibvorgang nach erfolgreicher
//! Autorisierung, wird die autorisierte Aktion verworfen, nicht
//! zurückgegeben (fail closed: keine Anforderung ohne Datensatz).
//! `authorize_stage_gated` (für `IsolateNetwork`/`KillProcessTree`) hat
//! keinen `FreezeStore`-Bezug.
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
pub fn reconcile_expired_freezes(
    store: &FreezeStore,
    now: Timestamp,
) -> EscalateResult<Vec<Freeze>> {
    Ok(store.reconcile_expired(now)?)
}

/// Autorisiert eine `FreezeCgroup`-Aktion und schreibt erst danach den
/// Freeze-Datensatz durabel.
///
/// # Description
/// Siehe Moduldoku, Abschnitt „Autorisierung vor Zustand", für die
/// Reihenfolge: [`crate::ladder::Ladder::stage_for`], dann
/// [`crate::action::Action::authorize`], und erst **danach**
/// [`FreezeStore::freeze`].
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
/// - [`EscalateError::NotEscalatable`]: der Befund rechtfertigt keine Stufe;
///   der Store bleibt unverändert.
/// - Alles, was [`crate::action::Action::authorize`] zurückgeben kann; der
///   Store bleibt unverändert.
/// - [`EscalateError::Store`]: der Freeze-Datensatz existiert bereits
///   (aktiv oder aufgelöst) oder der `FreezeStore`-Zugriff ist
///   fehlgeschlagen; die bereits autorisierte Aktion wird verworfen.
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

    let stage = Ladder::stage_for(finding).ok_or(EscalateError::NotEscalatable)?;
    let action = proposed.authorize(finding, stage, authorized_by, now)?;

    // Zustand erst nach Autorisierung (siehe Moduldoku).
    freeze_store.freeze(&Freeze {
        cgroup,
        finding: finding.id().clone(),
        frozen_at: now,
        expires_at: None,
    })?;
    Ok(action)
}

/// Autorisiert eine `ReleaseCgroup`-Aktion und löst erst danach den
/// zugehörigen Freeze durabel auf.
///
/// # Description
/// Siehe Moduldoku, Abschnitt „Autorisierung vor Zustand":
/// [`crate::ladder::Ladder::stage_for`], dann
/// [`crate::action::Action::authorize`], und erst **danach**
/// [`FreezeStore::resolve`].
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
/// - [`EscalateError::NotEscalatable`]: der Befund rechtfertigt keine Stufe;
///   der Freeze bleibt aktiv.
/// - Alles, was [`crate::action::Action::authorize`] zurückgeben kann; der
///   Freeze bleibt aktiv.
/// - [`EscalateError::Store`]: kein aktiver Freeze für diesen Schlüssel
///   (`FreezeNotFound`), der Freeze ist bereits aufgelöst
///   (`FreezeAlreadyResolved`), oder der `FreezeStore`-Zugriff ist
///   fehlgeschlagen; die bereits autorisierte Aktion wird verworfen.
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

    let stage = Ladder::stage_for(finding).ok_or(EscalateError::NotEscalatable)?;
    let action = proposed.authorize(finding, stage, authorized_by, now)?;

    // Zustand erst nach Autorisierung (siehe Moduldoku).
    let resolution = freeze_store.resolve(&cgroup, finding.id(), frozen_at, now)?;
    Ok((action, resolution))
}

/// Autorisiert eine stufengebundene Aktion ohne `FreezeStore`-Bezug
/// (`IsolateNetwork`, `KillProcessTree`).
///
/// # Description
/// Kein Zustand zu schreiben — diese beiden Aktionen haben keinen Bezug zu
/// [`FreezeStore`] (siehe Moduldoku, Abschnitt „Autorisierung vor
/// Zustand"). Reiner Aufruf von [`crate::ladder::Ladder::stage_for`]
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
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_dod_rules::{FindingKind, Verdict, triaged_finding_for_test};
    use harw_dod_signals::{Hardness, Severity};
    use harw_dod_warden_proto::EscalationStage;
    use harw_types::{CgroupId, FindingId};

    // `Severity::Critical` wird derzeit von keiner echten Regel erzeugt
    // (siehe `harw-dod-rules/src/rules/*`); die Fixtur muss sie trotzdem
    // direkt setzen können, um `Ladder::stage_for`s `Escalated`-Schwelle zu
    // erreichen. `triaged_finding_for_test` (Feature `test-support`, siehe
    // `harw-dod-rules/src/finding.rs`-Moduldoku) ist genau dafür da.
    fn finding_with(
        severity: Severity,
        hardness: Hardness,
        verdict: Verdict,
    ) -> TestResult<Finding<Triaged>> {
        let id = FindingId::try_from_str("freeze-ops-test-finding").map_err(ctx("non-empty id"))?;
        Ok(triaged_finding_for_test(
            "egress-flow",
            FindingKind::RuleTriggered,
            severity,
            hardness,
            "egress flow to evil.example.com:443 is outside the allowed network scope",
            jiff::Timestamp::UNIX_EPOCH,
            id,
            verdict,
        ))
    }

    fn actor() -> ApprovalActor {
        ApprovalActor::Operator {
            id: "operator-1".to_string(),
        }
    }

    #[test]
    fn test_authorize_freeze_rejects_non_freeze_action() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = FreezeStore::new(temp.path());
        let finding = finding_with(Severity::High, Hardness::Observed, Verdict::Confirmed)?;
        let proposed = Action::propose(ProposedAction::ReleaseCgroup {
            cgroup: CgroupId::try_from_str("cgroup-1").map_err(ctx("cgroup id"))?,
        });

        let Err(err) = authorize_freeze(&store, &finding, proposed, actor(), Timestamp::UNIX_EPOCH)
        else {
            return Err(TestError::Unexpected("Err erwartet".to_owned()));
        };
        assert!(matches!(err, EscalateError::NotAFreezeAction));
        assert!(store.active().map_err(ctx("active"))?.is_empty());
        Ok(())
    }

    #[test]
    fn test_authorize_freeze_succeeds_and_persists_the_record() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = FreezeStore::new(temp.path());
        let finding = finding_with(Severity::High, Hardness::Observed, Verdict::Confirmed)?;
        let proposed = Action::propose(ProposedAction::FreezeCgroup {
            cgroup: CgroupId::try_from_str("cgroup-1").map_err(ctx("cgroup id"))?,
        });

        let authorized =
            authorize_freeze(&store, &finding, proposed, actor(), Timestamp::UNIX_EPOCH)
                .map_err(ctx("confirmed, rule-triggered finding authorizes a freeze"))?;

        assert_eq!(store.active().map_err(ctx("active"))?.len(), 1);
        assert!(
            authorized
                .request()
                .proof
                .verify(finding.id(), &authorized.request().action)
                .is_ok()
        );
        Ok(())
    }

    /// Autorisierung vor Zustand: eine Autorisierung, die scheitert, weil der
    /// Befund keine Stufe rechtfertigt, hinterlässt keinen Freeze-Datensatz —
    /// ein bestätigter Folgeaufruf für denselben Schlüssel gelingt deshalb.
    #[test]
    fn test_a_refused_freeze_leaves_no_record() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = FreezeStore::new(temp.path());
        let cgroup = CgroupId::try_from_str("cgroup-1").map_err(ctx("cgroup id"))?;
        // `FalsePositive` sorgt dafür, dass `Ladder::stage_for` `None`
        // liefert und `authorize_freeze` mit `NotEscalatable` scheitert —
        // bevor der Store überhaupt berührt wird.
        let refused = finding_with(
            Severity::Critical,
            Hardness::Observed,
            Verdict::FalsePositive,
        )?;
        let proposed = Action::propose(ProposedAction::FreezeCgroup {
            cgroup: cgroup.clone(),
        });

        let Err(err) = authorize_freeze(&store, &refused, proposed, actor(), Timestamp::UNIX_EPOCH)
        else {
            return Err(TestError::Unexpected("Err erwartet".to_owned()));
        };
        assert!(matches!(err, EscalateError::NotEscalatable));
        assert!(store.active().map_err(ctx("active"))?.is_empty());

        // Gleicher Schlüssel (Befund-Id, cgroup, `frozen_at`): ein
        // Phantom-Datensatz ließe diesen Aufruf mit `FreezeAlreadyExists`
        // scheitern.
        let confirmed = finding_with(Severity::Critical, Hardness::Observed, Verdict::Confirmed)?;
        let proposed = Action::propose(ProposedAction::FreezeCgroup { cgroup });
        let _authorized =
            authorize_freeze(&store, &confirmed, proposed, actor(), Timestamp::UNIX_EPOCH)
                .map_err(ctx("no phantom record blocks the confirmed freeze"))?;
        assert_eq!(store.active().map_err(ctx("active"))?.len(), 1);
        Ok(())
    }

    #[test]
    fn test_authorize_release_rejects_non_release_action() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = FreezeStore::new(temp.path());
        let finding = finding_with(Severity::High, Hardness::Observed, Verdict::Confirmed)?;
        let proposed = Action::propose(ProposedAction::FreezeCgroup {
            cgroup: CgroupId::try_from_str("cgroup-1").map_err(ctx("cgroup id"))?,
        });

        let Err(err) = authorize_release(
            &store,
            &finding,
            proposed,
            actor(),
            Timestamp::UNIX_EPOCH,
            Timestamp::UNIX_EPOCH,
        ) else {
            return Err(TestError::Unexpected("Err erwartet".to_owned()));
        };
        assert!(matches!(err, EscalateError::NotAReleaseAction));
        Ok(())
    }

    #[test]
    fn test_authorize_release_resolves_an_active_freeze_and_authorizes() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = FreezeStore::new(temp.path());
        let cgroup = CgroupId::try_from_str("cgroup-1").map_err(ctx("cgroup id"))?;
        let finding = finding_with(Severity::High, Hardness::Observed, Verdict::Confirmed)?;
        store
            .freeze(&Freeze {
                cgroup: cgroup.clone(),
                finding: finding.id().clone(),
                frozen_at: Timestamp::UNIX_EPOCH,
                expires_at: None,
            })
            .map_err(ctx("freeze"))?;

        let proposed = Action::propose(ProposedAction::ReleaseCgroup { cgroup });
        let (authorized, resolution) = authorize_release(
            &store,
            &finding,
            proposed,
            actor(),
            Timestamp::UNIX_EPOCH,
            Timestamp::UNIX_EPOCH,
        )
        .map_err(ctx("an active freeze resolves and the release authorizes"))?;

        assert!(store.active().map_err(ctx("active"))?.is_empty());
        assert_eq!(resolution.freeze.finding, *finding.id());
        assert!(
            authorized
                .request()
                .proof
                .verify(finding.id(), &authorized.request().action)
                .is_ok()
        );
        Ok(())
    }

    /// Autorisierung vor Zustand: eine abgelehnte Freigabe schreibt keinen
    /// `Lifted`-Datensatz — der Freeze bleibt aktiv, und eine bestätigte
    /// Folgefreigabe für denselben Schlüssel gelingt.
    #[test]
    fn test_a_refused_release_keeps_the_freeze_active() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = FreezeStore::new(temp.path());
        let cgroup = CgroupId::try_from_str("cgroup-1").map_err(ctx("cgroup id"))?;
        let refused = finding_with(
            Severity::Critical,
            Hardness::Observed,
            Verdict::FalsePositive,
        )?;
        let record = Freeze {
            cgroup: cgroup.clone(),
            finding: refused.id().clone(),
            frozen_at: Timestamp::UNIX_EPOCH,
            expires_at: None,
        };
        store.freeze(&record).map_err(ctx("freeze"))?;

        let proposed = Action::propose(ProposedAction::ReleaseCgroup {
            cgroup: cgroup.clone(),
        });
        let Err(err) = authorize_release(
            &store,
            &refused,
            proposed,
            actor(),
            Timestamp::UNIX_EPOCH,
            Timestamp::UNIX_EPOCH,
        ) else {
            return Err(TestError::Unexpected("Err erwartet".to_owned()));
        };
        assert!(matches!(err, EscalateError::NotEscalatable));
        assert_eq!(store.active().map_err(ctx("active"))?, vec![record]);

        // Gleicher Schlüssel: ein vorzeitig geschriebener `Lifted`-Datensatz
        // ließe diesen Aufruf mit `FreezeAlreadyResolved` scheitern.
        let confirmed = finding_with(Severity::Critical, Hardness::Observed, Verdict::Confirmed)?;
        let proposed = Action::propose(ProposedAction::ReleaseCgroup { cgroup });
        let _released = authorize_release(
            &store,
            &confirmed,
            proposed,
            actor(),
            Timestamp::UNIX_EPOCH,
            Timestamp::UNIX_EPOCH,
        )
        .map_err(ctx("the still active freeze resolves on a confirmed release"))?;
        assert!(store.active().map_err(ctx("active"))?.is_empty());
        Ok(())
    }

    /// Wie oben, aber mit `NeedsReview`: auch ein Befund, der noch eine
    /// menschliche Prüfung braucht, lässt den Freeze aktiv.
    #[test]
    fn test_a_release_for_needs_review_keeps_the_freeze_active() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = FreezeStore::new(temp.path());
        let cgroup = CgroupId::try_from_str("cgroup-1").map_err(ctx("cgroup id"))?;
        let refused = finding_with(
            Severity::Critical,
            Hardness::Observed,
            Verdict::NeedsReview,
        )?;
        let record = Freeze {
            cgroup: cgroup.clone(),
            finding: refused.id().clone(),
            frozen_at: Timestamp::UNIX_EPOCH,
            expires_at: None,
        };
        store.freeze(&record).map_err(ctx("freeze"))?;

        let proposed = Action::propose(ProposedAction::ReleaseCgroup { cgroup });
        let Err(err) = authorize_release(
            &store,
            &refused,
            proposed,
            actor(),
            Timestamp::UNIX_EPOCH,
            Timestamp::UNIX_EPOCH,
        ) else {
            return Err(TestError::Unexpected("Err erwartet".to_owned()));
        };
        assert!(matches!(err, EscalateError::NotEscalatable));
        assert_eq!(store.active().map_err(ctx("active"))?, vec![record]);
        Ok(())
    }

    /// Eine bestätigte Freigabe ohne aktiven Freeze scheitert am Store; die
    /// bereits autorisierte Aktion wird verworfen, nicht zurückgegeben.
    #[test]
    fn test_authorize_release_without_active_freeze_is_a_store_error() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = FreezeStore::new(temp.path());
        let finding = finding_with(Severity::Critical, Hardness::Observed, Verdict::Confirmed)?;
        let proposed = Action::propose(ProposedAction::ReleaseCgroup {
            cgroup: CgroupId::try_from_str("cgroup-1").map_err(ctx("cgroup id"))?,
        });

        let Err(err) = authorize_release(
            &store,
            &finding,
            proposed,
            actor(),
            Timestamp::UNIX_EPOCH,
            Timestamp::UNIX_EPOCH,
        ) else {
            return Err(TestError::Unexpected("Err erwartet".to_owned()));
        };
        assert!(matches!(err, EscalateError::Store(_)));
        assert!(store.active().map_err(ctx("active"))?.is_empty());
        Ok(())
    }

    #[test]
    fn test_authorize_stage_gated_requires_escalated_for_kill() -> TestResult {
        let finding = finding_with(Severity::High, Hardness::Observed, Verdict::Confirmed)?;
        let proposed = Action::propose(ProposedAction::KillProcessTree {
            cgroup: CgroupId::try_from_str("cgroup-1").map_err(ctx("cgroup id"))?,
        });

        let Err(err) = authorize_stage_gated(&finding, proposed, actor(), Timestamp::UNIX_EPOCH)
        else {
            return Err(TestError::Unexpected("Err erwartet".to_owned()));
        };
        assert!(matches!(err, EscalateError::NotAdmissibleAtStage));
        Ok(())
    }

    #[test]
    fn test_authorize_stage_gated_succeeds_for_kill_when_escalated() -> TestResult {
        let finding = finding_with(Severity::Critical, Hardness::Observed, Verdict::Confirmed)?;
        assert_eq!(
            Ladder::stage_for(&finding),
            Some(EscalationStage::Escalated)
        );
        let proposed = Action::propose(ProposedAction::KillProcessTree {
            cgroup: CgroupId::try_from_str("cgroup-1").map_err(ctx("cgroup id"))?,
        });

        let authorized = authorize_stage_gated(&finding, proposed, actor(), Timestamp::UNIX_EPOCH)
            .map_err(ctx(
                "critical + observed reaches Escalated, which admits KillProcessTree",
            ))?;
        assert!(
            authorized
                .request()
                .proof
                .verify(finding.id(), &authorized.request().action)
                .is_ok()
        );
        Ok(())
    }

    // -- Rekonziliation ---------------------------------------------------------

    #[test]
    fn test_reconcile_expired_freezes_lifts_a_due_freeze_and_spares_an_indefinite_one() -> TestResult
    {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = FreezeStore::new(temp.path());
        let now = Timestamp::UNIX_EPOCH
            .checked_add(jiff::SignedDuration::from_secs(3600))
            .map_err(ctx("checked_add"))?;

        let due = Freeze {
            cgroup: CgroupId::try_from_str("cgroup-due").map_err(ctx("cgroup id"))?,
            finding: harw_types::FindingId::try_from_str("finding-1").map_err(ctx("finding id"))?,
            frozen_at: Timestamp::UNIX_EPOCH,
            expires_at: Some(
                now.checked_sub(jiff::SignedDuration::from_secs(1))
                    .map_err(ctx("checked_sub"))?,
            ),
        };
        let indefinite = Freeze {
            cgroup: CgroupId::try_from_str("cgroup-indefinite").map_err(ctx("cgroup id"))?,
            finding: harw_types::FindingId::try_from_str("finding-2").map_err(ctx("finding id"))?,
            frozen_at: Timestamp::UNIX_EPOCH,
            expires_at: None,
        };
        store.freeze(&due).map_err(ctx("freeze"))?;
        store.freeze(&indefinite).map_err(ctx("freeze"))?;

        let expired = reconcile_expired_freezes(&store, now).map_err(ctx("reconcile"))?;

        assert_eq!(expired, vec![due]);
        assert_eq!(store.active().map_err(ctx("active"))?, vec![indefinite]);
        Ok(())
    }
}
