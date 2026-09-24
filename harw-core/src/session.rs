//! `AgentSession` — der Session-State als FSM.
//!
//! Gültige Übergänge werden über `match` erzwungen. Ausgabe geht
//! ausschließlich über `SessionEvent`s, nie direkt an ein Terminal.
//!
//! # Zustandsautomat und Persistenz (W4a/A-SESS)
//! - `Idle --try_start_turn--> Running`; `Running --begin_approval/begin_handoff-->
//!   WaitingForApproval/WaitingForChild` und zurück.
//! - [`AgentSession::complete_turn`] akzeptiert nur den **aktiven** Turn
//!   (`turn_id == current_turn`, eigene `session_id`) aus `Running`,
//!   `WaitingForApproval` oder `WaitingForChild` (F-151).
//! - [`AgentSession::fail`] führt nach `Failed`; [`AgentSession::recover`] ist
//!   der einzige Weg zurück nach `Idle` und repariert dabei offene Tool-Calls
//!   im Live-Verlauf (F-152, F-150).
//! - [`AgentSession::hydrate_from_store`] lädt Verlauf (mit reparierten
//!   offenen Calls) und Sitzungszustand; [`AgentSession::persist_state`]
//!   schreibt Modus, Nutzung und Aktivierung (F-157). Beim Laden wird die
//!   Basis-Aktivierung nie erweitert: geladene Aktivierung ∩ aktuelle Basis.
//!
//! # Fehler
//! [`crate::error::CoreError::TurnRejected`] für unzulässige Übergänge,
//! [`crate::state_store::StateStoreError`] für Persistenz.
//!
//! Session-level tool/instructions/context filtering is controlled via
//! [`crate::activation::SessionActivation`]. See that module for details.
//!
//! Der Interaktionsmodus ([`crate::mode::InteractionMode`]) sitzt eine Ebene
//! darüber: er schneidet Tool-Aktivierung *und* Sandbox-Obergrenze gemeinsam
//! aus der Basis der Session (Agent-IR bzw. Spawn-Kontext) — nie über sie
//! hinaus, aber auch ohne Ratsche zurück — siehe [`AgentSession::set_mode`].
//!
//! # Folgeknoten zu AW2-01/AW2-02: `ContextProgram` je Sitzung — erbt oder bringt mit?
//!
//! [`SpawnContext::ceiling`] wird beim Handoff an ein Kind **geschnitten**
//! (`ManagedAgentSpawner::admit` → `cut_ceiling`, `harw-core/src/child_controller.rs`):
//! eine Decke ist eine Sicherheitsobergrenze, ein Kind darf nie mehr sehen als
//! sein Elternteil zuließ. Ein [`harw_agent_dsl::executable::ContextProgram`]
//! ist etwas anderes: es sagt, *welche* Sektionen ein Agent tatsächlich
//! anfordert (Auswahl), nicht *wie viel* er höchstens anfordern dürfte
//! (Obergrenze). Für diese Auswahl gilt kein Halbverband-Schnitt-Argument —
//! genau wie `detail`/`strength` innerhalb eines Kontextprogramms selbst
//! additiv bleiben (§ `harw_agent_dsl::context_program`, "Welche Achsen
//! `extends` schneidet"), gibt es keinen Sicherheitsgrund, das Programm *als
//! Ganzes* vom Elternteil zu vererben: `harw-registry-defaults` ordnet jeder
//! der neun Rollen (Orchestrator, Worker, Explorer, Researcher, …) ihr eigenes
//! Kontextprogramm zu, passend zu dem, was diese Rolle für ihre Aufgabe lesen
//! muss — ein Planungs-Kind braucht `plan.current` mit vollem Detail, ein
//! Recherche-Kind braucht `web.fetch_allowlist`, keines braucht zwingend das
//! Programm seines Elternteils.
//!
//! **Entscheidung dieses Knotens: ein Kind bringt sein Programm aus seiner
//! eigenen Rollendefinition mit — es erbt das des Elternteils nicht.** Die
//! Decke (`ceiling`) bleibt in jedem Fall die harte, geschnittene
//! Obergrenze: ein mitgebrachtes Programm, das mehr verlangt, als die
//! (geschnittene) Decke zulässt, wird weiterhin von
//! [`harw_agent_dsl::context_program::ContextCeilingAdmission::admits_program`]
//! abgewiesen, bevor es eine Sitzung je erreicht — die Rollenwahl kann die
//! Decke also nicht umgehen, nur innerhalb ihrer wählen.
//!
//! ## Wo dieser Knoten die Grenze zieht
//! [`AgentSession::context_program`] trägt dieses Programm — bewusst als
//! **eigenes Feld auf `AgentSession`, nicht auf [`SpawnContext`]**. Der Grund
//! ist rein mechanisch, nicht konzeptionell: `SpawnContext`s Felder sind alle
//! `pub`, und die Struktur wird an über einem Dutzend Stellen außerhalb dieses
//! Knotens per Literal aufgebaut (`harw-core/src/child_controller.rs`,
//! `harw-core/tests/child_controller.rs`, `harw-core/tests/turn_loop.rs`,
//! `harw-tui/src/app.rs`, `harw-tui/src/approval.rs`,
//! `harw-cli/src/main.rs`, `harw-cli/src/chat.rs`,
//! `harw-cli/src/lifecycle.rs`, `harw-cli/src/job_worker.rs`) — jede davon
//! liegt außerhalb des exklusiven Schreibbereichs dieses Knotens
//! (`harw-agent-dsl/src/executable.rs`, `harw-core/src/session.rs`,
//! `harw-core/src/turn_loop.rs`). Ein neues Pflichtfeld auf `SpawnContext`
//! bricht jede dieser Literal-Konstruktionen, genau wie es beim seinerzeitigen
//! Hinzufügen von `ceiling` selbst der Fall war (siehe dessen Doku oben, „hält
//! die nötige Anpassung … auf ein mechanisches `ceiling: None,` reduziert" —
//! diese Anpassung *fand* an all diesen Stellen statt, in einem Knoten, dessen
//! Schreibbereich sie einschloss). `AgentSession`s Felder sind dagegen privat;
//! jede externe Konstruktion läuft ausschließlich über
//! [`AgentSession::new`]/[`AgentSession::new_with_id`], die beide in dieser
//! Datei liegen. Ein neues privates Feld mit `None`-Default dort bricht daher
//! keine einzige externe Stelle.
//!
//! **Offener Befund:** [`SpawnContext::ceiling`] und
//! [`AgentSession::context_program`] leben deshalb (Stand dieses Knotens) auf
//! zwei verschiedenen Typen, nicht „daneben" auf demselben. Ein
//! `ContextProgram`-Feld direkt auf `SpawnContext` bleibt ein Folge-Knoten,
//! sollte je ein zweiter Konsument neben `ManagedAgentSpawner::admit`
//! entstehen, der das Programm ebenfalls braucht, bevor eine Sitzung existiert.
//!
//! ## Folgeknoten: `admit` verdrahtet das mitgebrachte Programm
//! [`crate::child_controller::ManagedAgentSpawner::admit`] ist die einzige
//! Stelle, die eine Kind-Sitzung tatsächlich erzeugt — also auch die einzige
//! Stelle, die [`AgentSession::with_context_program`] produktiv aufrufen kann.
//! Sie tut das direkt neben der bestehenden IR-Aktivierung
//! (`with_executable_agent_ir`), aus derselben [`ExecutableAgentIr`], die
//! bereits Tool-Surface, Budget und Pause-Sperre liefert: `ir.context_program()`
//! ist das Programm, das die Rolle aus ihrer eigenen Definition mitbringt —
//! erbt nicht vom Elternteil, siehe oben. Trägt die IR nichts Eigenes
//! (`ContextProgram::default()`, der Fall, wenn die Rollen-Definition keine
//! `[context]`/`[context_program]`-Tabelle deklariert), bleibt
//! [`AgentSession::context_program`] `None` — die tragende Auflage dieses
//! Feldes gilt also auch am produktiven Aufrufer, nicht nur in Tests.
//!
//! Die Kontext-Decke ([`SpawnContext::ceiling`]) wird davon unberührt weiter
//! **geschnitten**, nicht mitgebracht: `admit` ruft `cut_ceiling` unverändert
//! an derselben Stelle wie zuvor. Ein mitgebrachtes Programm erweitert diese
//! Decke aber nie — `admit` prüft es unmittelbar neben dem Deckenschnitt gegen
//! die bereits geschnittene Kind-Decke
//! (`ManagedAgentSpawner::describe_context_program_ceiling_violation`) und
//! weist die Admission ab, statt eine zu weitreichende Sektion
//! stillschweigend zu ignorieren — derselbe Fehlschluss wäre eine stille
//! Lücke, kein Absturz, genau wie bei einem übersprungenen `cut_ceiling`.

use crate::activation::{SessionActivation, ToolProfile};
use crate::context_budget::ContextBudget;
use crate::error::{CoreError, CoreResult};
use crate::history::ConversationHistory;
use crate::mode::InteractionMode;
use crate::state_store::{
    ActivationSnapshot, SESSION_STATE_VERSION, SessionStateSnapshot, StateStore, StateStoreResult,
    repair_open_tool_calls,
};
use crate::turn_loop::TurnControl;
use harw_agent_dsl::ExecutableAgentIr;
use harw_agent_dsl::executable::{ContextProgram, SnapshotId};
use harw_agent_dsl::roles::AgentRoleId;
use harw_authority::{PermissionRequest, SandboxSpec};
use harw_catalog::{AgentSuggestions, SpawnCapabilitySnapshot};
use harw_context::ContextCeiling;
use harw_extension_api::ExtensionRegistry;
use harw_observe::TraceContext;
use harw_protocol::events::SessionEvent;
use harw_protocol::events::TurnEvent;
use harw_tools::{ToolCall, ToolName};
use harw_types::{
    AgentRole, ApprovalActor, ItemId, ModelId, ProviderId, ReasoningEffort, SessionId, ThreadId,
    TokenUsage, ToolCallId, TurnId,
};
use std::collections::BTreeSet;
use tokio::sync::mpsc;

/// Session-State FSM — ungültige Übergänge sind über `match` abgesichert.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionState {
    Idle,
    Running,
    WaitingForApproval,
    /// NEU ggü. codex — für Orchestrator→Worker Handoffs.
    WaitingForChild,
    Failed(String),
}

impl std::fmt::Display for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle => write!(f, "Idle"),
            Self::Running => write!(f, "Running"),
            Self::WaitingForApproval => write!(f, "WaitingForApproval"),
            Self::WaitingForChild => write!(f, "WaitingForChild"),
            Self::Failed(msg) => write!(f, "Failed({msg})"),
        }
    }
}

