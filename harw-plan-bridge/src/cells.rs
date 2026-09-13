//! Deklaratives Fan-out: von der Zell-Definition zur Ausführungswelle.
//!
//! # Verantwortungsbereich
//! Eine Zelle (`[[cells]]` in der Agent-DSL) beschreibt *deklarativ*, welche
//! Plan-Knoten gemeinsam bearbeitet werden, wie ihre Schreibbereiche getrennt
//! werden müssen und wann die Welle als beendet gilt. [`CellPlan`] löst diese
//! Beschreibung gegen einen konkreten [`Plan`] auf und erzeugt daraus die
//! [`FanoutRequest`]s, mit denen der Orchestrator seine Kinder startet.
//!
//! # Auswahl der Mitglieder
//! `members_from_plan` ist ein Glob, der gegen **zwei** Dinge gehalten wird:
//! die `TaskId` des Knotens und jeden Eintrag seines `write_scope`. Ein Muster
//! wie `harw-tui/**` wählt damit alle Knoten, die in dieses Verzeichnis
//! schreiben, auch wenn ihre IDs anders lauten. Gehört die Zelle zu einem Clan,
//! wird die Auswahl zusätzlich auf dessen `plan_scope` eingeschränkt — ein Clan
//! kann seiner Zelle keinen Knoten außerhalb seines eigenen Reviers geben.
//!
//! # Batches
//! Bei `write_partition = Required` werden die Mitglieder über
//! `harw_plan::graph::partition_write_sets` in Gruppen zerlegt, deren
//! Schreibbereiche sich paarweise nicht überschneiden. Innerhalb einer Gruppe
//! darf parallel gearbeitet werden; zwischen den Gruppen nicht. Bei `Advisory`
//! und `None` gibt es genau einen Batch — die Trennung ist dann eine Bitte,
//! keine Grenze.
//!
//! # Exportierte Typen
//! [`CellPlan`].
//!
//! # Concurrency
//! [`CellPlan`] ist ein reiner Werttyp (`Send + Sync`); `from_cell` und
//! `fanout_requests` machen keine I/O.
//!
//! # Fehler
//! [`PlanBridgeError::CellMemberPattern`], wenn `members_from_plan` leer ist
//! oder nur aus Leerzeichen besteht — ein Muster, das nichts aussagt, darf
//! nicht stillschweigend "alles" oder "nichts" bedeuten.

use std::collections::HashMap;

use harw_agent_dsl::organization::{CellBarrier, CellWritePartition, RawCellSpec, RawClanSpec};
use harw_core::child_controller::{AgentBudget, FanoutRequest, JoinSemantics};
use harw_core::turn_loop::TurnInput;
use harw_plan::graph;
use harw_plan::{Plan, PlanNode, ScopeMatcher, TaskId};
use harw_types::SessionId;

use crate::error::PlanBridgeError;

/// Eine aufgelöste Zelle: Mitglieder, Batches und Join-Semantik.
///
/// # Description
/// Das Ergebnis von [`CellPlan::from_cell`] — die Übersetzung einer
/// deklarativen Zell-Definition in eine konkrete Ausführungswelle über einem
/// gegebenen Plan.
///
/// # Concurrency
/// Reiner Werttyp, `Clone + Send + Sync`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellPlan {
    /// Bezeichner der Zelle aus der Agent-DSL.
    pub cell_id: String,
    /// Alle ausgewählten Mitglieder, in `plan.nodes`-Reihenfolge.
    pub members: Vec<TaskId>,
    /// Die Ausführungsbatches; bei `write_partition = Required` paarweise
    /// schreibkonfliktfrei, sonst genau ein Batch mit allen Mitgliedern.
    pub batches: Vec<Vec<TaskId>>,
    /// Wie der Orchestrator auf die Kinder wartet.
    pub join: JoinSemantics,
    /// Rolle, unter der die Kinder laufen: die Clan-ID, sonst die Zell-ID.
    pub role: String,
}

