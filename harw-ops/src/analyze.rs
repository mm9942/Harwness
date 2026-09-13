//! `/analyze` — Bottom-up-Analyse eines Workspace über Analyst-Kindagenten.
//!
//! # Verantwortungsbereich
//! Implementiert die `analyze`-Operation gemäß AP W4-04. Sie ist die einzige
//! Operation dieses APs, die **selbst orchestriert**: sie lädt den
//! Workspace-Graph, legt je Crate einen `Analysis`-Plan-Knoten an, fährt die
//! Ebenen von den Blättern aufwärts als Fan-out-Wellen und verdichtet das
//! Ergebnis in einem `Synthesis`-Knoten.
//!
//! Deshalb trägt sie **kein** `agent_tool`-Attribut: eine Operation, die selbst
//! Kinder startet, darf nicht zusätzlich als einzelnes Kind-Werkzeug erscheinen
//! — das Modell würde sonst eine Orchestrierung für einen Einzelaufruf halten.
//!
//! # Ablauf
//! 1. [`WorkspaceGraph::load`] auf der kanonischen Sandbox-Wurzel.
//! 2. Optional [`WorkspaceGraph::subgraph`], wenn ein Crate genannt ist.
//! 3. Je Crate ein [`PlanNode`] (`kind = Analysis`, `read_scope = <dir>/**`),
//!    dessen Abhängigkeiten die **internen** Dependencies des Crates sind —
//!    [`WorkspaceGraph::topological_levels`] liefert genau diese Ordnung,
//!    Ebene 0 sind die Blätter.
//! 4. Je Ebene wird die Zelle des Clans [`RESEARCH_CLAN_ID`] der eingebauten
//!    Organisation über [`CellPlan::from_cell`] aufgelöst; ihre Batches sind die
//!    Startgruppen der Welle (siehe „Zell-gesteuerter Fan-out" unten).
//! 5. Bei `dry_run` endet die Operation hier und gibt den Plan **samt Batches**
//!    aus.
//! 6. Sonst je Batch ein [`fanout_children`]-Lauf mit der Rolle
//!    [`role_names::ANALYST`]; Findings werden abgelegt, als Evidenz angehängt
//!    und die Knoten auf `Completed` gefahren. Nach jeder Welle läuft
//!    [`PlanController::reconcile`]; seine Vorschläge gehen in die Ausgabe.
//! 7. Zum Schluss ein `Synthesis`-Knoten, der von allen Analyse-Knoten abhängt.
//!
//! # Zell-gesteuerter Fan-out
//! Welche Knoten gemeinsam starten dürfen, steht nicht mehr hier, sondern in
//! `harw-registry-defaults/agents/organization/default.toml`: die Zelle
//! `research-wave` des Clans `research` trägt Muster, Barriere und
//! Schreibtrennung. [`CellPlan::from_cell`] löst sie gegen den Plan der Ebene
//! auf, `write_partition = "required"` zerlegt sie in Batches mit paarweise
//! disjunkten Schreibbereichen, und [`CellPlan::join`] liefert die
//! [`JoinSemantics`] der Welle. Die Batches laufen **nacheinander**, ihre
//! Mitglieder nebenläufig — genau das bedeutet eine erzwungene Schreibtrennung.
//!
//! Deshalb tragen die Plan-Knoten den Clan im Namen (`research-<crate>`, siehe
//! [`node_id`]): [`CellPlan::from_cell`] wählt Mitglieder über einen Glob gegen
//! die `TaskId` **und** den `write_scope`; Analyse-Knoten haben keinen
//! `write_scope`, also entscheidet allein die `TaskId`.
//!
//! # Rückfall — `/analyze` darf daran nicht scheitern
//! `/analyze` ist der einzige Ende-zu-Ende-Pfad der Planungsfläche. Jede Stufe
//! der Zell-Auflösung fällt deshalb auf das bisherige Verhalten zurück, statt
//! einen Fehler zu erzeugen: eine nicht ladbare Organisation, ein fehlender
//! Clan, eine fehlende Zelle, ein Muster ohne Treffer, ein Auflösungsfehler und
//! sogar eine Zelle, die nur einen *Teil* der Ebene auswählt, führen alle zu
//! einer einzigen Welle mit allen Crates der Ebene in Graph-Reihenfolge
//! ([`wave_batches`]). Eine Zelle darf die Arbeit einer Ebene umsortieren und
//! aufteilen — sie darf sie niemals verschlucken.
//!
//! # `bottom_up = false`
//! Top-down heißt hier: die Wellen laufen in umgekehrter Reihenfolge **und** die
//! Analyse-Knoten bekommen keine Abhängigkeitskanten. Beides gehört zusammen —
//! ein Knoten, der auf seine Dependencies wartet, kann nicht vor ihnen laufen
//! (`harw_plan` weist den Statuswechsel sonst zu Recht ab). Die Knoten werden
//! trotzdem immer in Leaf-first-Reihenfolge **angelegt**, weil `AddNode` keine
//! unbekannten Abhängigkeiten akzeptiert.
//!
//! # Schlüsseltypen
//! - [`AnalyzeArgs`] — Argument-Container mit Flag-Parsing auf der
//!   Command-Fläche.
//! - `AnalyzeOperation` — vom `#[operation]`-Makro erzeugter Op-Struct.
//!
//! # Nebenläufigkeit
//! `AnalyzeOperation` ist ein zustandsloser Unit-Struct → `Send + Sync`. Die
//! Kinder einer Welle laufen nebenläufig; `max_parallel` deckelt sie.
//!
//! # Fehler
//! - [`OpError::InvalidArguments`]: unbekanntes Flag, ungültiges
//!   `--max-parallel`, zweiter Crate-Name.
//! - [`OpError::NotAvailable`]: kein Plan-Store (nur im Nicht-Dry-Run) oder
//!   kein Agent-Spawner im Kontext.
//! - [`OpError::Execution`]: der Workspace-Graph ist nicht ladbar, oder eine
//!   Plan-Mutation wurde abgelehnt.
//!
//! # Beispiel
//! ```rust,no_run
//! use harw_ops::analyze::AnalyzeArgs;
//! use harw_operations::FromRawArgs;
//!
//! // "/analyze --dry-run harw-core"
//! let args = AnalyzeArgs::from_raw_args(&["--dry-run".to_owned(), "harw-core".to_owned()])
//!     .expect("gültige Flags");
//! assert_eq!(args.dry_run, Some(true));
//! assert_eq!(args.crate_name.as_deref(), Some("harw-core"));
//! ```

use std::path::Path;

use harw_code_graph::{CrateNode, WorkspaceGraph};
use harw_core::child_controller::JoinSemantics;
use harw_core_bridge::{ChildReturnContract, fanout_children, parse_budget_hint};
use harw_macros::operation;
use harw_operations::require_service;
use harw_operations::{FromRawArgs, OpContext, OpError, OpOutput};
use harw_plan::{
    PathOrSymbol, Plan, PlanAction, PlanId, PlanNode, PlanNodeKind, PlanNodeStatus, PlanStore,
    PlanToolConfig, RevisionId, TaskId,
};
use harw_plan_bridge::{
    CellPlan, OpContextPlanExt, PlanController, ReconcileInput, offset_from_timestamp,
};
use harw_registry_defaults::embedded_agents::{
    DEFAULT_ORGANIZATION_ID, RESEARCH_CLAN_ID, RawCellSpec, RawClanSpec, ResolvedOrganization,
    clan_cell, default_organization,
};
use harw_registry_defaults::profile::role_names;
use harw_research::{
    Freshness, QuestionId, QuestionScope, ResearchFinding, ResearchQuestion, SourceClass,
};
use serde_json::{Value, json};

use crate::explore::{READ_ONLY_REDUCER, child_payload, finding_from_value, persist_finding};

// ── Konstanten ───────────────────────────────────────────────────────────────

/// Budget je Analyst-Kind einer Welle.
///
/// Grammatik siehe [`parse_budget_hint`]. Großzügiger als der Einzel-Lauf in
/// [`crate::explore`], weil ein Analyst ein ganzes Crate lesen muss.
const ANALYST_BUDGET: &str = "90k_tokens,60_tool_calls,300s";

/// Vorgabe für die Zahl gleichzeitiger Kinder je Welle.
const DEFAULT_MAX_PARALLEL: usize = 4;

/// Akteur-Kennung für Plan-Mutationen aus dieser Operation.
const ACTOR_ANALYZE: &str = "op:analyze";