pub struct AgentSession {
    id: SessionId,
    role: AgentRole,
    parent_session_id: Option<SessionId>,
    state: SessionState,
    registry: ExtensionRegistry,
    history: ConversationHistory,
    /// Der aktuell laufende / pausierte Turn (gesetzt ab `try_start_turn`,
    /// erhalten über einen Handoff hinweg, gelöscht bei `complete_turn`).
    current_turn: Option<TurnId>,
    /// Der [`TurnControl`]-Steuerblock des aktuell laufenden/pausierten Turns.
    ///
    /// Analog zu [`Self::current_turn`], gleiche Lebensdauer: gesetzt sobald
    /// ein Turn über [`crate::turn_loop::run_turn`]/`run_turn_durable` beginnt,
    /// erhalten über eine Handoff- oder Rückfrage-Pause hinweg, gelöscht bei
    /// [`Self::complete_turn`]. Schließt die in `turn_loop.rs`s Moduldoku
    /// („Fünfter Nachtrag") dokumentierte Lücke: ohne dieses Feld bauten sich
    /// `resume_after_child`/`resume_after_approval` (bzw. ihre
    /// `_durable`-Varianten) nach jeder Pause einen frischen, unbegrenzten
    /// `TurnControl::new()` mit eigenem `CancelToken` — ein vor der Pause
    /// gültiger Abbruchwunsch (Ctrl+C) ging dadurch verloren. `TurnControl`
    /// ist bereits `#[derive(Clone)]`; ein Klon teilt denselben `CancelToken`
    /// und denselben Zähler mit dem Original, ein `Arc` ist deshalb nicht
    /// nötig.
    active_turn_control: Option<TurnControl>,
    pending_approval: Option<PendingApproval>,
    pending_handoff: Option<PendingHandoff>,
    spawn_context: Option<SpawnContext>,
    context_budget: ContextBudget,
    /// Reasoning-Effort-Level, das jeder Turn dieser Session an den Provider
    /// durchreicht. `None` lässt den Provider seinen Default wählen.
    reasoning_effort: Option<ReasoningEffort>,
    /// Aktives Modell, das für Turns dieser Session bevorzugt wird.
    /// `None` lässt den Provider seinen Catalog-Default wählen.
    active_model: Option<ModelId>,
    /// Aktiver Provider, der für Turns dieser Session verwendet wird.
    /// `None` lässt den Session-Manager seinen konfigurierten Default wählen.
    active_provider: Option<ProviderId>,
    event_tx: mpsc::UnboundedSender<SessionEvent>,
    /// Optionaler Sink für Live-Turn-Events (Tool-Calls, Reasoning,
    /// Item-Updates). Andere Granularität als `event_tx` — daher ein
    /// eigener, separater Kanal statt Wiederverwendung.
    turn_event_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
    /// Optionaler agenten-übergreifender Live-Bus (siehe
    /// [`crate::agent_events`]); jedes Turn-Event wird mit Absender-Kennung
    /// zusätzlich hierher gespiegelt.
    agent_events: Option<crate::agent_events::AgentEventHub>,
    /// Löst bei einem Modellwechsel das neue Kontextfenster auf, damit
    /// Auto-Compaction und Byte-Budget dem aktiven Modell folgen.
    context_window_resolver: Option<std::sync::Arc<crate::child_controller::ContextWindowResolver>>,
    /// Vorgabe-Grenzen für Turns ohne eigene Grenzen (siehe `run_turn`).
    default_turn_limits: Option<crate::turn_loop::TurnLimits>,
    /// Startzeitpunkt des offenen Handoffs (für `ChildCompleted::duration_ms`).
    handoff_started_at: Option<std::time::Instant>,
    /// Session-level filter controlling which tools, instructions providers,
    /// and context providers are exposed to the model. Defaults to
    /// [`SessionActivation::default()`] (Full profile, no overrides), which
    /// exposes every registered tool.
    activation: SessionActivation,
    /// Basis-Aktivierung: die Tool-Freigabe, die diese Session bei ihrem
    /// Aufbau bekommen hat — aus der Agent-IR
    /// ([`AgentSession::with_executable_agent_ir`]) bzw. aus
    /// [`AgentSession::with_activation`] —, bevor ein Interaktionsmodus sie
    /// verengt hat.
    ///
    /// `apply_mode` schneidet **immer** von hier aus, nie vom aktuellen Wert.
    /// Damit gilt beides zugleich: ein Modus ist reversibel (Explore → Work
    /// stellt genau die Basis wieder her, keine Ratsche) und er kann nie mehr
    /// freigeben, als die Basis trug (ein `forbidden` aus der Agent-Definition
    /// bleibt in jedem Modus verboten).
    base_activation: SessionActivation,
    /// Basis-Sandbox: die Autorität, mit der der Spawn-Kontext angehängt
    /// wurde, vor jedem Modus-Schnitt. Dieselbe Rolle wie `base_activation`,
    /// eine Achse tiefer.
    ///
    /// `Option`, weil [`harw_authority::SandboxSpec`] bewusst kein `Default`
    /// hat: eine Sandbox entsteht ausschließlich aus einer aufgelösten
    /// [`harw_authority::WorkspaceBinding`] plus Policy-Entscheidung
    /// ([`harw_authority::SandboxSpec::from_resolved`]), es gibt also keinen
    /// neutralen Wert, den [`AgentSession::new_with_id`] erfinden dürfte.
    /// `None` heißt „diese Session hat keine Sandbox", nicht „unbegrenzt":
    /// ohne Spawn-Kontext gibt es nichts zu schneiden, und die
    /// Tool-Ausführung lehnt eine solche Session ohnehin ab.
    base_sandbox: Option<SandboxSpec>,
    /// Aufsummierte Token-Nutzung **aller** Turns dieser Session.
    ///
    /// Ohne diesen Akkumulator wäre eine Token-Obergrenze für Kind-Agenten
    /// nicht durchsetzbar: `TurnCompleted` trägt die Nutzung zwar als Event,
    /// aber ein Budget-Wächter besitzt den Event-Kanal nicht. Wird von
    /// [`AgentSession::complete_turn`] fortgeschrieben.
    total_usage: TokenUsage,
    /// Interaktionsmodus dieser Session. Er ist keine Anzeige, sondern die
    /// Quelle der Tool-Aktivierung und der Sandbox-Obergrenze; gewechselt wird
    /// er ausschließlich über [`AgentSession::set_mode`].
    mode: InteractionMode,
    /// Content-addressable identifier of the frozen executable policy that
    /// configured this session, when one was supplied at construction.
    executable_snapshot_id: Option<SnapshotId>,
    /// Welche Kontext-Sektionen dieser Agent tatsächlich anfordert (Auswahl),
    /// getrennt von [`SpawnContext::ceiling`] (Obergrenze). `None`: die
    /// Runtime verwendet ihre Standard-Kontextmontage unverändert — diese
    /// tragende Auflage gilt unabhängig davon, ob dieses Feld je gesetzt wird
    /// (kein bestehender Aufrufer sieht ohne expliziten
    /// [`Self::with_context_program`]-Aufruf einen anderen Kontext). Siehe die
    /// Moduldoku oben ("Folgeknoten zu AW2-01/AW2-02") für die Begründung,
    /// warum dieses Feld hier und nicht auf [`SpawnContext`] liegt, und warum
    /// ein Kind sein Programm mitbringt statt es zu erben.
    context_program: Option<ContextProgram>,
    /// Richtlinie für automatisches Verdichten des Verlaufs dieser Session.
    /// `None`: keine automatische Verdichtung.
    auto_compact: Option<crate::auto_compact::AutoCompactPolicy>,
    /// Beobachter, der nach jeder Verdichtung (siehe
    /// [`crate::compaction::compact_session`]) benachrichtigt wird. `None`:
    /// kein Beobachter registriert.
    compaction_observer: Option<std::sync::Arc<dyn crate::compaction::CompactionObserver>>,
    /// Beobachter, der über das Ergebnis jedes ausgeführten Tool-Aufrufs
    /// dieser Session benachrichtigt wird (Projektgedächtnis-Erfassung,
    /// siehe [`crate::capture::ToolOutcomeObserver`]). `None`: kein
    /// Beobachter registriert.
    tool_outcome_observer: Option<std::sync::Arc<dyn crate::capture::ToolOutcomeObserver>>,
    /// Fest zugeordnetes Provider-/Modellpaar für den Compaction-
    /// Zusammenfassungs-Aufruf dieser Session (Addendum C: interne
    /// Modellstellen). `(None, None)`: kein Pin gesetzt — `maybe_compact`
    /// lässt `CompactionPlan::summary_provider`/`summary_model` unangetastet
    /// (Katalog-/Hauptmodell-Default).
    compaction_summary_model: (Option<ProviderId>, Option<ModelId>),
    /// Schwellenwerte der Turn-Wächter dieser Session (Addendum F+G). `None`
    /// im Builder-Setter bedeutet „`GuardPolicy::default()` verwenden" —
    /// [`Self::guard_policy`] löst das bereits auf, das Feld selbst trägt
    /// deshalb immer einen konkreten Wert.
    guard_policy: crate::guard::GuardPolicy,
    /// Beobachter, der über jedes von [`crate::guard::TurnGuard`] erkannte
    /// Drift-Ereignis dieser Session benachrichtigt wird. `None`: kein
    /// Beobachter registriert.
    drift_observer: Option<std::sync::Arc<dyn crate::guard::DriftObserver>>,
    /// Berater, der vor jeder Werkzeugausführung dieser Session befragt wird,
    /// ob ein bekannter Pitfall zutrifft. `None`: keine Beratung.
    pitfall_advisor: Option<std::sync::Arc<dyn crate::guard::PitfallAdvisor>>,
    /// Beobachter, der nach jeder Modellrunde und jedem Tool-Ergebnis dieser
    /// Session über Fortschritt benachrichtigt wird (Lease-Erneuerung durch
    /// `ManagedAgentSpawner`). `None`: kein Beobachter registriert.
    progress_observer: Option<std::sync::Arc<dyn crate::guard::ProgressObserver>>,
    /// Laufende Kalibrierung Bytes→Tokens dieser Session. Der Turn-Loop
    /// schätzt vor jedem Request mit ihr und füttert sie danach mit der vom
    /// Provider gemeldeten Prompt-Token-Zahl nach.
    token_calibration: crate::context_budget::TokenCalibration,
    /// Obergrenze für Ausgabe-Tokens je Modellanfrage (Output-Reserve).
    /// `None`: Provider-Default.
    max_output_tokens: Option<u64>,
    /// Markiert, dass vor dem nächsten Modell-Request kompaktiert werden muss
    /// (z. B. nach einem Wechsel auf ein Modell mit kleinerem Fenster).
    pending_compaction: bool,
    /// Explizit konfigurierter `max_history_bytes`-Override. Ist er gesetzt,
    /// leitet [`AgentSession::set_active_model`] das History-Budget nicht aus
    /// dem neuen Fenster ab, sondern behält diesen Wert.
    configured_max_history_bytes: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct SpawnContext {
    pub sandbox: SandboxSpec,
    pub suggestions: Option<AgentSuggestions>,
    /// Target-role capability contract resolved by trusted runtime/catalog
    /// state at admission. This is distinct from advisory suggestions: only
    /// this snapshot may be considered for activation by a registry factory.
    pub capability_snapshot: Option<SpawnCapabilitySnapshot>,
    /// Identity that may answer approvals initiated by this session. It is
    /// attached by trusted ingress/session construction, never by turn JSON.
    pub approval_actor: Option<ApprovalActor>,
    /// The organizational role (§3 DSL spawn matrix) this session was
    /// admitted under. Used by `ManagedAgentSpawner::admit` to enforce the
    /// closed spawn matrix (`harw_agent_dsl::roles::can_spawn`) before a
    /// child is created — a session's own organizational role determines
    /// which target roles it may ever spawn, independent of sandbox/depth
    /// limits.
    pub organizational_role: AgentRoleId,
    /// Exact child-orchestrator role names that this already admitted agent
    /// may create. Derived exclusively from its frozen agent definition;
    /// absent means deny all child-orchestrator spawns.
    pub allowed_child_orchestrators: Vec<String>,
    /// Der Trace-Kontext, unter dem dieser Agent läuft.
    ///
    /// Wird beim Handoff an ein Kind **vererbt**, nicht neu erzeugt: ein Kind
    /// gehört zur selben Arbeit wie sein Elternteil, und genau das soll die
    /// gemeinsame `trace_id` später sichtbar machen. `ManagedAgentSpawner::admit`
    /// baut daraus den Trace des Kindes: dieselbe `trace_id`, eine frische
    /// `span_id` für das Kind, die `span_id` des Elternteils als
    /// `parent_span_id`. Ein Elternteil ohne Trace (`None`) vererbt ebenfalls
    /// `None` — dieses Feld erfindet nie einen Wurzel-Trace für eine Stelle,
    /// die keine Wurzel ist.
    pub trace: Option<TraceContext>,
    /// Die Kontext-Decke, unter der dieser Agent — und jedes seiner Kinder —
    /// laufen darf.
    ///
    /// # Warum hier, warum im selben Schritt wie die Berechtigungen
    /// Wird beim Handoff an ein Kind **im selben Schritt** geschnitten wie
    /// die Berechtigungen (siehe
    /// [`crate::child_controller::ManagedAgentSpawner::admit`], unmittelbar
    /// neben der Sandbox-Eskalationsprüfung und der Trace-Vererbung, nicht
    /// davor, nicht danach, nicht in einer eigenen Funktion). Läge der
    /// Deckenschnitt in einem zweiten, separaten Schritt, gäbe es einen
    /// Zustand dazwischen, in dem ein Kind bereits ein `ContextProgram`
    /// trägt, dessen Decke noch niemand geschnitten hat — ein Fehler in der
    /// Reihenfolge wäre dann eine stille Lücke, kein Absturz, und genau
    /// solche Lücken bleiben lange unentdeckt. Deshalb liegt dieses Feld
    /// direkt neben `trace`, nicht in einem eigenen Modul.
    ///
    /// # `None` ist fail-closed, nicht "unbegrenzt"
    /// Ein Elternteil ohne eigene Decke — nur an einer extern registrierten
    /// Wurzel möglich, siehe
    /// [`crate::child_controller::ManagedAgentSpawner::with_external_root_parent`] —
    /// vererbt seinem Kind die maximal restriktive Decke (keine Sektion,
    /// [`harw_context::TrustClass::Data`], kein Budget), niemals gar keine
    /// Grenze. Ein fehlendes Feld darf nie zu mehr Autorität führen als ein
    /// explizit gesetztes.
    ///
    /// `Option` statt eines Pflichtfelds folgt demselben Muster wie `trace`
    /// oben: es hält die nötige Anpassung an jeder bereits bestehenden
    /// externen Konstruktionsstelle (`harw-cli`, `harw-tui`) auf ein
    /// mechanisches `ceiling: None,` reduziert, ohne dass diese Stellen eine
    /// echte Decke erfinden müssten — sicher genau deshalb, weil `None` dort
    /// als geschlossen, nicht als offen, gelesen wird.
    pub ceiling: Option<ContextCeiling>,
}

/// Vorgabe-Wartezeit auf eine Freigabeentscheidung, bevor sie als abgelaufen
/// gilt (Interaktionsvertrag §4.4).
///
/// # Beschreibung
/// Re-Export von [`harw_types::DEFAULT_APPROVAL_TIMEOUT`] — nicht als eigene
/// `harw-core`-Politik neu definiert. `harw-types` ist der einzige
/// gemeinsame, zyklusfreie Ort für diesen Wert, weil sowohl `harw-core`
/// ([`PendingApproval::timeout_at`]) als auch `harw-protocol`
/// (`ApprovalRequest::timeout_at`) ihn brauchen und `harw-core` nicht von
/// `harw-protocol` abhängen darf. [`PendingApproval`] speichert `timeout_at`
/// als `requested_at + DEFAULT_APPROVAL_TIMEOUT` (sofern
/// [`AgentSession::begin_approval`] keine eigene Wartezeit erhält), und jede
/// Oberfläche — TUI wie ein künftiges Channel-Binding — liest denselben
/// Ablaufzeitpunkt statt eine eigene Frist zu erfinden.
pub use harw_types::DEFAULT_APPROVAL_TIMEOUT;

/// Begründung, die jede Oberfläche für eine durch Zeitablauf erzwungene
/// Ablehnung verwendet (Interaktionsvertrag §4.4: „timed-out-denied").
///
/// Re-Export von [`harw_types::APPROVAL_TIMEOUT_REASON`] — aus demselben
/// Grund wie [`DEFAULT_APPROVAL_TIMEOUT`] oben: eine einzige Zeichenkette
/// statt einer Kopie je Front-End/Crate.
pub use harw_types::APPROVAL_TIMEOUT_REASON;

#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub call: ToolCall,
    pub request: ItemId,
    pub actor: ApprovalActor,
    /// Wanduhrzeit, zu der die Pause begann (Serveruhr, `jiff::Timestamp::now()`
    /// am Aufrufort von [`AgentSession::begin_approval`]).
    pub requested_at: jiff::Timestamp,
    /// Ablaufzeitpunkt: `requested_at + timeout`. Nach Erreichen dieses
    /// Zeitpunkts muss jede Oberfläche [`ApprovalResolution::timed_out`]
    /// liefern, statt weiter auf eine Antwort zu warten (§4.4).
    pub timeout_at: jiff::Timestamp,
}

impl PendingApproval {
    /// Prüft, ob diese Pause zu `now` bereits abgelaufen ist.
    ///
    /// # Arguments
    /// - `now` (`jiff::Timestamp`): die Wanduhrzeit der Oberfläche, die prüft.
    ///
    /// # Returns
    /// `true`, wenn `now` `timeout_at` erreicht oder überschritten hat — die
    /// Grenze ist inklusiv, wie beim TTL-Vergleich des durablen
    /// `ApprovalStore` (C-APPR).
    #[must_use]
    pub fn is_timed_out(&self, now: jiff::Timestamp) -> bool {
        now >= self.timeout_at
    }
}

#[derive(Debug, Clone)]
pub struct PendingHandoff {
    pub child: SessionId,
    pub call_id: harw_types::ToolCallId,
    pub role: String,
}

/// Losgelöster Live-Event-Sender einer Session (siehe
/// [`AgentSession::live_emitter`]). Billig klonbar, `Send + Sync`.
#[derive(Debug, Clone, Default)]
pub struct LiveEmitter {
    turn_tx: Option<mpsc::UnboundedSender<TurnEvent>>,
    hub: Option<(
        crate::agent_events::AgentEventHub,
        SessionId,
        Option<SessionId>,
        String,
    )>,
}

impl LiveEmitter {
    /// `true`, wenn überhaupt jemand zuhört.
    #[must_use]
    pub fn is_observed(&self) -> bool {
        self.turn_tx.is_some()
            || self
                .hub
                .as_ref()
                .is_some_and(|(hub, ..)| hub.observer_count() > 0)
    }

    /// Sendet ein Turn-Event an Turn-Sink und Agenten-Bus (best effort).
    pub fn emit(&self, event: TurnEvent) {
        if let Some((hub, agent, parent, role)) = &self.hub
            && hub.observer_count() > 0
        {
            hub.publish(crate::agent_events::AgentEvent {
                agent: agent.clone(),
                parent: parent.clone(),
                role: role.clone(),
                kind: crate::agent_events::AgentEventKind::Turn(event.clone()),
            });
        }
        if let Some(tx) = &self.turn_tx {
            let _ = tx.send(event);
        }
    }
}

/// Handle das ein laufender Turn hält.
pub struct TurnHandle {
    pub turn_id: TurnId,
    pub session_id: SessionId,
}

/// Rejection wenn `try_start_turn` fehlschlägt.
#[derive(Debug)]
pub enum TurnRejection {
    NotIdle(SessionState),
}

impl std::fmt::Display for TurnRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotIdle(state) => write!(f, "session not idle: {state}"),
        }
    }
}
impl std::error::Error for TurnRejection {}

/// Ergebnis von [`AgentSession::hydrate_from_store`].
///
/// # Description
/// Beschreibt, was beim Hydrieren einer frischen Session tatsächlich geladen
/// wurde — für Tracing, Anzeige und Tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionHydration {
    /// `true`, wenn die Session bereits einen Verlauf hatte und deshalb
    /// nichts geladen wurde.
    pub skipped: bool,
    /// Anzahl geladener Verlaufs-Items (inklusive synthetischer Ergebnisse).
    pub history_items: usize,
    /// Tool-Calls, die beim Laden ein synthetisches Fehler-Ergebnis bekamen.
    pub repaired_tool_calls: Vec<ToolCallId>,
    /// `true`, wenn ein persistierter Sitzungszustand angewandt wurde.
    pub state_restored: bool,
}

