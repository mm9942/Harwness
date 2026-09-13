//! Managed admission for orchestrator-created child sessions.
//!
//! The generic extension trait can request a child, but only this controller
//! resolves a configured role into a session. It validates parent ownership,
//! preserves the parent sandbox, and accounts for active children before any
//! child is visible to a model runner.
//!
//! # Fan-out und Budget-Durchsetzung (W2-16..19)
//! Über die reine Admission hinaus fährt dieses Modul die admittierten Kinder
//! auch aus:
//! - [`ManagedAgentSpawner::run_child_with_budget`] setzt einen
//!   [`AgentBudget`] durch (Wanduhrzeit über kooperativen Abbruch,
//!   Tool-Aufrufe nach Turn-Ende).
//! - [`ManagedAgentSpawner::run_children`] führt eine ganze Fan-out-Welle
//!   nebenläufig aus, gedeckelt durch `max_parallel` und geklammert durch
//!   [`JoinSemantics`].
//! - [`ChildRegistryFactory::executable_agent_ir`] speist die Agent-DSL-IR
//!   ein: Tool-Aktivierung, Budget, verschärfte Tiefengrenze und die
//!   Pause-Sperre (`allow_pause`) stammen dann aus der Definition.

use crate::ModelProvider;
use crate::activation::SessionActivation;
use crate::session::SpawnContext;
use crate::session_manager::SessionManager;
use crate::state_store::StateStore;
use crate::turn_loop::{TurnInput, TurnOutcome, run_turn, run_turn_durable};
use harw_agent_dsl::executable::{BudgetSpec, ContextProgram, ExecutableAgentIr, SectionDetail};
use harw_catalog::{AgentSuggestions, SpawnCapabilitySnapshot};
use harw_context::{ContextBudgetSpec, ContextCeiling, SectionName, TrustClass};
use harw_extension_api::{
    AgentSpawnError, AgentSpawner, ExtensionRegistry, SpawnFuture, SpawnInput,
};
use harw_observe::TraceContext;
use harw_protocol::items::{ContentPart, TurnItem};
use harw_sandbox::SandboxSpec;
use harw_session_store::{ApprovalStore, ChildLeaseRecord, ChildLeaseStore};
use harw_types::{AgentRole, ReasoningEffort, SessionId, ToolCallId};
use jiff::{SignedDuration, Timestamp};
use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;
use tokio::sync::watch;
use uuid::Uuid;

/// Meldungstext für ein Geschwisterkind, das von [`JoinSemantics::AnyTerminal`]
/// abgebrochen wurde, weil ein anderes Kind zuerst fertig war.
const CANCELLED_BY_SIBLING: &str = "cancelled: sibling completed first";

/// Das Effort-Level, auf das ein Kind geklammert wird, wenn der Elternteil
/// selbst keines gesetzt hat.
///
/// # Beschreibung
/// F-017/E3b. Ein fehlendes Eltern-Level (`None`) heißt **nicht** „unbegrenzt":
/// ohne diesen Deckel hob ein `None` beim Elternteil auch den `effort_cap` der
/// Agent-IR auf, und das Kind lief mit dem Provider-Default — genau der
/// Befund E3(b) aus `w3-core-child-jobs-mcp.md`. Statt eines Provider-Defaults
/// klammert [`ManagedAgentSpawner::clamp_child_reasoning_effort`] dann auf
/// diesen Wert.
///
/// Belegt: `harw_types::ReasoningEffort` hat **kein** `Default`-Impl
/// (`harw-types/src/reasoning.rs:17-26`), und der Modellkatalog führt keinen
/// Effort-Default (`harw-model-catalog` kennt nur `ReasoningMode::Effort` als
/// Fähigkeitsmerkmal, `harw-model-catalog/src/descriptor.rs:137-147`). Der
/// Wert ist deshalb hier festgelegt: `Medium`, die Mitte der monotonen
/// Ordnung `Minimal < Low < Medium < High < Xhigh < Max`.
const DEFAULT_CHILD_REASONING_EFFORT: ReasoningEffort = ReasoningEffort::Medium;

/// Ein bereits geboxtes Kind-Future im Fan-out-Scheduler.
///
/// `Send` bleibt bewusst gefordert: sonst wäre das Future von
/// [`ManagedAgentSpawner::run_children`] selbst nicht mehr `Send` und ließe
/// sich nicht mit `tokio::spawn` in einen Task legen.
type ChildRunFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ChildRunResult, AgentSpawnError>> + Send + 'a>>;

async fn wait_for_child_cancellation(receiver: &mut watch::Receiver<bool>) {
    while !*receiver.borrow_and_update() {
        if receiver.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// Erzeugt eine frische, zufällige `span_id` (16 Hexzeichen, Kleinschreibung)
/// für ein neu admittiertes Kind.
///
/// # Beschreibung
/// AW1-01b. Jedes Kind bekommt eine **eigene** Spanne, auch wenn es dieselbe
/// `trace_id` wie sein Elternteil trägt (siehe
/// [`crate::session::SpawnContext::trace`]): die gemeinsame Arbeit zeigt sich
/// in der geteilten `trace_id`, nicht in einer geteilten `span_id`. Nutzt
/// `uuid::Uuid::new_v4` als bereits im Baum vorhandene Zufallsquelle
/// (`harw-types` bindet dieselbe Crate identisch für seine ID-Newtypes) und
/// schneidet die ersten 16 der 32 Hexzeichen ab — mehr als ausreichend
/// kollisionsarm für eine Spannen-ID.
///
/// # Returns
/// Ein `String` aus genau 16 Kleinbuchstaben-Hexzeichen, gültig als
/// [`TraceContext::span_id`] bzw. [`TraceContext::with_parent`]-Argument.
fn new_span_id() -> String {
    let simple = Uuid::new_v4().simple().to_string();
    simple[..16].to_owned()
}

/// Leitet den vererbten Trace eines Kindes aus dem Trace seines Elternteils ab.
///
/// # Beschreibung
/// AW1-01b — die einzige Erzeugungsregel für [`crate::session::SpawnContext::trace`]
/// bei der Kind-Admission: dieselbe `trace_id` wie der Parent, eine frische
/// [`new_span_id`] für das Kind, die `span_id` des Parents als
/// `parent_span_id`. Hat der Parent keinen Trace, hat das Kind auch keinen —
/// diese Funktion erfindet nie einen Wurzel-Trace.
///
/// # Arguments
/// - `parent_trace` (`Option<&TraceContext>`): der Trace des admittierenden
///   Elternteils, falls vorhanden.
///
/// # Returns
/// `Ok(Some(TraceContext))` mit dem geerbten Trace des Kindes, wenn der
/// Elternteil einen trug; `Ok(None)`, wenn nicht.
///
/// # Errors
/// [`AgentSpawnError`], wenn `trace_id`/`span_id` des Elternteils kein
/// gültiges Hexformat mehr tragen. Da `TraceContext`s reguläre Felder `pub`
/// sind (siehe `harw-observe`), ist das nicht durch die validierenden
/// Konstruktoren allein ausgeschlossen — die Admission lehnt einen solchen
/// Fall fail-closed ab, statt einen kaputten Trace weiterzureichen.
///
/// # Concurrency
/// Reine Funktion ohne geteilten Zustand.
fn inherit_trace(
    parent_trace: Option<&TraceContext>,
) -> Result<Option<TraceContext>, AgentSpawnError> {
    let Some(parent_trace) = parent_trace else {
        return Ok(None);
    };
    let child_trace = TraceContext::new(parent_trace.trace_id.clone(), new_span_id())
        .and_then(|context| context.with_parent(parent_trace.span_id.clone()))
        .map_err(|error| {
            ManagedAgentSpawner::reject(format!("could not derive child trace context: {error}"))
        })?;
    Ok(Some(child_trace))
}

/// Limits evaluated before a child session is allocated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildLimits {
    /// Maximum edge depth below a root session. A direct child has depth one.
    pub max_depth: u32,
    /// Maximum currently admitted children for one parent session.
    pub max_active_children_per_parent: usize,
    /// Maximum admitted lifetime before a child is considered zombie/stalled.
    pub lease_seconds: i64,
}

impl ChildLimits {
    #[must_use]
    pub fn conservative() -> Self {
        Self {
            max_depth: 4,
            max_active_children_per_parent: 8,
            lease_seconds: 15 * 60,
        }
    }

    /// Deckelt die gleichzeitigen Kinder je Parent auf einen extern
    /// bestimmten Fan-out-Wert.
    ///
    /// # Beschreibung
    /// W2-16. Die Ableitung ist **monoton reduzierend**: der Wert wird gegen
    /// [`Self::conservative`] geklammert (`min`), kann die konservative Grenze
    /// also nur senken, nie anheben. `max_depth` und `lease_seconds` bleiben
    /// unverändert — eine externe Fan-out-Angabe darf die Tiefenbegrenzung und
    /// die Lease-Dauer nicht berühren.
    ///
    /// Dies ist die crate-lokale Form der Profil-Ableitung: `harw-core` hängt
    /// bewusst **nicht** von `harw-model-catalog` ab, deshalb nimmt diese
    /// Funktion die nackte Zahl statt eines `ModelRuntimeProfile`. Ein
    /// Consumer-Crate, das beide Crates kennt, ruft sie mit
    /// `profile.max_child_fanout as usize` auf.
    ///
    /// # Argumente
    /// - `max_children` (`usize`): externer Fan-out-Deckel. `0` wird auf `1`
    ///   angehoben — ein Parent, der gar kein Kind starten dürfte, wäre keine
    ///   Grenze, sondern ein Totalausfall der Delegation.
    ///
    /// # Returns
    /// Eine [`ChildLimits`]-Instanz mit gedeckeltem
    /// `max_active_children_per_parent`.
    ///
    /// # Concurrency
    /// Reine Funktion ohne geteilten Zustand; aus jedem Thread aufrufbar.
    ///
    /// # Examples
    /// ```rust
    /// use harw_core::ChildLimits;
    ///
    /// let limits = ChildLimits::with_max_children(3);
    /// assert_eq!(limits.max_active_children_per_parent, 3);
    /// // Anheben ist unmöglich: die konservative Grenze bleibt die Obergrenze.
    /// assert_eq!(ChildLimits::with_max_children(64).max_active_children_per_parent, 8);
    /// // Ein Deckel von 0 wäre keine Grenze, sondern ein Ausfall.
    /// assert_eq!(ChildLimits::with_max_children(0).max_active_children_per_parent, 1);
    /// ```
    #[must_use]
    pub fn with_max_children(max_children: usize) -> Self {
        let conservative = Self::conservative();
        let capped = conservative
            .max_active_children_per_parent
            .min(max_children);
        Self {
            max_active_children_per_parent: if capped == 0 { 1 } else { capped },
            ..conservative
        }
    }
}

impl Default for ChildLimits {
    fn default() -> Self {
        Self::conservative()
    }
}

/// Budget-Vorgaben für einen einzelnen Child-Spawn.
///
/// Reiner Datentyp ohne eigenes Enforcement — Wave 6 (`AgentToolAdapter`)
/// wendet ihn als Clamp vor dem Spawn an, Wave 8 füllt `reasoning_effort` mit
/// monotoner Vererbung (Kind ≤ Parent, außer explizitem Owner-Override).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AgentBudget {
    /// Obergrenze für die Token-Nutzung des Kindes (`None` = kein Limit).
    pub max_tokens: Option<u64>,
    /// Obergrenze für die Anzahl Tool-Aufrufe des Kindes.
    pub max_tool_calls: Option<u32>,
    /// Obergrenze für die Laufzeit des Kindes in Millisekunden.
    pub max_wall_time_ms: Option<u64>,
    /// Reasoning-Effort-Deckel für das Kind (`None` = vom Provider-Default).
    pub reasoning_effort: Option<ReasoningEffort>,
}

/// Dimension einer verletzten Budgetgrenze.
///
/// # Beschreibung
/// W2-17. Der Variantenname ist der maschinenlesbare Teil der Fehlermeldung
/// `budget_exceeded: <dimension> (limit=<n>, used=<m>)`, die der Controller
/// mangels eigener Fehlervariante über [`AgentSpawnError`] transportiert. Ein
/// Aufrufer, der auf die Dimension reagieren will, vergleicht gegen
/// [`Self::as_str`] statt den Fließtext zu parsen.
///
/// # Concurrency
/// `Copy`; enthält keinen geteilten Zustand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetDimension {
    /// Modell-Token über die gesamte Kindsitzung.
    Tokens,
    /// Werkzeugaufrufe über die gesamte Kindsitzung.
    ToolCalls,
    /// Wanduhrzeit eines einzelnen Kind-Turns.
    WallTime,
}

impl BudgetDimension {
    /// Liefert das stabile, maschinenlesbare Label dieser Dimension.
    ///
    /// # Returns
    /// `"tokens"`, `"tool_calls"` oder `"wall_time"`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tokens => "tokens",
            Self::ToolCalls => "tool_calls",
            Self::WallTime => "wall_time",
        }
    }
}

/// Join-Semantik einer Fan-out-Welle (entspricht `CellBarrier` der Agent-DSL).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinSemantics {
    /// Auf alle Kinder warten (Standard für Research-Wellen).
    AllTerminal,
    /// Beim ersten Ergebnis die übrigen abbrechen.
    AnyTerminal,
    /// Alle Ergebnisse sammeln, auch fehlgeschlagene; der Orchestrator joint.
    Collect,
}

/// Eine Fan-out-Anforderung: bereits admittiertes Kind plus sein Turn-Input.
///
/// # Beschreibung
/// W2-18. Die Anforderung führt **kein** Admission-Argument mit: das Kind muss
/// vor dem Fan-out über [`AgentSpawner::spawn_child`] admittiert worden
/// sein. `budget` ist der pro Kind gültige Deckel; er kann aus
/// [`ManagedAgentSpawner::child_budget`] stammen (dann kommt er aus der
/// Agent-IR) oder vom Orchestrator gesetzt werden.
///
/// # Concurrency
/// `Send`; wird beim Start des Kindes in dessen Future verschoben.
#[derive(Debug, Clone)]
pub struct FanoutRequest {
    /// Das bereits admittierte Kind.
    pub child: SessionId,
    /// Der Turn-Input, mit dem das Kind gestartet wird.
    pub input: TurnInput,
    /// Der für dieses Kind durchzusetzende Budget-Deckel.
    pub budget: AgentBudget,
}

/// Immutable child record retained until the orchestration runtime explicitly
/// closes the child after delivering its result to the parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildRecord {
    pub child: SessionId,
    pub parent: SessionId,
    pub handoff_call_id: ToolCallId,
    pub role: String,
    pub depth: u32,
    pub admitted_at: Timestamp,
    pub lease_expires_at: Timestamp,
    /// Der bei der Admission festgelegte Budget-Deckel. Ohne Agent-IR ist das
    /// [`AgentBudget::default`] (kein Deckel in irgendeiner Dimension).
    pub budget: AgentBudget,
    /// Ob dieses Kind laut `[lifecycle]` seiner Agent-IR pausieren darf.
    /// Default `false` (fail-closed): ohne ausdrückliche Erlaubnis wird ein
    /// pausierender Turn als Vertragsbruch abgewiesen, statt unbegrenzt auf
    /// eine Freigabe zu warten, die das Kind nie erreichen kann.
    pub allow_pause: bool,
    /// Die absolute Tiefendecke, die dieses Kind an seine eigenen Nachkommen
    /// weitergibt: die größte Tiefe, auf der unterhalb dieses Kindes noch eine
    /// Sitzung sitzen darf.
    ///
    /// Der Wert entsteht bei der Admission aus der geerbten Decke des
    /// Elternteils, geschnitten mit `depth + spawn.max_depth` der eigenen
    /// Agent-IR — er kann die geerbte Decke also nur senken, nie anheben, und
    /// liegt damit stets unter [`ChildLimits::max_depth`]. Vererbt wie
    /// [`Self::trace`] und [`crate::session::SpawnContext::ceiling`].
    pub depth_ceiling: u32,
    /// Der bei der Admission geerbte Trace-Kontext (AW1-01b), siehe
    /// [`crate::session::SpawnContext::trace`] und [`inherit_trace`].
    /// `None`, wenn der Elternteil selbst keinen Trace trug. Fließt
    /// unverändert in [`Self::durable_lease`] ein — das ist der Konsument,
    /// der diesen Trace auf Platte schreibt.
    pub trace: Option<TraceContext>,
}

impl ChildRecord {
    fn durable_lease(&self) -> ChildLeaseRecord {
        ChildLeaseRecord {
            child: self.child.clone(),
            parent: self.parent.clone(),
            handoff_call_id: self.handoff_call_id.clone(),
            role: self.role.clone(),
            depth: self.depth,
            admitted_at: self.admitted_at,
            lease_expires_at: self.lease_expires_at,
            // AW1-01b: der bei der Admission geerbte Trace (siehe
            // `inherit_trace`) wird unverändert durchgereicht — das schließt
            // die Kette SpawnContext.trace -> ChildRecord.trace ->
            // ChildLeaseRecord.trace bis auf Platte.
            trace: self.trace.clone(),
        }
    }
}

/// A child removed from the active admission set after its lease elapsed. The
/// caller must feed an error result into `resume_after_child` using the exact
/// parent/call correlation carried here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpiredChild {
    pub child: SessionId,
    pub parent: SessionId,
    pub handoff_call_id: ToolCallId,
    pub role: String,
    pub expired_at: Timestamp,
}

/// Result returned after one child has been driven without holding the shared
/// session-manager lock. A waiting outcome remains managed and can be resumed
/// through the normal core APIs.
#[derive(Debug)]
pub struct ChildRunResult {
    pub child: SessionId,
    pub outcome: TurnOutcome,
}

/// Supplies a fresh, role-specific extension registry for an admitted child.
/// It is intentionally fallible: a role must not start with a partial plugin,
/// skill, or MCP activation.
pub trait ChildRegistryFactory: Send + Sync {
    fn build_registry(
        &self,
        role: &str,
        input: &SpawnInput,
        suggestions: Option<&AgentSuggestions>,
    ) -> Result<ExtensionRegistry, AgentSpawnError>;

    /// Resolves a target-role capability contract before the child exists.
    /// Implementations must use trusted runtime/catalog state rather than raw
    /// model handoff JSON. The default grants no activated capability.
    fn capability_snapshot(
        &self,
        _role: &str,
        _input: &SpawnInput,
    ) -> Result<Option<SpawnCapabilitySnapshot>, AgentSpawnError> {
        Ok(None)
    }

    /// Builds the actual child registry using an immutable activation
    /// contract. The compatibility default preserves existing factories, but
    /// never turns advisory suggestions into registered tools by itself.
    fn build_registry_with_capabilities(
        &self,
        role: &str,
        input: &SpawnInput,
        capability_snapshot: Option<&SpawnCapabilitySnapshot>,
    ) -> Result<ExtensionRegistry, AgentSpawnError> {
        self.build_registry(
            role,
            input,
            capability_snapshot.map(|snapshot| &snapshot.suggestions),
        )
    }

