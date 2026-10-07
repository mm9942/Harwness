//! Live-Stream der Kind-Agenten im Hauptverlauf (Runde 5, Teil I).
//!
//! # Verantwortung
//! Startet die UIA einen Orchestrator als Kind (`root-orchestrator`, oder
//! einen Sub-Orchestrator wie `coding-orchestrator`), zeigt der Verlauf
//! dessen Werkzeugaufrufe, Reasoning und Assistententext **live** als
//! eingerückten Block direkt unter der Agent-Zeile ([`SubAgentCell`]) — mit
//! denselben Zellen und derselben Zusammenfassungs-/Gruppierungslogik wie
//! beim Wurzel-Agenten ([`ToolCell`], [`ToolGroupCell`],
//! [`ReasoningHistoryCell`], [`AssistantHistoryCell`]). Werkzeug- und
//! Reasoning-Zellen des Kindes sind per Ctrl+O aufklappbar: jede neue Zelle
//! wird als [`ChildCellHandle`] gemeldet und von `app.rs` in dieselbe
//! Ctrl+O-Liste gehängt wie die Zellen der Wurzel.
//!
//! Startet ein Orchestrator seinerseits Kinder, erscheinen sie im Block als
//! Zusammenfassungszeile ([`SubAgentCell`]). Ist ein solches Kind wieder ein
//! Orchestrator, bekommt es einen eigenen, weiter eingerückten Block
//! darunter (beliebige Tiefe bis [`MAX_DEPTH`]); Worker bleiben eine Zeile.
//!
//! # Welche Kinder streamen
//! [`ChildStreamMode`] (`[tui] child_stream`, umschaltbar per
//! `/agent stream <orchestrators|all|none>`):
//! - `orchestrators` (Vorgabe): nur Kinder, deren **Rollendefinition** die
//!   Organisationsrolle `root-orchestrator` oder `child-orchestrator` trägt
//!   ([`OrchestratorRoles`], gespeist aus den eingebauten und den lokalen
//!   Agentendefinitionen). Nur für Rollen ohne jede Definition greift die
//!   Namenskonvention `*-orchestrator` als Notbehelf.
//! - `all`: jedes Kind.
//! - `none`: kein Block; Kinder erscheinen nur als Zusammenfassungszeile.
//!
//! Ein Block entsteht beim Start des Kindes, wenn der Modus es zulässt. Das
//! Umschalten wirkt sofort auf die Darstellung bestehender Blöcke (`none`
//! blendet alle aus, `orchestrators` blendet Worker-Blöcke aus) und auf
//! neu startende Kinder.
//!
//! # Deckel
//! Ein Block hält höchstens `cap` Einträge (Vorgabe
//! [`DEFAULT_VISIBLE_ENTRIES`]); ältere fallen vorne heraus und werden als
//! „… n frühere Schritte“ gezählt. Lange Läufe lassen den Verlauf also nicht
//! endlos wachsen. Live-Deltas (Text, Reasoning) werden als Schwanz von
//! höchstens [`LIVE_MAX_CHARS`] Zeichen gehalten und mit höchstens
//! [`LIVE_MAX_LINES`] Zeilen gezeigt.
//!
//! # Performance
//! Kein Neuaufbau des Verlaufs pro Ereignis: der Block ist **eine** geteilte
//! Verlaufszelle (`Arc<Mutex<ChildStreamBlock>>`), in die Ereignisse als
//! Deltas geschrieben werden (neuer Eintrag anhängen bzw. vorhandene
//! Werkzeugzelle fortschreiben).
//!
//! # Sicherheit
//! Gezeigt wird ausschließlich, was das Kind ohnehin als `TurnEvent` meldet.
//! Argumente, Ergebnisse, Reasoning und Text laufen durch dieselbe
//! Geheimnis-Redaction wie der Export ([`crate::export::redact_text`],
//! [`crate::export::redact_json_value`]) und beim Rendern durch
//! [`crate::sanitize`] (über die wiederverwendeten Zellen). Ein
//! sudo-Passwort taucht nie in Ereignissen auf.
//!
//! # Andere Prozesse
//! Läuft ein Kind in einem anderen Prozess, erreichen seine Turn-Ereignisse
//! den Agenten-Bus nicht; der Block bleibt leer (rendert keine Zeile) und
//! die Agent-Zeile zeigt wie bisher die Zusammenfassung. Kein Fehler.
//!
//! # Reihenfolge
//! Die Agent-Zeile entsteht über den Turn-Kanal der Wurzel, die Ereignisse
//! des Kindes kommen über den Agenten-Bus. Treffen Kind-Ereignisse vor der
//! Agent-Zeile ein, werden sie begrenzt vorgehalten
//! ([`PENDING_MAX_AGENTS`] × [`PENDING_MAX_EVENTS`]) und beim Anlegen des
//! Blocks nachgespielt.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use harw_agent_dsl::ExecutableAgentIr;
use harw_agent_dsl::roles::AgentRoleId;
use harw_extension_api::{ToolCall, ToolName};
use harw_protocol::events::TurnEvent;
use harw_protocol::items::{ContentPart, ToolCallResult, TurnItem};
use harw_types::{SessionId, ToolCallId};

use crate::export::{redact_json_value, redact_text};
use crate::history_cell::{
    AssistantHistoryCell, HistoryCell, ReasoningHistoryCell, SharedReasoningCell, SharedToolCell,
    SubAgentCell, SubAgentStatus, ToolCell, ToolGroupCell, ToolVerbosity, wrap_plain,
};
use crate::sanitize::sanitize_inline;
use crate::style;

// ─── Konstanten ──────────────────────────────────────────────────────────────

/// Vorgabe für die Zahl sichtbarer Einträge je Block (Deckel).
pub(crate) const DEFAULT_VISIBLE_ENTRIES: usize = 12;

/// Größte Verschachtelungstiefe eines Blocks (1 = Kind der Wurzel). Tiefere
/// Orchestratoren erscheinen nur noch als Zusammenfassungszeile.
pub(crate) const MAX_DEPTH: usize = 4;

/// Obergrenze (Zeichen) des vorgehaltenen Live-Schwanzes (Text/Reasoning).
pub(crate) const LIVE_MAX_CHARS: usize = 2000;

/// Maximale Zahl gezeigter Live-Zeilen je Art.
pub(crate) const LIVE_MAX_LINES: usize = 3;

/// Maximale Zahl gezeigter Zeilen eines Assistententexts im Block.
const ASSISTANT_MAX_LINES: usize = 12;

/// Höchstens so viele Agenten mit vorgehaltenen (noch nicht zuordenbaren)
/// Ereignissen.
pub(crate) const PENDING_MAX_AGENTS: usize = 16;

/// Höchstens so viele vorgehaltene Ereignisse je Agent.
pub(crate) const PENDING_MAX_EVENTS: usize = 128;

/// Höchstens so viele gemerkte Aufträge aus `ChildSpawned` (für `/agent`).
pub(crate) const SPAWN_TASKS_MAX: usize = 256;

/// Präfix jeder Blockzeile (eingerückt, dezenter Balken).
const BLOCK_PREFIX: &str = "  │ ";

/// Breite von [`BLOCK_PREFIX`] in Spalten.
const BLOCK_PREFIX_WIDTH: u16 = 4;

// ─── Modus ───────────────────────────────────────────────────────────────────

/// Welche Kind-Agenten live im Verlauf streamen (`[tui] child_stream`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ChildStreamMode {
    /// Nur Orchestrator-Kinder (Vorgabe).
    #[default]
    Orchestrators,
    /// Alle Kinder, auch Worker.
    All,
    /// Kein Live-Stream.
    Off,
}

