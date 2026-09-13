//! Reine Graph-Algorithmen über einen [`Plan`] (`harw-plan`).
//!
//! Verantwortungsbereich: Zustandslose, IO-freie Abfragen und Berechnungen
//! über den Dependency-Graph eines Plans — Bereitschafts- und Blockade-
//! Abfragen, topologische Ausführungswellen inklusive WriteSet-Batch-
//! Aufteilung, Explorations-Vollständigkeitsprüfung sowie Condense-
//! Kandidaten-Erkennung für abgeschlossene Research/Explore-Knoten.
//!
//! Dieses Modul greift weder auf `store`/`file_store` noch auf Systemzeit
//! zu — jeder Zeitwert (`now`) wird vom Aufrufer injiziert. Scope-Kollisionen
//! werden ausschließlich über [`crate::admission::ScopeMatcher::conflicts`]
//! entschieden, damit Schreib- und Lesebereichs-Semantik konsistent mit der
//! Admission-Prüfung bleibt.
//!
//! Wird vom Orchestrator/Controller genutzt, um ausführbare Knoten zu
//! ermitteln, Ausführungswellen für parallele Worker zu planen und
//! Policy-Verstöße (fehlende Exploration) vor einer Zuweisung zu erkennen.
//!
//! # Exportierte Funktionen
//! [`ready_nodes`], [`blocked_by`], [`topological_waves`],
//! [`partition_write_sets`], [`missing_explorations`],
//! [`condense_candidates`], [`children_of`].
//!
//! # Concurrency
//! Alle Funktionen sind rein funktional (`&Plan` → Ergebnis), zustandslos
//! und `Send + Sync`-kompatibel. Keine Locks, keine Systemzeit-Zugriffe,
//! keine I/O.
//!
//! # Fehler
//! [`topological_waves`] liefert `Err(PlanError::CycleDetected)`, wenn der
//! Dependency-Graph der nicht-terminalen Knoten einen Zyklus enthält. Alle
//! übrigen Funktionen sind fehlerfrei und liefern ggf. leere Ergebnisse.

use std::collections::{HashMap, HashSet};

use time::OffsetDateTime;

use crate::admission::ScopeMatcher;
use crate::config::PlanToolConfig;
use crate::error::{PlanError, PlanResult};
use crate::ids::{PathOrSymbol, TaskId};
use crate::types::{EvidenceKind, Plan, PlanNode, PlanNodeKind, PlanNodeStatus};

// ──────────────────────────────────────────────────────────────────────────────
// Interne Hilfsfunktionen
// ──────────────────────────────────────────────────────────────────────────────

/// Findet einen Knoten im Plan anhand seiner ID.
fn find_node<'a>(plan: &'a Plan, id: &TaskId) -> Option<&'a PlanNode> {
    plan.nodes.iter().find(|node| &node.id == id)
}

/// Gibt an, ob ein Status terminal ist (kein weiterer Übergang möglich).
///
/// Terminal-Knoten werden aus der Wellenplanung ausgeschlossen: Sie sind
/// entweder bereits erledigt (`Completed`) oder werden nie mehr ausgeführt
/// (`Superseded`, `Invalidated`).
fn is_terminal(status: PlanNodeStatus) -> bool {
    matches!(
        status,
        PlanNodeStatus::Completed | PlanNodeStatus::Superseded | PlanNodeStatus::Invalidated
    )
}

/// Prüft, ob sich zwei Scope-Listen überschneiden (paarweiser Vergleich).
///
/// Kollisionen werden ausschließlich über [`ScopeMatcher::conflicts`]
/// entschieden (siehe Modulkopf).
fn scopes_overlap(a: &[PathOrSymbol], b: &[PathOrSymbol]) -> bool {
    a.iter().any(|left| {
        b.iter()
            .any(|right| ScopeMatcher::conflicts(left.as_str(), right.as_str()))
    })
}

/// Findet die Wurzel einer Union-Find-Menge (mit Pfadkompression).
fn find_root(parent: &mut [usize], x: usize) -> usize {
    if parent[x] != x {
        parent[x] = find_root(parent, parent[x]);
    }
    parent[x]
}

