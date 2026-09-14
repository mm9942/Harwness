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
//! - [`crate::error::OpError::InvalidArguments`]: Argumente sind kein JSON-Objekt,
//!   tragen ein Effort-Feld (`effort`/`reasoning_effort`), oder ein Budget-Label
//!   ist syntaktisch ungültig.
//! - [`crate::error::OpError::NotAvailable`]: Kein `ManagedAgentSpawner`/`StateStore`
//!   im Kontext registriert, die Deklaration trägt ein unlesbares `budget_hint`,
//!   der Spawn/Lauf/Effort-Clamp schlug fehl, oder das Kind pausierte ohne
//!   Pause-Erlaubnis.
//!
//! # Spec-Quelle
//! `docs/design/wave-4-agents-as-tools.md`, `agent-definition-dsl.md` §13,
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

use harw_core::StateStore;
use harw_core::child_controller::{
    AgentBudget, ChildRegistryFactory, ChildRunResult, JoinSemantics, ManagedAgentSpawner,
};
use harw_core::turn_loop::{TurnInput, TurnOutcome};
use harw_sandbox::{Permission, PermissionSet, SandboxSpec};
use harw_types::{ReasoningEffort, SessionId};
use serde_json::{Value, json};

use harw_operations::context::OpContext;
use harw_operations::error::OpError;
use harw_operations::operation::{OpOutput, Operation, Surface};

