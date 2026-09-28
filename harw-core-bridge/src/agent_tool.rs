//! `AgentToolAdapter` — exponiert eine [`Operation`] als Aufruf eines Child-Agenten.
//!
//! # Verantwortungsbereich
//! Dieser Adapter übersetzt eine `Surface::AgentTool`-Deklaration einer Operation
//! in eine Laufzeit-Instanz, die dem Parent-LLM ermöglicht, einen Child-Agent wie
//! ein gewöhnliches Tool aufzurufen. Das Aufruf-Muster folgt dem "Agent-as-Tool"-Paradigma
//! aus der OpenAI Agents SDK-Referenz (Wave-4-Design-Doc, Abschnitt 3).
//!
//! # Abgrenzung gegenüber `ModelToolAdapter`
//! | Eigenschaft          | `ModelToolAdapter`              | `AgentToolAdapter`                       |
//! |----------------------|---------------------------------|------------------------------------------|
//! | Ausführungseinheit   | Operation (synchron/async)      | Child-Agent (eigenständiger Loop)        |
//! | Kapselung            | Direkte Delegation via `run`    | Black-Box Child (eigene Turns)           |
//! | Authority-Kontrolle  | keine (gleiche Sandbox)         | Monoton fallend via Reducer              |
//! | Budget               | n/a                             | typisiert via [`parse_budget_hint`]      |
//! | Rückgabe             | Freitext der Operation          | [`ChildReturnContract`]-validiertes JSON |
//! | Spawn-Semantik       | n/a                             | `ManagedAgentSpawner`                    |
//!
//! # W3-10-Implementierung
//! [`AgentToolAdapter::invoke`] löst den registrierten `ManagedAgentSpawner`
//! und `StateStore` aus dem [`OpContext`] auf, reduziert die Parent-Sandbox
//! monoton über die `authority_reducer`-Kennung (siehe
//! [`resolve_authority_reducer`]), spawnt den Child-Agenten und treibt dessen
//! Turn unter einem **typisierten Budget** bis zum Abschluss
//! (`ManagedAgentSpawner::run_child_with_budget`). Die Antwort des Kindes wird
//! nicht mehr blind als Freitext durchgereicht, sondern gegen den
//! [`ChildReturnContract`] seiner Agent-IR validiert.
//!
//! Vier Grenzen dieser Welle, jeweils bewusst gesetzt:
//! 1. **Budget** — ein unlesbares `budget_hint` macht das Werkzeug
//!    *nicht verfügbar*; es fällt nie still auf „kein Limit" zurück.
//! 2. **Return-Contract** — ein Kind, das den Vertrag bricht, erzeugt einen
//!    strukturierten Fehler-*Output*, keinen `Err`: das ist ein Datenfehler des
//!    Kindes, kein Laufzeitfehler des Werkzeugs (siehe
//!    [`ChildReturnContract`]).
//! 3. **Pause** — `AwaitingChild`/`AwaitingApproval` sind nur zulässig, wenn der
//!    [`ChildRecord`](harw_core::child_controller::ChildRecord) des Kindes
//!    `allow_pause = true` trägt; sonst fail-closed mit präziser Meldung.
//! 4. **Lease** (W4a/A-BRIDGE, G-016) — [`AgentToolAdapter::invoke`] und
//!    [`fanout_children`] liefern das Kind-Ergebnis selbst an den Aufrufer aus
//!    (Rückgabewert); eine andere Orchestrierungs-Laufzeit, die Kinder schließt,
//!    gibt es nicht. Deshalb gibt ein RAII-Guard (`ChildSlotGuard`) den
//!    Admission-Slot frei, **nachdem** das Ergebnis vollständig ausgewertet
//!    (Text kopiert, Contract geprüft) ist — und ebenso bei jedem Fehler,
//!    Vertragsbruch, Budget-Abbruch oder wenn der aufrufende Future verworfen
//!    wird. Einzige Ausnahme: eine *zulässige* Pause (`allow_pause = true`)
//!    hält den Slot, weil die Fortsetzung beim Aufrufer liegt.
//!
//! # Grundinvariante — Authority-Monotonie
//! Gemäß Design-Doc Abschnitt 2: Ein Child-Agent darf **niemals** mehr Authority
//! besitzen als der Parent-Call, der ihn erzeugt. Die `authority_reducer`-Kennung
//! referenziert eine monoton fallende Funktion; dieselbe Monotonie gilt für das
//! Budget (siehe [`tighten_budget`]) und für den Reasoning-Effort
//! (`clamp_child_reasoning_effort`). Einen Effort-Override aus Modell-Argumenten
//! gibt es nicht (W4a/A-BRIDGE, G-084): Tool-Argumente sind Modell-Output und
//! damit kein Owner-Nachweis; `effort`/`reasoning_effort` in den Argumenten wird
//! abgelehnt. Der Effort stammt allein aus Parent-Erbe, Rolle (Agent-IR) und
//! deklariertem Budget.
//!
//! # Kardinalität
//! Eine Operation deklariert **maximal eine** `Surface::AgentTool`-Fläche.
//! [`AgentToolAdapter::from_operation`] gibt `Option<Self>` zurück und verwendet die
//! **erste** gefundene Fläche, falls defensiv mehrere deklariert wurden.
//!
//! # Schlüsseltypen
//! - [`AgentToolAdapter`] — der Adapter selbst
//! - [`AgentProductAdapter`] — die `/agent`-Produktfläche (list/stop/budget)
//! - [`ChildReturnContract`] — wie das Kind-Ergebnis interpretiert wird
//! - [`parse_budget_hint`] / [`tighten_budget`] — Budget-Typisierung und -Verschärfung
//! - [`fanout_children`] — Fan-out-Einstiegspunkt für Ops (`/analyze`, Explore)
//!
//! # Nebenläufigkeit
//! `AgentToolAdapter` ist `Send + Sync`: enthält `Arc<dyn Operation>` (selbst
//! `Send + Sync`) und ausschließlich `&'static str`-Felder ohne inneren Zustand.
//!
//! # Fehlertypen
//! - [`harw_operations::error::OpError::InvalidArguments`]: Argumente sind kein
//!   JSON-Objekt, tragen ein Effort-Feld (`effort`/`reasoning_effort`), oder ein
//!   Budget-Label ist syntaktisch ungültig.
//! - [`harw_operations::error::OpError::NotAvailable`]: Kein
//!   `ManagedAgentSpawner`/`StateStore` im Kontext registriert, die Deklaration
//!   trägt ein unlesbares `budget_hint`, der Spawn/Lauf/Effort-Clamp schlug fehl,
//!   oder das Kind pausierte ohne Pause-Erlaubnis.
//!
//! # Spec-Quelle
//! `docs/design/agents-as-tools.md`, `agent-definition-dsl.md` §13,
//! `coding-philosophy.md` §4.
//!
//! # Beispiel
//! ```rust,no_run
//! use std::sync::Arc;
//! use harw_core_bridge::AgentToolAdapter;
//! use harw_operations::operation::{
//!     OpFuture, OpInput, OpOutput, Operation, OperationDomain,
//!     OperationMeta, PermissionTier, Surface,
//! };
//! use harw_operations::context::OpContext;
//!
//! struct SpawnResearcher;
//! impl Operation for SpawnResearcher {
//!     fn meta(&self) -> &OperationMeta {
//!         static M: std::sync::OnceLock<OperationMeta> = std::sync::OnceLock::new();
//!         M.get_or_init(|| OperationMeta {
//!             name: "spawn_researcher",
//!             summary: "Delegiert an Sub-Agent 'researcher'.",
//!             domain: OperationDomain::Agents,
//!             permission: PermissionTier::Operator,
//!             surfaces: vec![Surface::AgentTool {
//!                 child_name: "researcher",
//!                 authority_reducer: "reduce_to_read_only",
//!                 budget_hint: "8k_tokens,20_tool_calls,30s",
//!             }],
//!             aliases: &[],
//!             category: harw_operations::OperationCategory::Agent,
//!             args_schema: None,
//!             output_schema: None,
//!             busy: Default::default(),
//!         })
//!     }
//!     fn run<'a>(&'a self, _c: &'a OpContext, _i: OpInput) -> OpFuture<'a> {
//!         Box::pin(async { Ok(OpOutput { text: String::new(), data: None }) })
//!     }
//! }
//!
//! let adapter = AgentToolAdapter::from_operation(Arc::new(SpawnResearcher));
//! assert!(adapter.is_some());
//! assert_eq!(adapter.unwrap().child_name(), "researcher");
//! ```

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::Poll;
use std::time::{Duration, Instant};

use harw_authority::{Permission, PermissionRequest, PermissionSet, SandboxSpec};
use harw_core::cancel::CancelToken;
use harw_core::child_controller::{
    AgentBudget, CHILD_RETURN_MAX_BYTES, ChildRegistryFactory, ChildRunResult, JoinSemantics,
    ManagedAgentSpawner, cap_child_return_text_for_child,
};
// Runde 5, Teil J: Übergabe-Verdichtung und Fortsetzung am Budget-Ende.
use harw_core::child_handoff::{BudgetHandoff, ContinuationSeed};
use harw_core::turn_loop::{TurnInput, TurnOutcome};
use harw_core::{ModelMessage, StateStore};
use harw_types::{ReasoningEffort, SessionId};
use serde_json::{Value, json};

use harw_operations::context::OpContext;
use harw_operations::error::OpError;
use harw_operations::operation::{OpOutput, Operation, Surface};

use crate::context_ext::OpContextCoreExt;
use crate::return_validators::run_return_validators;

// ── Adapter ───────────────────────────────────────────────────────────────────

/// Adapter, der eine [`Operation`] als Aufruf eines Child-Agenten exponiert.
///
/// # Beschreibung
/// `AgentToolAdapter` kapselt die `Surface::AgentTool`-Metadaten einer Operation.
/// Er stellt eine [`invoke`][AgentToolAdapter::invoke]-Methode bereit, die einen
/// Child-Agent über den im [`OpContext`] registrierten `ManagedAgentSpawner`
/// spawnt, ihn unter einem typisierten Budget fährt und sein Ergebnis gegen den
/// [`ChildReturnContract`] seiner Agent-IR prüft.
///
/// Sind `ManagedAgentSpawner` oder `StateStore` im Kontext nicht registriert
/// (der Executor hat sie nicht bereitgestellt), gibt `invoke`
/// `Err(OpError::NotAvailable)` zurück.
///
/// Der Adapter wird ausschließlich über [`AgentToolAdapter::from_operation`] erzeugt.
///
/// # Nebenläufigkeit
/// `Send + Sync` — kann sicher hinter `Arc` über Thread-Grenzen geteilt werden.
///
/// # Spec-Quelle
/// `docs/design/agents-as-tools.md`, Abschnitt 4
pub struct AgentToolAdapter {
    op: Arc<dyn Operation>,
    child_name: &'static str,
    authority_reducer: &'static str,
    budget_hint: &'static str,
}

impl AgentToolAdapter {
    /// Dispatches the runtime-bound `/agent` product surface.
    ///
    /// This is deliberately separate from [`Self::invoke`]: an
    /// `AgentToolAdapter` invokes a model-declared child tool, while `/agent`
    /// is a parent-session control surface. The bridge owns it because the
    /// required runtime service is [`ManagedAgentSpawner`], which must not
    /// leak into `harw-ops`.
    ///
    /// Accepted requests are JSON objects with an `action` field:
    ///
    /// - `{"action":"list"}`
    /// - `{"action":"stop","target":"<child-session-id>"}`
    /// - `{"action":"budget","target":"<child-session-id>"}` — lesend
    /// - `{"action":"budget","target":"<child-session-id>","budget":{...}}` —
    ///   Setz-Versuch; wird typgeprüft, aber **nicht angewendet** (siehe
    ///   [`AgentProductAdapter::invoke`]).
    ///
    /// Alle drei Aktionen laufen über dieselben fail-closed Grenzprüfungen:
    /// `list` ist parent-gebunden, `stop`/`budget` verlangen ein Kind, das
    /// **dieser** Parent-Session gehört. Session-Historie und Sandbox-Details
    /// verlassen diese Grenze niemals.
    pub async fn invoke_product(
        ctx: &OpContext,
        args: serde_json::Value,
    ) -> Result<OpOutput, OpError> {
        AgentProductAdapter::invoke(ctx, args).await
    }

    /// Erstellt einen `AgentToolAdapter`, falls die Operation eine `Surface::AgentTool`-Fläche deklariert.
    ///
    /// # Beschreibung
    /// Durchsucht [`harw_operations::operation::OperationMeta::surfaces`] nach dem
    /// ersten `Surface::AgentTool`-Eintrag. Sind defensiv mehrere vorhanden, wird die
    /// **erste** verwendet — analog zu [`harw_operations::adapter::ModelToolAdapter`].
    ///
    /// # Argumente
    /// - `op` (`Arc<dyn Operation>`): Die zu adapterisierende Operation. Der `Arc`
    ///   wird intern gehalten; keine Clone der inneren Daten.
    ///
    /// # Returns
    /// - `Some(AgentToolAdapter)`: Operation hat mindestens eine `Surface::AgentTool`-Fläche.
    /// - `None`: Operation deklariert keine `Surface::AgentTool`-Fläche.
    ///
    /// # Errors
    /// Keine — diese Funktion schlägt nicht fehl. Insbesondere wird das
    /// `budget_hint` hier **nicht** geparst: eine kaputte Deklaration darf die
    /// Registrierung nicht verhindern, sondern muss beim Aufruf sichtbar werden
    /// (siehe [`Self::invoke`]).
    ///
    /// # Concurrency
    /// Sicher von mehreren Threads aufzurufen; ausschließlich lesender Zugriff auf
    /// `meta()` und atomares Inkrementieren des `Arc`-Referenzzählers.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use std::sync::Arc;
    /// use harw_core_bridge::AgentToolAdapter;
    /// // None bei Op ohne AgentTool-Surface:
    /// // let adapter = AgentToolAdapter::from_operation(Arc::new(NoSurfaceOp));
    /// // assert!(adapter.is_none());
    /// ```
    #[must_use]
    pub fn from_operation(op: Arc<dyn Operation>) -> Option<Self> {
        let meta = op.meta();
        let (child_name, authority_reducer, budget_hint) =
            meta.surfaces.iter().find_map(|s| match s {
                Surface::AgentTool {
                    child_name,
                    authority_reducer,
                    budget_hint,
                } => Some((*child_name, *authority_reducer, *budget_hint)),
                _ => None,
            })?;
        Some(Self {
            op,
            child_name,
            authority_reducer,
            budget_hint,
        })
    }

    /// Gibt den Namen des Child-Agents zurück, der gespawnt werden soll.
    ///
    /// # Beschreibung
    /// Liefert den `child_name`-String-Literal aus der `Surface::AgentTool`-Deklaration,
    /// z. B. `"researcher"`. Dieser Name ist gleichzeitig der Rollenname, unter dem
    /// der `ManagedAgentSpawner` die Registry-Factory und (über sie) die Agent-IR
    /// des Kindes auflöst.
    ///
    /// # Returns
    /// `&str` — statisches String-Literal. Lebensdauer ist an `'static` gebunden.
    ///
    /// # Concurrency
    /// Lesend, lock-frei.
    ///
    /// # Examples
    /// ```rust,no_run
    /// // assert_eq!(adapter.child_name(), "researcher");
    /// ```
    #[must_use]
    pub fn child_name(&self) -> &'static str {
        self.child_name
    }

    /// Gibt den Namen der Authority-Reducer-Funktion zurück.
    ///
    /// # Beschreibung
    /// Liefert die `authority_reducer`-Kennung aus `Surface::AgentTool`. Diese Kennung
    /// referenziert eine in der Extension-Registry registrierte, monoton fallende Funktion
    /// `fn(&SandboxSpec) -> SandboxSpec`, die die Parent-Sandbox auf eine Teilmenge für
    /// den Child reduziert (Wave-4-Design-Doc, Abschnitt 2).
    ///
    /// # Returns
    /// `&str` — statisches String-Literal.
    ///
    /// # Concurrency
    /// Lesend, lock-frei.
    ///
    /// # Examples
    /// ```rust,no_run
    /// // assert_eq!(adapter.authority_reducer(), "reduce_to_read_only");
    /// ```
    #[must_use]
    pub fn authority_reducer(&self) -> &'static str {
        self.authority_reducer
    }

    /// Gibt das Budget-Label für den Child-Agent-Spawn zurück.
    ///
    /// # Beschreibung
    /// Liefert das rohe `budget_hint`-Label aus `Surface::AgentTool`, z. B.
    /// `"8k_tokens,20_tool_calls,30s"` oder `"unlimited"`. Der Accessor gibt den
    /// **unveränderten** Deklarationstext zurück; die typisierte Form liefert
    /// [`parse_budget_hint`].
    ///
    /// # Returns
    /// `&str` — statisches String-Literal.
    ///
    /// # Concurrency
    /// Lesend, lock-frei.
    ///
    /// # Examples
    /// ```rust,no_run
    /// // assert_eq!(adapter.budget_hint(), "8k_tokens");
    /// ```
    #[must_use]
    pub fn budget_hint(&self) -> &'static str {
        self.budget_hint
    }

    /// Gibt eine Referenz auf die zugrundeliegende Operation zurück.
    ///
    /// # Beschreibung
    /// Ermöglicht Introspektion der Operation — z. B. für Registrierung, Logging oder
    /// Berechtigungsprüfungen — ohne den Adapter zu konsumieren. Der `Arc` wird nicht
    /// geklont; der Aufrufer erhält eine Referenz auf den intern gespeicherten `Arc`.
    ///
    /// # Returns
    /// `&Arc<dyn Operation>` — Referenz mit der Lebensdauer von `&self`.
    ///
    /// # Concurrency
    /// Lesend, lock-frei.
    ///
    /// # Examples
    /// ```rust,no_run
    /// // let name = adapter.operation().meta().name;
    /// ```
    #[must_use]
    pub fn operation(&self) -> &Arc<dyn Operation> {
        &self.op
    }

    /// Ruft den Child-Agent auf und wartet auf sein vertragskonformes Ergebnis.
    ///
    /// # Beschreibung
    /// Ablauf in dieser Reihenfolge — jede Stufe ist eine Grenze, keine
    /// Bequemlichkeit:
    /// 1. `args` muss ein JSON-Objekt sein und darf kein Effort-Feld
    ///    (`effort`/`reasoning_effort`) tragen, sonst `InvalidArguments`.
    /// 2. `ManagedAgentSpawner` und `StateStore` werden aus dem [`OpContext`]
    ///    aufgelöst; fehlen sie, ist das Werkzeug nicht verfügbar.
    /// 3. `budget_hint` wird über [`parse_budget_hint`] typisiert. Ein
    ///    unlesbares Label macht das Werkzeug **nicht verfügbar** — es fällt
    ///    nicht still auf „kein Limit" zurück, weil ein Tippfehler in der
    ///    Deklaration sonst zu *mehr* Freiheit für das Kind führen würde.
    /// 4. Die Parent-Sandbox wird monoton fallend reduziert
    ///    ([`resolve_authority_reducer`]) und das Kind gespawnt.
    /// 5. Das deklarierte Budget wird mit dem IR-Budget des Kindes verschnitten;
    ///    je Dimension gewinnt die **strengere** Grenze ([`tighten_budget`]).
    /// 6. Der Reasoning-Effort wird auf `budget.reasoning_effort` geklammert —
    ///    ohne Override. Scheitert die Klammerung, läuft das Kind **nicht**
    ///    (fail-closed), statt mit ungeklammertem Effort weiterzulaufen.
    /// 7. Der Turn läuft über `run_child_with_budget`.
    /// 8. Das Ergebnis wird gegen den [`ChildReturnContract`] des Kindes
    ///    ausgewertet (siehe [`resolve_child_contract`]).
    /// 9. Der Admission-Slot wird freigegeben — nach der Auswertung, und
    ///    ebenso bei jedem früheren Fehlerausgang ab Schritt 4 (RAII-Guard).
    ///    Nur eine zulässige Pause hält den Slot für die Fortsetzung.
    ///
    /// `args` wird als kompakter JSON-String an den Child-Turn übergeben — ein
    /// strukturierteres JSON→Prompt-Mapping ist expliziter Backlog einer
    /// Folge-Welle.
    ///
    /// # Argumente
    /// - `ctx` (`&OpContext`): Unveränderlicher Ausführungskontext. Liefert
    ///   die Parent-Sandbox (Authority-Monotonie-Basis), die Parent-`SessionId`
    ///   sowie optional `ManagedAgentSpawner`, `StateStore` und
    ///   `Arc<dyn ChildRegistryFactory>`.
    /// - `args` (`serde_json::Value`): JSON-Argumente aus dem Modell-Tool-Call.
    ///   Werden unverändert als `context` in `SpawnInput`, als Auftrag
    ///   (`SpawnInput::instructions`) sowie als Text (`args.to_string()`) im
    ///   initialen Child-Turn übergeben.
    ///
    /// # Returns
    /// - `Ok(OpOutput)` mit dem kanonisch serialisierten JSON des validierten
    ///   Kind-Ergebnisses (Contract `ResearchFinding`/`ReturnEnvelope`) bzw. dem
    ///   Freitext des Kindes (Contract `Text`).
    /// - `Ok(OpOutput)` mit `{"error": …, "raw": …, "contract": …}`, wenn das
    ///   Kind seinen Return-Contract gebrochen hat — **kein** `Err`, siehe
    ///   [`ChildReturnContract`].
    /// - `Ok(OpOutput)` mit `{"paused": …, "child": …}`, wenn das Kind pausieren
    ///   **darf** (`allow_pause = true`).
    ///
    /// # Errors
    /// - [`OpError::InvalidArguments`]: `args` ist kein JSON-Objekt.
    /// - [`OpError::InvalidArguments`]: `args` trägt `effort` oder
    ///   `reasoning_effort` — das Modell darf den Effort des Kindes nicht
    ///   bestimmen (G-084).
    /// - [`OpError::NotAvailable`]: Kein `ManagedAgentSpawner`/`StateStore` im
    ///   Kontext registriert.
    /// - [`OpError::NotAvailable`]: Die Effort-Klammerung des Kindes schlug fehl.
    /// - [`OpError::NotAvailable`]: Die `Surface::AgentTool`-Deklaration trägt ein
    ///   ungültiges `budget_hint`.
    /// - [`OpError::NotAvailable`]: `spawn_child_or_wait` oder
    ///   `run_child_with_budget` schlug fehl (Meldung nennt Kind, Rolle und
    ///   Pause-Sperre). Ein voller Admission-Slot ist dabei **kein**
    ///   Sofortfehler: `spawn_child_or_wait` wartet bis zu
    ///   [`CHILD_SLOT_MAX_WAIT`] auf einen frei werdenden Slot, bevor es den
    ///   Kapazitätsfehler zurückgibt.
    /// - [`OpError::NotAvailable`]: Das Kind pausierte, obwohl sein Lebenszyklus
    ///   das Pausieren verbietet.
    /// - [`OpError::NotAvailable`]: Die Abschlussantwort des Kindes ist nicht
    ///   abrufbar.
    /// - [`OpError::Execution`]: Der Kind-Turn endete terminal ohne Erfolg —
    ///   `TurnOutcome::Cancelled`/`Truncated`/`Refused`/`Failed` (Meldung nennt
    ///   Kind, Rolle und den jeweiligen Grund/Detail).
    ///
    /// # Panics
    /// Keine.
    ///
    /// # Concurrency
    /// `async fn`, `Send`-fähig. Mehrere gleichzeitige `invoke`-Aufrufe auf demselben
    /// Adapter sind sicher, da kein gemeinsamer veränderlicher Zustand verwendet wird.
    /// `spawn_child_or_wait` und `run_child_with_budget` sind selbst
    /// nebenläufigkeitssicher (siehe `ManagedAgentSpawner`-Dokumentation).
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_operations::error::OpError;
    /// # async fn run(adapter: &harw_core_bridge::AgentToolAdapter,
    /// #              ctx: &harw_operations::context::OpContext) {
    /// let result = adapter.invoke(ctx, serde_json::json!({ "topic": "Rust" })).await;
    /// // Ohne registrierten ManagedAgentSpawner/StateStore im Kontext:
    /// assert!(matches!(result, Err(OpError::NotAvailable(_))));
    /// # }
    /// ```
    pub async fn invoke(
        &self,
        ctx: &OpContext,
        args: serde_json::Value,
    ) -> Result<OpOutput, OpError> {
        use tracing::Instrument;
        let json_bytes = args.to_string().len();
        let span = tracing::info_span!(
            "operation.agent_tool",
            op = self.op.meta().name,
            child = self.child_name,
            reducer = self.authority_reducer,
            budget = self.budget_hint,
            json_bytes,
        );
        async move {
            if !args.is_object() {
                return Err(OpError::InvalidArguments(
                    "arguments must be a JSON object".to_owned(),
                ));
            }
            // K5/G-084: Tool-Argumente sind Modell-Output, kein Owner-Nachweis.
            // Ein Effort-Feld wird sichtbar abgelehnt statt still verworfen,
            // damit das Modell nicht glaubt, es habe den Effort gesetzt.
            if let Some(field) = model_effort_field(&args) {
                return Err(OpError::InvalidArguments(format!(
                    "`{field}` is not accepted: the child's reasoning effort is fixed by its \
                     role and declared budget, never by tool arguments"
                )));
            }

            let spawner = ctx.managed_spawner().ok_or_else(|| {
                OpError::NotAvailable(
                    "kein Agent-Spawner in diesem Kontext konfiguriert".to_owned(),
                )
            })?;
            let store = ctx.state_store().ok_or_else(|| {
                OpError::NotAvailable("kein StateStore in diesem Kontext konfiguriert".to_owned())
            })?;

            // Fail-closed: ein unlesbares Budget-Label der eigenen Deklaration
            // ist ein Konfigurationsfehler des Werkzeugs, kein Argumentfehler
            // des Modells — und darf niemals „kein Limit" bedeuten.
            let declared_budget = parse_budget_hint(self.budget_hint).map_err(|error| {
                OpError::NotAvailable(format!(
                    "die AgentTool-Deklaration von '{}' trägt ein ungültiges budget_hint: {error}",
                    self.op.meta().name
                ))
            })?;

            // #22 Welle 1B: ein unbekannter Contract startet kein Kind.
            let contract = resolve_child_contract(ctx, self.child_name).map_err(|error| {
                OpError::NotAvailable(format!(
                    "Child-Agent '{}' kann nicht gestartet werden: {error}",
                    self.child_name
                ))
            })?;
            let validators = resolve_child_validators(ctx, self.child_name);

            let reducer = resolve_authority_reducer(self.authority_reducer);
            let child_sandbox = reducer(ctx.sandbox());

            let spawn_input = harw_extension_api::SpawnInput {
                parent_session_id: ctx.session_id().clone(),
                handoff_call_id: harw_types::ToolCallId::new(),
                // Der Auftrag des Kindes: derselbe Argument-Text, der auch als
                // initialer Turn-Input übergeben wird (siehe unten).
                instructions: Some(task_instructions(&args)),
                context: args.clone(),
                // This tool declares no ceiling demand of its own: the child
                // simply inherits whatever ceiling its parent already
                // enforces, unchanged (see `SpawnInput::ceiling`).
                ceiling: None,
            };

            // Der Cancel-Token des laufenden Turns, falls die Laufzeit einen
            // gesetzt hat (`install_operation_model_tools` in
            // `harw-runtime::assembly`, aus `ToolExecutionContext::cancel`).
            // Ohne registrierten Turn (z. B. ein One-Shot-Pfad ohne
            // `TurnControl` oder ein Test-Fixture) fällt das Warten auf einen
            // frischen, nie abgebrochenen Token zurück und bleibt allein
            // durch `CHILD_SLOT_MAX_WAIT` begrenzt.
            let spawn_cancel = ctx.cancel_token().cloned().unwrap_or_else(CancelToken::new);
            let wait_started = Instant::now();
            let child_guard = spawner
                .spawn_child_or_wait(
                    self.child_name,
                    spawn_input,
                    child_sandbox,
                    None,
                    CHILD_SLOT_MAX_WAIT,
                    &spawn_cancel,
                )
                .await
                .map_err(|e| OpError::NotAvailable(format!("Agent-Spawn fehlgeschlagen: {e}")))?;
            let waited = wait_started.elapsed();
            if waited > Duration::from_millis(50) {
                tracing::debug!(
                    child = %child_guard.child(),
                    waited_ms = waited.as_millis() as u64,
                    "AgentToolAdapter: Spawn wartete auf freien Admission-Slot"
                );
            }
            // `keep()` entschärft den frisch erhaltenen `ChildGuard`, ohne das
            // Kind freizugeben: die Freigabepflicht geht unten unverändert an
            // `ChildSlotGuard` über (identische Freigabe-Semantik wie vor
            // W2d: `child_finished`, nicht `release_child`).
            let child = child_guard.keep();
            // K1/G-016: ab hier gibt jeder Ausgang (auch `?` und ein verworfener
            // Future) den Admission-Slot frei; nur eine zulässige Pause hält ihn.
            let slot = ChildSlotGuard::new(spawner.as_ref(), child.clone());

            // Ein einziger Schnappschuss des Admission-Records: Budget, Rolle und
            // Pause-Sperre stammen damit garantiert aus demselben Zustand. Zwei
            // getrennte Abfragen (`child_budget` + `child_record`) könnten sich
            // widersprechen, wenn das Kind dazwischen geschlossen wird — und ein
            // verlorenes Budget wäre eine Lockerung, kein neutraler Fehler.
            let record = spawner.child_record(&child);
            let role = record
                .as_ref()
                .map_or_else(|| self.child_name.to_owned(), |record| record.role.clone());
            let allow_pause = record.as_ref().is_some_and(|record| record.allow_pause);
            let ir_budget = record
                .as_ref()
                .map_or_else(AgentBudget::default, |record| record.budget);
            let budget = tighten_budget(declared_budget, ir_budget);
            tracing::info!(
                child = %child,
                role = %role,
                contract = contract.as_label(),
                allow_pause,
                max_tokens = ?budget.max_tokens,
                max_tool_calls = ?budget.max_tool_calls,
                max_wall_time_ms = ?budget.max_wall_time_ms,
                "AgentToolAdapter: Child-Agent gespawnt"
            );

            // Wave 8: Reasoning-Effort ist vom Parent bereits monoton geerbt.
            // Der Budget-Cap (Rolle ∩ Deklaration) verschärft ihn. K5/G-084:
            // kein Override — `owner_override` ist hier immer `None`. Ein
            // Clamp-Fehler ist fail-closed: das Kind liefe sonst mit
            // ungeklammertem Effort.
            let effective = spawner
                .clamp_child_reasoning_effort(&child, budget.reasoning_effort, None)
                .map_err(|e| {
                    tracing::warn!(
                        child = %child,
                        error = %e,
                        "AgentToolAdapter: Reasoning-Effort-Clamp fehlgeschlagen"
                    );
                    OpError::NotAvailable(format!(
                        "Reasoning-Effort des Child-Agenten nicht klammerbar (child={child}): {e}"
                    ))
                })?;
            tracing::info!(
                child = %child,
                ?effective,
                "AgentToolAdapter: Reasoning-Effort für Child geklammert"
            );

            // Runde 5, Teil J: eine Fortsetzung (`continue_from` in `args`,
            // bei der Admission geprüft und gebunden) startet mit ihrem
            // hinterlegten Auftrag — Übergabe als erster Kontext.
            let turn_input = if spawner.continuation_link(&child).is_some() {
                TurnInput::default()
            } else {
                TurnInput::user(args.to_string())
            };
            let run_result = spawner
                .run_child_with_budget(&child, store.as_ref(), None, turn_input, budget)
                .await
                .map_err(|e| {
                    // Runde 5, Teil M: Endbericht (Journal, ggf. Übergabe)
                    // statt eines nackten Fehlers.
                    with_child_end_report(
                        spawner.as_ref(),
                        &child,
                        OpError::NotAvailable(format!(
                            "Child-Agent-Ausführung fehlgeschlagen (child={child}, role='{role}', \
                             allow_pause={allow_pause}): {e}"
                        )),
                    )
                })?;

            match &run_result.outcome {
                TurnOutcome::Completed if run_result.budget_exhausted => {
                    // Teil C: Teilergebnis statt Fehler, ausdrücklich markiert.
                    let value = budget_exhausted_value(contract, &run_result, &role, &args);
                    let text = match &value {
                        Value::String(text) => text.clone(),
                        other => other.to_string(),
                    };
                    Ok(OpOutput {
                        text,
                        // Runde 5, Teil J: wie das Ergebnis entstand.
                        data: Some(json!({
                            "budget_exhausted": true,
                            "handoff": handoff_label(&run_result),
                        })),
                    })
                }
                TurnOutcome::Completed => {
                    tracing::info!(
                        child = %run_result.child,
                        contract = contract.as_label(),
                        "AgentToolAdapter: Child-Agent-Turn abgeschlossen"
                    );
                    // #22 Welle 1B: die Rückgabe-Validatoren der IR laufen
                    // auf dem ungekürzten Text, vor dem Contract.
                    if !validators.is_empty() {
                        let text =
                            full_child_return_text(&spawner, &run_result).map_err(|error| {
                                OpError::NotAvailable(format!(
                                    "Child-Agent-Abschlussantwort nicht verfügbar: {error}"
                                ))
                            })?;
                        if let Err(violation) = run_return_validators(&validators, &text) {
                            tracing::warn!(
                                child = %run_result.child,
                                validator = %violation.validator,
                                error = %violation.message,
                                "AgentToolAdapter: Rückgabe-Validator verletzt"
                            );
                            let mut report = violation.to_json();
                            if let Value::Object(map) = &mut report {
                                map.insert(
                                    "contract".to_owned(),
                                    Value::String(contract.as_label().to_owned()),
                                );
                            }
                            return Ok(OpOutput {
                                text: report.to_string(),
                                data: None,
                            });
                        }
                    }
                    match contract {
                        // Rückwärtskompatibel: der Freitext des Kindes geht
                        // unverändert (und ohne JSON-Quoting) an das Parent-Modell.
                        ChildReturnContract::Text => {
                            completed_child_output(plain_child_return_text(&spawner, &run_result))
                        }
                        // Typisierte Contracts parsen den ungekürzten Text: ein
                        // gekürztes JSON wäre sonst ein falscher Vertragsbruch.
                        typed => {
                            let text =
                                full_child_return_text(&spawner, &run_result).map_err(|error| {
                                    OpError::NotAvailable(format!(
                                        "Child-Agent-Abschlussantwort nicht verfügbar: {error}"
                                    ))
                                })?;
                            Ok(contract_output(typed, &text))
                        }
                    }
                }
                TurnOutcome::AwaitingApproval { .. } | TurnOutcome::AwaitingChild { .. } => {
                    let kind = match &run_result.outcome {
                        TurnOutcome::AwaitingApproval { .. } => PauseKind::Approval,
                        _ => PauseKind::Child,
                    };
                    let paused =
                        paused_child_result(kind, &child, &role, allow_pause, &run_result.outcome);
                    if paused.is_ok() {
                        // Zulässige Pause: die Fortsetzung liegt beim Aufrufer,
                        // der dafür das admittierte Kind braucht.
                        slot.keep_admitted();
                    }
                    paused
                }
                // Terminale, nicht-erfolgreiche Ausgänge (K1/G-016: der
                // Admission-Slot wird hier NICHT gehalten — `slot` fällt am
                // Ende des Blocks und gibt ihn frei, wie bei `Completed`).
                // Keine Pause-Erlaubnis ist hier einschlägig: das Kind wartet
                // auf nichts mehr, es gibt keine Fortsetzung.
                TurnOutcome::Cancelled { reason } => {
                    tracing::warn!(
                        child = %run_result.child,
                        reason = ?reason,
                        "AgentToolAdapter: Child-Agent-Turn abgebrochen"
                    );
                    Err(with_child_end_report(
                        spawner.as_ref(),
                        &run_result.child,
                        OpError::Execution(format!(
                            "Child-Agent '{}' (Rolle '{role}') wurde abgebrochen (reason={reason:?})",
                            run_result.child
                        )),
                    ))
                }
                TurnOutcome::Truncated => {
                    tracing::warn!(
                        child = %run_result.child,
                        "AgentToolAdapter: Child-Agent-Antwort abgeschnitten"
                    );
                    Err(with_child_end_report(
                        spawner.as_ref(),
                        &run_result.child,
                        OpError::Execution(format!(
                            "Child-Agent '{}' (Rolle '{role}') brach durch Abschneiden der \
                             Modellausgabe (max_tokens/Kontextfenster) ab, bevor etwaige \
                             Tool-Calls der Antwort ausgeführt wurden",
                            run_result.child
                        )),
                    ))
                }
                TurnOutcome::Refused { detail } => {
                    tracing::warn!(
                        child = %run_result.child,
                        detail = ?detail,
                        "AgentToolAdapter: Child-Agent-Antwort abgelehnt"
                    );
                    Err(with_child_end_report(
                        spawner.as_ref(),
                        &run_result.child,
                        OpError::Execution(format!(
                            "Child-Agent '{}' (Rolle '{role}') lehnte die Antwort ab \
                             (detail={detail:?})",
                            run_result.child
                        )),
                    ))
                }
                TurnOutcome::Failed { reason } => {
                    tracing::warn!(
                        child = %run_result.child,
                        reason = %reason,
                        "AgentToolAdapter: Child-Agent-Wiederaufnahme gescheitert"
                    );
                    Err(with_child_end_report(
                        spawner.as_ref(),
                        &run_result.child,
                        OpError::Execution(format!(
                            "Child-Agent '{}' (Rolle '{role}') scheiterte endgültig \
                             (reason={reason})",
                            run_result.child
                        )),
                    ))
                }
            }
            // Bei `Completed` fällt `slot` hier — nach der Auswertung.
        }
        .instrument(span)
        .await
    }
}

