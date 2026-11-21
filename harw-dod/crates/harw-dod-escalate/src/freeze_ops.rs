//! Verdrahtung zwischen der Eskalationsleiter und der [`FreezeStore`]-
//! Persistenz: [`authorize_freeze`], [`authorize_thaw`],
//! [`authorize_stage_gated`], [`reconcile_expired_freezes`].
//!
//! # Unsignierte Freigaben (PL-100 WP 3.0)
//! Jede Funktion hier liefert ein `Action<Authorized>`, also den
//! **unsignierten** [`crate::action::AuthorizedAction`] — keinen
//! Wire-Auftrag, keinen Beleg, keinen MAC. Signieren ist Sache von
//! `harw-escalator` (WP 3.2); siehe `action`-Moduldoku, Abschnitt „Diese
//! Crate signiert nichts". `authorize_thaw` ersetzt das frühere
//! `authorize_release`: die stillgelegte Aktion `ReleaseCgroup` hob Freeze
//! und Isolation gemeinsam auf, `ThawCgroup` hebt nur den Freeze auf — genau
//! das, was der [`FreezeStore`]-Datensatz festhält.
//!
//! # Autorisierung vor Zustand
//! **[`FreezeStore`] ist Zustand, kein Audit-Log:** er hält fest, ob ein
//! Freeze noch in Kraft ist (siehe Moduldoku von
//! `harw-session-store/src/freeze.rs`). [`authorize_freeze`] und
//! [`authorize_thaw`] führen deshalb **zuerst** die nebenwirkungsfreien
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
//! zurückgegeben (fail closed: keine Freigabe ohne Datensatz).
//! `authorize_stage_gated` (für `IsolateNetwork`/`LiftIsolation`/
//! `KillProcessTree`) hat keinen `FreezeStore`-Bezug und lehnt deshalb
//! `FreezeCgroup` und `ThawCgroup` ab
//! ([`EscalateError::NotAStageGatedAction`]): eine Freigabe dieser beiden
//! Aktionen ohne Datensatz ließe Ledger und Warden auseinanderlaufen.
//!
//! # Rekonziliationsdisziplin beim Start
//! [`reconcile_expired_freezes`] ist die von
//! `harw-session-store/src/freeze.rs`-Moduldoku (Abschnitt „Expiry")
//! angekündigte freie Funktion: sie wird **nicht** automatisch aus
//! [`authorize_freeze`]/[`authorize_thaw`] aufgerufen — genau wie
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
use harw_dod_warden_types::ProposedAction;
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
/// let Ok(temp) = tempfile::tempdir() else { return };
/// let store = FreezeStore::new(temp.path());
/// // `now` wird injiziert, nie von der Systemuhr gelesen.
/// let Ok(expired) = reconcile_expired_freezes(&store, Timestamp::UNIX_EPOCH) else {
///     return;
/// };
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
///   als auch `authorized_at` der Freigabe — injiziert, nie gelesen.
///
/// # Returns
/// Das autorisierte `Action<Authorized>` mit dem unsignierten
/// [`crate::action::AuthorizedAction`] (signiert wird erst im Escalator).
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
/// use harw_dod_escalate::{authorize_freeze, Action, EscalateResult};
/// use harw_dod_rules::{Finding, Triaged};
/// use harw_dod_warden_types::ProposedAction;
/// use harw_session_store::FreezeStore;
/// use harw_types::{ApprovalActor, CgroupId};
/// use jiff::Timestamp;
///
/// fn run(store: &FreezeStore, finding: &Finding<Triaged>, cgroup: CgroupId) -> EscalateResult<()> {
///     let proposed = Action::propose(ProposedAction::FreezeCgroup { cgroup });
///     let authorized = authorize_freeze(
///         store,
///         finding,
///         proposed,
///         ApprovalActor::Operator { id: "operator-1".to_string() },
///         Timestamp::UNIX_EPOCH,
///     )?;
///     assert!(matches!(
///         authorized.body().action(),
///         harw_dod_warden_types::WardenAction::FreezeCgroup { .. }
///     ));
///     Ok(())
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

