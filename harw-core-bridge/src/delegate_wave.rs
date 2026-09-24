//! `delegate_wave` — das Fan-out-Werkzeug der Orchestrator-Sitzungen.
//!
//! # Verantwortungsbereich
//! Ein Root- oder Child-Orchestrator (`root-orchestrator`,
//! `coding-orchestrator`, `research-orchestrator`, `analysis-orchestrator`)
//! startet mit **einem** Werkzeugaufruf eine ganze Welle von Kindern und
//! bekommt deren Ergebnisse als strukturierte Ergebnismenge zurück — ohne dass
//! seine eigene Sitzung pausiert. Das unterscheidet `delegate_wave` vom
//! Handoff `transfer_to_<rolle>`: ein Handoff übergibt den Turn an das Kind
//! (`TurnOutcome::AwaitingChild`), `delegate_wave` ist ein gewöhnlicher,
//! blockierender Werkzeugaufruf (Agent-as-Tool, `docs/design/
//! wave-4-agents-as-tools.md` §3).
//!
//! ```text
//! delegate_wave {
//!   targets: [{ role, task, complexity?, id? }, …],   // 1 ..= 16
//!   join: "all" | "any" | "collect",                 // Standard "all"
//!   max_parallel: n                                  // Standard min(4, Ziele)
//! }
//! ```
//!
//! # Admission — Schnittmenge, nie Vereinigung
//! Jedes Ziel läuft nur, wenn es in **allen** folgenden Mengen liegt
//! (`docs/design/delegation-capabilities.md`, „Sichtbarkeit“):
//! 1. der Laufzeit-Sichtbarkeit des Aufrufers
//!    (`AgentSpawner::delegation_target_names` → `delegation_visibility`:
//!    Rollenmatrix, exakte `child_orchestrators`-Freigabe, Resttiefe),
//! 2. den in seiner Definition deklarierten Zielen (`[delegation].targets`,
//!    über [`DelegateWavePolicy`] von der Composition-Root geliefert),
//! 3. den Rollen mit bekanntem Autoritäts-Reducer (fail-closed: ohne Reducer
//!    kein Spawn).
//!
//! Ein abgelehntes Ziel erscheint als `"unavailable"` mit immer derselben
//! Meldung — gleich, ob die Rolle unbekannt, verborgen, nicht deklariert oder
//! ohne Reducer ist. Die Antwort darf kein Agentenkatalog-Orakel sein.
//!
//! # Ausführung
//! Die zugelassenen Ziele laufen in einem rollierenden Pool mit höchstens
//! `max_parallel` Plätzen. Jeder Platz ist genau ein
//! [`fanout_children`]-Aufruf mit einer einzigen Frage — damit gelten
//! unverändert dieselben Grenzen wie für `/analyze` und `explore`: monoton
//! reduzierte Sandbox, Budget-Verschnitt mit der Agent-IR, Effort-Klammer,
//! Return-Contract samt Reparatur-Turn, Slot-Freigabe per RAII. Die
//! `uia-worker`-Kappung (`max_concurrent_instances_for_role`) greift pro
//! Rolle bereits in `fanout_children`.
//!
//! Join-Semantik über die Welle:
//! - `all`: auf jedes Ziel warten; erfüllt nur, wenn jedes abgeschlossen hat.
//! - `any`: das erste abgeschlossene Ziel gewinnt; laufende Geschwister werden
//!   durch Verwerfen ihres Futures abgebrochen (der RAII-Guard in
//!   `fanout_children` gibt ihren Slot frei, der Kind-Controller legt die
//!   Sitzung als gescheitert zurück), nicht gestartete nie gestartet.
//! - `collect`: auf jedes Ziel warten und alles sammeln, auch Fehlschläge.
//!
//! # Budget
//! Jedes Kind bekommt höchstens das Budget des **Aufrufers** (sein eigener
//! Admission-Record, `ManagedAgentSpawner::child_budget`), verschnitten mit
//! seiner eigenen Agent-IR — ein Kind verfügt nie über mehr als sein Parent.
//! Eine echte Anrechnung des Kind-Verbrauchs auf den Parent (Restbudget statt
//! Deckel) braucht den Kind-Controller; siehe [`wave_budget_cap`].
//!
//! # Nebenläufigkeit
//! Alle Plätze werden in **einer** Task gemeinsam gepollt
//! (`std::future::poll_fn`), es werden keine Tasks gespawnt — dieselbe Form
//! wie `fanout_children` und `ManagedAgentSpawner::run_children`.
//!
//! # Fehler
//! Nur Gesamtausfälle sind `Err`: ungültige Argumente
//! ([`OpError::InvalidArguments`]), fehlender Spawner/StateStore oder eine
//! Sitzung ohne jede Delegationsoberfläche ([`OpError::NotAvailable`]). Das
//! Scheitern einzelner Kinder ist immer ein Eintrag der Ergebnismenge.

use std::collections::{BTreeSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::task::Poll;

use harw_core::child_controller::{AgentBudget, ChildRegistryFactory, JoinSemantics};
use harw_operations::context::OpContext;
use harw_operations::error::OpError;
use harw_operations::op_schema::{
    array_schema, enum_string_schema, integer_schema, object_schema, string_schema,
};
use harw_operations::operation::{
    ApprovalPolicy, ArgsSchemaFn, OpFuture, OpInput, OpOutput, Operation, OperationCategory,
    OperationDomain, OperationMeta, PermissionTier, Surface,
};
use serde_json::{Map, Value, json};

use crate::agent_tool::{ChildReturnContract, fanout_children};
use crate::context_ext::OpContextCoreExt;

/// Werkzeugname, unter dem die Operation dem Modell erscheint.
pub const DELEGATE_WAVE_TOOL: &str = "delegate_wave";

/// Höchstzahl Ziele je Welle. Eine größere Welle ist ein Zerlegungsfehler des
/// Orchestrators, kein Fan-out.
pub const MAX_WAVE_TARGETS: usize = 16;

/// Standard-Parallelität, wenn `max_parallel` fehlt.
pub const DEFAULT_MAX_PARALLEL: usize = 4;

/// Höchstlänge einer vom Modell vergebenen Ziel-ID.
const MAX_TARGET_ID_CHARS: usize = 64;

/// Einheitliche Ablehnung — wortgleich mit der Admission-Meldung des
/// Kind-Controllers, damit keine Ablehnung mehr verrät als eine andere.
const NO_CAPABILITY: &str = "no delegation capability is available for this request";

/// Erlaubte Felder auf oberster Ebene.
const TOP_LEVEL_FIELDS: &[&str] = &["targets", "join", "max_parallel"];

/// Erlaubte Felder je Ziel.
const TARGET_FIELDS: &[&str] = &["id", "role", "task", "complexity"];

// ── Anfrage ───────────────────────────────────────────────────────────────────

/// Join-Semantik einer Welle, wie das Modell sie benennt.
///
/// # Concurrency
/// `Copy`, zustandslos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaveJoin {
    /// `"all"` → [`JoinSemantics::AllTerminal`].
    All,
    /// `"any"` → [`JoinSemantics::AnyTerminal`].
    Any,
    /// `"collect"` → [`JoinSemantics::Collect`].
    Collect,
}

