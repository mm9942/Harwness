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
//! - [`delegable_in_mode`] — die eine Plan-Modus-Regel (Plan R9, E1): im
//!   Plan-Modus bleiben nur lesende Ziele delegierbar.
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

/// Ab dieser Zahl sichtbarer Ziele (strikt größer) bietet der Turn-Loop statt
/// je eines `transfer_to_<name>` nur noch das eine Werkzeug `agents.delegate`
/// an (Plan R9, Teil C) — jedes `transfer_to_*` kostet rund 300 Tokens.
pub const DELEGATE_TOOL_THRESHOLD: usize = 8;

/// Plan R9, E1: die **eine** Plan-Modus-Regel für Delegationsziele.
///
/// # Beschreibung
/// Im Plan-Modus bleiben nur lesende Ziele delegierbar (sie erben den
/// Plan-Modus); schreibende oder ausführende Ziele erst nach der
/// Planfreigabe. Außerhalb des Plan-Modus ändert die Regel nichts.
/// Dieselbe Funktion filtern `ManagedAgentSpawner` (Sichtbarkeit und
/// Admission), der Turn-Loop (`transfer_to_*`, `agents.delegate`,
/// `agents.catalog`) und damit auch `delegate_wave`.
///
/// # Arguments
/// - `plan_mode`: ob der Aufrufer (selbst oder geerbt) im Plan-Modus ist.
/// - `target_read_only`: ob das Ziel weder schreibt noch Prozesse startet.
#[must_use]
pub const fn delegable_in_mode(plan_mode: bool, target_read_only: bool) -> bool {
    !plan_mode || target_read_only
}

/// Das kebab-case-Label einer Organisationsrolle (wie in der
/// Agentendefinition, z. B. `child-orchestrator`).
#[must_use]
pub const fn role_label(role: AgentRoleId) -> &'static str {
    match role {
        AgentRoleId::UserInterface => "user-interface",
        AgentRoleId::RootOrchestrator => "root-orchestrator",
        AgentRoleId::ChildOrchestrator => "child-orchestrator",
        AgentRoleId::Worker => "worker",
        AgentRoleId::UiaWorker => "uia-worker",
        AgentRoleId::AgentSteward => "agent-steward",
    }
}

/// Die Ablehnung eines schreibenden/ausführenden Ziels im Plan-Modus.
///
/// # Arguments
/// - `read_only_targets`: die im Plan-Modus delegierbaren (lesenden) Ziele
///   des Aufrufers — nur Namen, die er ohnehin sieht.
#[must_use]
pub fn plan_mode_refusal(read_only_targets: &[String]) -> String {
    let listed = if read_only_targets.is_empty() {
        "keines sichtbar".to_owned()
    } else {
        read_only_targets.join(", ")
    };
    format!(
        "{PLAN_MODE_REFUSAL_PREFIX} ({listed}); Schreib-/Ausführungsziele erst nach \
         Planfreigabe."
    )
}

/// Anfang jeder Plan-Modus-Ablehnung ([`plan_mode_refusal`]).
pub const PLAN_MODE_REFUSAL_PREFIX: &str = "Plan-Modus: nur lesende Ziele delegierbar";

/// Ob eine Spawn-Meldung die Plan-Modus-Ablehnung ist — der Turn-Loop gibt
/// sie dem Modell als Werkzeugfehler zurück, statt den Turn abzubrechen.
#[must_use]
pub fn is_plan_mode_refusal(message: &str) -> bool {
    message.contains(PLAN_MODE_REFUSAL_PREFIX)
}

/// Die Ablehnung eines Ziels, das der Aufrufer nicht delegieren darf.
///
/// # Beschreibung
/// Nennt ausschließlich die Ziele, die der Aufrufer ohnehin sieht — kein
/// Orakel über verborgene Agenten. `target` ist der vom Modell genannte Name.
#[must_use]
pub fn not_delegable_message(target: &str, visible: &[String]) -> String {
    let listed = if visible.is_empty() {
        "keine".to_owned()
    } else {
        visible.join(", ")
    };
    format!("Ziel {target} ist für dich nicht delegierbar; delegierbar sind: {listed}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    /// Plan R9: ein benutzerdefinierter Child-Orchestrator (etwa
    /// `intel-analysis-orchestrator`) sieht die Worker seiner Familie
    /// (`evidence-critic`) — dieselbe Spawn-Matrix wie für eingebaute Rollen.
    #[test]
    fn custom_child_orchestrator_sees_its_family_workers() {
        let candidates = vec![
            ("evidence-critic".to_owned(), AgentRoleId::Worker),
            ("synthesis-writer".to_owned(), AgentRoleId::Worker),
        ];
        let targets =
            visible_delegation_targets(AgentRoleId::ChildOrchestrator, &candidates, &[], 1);
        let names: Vec<&str> = targets.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["evidence-critic", "synthesis-writer"]);
    }

    #[test]
    fn plan_mode_keeps_only_read_only_targets() {
        assert!(delegable_in_mode(false, false));
        assert!(delegable_in_mode(false, true));
        assert!(delegable_in_mode(true, true));
        assert!(!delegable_in_mode(true, false));
    }

    #[test]
    fn refusal_messages_name_only_visible_targets() {
        let message = plan_mode_refusal(&["explorer".to_owned(), "planner".to_owned()]);
        assert_eq!(
            message,
            "Plan-Modus: nur lesende Ziele delegierbar (explorer, planner); \
             Schreib-/Ausführungsziele erst nach Planfreigabe."
        );
        assert_eq!(
            not_delegable_message("executor", &["explorer".to_owned()]),
            "Ziel executor ist für dich nicht delegierbar; delegierbar sind: explorer"
        );
        assert!(not_delegable_message("x", &[]).ends_with("delegierbar sind: keine"));
        assert!(is_plan_mode_refusal(&format!(
            "agent spawn failed: {message}"
        )));
        assert!(!is_plan_mode_refusal("no delegation capability"));
    }

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
