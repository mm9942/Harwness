//! `/analyze` — Bottom-up-Analyse eines Arbeitsbereichs über Analyst-Kindagenten.
//!
//! # Verantwortungsbereich
//! Implementiert die `analyze`-Operation gemäß AP W4-04. Sie ist die einzige
//! Operation dieses APs, die **selbst orchestriert**: sie baut den
//! Einheiten-Graphen des Arbeitsbereichs, legt je Analyse-Einheit einen
//! `Analysis`-Plan-Knoten an, fährt die Ebenen von den Blättern aufwärts als
//! Fan-out-Wellen und verdichtet das Ergebnis in einem `Synthesis`-Knoten.
//!
//! Deshalb trägt sie **kein** `agent_tool`-Attribut: eine Operation, die selbst
//! Kinder startet, darf nicht zusätzlich als einzelnes Kind-Werkzeug erscheinen
//! — das Modell würde sonst eine Orchestrierung für einen Einzelaufruf halten.
//!
//! # Einheiten statt Crates
//! `/analyze` ist verzeichnis- und projektneutral. Eine **Einheit** ist ein von
//! [`harw_explorer`] erkanntes Projekt (Cargo-Crate, npm/pnpm-Paket,
//! Python-Projekt, Go-Modul, Dokumentsammlung — beliebig verschachtelt); die
//! Kanten zwischen Einheiten stammen aus den Explorer-Relationen
//! (Pfad-Abhängigkeiten, namentliche Abhängigkeiten, Dokument-Links). Reine
//! Workspace-Hüllen (virtuelles `Cargo.toml`, `package.json` mit `workspaces`)
//! und Git-Wurzeln sind selbst keine Einheit — ihre Mitglieder sind es.
//!
//! [`WorkspaceGraph`] ist nur noch eine **Anreicherung** für Cargo: für jeden
//! erkannten Cargo-Workspace wird er geladen und liefert Version, externe
//! Abhängigkeiten und die präzisen internen Kanten (nur `[dependencies]`, ohne
//! Dev-/Build-Abhängigkeiten). Kanten zwischen zwei so angereicherten Crates
//! kommen dann ausschließlich aus dem [`WorkspaceGraph`]. Scheitert das Laden,
//! bleibt es bei den Explorer-Kanten; der Fehler steht im Bericht
//! (`graph.cargo_enrichment`), `/analyze` scheitert daran nicht.
//!
//! Kanten werden in fester Reihenfolge eingefügt (Cargo-Anreicherung,
//! Explorer-Abhängigkeiten, Dokument-Links) und nur, wenn sie keinen Zyklus
//! schließen — verworfene Kanten zählt `graph.dropped_edges`. Der Graph ist
//! dadurch immer azyklisch und in Ebenen zerlegbar.
//!
//! # Ablauf
//! 1. [`build_unit_graph`] auf der kanonischen Sandbox-Wurzel.
//! 2. Optional [`UnitGraph::subgraph`], wenn eine Einheit genannt ist (Name,
//!    Cargo-Crate-Name oder relativer Pfad).
//! 3. Je Einheit ein [`PlanNode`] (`kind = Analysis`, `read_scope = <dir>/**`),
//!    dessen Abhängigkeiten die Kanten der Einheit sind —
//!    [`UnitGraph::levels`] liefert genau diese Ordnung, Ebene 0 sind die
//!    Blätter.
//! 4. Je Ebene wird die Zelle des Clans [`RESEARCH_CLAN_ID`] der eingebauten
//!    Organisation über [`CellPlan::from_cell`] aufgelöst; ihre Batches sind die
//!    Startgruppen der Welle (siehe „Zell-gesteuerter Fan-out" unten).
//! 5. Bei `dry_run` endet die Operation hier und gibt den Plan **samt Batches**
//!    aus.
//! 6. Sonst je Batch ein [`fanout_children`]-Lauf mit der Rolle
//!    [`role_names::ANALYST`] (aus einer UIA-Sitzung
//!    [`role_names::UIA_EXPLORER`], siehe [`analyst_role_for`]); Findings
//!    werden abgelegt, als Evidenz angehängt und die Knoten auf `Completed`
//!    gefahren. Nach jeder Welle läuft [`PlanController::reconcile`]; seine
//!    Vorschläge gehen in die Ausgabe. Der Bericht trägt `status`/`notice`
//!    ([`completion_status`]).
//! 7. Zum Schluss ein `Synthesis`-Knoten, der von allen Analyse-Knoten abhängt.
//!
//! # Zell-gesteuerter Fan-out
//! Welche Knoten gemeinsam starten dürfen, steht nicht hier, sondern in
//! `harw-registry-defaults/agents/organization/default.toml`: die Zelle
//! `research-wave` des Clans `research` trägt Muster, Barriere und
//! Schreibtrennung. [`CellPlan::from_cell`] löst sie gegen den Plan der Ebene
//! auf, `write_partition = "required"` zerlegt sie in Batches mit paarweise
//! disjunkten Schreibbereichen, und [`CellPlan::join`] liefert die
//! [`JoinSemantics`] der Welle. Die Batches laufen **nacheinander**, ihre
//! Mitglieder nebenläufig — genau das bedeutet eine erzwungene Schreibtrennung.
//!
//! Deshalb tragen die Plan-Knoten den Clan im Namen (`research-<einheit>`,
//! siehe [`node_id`]): [`CellPlan::from_cell`] wählt Mitglieder über einen Glob
//! gegen die `TaskId` **und** den `write_scope`; Analyse-Knoten haben keinen
//! `write_scope`, also entscheidet allein die `TaskId`.
//!
//! # Rückfall — `/analyze` darf daran nicht scheitern
//! `/analyze` ist der einzige Ende-zu-Ende-Pfad der Planungsfläche. Jede Stufe
//! der Zell-Auflösung fällt deshalb auf das bisherige Verhalten zurück, statt
//! einen Fehler zu erzeugen: eine nicht ladbare Organisation, ein fehlender
//! Clan, eine fehlende Zelle, ein Muster ohne Treffer, ein Auflösungsfehler und
//! sogar eine Zelle, die nur einen *Teil* der Ebene auswählt, führen alle zu
//! einer einzigen Welle mit allen Einheiten der Ebene in Graph-Reihenfolge
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
//! # Ohne erkanntes Projekt
//! Findet der Explorer keine Einheit (oder scheitert er),
//! baut [`synthesize_directory_graph`] einen Verzeichnis-Graphen: eine Einheit
//! je direktem Unterverzeichnis der Wurzel (versteckte Verzeichnisse und eine
//! feste Rauschliste wie `target` oder `node_modules` ausgenommen), alle auf
//! Ebene 0 ohne Kanten — `bottom_up` wird dadurch zu einer einzigen Welle.
//! Ohne qualifizierendes Unterverzeichnis entsteht genau eine Einheit für die
//! Wurzel selbst.
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
//!   `--max-parallel`, zweiter Einheiten-Name.
//! - [`OpError::NotAvailable`]: kein Plan-Store (nur im Nicht-Dry-Run) oder
//!   kein Agent-Spawner im Kontext.
//! - [`OpError::Execution`]: die Wurzel ist nicht lesbar, die genannte Einheit
//!   existiert nicht, oder eine Plan-Mutation wurde abgelehnt.
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
//! assert_eq!(args.unit_name(), Some("harw-core"));
//! ```

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use harw_agent_dsl::roles::AgentRoleId;
use harw_code_graph::WorkspaceGraph;
use harw_core::child_controller::JoinSemantics;
use harw_core_bridge::{ChildReturnContract, OpContextCoreExt, fanout_children, parse_budget_hint};
use harw_explorer::{ExplorerIndex, ExplorerOptions, ProjectKind, RelationKind};
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
/// [`crate::explore`], weil ein Analyst eine ganze Einheit lesen muss.
const ANALYST_BUDGET: &str = "90k_tokens,60_tool_calls,300s";

/// Kennzeichen der Spawn-Ablehnung aus `ManagedAgentSpawner::admit`
/// (`harw-core/src/child_controller.rs`): die aufrufende Sitzung darf die
/// Kind-Rolle laut Spawn-Matrix nicht starten.
const NO_DELEGATION_MARKER: &str = "no delegation capability";

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
const ANALYSIS_STOP_CONDITION: &str = "Alle fünf Punkte sind für diese Einheit beantwortet oder \
     ausdrücklich als offen markiert. Kein Blick über die Grenze der Einheit hinaus außer für die \
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
/// - `crate_name` (`Option<String>`): einzelne Analyse-Einheit (Projektname,
///   Cargo-Crate-Name oder relativer Pfad); ohne Angabe der ganze
///   Arbeitsbereich. Der Feldname bleibt aus Kompatibilitätsgründen
///   `crate_name` (JSON-Schema, Web-/Modell-Fläche); auf den JSON-Flächen wird
///   zusätzlich `unit_name` angenommen. Intern immer über
///   [`AnalyzeArgs::unit_name`] lesen.
/// - `bottom_up` (`Option<bool>`): von den Blättern aufwärts (Vorgabe: `true`).
/// - `dry_run` (`Option<bool>`): nur den Plan erzeugen, keine Kinder starten.
/// - `max_parallel` (`Option<usize>`): Obergrenze gleichzeitiger Kinder je
///   Welle (Vorgabe: 4).
///
/// # Spec-Referenz
/// AP W4-04 — `/analyze`.
#[derive(Debug, Default, serde::Deserialize, harw_macros::OpArgs)]
pub struct AnalyzeArgs {
    /// Einzelne Analyse-Einheit (Projektname, Crate-Name oder relativer Pfad);
    /// ohne Angabe der ganze Arbeitsbereich. Auch als `unit_name` annehmbar.
    #[serde(default, alias = "unit_name")]
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

impl AnalyzeArgs {
    /// Liefert den Namen der gewählten Analyse-Einheit.
    ///
    /// # Beschreibung
    /// Das serialisierte Feld heißt aus Kompatibilitätsgründen weiterhin
    /// `crate_name`; gemeint ist aber jede Einheit, nicht nur ein Cargo-Crate.
    ///
    /// # Rückgabe
    /// `Some(name)`, wenn eine Einheit genannt ist, sonst `None`.
    #[must_use]
    pub fn unit_name(&self) -> Option<&str> {
        self.crate_name.as_deref()
    }
}

impl FromRawArgs for AnalyzeArgs {
    /// Parst Einheiten-Name und Flags aus der Command-Zeile.
    ///
    /// # Beschreibung
    /// Erkannt werden `--dry-run` / `--no-dry-run`, `--bottom-up` /
    /// `--top-down` (alias `--no-bottom-up`) sowie `--max-parallel <n>` und
    /// `--max-parallel=<n>`. Das erste flag-freie Token ist der Name der
    /// Analyse-Einheit.
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
    ///   ungültiger `--max-parallel`-Wert, zweiter Einheiten-Name.
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
                             Einheiten-Namen"
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

// ── Einheiten-Graph ──────────────────────────────────────────────────────────

/// Verzeichnisnamen, die im Verzeichnis-Rückfall nie als Einheit zählen.
///
/// Feste Ausschlussliste für gängige Build-/Abhängigkeits-Artefakte, die im
/// Wurzelverzeichnis liegen können. Versteckte Verzeichnisse (führendes `.`,
/// siehe [`synthesize_directory_graph`]) deckt diese Liste bewusst nicht ab —
/// dafür reicht der Namenstest allein.
const SYNTHETIC_EXCLUDED_DIRS: [&str; 6] = [
    "target",
    "node_modules",
    "dist",
    "build",
    "vendor",
    "__pycache__",
];

/// Projektarten, die selbst eine Analyse-Einheit bilden, in Vorrang-Reihenfolge.
///
/// Die erste vorhandene Art einer Projektwurzel bestimmt Name und Etikett der
/// Einheit. `CargoWorkspace` (Hülle) und `Git` (Repository-Grenze) fehlen
/// bewusst: sie beschreiben Gruppierungen, keinen analysierbaren Inhalt.
const UNIT_KIND_PRIORITY: [ProjectKind; 5] = [
    ProjectKind::CargoCrate,
    ProjectKind::Node,
    ProjectKind::Python,
    ProjectKind::Go,
    ProjectKind::Documents,
];

/// Eine Analyse-Einheit: ein Projekt oder (im Rückfall) ein Verzeichnis.
///
/// # Beschreibung
/// Sprach- und ökosystemneutrale Entsprechung des früheren Crate-Knotens.
/// `deps` enthält die **Namen** anderer Einheiten desselben Graphen, von denen
/// diese Einheit abhängt; der Graph ist per Konstruktion azyklisch.
///
/// # Nebenläufigkeit
/// Reiner Werttyp.
#[derive(Debug, Clone)]
struct AnalysisUnit {
    /// Eindeutiger Anzeigename (Manifest-Name, sonst Ordnername; bei
    /// Kollision um den relativen Pfad ergänzt).
    name: String,
    /// Erkannte Projektarten dieser Wurzel in [`UNIT_KIND_PRIORITY`]-Ordnung;
    /// leer für eine reine Verzeichnis-Einheit.
    kinds: Vec<ProjectKind>,
    /// Pfad relativ zur Wurzel (leer = die Wurzel selbst).
    rel: PathBuf,
    /// Absolutes Verzeichnis der Einheit.
    dir: PathBuf,
    /// Manifest relativ zur Wurzel, falls vorhanden.
    manifest: Option<PathBuf>,
    /// `[package].name`, wenn die Einheit ein Cargo-Crate ist.
    cargo_name: Option<String>,
    /// Version aus der Cargo-Anreicherung, falls bekannt.
    version: Option<String>,
    /// Namen der Einheiten, von denen diese abhängt.
    deps: Vec<String>,
    /// Externe Abhängigkeiten aus der Cargo-Anreicherung.
    external_deps: Vec<String>,
    /// Ebene von unten: `0` für Blätter, sonst `1 + max(Ebene der deps)`.
    level: u32,
}

impl AnalysisUnit {
    /// Etikett der vorrangigen Projektart (`cargo-crate`, `node`, …) oder
    /// `directory` für eine reine Verzeichnis-Einheit.
    fn kind_label(&self) -> &'static str {
        self.kinds.first().map_or("directory", |kind| kind.label())
    }