impl WaveJoin {
    /// Parst das Modell-Label.
    ///
    /// # Returns
    /// `Some(join)` für `"all"`, `"any"`, `"collect"`; sonst `None`.
    #[must_use]
    pub fn parse(label: &str) -> Option<Self> {
        match label {
            "all" => Some(Self::All),
            "any" => Some(Self::Any),
            "collect" => Some(Self::Collect),
            _ => None,
        }
    }

    /// Das stabile Label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Any => "any",
            Self::Collect => "collect",
        }
    }

    /// Die Join-Semantik des Kind-Controllers.
    #[must_use]
    pub const fn semantics(self) -> JoinSemantics {
        match self {
            Self::All => JoinSemantics::AllTerminal,
            Self::Any => JoinSemantics::AnyTerminal,
            Self::Collect => JoinSemantics::Collect,
        }
    }
}

/// Komplexitätsangabe eines Ziels (steuert die Modellstufe des Kindes, nie
/// seine Rechte — `harw_core::child_controller::TaskComplexity`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaveComplexity {
    /// `"simple"`.
    Simple,
    /// `"complex"`.
    Complex,
}

impl WaveComplexity {
    /// Das stabile Label, so wie der Kind-Controller es im Spawn-Kontext liest.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Simple => "simple",
            Self::Complex => "complex",
        }
    }
}

/// Ein Ziel der Welle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WaveTarget {
    /// Eindeutige ID innerhalb der Welle (vom Modell oder `t<n>`); zugleich
    /// die `question.id`, an die ein `ResearchFinding` gebunden wird.
    pub id: String,
    /// Rollenname des Kindes.
    pub role: String,
    /// Auftrag des Kindes.
    pub task: String,
    /// Optionale Komplexitätsangabe.
    pub complexity: Option<WaveComplexity>,
}

impl WaveTarget {
    /// Der Spawn-Kontext und Turn-Input des Kindes.
    ///
    /// # Beschreibung
    /// `question.id`/`question.question` folgt der Nutzlastform von
    /// `harw-ops::explore`, damit ein Kind mit `ResearchFinding`-Vertrag sein
    /// Finding an genau dieses Ziel bindet. `complexity` steht auf oberster
    /// Ebene, wo `TaskComplexity::from_spawn_context` es liest.
    ///
    /// # Argumente
    /// - `position` (`usize`): 0-basierte Position in der Welle.
    /// - `size` (`usize`): Anzahl Ziele der Welle.
    #[must_use]
    pub fn payload(&self, position: usize, size: usize) -> Value {
        let mut payload = json!({
            "id": self.id,
            "role": self.role,
            "task": self.task,
            "question": { "id": self.id, "question": self.task },
            "wave": { "tool": DELEGATE_WAVE_TOOL, "position": position + 1, "size": size },
        });
        if let (Some(complexity), Some(object)) = (self.complexity, payload.as_object_mut()) {
            object.insert("complexity".to_owned(), json!(complexity.as_str()));
        }
        payload
    }
}

/// Eine geprüfte `delegate_wave`-Anfrage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegateWaveRequest {
    /// Die Ziele in Aufrufreihenfolge (1 ..= [`MAX_WAVE_TARGETS`]).
    pub targets: Vec<WaveTarget>,
    /// Join-Semantik der Welle.
    pub join: WaveJoin,
    /// Höchstzahl gleichzeitig laufender Kinder (1 ..= Anzahl Ziele).
    pub max_parallel: usize,
}

impl DelegateWaveRequest {
    /// Parst und prüft die Modell-Argumente.
    ///
    /// # Beschreibung
    /// Geschlossenes Schema: unbekannte Felder — insbesondere Effort-,
    /// Budget- oder Sandbox-Wünsche — sind ein Argumentfehler, nie still
    /// ignoriert (Tool-Argumente sind Modell-Output, kein Owner-Nachweis,
    /// K5/G-084). `max_parallel` wird auf die Anzahl Ziele gekappt.
    ///
    /// # Errors
    /// [`OpError::InvalidArguments`] mit einer Meldung, die das verletzte Feld
    /// nennt.
    pub fn parse(args: &Value) -> Result<Self, OpError> {
        let object = args
            .as_object()
            .ok_or_else(|| invalid("arguments must be a JSON object"))?;
        reject_unknown_fields(object, TOP_LEVEL_FIELDS, "delegate_wave")?;

        let raw_targets = object
            .get("targets")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid("`targets` must be a non-empty array"))?;
        if raw_targets.is_empty() {
            return Err(invalid("`targets` must be a non-empty array"));
        }
        if raw_targets.len() > MAX_WAVE_TARGETS {
            return Err(invalid(&format!(
                "`targets` holds {} entries; at most {MAX_WAVE_TARGETS} are allowed per wave",
                raw_targets.len()
            )));
        }

        let mut targets = Vec::with_capacity(raw_targets.len());
        let mut seen_ids: BTreeSet<String> = BTreeSet::new();
        for (index, raw) in raw_targets.iter().enumerate() {
            let target = parse_target(raw, index)?;
            if !seen_ids.insert(target.id.clone()) {
                return Err(invalid(&format!(
                    "`targets[{index}].id` '{}' is not unique within the wave",
                    target.id
                )));
            }
            targets.push(target);
        }

        let join = match object.get("join") {
            None | Some(Value::Null) => WaveJoin::All,
            Some(Value::String(label)) => WaveJoin::parse(label)
                .ok_or_else(|| invalid("`join` must be one of \"all\", \"any\", \"collect\""))?,
            Some(_) => return Err(invalid("`join` must be a string")),
        };

