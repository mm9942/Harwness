//! # app
//!
//! Asynchroner ratatui-Chat-Renderer für die Harwness-TUI.
//!
//! ## Verantwortung
//!
//! Dieses Modul besitzt den interaktiven Chat-Loop. Es baut eine
//! [`harw_core::AgentSession`] mit dem Bootstrap-Turn-Loop
//! ([`harw_core::run_turn`]) und betreibt einen **asynchronen**
//! `tokio::select!`-Event-Loop über drei Quellen (Spec-Abschnitt 2.1):
//! - Terminal-Eingaben (`TuiEvent`) von einem blockierenden
//!   Reader-Thread (`spawn_input_reader`),
//! - Anwendungsereignisse (`HarwEvent`) vom internen Bus,
//! - Frame-Anforderungen (`frame_requester`), koalesziert von einem
//!   Scheduler-Task, der `TuiEvent::Draw` einspeist.
//!
//! - Terminal-Setup (Raw-Mode + Bracketed-Paste, **Alternate-Screen**) hinter
//!   einem RAII-Guard, der den Terminalzustand bei *jedem* Exit-Pfad (Fehler,
//!   Panic, sauberer Abbruch) zurückstellt.
//! - Verlauf wird intern in `ChatApp::cells` gespeichert und über einen
//!   scrollbaren `Paragraph`-Widget im Fullscreen-Layout gerendert. Kein
//!   `insert_before` / Terminal-Scrollback.
//! - Interner Scroll: PageUp / PageDown verschieben den Scroll-Offset innerhalb
//!   des History-Bereichs.
//! - Reine Zeilen-Logik (TTY-frei) ist in [`classify_line`] ausgelagert; die
//!   Tastensteuerung liegt in `handle_key`, das ausschließlich Zustand mutiert
//!   und `HarwEvent`s emittiert (testbar ohne Terminal).
//! - Da [`harw_core::ModelProvider`] kein Token-Streaming anbietet, wird die
//!   Antwort nach dem Turn **simuliert gestreamt**: der Volltext läuft durch
//!   einen `StreamCollector` und wird zeilenweise, frame-getaktet, enthüllt.
//!
//! ## Pausierte Turns (AP W5-03)
//! Ein Turn, den der Kern an einer Freigabe ([`TurnOutcome::AwaitingApproval`])
//! oder an einem Kind-Handoff ([`TurnOutcome::AwaitingChild`]) anhält, wird
//! **zu Ende geführt**, nicht abgebrochen: `drive_pauses_to_completion`
//! übergibt das Ergebnis an [`crate::approval::ApprovalDriver::drive_to_completion`]
//! und pollt dabei den Fragekanal, die Tastatur und die Turn-Ereignisse weiter,
//! während der Spinner läuft. Der `Arc<TuiApprovalHandler>` liegt dabei
//! gleichzeitig in der [`ExtensionRegistry`] der Session und im Treiber; der
//! Spawn-Kontext trägt zwingend einen `ApprovalActor`.
//!
//! ## Schlüsseltypen
//! - [`ChatApp`] — Zustand des Renderers (Eingabepuffer, Verlauf-Log, Popup, Scroll).
//! - [`Role`] — Rolle einer Chat-Nachricht (User, Assistant, System).
//! - [`LineAction`] — TTY-freies Ergebnis der Zeilen-Klassifizierung.
//! - [`TuiPlanServices`] — optionale Plan-/Ziel-Stores der Composition-Root.
//! - [`TuiError`] — Fehlertyp dieses Moduls.
//! - `TerminalGuard` — RAII-Guard für Raw-Mode und Alternate-Screen (privat).
//! - `SharedHistoryCell` — Verlaufszelle, die nach dem Anhängen noch
//!   fortgeschrieben werden kann (privat).
//!
//! ## Nebenläufigkeit
//! Der asynchrone Loop läuft auf einem `current_thread`-Tokio-Runtime.
//! Ein OS-Thread liest Tastatur-Ereignisse blockierend; ein Tokio-Task
//! koalesziert Frame-Anforderungen. Alle drei kommunizieren über
//! unbounded MPSC-Kanäle.
//!
//! ## Fehler
//! Alle Fehler dieses Moduls werden als [`TuiError`] ausgedrückt.
//!
//! ## Beispiele
//! ```ignore
//! use harw_tui::app::run_chat_tui;
//! // Provider wird aus dem Crate-Setup bezogen.
//! // run_chat_tui(provider)?;
//! ```

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::io::{self, Stdout};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, KeyCode, KeyEvent, KeyModifiers,
};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use harw_agent_dsl::{ExecutableAgentIr, roles::AgentRoleId};
use harw_core::{
    AgentSession, ChildLimits, ChildRegistryFactory, ConversationHistory, CoreError,
    InteractionMode, ManagedAgentSpawner, ModelError, ModelMessage, ModelProvider, SessionManager,
    SpawnContext, StateStore, TurnInput, TurnOutcome, run_turn,
};
use harw_extension_api::registry::ContextProviderRegistrationError;
use harw_extension_api::{AgentSpawnError, ApprovalHandler, ExtensionRegistry, SpawnInput};
use harw_observe::TraceContext;
use harw_operations::adapter::{CommandAdapter, ModelToolProvider};
use harw_operations::registry::OperationRegistry;
use harw_operations::{OpContext, PermissionTier, ServiceMap, SharedSessionController};
use harw_plan::PlanStore;
use harw_plan::goal::{GoalStore, evaluate_goal};
use harw_protocol::events::{SessionEvent, TurnEvent};
use harw_protocol::items::{ContentPart, TurnItem};
use harw_registry_defaults::assemble_default_registry;
use harw_registry_defaults::profile::{IdentityOverrides, profile_for_role, role_names};
use harw_sandbox::{SandboxSpec, WorkspaceRegistration, WorkspaceRegistry};
use harw_types::TokenUsage;
use harw_types::{AgentRole, ApprovalActor, SessionId, TenantId, WorkspaceId};
use uuid::Uuid;

use crate::CommandRegistry;
use crate::approval::{
    ApprovalDriver, ApprovalDriverError, ApprovalPrompt, ApprovalPromptReceiver, ChildTurnDriver,
    TuiApprovalHandler,
};
use crate::chat_scroll::{ChatScroll, ScrollAction};
use crate::command_exec::{CommandServices, execute_command_as};
use crate::command_popup::{CommandPopup, PopupAction};
use crate::events::{HarwEvent, HarwEventSender, harw_event_channel};
use crate::frame_requester::{FrameRequester, MIN_FRAME_INTERVAL, frame_channel};
use crate::gateway::ChatGateway;
use crate::history_cell::{
    ApprovalPromptCell, AssistantHistoryCell, GoalCell, HistoryCell, PlainHistoryCell,
    PlanGraphCell, ReasoningHistoryCell, SubAgentCell, SubAgentStatus, ToolCallHistoryCell,
    ToolResultHistoryCell, UserHistoryCell,
};
use crate::input_editor::{InputAction, InputEditor};
use crate::input_history::InputHistoryStore;
use crate::input_reader::spawn_input_reader;
use crate::session_controller::TuiSessionController;
use crate::spinner::Spinner;
use crate::style;
use crate::tui_event::TuiEvent;

/// Taktrate der Spinner-Animation während ein Turn läuft.
const SPINNER_INTERVAL: Duration = Duration::from_millis(80);

/// Willkommens-Systemzeile beim Start des Chats.
const WELCOME: &str = "Willkommen. Tippe eine Nachricht — Enter zum Senden.";

const NON_TEXT_CONTENT_PLACEHOLDER: &str = "[non-text content]";

/// Permission tier carried by the trusted local TUI actor for slash-command
/// dispatch. This stays aligned with [`trusted_tui_spawn_context`], which
/// identifies that actor as an `ApprovalActor::Operator`.
const LOCAL_TUI_OPERATION_PERMISSION: PermissionTier = PermissionTier::Operator;

/// Obergrenze der in einer [`ToolCallHistoryCell`] gezeigten Argument-Vorschau.
const TOOL_ARGUMENTS_PREVIEW_CHARS: usize = 80;

/// Wie viele Einträge die persistente Eingabe-Historie beim Start zurückliefert.
/// Großzügig genug, dass Wochen alter Gebrauch erreichbar bleibt, klein genug,
/// dass das Laden beim Start nicht auffällt.
const INPUT_HISTORY_CAP: usize = 1000;

/// Begründung, die eine per Taste abgelehnte Freigabe trägt.
const REASON_OPERATOR_REJECTED: &str = "operator rejected the tool call in the terminal UI";

/// Begründung, die eine per Abbruch (Esc/Ctrl+C) abgelehnte Freigabe trägt.
const REASON_OPERATOR_CANCELLED: &str = "operator cancelled the approval prompt in the terminal UI";

// ── Geteilte, nachträglich veränderbare Verlaufszellen ───────────────────────

/// Verlaufszelle, deren Inhalt der Aufrufer nach dem Anhängen noch verändern darf.
///
/// # Beschreibung
/// [`HistoryCell`] bietet **kein** Downcasting, und [`ChatApp`] hält den Verlauf
/// als `Vec<Box<dyn HistoryCell>>`. Eine Zelle, die über mehrere Ereignisse
/// hinweg fortgeschrieben wird — [`SubAgentCell`] über `ChildSpawned` →
/// `ChildProgress` → `ChildCompleted`, [`ApprovalPromptCell`] über Frage →
/// Entscheidung — braucht deshalb einen Seitenkanal auf **dieselbe** Instanz.
///
/// Gewählte Lösung: genau eine Instanz hinter einem `Arc<Mutex<T>>`. Der
/// Verlauf hält einen `SharedHistoryCell`-Wrapper, der Seitenkanal (bzw. die
/// aufrufende Funktion) hält einen `Arc`-Klon auf dieselbe Zelle. Damit
/// erzeugen drei Kind-Ereignisse **eine** Zelle, nicht drei.
///
/// Warum `Arc<Mutex<_>>` und nicht `Rc<RefCell<_>>`, obwohl der Renderer
/// einthreadig ist: [`HistoryCell`] fordert `Send + Sync`, was `Rc`/`RefCell`
/// nicht erfüllen. Der Lock wird ausschließlich unmittelbar um eine Mutation
/// oder um einen Render-Aufruf genommen und nie über einen `await`-Punkt
/// gehalten — es gibt daher keine Contention und keinen Deadlock-Pfad.
///
/// Warum kein neuer Zellentyp in `history_cell.rs`: dieser Wrapper ist reine
/// Renderer-Mechanik (Besitz und Auffindbarkeit), keine Darstellungslogik. Die
/// Darstellung bleibt vollständig bei der gewrappten Zelle.
///
/// # Nebenläufigkeit
/// `Send + Sync`, sofern `T` es ist. Ein vergifteter Lock führt **nicht** zu
/// einem Panic, sondern zu einer sichtbaren Ersatzzeile.
#[derive(Debug)]
struct SharedHistoryCell<T: HistoryCell> {
    /// Die geteilte, veränderbare Zelle.
    inner: Arc<Mutex<T>>,
}

impl<T: HistoryCell> HistoryCell for SharedHistoryCell<T> {
    /// Rendert die geteilte Zelle unter kurzzeitig gehaltenem Lock.
    ///
    /// # Argumente
    /// - `width` (`u16`): verfügbare Spaltenbreite.
    /// - `theme` ([`style::Theme`]): aktives Farbschema.
    ///
    /// # Rückgabe
    /// Die Zeilen der gewrappten Zelle; bei vergiftetem Lock genau eine
    /// Hinweiszeile statt eines Panics.
    fn display_lines(&self, width: u16, theme: style::Theme) -> Vec<Line<'static>> {
        match self.inner.lock() {
            Ok(cell) => cell.display_lines(width, theme),
            Err(_) => vec![Line::from(Span::styled(
                "⚠ Verlaufszelle nicht lesbar (Sperre vergiftet)".to_owned(),
                style::warning_style(theme),
            ))],
        }
    }
}

/// Zustand, den die Verarbeitung von [`TurnEvent`]s über einen Turn hinweg hält.
///
/// # Beschreibung
/// Zwei Seitenkanäle, die der Verlauf selbst nicht ausdrücken kann:
/// - `pending_tool_names` korreliert `ToolCallRequested` → `ToolCallCompleted`
///   (letzteres trägt nur die `call_id`),
/// - `child_cells` findet zu einer `child_id` die **eine** bereits angehängte
///   [`SubAgentCell`] wieder, damit `ChildProgress`/`ChildCompleted` sie
///   fortschreiben statt eine zweite Zelle anzulegen.
///
/// Ein Eintrag in `child_cells` bleibt nach `ChildCompleted` bewusst bestehen:
/// eine verspätet eintreffende Fortschrittsmeldung soll dieselbe Zelle treffen
/// und keine neue erzeugen.
#[derive(Debug, Default)]
struct TurnEventState {
    /// `call_id` → Werkzeugname des zugehörigen `ToolCallRequested`.
    pending_tool_names: HashMap<harw_types::ToolCallId, String>,
    /// `child_id` → die eine Verlaufszelle dieses Kindes.
    child_cells: HashMap<String, Arc<Mutex<SubAgentCell>>>,
}

impl TurnEventState {
    /// Wendet eine Mutation auf die Zelle eines bereits bekannten Kindes an.
    ///
    /// # Argumente
    /// - `child_id` (`&str`): Bezeichner aus dem `TurnEvent`.
    /// - `update` (`impl FnOnce(&mut SubAgentCell)`): die Mutation.
    ///
    /// # Rückgabe
    /// `true`, wenn die Zelle gefunden und verändert wurde; `false`, wenn zu
    /// dieser `child_id` keine Zelle existiert oder ihr Lock vergiftet ist.
    fn update_child<F>(&self, child_id: &str, update: F) -> bool
    where
        F: FnOnce(&mut SubAgentCell),
    {
        let Some(cell) = self.child_cells.get(child_id) else {
            tracing::warn!(child = %child_id, "tui.child_cell.unknown_child");
            return false;
        };
        match cell.lock() {
            Ok(mut guard) => {
                // Explizit dereferenziert: der Callback erwartet `&mut SubAgentCell`,
                // nicht den Guard.
                update(&mut guard);
                true
            }
            Err(_) => {
                tracing::error!(child = %child_id, "tui.child_cell.lock_poisoned");
                false
            }
        }
    }
}

/// Plan- und Ziel-Dienste, die der Renderer für [`PlanGraphCell`] und
/// [`GoalCell`] braucht.
///
/// # Beschreibung
/// Die TUI öffnet **niemals** selbst einen Plan-/Goal-Store: zwei nebenläufige
/// Store-Instanzen auf demselben Verzeichnis wären ein Datenverlust-Risiko
/// (dieselbe Begründung wie in `harw-cli/src/chat.rs::OneShotPlanServices`).
/// Die Composition-Root baut die Dienste genau einmal und reicht sie über
/// [`run_chat_tui_resumable_with_plan`] durch.
///
/// `None` an dieser Schnittstelle bedeutet: der Renderer zeigt Plan-Updates als
/// einzeilige Systemmeldung statt als Graph und kann nach `/goal check` keinen
/// Zielstand darstellen — er scheitert daran aber nie.
///
/// # Nebenläufigkeit
/// `Clone`: beide Felder sind `Arc`-Zeiger; ein Klon teilt denselben Store.
#[derive(Clone)]
pub struct TuiPlanServices {
    /// Der Plan-Store der aktiven Profilsitzung.
    pub plan_store: Arc<dyn PlanStore>,
    /// Der Goal-Store der aktiven Profilsitzung.
    pub goal_store: Arc<dyn GoalStore>,
    /// Zusätzliche Kontext-Beitragende, die in **jeden** Turn dieser Session
    /// einfließen sollen — insbesondere der Ziel-Kontext.
    ///
    /// Bewusst als `Arc<dyn ContextProvider>` und nicht als konkreter Typ:
    /// `harw-tui` hängt nicht an `harw-plan-bridge` und soll es auch nicht.
    /// Die Composition-Root kennt beide Seiten und baut den Provider; die
    /// Oberfläche hängt ihn nur noch in ihre Registry.
    ///
    /// Ohne diesen Weg überlebt ein per `--goal` gesetztes Ziel zwar im Store,
    /// erreicht aber keinen einzigen Modell-Turn.
    pub context_providers: Vec<Arc<dyn harw_extension_api::ContextProvider>>,
    /// Zusätzliche Freigabe-Politiken, die **hinter** die bestehenden gehängt
    /// werden (etwa die aus `[policy] require_approval_for` kompilierte).
    ///
    /// Anhängen kann nie lockern: `harw_core::turn_loop::check_approval` nimmt
    /// die erste Nicht-`Allow`-Entscheidung, eine zusätzliche Politik kann also
    /// nur weitere Werkzeuge unter Vorbehalt stellen.
    pub approval_handlers: Vec<Arc<dyn harw_extension_api::ApprovalHandler>>,
    /// Interaktionsmodus, mit dem die Session startet (aus `--mode` bzw.
    /// `[mode] default`); `None` behält den Session-Default.
    pub initial_mode: Option<harw_core::InteractionMode>,
}

/// Handgeschriebene `Debug`-Implementierung — die Store-Traits leiten kein
/// `Debug` ab.
impl std::fmt::Debug for TuiPlanServices {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TuiPlanServices")
            .field("plan_store", &"<dyn PlanStore>")
            .field("goal_store", &"<dyn GoalStore>")
            .finish()
    }
}

/// Internal control outcome returned when the event loop changes mode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TuiRunOutcome {
    /// The operator left the TUI normally.
    Quit,
    /// The active terminal must list or resolve a durable session. `None`
    /// represents `/resume` without an argument.
    Resume { selector: Option<String> },
}

/// Resolves the durable sessions that can be selected from the local TUI.
///
/// Discovery belongs to the composition root because the TUI does not own the
/// profile layout. The selector is deliberately synchronous: both commands
/// are initiated from the single-threaded event loop and implementations only
/// inspect local durable-session metadata.
pub trait ResumeSessionSelector {
    /// Returns the durable session IDs currently available to resume.
    fn available_sessions(&self) -> Result<Vec<SessionId>, String>;

    /// Resolves an exact ID or caller-defined unambiguous selector.
    fn resolve_session(&self, selector: &str) -> Result<SessionId, String>;
}

/// Local gateway with a replaceable session for the interactive `/resume`
/// path. `crate::gateway::LocalGateway` intentionally keeps its fields private
/// and exposes no session replacement operation, so this small adapter owns
/// that one additional lifecycle operation without widening the shared trait.
struct ResumableGateway {
    session: AgentSession,
    store: Arc<dyn StateStore>,
    model: Arc<dyn ModelProvider>,
}

impl ResumableGateway {
    fn new(
        session: AgentSession,
        store: Arc<dyn StateStore>,
        model: Arc<dyn ModelProvider>,
    ) -> Self {
        Self {
            session,
            store,
            model,
        }
    }

    fn replace_session(&mut self, session: AgentSession) {
        self.session = session;
    }
}

impl crate::gateway::ChatGateway for ResumableGateway {
    fn session_mut(&mut self) -> &mut AgentSession {
        &mut self.session
    }

    fn store(&self) -> &dyn StateStore {
        self.store.as_ref()
    }

    fn model(&self) -> &dyn ModelProvider {
        self.model.as_ref()
    }

    fn borrow_turn_ctx(&mut self) -> (&mut AgentSession, &dyn StateStore, &dyn ModelProvider) {
        (&mut self.session, self.store.as_ref(), self.model.as_ref())
    }
}

/// Rolle, unter der eine Chat-Zelle angehängt wird.
///
/// # Beschreibung
/// Steuert, welche interne `HistoryCell`-Implementierung beim Anhängen einer
/// Nachricht in [`ChatApp::push_line`] erzeugt wird. Spec-Quelle: SLICE 7.
///
/// # Beispiele
/// ```ignore
/// use harw_tui::app::{ChatApp, Role};
/// // adapters/sandbox/session_id werden real in `run_chat_tui` gebaut.
/// let mut app = ChatApp::new(Vec::new(), sandbox, session_id);
/// app.push_line(Role::User, "Hallo");
/// app.push_line(Role::Assistant, "Antwort");
/// app.push_line(Role::System, "Hinweis");
/// assert_eq!(app.cells_len(), 3);
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Vom Benutzer eingegebene Nachricht — erzeugt intern eine `UserHistoryCell`.
    User,
    /// Antwort des Modells — erzeugt intern eine `AssistantHistoryCell`.
    Assistant,
    /// System-/Hinweistext des Renderers — erzeugt intern eine `PlainHistoryCell`.
    System,
}

/// Entscheidung, was mit einer abgeschickten Eingabezeile geschehen soll.
///
/// # Beschreibung
/// TTY-freies Zwischenergebnis von [`classify_line`], damit die Zeilen-Logik
/// ohne Terminal getestet werden kann. Der Event-Loop in `run_loop` wertet
/// diese Variante aus und delegiert an den jeweils passenden Zweig.
///
/// # Beispiele
/// ```ignore
/// use harw_tui::app::{classify_line, LineAction};
/// assert_eq!(classify_line(""), LineAction::Ignore);
/// assert_eq!(classify_line("/quit"), LineAction::Quit);
/// assert!(matches!(classify_line("Hallo"), LineAction::Chat(_)));
/// assert!(matches!(classify_line("/status"), LineAction::Command(_)));
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LineAction {
    /// Loop verlassen — ausgelöst durch `/quit` oder `/exit`.
    Quit,
    /// Nichts tun — ausgelöst durch eine leere oder rein-whitespace Eingabe.
    Ignore,
    /// Einen Chat-Turn mit dem enthaltenen Text treiben.
    Chat(String),
    /// Eine `/command`-Zeile (oder `!`-Shell/Note/Mention) asynchron über die
    /// Operation-Adapter-Pipeline ausführen ([`HarwEvent::Command`] →
    /// [`crate::command_exec::execute_command`]); enthält die unveränderte
    /// Rohzeile. Die lokale [`CommandRegistry`] wird davon unabhängig
    /// weiterhin für die Popup-Autocomplete-Anzeige verwendet.
    Command(String),
    /// Eine Systemzeile im Scrollback anzeigen (Hinweis oder abgelehnte Eingabe).
    System(String),
}

/// Klassifiziert eine abgeschickte Zeile in eine [`LineAction`].
///
/// # Beschreibung
/// Reine, TTY-freie Funktion: nutzt [`crate::classify_input`] und übersetzt das
/// Ergebnis in die vom Event-Loop ausführbare Aktion. `/quit` und `/exit`
/// beenden den Loop; ein leerer Chat-Text wird ignoriert.
///
/// # Argumente
/// - `line` (`&str`): die vom Benutzer abgeschickte Rohzeile (ohne Zeilenende).
///
/// # Rückgabe
/// Die passende [`LineAction`].
#[must_use]
pub fn classify_line(line: &str) -> LineAction {
    let trimmed = line.trim();
    // Leere Zeile beendet nicht — sie wird ignoriert (codex-Muster).
    // Beendet wird über Ctrl+C 2×, Ctrl+D oder `/quit`/`/exit`.
    if trimmed.is_empty() {
        return LineAction::Ignore;
    }
    if trimmed == "/quit" || trimmed == "/exit" {
        return LineAction::Quit;
    }

    match crate::classify_input(line) {
        Ok(crate::Invocation::Chat(text)) => {
            if text.trim().is_empty() {
                LineAction::Ignore
            } else {
                LineAction::Chat(text)
            }
        }
        // Alle Nicht-Chat-Invocations (Command/Shell/Note/Mention) werden über
        // die CommandRegistry ausgeführt; die Rohzeile wird durchgereicht.
        Ok(_) => LineAction::Command(line.to_owned()),
        Err(error) => LineAction::System(format!("Eingabe abgelehnt: {error}")),
    }
}

/// Zustand des interaktiven Chat-Renderers.
///
/// # Beschreibung
/// Hält den logischen Nachrichtenverlauf als typisierte `HistoryCell`-Zellen
/// (Zustands-Log für Tests und spätere Features), den handgerollten
/// Eingabepuffer, das aktive Theme sowie das optionale `/command`-Popup mit der
/// zugehörigen [`CommandRegistry`]. Der sichtbare Verlauf wird aus `cells` als
/// scrollbarer `Paragraph`-Widget im Fullscreen-Alternate-Screen gerendert.
/// `scroll_offset` steuert den internen Scroll (0 = am unteren Ende).
/// Das Theme wird einmalig beim Erzeugen via `style::detect_theme` bestimmt.
/// Spec-Quelle: SLICE 7 / SLICE 8.
///
/// Zusätzlich hält `ChatApp` die Operation-Adapter-Pipeline
/// ([`CommandAdapter`], [`SandboxSpec`], [`SessionId`]), über die abgeschickte
/// `/command`-Zeilen asynchron dispatcht werden ([`HarwEvent::Command`] →
/// [`crate::command_exec::execute_command`]). Die separate `command_registry`
/// bleibt unabhängig davon ausschließlich für die Popup-Autocomplete-Anzeige
/// zuständig.
///
/// # Nebenläufigkeit
/// Nicht `Send`; ausschließlich vom Renderer-Thread (im `current_thread`-Runtime)
/// verwendet. Kein interner Mutex.
///
/// # Beispiele
/// ```ignore
/// use harw_tui::app::{ChatApp, Role};
/// // adapters/sandbox/session_id werden real in `run_chat_tui` gebaut.
/// let mut app = ChatApp::new(Vec::new(), sandbox, session_id);
/// app.push_line(Role::User, "Frage");
/// assert_eq!(app.cells_len(), 1);
/// ```
pub struct ChatApp {
    /// Logischer Nachrichtenverlauf als typisierte Zellen, älteste zuerst.
    cells: Vec<Box<dyn HistoryCell>>,
    /// Editor-Zustand für die aktuelle Eingabezeile (Puffer, Cursor, History).
    input: InputEditor,
    /// Typing received during a turn is replayed after the turn finishes.
    deferred_input: std::collections::VecDeque<TuiEvent>,
    /// Scroll-Zustand der Chat-Ansicht (Offset, Auto-Follow).
    scroll: ChatScroll,
    /// Persistente Eingabe-Historie; überdauert die Sitzung als Datei.
    input_history: InputHistoryStore,
    /// Befehlsverzeichnis für das `/command`-Popup (Autocomplete-Anzeige).
    command_registry: CommandRegistry,
    /// Geöffnetes `/command`-Popup oder `None` wenn geschlossen.
    command_popup: Option<CommandPopup>,
    /// Aktives Terminal-Farbschema, einmalig beim Erzeugen erkannt.
    theme: style::Theme,
    /// Über die Session-Laufzeit aufsummierte Token-Nutzung (aus
    /// `SessionEvent::TurnCompleted`). Wird in der Statuszeile angezeigt.
    total_usage: TokenUsage,
    /// `/`-Command-Adapter, gebaut aus der `OperationRegistry`
    /// (`CommandAdapter::from_operation` pro registrierter Op). Treibt die
    /// echte Ausführung von `/command`-Zeilen (siehe [`Self::adapters`]).
    adapters: Vec<CommandAdapter>,
    /// Authority-Boundary für alle Operation-Dispatches dieser Chat-Session.
    sandbox: SandboxSpec,
    /// Stabile Session-ID, die in jeden [`harw_operations::OpContext`] dieser
    /// Session einfließt.
    session_id: SessionId,
    /// Optionales Memory-Backend, wird bei jedem `/command`-Dispatch als
    /// `Arc<dyn harw_memory::Memory>`-Service in die `ServiceMap` gelegt.
    /// `None` bedeutet: `/memory`-Ops liefern `NotAvailable`.
    memory: Option<std::sync::Arc<dyn harw_memory::Memory>>,
    /// Exact resolved configuration snapshot used to build this runtime.
    runtime_config: Option<Arc<harw_config::ResolvedConfig>>,
    /// Durable job store shared by job-related slash commands.
    job_store: Option<Arc<harw_session_store::JobStore>>,
    /// Erkannter Projekt-Root, der im Chat-Header angezeigt werden soll.
    /// Leer-String wenn kein Root ermittelt werden konnte (Fallback-Pfad).
    project_root: String,
    /// Langlebiger Session-Controller — hält `reasoning_effort`, `active_model`
    /// und `active_provider` über mehrere Turns hinweg. Wird in jeden
    /// `execute_command`-Aufruf als `SharedSessionController` eingetragen und
    /// zwischen Turns via `apply_pending_controller_state` auf die
    /// [`harw_core::AgentSession`] angewendet.
    ///
    /// Ein `Arc::clone` dieses Handles wird in jedem [`build_services`][crate::command_exec]-Aufruf
    /// in die `ServiceMap` gelegt, sodass `/effort`- und `/model`-Ops denselben
    /// Zustand sehen wie der Renderer.
    session_controller: Arc<TuiSessionController>,
    /// Letzte in `draw_viewport` berechnete Gesamtzeilenzahl der History
    /// (nach Wrapping bei aktueller Terminalbreite). Wird von `handle_key`
    /// gelesen, damit PageUp/PageDown/Home/End mit echten statt
    /// hartcodierten Werten arbeiten. `Cell` statt eines Feldes hinter `&mut`,
    /// weil `draw_viewport` nur `&ChatApp` erhält (Zeichnen soll den Zustand
    /// nicht mutieren müssen) — vor dem ersten Draw ist der Wert `0`.
    last_history_total_lines: Cell<u16>,
    /// Letzte in `draw_viewport` gemessene sichtbare Zeilenzahl des
    /// History-Bereichs (`history_area.height`). Fallback vor dem ersten
    /// Draw ist `20`, passend zum früheren hartcodierten Platzhalterwert.
    last_history_visible_rows: Cell<u16>,
    /// Kind-Spawn-Autorität dieser Session, falls konfiguriert. Wird als
    /// [`ChildTurnDriver`] an [`ApprovalDriver::drive_to_completion`] gereicht,
    /// damit ein `TurnOutcome::AwaitingChild` real weitergetrieben wird statt
    /// den Turn stehenzulassen. `None` beantwortet jeden Handoff mit einem
    /// Fehlerergebnis (der Turn endet trotzdem regulär).
    managed_spawner: Option<Arc<ManagedAgentSpawner>>,
    /// Zuletzt beobachteter Interaktionsmodus der Session. Wird an der
    /// Turn-Grenze aus [`AgentSession::mode`] nachgezogen bzw. aus
    /// [`TurnEvent::ModeChanged`] übernommen und in der Statuszeile angezeigt.
    active_mode: InteractionMode,
    /// Optionale Plan-/Ziel-Dienste der Composition-Root für [`PlanGraphCell`]
    /// und [`GoalCell`]; `None` degradiert beide zu Systemzeilen.
    plan_services: Option<TuiPlanServices>,
}

