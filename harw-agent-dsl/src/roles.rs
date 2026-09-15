//! Geschlossene Rollenmenge und Spawn-Matrix des Harwness Agent Systems (§3 DSL-Spec).
//!
//! Dieses Modul definiert [`AgentRoleId`] als abgeschlossenes Rust-Enum sowie
//! die Funktion [`can_spawn`], die die normative Spawn-Tabelle aus §3 durchsetzt.
//!
//! # Schlüsseltypen
//! - [`AgentRoleId`] — sechs mögliche Rollen (geschlossen, nicht durch TOML erweiterbar)
//!
//! # Invarianten (§3, §22)
//! - Rollen sind durch Rust versiegelt; TOML-Definitionen dürfen keine neuen Rollen erfinden.
//! - Die Spawn-Matrix ist unveränderlich und wird durch die Runtime erzwungen.
//! - Worker können keine dauerhaften Agenten spawnen (nur AgentTools, ein separater Kanal).
//!
//! # Nebenläufigkeit
//! `AgentRoleId` ist `Copy + Send + Sync`.

use serde::{Deserialize, Serialize};

/// Geschlossene Rollenmenge eines Harwness-Agenten (§3).
///
/// # Beschreibung
/// Rollen definieren die Autorität und Position eines Agenten im Orchestrierungsbaum.
/// Sie sind als Rust-Enum versiegelt; TOML-Definitionen können nur eine vorhandene
/// Rolle auswählen, aber keine neue erfinden.
///
/// # Varianten
/// - `UserInterface` — sichtbare Benutzeroberfläche; darf nur Root-Orchestratoren spawnen.
/// - `RootOrchestrator` — Wurzel eines Agenten-Baums; darf Child-Orchestratoren und Worker spawnen.
/// - `ChildOrchestrator` — Untergeordneter Orchestrator; darf Worker spawnen. Weitere Child-Orchestratoren benötigen zusätzlich eine exakte Freigabe aus der Agentendefinition.
/// - `Worker` — Ausführungsworker; darf keine dauerhaften Agenten spawnen.
/// - `UiaWorker` — eigene, vollständig abgekapselte Organisationsrolle des
///   exklusiven Schnellhelfers der UIA (Addendum J, ersetzt den
///   Exklusivitäts-Mechanismus aus Addendum I). Nur `UserInterface` darf
///   `UiaWorker` spawnen; `UiaWorker` selbst darf nichts spawnen.
/// - `AgentSteward` — interner Agent, der das Wissen über Agentendefinitionen
///   explizit besitzt und umsetzt (Addendum K). Sowohl `UserInterface` als
///   auch `RootOrchestrator` dürfen `AgentSteward` spawnen; `AgentSteward`
///   selbst darf nichts spawnen.
///
/// # Beispiele
/// ```rust
/// use harw_agent_dsl::roles::AgentRoleId;
///
/// let role: AgentRoleId = serde_json::from_str("\"worker\"").unwrap();
/// assert_eq!(role, AgentRoleId::Worker);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentRoleId {
    /// Benutzeroberflächen-Agent; einziger Eintrittspunkt für Root-Admissions.
    UserInterface,
    /// Wurzel-Orchestrator; besitzt den Agenten-Baum.
    RootOrchestrator,
    /// Untergeordneter Orchestrator; darf innerhalb der zugewiesenen Tiefe delegieren.
    ChildOrchestrator,
    /// Ausführungsworker; darf keine dauerhaften Agenten spawnen.
    Worker,
    /// Exklusiver Schnellhelfer der UIA (Addendum J); eigene Organisationsrolle,
    /// vollständig abgekapselt vom regulären `Worker`. Nur `UserInterface` darf
    /// sie spawnen; sie selbst darf nichts spawnen.
    UiaWorker,
    /// Interner Agent, der das Wissen über Agentendefinitionen explizit
    /// besitzt und umsetzt (Addendum K). `UserInterface` und
    /// `RootOrchestrator` dürfen ihn spawnen; er selbst darf nichts spawnen.
    AgentSteward,
}