// ── /agent-Produktfläche ─────────────────────────────────────────────────────

/// Bridge-owned runtime adapter for the `/agent` product surface.
///
/// It is intentionally stateless: all authority and lifecycle decisions remain
/// with the `OpContext`-registered [`ManagedAgentSpawner`].
pub struct AgentProductAdapter;

impl AgentProductAdapter {
    /// Validates and dispatches a `/agent` product request.
    ///
    /// # Beschreibung
    /// Drei Aktionen, jede mit einer eigenen Grenze:
    ///
    /// - **`list`** — `ManagedAgentSpawner::list_children_for(session_id)`. Die
    ///   Ausgabe ist ein JSON-Objekt `{"children": [{child, role, depth,
    ///   admitted_at, lease_expires_at}]}`. Zeitstempel sind RFC 3339 (die
    ///   `Display`-Form von `jiff::Timestamp`). Bewusst **nicht** enthalten:
    ///   Session-Historie, Sandbox-Details, Parent-Korrelation, Handoff-IDs.
    /// - **`stop`** — nach [`ensure_owned_child_target`]
    ///   `request_cancellation(target)`; die Ausgabe ist
    ///   `{"cancelled": bool, "child": "<id>"}`. `false` heißt: das Kind war
    ///   bereits terminal (oder hatte keinen laufenden Turn) — das ist eine
    ///   Auskunft, kein Fehler.
    /// - **`budget`** — nach [`ensure_owned_child_target`] wird der Deckel des
    ///   Kindes **gelesen**. Ein mitgeschicktes `budget`-Objekt wird typgeprüft,
    ///   aber nicht angewendet: der Spawner hat keine mutable
    ///   Budget-Transition. Das steht explizit im Ausgabetext
    ///   (`"set_supported": false` plus `note`), statt als nackter Fehler zu
    ///   erscheinen — der Aufrufer bekommt so trotzdem die Information, die er
    ///   wollte.
    ///
    /// # Argumente
    /// - `ctx` (`&OpContext`): liefert die Parent-`SessionId` und den Spawner.
    /// - `args` (`serde_json::Value`): das Request-Objekt (siehe oben).
    ///
    /// # Returns
    /// `Ok(OpOutput)` mit kompaktem JSON-Text.
    ///
    /// # Errors
    /// - [`OpError::InvalidArguments`]: unbekannte/fehlende `action`, fehlendes
    ///   oder leeres `target`, `list` mit Zusatzargumenten, untypisiertes
    ///   `budget`-Feld.
    /// - [`OpError::NotAvailable`]: kein Spawner registriert, oder das Ziel
    ///   gehört nicht zu dieser Parent-Session (eine einzige, absichtlich
    ///   nichtssagende Meldung — siehe [`ensure_owned_child_target`]).
    ///
    /// # Concurrency
    /// Nimmt ausschließlich die kurzen Locks des Spawners; hält keinen Lock über
    /// ein `await`.
    pub async fn invoke(ctx: &OpContext, args: serde_json::Value) -> Result<OpOutput, OpError> {
        let request = AgentProductRequest::parse(args)?;
        let spawner = ctx.managed_spawner().ok_or_else(|| {
            OpError::NotAvailable("kein Agent-Spawner in diesem Kontext konfiguriert".to_owned())
        })?;

        match request {
            AgentProductRequest::List => {
                let children: Vec<Value> = spawner
                    .list_children_for(ctx.session_id())
                    .into_iter()
                    .map(|record| {
                        json!({
                            "child": record.child.as_str(),
                            "role": record.role,
                            "depth": record.depth,
                            "admitted_at": record.admitted_at.to_string(),
                            "lease_expires_at": record.lease_expires_at.to_string(),
                        })
                    })
                    .collect();
                Ok(OpOutput {
                    text: json!({ "children": children }).to_string(),
                    data: None,
                })
            }
            AgentProductRequest::Stop { target } => {
                ensure_owned_child_target(ctx, spawner.as_ref(), &target)?;
                let cancelled = spawner.request_cancellation(&target);
                tracing::info!(
                    child = %target,
                    cancelled,
                    "agent_product.stop"
                );
                Ok(OpOutput {
                    text: json!({ "cancelled": cancelled, "child": target.as_str() }).to_string(),
                    data: None,
                })
            }
            AgentProductRequest::Budget {
                target,
                set_requested,
            } => {
                ensure_owned_child_target(ctx, spawner.as_ref(), &target)?;
                let budget = spawner
                    .child_budget(&target)
                    .ok_or_else(|| OpError::NotAvailable(CHILD_TARGET_UNAVAILABLE.to_owned()))?;
                let note = if set_requested {
                    "das angeforderte Setzen wurde NICHT angewendet: der Agent-Spawner stellt \
                     keine mutable Child-Budget-Transition bereit; das gemeldete Budget ist der \
                     unveränderte Admission-Deckel"
                } else {
                    "schreibgeschützte Budget-Auskunft; null bedeutet: in dieser Dimension kein \
                     Limit"
                };
                Ok(OpOutput {
                    text: json!({
                        "child": target.as_str(),
                        "budget": budget_json(&budget),
                        "set_supported": false,
                        "note": note,
                    })
                    .to_string(),
                    data: None,
                })
            }
        }
    }
}

/// Runde 5, Teil M: ergänzt den Fehler eines nicht regulär beendeten Kindes
/// um seinen Endbericht (Status, Grund, Journal-Kurzfassung, ggf.
/// Übergabe). Ohne Bericht bleibt der Fehler unverändert.
fn with_child_end_report(
    spawner: &harw_core::child_controller::ManagedAgentSpawner,
    child: &harw_types::SessionId,
    error: OpError,
) -> OpError {
    let Some(report) = spawner.child_end_report(child) else {
        return error;
    };
    let text = report.to_parent_text();
    match error {
        OpError::Execution(message) => OpError::Execution(format!("{message}\n\n{text}")),
        OpError::NotAvailable(message) => OpError::NotAvailable(format!("{message}\n\n{text}")),
        other => other,
    }
}

/// Serialisiert einen [`AgentBudget`] für die Produktfläche.
///
/// `null` bedeutet in jeder Dimension „keine Grenze" — dieselbe Semantik wie
/// `Option::None` im Typ selbst.
fn budget_json(budget: &AgentBudget) -> Value {
    json!({
        "max_tokens": budget.max_tokens,
        "max_tool_calls": budget.max_tool_calls,
        "max_wall_time_ms": budget.max_wall_time_ms,
        "reasoning_effort": budget.reasoning_effort.map(|effort| effort.to_string()),
    })
}

/// Validated, intentionally minimal input vocabulary for the `/agent` product
/// adapter. Keeping this type bridge-local prevents command syntax from
/// coupling the core-free operations crate to core lifecycle state.
enum AgentProductRequest {
    List,
    Stop {
        target: SessionId,
    },
    Budget {
        target: SessionId,
        /// `true`, wenn die Anfrage ein `budget`-Objekt mitgeschickt hat, also
        /// ein Setz-Versuch war. Das Feld entscheidet nur über den Hinweistext;
        /// angewendet wird ein Budget in keinem Fall.
        set_requested: bool,
    },
}

impl AgentProductRequest {
    fn parse(args: serde_json::Value) -> Result<Self, OpError> {
        let object = args.as_object().ok_or_else(|| {
            OpError::InvalidArguments("arguments must be a JSON object".to_owned())
        })?;
        let action = object
            .get("action")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| OpError::InvalidArguments("action is required".to_owned()))?;

        match action {
            "list" => {
                if object.len() != 1 {
                    return Err(OpError::InvalidArguments(
                        "list does not accept target or additional arguments".to_owned(),
                    ));
                }
                Ok(Self::List)
            }
            "stop" => Ok(Self::Stop {
                target: parse_product_target(object)?,
            }),
            "budget" => {
                let target = parse_product_target(object)?;
                // Ohne `budget`-Feld ist die Anfrage eine reine Auskunft; mit
                // Feld wird sie trotzdem vollständig typgeprüft, bevor sie als
                // „nicht anwendbar" beantwortet wird. Ein malformter Setz-Versuch
                // darf nicht als gültige Auskunft durchgehen.
                let set_requested = match object.get("budget") {
                    Some(budget) => {
                        validate_budget_request(Some(budget))?;
                        true
                    }
                    None => false,
                };
                Ok(Self::Budget {
                    target,
                    set_requested,
                })
            }
            _ => Err(OpError::InvalidArguments(
                "action must be one of: list, stop, budget".to_owned(),
            )),
        }
    }
}

/// Checks the product vocabulary without claiming that the spawner can apply
/// it. A future mutable-budget capability can accept the same four fields;
/// until then this prevents malformed budget requests from reaching any
/// runtime boundary.
fn validate_budget_request(budget: Option<&serde_json::Value>) -> Result<(), OpError> {
    let budget = budget
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| OpError::InvalidArguments("budget must be a JSON object".to_owned()))?;
    if budget.is_empty() {
        return Err(OpError::InvalidArguments(
            "budget must specify at least one limit".to_owned(),
        ));
    }

    for (name, value) in budget {
        let valid = match name.as_str() {
            "max_tokens" | "max_wall_time_ms" => value.as_u64().is_some(),
            "max_tool_calls" => value
                .as_u64()
                .is_some_and(|value| u32::try_from(value).is_ok()),
            "reasoning_effort" => value
                .as_str()
                .is_some_and(|value| value.parse::<harw_types::ReasoningEffort>().is_ok()),
            _ => false,
        };
        if !valid {
            return Err(OpError::InvalidArguments(format!(
                "budget field `{name}` is unsupported or has an invalid value"
            )));
        }
    }
    Ok(())
}

fn parse_product_target(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Result<SessionId, OpError> {
    let target = object
        .get("target")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| OpError::InvalidArguments("target is required".to_owned()))?;
    SessionId::try_from(target).map_err(|_| {
        OpError::InvalidArguments("target must be a non-empty child session id".to_owned())
    })
}

/// Einzige Meldung für ein Ziel, das dieser Parent nicht besitzt.
///
/// Absichtlich nichtssagend: sie darf nicht verraten, ob eine fremde Session-ID
/// überhaupt existiert.
const CHILD_TARGET_UNAVAILABLE: &str = "agent target is unavailable in this parent session";

/// Confirms that a requested child is currently admitted by this parent before
/// a control transition is considered. The single fail-closed error avoids
/// revealing whether an arbitrary foreign session ID exists.
fn ensure_owned_child_target(
    ctx: &OpContext,
    spawner: &ManagedAgentSpawner,
    target: &SessionId,
) -> Result<(), OpError> {
    let owned = spawner.owns_descendant(ctx.session_id(), target);
    if owned {
        Ok(())
    } else {
        Err(OpError::NotAvailable(CHILD_TARGET_UNAVAILABLE.to_owned()))
    }
}

/// Leitet den Auftragstext (`SpawnInput::instructions`) eines Kindes aus
/// seinen Tool-Argumenten bzw. seiner Frage ab.
///
/// # Arguments
/// - `args` (`&Value`): Tool-Argumente bzw. gestellte Frage.
///
/// # Returns
/// Einen JSON-String unverändert (ohne JSON-Quoting) als Text, jeden anderen
/// Wert als kompaktes JSON — für Objekte identisch zum Text des initialen
/// Kind-Turns.
fn task_instructions(args: &Value) -> String {
    match args {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Liefert den **ungekürzten** Abschlusstext eines Kindes für typisierte
/// Contracts.
///
/// # Description
/// Bevorzugt [`ChildRunResult::full_text`] (vom Controller ungekürzt
/// mitgeliefert); fehlt er (`None`), fällt die Funktion auf
/// [`ManagedAgentSpawner::child_final_assistant_text`] zurück.
///
/// # Errors
/// [`harw_extension_api::AgentSpawnError`], wenn kein `full_text` vorliegt
/// und der Spawner keine Abschlussantwort liefern kann.
fn full_child_return_text(
    spawner: &ManagedAgentSpawner,
    result: &ChildRunResult,
) -> Result<String, harw_extension_api::AgentSpawnError> {
    match &result.full_text {
        Some(text) => Ok(text.clone()),
        None => spawner.child_final_assistant_text(&result.child),
    }
}

/// Liefert den Abschlusstext eines Kindes für den Freitext-Contract
/// ([`ChildReturnContract::Text`]), gekappt auf [`CHILD_RETURN_MAX_BYTES`].
///
/// # Description
/// Liegt [`ChildRunResult::full_text`] vor, wird er hier über
/// [`cap_child_return_text_for_child`] gekappt; sonst gilt der Text von
/// [`ManagedAgentSpawner::child_final_assistant_text`] unverändert.
///
/// # Errors
/// Wie [`full_child_return_text`].
fn plain_child_return_text(
    spawner: &ManagedAgentSpawner,
    result: &ChildRunResult,
) -> Result<String, harw_extension_api::AgentSpawnError> {
    match &result.full_text {
        // Runde 5, Teil H: die Kürzungsmarke nennt `agent.result` mit der Kind-ID.
        Some(text) => Ok(cap_child_return_text_for_child(
            text,
            CHILD_RETURN_MAX_BYTES,
            &result.child,
        )),
        None => spawner.child_final_assistant_text(&result.child),
    }
}

/// Converts the completed child response into the model-visible tool output.
///
/// The spawner alone owns child-session history access. Its retrieval failure
/// is therefore surfaced as `NotAvailable` rather than risking a synthetic
/// success acknowledgement with no child result.
fn completed_child_output(
    final_assistant_text: Result<String, harw_extension_api::AgentSpawnError>,
) -> Result<OpOutput, OpError> {
    final_assistant_text
        .map(|text| OpOutput { text, data: None })
        .map_err(|error| {
            OpError::NotAvailable(format!(
                "Child-Agent-Abschlussantwort nicht verfügbar: {error}"
            ))
        })
}

// ── Budget-Typisierung ───────────────────────────────────────────────────────

/// Erlaubte Segmentformen — steht in jeder Fehlermeldung von
/// [`parse_budget_hint`], damit eine kaputte Deklaration ohne Quellcode-Blick
/// reparierbar ist.
const BUDGET_HINT_GRAMMAR: &str = "erlaubt sind: `<n>[k|m]_tokens`, `<n>_tool_calls`, `<n>s`, \
     `<n>ms`, `effort=<minimal|low|medium|high|xhigh|max>`, Segmente durch Komma getrennt; \
     oder das Einzelwort `unlimited` bzw. ein leeres Label für „kein Limit\"";

/// Parst ein Budget-Label der Form `"8k_tokens,20_tool_calls,30s"`.
///
/// # Beschreibung
/// Grammatik (case-insensitiv, Whitespace um Segmente wird ignoriert):
///
/// ```text
/// hint       := "" | "unlimited" | segment ("," segment)*
/// segment    := tokens | tool_calls | wall_time | effort
/// tokens     := <dezimal> ["k"|"m"] ["_"] "tokens"     // 8k_tokens, 8000tokens, 1m_tokens
/// tool_calls := <dezimal> ["_"] "tool_calls"           // 20_tool_calls
/// wall_time  := <dezimal> ("s" | "ms")                 // 30s, 500ms
/// effort     := "effort=" <minimal|low|medium|high|xhigh|max>
/// ```
///
/// Der `k`/`m`-Multiplikator (×1 000 / ×1 000 000) gilt **nur** für `tokens`;
/// bei allen anderen Einheiten ist er ein Fehler statt einer stillen
/// Interpretation. `30s` wird zu `max_wall_time_ms = 30_000`.
///
/// Drei Dinge sind bewusst harte Fehler statt Toleranz:
/// - **unbekannte Segmente** (`"42_bananas"`) — stilles Ignorieren würde eine
///   gedachte Grenze in „keine Grenze" verwandeln;
/// - **doppelte Dimensionen** (`"1k_tokens,2k_tokens"`) — ein Last-wins-Verhalten
///   könnte die strengere Angabe überschreiben;
/// - **`unlimited` in Kombination** — es beschreibt das gesamte Budget und
///   widerspricht jedem Nachbarsegment.
///
/// # Argumente
/// - `hint` (`&str`): das rohe `budget_hint`-Label aus `Surface::AgentTool`
///   (oder eine gleich geformte Angabe eines Aufrufers).
///
/// # Returns
/// Ein [`AgentBudget`]; nicht genannte Dimensionen bleiben `None` (kein Limit).
/// `""` und `"unlimited"` liefern [`AgentBudget::default`].
///
/// # Errors
/// - [`OpError::InvalidArguments`]: leeres Segment, unbekanntes Segment,
///   doppelte Dimension, nicht-dezimale Zahl, Wertebereichs-Überlauf oder ein
///   unbekanntes Effort-Level. Die Meldung nennt immer das ganze Label und die
///   Grammatik.
///
/// # Concurrency
/// Reine Funktion ohne geteilten Zustand; aus jedem Thread aufrufbar.
///
/// # Examples
/// ```rust
/// use harw_core_bridge::parse_budget_hint;
///
/// let budget = parse_budget_hint("8k_tokens,20_tool_calls,30s").expect("gültiges Label");
/// assert_eq!(budget.max_tokens, Some(8_000));
/// assert_eq!(budget.max_tool_calls, Some(20));
/// assert_eq!(budget.max_wall_time_ms, Some(30_000));
///
/// assert!(parse_budget_hint("unlimited").expect("Standard").max_tokens.is_none());
/// assert!(parse_budget_hint("42_bananas").is_err());
/// ```
pub fn parse_budget_hint(hint: &str) -> Result<AgentBudget, OpError> {
    let normalized = hint.trim().to_ascii_lowercase();
    if normalized.is_empty() || normalized == "unlimited" {
        return Ok(AgentBudget::default());
    }

    let mut budget = AgentBudget::default();
    for raw_segment in normalized.split(',') {
        let segment = raw_segment.trim();
        if segment.is_empty() {
            return Err(invalid_hint(
                hint,
                "leeres Segment (doppeltes oder abschließendes Komma)",
            ));
        }
        if segment == "unlimited" {
            return Err(invalid_hint(
                hint,
                "`unlimited` beschreibt das gesamte Budget und darf nicht mit anderen Segmenten \
                 kombiniert werden",
            ));
        }

        if let Some(level) = segment.strip_prefix("effort=") {
            let parsed = level.parse::<ReasoningEffort>().map_err(|error| {
                invalid_hint(
                    hint,
                    &format!("unbekanntes Effort-Level '{level}': {error}"),
                )
            })?;
            set_budget_dimension(&mut budget.reasoning_effort, parsed, hint, "effort")?;
        } else if let Some(number) = strip_budget_unit(segment, "tokens") {
            let tokens = parse_scaled_count(number, hint, "tokens")?;
            set_budget_dimension(&mut budget.max_tokens, tokens, hint, "tokens")?;
        } else if let Some(number) = strip_budget_unit(segment, "tool_calls") {
            let calls = parse_plain_count(number, hint, "tool_calls")?;
            let calls = u32::try_from(calls).map_err(|_| {
                invalid_hint(hint, "`tool_calls` überschreitet den 32-Bit-Wertebereich")
            })?;
            set_budget_dimension(&mut budget.max_tool_calls, calls, hint, "tool_calls")?;
        } else if let Some(number) = segment.strip_suffix("ms") {
            let millis = parse_plain_count(number, hint, "ms")?;
            set_budget_dimension(&mut budget.max_wall_time_ms, millis, hint, "wall_time")?;
        } else if let Some(number) = segment.strip_suffix('s') {
            let seconds = parse_plain_count(number, hint, "s")?;
            let millis = seconds.checked_mul(1_000).ok_or_else(|| {
                invalid_hint(hint, "die Sekundenangabe überläuft in Millisekunden")
            })?;
            set_budget_dimension(&mut budget.max_wall_time_ms, millis, hint, "wall_time")?;
        } else {
            return Err(invalid_hint(
                hint,
                &format!("unbekanntes Segment '{segment}'"),
            ));
        }
    }
    Ok(budget)
}

/// Verschneidet zwei Budgets zur **strengeren** Grenze je Dimension.
///
/// # Beschreibung
/// Die Operation ist das Infimum (die größte untere Schranke) im Verband der
/// Budgets, in dem `None` das neutrale Element „keine Grenze" ist:
///
/// - `Some(a)` ∧ `Some(b)` = `Some(min(a, b))` — die strengere Zahl gewinnt;
/// - `Some(a)` ∧ `None` = `Some(a)` — **`None` verliert gegen jede Grenze**,
///   denn „keine Grenze" darf eine bestehende Grenze nicht aufheben;
/// - `None` ∧ `None` = `None`.
///
/// Daraus folgt die Monotonie, auf die sich der Adapter verlässt: das Ergebnis
/// ist nie schwächer als eines der Argumente. Wiederholtes Verschneiden kann ein
/// Budget also nur verschärfen, nie lockern (idempotent, kommutativ,
/// assoziativ). Genau deshalb darf `invoke` das deklarierte `budget_hint` und
/// das IR-Budget des Kindes in beliebiger Reihenfolge kombinieren.
///
/// # Argumente
/// - `left` (`AgentBudget`): z. B. das aus `budget_hint` typisierte Budget.
/// - `right` (`AgentBudget`): z. B. der Admission-Deckel aus der Agent-IR.
///
/// # Returns
/// Das dimensionsweise strengere [`AgentBudget`].
///
/// # Concurrency
/// Reine Funktion auf `Copy`-Daten; aus jedem Thread aufrufbar.
///
/// # Examples
/// ```rust
/// use harw_core_bridge::{parse_budget_hint, tighten_budget};
///
/// let declared = parse_budget_hint("8k_tokens").expect("gültig");
/// let from_ir = parse_budget_hint("2k_tokens,10_tool_calls").expect("gültig");
/// let effective = tighten_budget(declared, from_ir);
/// assert_eq!(effective.max_tokens, Some(2_000));
/// assert_eq!(effective.max_tool_calls, Some(10));
/// ```
#[must_use]
pub fn tighten_budget(left: AgentBudget, right: AgentBudget) -> AgentBudget {
    AgentBudget {
        max_tokens: tighter_limit(left.max_tokens, right.max_tokens),
        max_tool_calls: tighter_limit(left.max_tool_calls, right.max_tool_calls),
        max_wall_time_ms: tighter_limit(left.max_wall_time_ms, right.max_wall_time_ms),
        reasoning_effort: tighter_limit(left.reasoning_effort, right.reasoning_effort),
    }
}

/// Infimum zweier optionaler Grenzen; `None` ist das neutrale Element.
fn tighter_limit<T: Ord>(left: Option<T>, right: Option<T>) -> Option<T> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(only), None) | (None, Some(only)) => Some(only),
        (None, None) => None,
    }
}