/// Handgeschriebene `Debug`-Implementierung, da [`CommandAdapter`] (enthält
/// `Arc<dyn Operation>`) selbst kein `Debug` ableitet. Zeigt statt der
/// Adapter-Liste nur ihre Länge; alle übrigen Felder werden unverändert
/// ausgegeben.
impl std::fmt::Debug for ChatApp {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ChatApp")
            .field("cells_len", &self.cells.len())
            .field("input", &self.input.text())
            .field("scroll_offset", &self.scroll.offset())
            .field("command_popup_open", &self.command_popup.is_some())
            .field("theme", &self.theme)
            .field("total_usage", &self.total_usage)
            .field("adapters_len", &self.adapters.len())
            .field("session_id", &self.session_id)
            .field("session_controller", &self.session_controller)
            .field("active_mode", &self.active_mode)
            .field("has_managed_spawner", &self.managed_spawner.is_some())
            .field("has_plan_services", &self.plan_services.is_some())
            .finish()
    }
}

impl ChatApp {
    /// Baut einen leeren Chat-Zustand mit der übergebenen Operation-Adapter-
    /// Pipeline.
    ///
    /// # Beschreibung
    /// Initialisiert den Nachrichtenverlauf und die Eingabe auf Leerwerte und
    /// befüllt `command_registry` mit [`CommandRegistry::built_in()`]
    /// (ausschließlich für die Popup-Autocomplete-Anzeige, SLICE 5). Das Popup
    /// ist initial geschlossen. Das Theme wird via `style::detect_theme` aus
    /// der Prozessumgebung bestimmt (SLICE 8). `adapters`, `sandbox` und
    /// `session_id` werden vom Aufrufer (`run_chat_tui`) übernommen und bei
    /// jedem `/command`-Dispatch verwendet.
    ///
    /// # Argumente
    /// - `adapters` (`Vec<CommandAdapter>`): Alle `/`-Command-Adapter, gebaut
    ///   aus der `OperationRegistry` (`CommandAdapter::from_operation` pro Op).
    /// - `sandbox` (`SandboxSpec`): Authority-Boundary für alle
    ///   Operation-Dispatches dieser Session.
    /// - `session_id` (`SessionId`): Stabile Session-ID für
    ///   [`harw_operations::OpContext`].
    ///
    /// # Rückgabe
    /// Neuer, leerer `ChatApp`-Zustand mit der übergebenen Command-Pipeline.
    ///
    /// # Beispiele
    /// ```ignore
    /// use harw_tui::app::ChatApp;
    /// // adapters/sandbox/session_id werden real in `run_chat_tui` gebaut.
    /// let app = ChatApp::new(Vec::new(), sandbox, session_id);
    /// assert!(app.input().is_empty());
    /// ```
    #[must_use]
    pub fn new(adapters: Vec<CommandAdapter>, sandbox: SandboxSpec, session_id: SessionId) -> Self {
        Self::with_memory(adapters, sandbox, session_id, None)
    }

    /// Wie [`Self::new`], zusätzlich mit Memory-Backend.
    ///
    /// # Beschreibung
    /// Registriert `memory` als `Arc<dyn harw_memory::Memory>`-Service in jedem
    /// via [`crate::command_exec::execute_command`] aufgebauten `OpContext`.
    /// So dispatcht `/memory list`/`stats`/`recall`/`record`/`maintain` real.
    #[must_use]
    pub fn with_memory(
        adapters: Vec<CommandAdapter>,
        sandbox: SandboxSpec,
        session_id: SessionId,
        memory: Option<std::sync::Arc<dyn harw_memory::Memory>>,
    ) -> Self {
        // Die persistente Historie wird beim Erzeugen in den Editor gespielt,
        // damit die Pfeiltasten sofort auch Eingaben früherer Sitzungen finden.
        let input_history = InputHistoryStore::open(INPUT_HISTORY_CAP);
        let mut input = InputEditor::new();
        for entry in input_history.load() {
            input.push_history(entry);
        }

        Self {
            cells: Vec::new(),
            input,
            deferred_input: std::collections::VecDeque::new(),
            scroll: ChatScroll::new(),
            input_history,
            command_registry: CommandRegistry::built_in(),
            command_popup: None,
            theme: style::detect_theme(),
            total_usage: TokenUsage::default(),
            adapters,
            sandbox,
            session_id,
            memory,
            runtime_config: None,
            job_store: None,
            project_root: String::new(),
            session_controller: Arc::new(TuiSessionController::new()),
            last_history_total_lines: Cell::new(0),
            last_history_visible_rows: Cell::new(20),
            managed_spawner: None,
            active_mode: InteractionMode::default(),
            plan_services: None,
        }
    }

    /// Reuses a runtime-owned controller for both slash commands and model
    /// tool operations in this chat session.
    #[must_use]
    pub(crate) fn with_session_controller(mut self, controller: Arc<TuiSessionController>) -> Self {
        self.session_controller = controller;
        self
    }

    #[must_use]
    pub fn with_runtime_config(mut self, config: Arc<harw_config::ResolvedConfig>) -> Self {
        self.runtime_config = Some(config);
        self
    }

    #[must_use]
    pub(crate) fn runtime_config(&self) -> Option<&Arc<harw_config::ResolvedConfig>> {
        self.runtime_config.as_ref()
    }

    #[must_use]
    pub fn with_job_store(mut self, store: Arc<harw_session_store::JobStore>) -> Self {
        self.job_store = Some(store);
        self
    }

    #[must_use]
    pub(crate) fn job_store(&self) -> Option<&Arc<harw_session_store::JobStore>> {
        self.job_store.as_ref()
    }

    /// Hinterlegt die Kind-Spawn-Autorität dieser Session.
    ///
    /// # Beschreibung
    /// Der Renderer braucht denselben [`ManagedAgentSpawner`], den auch der
    /// Model-Tool-Pfad benutzt: [`ApprovalDriver::drive_to_completion`] erhält
    /// ihn als [`ChildTurnDriver`], um ein `TurnOutcome::AwaitingChild`
    /// weiterzutreiben. `None` ist zulässig — Handoffs werden dann mit einem
    /// Fehlerergebnis beantwortet und der Turn endet trotzdem regulär.
    ///
    /// # Argumente
    /// - `spawner` (`Option<Arc<ManagedAgentSpawner>>`): geteilte Autorität.
    ///
    /// # Rückgabe
    /// `Self` für Builder-Verkettung.
    #[must_use]
    pub(crate) fn with_managed_spawner(
        mut self,
        spawner: Option<Arc<ManagedAgentSpawner>>,
    ) -> Self {
        self.managed_spawner = spawner;
        self
    }

    /// Gibt die Kind-Spawn-Autorität dieser Session zurück.
    ///
    /// # Rückgabe
    /// `Option<&Arc<ManagedAgentSpawner>>` — `None`, wenn keine Kindrolle
    /// konfiguriert ist.
    #[must_use]
    pub(crate) fn managed_spawner(&self) -> Option<&Arc<ManagedAgentSpawner>> {
        self.managed_spawner.as_ref()
    }

    /// Hinterlegt die von der Composition-Root gebauten Plan-/Ziel-Dienste.
    ///
    /// # Beschreibung
    /// Ohne diese Dienste bleibt der Renderer voll funktionsfähig; er stellt
    /// Plan-Updates dann als Systemzeile statt als [`PlanGraphCell`] dar und
    /// zeigt nach `/goal check` keinen [`GoalCell`]-Zielstand.
    ///
    /// # Argumente
    /// - `services` ([`TuiPlanServices`]): geteilte Store-Handles.
    ///
    /// # Rückgabe
    /// `Self` für Builder-Verkettung.
    #[must_use]
    pub fn with_plan_services(mut self, services: TuiPlanServices) -> Self {
        self.plan_services = Some(services);
        self
    }

    /// Gibt die hinterlegten Plan-/Ziel-Dienste zurück.
    ///
    /// # Rückgabe
    /// `Option<&TuiPlanServices>` — `None`, wenn keine konfiguriert sind.
    #[must_use]
    pub(crate) fn plan_services(&self) -> Option<&TuiPlanServices> {
        self.plan_services.as_ref()
    }

    /// Gibt den zuletzt beobachteten Interaktionsmodus zurück.
    ///
    /// # Rückgabe
    /// Der [`InteractionMode`], den die Statuszeile anzeigt.
    #[must_use]
    pub(crate) fn active_mode(&self) -> InteractionMode {
        self.active_mode
    }

    /// Übernimmt einen beobachteten Moduswechsel in den Anzeigezustand.
    ///
    /// # Argumente
    /// - `mode` ([`InteractionMode`]): der jetzt gültige Modus.
    pub(crate) fn set_active_mode(&mut self, mode: InteractionMode) {
        self.active_mode = mode;
    }

    /// Hängt eine geteilte, später noch veränderbare Zelle an den Verlauf an.
    ///
    /// # Beschreibung
    /// Der Aufrufer behält einen `Arc`-Klon und kann die Zelle dadurch nach dem
    /// Anhängen weiter fortschreiben — siehe [`SharedHistoryCell`]. Der Verlauf
    /// hält genau **eine** Instanz.
    ///
    /// # Argumente
    /// - `cell` (`Arc<Mutex<T>>`): die geteilte Zelle.
    pub(crate) fn push_shared_cell<T>(&mut self, cell: Arc<Mutex<T>>)
    where
        T: HistoryCell + 'static,
    {
        self.cells.push(Box::new(SharedHistoryCell { inner: cell }));
        self.scroll.on_new_content();
    }

    /// Stores the detected project root path for display in the chat header.
    ///
    /// # Description
    /// Called from `run_chat_tui` after `assemble_default_registry` succeeds.
    /// The string is built from `project.project_root.display()` — a
    /// platform-appropriate path string. An empty string means no root was
    /// detected (graceful degradation).
    ///
    /// # Arguments
    /// - `root` (`String`): display-form of the project root path.
    ///
    /// # Returns
    /// `Self` for builder-style chaining.
    #[must_use]
    pub fn with_project_root(mut self, root: String) -> Self {
        self.project_root = root;
        self
    }

    /// Returns the detected project root path string.
    ///
    /// # Returns
    /// The project root as set by [`with_project_root`][Self::with_project_root],
    /// or an empty string if none was detected.
    #[must_use]
    #[allow(dead_code)] // Consumed by header rendering in a future wave.
    pub(crate) fn project_root(&self) -> &str {
        &self.project_root
    }

    /// Gibt das optional registrierte Memory-Backend zurück.
    #[must_use]
    pub(crate) fn memory(&self) -> Option<&std::sync::Arc<dyn harw_memory::Memory>> {
        self.memory.as_ref()
    }

    /// Gibt eine Arc-Referenz auf den langlebigen Session-Controller zurück.
    ///
    /// # Beschreibung
    /// Wird von `execute_command` und `build_services` in `command_exec.rs`
    /// verwendet, um den langen Controller in die `ServiceMap` zu legen.
    ///
    /// # Rückgabe
    /// `&Arc<TuiSessionController>` — gemeinsamer Zustand über alle Turns.
    #[must_use]
    pub(crate) fn session_controller(&self) -> &Arc<TuiSessionController> {
        &self.session_controller
    }

    /// Gibt die zuletzt in `draw_viewport` berechnete Gesamtzeilenzahl der
    /// History zurück (`0` vor dem ersten Draw).
    ///
    /// # Rückgabe
    /// `u16` — Anzahl aller (gewrappten) History-Zeilen beim letzten Zeichnen.
    #[must_use]
    pub(crate) fn last_history_total_lines(&self) -> u16 {
        self.last_history_total_lines.get()
    }

    /// Nimmt eine abgesendete Eingabe in die Historie auf — im Speicher und auf
    /// Platte.
    ///
    /// # Beschreibung
    /// Der [`InputEditor`] hält die Historie für die Pfeiltasten-Navigation der
    /// laufenden Sitzung, der [`InputHistoryStore`] schreibt sie für die
    /// nächste fort. Beide Seiten werden hier gemeinsam bedient, damit keine
    /// Absende-Stelle die eine ohne die andere aktualisiert.
    ///
    /// # Argumente
    /// - `line` (`&str`): die abgesendete Eingabe im Originaltext.
    ///
    /// # Nebenläufigkeit
    /// Schreibt synchron auf Platte. Der Anhänge-Vorgang ist ein einzelner,
    /// kurzer Schreibzugriff und läuft im Event-Loop-Thread.
    pub(crate) fn remember_input(&mut self, line: &str) {
        self.input.push_history(line.to_owned());
        self.input_history.append(line);
    }

    /// Gibt die zuletzt in `draw_viewport` gemessene sichtbare Zeilenzahl des
    /// History-Bereichs zurück (`20` vor dem ersten Draw als sicherer
    /// Platzhalter-Fallback).
    ///
    /// # Rückgabe
    /// `u16` — Höhe des History-Bereichs (`history_area.height`) beim
    /// letzten Zeichnen.
    #[must_use]
    pub(crate) fn last_history_visible_rows(&self) -> u16 {
        self.last_history_visible_rows.get()
    }

    /// Wendet ausstehende Controller-Mutationen auf `session` an.
    ///
    /// # Beschreibung
    /// Delegiert an [`TuiSessionController::apply_to_session`]. Darf nur zwischen
    /// Turns aufgerufen werden (nie während ein Turn läuft). Ist ein billiger
    /// No-op, wenn seit dem letzten Apply keine Setter aufgerufen wurden.
    ///
    /// # Argumente
    /// - `session` (`&mut AgentSession`): Die lebende Session, auf die Mutationen
    ///   übertragen werden (z. B. `reasoning_effort`).
    ///
    /// # Rückgabe
    /// `true`, wenn die Session tatsächlich verändert wurde.
    ///
    /// # Spec
    /// harw-tui Design §session_controller — apply_pending_controller_state.
    /// AP W5-05 — `/mode` wirkt an der Turn-Grenze.
    pub(crate) fn apply_pending_controller_state(&mut self, session: &mut AgentSession) -> bool {
        let applied = self.session_controller.apply_to_session(session);
        // Immer nachziehen, nicht nur bei `applied`: der Anzeigezustand soll
        // auch dann stimmen, wenn der Modus beim Aufbau der Session gesetzt
        // wurde oder ein anderer Pfad ihn verändert hat.
        self.active_mode = session.mode();
        if applied {
            tracing::debug!(
                mode = session.mode().as_str(),
                "tui.session_controller.applied_at_turn_boundary"
            );
        }
        applied
    }

    /// Gibt die `/`-Command-Adapter-Pipeline zurück.
    ///
    /// # Rückgabe
    /// Alle in dieser Session verfügbaren [`CommandAdapter`]s, in
    /// Registrierungsreihenfolge.
    #[must_use]
    pub(crate) fn adapters(&self) -> &[CommandAdapter] {
        &self.adapters
    }

    /// Gibt die Authority-Boundary für Operation-Dispatches zurück.
    ///
    /// # Rückgabe
    /// Referenz auf die in dieser Session eingefrorene [`SandboxSpec`].
    #[must_use]
    pub(crate) fn sandbox(&self) -> &SandboxSpec {
        &self.sandbox
    }

    /// Gibt die stabile Session-ID zurück.
    ///
    /// # Rückgabe
    /// Referenz auf die [`SessionId`] dieser Chat-Session.
    #[must_use]
    pub(crate) fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Hängt eine typisierte Zelle für die gegebene Rolle und den Text an den Log an.
    ///
    /// # Beschreibung
    /// Wählt anhand der `role` die passende interne `HistoryCell`-Implementierung:
    /// - [`Role::User`] → `UserHistoryCell`
    /// - [`Role::Assistant`] → `AssistantHistoryCell`
    /// - [`Role::System`] → `PlainHistoryCell`
    ///
    /// Die erzeugte Zelle wird am Ende von `cells` angehängt.
    /// `scroll_offset` wird auf 0 zurückgesetzt (Auto-Scroll zum neusten Eintrag).
    ///
    /// # Argumente
    /// - `role` ([`Role`]): Rolle der Nachricht; bestimmt das Rendering-Format.
    /// - `text` (`impl Into<String>`): Nachrichtentext; Ownership wird übernommen.
    pub fn push_line(&mut self, role: Role, text: impl Into<String>) {
        let text = text.into();
        let cell: Box<dyn HistoryCell> = match role {
            Role::User => Box::new(UserHistoryCell { text }),
            Role::Assistant => Box::new(AssistantHistoryCell { source: text }),
            Role::System => Box::new(PlainHistoryCell {
                lines: vec![Line::from(text)],
            }),
        };
        self.cells.push(cell);
        // Neuer Inhalt: Chat-Scroll-State benachrichtigen (No-Op wenn User gescrollt hat).
        self.scroll.on_new_content();
    }

    /// Hängt eine `PlainHistoryCell` mit vorgerenderten Zeilen an den Log an.
    ///
    /// # Beschreibung
    /// Wird intern verwendet, um mehrzeilige System-/Assistenten-Ausgaben direkt
    /// als bereits berechnete [`Line`]-Vektoren anzuhängen. `scroll_offset` wird
    /// auf 0 zurückgesetzt (Auto-Scroll zum neusten Eintrag).
    ///
    /// # Argumente
    /// - `lines` (`Vec<Line<'static>>`): Vorgerenderte Zeilen der Zelle.
    fn push_lines(&mut self, lines: Vec<Line<'static>>) {
        self.cells.push(Box::new(PlainHistoryCell { lines }));
        self.scroll.on_new_content();
    }

    /// Hängt eine bereits gebaute, unveränderliche Zelle an den Verlauf an.
    ///
    /// # Argumente
    /// - `cell` (`Box<dyn HistoryCell>`): die anzuhängende Zelle.
    pub(crate) fn push_cell(&mut self, cell: Box<dyn HistoryCell>) {
        self.cells.push(cell);
        self.scroll.on_new_content();
    }

    /// Gibt die Anzahl der gespeicherten Verlauf-Zellen zurück.
    ///
    /// # Rückgabe
    /// Anzahl der `HistoryCell`-Einträge im internen Log (`cells.len()`).
    #[must_use]
    pub fn cells_len(&self) -> usize {
        self.cells.len()
    }

    /// Gibt den aktuellen Eingabepuffer zurück.
    ///
    /// # Rückgabe
    /// Der aktuell getippte, noch nicht abgeschickte Text.
    #[must_use]
    pub fn input(&self) -> &str {
        self.input.text()
    }

    /// Gibt die über die Session-Laufzeit aufsummierte Token-Nutzung zurück.
    ///
    /// # Beschreibung
    /// Wird bei jedem `SessionEvent::TurnCompleted` in `run_loop` via
    /// [`TokenUsage::add`] aktualisiert und in der Statuszeile ([`status_line`])
    /// als kompakter Suffix angezeigt.
    ///
    /// # Rückgabe
    /// Referenz auf die intern akkumulierte [`TokenUsage`].
    #[must_use]
    pub fn total_usage(&self) -> &TokenUsage {
        &self.total_usage
    }

    /// Synchronisiert den Popup-Zustand mit dem aktuellen Eingabepuffer.
    ///
    /// # Beschreibung
    /// Öffnet das `/command`-Popup wenn `input` mit `/` beginnt und das Popup
    /// noch nicht geöffnet ist. Schließt es wenn kein `/`-Präfix vorhanden ist.
    /// Aktualisiert den Filtertext (ohne führendes `/`) wenn das Popup bereits
    /// offen ist. (Spec-Abschnitt 2.8 / SLICE 5)
    fn sync_popup(&mut self) {
        // Das Popup ist Autocomplete für den COMMAND-NAMEN. Sobald ein
        // Leerzeichen getippt wird (Argument-Eingabe beginnt), wird es
        // geschlossen: sonst fängt der Popup-Zweig Ziffern in Argumenten
        // (z. B. Job-IDs `job-42`) als Auswahl-Index ab und die Zeichen
        // erreichen die Eingabe nie.
        if let Some(query) = self.input.text().strip_prefix('/') {
            if !query.contains(char::is_whitespace) {
                if let Some(popup) = self.command_popup.as_mut() {
                    popup.on_query_change(query);
                } else {
                    let mut popup = CommandPopup::new(&self.command_registry);
                    popup.on_query_change(query);
                    self.command_popup = Some(popup);
                }
                return;
            }
        }
        self.command_popup = None;
    }

    /// Gibt `true` zurück wenn das `/command`-Popup aktuell geöffnet ist.
    ///
    /// # Rückgabe
    /// `true` wenn ein aktives Popup vorhanden ist, sonst `false`.
    #[must_use]
    fn has_popup(&self) -> bool {
        self.command_popup.is_some()
    }
}

/// RAII-Guard, der Raw-Mode, Bracketed-Paste und Alternate-Screen bei Drop zurückstellt.
///
/// # Beschreibung
/// Aktiviert Raw-Mode, `EnterAlternateScreen` und `EnableBracketedPaste` und baut ein
/// ratatui-Fullscreen-Terminal. Der Alternate-Screen claimt die gesamte Terminalfläche
/// und stellt sie beim Drop sauber wieder her, sodass der normale Scrollback erhalten bleibt.
///
/// # Nebenläufigkeit
/// Nicht thread-sicher; ausschließlich vom Renderer-Thread verwendet.
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    /// Aktiviert Raw-Mode, Alternate-Screen und Bracketed-Paste und baut das Fullscreen-Terminal.
    ///
    /// # Beschreibung
    /// Reihenfolge beim Aufbau:
    /// 1. Raw-Mode aktivieren.
    /// 2. `EnterAlternateScreen` + `EnableBracketedPaste` senden.
    /// 3. Backend + Fullscreen-`Terminal` bauen.
    ///
    /// Bei jedem Fehler nach Schritt 1 wird bereits aktivierter Zustand best-effort zurückgestellt.
    ///
    /// # Fehler
    /// [`TuiError::Io`], wenn Terminal-Setup fehlschlägt.
    fn enter() -> Result<Self, TuiError> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = crossterm::execute!(
            stdout,
            crossterm::terminal::EnterAlternateScreen,
            EnableBracketedPaste,
            crossterm::event::EnableMouseCapture,
        ) {
            let _ = crossterm::execute!(
                stdout,
                crossterm::event::DisableMouseCapture,
                DisableBracketedPaste,
                crossterm::terminal::LeaveAlternateScreen,
            );
            let _ = disable_raw_mode();
            return Err(TuiError::from(error));
        }
        let backend = CrosstermBackend::new(stdout);
        match Terminal::new(backend) {
            Ok(terminal) => Ok(Self { terminal }),
            Err(error) => {
                let _ = crossterm::execute!(
                    io::stdout(),
                    crossterm::event::DisableMouseCapture,
                    DisableBracketedPaste,
                    crossterm::terminal::LeaveAlternateScreen,
                );
                let _ = disable_raw_mode();
                Err(TuiError::from(error))
            }
        }
    }

    /// Liefert eine veränderbare Referenz auf das zugrunde liegende Terminal.
    fn terminal(&mut self) -> &mut Terminal<CrosstermBackend<Stdout>> {
        &mut self.terminal
    }
}

/// Stellt Terminal-Raw-Mode, Bracketed-Paste und Alternate-Screen beim Drop zurück (Best-effort).
impl Drop for TerminalGuard {
    /// Deaktiviert Bracketed-Paste, verlässt den Alternate-Screen, deaktiviert Raw-Mode
    /// und blendet den Cursor wieder ein.
    ///
    /// # Beschreibung
    /// Läuft in umgekehrter Reihenfolge zur Aktivierung. Alle Aufrufe sind best-effort —
    /// Fehler werden verworfen, damit kein Panic im Drop ausgelöst wird.
    fn drop(&mut self) {
        let _ = crossterm::execute!(
            self.terminal.backend_mut(),
            crossterm::event::DisableMouseCapture,
            DisableBracketedPaste,
            crossterm::terminal::LeaveAlternateScreen,
        );
        let _ = disable_raw_mode();
        let _ = self.terminal.show_cursor();
    }
}

fn session_id_for_canonical_project_root(project_root: &std::path::Path) -> SessionId {
    // Stable FNV-1a projection keeps filesystem paths out of session IDs while
    // preserving deterministic restart identity for the same canonical root.
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in project_root.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    SessionId::from_str(format!("local-tui-{hash:016x}"))
}

/// Erzeugt eine frische, projektbezogene Session-ID.
///
/// # Beschreibung
/// Der Projekt-Hash aus [`session_id_for_canonical_project_root`] bildet den
/// Stamm, damit `/resume` die Sitzungen eines Projekts weiterhin erkennt. Der
/// angehängte Startzeitpunkt in Nanosekunden macht jeden Start unterscheidbar.
/// Fällt die Systemuhr hinter die Epoche zurück, wird `0` verwendet — dann
/// kollidieren zwei Starts derselben Nanosekunde, was den Verlauf zusammenführt
/// statt etwas zu verlieren.
///
/// # Argumente
/// - `project_root` (`&Path`): kanonischer Projekt-Root.
///
/// # Rückgabe
/// Eine [`SessionId`] der Form `local-tui-<projekt-hash>-<nanos>`.
fn new_session_id(project_root: &std::path::Path) -> SessionId {
    let project = session_id_for_canonical_project_root(project_root);
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    SessionId::from_str(format!("{}-{started:x}", project.as_str()))
}

/// Wählt die Session-ID für diesen Start.
///
/// # Beschreibung
/// Mit `--resume` (oder `/resume`) reicht der Aufrufer die gewünschte ID
/// herein, und genau diese Sitzung wird fortgesetzt. Ohne eine solche Auswahl
/// beginnt jeder Start eine **neue** Sitzung: eine feste, aus dem Projektpfad
/// abgeleitete ID würde den Verlauf des letzten Laufs stillschweigend
/// weiterführen, obwohl niemand darum gebeten hat.
///
/// # Argumente
/// - `project_root` (`&Path`): kanonischer Projekt-Root.
/// - `existing_session_id` (`Option<SessionId>`): ausdrücklich gewählte Sitzung.
///
/// # Rückgabe
/// Die zu verwendende [`SessionId`].
fn selected_session_id(
    project_root: &std::path::Path,
    existing_session_id: Option<SessionId>,
) -> SessionId {
    existing_session_id.unwrap_or_else(|| new_session_id(project_root))
}

fn resume_request(raw: &str) -> Option<TuiRunOutcome> {
    let mut words = raw.split_whitespace();
    if words.next()? != "/resume" {
        return None;
    }
    match words.next() {
        None => Some(TuiRunOutcome::Resume { selector: None }),
        Some(selector) if words.next().is_none() => Some(TuiRunOutcome::Resume {
            selector: Some(selector.to_owned()),
        }),
        Some(_) => None,
    }
}

fn visible_message_text(content: &[ContentPart]) -> String {
    let mut visible = String::new();
    for part in content {
        match part {
            ContentPart::Text { text } => visible.push_str(text),
            ContentPart::ImageUrl { .. } => visible.push_str(NON_TEXT_CONTENT_PLACEHOLDER),
        }
    }
    visible
}

fn hydrate_visible_history(app: &mut ChatApp, history: &ConversationHistory) {
    for item in history.items() {
        match item {
            TurnItem::UserMessage(message) => {
                app.push_line(Role::User, visible_message_text(&message.content));
            }
            TurnItem::AssistantMessage(message) => {
                app.push_line(Role::Assistant, visible_message_text(&message.content));
            }
            TurnItem::ToolCall(_) => app.push_line(Role::System, "[tool call]"),
            TurnItem::ToolResult(_) => app.push_line(Role::System, "[tool result]"),
            TurnItem::Reasoning(_) => app.push_line(Role::System, "[reasoning]"),
            TurnItem::Error(_) => app.push_line(Role::System, "[error]"),
        }
    }
}

fn install_loaded_history(
    session: &mut AgentSession,
    app: &mut ChatApp,
    history: ConversationHistory,
) {
    hydrate_visible_history(app, &history);
    *session.history_mut() = history;
}

fn sandbox_for_project(
    sandbox: &SandboxSpec,
    project_root: &std::path::Path,
) -> Result<SandboxSpec, String> {
    let tenant = TenantId::from_str("local-tui");
    let workspace = WorkspaceId::from_str("project");
    let registry = WorkspaceRegistry::build(
        project_root,
        [WorkspaceRegistration {
            tenant: tenant.clone(),
            workspace: workspace.clone(),
            root: project_root.to_path_buf(),
        }],
    )
    .map_err(|error| format!("could not bind project workspace: {error}"))?;
    let binding = registry
        .resolve(&tenant, &workspace)
        .map_err(|error| format!("could not resolve project workspace: {error}"))?;
    Ok(SandboxSpec::from_resolved(
        binding,
        sandbox.permissions().clone(),
    ))
}

/// Generates a fresh root trace for the local TUI session.
///
/// This call site is a root: a local TUI session has no parent whose trace it
/// could inherit, so the `trace_id` that ties together the session's work
/// originates here. Mirrors `harw-core`'s `new_span_id` random source
/// (`harw-core/src/child_controller.rs`) instead of inventing a second one:
/// `uuid::Uuid::new_v4` supplies the full 32 hex characters for `trace_id`, a
/// second, independent draw supplies the first 16 for `span_id`. Both are
/// already valid lowercase hex of the required length by construction, so
/// [`TraceContext::new`] rejecting them is unreachable in practice — but
/// [`trusted_tui_spawn_context`] is not fallible, so a rejection is logged
/// and degrades to no trace rather than panicking.
fn new_tui_root_trace() -> Option<TraceContext> {
    let trace_id = Uuid::new_v4().simple().to_string();
    let span_id = Uuid::new_v4().simple().to_string()[..16].to_owned();
    match TraceContext::new(trace_id, span_id) {
        Ok(trace) => Some(trace),
        Err(error) => {
            tracing::warn!(
                error = %error,
                "could not build root trace context for the local TUI session"
            );
            None
        }
    }
}