    /// Relativer Pfad für Anzeige und Prompt (`.` für die Wurzel).
    fn rel_display(&self) -> String {
        if self.rel.as_os_str().is_empty() {
            ".".to_owned()
        } else {
            path_key(&self.rel)
        }
    }
}

/// Der Einheiten-Graph eines Arbeitsbereichs.
///
/// # Beschreibung
/// Ersetzt den früheren Cargo-only [`WorkspaceGraph`] als Arbeitsstruktur von
/// `/analyze`. Die Ebenen sind nach [`assign_levels`] in jeder Einheit
/// gespeichert und mit ihren `deps` konsistent.
#[derive(Debug, Clone)]
struct UnitGraph {
    /// Wurzel des Arbeitsbereichs.
    root: PathBuf,
    /// Alle Einheiten.
    units: Vec<AnalysisUnit>,
}

impl UnitGraph {
    /// Sucht eine Einheit anhand ihres Namens.
    fn get(&self, name: &str) -> Option<&AnalysisUnit> {
        self.units.iter().find(|unit| unit.name == name)
    }

    /// Sucht eine Einheit über Namen, Cargo-Crate-Namen oder relativen Pfad.
    fn find(&self, query: &str) -> Option<&AnalysisUnit> {
        let trimmed = query.trim().trim_end_matches('/');
        let path_query = trimmed.strip_prefix("./").unwrap_or(trimmed);
        self.get(query)
            .or_else(|| {
                self.units
                    .iter()
                    .find(|unit| unit.cargo_name.as_deref() == Some(query))
            })
            .or_else(|| {
                self.units
                    .iter()
                    .find(|unit| unit.rel_display() == path_query)
            })
    }

    /// Alle Einheiten, die `name` direkt (über `deps`) benutzen, nach Name
    /// sortiert.
    fn consumers_of(&self, name: &str) -> Vec<&AnalysisUnit> {
        let mut consumers: Vec<&AnalysisUnit> = self
            .units
            .iter()
            .filter(|unit| unit.deps.iter().any(|dep| dep == name))
            .collect();
        consumers.sort_by(|a, b| a.name.cmp(&b.name));
        consumers
    }

    /// Einheiten, die verzeichnismäßig **innerhalb** von `unit` liegen und
    /// eigenständig analysiert werden, nach Name sortiert.
    fn nested_in(&self, unit: &AnalysisUnit) -> Vec<&AnalysisUnit> {
        let mut nested: Vec<&AnalysisUnit> = self
            .units
            .iter()
            .filter(|other| other.rel != unit.rel && other.rel.starts_with(&unit.rel))
            .collect();
        nested.sort_by(|a, b| a.name.cmp(&b.name));
        nested
    }

    /// Teilgraph aus der Einheit `query` und allen transitiven Abhängigkeiten.
    ///
    /// # Errors
    /// - [`OpError::Execution`]: keine Einheit passt auf `query`.
    fn subgraph(&self, query: &str) -> Result<Self, OpError> {
        let Some(start) = self.find(query) else {
            let known: Vec<&str> = self.units.iter().map(|unit| unit.name.as_str()).collect();
            return Err(OpError::Execution(format!(
                "Teilgraph für '{query}' nicht bildbar: keine Einheit dieses Namens oder Pfads \
                 (bekannt: {})",
                known.join(", ")
            )));
        };
        let mut included: BTreeSet<String> = BTreeSet::new();
        let mut stack: Vec<String> = vec![start.name.clone()];
        while let Some(current) = stack.pop() {
            if !included.insert(current.clone()) {
                continue;
            }
            if let Some(unit) = self.get(&current) {
                stack.extend(
                    unit.deps
                        .iter()
                        .filter(|dep| !included.contains(*dep))
                        .cloned(),
                );
            }
        }
        let mut units: Vec<AnalysisUnit> = self
            .units
            .iter()
            .filter(|unit| included.contains(&unit.name))
            .cloned()
            .collect();
        assign_levels(&mut units);
        Ok(Self {
            root: self.root.clone(),
            units,
        })
    }

    /// Gruppiert alle Einheiten in Ebenen von unten (Index `0` = Blätter),
    /// innerhalb einer Ebene nach Name sortiert.
    fn levels(&self) -> Vec<Vec<&AnalysisUnit>> {
        let Some(max_level) = self.units.iter().map(|unit| unit.level).max() else {
            return Vec::new();
        };
        let mut levels: Vec<Vec<&AnalysisUnit>> = (0..=max_level).map(|_| Vec::new()).collect();
        for unit in &self.units {
            if let Some(bucket) = levels.get_mut(unit.level as usize) {
                bucket.push(unit);
            }
        }
        for bucket in &mut levels {
            bucket.sort_by(|a, b| a.name.cmp(&b.name));
        }
        levels.retain(|bucket| !bucket.is_empty());
        levels
    }

    /// Kompakte Textprojektion (`Ebene N: a, b`) für Prompts und Berichte.
    fn render_levels(&self) -> String {
        let mut out = String::new();
        for (index, bucket) in self.levels().iter().enumerate() {
            let names: Vec<&str> = bucket.iter().map(|unit| unit.name.as_str()).collect();
            out.push_str(&format!("Ebene {index}: {}\n", names.join(", ")));
        }
        out
    }
}

/// Berechnet die Ebenen aller Einheiten und hält `deps` konsistent.
///
/// # Beschreibung
/// Unbekannte Abhängigkeitsnamen werden entfernt. Sollte trotz azyklischer
/// Konstruktion ein Zyklus übrig sein, werden die offenen Kanten der ersten
/// blockierten Einheit verworfen (protokolliert) — so passen gespeicherte
/// Ebenen und `deps` immer zusammen, und `harw_plan` sieht nie eine Kante auf
/// einen später angelegten Knoten.
fn assign_levels(units: &mut [AnalysisUnit]) {
    let index: HashMap<String, usize> = units
        .iter()
        .enumerate()
        .map(|(position, unit)| (unit.name.clone(), position))
        .collect();
    for unit in units.iter_mut() {
        unit.deps.retain(|dep| index.contains_key(dep));
    }

    let mut levels: Vec<Option<u32>> = vec![None; units.len()];
    loop {
        let mut progressed = false;
        let mut pending = false;
        for position in 0..units.len() {
            if levels[position].is_some() {
                continue;
            }
            let mut ready = true;
            let mut max_dep: Option<u32> = None;
            for dep in &units[position].deps {
                match index.get(dep).and_then(|&other| levels[other]) {
                    Some(level) => max_dep = Some(max_dep.map_or(level, |max| max.max(level))),
                    None => {
                        ready = false;
                        break;
                    }
                }
            }
            if ready {
                levels[position] = Some(max_dep.map_or(0, |max| max + 1));
                progressed = true;
            } else {
                pending = true;
            }
        }
        if !pending {
            break;
        }
        if !progressed {
            if let Some(blocked) = levels.iter().position(Option::is_none) {
                tracing::warn!(
                    unit = units[blocked].name.as_str(),
                    "analyze.graph.cycle — offene Kanten verworfen"
                );
                let resolved: BTreeSet<String> = index
                    .iter()
                    .filter(|&(_, &other)| levels[other].is_some())
                    .map(|(name, _)| name.clone())
                    .collect();
                units[blocked].deps.retain(|dep| resolved.contains(dep));
            }
        }
    }
    for (unit, level) in units.iter_mut().zip(levels) {
        unit.level = level.unwrap_or(0);
    }
}

/// Kanten-Sammler, der nur azyklische Kanten zulässt.
///
/// `deps[i]` sind die Indizes der Einheiten, von denen Einheit `i` abhängt.
struct EdgeSet {
    deps: Vec<BTreeSet<usize>>,
    dropped: usize,
}

impl EdgeSet {
    /// Leerer Sammler für `count` Einheiten.
    fn new(count: usize) -> Self {
        Self {
            deps: vec![BTreeSet::new(); count],
            dropped: 0,
        }
    }

    /// `true`, wenn `target` von `start` aus über Kanten erreichbar ist.
    fn reaches(&self, start: usize, target: usize) -> bool {
        let mut visited = vec![false; self.deps.len()];
        let mut stack = vec![start];
        while let Some(current) = stack.pop() {
            if current == target {
                return true;
            }
            if visited.get(current).copied().unwrap_or(true) {
                continue;
            }
            visited[current] = true;
            if let Some(next) = self.deps.get(current) {
                stack.extend(next.iter().copied());
            }
        }
        false
    }

