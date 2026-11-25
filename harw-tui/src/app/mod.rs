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

mod input;
mod terminal;
mod turn_events;
mod key_routing;
mod viewport;
mod error;
mod composition;
mod session_overlays;
mod permission_state;
mod tool_history;
mod monitoring;
mod conversation_state;
mod agent_event_state;
mod state_access;
mod host_mode;
mod history_access;
mod composer;

pub use input::{LineAction, classify_line};
pub(crate) use terminal::TerminalGuard;
pub use error::TuiError;
use input::*;
use key_routing::*;
use turn_events::*;
use viewport::*;

#[cfg(test)]
mod tests;

mod busy_queue;
mod settings_glue;
// Runde 5, Teil F: Plan-Modus (Sperre, Plan-Fenster, `/plan …`, Editor).
pub(crate) mod plan_mode;
// Runde 5, Teil C: Kanban live (mtime-Signatur der Board-Dateien).
mod kanban_live;
// Runde 5, Teil G: gemeinsame Popup-Tasten für Idle- und Busy-Pfad.
mod popup_keys;
// Runde 5, Teil G: Modellwahl der UIA-Worker-Rollen (Worker-Bereich, Live-Wahl).
mod uia_workers;
// Live-Modellwechsel: Kinder folgen `/model switch`, Anzeige `<provider>/<modell>`.
pub(crate) mod live_model;
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
// PL-94 W01 M2b: Anzeigereihenfolge der Kind-Statusmeldungen (`crate::live_ranker`).
mod live_order_glue;
// PL-97 C5 (PL-94 W01 M2a/M2c): virtuelle Live-Zellen der Hintergrund-Läufe im Chat.
mod live_slots;
// Runde 5, Teil O: Doppel-Esc bei laufenden Kindern, ehrliche Abbruch-Beschriftung.
mod turn_safety;
// Runde 5, Teil O: Freigabe-Fragen von Kind-Agenten im Freigabedialog.
pub(crate) mod child_approvals;
// Runde 5, Teil P: Goal-Marke (Statuszeile) und Goal-/Schritt-Verlaufszeilen.
mod goal_marker_glue;
// Runde 6, Teil C: Schlussantwort ohne Historie-Index (Auto-Verdichtung).
mod final_reply;
// Runde 6, Teil C: Export-Einträge für Befehls-/System-/`!`-/Agentenzeilen.
pub(crate) mod export_capture;
// Runde 6, Teil B: `!`-Befehle sofort und asynchron auf dem Host.
mod operator_shell;
// Kleine Fenster: Mausrad nach Position, `scroll_panel_up/down`.
pub(crate) mod scroll_routing;
// PL-97 C2: Aufbau der Fläche zwischen Overlays und Statuszeile
// (Mission-Control-Fenster im Hochformat, sonst Seitenspalte/Zusammenfassung).
mod render;
// Plan R9, Teil F: Hintergrund-Jobs (Jobs-Gruppe, Notizen an die Wurzel,
// Beenden-Dialog).
pub(crate) mod jobs_glue;
// PL-97 M3: Aktivitäts-Reducer (PL-71) hinter den Werkzeugzellen —
// Gruppierung, Job-Lebenszyklus, Fehlschlag-Bursts für Finished / Failed.
pub(crate) mod activity_glue;
// Ansichten zeigen nach einer Änderung sofort den Live-Stand (Tests).
#[cfg(test)]
mod live_views_tests;
// PL-97 C3: Querformat-Fenster (drei Spalten, flach, kompakt).
#[cfg(test)]
mod chrome_localization_tests;
#[cfg(test)]
mod mission_landscape_tests;

use crate::i18n::{tr, tr_args, tr_args_for, tr_for};

use std::cell::{Cell, RefCell};
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
use harw_config::harness_config::ReasoningDisplayMode;
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
#[cfg(test)]
use crate::command_exec::execute_command_as;
use crate::command_exec::{ShellRunOutcome, busy_availability_for};
use crate::command_popup::{CommandPopup, PopupAction, PopupMode};
use crate::events::{HarwEvent, HarwEventSender};
use crate::export::{
    self, ExportAgentEntry, ExportEntry, ExportError, ExportErrorEntry, ExportMeta,
    ExportMetaExtensions, ExportOptions, ExportPlanEntry, ExportStatus,
};
use crate::frame_requester::{FrameRequester, MIN_FRAME_INTERVAL};
use crate::history_cell::{
    AssistantHistoryCell, GoalCell, HistoryCell, PlainHistoryCell, PlanGraphCell,
    ReasoningHistoryCell, SharedPlanGraphCell, SharedReasoningCell, SharedToolCell, SubAgentCell,
    SubAgentStatus, ToolCell, ToolGroupCell, ToolState, ToolVerbosity, UserHistoryCell,
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
                tr(
                    "app_notices.history_unreadable",
                    "⚠ History cell unreadable (poisoned lock)",
                )
                .to_owned(),
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
    /// Runde 6, Teil C: Schlussantwort und Verdichtung des laufenden Turns
    /// (zu Turn-Beginn zurückgesetzt, siehe `app/final_reply.rs`).
    final_reply: final_reply::FinalReplyCapture,
    /// Runde 6, Teil C: die zuletzt enthüllte Schlussantwort; der Rückfall
    /// nach einer Verdichtung zeigt sie nie ein zweites Mal.
    last_revealed_reply: Option<String>,
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
    /// neuesten Eintrag, hochgescrollt bleibt der Ausschnitt bei neuen
    /// Spur-Einträgen stehen (siehe [`ChatScroll::sync_layout`]).
    scroll: ChatScroll,
    /// Reasoning-Einträge vollständig zeigen (`r` schaltet um).
    show_reasoning: bool,
    /// Vollbild-Zustand des Panels vor dem Öffnen (wird beim Schließen
    /// wiederhergestellt).
    prev_maximized: bool,
    /// Plan R9, Teil F: gesetzt, wenn die Ansicht einen Hintergrund-Job statt
    /// eines Agenten zeigt (Kennung).
    job: Option<String>,
    /// Plan R9, Teil F: Kopf und Log-Ende des Jobs, im Takt neu gebaut
    /// (`jobs_glue::refresh`); `None` für einen unbekannten Job.
    job_view: Option<crate::jobs_panel::JobDetail>,
}