/// Baut die Wurzel-Kontext-Decke der lokalen TUI-Sitzung.
///
/// # Description
/// Diese Sitzung hat keinen Elternteil, dessen bereits geschnittene Decke
/// sie erben könnte — die Decke entsteht hier einmal, im selben Sinn wie
/// [`new_tui_root_trace`] den Wurzel-Trace einmal erzeugt (siehe
/// `harw_core::SpawnContext::ceiling` für die Vererbungsregel selbst).
/// `max_trust` ist [`TrustClass::Instruction`] (der höchste Rang), damit
/// kein Fragment allein wegen seiner Vertrauensklasse abgelehnt wird — ein
/// lokaler TUI-Operator mit voller Sandbox-Autorität ist nicht weniger
/// vertrauenswürdig als das restriktivste Kontextfragment.
///
/// Nur `harw_core::HISTORY_TAIL_SECTION` ist über die Crate-Grenzen hinweg
/// als öffentliche Konstante erreichbar; Sektionsnamen anderer
/// Kontext-Provider sind crate-privat. Die Wurzel-Decke umfasst deshalb
/// vorerst nur `history.tail` — eine bewusste, dokumentierte Lücke.
fn local_tui_root_context_ceiling() -> harw_context::ContextCeiling {
    let history_tail = harw_context::SectionName::try_new(harw_core::HISTORY_TAIL_SECTION)
        .expect("HISTORY_TAIL_SECTION is a valid section name by construction");
    harw_context::ContextCeiling {
        sections: [history_tail].into_iter().collect(),
        max_trust: harw_context::TrustClass::Instruction,
        budget: harw_context::ContextBudgetSpec {
            total: harw_lens_types::BudgetSpec { total: 1_000_000 },
            per_section: std::collections::BTreeMap::new(),
        },
    }
}

/// Builds the immutable child-spawn authority for the local TUI session.
///
/// The sandbox is supplied by the trusted runtime composition root after
/// project binding. Model output and tool-call arguments never participate in
/// constructing this context.
fn trusted_tui_spawn_context(sandbox: &SandboxSpec) -> SpawnContext {
    SpawnContext {
        sandbox: sandbox.clone(),
        suggestions: None,
        capability_snapshot: None,
        approval_actor: Some(ApprovalActor::Operator {
            id: "local-tui".to_owned(),
        }),
        organizational_role: AgentRoleId::RootOrchestrator,
        // Root: no parent exists whose trace could be inherited — see
        // `new_tui_root_trace`.
        trace: new_tui_root_trace(),
        // Root: no parent exists whose already-cut ceiling could be
        // inherited, so the ceiling is created here, once — see
        // `local_tui_root_context_ceiling`.
        ceiling: Some(local_tui_root_context_ceiling()),
    }
}

fn assemble_tui_registry(
    cwd: std::path::PathBuf,
) -> Result<harw_registry_defaults::AssembledRegistry, TuiError> {
    assemble_default_registry(cwd)
        .map_err(|error| TuiError::Core(format!("could not assemble default registry: {error}")))
}

/// Hängt einen weiteren [`ApprovalHandler`] an eine bereits gebaute Registry an.
///
/// # Beschreibung
/// AP W5-03, Bedingung 2: **derselbe** `Arc<TuiApprovalHandler>` muss in der
/// Klont einen [`TuiApprovalHandler`]-Zeiger als Trait-Objekt.
///
/// # Beschreibung
/// Die Umwandlung `Arc<TuiApprovalHandler>` → `Arc<dyn ApprovalHandler>` ist eine
/// Unsized-Coercion und braucht eine eigene Stelle, an der sie stattfindet.
/// `let x: Arc<dyn ApprovalHandler> = Arc::clone(&handler)` **compiliert nicht**:
/// bei der UFCS-Form `Arc::clone` wird der Typparameter aus dem *erwarteten* Typ
/// inferiert, Rust wählt also `T = dyn ApprovalHandler` und verlangt bereits ein
/// `&Arc<dyn ApprovalHandler>` als Argument — die Coercion käme zu spät.
///
/// Bei der Methodensyntax `handler.clone()` steht `T` dagegen durch den Receiver
/// fest; die Coercion geschieht beim Zurückgeben. Deshalb genau diese Form.
///
/// # Argumente
/// - `handler` (`&Arc<TuiApprovalHandler>`): der geteilte Handler.
///
/// # Rückgabe
/// Derselbe Handler als `Arc<dyn ApprovalHandler>` — **kein** zweiter Handler,
/// nur ein zweiter Zeiger. Das ist Bedingung 2 aus `approval.rs`: Registry und
/// Treiber müssen denselben Handler sehen, sonst fände der Treiber die
/// Rückkanäle nicht und löste jede Frage als Ablehnung auf.
fn as_dyn_approval_handler(handler: &Arc<TuiApprovalHandler>) -> Arc<dyn ApprovalHandler> {
    handler.clone()
}

/// [`ExtensionRegistry`] und im [`ApprovalDriver`] liegen. `ExtensionRegistry`
/// bietet nur für Tool-Provider ein nachträgliches `add_*`; für Freigabe-Handler
/// existiert ausschließlich der Builder. Diese Funktion baut die Registry
/// deshalb aus ihren eigenen Gettern neu auf — jeder Beitrag ist ein `Arc`, es
/// wird also nichts dupliziert, nur der Zeiger umgehängt — und hängt `handler`
/// **hinter** die bestehenden Handler.
///
/// Die Reihenfolge ist bedeutsam: `harw_core::turn_loop::check_approval` bricht
/// beim ersten Nicht-`Allow` ab. Die `DefaultApprovalPolicy` aus
/// `harw-registry-defaults` bleibt damit die entscheidende Politik; der
/// TUI-Handler läuft im Scope [`crate::approval::ApprovalScope::Deferred`] und
/// dient nur als Frage-/Antwortkanal, den der [`ApprovalDriver`] anhand des vom
/// Kern festgehaltenen Pausezustands bedient.
///
/// # Argumente
/// - `registry` ([`ExtensionRegistry`]): die zusammengebaute Registry; Besitz
///   geht über.
/// - `handler` (`Arc<dyn ApprovalHandler>`): der zusätzlich zu registrierende
///   Handler.
///
/// # Rückgabe
/// Eine neue [`ExtensionRegistry`] mit identischen Beiträgen plus `handler`.
///
/// # Errors
/// [`ContextProviderRegistrationError`]: einer der aus `registry`
/// übernommenen Kontextanbieter deklariert einen leeren oder bereits
/// vergebenen Namensraum. Da `registry` bereits eine gültig zusammengesetzte
/// Registry ist, ist dieser Fehler hier praktisch unerreichbar — die Prüfung
/// wird trotzdem nicht mit `expect()` verschluckt.
fn registry_with_approval_handler(
    registry: ExtensionRegistry,
    handler: Arc<dyn ApprovalHandler>,
) -> Result<ExtensionRegistry, ContextProviderRegistrationError> {
    let mut builder = ExtensionRegistry::builder();
    for provider in registry.tool_providers() {
        builder = builder.tool_provider(Arc::clone(provider));
    }
    for provider in registry.context_providers() {
        builder = builder.context_provider(Arc::clone(provider))?;
    }
    for provider in registry.instructions_providers() {
        builder = builder.instructions_provider(Arc::clone(provider));
    }
    for existing in registry.approval_handlers() {
        builder = builder.approval_handler(Arc::clone(existing));
    }
    builder = builder.approval_handler(handler);
    for observer in registry.turn_observers() {
        builder = builder.turn_observer(Arc::clone(observer));
    }
    if let Some(spawner) = registry.spawner() {
        builder = builder.spawner(Arc::clone(spawner));
    }
    Ok(builder.build())
}

/// Hängt die Beiträge der Composition-Root an eine bestehende Registry.
///
/// # Beschreibung
/// Gegenstück zu [`registry_with_approval_handler`] für die Beiträge, die
/// `harw-tui` nicht selbst bauen kann: der Ziel-Kontext-Provider stammt aus
/// `harw-plan-bridge`, die zusätzliche Freigabe-Politik aus `harw-core` — beide
/// kennt nur die Composition-Root, die sie als Trait-Objekte durchreicht.
///
/// Auch hier wird **angehängt**, nicht ersetzt: die vorhandenen Beitragenden
/// bleiben und behalten ihre Reihenfolge. Für Freigabe-Politiken ist das die
/// sicherheitsrelevante Eigenschaft — `harw_core::turn_loop::check_approval`
/// nimmt die erste Nicht-`Allow`-Entscheidung, eine angehängte Politik kann
/// also nur zusätzlich einschränken, nie lockern.
///
/// # Argumente
/// - `registry` ([`ExtensionRegistry`]): die bereits zusammengebaute Registry;
///   Besitz geht über.
/// - `services` (`&TuiPlanServices`): das Bündel der Composition-Root; nur die
///   Felder `context_providers` und `approval_handlers` werden gelesen.
///
/// # Rückgabe
/// Eine neue [`ExtensionRegistry`] mit allen bisherigen plus den übergebenen
/// Beiträgen. Es wird nichts dupliziert — jeder Beitrag ist ein `Arc`, nur der
/// Zeiger wird umgehängt.
///
/// # Errors
/// [`ContextProviderRegistrationError`]: ein Kontextanbieter aus `registry`
/// oder aus `services.context_providers` deklariert einen leeren oder
/// bereits vergebenen Namensraum — insbesondere wenn ein von der
/// Composition-Root beigetragener Anbieter denselben Namensraum wie ein
/// bereits registrierter beansprucht.
fn registry_with_plan_contributions(
    registry: ExtensionRegistry,
    services: &TuiPlanServices,
) -> Result<ExtensionRegistry, ContextProviderRegistrationError> {
    if services.context_providers.is_empty() && services.approval_handlers.is_empty() {
        return Ok(registry);
    }
    let mut builder = ExtensionRegistry::builder();
    for provider in registry.tool_providers() {
        builder = builder.tool_provider(Arc::clone(provider));
    }
    for provider in registry.context_providers() {
        builder = builder.context_provider(Arc::clone(provider))?;
    }
    for provider in &services.context_providers {
        builder = builder.context_provider(Arc::clone(provider))?;
    }
    for provider in registry.instructions_providers() {
        builder = builder.instructions_provider(Arc::clone(provider));
    }
    for existing in registry.approval_handlers() {
        builder = builder.approval_handler(Arc::clone(existing));
    }
    for handler in &services.approval_handlers {
        builder = builder.approval_handler(Arc::clone(handler));
    }
    for observer in registry.turn_observers() {
        builder = builder.turn_observer(Arc::clone(observer));
    }
    if let Some(spawner) = registry.spawner() {
        builder = builder.spawner(Arc::clone(spawner));
    }
    tracing::info!(
        context_providers = services.context_providers.len(),
        approval_handlers = services.approval_handlers.len(),
        "tui.registry.plan_contributions_appended"
    );
    Ok(builder.build())
}

/// Übersetzt einen [`ApprovalDriverError`] in einen [`TuiError`], ohne die
/// Ursache zu verlieren.
///
/// # Beschreibung
/// AP W5-03, Bedingung 7. [`TuiError::Core`] trägt Text, kein `source`. Damit
/// die verpackte [`CoreError`]-Ursache nicht verschwindet, wird die gesamte
/// `source()`-Kette angehängt — [`ApprovalDriverError`] rendert seine direkte
/// Ursache bereits in `Display`, tiefere Glieder kämen sonst nicht mit.
///
/// # Argumente
/// - `error` ([`ApprovalDriverError`]): der Fehler des Freigabetreibers.
///
/// # Rückgabe
/// [`TuiError::Core`] mit vollständiger Ursachenkette.
fn tui_error_from_approval_driver(error: ApprovalDriverError) -> TuiError {
    let mut message = error.to_string();
    let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(&error);
    while let Some(cause) = source {
        let rendered = cause.to_string();
        if !message.contains(&rendered) {
            message.push_str(&format!(": {rendered}"));
        }
        source = cause.source();
    }
    tracing::error!(error = %message, "tui.approval.driver_failed");
    TuiError::Core(message)
}

/// Rejects a session when prompt discovery and tool authority resolve to
/// different project roots.
///
/// The registry contributes project context and instruction documents to the
/// model prompt, while the sandbox constrains the tools that prompt can use.
/// They must describe the same canonical project root; continuing otherwise
/// could expose instructions from one project while granting tools access to
/// another.
fn ensure_tui_context_roots_align(
    prompt_project_root: &std::path::Path,
    sandbox_project_root: &std::path::Path,
) -> Result<(), TuiError> {
    if prompt_project_root == sandbox_project_root {
        return Ok(());
    }

    Err(TuiError::Core(format!(
        "refusing to start TUI with mismatched prompt discovery root `{}` and tool sandbox root `{}`",
        prompt_project_root.display(),
        sandbox_project_root.display(),
    )))
}

/// Vertrauenswürdiger, **rollenspezifisch eingeschränkter** Registry-Bauer für
/// Kinder der lokalen TUI.
///
/// # Beschreibung
/// AP W5-04. Vorher bekam jedes Kind die **volle** TUI-Registry
/// ([`assemble_default_registry`], Profil `Full`) — also `fs.write` und
/// `shell.exec`, unabhängig von seiner Rolle. Diese Fassung fragt stattdessen
/// [`profile_for_role`] und baut die Registry über
/// [`harw_registry_defaults::profile::assemble_registry`] mit genau dem Profil,
/// das die Rolle vorsieht. Die Zuordnung Rolle → Profil existiert damit
/// **einmal** im Workspace; ein eigener `match` über Rollennamen würde von ihr
/// abdriften.
///
/// Eine Instanz bedient **alle** registrierten Rollen (Muster aus
/// `harw-cli/src/chat.rs::OneShotChildRegistryFactory`). Der frühere
/// Ein-Rolle-pro-Factory-Zuschnitt entfällt: [`ManagedAgentSpawner`] ruft die
/// Factory ohnehin nur für bereits admittierte, registrierte Rollen auf, und
/// eine gemeinsame Instanz senkt die eingebauten Agentendefinitionen genau
/// einmal statt einmal je Rolle.
///
/// # Abweichung von der Referenz
/// Zusätzlich zum One-shot-Muster prüft [`Self::build_registry`] weiterhin über
/// [`ensure_tui_context_roots_align`], dass die Projekterkennung des Kindes
/// denselben Wurzelpfad liefert wie die des Elternteils. Diese Prüfung ist
/// TUI-spezifisch (der interaktive Pfad lebt länger als ein One-shot-Turn, ein
/// Verzeichniswechsel unter laufender Session ist dort real möglich) und wird
/// deshalb bewusst beibehalten.
///
/// # Sicherheitsregel
/// Diese Schicht ist **eine von dreien**. Sie verengt die Werkzeug-Sichtbarkeit
/// und weicht weder die IR-Aktivierung (siehe [`Self::executable_agent_ir`])
/// noch die Sandbox-Reduktion des Kerns auf. Eine unbekannte Rolle erhält das
/// Default-Profil ([`harw_registry_defaults::profile::RegistryProfile::Full`]) —
/// genau den Satz des Elternteils, nie mehr.
struct TuiChildRegistryFactory {
    /// Startpunkt der Projekterkennung für jede Kind-Registry.
    discovery_cwd: std::path::PathBuf,
    /// Der Projekt-Root des Elternteils; jede Kind-Registry muss ihn treffen.
    project_root: std::path::PathBuf,
    /// Der Modellanbieter, den jedes Kind für seinen Turn wiederverwendet.
    model: Arc<dyn ModelProvider>,
    /// Die eingebauten Rollen, bereits zu [`ExecutableAgentIr`] gesenkt,
    /// geschlüsselt nach Rollennamen.
    builtin_definitions: HashMap<String, ExecutableAgentIr>,
}

impl TuiChildRegistryFactory {
    /// Senkt die eingebauten Agentendefinitionen einmalig und hält sie für die
    /// Lebensdauer des Spawners.
    ///
    /// # Argumente
    /// - `discovery_cwd` (`&std::path::Path`): Startpunkt der Projekterkennung.
    /// - `project_root` (`&std::path::Path`): Projekt-Root des Elternteils.
    /// - `model` (`Arc<dyn ModelProvider>`): der von jedem Kind wiederverwendete
    ///   Modellanbieter (derselbe wie im Eltern-Turn).
    ///
    /// # Rückgabe
    /// `Ok(Self)` mit allen eingebauten Rollen aus
    /// [`role_names::ALL`] gesenkt.
    ///
    /// # Fehler
    /// [`TuiError::Core`], wenn eine eingebettete Agentendefinition nicht senkt.
    /// Praktisch unerreichbar: die TOML-Quellen sind zur Bauzeit eingebettet und
    /// werden von `harw-registry-defaults` selbst getestet.
    ///
    /// # Nebenläufigkeit
    /// Reine Konstruktion; kein geteilter Zustand.
    fn new(
        discovery_cwd: &std::path::Path,
        project_root: &std::path::Path,
        model: Arc<dyn ModelProvider>,
    ) -> Result<Self, TuiError> {
        // `existing` bleibt leer: die TUI kennt zu einem konfigurierten
        // Rollennamen keine gesenkte IR, die eine eingebaute überschreiben
        // dürfte. Ein leeres Set senkt daher genau die eingebauten Rollen.
        let builtin_definitions =
            harw_registry_defaults::embedded_agents::builtin_agent_definitions(&HashMap::new())
                .map_err(|error| {
                    TuiError::Core(format!(
                        "could not lower builtin agent definitions for TUI child spawning: {error}"
                    ))
                })?;
        Ok(Self {
            discovery_cwd: discovery_cwd.to_path_buf(),
            project_root: project_root.to_path_buf(),
            model,
            builtin_definitions,
        })
    }
}

impl ChildRegistryFactory for TuiChildRegistryFactory {
    /// Baut die Registry eines Kindes nach dem Profil seiner Rolle.
    ///
    /// # Argumente
    /// - `role` (`&str`): der registrierte Rollenname.
    /// - `_input` (`&SpawnInput`): ungenutzt — die Registry hängt allein an
    ///   `role`, niemals an Modell-JSON.
    /// - `_suggestions` (`Option<&harw_catalog::AgentSuggestions>`): ungenutzt;
    ///   Vorschläge werden nie zu registrierten Werkzeugen.
    ///
    /// # Rückgabe
    /// `Ok(ExtensionRegistry)` mit genau den Tool-Providern des Profils aus
    /// [`profile_for_role`].
    ///
    /// # Fehler
    /// [`AgentSpawnError`], wenn die Projekterkennung fehlschlägt oder der
    /// erkannte Projekt-Root nicht dem des Elternteils entspricht.
    ///
    /// # Nebenläufigkeit
    /// Zustandslos außer Lesezugriff auf `self`; aus mehreren Threads aufrufbar.
    fn build_registry(
        &self,
        role: &str,
        _input: &SpawnInput,
        _suggestions: Option<&harw_catalog::AgentSuggestions>,
    ) -> Result<ExtensionRegistry, AgentSpawnError> {
        let profile = profile_for_role(role).unwrap_or_default();
        let overrides = IdentityOverrides {
            agent_name: Some(role.to_owned()),
            ..IdentityOverrides::default()
        };
        let assembled = harw_registry_defaults::profile::assemble_registry(
            profile,
            self.discovery_cwd.clone(),
            overrides,
        )
        .map_err(|error| AgentSpawnError {
            message: format!("could not assemble child registry for role '{role}': {error}"),
        })?;
        ensure_tui_context_roots_align(&assembled.project.project_root, &self.project_root)
            .map_err(|error| AgentSpawnError {
                message: format!("could not validate child registry for role '{role}': {error}"),
            })?;
        tracing::debug!(
            role,
            profile = ?profile,
            read_only = profile.is_read_only(),
            "tui.child_registry.assembled"
        );
        Ok(assembled.registry)
    }

    /// Liefert den für den Eltern-Turn gewählten Modellanbieter.
    ///
    /// # Argumente
    /// - `_role` (`&str`): ungenutzt — [`ManagedAgentSpawner`] fragt nur für
    ///   bereits admittierte, registrierte Rollen.
    ///
    /// # Rückgabe
    /// `Ok(Arc<dyn ModelProvider>)` — immer derselbe geteilte Zeiger.
    ///
    /// # Fehler
    /// Nie: der Anbieter ist zur Konstruktionszeit bereits aufgelöst.
    fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
        Ok(Arc::clone(&self.model))
    }

    /// Liefert die eingebaute Agent-IR einer der Rollen aus [`role_names::ALL`].
    ///
    /// # Argumente
    /// - `role` (`&str`): der registrierte Rollenname.
    ///
    /// # Rückgabe
    /// `Some(&ExecutableAgentIr)` für eine eingebaute Rolle, sonst `None`. Der
    /// Kern zieht daraus Tool-Aktivierung, Budget und Pause-Sperre des Kindes —
    /// diese Schicht schwächt das nicht ab, sondern versorgt es.
    fn executable_agent_ir(&self, role: &str) -> Option<&ExecutableAgentIr> {
        self.builtin_definitions.get(role)
    }
}

fn configured_child_organizational_role(role: &str) -> Result<AgentRoleId, TuiError> {
    match role {
        "worker" => Ok(AgentRoleId::Worker),
        "child-orchestrator" => Ok(AgentRoleId::ChildOrchestrator),
        unsupported => Err(TuiError::Core(format!(
            "configured TUI child role '{unsupported}' is not permitted by the root spawn policy"
        ))),
    }
}

/// Leitet die Kind-Grenzwerte aus dem Laufzeitprofil des gewählten Modells ab.
///
/// # Beschreibung
/// AP W5-04. `harw-core` hängt bewusst nicht von `harw-model-catalog` ab und
/// bietet deshalb kein `limits_from_runtime_profile`; die dokumentierte
/// crate-lokale Form ist [`ChildLimits::with_max_children`], die ein
/// Consumer-Crate „mit `profile.max_child_fanout as usize`" aufruft. `harw-tui`
/// kennt beide Crates und tut genau das.
///
/// Die Ableitung ist **monoton reduzierend**: [`ChildLimits::with_max_children`]
/// klammert gegen [`ChildLimits::conservative`] und kann die konservative Grenze
/// nur senken. Ein Profil mit `max_child_fanout == 0`
/// ([`harw_model_catalog::runtime::DelegationPolicy::Forbidden`]) wird von
/// dieser Kernfunktion auf `1` angehoben — das ist ihr dokumentiertes Verhalten
/// („ein Deckel von 0 wäre keine Grenze, sondern ein Ausfall") und bleibt
/// bewusst unangetastet, damit es im Workspace nur eine Auslegung gibt.
///
/// # Argumente
/// - `runtime_config` (`&harw_config::ResolvedConfig`): die aufgelöste
///   Konfiguration; `harness.default_model` benennt das Modell.
///
/// # Rückgabe
/// [`ChildLimits`] mit gedeckeltem `max_active_children_per_parent`; ohne
/// gesetztes Default-Modell [`ChildLimits::conservative`].
fn tui_child_limits(runtime_config: &harw_config::ResolvedConfig) -> ChildLimits {
    let Some(model) = runtime_config.harness.default_model.as_deref() else {
        return ChildLimits::conservative();
    };
    let profile = harw_model_catalog::profile_for(model);
    let limits = ChildLimits::with_max_children(usize::from(profile.max_child_fanout));
    tracing::debug!(
        model,
        max_child_fanout = profile.max_child_fanout,
        max_active_children_per_parent = limits.max_active_children_per_parent,
        "tui.child_limits.derived_from_runtime_profile"
    );
    limits
}

/// Constructs the local TUI's child-spawn authority without mirroring its
/// root [`AgentSession`] in the child manager.
///
/// Registriert zwei Rollenmengen über **eine** gemeinsame
/// [`TuiChildRegistryFactory`]:
/// 1. die eingebauten Rollen aus [`role_names::ALL`] — jede mit dem Profil aus
///    [`profile_for_role`] und der eingebauten Agent-IR, organisatorisch
///    [`AgentRoleId::Worker`] (genau wie im One-shot-Pfad),
/// 2. die in `[agents]` konfigurierten Rollen mit ihrem konfigurierten
///    organisatorischen Rang.
///
/// Rollennamen der ersten Menge stammen ausschließlich aus [`role_names`], nie
/// aus Literalen. Konfigurierte Rollen werden **nach** den eingebauten
/// registriert: trägt eine Konfiguration denselben Namen, gewinnt sie.
///
/// Ein leeres `[agents]`-Set bleibt ein Fehler: der bisherige fail-closed
/// Zuschnitt („kein Kind-Spawnen ohne konfigurierte Kindrollen") wird durch die
/// eingebauten Rollen nicht aufgeweicht.
// clippy::too_many_arguments: die acht Parameter sind fachlich unabhängig
// (Session-Identität, Spawn-Kontext, Reasoning-Effort, globale Config, zwei
// getrennte Dateisystem-Wurzeln, Model-Provider, Event-Sender) und stammen an
// der einzigen Aufrufstelle aus disjunkten Teilen der Composition Root. Eine
// künstliche Parameter-Struct hätte hier kein zweites Verwendungsziel und
// würde nur die Signatur verschleiern, statt sie zu klären.
#[allow(clippy::too_many_arguments)]
fn build_tui_managed_spawner(
    session_id: SessionId,
    spawn_context: SpawnContext,
    reasoning_effort: Option<harw_types::ReasoningEffort>,
    runtime_config: &harw_config::ResolvedConfig,
    discovery_cwd: &std::path::Path,
    project_root: &std::path::Path,
    model: Arc<dyn ModelProvider>,
    event_tx: tokio::sync::mpsc::UnboundedSender<SessionEvent>,
) -> Result<Arc<ManagedAgentSpawner>, TuiError> {
    if runtime_config.agents.is_empty() {
        return Err(TuiError::Core(
            "refusing to expose child spawning without configured child role definitions"
                .to_owned(),
        ));
    }

    let mut configured_roles = BTreeMap::new();
    for (name, definition) in &runtime_config.agents {
        if name.trim().is_empty() {
            return Err(TuiError::Core(
                "refusing to register an empty configured child role name".to_owned(),
            ));
        }
        let organizational_role = configured_child_organizational_role(&definition.role)?;
        configured_roles.insert(name.clone(), organizational_role);
    }

    let factory: Arc<dyn ChildRegistryFactory> = Arc::new(TuiChildRegistryFactory::new(
        discovery_cwd,
        project_root,
        model,
    )?);

    let manager = Arc::new(Mutex::new(SessionManager::new(event_tx)));
    let mut spawner = ManagedAgentSpawner::new(manager, tui_child_limits(runtime_config));
    // 1. Eingebaute, read-only zugeschnittene Rollen — Namen aus `role_names`.
    for role in role_names::ALL {
        spawner = spawner.with_role(
            (*role).to_owned(),
            AgentRole::Agent {
                name: (*role).to_owned(),
            },
            AgentRoleId::Worker,
            Arc::clone(&factory),
        );
    }
    // 2. Konfigurierte Rollen; gleichnamige Konfiguration gewinnt.
    for (name, organizational_role) in configured_roles {
        spawner = spawner.with_role(
            name.clone(),
            AgentRole::Agent { name },
            organizational_role,
            Arc::clone(&factory),
        );
    }

    spawner
        .with_external_root_parent(session_id, spawn_context, reasoning_effort)
        .map(Arc::new)
        .map_err(|error| {
            TuiError::Core(format!(
                "could not register trusted TUI root for child spawning: {error}"
            ))
        })
}

/// Bündelt die per-Session konfigurierten Services eines modell-initiierten
/// Tool-Aufrufs.
///
/// # Description
/// `runtime_config`, `memory`, `job_store`, `controller`, `managed_spawner`
/// und `state_store` gehören fachlich zusammen: [`tui_model_tool_context`]
/// überführt sie 1:1 in die [`ServiceMap`] des jeweiligen `OpContext`, und
/// [`add_tui_model_tool_provider`] reicht sie unverändert an jeden über den
/// Provider gebauten Aufruf weiter. Das Bündeln hält beide Funktionen unter
/// der clippy-Grenze von sieben Parametern, ohne die einzelnen Felder
/// künstlich zu verstecken.
///
/// # Felder
/// - `managed_spawner` (`Option<&Arc<ManagedAgentSpawner>>`): `None` means no
///   child-spawning authority was configured for this session (the resolved
///   `agents` config was empty). In that case the [`ManagedAgentSpawner`]
///   service is intentionally **not** inserted into the [`ServiceMap`], so a
///   subsequent `/agent` operation call degrades to `OpError::NotAvailable`
///   instead of aborting the chat turn. `Some(spawner)` registers the
///   spawner service exactly as before, enabling child spawning.
struct TuiModelToolServices<'a> {
    runtime_config: Option<&'a Arc<harw_config::ResolvedConfig>>,
    memory: Option<&'a Arc<dyn harw_memory::Memory>>,
    job_store: Option<&'a Arc<harw_session_store::JobStore>>,
    controller: &'a Arc<TuiSessionController>,
    managed_spawner: Option<&'a Arc<ManagedAgentSpawner>>,
    state_store: &'a Arc<dyn StateStore>,
}

/// Builds an operation context for a model-initiated tool call.
///
/// # Description
/// The execution context is the sole source of per-call authority. All
/// services are captured from the trusted TUI composition root; model tool
/// arguments never participate in this construction.
///
/// # Arguments
/// - `services` (`&TuiModelToolServices<'_>`): see [`TuiModelToolServices`],
///   in particular its `managed_spawner` field for the `None`/`Some`
///   contract.
///
/// # Returns
/// `OpContext` — the fully assembled per-call context, carrying the
/// operation registry and every optional service that was configured.
fn tui_model_tool_context(
    execution_context: &harw_extension_api::ToolExecutionContext,
    operations: &[Arc<dyn harw_operations::Operation>],
    services: &TuiModelToolServices<'_>,
) -> OpContext {
    let mut operation_registry = OperationRegistry::new();
    for operation in operations {
        operation_registry.register(Arc::clone(operation));
    }

    let mut service_map = ServiceMap::new();
    service_map.insert(operation_registry);
    if let Some(config) = services.runtime_config {
        service_map.insert(Arc::clone(config));
    }
    if let Some(memory) = services.memory {
        service_map.insert(Arc::clone(memory));
    }
    if let Some(job_store) = services.job_store {
        service_map.insert(Arc::clone(job_store));
    }
    let shared_controller: SharedSessionController =
        Arc::clone(services.controller) as SharedSessionController;
    service_map.insert(shared_controller);
    if let Some(managed_spawner) = services.managed_spawner {
        service_map.insert(Arc::clone(managed_spawner));
    }
    service_map.insert(Arc::clone(services.state_store));

    OpContext::new(
        execution_context.session_id().clone(),
        execution_context.turn_id().clone(),
        execution_context.sandbox().clone(),
        service_map,
    )
}