impl ChildStreamMode {
    /// Liest einen Modus aus Nutzereingabe (`/agent stream <modus>`).
    ///
    /// # Rückgabe
    /// `Some(mode)` für `orchestrators`, `all` oder `none` (Groß-/
    /// Kleinschreibung egal; `off` gilt als `none`), sonst `None`.
    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "orchestrators" | "orchestrator" => Some(Self::Orchestrators),
            "all" => Some(Self::All),
            "none" | "off" => Some(Self::Off),
            _ => None,
        }
    }

    /// Anzeigeform, identisch mit dem Konfigurationswert.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Orchestrators => "orchestrators",
            Self::All => "all",
            Self::Off => "none",
        }
    }

    /// Ob ein Kind mit dieser Einstufung in diesem Modus live streamt.
    ///
    /// # Argumente
    /// - `orchestrator` (`bool`): Rollendefinition ist ein Orchestrator.
    pub(crate) fn admits(self, orchestrator: bool) -> bool {
        match self {
            Self::Orchestrators => orchestrator,
            Self::All => true,
            Self::Off => false,
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            Self::Orchestrators => 0,
            Self::All => 1,
            Self::Off => 2,
        }
    }

    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::All,
            2 => Self::Off,
            _ => Self::Orchestrators,
        }
    }
}

impl From<harw_config::ChildStreamModeToml> for ChildStreamMode {
    fn from(value: harw_config::ChildStreamModeToml) -> Self {
        match value {
            harw_config::ChildStreamModeToml::Orchestrators => Self::Orchestrators,
            harw_config::ChildStreamModeToml::All => Self::All,
            harw_config::ChildStreamModeToml::Off => Self::Off,
        }
    }
}

/// Zwischen Register und allen Blöcken geteilter Modus: ein Umschalten per
/// `/agent stream` wirkt sofort auf die Darstellung jedes Blocks.
#[derive(Debug, Clone)]
struct SharedMode(Arc<AtomicU8>);

impl SharedMode {
    fn new(mode: ChildStreamMode) -> Self {
        Self(Arc::new(AtomicU8::new(mode.to_u8())))
    }

    fn get(&self) -> ChildStreamMode {
        ChildStreamMode::from_u8(self.0.load(Ordering::Relaxed))
    }

    fn set(&self, mode: ChildStreamMode) {
        self.0.store(mode.to_u8(), Ordering::Relaxed);
    }
}

// ─── Orchestrator-Erkennung ──────────────────────────────────────────────────

/// Einstufung von Rollennamen anhand ihrer Agentendefinition.
///
/// # Beschreibung
/// Ein Rollenname (Spezialisierung, z. B. `coding-orchestrator`) gilt als
/// Orchestrator, wenn seine Definition die Organisationsrolle
/// [`AgentRoleId::RootOrchestrator`] oder [`AgentRoleId::ChildOrchestrator`]
/// trägt. Künftige Sub-Orchestratoren werden so ohne Namensliste erkannt.
/// Nur ein Name, zu dem es **keine** Definition gibt, fällt auf die
/// Konvention `*-orchestrator` zurück.
#[derive(Debug, Clone, Default)]
pub(crate) struct OrchestratorRoles {
    /// Namen mit Orchestrator-Definition.
    orchestrators: HashSet<String>,
    /// Alle Namen mit irgendeiner Definition.
    known: HashSet<String>,
}

/// Einmal geparste eingebaute Definitionen.
static BUILTIN_ROLES: OnceLock<OrchestratorRoles> = OnceLock::new();

impl OrchestratorRoles {
    /// Einstufung aus den eingebauten Agentendefinitionen
    /// (`harw-registry-defaults/agents/**.toml`).
    ///
    /// # Beschreibung
    /// Parst jede eingebettete Definition einmal (Ergebnis zwischengespeichert)
    /// und merkt Dateiname und Spezialisierung. Eine nicht parsbare Datei wird
    /// übersprungen (die Registry meldet sie ohnehin).
    pub(crate) fn builtin() -> Self {
        BUILTIN_ROLES
            .get_or_init(|| {
                let mut roles = Self::default();
                for (name, source) in harw_registry_defaults::embedded_agents::builtin_agent_toml()
                {
                    let Ok(raw) = harw_agent_dsl::parse::parse_toml(source) else {
                        continue;
                    };
                    roles.insert(name, raw.role);
                    roles.insert(&raw.specialization, raw.role);
                }
                roles
            })
            .clone()
    }

    /// Eingebaute plus lokal gesenkte Definitionen (`.harw/agents`); lokale
    /// Definitionen überschreiben gleichnamige eingebaute.
    ///
    /// # Argumente
    /// - `definitions`: z. B. `ConfigAgents::executable_agents`.
    pub(crate) fn from_definitions<'a, I>(definitions: I) -> Self
    where
        I: IntoIterator<Item = &'a ExecutableAgentIr>,
    {
        let mut roles = Self::builtin();
        for ir in definitions {
            roles.insert(ir.specialization(), ir.role());
        }
        roles
    }

    /// Trägt eine Definition ein (überschreibt eine frühere Einstufung).
    ///
    /// # Argumente
    /// - `name` (`&str`): Rollen-/Spezialisierungsname.
    /// - `role` ([`AgentRoleId`]): Organisationsrolle der Definition.
    pub(crate) fn insert(&mut self, name: &str, role: AgentRoleId) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        self.known.insert(name.to_owned());
        if matches!(
            role,
            AgentRoleId::RootOrchestrator | AgentRoleId::ChildOrchestrator
        ) {
            self.orchestrators.insert(name.to_owned());
        } else {
            self.orchestrators.remove(name);
        }
    }

    /// Ob der Rollenname eines Ereignisses ein Orchestrator ist.
    pub(crate) fn is_orchestrator(&self, role: &str) -> bool {
        let role = role.trim();
        if self.orchestrators.contains(role) {
            return true;
        }
        if self.known.contains(role) {
            return false;
        }
        // Notbehelf nur für Rollen ohne jede Definition.
        role.ends_with("-orchestrator")
    }
}

// ─── Handles für Ctrl+O ──────────────────────────────────────────────────────

/// Eine neu entstandene, aufklappbare Zelle eines Kind-Blocks; `app.rs`
/// hängt sie in seine Ctrl+O-Liste.
#[derive(Debug, Clone)]
pub(crate) enum ChildCellHandle {
    /// Einzelne Werkzeugzelle.
    Tool(SharedToolCell),
    /// Lese-Gruppe aufeinanderfolgender `fs.*`-Aufrufe.
    Group(Arc<Mutex<ToolGroupCell>>),
    /// Reasoning-Zelle.
    Reasoning(SharedReasoningCell),
}

// ─── Block ───────────────────────────────────────────────────────────────────

/// Ein Eintrag im Block eines Kindes.
#[derive(Debug)]
enum BlockEntry {
    /// Einzelne Werkzeugzelle.
    Tool(SharedToolCell),
    /// Lese-Gruppe.
    Group(Arc<Mutex<ToolGroupCell>>),
    /// Eingeklappte Reasoning-Zelle (`∴`).
    Reasoning(SharedReasoningCell),
    /// Assistententext (bereits redigiert).
    Assistant(String),
    /// Fehlerhinweis des Kindes (bereits redigiert).
    Notice(String),
    /// Ein Enkel: Zusammenfassungszeile, bei Orchestratoren (bzw. `all`) mit
    /// eigenem, weiter eingerücktem Block.
    Child {
        summary: Arc<Mutex<SubAgentCell>>,
        block: Option<SharedChildBlock>,
    },
}

/// Ergebnis der Anwendung eines Ereignisses auf einen Block.
#[derive(Debug, Default)]
struct ApplyOutcome {
    /// Neue, aufklappbare Zellen (Ctrl+O).
    handles: Vec<ChildCellHandle>,
    /// Neue verschachtelte Blöcke (Enkel), nach Session-Kennung.
    new_children: Vec<(String, SharedChildBlock)>,
}

/// Geteilter Live-Block eines Kindes.
pub(crate) type SharedChildBlock = Arc<Mutex<ChildStreamBlock>>;

