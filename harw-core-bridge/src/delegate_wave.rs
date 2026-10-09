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
//! docs/design/agents-as-tools.md` §3).
//!
//! ```text
//! delegate_wave {
//!   targets: [{ role, task, complexity?, id?, continue_from? }, …],   // 1 ..= 16
//!   join: "all" | "any" | "collect",                 // Standard "all"
//!   max_parallel: n                                  // Standard: alle Ziele gleichzeitig
//! }
//! ```
//!
//! # Admission (Plan R9, Teil C/E1)
//! Die Ziele sind die **sichtbaren** Ziele des Aufrufers
//! (`AgentSpawner::delegation_targets` → `delegation_visibility`:
//! Rollenmatrix, exakte `child_orchestrators`-Freigabe, Resttiefe und die
//! Plan-Modus-Regel — im Plan-Modus nur lesende Ziele). Davon bleiben die
//! Rollen mit bekanntem Autoritäts-Reducer (fail-closed). Die in der
//! Definition deklarierten Ziele (`[delegation].targets`, über
//! [`DelegateWavePolicy`]) verengen nur optional:
//! - eine Deklaration, die ausschließlich eingebaute Rollen nennt (die
//!   eingebauten Autoritätslisten), verengt nur eingebaute Ziele —
//!   benutzerdefinierte Agenten bleiben sichtbar;
//! - eine Deklaration, die benutzerdefinierte Agenten nennt (etwa die
//!   eigene Liste einer Analyse-Familie), ist vollständig;
//! - bliebe nach der Verengung nichts übrig, gilt die sichtbare Menge.
//!
//! Ein abgelehntes Ziel erscheint als `"unavailable"` mit einer benannten,
//! aufruferbezogenen Meldung: „Ziel X ist für dich nicht delegierbar;
//! delegierbar sind: …“ bzw. die Plan-Modus-Meldung. Genannt werden nur Ziele,
//! die der Aufrufer ohnehin sieht — kein Agentenkatalog-Orakel.
//!
//! # Ausführung
//! Admission ist fail-fast und all-or-nothing: es gibt keine Warteschlange.
//! Vor dem ersten Start wird geprüft, dass **alle** zugelassenen Ziele jetzt
//! gleichzeitig laufen dürfen (`max_parallel`, Rollenkappung, freie
//! Kind-Slots, Tiefe, Orchestrierungsgrenzen). Ist das nicht der Fall, wird
//! **nichts** gestartet und ein detaillierter Fehler zurückgegeben.
//! Jedes zugelassene Ziel belegt dann einen Platz. Jeder Platz ist genau ein
//! [`crate::fanout_children`]-Aufruf mit einer einzigen Frage — damit gelten
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
//! # Fortsetzung (Runde 5, Teil J)
//! Ein Ziel mit `continue_from: <child_id>` setzt ein eigenes Kind fort, das
//! an seinem Token-Budget endete (`harw_core::child_handoff`). Es muss
//! dieselbe Rolle tragen und durchläuft dieselbe Zulassung wie jedes Ziel;
//! danach prüft der Spawner Eigentum, Budget-Ende, Rolle und die Kettengrenze
//! (höchstens drei Fortsetzungen je ursprünglichem Kind) und nach der
//! Admission, dass die Sandbox nicht weiter ist als die des Vorgängers. Das
//! neue Kind bekommt die Übergabe als ersten Kontext (`task` optional) und
//! ein frisches Budget; im Agent-Panel heißt es „Fortsetzung von <id>“.
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
//! Sitzung ohne delegierbares Ziel ([`OpError::NotAvailable`] mit benanntem
//! Grund: „Restliche Spawn-Tiefe 0“, „Kein Spawn-Kontext (interner
//! Fehler)“, die Plan-Modus-Meldung oder „keine delegierbaren Ziele“). Das
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

// Runde 5, Teil J: Fortsetzung budget-beendeter Kinder (`continue_from`).
use harw_core::child_controller::ManagedAgentSpawner;
use harw_core::child_handoff::{ContinuationSeed, continuation_task};
use harw_types::SessionId;

use crate::agent_tool::{ChildReturnContract, fanout_children_with};
use crate::context_ext::OpContextCoreExt;