// Baut die Modus-Decke (Tool-Profil + Positivliste) für `mode`, unabhängig
// von jeder konkreten Session — eine reine Funktion von `InteractionMode`
// nach `SessionActivation`. Gemeinsam genutzt von `AgentSession::apply_mode`
// (Schnitt mit der Basis wird zur aktuellen Aktivierung) und
// `AgentSession::mode_ceiling` (derselbe Schnitt, ohne ihn zu speichern),
// damit beide exakt dieselbe Modus-Seite sehen.
fn mode_activation(mode: InteractionMode) -> SessionActivation {
    let mut activation = SessionActivation::new(mode.tool_profile());
    if let Some(names) = mode.allowed_tools() {
        for name in names {
            activation.enable_tool(ToolName::new(*name));
        }
    }
    activation
}

impl AgentSession {
    pub fn new(
        role: AgentRole,
        parent_session_id: Option<SessionId>,
        registry: ExtensionRegistry,
        event_tx: mpsc::UnboundedSender<SessionEvent>,
    ) -> Self {
        Self::new_with_id(
            SessionId::new(),
            role,
            parent_session_id,
            registry,
            event_tx,
        )
    }

    /// Creates a session with a caller-supplied identifier.
    pub fn new_with_id(
        session_id: SessionId,
        role: AgentRole,
        parent_session_id: Option<SessionId>,
        registry: ExtensionRegistry,
        event_tx: mpsc::UnboundedSender<SessionEvent>,
    ) -> Self {
        Self {
            id: session_id,
            role,
            parent_session_id,
            state: SessionState::Idle,
            registry,
            history: ConversationHistory::new(),
            current_turn: None,
            active_turn_control: None,
            pending_approval: None,
            pending_handoff: None,
            spawn_context: None,
            context_budget: ContextBudget::default(),
            reasoning_effort: None,
            active_model: None,
            active_provider: None,
            event_tx,
            turn_event_tx: None,
            activation: SessionActivation::default(),
            base_activation: SessionActivation::default(),
            base_sandbox: None,
            total_usage: TokenUsage::default(),
            mode: InteractionMode::default(),
            executable_snapshot_id: None,
            context_program: None,
            auto_compact: None,
            agent_events: None,
            handoff_started_at: None,
            default_turn_limits: None,
            context_window_resolver: None,
            compaction_observer: None,
            tool_outcome_observer: None,
            compaction_summary_model: (None, None),
            guard_policy: crate::guard::GuardPolicy::default(),
            drift_observer: None,
            pitfall_advisor: None,
            progress_observer: None,
            token_calibration: crate::context_budget::TokenCalibration::default(),
            max_output_tokens: None,
            pending_compaction: false,
            configured_max_history_bytes: None,
        }
    }

    /// Applies the runtime policy carried by a frozen executable agent IR.
    ///
    /// The resulting tool surface is deny-by-default: it starts from the
    /// minimal profile, admits only the IR's admitted names, then applies its
    /// forbidden names as final denials. The IR snapshot ID is retained for
    /// audit and correlation.
    ///
    /// Diese Fläche wird als **Basis** hinterlegt (`base_activation`), nicht
    /// nur als aktueller Wert: jeder spätere Modus-Wechsel schneidet gegen
    /// sie, statt sie zu ersetzen. Ein `forbidden`-Name der Agent-Definition
    /// überlebt deshalb jedes `/mode` — siehe [`Self::set_mode`]. Der
    /// anschließende `apply_mode`-Aufruf hält die Reihenfolge der
    /// Builder-Aufrufe egal; im Default-Modus [`InteractionMode::Chat`] ist
    /// der Schnitt die Identität.
    #[must_use]
    pub fn with_executable_agent_ir(mut self, executable: &ExecutableAgentIr) -> Self {
        let mut activation = SessionActivation::new(ToolProfile::Minimal);

        for name in executable.tool_surface().admitted() {
            activation.enable_tool(ToolName::new(name.clone()));
        }
        for name in executable.tool_surface().forbidden() {
            activation.disable_tool(ToolName::new(name.clone()));
        }

        self.base_activation = activation;
        self.executable_snapshot_id = Some(executable.snapshot_id());
        self.apply_mode();
        self
    }

    /// Returns the frozen executable policy snapshot ID, if this session was
    /// constructed through [`Self::with_executable_agent_ir`].
    #[must_use]
    pub fn executable_snapshot_id(&self) -> Option<&SnapshotId> {
        self.executable_snapshot_id.as_ref()
    }

    /// Attaches the immutable, trusted context established before the session
    /// started. It is intentionally unavailable to model/tool JSON.
    ///
    /// Die übergebene Sandbox wird als **Basis-Autorität** dieser Session
    /// festgehalten (`base_sandbox`) und sofort mit der Obergrenze des
    /// aktuellen [`InteractionMode`] geschnitten. Damit hängt die Autorität
    /// nicht von der Reihenfolge der Builder-Aufrufe ab:
    /// `with_mode(...).with_spawn_context(...)` und
    /// `with_spawn_context(...).with_mode(...)` ergeben dieselbe Sandbox. Für
    /// den Default-Modus [`InteractionMode::Chat`] ist der Schnitt die
    /// Identität, bestehende Aufrufer sehen also keine Änderung.
    ///
    /// Die Basis ist die Obergrenze, nicht ein zweiter Speicher neben der
    /// Wahrheit: sie wird ausschließlich hier gesetzt — von der Stelle also,
    /// die die vertrauenswürdige Autorität überhaupt erst anliefert — und von
    /// da an nur noch geschnitten.
    #[must_use]
    pub fn with_spawn_context(mut self, spawn_context: SpawnContext) -> Self {
        self.base_sandbox = Some(spawn_context.sandbox.clone());
        self.spawn_context = Some(spawn_context);
        self.apply_mode();
        self
    }

    /// Liefert den aktuellen Interaktionsmodus der Session.
    ///
    /// # Returns
    /// Den [`InteractionMode`] (`Copy`), der zuletzt über
    /// [`Self::with_mode`] oder [`Self::set_mode`] gesetzt wurde;
    /// [`InteractionMode::Chat`], wenn nie einer gesetzt wurde.
    #[must_use]
    pub fn mode(&self) -> InteractionMode {
        self.mode
    }

    /// Builder-Variante von [`Self::set_mode`] für den Aufbau einer Session.
    ///
    /// # Beschreibung
    /// Wendet den Modus genauso an wie [`Self::set_mode`] — Tool-Aktivierung
    /// und Sandbox-Schnitt —, sendet aber **kein**
    /// [`TurnEvent::ModeChanged`]: beim Aufbau hat noch kein Wechsel
    /// stattgefunden, und ein Sink ist zu diesem Zeitpunkt in der Regel noch
    /// gar nicht angehängt.
    ///
    /// # Arguments
    /// - `mode` (`InteractionMode`): der zu setzende Modus (`Copy`).
    ///
    /// # Returns
    /// `Self` mit gesetztem und bereits angewandtem Modus.
    ///
    /// # Beispiele
    /// ```rust,no_run
    /// use harw_core::mode::InteractionMode;
    /// // session.with_mode(InteractionMode::Explore);
    /// ```
    #[must_use]
    pub fn with_mode(mut self, mode: InteractionMode) -> Self {
        self.mode = mode;
        self.apply_mode();
        self
    }

    /// Wechselt den Modus und wendet ihn sofort an: Tool-Aktivierung und
    /// Sandbox werden **von der Basis aus** mit dem Modus geschnitten.
    /// Der Spawn-Kontext behält seinen Workspace; nur die Permissions schrumpfen.
    ///
    /// # Beschreibung
    /// Der Modus ist eine durchgesetzte Grenze, kein Hinweis an das Modell.
    /// Konkret geschieht zweierlei:
    /// 1. Aus [`InteractionMode::tool_profile`] und
    ///    [`InteractionMode::allowed_tools`] entsteht eine frische
    ///    [`SessionActivation`] — die „Modus-Decke". Die Aktivierung der
    ///    Session ist deren Schnitt mit der Basis-Aktivierung
    ///    ([`SessionActivation::intersect`], monoton: das Ergebnis erlaubt nie
    ///    mehr als jede Seite für sich). Basis ist, was
    ///    [`Self::with_executable_agent_ir`] bzw. [`Self::with_activation`]
    ///    hinterlegt hat — ein `forbidden`-Name der Agent-Definition bleibt
    ///    deshalb in **jedem** Modus verboten, auch in
    ///    [`InteractionMode::Work`]. `allowed_tools() == None` bedeutet „keine
    ///    namensbasierte Einschränkung durch den Modus", nicht „alles erlaubt":
    ///    die Basis gilt weiter.
    /// 2. Die Sandbox des Spawn-Kontexts wird als Schnitt der **Basis-Sandbox**
    ///    mit [`InteractionMode::permission_ceiling`] neu gesetzt
    ///    ([`harw_authority::SandboxSpec::restrict`]). Der
    ///    [`harw_authority::NetworkScope`] bleibt unangetastet — eine
    ///    Permission-Obergrenze sagt nichts über einzelne Ziele aus, weder über
    ///    Hostnamen ([`harw_authority::EgressTarget::Host`],
    ///    [`harw_authority::EgressTarget::DnsSuffix`]) noch über Adressbereiche
    ///    ([`harw_authority::EgressTarget::Cidr`]).
    ///
    /// Ohne Spawn-Kontext entfällt Schritt 2 ersatzlos: es gibt dann keine
    /// Autorität, die zu schneiden wäre, und die Tool-Ausführung lehnt eine
    /// solche Session ohnehin ab.
    ///
    /// # Warum von der Basis, nicht vom aktuellen Wert
    /// Beide Achsen sind dadurch **reversibel und trotzdem gedeckelt**:
    /// `Explore` → `Work` stellt exakt die Basis wieder her (keine Ratsche,
    /// die eine Session nach einem einzigen `/mode explore` dauerhaft
    /// entrechtet), und kein Modus kommt je über die Basis hinaus. Kumulativ
    /// auf den aktuellen Wert zu schneiden hätte nur die erste Hälfte; die
    /// Basis zu ersetzen — der frühere Zustand — nur die zweite.
    ///
    /// Einzel-Overrides, die zur Laufzeit über [`Self::activation_mut`]
    /// gesetzt wurden, gehören nicht zur Basis und fallen bei jedem
    /// Modus-Wechsel weg: andernfalls könnte ein alter `enable_tool`-Override
    /// ein Werkzeug in einen engeren Modus hineinretten.
    ///
    /// # Arguments
    /// - `mode` (`InteractionMode`): der neue Modus (`Copy`, kein Ownership-Transfer).
    ///
    /// # Panics
    /// Keine.
    ///
    /// # Nebenläufigkeit
    /// Verlangt exklusiven Zugriff (`&mut self`) und hält keine Sperre. Das
    /// [`TurnEvent::ModeChanged`] geht best-effort über den optionalen
    /// Turn-Event-Sink; ein abgehängter Empfänger wird ignoriert.
    pub fn set_mode(&mut self, mode: InteractionMode) {
        self.mode = mode;
        self.apply_mode();
        if let Some(tx) = &self.turn_event_tx {
            let _ = tx.send(TurnEvent::ModeChanged {
                mode: mode.as_str().to_owned(),
            });
        }
    }

    // Setzt den bereits in `self.mode` hinterlegten Modus durch: Activation und
    // Sandbox jeweils als Schnitt der Basis mit der Modus-Decke. Bewusst ohne
    // Event — der Aufrufer entscheidet, ob ein Wechsel stattgefunden hat.
    //
    // Idempotent und reihenfolgeunabhängig: die Funktion liest nur `mode`,
    // `base_activation` und `base_sandbox` und schreibt nur die abgeleiteten
    // Werte. Deshalb darf jeder Builder-Schritt, der eine Basis setzt, sie
    // anschließend aufrufen.
    fn apply_mode(&mut self) {
        self.activation = self.base_activation.intersect(&mode_activation(self.mode));

        let ceiling = self.mode.permission_ceiling();
        if let (Some(context), Some(base)) =
            (self.spawn_context.as_mut(), self.base_sandbox.as_ref())
        {
            context.sandbox = base.restrict(&PermissionRequest::from_permissions(ceiling.iter()));
        }
    }

    #[must_use]
    pub fn spawn_context(&self) -> Option<&SpawnContext> {
        self.spawn_context.as_ref()
    }

    /// Overrides the model-visible context budget for this session. Durable
    /// history is unchanged; only request assembly is bounded.
    #[must_use]
    pub fn with_context_budget(mut self, context_budget: ContextBudget) -> Self {
        self.context_budget = context_budget;
        self
    }

    /// Nicht-konsumierende Variante von [`Self::with_context_budget`].
    pub fn set_context_budget(&mut self, context_budget: ContextBudget) {
        self.context_budget = context_budget;
    }

    #[must_use]
    pub fn context_budget(&self) -> ContextBudget {
        self.context_budget
    }

    /// Hinterlegt einen explizit konfigurierten `max_history_bytes`-Override
    /// (z. B. `[harness.compaction].max_history_bytes`).
    ///
    /// # Beschreibung
    /// Ein gesetzter Wert wird sofort ins aktuelle [`ContextBudget`]
    /// übernommen und überlebt jeden späteren Modellwechsel über
    /// [`Self::set_active_model`]. `None` lässt das Budget unverändert und
    /// erlaubt die Ableitung aus dem Kontextfenster.
    #[must_use]
    pub fn with_configured_max_history_bytes(mut self, max_history_bytes: Option<usize>) -> Self {
        self.set_configured_max_history_bytes(max_history_bytes);
        self
    }

    /// Nicht-konsumierende Variante von
    /// [`Self::with_configured_max_history_bytes`].
    pub fn set_configured_max_history_bytes(&mut self, max_history_bytes: Option<usize>) {
        self.configured_max_history_bytes = max_history_bytes;
        if let Some(bytes) = max_history_bytes {
            self.context_budget.max_history_bytes = bytes;
        }
    }

    /// Explizit konfigurierter `max_history_bytes`-Override, falls vorhanden.
    #[must_use]
    pub fn configured_max_history_bytes(&self) -> Option<usize> {
        self.configured_max_history_bytes
    }

    /// Bytes→Tokens-Kalibrierung dieser Session (nur lesend).
    #[must_use]
    pub fn token_calibration(&self) -> &crate::context_budget::TokenCalibration {
        &self.token_calibration
    }

    /// Veränderlicher Zugriff auf die Kalibrierung, damit der Turn-Loop nach
    /// jeder Antwort `observe` aufrufen kann.
    pub fn token_calibration_mut(&mut self) -> &mut crate::context_budget::TokenCalibration {
        &mut self.token_calibration
    }

    /// Obergrenze für Ausgabe-Tokens je Modellanfrage; `None`: Provider-Default.
    #[must_use]
    pub fn max_output_tokens(&self) -> Option<u64> {
        self.max_output_tokens
    }

    /// Setzt die Ausgabe-Obergrenze je Modellanfrage (Output-Reserve).
    pub fn set_max_output_tokens(&mut self, max_output_tokens: Option<u64>) {
        self.max_output_tokens = max_output_tokens;
    }

    /// Builder-Variante von [`Self::set_max_output_tokens`].
    #[must_use]
    pub fn with_max_output_tokens(mut self, max_output_tokens: Option<u64>) -> Self {
        self.max_output_tokens = max_output_tokens;
        self
    }

    /// `true`, wenn vor dem nächsten Modell-Request kompaktiert werden muss.
    #[must_use]
    pub fn pending_compaction(&self) -> bool {
        self.pending_compaction
    }

    /// Setzt oder löscht die Markierung „vor dem nächsten Request
    /// kompaktieren“. Der Turn-Loop löscht sie nach erfolgter Kompaktierung.
    pub fn set_pending_compaction(&mut self, pending: bool) {
        self.pending_compaction = pending;
    }

