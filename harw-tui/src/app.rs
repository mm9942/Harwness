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
    ShellRunOutcome, busy_availability_for, dispatch_command_with_shell_result,
    dispatch_slash_command, execute_command_as,
};
use crate::command_popup::{CommandPopup, PopupAction, TabOutcome};
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
// Hinweis: die drei obigen Typen sind Re-Exporte aus
// `harw_tool_shell::host_permit_prompt` (siehe `crate::host_permit_dialog`-
// Moduldoku) — der Fragevertrag und die Ausstellungslogik leben dort bzw. in
// `harw_tool_shell::exec::ShellExecutor::authorize_host_command`; `app.rs`
// besitzt nur noch Rendering, Vorauswahl-Anzeige und das Arming-Delay.
use crate::input_editor::{InputAction, InputEditor};
use crate::input_history::InputHistoryStore;
use crate::runtime_commands;
use crate::{
    CapabilitySet, CommandAction, CommandRegistry, DispatchContext, Invocation, InvocationSurface,
    ShellCapability,
};
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
    /// Zeitpunkt, zu dem `Ctrl+C` zuletzt tatsächlich eine nicht-leere
    /// Eingabe-Warteschlange (`deferred_input`/`pending_turns`) verworfen hat
    /// (Fix E / Teil 1b). Rein transienter Statuszeilen-Hinweis (siehe
    /// [`render_viewport`]), analog zu `cancel_requested_at` — erzeugt KEINE
    /// dauerhafte Verlaufszeile. Wird an denselben Stellen wie
    /// `cancel_requested_at` zurückgesetzt (neuer Turn, Turn-Ende).
    queue_cleared_at: Option<Instant>,
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
    /// Live gestreamter Assistant-Text der laufenden Modell-Runde der Wurzel;
    /// transient unter dem Verlauf gezeichnet, geleert sobald die finale
    /// Nachricht als Zelle vorliegt.
    live_stream: String,
    /// Live gestreamtes Reasoning der laufenden Runde (nur Vorschau).
    live_reasoning: String,
    /// Sichtbarkeit und Fokus der Seitenpanels.
    panels: crate::panes::PanelState,
    /// Aktive Tastenbelegung (Standard oder aus der Keybindings-Datei, siehe
    /// [`Self::set_key_bindings`]); gilt für Panels und Composer.
    key_bindings: KeyBindings,
    /// Explorer-Panel über den gesamten Projektbaum; beim ersten Einblenden
    /// angelegt und im Hintergrund indiziert.
    explorer: Option<crate::explorer_panel::ExplorerPanel>,
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
            .field(
                "has_pending_host_permit_dialog",
                &self.pending_host_permit_dialog.is_some(),
            )
            .field("host_mode_active", &self.host_mode_active())
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
            active_cancel: None,
            cancel_requested_at: None,
            queue_cleared_at: None,
            escape_armed: false,
            scroll: ChatScroll::new(),
            input_history,
            command_registry: CommandRegistry::from_command_adapters(&adapters),
            command_popup: None,
            theme: style::detect_theme(),
            total_usage: TokenUsage::default(),
            agent_monitor: crate::agent_monitor::AgentMonitor::default(),
            agent_rx: None,
            live_stream: String::new(),
            live_reasoning: String::new(),
            panels: crate::panes::PanelState::default(),
            key_bindings: KeyBindings::default(),
            explorer: None,
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
            historic_agent_events: Vec::new(),
            active_mode: InteractionMode::default(),
            plan_services: None,
            tool_verbosity: ToolVerbosity::Compact,
            tool_cells: Vec::new(),
            open_tool_group: None,
            ctrl_o_expand_last_armed: false,
            pending_approval_dialog: None,
            pending_host_permit: None,
            pending_host_permit_dialog: None,
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
        let context_label = match &target {
            PickerTarget::Orchestrator => "Modell-Auswahl",
            PickerTarget::Uia => "UIA-Modell-Auswahl",
            PickerTarget::UiaWorker { .. } => "UIA-Worker-Modell-Auswahl",
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
            role: "Wurzel-Orchestrator".to_owned(),
            depth: 0,
            status: "running".to_owned(),
            task: None,
            tokens: None,
            tool_calls: None,
            duration_ms: None,
            budget: "—".to_owned(),
            result: None,
            can_stop: false,
        }];
        let mut seen = HashSet::from([root.as_str().to_owned()]);
        if let Some(spawner) = self.managed_spawner() {
            append_agent_tree_rows(spawner, &root, 1, &mut seen, &mut rows);
        }
        append_historic_agent_tree_rows(&self.historic_agent_events, &root, &mut seen, &mut rows);
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
                self.set_approval_mode(ApprovalMode::AlwaysAsk);
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
    /// Liefert `true`, wenn sich Sichtbares geändert hat.
    pub(crate) fn drain_agent_events(&mut self) -> bool {
        let mut changed = self.poll_explorer();
        let Some(rx) = self.agent_rx.as_mut() else {
            return changed;
        };
        loop {
            match rx.try_recv() {
                Ok(event) => changed |= self.agent_monitor.apply(&event),
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
        changed
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

/// Erkennt die bare Form eines Befehls **oder** dessen argloses `switch`
/// (Welle 4a/7b: `/model`, `/model switch`, `/uia-effort switch`, …).
///
/// # Beschreibung
/// `raw.trim() == command` oder `raw.trim() == format!("{command} switch")`
/// — jeweils exakt, kein zusätzliches Argument. `/model switch <id>` bleibt
/// unberührt (bleibt Text-Dispatch); nur die beiden argumentlosen Formen
/// öffnen den jeweiligen Picker.
///
/// # Argumente
/// - `raw` (`&str`): die unveränderte Befehlszeile.
/// - `command` (`&str`): der zu erkennende Befehl, z. B. `"/model"`.
///
/// # Rückgabe
/// `true` für die bare Form oder das arglose `switch`, sonst `false`.
fn is_bare_or_argless_switch(raw: &str, command: &str) -> bool {
    let trimmed = raw.trim();
    trimmed == command || trimmed == format!("{command} switch")
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

    loop {
        // Eine während des vorherigen Turns abgeschickte Eingabe hat Vorrang.
        // Sie passiert denselben Pfad wie eine frische `HarwEvent::Submit`.
        let mut submitted: Option<String> = app.pending_turns.pop_front();

        if submitted.is_none() {
            tokio::select! {
                maybe_tev = async { match app.deferred_input.pop_front() { Some(event) => Some(event), None => tui_rx.recv().await } } => {
                    let Some(tev) = maybe_tev else { return Ok(TuiRunOutcome::Quit) };

                    // Abgelaufenen Beenden-Hinweis verwerfen.
                    if let Some(arm) = app.pending_quit {
                        if arm.at.elapsed() > QUIT_HINT_WINDOW {
                            app.pending_quit = None;
                        }
                    }

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
                            // Die bare Form ist eine interaktive Projektion;
                            // `/agent list` bleibt der textuelle Slash-Befehl.
                            if raw.trim() == "/agent" {
                                app.open_agent_tree();
                                frame_req.schedule_frame();
                                continue;
                            }
                            // `/model`/`/uia-model`/`/uia-worker-model` ohne
                            // Argument (oder als argloses `switch`) öffnen den
                            // konsolidierten Provider/Modell-Picker statt der
                            // Text-Ausgabe (`show`) — vor dem regulären
                            // `/command`-Dispatch abgefangen, damit `show`
                            // nicht zusätzlich läuft. `/model switch <id>` mit
                            // Argument bleibt unberührt und läuft unverändert
                            // weiter unten. Bare `/provider`/`/uia-provider`
                            // öffnen seit der Picker-Konsolidierung (Welle 4a)
                            // **keinen** Picker mehr — sie laufen unverändert
                            // auf `show` durch den regulären Dispatch.
                            if is_bare_or_argless_switch(&raw, "/model") {
                                app.open_model_switch_picker(PickerTarget::Orchestrator);
                                frame_req.schedule_frame();
                                continue;
                            }
                            if is_bare_or_argless_switch(&raw, "/uia-model") {
                                app.open_model_switch_picker(PickerTarget::Uia);
                                frame_req.schedule_frame();
                                continue;
                            }
                            if is_bare_or_argless_switch(&raw, "/uia-worker-model") {
                                match app.resolved_config() {
                                    // Der Worker ist zwingend an den effektiven
                                    // UIA-Provider gebunden (UIA-Pin, sonst der
                                    // aktive/Standard-Provider) — keine eigene
                                    // `uia_worker_provider`-Konzeption.
                                    Some(config) => {
                                        let provider = config
                                            .harness
                                            .uia_provider
                                            .clone()
                                            .or_else(|| app.active_or_default_provider(&config));
                                        match provider {
                                            Some(fixed_provider) => app.open_model_switch_picker(
                                                PickerTarget::UiaWorker { fixed_provider },
                                            ),
                                            None => app.push_line(
                                                Role::System,
                                                "UIA-Worker-Modell-Auswahl nicht verfügbar: kein \
                                                 UIA-Pin-, aktiver oder Standard-Provider bekannt.",
                                            ),
                                        }
                                    }
                                    None => app.push_line(
                                        Role::System,
                                        "UIA-Worker-Modell-Auswahl nicht verfügbar: keine \
                                         Konfiguration geladen.",
                                    ),
                                }
                                frame_req.schedule_frame();
                                continue;
                            }
                            // `/effort`/`/uia-effort` ohne Argument (oder als
                            // argloses `switch`) öffnen die einstufige
                            // Effort-Auswahl (Welle 7b) — dieselbe
                            // Abfang-Reihenfolge wie beim Modell-Picker.
                            // `/effort <level>` mit Argument bleibt unberührt.
                            if is_bare_or_argless_switch(&raw, "/effort") {
                                app.open_effort_choice(EffortTarget::Session);
                                frame_req.schedule_frame();
                                continue;
                            }
                            if is_bare_or_argless_switch(&raw, "/uia-effort") {
                                app.open_effort_choice(EffortTarget::Uia);
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
                                            match execute_export_command_with_data(
                                                app.adapters(),
                                                app.sandbox(),
                                                app.session_id(),
                                                caller_tier,
                                                &raw,
                                                || runtime_commands::slash_service_map(rt.services()),
                                            )
                                            .await
                                            {
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
        let displayed_text = app
            .pending_turn_user_cell_override
            .take()
            .unwrap_or_else(|| text.clone());
        app.push_line(Role::User, displayed_text);
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
            &text,
            approval_driver,
            approvals,
            host_permit_prompts,
            tui_rx,
            turn_event_rx,
            &mut turn_state,
        )
        .await;

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
fn handle_turn_event(app: &mut ChatApp, state: &mut TurnEventState, event: TurnEvent) -> bool {
    match event {
        TurnEvent::ToolCallRequested {
            call_id,
            tool_name,
            arguments,
            ..
        } => {
            app.clear_live_stream();
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
            // Nur ein kurzer Schwanz bleibt sichtbar.
            let count = app.live_reasoning.chars().count();
            if count > 400 {
                app.live_reasoning = app.live_reasoning.chars().skip(count - 400).collect();
            }
            true
        }
        TurnEvent::CompactionApplied {
            reason,
            items_before,
            items_after,
            ..
        } => {
            app.push_line(
                Role::System,
                format!("⟲ Kontext verdichtet ({reason}): {items_before} → {items_after} Einträge"),
            );
            true
        }
        TurnEvent::UsageUpdated { .. } | TurnEvent::ContextUpdated { .. } => {
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
        TurnEvent::TurnFailed { reason, .. } => {
            mark_incomplete_tool_exports(app, state, &format!("unvollständig ({reason})"))
        }
        TurnEvent::TurnAborted { .. } => {
            mark_incomplete_tool_exports(app, state, "unvollständig (abgebrochen)")
        }
        // TurnCompleted läuft exklusiv über den SessionEvent-Pfad, damit die
        // Token-Summary nicht doppelt erscheint. TurnStarted trägt keinen
        // zusätzlichen sichtbaren Zustand.
        _ => false,
    }
}

/// Marks outstanding tools as incomplete when a turn terminates early.
fn mark_incomplete_tool_exports(
    app: &mut ChatApp,
    state: &mut TurnEventState,
    reason: &str,
) -> bool {
    let call_ids: Vec<_> = state.pending_tool_cells.keys().cloned().collect();
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
        cell.mark_incomplete();
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

/// Führt den strukturierten `/export`-Command bis zum `OpOutput` aus.
///
/// `execute_command_as` liefert aus Kompatibilitätsgründen nur den
/// Anzeigetext zurück. Für `/export` muss die TUI zusätzlich `data` behalten;
/// deshalb wird hier ausschließlich dieser eine Command über dieselbe
/// Registry-/Admission-/Adapter-Pipeline ausgeführt. Für alle anderen Commands
/// gibt die Funktion `None` zurück, sodass der bestehende Pfad unverändert
/// verwendet werden kann.
async fn execute_export_command_with_data<F>(
    adapters: &[CommandAdapter],
    sandbox: &SandboxSpec,
    session_id: &SessionId,
    caller_tier: crate::PermissionTier,
    raw_line: &str,
    services: F,
) -> Option<Result<OpOutput, String>>
where
    F: FnOnce() -> harw_operations::ServiceMap,
{
    let invocation = crate::classify_input(raw_line).ok()?;
    let Invocation::Command { name, .. } = &invocation else {
        return None;
    };
    if name != "export" {
        return None;
    }

    let registry = CommandRegistry::from_command_adapters(adapters);
    let action = match registry.dispatch(
        DispatchContext {
            caller_tier,
            surface: InvocationSurface::Tui,
            capabilities: CapabilitySet::with(ShellCapability::CommandsShell),
        },
        invocation,
    ) {
        Ok(action) => action,
        Err(error) => return Some(Err(format!("Eingabe abgelehnt: {error}"))),
    };

    let CommandAction::Command(spec, raw_args) = action else {
        return Some(Err(
            "Eingabe abgelehnt: /export ist kein ausführbarer Command.".to_owned(),
        ));
    };
    let path = format!("/{}", spec.name.as_str());
    let Some(adapter) = adapters.iter().find(|adapter| adapter.path() == path) else {
        return Some(Err(format!("Unbekannter Command: /{}", spec.name.as_str())));
    };
    let ctx = harw_operations::OpContext::new(
        session_id.clone(),
        harw_types::TurnId::new(),
        sandbox.clone(),
        services(),
    );
    Some(
        adapter
            .dispatch(&ctx, raw_args)
            .await
            .map_err(|error| format!("Fehler: {error}")),
    )
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
            ModelSwitchAction::Accept { model, .. } => {
                let target = picker.target().clone();
                app.overlay = None;
                let command = match target {
                    PickerTarget::Orchestrator => format!("/model switch {model}"),
                    PickerTarget::Uia => format!("/uia-model switch {model}"),
                    PickerTarget::UiaWorker { .. } => {
                        format!("/uia-worker-model switch {model}")
                    }
                };
                bus.send(HarwEvent::Command(command));
            }
        },
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
        || app.pending_approval_dialog.is_some()
        || app.pending_host_permit_dialog.is_some()
    {
        return None;
    }
    match app.panels.handle_key(key, &app.key_bindings) {
        crate::panes::PanelKey::Ignored => None,
        crate::panes::PanelKey::Changed => {
            app.ensure_explorer();
            Some(true)
        }
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
                _ => false,
            },
            crate::panes::PaneFocus::Explorer => handle_explorer_key(app, key),
            crate::panes::PaneFocus::Chat => false,
        }),
    }
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

    // Das erste Escape schließt ausschließlich die Autovervollständigung.
    // Ein direkt folgendes Escape erreicht danach den Composer und leert ihn.
    if matches!(key.code, KeyCode::Esc) && app.has_popup() {
        app.command_popup = None;
        app.escape_armed = true;
        return true;
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
            // Tab vervollständigt shell-artig: unveränderte Markierung →
            // Rang-/Präfixlogik (`CommandPopup::tab_outcome`); bewusst per
            // Pfeiltaste/Ziffer bewegte Markierung → deren Auswahl gilt.
            KeyCode::Tab => {
                let outcome = app
                    .command_popup
                    .as_ref()
                    .map(CommandPopup::tab_outcome)
                    .unwrap_or(TabOutcome::None);
                match outcome {
                    TabOutcome::None => {}
                    TabOutcome::Accept(name) => {
                        app.input.clear();
                        app.input.insert_str(&format!("/{name} "));
                        app.command_popup = None;
                    }
                    TabOutcome::ExtendQuery(common) => {
                        app.input.clear();
                        app.input.insert_str(&format!("/{common}"));
                        app.sync_popup();
                    }
                }
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

    // Keep a clone in the UI so Ctrl+C can cancel the core turn at its
    // cooperative checkpoints instead of merely queuing a character.
    let cancel = CancelToken::new();
    app.active_cancel = Some(cancel.clone());
    // Ein neuer Turn startet: ein evtl. noch angezeigter "Abbruch
    // angefordert …"-Hinweis aus einem vorherigen, jetzt abgeschlossenen Turn
    // gehört nicht mehr zum aktuellen Zustand. Derselbe Reset gilt für den
    // "Warteschlange verworfen"-Hinweis (Fix E / Teil 1b).
    app.cancel_requested_at = None;
    app.queue_cleared_at = None;
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
    app.active_cancel = None;
    // Turn ist beendet (egal ob normal, per Fehler oder per Abbruch) — der
    // transiente Abbruch-Hinweis hat damit ausgedient. Derselbe Reset gilt
    // für den "Warteschlange verworfen"-Hinweis (Fix E / Teil 1b).
    app.cancel_requested_at = None;
    app.queue_cleared_at = None;

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
                                match handle_busy_event(app, TuiEvent::Key(key)) {
                                    BusyKeyOutcome::Redraw => draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?,
                                    BusyKeyOutcome::Idle => {}
                                    BusyKeyOutcome::RunImmediate(raw) => {
                                        run_immediate_busy_command(guard, app, spinner, &raw).await?;
                                    }
                                }
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
                                    ChoiceAction::Stay => match queue_busy_key(app, key) {
                                        BusyKeyOutcome::Redraw => draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?,
                                        BusyKeyOutcome::Idle => {}
                                        BusyKeyOutcome::RunImmediate(raw) => {
                                            run_immediate_busy_command(guard, app, spinner, &raw).await?;
                                        }
                                    },
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
                            Some(event) => match handle_busy_event(app, event) {
                                BusyKeyOutcome::Redraw => draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?,
                                BusyKeyOutcome::Idle => {}
                                BusyKeyOutcome::RunImmediate(raw) => {
                                    run_immediate_busy_command(guard, app, spinner, &raw).await?;
                                }
                            },
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
                    _ = tokio::time::sleep(SPINNER_INTERVAL) => {
                        spinner.tick();
                        app.drain_agent_events();
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

/// Bricht den laufenden Turn ab und lehnt/verweigert eine offene Freigabe-
/// oder Host-Permit-Frage — die Ctrl+C-Kernlogik innerhalb von
/// [`drive_pauses_to_completion`], solange ein solcher Dialog sichtbar ist
/// (Fix E / Teil 1b).
///
/// # Beschreibung
/// Vor diesem Fix lehnte Ctrl+C bei offenem Dialog **nur** den Dialog ab —
/// `active_cancel`/`pending_quit` blieben unberührt, der Turn und alle
/// laufenden Kind-Agenten liefen unangetastet weiter. Diese Funktion
/// ergänzt **zusätzlich** zum bestehenden Ablehnen/Verweigern denselben
/// kooperativen Abbruch (`active_cancel.cancel(CancelReason::User)`) und
/// dieselbe zweistufige Beenden-Scharfstellung ([`ChatApp::pending_quit`])
/// wie [`handle_busy_event`] ohne offenen Dialog, inklusive Leeren bereits
/// eingereihter Eingaben (`deferred_input`/`pending_turns`). Aus dem
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
    let had_queued_input = !app.deferred_input.is_empty() || !app.pending_turns.is_empty();
    app.deferred_input.clear();
    app.pending_turns.clear();
    if had_queued_input {
        app.queue_cleared_at = Some(Instant::now());
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
                    Some(event) if pending.is_none() && app.pending_host_permit.is_none() => {
                        match handle_busy_event(app, event) {
                            BusyKeyOutcome::Redraw => draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?,
                            BusyKeyOutcome::Idle => {}
                            BusyKeyOutcome::RunImmediate(raw) => {
                                run_immediate_busy_command(guard, app, spinner, &raw).await?;
                            }
                        }
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
                                DialogAction::Stay => match queue_busy_key(app, key) {
                                    BusyKeyOutcome::Redraw => draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?,
                                    BusyKeyOutcome::Idle => {}
                                    BusyKeyOutcome::RunImmediate(raw) => {
                                        run_immediate_busy_command(guard, app, spinner, &raw).await?;
                                    }
                                },
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
                                ChoiceAction::Stay => match queue_busy_key(app, key) {
                                    BusyKeyOutcome::Redraw => draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?,
                                    BusyKeyOutcome::Idle => {}
                                    BusyKeyOutcome::RunImmediate(raw) => {
                                        run_immediate_busy_command(guard, app, spinner, &raw).await?;
                                    }
                                },
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
                        // Ein Maus-Ereignis kann `handle_busy_event` niemals in
                        // `BusyKeyOutcome::RunImmediate` überführen (nur ein
                        // fertig abgeschicktes Slash-Kommando kann das) — der
                        // volle Match bleibt trotzdem, damit ein künftiger
                        // Enum-Zweig hier nicht stillschweigend ignoriert wird.
                        match handle_busy_event(app, TuiEvent::Mouse(mouse)) {
                            BusyKeyOutcome::Redraw => draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?,
                            BusyKeyOutcome::Idle => {}
                            BusyKeyOutcome::RunImmediate(raw) => {
                                run_immediate_busy_command(guard, app, spinner, &raw).await?;
                            }
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
            _ = tokio::time::sleep(SPINNER_INTERVAL) => {
                spinner.tick();
                app.drain_agent_events();
                draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))?;
            }
        }
    }
}

/// Ergebnis eines Tastendrucks/Ereignisses während eines laufenden Turns
/// (Welle 4b, `queue_busy_key`/`handle_busy_event`).
///
/// # Beschreibung
/// Ersetzt das frühere `bool` (`true` → Redraw, `false` → Idle): ein fertig
/// abgeschickter Slash-Befehl mit `BusyAvailability::Immediate`
/// ([`busy_availability_for`]) läuft nicht mehr über `deferred_input`, sondern
/// wird von den beiden `select!`-Schleifen (`drive_turn_animated`,
/// `drive_pauses_to_completion`) sofort über [`dispatch_slash_command`]
/// ausgeführt — der laufende Turn, der Composer, `pending_turns` und
/// `deferred_input` bleiben dabei unberührt.
#[derive(Debug, Clone, PartialEq, Eq)]
enum BusyKeyOutcome {
    /// Kein sichtbarer Zustand geändert — kein Redraw nötig.
    Idle,
    /// Sichtbarer Zustand geändert — Redraw nötig.
    Redraw,
    /// Eine fertig abgeschickte `/command`-Zeile mit
    /// `BusyAvailability::Immediate`; der Aufrufer dispatcht sie sofort über
    /// [`dispatch_slash_command`] und zeigt die Ausgabe wie im Idle-Pfad.
    RunImmediate(String),
}

/// Führt einen während eines laufenden Turns als
/// [`BusyKeyOutcome::RunImmediate`] gemeldeten Slash-Befehl sofort aus und
/// zeigt die Ausgabe wie im Idle-Pfad (Welle 4b).
///
/// # Beschreibung
/// Derselbe Anzeigepfad wie der `HarwEvent::Command`-Zweig im Idle-Teil von
/// [`run_loop`] (mehrzeilige Ausgabe an `\n` aufgeteilt, `app.push_lines`),
/// gebaut über den wiederverwendbaren Helfer [`dispatch_slash_command`]. Der
/// laufende Turn, der Composer, `pending_turns` und `deferred_input` bleiben
/// dabei unberührt — nur die Ausgabe landet im Verlauf, gefolgt von einem
/// Redraw.
///
/// # Argumente
/// - `guard` (`&mut TerminalGuard`): Terminal-Guard zum Zeichnen des Frames.
/// - `app` (`&mut ChatApp`): liefert Runtime/Adapter/Sandbox/Session-ID und
///   nimmt die Ausgabezeilen auf.
/// - `spinner` (`&Spinner`): aktueller Spinner-Frame für den Redraw.
/// - `raw` (`&str`): die abgeschickte `/command`-Zeile.
///
/// # Fehler
/// [`TuiError::Io`] beim Zeichnen.
async fn run_immediate_busy_command(
    guard: &mut TerminalGuard,
    app: &mut ChatApp,
    spinner: &Spinner,
    raw: &str,
) -> Result<(), TuiError> {
    if raw.trim() == "/agent" {
        app.open_agent_tree();
        return draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label));
    }
    let output = dispatch_slash_command(
        app.runtime(),
        app.adapters(),
        app.sandbox(),
        app.session_id(),
        raw,
    )
    .await;
    let lines: Vec<Line<'static>> = output
        .split('\n')
        .map(|line| Line::from(line.to_owned()))
        .collect();
    app.push_lines(lines);
    draw_viewport(guard, app, spinner, app.pending_quit.map(|arm| arm.label))
}

/// Bearbeitet den Composer während eines laufenden Turns. Chat-Zeilen gehen
/// direkt in die FIFO. Ein fertig abgeschickter Slash-Befehl mit
/// `BusyAvailability::Immediate` ([`busy_availability_for`]) wird als
/// [`BusyKeyOutcome::RunImmediate`] gemeldet, statt in `deferred_input`
/// eingereiht zu werden; jeder andere Slash-Befehl bleibt wie bisher als
/// Paste-plus-Enter in der nachgelagerten Eingabe-Queue: nach dem Turn läuft
/// er dadurch über denselben autorisierten Command-Kanal wie interaktiv
/// eingegebene Befehle, statt im Composer stecken zu bleiben oder still
/// verloren zu gehen.
fn queue_busy_key(app: &mut ChatApp, key: KeyEvent) -> BusyKeyOutcome {
    match app.input.handle_key(key) {
        InputAction::Submit(text) => {
            app.remember_input(&text);
            match classify_line(&text) {
                LineAction::Chat(text) => app.pending_turns.push_back(text),
                LineAction::Command(raw) => {
                    if raw.trim() == "/agent" {
                        return BusyKeyOutcome::RunImmediate(raw);
                    }
                    if busy_availability_for(&app.command_registry, &raw)
                        == BusyAvailability::Immediate
                    {
                        return BusyKeyOutcome::RunImmediate(raw);
                    }
                    app.deferred_input.push_back(TuiEvent::Paste(raw));
                    app.deferred_input.push_back(TuiEvent::Key(KeyEvent::new(
                        KeyCode::Enter,
                        KeyModifiers::NONE,
                    )));
                }
                LineAction::Quit | LineAction::Ignore | LineAction::System(_) => {}
            }
            BusyKeyOutcome::Redraw
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
    // `/agent` kann während eines Turns als Immediate-Befehl geöffnet werden.
    // Danach gehören seine Navigation und sein scoped `s`-Abbruch exklusiv
    // dem Overlay, auch während der Spinner weiterläuft.
    if let TuiEvent::Key(key) = &event {
        if matches!(app.overlay.as_ref(), Some(Overlay::AgentTree(_))) {
            handle_agent_tree_key(app, *key);
            return BusyKeyOutcome::Redraw;
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
            if let Some(cancel) = &app.active_cancel {
                cancel.cancel(CancelReason::User);
                // Nur ein transienter Statuszeilen-Hinweis (siehe
                // `render_viewport`) — keine dauerhafte `push_line`-Zeile, da
                // dieser Hinweis mit dem Turn-Ende oder einem neuen Turn
                // automatisch wieder verschwinden muss.
                app.cancel_requested_at = Some(Instant::now());
                // Fix E (Teil 1b): ein einziger Ctrl+C-Druck wirft bereits
                // eingereihte Eingaben weg — sonst würden während des Turns
                // eingereihte Nachrichten/Befehle nach dem Abbruch automatisch
                // als nächster Turn ausgeliefert (siehe `run_loop`, wo
                // `pending_turns`/`deferred_input` an Turn-Grenzen gedraint
                // werden).
                let had_queued_input =
                    !app.deferred_input.is_empty() || !app.pending_turns.is_empty();
                app.deferred_input.clear();
                app.pending_turns.clear();
                if had_queued_input {
                    app.queue_cleared_at = Some(Instant::now());
                }
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
        TuiEvent::Key(key) => queue_busy_key(app, key),
    }
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
    match &app.overlay {
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
    let input_height = match (
        &app.pending_approval_dialog,
        &app.pending_host_permit_dialog,
    ) {
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

    // History | permanente Statuszeile | Eingabe. Der Sicherheitsmodus muss
    // sichtbar bleiben und darf nicht vom Verlauf verdrängt werden.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(1),
            Constraint::Length(input_height),
        ])
        .split(area);
    let status_area = chunks[1];
    let input_area = chunks[2];
    // Seitenpanels (Explorer links, Agenten rechts) teilen sich die obere
    // Fläche mit dem Verlauf; auf schmalen Terminals bleibt nur der Chat.
    let pane_areas = crate::panes::split(chunks[0], &app.panels);
    if let Some(agents_area) = pane_areas.agents {
        crate::agent_monitor::render_agents_panel(
            &app.agent_monitor,
            agents_area,
            frame.buffer_mut(),
            theme,
            app.panels.focus == crate::panes::PaneFocus::Agents,
        );
    }
    if let Some(explorer_area) = pane_areas.explorer {
        render_explorer_panel(app, explorer_area, frame.buffer_mut(), theme);
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
            format!(
                " | ctx {} {pct}% / {}",
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
    let agents_suffix = match app.agent_monitor.active_count() {
        0 | 1 => String::new(),
        n => format!(" | {n} Agenten aktiv"),
    };
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
    // Transienter Hinweis, dass Ctrl+C eine nicht-leere Eingabe-Warteschlange
    // (`deferred_input`/`pending_turns`) tatsächlich verworfen hat (Fix E /
    // Teil 1b) — analog zu `cancel_suffix` oben, keine dauerhafte
    // Verlaufszeile, verschwindet an denselben Stellen wieder
    // (`queue_cleared_at`-Reset an [`ChatApp`]).
    let queue_cleared_suffix = if app.queue_cleared_at.is_some() {
        " · Warteschlange verworfen"
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
    // Sichtbarkeit für bereits eingereihte, aber noch nicht gesendete
    // Nachrichten (`pending_turns`, siehe [`ChatApp`]): ohne diesen Hinweis
    // verschwindet eine während eines laufenden Turns abgeschickte Nachricht
    // scheinbar spurlos, bis sie beim Drainen der Queue plötzlich auftaucht.
    let queue_suffix = if app.pending_turns.is_empty() {
        String::new()
    } else if app.pending_turns.len() <= 2 {
        let preview: String = app
            .pending_turns
            .front()
            .map(|text| {
                let trimmed = text.trim();
                if trimmed.chars().count() > 30 {
                    let truncated: String = trimmed.chars().take(30).collect();
                    format!("{truncated}…")
                } else {
                    trimmed.to_owned()
                }
            })
            .unwrap_or_default();
        format!(" · wartet: \"{preview}\"")
    } else {
        format!(" · {n} Nachricht(en) warten", n = app.pending_turns.len())
    };
    let tool_suffix = if app.has_collapsed_tool_cells() {
        " · Ctrl+O: Werkzeugdetails"
    } else {
        ""
    };
    let pending_permission_suffix = app
        .pending_permission_stage()
        .map(|_| " · Freigabemodus wird nach dem Turn übernommen")
        .unwrap_or("");
    let status = format!(
        " {spinner_prefix}Shift+Tab: {permission} | Modus: {} | Σ Tokens: {} (in {}, out {}{cache_suffix}){context_suffix}{agents_suffix}{explorer_suffix}{cancel_suffix}{queue_cleared_suffix}{quit_suffix}{queue_suffix}{tool_suffix}{pending_permission_suffix}",
        app.active_mode().as_str(),
        crate::agent_monitor::human_tokens(usage.total()),
        crate::agent_monitor::human_tokens(usage.prompt_tokens()),
        crate::agent_monitor::human_tokens(usage.output_tokens),
    );
    // Solange eine Host-Arbeitsphase läuft, muss die Statuszeile das gemäß
    // `docs/design/mediated-process-execution.md` („permanent und
    // unübersehbar `HOST-MODUS AKTIV`") in einer eigenen, hervorgehobenen
    // Warnfarbe zeigen — ein einfacher String-Suffix in derselben Farbe wie
    // der Rest der Zeile wäre zu leicht zu übersehen.
    if app.host_mode_active() {
        let line = Line::from(vec![
            Span::styled(status, Style::default().fg(style::border_color(theme))),
            Span::styled(
                " · HOST-MODUS AKTIV (Strg+H beendet)",
                Style::default()
                    .fg(style::warning_color(theme))
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        frame.render_widget(Paragraph::new(line), status_area);
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
        all_lines.push(Line::styled(
            format!(
                "  ∴ {}",
                crate::sanitize::sanitize_inline(&app.live_reasoning)
            ),
            style::dim_style(theme),
        ));
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
    let (title_text, title_style, border_style) = if shell_mode {
        (
            " Shell-Modus · Enter führt aus · Esc/Backspace am Anfang verlässt ",
            style::shell_mode_style(theme),
            Style::default().fg(style::shell_mode_color(theme)),
        )
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
        // fertiges Slash-Kommando ohne `BusyAvailability::Immediate` wird für
        // die autorisierte Nach-Turn-Ausführung als Paste+Enter in
        // `deferred_input` gelegt (Welle 4b: ein `Immediate`-Kommando meldet
        // stattdessen `BusyKeyOutcome::RunImmediate`, siehe die eigenen Tests
        // dafür unten). Diese Assertions prüfen jetzt genau das, statt die
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

    /// Welle 4b: `/status` trägt `BusyAvailability::Immediate` und nur seine
    /// Anzeige läuft — `queue_busy_key` meldet `RunImmediate` statt den
    /// Befehl in `deferred_input` einzureihen; Composer und `deferred_input`
    /// bleiben unberührt (die eigentliche Ausführung obliegt dem Aufrufer,
    /// siehe `run_immediate_busy_command`).
    #[test]
    fn busy_turn_immediate_command_reports_run_immediate_without_touching_deferred_input()
    -> TestResult {
        let mut app = test_chat_app()?;
        app.input.insert_str("/status");

        let outcome = queue_busy_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(outcome, BusyKeyOutcome::RunImmediate("/status".to_owned()));
        assert!(app.input.is_empty());
        assert!(app.deferred_input.is_empty());
        Ok(())
    }

    /// `/mode plan` bleibt `DeferredUntilTurnEnd` (kein `Immediate`-Befehl aus
    /// Welle 2d/3d/4a) — unverändertes Verhalten: Paste+Enter in
    /// `deferred_input`, für die autorisierte Ausführung nach Turn-Ende.
    #[test]
    fn busy_turn_queues_submitted_command_for_authorized_dispatch_after_turn() -> TestResult {
        let mut app = test_chat_app()?;
        app.input.insert_str("/mode plan");

        assert_eq!(
            queue_busy_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            BusyKeyOutcome::Redraw
        );
        assert!(app.input.is_empty());
        assert_eq!(
            app.deferred_input.pop_front(),
            Some(TuiEvent::Paste("/mode plan".to_owned()))
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

    /// `/model switch x` bleibt eingereiht (Welle 4b-Sonderfall: `model` ist
    /// `Immediate` markiert, aber nur `show`/`list` dürfen sofort laufen).
    #[test]
    fn busy_turn_model_switch_with_argument_stays_deferred() -> TestResult {
        let mut app = test_chat_app()?;
        app.input.insert_str("/model switch x");

        assert_eq!(
            queue_busy_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            BusyKeyOutcome::Redraw
        );
        assert_eq!(
            app.deferred_input.pop_front(),
            Some(TuiEvent::Paste("/model switch x".to_owned()))
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

    /// Fix E (Teil 1b): ein erster Ctrl+C-Druck wirft bereits eingereihte
    /// Eingaben weg, statt sie nach dem Abbruch automatisch als nächsten Turn
    /// auszuliefern (`run_loop`, das `pending_turns`/`deferred_input` an
    /// Turn-Grenzen abarbeitet). Vor diesem Fix blieben beide Warteschlangen
    /// unangetastet und die eingereihte Nachricht liefe unverändert nach.
    #[test]
    fn ctrl_c_during_busy_clears_deferred_input_and_pending_turns() -> TestResult {
        let mut app = test_chat_app()?;
        app.active_cancel = Some(CancelToken::new());
        app.deferred_input.push_back(TuiEvent::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        )));
        app.pending_turns
            .push_back("noch nicht gesendet".to_owned());
        let ctrl_c = TuiEvent::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));

        assert_eq!(handle_busy_event(&mut app, ctrl_c), BusyKeyOutcome::Redraw);

        assert!(
            app.deferred_input.is_empty(),
            "Ctrl+C muss bereits eingereihte Tastatur-/Paste-Ereignisse verwerfen"
        );
        assert!(
            app.pending_turns.is_empty(),
            "Ctrl+C muss bereits abgeschickte, aber noch nicht ausgelieferte Nachrichten verwerfen"
        );
        assert!(
            app.queue_cleared_at.is_some(),
            "eine tatsächlich geleerte Warteschlange muss den transienten Statuszeilen-Hinweis setzen"
        );
        Ok(())
    }

    /// Leere Warteschlangen dürfen den „Warteschlange verworfen“-Hinweis
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
            app.queue_cleared_at.is_none(),
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
        assert!(
            app.pending_turns.is_empty(),
            "bereits eingereihte Nachrichten müssen verworfen werden"
        );
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

    /// Bare Form und argloses `switch` öffnen den Picker; `switch <id>` mit
    /// Argument bleibt Text-Dispatch (kein Picker).
    #[test]
    fn is_bare_or_argless_switch_matches_bare_and_argless_switch_only() -> TestResult {
        assert!(is_bare_or_argless_switch("/model", "/model"));
        assert!(is_bare_or_argless_switch("  /model  ", "/model"));
        assert!(is_bare_or_argless_switch("/model switch", "/model"));
        assert!(is_bare_or_argless_switch(
            "/uia-effort switch",
            "/uia-effort"
        ));

        assert!(!is_bare_or_argless_switch("/model switch x", "/model"));
        assert!(!is_bare_or_argless_switch("/model list", "/model"));
        assert!(!is_bare_or_argless_switch("/provider", "/model"));
        // Bare `/provider` selbst öffnet seit der Konsolidierung (Welle 4a)
        // keinen Picker mehr — es gibt schlicht keinen `is_bare_or_argless_switch`-
        // Aufruf mehr für `/provider`/`/uia-provider` im Trigger-Zweig
        // (siehe `run_loop`s `HarwEvent::Command`-Arm); diese Zeile hält nur
        // fest, dass der Prädikat selbst `/provider` nicht fälschlich matcht,
        // falls er versehentlich doch wieder verdrahtet würde.
        assert!(!is_bare_or_argless_switch("/provider switch", "/model"));
        Ok(())
    }

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
        assert!(
            app.deferred_input.is_empty(),
            "bereits eingereihte Eingaben müssen verworfen werden"
        );
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