/// Appends the model-facing operation provider without changing registry
/// approval handlers. Existing default approval behavior therefore continues
/// to gate unknown and operation tools.
///
/// # Arguments
/// - `services` (`&TuiModelToolServices<'_>`): forwarded unchanged into every
///   [`tui_model_tool_context`] built for this provider. See
///   [`TuiModelToolServices`] for the `managed_spawner` `None`/`Some`
///   contract.
fn add_tui_model_tool_provider(
    extension_registry: &mut harw_extension_api::ExtensionRegistry,
    operations: &[Arc<dyn harw_operations::Operation>],
    services: &TuiModelToolServices<'_>,
) {
    let provider_operations = operations.to_vec();
    let context_operations = operations.to_vec();
    let runtime_config = services.runtime_config.cloned();
    let memory = services.memory.cloned();
    let job_store = services.job_store.cloned();
    let controller = Arc::clone(services.controller);
    let managed_spawner = services.managed_spawner.cloned();
    let state_store = Arc::clone(services.state_store);
    let provider = ModelToolProvider::new(provider_operations, move |execution_context| {
        let services = TuiModelToolServices {
            runtime_config: runtime_config.as_ref(),
            memory: memory.as_ref(),
            job_store: job_store.as_ref(),
            controller: &controller,
            managed_spawner: managed_spawner.as_ref(),
            state_store: &state_store,
        };
        tui_model_tool_context(execution_context, &context_operations, &services)
    });
    extension_registry.add_tool_provider(Arc::new(provider));
}

/// Startet den interaktiven ratatui-Chat gegen den gegebenen Modell-Provider.
///
/// # Beschreibung
/// Baut eine [`harw_core::AgentSession`] (Rolle [`harw_types::AgentRole::Assistant`],
/// leere Extension-Registry, unbounded Event-Channel) und verwendet den vom
/// Composition Root injizierten [`StateStore`]. Dadurch kann der CLI-Aufrufer
/// einen [`harw_core::TranscriptStateStore`] für durable TUI-Verläufe liefern;
/// die TUI fällt nicht implizit auf [`harw_core::InMemoryStateStore`] zurück.
/// Anschließend aktiviert sie Raw-Mode/Alternate-Screen hinter einem
/// RAII-Guard (`TerminalGuard`) und betreibt den asynchronen internen
/// Event-Loop über einen `current_thread`-Tokio-Runtime.
///
/// Baut zusätzlich die `/`-Command-Adapter-Pipeline aus der übergebenen
/// [`OperationRegistry`] (`CommandAdapter::from_operation` pro registrierter
/// Op) sowie eine frische [`SessionId`] und übergibt beides zusammen mit der
/// `sandbox` an [`ChatApp::new`], sodass `/command`-Zeilen echt über
/// `harw-ops` dispatcht werden (siehe [`crate::command_exec::execute_command`]).
///
/// # Argumente
/// - `model` (`Box<dyn ModelProvider>`): der Provider, an den jeder Turn geht.
/// - `operations` (`OperationRegistry`): die 16 `harw-ops`-Kern-Operationen
///   (typischerweise via `harw_ops::register_all`), aus denen die
///   `/`-Command-Adapter gebaut werden.
/// - `sandbox` (`SandboxSpec`): Authority-Boundary für alle
///   Operation-Dispatches dieser Chat-Session.
///
/// # Rückgabe
/// `Ok(())` bei sauberem Verlassen des Loops.
///
/// # Fehler
/// - [`TuiError::Io`]: bei Terminal-Setup, Zeichnen oder Event-I/O.
/// - [`TuiError::Core`]: wenn der Turn-Loop einen Fehler meldet.
///
/// # Nebenläufigkeit
/// Läuft synchron im aufrufenden Thread; treibt den async-Loop über einen
/// lokalen `current_thread`-Runtime und startet einen Eingabe-Reader-Thread.
pub fn run_chat_tui(
    model: Box<dyn ModelProvider>,
    store: Box<dyn StateStore>,
    job_store: Arc<harw_session_store::JobStore>,
    runtime_config: Arc<harw_config::ResolvedConfig>,
    operations: OperationRegistry,
    sandbox: SandboxSpec,
    memory: Option<std::sync::Arc<dyn harw_memory::Memory>>,
) -> Result<(), TuiError> {
    run_chat_tui_resumable(
        model,
        store,
        job_store,
        runtime_config,
        None,
        operations,
        sandbox,
        memory,
        None,
        None,
    )
}

/// Runs the local TUI with optional durable-session resume support.
///
/// `existing_session_id` selects the exact initial session when present. The
/// optional `resume_selector` remains caller-owned: `/resume` asks it for the
/// selectable IDs, while `/resume <selector>` asks it to resolve the supplied
/// exact/prefix selector. A successful resolution rebuilds all session-scoped
/// runtime state without dropping raw mode or leaving the alternate screen.
#[allow(clippy::too_many_arguments)]
pub fn run_chat_tui_resumable(
    model: Box<dyn ModelProvider>,
    store: Box<dyn StateStore>,
    job_store: Arc<harw_session_store::JobStore>,
    runtime_config: Arc<harw_config::ResolvedConfig>,
    selected_executable_agent_ir: Option<ExecutableAgentIr>,
    operations: OperationRegistry,
    sandbox: SandboxSpec,
    memory: Option<std::sync::Arc<dyn harw_memory::Memory>>,
    existing_session_id: Option<SessionId>,
    resume_selector: Option<&dyn ResumeSessionSelector>,
) -> Result<(), TuiError> {
    run_chat_tui_resumable_with_plan(
        model,
        store,
        job_store,
        runtime_config,
        selected_executable_agent_ir,
        operations,
        sandbox,
        memory,
        existing_session_id,
        resume_selector,
        None,
    )
}

/// Wie [`run_chat_tui_resumable`], zusätzlich mit den Plan-/Ziel-Diensten der
/// Composition-Root.
///
/// # Beschreibung
/// AP W5-10b. `harw-tui` öffnet selbst niemals einen Plan- oder Goal-Store; die
/// Composition-Root baut beide genau einmal gegen das aktive Profil und reicht
/// sie hier durch. Nur mit ihnen kann der Renderer ein `PlanUpdated`-Ereignis
/// als [`PlanGraphCell`] und `/goal check` als [`GoalCell`] darstellen; ohne sie
/// bleibt beides eine Systemzeile.
///
/// Diese Funktion existiert **zusätzlich** zu [`run_chat_tui_resumable`], damit
/// die bestehende Signatur (und damit der Aufruf in `harw-cli/src/chat.rs`)
/// unverändert gültig bleibt.
///
/// # Argumente
/// Wie [`run_chat_tui_resumable`], zusätzlich:
/// - `plan_services` (`Option<TuiPlanServices>`): bereits gebaute Plan-/Ziel-
///   Stores; `None` schaltet die beiden Zellen ab, ohne den Chat zu berühren.
///
/// # Fehler
/// Wie [`run_chat_tui_resumable`].
#[allow(clippy::too_many_arguments)]
pub fn run_chat_tui_resumable_with_plan(
    model: Box<dyn ModelProvider>,
    store: Box<dyn StateStore>,
    job_store: Arc<harw_session_store::JobStore>,
    runtime_config: Arc<harw_config::ResolvedConfig>,
    selected_executable_agent_ir: Option<ExecutableAgentIr>,
    operations: OperationRegistry,
    sandbox: SandboxSpec,
    memory: Option<std::sync::Arc<dyn harw_memory::Memory>>,
    existing_session_id: Option<SessionId>,
    resume_selector: Option<&dyn ResumeSessionSelector>,
    plan_services: Option<TuiPlanServices>,
) -> Result<(), TuiError> {
    let model: Arc<dyn ModelProvider> = model.into();
    let store: Arc<dyn StateStore> = store.into();
    // Assemble the coding-agent registry (fs, shell, instructions, project
    // discovery). Both filesystem and project-discovery failures are fatal:
    // falling back to an empty registry or an uncanonicalized cwd would make
    // the session's trust boundary ambiguous.
    let cwd = std::env::current_dir().map_err(|error| {
        TuiError::Core(format!(
            "could not determine current working directory: {error}"
        ))
    })?;
    let canonical_cwd = cwd.canonicalize().map_err(|error| {
        TuiError::Core(format!(
            "could not canonicalize current working directory: {error}"
        ))
    })?;
    let assembled = assemble_tui_registry(canonical_cwd.clone())?;
    let project_root = assembled.project.project_root;
    let sandbox = sandbox_for_project(&sandbox, &project_root).map_err(TuiError::Core)?;
    let session_id = selected_session_id(&project_root, existing_session_id);
    let operation_list: Vec<Arc<dyn harw_operations::Operation>> =
        operations.iter().map(Arc::clone).collect();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(TuiError::from)?;

    let SessionRuntime {
        mut session,
        mut app,
        mut event_rx,
        mut turn_event_rx,
        mut approval_driver,
        mut approvals,
    } = build_session_runtime(
        session_id,
        &operation_list,
        &sandbox,
        &canonical_cwd,
        &project_root,
        &job_store,
        &runtime_config,
        memory.as_ref(),
        selected_executable_agent_ir.as_ref(),
        Arc::clone(&model),
        Arc::clone(&store),
        plan_services.as_ref(),
    )?;
    let history = runtime
        .block_on(store.load_history(session.id()))
        .map_err(|error| TuiError::Core(format!("durable history load failed: {error}")))?;
    install_loaded_history(&mut session, &mut app, history);
    app.push_lines(vec![Line::from(WELCOME)]);
    let mut gateway = ResumableGateway::new(session, Arc::clone(&store), Arc::clone(&model));
    let mut guard = TerminalGuard::enter()?;

    let result = runtime.block_on(async {
        let (tui_tx, mut tui_rx) = tokio::sync::mpsc::unbounded_channel::<TuiEvent>();
        let (harw_tx, mut harw_rx) = harw_event_channel();
        let (frame_req, frame_rx) = frame_channel();
        tokio::spawn(frame_scheduler(frame_rx, tui_tx.clone()));
        let _reader = spawn_input_reader(tui_tx);
        frame_req.schedule_frame();

        loop {
            match run_loop(
                &mut guard,
                &mut app,
                &mut gateway,
                &mut event_rx,
                &mut turn_event_rx,
                &mut tui_rx,
                &harw_tx,
                &mut harw_rx,
                &frame_req,
                &approval_driver,
                &mut approvals,
            )
            .await?
            {
                TuiRunOutcome::Quit => return Ok(()),
                TuiRunOutcome::Resume { selector: None } => {
                    let message = match resume_selector {
                        Some(selector) => match selector.available_sessions() {
                            Ok(ids) if ids.is_empty() => {
                                "No resumable sessions available.".to_owned()
                            }
                            Ok(ids) => format!(
                                "Select a session with /resume <selector>:\n{}",
                                ids.iter()
                                    .map(SessionId::as_str)
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            ),
                            Err(error) => format!("Could not list resumable sessions: {error}"),
                        },
                        None => "Session resume is not configured.".to_owned(),
                    };
                    app.push_line(Role::System, message);
                    frame_req.schedule_frame();
                }
                TuiRunOutcome::Resume {
                    selector: Some(raw_selector),
                } => {
                    let Some(selector) = resume_selector else {
                        app.push_line(Role::System, "Session resume is not configured.");
                        frame_req.schedule_frame();
                        continue;
                    };
                    let selected = match selector.resolve_session(&raw_selector) {
                        Ok(selected) => selected,
                        Err(error) => {
                            app.push_line(
                                Role::System,
                                format!("Could not resolve session: {error}"),
                            );
                            frame_req.schedule_frame();
                            continue;
                        }
                    };
                    let SessionRuntime {
                        session: mut next_session,
                        app: mut next_app,
                        event_rx: next_event_rx,
                        turn_event_rx: next_turn_event_rx,
                        approval_driver: next_driver,
                        approvals: next_approvals,
                    } = build_session_runtime(
                        selected,
                        &operation_list,
                        &sandbox,
                        &canonical_cwd,
                        &project_root,
                        &job_store,
                        &runtime_config,
                        memory.as_ref(),
                        selected_executable_agent_ir.as_ref(),
                        Arc::clone(&model),
                        Arc::clone(&store),
                        plan_services.as_ref(),
                    )?;
                    let history = gateway
                        .store()
                        .load_history(next_session.id())
                        .await
                        .map_err(|error| {
                            TuiError::Core(format!("durable history load failed: {error}"))
                        })?;
                    install_loaded_history(&mut next_session, &mut next_app, history);
                    next_app.push_lines(vec![Line::from(WELCOME)]);
                    gateway.replace_session(next_session);
                    app = next_app;
                    event_rx = next_event_rx;
                    turn_event_rx = next_turn_event_rx;
                    // Treiber und Fragekanal gehören zum Handler der **neuen**
                    // Registry; sie müssen zusammen mit ihr ersetzt werden,
                    // sonst fände der Treiber die Rückkanäle nicht mehr.
                    approval_driver = next_driver;
                    approvals = next_approvals;
                    frame_req.schedule_frame();
                }
            }
        }
    });
    // `guard` wird hier gedroppt und stellt das Terminal zurück, auch im Fehlerfall.
    drop(guard);
    result
}

/// Alles, was eine frisch aufgebaute (oder per `/resume` ersetzte) Chat-Session
/// an den Event-Loop übergibt.
///
/// # Beschreibung
/// Ein eigener Typ statt eines Tupels, weil AP W5-03 zwei weitere, **paarweise
/// zusammengehörige** Werte hinzufügt: der [`ApprovalDriver`] und der
/// [`ApprovalPromptReceiver`] gehören zum selben `Arc<TuiApprovalHandler>`, der
/// zugleich in der [`ExtensionRegistry`] dieser Session liegt. Ein Tupel würde
/// diese Kopplung verstecken.
struct SessionRuntime {
    /// Die aufgebaute Session.
    session: AgentSession,
    /// Der zugehörige Renderer-Zustand.
    app: ChatApp,
    /// Turn-granulare Session-Ereignisse (Token-Summary).
    event_rx: tokio::sync::mpsc::UnboundedReceiver<SessionEvent>,
    /// Werkzeug-/Kind-/Plan-granulare Turn-Ereignisse.
    turn_event_rx: tokio::sync::mpsc::UnboundedReceiver<TurnEvent>,
    /// Treiber über beide Pausearten; hält denselben Handler wie die Registry.
    approval_driver: ApprovalDriver,
    /// Fragekanal zum Renderer; **muss** gepollt werden, sonst läuft jede Frage
    /// in den Timeout und gilt damit als Ablehnung.
    approvals: ApprovalPromptReceiver,
}

#[allow(clippy::too_many_arguments)]
fn build_session_runtime(
    session_id: SessionId,
    operations: &[Arc<dyn harw_operations::Operation>],
    sandbox: &SandboxSpec,
    discovery_cwd: &std::path::Path,
    project_root: &std::path::Path,
    job_store: &Arc<harw_session_store::JobStore>,
    runtime_config: &Arc<harw_config::ResolvedConfig>,
    memory: Option<&Arc<dyn harw_memory::Memory>>,
    selected_executable_agent_ir: Option<&ExecutableAgentIr>,
    model: Arc<dyn ModelProvider>,
    state_store: Arc<dyn StateStore>,
    plan_services: Option<&TuiPlanServices>,
) -> Result<SessionRuntime, TuiError> {
    // Re-discover from the original canonical cwd so this registry carries the
    // same nested instruction cascade as the initial root selection. The
    // sandbox was already bound to that initial root; reject a filesystem
    // change that makes this second discovery resolve elsewhere.
    let assembled = assemble_tui_registry(discovery_cwd.to_path_buf())?;
    ensure_tui_context_roots_align(&assembled.project.project_root, project_root)?;
    // AP W5-03, Bedingung 2: genau ein Handler, dessen `Arc` gleichzeitig in
    // der Registry dieser Session und im `ApprovalDriver` liegt.
    let (approval_handler, approvals) = TuiApprovalHandler::new();
    let approval_driver = ApprovalDriver::new(Arc::clone(&approval_handler));
    let registered_handler = as_dyn_approval_handler(&approval_handler);
    let mut extension_registry =
        registry_with_approval_handler(assembled.registry, registered_handler)?;
    // Die von der Composition-Root mitgegebenen Beiträge — Ziel-Kontext und
    // zusätzliche Freigabe-Politik. Ohne diesen Schritt bleibt ein per `--goal`
    // gesetztes Ziel im Store liegen, ohne je einen Turn zu erreichen, und die
    // aus `[policy] require_approval_for` kompilierte Politik wirkt nirgends.
    if let Some(services) = plan_services {
        extension_registry = registry_with_plan_contributions(extension_registry, services)?;
    }
    let session_controller = Arc::new(TuiSessionController::new());
    let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();
    let (turn_event_tx, turn_event_rx) = tokio::sync::mpsc::unbounded_channel();
    // AP W5-03, Bedingung 1: `approval_actor` ist hier `Some(Operator)`. Ohne
    // ihn scheitert der Kern schon vor jedem `AwaitingApproval` mit
    // `CoreError::MissingApprovalActor`.
    let root_spawn_context = trusted_tui_spawn_context(sandbox);
    debug_assert!(
        root_spawn_context.approval_actor.is_some(),
        "the TUI spawn context must carry an approval actor"
    );
    // Child spawning is only exposed when at least one child role has been
    // configured. An empty role set is not an error here: the chat turn must
    // still succeed, just without the `/agent` spawn capability, which then
    // degrades gracefully to `OpError::NotAvailable` when invoked.
    let managed_spawner = if runtime_config.agents.is_empty() {
        None
    } else {
        Some(build_tui_managed_spawner(
            session_id.clone(),
            root_spawn_context,
            None,
            runtime_config,
            discovery_cwd,
            project_root,
            model,
            event_tx.clone(),
        )?)
    };
    add_tui_model_tool_provider(
        &mut extension_registry,
        operations,
        &TuiModelToolServices {
            runtime_config: Some(runtime_config),
            memory,
            job_store: Some(job_store),
            controller: &session_controller,
            managed_spawner: managed_spawner.as_ref(),
            state_store: &state_store,
        },
    );
    let mut session = build_tui_agent_session(
        session_id.clone(),
        extension_registry,
        event_tx,
        turn_event_tx,
        sandbox,
        selected_executable_agent_ir,
    );
    let adapters = operations
        .iter()
        .flat_map(|operation| CommandAdapter::from_operation(Arc::clone(operation)))
        .collect();
    let mut app = ChatApp::with_memory(adapters, sandbox.clone(), session_id, memory.cloned())
        .with_job_store(Arc::clone(job_store))
        .with_runtime_config(Arc::clone(runtime_config))
        .with_session_controller(session_controller)
        .with_project_root(project_root.display().to_string())
        .with_managed_spawner(managed_spawner);
    if let Some(services) = plan_services {
        app = app.with_plan_services(services.clone());
        // Startmodus aus `--mode` bzw. `[mode] default`. Er wird **vor** dem
        // ersten Turn gesetzt, weil `set_mode` Tool-Aktivierung und
        // Sandbox-Obergrenze schneidet — mitten in einem Turn wäre das die
        // falsche Stelle.
        if let Some(mode) = services.initial_mode {
            session.set_mode(mode);
            tracing::info!(mode = mode.as_str(), "tui.session.initial_mode");
        }
    }
    app.set_active_mode(session.mode());
    Ok(SessionRuntime {
        session,
        app,
        event_rx,
        turn_event_rx,
        approval_driver,
        approvals,
    })
}

/// Constructs the TUI-owned session after the trusted spawn context has been
/// established. A selected executable policy is applied exactly once here,
/// before any turn can be processed.
fn build_tui_agent_session(
    session_id: SessionId,
    extension_registry: harw_extension_api::ExtensionRegistry,
    event_tx: tokio::sync::mpsc::UnboundedSender<SessionEvent>,
    turn_event_tx: tokio::sync::mpsc::UnboundedSender<TurnEvent>,
    sandbox: &SandboxSpec,
    selected_executable_agent_ir: Option<&ExecutableAgentIr>,
) -> AgentSession {
    let session = AgentSession::new_with_id(
        session_id,
        AgentRole::Assistant,
        None,
        extension_registry,
        event_tx,
    )
    .with_spawn_context(trusted_tui_spawn_context(sandbox));
    let session = match selected_executable_agent_ir {
        Some(policy) => session.with_executable_agent_ir(policy),
        None => session,
    };
    session.with_turn_event_sink(turn_event_tx)
}

/// Zeitfenster, in dem ein zweites Ctrl+C/Ctrl+D den Chat beendet.
const QUIT_HINT_WINDOW: Duration = Duration::from_secs(2);

/// Maximale Popup-Höhe in Zeilen (ohne Rahmen).
const POPUP_MAX_ROWS: u16 = 8;

/// „Scharfgestellter" Beenden-Zustand: welches Label + wann gedrückt.
///
/// Ein zweiter Druck derselben Taste innerhalb von [`QUIT_HINT_WINDOW`] beendet;
/// jede andere Taste (und der Timeout) macht die Scharfstellung rückgängig.
#[derive(Clone, Copy)]
struct QuitArm {
    /// Anzeige-Label der Taste (`"Ctrl+C"` / `"Ctrl+D"`).
    label: &'static str,
    /// Zeitpunkt des ersten Drucks.
    at: Instant,
}

/// Betreibt den asynchronen `tokio::select!`-Event-Loop bis zum Verlassen.
///
/// # Beschreibung
/// Legt die drei internen Kanäle an (TUI-Eingabe, Anwendungs-Bus,
/// Frame-Anforderung), startet den Frame-Scheduler-Task und den blockierenden
/// Eingabe-Reader-Thread und verarbeitet dann in einer Schleife jeweils genau
/// ein Ereignis:
/// - [`TuiEvent::Draw`] → Fullscreen-Viewport neu zeichnen,
/// - [`TuiEvent::Key`] → [`handle_key`],
/// - [`TuiEvent::Paste`] → Text in den Eingabepuffer,
/// - [`TuiEvent::Resize`] → Redraw anfordern,
/// - [`HarwEvent::Submit`] → Turn treiben + Antwort streamen,
/// - [`HarwEvent::SystemMessage`] → Systemzeile in interne History (`cells`),
/// - [`HarwEvent::Command`] → `/command`-Zeile asynchron über die
///   Operation-Adapter-Pipeline ausführen ([`crate::command_exec::execute_command`]
///   mit `app.adapters()` / `app.sandbox()` / `app.session_id()`) und das
///   Ergebnis wie bei `SystemMessage` in die History übernehmen,
/// - [`HarwEvent::Quit`] → Loop verlassen,
/// - [`SessionEvent::TurnCompleted`] (über `event_rx`) → Token-Summary in
///   [`ChatApp::total_usage`] akkumulieren; andere `SessionEvent`-Varianten
///   und ein geschlossener Kanal werden in dieser Welle bewusst ignoriert,
/// - [`TurnEvent`] (über `turn_event_rx`) → [`handle_turn_event`].
///
/// # Warum die Submit-Behandlung außerhalb von `tokio::select!` steht
/// `select!` erzeugt die Futures **aller** Zweige, bevor der Rumpf eines
/// Zweiges läuft — `tui_rx` und `turn_event_rx` sind währenddessen schon
/// veränderlich geliehen. Der Turn-Pfad braucht dieselben beiden Empfänger
/// (Freigabetasten und Live-Zellen während des Turns, AP W5-03). Die
/// abgeschickte Zeile wird deshalb im Zweig nur gemerkt und erst **nach** dem
/// `select!` verarbeitet, wenn alle Leihen wieder frei sind.
///
/// # Argumente
/// - `event_rx` (`&mut UnboundedReceiver<SessionEvent>`): Turn-Granularität,
///   Token-Summary.
/// - `turn_event_rx` (`&mut UnboundedReceiver<TurnEvent>`): Tool-Call-,
///   Kind-, Plan- und Modus-Granularität.
/// - `approval_driver` (`&ApprovalDriver`): treibt pausierte Turns bis zum
///   Ende; hält denselben Handler wie die Registry dieser Session.
/// - `approvals` (`&mut ApprovalPromptReceiver`): Fragekanal desselben
///   Handlers. Er wird während eines laufenden Turns in
///   [`drive_turn_animated`] gepollt — bliebe er ungepollt, liefe jede Frage
///   in den Timeout und würde damit zur Ablehnung.
///
/// # Fehler
/// [`TuiError`] bei Zeichnen, Terminal-I/O oder Turn-Fehler.
///
/// # Nebenläufigkeit
/// Läuft auf dem `current_thread`-Runtime. Spawnt den Frame-Scheduler als
/// Tokio-Task und den Eingabe-Reader als OS-Thread.
#[allow(clippy::too_many_arguments)]
async fn run_loop(
    guard: &mut TerminalGuard,
    app: &mut ChatApp,
    gateway: &mut dyn crate::gateway::ChatGateway,
    event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<SessionEvent>,
    turn_event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<TurnEvent>,
    tui_rx: &mut tokio::sync::mpsc::UnboundedReceiver<TuiEvent>,
    harw_tx: &HarwEventSender,
    harw_rx: &mut tokio::sync::mpsc::UnboundedReceiver<HarwEvent>,
    frame_req: &FrameRequester,
    approval_driver: &ApprovalDriver,
    approvals: &mut ApprovalPromptReceiver,
) -> Result<TuiRunOutcome, TuiError> {
    // Seitenkanäle der Turn-Ereignisverarbeitung: Werkzeugnamen-Korrelation und
    // die eine Verlaufszelle je Kind-Session.
    let mut turn_state = TurnEventState::default();

    let mut pending_quit: Option<QuitArm> = None;
    let mut spinner = Spinner::new();

    loop {
        // Wird im `harw_rx`-Zweig gesetzt und **nach** dem `select!`
        // verarbeitet, damit der Turn-Pfad `tui_rx`/`turn_event_rx` erneut
        // veränderlich leihen darf (siehe Funktionsdoku).
        let mut submitted: Option<String> = None;

        tokio::select! {
            maybe_tev = async { match app.deferred_input.pop_front() { Some(event) => Some(event), None => tui_rx.recv().await } } => {
                let Some(tev) = maybe_tev else { return Ok(TuiRunOutcome::Quit) };

                // Abgelaufenen Beenden-Hinweis verwerfen.
                if let Some(arm) = pending_quit {
                    if arm.at.elapsed() > QUIT_HINT_WINDOW {
                        pending_quit = None;
                    }
                }

                match tev {
                    TuiEvent::Draw => {
                        draw_viewport(guard, app, &spinner, pending_quit.map(|arm| arm.label))?;
                    }
                    TuiEvent::Key(key) => {
                        if handle_key(app, key, &mut pending_quit, harw_tx) {
                            frame_req.schedule_frame();
                        }
                    }
                    TuiEvent::Mouse(mouse) => {
                        // Nur das Rad scrollt die Historie; Klicks und
                        // Bewegungen bleiben unbeachtet.
                        if app.scroll.handle_mouse(
                            mouse,
                            app.last_history_total_lines() as usize,
                            app.last_history_visible_rows() as usize,
                        ) == ScrollAction::Redraw
                        {
                            frame_req.schedule_frame();
                        }
                    }
                    TuiEvent::Paste(text) => {
                        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
                        app.input.insert_str(&normalized);
                        app.sync_popup();
                        frame_req.schedule_frame();
                    }
                    TuiEvent::Resize(_, _) => {
                        frame_req.schedule_frame();
                    }
                }
            }
            maybe_hev = harw_rx.recv() => {
                let Some(hev) = maybe_hev else { return Ok(TuiRunOutcome::Quit) };
                match hev {
                    HarwEvent::Quit => return Ok(TuiRunOutcome::Quit),
                    HarwEvent::SystemMessage(message) => {
                        // Mehrzeilige Ausgaben (z. B. `/help`) an `\n` aufteilen.
                        let lines: Vec<Line<'static>> = message
                            .split('\n')
                            .map(|line| Line::from(line.to_owned()))
                            .collect();
                        app.push_lines(lines);
                        frame_req.schedule_frame();
                    }
                    HarwEvent::Submit(text) => {
                        // Nur merken — verarbeitet wird nach dem `select!`.
                        submitted = Some(text);
                    }
                    HarwEvent::Command(raw) => {
                        if let Some(request) = resume_request(&raw) {
                            return Ok(request);
                        }
                        // `/tools` — handled locally via `tools_command` module;
                        // does NOT go through the Operation-Adapter pipeline so
                        // that it can mutate the session's `SessionActivation`
                        // directly without serialising through an async op.
                        //
                        // Split-borrow strategy: `registry()` returns `&ExtensionRegistry`
                        // from the session. We cannot hold that reference AND call
                        // `activation_mut()` at the same time because both go through
                        // `gateway.session_mut()`. Solution: collect all registry data
                        // we need into an owned value first, then drop the shared
                        // borrow before taking the mutable one.
                        let tools_outcome = if raw.trim_start_matches('/').starts_with("tools") {
                            let args = raw
                                .trim()
                                .strip_prefix("/tools")
                                .unwrap_or("")
                                .to_owned();
                            // Phase 1: clone tool specs out of the registry
                            // (avoids holding a `&ExtensionRegistry` borrow
                            //  while we later take `&mut SessionActivation`).
                            let tool_names: Vec<(String, bool)> = {
                                let session = gateway.session_mut();
                                let registry = session.registry();
                                let activation = session.activation();
                                registry
                                    .tool_providers()
                                    .iter()
                                    .flat_map(|p| p.tools())
                                    .map(|spec| {
                                        let name = spec.name().to_owned();
                                        let enabled =
                                            activation.is_tool_enabled(&harw_extension_api::ToolName::new(&name));
                                        (name, enabled)
                                    })
                                    .collect()
                            };
                            // Phase 2: mutate activation via the parsed args.
                            let activation = gateway.session_mut().activation_mut();
                            Some(crate::tools_command::dispatch_tools_command(
                                &args,
                                &tool_names,
                                activation,
                            ))
                        } else {
                            None
                        };

                        if let Some(outcome) = tools_outcome {
                            let lines: Vec<Line<'static>> = outcome
                                .into_lines()
                                .into_iter()
                                .map(Line::from)
                                .collect();
                            app.push_lines(lines);
                            frame_req.schedule_frame();
                        } else {
                            // `/command`-Zeile asynchron über die Operation-Adapter-
                            // Pipeline ausführen; identischer Render-/Redraw-Pfad wie
                            // bei `SystemMessage` (mehrzeilige Ausgaben an `\n`
                            // aufteilen).
                            let output = execute_command_as(
                                app.adapters(),
                                app.sandbox(),
                                app.session_id(),
                                LOCAL_TUI_OPERATION_PERMISSION,
                                &raw,
                                &CommandServices {
                                    runtime_config: app.runtime_config(),
                                    memory: app.memory(),
                                    controller: app.session_controller(),
                                    job_store: app.job_store(),
                                },
                            )
                            .await;
                            let lines: Vec<Line<'static>> = output
                                .split('\n')
                                .map(|line| Line::from(line.to_owned()))
                                .collect();
                            app.push_lines(lines);
                            // AP W5-05: Eine `/command`-Zeile läuft **zwischen**
                            // Turns. Das ist eine gültige Turn-Grenze, also darf
                            // ein soeben angefordertes `/mode` sofort wirken —
                            // sonst zeigte die Statuszeile bis zur nächsten
                            // Nachricht weiter den alten Modus.
                            app.apply_pending_controller_state(gateway.session_mut());
                            // AP W5-10b: Zielstand nach `/goal check` sichtbar
                            // machen, sofern die Composition-Root Plan-/Ziel-
                            // Dienste durchgereicht hat.
                            if let Some(cell) = goal_cell_for_command(app, &raw) {
                                app.push_cell(Box::new(cell));
                            }
                            frame_req.schedule_frame();
                        }
                    }
                }
            }
            maybe_sev = event_rx.recv() => {
                if let Some(SessionEvent::TurnCompleted { usage, .. }) = maybe_sev {
                    app.total_usage.add(&usage);
                    frame_req.schedule_frame();
                }
                // Andere SessionEvent-Varianten (TurnStarted, SessionConfigured, ...) und
                // ein geschlossener Kanal (None) werden in dieser Welle bewusst ignoriert —
                // nur die Token-Summary wird konsumiert.
            }
            maybe_tev = turn_event_rx.recv() => {
                if let Some(tev) = maybe_tev {
                    if handle_turn_event(app, &mut turn_state, tev) {
                        frame_req.schedule_frame();
                    }
                }
            }
        }

        // ── Turn-Pfad, außerhalb des `select!` ───────────────────────────────
        let Some(text) = submitted else {
            continue;
        };

        // Nutzerzelle in die interne History.
        app.push_line(Role::User, text.clone());
        // Auto-Correction-Detection (harw-memory M3): reine Textregel,
        // kein LLM-Call. Bei Match: Signal explizit an das Backend geben.
        if let Some(mem) = app.memory() {
            if let Some(sig) =
                harw_memory::detect_correction(&text, Some(format!("session:{}", app.session_id())))
            {
                // Fehler bewusst still verschlucken — Memory-Record ist
                // best-effort und darf den User-Turn niemals unterbrechen.
                let _ = mem.record(sig);
            }
        }
        // Turn treiben (Spinner animiert), Antwort simuliert streamen.
        //
        // `TuiError::Core` (Provider-/Model-Fehler, z. B. abgelaufene
        // Credentials) darf die Session NICHT beenden — analog zur
        // Rate-Limit-Behandlung in `drive_turn_animated` bleibt der
        // Chat offen und der Fehler wird als System-Zeile angezeigt.
        // `TuiError::Io` bleibt fatal und propagiert weiterhin nach
        // oben, da er einen nicht behebbaren Terminalfehler anzeigt.
        if let Err(error) = run_turn_streaming(
            guard,
            app,
            &mut spinner,
            gateway,
            &text,
            approval_driver,
            approvals,
            tui_rx,
            turn_event_rx,
            &mut turn_state,
        )
        .await
        {
            match error {
                TuiError::Io(_) => return Err(error),
                // Die Registrierung von Kontextanbietern geschieht beim
                // Aufbau der Sitzung, lange vor dieser Schleife -- die
                // Variante kann hier nicht auftreten. Sie bekommt trotzdem
                // einen eigenen Arm statt eines Sammelarms: ein `_ =>` nähme
                // dem Compiler genau die Hilfe, die bei der nächsten neuen
                // Variante zählt. Behandlung wie `Io`: ein
                // Konfigurationsdefekt ist durch Weiterlaufen nicht zu
                // beheben, und Zurückgeben ist die konservative Richtung.
                TuiError::ContextProviderRegistration(_) => return Err(error),
                TuiError::Core(_) => {
                    // `run_turn_streaming` gibt bei einem Fehler vor
                    // Erreichen von `spinner.stop()` zurück — hier
                    // sicherstellen, dass der Spinner nicht hängen bleibt.
                    spinner.stop();
                    app.push_line(Role::System, format!("⚠ {error}"));
                }
            }
        }
        frame_req.schedule_frame();
    }
}