        let max_parallel = match object.get("max_parallel") {
            None | Some(Value::Null) => DEFAULT_MAX_PARALLEL,
            Some(value) => match value.as_u64() {
                Some(0) | None => {
                    return Err(invalid("`max_parallel` must be a positive integer"));
                }
                Some(requested) => usize::try_from(requested).unwrap_or(usize::MAX),
            },
        };

        Ok(Self {
            max_parallel: max_parallel.min(targets.len()).max(1),
            targets,
            join,
        })
    }
}

/// Parst ein einzelnes Ziel.
fn parse_target(raw: &Value, index: usize) -> Result<WaveTarget, OpError> {
    let object = raw
        .as_object()
        .ok_or_else(|| invalid(&format!("`targets[{index}]` must be an object")))?;
    reject_unknown_fields(object, TARGET_FIELDS, &format!("targets[{index}]"))?;
    let role = required_text(object, "role", index)?;
    let task = required_text(object, "task", index)?;
    let id = match object.get("id") {
        None | Some(Value::Null) => format!("t{}", index + 1),
        Some(Value::String(id)) => {
            let id = id.trim();
            if id.is_empty() || id.chars().count() > MAX_TARGET_ID_CHARS {
                return Err(invalid(&format!(
                    "`targets[{index}].id` must hold 1 to {MAX_TARGET_ID_CHARS} characters"
                )));
            }
            id.to_owned()
        }
        Some(_) => return Err(invalid(&format!("`targets[{index}].id` must be a string"))),
    };
    let complexity = match object.get("complexity") {
        None | Some(Value::Null) => None,
        Some(Value::String(label)) if label == "simple" => Some(WaveComplexity::Simple),
        Some(Value::String(label)) if label == "complex" => Some(WaveComplexity::Complex),
        Some(_) => {
            return Err(invalid(&format!(
                "`targets[{index}].complexity` must be \"simple\" or \"complex\""
            )));
        }
    };
    Ok(WaveTarget {
        id,
        role,
        task,
        complexity,
    })
}

/// Liest ein nicht-leeres Textfeld eines Ziels.
fn required_text(
    object: &Map<String, Value>,
    field: &str,
    index: usize,
) -> Result<String, OpError> {
    object
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            invalid(&format!(
                "`targets[{index}].{field}` must be a non-empty string"
            ))
        })
}

/// Lehnt jedes Feld außerhalb von `allowed` ab (geschlossenes Schema).
fn reject_unknown_fields(
    object: &Map<String, Value>,
    allowed: &[&str],
    location: &str,
) -> Result<(), OpError> {
    match object.keys().find(|key| !allowed.contains(&key.as_str())) {
        Some(key) => Err(invalid(&format!(
            "`{location}` does not accept the field `{key}`; allowed are {allowed:?} \
             (effort, budget and sandbox of a child are fixed by its role, never by tool \
             arguments)"
        ))),
        None => Ok(()),
    }
}

/// Baut einen Argumentfehler.
fn invalid(message: &str) -> OpError {
    OpError::InvalidArguments(format!("delegate_wave: {message}"))
}

// ── Politik der Composition-Root ─────────────────────────────────────────────

/// Liefert die Reducer-Kennung einer Rolle (`authority_reducer_for_role(…).id()`).
pub type ReducerForRole = Arc<dyn Fn(&str) -> Option<&'static str> + Send + Sync>;

/// Liefert die deklarierten Delegationsziele einer Aufruferrolle
/// (`delegation_targets_for_role`).
pub type TargetsForCaller = Arc<dyn Fn(&str) -> Option<Vec<String>> + Send + Sync>;

/// Was `delegate_wave` von der Composition-Root braucht, ohne von
/// `harw-registry-defaults` abzuhängen (dieselbe Begründung wie der
/// gespiegelte `reducer_ceiling` in `agent_tool.rs`: die Adapter-Crate soll
/// nicht den gesamten Werkzeugbaum ziehen).
///
/// # Concurrency
/// `Clone + Send + Sync`; beide Funktionen müssen rein sein.
#[derive(Clone)]
pub struct DelegateWavePolicy {
    reducer_for_role: ReducerForRole,
    targets_for_caller: TargetsForCaller,
}

impl DelegateWavePolicy {
    /// Baut die Politik.
    ///
    /// # Argumente
    /// - `reducer_for_role`: Rolle → Reducer-Kennung
    ///   (`harw_registry_defaults::authority_reducer_for_role(role).map(|r| r.id())`).
    ///   `None` macht die Rolle für `delegate_wave` unerreichbar.
    /// - `targets_for_caller`: Aufruferrolle → `[delegation].targets`
    ///   (`harw_registry_defaults::authority::delegation_targets_for_role`).
    ///   `None` für eine bekannte Aufruferrolle heißt: kein Ziel.
    #[must_use]
    pub fn new(
        reducer_for_role: impl Fn(&str) -> Option<&'static str> + Send + Sync + 'static,
        targets_for_caller: impl Fn(&str) -> Option<Vec<String>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            reducer_for_role: Arc::new(reducer_for_role),
            targets_for_caller: Arc::new(targets_for_caller),
        }
    }
}

impl std::fmt::Debug for DelegateWavePolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DelegateWavePolicy").finish_non_exhaustive()
    }
}

/// Die deklarierte Zielmenge des Aufrufers.
///
/// # Beschreibung
/// - Aufrufer mit Admission-Record (ein Kind, z. B. ein Child-Orchestrator):
///   seine Rolle wird über die Politik aufgelöst; keine Deklaration heißt
///   **keine** Ziele (fail-closed — ein Worker bekommt keinen Fan-out).
/// - Aufrufer ohne Record (die Wurzelsitzung des Laufs): keine zusätzliche
///   Deklaration; es gilt allein ihre vertrauenswürdige Laufzeit-Sichtbarkeit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclaredTargets {
    /// Keine zusätzliche Einschränkung über die Sichtbarkeit hinaus.
    Unrestricted,
    /// Genau diese Rollen.
    Only(BTreeSet<String>),
}

impl DeclaredTargets {
    /// Löst die Deklaration für eine (optional bekannte) Aufruferrolle auf.
    #[must_use]
    pub fn for_caller(policy: &DelegateWavePolicy, caller_role: Option<&str>) -> Self {
        match caller_role {
            None => Self::Unrestricted,
            Some(role) => Self::Only(
                (policy.targets_for_caller)(role)
                    .unwrap_or_default()
                    .into_iter()
                    .collect(),
            ),
        }
    }