/// Eingerückter Live-Block unter der Agent-Zeile eines Kindes.
///
/// # Beschreibung
/// Genau eine Instanz je Kind, als geteilte Verlaufszelle direkt hinter der
/// [`SubAgentCell`] eingehängt. Ereignisse des Kindes werden über
/// [`ChildStreamRegistry::apply`] als Deltas eingetragen; ohne Einträge
/// (oder bei abgeschaltetem Stream) rendert der Block keine Zeile.
#[derive(Debug)]
pub(crate) struct ChildStreamBlock {
    /// Session-Kennung des Kindes.
    child_id: String,
    /// Rollendefinition ist ein Orchestrator.
    orchestrator: bool,
    /// Verschachtelungstiefe (1 = Kind der Wurzel).
    depth: usize,
    /// Geteilter Modus (Darstellung wird bei jedem Rendern geprüft).
    mode: SharedMode,
    /// Sichtbare Einträge, älteste vorne.
    entries: VecDeque<BlockEntry>,
    /// Anzahl herausgefallener Einträge („… n frühere Schritte“).
    dropped: usize,
    /// Deckel: höchstens so viele Einträge.
    cap: usize,
    /// Offene Lese-Gruppe, solange erweiterbar.
    open_group: Option<Arc<Mutex<ToolGroupCell>>>,
    /// Offene Werkzeugaufrufe (`call_id` → Zelle) bis zum Abschluss.
    pending_tools: HashMap<ToolCallId, SharedToolCell>,
    /// Laufende Enkel (`child_id` → Zusammenfassungszeile).
    children: HashMap<String, Arc<Mutex<SubAgentCell>>>,
    /// Live gestreamter Text der laufenden Runde (Schwanz).
    live_text: String,
    /// Live gestreamtes Reasoning der laufenden Runde (Schwanz).
    live_reasoning: String,
    /// Das Kind ist beendet; weitere Ereignisse werden ignoriert.
    finished: bool,
}

impl ChildStreamBlock {
    fn new(child_id: &str, orchestrator: bool, depth: usize, mode: SharedMode, cap: usize) -> Self {
        Self {
            child_id: child_id.to_owned(),
            orchestrator,
            depth,
            mode,
            entries: VecDeque::new(),
            dropped: 0,
            cap: cap.max(1),
            open_group: None,
            pending_tools: HashMap::new(),
            children: HashMap::new(),
            live_text: String::new(),
            live_reasoning: String::new(),
            finished: false,
        }
    }

    /// Session-Kennung des Kindes.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn child_id(&self) -> &str {
        &self.child_id
    }

    /// Zahl der aktuell gehaltenen Einträge.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Zahl der herausgefallenen Einträge.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn dropped(&self) -> usize {
        self.dropped
    }

    /// Ob der Block im aktuellen Modus sichtbar ist.
    fn is_streaming(&self) -> bool {
        self.mode.get().admits(self.orchestrator)
    }

    /// Hängt einen Eintrag an und hält den Deckel ein.
    fn push_entry(&mut self, entry: BlockEntry) {
        self.entries.push_back(entry);
        while self.entries.len() > self.cap {
            let Some(old) = self.entries.pop_front() else {
                break;
            };
            self.dropped = self.dropped.saturating_add(1);
            let closes_open = match (&old, self.open_group.as_ref()) {
                (BlockEntry::Group(group), Some(open)) => Arc::ptr_eq(group, open),
                _ => false,
            };
            if closes_open {
                self.open_group = None;
            }
        }
    }

    /// Hängt eine Werkzeugzelle an bzw. gruppiert sie wie bei der Wurzel
    /// (`ChatApp::append_tool_cell`).
    fn append_tool(&mut self, tool_name: &str, cell: SharedToolCell, out: &mut ApplyOutcome) {
        if ToolGroupCell::accepts(tool_name) {
            if let Some(group) = self.open_group.clone() {
                if let Ok(mut guard) = group.lock() {
                    guard.push(cell);
                }
                return;
            }
            let group = Arc::new(Mutex::new(ToolGroupCell::new()));
            if let Ok(mut guard) = group.lock() {
                guard.push(cell);
            }
            out.handles.push(ChildCellHandle::Group(Arc::clone(&group)));
            self.push_entry(BlockEntry::Group(Arc::clone(&group)));
            self.open_group = Some(group);
        } else {
            self.open_group = None;
            out.handles.push(ChildCellHandle::Tool(Arc::clone(&cell)));
            self.push_entry(BlockEntry::Tool(cell));
        }
    }

    /// Schreibt die Zusammenfassungszeile eines laufenden Enkels fort.
    fn update_child<F>(&self, child_id: &str, update: F) -> bool
    where
        F: FnOnce(&mut SubAgentCell),
    {
        let Some(cell) = self.children.get(child_id) else {
            return false;
        };
        match cell.lock() {
            Ok(mut guard) => {
                update(&mut guard);
                true
            }
            Err(_) => false,
        }
    }

    /// Markiert das Kind als beendet (Live-Schwanz verworfen, offene
    /// Zuordnungen freigegeben).
    ///
    /// # Rückgabe
    /// `true`, wenn sich Sichtbares änderte (ein Live-Schwanz verschwand).
    fn finish(&mut self) -> bool {
        let had_live = !self.live_text.is_empty() || !self.live_reasoning.is_empty();
        tracing::debug!(child = %self.child_id, "tui.child_stream.finished");
        self.finished = true;
        self.live_text.clear();
        self.live_reasoning.clear();
        // Runde 5 (Integration I): noch laufende Werkzeugzellen des Kindes
        // nicht auf „läuft“ stehen lassen.
        let had_open_tools = !self.pending_tools.is_empty();
        for (_, cell) in self.pending_tools.drain() {
            if let Ok(mut guard) = cell.lock() {
                guard.mark_incomplete_with("unvollständig (Agent beendet)");
            }
        }
        self.children.clear();
        self.open_group = None;
        had_live || had_open_tools
    }

    /// Wendet ein Turn-Ereignis des Kindes als Delta an.
    ///
    /// # Rückgabe
    /// `true`, wenn sich Sichtbares geändert hat (Redraw nötig).
    fn apply(
        &mut self,
        event: &TurnEvent,
        roles: &OrchestratorRoles,
        out: &mut ApplyOutcome,
    ) -> bool {
        if self.finished {
            return false;
        }
        match event {
            TurnEvent::ToolCallRequested {
                call_id,
                tool_name,
                arguments,
                ..
            } => {
                self.live_text.clear();
                if self.pending_tools.contains_key(call_id) {
                    return false;
                }
                let call = ToolCall {
                    id: call_id.clone(),
                    name: ToolName::new(tool_name.clone()),
                    arguments: redact_json_value(arguments),
                };
                let cell: SharedToolCell = Arc::new(Mutex::new(ToolCell::started(&call)));
                self.pending_tools
                    .insert(call_id.clone(), Arc::clone(&cell));
                self.append_tool(tool_name, cell, out);
                true
            }
            TurnEvent::ToolCallCompleted {
                call_id,
                result,
                duration_ms,
                placement,
                ..
            } => {
                let Some(cell) = self.pending_tools.remove(call_id) else {
                    return false;
                };
                let redacted = redact_result(result);
                match cell.lock() {
                    Ok(mut guard) => {
                        // R18 D-D: Ort wie im Hauptverlauf zeigen.
                        guard.complete_at(&redacted, *duration_ms, placement.as_ref());
                        true
                    }
                    Err(_) => false,
                }
            }
            TurnEvent::ItemAdded {
                item: TurnItem::Reasoning(reasoning),
                ..
            } => {
                self.live_reasoning.clear();
                let summary = reasoning.summary_text.join(" ");
                if summary.trim().is_empty() {
                    return true;
                }
                self.open_group = None;
                let shared = ReasoningHistoryCell::new(redact_text(&summary)).into_shared();
                out.handles
                    .push(ChildCellHandle::Reasoning(Arc::clone(&shared)));
                self.push_entry(BlockEntry::Reasoning(shared));
                true
            }
            TurnEvent::ItemAdded {
                item: TurnItem::AssistantMessage(message),
                ..
            } => {
                self.live_text.clear();
                let text = visible_text(&message.content);
                if text.trim().is_empty() {
                    return true;
                }
                self.open_group = None;
                self.push_entry(BlockEntry::Assistant(redact_text(&text)));
                true
            }
            TurnEvent::ItemAdded {
                item: TurnItem::Error(error),
                ..
            } => {
                self.open_group = None;
                self.push_entry(BlockEntry::Notice(format!(
                    "⚠ {}",
                    redact_text(&error.message)
                )));
                true
            }
            TurnEvent::AssistantDelta { text, .. } => {
                push_tail(&mut self.live_text, text);
                true
            }
            TurnEvent::ReasoningDelta { text, .. } => {
                push_tail(&mut self.live_reasoning, text);
                true
            }
            TurnEvent::ChildSpawned {
                child,
                role,
                question,
                ..
            } => {
                self.open_group = None;
                let child_id = child.as_str().to_owned();
                if self.children.contains_key(&child_id) {
                    return false;
                }
                let summary = Arc::new(Mutex::new(SubAgentCell {
                    child_id: child_id.clone(),
                    role: role.clone(),
                    question: question.as_deref().map(redact_text),
                    tool_calls: 0,
                    tokens: 0,
                    status: SubAgentStatus::Running,
                }));
                self.children.insert(child_id.clone(), Arc::clone(&summary));
                let orchestrator = roles.is_orchestrator(role);
                let block =
                    (self.depth < MAX_DEPTH && self.mode.get().admits(orchestrator)).then(|| {
                        Arc::new(Mutex::new(ChildStreamBlock::new(
                            &child_id,
                            orchestrator,
                            self.depth + 1,
                            self.mode.clone(),
                            self.cap,
                        )))
                    });
                if let Some(nested) = &block {
                    out.new_children.push((child_id, Arc::clone(nested)));
                }
                self.push_entry(BlockEntry::Child { summary, block });
                true
            }
            TurnEvent::ChildProgress {
                child,
                tool_calls,
                tokens,
                ..
            } => self.update_child(child.as_str(), |cell| {
                cell.apply_progress(*tool_calls, *tokens);
            }),
            TurnEvent::ChildCompleted {
                child,
                outcome,
                duration_ms,
                ..
            } => {
                let updated = self.update_child(child.as_str(), |cell| {
                    cell.apply_completion(outcome.clone(), *duration_ms);
                });
                self.children.remove(child.as_str());
                updated
            }
            TurnEvent::TurnCompleted { .. }
            | TurnEvent::TurnFailed { .. }
            | TurnEvent::TurnAborted { .. } => {
                let had_live = !self.live_text.is_empty() || !self.live_reasoning.is_empty();
                self.live_text.clear();
                self.live_reasoning.clear();
                had_live
            }
            _ => false,
        }
    }
}