impl CellPlan {
    /// Löst eine Zell-Definition gegen einen Plan auf.
    ///
    /// # Description
    /// Wählt die Mitglieder über `cell.members_from_plan` (Glob gegen `TaskId`
    /// **und** `write_scope`-Einträge via
    /// [`ScopeMatcher::matches_glob`]), beschränkt sie auf `clan.plan_scope`,
    /// teilt sie bei `write_partition = Required` über
    /// `graph::partition_write_sets` in Batches und übersetzt den
    /// [`CellBarrier`] in [`JoinSemantics`]:
    ///
    /// | `CellBarrier`  | `JoinSemantics` | Bedeutung                                    |
    /// |----------------|-----------------|----------------------------------------------|
    /// | `AllTerminal`  | `AllTerminal`   | auf alle Kinder warten                        |
    /// | `AnyTerminal`  | `AnyTerminal`   | beim ersten Ergebnis abbrechen                |
    /// | `ExplicitJoin` | `Collect`       | alles sammeln, der Orchestrator joint selbst  |
    ///
    /// [`CellKind`](harw_agent_dsl::organization::CellKind) wird hier bewusst
    /// nicht ausgewertet: er beschreibt die *Startform* (fan-out, Barrier,
    /// sequenziell), die der Orchestrator aus den Batches ableitet — eine
    /// sequenzielle Zelle ist eine mit Batches der Größe eins, und diese
    /// Entscheidung gehört dem Aufrufer, nicht dieser Auflösung.
    ///
    /// # Arguments
    /// - `cell` (`&RawCellSpec`): die aufzulösende Zell-Definition.
    /// - `clan` (`Option<&RawClanSpec>`): der besitzende Clan, falls die Zelle
    ///   einem zugeordnet ist. Sein `plan_scope` schränkt die Auswahl ein.
    /// - `plan` (`&Plan`): der Plan, gegen den aufgelöst wird.
    ///
    /// # Returns
    /// Den aufgelösten [`CellPlan`]. Findet das Muster keinen Knoten, sind
    /// `members` und `batches` leer — das ist kein Fehler, sondern eine Welle
    /// ohne Arbeit.
    ///
    /// # Errors
    /// - [`PlanBridgeError::CellMemberPattern`]: wenn `members_from_plan` leer
    ///   ist oder nur aus Leerzeichen besteht.
    ///
    /// # Concurrency
    /// Rein funktional, keine Seiteneffekte.
    pub fn from_cell(
        cell: &RawCellSpec,
        clan: Option<&RawClanSpec>,
        plan: &Plan,
    ) -> Result<Self, PlanBridgeError> {
        let pattern = cell.members_from_plan.trim();
        if pattern.is_empty() {
            return Err(PlanBridgeError::CellMemberPattern {
                pattern: cell.members_from_plan.clone(),
            });
        }

        let clan_scope = clan
            .map(|clan| clan.plan_scope.trim())
            .filter(|scope| !scope.is_empty());

        let members: Vec<&PlanNode> = plan
            .nodes
            .iter()
            .filter(|node| node_matches(node, pattern))
            .filter(|node| clan_scope.is_none_or(|scope| node_matches(node, scope)))
            .collect();

        let member_ids: Vec<TaskId> = members.iter().map(|node| node.id.clone()).collect();

        let batches = match cell.write_partition {
            CellWritePartition::Required => graph::partition_write_sets(&members),
            CellWritePartition::Advisory | CellWritePartition::None => {
                if member_ids.is_empty() {
                    Vec::new()
                } else {
                    vec![member_ids.clone()]
                }
            }
        };

        let plan_cell = Self {
            cell_id: cell.id.clone(),
            members: member_ids,
            batches,
            join: join_semantics(cell.barrier),
            role: clan
                .map(|clan| clan.id.clone())
                .unwrap_or_else(|| cell.id.clone()),
        };

        tracing::debug!(
            cell = plan_cell.cell_id.as_str(),
            role = plan_cell.role.as_str(),
            members = plan_cell.members.len(),
            batches = plan_cell.batches.len(),
            "Zelle gegen Plan aufgelöst"
        );
        Ok(plan_cell)
    }