    fn admits(&self, role: &str) -> bool {
        match self {
            Self::Unrestricted => true,
            Self::Only(roles) => roles.contains(role),
        }
    }
}

/// Ein zugelassenes Ziel samt allem, was sein Platz braucht.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedTarget {
    /// Position in der Welle.
    pub position: usize,
    /// Reducer-Kennung für die Sandbox des Kindes.
    pub reducer: &'static str,
}

/// Entscheidet je Ziel über die Zulassung (reine Schnittmenge).
///
/// # Argumente
/// - `request`: die geprüfte Anfrage.
/// - `visible`: die Laufzeit-Sichtbarkeit des Aufrufers.
/// - `declared`: die deklarierte Zielmenge des Aufrufers.
/// - `policy`: liefert die Reducer-Kennungen.
///
/// # Returns
/// Positionsgleich zu `request.targets`: `Some(reducer)` für ein zugelassenes
/// Ziel, `None` für ein abgelehntes.
#[must_use]
pub fn admit_targets(
    request: &DelegateWaveRequest,
    visible: &BTreeSet<String>,
    declared: &DeclaredTargets,
    policy: &DelegateWavePolicy,
) -> Vec<Option<&'static str>> {
    request
        .targets
        .iter()
        .map(|target| {
            let role = target.role.as_str();
            if !visible.contains(role) || !declared.admits(role) {
                return None;
            }
            (policy.reducer_for_role)(role)
        })
        .collect()
}

/// Der Budgetdeckel jedes Kindes einer Welle.
///
/// # Beschreibung
/// Heute: das Budget aus dem Admission-Record des Aufrufers (`None` für die
/// Wurzelsitzung → kein zusätzlicher Deckel). `fanout_children` verschneidet
/// ihn je Kind mit dessen Agent-IR, je Dimension gewinnt die strengere Grenze.
///
/// Sobald der Kind-Controller das **Restbudget** eines Parents führt
/// (Verbrauch seiner Kinder abgezogen), ist hier nur die Quelle zu tauschen:
/// `remaining_budget(parent)` statt `child_budget(parent)`.
#[must_use]
pub fn wave_budget_cap(caller_budget: Option<AgentBudget>) -> AgentBudget {
    caller_budget.unwrap_or_default()
}

// ── Ergebnis ──────────────────────────────────────────────────────────────────

/// Ausgang eines Ziels.
#[derive(Debug, Clone, PartialEq)]
pub enum TargetStatus {
    /// Vertragskonformes Ergebnis des Kindes.
    Completed(Value),
    /// Zulässige Pause (`{"paused": …, "child": …}`); kein Endergebnis.
    Paused(Value),
    /// Das Kind lief, lieferte aber kein verwertbares Ergebnis.
    Failed(String),
    /// Nicht gestartet oder abgebrochen, weil ein Geschwister gewann (`any`).
    Cancelled,
    /// Nicht zugelassen (siehe Moduldoku, „Admission“).
    Unavailable,
}

impl TargetStatus {
    /// Stabiles Label für die Ausgabe.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Completed(_) => "completed",
            Self::Paused(_) => "paused",
            Self::Failed(_) => "failed",
            Self::Cancelled => "cancelled",
            Self::Unavailable => "unavailable",
        }
    }

    /// Übersetzt den Ausgang eines einzelnen `fanout_children`-Platzes.
    fn from_fanout(value: Result<Vec<Result<Value, String>>, OpError>) -> Self {
        match value {
            Err(error) => Self::Failed(error.to_string()),
            Ok(mut results) => match (results.pop(), results.is_empty()) {
                (Some(Ok(value)), true) if is_pause_report(&value) => Self::Paused(value),
                (Some(Ok(value)), true) => Self::Completed(value),
                (Some(Err(message)), true) => Self::Failed(message),
                _ => Self::Failed("der Fan-out-Platz lieferte nicht genau ein Ergebnis".to_owned()),
            },
        }
    }
}

/// Ob ein Fan-out-Ergebnis der Pausenbericht `{"paused": …, "child": …}` ist.
fn is_pause_report(value: &Value) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == 2 && object.contains_key("paused") && object.contains_key("child")
    })
}

/// Bericht zu einem Ziel.
#[derive(Debug, Clone, PartialEq)]
pub struct TargetReport {
    /// Ziel-ID.
    pub id: String,
    /// Rolle.
    pub role: String,
    /// Ausgang.
    pub status: TargetStatus,
}

/// Die strukturierte Ergebnismenge einer Welle.
#[derive(Debug, Clone, PartialEq)]
pub struct DelegateWaveReport {
    /// Angewandte Join-Semantik.
    pub join: WaveJoin,
    /// Effektive Parallelität.
    pub max_parallel: usize,
    /// Je Ziel ein Bericht, in Aufrufreihenfolge.
    pub targets: Vec<TargetReport>,
}

impl DelegateWaveReport {
    /// Ob die Welle ihren Join erfüllt hat.
    ///
    /// # Returns
    /// - `all`: jedes Ziel abgeschlossen.
    /// - `any`: mindestens ein Ziel abgeschlossen.
    /// - `collect`: jedes zugelassene Ziel lief bis zum Ende (kein
    ///   `cancelled`); `unavailable` zählt als terminal.
    #[must_use]
    pub fn is_satisfied(&self) -> bool {
        let completed = |report: &TargetReport| matches!(report.status, TargetStatus::Completed(_));
        match self.join {
            WaveJoin::All => self.targets.iter().all(completed),
            WaveJoin::Any => self.targets.iter().any(completed),
            WaveJoin::Collect => self
                .targets
                .iter()
                .all(|report| !matches!(report.status, TargetStatus::Cancelled)),
        }
    }

    /// Anzahl Ziele mit dem Label `label`.
    #[must_use]
    pub fn count(&self, label: &str) -> usize {
        self.targets
            .iter()
            .filter(|report| report.status.label() == label)
            .count()
    }