/// Verarbeitet genau ein [`TurnEvent`] in den Renderer-Zustand.
///
/// # Beschreibung
/// AP W5-10b. Als freie Funktion ausgelagert, damit sowohl der Event-Loop
/// ([`run_loop`]) als auch der laufende Turn ([`drive_turn_animated`]) dieselbe
/// Verarbeitung benutzen — und damit sie ohne Terminal testbar ist.
///
/// Zuordnung Ereignis → Zelle:
/// - `ToolCallRequested` → [`ToolCallHistoryCell`] (Name wird für das spätere
///   `ToolCallCompleted` in `state.pending_tool_names` gemerkt),
/// - `ToolCallCompleted` → [`ToolResultHistoryCell`],
/// - `ItemAdded(Reasoning)` → [`ReasoningHistoryCell`], nur bei nicht-leerer
///   Zusammenfassung,
/// - `ChildSpawned` → **eine** [`SubAgentCell`] je `child_id`, geteilt über
///   [`SharedHistoryCell`],
/// - `ChildProgress` / `ChildCompleted` → schreiben **dieselbe** Zelle fort
///   (drei Ereignisse, eine Zelle),
/// - `PlanUpdated` → [`PlanGraphCell`], wenn Plan-Dienste vorliegen und der
///   aktuelle Plan lesbar ist; sonst eine Systemzeile mit derselben Information,
/// - `ModeChanged` → Anzeigemodus der Statuszeile.
///
/// `TurnCompleted` wird hier bewusst **nicht** verarbeitet: die Token-Summary
/// läuft exklusiv über den `SessionEvent`-Pfad, sonst entstünden Doppelantworten.
///
/// # Argumente
/// - `app` (`&mut ChatApp`): Renderer-Zustand, der die Zellen aufnimmt.
/// - `state` (`&mut TurnEventState`): Seitenkanäle (Werkzeugnamen, Kind-Zellen).
/// - `event` ([`TurnEvent`]): das zu verarbeitende Ereignis; Besitz geht über.
///
/// # Rückgabe
/// `true`, wenn sich der sichtbare Zustand geändert hat und ein Redraw nötig ist.
fn handle_turn_event(app: &mut ChatApp, state: &mut TurnEventState, event: TurnEvent) -> bool {
    match event {
        TurnEvent::ToolCallRequested {
            call_id,
            tool_name,
            arguments,
            ..
        } => {
            let preview = {
                let raw = arguments.to_string();
                if raw.chars().count() > TOOL_ARGUMENTS_PREVIEW_CHARS {
                    format!(
                        "{}…",
                        raw.chars()
                            .take(TOOL_ARGUMENTS_PREVIEW_CHARS)
                            .collect::<String>()
                    )
                } else {
                    raw
                }
            };
            state.pending_tool_names.insert(call_id, tool_name.clone());
            app.push_cell(Box::new(ToolCallHistoryCell {
                tool_name,
                arguments_preview: preview,
            }));
            true
        }
        TurnEvent::ToolCallCompleted {
            call_id,
            result,
            duration_ms,
            ..
        } => {
            let tool_name = state
                .pending_tool_names
                .remove(&call_id)
                .unwrap_or_else(|| "tool".to_owned());
            app.push_cell(Box::new(ToolResultHistoryCell {
                tool_name,
                success: result.is_success(),
                duration_ms,
            }));
            true
        }
        TurnEvent::ItemAdded {
            item: TurnItem::Reasoning(reasoning),
            ..
        } => {
            let summary = reasoning.summary_text.join(" ");
            if summary.is_empty() {
                return false;
            }
            app.push_cell(Box::new(ReasoningHistoryCell { summary }));
            true
        }
        TurnEvent::ChildSpawned {
            child,
            role,
            question,
            ..
        } => {
            let child_id = child.as_str().to_owned();
            let cell = Arc::new(Mutex::new(SubAgentCell {
                child_id: child_id.clone(),
                role,
                question,
                tool_calls: 0,
                tokens: 0,
                status: SubAgentStatus::Running,
            }));
            state
                .child_cells
                .insert(child_id.clone(), Arc::clone(&cell));
            app.push_shared_cell(cell);
            tracing::debug!(child = %child_id, "tui.child_cell.created");
            true
        }
        TurnEvent::ChildProgress {
            child,
            tool_calls,
            tokens,
            ..
        } => state.update_child(child.as_str(), |cell| {
            cell.apply_progress(tool_calls, tokens);
        }),
        TurnEvent::ChildCompleted {
            child,
            outcome,
            duration_ms,
            ..
        } => state.update_child(child.as_str(), |cell| {
            cell.apply_completion(outcome, duration_ms);
        }),
        TurnEvent::PlanUpdated {
            plan_id,
            revision,
            summary,
        } => {
            // Bewusst als `let`-Bindung: eine Scrutinee-Temporäre würde die
            // gemeinsame Leihe auf `app` über den ganzen `match` halten und
            // damit `push_cell`/`push_line` blockieren.
            let current_plan = app
                .plan_services()
                .map(|services| services.plan_store.current());
            match current_plan {
                Some(Ok(plan)) => {
                    app.push_cell(Box::new(PlanGraphCell { plan }));
                }
                Some(Err(error)) => {
                    tracing::warn!(
                        plan_id = %plan_id,
                        revision,
                        error = %error,
                        "tui.plan_cell.plan_unreadable"
                    );
                    app.push_line(
                        Role::System,
                        format!("Plan {plan_id} @{revision}: {summary} (Plan nicht lesbar)"),
                    );
                }
                // Keine Plan-Dienste durchgereicht: die Information geht nicht
                // verloren, sie wird nur einzeilig statt als Graph gezeigt.
                None => {
                    app.push_line(
                        Role::System,
                        format!("Plan {plan_id} @{revision}: {summary}"),
                    );
                }
            }
            true
        }
        TurnEvent::ModeChanged { mode } => match InteractionMode::parse(&mode) {
            Some(parsed) => {
                app.set_active_mode(parsed);
                true
            }
            None => {
                tracing::warn!(mode = %mode, "tui.mode.unknown_name_ignored");
                false
            }
        },
        // TurnCompleted hier NICHT verarbeiten — das würde den Wave-1-
        // Duplikat-Antwort-Bug wiederholen (Token-Summary läuft exklusiv
        // über den SessionEvent-Pfad). ItemAdded(AssistantMessage/...),
        // TurnStarted, TurnFailed, TurnAborted: bewusst kein UI-Effekt.
        _ => false,
    }
}

/// Baut nach einem `/goal check` den sichtbaren Zielstand.
///
/// # Beschreibung
/// AP W5-10b. Liest Ziel und Plan aus den durchgereichten Diensten und wertet
/// sie mit [`harw_plan::goal::evaluate_goal`] aus. Jeder fehlende Baustein —
/// keine Dienste, kein gesetztes Ziel, kein Plan — führt zu `None`, nie zu einem
/// Fehler: die Textausgabe der Operation steht ohnehin schon im Verlauf.
///
/// # Argumente
/// - `app` (`&ChatApp`): liefert die optionalen Plan-/Ziel-Dienste.
/// - `raw` (`&str`): die unveränderte Befehlszeile.
///
/// # Rückgabe
/// `Some(GoalCell)`, wenn `raw` ein `/goal check` war und der Zielstand
/// vollständig auswertbar ist; sonst `None`.
fn goal_cell_for_command(app: &ChatApp, raw: &str) -> Option<GoalCell> {
    if !is_goal_check_command(raw) {
        return None;
    }
    let services = app.plan_services()?;
    let goal = services
        .goal_store
        .current()
        .inspect_err(|error| {
            tracing::debug!(error = %error, "tui.goal_cell.no_goal");
        })
        .ok()?;
    let plan = services
        .plan_store
        .current()
        .inspect_err(|error| {
            tracing::debug!(error = %error, "tui.goal_cell.no_plan");
        })
        .ok()?;
    let report = evaluate_goal(&goal, &plan);
    Some(GoalCell {
        statement: goal.statement.clone(),
        report,
    })
}

/// Erkennt genau die Zeile `/goal check` (mit beliebigem Umgebungs-Whitespace).
///
/// # Argumente
/// - `raw` (`&str`): die unveränderte Befehlszeile.
///
/// # Rückgabe
/// `true` für `/goal check`, sonst `false`.
fn is_goal_check_command(raw: &str) -> bool {
    let mut words = raw.split_whitespace();
    words.next() == Some("/goal") && words.next() == Some("check")
}

/// Koalesziert Frame-Anforderungen und speist `TuiEvent::Draw` in den TUI-Kanal.
///
/// # Beschreibung
/// Wartet auf ein Signal vom [`FrameRequester`], leert dann alle sofort
/// verfügbaren weiteren Anfragen (Koaleszenz), sendet genau ein
/// [`TuiEvent::Draw`] und wartet [`MIN_FRAME_INTERVAL`], bevor das nächste
/// Draw entstehen kann (Throttling auf ~120 FPS). Endet, sobald der
/// Frame-Kanal geschlossen wird oder der TUI-Kanal keinen Empfänger mehr hat.
///
/// # Argumente
/// - `frame_rx`: Empfänger der Frame-Anforderungen.
/// - `tui_tx`: Sender in den TUI-Ereignis-Kanal.
///
/// # Nebenläufigkeit
/// Läuft als eigener Tokio-Task auf dem `current_thread`-Runtime.
async fn frame_scheduler(
    mut frame_rx: tokio::sync::mpsc::UnboundedReceiver<()>,
    tui_tx: tokio::sync::mpsc::UnboundedSender<TuiEvent>,
) {
    while frame_rx.recv().await.is_some() {
        // Koaleszenz: alle bereits anstehenden Anfragen verwerfen.
        while frame_rx.try_recv().is_ok() {}
        if tui_tx.send(TuiEvent::Draw).is_err() {
            break;
        }
        tokio::time::sleep(MIN_FRAME_INTERVAL).await;
    }
}

/// Verarbeitet einen Tastendruck: mutiert den Eingabezustand und emittiert
/// [`HarwEvent`]s. Gibt `true` zurück, wenn ein Redraw nötig ist.
///
/// # Tastensteuerung (an codex/codex-rs orientiert)
/// - **Enter** — Zeile absenden (leer/whitespace: ignoriert).
/// - **Ctrl+J** (auch Shift/Alt+Enter) — neue Zeile im Eingabepuffer.
/// - **Ctrl+C** — 1×: „again to quit"-Hinweis; 2× innerhalb 2 s: [`HarwEvent::Quit`].
/// - **Ctrl+D** — nur bei **leerer** Eingabe; 1×: Hinweis, 2×: Quit.
/// - **Esc** — aktuelle Eingabe leeren.
/// - **Backspace** — letztes Zeichen löschen.
/// - **PageUp** — History nach oben scrollen (via [`ChatScroll::page_up`]).
/// - **PageDown** — History nach unten scrollen (via [`ChatScroll::page_down`]).
/// - Bei offenem Popup: Pfeiltasten/Enter/Esc/Ziffern navigieren das Popup.
///
/// # Argumente
/// - `app` (`&mut ChatApp`): Zustand, der mutiert wird.
/// - `key` (`KeyEvent`): der bereits auf Press/Repeat gefilterte Tastendruck.
/// - `pending_quit` (`&mut Option<QuitArm>`): Scharfstellung des Beenden-Hinweises.
/// - `bus` (`&HarwEventSender`): Kanal, über den [`HarwEvent`]s emittiert werden.
///
/// # Rückgabe
/// `true` wenn ein Redraw angefordert werden soll, sonst `false`.
fn handle_key(
    app: &mut ChatApp,
    key: KeyEvent,
    pending_quit: &mut Option<QuitArm>,
    bus: &HarwEventSender,
) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // ── Globale Steuer-Keys (unabhängig von Popup/Editor-Zustand) ────────

    // Ctrl+C — Doppeldruck beendet (unabhängig vom Eingabeinhalt).
    if ctrl && matches!(key.code, KeyCode::Char('c' | 'C')) {
        if matches!(
            *pending_quit,
            Some(QuitArm {
                label: "Ctrl+C",
                ..
            })
        ) {
            bus.send(HarwEvent::Quit);
            return false;
        }
        *pending_quit = Some(QuitArm {
            label: "Ctrl+C",
            at: Instant::now(),
        });
        return true;
    }

    // Ctrl+D — nur bei leerer Eingabe; Doppeldruck beendet.
    if ctrl && matches!(key.code, KeyCode::Char('d' | 'D')) {
        if app.input.is_empty() {
            if matches!(
                *pending_quit,
                Some(QuitArm {
                    label: "Ctrl+D",
                    ..
                })
            ) {
                bus.send(HarwEvent::Quit);
                return false;
            }
            *pending_quit = Some(QuitArm {
                label: "Ctrl+D",
                at: Instant::now(),
            });
            return true;
        }
        return false;
    }

    // Jede andere Taste macht eine Scharfstellung rückgängig.
    *pending_quit = None;

    // ── ChatScroll konsultieren (PageUp/PageDown/Shift+Up/Shift+Down etc.) ──
    // Echte Werte aus dem letzten `draw_viewport`-Aufruf (vor dem ersten Draw:
    // 0 Zeilen / 20 sichtbar als sicherer Platzhalter) — ChatScroll clamped selbst.
    match app.scroll.handle_key(
        key,
        app.last_history_total_lines() as usize,
        app.last_history_visible_rows() as usize,
    ) {
        ScrollAction::Redraw => return true,
        ScrollAction::Passthrough => {}
    }

    // Ctrl+J — neue Zeile einfügen (schließt ein offenes Popup).
    if ctrl && matches!(key.code, KeyCode::Char('j' | 'J')) {
        app.command_popup = None;
        app.input.insert_newline();
        return true;
    }

    // Enter wird IMMER vor dem Popup behandelt — das Popup ist nur ein
    // Autocomplete-Hinweis und darf das Absenden nicht blockieren. Frühere
    // Version leitete Enter ins Popup, das arg-behaftete Commands (`/model list`)
    // verschluckte (Query matchte keinen Command-Namen → `Stay`) oder die
    // Argumente beim Autocomplete verwarf. Tab akzeptiert jetzt die Auswahl.
    if matches!(key.code, KeyCode::Enter) {
        // Shift/Alt+Enter fügt (wie in vielen TUIs) eine neue Zeile ein.
        if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT)
        {
            app.input.insert_newline();
            app.sync_popup();
            return true;
        }
        // Plain Enter: getippte Zeile absenden, offenes Popup schließen.
        let line = app.input.text().to_owned();
        app.input.clear();
        app.command_popup = None;
        match classify_line(&line) {
            LineAction::Quit => bus.send(HarwEvent::Quit),
            LineAction::Ignore => {}
            LineAction::System(text) => bus.send(HarwEvent::SystemMessage(text)),
            LineAction::Chat(text) => {
                app.remember_input(&line);
                app.scroll.force_follow();
                bus.send(HarwEvent::Submit(text));
            }
            LineAction::Command(raw) => {
                app.remember_input(&line);
                app.scroll.force_follow();
                bus.send(HarwEvent::Command(raw));
            }
        }
        return true;
    }

    // ── Popup-Navigations-Pfad ────────────────────────────────────────────
    if app.has_popup() {
        match key.code {
            // Tab akzeptiert die aktuell markierte Autocomplete-Auswahl.
            KeyCode::Tab => {
                if let Some(name) = app
                    .command_popup
                    .as_ref()
                    .and_then(CommandPopup::selected_name)
                {
                    app.input.clear();
                    app.input.insert_str(&format!("/{name} "));
                }
                app.command_popup = None;
                true
            }
            KeyCode::Up | KeyCode::Down | KeyCode::Esc | KeyCode::Char('1'..='9') => {
                if let Some(popup) = app.command_popup.as_mut() {
                    match popup.on_key(key) {
                        PopupAction::Stay => {}
                        PopupAction::Cancel => {
                            app.command_popup = None;
                        }
                        PopupAction::Accept(name) => {
                            app.input.clear();
                            app.input.insert_str(&format!("/{name} "));
                            app.command_popup = None;
                        }
                    }
                }
                true
            }
            KeyCode::Backspace => {
                app.input.backspace();
                app.sync_popup();
                true
            }
            KeyCode::Char(character) => {
                app.input.insert_char(character);
                app.sync_popup();
                true
            }
            _ => false,
        }
    } else {
        // ── Normaler Eingabe-Pfad (kein Popup aktiv) — InputEditor konsultieren ──
        // Esc während History-Browsing wird vom InputEditor selbst behandelt
        // (cancel_history + Redraw); Esc ohne History-Browsing leert den Puffer.
        if matches!(key.code, KeyCode::Esc) && !app.input.is_empty() {
            app.input.clear();
            app.command_popup = None;
            return true;
        }

        match app.input.handle_key(key) {
            InputAction::Submit(text) => {
                // InputEditor hat den Puffer bereits geleert.
                app.remember_input(&text);
                app.scroll.force_follow();
                app.command_popup = None;
                match classify_line(&text) {
                    LineAction::Quit => bus.send(HarwEvent::Quit),
                    LineAction::Ignore => {}
                    LineAction::System(msg) => bus.send(HarwEvent::SystemMessage(msg)),
                    LineAction::Chat(chat_text) => bus.send(HarwEvent::Submit(chat_text)),
                    LineAction::Command(raw) => bus.send(HarwEvent::Command(raw)),
                }
                true
            }
            InputAction::Redraw => {
                app.sync_popup();
                true
            }
            InputAction::Passthrough => false,
        }
    }
}

/// Treibt genau einen Turn (Spinner animiert) und streamt die Antwort simuliert.
///
/// # Beschreibung
/// Startet den Spinner, treibt den Turn via [`drive_turn_animated`] (dabei
/// animiert der Spinner frame-getaktet), stoppt den Spinner und enthüllt die
/// Antwort dann zeilenweise via [`reveal_reply`]. Die vollständige Antwort wird
/// in den Zustands-Log ([`ChatApp::push_line`]) übernommen.
///
/// # Fehler
/// [`TuiError`] bei Turn-Fehler oder Terminal-I/O.
#[allow(clippy::too_many_arguments)]
async fn run_turn_streaming(
    guard: &mut TerminalGuard,
    app: &mut ChatApp,
    spinner: &mut Spinner,
    gateway: &mut dyn crate::gateway::ChatGateway,
    text: &str,
    approval_driver: &ApprovalDriver,
    approvals: &mut ApprovalPromptReceiver,
    tui_rx: &mut tokio::sync::mpsc::UnboundedReceiver<TuiEvent>,
    turn_event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<TurnEvent>,
    turn_state: &mut TurnEventState,
) -> Result<(), TuiError> {
    // Apply any pending controller mutations (e.g. from `/effort`, `/model` or
    // `/mode`) to the live session before starting the turn. This is the safe
    // turn boundary: the previous turn has completed and the new one has not
    // yet started. `AgentSession::set_mode` re-cuts tool activation and the
    // sandbox ceiling, so it must never happen inside a running turn.
    // See harw-tui Design §session_controller — apply_to_session, AP W5-05.
    app.apply_pending_controller_state(gateway.session_mut());

    spinner.start();
    let reply = drive_turn_animated(
        guard,
        app,
        spinner,
        gateway,
        text,
        approval_driver,
        approvals,
        tui_rx,
        turn_event_rx,
        turn_state,
    )
    .await?;
    spinner.stop();

    reveal_reply(guard, app, &reply).await?;
    Ok(())
}

/// Maximale Wartezeit nach einem Rate-Limit in Sekunden, die automatisch
/// abgewartet wird. Darüber hinaus wird dem User eine manuelle Retry-Bitte
/// angezeigt, aber die Session bleibt geöffnet.
const RATE_LIMIT_AUTO_RETRY_CAP_SECS: u64 = 60;

/// Re-enters the core turn loop without appending the already-persisted user
/// message a second time. A rate-limited turn is returned to `Idle` by core,
/// so an input without `user_text` is the safe retry path for that same turn.
fn rate_limit_retry_input() -> TurnInput {
    TurnInput::default()
}

/// Treibt einen Turn und animiert währenddessen den Spinner.
///
/// # Beschreibung
/// Rennt das `run_turn`-Future gegen einen [`SPINNER_INTERVAL`]-Timer: bei jedem
/// Timer-Tick wird der Spinner weitergeschaltet und die Inline-Viewport neu
/// gezeichnet, bis der Turn abgeschlossen ist. Extrahiert danach die letzte
/// Assistant-Antwort aus der Session-Historie.
///
/// Wenn der Provider HTTP 429 zurückgibt ([`ModelError::RateLimited`]), wird
/// einmalig automatisch gewartet (`retry_after_secs`, max
/// [`RATE_LIMIT_AUTO_RETRY_CAP_SECS`]) und ein Retry versucht. Die Chat-Session
/// wird dabei **nicht** beendet:
/// - Retry erfolgreich → Antwort wie gewohnt.
/// - Retry erneut rate-limitiert → `Ok("⏱ Rate limit — please retry in Ns")`;
///   der User kann erneut senden.
/// - Retry mit anderem Fehler → `Err(TuiError::Core(...))` (normaler Fehlerfall).
///
/// # Freigaben und Kind-Wiederaufnahme (AP W5-03)
/// Beide Ausgänge — der Erstversuch **und** der Rate-Limit-Retry — münden in
/// dasselbe `outcome` und laufen anschließend durch
/// [`ApprovalDriver::drive_to_completion`]. Der Treiber liefert immer
/// [`TurnOutcome::Completed`]; erst danach wird die letzte Assistant-Antwort
/// gezogen. Der frühere Rückgabewert „pausiert (noch nicht unterstützt)" — der
/// jeden `fs.write` und jeden `shell.exec` in der TUI unabschließbar machte —
/// existiert nicht mehr.
///
/// Während der Treiber läuft, pollt diese Funktion in **einem** `select!` neben
/// dem [`SPINNER_INTERVAL`]-Timer:
/// - `approvals` — jede eintreffende Frage wird als [`ApprovalPromptCell`]
///   angezeigt. Ohne dieses Pollen liefe jede Frage in den Timeout und würde
///   damit zur Ablehnung.
/// - `tui_rx` — **nur solange eine Frage offen ist**: `y` gibt frei, `n`/`Esc`/
///   `Ctrl+C` lehnt ab. Ohne offene Frage bleibt der Zweig deaktiviert, damit
///   während eines Turns getippte Zeichen wie bisher im Kanal warten statt
///   verworfen zu werden.
/// - `turn_event_rx` — Werkzeug-, Kind- und Plan-Zellen erscheinen dadurch
///   **während** des Turns statt erst danach.
///
/// # Fehler
/// [`TuiError::Core`], wenn der Turn fehlschlägt (nicht durch Rate-Limit), der
/// Freigabetreiber scheitert oder keine Antwort vorliegt; [`TuiError::Io`] beim
/// Zeichnen.
#[allow(clippy::too_many_arguments)]
async fn drive_turn_animated(
    guard: &mut TerminalGuard,
    app: &mut ChatApp,
    spinner: &mut Spinner,
    gateway: &mut dyn crate::gateway::ChatGateway,
    text: &str,
    approval_driver: &ApprovalDriver,
    approvals: &mut ApprovalPromptReceiver,
    tui_rx: &mut tokio::sync::mpsc::UnboundedReceiver<TuiEvent>,
    turn_event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<TurnEvent>,
    turn_state: &mut TurnEventState,
) -> Result<String, TuiError> {
    // Einmal zeichnen, damit der Spinner sofort erscheint.
    draw_viewport(guard, app, spinner, None)?;

    // Erster Versuch — Turn-Future in einen Block scopen, damit der
    // `&mut session`-Borrow freigegeben wird, bevor wir `session.history()` lesen.
    let mut input_open = true;
    let first_result = {
        let (session, store, model) = gateway.borrow_turn_ctx();
        let turn = run_turn(session, model, store, TurnInput::user(text));
        tokio::pin!(turn);
        loop {
            tokio::select! {
                result = &mut turn => break result,
                event = tui_rx.recv(), if input_open => {
                    match event {
                        Some(event) => {
                            if handle_busy_event(app, event) {
                                draw_viewport(guard, app, spinner, None)?;
                            }
                        }
                        None => input_open = false,
                    }
                }
                // Werkzeug-, Kind- und Plan-Zellen erscheinen dadurch bereits
                // während des Turns statt erst nach seinem Ende.
                maybe_turn_event = turn_event_rx.recv() => {
                    if let Some(event) = maybe_turn_event {
                        if handle_turn_event(app, turn_state, event) {
                            draw_viewport(guard, app, spinner, None)?;
                        }
                    }
                }
                _ = tokio::time::sleep(SPINNER_INTERVAL) => {
                    spinner.tick();
                    draw_viewport(guard, app, spinner, None)?;
                }
            }
        }
    };

    // Rate-Limit-Behandlung: einmaliger automatischer Retry mit Backoff.
    let outcome = match first_result {
        Err(CoreError::Model(ModelError::RateLimited {
            retry_after_secs, ..
        })) => {
            let wait_secs = retry_after_secs.min(RATE_LIMIT_AUTO_RETRY_CAP_SECS);
            // Warten — Spinner läuft weiter, Session bleibt offen.
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(wait_secs);
            while tokio::time::Instant::now() < deadline {
                tokio::time::sleep(SPINNER_INTERVAL).await;
                spinner.tick();
                draw_viewport(guard, app, spinner, None)?;
            }
            // Zweiter Versuch nach Backoff.
            let retry_result = {
                let (session, store, model) = gateway.borrow_turn_ctx();
                let turn = run_turn(session, model, store, rate_limit_retry_input());
                tokio::pin!(turn);
                loop {
                    tokio::select! {
                            result = &mut turn => break result,
                    event = tui_rx.recv(), if input_open => {
                        match event {
                            Some(event) => {
                                if handle_busy_event(app, event) {
                                    draw_viewport(guard, app, spinner, None)?;
                                }
                            }
                            None => input_open = false,
                        }
                    }
                            maybe_turn_event = turn_event_rx.recv() => {
                                if let Some(event) = maybe_turn_event {
                                    if handle_turn_event(app, turn_state, event) {
                                        draw_viewport(guard, app, spinner, None)?;
                                    }
                                }
                            }
                            _ = tokio::time::sleep(SPINNER_INTERVAL) => {
                                spinner.tick();
                                draw_viewport(guard, app, spinner, None)?;
                            }
                        }
                }
            };
            match retry_result {
                // Noch immer rate-limitiert → Session am Leben lassen, User informieren.
                Err(CoreError::Model(ModelError::RateLimited {
                    retry_after_secs, ..
                })) => {
                    return Ok(format!(
                        "⏱ Rate limit — please retry in {}s",
                        retry_after_secs
                    ));
                }
                // Anderer Fehler oder Erfolg nach Retry.
                other => other.map_err(|error| TuiError::Core(error.to_string()))?,
            }
        }
        // Kein Rate-Limit: normaler Fehler oder Erfolg.
        other => other.map_err(|error| TuiError::Core(error.to_string()))?,
    };

    // AP W5-03: Beide Ausgänge oben (Erstversuch und Rate-Limit-Retry) landen
    // in `outcome` und werden hier gemeinsam abgearbeitet. `drive_to_completion`
    // liefert immer `Completed`; jede Pause wird entweder beantwortet oder zum
    // typisierten Fehler.
    if !matches!(outcome, TurnOutcome::Completed) {
        drive_pauses_to_completion(
            guard,
            app,
            spinner,
            gateway,
            outcome,
            approval_driver,
            approvals,
            tui_rx,
            turn_event_rx,
            turn_state,
        )
        .await?;
    }

    // Nachdem der Turn-Future gedroppt ist, ist der `&mut` auf die Session
    // frei — wir dürfen sie erneut ausleihen, um die letzte Assistant-Antwort
    // zu extrahieren.
    gateway
        .session_mut()
        .history()
        .to_model_messages()
        .into_iter()
        .rev()
        .find_map(|message| match message {
            ModelMessage::Assistant { text } => Some(text),
            _ => None,
        })
        .ok_or_else(|| TuiError::Core("Modell lieferte keine Assistant-Antwort".to_owned()))
}