    /// Returns the model provider selected for an admitted child role. The
    /// factory owns provider routing; the controller only owns lifecycle and
    /// concurrency boundaries.
    fn model_for(&self, role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError>;

    /// Die aufgelöste Agent-IR dieser Rolle, falls vorhanden. Der Controller
    /// wendet daraus Tool-Aktivierung, Budget und Pause-Sperre an.
    ///
    /// # Beschreibung
    /// W2-19. Liefert die Factory eine IR, dann zieht [`ManagedAgentSpawner`]
    /// bei der Admission vier Dinge daraus:
    /// 1. `tool_surface` → `SessionActivation` des Kindes (deny-by-default),
    /// 2. `spawn.budget` → [`AgentBudget`] im [`ChildRecord`],
    /// 3. `spawn.max_depth` → **zusätzliche**, nur verschärfende Tiefengrenze,
    /// 4. `lifecycle.allow_pause` → Pause-Sperre im [`ChildRecord`].
    ///
    /// Der Default liefert `None` — bestehende Factories bleiben unverändert
    /// gültig und werden ohne IR-Politik admittiert.
    ///
    /// # Argumente
    /// - `_role` (`&str`): der exakte registrierte Rollenname.
    ///
    /// # Returns
    /// `Some(&ExecutableAgentIr)`, wenn die Factory für diese Rolle eine
    /// gefrorene IR hält; sonst `None`.
    ///
    /// # Concurrency
    /// Muss aus mehreren Threads aufrufbar sein (`Send + Sync`-Supertrait) und
    /// darf keine Sperre über den Rückgabewert hinaus halten.
    fn executable_agent_ir(&self, _role: &str) -> Option<&ExecutableAgentIr> {
        None
    }
}

struct ChildRoleDefinition {
    role: AgentRole,
    /// Target organizational role (§3 DSL spawn matrix) granted to a child
    /// admitted under this definition. Distinct from `role`, which is the
    /// harw-types message/actor role — not the organizational spawn-matrix
    /// axis enforced by `harw_agent_dsl::roles::can_spawn`.
    organizational_role: harw_agent_dsl::roles::AgentRoleId,
    registry_factory: Arc<dyn ChildRegistryFactory>,
}

/// Trusted construction-time metadata for a root session driven outside the
/// [`SessionManager`]. It deliberately has no public constructor: only
/// [`ManagedAgentSpawner::with_external_root_parent`] may install it.
#[derive(Debug, Clone)]
struct ExternalRootParent {
    session_id: SessionId,
    spawn_context: SpawnContext,
    reasoning_effort: Option<ReasoningEffort>,
    /// Die Tool-/Instruktions-/Kontext-Aktivierung der Wurzelsitzung. F-017/E3b:
    /// Kinder dieser Wurzel werden damit geschnitten, weil eine extern
    /// gefahrene Wurzel keine Spiegelsitzung im [`SessionManager`] hat, aus
    /// der sich die Aktivierung sonst lesen ließe.
    activation: SessionActivation,
}

/// The production-shaped [`AgentSpawner`] implementation. It is shared by
/// registries but admits children only into its owned [`SessionManager`].
pub struct ManagedAgentSpawner {
    manager: Arc<Mutex<SessionManager>>,
    limits: ChildLimits,
    roles: BTreeMap<String, ChildRoleDefinition>,
    /// At most one trusted root parent which is intentionally driven outside
    /// the session manager. It is a construction-time bridge for TUI/CLI
    /// roots, not model-controlled spawn input.
    external_root_parent: Option<ExternalRootParent>,
    active: Mutex<BTreeMap<String, ChildRecord>>,
    /// Lease-expired children which may still be unwinding a model/tool future.
    /// Tombstones make late completion fail closed instead of restoring an
    /// apparently healthy, untracked child session after its parent has been
    /// resumed with the expiry failure.
    expired: Mutex<BTreeMap<String, ExpiredChild>>,
    /// Per-child cooperative cancellation channels. Lease reaping signals the
    /// receiver held by `run_child`, which drops the in-flight core future and
    /// prevents further model/tool dispatch for that child turn.
    cancellations: Mutex<BTreeMap<String, watch::Sender<bool>>>,
    /// Optional durable lease ledger. When present, production runtimes use
    /// `reap_expired_durable`/`reconcile_expired_leases` rather than the
    /// compatibility in-memory reaper.
    lease_store: Option<Arc<ChildLeaseStore>>,
}

impl ManagedAgentSpawner {
    #[must_use]
    pub fn new(manager: Arc<Mutex<SessionManager>>, limits: ChildLimits) -> Self {
        Self {
            manager,
            limits,
            roles: BTreeMap::new(),
            external_root_parent: None,
            active: Mutex::new(BTreeMap::new()),
            expired: Mutex::new(BTreeMap::new()),
            cancellations: Mutex::new(BTreeMap::new()),
            lease_store: None,
        }
    }

    /// Registers one exact role name. Replacing an existing definition is
    /// deliberate and happens only while constructing the runtime.
    ///
    /// `organizational_role` is the §3 DSL spawn-matrix role granted to a
    /// child admitted under `name`; `admit` checks it against the calling
    /// parent's own organizational role via `harw_agent_dsl::roles::can_spawn`
    /// before any sandbox/depth/lease check runs.
    #[must_use]
    pub fn with_role(
        mut self,
        name: impl Into<String>,
        role: AgentRole,
        organizational_role: harw_agent_dsl::roles::AgentRoleId,
        registry_factory: Arc<dyn ChildRegistryFactory>,
    ) -> Self {
        self.roles.insert(
            name.into(),
            ChildRoleDefinition {
                role,
                organizational_role,
                registry_factory,
            },
        );
        self
    }

    /// Registers the one trusted root parent that may admit children without
    /// a mirror [`crate::session::AgentSession`] in this spawner's manager.
    ///
    /// This is intentionally a construction-time API rather than part of
    /// [`AgentSpawner`]: model-provided [`SpawnInput`] can only select this
    /// already-registered exact session ID; it cannot create or replace
    /// parent metadata. A manager-owned parent always takes precedence during
    /// admission, so this registration is used only while the manager lacks
    /// `session_id`.
    ///
    /// # Arguments
    /// - `session_id` (`SessionId`): die extern gefahrene Wurzelsitzung.
    /// - `spawn_context` (`SpawnContext`): deren vertrauenswürdige
    ///   Sandbox-/Rollen-/Decken-Metadaten.
    /// - `reasoning_effort` (`Option<ReasoningEffort>`): das Effort-Level der
    ///   Wurzel, von dem Kinder monoton erben.
    /// - `parent_activation` (`SessionActivation`): die Aktivierung der Wurzel.
    ///   F-017/E3b: jedes Kind dieser Wurzel wird bei der Admission damit
    ///   geschnitten ([`SessionActivation::intersect`]), damit eine Rolle
    ///   niemals ein Werkzeug öffnen kann, das die Wurzel selbst nicht hat.
    ///   Aufrufer, die den Schnitt nicht wollen, übergeben
    ///   `SessionActivation::default()` (Profil `Full` — schneidet nichts weg).
    ///
    /// # Errors
    /// Returns [`AgentSpawnError`] when a root is already registered, the
    /// session manager lock is poisoned, or `session_id` is manager-owned at
    /// construction time.
    pub fn with_external_root_parent(
        mut self,
        session_id: SessionId,
        spawn_context: SpawnContext,
        reasoning_effort: Option<ReasoningEffort>,
        parent_activation: SessionActivation,
    ) -> Result<Self, AgentSpawnError> {
        if self.external_root_parent.is_some() {
            return Err(Self::reject(
                "an external root parent is already registered",
            ));
        }
        let manager = self
            .manager
            .lock()
            .map_err(|_| Self::reject("session manager lock is poisoned"))?;
        if manager.get(&session_id).is_ok() {
            return Err(Self::reject(format!(
                "external root parent {session_id} is already manager-owned"
            )));
        }
        drop(manager);
        self.external_root_parent = Some(ExternalRootParent {
            session_id,
            spawn_context,
            reasoning_effort,
            activation: parent_activation,
        });
        Ok(self)
    }

    /// Attaches the restart-safe lease ledger before the runtime is exposed.
    #[must_use]
    pub fn with_lease_store(mut self, lease_store: Arc<ChildLeaseStore>) -> Self {
        self.lease_store = Some(lease_store);
        self
    }

    /// Removes an admitted child after the runtime has durably recorded and
    /// delivered its terminal result. Unknown IDs are ignored so recovery can
    /// reconcile an already-closed record idempotently.
    pub fn close_child(&self, child: &SessionId) {
        if let Ok(mut active) = self.active.lock() {
            active.remove(child.as_str());
        }
        if let Ok(mut expired) = self.expired.lock() {
            expired.remove(child.as_str());
        }
        if let Ok(mut cancellations) = self.cancellations.lock() {
            cancellations.remove(child.as_str());
        }
    }

    /// Marks the lease completed before releasing in-memory admission state.
    /// Call this only after the child's terminal result has been durably
    /// delivered to its parent.
    pub fn close_child_durable(
        &self,
        child: &SessionId,
        completed_at: Timestamp,
    ) -> Result<(), AgentSpawnError> {
        if let Some(lease_store) = &self.lease_store {
            lease_store.complete(child, completed_at).map_err(|error| {
                Self::reject(format!("could not complete child lease: {error}"))
            })?;
        }
        self.close_child(child);
        Ok(())
    }

    #[must_use]
    pub fn child_record(&self, child: &SessionId) -> Option<ChildRecord> {
        self.active
            .lock()
            .ok()
            .and_then(|active| active.get(child.as_str()).cloned())
    }

    /// Liefert den bei der Admission festgelegten Budget-Deckel eines Kindes.
    ///
    /// # Beschreibung
    /// W2-19. Der Deckel stammt aus `spawn.budget` der Agent-IR (falls die
    /// Registry-Factory eine liefert) und wird im [`ChildRecord`] mitgeführt.
    /// Damit muss ein Orchestrator vor
    /// [`Self::run_child_with_budget`] keine zweite Quelle konsultieren: er
    /// liest den Deckel hier und reicht ihn unverändert weiter — oder
    /// verschärft ihn.
    ///
    /// # Argumente
    /// - `child` (`&SessionId`): das admittierte Kind.
    ///
    /// # Returns
    /// `Some(AgentBudget)`, solange das Kind admittiert ist; `None`, wenn es
    /// unbekannt, bereits geschlossen oder die Registry-Sperre vergiftet ist.
    /// Ein Kind ohne Agent-IR liefert [`AgentBudget::default`] — also
    /// ausdrücklich `Some(kein Deckel)`, nicht `None`.
    ///
    /// # Concurrency
    /// Nimmt kurz den `active`-Lock und gibt eine Kopie zurück.
    #[must_use]
    pub fn child_budget(&self, child: &SessionId) -> Option<AgentBudget> {
        self.child_record(child).map(|record| record.budget)
    }

    /// Returns the newest text response produced by an admitted child.
    ///
    /// A completed child is restored to this spawner's session manager before
    /// [`Self::run_child`] returns, so callers can retrieve its terminal text
    /// without receiving session or history access.
    ///
    /// # Errors
    /// Returns [`AgentSpawnError`] when the child is not admitted, its restored
    /// session is unavailable, or its history contains no assistant text.
    pub fn child_final_assistant_text(&self, child: &SessionId) -> Result<String, AgentSpawnError> {
        self.child_record(child)
            .ok_or_else(|| Self::reject(format!("child {child} is not admitted")))?;
        let manager = self
            .manager
            .lock()
            .map_err(|_| Self::reject("session manager lock is poisoned"))?;
        let session = manager.get(child).map_err(|error| {
            Self::reject(format!(
                "admitted child session {child} is not available: {error}"
            ))
        })?;
        session
            .history()
            .items()
            .iter()
            .rev()
            .find_map(|item| match item {
                TurnItem::AssistantMessage(message) => Some(
                    message
                        .content
                        .iter()
                        .filter_map(|part| match part {
                            ContentPart::Text { text } => Some(text.as_str()),
                            ContentPart::ImageUrl { .. } => None,
                        })
                        .collect(),
                ),
                _ => None,
            })
            .filter(|text: &String| !text.is_empty())
            .ok_or_else(|| Self::reject(format!("child {child} has no assistant response text")))
    }

    #[must_use]
    pub fn active_children_for(&self, parent: &SessionId) -> usize {
        self.active
            .lock()
            .map(|active| {
                active
                    .values()
                    .filter(|record| &record.parent == parent)
                    .count()
            })
            .unwrap_or(0)
    }