    /// Setzt das [`ContextProgram`], das diese Sitzung mitbringt.
    ///
    /// # Beschreibung
    /// Diese Sitzung bringt ihr Programm aus ihrer eigenen Rollendefinition
    /// mit — sie erbt es nicht automatisch von einem Elternteil (§ Moduldoku
    /// "Folgeknoten zu AW2-01/AW2-02"). Aufrufer, die eine Kette von
    /// Kind-Sitzungen mit demselben Programm laufen lassen wollen, müssen
    /// dieses Programm daher an jeder Konstruktionsstelle erneut übergeben;
    /// diese Methode selbst liest kein Elternteil.
    ///
    /// Setzt keine Decke ([`SpawnContext::ceiling`]) durch — eine Sitzung ohne
    /// zusätzliche Prüfung kann hierüber ein Programm setzen, das ihre eigene
    /// Decke überschreitet. Die Deckenprüfung
    /// ([`harw_agent_dsl::context_program::ContextCeilingAdmission::admits_program`])
    /// muss der Aufrufer selbst vor diesem Aufruf durchführen, z. B. über
    /// [`harw_agent_dsl::executable::ContextProgram::from_resolved_program`].
    ///
    /// # Arguments
    /// - `context_program` (`ContextProgram`): das mitgebrachte Programm.
    #[must_use]
    pub fn with_context_program(mut self, context_program: ContextProgram) -> Self {
        self.context_program = Some(context_program);
        self
    }

    /// Liefert das [`ContextProgram`], das diese Sitzung mitbringt.
    ///
    /// # Returns
    /// `None`, solange [`Self::with_context_program`] nie aufgerufen wurde —
    /// dann verwendet die Runtime ihre Standard-Kontextmontage unverändert
    /// (die tragende Auflage dieses Feldes, § Moduldoku).
    #[must_use]
    pub fn context_program(&self) -> Option<&ContextProgram> {
        self.context_program.as_ref()
    }

    /// Setzt das Reasoning-Effort-Level, das jeder Turn dieser Session an den
    /// Provider durchreicht. `None` lässt den Provider seinen Default wählen.
    #[must_use]
    pub fn with_reasoning_effort(mut self, reasoning_effort: Option<ReasoningEffort>) -> Self {
        self.reasoning_effort = reasoning_effort;
        self
    }

    #[must_use]
    pub fn reasoning_effort(&self) -> Option<ReasoningEffort> {
        self.reasoning_effort
    }

    /// Setzt das Reasoning-Effort-Level in-place.
    ///
    /// Nicht-konsumierendes Gegenstück zu [`Self::with_reasoning_effort`] für
    /// bereits im `SessionManager` registrierte Sessions (Wave 8: monotone
    /// Vererbung an Child-Spawns, die erst nach der Session-Erzeugung geklammert
    /// werden). `None` fällt auf den Provider-Default zurück.
    pub fn set_reasoning_effort(&mut self, reasoning_effort: Option<ReasoningEffort>) {
        self.reasoning_effort = reasoning_effort;
    }

    /// Setzt die Richtlinie für automatisches Verdichten des Verlaufs.
    /// `None`: keine automatische Verdichtung.
    #[must_use]
    pub fn with_auto_compact(
        mut self,
        policy: Option<crate::auto_compact::AutoCompactPolicy>,
    ) -> Self {
        self.auto_compact = policy;
        self
    }

    /// Liefert die aktuell gesetzte Auto-Compact-Richtlinie, falls vorhanden.
    #[must_use]
    pub fn auto_compact(&self) -> Option<&crate::auto_compact::AutoCompactPolicy> {
        self.auto_compact.as_ref()
    }

    /// Setzt die Auto-Compact-Richtlinie in-place.
    ///
    /// Nicht-konsumierendes Gegenstück zu [`Self::with_auto_compact`].
    pub fn set_auto_compact(&mut self, policy: Option<crate::auto_compact::AutoCompactPolicy>) {
        self.auto_compact = policy;
    }

    /// Setzt den Beobachter, der nach jeder Verdichtung
    /// ([`crate::compaction::compact_session`]) benachrichtigt wird.
    /// `None`: kein Beobachter registriert.
    #[must_use]
    pub fn with_compaction_observer(
        mut self,
        observer: Option<std::sync::Arc<dyn crate::compaction::CompactionObserver>>,
    ) -> Self {
        self.compaction_observer = observer;
        self
    }

    /// Liefert den aktuell registrierten Verdichtungs-Beobachter, falls vorhanden.
    #[must_use]
    pub fn compaction_observer(
        &self,
    ) -> Option<&std::sync::Arc<dyn crate::compaction::CompactionObserver>> {
        self.compaction_observer.as_ref()
    }

    /// Setzt das fest zugeordnete Provider-/Modellpaar für den Compaction-
    /// Zusammenfassungs-Aufruf dieser Session (Addendum C).
    ///
    /// # Arguments
    /// - `provider` (`Option<ProviderId>`): feste Provider-ID, oder `None`
    ///   für „Session-/Katalog-Default verwenden".
    /// - `model` (`Option<ModelId>`): feste Modell-ID, oder `None` für
    ///   „Session-/Katalog-Default verwenden".
    #[must_use]
    pub fn with_compaction_summary_model(
        mut self,
        provider: Option<ProviderId>,
        model: Option<ModelId>,
    ) -> Self {
        self.compaction_summary_model = (provider, model);
        self
    }

    /// Liefert das aktuell gesetzte Provider-/Modellpaar für den Compaction-
    /// Zusammenfassungs-Aufruf, falls gesetzt.
    #[must_use]
    pub fn compaction_summary_model(&self) -> (Option<&ProviderId>, Option<&ModelId>) {
        (
            self.compaction_summary_model.0.as_ref(),
            self.compaction_summary_model.1.as_ref(),
        )
    }

    /// Setzt den Beobachter, der über das Ergebnis jedes ausgeführten
    /// Tool-Aufrufs dieser Session benachrichtigt wird (Projektgedächtnis-
    /// Erfassung). `None`: kein Beobachter registriert.
    #[must_use]
    pub fn with_tool_outcome_observer(
        mut self,
        observer: Option<std::sync::Arc<dyn crate::capture::ToolOutcomeObserver>>,
    ) -> Self {
        self.tool_outcome_observer = observer;
        self
    }

    /// Liefert den aktuell registrierten Tool-Outcome-Beobachter, falls vorhanden.
    #[must_use]
    pub fn tool_outcome_observer(
        &self,
    ) -> Option<&std::sync::Arc<dyn crate::capture::ToolOutcomeObserver>> {
        self.tool_outcome_observer.as_ref()
    }

    /// Setzt die Schwellenwerte der Turn-Wächter dieser Session (Addendum
    /// F+G). `None` übernimmt [`crate::guard::GuardPolicy::default`].
    #[must_use]
    pub fn with_guard_policy(mut self, policy: Option<crate::guard::GuardPolicy>) -> Self {
        self.guard_policy = policy.unwrap_or_default();
        self
    }

    /// Liefert die aktuell gültigen Wächter-Schwellenwerte dieser Session
    /// (nie `None` — ein nicht gesetzter Wert löst bereits zu
    /// [`crate::guard::GuardPolicy::default`] auf).
    #[must_use]
    pub fn guard_policy(&self) -> crate::guard::GuardPolicy {
        self.guard_policy
    }

    /// Setzt den Beobachter, der über jedes erkannte Drift-Ereignis dieser
    /// Session benachrichtigt wird. `None`: kein Beobachter registriert.
    #[must_use]
    pub fn with_drift_observer(
        mut self,
        observer: Option<std::sync::Arc<dyn crate::guard::DriftObserver>>,
    ) -> Self {
        self.drift_observer = observer;
        self
    }

    /// Liefert den aktuell registrierten Drift-Beobachter, falls vorhanden.
    #[must_use]
    pub fn drift_observer(&self) -> Option<&std::sync::Arc<dyn crate::guard::DriftObserver>> {
        self.drift_observer.as_ref()
    }

    /// Setzt den Berater, der vor jeder Werkzeugausführung dieser Session
    /// nach bekannten Pitfalls befragt wird. `None`: keine Beratung.
    #[must_use]
    pub fn with_pitfall_advisor(
        mut self,
        advisor: Option<std::sync::Arc<dyn crate::guard::PitfallAdvisor>>,
    ) -> Self {
        self.pitfall_advisor = advisor;
        self
    }

    /// Liefert den aktuell registrierten Pitfall-Berater, falls vorhanden.
    #[must_use]
    pub fn pitfall_advisor(&self) -> Option<&std::sync::Arc<dyn crate::guard::PitfallAdvisor>> {
        self.pitfall_advisor.as_ref()
    }

    /// Setzt den Beobachter, der nach jeder Modellrunde und jedem
    /// Tool-Ergebnis dieser Session über Fortschritt benachrichtigt wird.
    /// `None`: kein Beobachter registriert.
    #[must_use]
    pub fn with_progress_observer(
        mut self,
        observer: Option<std::sync::Arc<dyn crate::guard::ProgressObserver>>,
    ) -> Self {
        self.progress_observer = observer;
        self
    }

    /// Liefert den aktuell registrierten Fortschritts-Beobachter, falls
    /// vorhanden.
    #[must_use]
    pub fn progress_observer(&self) -> Option<&std::sync::Arc<dyn crate::guard::ProgressObserver>> {
        self.progress_observer.as_ref()
    }

    /// Builder-style setter: overrides the model selected for every turn of
    /// this session. `None` lets the provider pick its catalog default.
    #[must_use]
    pub fn with_active_model(mut self, model: Option<ModelId>) -> Self {
        self.active_model = model;
        self
    }

    /// Returns the active model override for this session, if any.
    #[must_use]
    pub fn active_model(&self) -> Option<&ModelId> {
        self.active_model.as_ref()
    }

    /// Sets the active model override in-place.
    ///
    /// Non-consuming counterpart to [`Self::with_active_model`] for sessions
    /// already registered in the session manager. `None` falls back to the
    /// provider's catalog default.
    ///
    /// Mit Resolver ([`Self::with_context_window_resolver`]) wird bei einem
    /// echten Wechsel die Auto-Compact-Policy auf das neue Fenster skaliert
    /// und `max_history_bytes` neu abgeleitet — außer ein explizit
    /// konfigurierter Override ([`Self::with_configured_max_history_bytes`])
    /// ist gesetzt, dann bleibt dieser erhalten. Ist das neue Fenster kleiner
    /// als das alte, wird [`Self::pending_compaction`] gesetzt, damit die
    /// History vor dem nächsten Request auf das kleinere Fenster schrumpft.
    pub fn set_active_model(&mut self, model: Option<ModelId>) {
        let changed = self.active_model != model;
        if !changed {
            return;
        }
        let Some(resolve) = self.context_window_resolver.clone() else {
            self.active_model = model;
            return;
        };
        let old_window = resolve(self.active_model.as_ref().map(ModelId::as_str));
        self.active_model = model;
        let window = resolve(self.active_model.as_ref().map(ModelId::as_str));
        if let Some(policy) = self.auto_compact {
            self.auto_compact = Some(policy.rescaled(window));
        }
        self.context_budget.max_history_bytes = match self.configured_max_history_bytes {
            Some(configured) => configured,
            None => {
                let history = usize::try_from(window.saturating_mul(3)).unwrap_or(usize::MAX);
                history.max(ContextBudget::conservative().max_history_bytes)
            }
        };
        if window < old_window {
            self.pending_compaction = true;
        }
    }

    /// Setzt den Resolver für Kontextfenster je Modell (siehe
    /// [`Self::set_active_model`]).
    #[must_use]
    pub fn with_context_window_resolver(
        mut self,
        resolver: std::sync::Arc<crate::child_controller::ContextWindowResolver>,
    ) -> Self {
        self.context_window_resolver = Some(resolver);
        self
    }

    /// Builder-style setter: overrides the provider used for every turn of
    /// this session. `None` lets the session manager use its configured default.
    #[must_use]
    pub fn with_active_provider(mut self, provider: Option<ProviderId>) -> Self {
        self.active_provider = provider;
        self
    }

    /// Returns the active provider override for this session, if any.
    #[must_use]
    pub fn active_provider(&self) -> Option<&ProviderId> {
        self.active_provider.as_ref()
    }

    /// Sets the active provider override in-place.
    ///
    /// Non-consuming counterpart to [`Self::with_active_provider`] for sessions
    /// already registered in the session manager. `None` falls back to the
    /// session manager's configured default provider.
    pub fn set_active_provider(&mut self, provider: Option<ProviderId>) {
        self.active_provider = provider;
    }

    /// Attaches a sink for live per-turn events (tool calls, reasoning, item
    /// updates). Optional — sessions without a sink simply drop these events.
    #[must_use]
    pub fn with_turn_event_sink(mut self, turn_event_tx: mpsc::UnboundedSender<TurnEvent>) -> Self {
        self.turn_event_tx = Some(turn_event_tx);
        self
    }

    /// Hängt den agenten-übergreifenden Live-Bus an.
    #[must_use]
    pub fn with_agent_events(mut self, hub: crate::agent_events::AgentEventHub) -> Self {
        self.agent_events = Some(hub);
        self
    }

    /// Nicht-konsumierende Variante von [`Self::with_agent_events`].
    pub fn set_agent_events(&mut self, hub: Option<crate::agent_events::AgentEventHub>) {
        self.agent_events = hub;
    }

    /// Der angehängte Live-Bus, falls vorhanden.
    #[must_use]
    pub fn agent_events(&self) -> Option<&crate::agent_events::AgentEventHub> {
        self.agent_events.as_ref()
    }

    /// Anzeigename der Rolle dieser Session für Live-Beobachter.
    #[must_use]
    pub fn role_label(&self) -> String {
        match &self.role {
            AgentRole::Agent { name } => name.clone(),
            other => other.to_string(),
        }
    }

    /// Veröffentlicht ein Turn-Event dieser Session auf dem Live-Bus
    /// (No-op ohne Bus oder ohne Beobachter).
    pub fn publish_agent_event(&self, kind: crate::agent_events::AgentEventKind) {
        if let Some(hub) = &self.agent_events
            && hub.observer_count() > 0
        {
            hub.publish(crate::agent_events::AgentEvent {
                agent: self.id.clone(),
                parent: self.parent_session_id.clone(),
                role: self.role_label(),
                kind,
            });
        }
    }

    /// Ein vom `&self`-Borrow gelöster Sender für Live-Turn-Events (Turn-Sink
    /// plus Agenten-Bus), z. B. für einen Streaming-Callback, der während des
    /// Modellaufrufs aus dem Provider heraus feuert.
    #[must_use]
    pub fn live_emitter(&self) -> LiveEmitter {
        LiveEmitter {
            turn_tx: self.turn_event_tx.clone(),
            hub: self.agent_events.clone().map(|hub| {
                (
                    hub,
                    self.id.clone(),
                    self.parent_session_id.clone(),
                    self.role_label(),
                )
            }),
        }
    }

    /// Returns the attached live per-turn event sink, if any was configured
    /// via [`AgentSession::with_turn_event_sink`].
    #[must_use]
    pub fn turn_event_tx(&self) -> Option<&mpsc::UnboundedSender<TurnEvent>> {
        self.turn_event_tx.as_ref()
    }

    /// Liefert die aktuelle Aktivierung dieser Session.
    ///
    /// # Beschreibung
    /// Steuert, welche Werkzeuge, Instructions-Provider und Context-Provider
    /// für das Modell sichtbar sind. Der Wert ist stets der Schnitt aus
    /// [`Self::base_activation`] und der Modus-Decke des aktuellen
    /// [`InteractionMode`] — also genau [`Self::mode_ceiling`]:
    /// `apply_mode` schreibt hierher nichts anderes. Unmittelbar nach jedem
    /// [`Self::set_mode`]-Aufruf gilt daher, dass diese Aktivierung und
    /// [`Self::mode_ceiling`] für jedes Werkzeug dasselbe Ergebnis liefern.
    /// Ein Laufzeit-Toggle über [`Self::activation_mut`] (`/tools on|profile`)
    /// darf diesen Wert danach nur noch gegen die Decke aus
    /// [`Self::mode_ceiling`] validieren, nie über sie hinaus erweitern.
    ///
    /// # Returns
    /// Unveränderliche Referenz auf die aktuelle [`SessionActivation`].
    ///
    /// # Nebenläufigkeit
    /// Nur lesend; sicher aus jedem Thread, solange die Session geliehen ist.
    #[must_use]
    pub fn activation(&self) -> &SessionActivation {
        &self.activation
    }