/// Werkzeugname, unter dem die Operation dem Modell erscheint.
pub const DELEGATE_WAVE_TOOL: &str = "delegate_wave";

/// Höchstzahl Ziele je Welle. Eine größere Welle ist ein Zerlegungsfehler des
/// Orchestrators, kein Fan-out.
pub const MAX_WAVE_TARGETS: usize = 16;

/// Höchstlänge einer vom Modell vergebenen Ziel-ID.
const MAX_TARGET_ID_CHARS: usize = 64;

/// Meldung, wenn der Aufrufer sichtbare Ziele hat, aber keines davon für
/// `delegate_wave` taugt oder überhaupt keines sieht.
const NO_TARGETS: &str = "keine delegierbaren Ziele für dich";

/// Erlaubte Felder auf oberster Ebene.
const TOP_LEVEL_FIELDS: &[&str] = &["targets", "join", "max_parallel"];

/// Erlaubte Felder je Ziel.
/// Runde 5, Teil J: `continue_from` setzt ein budget-beendetes Kind fort.
/// Plan R9, Teil C: `user_approved` wie bei `transfer_to_*` (Runde 9, E4).
const TARGET_FIELDS: &[&str] = &[
    "id",
    "role",
    "task",
    "complexity",
    "continue_from",
    harw_core::user_approval::USER_APPROVED_FIELD,
];

// ── Anfrage ───────────────────────────────────────────────────────────────────

/// Join-Semantik einer Welle, wie das Modell sie benennt.
///
/// # Concurrency
/// `Copy`, zustandslos.
// `KebabEnum` liefert `as_str` und `parse` (`Option`, unbekannt → `None`).
// Die Labels sind einwortig, kebab- und snake_case fallen also zusammen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, harw_macros::KebabEnum)]
#[kebab_enum(case = "snake", parse_option, no_from_str)]
pub enum WaveJoin {
    /// `"all"` → [`JoinSemantics::AllTerminal`].
    All,
    /// `"any"` → [`JoinSemantics::AnyTerminal`].
    Any,
    /// `"collect"` → [`JoinSemantics::Collect`].
    Collect,
}

