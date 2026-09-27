//! Mandanten-Sicht auf Plan- und Goal-Stores (H12, Masterplan v2 §15).
//!
//! # Verantwortungsbereich
//! [`ScopedPlanStore`] und [`ScopedGoalStore`] legen sich als dünne Hülle um
//! einen vorhandenen [`PlanStore`] bzw. [`GoalStore`] und blenden alles aus,
//! was nicht dem Mandanten-Scope des Aufrufers gehört. Die Regel ist genau
//! die von `harw_operations::context::tenant_admits` (dort die kanonische
//! Formulierung; `harw-plan` hängt bewusst nicht von `harw-operations` ab und
//! führt sie deshalb als [`scope_admits`] noch einmal — ein Test sichert die
//! Wertetabelle):
//!
//! - Aufrufer **ohne** Scope (`None`, Einzelnutzer-Betrieb) sieht alles —
//!   die Hülle delegiert dann jede Methode unverändert.
//! - Aufrufer **mit** Scope sieht nur Pläne/Ziele genau seines Mandanten.
//! - Pläne/Ziele **ohne** Mandant (Altbestand) sind für gescopte Aufrufer
//!   unsichtbar (fail-closed).
//!
//! # Fremd ist dasselbe wie unbekannt
//! Ein fremder Plan oder ein fremdes Ziel scheitert **exakt** wie ein nicht
//! existierendes: `current()` liefert [`PlanError::PlanNotFound`] bzw.
//! [`PlanError::GoalNotFound`], ein Zugriff per ID [`PlanError::PlanUnknown`]
//! mit derselben ID, `list_plans` lässt ihn weg, `history` ist leer. Keine
//! Meldung nennt den fremden Mandanten.
//!
//! Zwei bewusste Ausnahmen, weil der Store einen gemeinsamen Namensraum bzw.
//! genau einen Ziel-Slot hat und ein Überschreiben fremder Daten schlimmer
//! wäre als das Durchsickern ihrer bloßen Existenz:
//! 1. `Create` mit einer ID, die ein fremder Plan trägt, scheitert wie jede
//!    doppelte ID mit [`PlanError::PlanExists`].
//! 2. `GoalAction::Set` über einem fremden Ziel wird mit
//!    [`PlanError::ActorNotAuthorized`] (Aktion [`FOREIGN_GOAL_SET_ACTION`])
//!    abgewiesen, statt das fremde Ziel still zu ersetzen.
//!
//! # Anlegen
//! Ein gescopter Aufrufer legt Pläne über
//! [`PlanStore::create_for_tenant`] an (der Mandant steht im selben
//! Schreibvorgang im Snapshot) und Ziele mit `Goal::tenant` = eigenem Scope —
//! die Hülle stempelt ihn bei `Set` selbst, ein mitgegebener Wert wird
//! überschrieben. Ein gescopter Aufrufer kann also weder ungescopte noch
//! fremde Einträge erzeugen.
//!
//! # Nebenläufigkeit
//! Beide Hüllen sind `Send + Sync`, solange der innere Store es ist (Trait-
//! Anforderung). Mandantenprüfung und Mutation sehen denselben Zustand:
//! Plan-Mutationen laufen gescopt über [`PlanStore::apply_batch`] (Plan-ID
//! und Revision werden unter dem Schreib-Lock geprüft, bei einem
//! Revisionskonflikt wird neu gelesen), Ziel-Mutationen über
//! [`GoalStore::apply_guarded`] (Prüfung unter dem Schreib-Lock der beiden
//! `harw-plan`-Stores).

use harw_types::TenantId;

use crate::actions::{PlanAction, PlanEvent};
use crate::catalog::{PlanApproval, PlanMeta, PlanSummary};
use crate::error::{PlanError, PlanResult};
use crate::goal::{Goal, GoalAction, GoalEvent, GoalStore};
use crate::ids::{PlanId, RevisionId};
use crate::store::{PlanRevision, PlanStore};
use crate::types::Plan;

/// Aktionsname in [`PlanError::ActorNotAuthorized`], wenn ein gescopter
/// Aufrufer ein Ziel setzen will, während der Store ein Ziel außerhalb seines
/// Scopes hält.
pub const FOREIGN_GOAL_SET_ACTION: &str = "Goal::Set(Ziel-Slot außerhalb des Mandanten-Scopes)";

