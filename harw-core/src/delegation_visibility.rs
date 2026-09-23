//! Sichtbare Delegationsziele für ein Modell (Addendum D — Delegationswissen).
//!
//! # Verantwortung
//! Projiziert eine Liste von Kandidaten-Rollennamen auf genau die Ziele, an
//! die der aufrufende Agent tatsächlich delegieren dürfte — mit denselben
//! Prädikaten, die [`crate::child_controller::ManagedAgentSpawner::admit`]
//! bei der echten Admission durchsetzt
//! ([`crate::child_controller::can_delegate_to`]). Das Modul erfindet keine
//! Kandidaten: die Kandidatenliste muss der Aufrufer beibringen (aus den
//! registrierten Rollen des Spawners, sofern von der Session aus erreichbar).
//!
//! Seit Addendum J ist `uia-worker` eine eigene, vollständig abgekapselte
//! Organisationsrolle ([`AgentRoleId::UiaWorker`]) in der versiegelten
//! Spawn-Matrix selbst — der Exklusivitäts-Mechanismus aus Addendum I
//! (`[spawn] exclusive_parent_role`) entfällt ersatzlos.
//!
//! # Schlüsseltypen
//! - [`DelegationTarget`] — ein einzelnes sichtbares Ziel.
//! - [`DelegationTargetKind`] — unterscheidet Worker von Kind-Orchestratoren.
//! - [`visible_delegation_targets`] — die reine Projektionsfunktion.
//!
//! # Nebenläufigkeit
//! Rein, synchron, zustandslos — keine Locks, kein IO.
//!
//! # Fehler
//! Keine — die Funktion liefert bei fehlender Berechtigung einfach eine
//! leere Liste statt eines Fehlers.
//!
//! # Beispiele
//! ```
//! use harw_agent_dsl::roles::AgentRoleId;
//! use harw_core::delegation_visibility::visible_delegation_targets;
//!
//! let candidates = vec![
//!     ("coder".to_owned(), AgentRoleId::Worker),
//!     ("reviewer".to_owned(), AgentRoleId::Worker),
//! ];
//! let targets = visible_delegation_targets(
//!     AgentRoleId::RootOrchestrator,
//!     &candidates,
//!     &[],
//!     3,
//! );
//! assert_eq!(targets.len(), 2);
//! ```

use crate::child_controller::can_delegate_to;
use harw_agent_dsl::roles::AgentRoleId;

/// Art eines sichtbaren Delegationsziels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelegationTargetKind {
    /// Ausführungsworker (keine weitere Delegation).
    Worker,
    /// Untergeordneter Orchestrator.
    ChildOrchestrator,
    /// Exklusiver Schnellhelfer der UIA (`uia-worker`, Addendum J).
    UiaWorker,
    /// Interner Agent, der das Wissen über Agentendefinitionen umsetzt
    /// (`agent-steward`, Addendum K).
    AgentSteward,
}

/// Ein einzelnes Delegationsziel, das der aufrufende Agent dem Modell
/// anbieten darf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationTarget {
    /// Exakter registrierter Rollenname (z. B. für `transfer_to_<name>` oder
    /// einen Kontextblock-Eintrag).
    pub name: String,
    /// Organisatorische Rolle (§3 DSL-Spawn-Matrix) des Ziels.
    pub role: AgentRoleId,
    /// Ob das Ziel ein Worker, ein Kind-Orchestrator oder der `uia-worker` ist.
    pub kind: DelegationTargetKind,
}