/// Autorisiert eine `ThawCgroup`-Aktion und löst erst danach den
/// zugehörigen Freeze durabel auf.
///
/// # Description
/// Nachfolger von `authorize_release` (PL-100 WP 3.0; die Aktion
/// `ReleaseCgroup` ist seit WP 3.1 stillgelegt). Siehe Moduldoku, Abschnitt
/// „Autorisierung vor Zustand": [`crate::ladder::Ladder::stage_for`], dann
/// [`crate::action::Action::authorize`], und erst **danach**
/// [`FreezeStore::resolve`]. Hebt nur den Freeze auf, nie eine
/// Netzisolation (dafür `LiftIsolation` über [`authorize_stage_gated`]).
///
/// # Arguments
/// - `freeze_store` (`&FreezeStore`): wo der aufzulösende Freeze liegt.
/// - `finding` (`&Finding<Triaged>`): der auslösende, bereits triagierte
///   Befund.
/// - `proposed` (`Action<Proposed>`): muss `ProposedAction::ThawCgroup`
///   sein.
/// - `authorized_by` (`ApprovalActor`): wer autorisiert.
/// - `frozen_at` (`jiff::Timestamp`): der ursprüngliche Freeze-Zeitpunkt —
///   Teil des `FreezeStore`-Schlüssels.
/// - `now` (`jiff::Timestamp`): sowohl `resolved_at` für die Auflösung als
///   auch `authorized_at` der Freigabe.
///
/// # Returns
/// Das autorisierte `Action<Authorized>` (unsigniert) zusammen mit der
/// durabel geschriebenen [`FreezeResolution`].
///
/// # Errors
/// - [`EscalateError::NotAThawAction`]: `proposed` ist keine
///   `ThawCgroup`-Variante (auch nicht das stillgelegte `ReleaseCgroup`).
/// - [`EscalateError::NotEscalatable`]: der Befund rechtfertigt keine Stufe;
///   der Freeze bleibt aktiv.
/// - Alles, was [`crate::action::Action::authorize`] zurückgeben kann; der
///   Freeze bleibt aktiv.
/// - [`EscalateError::Store`]: kein aktiver Freeze für diesen Schlüssel
///   (`FreezeNotFound`), der Freeze ist bereits aufgelöst
///   (`FreezeAlreadyResolved`), oder der `FreezeStore`-Zugriff ist
///   fehlgeschlagen; die bereits autorisierte Aktion wird verworfen.
///
/// # Examples
/// ```rust,no_run
/// use harw_dod_escalate::{authorize_thaw, Action, EscalateResult};
/// use harw_dod_rules::{Finding, Triaged};
/// use harw_dod_warden_types::ProposedAction;
/// use harw_session_store::FreezeStore;
/// use harw_types::{ApprovalActor, CgroupId};
/// use jiff::Timestamp;
///
/// fn run(
///     store: &FreezeStore,
///     finding: &Finding<Triaged>,
///     cgroup: CgroupId,
///     frozen_at: Timestamp,
/// ) -> EscalateResult<()> {
///     let proposed = Action::propose(ProposedAction::ThawCgroup { cgroup });
///     let (_authorized, resolution) = authorize_thaw(
///         store,
///         finding,
///         proposed,
///         ApprovalActor::Operator { id: "operator-1".to_string() },
///         frozen_at,
///         Timestamp::UNIX_EPOCH,
///     )?;
///     assert_eq!(resolution.freeze.frozen_at, frozen_at);
///     Ok(())
/// }
/// ```
pub fn authorize_thaw(
    freeze_store: &FreezeStore,
    finding: &Finding<Triaged>,
    proposed: Action<Proposed>,
    authorized_by: ApprovalActor,
    frozen_at: Timestamp,
    now: Timestamp,
) -> EscalateResult<(Action<Authorized>, FreezeResolution)> {
    let cgroup = match proposed.proposed() {
        ProposedAction::ThawCgroup { cgroup } => cgroup.clone(),
        _ => return Err(EscalateError::NotAThawAction),
    };

    let stage = Ladder::stage_for(finding).ok_or(EscalateError::NotEscalatable)?;
    let action = proposed.authorize(finding, stage, authorized_by, now)?;

    // Zustand erst nach Autorisierung (siehe Moduldoku).
    let resolution = freeze_store.resolve(&cgroup, finding.id(), frozen_at, now)?;
    Ok((action, resolution))
}