/// Setzt eine Budget-Dimension genau einmal; ein zweiter Wert ist ein Fehler.
fn set_budget_dimension<T>(
    slot: &mut Option<T>,
    value: T,
    hint: &str,
    dimension: &str,
) -> Result<(), OpError> {
    if slot.is_some() {
        return Err(invalid_hint(
            hint,
            &format!("die Dimension `{dimension}` ist mehrfach angegeben"),
        ));
    }
    *slot = Some(value);
    Ok(())
}

/// Schneidet eine Einheit (und ein optionales trennendes `_`) vom Segment ab.
fn strip_budget_unit<'a>(segment: &'a str, unit: &str) -> Option<&'a str> {
    let number = segment.strip_suffix(unit)?;
    Some(number.strip_suffix('_').unwrap_or(number))
}

/// Parst eine reine Dezimalzahl ohne Multiplikator.
fn parse_plain_count(text: &str, hint: &str, unit: &str) -> Result<u64, OpError> {
    let digits = text.trim();
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid_hint(
            hint,
            &format!("`{unit}` erwartet eine reine Dezimalzahl, fand '{text}'"),
        ));
    }
    digits
        .parse::<u64>()
        .map_err(|error| invalid_hint(hint, &format!("`{unit}`: {error}")))
}

/// Parst eine Dezimalzahl mit optionalem `k`/`m`-Multiplikator.
fn parse_scaled_count(text: &str, hint: &str, unit: &str) -> Result<u64, OpError> {
    let (digits, scale) = match text.strip_suffix('k') {
        Some(digits) => (digits, 1_000_u64),
        None => match text.strip_suffix('m') {
            Some(digits) => (digits, 1_000_000_u64),
            None => (text, 1_u64),
        },
    };
    let base = parse_plain_count(digits, hint, unit)?;
    base.checked_mul(scale)
        .ok_or_else(|| invalid_hint(hint, &format!("`{unit}` überläuft den 64-Bit-Wertebereich")))
}

/// Baut die einheitliche Fehlermeldung für ein ungültiges Budget-Label.
fn invalid_hint(hint: &str, reason: &str) -> OpError {
    OpError::InvalidArguments(format!(
        "ungültiges budget_hint '{hint}': {reason}; {BUDGET_HINT_GRAMMAR}"
    ))
}

// ── Return-Contract ──────────────────────────────────────────────────────────

/// Wie das Ergebnis eines Kind-Agenten interpretiert wird.
///
/// # Beschreibung
/// Der Contract stammt aus der Agent-IR des Kindes
/// (`return_pipeline().contract()`, `agent-definition-dsl.md` §13) und
/// entscheidet, ob die Antwort des Kindes als Freitext durchgereicht oder als
/// strukturiertes, **validiertes** JSON weitergegeben wird.
///
/// ## Warum ein Vertragsbruch kein `Err` ist
/// Bricht ein Kind seinen Return-Contract, ist das ein **Datenfehler des
/// Kindes**, kein Laufzeitfehler des Werkzeugs: Spawner, Sandbox, Budget und
/// Turn-Loop haben korrekt gearbeitet. Ein `Err(OpError)` würde den Tool-Call
/// des Parents als gescheitert markieren und die eigentliche Information — was
/// das Kind stattdessen gesagt hat — verwerfen; das Parent-Modell müsste raten.
/// Stattdessen liefert [`contract_output`] einen regulären [`OpOutput`] mit
/// `{"error": …, "raw": …, "contract": …}`: maschinenlesbar, mit gekürztem
/// Rohtext, sodass das Parent gezielt nachsteuern (erneut fragen, Vertrag
/// erklären, Ergebnis verwerfen) kann.
///
/// ## Unbekannte Labels sind ein Fehler (#22 Welle 1B)
/// Früher fiel ein unbekanntes Label still auf `Text` zurück. Seit der
/// Agent-IR v2 ist das Vokabular geschlossen
/// ([`harw_agent_dsl::ir_v2::ReturnContract`], `HARW-RETURN-001` beim
/// Senken): [`Self::parse`] ist strikt. Bekannte **Text**-Contracts
/// (`execution-summary`, `coding-task`, …, die Matrix-Contracts) werden als
/// [`Self::Text`] durchgereicht — ihr Label dokumentiert die erwartete Form,
/// dieses Werkzeug prüft sie nicht strukturell. Ein unbekanntes Label ist
/// [`UnknownReturnContract`]; das Agent-Werkzeug startet dann kein Kind,
/// `delegate_wave` meldet das Ziel als `failed`.
///
/// # Der vierte Arm — `SecurityVerdict` (AW6-02)
/// [`Self::SecurityVerdict`] validiert die Kind-Antwort gegen
/// [`harw_dod_signals::SecurityVerdict`] (`harwness.security-verdict/v1`).
/// Additiv, wie die Architektur es für einen vierten Arm vorsieht: die
/// drei bestehenden Arme (`Text`, `ResearchFinding`, `ReturnEnvelope`) sind
/// unverändert — kein bestehendes Match wurde umgebaut, nur um einen Fall
/// erweitert. Zwei Besonderheiten gegenüber den anderen typisierten Armen:
/// - **Kein `harw-dod-warden-proto`-Import.** Dieser Adapter kennt
///   `WardenAction` nicht (siehe `Cargo.toml`) — es gibt keinen Weg, aus
///   einem validierten Verdikt eine autorisierte Aktion zu bauen, weder hier
///   noch in `harw-dod-signals` selbst (siehe dortige Moduldoku
///   `verdict.rs`, Abschnitt „Warum ein Verdikt nichts auslöst").
/// - **Inhaltsfreie Vertragsbrüche.** Anders als bei `ResearchFinding`/
///   `ReturnEnvelope` enthält ein Vertragsbruch dieses Arms **keinen**
///   Rohtext-Auszug (siehe [`ContractViolation::to_json`]): ein Verdikt kann
///   Text tragen, den ein Triage-Agent aus angreiferkontrollierten
///   Sensorfeldern abgeleitet hat.
///
/// # Nebenläufigkeit
/// `Copy`, ohne inneren Zustand; frei zwischen Threads kopierbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildReturnContract {
    /// Freitext (Rückwärtskompatibilität).
    Text,
    /// Validiertes [`harw_research::ResearchFinding`] als JSON.
    ResearchFinding,
    /// Validierter [`harw_research::ReturnEnvelope`].
    ReturnEnvelope,
    /// Validiertes [`harw_dod_signals::SecurityVerdict`]
    /// (`harwness.security-verdict/v1`, AW6-02). Siehe Moduldoku oben,
    /// Abschnitt „Der vierte Arm".
    SecurityVerdict,
}

impl ChildReturnContract {
    /// Label des Recherche-Contracts (`harw-registry-defaults`: explorer,
    /// analyst, researcher-web, researcher-deps).
    pub const RESEARCH_FINDING_ID: &'static str = "harwness.return.research-finding@1";

    /// Label des reduzierten Return-Envelopes für read-only Kinder
    /// (`agent-definition-dsl.md` §13).
    pub const RETURN_ENVELOPE_ID: &'static str = "harwness.return.envelope@1";

    /// Label des Verdict-Vertrags (AW6-02). Wortwörtlich identisch mit
    /// [`harw_dod_signals::SecurityVerdict::CONTRACT_ID`] und mit
    /// `[defaults] return_contract` in
    /// `harw-registry-defaults/agents/families/security/security.toml` —
    /// eine Abweichung hier würde jene Vorgabe wirkungslos machen, ohne dass
    /// etwas fehlschlägt.
    pub const SECURITY_VERDICT_ID: &'static str = harw_dod_signals::SecurityVerdict::CONTRACT_ID;

    /// Bildet ein Contract-Label der Agent-IR auf die Auswertung ab — strikt
    /// (#22 Welle 1B).
    ///
    /// # Argumente
    /// - `id` (`&str`): das Label aus `[return] contract = …`.
    ///
    /// # Returns
    /// [`Self::ResearchFinding`] für `"harwness.return.research-finding@1"`,
    /// [`Self::ReturnEnvelope`] für `"harwness.return.envelope@1"`,
    /// [`Self::SecurityVerdict`] für [`Self::SECURITY_VERDICT_ID`]
    /// (`"harwness.security-verdict/v1"`), [`Self::Text`] für jeden anderen
    /// **bekannten** Contract der Agent-IR v2
    /// ([`harw_agent_dsl::ir_v2::ReturnContract::ALL`], etwa
    /// `"harwness.return.coding-task@1"`).
    ///
    /// # Errors
    /// [`UnknownReturnContract`] für ein Label außerhalb des Vokabulars
    /// (auch eine künftige `"harwness.security-verdict/v2"`) — kein stiller
    /// Rückfall auf Freitext.
    ///
    /// # Concurrency
    /// Reine Funktion; aus jedem Thread aufrufbar.
    ///
    /// # Examples
    /// ```rust
    /// use harw_core_bridge::ChildReturnContract;
    ///
    /// assert_eq!(
    ///     ChildReturnContract::parse("harwness.return.research-finding@1"),
    ///     Ok(ChildReturnContract::ResearchFinding)
    /// );
    /// assert_eq!(
    ///     ChildReturnContract::parse("harwness.security-verdict/v1"),
    ///     Ok(ChildReturnContract::SecurityVerdict)
    /// );
    /// assert_eq!(
    ///     ChildReturnContract::parse("harwness.return.coding-task@1"),
    ///     Ok(ChildReturnContract::Text)
    /// );
    /// assert!(ChildReturnContract::parse("harwness.return.nope@1").is_err());
    /// ```
    pub fn parse(id: &str) -> Result<Self, UnknownReturnContract> {
        let trimmed = id.trim();
        if trimmed == Self::SECURITY_VERDICT_ID {
            return Ok(Self::SecurityVerdict);
        }
        match trimmed {
            "harwness.return.research-finding@1" => Ok(Self::ResearchFinding),
            "harwness.return.envelope@1" => Ok(Self::ReturnEnvelope),
            other => match harw_agent_dsl::ir_v2::ReturnContract::parse(other) {
                Some(known) if !known.is_structured() => Ok(Self::Text),
                _ => Err(UnknownReturnContract {
                    label: other.to_owned(),
                }),
            },
        }
    }

    /// Liefert das stabile Label dieser Auswertung.
    ///
    /// # Returns
    /// Für [`Self::Text`] das Pseudolabel `"text"` — es ist **kein**
    /// Contract-Bezeichner der Agent-DSL, sondern benennt die Abwesenheit einer
    /// Prüfung. Für die übrigen Varianten das echte IR-Label.
    ///
    /// # Concurrency
    /// Reine Funktion; aus jedem Thread aufrufbar.
    #[must_use]
    pub fn as_label(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::ResearchFinding => Self::RESEARCH_FINDING_ID,
            Self::ReturnEnvelope => Self::RETURN_ENVELOPE_ID,
            Self::SecurityVerdict => Self::SECURITY_VERDICT_ID,
        }
    }
}

/// Ein Return-Contract-Label außerhalb des Vokabulars der Agent-IR v2
/// ([`ChildReturnContract::parse`], #22 Welle 1B).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownReturnContract {
    /// Das unbekannte Label.
    pub label: String,
}

impl std::fmt::Display for UnknownReturnContract {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unbekannter Return-Contract '{}' (bekannt: {})",
            self.label,
            harw_agent_dsl::ir_v2::ReturnContract::ALL
                .iter()
                .map(|contract| contract.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

impl std::error::Error for UnknownReturnContract {}

/// Maximale Länge des Rohtext-Auszugs in einem Vertragsbruch-Report.
const RAW_EXCERPT_CHARS: usize = 512;

/// Ein gebrochener Return-Contract, aufbereitet für zwei Zielgruppen.
///
/// `raw` ist `None` für [`ChildReturnContract::SecurityVerdict`] (siehe
/// [`Self::content_free`]): ein Verdikt kann Text tragen, den ein
/// Triage-Agent aus angreiferkontrollierten Sensorfeldern abgeleitet hat —
/// eine Ablehnung dieses Arms sagt **dass**, nicht **was** (AW6-02-Auftrag,
/// „Inhaltsfreie Fehlermeldungen").
struct ContractViolation {
    contract: ChildReturnContract,
    message: String,
    raw: Option<String>,
}

impl ContractViolation {
    /// Baut einen Vertragsbruch mit gekürztem Rohtext-Auszug — für die
    /// beiden bestehenden typisierten Arme (`ResearchFinding`,
    /// `ReturnEnvelope`), deren Vertragsbruch-Report dem Parent-Modell
    /// bewusst Rohtext zeigt, damit es nachsteuern kann.
    fn new(contract: ChildReturnContract, message: String, raw: &str) -> Self {
        Self {
            contract,
            message,
            raw: Some(excerpt(raw, RAW_EXCERPT_CHARS)),
        }
    }

    /// Baut einen Vertragsbruch **ohne** Rohtext-Auszug — für
    /// [`ChildReturnContract::SecurityVerdict`]. `message` muss selbst
    /// bereits inhaltsfrei sein (siehe
    /// `harw_dod_signals::error::SignalsError`, dessen `Display`-Ausgabe
    /// diese Eigenschaft garantiert).
    fn content_free(contract: ChildReturnContract, message: String) -> Self {
        Self {
            contract,
            message,
            raw: None,
        }
    }

    /// Modellsichtbare Form: strukturiert, damit das Parent nachsteuern kann.
    /// Enthält `"raw"` nur, wenn dieser Vertragsbruch einen Rohtext-Auszug
    /// trägt (siehe [`Self::content_free`]).
    fn to_json(&self) -> Value {
        match &self.raw {
            Some(raw) => json!({
                "error": self.message,
                "raw": raw,
                "contract": self.contract.as_label(),
            }),
            None => json!({
                "error": self.message,
                "contract": self.contract.as_label(),
            }),
        }
    }

    /// Programmatische Form für Aufrufer mit eigenem Fehlerkanal
    /// (siehe [`fanout_children`]).
    fn to_message(&self) -> String {
        match &self.raw {
            Some(raw) => format!(
                "Vertragsbruch ({}): {} — Rohtext: {}",
                self.contract.as_label(),
                self.message,
                raw
            ),
            None => format!(
                "Vertragsbruch ({}): {}",
                self.contract.as_label(),
                self.message
            ),
        }
    }
}

/// Kürzt einen Text auf `limit` Zeichen (nicht Bytes — keine Panik an
/// Mehrbyte-Grenzen) und markiert die Kürzung.
fn excerpt(raw: &str, limit: usize) -> String {
    if raw.chars().count() <= limit {
        return raw.to_owned();
    }
    let mut shortened: String = raw.chars().take(limit).collect();
    shortened.push('…');
    shortened
}

/// Löst den Return-Contract eines Kindes über seine Agent-IR auf.
///
/// # Beschreibung
/// Der einzige Pfad zur IR eines Kindes führt über
/// [`ChildRegistryFactory::executable_agent_ir`]: der `ManagedAgentSpawner`
/// hält seine Rollendefinitionen privat und veröffentlicht keine IR-Auskunft.
/// Diese Funktion holt sich deshalb eine optional im [`OpContext`] registrierte
/// `Arc<dyn ChildRegistryFactory>` — dieselbe Factory, die der Spawner bei der
/// Admission benutzt — und liest daraus `return_pipeline().contract()`.
///
/// Ist keine Factory registriert oder nennt die IR keinen Contract, gilt
/// [`ChildReturnContract::Text`]: ohne IR gibt es keine Vertragsbehauptung,
/// die dieses Werkzeug prüfen könnte.
///
/// # Argumente
/// - `ctx` (`&OpContext`): liefert die optionale Registry-Factory.
/// - `role` (`&str`): der Rollenname des Kindes (= `child_name` der Fläche).
///
/// # Errors
/// [`UnknownReturnContract`], wenn die IR ein Label außerhalb des
/// Vokabulars trägt (#22 Welle 1B: kein stiller Freitext-Rückfall).
///
/// # Concurrency
/// Nur lesend; die Factory ist per Supertrait `Send + Sync`.
fn resolve_child_contract(
    ctx: &OpContext,
    role: &str,
) -> Result<ChildReturnContract, UnknownReturnContract> {
    let Some(factory) = ctx.service::<Arc<dyn ChildRegistryFactory>>() else {
        tracing::debug!(
            role,
            "agent_tool.contract.no_registry_factory: Freitext-Rückfallebene"
        );
        return Ok(ChildReturnContract::Text);
    };
    let Some(label) = factory
        .executable_agent_ir(role)
        .and_then(|ir| ir.return_pipeline().contract())
    else {
        return Ok(ChildReturnContract::Text);
    };
    ChildReturnContract::parse(label).inspect_err(|error| {
        tracing::warn!(
            role,
            contract = label,
            %error,
            "agent_tool.contract.unknown_label: das Kind wird nicht gestartet"
        );
    })
}

/// Die Rückgabe-Validatoren eines Kindes aus seiner Agent-IR
/// (`[return] validators`, #22 Welle 1B), in Deklarationsreihenfolge; leer
/// ohne Registry-Factory oder IR.
pub(crate) fn resolve_child_validators(ctx: &OpContext, role: &str) -> Vec<String> {
    ctx.service::<Arc<dyn ChildRegistryFactory>>()
        .and_then(|factory| {
            factory
                .executable_agent_ir(role)
                .map(|ir| ir.return_pipeline().validators().to_vec())
        })
        .unwrap_or_default()
}

/// Wertet den Kind-Text gegen einen Contract aus.
///
/// # Returns
/// `Ok(Value)` mit dem **kanonisch serialisierten** JSON des geparsten und
/// validierten Typs (nicht dem Rohtext); `Err(ContractViolation)` bei jedem
/// Vertragsbruch.
///
/// Eine fehlgeschlagene Serialisierung des bereits validierten Typs ist
/// werkzeugseitig und praktisch unerreichbar (reine serde-Daten); sie wird
/// trotzdem als Violation gemeldet, deren Meldung die Werkzeugseite ausdrücklich
/// benennt — panikfrei und ohne dem Kind etwas anzulasten, das es nicht getan hat.
fn evaluate_child_return(
    contract: ChildReturnContract,
    text: &str,
) -> Result<Value, ContractViolation> {
    match contract {
        ChildReturnContract::Text => Ok(Value::String(text.to_owned())),
        ChildReturnContract::ResearchFinding => {
            let finding = harw_research::parse_finding(text).map_err(|error| {
                ContractViolation::new(
                    contract,
                    format!("die Kind-Antwort ist kein gültiges ResearchFinding-JSON: {error}"),
                    text,
                )
            })?;
            harw_research::validate_finding(&finding).map_err(|error| {
                ContractViolation::new(
                    contract,
                    format!("das ResearchFinding verletzt den Recherche-Vertrag: {error}"),
                    text,
                )
            })?;
            serde_json::to_value(&finding).map_err(|error| {
                ContractViolation::new(
                    contract,
                    format!(
                        "werkzeugseitig: die kanonische Serialisierung des validierten \
                         ResearchFindings schlug fehl: {error}"
                    ),
                    text,
                )
            })
        }
        ChildReturnContract::ReturnEnvelope => {
            let envelope = harw_research::parse_return_envelope(text).map_err(|error| {
                ContractViolation::new(
                    contract,
                    format!("die Kind-Antwort ist kein gültiger ReturnEnvelope: {error}"),
                    text,
                )
            })?;
            serde_json::to_value(&envelope).map_err(|error| {
                ContractViolation::new(
                    contract,
                    format!(
                        "werkzeugseitig: die kanonische Serialisierung des validierten \
                         ReturnEnvelopes schlug fehl: {error}"
                    ),
                    text,
                )
            })
        }
        ChildReturnContract::SecurityVerdict => {
            // Inhaltsfrei (siehe `ContractViolation::content_free`): weder
            // `text` noch die `SignalsError`-Meldung landen im
            // Vertragsbruch-Report — ein Verdikt kann Text tragen, den ein
            // Triage-Agent aus angreiferkontrollierten Sensorfeldern
            // abgeleitet hat.
            let verdict = harw_dod_signals::parse_and_validate_verdict(text)
                .map_err(|error| ContractViolation::content_free(contract, error.to_string()))?;
            serde_json::to_value(&verdict).map_err(|_| {
                ContractViolation::content_free(
                    contract,
                    "werkzeugseitig: die kanonische Serialisierung des validierten Verdikts \
                     schlug fehl"
                        .to_owned(),
                )
            })
        }
    }
}

/// Baut die modellsichtbare Tool-Antwort für einen abgeschlossenen Kind-Turn.
///
/// Gibt **immer** einen [`OpOutput`] zurück: bei [`ChildReturnContract::Text`]
/// den unveränderten Freitext, sonst das kanonische JSON — und bei einem
/// Vertragsbruch das strukturierte Fehlerobjekt statt eines `Err`
/// (Begründung: siehe [`ChildReturnContract`]).
fn contract_output(contract: ChildReturnContract, text: &str) -> OpOutput {
    match contract {
        ChildReturnContract::Text => OpOutput {
            text: text.to_owned(),
            data: None,
        },
        typed => match evaluate_child_return(typed, text) {
            Ok(value) => OpOutput {
                text: value.to_string(),
                data: None,
            },
            Err(violation) => {
                tracing::warn!(
                    contract = typed.as_label(),
                    error = %violation.message,
                    "agent_tool.contract.violation: strukturierter Fehler-Output statt Abbruch"
                );
                OpOutput {
                    text: violation.to_json().to_string(),
                    data: None,
                }
            }
        },
    }
}

// ── Pause-Behandlung ─────────────────────────────────────────────────────────

/// Art einer Kind-Pause; das Label ist der Wert des `paused`-Feldes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PauseKind {
    /// `TurnOutcome::AwaitingApproval` — ein Guardrail verlangt eine Freigabe.
    Approval,
    /// `TurnOutcome::AwaitingChild` — das Kind wartet selbst auf ein Enkelkind.
    Child,
}

impl PauseKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::Child => "child",
        }
    }
}