/// Plan-Bezeichner, unter dem `/analyze` einen fehlenden Plan anlegt.
const ANALYSIS_PLAN_ID: &str = "plan-analyze";

/// Bezeichner des abschließenden Synthesis-Knotens.
const SYNTHESIS_NODE_ID: &str = "analyze-synthesis";

/// Erwartetes Ausgabeformat eines Analyst-Kindes.
const ANALYSIS_EXPECTED_OUTPUT: &str = "Ein ResearchFinding, dessen Schlussfolgerung die fünf \
     geforderten Punkte in dieser Reihenfolge abarbeitet. Jede Behauptung trägt einen Beleg mit \
     Dateipfad und Zeilenbereich; alles Unbelegte gehört in unresolved_questions.";

/// Stop-Bedingung eines Analyst-Kindes.
const ANALYSIS_STOP_CONDITION: &str = "Alle fünf Punkte sind für dieses Crate beantwortet oder \
     ausdrücklich als offen markiert. Kein Blick über die Crate-Grenze hinaus außer für die \
     Konsumentenliste.";

// ── Argumente ────────────────────────────────────────────────────────────────

/// Eingabe-Argumente der `analyze`-Operation.
///
/// # Beschreibung
/// Auf der Command-Fläche werden Flags geparst (siehe [`FromRawArgs`]-Impl); auf
/// den JSON-Flächen sind es gewöhnliche Felder. Die Flag-Form existiert, weil
/// `/analyze --dry-run` sonst nur über einen Tool-Call erreichbar wäre.
///
/// # Felder
/// - `crate_name` (`Option<String>`): einzelnes Crate; ohne Angabe der ganze
///   Workspace.
/// - `bottom_up` (`Option<bool>`): von den Blättern aufwärts (Vorgabe: `true`).
/// - `dry_run` (`Option<bool>`): nur den Plan erzeugen, keine Kinder starten.
/// - `max_parallel` (`Option<usize>`): Obergrenze gleichzeitiger Kinder je
///   Welle (Vorgabe: 4).
///
/// # Spec-Referenz
/// AP W4-04 — `/analyze`.
#[derive(Debug, Default, serde::Deserialize, harw_macros::OpArgs)]
pub struct AnalyzeArgs {
    /// Einzelnes Crate; ohne Angabe der ganze Workspace.
    #[serde(default)]
    #[raw(first)]
    pub crate_name: Option<String>,
    /// Von den Blättern aufwärts (Standard: true).
    #[serde(default)]
    pub bottom_up: Option<bool>,
    /// Nur den Plan erzeugen, keine Kinder starten.
    #[serde(default)]
    pub dry_run: Option<bool>,
    /// Obergrenze gleichzeitiger Kinder je Welle.
    #[serde(default)]
    pub max_parallel: Option<usize>,
}

impl FromRawArgs for AnalyzeArgs {
    /// Parst Crate-Name und Flags aus der Command-Zeile.
    ///
    /// # Beschreibung
    /// Erkannt werden `--dry-run` / `--no-dry-run`, `--bottom-up` /
    /// `--top-down` (alias `--no-bottom-up`) sowie `--max-parallel <n>` und
    /// `--max-parallel=<n>`. Das erste flag-freie Token ist der Crate-Name.
    ///
    /// Ein unbekanntes Flag ist ein **Fehler**: still ignoriert würde
    /// `/analyze --dry-runn` einen echten Fan-out starten, den der Aufrufer
    /// gerade vermeiden wollte.
    ///
    /// # Argumente
    /// - `tokens` (`&[String]`): die rohen Command-Argumente.
    ///
    /// # Rückgabe
    /// Die befüllten [`AnalyzeArgs`].
    ///
    /// # Fehler
    /// - [`OpError::InvalidArguments`]: unbekanntes Flag, fehlender oder
    ///   ungültiger `--max-parallel`-Wert, zweiter Crate-Name.
    fn from_raw_args(tokens: &[String]) -> Result<Self, OpError> {
        let mut args = Self::default();
        let mut index = 0;
        while index < tokens.len() {
            let token = tokens[index].as_str();
            match token {
                "--dry-run" => args.dry_run = Some(true),
                "--no-dry-run" => args.dry_run = Some(false),
                "--bottom-up" => args.bottom_up = Some(true),
                "--top-down" | "--no-bottom-up" => args.bottom_up = Some(false),
                "--max-parallel" => {
                    index += 1;
                    let Some(value) = tokens.get(index) else {
                        return Err(OpError::InvalidArguments(
                            "--max-parallel erwartet eine Zahl".to_owned(),
                        ));
                    };
                    args.max_parallel = Some(parse_max_parallel(value)?);
                }
                other => {
                    if let Some(value) = other.strip_prefix("--max-parallel=") {
                        args.max_parallel = Some(parse_max_parallel(value)?);
                    } else if other.starts_with("--") {
                        return Err(OpError::InvalidArguments(format!(
                            "unbekanntes Flag '{other}'; erlaubt sind: --dry-run, --no-dry-run, \
                             --bottom-up, --top-down, --max-parallel <n>"
                        )));
                    } else if args.crate_name.is_none() {
                        args.crate_name = Some(other.to_owned());
                    } else {
                        return Err(OpError::InvalidArguments(format!(
                            "unerwartetes Argument '{other}'; /analyze nimmt höchstens einen \
                             Crate-Namen"
                        )));
                    }
                }
            }
            index += 1;
        }
        Ok(args)
    }
}

/// Parst den Wert von `--max-parallel`.
///
/// # Errors
/// - [`OpError::InvalidArguments`]: keine Zahl, oder `0` (eine Welle ohne Kind
///   ist keine Welle).
fn parse_max_parallel(raw: &str) -> Result<usize, OpError> {
    let value: usize = raw.trim().parse().map_err(|error| {
        OpError::InvalidArguments(format!(
            "--max-parallel erwartet eine positive ganze Zahl, fand '{raw}': {error}"
        ))
    })?;
    if value == 0 {
        return Err(OpError::InvalidArguments(
            "--max-parallel muss mindestens 1 sein".to_owned(),
        ));
    }
    Ok(value)
}

// ── Plan-Bausteine ───────────────────────────────────────────────────────────

/// Bildet den Plan-Knoten-Bezeichner eines Crates.
///
/// # Beschreibung
/// Der Bezeichner trägt den Clan, dem der Knoten gehört: `research-<crate>`.
/// Das ist keine Kosmetik, sondern die Bedingung dafür, dass die Zelle des
/// Research-Clans ihn überhaupt finden kann — `CellPlan::from_cell` wählt
/// Mitglieder über einen Glob gegen die `TaskId` und den `write_scope`, und ein
/// Analyse-Knoten hat keinen `write_scope`. Der Präfix kommt deshalb aus
/// [`RESEARCH_CLAN_ID`] und nicht aus einem Literal: ändert sich die Clan-ID der
/// eingebauten Organisation, ändern sich die Knotennamen mit.
fn node_id(crate_name: &str) -> String {
    format!("{RESEARCH_CLAN_ID}-{crate_name}")
}

/// Bildet den Lesebereich eines Crates relativ zur Workspace-Wurzel.
///
/// Fällt auf den Crate-Namen zurück, wenn das Verzeichnis nicht unterhalb der
/// Wurzel liegt — dann ist der Name die einzige Kennung, die der Knoten hat.
fn read_scope_for(root: &Path, crate_node: &CrateNode) -> String {
    match crate_node.dir.strip_prefix(root) {
        Ok(relative) if !relative.as_os_str().is_empty() => format!("{}/**", relative.display()),
        _ => format!("{}/**", crate_node.name),
    }
}

/// Baut den `Analysis`-Knoten eines Crates.
///
/// `created_at`/`updated_at` werden vom Plan-Store überschrieben (Design-Doc
/// §7); die hier gesetzten Werte sind nur Platzhalter für den Typ.
fn analysis_node(root: &Path, crate_node: &CrateNode, dependencies: Vec<TaskId>) -> PlanNode {
    let now = offset_from_timestamp(jiff::Timestamp::now());
    PlanNode {
        id: TaskId::new(node_id(&crate_node.name)),
        objective: format!("Bottom-up-Analyse von {}", crate_node.name),
        dependencies,
        input_contracts: Vec::new(),
        output_contracts: Vec::new(),
        read_scope: vec![PathOrSymbol::new(read_scope_for(root, crate_node))],
        write_scope: Vec::new(),
        forbidden_scope: Vec::new(),
        acceptance_criteria: Vec::new(),
        invalidation_conditions: Vec::new(),
        status: PlanNodeStatus::Draft,
        evidence: Vec::new(),
        kind: PlanNodeKind::Analysis,
        wave: Some(crate_node.level),
        assignment: None,
        parent: None,
        created_at: now,
        updated_at: now,
    }
}