impl WaveJoin {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, harw_macros::KebabEnum)]
#[kebab_enum(case = "snake", no_from_str)]
pub enum WaveComplexity {
    /// `"simple"`.
    Simple,
    /// `"complex"`.
    Complex,
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
    /// Runde 5, Teil J: ID eines eigenen, am Budget beendeten Kindes, das
    /// dieses Ziel (mit derselben Rolle) fortsetzt. `task` darf dann leer
    /// sein (Vorgabe: die offenen Punkte der Übergabe abarbeiten).
    pub continue_from: Option<String>,
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
        self.payload_with_task(&self.task, position, size)
    }

    /// Wie [`Self::payload`], aber mit einem anderen Auftragstext — für eine
    /// Fortsetzung (Runde 5, Teil J) die Übergabe samt Kennzeichnung
    /// (`harw_core::child_handoff::continuation_task`). Zusätzlich trägt die
    /// Nutzlast dann `continuation.of`.
    #[must_use]
    pub fn payload_with_task(&self, task: &str, position: usize, size: usize) -> Value {
        let mut payload = json!({
            "id": self.id,
            "role": self.role,
            "task": task,
            "question": { "id": self.id, "question": task },
            "wave": { "tool": DELEGATE_WAVE_TOOL, "position": position + 1, "size": size },
        });
        if let (Some(of), Some(object)) = (&self.continue_from, payload.as_object_mut()) {
            object.insert("continuation".to_owned(), json!({ "of": of }));
        }
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
            // Without an explicit `max_parallel` all targets run at once: a
            // wave never queues targets behind a smaller pool.
            None | Some(Value::Null) => targets.len(),
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
    // Runde 5, Teil J: eine Fortsetzung darf ohne eigenen Auftrag kommen.
    let continue_from = match object.get("continue_from") {
        None | Some(Value::Null) => None,
        Some(Value::String(id)) if !id.trim().is_empty() => Some(id.trim().to_owned()),
        Some(_) => {
            return Err(invalid(&format!(
                "`targets[{index}].continue_from` must be a non-empty child id"
            )));
        }
    };
    let task = match (&continue_from, object.get("task")) {
        (Some(_), None | Some(Value::Null)) => String::new(),
        (Some(_), Some(Value::String(text))) => text.trim().to_owned(),
        _ => required_text(object, "task", index)?,
    };
    // Plan R9, Teil C: vorab erteilte Freigaben der Nutzerin führen den
    // Auftrag an — dieselbe Zeile wie bei `transfer_to_*` (Runde 9, E4).
    let task = harw_core::user_approval::with_user_approval(
        Some(task).filter(|task| !task.is_empty()),
        raw,
    )
    .unwrap_or_default();
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
        continue_from,
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
    /// Plan R9, Teil C: ob ein Ziel ein benutzerdefinierter Agent ist (siehe
    /// Moduldoku, „Admission“). Vorgabe: keines.
    is_custom_target: Arc<dyn Fn(&str) -> bool + Send + Sync>,
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
            is_custom_target: Arc::new(|_| false),
        }
    }

    /// Plan R9, Teil C: kennzeichnet benutzerdefinierte Agenten. Eine
    /// Deklaration, die nur eingebaute Rollen nennt, verengt sie nicht.
    #[must_use]
    pub fn with_custom_targets(
        mut self,
        is_custom_target: impl Fn(&str) -> bool + Send + Sync + 'static,
    ) -> Self {
        self.is_custom_target = Arc::new(is_custom_target);
        self
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

    /// Ob die Deklaration `role` durchlässt (siehe Moduldoku, „Admission“):
    /// genannt, oder ein benutzerdefinierter Agent, solange die Deklaration
    /// selbst keinen benutzerdefinierten Agenten nennt.
    fn admits(&self, role: &str, policy: &DelegateWavePolicy) -> bool {
        match self {
            Self::Unrestricted => true,
            Self::Only(roles) => {
                roles.contains(role)
                    || ((policy.is_custom_target)(role)
                        && !roles
                            .iter()
                            .any(|named| (policy.is_custom_target)(named.as_str())))
            }
        }
    }
}

/// Die für `delegate_wave` delegierbaren Rollen des Aufrufers.
///
/// # Beschreibung
/// Sichtbare Rollen mit Reducer, optional verengt durch die Deklaration;
/// bliebe nach der Verengung nichts übrig, gelten alle sichtbaren Rollen mit
/// Reducer (Plan R9, Teil C: die Deklaration ist nur ein Filter).
///
/// # Returns
/// Sortiert (Cache-stabil, deterministische Meldungen).
#[must_use]
pub fn delegable_roles(
    visible: &BTreeSet<String>,
    declared: &DeclaredTargets,
    policy: &DelegateWavePolicy,
) -> Vec<String> {
    let with_reducer: Vec<String> = visible
        .iter()
        .filter(|role| (policy.reducer_for_role)(role.as_str()).is_some())
        .cloned()
        .collect();
    let narrowed: Vec<String> = with_reducer
        .iter()
        .filter(|role| declared.admits(role.as_str(), policy))
        .cloned()
        .collect();
    if narrowed.is_empty() {
        with_reducer
    } else {
        narrowed
    }
}

/// Entscheidet je Ziel über die Zulassung.
///
/// # Argumente
/// - `request`: die geprüfte Anfrage.
/// - `visible`: die Laufzeit-Sichtbarkeit des Aufrufers (bereits nach
///   Plan-Modus gefiltert).
/// - `withheld`: sichtbare Ziele, die allein der Plan-Modus zurückhält.
/// - `declared`: die deklarierte Zielmenge des Aufrufers.
/// - `policy`: liefert die Reducer-Kennungen.
///
/// # Returns
/// Positionsgleich zu `request.targets`: `Ok(reducer)` für ein zugelassenes
/// Ziel, `Err(meldung)` für ein abgelehntes — die Plan-Modus-Meldung oder
/// „Ziel X ist für dich nicht delegierbar; delegierbar sind: …“ (nur Ziele,
/// die der Aufrufer ohnehin sieht).
#[must_use]
pub fn admit_targets(
    request: &DelegateWaveRequest,
    visible: &BTreeSet<String>,
    withheld: &BTreeSet<String>,
    declared: &DeclaredTargets,
    policy: &DelegateWavePolicy,
) -> Vec<Result<&'static str, String>> {
    let delegable = delegable_roles(visible, declared, policy);
    request
        .targets
        .iter()
        .map(|target| {
            let role = target.role.as_str();
            if withheld.contains(role) {
                return Err(harw_core::delegation_visibility::plan_mode_refusal(
                    &delegable,
                ));
            }
            if !delegable.iter().any(|allowed| allowed == role) {
                return Err(harw_core::delegation_visibility::not_delegable_message(
                    role, &delegable,
                ));
            }
            (policy.reducer_for_role)(role).ok_or_else(|| {
                harw_core::delegation_visibility::not_delegable_message(role, &delegable)
            })
        })
        .collect()
}