    /// Lists every currently admitted child of `parent`, newest lease first.
    ///
    /// This is a read-only snapshot: it does not reap expired leases or
    /// mutate spawner state. Callers needing an up-to-date view around a
    /// reaping pass should call [`Self::reap_expired`] first.
    #[must_use]
    pub fn list_children_for(&self, parent: &SessionId) -> Vec<ChildRecord> {
        let mut records: Vec<ChildRecord> = self
            .active
            .lock()
            .map(|active| {
                active
                    .values()
                    .filter(|record| &record.parent == parent)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        records.sort_by_key(|record| std::cmp::Reverse(record.admitted_at));
        records
    }

    /// Requests cooperative cancellation of an admitted child's in-flight
    /// turn without reaping its lease or removing its admission record.
    ///
    /// The signal is observed by [`Self::run_child`], which drops the
    /// in-flight core future and returns a cancellation failure to the
    /// waiting parent. This does not itself close or unwind the child;
    /// callers still expect the lease reaper or a subsequent
    /// [`Self::close_child`] to release the admission slot once the parent
    /// has consumed the cancellation result.
    ///
    /// # Returns
    /// `true` when `child` is currently admitted and has a live cancellation
    /// channel to signal; `false` when `child` is unknown or has already
    /// completed, so the caller can report that no running turn was found.
    pub fn request_cancellation(&self, child: &SessionId) -> bool {
        if self.child_record(child).is_none() {
            return false;
        }
        self.cancellations
            .lock()
            .ok()
            .and_then(|cancellations| cancellations.get(child.as_str()).cloned())
            .map(|cancel| {
                cancel.send_replace(true);
                true
            })
            .unwrap_or(false)
    }

    /// Reaps admitted children whose lease has elapsed. It releases their
    /// concurrency slots and marks any currently registered session failed;
    /// the returned records are intentionally explicit so a higher-level
    /// orchestrator can resume the waiting parent with a correlated failure.
    pub fn reap_expired(&self, now: Timestamp) -> Vec<ExpiredChild> {
        let expired = match self.active.lock() {
            Ok(mut active) => {
                let ids = active
                    .iter()
                    .filter(|(_, record)| now >= record.lease_expires_at)
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>();
                ids.into_iter()
                    .filter_map(|id| active.remove(&id))
                    .collect::<Vec<_>>()
            }
            Err(_) => return Vec::new(),
        };
        let expired = expired
            .into_iter()
            .map(|record| ExpiredChild {
                child: record.child,
                parent: record.parent,
                handoff_call_id: record.handoff_call_id,
                role: record.role,
                expired_at: record.lease_expires_at,
            })
            .collect::<Vec<_>>();
        self.mark_expired(&expired);
        expired
    }

    /// Claims expired leases from durable storage, making the exact parent
    /// handoff correlation available after a process restart. Active local
    /// children are cancelled and terminalized just like the in-memory path.
    pub fn reap_expired_durable(
        &self,
        now: Timestamp,
    ) -> Result<Vec<ExpiredChild>, AgentSpawnError> {
        let Some(lease_store) = &self.lease_store else {
            return Ok(self.reap_expired(now));
        };
        let records = lease_store.claim_expired(now).map_err(|error| {
            Self::reject(format!("could not claim expired child leases: {error}"))
        })?;
        let expired = records
            .into_iter()
            .map(|record| ExpiredChild {
                child: record.child,
                parent: record.parent,
                handoff_call_id: record.handoff_call_id,
                role: record.role,
                expired_at: record.lease_expires_at,
            })
            .collect::<Vec<_>>();
        if let Ok(mut active) = self.active.lock() {
            for record in &expired {
                active.remove(record.child.as_str());
            }
        }
        self.mark_expired(&expired);
        Ok(expired)
    }

    /// Startup reconciliation entrypoint. It intentionally requires no live
    /// child session: callers can feed each returned correlated failure into a
    /// rehydrated parent through `resume_after_child_durable` exactly once.
    pub fn reconcile_expired_leases(
        &self,
        now: Timestamp,
    ) -> Result<Vec<ExpiredChild>, AgentSpawnError> {
        self.reap_expired_durable(now)
    }

    fn mark_expired(&self, expired: &[ExpiredChild]) {
        if let Ok(mut tombstones) = self.expired.lock() {
            for record in expired {
                tombstones.insert(record.child.as_str().to_owned(), record.clone());
            }
        }
        if let Ok(cancellations) = self.cancellations.lock() {
            for record in expired {
                if let Some(cancel) = cancellations.get(record.child.as_str()) {
                    cancel.send_replace(true);
                }
            }
        }
        if let Ok(mut manager) = self.manager.lock() {
            for record in expired {
                if let Ok(session) = manager.get_mut(&record.child) {
                    session.fail("child lease expired before reporting a result".to_owned());
                }
            }
        }
    }

    /// Drives one admitted child while the manager lock is released. This is
    /// the essential parallelism boundary: independent callers may await
    /// `run_child` concurrently without serializing model or tool execution
    /// behind the session registry mutex.
    pub async fn run_child(
        &self,
        child: &SessionId,
        store: &dyn StateStore,
        input: TurnInput,
    ) -> Result<ChildRunResult, AgentSpawnError> {
        self.run_child_with_approvals(child, store, None, input)
            .await
    }

    /// Durable counterpart to [`Self::run_child`].
    pub async fn run_child_durable(
        &self,
        child: &SessionId,
        store: &dyn StateStore,
        approvals: &ApprovalStore,
        input: TurnInput,
    ) -> Result<ChildRunResult, AgentSpawnError> {
        self.run_child_with_approvals(child, store, Some(approvals), input)
            .await
    }

    /// Führt einen Kind-Turn unter einem Budget aus.
    ///
    /// # Beschreibung
    /// W2-17. Erweitert [`Self::run_child`] um die Durchsetzung eines
    /// [`AgentBudget`]. Der Ausführungspfad selbst bleibt unverändert — Lease-,
    /// Tombstone- und `restore`-Behandlung laufen in **jedem** Ausgang genau
    /// wie ohne Budget, weil diese Methode den bestehenden Pfad umschließt und
    /// ihn nicht dupliziert.
    ///
    /// ## Wanduhrzeit
    /// `max_wall_time_ms` wickelt das gesamte Kind-Future in
    /// `tokio::time::timeout`. Läuft die Frist ab, geschieht **in dieser
    /// Reihenfolge**:
    /// 1. [`Self::request_cancellation`] — der einzige Abbruchmechanismus des
    ///    Controllers; ein zweiter würde die Session hinter dem Rücken des
    ///    Managers verlieren.
    /// 2. Das Kind-Future wird zu Ende erwartet und sein Ergebnis verworfen.
    ///    Nur sein regulärer Rückweg legt die Session in den `SessionManager`
    ///    zurück — würde das Future hier fallen gelassen, bliebe die Session
    ///    dauerhaft aus dem Manager entfernt.
    ///
    /// ## Tool-Aufrufe
    /// `max_tool_calls` wird **nach** Rückkehr des Turns geprüft, gezählt über
    /// `session.history()` (Anzahl `TurnItem::ToolCall`). Das entspricht der
    /// Semantik der Agent-DSL („über die gesamte Kindsitzung"), weil die
    /// History des Kindes alle bisherigen Turns umfasst.
    ///
    /// ## Tokens
    /// `max_tokens` rechnet über [`crate::session::AgentSession::total_usage`]
    /// ab — den Akkumulator, den `complete_turn` je Turn fortschreibt. Wie bei
    /// den Tool-Aufrufen ist die Abrechnungseinheit die **gesamte Kind-Session**,
    /// nicht der einzelne Turn. Die Prüfung erfolgt nach Rückkehr des Turns:
    /// ein Überschreiten bricht das Ergebnis ab, verhindert aber nicht den
    /// bereits erfolgten Modellaufruf.
    ///
    /// # Argumente
    /// - `child` (`&SessionId`): das admittierte Kind.
    /// - `store` (`&dyn StateStore`): Transkript-Persistenz des Kind-Turns.
    /// - `approvals` (`Option<&ApprovalStore>`): durabler Approval-Ledger;
    ///   `None` wählt den nicht-durablen Pfad.
    /// - `input` (`TurnInput`): der Turn-Input, wird verschoben.
    /// - `budget` (`AgentBudget`): die durchzusetzenden Obergrenzen.
    ///
    /// # Returns
    /// `Ok(ChildRunResult)`, wenn der Turn im Budget blieb.
    ///
    /// # Errors
    /// - [`AgentSpawnError`] mit `"budget_exceeded: wall_time (limit=…, used=…)"`,
    ///   wenn die Wanduhrfrist ablief.
    /// - [`AgentSpawnError`] mit `"budget_exceeded: tool_calls (limit=…, used=…)"`,
    ///   wenn das Kind mehr Werkzeugaufrufe verbraucht hat als erlaubt.
    /// - Jeder Fehler aus dem darunterliegenden Ausführungspfad (nicht
    ///   admittiert, Lease abgelaufen, Cancellation, Turn-Fehler, Pause-Sperre).
    ///
    /// # Concurrency
    /// Hält während des Turns keinen Lock; mehrere Kinder dürfen gleichzeitig
    /// laufen (siehe [`Self::run_children`]).
    pub async fn run_child_with_budget(
        &self,
        child: &SessionId,
        store: &dyn StateStore,
        approvals: Option<&ApprovalStore>,
        input: TurnInput,
        budget: AgentBudget,
    ) -> Result<ChildRunResult, AgentSpawnError> {
        let started = std::time::Instant::now();
        let mut turn = Box::pin(self.run_child_with_approvals(child, store, approvals, input));

        let outcome = match budget.max_wall_time_ms {
            Some(limit_ms) => {
                // Das Ergebnis wird gebunden, damit der `timeout`-Temporary vor
                // den Armen fällt — sonst bliebe `turn` bis zum Ende des
                // `match` ausgeliehen und wäre unten nicht mehr erwartbar.
                let timed =
                    tokio::time::timeout(Duration::from_millis(limit_ms), turn.as_mut()).await;
                match timed {
                    Ok(outcome) => outcome,
                    Err(_elapsed) => {
                        if !self.request_cancellation(child) {
                            tracing::warn!(
                                child = %child,
                                "child_budget.cancel_channel_missing",
                            );
                        }
                        let _discarded = turn.await;
                        let used_ms =
                            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                        tracing::warn!(
                            child = %child,
                            dimension = BudgetDimension::WallTime.as_str(),
                            limit = limit_ms,
                            used = used_ms,
                            "child_budget.exceeded",
                        );
                        return Err(Self::budget_exceeded(
                            BudgetDimension::WallTime,
                            limit_ms,
                            used_ms,
                        ));
                    }
                }
            }
            None => turn.await,
        };
        let outcome = outcome?;

        if let Some(limit) = budget.max_tool_calls {
            let used = self.child_tool_call_count(child)?;
            if used > limit {
                tracing::warn!(
                    child = %child,
                    dimension = BudgetDimension::ToolCalls.as_str(),
                    limit = limit,
                    used = used,
                    "child_budget.exceeded",
                );
                return Err(Self::budget_exceeded(
                    BudgetDimension::ToolCalls,
                    u64::from(limit),
                    u64::from(used),
                ));
            }
        }
        if let Some(limit) = budget.max_tokens {
            let used = self.child_token_usage(child)?;
            if used > limit {
                tracing::warn!(
                    child = %child,
                    dimension = BudgetDimension::Tokens.as_str(),
                    limit = limit,
                    used = used,
                    "child_budget.exceeded",
                );
                return Err(Self::budget_exceeded(BudgetDimension::Tokens, limit, used));
            }
        }
        Ok(outcome)
    }

    /// Führt mehrere bereits admittierte Kinder nebenläufig aus.
    ///
    /// # Beschreibung
    /// W2-18 — der eigentliche Fan-out. `max_parallel` begrenzt die
    /// gleichzeitig laufenden Kinder (`0` wird auf `1` angehoben). Die
    /// Ergebnisse kommen in der **Reihenfolge der Anforderungen** zurück, nicht
    /// in Abschlussreihenfolge — der Aufrufer kann sie so ohne
    /// Korrelationsschritt den Fragen zuordnen.
    ///
    /// Der Scheduler ist absichtlich `futures`-frei: `harw-core` führt diese
    /// Crate nicht. Die Kind-Futures werden geboxt in einem Slot-Vektor
    /// gehalten und über `std::future::poll_fn` gemeinsam gepollt; `JoinSet`
    /// scheidet aus, weil es `'static`-Futures verlangt, die geliehenen
    /// `&self`/`&dyn StateStore` aber genau das nicht sind.
    ///
    /// ## Nebenläufigkeits-Analyse: `remove`/`restore`
    /// [`Self::run_child`] nimmt die Session mit `SessionManager::remove` aus
    /// dem `Mutex<SessionManager>`, führt den Turn **ohne** den Lock aus und
    /// restauriert sie danach. Das ist unter echter Nebenläufigkeit korrekt:
    /// - `remove` liefert `Option<AgentSession>`; der Controller bildet `None`
    ///   auf einen sauberen [`AgentSpawnError`] ab. Kein `expect`, kein Panic.
    ///   Starten zwei Aufrufer dasselbe Kind, gewinnt genau einer; der zweite
    ///   bekommt „child session … is not available", statt den Zustand zu
    ///   verdoppeln.
    /// - `restore` weist eine doppelte Session-ID mit `CoreError::TurnRejected`
    ///   ab, statt sie zu überschreiben. Ein verspäteter `restore` kann eine
    ///   inzwischen neu angelegte Session also nicht überbügeln.
    /// - Der Manager-Lock wird nur für `remove` bzw. `restore` gehalten, beide
    ///   als eigenständige Anweisung. Der `MutexGuard` fällt am Ende seiner
    ///   Anweisung und wird nie über ein `await` gehalten — deshalb bleiben die
    ///   Kind-Futures `Send` und es entsteht keine Lock-Reihenfolge zwischen
    ///   Kindern.
    /// - Verschiedene Kinder berühren disjunkte `BTreeMap`-Einträge; eine
    ///   Reihenfolgeabhängigkeit zwischen ihnen existiert nicht.
    /// - Verschachtelt gehalten werden Locks nur in `admit`
    ///   (`manager` ⊃ `active` ⊃ `cancellations`, immer in dieser Richtung);
    ///   alle übrigen Pfade nehmen ihre Sperren nacheinander. Damit gibt es
    ///   keine Inversion und keinen Deadlock zwischen Fan-out und Admission.
    ///
    /// ## Join-Semantik
    /// - [`JoinSemantics::AnyTerminal`]: Sobald ein Kind `Ok` liefert, wird für
    ///   alle noch laufenden Geschwister [`Self::request_cancellation`]
    ///   gerufen; ihre Ergebnisse — und die noch nicht gestarteten — werden als
    ///   `Err("cancelled: sibling completed first")` markiert. Ein Geschwister,
    ///   das im selben Moment noch erfolgreich fertig wird, wird bewusst
    ///   verworfen: „genau ein Gewinner" ist der Vertrag dieser Semantik.
    /// - [`JoinSemantics::AllTerminal`] und [`JoinSemantics::Collect`] warten
    ///   beide auf alle Kinder und liefern hier dieselbe Form; sie
    ///   unterscheiden sich erst im Join-Schritt des Aufrufers, den diese
    ///   Methode nicht ausführt.
    ///
    /// # Argumente
    /// - `requests` (`Vec<FanoutRequest>`): die Welle; jedes Kind muss bereits
    ///   admittiert sein.
    /// - `store` (`&dyn StateStore`): gemeinsame Transkript-Persistenz.
    /// - `max_parallel` (`usize`): Deckel gleichzeitig laufender Kinder.
    /// - `join` (`JoinSemantics`): Klammerung der Welle.
    ///
    /// # Returns
    /// Ein `Vec` mit genau `requests.len()` Einträgen, positionsgleich zur
    /// Eingabe. Jeder Eintrag ist das Ergebnis von
    /// [`Self::run_child_with_budget`] für das Kind an dieser Position.
    ///
    /// # Panics
    /// Keine. Ein verlorener Scheduler-Slot wird als `Err` gemeldet, nicht
    /// als Panic.
    ///
    /// # Concurrency
    /// Pollt alle laufenden Kind-Futures in einer einzigen Task; es werden
    /// keine Tasks gespawnt. Die Nebenläufigkeit entsteht dadurch, dass die
    /// Kind-Turns den Manager-Lock nicht halten.
    pub async fn run_children<'a>(
        &'a self,
        requests: Vec<FanoutRequest>,
        store: &'a dyn StateStore,
        max_parallel: usize,
        join: JoinSemantics,
    ) -> Vec<Result<ChildRunResult, AgentSpawnError>> {
        let total = requests.len();
        if total == 0 {
            return Vec::new();
        }
        let slots = if max_parallel == 0 { 1 } else { max_parallel };
        let mut results: Vec<Option<Result<ChildRunResult, AgentSpawnError>>> =
            (0..total).map(|_| None).collect();
        let mut queue: VecDeque<(usize, FanoutRequest)> =
            requests.into_iter().enumerate().collect();
        let mut running: Vec<(usize, SessionId, ChildRunFuture<'a>)> = Vec::new();
        let mut winner_decided = false;

        // Bewusst ein Event statt eines betretenen Spans: ein
        // `tracing::span::Entered` ist `!Send` und würde — über die `await`s
        // dieser Schleife gehalten — das gesamte `run_children`-Future
        // `!Send` machen und damit `tokio::spawn` verbieten.
        tracing::info!(children = total, max_parallel = slots, "child_fanout.start",);

        loop {
            while running.len() < slots {
                let Some((position, request)) = queue.pop_front() else {
                    break;
                };
                if winner_decided {
                    results[position] = Some(Err(Self::reject(CANCELLED_BY_SIBLING)));
                    continue;
                }
                let child = request.child.clone();
                let future: ChildRunFuture<'a> = Box::pin(self.run_child_owned(
                    request.child,
                    request.input,
                    request.budget,
                    store,
                ));
                running.push((position, child, future));
            }
            if running.is_empty() {
                break;
            }

            // Gemeinsames Pollen aller laufenden Kinder mit einem Waker. Ein
            // bereits fertiges Kind bricht den Durchlauf ab; die übrigen haben
            // ihren Waker aus einem früheren Durchlauf noch registriert und
            // wecken uns selbst wieder.
            let (index, value) = std::future::poll_fn(|cx| {
                for (index, (_, _, future)) in running.iter_mut().enumerate() {
                    if let Poll::Ready(value) = future.as_mut().poll(cx) {
                        return Poll::Ready((index, value));
                    }
                }
                Poll::Pending
            })
            .await;
            let (position, _child, _future) = running.remove(index);

            let value = if winner_decided {
                Err(Self::reject(CANCELLED_BY_SIBLING))
            } else {
                value
            };
            if !winner_decided && matches!(join, JoinSemantics::AnyTerminal) && value.is_ok() {
                winner_decided = true;
                for (_, sibling, _) in &running {
                    if !self.request_cancellation(sibling) {
                        tracing::warn!(
                            child = %sibling,
                            "child_fanout.cancel_channel_missing",
                        );
                    }
                }
            }
            results[position] = Some(value);
        }

        let results: Vec<Result<ChildRunResult, AgentSpawnError>> = results
            .into_iter()
            .map(|slot| {
                slot.unwrap_or_else(|| {
                    Err(Self::reject("child fan-out scheduler lost a result slot"))
                })
            })
            .collect();
        tracing::info!(
            children = total,
            failed = results.iter().filter(|slot| slot.is_err()).count(),
            "child_fanout.complete",
        );
        results
    }

    /// Klammert das Reasoning-Effort-Level eines admittierten Kindes und gibt das
    /// effektiv gesetzte Level zurück.
    ///
    /// # Beschreibung
    /// Wave 8, korrigiert in F-017/E3b. Die Basis ist der in [`Self::admit`]
    /// monoton geerbte Parent-Wert; fehlt er (`None`), gilt
    /// [`DEFAULT_CHILD_REASONING_EFFORT`] als Basis — ein fehlender Eltern-Wert
    /// ist kein Freibrief.
    /// - `cap` senkt den Wert weiter (`min(base, cap)`) und hebt ihn nie an.
    /// - `owner_override` durchbricht die Monotonie gezielt (Owner-Authority) und
    ///   setzt das Level explizit — auch nach oben. `None` behält die Klammerung.
    ///
    /// # Argumente
    /// - `child` (`&SessionId`): das bereits admittierte Kind.
    /// - `cap` (`Option<ReasoningEffort>`): monotone Obergrenze aus dem Budget.
    /// - `owner_override` (`Option<ReasoningEffort>`): expliziter Owner-Override.
    ///
    /// # Returns
    /// `Ok(Option<ReasoningEffort>)` — das nach der Klammerung gesetzte Level.
    /// Seit F-017/E3b immer `Some(..)`: der `Option`-Typ bleibt nur erhalten,
    /// weil [`crate::session::AgentSession::set_reasoning_effort`] ihn führt.
    ///
    /// # Errors
    /// - [`AgentSpawnError`]: das Kind ist nicht (mehr) im Manager registriert
    ///   oder der Manager-Lock ist vergiftet.
    ///
    /// # Concurrency
    /// Nimmt kurz den `manager`-Lock; darf vor [`Self::run_child`] aufgerufen
    /// werden, solange das Kind noch nicht für die Ausführung entnommen wurde.
    pub fn clamp_child_reasoning_effort(
        &self,
        child: &SessionId,
        cap: Option<ReasoningEffort>,
        owner_override: Option<ReasoningEffort>,
    ) -> Result<Option<ReasoningEffort>, AgentSpawnError> {
        let mut manager = self
            .manager
            .lock()
            .map_err(|_| Self::reject("session manager lock is poisoned"))?;
        let session = manager
            .get_mut(child)
            .map_err(|error| Self::reject(format!("unknown child for effort clamp: {error}")))?;
        // F-017/E3b: Eine fehlende geerbte Basis heißt nicht „unbegrenzt".
        // Vorher hob `(None, _) => None` auch einen vorhandenen `cap` der
        // Agent-IR auf, und weil beide Produktionswurzeln mit
        // `reasoning_effort = None` registrieren, griff der Deckel nie (Befund
        // E3(b)). Ohne Basis gilt deshalb `DEFAULT_CHILD_REASONING_EFFORT`,
        // und der `cap` klammert wie immer nach unten.
        let base = session
            .reasoning_effort()
            .unwrap_or(DEFAULT_CHILD_REASONING_EFFORT);
        let capped = match cap {
            Some(c) => base.min(c),
            None => base,
        };
        let effective = Some(owner_override.unwrap_or(capped));
        session.set_reasoning_effort(effective);
        Ok(effective)
    }

    async fn run_child_with_approvals(
        &self,
        child: &SessionId,
        store: &dyn StateStore,
        approvals: Option<&ApprovalStore>,
        input: TurnInput,
    ) -> Result<ChildRunResult, AgentSpawnError> {
        let record = self
            .child_record(child)
            .ok_or_else(|| Self::reject(format!("child {child} is not admitted")))?;
        let factory = self
            .roles
            .get(&record.role)
            .ok_or_else(|| Self::reject(format!("child role '{}' disappeared", record.role)))?
            .registry_factory
            .clone();
        let model = factory.model_for(&record.role)?;
        let mut cancellation = self
            .cancellations
            .lock()
            .map_err(|_| Self::reject("child cancellation registry lock is poisoned"))?
            .get(child.as_str())
            .cloned()
            .ok_or_else(|| Self::reject(format!("child {child} has no cancellation channel")))?
            .subscribe();
        let mut session = self
            .manager
            .lock()
            .map_err(|_| Self::reject("session manager lock is poisoned"))?
            .remove(child)
            .ok_or_else(|| Self::reject(format!("child session {child} is not available")))?;

        let outcome = tokio::select! {
            () = wait_for_child_cancellation(&mut cancellation) => Err(Self::reject(format!(
                "child {child} was cancelled before its turn completed"
            ))),
            outcome = async {
                match approvals {
                    Some(approvals) => run_turn_durable(&mut session, model.as_ref(), store, approvals, input).await,
                    None => run_turn(&mut session, model.as_ref(), store, input).await,
                }
                .map_err(|error| Self::reject(error.to_string()))
            } => outcome,
        };
        let expired = self
            .expired
            .lock()
            .map_err(|_| Self::reject("expired-child registry lock is poisoned"))?
            .get(child.as_str())
            .cloned();
        if expired.is_some() {
            session.fail("child lease expired while its turn was still running".to_owned());
        }
        let restore = self
            .manager
            .lock()
            .map_err(|_| Self::reject("session manager lock is poisoned"))?
            .restore(session)
            .map_err(|error| Self::reject(error.to_string()));
        restore?;
        if let Some(expired) = expired {
            return Err(Self::reject(format!(
                "child {child} completed after lease expiry at {}; result discarded",
                expired.expired_at
            )));
        }
        let outcome = outcome?;
        // W2-19, fail-closed: ein Kind, dessen Lebenszyklus das Pausieren
        // verbietet, hat keinen Kanal, über den eine Freigabe je eintreffen
        // könnte. Die Session ist zu diesem Zeitpunkt bereits regulär
        // restauriert — nur das Ergebnis wird abgewiesen.
        // Das Label wird vor dem `match` gebunden, damit die Leihgabe an
        // `outcome` endet, bevor der Erfolgsarm es in das Ergebnis verschiebt.
        let pause_label = Self::pause_label(&outcome);
        match pause_label {
            Some(label) if !record.allow_pause => Err(Self::reject(format!(
                "child paused but its lifecycle forbids pausing: {label}"
            ))),
            _ => Ok(ChildRunResult {
                child: child.clone(),
                outcome,
            }),
        }
    }

    /// Besitzende Variante von [`Self::run_child_with_budget`] für den
    /// Fan-out-Scheduler: das Future darf nur `&self` und `store` ausleihen,
    /// nicht die Anforderung, die im Slot-Vektor keinen Platz hätte.
    async fn run_child_owned(
        &self,
        child: SessionId,
        input: TurnInput,
        budget: AgentBudget,
        store: &dyn StateStore,
    ) -> Result<ChildRunResult, AgentSpawnError> {
        self.run_child_with_budget(&child, store, None, input, budget)
            .await
    }

    /// Zählt die Werkzeugaufrufe in der Historie eines Kindes.
    ///
    /// Die Zählung deckt die **gesamte Kindsitzung** ab, nicht nur den letzten
    /// Turn — das ist die Semantik von `[spawn.budget] max_tool_calls`.
    fn child_tool_call_count(&self, child: &SessionId) -> Result<u32, AgentSpawnError> {
        let manager = self
            .manager
            .lock()
            .map_err(|_| Self::reject("session manager lock is poisoned"))?;
        let session = manager.get(child).map_err(|error| {
            Self::reject(format!(
                "cannot account tool calls for child {child}: {error}"
            ))
        })?;
        let used = session
            .history()
            .items()
            .iter()
            .filter(|item| matches!(item, TurnItem::ToolCall(_)))
            .count();
        Ok(u32::try_from(used).unwrap_or(u32::MAX))
    }