    /// Fügt `from → to` hinzu, sofern die Kante neu ist und keinen Zyklus
    /// schließt; eine zyklusschließende Kante wird gezählt und verworfen.
    fn add(&mut self, from: usize, to: usize) {
        if from == to || from >= self.deps.len() || to >= self.deps.len() {
            return;
        }
        if self.deps[from].contains(&to) {
            return;
        }
        if self.reaches(to, from) {
            self.dropped += 1;
            return;
        }
        self.deps[from].insert(to);
    }
}

/// Pfad als `/`-getrennte Zeichenkette (plattformunabhängig).
fn path_key(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Die innerste Einheit, deren Verzeichnis `path` enthält.
fn innermost_unit(units: &[AnalysisUnit], path: &Path) -> Option<usize> {
    units
        .iter()
        .enumerate()
        .filter(|(_, unit)| path.starts_with(&unit.rel))
        .max_by_key(|(_, unit)| unit.rel.components().count())
        .map(|(position, _)| position)
}

/// Baut die Einheiten aus den Projekten eines Explorer-Index.
///
/// # Beschreibung
/// Projekte werden nach Wurzel gruppiert. Eine Wurzel wird zur Einheit, wenn
/// sie mindestens eine Art aus [`UNIT_KIND_PRIORITY`] trägt; ein
/// `package.json` mit aufgelösten `workspaces`-Mitgliedern gilt dabei als
/// Hülle (wie ein virtuelles `Cargo.toml`) und zählt nicht. Namen werden
/// eindeutig gemacht — auch über [`node_id`], damit keine zwei Einheiten auf
/// denselben Plan-Knoten fallen.
fn units_from_explorer(root: &Path, index: &ExplorerIndex) -> Vec<AnalysisUnit> {
    let mut roots: Vec<&Path> = index
        .projects
        .iter()
        .map(|project| project.root.as_path())
        .collect();
    roots.sort();
    roots.dedup();

    let mut units: Vec<AnalysisUnit> = Vec::new();
    let mut taken: BTreeSet<String> = BTreeSet::new();
    for rel in roots {
        let at_root: Vec<&harw_explorer::Project> = index
            .projects
            .iter()
            .filter(|project| project.root.as_path() == rel)
            .filter(|project| project.kind != ProjectKind::Node || project.members.is_empty())
            .collect();
        let kinds: Vec<ProjectKind> = UNIT_KIND_PRIORITY
            .iter()
            .copied()
            .filter(|kind| at_root.iter().any(|project| project.kind == *kind))
            .collect();
        let Some(primary_kind) = kinds.first().copied() else {
            continue;
        };
        let Some(primary) = at_root.iter().find(|project| project.kind == primary_kind) else {
            continue;
        };
        let cargo_name = at_root
            .iter()
            .find(|project| project.kind == ProjectKind::CargoCrate)
            .map(|project| project.name.clone());

        let mut name = primary.name.clone();
        if taken.contains(&node_id(&name)) {
            name = format!("{}@{}", primary.name, path_key(rel));
        }
        let mut suffix = 2_usize;
        while taken.contains(&node_id(&name)) {
            name = format!("{}@{}-{suffix}", primary.name, path_key(rel));
            suffix += 1;
        }
        taken.insert(node_id(&name));

        units.push(AnalysisUnit {
            name,
            kinds,
            rel: rel.to_path_buf(),
            dir: if rel.as_os_str().is_empty() {
                root.to_path_buf()
            } else {
                root.join(rel)
            },
            manifest: primary.manifest.clone(),
            cargo_name,
            version: None,
            deps: Vec::new(),
            external_deps: Vec::new(),
            level: 0,
        });
    }
    units
}

/// Reichert Cargo-Einheiten über [`WorkspaceGraph`] an.
///
/// # Beschreibung
/// Lädt je erkanntem Cargo-Workspace den [`WorkspaceGraph`] und überträgt
/// Version und externe Abhängigkeiten auf die passenden Einheiten (gleicher
/// Crate-Name, Verzeichnis unterhalb des Workspace). Die internen
/// `[dependencies]` gehen als Kanten in `edges`.
///
/// # Rückgabe
/// `(angereichert, bericht)`: je Einheit, ob sie aus einem erfolgreich
/// geladenen Workspace stammt, und je Workspace ein JSON-Eintrag mit Anzahl
/// oder Fehler. Ein Ladefehler ist **kein** Fehler von `/analyze`.
fn enrich_with_cargo(
    root: &Path,
    index: &ExplorerIndex,
    units: &mut [AnalysisUnit],
    edges: &mut EdgeSet,
) -> (Vec<bool>, Vec<Value>) {
    let mut enriched = vec![false; units.len()];
    let mut report: Vec<Value> = Vec::new();
    for workspace in index
        .projects
        .iter()
        .filter(|project| project.kind == ProjectKind::CargoWorkspace)
    {
        let ws_dir = if workspace.root.as_os_str().is_empty() {
            root.to_path_buf()
        } else {
            root.join(&workspace.root)
        };
        let ws_label = if workspace.root.as_os_str().is_empty() {
            ".".to_owned()
        } else {
            path_key(&workspace.root)
        };
        let graph = match WorkspaceGraph::load(&ws_dir) {
            Ok(graph) => graph,
            Err(error) => {
                tracing::warn!(
                    workspace = ws_label.as_str(),
                    error = %error,
                    "analyze.cargo_enrichment.failed"
                );
                report.push(json!({ "workspace": ws_label, "error": error.to_string() }));
                continue;
            }
        };
        let mut matched = 0_usize;
        for crate_node in &graph.crates {
            let Some(position) = cargo_unit(units, &ws_dir, &crate_node.name) else {
                continue;
            };
            matched += 1;
            enriched[position] = true;
            if crate_node.version != "0.0.0" {
                units[position].version = Some(crate_node.version.clone());
            }
            units[position].external_deps = crate_node.external_deps.clone();
            for dep in &crate_node.deps {
                if let Some(target) = cargo_unit(units, &ws_dir, dep) {
                    edges.add(position, target);
                }
            }
        }
        report.push(json!({ "workspace": ws_label, "crates": matched }));
    }
    (enriched, report)
}

/// Index der Cargo-Einheit `crate_name` unterhalb von `ws_dir`.
fn cargo_unit(units: &[AnalysisUnit], ws_dir: &Path, crate_name: &str) -> Option<usize> {
    units.iter().position(|unit| {
        unit.cargo_name.as_deref() == Some(crate_name) && unit.dir.starts_with(ws_dir)
    })
}

/// Baut den Einheiten-Graphen aus einem Explorer-Index.
///
/// # Beschreibung
/// Kanten in fester Reihenfolge, jeweils nur azyklisch ([`EdgeSet::add`]):
/// 1. Cargo-Anreicherung ([`enrich_with_cargo`]),
/// 2. Explorer-Pfad- und Namensabhängigkeiten — zwischen zwei angereicherten
///    Cargo-Einheiten übersprungen, weil der [`WorkspaceGraph`] dort präziser
///    ist (keine Dev-Abhängigkeiten),
/// 3. Dokument-Links zwischen verschiedenen Einheiten.
///
/// Endpunkte einer Relation werden auf die innerste enthaltende Einheit
/// abgebildet; Mitgliedschaft und Verschachtelung sind Enthaltensein, keine
/// Abhängigkeit, und erzeugen keine Kante.
///
/// # Rückgabe
/// Graph plus JSON-Beschreibung seiner Herkunft; `None`, wenn der Index keine
/// einzige Einheit ergibt.
fn unit_graph_from_explorer(root: &Path, index: &ExplorerIndex) -> Option<(UnitGraph, Value)> {
    let mut units = units_from_explorer(root, index);
    if units.is_empty() {
        return None;
    }
    let mut edges = EdgeSet::new(units.len());
    let (enriched, cargo_report) = enrich_with_cargo(root, index, &mut units, &mut edges);

    let dependency_kinds = [RelationKind::PathDependency, RelationKind::CrateDependency];
    let doc_kinds = [RelationKind::DocLink];
    for pass in [&dependency_kinds[..], &doc_kinds[..]] {
        for relation in index
            .relations
            .iter()
            .filter(|relation| pass.contains(&relation.kind))
        {
            let (Some(from), Some(to)) = (
                innermost_unit(&units, &relation.from),
                innermost_unit(&units, &relation.to),
            ) else {
                continue;
            };
            if relation.kind != RelationKind::DocLink && enriched[from] && enriched[to] {
                continue;
            }
            edges.add(from, to);
        }
    }

    let names: Vec<String> = units.iter().map(|unit| unit.name.clone()).collect();
    for (unit, deps) in units.iter_mut().zip(&edges.deps) {
        unit.deps = deps
            .iter()
            .filter_map(|&target| names.get(target).cloned())
            .collect();
    }
    assign_levels(&mut units);

    let info = json!({
        "source": "explorer",
        "projects": index.projects.len(),
        "relations": index.relations.len(),
        "truncated": index.truncated,
        "dropped_edges": edges.dropped,
        "cargo_enrichment": cargo_report,
    });
    Some((
        UnitGraph {
            root: root.to_path_buf(),
            units,
        },
        info,
    ))
}

/// Baut den Einheiten-Graphen der Wurzel: Explorer, sonst Verzeichnis-Rückfall.
///
/// # Beschreibung
/// Läuft [`ExplorerIndex::build`] (unter Beachtung von `.gitignore`) und leitet
/// daraus über [`unit_graph_from_explorer`] den Graphen ab. Scheitert der
/// Explorer oder findet er keine Einheit, greift
/// [`synthesize_directory_graph`].
///
/// # Rückgabe
/// Graph plus JSON-Beschreibung seiner Herkunft (`graph` im Bericht).
///
/// # Errors
/// - [`OpError::Execution`]: auch der Verzeichnis-Rückfall kann die Wurzel
///   nicht lesen.
fn build_unit_graph(root: &Path) -> Result<(UnitGraph, Value), OpError> {
    let explorer_error = match ExplorerIndex::build(root, &ExplorerOptions::default()) {
        Ok(index) => match unit_graph_from_explorer(root, &index) {
            Some(result) => return Ok(result),
            None => None,
        },
        Err(error) => {
            tracing::warn!(error = %error, "analyze.explorer.failed — Verzeichnis-Rückfall");
            Some(error.to_string())
        }
    };
    let graph = synthesize_directory_graph(root)?;
    Ok((
        graph,
        json!({ "source": "directories", "explorer_error": explorer_error }),
    ))
}

/// Baut einen Verzeichnis-Graphen für eine Wurzel ohne erkanntes Projekt.
///
/// # Beschreibung
/// Liest die direkten Unterverzeichnisse von `root` (nicht rekursiv).
/// Übersprungen werden Verzeichnisse mit führendem `.` (deckt `.git`, `.harw`,
/// `.claude`, `.codex`, `.venv` einheitlich ab) sowie
/// [`SYNTHETIC_EXCLUDED_DIRS`]. Für jedes verbleibende Unterverzeichnis
/// entsteht eine Verzeichnis-Einheit auf Ebene 0 ohne Kanten. Gibt es kein
/// qualifizierendes Unterverzeichnis (z. B. ein Ordner mit wenigen losen
/// Dateien), entsteht genau eine Einheit für `root` selbst.
///
/// # Argumente
/// - `root` (`&Path`): die Wurzel; muss als Verzeichnis lesbar sein.
///
/// # Rückgabe
/// Ein [`UnitGraph`] mit mindestens einer Einheit.
///
/// # Errors
/// - [`OpError::Execution`]: `root` ist nicht als Verzeichnis lesbar, oder
///   ein einzelner Verzeichniseintrag ist nicht auflösbar. Kein Panic in
///   beiden Fällen.
fn synthesize_directory_graph(root: &Path) -> Result<UnitGraph, OpError> {
    let entries = fs::read_dir(root).map_err(|error| {
        OpError::Execution(format!(
            "Verzeichnis '{}' ist nicht als Verzeichnis lesbar: {error}",
            root.display()
        ))
    })?;

    let mut subdirs: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            OpError::Execution(format!(
                "Verzeichniseintrag unterhalb von '{}' nicht lesbar: {error}",
                root.display()
            ))
        })?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name.starts_with('.') || SYNTHETIC_EXCLUDED_DIRS.contains(&name) {
            continue;
        }
        subdirs.push(path);
    }
    subdirs.sort();

    let units: Vec<AnalysisUnit> = if subdirs.is_empty() {
        vec![directory_unit(root, root.to_path_buf())]
    } else {
        subdirs
            .into_iter()
            .map(|dir| directory_unit(root, dir))
            .collect()
    };

    Ok(UnitGraph {
        root: root.to_path_buf(),
        units,
    })
}