/// Vereinigt zwei Union-Find-Mengen.
fn union(parent: &mut [usize], a: usize, b: usize) {
    let root_a = find_root(parent, a);
    let root_b = find_root(parent, b);
    if root_a != root_b {
        parent[root_a] = root_b;
    }
}

/// Prüft, ob ein Knoten eine abgeschlossene Explore-Dependency besitzt.
fn has_completed_explore_dependency(plan: &Plan, node: &PlanNode) -> bool {
    node.dependencies.iter().any(|dependency_id| {
        find_node(plan, dependency_id).is_some_and(|dependency| {
            dependency.kind == PlanNodeKind::Explore
                && dependency.status == PlanNodeStatus::Completed
        })
    })
}

/// Prüft, ob ein Knoten ein hinreichend frisches `Finding`-Evidence-Item besitzt.
///
/// "Hinreichend frisch" bedeutet: Alter (`now - attached_at`) ist nicht
/// größer als `ttl_secs`. Ein Alter von 0 oder negativ (Uhrzeit-Ungenauigkeit)
/// gilt ebenfalls als frisch.
fn has_fresh_finding(node: &PlanNode, ttl_secs: u64, now: OffsetDateTime) -> bool {
    let ttl = time::Duration::seconds(i64::try_from(ttl_secs).unwrap_or(i64::MAX));
    node.evidence.iter().any(|evidence| {
        evidence.kind == EvidenceKind::Finding && (now - evidence.attached_at) <= ttl
    })
}

// ──────────────────────────────────────────────────────────────────────────────
// Öffentliche Funktionen
// ──────────────────────────────────────────────────────────────────────────────

/// Knoten, die ausführbar sind: Status `Draft`/`Ready` und alle Dependencies `Completed`.
///
/// # Description
/// Ein Knoten gilt als ausführbar, wenn sein eigener Status `Draft` oder
/// `Ready` ist und jede referenzierte Dependency im Plan existiert und den
/// Status `Completed` trägt. Eine fehlende oder unfertige Dependency
/// blockiert den Knoten — er erscheint dann nicht im Ergebnis (siehe auch
/// [`blocked_by`]).
///
/// # Arguments
/// - `plan` (`&Plan`): der zu untersuchende Plan.
///
/// # Returns
/// Referenzen auf alle aktuell ausführbaren Knoten, in `plan.nodes`-Reihenfolge.
///
/// # Concurrency
/// Rein funktional, keine Seiteneffekte.
pub fn ready_nodes(plan: &Plan) -> Vec<&PlanNode> {
    plan.nodes
        .iter()
        .filter(|node| matches!(node.status, PlanNodeStatus::Draft | PlanNodeStatus::Ready))
        .filter(|node| {
            node.dependencies.iter().all(|dependency_id| {
                find_node(plan, dependency_id)
                    .is_some_and(|dependency| dependency.status == PlanNodeStatus::Completed)
            })
        })
        .collect()
}

/// IDs der Dependencies, die einen Knoten aktuell blockieren (nicht `Completed`).
///
/// # Description
/// Liefert alle `dependencies`-IDs des Knotens `id`, deren referenzierter
/// Knoten entweder im Plan fehlt oder einen von `Completed` verschiedenen
/// Status trägt. Existiert `id` selbst nicht im Plan, ist das Ergebnis leer.
///
/// # Arguments
/// - `plan` (`&Plan`): der zu untersuchende Plan.
/// - `id` (`&TaskId`): der zu prüfende Knoten.
///
/// # Returns
/// Liste blockierender Dependency-IDs, in Deklarationsreihenfolge des Knotens.
///
/// # Concurrency
/// Rein funktional, keine Seiteneffekte.
pub fn blocked_by(plan: &Plan, id: &TaskId) -> Vec<TaskId> {
    let Some(node) = find_node(plan, id) else {
        return Vec::new();
    };

    node.dependencies
        .iter()
        .filter(|dependency_id| {
            !find_node(plan, dependency_id)
                .is_some_and(|dependency| dependency.status == PlanNodeStatus::Completed)
        })
        .cloned()
        .collect()
}