    /// Aufsummierte Token-Nutzung eines Kindes über alle seine Turns.
    ///
    /// Quelle ist der Akkumulator [`crate::session::AgentSession::total_usage`];
    /// er wird von `complete_turn` fortgeschrieben. Damit ist `max_tokens`
    /// genauso durchsetzbar wie `max_tool_calls` — beide rechnen über die
    /// gesamte Kind-Session ab, nicht über den einzelnen Turn.
    fn child_token_usage(&self, child: &SessionId) -> Result<u64, AgentSpawnError> {
        let manager = self
            .manager
            .lock()
            .map_err(|_| Self::reject("session manager lock is poisoned"))?;
        let session = manager.get(child).map_err(|error| {
            Self::reject(format!("cannot account tokens for child {child}: {error}"))
        })?;
        Ok(session.total_usage().total())
    }

    /// Baut die maschinenlesbare Budget-Verletzung.
    ///
    /// Bis `CoreError::BudgetExceeded { dimension, limit, used }` in
    /// `harw-core/src/error.rs` existiert, ist [`AgentSpawnError`] der einzige
    /// Transport. Das Format bleibt deshalb strikt:
    /// `budget_exceeded: <dimension> (limit=<n>, used=<m>)`.
    fn budget_exceeded(dimension: BudgetDimension, limit: u64, used: u64) -> AgentSpawnError {
        Self::reject(format!(
            "budget_exceeded: {} (limit={limit}, used={used})",
            dimension.as_str()
        ))
    }

    /// Übersetzt die typisierte DSL-Budgetangabe in den Runtime-Deckel.
    ///
    /// Ein unbekanntes `effort_cap`-Label wird fail-closed abgewiesen: die
    /// Agent-IR führt das Label bewusst undurchsichtig und verlangt vom
    /// Konsumenten Ablehnung statt eines stillen Defaults.
    fn budget_from_spec(spec: &BudgetSpec) -> Result<AgentBudget, AgentSpawnError> {
        let reasoning_effort = match spec.effort_cap() {
            Some(label) => Some(label.parse::<ReasoningEffort>().map_err(|error| {
                Self::reject(format!(
                    "child budget carries an unknown reasoning-effort cap '{label}': {error}"
                ))
            })?),
            None => None,
        };
        Ok(AgentBudget {
            max_tokens: spec.max_tokens(),
            max_tool_calls: spec.max_tool_calls(),
            // Sekunden → Millisekunden; ein Überlauf sättigt statt zu wrappen.
            max_wall_time_ms: spec.max_wall_secs().map(|secs| secs.saturating_mul(1_000)),
            reasoning_effort,
        })
    }

    /// Stabiles Label eines pausierenden Turn-Ausgangs, oder `None`, wenn der
    /// Turn terminal abgeschlossen ist.
    fn pause_label(outcome: &TurnOutcome) -> Option<&'static str> {
        match outcome {
            TurnOutcome::Completed => None,
            TurnOutcome::AwaitingChild { .. } => Some("awaiting_child"),
            TurnOutcome::AwaitingApproval { .. } => Some("awaiting_approval"),
        }
    }

    fn reject(message: impl Into<String>) -> AgentSpawnError {
        AgentSpawnError {
            message: message.into(),
        }
    }

    fn parent_depth(manager: &SessionManager, parent: &SessionId) -> Result<u32, AgentSpawnError> {
        let mut depth = 0_u32;
        let mut cursor = parent.clone();
        loop {
            let session = manager
                .get(&cursor)
                .map_err(|error| Self::reject(format!("unknown child parent: {error}")))?;
            let Some(next) = session.parent_session_id() else {
                return Ok(depth);
            };
            depth = depth.saturating_add(1);
            cursor = next.clone();
        }
    }

    /// Die vollständig geschlossene Decke: keine Sektion, die niedrigste
    /// Vertrauensklasse, kein Budget.
    ///
    /// # Beschreibung
    /// AW2-02. Das fail-closed Ergebnis, wenn ein Elternteil (nur an einer
    /// extern registrierten Wurzel möglich, siehe
    /// [`crate::session::SpawnContext::ceiling`]) selbst keine Decke trägt.
    /// Ein fehlendes Feld darf nie zu mehr Autorität führen als ein explizit
    /// gesetztes — deshalb ist diese Decke die restriktivste denkbare, nicht
    /// eine unbegrenzte.
    fn closed_ceiling() -> ContextCeiling {
        ContextCeiling {
            sections: std::collections::BTreeSet::new(),
            max_trust: TrustClass::Data,
            budget: ContextBudgetSpec {
                total: harw_lens_types::BudgetSpec { total: 0 },
                per_section: BTreeMap::new(),
            },
        }
    }

    /// Benennt den einen Aspekt, in dem `requested` über `parent` hinausgeht.
    ///
    /// # Beschreibung
    /// AW2-02. Wird nur aufgerufen, nachdem [`ContextCeiling::intersect`]
    /// bereits eine Abweichung von `requested` festgestellt hat (siehe
    /// [`Self::cut_ceiling`]) — mindestens einer der drei Zweige unten trifft
    /// deshalb garantiert zu. Prüft in derselben Reihenfolge wie
    /// [`ContextCeiling::admits`]: Sektionen, dann Vertrauensklasse, dann
    /// Budget.
    ///
    /// # Arguments
    /// - `parent` (`&ContextCeiling`): die Decke des Elternteils.
    /// - `requested` (`&ContextCeiling`): die vom Kind geforderte, zu weite
    ///   Decke.
    ///
    /// # Returns
    /// Eine für Menschen lesbare Beschreibung des verletzten Aspekts, zur
    /// Verwendung in der Ablehnungsmeldung von [`Self::cut_ceiling`].
    fn describe_ceiling_escalation(parent: &ContextCeiling, requested: &ContextCeiling) -> String {
        if let Some(section) = requested.sections.difference(&parent.sections).next() {
            return format!(
                "requested section '{section}' is not part of the parent's context ceiling"
            );
        }
        if requested.max_trust.trust_rank() > parent.max_trust.trust_rank() {
            return format!(
                "requested max_trust {:?} exceeds the parent's max_trust {:?}",
                requested.max_trust, parent.max_trust
            );
        }
        format!(
            "requested budget total {} exceeds the parent's effective budget total {}",
            requested.budget.total.total, parent.budget.total.total
        )
    }

    /// Prüft, ob ein von der Agent-IR mitgebrachtes [`ContextProgram`] innerhalb
    /// der (bereits geschnittenen) Kontext-Decke des Kindes liegt.
    ///
    /// # Beschreibung
    /// Ein [`ContextProgram`] wird — anders als [`ContextCeiling`] —
    /// mitgebracht, nicht vom Elternteil geschnitten (§ Moduldoku
    /// `harw-core/src/session.rs`, "Folgeknoten zu AW2-01/AW2-02"): jede Rolle
    /// bringt ihr eigenes Programm aus ihrer Definition mit, unabhängig von
    /// dem, was ihr Elternteil sieht. Trotzdem darf ein mitgebrachtes Programm
    /// die Decke nie *erweitern* — genau wie eine über [`SpawnInput::ceiling`]
    /// angeforderte Decke selbst nie über die des Elternteils hinausgehen darf
    /// ([`Self::cut_ceiling`]).
    ///
    /// Für ein über den `[context]`/`[context_program]`-Konfigurationspfad
    /// gebautes Programm (`harw_agent_dsl::executable::lower`) hat zuvor noch
    /// keine Ceiling-Prüfung stattgefunden: dieser Pfad läuft nicht durch
    /// [`harw_agent_dsl::context_program::ContextCeilingAdmission::admits_program`]
    /// — jene Prüfung deckt nur den `harwness.context.<name>@<v>`-
    /// Deklarationspfad ab (§ dessen Doku,
    /// [`ContextProgram::from_resolved_program`]). Diese Funktion schließt
    /// genau diese Lücke an der einzigen Stelle, die beide Seiten zur
    /// Laufzeit kennt: die dynamisch je Elternteil geschnittene Decke und das
    /// von der Rolle mitgebrachte Programm.
    ///
    /// Geprüft werden alle Sektionsnamen aus [`ContextProgram::must_include`]
    /// und [`ContextProgram::section_detail`] (Vereinigung, da
    /// `section_detail` auch `Normal`-starke Sektionen trägt, die in
    /// `must_include` nicht auftauchen) gegen `ceiling.sections`. Die
    /// [`harw_context::TrustClass`] je Sektion trägt dieses `ContextProgram`
    /// nicht (§ `ContextProgram::from_resolved_program`-Doku, "Limitation")
    /// und kann daher hier nicht geprüft werden.
    ///
    /// # Arguments
    /// - `ceiling` (`&ContextCeiling`): die bereits geschnittene Decke des
    ///   Kindes.
    /// - `program` (`&ContextProgram`): das von der Agent-IR der Rolle
    ///   mitgebrachte Programm.
    ///
    /// # Returns
    /// `Some(<Grund>)` beim ersten verletzten Sektionsnamen; `None`, wenn jede
    /// genannte Sektion innerhalb `ceiling.sections` liegt.
    fn describe_context_program_ceiling_violation(
        ceiling: &ContextCeiling,
        program: &ContextProgram,
    ) -> Option<String> {
        program
            .must_include()
            .iter()
            .map(String::as_str)
            .chain(program.section_detail().iter().map(SectionDetail::name))
            .find(|name| {
                !SectionName::try_new(*name).is_ok_and(|section| ceiling.sections.contains(&section))
            })
            .map(|name| {
                format!("declared section '{name}' is not part of the child's context ceiling")
            })
    }

    /// Schneidet die Kontext-Decke eines Kindes — **im selben Schritt** wie
    /// [`admit`](Self::admit) die Berechtigungen schneidet, siehe dort.
    ///
    /// # Beschreibung
    /// AW2-02. `parent_ceiling` ist `None` nur an einer extern registrierten
    /// Wurzel (siehe [`crate::session::SpawnContext::ceiling`]) und wird dann
    /// als [`Self::closed_ceiling`] gelesen — fail-closed, nicht unbegrenzt.
    /// Fordert das Kind keine eigene Decke (`requested` ist `None`), erbt es
    /// die Eltern-Decke unverändert. Fordert es eine eigene Decke, muss diese
    /// bereits vollständig innerhalb der Eltern-Decke liegen: liegt sie das
    /// nicht, weicht [`ContextCeiling::intersect`] von `requested` ab, und
    /// genau das ist das Signal zum Abweisen — nie zum stillschweigenden
    /// Beschneiden. Ein Kind, dessen überzogene Forderung stillschweigend
    /// beschnitten würde, liefe mit weniger Kontext als seine Deklaration
    /// verspricht, ohne dass es irgendwer erführe — derselbe Fehler wie ein
    /// stillschweigend weggelassenes `must_include`.
    ///
    /// # Arguments
    /// - `parent_ceiling` (`Option<&ContextCeiling>`): die bereits
    ///   geschnittene Decke des admittierenden Elternteils.
    /// - `requested` (`Option<&ContextCeiling>`): die vom Kind über
    ///   [`SpawnInput::ceiling`] geforderte Decke, falls vorhanden.
    ///
    /// # Returns
    /// Die für das Kind geltende, bereits geschnittene [`ContextCeiling`].
    ///
    /// # Errors
    /// [`AgentSpawnError`], wenn `requested` eine Sektion, eine
    /// Vertrauensklasse oder ein Budget verlangt, das über `parent_ceiling`
    /// hinausgeht. Die Meldung nennt den verletzten Aspekt
    /// ([`Self::describe_ceiling_escalation`]).
    fn cut_ceiling(
        parent_ceiling: Option<&ContextCeiling>,
        requested: Option<&ContextCeiling>,
    ) -> Result<ContextCeiling, AgentSpawnError> {
        let parent_ceiling = parent_ceiling.cloned().unwrap_or_else(Self::closed_ceiling);
        let Some(requested) = requested else {
            return Ok(parent_ceiling);
        };
        let cut = parent_ceiling.intersect(requested);
        if &cut != requested {
            return Err(Self::reject(format!(
                "child context ceiling escalation rejected: {}",
                Self::describe_ceiling_escalation(&parent_ceiling, requested)
            )));
        }
        Ok(cut)
    }

    fn admit(
        &self,
        role_name: &str,
        input: SpawnInput,
        sandbox: SandboxSpec,
        suggestions: Option<AgentSuggestions>,
    ) -> Result<SessionId, AgentSpawnError> {
        let definition = self
            .roles
            .get(role_name)
            .ok_or_else(|| Self::reject(format!("child role '{role_name}' is not registered")))?;

        // W2-19: die Agent-IR dieser Rolle wird vor jeder Prüfung aufgelöst,
        // weil sie die Tiefengrenze verschärfen darf. Ein fehlerhaftes Budget
        // (unbekanntes Effort-Label) lehnt die Admission fail-closed ab, bevor
        // irgendein Zustand entsteht.
        let executable_ir = definition.registry_factory.executable_agent_ir(role_name);
        let child_budget = match executable_ir.and_then(|ir| ir.spawn_contract().budget()) {
            Some(spec) => Self::budget_from_spec(spec)?,
            None => AgentBudget::default(),
        };
        let allow_pause = executable_ir.is_some_and(|ir| ir.lifecycle_machine().allow_pause());

        // The manager lock is taken up front (rather than after the active/
        // cancellations locks, as in earlier revisions) so the parent's
        // trusted `SpawnContext.organizational_role` is available for the
        // spawn-matrix authority check below — the most fundamental check,
        // ahead of the active-child limit, sandbox, and depth checks.
        let mut manager = self
            .manager
            .lock()
            .map_err(|_| Self::reject("session manager lock is poisoned"))?;
        // F-017/E3b: Die Aktivierung des Elternteils wird hier mitgelesen, aus
        // derselben vertrauenswürdigen Quelle wie Sandbox, Effort und Tiefe.
        // Für ein Kind eines Kindes (jede Ebene ab 2) ist das die Aktivierung
        // der Elternsitzung im Manager — dieselbe Sitzung, die `manager.get`
        // hier liefert und die bei ihrer eigenen Admission bereits gegen ihren
        // Elternteil geschnitten wurde. Der Schnitt ist damit über die ganze
        // Kette transitiv: Enkel ⊆ Kind ⊆ Wurzel.
        let (parent_context, parent_reasoning_effort, parent_activation, parent_depth) =
            match manager.get(&input.parent_session_id) {
                Ok(parent) => (
                    parent.spawn_context().cloned().ok_or_else(|| {
                        Self::reject("child parent has no trusted sandbox context")
                    })?,
                    parent.reasoning_effort(),
                    parent.activation().clone(),
                    Self::parent_depth(&manager, &input.parent_session_id)?,
                ),
                Err(_) => {
                    let external_root = self
                        .external_root_parent
                        .as_ref()
                        .filter(|root| root.session_id == input.parent_session_id)
                        .ok_or_else(|| {
                            Self::reject(format!(
                                "unknown child parent: {}",
                                input.parent_session_id
                            ))
                        })?;
                    (
                        external_root.spawn_context.clone(),
                        external_root.reasoning_effort,
                        external_root.activation.clone(),
                        0,
                    )
                }
            };
        if !harw_agent_dsl::roles::can_spawn(
            parent_context.organizational_role,
            definition.organizational_role,
        ) {
            return Err(Self::reject(format!(
                "organizational role {:?} is not permitted to spawn role {:?} (role '{role_name}')",
                parent_context.organizational_role, definition.organizational_role
            )));
        }

        let mut active = self
            .active
            .lock()
            .map_err(|_| Self::reject("child registry lock is poisoned"))?;
        let mut cancellations = self
            .cancellations
            .lock()
            .map_err(|_| Self::reject("child cancellation registry lock is poisoned"))?;
        let active_for_parent = active
            .values()
            .filter(|record| record.parent == input.parent_session_id)
            .count();
        if active_for_parent >= self.limits.max_active_children_per_parent {
            return Err(Self::reject(format!(
                "parent {} reached its active child limit of {}",
                input.parent_session_id, self.limits.max_active_children_per_parent
            )));
        }

        sandbox
            .ensure_child_of(&parent_context.sandbox)
            .map_err(|error| Self::reject(format!("child sandbox escalation rejected: {error}")))?;
        let approval_actor = parent_context.approval_actor.clone();
        // AW1-01b: Trace wird vererbt, nie neu erzeugt — dieselbe `trace_id`,
        // eine frische `span_id` für das Kind, siehe `inherit_trace`.
        let child_trace = inherit_trace(parent_context.trace.as_ref())?;
        // AW2-02: Die Kontext-Decke wird im selben Schritt geschnitten wie die
        // Berechtigungen — unmittelbar neben der Sandbox-Eskalationsprüfung
        // oben und der Trace-Vererbung direkt darüber, nicht in einem
        // zweiten Schritt, der vergessen werden könnte. Siehe `cut_ceiling`.
        let child_ceiling =
            Self::cut_ceiling(parent_context.ceiling.as_ref(), input.ceiling.as_ref())?;
        // Ein von der Rolle mitgebrachtes ContextProgram (unten, W2-19/
        // Folgeknoten zu AW2-01/AW2-02) darf die soeben geschnittene Decke
        // nie erweitern. Die Prüfung sitzt hier, direkt neben dem
        // Deckenschnitt, aus demselben Grund wie dieser selbst: ein
        // zweiter, separater Prüfschritt wäre eine Lücke, die vergessen
        // werden könnte.
        if let Some(ir) = executable_ir {
            if let Some(violation) =
                Self::describe_context_program_ceiling_violation(&child_ceiling, ir.context_program())
            {
                return Err(Self::reject(format!(
                    "child context program escalation rejected: {violation}"
                )));
            }
        }
        // Wave 8: Reasoning-Effort wird monoton vom Parent geerbt (Kind ≤ Parent).
        // Der Default ist Gleichheit; ein Budget-Clamp oder Owner-Override wird
        // nach der Session-Erzeugung über `clamp_child_reasoning_effort` angewandt.
        let depth = parent_depth.saturating_add(1);
        // W2-19 (korrigiert): `spawn.max_depth` einer Agent-IR begrenzt, wie
        // viele Ebenen an Kindagenten **unterhalb** des Agenten noch entstehen
        // dürfen — `Some(0)` heißt „dieser Agent darf keine Kinder erzeugen",
        // nicht „dieser Agent darf nicht selbst auf Tiefe 1 sitzen" (Belegstelle:
        // `harw_agent_dsl::executable::SpawnContract`, Feld `max_depth`).
        //
        // Maßgeblich ist deshalb der Vertrag des **Elternteils**, nicht der der
        // gerade gespawnten Rolle: gelesen wird die bei der Admission des
        // Elternteils berechnete, mitgeführte Tiefendecke
        // [`ChildRecord::depth_ceiling`] — dieselbe Art vererbter Zusage wie die
        // Kontext-Decke und der Trace darüber. Ein Elternteil ohne Lease-Eintrag
        // (jede Wurzel, extern registriert oder Manager-eigen) trägt keine
        // engere Zusage; für sie gilt allein die harte Controller-Grenze.
        let inherited_depth_ceiling = active
            .get(input.parent_session_id.as_str())
            .map_or(self.limits.max_depth, |parent| parent.depth_ceiling);
        if depth > inherited_depth_ceiling {
            return Err(Self::reject(format!(
                "child depth {depth} exceeds maximum {inherited_depth_ceiling}"
            )));
        }
        // Die Decke, die dieses Kind seinerseits an seine Nachkommen weitergibt.
        // Die Verschärfungsrichtung bleibt unverändert: die eigene IR schneidet
        // nur nach unten (`min`), sodass `self.limits.max_depth` über die ganze
        // Kette die harte Obergrenze bleibt und kein Rollenwert sie anhebt.
        let child_depth_ceiling = executable_ir
            .and_then(|ir| ir.spawn_contract().max_depth())
            .map_or(inherited_depth_ceiling, |ir_depth| {
                inherited_depth_ceiling.min(depth.saturating_add(ir_depth))
            });

        let capability_snapshot = definition
            .registry_factory
            .capability_snapshot(role_name, &input)?;
        let child_suggestions = capability_snapshot
            .as_ref()
            .map(|snapshot| snapshot.suggestions.clone())
            .or(suggestions);
        let registry = definition
            .registry_factory
            .build_registry_with_capabilities(role_name, &input, capability_snapshot.as_ref())?;
        if self.limits.lease_seconds <= 0 {
            return Err(Self::reject("child lease duration must be positive"));
        }
        let admitted_at = Timestamp::now();
        let lease_expires_at = admitted_at
            .checked_add(SignedDuration::from_secs(self.limits.lease_seconds))
            .map_err(|error| Self::reject(format!("child lease overflow: {error}")))?;
        let child = manager.create_governed_session(
            definition.role.clone(),
            Some(input.parent_session_id.clone()),
            registry,
            SpawnContext {
                sandbox,
                suggestions: child_suggestions,
                capability_snapshot,
                approval_actor,
                organizational_role: definition.organizational_role,
                trace: child_trace.clone(),
                // AW2-02: dieselbe Decke, die soeben neben der Sandbox
                // geschnitten wurde — kein zweiter, separater Zustand.
                ceiling: Some(child_ceiling),
            },
        );
        // W2-19: Tool-Aktivierung aus der Agent-IR. `with_executable_agent_ir`
        // ist ein verbrauchender Builder, deshalb wird die frisch angelegte
        // Session einmal entnommen und konfiguriert zurückgelegt — noch unter
        // demselben Manager-Lock, also für niemanden sonst beobachtbar.
        if let Some(ir) = executable_ir {
            let child_session = manager.remove(&child).ok_or_else(|| {
                Self::reject(format!(
                    "freshly created child session {child} disappeared before IR activation"
                ))
            })?;
            let mut child_session = child_session.with_executable_agent_ir(ir);
            // Die Rolle bringt ihr Programm aus ihrer eigenen Definition mit
            // — sie erbt es nicht vom Elternteil (§ Moduldoku
            // `harw-core/src/session.rs`, "Folgeknoten zu AW2-01/AW2-02").
            // Eine Rolle ohne deklariertes Programm (`ContextProgram::default()`,
            // der Fall für jede Agent-IR, deren `[context]`/`[context_program]`-
            // Tabelle fehlt) lässt `context_program()` bewusst auf `None` —
            // sonst würde `seed_context_load_ledger` (`turn_loop.rs`) für eine
            // Rolle ohne jede Erklärung plötzlich aktiv, statt unverändert
            // no-op zu bleiben. Gegen die Decke wurde das Programm bereits
            // oben, direkt neben `cut_ceiling`, geprüft.
            if ir.context_program() != &ContextProgram::default() {
                child_session = child_session.with_context_program(ir.context_program().clone());
            }
            manager.restore(child_session).map_err(|error| {
                Self::reject(format!(
                    "could not activate the agent IR for child {child}: {error}"
                ))
            })?;
        }
        // F-017/E3b: Die Kind-Aktivierung wird mit der des Elternteils
        // geschnitten — derselbe Schritt und derselbe Manager-Lock wie der
        // Deckenschnitt (`cut_ceiling`) und die Sandbox-Prüfung oben, damit
        // kein zweiter, vergessbarer Prüfschritt entsteht.
        //
        // Der Schnitt sitzt **nach** der IR-Aktivierung, denn genau die baut
        // die Kind-Aktivierung rollenbasiert neu auf (`with_executable_agent_ir`,
        // `session.rs`) und ohne IR bleibt `SessionActivation::default()` mit
        // Profil `Full` stehen. Beides darf nach dem Schnitt nicht mehr
        // erlauben als der Elternteil (Befund E1). `intersect` ist monoton:
        // das Ergebnis lässt nie mehr zu als eine der beiden Seiten allein.
        {
            let child_session = manager.get_mut(&child).map_err(|error| {
                Self::reject(format!(
                    "child session {child} disappeared before the activation cut: {error}"
                ))
            })?;
            // Der Schnitt geht in die **Basis** (`narrow_base_activation`),
            // nicht in den abgeleiteten aktuellen Wert: `set_mode`/`with_mode`
            // leiten `activation()` bei jedem Wechsel frisch aus der Basis ab
            // (`session.rs`, `apply_mode`), ein über `activation_mut()`
            // gesetztes Verbot fiele dabei weg. Eine Autoritätsgrenze darf
            // keinen Moduswechsel überleben müssen — sie muss ihn überleben.
            child_session.narrow_base_activation(&parent_activation);
        }
        // Monotone Vererbung: das Kind startet mit dem Effort-Level des Parents.
        if parent_reasoning_effort.is_some() {
            if let Ok(child_session) = manager.get_mut(&child) {
                child_session.set_reasoning_effort(parent_reasoning_effort);
            }
        }
        let record = ChildRecord {
            child: child.clone(),
            parent: input.parent_session_id,
            handoff_call_id: input.handoff_call_id,
            role: role_name.to_owned(),
            depth,
            admitted_at,
            lease_expires_at,
            budget: child_budget,
            allow_pause,
            depth_ceiling: child_depth_ceiling,
            trace: child_trace,
        };
        if let Some(lease_store) = &self.lease_store {
            if let Err(error) = lease_store.admit(&record.durable_lease()) {
                let _ = manager.remove(&child);
                return Err(Self::reject(format!(
                    "could not durably admit child lease: {error}"
                )));
            }
        }
        active.insert(child.as_str().to_owned(), record);
        let (cancel, _receiver) = watch::channel(false);
        cancellations.insert(child.as_str().to_owned(), cancel);
        Ok(child)
    }
}