/// Autorisiert eine stufengebundene Aktion ohne `FreezeStore`-Bezug
/// (`IsolateNetwork`, `LiftIsolation`, `KillProcessTree`).
///
/// # Description
/// Kein Zustand zu schreiben — diese Aktionen haben keinen Bezug zu
/// [`FreezeStore`] (siehe Moduldoku, Abschnitt „Autorisierung vor
/// Zustand"). `FreezeCgroup` und `ThawCgroup` verändern dagegen den
/// Freeze-Datensatz: sie werden **vor** jeder Stufenprüfung abgelehnt und
/// laufen nur über [`authorize_freeze`] bzw. [`authorize_thaw`] („keine
/// Freigabe ohne Datensatz"). Sonst Aufruf von
/// [`crate::ladder::Ladder::stage_for`] gefolgt von
/// [`crate::action::Action::authorize`]. Die Bestätigung „nicht
/// umkehrbar" (O4) für Kill und Isolation ist nicht Teil dieser unsignierten
/// Freigabe; sie gehört in den signierten v2-Auftrag des Escalators.
///
/// # Arguments
/// - `finding` (`&Finding<Triaged>`): der auslösende, bereits triagierte
///   Befund.
/// - `proposed` (`Action<Proposed>`): die vorgeschlagene Aktion.
/// - `authorized_by` (`ApprovalActor`): wer autorisiert.
/// - `now` (`jiff::Timestamp`): `authorized_at` der Freigabe.
///
/// # Returns
/// Das autorisierte `Action<Authorized>` (unsigniert).
///
/// # Errors
/// - [`EscalateError::NotAStageGatedAction`]: `proposed` ist
///   `FreezeCgroup` oder `ThawCgroup` (dafür [`authorize_freeze`] bzw.
///   [`authorize_thaw`]); geprüft vor der Stufe, unabhängig vom Befund.
/// - [`EscalateError::NotEscalatable`]: der Befund rechtfertigt keine Stufe.
/// - Alles, was [`crate::action::Action::authorize`] zurückgeben kann —
///   insbesondere [`EscalateError::NotAdmissibleAtStage`], wenn `proposed`
///   `IsolateNetwork`/`LiftIsolation`/`KillProcessTree` ist und die
///   ermittelte Stufe nur `RuleTriggered` erreicht, und
///   [`EscalateError::RetiredAction`] für das stillgelegte `ReleaseCgroup`.
pub fn authorize_stage_gated(
    finding: &Finding<Triaged>,
    proposed: Action<Proposed>,
    authorized_by: ApprovalActor,
    now: Timestamp,
) -> EscalateResult<Action<Authorized>> {
    // Freeze und Thaw nur mit FreezeStore-Datensatz (siehe Moduldoku).
    if matches!(
        proposed.proposed(),
        ProposedAction::FreezeCgroup { .. } | ProposedAction::ThawCgroup { .. }
    ) {
        return Err(EscalateError::NotAStageGatedAction);
    }
    let stage = Ladder::stage_for(finding).ok_or(EscalateError::NotEscalatable)?;
    proposed.authorize(finding, stage, authorized_by, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_dod_rules::{FindingKind, Verdict, triaged_finding_for_test};
    use harw_dod_signals::{Hardness, Severity};
    use harw_dod_warden_types::{EscalationStage, WardenAction};
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

    /// Prüft, dass der unsignierte Freigabeinhalt genau `expected` für genau
    /// diesen Befund trägt (Ersatz für die frühere v1-Belegprüfung
    /// `proof.verify(..)`; signiert wird erst im Escalator).
    fn assert_body_binds(
        authorized: &Action<Authorized>,
        finding: &Finding<Triaged>,
        expected: &WardenAction,
        stage: EscalationStage,
    ) {
        let body = authorized.body();
        assert_eq!(body.finding_id(), finding.id());
        assert_eq!(body.action(), expected);
        assert_eq!(body.stage(), stage);
        assert_eq!(body.approver(), &actor());
    }

    #[test]
    fn test_authorize_freeze_rejects_non_freeze_action() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = FreezeStore::new(temp.path());
        let finding = finding_with(Severity::High, Hardness::Observed, Verdict::Confirmed)?;
        let proposed = Action::propose(ProposedAction::ThawCgroup {
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
        let cgroup = CgroupId::try_from_str("cgroup-1").map_err(ctx("cgroup id"))?;
        let proposed = Action::propose(ProposedAction::FreezeCgroup {
            cgroup: cgroup.clone(),
        });

        let authorized =
            authorize_freeze(&store, &finding, proposed, actor(), Timestamp::UNIX_EPOCH)
                .map_err(ctx("confirmed, rule-triggered finding authorizes a freeze"))?;

        assert_eq!(store.active().map_err(ctx("active"))?.len(), 1);
        assert_body_binds(
            &authorized,
            &finding,
            &WardenAction::FreezeCgroup { cgroup },
            EscalationStage::RuleTriggered,
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
    fn test_authorize_thaw_rejects_non_thaw_action() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = FreezeStore::new(temp.path());
        let finding = finding_with(Severity::High, Hardness::Observed, Verdict::Confirmed)?;
        let proposed = Action::propose(ProposedAction::FreezeCgroup {
            cgroup: CgroupId::try_from_str("cgroup-1").map_err(ctx("cgroup id"))?,
        });

        let Err(err) = authorize_thaw(
            &store,
            &finding,
            proposed,
            actor(),
            Timestamp::UNIX_EPOCH,
            Timestamp::UNIX_EPOCH,
        ) else {
            return Err(TestError::Unexpected("Err erwartet".to_owned()));
        };
        assert!(matches!(err, EscalateError::NotAThawAction));
        Ok(())
    }

    /// Ablehnungstest für die stillgelegte v1-Variante: `ReleaseCgroup` ist
    /// kein Thaw. Der aktive Freeze bleibt unberührt.
    #[test]
    fn test_authorize_thaw_refuses_the_retired_release_action() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = FreezeStore::new(temp.path());
        let cgroup = CgroupId::try_from_str("cgroup-1").map_err(ctx("cgroup id"))?;
        let finding = finding_with(Severity::Critical, Hardness::Observed, Verdict::Confirmed)?;
        let record = Freeze {
            cgroup: cgroup.clone(),
            finding: finding.id().clone(),
            frozen_at: Timestamp::UNIX_EPOCH,
            expires_at: None,
        };
        store.freeze(&record).map_err(ctx("freeze"))?;
        // `ReleaseCgroup` ist seit PL-100 WP 3.1 `#[deprecated]`; dieser Test
        // nennt die Variante nur, um ihre Ablehnung zu prüfen.
        #[allow(deprecated)]
        let retired = ProposedAction::ReleaseCgroup { cgroup };

        let Err(err) = authorize_thaw(
            &store,
            &finding,
            Action::propose(retired),
            actor(),
            Timestamp::UNIX_EPOCH,
            Timestamp::UNIX_EPOCH,
        ) else {
            return Err(TestError::Unexpected("Err erwartet".to_owned()));
        };
        assert!(matches!(err, EscalateError::NotAThawAction));
        assert_eq!(store.active().map_err(ctx("active"))?, vec![record]);
        Ok(())
    }

    #[test]
    fn test_authorize_thaw_resolves_an_active_freeze_and_authorizes() -> TestResult {
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

        let proposed = Action::propose(ProposedAction::ThawCgroup {
            cgroup: cgroup.clone(),
        });
        let (authorized, resolution) = authorize_thaw(
            &store,
            &finding,
            proposed,
            actor(),
            Timestamp::UNIX_EPOCH,
            Timestamp::UNIX_EPOCH,
        )
        .map_err(ctx("an active freeze resolves and the thaw authorizes"))?;

        assert!(store.active().map_err(ctx("active"))?.is_empty());
        assert_eq!(resolution.freeze.finding, *finding.id());
        assert_body_binds(
            &authorized,
            &finding,
            &WardenAction::ThawCgroup { cgroup },
            EscalationStage::RuleTriggered,
        );
        Ok(())
    }

    /// Autorisierung vor Zustand: ein abgelehnter Thaw schreibt keinen
    /// `Lifted`-Datensatz — der Freeze bleibt aktiv, und ein bestätigter
    /// Folge-Thaw für denselben Schlüssel gelingt.
    #[test]
    fn test_a_refused_thaw_keeps_the_freeze_active() -> TestResult {
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

        let proposed = Action::propose(ProposedAction::ThawCgroup {
            cgroup: cgroup.clone(),
        });
        let Err(err) = authorize_thaw(
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
        let proposed = Action::propose(ProposedAction::ThawCgroup { cgroup });
        let _thawed = authorize_thaw(
            &store,
            &confirmed,
            proposed,
            actor(),
            Timestamp::UNIX_EPOCH,
            Timestamp::UNIX_EPOCH,
        )
        .map_err(ctx("the still active freeze resolves on a confirmed thaw"))?;
        assert!(store.active().map_err(ctx("active"))?.is_empty());
        Ok(())
    }

    /// Wie oben, aber mit `NeedsReview`: auch ein Befund, der noch eine
    /// menschliche Prüfung braucht, lässt den Freeze aktiv.
    #[test]
    fn test_a_thaw_for_needs_review_keeps_the_freeze_active() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = FreezeStore::new(temp.path());
        let cgroup = CgroupId::try_from_str("cgroup-1").map_err(ctx("cgroup id"))?;
        let refused = finding_with(Severity::Critical, Hardness::Observed, Verdict::NeedsReview)?;
        let record = Freeze {
            cgroup: cgroup.clone(),
            finding: refused.id().clone(),
            frozen_at: Timestamp::UNIX_EPOCH,
            expires_at: None,
        };
        store.freeze(&record).map_err(ctx("freeze"))?;

        let proposed = Action::propose(ProposedAction::ThawCgroup { cgroup });
        let Err(err) = authorize_thaw(
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

    /// Ein bestätigter Thaw ohne aktiven Freeze scheitert am Store; die
    /// bereits autorisierte Aktion wird verworfen, nicht zurückgegeben.
    #[test]
    fn test_authorize_thaw_without_active_freeze_is_a_store_error() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = FreezeStore::new(temp.path());
        let finding = finding_with(Severity::Critical, Hardness::Observed, Verdict::Confirmed)?;
        let proposed = Action::propose(ProposedAction::ThawCgroup {
            cgroup: CgroupId::try_from_str("cgroup-1").map_err(ctx("cgroup id"))?,
        });

        let Err(err) = authorize_thaw(
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
        let cgroup = CgroupId::try_from_str("cgroup-1").map_err(ctx("cgroup id"))?;
        let proposed = Action::propose(ProposedAction::KillProcessTree {
            cgroup: cgroup.clone(),
        });

        let authorized = authorize_stage_gated(&finding, proposed, actor(), Timestamp::UNIX_EPOCH)
            .map_err(ctx(
                "critical + observed reaches Escalated, which admits KillProcessTree",
            ))?;
        assert_body_binds(
            &authorized,
            &finding,
            &WardenAction::KillProcessTree { cgroup },
            EscalationStage::Escalated,
        );
        Ok(())
    }

    #[test]
    fn test_authorize_stage_gated_admits_lift_isolation_only_when_escalated() -> TestResult {
        let cgroup = CgroupId::try_from_str("cgroup-1").map_err(ctx("cgroup id"))?;

        let rule_triggered = finding_with(Severity::High, Hardness::Observed, Verdict::Confirmed)?;
        let Err(err) = authorize_stage_gated(
            &rule_triggered,
            Action::propose(ProposedAction::LiftIsolation {
                cgroup: cgroup.clone(),
            }),
            actor(),
            Timestamp::UNIX_EPOCH,
        ) else {
            return Err(TestError::Unexpected("Err erwartet".to_owned()));
        };
        assert!(matches!(err, EscalateError::NotAdmissibleAtStage));

        let escalated = finding_with(Severity::Critical, Hardness::Observed, Verdict::Confirmed)?;
        let authorized = authorize_stage_gated(
            &escalated,
            Action::propose(ProposedAction::LiftIsolation {
                cgroup: cgroup.clone(),
            }),
            actor(),
            Timestamp::UNIX_EPOCH,
        )
        .map_err(ctx("critical + observed admits LiftIsolation"))?;
        assert_body_binds(
            &authorized,
            &escalated,
            &WardenAction::LiftIsolation { cgroup },
            EscalationStage::Escalated,
        );
        Ok(())
    }

    /// Keine Freigabe ohne Datensatz: der Weg ohne `FreezeStore` lehnt
    /// `FreezeCgroup` ab, auch für einen bestätigten, eskalierten Befund.
    /// Derselbe Vorschlag für denselben Befund autorisiert über
    /// [`authorize_freeze`] und schreibt dort den Datensatz — die Ablehnung
    /// liegt am Weg, nicht am Befund.
    #[test]
    fn test_authorize_stage_gated_refuses_freeze() -> TestResult {
        let finding = finding_with(Severity::Critical, Hardness::Observed, Verdict::Confirmed)?;
        let cgroup = CgroupId::try_from_str("cgroup-1").map_err(ctx("cgroup id"))?;

        let Err(err) = authorize_stage_gated(
            &finding,
            Action::propose(ProposedAction::FreezeCgroup {
                cgroup: cgroup.clone(),
            }),
            actor(),
            Timestamp::UNIX_EPOCH,
        ) else {
            return Err(TestError::Unexpected("Err erwartet".to_owned()));
        };
        assert!(matches!(err, EscalateError::NotAStageGatedAction));

        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = FreezeStore::new(temp.path());
        let authorized = authorize_freeze(
            &store,
            &finding,
            Action::propose(ProposedAction::FreezeCgroup {
                cgroup: cgroup.clone(),
            }),
            actor(),
            Timestamp::UNIX_EPOCH,
        )
        .map_err(ctx("the same proposal authorizes through authorize_freeze"))?;
        assert_eq!(store.active().map_err(ctx("active"))?.len(), 1);
        assert_body_binds(
            &authorized,
            &finding,
            &WardenAction::FreezeCgroup { cgroup },
            EscalationStage::Escalated,
        );
        Ok(())
    }

    /// Keine Freigabe ohne Datensatz: ein Thaw am `FreezeStore` vorbei würde
    /// den Freeze dort aktiv lassen, während der Warden ihn aufhebt. Der Weg
    /// ohne Store lehnt `ThawCgroup` deshalb ab; derselbe Vorschlag löst den
    /// aktiven Datensatz über [`authorize_thaw`] auf und autorisiert dort.
    #[test]
    fn test_authorize_stage_gated_refuses_thaw() -> TestResult {
        let finding = finding_with(Severity::Critical, Hardness::Observed, Verdict::Confirmed)?;
        let cgroup = CgroupId::try_from_str("cgroup-1").map_err(ctx("cgroup id"))?;

        let Err(err) = authorize_stage_gated(
            &finding,
            Action::propose(ProposedAction::ThawCgroup {
                cgroup: cgroup.clone(),
            }),
            actor(),
            Timestamp::UNIX_EPOCH,
        ) else {
            return Err(TestError::Unexpected("Err erwartet".to_owned()));
        };
        assert!(matches!(err, EscalateError::NotAStageGatedAction));

        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let store = FreezeStore::new(temp.path());
        store
            .freeze(&Freeze {
                cgroup: cgroup.clone(),
                finding: finding.id().clone(),
                frozen_at: Timestamp::UNIX_EPOCH,
                expires_at: None,
            })
            .map_err(ctx("freeze"))?;
        let (authorized, resolution) = authorize_thaw(
            &store,
            &finding,
            Action::propose(ProposedAction::ThawCgroup {
                cgroup: cgroup.clone(),
            }),
            actor(),
            Timestamp::UNIX_EPOCH,
            Timestamp::UNIX_EPOCH,
        )
        .map_err(ctx("the same proposal authorizes through authorize_thaw"))?;
        assert!(store.active().map_err(ctx("active"))?.is_empty());
        assert_eq!(resolution.freeze.finding, *finding.id());
        assert_body_binds(
            &authorized,
            &finding,
            &WardenAction::ThawCgroup { cgroup },
            EscalationStage::Escalated,
        );
        Ok(())
    }

    /// Die Ablehnung von Freeze und Thaw hängt nicht am Befund: auch ein
    /// Befund ohne Stufe (`FalsePositive`) bekommt für **beide** Varianten
    /// `NotAStageGatedAction`, nicht `NotEscalatable`.
    #[test]
    fn test_authorize_stage_gated_refuses_freeze_and_thaw_before_the_stage_check() -> TestResult {
        let finding = finding_with(Severity::High, Hardness::Observed, Verdict::FalsePositive)?;
        assert_eq!(Ladder::stage_for(&finding), None);
        let cgroup = CgroupId::try_from_str("cgroup-1").map_err(ctx("cgroup id"))?;

        for proposed in [
            ProposedAction::FreezeCgroup {
                cgroup: cgroup.clone(),
            },
            ProposedAction::ThawCgroup {
                cgroup: cgroup.clone(),
            },
        ] {
            let Err(err) = authorize_stage_gated(
                &finding,
                Action::propose(proposed),
                actor(),
                Timestamp::UNIX_EPOCH,
            ) else {
                return Err(TestError::Unexpected("Err erwartet".to_owned()));
            };
            assert!(
                matches!(err, EscalateError::NotAStageGatedAction),
                "Freeze/Thaw wird vor der Stufenprüfung abgelehnt"
            );
        }
        Ok(())
    }

    /// Ablehnungstest für die stillgelegte v1-Variante: auch der Weg ohne
    /// `FreezeStore` autorisiert `ReleaseCgroup` nie.
    #[test]
    fn test_authorize_stage_gated_refuses_the_retired_release_action() -> TestResult {
        let finding = finding_with(Severity::Critical, Hardness::Observed, Verdict::Confirmed)?;
        // `ReleaseCgroup` ist seit PL-100 WP 3.1 `#[deprecated]`; dieser Test
        // nennt die Variante nur, um ihre Ablehnung zu prüfen.
        #[allow(deprecated)]
        let retired = ProposedAction::ReleaseCgroup {
            cgroup: CgroupId::try_from_str("cgroup-1").map_err(ctx("cgroup id"))?,
        };

        let Err(err) = authorize_stage_gated(
            &finding,
            Action::propose(retired),
            actor(),
            Timestamp::UNIX_EPOCH,
        ) else {
            return Err(TestError::Unexpected("Err erwartet".to_owned()));
        };
        assert!(matches!(err, EscalateError::RetiredAction));
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