/// Baut den abschließenden `Synthesis`-Knoten.
fn synthesis_node(dependencies: Vec<TaskId>) -> PlanNode {
    let now = offset_from_timestamp(jiff::Timestamp::now());
    PlanNode {
        id: TaskId::new(SYNTHESIS_NODE_ID),
        objective: "Verdichtung der Crate-Analysen zu einem Workspace-Bild".to_owned(),
        dependencies,
        input_contracts: Vec::new(),
        output_contracts: Vec::new(),
        read_scope: Vec::new(),
        write_scope: Vec::new(),
        forbidden_scope: Vec::new(),
        acceptance_criteria: Vec::new(),
        invalidation_conditions: Vec::new(),
        status: PlanNodeStatus::Draft,
        evidence: Vec::new(),
        kind: PlanNodeKind::Synthesis,
        wave: None,
        assignment: None,
        parent: None,
        created_at: now,
        updated_at: now,
    }
}

/// Baut die gebundene Frage an das Analyst-Kind eines Crates.
///
/// # Beschreibung
/// Die Frage ist absichtlich nummeriert und abschließend: das Kind soll nicht
/// „das Crate anschauen", sondern fünf benannte Dinge liefern. Der Scope
/// begrenzt es auf das Crate-Verzeichnis; die Konsumentenliste steht schon in
/// der Frage, damit das Kind sie nicht selbst erlaufen muss.
fn analysis_question(
    root: &Path,
    crate_node: &CrateNode,
    consumers: &[&CrateNode],
) -> ResearchQuestion {
    let consumer_names: Vec<&str> = consumers.iter().map(|node| node.name.as_str()).collect();
    let consumer_hint = if consumer_names.is_empty() {
        "Keine Workspace-Crate konsumiert es (Stand Graph).".to_owned()
    } else {
        format!(
            "Laut Graph konsumieren es: {}. Prüfe für jedes, welche Symbole es tatsächlich \
             benutzt.",
            consumer_names.join(", ")
        )
    };

    ResearchQuestion {
        id: QuestionId::new(node_id(&crate_node.name)),
        question: format!(
            "Analysiere das Crate `{name}` (Version {version}, Ebene {level}) vollständig und \
             liefere genau diese fünf Punkte:\n\
             1. Öffentliche API: jedes `pub`-Item mit Signatur, gruppiert nach Modul, und wofür \
                es da ist.\n\
             2. Konsumenten: welche Workspace-Crates dieses Crate benutzen und welche Symbole \
                sie davon ziehen. {consumer_hint}\n\
             3. Stubs und Lücken: jedes `todo!()`, `unimplemented!()`, `TODO`, `FIXME`, jede \
                Funktion, die einen Platzhalterwert liefert, und jede Fehlervariante, die nie \
                erzeugt wird.\n\
             4. Testabdeckung: welche öffentlichen Funktionen haben Tests, welche nicht, und \
                welche Tests prüfen nur, dass nichts panickt.\n\
             5. Abweichungen zwischen Doku und Verhalten: jede Stelle, an der `//!`- oder \
                `///`-Dokumentation etwas behauptet, das der Code nicht tut.\n\
             Belege jede Aussage mit Dateipfad und Zeilenbereich.",
            name = crate_node.name,
            version = crate_node.version,
            level = crate_node.level,
        ),
        scope: QuestionScope {
            paths: vec![read_scope_for(root, crate_node)],
            crates: vec![crate_node.name.clone()],
            urls: Vec::new(),
            sources: vec![SourceClass::LocalSource],
        },
        expected_output: ANALYSIS_EXPECTED_OUTPUT.to_owned(),
        freshness: Freshness::AnyTime,
        stop_condition: ANALYSIS_STOP_CONDITION.to_owned(),
        owner_task: Some(node_id(&crate_node.name)),
    }
}

/// Legt einen Plan an, falls noch keiner existiert.
///
/// # Errors
/// - [`OpError::Execution`]: der Store lehnte `Create` ab.
fn ensure_plan(plan: &dyn PlanStore) -> Result<(), OpError> {
    if plan.current().is_ok() {
        return Ok(());
    }
    plan.apply(
        PlanAction::Create {
            plan_id: PlanId::new(ANALYSIS_PLAN_ID),
            goal: "Bottom-up-Analyse des Workspace".to_owned(),
        },
        ACTOR_ANALYZE,
    )
    .map_err(|error| OpError::Execution(format!("Plan konnte nicht angelegt werden: {error}")))?;
    Ok(())
}

/// Fügt einen Knoten hinzu, falls er noch nicht existiert.
///
/// # Returns
/// `true`, wenn der Knoten neu angelegt wurde.
///
/// # Errors
/// - [`OpError::Execution`]: der Store lehnte `AddNode` ab.
fn ensure_node(plan: &dyn PlanStore, node: PlanNode) -> Result<bool, OpError> {
    let snapshot = plan
        .current()
        .map_err(|error| OpError::Execution(format!("Plan nicht lesbar: {error}")))?;
    if snapshot.nodes.iter().any(|existing| existing.id == node.id) {
        return Ok(false);
    }
    let id = node.id.clone();
    plan.apply(PlanAction::AddNode { node }, ACTOR_ANALYZE)
        .map_err(|error| {
            OpError::Execution(format!(
                "Plan-Knoten '{}' konnte nicht angelegt werden: {error}",
                id.as_str()
            ))
        })?;
    Ok(true)
}

/// Fährt einen Knoten über die legale Statuskette auf `Completed`.
///
/// # Beschreibung
/// `harw_plan` erlaubt nur `Draft → Ready → InProgress → Completed`. Diese
/// Funktion geht genau den fehlenden Rest dieser Kette; ein bereits
/// abgeschlossener Knoten ist ein No-op, ein terminal anderer Zustand ein
/// Fehler statt einer stillen Übergehung.
///
/// # Errors
/// - [`OpError::Execution`]: der Knoten fehlt, steht terminal oder eine
///   Transition wurde abgelehnt (z. B. weil eine Dependency offen ist).
fn advance_to_completed(plan: &dyn PlanStore, id: &str) -> Result<(), OpError> {
    let task = TaskId::new(id);
    let snapshot = plan
        .current()
        .map_err(|error| OpError::Execution(format!("Plan nicht lesbar: {error}")))?;
    let Some(node) = snapshot.nodes.iter().find(|node| node.id == task) else {
        return Err(OpError::Execution(format!(
            "Plan-Knoten '{id}' existiert nicht"
        )));
    };

    let remaining: &[PlanNodeStatus] = match node.status {
        PlanNodeStatus::Draft => &[
            PlanNodeStatus::Ready,
            PlanNodeStatus::InProgress,
            PlanNodeStatus::Completed,
        ],
        PlanNodeStatus::Ready => &[PlanNodeStatus::InProgress, PlanNodeStatus::Completed],
        PlanNodeStatus::InProgress => &[PlanNodeStatus::Completed],
        PlanNodeStatus::Completed => &[],
        terminal => {
            return Err(OpError::Execution(format!(
                "Plan-Knoten '{id}' steht auf {terminal:?} und kann nicht abgeschlossen werden"
            )));
        }
    };

    for status in remaining {
        plan.apply(
            PlanAction::SetStatus {
                id: task.clone(),
                status: *status,
                reason: Some("Analyse-Welle abgeschlossen".to_owned()),
            },
            ACTOR_ANALYZE,
        )
        .map_err(|error| {
            OpError::Execution(format!(
                "Statuswechsel von '{id}' auf {status:?} wurde abgelehnt: {error}"
            ))
        })?;
    }
    Ok(())
}

/// Ruft [`PlanController::reconcile`] und serialisiert seine Vorschläge.
///
/// # Beschreibung
/// Der Controller ist eine reine Funktion; hier wird **nichts** angewandt. Die
/// Vorschläge gehen unverändert in die Ausgabe, damit ein Mensch oder das
/// Modell entscheidet, was daraus folgt.
///
/// # Errors
/// - [`OpError::Execution`]: Plan nicht lesbar oder Schritt nicht
///   serialisierbar.
fn reconcile_proposals(
    ctx: &OpContext,
    plan: &dyn PlanStore,
    findings: &[ResearchFinding],
) -> Result<Vec<Value>, OpError> {
    let snapshot = plan
        .current()
        .map_err(|error| OpError::Execution(format!("Plan nicht lesbar: {error}")))?;
    let config = ctx
        .plan_config()
        .unwrap_or_else(PlanToolConfig::enabled_defaults);
    let steps = PlanController::reconcile(ReconcileInput {
        goal: None,
        plan: &snapshot,
        new_findings: findings,
        job_states: &[],
        config: &config,
        now: offset_from_timestamp(jiff::Timestamp::now()),
    });

    steps
        .iter()
        .map(|step| {
            serde_json::to_value(step).map_err(|error| {
                OpError::Execution(format!("Reconcile-Vorschlag nicht serialisierbar: {error}"))
            })
        })
        .collect()
}