/// Entscheidet, ob eine Kind-Pause eine Auskunft oder ein Vertragsbruch ist.
///
/// # Beschreibung
/// Die Unterscheidung hängt allein an `ChildRecord::allow_pause`, also am
/// `[lifecycle]`-Abschnitt der Agent-IR des Kindes:
///
/// - **`allow_pause = true`** — die Pause ist ein *vorgesehener* Zustand. Es gibt
///   einen Kanal, über den eine Freigabe (oder das Enkelkind-Ergebnis) eintreffen
///   kann, und der Aufrufer kann fortsetzen. Sie wird deshalb als regulärer
///   [`OpOutput`] `{"paused": "approval"|"child", "child": "<id>"}` gemeldet:
///   ein Fehler wäre hier eine Falschaussage.
/// - **`allow_pause = false`** — fail-closed. Read-only Kinder tragen diese
///   Sperre grundsätzlich (Default im [`ChildRecord`]), weil sie keinen Kanal
///   besitzen, über den je eine Freigabe eintreffen könnte; eine „Pause" wäre
///   ein stiller Dauerzustand. Der Kern weist einen solchen Ausgang bereits
///   selbst ab; dieser Zweig ist die zweite, redundante Grenze — und die, deren
///   Meldung Kind, Rolle und Outcome nennt.
///
/// # Argumente
/// - `kind` ([`PauseKind`]): welche Pause vorliegt.
/// - `child` (`&SessionId`): das pausierende Kind.
/// - `role` (`&str`): seine Rolle laut Admission-Record.
/// - `allow_pause` (`bool`): die Pause-Erlaubnis aus dem Admission-Record.
/// - `outcome` (`&TurnOutcome`): der vollständige Ausgang, für die Diagnose.
///
/// # Returns
/// `Ok(OpOutput)` mit dem Pause-Report, wenn die Pause zulässig ist.
///
/// # Errors
/// - [`OpError::NotAvailable`]: das Kind pausierte ohne Pause-Erlaubnis.
fn paused_child_result(
    kind: PauseKind,
    child: &SessionId,
    role: &str,
    allow_pause: bool,
    outcome: &TurnOutcome,
) -> Result<OpOutput, OpError> {
    if allow_pause {
        tracing::info!(
            child = %child,
            role,
            paused = kind.as_str(),
            "agent_tool.child_paused: zulässige Pause, Fortsetzung liegt beim Aufrufer"
        );
        return Ok(OpOutput {
            text: json!({ "paused": kind.as_str(), "child": child.as_str() }).to_string(),
            data: None,
        });
    }
    Err(OpError::NotAvailable(format!(
        "Child-Agent '{child}' (Rolle '{role}') pausierte auf {} statt abzuschließen \
         (Outcome: {outcome:?}), aber sein Lebenszyklus verbietet das Pausieren \
         (allow_pause = false). Read-only Kinder tragen diese Sperre grundsätzlich: sie haben \
         keinen Kanal, über den eine Freigabe je eintreffen könnte, weshalb die Pause \
         fail-closed abgewiesen wird, statt unbegrenzt zu warten.",
        kind.as_str()
    )))
}

// ── Authority-Reducer-Registry ──────────────────────────────────────────────

/// Die vollständige Liste bekannter `authority_reducer`-Kennungen.
///
/// Muss mit `KNOWN_AUTHORITY_REDUCERS` in `harw-macros/src/operation.rs`
/// übereinstimmen (das Makro weist unbekannte Kennungen zur Compile-Zeit ab).
/// Die Kennungen `reduce_to_read_only`, `reduce_to_read_registry`,
/// `reduce_to_read_network`, `reduce_to_read_explore` und
/// `reduce_to_read_workspace_network` sind wortgleich mit
/// `harw_registry_defaults::authority::REDUCE_TO_READ_*` (W5/RD), ebenso ihre
/// Permission-Obergrenzen (siehe [`reducer_ceiling`]).
const KNOWN_AUTHORITY_REDUCERS: &[&str] = &[
    "reduce_to_read_only",
    "reduce_to_read_execute",
    "reduce_to_read_registry",
    "reduce_to_read_network",
    "reduce_to_read_explore",
    "reduce_to_read_workspace_network",
];

/// Liefert die Permission-Obergrenze einer bekannten Reducer-Kennung.
///
/// # Beschreibung
/// Gespiegelt aus `harw_registry_defaults::authority::AuthorityReducer::ceiling`.
/// Bewusst **gespiegelt,
/// nicht aufgerufen**: `cargo metadata` zeigt zwar keinen Zyklus, aber
/// `harw-core-bridge/Cargo.toml` liegt außerhalb der Zuständigkeit von
/// A-BRIDGE, und `harw-registry-defaults` zöge den gesamten Werkzeugbaum
/// (Lens, Tool-Provider, Egress) in diese Adapter-Crate.
///
/// | Kennung | Obergrenze |
/// |---|---|
/// | `reduce_to_read_only` | `{ReadWorkspace}` |
/// | `reduce_to_read_execute` | `{ReadWorkspace, ExecuteProcess}` (nur Bridge) |
/// | `reduce_to_read_registry` | `{ReadWorkspace, ReadCargoRegistry}` |
/// | `reduce_to_read_network` | `{NetworkAccess}` — **ohne** `ReadWorkspace` |
/// | `reduce_to_read_explore` | `{ReadWorkspace, ReadCargoRegistry, NetworkAccess}` |
/// | `reduce_to_read_workspace_network` | `{ReadWorkspace, NetworkAccess}` |
///
/// `reduce_to_read_network` liest den Workspace absichtlich nicht: ein Kind mit
/// Netz und Workspace-Lesezugriff könnte Workspace-Daten über Anfrageparameter
/// hinaustragen (Plan-Annahme A5). Die zwei Explorer-Kennungen sind die
/// bewusste Ausnahme (Nutzerentscheidungen „der Explorer durchsucht auch das
/// Internet“ und „die UIA-Helfer recherchieren kurz online und fügen
/// manchmal Abhängigkeiten hinzu“) für genau die Rollen, deren TOML
/// `web.fetch`/`web.search` admittiert: `reduce_to_read_explore` für
/// `explorer`, `uia-worker` und `uia-writer`,
/// `reduce_to_read_workspace_network` für `uia-explorer`. Ihr Host-Scope
/// bleibt wie bei `reduce_to_read_network` an den egress-gebundenen Scope
/// des Parents gebunden, `NetworkAccess` nur, wenn der Parent es selbst
/// trägt. `reduce_to_read_registry` (`analyst`, `researcher-deps`,
/// `planner`) und `reduce_to_read_only` bleiben ohne Netz.
///
/// # Returns
/// `Some(PermissionSet)` für eine bekannte Kennung, sonst `None`.
fn reducer_ceiling(name: &str) -> Option<PermissionSet> {
    let permissions: &[Permission] = match name {
        "reduce_to_read_only" => &[Permission::ReadWorkspace],
        "reduce_to_read_execute" => &[Permission::ReadWorkspace, Permission::ExecuteProcess],
        "reduce_to_read_registry" => &[Permission::ReadWorkspace, Permission::ReadCargoRegistry],
        "reduce_to_read_network" => &[Permission::NetworkAccess],
        "reduce_to_read_explore" => &[
            Permission::ReadWorkspace,
            Permission::ReadCargoRegistry,
            Permission::NetworkAccess,
        ],
        "reduce_to_read_workspace_network" => {
            &[Permission::ReadWorkspace, Permission::NetworkAccess]
        }
        _ => return None,
    };
    Some(PermissionSet::from_policy(permissions.iter().copied()))
}

/// Löst eine `authority_reducer`-Kennung auf eine monoton reduzierende
/// Sandbox-Transformation auf. Unbekannte Kennungen fallen sicherheitshalber
/// auf die restriktivste bekannte Reduktion zurück (nie auf Identität) —
/// ein Tippfehler im `authority_reducer`-Label darf niemals zu MEHR
/// Authority für das Kind führen.
///
/// # Beschreibung
/// Der `authority_reducer`-String in `Surface::AgentTool` bzw. im
/// [`fanout_children`]-Aufruf ist eine Kennung, keine Funktionsreferenz. Diese
/// Funktion bildet die Kennung auf eine monoton fallende Funktion
/// `fn(&SandboxSpec) -> SandboxSpec` ab (Wave-4-Design-Doc, Abschnitt 2). Jede
/// Reduktion ist ein **Schnitt** mit der Parent-Sandbox: sie entfernt Rechte,
/// fügt nie welche hinzu (K3). Die so reduzierte Sandbox ist die Sandbox, mit
/// der das Kind admittiert wird; `admit` prüft zusätzlich `ensure_child_of`.
///
/// Das `#[operation]`-Makro prüft die Kennung zur Compile-Zeit gegen seine
/// eigene Kopie der Liste. Aufrufer von [`fanout_children`] übergeben die
/// Kennung aber als Laufzeit-String; für sie ist dieser Rückfall samt `warn`
/// die Mitgliedschaftsprüfung.
///
/// # Argumente
/// - `name` (`&str`): Die `authority_reducer`-Kennung.
///
/// # Returns
/// `fn(&SandboxSpec) -> SandboxSpec` — eine monoton reduzierende Funktion.
/// Bekannte Kennungen: siehe [`KNOWN_AUTHORITY_REDUCERS`]. Unbekannte Kennungen
/// fallen auf [`reduce_to_read_only`] zurück.
///
/// # Concurrency
/// Reine Funktion (bis auf das `tracing`-Event), lock-frei, threadsicher.
fn resolve_authority_reducer(name: &str) -> fn(&SandboxSpec) -> SandboxSpec {
    match name {
        "reduce_to_read_only" => reduce_to_read_only,
        "reduce_to_read_execute" => reduce_to_read_execute,
        "reduce_to_read_registry" => reduce_to_read_registry,
        "reduce_to_read_network" => reduce_to_read_network,
        "reduce_to_read_explore" => reduce_to_read_explore,
        "reduce_to_read_workspace_network" => reduce_to_read_workspace_network,
        unknown => {
            tracing::warn!(
                unknown,
                known = %KNOWN_AUTHORITY_REDUCERS.join(", "),
                "agent_tool.authority_reducer.unknown: Rückfall auf reduce_to_read_only"
            );
            reduce_to_read_only // sicherer Fallback, siehe Doku oben
        }
    }
}

/// Schneidet `parent` auf die Obergrenze einer Kennung ohne Netzrecht und
/// leert dabei auch den Host-Scope (ohne `NetworkAccess` bedeutungslos, aber
/// ein leerer Scope ist die strengere Aussage).
fn restrict_without_network(parent: &SandboxSpec, name: &str) -> SandboxSpec {
    let ceiling = reducer_ceiling(name).unwrap_or_else(PermissionSet::empty);
    parent.restrict(
        &PermissionRequest::from_permissions(ceiling.iter())
            .with_network_scope(harw_authority::NetworkScope::empty()),
    )
}

/// Reduziert eine Sandbox auf ausschließlich lesenden Workspace-Zugriff.
///
/// Schnittmenge der Parent-Permissions mit `{ ReadWorkspace }`, Host-Scope
/// geleert — die restriktivste bekannte Reduktion, auch Fallback in
/// [`resolve_authority_reducer`].
fn reduce_to_read_only(parent: &SandboxSpec) -> SandboxSpec {
    restrict_without_network(parent, "reduce_to_read_only")
}

/// Reduziert eine Sandbox auf lesenden Workspace-Zugriff plus Prozessausführung.
///
/// Schnittmenge mit `{ ReadWorkspace, ExecuteProcess }`, Host-Scope geleert.
fn reduce_to_read_execute(parent: &SandboxSpec) -> SandboxSpec {
    restrict_without_network(parent, "reduce_to_read_execute")
}

/// Reduziert eine Sandbox auf lesende Workspace- und Registry-Quellen.
///
/// Schnittmenge mit `{ ReadWorkspace, ReadCargoRegistry }`, Host-Scope geleert
/// (analyst, researcher-deps, planner laut W5/RD; bewusst ohne Netz).
/// `ReadCargoRegistry` bleibt nur, wenn der Parent es selbst hat.
fn reduce_to_read_registry(parent: &SandboxSpec) -> SandboxSpec {
    restrict_without_network(parent, "reduce_to_read_registry")
}

/// Reduziert eine Sandbox auf ausgehenden Netzzugriff ohne Workspace-Lesen.
///
/// Schnittmenge mit `{ NetworkAccess }` (researcher-web laut W5/RD). Der
/// Host-Scope des Parents bleibt als Obergrenze unverändert (`restrict`);
/// verengt wird er von der Composition (`researcher_web_network_scope`), nie
/// erweitert. `NetworkAccess` bleibt nur, wenn der Parent es selbst hat
/// (siehe [`restrict_with_parent_network`]).
fn reduce_to_read_network(parent: &SandboxSpec) -> SandboxSpec {
    restrict_with_parent_network(parent, "reduce_to_read_network")
}

/// Reduziert eine Sandbox auf lesende Workspace- und Registry-Quellen plus
/// ausgehendes Netz (explorer mit Websuche; weitergebbarer Lese-/Netzanteil
/// von uia-worker und uia-writer — deren `ExecuteProcess`/`WriteWorkspace`
/// gibt dieser Reducer nie weiter).
///
/// Schnittmenge mit `{ ReadWorkspace, ReadCargoRegistry, NetworkAccess }`;
/// der Host-Scope des Parents (egress-gebunden) bleibt Obergrenze, wie bei
/// [`reduce_to_read_network`]. Jedes Recht bleibt nur, wenn der Parent es
/// selbst hat.
fn reduce_to_read_explore(parent: &SandboxSpec) -> SandboxSpec {
    restrict_with_parent_network(parent, "reduce_to_read_explore")
}

/// Reduziert eine Sandbox auf lesenden Workspace-Zugriff plus ausgehendes
/// Netz (uia-explorer mit Websuche).
///
/// Schnittmenge mit `{ ReadWorkspace, NetworkAccess }`; Host-Scope wie bei
/// [`reduce_to_read_network`].
fn reduce_to_read_workspace_network(parent: &SandboxSpec) -> SandboxSpec {
    restrict_with_parent_network(parent, "reduce_to_read_workspace_network")
}

/// Schneidet `parent` auf die Obergrenze einer Kennung mit Netzrecht und
/// reicht den Host-Scope des Parents als Obergrenze durch.
///
/// `PermissionRequest::from_permissions` setzt `network_scope` standardmäßig
/// auf `NetworkScope::empty()`, und `restrict` schneidet immer nur (leer ∩
/// irgendwas = leer). Ohne den expliziten `.with_network_scope(...)`-Aufruf
/// würde der Host-Scope des Parents stets geleert, statt ihn als Obergrenze
/// durchzureichen. Erweitert wird er nie: die Composition verengt ihn
/// höchstens (`researcher_web_network_scope`).
fn restrict_with_parent_network(parent: &SandboxSpec, name: &str) -> SandboxSpec {
    let ceiling = reducer_ceiling(name).unwrap_or_else(PermissionSet::empty);
    parent.restrict(
        &PermissionRequest::from_permissions(ceiling.iter())
            .with_network_scope(parent.network_scope().clone()),
    )
}

// ── Fan-out ──────────────────────────────────────────────────────────────────

/// Meldung für ein Kind, das wegen eines schnelleren Geschwisters
/// (`JoinSemantics::AnyTerminal`) nicht mehr gewertet wird — wortgleich mit
/// der Meldung des Kern-Schedulers.
const CANCELLED_BY_SIBLING: &str = "cancelled: sibling completed first";

/// Maximale Länge einer vom Kind behaupteten `question_id` in Fehlermeldungen.
const QUESTION_ID_EXCERPT_CHARS: usize = 64;

/// Startet `questions.len()` Kinder derselben Rolle nebenläufig und liefert die
/// Ergebnisse **in Aufrufreihenfolge**.
///
/// # Beschreibung
/// Grundlage für `/analyze` und den Explore-Fan-out (`coding-philosophy.md` §4:
/// gebundene Fragen, gleiche Rolle, normalisierte Ergebnisse). Jedes Kind erhält
/// dieselbe reduzierte Sandbox und dasselbe Budget; seine Frage geht als
/// Turn-Input hinein und zusätzlich als `context` in den `SpawnInput`.
///
/// Ablauf (W4a/A-BRIDGE, K1/K2):
/// 1. Sandbox einmal monoton reduzieren ([`resolve_authority_reducer`]).
/// 2. Ein rollierender Pool mit höchstens `max_parallel` Plätzen. Ein Platz
///    durchläuft **lazy** und vollständig: `spawn_child_or_wait` (wartet bis
///    zu [`CHILD_SLOT_MAX_WAIT`] auf einen freien Admission-Slot, statt bei
///    voller Kapazität sofort zu scheitern) → Budget mit dem
///    IR-Budget verschneiden ([`tighten_budget`]) → Effort klammern (ohne
///    Override, fail-closed) → `run_child_with_budget` → Auswertung über
///    denselben Contract-Pfad wie [`AgentToolAdapter::invoke`]
///    ([`evaluate_child_return`], plus bei `ResearchFinding` die
///    Belegungsprüfung und `question_id`-Bindung) → Slot-Freigabe. Erst
///    danach wird die nächste Frage admittiert. Damit sind nie mehr als
///    `max_parallel` Kinder dieser Welle gleichzeitig admittiert — vorher wurden
///    alle Kinder vorab admittiert, und ab dem neunten scheiterte die Welle am
///    Admission-Limit, unabhängig von `max_parallel` (G-016).
///
///    Verletzt der Abschlusstext eines Kindes den Contract oder — bei
///    `ResearchFinding` — den Belegungs-Vertrag (ein behaupteter lokaler
///    Beleg ohne einen einzigen ausgeführten Werkzeugaufruf), bekommt **genau
///    dieses eine Kind einen Reparatur-Turn** auf derselben Session, bevor
///    seine Position endgültig als `Err` gilt (siehe [`evaluate_with_repair`]).
///    Das zählt nicht gegen `max_parallel`: der Platz bleibt belegt, es wird
///    keine neue Frage admittiert.
/// 3. [`JoinSemantics::AnyTerminal`]: das erste verwertbare Ergebnis gewinnt;
///    laufende Geschwister werden kooperativ abgebrochen, noch nicht gestartete
///    gar nicht erst admittiert. `AllTerminal` und `Collect` warten auf alle.
///
/// ## `question_id`-Bindung (K4, G-035)
/// Beim Contract [`ChildReturnContract::ResearchFinding`] muss das Finding zu
/// **der** offenen Frage gehören, die dieses Kind bekommen hat: seine
/// `question_id` muss der `id` der Frage gleichen (`question.question.id` in
/// der Form von `harw-ops::explore::child_payload`, sonst `question.id`). Eine
/// fremde oder fehlende `question_id` — oder eine Frage ohne `id` — ist ein
/// Vertragsbruch (`Err` an dieser Position), nie ein verwertbares Finding.
///
/// ## Warum ein Vertragsbruch hier `Err` ist, in `invoke` aber nicht
/// Bewusst unterschiedliche *Darstellung* derselben Auswertung: `invoke`
/// antwortet einem **Modell**, das aus einem `Err` nichts lernen kann und den
/// Rohtext braucht, um nachzusteuern. `fanout_children` antwortet einem
/// **Orchestrator-Code**, der die Welle partitionieren will („welche Kinder
/// haben ein verwertbares Ergebnis geliefert?"). Für ihn ist der eigene
/// Fehlerkanal die brauchbarere Form — und der gekürzte Rohtext steckt in der
/// Meldung, geht also nicht verloren.
///
/// ## Lease-Grenze (K1)
/// Diese Funktion stellt das Ergebnis selbst zu (Rückgabewert). Deshalb gibt
/// sie jeden Admission-Slot frei, sobald das Ergebnis des Kindes ausgewertet
/// ist — ebenso bei Spawn-Folgefehlern, Lauf-/Budgetfehlern, Vertragsbruch,
/// Geschwister-Abbruch und wenn der Future dieser Funktion verworfen wird
/// (RAII-Guard). Nur eine **zulässige** Pause (`allow_pause = true`) hält den
/// Slot, weil der Aufrufer das Kind fortsetzen können muss.
///
/// # Argumente
/// - `ctx` (`&OpContext`): Parent-Sandbox, Parent-`SessionId`, Spawner, Store.
/// - `role` (`&str`): die Rolle **aller** Kinder dieser Welle.
/// - `questions` (`&[serde_json::Value]`): eine Frage je Kind, positionsgleich
///   zum Ergebnisvektor.
/// - `authority_reducer` (`&str`): Kennung der Sandbox-Reduktion.
/// - `budget` ([`AgentBudget`]): Deckel je Kind, vor der Verschneidung mit der IR.
/// - `max_parallel` (`usize`): gewünschte gleichzeitig admittierte und
///   laufende Kinder dieser Welle (`0` wird zu `1`). **Kann nicht** die
///   `uia-worker`-Rollenfamilie über eine Instanz hinaus parallelisieren:
///   [`ManagedAgentSpawner::max_concurrent_instances_for_role`] deckelt
///   `role` unten auf `1`, sobald ihre Organisationsrolle
///   `harw_agent_dsl::roles::AgentRoleId::UiaWorker` ist — unabhängig davon,
///   welchen Wert der Aufrufer (z. B. `analyze(max_parallel: 4)`) übergibt.
///   Da diese Funktion nur **eine einzige** Rolle je Welle fährt (siehe
///   oben), ist hier — anders als bei
///   [`harw_core::child_controller::ManagedAgentSpawner::run_children`],
///   das gemischte Rollen je Anfrage zulässt — keine Minimumsbildung über
///   mehrere Rollen nötig.
/// - `join` ([`JoinSemantics`]): Klammerung der Welle.
/// - `contract` ([`ChildReturnContract`]): wie die Antworten ausgewertet werden.
///
/// # Returns
/// `Ok(Vec)` mit genau `questions.len()` Einträgen in Aufrufreihenfolge:
/// `Ok(Value)` je verwertbarem Ergebnis (kanonisches JSON bzw.
/// `{"paused": …}` bei zulässiger Pause), `Err(String)` je Kind, das kein
/// verwertbares Ergebnis geliefert hat.
///
/// # Errors
/// - [`OpError::NotAvailable`]: kein `ManagedAgentSpawner` bzw. `StateStore` im
///   Kontext. Nur diese Gesamtausfälle sind `Err` — das Scheitern **einzelner**
///   Kinder niemals.
///
/// # Panics
/// Keine.
///
/// # Concurrency
/// `async fn`; alle Plätze werden in **einer** Task gemeinsam gepollt
/// (`std::future::poll_fn`), es werden keine Tasks gespawnt. Die
/// Nebenläufigkeit entsteht, weil Kind-Turns den Session-Lock nicht halten.
///
/// # Examples
/// ```rust,no_run
/// use harw_core::child_controller::{AgentBudget, JoinSemantics};
/// use harw_core_bridge::{ChildReturnContract, fanout_children};
/// # async fn run(ctx: &harw_operations::context::OpContext) {
/// let questions = vec![
///     serde_json::json!({ "question": { "id": "q-jiff", "question": "Welche jiff-Version?" } }),
///     serde_json::json!({ "question": { "id": "q-tokio", "question": "Welche tokio-Version?" } }),
/// ];
/// let results = fanout_children(
///     ctx,
///     "researcher-deps",
///     &questions,
///     "reduce_to_read_registry",
///     AgentBudget::default(),
///     2,
///     JoinSemantics::AllTerminal,
///     ChildReturnContract::ResearchFinding,
/// )
/// .await;
/// # let _ = results;
/// # }
/// ```
// Acht Parameter sind hier Absicht, keine Nachlässigkeit: Sandbox-Reduktion,
// Budget, Parallelität, Join-Semantik und Return-Contract sind fünf voneinander
// unabhängige Politiken der Welle. Sie in eine Options-Struktur zu bündeln würde
// nur den Ort verschieben, an dem der Aufrufer sie alle benennen muss.
#[allow(clippy::too_many_arguments)]
pub async fn fanout_children(
    ctx: &OpContext,
    role: &str,
    questions: &[Value],
    authority_reducer: &str,
    budget: AgentBudget,
    max_parallel: usize,
    join: JoinSemantics,
    contract: ChildReturnContract,
) -> Result<Vec<Result<Value, String>>, OpError> {
    fanout_children_with(
        ctx,
        role,
        questions,
        authority_reducer,
        budget,
        max_parallel,
        join,
        contract,
        None,
    )
    .await
}

/// Wie [`fanout_children`]; mit `continuation` (Runde 5, Teil J) wird jedes
/// frisch admittierte Kind vor seinem ersten Lauf als Fortsetzung eines
/// budget-beendeten Vorgängers gebunden
/// ([`ManagedAgentSpawner::bind_continuation`]: gleicher Elternteil, gleiche
/// Rolle, keine weitere Sandbox, Kettengrenze). Scheitert die Bindung, wird
/// das Kind freigegeben, ohne zu laufen, und seine Position ist ein `Err`.
///
/// # Errors
/// Wie [`fanout_children`].
#[allow(clippy::too_many_arguments)]
pub(crate) async fn fanout_children_with(
    ctx: &OpContext,
    role: &str,
    questions: &[Value],
    authority_reducer: &str,
    budget: AgentBudget,
    max_parallel: usize,
    join: JoinSemantics,
    contract: ChildReturnContract,
    continuation: Option<&ContinuationSeed>,
) -> Result<Vec<Result<Value, String>>, OpError> {
    if questions.is_empty() {
        return Ok(Vec::new());
    }

    let spawner = ctx.managed_spawner().ok_or_else(|| {
        OpError::NotAvailable("kein Agent-Spawner in diesem Kontext konfiguriert".to_owned())
    })?;
    let store = ctx.state_store().ok_or_else(|| {
        OpError::NotAvailable("kein StateStore in diesem Kontext konfiguriert".to_owned())
    })?;

    let reducer = resolve_authority_reducer(authority_reducer);
    let child_sandbox = reducer(ctx.sandbox());

    let total = questions.len();
    let requested_slots = max_parallel.max(1);
    // Sicherheitsnetz gegen `analyze(max_parallel: N)` & Co.: eine
    // `uia-worker`-Rollenfamilie darf nie mit mehr als einer gleichzeitig
    // laufenden Instanz gefanoutet werden, unabhängig vom Aufrufer-Wunsch.
    // `max_concurrent_instances_for_role` kapselt die Organisationsrollen-
    // Fallunterscheidung vollständig in `harw-core` (die Regel gehört dem
    // Spawner, nicht diesem Adapter).
    let slots = requested_slots.min(spawner.max_concurrent_instances_for_role(role));
    if slots < requested_slots {
        tracing::info!(
            role,
            requested_max_parallel = requested_slots,
            effective_max_parallel = slots,
            "agent_fanout.uia_worker_capped",
        );
    }
    let winner = AtomicBool::new(false);
    // Je Position die Session-ID, sobald das Kind admittiert ist — nur damit
    // der Scheduler laufende Geschwister kooperativ abbrechen kann.
    let admitted: Vec<OnceLock<SessionId>> = (0..total).map(|_| OnceLock::new()).collect();
    // #22 Welle 1B: die Rückgabe-Validatoren der Zielrolle.
    let validators = resolve_child_validators(ctx, role);
    let shared = FanoutShared {
        ctx,
        spawner: spawner.as_ref(),
        store: store.as_ref(),
        role,
        child_sandbox: &child_sandbox,
        budget,
        contract,
        validators: &validators,
        winner: &winner,
        continuation,
    };

    let mut results: Vec<Option<Result<Value, String>>> = (0..total).map(|_| None).collect();
    let mut pending = questions.iter().enumerate();
    let mut running = Vec::with_capacity(slots.min(total));
    let mut winner_decided = false;
    // Ein Event statt eines betretenen Spans: `span::Entered` ist `!Send` und
    // würde über die `await`s gehalten den ganzen Future `!Send` machen.
    tracing::info!(
        role,
        children = total,
        max_parallel = slots,
        "agent_fanout.start"
    );

    loop {
        while running.len() < slots {
            let Some((position, question)) = pending.next() else {
                break;
            };
            if winner_decided {
                results[position] = Some(Err(CANCELLED_BY_SIBLING.to_owned()));
                continue;
            }
            running.push((
                position,
                Box::pin(run_fanout_slot(
                    &shared,
                    position,
                    question,
                    &admitted[position],
                )),
            ));
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

        let value = if winner_decided {
            Err(CANCELLED_BY_SIBLING.to_owned())
        } else {
            value
        };
        if !winner_decided && matches!(join, JoinSemantics::AnyTerminal) && value.is_ok() {
            winner_decided = true;
            winner.store(true, Ordering::SeqCst);
            for (sibling, _) in &running {
                if let Some(child) = admitted[*sibling].get() {
                    if !spawner.request_cancellation(child) {
                        tracing::warn!(child = %child, "agent_fanout.cancel_channel_missing");
                    }
                }
            }
        }
        results[position] = Some(value);
    }

    let results: Vec<Result<Value, String>> = results
        .into_iter()
        .map(|slot| {
            slot.unwrap_or_else(|| {
                Err("der Fan-out-Scheduler lieferte für dieses Kind kein Ergebnis".to_owned())
            })
        })
        .collect();
    tracing::info!(
        role,
        children = total,
        failed = results.iter().filter(|slot| slot.is_err()).count(),
        "agent_fanout.complete"
    );
    Ok(results)
}

/// Für alle Plätze einer Fan-out-Welle identische, geliehene Eingaben.
struct FanoutShared<'a> {
    ctx: &'a OpContext,
    spawner: &'a ManagedAgentSpawner,
    store: &'a dyn StateStore,
    role: &'a str,
    child_sandbox: &'a SandboxSpec,
    budget: AgentBudget,
    contract: ChildReturnContract,
    /// #22 Welle 1B: `[return] validators` der Zielrolle.
    validators: &'a [String],
    /// Gesetzt, sobald bei `AnyTerminal` ein Gewinner feststeht.
    winner: &'a AtomicBool,
    /// Runde 5, Teil J: bindet jedes Kind als Fortsetzung dieses Vorgängers.
    continuation: Option<&'a ContinuationSeed>,
}

