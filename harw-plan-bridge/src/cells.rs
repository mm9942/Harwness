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
//! # Ausführung (Zell-Executor, Plan Punkt 1)
//! [`CellPlan`] sagt, *welche* Knoten in *welchen* Batches laufen; der
//! Zell-Executor sagt, *wie*. [`CellSchedule`] übersetzt die Batches je nach
//! [`CellKind`] in Stufen:
//!
//! | `CellKind`   | Stufen                          | Parallelität je Stufe       |
//! |--------------|---------------------------------|-----------------------------|
//! | `Fanout`     | ein Batch = eine Stufe          | `min(Batchgröße, max_parallel)` |
//! | `Sequential` | ein Mitglied = eine Stufe       | `1`                         |
//! | `Barrier`    | ein Batch = eine Stufe, mit Tor | `min(Batchgröße, max_parallel)` |
//!
//! Zwischen Stufen wird nie parallel gearbeitet — genau das garantiert die
//! Schreibtrennung der Batches. [`run_cell`] fährt die Stufen über einen
//! vom Aufrufer gelieferten Stufen-Läufer (z. B. `delegate_wave` bzw.
//! `fanout_children` in `harw-core-bridge`) und wendet die
//! [`JoinSemantics`] über die Stufen hinweg an:
//!
//! - `AnyTerminal`: das erste abgeschlossene Mitglied gewinnt; alle noch
//!   nicht gestarteten Stufen werden übersprungen ([`SkipReason::WinnerFound`]).
//! - `AllTerminal`/`Collect`: alle Stufen laufen. Bei `CellKind::Barrier` ist
//!   jede Stufe zusätzlich ein Tor: erfüllt sie ihren Join nicht
//!   (`AllTerminal`: nicht alle Mitglieder abgeschlossen), werden die
//!   folgenden Stufen übersprungen ([`SkipReason::BarrierNotReached`]).
//!   `Collect` öffnet jedes Tor — der Orchestrator joint selbst.
//!
//! [`resolve_wave`] verallgemeinert die Wellenauflösung von `/analyze`
//! (`harw-ops/src/analyze.rs`, `plan_wave`/`cell_plan_for_wave`/
//! `wave_batches`) auf beliebige Elementtypen: Zelle auflösen, Batches auf die
//! Elemente zurückbilden und bei fehlender Zelle, leerer Auswahl oder
//! unvollständiger Abdeckung auf die ungeteilte Welle zurückfallen.
//!
//! # Exportierte Typen
//! [`CellPlan`], [`CellSchedule`], [`CellStage`], [`CellRun`],
//! [`MemberOutcome`], [`SkipReason`], [`ResolvedWave`]; Funktionen
//! [`run_cell`], [`resolve_wave`], [`batches_for_items`].
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
use std::future::Future;

use harw_agent_dsl::organization::{
    CellBarrier, CellKind, CellWritePartition, RawCellSpec, RawClanSpec,
};
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
        Self::from_cell_nodes(cell, clan, &plan.nodes)
    }

    /// Wie [`Self::from_cell`], aber direkt über eine Knotenliste statt über
    /// einen vollständigen [`Plan`].
    ///
    /// # Description
    /// Die Auswahl liest ausschließlich `id` und `write_scope` der Knoten —
    /// ein flüchtiger Aufrufer (etwa eine Analyse-Welle oder ein
    /// `delegate_wave`-Fan-out) muss dafür keinen Plan mit Revision und
    /// Zeitstempeln bauen.
    ///
    /// # Arguments
    /// - `cell` (`&RawCellSpec`): die aufzulösende Zell-Definition.
    /// - `clan` (`Option<&RawClanSpec>`): der besitzende Clan.
    /// - `nodes` (`&[PlanNode]`): die Kandidaten in Ausführungsreihenfolge.
    ///
    /// # Errors
    /// Wie [`Self::from_cell`].
    ///
    /// # Concurrency
    /// Rein funktional, keine Seiteneffekte.
    pub fn from_cell_nodes(
        cell: &RawCellSpec,
        clan: Option<&RawClanSpec>,
        nodes: &[PlanNode],
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

        let members: Vec<&PlanNode> = nodes
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
// Zell-Executor (Plan Punkt 1)
// ──────────────────────────────────────────────────────────────────────────────

impl CellPlan {
    /// Übersetzt die Batches dieser Zelle in einen Ausführungsplan.
    ///
    /// # Arguments
    /// - `kind` ([`CellKind`]): die Startform aus der Zell-Definition
    ///   (`RawCellSpec::kind`) — [`CellPlan`] trägt sie bewusst nicht selbst.
    /// - `max_parallel` (`usize`): Obergrenze gleichzeitig laufender
    ///   Mitglieder einer Stufe (`0` wird zu `1`).
    ///
    /// # Returns
    /// Den [`CellSchedule`] über die `TaskId`s dieser Zelle.
    ///
    /// # Concurrency
    /// Rein funktional.
    #[must_use]
    pub fn schedule(&self, kind: CellKind, max_parallel: usize) -> CellSchedule<TaskId> {
        CellSchedule::from_batches(
            self.cell_id.clone(),
            kind,
            self.join,
            self.batches.clone(),
            max_parallel,
        )
    }
}

/// Eine Stufe eines Zell-Ausführungsplans: Mitglieder, die gemeinsam laufen
/// dürfen, und deren Parallelitätsdeckel.
///
/// # Concurrency
/// Reiner Werttyp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellStage<T> {
    /// Mitglieder dieser Stufe in Ausführungsreihenfolge.
    pub members: Vec<T>,
    /// Höchstzahl gleichzeitig laufender Mitglieder (immer `≥ 1`).
    pub max_parallel: usize,
}