/// Beschreibt eine Welle für die Ausgabe.
///
/// `batches` sind die Startgruppen der Welle in Ausführungsreihenfolge; sie
/// stammen aus der Zelle des Research-Clans oder aus dem Rückfall
/// ([`wave_batches`]).
fn wave_json(
    level: usize,
    crates: &[&CrateNode],
    dependencies_enabled: bool,
    batches: &[Vec<&CrateNode>],
) -> Value {
    let nodes: Vec<Value> = crates
        .iter()
        .map(|crate_node| {
            let dependencies: Vec<String> = if dependencies_enabled {
                crate_node.deps.iter().map(|dep| node_id(dep)).collect()
            } else {
                Vec::new()
            };
            json!({
                "id": node_id(&crate_node.name),
                "crate": crate_node.name,
                "level": crate_node.level,
                "dependencies": dependencies,
            })
        })
        .collect();
    let batches: Vec<Vec<String>> = batches
        .iter()
        .map(|batch| {
            batch
                .iter()
                .map(|crate_node| node_id(&crate_node.name))
                .collect()
        })
        .collect();
    json!({ "level": level, "nodes": nodes, "batches": batches })
}

// ── Zell-Auflösung ───────────────────────────────────────────────────────────

/// Eine ausführbare Welle: die Batches der Ebene und ihre Join-Semantik.
///
/// # Beschreibung
/// Das Ergebnis der Zell-Auflösung einer Ebene. `batches` laufen nacheinander,
/// die Mitglieder eines Batches nebenläufig. Ohne auflösbare Zelle enthält
/// `batches` genau einen Batch mit allen Crates der Ebene und `join` ist
/// [`JoinSemantics::AllTerminal`] — das bisherige Verhalten.
///
/// # Nebenläufigkeit
/// Reiner Werttyp; hält nur Verweise auf den Workspace-Graphen.
struct WavePlan<'a> {
    /// Die Startgruppen der Welle in Ausführungsreihenfolge.
    batches: Vec<Vec<&'a CrateNode>>,
    /// Wie der Orchestrator auf die Kinder eines Batches wartet.
    join: JoinSemantics,
    /// Die Zell-ID, wenn die Welle aus einer Zelle stammt; sonst `None`.
    cell_id: Option<String>,
}

/// Lädt die eingebaute Default-Organisation.
///
/// # Rückgabe
/// `Some(organization)` bei erfolgreicher Auflösung, sonst `None`.
///
/// # Rückfall
/// Ein Fehler wird protokolliert und zu `None`: eine defekte oder fehlende
/// Organisationsdefinition darf `/analyze` nicht anhalten — die Operation
/// verliert dann nur die deklarative Wellensteuerung, nicht ihre Funktion.
///
/// # Nebenläufigkeit
/// Zustandslos; kein I/O außer der Systemuhr für die Trace-Zeitstempel.
fn load_organization() -> Option<ResolvedOrganization> {
    match default_organization() {
        Ok(organization) => Some(organization),
        Err(error) => {
            tracing::warn!(
                organization = DEFAULT_ORGANIZATION_ID,
                error = %error,
                "analyze.organization.unavailable"
            );
            None
        }
    }
}

/// Baut den In-Memory-Plan einer einzelnen Welle.
///
/// # Beschreibung
/// [`CellPlan::from_cell`] löst gegen einen [`Plan`] auf, nicht gegen einen
/// Crate-Graphen. Dieser Plan enthält genau die Analyse-Knoten *einer* Ebene —
/// dadurch bleibt die Wellenordnung erhalten, die
/// [`WorkspaceGraph::topological_levels`] vorgibt: eine Zelle über dem
/// Gesamtplan würde alle Ebenen zu einer einzigen Welle verschmelzen und die
/// Bottom-up-Ordnung zerstören.
///
/// Die Knoten tragen bewusst **keine** Abhängigkeiten: innerhalb einer Ebene
/// hängt kein Knoten von einem anderen ab, und die Auswahl liest ohnehin nur
/// `id` und `write_scope`. Der Plan wird nirgends persistiert.
///
/// # Argumente
/// - `root` (`&Path`): Workspace-Wurzel für die Lesebereiche.
/// - `crates` (`&[&CrateNode]`): die Crates dieser Ebene.
///
/// # Rückgabe
/// Ein flüchtiger [`Plan`] mit einem Analyse-Knoten je Crate.
///
/// # Nebenläufigkeit
/// Rein bis auf die Systemuhr für die Zeitstempel der Knoten.
fn wave_plan(root: &Path, crates: &[&CrateNode]) -> Plan {
    let now = offset_from_timestamp(jiff::Timestamp::now());
    Plan {
        id: PlanId::new(ANALYSIS_PLAN_ID),
        revision: RevisionId::new(0),
        parent_revision: None,
        goal_statement: "Analyse-Welle".to_owned(),
        goal_id: None,
        nodes: crates
            .iter()
            .map(|crate_node| analysis_node(root, crate_node, Vec::new()))
            .collect(),
        created_at: now,
        updated_at: now,
    }
}

/// Löst die Zelle einer Ebene auf.
///
/// # Argumente
/// - `cell` (`Option<(&RawClanSpec, &RawCellSpec)>`): Clan und Zelle aus der
///   Organisation; `None`, wenn keine gefunden wurde.
/// - `root` (`&Path`): Workspace-Wurzel.
/// - `crates` (`&[&CrateNode]`): die Crates dieser Ebene.
///
/// # Rückgabe
/// `Some(cell_plan)`, wenn die Zelle mindestens ein Mitglied auswählt; sonst
/// `None`.
///
/// # Rückfall
/// Ein Auflösungsfehler (leeres `members_from_plan`) und eine Zelle ohne
/// Treffer werden protokolliert und zu `None` — der Aufrufer fährt dann die
/// ungeteilte Welle.
///
/// # Nebenläufigkeit
/// Rein bis auf die Systemuhr in [`wave_plan`].
fn cell_plan_for_wave(
    cell: Option<(&RawClanSpec, &RawCellSpec)>,
    root: &Path,
    crates: &[&CrateNode],
) -> Option<CellPlan> {
    let (clan, spec) = cell?;
    let plan = wave_plan(root, crates);
    match CellPlan::from_cell(spec, Some(clan), &plan) {
        Ok(resolved) if !resolved.members.is_empty() => Some(resolved),
        Ok(resolved) => {
            tracing::warn!(
                cell = resolved.cell_id.as_str(),
                clan = clan.id.as_str(),
                pattern = spec.members_from_plan.as_str(),
                crates = crates.len(),
                "analyze.cell.no_members"
            );
            None
        }
        Err(error) => {
            tracing::warn!(
                cell = spec.id.as_str(),
                clan = clan.id.as_str(),
                error = %error,
                "analyze.cell.unresolved"
            );
            None
        }
    }
}