impl FanoutShared<'_> {
    // Ob ein Geschwister bereits gewonnen hat (nur bei `AnyTerminal` je gesetzt).
    fn sibling_won(&self) -> bool {
        self.winner.load(Ordering::SeqCst)
    }
}

/// Fährt genau einen Platz der Welle: Spawn, Budget, Effort, Lauf, Auswertung,
/// Freigabe. Jeder Fehler wird zur `Err(String)` dieser Position; der
/// Admission-Slot wird über [`ChildSlotGuard`] in jedem Ausgang freigegeben,
/// außer bei einer zulässigen Pause.
async fn run_fanout_slot(
    shared: &FanoutShared<'_>,
    position: usize,
    question: &Value,
    admitted: &OnceLock<SessionId>,
) -> Result<Value, String> {
    if shared.sibling_won() {
        return Err(CANCELLED_BY_SIBLING.to_owned());
    }
    let spawn_input = harw_extension_api::SpawnInput {
        parent_session_id: shared.ctx.session_id().clone(),
        handoff_call_id: harw_types::ToolCallId::new(),
        // Der Auftrag des Kindes: dieselbe Frage, die auch als initialer
        // Turn-Input übergeben wird (siehe unten).
        instructions: Some(task_instructions(question)),
        context: question.clone(),
        // This fan-out tool declares no ceiling demand of its own: each
        // question-child simply inherits whatever ceiling its parent
        // already enforces, unchanged (see `SpawnInput::ceiling`).
        ceiling: None,
    };
    // Der Cancel-Token des laufenden Turns, falls die Laufzeit einen gesetzt
    // hat (`install_operation_model_tools` in `harw-runtime::assembly`, aus
    // `ToolExecutionContext::cancel`). Ohne registrierten Turn fällt das
    // Warten auf einen frischen, nie abgebrochenen Token zurück und bleibt
    // allein durch `CHILD_SLOT_MAX_WAIT` begrenzt.
    let spawn_cancel = shared
        .ctx
        .cancel_token()
        .cloned()
        .unwrap_or_else(CancelToken::new);
    let wait_started = Instant::now();
    let child_guard = shared
        .spawner
        .spawn_child_or_wait(
            shared.role,
            spawn_input,
            shared.child_sandbox.clone(),
            None,
            CHILD_SLOT_MAX_WAIT,
            &spawn_cancel,
        )
        .await
        .map_err(|error| {
            tracing::warn!(role = shared.role, position, error = %error, "agent_fanout.spawn_failed");
            format!("Agent-Spawn fehlgeschlagen: {error}")
        })?;
    let waited = wait_started.elapsed();
    if waited > Duration::from_millis(50) {
        tracing::debug!(
            child = %child_guard.child(),
            position,
            waited_ms = waited.as_millis() as u64,
            "agent_fanout.spawn_waited_for_slot"
        );
    }
    // `keep()` entschärft den frisch erhaltenen `ChildGuard`, ohne das Kind
    // freizugeben: die Freigabepflicht geht unten unverändert an
    // `ChildSlotGuard` über (identische Freigabe-Semantik wie vor W2d).
    let child = child_guard.keep();
    let slot = ChildSlotGuard::new(shared.spawner, child.clone());
    if admitted.set(child.clone()).is_err() {
        // Unerreichbar (eine Position wird genau einmal gestartet); ohne ID
        // bleibt nur der kooperative Geschwister-Abbruch aus, nie die Freigabe.
        tracing::warn!(child = %child, position, "agent_fanout.admitted_cell_occupied");
    }
    if shared.sibling_won() {
        return Err(CANCELLED_BY_SIBLING.to_owned());
    }
    // Runde 5, Teil J: eine Fortsetzung wird vor ihrem ersten Lauf gebunden;
    // scheitert das, gibt `slot` das Kind frei, ohne dass es läuft.
    if let Some(seed) = shared.continuation {
        shared
            .spawner
            .bind_continuation(&child, seed)
            .map_err(|error| {
                tracing::warn!(child = %child, error = %error, "agent_fanout.continuation_refused");
                error.message
            })?;
    }

    // Fail-closed: ohne Admission-Record wäre `unwrap_or_default` „kein Limit".
    let ir_budget = shared
        .spawner
        .child_budget(&child)
        .ok_or_else(|| format!("Admission-Record von Child-Agent '{child}' fehlt"))?;
    let effective = tighten_budget(shared.budget, ir_budget);
    shared
        .spawner
        .clamp_child_reasoning_effort(&child, effective.reasoning_effort, None)
        .map_err(|error| {
            tracing::warn!(child = %child, error = %error, "agent_fanout.effort_clamp_failed");
            format!("Reasoning-Effort von Child-Agent '{child}' nicht klammerbar: {error}")
        })?;

    let run = shared
        .spawner
        .run_child_with_budget(
            &child,
            shared.store,
            None,
            TurnInput::user(question.to_string()),
            effective,
        )
        .await
        .map_err(|error| {
            // Runde 5, Teil M: Endbericht (Journal, ggf. Übergabe) anhängen.
            match shared.spawner.child_end_report(&child) {
                Some(report) => format!(
                    "Child-Ausführung fehlgeschlagen: {error}\n\n{}",
                    report.to_parent_text()
                ),
                None => format!("Child-Ausführung fehlgeschlagen: {error}"),
            }
        })?;
    if shared.sibling_won() {
        return Err(CANCELLED_BY_SIBLING.to_owned());
    }

    match fanout_child_value(
        shared.spawner,
        shared.store,
        effective,
        shared.contract,
        shared.validators,
        &run,
        question,
    )
    .await?
    {
        FanoutValue::Final(value) => Ok(value),
        FanoutValue::Paused(report) => {
            slot.keep_admitted();
            Ok(report)
        }
    }
    // Bei `Final` und jedem `Err` fällt `slot` hier — nach der Auswertung.
}

/// Verwertbares Ergebnis eines Fan-out-Kindes.
enum FanoutValue {
    /// Ausgewertetes Abschlussergebnis; der Slot wird freigegeben.
    Final(Value),
    /// Zulässige Pause (`{"paused": …}`); der Slot bleibt für die Fortsetzung.
    Paused(Value),
}

/// Wertet das Ergebnis genau eines Fan-out-Kindes aus.
///
/// Folgt derselben Pause-Unterscheidung wie [`paused_child_result`]: eine
/// erlaubte Pause ist eine Auskunft (`Ok`), eine verbotene ein Fehler (`Err`).
/// Ein terminaler, nicht-erfolgreicher Ausgang (`Cancelled`/`Truncated`/
/// `Refused`/`Failed`) ist ebenfalls sofort ein `Err` — dort gibt es nichts
/// fortzusetzen. Beim Contract `ResearchFinding` wird das Finding zusätzlich
/// an die gestellte Frage gebunden ([`bind_finding_to_question`]).
///
/// # Reparatur-Turn
/// Verletzt der Abschlusstext den Contract oder — bei `ResearchFinding` —
/// den Belegungs-Vertrag ([`evaluate_return_with_grounding`]), wird über
/// [`evaluate_with_repair`] **ein** Reparatur-Turn an dieselbe Kind-Session
/// gesendet, bevor endgültig aufgegeben wird (siehe dort). Die Bindung an
/// die gestellte Frage ([`bind_finding_to_question`]) ist davon
/// ausgenommen: eine falsch zugeordnete `question_id` ist kein Formatfehler,
/// den ein Reparatur-Turn beheben könnte, sondern ein Zuordnungsfehler.
async fn fanout_child_value(
    spawner: &ManagedAgentSpawner,
    store: &dyn StateStore,
    budget: AgentBudget,
    contract: ChildReturnContract,
    validators: &[String],
    result: &ChildRunResult,
    question: &Value,
) -> Result<FanoutValue, String> {
    let record = spawner.child_record(&result.child);
    let role = record
        .as_ref()
        .map_or_else(|| "unbekannt".to_owned(), |record| record.role.clone());
    let allow_pause = record.as_ref().is_some_and(|record| record.allow_pause);

    let pause = match &result.outcome {
        TurnOutcome::Completed => None,
        TurnOutcome::AwaitingApproval { .. } => Some(PauseKind::Approval),
        TurnOutcome::AwaitingChild { .. } => Some(PauseKind::Child),
        // Terminale, nicht-erfolgreiche Ausgänge sind keine Pause — hier ist
        // nichts fortsetzbar, also sofort ein `Err` statt eines `Final`-Werts.
        TurnOutcome::Cancelled { reason } => {
            return Err(format!(
                "Child-Agent '{}' (Rolle '{role}') wurde abgebrochen (reason={reason:?})",
                result.child
            ));
        }
        TurnOutcome::Truncated => {
            return Err(format!(
                "Child-Agent '{}' (Rolle '{role}') brach durch Abschneiden der Modellausgabe \
                 (max_tokens/Kontextfenster) ab, bevor etwaige Tool-Calls der Antwort \
                 ausgeführt wurden",
                result.child
            ));
        }
        TurnOutcome::Refused { detail } => {
            return Err(format!(
                "Child-Agent '{}' (Rolle '{role}') lehnte die Antwort ab (detail={detail:?})",
                result.child
            ));
        }
        TurnOutcome::Failed { reason } => {
            return Err(format!(
                "Child-Agent '{}' (Rolle '{role}') scheiterte endgültig (reason={reason})",
                result.child
            ));
        }
    };

    if let Some(kind) = pause {
        if allow_pause {
            return Ok(FanoutValue::Paused(
                json!({ "paused": kind.as_str(), "child": result.child.as_str() }),
            ));
        }
        return Err(format!(
            "Child-Agent '{}' (Rolle '{role}') pausierte auf {} statt abzuschließen \
             (Outcome: {:?}), obwohl sein Lebenszyklus das Pausieren verbietet \
             (allow_pause = false)",
            result.child,
            kind.as_str(),
            result.outcome
        ));
    }

    // Teil C: ein vom Token-Budget beendetes Kind liefert ein Teilergebnis,
    // keinen Fehler — ohne Reparatur-Turn (dafür ist kein Budget mehr da).
    if result.budget_exhausted {
        return Ok(FanoutValue::Final(budget_exhausted_value(
            contract, result, &role, question,
        )));
    }

    // Typisierte Contracts parsen den ungekürzten Text (`full_text`), damit
    // ein langes, gültiges JSON nicht an der Rückgabe-Kappung zerbricht.
    let text = full_child_return_text(spawner, result).map_err(|error| {
        format!(
            "Child-Agent-Abschlussantwort von '{}' nicht verfügbar: {error}",
            result.child
        )
    })?;
    // #22 Welle 1B: die Rückgabe-Validatoren der IR vor dem Contract.
    run_return_validators(validators, &text).map_err(|violation| {
        format!(
            "Child-Agent '{}' (Rolle '{role}'): {violation}",
            result.child
        )
    })?;
    let value =
        evaluate_with_repair(spawner, store, contract, &result.child, budget, &text).await?;
    if contract == ChildReturnContract::ResearchFinding {
        bind_finding_to_question(&value, question)?;
    }
    Ok(FanoutValue::Final(value))
}

/// Hinweistext eines Teilergebnisses nach erschöpftem Token-Budget.
const BUDGET_EXHAUSTED_NOTE: &str =
    "Token-Budget erschöpft — das Ergebnis ist ein Teilergebnis und unvollständig";

/// Baut das Fan-out-Ergebnis eines vom Token-Budget beendeten Kindes
/// (Teil C: Teilergebnis statt Fehler).
///
/// # Beschreibung
/// Die letzte Assistant-Antwort des Kindes (`full_text`) wird zuerst gegen
/// den Contract geprüft (ohne Reparatur- und Belegungsprüfung); ist sie
/// gültig, geht sie mit `"budget_exhausted": true` zurück. Sonst:
/// - [`ChildReturnContract::ResearchFinding`]: ein ausdrücklich als
///   Teilergebnis gekennzeichnetes Finding (Konfidenz `low`, der Budgethinweis
///   unter `unresolved_questions`, die Antwort des Kindes als
///   `conclusion`), an die gestellte Frage gebunden — damit Aufrufer wie
///   `/explore` es wie jedes Finding weiterverarbeiten können;
/// - [`ChildReturnContract::Text`]: der Freitext mit vorangestelltem Hinweis;
/// - sonst ein Objekt `{"budget_exhausted": true, "partial_result": …}`.
///
/// Runde 5, Teil J: jedes Objekt trägt zusätzlich `"handoff"`
/// (`"compacted"`/`"last_answer"`). Bei `compacted` ist `full_text` die
/// markierte Übergabe-Verdichtung: beim Contract `Text` geht sie unverändert
/// zurück (sie trägt ihre Markierung selbst), bei `ResearchFinding` wird sie
/// die `conclusion`, sonst `partial_result`.
fn budget_exhausted_value(
    contract: ChildReturnContract,
    result: &ChildRunResult,
    role: &str,
    question: &Value,
) -> Value {
    let partial = result.full_text.clone().unwrap_or_default();
    // Runde 5, Teil H: die Kürzungsmarke nennt `agent.result` mit der Kind-ID.
    let partial = cap_child_return_text_for_child(&partial, CHILD_RETURN_MAX_BYTES, &result.child);
    let handoff = handoff_label(result);
    let compacted = result.budget_handoff == Some(BudgetHandoff::Compacted);
    tracing::warn!(
        child = %result.child,
        role,
        contract = contract.as_label(),
        partial_bytes = partial.len(),
        handoff,
        "agent_fanout.budget_exhausted_partial_result"
    );
    // Runde 5, Teil J: eine verdichtete Übergabe ist Markdown, nie ein
    // Vertragsobjekt — sie wird nicht erst gegen den Contract geparst.
    if !compacted
        && contract != ChildReturnContract::Text
        && let Ok(Value::Object(mut map)) = evaluate_child_return(contract, &partial)
    {
        map.insert("budget_exhausted".to_owned(), Value::Bool(true));
        map.insert("handoff".to_owned(), json!(handoff));
        return Value::Object(map);
    }
    match contract {
        // Die Übergabe trägt ihre Markierung selbst (`HANDOFF_MARKER`).
        ChildReturnContract::Text if compacted => Value::String(partial),
        ChildReturnContract::Text => Value::String(if partial.is_empty() {
            format!("[budget_exhausted: true] {BUDGET_EXHAUSTED_NOTE}; keine Antwort vorhanden.")
        } else {
            format!("[budget_exhausted: true] {BUDGET_EXHAUSTED_NOTE}.\n\n{partial}")
        }),
        ChildReturnContract::ResearchFinding => {
            let conclusion = if compacted {
                partial.clone()
            } else if partial.trim().is_empty() {
                format!("{BUDGET_EXHAUSTED_NOTE}; das Kind hat noch keine Antwort geliefert.")
            } else {
                format!("Teilergebnis ({BUDGET_EXHAUSTED_NOTE}):\n{partial}")
            };
            let finding = harw_research::ResearchFinding {
                question_id: harw_research::QuestionId::new(
                    open_question_id(question).unwrap_or("unbekannt"),
                ),
                conclusion,
                evidence: Vec::new(),
                verified_versions: Vec::new(),
                constraints: Vec::new(),
                compatibility_notes: Vec::new(),
                unresolved_questions: vec![BUDGET_EXHAUSTED_NOTE.to_owned()],
                confidence: harw_research::Confidence::Low,
                produced_by: role.to_owned(),
                produced_at: harw_types::Clock::now(&harw_types::SystemClock),
                likelihood: None,
                confidence_rationale: BUDGET_EXHAUSTED_NOTE.to_owned(),
                hypotheses: Vec::new(),
                key_assumptions: Vec::new(),
                indicators: Vec::new(),
                dissent: Vec::new(),
            };
            match serde_json::to_value(&finding) {
                Ok(Value::Object(mut map)) => {
                    map.insert("budget_exhausted".to_owned(), Value::Bool(true));
                    map.insert("handoff".to_owned(), json!(handoff));
                    Value::Object(map)
                }
                _ => json!({
                    "budget_exhausted": true,
                    "handoff": handoff,
                    "partial_result": partial,
                }),
            }
        }
        ChildReturnContract::ReturnEnvelope | ChildReturnContract::SecurityVerdict => json!({
            "budget_exhausted": true,
            "handoff": handoff,
            "partial_result": partial,
        }),
    }
}

/// Runde 5, Teil J: das Label, wie das Ergebnis eines budget-beendeten
/// Kindes entstand (`"compacted"` oder `"last_answer"`).
fn handoff_label(result: &ChildRunResult) -> &'static str {
    result
        .budget_handoff
        .unwrap_or(BudgetHandoff::LastAnswer)
        .as_str()
}

// ── Contract-/Belegungsauswertung mit Ein-Versuch-Reparatur ─────────────────
//
// Ausgangsproblem (Auftrag "Explore-Kinder liefern kein valides
// ResearchFinding"): read-only Fan-out-Kinder (u. a. Modell `glm-5.3`)
// scheiterten am Contract auf vier Arten — Freitext statt JSON, Echo des
// eingebetteten JSON-Schemas selbst, `{"answers":{}}`, und ein formal
// valides, aber unbelegtes Finding (0 Werkzeugaufrufe, erfundene
// Zeitstempel). Ein einzelner Vertragsbruch beendete den Kind-Lauf bislang
// sofort mit einem `OpError` beim Parent — ohne jede Chance zur Selbstkorrektur.
// Die folgenden Bausteine geben dem Kind **genau einen** Reparatur-Turn,
// bevor endgültig aufgegeben wird.

/// Belege-Arten, die einen tatsächlich ausgeführten Werkzeugaufruf
/// voraussetzen — im Unterschied zu `official_docs`/`repository`/
/// `release_notes`/`standard`/`web`, die ein Kind auch ohne lokale
/// Werkzeuge (aus Trainingswissen oder mitgelieferten Web-Belegen) kennen
/// darf.
///
/// `package_registry_source` ist die sprachneutrale Form (npm, PyPI, crates.io,
/// Maven, …); `cargo_registry_source` bleibt aus Kompatibilitätsgründen
/// erhalten.
const GROUNDED_EVIDENCE_KINDS: [&str; 3] = [
    "local_source",
    "package_registry_source",
    "cargo_registry_source",
];

/// Ob ein kanonisches `ResearchFinding`-JSON mindestens einen Beleg einer
/// [`GROUNDED_EVIDENCE_KINDS`]-Art führt.
///
/// # Arguments
/// - `finding` (`&Value`): das kanonisch serialisierte, bereits
///   contract-validierte Finding.
///
/// # Returns
/// `true`, wenn `evidence` mindestens einen Eintrag mit `kind` aus
/// [`GROUNDED_EVIDENCE_KINDS`] enthält; sonst `false` — insbesondere auch
/// bei fehlendem oder leerem `evidence`-Feld.
fn claims_local_evidence(finding: &Value) -> bool {
    finding
        .get("evidence")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.get("kind")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| GROUNDED_EVIDENCE_KINDS.contains(&kind))
            })
        })
}

/// Zählt die von einem Kind über seine gesamte Session hinweg tatsächlich
/// ausgeführten Werkzeugaufrufe.
///
/// # Description
/// `ManagedAgentSpawner` führt intern denselben Zähler
/// (`child_tool_call_count`, für die `max_tool_calls`-Budgetdurchsetzung),
/// hält ihn aber privat — diese Crate hat nur über den ebenfalls von
/// `run_child_with_budget` benutzten [`StateStore`] Zugriff auf die
/// Kind-Historie. [`StateStore::load_history`] liefert dieselben
/// `TurnItem`s, die `run_turn`/`run_turn_durable` während des Laufs
/// persistiert haben; die Projektion auf [`ModelMessage::ToolCall`]
/// (`ConversationHistory::to_model_messages`) zählt sie, ohne den
/// `TurnItem`-Typ selbst zu benennen (diese Crate hängt nicht direkt von
/// `harw-protocol` ab).
///
/// # Arguments
/// - `store` (`&dyn StateStore`): derselbe Store, mit dem das Kind lief.
/// - `child` (`&SessionId`): die zu zählende Kind-Session.
///
/// # Returns
/// Anzahl der `ToolCall`-Einträge über die gesamte Historie des Kindes.
///
/// # Errors
/// `Err(String)`, wenn der Store die Historie nicht laden kann.
async fn count_child_tool_calls(
    store: &dyn StateStore,
    child: &SessionId,
) -> Result<usize, String> {
    let history = store.load_history(child).await.map_err(|error| {
        format!("Werkzeugaufruf-Zählung für Kind '{child}' fehlgeschlagen: {error}")
    })?;
    Ok(history
        .to_model_messages()
        .iter()
        .filter(|message| matches!(message, ModelMessage::ToolCall { .. }))
        .count())
}

/// Wertet den Kind-Text gegen den Contract und — bei `ResearchFinding` —
/// zusätzlich gegen den Belegungs-Vertrag aus.
///
/// # Description
/// Ein Finding, das mindestens einen Beleg der Art [`GROUNDED_EVIDENCE_KINDS`]
/// behauptet, aber aus einer Kind-Session stammt, die keinen einzigen
/// Werkzeugaufruf ausgeführt hat, ist eine unbelegte Behauptung — der
/// Beleg-Locator kann nicht tatsächlich gelesen worden sein. Das ist
/// derselbe Vertragsbruch-Kanal wie ein Contract-Verstoß
/// ([`ContractViolation`]), damit [`evaluate_with_repair`] beide Fälle
/// identisch behandelt.
///
/// Kann die Werkzeugaufruf-Anzahl nicht ermittelt werden (Store-Fehler), ist
/// das ein Store-/Werkzeugfehler dieses Adapters, keine Auskunft über das
/// Kind — der Fall wird geloggt und **nicht** als Vertragsbruch gewertet
/// (fail-open nur für diese eine, werkzeugseitige Fehlerquelle; die übrigen
/// Prüfungen dieser Funktion bleiben fail-closed).
///
/// # Errors
/// [`ContractViolation`] bei einem Contract- oder Belegungs-Verstoß.
async fn evaluate_return_with_grounding(
    store: &dyn StateStore,
    contract: ChildReturnContract,
    child: &SessionId,
    text: &str,
) -> Result<Value, ContractViolation> {
    let value = evaluate_child_return(contract, text)?;
    if contract != ChildReturnContract::ResearchFinding || !claims_local_evidence(&value) {
        return Ok(value);
    }
    match count_child_tool_calls(store, child).await {
        Ok(0) => Err(ContractViolation::new(
            contract,
            "Befund ohne Werkzeugaufruf — Belege nicht verifiziert (das Finding behauptet \
             lokale Belege der Art local_source/package_registry_source/cargo_registry_source, aber die Kind-Session \
             hat keinen einzigen Werkzeugaufruf ausgeführt)"
                .to_owned(),
            text,
        )),
        Ok(_) => Ok(value),
        Err(error) => {
            tracing::warn!(
                child = %child,
                error = %error,
                "agent_fanout.tool_call_count_unavailable: Belegungsprüfung übersprungen"
            );
            Ok(value)
        }
    }
}

/// Baut die Reparatur-Aufforderung an ein Kind, dessen letzte Antwort den
/// Vertrag verletzt hat.
///
/// Nutzt die kurze `violation.message` statt [`ContractViolation::to_message`]
/// — der Rohtext-Auszug, den `to_message` anhängt, ist hier redundant, das
/// Kind kennt seine eigene letzte Antwort bereits über die fortgeführte
/// Session-Historie.
fn repair_prompt(violation: &ContractViolation) -> String {
    format!(
        "Deine Antwort verletzt den Vertrag: {}. Antworte jetzt ausschließlich mit dem \
         geforderten JSON-Objekt (kein Schema, kein Tool-Call-Text).",
        violation.message
    )
}