/// Wie oft eine gescopte Plan-Mutation bei einem Revisionskonflikt neu
/// gelesen und erneut versucht wird.
const SCOPED_APPLY_ATTEMPTS: usize = 3;

/// Die Sichtbarkeitsregel (identisch zu `harw_operations::context::tenant_admits`).
///
/// # Arguments
/// - `scope` (`Option<&TenantId>`): Mandanten-Scope des Aufrufers.
/// - `item` (`Option<&TenantId>`): Mandant des Plans/Ziels.
///
/// # Returns
/// `true`, wenn der Aufrufer das Element sehen darf.
///
/// # Examples
/// ```rust
/// use harw_plan::tenant_scope::scope_admits;
/// use harw_types::TenantId;
///
/// let a = TenantId::from_str("tenant-a");
/// let b = TenantId::from_str("tenant-b");
/// assert!(scope_admits(None, None));
/// assert!(scope_admits(Some(&a), Some(&a)));
/// assert!(!scope_admits(Some(&a), Some(&b)));
/// assert!(!scope_admits(Some(&a), None));
/// ```
#[must_use]
pub fn scope_admits(scope: Option<&TenantId>, item: Option<&TenantId>) -> bool {
    match scope {
        None => true,
        Some(scope) => item == Some(scope),
    }
}

// ── Plan-Store ────────────────────────────────────────────────────────────────

/// Mandanten-gefilterte Sicht auf einen [`PlanStore`].
///
/// # Description
/// Siehe Modulkopf. Mit `scope == None` ist die Hülle transparent.
///
/// # Examples
/// ```rust
/// use harw_plan::tenant_scope::ScopedPlanStore;
/// use harw_plan::{InMemoryPlanStore, PlanAction, PlanId, PlanStore};
/// use harw_types::TenantId;
///
/// let store = InMemoryPlanStore::new();
/// let a = ScopedPlanStore::new(&store, Some(TenantId::from_str("tenant-a")));
/// let b = ScopedPlanStore::new(&store, Some(TenantId::from_str("tenant-b")));
/// let created = a.apply(
///     PlanAction::Create { plan_id: PlanId::new("p-a"), goal: "Ziel".to_owned() },
///     "human:alice",
/// );
/// assert!(created.is_ok());
/// assert!(a.current().is_ok());
/// assert!(b.current().is_err());
/// ```
pub struct ScopedPlanStore<'a> {
    inner: &'a dyn PlanStore,
    scope: Option<TenantId>,
}

impl<'a> ScopedPlanStore<'a> {
    /// Baut die Hülle.
    ///
    /// # Arguments
    /// - `inner` (`&dyn PlanStore`): der eigentliche Store.
    /// - `scope` (`Option<TenantId>`): Mandanten-Scope des Aufrufers, in einer
    ///   Operation `ctx.tenant().cloned()`.
    #[must_use]
    pub fn new(inner: &'a dyn PlanStore, scope: Option<TenantId>) -> Self {
        Self { inner, scope }
    }

    /// Der Mandanten-Scope dieser Sicht.
    #[must_use]
    pub fn scope(&self) -> Option<&TenantId> {
        self.scope.as_ref()
    }

    /// Darf diese Sicht `plan` sehen?
    #[must_use]
    pub fn admits(&self, plan: &Plan) -> bool {
        scope_admits(self.scope.as_ref(), plan.tenant.as_ref())
    }

    /// Plan `id`, falls sichtbar — sonst derselbe Fehler wie für eine
    /// unbekannte ID.
    fn admitted_by_id(&self, id: &PlanId) -> PlanResult<Plan> {
        let plan = self.inner.plan_by_id(id)?;
        if self.admits(&plan) {
            Ok(plan)
        } else {
            Err(PlanError::PlanUnknown { id: id.clone() })
        }
    }

