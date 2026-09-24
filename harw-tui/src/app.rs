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

mod busy_queue;
// Runde 5, Teil F: Plan-Modus (Sperre, Plan-Fenster, `/plan …`, Editor).
pub(crate) mod plan_mode;
// Runde 5, Teil C: Kanban live (mtime-Signatur der Board-Dateien).
mod kanban_live;
// Runde 5, Teil G: gemeinsame Popup-Tasten für Idle- und Busy-Pfad.
mod popup_keys;
// Runde 5, Teil G: Modellwahl der UIA-Worker-Rollen (Worker-Bereich, Live-Wahl).
mod uia_workers;
// Runde 5, Teil I: Einhängepunkte des Kind-Live-Streams (`crate::child_stream`).
mod child_stream_glue;
// Runde 5, Teil I: Live-Werte der Agentenbaum-Ansicht `/agent`.
mod agent_tree_live_glue;
// Runde 5, Teil L: `/btw <frage>` — flüchtige Nebenfrage ohne Werkzeuge.
pub(crate) mod btw;
// Runde 5, Teil K: Hintergrund-Agenten (Starter, Meldungen, Leerlauf-Freigaben).
pub(crate) mod background_agents;
// Runde 5, Teil M: Kind-Meldungen (`parent.message`) an die UIA.
mod agent_messages;
// Runde 5, Teil O: Doppel-Esc bei laufenden Kindern, ehrliche Abbruch-Beschriftung.
mod turn_safety;
// Runde 5, Teil O: Freigabe-Fragen von Kind-Agenten im Freigabedialog.
pub(crate) mod child_approvals;
// Runde 5, Teil P: Goal-Marke (Statuszeile) und Goal-/Schritt-Verlaufszeilen.
mod goal_marker_glue;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
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
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use harw_authority::SandboxSpec;
use harw_core::cancel::{CancelReason, CancelToken};
use harw_core::turn_loop::TurnControl;
use harw_core::{
    AgentSession, CompactionPlan, ConversationHistory, CoreError, InteractionMode,
    ManagedAgentSpawner, ModelError, TurnInput, TurnOutcome, compact_session, run_turn,
};
use harw_extension_api::allow_rules::{ApprovalRule, RuleDecision, RuleScope, derive_shell_rule};
use harw_extension_api::approval_mode::ApprovalMode;
use harw_extension_api::registry::ContextProviderRegistrationError;
use harw_extension_api::{ToolCall, ToolName};
use harw_operations::adapter::CommandAdapter;
use harw_operations::operation::BusyAvailability;
use harw_operations::{OpOutput, SessionController};
use harw_plan::PlanStore;
use harw_plan::goal::{GoalStore, evaluate_goal};
use harw_protocol::events::{SessionEvent, TurnEvent};
use harw_protocol::items::{ContentPart, ResultTrust, ToolCallResult, TurnItem};
use harw_protocol::{AgentOrchestrationEvent, AgentOrchestrationStatus};
use harw_types::ReasoningEffort;
use harw_types::SessionId;
use harw_types::TokenUsage;

use crate::agent_tree::{AgentRow, AgentTree, AgentTreeAction};
use crate::approval::{
    ApprovalDriver, ApprovalDriverError, ApprovalPrompt, ApprovalPromptReceiver, ChildTurnDriver,
};
use crate::approval_dialog::{ApprovalChoice, ApprovalDialog, ApprovalDialogRequest, DialogAction};
use crate::chat_scroll::{ChatScroll, ScrollAction};
use crate::choice_dialog::{ChoiceAction, ChoiceDialog};
use crate::clipboard::{self, ClipboardTarget};
use crate::command_exec::{
    ShellRunOutcome, busy_availability_for, dispatch_command_with_shell_result, execute_command_as,
};
use crate::command_popup::{CommandPopup, PopupAction, PopupMode};
use crate::events::{HarwEvent, HarwEventSender};
use crate::export::{
    self, ExportAgentEntry, ExportEntry, ExportError, ExportErrorEntry, ExportMeta,
    ExportMetaExtensions, ExportOptions, ExportPlanEntry, ExportStatus,
};
use crate::frame_requester::{FrameRequester, MIN_FRAME_INTERVAL};
use crate::history_cell::{
    AssistantHistoryCell, GoalCell, HistoryCell, PlainHistoryCell, PlanGraphCell,
    ReasoningHistoryCell, SharedReasoningCell, SharedToolCell, SubAgentCell, SubAgentStatus,
    ToolCell, ToolGroupCell, ToolState, ToolVerbosity, UserHistoryCell,
};
use crate::host_permit_dialog::{HostPermitPrompt, HostPermitPromptReceiver, HostPermitVariant};
use crate::keybindings::{KeyAction, KeyBindings};
use crate::model_switch_picker::{
    ModelEntry, ModelSwitchPicker, PickerAction as ModelSwitchAction, PickerTarget, ProviderEntry,
};
// Runde 5, Teil E: Lern-Angebot und Auto-Modus-Vermerke.
use crate::permissions_view::{LearnOfferView, LearnScope, auto_note_for};
use busy_queue::{BusyCommand, BusyDispatch, BusyJobDone, BusyJobs, FetchTarget};
// Hinweis: die drei obigen Typen sind Re-Exporte aus
// `harw_tool_shell::host_permit_prompt` (siehe `crate::host_permit_dialog`-
// Moduldoku) — der Fragevertrag und die Ausstellungslogik leben dort bzw. in
// `harw_tool_shell::exec::ShellExecutor::authorize_host_command`; `app.rs`
// besitzt nur noch Rendering, Vorauswahl-Anzeige und das Arming-Delay.
use crate::CommandRegistry;
use crate::command_data;
use crate::help_overlay::{HelpOverlay, HelpTab};
use crate::input_editor::{InputAction, InputEditor};
use crate::input_history::InputHistoryStore;
use crate::kanban_board::KanbanBoard;
use crate::local_commands::{self, LocalCommandContext, LocalIntercept, PanelToggle};
use crate::matrix_view::MatrixView;
use crate::mention::{MentionLimits, expand_file_mentions, scan_mention_candidates};
use crate::mention_popup::{
    MentionCandidate, MentionPopup, MentionPopupAction, current_mention_query,
};
use crate::mode_picker::ModePicker;
use crate::model_roles_view::ModelRolesView;
use crate::overlay_view::{OverlayOutcome, OverlayView};
use crate::runtime_commands;
use crate::status_line;
use crate::workbench_pane::{PaneCommand, WorkbenchPane};
// Nur Tests (über `use super::*`) rufen die in `runtime_root` gewanderte
// Coercion-Hilfe noch unqualifiziert auf; Prod in app.rs nutzt sie nicht.
use crate::runtime_root::TitleJobContext;
#[cfg(test)]
use crate::runtime_root::as_dyn_approval_handler;
use crate::session_controller::TuiSessionController;
use crate::session_picker::{PickerAction, SessionEntry, SessionPicker};
use crate::spinner::Spinner;
use crate::style;
use crate::tui_event::TuiEvent;

/// Taktrate der Spinner-Animation während ein Turn läuft.
const SPINNER_INTERVAL: Duration = Duration::from_millis(80);

const NON_TEXT_CONTENT_PLACEHOLDER: &str = "[non-text content]";

/// Wie viele Einträge die persistente Eingabe-Historie beim Start zurückliefert.
/// Großzügig genug, dass Wochen alter Gebrauch erreichbar bleibt, klein genug,
/// dass das Laden beim Start nicht auffällt.
const INPUT_HISTORY_CAP: usize = 1000;

/// Begründung, die eine per Taste abgelehnte Freigabe trägt.
const REASON_OPERATOR_REJECTED: &str = "operator rejected the tool call in the terminal UI";

/// Begründung, die eine per Abbruch (Esc/Ctrl+C) abgelehnte Freigabe trägt.
const REASON_OPERATOR_CANCELLED: &str = "operator cancelled the approval prompt in the terminal UI";

/// Alle sechs [`ReasoningEffort`]-Stufen, aufsteigend — Optionsliste für
/// [`ChatApp::open_effort_choice`] (Welle 7b).
const EFFORT_LEVELS: [ReasoningEffort; 6] = [
    ReasoningEffort::Minimal,
    ReasoningEffort::Low,
    ReasoningEffort::Medium,
    ReasoningEffort::High,
    ReasoningEffort::Xhigh,
    ReasoningEffort::Max,
];

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
///
/// `last_commentary_text` ist der einzige Turn-gebundene Wert hier: er wird
/// von [`drive_turn_animated`] zu Turn-Beginn auf `None` zurückgesetzt (im
/// Gegensatz zu den obigen Feldern, die absichtlich über Turns hinweg
/// bestehen bleiben) und im `TurnEvent::ItemAdded(AssistantMessage)`-Zweig
/// von [`handle_turn_event`] gesetzt, sobald eine `Commentary`-Nachricht live
/// über [`ChatApp::push_line`] gezeigt wird. `drive_turn_animated` nutzt ihn
/// als zweite Verteidigungslinie gegen eine doppelte Anzeige derselben
/// Antwort (siehe dortige Doku).
#[derive(Debug, Default)]
struct TurnEventState {
    /// `call_id` → die eine geteilte Werkzeugzelle dieses Aufrufs.
    pending_tool_cells: HashMap<harw_types::ToolCallId, SharedToolCell>,
    /// `call_id` → Position des strukturierten `ExportEntry::ToolCall`.
    /// Dadurch können Dauer und Trust nach dem Abschluss am Call ergänzt
    /// werden, ohne den geordneten Strom in Call/Result aufzuspalten.
    export_tool_calls: HashMap<harw_types::ToolCallId, usize>,
    /// Bereits als unvollständig markierte Calls; verhindert doppelte
    /// Resume-/Abbruch-Resultate.
    export_incomplete_tools: HashMap<harw_types::ToolCallId, ()>,
    /// `child_id` → die eine Verlaufszelle dieses Kindes.
    child_cells: HashMap<String, Arc<Mutex<SubAgentCell>>>,
    /// Text der zuletzt live gepushten `Commentary`-Assistant-Zeile des
    /// aktuell laufenden Turns (oder `None`, wenn keine erschien). Siehe
    /// Dokumentation oben.
    last_commentary_text: Option<String>,
    /// Runde 5, Teil O: `call_id` → Turn, in dem der Aufruf angefragt wurde.
    /// Ein Turn-Abbruch markiert nur offene Aufrufe **seines** Turns als
    /// unvollständig, nie ältere (etwa einen längst beendeten Handoff).
    tool_call_turns: HashMap<harw_types::ToolCallId, harw_types::TurnId>,
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

/// Zustand der Agenten-Detailansicht (Enter im Agenten-Panel).
#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentDetailState {
    /// Der angezeigte Agent.
    agent: SessionId,
    /// Abstand in umbrochenen Zeilen vom Ende der Spur; `0` folgt dem
    /// neuesten Eintrag.
    scroll: u16,
    /// Reasoning-Einträge vollständig zeigen (`r` schaltet um).
    show_reasoning: bool,
    /// Vollbild-Zustand des Panels vor dem Öffnen (wird beim Schließen
    /// wiederhergestellt).
    prev_maximized: bool,
}

/// Seitenweite (Zeilen) für PageUp/PageDown in der Agenten-Detailansicht.
const AGENT_DETAIL_PAGE: u16 = 10;

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
    /// Eine einklappbare Reasoning-Zelle (Ctrl+O klappt sie wie
    /// Werkzeugzellen auf/zu).
    Reasoning(SharedReasoningCell),
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
            Self::Reasoning(cell) => cell
                .lock()
                .map(|guard| guard.is_expanded())
                .unwrap_or(false),
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
            Self::Reasoning(cell) => {
                if let Ok(mut guard) = cell.lock() {
                    guard.set_expanded(expanded);
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
            // Reasoning-Zellen rendern sich selbst (Verbosity irrelevant).
            ToolCellHandle::Reasoning(cell) => cell.display_lines(width, theme),
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
    /// Plan-Modus: Freigabe `Delegated` plus [`InteractionMode::Plan`] — die
    /// Plan-Sperre (Teil F) blockiert ohnehin alles Verändernde; lesende
    /// Werkzeuge und `plan.write` sollen dort nicht jedes Mal fragen.
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

/// Format, das ein strukturierter `/export`-Marker für den anschließenden
/// Renderer auswählt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportOutputFormat {
    Markdown,
    Json,
}

/// Ziel eines [`Overlay::EffortChoice`]-Dialogs (Welle 7b).
///
/// Steuert, welche Befehlszeile eine getroffene Wahl synthetisiert
/// (`/effort ...` vs. `/uia-effort ...`) und welcher Konfigurations-/
/// Laufzeitwert die Vorauswahl liefert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EffortTarget {
    /// Der aktive Reasoning-Effort der laufenden Session (Controller-Snapshot,
    /// live wirksam über `/effort`).
    Session,
    /// Der persistierte UIA-Reasoning-Effort (`config.harness.reasoning.uia`,
    /// wirksam ab der nächsten Sitzung über `/uia-effort`).
    Uia,
}

/// Vollflächiges Overlay, das den normalen Eingabe-/Popup-Pfad ersetzt.
///
/// # Beschreibung
/// Analog zum `/command`-Popup, aber exklusiv: solange ein Overlay offen ist,
/// gehen Tasten ausschließlich an das Overlay (siehe `handle_overlay_key`).
#[derive(Debug)]
enum Overlay {
    /// Die interaktive, ausschließlich lesende Projektion des vom gemeinsamen
    /// `ManagedAgentSpawner` gehaltenen Kind-Agentenbaums. Die Ausführung und
    /// die Abbruchautorität bleiben beim Controller.
    AgentTree(AgentTree),
    /// `/resume` ohne Argument öffnet eine filterbare Liste vergangener
    /// Sitzungen (Plan Schritt 7).
    SessionPicker(SessionPicker),
    /// `/export` ohne `--datei` öffnet die Auswahl Zwischenablage/Datei/Abbrechen
    /// (Contract „Nachträgliche Entscheidungen", Slice E1).
    ExportChoice(ChoiceDialog),
    /// `/model`, `/uia-model` bzw. `/uia-worker-model` ohne Argument (oder
    /// als argloses `switch`) öffnen den konsolidierten zweistufigen
    /// Provider/Modell-Picker (Welle 4a). `/provider`/`/uia-provider` ohne
    /// Argument öffnen seit dieser Konsolidierung **keinen** Picker mehr
    /// (kein Alias) — sie laufen unverändert auf `show`.
    ///
    /// Geboxt, da [`ModelSwitchPicker`] deutlich größer ist als die übrigen
    /// Varianten (clippy::large_enum_variant).
    ModelSwitch(Box<ModelSwitchPicker>),
    /// `/effort`/`/uia-effort` ohne Argument (oder als argloses `switch`)
    /// öffnen die einstufige Effort-Auswahl (Welle 7b).
    EffortChoice {
        /// Session oder UIA — bestimmt die synthetisierte Befehlszeile.
        target: EffortTarget,
        /// Der eigentliche Auswahldialog.
        dialog: ChoiceDialog,
    },
    /// Generische Ansicht (Hilfe, Modelle je Rolle, Modus-Auswahl, Kanban,
    /// Wissensbrowser). Schreibaktionen erzeugen Slash-Zeilen; Daten kommen
    /// über [`OverlayView::refresh_command`] bzw. `Fetch` aus `OpOutput.data`.
    View(Box<dyn OverlayView>),
}

/// Ausstehender Datenabruf (`OpOutput.data`) für eine Ansicht oder das
/// Werkbank-Panel.
///
/// # Beschreibung
/// Tastenbehandlung ist synchron; die eigentliche Ausführung über
/// [`command_data::execute_command_with_data`] ist `async` und läuft deshalb
/// erst am Anfang der nächsten Runde von [`run_loop`]
/// ([`process_pending_fetches`]). Die Ergebnisse landen nie im Chat.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DataFetch {
    /// Befehl für die generische Ansicht der angegebenen Generation; ein
    /// Ergebnis für eine inzwischen ersetzte Ansicht wird verworfen.
    Overlay {
        /// Auszuführende Slash-Zeile.
        command: String,
        /// Generation der Ansicht beim Einreihen.
        generation: u64,
    },
    /// `/workbench show` für das Werkbank-Panel.
    Workbench,
}

/// Bekannte Agentenrollen für `@rolle`-Erwähnungen und das `@`-Popup.
const KNOWN_ROLES: &[&str] = harw_registry_defaults::profile::role_names::ALL;

/// Höchstzahl der Dateikandidaten im `@`-Popup.
const MENTION_CANDIDATE_CAP: usize = 200;

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
    /// `/new`: eine frische Sitzung beginnen (die aktuelle bleibt gespeichert
    /// und ist per `/resume` erreichbar).
    NewSession,
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

/// Erkennt, ob der aktuell getippte (noch nicht abgeschickte) Composer-Text
/// im Shell-Modus gerendert werden soll (Plan Teil F: `!`-Modus wie in
/// Claude Code).
///
/// # Beschreibung
/// Reine Prädikatsfunktion: `true` genau dann, wenn `text` mit `'!'`
/// beginnt (das schließt `"!"` und `"!!"` selbst ein). Entscheidet
/// ausschließlich über die Composer-**Darstellung** (Rahmenfarbe, Titel,
/// Prompt in [`draw_viewport`]); die tatsächliche Ausführungssemantik
/// (Escape via `\!`, Admission, Capability) bleibt unverändert bei
/// [`crate::classify_input`] und [`classify_line`].
///
/// # Argumente
/// - `text` (`&str`): der aktuelle, noch nicht abgeschickte Composer-Inhalt.
///
/// # Rückgabe
/// `true`, wenn `text` mit `'!'` beginnt.
///
/// # Beispiele
/// ```ignore
/// use harw_tui::app::is_shell_mode_input;
/// assert!(is_shell_mode_input("!ls -la"));
/// assert!(!is_shell_mode_input("ls -la"));
/// ```
#[must_use]
fn is_shell_mode_input(text: &str) -> bool {
    text.starts_with('!')
}

/// Obergrenze der im automatischen Folge-Turn eingebetteten `!`-Ausgabe
/// (Plan Teil F), in Unicode-Zeichen.
const SHELL_TURN_OUTPUT_MAX_CHARS: usize = 8000;

/// Kappt `text` zeichengrenzen-sicher (nie mitten in einem UTF-8-Codepunkt)
/// auf höchstens `max_chars` Zeichen und hängt bei tatsächlicher Kappung den
/// Hinweis `"\n[gekürzt]"` an (Plan Teil F).
///
/// # Argumente
/// - `text` (`&str`): der zu kappende Text.
/// - `max_chars` (`usize`): Obergrenze in Unicode-Zeichen (nicht Bytes).
///
/// # Rückgabe
/// `text` unverändert, wenn er höchstens `max_chars` Zeichen hat; sonst die
/// ersten `max_chars` Zeichen plus `"\n[gekürzt]"`.
#[must_use]
fn truncate_chars_with_marker(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_owned();
    }
    let mut truncated: String = text.chars().take(max_chars).collect();
    truncated.push_str("\n[gekürzt]");
    truncated
}

/// Baut die Nutzereingabe für den automatischen Folge-Turn nach einem
/// `!`/`!!`-Befehl (Plan Teil F).
///
/// # Beschreibung
/// Reine Funktion, unabhängig vom Renderer-Zustand testbar. `output` wird
/// über [`truncate_chars_with_marker`] auf höchstens
/// [`SHELL_TURN_OUTPUT_MAX_CHARS`] Zeichen gekappt.
///
/// # Argumente
/// - `command` (`&str`): der ausgeführte Befehlstext, ohne führendes `!`.
/// - `exit_code` (`i64`): Exit-Code des Prozesses.
/// - `output` (`&str`): `stdout` und `stderr` zusammengeführt.
///
/// # Rückgabe
/// Die vollständige Nutzereingabe-Nachricht für den Folge-Turn, im Format
/// „Ich habe `!<command>` ausgeführt (Exit <n>):" gefolgt von einem
/// Markdown-Codeblock (Sprache `text`) mit der (ggf. gekappten) Ausgabe.
#[must_use]
fn build_shell_turn_message(command: &str, exit_code: i64, output: &str) -> String {
    let truncated = truncate_chars_with_marker(output, SHELL_TURN_OUTPUT_MAX_CHARS);
    format!("Ich habe `!{command}` ausgeführt (Exit {exit_code}):\n```text\n{truncated}\n```")
}

/// Reiht nach einem tatsächlich gelaufenen `!`/`!!`-Befehl den automatischen
/// Folge-Turn ein (Plan Teil F).
///
/// # Beschreibung
/// Merkt `shell.command` als [`ChatApp::last_shell_command`] für den
/// nächsten `!!`-Aufruf, hinterlegt eine kompakte
/// [`ChatApp::pending_turn_user_cell_override`] (vermeidet doppelte Anzeige
/// der bereits in der Shell-Ergebniszelle sichtbaren Ausgabe — pragmatische
/// Entscheidung nach Plan Teil F, siehe Feld-Doku) und reicht die volle,
/// gekappte Turn-Nachricht ([`build_shell_turn_message`]) über denselben
/// Pfad weiter wie eine normal abgeschickte Nachricht: frei (`submitted ==
/// None`) wird sie sofort zum nächsten zu treibenden Turn; belegt
/// (`submitted.is_some()`) wird sie ans Ende von [`ChatApp::pending_turns`]
/// gehängt — identisch zu [`HarwEvent::Submit`] in `run_loop`.
///
/// # Argumente
/// - `app` (`&mut ChatApp`): nimmt `last_shell_command` und die
///   Anzeige-Überschreibung auf.
/// - `submitted` (`&mut Option<String>`): dieselbe lokale Variable, die
///   `run_loop` für den nächsten zu treibenden Turn verwendet.
/// - `shell` ([`ShellRunOutcome`]): das strukturierte Ergebnis des soeben
///   gelaufenen `!`/`!!`-Befehls.
fn queue_shell_follow_up_turn(
    app: &mut ChatApp,
    submitted: &mut Option<String>,
    shell: ShellRunOutcome,
) {
    app.pending_turn_user_cell_override = Some(format!(
        "↳ Ausgabe von !{} an den Agenten übergeben",
        shell.command
    ));
    let message = build_shell_turn_message(&shell.command, shell.exit_code, &shell.combined_output);
    app.last_shell_command = Some(shell.command);
    if submitted.is_some() {
        app.pending_turns.push_back(message);
    } else {
        *submitted = Some(message);
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
    /// Befehle und Datenabrufe, die während eines laufenden Turns nebenläufig
    /// laufen (Runde 4, Teil H; siehe [`busy_queue`]).
    busy_jobs: BusyJobs,
    /// Runde 5, Teil L: `/btw`-Nebenfrage (Schnappschuss, laufender Auftrag;
    /// siehe [`btw`]).
    btw: btw::BtwState,
    /// Kooperativer Abbruchgriff für den gerade laufenden Turn. `Ctrl+C`
    /// löst ihn auch dann aus, wenn kein Freigabe-Dialog sichtbar ist.
    active_cancel: Option<CancelToken>,
    /// Zeitpunkt, zu dem `Ctrl+C` zuletzt einen Abbruch angefordert hat,
    /// solange dieser noch aussteht. Rein transienter Statuszeilen-Hinweis
    /// (siehe [`render_viewport`]) — im Gegensatz zu `push_line` erzeugt das
    /// KEINE dauerhafte Verlaufszeile. Wird beim Start eines neuen Turns
    /// (`active_cancel` wird neu gesetzt) und beim Ende des laufenden Turns
    /// (`active_cancel = None`) wieder gelöscht.
    cancel_requested_at: Option<Instant>,
    /// Zeitpunkt, zu dem `Ctrl+C` einen Turn abgebrochen hat, während die
    /// Eingabe-Warteschlange (`deferred_input`/`pending_turns`) nicht leer war
    /// (Runde 4: die Warteschlange bleibt erhalten und wird nach dem Abbruch
    /// ausgeliefert, statt verworfen zu werden). Rein transienter
    /// Statuszeilen-Hinweis (siehe [`render_viewport`]), analog zu
    /// `cancel_requested_at` — erzeugt KEINE dauerhafte Verlaufszeile. Wird an
    /// denselben Stellen wie `cancel_requested_at` zurückgesetzt (neuer Turn,
    /// Turn-Ende).
    queue_kept_at: Option<Instant>,
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
    /// Live-Zustand aller Agenten (Wurzel, Kinder, UIA-Worker) aus dem
    /// agenten-übergreifenden Bus; speist Agenten-Panel und Statuszeile.
    agent_monitor: crate::agent_monitor::AgentMonitor,
    /// Abonnement auf [`harw_core::AgentEventHub`]; nicht-blockierend geleert
    /// bei jedem Spinner-Tick und jedem Turn-Event ([`Self::drain_agent_events`]).
    agent_rx: Option<tokio::sync::broadcast::Receiver<harw_core::AgentEvent>>,
    /// Runde 5, Teil C: Beobachtung der Dateien eines offenen Kanban-Boards
    /// ([`kanban_live::KanbanLiveWatch`]).
    kanban_live: kanban_live::KanbanLiveWatch,
    /// Live gestreamter Assistant-Text der laufenden Modell-Runde der Wurzel;
    /// transient unter dem Verlauf gezeichnet, geleert sobald die finale
    /// Nachricht als Zelle vorliegt.
    live_stream: String,
    /// Live gestreamtes Reasoning der laufenden Runde (nur Vorschau).
    live_reasoning: String,
    /// Sichtbarkeit und Fokus der Seitenpanels.
    panels: crate::panes::PanelState,
    /// Offene Detailansicht eines Agenten im Agenten-Panel (Enter);
    /// `None` = Listenansicht.
    agent_detail: Option<AgentDetailState>,
    /// Aktive Tastenbelegung (Standard oder aus der Keybindings-Datei, siehe
    /// [`Self::set_key_bindings`]); gilt für Panels und Composer.
    key_bindings: KeyBindings,
    /// Explorer-Panel über den gesamten Projektbaum; beim ersten Einblenden
    /// angelegt und im Hintergrund indiziert.
    explorer: Option<crate::explorer_panel::ExplorerPanel>,
    /// `/`-Command-Adapter, gebaut aus der `OperationRegistry`
    /// (`CommandAdapter::from_operation` pro registrierter Op). Treibt die
    /// echte Ausführung von `/command`-Zeilen (siehe [`Self::adapters`]).
    /// Geteilt (`Arc`), damit Busy-Tasks sie ohne Kopie mitnehmen.
    adapters: Arc<[CommandAdapter]>,
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
    /// Append-only Orchestrierungsbeobachtungen, die beim Resume aus dem
    /// StateStore geladen wurden. Sie sind reine Anzeigehistorie und werden
    /// nie in den lebenden Controller zurückgespielt.
    historic_agent_events: Vec<AgentOrchestrationEvent>,
    /// Zuletzt beobachteter Interaktionsmodus der Session. Wird an der
    /// Turn-Grenze aus [`AgentSession::mode`] nachgezogen bzw. aus
    /// [`TurnEvent::ModeChanged`] übernommen und in der Statuszeile angezeigt.
    active_mode: InteractionMode,
    /// Optionale Plan-/Ziel-Dienste der Composition-Root für [`PlanGraphCell`]
    /// und [`GoalCell`]; `None` degradiert beide zu Systemzeilen.
    plan_services: Option<TuiPlanServices>,
    /// Runde 5, Teil P: Goal-Marke und Übergänge (`crate::goal_marker`).
    goal_tracker: crate::goal_marker::GoalTracker,
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
    /// Runde 5, Teil I: Live-Blöcke der Kind-Agenten (Orchestratoren) unter
    /// ihrer Agent-Zeile; siehe [`crate::child_stream`].
    child_stream: crate::child_stream::ChildStreamRegistry,
    /// Ctrl+O-Vormerkung: `true` unmittelbar nach einem Druck, der nur die
    /// letzte Werkzeugzelle umgeschaltet hat — ein erneuter Druck schaltet
    /// dann alle um (Plan Schritt 2).
    ctrl_o_expand_last_armed: bool,
    /// Die aktuell offene Freigabefrage, die anstelle des Composers gezeichnet
    /// wird (Plan Schritt 3); `None` zeigt den normalen Composer.
    pending_approval_dialog: Option<ApprovalDialog>,
    /// Die aktuell offene Host-Permit-Frage, die anstelle des Composers
    /// gezeichnet wird (Plan „UIA-Shell-Worker und Shell-Modus", Schritt 2);
    /// `None` zeigt den normalen Composer. Unabhängig von
    /// `pending_approval_dialog`: beide Fragearten können, streng
    /// nacheinander, während desselben Turns auftreten — `y`/`n` gehen immer
    /// zuerst an eine offene [`ApprovalDialog`]-Frage, erst danach an diese.
    pending_host_permit: Option<HostPermitPrompt>,
    /// Der aus [`Self::pending_host_permit`] gebaute Auswahldialog
    /// („Einmalig" / „Host-Arbeitsphase" / „Nein"); immer gemeinsam mit
    /// `pending_host_permit` gesetzt bzw. geleert.
    pending_host_permit_dialog: Option<ChoiceDialog>,
    /// Runde 5, Teil B: eigenes sudo-Freigabefenster (`host.sudo_exec`) samt
    /// Fragekanal und Sitzungs-Merken des Passworts (siehe
    /// [`crate::sudo_dialog`]). Ein offenes Fenster fängt jede Eingabe ab.
    pub(crate) sudo: crate::sudo_dialog::SudoUi,
    /// Runde 5, Teil F: Plan-Fenster (`plan.exit`, `plan.enter`,
    /// `ask_user`), Plan-Fragekanal und geteilter Plan-Zustand (siehe
    /// [`crate::plan_dialog`], `app/plan_mode.rs`). Ein offenes Fenster fängt
    /// jede Eingabe ab.
    pub(crate) plan_ui: crate::plan_dialog::PlanUi,
    /// Vorgemerktes Ziel des Shift+Tab-Zyklus, solange ein Turn läuft (Plan
    /// Schritt 5, AP W5-05: der Wechsel gilt erst an der nächsten Turn-Grenze).
    pending_permission_stage: Option<PermissionCycleStage>,
    /// Interaktionsmodus, zu dem der Zyklus nach Verlassen der `Plan`-Stufe
    /// zurückkehrt (Plan Schritt 5).
    mode_before_plan: Option<InteractionMode>,
    /// Vollflächiges Overlay (Session-Picker, `/export`-, Modell/Provider-,
    /// Effort-Auswahl), das den normalen Eingabe-/Popup-Pfad ersetzt (Plan
    /// Schritt 6/7, Welle 4a/7b).
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
    /// Wurzelverzeichnis des Session-Stores (Transcripts und
    /// `.meta.json`-Sidecars), sofern der Composition-Root (`runtime_root.rs`)
    /// sie kennt — anders als [`Self::title_job_context`]s
    /// `session_store_root` wird dieses Feld **unabhängig** davon gesetzt,
    /// ob `[session] title_generation` aktiv ist (siehe Aufgabe 2, Plan
    /// `recursive-cooking-lobster.md` Teil F: Export-Datum bei Resume für
    /// jede Session). `None`, wenn `/resume` in diesem Lauf gar nicht
    /// konfiguriert ist. Genutzt von [`apply_session_store_started_at`].
    session_store_root: Option<std::path::PathBuf>,
    /// Startzeit des aktuellen TUI-/Resume-Laufs für Exportmetadaten.
    export_started_at: Option<String>,
    /// Zuletzt aus `SessionConfigured` bzw. der Laufzeit bekannte Modell-ID.
    export_session_model: Option<String>,
    /// Bekannte, nicht-sensitive Erweiterungsmetadaten des Laufs.
    export_meta_extensions: ExportMetaExtensions,
    /// Optionen/Format des letzten Marker-Exports, solange dessen Zielauswahl
    /// geöffnet ist.
    pending_export_options: Option<ExportOptions>,
    pending_export_format: ExportOutputFormat,
    /// „Scharfgestellter" Beenden-Hinweis (4c): ein erster Ctrl+C/Ctrl+D setzt
    /// dieses Feld; ein zweiter Druck derselben Taste innerhalb von
    /// [`QUIT_HINT_WINDOW`] beendet die Sitzung. Vormals eine lokale Variable
    /// in [`run_loop`] — jetzt ein `ChatApp`-Feld, damit auch der Busy-Pfad
    /// ([`handle_busy_event`]) dieselbe Scharfstellung lesen und setzen kann
    /// (Ctrl+C während eines laufenden Turns bricht zusätzlich ab, statt nur
    /// zu beenden).
    pending_quit: Option<QuitArm>,
    /// Welle 4c: gesetzt von [`handle_busy_event`], wenn ein zweiter
    /// Ctrl+C-Druck während eines laufenden Turns innerhalb von
    /// [`QUIT_HINT_WINDOW`] eintrifft. [`run_loop`] prüft dieses Feld direkt
    /// nach jedem `run_turn_streaming(...)`-Rückkehrpunkt und beendet dann
    /// sofort — derselbe Ausgang wie [`HarwEvent::Quit`] im Idle-Pfad. Ein
    /// eigenes `bool`-Feld statt eines durchgereichten `harw_tx`, weil
    /// `handle_busy_event` selbst keinen Zugriff auf den Ereigniskanal hat
    /// (siehe Plan „Ctrl+C-Hard-Interrupt, UI-Teil", Punkt 6).
    hard_quit_requested: bool,
    /// Zuletzt in dieser Sitzung tatsächlich gestartete `!`-Befehl (ohne
    /// führendes `!`), für `!!` (Plan Teil F: `!`-Modus wie in Claude Code).
    /// `None`, solange noch kein `!`-Befehl gelaufen ist. Wird nach jedem
    /// `!`/`!!`-Lauf mit einem [`ShellRunOutcome`] neu gesetzt — unabhängig
    /// vom Exit-Code, denn auch ein fehlgeschlagener Befehl bleibt
    /// wiederholbar.
    last_shell_command: Option<String>,
    /// Kompakte Anzeige-Überschreibung für die NÄCHSTE Nutzerzelle, die
    /// `run_loop` beim Treiben eines Turns pusht (Plan Teil F). Vermeidet die
    /// doppelte Anzeige der `!`-Ausgabe: sie steht bereits in der
    /// Shell-Ergebniszelle direkt darüber, der automatische Folge-Turn
    /// bräuchte sie sonst ein zweites Mal in seiner Nutzerzelle. `None` lässt
    /// `run_loop` unverändert den vollen Turn-Text anzeigen (Standardfall für
    /// jede normal getippte Nachricht); `Some(text)` wird einmalig konsumiert
    /// (`Option::take`) und ersetzt nur die Anzeige — der Modell-Turn selbst
    /// bekommt weiterhin den vollen Text.
    pending_turn_user_cell_override: Option<String>,
    /// Vormerkung für [`TerminalGuard::reassert_terminal_modes`] (Register
    /// "CSI-Sicherheitsnetz und Paste-Platzhalter", Punkt 7): `true`, sobald
    /// ein Ereignis eingetreten ist, nach dem Raw-Mode/Bracketed-Paste/
    /// Maus-Capture beschädigt zurückgekommen sein könnten (Ctrl+H beendet
    /// eine Host-Arbeitsphase, siehe [`Self::end_host_mode`], oder ein
    /// `shell.exec`-Werkzeugaufruf endet, siehe `handle_turn_event`).
    /// Gesetzt an diesen beiden Stellen, weil dort kein `&mut TerminalGuard`
    /// zur Hand ist; von einem Aufrufer, der einen Guard besitzt, über
    /// [`Self::take_needs_terminal_reassert`] konsumiert.
    needs_terminal_reassert: bool,
    /// Werkbank-Panel (rechte Spalte, `F5`); lädt über
    /// [`WorkbenchPane::refresh_command`] (Sitzung oder Projekt), sobald sichtbar und veraltet.
    workbench: WorkbenchPane,
    /// Geöffnetes `@`-Erwähnungs-Popup oder `None`.
    mention_popup: Option<MentionPopup>,
    /// Ausstehende Datenabrufe, abgearbeitet am Anfang jeder
    /// [`run_loop`]-Runde (siehe [`DataFetch`]).
    pending_fetches: Vec<DataFetch>,
    /// Slash-Zeilen aus synchronen Pfaden ohne Ereigniskanal (z. B.
    /// Werkbank-Tasten), die [`run_loop`] im Leerlauf als
    /// [`HarwEvent::Command`] abschickt.
    pending_commands: std::collections::VecDeque<String>,
    /// Generation der aktuell offenen generischen Ansicht
    /// ([`Overlay::View`]); wächst bei jedem Öffnen.
    overlay_generation: u64,
    /// Runde 5, Teil K: Meldungen und Leerlauf-Zustand der Hintergrund-Agenten.
    background: background_agents::BackgroundUi,
    /// Runde 5, Teil O: Nachfrage vor einem Esc-Abbruch mit laufenden Kindern.
    esc_confirm: turn_safety::EscConfirm,
    /// Runde 5, Teil O: Freigabe-Fragen von Kind-Agenten.
    child_approvals: child_approvals::ChildApprovalUi,
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
            .field("agent_detail", &self.agent_detail)
            .field(
                "has_pending_approval_dialog",
                &self.pending_approval_dialog.is_some(),
            )
            .field(
                "has_pending_host_permit_dialog",
                &self.pending_host_permit_dialog.is_some(),
            )
            .field("host_mode_active", &self.host_mode_active())
            .field("sudo", &self.sudo)
            // Runde 5, Teil F.
            .field("plan_ui", &self.plan_ui)
            .field("pending_permission_stage", &self.pending_permission_stage)
            .field("has_overlay", &self.overlay.is_some())
            .field("export_entries_len", &self.export_entries.len())
            .field("session_title", &self.session_title)
            .field("has_title_job_context", &self.title_job_context.is_some())
            .field("pending_quit_armed", &self.pending_quit.is_some())
            .field("hard_quit_requested", &self.hard_quit_requested)
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
            busy_jobs: BusyJobs::new(),
            // Runde 5, Teil L.
            btw: btw::BtwState::new(),
            active_cancel: None,
            cancel_requested_at: None,
            queue_kept_at: None,
            escape_armed: false,
            scroll: ChatScroll::new(),
            input_history,
            command_registry: CommandRegistry::from_command_adapters(&adapters)
                .with_local_specs(crate::command_catalog::local_command_specs()),
            command_popup: None,
            theme: style::detect_theme(),
            total_usage: TokenUsage::default(),
            agent_monitor: crate::agent_monitor::AgentMonitor::default(),
            agent_rx: None,
            // Runde 5, Teil C: Kanban live.
            kanban_live: kanban_live::KanbanLiveWatch::default(),
            live_stream: String::new(),
            live_reasoning: String::new(),
            panels: crate::panes::PanelState::default(),
            agent_detail: None,
            key_bindings: KeyBindings::default(),
            explorer: None,
            adapters: adapters.into(),
            sandbox,
            session_id,
            memory,
            runtime: None,
            project_root: String::new(),
            session_controller: Arc::new(TuiSessionController::new()),
            last_history_total_lines: Cell::new(0),
            last_history_visible_rows: Cell::new(20),
            managed_spawner: None,
            historic_agent_events: Vec::new(),
            active_mode: InteractionMode::default(),
            plan_services: None,
            goal_tracker: crate::goal_marker::GoalTracker::default(),
            tool_verbosity: ToolVerbosity::Compact,
            tool_cells: Vec::new(),
            open_tool_group: None,
            // Runde 5, Teil I.
            child_stream: crate::child_stream::ChildStreamRegistry::default(),
            ctrl_o_expand_last_armed: false,
            pending_approval_dialog: None,
            pending_host_permit: None,
            pending_host_permit_dialog: None,
            sudo: crate::sudo_dialog::SudoUi::default(),
            // Runde 5, Teil F.
            plan_ui: crate::plan_dialog::PlanUi::default(),
            pending_permission_stage: None,
            mode_before_plan: None,
            overlay: None,
            export_entries: Vec::new(),
            session_title: None,
            title_job_context: None,
            session_store_root: None,
            export_started_at: Some(export_timestamp_now()),
            export_session_model: None,
            export_meta_extensions: ExportMetaExtensions::new(),
            pending_export_options: None,
            pending_export_format: ExportOutputFormat::Markdown,
            pending_quit: None,
            hard_quit_requested: false,
            last_shell_command: None,
            pending_turn_user_cell_override: None,
            needs_terminal_reassert: false,
            workbench: WorkbenchPane::new(),
            mention_popup: None,
            pending_fetches: Vec::new(),
            pending_commands: std::collections::VecDeque::new(),
            overlay_generation: 0,
            background: background_agents::BackgroundUi::default(),
            // Runde 5, Teil O.
            esc_confirm: turn_safety::EscConfirm::default(),
            child_approvals: child_approvals::ChildApprovalUi::default(),
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

    /// Installiert die beim Resume geladene, unveränderliche
    /// Orchestrierungshistorie für die Agentenbaum-Projektion.
    pub(crate) fn set_historic_agent_events(&mut self, events: Vec<AgentOrchestrationEvent>) {
        self.historic_agent_events = events;
    }

    /// Ersetzt die aktive Tastenbelegung (z. B. aus der Keybindings-Datei
    /// geladen); gilt ab der nächsten Taste für Panels und Composer.
    pub(crate) fn set_key_bindings(&mut self, bindings: KeyBindings) {
        self.key_bindings = bindings;
    }

    /// Rebinds persistent input history to the runtime-selected Harw home.
    ///
    /// `ChatApp::new` has a standalone fallback for tests and embedders. The
    /// composition root calls this builder before the event loop starts, so
    /// replacing the editor's initially loaded fallback history is safe and
    /// makes an explicit `--home` authoritative for both reads and writes.
    #[must_use]
    pub(crate) fn with_home(mut self, home: &std::path::Path) -> Self {
        let input_history = InputHistoryStore::at_home(home, INPUT_HISTORY_CAP);
        let mut input = InputEditor::new();
        for entry in input_history.load() {
            input.push_history(entry);
        }
        self.input_history = input_history;
        self.input = input;
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

    /// Runde 5, Teil B: hängt den sudo-Fragekanal (`host.sudo_exec`) und die
    /// Merkfrist für „Für diese Sitzung“ an.
    ///
    /// # Argumente
    /// - `receiver`: Empfänger aus `RuntimeAssembly::take_sudo_prompts`
    ///   (`None` außerhalb einer TUI-Montage — dann gibt es kein Fenster).
    /// - `session_ttl`: Merkfrist (`[host] sudo_session_minutes`);
    ///   `Duration::ZERO` bietet nur „Einmalig“ an.
    ///
    /// # Rückgabe
    /// `Self` für Builder-Verkettung.
    #[must_use]
    pub(crate) fn with_sudo_prompts(
        mut self,
        receiver: Option<harw_tool_shell::SudoPromptReceiver>,
        session_ttl: Duration,
    ) -> Self {
        self.sudo = crate::sudo_dialog::SudoUi::new(receiver, session_ttl);
        self
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

    /// Hinterlegt die Wurzel des Session-Stores für jede Sitzung, bei der sie
    /// bekannt ist (Aufgabe 2, Plan `recursive-cooking-lobster.md` Teil F:
    /// Export-Datum bei Resume für jede Session).
    ///
    /// # Beschreibung
    /// Wird von der Composition-Root (`crate::runtime_root::build_root_runtime`)
    /// **immer** gesetzt, wenn `/resume` in diesem Lauf konfiguriert ist —
    /// unabhängig davon, ob `[session] title_generation` aktiv ist. Anders
    /// als [`Self::with_title_job_context`] (nur bei aktiver Titelerzeugung)
    /// ist dies der zuverlässige Weg, mit dem
    /// [`apply_session_store_started_at`] für jede fortgesetzte Sitzung das
    /// tatsächliche Sitzungsstart-Datum statt des TUI-Startzeitpunkts
    /// auflösen kann.
    ///
    /// # Rückgabe
    /// `Self` für Builder-Verkettung.
    #[must_use]
    pub(crate) fn with_session_store_root(mut self, root: std::path::PathBuf) -> Self {
        self.session_store_root = Some(root);
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

    /// Übernimmt ein im Session-Lifecycle bekannt gewordenes Modell für den
    /// Exportkopf. Ein vorhandener, explizit gesetzter Wert bleibt erhalten.
    fn set_export_session_model(&mut self, model: impl Into<String>) {
        let model = model.into();
        if !model.trim().is_empty() {
            self.export_session_model = Some(model);
        }
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

    /// Liest die aufgelöste Konfiguration aus derselben Runtime-Montage, die
    /// auch die Slash-Dienste bestückt — öffnet niemals eine zweite
    /// Config-Discovery.
    ///
    /// # Beschreibung
    /// Baut die Slash-`ServiceMap` über [`crate::runtime_commands::slash_service_map`]
    /// (dieselbe Fabrik, die `/command`-Dispatches benutzen) und liest daraus
    /// `Arc<harw_config::ResolvedConfig>`. `None` ohne Runtime-Montage oder
    /// wenn die Fläche den Dienst nicht bestückt hat.
    ///
    /// # Rückgabe
    /// `Some(Arc<ResolvedConfig>)`, sofern verfügbar; sonst `None`.
    #[must_use]
    fn resolved_config(&self) -> Option<Arc<harw_config::ResolvedConfig>> {
        let rt = self.runtime.as_ref()?;
        let services = runtime_commands::slash_service_map(rt.services());
        services.get::<Arc<harw_config::ResolvedConfig>>().cloned()
    }

    /// Löst eine konfigurierte Provider-ID oder einen konfigurierten Namen zu
    /// ihrem kanonischen Namen auf (Spiegel von `harw_ops::provider`s
    /// gleichnamiger privater Hilfsfunktion, die von hier aus nicht
    /// referenzierbar ist).
    #[must_use]
    fn canonical_provider_name<'a>(
        config: &'a harw_config::ResolvedConfig,
        requested: &str,
    ) -> Option<&'a str> {
        config.providers.iter().find_map(|(key, provider)| {
            (key == requested || provider.name == requested).then_some(provider.name.as_str())
        })
    }

    /// Liest den aktiven Provider (Snapshot des Controllers) oder — falls
    /// noch nicht explizit gewechselt — den Config-Default.
    ///
    /// # Beschreibung
    /// Dieselbe Präzedenz wie `harw_ops::provider::handle_show`: Laufzeit vor
    /// Konfiguration. Wird sowohl von [`Self::open_model_switch_picker`] (zur
    /// Vorauswahl) als auch vom `/uia-worker-model`-Trigger (zur Auflösung
    /// des effektiven UIA-Providers) verwendet.
    #[must_use]
    fn active_or_default_provider(&self, config: &harw_config::ResolvedConfig) -> Option<String> {
        self.session_controller
            .snapshot()
            .active_provider
            .clone()
            .or_else(|| config.harness.default_provider.clone())
    }

    /// Öffnet den konsolidierten Provider/Modell-Umschalt-Picker (Welle 4a).
    ///
    /// # Beschreibung
    /// Ersetzt die vier vormaligen Öffner (`open_provider_choice`,
    /// `open_uia_provider_choice`, `open_model_choice`,
    /// `open_uia_model_choice`). Baut `providers` (nur aktivierte Provider,
    /// alphabetisch) und `models_by_provider` (alle konfigurierten Modelle,
    /// nach kanonischem Provider gruppiert) aus der aufgelösten Config; ohne
    /// Konfiguration oder ohne aktivierte Provider wird stattdessen eine
    /// klare Systemzeile angehängt — nie ein leerer Dialog (dieselbe
    /// Zurückhaltung wie die vormaligen Öffner).
    ///
    /// Die Vorauswahl unterscheidet sich je `target`:
    /// - Für [`PickerTarget::Orchestrator`]: Controller-Snapshot
    ///   (`active_provider`/`active_model`), sonst `default_provider`/
    ///   `default_model`.
    /// - Für [`PickerTarget::Uia`]: `config.harness.uia_provider`/`uia_model`,
    ///   sonst derselbe Fallback wie beim Orchestrator (aktiver/Standard-
    ///   Provider bzw. Controller-Snapshot-Modell).
    /// - [`PickerTarget::UiaWorker`]: `fixed_provider` ist bereits der
    ///   effektive UIA-Provider (vom Aufrufer aufgelöst); aktives Modell ist
    ///   `config.harness.uia_worker_model`.
    ///
    /// # Argumente
    /// - `target` (`PickerTarget`): Umschalt-Kontext (siehe oben).
    pub(crate) fn open_model_switch_picker(&mut self, target: PickerTarget) {
        let context_label: String = match &target {
            PickerTarget::Orchestrator => "Modell-Auswahl".to_owned(),
            PickerTarget::Uia => "UIA-Modell-Auswahl".to_owned(),
            PickerTarget::UiaWorker { .. } => "UIA-Worker-Modell-Auswahl".to_owned(),
            PickerTarget::Role { .. } => target.context_label(),
            // Runde 5, Teil G.
            PickerTarget::UiaWorkerRole { .. } => target.context_label(),
        };

        let Some(config) = self.resolved_config() else {
            self.push_line(
                Role::System,
                format!("{context_label} nicht verfügbar: keine Konfiguration geladen."),
            );
            return;
        };

        let mut providers: Vec<&harw_config::ProviderToml> = config
            .providers
            .values()
            .filter(|provider| provider.enabled)
            .collect();
        providers.sort_by(|left, right| left.name.cmp(&right.name));

        if providers.is_empty() {
            self.push_line(
                Role::System,
                format!(
                    "{context_label} nicht verfügbar: keine aktivierten Provider konfiguriert."
                ),
            );
            return;
        }

        let provider_entries: Vec<ProviderEntry> = providers
            .iter()
            .map(|provider| ProviderEntry {
                id: provider.name.clone(),
                label: provider.name.clone(),
            })
            .collect();

        let mut models: Vec<&harw_config::ModelToml> = config.models.values().collect();
        models.sort_by(|left, right| left.id.cmp(&right.id));

        let mut models_by_provider: Vec<(String, Vec<ModelEntry>)> = Vec::new();
        for model in models {
            let Some(canonical) = Self::canonical_provider_name(&config, &model.provider) else {
                continue;
            };
            let label = model.name.as_deref().unwrap_or(model.id.as_str());
            let entry = ModelEntry {
                id: model.id.clone(),
                label: format!("{label} ({})", model.id),
            };
            match models_by_provider
                .iter_mut()
                .find(|(id, _)| id.as_str() == canonical)
            {
                Some((_, list)) => list.push(entry),
                None => models_by_provider.push((canonical.to_owned(), vec![entry])),
            }
        }

        let snap = self.session_controller.snapshot();
        let (active_provider, active_model): (Option<String>, Option<String>) = match &target {
            PickerTarget::Orchestrator => (
                snap.active_provider
                    .clone()
                    .or_else(|| config.harness.default_provider.clone()),
                snap.active_model
                    .clone()
                    .or_else(|| config.harness.default_model.clone()),
            ),
            PickerTarget::Uia => (
                config
                    .harness
                    .uia_provider
                    .clone()
                    .or_else(|| self.active_or_default_provider(&config)),
                config
                    .harness
                    .uia_model
                    .clone()
                    .or_else(|| snap.active_model.clone()),
            ),
            PickerTarget::UiaWorker { fixed_provider } => (
                Some(fixed_provider.clone()),
                config.harness.uia_worker_model.clone(),
            ),
            PickerTarget::Role { role } => {
                let row = harw_config::resolve_role_model(&config, *role);
                (row.provider, row.model)
            }
            // Runde 5, Teil G: Vorauswahl = feste Wahl der Rolle, sonst UIA.
            PickerTarget::UiaWorkerRole { role } => {
                match harw_config::resolve_uia_worker_model(&config, role).choice {
                    harw_config::UiaWorkerModelChoice::Fixed { provider, model } => {
                        (Some(provider), Some(model))
                    }
                    harw_config::UiaWorkerModelChoice::FollowUia => {
                        let row =
                            harw_config::resolve_role_model(&config, harw_config::ModelRole::Uia);
                        (row.provider, row.model)
                    }
                }
            }
        };

        let Some(picker) = ModelSwitchPicker::new(
            target,
            provider_entries,
            models_by_provider,
            active_provider.as_deref(),
            active_model.as_deref(),
        ) else {
            self.push_line(
                Role::System,
                format!(
                    "{context_label} nicht verfügbar: keine aktivierten Provider konfiguriert."
                ),
            );
            return;
        };
        self.overlay = Some(Overlay::ModelSwitch(Box::new(picker)));
    }

    /// Öffnet die einstufige Effort-Auswahl (Welle 7b).
    ///
    /// # Beschreibung
    /// Listet alle sechs [`ReasoningEffort`]-Stufen (aufsteigend) plus einen
    /// abschließenden Eintrag „Provider-Default (zurücksetzen)" (emittiert
    /// `clear`). Die Vorauswahl unterscheidet sich je `target`:
    /// [`EffortTarget::Session`] liest den Controller-Snapshot
    /// (`reasoning_effort`), [`EffortTarget::Uia`] den persistierten
    /// `config.harness.reasoning.uia`-Wert (geparst über
    /// [`ReasoningEffort::from_str`]; ein unbekannter oder fehlender Wert
    /// fällt auf keine Vorauswahl zurück).
    ///
    /// # Argumente
    /// - `target` (`EffortTarget`): Session oder UIA.
    pub(crate) fn open_effort_choice(&mut self, target: EffortTarget) {
        let active: Option<ReasoningEffort> = match target {
            EffortTarget::Session => self.session_controller.snapshot().reasoning_effort,
            EffortTarget::Uia => self
                .resolved_config()
                .and_then(|config| config.harness.reasoning.uia.clone())
                .and_then(|value| value.parse::<ReasoningEffort>().ok()),
        };

        let mut options: Vec<String> = EFFORT_LEVELS
            .iter()
            .map(std::string::ToString::to_string)
            .collect();
        options.push("Provider-Default (zurücksetzen)".to_owned());

        let selected = active
            .and_then(|active| EFFORT_LEVELS.iter().position(|level| *level == active))
            .unwrap_or(0);

        let title = match target {
            EffortTarget::Session => "Reasoning-Effort wählen",
            EffortTarget::Uia => "UIA-Reasoning-Effort wählen",
        };
        let dialog = ChoiceDialog::new(title, None, options).with_selected(selected);
        self.overlay = Some(Overlay::EffortChoice { target, dialog });
    }

    /// Gibt `true` zurück, wenn gerade ein Vollflächen-Overlay geöffnet ist.
    #[must_use]
    fn has_overlay(&self) -> bool {
        self.overlay.is_some()
    }

    /// Öffnet die Agentenbaum-Ansicht für die aktuelle Wurzelsitzung.
    ///
    /// Der Snapshot wird bei jedem Rendern neu aus dem gemeinsamen Spawner
    /// gelesen, damit Start, Abschluss und ein Abbruch ohne separaten
    /// TUI-internen Lifecycle-Cache sichtbar werden.
    fn open_agent_tree(&mut self) {
        self.overlay = Some(Overlay::AgentTree(AgentTree::default()));
    }

    /// Erzeugt einen topologisch sortierten, nicht-sensitiven Snapshot des
    /// eigenen Agenten-Teilbaums. Der Spawner besitzt die einzige Quelle für
    /// Admittierung und Status; unbekannte Telemetrie bleibt für die Anzeige
    /// bewusst `None` statt als Nullwert erfunden zu werden.
    fn agent_tree_rows(&self) -> Vec<AgentRow> {
        let root = self.session_id().clone();
        let mut rows = vec![AgentRow {
            id: root.as_str().to_owned(),
            parent: None,
            // Runde 5, Teil I: tatsächliche Wurzelrolle (UIA bzw. expliziter
            // Wurzel-Agent) statt „Wurzel-Orchestrator“.
            role: self.agent_tree_root_label(),
            depth: 0,
            status: "running".to_owned(),
            task: None,
            tokens: None,
            tool_calls: None,
            duration_ms: None,
            budget: "—".to_owned(),
            result: None,
            can_stop: false,
            ..AgentRow::default()
        }];
        let mut seen = HashSet::from([root.as_str().to_owned()]);
        if let Some(spawner) = self.managed_spawner() {
            append_agent_tree_rows(spawner, &root, 1, &mut seen, &mut rows);
        }
        append_historic_agent_tree_rows(&self.historic_agent_events, &root, &mut seen, &mut rows);
        // Runde 5, Teil I: Live-Werte (Monitor) und Spawn-Aufträge ergänzen.
        self.enrich_agent_tree_rows(&mut rows);
        // Runde 5, Teil K: „läuft im Hintergrund" statt nur „running".
        background_agents::mark_rows(self, &mut rows);
        rows
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
                // Runde 5: `Delegated` statt `AlwaysAsk` — Lesen und
                // `plan.write` (nur `.harw/plans`) laufen ohne Rückfrage, alles
                // andere sperrt die Plan-Sperre (`PlanModeGate`).
                self.set_approval_mode(ApprovalMode::Delegated);
                self.active_mode = InteractionMode::Plan;
                if let Err(error) = self
                    .session_controller
                    .request_mode(InteractionMode::Plan.as_str())
                {
                    tracing::warn!(
                        error = %error,
                        "tui.permission_cycle.request_plan_mode_failed"
                    );
                }
            }
            PermissionCycleStage::Ask => self.finish_permission_stage(ApprovalMode::AlwaysAsk),
            PermissionCycleStage::Auto => self.finish_permission_stage(ApprovalMode::Delegated),
            PermissionCycleStage::Full => self.finish_permission_stage(ApprovalMode::FullAccess),
        }
        // Runde 5, Teil F: die Plan-Sperre wirkt sofort, auch mitten im Turn.
        self.sync_plan_lock();
    }

    // Gemeinsamer Abschluss der drei Nicht-Plan-Stufen: setzt den Freigabemodus
    // und stellt, falls der Zyklus gerade aus `Plan` zurückkehrt, den vorherigen
    // Interaktionsmodus wieder her. Vormals ein zweites, strukturell
    // überflüssiges `match stage` mit einem `unreachable!`-Arm für `Plan` (der
    // Fall existiert jetzt gar nicht mehr, da jede Nicht-Plan-Stufe ihren
    // eigenen Match-Arm mit fixem `approval`-Wert hat).
    fn finish_permission_stage(&mut self, approval: ApprovalMode) {
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
            self.tool_cells
                .push(ToolCellHandle::Group(Arc::clone(&group)));
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

    /// Öffnet die Detailansicht für den im Agenten-Panel ausgewählten Agenten
    /// und maximiert das Panel dafür.
    ///
    /// # Rückgabe
    /// `true`, wenn ein Agent ausgewählt war (Redraw nötig).
    fn open_agent_detail(&mut self) -> bool {
        let Some(agent) = self.agent_monitor.selected_agent() else {
            return false;
        };
        let prev_maximized = self
            .agent_detail
            .as_ref()
            .map_or(self.panels.maximized, |detail| detail.prev_maximized);
        self.agent_detail = Some(AgentDetailState {
            agent,
            scroll: 0,
            show_reasoning: true,
            prev_maximized,
        });
        self.panels.maximized = true;
        true
    }

    /// Schließt die Detailansicht und stellt den vorherigen Vollbild-Zustand
    /// des Agenten-Panels wieder her.
    fn close_agent_detail(&mut self) {
        if let Some(detail) = self.agent_detail.take() {
            self.panels.maximized = detail.prev_maximized;
        }
    }

    /// Schließt die Detailansicht, wenn das Agenten-Panel Fokus oder
    /// Sichtbarkeit verloren hat (F3/F4/Esc über die Panel-Logik).
    fn sync_agent_detail(&mut self) {
        let agents_active =
            self.panels.focus == crate::panes::PaneFocus::Agents && self.panels.agents_visible;
        if agents_active || self.agent_detail.is_none() {
            return;
        }
        let keep_maximized = self.panels.maximized;
        self.close_agent_detail();
        // Ein anderes Panel behält einen gerade gewählten Vollbild-Zustand;
        // der Chat braucht keinen.
        self.panels.maximized = match self.panels.focus {
            crate::panes::PaneFocus::Chat => false,
            _ => keep_maximized && self.panels.maximized,
        };
    }

    /// Hängt eine (eingeklappte) Reasoning-Zelle an den Verlauf und merkt sie
    /// für Ctrl+O vor (Runde 2 / Welle 2).
    ///
    /// # Argumente
    /// - `summary` (`String`): Zusammengefasster Denkprozess-Text.
    /// - `origin` (`Option<&str>`): Rolle eines Kind-Agenten; `None` für den
    ///   Hauptagenten.
    fn push_reasoning_cell(&mut self, summary: String, origin: Option<&str>) {
        let mut cell = ReasoningHistoryCell::new(summary);
        if let Some(role) = origin {
            cell = cell.with_origin(role);
        }
        let shared = cell.into_shared();
        self.push_cell(Box::new(Arc::clone(&shared)));
        self.tool_cells.push(ToolCellHandle::Reasoning(shared));
    }

    /// Gibt `true` zurück, wenn mindestens eine Werkzeug- oder Reasoning-Zelle eingeklappt ist
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
        // Runde 5, Teil F: `/mode plan` & Co. ziehen die Plan-Sperre nach.
        self.sync_plan_lock();
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
    pub(crate) fn project_root(&self) -> &str {
        &self.project_root
    }

    /// Abonniert den agenten-übergreifenden Live-Bus des Laufs.
    pub(crate) fn attach_agent_events(&mut self, hub: &harw_core::AgentEventHub) {
        self.agent_rx = Some(hub.subscribe());
    }

    /// Übernimmt die bereits verbrauchte Nutzung einer fortgesetzten Sitzung
    /// in Statuszeile und Agenten-Panel (sonst begänne `/resume` bei 0).
    pub(crate) fn seed_session_usage(&mut self, agent: &str, usage: &TokenUsage) {
        if *usage == TokenUsage::default() {
            return;
        }
        self.total_usage = usage.clone();
        self.agent_monitor
            .seed_usage(agent, "assistant", usage.clone());
    }

    /// Leert den Live-Bus nicht-blockierend in den [`crate::agent_monitor::AgentMonitor`].
    /// Matrix-Spielereignisse gehen stattdessen an eine offene generische
    /// Ansicht ([`OverlayView::apply_event`]); ist das die Matrix-Ansicht,
    /// wird ihr Zustand nachgeladen. Wissensänderungen
    /// (`AgentEventKind::Knowledge`) markieren nur die betroffenen Panels als
    /// veraltet ([`Self::apply_knowledge_event`]).
    /// Liefert `true`, wenn sich Sichtbares geändert hat.
    pub(crate) fn drain_agent_events(&mut self) -> bool {
        let mut changed = self.poll_explorer();
        // Runde 5, Teil C: offenes Kanban-Board alle 2 s gegen die Platte
        // prüfen (Spinner-Takt im Busy-Pfad, Leerlauf-Takt in `run_loop`).
        changed |= self.poll_kanban_live();
        // Runde 5, Teil L: Abschluss einer `/btw`-Nebenfrage im Spinner-Takt.
        changed |= self.poll_btw();
        // Runde 5, Teil P: Goal-Marke und Schritt-Verlaufszeilen (max. 1×/s).
        changed |= self.poll_goal_marker();
        let Some(rx) = self.agent_rx.as_mut() else {
            return changed;
        };
        let mut matrix_events: Vec<serde_json::Value> = Vec::new();
        let mut knowledge_areas: Vec<String> = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(event) => match &event.kind {
                    harw_core::AgentEventKind::Matrix { run_id, event } => {
                        matrix_events.push(serde_json::json!({
                            "run_id": run_id,
                            "event": event,
                        }));
                    }
                    harw_core::AgentEventKind::Knowledge { area, .. } => {
                        knowledge_areas.push(area.clone());
                    }
                    _ => {
                        // Runde 5, Teil I: Turn-Ereignisse der Kinder als
                        // Delta in ihren Live-Block (sofern vorhanden).
                        if let harw_core::AgentEventKind::Turn(turn) = &event.kind {
                            changed |= self.child_stream.apply(
                                &event.agent,
                                event.parent.as_ref(),
                                &event.role,
                                turn,
                            );
                        }
                        changed |= self.agent_monitor.apply(&event);
                    }
                },
                Err(tokio::sync::broadcast::error::TryRecvError::Lagged(skipped)) => {
                    tracing::debug!(skipped, "tui.agent_events.lagged");
                }
                Err(tokio::sync::broadcast::error::TryRecvError::Empty) => break,
                Err(tokio::sync::broadcast::error::TryRecvError::Closed) => {
                    self.agent_rx = None;
                    break;
                }
            }
        }
        // Runde 5, Teil I: neue Kind-Zellen in die Ctrl+O-Liste.
        self.absorb_child_stream_handles();
        if !matrix_events.is_empty() {
            changed |= self.apply_matrix_events(&matrix_events);
        }
        for area in &knowledge_areas {
            changed |= self.apply_knowledge_event(area);
        }
        changed
    }

    /// Markiert nach einer Wissensänderung die betroffenen Panels als
    /// veraltet: das Werkbank-Panel bei `workbench`, eine offene generische
    /// Ansicht (Kanban-Board, KnowledgeBrowser), deren Nachlade-Befehl mit
    /// `/<area>` beginnt. Nachgeladen wird über den normalen Datenabruf
    /// (sichtbarkeitsgeprüfte Op), nicht aus dem Event.
    ///
    /// # Rückgabe
    /// `true`, wenn ein Panel markiert wurde.
    fn apply_knowledge_event(&mut self, area: &str) -> bool {
        let mut marked = false;
        if area == "workbench" {
            self.workbench.mark_stale();
            marked = true;
        }
        let command = format!("/{area}");
        let matches_view = match &self.overlay {
            Some(Overlay::View(view)) => view.refresh_command().is_some_and(|refresh| {
                refresh
                    .split_whitespace()
                    .next()
                    .is_some_and(|head| head == command)
            }),
            _ => false,
        };
        if matches_view {
            self.queue_overlay_refresh();
            marked = true;
        }
        marked
    }

    /// Reicht Matrix-Ereignisse an die offene generische Ansicht weiter und
    /// reiht bei der Matrix-Ansicht deren Nachladen ein.
    ///
    /// # Rückgabe
    /// `true`, wenn eine Ansicht die Ereignisse erhielt (Redraw nötig).
    fn apply_matrix_events(&mut self, events: &[serde_json::Value]) -> bool {
        let Some(Overlay::View(view)) = self.overlay.as_mut() else {
            return false;
        };
        for event in events {
            view.apply_event(event);
        }
        let is_matrix =
            view.refresh_command().as_deref() == Some(crate::matrix_view::REFRESH_COMMAND);
        if is_matrix {
            self.queue_overlay_refresh();
        }
        true
    }

    /// Legt das Explorer-Panel beim ersten Einblenden an und startet die
    /// Hintergrund-Indizierung ab der Projektwurzel (sonst ab dem cwd).
    fn ensure_explorer(&mut self) {
        if self.explorer.is_some() || !self.panels.explorer_visible {
            return;
        }
        let root = if self.project_root.is_empty() {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        } else {
            std::path::PathBuf::from(&self.project_root)
        };
        let mut panel = crate::explorer_panel::ExplorerPanel::new(root);
        panel.start_indexing();
        self.explorer = Some(panel);
    }

    /// Übernimmt einen fertigen Explorer-Index (nicht-blockierend).
    fn poll_explorer(&mut self) -> bool {
        self.explorer
            .as_mut()
            .is_some_and(crate::explorer_panel::ExplorerPanel::poll)
    }

    /// Verwirft den transienten Streaming-Text (finale Zelle liegt vor).
    pub(crate) fn clear_live_stream(&mut self) {
        self.live_stream.clear();
        self.live_reasoning.clear();
    }

    /// Token-Summe für die Statuszeile: live über alle Agenten, sobald der
    /// Bus Daten liefert, sonst die Turn-Summen aus `SessionEvent`s.
    fn display_usage(&self) -> TokenUsage {
        let live = self.agent_monitor.totals();
        if live.total() >= self.total_usage.total() {
            live
        } else {
            self.total_usage.clone()
        }
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
        // Runde 5, Teil L: `/btw` ist bewusst flüchtig — nie in die Historie.
        if btw::is_btw_line(line) {
            return;
        }
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
        // Runde 5, Teil G: „wie UIA“-Worker folgen der Live-Auswahl der UIA.
        uia_workers::sync_live_uia(self, session);
        // Immer nachziehen, nicht nur bei `applied`: der Anzeigezustand soll
        // auch dann stimmen, wenn der Modus beim Aufbau der Session gesetzt
        // wurde oder ein anderer Pfad ihn verändert hat.
        self.active_mode = session.mode();
        // Runde 5, Teil F: Sperre an der Turn-Grenze mit dem Kern abgleichen.
        self.sync_plan_lock();
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

    /// Geklonte Dispatch-Daten für einen Busy-Task (Runde 4, Teil H).
    fn busy_dispatch(&self) -> BusyDispatch {
        BusyDispatch {
            runtime: self.runtime.clone(),
            adapters: Arc::clone(&self.adapters),
            sandbox: self.sandbox.clone(),
            session_id: self.session_id.clone(),
            #[cfg(test)]
            test_controller: self
                .busy_jobs
                .dispatches_without_runtime()
                .then(|| Arc::clone(&self.session_controller)),
        }
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

    /// `true`, solange diese Sitzung eine laufende Host-Arbeitsphase
    /// ([`HostPermitVariant::SessionLease`]) hat.
    ///
    /// # Beschreibung
    /// Fragt direkt [`harw_sandbox::HostPermitSessionRegistry::is_session_approved`]
    /// über die Runtime-Montage ab, statt einen eigenen Merker zu pflegen — der
    /// Ablauf einer Phase (TTL) und ein `/`-seitiges Beenden
    /// ([`Self::end_host_mode`]) wirken dadurch ohne einen zweiten
    /// Wahrheitsort sofort auch hier. `false`, wenn keine Runtime-Montage
    /// vorliegt (z. B. in reinen Renderer-Tests).
    #[must_use]
    pub(crate) fn host_mode_active(&self) -> bool {
        self.runtime.as_ref().is_some_and(|runtime| {
            runtime
                .host_permit_session_registry()
                .is_session_approved(self.session_id.to_string().as_str())
        })
    }

    /// Beendet eine laufende Host-Arbeitsphase dieser Sitzung sofort (Ctrl+H,
    /// siehe [`handle_key`]) und widerruft alle dafür ausgestellten Permits.
    ///
    /// # Beschreibung
    /// Ruft [`harw_sandbox::HostPermitSessionRegistry::forget_session`] (löscht
    /// die Sitzungszustimmung und alle gemerkten Permit-Zuordnungen) und
    /// zusätzlich [`harw_sandbox::ProcessPermitLedger::revoke_session`] (entzieht
    /// auch bereits ausgestellte, aber noch nicht gemerkte Permits derselben
    /// Sitzung) auf. Ohne die zweite Erweiterung könnte ein bereits
    /// ausgestellter, aber dem Renderer nie gemeldeter Permit die Isolation
    /// überdauern.
    ///
    /// # Rückgabe
    /// `true`, wenn tatsächlich eine aktive Phase beendet wurde (und damit ein
    /// Redraw sowie eine Systemzeile angebracht sind); `false`, wenn keine
    /// Phase lief oder keine Runtime-Montage vorliegt.
    pub(crate) fn end_host_mode(&mut self) -> bool {
        let Some(runtime) = self.runtime.clone() else {
            return false;
        };
        let session = self.session_id.to_string();
        let registry = runtime.host_permit_session_registry();
        if !registry.is_session_approved(&session) {
            return false;
        }
        registry.forget_session(&session);
        // Runde 5, Teil N: eine Host-Arbeitsphase gilt prozessweit
        // (`/sandbox-lease`, `request_host`) — Strg+H beendet sie ganz, sonst
        // meldete die Zeile „beendet“, während Kinder weiter auf dem Host
        // liefen.
        registry.revoke_global_approval();
        let _ = runtime.host_permit_ledger().revoke_session(&session);
        self.push_line(Role::System, "Host-Modus beendet — Isolation wieder aktiv.");
        // Register „CSI-Sicherheitsnetz und Paste-Platzhalter", Punkt 7: die
        // beendete Host-Arbeitsphase kann Terminal-Modi (Raw-Mode/Bracketed-
        // Paste/Maus-Capture) beschädigt zurückgelassen haben — der Aufrufer
        // mit Zugriff auf den `TerminalGuard` reasserted sie best-effort.
        self.request_terminal_reassert();
        true
    }

    /// Merkt vor, dass Terminal-Modi best-effort reasserted werden sollen
    /// (siehe [`Self::needs_terminal_reassert`]-Dokumentation).
    pub(crate) fn request_terminal_reassert(&mut self) {
        self.needs_terminal_reassert = true;
    }

    /// Konsumiert die Vormerkung aus [`Self::request_terminal_reassert`].
    ///
    /// # Rückgabe
    /// `true` genau einmal pro Vormerkung — der Aufrufer ruft danach
    /// [`TerminalGuard::reassert_terminal_modes`] auf.
    #[must_use]
    pub(crate) fn take_needs_terminal_reassert(&mut self) -> bool {
        std::mem::take(&mut self.needs_terminal_reassert)
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
        // Runde 5, Teil G: beim Blättern durch die Eingabe-History öffnet
        // ein zurückgeholter `/befehl` kein Popup — Hoch/Runter bleiben bei
        // der History, bis getippt oder editiert wird.
        if self.input.is_browsing_history() {
            self.command_popup = None;
            self.mention_popup = None;
            return;
        }
        self.sync_command_popup();
        self.sync_mention_popup();
    }

    /// Befehls-Popup in zwei Stufen.
    ///
    /// # Beschreibung
    /// Stufe 1 (`/mo`): Autocomplete für den Befehlsnamen, solange kein
    /// Leerraum getippt ist. Stufe 2 (`/kanban mo`): Folgt auf einen
    /// vollständigen, bekannten Befehl genau ein Leerzeichen-getrenntes Wort,
    /// bietet [`CommandPopup::for_subcommands`] dessen Unterkommandos an.
    /// Sobald danach weiterer Leerraum folgt (Argument-Eingabe) oder nichts
    /// passt, schließt das Popup — sonst fingen Ziffern in Argumenten (z. B.
    /// Job-IDs `job-42`) die Eingabe ab.
    fn sync_command_popup(&mut self) {
        let Some(query) = self.input.text().strip_prefix('/') else {
            self.command_popup = None;
            return;
        };
        if !query.contains(char::is_whitespace) {
            let reuse = matches!(
                self.command_popup.as_ref().map(CommandPopup::mode),
                Some(PopupMode::CommandName)
            );
            if !reuse {
                self.command_popup = Some(CommandPopup::new(&self.command_registry));
            }
            if let Some(popup) = self.command_popup.as_mut() {
                popup.on_query_change(query);
            }
            return;
        }
        let Some((name, rest)) = query.split_once(char::is_whitespace) else {
            self.command_popup = None;
            return;
        };
        let sub_query = rest.trim_start();
        if sub_query.contains(char::is_whitespace) {
            self.command_popup = None;
            return;
        }
        let Some(spec) = self.command_registry.find(name) else {
            self.command_popup = None;
            return;
        };
        let canonical = spec.name.as_str().to_owned();
        let reuse = matches!(
            self.command_popup.as_ref().map(CommandPopup::mode),
            Some(PopupMode::Subcommand { command }) if *command == canonical
        );
        if !reuse {
            self.command_popup = CommandPopup::for_subcommands(spec);
        }
        let sub_query = sub_query.to_owned();
        if let Some(popup) = self.command_popup.as_mut() {
            popup.on_query_change(&sub_query);
            if popup.is_empty() {
                self.command_popup = None;
            }
        }
    }

    /// Öffnet, filtert oder schließt das `@`-Erwähnungs-Popup passend zum
    /// Wort unter dem Cursor (nur außerhalb des Befehls-Popups).
    fn sync_mention_popup(&mut self) {
        if self.command_popup.is_some() {
            self.mention_popup = None;
            return;
        }
        let cursor = editor_cursor_byte(&self.input);
        let query = current_mention_query(self.input.text(), cursor).map(|(_, q)| q.to_owned());
        match query {
            Some(query) => {
                if self.mention_popup.is_none() {
                    self.mention_popup = Some(MentionPopup::new(self.mention_candidates()));
                }
                if let Some(popup) = self.mention_popup.as_mut() {
                    popup.filter(&query);
                }
            }
            None => self.mention_popup = None,
        }
    }

    /// Kandidaten für das `@`-Popup: bekannte Rollen und bis zu
    /// [`MENTION_CANDIDATE_CAP`] Dateien unter der Projektwurzel.
    fn mention_candidates(&self) -> Vec<MentionCandidate> {
        let mut candidates: Vec<MentionCandidate> = KNOWN_ROLES
            .iter()
            .map(|role| MentionCandidate::role(*role))
            .collect();
        if !self.project_root.is_empty() {
            candidates.extend(
                scan_mention_candidates(
                    std::path::Path::new(&self.project_root),
                    MENTION_CANDIDATE_CAP,
                )
                .into_iter()
                .map(MentionCandidate::file),
            );
        }
        candidates
    }

    /// Ersetzt das `@token` unter dem Cursor durch `@insert` und schließt das
    /// Popup. Ohne folgenden Text wird ein Leerzeichen angehängt.
    fn accept_mention(&mut self, insert: &str) {
        let text = self.input.text().to_owned();
        let cursor = editor_cursor_byte(&self.input);
        self.mention_popup = None;
        let Some((start, query)) = current_mention_query(&text, cursor) else {
            return;
        };
        let end = start + 1 + query.len();
        let (Some(before), Some(tail)) = (text.get(..start), text.get(end..)) else {
            return;
        };
        let mut head = format!("{before}@{insert}");
        if tail.is_empty() {
            head.push(' ');
        }
        // Puffer ersetzen, ohne gemerkte Pastes zu verlieren: ein Platzhalter
        // vor oder nach der Erwähnung bleibt beim Absenden auflösbar.
        let cursor = head.len();
        self.input
            .replace_text_keeping_pastes(&format!("{head}{tail}"), cursor);
    }

    /// Gibt `true` zurück wenn das `/command`-Popup aktuell geöffnet ist.
    ///
    /// # Rückgabe
    /// `true` wenn ein aktives Popup vorhanden ist, sonst `false`.
    #[must_use]
    fn has_popup(&self) -> bool {
        self.command_popup.is_some()
    }

    /// Aktiver Freigabemodus aus der geteilten Zelle der Runtime-Montage.
    fn current_approval(&self) -> Option<ApprovalMode> {
        self.runtime.as_ref().map(|rt| rt.approval_mode().get())
    }

    /// Berechtigungsstufe des TUI-Nutzers; ohne Runtime-Montage die
    /// niedrigste Stufe.
    fn caller_tier(&self) -> crate::PermissionTier {
        self.runtime
            .as_ref()
            .map_or(crate::PermissionTier::Observer, |rt| {
                runtime_commands::caller_tier(rt.principal())
            })
    }

    /// Live-Provider und -Modell der Sitzung: Controller-Snapshot, sonst das
    /// zuletzt gemeldete Sitzungsmodell.
    fn live_model(&self) -> (Option<String>, Option<String>) {
        let snap = self.session_controller.snapshot();
        let model = snap
            .active_model
            .clone()
            .or_else(|| self.export_session_model.clone());
        (snap.active_provider, model)
    }

    /// Öffnet eine generische Ansicht und reiht ihren Initial-Abruf ein.
    fn open_overlay_view(&mut self, view: Box<dyn OverlayView>) {
        self.overlay_generation = self.overlay_generation.wrapping_add(1);
        self.overlay = Some(Overlay::View(view));
        self.queue_overlay_refresh();
    }

    /// Reiht den `refresh_command` der offenen generischen Ansicht ein.
    fn queue_overlay_refresh(&mut self) {
        let command = match &self.overlay {
            Some(Overlay::View(view)) => view.refresh_command(),
            _ => None,
        };
        if let Some(command) = command {
            self.queue_overlay_fetch(command);
        }
    }

    /// Reiht einen Datenabruf für die offene generische Ansicht ein
    /// (doppelte Einträge werden zusammengefasst).
    fn queue_overlay_fetch(&mut self, command: String) {
        let fetch = DataFetch::Overlay {
            command,
            generation: self.overlay_generation,
        };
        if !self.pending_fetches.contains(&fetch) {
            self.pending_fetches.push(fetch);
        }
    }

    /// `true`, wenn das Werkbank-Panel sichtbar ist und neu laden sollte.
    fn workbench_needs_refresh(&self) -> bool {
        self.panels.workbench_visible && self.workbench.is_stale()
    }

    /// Schaltet das Werkbank-Panel um (wie `F5`); beim Einblenden wird es
    /// als veraltet markiert und damit neu geladen.
    fn toggle_workbench(&mut self) {
        self.panels.workbench_visible = !self.panels.workbench_visible;
        if self.panels.workbench_visible {
            self.workbench.mark_stale();
        } else if self.panels.focus == crate::panes::PaneFocus::Workbench {
            self.panels.focus = crate::panes::PaneFocus::Chat;
            self.panels.maximized = false;
        }
    }

    /// Setzt die Composer-Zeile auf `text` (Cursor am Ende) und gibt den
    /// Fokus an den Chat.
    fn prefill_composer(&mut self, text: &str) {
        self.input.clear();
        self.input.insert_str(text);
        self.panels.focus = crate::panes::PaneFocus::Chat;
        self.panels.maximized = false;
        self.sync_popup();
    }

    /// Tasten des fokussierten Werkbank-Panels.
    ///
    /// # Rückgabe
    /// `true`, wenn neu gezeichnet werden soll.
    fn handle_workbench_key(&mut self, key: KeyEvent) -> bool {
        match self.workbench.handle_key(key) {
            PaneCommand::None => false,
            PaneCommand::Redraw => true,
            PaneCommand::Run(command) => {
                if command == self.workbench.refresh_command() {
                    // Neu laden ohne Chat-Ausgabe.
                    self.workbench.mark_stale();
                } else {
                    self.pending_commands.push_back(command);
                }
                true
            }
            PaneCommand::Prefill(text) => {
                self.prefill_composer(&text);
                true
            }
            PaneCommand::ReleaseFocus => {
                self.panels.focus = crate::panes::PaneFocus::Chat;
                self.panels.maximized = false;
                true
            }
        }
    }

    /// Leert die sichtbaren Verlaufszellen (`/clear`). Sitzungsverlauf,
    /// Modellkontext und Exporteinträge bleiben unverändert.
    fn clear_transcript(&mut self) {
        self.cells.clear();
        self.tool_cells.clear();
        self.open_tool_group = None;
        self.ctrl_o_expand_last_armed = false;
        self.live_stream.clear();
        self.live_reasoning.clear();
        self.scroll.force_follow();
    }

    /// Schaltet die ausführliche Werkzeuganzeige um (`/verbose`).
    ///
    /// # Beschreibung
    /// Neue Werkzeugzellen erhalten die neue [`ToolVerbosity`]; bestehende
    /// Zellen werden passend auf- bzw. zugeklappt.
    ///
    /// # Rückgabe
    /// `true`, wenn jetzt die ausführliche Anzeige aktiv ist.
    fn toggle_verbose(&mut self) -> bool {
        let verbose = self.tool_verbosity != ToolVerbosity::Verbose;
        self.tool_verbosity = if verbose {
            ToolVerbosity::Verbose
        } else {
            ToolVerbosity::Compact
        };
        for handle in &self.tool_cells {
            handle.set_expanded(verbose);
        }
        self.ctrl_o_expand_last_armed = false;
        verbose
    }

    /// `/uia-worker-model` bare: Picker für den an den effektiven
    /// UIA-Provider gebundenen Worker.
    ///
    /// # Beschreibung
    /// Der Worker ist zwingend an den effektiven UIA-Provider gebunden
    /// (UIA-Pin, sonst der aktive/Standard-Provider) — keine eigene
    /// `uia_worker_provider`-Konzeption.
    fn open_uia_worker_picker(&mut self) {
        match self.resolved_config() {
            Some(config) => {
                let provider = config
                    .harness
                    .uia_provider
                    .clone()
                    .or_else(|| self.active_or_default_provider(&config));
                match provider {
                    Some(fixed_provider) => {
                        self.open_model_switch_picker(PickerTarget::UiaWorker { fixed_provider });
                    }
                    None => self.push_line(
                        Role::System,
                        "UIA-Worker-Modell-Auswahl nicht verfügbar: kein \
                         UIA-Pin-, aktiver oder Standard-Provider bekannt.",
                    ),
                }
            }
            None => self.push_line(
                Role::System,
                "UIA-Worker-Modell-Auswahl nicht verfügbar: keine \
                 Konfiguration geladen.",
            ),
        }
    }

    /// Modus-/Freigabe-Auswahl mit aktuellem Zustand (`F7`, `/mode`).
    fn mode_picker_view(&self) -> ModePicker {
        let config = self.resolved_config();
        ModePicker::new(
            self.active_mode,
            config
                .as_deref()
                .map(|config| config.harness.mode.default.as_str()),
            self.current_approval(),
        )
    }

    /// Modelle je Rolle (`F8`, `/models`).
    fn model_roles_view(&self) -> ModelRolesView {
        let (provider, model) = self.live_model();
        match self.resolved_config() {
            Some(config) => {
                ModelRolesView::from_config(&config, provider.as_deref(), model.as_deref())
            }
            None => ModelRolesView::empty(provider.as_deref(), model.as_deref()),
        }
    }
}

/// Byte-Offset des Composer-Cursors.
///
/// # Beschreibung
/// `InputEditor::cursor` ist nur in Tests sichtbar; der Offset wird deshalb
/// aus [`InputEditor::cursor_position`] mit Breite `0` (keine weichen
/// Umbrüche: Zeile = harte Zeile, Spalte = Anzeigebreite davor)
/// zurückgerechnet. Ergebnis liegt immer auf einer Zeichengrenze.
fn editor_cursor_byte(editor: &InputEditor) -> usize {
    let (row, col) = editor.cursor_position(0);
    let text = editor.text();
    let mut line_start = 0usize;
    for _ in 0..row {
        match text.get(line_start..).and_then(|rest| rest.find('\n')) {
            Some(pos) => line_start += pos + 1,
            None => return text.len(),
        }
    }
    let line = text.get(line_start..).unwrap_or("");
    let line_end = line.find('\n').map_or(text.len(), |pos| line_start + pos);
    let mut width = 0usize;
    for (offset, grapheme) in text
        .get(line_start..line_end)
        .unwrap_or("")
        .grapheme_indices(true)
    {
        if width >= col {
            return line_start + offset;
        }
        width += UnicodeWidthStr::width(grapheme);
    }
    line_end
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

    /// Reassertiert Raw-Mode, Bracketed-Paste und Maus-Capture best-effort
    /// (Register „CSI-Sicherheitsnetz und Paste-Platzhalter", Punkt 7).
    ///
    /// # Beschreibung
    /// Ein Host-`shell.exec`/`!`-Lauf oder das Ende einer Host-Arbeitsphase
    /// (Ctrl+H) kann diese Terminal-Modi beschädigt zurücklassen — z. B.
    /// wenn eine ausgeführte Fremd-Anwendung sie selbst geändert hat. Diese
    /// Funktion stellt sie erneut her, ohne den Bildschirm zu löschen oder
    /// den Alternate-Screen zu verlassen/erneut zu betreten. Fehler werden
    /// protokolliert, aber nicht propagiert — ein fehlgeschlagenes
    /// Selbstheilen darf die TUI niemals abstürzen lassen.
    pub(crate) fn reassert_terminal_modes(&mut self) {
        if let Err(error) = enable_raw_mode() {
            tracing::warn!(%error, "tui.terminal.reassert_raw_mode_failed");
        }
        if let Err(error) = crossterm::execute!(
            self.terminal.backend_mut(),
            EnableBracketedPaste,
            EnableMouseCapture,
        ) {
            tracing::warn!(%error, "tui.terminal.reassert_modes_failed");
        }
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
    match words.next()? {
        "/new" => return Some(TuiRunOutcome::NewSession),
        "/resume" => {}
        _ => return None,
    }
    match words.next() {
        None => Some(TuiRunOutcome::Resume { selector: None }),
        Some(selector) if words.next().is_none() => Some(TuiRunOutcome::Resume {
            selector: Some(selector.to_owned()),
        }),
        Some(_) => None,
    }
}

/// `true`, wenn nach `/befehl` nur Leerraum steht (Unterkommando-Popup ohne
/// Suchtext).
fn subcommand_query_is_empty(text: &str) -> bool {
    text.strip_prefix('/')
        .and_then(|rest| rest.split_once(char::is_whitespace))
        .is_none_or(|(_, query)| query.trim().is_empty())
}

/// Hängt per `@pfad` erwähnte Projektdateien an eine Chat-Nachricht an.
///
/// # Rückgabe
/// `(modelltext, hinweis)`: der Text für das Modell (Original plus
/// `<datei>`-Blöcke) und optional eine Systemzeile mit angehängten und
/// abgelehnten Dateien. Ohne Projektwurzel bleibt der Text unverändert.
fn expand_mentions_for_turn(project_root: &str, text: &str) -> (String, Option<String>) {
    if project_root.is_empty() {
        return (text.to_owned(), None);
    }
    let root = std::path::Path::new(project_root);
    let expanded = expand_file_mentions(text, root, MentionLimits::default());
    if expanded.attached.is_empty() && expanded.rejected.is_empty() {
        return (expanded.text, None);
    }
    let mut lines = Vec::new();
    if !expanded.attached.is_empty() {
        let canonical_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let names: Vec<String> = expanded
            .attached
            .iter()
            .map(|path| {
                path.strip_prefix(&canonical_root)
                    .unwrap_or(path)
                    .display()
                    .to_string()
            })
            .collect();
        lines.push(format!("Angehängt: {}", names.join(", ")));
    }
    for (token, reason) in &expanded.rejected {
        lines.push(format!("Nicht angehängt: @{token} — {reason}"));
    }
    (expanded.text, Some(lines.join("\n")))
}

/// `true` für jede `/workbench …`-Zeile (Werkbank danach neu laden).
fn is_workbench_command(raw: &str) -> bool {
    raw.split_whitespace().next() == Some("/workbench")
}

/// Baut den [`LocalCommandContext`] aus dem App-Zustand und fragt
/// [`local_commands::intercept`]. Das Ergebnis besitzt keine Borrows auf
/// `app`, der Aufrufer darf danach mutieren.
fn local_intercept_for(app: &ChatApp, raw: &str) -> Option<LocalIntercept> {
    let config = app.resolved_config();
    let (live_provider, live_model) = app.live_model();
    let project_root = std::path::PathBuf::from(&app.project_root);
    let ctx = LocalCommandContext {
        registry: &app.command_registry,
        config: config.as_deref(),
        key_bindings: &app.key_bindings,
        active_mode: app.active_mode,
        approval: app.current_approval(),
        live_provider: live_provider.as_deref(),
        live_model: live_model.as_deref(),
        project_root: &project_root,
        session_id: app.session_id(),
        tier: app.caller_tier(),
        known_roles: KNOWN_ROLES,
    };
    local_commands::intercept(raw, &ctx)
}

/// Wendet ein [`LocalIntercept`] an.
///
/// # Rückgabe
/// `Some(outcome)`, wenn die TUI-Schleife mit diesem Ergebnis enden soll
/// (bare `/resume`); sonst `None`.
fn apply_local_intercept(
    app: &mut ChatApp,
    intercepted: LocalIntercept,
    bus: &HarwEventSender,
) -> Option<TuiRunOutcome> {
    match intercepted {
        LocalIntercept::OpenOverlay(view) => app.open_overlay_view(view),
        LocalIntercept::OpenModelPicker(target) => app.open_model_switch_picker(target),
        LocalIntercept::OpenUiaWorkerPicker => app.open_uia_worker_picker(),
        // Runde 5, Teil G: UIA-Wahl, danach Bereich „UIA-Worker-Modelle“.
        LocalIntercept::OpenUiaPickerThenWorkers => app.open_uia_picker_then_workers(),
        LocalIntercept::OpenEffortChoice(target) => app.open_effort_choice(target),
        LocalIntercept::OpenAgentTree => app.open_agent_tree(),
        LocalIntercept::OpenSessionPicker => {
            return Some(TuiRunOutcome::Resume { selector: None });
        }
        LocalIntercept::TogglePanel(PanelToggle::Workbench) => app.toggle_workbench(),
        LocalIntercept::TogglePanel(PanelToggle::Agents) => {
            app.panels.agents_visible = !app.panels.agents_visible;
            if !app.panels.agents_visible && app.panels.focus == crate::panes::PaneFocus::Agents {
                app.panels.focus = crate::panes::PaneFocus::Chat;
                app.panels.maximized = false;
            }
            app.sync_agent_detail();
        }
        LocalIntercept::TogglePanel(PanelToggle::Explorer) => {
            app.panels.explorer_visible = !app.panels.explorer_visible;
            if !app.panels.explorer_visible && app.panels.focus == crate::panes::PaneFocus::Explorer
            {
                app.panels.focus = crate::panes::PaneFocus::Chat;
                app.panels.maximized = false;
            }
            app.ensure_explorer();
        }
        LocalIntercept::ToggleVerbose => {
            let text = if app.toggle_verbose() {
                "Ausführliche Werkzeuganzeige: an"
            } else {
                "Ausführliche Werkzeuganzeige: aus"
            };
            app.push_lines(vec![Line::from(text)]);
        }
        LocalIntercept::ClearTranscript => {
            app.clear_transcript();
            app.push_lines(vec![Line::from(
                "Anzeige geleert — Sitzung und Modellkontext bleiben erhalten.",
            )]);
        }
        LocalIntercept::RenameSession(title) => {
            let title = title.trim().to_owned();
            app.set_session_title(title.clone());
            app.push_lines(vec![Line::from(format!(
                "Sitzungstitel (Anzeige) gesetzt: „{title}“"
            ))]);
        }
        LocalIntercept::Rewrite(line) => bus.send(HarwEvent::Command(line)),
        LocalIntercept::Chat(text) => {
            app.scroll.force_follow();
            app.pending_turns.push_back(text);
        }
        LocalIntercept::System(text) => {
            app.push_lines(
                text.split('\n')
                    .map(|line| Line::from(line.to_owned()))
                    .collect(),
            );
        }
        // Runde 5, Teil F: `/plan`, `/plan show|edit|list|open`.
        LocalIntercept::Plan(command) => plan_mode::apply_plan_command(app, &command),
        // Runde 5, Teil I: `/agent stream <orchestrators|all|none>`.
        LocalIntercept::ChildStream(args) => app.apply_child_stream_command(&args),
        // Runde 5, Teil L: `/btw <frage>` — Nebenfrage ohne Werkzeuge.
        LocalIntercept::Btw(question) => app.start_btw(&question),
        // Runde 5, Teil K: `/agent bg`, `/agent cancel <id>`.
        LocalIntercept::BackgroundAgents(args) => {
            background_agents::apply_agents_command(app, &args);
        }
    }
    None
}

/// Führt `raw` über [`command_data::execute_command_with_data`] aus, ohne
/// etwas in den Chat zu schreiben.
///
/// Nimmt bewusst nur Feld-Referenzen statt `&ChatApp` (dieselbe Form wie die
/// übrigen Dispatch-Aufrufe in [`run_loop`]), damit über das `await` keine
/// Referenz auf den ganzen App-Zustand gehalten wird.
async fn fetch_command_data(
    runtime: Option<&Arc<harw_runtime::RuntimeAssembly>>,
    adapters: &[CommandAdapter],
    sandbox: &SandboxSpec,
    session_id: &SessionId,
    raw: &str,
) -> Result<OpOutput, String> {
    let Some(rt) = runtime else {
        return Err("Fehler: keine Runtime-Montage".to_owned());
    };
    command_data::execute_command_with_data(
        adapters,
        sandbox,
        session_id,
        runtime_commands::caller_tier(rt.principal()),
        raw,
        || runtime_commands::slash_service_map(rt.services()),
    )
    .await
}

/// Nimmt alle ausstehenden Datenabrufe (siehe [`DataFetch`]) aus dem
/// App-Zustand und ergänzt das Werkbank-Panel, wenn es sichtbar und veraltet
/// ist und nicht schon ein Abruf dafür läuft.
///
/// # Beschreibung
/// Abrufe für eine inzwischen ersetzte oder geschlossene Ansicht werden
/// schon hier verworfen. Gemeinsame Vorstufe für den Leerlauf
/// ([`process_pending_fetches`]) und den Busy-Pfad ([`spawn_busy_fetches`]).
///
/// # Rückgabe
/// `(ziel, befehlszeile)` je Abruf, in Einreihungsreihenfolge.
fn take_fetch_requests(app: &mut ChatApp) -> Vec<(FetchTarget, String)> {
    let mut fetches = std::mem::take(&mut app.pending_fetches);
    if app.workbench_needs_refresh()
        && !app.busy_jobs.workbench_in_flight()
        && !fetches.contains(&DataFetch::Workbench)
    {
        fetches.push(DataFetch::Workbench);
    }
    fetches
        .into_iter()
        .filter_map(|fetch| match fetch {
            DataFetch::Overlay {
                command,
                generation,
            } => (generation == app.overlay_generation
                && matches!(app.overlay, Some(Overlay::View(_))))
            .then_some((FetchTarget::Overlay { generation }, command)),
            DataFetch::Workbench => Some((
                FetchTarget::Workbench,
                app.workbench.refresh_command().to_owned(),
            )),
        })
        .collect()
}

/// Wendet das Ergebnis eines Datenabrufs auf Ansicht bzw. Werkbank an.
///
/// # Beschreibung
/// Ein Ergebnis für eine inzwischen ersetzte Ansicht (andere Generation)
/// wird verworfen.
fn apply_fetch_result(app: &mut ChatApp, target: FetchTarget, result: Result<OpOutput, String>) {
    match target {
        FetchTarget::Overlay { generation } => {
            if generation != app.overlay_generation {
                return;
            }
            if let Some(Overlay::View(view)) = app.overlay.as_mut() {
                match result {
                    Ok(output) => match output.data {
                        Some(data) => view.apply_data(&data),
                        None => view.apply_error(&output.text),
                    },
                    Err(error) => view.apply_error(&error),
                }
            }
        }
        FetchTarget::Workbench => match result {
            Ok(output) => match output.data {
                Some(data) => app.workbench.apply_data(&data),
                None => app.workbench.apply_error(output.text),
            },
            Err(error) => app.workbench.apply_error(error),
        },
    }
}

/// Arbeitet alle ausstehenden Datenabrufe ab (siehe [`DataFetch`]); lädt
/// zusätzlich das Werkbank-Panel, wenn es sichtbar und veraltet ist.
///
/// # Rückgabe
/// `true`, wenn mindestens ein Abruf lief (Redraw nötig).
async fn process_pending_fetches(app: &mut ChatApp) -> bool {
    let requests = take_fetch_requests(app);
    if requests.is_empty() {
        return false;
    }
    for (target, command) in requests {
        let result = fetch_command_data(
            app.runtime.as_ref(),
            &app.adapters,
            &app.sandbox,
            &app.session_id,
            &command,
        )
        .await;
        apply_fetch_result(app, target, result);
    }
    true
}

/// Busy-Gegenstück zu [`process_pending_fetches`]: startet jeden
/// ausstehenden Datenabruf als eigenen Task, statt ihn im `select!`-Arm
/// abzuwarten (Runde 4, Teil H). Die Ergebnisse kommen über
/// [`ChatApp::busy_jobs`] zurück ([`apply_busy_job_done`]).
///
/// # Rückgabe
/// `true`, wenn mindestens ein Abruf gestartet wurde.
fn spawn_busy_fetches(app: &mut ChatApp) -> bool {
    let requests = take_fetch_requests(app);
    if requests.is_empty() {
        return false;
    }
    let dispatch = app.busy_dispatch();
    for (target, command) in requests {
        app.busy_jobs.start_fetch(&dispatch, target, command);
    }
    true
}

/// Wendet das Ergebnis einer Taste in einer generischen Ansicht an.
fn apply_overlay_outcome(app: &mut ChatApp, outcome: OverlayOutcome, bus: &HarwEventSender) {
    match outcome {
        OverlayOutcome::Stay => {}
        OverlayOutcome::Close => app.overlay = None,
        // Der `HarwEvent::Command`-Zweig lädt die offene Ansicht danach neu
        // (`queue_overlay_refresh`).
        OverlayOutcome::Run(command) => {
            // Runde 5, Teil G: `r`/`a` im Worker-Bereich wirken sofort.
            uia_workers::observe_command(app, &command);
            bus.send(HarwEvent::Command(command));
        }
        OverlayOutcome::RunAndClose(command) => {
            app.overlay = None;
            bus.send(HarwEvent::Command(command));
        }
        OverlayOutcome::Fetch(command) => app.queue_overlay_fetch(command),
        OverlayOutcome::Prefill(text) => {
            app.overlay = None;
            app.prefill_composer(&text);
        }
    }
}

/// Globale Ansichtstasten (Standard `F1` Hilfe, `F6` Kanban, `F7`
/// Modus/Freigabe, `F8` Modelle je Rolle, `F9` Matrix-Game), unabhängig vom
/// Panel-Fokus.
///
/// # Beschreibung
/// Greift nicht, solange eine Freigabefrage oder ein anderes als ein
/// generisches Overlay offen ist; eine offene generische Ansicht wird
/// ersetzt.
///
/// # Rückgabe
/// `Some(true)`, wenn eine Ansicht geöffnet wurde; `None` sonst.
fn handle_view_hotkey(app: &mut ChatApp, key: KeyEvent) -> Option<bool> {
    if app.pending_approval_dialog.is_some() || app.pending_host_permit_dialog.is_some() {
        return None;
    }
    if app
        .overlay
        .as_ref()
        .is_some_and(|overlay| !matches!(overlay, Overlay::View(_)))
    {
        return None;
    }
    let view: Box<dyn OverlayView> = match app.key_bindings.action_for(&key)? {
        KeyAction::OpenKanban => Box::new(KanbanBoard::new()),
        KeyAction::OpenMatrix => Box::new(MatrixView::new()),
        KeyAction::OpenModePicker => Box::new(app.mode_picker_view()),
        KeyAction::OpenModels => Box::new(app.model_roles_view()),
        KeyAction::ShowHelp => Box::new(HelpOverlay::new(
            &app.command_registry,
            &app.key_bindings,
            HelpTab::Commands,
        )),
        _ => return None,
    };
    app.open_overlay_view(view);
    Some(true)
}

/// Tasten des `@`-Erwähnungs-Popups (↑/↓, Enter/Tab übernehmen, Esc).
///
/// # Rückgabe
/// `Some(redraw)`, wenn das Popup die Taste verarbeitet hat; `None` gibt sie
/// an den Composer weiter (auch bei leerer Trefferliste, damit Enter weiter
/// absendet).
fn handle_mention_popup_key(app: &mut ChatApp, key: KeyEvent) -> Option<bool> {
    let popup = app.mention_popup.as_mut()?;
    if popup.is_empty()
        || key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT)
        || !matches!(
            key.code,
            KeyCode::Up | KeyCode::Down | KeyCode::Enter | KeyCode::Tab | KeyCode::Esc
        )
    {
        return None;
    }
    match popup.handle_key(key) {
        MentionPopupAction::Stay => {}
        MentionPopupAction::Cancel => app.mention_popup = None,
        MentionPopupAction::Accept(insert) => app.accept_mention(&insert),
    }
    Some(true)
}

/// Appends the controller-owned descendants of `parent` in pre-order.
///
/// The `seen` guard makes a malformed controller snapshot harmless for the
/// renderer. It never grants authority: stopping is still checked again by the
/// controller against the caller's owned subtree.
fn append_agent_tree_rows(
    spawner: &ManagedAgentSpawner,
    parent: &SessionId,
    depth: usize,
    seen: &mut HashSet<String>,
    rows: &mut Vec<AgentRow>,
) {
    for record in spawner.list_children_for(parent) {
        let id = record.child.as_str().to_owned();
        if !seen.insert(id.clone()) {
            continue;
        }
        let budget = match (
            record.budget.max_tokens,
            record.budget.max_tool_calls,
            record.budget.max_wall_time_ms,
        ) {
            (None, None, None) => "—".to_owned(),
            (tokens, tools, wall) => format!(
                "Tokens {} · Tools {} · Dauer {} ms",
                tokens.map_or_else(|| "—".to_owned(), |value| value.to_string()),
                tools.map_or_else(|| "—".to_owned(), |value| value.to_string()),
                wall.map_or_else(|| "—".to_owned(), |value| value.to_string()),
            ),
        };
        let child = record.child.clone();
        rows.push(AgentRow {
            id,
            parent: Some(record.parent.as_str().to_owned()),
            role: record.role,
            depth,
            status: record.status.as_str().to_owned(),
            task: None,
            tokens: None,
            tool_calls: None,
            duration_ms: None,
            budget,
            result: None,
            can_stop: !record.status.is_terminal(),
            // Runde 5, Teil I: Deckel für die Budget-Auslastung.
            budget_limits: crate::agent_tree_live::BudgetLimits {
                tokens: record.budget.max_tokens,
                tool_calls: record.budget.max_tool_calls,
                wall_ms: record.budget.max_wall_time_ms,
            },
            live: None,
        });
        append_agent_tree_rows(spawner, &child, depth.saturating_add(1), seen, rows);
    }
}

/// Fügt die beim Resume geladene, letzte Beobachtung jedes historischen
/// Kindes hinzu. Diese Projektion besitzt keine Ausführungsautorität: ein
/// nicht-terminaler Eintrag stammt aus einem früheren Prozess und wird daher
/// sichtbar als „unterbrochen“, nie als weiter laufender oder stoppbarer Job.
fn append_historic_agent_tree_rows(
    events: &[AgentOrchestrationEvent],
    root: &SessionId,
    seen: &mut HashSet<String>,
    rows: &mut Vec<AgentRow>,
) {
    let mut latest = HashMap::<String, AgentOrchestrationEvent>::new();
    for event in events.iter().filter(|event| &event.root_session_id == root) {
        let id = event.child_session_id.as_str().to_owned();
        if let Some(previous) = latest.get(&id) {
            let mut merged = event.clone();
            if merged.task.is_none() {
                merged.task = previous.task.clone();
            }
            latest.insert(id, merged);
        } else {
            latest.insert(id, event.clone());
        }
    }
    let mut events: Vec<_> = latest.into_values().collect();
    events.sort_by(|left, right| {
        left.depth.cmp(&right.depth).then_with(|| {
            left.child_session_id
                .as_str()
                .cmp(right.child_session_id.as_str())
        })
    });
    let mut depths: HashMap<String, usize> =
        rows.iter().map(|row| (row.id.clone(), row.depth)).collect();
    for event in events {
        let id = event.child_session_id.as_str().to_owned();
        if !seen.insert(id.clone()) {
            continue;
        }
        let parent = event.parent_session_id.as_str().to_owned();
        let depth = depths
            .get(&parent)
            .copied()
            .map_or(event.depth as usize, |parent_depth| {
                parent_depth.saturating_add(1)
            });
        // Ein einziges erschöpfendes `match` statt vorherigem `matches!`-Check
        // plus zweitem `match` mit `unreachable!`-Fallback: die nicht-
        // terminalen Stufen sind hier explizit aufgezählt statt über `_`
        // "irgendwie schon terminal" anzunehmen.
        let status = match event.status {
            AgentOrchestrationStatus::Completed => "completed",
            AgentOrchestrationStatus::Failed => "failed",
            AgentOrchestrationStatus::Cancelled => "cancelled",
            AgentOrchestrationStatus::Admitted
            | AgentOrchestrationStatus::Running
            | AgentOrchestrationStatus::Progress
            | AgentOrchestrationStatus::Paused => "interrupted",
        }
        .to_owned();
        rows.push(AgentRow {
            id: id.clone(),
            parent: Some(parent),
            role: event.role,
            depth,
            status,
            task: event.task,
            tokens: event.usage.as_ref().map(TokenUsage::total),
            tool_calls: None,
            duration_ms: event.duration_ms,
            budget: "—".to_owned(),
            result: event.detail,
            can_stop: false,
            // Runde 5, Teil I.
            ..AgentRow::default()
        });
        depths.insert(id, depth);
    }
}

/// Resolves the controller-owned parent for an exported child lifecycle event.
///
/// The controller record is authoritative whenever it is still retained. The
/// current session is the event source and is the safe fallback when a child
/// completed quickly enough that its record has already been released.
fn exported_agent_parent_id(app: &ChatApp, child: &SessionId) -> Option<String> {
    app.managed_spawner()
        .and_then(|spawner| spawner.child_record(child))
        .map(|record| record.parent.as_str().to_owned())
        .or_else(|| Some(app.session_id().as_str().to_owned()))
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

fn export_tool_result_value(result: &ToolCallResult) -> serde_json::Value {
    serde_json::to_value(result).unwrap_or_else(|_| {
        serde_json::json!({
            "status": "error",
            "message": "tool result could not be serialized",
        })
    })
}

fn export_tool_status(result: &ToolCallResult) -> ExportStatus {
    if result.is_success() {
        ExportStatus::Success
    } else {
        ExportStatus::Error
    }
}

fn export_tool_error(result: &ToolCallResult) -> Option<String> {
    match result {
        ToolCallResult::Success { .. } => None,
        ToolCallResult::Error { message } => Some(message.clone()),
    }
}

fn export_trust(trust: ResultTrust) -> String {
    match trust {
        ResultTrust::Untrusted => "untrusted".to_owned(),
        ResultTrust::Runtime => "runtime".to_owned(),
    }
}

fn export_tool_call_entry(
    call_id: &harw_types::ToolCallId,
    tool_name: &str,
    arguments: serde_json::Value,
) -> ExportEntry {
    ExportEntry::ToolCall {
        call_id: call_id.to_string(),
        tool_name: tool_name.to_owned(),
        arguments,
        duration_ms: None,
        trust: None,
        agent: None,
    }
}

fn export_tool_result_entry(
    call_id: &harw_types::ToolCallId,
    tool_name: Option<String>,
    result: &ToolCallResult,
    duration_ms: u64,
    trust: Option<ResultTrust>,
) -> ExportEntry {
    ExportEntry::ToolResult {
        call_id: call_id.to_string(),
        tool_name,
        result: export_tool_result_value(result),
        status: export_tool_status(result),
        error: export_tool_error(result),
        duration_ms: Some(duration_ms),
        trust: trust.map(export_trust),
        agent: None,
    }
}

fn hydrate_visible_history(app: &mut ChatApp, history: &ConversationHistory) {
    let mut tool_cells: HashMap<harw_types::ToolCallId, SharedToolCell> = HashMap::new();
    let mut tool_export_indices: HashMap<harw_types::ToolCallId, usize> = HashMap::new();
    let mut tool_export_order: Vec<harw_types::ToolCallId> = Vec::new();
    for item in history.items() {
        match item {
            TurnItem::UserMessage(message) => {
                app.push_line(Role::User, visible_message_text(&message.content));
            }
            TurnItem::AssistantMessage(message) => {
                app.push_line(Role::Assistant, visible_message_text(&message.content));
            }
            TurnItem::ToolCall(call_item) => {
                let call = ToolCall {
                    id: call_item.call_id.clone(),
                    name: ToolName::new(call_item.tool_name.clone()),
                    arguments: call_item.arguments.clone(),
                };
                match tool_cells.get(&call_item.call_id) {
                    Some(cell) => {
                        if let Ok(mut guard) = cell.lock() {
                            guard.resume_call(&call);
                            if let Some(index) = tool_export_indices.get(&call_item.call_id)
                                && let Some(ExportEntry::ToolCall {
                                    tool_name,
                                    arguments,
                                    ..
                                }) = app.export_entries.get_mut(*index)
                            {
                                *tool_name = call_item.tool_name.clone();
                                *arguments = call_item.arguments.clone();
                            }
                        }
                    }
                    None => {
                        let cell = Arc::new(Mutex::new(ToolCell::started(&call)));
                        tool_cells.insert(call_item.call_id.clone(), Arc::clone(&cell));
                        app.append_tool_cell(call_item.tool_name.as_str(), Arc::clone(&cell));
                        app.export_entries.push(export_tool_call_entry(
                            &call_item.call_id,
                            &call_item.tool_name,
                            call_item.arguments.clone(),
                        ));
                        tool_export_order.push(call_item.call_id.clone());
                        tool_export_indices.insert(
                            call_item.call_id.clone(),
                            app.export_entries.len().saturating_sub(1),
                        );
                    }
                }
            }
            TurnItem::ToolResult(result_item) => {
                let cell = match tool_cells.get(&result_item.call_id) {
                    Some(cell) => Arc::clone(cell),
                    None => {
                        let call = ToolCall {
                            id: result_item.call_id.clone(),
                            name: ToolName::new("tool.result"),
                            arguments: serde_json::json!({
                                "call_id": result_item.call_id.to_string(),
                                "orphaned": true,
                            }),
                        };
                        let cell = Arc::new(Mutex::new(ToolCell::started(&call)));
                        tool_cells.insert(result_item.call_id.clone(), Arc::clone(&cell));
                        app.append_tool_cell("tool.result", Arc::clone(&cell));
                        app.export_entries.push(export_tool_call_entry(
                            &result_item.call_id,
                            "tool.result",
                            serde_json::json!({
                                "call_id": result_item.call_id.to_string(),
                                "orphaned": true,
                            }),
                        ));
                        tool_export_order.push(result_item.call_id.clone());
                        tool_export_indices.insert(
                            result_item.call_id.clone(),
                            app.export_entries.len().saturating_sub(1),
                        );
                        cell
                    }
                };
                if let Ok(mut guard) = cell.lock() {
                    guard.complete(&result_item.result, result_item.duration_ms);
                    // A6: die Dauer gehört zum `ExportEntry::ToolResult`
                    // (unten, `export_tool_result_entry`), nicht zum
                    // `ToolCall`-Eintrag — kein Backfill von `duration_ms`
                    // hier mehr.
                    if let Some(index) = tool_export_indices.get(&result_item.call_id)
                        && let Some(ExportEntry::ToolCall { trust, .. }) =
                            app.export_entries.get_mut(*index)
                    {
                        *trust = Some(export_trust(result_item.trust));
                    }
                    let tool_name = match tool_export_indices
                        .get(&result_item.call_id)
                        .and_then(|index| app.export_entries.get(*index))
                    {
                        Some(ExportEntry::ToolCall { tool_name, .. }) => Some(tool_name.clone()),
                        _ => None,
                    };
                    app.export_entries.push(export_tool_result_entry(
                        &result_item.call_id,
                        tool_name,
                        &result_item.result,
                        result_item.duration_ms,
                        Some(result_item.trust),
                    ));
                }
            }
            TurnItem::Reasoning(reasoning) => {
                let summary = reasoning.summary_text.join(" ");
                if !summary.trim().is_empty() {
                    app.export_entries
                        .push(ExportEntry::Reasoning(summary.clone()));
                    app.push_reasoning_cell(summary, None);
                }
            }
            TurnItem::Error(error) => {
                app.push_line(Role::System, format!("⚠ {}", error.message));
                app.export_entries
                    .push(ExportEntry::Error(ExportErrorEntry {
                        code: Some(if error.retryable {
                            "retryable".to_owned()
                        } else {
                            "error".to_owned()
                        }),
                        message: error.message.clone(),
                        details: Some(serde_json::json!({ "retryable": error.retryable })),
                        agent: None,
                    }));
            }
        }
    }

    // Persistierte Aufrufe ohne Result bleiben sichtbar, aber werden nicht als
    // erfolgreich ausgegeben. Das ist insbesondere bei abgebrochenen Turns
    // nach einem Prozess- oder Agentenabbruch wichtig.
    for call_id in &tool_export_order {
        let Some(cell) = tool_cells.get(call_id) else {
            continue;
        };
        if let Ok(mut guard) = cell.lock() {
            if guard.state == ToolState::Running {
                guard.mark_incomplete();
                if let Some(index) = tool_export_indices.get(call_id)
                    && let Some(ExportEntry::ToolCall { tool_name, .. }) =
                        app.export_entries.get(*index)
                {
                    app.export_entries.push(export_tool_result_entry(
                        call_id,
                        Some(tool_name.clone()),
                        &ToolCallResult::error("unvollständig (Resume-Abbruch)"),
                        0,
                        Some(ResultTrust::Runtime),
                    ));
                }
            }
        }
    }
}

fn incident_hint(error: &TuiError, provider_error_streak: u32) -> Option<&'static str> {
    let TuiError::Core(message) = error else {
        return None;
    };
    let message = message.to_ascii_lowercase();
    let provider_error = [
        "provider",
        "model",
        "rate limit",
        "429",
        "timeout",
        "timed out",
        "connection",
        "http",
    ]
    .iter()
    .any(|needle| message.contains(needle));
    if provider_error && provider_error_streak >= 2 {
        return Some(
            "Hinweis: Wiederholter Providerfehler. Mit /bug-report kannst du einen Incident melden.",
        );
    }
    if ["panic", "panicked", "thread '"]
        .iter()
        .any(|needle| message.contains(needle))
    {
        return Some(
            "Hinweis: Panikhinweis erkannt. Mit /bug-report kannst du einen Incident melden.",
        );
    }
    if ["agent", "child"]
        .iter()
        .any(|needle| message.contains(needle))
        && ["killed", "terminated", "aborted", "cancelled", "canceled"]
            .iter()
            .any(|needle| message.contains(needle))
    {
        return Some(
            "Hinweis: Ein Agent wurde beendet. Mit /bug-report kannst du einen Incident melden.",
        );
    }
    if ["lock contention", "contention", "deadlock", "lock poisoned"]
        .iter()
        .any(|needle| message.contains(needle))
    {
        return Some(
            "Hinweis: Lock-Contention erkannt. Mit /bug-report kannst du einen Incident melden.",
        );
    }
    None
}

pub(crate) fn install_loaded_history(
    session: &mut AgentSession,
    app: &mut ChatApp,
    history: ConversationHistory,
) {
    hydrate_visible_history(app, &history);
    apply_session_store_started_at(app);
    *session.history_mut() = history;
}

/// Ersetzt [`ChatApp::export_started_at`] durch den tatsächlichen
/// Sitzungsstart aus dem Session-Store-Sidecar, sofern dessen Wurzel
/// bekannt ist (A1: „bei fortgesetzter Session kommt das Datum vom
/// Session-Start, nicht vom TUI-Start"; Aufgabe 2: für **jede** Session,
/// nicht nur bei aktiver Titelerzeugung).
///
/// # Beschreibung
/// Nutzt zuerst [`ChatApp::session_store_root`] — von `runtime_root.rs`
/// immer gesetzt, wenn `/resume` in diesem Lauf konfiguriert ist,
/// unabhängig von `[session] title_generation` (siehe
/// [`ChatApp::with_session_store_root`]). Ist dieses Feld `None` (ältere
/// Composition-Root-Pfade oder Tests, die nur den Titel-Job-Kontext
/// setzen), fällt die Funktion auf
/// [`ChatApp::title_job_context`]s `session_store_root`
/// (`runtime_root.rs::TitleJobContext`) zurück. Ist auch das unbekannt
/// (`/resume` in diesem Lauf gar nicht konfiguriert), bleibt
/// [`ChatApp::export_started_at`] unverändert beim TUI-Startzeitpunkt
/// (siehe [`export_timestamp_now`]). Ein fehlender oder unlesbarer Sidecar
/// wird nur geloggt, nie propagiert — dieselbe Best-Effort-Haltung wie
/// [`harw_session_store::meta::peek`] selbst, das bei fehlendem Sidecar aus
/// dem Transcript ableitet statt zu scheitern.
///
/// Liest bewusst über [`harw_session_store::meta::peek`], nicht über
/// [`harw_session_store::meta::load_or_derive`]: diese Funktion läuft bei
/// **jedem** TUI-Start (`install_loaded_history`), also auch für eine ganz
/// neue Session, deren Transcript hier noch leer ist. `load_or_derive` würde
/// in diesem Fall sofort einen Sidecar mit `first_user_message: None`
/// speichern und ihn damit für den Rest der Session einfrieren — der
/// Resume-Picker (und der Titel-Job in
/// `harw-runtime/src/session_title.rs::ensure_title`) sähen danach nie mehr
/// die tatsächliche erste Nutzernachricht, selbst nachdem sie im Transcript
/// eingetroffen ist. `peek` liefert denselben Anzeigewert (identisches
/// `created_at`), schreibt aber nie.
///
/// # Argumente
/// - `app` (`&mut ChatApp`): liefert Session-ID, Session-Store-Wurzel und
///   Titel-Job-Kontext, nimmt das aufgelöste `started_at` auf.
fn apply_session_store_started_at(app: &mut ChatApp) {
    let Some(store_root) = app.session_store_root.clone().or_else(|| {
        app.title_job_context
            .as_ref()
            .map(|ctx| ctx.session_store_root.clone())
    }) else {
        return;
    };
    match harw_session_store::meta::peek(&store_root, app.session_id()) {
        Ok(meta) => {
            app.export_started_at = Some(meta.created_at.as_second().to_string());
        }
        Err(error) => {
            tracing::warn!(
                session = %app.session_id(),
                error = %error,
                "tui.export.session_store_started_at_failed"
            );
        }
    }
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

/// Höchstlänge des Modell-Segments in der Statuszeile (Zeichen).
const STATUS_MODEL_MAX_CHARS: usize = 32;

/// „Scharfgestellter" Beenden-Zustand: welches Label + wann gedrückt.
///
/// Ein zweiter Druck derselben Taste innerhalb von [`QUIT_HINT_WINDOW`] beendet;
/// jede andere Taste (und der Timeout) macht die Scharfstellung rückgängig.
#[derive(Clone, Copy)]
pub(crate) struct QuitArm {
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
/// - `host_permit_prompts` (`&mut HostPermitPromptReceiver`): Fragekanal
///   dieser Wurzelsitzung (siehe
///   [`harw_runtime::RuntimeAssembly::take_host_permit_prompts`]).
///   Ebenfalls in [`drive_turn_animated`] gepollt, aus demselben Grund.
///   Runde 5, Teil K: zusätzlich im Leerlauf
///   ([`background_agents::poll_idle_prompts`]), damit Fragen von
///   Hintergrund-Agenten nicht in den Zeitablauf laufen.
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
    host_permit_prompts: &mut HostPermitPromptReceiver,
) -> Result<TuiRunOutcome, TuiError> {
    // Seitenkanäle der Turn-Ereignisverarbeitung: Werkzeugnamen-Korrelation und
    // die eine Verlaufszelle je Kind-Session.
    let mut turn_state = TurnEventState::default();

    let mut spinner = Spinner::new();
    let mut provider_error_streak = 0_u32;
    // Ein fertiger Busy-Auftrag weckt die Schleife über einen Frame, auch
    // wenn sein Turn schon vorbei ist (Runde 4, Teil H).
    app.busy_jobs.set_waker(frame_req.clone());
    // Runde 5, Teil L: eine fertige `/btw`-Nebenfrage weckt die Schleife.
    app.btw.set_waker(frame_req.clone());

    loop {
        // Runde 5, Teil B: außerhalb eines Turns darf kein sudo-Fenster offen
        // stehen (hier landen auch alle frühen Rückkehrwege eines Turns) —
        // sonst ginge Getipptes an den Composer, während das Fenster noch
        // sichtbar ist. Ablehnung ist der Default.
        // Runde 5, Teil K: Ausnahme, solange ein Hintergrund-Agent läuft —
        // dessen sudo-Frage wird im Leerlauf beantwortet (Tasten gehen dann
        // an das Fenster, siehe `background_agents::route_idle_prompt_event`).
        if !background_agents::keeps_idle_prompts(app) {
            crate::sudo_dialog::deny_open(app);
        }
        // Runde 5, Teil F: ebenso kein Plan-Fenster (Schließen = keine
        // Entscheidung) — und ein vorgemerktes `/plan edit` öffnet jetzt, im
        // Leerlauf, den externen Editor.
        plan_mode::close_open(app);
        if plan_mode::run_pending_editor(guard, app) {
            frame_req.schedule_frame();
        }
        // Ergebnisse von Befehlen/Abrufen, die während des letzten Turns
        // gestartet wurden und erst danach fertig wurden.
        let mut busy_results = false;
        while let Some(done) = app.busy_jobs.try_recv() {
            apply_busy_job_done(app, done);
            busy_results = true;
        }
        // Runde 5, Teil L: Abschluss einer `/btw`-Nebenfrage (Fehler als
        // Systemzeile).
        if app.poll_btw() {
            busy_results = true;
        }
        if busy_results {
            frame_req.schedule_frame();
        }
        // Slash-Zeilen aus synchronen Pfaden (Werkbank-Tasten) laufen über
        // denselben Command-Kanal wie getippte Befehle.
        while let Some(command) = app.pending_commands.pop_front() {
            harw_tx.send(HarwEvent::Command(command));
        }
        // Runde 5, Teil C: Kanban live, gedrosselt auf 2 s — auch rege
        // Eingaben schieben die Abfrage so nicht beliebig auf.
        app.poll_kanban_live();
        // Ausstehende Datenabrufe für Ansichten und Werkbank (nie in den Chat).
        if process_pending_fetches(app).await {
            frame_req.schedule_frame();
        }

        // Runde 5, Teil K: Freigabe-Fragen von Hintergrund-Agenten auch im
        // Leerlauf zeigen; fertige Hintergrund-Agenten melden.
        if background_agents::poll_idle_prompts(app, host_permit_prompts) {
            frame_req.schedule_frame();
        }
        // Runde 5, Teil O: Freigabe-Fragen von Kind-Agenten (Hintergrund)
        // im selben Freigabedialog, im selben Leerlauf-Takt.
        if child_approvals::poll(app) {
            frame_req.schedule_frame();
        }
        if background_agents::collect_finished(app) {
            frame_req.schedule_frame();
        }
        // Runde 5, Teil M: Meldungen/Fragen der Kinder an die UIA (Zeile im
        // Verlauf, Kontext für den nächsten Turn, Auto-Turn im Leerlauf).
        if agent_messages::collect_parent_messages(app) {
            frame_req.schedule_frame();
        }

        // Eine während des vorherigen Turns abgeschickte Eingabe hat Vorrang.
        // Sie passiert denselben Pfad wie eine frische `HarwEvent::Submit`.
        let mut submitted: Option<String> = app.pending_turns.pop_front();
        // Runde 5, Teil K: im Leerlauf startet eine Hintergrund-Meldung
        // selbst einen UIA-Turn.
        if submitted.is_none() {
            submitted = background_agents::take_auto_turn(app);
        }

        if submitted.is_none() {
            // Runde 5, Teil K: Wecker für Hintergrund-Meldungen und ein
            // Leerlauf-Takt für deren Freigabe-Fragen.
            let background_wake = background_agents::waker(app);
            let background_tick = background_agents::has_running(app);
            // Runde 5, Teil C: Leerlauf-Takt nur bei offenem Kanban-Board.
            let kanban_tick = app.kanban_live_active();
            // Runde 5, Teil I: Leerlauf-Takt bei offener Agentenbaum-Ansicht.
            let agent_tree_tick = app.agent_tree_live_active();
            tokio::select! {
                maybe_tev = async { match app.deferred_input.pop_front() { Some(event) => Some(event), None => tui_rx.recv().await } } => {
                    let Some(tev) = maybe_tev else { return Ok(TuiRunOutcome::Quit) };

                    // Abgelaufenen Beenden-Hinweis verwerfen.
                    if let Some(arm) = app.pending_quit {
                        if arm.at.elapsed() > QUIT_HINT_WINDOW {
                            app.pending_quit = None;
                        }
                    }
                    // Runde 5, Teil K: ein im Leerlauf offenes sudo- oder
                    // Host-Permit-Fenster (Hintergrund-Agent) bekommt die Eingabe.
                    let tev = match background_agents::route_idle_prompt_event(app, tev) {
                        Ok(redraw) => {
                            if redraw {
                                frame_req.schedule_frame();
                            }
                            continue;
                        }
                        Err(tev) => tev,
                    };
                    // Runde 5, Teil O: eine offene Kind-Freigabe bekommt die Eingabe.
                    let tev = match child_approvals::route_event(app, tev) {
                        Ok(redraw) => {
                            if redraw {
                                frame_req.schedule_frame();
                            }
                            continue;
                        }
                        Err(tev) => tev,
                    };

                    match tev {
                        TuiEvent::Draw => {
                            draw_viewport(guard, app, &spinner, app.pending_quit.map(|arm| arm.label))?;
                        }
                        TuiEvent::Key(key) => {
                            if handle_key(app, key, harw_tx) {
                                frame_req.schedule_frame();
                            }
                            // Register „CSI-Sicherheitsnetz und Paste-
                            // Platzhalter", Punkt 7: Ctrl+H kann soeben eine
                            // Host-Arbeitsphase beendet haben (siehe
                            // `ChatApp::end_host_mode`) — Terminal-Modi
                            // best-effort reassertieren.
                            if app.take_needs_terminal_reassert() {
                                guard.reassert_terminal_modes();
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
                            // Große Pastes landen nur als kompakter
                            // Platzhalter im Composer (siehe
                            // `InputEditor::insert_paste`-Doku).
                            app.input.insert_paste(&normalized);
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
                        // Runde 5, Teil K: bei laufenden Hintergrund-Agenten
                        // fragt `/quit` einmal nach.
                        HarwEvent::Quit => {
                            if background_agents::confirm_quit(app) {
                                return Ok(TuiRunOutcome::Quit);
                            }
                            frame_req.schedule_frame();
                        }
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
                            // Eine offene Ansicht lädt nach jedem Befehl (z. B.
                            // ihrem eigenen `Run`) neu; die Generationsprüfung
                            // verwirft das Ergebnis, falls der Befehl die
                            // Ansicht ersetzt.
                            app.queue_overlay_refresh();
                            if is_workbench_command(&raw) {
                                app.workbench.mark_stale();
                            }
                            // TUI-lokale Befehle und Präfixe (`/model`, `/mode`,
                            // `/help`, `#notiz`, `@rolle` …) — vor dem regulären
                            // `/command`-Dispatch abgefangen. `/tools` und
                            // `/compact` bleiben darunter unverändert.
                            // Runde 5, Teil L: `/btw` im Leerlauf antwortet auf
                            // den aktuellen Gesprächsstand.
                            if btw::is_btw_line(&raw) {
                                app.capture_btw_snapshot(gateway.session_mut(), None);
                            }
                            if let Some(intercepted) = local_intercept_for(app, &raw) {
                                if let Some(outcome) =
                                    apply_local_intercept(app, intercepted, harw_tx)
                                {
                                    return Ok(outcome);
                                }
                                frame_req.schedule_frame();
                                continue;
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
                                //
                                // Plan Teil F: gesetzt, wenn `raw` tatsächlich einen
                                // `!`/`!!`-Shell-Befehl ausgeführt hat — löst nach dem
                                // Anzeigen der Ergebniszelle unten den automatischen
                                // Folge-Turn aus.
                                let mut shell_result: Option<ShellRunOutcome> = None;
                                let (output, output_data) = if raw.trim() == "/compact" {
                                    let (session, store, model) = gateway.borrow_turn_ctx();
                                    let context_window_tokens = session
                                        .auto_compact()
                                        .map(|policy| policy.context_window_tokens())
                                        .unwrap_or(200_000);
                                    let mut plan = CompactionPlan::for_context_window(context_window_tokens);
                                    let (summary_provider, summary_model) =
                                        session.compaction_summary_model();
                                    plan.summary_provider = summary_provider.cloned();
                                    plan.summary_model = summary_model.cloned();
                                    let output = match compact_session(session, model, &plan, None).await {
                                        Ok(outcome) => {
                                            if let Err(error) =
                                                store.save_history(session.id(), session.history()).await
                                            {
                                                tracing::warn!(
                                                    %error,
                                                    "compact: Verlauf verdichtet, aber Persistenz fehlgeschlagen"
                                                );
                                            }
                                            let summarized_suffix = if outcome.summarized {
                                                ", zusammengefasst"
                                            } else {
                                                ""
                                            };
                                            format!(
                                                "Kontext verdichtet: {} → {} Bytes ({} entfernt, {} Duplikate, {} gekürzt{summarized_suffix})",
                                                outcome.bytes_before,
                                                outcome.bytes_after,
                                                outcome.items_dropped,
                                                outcome.calls_deduplicated,
                                                outcome.results_truncated,
                                            )
                                        }
                                        Err(error) => format!("Verdichtung fehlgeschlagen: {error}"),
                                    };
                                    (output, None)
                                } else {
                                // `/command`-Zeile asynchron über die Operation-Adapter-
                                // Pipeline ausführen; identischer Render-/Redraw-Pfad wie
                                // bei `SystemMessage` (mehrzeilige Ausgaben an `\n`
                                // aufteilen). Berechtigungsstufe und Slash-Dienste
                                // stammen aus der Runtime-Montage; die Dienste werden
                                // erst nach erfolgreicher Admission gebaut.
                                    match app.runtime() {
                                        Some(rt) => {
                                            let caller_tier = runtime_commands::caller_tier(rt.principal());
                                            let export_output = if export_request_for_command(&raw).is_some() {
                                                Some(
                                                    command_data::execute_command_with_data(
                                                        app.adapters(),
                                                        app.sandbox(),
                                                        app.session_id(),
                                                        caller_tier,
                                                        &raw,
                                                        || runtime_commands::slash_service_map(rt.services()),
                                                    )
                                                    .await,
                                                )
                                            } else {
                                                None
                                            };
                                            match export_output {
                                                Some(Ok(output)) => (output.text, output.data),
                                                Some(Err(error)) => (error, None),
                                                None => {
                                                    // Plan Teil F: statt der reinen
                                                    // `execute_command_as`-Textausgabe
                                                    // liefert dieser Dispatch zusätzlich
                                                    // ein strukturiertes Shell-Ergebnis,
                                                    // wenn `raw` ein `!`/`!!`-Befehl war
                                                    // (`!!` löst hier — anders als in
                                                    // `execute_command_as` — echt gegen
                                                    // `app.last_shell_command` auf).
                                                    let outcome = dispatch_command_with_shell_result(
                                                        app.adapters(),
                                                        app.sandbox(),
                                                        app.session_id(),
                                                        caller_tier,
                                                        &raw,
                                                        app.last_shell_command.as_deref(),
                                                        || runtime_commands::slash_service_map(
                                                            rt.services(),
                                                        ),
                                                    )
                                                    .await;
                                                    shell_result = outcome.shell;
                                                    (outcome.text, None)
                                                }
                                            }
                                        }
                                        None => {
                                            tracing::error!("tui.command.no_runtime_assembly");
                                            ("Fehler: keine Runtime-Montage".to_owned(), None)
                                        }
                                    }
                                };
                                let lines: Vec<Line<'static>> = output
                                    .split('\n')
                                    .map(|line| Line::from(line.to_owned()))
                                    .collect();
                                app.push_lines(lines);
                                // Register „CSI-Sicherheitsnetz und Paste-
                                // Platzhalter", Punkt 7: ein tatsächlich
                                // gelaufener `!`/`!!`-Host-Befehl kann
                                // Terminal-Modi (Raw-Mode/Bracketed-Paste/
                                // Maus-Capture) beschädigt zurückgelassen
                                // haben — best-effort reassertieren, bevor
                                // ihr Ergebnis weiterverarbeitet wird.
                                if shell_result.is_some() {
                                    guard.reassert_terminal_modes();
                                }
                                // Plan Teil F: nach einem tatsächlich gelaufenen
                                // `!`/`!!`-Befehl sofort einen Folge-Turn einreihen —
                                // frei über `submitted` (identischer Pfad wie
                                // `HarwEvent::Submit`), belegt über
                                // `app.pending_turns`. Kein `shell_result` (Admission-
                                // Fehler, `!!` ohne Vorgänger, nicht-Shell-Command) →
                                // kein Turn.
                                if let Some(shell) = shell_result {
                                    queue_shell_follow_up_turn(app, &mut submitted, shell);
                                }
                                // AP W5-05: Eine `/command`-Zeile läuft **zwischen**
                                // Turns. Das ist eine gültige Turn-Grenze, also darf
                                // ein soeben angefordertes `/mode` sofort wirken —
                                // sonst zeigte die Statuszeile bis zur nächsten
                                // Nachricht weiter den alten Modus.
                                if app.apply_pending_controller_state(gateway.session_mut()) {
                                    persist_gateway_state(gateway).await?;
                                }
                                // AP W5-10b: Zielstand nach `/goal check` sichtbar
                                // machen, sofern die Composition-Root Plan-/Ziel-
                                // Dienste durchgereicht hat.
                                if let Some(cell) = goal_cell_for_command(app, &raw) {
                                    app.push_cell(Box::new(cell));
                                }
                                // `/export`: bei `--datei <pfad>` direkt schreiben,
                                // sonst die Zielauswahl öffnen (Slice E1).
                                if let Some(request) = output_data
                                    .as_ref()
                                    .and_then(export_request_from_data)
                                    .or_else(|| export_request_for_command(&raw))
                                {
                                    resolve_export_request(app, &request);
                                }
                                frame_req.schedule_frame();
                            }
                        }
                    }
                }
                maybe_sev = event_rx.recv() => {
                    match maybe_sev {
                        Some(SessionEvent::SessionConfigured { model, .. }) => {
                            app.set_export_session_model(model);
                        }
                        Some(SessionEvent::TurnCompleted { usage, .. }) => {
                        app.total_usage.add(&usage);
                        // Plan Schritt 7: einmalige Titel-Job-Anstoßung nach dem
                        // ersten abgeschlossenen Turn der Sitzung. Der Kontext wird
                        // dabei verbraucht (`Option::take`) — höchstens ein Versuch
                        // je Sitzung, unabhängig davon, ob ein Modell auflösbar war
                        // (siehe `TitleJobContext`-Doku in `runtime_root.rs`).
                        if let Some(ctx) = app.title_job_context.take() {
                            let pin = harw_runtime::session_title::title_model_selection(&ctx.config);
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
                                    pin,
                                ),
                                None => tracing::debug!(
                                    "tui.session_title.no_model_available_skipping_job"
                                ),
                            }
                        }
                        frame_req.schedule_frame();
                        }
                        // Andere SessionEvent-Varianten und ein geschlossener Kanal
                        // bleiben für den Renderer ohne sichtbare Auswirkung.
                        _ => {}
                    }
                }
                maybe_tev = turn_event_rx.recv() => {
                    if let Some(tev) = maybe_tev {
                        if handle_turn_event(app, &mut turn_state, tev) {
                            frame_req.schedule_frame();
                        }
                        // Register „CSI-Sicherheitsnetz und Paste-
                        // Platzhalter", Punkt 7: ein abgeschlossener
                        // `shell.exec`-Aufruf kann Terminal-Modi beschädigt
                        // zurückgelassen haben (siehe `handle_turn_event`).
                        if app.take_needs_terminal_reassert() {
                            guard.reassert_terminal_modes();
                        }
                    }
                }
                // Runde 5, Teil C: Kanban live im Leerlauf — prüft die
                // Board-Dateien (und leert dabei den Agenten-Bus); ein
                // eingereihtes Nachladen läuft oben über
                // `process_pending_fetches`.
                _ = tokio::time::sleep(kanban_live::KANBAN_LIVE_INTERVAL), if kanban_tick => {
                    if app.drain_agent_events() {
                        frame_req.schedule_frame();
                    }
                }
                // Runde 5, Teil I: offene `/agent`-Ansicht zeichnet gedrosselt
                // neu (Live-Werte, laufende Dauer), auch ohne Bus-Ereignis.
                _ = tokio::time::sleep(crate::agent_tree_live::AGENT_TREE_LIVE_INTERVAL), if agent_tree_tick => {
                    app.drain_agent_events();
                    frame_req.schedule_frame();
                }
                // Runde 5, Teil K: ein Hintergrund-Agent ist fertig — der
                // Schleifenanfang holt die Meldung ab und startet ggf. den
                // Auto-Turn.
                _ = background_agents::wait_for_notice(background_wake) => {
                    frame_req.schedule_frame();
                }
                // Runde 5, Teil K: Leerlauf-Takt, solange Hintergrund-Agenten
                // laufen — holt deren sudo-/Host-Permit-Fragen ab (oben,
                // `poll_idle_prompts`) und zeigt ihren Fortschritt.
                _ = tokio::time::sleep(background_agents::IDLE_POLL_INTERVAL), if background_tick => {
                    app.drain_agent_events();
                    frame_req.schedule_frame();
                }
            }
        }

        // ── Turn-Pfad, außerhalb des `select!` ───────────────────────────────
        let Some(text) = submitted else {
            continue;
        };

        // Nutzerzelle in die interne History. Plan Teil F: eine kompakte
        // Anzeige-Überschreibung (gesetzt von `queue_shell_follow_up_turn`)
        // ersetzt einmalig nur die ANGEZEIGTE Nutzerzelle — das Modell
        // bekommt weiterhin `text` in voller Länge (siehe `run_turn_streaming`
        // unten). Ohne Überschreibung (jede normal getippte Nachricht):
        // unverändertes Verhalten.
        let display_override = app.pending_turn_user_cell_override.take();
        // `@pfad`-Anhänge nur für getippte Nachrichten expandieren — ein
        // automatischer `!`-Folge-Turn (mit Anzeige-Überschreibung) trägt
        // fremde Ausgabe, deren `@` keine Erwähnung ist.
        let (turn_text, attachment_note) = if display_override.is_none() && text.contains('@') {
            expand_mentions_for_turn(&app.project_root, &text)
        } else {
            (text.clone(), None)
        };
        // Runde 5, Teil K: wartende Hintergrund-Meldungen gehen dem Turn als
        // Kontext voran (auch einem normal getippten).
        let turn_text = background_agents::attach_queued_notices(app, turn_text);
        let displayed_text = display_override.unwrap_or_else(|| text.clone());
        app.push_line(Role::User, displayed_text);
        if let Some(note) = attachment_note {
            app.push_lines(
                note.split('\n')
                    .map(|line| Line::from(line.to_owned()))
                    .collect(),
            );
        }
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
        let turn_result = run_turn_streaming(
            guard,
            app,
            &mut spinner,
            gateway,
            &turn_text,
            approval_driver,
            approvals,
            host_permit_prompts,
            tui_rx,
            turn_event_rx,
            &mut turn_state,
        )
        .await;
        // Werkzeuge des Turns können die Werkbank verändert haben.
        app.workbench.mark_stale();

        // Welle 4c: ein zweiter Ctrl+C während des soeben beendeten Turns hat
        // `app.hard_quit_requested` gesetzt (siehe `handle_busy_event`) — der
        // Loop beendet direkt hier, unabhängig davon, ob der Turn selbst
        // erfolgreich war oder mit einem Fehler zurückkam (derselbe Ausgang
        // wie `HarwEvent::Quit` im Idle-Pfad oben).
        if app.hard_quit_requested {
            return Ok(TuiRunOutcome::Quit);
        }

        if let Err(error) = turn_result {
            let provider_error = matches!(
                &error,
                TuiError::Core(message)
                    if ["provider", "model", "rate limit", "429", "timeout", "timed out", "connection", "http"]
                        .iter()
                        .any(|needle| message.to_ascii_lowercase().contains(needle))
            );
            match &error {
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
                    provider_error_streak = if provider_error {
                        provider_error_streak.saturating_add(1)
                    } else {
                        0
                    };
                    if let Some(hint) = incident_hint(&error, provider_error_streak) {
                        app.push_line(Role::System, hint);
                    }
                }
            }
        } else {
            provider_error_streak = 0;
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
/// - `ItemAdded(AssistantMessage)` → nur bei `phase == Some(Commentary)` eine
///   sichtbare Assistant-Zeile über [`ChatApp::push_line`] (Live-Anzeige des
///   kurzen Ack-Texts vor Tool-Aufrufen); bei `FinalAnswer` oder fehlender
///   Phase bleibt die Anzeige `reveal_reply` am Turn-Ende vorbehalten, sonst
///   entstünde eine Doppelanzeige. Der live gepushte Text wird zusätzlich in
///   [`TurnEventState::last_commentary_text`] festgehalten — `reveal_reply`s
///   Aufrufer ([`drive_turn_animated`]) gleicht seine Turn-Antwort dagegen ab
///   ([`suppress_if_matches_commentary`]), als zweite Verteidigungslinie
///   gegen eine doppelte Anzeige derselben Nachricht,
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
/// Obergrenze (Zeichen) des vorgehaltenen Live-Reasoning-Schwanzes.
const LIVE_REASONING_MAX_CHARS: usize = 2000;
/// Maximale Zahl umbrochener Live-Reasoning-Zeilen im Chat.
const LIVE_REASONING_MAX_LINES: usize = 3;

/// Baut die transienten Live-Reasoning-Zeilen: die letzten (bis zu)
/// [`LIVE_REASONING_MAX_LINES`] umbrochenen Zeilen, gedimmt, die erste mit
/// `"∴ "`-Markierung.
///
/// # Argumente
/// - `text` (`&str`): Roher, gestreamter Reasoning-Text (wird sanitisiert).
/// - `width` (`u16`): Breite der Verlaufsfläche.
/// - `theme` ([`style::Theme`]): Farbschema.
fn live_reasoning_lines(text: &str, width: u16, theme: style::Theme) -> Vec<Line<'static>> {
    let sanitized = crate::sanitize::sanitize_inline(text);
    let wrapped = crate::history_cell::wrap_plain(sanitized.trim(), width.saturating_sub(4));
    let skip = wrapped.len().saturating_sub(LIVE_REASONING_MAX_LINES);
    wrapped
        .into_iter()
        .skip(skip)
        .enumerate()
        .map(|(index, line)| {
            let content: String = line
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect();
            let prefix = if index == 0 { "  ∴ " } else { "    " };
            Line::styled(format!("{prefix}{content}"), style::dim_style(theme))
        })
        .collect()
}

fn handle_turn_event(app: &mut ChatApp, state: &mut TurnEventState, event: TurnEvent) -> bool {
    match event {
        TurnEvent::ToolCallRequested {
            turn_id,
            call_id,
            tool_name,
            arguments,
            ..
        } => {
            app.clear_live_stream();
            // Runde 5, Teil O: Turn des Aufrufs für die Abbruch-Markierung.
            state.tool_call_turns.insert(call_id.clone(), turn_id);
            let already_known = state.pending_tool_cells.contains_key(&call_id);
            let call = ToolCall {
                id: call_id.clone(),
                name: ToolName::new(tool_name.clone()),
                arguments: arguments.clone(),
            };
            ensure_tool_cell(app, state, call_id.clone(), &call);
            if let Some(index) = state.export_tool_calls.get(&call_id).copied() {
                if let Some(ExportEntry::ToolCall {
                    tool_name: exported_name,
                    arguments: exported_arguments,
                    ..
                }) = app.export_entries.get_mut(index)
                {
                    *exported_name = tool_name;
                    *exported_arguments = arguments;
                }
            } else {
                app.export_entries.push(export_tool_call_entry(
                    &call_id,
                    call.name.as_str(),
                    call.arguments.clone(),
                ));
                state
                    .export_tool_calls
                    .insert(call_id.clone(), app.export_entries.len().saturating_sub(1));
            }
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
            let cell = match state.pending_tool_cells.get(&call_id).cloned() {
                Some(cell) => cell,
                None => {
                    tracing::warn!(call_id = %call_id, "tui.tool_cell.completed_without_request");
                    let orphan = ToolCall {
                        id: call_id.clone(),
                        name: ToolName::new("tool.result"),
                        arguments: serde_json::json!({
                            "call_id": call_id.to_string(),
                            "orphaned": true,
                        }),
                    };
                    let cell = Arc::new(Mutex::new(ToolCell::started(&orphan)));
                    state
                        .pending_tool_cells
                        .insert(call_id.clone(), Arc::clone(&cell));
                    app.append_tool_cell("tool.result", Arc::clone(&cell));
                    cell
                }
            };
            let tool_name = state
                .export_tool_calls
                .get(&call_id)
                .and_then(|index| app.export_entries.get(*index))
                .and_then(|entry| match entry {
                    ExportEntry::ToolCall { tool_name, .. } => Some(tool_name.clone()),
                    _ => None,
                });
            // Register „CSI-Sicherheitsnetz und Paste-Platzhalter", Punkt 7:
            // ein abgeschlossener `shell.exec`-Aufruf kann Terminal-Modi
            // (Raw-Mode/Bracketed-Paste/Maus-Capture) beschädigt
            // zurücklassen — der Aufrufer (mit Zugriff auf den
            // `TerminalGuard`) reasserted sie best-effort, sobald diese
            // Vormerkung ansteht (siehe `ChatApp::take_needs_terminal_reassert`).
            if tool_name.as_deref() == Some("shell.exec") {
                app.request_terminal_reassert();
            }
            if !state.export_tool_calls.contains_key(&call_id) {
                app.export_entries.push(export_tool_call_entry(
                    &call_id,
                    tool_name.as_deref().unwrap_or("tool.result"),
                    serde_json::json!({ "call_id": call_id.to_string(), "orphaned": true }),
                ));
                state
                    .export_tool_calls
                    .insert(call_id.clone(), app.export_entries.len().saturating_sub(1));
            }
            // A6: die Dauer gehört zum `ExportEntry::ToolResult` unten
            // (`export_tool_result_entry`), nicht zum `ToolCall`-Eintrag —
            // kein Backfill von `duration_ms` hier mehr.
            let export_entry = match cell.lock() {
                Ok(mut guard) => {
                    guard.complete(&result, duration_ms);
                    Some(export_tool_result_entry(
                        &call_id,
                        tool_name,
                        &result,
                        duration_ms,
                        None,
                    ))
                }
                Err(_) => {
                    tracing::error!(call_id = %call_id, "tui.tool_cell.lock_poisoned");
                    None
                }
            };
            if let Some(entry) = export_entry {
                app.export_entries.push(entry);
            }
            // Runde 5, Teil E: „auto ✓ <Grund>“ bzw. „Vom Auto-Modus
            // abgelehnt · <Kategorie>“ an der Zelle; löst der Deckel aus,
            // erscheint sein Hinweis einmal als Systemzeile.
            let auto_notice = app
                .runtime()
                .and_then(|rt| rt.auto_mode())
                .and_then(|auto| {
                    if let Some(note) = auto
                        .log()
                        .verdict_for(call_id.as_str())
                        .as_ref()
                        .and_then(auto_note_for)
                    {
                        if let Ok(mut guard) = cell.lock() {
                            guard.set_approval_note(&note);
                        }
                    }
                    auto.log().take_notice()
                });
            if let Some(notice) = auto_notice {
                app.push_line(Role::System, notice);
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
            app.export_entries
                .push(ExportEntry::Reasoning(summary.clone()));
            app.push_reasoning_cell(summary, None);
            true
        }
        TurnEvent::AssistantDelta { text, .. } => {
            app.live_stream.push_str(&text);
            true
        }
        TurnEvent::ReasoningDelta { text, .. } => {
            app.live_reasoning.push_str(&text);
            // Nur ein Schwanz bleibt vorgehalten — genug für die letzten
            // drei umbrochenen Zeilen auch auf breiten Terminals.
            let count = app.live_reasoning.chars().count();
            if count > LIVE_REASONING_MAX_CHARS {
                app.live_reasoning = app
                    .live_reasoning
                    .chars()
                    .skip(count - LIVE_REASONING_MAX_CHARS)
                    .collect();
            }
            true
        }
        TurnEvent::CompactionApplied {
            reason,
            items_before,
            items_after,
            tokens_before,
            tokens_after,
            summarized,
            elided_results,
            ..
        } => {
            // `turn_event_rx` trägt nur die Ereignisse der Root-Session
            // (Kind-Sessions melden über den Agenten-Bus) — die Momentaufnahme
            // für `/status` und `/usage` bleibt damit root-exklusiv.
            app.session_controller
                .record_compaction(harw_operations::LastCompaction {
                    reason: reason.clone(),
                    tokens_before,
                    tokens_after,
                    summarized,
                    elided_results,
                });
            app.push_line(
                Role::System,
                format!("⟲ Kontext verdichtet ({reason}): {items_before} → {items_after} Einträge"),
            );
            true
        }
        TurnEvent::ContextUpdated {
            used_tokens,
            window_tokens,
            estimated_next_tokens,
            threshold_tokens,
            reserve_tokens,
            ..
        } => {
            // Nur Root-Ereignisse erreichen `turn_event_rx` (siehe oben);
            // die Momentaufnahme speist `/status` und `/usage`.
            app.session_controller.record_context_updated(
                used_tokens,
                window_tokens,
                estimated_next_tokens,
                threshold_tokens,
                reserve_tokens,
            );
            // Die Werte der Statuszeile liest weiterhin der Agenten-Monitor
            // (Bus); hier nur ein Redraw-Anstoß.
            app.drain_agent_events();
            true
        }
        TurnEvent::UsageUpdated { .. } => {
            // Die Werte selbst liest die Statuszeile aus dem Agenten-Monitor
            // (Bus); hier nur ein Redraw-Anstoß.
            app.drain_agent_events();
            true
        }
        TurnEvent::ItemAdded {
            item: TurnItem::AssistantMessage(message),
            ..
        } => {
            // Die Runde ist fertig: der gestreamte Vorschautext wird durch die
            // echte Nachricht ersetzt (Commentary hier, Final via reveal_reply).
            app.clear_live_stream();
            // Nur der kurze Zwischentext vor einem Tool-Aufruf wird hier live
            // gerendert. Die finale Turn-Antwort bleibt exklusiv `reveal_reply`
            // am Turn-Ende vorbehalten (sonst erschiene sie doppelt: einmal
            // hier, einmal beim Abschluss).
            if message.phase != Some(harw_types::MessagePhase::Commentary) {
                return false;
            }
            let text = visible_message_text(&message.content);
            app.push_line(Role::Assistant, text.clone());
            // Zweite Verteidigungslinie gegen eine Doppelanzeige: siehe
            // `TurnEventState::last_commentary_text`-Doku und
            // `drive_turn_animated`.
            state.last_commentary_text = Some(text);
            true
        }
        TurnEvent::ItemAdded {
            item: TurnItem::Error(error),
            ..
        } => {
            app.export_entries
                .push(ExportEntry::Error(ExportErrorEntry {
                    code: Some("item_error".to_owned()),
                    message: error.message.clone(),
                    details: Some(serde_json::json!({ "retryable": error.retryable })),
                    agent: None,
                }));
            app.push_line(Role::System, format!("⚠ {}", error.message));
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
            let parent_id = exported_agent_parent_id(app, &child);
            let cell = Arc::new(Mutex::new(SubAgentCell {
                child_id: child_id.clone(),
                role: role.clone(),
                question: question.clone(),
                tool_calls: 0,
                tokens: 0,
                status: SubAgentStatus::Running,
            }));
            state
                .child_cells
                .insert(child_id.clone(), Arc::clone(&cell));
            app.export_entries
                .push(ExportEntry::Agent(ExportAgentEntry {
                    agent_id: child_id.clone(),
                    role: Some(role.clone()),
                    parent_id,
                    status: Some("running".to_owned()),
                    summary: question.clone(),
                }));
            app.push_shared_cell(cell);
            // Runde 5, Teil I: Live-Block (Orchestrator-Kinder) direkt darunter.
            app.attach_child_stream_block(&child_id, &role, question.as_deref());
            tracing::debug!(child = %child_id, "tui.child_cell.created");
            true
        }
        TurnEvent::ChildProgress {
            child,
            tool_calls,
            tokens,
            ..
        } => {
            let parent_id = exported_agent_parent_id(app, &child);
            let updated = state.update_child(child.as_str(), |cell| {
                cell.apply_progress(tool_calls, tokens);
            });
            if updated {
                app.export_entries
                    .push(ExportEntry::Agent(ExportAgentEntry {
                        agent_id: child.as_str().to_owned(),
                        role: None,
                        parent_id,
                        status: Some("running".to_owned()),
                        summary: Some(format!("{tool_calls} Tool-Aufrufe, {tokens} Tokens")),
                    }));
            }
            updated
        }
        TurnEvent::ChildCompleted {
            child,
            outcome,
            duration_ms,
            ..
        } => {
            let parent_id = exported_agent_parent_id(app, &child);
            let updated = state.update_child(child.as_str(), |cell| {
                cell.apply_completion(outcome.clone(), duration_ms);
            });
            if updated {
                app.export_entries
                    .push(ExportEntry::Agent(ExportAgentEntry {
                        agent_id: child.as_str().to_owned(),
                        role: None,
                        parent_id,
                        status: Some(outcome),
                        summary: Some(format!("Dauer: {duration_ms} ms")),
                    }));
            }
            updated
        }
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
                    // Der Graph wird sichtbar über `push_cell` gezeigt, aber
                    // `/export` liest ausschließlich aus `export_entries` mit —
                    // ohne diesen Push würde der häufigste Fall (Plan-Dienste
                    // vorhanden, Plan lesbar) beim Export komplett fehlen.
                    app.export_entries.push(ExportEntry::Plan(ExportPlanEntry {
                        plan_id: Some(plan_id.clone()),
                        revision: Some(revision),
                        summary: summary.clone(),
                        steps: Vec::new(),
                        status: None,
                        agent: None,
                    }));
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
                    app.export_entries.push(ExportEntry::Plan(ExportPlanEntry {
                        plan_id: Some(plan_id.clone()),
                        revision: Some(revision),
                        summary: format!("{summary} (Plan nicht lesbar: {error})"),
                        steps: Vec::new(),
                        status: None,
                        agent: None,
                    }));
                }
                // Keine Plan-Dienste durchgereicht: die Information geht nicht
                // verloren, sie wird nur einzeilig statt als Graph gezeigt.
                None => {
                    app.push_line(
                        Role::System,
                        format!("Plan {plan_id} @{revision}: {summary}"),
                    );
                    app.export_entries.push(ExportEntry::Plan(ExportPlanEntry {
                        plan_id: Some(plan_id),
                        revision: Some(revision),
                        summary,
                        steps: Vec::new(),
                        status: None,
                        agent: None,
                    }));
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
        // Runde 5, Teil O: nur offene Aufrufe des beendeten Turns; beim
        // Abbruch mit echtem Grund (Nutzerin vs. Turn-Grenze).
        TurnEvent::TurnFailed {
            turn_id, reason, ..
        } => {
            mark_incomplete_tool_exports(app, state, &turn_id, &format!("unvollständig ({reason})"))
        }
        TurnEvent::TurnAborted { turn_id } => {
            let label = turn_safety::aborted_label(app);
            mark_incomplete_tool_exports(app, state, &turn_id, &label)
        }
        // TurnCompleted läuft exklusiv über den SessionEvent-Pfad, damit die
        // Token-Summary nicht doppelt erscheint. TurnStarted trägt keinen
        // zusätzlichen sichtbaren Zustand.
        _ => false,
    }
}

/// Marks outstanding tools as incomplete when a turn terminates early.
///
/// Runde 5, Teil O: nur Aufrufe, die in `turn_id` angefragt wurden (oder deren
/// Turn unbekannt ist, etwa eine vorab gezeigte Freigabefrage). Zellen früherer
/// Turns bleiben unberührt — ein längst beendeter Aufruf wird nie nachträglich
/// als „abgebrochen“ exportiert.
fn mark_incomplete_tool_exports(
    app: &mut ChatApp,
    state: &mut TurnEventState,
    turn_id: &harw_types::TurnId,
    reason: &str,
) -> bool {
    let call_ids: Vec<_> = state
        .pending_tool_cells
        .keys()
        .filter(|call_id| {
            state
                .tool_call_turns
                .get(*call_id)
                .is_none_or(|requested_in| requested_in == turn_id)
        })
        .cloned()
        .collect();
    let mut changed = false;
    for call_id in call_ids {
        let Some(cell) = state.pending_tool_cells.get(&call_id) else {
            continue;
        };
        let Ok(mut cell) = cell.lock() else {
            tracing::error!(call_id = %call_id, "tui.tool_cell.lock_poisoned");
            continue;
        };
        if cell.state != ToolState::Running || state.export_incomplete_tools.contains_key(&call_id)
        {
            continue;
        }
        // Runde 5, Teil M: der echte Grund des Turn-Endes, nicht
        // „Resume-Abbruch" (das gilt nur für den Resume-Pfad).
        cell.mark_incomplete_with(reason);
        let tool_name = state
            .export_tool_calls
            .get(&call_id)
            .and_then(|index| app.export_entries.get(*index))
            .and_then(|entry| match entry {
                ExportEntry::ToolCall { tool_name, .. } => Some(tool_name.clone()),
                _ => None,
            });
        app.export_entries.push(export_tool_result_entry(
            &call_id,
            tool_name,
            &ToolCallResult::error(reason),
            0,
            Some(ResultTrust::Runtime),
        ));
        state.export_incomplete_tools.insert(call_id, ());
        changed = true;
    }
    changed
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
    /// Kanonisches Ausgabeformat aus dem Operation-Marker.
    format: ExportOutputFormat,
    /// `--tools`: Werkzeugaufrufe im Export einschließen.
    include_tool_calls: bool,
    /// Sichere Reasoning-Summaries einschließen.
    include_reasoning_summary: bool,
    /// `--datei <pfad>`: `Some(pfad)` überspringt die Auswahl und schreibt
    /// direkt dorthin; `None` öffnet [`Overlay::ExportChoice`].
    path: Option<String>,
    /// `--max-chars <n>`: harte Obergrenze der Gesamtlänge des gerenderten
    /// Exports in Unicode-Zeichen (nicht je Eintrag). Fehlt der Schlüssel im
    /// Marker, bleibt es `None` — keine Begrenzung.
    max_chars: Option<usize>,
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

    let mut include_tool_calls = true;
    let mut include_reasoning_summary = false;
    let mut format = ExportOutputFormat::Markdown;
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
                        format,
                        include_tool_calls,
                        include_reasoning_summary,
                        path: None,
                        max_chars: None,
                    });
                };
                path = Some(value.clone());
                index += 2;
            }
            "--format" => {
                if let Some(value) = tail.get(index + 1) {
                    format = match value.as_str() {
                        "json" => ExportOutputFormat::Json,
                        "md" | "markdown" => ExportOutputFormat::Markdown,
                        _ => format,
                    };
                    index += 2;
                } else {
                    index += 1;
                }
            }
            "--no-tools" => {
                include_tool_calls = false;
                index += 1;
            }
            "--reasoning-summary" => {
                include_reasoning_summary = true;
                index += 1;
            }
            _ => {
                // Unbekanntes Token: der reguläre Dispatch meldet den Fehler.
                index += 1;
            }
        }
    }
    Some(ExportRequest {
        format,
        include_tool_calls,
        include_reasoning_summary,
        path,
        // `--max-chars` ist Sache des strukturierten `export.request`-Markers
        // (`export_request_from_data`) — der Legacy-Raw-Fallback hier kennt
        // dieses Flag bewusst nicht (siehe `ExportRequest::max_chars`-Doku).
        max_chars: None,
    })
}

/// Liest den kanonischen `export.request`-Marker aus `OpOutput.data`.
///
/// Fehlende optionale Felder werden auf die Vertragsdefaults gesetzt. Ein
/// falscher Feldtyp oder ein unbekanntes Format verwirft den Marker; der
/// Aufrufer kann dann den Legacy-Fallback über die Rohzeile verwenden.
fn export_request_from_data(data: &serde_json::Value) -> Option<ExportRequest> {
    let object = data.as_object()?;
    if object.get("kind").and_then(serde_json::Value::as_str) != Some("export.request") {
        return None;
    }
    let format = match object
        .get("format")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("markdown")
    {
        "json" => ExportOutputFormat::Json,
        "md" | "markdown" => ExportOutputFormat::Markdown,
        _ => return None,
    };
    let include_tool_calls = match object.get("include_tool_calls") {
        None => true,
        Some(value) => value.as_bool()?,
    };
    let include_reasoning_summary = match object.get("include_reasoning_summary") {
        None => false,
        Some(value) => value.as_bool()?,
    };
    let path = match object.get("path") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(value.as_str()?.to_owned()),
    };
    // `max_chars` fehlt im Marker komplett, wenn `--max-chars` nicht
    // angegeben wurde (kein `null` — siehe `harw_ops::export`s Marker-Bau);
    // nur ein positiver Wert wird übernommen.
    let max_chars = object
        .get("max_chars")
        .and_then(serde_json::Value::as_u64)
        .filter(|value| *value > 0)
        .map(|value| value as usize);
    Some(ExportRequest {
        format,
        include_tool_calls,
        include_reasoning_summary,
        path,
        max_chars,
    })
}

/// Baut einen dateinamensicheren Zeitstempel aus der Systemzeit.
///
/// # Beschreibung
/// `time` und `jiff` sind in `harw-tui` **Produktions**-Abhängigkeiten (siehe
/// `Cargo.toml`); `jiff` formatiert das lesbare Datum in der Export-Kopfzeile
/// bereits ([`export::format_export_timestamp`]). Diese Funktion liefert
/// trotzdem bewusst weiterhin Sekunden seit der Unix-Epoche
/// (`"1757831400"`) als reinen Dezimalstring: das ist das Dateinamens-Token
/// für [`export::default_export_path`] (zeitzonenunabhängig, sortierbar,
/// ohne Sonderzeichen); Kollisionen fängt ohnehin [`export::write_export`]
/// über Nummernsuffixe ab. Dieselbe Zeichenkette dient auch als `started_at`
/// in [`build_export_meta`] — [`export::format_started_at_for_display`]
/// erkennt den reinen Zahlenstring und macht daraus über
/// [`export::format_export_timestamp`] ein lesbares Datum, ohne dass diese
/// Funktion selbst kalendarisch formatieren müsste.
fn export_timestamp_now() -> String {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_owned())
}

/// Baut die [`ExportMeta`]-Kopfzeile für `/export` (Titel, Session-ID,
/// Verzeichnis, Modell) — gemeinsam für [`build_export_markdown`] und den
/// JSON-Zweig von [`build_export`], damit beide Formate dieselbe
/// Session-Auflösung verwenden. `started_at` bleibt `None` (siehe
/// [`export_timestamp_now`] für den Grund); Titel, Verzeichnis und
/// Session-ID kommen aus der laufenden Sitzung.
///
/// # Argumente
/// - `app` (`&ChatApp`): liefert Titel, Projekt-Root, Session-ID und Verlauf.
fn build_export_meta(app: &ChatApp) -> ExportMeta {
    let controller = SessionController::snapshot(app.session_controller.as_ref());
    let config = app.resolved_config();
    let provider = controller.active_provider.or_else(|| {
        config
            .as_ref()
            .and_then(|config| config.harness.default_provider.clone())
    });
    let model = controller
        .active_model
        .or_else(|| app.export_session_model.clone())
        .or_else(|| {
            config
                .as_ref()
                .and_then(|config| config.harness.default_model.clone())
        });
    let model = match (provider, model) {
        (Some(provider), Some(model)) => Some(format!("{provider}/{model}")),
        (None, Some(model)) => Some(model),
        (Some(provider), None) => Some(provider),
        (None, None) => None,
    };
    ExportMeta {
        title: app.session_title().map(str::to_owned),
        session_id: app.session_id().to_string(),
        started_at: app.export_started_at.clone(),
        cwd: if app.project_root().is_empty() {
            None
        } else {
            Some(app.project_root().to_owned())
        },
        model,
    }
}

/// Rendert den Export ausschließlich als Markdown.
///
/// # Beschreibung
/// Eigenständiger Markdown-Zweig von [`build_export`]: baut dieselben
/// [`ExportMeta`] über [`build_export_meta`] und rendert sie über
/// [`export::render_markdown_with_extensions`]. [`build_export`] ruft diese
/// Funktion für [`ExportOutputFormat::Markdown`] auf statt die Logik zu
/// duplizieren — so bleibt genau eine Stelle maßgeblich für das
/// Markdown-Rendering.
///
/// # Argumente
/// - `app` (`&ChatApp`): liefert Titel, Projekt-Root, Session-ID und Verlauf.
/// - `opts` (`&ExportOptions`): Inhaltsauswahl (siehe [`export_request_for_command`]).
///
/// # Rückgabe
/// Das fertige Markdown-Dokument.
fn build_export_markdown(app: &ChatApp, opts: &ExportOptions) -> String {
    let meta = build_export_meta(app);
    export::render_markdown_with_extensions(
        &meta,
        &app.export_entries,
        opts,
        &app.export_meta_extensions,
    )
}

/// Baut das Exportdokument aus dem parallel mitgeführten [`ExportEntry`]-Verlauf
/// ([`ChatApp::export_entries`]) im angeforderten Format.
///
/// # Beschreibung
/// Werkzeugaufrufe und Denkschritte bleiben standardmäßig ausgeblendet
/// ([`ExportOptions::default`]) — dieselbe Zurückhaltung wie im Freigabe-Panel:
/// beide können interne Details offenlegen, die nicht jeder Export teilen
/// soll. `started_at` bleibt `None` (siehe [`export_timestamp_now`] für den
/// Grund); Titel, Verzeichnis und Session-ID kommen aus der laufenden Sitzung.
/// Delegiert für Markdown an [`build_export_markdown`]; für JSON rendert sie
/// direkt über [`export::render_json_with_extensions`].
///
/// # Argumente
/// - `app` (`&ChatApp`): liefert Titel, Projekt-Root, Session-ID und Verlauf.
/// - `opts` (`&ExportOptions`): Inhaltsauswahl (siehe [`export_request_for_command`]).
/// - `format` (`ExportOutputFormat`): Markdown oder JSON.
fn build_export(app: &ChatApp, opts: &ExportOptions, format: ExportOutputFormat) -> String {
    match format {
        ExportOutputFormat::Markdown => build_export_markdown(app, opts),
        ExportOutputFormat::Json => {
            let meta = build_export_meta(app);
            export::render_json_with_extensions(
                &meta,
                &app.export_entries,
                opts,
                &app.export_meta_extensions,
            )
        }
    }
}

/// Führt eine erkannte Exportanfrage mit genau deren Format und Optionen aus.
///
/// Bei einem Datei-Export wird der von `write_export_path` tatsächlich
/// gewählte Pfad gemeldet; dadurch bleibt auch ein Kollisionssuffix (`-2`, …)
/// in der Erfolgsmeldung sichtbar.
fn resolve_export_request(app: &mut ChatApp, request: &ExportRequest) {
    let opts = ExportOptions {
        include_tool_calls: request.include_tool_calls,
        include_reasoning: request.include_reasoning_summary,
        max_chars: request.max_chars,
        ..ExportOptions::default()
    };
    if let Some(path) = request.path.as_deref() {
        app.pending_export_options = None;
        let content = build_export(app, &opts, request.format);
        match export::write_export_path(std::path::Path::new(path), &content) {
            Ok(written_path) => app.push_line(
                Role::System,
                format!("Export gespeichert: {}", written_path.display()),
            ),
            Err(error) => app.push_line(Role::System, format!("Export fehlgeschlagen: {error}")),
        }
    } else {
        app.pending_export_options = Some(opts);
        app.pending_export_format = request.format;
        app.open_export_choice();
    }
}

/// Löst eine getroffene `/export`-Auswahl ein (Zwischenablage/Datei/Abbrechen).
///
/// # Beschreibung
/// Index `0` kopiert über [`clipboard::copy_or_sequence`] in die
/// Zwischenablage; landet die Sequenz dabei als [`ClipboardTarget::Osc52`]
/// (kein Systemwerkzeug erreichbar), wird sie roh auf `stdout` geschrieben —
/// denselben Deskriptor, den auch `TerminalGuard` für das Terminal verwendet;
/// ein `TerminalGuard` ist an dieser Stelle (Overlay-Tastenbehandlung) nicht
/// erreichbar. Index `1` schreibt über [`export::write_export_path`] in das
/// aktuelle Arbeitsverzeichnis. Jeder andere Index (insbesondere „Abbrechen")
/// tut nichts. Das Ergebnis erscheint als Systemzeile.
///
/// # Argumente
/// - `app` (`&mut ChatApp`): liefert den Verlauf und nimmt die Ergebniszeile auf.
/// - `bus` (`&HarwEventSender`): ungenutzt heute; symmetrische Signatur zu
///   [`handle_overlay_key`], falls ein künftiger Export-Pfad asynchron wird.
/// - `index` (`usize`): der von [`ChoiceDialog`] gemeldete Auswahlindex.
fn resolve_export_choice(app: &mut ChatApp, _bus: &HarwEventSender, index: usize) {
    let opts = app.pending_export_options.take().unwrap_or_default();
    let format = app.pending_export_format;
    match index {
        0 => {
            let content = build_export(app, &opts, format);
            match clipboard::copy_or_sequence(&content) {
                Ok((ClipboardTarget::Osc52, Some(sequence))) => {
                    // A7: in tmux fängt der Multiplexer OSC-Sequenzen seiner
                    // Kindprozesse ab, statt sie an das echte Terminal
                    // weiterzureichen — die Sequenz muss deshalb in ein
                    // DCS-Passthrough gehüllt werden.
                    let sequence = if std::env::var_os("TMUX").is_some() {
                        clipboard::wrap_osc52_for_tmux(&sequence)
                    } else {
                        sequence
                    };
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
                // A7: weder Systemwerkzeug noch OSC-52 verfügbar (z. B. harw
                // läuft in tmux ohne Zwischenablagen-Weiterleitung und der
                // Text übersteigt die OSC-52-Nutzlastgrenze) — statt nur den
                // Fehler zu melden, wird der Export als Datei gespeichert.
                Err(ExportError::NoClipboard) => {
                    write_export_to_default_path(
                        app,
                        &content,
                        "Keine Zwischenablage — Export gespeichert unter ",
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
            let content = build_export(app, &opts, format);
            write_export_to_default_path(app, &content, "Export gespeichert: ");
        }
        _ => {}
    }
}

/// Schreibt `content` über [`export::default_export_path`] ins aktuelle
/// Arbeitsverzeichnis und hängt je nach Ergebnis eine Systemzeile mit
/// `success_prefix` bzw. dem Fehlschlag an.
///
/// # Beschreibung
/// Gemeinsamer Schreibpfad für die reguläre Datei-Auswahl in
/// [`resolve_export_choice`] (Index `1`) und deren A7-Fallback, wenn beim
/// reinen `/export` (Index `0`) weder ein Zwischenablage-Werkzeug noch
/// OSC-52 verfügbar war.
///
/// # Argumente
/// - `app` (`&mut ChatApp`): nimmt die Ergebniszeile auf.
/// - `content` (`&str`): der bereits gerenderte Exportinhalt.
/// - `success_prefix` (`&str`): Text vor dem geschriebenen Pfad bei Erfolg.
fn write_export_to_default_path(app: &mut ChatApp, content: &str, success_prefix: &str) {
    let now = export_timestamp_now();
    let dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let path = export::default_export_path(&now, &dir);
    match export::write_export_path(&path, content) {
        Ok(written_path) => app.push_line(
            Role::System,
            format!("{success_prefix}{}", written_path.display()),
        ),
        Err(error) => app.push_line(Role::System, format!("Export fehlgeschlagen: {error}")),
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
/// (Session-Picker, `/export`-, Modell/Provider-, Effort-Auswahl) den
/// normalen Eingabepfad ersetzt.
///
/// # Beschreibung
/// - [`Overlay::SessionPicker`]: delegiert an [`SessionPicker::handle_key`].
///   `PickerAction::Open(id)` (`session_picker`) schließt das Overlay und
///   synthetisiert eine `/resume <id>`-Befehlszeile über den bestehenden
///   [`HarwEvent::Command`]-Pfad (`resume_request` in `run_loop` erkennt sie
///   und beendet den Loop mit `TuiRunOutcome::Resume { selector: Some(id) }`,
///   genau wie bei einer getippten Zeile) — kein neuer Ereignistyp nötig.
/// - [`Overlay::ExportChoice`]: delegiert an [`ChoiceDialog::handle_key`].
///   Eine getroffene Wahl schließt das Overlay und wird über
///   [`resolve_export_choice`] eingelöst.
/// - [`Overlay::ModelSwitch`]: delegiert an [`ModelSwitchPicker::on_key`].
///   `ModelSwitchAction::Accept { provider, model }` schließt das Overlay und
///   synthetisiert je nach [`ModelSwitchPicker::target`] `/model switch
///   <model>`, `/uia-model switch <model>` bzw. `/uia-worker-model switch
///   <model>` über [`HarwEvent::Command`] (`provider` bleibt implizit — der
///   Ziel-Op wechselt Provider+Modell atomar).
/// - [`Overlay::EffortChoice`]: delegiert an [`ChoiceDialog::handle_key`].
///   Eine getroffene Wahl schließt das Overlay und synthetisiert je nach
///   `target` `/effort <level>`/`/effort clear` bzw. `/uia-effort
///   <level>`/`/uia-effort clear` (letzter Eintrag „Provider-Default
///   (zurücksetzen)" → `clear`).
///
/// # Rückgabe
/// `true` (jede Taste verändert entweder den Overlay-Zustand oder schließt
/// ihn — in beiden Fällen ist ein Redraw nötig).
fn handle_overlay_key(app: &mut ChatApp, key: KeyEvent, bus: &HarwEventSender) -> bool {
    match app.overlay.as_mut() {
        // Der Arm bindet nichts aus dem Overlay-Inhalt, daher endet der
        // veränderliche Borrow von `app.overlay` schon vor diesem Aufruf und
        // `app` darf hier erneut voll geliehen werden (NLL) — kein separater
        // Vorab-Check mehr nötig, der Fall ist jetzt Teil dieses `match`.
        Some(Overlay::AgentTree(_)) => {
            handle_agent_tree_key(app, key);
        }
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
            ChoiceAction::Cancel => {
                app.overlay = None;
                app.pending_export_options = None;
            }
            ChoiceAction::Chosen(index) => {
                app.overlay = None;
                resolve_export_choice(app, bus, index);
            }
        },
        Some(Overlay::ModelSwitch(picker)) => match picker.on_key(key) {
            ModelSwitchAction::Stay => {}
            ModelSwitchAction::Cancel => app.overlay = None,
            ModelSwitchAction::Accept { provider, model } => {
                let command = picker.target().command_line(&provider, &model);
                // Runde 5, Teil G: Worker-Wahl live übernehmen, nach der
                // UIA-Wahl aus `/models` den Worker-Bereich öffnen.
                let reopen_workers = picker.opens_uia_workers_after();
                app.overlay = None;
                uia_workers::after_model_switch_accept(app, &command, reopen_workers);
                bus.send(HarwEvent::Command(command));
            }
        },
        Some(Overlay::View(view)) => {
            let outcome = view.on_key(key);
            apply_overlay_outcome(app, outcome, bus);
        }
        Some(Overlay::EffortChoice { target, dialog }) => {
            let target = *target;
            match dialog.handle_key(key) {
                ChoiceAction::Stay => {}
                ChoiceAction::Cancel => app.overlay = None,
                ChoiceAction::Chosen(index) => {
                    app.overlay = None;
                    let level = EFFORT_LEVELS
                        .get(index)
                        .map(std::string::ToString::to_string);
                    let level = level.unwrap_or_else(|| "clear".to_owned());
                    let command = match target {
                        EffortTarget::Session => format!("/effort {level}"),
                        EffortTarget::Uia => format!("/uia-effort {level}"),
                    };
                    bus.send(HarwEvent::Command(command));
                }
            }
        }
        None => {}
    }
    true
}

/// Handles the tree's local navigation and delegates a selected stop request
/// to the shared controller. It is used for both idle and busy input, so the
/// tree remains operable while a parent turn is running.
fn handle_agent_tree_key(app: &mut ChatApp, key: KeyEvent) {
    let rows = app.agent_tree_rows();
    let action = match app.overlay.as_mut() {
        Some(Overlay::AgentTree(tree)) => tree.handle_key(key, &rows),
        _ => AgentTreeAction::Stay,
    };
    match action {
        AgentTreeAction::Stay => {}
        AgentTreeAction::Close => app.overlay = None,
        AgentTreeAction::Stop(target) => {
            let child = SessionId::from_str(target.clone());
            let cancelled = app.managed_spawner().is_some_and(|spawner| {
                spawner.owns_descendant(app.session_id(), &child)
                    && spawner.request_cancellation(&child)
            });
            if !cancelled {
                app.push_line(
                    Role::System,
                    format!("Abbruch für Agent {target} konnte nicht angefordert werden."),
                );
            }
        }
    }
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
/// - **Strg+Pos1**/**Strg+Ende** — springt bei leerem Composer im Transkript
///   an Anfang/Ende ([`ChatScroll`]), sonst im Composer an Puffer-Anfang/-Ende
///   (siehe [`scroll_claims_key`] und `InputEditor::move_buffer_start`/
///   `move_buffer_end`).
/// - Bei offenem Popup: Pfeiltasten/Enter/Esc/Ziffern navigieren das Popup.
/// - Alle übrigen Composer-Tasten (Pos1/Ende, Strg+Links/Rechts,
///   Strg+Backspace/Strg+W, Strg+A/Strg+E …) siehe
///   `InputEditor::handle_key_at`-Doku; ein bar Esc kann dort zusätzlich der
///   Anfang einer zerstückelten CSI-Sequenz sein (dtach/zsh-Härtung).
///
/// # Argumente
/// - `app` (`&mut ChatApp`): Zustand, der mutiert wird — inklusive
///   [`ChatApp::pending_quit`] (Welle 4c: vormals ein separater Parameter,
///   jetzt ein Feld, damit auch [`handle_busy_event`] dieselbe Scharfstellung
///   lesen/setzen kann).
/// - `key` (`KeyEvent`): der bereits auf Press/Repeat gefilterte Tastendruck.
/// - `bus` (`&HarwEventSender`): Kanal, über den [`HarwEvent`]s emittiert werden.
///
/// # Rückgabe
/// `true` wenn ein Redraw angefordert werden soll, sonst `false`.
fn handle_key(app: &mut ChatApp, key: KeyEvent, bus: &HarwEventSender) -> bool {
    // Runde 5, Teil L: Esc bricht eine wartende `/btw`-Nebenfrage ab.
    if app.btw_esc_cancels(&key) {
        return true;
    }
    if let Some(redraw) = handle_view_hotkey(app, key) {
        return redraw;
    }
    if let Some(redraw) = handle_panel_key(app, key) {
        return redraw;
    }
    scroll_and_composer_key(app, key, bus)
}

/// Panel-Tasten (Standard F2/F3/F4/F11/Ctrl+E laut [`ChatApp::key_bindings`],
/// Esc und Navigation im fokussierten Panel).
/// `None`: Taste gehört dem Chat/Composer.
fn handle_panel_key(app: &mut ChatApp, key: KeyEvent) -> Option<bool> {
    // Ein offenes Popup/Dialog behält Esc & Pfeile für sich.
    if app.command_popup.as_ref().is_some_and(|p| !p.is_empty())
        || app.mention_popup.as_ref().is_some_and(|p| !p.is_empty())
        || app.pending_approval_dialog.is_some()
        || app.pending_host_permit_dialog.is_some()
    {
        return None;
    }
    // Die Agenten-Detailansicht bekommt ihre Tasten (insbesondere Esc) vor
    // der Panel-Logik, die Esc sonst als „Fokus zurück an den Chat" deutet.
    if app.agent_detail.is_some() && app.panels.focus == crate::panes::PaneFocus::Agents {
        if let Some(redraw) = handle_agent_detail_key(app, key) {
            return Some(redraw);
        }
    }
    match app.panels.handle_key(key, &app.key_bindings) {
        crate::panes::PanelKey::Ignored => None,
        crate::panes::PanelKey::Changed => {
            app.sync_agent_detail();
            app.ensure_explorer();
            // Frisch eingeblendete Werkbank lädt neu (siehe
            // `process_pending_fetches`).
            if app.panels.workbench_visible
                && app.key_bindings.action_for(&key) == Some(KeyAction::ToggleWorkbench)
            {
                app.workbench.mark_stale();
            }
            Some(true)
        }
        // Übrige Tasten verschluckt die Detailansicht (kein Rückfall auf
        // die Listennavigation).
        crate::panes::PanelKey::ForFocused(_) if app.agent_detail.is_some() => Some(false),
        crate::panes::PanelKey::ForFocused(key) => Some(match app.panels.focus {
            crate::panes::PaneFocus::Agents => match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    app.agent_monitor.select_next();
                    true
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    app.agent_monitor.select_prev();
                    true
                }
                KeyCode::Enter => app.open_agent_detail(),
                _ => false,
            },
            crate::panes::PaneFocus::Explorer => handle_explorer_key(app, key),
            crate::panes::PaneFocus::Workbench => app.handle_workbench_key(key),
            crate::panes::PaneFocus::Chat => false,
        }),
    }
}

/// Tasten der Agenten-Detailansicht.
///
/// # Beschreibung
/// Esc/q zurück zur Liste, j/↓ Richtung neuester Einträge, k/↑ ältere,
/// PageUp/PageDown um [`AGENT_DETAIL_PAGE`] Zeilen, Home/g an den Anfang,
/// End/G ans Ende (folgt dem Neuesten), `r` schaltet Reasoning um.
///
/// # Rückgabe
/// `Some(redraw)`, wenn die Taste verarbeitet wurde; `None` für Tasten, die
/// an die Panel-Logik weitergehen (z. B. F2/F3/F4/F11).
fn handle_agent_detail_key(app: &mut ChatApp, key: KeyEvent) -> Option<bool> {
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return None;
    }
    if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
        app.close_agent_detail();
        return Some(true);
    }
    let detail = app.agent_detail.as_mut()?;
    match key.code {
        KeyCode::Down | KeyCode::Char('j') => detail.scroll = detail.scroll.saturating_sub(1),
        KeyCode::Up | KeyCode::Char('k') => detail.scroll = detail.scroll.saturating_add(1),
        KeyCode::PageDown => detail.scroll = detail.scroll.saturating_sub(AGENT_DETAIL_PAGE),
        KeyCode::PageUp => detail.scroll = detail.scroll.saturating_add(AGENT_DETAIL_PAGE),
        KeyCode::Home | KeyCode::Char('g') => detail.scroll = u16::MAX,
        KeyCode::End | KeyCode::Char('G') => detail.scroll = 0,
        KeyCode::Char('r') => detail.show_reasoning = !detail.show_reasoning,
        _ => return None,
    }
    Some(true)
}

/// Entscheidet, ob eine Taste zuerst dem Transkript-Scroll ([`ChatScroll`])
/// angeboten werden soll, bevor sie den Composer erreicht (Register
/// „CSI-Sicherheitsnetz und Paste-Platzhalter", Punkt 5).
///
/// # Beschreibung
/// Strg+Pos1/Strg+Ende sind doppelt belegt: [`ChatScroll`] springt damit an
/// Transkript-Anfang/-Ende, [`InputEditor`] an Composer-Anfang/-Ende
/// ([`InputEditor::move_buffer_start`]/[`InputEditor::move_buffer_end`]).
/// Bei nicht-leerer Eingabe gewinnt der Composer — ein Strg+Pos1 während des
/// Tippens soll den Cursor bewegen, nicht wortlos das Transkript
/// verschieben; bei leerem Composer bleibt das bisherige Verhalten
/// (Transkript-Sprung) unverändert. Alle anderen von
/// [`ChatScroll::handle_key`] behandelten Tasten (PageUp/PageDown,
/// Shift+Up/Down) sind davon nicht betroffen.
fn scroll_claims_key(app: &ChatApp, key: &KeyEvent) -> bool {
    let is_ctrl_home_end = key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Home | KeyCode::End);
    !is_ctrl_home_end || app.input.is_empty()
}

fn scroll_and_composer_key(app: &mut ChatApp, key: KeyEvent, bus: &HarwEventSender) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    // ── Globale Steuer-Keys (unabhängig von Popup/Editor-Zustand) ────────

    // Ctrl+C — Doppeldruck beendet (unabhängig vom Eingabeinhalt).
    if ctrl && matches!(key.code, KeyCode::Char('c' | 'C')) {
        if matches!(
            app.pending_quit,
            Some(QuitArm {
                label: "Ctrl+C",
                ..
            })
        ) {
            bus.send(HarwEvent::Quit);
            return false;
        }
        app.pending_quit = Some(QuitArm {
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
            app.pending_quit,
            Some(QuitArm {
                label: "Ctrl+D",
                ..
            })
        ) {
            bus.send(HarwEvent::Quit);
            return false;
        }
        app.pending_quit = Some(QuitArm {
            label: "Ctrl+D",
            at: Instant::now(),
        });
        return true;
    }

    // Jede andere Taste macht eine Scharfstellung rückgängig.
    app.pending_quit = None;

    // Konfigurierbare Aktionen (Standardbelegung siehe `keybindings.rs`).
    let action = app.key_bindings.action_for(&key);
    if !matches!(key.code, KeyCode::Esc) {
        app.escape_armed = false;
    }

    // ── Vollflächige Overlays (Session-Picker, `/export`-, Modell/Provider-,
    // Effort-Auswahl) ─────────────────────────────────────────────────────
    // Exklusiv: solange eines offen ist, geht keine Taste an Popup, Editor
    // oder ChatScroll (Plan Schritt 6/7).
    if app.has_overlay() {
        return handle_overlay_key(app, key, bus);
    }

    // Runde 5, Teil G: `@`- und `/command`-Popup (Hoch/Runter/Tab/Esc/
    // Enter-Übernahme) gemeinsam mit dem Busy-Pfad (`popup_keys`).
    if let Some(redraw) = popup_keys::handle_popup_key(app, key) {
        return redraw;
    }

    // CyclePermissionMode (Standard Shift+Tab) — Zyklus ask → auto → full →
    // plan (Plan Schritt 5). Nur außerhalb eines offenen `/command`-Popups:
    // sonst hätte dieselbe Taste zwei Bedeutungen (Popup-Navigation vs.
    // Moduszyklus).
    if action == Some(KeyAction::CyclePermissionMode) && !app.has_popup() {
        // `handle_key` läuft ausschließlich außerhalb eines laufenden Turns
        // (während eines Turns übernimmt `handle_busy_event`) — der Wechsel
        // wirkt hier also sofort, nicht vorgemerkt.
        app.cycle_permission_stage(false);
        return true;
    }

    // DeleteLine (Standard Ctrl+K) löscht die komplette aktuelle Zeile (nicht
    // nur bis Zeilenende), auch bei offenem Command-Popup — nachfolgende
    // Zeilen rücken nach oben.
    if action == Some(KeyAction::DeleteLine) {
        app.input.delete_current_line();
        app.sync_popup();
        return true;
    }

    // ToggleToolCells (Standard Ctrl+O) — klappt die letzte bzw. bei erneutem
    // Druck alle Werkzeugzellen auf/zu (Plan Schritt 2).
    if action == Some(KeyAction::ToggleToolCells) {
        return app.toggle_tool_cells();
    }

    // EndHostMode (Standard Ctrl+H) — beendet eine laufende Host-Arbeitsphase
    // sofort (Plan „UIA-Shell-Worker und Shell-Modus", Schritt 2: „eine
    // Möglichkeit, sie zu beenden"). Ohne aktive Phase tut die Taste nichts (kein Redraw).
    if action == Some(KeyAction::EndHostMode) {
        return app.end_host_mode();
    }

    // ── ChatScroll konsultieren (PageUp/PageDown/Shift+Up/Shift+Down etc.) ──
    // Echte Werte aus dem letzten `draw_viewport`-Aufruf (vor dem ersten Draw:
    // 0 Zeilen / 20 sichtbar als sicherer Platzhalter) — ChatScroll clamped selbst.
    // Strg+Pos1/Strg+Ende gehen bei nicht-leerer Eingabe stattdessen an den
    // Composer (siehe `scroll_claims_key`).
    if scroll_claims_key(app, &key) {
        match app.scroll.handle_key(
            key,
            app.last_history_total_lines() as usize,
            app.last_history_visible_rows() as usize,
        ) {
            ScrollAction::Redraw => return true,
            ScrollAction::Passthrough => {}
        }
    }

    // InsertNewline (Standard Ctrl+J) — neue Zeile einfügen (schließt ein
    // offenes Popup).
    if action == Some(KeyAction::InsertNewline) {
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
        // Runde 5, Teil G: die Übernahme einer markierten Popup-Auswahl
        // (auch der Unterkommando-Sonderfall `/kanban `) liegt jetzt in
        // `popup_keys::handle_popup_key`, das oben bereits lief.
        // Plain Enter ohne Auswahl: getippte Zeile absenden — mit aufgelösten
        // Paste-Platzhaltern, sonst bekäme das Modell nur
        // `[Pasted text #<id> +<n> lines]` statt des eingefügten Texts.
        let line = app.input.submission_text();
        app.input.clear();
        app.command_popup = None;
        app.mention_popup = None;
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
            // Runde 5, Teil G: Tab/Shift+Tab/Esc und die Enter-Übernahme
            // behandelt `popup_keys::handle_popup_key` (oben, gemeinsam mit
            // dem Busy-Pfad); hier verbleiben Randfälle mit Modifikatoren.
            // Im Unterkommando-Modus sind Ziffern normale Eingabe.
            KeyCode::Char(character @ '1'..='9')
                if !app
                    .command_popup
                    .as_ref()
                    .is_some_and(CommandPopup::digits_select) =>
            {
                app.input.insert_char(character);
                app.sync_popup();
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
                            let line = popup.completion_line(&name);
                            app.input.clear();
                            app.input.insert_str(&line);
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
                app.mention_popup = None;
                app.escape_armed = false;
            } else {
                app.escape_armed = true;
                let _ = app.input.handle_key(key);
            }
            return true;
        }

        let action = app.input.handle_key(key);
        // CSI-Sicherheitsnetz (siehe `input_editor.rs`): der Esc, der eben
        // erst `escape_armed` scharfgestellt hat, war in Wahrheit der
        // Anfang einer zerstückelten CSI-Sequenz (z. B. Pos1/Ende/Maus-
        // Report nach einem host `shell.exec`, siehe dortige Doku) — kein
        // bewusster Tastendruck. Die Scharfstellung wird deshalb rückgängig
        // gemacht, damit ein später wirklich gedrücktes einzelnes Esc den
        // Composer weiterhin nicht sofort leert, sondern erst beim zweiten
        // bewussten Druck.
        if app.input.take_swallowed_escape() {
            app.escape_armed = false;
        }
        match action {
            InputAction::Submit(text) => {
                // InputEditor hat den Puffer bereits geleert.
                app.remember_input(&text);
                app.scroll.force_follow();
                app.command_popup = None;
                app.mention_popup = None;
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
/// Antwort — sofern [`drive_turn_animated`] eine liefert — zeilenweise via
/// [`reveal_reply`]. Liefert der Turn keine (siehe dortige Doku: Turn ohne
/// neue, nicht-`Commentary` Assistant-Nachricht), bleibt `reveal_reply`
/// bewusst aus, statt die Antwort des vorherigen Turns erneut zu zeigen. Die
/// vollständige Antwort wird in den Zustands-Log ([`ChatApp::push_line`])
/// übernommen.
///
/// # Fehler
/// [`TuiError`] bei Turn-Fehler oder Terminal-I/O.
///
/// Persistiert die nicht im Transcript enthaltene Session-Projektion an einer
/// sicheren TUI-Turn-Grenze. Ein Fehler wird nicht in einen flüchtigen
/// Weiterlauf umgewandelt: der Aufrufer erhält ihn als [`TuiError::Core`],
/// damit ein anschließendes Resume keine unbestätigte Modus- oder
/// Aktivierungsänderung sieht.
async fn persist_gateway_state(
    gateway: &mut dyn crate::gateway::ChatGateway,
) -> Result<(), TuiError> {
    let (session, store, _) = gateway.borrow_turn_ctx();
    session
        .persist_state(store)
        .await
        .map_err(|error| TuiError::Core(format!("could not persist session state: {error}")))
}

#[allow(clippy::too_many_arguments)]
async fn run_turn_streaming(
    guard: &mut TerminalGuard,
    app: &mut ChatApp,
    spinner: &mut Spinner,
    gateway: &mut dyn crate::gateway::ChatGateway,
    text: &str,
    approval_driver: &ApprovalDriver,
    approvals: &mut ApprovalPromptReceiver,
    host_permit_prompts: &mut HostPermitPromptReceiver,
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
    if app.apply_pending_controller_state(gateway.session_mut()) {
        persist_gateway_state(gateway).await?;
    }
    // Runde 5, Teil L: Gesprächsstand für `/btw` während dieses Turns (die
    // Sitzung ist danach bis zum Turn-Ende ausgeliehen).
    app.capture_btw_snapshot(gateway.session_mut(), Some(text));

    // Keep a clone in the UI so Ctrl+C can cancel the core turn at its
    // cooperative checkpoints instead of merely queuing a character.
    let cancel = CancelToken::new();
    app.active_cancel = Some(cancel.clone());
    // Ein neuer Turn startet: ein evtl. noch angezeigter "Abbruch
    // angefordert …"-Hinweis aus einem vorherigen, jetzt abgeschlossenen Turn
    // gehört nicht mehr zum aktuellen Zustand. Derselbe Reset gilt für den
    // "Warteschlange wird gesendet"-Hinweis.
    app.cancel_requested_at = None;
    app.queue_kept_at = None;
    // Runde 5, Teil O: Esc-Nachfrage und Abbruchgrund gelten nur je Turn.
    turn_safety::begin_turn(app);
    // Welle 4c, Punkt 8: denselben `ManagedAgentSpawner`, den die Wurzelsitzung
    // beim Admittieren von Kindern befragt ([`ChatApp::managed_spawner`] ist
    // exakt `RuntimeAssembly::spawner()` — siehe deren Montage in
    // `runtime_root.rs`, `with_managed_spawner(assembly.spawner().cloned())`,
    // und `assembly.rs`, wo dieselbe `Arc<ManagedAgentSpawner>`-Instanz sowohl
    // in den Registry-Builder als auch ins `RuntimeAssembly`-Feld wandert),
    // mit dem Eltern-Cancel-Token dieses Turns registrieren. Danach admittierte
    // Kinder erben `cancel.child()`; ein harter Ctrl+C-Abbruch bricht sie
    // dadurch mit ab. Ein Fehler (z. B. weil diese Session selbst ein
    // admittiertes Kind ist) ist nicht fatal für den Turn — nur geloggt.
    if let Some(spawner) = app.managed_spawner() {
        if let Err(error) = spawner.register_parent_cancel_token(app.session_id(), cancel.clone()) {
            tracing::warn!(error = %error, "tui.turn.register_parent_cancel_token_failed");
        }
    }
    // Runde 5, Teil E: die Nutzernachricht ist das „Ziel der Sitzung“ für
    // den Auto-Modus-Klassifizierer (Geheimnisse entfernt er selbst).
    if let Some(rt) = app.runtime() {
        if let Some(auto) = rt.auto_mode() {
            auto.context().set_goal(text);
            // Runde 5 (Integration E/F): der angeheftete, freigegebene Plan ist
            // Kontext für den Klassifizierer (gekürzt, ohne Geheimnis-Pfad —
            // Redaction übernimmt der Klassifizierer selbst).
            let plan = rt.plan_session().pinned().get().map(|doc| {
                let mut content = doc.content;
                if content.len() > 4096 {
                    let mut cut = 4096;
                    while !content.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    content.truncate(cut);
                }
                format!("{}\n{content}", doc.display_path)
            });
            auto.context().set_plan(plan);
        }
    }
    spinner.start();
    let reply = drive_turn_animated(
        guard,
        app,
        spinner,
        gateway,
        TurnInput::user(text).with_control(TurnControl::new().with_cancel(cancel)),
        approval_driver,
        approvals,
        host_permit_prompts,
        tui_rx,
        turn_event_rx,
        turn_state,
    )
    .await;
    spinner.stop();
    // Runde 5, Teil O: Abbruchgrund für spät verarbeitete `TurnAborted` merken.
    turn_safety::end_turn(app);
    app.active_cancel = None;
    // Turn ist beendet (egal ob normal, per Fehler oder per Abbruch) — der
    // transiente Abbruch-Hinweis hat damit ausgedient. Derselbe Reset gilt
    // für den "Warteschlange wird gesendet"-Hinweis.
    app.cancel_requested_at = None;
    app.queue_kept_at = None;

    let reply = reply?;

    // Auto-Compact läuft jetzt in harw-core selbst (Turn-Loop nach
    // Runden/Turns), gesteuert über `AgentSession::auto_compact()`. Die TUI
    // muss dafür nichts mehr tun.
    //
    // `None` heißt: dieser Turn hat keine neue, nicht-`Commentary`
    // Assistant-Nachricht angehängt (siehe `latest_final_reply`-Doku bei
    // `drive_turn_animated`) — `reveal_reply` bleibt dann bewusst aus, sonst
    // erschiene entweder eine bereits live gezeigte Commentary erneut oder
    // die Antwort des vorherigen Turns.
    if let Some(reply) = reply {
        reveal_reply(guard, app, &reply).await?;
    }
    Ok(())
}

/// Maximale Wartezeit nach einem Rate-Limit in Sekunden, die automatisch
/// abgewartet wird. Darüber hinaus wird dem User eine manuelle Retry-Bitte
/// angezeigt, aber die Session bleibt geöffnet.
///
/// Obergrenze je Wartephase. Anthropic meldet `retry-after` oft bei 60–120 s;
/// über diesem Deckel ist weitere automatische Warterei nicht mehr
/// produktiv — dann bekommt der User die Kontrolle zurück.
const RATE_LIMIT_AUTO_RETRY_CAP_SECS: u64 = 120;

/// Maximale Gesamtzahl automatischer Rate-Limit-Versuche (Erstversuch
/// eingeschlossen). Harte Anbieter-Limits können mehrere aufeinanderfolgende
/// 429 liefern; ein einzelner Retry reicht dort nicht aus.
const RATE_LIMIT_MAX_ATTEMPTS: u32 = 3;

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
/// gezeichnet, bis der Turn abgeschlossen ist. Extrahiert danach über
/// [`latest_final_reply`] die Turn-Antwort — auf die Items beschränkt, die
/// **während dieses Turns** neu an die Historie angehängt wurden.
///
/// # Bugfix: doppelte/verwaiste Antwortanzeige
/// Früher lieferte diese Funktion `gateway.session_mut().history()
/// .to_model_messages().into_iter().rev().find_map(...)` — turn-blind und
/// phasenblind über die **gesamte** Session-Historie. Zwei Symptome:
/// 1. War die letzte Assistant-Nachricht des Turns eine bereits über
///    [`handle_turn_event`] live gepushte `Commentary`-Nachricht (gefolgt nur
///    von Tool-Aufrufen), erschien ihr Text ein zweites Mal.
/// 2. Fügte der Turn **gar keine** neue Assistant-Nachricht hinzu (z. B. ein
///    zusammengefasster Warteschlangen-Turn, dispatcht über
///    `run_turn_streaming`), lieferte `find_map` die Antwort des
///    **vorherigen** Turns — sie erschien erneut, als hätte der neue Turn
///    sie beantwortet.
///
/// Die Behebung: [`latest_final_reply`] betrachtet ausschließlich Items ab
/// dem Historie-Stand vor diesem Turn (`history_len_before`, unten
/// festgehalten) und ausschließlich [`TurnItem::AssistantMessage`]s, deren
/// `phase` **nicht** `Commentary` ist — dafür braucht es `ConversationHistory
/// ::items()` statt `to_model_messages()`, weil Letzteres das `phase`-Feld
/// verwirft. Findet sich in diesem Bereich keine solche Nachricht, liefert
/// diese Funktion `Ok(None)`; der Aufrufer ([`run_turn_streaming`]) ruft dann
/// [`reveal_reply`] bewusst **nicht** auf. Als zweite Verteidigungslinie
/// gleicht [`suppress_if_matches_commentary`] das Ergebnis zusätzlich gegen
/// `turn_state.last_commentary_text` ab (siehe dortige Doku).
///
/// Wenn der Provider HTTP 429 zurückgibt ([`ModelError::RateLimited`]), läuft
/// eine budgetierte Retry-Schleife: bis zu [`RATE_LIMIT_MAX_ATTEMPTS`]
/// Versuche, je Wartephase `retry_after_secs` (gedeckelt auf
/// [`RATE_LIMIT_AUTO_RETRY_CAP_SECS`]) plus einem deterministischen Jitter
/// von bis zu 25 %, damit parallele Clients nicht im Gleichtakt erneut
/// an denselben Anbieter-Limiter schlagen. Die Chat-Session wird dabei
/// **nicht** beendet:
/// - Irgendein Versuch erfolgreich → Antwort wie gewohnt (`Ok(Some(..))`,
///   ggf. `Ok(None)` — siehe oben).
/// - Budget erschöpft, weiterhin 429 → `Ok(Some("⏱ Rate limit — …"))`; der
///   User kann erneut senden.
/// - Anderer Fehler → `Err(TuiError::Core(...))` (normaler Fehlerfall).
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
/// B6: bereits das **eigene** `select!` dieser Funktion (die noch nicht in
/// [`drive_pauses_to_completion`] delegierte Schleife um den `turn`-Future
/// selbst) pollt zusätzlich `host_permit_prompts` — ein Modell-Tool
/// (`sandbox-lease`) kann während des laufenden Turns eine Host-Permit-Frage
/// stellen, ohne dass der Turn dafür in `TurnOutcome::AwaitingApproval`
/// pausiert; ohne dieses Pollen erschiene der Dialog nie. `tui_rx` routet
/// Tasten währenddessen (mit demselben Arming-Delay und Ctrl+C-Fail-Safe wie
/// beim Freigabe-Panel) an [`ChoiceDialog::handle_key`] statt an den
/// normalen Composer-Pfad.
///
/// # Rückgabe
/// `Ok(Some(text))`, wenn eine Antwort enthüllt werden soll; `Ok(None)`, wenn
/// dieser Turn keine neue, nicht-`Commentary` Assistant-Nachricht angehängt
/// hat (siehe Bugfix-Abschnitt oben) — der Aufrufer lässt `reveal_reply` dann
/// aus.
///
/// # Fehler
/// [`TuiError::Core`], wenn der Turn fehlschlägt (nicht durch Rate-Limit) oder
/// der Freigabetreiber scheitert; [`TuiError::Io`] beim Zeichnen.
#[allow(clippy::too_many_arguments)]
async fn drive_turn_animated(
    guard: &mut TerminalGuard,
    app: &mut ChatApp,
    spinner: &mut Spinner,
    gateway: &mut dyn crate::gateway::ChatGateway,
    input: TurnInput,
    approval_driver: &ApprovalDriver,
    approvals: &mut ApprovalPromptReceiver,
    host_permit_prompts: &mut HostPermitPromptReceiver,
    tui_rx: &mut tokio::sync::mpsc::UnboundedReceiver<TuiEvent>,
    turn_event_rx: &mut tokio::sync::mpsc::UnboundedReceiver<TurnEvent>,
    turn_state: &mut TurnEventState,
) -> Result<Option<String>, TuiError> {
    // Einmal zeichnen, damit der Spinner sofort erscheint.
    draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;

    // Historie-Stand VOR diesem Turn und Reset des Commentary-Wächters
    // (siehe Bugfix-Abschnitt der Funktionsdoku oben): beides beschreibt
    // ausschließlich DIESEN Turn, nicht die gesamte Session-Historie, die
    // `turn_state` sonst über Turns hinweg unverändert lässt.
    let history_len_before = gateway.session_mut().history().len();
    turn_state.last_commentary_text = None;

    // Rate-Limit-Retry-Schleife: Erstversuch plus bis zu
    // RATE_LIMIT_MAX_ATTEMPTS-1 Wiederholungen. Jeder Versuch läuft durch
    // dieselbe animierte Select-Schleife; nur das TurnInput unterscheidet
    // sich — Retries nutzen `rate_limit_retry_input()`, damit die bereits
    // persistierte User-Message nicht doppelt angehängt wird.
    let mut input_open = true;
    let mut pending_input = Some(input);
    let mut attempt: u32 = 0;
    // B6: Zustand für die Host-Permit-Frage, analog zu
    // `drive_pauses_to_completion`. Überlebt bewusst Rate-Limit-Retries
    // (deklariert vor der äußeren `loop`), damit eine während eines
    // Versuchs geöffnete Frage nicht durch einen erneuten Versuch verloren
    // geht.
    let mut host_permit_shown_at: Option<Instant> = None;
    let mut host_permit_prompts_open = true;

    let outcome = loop {
        attempt += 1;
        let turn_input = pending_input.take().unwrap_or_else(rate_limit_retry_input);

        // Turn-Future in einen Block scopen, damit der `&mut session`-Borrow
        // freigegeben wird, bevor wir `session.history()` lesen.
        let result = {
            let (session, store, model) = gateway.borrow_turn_ctx();
            let turn = run_turn(session, model, store, turn_input);
            tokio::pin!(turn);
            loop {
                tokio::select! {
                    result = &mut turn => break result,
                    // Runde 5, Teil B: `host.sudo_exec` fragt während des
                    // laufenden Turns im eigenen Fenster (`crate::sudo_dialog`).
                    maybe_sudo = app.sudo.recv(), if app.sudo.is_listening() => {
                        if crate::sudo_dialog::accept_prompt(app, maybe_sudo) {
                            draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                        }
                    }
                    // Runde 5, Teil F: `plan.exit`, `plan.enter` und `ask_user`
                    // fragen während des laufenden Turns im eigenen Fenster.
                    maybe_plan = app.plan_ui.recv(), if app.plan_ui.is_listening() => {
                        if plan_mode::accept_request(app, maybe_plan) {
                            draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                        }
                    }
                    // Runde 5, Teil O: Freigabe-Fragen von Kind-Agenten im
                    // selben Freigabedialog (mit Absender).
                    maybe_child = app.child_approvals.recv(), if app.child_approvals.is_listening() => {
                        if child_approvals::accept(app, maybe_child) {
                            draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                        }
                    }
                    // B6: ein Modell-Tool (`sandbox-lease`) kann während des
                    // laufenden Turns eine Host-Permit-Frage stellen, ohne
                    // dass der Turn dafür pausiert (kein `TurnOutcome`-Wechsel
                    // wie bei `AwaitingApproval`) — deshalb muss dieser Kanal
                    // schon hier, neben dem laufenden `turn`-Future, gepollt
                    // werden. Muster aus `drive_pauses_to_completion`s
                    // `host_permit_prompts.recv()`-Arm.
                    maybe_host_prompt = host_permit_prompts.recv(), if host_permit_prompts_open => {
                        match maybe_host_prompt {
                            Some(prompt) => {
                                tracing::info!(
                                    session = prompt.session(),
                                    worker = prompt.worker_definition(),
                                    "tui.host_permit.prompt_shown"
                                );
                                host_permit_shown_at = Some(open_host_permit_prompt(app, prompt));
                                draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                            }
                            None => {
                                tracing::warn!("tui.host_permit.prompt_channel_ended");
                                host_permit_prompts_open = false;
                            }
                        }
                    }
                    event = tui_rx.recv(), if input_open => {
                        match event {
                            // Runde 5, Teil B: ein offenes sudo-Fenster fängt
                            // JEDES Ereignis ab — nichts erreicht Composer,
                            // Busy-Queue oder Eingabe-Historie.
                            Some(event) if app.sudo.is_open() => {
                                if crate::sudo_dialog::route_event(app, event) {
                                    draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                                }
                            }
                            // Runde 5, Teil F: ein offenes Plan-Fenster fängt
                            // jedes Ereignis ab (nach dem sudo-Fenster).
                            Some(event) if app.plan_ui.is_open() => {
                                if plan_mode::route_event(app, event) {
                                    draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                                }
                            }
                            // Runde 5, Teil O: eine offene Kind-Freigabe ist modal.
                            Some(event) if child_approvals::is_open(app) => {
                                match child_approvals::route_event(app, event) {
                                    Ok(redraw) => {
                                        if redraw {
                                            draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                                        }
                                    }
                                    Err(event) => {
                                        let outcome = handle_busy_event(app, event);
                                        settle_busy_outcome(guard, app, spinner, outcome)?;
                                    }
                                }
                            }
                            Some(TuiEvent::Key(key))
                                if key.modifiers.contains(KeyModifiers::CONTROL)
                                    && matches!(key.code, KeyCode::Char('c' | 'C'))
                                    && app.pending_host_permit.is_some() =>
                            {
                                // Ctrl+C bleibt fail-safe und lehnt eine offene
                                // Host-Permit-Frage sofort ab, unabhängig vom
                                // Arming-Delay des Dialogs — dieselbe
                                // Sicherheitsinvariante wie in
                                // `drive_pauses_to_completion`. Der laufende
                                // Turn selbst wird über `handle_busy_event`
                                // weiterhin ganz normal kooperativ abgebrochen.
                                if let Some(prompt) = app.pending_host_permit.take() {
                                    prompt.deny();
                                }
                                app.pending_host_permit_dialog = None;
                                host_permit_shown_at = None;
                                let outcome = handle_busy_event(app, TuiEvent::Key(key));
                                settle_busy_outcome(guard, app, spinner, outcome)?;
                            }
                            Some(TuiEvent::Key(key)) if app.pending_host_permit.is_some() => {
                                // Dasselbe Arming-Delay wie beim Freigabe-Panel
                                // (`approval_dialog_key_is_armed`), hier auf den
                                // generischen `ChoiceDialog` angewandt: solange
                                // nicht scharfgeschaltet, zählt keine Taste.
                                let since_shown = host_permit_shown_at.map_or(Duration::ZERO, |shown| shown.elapsed());
                                if !approval_dialog_key_is_armed(key, since_shown) {
                                    continue;
                                }
                                let Some(dialog) = app.pending_host_permit_dialog.as_mut() else {
                                    continue;
                                };
                                match dialog.handle_key(key) {
                                    ChoiceAction::Stay => {
                                        // Runde 5, Teil H: der Host-Permit-Dialog ist
                                        // modal — Pfeiltasten usw. dürfen nie im
                                        // Composer landen (dort riefen Hoch/Runter
                                        // die Eingabe-Historie ab, etwa ein altes
                                        // `/new`, das die nächste Eingabe abschickte).
                                        draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                                    }
                                    ChoiceAction::Cancel => {
                                        if let Some(prompt) = app.pending_host_permit.take() {
                                            prompt.deny();
                                        }
                                        app.pending_host_permit_dialog = None;
                                        host_permit_shown_at = None;
                                        app.push_line(Role::System, "Host-Ausführung abgelehnt.");
                                        draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                                    }
                                    ChoiceAction::Chosen(index) => {
                                        if let Some(prompt) = app.pending_host_permit.take() {
                                            apply_host_permit_decision(app, prompt, index);
                                            // Register „CSI-Sicherheitsnetz
                                            // und Paste-Platzhalter",
                                            // Punkt 7: eine Host-Permit-
                                            // Entscheidung kann eine Host-
                                            // Arbeitsphase beginnen oder
                                            // beenden, die Terminal-Modi
                                            // beschädigt zurücklässt.
                                            guard.reassert_terminal_modes();
                                        }
                                        app.pending_host_permit_dialog = None;
                                        host_permit_shown_at = None;
                                        draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                                    }
                                }
                            }
                            Some(event) => {
                                let outcome = handle_busy_event(app, event);
                                settle_busy_outcome(guard, app, spinner, outcome)?;
                            }
                            None => input_open = false,
                        }
                    }
                    // Werkzeug-, Kind- und Plan-Zellen erscheinen dadurch
                    // bereits während des Turns statt erst nach seinem Ende.
                    maybe_turn_event = turn_event_rx.recv() => {
                        if let Some(event) = maybe_turn_event {
                            if handle_turn_event(app, turn_state, event) {
                                draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                            }
                            // Register „CSI-Sicherheitsnetz und Paste-
                            // Platzhalter", Punkt 7: ein abgeschlossener
                            // `shell.exec`-Aufruf kann Terminal-Modi
                            // beschädigt zurückgelassen haben (siehe
                            // `handle_turn_event`).
                            if app.take_needs_terminal_reassert() {
                                guard.reassert_terminal_modes();
                            }
                        }
                    }
                    // Runde 4, Teil H: Ergebnisse nebenläufiger Busy-Befehle und
                    // -Datenabrufe; der Turn läuft währenddessen ungebremst weiter.
                    Some(done) = app.busy_jobs.recv() => {
                        apply_busy_job_done(app, done);
                        start_busy_work(app);
                        draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                    }
                    _ = tokio::time::sleep(SPINNER_INTERVAL) => {
                        spinner.tick();
                        app.drain_agent_events();
                        // Runde 5, Teil O: aufgegebene Kind-Fragen schließen,
                        // zurückgestellte wieder zeigen.
                        child_approvals::poll(app);
                        // Ansichten füllen sich auch während eines Turns.
                        start_busy_work(app);
                        draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                    }
                }
            }
        };

        match result {
            Err(CoreError::Model(ModelError::RateLimited {
                retry_after_secs, ..
            })) if attempt < RATE_LIMIT_MAX_ATTEMPTS => {
                // Wartezeit: provider-Hinweis, gedeckelt, plus deterministischer
                // Jitter (bis 25 %, abgeleitet aus der Versuchsnummer), damit
                // parallele Clients nicht im Gleichtakt erneut auf denselben
                // Limiter schlagen.
                let base = retry_after_secs.min(RATE_LIMIT_AUTO_RETRY_CAP_SECS);
                let jitter = (base / 4)
                    .min(15)
                    .saturating_mul(u64::from(attempt - 1) % 2);
                let wait_secs = base.saturating_add(jitter);
                let deadline =
                    tokio::time::Instant::now() + std::time::Duration::from_secs(wait_secs);
                while tokio::time::Instant::now() < deadline {
                    tokio::time::sleep(SPINNER_INTERVAL).await;
                    spinner.tick();
                    app.drain_agent_events();
                    draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                }
                // Nächster Schleifendurchlauf: leerer Retry-Input.
            }
            // Budget erschöpft und weiterhin 429 → Session am Leben lassen,
            // User informieren; er kann erneut senden.
            Err(CoreError::Model(ModelError::RateLimited {
                retry_after_secs, ..
            })) => {
                return Ok(Some(format!(
                    "⏱ Rate limit — provider busy; retry in {}s ({} attempts used)",
                    retry_after_secs, attempt
                )));
            }
            // Kein Rate-Limit: normaler Fehler oder Erfolg.
            other => break other.map_err(|error| TuiError::Core(error.to_string()))?,
        }
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
            host_permit_prompts,
            tui_rx,
            turn_event_rx,
            turn_state,
        )
        .await?;
    }

    // Runde 5, Teil B: nach Turn-Ende bleibt kein sudo-Fenster offen.
    // Runde 5, Teil K: außer ein Hintergrund-Agent läuft weiter (dann
    // beantwortet die Nutzerin es im Leerlauf).
    if !background_agents::keeps_idle_prompts(app) {
        crate::sudo_dialog::deny_open(app);
    }
    // Runde 5, Teil F: ebenso kein Plan-Fenster.
    plan_mode::close_open(app);

    // Nachdem der Turn-Future gedroppt ist, ist der `&mut` auf die Session
    // frei — wir dürfen sie erneut ausleihen, um die Turn-Antwort zu
    // extrahieren. Siehe Bugfix-Abschnitt der Funktionsdoku oben: nur Items
    // ab `history_len_before` gehören zu DIESEM Turn.
    let reply = latest_final_reply(gateway.session_mut().history(), history_len_before);
    Ok(suppress_if_matches_commentary(
        reply,
        turn_state.last_commentary_text.as_deref(),
    ))
}

/// Wählt die zu enthüllende Turn-Antwort aus den Items, die seit `since_len`
/// neu an `history` angehängt wurden.
///
/// # Beschreibung
/// Reine, terminal- und gateway-freie Funktion — extrahiert aus
/// [`drive_turn_animated`] (siehe dortiger Bugfix-Abschnitt), damit sich die
/// Turn-Eingrenzung ohne die volle async-Turn-Maschinerie testen lässt.
/// Iteriert die Items ab Index `since_len` rückwärts und liefert den
/// sichtbaren Text der ersten [`TurnItem::AssistantMessage`], deren `phase`
/// **nicht** [`harw_types::MessagePhase::Commentary`] ist — eine
/// `Commentary`-Nachricht wurde bereits live über [`handle_turn_event`]
/// gezeigt und darf hier nicht erneut auftauchen. Reasoning-, Tool- und
/// Error-Items werden dabei einfach übersprungen (kein Treffer), nicht als
/// Abbruchkriterium behandelt.
///
/// # Argumente
/// - `history` (`&ConversationHistory`): die vollständige Session-Historie.
/// - `since_len` (`usize`): Anzahl Items, die vor dem aktuellen Turn bereits
///   vorhanden waren (Historie-Länge unmittelbar vor `run_turn`).
///
/// # Rückgabe
/// `Some(text)` der jüngsten passenden Nachricht innerhalb dieses Turns;
/// `None`, wenn der Turn keine solche Nachricht angehängt hat (z. B. eine
/// `Commentary`-Nachricht gefolgt nur von Tool-Aufrufen, oder gar keine neue
/// Assistant-Nachricht).
fn latest_final_reply(history: &ConversationHistory, since_len: usize) -> Option<String> {
    history
        .items()
        .iter()
        .skip(since_len)
        .rev()
        .find_map(|item| match item {
            TurnItem::AssistantMessage(message)
                if message.phase != Some(harw_types::MessagePhase::Commentary) =>
            {
                Some(visible_message_text(&message.content))
            }
            _ => None,
        })
}

/// Zweite Verteidigungslinie gegen eine doppelte Antwortanzeige (siehe
/// [`drive_turn_animated`]s Bugfix-Abschnitt): unterdrückt `reply`, falls er
/// exakt dem zuletzt live gepushten `Commentary`-Text dieses Turns
/// ([`TurnEventState::last_commentary_text`]) entspricht.
///
/// # Argumente
/// - `reply` (`Option<String>`): das Ergebnis von [`latest_final_reply`].
/// - `last_commentary` (`Option<&str>`): `turn_state.last_commentary_text`
///   dieses Turns.
///
/// # Rückgabe
/// `None`, wenn `reply` mit `last_commentary` exakt übereinstimmt; sonst
/// unverändert `reply`.
fn suppress_if_matches_commentary(
    reply: Option<String>,
    last_commentary: Option<&str>,
) -> Option<String> {
    match (reply, last_commentary) {
        (Some(reply), Some(commentary)) if reply == commentary => None,
        (reply, _) => reply,
    }
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

/// Öffnet den Host-Permit-Dialog für eine frisch eingetroffene Frage (B6):
/// dieselbe Zustandsänderung, die auch der `host_permit_prompts.recv()`-Arm
/// von `drive_pauses_to_completion` inline vornimmt, hier als eigenständige,
/// aus [`drive_turn_animated`]s `select!` aufgerufene Funktion.
///
/// # Beschreibung
/// Dieselbe K3-Regel wie bei einer normalen Freigabefrage: eine noch offene
/// ältere Host-Permit-Frage wird nicht still überschrieben, sondern
/// abgelehnt (Ablehnung ist der Default — [`HostPermitPrompt`]s
/// Sicherheitsregel).
///
/// # Argumente
/// - `app` (`&mut ChatApp`): nimmt Dialog und Frage auf.
/// - `prompt` ([`HostPermitPrompt`]): die soeben eingetroffene Frage.
///
/// # Rückgabe
/// Den Zeitpunkt, zu dem der Dialog angezeigt wurde — Grundlage für das
/// Arming-Delay ([`approval_dialog_key_is_armed`]).
fn open_host_permit_prompt(app: &mut ChatApp, prompt: HostPermitPrompt) -> Instant {
    if let Some(stale) = app.pending_host_permit.take() {
        tracing::warn!("tui.host_permit.stale_prompt_closed");
        stale.deny();
    }
    app.pending_host_permit_dialog = Some(build_host_permit_dialog(&prompt));
    app.pending_host_permit = Some(prompt);
    Instant::now()
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
/// Baut den [`ChoiceDialog`] für eine soeben eingetroffene [`HostPermitPrompt`]
/// (Plan „UIA-Shell-Worker und Shell-Modus", Schritt 2).
///
/// # Beschreibung
/// Bietet genau die zwei Varianten aus
/// `docs/design/mediated-process-execution.md` plus eine Ablehnungsoption, in
/// dieser festen Reihenfolge — [`ChoiceAction::Chosen`] liefert damit einen
/// stabilen Index für [`apply_host_permit_decision`]:
/// 0. [`HostPermitVariant::SingleExecution`]
/// 1. [`HostPermitVariant::SessionLease`]
/// 2. Ablehnung
///
/// Vorausgewählt ist [`HostPermitPrompt::preselected_variant`] — der Modus
/// ändert nur diese Vorauswahl, nie die Optionsliste selbst; der Mensch
/// bestätigt in jedem Fall explizit (`Enter`).
///
/// Ist `prompt.worker_definition()` die eingebettete Sandbox-Lease-
/// Worker-Definition ([`harw_tool_shell::SANDBOX_LEASE_WORKER_DEFINITION`]
/// — das Modell-Tool `sandbox-lease` fragt darüber direkt einen Lease an,
/// kein tatsächlicher Befehl läuft), zeigen Titel und Hinweistext eine
/// eigene Formulierung: „Sandbox-Lease angefragt" statt „Host-Ausführung
/// erlauben?", und `prompt.command()` erscheint als „Grund" statt als
/// auszuführender Befehl (B6).
///
/// # Argumente
/// - `prompt` (`&HostPermitPrompt`): die anzuzeigende Frage.
///
/// # Rückgabe
/// Ein einsatzbereiter [`ChoiceDialog`] mit der passenden Vorauswahl.
fn build_host_permit_dialog(prompt: &HostPermitPrompt) -> ChoiceDialog {
    let selected = match prompt.preselected_variant() {
        HostPermitVariant::SingleExecution => 0,
        HostPermitVariant::SessionLease => 1,
    };
    let options = vec![
        HostPermitVariant::SingleExecution.label().to_owned(),
        HostPermitVariant::SessionLease.label().to_owned(),
        "Nein, ablehnen".to_owned(),
    ];
    // Runde 5, Teil N: Host-Mode-Anfrage aus dem Agentenbaum — zeigt, wer
    // fragt (Rolle, Kind-ID, Baum-Pfad), Grund, Befehl und cwd.
    if let Some((title, hint)) = crate::host_permit_dialog::escalation_dialog_text(prompt) {
        return ChoiceDialog::new(title, Some(hint), options).with_selected(selected);
    }
    let is_sandbox_lease =
        prompt.worker_definition() == harw_tool_shell::SANDBOX_LEASE_WORKER_DEFINITION;
    let (title, hint) = if is_sandbox_lease {
        (
            "Sandbox-Lease angefragt",
            format!(
                "Sitzung {} bittet um eine Sandbox-Lease. Grund: {}",
                prompt.session(),
                prompt.command(),
            ),
        )
    } else {
        (
            "Host-Ausführung erlauben?",
            format!(
                "Worker {} verlangt Host-Ausführung in Sitzung {}: {}",
                prompt.worker_definition(),
                prompt.session(),
                prompt.command(),
            ),
        )
    };
    ChoiceDialog::new(title, Some(hint), options).with_selected(selected)
}

/// Setzt eine Entscheidung aus dem Host-Permit-Dialog um (Plan
/// „UIA-Shell-Worker und Shell-Modus", Schritt 2).
///
/// # Beschreibung
/// Übersetzt den nullbasierten Options-Index aus [`build_host_permit_dialog`]
/// in die passende [`HostPermitPrompt`]-Antwort und hängt eine kurze
/// Systemzeile an, die die getroffene Entscheidung festhält (Nutzerzustimmung
/// muss laut Konzept „als Sitzungsereignis persistiert" werden — die
/// permanente Statuszeile `HOST-MODUS AKTIV` bleibt die primäre Anzeige einer
/// laufenden Phase, siehe [`ChatApp::host_mode_active`]).
///
/// # Argumente
/// - `app` (`&mut ChatApp`): nimmt die Systemzeile auf.
/// - `prompt` ([`HostPermitPrompt`]): die zu beantwortende Frage.
/// - `option_index` (`usize`): der gewählte, nullbasierte Options-Index aus
///   [`build_host_permit_dialog`] (`2` und jeder größere Index gelten als
///   Ablehnung — fail-closed statt eines Panics bei einem unerwarteten Index).
fn apply_host_permit_decision(app: &mut ChatApp, prompt: HostPermitPrompt, option_index: usize) {
    let message = match option_index {
        0 => {
            let delivered = prompt.approve(HostPermitVariant::SingleExecution);
            if delivered {
                "Host-Ausführung einmalig freigegeben."
            } else {
                "Host-Ausführung freigegeben, aber die Antwort kam nicht mehr an — der Auftrag ist bereits weitergelaufen."
            }
        }
        1 => {
            let delivered = prompt.approve(HostPermitVariant::SessionLease);
            if delivered {
                "Host-Arbeitsphase freigegeben — HOST-MODUS AKTIV (Strg+H beendet sie)."
            } else {
                "Host-Arbeitsphase freigegeben, aber die Antwort kam nicht mehr an — der Auftrag ist bereits weitergelaufen."
            }
        }
        _ => {
            prompt.deny();
            "Host-Ausführung abgelehnt."
        }
    };
    app.push_line(Role::System, message);
}

fn build_approval_dialog(
    prompt: &ApprovalPrompt,
    app: &ChatApp,
    timeout: Duration,
) -> ApprovalDialog {
    let call = prompt.call();
    // Runde 5, Teil E: mit Auto-Modus-Laufzeit ersetzt das Lern-Angebot
    // (erst ab der dritten gleichartigen Freigabe, Scope Sitzung/Projekt,
    // nie für ALWAYS_ASK_TOOLS oder riskante Muster) das sofortige
    // „nicht mehr fragen“.
    let learning = app
        .runtime()
        .and_then(|rt| rt.auto_mode())
        .map(|auto| auto.learning_offer(call));
    let remember_rule = if learning.is_some() {
        None
    } else if call.name.as_str() == "shell.exec" {
        call.arguments
            .get("command")
            .and_then(|value| value.as_str())
            .and_then(derive_shell_rule)
    } else {
        None
    };
    let dialog = ApprovalDialog::new(ApprovalDialogRequest {
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
    });
    match learning.flatten() {
        Some(offer) => dialog.with_learning_offer(LearnOfferView::from(&offer)),
        None => dialog,
    }
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
async fn apply_approval_decision(
    app: &mut ChatApp,
    pending: PendingApprovalPrompt,
    choice: ApprovalChoice,
) {
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

    // Runde 5, Teil E: angenommenes Lern-Angebot als Allow-Regel anlegen —
    // Sitzung nur im Speicher, Projekt zusätzlich über `/permissions allow
    // … --project` (ConfigWriter), erst jetzt nach der Bestätigung.
    if let ApprovalChoice::ApproveAndLearn { offer, scope } = &choice {
        let message = match app.runtime() {
            Some(rt) => {
                rt.services().allow_rules().add(offer.rule(*scope));
                if let Some(auto) = rt.auto_mode() {
                    auto.forget_learned(&harw_runtime::LearnKey {
                        tool: offer.tool.clone(),
                        pattern: offer.pattern.clone(),
                    });
                }
                if *scope == LearnScope::Project {
                    let command = offer.persist_command(quote_for_synthetic_command);
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
                        tool = %offer.tool,
                        result = %output,
                        "tui.approval.learned_rule_persist_attempted"
                    );
                }
                let scope_text = match scope {
                    LearnScope::Session => "für diese Sitzung",
                    LearnScope::Project => "dauerhaft im Projekt",
                };
                format!("Künftig erlaubt ({scope_text}): {}", offer.display)
            }
            None => format!(
                "„Künftig erlauben“ für {} gilt nur für diesen Aufruf — keine \
                 Laufzeit-Montage zum Speichern verfügbar.",
                offer.display
            ),
        };
        app.push_line(Role::System, message);
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

    // Runde 5, Teil E: manuelle Freigaben zählen für das Lern-Angebot.
    let approved_call = prompt.call().clone();
    let learned = matches!(choice, ApprovalChoice::ApproveAndLearn { .. });
    let (approved, delivered) = match choice {
        ApprovalChoice::Approve
        | ApprovalChoice::ApproveAndRemember(_)
        | ApprovalChoice::ApproveAndAutoMode
        | ApprovalChoice::ApproveAndLearn { .. } => (true, prompt.approve()),
        ApprovalChoice::Reject { reason } => {
            let reason = reason.unwrap_or_else(|| REASON_OPERATOR_REJECTED.to_owned());
            (false, prompt.reject(reason))
        }
    };
    if approved && delivered && !learned {
        if let Some(auto) = app.runtime().and_then(|rt| rt.auto_mode()) {
            auto.record_manual_approval(&approved_call);
        }
    }

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

/// Bricht den laufenden Turn ab und lehnt/verweigert eine offene Freigabe-
/// oder Host-Permit-Frage — die Ctrl+C-Kernlogik innerhalb von
/// [`drive_pauses_to_completion`], solange ein solcher Dialog sichtbar ist
/// (Fix E / Teil 1b; seit Runde 4 bleibt die Eingabe-Warteschlange dabei
/// erhalten und wird nach dem Abbruch ausgeliefert).
///
/// # Beschreibung
/// Vor diesem Fix lehnte Ctrl+C bei offenem Dialog **nur** den Dialog ab —
/// `active_cancel`/`pending_quit` blieben unberührt, der Turn und alle
/// laufenden Kind-Agenten liefen unangetastet weiter. Diese Funktion
/// ergänzt **zusätzlich** zum bestehenden Ablehnen/Verweigern denselben
/// kooperativen Abbruch (`active_cancel.cancel(CancelReason::User)`) und
/// dieselbe zweistufige Beenden-Scharfstellung ([`ChatApp::pending_quit`])
/// wie [`handle_busy_event`] ohne offenen Dialog. Bereits eingereihte
/// Eingaben (`deferred_input`/`pending_turns`) bleiben erhalten und werden
/// nach dem Abbruch ausgeliefert. Aus dem
/// `tokio::select!`-Zweig von [`drive_pauses_to_completion`] extrahiert,
/// damit die Kernlogik ohne den vollen Ereignis-Loop testbar ist.
///
/// # Argumente
/// - `app` (`&mut ChatApp`): trägt `active_cancel`, `pending_quit`, beide
///   Eingabe-Warteschlangen sowie die Dialogzustände.
/// - `pending` (`&mut Option<PendingApprovalPrompt>`): die noch offene
///   Freigabefrage der Schleife, falls vorhanden; wird konsumiert.
/// - `dialog_shown_at` (`&mut Option<Instant>`): Arming-Uhr des
///   Freigabe-Panels; wird wie beim bestehenden Ablehnen zurückgesetzt.
/// - `host_permit_shown_at` (`&mut Option<Instant>`): dieselbe Uhr für den
///   Host-Permit-Dialog.
///
/// # Nebenläufigkeit
/// Single-task, hält keinen Lock über den `await`-Punkt in
/// [`apply_approval_decision`] hinaus.
async fn cancel_turn_and_reject_open_dialogs(
    app: &mut ChatApp,
    pending: &mut Option<PendingApprovalPrompt>,
    dialog_shown_at: &mut Option<Instant>,
    host_permit_shown_at: &mut Option<Instant>,
) {
    if let Some(cancel) = &app.active_cancel {
        cancel.cancel(CancelReason::User);
    }
    app.pending_quit = Some(QuitArm {
        label: "Ctrl+C",
        at: Instant::now(),
    });
    // Bereits abgeschickte, aber noch nicht ausgelieferte Eingaben bleiben
    // stehen: `run_loop` liefert sie an der nächsten Turn-Grenze aus.
    if !app.deferred_input.is_empty() || !app.pending_turns.is_empty() {
        app.queue_kept_at = Some(Instant::now());
    }
    // Bestehendes Ablehnen/Verweigern des offenen Dialogs bleibt
    // zusätzlich bestehen, wird nicht ersetzt.
    if let Some(open) = pending.take() {
        apply_approval_decision(
            app,
            open,
            ApprovalChoice::Reject {
                reason: Some(REASON_OPERATOR_CANCELLED.to_owned()),
            },
        )
        .await;
    }
    app.pending_approval_dialog = None;
    *dialog_shown_at = None;
    if let Some(prompt) = app.pending_host_permit.take() {
        prompt.deny();
    }
    app.pending_host_permit_dialog = None;
    *host_permit_shown_at = None;
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
/// - `host_permit_prompts` (`&mut HostPermitPromptReceiver`): Fragekanal
///   dieser Wurzelsitzung (siehe
///   [`harw_runtime::RuntimeAssembly::take_host_permit_prompts`]); analog zu
///   `approvals` gepollt, sonst liefe jede Host-Permit-Frage in den Timeout.
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
    host_permit_prompts: &mut HostPermitPromptReceiver,
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
    // Analog zu `dialog_shown_at`/`approvals_open`, aber für die
    // Host-Permit-Frage (`app.pending_host_permit`/`app.pending_host_permit_dialog`).
    let mut host_permit_shown_at: Option<Instant> = None;
    let mut host_permit_prompts_open = true;
    let mut input_open = true;

    let (session, store, model) = gateway.borrow_turn_ctx();
    let drive = approval_driver.drive_to_completion(session, model, store, children, outcome);
    tokio::pin!(drive);

    loop {
        tokio::select! {
            result = &mut drive => {
                let finished = result.map_err(tui_error_from_approval_driver)?;
                // Runde 5, Teil O: `drive_to_completion` reicht terminale
                // Ausgänge (Abbruch, Kürzung, Ablehnung, Fehler) unverändert
                // durch — nur eine Pause darf hier nie mehr ankommen. Vorher
                // ließ ein Esc-Abbruch nach einer Kind-Pause Debug-Builds hier
                // in Panik geraten.
                debug_assert!(
                    !matches!(
                        finished,
                        TurnOutcome::AwaitingApproval { .. } | TurnOutcome::AwaitingChild { .. }
                    ),
                    "drive_to_completion must never return a pause"
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
                    draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                }
                // Dieselbe Regel für eine noch offene Host-Permit-Frage.
                if let Some(prompt) = app.pending_host_permit.take() {
                    prompt.deny();
                    app.pending_host_permit_dialog = None;
                    draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
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
                        // Runde 5, Teil O: die Wurzel-Frage hat Vorrang; eine
                        // offene Kind-Frage wartet, bis der Dialog frei ist.
                        child_approvals::yield_to_root(app);
                        app.pending_approval_dialog = Some(build_approval_dialog(&prompt, app, timeout));
                        dialog_shown_at = Some(Instant::now());
                        pending = Some(PendingApprovalPrompt { prompt, tool_cell });
                        draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                    }
                    None => {
                        tracing::warn!("tui.approval.prompt_channel_ended");
                        approvals_open = false;
                    }
                }
            }
            // Runde 5, Teil B: `host.sudo_exec` fragt im eigenen Fenster
            // (`crate::sudo_dialog`), auch während einer Pause.
            maybe_sudo = app.sudo.recv(), if app.sudo.is_listening() => {
                if crate::sudo_dialog::accept_prompt(app, maybe_sudo) {
                    draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                }
            }
            // Runde 5, Teil F: Plan-Fenster auch während einer Pause.
            maybe_plan = app.plan_ui.recv(), if app.plan_ui.is_listening() => {
                if plan_mode::accept_request(app, maybe_plan) {
                    draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                }
            }
            // Runde 5, Teil O: Freigabe-Fragen von Kind-Agenten, auch während
            // der Eltern-Turn auf ein Kind wartet.
            maybe_child = app.child_approvals.recv(), if app.child_approvals.is_listening() => {
                if child_approvals::accept(app, maybe_child) {
                    draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                }
            }
            maybe_host_prompt = host_permit_prompts.recv(), if host_permit_prompts_open => {
                match maybe_host_prompt {
                    Some(prompt) => {
                        tracing::info!(
                            session = prompt.session(),
                            worker = prompt.worker_definition(),
                            "tui.host_permit.prompt_shown"
                        );
                        // Dieselbe K3-Regel wie bei `approvals.recv()` oben:
                        // eine noch offene ältere Host-Permit-Frage wird nicht
                        // still überschrieben, sondern abgelehnt.
                        if let Some(stale) = app.pending_host_permit.take() {
                            tracing::warn!("tui.host_permit.stale_prompt_closed");
                            stale.deny();
                        }
                        app.pending_host_permit_dialog = Some(build_host_permit_dialog(&prompt));
                        app.pending_host_permit = Some(prompt);
                        host_permit_shown_at = Some(Instant::now());
                        draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                    }
                    None => {
                        tracing::warn!("tui.host_permit.prompt_channel_ended");
                        host_permit_prompts_open = false;
                    }
                }
            }
            maybe_event = tui_rx.recv(), if input_open => {
                match maybe_event {
                    // Runde 5, Teil B: ein offenes sudo-Fenster fängt JEDES
                    // Ereignis ab (vor Freigabe-Panel, Host-Permit und Busy-Pfad).
                    Some(event) if app.sudo.is_open() => {
                        if crate::sudo_dialog::route_event(app, event) {
                            draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                        }
                    }
                    // Runde 5, Teil F: ein offenes Plan-Fenster fängt jedes
                    // Ereignis ab (nach dem sudo-Fenster).
                    Some(event) if app.plan_ui.is_open() => {
                        if plan_mode::route_event(app, event) {
                            draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                        }
                    }
                    // Runde 5, Teil O: eine offene Kind-Freigabe ist modal.
                    Some(event) if child_approvals::is_open(app) => {
                        match child_approvals::route_event(app, event) {
                            Ok(redraw) => {
                                if redraw {
                                    draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                                }
                            }
                            Err(event) => {
                                let outcome = handle_busy_event(app, event);
                                settle_busy_outcome(guard, app, spinner, outcome)?;
                            }
                        }
                    }
                    Some(event) if pending.is_none() && app.pending_host_permit.is_none() => {
                        let outcome = handle_busy_event(app, event);
                        settle_busy_outcome(guard, app, spinner, outcome)?;
                    }
                    Some(TuiEvent::Key(key)) => {
                        // Strg+Pos1/Strg+Ende gehen bei nicht-leerer Eingabe
                        // an den Composer statt an den Transkript-Scroll
                        // (siehe `scroll_claims_key`).
                        if scroll_claims_key(app, &key)
                            && app.scroll.handle_key(key, app.last_history_total_lines() as usize,
                            app.last_history_visible_rows() as usize) == ScrollAction::Redraw {
                            draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                            continue;
                        }
                        // Ctrl+C bleibt fail-safe und lehnt sofort ab,
                        // unabhängig vom Arming-Delay des Panels — dieselbe
                        // Sicherheitsinvariante wie zuvor, jetzt für beide
                        // Fragearten.
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && matches!(key.code, KeyCode::Char('c' | 'C'))
                        {
                            // Fix E (Teil 1b): siehe
                            // `cancel_turn_and_reject_open_dialogs` — bricht
                            // zusätzlich zum bestehenden Ablehnen/Verweigern
                            // des offenen Dialogs auch den Turn kooperativ ab.
                            cancel_turn_and_reject_open_dialogs(
                                app,
                                &mut pending,
                                &mut dialog_shown_at,
                                &mut host_permit_shown_at,
                            )
                            .await;
                            draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                            continue;
                        }
                        if pending.is_some() {
                            let since_shown = dialog_shown_at.map_or(Duration::ZERO, |shown| shown.elapsed());
                            let armed = approval_dialog_key_is_armed(key, since_shown);
                            let Some(dialog) = app.pending_approval_dialog.as_mut() else {
                                continue;
                            };
                            match dialog.handle_key(key, armed) {
                                DialogAction::Stay => {
                                    let outcome = queue_busy_key(app, key);
                                    settle_busy_outcome(guard, app, spinner, outcome)?;
                                }
                                DialogAction::ToggleDetails => {
                                    draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                                }
                                DialogAction::Decided(choice) => {
                                    if let Some(open) = pending.take() {
                                        apply_approval_decision(app, open, choice).await;
                                    }
                                    app.pending_approval_dialog = None;
                                    dialog_shown_at = None;
                                    draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                                }
                            }
                        } else if app.pending_host_permit.is_some() {
                            // Dasselbe Arming-Delay wie beim Freigabe-Panel
                            // (`approval_dialog_key_is_armed`), hier auf den
                            // generischen `ChoiceDialog` angewandt: solange
                            // nicht scharfgeschaltet, zählt keine Taste.
                            let since_shown = host_permit_shown_at.map_or(Duration::ZERO, |shown| shown.elapsed());
                            if !approval_dialog_key_is_armed(key, since_shown) {
                                continue;
                            }
                            let Some(dialog) = app.pending_host_permit_dialog.as_mut() else {
                                continue;
                            };
                            match dialog.handle_key(key) {
                                ChoiceAction::Stay => {
                                    // Runde 5, Teil H: der Host-Permit-Dialog ist
                                    // modal — keine Taste erreicht den Composer.
                                    draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                                }
                                ChoiceAction::Cancel => {
                                    if let Some(prompt) = app.pending_host_permit.take() {
                                        prompt.deny();
                                    }
                                    app.pending_host_permit_dialog = None;
                                    host_permit_shown_at = None;
                                    app.push_line(Role::System, "Host-Ausführung abgelehnt.");
                                    draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                                }
                                ChoiceAction::Chosen(index) => {
                                    if let Some(prompt) = app.pending_host_permit.take() {
                                        apply_host_permit_decision(app, prompt, index);
                                        // Register „CSI-Sicherheitsnetz und
                                        // Paste-Platzhalter", Punkt 7: siehe
                                        // Gegenstück in `drive_turn_animated`.
                                        guard.reassert_terminal_modes();
                                    }
                                    app.pending_host_permit_dialog = None;
                                    host_permit_shown_at = None;
                                    draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                                }
                            }
                        }
                    }
                    Some(TuiEvent::Draw) | Some(TuiEvent::Resize(_, _)) => {
                        draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                    }
                    Some(TuiEvent::Mouse(mouse)) => {
                        // Maus-Ereignisse laufen über denselben Abschluss wie
                        // Tasten (`settle_busy_outcome`).
                        let outcome = handle_busy_event(app, TuiEvent::Mouse(mouse));
                        settle_busy_outcome(guard, app, spinner, outcome)?;
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
                        if let Some(prompt) = app.pending_host_permit.take() {
                            prompt.deny();
                            app.pending_host_permit_dialog = None;
                            host_permit_shown_at = None;
                        }
                    }
                }
            }
            maybe_turn_event = turn_event_rx.recv() => {
                if let Some(event) = maybe_turn_event {
                    if handle_turn_event(app, turn_state, event) {
                        draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                    }
                    // Register „CSI-Sicherheitsnetz und Paste-Platzhalter",
                    // Punkt 7: ein abgeschlossener `shell.exec`-Aufruf kann
                    // Terminal-Modi beschädigt zurückgelassen haben (siehe
                    // `handle_turn_event`).
                    if app.take_needs_terminal_reassert() {
                        guard.reassert_terminal_modes();
                    }
                }
            }
            // Runde 4, Teil H: Ergebnisse nebenläufiger Busy-Befehle und
            // -Datenabrufe; der Turn läuft währenddessen ungebremst weiter.
            Some(done) = app.busy_jobs.recv() => {
                apply_busy_job_done(app, done);
                start_busy_work(app);
                draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
            }
            _ = tokio::time::sleep(SPINNER_INTERVAL) => {
                spinner.tick();
                app.drain_agent_events();
                // Runde 5, Teil O: aufgegebene Kind-Fragen schließen,
                // zurückgestellte wieder zeigen.
                child_approvals::poll(app);
                // Ansichten füllen sich auch während eines Turns.
                start_busy_work(app);
                draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
            }
        }
    }
}

/// Ergebnis eines Tastendrucks/Ereignisses während eines laufenden Turns
/// (`queue_busy_key`/`handle_busy_event`).
///
/// # Beschreibung
/// Runde 4, Teil H: ein fertig abgeschickter Befehl wird eingestuft
/// ([`route_busy_command`]). Busy-sichere TUI-lokale Befehle wirken sofort
/// ([`Self::Local`]); `Immediate`-/`Staged`-Operationen landen in
/// [`ChatApp::busy_jobs`] und werden vom Aufrufer über
/// [`settle_busy_outcome`] als eigene Tasks gestartet
/// ([`Self::Dispatch`]) — der `select!`-Arm wartet nie auf sie. Alles
/// andere wird wie bisher als Paste+Enter bis zum Turn-Ende zurückgestellt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BusyKeyOutcome {
    /// Kein sichtbarer Zustand geändert — kein Redraw nötig.
    Idle,
    /// Sichtbarer Zustand geändert — Redraw nötig.
    Redraw,
    /// Ein busy-sicherer TUI-lokaler Befehl (Overlay, Picker, Panel,
    /// Systemzeile …) wurde sofort angewendet.
    Local,
    /// Ein Befehl der angegebenen Klasse (`Immediate`/`Staged`) wurde in
    /// [`ChatApp::busy_jobs`] eingereiht; der Aufrufer startet ihn.
    Dispatch(BusyAvailability),
}

/// Startet eingereihte Busy-Befehle und ausstehende Datenabrufe als eigene
/// Tasks und zeichnet neu, wenn `outcome` das verlangt.
///
/// # Beschreibung
/// Gemeinsamer Abschluss aller Busy-Tastenpfade in [`drive_turn_animated`]
/// und [`drive_pauses_to_completion`]. Kehrt sofort zurück — die Befehle
/// laufen nebenläufig, ihre Ergebnisse kommen über
/// [`BusyJobs::recv`] zurück ([`apply_busy_job_done`]).
///
/// # Fehler
/// [`TuiError::Io`] beim Zeichnen.
fn settle_busy_outcome(
    guard: &mut TerminalGuard,
    app: &mut ChatApp,
    spinner: &Spinner,
    outcome: BusyKeyOutcome,
) -> Result<(), TuiError> {
    if outcome == BusyKeyOutcome::Idle {
        return Ok(());
    }
    start_busy_work(app);
    draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))
}

/// Startet alle eingereihten Busy-Befehle und ausstehenden Datenabrufe.
///
/// # Nebenläufigkeit
/// Muss in einer Tokio-Runtime laufen (`tokio::spawn`); kehrt sofort zurück.
fn start_busy_work(app: &mut ChatApp) {
    if !app.busy_jobs.queued().is_empty() {
        let dispatch = app.busy_dispatch();
        app.busy_jobs.start_queued(&dispatch);
    }
    spawn_busy_fetches(app);
}

/// Wendet ein Ergebnis aus [`ChatApp::busy_jobs`] an.
///
/// # Beschreibung
/// - Befehl: Ausgabe als Systemzeilen mit „(während Turn)“ (bei `Staged`
///   zusätzlich „gilt ab nächstem Turn“, aber nur bei Erfolg); danach wie
///   im Idle-Pfad eine offene
///   Ansicht neu laden und die Werkbank bei `/workbench …` als veraltet
///   markieren.
/// - Datenabruf: an Ansicht bzw. Werkbank ([`apply_fetch_result`]).
fn apply_busy_job_done(app: &mut ChatApp, done: BusyJobDone) {
    match done {
        BusyJobDone::Command {
            command,
            text,
            succeeded,
        } => {
            tracing::debug!(command = %command.raw, succeeded, "tui.busy.command_finished");
            app.push_lines(busy_queue::command_result_lines(&command, &text, succeeded));
            app.queue_overlay_refresh();
            if is_workbench_command(&command.raw) {
                app.workbench.mark_stale();
            }
        }
        BusyJobDone::Fetch { target, result } => apply_fetch_result(app, target, result),
    }
}

/// `true` für lokale Abfänge, die während eines laufenden Turns gefahrlos
/// sofort wirken (Runde 4, Teil H).
///
/// # Beschreibung
/// Sofort: Ansichten, Picker, Agentenbaum, Panels, ausführliche Anzeige,
/// Systemzeilen und die reine Anzeige-Umbenennung. Zurückgestellt bleiben
/// Sitzungsauswahl (beendet den Loop), das Leeren des Transkripts (würde
/// laufende Zellen verlieren) und `Rewrite` (dessen Ziel neu eingestuft
/// werden müsste; läuft nach dem Turn über den normalen Pfad).
fn is_busy_safe_intercept(intercept: &LocalIntercept) -> bool {
    matches!(
        intercept,
        LocalIntercept::OpenOverlay(_)
            | LocalIntercept::OpenModelPicker(_)
            | LocalIntercept::OpenUiaWorkerPicker
            | LocalIntercept::OpenUiaPickerThenWorkers
            | LocalIntercept::OpenEffortChoice(_)
            | LocalIntercept::OpenAgentTree
            | LocalIntercept::TogglePanel(_)
            | LocalIntercept::ToggleVerbose
            | LocalIntercept::System(_)
            | LocalIntercept::RenameSession(_)
            // Runde 5, Teil I: reine Anzeige-Umschaltung, sofort wirksam.
            | LocalIntercept::ChildStream(_)
            // Runde 5, Teil L: `/btw` läuft neben dem Turn, ohne ihn zu berühren.
            | LocalIntercept::Btw(_)
            // Runde 5, Teil K: Liste bzw. Abbruch eines Hintergrund-Agenten.
            | LocalIntercept::BackgroundAgents(_)
    ) || matches!(
        // Runde 5, Teil F: `/plan edit` braucht das Terminal und wartet.
        intercept,
        LocalIntercept::Plan(command) if command.busy_safe()
    )
}

/// Stellt eine `/command`-Zeile bis zum Turn-Ende zurück (Paste+Enter in
/// `deferred_input`): nach dem Turn läuft sie über denselben autorisierten
/// Command-Kanal wie interaktiv eingegebene Befehle.
fn defer_busy_command(app: &mut ChatApp, raw: String) {
    app.deferred_input.push_back(TuiEvent::Paste(raw));
    app.deferred_input.push_back(TuiEvent::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )));
}

/// Stuft eine während eines Turns abgeschickte `/command`-Zeile ein und
/// wendet sie an (Runde 4, Teil H).
///
/// # Beschreibung
/// 1. Lokaler Abfang ([`local_intercept_for`]): busy-sicher
///    ([`is_busy_safe_intercept`]) → sofort anwenden, [`BusyKeyOutcome::Local`];
///    `Chat` → in `pending_turns`; sonst zurückstellen.
/// 2. Sonst [`busy_availability_for`]: `Immediate`/`Staged` → in
///    [`ChatApp::busy_jobs`] einreihen, [`BusyKeyOutcome::Dispatch`];
///    `DeferredUntilTurnEnd` → zurückstellen ([`defer_busy_command`]).
///
/// Nie wird hier die Session selbst verändert: `Staged`-Befehle merken ihre
/// Änderung nur im Controller vor, angewendet wird sie an der nächsten
/// Turn-Grenze.
fn route_busy_command(app: &mut ChatApp, raw: String) -> BusyKeyOutcome {
    if let Some(intercepted) = local_intercept_for(app, &raw) {
        if is_busy_safe_intercept(&intercepted) {
            // Busy-sichere Abfänge senden nichts auf den Bus (nur `Rewrite`
            // täte das); der Wegwerf-Kanal fängt es trotzdem sicher ab.
            let (scratch, _scratch_rx) = crate::events::harw_event_channel();
            let _ = apply_local_intercept(app, intercepted, &scratch);
            return BusyKeyOutcome::Local;
        }
        if let LocalIntercept::Chat(text) = intercepted {
            app.pending_turns.push_back(text);
            return BusyKeyOutcome::Redraw;
        }
        defer_busy_command(app, raw);
        return BusyKeyOutcome::Redraw;
    }
    let class = busy_availability_for(&app.command_registry, &raw);
    if class.runs_during_turn() {
        app.busy_jobs.queue(BusyCommand { raw, class });
        return BusyKeyOutcome::Dispatch(class);
    }
    defer_busy_command(app, raw);
    BusyKeyOutcome::Redraw
}

/// Leitet eine Taste während eines laufenden Turns an das offene Overlay
/// weiter (Runde 4, Teil H).
///
/// # Beschreibung
/// Nutzt denselben [`handle_overlay_key`] wie der Leerlauf, aber mit einem
/// Wegwerf-Bus: Befehle, die Ansichten und Picker absenden (z. B. `/model
/// switch <id>` aus dem Modell-Picker, `/effort high` aus der
/// Effort-Auswahl), werden über [`route_busy_command`] neu eingestuft —
/// `Staged` merkt den Controller sofort vor (nie `apply_to_session` mitten im
/// Turn), alles Zurückgestellte wartet bis zum Turn-Ende.
fn route_busy_overlay_key(app: &mut ChatApp, key: KeyEvent) -> BusyKeyOutcome {
    let (scratch, mut scratch_rx) = crate::events::harw_event_channel();
    handle_overlay_key(app, key, &scratch);
    let mut outcome = BusyKeyOutcome::Redraw;
    while let Ok(event) = scratch_rx.try_recv() {
        match event {
            HarwEvent::Command(raw) => {
                let routed = route_busy_command(app, raw);
                if matches!(routed, BusyKeyOutcome::Dispatch(_)) {
                    outcome = routed;
                }
            }
            HarwEvent::Submit(text) => app.pending_turns.push_back(text),
            HarwEvent::SystemMessage(message) => app.push_lines(
                message
                    .split('\n')
                    .map(|line| Line::from(line.to_owned()))
                    .collect(),
            ),
            HarwEvent::Quit => {
                tracing::debug!("tui.busy.overlay_quit_ignored");
            }
        }
    }
    outcome
}

/// Holt die zuletzt eingereihte Nachricht zurück in den (leeren) Composer
/// (Alt+↑ während eines Turns, Runde 4, Teil H).
///
/// # Rückgabe
/// `true`, wenn eine Nachricht zurückgeholt wurde.
fn recall_last_pending_turn(app: &mut ChatApp) -> bool {
    if !app.input.is_empty() {
        return false;
    }
    let Some(text) = app.pending_turns.pop_back() else {
        return false;
    };
    app.input.insert_paste(&text);
    app.sync_popup();
    true
}

/// Bearbeitet den Composer während eines laufenden Turns. Chat-Zeilen gehen
/// direkt in die FIFO (`pending_turns`, sichtbar im Warteschlangen-Block über
/// dem Composer); eine fertig abgeschickte `/command`-Zeile stuft
/// [`route_busy_command`] ein (lokal sofort, `Immediate`/`Staged`
/// nebenläufig, sonst zurückgestellt).
fn queue_busy_key(app: &mut ChatApp, key: KeyEvent) -> BusyKeyOutcome {
    match app.input.handle_key(key) {
        InputAction::Submit(text) => {
            app.remember_input(&text);
            app.mention_popup = None;
            match classify_line(&text) {
                LineAction::Chat(text) => {
                    app.pending_turns.push_back(text);
                    BusyKeyOutcome::Redraw
                }
                LineAction::Command(raw) => route_busy_command(app, raw),
                LineAction::Quit | LineAction::Ignore | LineAction::System(_) => {
                    BusyKeyOutcome::Redraw
                }
            }
        }
        InputAction::Redraw => {
            app.sync_popup();
            BusyKeyOutcome::Redraw
        }
        InputAction::Passthrough => BusyKeyOutcome::Idle,
    }
}

/// Processes navigation immediately while preserving typing for the next prompt.
///
/// # Ctrl+C-Hard-Interrupt (Welle 4c, Punkt 5/6)
/// Ein abgelaufener Beenden-Hinweis wird zuerst verworfen — dasselbe
/// Äquivalent zur Ablaufprüfung im Idle-Pfad von [`run_loop`], nur hier für
/// den Busy-Pfad. Ein erster Ctrl+C-Druck bricht den laufenden Turn
/// kooperativ ab (`active_cancel.cancel`) und scharft zusätzlich
/// [`ChatApp::pending_quit`]; ein zweiter Druck derselben Taste innerhalb von
/// [`QUIT_HINT_WINDOW`] setzt [`ChatApp::hard_quit_requested`], das
/// [`run_loop`] direkt nach dem laufenden `run_turn_streaming(...)`-Aufruf
/// prüft und dann sofort beendet — derselbe Ausgang wie [`HarwEvent::Quit`]
/// im Idle-Pfad.
fn handle_busy_event(app: &mut ChatApp, event: TuiEvent) -> BusyKeyOutcome {
    // Abgelaufenen Beenden-Hinweis verwerfen — Pendant zur Ablaufprüfung im
    // Idle-Pfad von `run_loop`, hier für den Busy-Pfad (Welle 4c, Punkt 5).
    if let Some(arm) = app.pending_quit {
        if arm.at.elapsed() > QUIT_HINT_WINDOW {
            app.pending_quit = None;
        }
    }
    // Runde 5, Teil L: Esc bricht zuerst eine wartende `/btw`-Nebenfrage ab —
    // der Turn läuft weiter; erst ein weiteres Esc unterbricht ihn.
    if let TuiEvent::Key(key) = &event
        && app.btw_esc_cancels(key)
    {
        return BusyKeyOutcome::Redraw;
    }
    if let TuiEvent::Key(key) = &event {
        let is_ctrl_c = key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'C'));
        if !is_ctrl_c {
            // Runde 4, Teil H: Ansichten (F1/F6/…) öffnen auch während eines
            // Turns, und ein offenes Overlay (Hilfe, Picker, Wissensbrowser,
            // Agentenbaum …) bekommt seine Tasten — Esc schließt es, statt den
            // Turn zu unterbrechen. Ctrl+C bleibt der Turn-Abbruch.
            if handle_view_hotkey(app, *key).is_some() {
                return BusyKeyOutcome::Local;
            }
            if app.overlay.is_some() {
                return route_busy_overlay_key(app, *key);
            }
            // Runde 5, Teil G: offene `/`- und `@`-Popups bekommen Hoch/
            // Runter/Tab/Esc/Enter wie im Idle-Pfad (`popup_keys`). Ein
            // offenes sudo-Fenster hat die Taste vorher schon abgefangen
            // (Aufrufer). Enter ohne Übernahme sendet über `queue_busy_key`
            // nach den Busy-Klassen ab.
            if let Some(redraw) = popup_keys::handle_popup_key(app, *key) {
                return if redraw {
                    BusyKeyOutcome::Redraw
                } else {
                    BusyKeyOutcome::Idle
                };
            }
            // Alt+↑ holt die zuletzt eingereihte Nachricht zum Bearbeiten
            // zurück in den leeren Composer.
            if key.code == KeyCode::Up
                && key.modifiers == KeyModifiers::ALT
                && recall_last_pending_turn(app)
            {
                return BusyKeyOutcome::Redraw;
            }
        }
        // Panels bleiben auch während eines Turns bedienbar — gerade dann
        // will man den Agenten zusehen.
        if !(key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | 'C')))
            && let Some(redraw) = handle_panel_key(app, *key)
        {
            return if redraw {
                BusyKeyOutcome::Redraw
            } else {
                BusyKeyOutcome::Idle
            };
        }
    }
    let total = app.last_history_total_lines() as usize;
    let rows = app.last_history_visible_rows() as usize;
    match event {
        TuiEvent::Key(key)
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('c' | 'C')) =>
        {
            // Zweiter Druck innerhalb des Fensters: harter Abbruch statt
            // eines weiteren kooperativen Cancels.
            if matches!(
                app.pending_quit,
                Some(QuitArm {
                    label: "Ctrl+C",
                    ..
                })
            ) {
                app.hard_quit_requested = true;
                return BusyKeyOutcome::Redraw;
            }
            if interrupt_turn(app) {
                app.pending_quit = Some(QuitArm {
                    label: "Ctrl+C",
                    at: Instant::now(),
                });
                return BusyKeyOutcome::Redraw;
            }
            BusyKeyOutcome::Idle
        }
        TuiEvent::Mouse(mouse) => {
            if app.scroll.handle_mouse(mouse, total, rows) == ScrollAction::Redraw {
                BusyKeyOutcome::Redraw
            } else {
                BusyKeyOutcome::Idle
            }
        }
        // Strg+Pos1/Strg+Ende gehen bei nicht-leerer Eingabe an den
        // Composer statt an den Transkript-Scroll (siehe `scroll_claims_key`).
        TuiEvent::Key(key)
            if scroll_claims_key(app, &key)
                && app.scroll.handle_key(key, total, rows) == ScrollAction::Redraw =>
        {
            BusyKeyOutcome::Redraw
        }
        // CyclePermissionMode (Standard Shift+Tab) gilt sofort und wird nicht
        // in `deferred_input` eingereiht, damit der Zyklus nach Turn-Ende
        // nicht ein zweites Mal läuft.
        TuiEvent::Key(key)
            if app.key_bindings.action_for(&key) == Some(KeyAction::CyclePermissionMode) =>
        {
            app.cycle_permission_stage(false);
            BusyKeyOutcome::Redraw
        }
        // Runde 5: ToggleToolCells (Standard Ctrl+O) wirkt auch während eines
        // laufenden Turns sofort — auch auf die Live-Blöcke der Kind-Agenten
        // (Teil I) — und wird nicht in die Busy-Queue eingereiht.
        TuiEvent::Key(key)
            if app.key_bindings.action_for(&key) == Some(KeyAction::ToggleToolCells) =>
        {
            if app.toggle_tool_cells() {
                BusyKeyOutcome::Redraw
            } else {
                BusyKeyOutcome::Idle
            }
        }
        TuiEvent::Draw | TuiEvent::Resize(_, _) => BusyKeyOutcome::Redraw,
        TuiEvent::Paste(text) => {
            // Große Pastes landen nur als kompakter Platzhalter im Composer
            // (siehe `InputEditor::insert_paste`-Doku) — derselbe Pfad wie
            // im Idle-Fall oben in `run_loop`.
            app.input
                .insert_paste(&text.replace("\r\n", "\n").replace('\r', "\n"));
            app.sync_popup();
            BusyKeyOutcome::Redraw
        }
        // Runde 4 (Nutzerwunsch): Esc unterbricht den laufenden Turn wie ein
        // erster Ctrl+C-Druck — nur ohne Beenden-Scharfstellung, damit Esc
        // nie die App schließt. Ein offenes Popup schließt Esc zuerst (über
        // `queue_busy_key`); der Composer-Text bleibt stehen.
        TuiEvent::Key(key)
            if key.code == KeyCode::Esc
                && key.modifiers.is_empty()
                && !app.has_popup()
                && app.active_cancel.is_some() =>
        {
            // Runde 5, Teil O: laufen Kinder, die der Abbruch mitrisse,
            // fragt das erste Esc nur nach (`turn_safety`).
            if !turn_safety::esc_should_interrupt(app) {
                return BusyKeyOutcome::Redraw;
            }
            if interrupt_turn(app) {
                BusyKeyOutcome::Redraw
            } else {
                BusyKeyOutcome::Idle
            }
        }
        TuiEvent::Key(key) => queue_busy_key(app, key),
    }
}

/// Bricht den laufenden Turn kooperativ ab (Ctrl+C und Esc im Busy-Pfad).
///
/// # Beschreibung
/// Während des Turns abgeschickte Nachrichten und Befehle bleiben in
/// `pending_turns`/`deferred_input` stehen: sie sind abgeschickt, also gehen
/// sie raus — `run_loop` liefert sie direkt nach dem Abbruch an der
/// Turn-Grenze aus (Runde 4, Nutzerwunsch). Beide Hinweise sind nur
/// transiente Statuszeilen-Hinweise (siehe `render_viewport`).
///
/// # Rückgabe
/// `true`, wenn ein laufender Turn abgebrochen wurde.
fn interrupt_turn(app: &mut ChatApp) -> bool {
    let Some(cancel) = &app.active_cancel else {
        return false;
    };
    cancel.cancel(CancelReason::User);
    app.cancel_requested_at = Some(Instant::now());
    if !app.deferred_input.is_empty() || !app.pending_turns.is_empty() {
        app.queue_kept_at = Some(Instant::now());
    }
    true
}

/// Übernimmt die finale Antwort als Zelle.
///
/// # Beschreibung
/// Der Text wurde (bei streamenden Providern) bereits live als transiente
/// Vorschau gezeigt (`ChatApp::live_stream`); hier wird er durch genau eine
/// finale [`AssistantHistoryCell`] ersetzt und ein Frame gezeichnet.
///
/// # Argumente
/// - `guard` (`&mut TerminalGuard`): Terminal-Guard zum Zeichnen der Frames.
/// - `app` (`&mut ChatApp`): Chat-Zustand; erhält die finale Zelle.
/// - `reply` (`&str`): die vollständige Modell-Antwort.
///
/// # Fehler
/// [`TuiError::Io`] beim Zeichnen.
async fn reveal_reply(
    guard: &mut TerminalGuard,
    app: &mut ChatApp,
    reply: &str,
) -> Result<(), TuiError> {
    // Vollständige Antwort in genau EINE finalisierte Zelle. Der live
    // gestreamte Vorschautext (`live_stream`) war nur transient und wird hier
    // durch die finale Zelle ersetzt — so entsteht keine Doppelanzeige.
    app.clear_live_stream();
    app.push_line(Role::Assistant, reply);
    draw_viewport(guard, app, &Spinner::new(), None)?;
    Ok(())
}

/// Tasten für das fokussierte Explorer-Panel.
fn handle_explorer_key(app: &mut ChatApp, key: KeyEvent) -> bool {
    use crate::explorer_panel::ExplorerAction;
    app.ensure_explorer();
    let Some(panel) = app.explorer.as_mut() else {
        return false;
    };
    match panel.handle_key(key) {
        ExplorerAction::None => {
            // Esc ohne offenen Filter/Vorschau gibt den Fokus an den Chat.
            if key.code == KeyCode::Esc {
                app.panels.focus = crate::panes::PaneFocus::Chat;
                app.panels.maximized = false;
                return true;
            }
            false
        }
        ExplorerAction::Redraw | ExplorerAction::Rebuild => true,
        ExplorerAction::InsertPath(path) => {
            let needs_space =
                !app.input.is_empty() && !app.input.text().ends_with(char::is_whitespace);
            if needs_space {
                app.input.insert_str(" ");
            }
            app.input.insert_str(&format!("@{path} "));
            app.panels.focus = crate::panes::PaneFocus::Chat;
            app.panels.maximized = false;
            true
        }
    }
}

/// Zeichnet das Explorer-Panel.
fn render_explorer_panel(
    app: &ChatApp,
    area: Rect,
    buf: &mut ratatui::buffer::Buffer,
    theme: style::Theme,
) {
    if let Some(panel) = &app.explorer {
        panel.render(
            area,
            buf,
            theme,
            app.panels.focus == crate::panes::PaneFocus::Explorer,
        );
    }
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

    // Vollflächige Overlays (Session-Picker, `/export`-, Modell/Provider-,
    // Effort-Auswahl) ersetzen die gesamte Viewport (Plan Schritt 6/7) —
    // `Clear` erst, sonst bliebe Chat-Text unter dem Overlay stehen
    // (dasselbe Muster wie beim `/command`-Popup weiter unten).
    // Runde 5, Teil B: ein offenes sudo-Fenster (fängt jede Eingabe ab) darf
    // nie unter einem Overlay verborgen sein — das Overlay pausiert solange.
    match app.overlay.as_ref().filter(|_| !app.sudo.is_open()) {
        Some(Overlay::AgentTree(tree)) => {
            frame.render_widget(Clear, area);
            tree.render(area, frame.buffer_mut(), theme, &app.agent_tree_rows());
            return;
        }
        Some(Overlay::SessionPicker(picker)) => {
            frame.render_widget(Clear, area);
            picker.render(area, frame.buffer_mut(), &theme);
            return;
        }
        Some(Overlay::ModelSwitch(picker)) => {
            frame.render_widget(Clear, area);
            picker.render(area, frame.buffer_mut(), theme);
            return;
        }
        Some(Overlay::ExportChoice(dialog)) | Some(Overlay::EffortChoice { dialog, .. }) => {
            frame.render_widget(Clear, area);
            dialog.render(area, frame.buffer_mut(), theme);
            return;
        }
        Some(Overlay::View(view)) => {
            frame.render_widget(Clear, area);
            view.render(area, frame.buffer_mut(), theme);
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
    // Runde 5, Teil B: ein offenes sudo-Fenster hat Vorrang vor jeder Frage.
    let sudo_height = app.sudo.desired_height(area.width, theme);
    // Runde 5, Teil F: danach ein offenes Plan-Fenster.
    let plan_height = app.plan_ui.desired_height(area.width, theme);
    let input_height = match (
        &app.pending_approval_dialog,
        &app.pending_host_permit_dialog,
    ) {
        _ if sudo_height.is_some() => sudo_height.unwrap_or_default(),
        _ if plan_height.is_some() => plan_height.unwrap_or_default(),
        (Some(dialog), _) => dialog.desired_height(area.width),
        // Eine Host-Permit-Frage kann nur auftreten, wenn keine normale
        // Werkzeugfreigabe offen ist (siehe `drive_pauses_to_completion`:
        // `y`/`n` gehen zuerst an eine offene `ApprovalDialog`-Frage) — die
        // beiden Panels ersetzen den Composer deshalb nie gleichzeitig.
        (None, Some(dialog)) => dialog.desired_height(),
        (None, None) => {
            let input_line_count = app.input.visible_lines(input_width).len().clamp(1, 8) as u16;
            input_line_count + 2
        }
    };

    // Runde 4, Teil H: während eines Turns abgeschickte Nachrichten und
    // zurückgestellte Befehle stehen sichtbar über dem Composer, bis sie
    // ausgeliefert werden (auch nach einem Abbruch).
    let queue_lines = busy_queue::queue_block_lines(
        app.pending_turns.iter().map(String::as_str),
        app.deferred_input.iter().filter_map(|event| match event {
            TuiEvent::Paste(text) => Some(text.as_str()),
            _ => None,
        }),
        area.width,
    );
    // Der Block darf den Verlauf nie ganz verdrängen.
    let queue_height = (queue_lines.len() as u16).min(area.height.saturating_sub(input_height + 4));

    // History | Warteschlange | permanente Statuszeile | Eingabe. Der
    // Sicherheitsmodus muss sichtbar bleiben und darf nicht vom Verlauf
    // verdrängt werden.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(queue_height),
            Constraint::Length(1),
            Constraint::Length(input_height),
        ])
        .split(area);
    let queue_area = chunks[1];
    let status_area = chunks[2];
    let input_area = chunks[3];
    if queue_height > 0 {
        frame.render_widget(
            Paragraph::new(queue_lines).style(Style::default().fg(style::border_color(theme))),
            queue_area,
        );
    }
    // Seitenpanels (Explorer links, Agenten rechts) teilen sich die obere
    // Fläche mit dem Verlauf; auf schmalen Terminals bleibt nur der Chat.
    let pane_areas = crate::panes::split(chunks[0], &app.panels);
    if let Some(agents_area) = pane_areas.agents {
        if let Some(detail) = app.agent_detail.as_ref() {
            app.agent_monitor.render_agent_detail(
                &detail.agent,
                agents_area,
                frame.buffer_mut(),
                detail.scroll,
                detail.show_reasoning,
            );
        } else {
            crate::agent_monitor::render_agents_panel(
                &app.agent_monitor,
                agents_area,
                frame.buffer_mut(),
                theme,
                app.panels.focus == crate::panes::PaneFocus::Agents,
            );
        }
    }
    if let Some(explorer_area) = pane_areas.explorer {
        render_explorer_panel(app, explorer_area, frame.buffer_mut(), theme);
    }
    if let Some(workbench_area) = pane_areas.workbench {
        app.workbench.render(
            workbench_area,
            frame.buffer_mut(),
            theme,
            app.panels.focus == crate::panes::PaneFocus::Workbench,
        );
    }
    let history_area = pane_areas
        .chat
        .unwrap_or(Rect::new(chunks[0].x, chunks[0].y, 0, 0));
    let permission = match app.current_permission_stage() {
        PermissionCycleStage::Ask => "Ask",
        PermissionCycleStage::Auto => "Auto",
        PermissionCycleStage::Full => "Full Access",
        PermissionCycleStage::Plan => "Plan",
    };
    let usage = app.display_usage();
    let cached = usage.cached_tokens.unwrap_or(0);
    let cache_write = usage.cache_write_tokens.unwrap_or(0);
    let cache_suffix = if cached > 0 || cache_write > 0 {
        format!(
            ", cache {} / neu {}",
            crate::agent_monitor::human_tokens(cached),
            crate::agent_monitor::human_tokens(cache_write)
        )
    } else {
        String::new()
    };
    // Kontextfenster der Wurzel als Balken (aus `ContextUpdated`).
    let context_suffix = app
        .agent_monitor
        .agent(app.session_id().as_str())
        .and_then(|root| Some((root.context_percent()?, root.context_window)))
        .map(|(pct, window)| {
            // Schwelle der nächsten Kompaktierung (aus `ContextUpdated`), falls bekannt.
            let threshold = SessionController::context_usage(app.session_controller.as_ref())
                .and_then(|usage| usage.threshold_tokens)
                .map(|tokens| {
                    format!(
                        ", verdichtet ab {}",
                        crate::agent_monitor::human_tokens(tokens)
                    )
                })
                .unwrap_or_default();
            format!(
                " | ctx {} {pct}% / {}{threshold}",
                crate::agent_monitor::gauge(pct, 8),
                crate::agent_monitor::human_tokens(window)
            )
        })
        .unwrap_or_default();
    let explorer_suffix = if app
        .explorer
        .as_ref()
        .is_some_and(crate::explorer_panel::ExplorerPanel::is_indexing)
    {
        " | Explorer indiziert…"
    } else {
        ""
    };
    // Runde 5, Teil K: „· N im Hintergrund".
    let agents_suffix = background_agents::status_suffix(app, app.agent_monitor.active_count());
    // Spinner-Präfix: solange ein Turn läuft, zeigt die Statuszeile das
    // animierte Glyph plus Label — vorher wurde `spinner`/`quit_hint` zwar
    // berechnet und weitergereicht, aber nie tatsächlich gerendert. Die
    // Statuszeile ist die einzige dauerhaft sichtbare Zeile außerhalb der
    // History, deshalb landet die Animation hier statt am Input-Präfix.
    let spinner_prefix = if spinner.is_active() {
        format!("{} denkt… | ", spinner.glyph())
    } else {
        String::new()
    };
    // Transienter "Abbruch angefordert …"-Hinweis (siehe `cancel_requested_at`
    // an [`ChatApp`]) — bewusst NICHT über `push_line`, damit er nie in der
    // permanenten Verlaufshistorie landet und automatisch verschwindet,
    // sobald der Turn beendet ist oder ein neuer Turn startet.
    let cancel_suffix = if app.cancel_requested_at.is_some() {
        " · Abbruch angefordert…"
    } else {
        ""
    };
    // Transienter Hinweis, dass nach einem Ctrl+C-Abbruch die nicht-leere
    // Eingabe-Warteschlange (`deferred_input`/`pending_turns`) ausgeliefert
    // wird — analog zu `cancel_suffix` oben, keine dauerhafte
    // Verlaufszeile, verschwindet an denselben Stellen wieder
    // (`queue_kept_at`-Reset an [`ChatApp`]).
    let queue_cleared_suffix = if app.queue_kept_at.is_some() {
        " · Warteschlange wird gesendet"
    } else {
        ""
    };
    // `quit_hint` (z. B. "Ctrl+C"/"Ctrl+D") signalisiert die Scharfstellung
    // des zweistufigen Beenden-Hinweises (`QuitArm`) und wurde bisher
    // stillschweigend verworfen.
    let quit_suffix = match quit_hint {
        Some(label) => format!(" · nochmal {label} zum Beenden"),
        None => String::new(),
    };
    // Eingereihte Nachrichten zeigt der Warteschlangen-Block über dem
    // Composer (siehe oben); die Statuszeile trägt nur noch ihre Anzahl.
    let queue_suffix = match app.pending_turns.len() {
        0 => String::new(),
        n => format!(" · {n} wartend"),
    };
    let tool_suffix = if app.has_collapsed_tool_cells() {
        " · Ctrl+O: Werkzeug-/Reasoning-Details"
    } else {
        ""
    };
    // Kontexthinweis für das fokussierte Agenten-Panel.
    let agents_hint = if app.panels.focus == crate::panes::PaneFocus::Agents {
        match app.agent_detail.as_ref() {
            Some(detail) => {
                let entries = app
                    .agent_monitor
                    .trace(&detail.agent)
                    .map_or(0, crate::agent_monitor::AgentTrace::len);
                format!(
                    " · {entries} Spureinträge · Esc: Liste | j/k/Bild↑↓: scrollen | g/G: Anfang/Ende | r: Reasoning"
                )
            }
            None => " · Enter: Agentendetails".to_owned(),
        }
    } else {
        String::new()
    };
    let pending_permission_suffix = app
        .pending_permission_stage()
        .map(|_| " · Freigabemodus wird nach dem Turn übernommen")
        .unwrap_or("");
    let mode_segment = status_line::mode_segment(
        app.active_mode(),
        app.current_approval(),
        app.pending_permission_stage().is_some(),
    );
    let (live_provider, live_model) = app.live_model();
    let model_segment = status_line::model_segment(
        live_provider.as_deref(),
        live_model.as_deref(),
        STATUS_MODEL_MAX_CHARS,
    );
    let status = format!(
        " {spinner_prefix}Shift+Tab: {permission} | {mode_segment} | {model_segment} | Σ Tokens: {} (in {}, out {}{cache_suffix}){context_suffix}{agents_suffix}{explorer_suffix}{cancel_suffix}{queue_cleared_suffix}{quit_suffix}{queue_suffix}{tool_suffix}{agents_hint}{pending_permission_suffix}",
        crate::agent_monitor::human_tokens(usage.total()),
        crate::agent_monitor::human_tokens(usage.prompt_tokens()),
        crate::agent_monitor::human_tokens(usage.output_tokens),
    );
    // Solange eine Host-Arbeitsphase läuft, muss die Statuszeile das gemäß
    // `docs/design/mediated-process-execution.md` („permanent und
    // unübersehbar `HOST-MODUS AKTIV`") in einer eigenen, hervorgehobenen
    // Warnfarbe zeigen — ein einfacher String-Suffix in derselben Farbe wie
    // der Rest der Zeile wäre zu leicht zu übersehen.
    // Runde 5, Teil P: feste Goal-Marke vorne, solange ein Goal aktiv ist.
    let mut goal_spans = app.goal_status_spans(theme, status_area.width);
    if app.host_mode_active() {
        goal_spans.extend([
            Span::styled(status, Style::default().fg(style::border_color(theme))),
            Span::styled(
                " · HOST-MODUS AKTIV (Strg+H beendet)",
                Style::default()
                    .fg(style::warning_color(theme))
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        frame.render_widget(Paragraph::new(Line::from(goal_spans)), status_area);
    } else if app.current_permission_stage() == PermissionCycleStage::Plan {
        // Runde 5, Teil F: deutliche Plan-Marke in eigener Farbe vorne.
        let mut spans = vec![crate::plan_dialog::plan_status_span(theme)];
        spans.append(&mut goal_spans);
        spans.push(Span::styled(
            status,
            Style::default().fg(style::border_color(theme)),
        ));
        frame.render_widget(Paragraph::new(Line::from(spans)), status_area);
    } else if !goal_spans.is_empty() {
        goal_spans.push(Span::styled(
            status,
            Style::default().fg(style::border_color(theme)),
        ));
        frame.render_widget(Paragraph::new(Line::from(goal_spans)), status_area);
    } else {
        frame.render_widget(
            Paragraph::new(status).style(Style::default().fg(style::border_color(theme))),
            status_area,
        );
    }

    // ── History ──────────────────────────────────────────────────────
    // Alle Zellen zu einem flachen Zeilen-Vec zusammenführen.
    let width = history_area.width;
    let mut all_lines: Vec<Line<'static>> = app
        .cells
        .iter()
        .flat_map(|cell| cell.display_lines(width, theme))
        .collect();
    // Transient: live gestreamtes Reasoning/Text der laufenden Runde.
    if !app.live_reasoning.is_empty() {
        all_lines.extend(live_reasoning_lines(&app.live_reasoning, width, theme));
    }
    if !app.live_stream.is_empty() {
        for (index, text) in app.live_stream.lines().enumerate() {
            let prefix = if index == 0 { "● " } else { "  " };
            all_lines.push(Line::styled(
                format!("{prefix}{}", crate::sanitize::sanitize_inline(text)),
                style::assistant_style(theme),
            ));
        }
        all_lines.push(Line::styled("  ▍", style::dim_style(theme)));
    }

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
            let popup_height = popup
                .preferred_height(POPUP_MAX_ROWS)
                .min(history_area.height);
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

    // `@`-Erwähnungs-Popup (nur mit Treffern; schließt das Befehls-Popup
    // aus, siehe `ChatApp::sync_mention_popup`).
    if let Some(popup) = app.mention_popup.as_ref().filter(|popup| !popup.is_empty()) {
        let popup_height = POPUP_MAX_ROWS.min(history_area.height);
        if popup_height > 0 {
            let popup_area = Rect::new(
                history_area.x,
                history_area.y + history_area.height - popup_height,
                history_area.width,
                popup_height,
            );
            frame.render_widget(Clear, popup_area);
            popup.render(popup_area, frame.buffer_mut(), theme);
        }
    }

    // ── Eingabe / Freigabe-Panel ─────────────────────────────────────
    // Solange eine Freigabefrage offen ist, ersetzt das `ApprovalDialog` den
    // Composer vollständig (Plan Schritt 3); der Rest des Layouts (History,
    // Status) bleibt unverändert. Kein Text-Cursor in diesem Fall — das Panel
    // wird über Pfeiltasten/Ziffern bedient, nicht getippt.
    // Runde 5, Teil B: das sudo-Fenster ersetzt den Composer vor allen
    // anderen Fragen; kein Text-Cursor (das Passwort wird nie angezeigt).
    if app.sudo.render(input_area, frame.buffer_mut(), theme) {
        return;
    }
    // Runde 5, Teil F: Plan-Freigabe (Plan über dem Verlauf, Optionen statt
    // Composer), Plan-Vorschlag und `ask_user`-Auswahl.
    if app
        .plan_ui
        .render(input_area, history_area, frame.buffer_mut(), theme)
    {
        return;
    }
    if let Some(dialog) = &app.pending_approval_dialog {
        dialog.render(input_area, frame.buffer_mut(), &theme);
        return;
    }
    // Dieselbe Composer-Ersetzung für eine offene Host-Permit-Frage (Plan
    // „UIA-Shell-Worker und Shell-Modus", Schritt 2) — nur erreichbar, wenn
    // keine `ApprovalDialog`-Frage offen ist (siehe Kommentar bei
    // `input_height`).
    if let Some(dialog) = &app.pending_host_permit_dialog {
        dialog.render(input_area, frame.buffer_mut(), theme);
        return;
    }

    // Plan Teil F: beginnt der getippte Text mit `!`, rendert der Composer
    // im Shell-Modus (eigene Akzentfarbe für Rahmen und Prompt, eigener
    // Titel/Hinweis) — `is_shell_mode_input` ist eine reine Prädikatsfunktion
    // auf dem noch nicht abgeschickten Text, siehe dort.
    let shell_mode = is_shell_mode_input(app.input.text());
    let prompt_style = if shell_mode {
        style::shell_mode_style(theme)
    } else {
        Style::default()
    };
    let mut input_lines: Vec<Line<'static>> = Vec::new();
    for (index, segment) in app.input.visible_lines(input_width).iter().enumerate() {
        if index == 0 {
            input_lines.push(Line::from(vec![
                Span::styled("› ", prompt_style),
                Span::raw(segment.clone()),
            ]));
        } else {
            input_lines.push(Line::from(format!("  {segment}")));
        }
    }
    let (cursor_row, cursor_col) = app.input.cursor_position(input_width);
    let input_rows = input_area.height.saturating_sub(2) as usize;
    let input_top = cursor_row.saturating_sub(input_rows.saturating_sub(1));
    // Runde 5, Teil F: im Plan-Modus zeigt der leere Composer den Hinweis
    // „Plan-Modus – es wird nichts verändert“ (Rahmen/Titel in Plan-Farbe).
    let plan_mode_composer =
        !shell_mode && app.current_permission_stage() == PermissionCycleStage::Plan;
    if plan_mode_composer && app.input.text().is_empty() {
        input_lines = vec![crate::plan_dialog::plan_placeholder_line(theme)];
    }
    let (title_text, title_style, border_style) = if shell_mode {
        (
            " Shell-Modus · Enter führt aus · Esc/Backspace am Anfang verlässt ",
            style::shell_mode_style(theme),
            Style::default().fg(style::shell_mode_color(theme)),
        )
    } else if plan_mode_composer {
        crate::plan_dialog::plan_composer_chrome(theme)
    } else {
        (
            " harw ",
            style::selected_style(theme),
            Style::default().fg(style::border_color(theme)),
        )
    };
    let input_widget = Paragraph::new(input_lines.into_iter().skip(input_top).collect::<Vec<_>>())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(title_text, title_style))
                .border_style(border_style),
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
    use crate::test_support::{TestError, TestResult, ctx};
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
    fn test_sandbox() -> TestResult<SandboxSpec> {
        use harw_authority::{Permission, PermissionSet, WorkspaceRegistration, WorkspaceRegistry};
        use harw_types::{TenantId, WorkspaceId};

        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("harw-tui-app-test-{}-{}", std::process::id(), id));
        std::fs::create_dir_all(root.join("workspace")).map_err(ctx("temp workspace dir"))?;
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("tui-test"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )
        .map_err(ctx("workspace registry build"))?;
        let binding = registry
            .resolve(
                &TenantId::from_str("tui-test"),
                &WorkspaceId::from_str("workspace"),
            )
            .map_err(ctx("resolve workspace binding"))?;
        Ok(SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy([Permission::ReadWorkspace, Permission::WriteWorkspace]),
        ))
    }

    /// Runde 5, Teil B: solange das sudo-Fenster offen ist, erreicht keine
    /// Taste und kein Paste den Composer, die Busy-Warteschlange oder die
    /// Eingabe-Historie; Verlauf und Export-Einträge enthalten das Passwort
    /// nie, und nach Enter geht genau eine Antwort an die Ausführung.
    #[tokio::test]
    async fn sudo_dialog_swallows_every_key_and_paste_and_never_logs_the_password() -> TestResult {
        let mut app = ChatApp::new(Vec::new(), test_sandbox()?, SessionId::new());
        let (prompt, answer) = harw_tool_shell::SudoPrompt::new(
            "s1".to_owned(),
            "uia-shell-worker".to_owned(),
            vec!["apt-get".to_owned(), "update".to_owned()],
            PathBuf::from("/workspace"),
            "Paketlisten".to_owned(),
            false,
        );
        // Bereits scharf: das Fenster ist seit zwei Sekunden offen.
        let Some(shown) = Instant::now().checked_sub(Duration::from_secs(2)) else {
            return Ok(());
        };
        assert!(app.sudo.open(prompt, shown).is_none());
        // Die Historie kann aus einer persistierten Datei vorbelegt sein;
        // entscheidend ist, dass der Dialog nichts hinzufügt.
        let history_before = app.input.history().len();
        for c in "sehr-geheim".chars() {
            crate::sudo_dialog::route_event(
                &mut app,
                TuiEvent::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)),
            );
        }
        crate::sudo_dialog::route_event(&mut app, TuiEvent::Paste("-paste".to_owned()));
        for code in [KeyCode::PageUp, KeyCode::F(6), KeyCode::Home, KeyCode::End] {
            crate::sudo_dialog::route_event(
                &mut app,
                TuiEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)),
            );
        }
        assert!(app.input.text().is_empty(), "Composer bleibt leer");
        assert_eq!(
            app.input.history().len(),
            history_before,
            "keine neue Eingabe-Historie"
        );
        assert!(
            !app.input
                .history()
                .iter()
                .any(|entry| entry.contains("geheim")),
            "das Passwort steht nie in der Historie"
        );
        assert!(app.deferred_input.is_empty(), "keine Busy-Warteschlange");
        assert!(app.pending_turns.is_empty());
        assert!(app.overlay.is_none(), "keine Ansicht per Hotkey");
        assert!(app.sudo.is_open());

        crate::sudo_dialog::route_event(
            &mut app,
            TuiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        );
        assert!(!app.sudo.is_open());
        assert!(matches!(
            answer.into_answer().await,
            Some(harw_tool_shell::SudoAnswer::Password {
                remember: false,
                ..
            })
        ));
        let exported = format!("{:?}", app.export_entries);
        assert!(!exported.contains("geheim"), "{exported}");
        assert!(exported.contains("apt-get update"), "{exported}");
        Ok(())
    }

    /// Baut einen `ChatApp`-Testzustand mit leerer Adapter-Pipeline und einer
    /// frischen Test-Sandbox. Für Tests, die nur Verlauf/Popup/Scroll prüfen
    /// (nicht die Command-Adapter-Pipeline selbst — dafür siehe
    /// `command_exec.rs`).
    ///
    /// # Beschreibung
    /// `ChatApp::new` baut `command_registry` aus der (hier leeren)
    /// Adapter-Pipeline (`CommandRegistry::from_command_adapters`) — für
    /// Popup-/Tab-Tests wird die Registry deshalb im Anschluss durch
    /// [`CommandRegistry::built_in()`] ersetzt, damit `/`-Präfixe echte
    /// Treffer liefern statt eines leeren, sofort wieder geschlossenen Popups.
    pub(super) fn test_chat_app() -> TestResult<ChatApp> {
        let mut app = ChatApp::new(Vec::new(), test_sandbox()?, SessionId::new());
        app.command_registry = CommandRegistry::built_in().map_err(ctx("built_in"))?;
        Ok(app)
    }

    /// Enter im Agenten-Panel öffnet die Detailansicht (Panel maximiert),
    /// Tasten scrollen/schalten dort, Esc kehrt zur Liste zurück, ohne den
    /// Fokus an den Chat abzugeben, und stellt den Vollbild-Zustand wieder her.
    #[test]
    fn agents_panel_enter_opens_detail_and_esc_closes_it() -> TestResult {
        let mut app = test_chat_app()?;
        app.agent_monitor
            .seed_usage("agent-1", "explorer", TokenUsage::default());
        app.panels.agents_visible = true;
        app.panels.focus = crate::panes::PaneFocus::Agents;
        assert!(!app.panels.maximized);

        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert_eq!(handle_panel_key(&mut app, key(KeyCode::Enter)), Some(true));
        let detail = app
            .agent_detail
            .clone()
            .ok_or(TestError::Missing("agent detail"))?;
        assert_eq!(detail.agent.as_str(), "agent-1");
        assert_eq!(detail.scroll, 0);
        assert!(app.panels.maximized, "Detailansicht maximiert das Panel");

        assert_eq!(
            handle_panel_key(&mut app, key(KeyCode::Char('k'))),
            Some(true)
        );
        assert_eq!(handle_panel_key(&mut app, key(KeyCode::PageUp)), Some(true));
        assert_eq!(app.agent_detail.as_ref().map(|d| d.scroll), Some(11));
        assert_eq!(
            handle_panel_key(&mut app, key(KeyCode::Char('j'))),
            Some(true)
        );
        assert_eq!(app.agent_detail.as_ref().map(|d| d.scroll), Some(10));
        assert_eq!(handle_panel_key(&mut app, key(KeyCode::Home)), Some(true));
        assert_eq!(app.agent_detail.as_ref().map(|d| d.scroll), Some(u16::MAX));
        assert_eq!(handle_panel_key(&mut app, key(KeyCode::End)), Some(true));
        assert_eq!(app.agent_detail.as_ref().map(|d| d.scroll), Some(0));
        let before = app.agent_detail.as_ref().map(|d| d.show_reasoning);
        assert_eq!(
            handle_panel_key(&mut app, key(KeyCode::Char('r'))),
            Some(true)
        );
        assert_ne!(app.agent_detail.as_ref().map(|d| d.show_reasoning), before);

        assert_eq!(handle_panel_key(&mut app, key(KeyCode::Esc)), Some(true));
        assert!(app.agent_detail.is_none());
        assert_eq!(app.panels.focus, crate::panes::PaneFocus::Agents);
        assert!(!app.panels.maximized, "vorheriger Vollbild-Zustand zurück");

        // Ein zweites Esc gibt den Fokus wie bisher an den Chat zurück.
        assert_eq!(handle_panel_key(&mut app, key(KeyCode::Esc)), Some(true));
        assert_eq!(app.panels.focus, crate::panes::PaneFocus::Chat);
        Ok(())
    }

    /// Runde 5: Ctrl+O wirkt auch während eines laufenden Turns sofort und
    /// landet nicht im Composer bzw. in der Busy-Queue.
    #[test]
    fn toggle_tool_cells_works_while_busy() -> TestResult {
        let mut app = test_chat_app()?;
        app.push_reasoning_cell("erste Zeile\nzweite Zeile".to_owned(), None);
        assert!(app.has_collapsed_tool_cells());
        let ctrl_o = TuiEvent::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert!(matches!(
            handle_busy_event(&mut app, ctrl_o),
            BusyKeyOutcome::Redraw
        ));
        assert!(!app.has_collapsed_tool_cells());
        assert!(app.input.text().is_empty(), "Ctrl+O darf nichts eintippen");
        Ok(())
    }

    /// Ctrl+O klappt auch Reasoning-Zellen auf und zu.
    #[test]
    fn toggle_tool_cells_expands_reasoning_cells() -> TestResult {
        let mut app = test_chat_app()?;
        app.push_reasoning_cell("erste Zeile\nzweite Zeile".to_owned(), None);
        assert!(app.has_collapsed_tool_cells());
        assert!(app.toggle_tool_cells());
        assert!(!app.has_collapsed_tool_cells());
        Ok(())
    }

    /// Live-Reasoning zeigt höchstens drei umbrochene Zeilen, die neuesten.
    #[test]
    fn live_reasoning_shows_last_three_wrapped_lines() {
        let text = (0..40)
            .map(|index| format!("wort{index}"))
            .collect::<Vec<_>>()
            .join(" ");
        let lines = live_reasoning_lines(&text, 24, style::Theme::Dark);
        assert_eq!(lines.len(), 3);
        let rendered: Vec<String> = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect()
            })
            .collect();
        assert!(rendered[0].starts_with("  ∴ "));
        assert!(rendered[2].contains("wort39"));
    }

    /// Baut einen Session-Store-Sidecar mit festem `created_at`/`last_opened_at`
    /// unter `root` für `session_id` (Aufgabe 2: Export-Datum bei Resume).
    fn save_session_meta_with_created_at(
        root: &std::path::Path,
        session_id: &SessionId,
        created_at: jiff::Timestamp,
    ) -> TestResult {
        std::fs::create_dir_all(root).map_err(ctx("temp session store root"))?;
        let meta = harw_session_store::meta::SessionMeta {
            version: harw_session_store::meta::SESSION_META_VERSION,
            session_id: session_id.clone(),
            title: None,
            title_source: harw_session_store::meta::TitleSource::None,
            created_at,
            last_opened_at: created_at,
            cwd: None,
            project_root: None,
            project_key: None,
            first_user_message: None,
            turns: 0,
            usage_rounds: 0,
            total_usage: harw_types::TokenUsage::default(),
            drift_events: std::collections::BTreeMap::new(),
        };
        harw_session_store::meta::save(root, &meta).map_err(ctx("save session meta sidecar"))?;
        Ok(())
    }

    /// Aufgabe 2 (Plan `recursive-cooking-lobster.md` Teil F): eine über
    /// [`ChatApp::with_session_store_root`] gesetzte Wurzel wird auch OHNE
    /// jeden Titel-Job-Kontext ausgewertet — anders als vor Aufgabe 2, wo nur
    /// `title_job_context.session_store_root` (nur bei aktiver
    /// Titelerzeugung gesetzt) zur Verfügung stand.
    #[test]
    fn apply_session_store_started_at_uses_own_field_without_title_job_context() -> TestResult {
        let mut app = test_chat_app()?;
        let root =
            std::env::temp_dir().join(format!("harw-tui-app-test-started-at-{}", app.session_id()));
        let created_at =
            jiff::Timestamp::from_second(1_700_000_000).map_err(ctx("valid timestamp"))?;
        save_session_meta_with_created_at(&root, app.session_id(), created_at)?;

        app = app.with_session_store_root(root.clone());
        apply_session_store_started_at(&mut app);
        std::fs::remove_dir_all(&root).ok();

        assert!(app.title_job_context.is_none());
        assert_eq!(
            app.export_started_at,
            Some(created_at.as_second().to_string())
        );
        Ok(())
    }

    /// Ohne eigenes `session_store_root` fällt [`apply_session_store_started_at`]
    /// weiterhin auf `title_job_context.session_store_root` zurück (Rückwärts-
    /// kompatibilität mit dem Verhalten vor Aufgabe 2).
    #[test]
    fn apply_session_store_started_at_falls_back_to_title_job_context() -> TestResult {
        let mut app = test_chat_app()?;
        let root = std::env::temp_dir().join(format!(
            "harw-tui-app-test-started-at-fallback-{}",
            app.session_id()
        ));
        let created_at =
            jiff::Timestamp::from_second(1_650_000_000).map_err(ctx("valid timestamp"))?;
        save_session_meta_with_created_at(&root, app.session_id(), created_at)?;

        app = app.with_title_job_context(TitleJobContext {
            provider: Arc::new(ScriptedModel::new(Vec::new())),
            session_store_root: root.clone(),
            title_model: None,
            config: Arc::new(harw_config::ResolvedConfig::default()),
        });
        assert!(app.session_store_root.is_none());
        apply_session_store_started_at(&mut app);
        std::fs::remove_dir_all(&root).ok();

        assert_eq!(
            app.export_started_at,
            Some(created_at.as_second().to_string())
        );
        Ok(())
    }

    /// Ist weder das eigene Feld noch der Titel-Job-Kontext bekannt (kein
    /// `/resume` in diesem Lauf konfiguriert), bleibt `export_started_at`
    /// unverändert beim TUI-Startzeitpunkt aus `ChatApp::new`.
    #[test]
    fn apply_session_store_started_at_leaves_tui_start_when_nothing_known() -> TestResult {
        let mut app = test_chat_app()?;
        let original = app.export_started_at.clone();

        apply_session_store_started_at(&mut app);

        assert_eq!(app.export_started_at, original);
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Plan Teil F: `!`-Modus wie in Claude Code — Composer-Moduserkennung,
    // Folge-Turn-Nachricht (inkl. Kappung) und Einreihung.
    // -----------------------------------------------------------------------

    #[test]
    fn is_shell_mode_input_true_for_bang_prefix() -> TestResult {
        assert!(is_shell_mode_input("!ls -la"));
        Ok(())
    }

    #[test]
    fn is_shell_mode_input_true_for_double_bang() -> TestResult {
        assert!(is_shell_mode_input("!!"));
        Ok(())
    }

    #[test]
    fn is_shell_mode_input_false_for_plain_text() -> TestResult {
        assert!(!is_shell_mode_input("ls -la"));
        assert!(!is_shell_mode_input(""));
        assert!(!is_shell_mode_input("/status"));
        Ok(())
    }

    #[test]
    fn truncate_chars_with_marker_leaves_short_text_unchanged() -> TestResult {
        assert_eq!(truncate_chars_with_marker("hallo", 8000), "hallo");
        assert_eq!(truncate_chars_with_marker("", 8000), "");
        Ok(())
    }

    /// Kappung ist zeichengrenzen-sicher: ein 8001 Zeichen langer,
    /// mehrbytiger Text (Umlaute) darf nicht mitten in einem UTF-8-Codepunkt
    /// getrennt werden — `.chars().take(n)` garantiert das strukturell.
    #[test]
    fn truncate_chars_with_marker_caps_long_multibyte_text_with_marker() -> TestResult {
        let text: String = "ä".repeat(8001);
        let truncated = truncate_chars_with_marker(&text, 8000);
        assert!(truncated.ends_with("\n[gekürzt]"));
        let body = truncated
            .strip_suffix("\n[gekürzt]")
            .ok_or(TestError::Missing("marker suffix"))?;
        assert_eq!(body.chars().count(), 8000);
        assert!(body.chars().all(|c| c == 'ä'));
        Ok(())
    }

    #[test]
    fn build_shell_turn_message_includes_command_and_exit_code() -> TestResult {
        let message = build_shell_turn_message("ls -la", 0, "total 0\n");
        assert!(message.starts_with("Ich habe `!ls -la` ausgeführt (Exit 0):"));
        assert!(message.contains("```text\ntotal 0\n\n```"));
        Ok(())
    }

    #[test]
    fn build_shell_turn_message_truncates_long_output() -> TestResult {
        let output: String = "x".repeat(SHELL_TURN_OUTPUT_MAX_CHARS + 500);
        let message = build_shell_turn_message("yes | head", 0, &output);
        assert!(message.contains("[gekürzt]"));
        // Nur die (gekürzte) Ausgabe zählt gegen die Obergrenze, nicht die
        // umgebende Nachricht (Header + Codeblock-Markierungen).
        let fence_body = message
            .split("```text\n")
            .nth(1)
            .and_then(|rest| rest.rsplit_once("\n```"))
            .map(|(body, _)| body)
            .ok_or(TestError::Missing("fenced code block"))?;
        assert_eq!(
            fence_body.chars().count(),
            SHELL_TURN_OUTPUT_MAX_CHARS + "\n[gekürzt]".chars().count()
        );
        Ok(())
    }

    fn shell_outcome(command: &str, exit_code: i64, combined_output: &str) -> ShellRunOutcome {
        ShellRunOutcome {
            command: command.to_owned(),
            exit_code,
            combined_output: combined_output.to_owned(),
        }
    }

    /// Ist gerade kein Turn unterwegs (`submitted == None`, der Normalfall
    /// direkt nach einem `!`-Befehl im Idle-Pfad), wird der Folge-Turn sofort
    /// zum nächsten zu treibenden Turn — derselbe Pfad wie `HarwEvent::Submit`.
    #[test]
    fn queue_shell_follow_up_turn_submits_immediately_when_idle() -> TestResult {
        let mut app = test_chat_app()?;
        let mut submitted: Option<String> = None;

        queue_shell_follow_up_turn(
            &mut app,
            &mut submitted,
            shell_outcome("echo hi", 0, "hi\n"),
        );

        let text = submitted.ok_or(TestError::Missing("turn queued immediately"))?;
        assert!(text.starts_with("Ich habe `!echo hi` ausgeführt (Exit 0):"));
        assert!(app.pending_turns.is_empty());
        assert_eq!(app.last_shell_command.as_deref(), Some("echo hi"));
        assert_eq!(
            app.pending_turn_user_cell_override.as_deref(),
            Some("↳ Ausgabe von !echo hi an den Agenten übergeben")
        );
        Ok(())
    }

    /// Läuft bereits ein Turn (`submitted.is_some()`), wird der Folge-Turn
    /// stattdessen an `app.pending_turns` gehängt statt den belegten Platz zu
    /// überschreiben.
    #[test]
    fn queue_shell_follow_up_turn_queues_when_turn_already_submitted() -> TestResult {
        let mut app = test_chat_app()?;
        let mut submitted: Option<String> = Some("bereits abgeschickte Nachricht".to_owned());

        queue_shell_follow_up_turn(&mut app, &mut submitted, shell_outcome("pwd", 1, "err\n"));

        assert_eq!(submitted.as_deref(), Some("bereits abgeschickte Nachricht"));
        assert_eq!(app.pending_turns.len(), 1);
        assert!(
            app.pending_turns
                .front()
                .ok_or(TestError::Missing("queued follow-up turn"))?
                .starts_with("Ich habe `!pwd` ausgeführt (Exit 1):")
        );
        Ok(())
    }

    #[test]
    fn export_request_marker_reads_format_options_and_path() -> TestResult {
        let request = export_request_from_data(&json!({
            "kind": "export.request",
            "format": "json",
            "include_tool_calls": false,
            "include_reasoning_summary": true,
            "path": "exports/session with spaces.json",
        }))
        .ok_or(TestError::Missing("valid export marker"))?;

        assert_eq!(request.format, ExportOutputFormat::Json);
        assert!(!request.include_tool_calls);
        assert!(request.include_reasoning_summary);
        assert_eq!(
            request.path.as_deref(),
            Some("exports/session with spaces.json")
        );
        // Kein `max_chars`-Schlüssel im Marker → `None`, keine Begrenzung.
        assert_eq!(request.max_chars, None);
        Ok(())
    }

    /// `max_chars` im Marker (`--max-chars <n>` am `/export`-Command, siehe
    /// `harw_ops::export`) wird als positive Zahl übernommen und landet
    /// unverändert in `ExportOptions.max_chars`.
    #[test]
    fn export_request_marker_reads_max_chars() -> TestResult {
        let request = export_request_from_data(&json!({
            "kind": "export.request",
            "format": "markdown",
            "max_chars": 20000,
        }))
        .ok_or(TestError::Missing("valid export marker"))?;

        assert_eq!(request.max_chars, Some(20000));

        let opts = ExportOptions {
            include_tool_calls: request.include_tool_calls,
            include_reasoning: request.include_reasoning_summary,
            max_chars: request.max_chars,
            ..ExportOptions::default()
        };
        assert_eq!(opts.max_chars, Some(20000));
        Ok(())
    }

    #[test]
    fn build_export_markdown_reflects_reasoning_and_tool_options() -> TestResult {
        let mut app = test_chat_app()?;
        app.push_line(Role::User, "Frage");
        app.export_entries
            .push(ExportEntry::Reasoning("sichere Zusammenfassung".to_owned()));
        app.export_entries.push(ExportEntry::Tool {
            label: "shell.exec".to_owned(),
            summary: Some("Ergebnis".to_owned()),
        });

        // Standard: Reasoning ausgeblendet, Werkzeuge eingeblendet.
        let default_markdown = build_export_markdown(&app, &ExportOptions::default());
        assert!(!default_markdown.contains("sichere Zusammenfassung"));
        assert!(default_markdown.contains("shell.exec"));

        // `--reasoning-summary` muss im Markdown tatsächlich wirken.
        let with_reasoning = build_export_markdown(
            &app,
            &ExportOptions {
                include_reasoning: true,
                ..ExportOptions::default()
            },
        );
        assert!(with_reasoning.contains("sichere Zusammenfassung"));

        // `build_export` mit `ExportOutputFormat::Markdown` muss identisch zu
        // `build_export_markdown` sein — beide teilen sich denselben Zweig.
        let via_build_export = build_export(
            &app,
            &ExportOptions {
                include_reasoning: true,
                ..ExportOptions::default()
            },
            ExportOutputFormat::Markdown,
        );
        assert_eq!(with_reasoning, via_build_export);
        Ok(())
    }

    #[test]
    fn structured_export_request_feeds_json_renderer_options() -> TestResult {
        let mut app = test_chat_app()?;
        app.push_line(Role::User, "Frage");
        app.export_entries
            .push(ExportEntry::Reasoning("sichere Zusammenfassung".to_owned()));
        app.export_entries.push(ExportEntry::Tool {
            label: "shell.exec".to_owned(),
            summary: Some("Ergebnis".to_owned()),
        });

        let request = export_request_from_data(&json!({
            "kind": "export.request",
            "format": "json",
            "include_tool_calls": false,
            "include_reasoning_summary": true,
            "path": null,
        }))
        .ok_or(TestError::Missing("valid export marker"))?;
        let opts = ExportOptions {
            include_tool_calls: request.include_tool_calls,
            include_reasoning: request.include_reasoning_summary,
            ..ExportOptions::default()
        };
        let document: Value = serde_json::from_str(&build_export(&app, &opts, request.format))
            .map_err(ctx("valid JSON export"))?;
        let events = document["events"]
            .as_array()
            .ok_or(TestError::Missing("events array"))?;

        assert!(
            events
                .iter()
                .any(|event| event["type"] == "reasoning_summary")
        );
        assert!(!events.iter().any(|event| event["type"] == "tool"));
        Ok(())
    }

    #[test]
    fn file_export_reports_the_actual_collision_suffix() -> TestResult {
        let mut app = test_chat_app()?;
        let path = std::env::temp_dir().join(format!(
            "harw-tui-export-suffix-{}-{}.md",
            std::process::id(),
            SessionId::new()
        ));
        let first =
            export::write_export_path(&path, "first\n").map_err(ctx("create first export"))?;

        resolve_export_request(
            &mut app,
            &ExportRequest {
                format: ExportOutputFormat::Markdown,
                include_tool_calls: true,
                include_reasoning_summary: false,
                path: Some(path.to_string_lossy().into_owned()),
                max_chars: None,
            },
        );

        let message = app
            .export_entries
            .iter()
            .rev()
            .find_map(|entry| match entry {
                ExportEntry::System(text) => Some(text.as_str()),
                _ => None,
            })
            .ok_or(TestError::Missing("export status message"))?;
        let expected_suffix = format!("{}-2.md", path.with_extension("").display());
        assert!(message.contains(&expected_suffix), "message: {message}");

        let second = path.with_file_name(format!(
            "{}-2.md",
            path.file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or(TestError::Missing("stem"))?
        ));
        std::fs::remove_file(first).ok();
        std::fs::remove_file(second).ok();
        Ok(())
    }

    /// A7-Fallback-Entscheid: schlägt beim reinen `/export`
    /// (`resolve_export_choice`-Index `0`, „Zwischenablage") sowohl jedes
    /// Systemwerkzeug als auch der OSC-52-Fallback fehl, wird stattdessen
    /// eine Datei über [`export::default_export_path`] geschrieben und ihr
    /// Pfad gemeldet — statt nur den `NoClipboard`-Fehler anzuzeigen. In der
    /// Testumgebung ist ohnehin kein Zwischenablage-Werkzeug installiert;
    /// ein absichtlich übergroßer Inhalt (> `OSC52_MAX_BYTES`) lässt
    /// zusätzlich den OSC-52-Fallback selbst scheitern, sodass
    /// `clipboard::copy_or_sequence` deterministisch `NoClipboard` liefert.
    #[test]
    fn export_choice_falls_back_to_a_file_when_no_clipboard_is_available() -> TestResult {
        let mut app = test_chat_app()?;
        // Größer als `clipboard::OSC52_MAX_BYTES` (100_000) — macht auch den
        // OSC-52-Fallback selbst unmöglich, nicht nur die Systemwerkzeuge.
        app.push_line(Role::User, "x".repeat(150_000));
        app.pending_export_options = Some(ExportOptions::default());
        app.pending_export_format = ExportOutputFormat::Markdown;
        let (bus, _receiver) = harw_event_channel();

        resolve_export_choice(&mut app, &bus, 0);

        let message = app
            .export_entries
            .iter()
            .rev()
            .find_map(|entry| match entry {
                ExportEntry::System(text) => Some(text.clone()),
                _ => None,
            })
            .ok_or(TestError::Missing("export status message"))?;
        let expected_prefix = "Keine Zwischenablage — Export gespeichert unter ";
        assert!(message.starts_with(expected_prefix), "message: {message}");

        let path = std::path::PathBuf::from(
            message
                .strip_prefix(expected_prefix)
                .ok_or(TestError::Missing("prefix checked above"))?,
        );
        assert!(
            path.exists(),
            "the fallback export file must actually be written: {path:?}"
        );
        std::fs::remove_file(&path).ok();
        Ok(())
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
            allowed_child_orchestrators: Vec::new(),
            trace: None,
            ceiling: None,
        }
    }

    #[test]
    fn busy_turn_scrolls_immediately_and_preserves_typed_input_in_order() -> TestResult {
        // `queue_busy_key` (siehe Doku dort) wurde bewusst umgebaut: Tastatur-
        // Events werden während eines laufenden Turns nicht mehr roh in
        // `deferred_input` zwischengelagert, sondern live in `app.input`
        // editiert — der Composer bleibt beim Tippen sichtbar aktuell.
        // Fertige Chat-Zeilen landen direkt in `pending_turns`; nur ein
        // fertiges, zurückgestelltes Slash-Kommando wird für die autorisierte
        // Nach-Turn-Ausführung als Paste+Enter in `deferred_input` gelegt
        // (Runde 4, Teil H: `Immediate`/`Staged` meldet
        // `BusyKeyOutcome::Dispatch`, lokale Befehle `Local`, siehe die
        // eigenen Tests dafür unten). Diese Assertions prüfen jetzt genau das, statt die
        // alte Roh-Event-Warteschlange: Scrollen wirkt weiterhin sofort, und
        // Tippen + Einfügen bleiben in der Reihenfolge im Composer erhalten.
        let mut app = test_chat_app()?;
        app.last_history_total_lines.set(100);
        app.last_history_visible_rows.set(10);
        let typed = TuiEvent::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        let pasted = TuiEvent::Paste("next prompt".to_owned());
        assert_eq!(
            handle_busy_event(&mut app, typed.clone()),
            BusyKeyOutcome::Redraw
        );
        assert_eq!(
            handle_busy_event(
                &mut app,
                TuiEvent::Key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE))
            ),
            BusyKeyOutcome::Redraw
        );
        assert!(app.scroll.offset() > 0);
        assert_eq!(
            handle_busy_event(&mut app, pasted.clone()),
            BusyKeyOutcome::Redraw
        );
        assert_eq!(app.input.text(), "xnext prompt");
        assert!(app.deferred_input.is_empty());
        Ok(())
    }

    /// `/status` trägt `BusyAvailability::Immediate` — `queue_busy_key`
    /// reiht ihn in `busy_jobs` ein und meldet `Dispatch(Immediate)`, statt
    /// ihn in `deferred_input` zurückzustellen; der Aufrufer startet ihn als
    /// eigenen Task (`settle_busy_outcome`).
    #[test]
    fn busy_turn_immediate_command_is_dispatched_without_touching_deferred_input() -> TestResult {
        let mut app = test_chat_app()?;
        app.input.insert_str("/status");

        let outcome = queue_busy_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            outcome,
            BusyKeyOutcome::Dispatch(BusyAvailability::Immediate)
        );
        assert_eq!(
            app.busy_jobs.queued().front(),
            Some(&BusyCommand {
                raw: "/status".to_owned(),
                class: BusyAvailability::Immediate,
            })
        );
        assert!(app.input.is_empty());
        assert!(app.deferred_input.is_empty());
        Ok(())
    }

    /// `/compact` bleibt `DeferredUntilTurnEnd`: Paste+Enter in
    /// `deferred_input`, für die autorisierte Ausführung nach Turn-Ende.
    #[test]
    fn busy_turn_queues_submitted_command_for_authorized_dispatch_after_turn() -> TestResult {
        let mut app = test_chat_app()?;
        app.input.insert_str("/compact");

        assert_eq!(
            queue_busy_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            BusyKeyOutcome::Redraw
        );
        assert!(app.input.is_empty());
        assert!(app.busy_jobs.queued().is_empty());
        assert_eq!(
            app.deferred_input.pop_front(),
            Some(TuiEvent::Paste("/compact".to_owned()))
        );
        assert_eq!(
            app.deferred_input.pop_front(),
            Some(TuiEvent::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
        );
        assert!(app.deferred_input.is_empty());
        Ok(())
    }

    /// `/model switch x` und `/mode plan` sind `Staged`: sie laufen sofort,
    /// merken ihre Änderung aber nur im Controller vor (gilt ab dem nächsten
    /// Turn).
    #[test]
    fn busy_turn_staged_commands_are_dispatched_as_staged() -> TestResult {
        for raw in ["/model switch x", "/mode plan", "/effort high"] {
            let mut app = test_chat_app()?;
            app.input.insert_str(raw);
            assert_eq!(
                queue_busy_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                BusyKeyOutcome::Dispatch(BusyAvailability::Staged),
                "{raw}"
            );
            assert_eq!(
                app.busy_jobs
                    .queued()
                    .front()
                    .map(|command| command.raw.as_str()),
                Some(raw)
            );
            assert!(app.deferred_input.is_empty(), "{raw}");
        }
        Ok(())
    }

    /// Ein unbekannter Befehl bleibt sicher eingereiht — `busy_availability_for`
    /// liefert dafür `DeferredUntilTurnEnd`, der eigentliche „unbekannter
    /// Befehl"-Fehler entsteht erst im späteren Dispatch.
    #[test]
    fn busy_turn_unknown_command_stays_deferred() -> TestResult {
        let mut app = test_chat_app()?;
        app.input.insert_str("/no-such-command");

        assert_eq!(
            queue_busy_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            BusyKeyOutcome::Redraw
        );
        assert_eq!(
            app.deferred_input.pop_front(),
            Some(TuiEvent::Paste("/no-such-command".to_owned()))
        );
        assert_eq!(
            app.deferred_input.pop_front(),
            Some(TuiEvent::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
        );
        assert!(app.deferred_input.is_empty());
        Ok(())
    }

    /// Chattext (kein Slash-Befehl) landet weiterhin direkt in `pending_turns`,
    /// unabhängig von `busy_availability_for`.
    #[test]
    fn busy_turn_chat_text_still_goes_to_pending_turns() -> TestResult {
        let mut app = test_chat_app()?;
        app.input.insert_str("hallo welt");

        assert_eq!(
            queue_busy_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            BusyKeyOutcome::Redraw
        );
        assert_eq!(app.pending_turns.pop_front(), Some("hallo welt".to_owned()));
        assert!(app.deferred_input.is_empty());
        Ok(())
    }

    /// Welle 4c: ein erster Ctrl+C während eines laufenden Turns bricht ihn
    /// weiterhin kooperativ ab und scharft zusätzlich `app.pending_quit`; ein
    /// zweiter Druck derselben Taste innerhalb von `QUIT_HINT_WINDOW` setzt
    /// `app.hard_quit_requested` — derselbe Doppeldruck-Vertrag wie im
    /// Idle-Pfad (`handle_key`/`double_ctrl_d_quits_regardless_of_...`), nur
    /// mit zusätzlichem Cancel des laufenden Turns statt eines sofortigen
    /// `HarwEvent::Quit`.
    #[test]
    fn double_ctrl_c_during_busy_quits_like_idle() -> TestResult {
        let mut app = test_chat_app()?;
        let cancel = CancelToken::new();
        app.active_cancel = Some(cancel.clone());
        let ctrl_c = || TuiEvent::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));

        assert_eq!(
            handle_busy_event(&mut app, ctrl_c()),
            BusyKeyOutcome::Redraw
        );
        assert!(
            cancel.is_cancelled(),
            "erster Ctrl+C muss den Turn kooperativ abbrechen"
        );
        assert!(matches!(
            app.pending_quit,
            Some(QuitArm {
                label: "Ctrl+C",
                ..
            })
        ));
        assert!(!app.hard_quit_requested);

        assert_eq!(
            handle_busy_event(&mut app, ctrl_c()),
            BusyKeyOutcome::Redraw
        );
        assert!(
            app.hard_quit_requested,
            "zweiter Ctrl+C-Druck binnen des Fensters muss hart beenden"
        );
        Ok(())
    }

    /// Runde 4 (Nutzerwunsch): ein Ctrl+C-Abbruch bricht nur den laufenden
    /// Turn ab. Bereits abgeschickte, aber noch nicht ausgelieferte Eingaben
    /// bleiben in `pending_turns`/`deferred_input` und werden von `run_loop`
    /// an der nächsten Turn-Grenze ausgeliefert; die Statuszeile meldet das.
    #[test]
    fn ctrl_c_during_busy_keeps_queued_input_for_delivery() -> TestResult {
        let mut app = test_chat_app()?;
        let cancel = CancelToken::new();
        app.active_cancel = Some(cancel.clone());
        app.deferred_input.push_back(TuiEvent::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        )));
        app.pending_turns
            .push_back("noch nicht gesendet".to_owned());
        let ctrl_c = TuiEvent::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));

        assert_eq!(handle_busy_event(&mut app, ctrl_c), BusyKeyOutcome::Redraw);

        assert!(
            cancel.is_cancelled(),
            "Ctrl+C muss den laufenden Turn abbrechen"
        );
        assert_eq!(
            app.pending_turns.front().map(String::as_str),
            Some("noch nicht gesendet"),
            "abgeschickte Nachrichten müssen nach dem Abbruch ausgeliefert werden"
        );
        assert_eq!(app.deferred_input.len(), 1);
        assert!(
            app.queue_kept_at.is_some(),
            "eine nicht-leere Warteschlange muss den Statuszeilen-Hinweis setzen"
        );
        Ok(())
    }

    /// Runde 4: Esc unterbricht einen laufenden Turn wie Ctrl+C, behält die
    /// Warteschlange und den Composer-Text, scharft aber kein Beenden.
    #[test]
    fn esc_during_busy_interrupts_and_keeps_queue_without_quit_arm() -> TestResult {
        let mut app = test_chat_app()?;
        let cancel = CancelToken::new();
        app.active_cancel = Some(cancel.clone());
        app.pending_turns.push_back("gleich danach".to_owned());
        app.input.insert_str("angefangen");
        let esc = TuiEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert_eq!(handle_busy_event(&mut app, esc), BusyKeyOutcome::Redraw);

        assert!(
            cancel.is_cancelled(),
            "Esc muss den laufenden Turn abbrechen"
        );
        assert_eq!(
            app.pending_turns.front().map(String::as_str),
            Some("gleich danach")
        );
        assert!(app.queue_kept_at.is_some());
        assert_eq!(app.input(), "angefangen", "Composer-Text bleibt stehen");
        assert!(
            app.pending_quit.is_none(),
            "Esc darf nie das Beenden scharfstellen"
        );
        assert!(!app.hard_quit_requested);
        Ok(())
    }

    /// Leere Warteschlangen dürfen den „Warteschlange wird gesendet“-Hinweis
    /// nicht fälschlich scharfstellen — sonst zeigte die Statuszeile bei
    /// jedem Ctrl+C einen Hinweis, obwohl nichts verworfen wurde.
    #[test]
    fn ctrl_c_during_busy_with_empty_queues_does_not_arm_queue_cleared_hint() -> TestResult {
        let mut app = test_chat_app()?;
        app.active_cancel = Some(CancelToken::new());
        assert!(app.deferred_input.is_empty());
        assert!(app.pending_turns.is_empty());
        let ctrl_c = TuiEvent::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));

        assert_eq!(handle_busy_event(&mut app, ctrl_c), BusyKeyOutcome::Redraw);

        assert!(
            app.queue_kept_at.is_none(),
            "ohne eingereihte Eingaben gibt es nichts zu verwerfen"
        );
        Ok(())
    }

    /// Nach Ablauf von `QUIT_HINT_WINDOW` beendet ein erneuter Ctrl+C-Druck
    /// nicht hart — die Scharfstellung ist verfallen und wird stattdessen neu
    /// gesetzt, exakt wie die Ablaufprüfung im Idle-Pfad von `run_loop`
    /// (Welle 4c, Punkt 5).
    #[test]
    fn ctrl_c_during_busy_after_window_expiry_does_not_hard_quit() -> TestResult {
        let mut app = test_chat_app()?;
        app.active_cancel = Some(CancelToken::new());
        app.pending_quit = Some(QuitArm {
            label: "Ctrl+C",
            at: Instant::now() - QUIT_HINT_WINDOW - Duration::from_millis(1),
        });
        let ctrl_c = TuiEvent::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));

        assert_eq!(handle_busy_event(&mut app, ctrl_c), BusyKeyOutcome::Redraw);
        assert!(
            !app.hard_quit_requested,
            "ein abgelaufener Hinweis darf keinen harten Abbruch auslösen"
        );
        assert!(matches!(
            app.pending_quit,
            Some(QuitArm {
                label: "Ctrl+C",
                ..
            })
        ));
        Ok(())
    }

    #[test]
    fn rendered_input_keeps_cursor_on_wrapped_text_and_long_input_visible() -> TestResult {
        let mut app = test_chat_app()?;
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(20, 16))
            .map_err(ctx("test terminal"))?;
        app.input.insert_str("one two three four five");
        app.input.move_left();
        terminal
            .draw(|frame| render_viewport(frame, &app, &Spinner::new(), None))
            .map_err(ctx("draw"))?;
        let position = terminal.get_cursor_position().map_err(ctx("cursor"))?;
        assert_eq!(terminal.backend().buffer()[position].symbol(), "e");

        app.input.clear();
        app.input.insert_str("0\n1\n2\n3\n4\n5\n6\n7\n8\n9\nlast");
        terminal
            .draw(|frame| render_viewport(frame, &app, &Spinner::new(), None))
            .map_err(ctx("draw"))?;
        let position = terminal.get_cursor_position().map_err(ctx("cursor"))?;
        assert_eq!(
            terminal.backend().buffer()[(position.x - 1, position.y)].symbol(),
            "t"
        );
        // `render_viewport` legt die Statuszeile inzwischen ÜBER den Composer
        // statt darunter (siehe Kommentar „History | permanente Statuszeile |
        // Eingabe" dort): der Sicherheitsmodus soll nicht vom Verlauf
        // verdrängt werden können. Dadurch sitzt `input_area` jetzt eine Zeile
        // tiefer als zur Einführung dieses Tests (damals History | Eingabe |
        // Status), und die alte Grenze `< 14` war an die alte Reihenfolge
        // gebunden. Mit 20x16-Testterminal, 11 Composer-Zeilen (geclamped auf
        // 8, `input_height` = 10) und der Statuszeile jetzt bei y=5 liegt
        // `input_area` bei y=6..16; die eigene Bodenkante des Composers (Zeile
        // 15) bleibt die relevante Grenze, kein `status_area` mehr darunter.
        assert!(
            position.y < 15,
            "cursor remains inside input, above its own bottom border"
        );
        Ok(())
    }

    fn test_executable_agent_ir(
        admitted: &[&str],
        forbidden: &[&str],
    ) -> TestResult<ExecutableAgentIr> {
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
        .map_err(ctx("test executable policy TOML"))?;
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
            reasoning_effort: raw.reasoning_effort,
        };

        harw_agent_dsl::lower(&resolved).map_err(ctx("lower test executable policy"))
    }

    /// W2d-2/T2b: `build_tui_agent_session` ist entfallen (Montage lebt jetzt in
    /// `harw_runtime::RuntimeAssembly`); die Session wird direkt über
    /// `AgentSession::new_with_id(..).with_spawn_context(..).with_turn_event_sink(..)`
    /// gebaut (CONTRACTS-W2d2 §2 T2b). Das geprüfte Verhalten
    /// (`with_executable_agent_ir` schneidet die Werkzeugfläche) ist unverändert.
    #[test]
    fn selected_executable_policy_limits_tui_session_tools_and_retains_snapshot() -> TestResult {
        let policy = test_executable_agent_ir(&["stop"], &[])?;
        let snapshot_id = policy.snapshot_id();
        let sandbox = test_sandbox()?;
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
        Ok(())
    }

    /// W2d-2/T2b: siehe oben — ohne `with_executable_agent_ir` bleibt die
    /// volle Werkzeugfläche sichtbar.
    #[test]
    fn absent_executable_policy_preserves_full_tui_session_visibility() -> TestResult {
        let sandbox = test_sandbox()?;
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
        Ok(())
    }

    #[test]
    fn rate_limit_retry_does_not_append_the_user_message_again() -> TestResult {
        let retry = rate_limit_retry_input();

        assert!(
            retry.user_text.is_none(),
            "core already persisted the failed turn's user message"
        );
        Ok(())
    }

    fn test_operations() -> Vec<Arc<dyn harw_operations::Operation>> {
        let mut registry = OperationRegistry::new();
        harw_ops::register_all(&mut registry);
        registry.iter().map(Arc::clone).collect()
    }

    #[test]
    fn test_classify_line_ignore_on_empty() -> TestResult {
        // Leere Zeilen beenden nicht mehr — sie werden ignoriert.
        assert_eq!(classify_line(""), LineAction::Ignore);
        assert_eq!(classify_line("   "), LineAction::Ignore);
        Ok(())
    }

    #[test]
    fn test_classify_line_quit_on_slash_quit() -> TestResult {
        assert_eq!(classify_line("/quit"), LineAction::Quit);
        assert_eq!(classify_line("/exit"), LineAction::Quit);
        Ok(())
    }

    #[test]
    fn test_classify_line_chat_passes_text() -> TestResult {
        assert_eq!(
            classify_line("hallo welt"),
            LineAction::Chat("hallo welt".to_owned())
        );
        Ok(())
    }

    #[test]
    fn test_classify_line_command_is_dispatched() -> TestResult {
        // `/command`-Zeilen werden nun zur Ausführung durchgereicht, nicht mehr
        // als „noch nicht unterstützt"-Hinweis abgewiesen.
        assert_eq!(
            classify_line("/status"),
            LineAction::Command("/status".to_owned())
        );
        Ok(())
    }

    #[test]
    fn test_chatapp_push_and_read() -> TestResult {
        let mut app = test_chat_app()?;
        app.push_line(Role::User, "hi");
        // Nach einem push_line muss genau eine Zelle vorhanden sein.
        assert_eq!(app.cells_len(), 1);
        assert!(app.input().is_empty());
        Ok(())
    }

    /// Prüft, dass nach je einem User- und Assistant-Push zwei Zellen vorhanden sind.
    #[test]
    fn test_push_user_and_assistant_cells_count() -> TestResult {
        let mut app = test_chat_app()?;
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
        Ok(())
    }

    /// Prüft, dass nach einem System-Push die PlainHistoryCell den Text enthält.
    #[test]
    fn test_push_system_cell_plain_content() -> TestResult {
        let mut app = test_chat_app()?;
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
        Ok(())
    }

    /// Popup öffnet sich wenn der Eingabepuffer mit `/` beginnt.
    #[test]
    fn test_popup_opens_on_slash_prefix() -> TestResult {
        let mut app = test_chat_app()?;
        assert!(app.command_popup.is_none(), "initial kein Popup");

        app.input.insert_char('/');
        app.sync_popup();

        assert!(
            app.command_popup.is_some(),
            "nach '/' muss Popup geöffnet sein"
        );
        assert!(
            !app.command_popup
                .as_ref()
                .ok_or(TestError::Missing("command_popup"))?
                .is_empty(),
            "ungefiltertes Popup darf nicht leer sein"
        );
        Ok(())
    }

    /// Popup schließt sich wenn der Eingabepuffer das `/`-Präfix verliert.
    #[test]
    fn test_popup_closes_without_slash_prefix() -> TestResult {
        let mut app = test_chat_app()?;

        app.input.insert_char('/');
        app.sync_popup();
        assert!(app.command_popup.is_some());

        app.input.backspace();
        app.sync_popup();

        assert!(
            app.command_popup.is_none(),
            "nach Entfernen des '/' muss Popup geschlossen sein"
        );
        Ok(())
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
    fn double_ctrl_d_quits_regardless_of_composer_or_popup_state() -> TestResult {
        let mut app = test_chat_app()?;
        // `sync_popup` schließt das Popup, sobald der Query-Teil nach dem
        // `/` ein Leerzeichen enthält (Argument-Eingabe hat begonnen, siehe
        // Doku an `sync_popup`) — "/status mit Entwurf" erfüllt die
        // Vorbedingung deshalb nicht mehr. Ein reiner Kommandoname ohne
        // Leerzeichen hält das Popup dagegen offen und deckt denselben Fall
        // ab (nicht-leerer Composer + offenes Popup); die geprüfte Aussage
        // (doppeltes Ctrl-D beendet immer) bleibt unverändert.
        app.input.insert_str("/status");
        app.sync_popup();
        assert!(app.command_popup.is_some(), "Vorbedingung: Popup ist offen");

        let (bus, mut receiver) = harw_event_channel();
        let ctrl_d = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);

        assert!(handle_key(&mut app, ctrl_d, &bus));
        assert!(matches!(
            app.pending_quit,
            Some(QuitArm {
                label: "Ctrl+D",
                ..
            })
        ));
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));

        assert!(!handle_key(&mut app, ctrl_d, &bus));
        assert!(matches!(receiver.try_recv(), Ok(HarwEvent::Quit)));
        Ok(())
    }

    /// Enter bei einem offenen Popup übernimmt den markierten Befehl und
    /// sendet den unvollständigen Präfix nicht als unbekannten Command ab.
    #[test]
    fn test_handle_key_enter_accepts_popup_selection_without_submit() -> TestResult {
        let mut app = test_chat_app()?;
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
        let redraw = handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &bus,
        );

        assert!(redraw, "Enter muss einen Redraw anfordern");
        assert_eq!(app.input(), "/compact ");
        assert!(
            app.command_popup.is_none(),
            "Auswahl muss das Popup schließen"
        );
        assert!(
            receiver.try_recv().is_err(),
            "Autocomplete darf noch keinen Command absenden"
        );
        Ok(())
    }

    #[test]
    fn busy_submit_is_queued_in_fifo_order() -> TestResult {
        let mut app = test_chat_app()?;
        app.input.insert_str("erste Nachricht");
        assert_eq!(
            queue_busy_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            BusyKeyOutcome::Redraw
        );
        app.input.insert_str("zweite Nachricht");
        assert_eq!(
            queue_busy_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            BusyKeyOutcome::Redraw
        );

        assert_eq!(
            app.pending_turns.pop_front().as_deref(),
            Some("erste Nachricht")
        );
        assert_eq!(
            app.pending_turns.pop_front().as_deref(),
            Some("zweite Nachricht")
        );
        assert!(app.pending_turns.is_empty());
        Ok(())
    }

    /// Der Composer bleibt beim ersten Escape erhalten und wird beim zweiten
    /// unmittelbaren Escape geleert.
    #[test]
    fn two_escapes_clear_the_input_box() -> TestResult {
        let mut app = test_chat_app()?;
        app.input.insert_str("nicht verlieren beim ersten Escape");
        let (bus, _receiver) = harw_event_channel();
        let escape = KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE);

        assert!(handle_key(&mut app, escape, &bus));
        assert_eq!(app.input(), "nicht verlieren beim ersten Escape");
        assert!(app.escape_armed);

        assert!(handle_key(&mut app, escape, &bus));
        assert!(app.input().is_empty());
        assert!(!app.escape_armed);
        Ok(())
    }

    /// `KeyCode::Tab` bei offenem Popup akzeptiert die markierte Auswahl,
    /// setzt `app.input` auf `"/<name> "` und sendet dabei NICHTS über den
    /// Bus (kein Absenden, nur Autocomplete).
    #[test]
    fn test_handle_key_tab_accepts_popup_selection_without_submit() -> TestResult {
        let mut app = test_chat_app()?;
        app.input.clear();
        app.input.insert_str("/hel");
        app.sync_popup();
        match app
            .command_popup
            .as_ref()
            .and_then(CommandPopup::selected_name)
        {
            Some(name) => assert_eq!(name, "help", "einzige Übereinstimmung für 'hel'"),
            None => {
                return Err(TestError::Unexpected(
                    "Popup muss eine Auswahl für 'hel' markieren".into(),
                ));
            }
        }

        let (bus, mut receiver) = harw_event_channel();
        let key = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);

        let redraw = handle_key(&mut app, key, &bus);

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
            Ok(event) => {
                return Err(TestError::Unexpected(format!(
                    "Tab darf kein HarwEvent senden, war: {event:?}"
                )));
            }
        }
        Ok(())
    }

    /// Regression: `/mo` listet zwei Präfix-Treffer (`/mode`, `/model`) UND
    /// einen reinen Teilstring-Treffer (`/memory`, enthält „mo", ist aber
    /// kein Präfix-Treffer). Ohne bewegte Markierung darf Tab weder den
    /// Teilstring-Treffer noch irgendeinen einzelnen Präfix-Treffer sofort
    /// übernehmen, solange mehrere Präfix-Treffer uneindeutig sind —
    /// stattdessen wird die Eingabe auf deren längstes gemeinsames Präfix
    /// erweitert (`/mode`) und das Popup bleibt offen.
    #[test]
    fn test_handle_key_tab_extends_query_to_common_prefix_for_ambiguous_matches() -> TestResult {
        let mut app = test_chat_app()?;
        app.input.clear();
        app.input.insert_str("/mo");
        app.sync_popup();

        let (bus, mut receiver) = harw_event_channel();
        let key = KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE);

        let redraw = handle_key(&mut app, key, &bus);

        assert!(redraw, "Tab muss einen Redraw anfordern");
        assert_eq!(
            app.input(),
            "/mode",
            "Tab muss auf das gemeinsame Präfix von /mode und /model erweitern"
        );
        assert_ne!(
            app.input(),
            "/memory ",
            "Tab darf niemals den reinen Teilstring-Treffer /memory übernehmen"
        );
        assert!(
            app.command_popup.is_some(),
            "Popup muss offen bleiben, solange die Erweiterung mehrdeutig bleibt"
        );
        assert!(
            receiver.try_recv().is_err(),
            "Query-Erweiterung darf keinen Command absenden"
        );
        Ok(())
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
    fn test_sync_popup_closes_when_argument_has_whitespace() -> TestResult {
        let mut app = test_chat_app()?;
        app.input.clear();
        app.input.insert_str("/stop job-");

        app.sync_popup();

        assert!(
            app.command_popup.is_none(),
            "Popup muss bei Whitespace nach '/' geschlossen sein"
        );
        Ok(())
    }

    /// Kontrast: `/mod` (kein Leerzeichen) hält das Popup offen — reine
    /// Command-Namen-Eingabe ohne Argument-Phase.
    #[test]
    fn test_sync_popup_stays_open_without_whitespace() -> TestResult {
        let mut app = test_chat_app()?;
        app.input.clear();
        app.input.insert_str("/mod");

        app.sync_popup();

        assert!(
            app.command_popup.is_some(),
            "Popup muss ohne Whitespace nach '/' offen bleiben"
        );
        Ok(())
    }

    /// Kern-Regressionstest: Zeichen für Zeichen `/stop job-42` eintippen —
    /// die Ziffern `4` und `2` dürfen NICHT vom Popup als Auswahl-Index
    /// verschluckt werden, sobald die Argument-Phase (nach dem Leerzeichen)
    /// erreicht ist.
    #[test]
    fn test_handle_key_types_digits_in_argument_not_swallowed() -> TestResult {
        let mut app = test_chat_app()?;
        let (bus, mut receiver) = harw_event_channel();

        for character in "/stop job-42".chars() {
            let key = KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE);
            handle_key(&mut app, key, &bus);
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
        Ok(())
    }

    #[test]
    fn resume_command_becomes_a_runtime_request_only_for_valid_shapes() -> TestResult {
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
        assert_eq!(resume_request("/new"), Some(TuiRunOutcome::NewSession));
        assert_eq!(resume_request("/news"), None);
        Ok(())
    }

    #[test]
    fn durable_history_hydrates_core_and_redacts_non_text_visible_content() -> TestResult {
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
            tool_name: "fs.read".to_owned(),
            arguments: json!({ "path": "/tmp/visible.txt" }),
        }));
        history.push(TurnItem::ToolResult(ToolResultItem {
            id: ItemId::new(),
            call_id,
            result: ToolCallResult::error("not found"),
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
        let mut app = test_chat_app()?;
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
        assert!(visible.contains("1 Datei gelesen"));
        assert!(visible.contains("1 fehlgeschlagen"));
        assert!(visible.contains("private reasoning"));
        assert!(visible.contains("sensitive backend detail"));
        for secret in ["secret.example", "raw secret"] {
            assert!(!visible.contains(secret));
        }
        Ok(())
    }

    #[test]
    fn resume_hydration_pairs_orphan_result_and_marks_open_call() -> TestResult {
        use harw_protocol::items::{ResultTrust, ToolCallItem, ToolCallResult, ToolResultItem};
        use harw_types::{ItemId, ToolCallId};

        let orphan_id = ToolCallId::new();
        let open_id = ToolCallId::new();
        let mut history = ConversationHistory::new();
        history.push(TurnItem::ToolResult(ToolResultItem {
            id: ItemId::new(),
            call_id: orphan_id,
            result: ToolCallResult::error("orphan result"),
            duration_ms: 9,
            trust: ResultTrust::Runtime,
        }));
        history.push(TurnItem::ToolCall(ToolCallItem {
            id: ItemId::new(),
            call_id: open_id,
            tool_name: "shell.exec".to_owned(),
            arguments: json!({ "command": "echo pending" }),
        }));

        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut session = AgentSession::new_with_id(
            SessionId::from_str("resume-tool-pairs"),
            AgentRole::Assistant,
            None,
            ExtensionRegistry::builder().build(),
            event_tx,
        );
        let mut app = test_chat_app()?;
        install_loaded_history(&mut session, &mut app, history);

        let visible = app
            .cells
            .iter()
            .flat_map(|cell| cell.display_lines(200, style::Theme::Dark))
            .flat_map(|line| line.spans)
            .map(|span| span.content.into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(visible.contains("tool.result"));
        assert!(visible.contains("orphan result"));
        assert!(visible.contains("unvollständig (Resume-Abbruch)"));
        let tool_entries = app
            .export_entries
            .iter()
            .filter(|entry| {
                matches!(
                    entry,
                    ExportEntry::ToolCall { .. } | ExportEntry::ToolResult { .. }
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(tool_entries.len(), 4);
        assert!(matches!(
            tool_entries[0],
            ExportEntry::ToolCall { tool_name, .. } if tool_name == "tool.result"
        ));
        assert!(matches!(tool_entries[1], ExportEntry::ToolResult { .. }));
        assert!(matches!(
            tool_entries[2],
            ExportEntry::ToolCall { tool_name, .. } if tool_name == "shell.exec"
        ));
        assert!(matches!(
            tool_entries[3],
            ExportEntry::ToolResult { error: Some(error), .. }
                if error == "unvollständig (Resume-Abbruch)"
        ));
        Ok(())
    }

    #[test]
    fn incident_hint_is_small_and_only_suggests_bug_report_on_known_signals() -> TestResult {
        assert!(
            incident_hint(&TuiError::Core("provider timeout".to_owned()), 2)
                .is_some_and(|hint| hint.contains("/bug-report"))
        );
        assert!(
            incident_hint(&TuiError::Core("agent killed".to_owned()), 0)
                .is_some_and(|hint| hint.contains("/bug-report"))
        );
        assert!(
            incident_hint(&TuiError::Core("ordinary validation error".to_owned()), 9).is_none()
        );
        Ok(())
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
            .approval_handler(Arc::new(
                harw_registry_defaults::DefaultApprovalPolicy::new(ApprovalModeCell::default()),
            ))
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
    async fn drive_turn_answering(approve: bool) -> TestResult<DrivenTurn> {
        let executions = Arc::new(AtomicUsize::new(0));
        let (handler, mut prompts) = TuiApprovalHandler::new();
        let driver = ApprovalDriver::new(Arc::clone(&handler));
        let sandbox = test_sandbox()?;
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
                return Ok(DrivenTurn {
                    outcome: Err(error.to_string()),
                    executions: executions.load(Ordering::SeqCst),
                    prompts_seen: 0,
                });
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

        Ok(DrivenTurn {
            outcome: result.map_err(|error| tui_error_from_approval_driver(error).to_string()),
            executions: executions.load(Ordering::SeqCst),
            prompts_seen,
        })
    }

    /// Bedingung 1 + 5: der Spawn-Kontext trägt einen Approval-Actor, und eine
    /// freigegebene Pause endet in `Completed` — das Werkzeug lief genau einmal.
    #[tokio::test]
    async fn approved_pause_completes_the_turn_and_runs_the_tool() -> TestResult {
        let driven = drive_turn_answering(true).await?;

        match driven.outcome {
            Ok(TurnOutcome::Completed) => {}
            Ok(other) => {
                return Err(TestError::Unexpected(format!(
                    "an approved pause must complete the turn, was: {other:?}"
                )));
            }
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "an approved pause must not fail the turn: {error}"
                )));
            }
        }
        assert_eq!(driven.prompts_seen, 1, "genau eine Frage erwartet");
        assert_eq!(
            driven.executions, 1,
            "eine Freigabe muss das Werkzeug genau einmal ausführen"
        );
        Ok(())
    }

    /// Ablehnung: der Turn endet trotzdem sauber, das Werkzeug läuft **nicht**.
    #[tokio::test]
    async fn rejected_pause_completes_the_turn_without_running_the_tool() -> TestResult {
        let driven = drive_turn_answering(false).await?;

        match driven.outcome {
            Ok(TurnOutcome::Completed) => {}
            Ok(other) => {
                return Err(TestError::Unexpected(format!(
                    "a rejected pause must still complete the turn, was: {other:?}"
                )));
            }
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "a rejected pause must not fail the turn: {error}"
                )));
            }
        }
        assert_eq!(driven.prompts_seen, 1, "genau eine Frage erwartet");
        assert_eq!(
            driven.executions, 0,
            "eine Ablehnung darf das Werkzeug nicht ausführen"
        );
        Ok(())
    }

    /// Bedingung 3: nur ein ausdrückliches `y` gibt frei.
    #[test]
    fn only_an_explicit_y_approves_a_pending_prompt() -> TestResult {
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
        Ok(())
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
    async fn answering_a_prompt_updates_the_same_cell() -> TestResult {
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
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "die Frage muss den Renderer erreichen: {error}"
                )));
            }
        };
        let tool_cell: SharedToolCell = Arc::new(Mutex::new(ToolCell::started(&call)));
        let mut app = ChatApp::new(Vec::new(), test_sandbox()?, SessionId::new());

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
            other => {
                return Err(TestError::Unexpected(format!(
                    "eine Freigabe muss den Treiber erreichen, war: {other:?}"
                )));
            }
        }
        match tool_cell.lock() {
            Ok(cell) => assert_eq!(cell.approval_note.as_deref(), Some("✓ freigegeben")),
            Err(_) => {
                return Err(TestError::Unexpected(
                    "die Zelle muss nach der Antwort lesbar bleiben".into(),
                ));
            }
        }
        Ok(())
    }

    /// Eine Ablehnung schreibt dieselbe Werkzeugzelle mit der Ablehnungsnotiz
    /// fort — die Nachfolgerin des früheren `Some(false)` an der
    /// `ApprovalPromptCell` (siehe Kommentar oben).
    #[tokio::test]
    async fn rejecting_a_prompt_marks_the_same_cell_as_denied() -> TestResult {
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
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "die Frage muss den Renderer erreichen: {error}"
                )));
            }
        };
        let tool_cell: SharedToolCell = Arc::new(Mutex::new(ToolCell::started(&call)));
        let mut app = ChatApp::new(Vec::new(), test_sandbox()?, SessionId::new());

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
            other => {
                return Err(TestError::Unexpected(format!(
                    "eine Ablehnung muss den Treiber erreichen, war: {other:?}"
                )));
            }
        }
        match tool_cell.lock() {
            Ok(cell) => assert_eq!(cell.approval_note.as_deref(), Some("✗ abgelehnt")),
            Err(_) => {
                return Err(TestError::Unexpected(
                    "die Zelle muss nach der Antwort lesbar bleiben".into(),
                ));
            }
        }
        Ok(())
    }

    /// Fix E (Teil 1b): drückt der Nutzer Ctrl+C, während
    /// `drive_pauses_to_completion` eine Freigabefrage anzeigt, bricht das
    /// jetzt zusätzlich zum bestehenden Ablehnen des Dialogs auch den
    /// laufenden Turn kooperativ ab und scharft den zweistufigen
    /// Beenden-Hinweis. Vor diesem Fix blieben `active_cancel`/`pending_quit`
    /// unberührt — nur der Dialog wurde abgelehnt, der Turn und alle
    /// laufenden Kind-Agenten liefen weiter. Getestet direkt an der
    /// extrahierten Kernlogik [`cancel_turn_and_reject_open_dialogs`], ohne
    /// den vollen `drive_pauses_to_completion`-Ereignis-Loop aufzuziehen.
    #[tokio::test]
    async fn ctrl_c_with_open_approval_dialog_cancels_turn_and_still_rejects_dialog() -> TestResult
    {
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
            Err(error) => {
                return Err(TestError::Unexpected(format!(
                    "die Frage muss den Renderer erreichen: {error}"
                )));
            }
        };
        let tool_cell: SharedToolCell = Arc::new(Mutex::new(ToolCell::started(&call)));
        let mut app = ChatApp::new(Vec::new(), test_sandbox()?, SessionId::new());
        let cancel = CancelToken::new();
        app.active_cancel = Some(cancel.clone());
        app.pending_turns
            .push_back("noch nicht gesendet".to_owned());
        let mut pending = Some(PendingApprovalPrompt {
            prompt,
            tool_cell: Arc::clone(&tool_cell),
        });
        let mut dialog_shown_at = Some(Instant::now());
        let mut host_permit_shown_at: Option<Instant> = None;

        cancel_turn_and_reject_open_dialogs(
            &mut app,
            &mut pending,
            &mut dialog_shown_at,
            &mut host_permit_shown_at,
        )
        .await;

        assert!(
            cancel.is_cancelled(),
            "Ctrl+C muss den Turn auch bei offenem Freigabe-Dialog kooperativ abbrechen"
        );
        assert!(
            matches!(
                app.pending_quit,
                Some(QuitArm {
                    label: "Ctrl+C",
                    ..
                })
            ),
            "Ctrl+C muss den zweistufigen Beenden-Hinweis scharfstellen"
        );
        assert_eq!(
            app.pending_turns.front().map(String::as_str),
            Some("noch nicht gesendet"),
            "abgeschickte Nachrichten bleiben für die Auslieferung nach dem Abbruch"
        );
        assert!(app.queue_kept_at.is_some());
        assert!(pending.is_none(), "die Frage muss konsumiert sein");
        assert!(dialog_shown_at.is_none());
        assert!(app.pending_approval_dialog.is_none());

        // Bestehendes Ablehnen bleibt zusätzlich bestehen, wird nicht ersetzt.
        match handler.await_resolution(&request).await {
            ApprovalResolution::Reject { reason } => {
                assert_eq!(reason, REASON_OPERATOR_CANCELLED);
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "Ctrl+C muss den Dialog weiterhin ablehnen, war: {other:?}"
                )));
            }
        }
        match tool_cell.lock() {
            Ok(cell) => assert_eq!(cell.approval_note.as_deref(), Some("✗ abgelehnt")),
            Err(_) => {
                return Err(TestError::Unexpected(
                    "die Zelle muss nach der Antwort lesbar bleiben".into(),
                ));
            }
        }
        Ok(())
    }

    /// Bedingung 7: die Kern-Ursache bleibt in der Fehlermeldung erhalten.
    #[test]
    fn approval_driver_errors_keep_their_cause() -> TestResult {
        let error = ApprovalDriverError::MissingPendingApproval {
            session: "session-42".to_owned(),
        };
        let expected = error.to_string();

        match tui_error_from_approval_driver(error) {
            TuiError::Core(message) => {
                assert!(message.contains("session-42"));
                assert!(message.contains(&expected));
            }
            TuiError::Io(_) => {
                return Err(TestError::Unexpected(
                    "ein Treiberfehler ist kein Terminal-I/O-Fehler".into(),
                ));
            }
            TuiError::ContextProviderRegistration(_) => {
                return Err(TestError::Unexpected(
                    "ein Treiberfehler ist kein Namensraum-Konflikt".into(),
                ));
            }
        }
        Ok(())
    }

    /// TUI-Berechtigungen (E4: lokaler Principal-Tier `Operator`, siehe
    /// CONTRACTS-W2d2 §4). W2d-2/T2b: `LOCAL_TUI_OPERATION_PERMISSION` ist mit
    /// der Montage entfallen — die Produktionsfläche löst die Stufe jetzt über
    /// `runtime_commands::caller_tier(rt.principal())` auf, deren Ergebnis für
    /// den lokalen TUI-Principal laut E4 `PermissionTier::Operator` ist; dieser
    /// Test benutzt denselben Wert direkt. `CommandServices` ist durch die
    /// Closure `F: FnOnce() -> ServiceMap` ersetzt (CE, CONTRACTS-W2d2 §1.2).
    #[tokio::test]
    async fn local_tui_permissions_are_accessible_but_maintainer_commands_are_blocked() -> TestResult
    {
        let sandbox = test_sandbox()?;
        let operations = test_operations();
        let adapters = operations
            .iter()
            .flat_map(|operation| CommandAdapter::from_operation(Arc::clone(operation)))
            .collect::<Vec<_>>();
        let controller = Arc::new(TuiSessionController::new());

        // `build_services` (command_exec.rs) spiegelt die Produktionsfläche
        // `RuntimeServices::service_map` bewusst nicht 1:1 — es fehlt dort die
        // `ApprovalModeCell` (siehe `harw-runtime/src/services.rs::assemble`,
        // wo `self.parts.approval_mode.clone()` unbedingt auf jeder Fläche
        // eingetragen wird). `/permissions` liest/schreibt den Freigabemodus
        // ausschließlich über `ctx.service::<ApprovalModeCell>()`
        // (`harw-ops/src/permissions.rs`), also braucht dieser Test dieselbe
        // Zelle wie die echte Laufzeit — hier direkt nach `build_services`
        // nachgetragen, ohne `command_exec.rs` selbst zu ändern.
        let permissions = execute_command_as(
            &adapters,
            &sandbox,
            &SessionId::new(),
            harw_operations::PermissionTier::Operator,
            "/permissions",
            || {
                let mut services = build_services(&adapters, None, None, &controller, None, None);
                services.insert(ApprovalModeCell::default());
                services
            },
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
            || build_services(&adapters, None, None, &controller, None, None),
        )
        .await;

        assert_eq!(
            output,
            "Berechtigung verweigert: /plugins erfordert Maintainer; aktuelle Stufe ist Operator"
        );
        Ok(())
    }

    /// `/mode` an der Turn-Grenze (AP W5-05). W2d-2/T2b: die Session wird über
    /// `AgentSession::new_with_id(..).with_spawn_context(..).with_turn_event_sink(..)`
    /// gebaut statt über das gelöschte `build_tui_agent_session`.
    #[test]
    fn mode_request_is_applied_at_the_turn_boundary() -> TestResult {
        let sandbox = test_sandbox()?;
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
            return Err(TestError::Unexpected(format!(
                "`/mode explore` muss angenommen werden: {error}"
            )));
        }
        assert_eq!(
            session.mode(),
            InteractionMode::Chat,
            "ein laufender Turn darf seinen Modus nicht unter sich wechseln"
        );

        assert!(app.apply_pending_controller_state(&mut session));

        assert_eq!(session.mode(), InteractionMode::Explore);
        assert_eq!(app.active_mode(), InteractionMode::Explore);
        Ok(())
    }

    /// Ein unbekannter Modusname wird abgewiesen statt still auf den Default zu
    /// fallen; die Session bleibt unangetastet.
    #[test]
    fn unknown_mode_names_never_change_the_session() -> TestResult {
        let sandbox = test_sandbox()?;
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
        Ok(())
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
    fn three_child_events_produce_a_single_sub_agent_cell() -> TestResult {
        let mut app = test_chat_app()?;
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
        Ok(())
    }

    /// Teil 1 (`recursive-cooking-lobster.md`): der kurze Ack-Text vor einem
    /// Tool-Aufruf (`MessagePhase::Commentary`) muss sofort als sichtbare
    /// Assistant-Zelle erscheinen, statt erst am Turn-Ende über
    /// `reveal_reply`.
    #[test]
    fn commentary_assistant_message_renders_immediately() -> TestResult {
        use harw_protocol::items::AssistantMessageItem;

        let mut app = test_chat_app()?;
        let mut state = TurnEventState::default();
        let before = app.cells_len();

        let handled = handle_turn_event(
            &mut app,
            &mut state,
            TurnEvent::ItemAdded {
                turn_id: TurnId::new(),
                item: TurnItem::AssistantMessage(AssistantMessageItem {
                    id: ItemId::new(),
                    content: vec![ContentPart::Text {
                        text: "Ich prüfe die Konfiguration …".to_owned(),
                    }],
                    phase: Some(harw_types::MessagePhase::Commentary),
                }),
            },
        );

        assert!(handled, "Commentary-Nachrichten müssen ein Redraw auslösen");
        assert_eq!(
            app.cells_len(),
            before + 1,
            "Commentary-Nachricht muss eine neue Zelle anlegen"
        );
        let rendered = rendered_cells(&app);
        assert!(
            rendered.contains("Ich prüfe die Konfiguration …"),
            "Commentary-Text fehlt im gerenderten Verlauf: {rendered:?}"
        );
        Ok(())
    }

    /// Gegenprobe zu `commentary_assistant_message_renders_immediately`:
    /// `phase == FinalAnswer` darf **keine** zusätzliche Zelle erzeugen — die
    /// finale Antwort bleibt exklusiv `reveal_reply` am Turn-Ende vorbehalten,
    /// sonst entstünde eine Doppelanzeige.
    #[test]
    fn final_answer_assistant_message_is_not_rendered_live() -> TestResult {
        use harw_protocol::items::AssistantMessageItem;

        let mut app = test_chat_app()?;
        let mut state = TurnEventState::default();
        let before = app.cells_len();

        let handled = handle_turn_event(
            &mut app,
            &mut state,
            TurnEvent::ItemAdded {
                turn_id: TurnId::new(),
                item: TurnItem::AssistantMessage(AssistantMessageItem {
                    id: ItemId::new(),
                    content: vec![ContentPart::Text {
                        text: "Das ist die finale Antwort.".to_owned(),
                    }],
                    phase: Some(harw_types::MessagePhase::FinalAnswer),
                }),
            },
        );

        assert!(
            !handled,
            "FinalAnswer darf keinen Redraw über handle_turn_event auslösen"
        );
        assert_eq!(
            app.cells_len(),
            before,
            "FinalAnswer darf keine zusätzliche Zelle anlegen (bleibt reveal_reply vorbehalten)"
        );
        Ok(())
    }

    // ------------------------------------------------------------------
    // Bugfix: doppelte/verwaiste Antwortanzeige (`latest_final_reply`,
    // `suppress_if_matches_commentary`) — siehe Bugfix-Abschnitt der
    // `drive_turn_animated`-Doku.
    // ------------------------------------------------------------------

    fn tool_result_pair(history: &mut ConversationHistory) {
        let call_id = ToolCallId::new();
        history.push_tool_call(call_id.clone(), "fs.read", json!({ "path": "/tmp/x" }));
        history.push_tool_result(call_id, ToolCallResult::success(json!({ "ok": true })), 5);
    }

    /// Fall (a): endet der Turn mit einer bereits live gezeigten
    /// `Commentary`-Nachricht, gefolgt nur von Tool-Aufrufen, darf
    /// `latest_final_reply` sie nicht ein zweites Mal liefern.
    #[test]
    fn latest_final_reply_commentary_then_tool_calls_returns_none() -> TestResult {
        let mut history = ConversationHistory::new();
        history.push_assistant_text(
            "Ich prüfe die Konfiguration …",
            Some(harw_types::MessagePhase::Commentary),
        );
        tool_result_pair(&mut history);

        assert_eq!(latest_final_reply(&history, 0), None);
        Ok(())
    }

    /// Fall (b): eine finale (nicht-`Commentary`) Antwort wird genau einmal
    /// geliefert — unabhängig von einer vorherigen `Commentary`-Nachricht
    /// desselben Turns.
    #[test]
    fn latest_final_reply_final_answer_is_returned_once() -> TestResult {
        let mut history = ConversationHistory::new();
        history.push_assistant_text(
            "Ich prüfe die Konfiguration …",
            Some(harw_types::MessagePhase::Commentary),
        );
        history.push_assistant_text(
            "Die finale Antwort.",
            Some(harw_types::MessagePhase::FinalAnswer),
        );

        assert_eq!(
            latest_final_reply(&history, 0),
            Some("Die finale Antwort.".to_owned())
        );
        Ok(())
    }

    /// Eine Assistant-Nachricht ganz ohne `phase` gilt — wie `FinalAnswer` —
    /// als enthüllbar (siehe `handle_turn_event`s Doku: „bei `FinalAnswer`
    /// oder fehlender Phase bleibt die Anzeige `reveal_reply` vorbehalten").
    #[test]
    fn latest_final_reply_missing_phase_is_returned() -> TestResult {
        let mut history = ConversationHistory::new();
        history.push_assistant_text("Antwort ohne Phasen-Metadaten.", None);

        assert_eq!(
            latest_final_reply(&history, 0),
            Some("Antwort ohne Phasen-Metadaten.".to_owned())
        );
        Ok(())
    }

    /// Fall (c): fügt der aktuelle Turn gar keine neue Assistant-Nachricht
    /// hinzu (z. B. ein zusammengefasster Warteschlangen-Turn), darf die
    /// Antwort des VORHERIGEN Turns nicht erneut geliefert werden —
    /// `since_len` grenzt die Suche strikt auf die neuen Items ein.
    #[test]
    fn latest_final_reply_no_new_assistant_item_returns_none() -> TestResult {
        let mut history = ConversationHistory::new();
        history.push_assistant_text(
            "Antwort des vorherigen Turns.",
            Some(harw_types::MessagePhase::FinalAnswer),
        );
        let since_len = history.len();
        tool_result_pair(&mut history);

        assert_eq!(latest_final_reply(&history, since_len), None);
        Ok(())
    }

    /// Zweite Verteidigungslinie: ein `reply`, der exakt dem zuletzt live
    /// gepushten Commentary-Text entspricht, wird unterdrückt.
    #[test]
    fn suppress_if_matches_commentary_dedupes_identical_text() -> TestResult {
        assert_eq!(
            suppress_if_matches_commentary(Some("gleicher Text".to_owned()), Some("gleicher Text")),
            None
        );
        Ok(())
    }

    /// Unterschiedlicher Text, kein Commentary-Wächter gesetzt, oder gar
    /// keine Antwort — in allen drei Fällen bleibt `reply` unverändert.
    #[test]
    fn suppress_if_matches_commentary_keeps_unrelated_values() -> TestResult {
        assert_eq!(
            suppress_if_matches_commentary(Some("neuer Text".to_owned()), Some("alter Text")),
            Some("neuer Text".to_owned())
        );
        assert_eq!(
            suppress_if_matches_commentary(Some("neuer Text".to_owned()), None),
            Some("neuer Text".to_owned())
        );
        assert_eq!(
            suppress_if_matches_commentary(None, Some("alter Text")),
            None
        );
        Ok(())
    }

    /// A6: eine abgeschlossene Werkzeugausführung schreibt die Dauer nur in
    /// den `ExportEntry::ToolResult`-Eintrag; der zugehörige
    /// `ToolCall`-Eintrag bleibt ohne `duration_ms` (kein Backfill mehr).
    #[test]
    fn tool_call_completion_leaves_the_call_entry_without_a_duration() -> TestResult {
        let mut app = test_chat_app()?;
        let mut state = TurnEventState::default();
        let turn_id = TurnId::new();
        let call_id = ToolCallId::new();

        assert!(handle_turn_event(
            &mut app,
            &mut state,
            TurnEvent::ToolCallRequested {
                turn_id: turn_id.clone(),
                call_id: call_id.clone(),
                tool_name: "fs.read".to_owned(),
                arguments: json!({ "path": "/tmp/x" }),
            }
        ));
        assert!(handle_turn_event(
            &mut app,
            &mut state,
            TurnEvent::ToolCallCompleted {
                turn_id,
                call_id: call_id.clone(),
                result: ToolCallResult::error("nicht gefunden"),
                duration_ms: 42,
            }
        ));

        let call_duration = app.export_entries.iter().find_map(|entry| match entry {
            ExportEntry::ToolCall {
                call_id: id,
                duration_ms,
                ..
            } if *id == call_id.to_string() => Some(*duration_ms),
            _ => None,
        });
        assert_eq!(
            call_duration,
            Some(None),
            "ToolCall-Eintrag muss existieren, aber ohne Dauer"
        );

        let result_duration = app.export_entries.iter().find_map(|entry| match entry {
            ExportEntry::ToolResult {
                call_id: id,
                duration_ms,
                ..
            } if *id == call_id.to_string() => Some(*duration_ms),
            _ => None,
        });
        assert_eq!(
            result_duration,
            Some(Some(42)),
            "ToolResult-Eintrag muss die Dauer tragen"
        );
        Ok(())
    }

    /// A6 (Hydrate-Pfad): beim Laden einer durablen Historie bekommt der
    /// `ExportEntry::ToolCall`-Eintrag `trust` nachgetragen, aber keine Dauer
    /// mehr — die Dauer bleibt exklusiv am `ExportEntry::ToolResult`.
    #[test]
    fn hydrated_tool_result_leaves_the_call_entry_without_a_duration() -> TestResult {
        use harw_protocol::items::{ResultTrust, ToolCallItem, ToolCallResult, ToolResultItem};
        use harw_types::{ItemId, ToolCallId};

        let call_id = ToolCallId::new();
        let mut history = ConversationHistory::new();
        history.push(TurnItem::ToolCall(ToolCallItem {
            id: ItemId::new(),
            call_id: call_id.clone(),
            tool_name: "fs.read".to_owned(),
            arguments: json!({ "path": "/tmp/x" }),
        }));
        history.push(TurnItem::ToolResult(ToolResultItem {
            id: ItemId::new(),
            call_id: call_id.clone(),
            result: ToolCallResult::error("nicht gefunden"),
            duration_ms: 17,
            trust: ResultTrust::Runtime,
        }));

        let (event_tx, _event_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut session = AgentSession::new_with_id(
            SessionId::from_str("hydrate-duration"),
            AgentRole::Assistant,
            None,
            ExtensionRegistry::builder().build(),
            event_tx,
        );
        let mut app = test_chat_app()?;
        install_loaded_history(&mut session, &mut app, history);

        let call_entry = app.export_entries.iter().find_map(|entry| match entry {
            ExportEntry::ToolCall {
                call_id: id,
                duration_ms,
                trust,
                ..
            } if *id == call_id.to_string() => Some((*duration_ms, trust.clone())),
            _ => None,
        });
        assert_eq!(
            call_entry,
            Some((None, Some(export_trust(ResultTrust::Runtime)))),
            "ToolCall-Eintrag: keine Dauer, aber nachgetragenes Vertrauen"
        );

        let result_duration = app.export_entries.iter().find_map(|entry| match entry {
            ExportEntry::ToolResult {
                call_id: id,
                duration_ms,
                ..
            } if *id == call_id.to_string() => Some(*duration_ms),
            _ => None,
        });
        assert_eq!(result_duration, Some(Some(17)));
        Ok(())
    }

    /// Ein Fortschritt für ein unbekanntes Kind erzeugt **keine** Zelle.
    #[test]
    fn progress_for_an_unknown_child_creates_no_cell() -> TestResult {
        let mut app = test_chat_app()?;
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
        Ok(())
    }

    /// Ohne Plan-Dienste geht die Plan-Information nicht verloren — sie wird nur
    /// einzeilig statt als Graph gezeigt.
    #[test]
    fn plan_updates_degrade_to_a_system_line_without_plan_services() -> TestResult {
        let mut app = test_chat_app()?;
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
        Ok(())
    }

    /// `ModeChanged` zieht den internen Modus nach; ein unbekannter Name nicht.
    #[test]
    fn mode_changed_events_update_the_active_mode() -> TestResult {
        let mut app = test_chat_app()?;
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
        Ok(())
    }

    /// Nur die exakte Zeile `/goal check` löst eine [`GoalCell`] aus.
    #[test]
    fn only_goal_check_requests_a_goal_cell() -> TestResult {
        assert!(is_goal_check_command("/goal check"));
        assert!(is_goal_check_command("  /goal   check  "));
        assert!(!is_goal_check_command("/goal set"));
        assert!(!is_goal_check_command("/goal"));
        assert!(!is_goal_check_command("/plan check"));
        Ok(())
    }

    /// Ohne durchgereichte Plan-Dienste gibt es keinen Zielstand — und keinen
    /// Fehler.
    #[test]
    fn goal_cell_is_absent_without_plan_services() -> TestResult {
        let app = test_chat_app()?;

        assert!(goal_cell_for_command(&app, "/goal check").is_none());
        Ok(())
    }

    // ── Welle 4a/7b: Picker-Konsolidierung (`/model`, `/uia-model`,
    // `/uia-worker-model`, `/effort`, `/uia-effort`) ──────────────────────

    /// Ohne Konfiguration (Test-`ChatApp` ohne Runtime-Montage) wird kein
    /// leerer Dialog geöffnet, sondern eine klare Systemzeile angehängt —
    /// für jedes der drei `PickerTarget`-Ziele.
    #[test]
    fn open_model_switch_picker_without_config_pushes_system_line_not_overlay() -> TestResult {
        for target in [
            PickerTarget::Orchestrator,
            PickerTarget::Uia,
            PickerTarget::UiaWorker {
                fixed_provider: "anthropic".to_owned(),
            },
        ] {
            let mut app = test_chat_app()?;
            app.open_model_switch_picker(target);
            assert!(app.overlay.is_none());
        }
        Ok(())
    }

    /// `Accept` auf [`Overlay::ModelSwitch`] synthetisiert je nach `target`
    /// die richtige Befehlszeile — `/model switch`, `/uia-model switch` bzw.
    /// `/uia-worker-model switch` — und schließt das Overlay.
    #[test]
    fn model_switch_accept_emits_the_command_line_for_each_target() -> TestResult {
        let providers = vec![ProviderEntry {
            id: "anthropic".to_owned(),
            label: "Anthropic".to_owned(),
        }];
        let models = vec![(
            "anthropic".to_owned(),
            vec![ModelEntry {
                id: "claude-sonnet".to_owned(),
                label: "Claude Sonnet".to_owned(),
            }],
        )];

        let cases = [
            (PickerTarget::Orchestrator, "/model switch claude-sonnet"),
            (PickerTarget::Uia, "/uia-model switch claude-sonnet"),
            (
                PickerTarget::UiaWorker {
                    fixed_provider: "anthropic".to_owned(),
                },
                "/uia-worker-model switch claude-sonnet",
            ),
        ];

        for (target, expected) in cases {
            let mut app = test_chat_app()?;
            let picker =
                ModelSwitchPicker::new(target, providers.clone(), models.clone(), None, None)
                    .ok_or(TestError::Missing("providers fixture ist nicht leer"))?;
            app.overlay = Some(Overlay::ModelSwitch(Box::new(picker)));

            let (bus, mut receiver) = harw_event_channel();
            // Erster Enter wählt (bzw. bestätigt) den einzigen Provider und
            // wechselt in die Modell-Stufe (bei `UiaWorker` ist die
            // Provider-Stufe bereits übersprungen, ein Enter genügt dort
            // direkt für das Modell).
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                &bus,
            );
            if app.overlay.is_some() {
                handle_key(
                    &mut app,
                    KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                    &bus,
                );
            }

            assert!(
                app.overlay.is_none(),
                "Overlay muss nach Accept geschlossen sein"
            );
            match receiver.try_recv() {
                Ok(HarwEvent::Command(command)) => assert_eq!(command, expected),
                other => {
                    return Err(TestError::Unexpected(format!(
                        "erwartete HarwEvent::Command({expected:?}), bekam {other:?}"
                    )));
                }
            }
        }
        Ok(())
    }

    /// `PickerTarget::Role` synthetisiert `/models set <rolle> <provider>/<modell>`.
    #[test]
    fn model_switch_accept_for_role_target_emits_models_set() -> TestResult {
        let providers = vec![ProviderEntry {
            id: "anthropic".to_owned(),
            label: "Anthropic".to_owned(),
        }];
        let models = vec![(
            "anthropic".to_owned(),
            vec![ModelEntry {
                id: "claude-sonnet".to_owned(),
                label: "Claude Sonnet".to_owned(),
            }],
        )];
        let target = PickerTarget::Role {
            role: harw_config::ModelRole::Orchestrator,
        };
        let expected = target.command_line("anthropic", "claude-sonnet");
        let picker = ModelSwitchPicker::new(target, providers, models, None, None)
            .ok_or(TestError::Missing("providers fixture ist nicht leer"))?;
        let mut app = test_chat_app()?;
        app.overlay = Some(Overlay::ModelSwitch(Box::new(picker)));
        let (bus, mut receiver) = harw_event_channel();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        handle_key(&mut app, enter, &bus);
        if app.overlay.is_some() {
            handle_key(&mut app, enter, &bus);
        }
        assert!(app.overlay.is_none());
        match receiver.try_recv() {
            Ok(HarwEvent::Command(command)) => {
                assert_eq!(command, expected);
                assert!(command.starts_with("/models set "), "{command}");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartete /models set, bekam {other:?}"
                )));
            }
        }
        Ok(())
    }

    // ── T21: lokale Befehle, Ansichten, Werkbank, Erwähnungen ──────────

    /// `/help` öffnet die generische Hilfe-Ansicht, `/workbench` blendet die
    /// Werkbank ein und markiert sie zum Laden.
    #[test]
    fn local_intercept_opens_views_and_toggles_workbench() -> TestResult {
        let mut app = test_chat_app()?;
        let (bus, _receiver) = harw_event_channel();

        let help = local_intercept_for(&app, "/help").ok_or(TestError::Missing("help"))?;
        assert!(apply_local_intercept(&mut app, help, &bus).is_none());
        assert!(matches!(app.overlay, Some(Overlay::View(_))));

        app.overlay = None;
        let workbench =
            local_intercept_for(&app, "/workbench").ok_or(TestError::Missing("workbench"))?;
        assert!(apply_local_intercept(&mut app, workbench, &bus).is_none());
        assert!(app.panels.workbench_visible);
        assert!(app.workbench_needs_refresh());

        // Befehle mit Argumenten bleiben beim regulären Dispatch.
        assert!(local_intercept_for(&app, "/model switch x").is_none());
        assert!(local_intercept_for(&app, "/status").is_none());
        Ok(())
    }

    /// `#notiz` und `/sessions` werden zu Slash-Zeilen umgeschrieben und
    /// erneut über den Command-Kanal geschickt; `@rolle` wird Chat.
    #[test]
    fn local_intercept_rewrites_notes_and_routes_mentions_to_chat() -> TestResult {
        let mut app = test_chat_app()?;
        let (bus, mut receiver) = harw_event_channel();

        let note = local_intercept_for(&app, "#Merken").ok_or(TestError::Missing("note"))?;
        assert!(apply_local_intercept(&mut app, note, &bus).is_none());
        match receiver.try_recv() {
            Ok(HarwEvent::Command(command)) => assert!(
                command == "/diary note Merken" || command == "/memory record Merken",
                "{command}"
            ),
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartete Rewrite, bekam {other:?}"
                )));
            }
        }

        let sessions =
            local_intercept_for(&app, "/sessions").ok_or(TestError::Missing("sessions"))?;
        assert!(apply_local_intercept(&mut app, sessions, &bus).is_none());
        assert!(matches!(
            receiver.try_recv(),
            Ok(HarwEvent::Command(command)) if command == "/resume"
        ));

        let role = KNOWN_ROLES
            .first()
            .ok_or(TestError::Missing("bekannte Rolle"))?;
        let mention = local_intercept_for(&app, &format!("@{role} bitte prüfen"))
            .ok_or(TestError::Missing("mention"))?;
        assert!(apply_local_intercept(&mut app, mention, &bus).is_none());
        let queued = app
            .pending_turns
            .pop_front()
            .ok_or(TestError::Missing("chat turn"))?;
        assert!(queued.contains(role), "{queued}");
        assert!(queued.contains("bitte prüfen"), "{queued}");
        Ok(())
    }

    /// `/clear` leert nur die Anzeige; `/verbose` schaltet die Werkzeug-
    /// Ausführlichkeit um.
    #[test]
    fn local_clear_and_verbose_change_display_state() -> TestResult {
        let mut app = test_chat_app()?;
        let (bus, _receiver) = harw_event_channel();
        app.push_line(Role::User, "hallo");
        app.push_line(Role::Assistant, "welt");
        let exported = app.export_entries.len();

        let clear = local_intercept_for(&app, "/clear").ok_or(TestError::Missing("clear"))?;
        apply_local_intercept(&mut app, clear, &bus);
        // Nur die Hinweiszeile bleibt; der Export-Verlauf ist unverändert.
        assert_eq!(app.cells_len(), 1);
        assert_eq!(app.export_entries.len(), exported);

        let before = app.tool_verbosity;
        let verbose = local_intercept_for(&app, "/verbose").ok_or(TestError::Missing("verbose"))?;
        apply_local_intercept(&mut app, verbose, &bus);
        assert_ne!(app.tool_verbosity, before);
        Ok(())
    }

    /// F6 öffnet das Kanban-Board als generische Ansicht und reiht dessen
    /// Initial-Abruf ein; ohne Runtime landet ein Fehler in der Ansicht,
    /// nicht im Chat.
    #[tokio::test]
    async fn view_hotkey_opens_kanban_and_fetch_does_not_touch_chat() -> TestResult {
        let mut app = test_chat_app()?;
        let (bus, _receiver) = harw_event_channel();
        assert!(handle_key(
            &mut app,
            KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE),
            &bus
        ));
        assert!(matches!(app.overlay, Some(Overlay::View(_))));
        assert!(app.pending_fetches.contains(&DataFetch::Overlay {
            command: crate::kanban_board::REFRESH_COMMAND.to_owned(),
            generation: app.overlay_generation,
        }));

        let cells = app.cells_len();
        assert!(process_pending_fetches(&mut app).await);
        assert_eq!(
            app.cells_len(),
            cells,
            "Datenabruf schreibt nie in den Chat"
        );
        assert!(app.pending_fetches.is_empty());
        assert!(!process_pending_fetches(&mut app).await);
        Ok(())
    }

    /// F9 öffnet das Matrix-Panel; Matrix-Ereignisse vom Bus gehen an die
    /// Ansicht (nicht an den Agenten-Monitor) und reihen ihr Nachladen ein.
    #[test]
    fn matrix_hotkey_and_live_events_queue_refresh() -> TestResult {
        let mut app = test_chat_app()?;
        let (bus, _receiver) = harw_event_channel();
        let hub = harw_core::AgentEventHub::default();
        app.attach_agent_events(&hub);

        // Ohne offene Ansicht: kein Abruf, kein Monitor-Eintrag.
        hub.publish_matrix(
            app.session_id.clone(),
            "run-1",
            serde_json::json!({"round": 1, "text": "x"}),
        );
        app.drain_agent_events();
        assert!(app.pending_fetches.is_empty());

        assert!(handle_key(
            &mut app,
            KeyEvent::new(KeyCode::F(9), KeyModifiers::NONE),
            &bus
        ));
        assert!(matches!(app.overlay, Some(Overlay::View(_))));
        let fetch = DataFetch::Overlay {
            command: crate::matrix_view::REFRESH_COMMAND.to_owned(),
            generation: app.overlay_generation,
        };
        assert!(app.pending_fetches.contains(&fetch));
        app.pending_fetches.clear();

        hub.publish_matrix(
            app.session_id.clone(),
            "run-1",
            serde_json::json!({"round": 2, "kind": "roll", "text": "Würfel"}),
        );
        assert!(app.drain_agent_events());
        assert_eq!(app.pending_fetches, vec![fetch]);

        // Andere Ansichten erhalten das Ereignis, laden aber nicht nach.
        app.open_overlay_view(Box::new(KanbanBoard::new()));
        app.pending_fetches.clear();
        hub.publish_matrix(app.session_id.clone(), "run-1", serde_json::json!({}));
        app.drain_agent_events();
        assert!(app.pending_fetches.is_empty());
        Ok(())
    }

    /// Knowledge-Ereignisse markieren nur die passenden Panels als veraltet:
    /// `kanban` lädt ein offenes Board nach, `workbench` das Werkbank-Panel,
    /// fremde Flächen lassen die offene Ansicht in Ruhe.
    #[test]
    fn knowledge_events_mark_the_matching_panels_stale() -> TestResult {
        let mut app = test_chat_app()?;
        let hub = harw_core::AgentEventHub::default();
        app.attach_agent_events(&hub);
        app.open_overlay_view(Box::new(KanbanBoard::new()));
        app.pending_fetches.clear();

        hub.publish_knowledge(app.session_id.clone(), "palace", None);
        assert!(!app.drain_agent_events());
        assert!(app.pending_fetches.is_empty());

        hub.publish_knowledge(
            app.session_id.clone(),
            "kanban",
            Some("default/card-1".to_owned()),
        );
        assert!(app.drain_agent_events());
        assert_eq!(
            app.pending_fetches,
            vec![DataFetch::Overlay {
                command: crate::kanban_board::REFRESH_COMMAND.to_owned(),
                generation: app.overlay_generation,
            }]
        );

        app.workbench
            .apply_data(&serde_json::json!({"scope": "session:s"}));
        assert!(!app.workbench.is_stale());
        hub.publish_knowledge(app.session_id.clone(), "workbench", None);
        assert!(app.drain_agent_events());
        assert!(app.workbench.is_stale());
        Ok(())
    }

    /// Ein Abruf für eine inzwischen ersetzte Ansicht wird verworfen.
    #[tokio::test]
    async fn stale_overlay_fetch_is_dropped_after_view_replaced() -> TestResult {
        let mut app = test_chat_app()?;
        app.open_overlay_view(Box::new(KanbanBoard::new()));
        let stale_generation = app.overlay_generation;
        app.open_overlay_view(Box::new(HelpOverlay::new(
            &app.command_registry,
            &app.key_bindings,
            HelpTab::Keys,
        )));
        assert_ne!(stale_generation, app.overlay_generation);
        assert!(app.pending_fetches.iter().all(|fetch| matches!(
            fetch,
            DataFetch::Overlay { generation, .. } if *generation == stale_generation
        )));
        // Läuft ohne Wirkung auf die neue Ansicht durch.
        process_pending_fetches(&mut app).await;
        assert!(matches!(app.overlay, Some(Overlay::View(_))));
        Ok(())
    }

    /// Generische Ansicht: `Prefill` schließt und füllt den Composer, `Run`
    /// schickt die Zeile über den Bus und lässt die Ansicht offen.
    #[test]
    fn overlay_outcomes_prefill_and_run() -> TestResult {
        let mut app = test_chat_app()?;
        let (bus, mut receiver) = harw_event_channel();
        app.open_overlay_view(Box::new(KanbanBoard::new()));

        apply_overlay_outcome(
            &mut app,
            OverlayOutcome::Run("/kanban show".to_owned()),
            &bus,
        );
        assert!(matches!(app.overlay, Some(Overlay::View(_))));
        assert!(matches!(
            receiver.try_recv(),
            Ok(HarwEvent::Command(command)) if command == "/kanban show"
        ));

        apply_overlay_outcome(
            &mut app,
            OverlayOutcome::Prefill("/kanban add ".to_owned()),
            &bus,
        );
        assert!(app.overlay.is_none());
        assert_eq!(app.input(), "/kanban add ");
        Ok(())
    }

    /// Werkbank-Tasten: `n` füllt den Composer und gibt den Fokus ab, `R`
    /// lädt still neu (kein Chat-Befehl).
    #[test]
    fn workbench_focus_keys_prefill_and_refresh() -> TestResult {
        let mut app = test_chat_app()?;
        app.panels.workbench_visible = true;
        app.panels.focus = crate::panes::PaneFocus::Workbench;
        app.workbench.apply_error("noch nicht geladen".to_owned());
        assert!(!app.workbench.is_stale());

        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        assert_eq!(
            handle_panel_key(&mut app, key(KeyCode::Char('R'))),
            Some(true)
        );
        assert!(app.workbench.is_stale());
        assert!(app.pending_commands.is_empty());

        assert_eq!(
            handle_panel_key(&mut app, key(KeyCode::Char('n'))),
            Some(true)
        );
        assert_eq!(app.input(), crate::workbench_pane::NOTE_PREFILL);
        assert_eq!(app.panels.focus, crate::panes::PaneFocus::Chat);
        Ok(())
    }

    /// Stufe 2 des Befehls-Popups: nach `/model ` erscheinen die
    /// Unterkommandos; Enter ohne Suchtext sendet ab, mit Suchtext wird
    /// vervollständigt.
    #[test]
    fn subcommand_popup_after_complete_command() -> TestResult {
        let mut app = test_chat_app()?;
        let Some(spec) = app.command_registry.find("model") else {
            return Ok(());
        };
        if spec.subcommands.is_empty() {
            return Ok(());
        }

        app.input.insert_str("/model ");
        app.sync_popup();
        assert!(matches!(
            app.command_popup.as_ref().map(CommandPopup::mode),
            Some(PopupMode::Subcommand { .. })
        ));
        let (bus, mut receiver) = harw_event_channel();
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        assert!(handle_key(&mut app, enter, &bus));
        assert!(matches!(
            receiver.try_recv(),
            Ok(HarwEvent::Command(command)) if command.trim() == "/model"
        ));

        app.input.insert_str("/model sh");
        app.sync_popup();
        assert!(handle_key(&mut app, enter, &bus));
        assert_eq!(app.input(), "/model show ");
        assert!(receiver.try_recv().is_err());

        // Argumente nach dem Unterkommando schließen das Popup.
        app.input.clear();
        app.input.insert_str("/model switch x");
        app.sync_popup();
        assert!(app.command_popup.is_none());
        Ok(())
    }

    #[test]
    fn subcommand_query_is_empty_detects_bare_command_with_space() {
        assert!(subcommand_query_is_empty("/model "));
        assert!(subcommand_query_is_empty("/model"));
        assert!(!subcommand_query_is_empty("/model s"));
    }

    /// Busy-sichere lokale Befehle wirken während eines Turns sofort
    /// (`Local`); Sitzungswechsel, Transkript-Leeren und Befehle ohne
    /// busy-sichere Klasse werden zurückgestellt (Runde 4, Teil H).
    #[test]
    fn busy_local_commands_run_locally_and_session_commands_are_deferred() -> TestResult {
        let local_app = || -> TestResult<ChatApp> {
            let mut app = test_chat_app()?;
            app.command_registry = CommandRegistry::built_in()
                .map_err(ctx("built_in"))?
                .with_local_specs(crate::command_catalog::local_command_specs());
            Ok(app)
        };
        let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        for raw in [
            "/help",
            "/models",
            "/workbench",
            "/keys",
            "/whoami",
            "/agent",
        ] {
            let mut app = local_app()?;
            app.input.insert_str(raw);
            assert_eq!(
                queue_busy_key(&mut app, enter),
                BusyKeyOutcome::Local,
                "{raw}"
            );
            assert!(app.deferred_input.is_empty(), "{raw}");
            assert!(app.busy_jobs.queued().is_empty(), "{raw}");
        }
        // `/whoami` schreibt seine Systemzeile sofort, `/help` öffnet das Overlay.
        let mut app = local_app()?;
        app.input.insert_str("/help");
        queue_busy_key(&mut app, enter);
        assert!(app.overlay.is_some());

        for raw in [
            "/clear",
            "/resume",
            "/compact",
            "/tools",
            "/new",
            "/sessions",
        ] {
            let mut app = local_app()?;
            app.input.insert_str(raw);
            assert_eq!(
                queue_busy_key(&mut app, enter),
                BusyKeyOutcome::Redraw,
                "{raw}"
            );
            assert_eq!(
                app.deferred_input.pop_front(),
                Some(TuiEvent::Paste(raw.to_owned())),
                "{raw}"
            );
            assert!(app.busy_jobs.queued().is_empty(), "{raw}");
        }
        Ok(())
    }

    /// Klartext aller Verlaufszellen (Testhilfe).
    fn history_plain(app: &ChatApp) -> String {
        app.cells
            .iter()
            .flat_map(|cell| cell.display_lines(200, style::Theme::Dark))
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Ein offenes Overlay bekommt während eines Turns seine Tasten: Esc
    /// schließt die Hilfe, statt den Turn zu unterbrechen.
    #[test]
    fn busy_keys_reach_open_overlay_and_esc_closes_it_without_interrupt() -> TestResult {
        let mut app = test_chat_app()?;
        app.command_registry = CommandRegistry::built_in()
            .map_err(ctx("built_in"))?
            .with_local_specs(crate::command_catalog::local_command_specs());
        let cancel = CancelToken::new();
        app.active_cancel = Some(cancel.clone());
        app.input.insert_str("/help");
        assert_eq!(
            handle_busy_event(
                &mut app,
                TuiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            ),
            BusyKeyOutcome::Local
        );
        assert!(matches!(app.overlay, Some(Overlay::View(_))));

        // Tippen landet im Overlay, nicht im Composer.
        handle_busy_event(
            &mut app,
            TuiEvent::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
        );
        assert!(app.input.is_empty());

        handle_busy_event(
            &mut app,
            TuiEvent::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );
        assert!(app.overlay.is_none(), "Esc schließt das Overlay");
        assert!(
            !cancel.is_cancelled(),
            "Esc im Overlay unterbricht den Turn nicht"
        );
        assert!(app.cancel_requested_at.is_none());
        Ok(())
    }

    /// Alt+↑ holt die zuletzt eingereihte Nachricht zurück in den leeren
    /// Composer.
    #[test]
    fn busy_alt_up_recalls_last_pending_turn() -> TestResult {
        let mut app = test_chat_app()?;
        app.pending_turns.push_back("erste".to_owned());
        app.pending_turns.push_back("zweite".to_owned());
        let alt_up = TuiEvent::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::ALT));
        assert_eq!(
            handle_busy_event(&mut app, alt_up.clone()),
            BusyKeyOutcome::Redraw
        );
        assert_eq!(app.input.submission_text(), "zweite");
        assert_eq!(app.pending_turns.len(), 1);
        // Mit Text im Composer holt Alt+↑ nichts zurück.
        handle_busy_event(&mut app, alt_up);
        assert_eq!(app.pending_turns.len(), 1);
        Ok(())
    }

    /// App mit allen `harw-ops`-Adaptern und Dispatch ohne Montage.
    fn ops_chat_app() -> TestResult<ChatApp> {
        let mut ops = OperationRegistry::new();
        harw_ops::register_all(&mut ops);
        let adapters: Vec<CommandAdapter> = ops
            .iter()
            .flat_map(|op| CommandAdapter::from_operation(Arc::clone(op)))
            .collect();
        let mut app = ChatApp::new(adapters, test_sandbox()?, SessionId::new());
        app.busy_jobs.enable_dispatch_without_runtime();
        Ok(app)
    }

    /// Ein Picker-Accept während eines Turns (Effort-Auswahl) wird als
    /// `Staged` eingestuft, läuft sofort und merkt die Änderung nur im
    /// Controller vor — die Session selbst ändert sich erst an der
    /// Turn-Grenze (`apply_pending_controller_state`).
    #[tokio::test]
    async fn busy_picker_accept_stages_controller_without_touching_session() -> TestResult {
        let sandbox = test_sandbox()?;
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
        let mut app = ops_chat_app()?;
        app.active_cancel = Some(CancelToken::new());

        app.input.insert_str("/effort");
        let enter = TuiEvent::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            handle_busy_event(&mut app, enter.clone()),
            BusyKeyOutcome::Local
        );
        assert!(matches!(app.overlay, Some(Overlay::EffortChoice { .. })));

        assert_eq!(
            handle_busy_event(&mut app, enter),
            BusyKeyOutcome::Dispatch(BusyAvailability::Staged)
        );
        assert!(app.overlay.is_none());
        assert!(app.deferred_input.is_empty());
        let queued = app
            .busy_jobs
            .queued()
            .front()
            .cloned()
            .ok_or(TestError::Missing("eingereihter /effort-Befehl"))?;
        assert!(queued.raw.starts_with("/effort "), "{queued:?}");

        start_busy_work(&mut app);
        let done = tokio::time::timeout(Duration::from_secs(5), app.busy_jobs.recv())
            .await
            .map_err(ctx("Busy-Ergebnis"))?
            .ok_or(TestError::Missing("Busy-Ergebnis"))?;
        apply_busy_job_done(&mut app, done);

        let history = history_plain(&app);
        assert!(history.contains("gilt ab nächstem Turn"), "{history}");
        assert!(history.contains("Reasoning-Effort gesetzt"), "{history}");
        assert!(app.session_controller.snapshot().reasoning_effort.is_some());
        // Die Session selbst ist unberührt, bis die Turn-Grenze anwendet.
        assert!(app.apply_pending_controller_state(&mut session));
        Ok(())
    }

    /// Testoperation, die vor ihrer Antwort wartet.
    struct SlowOperation {
        meta: harw_operations::OperationMeta,
        delay: Duration,
    }

    impl SlowOperation {
        fn new(delay: Duration) -> Self {
            Self {
                meta: harw_operations::OperationMeta {
                    name: "slow",
                    summary: "Wartet und antwortet dann.",
                    permission: harw_operations::PermissionTier::Observer,
                    surfaces: vec![harw_operations::Surface::Command {
                        path: "/slow",
                        visibility: harw_operations::CommandVisibility::TuiOnly,
                    }],
                    busy: BusyAvailability::Immediate,
                    ..harw_operations::OperationMeta::default()
                },
                delay,
            }
        }
    }

    impl harw_operations::Operation for SlowOperation {
        fn meta(&self) -> &harw_operations::OperationMeta {
            &self.meta
        }

        fn run<'a>(
            &'a self,
            _ctx: &'a harw_operations::OpContext,
            _input: harw_operations::OpInput,
        ) -> harw_operations::OpFuture<'a> {
            Box::pin(async move {
                tokio::time::sleep(self.delay).await;
                Ok(OpOutput::from("langsam fertig".to_owned()))
            })
        }
    }

    fn slow_chat_app(delay: Duration) -> TestResult<ChatApp> {
        let adapters = CommandAdapter::from_operation(Arc::new(SlowOperation::new(delay)));
        let mut app = ChatApp::new(adapters, test_sandbox()?, SessionId::new());
        app.busy_jobs.enable_dispatch_without_runtime();
        Ok(app)
    }

    /// Ein langsamer Sofortbefehl blockiert die Busy-Schleife nicht: das
    /// Starten kehrt sofort zurück, und ein Turn-Ereignis, das während der
    /// Ausführung eintrifft, wird vor dem Befehlsergebnis bedient.
    #[tokio::test]
    async fn slow_immediate_command_does_not_stop_turn_events() -> TestResult {
        let mut app = slow_chat_app(Duration::from_millis(400))?;
        app.input.insert_str("/slow");
        assert_eq!(
            queue_busy_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            BusyKeyOutcome::Dispatch(BusyAvailability::Immediate)
        );
        let started = Instant::now();
        start_busy_work(&mut app);
        assert!(started.elapsed() < Duration::from_millis(200));
        assert_eq!(app.busy_jobs.running(), 1);

        let (turn_tx, mut turn_rx) = tokio::sync::mpsc::unbounded_channel::<u32>();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            let _ = turn_tx.send(7);
        });
        tokio::select! {
            event = turn_rx.recv() => assert_eq!(event, Some(7)),
            _ = app.busy_jobs.recv() => {
                return Err(TestError::Unexpected(
                    "der langsame Befehl darf das Turn-Ereignis nicht überholen".to_owned(),
                ));
            }
        }

        let done = tokio::time::timeout(Duration::from_secs(5), app.busy_jobs.recv())
            .await
            .map_err(ctx("Busy-Ergebnis"))?
            .ok_or(TestError::Missing("Busy-Ergebnis"))?;
        apply_busy_job_done(&mut app, done);
        assert_eq!(app.busy_jobs.running(), 0);
        let history = history_plain(&app);
        assert!(history.contains("/slow (während Turn)"), "{history}");
        assert!(history.contains("langsam fertig"), "{history}");
        Ok(())
    }

    /// Überschreitet ein Befehl das Zeitlimit, meldet der Busy-Pfad das
    /// statt ewig zu warten.
    #[tokio::test]
    async fn busy_command_timeout_is_reported() -> TestResult {
        let mut app = slow_chat_app(Duration::from_secs(5))?;
        app.busy_jobs.set_timeout(Duration::from_millis(50));
        app.busy_jobs.queue(BusyCommand {
            raw: "/slow".to_owned(),
            class: BusyAvailability::Immediate,
        });
        start_busy_work(&mut app);
        let done = tokio::time::timeout(Duration::from_secs(5), app.busy_jobs.recv())
            .await
            .map_err(ctx("Busy-Ergebnis"))?
            .ok_or(TestError::Missing("Busy-Ergebnis"))?;
        apply_busy_job_done(&mut app, done);
        assert!(history_plain(&app).contains("Zeitlimit überschritten"));
        Ok(())
    }

    /// Der Warteschlangen-Block steht über dem Composer, solange Nachrichten
    /// warten, und verschwindet, sobald sie ausgeliefert sind.
    #[test]
    fn queue_block_renders_until_delivery() -> TestResult {
        let mut app = test_chat_app()?;
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 20))
            .map_err(ctx("test terminal"))?;
        let screen = |terminal: &ratatui::Terminal<ratatui::backend::TestBackend>| {
            let buffer = terminal.backend().buffer();
            (0..buffer.area.height)
                .map(|y| {
                    (0..buffer.area.width)
                        .map(|x| buffer[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        app.pending_turns
            .push_back("bitte danach prüfen".to_owned());
        terminal
            .draw(|frame| render_viewport(frame, &app, &Spinner::new(), None))
            .map_err(ctx("draw"))?;
        let with_queue = screen(&terminal);
        assert!(
            with_queue.contains("Wartet auf den nächsten Turn"),
            "{with_queue}"
        );
        assert!(with_queue.contains("bitte danach prüfen"), "{with_queue}");

        // Auslieferung an der Turn-Grenze (`run_loop` nimmt sie aus der FIFO).
        let delivered = app.pending_turns.pop_front();
        assert_eq!(delivered.as_deref(), Some("bitte danach prüfen"));
        terminal
            .draw(|frame| render_viewport(frame, &app, &Spinner::new(), None))
            .map_err(ctx("draw"))?;
        let after = screen(&terminal);
        assert!(!after.contains("Wartet auf den nächsten Turn"), "{after}");
        Ok(())
    }

    #[test]
    fn workbench_command_detection() {
        assert!(is_workbench_command("/workbench"));
        assert!(is_workbench_command("  /workbench note x"));
        assert!(!is_workbench_command("/workbenches"));
        assert!(!is_workbench_command("workbench"));
    }

    /// Der rückgerechnete Cursor-Offset stimmt mit dem echten überein.
    #[test]
    fn editor_cursor_byte_matches_editor_cursor() {
        let mut editor = InputEditor::new();
        editor.insert_str("ab\nc@dé x");
        assert_eq!(editor_cursor_byte(&editor), editor.cursor());
        for _ in 0..4 {
            editor.move_left();
            assert_eq!(editor_cursor_byte(&editor), editor.cursor());
        }
    }

    /// `@`-Popup öffnet mit Rollen; die Übernahme ersetzt das Token unter dem
    /// Cursor und erhält den Rest der Zeile.
    #[test]
    fn mention_popup_opens_and_accept_replaces_token() -> TestResult {
        let mut app = test_chat_app()?;
        let role = KNOWN_ROLES
            .first()
            .ok_or(TestError::Missing("bekannte Rolle"))?;
        app.input.insert_str("@");
        app.sync_popup();
        assert!(
            app.mention_popup
                .as_ref()
                .is_some_and(|popup| !popup.is_empty())
        );

        app.input.clear();
        app.input.insert_str("a @ex b");
        app.input.move_left();
        app.input.move_left();
        app.accept_mention(role);
        assert_eq!(app.input(), format!("a @{role} b"));
        assert_eq!(app.input.cursor(), format!("a @{role}").len());
        assert!(app.mention_popup.is_none());

        app.input.clear();
        app.input.insert_str("schau @src/ma");
        app.accept_mention("src/main.rs");
        assert_eq!(app.input(), "schau @src/main.rs ");
        Ok(())
    }

    #[test]
    fn expand_mentions_without_root_keeps_text() {
        let (text, note) = expand_mentions_for_turn("", "hallo @datei.rs");
        assert_eq!(text, "hallo @datei.rs");
        assert!(note.is_none());
    }

    /// `Cancel` (Esc) auf [`Overlay::ModelSwitch`] schließt das Overlay ohne
    /// eine Befehlszeile zu emittieren.
    #[test]
    fn model_switch_cancel_closes_overlay_without_emitting_a_command() -> TestResult {
        let providers = vec![ProviderEntry {
            id: "anthropic".to_owned(),
            label: "Anthropic".to_owned(),
        }];
        let models = vec![(
            "anthropic".to_owned(),
            vec![ModelEntry {
                id: "claude-sonnet".to_owned(),
                label: "Claude Sonnet".to_owned(),
            }],
        )];
        let picker =
            ModelSwitchPicker::new(PickerTarget::Orchestrator, providers, models, None, None)
                .ok_or(TestError::Missing("providers fixture ist nicht leer"))?;

        let mut app = test_chat_app()?;
        app.overlay = Some(Overlay::ModelSwitch(Box::new(picker)));
        let (bus, mut receiver) = harw_event_channel();

        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &bus,
        );

        assert!(app.overlay.is_none());
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
        Ok(())
    }

    /// Bare `/effort`/`/uia-effort` öffnet [`Overlay::EffortChoice`] mit
    /// allen sechs Stufen plus dem Reset-Eintrag; die Vorauswahl folgt dem
    /// Controller-Snapshot (`Session`) bzw. bleibt ohne Konfiguration auf
    /// Index 0 (`Uia`, kein persistierter Wert im Test-`ChatApp`).
    #[test]
    fn open_effort_choice_lists_all_levels_with_reset_entry() -> TestResult {
        let mut app = test_chat_app()?;
        app.session_controller
            .set_reasoning_effort(Some(ReasoningEffort::High))
            .map_err(ctx("set_reasoning_effort muss gelingen"))?;

        app.open_effort_choice(EffortTarget::Session);
        match &app.overlay {
            Some(Overlay::EffortChoice { target, .. }) => {
                assert_eq!(*target, EffortTarget::Session);
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartete Overlay::EffortChoice, bekam {other:?}"
                )));
            }
        }

        app.open_effort_choice(EffortTarget::Uia);
        assert!(matches!(
            app.overlay,
            Some(Overlay::EffortChoice {
                target: EffortTarget::Uia,
                ..
            })
        ));
        Ok(())
    }

    /// Eine getroffene Effort-Wahl sendet `/effort <level>` bzw.
    /// `/uia-effort <level>`; der letzte Eintrag („Provider-Default
    /// (zurücksetzen)") sendet `clear` statt einer Stufe.
    #[test]
    fn effort_choice_accept_emits_level_or_clear_per_target() -> TestResult {
        // Session: dritte Stufe (Index 2 = "medium") direkt bestätigen.
        let mut app = test_chat_app()?;
        app.open_effort_choice(EffortTarget::Session);
        let (bus, mut receiver) = harw_event_channel();
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &bus,
        );
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
            &bus,
        );
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &bus,
        );
        assert!(app.overlay.is_none());
        match receiver.try_recv() {
            Ok(HarwEvent::Command(command)) => assert_eq!(command, "/effort medium"),
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartete HarwEvent::Command(\"/effort medium\"), bekam {other:?}"
                )));
            }
        }

        // Uia: letzten Eintrag (Reset) wählen → `clear`.
        let mut app = test_chat_app()?;
        app.open_effort_choice(EffortTarget::Uia);
        let (bus, mut receiver) = harw_event_channel();
        for _ in 0..EFFORT_LEVELS.len() {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
                &bus,
            );
        }
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &bus,
        );
        assert!(app.overlay.is_none());
        match receiver.try_recv() {
            Ok(HarwEvent::Command(command)) => assert_eq!(command, "/uia-effort clear"),
            other => {
                return Err(TestError::Unexpected(format!(
                    "erwartete HarwEvent::Command(\"/uia-effort clear\"), bekam {other:?}"
                )));
            }
        }
        Ok(())
    }
}

// ── Tests zur Scharfschaltung der Freigabefrage (W1-08) ──────────────────────

#[cfg(test)]
mod approval_arming_tests {
    use super::tests::test_chat_app;
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use crossterm::event::KeyEventKind;
    use ratatui::buffer::Buffer;

    /// Tastendruck ohne Modifier.
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// Sichtbare Frage, Totzeit abgelaufen.
    const ARMED: Duration = Duration::from_millis(900);

    /// Pflichtfall G-008: ein `y` innerhalb der Totzeit gibt **nichts** frei.
    #[test]
    fn an_early_y_is_ignored_and_a_late_one_approves() -> TestResult {
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
        Ok(())
    }

    /// Auch die Ablehnung per `n` ist der Tipp-Falle entzogen — sonst
    /// beantwortete ein getipptes „nein" die Frage, bevor sie gelesen ist.
    #[test]
    fn an_early_n_is_ignored_and_a_late_one_rejects() -> TestResult {
        assert_eq!(
            classify_armed_approval_key(key(KeyCode::Char('n')), Duration::from_millis(10), true),
            ApprovalKeyAction::NotArmed
        );
        assert_eq!(
            classify_armed_approval_key(key(KeyCode::Char('n')), ARMED, true),
            ApprovalKeyAction::Reject(REASON_OPERATOR_REJECTED)
        );
        Ok(())
    }

    /// Eine nicht sichtbare Frage (hochgescrollt) wird nie beantwortet.
    #[test]
    fn an_invisible_question_accepts_no_answer() -> TestResult {
        assert_eq!(
            classify_armed_approval_key(key(KeyCode::Char('y')), ARMED, false),
            ApprovalKeyAction::NotArmed
        );
        assert_eq!(
            classify_armed_approval_key(key(KeyCode::Char('n')), ARMED, false),
            ApprovalKeyAction::NotArmed
        );
        Ok(())
    }

    /// Eine gedrückt gehaltene Taste ist keine Entscheidung.
    #[test]
    fn a_repeated_key_never_answers() -> TestResult {
        let repeat =
            KeyEvent::new_with_kind(KeyCode::Char('y'), KeyModifiers::NONE, KeyEventKind::Repeat);
        assert_eq!(
            classify_armed_approval_key(repeat, ARMED, true),
            ApprovalKeyAction::NotArmed
        );
        Ok(())
    }

    /// Abbruchtasten bleiben immer wirksam: sie lehnen ab (fail-safe).
    #[test]
    fn cancel_keys_stay_armed_because_they_reject() -> TestResult {
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
        Ok(())
    }

    /// `v` klappt jederzeit auf, andere Tasten bleiben bedeutungslos.
    #[test]
    fn v_toggles_details_and_other_keys_are_ignored() -> TestResult {
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
        Ok(())
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
    fn rearm_restarts_the_arming_delay() -> TestResult {
        let answer_key = key(KeyCode::Char('y'));

        assert!(approval_dialog_key_is_armed(answer_key, ARMED));
        // Rearm: eine neu eingetroffene Frage setzt die seit dem Anzeigen
        // verstrichene Zeit auf null zurück.
        assert!(!approval_dialog_key_is_armed(answer_key, Duration::ZERO));
        Ok(())
    }

    // ── Host-Permit-Dialog (Plan „UIA-Shell-Worker und Shell-Modus", Schritt 2) ──
    //
    // Der Frage-/Antwortvertrag (`HostPermitPrompt`) und die
    // Ausstellungslogik (Ledger/Registry) leben inzwischen in
    // `harw_tool_shell` (Vertrag) bzw. `harw_tool_shell::exec::ShellExecutor`
    // (Ausstellung, eigenständig dort getestet). Die folgenden Tests prüfen
    // deshalb nur noch, was `app.rs` selbst besitzt: welche Vorauswahl der
    // Dialog anzeigt und welche Entscheidung der App-seitige Dispatch über
    // den Antwortkanal der Frage zurücksendet.

    /// Baut eine echte [`HostPermitPrompt`] über ihren öffentlichen
    /// Konstruktor (`harw_tool_shell::host_permit_prompt::HostPermitPrompt::new`)
    /// — denselben Vertrag, den
    /// `harw_tool_shell::exec::ShellExecutor::authorize_host_command`
    /// tatsächlich verwendet — statt die privaten Felder des Typs zu erraten.
    fn build_host_permit_prompt(
        preselected: HostPermitVariant,
    ) -> (
        HostPermitPrompt,
        tokio::sync::oneshot::Receiver<Option<HostPermitVariant>>,
    ) {
        build_host_permit_prompt_for("host-process-worker@1", "echo hi", preselected)
    }

    /// Wie [`build_host_permit_prompt`], aber mit einstellbarer
    /// `worker_definition`/`command` — für B6-Tests, die zwischen einer
    /// Sandbox-Lease-Anfrage
    /// ([`harw_tool_shell::SANDBOX_LEASE_WORKER_DEFINITION`]) und einer
    /// gewöhnlichen Host-Befehlsanfrage unterscheiden müssen.
    fn build_host_permit_prompt_for(
        worker_definition: &str,
        command: &str,
        preselected: HostPermitVariant,
    ) -> (
        HostPermitPrompt,
        tokio::sync::oneshot::Receiver<Option<HostPermitVariant>>,
    ) {
        HostPermitPrompt::new(
            "s1".to_owned(),
            worker_definition.to_owned(),
            command.to_owned(),
            std::path::PathBuf::from("/workspace"),
            preselected,
        )
    }

    /// `build_host_permit_dialog` wählt Option 0 (Einmalig) vor, wenn der
    /// Aufrufer [`HostPermitVariant::SingleExecution`] vorgeschlagen hat
    /// (Arbeitsmodus außerhalb von `shell`).
    #[tokio::test]
    async fn build_host_permit_dialog_preselects_single_execution() -> TestResult {
        let (prompt, answer) = build_host_permit_prompt(HostPermitVariant::SingleExecution);
        let mut dialog = build_host_permit_dialog(&prompt);
        let action = dialog.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, ChoiceAction::Chosen(0));
        assert!(prompt.deny());
        assert_eq!(
            answer
                .await
                .map_err(ctx("responder must deliver an answer"))?,
            None
        );
        Ok(())
    }

    /// `build_host_permit_dialog` wählt Option 1 (Host-Arbeitsphase) vor,
    /// wenn der Aufrufer [`HostPermitVariant::SessionLease`] vorgeschlagen hat
    /// (Shell-Modus).
    #[tokio::test]
    async fn build_host_permit_dialog_preselects_session_lease() -> TestResult {
        let (prompt, answer) = build_host_permit_prompt(HostPermitVariant::SessionLease);
        let mut dialog = build_host_permit_dialog(&prompt);
        let action = dialog.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(action, ChoiceAction::Chosen(1));
        assert!(prompt.deny());
        assert_eq!(
            answer
                .await
                .map_err(ctx("responder must deliver an answer"))?,
            None
        );
        Ok(())
    }

    /// Fix E (Teil 1b): dieselbe Erwartung wie beim Freigabe-Dialog
    /// ([`ctrl_c_with_open_approval_dialog_cancels_turn_and_still_rejects_dialog`]),
    /// hier für die Host-Permit-Frage — Ctrl+C bricht den Turn zusätzlich zum
    /// bestehenden Verweigern der Frage kooperativ ab, statt nur die Frage
    /// abzulehnen.
    #[tokio::test]
    async fn ctrl_c_with_open_host_permit_dialog_cancels_turn_and_still_denies_prompt() -> TestResult
    {
        let (prompt, answer) = build_host_permit_prompt(HostPermitVariant::SingleExecution);
        let dialog = build_host_permit_dialog(&prompt);
        let mut app = test_chat_app()?;
        let cancel = CancelToken::new();
        app.active_cancel = Some(cancel.clone());
        app.deferred_input.push_back(TuiEvent::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        )));
        app.pending_host_permit_dialog = Some(dialog);
        app.pending_host_permit = Some(prompt);
        let mut pending: Option<PendingApprovalPrompt> = None;
        let mut dialog_shown_at: Option<Instant> = None;
        let mut host_permit_shown_at = Some(Instant::now());

        cancel_turn_and_reject_open_dialogs(
            &mut app,
            &mut pending,
            &mut dialog_shown_at,
            &mut host_permit_shown_at,
        )
        .await;

        assert!(
            cancel.is_cancelled(),
            "Ctrl+C muss den Turn auch bei offenem Host-Permit-Dialog kooperativ abbrechen"
        );
        assert!(
            matches!(
                app.pending_quit,
                Some(QuitArm {
                    label: "Ctrl+C",
                    ..
                })
            ),
            "Ctrl+C muss den zweistufigen Beenden-Hinweis scharfstellen"
        );
        assert_eq!(
            app.deferred_input.len(),
            1,
            "eingereihte Eingaben bleiben für die Auslieferung nach dem Abbruch"
        );
        assert!(app.queue_kept_at.is_some());
        assert!(app.pending_host_permit.is_none());
        assert!(app.pending_host_permit_dialog.is_none());
        assert!(host_permit_shown_at.is_none());

        // Bestehendes Verweigern bleibt zusätzlich bestehen, wird nicht ersetzt.
        assert_eq!(
            answer
                .await
                .map_err(ctx("responder must deliver an answer"))?,
            None,
            "Ctrl+C muss die Host-Permit-Frage weiterhin verweigern"
        );
        Ok(())
    }

    /// Rendert einen [`ChoiceDialog`] in einen ausreichend breiten Puffer und
    /// gibt seinen sichtbaren Text als flachen String zurück — dasselbe
    /// Muster wie `choice_dialog.rs`s eigene `test_render_contains_option_labels`.
    fn rendered_choice_dialog(dialog: &ChoiceDialog) -> String {
        let area = Rect::new(0, 0, 90, 10);
        let mut buf = Buffer::empty(area);
        dialog.render(area, &mut buf, style::Theme::Dark);
        buf.content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<Vec<_>>()
            .join("")
    }

    /// B6: eine Sandbox-Lease-Anfrage
    /// ([`harw_tool_shell::SANDBOX_LEASE_WORKER_DEFINITION`]) zeigt einen
    /// eigenen Titel und rahmt `prompt.command()` als „Grund" statt als
    /// auszuführenden Befehl.
    #[test]
    fn build_host_permit_dialog_uses_sandbox_lease_wording_for_the_lease_worker() -> TestResult {
        let (prompt, _answer) = build_host_permit_prompt_for(
            harw_tool_shell::SANDBOX_LEASE_WORKER_DEFINITION,
            "brauche Host-PATH für cargo",
            HostPermitVariant::SessionLease,
        );
        let dialog = build_host_permit_dialog(&prompt);
        let rendered = rendered_choice_dialog(&dialog);

        assert!(
            rendered.contains("Sandbox-Lease angefragt"),
            "rendered: {rendered}"
        );
        assert!(
            rendered.contains("Grund: brauche Host-PATH für cargo"),
            "rendered: {rendered}"
        );
        assert!(
            !rendered.contains("Host-Ausführung erlauben?"),
            "rendered: {rendered}"
        );
        Ok(())
    }

    /// Ein gewöhnlicher Host-Befehl (nicht die Sandbox-Lease-Worker-
    /// Definition) behält die bisherige Formulierung bei.
    #[test]
    fn build_host_permit_dialog_keeps_the_command_wording_for_other_workers() -> TestResult {
        let (prompt, _answer) = build_host_permit_prompt(HostPermitVariant::SingleExecution);
        let dialog = build_host_permit_dialog(&prompt);
        let rendered = rendered_choice_dialog(&dialog);

        assert!(
            rendered.contains("Host-Ausführung erlauben?"),
            "rendered: {rendered}"
        );
        assert!(
            rendered.contains("host-process-worker@1"),
            "rendered: {rendered}"
        );
        assert!(
            !rendered.contains("Sandbox-Lease angefragt"),
            "rendered: {rendered}"
        );
        Ok(())
    }

    /// B6: `open_host_permit_prompt` — die Funktion, die
    /// `drive_turn_animated`s neuer `host_permit_prompts.recv()`-Arm bei
    /// einer während des laufenden Turns eintreffenden Frage aufruft —
    /// öffnet sofort Dialog und Frage. Ein vollständiger Test des
    /// `select!`-Loops selbst existiert nicht (kein Test-Terminal/-Gateway
    /// für `drive_turn_animated` vorhanden); dieser Test deckt die
    /// tatsächliche Zustandsänderung ab, die den Dialog sichtbar macht.
    #[test]
    fn host_permit_prompt_arrival_opens_the_dialog_during_a_running_turn() -> TestResult {
        let mut app = test_chat_app()?;
        assert!(app.pending_host_permit.is_none());
        assert!(app.pending_host_permit_dialog.is_none());

        let (prompt, _answer) = build_host_permit_prompt(HostPermitVariant::SingleExecution);
        let shown_at = open_host_permit_prompt(&mut app, prompt);

        assert!(app.pending_host_permit.is_some());
        assert!(app.pending_host_permit_dialog.is_some());
        assert!(shown_at.elapsed() < Duration::from_secs(1));
        Ok(())
    }

    /// `open_host_permit_prompt` lehnt eine noch offene ältere Frage ab,
    /// statt sie still zu überschreiben (dieselbe K3-Regel wie beim
    /// normalen Freigabe-Panel).
    #[tokio::test]
    async fn open_host_permit_prompt_denies_a_stale_open_prompt() -> TestResult {
        let mut app = test_chat_app()?;
        let (stale_prompt, stale_answer) =
            build_host_permit_prompt(HostPermitVariant::SingleExecution);
        open_host_permit_prompt(&mut app, stale_prompt);
        assert!(app.pending_host_permit.is_some());

        let (fresh_prompt, _fresh_answer) =
            build_host_permit_prompt(HostPermitVariant::SessionLease);
        open_host_permit_prompt(&mut app, fresh_prompt);

        assert_eq!(
            stale_answer
                .await
                .map_err(ctx("responder must deliver an answer"))?,
            None,
            "the stale prompt must be denied, not silently dropped"
        );
        assert!(app.pending_host_permit.is_some());
        Ok(())
    }

    /// `apply_host_permit_decision` mit Options-Index 0 genehmigt genau die
    /// Einzelfreigabe — der eigentliche Ledger-/Registry-Pfad ist bereits in
    /// `harw_tool_shell::exec`'s eigenen Tests abgedeckt; hier wird nur
    /// geprüft, dass der App-seitige Dispatch die richtige Variante über den
    /// Antwortkanal sendet und die erwartete Systemzeile anhängt.
    #[tokio::test]
    async fn apply_host_permit_decision_index_zero_approves_single_execution() -> TestResult {
        let (prompt, answer) = build_host_permit_prompt(HostPermitVariant::SingleExecution);
        let mut app = test_chat_app()?;
        apply_host_permit_decision(&mut app, prompt, 0);
        assert_eq!(
            answer
                .await
                .map_err(ctx("responder must deliver an answer"))?,
            Some(HostPermitVariant::SingleExecution)
        );
        assert!(
            app.cells.iter().any(|cell| cell
                .display_lines(80, app.theme)
                .iter()
                .any(|line| line_contains(line, "einmalig freigegeben"))),
            "a system line must confirm the single-execution approval"
        );
        Ok(())
    }

    /// `apply_host_permit_decision` mit Options-Index 1 genehmigt die
    /// Host-Arbeitsphase.
    #[tokio::test]
    async fn apply_host_permit_decision_index_one_approves_session_lease() -> TestResult {
        let (prompt, answer) = build_host_permit_prompt(HostPermitVariant::SessionLease);
        let mut app = test_chat_app()?;
        apply_host_permit_decision(&mut app, prompt, 1);
        assert_eq!(
            answer
                .await
                .map_err(ctx("responder must deliver an answer"))?,
            Some(HostPermitVariant::SessionLease)
        );
        assert!(
            app.cells.iter().any(|cell| cell
                .display_lines(80, app.theme)
                .iter()
                .any(|line| line_contains(line, "HOST-MODUS AKTIV"))),
            "a system line must confirm the session-lease approval"
        );
        Ok(())
    }

    /// Jeder andere Options-Index (hier: die „Nein"-Option, Index 2) lehnt ab
    /// — fail-closed statt eines Panics bei einem unerwarteten Index.
    #[tokio::test]
    async fn apply_host_permit_decision_any_other_index_denies() -> TestResult {
        let (prompt, answer) = build_host_permit_prompt(HostPermitVariant::SingleExecution);
        let mut app = test_chat_app()?;
        apply_host_permit_decision(&mut app, prompt, 2);
        assert_eq!(
            answer
                .await
                .map_err(ctx("responder must deliver an answer"))?,
            None
        );
        Ok(())
    }

    /// Fällt der Fragekanal weg, ohne dass je geantwortet wurde (z. B. der
    /// Dialog wird verworfen, ohne `approve`/`deny` aufzurufen), schließt
    /// [`HostPermitPrompt`]s `Drop` den Antwortkanal — der Empfänger sieht
    /// `Err`, nie eine stillschweigende Zustimmung. Dieselbe Sicherheitsregel
    /// wie beim normalen Freigabe-Panel: Ablehnung ist der Default.
    #[tokio::test]
    async fn dropped_host_permit_prompt_fails_closed() -> TestResult {
        let (prompt, answer) = build_host_permit_prompt(HostPermitVariant::SingleExecution);
        drop(prompt);
        assert!(
            answer.await.is_err(),
            "a dropped prompt must close the answer channel instead of implicitly approving"
        );
        Ok(())
    }

    /// Kleiner Helfer, der eine gerenderte [`Line`] auf enthaltenen Text prüft
    /// (Spans zu einem String zusammengefügt), ohne von der genauen
    /// Span-Aufteilung abzuhängen.
    fn line_contains(line: &Line<'static>, needle: &str) -> bool {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>()
            .contains(needle)
    }
}