/// Wertet den Abschlusstext eines Fan-out-Kindes gegen Contract und
/// Belegungs-Vertrag aus; verletzt die erste Antwort einen der beiden, wird
/// **genau ein** Reparatur-Turn an dieselbe Kind-Session gesendet und erneut
/// gewertet, bevor endgültig aufgegeben wird.
///
/// # Description
/// Der Reparatur-Turn läuft über [`ManagedAgentSpawner::run_child_with_budget`]
/// auf **derselben** `child`-`SessionId` (nicht über einen neuen Spawn): die
/// Kind-Session bleibt zwischen beiden Turns admittiert (der Admission-Slot
/// wird erst nach dieser Auswertung freigegeben, siehe [`run_fanout_slot`]),
/// ihre Historie — inklusive bereits ausgeführter Werkzeugaufrufe — bleibt
/// erhalten und fließt in den zweiten `evaluate_return_with_grounding`-Versuch
/// ein. `run_child_with_budget` erlaubt einen zweiten Turn auf einer bereits
/// `Completed` markierten Session ausdrücklich (siehe
/// `ManagedAgentSpawner::mark_running`); nur ein bereits abgebrochenes Kind
/// (`ChildStatus::Cancelled`) lehnt einen weiteren Turn ab.
///
/// Der Reparatur-Turn zählt gegen dasselbe, bereits verschärfte `budget` wie
/// der erste Turn (`max_tool_calls`/`max_tokens` rechnen kumulativ über die
/// gesamte Kind-Session ab) — es gibt kein zusätzliches Budget für den
/// Reparaturversuch.
///
/// # Arguments
/// - `spawner` (`&ManagedAgentSpawner`): fährt den Reparatur-Turn.
/// - `store` (`&dyn StateStore`): persistiert den Reparatur-Turn und liefert
///   die Werkzeugaufruf-Zählung für die erneute Belegungsprüfung.
/// - `contract` (`ChildReturnContract`): derselbe Contract wie beim ersten
///   Versuch.
/// - `child` (`&SessionId`): die fortzuführende Kind-Session.
/// - `budget` (`AgentBudget`): das für dieses Kind bereits verschärfte
///   Budget (siehe [`tighten_budget`]).
/// - `text` (`&str`): der erste Abschlusstext des Kindes.
///
/// # Returns
/// Das kanonisch serialisierte, validierte JSON — entweder aus dem ersten
/// Versuch (kein Vertragsbruch) oder aus dem Reparatur-Turn.
///
/// # Errors
/// `Err(String)`: die (ggf. um den Reparatur-Ausgang ergänzte)
/// Vertragsbruch-Meldung. Scheitert auch der Reparatur-Versuch, trägt die
/// Meldung den Hinweis „nach 1 Reparaturversuch".
async fn evaluate_with_repair(
    spawner: &ManagedAgentSpawner,
    store: &dyn StateStore,
    contract: ChildReturnContract,
    child: &SessionId,
    budget: AgentBudget,
    text: &str,
) -> Result<Value, String> {
    let violation = match evaluate_return_with_grounding(store, contract, child, text).await {
        Ok(value) => return Ok(value),
        Err(violation) => violation,
    };
    tracing::warn!(
        child = %child,
        contract = contract.as_label(),
        error = %violation.message,
        "agent_fanout.contract_violation.repair_attempt"
    );

    let repair_run = spawner
        .run_child_with_budget(
            child,
            store,
            None,
            TurnInput::user(repair_prompt(&violation)),
            budget,
        )
        .await
        .map_err(|error| {
            format!(
                "{} (nach 1 Reparaturversuch: Reparatur-Turn fehlgeschlagen: {error})",
                violation.to_message()
            )
        })?;
    if !matches!(repair_run.outcome, TurnOutcome::Completed) || repair_run.budget_exhausted {
        return Err(format!(
            "{} (nach 1 Reparaturversuch: der Reparatur-Turn schloss nicht regulär ab, \
             Outcome: {:?})",
            violation.to_message(),
            repair_run.outcome
        ));
    }
    let repaired_text = full_child_return_text(spawner, &repair_run).map_err(|error| {
        format!(
            "{} (nach 1 Reparaturversuch: Abschlussantwort nicht verfügbar: {error})",
            violation.to_message()
        )
    })?;
    evaluate_return_with_grounding(store, contract, child, &repaired_text)
        .await
        .map_err(|second_violation| {
            format!(
                "{} (nach 1 Reparaturversuch)",
                second_violation.to_message()
            )
        })
}

/// Liefert die `id` einer gestellten Frage.
///
/// Akzeptiert die Nutzlastform von `harw-ops` (`{"question": {"id": …}}`) und
/// eine flache Form (`{"id": …}`); leere IDs gelten als fehlend.
fn open_question_id(question: &Value) -> Option<&str> {
    question
        .get("question")
        .and_then(|inner| inner.get("id"))
        .or_else(|| question.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
}

/// Prüft, dass ein validiertes Finding zu der Frage gehört, die dem Kind
/// gestellt wurde (K4, G-035).
///
/// # Arguments
/// - `finding` (`&Value`): kanonisches `ResearchFinding`-JSON.
/// - `question` (`&Value`): die an dieses Kind gestellte Frage.
///
/// # Errors
/// `Err(String)` (Vertragsbruch-Meldung), wenn die Frage keine `id` trägt, das
/// Finding keine `question_id` hat oder beide nicht exakt übereinstimmen. Die
/// vom Kind behauptete ID wird nur gekürzt zitiert.
fn bind_finding_to_question(finding: &Value, question: &Value) -> Result<(), String> {
    let label = ChildReturnContract::RESEARCH_FINDING_ID;
    let expected = open_question_id(question).ok_or_else(|| {
        format!(
            "Vertragsbruch ({label}): die gestellte Frage trägt keine `id`; ein Finding ist \
             keiner offenen Frage zuordenbar"
        )
    })?;
    let claimed = finding
        .get("question_id")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("Vertragsbruch ({label}): das Finding trägt keine question_id"))?;
    if claimed == expected {
        return Ok(());
    }
    Err(format!(
        "Vertragsbruch ({label}): das Finding beantwortet die Frage '{}', gestellt war '{}'",
        excerpt(claimed, QUESTION_ID_EXCERPT_CHARS),
        excerpt(expected, QUESTION_ID_EXCERPT_CHARS)
    ))
}

// ── Slot-Freigabe und Effort-Sperre ──────────────────────────────────────────

/// Obergrenze, wie lange ein Spawn-Versuch auf einen freien Admission-Slot
/// wartet, bevor er endgültig mit dem ursprünglichen Kapazitätsfehler
/// fehlschlägt (siehe
/// [`harw_core::child_controller::ManagedAgentSpawner::spawn_child_or_wait`]).
///
/// Ohne dieses Warten scheiterten parallele `explore`-/Fan-out-Aufrufe, die
/// `max_active_children_per_parent` überschreiten, sofort statt sich
/// einzureihen. Die Obergrenze ist bewusst endlich, damit ein hängendes
/// Geschwister-Kind (das seinen Slot nie freigibt) den Parent nicht auf
/// unbestimmte Zeit blockiert.
const CHILD_SLOT_MAX_WAIT: Duration = Duration::from_secs(120);

/// Hält den Admission-Slot eines Kindes und gibt ihn beim Drop frei (K1, G-016).
///
/// # Beschreibung
/// Freigabe über [`harw_extension_api::AgentSpawner::child_finished`] — beim
/// `ManagedAgentSpawner` identisch mit `close_child`. Der Guard wird direkt
/// nach einem erfolgreichen Spawn angelegt, damit **jeder** spätere Ausgang
/// (`?`, Vertragsbruch, Budget-Abbruch, verworfener Future) den Slot freigibt.
/// [`Self::keep_admitted`] entschärft ihn für eine zulässige Pause.
///
/// # Concurrency
/// Leiht den Spawner; `child_finished` nimmt nur kurze interne Locks und hält
/// keinen über ein `await`.
struct ChildSlotGuard<'a> {
    spawner: &'a ManagedAgentSpawner,
    child: SessionId,
    armed: bool,
}

impl<'a> ChildSlotGuard<'a> {
    // Übernimmt die Freigabepflicht für ein soeben admittiertes Kind.
    fn new(spawner: &'a ManagedAgentSpawner, child: SessionId) -> Self {
        Self {
            spawner,
            child,
            armed: true,
        }
    }

    // Behält das Kind admittiert (nur für eine zulässige Pause).
    fn keep_admitted(mut self) {
        self.armed = false;
        tracing::debug!(child = %self.child, "agent_tool.child_slot.kept_for_pause");
    }
}

impl Drop for ChildSlotGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            harw_extension_api::AgentSpawner::child_finished(self.spawner, &self.child);
            tracing::debug!(child = %self.child, "agent_tool.child_slot.released");
        }
    }
}

/// Argumentfelder, mit denen ein Modell früher den Kind-Effort überschrieb.
const MODEL_EFFORT_FIELDS: &[&str] = &["effort", "reasoning_effort"];