/// Baut eine einzelne Verzeichnis-Einheit für [`synthesize_directory_graph`].
///
/// # Beschreibung
/// `name` ist der Basisname von `dir`, Rückfall `"project"`, falls er nicht
/// ermittelbar ist (z. B. Wurzelpfad `/`). `kinds` bleibt leer — das markiert
/// die Einheit als reines Verzeichnis, ausgewertet in [`analysis_question`].
fn directory_unit(root: &Path, dir: PathBuf) -> AnalysisUnit {
    let name = dir
        .file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| "project".to_owned());
    let rel = dir
        .strip_prefix(root)
        .map(Path::to_path_buf)
        .unwrap_or_default();
    AnalysisUnit {
        name,
        kinds: Vec::new(),
        rel,
        dir,
        manifest: None,
        cargo_name: None,
        version: None,
        deps: Vec::new(),
        external_deps: Vec::new(),
        level: 0,
    }
}

// ── Plan-Bausteine ───────────────────────────────────────────────────────────

/// Bildet den Plan-Knoten-Bezeichner einer Einheit.
///
/// # Beschreibung
/// Der Bezeichner trägt den Clan, dem der Knoten gehört: `research-<einheit>`.
/// Das ist keine Kosmetik, sondern die Bedingung dafür, dass die Zelle des
/// Research-Clans ihn überhaupt finden kann — `CellPlan::from_cell` wählt
/// Mitglieder über einen Glob gegen die `TaskId` und den `write_scope`, und ein
/// Analyse-Knoten hat keinen `write_scope`. Der Präfix kommt deshalb aus
/// [`RESEARCH_CLAN_ID`] und nicht aus einem Literal: ändert sich die Clan-ID der
/// eingebauten Organisation, ändern sich die Knotennamen mit.
///
/// Einheiten-Namen sind nicht mehr auf Crate-Namen beschränkt (`@scope/pkg`,
/// Pfade, Leerzeichen); jedes Zeichen außer ASCII-Alphanumerik, `-`, `_` und
/// `.` wird deshalb zu `-`, damit kein `/` den Glob der Zelle bricht. Für
/// Crate-Namen ist die Abbildung die Identität.
fn node_id(unit_name: &str) -> String {
    let slug: String = unit_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '-'
            }
        })
        .collect();
    format!("{RESEARCH_CLAN_ID}-{slug}")
}

/// Bildet den Lesebereich einer Einheit relativ zur Wurzel.
///
/// Die Wurzel-Einheit selbst liest `**`. Fällt auf den Namen zurück, wenn das
/// Verzeichnis nicht unterhalb der Wurzel liegt — dann ist der Name die
/// einzige Kennung, die der Knoten hat.
fn read_scope_for(root: &Path, unit: &AnalysisUnit) -> String {
    match unit.dir.strip_prefix(root) {
        Ok(relative) if relative.as_os_str().is_empty() => "**".to_owned(),
        Ok(relative) => format!("{}/**", path_key(relative)),
        Err(_) => format!("{}/**", unit.name),
    }
}