/// Der Budgetdeckel jedes Kindes einer Welle.
///
/// # Beschreibung
/// Quelle ist das **Restbudget** des Aufrufers
/// (`ManagedAgentSpawner::remaining_budget`: Budget minus Verbrauch seiner
/// Kinder; `None` für die Wurzelsitzung → kein zusätzlicher Deckel).
/// `fanout_children` verschneidet ihn je Kind mit dessen Agent-IR, je
/// Dimension gewinnt die strengere Grenze.
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
    /// Nicht zugelassen, mit benanntem Grund (siehe Moduldoku, „Admission“).
    Unavailable(String),
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
            Self::Unavailable(_) => "unavailable",
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
                        TargetStatus::Unavailable(message) => {
                            object.insert("error".to_owned(), json!(message));
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

    // Plan R9, Teil C/E1: die sichtbaren Ziele samt Plan-Modus-Regel; ein
    // Aufrufer ohne Ziele bekommt den benannten Grund (Resttiefe 0, kein
    // Spawn-Kontext, Plan-Modus), nie eine Pauschalmeldung.
    let delegation = harw_extension_api::AgentSpawner::delegation_targets(
        spawner.as_ref(),
        ctx.session_id(),
        false,
    )
    .map_err(|error| OpError::NotAvailable(format!("delegate_wave: {error}")))?;
    let visible: BTreeSet<String> = delegation
        .targets
        .iter()
        .map(|target| target.name.clone())
        .collect();
    let withheld: BTreeSet<String> = delegation
        .withheld_by_plan_mode
        .iter()
        .map(|target| target.name.clone())
        .collect();
    let caller = spawner.child_record(ctx.session_id());
    let declared =
        DeclaredTargets::for_caller(policy, caller.as_ref().map(|record| record.role.as_str()));
    let delegable = delegable_roles(&visible, &declared, policy);
    if delegable.is_empty() {
        let reason = if withheld.is_empty() {
            NO_TARGETS.to_owned()
        } else {
            harw_core::delegation_visibility::plan_mode_refusal(&[])
        };
        return Err(OpError::NotAvailable(format!("delegate_wave: {reason}")));
    }
    let admission = admit_targets(request, &visible, &withheld, &declared, policy);
    let budget = wave_budget_cap(spawner.remaining_budget(ctx.session_id()));

    let size = request.targets.len();
    let mut statuses: Vec<Option<TargetStatus>> = (0..size).map(|_| None).collect();
    // Runde 5, Teil J: Fortsetzungen werden erst nach der regulären
    // Zulassung geprüft (nur eigene, budget-beendete Kinder derselben Rolle,
    // Kettengrenze); ein abgelehntes Ziel verrät dabei nichts Neues.
    let mut seeds: Vec<Option<ContinuationSeed>> = (0..size).map(|_| None).collect();
    let mut payloads: Vec<Value> = Vec::with_capacity(size);
    for (position, target) in request.targets.iter().enumerate() {
        let continuation = match (&target.continue_from, &admission[position]) {
            (Some(from), Ok(_)) => Some(prepare_continuation(ctx, &spawner, target, from)),
            _ => None,
        };
        payloads.push(match continuation {
            Some(Ok(seed)) => {
                let task = continuation_task(
                    &seed.of,
                    &seed.end,
                    &seed.handoff,
                    Some(target.task.as_str()),
                );
                seeds[position] = Some(seed);
                target.payload_with_task(&task, position, size)
            }
            Some(Err(message)) => {
                statuses[position] = Some(TargetStatus::Failed(message));
                target.payload(position, size)
            }
            None => target.payload(position, size),
        });
    }
    // #22 Welle 1B: ein unbekannter Contract ist ein Fehler des Ziels, kein
    // stiller Freitext-Rückfall.
    let contract_results: Vec<Result<ChildReturnContract, String>> = request
        .targets
        .iter()
        .map(|target| child_contract(ctx, &target.role))
        .collect();
    let contracts: Vec<ChildReturnContract> = contract_results
        .iter()
        .map(|contract| contract.clone().unwrap_or(ChildReturnContract::Text))
        .collect();

    let mut queue: VecDeque<(usize, &'static str)> = VecDeque::new();
    for (position, reducer) in admission.into_iter().enumerate() {
        match reducer {
            // Eine abgelehnte Fortsetzung steht schon als `failed` fest.
            Ok(_) if statuses[position].is_some() => {}
            Ok(_) if contract_results[position].is_err() => {
                let message = contract_results[position].clone().err().unwrap_or_default();
                statuses[position] = Some(TargetStatus::Failed(message));
            }
            Ok(reducer) => queue.push_back((position, reducer)),
            Err(message) => statuses[position] = Some(TargetStatus::Unavailable(message)),
        }
    }
    // Fail-fast, all-or-nothing admission: no target is queued behind a free
    // slot. If not every runnable target can start right now, nothing starts.
    if queue.len() > request.max_parallel {
        return Err(OpError::NotAvailable(format!(
            "delegate_wave: {} {} targets are runnable but max_parallel is {}; targets are \
             never queued behind a smaller pool. Nothing was started. {} Send at most {} \
             target(s) per wave, or raise max_parallel.",
            harw_core::background_children::DELEGATION_REJECTED_MARKER,
            queue.len(),
            request.max_parallel,
            harw_extension_api::FAIL_FAST_CONSEQUENCE,
            request.max_parallel,
        )));
    }
    let wave_roles: Vec<&str> = queue
        .iter()
        .map(|(position, _)| request.targets[*position].role.as_str())
        .collect();
    spawner
        .preflight_wave_admission(ctx.session_id(), &wave_roles)
        .map_err(|error| OpError::NotAvailable(format!("delegate_wave: {error}")))?;
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
            let future: SlotFuture<'_> = Box::pin(fanout_children_with(
                ctx,
                &target.role,
                std::slice::from_ref(&payloads[position]),
                reducer,
                budget,
                1,
                JoinSemantics::AllTerminal,
                contracts[position],
                // Runde 5, Teil J: Fortsetzung binden, sonst `None`.
                seeds[position].as_ref(),
            ));
            running.push((position, future));
        }
        if running.is_empty() {
            break;
        }

        // TODO(PL-90 background-only): this is a blocking join. Delegated work
        // must run only in the background and report back automatically;
        // converting the synchronous wave join is a separate, later wave.
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
///
/// # Errors
/// Die Meldung für den `failed`-Eintrag, wenn die IR ein unbekanntes
/// Contract-Label trägt (#22 Welle 1B, [`ChildReturnContract::parse`]).
fn child_contract(ctx: &OpContext, role: &str) -> Result<ChildReturnContract, String> {
    let Some(label) = ctx
        .service::<Arc<dyn ChildRegistryFactory>>()
        .and_then(|factory| {
            factory
                .executable_agent_ir(role)
                .and_then(|ir| ir.return_pipeline().contract())
                .map(str::to_owned)
        })
    else {
        return Ok(ChildReturnContract::Text);
    };
    ChildReturnContract::parse(&label)
        .map_err(|error| format!("Zielrolle '{role}' kann nicht gestartet werden: {error}"))
}