/// Seitenweite (Zeilen) für PageUp/PageDown in der Agenten-Detailansicht.
const AGENT_DETAIL_PAGE: usize = 10;

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
    /// Eine Plan-Zelle; ausgeklappt zeigt sie alle Knoten statt nur des
    /// Deltas (R18 F4).
    Plan(SharedPlanGraphCell),
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
            Self::Plan(cell) => cell
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
            Self::Plan(cell) => {
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
                    tr(
                        "app_notices.tool_unreadable",
                        "⚠ Tool cell unreadable (poisoned lock)",
                    )
                    .to_owned(),
                    style::warning_style(theme),
                ))],
            },
            ToolCellHandle::Group(group) => match group.lock() {
                Ok(guard) => guard.display_lines_with(width, theme, self.verbosity),
                Err(_) => vec![Line::from(Span::styled(
                    tr(
                        "app_notices.group_unreadable",
                        "⚠ Tool group unreadable (poisoned lock)",
                    )
                    .to_owned(),
                    style::warning_style(theme),
                ))],
            },
            // Reasoning- und Plan-Zellen rendern sich selbst (Verbosity
            // irrelevant).
            ToolCellHandle::Reasoning(cell) => cell.display_lines(width, theme),
            ToolCellHandle::Plan(cell) => cell.display_lines(width, theme),
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
    /// Plan R9, Teil F: Beenden mit laufenden Hintergrund-Jobs — „Jobs
    /// stoppen“ / „Weiterlaufen lassen“ / „Abbrechen“
    /// ([`jobs_glue::before_quit`]).
    JobsQuitChoice(ChoiceDialog),
    /// PL-97 C4 (W20-E): Bestätigung von `x` im Jobs-Fenster; Enter schickt
    /// `/jobs stop <id>` über den regulären Befehlsweg
    /// ([`crate::mission::stop_job`]).
    StopJob(crate::mission::stop_job::StopJobDialog),
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
    /// Konsolidierte `parent.message {kind: "info"}`-Zwischenstände je
    /// Kind-Agent im 30s-Fenster (gespiegelt aus
    /// `PARENT_INFO_MIN_INTERVAL` in `harw-core::child_comms`):
    /// Kind-Name → (letzter Info-Text, Eingangszeitpunkt).
    child_infos: std::collections::HashMap<String, (String, std::time::Instant)>,
    /// Zwei zuletzt eingetroffene Agentenmeldungen als reine Anzeigekopie.
    recency: crate::recency::RecencySlots,
    /// Zustand des Live-Aktivitäts-Rankers: bestimmt die Reihenfolge der
    /// Kind-Statusmeldungen in der Statuszeile (`app/live_order_glue.rs`).
    /// Hinter einer `RefCell`, weil das Zeichnen nur `&ChatApp` hat.
    live_order: RefCell<live_order_glue::LiveOrder>,
    /// PL-97 C5: Projektion der virtuellen Live-Zellen im Verlauf
    /// (`app/live_slots.rs`); hinter einer `RefCell`, weil das Zeichnen nur
    /// `&ChatApp` hat.
    live_slots: RefCell<live_slots::LiveSlots>,
    /// PL-97 C2: presentation state of the Mission Control windows (Jobs and
    /// Finished/Failed selection and scroll, LIVE pin and ranking).
    mission: crate::mission::MissionUi,
    /// TOML-gesteuertes Verhalten abgelaufener Kind-Statusmeldungen
    /// (nach dem 30s-Fenster): `[tui].status_expiry` — konsolidieren oder
    /// ausblenden. Default: konsolidieren.
    status_expiry: harw_config::StatusExpiryMode,
    /// PL-97 R1: Darstellung der Reasoning-Zusammenfassungen (`[tui]
    /// reasoning`): `message` (ausgeklappter Markdown-Block, Vorgabe),
    /// `collapsed` (eingeklappt) oder `hidden` (keine Zelle, kein Live-Text).
    reasoning_mode: ReasoningDisplayMode,
    /// Read-only Step-/TODO-Anzeige (h22): Bibliothekszustand der
    /// `harw-step-list`; die Explorer-Ansicht rendert daraus nur Zeilen.
    /// `None`, solange keine Liste geladen/gesetzt wurde.
    step_list: Option<harw_step_list::StepList>,
    /// Bereits abgeschickte Benutzertexte. Ein laufender Turn darf nie
    /// abgebrochen oder vermischt werden; diese FIFO wird ausschließlich an
    /// Turn-Grenzen abgearbeitet.
    pending_turns: std::collections::VecDeque<String>,
    /// `/image`: vorgemerkte Bilder für die nächste Nachricht.
    images: crate::image_attach::ImageAttachments,
    /// Befehle und Datenabrufe, die während eines laufenden Turns nebenläufig
    /// laufen (Runde 4, Teil H; siehe [`busy_queue`]).
    busy_jobs: BusyJobs,
    /// Runde 5, Teil L: `/btw`-Nebenfrage (Schnappschuss, laufender Auftrag;
    /// siehe [`btw`]).
    btw: btw::BtwState,
    /// Runde 6, Teil B: laufende `!`-Befehle (Host) und ihr Ergebniskanal
    /// (siehe [`operator_shell`]).
    operator_shell: operator_shell::OperatorShellJobs,
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
    /// R10 Welle 3B: kanonische Namen der Befehle, die diese Sitzung
    /// (kompilierter Agent, [`crate::fixed_agent`]) weder im Popup, in der
    /// Vervollständigung noch beim Dispatch zeigt. Kleingeschrieben; leer für
    /// die normale, uneingeschränkte TUI. Siehe [`Self::with_hidden_commands`].
    hidden_commands: Vec<String>,
    /// R10 Welle 3B: Titel-Überschreibung für die Statuszeile (Name und
    /// Kurz-Digest des kompilierten Agenten). `None` zeigt keinen
    /// zusätzlichen Titel. Siehe [`Self::with_title_override`].
    title_override: Option<String>,
    /// R10 Welle 3B: erlaubte `/model switch`-Ziele (`provider`, `model`),
    /// wenn `Some` — ein Ziel außerhalb der Liste wird abgelehnt, statt
    /// dispatcht zu werden. `None` lässt `/model switch` uneingeschränkt.
    /// Siehe [`Self::with_model_switch_allowlist`].
    model_switch_allowlist: Option<Vec<(String, String)>>,
    /// Explicit locale of this session; never inferred from thread state.
    language: harw_config::UiLanguage,
    /// Configured theme, with terminal detection for `auto`.
    theme: style::Theme,
    /// Über die Session-Laufzeit aufsummierte Token-Nutzung (aus
    /// `SessionEvent::TurnCompleted`). Wird in der Statuszeile angezeigt.
    total_usage: TokenUsage,
    /// Live-Zustand aller Agenten (Wurzel, Kinder, UIA-Worker) aus dem
    /// agenten-übergreifenden Bus; speist Agenten-Panel und Statuszeile.
    agent_monitor: crate::agent_monitor::AgentMonitor,
    /// `child_id` → Verlaufszelle eines Kindes, über den Turn hinaus, der es
    /// gestartet hat. Ein in den Hintergrund gegebener Orchestrator meldet
    /// seinen Fortschritt nur noch über den Agenten-Hub; ohne diese Zuordnung
    /// bliebe seine Zelle auf „läuft · 0 Tools · 0 Tok" stehen
    /// (Befund 2026-09-28, siehe [`ChatApp::sync_child_cell`]).
    child_cells: HashMap<String, Arc<Mutex<SubAgentCell>>>,
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
    /// Flächen des letzten Frames (Agenten-Panel, Dialog, Plan) für das
    /// Maus-Routing ([`scroll_routing::route_scroll_input`]).
    last_regions: Cell<scroll_routing::FrameRegions>,
    /// Fußzeile mit Agentenzahlen unter dem Composer. Standardmäßig aus: die
    /// zusätzliche Zeile verschiebt die zellgenauen Fensteranker (PL-97 C2/C3).
    footer_counts_enabled: bool,
    /// Hochformat-Fenster „Reasoning“: zeitlich geordneter Strom aller
    /// Agenten (Root und Kinder), je Zeile mit der Rolle davor.
    pub(crate) reasoning_feed: crate::mission::reasoning_feed::ReasoningFeed,
    /// Seitenverhältnis einer Terminalzelle (Breite/Höhe ×1000) für die
    /// quadratischen Agents/Jobs-Fenster; `0` = unbekannt (Vorgabe 500).
    /// Gesetzt von [`Self::refresh_cell_aspect`].
    pub(crate) cell_aspect_permille: Cell<u16>,
    /// Feste Vorgabe aus `[harness.tui] cell_aspect` (schlägt die Messung).
    cell_aspect_override: Option<u16>,
    /// Nächstes Bild zeichnet den ganzen Bildschirm neu (`Terminal::clear`
    /// vor `draw`): nach einer Größenänderung und auf `Ctrl+L`. ratatui
    /// schreibt sonst nur geänderte Zellen; zählt der Terminal-Emulator eine
    /// Zeichenbreite anders (tmux, Handy-Terminals), blieben Reste stehen und
    /// Zeilen vermischten sich (Befund 2026-09-28).
    full_redraw: Cell<bool>,
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
    /// PL-97 D5: DoD-Marke der Statuszeile und Titelzusatz des
    /// Finished-/Failed-Fensters (gelesen über `/dod status`, siehe
    /// [`poll_dod_mark`]).
    dod_mark: crate::dod_view::DodMarkFeed,
    /// Weckt [`run_loop`], wenn ein nebenläufiger DoD-Markenabruf fertig ist
    /// (gesetzt beim Start von [`run_loop`]).
    dod_mark_waker: Option<FrameRequester>,
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
    /// PL-97 M3: Aktivitäts-Reducer der Wurzel-Werkzeugzellen (entscheidet
    /// die Gruppierung, siehe [`activity_glue`]).
    activity: activity_glue::ActivityGlue,
    /// Zuletzt als Zelle gezeigter Plan-Stand: der nächste
    /// `TurnEvent::PlanUpdated` desselben Plans rendert nur das Delta
    /// (R18 F4).
    last_shown_plan: Option<harw_plan::Plan>,
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
    /// ob `[session] title_generation` aktiv ist (Export-Datum bei Resume für
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
    /// Plan R9, Teil F: Takt der Jobs-Gruppe und Wahl im Beenden-Dialog.
    jobs_ui: jobs_glue::JobsUi,
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
    /// ein Ereignis eingetreten ist, nach dem Raw-Mode/Alternate-Screen/Bracketed-Paste/
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


// PL-100 Phase 4 (B22, ADR 0002): Aufräumen und Ablaufprüfung einer
// Host-Arbeitsphase, direkt gegen Registry und Ledger (ohne
// Runtime-Montage, die TUI-Tests sonst nicht haben).
#[cfg(test)]
mod host_lease_expiry_tests {
    use super::ChatApp;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_sandbox::{
        HostApprovalScope, HostPermitSessionRegistry, LeaseInstant, ProcessEnvironment,
        ProcessPermitError, ProcessPermitLedger, request_for_workspace,
    };
    use std::path::Path;
    use std::time::Duration;

    // Registry mit 1 s Phasendauer und ein Zeitpunkt 5 s zuvor: eine dort
    // erteilte Phase ist bereits abgelaufen (ohne `sleep`).
    fn short_registry_and_past() -> TestResult<(HostPermitSessionRegistry, LeaseInstant)> {
        let registry = HostPermitSessionRegistry::with_lease_ttl(Duration::from_secs(1))
            .map_err(ctx("with_lease_ttl"))?;
        let past = LeaseInstant::now()
            .checked_sub(Duration::from_secs(5))
            .ok_or(TestError::Missing("instant five seconds ago"))?;
        Ok((registry, past))
    }

    #[test]
    fn clear_without_stored_window_changes_nothing() {
        let registry = HostPermitSessionRegistry::default();
        let ledger = ProcessPermitLedger::default();
        registry.mark_global_single_use();
        assert_eq!(
            ChatApp::clear_host_lease_in(&registry, &ledger, "root", false),
            None
        );
        assert!(
            registry.has_global_single_use(),
            "nothing stored: nothing may change"
        );
    }

    #[test]
    fn clear_ends_an_active_lease_and_revokes_the_session_permits() -> TestResult {
        let registry = HostPermitSessionRegistry::default();
        let ledger = ProcessPermitLedger::default();
        registry.mark_session_approved("root");
        registry.mark_global_approval();
        let request = request_for_workspace(
            "root",
            "host-process-worker@1",
            "echo hi",
            Path::new("/workspace"),
            ProcessEnvironment::LocalHost,
        );
        let id = ledger
            .issue_after_local_approval(
                request.clone(),
                HostApprovalScope::SingleExecution,
                Some(Duration::from_secs(300)),
            )
            .map_err(ctx("issue_after_local_approval"))?;

        assert_eq!(
            ChatApp::clear_host_lease_in(&registry, &ledger, "root", false),
            Some(true),
            "an active lease is reported as ended"
        );
        assert!(!registry.is_session_approved("root"));
        assert_eq!(registry.session_lease("root"), None);
        assert_eq!(registry.global_lease(), None);
        assert_eq!(
            ledger.authorize(id, &request),
            Err(ProcessPermitError::UnknownPermit),
            "the session's ledger permits must be revoked"
        );
        Ok(())
    }

    #[test]
    fn clear_removes_an_expired_lease_once_and_reports_it_as_expired() -> TestResult {
        // Vorher kehrte `end_host_mode` nach dem Ablauf früh zurück und ließ
        // die abgelaufenen Fenster liegen.
        let (registry, past) = short_registry_and_past()?;
        let ledger = ProcessPermitLedger::default();
        registry.mark_session_approved_at("root", past);
        registry.mark_global_approval_at(past);
        assert!(
            !registry.is_session_approved("root"),
            "the lease must have expired"
        );

        assert_eq!(
            ChatApp::clear_host_lease_in(&registry, &ledger, "root", false),
            Some(false),
            "an expired lease is reported as expired"
        );
        assert_eq!(registry.session_lease("root"), None);
        assert_eq!(registry.global_lease(), None);
        assert_eq!(
            ChatApp::clear_host_lease_in(&registry, &ledger, "root", false),
            None,
            "nothing is stored any more, so no second notice"
        );
        Ok(())
    }