    /// Liefert die Basis-Aktivierung: die Tool-Freigabe, die diese Session bei
    /// ihrem Aufbau bekommen hat, unabhängig vom aktuellen
    /// [`InteractionMode`].
    ///
    /// # Beschreibung
    /// [`Self::activation`] ist stets der Schnitt dieser Basis mit der Decke
    /// des aktuellen Modus und damit eine Teilmenge von ihr. Wer wissen will,
    /// *warum* ein Werkzeug unsichtbar ist — vom Modus verengt oder von der
    /// Agent-Definition verboten —, vergleicht beide.
    ///
    /// # Returns
    /// Unveränderliche Referenz auf die Basis-[`SessionActivation`]. Für eine
    /// Session, die weder [`Self::with_executable_agent_ir`] noch
    /// [`Self::with_activation`] gesehen hat, ist das
    /// [`SessionActivation::default()`] (Profil `Full`, keine Overrides).
    ///
    /// # Nebenläufigkeit
    /// Nur lesend; sicher aus jedem Thread, solange die Session geliehen ist.
    #[must_use]
    pub fn base_activation(&self) -> &SessionActivation {
        &self.base_activation
    }

    /// Liefert die Modus-Decke: die Obergrenze, gegen die Laufzeit-Toggles wie
    /// `/tools on|profile|reset` validieren müssen.
    ///
    /// # Beschreibung
    /// Der Schnitt aus [`Self::base_activation`] und der Modus-Decke des
    /// aktuellen [`InteractionMode`] (Profil + Positivliste, siehe
    /// [`InteractionMode::tool_profile`] und [`InteractionMode::allowed_tools`])
    /// — dieselbe Modus-Seite, die `apply_mode` auch in [`Self::activation`]
    /// einsetzt (beide nutzen dieselbe private `mode_activation`-Hilfsfunktion).
    /// Unmittelbar nach [`Self::set_mode`] liefern [`Self::activation`] und
    /// dieser Wert deshalb für jedes Werkzeug dasselbe Ergebnis. Ein
    /// Laufzeit-Toggle wie `/tools on <name>` oder `/tools profile <p>` darf
    /// die Aktivierung danach nur noch innerhalb dieser Decke bewegen, nie
    /// über sie hinaus — dieser Wert ist der Referenzpunkt, gegen den ein
    /// solcher Toggle schneiden muss, nicht die (potenziell weitere) Basis
    /// allein.
    ///
    /// # Returns
    /// Eine frisch berechnete [`SessionActivation`] — die Modus-Obergrenze,
    /// kein gespeicherter Zustand.
    ///
    /// # Nebenläufigkeit
    /// Nur lesend, alloziert bei jedem Aufruf eine neue [`SessionActivation`];
    /// sicher aus jedem Thread, solange die Session geliehen ist.
    #[must_use]
    pub fn mode_ceiling(&self) -> SessionActivation {
        self.base_activation.intersect(&mode_activation(self.mode))
    }

    /// Verengt die Basis dauerhaft auf den Schnitt mit `ceiling`.
    ///
    /// # Beschreibung
    /// Für Autoritätsgrenzen, die *jeden* späteren Modus-Wechsel überleben
    /// müssen — etwa den Schnitt einer Kind-Sitzung mit der Aktivierung ihres
    /// Elternteils (`ManagedAgentSpawner::admit`, Befund F-017/E1). Ein
    /// Schreibzugriff über [`Self::activation_mut`] taugt dafür **nicht**: der
    /// aktuelle Wert wird bei jedem [`Self::set_mode`] aus der Basis neu
    /// abgeleitet und ein dort gesetztes Verbot damit stillschweigend
    /// verworfen.
    ///
    /// Monoton: [`SessionActivation::intersect`] lässt nie mehr zu als jede
    /// Seite allein, die Basis wird also nie weiter, nur enger. Mehrfaches
    /// Anwenden derselben Decke ist idempotent. Anschließend wird der aktuelle
    /// Modus über die neue Basis neu angewandt, damit
    /// [`Self::activation`] sofort zur verengten Basis passt.
    ///
    /// # Arguments
    /// - `ceiling` (`&SessionActivation`): die Decke, auf die verengt wird.
    ///
    /// # Panics
    /// Keine.
    ///
    /// # Nebenläufigkeit
    /// Verlangt exklusiven Zugriff (`&mut self`), hält keine Sperre und sendet
    /// kein Event — der Aufrufer entscheidet, ob ein Wechsel sichtbar wird.
    pub fn narrow_base_activation(&mut self, ceiling: &SessionActivation) {
        self.base_activation = self.base_activation.intersect(ceiling);
        self.apply_mode();
    }

    /// Returns a mutable reference to the session's activation filter.
    ///
    /// # Description
    /// Use this to toggle individual tools, instructions, or context providers
    /// at runtime without rebuilding the session.
    ///
    /// # Returns
    /// Exclusive mutable reference to [`SessionActivation`].
    pub fn activation_mut(&mut self) -> &mut SessionActivation {
        &mut self.activation
    }

    /// Builder-style setter for the activation filter (consumes and returns
    /// `self` for chaining with other `with_*` methods).
    ///
    /// Die übergebene Aktivierung wird zugleich zur **Basis** dieser Session
    /// ([`Self::base_activation`]): ein späterer [`Self::set_mode`]-Aufruf
    /// schneidet gegen sie, statt sie zu verlieren. Der anschließende
    /// `apply_mode`-Aufruf hält die Reihenfolge der Builder-Aufrufe egal —
    /// `with_mode(m).with_activation(a)` und `with_activation(a).with_mode(m)`
    /// liefern dieselbe Sitzung; im Default-Modus
    /// [`InteractionMode::Chat`] ist der Schnitt die Identität.
    ///
    /// # Arguments
    /// - `activation` (`SessionActivation`): the new filter to install.
    ///
    /// # Returns
    /// `Self` with the activation replaced.
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_core::activation::{SessionActivation, ToolProfile};
    /// // session.with_activation(SessionActivation::new(ToolProfile::Minimal));
    /// ```
    #[must_use]
    pub fn with_activation(mut self, activation: SessionActivation) -> Self {
        self.base_activation = activation;
        self.apply_mode();
        self
    }

    #[must_use]
    pub fn id(&self) -> &SessionId {
        &self.id
    }
    #[must_use]
    pub fn role(&self) -> &AgentRole {
        &self.role
    }
    #[must_use]
    pub fn parent_session_id(&self) -> Option<&SessionId> {
        self.parent_session_id.as_ref()
    }
    #[must_use]
    pub fn state(&self) -> &SessionState {
        &self.state
    }
    #[must_use]
    pub fn history(&self) -> &ConversationHistory {
        &self.history
    }
    pub fn history_mut(&mut self) -> &mut ConversationHistory {
        &mut self.history
    }
    #[must_use]
    pub fn registry(&self) -> &ExtensionRegistry {
        &self.registry
    }
    /// Die ID des aktuell laufenden bzw. pausierten Turns.
    #[must_use]
    pub fn current_turn(&self) -> Option<&TurnId> {
        self.current_turn.as_ref()
    }

    /// Hinterlegt den [`TurnControl`]-Steuerblock des gerade gestarteten
    /// Turns, damit er über eine Handoff- oder Rückfrage-Pause hinweg
    /// erhalten bleibt.
    ///
    /// # Description
    /// Aufgerufen von `run_turn_with_approvals` (`turn_loop.rs`) direkt nach
    /// `try_start_turn`, mit einem Klon des `TurnInput::control`-Blocks des
    /// neuen Turns. Ein späterer `resume_after_child`/`resume_after_approval`
    /// liest denselben Block über [`Self::active_turn_control`] zurück, statt
    /// sich einen frischen zu bauen — siehe Feld-Doku.
    ///
    /// # Arguments
    /// - `control` (`TurnControl`): der Steuerblock des soeben gestarteten
    ///   Turns (Ownership).
    pub(crate) fn set_active_turn_control(&mut self, control: TurnControl) {
        self.active_turn_control = Some(control);
    }

    /// Der [`TurnControl`]-Steuerblock des aktuell laufenden/pausierten
    /// Turns, sofern einer hinterlegt ist.
    ///
    /// # Returns
    /// `Some(&TurnControl)`, wenn [`Self::set_active_turn_control`] seit dem
    /// letzten [`Self::complete_turn`] aufgerufen wurde; sonst `None` — z. B.
    /// wenn eine Session ihren Turn nie über `run_turn`/`run_turn_durable`
    /// gestartet hat (Tests, die direkt `try_start_turn` rufen).
    #[must_use]
    pub(crate) fn active_turn_control(&self) -> Option<&TurnControl> {
        self.active_turn_control.as_ref()
    }

    /// Versucht einen Turn zu starten. Nur aus `Idle` möglich.
    pub fn try_start_turn(&mut self) -> Result<TurnHandle, TurnRejection> {
        match &self.state {
            SessionState::Idle => {
                let turn_id = TurnId::new();
                self.state = SessionState::Running;
                self.current_turn = Some(turn_id.clone());
                let _ = self.event_tx.send(SessionEvent::TurnStarted {
                    session_id: self.id.clone(),
                    turn_id: turn_id.clone(),
                });
                Ok(TurnHandle {
                    turn_id,
                    session_id: self.id.clone(),
                })
            }
            other => Err(TurnRejection::NotIdle(other.clone())),
        }
    }

    /// Aufsummierte Token-Nutzung aller bisherigen Turns dieser Session.
    ///
    /// # Beschreibung
    /// Quelle für die Durchsetzung einer Token-Obergrenze (`AgentBudget::max_tokens`)
    /// bei Kind-Agenten. Der Wert wächst monoton und wird nie zurückgesetzt —
    /// eine Session ist die Abrechnungseinheit, nicht der einzelne Turn.
    ///
    /// # Concurrency
    /// Lesend, lock-frei; erfordert eine geteilte Referenz auf die Session.
    #[must_use]
    pub fn total_usage(&self) -> &TokenUsage {
        &self.total_usage
    }

    /// Schließt den **aktiven** Turn ab und kehrt nach `Idle` zurück.
    ///
    /// # Description
    /// Prüft vor jeder Mutation (F-151):
    /// 1. `handle.session_id` ist diese Session,
    /// 2. der Zustand ist `Running`, `WaitingForApproval` oder
    ///    `WaitingForChild` (nie `Idle`, nie `Failed` — aus `Failed` führt nur
    ///    [`Self::recover`] heraus),
    /// 3. `handle.turn_id` ist [`Self::current_turn`].
    ///
    /// Erst dann wird `usage` auf [`Self::total_usage`] addiert, Pending-Zustand
    /// geleert und `SessionEvent::TurnCompleted` gesendet.
    ///
    /// # Arguments
    /// - `handle` (`TurnHandle`): Handle aus [`Self::try_start_turn`] (Ownership).
    /// - `usage` (`TokenUsage`): Nutzung dieses Turns.
    ///
    /// # Errors
    /// - [`CoreError::TurnRejected`]: fremde Session, unzulässiger Zustand oder
    ///   nicht der aktive Turn. Die Session bleibt dann unverändert.
    ///
    /// # Concurrency
    /// Verlangt `&mut self`; sendet best-effort über den Event-Kanal.
    pub fn complete_turn(&mut self, handle: TurnHandle, usage: TokenUsage) -> CoreResult<()> {
        if handle.session_id != self.id {
            return Err(CoreError::TurnRejected(format!(
                "turn {} belongs to session {}, not to session {}",
                handle.turn_id, handle.session_id, self.id
            )));
        }
        match &self.state {
            SessionState::Running
            | SessionState::WaitingForApproval
            | SessionState::WaitingForChild => {}
            other => {
                return Err(CoreError::TurnRejected(format!(
                    "session {} cannot complete turn {} from state {other}",
                    self.id, handle.turn_id
                )));
            }
        }
        if self.current_turn.as_ref() != Some(&handle.turn_id) {
            return Err(CoreError::TurnRejected(format!(
                "turn {} is not the active turn of session {} (active: {})",
                handle.turn_id,
                self.id,
                self.current_turn
                    .as_ref()
                    .map_or_else(|| "none".to_owned(), ToString::to_string)
            )));
        }
        self.total_usage.add(&usage);
        self.state = SessionState::Idle;
        self.current_turn = None;
        self.active_turn_control = None;
        self.pending_approval = None;
        self.pending_handoff = None;
        let _ = self.event_tx.send(SessionEvent::TurnCompleted {
            session_id: handle.session_id,
            turn_id: handle.turn_id,
            usage,
        });
        Ok(())
    }

    /// In `WaitingForChild` wechseln (Handoff).
    pub fn begin_handoff(
        &mut self,
        child: SessionId,
        call_id: harw_types::ToolCallId,
        role: String,
    ) -> CoreResult<()> {
        if self.state != SessionState::Running {
            return Err(CoreError::NotIdle {
                session_id: self.id.to_string(),
                state: self.state.to_string(),
            });
        }
        self.pending_handoff = Some(PendingHandoff {
            child,
            call_id,
            role,
        });
        self.handoff_started_at = Some(std::time::Instant::now());
        self.state = SessionState::WaitingForChild;
        Ok(())
    }

    /// Setzt die Vorgabe-Grenzen, die jeder Turn ohne eigene
    /// [`crate::turn_loop::TurnLimits`] bekommt.
    #[must_use]
    pub fn with_default_turn_limits(mut self, limits: crate::turn_loop::TurnLimits) -> Self {
        self.default_turn_limits = Some(limits);
        self
    }

    /// Die Vorgabe-Grenzen dieser Session, falls gesetzt.
    #[must_use]
    pub fn default_turn_limits(&self) -> Option<crate::turn_loop::TurnLimits> {
        self.default_turn_limits
    }

    /// Meldet, dass die Session konfiguriert ist (Modell steht fest).
    pub fn announce_configured(&self) {
        let _ = self.event_tx.send(SessionEvent::SessionConfigured {
            session_id: self.id.clone(),
            thread_id: ThreadId::new(),
            model: self
                .active_model
                .as_ref()
                .map_or_else(|| "default".to_owned(), |m| m.as_str().to_owned()),
        });
    }

    /// Meldet das saubere Ende der Session.
    pub fn announce_closed(&self, reason: Option<String>) {
        let _ = self.event_tx.send(SessionEvent::SessionClosed {
            session_id: self.id.clone(),
            reason,
        });
    }

    /// Meldet einen Fehler auf Session-Ebene (z. B. einen gescheiterten Turn).
    pub fn report_error(&self, message: String, retryable: bool) {
        let _ = self.event_tx.send(SessionEvent::SessionError {
            session_id: self.id.clone(),
            message,
            retryable,
        });
    }

    /// Entnimmt die bisherige Laufzeit des zuletzt begonnenen Handoffs in
    /// Millisekunden (0, wenn kein Startzeitpunkt bekannt ist).
    pub fn take_handoff_elapsed_ms(&mut self) -> u64 {
        self.handoff_started_at.take().map_or(0, |start| {
            u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
        })
    }