/// Topologische Ausführungswellen für alle nicht-terminalen Knoten.
///
/// # Description
/// Berechnet Ausführungsebenen per Kahn-Algorithmus (iterative
/// Restgrad-Reduktion, keine Rekursion) über den Dependency-Graph: Ebene `n`
/// enthält Knoten, deren sämtliche nicht-terminalen Dependencies bereits in
/// Ebenen `< n` eingeplant wurden. Terminal-Knoten (`Completed`,
/// `Superseded`, `Invalidated`) werden übersprungen — ihre Kanten zählen
/// nicht als offene Abhängigkeit für die verbleibenden Knoten.
///
/// Innerhalb jeder Ebene werden Knoten mit kollidierenden `write_scope`s per
/// [`partition_write_sets`] auf mehrere Folge-Batches aufgeteilt
/// (WriteSet-Disjunktheit, coding-philosophy §8). Jeder Batch bildet einen
/// eigenen Eintrag im Ergebnis; alle Batches einer Ebene stehen vor allen
/// Batches der nächsten Ebene.
///
/// # Arguments
/// - `plan` (`&Plan`): der zu planende Plan.
///
/// # Returns
/// `Ok(batches)`: geordnete Liste von Ausführungsbatches (`Vec<TaskId>`);
/// jeder Batch ist intern write-scope-konfliktfrei.
///
/// # Errors
/// - [`PlanError::CycleDetected`]: wenn nach Erschöpfen aller Knoten mit
///   Restgrad 0 noch nicht-terminale Knoten mit offenen Dependencies
///   verbleiben.
///
/// # Concurrency
/// Rein funktional, keine Seiteneffekte.
pub fn topological_waves(plan: &Plan) -> PlanResult<Vec<Vec<TaskId>>> {
    let active: Vec<&PlanNode> = plan
        .nodes
        .iter()
        .filter(|node| !is_terminal(node.status))
        .collect();

    let active_ids: HashSet<&TaskId> = active.iter().map(|node| &node.id).collect();

    let mut remaining: HashMap<&TaskId, usize> = HashMap::new();
    let mut dependents: HashMap<&TaskId, Vec<&TaskId>> = HashMap::new();

    for node in &active {
        let open_dependencies = node
            .dependencies
            .iter()
            .filter(|dependency_id| active_ids.contains(dependency_id))
            .count();
        remaining.insert(&node.id, open_dependencies);

        for dependency_id in &node.dependencies {
            if active_ids.contains(dependency_id) {
                dependents.entry(dependency_id).or_default().push(&node.id);
            }
        }
    }

    let mut waves: Vec<Vec<TaskId>> = Vec::new();
    let mut scheduled: HashSet<&TaskId> = HashSet::new();

    while scheduled.len() < active.len() {
        let level: Vec<&PlanNode> = active
            .iter()
            .copied()
            .filter(|node| {
                !scheduled.contains(&node.id) && remaining.get(&node.id).copied() == Some(0)
            })
            .collect();

        if level.is_empty() {
            let Some(stuck) = active
                .iter()
                .copied()
                .find(|node| !scheduled.contains(&node.id))
            else {
                // Unerreichbar: `while scheduled.len() < active.len()` garantiert,
                // dass mindestens ein Knoten noch nicht eingeplant ist.
                unreachable!(
                    "Schleifeninvariante verletzt: scheduled.len() < active.len() \
                     erfordert einen verbleibenden Knoten"
                );
            };

            let blocking_dependency = stuck
                .dependencies
                .iter()
                .find(|dependency_id| {
                    active_ids.contains(dependency_id) && !scheduled.contains(dependency_id)
                })
                .cloned()
                .unwrap_or_else(|| stuck.id.clone());

            return Err(PlanError::CycleDetected {
                child: stuck.id.clone(),
                parent: blocking_dependency,
            });
        }

        for batch in partition_write_sets(&level) {
            waves.push(batch);
        }

        for node in &level {
            scheduled.insert(&node.id);
            if let Some(children) = dependents.get(&node.id) {
                for child_id in children {
                    if let Some(count) = remaining.get_mut(child_id) {
                        *count -= 1;
                    }
                }
            }
        }
    }

    Ok(waves)
}