/// Der Ausführungsplan einer Zelle: geordnete Stufen plus Join-Semantik.
///
/// # Description
/// Siehe die Tabelle in der Moduldokumentation. Stufen laufen **nie**
/// gleichzeitig; innerhalb einer Stufe höchstens `max_parallel` Mitglieder.
///
/// # Concurrency
/// Reiner Werttyp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellSchedule<T> {
    /// Bezeichner der Zelle (für Protokoll und Ausgabe).
    pub cell_id: String,
    /// Startform der Zelle.
    pub kind: CellKind,
    /// Join-Semantik über die gesamte Zelle.
    pub join: JoinSemantics,
    /// Die Stufen in Ausführungsreihenfolge; nie eine leere Stufe.
    pub stages: Vec<CellStage<T>>,
}

impl<T> CellSchedule<T> {
    /// Baut einen Ausführungsplan aus bereits schreibgetrennten Batches.
    ///
    /// # Arguments
    /// - `cell_id` (`String`): Bezeichner der Zelle.
    /// - `kind` ([`CellKind`]): Startform.
    /// - `join` ([`JoinSemantics`]): Join über die Zelle.
    /// - `batches` (`Vec<Vec<T>>`): paarweise schreibkonfliktfreie Batches in
    ///   Reihenfolge; leere Batches werden verworfen.
    /// - `max_parallel` (`usize`): Deckel je Stufe (`0` wird zu `1`).
    ///
    /// # Returns
    /// `Fanout`/`Barrier`: eine Stufe je nicht-leerem Batch mit
    /// `min(Batchgröße, max_parallel)`; `Sequential`: eine Stufe je Mitglied
    /// mit Parallelität `1`.
    ///
    /// # Concurrency
    /// Rein funktional.
    #[must_use]
    pub fn from_batches(
        cell_id: String,
        kind: CellKind,
        join: JoinSemantics,
        batches: Vec<Vec<T>>,
        max_parallel: usize,
    ) -> Self {
        let cap = max_parallel.max(1);
        let mut stages: Vec<CellStage<T>> = Vec::new();
        for batch in batches.into_iter().filter(|batch| !batch.is_empty()) {
            match kind {
                CellKind::Fanout | CellKind::Barrier => {
                    let max_parallel = batch.len().min(cap);
                    stages.push(CellStage {
                        members: batch,
                        max_parallel,
                    });
                }
                CellKind::Sequential => {
                    for member in batch {
                        stages.push(CellStage {
                            members: vec![member],
                            max_parallel: 1,
                        });
                    }
                }
            }
        }
        Self {
            cell_id,
            kind,
            join,
            stages,
        }
    }

    /// Alle Mitglieder in Ausführungsreihenfolge.
    pub fn members(&self) -> impl Iterator<Item = &T> {
        self.stages.iter().flat_map(|stage| stage.members.iter())
    }
}

/// Warum ein Mitglied nicht gestartet wurde.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// `AnyTerminal`: ein früheres Mitglied hat bereits abgeschlossen.
    WinnerFound,
    /// `CellKind::Barrier`: eine frühere Stufe hat ihr Tor nicht erreicht.
    BarrierNotReached,
}

impl SkipReason {
    /// Stabiles, maschinenlesbares Label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WinnerFound => "winner_found",
            Self::BarrierNotReached => "barrier_not_reached",
        }
    }
}

/// Ergebnis eines einzelnen Zell-Mitglieds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemberOutcome<V> {
    /// Das Mitglied lieferte ein verwertbares Ergebnis.
    Completed(V),
    /// Das Mitglied lief, lieferte aber kein verwertbares Ergebnis.
    Failed(String),
    /// Das Mitglied wurde nie gestartet.
    Skipped(SkipReason),
}

impl<V> MemberOutcome<V> {
    /// `true` für [`MemberOutcome::Completed`].
    #[must_use]
    pub fn is_completed(&self) -> bool {
        matches!(self, Self::Completed(_))
    }
}

/// Das Ergebnis einer vollständig gefahrenen Zelle.
///
/// # Concurrency
/// Reiner Werttyp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellRun<T, V> {
    /// Bezeichner der Zelle.
    pub cell_id: String,
    /// Die angewandte Join-Semantik.
    pub join: JoinSemantics,
    /// Je Mitglied sein Ergebnis, in Ausführungsreihenfolge.
    pub results: Vec<(T, MemberOutcome<V>)>,
}

impl<T, V> CellRun<T, V> {
    /// Ob die Zelle ihren Join erfüllt hat.
    ///
    /// # Returns
    /// - `AllTerminal`: jedes Mitglied ist abgeschlossen.
    /// - `AnyTerminal`: mindestens ein Mitglied ist abgeschlossen.
    /// - `Collect`: kein Mitglied wurde übersprungen (alle liefen bis zum
    ///   Ende, gleich mit welchem Ausgang).
    #[must_use]
    pub fn is_satisfied(&self) -> bool {
        match self.join {
            JoinSemantics::AllTerminal => self
                .results
                .iter()
                .all(|(_, outcome)| outcome.is_completed()),
            JoinSemantics::AnyTerminal => self
                .results
                .iter()
                .any(|(_, outcome)| outcome.is_completed()),
            JoinSemantics::Collect => self
                .results
                .iter()
                .all(|(_, outcome)| !matches!(outcome, MemberOutcome::Skipped(_))),
        }
    }