    #[test]
    fn expiry_cleanup_keeps_a_pending_global_single_use_but_ctrl_h_ends_it() -> TestResult {
        // Szenario: die globale Phase ist abgelaufen, danach erteilt der
        // Nutzer auf `/sandbox-lease request` eine Einmalfreigabe. Das
        // automatische Aufräumen (`keep = true`) darf sie nicht still
        // löschen; Strg+H (`keep = false`) beendet sie mit.
        for (keep, expected) in [(true, true), (false, false)] {
            let (registry, past) = short_registry_and_past()?;
            let ledger = ProcessPermitLedger::default();
            registry.mark_global_approval_at(past);
            registry.mark_global_single_use();

            assert_eq!(
                ChatApp::clear_host_lease_in(&registry, &ledger, "root", keep),
                Some(false),
                "keep={keep}"
            );
            assert_eq!(registry.global_lease(), None, "keep={keep}");
            assert_eq!(registry.has_global_single_use(), expected, "keep={keep}");
        }
        Ok(())
    }

    #[test]
    fn expired_detection_needs_a_stored_window_without_cover() -> TestResult {
        let registry = HostPermitSessionRegistry::default();
        assert!(
            !ChatApp::host_lease_expired_in(&registry, "root"),
            "nothing stored"
        );
        registry.mark_global_approval();
        assert!(
            !ChatApp::host_lease_expired_in(&registry, "root"),
            "an active lease is not expired"
        );

        let (expired, past) = short_registry_and_past()?;
        expired.mark_session_approved_at("root", past);
        assert!(ChatApp::host_lease_expired_in(&expired, "root"));
        let (expired_global, past) = short_registry_and_past()?;
        expired_global.mark_global_approval_at(past);
        assert!(
            ChatApp::host_lease_expired_in(&expired_global, "root"),
            "an expired process-wide lease counts for the root session"
        );
        Ok(())
    }

    #[test]
    fn poll_delay_is_capped_zero_when_expired_and_none_without_window() -> TestResult {
        let registry = HostPermitSessionRegistry::default();
        assert_eq!(ChatApp::host_lease_poll_delay_in(&registry, "root"), None);
        // 8 h Restlaufzeit: gekappt auf den Prüfabstand.
        registry.mark_global_approval();
        assert_eq!(
            ChatApp::host_lease_poll_delay_in(&registry, "root"),
            Some(ChatApp::HOST_LEASE_POLL_INTERVAL)
        );

        // Kürzere Restlaufzeit als der Prüfabstand: höchstens die Restlaufzeit.
        let short = HostPermitSessionRegistry::with_lease_ttl(Duration::from_secs(20))
            .map_err(ctx("with_lease_ttl"))?;
        short.mark_session_approved("root");
        let delay = ChatApp::host_lease_poll_delay_in(&short, "root")
            .ok_or(TestError::Missing("delay under a short lease"))?;
        assert!(delay <= Duration::from_secs(20), "{delay:?}");

        // Abgelaufen, aber noch gespeichert: sofort prüfen.
        let (expired, past) = short_registry_and_past()?;
        expired.mark_session_approved_at("root", past);
        assert_eq!(
            ChatApp::host_lease_poll_delay_in(&expired, "root"),
            Some(Duration::ZERO)
        );
        Ok(())
    }
}

/// Byte-Offset des Composer-Cursors.
///
/// # Beschreibung
/// `InputEditor::cursor` ist nur in Tests sichtbar; der Offset wird deshalb
/// aus [`InputEditor::cursor_position_raw`] mit Breite `0` (keine weichen
/// Umbrüche: Zeile = harte Zeile, Spalte = Anzeigebreite davor)
/// zurückgerechnet. Ergebnis liegt immer auf einer Zeichengrenze.
fn editor_cursor_byte(editor: &InputEditor) -> usize {
    let (row, col) = editor.cursor_position_raw(0);
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
    crate::i18n::with_language(app.language, || local_intercept_for_scoped(app, raw))
}

fn local_intercept_for_scoped(app: &ChatApp, raw: &str) -> Option<LocalIntercept> {
    // PL-97 D5: `/dod view` ist TUI-lokal (Harw-DoD-Ansicht); alle übrigen
    // `/dod`-Formen gehören der `dod`-Operation. Ein ausgeblendetes `/dod`
    // (fixed agent) meldet `local_commands::intercept` wie gewohnt.
    if crate::dod_view::is_view_command(raw)
        && !app
            .hidden_commands
            .iter()
            .any(|hidden| hidden.eq_ignore_ascii_case("dod"))
    {
        return Some(LocalIntercept::OpenOverlay(Box::new(
            crate::dod_view::DodView::new(),
        )));
    }
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
        live_model_command: app.live_model_command(),
        project_root: &project_root,
        session_id: app.session_id(),
        tier: app.caller_tier(),
        known_roles: KNOWN_ROLES,
        // R10 Welle 3B: fixed-agent-Beschränkungen, leer/`None` für die
        // normale, uneingeschränkte TUI.
        hidden_commands: &app.hidden_commands,
        model_switch_allowlist: app.model_switch_allowlist.as_deref(),
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
    crate::i18n::with_language(app.language, || {
        apply_local_intercept_scoped(app, intercepted, bus)
    })
}