    /// Pausiert die Session für eine Freigabeentscheidung.
    ///
    /// # Description
    /// Legt [`PendingApproval`] ab und trägt `now` als `requested_at` ein;
    /// `timeout_at` ist `now + `[`DEFAULT_APPROVAL_TIMEOUT`]. Die Wartefrist
    /// wird hier festgelegt — einmalig, unabhängig davon, welche Oberfläche
    /// später darauf wartet (Interaktionsvertrag §4.4).
    ///
    /// # Arguments
    /// - `call` (`ToolCall`): der exakte, festzuhaltende Werkzeugaufruf.
    /// - `request` (`ItemId`): Korrelations-ID der Freigabeanfrage.
    /// - `actor` (`ApprovalActor`): wer diese Anfrage beantworten darf.
    /// - `now` (`jiff::Timestamp`): Serveruhr des Aufrufers, zur Testbarkeit
    ///   injiziert statt intern `jiff::Timestamp::now()` zu rufen.
    ///
    /// # Errors
    /// [`CoreError::NotIdle`], wenn die Session nicht [`SessionState::Running`]
    /// ist.
    pub fn begin_approval(
        &mut self,
        call: ToolCall,
        request: ItemId,
        actor: ApprovalActor,
        now: jiff::Timestamp,
    ) -> CoreResult<()> {
        if self.state != SessionState::Running {
            return Err(CoreError::NotIdle {
                session_id: self.id.to_string(),
                state: self.state.to_string(),
            });
        }
        let timeout_at = now.checked_add(DEFAULT_APPROVAL_TIMEOUT).unwrap_or(now);
        self.pending_approval = Some(PendingApproval {
            call,
            request,
            actor,
            requested_at: now,
            timeout_at,
        });
        self.state = SessionState::WaitingForApproval;
        Ok(())
    }

    pub fn resolve_approval(&mut self, actor: &ApprovalActor) -> CoreResult<PendingApproval> {
        match self.state {
            SessionState::WaitingForApproval => {
                let pending = self.pending_approval.take().ok_or_else(|| {
                    CoreError::TurnRejected("approval state has no pending call".to_owned())
                })?;
                if &pending.actor != actor {
                    self.pending_approval = Some(pending);
                    return Err(CoreError::ApprovalActorMismatch {
                        session_id: self.id.to_string(),
                    });
                }
                self.state = SessionState::Running;
                Ok(pending)
            }
            _ => Err(CoreError::NotIdle {
                session_id: self.id.to_string(),
                state: self.state.to_string(),
            }),
        }
    }

    /// Borrows the immutable approval correlation while the session remains
    /// paused. A durable store consumes a callback before the core transitions
    /// the session back to `Running`.
    #[must_use]
    pub fn pending_approval(&self) -> Option<&PendingApproval> {
        self.pending_approval.as_ref()
    }

    /// Child hat geliefert — zurück zu `Running`.
    pub fn child_completed(
        &mut self,
        child: &SessionId,
        call_id: &harw_types::ToolCallId,
    ) -> CoreResult<()> {
        match &self.state {
            SessionState::WaitingForChild => {
                let pending = self.pending_handoff.as_ref().ok_or_else(|| {
                    CoreError::TurnRejected("handoff state has no pending child".to_owned())
                })?;
                if &pending.child != child || &pending.call_id != call_id {
                    return Err(CoreError::TurnRejected(
                        "child result does not match the pending handoff".to_owned(),
                    ));
                }
                self.state = SessionState::Running;
                self.pending_handoff = None;
                Ok(())
            }
            other => Err(CoreError::NotIdle {
                session_id: self.id.to_string(),
                state: other.to_string(),
            }),
        }
    }

    /// Holt eine Session aus `Failed` zurück in den bedienbaren Zustand `Idle`.
    ///
    /// # Description
    /// Einziger Übergang aus `Failed` (F-152). Leert `current_turn`,
    /// `pending_approval` und `pending_handoff` und repariert offene Tool-Calls
    /// des Live-Verlaufs über
    /// [`crate::state_store::repair_open_tool_calls`] (synthetisches
    /// Fehler-Ergebnis, `ResultTrust::Runtime`), damit der nächste Turn einen
    /// providergültigen Verlauf sendet. Die synthetischen Ergebnisse werden
    /// hier nicht persistiert; ein späteres Hydrieren erzeugt sie
    /// deterministisch erneut. Modus, Aktivierung, Sandbox und
    /// [`Self::total_usage`] bleiben unverändert — ein Recovery erweitert nie
    /// Autorität.
    ///
    /// # Returns
    /// Die `call_id`s der reparierten Tool-Calls.
    ///
    /// # Errors
    /// - [`CoreError::TurnRejected`]: die Session ist nicht in `Failed`
    ///   (unverändert).
    ///
    /// # Concurrency
    /// Verlangt `&mut self`, hält keine Sperre, sendet kein Event.
    pub fn recover(&mut self) -> CoreResult<Vec<ToolCallId>> {
        if !matches!(self.state, SessionState::Failed(_)) {
            return Err(CoreError::TurnRejected(format!(
                "session {} cannot recover from state {}; only Failed is recoverable",
                self.id, self.state
            )));
        }
        self.state = SessionState::Idle;
        self.current_turn = None;
        self.pending_approval = None;
        self.pending_handoff = None;
        let repaired = repair_open_tool_calls(&mut self.history);
        tracing::info!(
            session_id = %self.id,
            repaired_tool_calls = repaired.len(),
            "session.recovered"
        );
        Ok(repaired)
    }

    /// Ersetzt den Verlauf durch einen geladenen und repariert offene Calls.
    ///
    /// # Arguments
    /// - `history` (`ConversationHistory`): geladener Verlauf (Ownership).
    ///
    /// # Returns
    /// Die `call_id`s der reparierten Tool-Calls.
    ///
    /// # Concurrency
    /// Verlangt `&mut self`.
    pub fn hydrate_history(&mut self, mut history: ConversationHistory) -> Vec<ToolCallId> {
        let repaired = repair_open_tool_calls(&mut history);
        self.history = history;
        repaired
    }

    /// Erzeugt den persistierbaren Sitzungszustand (F-157).
    ///
    /// # Description
    /// Modus, [`Self::total_usage`], IR-Snapshot-ID sowie Basis- und wirksame
    /// Aktivierung. Aktivierungen werden über die Namen aller im Registry
    /// registrierten Werkzeuge abgetastet
    /// ([`crate::state_store::ActivationSnapshot::capture`]).
    ///
    /// # Returns
    /// Einen [`SessionStateSnapshot`] mit Version [`SESSION_STATE_VERSION`].
    ///
    /// # Concurrency
    /// Nur lesend; ruft `ToolProvider::tools()` jedes Providers einmal auf.
    #[must_use]
    pub fn state_snapshot(&self) -> SessionStateSnapshot {
        let tool_names: BTreeSet<String> = self
            .registry
            .tool_providers()
            .iter()
            .flat_map(|provider| provider.tools())
            .map(|spec| spec.name().to_owned())
            .collect();
        SessionStateSnapshot {
            version: SESSION_STATE_VERSION,
            mode: self.mode,
            total_usage: self.total_usage.clone(),
            executable_snapshot_id: self
                .executable_snapshot_id
                .as_ref()
                .map(ToString::to_string),
            base_activation: ActivationSnapshot::capture(
                &self.base_activation,
                tool_names.iter().map(String::as_str),
            ),
            activation: ActivationSnapshot::capture(
                &self.activation,
                tool_names.iter().map(String::as_str),
            ),
        }
    }

    /// Wendet einen persistierten Sitzungszustand an, ohne Autorität zu erweitern.
    ///
    /// # Description
    /// Reihenfolge:
    /// 1. Unbekannte `version` → nichts wird angewandt (`false`).
    /// 2. Stimmt `executable_snapshot_id` mit der aktuellen IR überein, wird die
    ///    aktuelle Basis mit der geladenen Basis **geschnitten** (bewahrt z. B.
    ///    eine Verengung aus `narrow_base_activation`); bei abweichender IR
    ///    gilt allein die aktuelle Basis.
    /// 3. Modus setzen und anwenden (Tool-Decke und Sandbox aus der Basis, ohne
    ///    `ModeChanged`-Event).
    /// 4. Wirksame Aktivierung = geladene Aktivierung ∩ [`Self::mode_ceiling`]
    ///    — nie mehr als die aktuelle Basis.
    /// 5. Geladene Nutzung wird auf [`Self::total_usage`] **addiert** (für eine
    ///    frische Session identisch mit Ersetzen; nie Unterzählung).
    ///
    /// # Arguments
    /// - `snapshot` (`&SessionStateSnapshot`): der geladene Zustand.
    ///
    /// # Returns
    /// `true`, wenn der Zustand angewandt wurde.
    ///
    /// # Concurrency
    /// Verlangt `&mut self`; sendet kein Event.
    pub fn restore_state(&mut self, snapshot: &SessionStateSnapshot) -> bool {
        if snapshot.version != SESSION_STATE_VERSION {
            tracing::warn!(
                session_id = %self.id,
                version = snapshot.version,
                supported = SESSION_STATE_VERSION,
                "session.state_restore_skipped_unknown_version"
            );
            return false;
        }
        let current_snapshot_id = self
            .executable_snapshot_id
            .as_ref()
            .map(ToString::to_string);
        if snapshot.executable_snapshot_id == current_snapshot_id {
            self.base_activation = self
                .base_activation
                .intersect(&snapshot.base_activation.to_activation());
        } else {
            tracing::warn!(
                session_id = %self.id,
                "session.state_restore_base_ignored_executable_changed"
            );
        }
        self.mode = snapshot.mode;
        self.apply_mode();
        self.activation = snapshot
            .activation
            .to_activation()
            .intersect(&self.mode_ceiling());
        self.total_usage.add(&snapshot.total_usage);
        true
    }

    /// Persistiert den Sitzungszustand über `store` (F-157).
    ///
    /// # Arguments
    /// - `store` (`&dyn StateStore`): Ziel-Store.
    ///
    /// # Errors
    /// Durchgereichter [`crate::state_store::StateStoreError`].
    ///
    /// # Concurrency
    /// `async`; der Snapshot wird vor dem ersten `.await` gebaut.
    pub async fn persist_state(&self, store: &dyn StateStore) -> StateStoreResult<()> {
        let snapshot = self.state_snapshot();
        store.save_session_state(&self.id, &snapshot).await
    }

    /// Hydriert eine frische Session aus `store`: Verlauf und Sitzungszustand.
    ///
    /// # Description
    /// Hat die Session bereits einen Verlauf, geschieht nichts
    /// (`skipped = true`). Sonst: Verlauf laden, offene Tool-Calls reparieren
    /// ([`Self::hydrate_history`]), dann den jüngsten Sitzungszustand über
    /// [`Self::restore_state`] anwenden. Der Session-Zustand (`Idle`/`Running`)
    /// wird nicht berührt.
    ///
    /// # Arguments
    /// - `store` (`&dyn StateStore`): Quelle.
    ///
    /// # Returns
    /// Einen [`SessionHydration`]-Bericht.
    ///
    /// # Errors
    /// Durchgereichter [`crate::state_store::StateStoreError`]; die Session
    /// bleibt dann unverändert, sofern der Verlauf nicht bereits geladen war
    /// (Zustand wird erst nach erfolgreichem Laden beider Teile angewandt).
    ///
    /// # Concurrency
    /// `async`, verlangt `&mut self` über die `.await`-Punkte.
    pub async fn hydrate_from_store(
        &mut self,
        store: &dyn StateStore,
    ) -> StateStoreResult<SessionHydration> {
        if !self.history.is_empty() {
            return Ok(SessionHydration {
                skipped: true,
                ..SessionHydration::default()
            });
        }
        let history = store.load_history(&self.id).await?;
        let state = store.load_session_state(&self.id).await?;
        let repaired_tool_calls = self.hydrate_history(history);
        let state_restored = state
            .as_ref()
            .is_some_and(|snapshot| self.restore_state(snapshot));
        tracing::info!(
            session_id = %self.id,
            history_items = self.history.len(),
            repaired_tool_calls = repaired_tool_calls.len(),
            state_restored,
            "session.hydrated"
        );
        Ok(SessionHydration {
            skipped: false,
            history_items: self.history.len(),
            repaired_tool_calls,
            state_restored,
        })
    }