    /// Anzahl abgeschlossener Mitglieder.
    #[must_use]
    pub fn completed(&self) -> usize {
        self.results
            .iter()
            .filter(|(_, outcome)| outcome.is_completed())
            .count()
    }
}

/// Meldung für ein Mitglied, zu dem der Stufen-Läufer kein Ergebnis lieferte.
const MISSING_STAGE_RESULT: &str = "der Stufen-Läufer lieferte für dieses Mitglied kein Ergebnis";

/// Fährt einen Zell-Ausführungsplan Stufe für Stufe.
///
/// # Description
/// Ruft `run_stage` genau einmal je Stufe, in Reihenfolge, und wartet die
/// Stufe vollständig ab, bevor die nächste beginnt (Schreibtrennung). Der
/// Läufer bekommt die Stufe und die Join-Semantik, die **innerhalb** der Stufe
/// gilt (bei `AnyTerminal` darf er Geschwister abbrechen, sobald eines
/// abschließt); er liefert positionsgleich zu `stage.members` je Mitglied
/// `Ok(wert)` oder `Err(meldung)`. Fehlende Positionen werden zu
/// [`MemberOutcome::Failed`], überzählige verworfen.
///
/// Über die Stufen hinweg gelten die Regeln der Moduldokumentation
/// (Gewinner bei `AnyTerminal`, Tor bei `CellKind::Barrier`).
///
/// # Arguments
/// - `schedule` ([`CellSchedule<T>`]): der Ausführungsplan.
/// - `run_stage` (`FnMut(CellStage<T>, JoinSemantics) -> Fut`): der
///   Stufen-Läufer, z. B. ein `delegate_wave`- oder `fanout_children`-Aufruf.
///
/// # Returns
/// Das [`CellRun`] mit genau einem Eintrag je Mitglied des Plans.
///
/// # Concurrency
/// Startet selbst keine Tasks; Nebenläufigkeit entsteht ausschließlich im
/// Läufer. Das zurückgegebene Future ist `Send`, wenn `T`, `V`, `F` und `Fut`
/// es sind.
///
/// # Examples
/// ```rust,no_run
/// use harw_agent_dsl::organization::CellKind;
/// use harw_core::child_controller::JoinSemantics;
/// use harw_plan_bridge::cells::{CellSchedule, run_cell};
///
/// # async fn demo() {
/// let schedule = CellSchedule::from_batches(
///     "wave".to_owned(),
///     CellKind::Fanout,
///     JoinSemantics::AllTerminal,
///     vec![vec!["a", "b"], vec!["c"]],
///     4,
/// );
/// let run = run_cell(schedule, |stage, _join| async move {
///     stage
///         .members
///         .iter()
///         .map(|member| Ok::<String, String>(format!("{member} erledigt")))
///         .collect()
/// })
/// .await;
/// assert!(run.is_satisfied());
/// # }
/// ```
pub async fn run_cell<T, V, F, Fut>(schedule: CellSchedule<T>, mut run_stage: F) -> CellRun<T, V>
where
    T: Clone,
    F: FnMut(CellStage<T>, JoinSemantics) -> Fut,
    Fut: Future<Output = Vec<Result<V, String>>>,
{
    let CellSchedule {
        cell_id,
        kind,
        join,
        stages,
    } = schedule;
    let mut results: Vec<(T, MemberOutcome<V>)> = Vec::new();
    let mut skip: Option<SkipReason> = None;

    for stage in stages {
        if let Some(reason) = skip {
            results.extend(
                stage
                    .members
                    .into_iter()
                    .map(|member| (member, MemberOutcome::Skipped(reason))),
            );
            continue;
        }

        let members = stage.members.clone();
        let mut outcomes = run_stage(stage, join).await.into_iter();
        let mut stage_completed = 0_usize;
        let stage_size = members.len();
        for member in members {
            let outcome = match outcomes.next() {
                Some(Ok(value)) => {
                    stage_completed += 1;
                    MemberOutcome::Completed(value)
                }
                Some(Err(message)) => MemberOutcome::Failed(message),
                None => MemberOutcome::Failed(MISSING_STAGE_RESULT.to_owned()),
            };
            results.push((member, outcome));
        }

        skip = next_skip(kind, join, stage_completed, stage_size);
        tracing::debug!(
            cell = cell_id.as_str(),
            stage_size,
            stage_completed,
            skip = skip.map(SkipReason::as_str),
            "cell.stage.complete"
        );
    }

    CellRun {
        cell_id,
        join,
        results,
    }
}

/// Entscheidet nach einer Stufe, ob die folgenden übersprungen werden.
fn next_skip(
    kind: CellKind,
    join: JoinSemantics,
    stage_completed: usize,
    stage_size: usize,
) -> Option<SkipReason> {
    if join == JoinSemantics::AnyTerminal && stage_completed > 0 {
        return Some(SkipReason::WinnerFound);
    }
    if kind != CellKind::Barrier {
        return None;
    }
    let gate_open = match join {
        JoinSemantics::AllTerminal => stage_completed == stage_size,
        JoinSemantics::AnyTerminal => stage_completed > 0,
        JoinSemantics::Collect => true,
    };
    if gate_open {
        None
    } else {
        Some(SkipReason::BarrierNotReached)
    }
}