/// Baut den `Analysis`-Knoten einer Einheit.
///
/// `created_at`/`updated_at` werden vom Plan-Store überschrieben (Design-Doc
/// §7); die hier gesetzten Werte sind nur Platzhalter für den Typ.
fn analysis_node(root: &Path, unit: &AnalysisUnit, dependencies: Vec<TaskId>) -> PlanNode {
    let now = offset_from_timestamp(jiff::Timestamp::now());
    PlanNode {
        id: TaskId::new(node_id(&unit.name)),
        objective: format!("Bottom-up-Analyse von {}", unit.name),
        dependencies,
        input_contracts: Vec::new(),
        output_contracts: Vec::new(),
        read_scope: vec![PathOrSymbol::new(read_scope_for(root, unit))],
        write_scope: Vec::new(),
        forbidden_scope: Vec::new(),
        acceptance_criteria: Vec::new(),
        invalidation_conditions: Vec::new(),
        status: PlanNodeStatus::Draft,
        evidence: Vec::new(),
        kind: PlanNodeKind::Analysis,
        wave: Some(unit.level),
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
        objective: "Verdichtung der Einheiten-Analysen zu einem Gesamtbild".to_owned(),
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

/// Sprachspezifische Hinweise für den Analyse-Prompt.
///
/// Jede Angabe ist ein kurzer Einschub, den [`analysis_question`] in die
/// sprachneutral formulierten fünf Punkte einsetzt.
struct LanguageHints {
    /// Sprache/Ökosystem für die Anzeige.
    language: &'static str,
    /// Was hier als öffentliche Oberfläche zählt.
    api: &'static str,
    /// Sprachübliche Platzhalter- und Stub-Marker.
    markers: &'static str,
    /// Sprachübliche Doku-Kommentare.
    docs: &'static str,
}

/// Liefert die Hinweise zu einer Projektart; `None` für Dokumentsammlungen
/// und Arten ohne eigene Sprache.
fn language_hints(kind: ProjectKind) -> Option<LanguageHints> {
    match kind {
        ProjectKind::CargoCrate | ProjectKind::CargoWorkspace => Some(LanguageHints {
            language: "Rust",
            api: "jedes `pub`-Item",
            markers: "`todo!()`, `unimplemented!()`",
            docs: "`//!`/`///`",
        }),
        ProjectKind::Node => Some(LanguageHints {
            language: "JavaScript/TypeScript",
            api: "jede `export`-Deklaration sowie die Einstiegspunkte aus `package.json` \
                  (`main`, `exports`, `bin`)",
            markers: "`throw new Error(\"not implemented\")` und ähnliche Platzhalter-Würfe",
            docs: "JSDoc/TSDoc `/** … */`",
        }),
        ProjectKind::Python => Some(LanguageHints {
            language: "Python",
            api: "jeder Name auf Modulebene ohne führenden Unterstrich bzw. laut `__all__`, \
                  sowie Kommandozeilen-Einstiegspunkte",
            markers: "`raise NotImplementedError`, Rümpfe aus nur `pass` oder `...`",
            docs: "Docstrings",
        }),
        ProjectKind::Go => Some(LanguageHints {
            language: "Go",
            api: "jeder exportierte (großgeschriebene) Bezeichner",
            markers: "`panic(\"not implemented\")` und ähnliche Platzhalter",
            docs: "Kommentare direkt vor Deklarationen und `doc.go`",
        }),
        ProjectKind::Git | ProjectKind::Documents => None,
    }
}

/// Baut die gebundene Frage an das Analyst-Kind einer Einheit.
///
/// # Beschreibung
/// Die Frage ist absichtlich nummeriert und abschließend: das Kind soll nicht
/// „die Einheit anschauen", sondern fünf benannte Dinge liefern. Der Text ist
/// sprachneutral; die sprachüblichen Begriffe (öffentliche Oberfläche,
/// Stub-Marker, Doku-Kommentare) kommen aus [`language_hints`] der erkannten
/// Projektarten, für reine Verzeichnisse aus einer allgemeinen Aufzählung. Eine
/// Dokumentsammlung bekommt Punkte, die auf Dokumente passen. Der Scope
/// begrenzt das Kind auf das Verzeichnis der Einheit; eigenständig analysierte
/// verschachtelte Einheiten werden ausdrücklich ausgenommen, und die
/// Konsumentenliste steht schon in der Frage.
fn analysis_question(
    root: &Path,
    unit: &AnalysisUnit,
    consumers: &[&AnalysisUnit],
    nested: &[&AnalysisUnit],
) -> ResearchQuestion {
    let consumer_names: Vec<&str> = consumers.iter().map(|other| other.name.as_str()).collect();
    let consumer_hint = if consumer_names.is_empty() {
        "Keine andere Einheit des Arbeitsbereichs benutzt sie (Stand Graph).".to_owned()
    } else {
        format!(
            "Laut Graph benutzen sie: {}. Prüfe für jede, was sie tatsächlich davon verwendet.",
            consumer_names.join(", ")
        )
    };
    let nested_hint = if nested.is_empty() {
        String::new()
    } else {
        let entries: Vec<String> = nested
            .iter()
            .map(|other| format!("`{}` ({})", other.rel_display(), other.name))
            .collect();
        format!(
            "Verschachtelte Einheiten werden eigenständig analysiert und gehören nicht zu \
             dieser Analyse: {}.\n",
            entries.join(", ")
        )
    };

    let hints: Vec<LanguageHints> = unit
        .kinds
        .iter()
        .copied()
        .filter_map(language_hints)
        .collect();
    let is_documents = hints.is_empty() && unit.kinds.contains(&ProjectKind::Documents);
    let description = match unit.kinds.first() {
        Some(_) => {
            let labels: Vec<&str> = unit.kinds.iter().map(|kind| kind.label()).collect();
            format!(
                "Einheit `{}` (Projektart {})",
                unit.name,
                labels.join(" + ")
            )
        }
        None => format!("Verzeichnis `{}`", unit.name),
    };
    let version = unit
        .version
        .as_deref()
        .map(|version| format!("Version {version}, "))
        .unwrap_or_default();
    let manifest = unit
        .manifest
        .as_deref()
        .map(|manifest| format!("Manifest `{}`, ", path_key(manifest)))
        .unwrap_or_default();
    let external = if unit.external_deps.is_empty() {
        String::new()
    } else {
        format!(
            "Externe Abhängigkeiten laut Manifest: {}.\n",
            unit.external_deps.join(", ")
        )
    };
    let header = format!(
        "Analysiere {description} (Pfad `{path}`, {manifest}{version}Ebene {level}) vollständig \
         und liefere genau diese fünf Punkte:\n{external}",
        path = unit.rel_display(),
        level = unit.level,
    );

    let points = if is_documents {
        format!(
            "1. Inhalt: jedes Dokument mit Titel, Zweck und Kernaussagen, gruppiert nach \
                Unterordner.\n\
             2. Konsumenten: welche anderen Einheiten auf diese Dokumente verweisen und wofür. \
                {consumer_hint}\n\
             3. Lücken: jedes `TODO`, `FIXME`, `TBD`, jeder leere oder als Platzhalter \
                markierte Abschnitt.\n\
             4. Querverweise: welche Dokumente aufeinander verweisen und welche Links ins \
                Leere zeigen.\n\
             5. Widersprüche: jede Stelle, an der zwei Dokumente (oder ein Dokument und der \
                Stand des Arbeitsbereichs) einander widersprechen.\n"
        )
    } else {
        let (api, markers, docs) = if hints.is_empty() {
            (
                "alles, was von außen benutzt werden soll — öffentliche bzw. exportierte \
                 Symbole in der jeweiligen Sprache, Einstiegspunkte, Kommandos, Schnittstellen"
                    .to_owned(),
                "sprachübliche Platzhalter (z. B. `todo!()`, `raise NotImplementedError`, \
                 `throw new Error(\"not implemented\")`, `panic(\"not implemented\")`)"
                    .to_owned(),
                "Doku-Kommentare/Docstrings".to_owned(),
            )
        } else {
            let api: Vec<String> = hints
                .iter()
                .map(|hint| format!("{}: {}", hint.language, hint.api))
                .collect();
            let markers: Vec<String> = hints
                .iter()
                .map(|hint| format!("{}: {}", hint.language, hint.markers))
                .collect();
            let docs: Vec<String> = hints
                .iter()
                .map(|hint| format!("{}: {}", hint.language, hint.docs))
                .collect();
            (api.join("; "), markers.join("; "), docs.join("; "))
        };
        format!(
            "1. Öffentliche Oberfläche ({api}) mit Signatur bzw. Fundstelle, gruppiert nach \
                Modul oder Datei, und wofür sie da ist.\n\
             2. Konsumenten: welche anderen Einheiten des Arbeitsbereichs diese benutzen und \
                was sie davon verwenden. {consumer_hint}\n\
             3. Stubs und Lücken: jedes `TODO`, `FIXME`, `XXX`, `HACK`, jeder Platzhalter \
                ({markers}), jede Funktion, die einen Platzhalterwert liefert, und jeder \
                Fehlerfall, der nie ausgelöst wird.\n\
             4. Testabdeckung: welche öffentlichen Teile haben Tests, welche nicht, und welche \
                Tests prüfen nur, dass nichts abstürzt.\n\
             5. Abweichungen zwischen Doku und Verhalten: jede Stelle, an der Dokumentation \
                ({docs}; README und weitere Markdown-Dateien) etwas behauptet, das der Code \
                nicht tut.\n"
        )
    };

    ResearchQuestion {
        id: QuestionId::new(node_id(&unit.name)),
        question: format!(
            "{header}{points}{nested_hint}Belege jede Aussage mit Dateipfad und Zeilenbereich."
        ),
        scope: QuestionScope {
            paths: vec![read_scope_for(root, unit)],
            crates: unit.cargo_name.iter().cloned().collect(),
            urls: Vec::new(),
            sources: vec![SourceClass::LocalSource],
        },
        expected_output: ANALYSIS_EXPECTED_OUTPUT.to_owned(),
        freshness: Freshness::AnyTime,
        stop_condition: ANALYSIS_STOP_CONDITION.to_owned(),
        owner_task: Some(node_id(&unit.name)),
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
    // Runde 5, Teil P: der Store hält mehrere Pläne. Ist kein Plan aktiv
    // (etwa nach `plan archive`), aber `plan-analyze` liegt schon im Store,
    // wird er wieder aktiv statt an `PlanExists` zu scheitern.
    let analysis_id = PlanId::new(ANALYSIS_PLAN_ID);
    if plan.plan_by_id(&analysis_id).is_ok() {
        plan.switch_plan(&analysis_id, ACTOR_ANALYZE)
            .map_err(|error| {
                OpError::Execution(format!("Plan konnte nicht aktiviert werden: {error}"))
            })?;
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
    units: &[&AnalysisUnit],
    dependencies_enabled: bool,
    batches: &[Vec<&AnalysisUnit>],
) -> Value {
    let nodes: Vec<Value> = units
        .iter()
        .map(|unit| {
            let dependencies: Vec<String> = if dependencies_enabled {
                unit.deps.iter().map(|dep| node_id(dep)).collect()
            } else {
                Vec::new()
            };
            json!({
                "id": node_id(&unit.name),
                "crate": unit.name,
                "unit": unit.name,
                "kind": unit.kind_label(),
                "path": unit.rel_display(),
                "level": unit.level,
                "dependencies": dependencies,
            })
        })
        .collect();
    let batches: Vec<Vec<String>> = batches
        .iter()
        .map(|batch| batch.iter().map(|unit| node_id(&unit.name)).collect())
        .collect();
    json!({ "level": level, "nodes": nodes, "batches": batches })
}

// ── Zell-Auflösung ───────────────────────────────────────────────────────────

/// Eine ausführbare Welle: die Batches der Ebene und ihre Join-Semantik.
///
/// # Beschreibung
/// Das Ergebnis der Zell-Auflösung einer Ebene. `batches` laufen nacheinander,
/// die Mitglieder eines Batches nebenläufig. Ohne auflösbare Zelle enthält
/// `batches` genau einen Batch mit allen Einheiten der Ebene und `join` ist
/// [`JoinSemantics::AllTerminal`] — das bisherige Verhalten.
///
/// # Nebenläufigkeit
/// Reiner Werttyp; hält nur Verweise auf den Einheiten-Graphen.
struct WavePlan<'a> {
    /// Die Startgruppen der Welle in Ausführungsreihenfolge.
    batches: Vec<Vec<&'a AnalysisUnit>>,
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
/// Einheiten-Graphen. Dieser Plan enthält genau die Analyse-Knoten *einer* Ebene —
/// dadurch bleibt die Wellenordnung erhalten, die
/// [`UnitGraph::levels`] vorgibt: eine Zelle über dem
/// Gesamtplan würde alle Ebenen zu einer einzigen Welle verschmelzen und die
/// Bottom-up-Ordnung zerstören.
///
/// Die Knoten tragen bewusst **keine** Abhängigkeiten: innerhalb einer Ebene
/// hängt kein Knoten von einem anderen ab, und die Auswahl liest ohnehin nur
/// `id` und `write_scope`. Der Plan wird nirgends persistiert.
///
/// # Argumente
/// - `root` (`&Path`): Wurzel für die Lesebereiche.
/// - `units` (`&[&AnalysisUnit]`): die Einheiten dieser Ebene.
///
/// # Rückgabe
/// Ein flüchtiger [`Plan`] mit einem Analyse-Knoten je Einheit.
///
/// # Nebenläufigkeit
/// Rein bis auf die Systemuhr für die Zeitstempel der Knoten.
fn wave_plan(root: &Path, units: &[&AnalysisUnit]) -> Plan {
    let now = offset_from_timestamp(jiff::Timestamp::now());
    Plan {
        id: PlanId::new(ANALYSIS_PLAN_ID),
        revision: RevisionId::new(0),
        parent_revision: None,
        goal_statement: "Analyse-Welle".to_owned(),
        goal_id: None,
        nodes: units
            .iter()
            .map(|unit| analysis_node(root, unit, Vec::new()))
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
/// - `root` (`&Path`): Wurzel.
/// - `units` (`&[&AnalysisUnit]`): die Einheiten dieser Ebene.
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
    units: &[&AnalysisUnit],
) -> Option<CellPlan> {
    let (clan, spec) = cell?;
    let plan = wave_plan(root, units);
    match CellPlan::from_cell(spec, Some(clan), &plan) {
        Ok(resolved) if !resolved.members.is_empty() => Some(resolved),
        Ok(resolved) => {
            tracing::warn!(
                cell = resolved.cell_id.as_str(),
                clan = clan.id.as_str(),
                pattern = spec.members_from_plan.as_str(),
                units = units.len(),
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

/// Übersetzt die Batches einer aufgelösten Zelle in Einheiten-Gruppen.
///
/// # Beschreibung
/// Bildet jede `TaskId` eines Batches auf ihr Einheit zurück. Die Zelle darf die
/// Ebene umsortieren und aufteilen, aber nichts verschlucken: deckt sie nicht
/// **jedes** Einheit der Ebene ab, wird das Ergebnis verworfen und die ungeteilte
/// Welle zurückgegeben. Sonst würde eine zu enge Zelle stillschweigend Einheiten
/// von der Analyse ausschließen — ein Rückschritt gegenüber dem Verhalten ohne
/// Organisation.
///
/// # Argumente
/// - `cell` (`Option<&CellPlan>`): die aufgelöste Zelle dieser Ebene.
/// - `units` (`&[&AnalysisUnit]`): die Einheiten der Ebene in Graph-Reihenfolge.
///
/// # Rückgabe
/// Die Batches in Ausführungsreihenfolge; im Rückfall genau ein Batch mit allen
/// Einheiten.
///
/// # Nebenläufigkeit
/// Rein funktional, keine Seiteneffekte.
fn wave_batches<'a>(
    cell: Option<&CellPlan>,
    units: &[&'a AnalysisUnit],
) -> Vec<Vec<&'a AnalysisUnit>> {
    let Some(cell) = cell else {
        return vec![units.to_vec()];
    };

    let mut batches: Vec<Vec<&'a AnalysisUnit>> = Vec::with_capacity(cell.batches.len());
    let mut covered = 0_usize;
    for batch in &cell.batches {
        let mut members: Vec<&'a AnalysisUnit> = Vec::with_capacity(batch.len());
        for task in batch {
            match units
                .iter()
                .find(|unit| node_id(&unit.name) == task.as_str())
            {
                Some(unit) => {
                    members.push(*unit);
                    covered += 1;
                }
                None => tracing::warn!(
                    cell = cell.cell_id.as_str(),
                    task = task.as_str(),
                    "analyze.cell.member_without_unit"
                ),
            }
        }
        if !members.is_empty() {
            batches.push(members);
        }
    }

    if batches.is_empty() || covered != units.len() {
        tracing::warn!(
            cell = cell.cell_id.as_str(),
            covered,
            expected = units.len(),
            "analyze.cell.partial_cover — Rückfall auf die ungeteilte Welle"
        );
        return vec![units.to_vec()];
    }
    batches
}

/// Baut die Welle einer Ebene aus Zelle oder Rückfall.
///
/// # Argumente
/// - `cell` (`Option<(&RawClanSpec, &RawCellSpec)>`): Clan und Zelle aus der Organisation.
/// - `root` (`&Path`): Wurzel.
/// - `units` (`&[&AnalysisUnit]`): die Einheiten dieser Ebene.
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
    units: &[&'a AnalysisUnit],
) -> WavePlan<'a> {
    let resolved = cell_plan_for_wave(cell, root, units);
    WavePlan {
        batches: wave_batches(resolved.as_ref(), units),
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

/// Analysiert einen Arbeitsbereich bottom-up über Analyst-Kindagenten.
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
/// `Ok(OpOutput)` mit einem JSON-Bericht: Wellen, Einheiten-Anzahl, Findings,
/// offene Fragen, Reconcile-Vorschläge und Fehlschläge je Knoten.
///
/// # Fehler
/// - [`OpError::InvalidArguments`]: siehe [`AnalyzeArgs::from_raw_args`].
/// - [`OpError::NotAvailable`]: kein Plan-Store oder kein Agent-Spawner.
/// - [`OpError::Execution`]: Wurzel nicht lesbar, genannte Einheit unbekannt
///   oder Plan-Mutation abgelehnt.
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
    // F-031: `analyze` schreibt dauerhaft in den Plan-Store und startet einen
    // Analyst-Kindagenten-Fan-out (siehe unten, Persistenz-/Spawn-Pfad) — das
    // ist eine Mutation, auch wenn der ModelTool-Zweig bewusst `readonly`
    // bleibt (Kindagenten laufen dort mit reduzierter, lesender Autorität).
    // Die Web-Fläche muss diese Mutation als `POST` deklarieren, sonst würde
    // eine GET-Route Nebenwirkungen auslösen dürfen (die Schwachstelle, die
    // dieser Vertragswechsel schließt).
    web(path = "/api/analyze", method = "post", approval = "none")
)]
async fn analyze(ctx: &OpContext, args: AnalyzeArgs) -> Result<OpOutput, OpError> {
    let bottom_up = args.bottom_up.unwrap_or(true);
    let dry_run = args.dry_run.unwrap_or(false);
    let max_parallel = args.max_parallel.unwrap_or(DEFAULT_MAX_PARALLEL).max(1);

    let workspace_root = ctx.sandbox().workspace().canonical_root();
    let (full_graph, graph_info) = build_unit_graph(workspace_root)?;
    let graph = match args.unit_name() {
        Some(unit_name) => full_graph.subgraph(unit_name)?,
        None => full_graph,
    };

    let levels = graph.levels();
    if levels.is_empty() {
        return Err(OpError::Execution(
            "der Arbeitsbereich enthält keine analysierbare Einheit".to_owned(),
        ));
    }

    let root = graph.root.as_path();
    let leaf_first: Vec<String> = levels
        .iter()
        .flatten()
        .map(|unit| node_id(&unit.name))
        .collect();
    let unit_count = leaf_first.len();
    let rendered = graph.render_levels();

    // Die Wellensteuerung kommt aus der eingebauten Organisation; jede Stufe
    // fällt einzeln auf das bisherige Verhalten zurück (siehe Modul-Doku).
    let organization = load_organization();
    let cell = organization
        .as_ref()
        .and_then(|organization| clan_cell(organization, RESEARCH_CLAN_ID));
    let waves: Vec<WavePlan<'_>> = levels
        .iter()
        .map(|units| plan_wave(cell, root, units))
        .collect();

    let waves_json: Vec<Value> = levels
        .iter()
        .zip(waves.iter())
        .enumerate()
        .map(|(level, (units, wave))| wave_json(level, units, bottom_up, &wave.batches))
        .collect();

    if dry_run {
        let report = json!({
            "dry_run": true,
            "root": root.display().to_string(),
            "bottom_up": bottom_up,
            "crate_count": unit_count,
            "unit_count": unit_count,
            "graph": graph_info,
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
    for units in &levels {
        for unit in units {
            let dependencies: Vec<TaskId> = if bottom_up {
                unit.deps
                    .iter()
                    .filter(|dep| graph.get(dep.as_str()).is_some())
                    .map(|dep| TaskId::new(node_id(dep.as_str())))
                    .collect()
            } else {
                Vec::new()
            };
            if ensure_node(plan.as_ref(), analysis_node(root, unit, dependencies))? {
                created += 1;
            }
        }
    }

    // ── Wellen fahren ────────────────────────────────────────────────────────
    let budget = parse_budget_hint(ANALYST_BUDGET)?;
    // Dieselbe Rollenwahl wie `/explore` und `/research*`: eine UIA-Wurzel
    // darf den `Worker` `analyst` nie starten (Spawn-Matrix), siehe
    // [`analyst_role_for`].
    let child_role = analyst_role_for(
        ctx.managed_spawner()
            .and_then(|spawner| spawner.session_organizational_role(ctx.session_id())),
    );
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
                .map(|unit| {
                    let consumers = graph.consumers_of(&unit.name);
                    let nested = graph.nested_in(unit);
                    child_payload(&analysis_question(root, unit, &consumers, &nested))
                })
                .collect::<Result<Vec<Value>, OpError>>()?;

            tracing::info!(
                wave = position,
                batch = batch_index,
                batches = wave.batches.len(),
                cell = wave.cell_id.as_deref().unwrap_or("<rückfall>"),
                units = questions.len(),
                max_parallel,
                "analyze.wave.start"
            );

            let results = fanout_children(
                ctx,
                child_role,
                &questions,
                READ_ONLY_REDUCER,
                budget,
                max_parallel,
                wave.join,
                ChildReturnContract::ResearchFinding,
            )
            .await?;

            for (unit, outcome) in batch.iter().zip(results) {
                let id = node_id(&unit.name);
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

    let (status, notice) = completion_status(unit_count, completed.len(), &failures);
    let report = json!({
        "status": status,
        "notice": notice,
        "child_role": child_role,
        "dry_run": false,
        "root": root.display().to_string(),
        "bottom_up": bottom_up,
        "crate_count": unit_count,
        "unit_count": unit_count,
        "graph": graph_info,
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

/// Wählt die Kind-Rolle der Analyse-Wellen passend zur aufrufenden Sitzung.
///
/// # Beschreibung
/// `analyst` trägt `role = "worker"`. Die Spawn-Matrix
/// (`harw-agent-dsl/src/roles.rs::can_spawn`) lässt eine UIA-Sitzung
/// (`AgentRoleId::UserInterface`, die TUI-Wurzel im Modus `chat`) nie einen
/// `Worker` starten — `analyze` scheiterte dort an jedem Kind mit „no
/// delegation capability is available for this request“, obwohl derselbe
/// Turn `root-orchestrator` spawnen durfte. Wie `/explore` und `/research*`
/// weicht `/analyze` deshalb für UIA-Aufrufer auf
/// [`role_names::UIA_EXPLORER`] aus (`AgentRoleId::UiaWorker`, von der UIA
/// bereits spawnbar, read-only Datei-Werkzeuge). Die Rechte bleiben dabei
/// unverändert: die Kind-Sandbox wird weiterhin über
/// [`READ_ONLY_REDUCER`] aus der Eltern-Sandbox verengt (kein Netz, kein
/// Schreiben, keine Ausführung), und die `uia-worker`-Familie läuft höchstens
/// mit einer gleichzeitigen Instanz. Alle anderen Aufrufer (oder eine nicht
/// ermittelbare Rolle) behalten [`role_names::ANALYST`].
///
/// # Argumente
/// - `caller` (`Option<AgentRoleId>`): Organisationsrolle der aufrufenden
///   Sitzung laut Spawner.
///
/// # Rückgabe
/// Der Registry-Name der Kind-Rolle.
fn analyst_role_for(caller: Option<AgentRoleId>) -> &'static str {
    match caller {
        Some(AgentRoleId::UserInterface) => role_names::UIA_EXPLORER,
        _ => role_names::ANALYST,
    }
}

/// Bewertet, ob der Analyse-Bericht vollständig ist.
///
/// # Beschreibung
/// Ohne diese Kennzeichnung sah ein Bericht, in dem **jedes** Kind gescheitert
/// war, genauso aus wie ein erfolgreicher: Wellen, Ebenen und Plan-Knoten
/// stehen immer darin, weil sie vor dem Fan-out entstehen. Das Modell hielt
/// ihn für ein „Prototype-Ergebnis“. Jetzt trägt der Bericht `status`
/// (`complete`, `partial`, `incomplete`) und bei Lücken einen `notice`, der
/// ausdrücklich sagt, dass Wellen/Ebenen nur der Plan sind — und bei einer
/// Spawn-Ablehnung, warum sie kam und was stattdessen geht.
///
/// # Argumente
/// - `units` (`usize`): Zahl der geplanten Einheiten.
/// - `completed` (`usize`): Zahl der abgeschlossenen Einheiten.
/// - `failures` (`&[Value]`): Fehlschläge je Knoten (`{"node", "error"}`).
///
/// # Rückgabe
/// `(status, notice)`; `notice` ist `None` genau bei `complete`.
fn completion_status(
    units: usize,
    completed: usize,
    failures: &[Value],
) -> (&'static str, Option<String>) {
    if failures.is_empty() && completed >= units {
        return ("complete", None);
    }
    let status = if completed == 0 {
        "incomplete"
    } else {
        "partial"
    };
    let mut notice = format!(
        "UNVOLLSTÄNDIG: nur {completed} von {units} Einheiten wurden analysiert, \
         {} Kind-Läufe sind gescheitert (siehe `failures`). `waves`/`levels` \
         zeigen nur den geplanten Aufbau, keine Analyseergebnisse — den Bericht \
         nicht als fertige Analyse ausgeben.",
        failures.len()
    );
    let delegation_denied = failures.iter().any(|failure| {
        failure
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|error| error.contains(NO_DELEGATION_MARKER))
    });
    if delegation_denied {
        notice.push_str(
            " Grund: diese Sitzung darf die Analyse-Kindrolle laut Spawn-Matrix \
             nicht starten (z. B. ein Kind-Agent ohne Delegationsrecht). \
             Stattdessen: die Einheiten mit den lesenden `fs.*`-Werkzeugen \
             selbst prüfen oder die Analyse von einer Sitzung mit \
             Delegationsrecht (UIA-Wurzel oder `root-orchestrator`) ausführen \
             lassen.",
        );
    }
    (status, Some(notice))
}

/// Serialisiert einen Bericht als [`OpOutput`].
///
/// # Errors
/// - [`OpError::Execution`]: der Bericht ist nicht serialisierbar.
fn render(report: &Value) -> Result<OpOutput, OpError> {
    let text = serde_json::to_string_pretty(report)
        .map_err(|error| OpError::Execution(format!("Bericht nicht serialisierbar: {error}")))?;
    Ok(OpOutput::from(text))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{
        AnalysisUnit, AnalyzeArgs, AnalyzeOperation, analysis_question, build_unit_graph,
        cell_plan_for_wave, directory_unit, load_organization, node_id, parse_max_parallel,
        plan_wave, wave_batches,
    };
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::testutil::toks;
    use harw_authority::{PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
    use harw_core::child_controller::JoinSemantics;
    use harw_explorer::ProjectKind;
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
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Legt ein Mini-Workspace-Fixture an: `b` hängt von `a` ab.
    ///
    /// Erwartete Leaf-first-Reihenfolge: `a`, dann `b`.
    fn mini_workspace(dir: &std::path::Path) -> TestResult {
        let write = |path: PathBuf, content: &str| -> TestResult {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(ctx("Verzeichnis anlegen"))?;
            }
            std::fs::write(path, content).map_err(ctx("Manifest schreiben"))?;
            Ok(())
        };
        write(
            dir.join("Cargo.toml"),
            "[workspace]\nmembers = [\"a\", \"b\"]\n",
        )?;
        write(
            dir.join("a/Cargo.toml"),
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
        )?;
        write(
            dir.join("b/Cargo.toml"),
            "[package]\nname = \"b\"\nversion = \"0.1.0\"\n\n[dependencies]\na = { path = \"../a\" }\n",
        )?;
        Ok(())
    }

    /// Baut einen [`OpContext`], dessen Sandbox auf ein Mini-Workspace zeigt.
    fn workspace_context() -> TestResult<(OpContext, PathBuf)> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-analyze-test-{}-{id}", std::process::id()));
        let workspace = root.join("ws");
        std::fs::create_dir_all(&workspace).map_err(ctx("Test-Workspace anlegen"))?;
        mini_workspace(&workspace)?;

        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("Workspace-Registry bauen"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .map_err(ctx("Workspace-Binding auflösen"))?;
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());
        Ok((
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, ServiceMap::new()),
            root,
        ))
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
    fn test_analyze_args_from_raw_args_takes_first_free_token_as_crate() -> TestResult {
        match AnalyzeArgs::from_raw_args(&toks(&["harw-core"])) {
            Ok(args) => assert_eq!(args.crate_name.as_deref(), Some("harw-core")),
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "unerwarteter Fehler: {error}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_analyze_args_from_raw_args_parses_flags_in_any_order() -> TestResult {
        match AnalyzeArgs::from_raw_args(&toks(&["--dry-run", "harw-core", "--top-down"])) {
            Ok(args) => {
                assert_eq!(args.dry_run, Some(true));
                assert_eq!(args.bottom_up, Some(false));
                assert_eq!(args.crate_name.as_deref(), Some("harw-core"));
            }
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "unerwarteter Fehler: {error}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_analyze_args_from_raw_args_parses_max_parallel_both_forms() -> TestResult {
        match AnalyzeArgs::from_raw_args(&toks(&["--max-parallel", "8"])) {
            Ok(args) => assert_eq!(args.max_parallel, Some(8)),
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "unerwarteter Fehler: {error}"
                )));
            }
        }
        match AnalyzeArgs::from_raw_args(&toks(&["--max-parallel=3"])) {
            Ok(args) => assert_eq!(args.max_parallel, Some(3)),
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "unerwarteter Fehler: {error}"
                )));
            }
        }
        Ok(())
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
    async fn test_analyze_dry_run_lists_nodes_in_leaf_first_order() -> TestResult {
        let (ctx, root) = workspace_context()?;
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
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "Dry-Run darf nicht fehlschlagen: {error}"
                )));
            }
        };
        let report: serde_json::Value = match serde_json::from_str(&output.text) {
            Ok(value) => value,
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "Dry-Run-Ausgabe ist kein JSON: {error}"
                )));
            }
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
        Ok(())
    }

    #[tokio::test]
    async fn test_analyze_dry_run_for_single_crate_uses_subgraph() -> TestResult {
        let (ctx, root) = workspace_context()?;
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
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "Dry-Run darf nicht fehlschlagen: {error}"
                )));
            }
        };
        let report: serde_json::Value = match serde_json::from_str(&output.text) {
            Ok(value) => value,
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "Dry-Run-Ausgabe ist kein JSON: {error}"
                )));
            }
        };
        assert_eq!(report["crate_count"], serde_json::json!(1));
        assert_eq!(report["leaf_first"], serde_json::json!([node_id("a")]));
        Ok(())
    }

    #[tokio::test]
    async fn test_analyze_without_plan_store_is_not_available() -> TestResult {
        let (ctx, root) = workspace_context()?;
        let result = super::analyze(&ctx, AnalyzeArgs::default()).await;
        std::fs::remove_dir_all(root).ok();

        assert!(
            matches!(result, Err(OpError::NotAvailable(_))),
            "ohne Plan-Store muss /analyze fail-closed sein, war: {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_analyze_on_flat_non_rust_root_synthesizes_a_single_pseudo_crate() -> TestResult {
        // Seit dem Nicht-Rust-Rückfall (Teil 2) scheitert `/analyze` ohne
        // `Cargo.toml` nicht mehr — `empty_context()` hat weder ein Manifest
        // noch qualifizierende Unterverzeichnisse, also entsteht genau ein
        // Pseudo-Crate für die Wurzel selbst (Fixture-Basisname "ws").
        let (ctx, root) = empty_context()?;
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
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "Nicht-Rust-Rückfall darf nicht fehlschlagen: {error}"
                )));
            }
        };
        let report: serde_json::Value = match serde_json::from_str(&output.text) {
            Ok(value) => value,
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "Ausgabe ist kein JSON: {error}"
                )));
            }
        };

        assert_eq!(report["crate_count"], serde_json::json!(1));
        assert_eq!(report["leaf_first"], serde_json::json!([node_id("ws")]));
        assert_eq!(
            report["waves"][0]["nodes"][0]["dependencies"],
            serde_json::json!([]),
            "Pseudo-Crates tragen keine hergeleiteten Abhängigkeiten"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_analyze_on_non_rust_root_synthesizes_one_pseudo_crate_per_subdir() -> TestResult {
        let (ctx, root) = directory_context(&["backend", "frontend"])?;
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
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "Nicht-Rust-Rückfall darf nicht fehlschlagen: {error}"
                )));
            }
        };
        let report: serde_json::Value = match serde_json::from_str(&output.text) {
            Ok(value) => value,
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "Ausgabe ist kein JSON: {error}"
                )));
            }
        };

        assert_eq!(report["crate_count"], serde_json::json!(2));
        assert_eq!(
            report["leaf_first"],
            serde_json::json!([node_id("backend"), node_id("frontend")]),
            "beide Pseudo-Crates liegen auf Ebene 0 und werden alphabetisch sortiert"
        );
        assert_eq!(
            report["waves"].as_array().map(|waves| waves.len()),
            Some(1),
            "ohne hergeleitete Abhängigkeiten bleibt es eine einzige Welle"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_analyze_on_non_rust_root_excludes_noise_directories() -> TestResult {
        let (ctx, root) = directory_context(&[".git", "target", "node_modules", "src"])?;
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
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "Nicht-Rust-Rückfall darf nicht fehlschlagen: {error}"
                )));
            }
        };
        let report: serde_json::Value = match serde_json::from_str(&output.text) {
            Ok(value) => value,
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "Ausgabe ist kein JSON: {error}"
                )));
            }
        };

        assert_eq!(
            report["crate_count"],
            serde_json::json!(1),
            "nur 'src' darf als Pseudo-Crate zählen, .git/target/node_modules nicht"
        );
        assert_eq!(report["leaf_first"], serde_json::json!([node_id("src")]));
        Ok(())
    }

    #[tokio::test]
    async fn test_analyze_on_root_with_cargo_toml_still_uses_the_real_graph_load_path() -> TestResult
    {
        // Regressionsschutz: für einen Cargo-Workspace reichert
        // `WorkspaceGraph::load` die Explorer-Einheiten an — erkennbar an der
        // realen internen Abhängigkeit a→b, die der Verzeichnis-Rückfall nie
        // herleitet (dessen Einheiten tragen immer leere `deps`).
        let (ctx, root) = workspace_context()?;
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
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "Dry-Run darf nicht fehlschlagen: {error}"
                )));
            }
        };
        let report: serde_json::Value = match serde_json::from_str(&output.text) {
            Ok(value) => value,
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "Dry-Run-Ausgabe ist kein JSON: {error}"
                )));
            }
        };

        assert_eq!(report["crate_count"], serde_json::json!(2));
        assert_eq!(
            report["waves"][1]["nodes"][0]["dependencies"],
            serde_json::json!([node_id("a")]),
            "die reale Abhängigkeit a→b beweist, dass Cargo.toml geparst wurde"
        );
        Ok(())
    }

    // ── Zell-gesteuerter Fan-out ─────────────────────────────────────────────

    /// Löst die eingebaute Organisation auf; ein Fehler ist ein Defekt der TOML-Datei.
    fn organization() -> TestResult<ResolvedOrganization> {
        default_organization().map_err(ctx("die eingebaute Organisation muss auflösen"))
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

    /// Baut eine Cargo-Einheit unterhalb von `/ws`.
    fn crate_node(name: &str, level: u32) -> AnalysisUnit {
        AnalysisUnit {
            name: name.to_owned(),
            kinds: vec![ProjectKind::CargoCrate],
            rel: PathBuf::from(name),
            dir: PathBuf::from(format!("/ws/{name}")),
            manifest: Some(PathBuf::from(format!("{name}/Cargo.toml"))),
            cargo_name: Some(name.to_owned()),
            version: Some("0.1.0".to_owned()),
            deps: Vec::new(),
            external_deps: Vec::new(),
            level,
        }
    }

    #[test]
    fn test_load_organization_yields_the_embedded_default_organization() -> TestResult {
        match load_organization() {
            Some(organization) => {
                assert_eq!(organization.id.as_string(), DEFAULT_ORGANIZATION_ID)
            }
            None => {
                return Err(TestError::Unexpected(
                    "die eingebaute Organisation muss ladbar sein".to_owned(),
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn test_node_id_falls_into_the_research_clan_scope() -> TestResult {
        let organization = organization()?;
        let Some((clan, cell)) = clan_cell(&organization, RESEARCH_CLAN_ID) else {
            return Err(TestError::Unexpected(
                "der Research-Clan muss eine Zelle haben".to_owned(),
            ));
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
        Ok(())
    }

    #[test]
    fn test_research_cell_selects_exactly_the_research_nodes() -> TestResult {
        let organization = organization()?;
        let Some((clan, cell)) = clan_cell(&organization, RESEARCH_CLAN_ID) else {
            return Err(TestError::Unexpected(
                "der Research-Clan muss eine Zelle haben".to_owned(),
            ));
        };
        let plan = plan_with(vec![
            plan_node("research-a", &[]),
            plan_node("research-b", &[]),
            plan_node("coding-c", &[]),
        ]);

        let resolved = match CellPlan::from_cell(cell, Some(clan), &plan) {
            Ok(resolved) => resolved,
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "die Zelle muss auflösen: {error}"
                )));
            }
        };

        assert_eq!(
            resolved.members,
            vec![TaskId::new("research-a"), TaskId::new("research-b")],
            "der Coding-Knoten gehört einem anderen Clan"
        );
        assert_eq!(resolved.role, RESEARCH_CLAN_ID);
        assert_eq!(resolved.join, JoinSemantics::AllTerminal);
        Ok(())
    }

    #[test]
    fn test_required_write_partition_splits_overlapping_write_scopes() -> TestResult {
        let organization = organization()?;
        let Some((clan, cell)) = clan_cell(&organization, RESEARCH_CLAN_ID) else {
            return Err(TestError::Unexpected(
                "der Research-Clan muss eine Zelle haben".to_owned(),
            ));
        };

        let overlapping = plan_with(vec![
            plan_node("research-a", &["src/shared.rs"]),
            plan_node("research-b", &["src/shared.rs"]),
        ]);
        let resolved = match CellPlan::from_cell(cell, Some(clan), &overlapping) {
            Ok(resolved) => resolved,
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "die Zelle muss auflösen: {error}"
                )));
            }
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
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "die Zelle muss auflösen: {error}"
                )));
            }
        };
        assert_eq!(resolved.batches.len(), 1, "disjunkte Pfade laufen zusammen");
        assert_eq!(resolved.batches[0].len(), 2);
        Ok(())
    }

    #[test]
    fn test_wave_batches_without_a_cell_is_one_batch_in_graph_order() {
        let crates = [crate_node("a", 0), crate_node("b", 0)];
        let level: Vec<&AnalysisUnit> = crates.iter().collect();

        let batches = wave_batches(None, &level);

        assert_eq!(batches.len(), 1, "ohne Zelle bleibt die Welle ungeteilt");
        let names: Vec<&str> = batches[0].iter().map(|node| node.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn test_wave_batches_follows_the_cell_batch_order() {
        let crates = [crate_node("a", 0), crate_node("b", 0)];
        let level: Vec<&AnalysisUnit> = crates.iter().collect();
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
        let level: Vec<&AnalysisUnit> = crates.iter().collect();
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
        let level: Vec<&AnalysisUnit> = crates.iter().collect();

        assert!(cell_plan_for_wave(None, Path::new("/ws"), &level).is_none());

        let wave = plan_wave(None, Path::new("/ws"), &level);
        assert_eq!(wave.batches.len(), 1);
        assert_eq!(wave.batches[0].len(), 2);
        assert_eq!(wave.join, JoinSemantics::AllTerminal);
        assert!(wave.cell_id.is_none());
    }

    #[test]
    fn test_plan_wave_with_the_research_cell_covers_every_crate() -> TestResult {
        let organization = organization()?;
        let cell = clan_cell(&organization, RESEARCH_CLAN_ID);
        let crates = [crate_node("a", 0), crate_node("b", 0)];
        let level: Vec<&AnalysisUnit> = crates.iter().collect();

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
        Ok(())
    }

    #[tokio::test]
    async fn test_analyze_dry_run_reports_the_cell_and_its_batches() -> TestResult {
        // Der Kontext hat keinen Agent-Spawner: würde der Dry-Run ein Kind
        // starten, käme `OpError::NotAvailable` zurück. `Ok` ist damit der
        // Beweis, dass kein Kind gestartet wurde.
        let (ctx, root) = workspace_context()?;
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
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "Dry-Run darf nicht fehlschlagen: {error}"
                )));
            }
        };
        let report: serde_json::Value = match serde_json::from_str(&output.text) {
            Ok(value) => value,
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "Dry-Run-Ausgabe ist kein JSON: {error}"
                )));
            }
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
        Ok(())
    }

    /// Kontext ohne Workspace-Manifest — für den Fehlerpfad des Graph-Ladens.
    fn empty_context() -> TestResult<(OpContext, PathBuf)> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "harw-analyze-empty-test-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(root.join("ws")).map_err(ctx("Test-Workspace anlegen"))?;
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("Workspace-Registry bauen"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .map_err(ctx("Workspace-Binding auflösen"))?;
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());
        Ok((
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, ServiceMap::new()),
            root,
        ))
    }

    /// Kontext ohne Workspace-Manifest, mit den gegebenen direkten
    /// Unterverzeichnissen der Workspace-Wurzel — für den Nicht-Rust-Rückfall
    /// von `synthesize_directory_graph`.
    fn directory_context(subdirs: &[&str]) -> TestResult<(OpContext, PathBuf)> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-analyze-dir-test-{}-{id}", std::process::id()));
        let workspace = root.join("ws");
        std::fs::create_dir_all(&workspace).map_err(ctx("Test-Workspace anlegen"))?;
        for subdir in subdirs {
            std::fs::create_dir_all(workspace.join(subdir))
                .map_err(ctx("Unterverzeichnis anlegen"))?;
        }
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("Workspace-Registry bauen"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .map_err(ctx("Workspace-Binding auflösen"))?;
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());
        Ok((
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, ServiceMap::new()),
            root,
        ))
    }

    // ── Projektneutraler Einheiten-Graph ─────────────────────────────────────

    /// Schreibt eine Datei samt Elternverzeichnissen.
    fn write_file(path: &Path, content: &str) -> TestResult {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(ctx("Verzeichnis anlegen"))?;
        }
        std::fs::write(path, content).map_err(ctx("Datei schreiben"))?;
        Ok(())
    }

    /// Legt ein Nicht-Cargo-Fixture an: zwei npm-Pakete und zwei
    /// Python-Projekte mit je einer Pfad-Abhängigkeit.
    ///
    /// Erwartet: `web → shared-js` (`file:`-Abhängigkeit in `package.json`)
    /// und `api → core` (`[tool.uv.sources]` in `pyproject.toml`).
    fn polyglot_workspace(dir: &Path) -> TestResult {
        write_file(
            &dir.join("web/package.json"),
            r#"{ "name": "web", "dependencies": { "shared-js": "file:../shared-js" } }"#,
        )?;
        write_file(
            &dir.join("shared-js/package.json"),
            r#"{ "name": "shared-js", "version": "1.0.0" }"#,
        )?;
        write_file(
            &dir.join("api/pyproject.toml"),
            "[project]\nname = \"api\"\n\n[tool.uv.sources]\ncore = { path = \"../core\" }\n",
        )?;
        write_file(
            &dir.join("core/pyproject.toml"),
            "[project]\nname = \"core\"\n",
        )?;
        Ok(())
    }

    /// Kontext, dessen Sandbox auf das Nicht-Cargo-Fixture zeigt.
    fn polyglot_context() -> TestResult<(OpContext, PathBuf)> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "harw-analyze-polyglot-test-{}-{id}",
            std::process::id()
        ));
        let workspace = root.join("ws");
        std::fs::create_dir_all(&workspace).map_err(ctx("Test-Workspace anlegen"))?;
        polyglot_workspace(&workspace)?;
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("Workspace-Registry bauen"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .map_err(ctx("Workspace-Binding auflösen"))?;
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::empty());
        Ok((
            OpContext::new(SessionId::new(), TurnId::new(), sandbox, ServiceMap::new()),
            root,
        ))
    }

    /// Führt einen Dry-Run aus und parst den Bericht.
    async fn dry_run_report(
        op_ctx: &OpContext,
        args: AnalyzeArgs,
    ) -> TestResult<serde_json::Value> {
        let output = super::analyze(
            op_ctx,
            AnalyzeArgs {
                dry_run: Some(true),
                ..args
            },
        )
        .await
        .map_err(ctx("Dry-Run darf nicht fehlschlagen"))?;
        serde_json::from_str(&output.text).map_err(ctx("Dry-Run-Ausgabe ist kein JSON"))
    }

    #[tokio::test]
    async fn test_analyze_on_npm_and_python_fixture_builds_units_and_edges() -> TestResult {
        let (op_ctx, root) = polyglot_context()?;
        let report = dry_run_report(&op_ctx, AnalyzeArgs::default()).await;
        std::fs::remove_dir_all(root).ok();
        let report = report?;

        assert_eq!(report["graph"]["source"], serde_json::json!("explorer"));
        assert_eq!(report["unit_count"], serde_json::json!(4));
        assert_eq!(report["crate_count"], serde_json::json!(4));
        assert_eq!(
            report["leaf_first"],
            serde_json::json!([
                node_id("core"),
                node_id("shared-js"),
                node_id("api"),
                node_id("web")
            ]),
            "Pfad-Abhängigkeiten müssen die Ziele eine Ebene tiefer legen"
        );
        assert_eq!(
            report["waves"][1]["nodes"][0]["unit"],
            serde_json::json!("api")
        );
        assert_eq!(
            report["waves"][1]["nodes"][0]["kind"],
            serde_json::json!("python")
        );
        assert_eq!(
            report["waves"][1]["nodes"][0]["dependencies"],
            serde_json::json!([node_id("core")])
        );
        assert_eq!(
            report["waves"][1]["nodes"][1]["kind"],
            serde_json::json!("node")
        );
        assert_eq!(
            report["waves"][1]["nodes"][1]["dependencies"],
            serde_json::json!([node_id("shared-js")])
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_analyze_on_polyglot_fixture_subgraph_by_unit_name_and_path() -> TestResult {
        let (op_ctx, root) = polyglot_context()?;
        let by_name = dry_run_report(
            &op_ctx,
            AnalyzeArgs {
                crate_name: Some("web".to_owned()),
                ..AnalyzeArgs::default()
            },
        )
        .await;
        let by_path = dry_run_report(
            &op_ctx,
            AnalyzeArgs {
                crate_name: Some("./api/".to_owned()),
                ..AnalyzeArgs::default()
            },
        )
        .await;
        let unknown = super::analyze(
            &op_ctx,
            AnalyzeArgs {
                crate_name: Some("gibt-es-nicht".to_owned()),
                dry_run: Some(true),
                ..AnalyzeArgs::default()
            },
        )
        .await;
        std::fs::remove_dir_all(root).ok();

        assert_eq!(
            by_name?["leaf_first"],
            serde_json::json!([node_id("shared-js"), node_id("web")])
        );
        assert_eq!(
            by_path?["leaf_first"],
            serde_json::json!([node_id("core"), node_id("api")])
        );
        assert!(matches!(unknown, Err(OpError::Execution(_))));
        Ok(())
    }

    #[test]
    fn test_build_unit_graph_marks_nested_projects_and_skips_workspace_shells() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("Temp-Verzeichnis anlegen"))?;
        let root = std::fs::canonicalize(dir.path()).map_err(ctx("Wurzel kanonisieren"))?;
        // Wurzel-`package.json` mit `workspaces` ist eine Hülle, keine Einheit;
        // das Mitglied `packages/ui` ist eine. `packages/ui/tools` ist ein
        // darin verschachteltes Python-Projekt.
        write_file(
            &root.join("package.json"),
            r#"{ "name": "mono", "private": true, "workspaces": ["packages/*"] }"#,
        )?;
        write_file(
            &root.join("packages/ui/package.json"),
            r#"{ "name": "@acme/ui" }"#,
        )?;
        write_file(
            &root.join("packages/ui/tools/pyproject.toml"),
            "[project]\nname = \"ui-tools\"\n",
        )?;

        let (graph, info) = build_unit_graph(&root).map_err(ctx("Graph bauen"))?;
        assert_eq!(info["source"], serde_json::json!("explorer"));
        let mut names: Vec<&str> = graph.units.iter().map(|unit| unit.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec!["@acme/ui", "ui-tools"],
            "die Hülle 'mono' zählt nicht"
        );
        assert_eq!(node_id("@acme/ui"), "research--acme-ui");

        let Some(ui) = graph.get("@acme/ui") else {
            return Err(TestError::Unexpected("Einheit @acme/ui fehlt".to_owned()));
        };
        let nested: Vec<&str> = graph
            .nested_in(ui)
            .iter()
            .map(|unit| unit.name.as_str())
            .collect();
        assert_eq!(nested, vec!["ui-tools"]);

        let question = analysis_question(&root, ui, &[], &graph.nested_in(ui));
        assert!(question.question.contains("packages/ui/tools"));
        assert_eq!(question.scope.paths, vec!["packages/ui/**".to_owned()]);
        assert!(
            question.scope.crates.is_empty(),
            "npm-Pakete sind keine Crates"
        );
        Ok(())
    }

    #[test]
    fn test_analysis_question_is_language_neutral_for_python_units() {
        let unit = AnalysisUnit {
            kinds: vec![ProjectKind::Python],
            cargo_name: None,
            version: None,
            manifest: Some(PathBuf::from("core/pyproject.toml")),
            ..crate_node("core", 0)
        };
        let question = analysis_question(Path::new("/ws"), &unit, &[], &[]);
        let text = question.question.as_str();
        assert!(text.contains("NotImplementedError"), "{text}");
        assert!(text.contains("Docstrings"), "{text}");
        assert!(
            !text.contains("todo!()"),
            "keine Rust-Marker für Python: {text}"
        );
        assert!(
            !text.contains("`pub`"),
            "keine Rust-API-Begriffe für Python: {text}"
        );
        assert!(!text.contains("Crate"), "{text}");
    }

    #[test]
    fn test_analysis_question_for_rust_units_keeps_rust_markers() {
        let unit = crate_node("a", 0);
        let question = analysis_question(Path::new("/ws"), &unit, &[], &[]);
        assert!(question.question.contains("todo!()"));
        assert!(question.question.contains("`pub`"));
        assert_eq!(question.scope.crates, vec!["a".to_owned()]);
    }

    #[test]
    fn test_analysis_question_for_plain_directory_is_generic() {
        let unit = directory_unit(Path::new("/ws"), PathBuf::from("/ws/scripts"));
        let question = analysis_question(Path::new("/ws"), &unit, &[], &[]);
        assert!(question.question.contains("Verzeichnis `scripts`"));
        assert!(question.question.contains("raise NotImplementedError"));
        assert_eq!(question.scope.paths, vec!["scripts/**".to_owned()]);
    }

    #[test]
    fn test_analyze_args_accept_unit_name_and_crate_name_in_json() -> TestResult {
        let legacy: AnalyzeArgs =
            serde_json::from_value(serde_json::json!({ "crate_name": "harw-core" }))
                .map_err(ctx("crate_name muss weiter gelten"))?;
        assert_eq!(legacy.unit_name(), Some("harw-core"));
        let neutral: AnalyzeArgs =
            serde_json::from_value(serde_json::json!({ "unit_name": "web" }))
                .map_err(ctx("unit_name muss gelten"))?;
        assert_eq!(neutral.unit_name(), Some("web"));
        Ok(())
    }
}