    /// Terminaler Fehler.
    pub fn fail(&mut self, reason: String) {
        self.state = SessionState::Failed(reason.clone());
        self.pending_approval = None;
        self.pending_handoff = None;
        let _ = self.event_tx.send(SessionEvent::SessionFailed {
            session_id: self.id.clone(),
            reason,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_agent_dsl::authority::AuthorityCeiling;
    use harw_agent_dsl::lower;
    use harw_agent_dsl::parse::parse_toml;
    use harw_agent_dsl::resolved::{ResolutionTrace, ResolvedAgentDefinition};
    use harw_authority::{Permission, PermissionSet, WorkspaceRegistration, WorkspaceRegistry};
    use harw_context::{ContextBudgetSpec, SectionName, TrustClass};
    use harw_extension_api::ExtensionRegistryBuilder;
    use harw_types::{TenantId, WorkspaceId};
    use std::path::PathBuf;

    fn test_session() -> AgentSession {
        let (event_tx, _receiver) = mpsc::unbounded_channel();
        AgentSession::new(
            AgentRole::Assistant,
            None,
            ExtensionRegistryBuilder::default().build(),
            event_tx,
        )
    }

    #[test]
    fn context_program_defaults_to_none() {
        let session = test_session();
        assert!(
            session.context_program().is_none(),
            "a session without with_context_program must render exactly as before this node"
        );
    }

    #[test]
    fn with_context_program_sets_and_returns_it() {
        // `ContextProgram` has no public constructor besides `Default` and
        // `from_resolved_program` (which needs a full `ResolvedContextProgramDefinition`
        // plus a `ContextCeiling` — exercised in `harw-agent-dsl`'s own tests).
        // This session-level test only proves the round-trip through
        // `with_context_program`/`context_program`, so `Default` suffices; a
        // non-default program would exercise the same builder plumbing.
        let program = ContextProgram::default();
        let session = test_session().with_context_program(program.clone());

        assert_eq!(
            session.context_program(),
            Some(&program),
            "the session must return exactly the program it was given"
        );
    }

    #[test]
    fn context_program_does_not_affect_context_budget_or_activation() {
        // A declared context_program must not leak into unrelated session
        // state — the field is additive, not a hidden side channel.
        let baseline = test_session();
        let with_program = test_session().with_context_program(ContextProgram::default());

        assert_eq!(with_program.context_budget(), baseline.context_budget());
        assert_eq!(with_program.mode(), baseline.mode());
    }

    fn test_approval_call() -> ToolCall {
        ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("fs.write"),
            arguments: serde_json::json!({}),
        }
    }

    fn test_approval_actor() -> ApprovalActor {
        ApprovalActor::Operator {
            id: "session-test-operator".to_owned(),
        }
    }

    #[test]
    fn begin_approval_stores_requested_at_and_computes_default_timeout_at() -> TestResult {
        let mut session = test_session();
        session
            .try_start_turn()
            .map_err(ctx("an idle session can start a turn"))?;
        let now = jiff::Timestamp::constant(1_700_000_000, 0);

        session
            .begin_approval(
                test_approval_call(),
                ItemId::new(),
                test_approval_actor(),
                now,
            )
            .map_err(ctx("a running session accepts an approval pause"))?;

        let pending = session.pending_approval().ok_or(TestError::Missing(
            "begin_approval muss ein Approval speichern",
        ))?;
        assert_eq!(pending.requested_at, now);
        assert_eq!(
            pending.timeout_at,
            now.checked_add(DEFAULT_APPROVAL_TIMEOUT)
                .map_err(ctx("default timeout stays in range"))?
        );
        Ok(())
    }

    #[test]
    fn pending_approval_is_timed_out_is_inclusive_at_the_deadline() -> TestResult {
        let mut session = test_session();
        session
            .try_start_turn()
            .map_err(ctx("an idle session can start a turn"))?;
        let now = jiff::Timestamp::constant(1_700_000_000, 0);
        session
            .begin_approval(
                test_approval_call(),
                ItemId::new(),
                test_approval_actor(),
                now,
            )
            .map_err(ctx("a running session accepts an approval pause"))?;
        let pending = session.pending_approval().ok_or(TestError::Missing(
            "begin_approval muss ein Approval speichern",
        ))?;

        assert!(!pending.is_timed_out(now), "freshly opened, not timed out");
        assert!(
            !pending.is_timed_out(
                pending
                    .timeout_at
                    .checked_sub(jiff::SignedDuration::from_secs(1))
                    .map_err(ctx("one second before the deadline stays in range"))?
            ),
            "one second before the deadline must not be timed out"
        );
        assert!(
            pending.is_timed_out(pending.timeout_at),
            "exactly at timeout_at counts as timed out (inclusive bound)"
        );
        assert!(
            pending.is_timed_out(
                pending
                    .timeout_at
                    .checked_add(jiff::SignedDuration::from_secs(1))
                    .map_err(ctx("one second after the deadline stays in range"))?
            ),
            "past the deadline must stay timed out"
        );
        Ok(())
    }

    #[test]
    fn begin_approval_rejects_a_session_that_is_not_running() {
        let mut session = test_session();
        let result = session.begin_approval(
            test_approval_call(),
            ItemId::new(),
            test_approval_actor(),
            jiff::Timestamp::now(),
        );
        assert!(matches!(result, Err(CoreError::NotIdle { .. })));
        assert!(session.pending_approval().is_none());
    }

    fn executable_agent_ir(admitted: &[&str], forbidden: &[&str]) -> TestResult<ExecutableAgentIr> {
        let admitted = admitted
            .iter()
            .map(|name| format!("\"{name}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let forbidden = forbidden
            .iter()
            .map(|name| format!("\"{name}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let raw = parse_toml(&format!(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.session-policy-test@1"
version = "1.0.0"
role = "worker"
specialization = "session-policy-test"

[tools]
admitted = [{admitted}]
forbidden = [{forbidden}]
"#
        ))
        .map_err(ctx("test agent definition must parse"))?;
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

        lower(&resolved).map_err(ctx("test agent definition must lower"))
    }

    #[test]
    fn test_new_with_id_preserves_supplied_session_id() {
        let (event_tx, _receiver) = mpsc::unbounded_channel();
        let session_id = SessionId::new();
        let session = AgentSession::new_with_id(
            session_id.clone(),
            AgentRole::Assistant,
            None,
            ExtensionRegistryBuilder::default().build(),
            event_tx,
        );

        assert_eq!(session.id(), &session_id);
    }

    #[test]
    fn executable_policy_with_empty_admitted_list_allows_no_tools() -> TestResult {
        let executable = executable_agent_ir(&[], &[])?;
        let session = test_session().with_executable_agent_ir(&executable);

        assert_eq!(session.activation().profile(), ToolProfile::Minimal);
        assert!(
            !session
                .activation()
                .is_tool_enabled(&ToolName::new("fs.read"))
        );
        assert!(
            !session
                .activation()
                .is_tool_enabled(&ToolName::new("custom.tool"))
        );
        Ok(())
    }

    #[test]
    fn executable_policy_makes_admitted_tools_visible() -> TestResult {
        let executable = executable_agent_ir(&["fs.read", "custom.tool"], &[])?;
        let session = test_session().with_executable_agent_ir(&executable);

        assert!(
            session
                .activation()
                .is_tool_enabled(&ToolName::new("fs.read"))
        );
        assert!(
            session
                .activation()
                .is_tool_enabled(&ToolName::new("custom.tool"))
        );
        assert!(
            !session
                .activation()
                .is_tool_enabled(&ToolName::new("shell.exec"))
        );
        Ok(())
    }

    #[test]
    fn executable_policy_forbidden_tool_wins_over_admission() -> TestResult {
        let executable = executable_agent_ir(&["shell.exec"], &["shell.exec"])?;
        let session = test_session().with_executable_agent_ir(&executable);

        assert!(
            !session
                .activation()
                .is_tool_enabled(&ToolName::new("shell.exec"))
        );
        Ok(())
    }

    #[test]
    fn executable_policy_retains_snapshot_id() -> TestResult {
        let executable = executable_agent_ir(&[], &[])?;
        let snapshot_id = executable.snapshot_id();
        let session = test_session().with_executable_agent_ir(&executable);

        assert_eq!(session.executable_snapshot_id(), Some(&snapshot_id));
        Ok(())
    }

    #[test]
    fn normal_session_defaults_to_full_tool_profile() {
        assert_eq!(test_session().activation().profile(), ToolProfile::Full);
    }

    #[test]
    fn test_set_reasoning_effort_round_trips_some_value() {
        let mut session = test_session();
        assert_eq!(session.reasoning_effort(), None);

        session.set_reasoning_effort(Some(ReasoningEffort::High));

        assert_eq!(session.reasoning_effort(), Some(ReasoningEffort::High));
    }

    #[test]
    fn test_set_reasoning_effort_none_resets_to_provider_default() {
        let mut session = test_session().with_reasoning_effort(Some(ReasoningEffort::Medium));
        assert_eq!(session.reasoning_effort(), Some(ReasoningEffort::Medium));

        session.set_reasoning_effort(None);

        assert_eq!(session.reasoning_effort(), None);
    }

    #[test]
    fn test_set_reasoning_effort_overwrites_previous_value() {
        let mut session = test_session().with_reasoning_effort(Some(ReasoningEffort::Low));

        session.set_reasoning_effort(Some(ReasoningEffort::Minimal));

        assert_eq!(session.reasoning_effort(), Some(ReasoningEffort::Minimal));
    }

    #[test]
    fn test_agent_session_active_model_default_is_none() {
        let session = test_session();
        assert_eq!(session.active_model(), None);
    }

    #[test]
    fn test_agent_session_set_active_model_persists() {
        let mut session = test_session();
        let model = ModelId::from("claude-opus-4");

        session.set_active_model(Some(model.clone()));

        assert_eq!(session.active_model(), Some(&model));

        // Clearing resets to None.
        session.set_active_model(None);
        assert_eq!(session.active_model(), None);
    }

    fn resolver_session() -> AgentSession {
        let resolver: std::sync::Arc<crate::child_controller::ContextWindowResolver> =
            std::sync::Arc::new(|model: Option<&str>| match model {
                Some("small") => 32_000,
                Some("big") => 1_000_000,
                _ => 200_000,
            });
        test_session().with_context_window_resolver(resolver)
    }

    #[test]
    fn test_set_active_model_smaller_window_sets_pending_compaction() {
        let mut session = resolver_session();
        assert!(!session.pending_compaction());

        session.set_active_model(Some(ModelId::from("big")));
        assert!(!session.pending_compaction(), "größeres Fenster: kein Flag");
        assert_eq!(session.context_budget().max_history_bytes, 3_000_000);

        session.set_active_model(Some(ModelId::from("small")));
        assert!(session.pending_compaction(), "kleineres Fenster: Flag");
        assert_eq!(
            session.context_budget().max_history_bytes,
            ContextBudget::conservative().max_history_bytes
        );

        session.set_pending_compaction(false);
        session.set_active_model(Some(ModelId::from("small")));
        assert!(!session.pending_compaction(), "kein Wechsel: kein Flag");
    }

    #[test]
    fn test_set_active_model_keeps_configured_max_history_bytes() {
        let mut session = resolver_session().with_configured_max_history_bytes(Some(123_456));
        assert_eq!(session.configured_max_history_bytes(), Some(123_456));
        assert_eq!(session.context_budget().max_history_bytes, 123_456);

        session.set_active_model(Some(ModelId::from("big")));
        assert_eq!(session.context_budget().max_history_bytes, 123_456);
        session.set_active_model(Some(ModelId::from("small")));
        assert_eq!(session.context_budget().max_history_bytes, 123_456);
        assert!(session.pending_compaction());
    }

    #[test]
    fn test_set_active_model_without_resolver_leaves_budget_and_flag() {
        let mut session = test_session();
        let before = session.context_budget().max_history_bytes;
        session.set_active_model(Some(ModelId::from("small")));
        assert_eq!(session.context_budget().max_history_bytes, before);
        assert!(!session.pending_compaction());
    }

    #[test]
    fn test_max_output_tokens_and_calibration_accessors() {
        let mut session = test_session();
        assert_eq!(session.max_output_tokens(), None);
        session.set_max_output_tokens(Some(16_384));
        assert_eq!(session.max_output_tokens(), Some(16_384));
        let session2 = test_session().with_max_output_tokens(Some(4_096));
        assert_eq!(session2.max_output_tokens(), Some(4_096));

        let default_bpt = crate::context_budget::DEFAULT_BYTES_PER_TOKEN;
        assert!((session.token_calibration().bytes_per_token() - default_bpt).abs() < f64::EPSILON);
        // 4 Bytes je Token beobachtet → EMA bewegt sich nach oben.
        session.token_calibration_mut().observe(40_000, 10_000);
        assert!(session.token_calibration().bytes_per_token() > default_bpt);
    }

    #[test]
    fn test_agent_session_set_active_provider_persists() {
        let mut session = test_session();
        let provider = ProviderId::from("anthropic");

        session.set_active_provider(Some(provider.clone()));

        assert_eq!(session.active_provider(), Some(&provider));

        // Builder-style with_active_provider also works.
        let session2 = test_session().with_active_provider(Some(provider.clone()));
        assert_eq!(session2.active_provider(), Some(&provider));

        // Clearing resets to None.
        let mut session3 = test_session().with_active_provider(Some(provider));
        session3.set_active_provider(None);
        assert_eq!(session3.active_provider(), None);
    }

    // -----------------------------------------------------------------------
    // Interaktionsmodus (W2-15)
    // -----------------------------------------------------------------------

    /// Baut eine Sandbox auf dem echten Harness-Verzeichnis mit genau den
    /// übergebenen Permissions. Kein Netzwerk, kein Schreibzugriff — nur die
    /// Auflösung eines bereits vorhandenen Pfades.
    fn test_sandbox(permissions: &[Permission]) -> TestResult<SandboxSpec> {
        let harness_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or(TestError::Missing(
                "harw-core hat ein Workspace-Elternverzeichnis",
            ))?
            .to_path_buf();
        let registry = WorkspaceRegistry::build(
            &harness_root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("core-mode-tests"),
                root: PathBuf::from("harw-core"),
            }],
        )
        .map_err(ctx("Test-Workspace ist registrierbar"))?;
        Ok(SandboxSpec::from_resolved(
            registry
                .resolve(
                    &TenantId::from_str("test-tenant"),
                    &WorkspaceId::from_str("core-mode-tests"),
                )
                .map_err(ctx("Test-Workspace löst auf"))?,
            PermissionSet::from_policy(permissions.iter().copied()),
        ))
    }

    fn test_spawn_context(permissions: &[Permission]) -> TestResult<SpawnContext> {
        Ok(SpawnContext {
            sandbox: test_sandbox(permissions)?,
            suggestions: None,
            capability_snapshot: None,
            approval_actor: None,
            organizational_role: AgentRoleId::RootOrchestrator,
            allowed_child_orchestrators: Vec::new(),
            trace: None,
            ceiling: None,
        })
    }

    fn session_with_permissions(permissions: &[Permission]) -> TestResult<AgentSession> {
        Ok(test_session().with_spawn_context(test_spawn_context(permissions)?))
    }

    fn permissions_of(session: &AgentSession) -> TestResult<PermissionSet> {
        Ok(session
            .spawn_context()
            .ok_or(TestError::Missing("Test-Session hat einen Spawn-Kontext"))?
            .sandbox
            .permissions()
            .clone())
    }

    #[test]
    fn test_default_mode_is_chat() {
        assert_eq!(test_session().mode(), InteractionMode::Chat);
    }

    #[test]
    fn test_with_mode_sets_and_applies_without_emitting_an_event() {
        let (turn_tx, mut turn_rx) = mpsc::unbounded_channel();
        let session = test_session()
            .with_turn_event_sink(turn_tx)
            .with_mode(InteractionMode::Explore);

        assert_eq!(session.mode(), InteractionMode::Explore);
        assert_eq!(session.activation().profile(), ToolProfile::Minimal);
        assert!(
            turn_rx.try_recv().is_err(),
            "der Aufbau einer Session ist kein Moduswechsel"
        );
    }

    #[test]
    fn test_set_mode_emits_mode_changed_with_canonical_name() -> TestResult {
        let (turn_tx, mut turn_rx) = mpsc::unbounded_channel();
        let mut session =
            session_with_permissions(&[Permission::ReadWorkspace])?.with_turn_event_sink(turn_tx);

        session.set_mode(InteractionMode::Explore);

        let event = turn_rx
            .try_recv()
            .map_err(ctx("ModeChanged wird gesendet"))?;
        assert!(
            matches!(event, TurnEvent::ModeChanged { mode } if mode == "explore"),
            "das Event muss den kanonischen Modusnamen tragen"
        );
        Ok(())
    }

    #[test]
    fn test_set_mode_without_event_sink_is_a_no_op_for_events() -> TestResult {
        // Ohne Sink darf set_mode nicht scheitern und muss trotzdem wirken.
        let mut session = session_with_permissions(&[Permission::WriteWorkspace])?;
        session.set_mode(InteractionMode::Explore);
        assert_eq!(session.mode(), InteractionMode::Explore);
        assert!(!permissions_of(&session)?.contains(Permission::WriteWorkspace));
        Ok(())
    }

    #[test]
    fn test_set_mode_explore_disables_write_and_shell_tools() -> TestResult {
        let mut session = session_with_permissions(&[
            Permission::ReadWorkspace,
            Permission::WriteWorkspace,
            Permission::ExecuteProcess,
        ])?;
        assert!(
            session
                .activation()
                .is_tool_enabled(&ToolName::new("fs.write")),
            "der Default-Modus Chat filtert nicht"
        );

        session.set_mode(InteractionMode::Explore);

        assert!(
            !session
                .activation()
                .is_tool_enabled(&ToolName::new("fs.write"))
        );
        assert!(
            !session
                .activation()
                .is_tool_enabled(&ToolName::new("shell.exec"))
        );
        assert!(
            session
                .activation()
                .is_tool_enabled(&ToolName::new("fs.read"))
        );
        assert!(
            session
                .activation()
                .is_tool_enabled(&ToolName::new("deps.source_read"))
        );
        assert_eq!(session.activation().profile(), ToolProfile::Minimal);
        Ok(())
    }

    #[test]
    fn test_set_mode_explore_removes_write_and_execute_permissions() -> TestResult {
        let mut session = session_with_permissions(&[
            Permission::ReadWorkspace,
            Permission::WriteWorkspace,
            Permission::ExecuteProcess,
            Permission::NetworkAccess,
            Permission::ReadCargoRegistry,
        ])?;

        session.set_mode(InteractionMode::Explore);

        let permissions = permissions_of(&session)?;
        assert!(permissions.contains(Permission::ReadWorkspace));
        assert!(permissions.contains(Permission::ReadCargoRegistry));
        for removed in [
            Permission::WriteWorkspace,
            Permission::ExecuteProcess,
            Permission::NetworkAccess,
        ] {
            assert!(
                !permissions.contains(removed),
                "Explore muss {removed:?} entziehen"
            );
        }
        Ok(())
    }

    #[test]
    fn test_set_mode_plan_keeps_network_but_not_mutation() -> TestResult {
        let mut session = session_with_permissions(&[
            Permission::ReadWorkspace,
            Permission::WriteWorkspace,
            Permission::ExecuteProcess,
            Permission::NetworkAccess,
        ])?;

        session.set_mode(InteractionMode::Plan);

        let permissions = permissions_of(&session)?;
        assert!(permissions.contains(Permission::NetworkAccess));
        assert!(permissions.contains(Permission::ReadWorkspace));
        assert!(!permissions.contains(Permission::WriteWorkspace));
        assert!(!permissions.contains(Permission::ExecuteProcess));
        assert!(
            session
                .activation()
                .is_tool_enabled(&ToolName::new("web.docs_rs"))
        );
        assert!(
            !session
                .activation()
                .is_tool_enabled(&ToolName::new("fs.write"))
        );
        Ok(())
    }

    /// Runde 5, Teil F: im Plan-Modus ist `plan.write` das einzige
    /// Schreibwerkzeug; `plan.exit`/`ask_user` sind sichtbar, alles
    /// Schreibende und Ausführende ist abgeschaltet.
    #[test]
    fn test_set_mode_plan_enables_only_the_plan_file_writer() -> TestResult {
        let mut session = session_with_permissions(&[
            Permission::ReadWorkspace,
            Permission::WriteWorkspace,
            Permission::ExecuteProcess,
            Permission::NetworkAccess,
        ])?;
        session.set_mode(InteractionMode::Plan);
        let enabled = |name: &str| session.activation().is_tool_enabled(&ToolName::new(name));
        for allowed in ["plan.write", "plan.exit", "ask_user", "fs.read", "explore"] {
            assert!(
                enabled(allowed),
                "{allowed} muss im Plan-Modus sichtbar sein"
            );
        }
        for denied in [
            "fs.write",
            "fs.edit",
            "shell.exec",
            "host.sudo_exec",
            "process.kill",
            "plan.enter",
        ] {
            assert!(
                !enabled(denied),
                "{denied} muss im Plan-Modus gesperrt sein"
            );
        }
        session.set_mode(InteractionMode::Work);
        assert!(
            session
                .activation()
                .is_tool_enabled(&ToolName::new("fs.write")),
            "das Verlassen des Plan-Modus stellt die Schreibwerkzeuge wieder her"
        );
        Ok(())
    }

    #[test]
    fn test_set_mode_work_restores_the_base_and_invents_nothing() -> TestResult {
        // Der Schnitt läuft immer von der Basis-Sandbox aus: reversibel nach
        // oben bis zur Basis, nie darüber hinaus.
        let mut session =
            session_with_permissions(&[Permission::ReadWorkspace, Permission::WriteWorkspace])?;

        session.set_mode(InteractionMode::Explore);
        assert!(!permissions_of(&session)?.contains(Permission::WriteWorkspace));

        session.set_mode(InteractionMode::Work);

        let permissions = permissions_of(&session)?;
        assert!(
            permissions.contains(Permission::WriteWorkspace),
            "Work muss die Basis-Permission zurückgeben — sonst wäre /mode eine Ratsche"
        );
        assert!(
            !permissions.contains(Permission::ExecuteProcess),
            "Work darf keine Permission erfinden, die die Basis nie hatte"
        );
        assert!(permissions.contains(Permission::ReadWorkspace));
        Ok(())
    }

    #[test]
    fn test_set_mode_work_reopens_tool_activation_only() -> TestResult {
        let mut session = session_with_permissions(&[Permission::ReadWorkspace])?;

        session.set_mode(InteractionMode::Explore);
        assert!(
            !session
                .activation()
                .is_tool_enabled(&ToolName::new("fs.write"))
        );

        session.set_mode(InteractionMode::Work);

        assert_eq!(session.activation().profile(), ToolProfile::Full);
        assert!(
            session
                .activation()
                .is_tool_enabled(&ToolName::new("fs.write")),
            "die Tool-Aktivierung ist nicht monoton — die Sandbox trägt die Grenze"
        );
        assert!(
            !permissions_of(&session)?.contains(Permission::WriteWorkspace),
            "ein wieder sichtbares Werkzeug bekommt keine Autorität zurück"
        );
        Ok(())
    }

    #[test]
    fn test_set_mode_drops_earlier_tool_overrides() -> TestResult {
        let mut session = session_with_permissions(&[Permission::ReadWorkspace])?;
        session
            .activation_mut()
            .enable_tool(ToolName::new("shell.exec"));

        session.set_mode(InteractionMode::Explore);

        assert!(
            !session
                .activation()
                .is_tool_enabled(&ToolName::new("shell.exec")),
            "ein alter enable_tool-Override darf kein Werkzeug in Explore retten"
        );
        Ok(())
    }

    #[test]
    fn test_set_mode_preserves_the_workspace_binding() -> TestResult {
        let before = test_sandbox(&[Permission::ReadWorkspace, Permission::WriteWorkspace])?;
        let mut session = test_session().with_spawn_context(SpawnContext {
            sandbox: before.clone(),
            suggestions: None,
            capability_snapshot: None,
            approval_actor: None,
            organizational_role: AgentRoleId::RootOrchestrator,
            allowed_child_orchestrators: Vec::new(),
            trace: None,
            ceiling: None,
        });

        session.set_mode(InteractionMode::Explore);

        let after = session
            .spawn_context()
            .ok_or(TestError::Missing("Spawn-Kontext bleibt erhalten"))?
            .sandbox
            .clone();
        assert_eq!(before.workspace(), after.workspace());
        assert!(after.ensure_child_of(&before).is_ok());
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Trace-Kontext (AW1-01b)
    // -----------------------------------------------------------------------

    #[test]
    fn test_with_spawn_context_preserves_the_trace_context() -> TestResult {
        let trace = TraceContext::new("a".repeat(32), "b".repeat(16))
            .map_err(ctx("valid trace context"))?;
        let mut context = test_spawn_context(&[Permission::ReadWorkspace])?;
        context.trace = Some(trace.clone());

        let session = test_session().with_spawn_context(context);

        assert_eq!(
            session
                .spawn_context()
                .ok_or(TestError::Missing("session has a spawn context"))?
                .trace,
            Some(trace)
        );
        Ok(())
    }

    #[test]
    fn test_spawn_context_without_a_trace_stays_none_through_construction() -> TestResult {
        let session = session_with_permissions(&[Permission::ReadWorkspace])?;

        assert!(
            session
                .spawn_context()
                .ok_or(TestError::Missing("session has a spawn context"))?
                .trace
                .is_none(),
            "a context built without a trace must not gain one along the way"
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Kontext-Decke (AW2-02)
    // -----------------------------------------------------------------------

    #[test]
    fn test_with_spawn_context_preserves_the_context_ceiling() -> TestResult {
        // `with_spawn_context` deliberately restricts only `sandbox` via the
        // mode's permission ceiling (see its doc comment above); the context
        // ceiling is a separate authority cut entirely by
        // `ManagedAgentSpawner::admit` at handoff time, and must survive
        // session construction untouched — the same guarantee already
        // covered for `trace` above.
        let ceiling = ContextCeiling {
            sections: [SectionName::try_new("history.tail")?]
                .into_iter()
                .collect(),
            max_trust: TrustClass::Evidence,
            budget: ContextBudgetSpec {
                total: harw_lens_types::BudgetSpec { total: 500 },
                per_section: std::collections::BTreeMap::new(),
            },
        };
        let mut context = test_spawn_context(&[Permission::ReadWorkspace])?;
        context.ceiling = Some(ceiling.clone());

        let session = test_session().with_spawn_context(context);

        assert_eq!(
            session
                .spawn_context()
                .ok_or(TestError::Missing("session has a spawn context"))?
                .ceiling,
            Some(ceiling),
            "with_spawn_context must not silently widen, narrow, or drop the context ceiling"
        );
        Ok(())
    }

    #[test]
    fn test_spawn_context_without_a_ceiling_stays_none_through_construction() -> TestResult {
        let session = session_with_permissions(&[Permission::ReadWorkspace])?;

        assert!(
            session
                .spawn_context()
                .ok_or(TestError::Missing("session has a spawn context"))?
                .ceiling
                .is_none(),
            "a context built without a ceiling must not gain one along the way; \
             `admit` — not construction — is where `None` is later read as fail-closed"
        );
        Ok(())
    }

    #[test]
    fn test_mode_ceiling_is_independent_of_builder_order() -> TestResult {
        let permissions = [
            Permission::ReadWorkspace,
            Permission::WriteWorkspace,
            Permission::ExecuteProcess,
        ];
        let mode_first = test_session()
            .with_mode(InteractionMode::Explore)
            .with_spawn_context(test_spawn_context(&permissions)?);
        let mut context_first =
            test_session().with_spawn_context(test_spawn_context(&permissions)?);
        context_first.set_mode(InteractionMode::Explore);

        assert_eq!(
            permissions_of(&mode_first)?,
            permissions_of(&context_first)?
        );
        assert!(!permissions_of(&mode_first)?.contains(Permission::WriteWorkspace));
        Ok(())
    }

    #[test]
    fn test_set_mode_without_spawn_context_still_filters_tools() {
        let mut session = test_session();
        assert!(session.spawn_context().is_none());

        session.set_mode(InteractionMode::Explore);

        assert_eq!(session.mode(), InteractionMode::Explore);
        assert!(session.spawn_context().is_none());
        assert!(
            !session
                .activation()
                .is_tool_enabled(&ToolName::new("fs.write"))
        );
    }

    // -----------------------------------------------------------------------
    // mode_ceiling (W2d1/F-C, T3)
    // -----------------------------------------------------------------------

    #[test]
    fn test_mode_ceiling_full_mode_keeps_base_disabled_tool_out() {
        // Basis: Full-Profil mit einem explizit verbotenen Werkzeug. Modus mit
        // Full-Profil (Work) filtert namensbasiert nicht — die Decke muss das
        // Verbot der Basis trotzdem durchreichen, kein Modus darf mehr
        // erlauben als die Basis je trug.
        let mut base = SessionActivation::new(ToolProfile::Full);
        base.disable_tool(ToolName::new("x"));
        let session = test_session()
            .with_activation(base)
            .with_mode(InteractionMode::Work);

        let ceiling = session.mode_ceiling();

        assert_eq!(ceiling.profile(), ToolProfile::Full);
        assert!(!ceiling.is_tool_enabled(&ToolName::new("x")));
        assert!(ceiling.is_tool_enabled(&ToolName::new("fs.read")));
    }

    #[test]
    fn test_mode_ceiling_explore_mode_intersects_allowlist_with_base() {
        // Basis: Minimal-Profil mit zwei explizit freigeschalteten Namen
        // ("fs.read" liegt auch in der Explore-Positivliste, "custom.tool"
        // nicht). Modus: Explore (Minimal + Positivliste). Die Decke darf nur
        // enthalten, was in beiden Seiten sichtbar ist.
        let mut base = SessionActivation::new(ToolProfile::Minimal);
        base.enable_tool(ToolName::new("fs.read"));
        base.enable_tool(ToolName::new("custom.tool"));
        let session = test_session()
            .with_activation(base)
            .with_mode(InteractionMode::Explore);

        let ceiling = session.mode_ceiling();

        assert_eq!(ceiling.profile(), ToolProfile::Minimal);
        // In beiden Seiten sichtbar: bleibt sichtbar.
        assert!(ceiling.is_tool_enabled(&ToolName::new("fs.read")));
        // Nur in der Basis freigeschaltet, nicht in der Explore-Positivliste:
        // fällt aus dem Schnitt heraus.
        assert!(!ceiling.is_tool_enabled(&ToolName::new("custom.tool")));
        // Nur in der Explore-Positivliste, nicht in der Basis freigeschaltet:
        // fällt ebenfalls heraus.
        assert!(!ceiling.is_tool_enabled(&ToolName::new("fs.list")));
        assert!(!ceiling.is_tool_enabled(&ToolName::new("shell.exec")));
    }

    #[test]
    fn test_mode_ceiling_matches_activation_after_set_mode() -> TestResult {
        // `SessionActivation` hat kein `PartialEq` (siehe activation.rs) —
        // der Vergleich läuft daher über `is_tool_enabled` an Sondennamen
        // sowie `profile()`, wie schon `intersect`s eigene Tests in
        // activation.rs es tun.
        let mut session = session_with_permissions(&[
            Permission::ReadWorkspace,
            Permission::WriteWorkspace,
            Permission::ExecuteProcess,
        ])?;

        session.set_mode(InteractionMode::Explore);

        let ceiling = session.mode_ceiling();
        assert_eq!(session.activation().profile(), ceiling.profile());
        for name in [
            "fs.read",
            "fs.write",
            "shell.exec",
            "deps.source_read",
            "custom.tool",
        ] {
            let tool = ToolName::new(name);
            assert_eq!(
                session.activation().is_tool_enabled(&tool),
                ceiling.is_tool_enabled(&tool),
                "activation() muss nach set_mode exakt mode_ceiling() entsprechen für {name}"
            );
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Zustandsautomat (W4a/A-SESS: F-151, F-152)
    // -----------------------------------------------------------------------

    #[test]
    fn test_complete_turn_accepts_the_active_turn() -> TestResult {
        let mut session = test_session();
        let handle = session
            .try_start_turn()
            .map_err(ctx("turn starts from Idle"))?;

        session
            .complete_turn(handle, TokenUsage::default())
            .map_err(ctx("the active turn completes"))?;

        assert_eq!(session.state(), &SessionState::Idle);
        assert!(session.current_turn().is_none());
        Ok(())
    }

    #[test]
    fn test_complete_turn_rejects_a_foreign_turn_id_without_mutation() -> TestResult {
        let mut session = test_session();
        let active = session
            .try_start_turn()
            .map_err(ctx("turn starts from Idle"))?;
        let usage = TokenUsage {
            cache_separate: false,
            input_tokens: 10,
            output_tokens: 5,
            reasoning_tokens: None,
            cached_tokens: None,
            cache_write_tokens: None,
        };

        let Err(error) = session.complete_turn(
            TurnHandle {
                turn_id: TurnId::new(),
                session_id: session.id().clone(),
            },
            usage,
        ) else {
            return Err(TestError::Unexpected(
                "a stale turn id must be rejected".to_owned(),
            ));
        };

        assert!(matches!(error, CoreError::TurnRejected(_)));
        assert_eq!(session.state(), &SessionState::Running);
        assert_eq!(session.current_turn(), Some(&active.turn_id));
        assert_eq!(session.total_usage(), &TokenUsage::default());
        Ok(())
    }

    #[test]
    fn test_complete_turn_from_failed_is_rejected() -> TestResult {
        let mut session = test_session();
        let handle = session
            .try_start_turn()
            .map_err(ctx("turn starts from Idle"))?;
        session.fail("provider timeout".to_owned());

        let Err(error) = session.complete_turn(handle, TokenUsage::default()) else {
            return Err(TestError::Unexpected(
                "Failed is left only through recover".to_owned(),
            ));
        };

        assert!(matches!(error, CoreError::TurnRejected(_)));
        assert!(matches!(session.state(), SessionState::Failed(_)));
        Ok(())
    }

    #[test]
    fn test_recover_returns_failed_session_to_idle_and_allows_a_new_turn() -> TestResult {
        let mut session = test_session();
        let _handle = session
            .try_start_turn()
            .map_err(ctx("turn starts from Idle"))?;
        session.history_mut().push_tool_call(
            ToolCallId::from_str("crashed"),
            "fs.read",
            serde_json::json!({}),
        );
        session.fail("provider 5xx".to_owned());

        let repaired = session.recover().map_err(ctx("Failed is recoverable"))?;

        assert_eq!(repaired, vec![ToolCallId::from_str("crashed")]);
        assert_eq!(session.state(), &SessionState::Idle);
        assert!(session.current_turn().is_none());
        assert_eq!(session.history().len(), 2);
        assert!(session.try_start_turn().is_ok());
        Ok(())
    }

    #[test]
    fn test_recover_from_idle_is_rejected() -> TestResult {
        let mut session = test_session();

        let Err(error) = session.recover() else {
            return Err(TestError::Unexpected(
                "only Failed is recoverable".to_owned(),
            ));
        };

        assert!(matches!(error, CoreError::TurnRejected(_)));
        assert_eq!(session.state(), &SessionState::Idle);
        Ok(())
    }
}