/// Zerlegt eine Knotenmenge in Gruppen paarweise disjunkter `write_scope`s.
///
/// # Description
/// Greedy-Bin-Packing: Knoten werden in der gegebenen Reihenfolge
/// verarbeitet; jeder Knoten wird der ersten bestehenden Gruppe zugeordnet,
/// in der keines ihrer Mitglieder mit seinem `write_scope` kollidiert
/// ([`ScopeMatcher::conflicts`]). Findet sich keine kollisionsfreie Gruppe,
/// wird eine neue Gruppe eröffnet.
///
/// # Arguments
/// - `nodes` (`&[&PlanNode]`): die zu partitionierende Knotenmenge.
///
/// # Returns
/// Gruppen von Knoten-IDs; jede Gruppe ist intern konfliktfrei bezüglich
/// `write_scope`, in Zuordnungsreihenfolge.
///
/// # Concurrency
/// Rein funktional, keine Seiteneffekte.
pub fn partition_write_sets(nodes: &[&PlanNode]) -> Vec<Vec<TaskId>> {
    let mut groups: Vec<Vec<&PlanNode>> = Vec::new();

    for node in nodes {
        let target_group = groups.iter_mut().find(|group| {
            !group
                .iter()
                .any(|member| scopes_overlap(&member.write_scope, &node.write_scope))
        });

        match target_group {
            Some(group) => group.push(*node),
            None => groups.push(vec![*node]),
        }
    }

    groups
        .into_iter()
        .map(|group| group.into_iter().map(|node| node.id.clone()).collect())
        .collect()
}

/// Knoten, deren Art laut Konfiguration eine frische Exploration verlangt,
/// die aber weder aufweisen.
///
/// # Description
/// Liefert alle Knoten, deren `kind` in `cfg.require_exploration_for`
/// vorkommt, die aber weder eine `Completed`-Dependency vom Typ
/// [`PlanNodeKind::Explore`] noch ein Evidence-Item der Art
/// [`EvidenceKind::Finding`] besitzen, das nicht älter als
/// `cfg.exploration_ttl_secs` ist. `now` wird injiziert — dieses Modul
/// greift nicht auf Systemzeit zu.
///
/// # Arguments
/// - `plan` (`&Plan`): der zu prüfende Plan.
/// - `cfg` (`&PlanToolConfig`): liefert `require_exploration_for` und
///   `exploration_ttl_secs`.
/// - `now` (`OffsetDateTime`): der Referenzzeitpunkt für die TTL-Prüfung.
///
/// # Returns
/// IDs aller Knoten mit fehlender oder veralteter Exploration, in
/// `plan.nodes`-Reihenfolge.
///
/// # Concurrency
/// Rein funktional, keine Seiteneffekte, keine Systemzeit-Zugriffe.
pub fn missing_explorations(
    plan: &Plan,
    cfg: &PlanToolConfig,
    now: OffsetDateTime,
) -> Vec<TaskId> {
    plan.nodes
        .iter()
        .filter(|node| cfg.require_exploration_for.contains(&node.kind))
        .filter(|node| !has_completed_explore_dependency(plan, node))
        .filter(|node| !has_fresh_finding(node, cfg.exploration_ttl_secs, now))
        .map(|node| node.id.clone())
        .collect()
}

/// Gruppen abgeschlossener Research/Explore-Knoten mit überlappendem `read_scope`.
///
/// # Description
/// Bildet über alle `Completed`-Knoten vom Typ [`PlanNodeKind::Research`]
/// oder [`PlanNodeKind::Explore`] die transitive Hülle der
/// `read_scope`-Überlappung (Union-Find über [`ScopeMatcher::conflicts`])
/// und liefert nur Gruppen mit mindestens `min_group` Mitgliedern zurück —
/// Kandidaten für eine Verdichtung zu einem Contract-Knoten.
///
/// # Arguments
/// - `plan` (`&Plan`): der zu untersuchende Plan.
/// - `min_group` (`usize`): Mindestgröße einer Gruppe, damit sie als
///   Kandidat zurückgegeben wird.
///
/// # Returns
/// Gruppen von Knoten-IDs, deren `read_scope`s paarweise (transitiv)
/// überlappen und deren Größe `>= min_group` ist. Reihenfolge der Gruppen
/// ist nicht garantiert.
///
/// # Concurrency
/// Rein funktional, keine Seiteneffekte.
pub fn condense_candidates(plan: &Plan, min_group: usize) -> Vec<Vec<TaskId>> {
    let candidates: Vec<&PlanNode> = plan
        .nodes
        .iter()
        .filter(|node| {
            node.status == PlanNodeStatus::Completed
                && matches!(node.kind, PlanNodeKind::Research | PlanNodeKind::Explore)
        })
        .collect();

    let mut parent: Vec<usize> = (0..candidates.len()).collect();

    for i in 0..candidates.len() {
        for j in (i + 1)..candidates.len() {
            if scopes_overlap(&candidates[i].read_scope, &candidates[j].read_scope) {
                union(&mut parent, i, j);
            }
        }
    }

    let mut groups: HashMap<usize, Vec<TaskId>> = HashMap::new();
    for (index, node) in candidates.iter().enumerate() {
        let root = find_root(&mut parent, index);
        groups.entry(root).or_default().push(node.id.clone());
    }

    groups
        .into_values()
        .filter(|group| group.len() >= min_group)
        .collect()
}