use crate::context_ext::OpContextCoreExt;

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
/// `docs/design/wave-4-agents-as-tools.md`, Abschnitt 4
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
    /// Durchsucht [`crate::operation::OperationMeta::surfaces`] nach dem ersten
    /// `Surface::AgentTool`-Eintrag. Sind defensiv mehrere vorhanden, wird die
    /// **erste** verwendet — analog zu [`crate::adapter::ModelToolAdapter`].
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
    ///   Werden unverändert als `context` in `SpawnInput` sowie als Text
    ///   (`args.to_string()`) im initialen Child-Turn übergeben.
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
    /// - [`OpError::NotAvailable`]: `spawn_child` oder `run_child_with_budget`
    ///   schlug fehl (Meldung nennt Kind, Rolle und Pause-Sperre).
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
    /// `spawn_child` und `run_child_with_budget` sind selbst nebenläufigkeitssicher
    /// (siehe `ManagedAgentSpawner`-Dokumentation).
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

            let contract = resolve_child_contract(ctx, self.child_name);

            let reducer = resolve_authority_reducer(self.authority_reducer);
            let child_sandbox = reducer(ctx.sandbox());

            let spawn_input = harw_extension_api::SpawnInput {
                parent_session_id: ctx.session_id().clone(),
                handoff_call_id: harw_types::ToolCallId::new(),
                instructions: None,
                context: args.clone(),
                // This tool declares no ceiling demand of its own: the child
                // simply inherits whatever ceiling its parent already
                // enforces, unchanged (see `SpawnInput::ceiling`).
                ceiling: None,
            };

            let child = harw_extension_api::AgentSpawner::spawn_child(
                spawner.as_ref(),
                self.child_name,
                spawn_input,
                child_sandbox,
                None,
            )
            .await
            .map_err(|e| OpError::NotAvailable(format!("Agent-Spawn fehlgeschlagen: {e}")))?;
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

            let run_result = spawner
                .run_child_with_budget(
                    &child,
                    store.as_ref(),
                    None,
                    TurnInput::user(args.to_string()),
                    budget,
                )
                .await
                .map_err(|e| {
                    OpError::NotAvailable(format!(
                        "Child-Agent-Ausführung fehlgeschlagen (child={child}, role='{role}', \
                         allow_pause={allow_pause}): {e}"
                    ))
                })?;

            match &run_result.outcome {
                TurnOutcome::Completed => {
                    tracing::info!(
                        child = %run_result.child,
                        contract = contract.as_label(),
                        "AgentToolAdapter: Child-Agent-Turn abgeschlossen"
                    );
                    match contract {
                        // Rückwärtskompatibel: der Freitext des Kindes geht
                        // unverändert (und ohne JSON-Quoting) an das Parent-Modell.
                        ChildReturnContract::Text => {
                            completed_child_output(spawner.child_final_assistant_text(&child))
                        }
                        typed => {
                            let text =
                                spawner.child_final_assistant_text(&child).map_err(|error| {
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
                    Err(OpError::Execution(format!(
                        "Child-Agent '{}' (Rolle '{role}') wurde abgebrochen (reason={reason:?})",
                        run_result.child
                    )))
                }
                TurnOutcome::Truncated => {
                    tracing::warn!(
                        child = %run_result.child,
                        "AgentToolAdapter: Child-Agent-Antwort abgeschnitten"
                    );
                    Err(OpError::Execution(format!(
                        "Child-Agent '{}' (Rolle '{role}') brach durch Abschneiden der \
                         Modellausgabe (max_tokens/Kontextfenster) ab, bevor etwaige Tool-Calls \
                         der Antwort ausgeführt wurden",
                        run_result.child
                    )))
                }
                TurnOutcome::Refused { detail } => {
                    tracing::warn!(
                        child = %run_result.child,
                        detail = ?detail,
                        "AgentToolAdapter: Child-Agent-Antwort abgelehnt"
                    );
                    Err(OpError::Execution(format!(
                        "Child-Agent '{}' (Rolle '{role}') lehnte die Antwort ab (detail={detail:?})",
                        run_result.child
                    )))
                }
                TurnOutcome::Failed { reason } => {
                    tracing::warn!(
                        child = %run_result.child,
                        reason = %reason,
                        "AgentToolAdapter: Child-Agent-Wiederaufnahme gescheitert"
                    );
                    Err(OpError::Execution(format!(
                        "Child-Agent '{}' (Rolle '{role}') scheiterte endgültig (reason={reason})",
                        run_result.child
                    )))
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
                let budget = spawner.child_budget(&target).ok_or_else(|| {
                    OpError::NotAvailable(CHILD_TARGET_UNAVAILABLE.to_owned())
                })?;
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
    let owned = spawner
        .child_record(target)
        .is_some_and(|record| record.parent == ctx.session_id().clone());
    if owned {
        Ok(())
    } else {
        Err(OpError::NotAvailable(CHILD_TARGET_UNAVAILABLE.to_owned()))
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
                invalid_hint(hint, &format!("unbekanntes Effort-Level '{level}': {error}"))
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
    base.checked_mul(scale).ok_or_else(|| {
        invalid_hint(
            hint,
            &format!("`{unit}` überläuft den 64-Bit-Wertebereich"),
        )
    })
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
/// ## Warum unbekannte Labels auf `Text` fallen
/// Ein unbekanntes Contract-Label bedeutet: dieses Werkzeug kann die Antwort
/// nicht prüfen. Dann darf es auch nicht so tun — der Freitext geht ungeprüft
/// durch, begleitet von einem `warn`. Das ist die **umgekehrte** Polarität zum
/// Authority-Reducer ([`resolve_authority_reducer`]), der bei einer unbekannten
/// Kennung auf die *restriktivste* Variante fällt: dort geht es um Rechte (im
/// Zweifel weniger), hier um eine Behauptung (im Zweifel keine).
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

    /// Bildet ein Contract-Label der Agent-IR auf die Auswertung ab.
    ///
    /// # Argumente
    /// - `id` (`&str`): das undurchsichtige Label aus `[return] contract = …`.
    ///
    /// # Returns
    /// [`Self::ResearchFinding`] für `"harwness.return.research-finding@1"`,
    /// [`Self::ReturnEnvelope`] für `"harwness.return.envelope@1"`,
    /// [`Self::SecurityVerdict`] für [`Self::SECURITY_VERDICT_ID`]
    /// (`"harwness.security-verdict/v1"`), sonst [`Self::Text`]. Bewusst
    /// kein `Result`: ein Label, das dieses Werkzeug nicht kennt (etwa
    /// `"harwness.return.coding-task@1"` oder eine künftige
    /// `"harwness.security-verdict/v2"`), darf nicht als „ungültig" gelten —
    /// es wird nur nicht geprüft (siehe [`resolve_child_contract`] für den
    /// begleitenden `warn`).
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
    ///     ChildReturnContract::ResearchFinding
    /// );
    /// assert_eq!(
    ///     ChildReturnContract::parse("harwness.security-verdict/v1"),
    ///     ChildReturnContract::SecurityVerdict
    /// );
    /// assert_eq!(
    ///     ChildReturnContract::parse("harwness.return.coding-task@1"),
    ///     ChildReturnContract::Text
    /// );
    /// ```
    #[must_use]
    pub fn parse(id: &str) -> Self {
        let trimmed = id.trim();
        if trimmed == Self::SECURITY_VERDICT_ID {
            return Self::SecurityVerdict;
        }
        match trimmed {
            "harwness.return.research-finding@1" => Self::ResearchFinding,
            "harwness.return.envelope@1" => Self::ReturnEnvelope,
            _ => Self::Text,
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
/// Ist keine Factory registriert (der heutige Normalfall, weil die Composition
/// Roots nur Spawner und `StateStore` registrieren), gilt [`ChildReturnContract::Text`]:
/// ohne IR gibt es keine Vertragsbehauptung, die dieses Werkzeug prüfen könnte.
///
/// # Argumente
/// - `ctx` (`&OpContext`): liefert die optionale Registry-Factory.
/// - `role` (`&str`): der Rollenname des Kindes (= `child_name` der Fläche).
///
/// # Returns
/// Der aufgelöste [`ChildReturnContract`]; [`ChildReturnContract::Text`] als
/// Rückfallebene.
///
/// # Concurrency
/// Nur lesend; die Factory ist per Supertrait `Send + Sync`.
fn resolve_child_contract(ctx: &OpContext, role: &str) -> ChildReturnContract {
    let Some(factory) = ctx.service::<Arc<dyn ChildRegistryFactory>>() else {
        tracing::debug!(
            role,
            "agent_tool.contract.no_registry_factory: Freitext-Rückfallebene"
        );
        return ChildReturnContract::Text;
    };
    let Some(label) = factory
        .executable_agent_ir(role)
        .and_then(|ir| ir.return_pipeline().contract())
    else {
        return ChildReturnContract::Text;
    };
    let contract = ChildReturnContract::parse(label);
    if contract == ChildReturnContract::Text {
        tracing::warn!(
            role,
            contract = label,
            known = "harwness.return.research-finding@1, harwness.return.envelope@1, \
                     harwness.security-verdict/v1",
            "agent_tool.contract.unknown_label: Ergebnis wird ungeprüft als Freitext gereicht"
        );
    }
    contract
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
/// Die Kennungen `reduce_to_read_only`, `reduce_to_read_registry` und
/// `reduce_to_read_network` sind wortgleich mit
/// `harw_registry_defaults::authority::REDUCE_TO_READ_*` (W5/RD), ebenso ihre
/// Permission-Obergrenzen (siehe [`reducer_ceiling`]).
const KNOWN_AUTHORITY_REDUCERS: &[&str] = &[
    "reduce_to_read_only",
    "reduce_to_read_execute",
    "reduce_to_read_registry",
    "reduce_to_read_network",
];

/// Liefert die Permission-Obergrenze einer bekannten Reducer-Kennung.
///
/// # Beschreibung
/// Gespiegelt aus `harw_registry_defaults::authority::AuthorityReducer::ceiling`
/// (W5/RD, `docs/remediation/ledger/W5/RD.md` §2/§3.5). Bewusst **gespiegelt,
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
///
/// `reduce_to_read_network` liest den Workspace absichtlich nicht: ein Kind mit
/// Netz und Workspace-Lesezugriff könnte Workspace-Daten über Anfrageparameter
/// hinaustragen (Plan-Annahme A5).
///
/// # Returns
/// `Some(PermissionSet)` für eine bekannte Kennung, sonst `None`.
fn reducer_ceiling(name: &str) -> Option<PermissionSet> {
    let permissions: &[Permission] = match name {
        "reduce_to_read_only" => &[Permission::ReadWorkspace],
        "reduce_to_read_execute" => &[Permission::ReadWorkspace, Permission::ExecuteProcess],
        "reduce_to_read_registry" => &[Permission::ReadWorkspace, Permission::ReadCargoRegistry],
        "reduce_to_read_network" => &[Permission::NetworkAccess],
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
    parent.restrict_with(&ceiling, &harw_sandbox::NetworkScope::empty())
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
/// (explorer, analyst, researcher-deps, planner laut W5/RD).
/// `ReadCargoRegistry` bleibt nur, wenn der Parent es selbst hat.
fn reduce_to_read_registry(parent: &SandboxSpec) -> SandboxSpec {
    restrict_without_network(parent, "reduce_to_read_registry")
}

/// Reduziert eine Sandbox auf ausgehenden Netzzugriff ohne Workspace-Lesen.
///
/// Schnittmenge mit `{ NetworkAccess }` (researcher-web laut W5/RD). Der
/// Host-Scope des Parents bleibt als Obergrenze unverändert (`restrict`);
/// verengt wird er von der Composition (`researcher_web_network_scope`), nie
/// erweitert. `NetworkAccess` bleibt nur, wenn der Parent es selbst hat.
fn reduce_to_read_network(parent: &SandboxSpec) -> SandboxSpec {
    let ceiling = reducer_ceiling("reduce_to_read_network").unwrap_or_else(PermissionSet::empty);
    parent.restrict(&ceiling)
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
///    durchläuft **lazy** und vollständig: `spawn_child` → Budget mit dem
///    IR-Budget verschneiden ([`tighten_budget`]) → Effort klammern (ohne
///    Override, fail-closed) → `run_child_with_budget` → Auswertung über
///    denselben Contract-Pfad wie [`AgentToolAdapter::invoke`]
///    ([`evaluate_child_return`], plus `question_id`-Bindung) → Slot-Freigabe.
///    Erst danach wird die nächste Frage admittiert. Damit sind nie mehr als
///    `max_parallel` Kinder dieser Welle gleichzeitig admittiert — vorher wurden
///    alle Kinder vorab admittiert, und ab dem neunten scheiterte die Welle am
///    Admission-Limit, unabhängig von `max_parallel` (G-016).
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
/// - `max_parallel` (`usize`): gleichzeitig admittierte und laufende Kinder
///   dieser Welle (`0` wird zu `1`).
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
    let slots = max_parallel.max(1);
    let winner = AtomicBool::new(false);
    // Je Position die Session-ID, sobald das Kind admittiert ist — nur damit
    // der Scheduler laufende Geschwister kooperativ abbrechen kann.
    let admitted: Vec<OnceLock<SessionId>> = (0..total).map(|_| OnceLock::new()).collect();
    let shared = FanoutShared {
        ctx,
        spawner: spawner.as_ref(),
        store: store.as_ref(),
        role,
        child_sandbox: &child_sandbox,
        budget,
        contract,
        winner: &winner,
    };

    let mut results: Vec<Option<Result<Value, String>>> = (0..total).map(|_| None).collect();
    let mut pending = questions.iter().enumerate();
    let mut running = Vec::with_capacity(slots.min(total));
    let mut winner_decided = false;
    // Ein Event statt eines betretenen Spans: `span::Entered` ist `!Send` und
    // würde über die `await`s gehalten den ganzen Future `!Send` machen.
    tracing::info!(role, children = total, max_parallel = slots, "agent_fanout.start");

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
                Box::pin(run_fanout_slot(&shared, position, question, &admitted[position])),
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
    /// Gesetzt, sobald bei `AnyTerminal` ein Gewinner feststeht.
    winner: &'a AtomicBool,
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
        instructions: None,
        context: question.clone(),
        // This fan-out tool declares no ceiling demand of its own: each
        // question-child simply inherits whatever ceiling its parent
        // already enforces, unchanged (see `SpawnInput::ceiling`).
        ceiling: None,
    };
    let child = harw_extension_api::AgentSpawner::spawn_child(
        shared.spawner,
        shared.role,
        spawn_input,
        shared.child_sandbox.clone(),
        None,
    )
    .await
    .map_err(|error| {
        tracing::warn!(role = shared.role, position, error = %error, "agent_fanout.spawn_failed");
        format!("Agent-Spawn fehlgeschlagen: {error}")
    })?;
    let slot = ChildSlotGuard::new(shared.spawner, child.clone());
    if admitted.set(child.clone()).is_err() {
        // Unerreichbar (eine Position wird genau einmal gestartet); ohne ID
        // bleibt nur der kooperative Geschwister-Abbruch aus, nie die Freigabe.
        tracing::warn!(child = %child, position, "agent_fanout.admitted_cell_occupied");
    }
    if shared.sibling_won() {
        return Err(CANCELLED_BY_SIBLING.to_owned());
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
        .map_err(|error| format!("Child-Ausführung fehlgeschlagen: {error}"))?;
    if shared.sibling_won() {
        return Err(CANCELLED_BY_SIBLING.to_owned());
    }

    match fanout_child_value(shared.spawner, shared.contract, &run, question)? {
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
fn fanout_child_value(
    spawner: &ManagedAgentSpawner,
    contract: ChildReturnContract,
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

    let text = spawner
        .child_final_assistant_text(&result.child)
        .map_err(|error| {
            format!(
                "Child-Agent-Abschlussantwort von '{}' nicht verfügbar: {error}",
                result.child
            )
        })?;
    let value = evaluate_child_return(contract, &text).map_err(|violation| violation.to_message())?;
    if contract == ChildReturnContract::ResearchFinding {
        bind_finding_to_question(&value, question)?;
    }
    Ok(FanoutValue::Final(value))
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
    use harw_catalog::AgentSuggestions;
    use harw_core::child_controller::{AgentBudget, ChildRegistryFactory, ManagedAgentSpawner};
    use harw_core::turn_loop::TurnOutcome;
    use harw_core::{ChildLimits, InMemoryStateStore, ModelProvider, SessionManager, StateStore};
    use harw_extension_api::{AgentSpawnError, ExtensionRegistry, SpawnInput};
    use harw_sandbox::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_types::{ItemId, ReasoningEffort, SessionId, TenantId, ToolCallId, TurnId, WorkspaceId};

    use super::{
        AgentToolAdapter, ChildReturnContract, KNOWN_AUTHORITY_REDUCERS, PauseKind,
        bind_finding_to_question, completed_child_output, contract_output, model_effort_field,
        parse_budget_hint, paused_child_result, reducer_ceiling, resolve_authority_reducer,
        resolve_child_contract, tighten_budget,
    };
    use crate::context_ext::OpContextCoreExt;
    use harw_operations::context::{OpContext, ServiceMap};
    use harw_operations::error::OpError;
    use harw_operations::operation::{
        OpFuture, OpInput, OpOutput, Operation, OperationDomain, OperationMeta, PermissionTier,
        Surface,
    };

    // ── test helpers ──────────────────────────────────────────────────────────

    /// Erstellt einen minimalen [`OpContext`] für Tests.
    ///
    /// Verwendet einen atomaren Zähler für Thread-sichere, eindeutige
    /// Verzeichnisnamen, sodass parallele Tests nicht kollidieren.
    fn make_test_ctx() -> (OpContext, PathBuf) {
        make_test_ctx_with(ServiceMap::new())
    }

    /// Wie [`make_test_ctx`], aber mit vorbereiteter [`ServiceMap`].
    fn make_test_ctx_with(services: ServiceMap) -> (OpContext, PathBuf) {
        static CTX_COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = CTX_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = std::env::temp_dir().join(format!(
            "harw-agent-tool-adapter-test-{}-{}",
            std::process::id(),
            id
        ));
        std::fs::create_dir_all(tmp.join("ws")).unwrap();
        let registry = WorkspaceRegistry::build(
            &tmp,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("ws"),
                root: PathBuf::from("ws"),
            }],
        )
        .unwrap();
        let binding = registry
            .resolve(
                &TenantId::from_str("test-tenant"),
                &WorkspaceId::from_str("ws"),
            )
            .unwrap();
        let sandbox = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace]),
        );
        let ctx = OpContext::new(SessionId::new(), TurnId::new(), sandbox, services);
        (ctx, tmp)
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
        <OpContext as OpContextCoreExt>::register_agent_tool_services(&mut services, spawner, store);
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
    fn ir_with_contract(contract: &str) -> ExecutableAgentIr {
        let raw = parse_toml(&format!(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.bridge-contract-test@1"
version = "1.0.0"
role = "worker"
specialization = "bridge-contract-test"

[return]
contract = "{contract}"
"#
        ))
        .expect("Test-Agent-Definition muss parsen");
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
        lower(&resolved).expect("Test-Agent-Definition muss lowern")
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
    fn from_operation_extracts_agent_tool_metadata() {
        let adapter = AgentToolAdapter::from_operation(Arc::new(AgentOp))
            .expect("Op mit AgentTool-Surface muss Some liefern");
        assert_eq!(adapter.child_name(), "researcher");
        assert_eq!(adapter.authority_reducer(), "reduce_ro");
        assert_eq!(adapter.budget_hint(), "8k");
    }

    #[test]
    fn from_operation_operation_accessor_returns_correct_meta_name() {
        let adapter = AgentToolAdapter::from_operation(Arc::new(AgentOp)).expect("Erwartet Some");
        assert_eq!(adapter.operation().meta().name, "spawn_x");
    }

    // ── Budget-Parser ─────────────────────────────────────────────────────────

    #[test]
    fn test_parse_budget_hint_reads_every_unit() {
        let budget =
            parse_budget_hint("8k_tokens,20_tool_calls,30s").expect("vollständiges Label ist gültig");
        assert_eq!(budget.max_tokens, Some(8_000));
        assert_eq!(budget.max_tool_calls, Some(20));
        assert_eq!(budget.max_wall_time_ms, Some(30_000));
        assert_eq!(budget.reasoning_effort, None);
    }

    #[test]
    fn test_parse_budget_hint_reads_millis_effort_and_mega_scale() {
        let budget = parse_budget_hint(" 500ms , effort=low , 1m_tokens ")
            .expect("Whitespace und beliebige Reihenfolge sind erlaubt");
        assert_eq!(budget.max_wall_time_ms, Some(500));
        assert_eq!(budget.reasoning_effort, Some(ReasoningEffort::Low));
        assert_eq!(budget.max_tokens, Some(1_000_000));
    }

    #[test]
    fn test_parse_budget_hint_scales_only_tokens() {
        let plain = parse_budget_hint("64000_tokens").expect("Zahl ohne Suffix ist gültig");
        assert_eq!(plain.max_tokens, Some(64_000));

        let scaled_tool_calls = parse_budget_hint("2k_tool_calls");
        assert!(
            matches!(scaled_tool_calls, Err(OpError::InvalidArguments(ref message)) if message.contains("tool_calls")),
            "der k/m-Multiplikator gilt nur für Tokens: {scaled_tool_calls:?}"
        );
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
    fn test_parse_budget_hint_empty_and_unlimited_default_to_no_limit() {
        for hint in ["", "   ", "unlimited", "UNLIMITED"] {
            let budget = parse_budget_hint(hint).expect("Label ohne Grenzen ist gültig");
            assert_eq!(budget, AgentBudget::default(), "Label {hint:?}");
            assert!(budget.max_tokens.is_none());
            assert!(budget.max_tool_calls.is_none());
            assert!(budget.max_wall_time_ms.is_none());
            assert!(budget.reasoning_effort.is_none());
        }
    }

    // ── Budget-Verschärfung ───────────────────────────────────────────────────

    #[test]
    fn test_tighten_budget_keeps_the_stricter_limit_per_dimension() {
        let declared = parse_budget_hint("8k_tokens,20_tool_calls,60s,effort=high")
            .expect("gültiges Label");
        let from_ir =
            parse_budget_hint("2k_tokens,64_tool_calls,30s,effort=low").expect("gültiges Label");

        let effective = tighten_budget(declared, from_ir);
        assert_eq!(effective.max_tokens, Some(2_000), "IR-Grenze ist strenger");
        assert_eq!(
            effective.max_tool_calls,
            Some(20),
            "Deklaration ist strenger"
        );
        assert_eq!(effective.max_wall_time_ms, Some(30_000));
        assert_eq!(effective.reasoning_effort, Some(ReasoningEffort::Low));
    }

    #[test]
    fn test_tighten_budget_lets_none_lose_against_any_limit() {
        let limited = parse_budget_hint("4k_tokens").expect("gültiges Label");
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
    }

    #[test]
    fn test_tighten_budget_is_idempotent() {
        let budget = parse_budget_hint("1k_tokens,5_tool_calls,10s").expect("gültiges Label");
        assert_eq!(tighten_budget(budget, budget), budget);
    }

    // ── Return-Contract ───────────────────────────────────────────────────────

    #[test]
    fn test_child_return_contract_parse_maps_known_ids() {
        assert_eq!(
            ChildReturnContract::parse("harwness.return.research-finding@1"),
            ChildReturnContract::ResearchFinding
        );
        assert_eq!(
            ChildReturnContract::parse("harwness.return.envelope@1"),
            ChildReturnContract::ReturnEnvelope
        );
    }

    #[test]
    fn test_child_return_contract_parse_falls_back_to_text() {
        for id in [
            "harwness.return.coding-task@1",
            "harwness.return.plan-proposal@1",
            "",
            "nonsense",
            // Eine hypothetische künftige Fassung des Verdict-Vertrags: wird
            // erkannt als "nicht v1", nicht geraten gegen das heutige Schema
            // geprüft.
            "harwness.security-verdict/v2",
        ] {
            assert_eq!(
                ChildReturnContract::parse(id),
                ChildReturnContract::Text,
                "unbekanntes Label {id:?} darf keine Prüfung behaupten"
            );
        }
    }

    #[test]
    fn test_child_return_contract_parse_maps_security_verdict() {
        // Der vierte Arm bricht die drei bestehenden Zuordnungen nicht (siehe
        // `test_child_return_contract_parse_maps_known_ids` oben, unverändert).
        assert_eq!(
            ChildReturnContract::parse("harwness.security-verdict/v1"),
            ChildReturnContract::SecurityVerdict
        );
        assert_eq!(
            ChildReturnContract::SecurityVerdict.as_label(),
            "harwness.security-verdict/v1"
        );
    }

    #[test]
    fn test_contract_output_returns_canonical_json_for_a_valid_finding() {
        let output = contract_output(ChildReturnContract::ResearchFinding, VALID_FINDING);
        let value: serde_json::Value =
            serde_json::from_str(&output.text).expect("die Ausgabe muss gültiges JSON sein");

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
    }

    #[test]
    fn test_contract_output_accepts_a_fenced_finding() {
        let fenced = format!("```json\n{VALID_FINDING}\n```");
        let output = contract_output(ChildReturnContract::ResearchFinding, &fenced);
        let value: serde_json::Value =
            serde_json::from_str(&output.text).expect("die Ausgabe muss gültiges JSON sein");
        assert_eq!(value["produced_by"], "explorer-1");
    }

    #[test]
    fn test_contract_output_reports_garbage_as_structured_error_output() {
        let output = contract_output(
            ChildReturnContract::ResearchFinding,
            "Ich habe nachgesehen, jiff sieht aktuell aus.",
        );
        let value: serde_json::Value =
            serde_json::from_str(&output.text).expect("auch der Fehlerfall ist JSON");

        assert!(
            value["error"].as_str().is_some_and(|m| !m.is_empty()),
            "der Vertragsbruch muss benannt werden: {value}"
        );
        assert_eq!(
            value["raw"], "Ich habe nachgesehen, jiff sieht aktuell aus.",
            "der Rohtext muss erhalten bleiben, damit das Parent nachsteuern kann"
        );
        assert_eq!(value["contract"], "harwness.return.research-finding@1");
    }

    #[test]
    fn test_contract_output_reports_an_invalid_finding_as_structured_error_output() {
        // Syntaktisch gültiges JSON, das die Vertragsregel „ab Confidence
        // Medium sind Belege Pflicht" verletzt.
        let no_evidence = r#"{"question_id":"q-2","conclusion":"vermutlich aktuell",
            "evidence":[],"confidence":"high","produced_by":"explorer-2",
            "produced_at":"2026-08-27T00:00:00Z"}"#;
        let output = contract_output(ChildReturnContract::ResearchFinding, no_evidence);
        let value: serde_json::Value =
            serde_json::from_str(&output.text).expect("auch der Fehlerfall ist JSON");
        assert!(
            value["error"]
                .as_str()
                .is_some_and(|m| m.contains("Recherche-Vertrag")),
            "die Validierung muss vom Parsen unterscheidbar gemeldet werden: {value}"
        );
    }

    #[test]
    fn test_contract_output_validates_a_return_envelope() {
        let envelope = r#"{"agent_id":"explorer-1","outcome":"success",
            "summary":"fertig","payload":{"k":1}}"#;
        let output = contract_output(ChildReturnContract::ReturnEnvelope, envelope);
        let value: serde_json::Value =
            serde_json::from_str(&output.text).expect("die Ausgabe muss gültiges JSON sein");
        assert_eq!(value["agent_id"], "explorer-1");
        assert_eq!(value["outcome"], "success");
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
    fn test_contract_output_validates_a_security_verdict() {
        let output = contract_output(ChildReturnContract::SecurityVerdict, VALID_VERDICT);
        let value: serde_json::Value =
            serde_json::from_str(&output.text).expect("die Ausgabe muss gültiges JSON sein");
        assert_eq!(value["classification"], "suspicious");
        assert_eq!(value["issued_by"], "security-triage-1");
        assert!(
            value.get("error").is_none(),
            "ein gültiges Verdikt darf keinen Fehler melden"
        );
    }

    #[test]
    fn test_contract_output_rejects_wrong_verdict_contract_version_without_content() {
        let v2 = VALID_VERDICT.replace(
            "harwness.security-verdict/v1",
            "harwness.security-verdict/v2",
        );
        let output = contract_output(ChildReturnContract::SecurityVerdict, &v2);
        let value: serde_json::Value =
            serde_json::from_str(&output.text).expect("auch der Fehlerfall ist JSON");
        assert!(
            value["error"].as_str().is_some_and(|m| !m.is_empty()),
            "der Vertragsbruch muss benannt werden: {value}"
        );
        assert_eq!(value["contract"], "harwness.security-verdict/v1");
    }

    #[test]
    fn test_contract_output_reports_malformed_verdict_without_raw_text() {
        // Der zentrale Unterschied zu den beiden anderen typisierten Armen:
        // kein `"raw"`-Feld und die eingebettete Fehlermeldung enthält den
        // Rohtext nicht — ein Verdikt sagt bei Ablehnung "dass", nicht "was".
        let attacker_text = "SECRET_PROCESS_NAME_do_not_log_me";
        let output = contract_output(ChildReturnContract::SecurityVerdict, attacker_text);
        let value: serde_json::Value =
            serde_json::from_str(&output.text).expect("auch der Fehlerfall ist JSON");
        assert!(
            value.get("raw").is_none(),
            "ein Verdict-Vertragsbruch darf keinen Rohtext-Auszug enthalten: {value}"
        );
        assert!(
            !output.text.contains("SECRET_PROCESS_NAME_do_not_log_me"),
            "die Ablehnung darf den angreiferkontrollierten Rohtext nicht zitieren: {value}"
        );
        assert_eq!(value["contract"], "harwness.security-verdict/v1");
    }

    #[test]
    fn test_contract_output_rejects_a_verdict_with_unknown_field() {
        // `deny_unknown_fields` (K19): ein zusätzliches Feld macht das
        // Verdikt ungültig, statt es stillschweigend zu ignorieren.
        let with_extra = VALID_VERDICT.replace(
            "\"issued_by\": \"security-triage-1\",",
            "\"issued_by\": \"security-triage-1\", \"extra\": true,",
        );
        let output = contract_output(ChildReturnContract::SecurityVerdict, &with_extra);
        let value: serde_json::Value =
            serde_json::from_str(&output.text).expect("auch der Fehlerfall ist JSON");
        assert!(value["error"].as_str().is_some_and(|m| !m.is_empty()));
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
    fn test_security_verdict_output_carries_no_action_target() {
        let output = contract_output(ChildReturnContract::SecurityVerdict, VALID_VERDICT);
        let value: serde_json::Value =
            serde_json::from_str(&output.text).expect("die Ausgabe muss gültiges JSON sein");
        assert_eq!(value["suggested_response"], "escalate");
        for forbidden in ["cgroup", "pid", "process_id", "host", "action", "target"] {
            assert!(
                value.get(forbidden).is_none(),
                "ein Verdikt darf kein Aktionsziel '{forbidden}' tragen: {value}"
            );
        }
    }

    #[test]
    fn test_security_verdict_arm_does_not_disturb_the_three_existing_arms() {
        // Additivitätsbeleg: dieselben drei bestehenden Zuordnungen liefern
        // nach Einführung des vierten Arms noch dieselben Ergebnisse.
        let finding_output = contract_output(ChildReturnContract::ResearchFinding, VALID_FINDING);
        assert!(
            serde_json::from_str::<serde_json::Value>(&finding_output.text)
                .expect("json")
                .get("error")
                .is_none()
        );

        let envelope = r#"{"agent_id":"explorer-1","outcome":"success",
            "summary":"fertig","payload":{"k":1}}"#;
        let envelope_output = contract_output(ChildReturnContract::ReturnEnvelope, envelope);
        assert!(
            serde_json::from_str::<serde_json::Value>(&envelope_output.text)
                .expect("json")
                .get("error")
                .is_none()
        );

        let text_output = contract_output(ChildReturnContract::Text, "unverändert");
        assert_eq!(text_output.text, "unverändert");
    }

    #[test]
    fn test_resolve_child_contract_defaults_to_text_without_registry_factory() {
        let (ctx, tmp) = make_test_ctx();
        let contract = resolve_child_contract(&ctx, "researcher");
        std::fs::remove_dir_all(tmp).ok();
        assert_eq!(
            contract,
            ChildReturnContract::Text,
            "ohne Registry-Factory gibt es keine IR und damit keine Vertragsbehauptung"
        );
    }

    #[test]
    fn test_resolve_child_contract_reads_the_agent_ir() {
        let factory: Arc<dyn ChildRegistryFactory> = Arc::new(IrRegistryFactory {
            ir: ir_with_contract("harwness.return.research-finding@1"),
        });
        let mut services = ServiceMap::new();
        services.insert(factory);
        let (ctx, tmp) = make_test_ctx_with(services);

        let contract = resolve_child_contract(&ctx, "explorer");
        std::fs::remove_dir_all(tmp).ok();
        assert_eq!(contract, ChildReturnContract::ResearchFinding);
    }

    #[test]
    fn test_resolve_child_contract_reads_the_security_verdict_ir_label() {
        let factory: Arc<dyn ChildRegistryFactory> = Arc::new(IrRegistryFactory {
            ir: ir_with_contract(ChildReturnContract::SECURITY_VERDICT_ID),
        });
        let mut services = ServiceMap::new();
        services.insert(factory);
        let (ctx, tmp) = make_test_ctx_with(services);

        let contract = resolve_child_contract(&ctx, "security-triage-1");
        std::fs::remove_dir_all(tmp).ok();
        assert_eq!(contract, ChildReturnContract::SecurityVerdict);
    }

    #[test]
    fn test_resolve_child_contract_falls_back_on_an_unknown_ir_label() {
        let factory: Arc<dyn ChildRegistryFactory> = Arc::new(IrRegistryFactory {
            ir: ir_with_contract("harwness.return.coding-task@1"),
        });
        let mut services = ServiceMap::new();
        services.insert(factory);
        let (ctx, tmp) = make_test_ctx_with(services);

        let contract = resolve_child_contract(&ctx, "coder");
        std::fs::remove_dir_all(tmp).ok();
        assert_eq!(
            contract,
            ChildReturnContract::Text,
            "ein nicht implementierter Contract darf nicht als geprüft gelten"
        );
    }

    // ── Pause-Behandlung ──────────────────────────────────────────────────────

    #[test]
    fn test_paused_child_fails_closed_with_role_and_outcome() {
        let child = SessionId::new();
        let outcome = TurnOutcome::AwaitingApproval {
            call_id: ToolCallId::new(),
            request: ItemId::new(),
        };
        let result = paused_child_result(PauseKind::Approval, &child, "explorer", false, &outcome);

        match result {
            Err(OpError::NotAvailable(message)) => {
                assert!(message.contains("explorer"), "Rolle fehlt: {message}");
                assert!(
                    message.contains(child.as_str()),
                    "Kind-ID fehlt: {message}"
                );
                assert!(
                    message.contains("AwaitingApproval"),
                    "Outcome fehlt: {message}"
                );
                assert!(
                    message.contains("allow_pause = false"),
                    "der Hinweis auf die Pause-Sperre fehlt: {message}"
                );
            }
            other => panic!("Pause ohne Erlaubnis muss fail-closed sein, war: {other:?}"),
        }
    }

    #[test]
    fn test_paused_child_reports_a_permitted_pause_as_structured_output() {
        let child = SessionId::new();
        let outcome = TurnOutcome::AwaitingChild {
            child: SessionId::new(),
            call_id: ToolCallId::new(),
            role: "grandchild".to_owned(),
        };
        let output = paused_child_result(PauseKind::Child, &child, "planner", true, &outcome)
            .expect("eine erlaubte Pause ist kein Fehler");
        let value: serde_json::Value =
            serde_json::from_str(&output.text).expect("der Pause-Report ist JSON");

        assert_eq!(value["paused"], "child");
        assert_eq!(value["child"], child.as_str());
    }

    // ── invoke tests ──────────────────────────────────────────────────────────
    //
    // `make_test_ctx()` registriert bewusst keinen `ManagedAgentSpawner`/
    // `StateStore` in der `ServiceMap`, sodass `ctx.managed_spawner()` und
    // `ctx.state_store()` `None` liefern und `invoke()` den frühen
    // `NotAvailable`-Pfad nimmt. Die Contract- und Pause-Mapper-Tests decken die
    // modell-sichtbare Rückgabe isoliert ab.

    #[test]
    fn completed_child_output_returns_the_child_final_response() {
        let output = completed_child_output(Ok("final child response".to_owned()))
            .expect("available child response must produce tool output");

        assert_eq!(output.text, "final child response");
        assert!(
            !output.text.contains("hat den Turn abgeschlossen"),
            "die Tool-Antwort darf keine generische Abschlussbestätigung sein"
        );
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
    async fn invoke_returns_not_available_in_skeleton_phase() {
        let adapter = AgentToolAdapter::from_operation(Arc::new(AgentOp)).expect("Erwartet Some");
        let (ctx, tmp) = make_test_ctx();
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
            other => panic!("Erwartet OpError::NotAvailable, war: {other:?}"),
        }
    }

    #[tokio::test]
    async fn invoke_rejects_an_undecodable_budget_hint_before_spawning() {
        // `AgentOp` trägt das historische Label "8k"; mit vollständiger
        // Core-Laufzeit im Kontext ist der Budget-Parser die nächste Grenze —
        // und sie muss fail-closed sein statt „kein Limit" zu bedeuten.
        let adapter = AgentToolAdapter::from_operation(Arc::new(AgentOp)).expect("Erwartet Some");
        let (services, _events) = services_with_runtime();
        let (ctx, tmp) = make_test_ctx_with(services);
        let result = adapter
            .invoke(&ctx, serde_json::json!({ "topic": "Rust" }))
            .await;
        std::fs::remove_dir_all(tmp).ok();

        assert!(
            matches!(result, Err(OpError::NotAvailable(ref message)) if message.contains("budget_hint")),
            "ein unlesbares Budget-Label muss das Werkzeug sperren: {result:?}"
        );
    }

    #[tokio::test]
    async fn invoke_rejects_non_object_args_before_child_scheduling() {
        let adapter = AgentToolAdapter::from_operation(Arc::new(AgentOp)).expect("Erwartet Some");
        for args in [
            serde_json::Value::Null,
            serde_json::json!("not-an-object"),
            serde_json::json!(["not-an-object"]),
        ] {
            let (ctx, tmp) = make_test_ctx();
            let result = adapter.invoke(&ctx, args).await;
            std::fs::remove_dir_all(tmp).ok();
            assert!(
                matches!(&result, Err(OpError::InvalidArguments(message)) if message == "arguments must be a JSON object"),
                "nicht-objektartige Args müssen vor dem Scheduling deterministisch abgelehnt werden: {result:?}"
            );
        }
    }

    // ── /agent product boundary tests ───────────────────────────────────────

    #[tokio::test]
    async fn agent_product_rejects_missing_or_unknown_action_before_runtime_lookup() {
        for args in [
            serde_json::json!({}),
            serde_json::json!({ "action": "resume" }),
            serde_json::json!("list"),
        ] {
            let (ctx, tmp) = make_test_ctx();
            let result = AgentToolAdapter::invoke_product(&ctx, args).await;
            std::fs::remove_dir_all(tmp).ok();
            assert!(
                matches!(result, Err(OpError::InvalidArguments(_))),
                "invalid /agent request must be rejected before runtime dispatch: {result:?}"
            );
        }
    }

    #[tokio::test]
    async fn agent_product_rejects_missing_or_empty_target_before_runtime_lookup() {
        for args in [
            serde_json::json!({ "action": "stop" }),
            serde_json::json!({ "action": "budget", "target": "", "budget": { "max_tokens": 1 } }),
        ] {
            let (ctx, tmp) = make_test_ctx();
            let result = AgentToolAdapter::invoke_product(&ctx, args).await;
            std::fs::remove_dir_all(tmp).ok();
            assert!(
                matches!(result, Err(OpError::InvalidArguments(_))),
                "missing or empty targets must fail closed: {result:?}"
            );
        }
    }

    #[tokio::test]
    async fn agent_product_rejects_list_extra_arguments() {
        let (ctx, tmp) = make_test_ctx();
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
    }

    #[tokio::test]
    async fn agent_product_rejects_untyped_budget_before_runtime_lookup() {
        let (ctx, tmp) = make_test_ctx();
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
    }

    #[tokio::test]
    async fn agent_product_reports_missing_spawner_without_success_output() {
        let (ctx, tmp) = make_test_ctx();
        let result =
            AgentToolAdapter::invoke_product(&ctx, serde_json::json!({ "action": "list" })).await;
        std::fs::remove_dir_all(tmp).ok();

        assert!(
            matches!(result, Err(OpError::NotAvailable(ref message)) if message.contains("kein Agent-Spawner")),
            "a missing runtime spawner must not become a synthetic /agent success: {result:?}"
        );
    }

    #[tokio::test]
    async fn agent_product_list_reports_an_empty_child_set_as_json() {
        let (services, _events) = services_with_runtime();
        let (ctx, tmp) = make_test_ctx_with(services);
        let result =
            AgentToolAdapter::invoke_product(&ctx, serde_json::json!({ "action": "list" })).await;
        std::fs::remove_dir_all(tmp).ok();

        let output = result.expect("mit Spawner ist /agent list verfügbar");
        let value: serde_json::Value =
            serde_json::from_str(&output.text).expect("die Liste ist JSON");
        assert_eq!(
            value["children"].as_array().map(Vec::len),
            Some(0),
            "ohne admittierte Kinder ist die Liste leer, nicht 'nicht verfügbar': {}",
            output.text
        );
    }

    #[tokio::test]
    async fn agent_product_stop_on_a_foreign_child_stays_fail_closed() {
        let (services, _events) = services_with_runtime();
        let (ctx, tmp) = make_test_ctx_with(services);
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
    }

    #[tokio::test]
    async fn agent_product_budget_on_a_foreign_child_stays_fail_closed() {
        for args in [
            serde_json::json!({ "action": "budget", "target": "foreign-child" }),
            serde_json::json!({
                "action": "budget",
                "target": "foreign-child",
                "budget": { "max_tokens": 10 }
            }),
        ] {
            let (services, _events) = services_with_runtime();
            let (ctx, tmp) = make_test_ctx_with(services);
            let result = AgentToolAdapter::invoke_product(&ctx, args).await;
            std::fs::remove_dir_all(tmp).ok();

            assert!(
                matches!(result, Err(OpError::NotAvailable(ref message)) if message == "agent target is unavailable in this parent session"),
                "lesen wie setzen bleiben an der Besitzgrenze: {result:?}"
            );
        }
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
    fn test_resolve_authority_reducer_never_widens_any_parent_permission_set() {
        let (ctx, tmp) = make_test_ctx();
        let binding = ctx.sandbox().workspace().clone();
        std::fs::remove_dir_all(tmp).ok();
        let names = KNOWN_AUTHORITY_REDUCERS
            .iter()
            .copied()
            .chain(["reduce_ro", "", "reduce_to_everything"]);
        for name in names {
            let reducer = resolve_authority_reducer(name);
            let ceiling = reducer_ceiling(name)
                .or_else(|| reducer_ceiling("reduce_to_read_only"))
                .expect("read_only ist bekannt");
            for granted in every_permission_subset() {
                let parent = SandboxSpec::from_resolved(binding.clone(), granted.clone())
                    .with_network_scope(harw_sandbox::NetworkScope::from_hosts([
                        "docs.rs".to_owned(),
                    ]));
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
                assert_eq!(
                    child.permissions(),
                    &granted.intersection(&ceiling),
                    "{name}: die Reduktion muss exakt der Schnitt sein"
                );
                assert!(
                    child.ensure_child_of(&parent).is_ok(),
                    "{name}: die Kind-Sandbox muss die Admission-Prüfung bestehen"
                );
            }
        }
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
            Some(set(&[Permission::ReadWorkspace, Permission::ReadCargoRegistry]))
        );
        assert_eq!(
            reducer_ceiling("reduce_to_read_network"),
            Some(set(&[Permission::NetworkAccess])),
            "W5/RD: die Netz-Obergrenze enthält kein ReadWorkspace"
        );
        assert_eq!(reducer_ceiling("reduce_ro"), None);
        for name in KNOWN_AUTHORITY_REDUCERS {
            assert!(reducer_ceiling(name).is_some(), "{name} ohne Obergrenze");
        }
    }

    #[test]
    fn test_reduce_to_read_network_never_reads_the_workspace_and_keeps_only_parent_hosts() {
        let (ctx, tmp) = make_test_ctx();
        let binding = ctx.sandbox().workspace().clone();
        std::fs::remove_dir_all(tmp).ok();
        let parent = SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy(ALL_PERMISSIONS),
        )
        .with_network_scope(harw_sandbox::NetworkScope::from_hosts(["docs.rs".to_owned()]));

        let network = resolve_authority_reducer("reduce_to_read_network")(&parent);
        assert!(!network.permissions().contains(Permission::ReadWorkspace));
        assert!(network.permissions().contains(Permission::NetworkAccess));
        assert_eq!(network.network_scope(), parent.network_scope());

        for name in ["reduce_to_read_only", "reduce_to_read_registry", "reduce_to_read_execute"] {
            let child = resolve_authority_reducer(name)(&parent);
            assert!(!child.permissions().contains(Permission::NetworkAccess), "{name}");
            assert!(child.network_scope().is_empty(), "{name}: Host-Scope muss leer sein");
        }
    }

    #[test]
    fn test_bind_finding_to_question_accepts_only_the_open_question() {
        let finding = serde_json::json!({ "question_id": "q-1" });
        let nested = serde_json::json!({ "question": { "id": "q-1" }, "response_format": {} });
        let flat = serde_json::json!({ "id": "q-1" });
        assert_eq!(bind_finding_to_question(&finding, &nested), Ok(()));
        assert_eq!(bind_finding_to_question(&finding, &flat), Ok(()));

        let foreign = serde_json::json!({ "question": { "id": "q-2" } });
        let error = bind_finding_to_question(&finding, &foreign)
            .expect_err("ein Finding zu einer anderen Frage ist ein Vertragsbruch");
        assert!(error.contains("'q-1'") && error.contains("'q-2'"), "{error}");

        let without_id = serde_json::json!({ "question": "Welche Version?" });
        assert!(bind_finding_to_question(&finding, &without_id).is_err());
        let blank_id = serde_json::json!({ "question": { "id": "  " } });
        assert!(bind_finding_to_question(&finding, &blank_id).is_err());
        let no_claim = serde_json::json!({ "conclusion": "c" });
        assert!(bind_finding_to_question(&no_claim, &nested).is_err());
    }

    #[test]
    fn test_model_effort_field_detects_both_override_fields() {
        assert_eq!(model_effort_field(&serde_json::json!({ "effort": "max" })), Some("effort"));
        assert_eq!(
            model_effort_field(&serde_json::json!({ "reasoning_effort": null })),
            Some("reasoning_effort")
        );
        assert_eq!(model_effort_field(&serde_json::json!({ "topic": "effort" })), None);
    }

    #[tokio::test]
    async fn test_invoke_rejects_a_model_effort_argument_before_spawning() {
        let adapter = AgentToolAdapter::from_operation(Arc::new(AgentOp)).expect("Erwartet Some");
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
                .expect("Spawner registriert");
            let (ctx, tmp) = make_test_ctx_with(services);
            let result = adapter.invoke(&ctx, args).await;
            let active = spawner.active_children_for(ctx.session_id());
            std::fs::remove_dir_all(tmp).ok();
            assert!(
                matches!(&result, Err(OpError::InvalidArguments(message)) if message.contains("effort")),
                "ein Effort-Argument des Modells muss abgelehnt werden: {result:?}"
            );
            assert_eq!(active, 0, "abgelehnter Aufruf darf kein Kind admittieren");
        }
    }

    // ── Send + Sync compile-time check ────────────────────────────────────────

    #[test]
    fn agent_tool_adapter_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<AgentToolAdapter>();
    }
}