/// Die aufgelöste Welle über beliebigen Elementen (verallgemeinert aus
/// `/analyze`).
///
/// # Concurrency
/// Reiner Werttyp über geliehenen Elementen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWave<'a, T> {
    /// Die Batches in Ausführungsreihenfolge; im Rückfall genau ein Batch mit
    /// allen Elementen.
    pub batches: Vec<Vec<&'a T>>,
    /// Join-Semantik der Zelle; im Rückfall [`JoinSemantics::AllTerminal`].
    pub join: JoinSemantics,
    /// Startform der Zelle; im Rückfall [`CellKind::Fanout`].
    pub kind: CellKind,
    /// Bezeichner der tatsächlich verwendeten Zelle; `None` im Rückfall.
    pub cell_id: Option<String>,
}

impl<'a, T> ResolvedWave<'a, T> {
    /// Übersetzt die Welle in einen Ausführungsplan (siehe
    /// [`CellSchedule::from_batches`]).
    #[must_use]
    pub fn schedule(&self, max_parallel: usize) -> CellSchedule<&'a T> {
        CellSchedule::from_batches(
            self.cell_id.clone().unwrap_or_default(),
            self.kind,
            self.join,
            self.batches.clone(),
            max_parallel,
        )
    }
}

/// Löst die Zelle einer Welle über beliebigen Elementen auf.
///
/// # Description
/// Verallgemeinerung von `plan_wave`/`cell_plan_for_wave` aus
/// `harw-ops/src/analyze.rs`: `node_for` bildet jedes Element auf einen
/// flüchtigen [`PlanNode`] ab (nur `id` und `write_scope` zählen), die Zelle
/// wird über [`CellPlan::from_cell_nodes`] aufgelöst und ihre Batches über
/// [`batches_for_items`] auf die Elemente zurückgebildet. Jede Stufe fällt
/// einzeln auf die ungeteilte Welle zurück — ohne Zelle, bei einem
/// Auflösungsfehler, bei leerer Auswahl oder wenn die Zelle nicht **jedes**
/// Element abdeckt (eine zu enge Zelle darf nie still Arbeit verschlucken).
///
/// # Arguments
/// - `cell` (`Option<(&RawClanSpec, &RawCellSpec)>`): Clan und Zelle aus der
///   Organisation.
/// - `items` (`&[&T]`): die Elemente dieser Welle in Ausführungsreihenfolge.
/// - `node_for` (`Fn(&T) -> PlanNode`): Abbildung Element → Plan-Knoten; die
///   `TaskId`s müssen eindeutig sein.
///
/// # Returns
/// Die [`ResolvedWave`].
///
/// # Concurrency
/// Rein funktional (bis auf `tracing`).
pub fn resolve_wave<'a, T>(
    cell: Option<(&RawClanSpec, &RawCellSpec)>,
    items: &[&'a T],
    node_for: impl Fn(&T) -> PlanNode,
) -> ResolvedWave<'a, T> {
    let fallback = || ResolvedWave {
        batches: if items.is_empty() {
            Vec::new()
        } else {
            vec![items.to_vec()]
        },
        join: JoinSemantics::AllTerminal,
        kind: CellKind::Fanout,
        cell_id: None,
    };
    let Some((clan, spec)) = cell else {
        return fallback();
    };
    let nodes: Vec<PlanNode> = items.iter().map(|item| node_for(*item)).collect();
    let resolved = match CellPlan::from_cell_nodes(spec, Some(clan), &nodes) {
        Ok(resolved) if !resolved.members.is_empty() => resolved,
        Ok(resolved) => {
            tracing::warn!(
                cell = resolved.cell_id.as_str(),
                clan = clan.id.as_str(),
                items = items.len(),
                "cell.wave.no_members"
            );
            return fallback();
        }
        Err(error) => {
            tracing::warn!(
                cell = spec.id.as_str(),
                clan = clan.id.as_str(),
                error = %error,
                "cell.wave.unresolved"
            );
            return fallback();
        }
    };
    let keyed: Vec<(TaskId, &'a T)> = nodes
        .into_iter()
        .zip(items.iter().copied())
        .map(|(node, item)| (node.id, item))
        .collect();
    let Some(batches) = batches_for_keyed(&resolved, &keyed) else {
        return fallback();
    };
    ResolvedWave {
        batches,
        join: resolved.join,
        kind: spec.kind,
        cell_id: Some(resolved.cell_id),
    }
}