    /// Die JSON-Form der Ergebnismenge (Modell-Ausgabe und `OpOutput::data`).
    #[must_use]
    pub fn to_json(&self) -> Value {
        let results: Vec<Value> = self
            .targets
            .iter()
            .map(|report| {
                let mut entry = json!({
                    "id": report.id,
                    "role": report.role,
                    "status": report.status.label(),
                });
                if let Some(object) = entry.as_object_mut() {
                    match &report.status {
                        TargetStatus::Completed(value) | TargetStatus::Paused(value) => {
                            object.insert("result".to_owned(), value.clone());
                        }
                        TargetStatus::Failed(message) => {
                            object.insert("error".to_owned(), json!(message));
                        }
                        TargetStatus::Unavailable => {
                            object.insert("error".to_owned(), json!(NO_CAPABILITY));
                        }
                        TargetStatus::Cancelled => {}
                    }
                }
                entry
            })
            .collect();
        json!({
            "tool": DELEGATE_WAVE_TOOL,
            "join": self.join.as_str(),
            "max_parallel": self.max_parallel,
            "satisfied": self.is_satisfied(),
            "counts": {
                "completed": self.count("completed"),
                "failed": self.count("failed"),
                "paused": self.count("paused"),
                "cancelled": self.count("cancelled"),
                "unavailable": self.count("unavailable"),
            },
            "results": results,
        })
    }

    /// Die Werkzeugausgabe: kompaktes JSON als Text, dieselbe Struktur als
    /// `data`.
    #[must_use]
    pub fn into_output(self) -> OpOutput {
        let data = self.to_json();
        OpOutput {
            text: data.to_string(),
            data: Some(data),
        }
    }
}

// ── Ausführung ────────────────────────────────────────────────────────────────

/// Ein laufender Fan-out-Platz.
type SlotFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<Result<Value, String>>, OpError>> + Send + 'a>>;

/// Fährt eine geprüfte Welle und liefert ihre Ergebnismenge.
///
/// # Beschreibung
/// Siehe Moduldoku (Admission, Ausführung, Budget). Der Aufrufer ist die
/// Sitzung aus `ctx.session_id()`; ihre Sitzung pausiert nicht.
///
/// # Errors
/// - [`OpError::NotAvailable`]: kein `ManagedAgentSpawner`/`StateStore` im
///   Kontext, oder die Sitzung sieht überhaupt kein Delegationsziel.
///
/// # Concurrency
/// `async`; alle Plätze werden in dieser einen Task gepollt.
pub async fn delegate_wave(
    ctx: &OpContext,
    request: &DelegateWaveRequest,
    policy: &DelegateWavePolicy,
) -> Result<DelegateWaveReport, OpError> {
    let spawner = ctx.managed_spawner().ok_or_else(|| {
        OpError::NotAvailable("kein Agent-Spawner in diesem Kontext konfiguriert".to_owned())
    })?;
    if ctx.state_store().is_none() {
        return Err(OpError::NotAvailable(
            "kein StateStore in diesem Kontext konfiguriert".to_owned(),
        ));
    }

    let visible: BTreeSet<String> = harw_extension_api::AgentSpawner::delegation_target_names(
        spawner.as_ref(),
        ctx.session_id(),
    )
    .into_iter()
    .collect();
    if visible.is_empty() {
        return Err(OpError::NotAvailable(format!(
            "delegate_wave: {NO_CAPABILITY}"
        )));
    }
    let caller = spawner.child_record(ctx.session_id());
    let declared =
        DeclaredTargets::for_caller(policy, caller.as_ref().map(|record| record.role.as_str()));
    let admission = admit_targets(request, &visible, &declared, policy);
    let budget = wave_budget_cap(spawner.child_budget(ctx.session_id()));

    let size = request.targets.len();
    let payloads: Vec<Value> = request
        .targets
        .iter()
        .enumerate()
        .map(|(position, target)| target.payload(position, size))
        .collect();
    let contracts: Vec<ChildReturnContract> = request
        .targets
        .iter()
        .map(|target| child_contract(ctx, &target.role))
        .collect();

    let mut statuses: Vec<Option<TargetStatus>> = (0..size).map(|_| None).collect();
    let mut queue: VecDeque<(usize, &'static str)> = VecDeque::new();
    for (position, reducer) in admission.iter().enumerate() {
        match reducer {
            Some(reducer) => queue.push_back((position, *reducer)),
            None => statuses[position] = Some(TargetStatus::Unavailable),
        }
    }
    tracing::info!(
        targets = size,
        admitted = queue.len(),
        join = request.join.as_str(),
        max_parallel = request.max_parallel,
        "delegate_wave.start"
    );

    let mut running: Vec<(usize, SlotFuture<'_>)> = Vec::new();
    loop {
        while running.len() < request.max_parallel {
            let Some((position, reducer)) = queue.pop_front() else {
                break;
            };
            let target = &request.targets[position];
            let future: SlotFuture<'_> = Box::pin(fanout_children(
                ctx,
                &target.role,
                std::slice::from_ref(&payloads[position]),
                reducer,
                budget,
                1,
                JoinSemantics::AllTerminal,
                contracts[position],
            ));
            running.push((position, future));
        }
        if running.is_empty() {
            break;
        }

        let (index, value) = std::future::poll_fn(|cx| {
            for (index, (_, future)) in running.iter_mut().enumerate() {
                if let Poll::Ready(value) = future.as_mut().poll(cx) {
                    return Poll::Ready((index, value));
                }
            }
            Poll::Pending
        })
        .await;
        let (position, _finished) = running.remove(index);
        let status = TargetStatus::from_fanout(value);
        let won = request.join == WaveJoin::Any && matches!(status, TargetStatus::Completed(_));
        statuses[position] = Some(status);
        if won {
            // Verwerfen bricht ab: der RAII-Guard in `fanout_children` gibt
            // jeden Slot frei, der Kind-Controller legt die Sitzung zurück.
            for (sibling, future) in running.drain(..) {
                drop(future);
                statuses[sibling] = Some(TargetStatus::Cancelled);
            }
            for (pending, _) in queue.drain(..) {
                statuses[pending] = Some(TargetStatus::Cancelled);
            }
            break;
        }
    }

    let targets: Vec<TargetReport> = request
        .targets
        .iter()
        .zip(statuses)
        .map(|(target, status)| TargetReport {
            id: target.id.clone(),
            role: target.role.clone(),
            status: status.unwrap_or_else(|| {
                TargetStatus::Failed("der Wellen-Scheduler lieferte kein Ergebnis".to_owned())
            }),
        })
        .collect();
    let report = DelegateWaveReport {
        join: request.join,
        max_parallel: request.max_parallel,
        targets,
    };
    tracing::info!(
        completed = report.count("completed"),
        failed = report.count("failed"),
        cancelled = report.count("cancelled"),
        unavailable = report.count("unavailable"),
        satisfied = report.is_satisfied(),
        "delegate_wave.complete"
    );
    Ok(report)
}