/// Liefert das erste Effort-Feld in Modell-Argumenten, falls vorhanden (K5).
fn model_effort_field(args: &Value) -> Option<&'static str> {
    MODEL_EFFORT_FIELDS
        .iter()
        .copied()
        .find(|field| args.get(*field).is_some())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};

    use harw_agent_dsl::authority::AuthorityCeiling;
    use harw_agent_dsl::executable::{ExecutableAgentIr, lower};
    use harw_agent_dsl::parse::parse_toml;
    use harw_agent_dsl::resolved::{ResolutionTrace, ResolvedAgentDefinition};
    use harw_authority::{
        NetworkScope, Permission, PermissionSet, SandboxSpec, WorkspaceRegistration,
        WorkspaceRegistry,
    };
    use harw_catalog::AgentSuggestions;
    use harw_core::child_controller::{AgentBudget, ChildRegistryFactory, ManagedAgentSpawner};
    use harw_core::turn_loop::TurnOutcome;
    use harw_core::{
        ChildLimits, ConversationHistory, InMemoryStateStore, ModelProvider, SessionManager,
        StateStore,
    };
    use harw_extension_api::{AgentSpawnError, ExtensionRegistry, SpawnInput};
    use harw_types::{
        ItemId, ReasoningEffort, SessionId, TenantId, ToolCallId, TurnId, WorkspaceId,
    };

    use super::{
        AgentToolAdapter, ChildReturnContract, ContractViolation, KNOWN_AUTHORITY_REDUCERS,
        PauseKind, bind_finding_to_question, claims_local_evidence, completed_child_output,
        contract_output, count_child_tool_calls, evaluate_return_with_grounding,
        evaluate_with_repair, model_effort_field, parse_budget_hint, paused_child_result,
        reducer_ceiling, repair_prompt, resolve_authority_reducer, resolve_child_contract,
        resolve_child_validators, run_return_validators, tighten_budget,
    };
    use crate::context_ext::OpContextCoreExt;
    use crate::test_support::{TestError, TestResult};
    use harw_operations::context::{OpContext, ServiceMap};
    use harw_operations::error::OpError;
    use harw_operations::operation::{
        BusyAvailability, OpFuture, OpInput, OpOutput, Operation, OperationDomain, OperationMeta,
        PermissionTier, Surface,
    };

    // ── test helpers ──────────────────────────────────────────────────────────

    /// Erstellt einen minimalen [`OpContext`] für Tests.
    ///
    /// Verwendet einen atomaren Zähler für Thread-sichere, eindeutige
    /// Verzeichnisnamen, sodass parallele Tests nicht kollidieren.
    fn make_test_ctx() -> TestResult<(OpContext, PathBuf)> {
        make_test_ctx_with(ServiceMap::new())
    }

    /// Wie [`make_test_ctx`], aber mit vorbereiteter [`ServiceMap`].
    fn make_test_ctx_with(services: ServiceMap) -> TestResult<(OpContext, PathBuf)> {
        static CTX_COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = CTX_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "harw-agent-tool-adapter-test-{}-{}",
            std::process::id(),
            id
        ));
        std::fs::create_dir_all(tmp.join("ws")).map_err(crate::test_support::ctx(
            "Test-Workspace-Verzeichnis anlegen",
        ))?;
        let registry = WorkspaceRegistry::build(
            &tmp,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: PathBuf::from("ws"),
            }],
        )
        .map_err(crate::test_support::ctx("Test-Workspace-Registry aufbauen"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .map_err(crate::test_support::ctx("Test-Workspace auflösen"))?;
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        let ctx = OpContext::new(SessionId::new(), TurnId::new(), sandbox, services);
        Ok((ctx, tmp))
    }

    /// Baut eine `ServiceMap` mit echter Core-Laufzeit (Spawner ohne Rollen,
    /// In-Memory-Store), damit die Produktfläche ihre Grenzprüfungen zeigt.
    ///
    /// Der zweite Rückgabewert hält den Session-Event-Empfänger am Leben; wird
    /// er fallen gelassen, laufen alle Session-Events ins Leere.
    fn services_with_runtime() -> (ServiceMap, Box<dyn std::any::Any>) {
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
        (services, Box::new(event_rx))
    }

    /// Ein gültiges `ResearchFinding` als Kind-Antwort (Formfixture aus der
    /// `harw-research`-Crate-Dokumentation).
    const VALID_FINDING: &str = r#"{"question_id":"q-1","conclusion":"jiff 0.2.32 is current",
        "evidence":[{"kind":"cargo_registry_source",
        "locator":"crates.io/crates/jiff","retrieved_at":"2026-08-27T00:00:00Z"}],
        "confidence":"high","produced_by":"explorer-1",
        "produced_at":"2026-08-27T00:00:00Z"}"#;

    /// Registry-Factory, die für jede Rolle dieselbe gefrorene Agent-IR liefert.
    struct IrRegistryFactory {
        ir: ExecutableAgentIr,
    }

    impl ChildRegistryFactory for IrRegistryFactory {
        fn build_registry(
            &self,
            _role: &str,
            _input: &SpawnInput,
            _suggestions: Option<&AgentSuggestions>,
        ) -> Result<ExtensionRegistry, AgentSpawnError> {
            Err(AgentSpawnError {
                message: "Test-Factory startet keine Kinder".to_owned(),
            })
        }

        fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
            Err(AgentSpawnError {
                message: "Test-Factory hält keinen Model-Provider".to_owned(),
            })
        }

        fn executable_agent_ir(&self, _role: &str) -> Option<&ExecutableAgentIr> {
            Some(&self.ir)
        }
    }

    /// Lowert eine Test-Agent-IR mit dem angegebenen `[return]`-Abschnitt.
    fn ir_with_contract(contract: &str) -> TestResult<ExecutableAgentIr> {
        ir_with_return(&format!("contract = \"{contract}\""))
    }

    /// Wie [`ir_with_contract`], mit dem vollständigen Inhalt von `[return]`.
    fn ir_with_return(return_table: &str) -> TestResult<ExecutableAgentIr> {
        let raw = parse_toml(&format!(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.bridge-contract-test@1"
version = "1.0.0"
role = "worker"
specialization = "bridge-contract-test"

[return]
{return_table}
"#
        ))
        .map_err(crate::test_support::ctx(
            "Test-Agent-Definition muss parsen",
        ))?;
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
            reasoning_effort: raw.reasoning_effort.clone(),
        };
        lower(&resolved).map_err(crate::test_support::ctx(
            "Test-Agent-Definition muss lowern",
        ))
    }

    // ── test-op fixtures ──────────────────────────────────────────────────────

    /// Op mit einer `Surface::AgentTool`-Fläche.
    struct AgentOp;

    impl Operation for AgentOp {
        fn meta(&self) -> &OperationMeta {
            static M: OnceLock<OperationMeta> = OnceLock::new();
            M.get_or_init(|| OperationMeta {
                name: "spawn_x",
                summary: "Spawnt Child-Agent X.",
                domain: OperationDomain::Agents,
                permission: PermissionTier::Operator,
                surfaces: vec![Surface::AgentTool {
                    child_name: "researcher",
                    authority_reducer: "reduce_ro",
                    budget_hint: "8k",
                }],
                aliases: &[],
                category: harw_operations::OperationCategory::Agent,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }

        fn run<'a>(&'a self, _c: &'a OpContext, _i: OpInput) -> OpFuture<'a> {
            Box::pin(async {
                Ok(OpOutput {
                    text: "op".to_owned(),
                    data: None,
                })
            })
        }
    }

    /// Op ohne jegliche Surface-Deklaration.
    struct NonAgentOp;

    impl Operation for NonAgentOp {
        fn meta(&self) -> &OperationMeta {
            static M: OnceLock<OperationMeta> = OnceLock::new();
            M.get_or_init(|| OperationMeta {
                name: "no_agent",
                summary: "Keine AgentTool-Fläche.",
                domain: OperationDomain::Misc,
                permission: PermissionTier::Observer,
                surfaces: vec![],
                aliases: &[],
                category: harw_operations::OperationCategory::Agent,
                args_schema: None,
                output_schema: None,
                busy: BusyAvailability::DeferredUntilTurnEnd,
            })
        }

        fn run<'a>(&'a self, _c: &'a OpContext, _i: OpInput) -> OpFuture<'a> {
            Box::pin(async {
                Ok(OpOutput {
                    text: String::new(),
                    data: None,
                })
            })
        }
    }

    // ── from_operation tests ──────────────────────────────────────────────────

    #[test]
    fn from_operation_returns_none_without_agent_tool_surface() {
        assert!(
            AgentToolAdapter::from_operation(Arc::new(NonAgentOp)).is_none(),
            "Op ohne Surface::AgentTool muss None liefern"
        );
    }

    #[test]
    fn from_operation_extracts_agent_tool_metadata() -> TestResult {
        let adapter = AgentToolAdapter::from_operation(Arc::new(AgentOp)).ok_or(
            TestError::Missing("Op mit AgentTool-Surface muss Some liefern"),
        )?;
        assert_eq!(adapter.child_name(), "researcher");
        assert_eq!(adapter.authority_reducer(), "reduce_ro");
        assert_eq!(adapter.budget_hint(), "8k");
        Ok(())
    }

    #[test]
    fn from_operation_operation_accessor_returns_correct_meta_name() -> TestResult {
        let adapter = AgentToolAdapter::from_operation(Arc::new(AgentOp))
            .ok_or(TestError::Missing("Erwartet Some"))?;
        assert_eq!(adapter.operation().meta().name, "spawn_x");
        Ok(())
    }

    // ── Budget-Parser ─────────────────────────────────────────────────────────

    #[test]
    fn test_parse_budget_hint_reads_every_unit() -> TestResult {
        let budget = parse_budget_hint("8k_tokens,20_tool_calls,30s")
            .map_err(crate::test_support::ctx("vollständiges Label ist gültig"))?;
        assert_eq!(budget.max_tokens, Some(8_000));
        assert_eq!(budget.max_tool_calls, Some(20));
        assert_eq!(budget.max_wall_time_ms, Some(30_000));
        assert_eq!(budget.reasoning_effort, None);
        Ok(())
    }

    #[test]
    fn test_parse_budget_hint_reads_millis_effort_and_mega_scale() -> TestResult {
        let budget = parse_budget_hint(" 500ms , effort=low , 1m_tokens ").map_err(
            crate::test_support::ctx("Whitespace und beliebige Reihenfolge sind erlaubt"),
        )?;
        assert_eq!(budget.max_wall_time_ms, Some(500));
        assert_eq!(budget.reasoning_effort, Some(ReasoningEffort::Low));
        assert_eq!(budget.max_tokens, Some(1_000_000));
        Ok(())
    }

    #[test]
    fn test_parse_budget_hint_scales_only_tokens() -> TestResult {
        let plain = parse_budget_hint("64000_tokens")
            .map_err(crate::test_support::ctx("Zahl ohne Suffix ist gültig"))?;
        assert_eq!(plain.max_tokens, Some(64_000));

        let scaled_tool_calls = parse_budget_hint("2k_tool_calls");
        assert!(
            matches!(scaled_tool_calls, Err(OpError::InvalidArguments(ref message)) if message.contains("tool_calls")),
            "der k/m-Multiplikator gilt nur für Tokens: {scaled_tool_calls:?}"
        );
        Ok(())
    }

    #[test]
    fn test_parse_budget_hint_rejects_unknown_segment() {
        let result = parse_budget_hint("8k_tokens,42_bananas");
        assert!(
            matches!(result, Err(OpError::InvalidArguments(ref message)) if message.contains("42_bananas")),
            "ein unbekanntes Segment darf nicht still ignoriert werden: {result:?}"
        );
    }

    #[test]
    fn test_parse_budget_hint_rejects_bare_number_without_unit() {
        // Das historische Label "8k" ist mehrdeutig und damit ungültig — es darf
        // nicht stillschweigend als Token-Grenze durchgehen.
        assert!(
            matches!(parse_budget_hint("8k"), Err(OpError::InvalidArguments(_))),
            "eine Zahl ohne Einheit ist kein Budget"
        );
    }

    #[test]
    fn test_parse_budget_hint_rejects_duplicate_dimension() {
        let result = parse_budget_hint("1k_tokens,2k_tokens");
        assert!(
            matches!(result, Err(OpError::InvalidArguments(ref message)) if message.contains("mehrfach")),
            "eine doppelt angegebene Dimension darf nicht last-wins überschrieben werden: {result:?}"
        );
    }

    #[test]
    fn test_parse_budget_hint_rejects_empty_segment_and_combined_unlimited() {
        assert!(
            matches!(
                parse_budget_hint("8k_tokens,"),
                Err(OpError::InvalidArguments(_))
            ),
            "ein abschließendes Komma ist ein Syntaxfehler"
        );
        assert!(
            matches!(
                parse_budget_hint("unlimited,30s"),
                Err(OpError::InvalidArguments(_))
            ),
            "`unlimited` darf nicht mit anderen Segmenten kombiniert werden"
        );
    }

    #[test]
    fn test_parse_budget_hint_rejects_unknown_effort_level() {
        let result = parse_budget_hint("effort=turbo");
        assert!(
            matches!(result, Err(OpError::InvalidArguments(ref message)) if message.contains("turbo")),
            "ein unbekanntes Effort-Level muss fail-closed abgewiesen werden: {result:?}"
        );
    }

    #[test]
    fn test_parse_budget_hint_empty_and_unlimited_default_to_no_limit() -> TestResult {
        for hint in ["", "   ", "unlimited", "UNLIMITED"] {
            let budget = parse_budget_hint(hint)
                .map_err(crate::test_support::ctx("Label ohne Grenzen ist gültig"))?;
            assert_eq!(budget, AgentBudget::default(), "Label {hint:?}");
            assert!(budget.max_tokens.is_none());
            assert!(budget.max_tool_calls.is_none());
            assert!(budget.max_wall_time_ms.is_none());
            assert!(budget.reasoning_effort.is_none());
        }
        Ok(())
    }

    // ── Budget-Verschärfung ───────────────────────────────────────────────────

    #[test]
    fn test_tighten_budget_keeps_the_stricter_limit_per_dimension() -> TestResult {
        let declared = parse_budget_hint("8k_tokens,20_tool_calls,60s,effort=high")
            .map_err(crate::test_support::ctx("gültiges Label"))?;
        let from_ir = parse_budget_hint("2k_tokens,64_tool_calls,30s,effort=low")
            .map_err(crate::test_support::ctx("gültiges Label"))?;

        let effective = tighten_budget(declared, from_ir);
        assert_eq!(effective.max_tokens, Some(2_000), "IR-Grenze ist strenger");
        assert_eq!(
            effective.max_tool_calls,
            Some(20),
            "Deklaration ist strenger"
        );
        assert_eq!(effective.max_wall_time_ms, Some(30_000));
        assert_eq!(effective.reasoning_effort, Some(ReasoningEffort::Low));
        Ok(())
    }

    #[test]
    fn test_tighten_budget_lets_none_lose_against_any_limit() -> TestResult {
        let limited =
            parse_budget_hint("4k_tokens").map_err(crate::test_support::ctx("gültiges Label"))?;
        let unlimited = AgentBudget::default();

        assert_eq!(
            tighten_budget(limited, unlimited).max_tokens,
            Some(4_000),
            "`None` darf eine bestehende Grenze nicht aufheben"
        );
        assert_eq!(
            tighten_budget(unlimited, limited).max_tokens,
            Some(4_000),
            "die Verschneidung ist kommutativ"
        );
        assert_eq!(
            tighten_budget(unlimited, unlimited),
            AgentBudget::default(),
            "ohne jede Grenze bleibt das Budget offen"
        );
        Ok(())
    }

    #[test]
    fn test_tighten_budget_is_idempotent() -> TestResult {
        let budget = parse_budget_hint("1k_tokens,5_tool_calls,10s")
            .map_err(crate::test_support::ctx("gültiges Label"))?;
        assert_eq!(tighten_budget(budget, budget), budget);
        Ok(())
    }

    // ── Return-Contract ───────────────────────────────────────────────────────

    #[test]
    fn test_child_return_contract_parse_maps_known_ids() {
        assert_eq!(
            ChildReturnContract::parse("harwness.return.research-finding@1"),
            Ok(ChildReturnContract::ResearchFinding)
        );
        assert_eq!(
            ChildReturnContract::parse("harwness.return.envelope@1"),
            Ok(ChildReturnContract::ReturnEnvelope)
        );
    }

    /// #22 Welle 1B: bekannte Text-Contracts werden als ungeprüfter Freitext
    /// durchgereicht, ein unbekanntes Label ist ein Fehler — kein stiller
    /// Rückfall mehr.
    #[test]
    fn test_child_return_contract_parse_is_strict() {
        for id in [
            "harwness.return.coding-task@1",
            "harwness.return.plan-proposal@1",
            "harwness.return.execution-summary@1",
            "harwness.matrix.umpire-ruling@1",
        ] {
            assert_eq!(
                ChildReturnContract::parse(id),
                Ok(ChildReturnContract::Text),
                "bekanntes Text-Label {id:?} behauptet keine Prüfung"
            );
        }
        for id in [
            "",
            "nonsense",
            // Eine hypothetische künftige Fassung des Verdict-Vertrags: wird
            // erkannt als "nicht v1", nicht geraten gegen das heutige Schema
            // geprüft — und nicht mehr still als Freitext durchgereicht.
            "harwness.security-verdict/v2",
        ] {
            let parsed = ChildReturnContract::parse(id);
            assert!(
                parsed.as_ref().is_err_and(|error| error.label == id.trim()),
                "unbekanntes Label {id:?} ist ein Fehler: {parsed:?}"
            );
        }
        // Jedes Label des IR-Vokabulars ist bekannt.
        for contract in harw_agent_dsl::ir_v2::ReturnContract::ALL {
            assert!(ChildReturnContract::parse(contract.as_str()).is_ok());
        }
    }

    #[test]
    fn test_child_return_contract_parse_maps_security_verdict() {
        // Der vierte Arm bricht die drei bestehenden Zuordnungen nicht (siehe
        // `test_child_return_contract_parse_maps_known_ids` oben, unverändert).
        assert_eq!(
            ChildReturnContract::parse("harwness.security-verdict/v1"),
            Ok(ChildReturnContract::SecurityVerdict)
        );
        assert_eq!(
            ChildReturnContract::SecurityVerdict.as_label(),
            "harwness.security-verdict/v1"
        );
    }

    #[test]
    fn test_contract_output_returns_canonical_json_for_a_valid_finding() -> TestResult {
        let output = contract_output(ChildReturnContract::ResearchFinding, VALID_FINDING);
        let value: serde_json::Value = serde_json::from_str(&output.text).map_err(
            crate::test_support::ctx("die Ausgabe muss gültiges JSON sein"),
        )?;

        assert_eq!(value["question_id"], "q-1");
        assert_eq!(value["conclusion"], "jiff 0.2.32 is current");
        assert!(
            value.get("error").is_none(),
            "ein gültiges Finding darf keinen Fehler melden"
        );
        assert!(
            !output.text.contains('\n'),
            "die Ausgabe muss die kanonische Serialisierung sein, nicht der Rohtext"
        );
        Ok(())
    }

    #[test]
    fn test_contract_output_accepts_a_fenced_finding() -> TestResult {
        let fenced = format!("```json\n{VALID_FINDING}\n```");
        let output = contract_output(ChildReturnContract::ResearchFinding, &fenced);
        let value: serde_json::Value = serde_json::from_str(&output.text).map_err(
            crate::test_support::ctx("die Ausgabe muss gültiges JSON sein"),
        )?;
        assert_eq!(value["produced_by"], "explorer-1");
        Ok(())
    }

    #[test]
    fn test_contract_output_reports_garbage_as_structured_error_output() -> TestResult {
        let output = contract_output(
            ChildReturnContract::ResearchFinding,
            "Ich habe nachgesehen, jiff sieht aktuell aus.",
        );
        let value: serde_json::Value = serde_json::from_str(&output.text)
            .map_err(crate::test_support::ctx("auch der Fehlerfall ist JSON"))?;

        assert!(
            value["error"].as_str().is_some_and(|m| !m.is_empty()),
            "der Vertragsbruch muss benannt werden: {value}"
        );
        assert_eq!(
            value["raw"], "Ich habe nachgesehen, jiff sieht aktuell aus.",
            "der Rohtext muss erhalten bleiben, damit das Parent nachsteuern kann"
        );
        assert_eq!(value["contract"], "harwness.return.research-finding@1");
        Ok(())
    }

    #[test]
    fn test_contract_output_reports_an_invalid_finding_as_structured_error_output() -> TestResult {
        // Syntaktisch gültiges JSON, das die Vertragsregel „ab Confidence
        // Medium sind Belege Pflicht" verletzt.
        let no_evidence = r#"{"question_id":"q-2","conclusion":"vermutlich aktuell",
            "evidence":[],"confidence":"high","produced_by":"explorer-2",
            "produced_at":"2026-08-27T00:00:00Z"}"#;
        let output = contract_output(ChildReturnContract::ResearchFinding, no_evidence);
        let value: serde_json::Value = serde_json::from_str(&output.text)
            .map_err(crate::test_support::ctx("auch der Fehlerfall ist JSON"))?;
        assert!(
            value["error"]
                .as_str()
                .is_some_and(|m| m.contains("Recherche-Vertrag")),
            "die Validierung muss vom Parsen unterscheidbar gemeldet werden: {value}"
        );
        Ok(())
    }

    #[test]
    fn test_contract_output_validates_a_return_envelope() -> TestResult {
        let envelope = r#"{"agent_id":"explorer-1","outcome":"success",
            "summary":"fertig","payload":{"k":1}}"#;
        let output = contract_output(ChildReturnContract::ReturnEnvelope, envelope);
        let value: serde_json::Value = serde_json::from_str(&output.text).map_err(
            crate::test_support::ctx("die Ausgabe muss gültiges JSON sein"),
        )?;
        assert_eq!(value["agent_id"], "explorer-1");
        assert_eq!(value["outcome"], "success");
        Ok(())
    }

    #[test]
    fn test_contract_output_keeps_text_contract_verbatim() {
        let output = contract_output(ChildReturnContract::Text, "freie Antwort des Kindes");
        assert_eq!(
            output.text, "freie Antwort des Kindes",
            "der Text-Contract darf nichts umformen"
        );
    }

    /// Ein gültiges `SecurityVerdict` als Kind-Antwort.
    const VALID_VERDICT: &str = r#"{
        "contract": "harwness.security-verdict/v1",
        "finding": "finding-1",
        "bound_evidence": "0000000000000000000000000000000000000000000000000000000000000000",
        "classification": "suspicious",
        "severity": "medium",
        "rationale": "ungewöhnliche Prozesskette beobachtet",
        "suggested_response": "escalate",
        "issued_by": "security-triage-1",
        "issued_at": "2026-08-27T00:00:00Z"
    }"#;

    #[test]
    fn test_contract_output_validates_a_security_verdict() -> TestResult {
        let output = contract_output(ChildReturnContract::SecurityVerdict, VALID_VERDICT);
        let value: serde_json::Value = serde_json::from_str(&output.text).map_err(
            crate::test_support::ctx("die Ausgabe muss gültiges JSON sein"),
        )?;
        assert_eq!(value["classification"], "suspicious");
        assert_eq!(value["issued_by"], "security-triage-1");
        assert!(
            value.get("error").is_none(),
            "ein gültiges Verdikt darf keinen Fehler melden"
        );
        Ok(())
    }

    #[test]
    fn test_contract_output_rejects_wrong_verdict_contract_version_without_content() -> TestResult {
        let v2 = VALID_VERDICT.replace(
            "harwness.security-verdict/v1",
            "harwness.security-verdict/v2",
        );
        let output = contract_output(ChildReturnContract::SecurityVerdict, &v2);
        let value: serde_json::Value = serde_json::from_str(&output.text)
            .map_err(crate::test_support::ctx("auch der Fehlerfall ist JSON"))?;
        assert!(
            value["error"].as_str().is_some_and(|m| !m.is_empty()),
            "der Vertragsbruch muss benannt werden: {value}"
        );
        assert_eq!(value["contract"], "harwness.security-verdict/v1");
        Ok(())
    }

    #[test]
    fn test_contract_output_reports_malformed_verdict_without_raw_text() -> TestResult {
        // Der zentrale Unterschied zu den beiden anderen typisierten Armen:
        // kein `"raw"`-Feld und die eingebettete Fehlermeldung enthält den
        // Rohtext nicht — ein Verdikt sagt bei Ablehnung "dass", nicht "was".
        let attacker_text = "SECRET_PROCESS_NAME_do_not_log_me";
        let output = contract_output(ChildReturnContract::SecurityVerdict, attacker_text);
        let value: serde_json::Value = serde_json::from_str(&output.text)
            .map_err(crate::test_support::ctx("auch der Fehlerfall ist JSON"))?;
        assert!(
            value.get("raw").is_none(),
            "ein Verdict-Vertragsbruch darf keinen Rohtext-Auszug enthalten: {value}"
        );
        assert!(
            !output.text.contains("SECRET_PROCESS_NAME_do_not_log_me"),
            "die Ablehnung darf den angreiferkontrollierten Rohtext nicht zitieren: {value}"
        );
        assert_eq!(value["contract"], "harwness.security-verdict/v1");
        Ok(())
    }

    #[test]
    fn test_contract_output_rejects_a_verdict_with_unknown_field() -> TestResult {
        // `deny_unknown_fields` (K19): ein zusätzliches Feld macht das
        // Verdikt ungültig, statt es stillschweigend zu ignorieren.
        let with_extra = VALID_VERDICT.replace(
            "\"issued_by\": \"security-triage-1\",",
            "\"issued_by\": \"security-triage-1\", \"extra\": true,",
        );
        let output = contract_output(ChildReturnContract::SecurityVerdict, &with_extra);
        let value: serde_json::Value = serde_json::from_str(&output.text)
            .map_err(crate::test_support::ctx("auch der Fehlerfall ist JSON"))?;
        assert!(value["error"].as_str().is_some_and(|m| !m.is_empty()));
        Ok(())
    }

    /// Kein Weg von einem validierten Verdikt zu einer autorisierten Aktion.
    ///
    /// Strukturell (siehe `harw-dod-signals/src/verdict.rs`-Moduldoku,
    /// Abschnitt „Warum ein Verdikt nichts auslöst"): weder diese Crate noch
    /// `harw-dod-signals` hängen von `harw-dod-warden-proto` ab (siehe beider
    /// `Cargo.toml`) — `WardenAction` ist an keiner der beiden Stellen
    /// nennbar. Dieser Test belegt den einzigen Vorschlagskanal, den es
    /// überhaupt gibt: `suggested_response` serialisiert als bloßer
    /// Kategorie-String ohne Ziel (kein `cgroup`, kein `pid`, kein Host) —
    /// selbst ein Aufrufer, der eine Übersetzung versuchen wollte, fände
    /// hier keine Zielangaben, aus denen sich eine Aktion zusammensetzen
    /// ließe.
    #[test]
    fn test_security_verdict_output_carries_no_action_target() -> TestResult {
        let output = contract_output(ChildReturnContract::SecurityVerdict, VALID_VERDICT);
        let value: serde_json::Value = serde_json::from_str(&output.text).map_err(
            crate::test_support::ctx("die Ausgabe muss gültiges JSON sein"),
        )?;
        assert_eq!(value["suggested_response"], "escalate");
        for forbidden in ["cgroup", "pid", "process_id", "host", "action", "target"] {
            assert!(
                value.get(forbidden).is_none(),
                "ein Verdikt darf kein Aktionsziel '{forbidden}' tragen: {value}"
            );
        }
        Ok(())
    }

    #[test]
    fn test_security_verdict_arm_does_not_disturb_the_three_existing_arms() -> TestResult {
        // Additivitätsbeleg: dieselben drei bestehenden Zuordnungen liefern
        // nach Einführung des vierten Arms noch dieselben Ergebnisse.
        let finding_output = contract_output(ChildReturnContract::ResearchFinding, VALID_FINDING);
        assert!(
            serde_json::from_str::<serde_json::Value>(&finding_output.text)
                .map_err(crate::test_support::ctx("json"))?
                .get("error")
                .is_none()
        );

        let envelope = r#"{"agent_id":"explorer-1","outcome":"success",
            "summary":"fertig","payload":{"k":1}}"#;
        let envelope_output = contract_output(ChildReturnContract::ReturnEnvelope, envelope);
        assert!(
            serde_json::from_str::<serde_json::Value>(&envelope_output.text)
                .map_err(crate::test_support::ctx("json"))?
                .get("error")
                .is_none()
        );

        let text_output = contract_output(ChildReturnContract::Text, "unverändert");
        assert_eq!(text_output.text, "unverändert");
        Ok(())
    }

    #[test]
    fn test_resolve_child_contract_defaults_to_text_without_registry_factory() -> TestResult {
        let (ctx, tmp) = make_test_ctx()?;
        let contract = resolve_child_contract(&ctx, "researcher");
        std::fs::remove_dir_all(tmp).ok();
        assert_eq!(
            contract,
            Ok(ChildReturnContract::Text),
            "ohne Registry-Factory gibt es keine IR und damit keine Vertragsbehauptung"
        );
        Ok(())
    }

    #[test]
    fn test_resolve_child_contract_reads_the_agent_ir() -> TestResult {
        let factory: Arc<dyn ChildRegistryFactory> = Arc::new(IrRegistryFactory {
            ir: ir_with_contract("harwness.return.research-finding@1")?,
        });
        let mut services = ServiceMap::new();
        services.insert(factory);
        let (ctx, tmp) = make_test_ctx_with(services)?;

        let contract = resolve_child_contract(&ctx, "explorer");
        std::fs::remove_dir_all(tmp).ok();
        assert_eq!(contract, Ok(ChildReturnContract::ResearchFinding));
        Ok(())
    }

    #[test]
    fn test_resolve_child_contract_reads_the_security_verdict_ir_label() -> TestResult {
        let factory: Arc<dyn ChildRegistryFactory> = Arc::new(IrRegistryFactory {
            ir: ir_with_contract(ChildReturnContract::SECURITY_VERDICT_ID)?,
        });
        let mut services = ServiceMap::new();
        services.insert(factory);
        let (ctx, tmp) = make_test_ctx_with(services)?;

        let contract = resolve_child_contract(&ctx, "security-triage-1");
        std::fs::remove_dir_all(tmp).ok();
        assert_eq!(contract, Ok(ChildReturnContract::SecurityVerdict));
        Ok(())
    }

    #[test]
    fn test_resolve_child_contract_passes_a_known_text_contract_through() -> TestResult {
        let factory: Arc<dyn ChildRegistryFactory> = Arc::new(IrRegistryFactory {
            ir: ir_with_contract("harwness.return.coding-task@1")?,
        });
        let mut services = ServiceMap::new();
        services.insert(factory);
        let (ctx, tmp) = make_test_ctx_with(services)?;

        let contract = resolve_child_contract(&ctx, "coder");
        std::fs::remove_dir_all(tmp).ok();
        assert_eq!(
            contract,
            Ok(ChildReturnContract::Text),
            "ein bekannter Text-Contract wird nicht strukturell geprüft"
        );
        Ok(())
    }

    /// #22 Welle 1B: die Rückgabe-Validatoren kommen aus der IR des Kindes,
    /// in Deklarationsreihenfolge; ohne Factory gibt es keine.
    #[test]
    fn test_resolve_child_validators_reads_the_agent_ir() -> TestResult {
        let (ctx, tmp) = make_test_ctx()?;
        assert!(resolve_child_validators(&ctx, "researcher").is_empty());
        std::fs::remove_dir_all(tmp).ok();

        let factory: Arc<dyn ChildRegistryFactory> = Arc::new(IrRegistryFactory {
            ir: ir_with_return(
                "contract = \"harwness.return.research-finding@1\"\n\
                 validators = [\"non-empty\", \"json-object\"]",
            )?,
        });
        let mut services = ServiceMap::new();
        services.insert(factory);
        let (ctx, tmp) = make_test_ctx_with(services)?;
        let validators = resolve_child_validators(&ctx, "explorer");
        std::fs::remove_dir_all(tmp).ok();
        assert_eq!(validators, ["non-empty", "json-object"]);
        // Ein gültiges Finding besteht beide Validatoren, Freitext nicht.
        assert!(run_return_validators(&validators, VALID_FINDING).is_ok());
        assert!(run_return_validators(&validators, "nur Freitext").is_err());
        Ok(())
    }

    // ── Pause-Behandlung ──────────────────────────────────────────────────────

    #[test]
    fn test_paused_child_fails_closed_with_role_and_outcome() -> TestResult {
        let child = SessionId::new();
        let outcome = TurnOutcome::AwaitingApproval {
            call_id: ToolCallId::new(),
            request: ItemId::new(),
        };
        let result = paused_child_result(PauseKind::Approval, &child, "explorer", false, &outcome);

        match result {
            Err(OpError::NotAvailable(message)) => {
                assert!(message.contains("explorer"), "Rolle fehlt: {message}");
                assert!(message.contains(child.as_str()), "Kind-ID fehlt: {message}");
                assert!(
                    message.contains("AwaitingApproval"),
                    "Outcome fehlt: {message}"
                );
                assert!(
                    message.contains("allow_pause = false"),
                    "der Hinweis auf die Pause-Sperre fehlt: {message}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "Pause ohne Erlaubnis muss fail-closed sein, war: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_paused_child_reports_a_permitted_pause_as_structured_output() -> TestResult {
        let child = SessionId::new();
        let outcome = TurnOutcome::AwaitingChild {
            child: SessionId::new(),
            call_id: ToolCallId::new(),
            role: "grandchild".to_owned(),
        };
        let output = paused_child_result(PauseKind::Child, &child, "planner", true, &outcome)
            .map_err(crate::test_support::ctx(
                "eine erlaubte Pause ist kein Fehler",
            ))?;
        let value: serde_json::Value = serde_json::from_str(&output.text)
            .map_err(crate::test_support::ctx("der Pause-Report ist JSON"))?;

        assert_eq!(value["paused"], "child");
        assert_eq!(value["child"], child.as_str());
        Ok(())
    }

    // ── Reparatur-Turn: Belegungsprüfung und Ein-Versuch-Reparatur ───────────
    //
    // Ein voller Spawn-plus-Lauf-Beweis (echtes Kind über `spawn_child`
    // admittiert und über `run_child_with_budget` zweimal gefahren) bräuchte
    // eine registrierte Eltern-`AgentSession` samt `SpawnContext`
    // (`organizational_role`, `allowed_child_orchestrators`, Sandbox, …) oder
    // einen `with_external_root_parent`-Aufbau — dieselbe Maschinerie, die
    // laut Kommentar bei `fanout_children_uia_worker_cap_is_reachable_from_this_crate`
    // (oben) bewusst in `harw-core/src/child_controller.rs`s eigener
    // Testsuite lebt, nicht hier. Die folgenden Tests decken stattdessen jede
    // neue Entscheidung (Belegungsprüfung, Contract-Auswertung,
    // Reparatur-Prompt, Reparatur-Anstoß) isoliert über [`InMemoryStateStore`]
    // und einen ungebundenen [`ManagedAgentSpawner`] ab, ohne einen echten
    // Kind-Lauf zu benötigen.

    /// Echo des eingebetteten JSON-Schemas selbst statt einer ausgefüllten
    /// Instanz — das Ausgangsproblem dieses Auftrags (u. a. Modell `glm-5.3`).
    const SCHEMA_ECHO_TEXT: &str = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema",
        "title":"ResearchFinding","type":"object"}"#;

    /// Ein gültiges `ResearchFinding` mit einem *nicht* lokalen Beleg
    /// (`kind: "web"`) — löst die Belegungsprüfung nicht aus.
    const VALID_FINDING_WITHOUT_LOCAL_EVIDENCE: &str = r#"{"question_id":"q-1","conclusion":"c",
        "evidence":[{"kind":"web","locator":"https://example.com",
        "retrieved_at":"2026-08-27T00:00:00Z"}],"confidence":"high","produced_by":"explorer-1",
        "produced_at":"2026-08-27T00:00:00Z"}"#;

    #[test]
    fn test_claims_local_evidence_true_only_for_grounded_kinds() {
        let local = serde_json::json!({ "evidence": [{ "kind": "local_source" }] });
        let registry = serde_json::json!({ "evidence": [{ "kind": "cargo_registry_source" }] });
        let web = serde_json::json!({ "evidence": [{ "kind": "web" }] });
        let empty = serde_json::json!({ "evidence": [] });
        let missing = serde_json::json!({});

        assert!(claims_local_evidence(&local));
        assert!(claims_local_evidence(&registry));
        let package = serde_json::json!({ "evidence": [{ "kind": "package_registry_source" }] });
        assert!(claims_local_evidence(&package));
        assert!(!claims_local_evidence(&web));
        assert!(!claims_local_evidence(&empty));
        assert!(!claims_local_evidence(&missing));
    }

    #[tokio::test]
    async fn test_count_child_tool_calls_counts_only_tool_call_items() -> TestResult {
        let store = InMemoryStateStore::new();
        let child = SessionId::new();
        let mut history = ConversationHistory::new();
        history.push_user_text("frage");
        history.push_tool_call(ToolCallId::new(), "fs.read", serde_json::Value::Null);
        history.push_tool_call(ToolCallId::new(), "fs.grep", serde_json::Value::Null);
        history.push_assistant_text("antwort", None);
        store
            .save_history(&child, &history)
            .await
            .map_err(crate::test_support::ctx(
                "Historie muss sich speichern lassen",
            ))?;

        let count =
            count_child_tool_calls(&store, &child)
                .await
                .map_err(crate::test_support::ctx(
                    "die Zählung darf bei einem funktionierenden Store nicht fehlschlagen",
                ))?;
        assert_eq!(count, 2, "nur ToolCall-Items dürfen gezählt werden");
        Ok(())
    }

    #[tokio::test]
    async fn test_count_child_tool_calls_is_zero_for_an_untouched_child() -> TestResult {
        let store = InMemoryStateStore::new();
        let child = SessionId::new();

        let count =
            count_child_tool_calls(&store, &child)
                .await
                .map_err(crate::test_support::ctx(
                    "ein leerer Verlauf ist kein Store-Fehler",
                ))?;
        assert_eq!(count, 0);
        Ok(())
    }

    #[tokio::test]
    async fn test_evaluate_return_with_grounding_rejects_schema_echo_as_contract_violation()
    -> TestResult {
        let store = InMemoryStateStore::new();
        let child = SessionId::new();

        let Err(violation) = evaluate_return_with_grounding(
            &store,
            ChildReturnContract::ResearchFinding,
            &child,
            SCHEMA_ECHO_TEXT,
        )
        .await
        else {
            return Err(TestError::Unexpected(
                "ein Schema-Echo ist kein gültiges ResearchFinding".into(),
            ));
        };

        assert!(
            violation
                .message
                .contains("kein gültiges ResearchFinding-JSON"),
            "{}",
            violation.message
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_evaluate_return_with_grounding_rejects_local_evidence_without_a_tool_call()
    -> TestResult {
        let store = InMemoryStateStore::new();
        let child = SessionId::new();

        // `VALID_FINDING` (oben) behauptet einen cargo_registry_source-Beleg;
        // die Kind-Session hat hier keinen einzigen Werkzeugaufruf ausgeführt.
        let Err(violation) = evaluate_return_with_grounding(
            &store,
            ChildReturnContract::ResearchFinding,
            &child,
            VALID_FINDING,
        )
        .await
        else {
            return Err(TestError::Unexpected(
                "ein behaupteter lokaler Beleg ohne Werkzeugaufruf ist ein Vertragsbruch".into(),
            ));
        };

        assert!(
            violation.message.contains("Befund ohne Werkzeugaufruf"),
            "{}",
            violation.message
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_evaluate_return_with_grounding_accepts_local_evidence_with_a_tool_call()
    -> TestResult {
        let store = InMemoryStateStore::new();
        let child = SessionId::new();
        let mut history = ConversationHistory::new();
        history.push_tool_call(ToolCallId::new(), "fs.read", serde_json::Value::Null);
        store
            .save_history(&child, &history)
            .await
            .map_err(crate::test_support::ctx(
                "Historie muss sich speichern lassen",
            ))?;

        let value = evaluate_return_with_grounding(
            &store,
            ChildReturnContract::ResearchFinding,
            &child,
            VALID_FINDING,
        )
        .await
        .map_err(|violation| {
            TestError::Unexpected(format!(
                "ein belegter Werkzeugaufruf darf nicht abgelehnt werden: {}",
                violation.message
            ))
        })?;
        assert_eq!(value["question_id"], "q-1");
        Ok(())
    }

    #[tokio::test]
    async fn test_evaluate_return_with_grounding_accepts_non_local_evidence_without_a_tool_call()
    -> TestResult {
        let store = InMemoryStateStore::new();
        let child = SessionId::new();

        let value = evaluate_return_with_grounding(
            &store,
            ChildReturnContract::ResearchFinding,
            &child,
            VALID_FINDING_WITHOUT_LOCAL_EVIDENCE,
        )
        .await
        .map_err(|violation| {
            TestError::Unexpected(format!(
                "Web-Belege verlangen keinen Werkzeugaufruf: {}",
                violation.message
            ))
        })?;
        assert_eq!(value["question_id"], "q-1");
        Ok(())
    }

    #[test]
    fn test_repair_prompt_uses_the_short_message_and_states_the_instance_only_rule() {
        let violation = ContractViolation::new(
            ChildReturnContract::ResearchFinding,
            "kurzer Testfehler".to_owned(),
            "roher Kindtext, der nicht im Reparatur-Prompt auftauchen soll",
        );
        let prompt = repair_prompt(&violation);

        assert!(prompt.contains("Deine Antwort verletzt den Vertrag: kurzer Testfehler"));
        assert!(prompt.contains("kein Schema, kein Tool-Call-Text"));
        assert!(
            !prompt.contains("roher Kindtext"),
            "der Reparatur-Prompt soll die kurze Meldung nutzen, nicht den \
             Rohtext-Auszug aus `to_message`: {prompt}"
        );
    }

    #[tokio::test]
    async fn test_evaluate_with_repair_returns_immediately_when_the_first_attempt_is_valid()
    -> TestResult {
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let spawner = ManagedAgentSpawner::new(
            Arc::new(Mutex::new(SessionManager::new(event_tx))),
            ChildLimits::conservative(),
        );
        let store = InMemoryStateStore::new();
        let child = SessionId::new();

        let value = evaluate_with_repair(
            &spawner,
            &store,
            ChildReturnContract::ResearchFinding,
            &child,
            AgentBudget::default(),
            VALID_FINDING_WITHOUT_LOCAL_EVIDENCE,
        )
        .await
        .map_err(crate::test_support::ctx(
            "ein bereits gültiges Finding braucht keine Reparatur",
        ))?;
        assert_eq!(value["question_id"], "q-1");

        // Kein Kind wurde admittiert: hätte `evaluate_with_repair` den
        // Reparatur-Pfad genommen, wäre `run_child_with_budget` an der
        // fehlenden Admission gescheitert (siehe Test unten). Dass hier kein
        // Fehler auftrat, belegt den sofortigen Rückweg ohne Reparatur-Turn.
        assert_eq!(spawner.child_record(&child), None);
        Ok(())
    }

    #[tokio::test]
    async fn test_evaluate_with_repair_attempts_one_repair_turn_and_reports_it_on_failure()
    -> TestResult {
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let spawner = ManagedAgentSpawner::new(
            Arc::new(Mutex::new(SessionManager::new(event_tx))),
            ChildLimits::conservative(),
        );
        let store = InMemoryStateStore::new();
        // Absichtlich nie admittiert: der Reparatur-Turn muss dennoch
        // *versucht* werden (Beweis, dass `evaluate_with_repair` nach dem
        // ersten Vertragsbruch tatsächlich einen zweiten Turn anstößt), er
        // scheitert hier lediglich an der fehlenden Admission — das reicht,
        // um den "nach 1 Reparaturversuch"-Pfad zu belegen.
        let child = SessionId::new();

        let Err(error) = evaluate_with_repair(
            &spawner,
            &store,
            ChildReturnContract::ResearchFinding,
            &child,
            AgentBudget::default(),
            SCHEMA_ECHO_TEXT,
        )
        .await
        else {
            return Err(TestError::Unexpected(
                "ein Schema-Echo ohne erfolgreiche Reparatur bleibt ein Fehler".into(),
            ));
        };

        assert!(error.contains("nach 1 Reparaturversuch"), "{error}");
        assert!(error.contains("Reparatur-Turn fehlgeschlagen"), "{error}");
        assert!(
            error.contains("kein gültiges ResearchFinding-JSON"),
            "{error}"
        );
        Ok(())
    }

    // ── invoke tests ──────────────────────────────────────────────────────────
    //
    // `make_test_ctx()` registriert bewusst keinen `ManagedAgentSpawner`/
    // `StateStore` in der `ServiceMap`, sodass `ctx.managed_spawner()` und
    // `ctx.state_store()` `None` liefern und `invoke()` den frühen
    // `NotAvailable`-Pfad nimmt. Die Contract- und Pause-Mapper-Tests decken die
    // modell-sichtbare Rückgabe isoliert ab.

    #[test]
    fn completed_child_output_returns_the_child_final_response() -> TestResult {
        let output = completed_child_output(Ok("final child response".to_owned())).map_err(
            crate::test_support::ctx("available child response must produce tool output"),
        )?;

        assert_eq!(output.text, "final child response");
        assert!(
            !output.text.contains("hat den Turn abgeschlossen"),
            "die Tool-Antwort darf keine generische Abschlussbestätigung sein"
        );
        Ok(())
    }

    #[test]
    fn completed_child_output_maps_unavailable_response_to_not_available() {
        let result = completed_child_output(Err(harw_extension_api::AgentSpawnError {
            message: "no assistant response text".to_owned(),
        }));

        assert!(
            matches!(result, Err(OpError::NotAvailable(message)) if message.contains("Abschlussantwort nicht verfügbar")),
            "fehlender Child-Text muss fail-closed als NotAvailable enden"
        );
    }

    #[tokio::test]
    async fn invoke_returns_not_available_in_skeleton_phase() -> TestResult {
        let adapter = AgentToolAdapter::from_operation(Arc::new(AgentOp))
            .ok_or(TestError::Missing("Erwartet Some"))?;
        let (ctx, tmp) = make_test_ctx()?;
        let result = adapter
            .invoke(&ctx, serde_json::json!({ "topic": "Rust" }))
            .await;
        std::fs::remove_dir_all(tmp).ok();
        match result {
            Err(OpError::NotAvailable(msg)) => {
                assert!(
                    msg.contains("kein Agent-Spawner"),
                    "Fehlermeldung sollte 'kein Agent-Spawner' enthalten, war: {msg}"
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "Erwartet OpError::NotAvailable, war: {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[tokio::test]
    async fn invoke_rejects_an_undecodable_budget_hint_before_spawning() -> TestResult {
        // `AgentOp` trägt das historische Label "8k"; mit vollständiger
        // Core-Laufzeit im Kontext ist der Budget-Parser die nächste Grenze —
        // und sie muss fail-closed sein statt „kein Limit" zu bedeuten.
        let adapter = AgentToolAdapter::from_operation(Arc::new(AgentOp))
            .ok_or(TestError::Missing("Erwartet Some"))?;
        let (services, _events) = services_with_runtime();
        let (ctx, tmp) = make_test_ctx_with(services)?;
        let result = adapter
            .invoke(&ctx, serde_json::json!({ "topic": "Rust" }))
            .await;
        std::fs::remove_dir_all(tmp).ok();

        assert!(
            matches!(result, Err(OpError::NotAvailable(ref message)) if message.contains("budget_hint")),
            "ein unlesbares Budget-Label muss das Werkzeug sperren: {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn invoke_rejects_non_object_args_before_child_scheduling() -> TestResult {
        let adapter = AgentToolAdapter::from_operation(Arc::new(AgentOp))
            .ok_or(TestError::Missing("Erwartet Some"))?;
        for args in [
            serde_json::Value::Null,
            serde_json::json!("not-an-object"),
            serde_json::json!(["not-an-object"]),
        ] {
            let (ctx, tmp) = make_test_ctx()?;
            let result = adapter.invoke(&ctx, args).await;
            std::fs::remove_dir_all(tmp).ok();
            assert!(
                matches!(&result, Err(OpError::InvalidArguments(message)) if message == "arguments must be a JSON object"),
                "nicht-objektartige Args müssen vor dem Scheduling deterministisch abgelehnt werden: {result:?}"
            );
        }
        Ok(())
    }

    // ── /agent product boundary tests ───────────────────────────────────────

    #[tokio::test]
    async fn agent_product_rejects_missing_or_unknown_action_before_runtime_lookup() -> TestResult {
        for args in [
            serde_json::json!({}),
            serde_json::json!({ "action": "resume" }),
            serde_json::json!("list"),
        ] {
            let (ctx, tmp) = make_test_ctx()?;
            let result = AgentToolAdapter::invoke_product(&ctx, args).await;
            std::fs::remove_dir_all(tmp).ok();
            assert!(
                matches!(result, Err(OpError::InvalidArguments(_))),
                "invalid /agent request must be rejected before runtime dispatch: {result:?}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn agent_product_rejects_missing_or_empty_target_before_runtime_lookup() -> TestResult {
        for args in [
            serde_json::json!({ "action": "stop" }),
            serde_json::json!({ "action": "budget", "target": "", "budget": { "max_tokens": 1 } }),
        ] {
            let (ctx, tmp) = make_test_ctx()?;
            let result = AgentToolAdapter::invoke_product(&ctx, args).await;
            std::fs::remove_dir_all(tmp).ok();
            assert!(
                matches!(result, Err(OpError::InvalidArguments(_))),
                "missing or empty targets must fail closed: {result:?}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn agent_product_rejects_list_extra_arguments() -> TestResult {
        let (ctx, tmp) = make_test_ctx()?;
        let result = AgentToolAdapter::invoke_product(
            &ctx,
            serde_json::json!({ "action": "list", "target": "foreign-child" }),
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        assert!(
            matches!(result, Err(OpError::InvalidArguments(ref message)) if message.contains("list does not accept")),
            "list must not accept arbitrary target probes: {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn agent_product_rejects_untyped_budget_before_runtime_lookup() -> TestResult {
        let (ctx, tmp) = make_test_ctx()?;
        let result = AgentToolAdapter::invoke_product(
            &ctx,
            serde_json::json!({
                "action": "budget",
                "target": "child-a",
                "budget": { "max_tokens": "many" }
            }),
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        assert!(
            matches!(result, Err(OpError::InvalidArguments(ref message)) if message.contains("max_tokens")),
            "budget limits must be typed before a spawner is consulted: {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn agent_product_reports_missing_spawner_without_success_output() -> TestResult {
        let (ctx, tmp) = make_test_ctx()?;
        let result =
            AgentToolAdapter::invoke_product(&ctx, serde_json::json!({ "action": "list" })).await;
        std::fs::remove_dir_all(tmp).ok();

        assert!(
            matches!(result, Err(OpError::NotAvailable(ref message)) if message.contains("kein Agent-Spawner")),
            "a missing runtime spawner must not become a synthetic /agent success: {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn agent_product_list_reports_an_empty_child_set_as_json() -> TestResult {
        let (services, _events) = services_with_runtime();
        let (ctx, tmp) = make_test_ctx_with(services)?;
        let result =
            AgentToolAdapter::invoke_product(&ctx, serde_json::json!({ "action": "list" })).await;
        std::fs::remove_dir_all(tmp).ok();

        let output = result.map_err(crate::test_support::ctx(
            "mit Spawner ist /agent list verfügbar",
        ))?;
        let value: serde_json::Value = serde_json::from_str(&output.text)
            .map_err(crate::test_support::ctx("die Liste ist JSON"))?;
        assert_eq!(
            value["children"].as_array().map(Vec::len),
            Some(0),
            "ohne admittierte Kinder ist die Liste leer, nicht 'nicht verfügbar': {}",
            output.text
        );
        Ok(())
    }

    #[tokio::test]
    async fn agent_product_stop_on_a_foreign_child_stays_fail_closed() -> TestResult {
        let (services, _events) = services_with_runtime();
        let (ctx, tmp) = make_test_ctx_with(services)?;
        let result = AgentToolAdapter::invoke_product(
            &ctx,
            serde_json::json!({ "action": "stop", "target": "foreign-child" }),
        )
        .await;
        std::fs::remove_dir_all(tmp).ok();

        assert!(
            matches!(result, Err(OpError::NotAvailable(ref message)) if message == "agent target is unavailable in this parent session"),
            "ein fremdes Kind darf weder gestoppt noch als existent bestätigt werden: {result:?}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn agent_product_budget_on_a_foreign_child_stays_fail_closed() -> TestResult {
        for args in [
            serde_json::json!({ "action": "budget", "target": "foreign-child" }),
            serde_json::json!({
                "action": "budget",
                "target": "foreign-child",
                "budget": { "max_tokens": 10 }
            }),
        ] {
            let (services, _events) = services_with_runtime();
            let (ctx, tmp) = make_test_ctx_with(services)?;
            let result = AgentToolAdapter::invoke_product(&ctx, args).await;
            std::fs::remove_dir_all(tmp).ok();

            assert!(
                matches!(result, Err(OpError::NotAvailable(ref message)) if message == "agent target is unavailable in this parent session"),
                "lesen wie setzen bleiben an der Besitzgrenze: {result:?}"
            );
        }
        Ok(())
    }

    // ── W4a/A-BRIDGE: K3 Reducer, K4 question_id, K5 Effort ──────────────────

    /// Alle sieben Permissions (Stand `harw-sandbox`).
    const ALL_PERMISSIONS: [Permission; 7] = [
        Permission::ReadWorkspace,
        Permission::WriteWorkspace,
        Permission::ExecuteProcess,
        Permission::NetworkAccess,
        Permission::ReadSecrets,
        Permission::ManagePlugins,
        Permission::ReadCargoRegistry,
    ];

    /// Alle 2⁷ Teilmengen der Permissions.
    fn every_permission_subset() -> Vec<PermissionSet> {
        (0_u32..(1 << ALL_PERMISSIONS.len()))
            .map(|mask| {
                PermissionSet::from_policy(
                    ALL_PERMISSIONS
                        .iter()
                        .enumerate()
                        .filter(|(bit, _)| mask & (1 << bit) != 0)
                        .map(|(_, permission)| *permission),
                )
            })
            .collect()
    }

    #[test]
    fn test_resolve_authority_reducer_never_widens_any_parent_permission_set() -> TestResult {
        let (ctx, tmp) = make_test_ctx()?;
        let binding = ctx.sandbox().workspace().clone();
        std::fs::remove_dir_all(tmp).ok();
        let names = KNOWN_AUTHORITY_REDUCERS.iter().copied().chain([
            "reduce_ro",
            "",
            "reduce_to_everything",
        ]);
        for name in names {
            let reducer = resolve_authority_reducer(name);
            let ceiling = reducer_ceiling(name)
                .or_else(|| reducer_ceiling("reduce_to_read_only"))
                .ok_or(TestError::Missing("read_only ist bekannt"))?;
            for granted in every_permission_subset() {
                // `SandboxSpec` hat keinen `with_network_scope`-Setter mehr: die reale API
                // erlaubt außerhalb von harw-authority nur `NetworkScope::empty()` über
                // `from_resolved`; ein nicht-leerer Scope entsteht ausschließlich über
                // `PolicyBootstrap::issue` gegen eine echte Policy-Datei
                // (harw-authority/src/lib.rs:591-599, :849). Diese Prüfung testet nur die
                // Permission-Monotonie, nicht den Netz-Scope — der leere Scope aus
                // `from_resolved` genügt dafür unverändert.
                let parent = SandboxSpec::from_resolved(binding.clone(), granted.clone());
                let child = reducer(&parent);
                assert!(
                    child.permissions().is_subset_of(parent.permissions()),
                    "{name}: Kind {:?} erweitert Parent {:?}",
                    child.permissions(),
                    parent.permissions()
                );
                assert!(
                    child.permissions().is_subset_of(&ceiling),
                    "{name}: Kind {:?} überschreitet die Obergrenze {ceiling:?}",
                    child.permissions()
                );
                // `PermissionSet::intersection` existiert nicht (bewusst: die einzige
                // Verengungsoperation ist `restrict`/`is_subset_of`). Der Schnitt wird
                // hier über die öffentliche API (`iter`, `contains`, `from_policy`)
                // nachgebildet — semantisch identisch zur Mengen-Schnittmenge.
                let expected_intersection = PermissionSet::from_policy(
                    granted
                        .iter()
                        .filter(|permission| ceiling.contains(*permission)),
                );
                assert_eq!(
                    child.permissions(),
                    &expected_intersection,
                    "{name}: die Reduktion muss exakt der Schnitt sein"
                );
                assert!(
                    child.ensure_child_of(&parent).is_ok(),
                    "{name}: die Kind-Sandbox muss die Admission-Prüfung bestehen"
                );
            }
        }
        Ok(())
    }

    #[test]
    fn test_reducer_ceilings_match_the_registry_defaults_contract() {
        let set = |permissions: &[Permission]| PermissionSet::from_policy(permissions.to_vec());
        assert_eq!(
            reducer_ceiling("reduce_to_read_only"),
            Some(set(&[Permission::ReadWorkspace]))
        );
        assert_eq!(
            reducer_ceiling("reduce_to_read_registry"),
            Some(set(&[
                Permission::ReadWorkspace,
                Permission::ReadCargoRegistry
            ]))
        );
        assert_eq!(
            reducer_ceiling("reduce_to_read_network"),
            Some(set(&[Permission::NetworkAccess])),
            "W5/RD: die Netz-Obergrenze enthält kein ReadWorkspace"
        );
        assert_eq!(
            reducer_ceiling("reduce_to_read_explore"),
            Some(set(&[
                Permission::ReadWorkspace,
                Permission::ReadCargoRegistry,
                Permission::NetworkAccess
            ]))
        );
        assert_eq!(
            reducer_ceiling("reduce_to_read_workspace_network"),
            Some(set(&[Permission::ReadWorkspace, Permission::NetworkAccess]))
        );
        // Netz tragen nur die Netz-Kennungen; `reduce_to_read_registry`
        // (analyst, researcher-deps) bleibt ohne Netz.
        for name in KNOWN_AUTHORITY_REDUCERS {
            let networked = reducer_ceiling(name)
                .is_some_and(|ceiling| ceiling.contains(Permission::NetworkAccess));
            assert_eq!(
                networked,
                matches!(
                    *name,
                    "reduce_to_read_network"
                        | "reduce_to_read_explore"
                        | "reduce_to_read_workspace_network"
                ),
                "{name}"
            );
        }
        assert_eq!(reducer_ceiling("reduce_ro"), None);
        for name in KNOWN_AUTHORITY_REDUCERS {
            assert!(reducer_ceiling(name).is_some(), "{name} ohne Obergrenze");
        }
    }

    /// `reduce_to_read_network` (und die zwei Explorer-Kennungen) reichen die
    /// Parent-Hosts unverändert durch, während die drei anderen Reduzierer den
    /// Host-Scope leeren. Der Parent
    /// trägt hierfür über `SandboxSpec::from_resolved_for_test` (Feature
    /// `test-support` von `harw-authority`, nur in `[dev-dependencies]`) eine
    /// echte, nicht-leere Host-Allow-Liste — vorher konnte von außerhalb von
    /// `harw-authority` nur `NetworkScope::empty()` erzeugt werden, wodurch
    /// dieser Test vakuos war (siehe git-Historie).
    #[test]
    fn test_reduce_to_read_network_never_reads_the_workspace_and_keeps_only_parent_hosts()
    -> TestResult {
        let (ctx, tmp) = make_test_ctx()?;
        let binding = ctx.sandbox().workspace().clone();
        std::fs::remove_dir_all(tmp).ok();
        let parent = SandboxSpec::from_resolved_for_test(
            binding,
            PermissionSet::from_policy(ALL_PERMISSIONS),
            NetworkScope::from_hosts(["docs.rs".to_owned()]),
        );

        let network = resolve_authority_reducer("reduce_to_read_network")(&parent);
        assert!(!network.permissions().contains(Permission::ReadWorkspace));
        assert!(network.permissions().contains(Permission::NetworkAccess));
        assert_eq!(network.network_scope(), parent.network_scope());
        assert!(
            network.network_scope().allows("docs.rs"),
            "reduce_to_read_network muss die Parent-Hosts durchreichen"
        );

        for name in ["reduce_to_read_explore", "reduce_to_read_workspace_network"] {
            let child = resolve_authority_reducer(name)(&parent);
            assert!(
                child.permissions().contains(Permission::ReadWorkspace),
                "{name}"
            );
            assert!(
                child.permissions().contains(Permission::NetworkAccess),
                "{name}"
            );
            assert_eq!(
                child.network_scope(),
                parent.network_scope(),
                "{name}: Host-Scope bleibt an den Parent gebunden"
            );
            assert!(!child.network_scope().allows("example.org"), "{name}");
        }
        assert!(
            resolve_authority_reducer("reduce_to_read_explore")(&parent)
                .permissions()
                .contains(Permission::ReadCargoRegistry)
        );
        assert!(
            !resolve_authority_reducer("reduce_to_read_workspace_network")(&parent)
                .permissions()
                .contains(Permission::ReadCargoRegistry)
        );

        for name in [
            "reduce_to_read_only",
            "reduce_to_read_registry",
            "reduce_to_read_execute",
        ] {
            let child = resolve_authority_reducer(name)(&parent);
            assert!(
                !child.permissions().contains(Permission::NetworkAccess),
                "{name}"
            );
            assert!(
                child.network_scope().is_empty(),
                "{name}: Host-Scope muss leer sein"
            );
        }
        Ok(())
    }

    /// Die Netz-Kennungen (`reduce_to_read_explore` für `explorer`,
    /// `uia-worker`, `uia-writer`; `reduce_to_read_workspace_network` für
    /// `uia-explorer`; `reduce_to_read_network` für `researcher-web`)
    /// gewähren nie Netz, das der Parent nicht selbst trägt, und nie
    /// Schreib-/Ausführungsrecht — auch nicht einem voll berechtigten
    /// Parent.
    #[test]
    fn test_network_reducers_never_grant_network_the_parent_lacks() -> TestResult {
        let (ctx, tmp) = make_test_ctx()?;
        let binding = ctx.sandbox().workspace().clone();
        std::fs::remove_dir_all(tmp).ok();
        let without_network: Vec<Permission> = ALL_PERMISSIONS
            .iter()
            .copied()
            .filter(|permission| *permission != Permission::NetworkAccess)
            .collect();
        let offline_parent = SandboxSpec::from_resolved_for_test(
            binding.clone(),
            PermissionSet::from_policy(without_network),
            NetworkScope::empty(),
        );
        let online_parent = SandboxSpec::from_resolved_for_test(
            binding,
            PermissionSet::from_policy(ALL_PERMISSIONS),
            NetworkScope::from_hosts(["crates.io".to_owned()]),
        );
        for name in [
            "reduce_to_read_network",
            "reduce_to_read_explore",
            "reduce_to_read_workspace_network",
        ] {
            let offline = resolve_authority_reducer(name)(&offline_parent);
            assert!(
                !offline.permissions().contains(Permission::NetworkAccess),
                "{name}: Netz ohne Netz beim Parent"
            );
            assert!(offline.network_scope().is_empty(), "{name}");
            let online = resolve_authority_reducer(name)(&online_parent);
            assert!(
                online
                    .permissions()
                    .is_subset_of(online_parent.permissions())
            );
            assert!(
                online.network_scope().allows("crates.io")
                    && !online.network_scope().allows("example.org"),
                "{name}: Host-Scope nie breiter als beim Parent"
            );
            for gated in [
                Permission::WriteWorkspace,
                Permission::ExecuteProcess,
                Permission::ReadSecrets,
                Permission::ManagePlugins,
            ] {
                assert!(!online.permissions().contains(gated), "{name}: {gated:?}");
            }
        }
        Ok(())
    }

    #[test]
    fn test_bind_finding_to_question_accepts_only_the_open_question() -> TestResult {
        let finding = serde_json::json!({ "question_id": "q-1" });
        let nested = serde_json::json!({ "question": { "id": "q-1" }, "response_format": {} });
        let flat = serde_json::json!({ "id": "q-1" });
        assert_eq!(bind_finding_to_question(&finding, &nested), Ok(()));
        assert_eq!(bind_finding_to_question(&finding, &flat), Ok(()));

        let foreign = serde_json::json!({ "question": { "id": "q-2" } });
        let Err(error) = bind_finding_to_question(&finding, &foreign) else {
            return Err(TestError::Unexpected(
                "ein Finding zu einer anderen Frage ist ein Vertragsbruch".into(),
            ));
        };
        assert!(
            error.contains("'q-1'") && error.contains("'q-2'"),
            "{error}"
        );

        let without_id = serde_json::json!({ "question": "Welche Version?" });
        assert!(bind_finding_to_question(&finding, &without_id).is_err());
        let blank_id = serde_json::json!({ "question": { "id": "  " } });
        assert!(bind_finding_to_question(&finding, &blank_id).is_err());
        let no_claim = serde_json::json!({ "conclusion": "c" });
        assert!(bind_finding_to_question(&no_claim, &nested).is_err());
        Ok(())
    }

    #[test]
    fn test_model_effort_field_detects_both_override_fields() {
        assert_eq!(
            model_effort_field(&serde_json::json!({ "effort": "max" })),
            Some("effort")
        );
        assert_eq!(
            model_effort_field(&serde_json::json!({ "reasoning_effort": null })),
            Some("reasoning_effort")
        );
        assert_eq!(
            model_effort_field(&serde_json::json!({ "topic": "effort" })),
            None
        );
    }

    #[tokio::test]
    async fn test_invoke_rejects_a_model_effort_argument_before_spawning() -> TestResult {
        let adapter = AgentToolAdapter::from_operation(Arc::new(AgentOp))
            .ok_or(TestError::Missing("Erwartet Some"))?;
        for args in [
            serde_json::json!({ "topic": "Rust", "effort": "max" }),
            serde_json::json!({ "topic": "Rust", "reasoning_effort": "xhigh" }),
        ] {
            // Mit vollständiger Laufzeit: die Ablehnung muss vor Budget-Parser
            // und Spawn greifen, sonst entstünde ein Kind.
            let (services, _events) = services_with_runtime();
            let spawner = services
                .get::<Arc<ManagedAgentSpawner>>()
                .map(Arc::clone)
                .ok_or(TestError::Missing("Spawner registriert"))?;
            let (ctx, tmp) = make_test_ctx_with(services)?;
            let result = adapter.invoke(&ctx, args).await;
            let active = spawner.active_children_for(ctx.session_id());
            std::fs::remove_dir_all(tmp).ok();
            assert!(
                matches!(&result, Err(OpError::InvalidArguments(message)) if message.contains("effort")),
                "ein Effort-Argument des Modells muss abgelehnt werden: {result:?}"
            );
            assert_eq!(active, 0, "abgelehnter Aufruf darf kein Kind admittieren");
        }
        Ok(())
    }

    // ── UiaWorker-Fan-out-Deckelung ───────────────────────────────────────────

    /// `fanout_children` (siehe oben, `agent_tool.rs`) deckelt `max_parallel` für
    /// eine `uia-worker`-Rolle über
    /// [`ManagedAgentSpawner::max_concurrent_instances_for_role`] statt über einen
    /// direkten `harw_agent_dsl::roles::AgentRoleId`-Vergleich in dieser Datei
    /// (die Regel gehört dem Spawner). Dieser Test belegt, dass die
    /// Deckelungsmethode selbst — von genau hier aus, derselben Crate, aus der
    /// `fanout_children` sie aufruft — für eine registrierte `uia-worker`-Rolle
    /// `1` liefert und für eine andere Rolle unbeschränkt bleibt. Der volle
    /// asynchrone Scheduler-Beweis (echte Nebenläufigkeitsmessung über mehrere
    /// tatsächlich laufende Kind-Turns) lebt in
    /// `harw-core/src/child_controller.rs`
    /// (`run_children_caps_uia_worker_wave_to_one_regardless_of_max_parallel`),
    /// weil `ManagedAgentSpawner`s `active`/`cancellations`-Felder, die diese
    /// Testinfrastruktur braucht, `harw-core`-privat sind und von hier aus nicht
    /// erreichbar sind.
    #[test]
    fn fanout_children_uia_worker_cap_is_reachable_from_this_crate() -> TestResult {
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let spawner = ManagedAgentSpawner::new(
            Arc::new(Mutex::new(SessionManager::new(event_tx))),
            ChildLimits::conservative(),
        )
        .with_role(
            "uia-worker",
            harw_types::AgentRole::Agent {
                name: "uia-worker".to_owned(),
            },
            harw_agent_dsl::roles::AgentRoleId::UiaWorker,
            Arc::new(IrRegistryFactory {
                ir: ir_with_contract("harwness.return.coding-task@1")?,
            }),
        )
        .with_role(
            "worker",
            harw_types::AgentRole::Agent {
                name: "worker".to_owned(),
            },
            harw_agent_dsl::roles::AgentRoleId::Worker,
            Arc::new(IrRegistryFactory {
                ir: ir_with_contract("harwness.return.coding-task@1")?,
            }),
        );

        assert_eq!(
            spawner.max_concurrent_instances_for_role("uia-worker"),
            1,
            "a uia-worker role must cap fanout_children's slots to 1"
        );
        assert_eq!(
            spawner.max_concurrent_instances_for_role("worker"),
            usize::MAX,
            "a non-uia-worker role must stay unbounded"
        );
        Ok(())
    }

    // ── Send + Sync compile-time check ────────────────────────────────────────

    #[test]
    fn agent_tool_adapter_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AgentToolAdapter>();
    }

    // --- Teil C: Teilergebnis nach erschöpftem Token-Budget ---------------

    fn exhausted_run(text: Option<&str>) -> harw_core::child_controller::ChildRunResult {
        harw_core::child_controller::ChildRunResult {
            child: SessionId::new(),
            outcome: TurnOutcome::Completed,
            full_text: text.map(ToOwned::to_owned),
            usage: harw_core::child_controller::ChildUsage::default(),
            budget_exhausted: true,
            budget_handoff: Some(harw_core::child_handoff::BudgetHandoff::LastAnswer),
        }
    }

    /// Runde 5, Teil J: ein budget-beendetes Kind mit verdichteter Übergabe.
    fn compacted_run(text: &str) -> harw_core::child_controller::ChildRunResult {
        harw_core::child_controller::ChildRunResult {
            budget_handoff: Some(harw_core::child_handoff::BudgetHandoff::Compacted),
            ..exhausted_run(Some(text))
        }
    }

    fn handoff_text() -> String {
        format!(
            "{} Budget erreicht – Übergabe-Zusammenfassung (verdichtet aus 3 Modellrunden, 2 \
             Tool-Aufrufen)\n\n## Auftrag\nA\n## Offene Punkte\n- B",
            harw_core::child_handoff::HANDOFF_MARKER
        )
    }

    #[test]
    fn a_compacted_text_handoff_goes_back_unchanged_with_its_marker() {
        let text = handoff_text();
        let value = super::budget_exhausted_value(
            super::ChildReturnContract::Text,
            &compacted_run(&text),
            "worker",
            &serde_json::Value::Null,
        );
        // Unverändert — also ohne den Teilergebnis-Präfix davor.
        assert_eq!(value.as_str(), Some(text.as_str()));
    }

    #[test]
    fn a_compacted_research_handoff_becomes_a_marked_finding() -> TestResult {
        let question = serde_json::json!({ "question": { "id": "q-9", "text": "Wo?" } });
        let value = super::budget_exhausted_value(
            super::ChildReturnContract::ResearchFinding,
            &compacted_run(&handoff_text()),
            "explorer",
            &question,
        );
        assert_eq!(
            value.get("handoff"),
            Some(&serde_json::Value::from("compacted"))
        );
        assert_eq!(
            value.get("budget_exhausted"),
            Some(&serde_json::Value::Bool(true))
        );
        super::bind_finding_to_question(&value, &question).map_err(TestError::Unexpected)?;
        let finding: harw_research::ResearchFinding = serde_json::from_value(value)
            .map_err(|error| TestError::Unexpected(error.to_string()))?;
        assert!(
            finding
                .conclusion
                .starts_with(harw_core::child_handoff::HANDOFF_MARKER)
        );
        assert!(finding.conclusion.contains("## Offene Punkte"));
        Ok(())
    }

    #[test]
    fn a_typed_handoff_names_how_it_was_produced() {
        let compacted = super::budget_exhausted_value(
            super::ChildReturnContract::ReturnEnvelope,
            &compacted_run(&handoff_text()),
            "worker",
            &serde_json::Value::Null,
        );
        assert_eq!(
            compacted.get("handoff"),
            Some(&serde_json::Value::from("compacted"))
        );
        let fallback = super::budget_exhausted_value(
            super::ChildReturnContract::ReturnEnvelope,
            &exhausted_run(Some("halb")),
            "worker",
            &serde_json::Value::Null,
        );
        assert_eq!(
            fallback.get("handoff"),
            Some(&serde_json::Value::from("last_answer"))
        );
        assert_eq!(
            fallback.get("partial_result"),
            Some(&serde_json::Value::from("halb"))
        );
    }

    #[test]
    fn a_budget_exhausted_research_child_yields_a_marked_partial_finding() -> TestResult {
        let question = serde_json::json!({ "question": { "id": "q-7", "text": "Wo?" } });
        let value = super::budget_exhausted_value(
            super::ChildReturnContract::ResearchFinding,
            &exhausted_run(Some("Bisher gefunden: src/lib.rs")),
            "explorer",
            &question,
        );
        assert_eq!(
            value.get("budget_exhausted"),
            Some(&serde_json::Value::Bool(true))
        );
        super::bind_finding_to_question(&value, &question).map_err(TestError::Unexpected)?;
        let finding: harw_research::ResearchFinding = serde_json::from_value(value)
            .map_err(|error| TestError::Unexpected(error.to_string()))?;
        assert!(finding.conclusion.contains("Bisher gefunden: src/lib.rs"));
        assert!(finding.conclusion.contains("Teilergebnis"));
        assert_eq!(finding.confidence, harw_research::Confidence::Low);
        assert!(!finding.unresolved_questions.is_empty());
        Ok(())
    }

    #[test]
    fn a_budget_exhausted_text_child_keeps_its_answer_with_a_marker() {
        let value = super::budget_exhausted_value(
            super::ChildReturnContract::Text,
            &exhausted_run(Some("halbe Antwort")),
            "worker",
            &serde_json::Value::Null,
        );
        let text = value.as_str().unwrap_or_default();
        assert!(text.starts_with("[budget_exhausted: true]"), "{text}");
        assert!(text.ends_with("halbe Antwort"), "{text}");
    }
}