/// Bildet die Batches einer aufgelösten Zelle auf Elemente zurück
/// (verallgemeinert aus `wave_batches` in `harw-ops/src/analyze.rs`).
///
/// # Arguments
/// - `cell` (`Option<&CellPlan>`): die aufgelöste Zelle; `None` ergibt die
///   ungeteilte Welle.
/// - `items` (`&[&T]`): die Elemente in Ausführungsreihenfolge.
/// - `task_of` (`Fn(&T) -> TaskId`): die `TaskId`, unter der ein Element in
///   der Zelle steht.
///
/// # Returns
/// Die Batches in Ausführungsreihenfolge; deckt die Zelle nicht **jedes**
/// Element ab (oder gibt es keine Zelle), genau ein Batch mit allen
/// Elementen — bzw. keiner, wenn `items` leer ist.
///
/// # Concurrency
/// Rein funktional.
pub fn batches_for_items<'a, T>(
    cell: Option<&CellPlan>,
    items: &[&'a T],
    task_of: impl Fn(&T) -> TaskId,
) -> Vec<Vec<&'a T>> {
    let undivided = || {
        if items.is_empty() {
            Vec::new()
        } else {
            vec![items.to_vec()]
        }
    };
    let Some(cell) = cell else {
        return undivided();
    };
    let keyed: Vec<(TaskId, &'a T)> = items.iter().map(|item| (task_of(*item), *item)).collect();
    batches_for_keyed(cell, &keyed).unwrap_or_else(undivided)
}