impl AgentSpawner for ManagedAgentSpawner {
    fn spawn_child<'a>(
        &'a self,
        role: &'a str,
        input: SpawnInput,
        sandbox: SandboxSpec,
        suggestions: Option<AgentSuggestions>,
    ) -> SpawnFuture<'a> {
        Box::pin(async move { self.admit(role, input, sandbox, suggestions) })
    }

    fn child_finished(&self, child: &SessionId) {
        self.close_child(child);
    }

    fn child_completed(
        &self,
        child: &SessionId,
        completed_at: Timestamp,
    ) -> Result<(), AgentSpawnError> {
        self.close_child_durable(child, completed_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activation::ToolProfile;
    use crate::model::{EchoModelProvider, ModelFuture, ModelRequest, ModelResponse};
    use crate::session::AgentSession;
    use crate::state_store::InMemoryStateStore;
    use harw_agent_dsl::authority::AuthorityCeiling;
    use harw_agent_dsl::executable::lower;
    use harw_agent_dsl::parse::parse_toml;
    use harw_agent_dsl::resolved::{ResolutionTrace, ResolvedAgentDefinition};
    use harw_context::SectionName;
    use harw_extension_api::{
        ApprovalDecision, ApprovalHandler, ExtFuture, ExtensionRegistryBuilder,
    };
    use harw_sandbox::{Permission, PermissionSet, WorkspaceRegistration, WorkspaceRegistry};
    use harw_tools::{ToolCall, ToolName};
    use harw_types::{ApprovalActor, ItemId, TenantId, TokenUsage, WorkspaceId};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::mpsc;

    struct EmptyChildRegistry;

    impl ChildRegistryFactory for EmptyChildRegistry {
        fn build_registry(
            &self,
            _role: &str,
            _input: &SpawnInput,
            _suggestions: Option<&AgentSuggestions>,
        ) -> Result<ExtensionRegistry, AgentSpawnError> {
            Ok(ExtensionRegistryBuilder::default().build())
        }

        fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
            Err(AgentSpawnError {
                message: "test registry does not run children".to_owned(),
            })
        }
    }

    /// Registry-Factory mit echtem Echo-Provider — der Kind-Turn läuft durch
    /// und endet terminal.
    struct EchoChildRegistry {
        reply: &'static str,
    }

    impl ChildRegistryFactory for EchoChildRegistry {
        fn build_registry(
            &self,
            _role: &str,
            _input: &SpawnInput,
            _suggestions: Option<&AgentSuggestions>,
        ) -> Result<ExtensionRegistry, AgentSpawnError> {
            Ok(ExtensionRegistryBuilder::default().build())
        }

        fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
            Ok(Arc::new(EchoModelProvider::new(self.reply)))
        }
    }

    /// Modell, das nie antwortet — nur die kooperative Cancellation beendet
    /// einen darauf laufenden Kind-Turn.
    struct HangingModel;

    impl ModelProvider for HangingModel {
        fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
            Box::pin(async move {
                std::future::pending::<()>().await;
                Ok(ModelResponse::text("unreachable"))
            })
        }
    }

    struct HangingChildRegistry;

    impl ChildRegistryFactory for HangingChildRegistry {
        fn build_registry(
            &self,
            _role: &str,
            _input: &SpawnInput,
            _suggestions: Option<&AgentSuggestions>,
        ) -> Result<ExtensionRegistry, AgentSpawnError> {
            Ok(ExtensionRegistryBuilder::default().build())
        }

        fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
            Ok(Arc::new(HangingModel))
        }
    }

    /// Modell, das die Zahl gleichzeitig laufender Kind-Turns misst. Es hält
    /// zwischen Zähler-Inkrement und -Dekrement mehrere Yield-Punkte, damit
    /// echte Nebenläufigkeit sichtbar wird.
    struct ConcurrencyProbeModel {
        inflight: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
    }

    impl ModelProvider for ConcurrencyProbeModel {
        fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
            let inflight = Arc::clone(&self.inflight);
            let peak = Arc::clone(&self.peak);
            Box::pin(async move {
                let current = inflight.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(current, Ordering::SeqCst);
                for _ in 0..8 {
                    tokio::task::yield_now().await;
                }
                inflight.fetch_sub(1, Ordering::SeqCst);
                Ok(ModelResponse::text("probe complete"))
            })
        }
    }

    struct ConcurrencyProbeRegistry {
        inflight: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
    }

    impl ChildRegistryFactory for ConcurrencyProbeRegistry {
        fn build_registry(
            &self,
            _role: &str,
            _input: &SpawnInput,
            _suggestions: Option<&AgentSuggestions>,
        ) -> Result<ExtensionRegistry, AgentSpawnError> {
            Ok(ExtensionRegistryBuilder::default().build())
        }

        fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
            Ok(Arc::new(ConcurrencyProbeModel {
                inflight: Arc::clone(&self.inflight),
                peak: Arc::clone(&self.peak),
            }))
        }
    }

    /// Modell, das genau einen Tool-Call anfordert — zusammen mit dem
    /// `AskUserApproval`-Handler pausiert der Turn dadurch.
    struct ToolCallingModel;

    impl ModelProvider for ToolCallingModel {
        fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
            Box::pin(async move {
                Ok(ModelResponse {
                    message: None,
                    tool_calls: vec![ToolCall {
                        id: ToolCallId::new(),
                        name: ToolName::new("fs.read"),
                        arguments: serde_json::Value::Null,
                    }],
                    usage: TokenUsage::default(),
                })
            })
        }
    }

    struct ToolCallingChildRegistry;

    impl ChildRegistryFactory for ToolCallingChildRegistry {
        fn build_registry(
            &self,
            _role: &str,
            _input: &SpawnInput,
            _suggestions: Option<&AgentSuggestions>,
        ) -> Result<ExtensionRegistry, AgentSpawnError> {
            Ok(ExtensionRegistryBuilder::default().build())
        }

        fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
            Ok(Arc::new(ToolCallingModel))
        }
    }

    struct AskUserApproval;

    impl ApprovalHandler for AskUserApproval {
        fn review<'a>(&'a self, _call: &'a ToolCall) -> ExtFuture<'a, ApprovalDecision> {
            Box::pin(async move { ApprovalDecision::AskUser(ItemId::new()) })
        }
    }

    /// Registry-Factory, die eine gefrorene Agent-IR liefert.
    struct IrChildRegistry {
        ir: ExecutableAgentIr,
    }

    impl ChildRegistryFactory for IrChildRegistry {
        fn build_registry(
            &self,
            _role: &str,
            _input: &SpawnInput,
            _suggestions: Option<&AgentSuggestions>,
        ) -> Result<ExtensionRegistry, AgentSpawnError> {
            Ok(ExtensionRegistryBuilder::default().build())
        }

        fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
            Ok(Arc::new(EchoModelProvider::new("ir child complete")))
        }

        fn executable_agent_ir(&self, _role: &str) -> Option<&ExecutableAgentIr> {
            Some(&self.ir)
        }
    }

    /// Lowert eine Test-Agent-IR aus einem TOML-Fragment (Sektionen `[spawn]`,
    /// `[spawn.budget]`, `[lifecycle]`, `[tools]`).
    fn test_agent_ir(sections: &str) -> ExecutableAgentIr {
        let raw = parse_toml(&format!(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.child-controller-test@1"
version = "1.0.0"
role = "worker"
specialization = "child-controller-test"
{sections}
"#
        ))
        .expect("test agent definition must parse");
        let resolved = ResolvedAgentDefinition {
            id: raw.id,
            version: raw.version,
            role: raw.role,
            specialization: raw.specialization,
            name: raw.name,
            description: raw.description,
            authority: AuthorityCeiling::default(),
            trace: ResolutionTrace { steps: Vec::new() },
            config: raw.tables,
        };
        lower(&resolved).expect("test agent definition must lower")
    }

    fn test_sandbox(permissions: PermissionSet) -> SandboxSpec {
        let harness_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("harw-core has a workspace parent")
            .to_path_buf();
        let registry = WorkspaceRegistry::build(
            &harness_root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("external-root-controller-tests"),
                root: PathBuf::from("harw-core"),
            }],
        )
        .expect("test workspace is registered");
        SandboxSpec::from_resolved(
            registry
                .resolve(
                    &TenantId::from_str("test-tenant"),
                    &WorkspaceId::from_str("external-root-controller-tests"),
                )
                .expect("test workspace resolves"),
            permissions,
        )
    }

    fn external_root_context(
        sandbox: SandboxSpec,
        organizational_role: harw_agent_dsl::roles::AgentRoleId,
    ) -> SpawnContext {
        SpawnContext {
            sandbox,
            suggestions: None,
            capability_snapshot: None,
            approval_actor: Some(ApprovalActor::Operator {
                id: "external-root-operator".to_owned(),
            }),
            organizational_role,
            trace: None,
            // AW2-02: `None` here is fail-closed (see `SpawnContext::ceiling`
            // doc), not "unlimited" — tests that need a specific parent
            // ceiling override this field afterward, the same way existing
            // tests override `trace` after calling this helper.
            ceiling: None,
        }
    }

    fn worker_spawner(manager: Arc<Mutex<SessionManager>>) -> ManagedAgentSpawner {
        ManagedAgentSpawner::new(manager, ChildLimits::conservative()).with_role(
            "worker",
            AgentRole::Agent {
                name: "worker".to_owned(),
            },
            harw_agent_dsl::roles::AgentRoleId::Worker,
            Arc::new(EmptyChildRegistry),
        )
    }

    fn spawn_input(parent_session_id: SessionId) -> SpawnInput {
        SpawnInput {
            parent_session_id,
            handoff_call_id: ToolCallId::new(),
            instructions: None,
            context: serde_json::Value::Null,
            ceiling: None,
        }
    }

    /// Wie [`spawn_input`], aber mit einer vom Kind geforderten Decke — für
    /// Tests der AW2-02-Eskalationsprüfung.
    fn spawn_input_requesting(parent_session_id: SessionId, ceiling: ContextCeiling) -> SpawnInput {
        SpawnInput {
            ceiling: Some(ceiling),
            ..spawn_input(parent_session_id)
        }
    }

    fn spawner_with_admitted_child() -> (ManagedAgentSpawner, SessionId) {
        spawner_with_admitted_child_and_lease_store(None)
    }

    fn spawner_with_admitted_child_and_lease_store(
        lease_store: Option<Arc<ChildLeaseStore>>,
    ) -> (ManagedAgentSpawner, SessionId) {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events.clone())));
        let child_session = AgentSession::new(
            AgentRole::Agent {
                name: "worker".to_owned(),
            },
            None,
            ExtensionRegistryBuilder::default().build(),
            events,
        );
        let child = child_session.id().clone();
        manager
            .lock()
            .expect("test session manager lock")
            .restore(child_session)
            .expect("test child session restores");

        let spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative());
        let spawner = if let Some(lease_store) = lease_store.as_ref() {
            spawner.with_lease_store(lease_store.clone())
        } else {
            spawner
        };
        let now = Timestamp::now();
        let record = ChildRecord {
            child: child.clone(),
            parent: SessionId::new(),
            handoff_call_id: ToolCallId::new(),
            role: "worker".to_owned(),
            depth: 1,
            admitted_at: now,
            lease_expires_at: now,
            budget: AgentBudget::default(),
            allow_pause: false,
            depth_ceiling: ChildLimits::conservative().max_depth,
            trace: None,
        };
        if let Some(lease_store) = lease_store {
            lease_store
                .admit(&record.durable_lease())
                .expect("test lease admission persists");
        }
        spawner
            .active
            .lock()
            .expect("test child registry lock")
            .insert(child.as_str().to_owned(), record);
        (spawner, child)
    }

    #[test]
    fn child_final_assistant_text_returns_the_newest_response() {
        let (spawner, child) = spawner_with_admitted_child();
        let mut manager = spawner.manager.lock().expect("test session manager lock");
        let history = manager
            .get_mut(&child)
            .expect("admitted child session exists")
            .history_mut();
        history.push_assistant_text("intermediate response", None);
        history.push_user_text("continue");
        history.push_assistant_text("completed child response", None);
        drop(manager);

        assert_eq!(
            spawner
                .child_final_assistant_text(&child)
                .expect("completed child text is available"),
            "completed child response"
        );
    }

    #[test]
    fn child_final_assistant_text_rejects_unknown_child() {
        let (spawner, _child) = spawner_with_admitted_child();
        let error = spawner
            .child_final_assistant_text(&SessionId::new())
            .expect_err("unknown child must be rejected");

        assert!(error.message.contains("is not admitted"));
    }

    #[test]
    fn child_final_assistant_text_rejects_child_without_assistant_response() {
        let (spawner, child) = spawner_with_admitted_child();
        spawner
            .manager
            .lock()
            .expect("test session manager lock")
            .get_mut(&child)
            .expect("admitted child session exists")
            .history_mut()
            .push_user_text("work on this");

        let error = spawner
            .child_final_assistant_text(&child)
            .expect_err("child without an assistant response must be rejected");

        assert!(error.message.contains("no assistant response text"));
    }

    #[test]
    fn child_completed_durably_records_completion_before_releasing_admission() {
        let temporary_directory = tempfile::tempdir().expect("temporary lease directory");
        let lease_store = Arc::new(ChildLeaseStore::new(temporary_directory.path()));
        let (spawner, child) =
            spawner_with_admitted_child_and_lease_store(Some(lease_store.clone()));
        let record = spawner
            .child_record(&child)
            .expect("test child is admitted");
        let completed_at = Timestamp::now();

        AgentSpawner::child_completed(&spawner, &child, completed_at)
            .expect("durable child completion succeeds");

        assert!(spawner.child_record(&child).is_none());
        assert!(
            lease_store
                .active()
                .expect("active leases are readable")
                .is_empty()
        );
        let completion_path = lease_store
            .root()
            .join(format!("{}.completed.json", child.as_str()));
        let completion: harw_session_store::ChildLeaseCompletionRecord = serde_json::from_slice(
            &std::fs::read(completion_path).expect("completion record is written"),
        )
        .expect("completion record is valid JSON");
        assert_eq!(completion.lease, record.durable_lease());
        assert_eq!(completion.completed_at, completed_at);
    }

    #[test]
    fn external_root_parent_admits_a_child_without_a_manager_mirror() {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
        let parent = SessionId::new();
        let spawner = worker_spawner(manager.clone())
            .with_external_root_parent(
                parent.clone(),
                external_root_context(
                    sandbox.clone(),
                    harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
                ),
                Some(ReasoningEffort::Medium),
                SessionActivation::default(),
            )
            .expect("trusted external root registers during construction");

        let child = spawner
            .admit("worker", spawn_input(parent.clone()), sandbox, None)
            .expect("registered external root admits a direct child");

        let record = spawner.child_record(&child).expect("child is tracked");
        assert_eq!(record.parent, parent);
        assert_eq!(record.depth, 1);
        let manager = manager.lock().expect("manager lock");
        let child_session = manager.get(&child).expect("child is manager-owned");
        assert_eq!(child_session.parent_session_id(), Some(&record.parent));
        assert_eq!(
            child_session.reasoning_effort(),
            Some(ReasoningEffort::Medium)
        );
        assert_eq!(
            child_session
                .spawn_context()
                .expect("child has trusted context")
                .approval_actor,
            Some(ApprovalActor::Operator {
                id: "external-root-operator".to_owned(),
            })
        );
    }

    #[test]
    fn external_root_parent_rejects_unknown_parent_ids() {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
        let spawner = worker_spawner(manager)
            .with_external_root_parent(
                SessionId::new(),
                external_root_context(
                    sandbox.clone(),
                    harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
                ),
                None,
                SessionActivation::default(),
            )
            .expect("trusted external root registers during construction");

        let error = spawner
            .admit("worker", spawn_input(SessionId::new()), sandbox, None)
            .expect_err("unregistered parent must be denied");

        assert!(error.message.contains("unknown child parent"));
    }

    #[test]
    fn external_root_parent_rejects_sandbox_escalation() {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let parent_sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
        let parent = SessionId::new();
        let spawner = worker_spawner(manager)
            .with_external_root_parent(
                parent.clone(),
                external_root_context(
                    parent_sandbox,
                    harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
                ),
                None,
                SessionActivation::default(),
            )
            .expect("trusted external root registers during construction");

        let error = spawner
            .admit(
                "worker",
                spawn_input(parent),
                test_sandbox(PermissionSet::from_policy([
                    Permission::ReadWorkspace,
                    Permission::WriteWorkspace,
                ])),
                None,
            )
            .expect_err("external root cannot escalate a child sandbox");

        assert!(error.message.contains("child sandbox escalation rejected"));
    }

    #[test]
    fn external_root_parent_enforces_the_role_spawn_matrix() {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
        let parent = SessionId::new();
        let spawner = worker_spawner(manager)
            .with_external_root_parent(
                parent.clone(),
                external_root_context(sandbox.clone(), harw_agent_dsl::roles::AgentRoleId::Worker),
                None,
                SessionActivation::default(),
            )
            .expect("trusted external root registers during construction");

        let error = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .expect_err("worker roots cannot spawn durable workers");

        assert!(error.message.contains("not permitted to spawn role"));
    }

    // --- W2-16: `ChildLimits`-Ableitung ------------------------------------

    #[test]
    fn with_max_children_reduces_only_the_active_ceiling() {
        let limits = ChildLimits::with_max_children(3);

        assert_eq!(limits.max_active_children_per_parent, 3);
        assert_eq!(limits.max_depth, ChildLimits::conservative().max_depth);
        assert_eq!(
            limits.lease_seconds,
            ChildLimits::conservative().lease_seconds
        );
    }

    #[test]
    fn with_max_children_never_raises_the_conservative_ceiling() {
        assert_eq!(
            ChildLimits::with_max_children(64).max_active_children_per_parent,
            ChildLimits::conservative().max_active_children_per_parent
        );
    }

    #[test]
    fn with_max_children_lifts_a_zero_fanout_to_one() {
        assert_eq!(
            ChildLimits::with_max_children(0).max_active_children_per_parent,
            1
        );
    }

    // --- W2-17/18 gemeinsame Testinfrastruktur ----------------------------

    /// Baut einen Spawner mit `children` bereits admittierten, tatsächlich
    /// ausführbaren Kindern: Rolle registriert, Cancellation-Kanal vorhanden,
    /// Lease weit in der Zukunft, Spawn-Kontext mit Approval-Actor.
    fn runnable_children(
        factory: Arc<dyn ChildRegistryFactory>,
        allow_pause: bool,
        children: usize,
        mut registry: impl FnMut() -> ExtensionRegistry,
    ) -> (ManagedAgentSpawner, Vec<SessionId>) {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events.clone())));
        let spawner = ManagedAgentSpawner::new(Arc::clone(&manager), ChildLimits::conservative())
            .with_role(
                "worker",
                AgentRole::Agent {
                    name: "worker".to_owned(),
                },
                harw_agent_dsl::roles::AgentRoleId::Worker,
                factory,
            );
        let parent = SessionId::new();
        let admitted_at = Timestamp::now();
        let lease_expires_at = admitted_at
            .checked_add(SignedDuration::from_secs(300))
            .expect("test lease does not overflow");
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
        let mut ids = Vec::with_capacity(children);
        for _ in 0..children {
            let session = AgentSession::new(
                AgentRole::Agent {
                    name: "worker".to_owned(),
                },
                Some(parent.clone()),
                registry(),
                events.clone(),
            )
            .with_spawn_context(external_root_context(
                sandbox.clone(),
                harw_agent_dsl::roles::AgentRoleId::Worker,
            ));
            let child = session.id().clone();
            manager
                .lock()
                .expect("test session manager lock")
                .restore(session)
                .expect("test child session restores");
            spawner
                .active
                .lock()
                .expect("test child registry lock")
                .insert(
                    child.as_str().to_owned(),
                    ChildRecord {
                        child: child.clone(),
                        parent: parent.clone(),
                        handoff_call_id: ToolCallId::new(),
                        role: "worker".to_owned(),
                        depth: 1,
                        admitted_at,
                        lease_expires_at,
                        budget: AgentBudget::default(),
                        allow_pause,
                        depth_ceiling: ChildLimits::conservative().max_depth,
                        trace: None,
                    },
                );
            let (cancel, _cancel_receiver) = watch::channel(false);
            spawner
                .cancellations
                .lock()
                .expect("test cancellation registry lock")
                .insert(child.as_str().to_owned(), cancel);
            ids.push(child);
        }
        (spawner, ids)
    }

    fn empty_registry() -> ExtensionRegistry {
        ExtensionRegistryBuilder::default().build()
    }

    fn child_is_manager_owned(spawner: &ManagedAgentSpawner, child: &SessionId) -> bool {
        spawner
            .manager
            .lock()
            .expect("test session manager lock")
            .get(child)
            .is_ok()
    }

    // --- W2-17: Budget-Durchsetzung ---------------------------------------

    #[tokio::test]
    async fn wall_time_budget_cancels_cooperatively_and_returns_the_session() {
        let (spawner, children) =
            runnable_children(Arc::new(HangingChildRegistry), true, 1, empty_registry);
        let child = children[0].clone();
        let store = InMemoryStateStore::new();

        let error = spawner
            .run_child_with_budget(
                &child,
                &store,
                None,
                TurnInput::user("work"),
                AgentBudget {
                    max_wall_time_ms: Some(25),
                    ..AgentBudget::default()
                },
            )
            .await
            .expect_err("an elapsed wall-time budget must reject the run");

        assert!(
            error
                .message
                .starts_with("budget_exceeded: wall_time (limit=25, used="),
            "unexpected message: {}",
            error.message
        );
        assert!(
            child_is_manager_owned(&spawner, &child),
            "the cooperatively cancelled child must be back in the session manager"
        );
    }

    #[tokio::test]
    async fn tool_call_budget_violation_reports_a_machine_readable_message() {
        let (spawner, children) = runnable_children(
            Arc::new(EchoChildRegistry {
                reply: "worker complete",
            }),
            true,
            1,
            empty_registry,
        );
        let child = children[0].clone();
        {
            let mut manager = spawner.manager.lock().expect("test session manager lock");
            let history = manager
                .get_mut(&child)
                .expect("admitted child session exists")
                .history_mut();
            for _ in 0..3 {
                history.push_tool_call(ToolCallId::new(), "fs.read", serde_json::Value::Null);
            }
        }
        let store = InMemoryStateStore::new();

        let error = spawner
            .run_child_with_budget(
                &child,
                &store,
                None,
                TurnInput::user("work"),
                AgentBudget {
                    max_tool_calls: Some(1),
                    ..AgentBudget::default()
                },
            )
            .await
            .expect_err("an exceeded tool-call budget must reject the run");

        assert_eq!(
            error.message,
            "budget_exceeded: tool_calls (limit=1, used=3)"
        );
    }

    #[tokio::test]
    async fn a_run_inside_its_tool_call_budget_succeeds() {
        let (spawner, children) = runnable_children(
            Arc::new(EchoChildRegistry {
                reply: "worker complete",
            }),
            true,
            1,
            empty_registry,
        );
        let child = children[0].clone();
        let store = InMemoryStateStore::new();

        let result = spawner
            .run_child_with_budget(
                &child,
                &store,
                None,
                TurnInput::user("work"),
                AgentBudget {
                    max_tool_calls: Some(2),
                    max_wall_time_ms: Some(30_000),
                    ..AgentBudget::default()
                },
            )
            .await
            .expect("a child inside its budget must complete");

        assert_eq!(result.child, child);
        assert!(matches!(result.outcome, TurnOutcome::Completed));
    }

    // --- W2-18: Fan-out ----------------------------------------------------

    fn fanout_requests(children: &[SessionId]) -> Vec<FanoutRequest> {
        children
            .iter()
            .enumerate()
            .map(|(index, child)| FanoutRequest {
                child: child.clone(),
                input: TurnInput::user(format!("question {index}")),
                budget: AgentBudget::default(),
            })
            .collect()
    }

    #[tokio::test]
    async fn run_children_returns_results_in_request_order() {
        let (spawner, children) = runnable_children(
            Arc::new(EchoChildRegistry {
                reply: "worker complete",
            }),
            true,
            3,
            empty_registry,
        );
        let store = InMemoryStateStore::new();

        let results = spawner
            .run_children(
                fanout_requests(&children),
                &store,
                3,
                JoinSemantics::AllTerminal,
            )
            .await;

        assert_eq!(results.len(), 3);
        for (position, result) in results.into_iter().enumerate() {
            let run = result.expect("every child of the wave completes");
            assert_eq!(
                run.child, children[position],
                "result slot {position} must carry the child requested at that position"
            );
        }
    }

    #[tokio::test]
    async fn run_children_with_max_parallel_one_serialises_the_wave() {
        let inflight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (spawner, children) = runnable_children(
            Arc::new(ConcurrencyProbeRegistry {
                inflight: Arc::clone(&inflight),
                peak: Arc::clone(&peak),
            }),
            true,
            3,
            empty_registry,
        );
        let store = InMemoryStateStore::new();

        let results = spawner
            .run_children(
                fanout_requests(&children),
                &store,
                1,
                JoinSemantics::AllTerminal,
            )
            .await;

        assert_eq!(results.len(), 3);
        assert!(results.iter().all(Result::is_ok));
        assert_eq!(
            peak.load(Ordering::SeqCst),
            1,
            "max_parallel = 1 must never have two child turns in flight"
        );
    }

    #[tokio::test]
    async fn run_children_actually_overlaps_child_turns() {
        let inflight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (spawner, children) = runnable_children(
            Arc::new(ConcurrencyProbeRegistry {
                inflight: Arc::clone(&inflight),
                peak: Arc::clone(&peak),
            }),
            true,
            3,
            empty_registry,
        );
        let store = InMemoryStateStore::new();

        let results = spawner
            .run_children(
                fanout_requests(&children),
                &store,
                3,
                JoinSemantics::Collect,
            )
            .await;

        assert_eq!(results.len(), 3);
        assert!(
            peak.load(Ordering::SeqCst) >= 2,
            "the session-manager lock must not serialise independent child turns"
        );
    }

    #[tokio::test]
    async fn run_children_any_terminal_cancels_the_remaining_children() {
        // Das erste Kind antwortet sofort, die beiden anderen nie — ohne
        // AnyTerminal-Abbruch würde die Welle hängen.
        struct FirstWinsRegistry {
            fast: Mutex<bool>,
        }
        impl ChildRegistryFactory for FirstWinsRegistry {
            fn build_registry(
                &self,
                _role: &str,
                _input: &SpawnInput,
                _suggestions: Option<&AgentSuggestions>,
            ) -> Result<ExtensionRegistry, AgentSpawnError> {
                Ok(ExtensionRegistryBuilder::default().build())
            }

            fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
                let mut fast = self.fast.lock().map_err(|_| AgentSpawnError {
                    message: "test registry lock is poisoned".to_owned(),
                })?;
                let model: Arc<dyn ModelProvider> = if *fast {
                    *fast = false;
                    Arc::new(EchoModelProvider::new("winner"))
                } else {
                    Arc::new(HangingModel)
                };
                Ok(model)
            }
        }

        let (spawner, children) = runnable_children(
            Arc::new(FirstWinsRegistry {
                fast: Mutex::new(true),
            }),
            true,
            3,
            empty_registry,
        );
        let store = InMemoryStateStore::new();

        let results = spawner
            .run_children(
                fanout_requests(&children),
                &store,
                3,
                JoinSemantics::AnyTerminal,
            )
            .await;

        assert_eq!(results.len(), 3);
        let winner = results[0].as_ref().expect("the first child wins the wave");
        assert_eq!(winner.child, children[0]);
        for position in [1_usize, 2] {
            let error = results[position]
                .as_ref()
                .expect_err("a cancelled sibling must not report a result");
            assert_eq!(error.message, CANCELLED_BY_SIBLING);
        }
    }

    #[tokio::test]
    async fn run_children_returns_an_empty_wave_unchanged() {
        let (spawner, _children) =
            runnable_children(Arc::new(EmptyChildRegistry), true, 0, empty_registry);
        let store = InMemoryStateStore::new();

        let results = spawner
            .run_children(Vec::new(), &store, 4, JoinSemantics::AllTerminal)
            .await;

        assert!(results.is_empty());
    }

    // --- W2-19: IR-gesteuerte Kind-Konfiguration ---------------------------

    #[tokio::test]
    async fn a_child_forbidden_to_pause_fails_closed_on_a_paused_turn() {
        let (spawner, children) =
            runnable_children(Arc::new(ToolCallingChildRegistry), false, 1, || {
                ExtensionRegistryBuilder::default()
                    .approval_handler(Arc::new(AskUserApproval))
                    .build()
            });
        let child = children[0].clone();
        let store = InMemoryStateStore::new();

        let error = spawner
            .run_child(&child, &store, TurnInput::user("work"))
            .await
            .expect_err("a pause-forbidden child must not report a waiting outcome");

        assert_eq!(
            error.message,
            "child paused but its lifecycle forbids pausing: awaiting_approval"
        );
        assert!(
            child_is_manager_owned(&spawner, &child),
            "the rejected child session must still be restored regularly"
        );
    }

    #[tokio::test]
    async fn a_child_allowed_to_pause_reports_the_waiting_outcome() {
        let (spawner, children) =
            runnable_children(Arc::new(ToolCallingChildRegistry), true, 1, || {
                ExtensionRegistryBuilder::default()
                    .approval_handler(Arc::new(AskUserApproval))
                    .build()
            });
        let child = children[0].clone();
        let store = InMemoryStateStore::new();

        let result = spawner
            .run_child(&child, &store, TurnInput::user("work"))
            .await
            .expect("a pause-permitted child may report a waiting outcome");

        assert!(matches!(
            result.outcome,
            TurnOutcome::AwaitingApproval { .. }
        ));
    }

    fn ir_spawner(ir: ExecutableAgentIr) -> (ManagedAgentSpawner, SessionId, SandboxSpec) {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
        let parent = SessionId::new();
        let spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative())
            .with_role(
                "worker",
                AgentRole::Agent {
                    name: "worker".to_owned(),
                },
                harw_agent_dsl::roles::AgentRoleId::Worker,
                Arc::new(IrChildRegistry { ir }),
            )
            .with_external_root_parent(
                parent.clone(),
                external_root_context(
                    sandbox.clone(),
                    harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
                ),
                None,
                SessionActivation::default(),
            )
            .expect("trusted external root registers during construction");
        (spawner, parent, sandbox)
    }

    #[test]
    fn admit_carries_the_agent_ir_budget_pause_lock_and_tool_surface() {
        let (spawner, parent, sandbox) = ir_spawner(test_agent_ir(
            r#"
[spawn.budget]
max_tokens = 4096
max_tool_calls = 12
max_wall_secs = 30
effort_cap = "low"

[lifecycle]
allow_pause = true

[tools]
admitted = ["fs.read"]
"#,
        ));

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .expect("the IR-governed child is admitted");

        let record = spawner.child_record(&child).expect("child is tracked");
        assert!(record.allow_pause);
        assert_eq!(
            spawner.child_budget(&child),
            Some(AgentBudget {
                max_tokens: Some(4096),
                max_tool_calls: Some(12),
                max_wall_time_ms: Some(30_000),
                reasoning_effort: Some(ReasoningEffort::Low),
            })
        );

        let manager = spawner.manager.lock().expect("test session manager lock");
        let session = manager.get(&child).expect("child is manager-owned");
        assert!(
            session
                .activation()
                .is_tool_enabled(&ToolName::new("fs.read"))
        );
        assert!(
            !session
                .activation()
                .is_tool_enabled(&ToolName::new("fs.write"))
        );
        assert!(session.executable_snapshot_id().is_some());
    }

    #[test]
    fn admit_defaults_to_a_pause_lock_without_an_agent_ir() {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
        let parent = SessionId::new();
        let spawner = worker_spawner(manager)
            .with_external_root_parent(
                parent.clone(),
                external_root_context(
                    sandbox.clone(),
                    harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
                ),
                None,
                SessionActivation::default(),
            )
            .expect("trusted external root registers during construction");

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .expect("child admitted");

        let record = spawner.child_record(&child).expect("child is tracked");
        assert!(
            !record.allow_pause,
            "without a lifecycle statement the controller stays fail-closed"
        );
        assert_eq!(spawner.child_budget(&child), Some(AgentBudget::default()));
    }

    /// Baut einen Spawner für die einzige zweistufige Kette, die die
    /// Spawn-Matrix aus §3 erlaubt: eine Manager-eigene Wurzel
    /// (`RootOrchestrator`) admittiert die Rolle `manager`
    /// (`ChildOrchestrator`), die ihrerseits `worker` admittieren kann. Die
    /// übergebene IR hängt an `manager` — an der Rolle also, deren
    /// Spawn-Vertrag über ihre *Nachkommen* entscheidet. Die Wurzel ist
    /// bewusst Manager-eigen, weil `parent_depth` die Kette rein über den
    /// SessionManager läuft (siehe
    /// `admit_propagates_the_root_trace_id_across_a_grandchild`).
    fn two_hop_spawner(
        manager_ir: ExecutableAgentIr,
    ) -> (ManagedAgentSpawner, SessionId, SandboxSpec) {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events.clone())));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
        let root_session = AgentSession::new(
            AgentRole::Agent {
                name: "root".to_owned(),
            },
            None,
            ExtensionRegistryBuilder::default().build(),
            events,
        )
        .with_spawn_context(external_root_context(
            sandbox.clone(),
            harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
        ));
        let root = root_session.id().clone();
        manager
            .lock()
            .expect("test session manager lock")
            .restore(root_session)
            .expect("root session restores");

        let spawner = ManagedAgentSpawner::new(Arc::clone(&manager), ChildLimits::conservative())
            .with_role(
                "manager",
                AgentRole::Agent {
                    name: "manager".to_owned(),
                },
                harw_agent_dsl::roles::AgentRoleId::ChildOrchestrator,
                Arc::new(IrChildRegistry { ir: manager_ir }),
            )
            .with_role(
                "worker",
                AgentRole::Agent {
                    name: "worker".to_owned(),
                },
                harw_agent_dsl::roles::AgentRoleId::Worker,
                Arc::new(EmptyChildRegistry),
            );
        (spawner, root, sandbox)
    }

    #[test]
    fn a_role_that_forbids_grandchildren_is_still_admissible_as_a_child() {
        // `max_depth = 0` heißt „diese Rolle darf keine Kinder erzeugen" — es
        // heißt nicht, dass sie selbst nicht als Kind laufen darf. Genau daran
        // scheiterten die `security-*-triage`-Rollen.
        let (spawner, parent, sandbox) = ir_spawner(test_agent_ir(
            r#"
[spawn]
max_depth = 0
"#,
        ));

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .expect("a role that forbids its own children is still admissible as a child");

        let record = spawner.child_record(&child).expect("child is tracked");
        assert_eq!(record.depth, 1);
        assert_eq!(
            record.depth_ceiling, 1,
            "max_depth = 0 auf Tiefe 1 deckelt die Nachkommen auf Tiefe 1 — also keine"
        );
    }

    #[test]
    fn the_agent_ir_can_only_tighten_the_depth_limit() {
        // Verschärfen: `max_depth = 0` am Elternteil verbietet den Enkel,
        // obwohl die Controller-Grenze (4) ihn zuließe.
        let (spawner, root, sandbox) = two_hop_spawner(test_agent_ir(
            r#"
[spawn]
max_depth = 0
"#,
        ));
        let child = spawner
            .admit("manager", spawn_input(root), sandbox.clone(), None)
            .expect("the depth-0 role is itself admissible");
        let error = spawner
            .admit("worker", spawn_input(child), sandbox, None)
            .expect_err("a parent contract of max_depth = 0 forbids any grandchild");
        assert_eq!(error.message, "child depth 2 exceeds maximum 1");

        // Erweitern: `max_depth = 99` hebt nichts an — die geerbte Decke bleibt
        // `ChildLimits::conservative().max_depth`.
        let (spawner, root, sandbox) = two_hop_spawner(test_agent_ir(
            r#"
[spawn]
max_depth = 99
"#,
        ));
        let child = spawner
            .admit("manager", spawn_input(root), sandbox.clone(), None)
            .expect("depth 1 is inside the conservative limit");
        assert_eq!(
            spawner
                .child_record(&child)
                .expect("child is tracked")
                .depth_ceiling,
            ChildLimits::conservative().max_depth,
            "an IR value above the controller limit must not raise the inherited ceiling"
        );
        let grandchild = spawner
            .admit("worker", spawn_input(child), sandbox, None)
            .expect("depth 2 is inside the conservative limit");
        assert_eq!(
            spawner
                .child_record(&grandchild)
                .expect("grandchild is tracked")
                .depth,
            2
        );
    }

    #[test]
    fn the_agent_ir_cannot_raise_the_controller_depth_limit() {
        // `max_depth = 99` in der Definition darf die konservative Grenze
        // nicht anheben — die Admission auf Tiefe 1 gelingt trotzdem, weil
        // `min(4, 99) = 4`.
        let (spawner, parent, sandbox) = ir_spawner(test_agent_ir(
            r#"
[spawn]
max_depth = 99
"#,
        ));

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .expect("depth 1 is inside the conservative limit");

        assert_eq!(
            spawner
                .child_record(&child)
                .expect("child is tracked")
                .depth,
            1
        );
    }

    #[test]
    fn an_unknown_effort_cap_rejects_the_admission_fail_closed() {
        let (spawner, parent, sandbox) = ir_spawner(test_agent_ir(
            r#"
[spawn.budget]
effort_cap = "ludicrous"
"#,
        ));

        let error = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .expect_err("an unknown effort label must not fall back to a default");

        assert!(
            error
                .message
                .contains("unknown reasoning-effort cap 'ludicrous'"),
            "unexpected message: {}",
            error.message
        );
    }

    // --- AW1-01b: Trace-Vererbung -------------------------------------------

    fn test_trace(trace_id: &str, span_id: &str) -> TraceContext {
        TraceContext::new(trace_id.to_owned(), span_id.to_owned()).expect("test trace is valid hex")
    }

    // --- AW2-02: Kontext-Decken-Vererbung ------------------------------------

    fn test_ceiling(sections: &[&str], max_trust: TrustClass, budget_total: u32) -> ContextCeiling {
        ContextCeiling {
            sections: sections
                .iter()
                .map(|name| SectionName::try_new(*name).expect("valid section name"))
                .collect(),
            max_trust,
            budget: ContextBudgetSpec {
                total: harw_lens_types::BudgetSpec {
                    total: budget_total,
                },
                per_section: BTreeMap::new(),
            },
        }
    }

    #[test]
    fn admit_inherits_the_parents_trace_id_but_assigns_a_fresh_span_id() {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
        let parent = SessionId::new();
        let parent_trace = test_trace(&"a".repeat(32), &"b".repeat(16));
        let mut parent_context = external_root_context(
            sandbox.clone(),
            harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
        );
        parent_context.trace = Some(parent_trace.clone());
        let spawner = worker_spawner(manager)
            .with_external_root_parent(
                parent.clone(),
                parent_context,
                None,
                SessionActivation::default(),
            )
            .expect("trusted external root registers during construction");

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .expect("child is admitted with an inherited trace");

        let manager = spawner.manager.lock().expect("test session manager lock");
        let child_trace = manager
            .get(&child)
            .expect("child is manager-owned")
            .spawn_context()
            .expect("child has a trusted spawn context")
            .trace
            .clone()
            .expect("child inherits a trace from its parent");

        assert_eq!(
            child_trace.trace_id, parent_trace.trace_id,
            "the child must carry the same trace_id as its parent — same work"
        );
        assert_ne!(
            child_trace.span_id, parent_trace.span_id,
            "the child must be its own span, not a copy of the parent's"
        );
        assert_eq!(
            child_trace.parent_span_id.as_deref(),
            Some(parent_trace.span_id.as_str()),
            "the child's parent_span_id must point at the parent's own span_id"
        );
    }

    #[test]
    fn admit_gives_a_traceless_parent_a_traceless_child() {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
        let parent = SessionId::new();
        let spawner = worker_spawner(manager)
            .with_external_root_parent(
                parent.clone(),
                external_root_context(
                    sandbox.clone(),
                    harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
                ),
                None,
                SessionActivation::default(),
            )
            .expect("trusted external root registers during construction");

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .expect("child is admitted");

        let manager = spawner.manager.lock().expect("test session manager lock");
        let child_context = manager
            .get(&child)
            .expect("child is manager-owned")
            .spawn_context()
            .expect("child has a trusted spawn context");

        assert!(
            child_context.trace.is_none(),
            "a parent without a trace must not hand its child a fabricated root trace"
        );
    }

    #[test]
    fn admit_propagates_the_root_trace_id_across_a_grandchild() {
        // A manager-owned root (RootOrchestrator) admits a "manager" child
        // (ChildOrchestrator), which in turn admits a "worker" grandchild
        // (Worker) — the only two-hop path the §3 spawn matrix allows. The
        // root is deliberately manager-owned rather than an external root
        // parent: `ManagedAgentSpawner::parent_depth` walks
        // `parent_session_id` purely through the manager, and an external
        // root has no manager entry for that walk to land on once a second
        // admission hop is involved.
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events.clone())));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
        let root_trace = test_trace(&"c".repeat(32), &"d".repeat(16));
        let root_context = SpawnContext {
            sandbox: sandbox.clone(),
            suggestions: None,
            capability_snapshot: None,
            approval_actor: None,
            organizational_role: harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
            trace: Some(root_trace.clone()),
            ceiling: None,
        };
        let root_session = AgentSession::new(
            AgentRole::Agent {
                name: "root".to_owned(),
            },
            None,
            ExtensionRegistryBuilder::default().build(),
            events,
        )
        .with_spawn_context(root_context);
        let root = root_session.id().clone();
        manager
            .lock()
            .expect("test session manager lock")
            .restore(root_session)
            .expect("root session restores");

        let spawner = ManagedAgentSpawner::new(Arc::clone(&manager), ChildLimits::conservative())
            .with_role(
                "manager",
                AgentRole::Agent {
                    name: "manager".to_owned(),
                },
                harw_agent_dsl::roles::AgentRoleId::ChildOrchestrator,
                Arc::new(EmptyChildRegistry),
            )
            .with_role(
                "worker",
                AgentRole::Agent {
                    name: "worker".to_owned(),
                },
                harw_agent_dsl::roles::AgentRoleId::Worker,
                Arc::new(EmptyChildRegistry),
            );

        let child = spawner
            .admit("manager", spawn_input(root), sandbox.clone(), None)
            .expect("child is admitted with an inherited trace");
        let grandchild = spawner
            .admit("worker", spawn_input(child), sandbox, None)
            .expect("grandchild is admitted with an inherited trace");

        let manager = manager.lock().expect("test session manager lock");
        let grandchild_trace = manager
            .get(&grandchild)
            .expect("grandchild is manager-owned")
            .spawn_context()
            .expect("grandchild has a trusted spawn context")
            .trace
            .clone()
            .expect("grandchild inherits a trace across two admission hops");

        assert_eq!(
            grandchild_trace.trace_id, root_trace.trace_id,
            "the grandchild must still carry the root's trace_id two hops down"
        );
    }

    #[test]
    fn admit_inherits_the_parents_cut_context_ceiling_when_the_child_requests_none() {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
        let parent = SessionId::new();
        let parent_ceiling =
            test_ceiling(&["history.tail", "plan.current"], TrustClass::Evidence, 750);
        let mut parent_context = external_root_context(
            sandbox.clone(),
            harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
        );
        parent_context.ceiling = Some(parent_ceiling.clone());
        let spawner = worker_spawner(manager)
            .with_external_root_parent(
                parent.clone(),
                parent_context,
                None,
                SessionActivation::default(),
            )
            .expect("trusted external root registers during construction");

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .expect("a child requesting no ceiling of its own is always admissible");

        let manager = spawner.manager.lock().expect("test session manager lock");
        let child_ceiling = manager
            .get(&child)
            .expect("child is manager-owned")
            .spawn_context()
            .expect("child has a trusted spawn context")
            .ceiling
            .clone()
            .expect("child inherits a ceiling from its parent");

        assert_eq!(
            child_ceiling, parent_ceiling,
            "a child that requests no ceiling of its own must inherit its parent's, unchanged"
        );
    }

    #[test]
    fn admit_rejects_a_child_that_requests_a_section_outside_the_parents_ceiling() {
        // The most important AW2-02 test: an over-reaching request must be
        // refused outright, not silently narrowed to fit.
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
        let parent = SessionId::new();
        let mut parent_context = external_root_context(
            sandbox.clone(),
            harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
        );
        parent_context.ceiling = Some(test_ceiling(&["history.tail"], TrustClass::Evidence, 500));
        let spawner = worker_spawner(manager)
            .with_external_root_parent(
                parent.clone(),
                parent_context,
                None,
                SessionActivation::default(),
            )
            .expect("trusted external root registers during construction");

        let requested = test_ceiling(
            &["history.tail", "secrets.vault"],
            TrustClass::Evidence,
            500,
        );
        let error = spawner
            .admit(
                "worker",
                spawn_input_requesting(parent, requested),
                sandbox,
                None,
            )
            .expect_err(
                "a child must never be granted a section its parent's ceiling does not carry",
            );

        assert!(
            error.message.contains("secrets.vault"),
            "the rejection must name the exact offending section, got: {}",
            error.message
        );
    }

    // --- Folgeknoten zu AW2-01/AW2-02: `ContextProgram` je Sitzung ----------

    #[test]
    fn admit_binds_the_roles_declared_context_program_to_the_child_session() {
        // Der wichtigste Test dieses Knotens: eine Rolle, deren Agent-IR ein
        // `[context]`-Programm deklariert, muss dieses Programm tatsächlich
        // auf ihrer Sitzung tragen — nicht bloß eine IR mit einem gefüllten
        // Feld haben, das nirgends ankommt.
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
        let parent = SessionId::new();
        let mut parent_context = external_root_context(
            sandbox.clone(),
            harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
        );
        parent_context.ceiling = Some(test_ceiling(
            &["history.tail", "plan.current"],
            TrustClass::Evidence,
            750,
        ));
        let ir = test_agent_ir(
            r#"
[context]
must_include = ["history.tail"]
"#,
        );
        let spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative())
            .with_role(
                "worker",
                AgentRole::Agent {
                    name: "worker".to_owned(),
                },
                harw_agent_dsl::roles::AgentRoleId::Worker,
                Arc::new(IrChildRegistry { ir: ir.clone() }),
            )
            .with_external_root_parent(
                parent.clone(),
                parent_context,
                None,
                SessionActivation::default(),
            )
            .expect("trusted external root registers during construction");

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .expect("a program that fits within the cut ceiling is admissible");

        let manager = spawner.manager.lock().expect("test session manager lock");
        let session = manager.get(&child).expect("child is manager-owned");
        assert_eq!(
            session.context_program(),
            Some(ir.context_program()),
            "the child session must carry exactly the program its role declared"
        );
    }

    #[test]
    fn admit_leaves_context_program_none_for_a_role_without_one() {
        // Eine Rolle ohne `[context]`-Tabelle verhält sich exakt unverändert:
        // `context_program()` bleibt `None`, egal ob die IR sonst Politik
        // trägt (hier: Tool-Surface).
        let (spawner, parent, sandbox) = ir_spawner(test_agent_ir(
            r#"
[tools]
admitted = ["fs.read"]
"#,
        ));

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .expect("child without a declared context program is still admitted");

        let manager = spawner.manager.lock().expect("test session manager lock");
        let session = manager.get(&child).expect("child is manager-owned");
        assert!(
            session.context_program().is_none(),
            "a role without a declared [context] table must not gain one along the way"
        );
    }

    #[test]
    fn admit_rejects_a_declared_context_program_that_widens_the_cut_ceiling() {
        // Ein Programm darf die Decke nie erweitern: hier verlangt es
        // `secrets.vault`, das die (bereits geschnittene) Kind-Decke nicht
        // enthält.
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
        let parent = SessionId::new();
        let mut parent_context = external_root_context(
            sandbox.clone(),
            harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
        );
        parent_context.ceiling = Some(test_ceiling(&["history.tail"], TrustClass::Evidence, 500));
        let ir = test_agent_ir(
            r#"
[context]
must_include = ["secrets.vault"]
"#,
        );
        let spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative())
            .with_role(
                "worker",
                AgentRole::Agent {
                    name: "worker".to_owned(),
                },
                harw_agent_dsl::roles::AgentRoleId::Worker,
                Arc::new(IrChildRegistry { ir }),
            )
            .with_external_root_parent(
                parent.clone(),
                parent_context,
                None,
                SessionActivation::default(),
            )
            .expect("trusted external root registers during construction");

        let active_before = spawner.active.lock().expect("active lock").len();

        let error = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .expect_err("a declared program must never widen the child's cut context ceiling");

        assert!(
            error.message.contains("secrets.vault"),
            "the rejection must name the exact offending section, got: {}",
            error.message
        );

        // Belegt, dass kein Kind mit dem überzogenen Programm entstanden ist —
        // die Ablehnung darf keine halbfertige Sitzung hinterlassen.
        let active_after = spawner.active.lock().expect("active lock").len();
        assert_eq!(
            active_before, active_after,
            "a rejected context-program escalation must not leave a partially admitted child behind"
        );
    }

    #[test]
    fn cut_ceiling_is_idempotent() {
        let parent = test_ceiling(
            &["history.tail", "plan.current"],
            TrustClass::Instruction,
            1_000,
        );
        let requested = test_ceiling(&["history.tail"], TrustClass::Evidence, 400);

        let once = ManagedAgentSpawner::cut_ceiling(Some(&parent), Some(&requested))
            .expect("the requested ceiling fits within the parent's");
        let twice = ManagedAgentSpawner::cut_ceiling(Some(&once), Some(&once))
            .expect("a ceiling already cut against itself must still fit");

        assert_eq!(
            once, twice,
            "cutting an already-cut ceiling again must be a no-op"
        );
    }

    #[test]
    fn admit_grandchild_ceiling_never_exceeds_the_roots_ceiling() {
        // Same manager-owned root/manager-child/worker-grandchild shape as
        // `admit_propagates_the_root_trace_id_across_a_grandchild` above —
        // the only two-hop path the §3 spawn matrix allows.
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events.clone())));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
        let root_ceiling = test_ceiling(
            &["history.tail", "plan.current"],
            TrustClass::Instruction,
            1_000,
        );
        let root_context = SpawnContext {
            sandbox: sandbox.clone(),
            suggestions: None,
            capability_snapshot: None,
            approval_actor: None,
            organizational_role: harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
            trace: None,
            ceiling: Some(root_ceiling.clone()),
        };
        let root_session = AgentSession::new(
            AgentRole::Agent {
                name: "root".to_owned(),
            },
            None,
            ExtensionRegistryBuilder::default().build(),
            events,
        )
        .with_spawn_context(root_context);
        let root = root_session.id().clone();
        manager
            .lock()
            .expect("test session manager lock")
            .restore(root_session)
            .expect("root session restores");

        let spawner = ManagedAgentSpawner::new(Arc::clone(&manager), ChildLimits::conservative())
            .with_role(
                "manager",
                AgentRole::Agent {
                    name: "manager".to_owned(),
                },
                harw_agent_dsl::roles::AgentRoleId::ChildOrchestrator,
                Arc::new(EmptyChildRegistry),
            )
            .with_role(
                "worker",
                AgentRole::Agent {
                    name: "worker".to_owned(),
                },
                harw_agent_dsl::roles::AgentRoleId::Worker,
                Arc::new(EmptyChildRegistry),
            );

        let child_request = test_ceiling(&["history.tail"], TrustClass::Evidence, 400);
        let child = spawner
            .admit(
                "manager",
                spawn_input_requesting(root, child_request),
                sandbox.clone(),
                None,
            )
            .expect("child is admitted with a narrower ceiling");

        let grandchild_request = test_ceiling(&["history.tail"], TrustClass::Data, 100);
        let grandchild = spawner
            .admit(
                "worker",
                spawn_input_requesting(child, grandchild_request.clone()),
                sandbox,
                None,
            )
            .expect("grandchild is admitted with a narrower ceiling still");

        let manager = manager.lock().expect("test session manager lock");
        let grandchild_ceiling = manager
            .get(&grandchild)
            .expect("grandchild is manager-owned")
            .spawn_context()
            .expect("grandchild has a trusted spawn context")
            .ceiling
            .clone()
            .expect("grandchild inherits a ceiling across two admission hops");

        assert_eq!(
            grandchild_ceiling, grandchild_request,
            "the grandchild's own narrower request must be exactly what it ends up with"
        );
        assert!(
            grandchild_ceiling.sections.is_subset(&root_ceiling.sections),
            "the grandchild must never see a section the root ceiling didn't already carry"
        );
        assert!(
            grandchild_ceiling.max_trust.trust_rank() <= root_ceiling.max_trust.trust_rank(),
            "the grandchild's max_trust must never exceed the root's, two hops down"
        );
        assert!(
            grandchild_ceiling.budget.total.total <= root_ceiling.budget.total.total,
            "the grandchild's budget must never exceed the root's, two hops down"
        );
        assert_ne!(
            grandchild_ceiling, root_ceiling,
            "this test is only meaningful if real narrowing happened along the way"
        );
    }

    #[test]
    fn admit_cuts_the_ceiling_alongside_the_sandbox_in_one_admission() {
        // Proves the "same step" promise directly: one successful admission,
        // both the sandbox and the ceiling checked from its one result.
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let parent_sandbox = test_sandbox(PermissionSet::from_policy([
            Permission::ReadWorkspace,
            Permission::WriteWorkspace,
        ]));
        let child_sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
        let parent = SessionId::new();
        let mut parent_context = external_root_context(
            parent_sandbox.clone(),
            harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
        );
        parent_context.ceiling = Some(test_ceiling(
            &["history.tail", "plan.current"],
            TrustClass::Instruction,
            1_000,
        ));
        let requested_ceiling = test_ceiling(&["history.tail"], TrustClass::Evidence, 200);
        let spawner = worker_spawner(manager)
            .with_external_root_parent(
                parent.clone(),
                parent_context,
                None,
                SessionActivation::default(),
            )
            .expect("trusted external root registers during construction");

        let child = spawner
            .admit(
                "worker",
                spawn_input_requesting(parent, requested_ceiling.clone()),
                child_sandbox,
                None,
            )
            .expect("a narrower sandbox and a narrower ceiling are both admissible together");

        let manager = spawner.manager.lock().expect("test session manager lock");
        let child_context = manager
            .get(&child)
            .expect("child is manager-owned")
            .spawn_context()
            .expect("child has a trusted spawn context");

        assert!(
            child_context.sandbox.ensure_child_of(&parent_sandbox).is_ok(),
            "the child's sandbox must be a valid narrowing of the parent's"
        );
        assert_eq!(
            child_context.ceiling,
            Some(requested_ceiling),
            "the child's ceiling must be cut in the very same admission that narrows the sandbox"
        );
    }

    #[test]
    fn admit_rejecting_a_ceiling_escalation_leaves_no_partial_child_behind() {
        // The sandbox check above the ceiling cut already passed by the time
        // the ceiling is checked; this proves that a ceiling rejection still
        // aborts the whole admission — no observable intermediate state.
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
        let parent = SessionId::new();
        let mut parent_context = external_root_context(
            sandbox.clone(),
            harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
        );
        parent_context.ceiling = Some(test_ceiling(&["history.tail"], TrustClass::Data, 100));
        let spawner = worker_spawner(manager)
            .with_external_root_parent(
                parent.clone(),
                parent_context,
                None,
                SessionActivation::default(),
            )
            .expect("trusted external root registers during construction");

        let active_before = spawner.active.lock().expect("active lock").len();

        let escalating_request = test_ceiling(&["history.tail"], TrustClass::Instruction, 100);
        let result = spawner.admit(
            "worker",
            spawn_input_requesting(parent, escalating_request),
            sandbox,
            None,
        );

        assert!(
            result.is_err(),
            "a wider requested ceiling must be rejected outright"
        );
        let active_after = spawner.active.lock().expect("active lock").len();
        assert_eq!(
            active_before, active_after,
            "a rejected ceiling escalation must not leave a partially admitted child behind"
        );
    }

    /// AW2-02 Attacker-Fixture: eine Kinddefinition, die eine höhere
    /// Vertrauensklasse verlangt als ihr Elternteil zulässt.
    ///
    /// Identisch mit der real hinterlegten Fixture
    /// `harw-extension-api/tests/fixtures/context_escalation_attacker.toml`,
    /// die derselben Form wie
    /// `harw-agent-dsl/tests/fixtures/authority_elevation_attacker.toml`
    /// folgt (Schema-Kopf, `id`, `role`, `specialization`), ergänzt um eine
    /// `[context_ceiling]`-Sektion. `harw-core` hat (Stand dieses Knotens)
    /// noch keinen Pfad, der eine Kind-Deckenforderung aus einer
    /// Agent-Definition parst — `SpawnInput::ceiling` wird bislang
    /// ausschließlich in Rust gesetzt (Model-Handoff/`harw-cli`/`harw-tui`,
    /// außerhalb dieses Knotens). Der Test unten baut deshalb exakt die
    /// `ContextCeiling` nach, die diese TOML deklariert, statt einen neuen,
    /// ungeprüften Parser-Pfad einzuführen — die geprüfte Zusicherung ("eine
    /// weitere Decke wird abgewiesen") ist dieselbe, ob die Forderung aus
    /// TOML oder aus Rust kommt.
    const CONTEXT_ESCALATION_ATTACKER_TOML: &str = r#"