/// Was ein Tastendruck mit einer offenen Freigabefrage macht.
///
/// # Beschreibung
/// AP W5-03. Bewusst als reine, terminalfreie Klassifikation ausgelagert, damit
/// die Tastenbelegung ohne TTY testbar ist. Es gibt **keinen** Wert, der eine
/// Freigabe aus etwas anderem als einem ausdrücklichen `y` macht.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovalKeyAction {
    /// Ausdrückliche Freigabe (`y` / `Y`).
    Approve,
    /// Ablehnung mit fester Begründung (`n` / `N`, `Esc`, `Ctrl+C`).
    Reject(&'static str),
    /// Taste ohne Bedeutung — die Frage bleibt offen.
    Ignore,
}

/// Klassifiziert einen Tastendruck gegen eine offene Freigabefrage.
///
/// # Argumente
/// - `key` ([`KeyEvent`]): der bereits auf Press/Repeat gefilterte Tastendruck.
///
/// # Rückgabe
/// [`ApprovalKeyAction::Approve`] **nur** für `y`/`Y`;
/// [`ApprovalKeyAction::Reject`] für `n`/`N`, `Esc` und `Ctrl+C`; sonst
/// [`ApprovalKeyAction::Ignore`].
fn classify_approval_key(key: KeyEvent) -> ApprovalKeyAction {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return match key.code {
            KeyCode::Char('c' | 'C') => ApprovalKeyAction::Reject(REASON_OPERATOR_CANCELLED),
            _ => ApprovalKeyAction::Ignore,
        };
    }
    match key.code {
        KeyCode::Char('y' | 'Y') => ApprovalKeyAction::Approve,
        KeyCode::Char('n' | 'N') => ApprovalKeyAction::Reject(REASON_OPERATOR_REJECTED),
        KeyCode::Esc => ApprovalKeyAction::Reject(REASON_OPERATOR_CANCELLED),
        _ => ApprovalKeyAction::Ignore,
    }
}

/// Eine im Verlauf sichtbare, noch unbeantwortete Freigabefrage.
///
/// # Beschreibung
/// Hält die Frage und die **eine** [`ApprovalPromptCell`], die sie anzeigt,
/// zusammen — damit die Antwort und die Anzeige nicht auseinanderlaufen können.
struct PendingApprovalPrompt {
    /// Die noch unbeantwortete Frage; wird beim Beantworten konsumiert.
    prompt: ApprovalPrompt,
    /// Die zugehörige, geteilte Verlaufszelle.
    cell: Arc<Mutex<ApprovalPromptCell>>,
}

impl PendingApprovalPrompt {
    /// Beantwortet die Frage und zieht die Zelle nach.
    ///
    /// # Beschreibung
    /// Die Zelle bekommt `true` nur, wenn ausdrücklich freigegeben **und** die
    /// Antwort auch zugestellt wurde. Konnte eine Freigabe nicht zugestellt
    /// werden, wartet niemand mehr darauf — sie als „freigegeben" anzuzeigen
    /// wäre falsch.
    ///
    /// # Argumente
    /// - `approved` (`bool`): `true` nur bei ausdrücklicher Freigabe.
    /// - `delivered` (`bool`): ob die Antwort den wartenden Treiber erreicht hat.
    fn record_decision(cell: &Arc<Mutex<ApprovalPromptCell>>, approved: bool, delivered: bool) {
        match cell.lock() {
            Ok(mut cell) => cell.apply_decision(approved && delivered),
            Err(_) => tracing::error!("tui.approval.cell_lock_poisoned"),
        }
    }

    /// Gibt den Werkzeugaufruf ausdrücklich frei.
    fn approve(self) {
        let delivered = self.prompt.approve();
        Self::record_decision(&self.cell, true, delivered);
    }

    /// Lehnt den Werkzeugaufruf mit Begründung ab.
    ///
    /// # Argumente
    /// - `reason` (`&str`): Begründung, die als Werkzeugergebnis in den
    ///   Modellverlauf wandert.
    fn reject(self, reason: &str) {
        let delivered = self.prompt.reject(reason.to_owned());
        Self::record_decision(&self.cell, false, delivered);
    }
}

/// Arbeitet jede Pause eines Turns ab, während der Renderer weiterläuft.
///
/// # Beschreibung
/// AP W5-03 — die Gegenseite zum früheren „pausiert (noch nicht unterstützt)".
/// Übergibt `outcome` an [`ApprovalDriver::drive_to_completion`] und pollt
/// dessen Future gemeinsam mit dem Fragekanal, der Tastatur (nur bei offener
/// Frage), den Turn-Ereignissen und dem Spinner-Timer.
///
/// Der Kind-Treiber wird **vor** dem Anlegen des Futures als `Arc` aus `app`
/// herausgezogen: sonst hielte das Future eine unveränderliche Leihe auf `app`
/// und die Schleife könnte keine Zellen mehr anhängen.
///
/// # Argumente
/// - `outcome` ([`TurnOutcome`]): die gemeldete Pause (nie `Completed`).
/// - `approval_driver` (`&ApprovalDriver`): Treiber über beide Pausearten.
/// - `approvals` (`&mut ApprovalPromptReceiver`): Fragekanal desselben Handlers.
/// - `tui_rx` / `turn_event_rx` / `turn_state`: siehe [`drive_turn_animated`].
///
/// # Rückgabe
/// `Ok(())`, sobald der Turn [`TurnOutcome::Completed`] erreicht hat.
///
/// # Fehler
/// [`TuiError::Core`] aus [`tui_error_from_approval_driver`], wenn der Kern eine
/// Wiederaufnahme ablehnt, der Pausezustand fehlt oder die Abbruchgrenze greift;
/// [`TuiError::Io`] beim Zeichnen.
///
/// # Nebenläufigkeit
/// Single-task auf dem `current_thread`-Runtime; hält keinen Lock über einen
/// `await`-Punkt.
#[allow(clippy::too_many_arguments)]
async fn drive_pauses_to_completion(
    guard: &mut TerminalGuard,
    app: &mut ChatApp,
    spinner: &mut Spinner,
    gateway: &mut dyn crate::gateway::ChatGateway,
    outcome: TurnOutcome,
    approval_driver: &ApprovalDriver,
    approvals: &mut ApprovalPromptReceiver,
    tui_rx: &mut tokio::sync::mpsc::UnboundedReceiver<TuiEvent>,
    turn_event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<TurnEvent>,
    turn_state: &mut TurnEventState,
) -> Result<(), TuiError> {
    // Bedingung 4: `None` ist zulässig und beantwortet jeden Handoff mit einem
    // Fehlerergebnis — der Eltern-Turn endet trotzdem regulär.
    let spawner = app.managed_spawner().map(Arc::clone);
    let children: Option<&dyn ChildTurnDriver> = match spawner.as_deref() {
        Some(driver) => Some(driver),
        None => None,
    };

    let mut pending: Option<PendingApprovalPrompt> = None;
    let mut approvals_open = true;
    let mut input_open = true;

    let (session, store, model) = gateway.borrow_turn_ctx();
    let drive = approval_driver.drive_to_completion(session, model, store, children, outcome);
    tokio::pin!(drive);

    loop {
        tokio::select! {
            result = &mut drive => {
                let finished = result.map_err(tui_error_from_approval_driver)?;
                debug_assert!(
                    matches!(finished, TurnOutcome::Completed),
                    "drive_to_completion must only ever return Completed"
                );
                // Eine noch offene Frage nach Turn-Ende: Ablehnung ist der
                // Default, und die Zelle darf nicht als Frage stehenbleiben.
                if let Some(open) = pending.take() {
                    open.reject(REASON_OPERATOR_CANCELLED);
                    draw_viewport(guard, app, spinner, None)?;
                }
                return Ok(());
            }
            maybe_prompt = approvals.recv(), if approvals_open => {
                match maybe_prompt {
                    Some(prompt) => {
                        tracing::info!(
                            tool = prompt.tool_name(),
                            request = %prompt.request(),
                            "tui.approval.prompt_shown"
                        );
                        let cell = Arc::new(Mutex::new(ApprovalPromptCell {
                            tool_name: prompt.tool_name().to_owned(),
                            arguments_raw: prompt.arguments_json(),
                            decision: None,
                        }));
                        app.push_shared_cell(Arc::clone(&cell));
                        // Eine zuvor offene Frage kann es nicht geben: der
                        // Treiber stellt sie streng nacheinander.
                        pending = Some(PendingApprovalPrompt { prompt, cell });
                        draw_viewport(guard, app, spinner, None)?;
                    }
                    None => {
                        tracing::warn!("tui.approval.prompt_channel_ended");
                        approvals_open = false;
                    }
                }
            }
            maybe_event = tui_rx.recv(), if input_open => {
                match maybe_event {
                    Some(event) if pending.is_none() => {
                        if handle_busy_event(app, event) {
                            draw_viewport(guard, app, spinner, None)?;
                        }
                    }
                    Some(TuiEvent::Key(key)) => {
                        if app.scroll.handle_key(key, app.last_history_total_lines() as usize,
                            app.last_history_visible_rows() as usize) == ScrollAction::Redraw {
                            draw_viewport(guard, app, spinner, None)?;
                            continue;
                        }
                        match classify_approval_key(key) {
                            ApprovalKeyAction::Ignore => {}
                            ApprovalKeyAction::Approve => {
                                if let Some(open) = pending.take() {
                                    open.approve();
                                }
                                draw_viewport(guard, app, spinner, None)?;
                            }
                            ApprovalKeyAction::Reject(reason) => {
                                if let Some(open) = pending.take() {
                                    open.reject(reason);
                                }
                                draw_viewport(guard, app, spinner, None)?;
                            }
                        }
                    }
                    Some(TuiEvent::Draw) | Some(TuiEvent::Resize(_, _)) => {
                        draw_viewport(guard, app, spinner, None)?;
                    }
                    Some(TuiEvent::Mouse(mouse)) => {
                        if handle_busy_event(app, TuiEvent::Mouse(mouse)) {
                            draw_viewport(guard, app, spinner, None)?;
                        }
                    }
                    // Pasted text never answers an approval prompt.
                    Some(TuiEvent::Paste(_)) => {}
                    None => {
                        // Kein Terminal mehr — Ablehnung ist der Default.
                        tracing::warn!("tui.approval.input_channel_ended");
                        input_open = false;
                        if let Some(open) = pending.take() {
                            open.reject(REASON_OPERATOR_CANCELLED);
                        }
                    }
                }
            }
            maybe_turn_event = turn_event_rx.recv() => {
                if let Some(event) = maybe_turn_event {
                    if handle_turn_event(app, turn_state, event) {
                        draw_viewport(guard, app, spinner, None)?;
                    }
                }
            }
            _ = tokio::time::sleep(SPINNER_INTERVAL) => {
                spinner.tick();
                draw_viewport(guard, app, spinner, None)?;
            }
        }
    }
}

/// Processes navigation immediately while preserving typing for the next prompt.
fn handle_busy_event(app: &mut ChatApp, event: TuiEvent) -> bool {
    let total = app.last_history_total_lines() as usize;
    let rows = app.last_history_visible_rows() as usize;
    match event {
        TuiEvent::Mouse(mouse) => {
            app.scroll.handle_mouse(mouse, total, rows) == ScrollAction::Redraw
        }
        TuiEvent::Key(key) if app.scroll.handle_key(key, total, rows) == ScrollAction::Redraw => {
            true
        }
        TuiEvent::Draw | TuiEvent::Resize(_, _) => true,
        event => {
            app.deferred_input.push_back(event);
            false
        }
    }
}

/// Enthüllt die Antwort simuliert gestreamt, zeilenweise via [`StreamCollector`].
///
/// # Beschreibung
/// Da [`ModelProvider`] keine Token-Deltas liefert, wird der Volltext hier in
/// kleinen Häppchen ([`REVEAL_CHUNK_CHARS`]) durch einen [`StreamCollector`]
/// geschoben. Sobald eine vollständige Zeile vorliegt, wird sie als
/// [`AssistantHistoryCell`] in `app.cells` gepusht und ein Frame gezeichnet.
/// Am Ende wird der verbleibende Rest (ohne abschließendes `\n`) ausgeliefert.
/// Die vollständige Antwort wird abschließend in [`ChatApp::push_line`] übernommen.
///
/// # Argumente
/// - `guard` (`&mut TerminalGuard`): Terminal-Guard zum Zeichnen der Frames.
/// - `app` (`&mut ChatApp`): Chat-Zustand; erhält die Stream-Fragmente und die finale Zelle.
/// - `reply` (`&str`): die vollständige Modell-Antwort.
///
/// # Fehler
/// [`TuiError::Io`] beim Zeichnen.
async fn reveal_reply(
    guard: &mut TerminalGuard,
    app: &mut ChatApp,
    reply: &str,
) -> Result<(), TuiError> {
    // Vollständige Antwort in genau EINE finalisierte Zelle. Kein Chunk-Push
    // mehr — der frühere Streaming-Preview hat für jede Zeile eine eigene
    // Fragment-Zelle erzeugt und danach die Gesamtantwort noch einmal
    // gepusht, was zu doppelten Antworten im Chat führte.
    app.push_line(Role::Assistant, reply);
    draw_viewport(guard, app, &Spinner::new(), None)?;
    Ok(())
}

/// Zeichnet die Fullscreen-Viewport: History (scrollbar) oben, Status mitte, Eingabebox unten.
///
/// # Beschreibung
/// Dreiteiliges Layout (Direction::Vertical):
/// - **History** (`Constraint::Min(3)`): alle `cells` als scrollbarer `Paragraph`.
///   `app.scroll_offset` bestimmt den Abstand vom Ende (0 = unten). PageUp/PageDown
///   steuern diesen Offset via [`handle_key`].
/// - **Status** (`Constraint::Length(1)`): animierter Spinner, Beenden-Hinweis oder
///   Standard-Tastenlegende.
/// - **Eingabe** (`Constraint::Length(3–6)`): Mehrzeilige Eingabe mit `› `-Präfix und
///   Rahmen. Höhe passt sich der Zeilenanzahl an (Clamp 3–6).
///
/// Ein offenes, nicht leeres `/command`-Popup überlagert den unteren Teil des History-
/// Bereichs, direkt oberhalb der Statuszeile.
///
/// # Argumente
/// - `guard` (`&mut TerminalGuard`): Terminal-Guard.
/// - `app` (`&ChatApp`): aktueller Zustand (Eingabe, History, Theme, Popup, Scroll-Offset).
/// - `spinner` (`&Spinner`): Spinner-Zustand für die Statuszeile.
/// - `quit_hint` (`Option<&str>`): Label der scharfgestellten Beenden-Taste.
///
/// # Fehler
/// [`TuiError::Io`] beim Zeichnen.
fn draw_viewport(
    guard: &mut TerminalGuard,
    app: &ChatApp,
    spinner: &Spinner,
    quit_hint: Option<&str>,
) -> Result<(), TuiError> {
    guard
        .terminal()
        .draw(|frame| render_viewport(frame, app, spinner, quit_hint))
        .map_err(TuiError::from)?;
    Ok(())
}

/// Renders the same viewport on a real terminal or a test backend.
fn render_viewport(
    frame: &mut ratatui::Frame<'_>,
    app: &ChatApp,
    spinner: &Spinner,
    quit_hint: Option<&str>,
) {
    let theme = app.theme;
    let area = frame.area();

    // Eingabehöhe wächst mit der Zeilenanzahl, begrenzt auf 3–10.
    // Rahmen (2 Spalten) und das `"› "`-Präfix (2 Spalten) gehen von der
    // nutzbaren Textbreite ab; eine weitere Spalte bleibt für den Cursor frei.
    // Höhe und Cursor-Position müssen mit
    // derselben Breite rechnen, sonst laufen sie auseinander.
    let input_width = (area.width.saturating_sub(5)) as usize;
    let input_line_count = app.input.visible_lines(input_width).len().clamp(1, 8) as u16;
    let input_height = input_line_count + 2;

    // Dreiteiliges vertikales Layout: History | Eingabe | Status.
    // Status kommt bewusst UNTER die Eingabebox — dort erwartet das Auge
    // Fußzeilen-Hinweise, ohne den Sichtabstand zwischen Chat und
    // Eingabefeld zu vergrößern.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(input_height),
            Constraint::Length(1),
        ])
        .split(area);
    let history_area = chunks[0];
    let input_area = chunks[1];
    let status_area = chunks[2];

    // ── History ──────────────────────────────────────────────────────
    // Alle Zellen zu einem flachen Zeilen-Vec zusammenführen.
    let width = history_area.width;
    let all_lines: Vec<Line<'static>> = app
        .cells
        .iter()
        .flat_map(|cell| cell.display_lines(width, theme))
        .collect();

    // Scroll-Offset: 0 = ganz unten; wächst nach oben.
    // Gesamtzeilen → sichtbaren Bereich berechnen → Paragraph.scroll() aufrufen.
    // Die gerenderte Zeilenzahl ist nicht die Zahl der `Line`-Objekte:
    // `Wrap` bricht lange Zeilen zusätzlich um. Würde der Scroll-Offset
    // gegen die ungewrappte Zahl geclamped, bliebe der obere Teil langer
    // Ausgaben (etwa `/help`) unerreichbar.
    let history_widget = Paragraph::new(all_lines).wrap(Wrap { trim: false });
    let total_lines = u16::try_from(history_widget.line_count(width)).unwrap_or(u16::MAX);
    let visible_rows = history_area.height;
    // Für PageUp/PageDown/Home/End außerhalb des Draw-Closures cachen
    // (`handle_key` hat keinen Zugriff auf `frame.area()`).
    app.last_history_total_lines.set(total_lines);
    app.last_history_visible_rows.set(visible_rows);
    // Rohes Offset (in Zeilen vom Anfang gesehen).
    let scroll_from_top: u16 = if total_lines > visible_rows {
        let max_offset = total_lines - visible_rows;
        // scroll.offset() zählt vom Ende → in „von oben" umrechnen.
        let back = app.scroll.offset() as u16;
        max_offset.saturating_sub(back)
    } else {
        0
    };

    frame.render_widget(history_widget.scroll((scroll_from_top, 0)), history_area);

    // ── Status ───────────────────────────────────────────────────────
    // Popup überlagert den unteren Teil des History-Bereichs (falls offen).
    let popup_open = app
        .command_popup
        .as_ref()
        .is_some_and(|popup| !popup.is_empty());
    if popup_open {
        if let Some(popup) = app.command_popup.as_ref() {
            let popup_height = POPUP_MAX_ROWS.min(history_area.height);
            if popup_height > 0 {
                let popup_area = Rect::new(
                    history_area.x,
                    history_area.y + history_area.height - popup_height,
                    history_area.width,
                    popup_height,
                );
                // Ohne `Clear` bleibt der Chat-Text unter dem Popup
                // stehen: die Zellen, die das Popup nicht selbst
                // überschreibt, behalten ihren alten Inhalt, und beide
                // Texte lesen sich ineinander verschachtelt.
                frame.render_widget(Clear, popup_area);
                popup.render(popup_area, frame.buffer_mut(), theme);
            }
        }
    }
    let sl = status_line(
        theme,
        spinner,
        quit_hint,
        &app.total_usage,
        app.active_mode(),
    );
    frame.render_widget(Paragraph::new(sl), status_area);

    // ── Eingabe ──────────────────────────────────────────────────────
    let mut input_lines: Vec<Line<'static>> = Vec::new();
    for (index, segment) in app.input.visible_lines(input_width).iter().enumerate() {
        let prefix = if index == 0 { "› " } else { "  " };
        input_lines.push(Line::from(format!("{prefix}{segment}")));
    }
    let (cursor_row, cursor_col) = app.input.cursor_position(input_width);
    let input_rows = input_area.height.saturating_sub(2) as usize;
    let input_top = cursor_row.saturating_sub(input_rows.saturating_sub(1));
    let input_widget = Paragraph::new(input_lines.into_iter().skip(input_top).collect::<Vec<_>>())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(" harw ", style::selected_style(theme)))
                .border_style(Style::default().fg(style::border_color(theme))),
        );
    frame.render_widget(input_widget, input_area);

    // Terminal-Cursor auf die Schreibposition setzen. Ohne diesen Aufruf
    // blendet ratatui den Cursor für den Frame aus, und der Schreibende
    // sieht nicht, wo das nächste Zeichen landet.
    let max_x = input_area.x + input_area.width.saturating_sub(2);
    let max_y = input_area.y + input_area.height.saturating_sub(2);
    let cursor_x = input_area
        .x
        .saturating_add(3)
        .saturating_add(u16::try_from(cursor_col).unwrap_or(u16::MAX))
        .min(max_x);
    let cursor_y = input_area
        .y
        .saturating_add(1)
        .saturating_add(u16::try_from(cursor_row.saturating_sub(input_top)).unwrap_or(u16::MAX))
        .min(max_y);
    frame.set_cursor_position((cursor_x, cursor_y));
}

/// Formatiert eine Token-Zahl kompakt (`999` → `"999"`, `1234` → `"1.2k"`).
///
/// # Beschreibung
/// Werte unter 1000 werden unverändert als Dezimalzahl ausgegeben; ab 1000
/// wird auf eine Nachkommastelle in Tausendern gerundet (z.B. `1234` → `1.2k`).
/// Genutzt von [`status_line`] für den kompakten Token-Nutzungs-Suffix.
///
/// # Argumente
/// - `n` (`u64`): die zu formatierende Token-Anzahl.
///
/// # Rückgabe
/// Kompakte, menschenlesbare Darstellung als `String`.
fn format_tokens_compact(n: u64) -> String {
    if n < 1000 {
        n.to_string()
    } else {
        format!("{:.1}k", n as f64 / 1000.0)
    }
}

/// Baut die Statuszeile für die Fullscreen-Viewport.
///
/// # Beschreibung
/// Priorität der Inhalte (höchste zuerst):
/// 1. Laufender Turn → animierter Spinner-Glyph + „denkt…" + Modus-Anzeige.
/// 2. Scharfgestelltes Beenden → gelber Hinweis mit dem Taste-Label.
/// 3. Standard-Tastenlegende (Enter, Ctrl+J, Ctrl+C/D) — gefolgt vom aktiven
///    Interaktionsmodus (AP W5-05) und, bei `total_usage.total() > 0`, einem
///    kompakten Token-Nutzungs-Suffix (z. B.
///    `" · 1.2k Tokens (0.9k in + 0.3k out)"`), formatiert über
///    [`format_tokens_compact`].
///
/// # Argumente
/// - `theme` ([`style::Theme`]): aktives Farbschema für Spinner und Legende.
/// - `spinner` (`&Spinner`): Spinner-Zustand; `is_active()` und `glyph()` werden
///   abgefragt.
/// - `quit_hint` (`Option<&str>`): wenn `Some(label)`, wird der Beenden-Hinweis
///   mit dem Label angezeigt (z. B. `"Ctrl+C"`).
/// - `total_usage` (`&TokenUsage`): über die Session-Laufzeit aufsummierte
///   Token-Nutzung; nur im Standard-Legenden-Zweig als Suffix sichtbar.
/// - `mode` ([`InteractionMode`]): der zuletzt an einer Turn-Grenze angewendete
///   Interaktionsmodus.
///
/// # Rückgabe
/// Eine fertig gestaltete [`ratatui::text::Line`] mit Lebensdauer `'static`.
fn status_line(
    theme: style::Theme,
    spinner: &Spinner,
    quit_hint: Option<&str>,
    total_usage: &TokenUsage,
    mode: InteractionMode,
) -> Line<'static> {
    if spinner.is_active() {
        Line::from(vec![
            Span::styled(
                format!("{} ", spinner.glyph()),
                style::selected_style(theme),
            ),
            Span::styled("denkt…", style::dim_style(theme)),
            Span::styled(
                format!(" · Modus: {} ", mode.as_str()),
                style::dim_style(theme),
            ),
        ])
    } else if let Some(label) = quit_hint {
        Line::from(Span::styled(
            format!(" {label} erneut drücken zum Beenden "),
            style::warning_style(theme),
        ))
    } else {
        let mut text =
            " Enter: senden · Ctrl+J: neue Zeile · Ctrl+C 2× / Ctrl+D: beenden ".to_owned();
        text.push_str(&format!("· Modus: {} ", mode.as_str()));
        if total_usage.total() > 0 {
            text.push_str(&format!(
                "· {} Tokens ({} in + {} out) ",
                format_tokens_compact(total_usage.total()),
                format_tokens_compact(total_usage.input_tokens),
                format_tokens_compact(total_usage.output_tokens),
            ));
        }
        Line::from(Span::styled(text, style::dim_style(theme)))
    }
}

/// Fehler des TUI-Chat-Renderers.
///
/// # Beschreibung
/// Handgeschriebener Fehler-Enum (kein `anyhow`/`thiserror`) mit `From`-Impl für
/// [`std::io::Error`], damit `?` an I/O-Grenzen funktioniert. Beide Varianten
/// tragen alle Informationen, die zum Verstehen des Fehlers nötig sind.
///
/// # Fehler-Varianten
/// - [`TuiError::Io`] — bei Terminal-Setup (Raw-Mode, Bracketed-Paste,
///   Inline-Viewport), beim Zeichnen oder bei der Event-Verarbeitung.
/// - [`TuiError::Core`] — wenn der Turn-Loop einen Fehler liefert oder keine
///   Assistant-Antwort in der Session-Historie vorhanden ist.
///
/// # Beispiele
/// ```ignore
/// use harw_tui::app::TuiError;
/// let err = TuiError::Core("Modell antwortet nicht".to_owned());
/// assert!(err.to_string().contains("chat turn failed"));
/// ```
pub enum TuiError {
    /// I/O-Fehler bei Terminal-Setup, Zeichnen oder Event-Verarbeitung.
    Io(io::Error),
    /// Vom Core/Turn-Loop gemeldeter Fehler (als Text übernommen).
    Core(String),
    /// Ein Kontextanbieter deklariert einen leeren oder bereits vergebenen
    /// Namensraum beim Zusammenbau der Registry
    /// ([`registry_with_approval_handler`], [`registry_with_plan_contributions`]).
    ContextProviderRegistration(ContextProviderRegistrationError),
}

/// Menschenlesbare Darstellung; enthält weder interne Feldnamen noch Rust-Typen.
impl std::fmt::Display for TuiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TuiError::Io(error) => write!(formatter, "terminal I/O failed: {error}"),
            TuiError::Core(message) => write!(formatter, "chat turn failed: {message}"),
            TuiError::ContextProviderRegistration(error) => {
                write!(formatter, "context provider registration failed: {error}")
            }
        }
    }
}

/// Debug delegiert an [`std::fmt::Display`], um eine einzige Formatierung zu halten.
impl std::fmt::Debug for TuiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Debug delegiert an Display, um eine einzige Formatierung zu halten.
        std::fmt::Display::fmt(self, formatter)
    }
}

/// Implementiert `std::error::Error`; `source()` liefert den eingebetteten I/O-Fehler.
impl std::error::Error for TuiError {
    /// Gibt den eingebetteten [`std::io::Error`] zurück, falls vorhanden.
    ///
    /// # Rückgabe
    /// - `Some(&io::Error)` für [`TuiError::Io`].
    /// - `None` für [`TuiError::Core`].
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TuiError::Io(error) => Some(error),
            TuiError::Core(_) => None,
            TuiError::ContextProviderRegistration(error) => Some(error),
        }
    }
}

/// Konvertiert [`std::io::Error`] in [`TuiError::Io`], sodass `?` an I/O-Grenzen
/// funktioniert.
impl From<io::Error> for TuiError {
    fn from(error: io::Error) -> Self {
        TuiError::Io(error)
    }
}