impl HistoryCell for ChildStreamBlock {
    /// Rendert den Block eingerückt (`"  │ "`): ggf. „… n frühere Schritte“,
    /// dann die Einträge (Werkzeug-, Reasoning-, Text-, Enkelzeilen) und
    /// zuletzt den Live-Schwanz. Abgeschaltet oder leer: keine Zeile.
    fn display_lines(&self, width: u16, theme: style::Theme) -> Vec<Line<'static>> {
        if !self.is_streaming() {
            return Vec::new();
        }
        let inner = width.saturating_sub(BLOCK_PREFIX_WIDTH).max(1);
        let dim = style::dim_style(theme);
        let mut body: Vec<Line<'static>> = Vec::new();
        if self.dropped > 0 {
            body.push(Line::from(Span::styled(
                format!("… {} frühere Schritte", self.dropped),
                dim,
            )));
        }
        for entry in &self.entries {
            body.extend(render_entry(entry, inner, theme));
        }
        if !self.live_reasoning.is_empty() {
            body.extend(live_lines(&self.live_reasoning, "∴ ", inner, dim));
        }
        if !self.live_text.is_empty() {
            body.extend(live_lines(&self.live_text, "● ", inner, dim));
        }
        body.into_iter()
            .map(|line| prefix_line(line, dim))
            .collect()
    }
}

/// Rendert einen Block-Eintrag in der inneren Breite.
fn render_entry(entry: &BlockEntry, width: u16, theme: style::Theme) -> Vec<Line<'static>> {
    match entry {
        BlockEntry::Tool(cell) => match cell.lock() {
            Ok(guard) => guard.display_lines_with(width, theme, ToolVerbosity::Compact),
            Err(_) => poisoned(theme),
        },
        BlockEntry::Group(group) => match group.lock() {
            Ok(guard) => guard.display_lines_with(width, theme, ToolVerbosity::Compact),
            Err(_) => poisoned(theme),
        },
        BlockEntry::Reasoning(cell) => cell.display_lines(width, theme),
        BlockEntry::Assistant(text) => render_assistant(text, width, theme),
        BlockEntry::Notice(text) => wrap_plain(&sanitize_inline(text), width)
            .into_iter()
            .map(|line| line.style(style::warning_style(theme)))
            .collect(),
        BlockEntry::Child { summary, block } => {
            let mut lines = match summary.lock() {
                Ok(guard) => guard.display_lines(width, theme),
                Err(_) => poisoned(theme),
            };
            if let Some(block) = block {
                match block.lock() {
                    Ok(guard) => lines.extend(guard.display_lines(width, theme)),
                    Err(_) => lines.extend(poisoned(theme)),
                }
            }
            lines
        }
    }
}

/// Assistententext wie bei der Wurzel (`» `, Markdown), aber auf
/// [`ASSISTANT_MAX_LINES`] Zeilen gekürzt.
fn render_assistant(text: &str, width: u16, theme: style::Theme) -> Vec<Line<'static>> {
    let cell = AssistantHistoryCell {
        source: text.to_owned(),
    };
    let mut lines = cell.display_lines(width, theme);
    if lines.len() > ASSISTANT_MAX_LINES {
        let hidden = lines.len() - ASSISTANT_MAX_LINES;
        lines.truncate(ASSISTANT_MAX_LINES);
        lines.push(Line::from(Span::styled(
            format!("  … ({hidden} weitere Zeilen)"),
            style::dim_style(theme),
        )));
    }
    lines
}

/// Letzte (bis zu) [`LIVE_MAX_LINES`] umbrochene Zeilen eines Live-Schwanzes.
fn live_lines(text: &str, marker: &str, width: u16, dim: Style) -> Vec<Line<'static>> {
    let sanitized = sanitize_inline(&redact_text(text));
    let lead = marker.chars().count();
    let lead_width = u16::try_from(lead).unwrap_or(u16::MAX);
    let wrapped = wrap_plain(sanitized.trim(), width.saturating_sub(lead_width).max(1));
    let skip = wrapped.len().saturating_sub(LIVE_MAX_LINES);
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
            let prefix = if index == 0 {
                marker.to_owned()
            } else {
                " ".repeat(lead)
            };
            Line::styled(format!("{prefix}{content}"), dim)
        })
        .collect()
}

/// Setzt den Block-Präfix vor eine Zeile.
fn prefix_line(line: Line<'static>, dim: Style) -> Line<'static> {
    let mut spans = Vec::with_capacity(line.spans.len() + 1);
    spans.push(Span::styled(BLOCK_PREFIX, dim));
    spans.extend(line.spans);
    Line::from(spans).style(line.style)
}

/// Ersatzzeile für eine vergiftete Sperre (kein Panic).
fn poisoned(theme: style::Theme) -> Vec<Line<'static>> {
    vec![Line::from(Span::styled(
        "⚠ Zelle nicht lesbar (Sperre vergiftet)".to_owned(),
        style::warning_style(theme),
    ))]
}