schema = "harwness.agent/v1"
id = "harwness.agent.context-escalation-attacker@1"
version = "1.0.0"
role = "worker"
specialization = "context-escalation-attacker"

[context_ceiling]
sections = ["history.tail"]
max_trust = "instruction"
"#;

    #[test]
    fn context_escalation_attacker_fixture_is_rejected() {
        assert!(
            CONTEXT_ESCALATION_ATTACKER_TOML.contains("max_trust = \"instruction\""),
            "fixture drifted from what this test actually exercises"
        );

        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
        let parent = SessionId::new();
        let mut parent_context = external_root_context(
            sandbox.clone(),
            harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
        );
        parent_context.ceiling = Some(test_ceiling(&["history.tail"], TrustClass::Evidence, 500));
        let spawner = worker_spawner(manager)
            .with_external_root_parent(
                parent.clone(),
                parent_context,
                None,
                SessionActivation::default(),
            )
            .expect("trusted external root registers during construction");

        // Die von der Fixture beschriebene Forderung, in Rust nachgebaut
        // (siehe Doc-Kommentar oben): dieselben Sektionen, aber
        // `max_trust = "instruction"` statt der vom Elternteil erlaubten
        // `Evidence`-Obergrenze.
        let attacker_ceiling = test_ceiling(&["history.tail"], TrustClass::Instruction, 500);

        let error = spawner
            .admit(
                "worker",
                spawn_input_requesting(parent, attacker_ceiling),
                sandbox,
                None,
            )
            .expect_err(
                "a child must never be admitted with a higher max_trust than its parent's ceiling",
            );

        assert!(
            error.message.contains("max_trust"),
            "the rejection must name the violated aspect (max_trust), got: {}",
            error.message
        );
    }

    #[test]
    fn durable_lease_carries_the_inherited_trace_into_the_child_lease_record() {
        let trace = test_trace(&"e".repeat(32), &"f".repeat(16));
        let now = Timestamp::now();
        let record = ChildRecord {
            child: SessionId::new(),
            parent: SessionId::new(),
            handoff_call_id: ToolCallId::new(),
            role: "worker".to_owned(),
            depth: 1,
            admitted_at: now,
            lease_expires_at: now,
            budget: AgentBudget::default(),
            allow_pause: false,
            depth_ceiling: ChildLimits::conservative().max_depth,
            trace: Some(trace.clone()),
        };

        let lease = record.durable_lease();

        assert_eq!(
            lease.trace,
            Some(trace),
            "durable_lease must carry the record's inherited trace, not drop it"
        );
    }

    #[test]
    fn admit_with_a_lease_store_persists_the_inherited_trace_to_disk() {
        let temporary_directory = tempfile::tempdir().expect("temporary lease directory");
        let lease_store = Arc::new(ChildLeaseStore::new(temporary_directory.path()));
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
        let parent = SessionId::new();
        let parent_trace = test_trace(&"1".repeat(32), &"2".repeat(16));
        let mut parent_context = external_root_context(
            sandbox.clone(),
            harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
        );
        parent_context.trace = Some(parent_trace.clone());
        let spawner = worker_spawner(manager)
            .with_lease_store(lease_store.clone())
            .with_external_root_parent(
                parent.clone(),
                parent_context,
                None,
                SessionActivation::default(),
            )
            .expect("trusted external root registers during construction");

        spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .expect("child is admitted and its lease is durably recorded");

        let active_leases = lease_store.active().expect("active leases are readable");
        assert_eq!(active_leases.len(), 1);
        let persisted_trace = active_leases[0]
            .trace
            .clone()
            .expect("the durable lease on disk carries the inherited trace");
        assert_eq!(persisted_trace.trace_id, parent_trace.trace_id);
        assert_eq!(
            persisted_trace.parent_span_id.as_deref(),
            Some(parent_trace.span_id.as_str())
        );
    }

    // -----------------------------------------------------------------------
    // F-017/E3b: Schnitt der Kind-Aktivierung mit der Eltern-Aktivierung
    // (Befund E1) und Effort-Clamp ohne geerbte Basis (Befund E3b)
    // -----------------------------------------------------------------------

    /// Wie [`ir_spawner`], aber mit einer frei wählbaren Aktivierung der
    /// extern registrierten Wurzel.
    fn ir_spawner_with_parent_activation(
        ir: ExecutableAgentIr,
        parent_activation: SessionActivation,
    ) -> (ManagedAgentSpawner, SessionId, SandboxSpec) {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
        let parent = SessionId::new();
        let spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative())
            .with_role(
                "worker",
                AgentRole::Agent {
                    name: "worker".to_owned(),
                },
                harw_agent_dsl::roles::AgentRoleId::Worker,
                Arc::new(IrChildRegistry { ir }),
            )
            .with_external_root_parent(
                parent.clone(),
                external_root_context(
                    sandbox.clone(),
                    harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
                ),
                None,
                parent_activation,
            )
            .expect("trusted external root registers during construction");
        (spawner, parent, sandbox)
    }

    /// Die Tool-Oberfläche einer Rolle, die `fs.read` **und** `shell.exec`
    /// zulässt — genug, um einen fehlenden Schnitt sichtbar zu machen.
    fn read_and_exec_ir() -> ExecutableAgentIr {
        test_agent_ir(
            r#"
[tools]
admitted = ["fs.read", "shell.exec"]
"#,
        )
    }

    #[test]
    fn child_activation_is_cut_with_the_parent_activation() {
        let mut parent_activation = SessionActivation::new(ToolProfile::Full);
        parent_activation.disable_tool(ToolName::new("shell.exec"));
        let (spawner, parent, sandbox) =
            ir_spawner_with_parent_activation(read_and_exec_ir(), parent_activation);

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .expect("the child is admitted");

        let manager = spawner.manager.lock().expect("test session manager lock");
        let activation = manager
            .get(&child)
            .expect("child is manager-owned")
            .activation();
        assert!(
            activation.is_tool_enabled(&ToolName::new("fs.read")),
            "ein vom Elternteil erlaubtes und von der Rolle zugelassenes Werkzeug bleibt offen"
        );
        assert!(
            !activation.is_tool_enabled(&ToolName::new("shell.exec")),
            "die Rolle lässt shell.exec zu, der Elternteil nicht — der Schnitt entscheidet"
        );
    }

    #[test]
    fn parent_activation_cut_survives_a_later_mode_switch() {
        // Der Schnitt ist eine Autoritätsgrenze, kein Laufzeit-Override: er
        // liegt in der Basis und muss deshalb jeden `set_mode` überleben.
        // `Work` ist der schärfste Fall — `ToolProfile::Full` ohne
        // Allowlist, also die weiteste Modus-Decke überhaupt.
        let mut parent_activation = SessionActivation::new(ToolProfile::Full);
        parent_activation.disable_tool(ToolName::new("shell.exec"));
        let (spawner, parent, sandbox) =
            ir_spawner_with_parent_activation(read_and_exec_ir(), parent_activation);

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .expect("the child is admitted");

        let mut manager = spawner.manager.lock().expect("test session manager lock");
        let child_session = manager.get_mut(&child).expect("child is manager-owned");
        child_session.set_mode(crate::mode::InteractionMode::Work);
        assert!(
            !child_session
                .activation()
                .is_tool_enabled(&ToolName::new("shell.exec")),
            "das Verbot des Elternteils überlebt den Moduswechsel"
        );
        assert!(
            !child_session
                .base_activation()
                .is_tool_enabled(&ToolName::new("shell.exec")),
            "der Schnitt sitzt in der Basis, nicht nur im abgeleiteten Wert"
        );
        assert!(
            child_session
                .activation()
                .is_tool_enabled(&ToolName::new("fs.read")),
            "was Elternteil und Rolle erlauben, bleibt auch in Work offen"
        );
    }

    #[test]
    fn grandchild_activation_inherits_the_whole_intersection_chain() {
        // Die Wurzel ist hier bewusst **manager-eigen**: nur so ist sie für
        // `parent_depth` auflösbar, und nur so belegt der Test die zweite
        // Bezugsquelle des Schnitts — bei einem Kind eines Kindes ist der
        // Elternteil die Sitzung im Manager, deren Aktivierung `admit` über
        // `AgentSession::activation()` liest.
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events.clone())));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));

        let mut root_activation = SessionActivation::new(ToolProfile::Full);
        root_activation.disable_tool(ToolName::new("shell.exec"));
        let root_session = AgentSession::new(
            AgentRole::Agent {
                name: "root".to_owned(),
            },
            None,
            ExtensionRegistryBuilder::default().build(),
            events,
        )
        .with_spawn_context(external_root_context(
            sandbox.clone(),
            harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
        ))
        .with_activation(root_activation);
        let root = root_session.id().clone();
        manager
            .lock()
            .expect("test session manager lock")
            .restore(root_session)
            .expect("test root session restores");

        let spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative())
            .with_role(
                "middle",
                AgentRole::Agent {
                    name: "middle".to_owned(),
                },
                harw_agent_dsl::roles::AgentRoleId::ChildOrchestrator,
                Arc::new(IrChildRegistry {
                    ir: read_and_exec_ir(),
                }),
            )
            .with_role(
                "worker",
                AgentRole::Agent {
                    name: "worker".to_owned(),
                },
                harw_agent_dsl::roles::AgentRoleId::Worker,
                Arc::new(IrChildRegistry {
                    ir: read_and_exec_ir(),
                }),
            );

        let middle = spawner
            .admit("middle", spawn_input(root), sandbox.clone(), None)
            .expect("the child orchestrator is admitted");
        let grandchild = spawner
            .admit("worker", spawn_input(middle.clone()), sandbox, None)
            .expect("the grandchild is admitted below the child orchestrator");

        let manager = spawner.manager.lock().expect("test session manager lock");
        for (label, session_id) in [("Kind", &middle), ("Enkel", &grandchild)] {
            let activation = manager
                .get(session_id)
                .expect("session is manager-owned")
                .activation();
            assert!(
                activation.is_tool_enabled(&ToolName::new("fs.read")),
                "{label}: fs.read ist auf jeder Ebene erlaubt"
            );
            assert!(
                !activation.is_tool_enabled(&ToolName::new("shell.exec")),
                "{label}: das Verbot der Wurzel wirkt transitiv nach unten"
            );
        }
    }

    #[test]
    fn parent_activation_cut_also_applies_without_an_agent_ir() {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
        let parent = SessionId::new();
        // Ohne IR bleibt die Kind-Aktivierung sonst auf `ToolProfile::Full`
        // stehen (Befund E1) — auch dieser Pfad muss geschnitten werden.
        let mut parent_activation = SessionActivation::new(ToolProfile::Full);
        parent_activation.disable_tool(ToolName::new("shell.exec"));
        parent_activation.disable_context("workspace_files");
        let spawner = worker_spawner(manager)
            .with_external_root_parent(
                parent.clone(),
                external_root_context(
                    sandbox.clone(),
                    harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
                ),
                None,
                parent_activation,
            )
            .expect("trusted external root registers during construction");

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .expect("the child is admitted");

        let manager = spawner.manager.lock().expect("test session manager lock");
        let activation = manager
            .get(&child)
            .expect("child is manager-owned")
            .activation();
        assert!(!activation.is_tool_enabled(&ToolName::new("shell.exec")));
        assert!(!activation.is_context_enabled("workspace_files"));
        assert!(activation.is_tool_enabled(&ToolName::new("fs.read")));
    }

    #[test]
    fn effort_clamp_without_an_inherited_base_falls_back_to_the_default() {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]));
        let parent = SessionId::new();
        let spawner = worker_spawner(manager)
            .with_external_root_parent(
                parent.clone(),
                external_root_context(
                    sandbox.clone(),
                    harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
                ),
                // Genau die Produktionslage aus Befund E3(b): die Wurzel führt
                // kein Effort-Level.
                None,
                SessionActivation::default(),
            )
            .expect("trusted external root registers during construction");

        let capped = spawner
            .admit("worker", spawn_input(parent.clone()), sandbox.clone(), None)
            .expect("first child is admitted");
        assert_eq!(
            spawner
                .manager
                .lock()
                .expect("test session manager lock")
                .get(&capped)
                .expect("child is manager-owned")
                .reasoning_effort(),
            None,
            "ohne Eltern-Level erbt das Kind bei der Admission nichts"
        );
        assert_eq!(
            spawner
                .clamp_child_reasoning_effort(&capped, Some(ReasoningEffort::Low), None)
                .expect("known child clamps cleanly"),
            Some(ReasoningEffort::Low),
            "der Deckel der Agent-IR greift jetzt auch ohne geerbte Basis"
        );

        let uncapped = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .expect("second child is admitted");
        assert_eq!(
            spawner
                .clamp_child_reasoning_effort(&uncapped, None, None)
                .expect("known child clamps cleanly"),
            Some(DEFAULT_CHILD_REASONING_EFFORT),
            "ohne Deckel und ohne Basis gilt der Default, nicht der Provider-Default"
        );
    }

    #[test]
    fn effort_clamp_owner_override_still_beats_the_default_base() {
        let (spawner, child) = spawner_with_admitted_child();

        let effective = spawner
            .clamp_child_reasoning_effort(
                &child,
                Some(ReasoningEffort::Minimal),
                Some(ReasoningEffort::High),
            )
            .expect("known child clamps cleanly");

        assert_eq!(effective, Some(ReasoningEffort::High));
    }
}