/// Runde 5, Teil J: prüft ein `continue_from`-Ziel gegen das
/// Fortsetzungs-Buch des Spawners (eigenes Kind des Aufrufers, am Budget
/// beendet, dieselbe Rolle, Kettengrenze). Der Aufrufer ist die Sitzung aus
/// dem Ausführungskontext, nie ein Modell-Argument.
///
/// # Errors
/// Die Meldung für den `failed`-Eintrag des Ziels.
fn prepare_continuation(
    ctx: &OpContext,
    spawner: &ManagedAgentSpawner,
    target: &WaveTarget,
    from: &str,
) -> Result<ContinuationSeed, String> {
    let from = SessionId::try_from_str(from.to_owned())
        .map_err(|_| format!("continue_from: '{from}' ist keine gültige Kind-ID"))?;
    spawner
        .prepare_continuation(ctx.session_id(), &from, &target.role)
        .map_err(|error| error.message)
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
                            string_schema(
                                "Sichtbares Delegationsziel (die Liste steht als Enum hier; \
                                 Details: agents.catalog).",
                            ),
                        ),
                        (
                            "task",
                            string_schema(
                                "Abgegrenzter Auftrag dieses Kindes (Pflicht, außer bei \
                                 continue_from).",
                            ),
                        ),
                        (
                            "complexity",
                            enum_string_schema(
                                "Steuert die Modellstufe des Kindes, nie seine Rechte.",
                                &["simple", "complex"],
                            ),
                        ),
                        // Runde 5, Teil J: Fortsetzung eines budget-beendeten Kindes.
                        (
                            "continue_from",
                            string_schema(
                                "Optional: ID eines eigenen Kindes, das am Token-Budget endete \
                                 (siehe dessen Übergabe). Das neue Kind derselben Rolle bekommt \
                                 die Übergabe als Kontext und ein frisches Budget; höchstens 3 \
                                 Fortsetzungen je ursprünglichem Kind.",
                            ),
                        ),
                        // Plan R9, Teil C: wie bei `transfer_to_*` (Runde 9, E4).
                        (
                            harw_core::user_approval::USER_APPROVED_FIELD,
                            harw_core::user_approval::user_approved_schema(),
                        ),
                    ],
                    &["role"],
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