/// Projiziert Delegations-Kandidaten auf die für `caller_role` tatsächlich
/// sichtbaren Ziele.
///
/// # Description
/// Verwendet exakt dieselben Prädikate wie
/// [`crate::child_controller::ManagedAgentSpawner::admit`]
/// (über [`crate::child_controller::can_delegate_to`]): die geschlossene
/// Spawn-Matrix ([`harw_agent_dsl::roles::can_spawn`], die `uia-worker`
/// selbst als Rolle trägt, Addendum J) und — für ein `ChildOrchestrator`-Ziel
/// — die exakte Freigabeliste `allowed_child_orchestrators`. Damit kann die
/// dem Modell gezeigte Liste nie mehr zeigen, als tatsächlich admittiert
/// werden dürfte.
///
/// `remaining_depth == 0` liefert immer eine leere Liste — auf dieser Tiefe
/// dürfte ohnehin kein Kind mehr entstehen.
///
/// # Arguments
/// - `caller_role` (`AgentRoleId`): organisatorische Rolle des delegieren
///   wollenden Agenten.
/// - `candidates` (`&[(String, AgentRoleId)]`): Kandidatenliste als
///   (Rollenname, organisatorische Rolle) — vom Aufrufer aus den
///   registrierten Rollen des Spawners zu befüllen, niemals zu erfinden.
/// - `allowed_child_orchestrators` (`&[String]`): exakte Namen, die
///   `caller_role` laut seiner eigenen, eingefrorenen Agentendefinition als
///   Kind-Orchestrator delegieren darf.
/// - `remaining_depth` (`u32`): verbleibende Tiefe bis zur harten
///   Tiefengrenze; `0` bedeutet „keine Kinder mehr möglich".
///
/// # Returns
/// Die sichtbaren Ziele, deterministisch nach `name` sortiert (stabiler
/// Cache-Präfix für einen Kontextblock/System-Prompt). Leer, wenn nichts
/// sichtbar ist.
#[must_use]
pub fn visible_delegation_targets(
    caller_role: AgentRoleId,
    candidates: &[(String, AgentRoleId)],
    allowed_child_orchestrators: &[String],
    remaining_depth: u32,
) -> Vec<DelegationTarget> {
    if remaining_depth == 0 {
        return Vec::new();
    }

    let mut targets: Vec<DelegationTarget> = candidates
        .iter()
        .filter(|(name, role)| {
            can_delegate_to(caller_role, *role, name, allowed_child_orchestrators)
        })
        .map(|(name, role)| DelegationTarget {
            name: name.clone(),
            role: *role,
            kind: match role {
                AgentRoleId::ChildOrchestrator => DelegationTargetKind::ChildOrchestrator,
                AgentRoleId::UiaWorker => DelegationTargetKind::UiaWorker,
                AgentRoleId::AgentSteward => DelegationTargetKind::AgentSteward,
                AgentRoleId::UserInterface
                | AgentRoleId::RootOrchestrator
                | AgentRoleId::Worker => DelegationTargetKind::Worker,
            },
        })
        .collect();
    targets.sort_by(|a, b| a.name.cmp(&b.name));
    targets
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    fn candidates() -> Vec<(String, AgentRoleId)> {
        vec![
            ("zeta-worker".to_owned(), AgentRoleId::Worker),
            ("alpha-sub".to_owned(), AgentRoleId::ChildOrchestrator),
            ("beta-worker".to_owned(), AgentRoleId::Worker),
        ]
    }

    #[test]
    fn root_orchestrator_sees_worker_and_allowed_child_orchestrator_sorted() -> TestResult {
        let targets = visible_delegation_targets(
            AgentRoleId::RootOrchestrator,
            &candidates(),
            &["alpha-sub".to_owned()],
            3,
        );
        let names: Vec<&str> = targets.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["alpha-sub", "beta-worker", "zeta-worker"]);
        assert_eq!(
            targets
                .iter()
                .find(|t| t.name == "alpha-sub")
                .ok_or(TestError::Missing("alpha-sub visible"))?
                .kind,
            DelegationTargetKind::ChildOrchestrator
        );
        Ok(())
    }

    #[test]
    fn child_orchestrator_without_exact_grant_hides_that_target() {
        let targets = visible_delegation_targets(
            AgentRoleId::RootOrchestrator,
            &candidates(),
            &[], // keine Freigabe für "alpha-sub"
            3,
        );
        let names: Vec<&str> = targets.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["beta-worker", "zeta-worker"]);
    }

    #[test]
    fn worker_caller_sees_nothing() {
        let targets = visible_delegation_targets(
            AgentRoleId::Worker,
            &candidates(),
            &["alpha-sub".to_owned()],
            3,
        );
        assert!(targets.is_empty());
    }

    #[test]
    fn zero_remaining_depth_hides_everything() {
        let targets = visible_delegation_targets(
            AgentRoleId::RootOrchestrator,
            &candidates(),
            &["alpha-sub".to_owned()],
            0,
        );
        assert!(targets.is_empty());
    }

    #[test]
    fn empty_candidates_yield_empty_list() {
        let targets = visible_delegation_targets(AgentRoleId::RootOrchestrator, &[], &[], 3);
        assert!(targets.is_empty());
    }

    /// Addendum J: die UIA sieht/spawnt ihren `uia-worker` (eigene, exklusiv
    /// nur der UIA zugängliche Rolle in der versiegelten Matrix selbst).
    #[test]
    fn uia_worker_is_visible_only_to_user_interface() {
        let candidates = vec![("uia-worker".to_owned(), AgentRoleId::UiaWorker)];

        let uia_targets =
            visible_delegation_targets(AgentRoleId::UserInterface, &candidates, &[], 3);
        let names: Vec<&str> = uia_targets.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["uia-worker"],
            "die UIA muss ihren uia-worker sehen"
        );
        assert_eq!(
            uia_targets[0].kind,
            DelegationTargetKind::UiaWorker,
            "uia-worker muss als eigene Kind-Art erkennbar sein"
        );
    }

    /// Root darf den `uia-worker` weder sehen noch spawnen — die Rolle ist
    /// laut versiegelter Matrix exklusiv der UIA vorbehalten.
    #[test]
    fn uia_worker_is_hidden_from_root_orchestrator() {
        let candidates = vec![("uia-worker".to_owned(), AgentRoleId::UiaWorker)];

        let root_targets =
            visible_delegation_targets(AgentRoleId::RootOrchestrator, &candidates, &[], 3);
        assert!(
            root_targets.is_empty(),
            "Root darf den uia-worker nicht sehen"
        );
    }

    /// Auch ein ChildOrchestrator sieht den `uia-worker` nicht.
    #[test]
    fn uia_worker_is_hidden_from_child_orchestrator() {
        let candidates = vec![("uia-worker".to_owned(), AgentRoleId::UiaWorker)];

        let sub_targets =
            visible_delegation_targets(AgentRoleId::ChildOrchestrator, &candidates, &[], 3);
        assert!(
            sub_targets.is_empty(),
            "Sub-Orchestratoren duerfen den uia-worker nicht sehen"
        );
    }

    /// Ein normaler `Worker` bleibt für die UIA weiterhin unsichtbar — die
    /// UIA sieht ausschließlich `RootOrchestrator` und `UiaWorker`.
    #[test]
    fn non_exclusive_worker_stays_hidden_from_user_interface() {
        let candidates = vec![("normal-worker".to_owned(), AgentRoleId::Worker)];

        let uia_targets =
            visible_delegation_targets(AgentRoleId::UserInterface, &candidates, &[], 3);
        assert!(
            uia_targets.is_empty(),
            "ein normaler Worker bleibt fuer die UIA unsichtbar"
        );
    }

    /// Addendum K: sowohl die UIA als auch der RootOrchestrator sehen/spawnen
    /// den `agent-steward`.
    #[test]
    fn agent_steward_is_visible_to_user_interface_and_root_orchestrator() {
        let candidates = vec![("agent-steward".to_owned(), AgentRoleId::AgentSteward)];

        let uia_targets =
            visible_delegation_targets(AgentRoleId::UserInterface, &candidates, &[], 3);
        let uia_names: Vec<&str> = uia_targets.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            uia_names,
            vec!["agent-steward"],
            "die UIA muss den agent-steward sehen"
        );
        assert_eq!(uia_targets[0].kind, DelegationTargetKind::AgentSteward);

        let root_targets =
            visible_delegation_targets(AgentRoleId::RootOrchestrator, &candidates, &[], 3);
        let root_names: Vec<&str> = root_targets.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(
            root_names,
            vec!["agent-steward"],
            "Root muss den agent-steward sehen"
        );
        assert_eq!(root_targets[0].kind, DelegationTargetKind::AgentSteward);
    }

    /// Ein ChildOrchestrator darf den `agent-steward` weder sehen noch
    /// spawnen (Addendum K: exklusiv UIA und Root vorbehalten).
    #[test]
    fn agent_steward_is_hidden_from_child_orchestrator() {
        let candidates = vec![("agent-steward".to_owned(), AgentRoleId::AgentSteward)];

        let sub_targets =
            visible_delegation_targets(AgentRoleId::ChildOrchestrator, &candidates, &[], 3);
        assert!(
            sub_targets.is_empty(),
            "Sub-Orchestratoren duerfen den agent-steward nicht sehen"
        );
    }

    /// Ein `Worker` sieht den `agent-steward` nicht.
    #[test]
    fn agent_steward_is_hidden_from_worker() {
        let candidates = vec![("agent-steward".to_owned(), AgentRoleId::AgentSteward)];

        let worker_targets = visible_delegation_targets(AgentRoleId::Worker, &candidates, &[], 3);
        assert!(
            worker_targets.is_empty(),
            "ein Worker darf den agent-steward nicht sehen"
        );
    }
}