/// Hängt `text` an und kappt auf die letzten [`LIVE_MAX_CHARS`] Zeichen.
fn push_tail(buffer: &mut String, text: &str) {
    buffer.push_str(text);
    let count = buffer.chars().count();
    if count > LIVE_MAX_CHARS {
        *buffer = buffer.chars().skip(count - LIVE_MAX_CHARS).collect();
    }
}

/// Sichtbarer Text einer Assistentennachricht (Bilder als Platzhalter).
fn visible_text(content: &[ContentPart]) -> String {
    let mut visible = String::new();
    for part in content {
        match part {
            ContentPart::Text { text } => visible.push_str(text),
            ContentPart::ImageUrl { .. } | ContentPart::Media { .. } => visible.push_str("[Bild]"),
        }
    }
    visible
}

/// Redigiert ein Werkzeugergebnis wie der Export (auch für die Spur der
/// Agenten-Detailansicht, `agent_monitor.rs`).
pub(crate) fn redact_result(result: &ToolCallResult) -> ToolCallResult {
    match result {
        ToolCallResult::Success { value } => ToolCallResult::success(redact_json_value(value)),
        ToolCallResult::Error { message } => ToolCallResult::error(redact_text(message)),
    }
}

/// Ereignisse, die sich lohnen, bis zum Anlegen des Blocks vorzuhalten
/// (keine Deltas und reinen Zähler-Ereignisse).
fn is_bufferable(event: &TurnEvent) -> bool {
    !matches!(
        event,
        TurnEvent::AssistantDelta { .. }
            | TurnEvent::ReasoningDelta { .. }
            | TurnEvent::UsageUpdated { .. }
            | TurnEvent::ContextUpdated { .. }
            | TurnEvent::CompactionApplied { .. }
    )
}

// ─── Register ────────────────────────────────────────────────────────────────

/// Register aller Live-Blöcke einer [`crate::app::ChatApp`].
///
/// # Beschreibung
/// Ordnet Ereignisse des Agenten-Busses (`AgentEventKind::Turn`) über die
/// Absender-Session dem Block des Kindes zu. Blöcke direkter Kinder der
/// Wurzel legt [`Self::attach_root_child`] an (aufgerufen, sobald die
/// Agent-Zeile entsteht); Blöcke von Enkeln entstehen aus dem
/// `ChildSpawned`-Ereignis ihres Eltern-Blocks. Beim `ChildCompleted` eines
/// Kindes wird dessen Block abgeschlossen und aus der Zuordnung entfernt
/// (die Verlaufszelle bleibt bestehen).
#[derive(Debug)]
pub(crate) struct ChildStreamRegistry {
    /// Geteilter Modus.
    mode: SharedMode,
    /// Orchestrator-Einstufung.
    roles: OrchestratorRoles,
    /// Deckel je Block.
    cap: usize,
    /// Laufende Blöcke nach Session-Kennung.
    blocks: HashMap<String, SharedChildBlock>,
    /// Vorgehaltene Ereignisse noch nicht zuordenbarer Kinder (FIFO).
    pending: VecDeque<(String, VecDeque<TurnEvent>)>,
    /// Neue aufklappbare Zellen seit dem letzten [`Self::take_new_handles`].
    new_handles: Vec<ChildCellHandle>,
    /// Aufträge aus `ChildSpawned` (`child_id`, Auftrag), älteste vorne;
    /// gelesen von der Agentenbaum-Ansicht `/agent`.
    spawn_tasks: VecDeque<(String, String)>,
}

impl Default for ChildStreamRegistry {
    fn default() -> Self {
        Self::new(ChildStreamMode::default(), OrchestratorRoles::builtin())
    }
}

impl ChildStreamRegistry {
    /// Neues Register.
    ///
    /// # Argumente
    /// - `mode` ([`ChildStreamMode`]): Startmodus (`[tui] child_stream`).
    /// - `roles` ([`OrchestratorRoles`]): Orchestrator-Einstufung.
    pub(crate) fn new(mode: ChildStreamMode, roles: OrchestratorRoles) -> Self {
        Self {
            mode: SharedMode::new(mode),
            roles,
            cap: DEFAULT_VISIBLE_ENTRIES,
            blocks: HashMap::new(),
            pending: VecDeque::new(),
            new_handles: Vec::new(),
            spawn_tasks: VecDeque::new(),
        }
    }