// Plan R9, Teil C/E1: echte Kette UIA → Root-Orchestrator → `delegate_wave`
// mitten im Turn (`delegate_wave/chain_tests.rs`).
#[cfg(test)]
mod chain_tests;

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
        delegable_roles, delegate_wave, is_pause_report, wave_budget_cap,
    };
    use crate::context_ext::OpContextCoreExt;
    use crate::test_support::{TestError, TestResult, ctx};
    use std::collections::BTreeSet;

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
        assert_eq!(request.max_parallel, 2, "Standard: alle Ziele gleichzeitig");
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

    /// Runde 5, Teil J: `continue_from` macht den Auftrag optional und
    /// landet als `continuation.of` in der Nutzlast.
    #[test]
    fn test_parse_accepts_a_continuation_without_task() -> TestResult {
        let request = parse(json!({
            "targets": [
                { "role": "explorer", "continue_from": " child-1 " },
                { "role": "explorer", "task": "nur Modul C", "continue_from": "child-2" },
            ]
        }))?;
        assert_eq!(request.targets[0].continue_from.as_deref(), Some("child-1"));
        assert_eq!(request.targets[0].task, "");
        assert_eq!(request.targets[1].task, "nur Modul C");
        let payload = request.targets[1].payload_with_task("Fortsetzung von child-2: …", 1, 2);
        assert_eq!(payload["continuation"]["of"], json!("child-2"));
        assert_eq!(payload["task"], json!("Fortsetzung von child-2: …"));
        assert_eq!(
            payload["question"]["question"],
            json!("Fortsetzung von child-2: …")
        );
        let plain = parse(json!({ "targets": [{ "role": "explorer", "task": "a" }] }))?;
        assert!(plain.targets[0].payload(0, 1).get("continuation").is_none());

        expect_invalid(
            json!({ "targets": [{ "role": "explorer", "continue_from": "" }] }),
            "continue_from",
        )?;
        expect_invalid(
            json!({ "targets": [{ "role": "explorer", "continue_from": 7 }] }),
            "continue_from",
        )?;
        // Ohne `continue_from` bleibt der Auftrag Pflicht.
        expect_invalid(json!({ "targets": [{ "role": "explorer" }] }), "task")?;
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

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    /// Plan R9, Teil C: sichtbar ∩ Reducer, optional verengt durch die
    /// Deklaration; jede Ablehnung nennt nur sichtbare Ziele.
    #[test]
    fn test_admission_uses_visibility_reducer_and_optional_declaration() -> TestResult {
        let request = parse(json!({ "targets": [
            { "role": "explorer", "task": "a" },
            { "role": "planner", "task": "b" },
            { "role": "researcher-web", "task": "c" },
            { "role": "gibt-es-nicht", "task": "d" },
            { "role": "executor", "task": "e" },
        ] }))?;
        let visible = set(&["explorer", "researcher-web", "executor", "planner"]);
        let policy = policy();

        // Child-Orchestrator: seine Deklaration verengt.
        let declared = DeclaredTargets::for_caller(&policy, Some("coding-orchestrator"));
        let refusal = |role: &str| {
            format!(
                "Ziel {role} ist für dich nicht delegierbar; delegierbar sind: explorer, planner"
            )
        };
        assert_eq!(
            admit_targets(&request, &visible, &BTreeSet::new(), &declared, &policy),
            vec![
                Ok("reduce_to_read_explore"),
                Ok("reduce_to_read_registry"),
                Err(refusal("researcher-web")),
                Err(refusal("gibt-es-nicht")),
                Err(refusal("executor")),
            ]
        );

        // Wurzelsitzung: allein die Sichtbarkeit (plus Reducer).
        let unrestricted = DeclaredTargets::for_caller(&policy, None);
        assert_eq!(unrestricted, DeclaredTargets::Unrestricted);
        let admitted = admit_targets(&request, &visible, &BTreeSet::new(), &unrestricted, &policy);
        assert_eq!(
            admitted[..3],
            [
                Ok("reduce_to_read_explore"),
                Ok("reduce_to_read_registry"),
                Ok("reduce_to_read_network"),
            ]
        );
        assert_eq!(
            admitted[4],
            Err(
                "Ziel executor ist für dich nicht delegierbar; delegierbar sind: explorer, \
                 planner, researcher-web"
                    .to_owned()
            ),
            "ohne Reducer nicht delegierbar"
        );

        // Eine Deklaration, die nichts Sichtbares übrig ließe, fällt auf die
        // sichtbare Menge zurück (sie ist nur ein Filter).
        let nothing = DeclaredTargets::Only(set(&["gibt-es-nicht"]));
        assert_eq!(
            delegable_roles(&visible, &nothing, &policy),
            vec!["explorer", "planner", "researcher-web"]
        );
        // Ohne sichtbare Ziele (ein Worker) bleibt es leer.
        assert!(delegable_roles(&BTreeSet::new(), &nothing, &policy).is_empty());
        Ok(())
    }

    /// Plan R9, E1: ein allein vom Plan-Modus zurückgehaltenes Ziel wird mit
    /// der Plan-Modus-Meldung abgelehnt, die nur lesende Ziele nennt.
    #[test]
    fn test_admission_refuses_withheld_targets_with_the_plan_mode_message() -> TestResult {
        let request = parse(json!({ "targets": [
            { "role": "explorer", "task": "a" },
            { "role": "executor", "task": "b" },
        ] }))?;
        let admitted = admit_targets(
            &request,
            &set(&["explorer", "planner"]),
            &set(&["executor"]),
            &DeclaredTargets::Unrestricted,
            &policy(),
        );
        assert_eq!(
            admitted,
            vec![
                Ok("reduce_to_read_explore"),
                Err(
                    "Plan-Modus: nur lesende Ziele delegierbar (explorer, planner); \
                     Schreib-/Ausführungsziele erst nach Planfreigabe."
                        .to_owned()
                ),
            ]
        );
        Ok(())
    }

    /// Plan R9, Teil C: benutzerdefinierte Agenten. Eine eingebaute
    /// Deklaration (nur eingebaute Rollen) verengt sie nicht; die eigene
    /// Liste eines benutzerdefinierten Orchestrators (nennt benutzerdefinierte
    /// Agenten) ist vollständig — `evidence-critic` ist aus
    /// `intel-analysis-orchestrator` delegierbar, `explorer` nicht.
    #[test]
    fn test_custom_targets_pass_builtin_declarations_and_custom_lists_are_complete() -> TestResult {
        let custom = [
            "evidence-critic",
            "synthesis-writer",
            "intel-analysis-orchestrator",
        ];
        let policy = DelegateWavePolicy::new(
            |role| match role {
                "explorer" | "evidence-critic" => Some("reduce_to_read_explore"),
                "synthesis-writer" => Some("reduce_to_read_only"),
                _ => None,
            },
            |caller| match caller {
                "root-orchestrator" => Some(vec!["explorer".to_owned()]),
                "intel-analysis-orchestrator" => Some(vec![
                    "evidence-critic".to_owned(),
                    "synthesis-writer".to_owned(),
                ]),
                _ => None,
            },
        )
        .with_custom_targets(move |role| custom.contains(&role));
        let visible = set(&["evidence-critic", "explorer", "synthesis-writer"]);

        let root = DeclaredTargets::for_caller(&policy, Some("root-orchestrator"));
        assert_eq!(
            delegable_roles(&visible, &root, &policy),
            vec!["evidence-critic", "explorer", "synthesis-writer"]
        );

        let intel = DeclaredTargets::for_caller(&policy, Some("intel-analysis-orchestrator"));
        let request = parse(json!({ "targets": [
            { "role": "evidence-critic", "task": "prüfe die Belege" },
            { "role": "explorer", "task": "x" },
        ] }))?;
        assert_eq!(
            admit_targets(&request, &visible, &BTreeSet::new(), &intel, &policy),
            vec![
                Ok("reduce_to_read_explore"),
                Err(
                    "Ziel explorer ist für dich nicht delegierbar; delegierbar sind: \
                     evidence-critic, synthesis-writer"
                        .to_owned()
                ),
            ]
        );
        Ok(())
    }

    /// Plan R9, Teil C: `user_approved` je Ziel führt den Auftrag an (wie
    /// bei `transfer_to_*`); unbekannte Werte fallen weg.
    #[test]
    fn test_parse_accepts_user_approved_per_target() -> TestResult {
        let request = parse(json!({ "targets": [
            { "role": "explorer", "task": "Spiele", "user_approved": ["scenario"] },
            { "role": "explorer", "task": "Plane", "user_approved": ["alles"] },
        ] }))?;
        assert_eq!(
            request.targets[0].task,
            "Die Nutzerin hat vorab freigegeben: scenario.\n\nSpiele"
        );
        assert_eq!(request.targets[1].task, "Plane");
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
    fn test_wave_labels_round_trip_and_unknown_labels_are_none() {
        for join in WaveJoin::ALL {
            assert_eq!(WaveJoin::parse(join.as_str()), Some(*join));
        }
        assert_eq!(WaveJoin::parse("some"), None);
        assert_eq!(WaveJoin::parse(""), None);
        assert_eq!(WaveComplexity::Simple.as_str(), "simple");
        assert_eq!(WaveComplexity::Complex.to_string(), "complex");
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

    /// Plan R9, Teil C: ein abgelehntes Ziel trägt seinen benannten Grund.
    #[test]
    fn test_report_json_names_why_a_target_was_unavailable() {
        let rendered = report(
            WaveJoin::Collect,
            vec![
                TargetStatus::Completed(json!("Antwort")),
                TargetStatus::Unavailable(
                    "Ziel x ist für dich nicht delegierbar; delegierbar sind: explorer".to_owned(),
                ),
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
            json!("Ziel x ist für dich nicht delegierbar; delegierbar sind: explorer")
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

    /// Eine Sitzung, die der Spawner nicht kennt, hat keinen Spawn-Kontext —
    /// der Aufruf scheitert als Ganzes mit dem benannten Grund, ohne eine
    /// einzige Rolle zu nennen (Plan R9, Teil C).
    #[tokio::test]
    async fn test_delegate_wave_without_spawn_context_names_the_reason() -> TestResult {
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
                assert!(
                    message.contains("Kein Spawn-Kontext (interner Fehler)"),
                    "{message}"
                );
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