fn apply_local_intercept_scoped(
    app: &mut ChatApp,
    intercepted: LocalIntercept,
    bus: &HarwEventSender,
) -> Option<TuiRunOutcome> {
    match intercepted {
        // `/models` baut dieselbe Übersicht wie F8 aus dem Live-Stand.
        LocalIntercept::OpenOverlay(view)
            if view.refresh_command().as_deref()
                == Some(crate::model_roles_view::REFRESH_COMMAND) =>
        {
            let view = app.model_roles_view();
            app.open_overlay_view(Box::new(view));
        }
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
            if !app.panels.agents_visible && app.panels.focus.is_mission() {
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
            let text = match app.toggle_verbose() {
                ToolVerbosity::Verbose => {
                    tr("app_notices.verbosity_verbose", "Detailed tool display: on")
                }
                ToolVerbosity::Activity => {
                    tr("app_notices.verbosity_activity", "Tool display: one line")
                }
                ToolVerbosity::Compact => tr(
                    "app_notices.verbosity_compact",
                    "Detailed tool display: off",
                ),
            };
            app.push_lines(vec![Line::from(text)]);
        }
        LocalIntercept::ClearTranscript => {
            app.clear_transcript();
            app.push_lines(vec![Line::from(tr(
                "app_notices.clear",
                "Display cleared — session and model context are preserved.",
            ))]);
        }
        LocalIntercept::RenameSession(title) => {
            let title = title.trim().to_owned();
            app.set_session_title(title.clone());
            app.push_lines(vec![Line::from(tr_args(
                "app_notices.renamed",
                "Session title (display) set: “{title}”",
                &[("title", &title)],
            ))]);
        }
        LocalIntercept::AttachImage(args) => {
            let home = harw_home::paths::home_dir().ok();
            let base = std::path::PathBuf::from(&app.project_root);
            let note = app.images.command(&args, home.as_deref(), &base);
            app.push_system_text_exported(&note);
        }
        LocalIntercept::Rewrite(line) => bus.send(HarwEvent::Command(line)),
        LocalIntercept::Chat(text) => {
            app.scroll.force_follow();
            app.pending_turns.push_back(text);
        }
        // Runde 6, Teil C: lokale Befehlsausgaben auch in den Export.
        LocalIntercept::System(text) => app.push_system_text_exported(&text),
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
    language: harw_config::UiLanguage,
    runtime: Option<&Arc<harw_runtime::RuntimeAssembly>>,
    adapters: &[CommandAdapter],
    sandbox: &SandboxSpec,
    session_id: &SessionId,
    raw: &str,
) -> Result<OpOutput, String> {
    let Some(rt) = runtime else {
        return Err(crate::i18n::tr_for(
            language,
            "dispatch.no_runtime",
            "Error: no runtime assembly",
        )
        .to_owned());
    };
    command_data::execute_command_with_data_for(
        language,
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
    crate::i18n::with_language(app.language, || {
        apply_fetch_result_scoped(app, target, result)
    })
}

fn apply_fetch_result_scoped(
    app: &mut ChatApp,
    target: FetchTarget,
    result: Result<OpOutput, String>,
) {
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
            app.language,
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

/// Zeitlimit eines nebenläufigen DoD-Markenabrufs (wie ein Busy-Abruf):
/// danach gilt der Abruf als gescheitert und die Marke verschwindet.
const DOD_MARK_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// PL-97 D5: führt die DoD-Marke der Statuszeile über `/dod status` nach
/// (regulärer Datenpfad [`fetch_command_data`], nie in den Chat).
///
/// # Beschreibung
/// Läuft überall dort, wo [`jobs_glue::poll`] läuft (Leerlauf in
/// [`run_loop`], Busy-Pfad in [`ChatApp::drain_agent_events`]), kein eigener
/// Timer. Wartet nie: ein fertiger Abruf wird übernommen, ein fälliger
/// (höchstens alle [`crate::dod_view::MARK_REFRESH_INTERVAL`] — so lange
/// cacht der `DodService` ohnehin) startet als eigener Task, wie die
/// Busy-Abrufe ([`spawn_busy_fetches`]); sein Ende weckt die Schleife über
/// [`ChatApp::dod_mark_waker`]. Ohne Runtime-Montage geschieht nichts. Ist
/// die DoD-Ansicht offen, wird sie mit jedem fertigen Abruf neu geladen.
///
/// # Nebenläufigkeit
/// Startet einen Abruf nur innerhalb einer Tokio-Runtime; ohne Runtime
/// (synchrone Tests) bleibt es beim Übernehmen.
///
/// # Rückgabe
/// `true`, wenn sich Sichtbares änderte (Marke oder eingereihtes Nachladen).
fn poll_dod_mark(app: &mut ChatApp) -> bool {
    let mut changed = false;
    if let Some(mark_changed) = app.dod_mark.poll() {
        changed = mark_changed;
        let dod_view_open = matches!(
            &app.overlay,
            Some(Overlay::View(view))
                if view.refresh_command().as_deref() == Some(crate::dod_view::REFRESH_COMMAND)
        );
        if dod_view_open {
            app.queue_overlay_refresh();
            changed = true;
        }
    }
    let now = Instant::now();
    if !app.dod_mark.due(now) || tokio::runtime::Handle::try_current().is_err() {
        return changed;
    }
    let Some(runtime) = app.runtime.clone() else {
        return changed;
    };
    let adapters = Arc::clone(&app.adapters);
    let sandbox = app.sandbox.clone();
    let session_id = app.session_id.clone();
    let waker = app.dod_mark_waker.clone();
    let (tx, read) = tokio::sync::oneshot::channel();
    let language = app.language;
    tokio::spawn(async move {
        let fetch = fetch_command_data(
            language,
            Some(&runtime),
            &adapters,
            &sandbox,
            &session_id,
            crate::dod_view::REFRESH_COMMAND,
        );
        let result = match tokio::time::timeout(DOD_MARK_READ_TIMEOUT, fetch).await {
            Ok(result) => result,
            Err(_) => Err(crate::i18n::tr_args_for(
                language,
                "dispatch.timeout",
                "Time limit exceeded ({seconds} s).",
                &[("seconds", &DOD_MARK_READ_TIMEOUT.as_secs().to_string())],
            )),
        };
        let _ = tx.send(result);
        if let Some(waker) = waker {
            waker.schedule_frame();
        }
    });
    app.dod_mark.begin(now, read);
    changed
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
            (tokens, tools, wall) => crate::i18n::tr_args(
                "agents.budget_caps",
                "Tokens {tokens} · Tools {tools} · Duration {wall} ms",
                &[
                    (
                        "tokens",
                        &tokens.map_or_else(|| "—".to_owned(), |value| value.to_string()),
                    ),
                    (
                        "tools",
                        &tools.map_or_else(|| "—".to_owned(), |value| value.to_string()),
                    ),
                    (
                        "wall",
                        &wall.map_or_else(|| "—".to_owned(), |value| value.to_string()),
                    ),
                ],
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
            ContentPart::ImageUrl { .. } | ContentPart::Media { .. } => {
                visible.push_str(NON_TEXT_CONTENT_PLACEHOLDER);
            }
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

/// Hochformat-„Reasoning“: Ausgang und einzeilige Kurzfassung eines
/// abgeschlossenen Aufrufs (erste Zeile der Fehlermeldung, sonst leer).
fn feed_tool_outcome(result: &ToolCallResult) -> (bool, String) {
    match result {
        ToolCallResult::Success { .. } => (true, String::new()),
        ToolCallResult::Error { message } => (
            false,
            message.lines().next().unwrap_or_default().trim().to_owned(),
        ),
    }
}

/// Hochformat-„Reasoning“: überträgt ein Bus-Ereignis eines Kind-Agenten in
/// den gemeinsamen Strom. Root-Ereignisse kommen über `handle_turn_event`
/// (und den Turn-Lebenszyklus) und werden hier übersprungen, sonst stünden
/// sie doppelt da. Muss **vor** [`crate::agent_monitor::AgentMonitor::apply`]
/// laufen: der Werkzeugname eines Abschlusses stammt aus `current_tool`.
fn feed_agent_event(
    feed: &mut crate::mission::reasoning_feed::ReasoningFeed,
    monitor: &crate::agent_monitor::AgentMonitor,
    root: &SessionId,
    event: &harw_core::AgentEvent,
) {
    if event.agent == *root {
        return;
    }
    let agent = event.agent.as_str();
    let role = event.role.as_str();
    match &event.kind {
        harw_core::AgentEventKind::Turn(TurnEvent::ReasoningDelta { text, .. }) => {
            feed.push_reasoning(agent, role, text);
        }
        harw_core::AgentEventKind::Turn(TurnEvent::ToolCallRequested { tool_name, .. }) => {
            feed.push_tool_start(agent, role, tool_name);
        }
        harw_core::AgentEventKind::Turn(TurnEvent::ToolCallCompleted { result, .. }) => {
            let tool = monitor
                .agent(agent)
                .and_then(|live| live.current_tool.clone())
                .unwrap_or_else(|| "tool".to_owned());
            let (ok, summary) = feed_tool_outcome(result);
            feed.push_tool_end(agent, role, &tool, ok, &summary);
        }
        harw_core::AgentEventKind::Orchestration(orch) => {
            let ok = match orch.status {
                AgentOrchestrationStatus::Completed => true,
                AgentOrchestrationStatus::Failed | AgentOrchestrationStatus::Cancelled => false,
                _ => return,
            };
            // Nur der Übergang zählt: wiederholte Endmeldungen desselben
            // Laufs erzeugen keine zweite Zeile.
            if monitor
                .agent(agent)
                .is_some_and(|live| live.phase.is_active())
            {
                feed.push_finished(agent, role, ok);
            }
        }
        _ => {}
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
    crate::i18n::with_language(app.language, || {
        hydrate_visible_history_scoped(app, history)
    });
}

fn hydrate_visible_history_scoped(app: &mut ChatApp, history: &ConversationHistory) {
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
                        app.append_tool_cell(&call, None, Arc::clone(&cell));
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
                        app.append_tool_cell(&call, None, Arc::clone(&cell));
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
                // PL-97 M3: Ende des Aufrufs in den Aktivitäts-Reducer.
                activity_glue::tool_completed(
                    app,
                    result_item.call_id.as_str(),
                    &cell,
                    Some(&result_item.result),
                );
            }
            TurnItem::Reasoning(reasoning) => {
                // PL-97 R1: Leerzeilen zwischen den Einträgen erhalten die Absätze.
                let summary = reasoning.summary_text.join("\n\n");
                if !summary.trim().is_empty() {
                    app.export_entries
                        .push(ExportEntry::Reasoning(summary.clone()));
                    app.push_reasoning_cell(summary, None);
                    // Symmetrisch zum Live-Pfad: die fertige Zelle ersetzt
                    // eine etwaige Live-Vorschau.
                    app.live_reasoning.clear();
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
        let mut marked = false;
        if let Ok(mut guard) = cell.lock()
            && guard.state == ToolState::Running
        {
            guard.mark_incomplete();
            marked = true;
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
        if marked {
            // PL-97 M3: der offene Aufruf endet auch im Reducer (fehlgeschlagen).
            activity_glue::tool_completed(app, call_id.as_str(), cell, None);
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
///
/// Dasselbe Fenster nutzt der Not-Aus im Eingabe-Thread
/// ([`crate::hard_kill::DOUBLE_PRESS_WINDOW`]); beide teilen sich die
/// Konstante, damit Hinweis und Not-Aus nie auseinanderlaufen.
const QUIT_HINT_WINDOW: Duration = crate::hard_kill::DOUBLE_PRESS_WINDOW;

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

    // Bilder früherer Nachrichten (fortgesetzte Sitzung) wieder auffindbar machen.
    app.images
        .attach_existing(harw_home::paths::home_dir().ok().as_deref());

    let mut spinner = Spinner::new();
    let mut provider_error_streak = 0_u32;
    // Ein fertiger Busy-Auftrag weckt die Schleife über einen Frame, auch
    // wenn sein Turn schon vorbei ist (Runde 4, Teil H).
    app.busy_jobs.set_waker(frame_req.clone());
    // Runde 5, Teil L: eine fertige `/btw`-Nebenfrage weckt die Schleife.
    app.btw.set_waker(frame_req.clone());
    // Runde 6, Teil B: ein fertiger `!`-Befehl weckt die Schleife.
    app.operator_shell.set_waker(frame_req.clone());
    // PL-97 D5: ein fertiger DoD-Markenabruf weckt die Schleife.
    app.dod_mark_waker = Some(frame_req.clone());
    // Hochformat: Zellproportion für die quadratischen Mission-Fenster.
    app.refresh_cell_aspect();

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
        // Plan R9, Teil F: Job-Ereignisse (Systemzeilen, Notizen an die UIA
        // mit Auto-Turn beim Ende eines Jobs) und die Jobs-Gruppe.
        if jobs_glue::poll(app) {
            frame_req.schedule_frame();
        }
        // PL-97 D5: DoD-Marke im selben Takt (gedrosselt auf den DoD-Cache,
        // nebenläufig — wartet nie auf den Abruf).
        if poll_dod_mark(app) {
            frame_req.schedule_frame();
        }
        // PL-100 Phase 4 (B22, ADR 0002): eine abgelaufene Host-Arbeitsphase
        // genau einmal melden und aufräumen (geweckt vom Timer-Zweig unten).
        if app.poll_host_lease_expiry() {
            frame_req.schedule_frame();
            if app.take_needs_terminal_reassert() {
                guard.reassert_terminal_modes();
            }
        }

        // Eine während des vorherigen Turns abgeschickte Eingabe hat Vorrang.
        // Sie passiert denselben Pfad wie eine frische `HarwEvent::Submit`.
        let mut submitted: Option<String> = app.pending_turns.pop_front();
        // Runde 6, Teil B: fertige `!`-Befehle (Host) anzeigen und erst
        // jetzt, nach ihrem Ende, den Folge-Turn einreihen.
        if operator_shell::poll_idle(app, &mut submitted) {
            frame_req.schedule_frame();
        }
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
            // Plan R9, Teil F: Wecker für Job-Ereignisse und ein Takt für
            // Laufzeit/Fortschritt, solange Jobs laufen.
            let jobs_wake = jobs_glue::waker(app);
            let jobs_tick = jobs_glue::has_running(app);
            // PL-100 Phase 4 (B22): Wecker zum Ablauf einer gespeicherten
            // Host-Arbeitsphase (Restlaufzeit, höchstens alle 30 s).
            let host_lease_wake = app.host_lease_poll_delay();
            tokio::select! {
                maybe_tev = async { match app.deferred_input.pop_front() { Some(event) => Some(event), None => tui_rx.recv().await } } => {
                    let Some(tev) = maybe_tev else { return Ok(TuiRunOutcome::Quit) };

                    // Abgelaufenen Beenden-Hinweis verwerfen.
                    if let Some(arm) = app.pending_quit
                        && arm.at.elapsed() > QUIT_HINT_WINDOW {
                            app.pending_quit = None;
                        }
                    // Runde 5, Teil K: ein im Leerlauf offenes sudo- oder
                    // Host-Permit-Fenster (Hintergrund-Agent) bekommt die Eingabe.
                    // Mausrad nach Position und `scroll_panel_up/down` —
                    // vor allen Fenstern, weil Scrollen nichts entscheidet.
                    if let Some(redraw) = scroll_routing::route_scroll_input(app, &tev) {
                        if redraw {
                            frame_req.schedule_frame();
                        }
                        continue;
                    }
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
                            app.full_redraw.set(true);
                            app.refresh_cell_aspect();
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
                            // Plan R9, Teil F: laufende Jobs stoppen oder
                            // ablösen (Dialog beim ersten Beenden).
                            if background_agents::confirm_quit(app)
                                && jobs_glue::before_quit(app).await
                            {
                                return Ok(TuiRunOutcome::Quit);
                            }
                            frame_req.schedule_frame();
                        }
                        HarwEvent::SystemMessage(message) => {
                            // Mehrzeilige Ausgaben (z. B. `/help`) an `\n` aufteilen.
                            // Runde 6, Teil C: auch in den Export.
                            app.push_system_text_exported(&message);
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
                            // Runde 6, Teil B: `!`/`!!` läuft asynchron auf
                            // dem Host; Ausgabe und Folge-Turn kommen erst
                            // nach dem Ende (oben, `operator_shell::poll_idle`).
                            if operator_shell::intercept(app, &raw) {
                                frame_req.schedule_frame();
                                continue;
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
                                Some(crate::i18n::with_language(app.language, || {
                                    crate::tools_command::dispatch_tools_command_bounded(
                                        &args,
                                        &tool_names,
                                        gateway.session_mut().activation_mut(),
                                        &ceiling,
                                    )
                                }))
                            } else {
                                None
                            };

                            if let Some(outcome) = tools_outcome {
                                let lines: Vec<Line<'static>> = crate::i18n::with_language(app.language, || outcome.into_lines())
                                    .into_iter()
                                    .map(Line::from)
                                    .collect();
                                // Runde 6, Teil C: `/tools`-Ausgabe auch in den Export.
                                let text = export_capture::command_export_text(
                                    &raw,
                                    &export_capture::lines_plain_text(&lines),
                                );
                                app.push_system_lines_exported(lines, text);
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
                                                    command_data::execute_command_with_data_for(
                                                        app.language,
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
                                                    let language = app.language;
                                                    let (outcome, slash_result) = crate::command_exec::dispatch_command_with_shell_result_for(
                                                        language,
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
                                                    if let Some(result) = slash_result.as_ref() {
                                                        app.apply_settings_completion(&raw, language, result.as_ref().map_err(String::as_str));
                                                    }
                                                    let data = slash_result.and_then(Result::ok).and_then(|output| output.data);
                                                    shell_result = outcome.shell;
                                                    (outcome.text, data)
                                                }
                                            }
                                        }
                                        None => {
                                            tracing::error!("tui.command.no_runtime_assembly");
                                            (crate::i18n::tr_for(app.language, "dispatch.no_runtime", "Error: no runtime assembly").to_owned(), None)
                                        }
                                    }
                                };
                                let lines: Vec<Line<'static>> = output
                                    .split('\n')
                                    .map(|line| Line::from(line.to_owned()))
                                    .collect();
                                // Runde 6, Teil C: Befehlsausgabe (auch „Export
                                // angefordert") in den Export; ein gelaufener
                                // `!`-Befehl bekommt seinen eigenen Eintrag.
                                match shell_result.as_ref() {
                                    Some(shell) => {
                                        app.push_lines(lines);
                                        export_capture::export_shell_result(
                                            app,
                                            &shell.command,
                                            Some(shell.exit_code),
                                            &shell.combined_output,
                                        );
                                    }
                                    None => app.push_system_lines_exported(
                                        lines,
                                        export_capture::command_export_text(&raw, &output),
                                    ),
                                }
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
                // Plan R9, Teil F: ein Job-Ereignis — der Schleifenanfang
                // holt es ab (und startet beim Ende ggf. den Auto-Turn).
                _ = jobs_glue::wait_for_event(jobs_wake) => {
                    frame_req.schedule_frame();
                }
                _ = tokio::time::sleep(jobs_glue::JOBS_REFRESH_INTERVAL), if jobs_tick => {
                    if jobs_glue::refresh(app, true) {
                        frame_req.schedule_frame();
                    }
                }
                // PL-100 Phase 4 (B22, ADR 0002): weckt die Schleife zum
                // Ablauf der Host-Arbeitsphase; der Schleifenanfang
                // (`poll_host_lease_expiry`) meldet ihn und zeichnet neu.
                _ = tokio::time::sleep(host_lease_wake.unwrap_or(ChatApp::HOST_LEASE_POLL_INTERVAL)), if host_lease_wake.is_some() => {}
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
        let attached_images = app.images.len();
        if attached_images > 0 {
            app.push_system_text_exported(&tr_args_for(
                app.language,
                "app_notices.images_attached",
                "(Images attached: {count})",
                &[("count", &attached_images.to_string())],
            ));
        }
        if let Some(note) = attachment_note {
            // Runde 6, Teil C: Anhang-Hinweis auch in den Export.
            app.push_system_text_exported(&note);
        }
        // Auto-Correction-Detection (harw-memory M3): reine Textregel,
        // kein LLM-Call. Bei Match: Signal explizit an das Backend geben.
        if let Some(mem) = app.memory()
            && let Some(sig) =
                harw_memory::detect_correction(&text, Some(format!("session:{}", app.session_id())))
        {
            // Fehler bewusst still verschlucken — Memory-Record ist
            // best-effort und darf den User-Turn niemals unterbrechen.
            let _ = mem.record(sig);
        }
        // Turn treiben (Spinner animiert), Antwort simuliert streamen.
        //
        // `TuiError::Core` (Provider-/Model-Fehler, z. B. abgelaufene
        // Credentials) darf die Session NICHT beenden — analog zur
        // Rate-Limit-Behandlung in `drive_turn_animated` bleibt der
        // Chat offen und der Fehler wird als System-Zeile angezeigt.
        // `TuiError::Io` bleibt fatal und propagiert weiterhin nach
        // oben, da er einen nicht behebbaren Terminalfehler anzeigt.
        let turn_images = app.images.take();
        let turn_result = run_turn_streaming(
            guard,
            app,
            &mut spinner,
            gateway,
            &turn_text,
            turn_images,
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
            // Plan R9, Teil F: harw endet sofort — laufende Jobs ablösen.
            jobs_glue::detach_for_exit(app);
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
        language: app.language,
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
    // Aufgelöstes Modell der UIA statt `anthropic/default` (siehe
    // `export_capture::export_model_label`).
    let model = export_capture::export_model_label(
        &controller,
        config.as_deref(),
        app.export_session_model.as_deref(),
    );
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
    crate::i18n::with_language(app.language, || resolve_export_request_scoped(app, request));
}

fn resolve_export_request_scoped(app: &mut ChatApp, request: &ExportRequest) {
    let opts = ExportOptions {
        include_tool_calls: request.include_tool_calls,
        include_reasoning: request.include_reasoning_summary,
        max_chars: request.max_chars,
        ..ExportOptions::default()
    };
    if let Some(path) = request.path.as_deref() {
        app.pending_export_options = None;
        let content = build_export(app, &opts, request.format);
        // Runde 6, Teil C: `~`/`$HOME` expandieren, Verzeichnis → Standardname
        // mit Format-Endung; die Traversal-Prüfung bleibt in `write_export_path`.
        let target = export_capture::export_target_path(path, request.format);
        match export::write_export_path(&target, &content) {
            Ok(written_path) => app.push_line(
                Role::System,
                tr_args(
                    "app_notices.export_saved",
                    "Export saved: {path}",
                    &[("path", &written_path.display().to_string())],
                ),
            ),
            Err(error) => app.push_line(
                Role::System,
                tr_args(
                    "app_notices.export_failed",
                    "Export failed: {error}",
                    &[("error", &error.to_string())],
                ),
            ),
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
    crate::i18n::with_language(app.language, || {
        resolve_export_choice_scoped(app, _bus, index)
    })
}

fn resolve_export_choice_scoped(app: &mut ChatApp, _bus: &HarwEventSender, index: usize) {
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
                            tr(
                                "app_notices.export_osc_success",
                                "Export copied to clipboard (OSC-52).",
                            ),
                        );
                    } else {
                        app.push_line(
                            Role::System,
                            tr(
                                "app_notices.export_osc_failed",
                                "Export: could not write OSC-52 sequence.",
                            ),
                        );
                    }
                }
                Ok((target, _)) => {
                    app.push_line(
                        Role::System,
                        tr_args(
                            "app_notices.export_copied",
                            "Export copied to clipboard ({target}).",
                            &[("target", &format!("{target:?}"))],
                        ),
                    );
                }
                // A7: weder Systemwerkzeug noch OSC-52 verfügbar (z. B. harw
                // läuft in tmux ohne Zwischenablagen-Weiterleitung und der
                // Text übersteigt die OSC-52-Nutzlastgrenze) — statt nur den
                // Fehler zu melden, wird der Export als Datei gespeichert.
                Err(ExportError::NoClipboard) => {
                    write_export_to_default_path(app, &content, true, format);
                }
                Err(error) => {
                    app.push_line(
                        Role::System,
                        tr_args(
                            "app_notices.export_clipboard_error",
                            "Export: clipboard unavailable: {error}",
                            &[("error", &error.to_string())],
                        ),
                    );
                }
            }
        }
        1 => {
            let content = build_export(app, &opts, format);
            write_export_to_default_path(app, &content, false, format);
        }
        _ => {}
    }
}

/// Schreibt `content` über [`export::default_export_path`] ins aktuelle
/// Arbeitsverzeichnis und hängt je nach Ergebnis eine Systemzeile mit
/// der vollständigen Erfolgsmeldung bzw. dem Fehlschlag an.
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
/// - `format` (`ExportOutputFormat`): bestimmt die Dateiendung (Runde 6, Teil C).
fn write_export_to_default_path(
    app: &mut ChatApp,
    content: &str,
    clipboard_unavailable: bool,
    format: ExportOutputFormat,
) {
    // Runde 6, Teil C: die Dateiendung folgt dem Format (`.json` bei JSON).
    let path = export_capture::default_export_target(format);
    match export::write_export_path(&path, content) {
        Ok(written_path) => app.push_line(
            Role::System,
            if clipboard_unavailable {
                tr_args(
                    "app_notices.export_fallback",
                    "No clipboard — export saved to {path}",
                    &[("path", &written_path.display().to_string())],
                )
            } else {
                tr_args(
                    "app_notices.export_saved",
                    "Export saved: {path}",
                    &[("path", &written_path.display().to_string())],
                )
            },
        ),
        Err(error) => app.push_line(
            Role::System,
            tr_args(
                "app_notices.export_failed",
                "Export failed: {error}",
                &[("error", &error.to_string())],
            ),
        ),
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
    images: Vec<harw_protocol::MediaRef>,
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
    // Die direkt von der TUI getriebene Root-Session ist ebenfalls ein
    // sichtbarer Agentenlauf. Ohne diese Projektion konnte der Status unten
    // "denkt…" zeigen, während der Agenten-Dock "0 aktiv" meldete.
    let surface_root_id = app.session_id().as_str().to_owned();
    app.agent_monitor
        .begin_surface_root(&surface_root_id, "assistant");
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
    if let Some(spawner) = app.managed_spawner()
        && let Err(error) = spawner.register_parent_cancel_token(app.session_id(), cancel.clone())
    {
        tracing::warn!(error = %error, "tui.turn.register_parent_cancel_token_failed");
    }
    // Runde 5, Teil E: die Nutzernachricht ist das „Ziel der Sitzung“ für
    // den Auto-Modus-Klassifizierer (Geheimnisse entfernt er selbst).
    if let Some(rt) = app.runtime()
        && let Some(auto) = rt.auto_mode()
    {
        // Runde 6, Teil A: `set_goal` ist ein Ringpuffer der letzten drei
        // Nachrichten; die Sitzungs-Id bestimmt, wessen Host-Lease zählt.
        auto.context().set_goal(text);
        auto.set_lease_session(app.session_id().to_string());
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
    spinner.start();
    let reply = drive_turn_animated(
        guard,
        app,
        spinner,
        gateway,
        TurnInput::user(text)
            .with_images(images)
            .with_control(TurnControl::new().with_cancel(cancel)),
        approval_driver,
        approvals,
        host_permit_prompts,
        tui_rx,
        turn_event_rx,
        turn_state,
    )
    .await;
    spinner.stop();
    // Falls kein terminales TurnEvent angekommen ist, schließt der äußere
    // Turn-Lifecycle die Surface-Projektion. Ein bereits verarbeitetes
    // Failed/Aborted-Event gewinnt (finish_surface_root ist idempotent).
    let surface_cancelled = app.cancel_requested_at.is_some();
    let surface_failed = reply.is_err() && !surface_cancelled;
    app.agent_monitor
        .finish_surface_root(&surface_root_id, surface_cancelled, surface_failed);
    app.reasoning_feed.push_finished(
        &surface_root_id,
        "assistant",
        !surface_cancelled && !surface_failed,
    );
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
/// Runde 6, Teil C: Vorrang hat jetzt die Schlussantwort aus
/// `TurnEvent::ItemAdded`; der Index-Weg gilt nur noch ohne Verdichtung, sonst
/// greift ein Rückfall ohne Index (siehe `app/final_reply.rs`).
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
    // Runde 6, Teil C: Schlussantwort/Verdichtung gelten je Turn.
    turn_state.final_reply = final_reply::FinalReplyCapture::default();

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
                                // Full Access: Sitzungs-Lease ohne Dialog.
                                if let Some(prompt) = auto_grant_host_permit(app, prompt) {
                                    host_permit_shown_at = Some(open_host_permit_prompt(app, prompt));
                                }
                                draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                            }
                            None => {
                                tracing::warn!("tui.host_permit.prompt_channel_ended");
                                host_permit_prompts_open = false;
                            }
                        }
                    }
                    event = tui_rx.recv(), if input_open => {
                        // Mausrad nach Position und `scroll_panel_up/down`
                        // vor allen Fenstern (Scrollen entscheidet nichts).
                        if let Some(scroll_event) = event.as_ref()
                            && let Some(redraw) = scroll_routing::route_scroll_input(app, scroll_event)
                        {
                            if redraw {
                                draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                            }
                            continue;
                        }
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
                                        app.push_line(Role::System, tr_for(app.language, "app_notices.host_denied", "Host execution denied."));
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
                            let redraw = handle_turn_event(app, turn_state, event);
                            // Terminal-Ownership MUSS vor dem ersten Frame nach
                            // Host-Ausführung wiederhergestellt sein. Andersherum
                            // landet genau dieser Frame im normalen tmux-Scrollback
                            // und bleibt dort als duplizierter Dock stehen.
                            if app.take_needs_terminal_reassert() {
                                guard.reassert_terminal_modes();
                            }
                            if redraw {
                                draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
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
    //
    // Runde 6, Teil C: zuerst noch gepufferte Turn-Ereignisse verarbeiten
    // (die Schlussantwort kommt als `ItemAdded`), dann ohne Historie-Index
    // wählen — eine Auto-Verdichtung im Turn verkleinert den Verlauf, dann
    // fände `latest_final_reply` ab `history_len_before` nichts mehr.
    while let Ok(event) = turn_event_rx.try_recv() {
        handle_turn_event(app, turn_state, event);
        if app.take_needs_terminal_reassert() {
            guard.reassert_terminal_modes();
        }
    }
    let reply = final_reply::select_final_reply(
        &turn_state.final_reply,
        gateway.session_mut().history(),
        history_len_before,
        turn_state.last_revealed_reply.as_deref(),
    );
    let reply = suppress_if_matches_commentary(reply, turn_state.last_commentary_text.as_deref());
    if let Some(reply) = &reply {
        turn_state.last_revealed_reply = Some(reply.clone());
    }
    Ok(reply)
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

/// Beantwortet eine Host-Permit-Frage unter „Full Access" selbst, ohne
/// Dialog (Nutzerentscheidung 2026-09-24: Full Access fragt nie).
///
/// # Beschreibung
/// Unter [`ApprovalMode::FullAccess`] wird die Frage sofort mit
/// [`HostPermitVariant::SessionLease`] beantwortet: die Host-Arbeitsphase
/// gilt dann bis zum Ablauf ihres Fensters (`[sandbox] host_lease_ttl_secs`,
/// Vorgabe 8 h; PL-100 Phase 4, B22, ADR 0002) oder bis die Nutzerin sie
/// vorher mit Strg+H oder `/sandbox-lease revoke` beendet. Nach dem Ablauf
/// stellt der nächste Host-Befehl eine neue Frage, und auch die beantwortet
/// diese Funktion unter Full Access ohne Rückfrage mit einem neuen Fenster.
/// Der Ablauf begrenzt also nur Phasen außerhalb von Full Access; unter Full
/// Access bleibt B22 ein bewusst getragenes Restrisiko (Full Access fragt
/// nie, und die Taint-Regel gilt laut PL-100 O1 nur außerhalb von Full
/// Access). In jedem anderen Modus (oder ohne Runtime-Montage) kommt die
/// Frage unverändert zurück und der Aufrufer öffnet den Dialog wie bisher.
///
/// # Rückgabe
/// `None`, wenn die Frage beantwortet wurde; sonst `Some(prompt)`.
fn auto_grant_host_permit(app: &mut ChatApp, prompt: HostPermitPrompt) -> Option<HostPermitPrompt> {
    let mode = app.current_approval();
    auto_grant_host_permit_in_mode(app, prompt, mode)
}

/// [`auto_grant_host_permit`] mit ausdrücklich übergebenem Modus (testbar
/// ohne Runtime-Montage).
fn auto_grant_host_permit_in_mode(
    app: &mut ChatApp,
    prompt: HostPermitPrompt,
    mode: Option<ApprovalMode>,
) -> Option<HostPermitPrompt> {
    crate::i18n::with_language(app.language, || {
        auto_grant_host_permit_in_mode_scoped(app, prompt, mode)
    })
}

fn auto_grant_host_permit_in_mode_scoped(
    app: &mut ChatApp,
    prompt: HostPermitPrompt,
    mode: Option<ApprovalMode>,
) -> Option<HostPermitPrompt> {
    if mode != Some(ApprovalMode::FullAccess) {
        return Some(prompt);
    }
    tracing::info!(
        session = prompt.session(),
        worker = prompt.worker_definition(),
        "tui.host_permit.full_access_auto_lease"
    );
    let delivered = prompt.approve(HostPermitVariant::SessionLease);
    let message = if delivered {
        tr(
            "app_notices.host_auto",
            "Full Access: host work phase approved without asking — HOST MODE ACTIVE (Ctrl+H or /sandbox-lease revoke ends it).",
        )
    } else {
        tr(
            "app_notices.host_auto_late",
            "Full Access: host work phase approved, but the response was not delivered — the request has already moved on.",
        )
    };
    app.push_line(Role::System, message);
    None
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
    app.pending_host_permit_dialog = Some(crate::i18n::with_language(app.language, || {
        build_host_permit_dialog(&prompt)
    }));
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
        tr("app_notices.host_single_option", "Once").to_owned(),
        tr("app_notices.host_lease_option", "Host work phase").to_owned(),
        tr("app_notices.host_deny", "No, deny").to_owned(),
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
            tr("app_notices.sandbox_request", "Sandbox lease requested"),
            tr_args(
                "app_notices.sandbox_details",
                "Session {session} requests a sandbox lease. Reason: {reason}",
                &[("session", prompt.session()), ("reason", prompt.command())],
            ),
        )
    } else {
        (
            tr("app_notices.host_request", "Allow host execution?"),
            tr_args(
                "app_notices.host_details",
                "Worker {worker} requests host execution in session {session}: {command}",
                &[
                    ("worker", prompt.worker_definition()),
                    ("session", prompt.session()),
                    ("command", prompt.command()),
                ],
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
    crate::i18n::with_language(app.language, || {
        apply_host_permit_decision_scoped(app, prompt, option_index)
    })
}

fn apply_host_permit_decision_scoped(
    app: &mut ChatApp,
    prompt: HostPermitPrompt,
    option_index: usize,
) {
    let message = match option_index {
        0 => {
            let delivered = prompt.approve(HostPermitVariant::SingleExecution);
            if delivered {
                tr(
                    "app_notices.host_single_approved",
                    "Host execution approved once.",
                )
            } else {
                tr(
                    "app_notices.host_single_late",
                    "Host execution approved, but the response was not delivered — the request has already moved on.",
                )
            }
        }
        1 => {
            let delivered = prompt.approve(HostPermitVariant::SessionLease);
            if delivered {
                tr(
                    "app_notices.host_lease_approved",
                    "Host work phase approved — HOST MODE ACTIVE (Ctrl+H ends it).",
                )
            } else {
                tr(
                    "app_notices.host_lease_late",
                    "Host work phase approved, but the response was not delivered — the request has already moved on.",
                )
            }
        }
        _ => {
            prompt.deny();
            tr("app_notices.host_denied", "Host execution denied.")
        }
    };
    app.push_line(Role::System, message);
}

/// Risiko-Hinweis für den Freigabe-Dialog: nennt den Remote-OCR-Host, wenn
/// `doc.read_pdf` die Datei unter `[tools.doc].remote_ocr = "ask"` an einen
/// externen Dienst schicken würde; sonst `None`.
pub(crate) fn approval_risk(call: &ToolCall) -> Option<String> {
    // Auch ein Symlink nach außerhalb des Workspace: der Dialog nennt Link und Ziel.
    harw_registry_defaults::remote_ocr_approval_notice(call)
        .or_else(|| harw_registry_defaults::symlink_approval_notice(call))
        // Plan R9: erster Abruf einer Domain im offenen Recherche-Netz.
        .or_else(|| harw_registry_defaults::open_web_approval_notice(call))
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
        // Bleibt der Begründung des Agenten vorbehalten; der vom Kern
        // zugeschriebene Grund hat eine eigene Zeile (`with_core_reason`).
        justification: None,
        risk: approval_risk(call),
        origin: None,
        remember_rule,
        deadline: Instant::now() + timeout,
        reason_input_enabled: true,
    });
    // Runde 6, Teil A: fragt der Auto-Modus (auch statt einer Ablehnung),
    // steht sein Grund als „Auto-Modus: <Kategorie> – <Grund>“ im Dialog.
    // PL-100 O1: der vom Kern zugeschriebene Grund (DoD-Taint, „nach
    // DoD-Abfrage in diesem Turn“; bereits escapt) steht unter eigener
    // Beschriftung davor. Er ersetzt den Grund des Auto-Modus nicht: fragen
    // beide, zeigt der Dialog beide Zeilen, den Taint-Grund zuerst.
    let dialog = dialog
        .with_core_reason(prompt.reason().map(str::to_owned))
        .with_auto_reason(
            app.runtime()
                .and_then(|rt| rt.auto_mode())
                .and_then(|auto| auto.log().verdict_for(call.id.as_str()))
                .as_ref()
                .and_then(crate::permissions_view::auto_ask_reason_for),
        );
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
                let output = crate::command_exec::execute_command_as_for(
                    app.language,
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
                    let output = crate::command_exec::execute_command_as_for(
                        app.language,
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
    if approved
        && delivered
        && !learned
        && let Some(auto) = app.runtime().and_then(|rt| rt.auto_mode())
    {
        auto.record_manual_approval(&approved_call);
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
                        let tool_cell = ensure_tool_cell(
                            app,
                            turn_state,
                            prompt.call_id().clone(),
                            prompt.call(),
                            None,
                        );
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
                        // Full Access: Sitzungs-Lease ohne Dialog.
                        if let Some(prompt) = auto_grant_host_permit(app, prompt) {
                            // Dieselbe K3-Regel wie bei `approvals.recv()` oben:
                            // eine noch offene ältere Host-Permit-Frage wird nicht
                            // still überschrieben, sondern abgelehnt.
                            if let Some(stale) = app.pending_host_permit.take() {
                                tracing::warn!("tui.host_permit.stale_prompt_closed");
                                stale.deny();
                            }
                            app.pending_host_permit_dialog = Some(crate::i18n::with_language(app.language, || build_host_permit_dialog(&prompt)));
                            app.pending_host_permit = Some(prompt);
                            host_permit_shown_at = Some(Instant::now());
                        }
                        draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                    }
                    None => {
                        tracing::warn!("tui.host_permit.prompt_channel_ended");
                        host_permit_prompts_open = false;
                    }
                }
            }
            maybe_event = tui_rx.recv(), if input_open => {
                // Mausrad nach Position und `scroll_panel_up/down` vor allen
                // Fenstern (Scrollen entscheidet nichts).
                if let Some(scroll_event) = maybe_event.as_ref()
                    && let Some(redraw) = scroll_routing::route_scroll_input(app, scroll_event)
                {
                    if redraw {
                        draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                    }
                    continue;
                }
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
                                DialogAction::ToggleDetails | DialogAction::Scrolled => {
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
                                    app.push_line(Role::System, tr_for(app.language, "app_notices.host_denied", "Host execution denied."));
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
                    Some(TuiEvent::Resize(_, _)) => {
                        app.full_redraw.set(true);
                        app.refresh_cell_aspect();
                        draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
                    }
                    Some(TuiEvent::Draw) => {
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
                    let redraw = handle_turn_event(app, turn_state, event);
                    // Wie im normalen Turn-Pfad: erst Alternate-Screen/Modi
                    // reparieren, dann zeichnen. Kein Frame darf zwischen
                    // Host-Rückkehr und Reassert in den Terminal-Scrollback.
                    if app.take_needs_terminal_reassert() {
                        guard.reassert_terminal_modes();
                    }
                    if redraw {
                        draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
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
/// - Befehl: Ausgabe mit neutralem lokalisiertem Busy-Kopf; das validierte
///   Operationsergebnis beschreibt den Wirkungszeitpunkt. Danach wie
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
            data,
            language,
        } => {
            let output = OpOutput { text, data };
            app.apply_settings_completion(
                &command.raw,
                language,
                if succeeded {
                    Ok(&output)
                } else {
                    Err(&output.text)
                },
            );
            tracing::debug!(command = %command.raw, succeeded, "tui.busy.command_finished");
            // Runde 6, Teil C: Busy-Ergebnis auch in den Export.
            app.push_lines_exported(crate::i18n::with_language(language, || {
                busy_queue::command_result_lines(&command, &output.text)
            }));
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
            | LocalIntercept::AttachImage(_)
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
    // Runde 6, Teil B: `!`/`!!` läuft sofort (immediate) auf dem Host, nie
    // hinter dem Turn; das Ergebnis wird erst nach dem Ende eingereiht.
    if operator_shell::intercept(app, &raw) {
        return BusyKeyOutcome::Redraw;
    }
    if let Some(intercepted) = local_intercept_for(app, &raw) {
        if is_busy_safe_intercept(&intercepted) {
            // Busy-sichere Abfänge senden nichts auf den Bus (nur `Rewrite`
            // täte das); der Wegwerf-Kanal fängt es trotzdem sicher ab.
            let (scratch, _scratch_rx) = crate::events::harw_event_channel();
            let _ = apply_local_intercept(app, intercepted, &scratch);
            return BusyKeyOutcome::Local;
        }
        if let LocalIntercept::Chat(text) = intercepted {
            // Eigene Nachricht: ans Ende springen und wieder folgen.
            app.scroll.force_follow();
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
            HarwEvent::Submit(text) => {
                // Eigene Nachricht: ans Ende springen und wieder folgen.
                app.scroll.force_follow();
                app.pending_turns.push_back(text);
            }
            // Runde 6, Teil C: Bus-Systemzeile auch in den Export.
            HarwEvent::SystemMessage(message) => app.push_system_text_exported(&message),
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
                    // Eigene Nachricht: ans Ende springen und wieder folgen.
                    app.scroll.force_follow();
                    app.pending_turns.push_back(text);
                    BusyKeyOutcome::Redraw
                }
                LineAction::Command(raw) => route_busy_command(app, raw),
                LineAction::Quit | LineAction::Ignore => BusyKeyOutcome::Redraw,
                LineAction::System(message) => {
                    // Der Puffer wurde vom InputEditor schon geleert; ohne
                    // Rückholung und Systemzeile verschwindet eine während
                    // des Turns abgelehnte Eingabe (z. B. unterminiertes
                    // Quote) kommentarlos.
                    app.push_system_text_exported(&message);
                    app.input.insert_paste(&text);
                    app.sync_popup();
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
    crate::i18n::with_language(app.language, || handle_busy_event_scoped(app, event))
}

fn handle_busy_event_scoped(app: &mut ChatApp, event: TuiEvent) -> BusyKeyOutcome {
    // Abgelaufenen Beenden-Hinweis verwerfen — Pendant zur Ablaufprüfung im
    // Idle-Pfad von `run_loop`, hier für den Busy-Pfad (Welle 4c, Punkt 5).
    if let Some(arm) = app.pending_quit
        && arm.at.elapsed() > QUIT_HINT_WINDOW
    {
        app.pending_quit = None;
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
            // Ctrl+L zeichnet auch während eines Turns alles neu — gerade
            // beim Streamen bleiben auf Handy-Terminals sonst Reste stehen.
            if app.key_bindings.action_for(key) == Some(KeyAction::RedrawScreen) {
                app.full_redraw.set(true);
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
            // Runde 6, Teil B: Laufende `!`-Befehle bricht Ctrl+C zuerst
            // ab; der Turn arbeitet weiter, erst das nächste Ctrl+C gilt ihm.
            if operator_shell::cancel_on_ctrl_c(app) {
                app.pending_quit = None;
                return BusyKeyOutcome::Redraw;
            }
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
            let redraw = match scroll_routing::route_wheel(app, mouse) {
                Some(redraw) => redraw,
                None => app.scroll.handle_mouse(mouse, total, rows) == ScrollAction::Redraw,
            };
            if redraw {
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
    // Runde 6, Teil C: Anzeige und Export über dieselbe Stelle.
    final_reply::commit_final_reply(app, reply);
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

/// Zeichnet das Explorer-Panel. h22: Darunter wird — wenn eine Step-Liste
/// geladen ist — deren read-only Ansicht im unteren Bereich des Explorer-
/// Fensters ergänzt (Layout-Split, Fokuslogik des Panels unverändert).
fn render_explorer_panel(
    app: &ChatApp,
    area: Rect,
    buf: &mut ratatui::buffer::Buffer,
    theme: style::Theme,
) {
    if let Some(panel) = &app.explorer {
        if let Some(list) = &app.step_list {
            // Zwei Bereiche: Explorer oben, StepList unten (min. 3 Zeilen:
            // Kopf + 2 Steps; bei knapper Höhe entfällt die Ansicht nicht,
            // sondern teilt sich fair).
            let rows = u16::try_from(1 + list.steps().len().min(8)).unwrap_or(1);
            let split = ratatui::layout::Layout::vertical([
                ratatui::layout::Constraint::Min(3),
                ratatui::layout::Constraint::Length((rows + 1).max(3)),
            ])
            .split(area);
            panel.render(
                split[0],
                buf,
                theme,
                app.panels.focus == crate::panes::PaneFocus::Explorer,
            );
            render_step_list(list, split[1], buf, theme);
        } else {
            panel.render(
                area,
                buf,
                theme,
                app.panels.focus == crate::panes::PaneFocus::Explorer,
            );
        }
    }
}

/// Zeichnet die read-only StepList-Ansicht (h22). Reine Projektion: keine
/// Interaktion, keine Mutation — Änderungen laufen über den Step-Owner.
fn render_step_list(
    list: &harw_step_list::StepList,
    area: Rect,
    buf: &mut ratatui::buffer::Buffer,
    _theme: style::Theme,
) {
    use ratatui::text::Line;
    use ratatui::widgets::{Paragraph, Widget, Wrap};

    let Some((lines, done, total)) = crate::step_list_view::step_lines(list) else {
        return;
    };
    let mut rows = vec![Line::styled(
        crate::step_list_view::progress_line(done, total),
        ratatui::style::Style::new().add_modifier(ratatui::style::Modifier::BOLD),
    )];
    for line in lines {
        rows.push(Line::styled(
            format!("{} {}", line.mark, line.title),
            ratatui::style::Style::default(),
        ));
    }
    Paragraph::new(rows)
        .wrap(Wrap { trim: true })
        .render(area, buf);
}

/// Unter dieser Breite bekommen Goal-, Plan- und Host-Marke eine eigene
/// Statuszeile (Hochformat, Handy): auf einer Zeile würden sie Modus, Modell
/// und Kontext still verdrängen.
const STATUS_TWO_ROWS_MAX_COLS: u16 = 80;

/// Ab dieser Breite trägt die Host-Warnung noch ihren Tastenhinweis
/// „(Strg+H beendet)"; darunter nur „HOST-MODUS AKTIV".
const STATUS_HOST_HINT_MIN_COLS: u16 = 60;

/// Build owned effort presentation; ordered protocol actions remain fixed.
fn effort_choice_dialog(target: EffortTarget) -> ChoiceDialog {
    let mut options: Vec<String> = EFFORT_LEVELS
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
    options.push(tr("app_notices.effort_default", "Provider default (reset)").to_owned());

    let title = match target {
        EffortTarget::Session => tr("app_notices.effort_title", "Choose reasoning effort"),
        EffortTarget::Uia => tr(
            "app_notices.effort_uia_title",
            "Choose UIA reasoning effort",
        ),
    };
    ChoiceDialog::new(title, None, options)
}

fn export_choice_dialog() -> ChoiceDialog {
    ChoiceDialog::new(
        tr("app_notices.export_title", "Export"),
        Some(
            tr(
                "app_notices.export_prompt",
                "How should the session be exported?",
            )
            .to_owned(),
        ),
        vec![
            tr("app_notices.export_clipboard", "Copy to clipboard").to_owned(),
            tr("app_notices.export_file", "Save as file").to_owned(),
            tr("app_notices.cancel", "Cancel").to_owned(),
        ],
    )
}

/// Reserve a separate mark row on narrow terminals.
fn status_rows_for(app: &ChatApp, theme: style::Theme, width: u16) -> u16 {
    let has_marks = app.host_mode_active()
        || app.dod_mark.visible().is_some()
        || app.current_permission_stage() == PermissionCycleStage::Plan
        || !app.goal_status_spans(theme, width).is_empty();
    if width < STATUS_TWO_ROWS_MAX_COLS && has_marks {
        2
    } else {
        1
    }
}

/// Host-Warnung der Statuszeile (vorne, Warnfarbe, fett).
const HOST_MODE_MARK: &str = " HOST MODE ACTIVE (Ctrl+H ends it) ·";

/// Host-Warnung unter [`STATUS_HOST_HINT_MIN_COLS`]: ohne Tastenhinweis,
/// „HOST-MODUS AKTIV" steht immer vollständig da.
const HOST_MODE_MARK_SHORT: &str = " HOST MODE ACTIVE ·";

/// Plan-Marke, wenn die volle Marke neben der Host-Warnung nicht passt.
const PLAN_MARK_SHORT: &str = " ⏸ plan ";

/// Setzt die Marken der Statuszeile zusammen: Host-Warnung zuerst mit fest
/// reserviertem Platz, dann die DoD-Marke (PL-97 D5) und die Plan-Marke
/// (je voll, sonst kurz, sonst weg) und die Goal-Marke, gekürzt auf den Rest.
///
/// # Rückgabe
/// Die Spans und ihre Gesamtbreite in Spalten (nie über `width`, außer die
/// Host-Warnung allein ist breiter — sie wird nie gekürzt).
/// Sichtbarkeitsfenster für Info-Zwischenstände von Kind-Agenten in der
/// Statuszeile: 30 s, passend zur Drossel `PARENT_INFO_MIN_INTERVAL` in
/// `harw-core::child_comms`. Ältere Einträge bleiben in
/// [`ChatApp::child_infos`], werden aber nicht prominent gezeigt.
fn child_info_visible(age_secs: u64) -> bool {
    age_secs < 30
}

/// Kompaktformat für abgelaufene Kind-Statusmeldungen im
/// `consolidate`-Modus (`[tui].status_expiry = "consolidate"`):
/// nur noch Name und Alter, niedrig priorisiert.
fn child_info_expired_segment(child: &str, age_secs: u64) -> String {
    tr_args(
        "chrome.child_expired",
        "{child}: {seconds}s",
        &[("child", child), ("seconds", &age_secs.to_string())],
    )
}

/// Own the scroll hint at the app presentation boundary; ChatScroll retains
/// its geometry/state API and compatibility formatter.
pub(crate) fn scroll_indicator_text(scroll: &ChatScroll, key: &str) -> Option<String> {
    if scroll.is_following() {
        return None;
    }
    Some(match scroll.unseen_lines() {
        0 => tr_args(
            "chrome.scroll_up",
            "↑ scrolled up · {key} jumps to end",
            &[("key", key)],
        ),
        1 => tr_args(
            "chrome.scroll_one",
            "↓ 1 new line · {key} jumps to end",
            &[("key", key)],
        ),
        n => tr_args(
            "chrome.scroll_many",
            "↓ {count} new lines · {key} jumps to end",
            &[("count", &n.to_string()), ("key", key)],
        ),
    })
}

fn host_mode_mark(width: u16) -> String {
    let short = tr("chrome.host_active_short", HOST_MODE_MARK_SHORT);
    let full = tr("chrome.host_active", HOST_MODE_MARK);
    let text = if width >= STATUS_HOST_HINT_MIN_COLS
        && status_line::display_width(full) <= usize::from(width)
    {
        full
    } else if status_line::display_width(short) <= usize::from(width) {
        short
    } else {
        tr("chrome.host_tiny", "! HOST")
    };
    // At one column retain the alert instead of replacing it with an ellipsis.
    if width == 1 {
        "!".to_owned()
    } else {
        status_line::fit_width(text, usize::from(width))
    }
}

fn status_mark_spans(
    host: Option<Span<'static>>,
    dod: Option<(Span<'static>, Span<'static>)>,
    plan: Option<(Span<'static>, Span<'static>)>,
    goal: Vec<Span<'static>>,
    width: usize,
) -> (Vec<Span<'static>>, usize) {
    let mut spans = Vec::new();
    let mut used = 0usize;
    if let Some(host) = host {
        used += host.width();
        spans.push(host);
    }
    let mut remaining = width.saturating_sub(used);
    // PL-97 D5: DoD-Marke direkt nach der Host-Warnung, vor Plan und Goal
    // (voll, sonst kurz, sonst weg) — wie die Plan-Marke.
    for (full, short) in [dod, plan].into_iter().flatten() {
        let pick = if full.width() <= remaining {
            Some(full)
        } else if short.width() <= remaining {
            Some(short)
        } else {
            None
        };
        if let Some(span) = pick {
            remaining -= span.width();
            used += span.width();
            spans.push(span);
        }
    }
    for span in goal {
        if remaining == 0 {
            break;
        }
        let text = status_line::fit_width(span.content.as_ref(), remaining);
        let width = status_line::display_width(&text);
        remaining -= width.min(remaining);
        used += width;
        spans.push(Span::styled(text, span.style));
    }
    (spans, used)
}




#[cfg(test)]
mod history_localization_tests;