/// Konvertiert [`ContextProviderRegistrationError`] in
/// [`TuiError::ContextProviderRegistration`], sodass `?` beim Registry-Umbau
/// funktioniert.
impl From<ContextProviderRegistrationError> for TuiError {
    fn from(error: ContextProviderRegistrationError) -> Self {
        TuiError::ContextProviderRegistration(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use harw_core::{
        ApprovalResolution, InMemoryStateStore, ModelFuture, ModelRequest, ModelResponse,
    };
    use harw_extension_api::{
        AgentSpawner, ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture, ToolName,
        ToolOutput, ToolProvider, ToolSpec,
    };
    use harw_operations::SessionController;
    use harw_tools::serde_json::{Value, json};
    use harw_tools::{FunctionToolSpec, JsonSchema};
    use harw_types::{ItemId, ToolCallId, TurnId};

    /// Baut eine gültige Test-`SandboxSpec` gegen ein eindeutiges Temp-Verzeichnis
    /// (Muster übernommen aus `harw-operations/src/adapter/command.rs`).
    fn test_sandbox() -> SandboxSpec {
        use harw_sandbox::{Permission, PermissionSet, WorkspaceRegistration, WorkspaceRegistry};
        use harw_types::{TenantId, WorkspaceId};

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-tui-app-test-{}-{}", std::process::id(), id));
        std::fs::create_dir_all(root.join("workspace")).expect("temp workspace dir");
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("tui-test"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )
        .expect("workspace registry build");
        let binding = registry
            .resolve(
                &TenantId::from_str("tui-test"),
                &WorkspaceId::from_str("workspace"),
            )
            .expect("resolve workspace binding");
        SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace, Permission::WriteWorkspace]),
        )
    }

    /// Baut einen `ChatApp`-Testzustand mit leerer Adapter-Pipeline und einer
    /// frischen Test-Sandbox. Für Tests, die nur Verlauf/Popup/Scroll prüfen
    /// (nicht die Command-Adapter-Pipeline selbst — dafür siehe
    /// `command_exec.rs`).
    fn test_chat_app() -> ChatApp {
        ChatApp::new(Vec::new(), test_sandbox(), SessionId::new())
    }

    #[test]
    fn busy_turn_scrolls_immediately_and_preserves_typed_input_in_order() {
        let mut app = test_chat_app();
        app.last_history_total_lines.set(100);
        app.last_history_visible_rows.set(10);
        let typed = TuiEvent::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        let pasted = TuiEvent::Paste("next prompt".to_owned());
        assert!(!handle_busy_event(&mut app, typed.clone()));
        assert!(handle_busy_event(
            &mut app,
            TuiEvent::Key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))
        ));
        assert!(app.scroll.offset() > 0);
        assert!(!handle_busy_event(&mut app, pasted.clone()));
        assert_eq!(app.deferred_input.pop_front(), Some(typed));
        assert_eq!(app.deferred_input.pop_front(), Some(pasted));
        assert!(app.deferred_input.is_empty());
    }

    #[test]
    fn rendered_input_keeps_cursor_on_wrapped_text_and_long_input_visible() {
        let mut app = test_chat_app();
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(20, 16))
            .expect("test terminal");
        app.input.insert_str("one two three four five");
        app.input.move_left();
        terminal
            .draw(|frame| render_viewport(frame, &app, &Spinner::new(), None))
            .expect("draw");
        let position = terminal.get_cursor_position().expect("cursor");
        assert_eq!(terminal.backend().buffer()[position].symbol(), "e");

        app.input.clear();
        app.input.insert_str("0\n1\n2\n3\n4\n5\n6\n7\n8\n9\nlast");
        terminal
            .draw(|frame| render_viewport(frame, &app, &Spinner::new(), None))
            .expect("draw");
        let position = terminal.get_cursor_position().expect("cursor");
        assert_eq!(
            terminal.backend().buffer()[(position.x - 1, position.y)].symbol(),
            "t"
        );
        assert!(
            position.y < 14,
            "cursor remains inside input, above border and status"
        );
    }

    fn test_executable_agent_ir(admitted: &[&str], forbidden: &[&str]) -> ExecutableAgentIr {
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
        let raw = harw_agent_dsl::parse::parse_toml(&format!(
            r#"
schema = "harwness.agent/v1"
id = "harwness.agent.tui-policy-test@1"
version = "1.0.0"
role = "worker"
specialization = "tui-policy-test"

[tools]
admitted = [{admitted}]
forbidden = [{forbidden}]
"#
        ))
        .expect("test executable policy TOML");
        let resolved = harw_agent_dsl::resolved::ResolvedAgentDefinition {
            id: raw.id,
            version: raw.version,
            role: raw.role,
            specialization: raw.specialization,
            name: raw.name,
            description: raw.description,
            authority: harw_agent_dsl::authority::AuthorityCeiling::default(),
            trace: harw_agent_dsl::resolved::ResolutionTrace { steps: Vec::new() },
            config: raw.tables,
        };

        harw_agent_dsl::lower(&resolved).expect("lower test executable policy")
    }

    #[test]
    fn selected_executable_policy_limits_tui_session_tools_and_retains_snapshot() {
        let policy = test_executable_agent_ir(&["stop"], &[]);
        let snapshot_id = policy.snapshot_id();
        let sandbox = test_sandbox();
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (turn_event_tx, _turn_event_rx) = tokio::sync::mpsc::unbounded_channel();

        let session = build_tui_agent_session(
            SessionId::new(),
            harw_extension_api::ExtensionRegistry::builder().build(),
            event_tx,
            turn_event_tx,
            &sandbox,
            Some(&policy),
        );

        assert!(
            session
                .activation()
                .is_tool_enabled(&harw_extension_api::ToolName::new("stop"))
        );
        assert!(
            !session
                .activation()
                .is_tool_enabled(&harw_extension_api::ToolName::new("shell.exec")),
            "selected executable policy must be deny-by-default"
        );
        assert_eq!(session.executable_snapshot_id(), Some(&snapshot_id));
    }

    #[test]
    fn absent_executable_policy_preserves_full_tui_session_visibility() {
        let sandbox = test_sandbox();
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (turn_event_tx, _turn_event_rx) = tokio::sync::mpsc::unbounded_channel();

        let session = build_tui_agent_session(
            SessionId::new(),
            harw_extension_api::ExtensionRegistry::builder().build(),
            event_tx,
            turn_event_tx,
            &sandbox,
            None,
        );

        assert!(
            session
                .activation()
                .is_tool_enabled(&harw_extension_api::ToolName::new("shell.exec"))
        );
        assert!(session.executable_snapshot_id().is_none());
    }

    #[test]
    fn rate_limit_retry_does_not_append_the_user_message_again() {
        let retry = rate_limit_retry_input();

        assert!(
            retry.user_text.is_none(),
            "core already persisted the failed turn's user message"
        );
    }

    fn test_operations() -> Vec<Arc<dyn harw_operations::Operation>> {
        let mut registry = OperationRegistry::new();
        harw_ops::register_all(&mut registry);
        registry.iter().map(Arc::clone).collect()
    }

    fn test_managed_spawner(
        sandbox: &SandboxSpec,
    ) -> (Arc<ManagedAgentSpawner>, Arc<dyn StateStore>, SessionId) {
        let mut runtime_config = harw_config::ResolvedConfig::default();
        runtime_config.agents.insert(
            "worker".to_owned(),
            harw_config::AgentToml {
                name: "worker".to_owned(),
                role: "worker".to_owned(),
                description: String::new(),
                system_file: None,
                providers: Vec::new(),
                models: Vec::new(),
                skills: Vec::new(),
                suggestions: harw_config::AgentSuggestionsToml::default(),
                primary_provider: None,
                secondary_providers: Vec::new(),
                timeout_seconds: 120,
                max_retries: 2,
            },
        );
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let state_store: Arc<dyn StateStore> = Arc::new(harw_core::InMemoryStateStore::new());
        let model: Arc<dyn ModelProvider> = Arc::new(harw_core::EchoModelProvider::new("ok"));
        let cwd = std::env::current_dir().expect("test cwd");
        let root_session_id = SessionId::new();
        let spawner = build_tui_managed_spawner(
            root_session_id.clone(),
            trusted_tui_spawn_context(sandbox),
            None,
            &runtime_config,
            &cwd,
            &cwd,
            model,
            event_tx,
        )
        .expect("configured TUI child role must register a trusted root");
        (spawner, state_store, root_session_id)
    }

    #[test]
    fn model_operation_provider_is_appended_to_tui_registry() {
        let mut extension_registry = harw_extension_api::ExtensionRegistry::builder().build();
        let operations = test_operations();
        let controller = Arc::new(TuiSessionController::new());
        let sandbox = test_sandbox();
        let (managed_spawner, state_store, _) = test_managed_spawner(&sandbox);

        add_tui_model_tool_provider(
            &mut extension_registry,
            &operations,
            &TuiModelToolServices {
                runtime_config: None,
                memory: None,
                job_store: None,
                controller: &controller,
                managed_spawner: Some(&managed_spawner),
                state_store: &state_store,
            },
        );

        assert_eq!(extension_registry.tool_providers().len(), 1);
        assert!(extension_registry.approval_handlers().is_empty());
        let tool_names: Vec<String> = extension_registry.tool_providers()[0]
            .tools()
            .into_iter()
            .map(|tool| tool.name().to_owned())
            .collect();
        assert!(
            tool_names.iter().any(|name| name == "stop"),
            "the registered provider must expose the operation model tools"
        );
    }

    #[test]
    fn model_tool_context_binds_execution_sandbox_and_trusted_services() {
        let operations = test_operations();
        let controller = Arc::new(TuiSessionController::new());
        let sandbox = test_sandbox();
        let (managed_spawner, state_store, _) = test_managed_spawner(&sandbox);
        let execution_context = harw_extension_api::ToolExecutionContext::new(
            SessionId::new(),
            harw_types::TurnId::new(),
            sandbox.clone(),
        );

        let context = tui_model_tool_context(
            &execution_context,
            &operations,
            &TuiModelToolServices {
                runtime_config: None,
                memory: None,
                job_store: None,
                controller: &controller,
                managed_spawner: Some(&managed_spawner),
                state_store: &state_store,
            },
        );

        assert_eq!(context.session_id(), execution_context.session_id());
        assert_eq!(context.turn_id(), execution_context.turn_id());
        assert_eq!(context.sandbox(), &sandbox);
        assert_eq!(
            context
                .service::<OperationRegistry>()
                .expect("operation registry service")
                .len(),
            operations.len()
        );
        let expected_controller: SharedSessionController =
            Arc::clone(&controller) as SharedSessionController;
        let bound_controller = context
            .service::<SharedSessionController>()
            .expect("shared session controller service");
        assert!(Arc::ptr_eq(bound_controller, &expected_controller));
        let bound_spawner = context
            .service::<Arc<ManagedAgentSpawner>>()
            .expect("managed spawner service");
        assert!(Arc::ptr_eq(bound_spawner, &managed_spawner));
        let bound_store = context
            .service::<Arc<dyn StateStore>>()
            .expect("state store service");
        assert!(Arc::ptr_eq(bound_store, &state_store));
    }

    #[test]
    fn model_tool_context_omits_spawner_service_when_no_spawner_is_configured() {
        let operations = test_operations();
        let controller = Arc::new(TuiSessionController::new());
        let sandbox = test_sandbox();
        let state_store: Arc<dyn StateStore> = Arc::new(harw_core::InMemoryStateStore::new());
        let execution_context = harw_extension_api::ToolExecutionContext::new(
            SessionId::new(),
            harw_types::TurnId::new(),
            sandbox.clone(),
        );

        let context = tui_model_tool_context(
            &execution_context,
            &operations,
            &TuiModelToolServices {
                runtime_config: None,
                memory: None,
                job_store: None,
                controller: &controller,
                managed_spawner: None,
                state_store: &state_store,
            },
        );

        assert!(
            context.service::<Arc<ManagedAgentSpawner>>().is_none(),
            "an empty configured agent set must not register a spawner service"
        );
        assert_eq!(context.session_id(), execution_context.session_id());
        assert_eq!(
            context
                .service::<OperationRegistry>()
                .expect("operation registry service")
                .len(),
            operations.len()
        );
    }

    #[test]
    fn model_tool_provider_registers_without_a_managed_spawner() {
        let mut extension_registry = harw_extension_api::ExtensionRegistry::builder().build();
        let operations = test_operations();
        let controller = Arc::new(TuiSessionController::new());
        let state_store: Arc<dyn StateStore> = Arc::new(harw_core::InMemoryStateStore::new());

        add_tui_model_tool_provider(
            &mut extension_registry,
            &operations,
            &TuiModelToolServices {
                runtime_config: None,
                memory: None,
                job_store: None,
                controller: &controller,
                managed_spawner: None,
                state_store: &state_store,
            },
        );

        assert_eq!(
            extension_registry.tool_providers().len(),
            1,
            "the model tool provider must still be appended when no agent roles are configured"
        );
        let tool_names: Vec<String> = extension_registry.tool_providers()[0]
            .tools()
            .into_iter()
            .map(|tool| tool.name().to_owned())
            .collect();
        assert!(
            tool_names.iter().any(|name| name == "stop"),
            "the turn must still expose operation tools without a spawner"
        );
    }

    #[tokio::test]
    async fn managed_spawner_admits_configured_worker_from_trusted_external_root() {
        let sandbox = test_sandbox();
        let (managed_spawner, _state_store, root_session_id) = test_managed_spawner(&sandbox);
        let handoff_call_id = ToolCallId::new();

        let child = managed_spawner
            .spawn_child(
                "worker",
                SpawnInput {
                    parent_session_id: root_session_id.clone(),
                    handoff_call_id: handoff_call_id.clone(),
                    instructions: Some("inspect the assigned task".to_owned()),
                    context: Default::default(),
                    // No ceiling demand of its own: inherits the trusted
                    // external root's ceiling unchanged.
                    ceiling: None,
                },
                sandbox,
                None,
            )
            .await
            .map_err(|error| TuiError::Core(format!("configured worker admission failed: {error}")))
            .expect("the trusted external root must admit its configured worker role");

        let record = managed_spawner
            .child_record(&child)
            .expect("a successfully admitted child must retain its active record");
        assert_eq!(record.child, child);
        assert_eq!(record.parent, root_session_id);
        assert_eq!(record.handoff_call_id, handoff_call_id);
        assert_eq!(record.role, "worker");
        assert_eq!(record.depth, 1);
        assert_eq!(managed_spawner.active_children_for(&record.parent), 1);
    }

    #[test]
    fn managed_spawner_refuses_an_unconfigured_child_role_set() {
        let sandbox = test_sandbox();
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let runtime_config = harw_config::ResolvedConfig::default();
        let model: Arc<dyn ModelProvider> = Arc::new(harw_core::EchoModelProvider::new("ok"));
        let cwd = std::env::current_dir().expect("test cwd");

        let result = build_tui_managed_spawner(
            SessionId::new(),
            trusted_tui_spawn_context(&sandbox),
            None,
            &runtime_config,
            &cwd,
            &cwd,
            model,
            event_tx,
        );
        let error = match result {
            Ok(_) => panic!("an empty role configuration must fail closed"),
            Err(error) => error,
        };

        match error {
            TuiError::Core(message) => assert!(message.contains("without configured child role")),
            TuiError::Io(_) => panic!("role registration failure must be a typed TUI error"),
            TuiError::ContextProviderRegistration(_) => {
                panic!("role registration failure is not a provider-namespace error")
            }
        }
    }

    #[test]
    fn trusted_spawn_context_preserves_runtime_sandbox_authority() {
        let sandbox = test_sandbox();

        let context = trusted_tui_spawn_context(&sandbox);

        assert_eq!(context.sandbox, sandbox);
        assert!(context.suggestions.is_none());
        assert!(context.capability_snapshot.is_none());
        assert_eq!(
            context.approval_actor,
            Some(ApprovalActor::Operator {
                id: "local-tui".to_owned(),
            })
        );
        assert_eq!(context.organizational_role, AgentRoleId::RootOrchestrator);
    }

    /// AW1-01c: the local TUI spawn context is a root — it carries a
    /// freshly-generated trace with the right hex shapes and no parent span.
    #[test]
    fn trusted_spawn_context_carries_a_freshly_generated_root_trace() {
        let sandbox = test_sandbox();

        let context = trusted_tui_spawn_context(&sandbox);

        let trace = context.trace.expect("local TUI root must carry a trace");
        assert_eq!(trace.trace_id.len(), 32);
        assert!(trace.trace_id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(trace.trace_id, trace.trace_id.to_lowercase());
        assert_eq!(trace.span_id.len(), 16);
        assert!(trace.span_id.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(trace.span_id, trace.span_id.to_lowercase());
        assert!(
            trace.parent_span_id.is_none(),
            "a root trace must not carry a parent span"
        );
    }

    /// AW1-01c: two local TUI sessions must not look like the same session —
    /// the random source must not be broken/constant.
    #[test]
    fn trusted_spawn_context_root_traces_differ_across_two_calls() {
        let sandbox = test_sandbox();

        let first = trusted_tui_spawn_context(&sandbox)
            .trace
            .expect("first call must carry a trace");
        let second = trusted_tui_spawn_context(&sandbox)
            .trace
            .expect("second call must carry a trace");

        assert_ne!(first.trace_id, second.trace_id);
    }

    #[tokio::test]
    async fn local_tui_permissions_are_accessible_but_maintainer_commands_are_blocked() {
        let sandbox = test_sandbox();
        let operations = test_operations();
        let adapters = operations
            .iter()
            .flat_map(|operation| CommandAdapter::from_operation(Arc::clone(operation)))
            .collect::<Vec<_>>();
        let controller = Arc::new(TuiSessionController::new());

        let permissions = execute_command_as(
            &adapters,
            &sandbox,
            &SessionId::new(),
            LOCAL_TUI_OPERATION_PERMISSION,
            "/permissions",
            &CommandServices {
                runtime_config: None,
                memory: None,
                controller: &controller,
                job_store: None,
            },
        )
        .await;
        assert!(permissions.contains("Freigabemodus"), "{permissions}");
        assert!(permissions.contains("ask|auto|full"), "{permissions}");

        let output = execute_command_as(
            &adapters,
            &sandbox,
            &SessionId::new(),
            LOCAL_TUI_OPERATION_PERMISSION,
            "/plugins",
            &CommandServices {
                runtime_config: None,
                memory: None,
                controller: &controller,
                job_store: None,
            },
        )
        .await;

        assert_eq!(
            output,
            "Berechtigung verweigert: /plugins erfordert Maintainer; aktuelle Stufe ist Operator"
        );
    }

    #[test]
    fn test_classify_line_ignore_on_empty() {
        // Leere Zeilen beenden nicht mehr — sie werden ignoriert.
        assert_eq!(classify_line(""), LineAction::Ignore);
        assert_eq!(classify_line("   "), LineAction::Ignore);
    }

    #[test]
    fn test_classify_line_quit_on_slash_quit() {
        assert_eq!(classify_line("/quit"), LineAction::Quit);
        assert_eq!(classify_line("/exit"), LineAction::Quit);
    }

    #[test]
    fn test_classify_line_chat_passes_text() {
        assert_eq!(
            classify_line("hallo welt"),
            LineAction::Chat("hallo welt".to_owned())
        );
    }

    #[test]
    fn test_classify_line_command_is_dispatched() {
        // `/command`-Zeilen werden nun zur Ausführung durchgereicht, nicht mehr
        // als „noch nicht unterstützt"-Hinweis abgewiesen.
        assert_eq!(
            classify_line("/status"),
            LineAction::Command("/status".to_owned())
        );
    }

    #[test]
    fn test_chatapp_push_and_read() {
        let mut app = test_chat_app();
        app.push_line(Role::User, "hi");
        // Nach einem push_line muss genau eine Zelle vorhanden sein.
        assert_eq!(app.cells_len(), 1);
        assert!(app.input().is_empty());
    }

    /// Prüft, dass nach je einem User- und Assistant-Push zwei Zellen vorhanden sind.
    #[test]
    fn test_push_user_and_assistant_cells_count() {
        let mut app = test_chat_app();
        app.push_line(Role::User, "Frage");
        app.push_line(Role::Assistant, "Antwort");
        assert_eq!(
            app.cells_len(),
            2,
            "Zwei Zellen erwartet nach User + Assistant push"
        );
        // UserHistoryCell hat "> "-Präfix auf der ersten Zeile.
        let user_lines = app.cells[0].display_lines(80, style::Theme::Dark);
        assert!(
            !user_lines.is_empty(),
            "User-Zelle muss mindestens eine Zeile liefern"
        );
        let first_content: String = user_lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            first_content.starts_with("> "),
            "User-Zelle muss mit '> ' beginnen, war: {first_content:?}"
        );
        // AssistantHistoryCell liefert den Quelltext umbrochen.
        let asst_lines = app.cells[1].display_lines(80, style::Theme::Dark);
        assert!(
            !asst_lines.is_empty(),
            "Assistant-Zelle muss mindestens eine Zeile liefern"
        );
    }

    /// Prüft, dass nach einem System-Push die PlainHistoryCell den Text enthält.
    #[test]
    fn test_push_system_cell_plain_content() {
        let mut app = test_chat_app();
        app.push_line(Role::System, "Willkommen");
        assert_eq!(app.cells_len(), 1);
        assert_eq!(
            app.cells[0].desired_height(200, style::Theme::Dark),
            1,
            "Kurze System-Zeile muss height=1 haben"
        );
        let lines = app.cells[0].display_lines(200, style::Theme::Dark);
        let content: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(content, "Willkommen");
    }

    /// Popup öffnet sich wenn der Eingabepuffer mit `/` beginnt.
    #[test]
    fn test_popup_opens_on_slash_prefix() {
        let mut app = test_chat_app();
        assert!(app.command_popup.is_none(), "initial kein Popup");

        app.input.insert_char('/');
        app.sync_popup();

        assert!(
            app.command_popup.is_some(),
            "nach '/' muss Popup geöffnet sein"
        );
        assert!(
            !app.command_popup.as_ref().unwrap().is_empty(),
            "ungefiltertes Popup darf nicht leer sein"
        );
    }

    /// Popup schließt sich wenn der Eingabepuffer das `/`-Präfix verliert.
    #[test]
    fn test_popup_closes_without_slash_prefix() {
        let mut app = test_chat_app();

        app.input.insert_char('/');
        app.sync_popup();
        assert!(app.command_popup.is_some());

        app.input.backspace();
        app.sync_popup();

        assert!(
            app.command_popup.is_none(),
            "nach Entfernen des '/' muss Popup geschlossen sein"
        );
    }

    // ────────────────────────────────────────────────────────────────────
    // Regressionstests: Bug 1 — Popup verschluckte Enter.
    //
    // Vorher leitete `handle_key` Enter bei offenem Popup in den
    // Popup-Zweig; ein arg-behafteter Command (`/status`) matchte dort
    // keinen Command-Namen (`PopupAction::Stay`) und die Zeile wurde nie
    // abgeschickt. Fix: Enter wird jetzt immer VOR dem Popup-Zweig
    // behandelt.
    // ────────────────────────────────────────────────────────────────────

    /// Plain Enter bei offenem Popup (Eingabe `/status`, kein Leerzeichen)
    /// sendet die Zeile ab: das erwartete `HarwEvent::Command` erscheint auf
    /// dem Bus und `app.input` wird geleert.
    #[test]
    fn test_handle_key_enter_submits_line_and_closes_popup() {
        let mut app = test_chat_app();
        app.input.clear();
        app.input.insert_str("/status");
        app.sync_popup();
        assert!(
            app.command_popup.is_some(),
            "Popup muss vor dem Enter-Druck offen sein"
        );

        let (bus, mut receiver) = harw_event_channel();
        let mut pending_quit: Option<QuitArm> = None;
        let key = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);

        let redraw = handle_key(&mut app, key, &mut pending_quit, &bus);

        assert!(redraw, "Enter muss einen Redraw anfordern");
        assert!(
            app.command_popup.is_none(),
            "plain Enter muss ein offenes Popup schließen"
        );
        assert!(
            app.input().is_empty(),
            "plain Enter muss die Eingabe leeren"
        );

        match receiver.try_recv() {
            Ok(HarwEvent::Command(raw)) => {
                assert_eq!(
                    raw, "/status",
                    "Rohzeile muss unverändert weitergereicht werden"
                );
            }
            other => panic!("erwartete HarwEvent::Command(\"/status\"), war: {other:?}"),
        }
    }

    /// `KeyCode::Tab` bei offenem Popup akzeptiert die markierte Auswahl,
    /// setzt `app.input` auf `"/<name> "` und sendet dabei NICHTS über den
    /// Bus (kein Absenden, nur Autocomplete).
    #[test]
    fn test_handle_key_tab_accepts_popup_selection_without_submit() {
        let mut app = test_chat_app();
        app.input.clear();
        app.input.insert_str("/hel");
        app.sync_popup();
        match app
            .command_popup
            .as_ref()
            .and_then(CommandPopup::selected_name)
        {
            Some(name) => assert_eq!(name, "help", "einzige Übereinstimmung für 'hel'"),
            None => panic!("Popup muss eine Auswahl für 'hel' markieren"),
        }

        let (bus, mut receiver) = harw_event_channel();
        let mut pending_quit: Option<QuitArm> = None;
        let key = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);

        let redraw = handle_key(&mut app, key, &mut pending_quit, &bus);

        assert!(redraw, "Tab muss einen Redraw anfordern");
        assert_eq!(
            app.input(),
            "/help ",
            "Tab muss die Auswahl inkl. trailing Space übernehmen"
        );
        assert!(
            app.command_popup.is_none(),
            "Tab muss das Popup nach der Auswahl schließen"
        );
        match receiver.try_recv() {
            Err(_) => {}
            Ok(event) => panic!("Tab darf kein HarwEvent senden, war: {event:?}"),
        }
    }

    // ────────────────────────────────────────────────────────────────────
    // Regressionstests: Bug 2 — Ziffern in Argumenten vom Popup abgefangen.
    //
    // Vorher fing der Popup-Zweig `KeyCode::Char('1'..='9')` als
    // Auswahl-Index ab, auch während der Argument-Eingabe nach einem
    // Leerzeichen — z. B. wurde aus `job-42` `job-`. Fix: `sync_popup`
    // schließt das Popup, sobald die Eingabe nach `/` Whitespace enthält.
    // ────────────────────────────────────────────────────────────────────

    /// Nach `/stop job-` (Leerzeichen vorhanden) muss `sync_popup` das
    /// Popup schließen — die Eingabe befindet sich in der Argument-Phase.
    #[test]
    fn test_sync_popup_closes_when_argument_has_whitespace() {
        let mut app = test_chat_app();
        app.input.clear();
        app.input.insert_str("/stop job-");

        app.sync_popup();

        assert!(
            app.command_popup.is_none(),
            "Popup muss bei Whitespace nach '/' geschlossen sein"
        );
    }

    /// Kontrast: `/mod` (kein Leerzeichen) hält das Popup offen — reine
    /// Command-Namen-Eingabe ohne Argument-Phase.
    #[test]
    fn test_sync_popup_stays_open_without_whitespace() {
        let mut app = test_chat_app();
        app.input.clear();
        app.input.insert_str("/mod");

        app.sync_popup();

        assert!(
            app.command_popup.is_some(),
            "Popup muss ohne Whitespace nach '/' offen bleiben"
        );
    }

    /// Kern-Regressionstest: Zeichen für Zeichen `/stop job-42` eintippen —
    /// die Ziffern `4` und `2` dürfen NICHT vom Popup als Auswahl-Index
    /// verschluckt werden, sobald die Argument-Phase (nach dem Leerzeichen)
    /// erreicht ist.
    #[test]
    fn test_handle_key_types_digits_in_argument_not_swallowed() {
        let mut app = test_chat_app();
        let (bus, mut receiver) = harw_event_channel();
        let mut pending_quit: Option<QuitArm> = None;

        for character in "/stop job-42".chars() {
            let key = KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE);
            handle_key(&mut app, key, &mut pending_quit, &bus);
        }

        assert_eq!(
            app.input(),
            "/stop job-42",
            "alle getippten Zeichen inkl. Ziffern müssen in der Eingabe landen"
        );
        // Kein Enter gedrückt — es darf noch nichts abgeschickt worden sein.
        assert!(
            receiver.try_recv().is_err(),
            "ohne Enter darf kein HarwEvent gesendet werden"
        );
    }

    #[test]
    fn session_id_mapper_is_stable_and_opaque() {
        let root = std::path::Path::new("/workspace/project-alpha");
        let first = session_id_for_canonical_project_root(root);
        let second = session_id_for_canonical_project_root(root);
        assert_eq!(first, second);
        assert!(!first.as_str().contains("project-alpha"));
        assert!(
            !first.as_str().starts_with("local-tui:"),
            "derived TUI session IDs must not use the legacy colon separator"
        );

        let hash = first
            .as_str()
            .strip_prefix("local-tui-")
            .expect("derived TUI session IDs use the portable local-tui- prefix");
        assert_eq!(hash.len(), 16);
        assert!(
            hash.bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "derived TUI session ID hash must be lowercase hexadecimal"
        );
    }

    #[test]
    fn session_id_mapper_separates_project_roots() {
        assert_ne!(
            session_id_for_canonical_project_root(std::path::Path::new("/workspace/a")),
            session_id_for_canonical_project_root(std::path::Path::new("/workspace/b")),
        );
    }

    #[test]
    fn explicit_session_id_overrides_project_derived_id() {
        let explicit = SessionId::from_str("resume-this-exact-session");
        assert_eq!(
            selected_session_id(
                std::path::Path::new("/workspace/unrelated-project"),
                Some(explicit.clone()),
            ),
            explicit,
        );
    }

    #[test]
    fn ordinary_starts_create_distinct_sessions_for_the_same_project() {
        let root = std::path::Path::new("/workspace/project");
        let first = selected_session_id(root, None);
        let second = selected_session_id(root, None);
        assert_ne!(first, second);
        assert!(
            first
                .as_str()
                .starts_with(session_id_for_canonical_project_root(root).as_str())
        );
    }

    #[test]
    fn resume_command_becomes_a_runtime_request_only_for_valid_shapes() {
        assert_eq!(
            resume_request("/resume"),
            Some(TuiRunOutcome::Resume { selector: None })
        );
        assert_eq!(
            resume_request("  /resume session-prefix  "),
            Some(TuiRunOutcome::Resume {
                selector: Some("session-prefix".to_owned()),
            }),
        );
        assert_eq!(resume_request("/resume too many"), None);
        assert_eq!(resume_request("/resume-other"), None);
    }

    #[test]
    fn durable_history_hydrates_core_and_redacts_non_text_visible_content() {
        use harw_protocol::items::{
            AssistantMessageItem, ErrorItem, ReasoningItem, ToolCallItem, ToolCallResult,
            ToolResultItem, UserMessageItem,
        };
        use harw_types::{ItemId, ToolCallId};

        let call_id = ToolCallId::new();
        let mut history = ConversationHistory::new();
        history.push(TurnItem::UserMessage(UserMessageItem {
            id: ItemId::new(),
            content: vec![
                ContentPart::Text {
                    text: "look ".to_owned(),
                },
                ContentPart::ImageUrl {
                    url: "https://secret.example/token".to_owned(),
                    detail: None,
                },
            ],
        }));
        history.push(TurnItem::AssistantMessage(AssistantMessageItem {
            id: ItemId::new(),
            content: vec![ContentPart::Text {
                text: "persisted answer".to_owned(),
            }],
            phase: None,
        }));
        history.push(TurnItem::ToolCall(ToolCallItem {
            id: ItemId::new(),
            call_id: call_id.clone(),
            tool_name: "read_file".to_owned(),
            arguments: Default::default(),
        }));
        history.push(TurnItem::ToolResult(ToolResultItem {
            id: ItemId::new(),
            call_id,
            result: ToolCallResult::error("do-not-render"),
            duration_ms: 3,
        }));
        history.push(TurnItem::Reasoning(ReasoningItem {
            id: ItemId::new(),
            summary_text: vec!["private reasoning".to_owned()],
            raw_content: vec!["raw secret".to_owned()],
        }));
        history.push(TurnItem::Error(ErrorItem {
            id: ItemId::new(),
            message: "sensitive backend detail".to_owned(),
            retryable: false,
        }));

        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut session = AgentSession::new_with_id(
            SessionId::from_str("resumed-session"),
            AgentRole::Assistant,
            None,
            harw_extension_api::ExtensionRegistry::builder().build(),
            event_tx,
        );
        let mut app = test_chat_app();
        install_loaded_history(&mut session, &mut app, history);

        assert_eq!(session.history().len(), 6);
        let visible = app
            .cells
            .iter()
            .flat_map(|cell| cell.display_lines(200, style::Theme::Dark))
            .flat_map(|line| line.spans)
            .map(|span| span.content.into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(visible.contains("look [non-text content]"));
        assert!(visible.contains("persisted answer"));
        assert!(visible.contains("[tool call]"));
        assert!(visible.contains("[tool result]"));
        assert!(visible.contains("[reasoning]"));
        assert!(visible.contains("[error]"));
        for secret in [
            "secret.example",
            "read_file",
            "do-not-render",
            "private reasoning",
            "raw secret",
            "sensitive backend detail",
        ] {
            assert!(!visible.contains(secret));
        }
    }

    #[test]
    fn tui_registry_discovery_failure_is_typed_and_fail_closed() {
        let missing_root = std::env::temp_dir().join(format!(
            "harw-tui-missing-project-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock must be after the Unix epoch")
                .as_nanos()
        ));

        let error = match assemble_tui_registry(missing_root) {
            Ok(_) => panic!("missing project must fail"),
            Err(error) => error,
        };

        match error {
            TuiError::Core(message) => {
                assert!(message.contains("could not assemble default registry"));
                assert!(message.contains("project discovery failed"));
            }
            TuiError::Io(_) => panic!("registry discovery must use the typed core error path"),
            TuiError::ContextProviderRegistration(_) => {
                panic!("registry discovery failure is not a provider-namespace error")
            }
        }
    }

    #[test]
    fn tui_context_root_alignment_accepts_the_shared_project_root() {
        let root = std::path::Path::new("/workspace/project");

        ensure_tui_context_roots_align(root, root)
            .expect("a shared project root must be safe to start");
    }

    // ────────────────────────────────────────────────────────────────────
    // AP W5-03 — Freigabe-Treiber (P0). Alle Tests laufen headless über den
    // Fragekanal; kein Terminal, kein Netzwerk.
    // ────────────────────────────────────────────────────────────────────

    /// Name des Werkzeugs, das die Freigabetests anhalten lassen.
    const APPROVAL_TEST_TOOL: &str = "fs.write";

    /// Modell, das eine vorher festgelegte Folge von Antworten liefert.
    struct ScriptedModel {
        responses: Mutex<VecDeque<ModelResponse>>,
    }

    impl ScriptedModel {
        fn new(responses: Vec<ModelResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
            }
        }

        fn tool_response(name: &str, arguments: Value) -> ModelResponse {
            ModelResponse {
                message: None,
                tool_calls: vec![ToolCall {
                    id: ToolCallId::new(),
                    name: ToolName::new(name),
                    arguments,
                }],
                usage: TokenUsage::default(),
            }
        }
    }

    impl ModelProvider for ScriptedModel {
        fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
            let next = match self.responses.lock() {
                Ok(mut responses) => responses.pop_front(),
                Err(_) => None,
            };
            Box::pin(async move {
                Ok(next.unwrap_or_else(|| ModelResponse::text("scripted model exhausted")))
            })
        }
    }

    /// Werkzeug, das ausschließlich zählt, wie oft es ausgeführt wurde.
    struct CountingTool {
        executions: Arc<AtomicUsize>,
    }

    impl ToolExecutor for CountingTool {
        fn execute<'a>(
            &'a self,
            _context: &'a ToolExecutionContext,
            _call: &'a ToolCall,
        ) -> ToolExecutorFuture<'a> {
            let executions = Arc::clone(&self.executions);
            Box::pin(async move {
                executions.fetch_add(1, Ordering::SeqCst);
                Ok(ToolOutput::text("written"))
            })
        }
    }

    struct CountingToolProvider {
        executions: Arc<AtomicUsize>,
    }

    impl ToolProvider for CountingToolProvider {
        fn tools(&self) -> Vec<ToolSpec> {
            vec![ToolSpec::Function(FunctionToolSpec {
                name: ToolName::new(APPROVAL_TEST_TOOL),
                description: "test-only write tool".to_owned(),
                parameters: JsonSchema::default(),
                strict: false,
            })]
        }

        fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
            if name.as_str() == APPROVAL_TEST_TOOL {
                Some(Arc::new(CountingTool {
                    executions: Arc::clone(&self.executions),
                }))
            } else {
                None
            }
        }
    }

    /// Baut eine Session **wie `build_session_runtime`**: die `DefaultApprovalPolicy`
    /// entscheidet, der TUI-Handler wird über [`registry_with_approval_handler`]
    /// dahinter gehängt, und der Spawn-Kontext stammt aus
    /// [`trusted_tui_spawn_context`] (und trägt damit den Approval-Actor).
    fn approval_test_session(
        handler: &Arc<TuiApprovalHandler>,
        executions: &Arc<AtomicUsize>,
        sandbox: &SandboxSpec,
    ) -> AgentSession {
        let base = ExtensionRegistry::builder()
            .tool_provider(Arc::new(CountingToolProvider {
                executions: Arc::clone(executions),
            }))
            .approval_handler(Arc::new(harw_registry_defaults::DefaultApprovalPolicy))
            .build();
        let registered = as_dyn_approval_handler(handler);
        let registry = registry_with_approval_handler(base, registered)
            .expect("test registry has no namespace collision");
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        AgentSession::new(AgentRole::Assistant, None, registry, event_tx)
            .with_spawn_context(trusted_tui_spawn_context(sandbox))
    }

    /// Ergebnis eines headless getriebenen Freigabeturns.
    struct DrivenTurn {
        outcome: Result<TurnOutcome, String>,
        executions: usize,
        prompts_seen: usize,
    }

    /// Treibt einen Turn, der an `fs.write` pausiert, und beantwortet jede
    /// eintreffende Frage mit `approve`.
    async fn drive_turn_answering(approve: bool) -> DrivenTurn {
        let executions = Arc::new(AtomicUsize::new(0));
        let (handler, mut prompts) = TuiApprovalHandler::new();
        let driver = ApprovalDriver::new(Arc::clone(&handler));
        let sandbox = test_sandbox();
        let mut session = approval_test_session(&handler, &executions, &sandbox);
        let model = ScriptedModel::new(vec![
            ScriptedModel::tool_response(APPROVAL_TEST_TOOL, json!({ "path": "note.txt" })),
            ModelResponse::text("fertig"),
        ]);
        let store = InMemoryStateStore::new();

        let first = run_turn(
            &mut session,
            &model,
            &store,
            TurnInput::user("schreib etwas"),
        )
        .await;
        let outcome = match first {
            Ok(outcome) => outcome,
            Err(error) => {
                return DrivenTurn {
                    outcome: Err(error.to_string()),
                    executions: executions.load(Ordering::SeqCst),
                    prompts_seen: 0,
                };
            }
        };
        assert!(
            matches!(outcome, TurnOutcome::AwaitingApproval { .. }),
            "the default policy must pause an unapproved write"
        );

        let mut prompts_seen = 0usize;
        let drive = driver.drive_to_completion(&mut session, &model, &store, None, outcome);
        tokio::pin!(drive);
        let result = loop {
            tokio::select! {
                result = &mut drive => break result,
                maybe_prompt = prompts.recv() => {
                    if let Some(prompt) = maybe_prompt {
                        prompts_seen += 1;
                        if approve {
                            prompt.approve();
                        } else {
                            prompt.reject("operator rejected the tool call in the test");
                        }
                    }
                }
            }
        };

        DrivenTurn {
            outcome: result.map_err(|error| tui_error_from_approval_driver(error).to_string()),
            executions: executions.load(Ordering::SeqCst),
            prompts_seen,
        }
    }

    /// Bedingung 1 + 5: der Spawn-Kontext trägt einen Approval-Actor, und eine
    /// freigegebene Pause endet in `Completed` — das Werkzeug lief genau einmal.
    #[tokio::test]
    async fn approved_pause_completes_the_turn_and_runs_the_tool() {
        let driven = drive_turn_answering(true).await;

        match driven.outcome {
            Ok(TurnOutcome::Completed) => {}
            Ok(other) => panic!("an approved pause must complete the turn, was: {other:?}"),
            Err(error) => panic!("an approved pause must not fail the turn: {error}"),
        }
        assert_eq!(driven.prompts_seen, 1, "genau eine Frage erwartet");
        assert_eq!(
            driven.executions, 1,
            "eine Freigabe muss das Werkzeug genau einmal ausführen"
        );
    }

    /// Ablehnung: der Turn endet trotzdem sauber, das Werkzeug läuft **nicht**.
    #[tokio::test]
    async fn rejected_pause_completes_the_turn_without_running_the_tool() {
        let driven = drive_turn_answering(false).await;

        match driven.outcome {
            Ok(TurnOutcome::Completed) => {}
            Ok(other) => panic!("a rejected pause must still complete the turn, was: {other:?}"),
            Err(error) => panic!("a rejected pause must not fail the turn: {error}"),
        }
        assert_eq!(driven.prompts_seen, 1, "genau eine Frage erwartet");
        assert_eq!(
            driven.executions, 0,
            "eine Ablehnung darf das Werkzeug nicht ausführen"
        );
    }

    /// Bedingung 2: derselbe `Arc` liegt in der Registry und im Treiber, und er
    /// wird **hinter** die bestehende Politik gehängt.
    #[test]
    fn tui_approval_handler_is_appended_behind_the_existing_policy() {
        let base = ExtensionRegistry::builder()
            .approval_handler(Arc::new(harw_registry_defaults::DefaultApprovalPolicy))
            .build();
        let (handler, _prompts) = TuiApprovalHandler::new();
        let driver = ApprovalDriver::new(Arc::clone(&handler));
        let registered = as_dyn_approval_handler(&handler);

        let registry = registry_with_approval_handler(base, registered)
            .expect("test registry has no namespace collision");

        assert_eq!(
            registry.approval_handlers().len(),
            2,
            "die bestehende Politik darf nicht ersetzt werden"
        );
        let expected = as_dyn_approval_handler(driver.handler());
        assert!(
            Arc::ptr_eq(&registry.approval_handlers()[1], &expected),
            "Registry und Treiber müssen denselben Handler halten"
        );
    }

    /// Der Handler-Umbau darf keinen anderen Registry-Beitrag verlieren.
    #[test]
    fn appending_the_approval_handler_preserves_every_other_contribution() {
        let executions = Arc::new(AtomicUsize::new(0));
        let base = ExtensionRegistry::builder()
            .tool_provider(Arc::new(CountingToolProvider {
                executions: Arc::clone(&executions),
            }))
            .build();
        let (handler, _prompts) = TuiApprovalHandler::new();

        let registered = as_dyn_approval_handler(&handler);
        let registry = registry_with_approval_handler(base, registered)
            .expect("test registry has no namespace collision");

        assert_eq!(registry.tool_providers().len(), 1);
        let names: Vec<String> = registry.tool_providers()[0]
            .tools()
            .into_iter()
            .map(|tool| tool.name().to_owned())
            .collect();
        assert_eq!(names, vec![APPROVAL_TEST_TOOL.to_owned()]);
    }

    /// Bedingung 3: nur ein ausdrückliches `y` gibt frei.
    #[test]
    fn only_an_explicit_y_approves_a_pending_prompt() {
        let approve = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE);
        let reject = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE);
        let escape = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);
        let cancel = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let other = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);

        assert_eq!(classify_approval_key(approve), ApprovalKeyAction::Approve);
        assert_eq!(
            classify_approval_key(reject),
            ApprovalKeyAction::Reject(REASON_OPERATOR_REJECTED)
        );
        assert_eq!(
            classify_approval_key(escape),
            ApprovalKeyAction::Reject(REASON_OPERATOR_CANCELLED)
        );
        assert_eq!(
            classify_approval_key(cancel),
            ApprovalKeyAction::Reject(REASON_OPERATOR_CANCELLED)
        );
        assert_eq!(classify_approval_key(other), ApprovalKeyAction::Ignore);
    }

    /// Die Antwort landet beim wartenden Treiber **und** in derselben Zelle.
    #[tokio::test]
    async fn answering_a_prompt_updates_the_same_cell() {
        let (handler, mut prompts) = TuiApprovalHandler::new();
        let request = ItemId::new();
        let call = ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(APPROVAL_TEST_TOOL),
            arguments: json!({ "path": "note.txt" }),
        };
        assert!(handler.open_prompt(&request, &call));

        let prompt = match prompts.try_recv() {
            Ok(prompt) => prompt,
            Err(error) => panic!("die Frage muss den Renderer erreichen: {error}"),
        };
        let cell = Arc::new(Mutex::new(ApprovalPromptCell {
            tool_name: prompt.tool_name().to_owned(),
            arguments_raw: prompt.arguments_json(),
            decision: None,
        }));
        PendingApprovalPrompt {
            prompt,
            cell: Arc::clone(&cell),
        }
        .approve();

        match handler.await_resolution(&request).await {
            ApprovalResolution::Approve => {}
            other => panic!("eine Freigabe muss den Treiber erreichen, war: {other:?}"),
        }
        match cell.lock() {
            Ok(cell) => assert_eq!(cell.decision, Some(true)),
            Err(_) => panic!("die Zelle muss nach der Antwort lesbar bleiben"),
        }
    }

    /// Eine Ablehnung schreibt dieselbe Zelle auf `Some(false)` fort.
    #[tokio::test]
    async fn rejecting_a_prompt_marks_the_same_cell_as_denied() {
        let (handler, mut prompts) = TuiApprovalHandler::new();
        let request = ItemId::new();
        let call = ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(APPROVAL_TEST_TOOL),
            arguments: json!({ "path": "note.txt" }),
        };
        assert!(handler.open_prompt(&request, &call));
        let prompt = match prompts.try_recv() {
            Ok(prompt) => prompt,
            Err(error) => panic!("die Frage muss den Renderer erreichen: {error}"),
        };
        let cell = Arc::new(Mutex::new(ApprovalPromptCell {
            tool_name: prompt.tool_name().to_owned(),
            arguments_raw: prompt.arguments_json(),
            decision: None,
        }));

        PendingApprovalPrompt {
            prompt,
            cell: Arc::clone(&cell),
        }
        .reject(REASON_OPERATOR_REJECTED);

        match handler.await_resolution(&request).await {
            ApprovalResolution::Reject { reason } => {
                assert_eq!(reason, REASON_OPERATOR_REJECTED);
            }
            other => panic!("eine Ablehnung muss den Treiber erreichen, war: {other:?}"),
        }
        match cell.lock() {
            Ok(cell) => assert_eq!(cell.decision, Some(false)),
            Err(_) => panic!("die Zelle muss nach der Antwort lesbar bleiben"),
        }
    }

    /// Bedingung 7: die Kern-Ursache bleibt in der Fehlermeldung erhalten.
    #[test]
    fn approval_driver_errors_keep_their_cause() {
        let error = ApprovalDriverError::MissingPendingApproval {
            session: "session-42".to_owned(),
        };
        let expected = error.to_string();

        match tui_error_from_approval_driver(error) {
            TuiError::Core(message) => {
                assert!(message.contains("session-42"));
                assert!(message.contains(&expected));
            }
            TuiError::Io(_) => panic!("ein Treiberfehler ist kein Terminal-I/O-Fehler"),
            TuiError::ContextProviderRegistration(_) => {
                panic!("ein Treiberfehler ist kein Namensraum-Konflikt")
            }
        }
    }

    // ────────────────────────────────────────────────────────────────────
    // AP W5-04 — rollenspezifische Kind-Registry.
    // ────────────────────────────────────────────────────────────────────

    fn test_child_registry_factory() -> (TuiChildRegistryFactory, std::path::PathBuf) {
        let cwd = match std::env::current_dir() {
            Ok(cwd) => cwd,
            Err(error) => panic!("test cwd must be readable: {error}"),
        };
        let assembled = match assemble_tui_registry(cwd.clone()) {
            Ok(assembled) => assembled,
            Err(error) => panic!("the workspace must be a discoverable project: {error}"),
        };
        let project_root = assembled.project.project_root.clone();
        let model: Arc<dyn ModelProvider> = Arc::new(harw_core::EchoModelProvider::new("ok"));
        let factory = match TuiChildRegistryFactory::new(&cwd, &project_root, model) {
            Ok(factory) => factory,
            Err(error) => panic!("the builtin agent definitions must lower: {error}"),
        };
        (factory, project_root)
    }

    fn test_spawn_input() -> SpawnInput {
        SpawnInput {
            parent_session_id: SessionId::new(),
            handoff_call_id: ToolCallId::new(),
            instructions: Some("erkunde das Projekt".to_owned()),
            context: Default::default(),
            // Keine eigene Deckenforderung: erbt die Decke des Elternteils.
            ceiling: None,
        }
    }

    fn registered_tool_names(registry: &ExtensionRegistry) -> Vec<String> {
        registry
            .tool_providers()
            .iter()
            .flat_map(|provider| provider.tools())
            .map(|tool| tool.name().to_owned())
            .collect()
    }

    /// Kernanforderung W5-04: die Explorer-Registry kennt weder `fs.write` noch
    /// `shell.exec` — namentlich geprüft, nicht nur über das Profil.
    #[test]
    fn explorer_child_registry_has_no_write_and_no_shell_tool() {
        let (factory, _project_root) = test_child_registry_factory();

        let registry = match factory.build_registry(role_names::EXPLORER, &test_spawn_input(), None)
        {
            Ok(registry) => registry,
            Err(error) => panic!("the explorer role must assemble: {}", error.message),
        };

        let names = registered_tool_names(&registry);
        assert!(
            !names.iter().any(|name| name == "fs.write"),
            "der Explorer darf `fs.write` nicht sehen, sah: {names:?}"
        );
        assert!(
            !names.iter().any(|name| name == "shell.exec"),
            "der Explorer darf `shell.exec` nicht sehen, sah: {names:?}"
        );
        assert!(
            names.iter().any(|name| name == "fs.read"),
            "der Explorer muss lesend arbeiten können, sah: {names:?}"
        );
    }

    /// Die Rolle-→-Profil-Zuordnung stammt ausschließlich aus
    /// `harw-registry-defaults`; eine unbekannte Rolle bekommt das
    /// Eltern-Profil, nie mehr.
    #[test]
    fn child_registry_profiles_come_from_the_shared_role_mapping() {
        let (factory, _project_root) = test_child_registry_factory();

        for role in role_names::ALL {
            let registry = match factory.build_registry(role, &test_spawn_input(), None) {
                Ok(registry) => registry,
                Err(error) => panic!("role '{role}' must assemble: {}", error.message),
            };
            let names = registered_tool_names(&registry);
            assert!(
                !names.iter().any(|name| name == "shell.exec"),
                "keine eingebaute Rolle darf `shell.exec` sehen ({role}): {names:?}"
            );
            assert!(
                factory.executable_agent_ir(role).is_some(),
                "jede eingebaute Rolle muss ihre Agent-IR mitbringen ({role})"
            );
        }

        // Eine konfigurierte, nicht eingebaute Rolle fällt auf das volle
        // Eltern-Profil zurück — und bringt keine eingebaute IR mit.
        let fallback = match factory.build_registry("worker", &test_spawn_input(), None) {
            Ok(registry) => registry,
            Err(error) => panic!("a configured role must assemble: {}", error.message),
        };
        assert!(
            registered_tool_names(&fallback)
                .iter()
                .any(|name| name == "fs.write"),
            "eine konfigurierte Rolle behält den Werkzeugsatz des Elternteils"
        );
        assert!(factory.executable_agent_ir("worker").is_none());
    }

    /// Die Kind-Grenzen kommen aus dem Laufzeitprofil des gewählten Modells und
    /// können die konservative Grenze nur senken.
    #[test]
    fn child_limits_are_derived_monotonically_from_the_runtime_profile() {
        let mut config = harw_config::ResolvedConfig::default();
        assert_eq!(
            tui_child_limits(&config),
            ChildLimits::conservative(),
            "ohne Default-Modell bleibt es bei der konservativen Grenze"
        );

        config.harness.default_model = Some("claude-opus-4-8".to_owned());
        let limits = tui_child_limits(&config);
        let conservative = ChildLimits::conservative();
        assert!(
            limits.max_active_children_per_parent <= conservative.max_active_children_per_parent,
            "die Ableitung darf nur senken"
        );
        assert!(limits.max_active_children_per_parent >= 1);
        assert_eq!(limits.max_depth, conservative.max_depth);
        assert_eq!(limits.lease_seconds, conservative.lease_seconds);
    }

    // ────────────────────────────────────────────────────────────────────
    // AP W5-05 — `/mode` an der Turn-Grenze.
    // ────────────────────────────────────────────────────────────────────

    /// `/mode explore` wirkt genau an der Turn-Grenze — vorher nicht, nachher
    /// vollständig — und erscheint danach in der Statuszeile.
    #[test]
    fn mode_request_is_applied_at_the_turn_boundary() {
        let sandbox = test_sandbox();
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (turn_event_tx, _turn_event_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut session = build_tui_agent_session(
            SessionId::new(),
            harw_extension_api::ExtensionRegistry::builder().build(),
            event_tx,
            turn_event_tx,
            &sandbox,
            None,
        );
        let controller = Arc::new(TuiSessionController::new());
        let mut app = ChatApp::new(Vec::new(), sandbox, SessionId::new())
            .with_session_controller(Arc::clone(&controller));

        assert_eq!(session.mode(), InteractionMode::Chat);
        if let Err(error) = SessionController::request_mode(controller.as_ref(), "explore") {
            panic!("`/mode explore` muss angenommen werden: {error}");
        }
        assert_eq!(
            session.mode(),
            InteractionMode::Chat,
            "ein laufender Turn darf seinen Modus nicht unter sich wechseln"
        );

        assert!(app.apply_pending_controller_state(&mut session));

        assert_eq!(session.mode(), InteractionMode::Explore);
        assert_eq!(app.active_mode(), InteractionMode::Explore);
        let rendered: String = status_line(
            style::Theme::Dark,
            &Spinner::new(),
            None,
            &TokenUsage::default(),
            app.active_mode(),
        )
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect();
        assert!(
            rendered.contains("Modus: explore"),
            "die Statuszeile muss den aktiven Modus zeigen, war: {rendered:?}"
        );
    }

    /// Ein unbekannter Modusname wird abgewiesen statt still auf den Default zu
    /// fallen; die Session bleibt unangetastet.
    #[test]
    fn unknown_mode_names_never_change_the_session() {
        let sandbox = test_sandbox();
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (turn_event_tx, _turn_event_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut session = build_tui_agent_session(
            SessionId::new(),
            harw_extension_api::ExtensionRegistry::builder().build(),
            event_tx,
            turn_event_tx,
            &sandbox,
            None,
        );
        let controller = Arc::new(TuiSessionController::new());
        let mut app = ChatApp::new(Vec::new(), sandbox, SessionId::new())
            .with_session_controller(Arc::clone(&controller));

        assert!(SessionController::request_mode(controller.as_ref(), "yolo").is_err());
        assert!(!app.apply_pending_controller_state(&mut session));
        assert_eq!(session.mode(), InteractionMode::Chat);
    }

    // ────────────────────────────────────────────────────────────────────
    // AP W5-10b — Zellen-Verdrahtung.
    // ────────────────────────────────────────────────────────────────────

    fn rendered_cells(app: &ChatApp) -> String {
        app.cells
            .iter()
            .flat_map(|cell| cell.display_lines(120, style::Theme::Dark))
            .flat_map(|line| line.spans)
            .map(|span| span.content.into_owned())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Kernanforderung W5-10b: drei Kind-Ereignisse erzeugen **eine** Zelle, die
    /// fortgeschrieben wird — nicht drei.
    #[test]
    fn three_child_events_produce_a_single_sub_agent_cell() {
        let mut app = test_chat_app();
        let mut state = TurnEventState::default();
        let turn_id = TurnId::new();
        let child = SessionId::from_str("child-explorer-1");
        let before = app.cells_len();

        assert!(handle_turn_event(
            &mut app,
            &mut state,
            TurnEvent::ChildSpawned {
                turn_id: turn_id.clone(),
                child: child.clone(),
                role: role_names::EXPLORER.to_owned(),
                question: Some("Wo liegt der Turn-Loop?".to_owned()),
            }
        ));
        assert_eq!(app.cells_len(), before + 1);

        assert!(handle_turn_event(
            &mut app,
            &mut state,
            TurnEvent::ChildProgress {
                turn_id: turn_id.clone(),
                child: child.clone(),
                tool_calls: 7,
                tokens: 4242,
            }
        ));
        assert!(handle_turn_event(
            &mut app,
            &mut state,
            TurnEvent::ChildCompleted {
                turn_id,
                child,
                outcome: "completed".to_owned(),
                duration_ms: 512,
            }
        ));

        assert_eq!(
            app.cells_len(),
            before + 1,
            "drei Ereignisse desselben Kindes dürfen nur eine Zelle erzeugen"
        );
        let rendered = rendered_cells(&app);
        assert!(rendered.contains(role_names::EXPLORER));
        assert!(rendered.contains('7'), "Tool-Aufrufe fehlen: {rendered:?}");
        assert!(
            rendered.contains("fertig"),
            "die Zelle muss den Abschluss zeigen: {rendered:?}"
        );
    }

    /// Ein Fortschritt für ein unbekanntes Kind erzeugt **keine** Zelle.
    #[test]
    fn progress_for_an_unknown_child_creates_no_cell() {
        let mut app = test_chat_app();
        let mut state = TurnEventState::default();
        let before = app.cells_len();

        let handled = handle_turn_event(
            &mut app,
            &mut state,
            TurnEvent::ChildProgress {
                turn_id: TurnId::new(),
                child: SessionId::from_str("never-spawned"),
                tool_calls: 1,
                tokens: 1,
            },
        );

        assert!(!handled);
        assert_eq!(app.cells_len(), before);
    }

    /// Ohne Plan-Dienste geht die Plan-Information nicht verloren — sie wird nur
    /// einzeilig statt als Graph gezeigt.
    #[test]
    fn plan_updates_degrade_to_a_system_line_without_plan_services() {
        let mut app = test_chat_app();
        let mut state = TurnEventState::default();

        assert!(handle_turn_event(
            &mut app,
            &mut state,
            TurnEvent::PlanUpdated {
                plan_id: "plan-7".to_owned(),
                revision: 3,
                summary: "Knoten ergänzt".to_owned(),
            }
        ));

        let rendered = rendered_cells(&app);
        assert!(rendered.contains("plan-7"));
        assert!(rendered.contains("Knoten ergänzt"));
    }

    /// `ModeChanged` zieht den Anzeigemodus nach; ein unbekannter Name nicht.
    #[test]
    fn mode_changed_events_update_the_status_bar_mode() {
        let mut app = test_chat_app();
        let mut state = TurnEventState::default();

        assert!(handle_turn_event(
            &mut app,
            &mut state,
            TurnEvent::ModeChanged {
                mode: "explore".to_owned(),
            }
        ));
        assert_eq!(app.active_mode(), InteractionMode::Explore);

        assert!(!handle_turn_event(
            &mut app,
            &mut state,
            TurnEvent::ModeChanged {
                mode: "teleport".to_owned(),
            }
        ));
        assert_eq!(app.active_mode(), InteractionMode::Explore);
    }

    /// Nur die exakte Zeile `/goal check` löst eine [`GoalCell`] aus.
    #[test]
    fn only_goal_check_requests_a_goal_cell() {
        assert!(is_goal_check_command("/goal check"));
        assert!(is_goal_check_command("  /goal   check  "));
        assert!(!is_goal_check_command("/goal set"));
        assert!(!is_goal_check_command("/goal"));
        assert!(!is_goal_check_command("/plan check"));
    }

    /// Ohne durchgereichte Plan-Dienste gibt es keinen Zielstand — und keinen
    /// Fehler.
    #[test]
    fn goal_cell_is_absent_without_plan_services() {
        let app = test_chat_app();

        assert!(goal_cell_for_command(&app, "/goal check").is_none());
    }

    #[test]
    fn tui_context_root_alignment_rejects_mismatched_prompt_and_tool_roots() {
        let error = ensure_tui_context_roots_align(
            std::path::Path::new("/workspace/prompt-project"),
            std::path::Path::new("/workspace/tool-project"),
        )
        .expect_err("mismatched prompt and tool roots must fail closed");

        match error {
            TuiError::Core(message) => {
                assert!(message.contains("mismatched prompt discovery root"));
                assert!(message.contains("/workspace/prompt-project"));
                assert!(message.contains("/workspace/tool-project"));
            }
            TuiError::Io(_) => panic!("root mismatch must use the typed core error path"),
            TuiError::ContextProviderRegistration(_) => {
                panic!("root mismatch is not a provider-namespace error")
            }
        }
    }
}