/// Der Return-Contract einer Zielrolle — dieselbe Auflösung wie
/// `resolve_child_contract` in `agent_tool.rs`: ohne Registry-Factory oder
/// ohne Label gilt [`ChildReturnContract::Text`].
fn child_contract(ctx: &OpContext, role: &str) -> ChildReturnContract {
    ctx.service::<Arc<dyn ChildRegistryFactory>>()
        .and_then(|factory| {
            factory
                .executable_agent_ir(role)
                .and_then(|ir| ir.return_pipeline().contract())
                .map(ChildReturnContract::parse)
        })
        .unwrap_or(ChildReturnContract::Text)
}

// ── Operation ─────────────────────────────────────────────────────────────────

/// Die Modell-Tool-Fläche von `delegate_wave`.
///
/// # Registrierung
/// Die Composition-Root hängt sie ausschließlich an die Registry von
/// Orchestrator-Sitzungen (`harw_registry_defaults::profile::
/// composition_tools_for_role`), z. B. über
/// `harw_operations::adapter::model_tool::ModelToolProvider::new(vec![Arc::new(op)], …)`.
/// Die `SessionActivation` des Kindes schaltet das Werkzeug nur frei, weil
/// die Orchestrator-TOMLs `delegate_wave` admittieren.
///
/// # Concurrency
/// `Send + Sync`; zustandslos bis auf die geteilte Politik.
pub struct DelegateWaveOperation {
    policy: DelegateWavePolicy,
}

impl DelegateWaveOperation {
    /// Baut die Operation über einer Politik.
    #[must_use]
    pub fn new(policy: DelegateWavePolicy) -> Self {
        Self { policy }
    }
}

/// Das geschlossene Argument-Schema (`additionalProperties: false`).
const DELEGATE_WAVE_ARGS_SCHEMA: ArgsSchemaFn = || {
    object_schema(
        vec![
            (
                "targets",
                array_schema(object_schema(
                    vec![
                        (
                            "id",
                            string_schema(
                                "Optionale, in der Welle eindeutige ID (Standard t1, t2, …); \
                                 ein ResearchFinding muss sie als question_id tragen.",
                            ),
                        ),
                        (
                            "role",
                            string_schema("Exakter Rollenname eines sichtbaren Delegationsziels."),
                        ),
                        ("task", string_schema("Abgegrenzter Auftrag dieses Kindes.")),
                        (
                            "complexity",
                            enum_string_schema(
                                "Steuert die Modellstufe des Kindes, nie seine Rechte.",
                                &["simple", "complex"],
                            ),
                        ),
                    ],
                    &["role", "task"],
                )),
            ),
            (
                "join",
                enum_string_schema(
                    "all = auf alle warten; any = erstes Ergebnis gewinnt; collect = alles \
                     sammeln, auch Fehlschläge.",
                    &["all", "any", "collect"],
                ),
            ),
            (
                "max_parallel",
                integer_schema("Höchstzahl gleichzeitig laufender Kinder (Standard 4)."),
            ),
        ],
        &["targets"],
    )
};

impl Operation for DelegateWaveOperation {
    fn meta(&self) -> &OperationMeta {
        static META: OnceLock<OperationMeta> = OnceLock::new();
        META.get_or_init(|| OperationMeta {
            name: DELEGATE_WAVE_TOOL,
            summary: "Startet eine Welle von Kind-Agenten (Rolle + Auftrag je Ziel) und liefert \
                      ihre Ergebnisse gesammelt zurück, ohne die eigene Sitzung zu pausieren.",
            domain: OperationDomain::Agents,
            permission: PermissionTier::Operator,
            surfaces: vec![Surface::ModelTool {
                readonly: false,
                approval: ApprovalPolicy::None,
            }],
            category: OperationCategory::Agent,
            args_schema: Some(DELEGATE_WAVE_ARGS_SCHEMA),
            ..OperationMeta::default()
        })
    }