/// Kinder eines Composite-Knotens (`parent == id`).
///
/// # Description
/// Liefert alle Knoten, deren `parent`-Feld auf `id` zeigt — die direkten
/// Kinder eines [`PlanNodeKind::Composite`]-Knotens.
///
/// # Arguments
/// - `plan` (`&Plan`): der zu durchsuchende Plan.
/// - `id` (`&TaskId`): die ID des potenziellen Composite-Elternknotens.
///
/// # Returns
/// Referenzen auf alle Kinder, in `plan.nodes`-Reihenfolge.
///
/// # Concurrency
/// Rein funktional, keine Seiteneffekte.
pub fn children_of<'a>(plan: &'a Plan, id: &TaskId) -> Vec<&'a PlanNode> {
    plan.nodes
        .iter()
        .filter(|node| node.parent.as_ref() == Some(id))
        .collect()
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::{PathOrSymbol, PlanId, RevisionId};

    fn make_plan(nodes: Vec<PlanNode>) -> Plan {
        Plan {
            id: PlanId::new("p-graph-test"),
            revision: RevisionId::new(1),
            parent_revision: None,
            goal_statement: "graph test".to_owned(),
            goal_id: None,
            nodes,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn make_node(id: &str, status: PlanNodeStatus, kind: PlanNodeKind) -> PlanNode {
        PlanNode {
            id: TaskId::new(id),
            objective: "test objective".to_owned(),
            dependencies: Vec::new(),
            input_contracts: Vec::new(),
            output_contracts: Vec::new(),
            read_scope: Vec::new(),
            write_scope: vec![PathOrSymbol::new(format!("src/{id}.rs"))],
            forbidden_scope: Vec::new(),
            acceptance_criteria: Vec::new(),
            invalidation_conditions: Vec::new(),
            status,
            evidence: Vec::new(),
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
            kind,
            wave: None,
            assignment: None,
            parent: None,
        }
    }

    // ── (e) ready_nodes überspringt Knoten mit unfertiger Dependency ────────

    #[test]
    fn test_ready_nodes_skips_node_with_unfinished_dependency() {
        let dependency = make_node("dep", PlanNodeStatus::Draft, PlanNodeKind::Coding);
        let mut child = make_node("child", PlanNodeStatus::Draft, PlanNodeKind::Coding);
        child.dependencies = vec![TaskId::new("dep")];
        let plan = make_plan(vec![dependency, child]);

        let ready: Vec<TaskId> = ready_nodes(&plan)
            .into_iter()
            .map(|node| node.id.clone())
            .collect();

        assert!(
            ready.contains(&TaskId::new("dep")),
            "dep hat keine Dependencies und muss ausführbar sein"
        );
        assert!(
            !ready.contains(&TaskId::new("child")),
            "child mit unfertiger Dependency darf nicht ausführbar sein"
        );
    }

    #[test]
    fn test_blocked_by_returns_only_unfinished_dependencies() {
        let dependency_done =
            make_node("dep-done", PlanNodeStatus::Completed, PlanNodeKind::Coding);
        let dependency_open =
            make_node("dep-open", PlanNodeStatus::InProgress, PlanNodeKind::Coding);
        let mut child = make_node("child", PlanNodeStatus::Draft, PlanNodeKind::Coding);
        child.dependencies = vec![TaskId::new("dep-done"), TaskId::new("dep-open")];
        let plan = make_plan(vec![dependency_done, dependency_open, child]);

        let blocked = blocked_by(&plan, &TaskId::new("child"));

        assert_eq!(blocked, vec![TaskId::new("dep-open")]);
    }

    // ── (a) Kette a→b→c ergibt 3 Wellen ──────────────────────────────────────

    #[test]
    fn test_topological_waves_chain_yields_three_waves() {
        let a = make_node("a", PlanNodeStatus::Draft, PlanNodeKind::Coding);
        let mut b = make_node("b", PlanNodeStatus::Draft, PlanNodeKind::Coding);
        b.dependencies = vec![TaskId::new("a")];
        let mut c = make_node("c", PlanNodeStatus::Draft, PlanNodeKind::Coding);
        c.dependencies = vec![TaskId::new("b")];

        let plan = make_plan(vec![a, b, c]);
        let waves = topological_waves(&plan).expect("Kette darf keinen Zyklus enthalten");

        assert_eq!(waves.len(), 3, "Kette a→b→c muss genau 3 Wellen ergeben");
        assert_eq!(waves[0], vec![TaskId::new("a")]);
        assert_eq!(waves[1], vec![TaskId::new("b")]);
        assert_eq!(waves[2], vec![TaskId::new("c")]);
    }

    // ── (b) zwei unabhängige Knoten mit disjunktem write_scope ──────────────

    #[test]
    fn test_topological_waves_independent_disjoint_scopes_single_batch() {
        let a = make_node("a", PlanNodeStatus::Draft, PlanNodeKind::Coding);
        let b = make_node("b", PlanNodeStatus::Draft, PlanNodeKind::Coding);
        let plan = make_plan(vec![a, b]);

        let waves = topological_waves(&plan).expect("keine Abhängigkeiten, kein Zyklus");

        assert_eq!(
            waves.len(),
            1,
            "disjunkte write_scopes müssen in einer Welle mit einem Batch landen"
        );
        assert_eq!(waves[0].len(), 2);
    }

    // ── (c) zwei unabhängige Knoten mit kollidierendem write_scope ──────────

    #[test]
    fn test_topological_waves_independent_colliding_scopes_two_batches() {
        let mut a = make_node("a", PlanNodeStatus::Draft, PlanNodeKind::Coding);
        a.write_scope = vec![PathOrSymbol::new("src/shared.rs")];
        let mut b = make_node("b", PlanNodeStatus::Draft, PlanNodeKind::Coding);
        b.write_scope = vec![PathOrSymbol::new("src/shared.rs")];
        let plan = make_plan(vec![a, b]);

        let waves = topological_waves(&plan).expect("keine Abhängigkeiten, kein Zyklus");

        assert_eq!(
            waves.len(),
            2,
            "kollidierende write_scopes müssen auf zwei Batches aufgeteilt werden"
        );
        assert_eq!(waves[0].len(), 1);
        assert_eq!(waves[1].len(), 1);
    }

    // ── (d) Zyklus → Err ─────────────────────────────────────────────────────

    #[test]
    fn test_topological_waves_cycle_returns_err() {
        let mut a = make_node("a", PlanNodeStatus::Draft, PlanNodeKind::Coding);
        a.dependencies = vec![TaskId::new("b")];
        let mut b = make_node("b", PlanNodeStatus::Draft, PlanNodeKind::Coding);
        b.dependencies = vec![TaskId::new("a")];
        let plan = make_plan(vec![a, b]);

        assert!(matches!(
            topological_waves(&plan),
            Err(PlanError::CycleDetected { .. })
        ));
    }

    // ── partition_write_sets (Bonus-Test) ────────────────────────────────────

    #[test]
    fn test_partition_write_sets_splits_colliding_nodes() {
        let mut a = make_node("a", PlanNodeStatus::Draft, PlanNodeKind::Coding);
        a.write_scope = vec![PathOrSymbol::new("src/shared.rs")];
        let mut b = make_node("b", PlanNodeStatus::Draft, PlanNodeKind::Coding);
        b.write_scope = vec![PathOrSymbol::new("src/shared.rs")];
        let mut c = make_node("c", PlanNodeStatus::Draft, PlanNodeKind::Coding);
        c.write_scope = vec![PathOrSymbol::new("src/other.rs")];

        let nodes = [&a, &b, &c];
        let groups = partition_write_sets(&nodes);

        assert_eq!(groups.len(), 2, "a und b kollidieren, c ist konfliktfrei");
    }

    // ── (f) missing_explorations findet Coding-Knoten ohne Explore-Dep ──────

    #[test]
    fn test_missing_explorations_finds_coding_node_without_explore_dependency() {
        let coding_node = make_node("impl-1", PlanNodeStatus::Draft, PlanNodeKind::Coding);
        let plan = make_plan(vec![coding_node]);
        let cfg = PlanToolConfig {
            require_exploration_for: vec![PlanNodeKind::Coding],
            exploration_ttl_secs: 3600,
            ..PlanToolConfig::enabled_defaults()
        };

        let missing = missing_explorations(&plan, &cfg, OffsetDateTime::UNIX_EPOCH);

        assert_eq!(missing, vec![TaskId::new("impl-1")]);
    }

    #[test]
    fn test_missing_explorations_ignores_node_with_completed_explore_dependency() {
        let explore = make_node("explore-1", PlanNodeStatus::Completed, PlanNodeKind::Explore);
        let mut coding_node = make_node("impl-1", PlanNodeStatus::Draft, PlanNodeKind::Coding);
        coding_node.dependencies = vec![TaskId::new("explore-1")];
        let plan = make_plan(vec![explore, coding_node]);
        let cfg = PlanToolConfig {
            require_exploration_for: vec![PlanNodeKind::Coding],
            exploration_ttl_secs: 3600,
            ..PlanToolConfig::enabled_defaults()
        };

        let missing = missing_explorations(&plan, &cfg, OffsetDateTime::UNIX_EPOCH);

        assert!(
            missing.is_empty(),
            "abgeschlossene Explore-Dependency deckt die Anforderung ab"
        );
    }

    // ── condense_candidates (Bonus-Test) ─────────────────────────────────────

    #[test]
    fn test_condense_candidates_groups_overlapping_completed_explorations() {
        let mut r1 = make_node("r1", PlanNodeStatus::Completed, PlanNodeKind::Research);
        r1.read_scope = vec![PathOrSymbol::new("src/module")];
        let mut r2 = make_node("r2", PlanNodeStatus::Completed, PlanNodeKind::Explore);
        r2.read_scope = vec![PathOrSymbol::new("src/module/sub.rs")];
        let mut isolated = make_node("r3", PlanNodeStatus::Completed, PlanNodeKind::Research);
        isolated.read_scope = vec![PathOrSymbol::new("docs/unrelated.md")];

        let plan = make_plan(vec![r1, r2, isolated]);

        let groups = condense_candidates(&plan, 2);

        assert_eq!(
            groups.len(),
            1,
            "nur die überlappende Gruppe erreicht min_group=2"
        );
        assert_eq!(groups[0].len(), 2);
    }

    // ── (g) children_of ───────────────────────────────────────────────────

    #[test]
    fn test_children_of_returns_composite_children() {
        let parent_node = make_node("parent-1", PlanNodeStatus::Draft, PlanNodeKind::Composite);
        let mut child_a = make_node("child-a", PlanNodeStatus::Draft, PlanNodeKind::Coding);
        child_a.parent = Some(TaskId::new("parent-1"));
        let mut child_b = make_node("child-b", PlanNodeStatus::Draft, PlanNodeKind::Coding);
        child_b.parent = Some(TaskId::new("parent-1"));
        let unrelated = make_node("other", PlanNodeStatus::Draft, PlanNodeKind::Coding);

        let plan = make_plan(vec![parent_node, child_a, child_b, unrelated]);

        let children: Vec<TaskId> = children_of(&plan, &TaskId::new("parent-1"))
            .into_iter()
            .map(|node| node.id.clone())
            .collect();

        assert_eq!(children.len(), 2);
        assert!(children.contains(&TaskId::new("child-a")));
        assert!(children.contains(&TaskId::new("child-b")));
    }
}