/// Kern von [`batches_for_items`]/[`resolve_wave`]: `None` bei
/// unvollständiger Abdeckung oder ohne ein einziges zugeordnetes Element.
fn batches_for_keyed<'a, T>(cell: &CellPlan, keyed: &[(TaskId, &'a T)]) -> Option<Vec<Vec<&'a T>>> {
    let mut batches: Vec<Vec<&'a T>> = Vec::with_capacity(cell.batches.len());
    let mut covered = 0_usize;
    for batch in &cell.batches {
        let mut members: Vec<&'a T> = Vec::with_capacity(batch.len());
        for task in batch {
            match keyed.iter().find(|(id, _)| id == task) {
                Some((_, item)) => {
                    members.push(*item);
                    covered += 1;
                }
                None => tracing::warn!(
                    cell = cell.cell_id.as_str(),
                    task = task.as_str(),
                    "cell.batch.member_without_item"
                ),
            }
        }
        if !members.is_empty() {
            batches.push(members);
        }
    }
    if batches.is_empty() || covered != keyed.len() {
        tracing::warn!(
            cell = cell.cell_id.as_str(),
            covered,
            expected = keyed.len(),
            "cell.batch.partial_cover — Rückfall auf die ungeteilte Welle"
        );
        return None;
    }
    Some(batches)
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
    use crate::test_support::{TestError, TestResult, ctx};
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
    fn test_from_cell_selects_members_by_task_id_glob() -> TestResult {
        let plan = plan_with(vec![
            node_writing("tui-1", &["harw-tui/src/a.rs"]),
            node_writing("cli-1", &["harw-cli/src/b.rs"]),
            node_writing("tui-2", &["harw-tui/src/c.rs"]),
        ])?;

        let resolved = CellPlan::from_cell(
            &cell("tui-*", CellWritePartition::None, CellBarrier::AllTerminal),
            None,
            &plan,
        )
        .map_err(ctx("from_cell schlug fehl"))?;

        assert_eq!(
            resolved.members,
            vec![TaskId::new("tui-1"), TaskId::new("tui-2")]
        );
        assert_eq!(resolved.batches, vec![resolved.members.clone()]);
        assert_eq!(resolved.role, "cell-1");
        Ok(())
    }

    #[test]
    fn test_from_cell_selects_members_by_write_scope_glob() -> TestResult {
        let plan = plan_with(vec![
            node_writing("t-1", &["harw-tui/src/a.rs"]),
            node_writing("t-2", &["harw-cli/src/b.rs"]),
        ])?;

        let resolved = CellPlan::from_cell(
            &cell(
                "harw-tui/**",
                CellWritePartition::None,
                CellBarrier::AllTerminal,
            ),
            None,
            &plan,
        )
        .map_err(ctx("from_cell schlug fehl"))?;

        assert_eq!(resolved.members, vec![TaskId::new("t-1")]);
        Ok(())
    }

    #[test]
    fn test_clan_plan_scope_narrows_the_selection() -> TestResult {
        let plan = plan_with(vec![
            node_writing("t-1", &["harw-tui/src/a.rs"]),
            node_writing("t-2", &["harw-cli/src/b.rs"]),
        ])?;
        let clan = RawClanSpec {
            id: "clan-tui".to_owned(),
            name: "TUI".to_owned(),
            leader: definition_ref("leader"),
            family: definition_ref("family"),
            plan_scope: "harw-tui/**".to_owned(),
            child_depth_cost: 1,
        };

        let resolved = CellPlan::from_cell(
            &cell("t-*", CellWritePartition::None, CellBarrier::AllTerminal),
            Some(&clan),
            &plan,
        )
        .map_err(ctx("from_cell schlug fehl"))?;

        assert_eq!(resolved.members, vec![TaskId::new("t-1")]);
        assert_eq!(resolved.role, "clan-tui");
        Ok(())
    }

    #[test]
    fn test_required_partition_splits_conflicting_write_scopes_into_batches() -> TestResult {
        let plan = plan_with(vec![
            node_writing("t-1", &["src/shared.rs"]),
            node_writing("t-2", &["src/shared.rs"]),
            node_writing("t-3", &["src/other.rs"]),
        ])?;

        let resolved = CellPlan::from_cell(
            &cell(
                "t-*",
                CellWritePartition::Required,
                CellBarrier::AllTerminal,
            ),
            None,
            &plan,
        )
        .map_err(ctx("from_cell schlug fehl"))?;

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
        Ok(())
    }

    #[test]
    fn test_advisory_partition_keeps_everything_in_one_batch() -> TestResult {
        let plan = plan_with(vec![
            node_writing("t-1", &["src/shared.rs"]),
            node_writing("t-2", &["src/shared.rs"]),
        ])?;

        let resolved = CellPlan::from_cell(
            &cell(
                "t-*",
                CellWritePartition::Advisory,
                CellBarrier::AllTerminal,
            ),
            None,
            &plan,
        )
        .map_err(ctx("from_cell schlug fehl"))?;

        assert_eq!(resolved.batches.len(), 1);
        assert_eq!(resolved.batches[0].len(), 2);
        Ok(())
    }

    #[test]
    fn test_barrier_maps_to_join_semantics() -> TestResult {
        let plan = plan_with(vec![node_writing("t-1", &["src/a.rs"])])?;
        let cases = [
            (CellBarrier::AllTerminal, JoinSemantics::AllTerminal),
            (CellBarrier::AnyTerminal, JoinSemantics::AnyTerminal),
            (CellBarrier::ExplicitJoin, JoinSemantics::Collect),
        ];

        for (barrier, expected) in cases {
            let resolved =
                CellPlan::from_cell(&cell("t-*", CellWritePartition::None, barrier), None, &plan)
                    .map_err(ctx("from_cell schlug fehl"))?;
            assert_eq!(resolved.join, expected, "Barriere {barrier:?}");
        }
        Ok(())
    }

    #[test]
    fn test_blank_member_pattern_is_rejected() -> TestResult {
        let plan = plan_with(vec![node_writing("t-1", &["src/a.rs"])])?;

        match CellPlan::from_cell(
            &cell("   ", CellWritePartition::None, CellBarrier::AllTerminal),
            None,
            &plan,
        ) {
            Err(PlanBridgeError::CellMemberPattern { pattern }) => {
                assert_eq!(pattern, "   ");
                Ok(())
            }
            other => Err(TestError::Unexpected(format!(
                "erwartet CellMemberPattern, bekommen: {other:?}"
            ))),
        }
    }

    #[test]
    fn test_pattern_without_matches_yields_an_empty_wave() -> TestResult {
        let plan = plan_with(vec![node_writing("t-1", &["src/a.rs"])])?;

        let resolved = CellPlan::from_cell(
            &cell(
                "nichts-*",
                CellWritePartition::Required,
                CellBarrier::AllTerminal,
            ),
            None,
            &plan,
        )
        .map_err(ctx("from_cell schlug fehl"))?;

        assert!(resolved.members.is_empty());
        assert!(resolved.batches.is_empty());
        Ok(())
    }

    #[test]
    fn test_fanout_requests_follow_batch_order_and_skip_unresolved_members() -> TestResult {
        let plan = plan_with(vec![
            node_writing("t-1", &["src/a.rs"]),
            node_writing("t-2", &["src/b.rs"]),
        ])?;
        let resolved_cell = CellPlan::from_cell(
            &cell("t-*", CellWritePartition::None, CellBarrier::AllTerminal),
            None,
            &plan,
        )
        .map_err(ctx("from_cell schlug fehl"))?;

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
        Ok(())
    }

    #[test]
    fn test_fanout_requests_are_deterministic() -> TestResult {
        let plan = plan_with(vec![
            node_writing("t-1", &["src/a.rs"]),
            node_writing("t-2", &["src/b.rs"]),
            node_writing("t-3", &["src/c.rs"]),
        ])?;
        let resolved_cell = CellPlan::from_cell(
            &cell(
                "t-*",
                CellWritePartition::Required,
                CellBarrier::AllTerminal,
            ),
            None,
            &plan,
        )
        .map_err(ctx("from_cell schlug fehl"))?;

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
        Ok(())
    }

    // ── Zell-Executor ────────────────────────────────────────────────────────

    /// Treibt ein Future ohne Async-Laufzeit bis zum Ergebnis. Die Läufer
    /// dieser Tests warten auf nichts; nach höchstens einer Handvoll
    /// Durchläufen ist das Future fertig — sonst scheitert der Test statt
    /// ewig zu drehen.
    fn block_on<F: Future>(future: F) -> TestResult<F::Output> {
        use std::task::{Context, Poll, Waker};

        let mut future = std::pin::pin!(future);
        let mut cx = Context::from_waker(Waker::noop());
        for _ in 0..1_000 {
            if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
                return Ok(value);
            }
        }
        Err(TestError::Unexpected(
            "Future wurde nach 1000 Durchläufen nicht fertig".to_owned(),
        ))
    }

    fn batches(spec: &[&[&'static str]]) -> Vec<Vec<&'static str>> {
        spec.iter().map(|batch| batch.to_vec()).collect()
    }

    #[test]
    fn test_schedule_fanout_keeps_batches_and_caps_parallelism() {
        let schedule = CellSchedule::from_batches(
            "c".to_owned(),
            CellKind::Fanout,
            JoinSemantics::AllTerminal,
            batches(&[&["a", "b", "c"], &[], &["d"]]),
            2,
        );
        assert_eq!(schedule.stages.len(), 2, "leere Batches verschwinden");
        assert_eq!(schedule.stages[0].members, vec!["a", "b", "c"]);
        assert_eq!(schedule.stages[0].max_parallel, 2);
        assert_eq!(schedule.stages[1].max_parallel, 1);
        assert_eq!(
            schedule.members().copied().collect::<Vec<_>>(),
            vec!["a", "b", "c", "d"]
        );
    }

    #[test]
    fn test_schedule_sequential_runs_every_member_alone() {
        let schedule = CellSchedule::from_batches(
            "c".to_owned(),
            CellKind::Sequential,
            JoinSemantics::AllTerminal,
            batches(&[&["a", "b"], &["c"]]),
            8,
        );
        assert_eq!(schedule.stages.len(), 3);
        assert!(
            schedule
                .stages
                .iter()
                .all(|stage| stage.members.len() == 1 && stage.max_parallel == 1)
        );
    }

    #[test]
    fn test_schedule_zero_max_parallel_is_raised_to_one() {
        let schedule = CellSchedule::from_batches(
            "c".to_owned(),
            CellKind::Barrier,
            JoinSemantics::Collect,
            batches(&[&["a", "b"]]),
            0,
        );
        assert_eq!(schedule.stages[0].max_parallel, 1);
    }

    #[test]
    fn test_cell_plan_schedule_uses_the_resolved_batches_and_join() -> TestResult {
        let plan = plan_with(vec![
            node_writing("t-1", &["src/shared.rs"]),
            node_writing("t-2", &["src/shared.rs"]),
        ])?;
        let resolved = CellPlan::from_cell(
            &cell(
                "t-*",
                CellWritePartition::Required,
                CellBarrier::ExplicitJoin,
            ),
            None,
            &plan,
        )
        .map_err(ctx("from_cell schlug fehl"))?;
        let schedule = resolved.schedule(CellKind::Fanout, 4);
        assert_eq!(schedule.join, JoinSemantics::Collect);
        assert_eq!(
            schedule.stages.len(),
            2,
            "kollidierende Knoten, zwei Stufen"
        );
        Ok(())
    }

    #[test]
    fn test_run_cell_all_terminal_runs_every_stage_in_order() -> TestResult {
        let schedule = CellSchedule::from_batches(
            "c".to_owned(),
            CellKind::Fanout,
            JoinSemantics::AllTerminal,
            batches(&[&["a", "b"], &["c"]]),
            4,
        );
        let mut seen: Vec<Vec<&str>> = Vec::new();
        let run = block_on(run_cell(schedule, |stage, join| {
            assert_eq!(join, JoinSemantics::AllTerminal);
            seen.push(stage.members.clone());
            let results: Vec<Result<String, String>> = stage
                .members
                .iter()
                .map(|member| {
                    if *member == "b" {
                        Err("b scheiterte".to_owned())
                    } else {
                        Ok(format!("{member}!"))
                    }
                })
                .collect();
            async move { results }
        }))?;
        assert_eq!(seen, vec![vec!["a", "b"], vec!["c"]]);
        assert_eq!(run.results.len(), 3);
        assert_eq!(run.results[0].1, MemberOutcome::Completed("a!".to_owned()));
        assert_eq!(
            run.results[1].1,
            MemberOutcome::Failed("b scheiterte".to_owned())
        );
        assert!(run.results[2].1.is_completed(), "Fanout läuft weiter");
        assert!(!run.is_satisfied(), "AllTerminal mit einem Fehlschlag");
        assert_eq!(run.completed(), 2);
        Ok(())
    }

    #[test]
    fn test_run_cell_any_terminal_skips_later_stages_after_a_winner() -> TestResult {
        let schedule = CellSchedule::from_batches(
            "c".to_owned(),
            CellKind::Sequential,
            JoinSemantics::AnyTerminal,
            batches(&[&["a", "b", "c"]]),
            4,
        );
        let mut calls = 0_usize;
        let run = block_on(run_cell(schedule, |stage, _join| {
            calls += 1;
            let results: Vec<Result<&str, String>> = stage
                .members
                .iter()
                .map(|member| {
                    if *member == "a" {
                        Err("a scheiterte".to_owned())
                    } else {
                        Ok(*member)
                    }
                })
                .collect();
            async move { results }
        }))?;
        assert_eq!(calls, 2, "nach dem Gewinner b startet c nicht mehr");
        assert_eq!(run.results[1].1, MemberOutcome::Completed("b"));
        assert_eq!(
            run.results[2].1,
            MemberOutcome::Skipped(SkipReason::WinnerFound)
        );
        assert!(run.is_satisfied());
        Ok(())
    }

    #[test]
    fn test_run_cell_barrier_stops_at_a_failed_gate_unless_collecting() -> TestResult {
        for (join, expect_skip) in [
            (JoinSemantics::AllTerminal, true),
            (JoinSemantics::Collect, false),
        ] {
            let schedule = CellSchedule::from_batches(
                "c".to_owned(),
                CellKind::Barrier,
                join,
                batches(&[&["a", "b"], &["c"]]),
                4,
            );
            let run = block_on(run_cell(schedule, |stage, _join| {
                let results: Vec<Result<(), String>> = stage
                    .members
                    .iter()
                    .map(|member| {
                        if *member == "b" {
                            Err("b scheiterte".to_owned())
                        } else {
                            Ok(())
                        }
                    })
                    .collect();
                async move { results }
            }))?;
            let last = &run.results[2].1;
            if expect_skip {
                assert_eq!(*last, MemberOutcome::Skipped(SkipReason::BarrierNotReached));
                assert!(!run.is_satisfied());
            } else {
                assert!(last.is_completed(), "Collect öffnet jedes Tor");
                assert!(run.is_satisfied(), "Collect: alle liefen bis zum Ende");
            }
        }
        Ok(())
    }

    #[test]
    fn test_run_cell_marks_missing_runner_results_as_failed() -> TestResult {
        let schedule = CellSchedule::from_batches(
            "c".to_owned(),
            CellKind::Fanout,
            JoinSemantics::Collect,
            batches(&[&["a", "b"]]),
            4,
        );
        let run = block_on(run_cell(schedule, |_stage, _join| async {
            vec![Ok::<u8, String>(1)]
        }))?;
        assert_eq!(run.results[0].1, MemberOutcome::Completed(1));
        assert!(matches!(run.results[1].1, MemberOutcome::Failed(_)));
        Ok(())
    }

    /// Ein Analyse-ähnliches Element: Name und Schreibbereich.
    #[derive(Debug, PartialEq, Eq)]
    struct Unit {
        name: &'static str,
        writes: &'static [&'static str],
    }

    fn unit_node(unit: &Unit) -> PlanNode {
        node_writing(unit.name, unit.writes)
    }

    fn research_clan() -> RawClanSpec {
        RawClanSpec {
            id: "research".to_owned(),
            name: "Research".to_owned(),
            leader: definition_ref("research-orchestrator"),
            family: definition_ref("research"),
            plan_scope: "research-*".to_owned(),
            child_depth_cost: 1,
        }
    }

    #[test]
    fn test_resolve_wave_partitions_items_through_the_cell() {
        let a = Unit {
            name: "research-a",
            writes: &["src/shared.rs"],
        };
        let b = Unit {
            name: "research-b",
            writes: &["src/shared.rs"],
        };
        let c = Unit {
            name: "research-c",
            writes: &["src/other.rs"],
        };
        let items = [&a, &b, &c];
        let clan = research_clan();
        let mut spec = cell(
            "research-*",
            CellWritePartition::Required,
            CellBarrier::AnyTerminal,
        );
        spec.kind = CellKind::Sequential;
        let wave = resolve_wave(Some((&clan, &spec)), &items, unit_node);
        assert_eq!(wave.cell_id.as_deref(), Some("cell-1"));
        assert_eq!(wave.join, JoinSemantics::AnyTerminal);
        assert_eq!(wave.kind, CellKind::Sequential);
        assert_eq!(wave.batches.len(), 2);
        let flattened: usize = wave.batches.iter().map(Vec::len).sum();
        assert_eq!(flattened, 3);
        let schedule = wave.schedule(4);
        assert_eq!(
            schedule.stages.len(),
            3,
            "sequenziell: ein Mitglied je Stufe"
        );
    }

    #[test]
    fn test_resolve_wave_falls_back_without_cell_or_on_partial_cover() {
        let a = Unit {
            name: "research-a",
            writes: &[],
        };
        let outsider = Unit {
            name: "coding-x",
            writes: &[],
        };
        let items = [&a, &outsider];

        let without = resolve_wave(None, &items, unit_node);
        assert_eq!(without.batches, vec![vec![&a, &outsider]]);
        assert_eq!(without.join, JoinSemantics::AllTerminal);
        assert_eq!(without.cell_id, None);

        // Die Zelle wählt nur `research-*` — `coding-x` bliebe ungedeckt,
        // also Rückfall auf die ungeteilte Welle statt stiller Verlust.
        let clan = research_clan();
        let spec = cell(
            "research-*",
            CellWritePartition::None,
            CellBarrier::AnyTerminal,
        );
        let partial = resolve_wave(Some((&clan, &spec)), &items, unit_node);
        assert_eq!(partial.batches, vec![vec![&a, &outsider]]);
        assert_eq!(partial.join, JoinSemantics::AllTerminal);
        assert_eq!(partial.cell_id, None);

        let empty: [&Unit; 0] = [];
        assert!(resolve_wave(None, &empty, unit_node).batches.is_empty());
    }

    #[test]
    fn test_batches_for_items_maps_tasks_back_and_falls_back_on_partial_cover() -> TestResult {
        let plan = plan_with(vec![
            node_writing("t-1", &["src/shared.rs"]),
            node_writing("t-2", &["src/shared.rs"]),
        ])?;
        let resolved = CellPlan::from_cell(
            &cell(
                "t-*",
                CellWritePartition::Required,
                CellBarrier::AllTerminal,
            ),
            None,
            &plan,
        )
        .map_err(ctx("from_cell schlug fehl"))?;
        let one = "t-1";
        let two = "t-2";
        let items = [&one, &two];
        let mapped = batches_for_items(Some(&resolved), &items, |item| TaskId::new(*item));
        assert_eq!(mapped.len(), 2);
        assert_eq!(mapped.iter().map(Vec::len).sum::<usize>(), 2);

        let three = "t-3";
        let uncovered = [&one, &two, &three];
        let fallback = batches_for_items(Some(&resolved), &uncovered, |item| TaskId::new(*item));
        assert_eq!(fallback, vec![vec![&one, &two, &three]]);
        assert_eq!(
            batches_for_items(None, &items, |item| TaskId::new(*item)),
            vec![vec![&one, &two]]
        );
        Ok(())
    }
}