    fn run<'a>(&'a self, ctx: &'a OpContext, input: OpInput) -> OpFuture<'a> {
        Box::pin(async move {
            if !input.invocation.is_model_tool() {
                return Err(invalid("is only available as a model tool"));
            }
            let request = DelegateWaveRequest::parse(input.invocation.json_args())?;
            delegate_wave(ctx, &request, &self.policy)
                .await
                .map(DelegateWaveReport::into_output)
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_core::child_controller::{AgentBudget, ManagedAgentSpawner};
    use harw_core::{ChildLimits, InMemoryStateStore, SessionManager, StateStore};
    use harw_operations::context::{OpContext, ServiceMap};
    use harw_operations::error::OpError;
    use harw_operations::operation::{OpInput, Operation, Surface};
    use harw_types::{SessionId, TenantId, TurnId, WorkspaceId};
    use serde_json::{Value, json};

    use super::{
        DeclaredTargets, DelegateWaveOperation, DelegateWavePolicy, DelegateWaveReport,
        DelegateWaveRequest, TargetReport, TargetStatus, WaveComplexity, WaveJoin, admit_targets,
        delegate_wave, is_pause_report, wave_budget_cap,
    };
    use crate::context_ext::OpContextCoreExt;
    use crate::test_support::{TestError, TestResult, ctx};

    fn policy() -> DelegateWavePolicy {
        DelegateWavePolicy::new(
            |role| match role {
                "explorer" => Some("reduce_to_read_explore"),
                "planner" => Some("reduce_to_read_registry"),
                "researcher-web" => Some("reduce_to_read_network"),
                _ => None,
            },
            |caller| match caller {
                "coding-orchestrator" => Some(vec!["planner".to_owned(), "explorer".to_owned()]),
                _ => None,
            },
        )
    }

    fn parse(args: Value) -> TestResult<DelegateWaveRequest> {
        DelegateWaveRequest::parse(&args)
            .map_err(|error| TestError::Unexpected(format!("parse schlug fehl: {error}")))
    }

    fn expect_invalid(args: Value, needle: &str) -> TestResult {
        match DelegateWaveRequest::parse(&args) {
            Err(OpError::InvalidArguments(message)) => {
                assert!(message.contains(needle), "{message} enthält nicht {needle}");
                Ok(())
            }
            other => Err(TestError::Unexpected(format!(
                "erwartet InvalidArguments({needle}), bekommen {other:?}"
            ))),
        }
    }

    #[test]
    fn test_parse_applies_defaults_and_caps_parallelism() -> TestResult {
        let request = parse(json!({
            "targets": [
                { "role": "explorer", "task": "Finde X" },
                { "role": "planner", "task": "Plane Y", "complexity": "complex", "id": "plan" },
            ]
        }))?;
        assert_eq!(request.join, WaveJoin::All);
        assert_eq!(request.max_parallel, 2, "Standard 4, gekappt auf 2 Ziele");
        assert_eq!(request.targets[0].id, "t1");
        assert_eq!(request.targets[1].id, "plan");
        assert_eq!(request.targets[1].complexity, Some(WaveComplexity::Complex));

        let request = parse(json!({
            "targets": [{ "role": "explorer", "task": "a" }],
            "join": "any",
            "max_parallel": 9,
        }))?;
        assert_eq!(request.join, WaveJoin::Any);
        assert_eq!(request.max_parallel, 1);
        Ok(())
    }

    #[test]
    fn test_parse_rejects_malformed_waves() -> TestResult {
        expect_invalid(json!([]), "JSON object")?;
        expect_invalid(json!({ "targets": [] }), "non-empty array")?;
        expect_invalid(
            json!({ "targets": [{ "role": "explorer", "task": "a" }], "effort": "high" }),
            "`effort`",
        )?;
        expect_invalid(
            json!({ "targets": [{ "role": "explorer", "task": "a", "budget": "1k_tokens" }] }),
            "`budget`",
        )?;
        expect_invalid(json!({ "targets": [{ "role": "", "task": "a" }] }), "role")?;
        expect_invalid(json!({ "targets": [{ "role": "explorer" }] }), "task")?;
        expect_invalid(
            json!({ "targets": [{ "role": "explorer", "task": "a", "complexity": "hard" }] }),
            "complexity",
        )?;
        expect_invalid(
            json!({ "targets": [
                { "role": "explorer", "task": "a", "id": "x" },
                { "role": "planner", "task": "b", "id": "x" },
            ] }),
            "not unique",
        )?;
        expect_invalid(
            json!({ "targets": [
                { "role": "explorer", "task": "a", "id": "t2" },
                { "role": "planner", "task": "b" },
            ] }),
            "not unique",
        )?;
        expect_invalid(
            json!({ "targets": [{ "role": "explorer", "task": "a" }], "join": "some" }),
            "`join`",
        )?;
        expect_invalid(
            json!({ "targets": [{ "role": "explorer", "task": "a" }], "max_parallel": 0 }),
            "max_parallel",
        )?;
        let too_many: Vec<Value> = (0..=super::MAX_WAVE_TARGETS)
            .map(|index| json!({ "role": "explorer", "task": format!("q{index}") }))
            .collect();
        expect_invalid(json!({ "targets": too_many }), "at most")?;
        Ok(())
    }

    #[test]
    fn test_payload_binds_question_id_and_complexity() -> TestResult {
        let request = parse(json!({
            "targets": [{ "role": "explorer", "task": "Finde X", "complexity": "simple" }]
        }))?;
        let payload = request.targets[0].payload(0, 1);
        assert_eq!(payload["question"]["id"], json!("t1"));
        assert_eq!(payload["question"]["question"], json!("Finde X"));
        assert_eq!(payload["complexity"], json!("simple"));
        assert_eq!(payload["wave"]["position"], json!(1));
        let plain = parse(json!({ "targets": [{ "role": "explorer", "task": "a" }] }))?;
        assert!(plain.targets[0].payload(0, 1).get("complexity").is_none());
        Ok(())
    }

    #[test]
    fn test_admission_is_an_intersection_of_visibility_declaration_and_reducer() -> TestResult {
        let request = parse(json!({ "targets": [
            { "role": "explorer", "task": "a" },
            { "role": "planner", "task": "b" },
            { "role": "researcher-web", "task": "c" },
            { "role": "gibt-es-nicht", "task": "d" },
            { "role": "executor", "task": "e" },
        ] }))?;
        let visible = ["explorer", "researcher-web", "executor", "planner"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let policy = policy();

        // Child-Orchestrator: nur seine deklarierten Ziele.
        let declared = DeclaredTargets::for_caller(&policy, Some("coding-orchestrator"));
        assert_eq!(
            admit_targets(&request, &visible, &declared, &policy),
            vec![
                Some("reduce_to_read_explore"),
                Some("reduce_to_read_registry"),
                None, // sichtbar, aber nicht deklariert
                None, // unbekannt
                None, // sichtbar, aber ohne Reducer
            ]
        );

        // Wurzelsitzung: allein die Sichtbarkeit (plus Reducer).
        let unrestricted = DeclaredTargets::for_caller(&policy, None);
        assert_eq!(unrestricted, DeclaredTargets::Unrestricted);
        assert_eq!(
            admit_targets(&request, &visible, &unrestricted, &policy),
            vec![
                Some("reduce_to_read_explore"),
                Some("reduce_to_read_registry"),
                Some("reduce_to_read_network"),
                None,
                None,
            ]
        );

        // Bekannte Rolle ohne Deklaration (ein Worker): fail-closed.
        let worker = DeclaredTargets::for_caller(&policy, Some("explorer"));
        assert!(
            admit_targets(&request, &visible, &worker, &policy)
                .iter()
                .all(Option::is_none)
        );
        Ok(())
    }

    fn report(join: WaveJoin, statuses: Vec<TargetStatus>) -> DelegateWaveReport {
        DelegateWaveReport {
            join,
            max_parallel: 2,
            targets: statuses
                .into_iter()
                .enumerate()
                .map(|(index, status)| TargetReport {
                    id: format!("t{}", index + 1),
                    role: "explorer".to_owned(),
                    status,
                })
                .collect(),
        }
    }

    #[test]
    fn test_report_satisfaction_follows_the_join() {
        let mixed = || {
            vec![
                TargetStatus::Completed(json!({ "ok": true })),
                TargetStatus::Failed("kaputt".to_owned()),
            ]
        };
        assert!(!report(WaveJoin::All, mixed()).is_satisfied());
        assert!(report(WaveJoin::Any, mixed()).is_satisfied());
        assert!(report(WaveJoin::Collect, mixed()).is_satisfied());
        assert!(
            !report(
                WaveJoin::Collect,
                vec![
                    TargetStatus::Failed("x".to_owned()),
                    TargetStatus::Cancelled
                ]
            )
            .is_satisfied()
        );
        assert!(
            !report(
                WaveJoin::Any,
                vec![TargetStatus::Paused(
                    json!({ "paused": "approval", "child": "c" })
                )]
            )
            .is_satisfied(),
            "eine Pause ist kein Abschluss"
        );
    }

    #[test]
    fn test_report_json_never_names_why_a_target_was_unavailable() {
        let rendered = report(
            WaveJoin::Collect,
            vec![
                TargetStatus::Completed(json!("Antwort")),
                TargetStatus::Unavailable,
                TargetStatus::Cancelled,
            ],
        )
        .to_json();
        assert_eq!(rendered["counts"]["completed"], json!(1));
        assert_eq!(rendered["counts"]["unavailable"], json!(1));
        assert_eq!(rendered["results"][0]["result"], json!("Antwort"));
        assert_eq!(rendered["results"][1]["status"], json!("unavailable"));
        assert_eq!(
            rendered["results"][1]["error"],
            json!("no delegation capability is available for this request")
        );
        assert!(rendered["results"][2].get("error").is_none());
        assert_eq!(rendered["satisfied"], json!(false));
    }

    #[test]
    fn test_from_fanout_distinguishes_pause_result_and_errors() {
        assert!(matches!(
            TargetStatus::from_fanout(Ok(vec![Ok(json!({ "paused": "child", "child": "c" }))])),
            TargetStatus::Paused(_)
        ));
        assert!(matches!(
            TargetStatus::from_fanout(Ok(vec![Ok(
                json!({ "paused": "child", "extra": 1, "child": "c" })
            )])),
            TargetStatus::Completed(_)
        ));
        assert!(matches!(
            TargetStatus::from_fanout(Ok(vec![Err("nope".to_owned())])),
            TargetStatus::Failed(message) if message == "nope"
        ));
        assert!(matches!(
            TargetStatus::from_fanout(Ok(Vec::new())),
            TargetStatus::Failed(_)
        ));
        assert!(matches!(
            TargetStatus::from_fanout(Err(OpError::NotAvailable("weg".to_owned()))),
            TargetStatus::Failed(_)
        ));
        assert!(!is_pause_report(&json!("text")));
    }

    #[test]
    fn test_wave_budget_cap_is_the_callers_budget_or_no_cap() {
        let caller = AgentBudget {
            max_tokens: Some(10),
            ..AgentBudget::default()
        };
        assert_eq!(wave_budget_cap(Some(caller)), caller);
        assert_eq!(wave_budget_cap(None), AgentBudget::default());
    }

    #[test]
    fn test_operation_is_a_closed_schema_model_tool() {
        let operation = DelegateWaveOperation::new(policy());
        let meta = operation.meta();
        assert_eq!(meta.name, "delegate_wave");
        assert!(
            meta.surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::ModelTool { .. }))
        );
        assert!(
            !meta
                .surfaces
                .iter()
                .any(|surface| matches!(surface, Surface::Command { .. })),
            "kein Slash-Command: nur Orchestrator-Sitzungen rufen es"
        );
        assert!(meta.args_schema.is_some());
    }

    /// Minimaler Kontext mit echter Core-Laufzeit (Spawner ohne Rollen).
    fn runtime_ctx() -> TestResult<(OpContext, PathBuf, Box<dyn std::any::Any>)> {
        let tmp = std::env::temp_dir().join(format!(
            "harw-delegate-wave-test-{}-{}",
            std::process::id(),
            SessionId::new()
        ));
        std::fs::create_dir_all(tmp.join("ws")).map_err(ctx("Test-Workspace anlegen"))?;
        let registry = WorkspaceRegistry::build(
            &tmp,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: PathBuf::from("ws"),
            }],
        )
        .map_err(ctx("Workspace-Registry aufbauen"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .map_err(ctx("Workspace auflösen"))?;
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
        let spawner = Arc::new(ManagedAgentSpawner::new(
            Arc::new(Mutex::new(SessionManager::new(event_tx))),
            ChildLimits::conservative(),
        ));
        let store: Arc<dyn StateStore> = Arc::new(InMemoryStateStore::new());
        let mut services = ServiceMap::new();
        <OpContext as OpContextCoreExt>::register_agent_tool_services(
            &mut services,
            spawner,
            store,
        );
        let ctx = OpContext::new(SessionId::new(), TurnId::new(), sandbox, services);
        Ok((ctx, tmp, Box::new(event_rx)))
    }

    #[tokio::test]
    async fn test_delegate_wave_without_spawner_is_not_available() -> TestResult {
        let (runtime, tmp, _events) = runtime_ctx()?;
        let bare = OpContext::new(
            SessionId::new(),
            TurnId::new(),
            runtime.sandbox().clone(),
            ServiceMap::new(),
        );
        let request = parse(json!({ "targets": [{ "role": "explorer", "task": "a" }] }))?;
        let result = delegate_wave(&bare, &request, &policy()).await;
        assert!(
            matches!(result, Err(OpError::NotAvailable(_))),
            "{result:?}"
        );
        let _ = std::fs::remove_dir_all(tmp);
        Ok(())
    }

    /// Eine Sitzung, die der Spawner nicht kennt, sieht kein Ziel — der
    /// Aufruf scheitert als Ganzes mit der einheitlichen Meldung, ohne eine
    /// einzige Rolle zu nennen.
    #[tokio::test]
    async fn test_delegate_wave_without_visible_targets_names_no_role() -> TestResult {
        let (runtime, tmp, _events) = runtime_ctx()?;
        let operation = DelegateWaveOperation::new(policy());
        let result = operation
            .run(
                &runtime,
                OpInput::model_tool(json!({ "targets": [{ "role": "explorer", "task": "a" }] })),
            )
            .await;
        match result {
            Err(OpError::NotAvailable(message)) => {
                assert!(message.contains("no delegation capability"), "{message}");
                assert!(!message.contains("explorer"), "{message}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartet NotAvailable, bekommen {other:?}"
                )));
            }
        }
        let _ = std::fs::remove_dir_all(tmp);
        Ok(())
    }

    #[tokio::test]
    async fn test_operation_rejects_command_invocations() -> TestResult {
        let (runtime, tmp, _events) = runtime_ctx()?;
        let operation = DelegateWaveOperation::new(policy());
        let result = operation
            .run(&runtime, OpInput::command("/delegate_wave", Vec::new()))
            .await;
        assert!(
            matches!(result, Err(OpError::InvalidArguments(_))),
            "{result:?}"
        );
        let _ = std::fs::remove_dir_all(tmp);
        Ok(())
    }
}