/// Übersetzt die Batches einer aufgelösten Zelle in Crate-Gruppen.
///
/// # Beschreibung
/// Bildet jede `TaskId` eines Batches auf ihr Crate zurück. Die Zelle darf die
/// Ebene umsortieren und aufteilen, aber nichts verschlucken: deckt sie nicht
/// **jedes** Crate der Ebene ab, wird das Ergebnis verworfen und die ungeteilte
/// Welle zurückgegeben. Sonst würde eine zu enge Zelle stillschweigend Crates
/// von der Analyse ausschließen — ein Rückschritt gegenüber dem Verhalten ohne
/// Organisation.
///
/// # Argumente
/// - `cell` (`Option<&CellPlan>`): die aufgelöste Zelle dieser Ebene.
/// - `crates` (`&[&CrateNode]`): die Crates der Ebene in Graph-Reihenfolge.
///
/// # Rückgabe
/// Die Batches in Ausführungsreihenfolge; im Rückfall genau ein Batch mit allen
/// Crates.
///
/// # Nebenläufigkeit
/// Rein funktional, keine Seiteneffekte.
fn wave_batches<'a>(cell: Option<&CellPlan>, crates: &[&'a CrateNode]) -> Vec<Vec<&'a CrateNode>> {
    let Some(cell) = cell else {
        return vec![crates.to_vec()];
    };

    let mut batches: Vec<Vec<&'a CrateNode>> = Vec::with_capacity(cell.batches.len());
    let mut covered = 0_usize;
    for batch in &cell.batches {
        let mut members: Vec<&'a CrateNode> = Vec::with_capacity(batch.len());
        for task in batch {
            match crates
                .iter()
                .find(|crate_node| node_id(&crate_node.name) == task.as_str())
            {
                Some(crate_node) => {
                    members.push(*crate_node);
                    covered += 1;
                }
                None => tracing::warn!(
                    cell = cell.cell_id.as_str(),
                    task = task.as_str(),
                    "analyze.cell.member_without_crate"
                ),
            }
        }
        if !members.is_empty() {
            batches.push(members);
        }
    }

    if batches.is_empty() || covered != crates.len() {
        tracing::warn!(
            cell = cell.cell_id.as_str(),
            covered,
            expected = crates.len(),
            "analyze.cell.partial_cover — Rückfall auf die ungeteilte Welle"
        );
        return vec![crates.to_vec()];
    }
    batches
}

/// Baut die Welle einer Ebene aus Zelle oder Rückfall.
///
/// # Argumente
/// - `cell` (`Option<(&RawClanSpec, &RawCellSpec)>`): Clan und Zelle aus der Organisation.
/// - `root` (`&Path`): Workspace-Wurzel.
/// - `crates` (`&[&CrateNode]`): die Crates dieser Ebene.
///
/// # Rückgabe
/// Der [`WavePlan`] dieser Ebene; ohne Zelle ein einziger Batch mit
/// [`JoinSemantics::AllTerminal`].
///
/// # Nebenläufigkeit
/// Rein bis auf die Systemuhr in [`wave_plan`].
fn plan_wave<'a>(
    cell: Option<(&RawClanSpec, &RawCellSpec)>,
    root: &Path,
    crates: &[&'a CrateNode],
) -> WavePlan<'a> {
    let resolved = cell_plan_for_wave(cell, root, crates);
    WavePlan {
        batches: wave_batches(resolved.as_ref(), crates),
        join: resolved
            .as_ref()
            .map_or(JoinSemantics::AllTerminal, |plan| plan.join),
        cell_id: resolved.map(|plan| plan.cell_id),
    }
}

/// Beschreibt die verwendete Zelle für die Ausgabe.
///
/// # Rückgabe
/// Ein JSON-Objekt mit Organisation, Clan, Zelle, Muster und Barriere, oder
/// [`Value::Null`], wenn keine Zelle gefunden wurde — die Ausgabe sagt damit
/// ausdrücklich, ob der Rückfall gegriffen hat.
fn cell_json(cell: Option<(&RawClanSpec, &RawCellSpec)>) -> Value {
    let Some((clan, spec)) = cell else {
        return Value::Null;
    };
    let barrier = match serde_json::to_value(spec.barrier) {
        Ok(value) => value,
        Err(_) => Value::Null,
    };
    json!({
        "organization": DEFAULT_ORGANIZATION_ID,
        "clan": clan.id,
        "cell": spec.id,
        "plan_scope": clan.plan_scope,
        "members_from_plan": spec.members_from_plan,
        "barrier": barrier,
    })
}

// ── Operation ────────────────────────────────────────────────────────────────

/// Analysiert einen Workspace bottom-up über Analyst-Kindagenten.
///
/// # Beschreibung
/// Siehe Modul-Dokumentation für den vollständigen Ablauf. Die Operation trägt
/// **kein** `agent_tool`-Attribut, weil sie selbst orchestriert, und ist als
/// Command `tui_only`, weil ihre Ausgabe eine Wellen-Übersicht ist, die in einem
/// Chat-Kanal nicht sinnvoll gerendert werden kann.
///
/// # Argumente
/// - `ctx` (`&OpContext`): Ausführungskontext mit Sandbox, Plan-Store,
///   Finding-Store und Agent-Spawner.
/// - `args` ([`AnalyzeArgs`]): die Argumente; werden konsumiert.
///
/// # Rückgabe
/// `Ok(OpOutput)` mit einem JSON-Bericht: Wellen, Crate-Anzahl, Findings,
/// offene Fragen, Reconcile-Vorschläge und Fehlschläge je Knoten.
///
/// # Fehler
/// - [`OpError::InvalidArguments`]: siehe [`AnalyzeArgs::from_raw_args`].
/// - [`OpError::NotAvailable`]: kein Plan-Store oder kein Agent-Spawner.
/// - [`OpError::Execution`]: Workspace-Graph nicht ladbar oder Plan-Mutation
///   abgelehnt.
///
/// # Nebenläufigkeit
/// Die Kinder einer Welle laufen nebenläufig, gedeckelt durch `max_parallel`;
/// die Wellen selbst laufen strikt nacheinander.
///
/// # Beispiel
/// ```rust,no_run
/// // Aufruf erfolgt über Operation::run().
/// ```
#[operation(
    name = "analyze",
    summary = "Analysiert den Workspace bottom-up über read-only Analyst-Kindagenten.",
    domain = "agents",
    permission = "operator",
    command(path = "/analyze", visibility = "tui_only"),
    model_tool(readonly, approval = "none"),
    // Web-Fläche übernimmt dieselbe Achse wie das ModelTool: bottom-up-Analyse
    // über read-only Analyst-Kindagenten, keine Mutation.
    web(path = "/api/analyze", readonly, approval = "none")
)]
async fn analyze(ctx: &OpContext, args: AnalyzeArgs) -> Result<OpOutput, OpError> {
    let bottom_up = args.bottom_up.unwrap_or(true);
    let dry_run = args.dry_run.unwrap_or(false);
    let max_parallel = args.max_parallel.unwrap_or(DEFAULT_MAX_PARALLEL).max(1);

    let workspace_root = ctx.sandbox().workspace().canonical_root();
    let full_graph = WorkspaceGraph::load(workspace_root).map_err(|error| {
        OpError::Execution(format!(
            "Workspace-Graph konnte nicht geladen werden: {error}"
        ))
    })?;
    let graph = match args.crate_name.as_deref() {
        Some(name) => full_graph.subgraph(name).map_err(|error| {
            OpError::Execution(format!("Teilgraph für '{name}' nicht bildbar: {error}"))
        })?,
        None => full_graph,
    };

    let levels = graph
        .topological_levels()
        .map_err(|error| OpError::Execution(format!("Ebenen nicht berechenbar: {error}")))?;
    if levels.is_empty() {
        return Err(OpError::Execution(
            "der Workspace enthält kein analysierbares Crate".to_owned(),
        ));
    }

    let root = graph.root.as_path();
    let leaf_first: Vec<String> = levels
        .iter()
        .flatten()
        .map(|crate_node| node_id(&crate_node.name))
        .collect();
    let crate_count = leaf_first.len();
    let rendered = graph
        .render_levels()
        .map_err(|error| OpError::Execution(format!("Ebenen nicht darstellbar: {error}")))?;

    // Die Wellensteuerung kommt aus der eingebauten Organisation; jede Stufe
    // fällt einzeln auf das bisherige Verhalten zurück (siehe Modul-Doku).
    let organization = load_organization();
    let cell = organization
        .as_ref()
        .and_then(|organization| clan_cell(organization, RESEARCH_CLAN_ID));
    let waves: Vec<WavePlan<'_>> = levels
        .iter()
        .map(|crates| plan_wave(cell, root, crates))
        .collect();

    let waves_json: Vec<Value> = levels
        .iter()
        .zip(waves.iter())
        .enumerate()
        .map(|(level, (crates, wave))| wave_json(level, crates, bottom_up, &wave.batches))
        .collect();

    if dry_run {
        let report = json!({
            "dry_run": true,
            "root": root.display().to_string(),
            "bottom_up": bottom_up,
            "crate_count": crate_count,
            "cell": cell_json(cell),
            "waves": waves_json,
            "leaf_first": leaf_first,
            "levels": rendered,
            "synthesis": { "id": SYNTHESIS_NODE_ID, "dependencies": leaf_first },
        });
        return render(&report);
    }

    // ── Plan aufbauen ────────────────────────────────────────────────────────
    let plan = require_service!(ctx.plan_store(), "Plan-Store");
    ensure_plan(plan.as_ref())?;

    // Immer leaf-first anlegen: `AddNode` akzeptiert keine unbekannte Dependency.
    let mut created = 0_usize;
    for crates in &levels {
        for crate_node in crates {
            let dependencies: Vec<TaskId> = if bottom_up {
                crate_node
                    .deps
                    .iter()
                    .filter(|dep| graph.get(dep.as_str()).is_some())
                    .map(|dep| TaskId::new(node_id(dep.as_str())))
                    .collect()
            } else {
                Vec::new()
            };
            if ensure_node(plan.as_ref(), analysis_node(root, crate_node, dependencies))? {
                created += 1;
            }
        }
    }

    // ── Wellen fahren ────────────────────────────────────────────────────────
    let budget = parse_budget_hint(ANALYST_BUDGET)?;
    let mut wave_order: Vec<&WavePlan<'_>> = waves.iter().collect();
    if !bottom_up {
        wave_order.reverse();
    }

    let mut completed: Vec<String> = Vec::new();
    let mut failures: Vec<Value> = Vec::new();
    let mut proposals: Vec<Value> = Vec::new();
    let mut open_questions: Vec<String> = Vec::new();
    let mut finding_count = 0_usize;

    for (position, wave) in wave_order.iter().enumerate() {
        let mut wave_findings: Vec<ResearchFinding> = Vec::new();

        // Die Batches laufen nacheinander: bei `write_partition = "required"`
        // ist genau das die Trennung, die die Zelle zusagt.
        for (batch_index, batch) in wave.batches.iter().enumerate() {
            if batch.is_empty() {
                continue;
            }

            let questions: Vec<Value> = batch
                .iter()
                .map(|crate_node| {
                    let consumers = graph.consumers_of(&crate_node.name);
                    child_payload(&analysis_question(root, crate_node, &consumers))
                })
                .collect::<Result<Vec<Value>, OpError>>()?;

            tracing::info!(
                wave = position,
                batch = batch_index,
                batches = wave.batches.len(),
                cell = wave.cell_id.as_deref().unwrap_or("<rückfall>"),
                crates = questions.len(),
                max_parallel,
                "analyze.wave.start"
            );

            let results = fanout_children(
                ctx,
                role_names::ANALYST,
                &questions,
                READ_ONLY_REDUCER,
                budget,
                max_parallel,
                wave.join,
                ChildReturnContract::ResearchFinding,
            )
            .await?;

            for (crate_node, outcome) in batch.iter().zip(results) {
                let id = node_id(&crate_node.name);
                match outcome {
                    Ok(value) => match finding_from_value(value) {
                        Ok(finding) => {
                            open_questions.extend(finding.unresolved_questions.iter().cloned());
                            // Der Borrow von `id` muss vor dem `match` enden, sonst
                            // dürfte kein Arm ihn verschieben (E0505).
                            let recorded =
                                persist_finding(ctx, Some(id.as_str()), &finding, ACTOR_ANALYZE)
                                    .and_then(|_| advance_to_completed(plan.as_ref(), id.as_str()));
                            match recorded {
                                Ok(()) => completed.push(id),
                                Err(error) => {
                                    failures
                                        .push(json!({ "node": id, "error": error.to_string() }));
                                }
                            }
                            wave_findings.push(finding);
                        }
                        Err(error) => {
                            failures.push(json!({ "node": id, "error": error.to_string() }));
                        }
                    },
                    Err(error) => failures.push(json!({ "node": id, "error": error })),
                }
            }
        }

        finding_count += wave_findings.len();
        proposals.extend(reconcile_proposals(ctx, plan.as_ref(), &wave_findings)?);
        tracing::info!(
            wave = position,
            findings = wave_findings.len(),
            failures = failures.len(),
            "analyze.wave.done"
        );
    }

    // ── Synthesis ────────────────────────────────────────────────────────────
    let synthesis_dependencies: Vec<TaskId> = leaf_first
        .iter()
        .map(|id| TaskId::new(id.as_str()))
        .collect();
    let synthesis_created = ensure_node(plan.as_ref(), synthesis_node(synthesis_dependencies))?;

    open_questions.sort();
    open_questions.dedup();

    let report = json!({
        "dry_run": false,
        "root": root.display().to_string(),
        "bottom_up": bottom_up,
        "crate_count": crate_count,
        "nodes_created": created,
        "cell": cell_json(cell),
        "waves": waves_json,
        "leaf_first": leaf_first,
        "levels": rendered,
        "findings": finding_count,
        "completed": completed,
        "failures": failures,
        "open_questions": open_questions,
        "proposals": proposals,
        "synthesis": { "id": SYNTHESIS_NODE_ID, "created": synthesis_created },
    });
    render(&report)
}