    /// Baut die Fan-out-Anforderungen für bereits admittierte Kinder.
    ///
    /// # Description
    /// Der Aufrufer hat die Kinder bereits über
    /// `ManagedAgentSpawner::spawn_child` admittiert und übergibt hier die
    /// Zuordnung `TaskId -> (SessionId, TurnInput)`. Diese Funktion trägt
    /// **keine** Admission nach — ein `FanoutRequest` für ein nicht
    /// admittiertes Kind wäre eine Umgehung des Kind-Controllers.
    ///
    /// Die Reihenfolge folgt den [`Self::batches`], nicht der übergebenen
    /// Zuordnung: die Ausgabe ist damit deterministisch, auch wenn `resolved`
    /// eine `HashMap` ist. Mitglieder ohne Eintrag in `resolved` werden
    /// übersprungen und protokolliert.
    ///
    /// # Arguments
    /// - `resolved` (`&HashMap<TaskId, (SessionId, TurnInput)>`): die
    ///   admittierten Kinder je Knoten.
    /// - `budget` (`AgentBudget`): der pro Kind geltende Deckel.
    ///
    /// # Returns
    /// Die Fan-out-Anforderungen in Batch- und Mitgliederreihenfolge.
    ///
    /// # Concurrency
    /// Rein funktional, keine Seiteneffekte.
    #[must_use]
    pub fn fanout_requests(
        &self,
        resolved: &HashMap<TaskId, (SessionId, TurnInput)>,
        budget: AgentBudget,
    ) -> Vec<FanoutRequest> {
        self.batches
            .iter()
            .flatten()
            .filter_map(|task| match resolved.get(task) {
                Some(entry) => Some(entry),
                None => {
                    tracing::warn!(
                        cell = self.cell_id.as_str(),
                        task = %task,
                        "Zell-Mitglied ohne admittiertes Kind — übersprungen"
                    );
                    None
                }
            })
            .map(|(child, input)| FanoutRequest {
                child: child.clone(),
                input: input.clone(),
                budget,
            })
            .collect()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Interne Hilfsfunktionen
// ──────────────────────────────────────────────────────────────────────────────

/// Prüft, ob ein Knoten auf ein Zell-/Clan-Muster passt.
///
/// Getroffen wird über die `TaskId` **oder** über einen Eintrag des
/// `write_scope` — ein Muster darf sowohl Knoten benennen als auch Reviere.
fn node_matches(node: &PlanNode, pattern: &str) -> bool {
    if ScopeMatcher::matches_glob(pattern, node.id.as_str()) {
        return true;
    }
    node.write_scope
        .iter()
        .any(|scope| ScopeMatcher::matches_glob(pattern, scope.as_str()))
}

/// Übersetzt die deklarative Barriere in die Join-Semantik des Controllers.
fn join_semantics(barrier: CellBarrier) -> JoinSemantics {
    match barrier {
        CellBarrier::AllTerminal => JoinSemantics::AllTerminal,
        CellBarrier::AnyTerminal => JoinSemantics::AnyTerminal,
        // Ein expliziter Join heißt: die Runtime bricht nichts ab und sammelt
        // auch fehlgeschlagene Ergebnisse — der Orchestrator entscheidet.
        CellBarrier::ExplicitJoin => JoinSemantics::Collect,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{coding_node, plan_with};
    use harw_agent_dsl::organization::CellKind;
    use harw_plan::{PathOrSymbol, PlanNodeStatus};

    fn cell(
        pattern: &str,
        write_partition: CellWritePartition,
        barrier: CellBarrier,
    ) -> RawCellSpec {
        RawCellSpec {
            id: "cell-1".to_owned(),
            clan: "clan-1".to_owned(),
            kind: CellKind::Fanout,
            barrier,
            write_partition,
            members_from_plan: pattern.to_owned(),
        }
    }

    /// Baut eine minimale Definitionsreferenz für Clan-Fixtures.
    fn definition_ref(name: &str) -> harw_agent_dsl::ids::DefinitionRef {
        harw_agent_dsl::ids::DefinitionRef {
            id: harw_agent_dsl::ids::DefinitionId {
                namespace: "test".to_owned(),
                kind: "agent".to_owned(),
                name: name.to_owned(),
                major: 1,
            },
            version: None,
        }
    }

    fn node_writing(id: &str, paths: &[&str]) -> PlanNode {
        let mut node = coding_node(id, PlanNodeStatus::Ready);
        node.write_scope = paths.iter().map(|path| PathOrSymbol::new(*path)).collect();
        node
    }

    #[test]
    fn test_from_cell_selects_members_by_task_id_glob() {
        let plan = plan_with(vec![
            node_writing("tui-1", &["harw-tui/src/a.rs"]),
            node_writing("cli-1", &["harw-cli/src/b.rs"]),
            node_writing("tui-2", &["harw-tui/src/c.rs"]),
        ]);

        let resolved = match CellPlan::from_cell(
            &cell("tui-*", CellWritePartition::None, CellBarrier::AllTerminal),
            None,
            &plan,
        ) {
            Ok(resolved) => resolved,
            Err(error) => panic!("from_cell schlug fehl: {error}"),
        };

        assert_eq!(
            resolved.members,
            vec![TaskId::new("tui-1"), TaskId::new("tui-2")]
        );
        assert_eq!(resolved.batches, vec![resolved.members.clone()]);
        assert_eq!(resolved.role, "cell-1");
    }

    #[test]
    fn test_from_cell_selects_members_by_write_scope_glob() {
        let plan = plan_with(vec![
            node_writing("t-1", &["harw-tui/src/a.rs"]),
            node_writing("t-2", &["harw-cli/src/b.rs"]),
        ]);

        let resolved = match CellPlan::from_cell(
            &cell(
                "harw-tui/**",
                CellWritePartition::None,
                CellBarrier::AllTerminal,
            ),
            None,
            &plan,
        ) {
            Ok(resolved) => resolved,
            Err(error) => panic!("from_cell schlug fehl: {error}"),
        };

        assert_eq!(resolved.members, vec![TaskId::new("t-1")]);
    }

    #[test]
    fn test_clan_plan_scope_narrows_the_selection() {
        let plan = plan_with(vec![
            node_writing("t-1", &["harw-tui/src/a.rs"]),
            node_writing("t-2", &["harw-cli/src/b.rs"]),
        ]);
        let clan = RawClanSpec {
            id: "clan-tui".to_owned(),
            name: "TUI".to_owned(),
            leader: definition_ref("leader"),
            family: definition_ref("family"),
            plan_scope: "harw-tui/**".to_owned(),
            child_depth_cost: 1,
        };

        let resolved = match CellPlan::from_cell(
            &cell("t-*", CellWritePartition::None, CellBarrier::AllTerminal),
            Some(&clan),
            &plan,
        ) {
            Ok(resolved) => resolved,
            Err(error) => panic!("from_cell schlug fehl: {error}"),
        };

        assert_eq!(resolved.members, vec![TaskId::new("t-1")]);
        assert_eq!(resolved.role, "clan-tui");
    }

    #[test]
    fn test_required_partition_splits_conflicting_write_scopes_into_batches() {
        let plan = plan_with(vec![
            node_writing("t-1", &["src/shared.rs"]),
            node_writing("t-2", &["src/shared.rs"]),
            node_writing("t-3", &["src/other.rs"]),
        ]);

        let resolved = match CellPlan::from_cell(
            &cell(
                "t-*",
                CellWritePartition::Required,
                CellBarrier::AllTerminal,
            ),
            None,
            &plan,
        ) {
            Ok(resolved) => resolved,
            Err(error) => panic!("from_cell schlug fehl: {error}"),
        };

        assert_eq!(resolved.members.len(), 3);
        assert_eq!(resolved.batches.len(), 2, "Batches: {:?}", resolved.batches);

        // Jedes Mitglied kommt genau einmal vor …
        let mut flattened: Vec<&str> = resolved
            .batches
            .iter()
            .flatten()
            .map(TaskId::as_str)
            .collect();
        flattened.sort_unstable();
        assert_eq!(flattened, vec!["t-1", "t-2", "t-3"]);

        // … und t-1 und t-2 (gleicher Schreibpfad) liegen nicht zusammen.
        let together = resolved.batches.iter().any(|batch| {
            batch.contains(&TaskId::new("t-1")) && batch.contains(&TaskId::new("t-2"))
        });
        assert!(!together, "kollidierende Knoten im selben Batch");
    }

    #[test]
    fn test_advisory_partition_keeps_everything_in_one_batch() {
        let plan = plan_with(vec![
            node_writing("t-1", &["src/shared.rs"]),
            node_writing("t-2", &["src/shared.rs"]),
        ]);

        let resolved = match CellPlan::from_cell(
            &cell(
                "t-*",
                CellWritePartition::Advisory,
                CellBarrier::AllTerminal,
            ),
            None,
            &plan,
        ) {
            Ok(resolved) => resolved,
            Err(error) => panic!("from_cell schlug fehl: {error}"),
        };

        assert_eq!(resolved.batches.len(), 1);
        assert_eq!(resolved.batches[0].len(), 2);
    }

    #[test]
    fn test_barrier_maps_to_join_semantics() {
        let plan = plan_with(vec![node_writing("t-1", &["src/a.rs"])]);
        let cases = [
            (CellBarrier::AllTerminal, JoinSemantics::AllTerminal),
            (CellBarrier::AnyTerminal, JoinSemantics::AnyTerminal),
            (CellBarrier::ExplicitJoin, JoinSemantics::Collect),
        ];

        for (barrier, expected) in cases {
            let resolved = match CellPlan::from_cell(
                &cell("t-*", CellWritePartition::None, barrier),
                None,
                &plan,
            ) {
                Ok(resolved) => resolved,
                Err(error) => panic!("from_cell schlug fehl: {error}"),
            };
            assert_eq!(resolved.join, expected, "Barriere {barrier:?}");
        }
    }

    #[test]
    fn test_blank_member_pattern_is_rejected() {
        let plan = plan_with(vec![node_writing("t-1", &["src/a.rs"])]);

        match CellPlan::from_cell(
            &cell("   ", CellWritePartition::None, CellBarrier::AllTerminal),
            None,
            &plan,
        ) {
            Err(PlanBridgeError::CellMemberPattern { pattern }) => {
                assert_eq!(pattern, "   ");
            }
            other => panic!("erwartet CellMemberPattern, bekommen: {other:?}"),
        }
    }

    #[test]
    fn test_pattern_without_matches_yields_an_empty_wave() {
        let plan = plan_with(vec![node_writing("t-1", &["src/a.rs"])]);

        let resolved = match CellPlan::from_cell(
            &cell(
                "nichts-*",
                CellWritePartition::Required,
                CellBarrier::AllTerminal,
            ),
            None,
            &plan,
        ) {
            Ok(resolved) => resolved,
            Err(error) => panic!("from_cell schlug fehl: {error}"),
        };

        assert!(resolved.members.is_empty());
        assert!(resolved.batches.is_empty());
    }

    #[test]
    fn test_fanout_requests_follow_batch_order_and_skip_unresolved_members() {
        let plan = plan_with(vec![
            node_writing("t-1", &["src/a.rs"]),
            node_writing("t-2", &["src/b.rs"]),
        ]);
        let resolved_cell = match CellPlan::from_cell(
            &cell("t-*", CellWritePartition::None, CellBarrier::AllTerminal),
            None,
            &plan,
        ) {
            Ok(resolved) => resolved,
            Err(error) => panic!("from_cell schlug fehl: {error}"),
        };

        // Nur t-1 wurde admittiert.
        let child = SessionId::new();
        let mut children: HashMap<TaskId, (SessionId, TurnInput)> = HashMap::new();
        children.insert(
            TaskId::new("t-1"),
            (child.clone(), TurnInput::user("arbeite an t-1")),
        );

        let requests = resolved_cell.fanout_requests(&children, AgentBudget::default());

        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].child, child);
        assert_eq!(
            requests[0].input.user_text.as_deref(),
            Some("arbeite an t-1")
        );
    }

    #[test]
    fn test_fanout_requests_are_deterministic() {
        let plan = plan_with(vec![
            node_writing("t-1", &["src/a.rs"]),
            node_writing("t-2", &["src/b.rs"]),
            node_writing("t-3", &["src/c.rs"]),
        ]);
        let resolved_cell = match CellPlan::from_cell(
            &cell(
                "t-*",
                CellWritePartition::Required,
                CellBarrier::AllTerminal,
            ),
            None,
            &plan,
        ) {
            Ok(resolved) => resolved,
            Err(error) => panic!("from_cell schlug fehl: {error}"),
        };

        let mut children: HashMap<TaskId, (SessionId, TurnInput)> = HashMap::new();
        for id in ["t-1", "t-2", "t-3"] {
            children.insert(
                TaskId::new(id),
                (
                    SessionId::new(),
                    TurnInput::user(format!("arbeite an {id}")),
                ),
            );
        }

        let first = resolved_cell.fanout_requests(&children, AgentBudget::default());
        let second = resolved_cell.fanout_requests(&children, AgentBudget::default());

        let ids = |requests: &[FanoutRequest]| {
            requests
                .iter()
                .map(|request| request.child.as_str().to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&first), ids(&second));
        assert_eq!(first.len(), 3);
    }
}