    /// Gescopte Mutation des aktiven Plans über `apply_batch` (Plan-ID und
    /// Revision unter dem Schreib-Lock geprüft).
    fn apply_scoped(&self, action: PlanAction, actor: &str) -> PlanResult<PlanEvent> {
        let mut last_conflict = None;
        for _ in 0..SCOPED_APPLY_ATTEMPTS {
            let plan = self.current()?;
            match self
                .inner
                .apply_batch(&plan.id, vec![action.clone()], actor, plan.revision)
            {
                Ok(mut applied) => return applied.events.pop().ok_or(PlanError::PlanNotFound),
                Err(conflict @ PlanError::RevisionConflict { .. }) => {
                    last_conflict = Some(conflict);
                }
                // Ein Einzel-Batch meldet die Ursache verpackt; ausgepackt ist
                // sie identisch zu dem, was `apply` gemeldet hätte.
                Err(PlanError::BatchActionRejected { source, .. }) => return Err(*source),
                Err(error) => return Err(error),
            }
        }
        Err(last_conflict.unwrap_or(PlanError::PlanNotFound))
    }
}

impl PlanStore for ScopedPlanStore<'_> {
    fn current(&self) -> PlanResult<Plan> {
        let plan = self.inner.current()?;
        if self.admits(&plan) {
            Ok(plan)
        } else {
            Err(PlanError::PlanNotFound)
        }
    }

    fn revision(&self) -> RevisionId {
        if self.scope.is_none() {
            return self.inner.revision();
        }
        self.current()
            .map_or(RevisionId::new(0), |plan| plan.revision)
    }

    fn apply(&self, action: PlanAction, actor: &str) -> PlanResult<PlanEvent> {
        match action {
            PlanAction::Create { plan_id, goal } => {
                self.create_for_tenant(plan_id, goal, self.scope.clone(), actor)
            }
            other if self.scope.is_none() => self.inner.apply(other, actor),
            other => self.apply_scoped(other, actor),
        }
    }

    fn create_for_tenant(
        &self,
        plan_id: PlanId,
        goal: String,
        tenant: Option<TenantId>,
        actor: &str,
    ) -> PlanResult<PlanEvent> {
        // Ein gescopter Aufrufer legt ausschließlich im eigenen Mandanten an.
        let tenant = match &self.scope {
            Some(scope) => Some(scope.clone()),
            None => tenant,
        };
        match tenant {
            None => self
                .inner
                .apply(PlanAction::Create { plan_id, goal }, actor),
            Some(tenant) => self
                .inner
                .create_for_tenant(plan_id, goal, Some(tenant), actor),
        }
    }

    fn history(&self, since: Option<RevisionId>) -> PlanResult<Vec<PlanEvent>> {
        if self.scope.is_none() {
            return self.inner.history(since);
        }
        match self.current() {
            Ok(_) => self.inner.history(since),
            Err(PlanError::PlanNotFound) => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }

    fn apply_batch(
        &self,
        plan: &PlanId,
        actions: Vec<PlanAction>,
        actor: &str,
        expected_rev: RevisionId,
    ) -> PlanResult<PlanRevision> {
        if self.scope.is_some() {
            // Der Mandant eines Plans ist unveränderlich; `apply_batch` prüft
            // die Plan-ID unter dem Schreib-Lock. Unsichtbar ⇒ wie „kein bzw.
            // ein anderer Plan aktiv“.
            match self.admitted_by_id(plan) {
                Ok(_) => {}
                Err(PlanError::PlanUnknown { .. }) => return Err(PlanError::PlanNotFound),
                Err(error) => return Err(error),
            }
        }
        self.inner.apply_batch(plan, actions, actor, expected_rev)
    }

    fn list_plans(&self) -> PlanResult<Vec<PlanSummary>> {
        let summaries = self.inner.list_plans()?;
        if self.scope.is_none() {
            return Ok(summaries);
        }
        let mut visible = Vec::with_capacity(summaries.len());
        for summary in summaries {
            match self.inner.plan_by_id(&summary.id) {
                Ok(plan) if self.admits(&plan) => visible.push(summary),
                Ok(_) | Err(PlanError::PlanUnknown { .. }) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(visible)
    }

    fn plan_by_id(&self, id: &PlanId) -> PlanResult<Plan> {
        self.admitted_by_id(id)
    }

    fn plan_meta(&self, id: &PlanId) -> PlanResult<PlanMeta> {
        self.admitted_by_id(id)?;
        self.inner.plan_meta(id)
    }

    fn switch_plan(&self, id: &PlanId, actor: &str) -> PlanResult<Plan> {
        self.admitted_by_id(id)?;
        self.inner.switch_plan(id, actor)
    }

    fn archive_plan(&self, id: &PlanId, actor: &str) -> PlanResult<()> {
        self.admitted_by_id(id)?;
        self.inner.archive_plan(id, actor)
    }

    fn set_approval(&self, id: &PlanId, approval: PlanApproval, actor: &str) -> PlanResult<()> {
        self.admitted_by_id(id)?;
        self.inner.set_approval(id, approval, actor)
    }

    // `ready_nodes` und `waves` bleiben Default-Methoden über `self.current()`
    // und sind damit automatisch gescopt.
}

// ── Goal-Store ────────────────────────────────────────────────────────────────

/// Mandanten-gefilterte Sicht auf einen [`GoalStore`].
///
/// # Description
/// Siehe Modulkopf. Mit `scope == None` ist die Hülle transparent.
///
/// # Examples
/// ```rust
/// use harw_plan::goal::GoalStore;
/// use harw_plan::goal_store::InMemoryGoalStore;
/// use harw_plan::tenant_scope::ScopedGoalStore;
/// use harw_types::TenantId;
///
/// let store = InMemoryGoalStore::new();
/// let b = ScopedGoalStore::new(&store, Some(TenantId::from_str("tenant-b")));
/// assert!(b.current().is_err());
/// ```
pub struct ScopedGoalStore<'a> {
    inner: &'a dyn GoalStore,
    scope: Option<TenantId>,
}

impl<'a> ScopedGoalStore<'a> {
    /// Baut die Hülle.
    ///
    /// # Arguments
    /// - `inner` (`&dyn GoalStore`): der eigentliche Store.
    /// - `scope` (`Option<TenantId>`): Mandanten-Scope des Aufrufers, in einer
    ///   Operation `ctx.tenant().cloned()`.
    #[must_use]
    pub fn new(inner: &'a dyn GoalStore, scope: Option<TenantId>) -> Self {
        Self { inner, scope }
    }

    /// Der Mandanten-Scope dieser Sicht.
    #[must_use]
    pub fn scope(&self) -> Option<&TenantId> {
        self.scope.as_ref()
    }

    /// Darf diese Sicht `goal` sehen?
    #[must_use]
    pub fn admits(&self, goal: &Goal) -> bool {
        scope_admits(self.scope.as_ref(), goal.tenant.as_ref())
    }

    /// Stempelt bei `Set` den eigenen Scope in das neue Ziel.
    fn stamp(&self, action: GoalAction) -> GoalAction {
        match (action, &self.scope) {
            (GoalAction::Set { mut goal }, Some(scope)) => {
                goal.tenant = Some(scope.clone());
                GoalAction::Set { goal }
            }
            (other, _) => other,
        }
    }

    /// Die Mandantenprüfung gegen das gespeicherte Ziel.
    fn check(&self, current: Option<&Goal>, is_set: bool, actor: &str) -> PlanResult<()> {
        match current {
            Some(goal) if !self.admits(goal) => {
                if is_set {
                    Err(PlanError::ActorNotAuthorized {
                        action: FOREIGN_GOAL_SET_ACTION.to_owned(),
                        actor: actor.to_owned(),
                    })
                } else {
                    Err(PlanError::GoalNotFound)
                }
            }
            _ => Ok(()),
        }
    }
}

impl GoalStore for ScopedGoalStore<'_> {
    fn current(&self) -> PlanResult<Goal> {
        let goal = self.inner.current()?;
        if self.admits(&goal) {
            Ok(goal)
        } else {
            Err(PlanError::GoalNotFound)
        }
    }

    fn revision(&self) -> u64 {
        if self.scope.is_none() {
            return self.inner.revision();
        }
        self.current().map_or(0, |goal| goal.revision)
    }

    fn apply(&self, action: GoalAction, actor: &str) -> PlanResult<GoalEvent> {
        if self.scope.is_none() {
            return self.inner.apply(action, actor);
        }
        self.apply_guarded(action, actor, &|_| Ok(()))
    }

    fn apply_guarded(
        &self,
        action: GoalAction,
        actor: &str,
        guard: &dyn Fn(Option<&Goal>) -> PlanResult<()>,
    ) -> PlanResult<GoalEvent> {
        if self.scope.is_none() {
            return self.inner.apply_guarded(action, actor, guard);
        }
        let is_set = matches!(action, GoalAction::Set { .. });
        let action = self.stamp(action);
        self.inner.apply_guarded(action, actor, &|current| {
            self.check(current, is_set, actor)?;
            // Der Aufrufer-Guard sieht nur, was diese Sicht sieht.
            guard(current.filter(|goal| self.admits(goal)))
        })
    }

    fn history(&self, since: Option<u64>) -> PlanResult<Vec<GoalEvent>> {
        if self.scope.is_none() {
            return self.inner.history(since);
        }
        // Der Store hält nacheinander mehrere Ziele (jedes `Set` ersetzt). Ein
        // Event gehört dem Ziel des letzten vorangehenden `Set`; sichtbar ist
        // es nur, wenn dieses Ziel sichtbar ist.
        let mut owner_visible = false;
        let mut visible = Vec::new();
        for event in self.inner.history(None)? {
            if let GoalAction::Set { goal } = &event.action {
                owner_visible = self.admits(goal);
            }
            let in_range = since.is_none_or(|from| event.revision >= from);
            if owner_visible && in_range {
                visible.push(event);
            }
        }
        Ok(visible)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_store::FilePlanStore;
    use crate::goal::{GoalId, GoalStatus};
    use crate::goal_store::{FileGoalStore, InMemoryGoalStore};
    use crate::memory_store::InMemoryPlanStore;
    use crate::test_support::{TestError, TestResult};
    use crate::testing::base_node;
    use crate::types::{Criterion, PlanNodeStatus};
    use time::OffsetDateTime;

    const ACTOR: &str = "human:tester";

    fn tenant(name: &str) -> TenantId {
        TenantId::from_str(name)
    }

    fn create(store: &dyn PlanStore, id: &str) -> PlanResult<PlanEvent> {
        store.apply(
            PlanAction::Create {
                plan_id: PlanId::new(id),
                goal: format!("Ziel von {id}"),
            },
            ACTOR,
        )
    }

    fn add_node(store: &dyn PlanStore, id: &str) -> PlanResult<PlanEvent> {
        store.apply(
            PlanAction::AddNode {
                node: base_node(id),
            },
            ACTOR,
        )
    }

    fn goal(id: &str) -> Goal {
        Goal {
            id: GoalId::new(id),
            revision: 0,
            statement: format!("Ziel {id}"),
            non_goals: vec![],
            invariants: vec![],
            acceptance_criteria: vec![],
            constraints: vec![],
            open_questions: vec![],
            status: GoalStatus::Active,
            plan_id: None,
            plan_revision: None,
            evidence: vec![],
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            tenant: None,
        }
    }

    fn set_goal(store: &dyn GoalStore, id: &str) -> PlanResult<GoalEvent> {
        store.apply(GoalAction::Set { goal: goal(id) }, ACTOR)
    }

    fn add_criterion(store: &dyn GoalStore) -> PlanResult<GoalEvent> {
        store.apply(
            GoalAction::AddCriterion {
                criterion: Criterion {
                    description: "belegt".to_owned(),
                    verification: vec![],
                },
            },
            ACTOR,
        )
    }

    /// Fehlertext eines erwarteten Fehlschlags.
    fn err_text<T>(result: PlanResult<T>) -> TestResult<String> {
        match result {
            Ok(_) => Err(TestError::Unexpected(
                "Erfolg statt des erwarteten Fehlers".to_owned(),
            )),
            Err(error) => Ok(error.to_string()),
        }
    }

    #[test]
    fn test_scope_admits_matches_the_operations_rule_table() {
        let a = tenant("tenant-a");
        let b = tenant("tenant-b");
        assert!(scope_admits(None, None));
        assert!(scope_admits(None, Some(&a)));
        assert!(scope_admits(Some(&a), Some(&a)));
        assert!(!scope_admits(Some(&a), Some(&b)));
        assert!(!scope_admits(Some(&a), None));
    }

    #[test]
    fn test_scoped_create_stamps_the_tenant_in_memory_store() -> TestResult {
        let store = InMemoryPlanStore::new();
        let scoped = ScopedPlanStore::new(&store, Some(tenant("tenant-a")));
        create(&scoped, "p-a")?;
        let plan = store.current()?;
        assert_eq!(plan.tenant, Some(tenant("tenant-a")));
        Ok(())
    }

    #[test]
    fn test_unscoped_create_leaves_the_plan_untenanted() -> TestResult {
        let store = InMemoryPlanStore::new();
        let unscoped = ScopedPlanStore::new(&store, None);
        create(&unscoped, "p-free")?;
        assert_eq!(store.current()?.tenant, None);
        Ok(())
    }

    #[test]
    fn test_scoped_create_persists_and_reloads_the_tenant_on_disk() -> TestResult {
        let dir = tempfile::tempdir()?;
        {
            let store = FilePlanStore::new(dir.path())?;
            let scoped = ScopedPlanStore::new(&store, Some(tenant("tenant-a")));
            create(&scoped, "p-disk")?;
            add_node(&scoped, "t-1")?;
        }
        let reopened = FilePlanStore::new(dir.path())?;
        let plan = reopened.plan_by_id(&PlanId::new("p-disk"))?;
        assert_eq!(plan.tenant, Some(tenant("tenant-a")));
        assert_eq!(plan.nodes.len(), 1, "Mutationen behalten den Mandanten");
        Ok(())
    }

    #[test]
    fn test_default_create_for_tenant_rejects_a_tenant_fail_closed() -> TestResult {
        /// Minimaler Fremd-Store: nur die Pflichtmethoden, delegiert an einen
        /// In-Memory-Store, damit die Default-Methode greift.
        struct Foreign(InMemoryPlanStore);
        impl PlanStore for Foreign {
            fn current(&self) -> PlanResult<Plan> {
                self.0.current()
            }
            fn revision(&self) -> RevisionId {
                self.0.revision()
            }
            fn apply(&self, action: PlanAction, actor: &str) -> PlanResult<PlanEvent> {
                self.0.apply(action, actor)
            }
            fn history(&self, since: Option<RevisionId>) -> PlanResult<Vec<PlanEvent>> {
                self.0.history(since)
            }
            fn apply_batch(
                &self,
                plan: &PlanId,
                actions: Vec<PlanAction>,
                actor: &str,
                expected_rev: RevisionId,
            ) -> PlanResult<PlanRevision> {
                self.0.apply_batch(plan, actions, actor, expected_rev)
            }
        }
        let store = Foreign(InMemoryPlanStore::new());
        let result = store.create_for_tenant(
            PlanId::new("p-x"),
            "Ziel".to_owned(),
            Some(tenant("tenant-a")),
            ACTOR,
        );
        assert!(matches!(result, Err(PlanError::CatalogUnsupported { .. })));
        assert!(matches!(store.current(), Err(PlanError::PlanNotFound)));
        Ok(())
    }

    #[test]
    fn test_scoped_list_shows_only_own_plans() -> TestResult {
        let store = InMemoryPlanStore::new();
        create(
            &ScopedPlanStore::new(&store, Some(tenant("tenant-a"))),
            "p-a",
        )?;
        create(
            &ScopedPlanStore::new(&store, Some(tenant("tenant-b"))),
            "p-b",
        )?;
        create(&store, "p-legacy")?;

        let ids = |scope: Option<TenantId>| -> TestResult<Vec<String>> {
            Ok(ScopedPlanStore::new(&store, scope)
                .list_plans()?
                .into_iter()
                .map(|summary| summary.id.as_str().to_owned())
                .collect())
        };
        assert_eq!(ids(Some(tenant("tenant-a")))?, vec!["p-a".to_owned()]);
        assert_eq!(ids(Some(tenant("tenant-b")))?, vec!["p-b".to_owned()]);
        assert_eq!(ids(None)?.len(), 3, "ungescopt sieht alles");
        Ok(())
    }

    #[test]
    fn test_foreign_plan_by_id_fails_exactly_like_an_unknown_id() -> TestResult {
        let store = InMemoryPlanStore::new();
        create(
            &ScopedPlanStore::new(&store, Some(tenant("tenant-a"))),
            "p-x",
        )?;
        let b = ScopedPlanStore::new(&store, Some(tenant("tenant-b")));
        let foreign = PlanId::new("p-x");

        // Referenz: dieselbe ID in einem leeren Store ist schlicht unbekannt.
        let empty = InMemoryPlanStore::new();
        let unknown = err_text(empty.plan_by_id(&foreign))?;

        assert_eq!(err_text(b.plan_by_id(&foreign))?, unknown);
        assert_eq!(err_text(b.plan_meta(&foreign))?, unknown);
        assert_eq!(err_text(b.switch_plan(&foreign, ACTOR))?, unknown);
        assert_eq!(err_text(b.archive_plan(&foreign, ACTOR))?, unknown);
        assert_eq!(
            err_text(b.set_approval(&foreign, PlanApproval::Confirmed, ACTOR))?,
            unknown
        );
        assert!(!unknown.contains("tenant-a"));
        Ok(())
    }

    #[test]
    fn test_foreign_active_plan_reads_and_mutates_like_no_plan() -> TestResult {
        let store = InMemoryPlanStore::new();
        let a = ScopedPlanStore::new(&store, Some(tenant("tenant-a")));
        create(&a, "p-a")?;
        let before = store.current()?;

        let b = ScopedPlanStore::new(&store, Some(tenant("tenant-b")));
        let empty = InMemoryPlanStore::new();
        assert_eq!(err_text(b.current())?, err_text(empty.current())?);
        assert_eq!(
            err_text(add_node(&b, "t-1"))?,
            err_text(add_node(&empty, "t-1"))?
        );
        assert_eq!(
            err_text(b.apply_batch(
                &before.id,
                vec![PlanAction::AddNode {
                    node: base_node("t-2")
                }],
                ACTOR,
                before.revision,
            ))?,
            err_text(empty.apply_batch(&before.id, vec![], ACTOR, before.revision))?
        );
        assert!(b.history(None)?.is_empty());
        assert_eq!(b.revision(), RevisionId::new(0));

        let after = store.current()?;
        assert_eq!(after.revision, before.revision, "nichts verändert");
        assert!(after.nodes.is_empty());
        Ok(())
    }

    #[test]
    fn test_untenanted_plan_is_hidden_from_scoped_but_visible_unscoped() -> TestResult {
        let store = InMemoryPlanStore::new();
        create(&store, "p-legacy")?;
        let scoped = ScopedPlanStore::new(&store, Some(tenant("tenant-a")));
        assert!(matches!(scoped.current(), Err(PlanError::PlanNotFound)));
        let unscoped = ScopedPlanStore::new(&store, None);
        assert_eq!(unscoped.current()?.id.as_str(), "p-legacy");
        Ok(())
    }

    #[test]
    fn test_scoped_mutation_of_own_plan_matches_the_plain_apply_event() -> TestResult {
        let store = InMemoryPlanStore::new();
        let a = ScopedPlanStore::new(&store, Some(tenant("tenant-a")));
        create(&a, "p-a")?;
        let event = add_node(&a, "t-1")?;
        assert_eq!(event.revision, RevisionId::new(2));
        assert_eq!(event.actor, ACTOR);
        let plan = a.current()?;
        assert_eq!(plan.nodes.len(), 1);
        assert_eq!(plan.tenant, Some(tenant("tenant-a")));

        // Eine abgewiesene Aktion meldet dieselbe Ursache wie `apply`.
        let bad = PlanAction::SetStatus {
            id: crate::ids::TaskId::new("t-missing"),
            status: PlanNodeStatus::InProgress,
            reason: None,
        };
        let scoped_error = err_text(a.apply(bad.clone(), ACTOR))?;
        let plain_error = err_text(store.apply(bad, ACTOR))?;
        assert_eq!(scoped_error, plain_error);
        Ok(())
    }

    #[test]
    fn test_scoped_goal_set_stamps_the_tenant() -> TestResult {
        let store = InMemoryGoalStore::new();
        let a = ScopedGoalStore::new(&store, Some(tenant("tenant-a")));
        set_goal(&a, "g-a")?;
        assert_eq!(store.current()?.tenant, Some(tenant("tenant-a")));
        Ok(())
    }

    #[test]
    fn test_foreign_goal_reads_and_mutates_like_a_missing_goal() -> TestResult {
        let store = InMemoryGoalStore::new();
        set_goal(
            &ScopedGoalStore::new(&store, Some(tenant("tenant-a"))),
            "g-a",
        )?;
        let before = store.current()?;

        let b = ScopedGoalStore::new(&store, Some(tenant("tenant-b")));
        let empty = InMemoryGoalStore::new();
        assert_eq!(err_text(b.current())?, err_text(empty.current())?);
        assert_eq!(
            err_text(add_criterion(&b))?,
            err_text(add_criterion(&empty))?
        );
        assert!(b.history(None)?.is_empty());
        assert_eq!(b.revision(), 0);

        let after = store.current()?;
        assert_eq!(after.revision, before.revision, "nichts verändert");
        assert!(after.acceptance_criteria.is_empty());
        Ok(())
    }

    #[test]
    fn test_scoped_goal_set_never_replaces_a_foreign_goal() -> TestResult {
        let dir = tempfile::tempdir()?;
        let store = FileGoalStore::new(dir.path())?;
        set_goal(
            &ScopedGoalStore::new(&store, Some(tenant("tenant-a"))),
            "g-a",
        )?;

        let b = ScopedGoalStore::new(&store, Some(tenant("tenant-b")));
        let result = set_goal(&b, "g-b");
        assert!(matches!(
            result,
            Err(PlanError::ActorNotAuthorized { ref action, .. }) if action == FOREIGN_GOAL_SET_ACTION
        ));
        let kept = store.current()?;
        assert_eq!(kept.id.as_str(), "g-a");
        assert_eq!(kept.tenant, Some(tenant("tenant-a")));
        Ok(())
    }

    #[test]
    fn test_untenanted_goal_is_hidden_from_scoped_but_visible_unscoped() -> TestResult {
        let store = InMemoryGoalStore::new();
        set_goal(&store, "g-legacy")?;
        let scoped = ScopedGoalStore::new(&store, Some(tenant("tenant-a")));
        assert!(matches!(scoped.current(), Err(PlanError::GoalNotFound)));
        assert_eq!(
            ScopedGoalStore::new(&store, None).current()?.id.as_str(),
            "g-legacy"
        );
        Ok(())
    }

    #[test]
    fn test_scoped_goal_history_hides_events_of_foreign_goals() -> TestResult {
        let store = InMemoryGoalStore::new();
        // Ungescopt (Altbestand) angelegt und ergänzt, dann vom Betreiber
        // (ungescopt) durch ein Ziel von tenant-a ersetzt.
        set_goal(&store, "g-legacy")?;
        add_criterion(&store)?;
        let mut owned = goal("g-a");
        owned.tenant = Some(tenant("tenant-a"));
        store.apply(GoalAction::Set { goal: owned }, ACTOR)?;
        let a = ScopedGoalStore::new(&store, Some(tenant("tenant-a")));
        add_criterion(&a)?;

        let history = a.history(None)?;
        assert_eq!(history.len(), 2, "nur Set + Kriterium von tenant-a");
        assert!(history.iter().all(|event| event.revision >= 3));
        assert_eq!(store.history(None)?.len(), 4);
        Ok(())
    }

    #[test]
    fn test_scoped_goal_guard_runs_before_the_mutation() -> TestResult {
        let store = InMemoryGoalStore::new();
        let a = ScopedGoalStore::new(&store, Some(tenant("tenant-a")));
        set_goal(&a, "g-a")?;
        let refused = a.apply_guarded(
            GoalAction::SetStatus {
                status: GoalStatus::Blocked,
                reason: None,
            },
            ACTOR,
            &|current| match current {
                Some(goal) if goal.id.as_str() == "g-a" => Err(PlanError::GoalNotFound),
                _ => Ok(()),
            },
        );
        assert!(matches!(refused, Err(PlanError::GoalNotFound)));
        assert_eq!(store.current()?.status, GoalStatus::Active);
        Ok(())
    }
}