/// Serialisiert einen Bericht als [`OpOutput`].
///
/// # Errors
/// - [`OpError::Execution`]: der Bericht ist nicht serialisierbar.
fn render(report: &Value) -> Result<OpOutput, OpError> {
    let text = serde_json::to_string_pretty(report)
        .map_err(|error| OpError::Execution(format!("Bericht nicht serialisierbar: {error}")))?;
    Ok(OpOutput { text })
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{
        AnalyzeArgs, AnalyzeOperation, cell_plan_for_wave, load_organization, node_id,
        parse_max_parallel, plan_wave, wave_batches,
    };
    use crate::testutil::toks;
    use harw_code_graph::CrateNode;
    use harw_core::child_controller::JoinSemantics;
    use harw_operations::context::ServiceMap;
    use harw_operations::{FromRawArgs, OpContext, OpError, Operation, Surface};
    use harw_plan::{
        PathOrSymbol, Plan, PlanId, PlanNode, PlanNodeKind, PlanNodeStatus, RevisionId,
        ScopeMatcher, TaskId,
    };
    use harw_plan_bridge::{CellPlan, offset_from_timestamp};
    use harw_registry_defaults::embedded_agents::{
        DEFAULT_ORGANIZATION_ID, RESEARCH_CLAN_ID, ResolvedOrganization, clan_cell,
        default_organization,
    };
    use harw_sandbox::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Legt ein Mini-Workspace-Fixture an: `b` hängt von `a` ab.
    ///
    /// Erwartete Leaf-first-Reihenfolge: `a`, dann `b`.
    fn mini_workspace(dir: &std::path::Path) {
        let write = |path: PathBuf, content: &str| {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("Verzeichnis anlegen");
            }
            std::fs::write(path, content).expect("Manifest schreiben");
        };
        write(
            dir.join("Cargo.toml"),
            "[workspace]\nmembers = [\"a\", \"b\"]\n",
        );
        write(
            dir.join("a/Cargo.toml"),
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
        );
        write(
            dir.join("b/Cargo.toml"),
            "[package]\nname = \"b\"\nversion = \"0.1.0\"\n\n[dependencies]\na = { path = \"../a\" }\n",
        );
    }

    /// Baut einen [`OpContext`], dessen Sandbox auf ein Mini-Workspace zeigt.
    fn workspace_context() -> (OpContext, PathBuf) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-analyze-test-{}-{id}", std::process::id()));
        let workspace = root.join("ws");
        std::fs::create_dir_all(&workspace).expect("Test-Workspace anlegen");
        mini_workspace(&workspace);

        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: PathBuf::from("ws"),
            }],
        )
        .expect("Workspace-Registry bauen");
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .expect("Workspace-Binding auflösen");
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());
        (
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, ServiceMap::new()),
            root,
        )
    }

    #[test]
    fn test_analyze_args_defaults_are_all_none() {
        let args = AnalyzeArgs::default();
        assert!(args.crate_name.is_none());
        assert!(args.bottom_up.is_none());
        assert!(args.dry_run.is_none());
        assert!(args.max_parallel.is_none());
    }

    #[test]
    fn test_analyze_args_from_raw_args_takes_first_free_token_as_crate() {
        match AnalyzeArgs::from_raw_args(&toks(&["harw-core"])) {
            Ok(args) => assert_eq!(args.crate_name.as_deref(), Some("harw-core")),
            Err(error) => panic!("unerwarteter Fehler: {error}"),
        }
    }

    #[test]
    fn test_analyze_args_from_raw_args_parses_flags_in_any_order() {
        match AnalyzeArgs::from_raw_args(&toks(&["--dry-run", "harw-core", "--top-down"])) {
            Ok(args) => {
                assert_eq!(args.dry_run, Some(true));
                assert_eq!(args.bottom_up, Some(false));
                assert_eq!(args.crate_name.as_deref(), Some("harw-core"));
            }
            Err(error) => panic!("unerwarteter Fehler: {error}"),
        }
    }

    #[test]
    fn test_analyze_args_from_raw_args_parses_max_parallel_both_forms() {
        match AnalyzeArgs::from_raw_args(&toks(&["--max-parallel", "8"])) {
            Ok(args) => assert_eq!(args.max_parallel, Some(8)),
            Err(error) => panic!("unerwarteter Fehler: {error}"),
        }
        match AnalyzeArgs::from_raw_args(&toks(&["--max-parallel=3"])) {
            Ok(args) => assert_eq!(args.max_parallel, Some(3)),
            Err(error) => panic!("unerwarteter Fehler: {error}"),
        }
    }

    #[test]
    fn test_analyze_args_from_raw_args_rejects_unknown_flag() {
        assert!(matches!(
            AnalyzeArgs::from_raw_args(&toks(&["--dry-runn"])),
            Err(OpError::InvalidArguments(_))
        ));
    }

    #[test]
    fn test_analyze_args_from_raw_args_rejects_second_crate_name() {
        assert!(matches!(
            AnalyzeArgs::from_raw_args(&toks(&["a", "b"])),
            Err(OpError::InvalidArguments(_))
        ));
    }

    #[test]
    fn test_parse_max_parallel_rejects_zero_and_non_numbers() {
        assert!(matches!(
            parse_max_parallel("0"),
            Err(OpError::InvalidArguments(_))
        ));
        assert!(matches!(
            parse_max_parallel("viele"),
            Err(OpError::InvalidArguments(_))
        ));
    }

    #[test]
    fn test_analyze_declares_no_agent_tool_surface() {
        let meta = AnalyzeOperation.meta();
        assert_eq!(meta.name, "analyze");
        assert!(
            !meta
                .surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::AgentTool { .. })),
            "/analyze orchestriert selbst und darf keine AgentTool-Fläche tragen"
        );
        assert!(meta.surfaces.iter().any(|surface| matches!(
            surface,
            Surface::Command {
                path,
                visibility: harw_operations::CommandVisibility::TuiOnly
            } if *path == "/analyze"
        )));
    }

    #[tokio::test]
    async fn test_analyze_dry_run_lists_nodes_in_leaf_first_order() {
        let (ctx, root) = workspace_context();
        let result = super::analyze(
            &ctx,
            AnalyzeArgs {
                dry_run: Some(true),
                ..AnalyzeArgs::default()
            },
        )
        .await;
        std::fs::remove_dir_all(root).ok();

        let output = match result {
            Ok(output) => output,
            Err(error) => panic!("Dry-Run darf nicht fehlschlagen: {error}"),
        };
        let report: serde_json::Value = match serde_json::from_str(&output.text) {
            Ok(value) => value,
            Err(error) => panic!("Dry-Run-Ausgabe ist kein JSON: {error}"),
        };

        assert_eq!(report["dry_run"], serde_json::json!(true));
        assert_eq!(report["crate_count"], serde_json::json!(2));
        assert_eq!(
            report["leaf_first"],
            serde_json::json!([node_id("a"), node_id("b")]),
            "Blätter müssen vor ihren Konsumenten stehen"
        );
        assert_eq!(
            report["waves"][0]["nodes"][0]["crate"],
            serde_json::json!("a")
        );
        assert_eq!(
            report["waves"][1]["nodes"][0]["dependencies"],
            serde_json::json!([node_id("a")]),
            "b muss von a abhängen"
        );
    }

    #[tokio::test]
    async fn test_analyze_dry_run_for_single_crate_uses_subgraph() {
        let (ctx, root) = workspace_context();
        let result = super::analyze(
            &ctx,
            AnalyzeArgs {
                crate_name: Some("a".to_owned()),
                dry_run: Some(true),
                ..AnalyzeArgs::default()
            },
        )
        .await;
        std::fs::remove_dir_all(root).ok();

        let output = match result {
            Ok(output) => output,
            Err(error) => panic!("Dry-Run darf nicht fehlschlagen: {error}"),
        };
        let report: serde_json::Value = match serde_json::from_str(&output.text) {
            Ok(value) => value,
            Err(error) => panic!("Dry-Run-Ausgabe ist kein JSON: {error}"),
        };
        assert_eq!(report["crate_count"], serde_json::json!(1));
        assert_eq!(report["leaf_first"], serde_json::json!([node_id("a")]));
    }

    #[tokio::test]
    async fn test_analyze_without_plan_store_is_not_available() {
        let (ctx, root) = workspace_context();
        let result = super::analyze(&ctx, AnalyzeArgs::default()).await;
        std::fs::remove_dir_all(root).ok();

        assert!(
            matches!(result, Err(OpError::NotAvailable(_))),
            "ohne Plan-Store muss /analyze fail-closed sein, war: {result:?}"
        );
    }

    #[tokio::test]
    async fn test_analyze_on_non_workspace_root_is_execution_error() {
        let (ctx, root) = empty_context();
        let result = super::analyze(
            &ctx,
            AnalyzeArgs {
                dry_run: Some(true),
                ..AnalyzeArgs::default()
            },
        )
        .await;
        std::fs::remove_dir_all(root).ok();

        assert!(
            matches!(result, Err(OpError::Execution(_))),
            "ohne Workspace-Manifest muss der Graph-Ladefehler durchschlagen, war: {result:?}"
        );
    }

    // ── Zell-gesteuerter Fan-out ─────────────────────────────────────────────

    /// Löst die eingebaute Organisation auf; ein Fehler ist ein Defekt der TOML-Datei.
    fn organization() -> ResolvedOrganization {
        match default_organization() {
            Ok(organization) => organization,
            Err(error) => panic!("die eingebaute Organisation muss auflösen: {error}"),
        }
    }

    /// Baut einen Plan-Knoten mit gegebener ID und Schreibbereich.
    fn plan_node(id: &str, write_scope: &[&str]) -> PlanNode {
        let now = offset_from_timestamp(jiff::Timestamp::now());
        PlanNode {
            id: TaskId::new(id),
            objective: format!("Testknoten {id}"),
            dependencies: Vec::new(),
            input_contracts: Vec::new(),
            output_contracts: Vec::new(),
            read_scope: Vec::new(),
            write_scope: write_scope.iter().map(|p| PathOrSymbol::new(*p)).collect(),
            forbidden_scope: Vec::new(),
            acceptance_criteria: Vec::new(),
            invalidation_conditions: Vec::new(),
            status: PlanNodeStatus::Draft,
            evidence: Vec::new(),
            kind: PlanNodeKind::Analysis,
            wave: None,
            assignment: None,
            parent: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// Baut einen flüchtigen Plan aus den gegebenen Knoten.
    fn plan_with(nodes: Vec<PlanNode>) -> Plan {
        let now = offset_from_timestamp(jiff::Timestamp::now());
        Plan {
            id: PlanId::new("plan-test"),
            revision: RevisionId::new(0),
            parent_revision: None,
            goal_statement: "Test".to_owned(),
            goal_id: None,
            nodes,
            created_at: now,
            updated_at: now,
        }
    }

    /// Baut einen Crate-Knoten unterhalb von `/ws`.
    fn crate_node(name: &str, level: u32) -> CrateNode {
        CrateNode {
            name: name.to_owned(),
            version: "0.1.0".to_owned(),
            manifest_path: PathBuf::from(format!("/ws/{name}/Cargo.toml")),
            dir: PathBuf::from(format!("/ws/{name}")),
            deps: Vec::new(),
            dev_deps: Vec::new(),
            build_deps: Vec::new(),
            external_deps: Vec::new(),
            is_leaf: true,
            level,
        }
    }

    #[test]
    fn test_load_organization_yields_the_embedded_default_organization() {
        match load_organization() {
            Some(organization) => {
                assert_eq!(organization.id.as_string(), DEFAULT_ORGANIZATION_ID)
            }
            None => panic!("die eingebaute Organisation muss ladbar sein"),
        }
    }

    #[test]
    fn test_node_id_falls_into_the_research_clan_scope() {
        let organization = organization();
        let Some((clan, cell)) = clan_cell(&organization, RESEARCH_CLAN_ID) else {
            panic!("der Research-Clan muss eine Zelle haben");
        };
        assert_eq!(node_id("harw-core"), "research-harw-core");
        assert!(
            ScopeMatcher::matches_glob(&clan.plan_scope, &node_id("harw-core")),
            "der Knotenname muss in den plan_scope des Clans fallen"
        );
        assert!(
            ScopeMatcher::matches_glob(&cell.members_from_plan, &node_id("harw-core")),
            "der Knotenname muss auf das Mitgliedermuster der Zelle passen"
        );
    }

    #[test]
    fn test_research_cell_selects_exactly_the_research_nodes() {
        let organization = organization();
        let Some((clan, cell)) = clan_cell(&organization, RESEARCH_CLAN_ID) else {
            panic!("der Research-Clan muss eine Zelle haben");
        };
        let plan = plan_with(vec![
            plan_node("research-a", &[]),
            plan_node("research-b", &[]),
            plan_node("coding-c", &[]),
        ]);

        let resolved = match CellPlan::from_cell(cell, Some(clan), &plan) {
            Ok(resolved) => resolved,
            Err(error) => panic!("die Zelle muss auflösen: {error}"),
        };

        assert_eq!(
            resolved.members,
            vec![TaskId::new("research-a"), TaskId::new("research-b")],
            "der Coding-Knoten gehört einem anderen Clan"
        );
        assert_eq!(resolved.role, RESEARCH_CLAN_ID);
        assert_eq!(resolved.join, JoinSemantics::AllTerminal);
    }

    #[test]
    fn test_required_write_partition_splits_overlapping_write_scopes() {
        let organization = organization();
        let Some((clan, cell)) = clan_cell(&organization, RESEARCH_CLAN_ID) else {
            panic!("der Research-Clan muss eine Zelle haben");
        };

        let overlapping = plan_with(vec![
            plan_node("research-a", &["src/shared.rs"]),
            plan_node("research-b", &["src/shared.rs"]),
        ]);
        let resolved = match CellPlan::from_cell(cell, Some(clan), &overlapping) {
            Ok(resolved) => resolved,
            Err(error) => panic!("die Zelle muss auflösen: {error}"),
        };
        assert_eq!(
            resolved.batches.len(),
            2,
            "gleicher Schreibpfad darf nicht gleichzeitig laufen: {:?}",
            resolved.batches
        );

        let disjoint = plan_with(vec![
            plan_node("research-a", &["src/a.rs"]),
            plan_node("research-b", &["src/b.rs"]),
        ]);
        let resolved = match CellPlan::from_cell(cell, Some(clan), &disjoint) {
            Ok(resolved) => resolved,
            Err(error) => panic!("die Zelle muss auflösen: {error}"),
        };
        assert_eq!(resolved.batches.len(), 1, "disjunkte Pfade laufen zusammen");
        assert_eq!(resolved.batches[0].len(), 2);
    }

    #[test]
    fn test_wave_batches_without_a_cell_is_one_batch_in_graph_order() {
        let crates = [crate_node("a", 0), crate_node("b", 0)];
        let level: Vec<&CrateNode> = crates.iter().collect();

        let batches = wave_batches(None, &level);

        assert_eq!(batches.len(), 1, "ohne Zelle bleibt die Welle ungeteilt");
        let names: Vec<&str> = batches[0].iter().map(|node| node.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn test_wave_batches_follows_the_cell_batch_order() {
        let crates = [crate_node("a", 0), crate_node("b", 0)];
        let level: Vec<&CrateNode> = crates.iter().collect();
        let cell = CellPlan {
            cell_id: "research-wave".to_owned(),
            members: vec![TaskId::new(node_id("b")), TaskId::new(node_id("a"))],
            batches: vec![
                vec![TaskId::new(node_id("b"))],
                vec![TaskId::new(node_id("a"))],
            ],
            join: JoinSemantics::AllTerminal,
            role: RESEARCH_CLAN_ID.to_owned(),
        };

        let batches = wave_batches(Some(&cell), &level);

        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0][0].name, "b");
        assert_eq!(batches[1][0].name, "a");
    }

    #[test]
    fn test_wave_batches_falls_back_when_the_cell_misses_a_crate() {
        // Eine Zelle darf die Ebene aufteilen und umsortieren, aber kein Crate
        // verschlucken: deckt sie nicht alle ab, gilt die ungeteilte Welle.
        let crates = [crate_node("a", 0), crate_node("b", 0)];
        let level: Vec<&CrateNode> = crates.iter().collect();
        let cell = CellPlan {
            cell_id: "research-wave".to_owned(),
            members: vec![TaskId::new(node_id("a"))],
            batches: vec![vec![TaskId::new(node_id("a"))]],
            join: JoinSemantics::AllTerminal,
            role: RESEARCH_CLAN_ID.to_owned(),
        };

        let batches = wave_batches(Some(&cell), &level);

        assert_eq!(batches.len(), 1);
        let names: Vec<&str> = batches[0].iter().map(|node| node.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"], "kein Crate darf verlorengehen");
    }

    #[test]
    fn test_plan_wave_without_a_cell_keeps_the_previous_behaviour() {
        // Der Rückfall: ohne Clan/Zelle läuft die Welle wie vor der
        // Organisation — ein Batch, `AllTerminal`, kein Fehler.
        let crates = [crate_node("a", 0), crate_node("b", 0)];
        let level: Vec<&CrateNode> = crates.iter().collect();

        assert!(cell_plan_for_wave(None, Path::new("/ws"), &level).is_none());

        let wave = plan_wave(None, Path::new("/ws"), &level);
        assert_eq!(wave.batches.len(), 1);
        assert_eq!(wave.batches[0].len(), 2);
        assert_eq!(wave.join, JoinSemantics::AllTerminal);
        assert!(wave.cell_id.is_none());
    }

    #[test]
    fn test_plan_wave_with_the_research_cell_covers_every_crate() {
        let organization = organization();
        let cell = clan_cell(&organization, RESEARCH_CLAN_ID);
        let crates = [crate_node("a", 0), crate_node("b", 0)];
        let level: Vec<&CrateNode> = crates.iter().collect();

        let wave = plan_wave(cell, Path::new("/ws"), &level);

        assert_eq!(
            wave.cell_id.as_deref(),
            Some("research-wave"),
            "die Welle muss aus der Zelle stammen, nicht aus dem Rückfall"
        );
        assert_eq!(wave.join, JoinSemantics::AllTerminal);
        // Analyse-Knoten schreiben nichts; `required` findet also keinen
        // Konflikt und lässt die Ebene in einem Batch.
        assert_eq!(wave.batches.len(), 1);
        let names: Vec<&str> = wave.batches[0]
            .iter()
            .map(|node| node.name.as_str())
            .collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn test_analyze_dry_run_reports_the_cell_and_its_batches() {
        // Der Kontext hat keinen Agent-Spawner: würde der Dry-Run ein Kind
        // starten, käme `OpError::NotAvailable` zurück. `Ok` ist damit der
        // Beweis, dass kein Kind gestartet wurde.
        let (ctx, root) = workspace_context();
        let result = super::analyze(
            &ctx,
            AnalyzeArgs {
                dry_run: Some(true),
                ..AnalyzeArgs::default()
            },
        )
        .await;
        std::fs::remove_dir_all(root).ok();

        let output = match result {
            Ok(output) => output,
            Err(error) => panic!("Dry-Run darf nicht fehlschlagen: {error}"),
        };
        let report: serde_json::Value = match serde_json::from_str(&output.text) {
            Ok(value) => value,
            Err(error) => panic!("Dry-Run-Ausgabe ist kein JSON: {error}"),
        };

        assert_eq!(report["dry_run"], serde_json::json!(true));
        assert_eq!(report["cell"]["clan"], serde_json::json!(RESEARCH_CLAN_ID));
        assert_eq!(report["cell"]["cell"], serde_json::json!("research-wave"));
        assert_eq!(
            report["cell"]["organization"],
            serde_json::json!(DEFAULT_ORGANIZATION_ID)
        );
        // Der vollständige Wellenplan bleibt erhalten und trägt jetzt zusätzlich
        // die Batches der Zelle.
        assert_eq!(report["crate_count"], serde_json::json!(2));
        assert_eq!(
            report["waves"][0]["batches"],
            serde_json::json!([[node_id("a")]])
        );
        assert_eq!(
            report["waves"][1]["batches"],
            serde_json::json!([[node_id("b")]])
        );
        assert_eq!(
            report["leaf_first"],
            serde_json::json!([node_id("a"), node_id("b")])
        );
    }

    /// Kontext ohne Workspace-Manifest — für den Fehlerpfad des Graph-Ladens.
    fn empty_context() -> (OpContext, PathBuf) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "harw-analyze-empty-test-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("ws")).expect("Test-Workspace anlegen");
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: PathBuf::from("ws"),
            }],
        )
        .expect("Workspace-Registry bauen");
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .expect("Workspace-Binding auflösen");
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());
        (
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, ServiceMap::new()),
            root,
        )
    }
}
