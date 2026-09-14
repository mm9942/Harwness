//! # app
//!
//! Asynchroner ratatui-Chat-Renderer für die Harwness-TUI.
//!
//! ## Verantwortung
//!
//! Dieses Modul besitzt den interaktiven Chat-Loop (`run_loop`) und den
//! Renderer-Zustand ([`ChatApp`]). Es **montiert keine Laufzeit**: Session,
//! Registry, Sandbox, Spawn-Kontext, Freigabekette und Dienste baut die
//! gemeinsame Runtime-Montage ([`harw_runtime::RuntimeAssembly`]); die
//! TUI-Composition-Root (`crate::runtime_root`, `run_tui`) verdrahtet sie mit
//! diesem Modul (W2d-2, CONTRACTS-W2d2 §1.2). Der Loop treibt Turns über
//! [`harw_core::run_turn`] und betreibt einen **asynchronen**
//! `tokio::select!`-Event-Loop über drei Quellen (Spec-Abschnitt 2.1):
//! - Terminal-Eingaben (`TuiEvent`) von einem blockierenden
//!   Reader-Thread (`crate::input_reader::spawn_input_reader`),
//! - Anwendungsereignisse (`HarwEvent`) vom internen Bus,
//! - Frame-Anforderungen (`frame_requester`), koalesziert von
//!   `frame_scheduler`, der `TuiEvent::Draw` einspeist.
//!
//! - Terminal-Setup (Raw-Mode + Bracketed-Paste, **Alternate-Screen**) hinter
//!   einem RAII-Guard (`TerminalGuard`), der den Terminalzustand bei *jedem*
//!   Exit-Pfad (Fehler, Panic, sauberer Abbruch) zurückstellt.
//! - Verlauf wird intern in `ChatApp::cells` gespeichert und über einen
//!   scrollbaren `Paragraph`-Widget im Fullscreen-Layout gerendert. Kein
//!   `insert_before` / Terminal-Scrollback.
//! - Interner Scroll: PageUp / PageDown verschieben den Scroll-Offset innerhalb
//!   des History-Bereichs.
//! - Reine Zeilen-Logik (TTY-frei) ist in [`classify_line`] ausgelagert; die
//!   Tastensteuerung liegt in `handle_key`, das ausschließlich Zustand mutiert
//!   und `HarwEvent`s emittiert (testbar ohne Terminal).
//! - Da [`harw_core::ModelProvider`] kein Token-Streaming anbietet, wird die
//!   Antwort nach dem Turn **simuliert gestreamt**.
//!
//! ## Slash-Kommandos und `/tools`
//! - `/command`-Zeilen laufen über
//!   `crate::command_exec::execute_command_as`. Die Berechtigungsstufe kommt
//!   aus dem Principal der Montage (`crate::runtime_commands::caller_tier`),
//!   die Dienste aus ihrer Slash-Fläche
//!   (`crate::runtime_commands::slash_service_map`) — erst nach
//!   erfolgreicher Admission gebaut. Ohne Montage (`ChatApp::with_runtime`
//!   nicht aufgerufen) antwortet der Loop mit "Fehler: keine Runtime-Montage".
//! - `/tools` läuft lokal über
//!   [`crate::tools_command::dispatch_tools_command_bounded`] und kann die
//!   Decke der Session (`AgentSession::mode_ceiling`, Basis ∩ Modus) nie
//!   erweitern.
//!
//! ## Pausierte Turns (AP W5-03)
//! Ein Turn, den der Kern an einer Freigabe ([`TurnOutcome::AwaitingApproval`])
//! oder an einem Kind-Handoff ([`TurnOutcome::AwaitingChild`]) anhält, wird
//! **zu Ende geführt**, nicht abgebrochen: `drive_pauses_to_completion`
//! übergibt das Ergebnis an [`crate::approval::ApprovalDriver::drive_to_completion`]
//! und pollt dabei den Fragekanal, die Tastatur und die Turn-Ereignisse weiter,
//! während der Spinner läuft. Derselbe `Arc<TuiApprovalHandler>` liegt in der
//! Freigabekette der Wurzel-Session und im Treiber; beides richtet die
//! Composition-Root ein.
//!
//! ## Schlüsseltypen
//! - [`ChatApp`] — Zustand des Renderers (Eingabepuffer, Verlauf-Log, Popup, Scroll).
//! - [`Role`] — Rolle einer Chat-Nachricht (User, Assistant, System).
//! - [`LineAction`] — TTY-freies Ergebnis der Zeilen-Klassifizierung.
//! - [`TuiPlanServices`] — lesende Sicht auf Plan-/Ziel-Stores der Montage.
//! - [`TuiRunOutcome`] / [`ResumeSessionSelector`] — Ausgang des Loops und
//!   `/resume`-Auswahl.
//! - [`TuiError`] — Fehlertyp dieses Moduls.
//! - `TerminalGuard` — RAII-Guard für Raw-Mode und Alternate-Screen (crate-intern).
//! - `SharedHistoryCell` — Verlaufszelle, die nach dem Anhängen noch
//!   fortgeschrieben werden kann (privat).
//!
//! ## Crate-interne Schnittstelle zu `runtime_root`
//! `WELCOME`, `TerminalGuard::enter`, `frame_scheduler`, `run_loop`,
//! `install_loaded_history`, `tui_error_from_approval_driver`,
//! `ChatApp::push_lines`, `ChatApp::with_runtime` / `ChatApp::runtime` sind
//! `pub(crate)`.
//!
//! ## Nebenläufigkeit
//! Der asynchrone Loop läuft auf einem `current_thread`-Tokio-Runtime.
//! Ein OS-Thread liest Tastatur-Ereignisse blockierend; ein Tokio-Task
//! koalesziert Frame-Anforderungen. Alle drei kommunizieren über
//! unbounded MPSC-Kanäle. Die Montage wird als `Arc` geteilt und nur lesend
//! benutzt.
//!
//! ## Fehler
//! Alle Fehler dieses Moduls werden als [`TuiError`] ausgedrückt.
//!
//! ## Beispiele
//! ```ignore
//! use harw_tui::app::{ChatApp, Role};
//! // adapters/sandbox/session_id und die Montage liefert `crate::runtime_root`.
//! let mut app = ChatApp::new(adapters, sandbox, session_id).with_runtime(assembly);
//! app.push_line(Role::System, "Hinweis");
//! assert_eq!(app.cells_len(), 1);
//! ```

use std::cell::Cell;
use std::collections::HashMap;
use std::io::{self, Stdout, Write as _};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, EnableMouseCapture, KeyCode, KeyEvent,
    KeyModifiers,
};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use harw_core::cancel::{CancelReason, CancelToken};
use harw_core::turn_loop::TurnControl;
use harw_core::{
    AgentSession, ConversationHistory, CoreError, InteractionMode, ManagedAgentSpawner, ModelError,
    ModelMessage, TurnInput, TurnOutcome, run_turn,
};
use harw_extension_api::allow_rules::{ApprovalRule, RuleDecision, RuleScope, derive_shell_rule};
use harw_extension_api::approval_mode::ApprovalMode;
use harw_extension_api::registry::ContextProviderRegistrationError;
use harw_extension_api::{ToolCall, ToolName};
use harw_operations::SessionController;
use harw_operations::adapter::CommandAdapter;
use harw_plan::PlanStore;
use harw_plan::goal::{GoalStore, evaluate_goal};
use harw_protocol::events::{SessionEvent, TurnEvent};
use harw_protocol::items::{ContentPart, TurnItem};
use harw_sandbox::SandboxSpec;
use harw_types::SessionId;
use harw_types::TokenUsage;

use crate::CommandRegistry;
use crate::approval::{
    ApprovalDriver, ApprovalDriverError, ApprovalPrompt, ApprovalPromptReceiver, ChildTurnDriver,
};
use crate::approval_dialog::{ApprovalChoice, ApprovalDialog, ApprovalDialogRequest, DialogAction};
use crate::chat_scroll::{ChatScroll, ScrollAction};
use crate::choice_dialog::{ChoiceAction, ChoiceDialog};
use crate::clipboard::{self, ClipboardTarget};
use crate::command_exec::execute_command_as;
use crate::command_popup::{CommandPopup, PopupAction};
use crate::events::{HarwEvent, HarwEventSender};
use crate::export::{self, ExportEntry, ExportMeta, ExportOptions};
use crate::frame_requester::{FrameRequester, MIN_FRAME_INTERVAL};
use crate::history_cell::{
    AssistantHistoryCell, GoalCell, HistoryCell, PlainHistoryCell, PlanGraphCell,
    ReasoningHistoryCell, SharedToolCell, SubAgentCell, SubAgentStatus, ToolCell, ToolGroupCell,
    ToolVerbosity, UserHistoryCell,
};
use crate::input_editor::{InputAction, InputEditor};
use crate::input_history::InputHistoryStore;
use crate::runtime_commands;
// Nur Tests (über `use super::*`) rufen die in `runtime_root` gewanderte
// Coercion-Hilfe noch unqualifiziert auf; Prod in app.rs nutzt sie nicht.
#[cfg(test)]
use crate::runtime_root::as_dyn_approval_handler;
use crate::runtime_root::TitleJobContext;
use crate::session_controller::TuiSessionController;
use crate::session_picker::{PickerAction, SessionEntry, SessionPicker};
use crate::spinner::Spinner;
use crate::style;
use crate::tui_event::TuiEvent;

/// Taktrate der Spinner-Animation während ein Turn läuft.
const SPINNER_INTERVAL: Duration = Duration::from_millis(80);

/// Willkommens-Systemzeile beim Start des Chats.
pub(crate) const WELCOME: &str = "Willkommen. Tippe eine Nachricht — Enter zum Senden.";

const NON_TEXT_CONTENT_PLACEHOLDER: &str = "[non-text content]";

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
/// `ChildProgress` → `ChildCompleted`, [`ToolCell`] über angefordert →
/// abgeschlossen — braucht deshalb einen Seitenkanal auf **dieselbe** Instanz.
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
/// - `pending_tool_cells` korreliert `ToolCallRequested`/eine Freigabefrage →
///   `ToolCallCompleted` (letzteres trägt nur die `call_id`) über **dieselbe**
///   geteilte [`ToolCell`] (Plan Schritt 2, Contract-Slice B3) — ersetzt die
///   frühere `HashMap<ToolCallId, String>`, die nur den Werkzeugnamen für eine
///   zweite, separate Ergebniszelle vorhielt.
/// - `child_cells` findet zu einer `child_id` die **eine** bereits angehängte
///   [`SubAgentCell`] wieder, damit `ChildProgress`/`ChildCompleted` sie
///   fortschreiben statt eine zweite Zelle anzulegen.
///
/// Ein Eintrag in `child_cells` bleibt nach `ChildCompleted` bewusst bestehen:
/// eine verspätet eintreffende Fortschrittsmeldung soll dieselbe Zelle treffen
/// und keine neue erzeugen. `pending_tool_cells`-Einträge bleiben nach
/// `ToolCallCompleted` ebenfalls bestehen (statt entfernt zu werden): eine
/// Freigabefrage für denselben Aufruf, die knapp vor oder nach dem
/// `ToolCallRequested`-Ereignis eintrifft, muss dieselbe Zelle wiederfinden,
/// egal in welcher Reihenfolge beide beim Renderer ankommen (siehe
/// [`ensure_tool_cell`]).
#[derive(Debug, Default)]
struct TurnEventState {
    /// `call_id` → die eine geteilte Werkzeugzelle dieses Aufrufs.
    pending_tool_cells: HashMap<harw_types::ToolCallId, SharedToolCell>,
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

// ── Werkzeugzellen: Handle, Verbosity-Wrapper (Plan Schritt 2) ───────────────

/// Kennung einer einzelnen oder gruppierten Werkzeugzelle, wie sie
/// [`ChatApp`] für Ctrl+O „letzte bzw. alle aufklappen“ vorhält.
///
/// # Beschreibung
/// Nur **Top-Level**-Zellen bekommen ein Handle: eine [`ToolCell`], die einer
/// [`ToolGroupCell`] beigetreten ist, wird ausschließlich über die Gruppe
/// auf-/zugeklappt, nie einzeln (siehe [`ChatApp::append_tool_cell`]).
#[derive(Debug, Clone)]
enum ToolCellHandle {
    /// Eine einzelne, nicht gruppierte Werkzeugzelle.
    Single(SharedToolCell),
    /// Eine Sammelzelle aufeinanderfolgender lesender `fs.*`-Aufrufe.
    Group(Arc<Mutex<ToolGroupCell>>),
}

impl ToolCellHandle {
    /// Liest den aktuellen Ausklapp-Zustand.
    ///
    /// # Rückgabe
    /// Ein vergifteter Lock zählt als eingeklappt — konservativ für die
    /// „sind noch nicht alle ausgeklappt"-Entscheidung von Ctrl+O.
    fn is_expanded(&self) -> bool {
        match self {
            Self::Single(cell) => cell.lock().map(|guard| guard.expanded).unwrap_or(false),
            Self::Group(group) => group.lock().map(|guard| guard.expanded).unwrap_or(false),
        }
    }

    /// Setzt den Ausklapp-Zustand direkt (Ctrl+O „alle").
    fn set_expanded(&self, expanded: bool) {
        match self {
            Self::Single(cell) => {
                if let Ok(mut guard) = cell.lock() {
                    guard.set_expanded(expanded);
                }
            }
            Self::Group(group) => {
                if let Ok(mut guard) = group.lock() {
                    guard.expanded = expanded;
                }
            }
        }
    }

    /// Schaltet den Ausklapp-Zustand um (Ctrl+O „letzte").
    fn toggle_expanded(&self) {
        let next = !self.is_expanded();
        self.set_expanded(next);
    }
}

/// Verlaufszelle für eine geteilte Werkzeugzelle/-gruppe mit fest
/// eingebranntem [`ToolVerbosity`] zum Zeitpunkt des Anhängens.
///
/// # Beschreibung
/// Plan Schritt 2 „Verbosity". `tool_verbosity` ändert sich innerhalb einer
/// laufenden Sitzung nicht — es wird einmalig beim Start über
/// [`ChatApp::with_verbose_tools`] gesetzt (die CLI reicht `--verbose`
/// durch, ein anderer Slice) — ein Erfassen bei Erzeugung genügt daher, statt
/// bei jedem Rendern erneut `self.tool_verbosity` von der `App` zu lesen
/// (wofür `HistoryCell::display_lines` ohnehin keinen Zugriff böte).
#[derive(Debug)]
struct ToolHistoryCell {
    /// Die zugrunde liegende Einzel- oder Sammelzelle.
    handle: ToolCellHandle,
    /// Zum Anhängezeitpunkt aktive Verbosity.
    verbosity: ToolVerbosity,
}

impl HistoryCell for ToolHistoryCell {
    /// Delegiert an [`ToolCell::display_lines_with`] bzw.
    /// [`ToolGroupCell::display_lines_with`] mit der eingebrannten Verbosity;
    /// ein vergifteter Lock ergibt eine sichtbare Hinweiszeile statt eines Panics.
    fn display_lines(&self, width: u16, theme: style::Theme) -> Vec<Line<'static>> {
        match &self.handle {
            ToolCellHandle::Single(cell) => match cell.lock() {
                Ok(guard) => guard.display_lines_with(width, theme, self.verbosity),
                Err(_) => vec![Line::from(Span::styled(
                    "⚠ Werkzeugzelle nicht lesbar (Sperre vergiftet)".to_owned(),
                    style::warning_style(theme),
                ))],
            },
            ToolCellHandle::Group(group) => match group.lock() {
                Ok(guard) => guard.display_lines_with(width, theme, self.verbosity),
                Err(_) => vec![Line::from(Span::styled(
                    "⚠ Werkzeuggruppe nicht lesbar (Sperre vergiftet)".to_owned(),
                    style::warning_style(theme),
                ))],
            },
        }
    }
}