    /// Setzt den Deckel (mindestens 1) für künftig angelegte Blöcke.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn with_cap(mut self, cap: usize) -> Self {
        self.cap = cap.max(1);
        self
    }

    /// Aktueller Modus.
    pub(crate) fn mode(&self) -> ChildStreamMode {
        self.mode.get()
    }

    /// Schaltet den Modus für die Sitzung um (wirkt sofort auf alle Blöcke).
    pub(crate) fn set_mode(&mut self, mode: ChildStreamMode) {
        self.mode.set(mode);
        if mode == ChildStreamMode::Off {
            self.pending.clear();
        }
    }

    /// Zahl der laufenden (zuordenbaren) Blöcke.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Legt den Block eines direkten Kindes der Wurzel an.
    ///
    /// # Beschreibung
    /// Aufgerufen, sobald die Agent-Zeile des Kindes im Verlauf steht; der
    /// Aufrufer hängt den zurückgegebenen Block als geteilte Zelle direkt
    /// dahinter. Vorgehaltene Ereignisse des Kindes werden nachgespielt.
    ///
    /// # Rückgabe
    /// `Some(block)`, wenn das Kind im aktuellen Modus streamt und noch
    /// keinen Block hat; sonst `None` (dann bleibt es bei der Agent-Zeile).
    pub(crate) fn attach_root_child(
        &mut self,
        child_id: &str,
        role: &str,
    ) -> Option<SharedChildBlock> {
        let orchestrator = self.roles.is_orchestrator(role);
        if !self.mode.get().admits(orchestrator) {
            self.drop_pending(child_id);
            return None;
        }
        if self.blocks.contains_key(child_id) {
            return None;
        }
        let block = Arc::new(Mutex::new(ChildStreamBlock::new(
            child_id,
            orchestrator,
            1,
            self.mode.clone(),
            self.cap,
        )));
        self.blocks.insert(child_id.to_owned(), Arc::clone(&block));
        self.flush_pending(child_id, &block);
        Some(block)
    }

    /// Wendet ein Turn-Ereignis des Agenten-Busses an.
    ///
    /// # Argumente
    /// - `agent` (`&SessionId`): Absender-Session.
    /// - `parent` (`Option<&SessionId>`): deren Eltern-Session (`None` = Wurzel).
    /// - `role` (`&str`): Rollenname des Absenders.
    /// - `event` (`&TurnEvent`): das Ereignis.
    ///
    /// # Rückgabe
    /// `true`, wenn sich ein sichtbarer Block geändert hat.
    pub(crate) fn apply(
        &mut self,
        agent: &SessionId,
        parent: Option<&SessionId>,
        role: &str,
        event: &TurnEvent,
    ) -> bool {
        let agent_id = agent.as_str();
        if let TurnEvent::ChildSpawned {
            child,
            question: Some(question),
            ..
        } = event
        {
            self.remember_task(child.as_str(), question);
        }
        let mut changed = match self.blocks.get(agent_id).cloned() {
            Some(block) => self.apply_to(&block, event),
            None => {
                if parent.is_some() {
                    self.buffer(agent_id, role, event);
                }
                false
            }
        };
        if let TurnEvent::ChildCompleted { child, .. } = event {
            if let Some(block) = self.blocks.remove(child.as_str())
                && let Ok(mut guard) = block.lock()
            {
                changed |= guard.finish();
            }
            self.drop_pending(child.as_str());
        }
        changed
    }

    /// Auftrag eines Kindes aus seinem `ChildSpawned`-Ereignis (bereits
    /// redigiert), falls noch gemerkt.
    pub(crate) fn task_for(&self, child_id: &str) -> Option<String> {
        self.spawn_tasks
            .iter()
            .rev()
            .find(|(id, _)| id == child_id)
            .map(|(_, task)| task.clone())
    }

    /// Merkt den Auftrag eines Kindes (begrenzt auf [`SPAWN_TASKS_MAX`]).
    pub(crate) fn remember_task(&mut self, child_id: &str, question: &str) {
        let question = question.trim();
        if question.is_empty() || self.task_for(child_id).is_some() {
            return;
        }
        self.spawn_tasks
            .push_back((child_id.to_owned(), redact_text(question)));
        while self.spawn_tasks.len() > SPAWN_TASKS_MAX {
            self.spawn_tasks.pop_front();
        }
    }

    /// Entnimmt die seit dem letzten Aufruf neu entstandenen, aufklappbaren
    /// Zellen (für die Ctrl+O-Liste von `app.rs`).
    pub(crate) fn take_new_handles(&mut self) -> Vec<ChildCellHandle> {
        std::mem::take(&mut self.new_handles)
    }

    /// Wendet ein Ereignis auf einen bekannten Block an und registriert
    /// dabei entstandene Enkel-Blöcke.
    fn apply_to(&mut self, block: &SharedChildBlock, event: &TurnEvent) -> bool {
        let mut out = ApplyOutcome::default();
        let changed = match block.lock() {
            Ok(mut guard) => guard.apply(event, &self.roles, &mut out),
            Err(_) => {
                tracing::error!("tui.child_stream.lock_poisoned");
                false
            }
        };
        self.new_handles.extend(out.handles);
        for (child_id, nested) in out.new_children {
            self.blocks.insert(child_id.clone(), Arc::clone(&nested));
            self.flush_pending(&child_id, &nested);
        }
        changed
    }

    /// Hält ein Ereignis eines (noch) unbekannten Kindes vor, sofern dessen
    /// Rolle im aktuellen Modus streamen würde.
    fn buffer(&mut self, agent_id: &str, role: &str, event: &TurnEvent) {
        if !is_bufferable(event) {
            return;
        }
        if !self.mode.get().admits(self.roles.is_orchestrator(role)) {
            return;
        }
        if let Some((_, events)) = self.pending.iter_mut().find(|(id, _)| id == agent_id) {
            events.push_back(event.clone());
            while events.len() > PENDING_MAX_EVENTS {
                events.pop_front();
            }
            return;
        }
        let mut events = VecDeque::new();
        events.push_back(event.clone());
        self.pending.push_back((agent_id.to_owned(), events));
        while self.pending.len() > PENDING_MAX_AGENTS {
            self.pending.pop_front();
        }
    }

    /// Spielt vorgehaltene Ereignisse in einen frisch angelegten Block nach.
    fn flush_pending(&mut self, child_id: &str, block: &SharedChildBlock) {
        let Some(position) = self.pending.iter().position(|(id, _)| id == child_id) else {
            return;
        };
        let Some((_, events)) = self.pending.remove(position) else {
            return;
        };
        for event in &events {
            self.apply_to(block, event);
        }
    }

    /// Verwirft vorgehaltene Ereignisse eines Kindes.
    fn drop_pending(&mut self, child_id: &str) {
        self.pending.retain(|(id, _)| id != child_id);
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use harw_protocol::items::{AssistantMessageItem, ReasoningItem};
    use harw_registry_defaults::profile::role_names;
    use harw_types::{ItemId, TurnId};
    use serde_json::json;

    const WIDTH: u16 = 120;

    fn text(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render(block: &SharedChildBlock) -> TestResult<Vec<Line<'static>>> {
        let guard = block
            .lock()
            .map_err(|_| TestError::Unexpected("Sperre vergiftet".into()))?;
        Ok(guard.display_lines(WIDTH, style::Theme::Dark))
    }

    fn tool_requested(call_id: &ToolCallId, name: &str, args: serde_json::Value) -> TurnEvent {
        TurnEvent::ToolCallRequested {
            turn_id: TurnId::new(),
            call_id: call_id.clone(),
            tool_name: name.to_owned(),
            arguments: args,
        }
    }

    fn tool_completed(call_id: &ToolCallId, result: ToolCallResult) -> TurnEvent {
        TurnEvent::ToolCallCompleted {
            turn_id: TurnId::new(),
            call_id: call_id.clone(),
            result,
            duration_ms: 7,
            placement: None,
        }
    }

    fn reasoning(summary: &str) -> TurnEvent {
        TurnEvent::ItemAdded {
            turn_id: TurnId::new(),
            item: TurnItem::Reasoning(ReasoningItem {
                id: ItemId::new(),
                summary_text: vec![summary.to_owned()],
                raw_content: Vec::new(),
            }),
        }
    }

    fn assistant(body: &str) -> TurnEvent {
        TurnEvent::ItemAdded {
            turn_id: TurnId::new(),
            item: TurnItem::AssistantMessage(AssistantMessageItem {
                id: ItemId::new(),
                content: vec![ContentPart::Text {
                    text: body.to_owned(),
                }],
                phase: None,
            }),
        }
    }

    fn spawned(child: &SessionId, role: &str) -> TurnEvent {
        TurnEvent::ChildSpawned {
            turn_id: TurnId::new(),
            child: child.clone(),
            role: role.to_owned(),
            question: Some("Teilauftrag".to_owned()),
        }
    }

    fn registry(mode: ChildStreamMode) -> ChildStreamRegistry {
        ChildStreamRegistry::new(mode, OrchestratorRoles::builtin())
    }

    /// Orchestrator-Erkennung über die Rollendefinition, nicht über eine
    /// feste Namensliste.
    #[test]
    fn orchestrators_are_recognised_by_definition() {
        let roles = OrchestratorRoles::builtin();
        assert!(roles.is_orchestrator(role_names::ROOT_ORCHESTRATOR));
        assert!(roles.is_orchestrator(role_names::CODING_ORCHESTRATOR));
        assert!(roles.is_orchestrator(role_names::RESEARCH_ORCHESTRATOR));
        assert!(!roles.is_orchestrator(role_names::EXPLORER));
        assert!(!roles.is_orchestrator(role_names::EXECUTOR));

        // Eine künftige Definition wird allein über ihre Rolle erkannt …
        let mut custom = OrchestratorRoles::default();
        custom.insert("review-lead", AgentRoleId::ChildOrchestrator);
        assert!(custom.is_orchestrator("review-lead"));
        // … und eine Definition schlägt die Namenskonvention.
        custom.insert("fake-orchestrator", AgentRoleId::Worker);
        assert!(!custom.is_orchestrator("fake-orchestrator"));
        // Nur ohne jede Definition greift der Notbehelf.
        assert!(custom.is_orchestrator("unknown-orchestrator"));
    }

    #[test]
    fn mode_parses_and_admits() {
        assert_eq!(ChildStreamMode::parse(" All "), Some(ChildStreamMode::All));
        assert_eq!(ChildStreamMode::parse("none"), Some(ChildStreamMode::Off));
        assert_eq!(
            ChildStreamMode::parse("orchestrators"),
            Some(ChildStreamMode::Orchestrators)
        );
        assert_eq!(ChildStreamMode::parse("workers"), None);
        assert!(ChildStreamMode::Orchestrators.admits(true));
        assert!(!ChildStreamMode::Orchestrators.admits(false));
        assert!(ChildStreamMode::All.admits(false));
        assert!(!ChildStreamMode::Off.admits(true));
        assert_eq!(
            ChildStreamMode::from(harw_config::ChildStreamModeToml::Off),
            ChildStreamMode::Off
        );
    }

    /// Werkzeug- und Reasoning-Ereignisse eines `root-orchestrator`-Kindes
    /// erzeugen eingerückte Zellen im Block; Ctrl+O-Handles entstehen.
    #[test]
    fn root_orchestrator_tool_and_reasoning_events_render_indented() -> TestResult {
        let mut reg = registry(ChildStreamMode::Orchestrators);
        let root = SessionId::from_str("root-uia");
        let orch = SessionId::from_str("child-root-orchestrator-1");
        let block = reg
            .attach_root_child(orch.as_str(), role_names::ROOT_ORCHESTRATOR)
            .ok_or(TestError::Missing("Orchestrator-Block"))?;
        assert!(render(&block)?.is_empty(), "leerer Block rendert nichts");
        {
            let guard = block
                .lock()
                .map_err(|_| TestError::Unexpected("Sperre vergiftet".into()))?;
            assert_eq!(guard.child_id(), orch.as_str());
        }

        let call = ToolCallId::new();
        assert!(reg.apply(
            &orch,
            Some(&root),
            role_names::ROOT_ORCHESTRATOR,
            &tool_requested(&call, "shell.exec", json!({"cmd": ["cargo", "--version"]}))
        ));
        assert!(reg.apply(
            &orch,
            Some(&root),
            role_names::ROOT_ORCHESTRATOR,
            &tool_completed(
                &call,
                ToolCallResult::success(
                    json!({"exit_code": 0, "stdout": "cargo 1.98.0", "stderr": ""})
                )
            )
        ));
        assert!(reg.apply(
            &orch,
            Some(&root),
            role_names::ROOT_ORCHESTRATOR,
            &reasoning("Ich zerlege den Auftrag in zwei Wellen.")
        ));
        assert!(reg.apply(
            &orch,
            Some(&root),
            role_names::ROOT_ORCHESTRATOR,
            &assistant("Welle 1 läuft.")
        ));

        let lines = render(&block)?;
        assert!(!lines.is_empty());
        for line in &lines {
            let first = line
                .spans
                .first()
                .map(|span| span.content.as_ref())
                .unwrap_or_default();
            assert_eq!(first, BLOCK_PREFIX, "jede Zeile ist eingerückt");
        }
        let rendered = text(&lines);
        assert!(rendered.contains("∴ "), "Reasoning mit ∴: {rendered}");
        assert!(rendered.contains("Welle 1 läuft."), "{rendered}");
        assert_eq!(reg.take_new_handles().len(), 2, "Werkzeug + Reasoning");
        Ok(())
    }

    /// Worker-Kinder der Wurzel bekommen im Vorgabemodus keinen Block, ihre
    /// Ereignisse erzeugen keine Zellen.
    #[test]
    fn worker_children_produce_no_cells() -> TestResult {
        let mut reg = registry(ChildStreamMode::Orchestrators);
        let root = SessionId::from_str("root-uia");
        let worker = SessionId::from_str("child-explorer-1");
        assert!(
            reg.attach_root_child(worker.as_str(), role_names::EXPLORER)
                .is_none()
        );
        let call = ToolCallId::new();
        assert!(!reg.apply(
            &worker,
            Some(&root),
            role_names::EXPLORER,
            &tool_requested(&call, "fs.read", json!({"path": "a.rs"}))
        ));
        assert!(reg.take_new_handles().is_empty());
        assert_eq!(reg.block_count(), 0);
        Ok(())
    }

    /// `none` legt keinen Block an und blendet bestehende sofort aus; `all`
    /// streamt auch Worker.
    #[test]
    fn none_and_all_modes_take_effect() -> TestResult {
        let root = SessionId::from_str("root-uia");

        let mut off = registry(ChildStreamMode::Off);
        assert!(
            off.attach_root_child("child-orch", role_names::ROOT_ORCHESTRATOR)
                .is_none()
        );

        let mut all = registry(ChildStreamMode::All);
        let worker = SessionId::from_str("child-explorer-2");
        let block = all
            .attach_root_child(worker.as_str(), role_names::EXPLORER)
            .ok_or(TestError::Missing("Worker-Block bei all"))?;
        let call = ToolCallId::new();
        assert!(all.apply(
            &worker,
            Some(&root),
            role_names::EXPLORER,
            &tool_requested(&call, "fs.read", json!({"path": "a.rs"}))
        ));
        assert!(!render(&block)?.is_empty());

        // Umschalten auf `none` blendet den bestehenden Block sofort aus …
        all.set_mode(ChildStreamMode::Off);
        assert!(render(&block)?.is_empty());
        // … `orchestrators` lässt einen Worker-Block ausgeblendet …
        all.set_mode(ChildStreamMode::Orchestrators);
        assert!(render(&block)?.is_empty());
        // … und `all` zeigt ihn wieder.
        all.set_mode(ChildStreamMode::All);
        assert!(!render(&block)?.is_empty());
        Ok(())
    }

    /// Der Deckel hält die Zahl der Einträge; der Rest wird gezählt.
    #[test]
    fn cap_limits_visible_entries() -> TestResult {
        let mut reg = registry(ChildStreamMode::Orchestrators).with_cap(3);
        let root = SessionId::from_str("root-uia");
        let orch = SessionId::from_str("child-orch-cap");
        let block = reg
            .attach_root_child(orch.as_str(), role_names::ROOT_ORCHESTRATOR)
            .ok_or(TestError::Missing("Block"))?;
        for index in 0..10 {
            let call = ToolCallId::new();
            reg.apply(
                &orch,
                Some(&root),
                role_names::ROOT_ORCHESTRATOR,
                &tool_requested(&call, "shell.exec", json!({"cmd": ["echo", index]})),
            );
        }
        {
            let guard = block
                .lock()
                .map_err(|_| TestError::Unexpected("Sperre vergiftet".into()))?;
            assert_eq!(guard.entry_count(), 3);
            assert_eq!(guard.dropped(), 7);
        }
        let rendered = text(&render(&block)?);
        assert!(rendered.contains("… 7 frühere Schritte"), "{rendered}");
        Ok(())
    }

    /// Enkel-Worker erscheinen im Block nur als Zusammenfassungszeile; ihre
    /// eigenen Werkzeugaufrufe erzeugen keine Zellen.
    #[test]
    fn grandchild_workers_are_summarised() -> TestResult {
        let mut reg = registry(ChildStreamMode::Orchestrators);
        let root = SessionId::from_str("root-uia");
        let orch = SessionId::from_str("child-orch-g");
        let worker = SessionId::from_str("grandchild-executor-1");
        let block = reg
            .attach_root_child(orch.as_str(), role_names::ROOT_ORCHESTRATOR)
            .ok_or(TestError::Missing("Block"))?;
        assert!(reg.apply(
            &orch,
            Some(&root),
            role_names::ROOT_ORCHESTRATOR,
            &spawned(&worker, role_names::EXECUTOR)
        ));
        assert!(reg.apply(
            &orch,
            Some(&root),
            role_names::ROOT_ORCHESTRATOR,
            &TurnEvent::ChildProgress {
                turn_id: TurnId::new(),
                child: worker.clone(),
                tool_calls: 4,
                tokens: 900,
            }
        ));
        let call = ToolCallId::new();
        assert!(!reg.apply(
            &worker,
            Some(&orch),
            role_names::EXECUTOR,
            &tool_requested(&call, "fs.write", json!({"path": "x.rs"}))
        ));
        let lines = render(&block)?;
        assert_eq!(lines.len(), 1, "genau eine Zusammenfassungszeile");
        let rendered = text(&lines);
        assert!(rendered.contains(role_names::EXECUTOR), "{rendered}");
        assert!(rendered.contains("4 Tools · 900 Tok"), "{rendered}");
        assert_eq!(reg.take_new_handles().len(), 0);
        Ok(())
    }

    /// Verschachtelung root → Sub-Orchestrator → Worker: der Sub-Orchestrator
    /// bekommt einen eigenen, weiter eingerückten Live-Block; sein Worker
    /// bleibt eine Zusammenfassungszeile.
    #[test]
    fn nested_sub_orchestrator_gets_its_own_block() -> TestResult {
        let mut reg = registry(ChildStreamMode::Orchestrators);
        let uia = SessionId::from_str("root-uia");
        let root_orch = SessionId::from_str("child-root-orch");
        let coding = SessionId::from_str("grandchild-coding-orch");
        let worker = SessionId::from_str("greatgrandchild-executor");
        let block = reg
            .attach_root_child(root_orch.as_str(), role_names::ROOT_ORCHESTRATOR)
            .ok_or(TestError::Missing("Block"))?;

        reg.apply(
            &root_orch,
            Some(&uia),
            role_names::ROOT_ORCHESTRATOR,
            &spawned(&coding, role_names::CODING_ORCHESTRATOR),
        );
        assert_eq!(reg.block_count(), 2, "Sub-Orchestrator-Block registriert");

        let call = ToolCallId::new();
        assert!(reg.apply(
            &coding,
            Some(&root_orch),
            role_names::CODING_ORCHESTRATOR,
            &tool_requested(&call, "shell.exec", json!({"cmd": ["git", "status"]}))
        ));
        assert!(reg.apply(
            &coding,
            Some(&root_orch),
            role_names::CODING_ORCHESTRATOR,
            &reasoning("Welle A an den executor.")
        ));
        reg.apply(
            &coding,
            Some(&root_orch),
            role_names::CODING_ORCHESTRATOR,
            &spawned(&worker, role_names::EXECUTOR),
        );
        assert_eq!(reg.block_count(), 2, "Worker bekommt keinen Block");
        let worker_call = ToolCallId::new();
        assert!(!reg.apply(
            &worker,
            Some(&coding),
            role_names::EXECUTOR,
            &tool_requested(&worker_call, "fs.write", json!({"path": "y.rs"}))
        ));

        let lines = render(&block)?;
        // Zeile 0: Agent-Zeile des Sub-Orchestrators (einfach eingerückt).
        // Danach seine Blockzeilen (doppelt eingerückt).
        let doubly = lines
            .iter()
            .filter(|line| {
                line.spans.len() >= 2
                    && line.spans[0].content.as_ref() == BLOCK_PREFIX
                    && line.spans[1].content.as_ref() == BLOCK_PREFIX
            })
            .count();
        assert!(doubly >= 3, "Werkzeug, Reasoning, Worker-Zeile: {doubly}");
        let rendered = text(&lines);
        assert!(
            rendered.contains(role_names::CODING_ORCHESTRATOR),
            "{rendered}"
        );
        assert!(rendered.contains("∴ "), "{rendered}");
        assert!(rendered.contains(role_names::EXECUTOR), "{rendered}");
        assert!(
            !rendered.contains("y.rs"),
            "Worker-Werkzeug bleibt verborgen"
        );
        assert_eq!(reg.take_new_handles().len(), 2);

        // Abschluss des Sub-Orchestrators entfernt dessen Zuordnung.
        reg.apply(
            &root_orch,
            Some(&uia),
            role_names::ROOT_ORCHESTRATOR,
            &TurnEvent::ChildCompleted {
                turn_id: TurnId::new(),
                child: coding.clone(),
                outcome: "completed".to_owned(),
                duration_ms: 10,
            },
        );
        assert_eq!(reg.block_count(), 1);
        Ok(())
    }

    /// Kein Neuaufbau: ein Abschluss schreibt dieselbe Werkzeugzelle fort,
    /// neue Ereignisse hängen nur an; vorhandene Einträge bleiben dieselben
    /// Instanzen.
    #[test]
    fn events_append_deltas_without_rebuild() -> TestResult {
        let mut reg = registry(ChildStreamMode::Orchestrators);
        let root = SessionId::from_str("root-uia");
        let orch = SessionId::from_str("child-orch-delta");
        let block = reg
            .attach_root_child(orch.as_str(), role_names::ROOT_ORCHESTRATOR)
            .ok_or(TestError::Missing("Block"))?;
        let call = ToolCallId::new();
        reg.apply(
            &orch,
            Some(&root),
            role_names::ROOT_ORCHESTRATOR,
            &tool_requested(&call, "shell.exec", json!({"cmd": ["ls"]})),
        );
        let first = match reg.take_new_handles().pop() {
            Some(ChildCellHandle::Tool(cell)) => cell,
            other => {
                return Err(TestError::Unexpected(format!(
                    "Werkzeug-Handle erwartet: {other:?}"
                )));
            }
        };
        reg.apply(
            &orch,
            Some(&root),
            role_names::ROOT_ORCHESTRATOR,
            &tool_completed(&call, ToolCallResult::error("nicht erlaubt")),
        );
        reg.apply(
            &orch,
            Some(&root),
            role_names::ROOT_ORCHESTRATOR,
            &TurnEvent::AssistantDelta {
                turn_id: TurnId::new(),
                text: "Zwischen".to_owned(),
            },
        );
        reg.apply(
            &orch,
            Some(&root),
            role_names::ROOT_ORCHESTRATOR,
            &TurnEvent::AssistantDelta {
                turn_id: TurnId::new(),
                text: "stand".to_owned(),
            },
        );
        let guard = block
            .lock()
            .map_err(|_| TestError::Unexpected("Sperre vergiftet".into()))?;
        assert_eq!(guard.entry_count(), 1, "Abschluss legt keinen Eintrag an");
        match guard.entries.front() {
            Some(BlockEntry::Tool(cell)) => {
                assert!(Arc::ptr_eq(cell, &first), "dieselbe Zellinstanz");
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "Werkzeugeintrag erwartet: {other:?}"
                )));
            }
        }
        assert_eq!(guard.live_text, "Zwischenstand", "Deltas angehängt");
        let rendered = text(&guard.display_lines(WIDTH, style::Theme::Dark));
        assert!(rendered.contains("● Zwischenstand"), "{rendered}");
        Ok(())
    }

    /// Der Auftrag aus `ChildSpawned` bleibt für `/agent` abrufbar, auch für
    /// Kinder ohne Live-Block.
    #[test]
    fn spawn_task_is_remembered_for_agent_view() {
        let mut reg = registry(ChildStreamMode::Off);
        let root = SessionId::from_str("root-uia");
        let child = SessionId::from_str("child-explorer-t");
        reg.apply(
            &root,
            None,
            "assistant-ui",
            &spawned(&child, role_names::EXPLORER),
        );
        assert_eq!(reg.task_for(child.as_str()).as_deref(), Some("Teilauftrag"));
        assert_eq!(reg.task_for("unbekannt"), None);
    }

    /// Ereignisse, die vor der Agent-Zeile eintreffen, werden nachgespielt;
    /// Geheimnisse in Argumenten werden redigiert.
    #[test]
    fn early_events_are_replayed_and_secrets_redacted() -> TestResult {
        let mut reg = registry(ChildStreamMode::Orchestrators);
        let root = SessionId::from_str("root-uia");
        let orch = SessionId::from_str("child-orch-early");
        let call = ToolCallId::new();
        assert!(!reg.apply(
            &orch,
            Some(&root),
            role_names::ROOT_ORCHESTRATOR,
            &tool_requested(
                &call,
                "web.fetch",
                json!({"url": "https://example.test", "api_key": "sk-geheim-123"})
            )
        ));
        let block = reg
            .attach_root_child(orch.as_str(), role_names::ROOT_ORCHESTRATOR)
            .ok_or(TestError::Missing("Block"))?;
        let guard = block
            .lock()
            .map_err(|_| TestError::Unexpected("Sperre vergiftet".into()))?;
        assert_eq!(
            guard.entry_count(),
            1,
            "vorgehaltenes Ereignis nachgespielt"
        );
        let rendered = format!(
            "{:?}",
            guard.entries.front().ok_or(TestError::Missing("Eintrag"))?
        );
        assert!(!rendered.contains("sk-geheim-123"), "{rendered}");
        Ok(())
    }
}