/// Regressionstests zum Bugreport „analyze: no delegation capability“ (Runde 5).
#[cfg(test)]
mod delegation_tests {
    use super::{analyst_role_for, completion_status};
    use harw_agent_dsl::roles::AgentRoleId;
    use harw_registry_defaults::profile::role_names;
    use serde_json::json;

    #[test]
    fn test_uia_caller_gets_the_uia_spawnable_analysis_role() {
        // Die UIA darf `analyst` (`role = "worker"`) nie spawnen; die
        // Ausweichrolle muss eine sein, die die Spawn-Matrix ihr erlaubt.
        assert_eq!(
            analyst_role_for(Some(AgentRoleId::UserInterface)),
            role_names::UIA_EXPLORER
        );
    }

    #[test]
    fn test_other_callers_keep_the_analyst_role() {
        for caller in [
            None,
            Some(AgentRoleId::RootOrchestrator),
            Some(AgentRoleId::ChildOrchestrator),
            Some(AgentRoleId::Worker),
        ] {
            assert_eq!(analyst_role_for(caller), role_names::ANALYST, "{caller:?}");
        }
    }

    #[test]
    fn test_complete_report_has_no_notice() {
        assert_eq!(completion_status(2, 2, &[]), ("complete", None));
    }

    #[test]
    fn test_report_without_any_finding_is_marked_incomplete() {
        let failures = vec![json!({ "node": "research-a", "error": "Budget erschöpft" })];
        let (status, notice) = completion_status(1, 0, &failures);
        assert_eq!(status, "incomplete");
        let notice = notice.unwrap_or_default();
        assert!(notice.starts_with("UNVOLLSTÄNDIG"), "{notice}");
        assert!(notice.contains("keine Analyseergebnisse"), "{notice}");
        assert!(!notice.contains("Spawn-Matrix"), "{notice}");
    }

    #[test]
    fn test_partial_report_names_the_delegation_denial_and_the_alternative() {
        let failures = vec![json!({
            "node": "research-b",
            "error": "Agent-Spawn fehlgeschlagen: no delegation capability is available for this request",
        })];
        let (status, notice) = completion_status(2, 1, &failures);
        assert_eq!(status, "partial");
        let notice = notice.unwrap_or_default();
        assert!(notice.contains("Spawn-Matrix"), "{notice}");
        assert!(notice.contains("root-orchestrator"), "{notice}");
    }
}