// ── Freigabemodus-Zyklus (Plan Schritt 5) ────────────────────────────────────

/// Die vier Stufen des Shift+Tab-Zyklus: `ask → auto → full → plan → ask`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PermissionCycleStage {
    /// Jeder Werkzeugaufruf wird bestätigt ([`ApprovalMode::AlwaysAsk`]).
    Ask,
    /// Harw entscheidet die harmlosen Fälle selbst ([`ApprovalMode::Delegated`]).
    Auto,
    /// Kein Werkzeugaufruf fragt nach ([`ApprovalMode::FullAccess`]).
    Full,
    /// Plan-Modus: Freigabe `AlwaysAsk` plus [`InteractionMode::Plan`].
    Plan,
}

impl PermissionCycleStage {
    /// Nächste Stufe im Zyklus (zyklisch, `Plan` → `Ask`).
    fn next(self) -> Self {
        match self {
            Self::Ask => Self::Auto,
            Self::Auto => Self::Full,
            Self::Full => Self::Plan,
            Self::Plan => Self::Ask,
        }
    }
}

// ── Vollflächige Overlays (Schritt 6/7) ──────────────────────────────────────

/// Vollflächiges Overlay, das den normalen Eingabe-/Popup-Pfad ersetzt.
///
/// # Beschreibung
/// Analog zum `/command`-Popup, aber exklusiv: solange ein Overlay offen ist,
/// gehen Tasten ausschließlich an das Overlay (siehe `handle_overlay_key`).
#[derive(Debug)]
enum Overlay {
    /// `/resume` ohne Argument öffnet eine filterbare Liste vergangener
    /// Sitzungen (Plan Schritt 7).
    SessionPicker(SessionPicker),
    /// `/export` ohne `--datei` öffnet die Auswahl Zwischenablage/Datei/Abbrechen
    /// (Contract „Nachträgliche Entscheidungen", Slice E1).
    ExportChoice(ChoiceDialog),
}

/// Plan- und Ziel-Dienste, die der Renderer für [`PlanGraphCell`] und
/// [`GoalCell`] braucht.
///
/// # Beschreibung
/// Die TUI öffnet **niemals** selbst einen Plan-/Goal-Store: zwei nebenläufige
/// Store-Instanzen auf demselben Verzeichnis wären ein Datenverlust-Risiko.
/// Die Runtime-Montage ([`harw_runtime::RuntimeAssembly`]) hält die Dienste
/// genau einmal ([`harw_runtime::PlanServices`]); dieser Typ ist nur die
/// schmale, lesende Sicht des Renderers darauf und entsteht über
/// `impl From<&harw_runtime::PlanServices>`.
///
/// Ziel-Kontext, zusätzliche Freigabe-Politiken und der Startmodus gehören
/// nicht mehr hierher: sie werden in der Runtime-Montage verdrahtet
/// (W2d-2, CONTRACTS-W2d2 §1.2).
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
}