/// Prüft, ob `caller` einen Agenten mit Rolle `target` spawnen darf (§3-Tabelle).
///
/// # Beschreibung
/// Implementiert die normative Spawn-Matrix aus §3 der DSL-Spezifikation.
/// AgentTools sind ein separater Kanal und werden hier nicht berücksichtigt —
/// diese Funktion betrifft ausschließlich das Spawnen dauerhafter Agenten.
///
/// # Argumente
/// - `caller` (`AgentRoleId`): Rolle des spawnen wollenden Agenten.
/// - `target` (`AgentRoleId`): Rolle des zu spawnenden Agenten.
///
/// # Rückgabe
/// `true`, wenn das Spawn erlaubt ist; `false` sonst.
///
/// # Nebenläufigkeit
/// Zustandslos und thread-sicher.
///
/// # Beispiele
/// ```rust
/// use harw_agent_dsl::roles::{AgentRoleId, can_spawn};
///
/// assert!(can_spawn(AgentRoleId::RootOrchestrator, AgentRoleId::Worker));
/// assert!(!can_spawn(AgentRoleId::Worker, AgentRoleId::Worker));
/// ```
pub fn can_spawn(caller: AgentRoleId, target: AgentRoleId) -> bool {
    match (caller, target) {
        // UserInterface → RootOrchestrator (Standardweg) und UiaWorker (eigene,
        // abgekapselte Organisationsrolle, Addendum J).
        (AgentRoleId::UserInterface, AgentRoleId::RootOrchestrator) => true,
        (AgentRoleId::UserInterface, AgentRoleId::UiaWorker) => true,
        (AgentRoleId::UserInterface, AgentRoleId::AgentSteward) => true,
        (AgentRoleId::UserInterface, _) => false,

        // RootOrchestrator → ChildOrchestrator und Worker; nicht sich selbst (Root),
        // nicht UiaWorker (exklusiv der UIA vorbehalten), aber AgentSteward
        // (Addendum K: sowohl UIA als auch Root dürfen ihn spawnen).
        (AgentRoleId::RootOrchestrator, AgentRoleId::ChildOrchestrator) => true,
        (AgentRoleId::RootOrchestrator, AgentRoleId::Worker) => true,
        (AgentRoleId::RootOrchestrator, AgentRoleId::AgentSteward) => true,
        (AgentRoleId::RootOrchestrator, _) => false,

        // ChildOrchestrator → ChildOrchestrator is only the sealed role upper bound;
        // the runtime additionally requires an exact definition-level grant.
        (AgentRoleId::ChildOrchestrator, AgentRoleId::ChildOrchestrator) => true,
        (AgentRoleId::ChildOrchestrator, AgentRoleId::Worker) => true,
        (AgentRoleId::ChildOrchestrator, _) => false,

        // Worker → nichts (nur AgentTools, separater Kanal)
        (AgentRoleId::Worker, _) => false,

        // UiaWorker → nichts (vollständig abgekapselt, keine Spawn-Fläche).
        (AgentRoleId::UiaWorker, _) => false,

        // AgentSteward → nichts (Addendum K: setzt um, spawnt selbst nicht).
        (AgentRoleId::AgentSteward, _) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Überprüft die vollständige 4×4 Spawn-Matrix gegen §3-Tabelle.
    #[test]
    fn test_can_spawn_matrix_matches_doc() {
        let all_roles = [
            AgentRoleId::UserInterface,
            AgentRoleId::RootOrchestrator,
            AgentRoleId::ChildOrchestrator,
            AgentRoleId::Worker,
            AgentRoleId::UiaWorker,
            AgentRoleId::AgentSteward,
        ];

        // UserInterface
        assert!(can_spawn(
            AgentRoleId::UserInterface,
            AgentRoleId::RootOrchestrator
        ));
        assert!(!can_spawn(
            AgentRoleId::UserInterface,
            AgentRoleId::UserInterface
        ));
        assert!(!can_spawn(
            AgentRoleId::UserInterface,
            AgentRoleId::ChildOrchestrator
        ));
        assert!(!can_spawn(AgentRoleId::UserInterface, AgentRoleId::Worker));
        assert!(can_spawn(
            AgentRoleId::UserInterface,
            AgentRoleId::UiaWorker
        ));
        assert!(can_spawn(
            AgentRoleId::UserInterface,
            AgentRoleId::AgentSteward
        ));

        // RootOrchestrator
        assert!(!can_spawn(
            AgentRoleId::RootOrchestrator,
            AgentRoleId::RootOrchestrator
        ));
        assert!(can_spawn(
            AgentRoleId::RootOrchestrator,
            AgentRoleId::ChildOrchestrator
        ));
        assert!(can_spawn(
            AgentRoleId::RootOrchestrator,
            AgentRoleId::Worker
        ));
        assert!(!can_spawn(
            AgentRoleId::RootOrchestrator,
            AgentRoleId::UserInterface
        ));
        assert!(can_spawn(
            AgentRoleId::RootOrchestrator,
            AgentRoleId::AgentSteward
        ));

        // ChildOrchestrator
        assert!(!can_spawn(
            AgentRoleId::ChildOrchestrator,
            AgentRoleId::RootOrchestrator
        ));
        assert!(can_spawn(
            AgentRoleId::ChildOrchestrator,
            AgentRoleId::ChildOrchestrator
        ));
        assert!(can_spawn(
            AgentRoleId::ChildOrchestrator,
            AgentRoleId::Worker
        ));
        assert!(!can_spawn(
            AgentRoleId::ChildOrchestrator,
            AgentRoleId::UserInterface
        ));

        // Worker darf nichts spawnen
        for target in &all_roles {
            assert!(
                !can_spawn(AgentRoleId::Worker, *target),
                "Worker darf {target:?} nicht spawnen"
            );
        }

        // UiaWorker darf nichts spawnen (vollständig abgekapselt, Addendum J)
        for target in &all_roles {
            assert!(
                !can_spawn(AgentRoleId::UiaWorker, *target),
                "UiaWorker darf {target:?} nicht spawnen"
            );
        }

        // AgentSteward darf nichts spawnen (Addendum K)
        for target in &all_roles {
            assert!(
                !can_spawn(AgentRoleId::AgentSteward, *target),
                "AgentSteward darf {target:?} nicht spawnen"
            );
        }

        // Root/Sub-Orchestratoren duerfen den UiaWorker nie spawnen — exklusiv
        // der UIA vorbehalten.
        assert!(!can_spawn(
            AgentRoleId::RootOrchestrator,
            AgentRoleId::UiaWorker
        ));
        assert!(!can_spawn(
            AgentRoleId::ChildOrchestrator,
            AgentRoleId::UiaWorker
        ));

        // ChildOrchestrator darf AgentSteward nicht spawnen (nur UIA/Root, Addendum K).
        assert!(!can_spawn(
            AgentRoleId::ChildOrchestrator,
            AgentRoleId::AgentSteward
        ));
    }

    #[test]
    fn test_serde_kebab_case() {
        let json = serde_json::to_string(&AgentRoleId::RootOrchestrator).unwrap();
        assert_eq!(json, "\"root-orchestrator\"");
        let back: AgentRoleId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, AgentRoleId::RootOrchestrator);
    }

    #[test]
    fn test_worker_role_serde() {
        let json = serde_json::to_string(&AgentRoleId::Worker).unwrap();
        assert_eq!(json, "\"worker\"");
    }

    /// Verankert das Bedrohungsmodell von Knoten AW6-01 (Security-Familie,
    /// `harw-registry-defaults/agents/families/security/security.toml`) an
    /// dieser bereits bestehenden, seit W7 tatsächlich durchgesetzten Regel
    /// (`harw-core/src/child_controller.rs::admit()`).
    ///
    /// Ein künftiger Triage-Agent (AW6-03) läuft mit `role = "worker"` und
    /// liest angreiferkontrollierte Sensorfelder. Selbst wenn ein Angreifer
    /// diesen Agenten vollständig kontrollieren könnte, verhindert diese
    /// Regel — unabhängig von jeder Family-Zugehörigkeit oder jedem
    /// `universe` — dass er einen weiteren dauerhaften Agenten spawnt: die
    /// Spawn-Matrix ist rollenbasiert, nicht familienbasiert, und kennt keine
    /// Ausnahme für "aber diese Rolle gehört zur Security-Familie". Die
    /// `universe`-Disjunktheit (siehe `authority.rs`) schließt den Weg über
    /// *Werkzeuge*; diese Regel schließt den Weg über *weitere Agenten* — die
    /// beiden zusammen sind die vollständige Aussage "ein kompromittierter
    /// Triage-Agent kann so wenig wie ein Worker heute schon kann".
    #[test]
    fn test_worker_cannot_spawn_regardless_of_family_membership() {
        // Ein Worker darf KEINE der vier Rollen spawnen — das gilt
        // unverändert für jede denkbare Family-Zuordnung, weil `can_spawn`
        // ausschließlich auf `AgentRoleId` prüft, nie auf eine Family- oder
        // Universe-Zugehörigkeit.
        for target in [
            AgentRoleId::UserInterface,
            AgentRoleId::RootOrchestrator,
            AgentRoleId::ChildOrchestrator,
            AgentRoleId::Worker,
        ] {
            assert!(
                !can_spawn(AgentRoleId::Worker, target),
                "ein Worker (z. B. ein künftiger Security-Triage-Agent) darf {target:?} nicht spawnen \
                 — unabhaengig davon, welcher Family er angehoert"
            );
        }
    }

    /// Addendum J: die UIA darf ihren `uia-worker` spawnen — eine eigene,
    /// vollständig abgekapselte Organisationsrolle, kein exklusiv gebundener
    /// `Worker` mehr (ersetzt den Exklusivitäts-Mechanismus aus Addendum I).
    #[test]
    fn test_can_spawn_uia_to_uia_worker_ok() {
        assert!(can_spawn(AgentRoleId::UserInterface, AgentRoleId::UiaWorker));
    }

    /// UiaWorker darf von niemandem außer der UIA gespawnt werden.
    #[test]
    fn test_can_spawn_uia_worker_denied_for_non_uia_callers() {
        assert!(!can_spawn(
            AgentRoleId::RootOrchestrator,
            AgentRoleId::UiaWorker
        ));
        assert!(!can_spawn(
            AgentRoleId::ChildOrchestrator,
            AgentRoleId::UiaWorker
        ));
        assert!(!can_spawn(AgentRoleId::Worker, AgentRoleId::UiaWorker));
        assert!(!can_spawn(AgentRoleId::UiaWorker, AgentRoleId::UiaWorker));
    }

    /// Die UIA sieht/spawnt keinen normalen Worker — nur ihren `uia-worker`.
    #[test]
    fn test_can_spawn_uia_to_normal_worker_denied() {
        assert!(!can_spawn(AgentRoleId::UserInterface, AgentRoleId::Worker));
    }

    /// UIA → Worker bleibt in jedem Fall verboten (bleibt auch mit der neuen
    /// Rolle unverändert bestehen, Addendum J).
    #[test]
    fn test_uia_to_worker_stays_denied() {
        assert!(!can_spawn(AgentRoleId::UserInterface, AgentRoleId::Worker));
    }

    /// Addendum K: sowohl die UIA als auch der RootOrchestrator dürfen den
    /// `agent-steward` spawnen.
    #[test]
    fn test_can_spawn_agent_steward_from_uia_and_root_ok() {
        assert!(can_spawn(
            AgentRoleId::UserInterface,
            AgentRoleId::AgentSteward
        ));
        assert!(can_spawn(
            AgentRoleId::RootOrchestrator,
            AgentRoleId::AgentSteward
        ));
    }

    /// AgentSteward darf von niemandem außer UIA und Root gespawnt werden.
    #[test]
    fn test_can_spawn_agent_steward_denied_for_other_callers() {
        assert!(!can_spawn(
            AgentRoleId::ChildOrchestrator,
            AgentRoleId::AgentSteward
        ));
        assert!(!can_spawn(AgentRoleId::Worker, AgentRoleId::AgentSteward));
        assert!(!can_spawn(
            AgentRoleId::UiaWorker,
            AgentRoleId::AgentSteward
        ));
        assert!(!can_spawn(
            AgentRoleId::AgentSteward,
            AgentRoleId::AgentSteward
        ));
    }
}