/// Übernimmt Plan- und Ziel-Store aus den Plan-Diensten der Runtime-Montage.
///
/// # Beschreibung
/// Klont nur die beiden `Arc`-Zeiger (`plan` → `plan_store`,
/// `goal` → `goal_store`); es entsteht kein zweiter Store. Befund-Speicher und
/// Plan-Konfiguration braucht der Renderer nicht.
///
/// # Nebenläufigkeit
/// Rein lesend; zwei atomare Referenzzähler-Erhöhungen.
impl From<&harw_runtime::PlanServices> for TuiPlanServices {
    fn from(services: &harw_runtime::PlanServices) -> Self {
        Self {
            plan_store: Arc::clone(&services.plan),
            goal_store: Arc::clone(&services.goal),
        }
    }
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

/// Rolle, unter der eine Chat-Zelle angehängt wird.
///
/// # Beschreibung
/// Steuert, welche interne `HistoryCell`-Implementierung beim Anhängen einer
/// Nachricht in [`ChatApp::push_line`] erzeugt wird. Spec-Quelle: SLICE 7.
///
/// # Beispiele
/// ```ignore
/// use harw_tui::app::{ChatApp, Role};
/// // adapters/sandbox/session_id baut real `crate::runtime_root`.
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
    /// Operation-Adapter-Pipeline ausführen (`HarwEvent::Command` →
    /// `crate::command_exec::execute_command_as`); enthält die unveränderte
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
/// `/command`-Zeilen asynchron dispatcht werden (`HarwEvent::Command` →
/// `crate::command_exec::execute_command_as`). Die separate `command_registry`
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
/// // adapters/sandbox/session_id baut real `crate::runtime_root`.
/// let mut app = ChatApp::new(Vec::new(), sandbox, session_id);
/// app.push_line(Role::User, "Frage");
/// assert_eq!(app.cells_len(), 1);
/// ```
pub struct ChatApp {
    /// Logischer Nachrichtenverlauf als typisierte Zellen, älteste zuerst.
    cells: Vec<Box<dyn HistoryCell>>,
    /// Editor-Zustand für die aktuelle Eingabezeile (Puffer, Cursor, History).
    input: InputEditor,
    /// Eingabeereignisse, die während eines Turns ankamen und nach ihm erneut
    /// durch den normalen Editorpfad laufen. Dadurch bleibt der Composer auch
    /// bei Modellarbeit und Freigabefragen vollständig bedienbar.
    deferred_input: std::collections::VecDeque<TuiEvent>,
    /// Bereits abgeschickte Benutzertexte. Ein laufender Turn darf nie
    /// abgebrochen oder vermischt werden; diese FIFO wird ausschließlich an
    /// Turn-Grenzen abgearbeitet.
    pending_turns: std::collections::VecDeque<String>,
    /// Kooperativer Abbruchgriff für den gerade laufenden Turn. `Ctrl+C`
    /// löst ihn auch dann aus, wenn kein Freigabe-Dialog sichtbar ist.
    active_cancel: Option<CancelToken>,
    /// Ein erstes Escape schließt nur Popup/History-Navigation; ein zweites
    /// Escape leert den Composer.
    escape_armed: bool,
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
    /// Optionales Memory-Backend für die Korrektur-Erkennung
    /// (`harw_memory::detect_correction`) im Chat-Pfad. Die Slash-Dienste
    /// (inklusive Memory) stammen dagegen aus der Runtime-Montage.
    memory: Option<std::sync::Arc<dyn harw_memory::Memory>>,
    /// Die Runtime-Montage dieses Laufs. Liefert Principal (Berechtigungsstufe
    /// für Slash-Kommandos) und die Slash-[`harw_operations::ServiceMap`]. `None` bedeutet:
    /// `/command`-Zeilen werden mit "Fehler: keine Runtime-Montage" beantwortet.
    runtime: Option<Arc<harw_runtime::RuntimeAssembly>>,
    /// Erkannter Projekt-Root, der im Chat-Header angezeigt werden soll.
    /// Leer-String wenn kein Root ermittelt werden konnte (Fallback-Pfad).
    project_root: String,
    /// Langlebiger Session-Controller — hält `reasoning_effort`, `active_model`
    /// und `active_provider` über mehrere Turns hinweg und wird zwischen Turns
    /// via `apply_pending_controller_state` auf die
    /// [`harw_core::AgentSession`] angewendet.
    ///
    /// Die Composition-Root setzt über `with_session_controller` denselben
    /// Controller, den die Runtime-Montage in ihre Slash-/Modell-Dienste legt,
    /// sodass `/effort`-, `/model`- und `/mode`-Ops denselben Zustand sehen wie
    /// der Renderer.
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
    /// Detailgrad, mit dem Werkzeugzellen gerendert werden (Plan Schritt 2
    /// „Verbosity"); gesetzt über [`Self::with_verbose_tools`].
    tool_verbosity: ToolVerbosity,
    /// Alle bisher angehängten Top-Level-Werkzeugzellen (einzeln oder
    /// gruppiert), in Ankunftsreihenfolge — Grundlage für Ctrl+O „letzte bzw.
    /// alle aufklappen" (Plan Schritt 2).
    tool_cells: Vec<ToolCellHandle>,
    /// Die aktuell offene Lese-Gruppe aufeinanderfolgender `fs.*`-Aufrufe,
    /// sofern noch erweiterbar; `None` schließt implizit jede vorherige
    /// Gruppe (Plan Schritt 2 „Gruppierung").
    open_tool_group: Option<Arc<Mutex<ToolGroupCell>>>,
    /// Ctrl+O-Vormerkung: `true` unmittelbar nach einem Druck, der nur die
    /// letzte Werkzeugzelle umgeschaltet hat — ein erneuter Druck schaltet
    /// dann alle um (Plan Schritt 2).
    ctrl_o_expand_last_armed: bool,
    /// Die aktuell offene Freigabefrage, die anstelle des Composers gezeichnet
    /// wird (Plan Schritt 3); `None` zeigt den normalen Composer.
    pending_approval_dialog: Option<ApprovalDialog>,
    /// Vorgemerktes Ziel des Shift+Tab-Zyklus, solange ein Turn läuft (Plan
    /// Schritt 5, AP W5-05: der Wechsel gilt erst an der nächsten Turn-Grenze).
    pending_permission_stage: Option<PermissionCycleStage>,
    /// Interaktionsmodus, zu dem der Zyklus nach Verlassen der `Plan`-Stufe
    /// zurückkehrt (Plan Schritt 5).
    mode_before_plan: Option<InteractionMode>,
    /// Vollflächiges Overlay (Session-Picker, `/export`-Auswahl), das den
    /// normalen Eingabe-/Popup-Pfad ersetzt (Plan Schritt 6/7).
    overlay: Option<Overlay>,
    /// Gesprächsverlauf als quellcode-unabhängige Einträge für `/export`
    /// (Contract „Nachträgliche Entscheidungen", Slice E1); parallel zu
    /// `cells` aufgebaut, weil [`HistoryCell`] keinen Text-Accessor bietet.
    export_entries: Vec<ExportEntry>,
    /// Anzeigetitel der Sitzung, sobald bekannt (Plan Schritt 7); `None` zeigt
    /// keinen Titel in der Statuszeile.
    session_title: Option<String>,
    /// Kontext für die einmalige Titel-Job-Anstoßung nach dem ersten
    /// abgeschlossenen Turn (Plan Schritt 7); wird beim ersten Gebrauch über
    /// `take()` konsumiert.
    title_job_context: Option<TitleJobContext>,
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
            .field("turn_running", &self.active_cancel.is_some())
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
            .field("has_runtime", &self.runtime.is_some())
            .field("tool_verbosity", &self.tool_verbosity)
            .field("tool_cells_len", &self.tool_cells.len())
            .field(
                "has_pending_approval_dialog",
                &self.pending_approval_dialog.is_some(),
            )
            .field("pending_permission_stage", &self.pending_permission_stage)
            .field("has_overlay", &self.overlay.is_some())
            .field("export_entries_len", &self.export_entries.len())
            .field("session_title", &self.session_title)
            .field("has_title_job_context", &self.title_job_context.is_some())
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
    /// `session_id` werden von der Composition-Root (`crate::runtime_root`)
    /// übernommen und bei jedem `/command`-Dispatch verwendet.
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
    /// // adapters/sandbox/session_id baut real `crate::runtime_root`.
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
    /// Hinterlegt `memory` für die Korrektur-Erkennung im Chat-Pfad. Die
    /// Dienste der `/memory`-Kommandos kommen aus der Runtime-Montage
    /// (`Self::with_runtime`).
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
            pending_turns: std::collections::VecDeque::new(),
            active_cancel: None,
            escape_armed: false,
            scroll: ChatScroll::new(),
            input_history,
            command_registry: CommandRegistry::from_command_adapters(&adapters),
            command_popup: None,
            theme: style::detect_theme(),
            total_usage: TokenUsage::default(),
            adapters,
            sandbox,
            session_id,
            memory,
            runtime: None,
            project_root: String::new(),
            session_controller: Arc::new(TuiSessionController::new()),
            last_history_total_lines: Cell::new(0),
            last_history_visible_rows: Cell::new(20),
            managed_spawner: None,
            active_mode: InteractionMode::default(),
            plan_services: None,
            tool_verbosity: ToolVerbosity::Compact,
            tool_cells: Vec::new(),
            open_tool_group: None,
            ctrl_o_expand_last_armed: false,
            pending_approval_dialog: None,
            pending_permission_stage: None,
            mode_before_plan: None,
            overlay: None,
            export_entries: Vec::new(),
            session_title: None,
            title_job_context: None,
        }
    }

    /// Reuses a runtime-owned controller for both slash commands and model
    /// tool operations in this chat session.
    #[must_use]
    pub(crate) fn with_session_controller(mut self, controller: Arc<TuiSessionController>) -> Self {
        self.session_controller = controller;
        self
    }

    /// Hinterlegt die Runtime-Montage dieses Laufs.
    ///
    /// # Beschreibung
    /// Aus der Montage bezieht `run_loop` für jede `/command`-Zeile die
    /// Berechtigungsstufe des Aufrufers
    /// ([`crate::runtime_commands::caller_tier`] über
    /// [`harw_runtime::RuntimeAssembly::principal`]) und die Slash-Dienste
    /// ([`crate::runtime_commands::slash_service_map`] über
    /// [`harw_runtime::RuntimeAssembly::services`]).
    ///
    /// # Argumente
    /// - `assembly` (`Arc<harw_runtime::RuntimeAssembly>`): geteilte Montage;
    ///   Besitz des `Arc` geht über.
    ///
    /// # Rückgabe
    /// `Self` für Builder-Verkettung.
    #[must_use]
    pub(crate) fn with_runtime(mut self, assembly: Arc<harw_runtime::RuntimeAssembly>) -> Self {
        self.runtime = Some(assembly);
        self
    }

    /// Gibt die hinterlegte Runtime-Montage zurück.
    ///
    /// # Rückgabe
    /// `Option<&Arc<harw_runtime::RuntimeAssembly>>` — `None`, solange
    /// [`Self::with_runtime`] nicht aufgerufen wurde.
    #[must_use]
    pub(crate) fn runtime(&self) -> Option<&Arc<harw_runtime::RuntimeAssembly>> {
        self.runtime.as_ref()
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

    /// Setzt den Detailgrad, mit dem Werkzeugzellen gerendert werden (Plan
    /// Schritt 2 „Verbosity").
    ///
    /// # Beschreibung
    /// Öffentliche, von `--verbose` unabhängige API: der interne
    /// [`ToolVerbosity`]-Typ ist `pub(crate)` und kann deshalb nicht direkt in
    /// einer öffentlichen Signatur erscheinen. Die CLI (ein anderer Slice)
    /// reicht `--verbose` als `bool` durch; `true` entspricht
    /// `ToolVerbosity::Verbose` (zusätzlich immer ausgeklappt, rohe Argumente
    /// sichtbar), `false` dem Default `ToolVerbosity::Compact`.
    ///
    /// # Argumente
    /// - `verbose` (`bool`): `true` aktiviert die ausführliche Darstellung.
    ///
    /// # Rückgabe
    /// `Self` für Builder-Verkettung.
    #[must_use]
    pub fn with_verbose_tools(mut self, verbose: bool) -> Self {
        self.tool_verbosity = if verbose {
            ToolVerbosity::Verbose
        } else {
            ToolVerbosity::Compact
        };
        self
    }

    /// Hinterlegt den Kontext für die einmalige Titel-Job-Anstoßung nach dem
    /// ersten abgeschlossenen Turn (Plan Schritt 7).
    ///
    /// # Beschreibung
    /// Wird von der Composition-Root (`crate::runtime_root::build_root_runtime`)
    /// gesetzt, wenn Titel-Generierung konfiguriert und eine
    /// Session-Store-Wurzel bekannt ist. `run_loop` löst den Job beim ersten
    /// `SessionEvent::TurnCompleted` ein und verwirft den Kontext danach
    /// (`Option::take`) — er wird also höchstens einmal je Sitzung verwendet.
    ///
    /// # Rückgabe
    /// `Self` für Builder-Verkettung.
    #[must_use]
    pub(crate) fn with_title_job_context(mut self, ctx: TitleJobContext) -> Self {
        self.title_job_context = Some(ctx);
        self
    }

    /// Setzt den Anzeigetitel der Sitzung (Plan Schritt 7).
    ///
    /// # Beschreibung
    /// Öffentlicher Setter, damit eine spätere Verdrahtung (Titel-Job-Ergebnis,
    /// `/resume`-Wiederherstellung) den Titel unabhängig vom Konstruktor
    /// nachtragen kann. Erscheint danach in der Statuszeile ([`status_line`]).
    ///
    /// # Argumente
    /// - `title` (`impl Into<String>`): der neue Anzeigetitel.
    pub fn set_session_title(&mut self, title: impl Into<String>) {
        self.session_title = Some(title.into());
    }

    /// Gibt den aktuellen Anzeigetitel der Sitzung zurück, falls bekannt.
    #[must_use]
    pub(crate) fn session_title(&self) -> Option<&str> {
        self.session_title.as_deref()
    }

    /// Öffnet den Session-Picker als Vollflächen-Overlay (Plan Schritt 7).
    ///
    /// # Beschreibung
    /// Aufgerufen von der Composition-Root (`runtime_root::run_tui`), nachdem
    /// `/resume` ohne Argument den Loop mit
    /// `TuiRunOutcome::Resume { selector: None }` verlassen hat und die
    /// verfügbaren Sitzungen geladen wurden. Ein bereits offenes Overlay wird
    /// ersetzt.
    ///
    /// # Argumente
    /// - `entries` (`Vec<SessionEntry>`): die anzuzeigenden Sitzungen.
    pub(crate) fn open_session_picker(&mut self, entries: Vec<SessionEntry>) {
        self.overlay = Some(Overlay::SessionPicker(SessionPicker::new(
            entries,
            SystemTime::now(),
        )));
    }

    /// Öffnet die `/export`-Zielauswahl als Vollflächen-Overlay.
    ///
    /// # Spec
    /// Contract „Nachträgliche Entscheidungen" (Slice E1): „In die
    /// Zwischenablage kopieren" / „Als Datei speichern" / „Abbrechen".
    fn open_export_choice(&mut self) {
        self.overlay = Some(Overlay::ExportChoice(ChoiceDialog::new(
            "Export",
            Some("Wie soll die Session exportiert werden?".to_owned()),
            vec![
                "In die Zwischenablage kopieren".to_owned(),
                "Als Datei speichern".to_owned(),
                "Abbrechen".to_owned(),
            ],
        )));
    }

    /// Gibt `true` zurück, wenn gerade ein Vollflächen-Overlay geöffnet ist.
    #[must_use]
    fn has_overlay(&self) -> bool {
        self.overlay.is_some()
    }

    /// Leitet die aktuell wirksame [`PermissionCycleStage`] aus dem
    /// Freigabemodus der Montage und dem aktiven Interaktionsmodus ab (Plan
    /// Schritt 5).
    ///
    /// # Rückgabe
    /// [`PermissionCycleStage::Plan`], wenn die Sitzung im Plan-Modus ist;
    /// sonst die Stufe, die dem aktuellen [`ApprovalMode`] entspricht. Ohne
    /// Runtime-Montage gilt [`PermissionCycleStage::Ask`] als sicherer Default.
    #[must_use]
    pub(crate) fn current_permission_stage(&self) -> PermissionCycleStage {
        if self.active_mode == InteractionMode::Plan {
            return PermissionCycleStage::Plan;
        }
        match self.runtime.as_ref().map(|rt| rt.approval_mode().get()) {
            Some(ApprovalMode::AlwaysAsk) | None => PermissionCycleStage::Ask,
            Some(ApprovalMode::Delegated) => PermissionCycleStage::Auto,
            Some(ApprovalMode::FullAccess) => PermissionCycleStage::Full,
        }
    }

    /// Gibt die vorgemerkte, noch nicht angewendete Zyklus-Stufe zurück, falls
    /// ein Wechsel während eines laufenden Turns angefordert wurde (Plan
    /// Schritt 5).
    #[must_use]
    pub(crate) fn pending_permission_stage(&self) -> Option<PermissionCycleStage> {
        self.pending_permission_stage
    }

    /// Schreibt einen neuen [`ApprovalMode`] in die geteilte Zelle der
    /// Montage; ohne Montage ein No-op (es gibt dann nichts zu schreiben).
    fn set_approval_mode(&self, mode: ApprovalMode) {
        if let Some(rt) = self.runtime.as_ref() {
            rt.approval_mode().set(mode);
        }
    }

    /// Setzt eine Stufe des Shift+Tab-Zyklus sofort um (Plan Schritt 5).
    ///
    /// # Beschreibung
    /// `Plan` merkt sich den bisherigen Interaktionsmodus
    /// (`Self::mode_before_plan`) und fordert [`InteractionMode::Plan`] an;
    /// jede andere Stufe setzt nur den Freigabemodus — kehrt der Zyklus dabei
    /// gerade aus `Plan` zurück, wird zusätzlich der gemerkte vorherige Modus
    /// angefordert. Beide Anforderungen laufen über
    /// [`harw_operations::SessionController::request_mode`] und wirken daher
    /// erst an der nächsten Turn-Grenze (AP W5-05, siehe
    /// [`Self::apply_pending_controller_state`]).
    ///
    /// # Argumente
    /// - `stage` ([`PermissionCycleStage`]): die anzuwendende Zielstufe.
    fn apply_permission_stage(&mut self, stage: PermissionCycleStage) {
        match stage {
            PermissionCycleStage::Plan => {
                if self.mode_before_plan.is_none() {
                    self.mode_before_plan = Some(self.active_mode);
                }
                self.set_approval_mode(ApprovalMode::AlwaysAsk);
                self.active_mode = InteractionMode::Plan;
                if let Err(error) =
                    self.session_controller.request_mode(InteractionMode::Plan.as_str())
                {
                    tracing::warn!(
                        error = %error,
                        "tui.permission_cycle.request_plan_mode_failed"
                    );
                }
            }
            PermissionCycleStage::Ask | PermissionCycleStage::Auto | PermissionCycleStage::Full => {
                let approval = match stage {
                    PermissionCycleStage::Ask => ApprovalMode::AlwaysAsk,
                    PermissionCycleStage::Auto => ApprovalMode::Delegated,
                    PermissionCycleStage::Full => ApprovalMode::FullAccess,
                    PermissionCycleStage::Plan => unreachable!("oben behandelt"),
                };
                self.set_approval_mode(approval);
                if let Some(previous) = self.mode_before_plan.take() {
                    self.active_mode = previous;
                    if let Err(error) = self.session_controller.request_mode(previous.as_str()) {
                        tracing::warn!(
                            error = %error,
                            "tui.permission_cycle.restore_mode_failed"
                        );
                    }
                }
            }
        }
    }

    /// Verarbeitet einen Shift+Tab-Druck: zyklischer Wechsel
    /// `ask → auto → full → plan → ask` (Plan Schritt 5).
    ///
    /// # Argumente
    /// - `turn_running` (`bool`): `true`, wenn gerade ein Turn läuft — der
    ///   Zielzustand wird dann nur vorgemerkt (AP W5-05) und die Statuszeile
    ///   zeigt „(ab nächstem Turn)", statt sofort zu wirken.
    pub(crate) fn cycle_permission_stage(&mut self, turn_running: bool) {
        let current = self
            .pending_permission_stage
            .unwrap_or_else(|| self.current_permission_stage());
        let next = current.next();
        // Freigabestufen gelten sofort, auch während ein Turn läuft. Der
        // Interaktionsmodus wird vom Kern weiter an der Turn-Grenze übernommen,
        // aber der sichtbare Zielzustand aktualisiert sich unverzüglich.
        let _ = turn_running;
        self.pending_permission_stage = None;
        self.apply_permission_stage(next);
    }

    /// Schließt die aktuell offene Lese-Gruppe (Plan Schritt 2 „Gruppierung").
    ///
    /// # Beschreibung
    /// Aufgerufen, sobald ein anderes Werkzeug, Assistententext oder eine
    /// sonstige Verlaufszelle angehängt wird — die nächste passende `fs.*`-
    /// Anfrage beginnt danach eine neue Gruppe statt der alten beizutreten.
    fn close_tool_group(&mut self) {
        self.open_tool_group = None;
    }

    /// Hängt eine neue, geteilte Werkzeugzelle an den Verlauf an und gruppiert
    /// sie bei Bedarf mit der zuletzt offenen Lese-Gruppe (Plan Schritt 2).
    ///
    /// # Beschreibung
    /// Passt `tool_name` in [`ToolGroupCell::accepts`], wird die Zelle der
    /// zuletzt offenen Gruppe beigetreten (oder eine neue Gruppe eröffnet);
    /// sonst wird jede offene Gruppe geschlossen und die Zelle einzeln
    /// angehängt. Nur Top-Level-Zellen/-Gruppen bekommen ein
    /// [`ToolCellHandle`] in `self.tool_cells` (Ctrl+O „alle").
    ///
    /// # Argumente
    /// - `tool_name` (`&str`): roher Werkzeugname des Aufrufs.
    /// - `cell` ([`SharedToolCell`]): die neu erzeugte, geteilte Zelle.
    fn append_tool_cell(&mut self, tool_name: &str, cell: SharedToolCell) {
        if ToolGroupCell::accepts(tool_name) {
            if let Some(group) = self.open_tool_group.clone() {
                if let Ok(mut guard) = group.lock() {
                    guard.push(cell);
                }
                return;
            }
            let group = Arc::new(Mutex::new(ToolGroupCell::new()));
            if let Ok(mut guard) = group.lock() {
                guard.push(cell);
            }
            self.push_cell(Box::new(ToolHistoryCell {
                handle: ToolCellHandle::Group(Arc::clone(&group)),
                verbosity: self.tool_verbosity,
            }));
            self.tool_cells.push(ToolCellHandle::Group(Arc::clone(&group)));
            self.open_tool_group = Some(group);
        } else {
            self.close_tool_group();
            self.push_cell(Box::new(ToolHistoryCell {
                handle: ToolCellHandle::Single(Arc::clone(&cell)),
                verbosity: self.tool_verbosity,
            }));
            self.tool_cells.push(ToolCellHandle::Single(cell));
        }
    }

    /// Verarbeitet Ctrl+O: klappt die zuletzt angehängte Werkzeugzelle auf/zu,
    /// ein unmittelbar folgender zweiter Druck klappt stattdessen alle um
    /// (Plan Schritt 2).
    ///
    /// # Rückgabe
    /// `true`, wenn mindestens eine Zelle vorhanden war (Redraw nötig);
    /// `false` bei leerem Verlauf.
    pub(crate) fn toggle_tool_cells(&mut self) -> bool {
        if self.tool_cells.is_empty() {
            return false;
        }
        if self.ctrl_o_expand_last_armed {
            let target = !self.tool_cells.iter().all(ToolCellHandle::is_expanded);
            for handle in &self.tool_cells {
                handle.set_expanded(target);
            }
            self.ctrl_o_expand_last_armed = false;
        } else {
            if let Some(last) = self.tool_cells.last() {
                last.toggle_expanded();
            }
            self.ctrl_o_expand_last_armed = true;
        }
        true
    }

    /// Gibt `true` zurück, wenn mindestens eine Werkzeugzelle eingeklappt ist
    /// (Statuszeilen-Hinweis auf Ctrl+O, Plan Schritt 5).
    #[must_use]
    fn has_collapsed_tool_cells(&self) -> bool {
        self.tool_cells.iter().any(|handle| !handle.is_expanded())
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
    /// Called from the composition root (`crate::runtime_root`) with the
    /// project root of the runtime assembly.
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
    /// AP W5-05 — `/mode` wirkt an der Turn-Grenze. Löst zusätzlich einen
    /// während des letzten Turns vorgemerkten Shift+Tab-Zyklus ein (Plan
    /// Schritt 5, [`Self::cycle_permission_stage`]).
    pub(crate) fn apply_pending_controller_state(&mut self, session: &mut AgentSession) -> bool {
        if let Some(stage) = self.pending_permission_stage.take() {
            self.apply_permission_stage(stage);
        }
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
        // Eine neue User-/Assistant-/System-Zeile schließt jede offene
        // Lese-Gruppe (Plan Schritt 2 „Gruppierung": „sobald … Assistententext
        // kommt, wird die Gruppe geschlossen").
        self.close_tool_group();
        self.export_entries.push(match role {
            Role::User => ExportEntry::User(text.clone()),
            Role::Assistant => ExportEntry::Assistant(text.clone()),
            Role::System => ExportEntry::System(text.clone()),
        });
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
    pub(crate) fn push_lines(&mut self, lines: Vec<Line<'static>>) {
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
/// Aktiviert Raw-Mode, `EnterAlternateScreen`, Bracketed-Paste und Maus-Capture und baut
/// ein ratatui-Fullscreen-Terminal. Das Mausrad wird als `MouseEvent` an den vorhandenen
/// Chat-Scrollpfad geliefert, statt vom Terminal in `Up`/`Down` für die Input-History
/// übersetzt zu werden. Shift+Mauszug bleibt terminalseitig für die Textauswahl verfügbar.
///
/// # Nebenläufigkeit
/// Nicht thread-sicher; ausschließlich vom Renderer-Thread verwendet.
pub(crate) struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    /// Aktiviert Raw-Mode, Alternate-Screen, Bracketed-Paste und Maus-Capture.
    ///
    /// # Beschreibung
    /// Reihenfolge beim Aufbau:
    /// 1. Raw-Mode aktivieren.
    /// 2. `EnterAlternateScreen`, `EnableBracketedPaste` und `EnableMouseCapture` senden.
    /// 3. Backend + Fullscreen-`Terminal` bauen.
    ///
    /// Bei jedem Fehler nach Schritt 1 wird bereits aktivierter Zustand best-effort zurückgestellt.
    ///
    /// # Fehler
    /// [`TuiError::Io`], wenn Terminal-Setup fehlschlägt.
    pub(crate) fn enter() -> Result<Self, TuiError> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = crossterm::execute!(
            stdout,
            crossterm::terminal::EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture,
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

pub(crate) fn install_loaded_history(
    session: &mut AgentSession,
    app: &mut ChatApp,
    history: ConversationHistory,
) {
    hydrate_visible_history(app, &history);
    *session.history_mut() = history;
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
pub(crate) fn tui_error_from_approval_driver(error: ApprovalDriverError) -> TuiError {
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
/// - [`HarwEvent::Command`] → `/resume` beendet den Loop mit
///   [`TuiRunOutcome::Resume`]; `/tools` läuft lokal und gedeckelt über
///   [`crate::tools_command::dispatch_tools_command_bounded`]; jede andere
///   `/command`-Zeile läuft asynchron über die Operation-Adapter-Pipeline
///   ([`crate::command_exec::execute_command_as`] mit `app.adapters()` /
///   `app.sandbox()` / `app.session_id()`, Berechtigungsstufe aus dem
///   Principal der Montage, Dienste aus deren Slash-Fläche; ohne Montage die
///   Zeile "Fehler: keine Runtime-Montage") und das Ergebnis wie bei
///   `SystemMessage` in die History übernehmen,
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
pub(crate) async fn run_loop(
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
        // Eine während des vorherigen Turns abgeschickte Eingabe hat Vorrang.
        // Sie passiert denselben Pfad wie eine frische `HarwEvent::Submit`.
        let mut submitted: Option<String> = app.pending_turns.pop_front();

        if submitted.is_none() { tokio::select! {
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
                        // Ein Turn läuft nie parallel zum nächsten. Falls bereits
                        // etwas zur Verarbeitung bereitsteht, bleibt diese Eingabe
                        // FIFO erhalten.
                        if submitted.is_some() {
                            app.pending_turns.push_back(text);
                        } else {
                            submitted = Some(text);
                        }
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
                            // Phase 2: take the session ceiling (base ∩ mode) as an
                            // owned value first, then mutate activation via the
                            // parsed args. `/tools` may narrow, but never widen
                            // beyond this ceiling (W2d-1/F-T, E8).
                            let ceiling = gateway.session_mut().mode_ceiling();
                            Some(crate::tools_command::dispatch_tools_command_bounded(
                                &args,
                                &tool_names,
                                gateway.session_mut().activation_mut(),
                                &ceiling,
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
                            // `/compact` mutiert die aktive TUI-Sitzung direkt. Die
                            // Operation-Adapter besitzen absichtlich keinen Zugriff auf
                            // den lebenden `AgentSession`; über sie wäre deshalb nur der
                            // frühere Not-available-Stub erreichbar. Der gekürzte Verlauf
                            // wird sofort persistiert, damit ein anschließendes `/resume`
                            // denselben Kontext erhält.
                            let output = if raw.trim() == "/compact" {
                                const COMPACT_HISTORY_BYTES: usize = 128 * 1024;
                                let (session, store, _) = gateway.borrow_turn_ctx();
                                let before = session.history().len();
                                let (compacted, _used_bytes, dropped) = session
                                    .history()
                                    .tail_within_estimated_bytes(COMPACT_HISTORY_BYTES);
                                *session.history_mut() = compacted;
                                let after = session.history().len();
                                match store.save_history(session.id(), session.history()).await {
                                    Ok(()) => format!(
                                        "Session-Kontext komprimiert: {dropped} ältere Einträge entfernt ({before} → {after})."
                                    ),
                                    Err(error) => format!(
                                        "Session-Kontext wurde nur im Speicher komprimiert; Persistenz fehlgeschlagen: {error}"
                                    ),
                                }
                            } else {
                            // `/command`-Zeile asynchron über die Operation-Adapter-
                            // Pipeline ausführen; identischer Render-/Redraw-Pfad wie
                            // bei `SystemMessage` (mehrzeilige Ausgaben an `\n`
                            // aufteilen). Berechtigungsstufe und Slash-Dienste
                            // stammen aus der Runtime-Montage; die Dienste werden
                            // erst nach erfolgreicher Admission gebaut.
                            match app.runtime() {
                                Some(rt) => {
                                    execute_command_as(
                                        app.adapters(),
                                        app.sandbox(),
                                        app.session_id(),
                                        runtime_commands::caller_tier(rt.principal()),
                                        &raw,
                                        || runtime_commands::slash_service_map(rt.services()),
                                    )
                                    .await
                                }
                                None => {
                                    tracing::error!("tui.command.no_runtime_assembly");
                                    "Fehler: keine Runtime-Montage".to_owned()
                                }
                            }
                            };
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
                            // `/export`: bei `--datei <pfad>` direkt schreiben,
                            // sonst die Zielauswahl öffnen (Slice E1).
                            if let Some(request) = export_request_for_command(&raw) {
                                match request.path {
                                    Some(path) => {
                                        let opts = ExportOptions {
                                            include_tool_calls: request.include_tool_calls,
                                            ..ExportOptions::default()
                                        };
                                        let markdown = build_export_markdown(app, &opts);
                                        match export::write_export(
                                            std::path::Path::new(&path),
                                            &markdown,
                                        ) {
                                            Ok(()) => app.push_line(
                                                Role::System,
                                                format!("Export gespeichert: {path}"),
                                            ),
                                            Err(error) => app.push_line(
                                                Role::System,
                                                format!("Export fehlgeschlagen: {error}"),
                                            ),
                                        }
                                    }
                                    None => app.open_export_choice(),
                                }
                            }
                            frame_req.schedule_frame();
                        }
                    }
                }
            }
            maybe_sev = event_rx.recv() => {
                if let Some(SessionEvent::TurnCompleted { usage, .. }) = maybe_sev {
                    app.total_usage.add(&usage);
                    // Plan Schritt 7: einmalige Titel-Job-Anstoßung nach dem
                    // ersten abgeschlossenen Turn der Sitzung. Der Kontext wird
                    // dabei verbraucht (`Option::take`) — höchstens ein Versuch
                    // je Sitzung, unabhängig davon, ob ein Modell auflösbar war
                    // (siehe `TitleJobContext`-Doku in `runtime_root.rs`).
                    if let Some(ctx) = app.title_job_context.take() {
                        let model = ctx.title_model.clone().or_else(|| {
                            gateway
                                .session_mut()
                                .active_model()
                                .map(|id| id.as_str().to_owned())
                        });
                        match model {
                            Some(model) => harw_runtime::spawn_title_job(
                                ctx.session_store_root,
                                app.session_id().clone(),
                                ctx.provider,
                                model,
                            ),
                            None => tracing::debug!(
                                "tui.session_title.no_model_available_skipping_job"
                            ),
                        }
                    }
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
        } }

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

/// Findet oder erzeugt die eine geteilte [`ToolCell`] für `call_id` (Plan
/// Schritt 2/3, Contract-Slice B3).
///
/// # Beschreibung
/// Eine Freigabefrage für einen Aufruf trifft ein, **bevor** dessen
/// `ToolCallCompleted` feststeht, aber unter Umständen knapp **vor oder nach**
/// dem zugehörigen `TurnEvent::ToolCallRequested` — beide laufen über
/// getrennte Kanäle (`turn_event_rx` bzw. der Fragekanal des
/// [`ApprovalDriver`]), deren relative Ankunftsreihenfolge nicht garantiert
/// ist. Diese Funktion macht beide Aufrufer robust: existiert bereits eine
/// Zelle für `call_id` (`state.pending_tool_cells`), wird sie unverändert
/// zurückgegeben (keine zweite Zelle, kein zweiter Verlaufseintrag); sonst
/// wird sie über [`ToolCell::started`] neu angelegt, in `state` vermerkt und
/// über [`ChatApp::append_tool_cell`] an den Verlauf gehängt.
///
/// # Argumente
/// - `app` (`&mut ChatApp`): Renderer-Zustand, der die Zelle aufnimmt.
/// - `state` (`&mut TurnEventState`): hält `call_id → Zelle`.
/// - `call_id` (`harw_types::ToolCallId`): Kennung des Aufrufs.
/// - `call` (`&ToolCall`): vollständiger Aufruf (Name + Argumente).
///
/// # Rückgabe
/// Die eine, geteilte [`SharedToolCell`] dieses Aufrufs.
fn ensure_tool_cell(
    app: &mut ChatApp,
    state: &mut TurnEventState,
    call_id: harw_types::ToolCallId,
    call: &ToolCall,
) -> SharedToolCell {
    if let Some(existing) = state.pending_tool_cells.get(&call_id) {
        return Arc::clone(existing);
    }
    let cell: SharedToolCell = Arc::new(Mutex::new(ToolCell::started(call)));
    state.pending_tool_cells.insert(call_id, Arc::clone(&cell));
    app.append_tool_cell(call.name.as_str(), Arc::clone(&cell));
    cell
}

/// Verarbeitet genau ein [`TurnEvent`] in den Renderer-Zustand.
///
/// # Beschreibung
/// AP W5-10b. Als freie Funktion ausgelagert, damit sowohl der Event-Loop
/// ([`run_loop`]) als auch der laufende Turn ([`drive_turn_animated`]) dieselbe
/// Verarbeitung benutzen — und damit sie ohne Terminal testbar ist.
///
/// Zuordnung Ereignis → Zelle:
/// - `ToolCallRequested` → **eine** geteilte [`ToolCell`] je `call_id`, über
///   [`ensure_tool_cell`] angelegt bzw. wiedergefunden (Plan Schritt 2:
///   „eine Zelle statt zwei"); aufeinanderfolgende lesende `fs.*`-Aufrufe
///   werden über [`ChatApp::append_tool_cell`] zu einer [`ToolGroupCell`]
///   gebündelt,
/// - `ToolCallCompleted` → schreibt **dieselbe** Zelle über [`ToolCell::complete`]
///   fort (kein zweiter Verlaufseintrag),
/// - `ItemAdded(Reasoning)` → [`ReasoningHistoryCell`], nur bei nicht-leerer
///   Zusammenfassung; schließt eine offene Lese-Gruppe,
/// - `ChildSpawned` → **eine** [`SubAgentCell`] je `child_id`, geteilt über
///   [`SharedHistoryCell`]; schließt eine offene Lese-Gruppe,
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
/// - `state` (`&mut TurnEventState`): Seitenkanäle (Werkzeugzellen, Kind-Zellen).
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
            let already_known = state.pending_tool_cells.contains_key(&call_id);
            let call = ToolCall {
                id: call_id.clone(),
                name: ToolName::new(tool_name),
                arguments,
            };
            ensure_tool_cell(app, state, call_id, &call);
            // Bereits während einer Freigabefrage angelegt (Wettlauf zwischen
            // den beiden Kanälen, siehe `ensure_tool_cell`-Doku): kein neuer
            // sichtbarer Zustand, kein Redraw nötig.
            !already_known
        }
        TurnEvent::ToolCallCompleted {
            call_id,
            result,
            duration_ms,
            ..
        } => {
            let Some(cell) = state.pending_tool_cells.get(&call_id).cloned() else {
                tracing::warn!(call_id = %call_id, "tui.tool_cell.completed_without_request");
                return false;
            };
            let export_entry = match cell.lock() {
                Ok(mut guard) => {
                    guard.complete(&result, duration_ms);
                    Some(ExportEntry::Tool {
                        label: guard.label.clone(),
                        summary: guard.summary.clone(),
                    })
                }
                Err(_) => {
                    tracing::error!(call_id = %call_id, "tui.tool_cell.lock_poisoned");
                    None
                }
            };
            if let Some(entry) = export_entry {
                app.export_entries.push(entry);
            }
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
            app.close_tool_group();
            app.export_entries.push(ExportEntry::Reasoning(summary.clone()));
            app.push_cell(Box::new(ReasoningHistoryCell { summary }));
            true
        }
        TurnEvent::ChildSpawned {
            child,
            role,
            question,
            ..
        } => {
            app.close_tool_group();
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
                    app.close_tool_group();
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

// ── `/export` (Contract „Nachträgliche Entscheidungen", Slice E1) ───────────

/// Eine erkannte `/export`-Anfrage.
struct ExportRequest {
    /// `--tools`: Werkzeugaufrufe im Export einschließen.
    include_tool_calls: bool,
    /// `--datei <pfad>`: `Some(pfad)` überspringt die Auswahl und schreibt
    /// direkt dorthin; `None` öffnet [`Overlay::ExportChoice`].
    path: Option<String>,
}

/// Erkennt `/export [--tools] [--datei <pfad>]` in der rohen Befehlszeile.
///
/// # Beschreibung
/// Tokenisiert über [`crate::input::tokenize`] — dieselbe Quotierung wie der
/// reguläre `/command`-Dispatch — statt naiv auf Leerzeichen zu splitten (wie
/// [`is_goal_check_command`]): `--datei` kann einen Pfad mit Leerzeichen
/// tragen (`--datei "mit leerzeichen.md"`), den ein naiver Split zerrisse.
/// Spiegelt `harw_ops::export::ExportArgs::from_raw_args` (dort Op-intern,
/// von hier aus nicht referenzierbar) — siehe dessen Moduldoku für den
/// vollständigen Vertrag.
///
/// # Argumente
/// - `raw` (`&str`): die unveränderte Befehlszeile.
///
/// # Rückgabe
/// `Some(request)` für jede erkennbare `/export`-Zeile (auch mit unbekannten
/// Flags — die überlässt diese Funktion dem regulären `/command`-Dispatch,
/// der sie als Fehler meldet); `None` für jede andere Zeile oder bei einem
/// Tokenisierungsfehler (der reguläre Dispatch meldet ihn ohnehin bereits als
/// Systemzeile).
fn export_request_for_command(raw: &str) -> Option<ExportRequest> {
    let rest = raw.trim().strip_prefix('/')?;
    let mut tokens = crate::input::tokenize(rest).ok()?.into_iter();
    if tokens.next()?.as_str() != "export" {
        return None;
    }
    let tail: Vec<String> = tokens.collect();

    let mut include_tool_calls = false;
    let mut path = None;
    let mut index = 0;
    while index < tail.len() {
        match tail[index].as_str() {
            "--tools" => {
                include_tool_calls = true;
                index += 1;
            }
            "--datei" => {
                let Some(value) = tail.get(index + 1) else {
                    // Fehlender Pfad: der reguläre Dispatch meldet den Fehler
                    // über `OpError::InvalidArguments`; hier keine Auswahl öffnen.
                    return Some(ExportRequest {
                        include_tool_calls,
                        path: None,
                    });
                };
                path = Some(value.clone());
                index += 2;
            }
            _ => {
                // Unbekanntes Token: der reguläre Dispatch meldet den Fehler.
                index += 1;
            }
        }
    }
    Some(ExportRequest {
        include_tool_calls,
        path,
    })
}

/// Baut einen dateinamensicheren Zeitstempel aus der Systemzeit.
///
/// # Beschreibung
/// `time`/`jiff` sind in `harw-tui` bewusst nur Dev-Dependencies (siehe
/// `Cargo.toml`, Kommentar über `harw-fsutil`) — eine Kalenderdatum-Formatierung
/// steht in Produktionscode deshalb nicht zur Verfügung. Sekunden seit der
/// Unix-Epoche (`"1757831400"`) sind für [`export::default_export_path`]
/// ausreichend eindeutig; Kollisionen fängt ohnehin [`export::write_export`]
/// über Nummernsuffixe ab.
fn export_timestamp_now() -> String {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_owned())
}

/// Baut das Markdown-Dokument für `/export` aus dem parallel mitgeführten
/// [`ExportEntry`]-Verlauf ([`ChatApp::export_entries`]).
///
/// # Beschreibung
/// Werkzeugaufrufe und Denkschritte bleiben standardmäßig ausgeblendet
/// ([`ExportOptions::default`]) — dieselbe Zurückhaltung wie im Freigabe-Panel:
/// beide können interne Details offenlegen, die nicht jeder Export teilen
/// soll. `started_at` bleibt `None` (siehe [`export_timestamp_now`] für den
/// Grund); Titel, Verzeichnis und Session-ID kommen aus der laufenden Sitzung.
///
/// # Argumente
/// - `app` (`&ChatApp`): liefert Titel, Projekt-Root, Session-ID und Verlauf.
/// - `opts` (`&ExportOptions`): Inhaltsauswahl (siehe [`export_request_for_command`]).
fn build_export_markdown(app: &ChatApp, opts: &ExportOptions) -> String {
    let meta = ExportMeta {
        title: app.session_title().map(str::to_owned),
        session_id: app.session_id().to_string(),
        started_at: None,
        cwd: if app.project_root().is_empty() {
            None
        } else {
            Some(app.project_root().to_owned())
        },
        model: None,
    };
    export::render_markdown(&meta, &app.export_entries, opts)
}

/// Löst eine getroffene `/export`-Auswahl ein (Zwischenablage/Datei/Abbrechen).
///
/// # Beschreibung
/// Index `0` kopiert über [`clipboard::copy_or_sequence`] in die
/// Zwischenablage; landet die Sequenz dabei als [`ClipboardTarget::Osc52`]
/// (kein Systemwerkzeug erreichbar), wird sie roh auf `stdout` geschrieben —
/// denselben Deskriptor, den auch `TerminalGuard` für das Terminal verwendet;
/// ein `TerminalGuard` ist an dieser Stelle (Overlay-Tastenbehandlung) nicht
/// erreichbar. Index `1` schreibt über [`export::write_export`] in das
/// aktuelle Arbeitsverzeichnis. Jeder andere Index (insbesondere „Abbrechen")
/// tut nichts. Das Ergebnis erscheint als Systemzeile.
///
/// # Argumente
/// - `app` (`&mut ChatApp`): liefert den Verlauf und nimmt die Ergebniszeile auf.
/// - `bus` (`&HarwEventSender`): ungenutzt heute; symmetrische Signatur zu
///   [`handle_overlay_key`], falls ein künftiger Export-Pfad asynchron wird.
/// - `index` (`usize`): der von [`ChoiceDialog`] gemeldete Auswahlindex.
fn resolve_export_choice(app: &mut ChatApp, _bus: &HarwEventSender, index: usize) {
    match index {
        0 => {
            let markdown = build_export_markdown(app, &ExportOptions::default());
            match clipboard::copy_or_sequence(&markdown) {
                Ok((ClipboardTarget::Osc52, Some(sequence))) => {
                    let mut stdout = io::stdout();
                    let written = stdout
                        .write_all(sequence.as_bytes())
                        .and_then(|()| stdout.flush());
                    if written.is_ok() {
                        app.push_line(
                            Role::System,
                            "Export in die Zwischenablage kopiert (OSC-52).",
                        );
                    } else {
                        app.push_line(
                            Role::System,
                            "Export: OSC-52-Sequenz konnte nicht geschrieben werden.",
                        );
                    }
                }
                Ok((target, _)) => {
                    app.push_line(
                        Role::System,
                        format!("Export in die Zwischenablage kopiert ({target:?})."),
                    );
                }
                Err(error) => {
                    app.push_line(
                        Role::System,
                        format!("Export: Zwischenablage nicht verfügbar: {error}"),
                    );
                }
            }
        }
        1 => {
            let markdown = build_export_markdown(app, &ExportOptions::default());
            let now = export_timestamp_now();
            let dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let path = export::default_export_path(&now, &dir);
            match export::write_export(&path, &markdown) {
                Ok(()) => {
                    app.push_line(Role::System, format!("Export gespeichert: {}", path.display()))
                }
                Err(error) => {
                    app.push_line(Role::System, format!("Export fehlgeschlagen: {error}"))
                }
            }
        }
        _ => {}
    }
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
pub(crate) async fn frame_scheduler(
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

/// Verarbeitet einen Tastendruck, während ein Vollflächen-Overlay
/// (Session-Picker, `/export`-Auswahl) den normalen Eingabepfad ersetzt.
///
/// # Beschreibung
/// - [`Overlay::SessionPicker`]: delegiert an [`SessionPicker::handle_key`].
///   `PickerAction::Open(id)` schließt das Overlay und synthetisiert eine
///   `/resume <id>`-Befehlszeile über den bestehenden [`HarwEvent::Command`]-
///   Pfad (`resume_request` in `run_loop` erkennt sie und beendet den Loop
///   mit `TuiRunOutcome::Resume { selector: Some(id) }`, genau wie bei einer
///   getippten Zeile) — kein neuer Ereignistyp nötig.
/// - [`Overlay::ExportChoice`]: delegiert an [`ChoiceDialog::handle_key`].
///   Eine getroffene Wahl schließt das Overlay und wird über
///   [`resolve_export_choice`] eingelöst.
///
/// # Rückgabe
/// `true` (jede Taste verändert entweder den Overlay-Zustand oder schließt
/// ihn — in beiden Fällen ist ein Redraw nötig).
fn handle_overlay_key(app: &mut ChatApp, key: KeyEvent, bus: &HarwEventSender) -> bool {
    match app.overlay.as_mut() {
        Some(Overlay::SessionPicker(picker)) => match picker.handle_key(key) {
            PickerAction::Stay => {}
            PickerAction::Cancel => app.overlay = None,
            PickerAction::Open(id) => {
                app.overlay = None;
                bus.send(HarwEvent::Command(format!("/resume {id}")));
            }
        },
        Some(Overlay::ExportChoice(dialog)) => match dialog.handle_key(key) {
            ChoiceAction::Stay => {}
            ChoiceAction::Cancel => app.overlay = None,
            ChoiceAction::Chosen(index) => {
                app.overlay = None;
                resolve_export_choice(app, bus, index);
            }
        },
        None => {}
    }
    true
}

/// Verarbeitet einen Tastendruck: mutiert den Eingabezustand und emittiert
/// [`HarwEvent`]s. Gibt `true` zurück, wenn ein Redraw nötig ist.
///
/// # Tastensteuerung (an codex/codex-rs orientiert)
/// - **Enter** — Zeile absenden (leer/whitespace: ignoriert).
/// - **Ctrl+J** (auch Shift/Alt+Enter) — neue Zeile im Eingabepuffer.
/// - **Ctrl+C** — 1×: „again to quit"-Hinweis; 2× innerhalb 2 s: [`HarwEvent::Quit`].
/// - **Ctrl+D** — 1×: Hinweis, 2× innerhalb 2 s: Quit, unabhängig von Composer, Popup oder Overlay.
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

    // Ctrl+D — Doppeldruck beendet unabhängig vom aktuellen UI-Zustand.
    // Der Check liegt vor Overlay, Popup und Composer, damit ein verlässlicher
    // Notausstieg auch bei offenem Dialog oder nicht leerer Eingabe funktioniert.
    if ctrl && matches!(key.code, KeyCode::Char('d' | 'D')) {
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

    // Jede andere Taste macht eine Scharfstellung rückgängig.
    *pending_quit = None;
    if !matches!(key.code, KeyCode::Esc) {
        app.escape_armed = false;
    }

    // ── Vollflächige Overlays (Session-Picker, `/export`-Auswahl) ────────
    // Exklusiv: solange eines offen ist, geht keine Taste an Popup, Editor
    // oder ChatScroll (Plan Schritt 6/7).
    if app.has_overlay() {
        return handle_overlay_key(app, key, bus);
    }

    // Das erste Escape schließt ausschließlich die Autovervollständigung.
    // Ein direkt folgendes Escape erreicht danach den Composer und leert ihn.
    if matches!(key.code, KeyCode::Esc) && app.has_popup() {
        app.command_popup = None;
        app.escape_armed = true;
        return true;
    }

    // Shift+Tab — Zyklus ask → auto → full → plan (Plan Schritt 5). Nur
    // außerhalb eines offenen `/command`-Popups: sonst hätte dieselbe Taste
    // zwei Bedeutungen (Popup-Navigation vs. Moduszyklus).
    if matches!(key.code, KeyCode::BackTab) && !app.has_popup() {
        // `handle_key` läuft ausschließlich außerhalb eines laufenden Turns
        // (während eines Turns übernimmt `handle_busy_event`) — der Wechsel
        // wirkt hier also sofort, nicht vorgemerkt.
        app.cycle_permission_stage(false);
        return true;
    }

    // Ctrl+K löscht nur bis zum Ende der aktuellen Zeile, auch bei offenem
    // Command-Popup. Mehrzeilige Entwürfe unterhalb des Cursors bleiben stehen.
    if ctrl && matches!(key.code, KeyCode::Char('k' | 'K')) {
        app.input.delete_to_end();
        app.sync_popup();
        return true;
    }

    // Ctrl+O — klappt die letzte bzw. bei erneutem Druck alle Werkzeugzellen
    // auf/zu (Plan Schritt 2).
    if ctrl && matches!(key.code, KeyCode::Char('o' | 'O')) {
        return app.toggle_tool_cells();
    }

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

    // Enter übernimmt bei sichtbaren Command-Vorschlägen zuerst die markierte
    // Auswahl. Dadurch wird `/co` nie als unbekannter Command abgesendet,
    // obwohl `/compact` sichtbar gewählt werden kann. Argumentbehaftete
    // Commands sind davon nicht betroffen: `sync_popup` schließt das Popup,
    // sobald nach dem Namen Whitespace steht.
    if matches!(key.code, KeyCode::Enter) {
        // Shift/Alt+Enter fügt (wie in vielen TUIs) eine neue Zeile ein.
        if key.modifiers.contains(KeyModifiers::SHIFT) || key.modifiers.contains(KeyModifiers::ALT)
        {
            app.input.insert_newline();
            app.sync_popup();
            return true;
        }
        if let Some(name) = app
            .command_popup
            .as_ref()
            .and_then(CommandPopup::selected_name)
        {
            app.input.clear();
            app.input.insert_str(&format!("/{name} "));
            app.command_popup = None;
            return true;
        }
        // Plain Enter ohne Auswahl: getippte Zeile absenden.
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
        // Zwei Escape-Tasten leeren den Composer. Das erste Escape ist ein
        // harmloser Rücksprung (und kann damit auch die History-Navigation
        // beenden); erst der zweite unmittelbare Druck verwirft den Text.
        if matches!(key.code, KeyCode::Esc) && !app.input.is_empty() {
            if app.escape_armed {
                app.input.clear();
                app.command_popup = None;
                app.escape_armed = false;
            } else {
                app.escape_armed = true;
                let _ = app.input.handle_key(key);
            }
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

    // Keep a clone in the UI so Ctrl+C can cancel the core turn at its
    // cooperative checkpoints instead of merely queuing a character.
    let cancel = CancelToken::new();
    app.active_cancel = Some(cancel.clone());
    spinner.start();
    let reply = drive_turn_animated(
        guard,
        app,
        spinner,
        gateway,
        TurnInput::user(text).with_control(TurnControl::new().with_cancel(cancel)),
        approval_driver,
        approvals,
        tui_rx,
        turn_event_rx,
        turn_state,
    )
    .await;
    spinner.stop();
    app.active_cancel = None;

    let reply = reply?;
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
/// - `approvals` — jede eintreffende Frage öffnet das [`ApprovalDialog`]-Panel
///   anstelle des Composers (Plan Schritt 3). Ohne dieses Pollen liefe jede
///   Frage in den Timeout und würde damit zur Ablehnung.
/// - `tui_rx` — **nur solange eine Frage offen ist**: alle Tasten, die
///   [`ApprovalDialog::handle_key`] entgegennimmt (`y`/`n`/`Esc`/Pfeile/
///   Ziffern/`v`/`Tab`), plus `Ctrl+C` als fail-safe sofortige Ablehnung.
///   Ohne offene Frage bleibt der Zweig deaktiviert, damit während eines
///   Turns getippte Zeichen wie bisher im Kanal warten statt verworfen zu
///   werden.
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
    input: TurnInput,
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
        let turn = run_turn(session, model, store, input);
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

/// Totzeit, bevor eine frisch angezeigte Freigabefrage `y`/`n` annimmt.
///
/// # Beschreibung
/// W1-08 (Register G-008, w4-tui-control §1.4 K1): Ohne Totzeit beantwortet
/// jedes `y` oder `n`, das im Moment des Erscheinens ohnehin getippt wurde
/// („sync", „you", „nein"), die Frage — eine Tipp-Falle mit Ausführungsfolge.
/// 700 ms liegen über der Reaktionszeit eines Tippstroms und deutlich unter
/// der eines Menschen, der die Frage erst liest.
const APPROVAL_ARMING_DELAY: Duration = Duration::from_millis(700);

/// Was ein Tastendruck mit einer offenen Freigabefrage macht.
///
/// # Beschreibung
/// AP W5-03. Bewusst als reine, terminalfreie Klassifikation ausgelagert, damit
/// die Tastenbelegung ohne TTY testbar ist. Es gibt **keinen** Wert, der eine
/// Freigabe aus etwas anderem als einem ausdrücklichen `y` macht.
///
/// Seit Plan Schritt 3 zeichnet [`drive_pauses_to_completion`] die Freigabe
/// als [`crate::approval_dialog::ApprovalDialog`] statt als eigene
/// Verlaufszelle; dessen Tastenbelegung übernimmt `ApprovalDialog::handle_key`
/// vollständig (inklusive eines eigenen Arming-Vertrags für **jede** Taste,
/// nicht nur `y`/`n`). Dieser Typ und die zugehörigen Funktionen
/// ([`classify_approval_key`], [`is_approval_answer_key`],
/// [`classify_armed_approval_key`]) bleiben unverändert bestehen — sie werden
/// im Produktionspfad nicht mehr aufgerufen, aber ihre Tests dokumentieren
/// weiterhin den historischen Arming-Vertrag der `y`/`n`-Kurzwahl.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApprovalKeyAction {
    /// Ausdrückliche Freigabe (`y` / `Y`).
    Approve,
    /// Ablehnung mit fester Begründung (`n` / `N`, `Esc`, `Ctrl+C`).
    Reject(&'static str),
    /// Klappt die Argumente der Frage auf bzw. wieder ein (`v` / `V`).
    ToggleDetails,
    /// Eine Antworttaste, die (noch) nicht zählt: Totzeit läuft, die Frage ist
    /// nicht sichtbar, oder die Taste kam aus einer Auto-Wiederholung.
    NotArmed,
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
/// [`ApprovalKeyAction::Reject`] für `n`/`N`, `Esc` und `Ctrl+C`;
/// [`ApprovalKeyAction::ToggleDetails`] für `v`/`V`; sonst
/// [`ApprovalKeyAction::Ignore`].
#[allow(dead_code)] // Siehe Moduldoku von `ApprovalKeyAction`.
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
        KeyCode::Char('v' | 'V') => ApprovalKeyAction::ToggleDetails,
        KeyCode::Esc => ApprovalKeyAction::Reject(REASON_OPERATOR_CANCELLED),
        _ => ApprovalKeyAction::Ignore,
    }
}

/// Prüft, ob `key` eine Antworttaste (`y`/`n`) ohne Modifier ist.
#[allow(dead_code)] // Siehe Moduldoku von `ApprovalKeyAction`.
fn is_approval_answer_key(key: KeyEvent) -> bool {
    !key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('y' | 'Y' | 'n' | 'N'))
}

/// Klassifiziert einen Tastendruck gegen eine offene Freigabefrage **mit**
/// Scharfschaltung — reine Funktion, ohne Uhr und ohne Terminal.
///
/// # Beschreibung
/// W1-08 (Register G-008). `y`/`n` zählen nur, wenn
/// 1. seit dem Anzeigen der Frage mindestens [`APPROVAL_ARMING_DELAY`]
///    vergangen ist (Tipp-Falle, w4-tui-control K1),
/// 2. die Frage sichtbar ist (`question_visible`; hochgescrollt beantwortet
///    niemand, was er nicht liest, w4-tui-control K2),
/// 3. der Tastendruck keine Auto-Wiederholung ist (eine gedrückt gehaltene
///    Taste ist keine Entscheidung).
///
/// `Esc` und `Ctrl+C` bleiben immer wirksam: sie **lehnen ab**, sind also
/// fail-safe und dürfen nie blockiert werden. `v` klappt jederzeit auf.
///
/// # Argumente
/// - `key` ([`KeyEvent`]): der bereits auf Press/Repeat gefilterte Tastendruck.
/// - `since_shown` (`Duration`): Zeit seit dem Anzeigen der Frage.
/// - `question_visible` (`bool`): ob die Frage im Sichtbereich steht.
///
/// # Rückgabe
/// [`ApprovalKeyAction::NotArmed`] statt `Approve`/`Reject`, solange eine der
/// drei Bedingungen verletzt ist.
#[allow(dead_code)] // Siehe Moduldoku von `ApprovalKeyAction`.
fn classify_armed_approval_key(
    key: KeyEvent,
    since_shown: Duration,
    question_visible: bool,
) -> ApprovalKeyAction {
    let armed = since_shown >= APPROVAL_ARMING_DELAY
        && question_visible
        && key.kind != crossterm::event::KeyEventKind::Repeat;
    if is_approval_answer_key(key) && !armed {
        return ApprovalKeyAction::NotArmed;
    }
    classify_approval_key(key)
}

/// Ob eine Taste während einer offenen Freigabefrage überhaupt zählt — die
/// Arming-Bedingung des neuen [`ApprovalDialog`]-Panels (Plan Schritt 3).
///
/// # Beschreibung
/// Dieselbe Grundüberlegung wie bei [`classify_armed_approval_key`] (W1-08,
/// Register G-008): seit dem Anzeigen der Frage muss mindestens
/// [`APPROVAL_ARMING_DELAY`] vergangen sein, und der Tastendruck darf keine
/// Auto-Wiederholung sein. Die frühere dritte Bedingung „die Frage steht im
/// Sichtbereich" entfällt strukturell: das Panel ersetzt seit Plan Schritt 3
/// den Composer vollständig und ist damit immer sichtbar, sobald eine Frage
/// offen ist.
///
/// Anders als [`classify_armed_approval_key`] (dort nur für `y`/`n`) gilt
/// diese Bedingung für **jede** Taste — genau der Vertrag, den
/// [`ApprovalDialog::handle_key`] von seinem Aufrufer verlangt (siehe dessen
/// Moduldoku: „liefert für jede Taste `DialogAction::Stay`, solange
/// `armed == false`").
///
/// # Argumente
/// - `key` ([`KeyEvent`]): der bereits auf Press/Repeat gefilterte Tastendruck.
/// - `since_shown` (`Duration`): Zeit seit dem Anzeigen der Frage.
fn approval_dialog_key_is_armed(key: KeyEvent, since_shown: Duration) -> bool {
    since_shown >= APPROVAL_ARMING_DELAY && key.kind != crossterm::event::KeyEventKind::Repeat
}

/// Setzt `value` in doppelte Anführungszeichen, mit denselben Escapes, die
/// [`crate::input::tokenize`] im Quote-Modus erwartet (`\"`, `\\`).
///
/// # Beschreibung
/// Für synthetische `/command`-Zeilen, die ein mehrwortiges Argument (hier
/// eine von [`derive_shell_rule`] abgeleitete Regel wie `"git status"`)
/// unversehrt durch den regulären Dispatch schleusen — ohne Quotierung würde
/// `/permissions allow shell.exec git status --project` `status` als
/// eigenständiges, dem Op unbekanntes drittes Token missverstehen.
fn quote_for_synthetic_command(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Baut das [`ApprovalDialog`] für eine soeben eingetroffene [`ApprovalPrompt`]
/// (Plan Schritt 3).
///
/// # Argumente
/// - `prompt` (`&ApprovalPrompt`): die anzuzeigende Frage.
/// - `app` (`&ChatApp`): liefert `cwd` (Projekt-Root, falls bekannt).
/// - `timeout` (`Duration`): Restlaufzeit bis zur automatischen Ablehnung,
///   aus [`crate::approval::TuiApprovalHandler::timeout`].
///
/// # Rückgabe
/// Ein einsatzbereites [`ApprovalDialog`] mit Option 2 („nicht mehr fragen")
/// nur, wenn [`derive_shell_rule`] für `shell.exec` einen Vorschlag liefert.
fn build_approval_dialog(prompt: &ApprovalPrompt, app: &ChatApp, timeout: Duration) -> ApprovalDialog {
    let call = prompt.call();
    let remember_rule = if call.name.as_str() == "shell.exec" {
        call.arguments
            .get("command")
            .and_then(|value| value.as_str())
            .and_then(derive_shell_rule)
    } else {
        None
    };
    ApprovalDialog::new(ApprovalDialogRequest {
        call: call.clone(),
        cwd: if app.project_root().is_empty() {
            None
        } else {
            Some(app.project_root().to_owned())
        },
        justification: None,
        risk: None,
        origin: None,
        remember_rule,
        deadline: Instant::now() + timeout,
        reason_input_enabled: true,
    })
}

/// Setzt eine Entscheidung aus dem Freigabe-Panel um (Plan Schritt 3).
///
/// # Beschreibung
/// - [`ApprovalChoice::ApproveAndRemember`]: legt zusätzlich eine
///   Projekt-Regel im geteilten `harw_extension_api::allow_rules::AllowRuleSet`
///   der Montage an und stößt best-effort die Persistenz über den bestehenden
///   `/permissions allow`-Pfad an (schreibt dieselbe Regel zusätzlich nach
///   `.harw`/Projekt-Settings). Ohne Runtime-Montage bleibt die Freigabe auf
///   diesen einen Aufruf beschränkt; eine Systemzeile erklärt das.
/// - [`ApprovalChoice::ApproveAndAutoMode`]: schaltet zusätzlich die geteilte
///   `harw_extension_api::approval_mode::ApprovalModeCell` auf `auto`
///   ([`ApprovalMode::Delegated`]).
/// - [`ApprovalChoice::Reject`]: lehnt mit der eingegebenen Begründung ab,
///   falls eine vorliegt — [`ApprovalPrompt::reject`] unterstützt das direkt;
///   ohne Begründung gilt [`REASON_OPERATOR_REJECTED`].
///
/// Am Ende wird immer eine kompakte Notiz an der zugehörigen [`ToolCell`]
/// gesetzt (`✓ freigegeben` nur bei ausdrücklicher Freigabe **und**
/// zugestellter Antwort, sonst `✗ abgelehnt`).
///
/// # Argumente
/// - `app` (`&mut ChatApp`): liefert Adapter/Sandbox/Runtime für die
///   Persistenz-Anstoßung und nimmt eine optionale Systemzeile auf.
/// - `pending` ([`PendingApprovalPrompt`]): die zu beantwortende Frage samt Zelle.
/// - `choice` ([`ApprovalChoice`]): die getroffene Entscheidung.
async fn apply_approval_decision(app: &mut ChatApp, pending: PendingApprovalPrompt, choice: ApprovalChoice) {
    let PendingApprovalPrompt { prompt, tool_cell } = pending;
    let tool_name = prompt.tool_name().to_owned();

    if let ApprovalChoice::ApproveAndRemember(rule) = &choice {
        match app.runtime() {
            Some(rt) => {
                rt.services().allow_rules().add(ApprovalRule {
                    tool: tool_name.clone(),
                    pattern: Some(rule.clone()),
                    decision: RuleDecision::Allow,
                    scope: RuleScope::Project,
                });
                let command = format!(
                    "/permissions allow {tool_name} {} --project",
                    quote_for_synthetic_command(rule)
                );
                let output = execute_command_as(
                    app.adapters(),
                    app.sandbox(),
                    app.session_id(),
                    runtime_commands::caller_tier(rt.principal()),
                    &command,
                    || runtime_commands::slash_service_map(rt.services()),
                )
                .await;
                tracing::info!(
                    tool = %tool_name,
                    rule = %rule,
                    result = %output,
                    "tui.approval.remember_rule_persist_attempted"
                );
            }
            None => {
                tracing::warn!(tool = %tool_name, rule = %rule, "tui.approval.remember_rule_no_runtime");
                app.push_line(
                    Role::System,
                    format!(
                        "„Nicht mehr fragen“ für {tool_name} ({rule}) gilt nur für diesen \
                         Aufruf — keine Laufzeit-Montage zum Speichern verfügbar."
                    ),
                );
            }
        }
    }

    if matches!(choice, ApprovalChoice::ApproveAndAutoMode) {
        match app.runtime() {
            Some(rt) => rt.approval_mode().set(ApprovalMode::Delegated),
            None => app.push_line(
                Role::System,
                "Auto-Modus konnte nicht gesetzt werden — keine Laufzeit-Montage verfügbar.",
            ),
        }
    }

    let (approved, delivered) = match choice {
        ApprovalChoice::Approve
        | ApprovalChoice::ApproveAndRemember(_)
        | ApprovalChoice::ApproveAndAutoMode => (true, prompt.approve()),
        ApprovalChoice::Reject { reason } => {
            let reason = reason.unwrap_or_else(|| REASON_OPERATOR_REJECTED.to_owned());
            (false, prompt.reject(reason))
        }
    };

    let note = if approved && delivered {
        "✓ freigegeben"
    } else {
        "✗ abgelehnt"
    };
    match tool_cell.lock() {
        Ok(mut cell) => cell.set_approval_note(note),
        Err(_) => tracing::error!("tui.approval.tool_cell_lock_poisoned"),
    }
}

/// Eine offene, noch unbeantwortete Freigabefrage samt der zugehörigen
/// Werkzeugzelle (Plan Schritt 3).
///
/// # Beschreibung
/// Ersetzt die frühere separate Verlaufszelle (`ApprovalPromptCell`/
/// `ApprovalPromptView`): der Verlauf bekommt nach der Entscheidung nur eine
/// kompakte Notiz an der ohnehin über [`ensure_tool_cell`] angelegten
/// [`ToolCell`] ([`apply_approval_decision`], `ToolCell::set_approval_note`).
struct PendingApprovalPrompt {
    /// Die noch unbeantwortete Frage; wird beim Beantworten konsumiert.
    prompt: ApprovalPrompt,
    /// Die zugehörige, geteilte Werkzeugzelle im Verlauf.
    tool_cell: SharedToolCell,
}

/// Arbeitet jede Pause eines Turns ab, während der Renderer weiterläuft.
///
/// # Beschreibung
/// AP W5-03 — die Gegenseite zum früheren „pausiert (noch nicht unterstützt)".
/// Übergibt `outcome` an [`ApprovalDriver::drive_to_completion`] und pollt
/// dessen Future gemeinsam mit dem Fragekanal, der Tastatur (nur bei offener
/// Frage), den Turn-Ereignissen und dem Spinner-Timer.
///
/// Seit Plan Schritt 3 zeichnet [`render_viewport`] anstelle des Composers das
/// [`ApprovalDialog`]-Panel, solange `app.pending_approval_dialog` gesetzt ist
/// — diese Funktion setzt und löscht es synchron mit `pending`
/// ([`PendingApprovalPrompt`]), beide immer gemeinsam. Die Arming-Bedingung
/// bleibt dieselbe wie zuvor ([`approval_dialog_key_is_armed`], abgeleitet aus
/// [`classify_armed_approval_key`]), gilt jetzt aber für **jede** Taste, die
/// [`ApprovalDialog::handle_key`] entgegennimmt — nicht mehr nur für `y`/`n`.
/// `Ctrl+C` bleibt fail-safe und lehnt unabhängig vom Arming-Delay sofort ab
/// (Sicherheitsinvariante aus der ursprünglichen Freigabe-Logik).
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
    let mut dialog_shown_at: Option<Instant> = None;
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
                // Default, und das Panel darf nicht als Frage stehenbleiben.
                if let Some(open) = pending.take() {
                    apply_approval_decision(
                        app,
                        open,
                        ApprovalChoice::Reject { reason: Some(REASON_OPERATOR_CANCELLED.to_owned()) },
                    )
                    .await;
                    app.pending_approval_dialog = None;
                    // `dialog_shown_at` wird hier bewusst NICHT zurückgesetzt:
                    // die Funktion kehrt direkt danach zurück, die lokale
                    // Variable fällt mit ihr weg. Die Arming-Uhr selbst ist
                    // davon unberührt — sie wird beim Öffnen einer neuen Frage
                    // (unten, `dialog_shown_at = Some(Instant::now())`) frisch
                    // gesetzt und nur gelesen, während `pending_approval_dialog`
                    // tatsächlich `Some` ist (siehe unten); ein verwaister
                    // `Some`-Wert würde also nie fälschlich als „schon lange
                    // offen" gelesen.
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
                        // Eine noch offene ältere Frage wird nicht still
                        // überschrieben (w4-tui-control K3): nach einem
                        // Zeitablauf im Treiber stünden sonst zwei offene
                        // Fragen im Verlauf, und `y` beantwortete die falsche.
                        if let Some(stale) = pending.take() {
                            tracing::warn!("tui.approval.stale_prompt_closed");
                            apply_approval_decision(
                                app,
                                stale,
                                ApprovalChoice::Reject { reason: Some(REASON_OPERATOR_CANCELLED.to_owned()) },
                            )
                            .await;
                        }
                        // Dieselbe Zelle, die auch `TurnEvent::ToolCallRequested`
                        // anlegt bzw. wiederfindet (Wettlauf beider Kanäle, siehe
                        // `ensure_tool_cell`-Doku).
                        let tool_cell =
                            ensure_tool_cell(app, turn_state, prompt.call_id().clone(), prompt.call());
                        let timeout = approval_driver.handler().timeout();
                        app.pending_approval_dialog = Some(build_approval_dialog(&prompt, app, timeout));
                        dialog_shown_at = Some(Instant::now());
                        pending = Some(PendingApprovalPrompt { prompt, tool_cell });
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
                        // Ctrl+C bleibt fail-safe und lehnt sofort ab,
                        // unabhängig vom Arming-Delay des Panels — dieselbe
                        // Sicherheitsinvariante wie zuvor.
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && matches!(key.code, KeyCode::Char('c' | 'C'))
                        {
                            if let Some(open) = pending.take() {
                                apply_approval_decision(
                                    app,
                                    open,
                                    ApprovalChoice::Reject { reason: Some(REASON_OPERATOR_CANCELLED.to_owned()) },
                                )
                                .await;
                            }
                            app.pending_approval_dialog = None;
                            dialog_shown_at = None;
                            draw_viewport(guard, app, spinner, None)?;
                            continue;
                        }
                        let since_shown = dialog_shown_at.map_or(Duration::ZERO, |shown| shown.elapsed());
                        let armed = approval_dialog_key_is_armed(key, since_shown);
                        let Some(dialog) = app.pending_approval_dialog.as_mut() else {
                            continue;
                        };
                        match dialog.handle_key(key, armed) {
                            DialogAction::Stay => {
                                if queue_busy_key(app, key) {
                                    draw_viewport(guard, app, spinner, None)?;
                                }
                            }
                            DialogAction::ToggleDetails => {
                                draw_viewport(guard, app, spinner, None)?;
                            }
                            DialogAction::Decided(choice) => {
                                if let Some(open) = pending.take() {
                                    apply_approval_decision(app, open, choice).await;
                                }
                                app.pending_approval_dialog = None;
                                dialog_shown_at = None;
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
                            apply_approval_decision(
                                app,
                                open,
                                ApprovalChoice::Reject { reason: Some(REASON_OPERATOR_CANCELLED.to_owned()) },
                            )
                            .await;
                            app.pending_approval_dialog = None;
                            dialog_shown_at = None;
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

/// Bearbeitet den Composer während eines laufenden Turns. Chat-Zeilen gehen
/// direkt in die FIFO. Slash-Kommandos bleiben nach Enter im Composer, damit
/// sie weder verloren gehen noch die laufende Ausführung beeinflussen.
fn queue_busy_key(app: &mut ChatApp, key: KeyEvent) -> bool {
    match app.input.handle_key(key) {
        InputAction::Submit(text) => {
            app.remember_input(&text);
            match classify_line(&text) {
                LineAction::Chat(text) => app.pending_turns.push_back(text),
                // Commands require the runtime command channel. Preserve the
                // original line in the composer rather than silently losing it.
                LineAction::Command(raw) => app.input.insert_str(&raw),
                LineAction::Quit | LineAction::Ignore | LineAction::System(_) => {}
            }
            true
        }
        InputAction::Redraw => { app.sync_popup(); true }
        InputAction::Passthrough => false,
    }
}

/// Processes navigation immediately while preserving typing for the next prompt.
fn handle_busy_event(app: &mut ChatApp, event: TuiEvent) -> bool {
    let total = app.last_history_total_lines() as usize;
    let rows = app.last_history_visible_rows() as usize;
    match event {
        TuiEvent::Key(key)
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('c' | 'C')) =>
        {
            if let Some(cancel) = &app.active_cancel {
                cancel.cancel(CancelReason::User);
                app.push_line(Role::System, "Abbruch angefordert …");
                return true;
            }
            false
        }
        TuiEvent::Mouse(mouse) => {
            app.scroll.handle_mouse(mouse, total, rows) == ScrollAction::Redraw
        }
        TuiEvent::Key(key) if app.scroll.handle_key(key, total, rows) == ScrollAction::Redraw => {
            true
        }
        // Shift+Tab gilt sofort und wird nicht in `deferred_input` eingereiht,
        // damit der Zyklus nach Turn-Ende nicht ein zweites Mal läuft.
        TuiEvent::Key(key) if matches!(key.code, KeyCode::BackTab) => {
            app.cycle_permission_stage(false);
            true
        }
        TuiEvent::Draw | TuiEvent::Resize(_, _) => true,
        TuiEvent::Paste(text) => {
            app.input.insert_str(&text.replace("\r\n", "\n").replace('\r', "\n"));
            app.sync_popup();
            true
        }
        TuiEvent::Key(key) => queue_busy_key(app, key),
    }
}

/// Enthüllt die Antwort simuliert gestreamt, zeilenweise via [`StreamCollector`].
///
/// # Beschreibung
/// Da [`harw_core::ModelProvider`] keine Token-Deltas liefert, wird der Volltext hier in
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

/// Zeichnet die Fullscreen-Viewport: scrollbare History oben und Eingabebox unten.
///
/// # Beschreibung
/// Dreiteiliges Layout: History oben, eine dauerhafte Statuszeile und der
/// mehrzeilige Composer unten. Ein offenes `/command`-Popup überlagert den
/// unteren Teil der History.
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

    // Vollflächige Overlays (Session-Picker, `/export`-Auswahl) ersetzen die
    // gesamte Viewport (Plan Schritt 6/7) — `Clear` erst, sonst bliebe
    // Chat-Text unter dem Overlay stehen (dasselbe Muster wie beim
    // `/command`-Popup weiter unten).
    match &app.overlay {
        Some(Overlay::SessionPicker(picker)) => {
            frame.render_widget(Clear, area);
            picker.render(area, frame.buffer_mut(), &theme);
            return;
        }
        Some(Overlay::ExportChoice(dialog)) => {
            frame.render_widget(Clear, area);
            dialog.render(area, frame.buffer_mut(), theme);
            return;
        }
        None => {}
    }

    // Eingabehöhe wächst mit der Zeilenanzahl, begrenzt auf 3–10 — außer eine
    // Freigabefrage ist offen (Plan Schritt 3): dann ersetzt das
    // [`ApprovalDialog`]-Panel den Composer, und seine eigene
    // `desired_height` bestimmt die Höhe dieser Layout-Zeile.
    // Rahmen (2 Spalten) und das `"› "`-Präfix (2 Spalten) gehen von der
    // nutzbaren Textbreite ab; eine weitere Spalte bleibt für den Cursor frei.
    // Höhe und Cursor-Position müssen mit
    // derselben Breite rechnen, sonst laufen sie auseinander.
    let input_width = (area.width.saturating_sub(5)) as usize;
    let input_height = match &app.pending_approval_dialog {
        Some(dialog) => dialog.desired_height(area.width),
        None => {
            let input_line_count = app.input.visible_lines(input_width).len().clamp(1, 8) as u16;
            input_line_count + 2
        }
    };

    // History | permanente Statuszeile | Eingabe. Der Sicherheitsmodus muss
    // sichtbar bleiben und darf nicht vom Verlauf verdrängt werden.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1), Constraint::Length(input_height)])
        .split(area);
    let history_area = chunks[0];
    let status_area = chunks[1];
    let input_area = chunks[2];
    let permission = match app.current_permission_stage() {
        PermissionCycleStage::Ask => "Ask",
        PermissionCycleStage::Auto => "Auto",
        PermissionCycleStage::Full => "Full Access",
        PermissionCycleStage::Plan => "Plan",
    };
    let status = format!(
        " Shift+Tab: {permission} | Modus: {} | Tokens: {} (in {}, out {})",
        app.active_mode.as_str(), app.total_usage.total(),
        app.total_usage.input_tokens, app.total_usage.output_tokens,
    );
    frame.render_widget(Paragraph::new(status).style(Style::default().fg(style::border_color(theme))), status_area);

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

    // ── Eingabe / Freigabe-Panel ─────────────────────────────────────
    // Solange eine Freigabefrage offen ist, ersetzt das `ApprovalDialog` den
    // Composer vollständig (Plan Schritt 3); der Rest des Layouts (History,
    // Status) bleibt unverändert. Kein Text-Cursor in diesem Fall — das Panel
    // wird über Pfeiltasten/Ziffern bedient, nicht getippt.
    if let Some(dialog) = &app.pending_approval_dialog {
        dialog.render(input_area, frame.buffer_mut(), &theme);
        return;
    }

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
    /// Namensraum beim Zusammenbau einer Registry durch die Composition-Root.
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

    use harw_agent_dsl::ExecutableAgentIr;
    use harw_agent_dsl::roles::AgentRoleId;
    use harw_core::{
        ApprovalResolution, InMemoryStateStore, ModelFuture, ModelProvider, ModelRequest,
        ModelResponse, SpawnContext,
    };
    use harw_extension_api::approval_mode::ApprovalModeCell;
    use harw_extension_api::{
        ExtensionRegistry, ToolCall, ToolExecutionContext, ToolExecutor, ToolExecutorFuture,
        ToolName, ToolOutput, ToolProvider, ToolSpec,
    };
    use harw_operations::SessionController;
    use harw_operations::registry::OperationRegistry;
    use harw_registry_defaults::profile::role_names;
    use harw_tools::serde_json::{Value, json};
    use harw_tools::{FunctionToolSpec, JsonSchema};
    use harw_types::{AgentRole, ApprovalActor, ItemId, ToolCallId, TurnId};

    use crate::approval::TuiApprovalHandler;
    use crate::command_exec::build_services;
    use crate::events::harw_event_channel;

    // ────────────────────────────────────────────────────────────────────
    // W2d-2 / T2b (CONTRACTS-W2d2 §2 T2b): dieses Testmodul lief bis W2d-2
    // gegen Montage-Helfer, die app.rs selbst besaß (`build_tui_agent_session`,
    // `trusted_tui_spawn_context`, `assemble_tui_registry`,
    // `registry_with_approval_handler`, `build_tui_managed_spawner`,
    // `TuiChildRegistryFactory`, `tui_child_limits`, `TuiModelToolServices`,
    // `LOCAL_TUI_OPERATION_PERMISSION`, die Projekt-Wurzel-Session-ID-Ableitung).
    // Seit W2d-2 montiert app.rs nichts mehr selbst (`crate::runtime_root`,
    // `harw_runtime::RuntimeAssembly`). Tests, die ausschließlich das Verhalten
    // dieser gelöschten Montage-Helfer prüften, sind entfernt — ihre Abdeckung
    // steht jetzt in harw-runtime/harw-registry-defaults/harw-core (siehe
    // docs/remediation/ledger/W2d2/T2b.md). Tests, die echtes TUI-Verhalten
    // prüfen (Popup, Tastatur, Freigabefluss, `/mode`, Zellen-Rendering),
    // bleiben erhalten; wo sie eine Session brauchten, bauen sie sie jetzt
    // direkt über `AgentSession::new_with_id(..).with_spawn_context(..)`.
    // ────────────────────────────────────────────────────────────────────

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

    /// Test-lokaler Ersatz für das gelöschte `trusted_tui_spawn_context`
    /// (W2d-2/T2b, CONTRACTS-W2d2 §2 T2b): dieselben Feldwerte, die die
    /// gelöschte Funktion für den lokalen TUI-Root vergab — ein
    /// `ApprovalActor::Operator { id: "local-tui" }`, keine Vorschläge, kein
    /// Capability-Snapshot, Organisationsrolle `RootOrchestrator`. Anders als
    /// die Montage-Funktion trägt dieser Testwert keinen frischen Trace und
    /// keine Kontext-Decke — beide sind für die hier verbliebenen Tests
    /// (Freigabefluss, Modus-Wechsel, ausführbare Policy) ohne Bedeutung; ihre
    /// jeweilige Erzeugung ist in `harw-runtime/src/trace.rs` und
    /// `harw-runtime/src/ceiling.rs` eigenständig getestet.
    fn test_spawn_context(sandbox: &SandboxSpec) -> SpawnContext {
        SpawnContext {
            sandbox: sandbox.clone(),
            suggestions: None,
            capability_snapshot: None,
            approval_actor: Some(ApprovalActor::Operator {
                id: "local-tui".to_owned(),
            }),
            organizational_role: AgentRoleId::RootOrchestrator,
            trace: None,
            ceiling: None,
        }
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

    /// W2d-2/T2b: `build_tui_agent_session` ist entfallen (Montage lebt jetzt in
    /// `harw_runtime::RuntimeAssembly`); die Session wird direkt über
    /// `AgentSession::new_with_id(..).with_spawn_context(..).with_turn_event_sink(..)`
    /// gebaut (CONTRACTS-W2d2 §2 T2b). Das geprüfte Verhalten
    /// (`with_executable_agent_ir` schneidet die Werkzeugfläche) ist unverändert.
    #[test]
    fn selected_executable_policy_limits_tui_session_tools_and_retains_snapshot() {
        let policy = test_executable_agent_ir(&["stop"], &[]);
        let snapshot_id = policy.snapshot_id();
        let sandbox = test_sandbox();
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (turn_event_tx, _turn_event_rx) = tokio::sync::mpsc::unbounded_channel();

        let session = AgentSession::new_with_id(
            SessionId::new(),
            AgentRole::Assistant,
            None,
            ExtensionRegistry::builder().build(),
            event_tx,
        )
        .with_spawn_context(test_spawn_context(&sandbox))
        .with_turn_event_sink(turn_event_tx)
        .with_executable_agent_ir(&policy);

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

    /// W2d-2/T2b: siehe oben — ohne `with_executable_agent_ir` bleibt die
    /// volle Werkzeugfläche sichtbar.
    #[test]
    fn absent_executable_policy_preserves_full_tui_session_visibility() {
        let sandbox = test_sandbox();
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (turn_event_tx, _turn_event_rx) = tokio::sync::mpsc::unbounded_channel();

        let session = AgentSession::new_with_id(
            SessionId::new(),
            AgentRole::Assistant,
            None,
            ExtensionRegistry::builder().build(),
            event_tx,
        )
        .with_spawn_context(test_spawn_context(&sandbox))
        .with_turn_event_sink(turn_event_tx);

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

    /// Zwei Ctrl+D beenden auch bei nicht leerem Composer und offenem Popup.
    #[test]
    fn double_ctrl_d_quits_regardless_of_composer_or_popup_state() {
        let mut app = test_chat_app();
        app.input.insert_str("/status mit Entwurf");
        app.sync_popup();
        assert!(app.command_popup.is_some(), "Vorbedingung: Popup ist offen");

        let (bus, mut receiver) = harw_event_channel();
        let mut pending_quit = None;
        let ctrl_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);

        assert!(handle_key(&mut app, ctrl_d, &mut pending_quit, &bus));
        assert!(matches!(
            pending_quit,
            Some(QuitArm { label: "Ctrl+D", .. })
        ));
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));

        assert!(!handle_key(&mut app, ctrl_d, &mut pending_quit, &bus));
        assert!(matches!(receiver.try_recv(), Ok(HarwEvent::Quit)));
    }

    /// Enter bei einem offenen Popup übernimmt den markierten Befehl und
    /// sendet den unvollständigen Präfix nicht als unbekannten Command ab.
    #[test]
    fn test_handle_key_enter_accepts_popup_selection_without_submit() {
        let mut app = test_chat_app();
        app.input.clear();
        app.input.insert_str("/co");
        app.sync_popup();
        assert_eq!(
            app.command_popup
                .as_ref()
                .and_then(CommandPopup::selected_name),
            Some("compact"),
            "`/co` muss `/compact` vorauswählen"
        );

        let (bus, mut receiver) = harw_event_channel();
        let mut pending_quit: Option<QuitArm> = None;
        let redraw = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &mut pending_quit,
            &bus,
        );

        assert!(redraw, "Enter muss einen Redraw anfordern");
        assert_eq!(app.input(), "/compact ");
        assert!(app.command_popup.is_none(), "Auswahl muss das Popup schließen");
        assert!(
            receiver.try_recv().is_err(),
            "Autocomplete darf noch keinen Command absenden"
        );
    }

    #[test]
    fn busy_submit_is_queued_in_fifo_order() {
        let mut app = test_chat_app();
        app.input.insert_str("erste Nachricht");
        assert!(queue_busy_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        ));
        app.input.insert_str("zweite Nachricht");
        assert!(queue_busy_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        ));

        assert_eq!(app.pending_turns.pop_front().as_deref(), Some("erste Nachricht"));
        assert_eq!(app.pending_turns.pop_front().as_deref(), Some("zweite Nachricht"));
        assert!(app.pending_turns.is_empty());
    }

    /// Der Composer bleibt beim ersten Escape erhalten und wird beim zweiten
    /// unmittelbaren Escape geleert.
    #[test]
    fn two_escapes_clear_the_input_box() {
        let mut app = test_chat_app();
        app.input.insert_str("nicht verlieren beim ersten Escape");
        let (bus, _receiver) = harw_event_channel();
        let mut pending_quit = None;
        let escape = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);

        assert!(handle_key(&mut app, escape, &mut pending_quit, &bus));
        assert_eq!(app.input(), "nicht verlieren beim ersten Escape");
        assert!(app.escape_armed);

        assert!(handle_key(&mut app, escape, &mut pending_quit, &bus));
        assert!(app.input().is_empty());
        assert!(!app.escape_armed);
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
            AssistantMessageItem, ErrorItem, ReasoningItem, ResultTrust, ToolCallItem,
            ToolCallResult, ToolResultItem, UserMessageItem,
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
            trust: ResultTrust::Untrusted,
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
            ExtensionRegistry::builder().build(),
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
                // `reasoning`/`stop` sind für die Freigabetests irrelevant —
                // Default liefert `None` bzw. `StopReason::EndTurn`.
                ..Default::default()
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

    /// Baut eine Session mit derselben Freigabekette wie die Laufzeit-Montage:
    /// `DefaultApprovalPolicy` entscheidet zuerst, der TUI-Handler hängt direkt
    /// dahinter (W2d-2/T2b — Test-lokaler Ersatz für das gelöschte
    /// `registry_with_approval_handler`: beide Handler werden in einem
    /// Builder-Aufruf in genau dieser Reihenfolge registriert, statt eine
    /// Basis-Registry nachträglich um den TUI-Handler zu erweitern). Der
    /// Spawn-Kontext kommt aus [`test_spawn_context`] und trägt damit den
    /// Approval-Actor.
    fn approval_test_session(
        handler: &Arc<TuiApprovalHandler>,
        executions: &Arc<AtomicUsize>,
        sandbox: &SandboxSpec,
    ) -> AgentSession {
        let registered = as_dyn_approval_handler(handler);
        let registry = ExtensionRegistry::builder()
            .tool_provider(Arc::new(CountingToolProvider {
                executions: Arc::clone(executions),
            }))
            .approval_handler(Arc::new(harw_registry_defaults::DefaultApprovalPolicy::new(
                ApprovalModeCell::default(),
            )))
            .approval_handler(registered)
            .build();
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        AgentSession::new(AgentRole::Assistant, None, registry, event_tx)
            .with_spawn_context(test_spawn_context(sandbox))
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

    /// Die Antwort landet beim wartenden Treiber **und** an derselben
    /// Werkzeugzelle.
    ///
    /// Seit der Umstellung auf das Freigabe-Panel (`ApprovalDialog`) trägt
    /// nicht mehr eine eigene `ApprovalPromptCell` die Entscheidung, sondern
    /// dieselbe [`SharedToolCell`], die auch `TurnEvent::ToolCallRequested`
    /// über `ensure_tool_cell` anlegt (siehe [`PendingApprovalPrompt`]). Die
    /// geprüfte Eigenschaft bleibt dieselbe: die Freigabe erreicht den
    /// Treiber, und dieselbe Zelle trägt danach das Ergebnis.
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
        let tool_cell: SharedToolCell = Arc::new(Mutex::new(ToolCell::started(&call)));
        let mut app = ChatApp::new(Vec::new(), test_sandbox(), SessionId::new());

        apply_approval_decision(
            &mut app,
            PendingApprovalPrompt {
                prompt,
                tool_cell: Arc::clone(&tool_cell),
            },
            ApprovalChoice::Approve,
        )
        .await;

        match handler.await_resolution(&request).await {
            ApprovalResolution::Approve => {}
            other => panic!("eine Freigabe muss den Treiber erreichen, war: {other:?}"),
        }
        match tool_cell.lock() {
            Ok(cell) => assert_eq!(cell.approval_note.as_deref(), Some("✓ freigegeben")),
            Err(_) => panic!("die Zelle muss nach der Antwort lesbar bleiben"),
        }
    }

    /// Eine Ablehnung schreibt dieselbe Werkzeugzelle mit der Ablehnungsnotiz
    /// fort — die Nachfolgerin des früheren `Some(false)` an der
    /// `ApprovalPromptCell` (siehe Kommentar oben).
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
        let tool_cell: SharedToolCell = Arc::new(Mutex::new(ToolCell::started(&call)));
        let mut app = ChatApp::new(Vec::new(), test_sandbox(), SessionId::new());

        apply_approval_decision(
            &mut app,
            PendingApprovalPrompt {
                prompt,
                tool_cell: Arc::clone(&tool_cell),
            },
            ApprovalChoice::Reject {
                reason: Some(REASON_OPERATOR_REJECTED.to_owned()),
            },
        )
        .await;

        match handler.await_resolution(&request).await {
            ApprovalResolution::Reject { reason } => {
                assert_eq!(reason, REASON_OPERATOR_REJECTED);
            }
            other => panic!("eine Ablehnung muss den Treiber erreichen, war: {other:?}"),
        }
        match tool_cell.lock() {
            Ok(cell) => assert_eq!(cell.approval_note.as_deref(), Some("✗ abgelehnt")),
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

    /// TUI-Berechtigungen (E4: lokaler Principal-Tier `Operator`, siehe
    /// CONTRACTS-W2d2 §4). W2d-2/T2b: `LOCAL_TUI_OPERATION_PERMISSION` ist mit
    /// der Montage entfallen — die Produktionsfläche löst die Stufe jetzt über
    /// `runtime_commands::caller_tier(rt.principal())` auf, deren Ergebnis für
    /// den lokalen TUI-Principal laut E4 `PermissionTier::Operator` ist; dieser
    /// Test benutzt denselben Wert direkt. `CommandServices` ist durch die
    /// Closure `F: FnOnce() -> ServiceMap` ersetzt (CE, CONTRACTS-W2d2 §1.2).
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
            harw_operations::PermissionTier::Operator,
            "/permissions",
            || build_services(&adapters, None, None, &controller, None),
        )
        .await;
        assert!(permissions.contains("Freigabemodus"), "{permissions}");
        assert!(permissions.contains("ask|auto|full"), "{permissions}");

        let output = execute_command_as(
            &adapters,
            &sandbox,
            &SessionId::new(),
            harw_operations::PermissionTier::Operator,
            "/plugins",
            || build_services(&adapters, None, None, &controller, None),
        )
        .await;

        assert_eq!(
            output,
            "Berechtigung verweigert: /plugins erfordert Maintainer; aktuelle Stufe ist Operator"
        );
    }

    /// `/mode` an der Turn-Grenze (AP W5-05). W2d-2/T2b: die Session wird über
    /// `AgentSession::new_with_id(..).with_spawn_context(..).with_turn_event_sink(..)`
    /// gebaut statt über das gelöschte `build_tui_agent_session`.
    #[test]
    fn mode_request_is_applied_at_the_turn_boundary() {
        let sandbox = test_sandbox();
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (turn_event_tx, _turn_event_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut session = AgentSession::new_with_id(
            SessionId::new(),
            AgentRole::Assistant,
            None,
            ExtensionRegistry::builder().build(),
            event_tx,
        )
        .with_spawn_context(test_spawn_context(&sandbox))
        .with_turn_event_sink(turn_event_tx);
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

    }

    /// Ein unbekannter Modusname wird abgewiesen statt still auf den Default zu
    /// fallen; die Session bleibt unangetastet.
    #[test]
    fn unknown_mode_names_never_change_the_session() {
        let sandbox = test_sandbox();
        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let (turn_event_tx, _turn_event_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut session = AgentSession::new_with_id(
            SessionId::new(),
            AgentRole::Assistant,
            None,
            ExtensionRegistry::builder().build(),
            event_tx,
        )
        .with_spawn_context(test_spawn_context(&sandbox))
        .with_turn_event_sink(turn_event_tx);
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

    /// `ModeChanged` zieht den internen Modus nach; ein unbekannter Name nicht.
    #[test]
    fn mode_changed_events_update_the_active_mode() {
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
}

// ── Tests zur Scharfschaltung der Freigabefrage (W1-08) ──────────────────────

#[cfg(test)]
mod approval_arming_tests {
    use super::*;
    use crossterm::event::KeyEventKind;

    /// Tastendruck ohne Modifier.
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Sichtbare Frage, Totzeit abgelaufen.
    const ARMED: Duration = Duration::from_millis(900);

    /// Pflichtfall G-008: ein `y` innerhalb der Totzeit gibt **nichts** frei.
    #[test]
    fn an_early_y_is_ignored_and_a_late_one_approves() {
        assert_eq!(
            classify_armed_approval_key(key(KeyCode::Char('y')), Duration::ZERO, true),
            ApprovalKeyAction::NotArmed
        );
        assert_eq!(
            classify_armed_approval_key(
                key(KeyCode::Char('y')),
                APPROVAL_ARMING_DELAY - Duration::from_millis(1),
                true
            ),
            ApprovalKeyAction::NotArmed
        );
        assert_eq!(
            classify_armed_approval_key(key(KeyCode::Char('y')), APPROVAL_ARMING_DELAY, true),
            ApprovalKeyAction::Approve
        );
        assert_eq!(
            classify_armed_approval_key(key(KeyCode::Char('Y')), ARMED, true),
            ApprovalKeyAction::Approve
        );
    }

    /// Auch die Ablehnung per `n` ist der Tipp-Falle entzogen — sonst
    /// beantwortete ein getipptes „nein" die Frage, bevor sie gelesen ist.
    #[test]
    fn an_early_n_is_ignored_and_a_late_one_rejects() {
        assert_eq!(
            classify_armed_approval_key(key(KeyCode::Char('n')), Duration::from_millis(10), true),
            ApprovalKeyAction::NotArmed
        );
        assert_eq!(
            classify_armed_approval_key(key(KeyCode::Char('n')), ARMED, true),
            ApprovalKeyAction::Reject(REASON_OPERATOR_REJECTED)
        );
    }

    /// Eine nicht sichtbare Frage (hochgescrollt) wird nie beantwortet.
    #[test]
    fn an_invisible_question_accepts_no_answer() {
        assert_eq!(
            classify_armed_approval_key(key(KeyCode::Char('y')), ARMED, false),
            ApprovalKeyAction::NotArmed
        );
        assert_eq!(
            classify_armed_approval_key(key(KeyCode::Char('n')), ARMED, false),
            ApprovalKeyAction::NotArmed
        );
    }

    /// Eine gedrückt gehaltene Taste ist keine Entscheidung.
    #[test]
    fn a_repeated_key_never_answers() {
        let repeat = KeyEvent::new_with_kind(
            KeyCode::Char('y'),
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        );
        assert_eq!(
            classify_armed_approval_key(repeat, ARMED, true),
            ApprovalKeyAction::NotArmed
        );
    }

    /// Abbruchtasten bleiben immer wirksam: sie lehnen ab (fail-safe).
    #[test]
    fn cancel_keys_stay_armed_because_they_reject() {
        assert_eq!(
            classify_armed_approval_key(key(KeyCode::Esc), Duration::ZERO, false),
            ApprovalKeyAction::Reject(REASON_OPERATOR_CANCELLED)
        );
        assert_eq!(
            classify_armed_approval_key(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                Duration::ZERO,
                false
            ),
            ApprovalKeyAction::Reject(REASON_OPERATOR_CANCELLED)
        );
    }

    /// `v` klappt jederzeit auf, andere Tasten bleiben bedeutungslos.
    #[test]
    fn v_toggles_details_and_other_keys_are_ignored() {
        assert_eq!(
            classify_armed_approval_key(key(KeyCode::Char('v')), Duration::ZERO, true),
            ApprovalKeyAction::ToggleDetails
        );
        assert_eq!(
            classify_armed_approval_key(key(KeyCode::Char('V')), ARMED, false),
            ApprovalKeyAction::ToggleDetails
        );
        assert_eq!(
            classify_armed_approval_key(key(KeyCode::Char('x')), ARMED, true),
            ApprovalKeyAction::Ignore
        );
        assert_eq!(
            classify_armed_approval_key(
                KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL),
                ARMED,
                true
            ),
            ApprovalKeyAction::Ignore
        );
    }

    /// Der Anzeigezustand schaltet nach dem erneuten Anzeigen einer Frage
    /// (Rearm) wieder scharf.
    ///
    /// Seit der Umstellung auf das Freigabe-Panel gibt es keinen eigenen
    /// `ApprovalPresentation`-Wrapper mit `since_shown`/`rearm` mehr — die
    /// Schleife in `drive_pauses_to_completion` hält nur noch ein rohes
    /// `Option<Instant>` (`dialog_shown_at`) und setzt es bei jeder neuen
    /// Frage auf `Instant::now()` zurück. Die geprüfte Eigenschaft bleibt
    /// dieselbe — nur direkt an der Bedingungsfunktion des Panels
    /// ([`approval_dialog_key_is_armed`]) statt am inzwischen entfernten
    /// Wrapper: vor dem Rearm (Anzeigedauer erreicht die Verzögerung) ist die
    /// Taste scharf, unmittelbar danach (Anzeigedauer zurück auf null) wieder
    /// nicht.
    #[test]
    fn rearm_restarts_the_arming_delay() {
        let answer_key = key(KeyCode::Char('y'));

        assert!(approval_dialog_key_is_armed(answer_key, ARMED));
        // Rearm: eine neu eingetroffene Frage setzt die seit dem Anzeigen
        // verstrichene Zeit auf null zurück.
        assert!(!approval_dialog_key_is_armed(answer_key, Duration::ZERO));
    }
}
