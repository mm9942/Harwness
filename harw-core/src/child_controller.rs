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
//!
//! # Lebenszyklus und Slot-Freigabe (A-CHILD: F-072, G-016, F-182, F-183)
//! - [`ManagedAgentSpawner::release_child`] gibt den Admission-Slot frei,
//!   bricht den Kind-Token (und damit alle Nachkommen) ab, entfernt die
//!   Kind-Session aus dem [`SessionManager`] und schließt einen durablen Lease.
//! - [`ChildGuard`] ist die RAII-Form davon: `Drop` gibt den Slot frei, auch bei
//!   Early-Return (`?`) oder Panic.
//! - Jedes Kind trägt einen [`CancelToken`], abgeleitet über
//!   [`CancelToken::child`] aus dem Token seines Elternteils. Ein Eltern-Abbruch
//!   bricht so jeden laufenden Kind-Turn ab. Cancel ist **terminal**: ein
//!   abgebrochenes Kind wird nicht wieder ausgeführt, sondern vom Reaper
//!   freigegeben.
//! - [`ManagedAgentSpawner::reap`] (periodisch) und die Admission (wenn der
//!   Eltern-Deckel erreicht ist) räumen verwaiste, abgebrochene und
//!   abgelaufene Kinder ab.
//! - [`ChildStatus::Completed`] wird nur bei einem echten
//!   `TurnOutcome::Completed` innerhalb des Budgets gesetzt, nie bei Fehler,
//!   Budget-Verletzung oder Abbruch.
//!
//! # Rollenerkennung für nicht mehr blockierende Delegation
//! [`ManagedAgentSpawner::parent_organizational_role`] liest die
//! organisatorische Rolle (§3 DSL-Spawn-Matrix) der Elternsitzung eines
//! admittierten Kindes, ohne die interne Admission-Prüfung zu duplizieren.
//! Aufrufer außerhalb dieser Crate (z. B. eine Approval-Schicht) nutzen das, um eine
//! `UserInterface`-Delegation zu erkennen und stattdessen die nicht
//! blockierende Warteschlangen-Admission
//! (`JobAdmissionService::submit_queued_async` in `harw-core::admission`) zu
//! wählen, statt bei Kapazitätsdruck hart abzulehnen.
//!
//! Für einen Fan-out innerhalb **dieses** Controllers (z. B. mehrere parallele
//! `explore`-Kind-Aufrufe desselben Elternteils, deren Anzahl
//! `max_active_children_per_parent` überschreiten kann) gibt es dasselbe
//! Muster lokal: [`ManagedAgentSpawner::admit_or_wait`] /
//! [`ManagedAgentSpawner::spawn_child_or_wait`] wiederholen nur eine
//! [`AdmitRejection::Capacity`]-Ablehnung, gewickelt in `Notify` statt einer
//! separaten Warteschlange — [`ManagedAgentSpawner::admit`] selbst lehnt
//! Kapazitätsdruck unverändert sofort ab.
//!
//! # Nebenläufigkeit
//! `ManagedAgentSpawner` ist `Send + Sync`. Alle Sperren sind `std::sync::Mutex`
//! und werden **nie** über ein `.await` gehalten. [`ManagedAgentSpawner::admit`]
//! hält `active` und `cancellations` durchgehend von der Deckel-Prüfung bis
//! zum Eintrag des neuen Kindes — auch über Registry-Montage,
//! Capability-Snapshot und Lease-Datei-I/O hinweg. Ein früherer, separater
//! `reserved`-Zähler samt `SlotReservation`-Guard, der genau dieses Fenster
//! ohne durchgehende Sperre hätte abdecken sollen, ist deshalb entfallen
//! (siehe [`ManagedAgentSpawner::admit`]s eigene Prüfung): eine zweite
//! Admission desselben Elternteils kann unter der durchgehend gehaltenen
//! Sperre gar nicht erst in die Deckel-Prüfung eintreten, solange die erste
//! noch läuft. Verschachtelt wird nur noch ein Paar, immer in dieser
//! Richtung: `manager` ⊃ `released` (Rückgabe bzw. Verwerfen einer laufenden
//! Session). Die Auftrags- und Fortschritts-Registries (`child_tasks`,
//! `progress_sinks`) sind Blatt-Sperren: unter ihnen wird nie eine weitere
//! Sperre genommen und nie ein Beobachter aufgerufen.
//!
//! # Auftrag, Ergebnis und Live-Fortschritt
//! - Die Admission hinterlegt `SpawnInput::instructions` (sonst einen nicht
//!   leeren `context`) als einmaligen Auftrag; der erste Lauf mit leerem
//!   [`TurnInput`] bekommt ihn als User-Turn.
//! - Orchestrierungs-Events tragen in `task` den Kurzkopf dieses Auftrags und
//!   bei `Completed`/`Failed` in `detail` den Kurzkopf der finalen Antwort
//!   bzw. des Fehlergrunds (siehe [`orchestration_detail_head`]).
//! - [`ChildRunResult::full_text`] trägt die ungekürzte finale Antwort.
//! - Der [`ManagedAgentSpawner::progress_observer`] sendet gedrosselt
//!   `TurnEvent::ChildProgress` an den Live-Kanal des Elternteils (siehe
//!   [`ManagedAgentSpawner::attach_child_progress_sink`]).
//!
//! # Fehler
//! Alle öffentlichen Fehler sind [`AgentSpawnError`] mit lesbarer Meldung.
//!
//! # Examples
//! ```rust,no_run
//! use harw_core::child_controller::ManagedAgentSpawner;
//! # fn demo(spawner: &ManagedAgentSpawner, child: harw_types::SessionId) {
//! let guard = spawner.guard_child(child);
//! // … Kind ausführen und Ergebnis auswerten …
//! drop(guard); // Slot, Token und Session werden freigegeben
//! # }
//! ```

use crate::ModelProvider;
use crate::activation::SessionActivation;
use crate::cancel::{CancelReason, CancelToken};
use crate::session::{AgentSession, LiveEmitter, SpawnContext};
use crate::session_manager::SessionManager;
use crate::state_store::StateStore;
use crate::turn_loop::{TurnInput, TurnOutcome, run_turn, run_turn_durable};
use harw_agent_dsl::executable::{BudgetSpec, ContextProgram, ExecutableAgentIr, SectionDetail};
use harw_authority::SandboxSpec;
use harw_catalog::{AgentSuggestions, SpawnCapabilitySnapshot};
use harw_context::{ContextBudgetSpec, ContextCeiling, SectionName, TrustClass};
use harw_extension_api::{
    AgentSpawnError, AgentSpawner, ExtensionRegistry, SpawnFuture, SpawnInput,
};
use harw_observe::TraceContext;
use harw_protocol::items::{ContentPart, TurnItem};
use harw_protocol::{AgentOrchestrationEvent, AgentOrchestrationStatus, TurnEvent};
use harw_session_store::{ApprovalStore, ChildLeaseRecord, ChildLeaseStore};
use harw_types::{AgentRole, ReasoningEffort, SessionId, TokenUsage, ToolCallId, TurnId};
use jiff::{SignedDuration, Timestamp};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;
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

/// Löst den Kind-Default-Reasoning-Effort nach der Nutzerentscheidung-Rangfolge
/// **Provider > Modell > Agent > Rolle** auf (Welle 8,
/// `recursive-cooking-lobster.md`).
///
/// # Beschreibung
/// Spiegelt absichtlich dieselbe Rangfolgen-Logik wie
/// `harw_runtime::guard_wiring::resolve_default_reasoning_effort` — dieses
/// Crate (`harw-core`) darf nicht von `harw-runtime` abhängen (Schichtung:
/// `harw-runtime` hängt von `harw-core` ab, nie umgekehrt), deshalb lebt hier
/// eine zweite, bewusst identische Implementierung statt eines Imports.
/// **Wer die Rangfolge ändert, muss beide Stellen nachziehen.**
///
/// Anders als die `harw-runtime`-Fassung nimmt `role_default` hier keinen
/// `Option`, weil [`RoleEffortWeights::for_child`] immer einen konkreten
/// Wert liefert — der Boden dieser Rangfolge ist an dieser Anwendungsstelle
/// nie unbestimmt.
///
/// # Arguments
/// - `provider_default` (`Option<ReasoningEffort>`): Provider-Standard des
///   für diese Rolle tatsächlich aufgelösten Providers, aus
///   [`ChildRegistryFactory::reasoning_effort_defaults_for_role`].
/// - `model_default` (`Option<ReasoningEffort>`): Modell-Standard desselben
///   aufgelösten Modells, aus derselben Quelle.
/// - `agent_default` (`Option<&str>`): [`harw_agent_dsl::executable::ExecutableAgentIr::reasoning_effort`]
///   der Rolle — ein undurchsichtiges DSL-Label. Ein Label, das nicht als
///   [`ReasoningEffort`] geparst werden kann, wird `tracing::warn!`-gemeldet
///   und übersprungen (fällt zur Rollen-Ebene durch), statt die Admission
///   abzubrechen.
/// - `role_default` (`ReasoningEffort`): das Rollengewicht aus
///   [`RoleEffortWeights::for_child`] — der Boden dieser Rangfolge.
///
/// # Returns
/// Das nach der Rangfolge gewinnende [`ReasoningEffort`]-Level, **vor** der
/// Klammerung gegen das geerbte Eltern-Level (die bleibt Sache des Aufrufers,
/// siehe [`ManagedAgentSpawner::admit`]).
///
/// # Concurrency
/// Rein; von jedem Thread aus sicher.
fn resolve_child_default_reasoning_effort(
    provider_default: Option<ReasoningEffort>,
    model_default: Option<ReasoningEffort>,
    agent_default: Option<&str>,
    role_default: ReasoningEffort,
) -> ReasoningEffort {
    if let Some(effort) = provider_default {
        return effort;
    }
    if let Some(effort) = model_default {
        return effort;
    }
    if let Some(label) = agent_default {
        match label.parse::<ReasoningEffort>() {
            Ok(effort) => return effort,
            Err(error) => {
                tracing::warn!(
                    label,
                    error = %error,
                    "core.child_admit.reasoning_default.invalid_agent_label"
                );
                // Fällt bewusst zur Rollen-Ebene durch, statt abzubrechen.
            }
        }
    }
    role_default
}

/// Obergrenze (in Bytes) für den Text, den [`ManagedAgentSpawner::child_final_assistant_text`]
/// an den Elternteil zurückgibt.
///
/// # Beschreibung
/// Verhindert, dass ein Kind mit einer sehr langen finalen Antwort den
/// Kontext des Elternteils sprengt. Die Kürzung geschieht ausschließlich auf
/// dem Rückgabepfad — eine eventuell vorhandene vollständige Persistenz
/// (z. B. Transcript/State-Store des Kindes) bleibt davon unberührt, weil
/// diese Konstante nur von [`cap_child_return_text`] konsumiert wird.
pub const CHILD_RETURN_MAX_BYTES: usize = 8 * 1024;

/// Kürzt einen Text auf höchstens `max_bytes`, ohne einen UTF-8-Zeichen zu zerschneiden.
///
/// # Beschreibung
/// Ist `text.len() <= max_bytes`, wird `text` unverändert zurückgegeben.
/// Andernfalls werden ein Kopf (~3/4 des Budgets) und ein Ende (~1/4 des
/// Budgets) behalten, getrennt durch eine Markierung `\n[… {n} Bytes der
/// Kind-Antwort gekürzt …]\n`, wobei `{n}` die Anzahl der weggelassenen
/// Bytes ist. Kopf und Ende werden jeweils auf die nächstliegende gültige
/// UTF-8-Zeichengrenze zurückgeschnitten, damit niemals ein Mehrbyte-Zeichen
/// mittendrin geteilt wird.
///
/// # Argumente
/// - `text` (`&str`): der ungekürzte Text.
/// - `max_bytes` (`usize`): das Byte-Budget für Kopf + Ende (ohne die
///   Markierung selbst).
///
/// # Rückgabe
/// Der unveränderte Text, oder ein gekürzter Text aus Kopf + Markierung +
/// Ende, dessen Gesamtlänge `max_bytes` um die Länge der Markierung
/// überschreiten kann (die Markierung selbst zählt nicht gegen `max_bytes`).
///
/// # Beispiele
/// ```rust
/// use harw_core::child_controller::cap_child_return_text;
///
/// assert_eq!(cap_child_return_text("kurz", 100), "kurz");
/// let long = "a".repeat(200);
/// let capped = cap_child_return_text(&long, 100);
/// assert!(capped.len() <= 100 + 64);
/// assert!(capped.contains("gekürzt"));
/// ```
#[must_use]
pub fn cap_child_return_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }

    let head_budget = max_bytes * 3 / 4;
    let tail_budget = max_bytes - head_budget;

    let head_end = floor_char_boundary(text, head_budget);
    let tail_start_min = text.len().saturating_sub(tail_budget);
    let tail_start = ceil_char_boundary(text, tail_start_min);
    // Kopf und Ende dürfen sich nicht überlappen; bei sehr kleinen Budgets
    // (oder sehr großen UTF-8-Zeichen an der Grenze) wird das Ende notfalls
    // hinter das Kopfende gezogen.
    let tail_start = tail_start.max(head_end);

    let omitted = text.len().saturating_sub(head_end) - (text.len() - tail_start);
    let marker = format!("\n[… {omitted} Bytes der Kind-Antwort gekürzt …]\n");

    tracing::debug!(
        original_bytes = text.len(),
        max_bytes,
        omitted_bytes = omitted,
        "child_final_assistant_text.truncated"
    );

    let mut capped = String::with_capacity(head_end + marker.len() + (text.len() - tail_start));
    capped.push_str(&text[..head_end]);
    capped.push_str(&marker);
    capped.push_str(&text[tail_start..]);
    capped
}

/// Größte Byte-Position `<= idx`, die auf einer UTF-8-Zeichengrenze von `s` liegt.
fn floor_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut i = idx;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Kleinste Byte-Position `>= idx`, die auf einer UTF-8-Zeichengrenze von `s` liegt.
fn ceil_char_boundary(s: &str, idx: usize) -> usize {
    if idx >= s.len() {
        return s.len();
    }
    let mut i = idx;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Obergrenze (in Zeichen, nicht Bytes) für `task` und `detail` eines vom
/// Controller erzeugten [`AgentOrchestrationEvent`].
///
/// # Beschreibung
/// Liegt bewusst unter [`AgentOrchestrationEvent::MAX_DETAIL_CHARS`], damit
/// das Protokoll-seitige Kürzen nie ein bereits gesetztes Kürzungszeichen
/// abschneidet. Siehe [`orchestration_detail_head`].
pub const ORCHESTRATION_DETAIL_MAX_CHARS: usize = 500;

/// Mindestabstand zwischen zwei [`TurnEvent::ChildProgress`]-Meldungen
/// desselben Kindes (Drosselung, siehe
/// [`ManagedAgentSpawner::attach_child_progress_sink`]).
pub const CHILD_PROGRESS_MIN_INTERVAL: Duration = Duration::from_millis(500);

/// Liefert einen kurzen, zeichengrenzen-sicheren Kopf eines Textes für
/// Orchestrierungs-Events (`task`/`detail`).
///
/// # Beschreibung
/// Entfernt führenden und abschließenden Leerraum. Ist der Rest länger als
/// [`ORCHESTRATION_DETAIL_MAX_CHARS`] Zeichen, werden die ersten
/// `ORCHESTRATION_DETAIL_MAX_CHARS - 1` Zeichen behalten und ein `…`
/// angehängt — das Ergebnis hat damit höchstens
/// [`ORCHESTRATION_DETAIL_MAX_CHARS`] Zeichen. Gezählt wird in `char`s, ein
/// Mehrbyte-Zeichen wird also nie zerschnitten.
///
/// # Arguments
/// - `text` (`&str`): der ungekürzte Text.
///
/// # Returns
/// `None` für einen leeren bzw. reinen Leerraum-Text, sonst den Kopf.
///
/// # Examples
/// ```rust
/// use harw_core::child_controller::{ORCHESTRATION_DETAIL_MAX_CHARS, orchestration_detail_head};
///
/// assert_eq!(orchestration_detail_head("  kurz \n"), Some("kurz".to_owned()));
/// assert_eq!(orchestration_detail_head("   "), None);
/// let long = "ä".repeat(2_000);
/// let head = orchestration_detail_head(&long).unwrap_or_default();
/// assert_eq!(head.chars().count(), ORCHESTRATION_DETAIL_MAX_CHARS);
/// assert!(head.ends_with('…'));
/// ```
#[must_use]
pub fn orchestration_detail_head(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().count() <= ORCHESTRATION_DETAIL_MAX_CHARS {
        return Some(trimmed.to_owned());
    }
    let mut head: String = trimmed
        .chars()
        .take(ORCHESTRATION_DETAIL_MAX_CHARS.saturating_sub(1))
        .collect();
    head.push('…');
    Some(head)
}

/// Leitet den Auftragstext eines Kindes aus seinem [`SpawnInput`] ab.
///
/// # Beschreibung
/// Vorrang hat `instructions` (sofern nicht leer). Sonst wird ein nicht
/// leerer `context` verwendet: ein JSON-String direkt, jeder andere Wert
/// (außer `null`, `{}` und `[]`) als kompaktes JSON.
///
/// # Returns
/// Den **ungekürzten** Auftragstext oder `None`, wenn keiner vorliegt.
fn spawn_task_text(instructions: Option<&str>, context: &serde_json::Value) -> Option<String> {
    if let Some(text) = instructions
        && !text.trim().is_empty()
    {
        return Some(text.to_owned());
    }
    match context {
        serde_json::Value::Null => None,
        serde_json::Value::String(text) if text.trim().is_empty() => None,
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Object(map) if map.is_empty() => None,
        serde_json::Value::Array(items) if items.is_empty() => None,
        other => Some(other.to_string()),
    }
}

/// Auftrags- und Ergebniszustand eines admittierten Kindes, der nicht im
/// öffentlichen [`ChildRecord`] liegt (dessen Literal-Konstruktion in anderen
/// Crates sonst bräche).
#[derive(Debug, Clone, Default)]
struct ChildTaskState {
    /// Der ungekürzte Auftrag; wird vom ersten Lauf mit leerem
    /// [`TurnInput`] als User-Text verbraucht.
    pending_task: Option<String>,
    /// Kurzkopf des Auftrags für `AgentOrchestrationEvent::task`.
    task: Option<String>,
    /// Kurzkopf der finalen Antwort bzw. des Fehlergrunds für
    /// `AgentOrchestrationEvent::detail` (`Completed`/`Failed`).
    outcome_detail: Option<String>,
}

/// Ziel der [`TurnEvent::ChildProgress`]-Meldungen eines Kindes: der
/// Live-Kanal seines Elternteils samt dessen Turn-ID.
#[derive(Debug, Clone)]
struct ChildProgressSink {
    turn_id: TurnId,
    emitter: LiveEmitter,
    /// Zeitpunkt der letzten gesendeten Meldung (Drosselung).
    last_emitted: Option<std::time::Instant>,
}

/// Geteilte Registry der Fortschritts-Senken je Kind (Schlüssel: Kind-ID).
type ProgressSinks = Arc<Mutex<BTreeMap<String, ChildProgressSink>>>;

/// Ein bereits geboxtes Kind-Future im Fan-out-Scheduler.
///
/// `Send` bleibt bewusst gefordert: sonst wäre das Future von
/// [`ManagedAgentSpawner::run_children`] selbst nicht mehr `Send` und ließe
/// sich nicht mit `tokio::spawn` in einen Task legen.
type ChildRunFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ChildRunResult, AgentSpawnError>> + Send + 'a>>;

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
    /// Der aktuelle Lebenszyklus-Status des Kindes (A-CHILD). Neu admittierte
    /// Kinder starten mit [`ChildStatus::Admitted`]; nur der Controller
    /// schreibt diesen Wert fort. Er fließt **nicht** in den durablen Lease ein.
    pub status: ChildStatus,
    /// Bei der Admission aus `SpawnInput::context` gelesene
    /// Aufgabenkomplexität (Addendum D,
    /// [`TaskComplexity::from_spawn_context`]). `None`, wenn der Aufrufer
    /// keine Einstufung mitgegeben hat. Wird beim Kind-Start an
    /// [`ChildRegistryFactory::model_for_task`] weitergereicht.
    pub task_complexity: Option<TaskComplexity>,
    /// Live-Zähler (Tokens, Tool-Aufrufe) für Beobachter.
    pub live: ChildLiveStats,
    /// Das Modell, das dieses Kind anspricht: das bei der Admission gepinnte
    /// Kind-Modell, sonst das `active_model` der Kind-Session. `None`, wenn
    /// keines von beiden bekannt ist (dann gilt der Provider-Default).
    pub model: Option<String>,
    /// Bisher abgerechneter Verbrauch dieses Kindes **samt Nachkommen**.
    ///
    /// Das Kind selbst trägt am Ende jedes
    /// [`ManagedAgentSpawner::run_child_with_budget`] den Verbrauch des Laufs
    /// ein; Nachkommen werden beim Abschluss ihres Laufs bzw. bei ihrer
    /// Freigabe (`close_child`) jeweils eine Ebene nach oben verrechnet.
    /// Grundlage für [`ManagedAgentSpawner::remaining_budget`].
    pub consumed: ChildUsage,
    /// Der Teil von [`Self::consumed`], der dem direkten Elternteil bereits
    /// angerechnet wurde. Weitere Anrechnungen übertragen nur die Differenz,
    /// damit Lauf-Ende und Freigabe nichts doppelt verbuchen.
    pub charged_to_parent: ChildUsage,
}

/// Verbrauch eines Kindes in den drei durchgesetzten Budget-Dimensionen.
///
/// # Beschreibung
/// In [`ChildRunResult::usage`] der Verbrauch **eines** Laufs, in
/// [`ChildRecord::consumed`] die aufsummierte Anrechnung (Kind plus
/// Nachkommen). Alle Rechnungen sättigen, statt überzulaufen.
///
/// # Concurrency
/// `Copy`; enthält keinen geteilten Zustand.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChildUsage {
    /// Verbrauchte Modell-Token.
    pub tokens: u64,
    /// Ausgeführte Werkzeugaufrufe.
    pub tool_calls: u32,
    /// Verbrauchte Wanduhrzeit in Millisekunden.
    pub wall_time_ms: u64,
}

impl ChildUsage {
    /// Sättigende, dimensionsweise Summe.
    #[must_use]
    pub fn saturating_add(self, other: Self) -> Self {
        Self {
            tokens: self.tokens.saturating_add(other.tokens),
            tool_calls: self.tool_calls.saturating_add(other.tool_calls),
            wall_time_ms: self.wall_time_ms.saturating_add(other.wall_time_ms),
        }
    }

    /// Sättigende, dimensionsweise Differenz (nie unter 0).
    #[must_use]
    pub fn saturating_sub(self, other: Self) -> Self {
        Self {
            tokens: self.tokens.saturating_sub(other.tokens),
            tool_calls: self.tool_calls.saturating_sub(other.tool_calls),
            wall_time_ms: self.wall_time_ms.saturating_sub(other.wall_time_ms),
        }
    }
}

/// Zieht den Verbrauch dimensionsweise von einem Budget ab.
///
/// Eine nicht gesetzte Dimension bleibt nicht gesetzt (kein Deckel); eine
/// gesetzte sättigt bei 0. `reasoning_effort` bleibt unverändert.
fn budget_minus_usage(budget: AgentBudget, usage: ChildUsage) -> AgentBudget {
    AgentBudget {
        max_tokens: budget
            .max_tokens
            .map(|limit| limit.saturating_sub(usage.tokens)),
        max_tool_calls: budget
            .max_tool_calls
            .map(|limit| limit.saturating_sub(usage.tool_calls)),
        max_wall_time_ms: budget
            .max_wall_time_ms
            .map(|limit| limit.saturating_sub(usage.wall_time_ms)),
        reasoning_effort: budget.reasoning_effort,
    }
}

/// Infimum zweier Budgets je Dimension; `None` ist das neutrale Element.
fn tighten_agent_budget(left: AgentBudget, right: AgentBudget) -> AgentBudget {
    fn tighter<T: Ord>(left: Option<T>, right: Option<T>) -> Option<T> {
        match (left, right) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (Some(only), None) | (None, Some(only)) => Some(only),
            (None, None) => None,
        }
    }
    AgentBudget {
        max_tokens: tighter(left.max_tokens, right.max_tokens),
        max_tool_calls: tighter(left.max_tool_calls, right.max_tool_calls),
        max_wall_time_ms: tighter(left.max_wall_time_ms, right.max_wall_time_ms),
        reasoning_effort: tighter(left.reasoning_effort, right.reasoning_effort),
    }
}

/// Laufende Zähler eines Kindes, fortgeschrieben vom Progress-Observer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChildLiveStats {
    pub usage: TokenUsage,
    pub tool_calls: u32,
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
    /// Die **ungekürzte** finale Assistenten-Antwort des Kindes, gesetzt bei
    /// `TurnOutcome::Completed`, sofern das Kind Text geliefert hat; sonst
    /// `None`. Typisierte Rückgabe-Verträge parsen diesen Text; für eine
    /// Freitext-Rückgabe an den Elternteil kappt der Aufrufer ihn selbst über
    /// [`cap_child_return_text`] (die gekappte Form liefert weiterhin
    /// [`ManagedAgentSpawner::child_final_assistant_text`]).
    pub full_text: Option<String>,
    /// Verbrauch **dieses** Laufs (Token- und Tool-Aufruf-Differenz der
    /// Kind-Session plus Wanduhrzeit). Gesetzt von
    /// [`ManagedAgentSpawner::run_child_with_budget`]; die übrigen
    /// Lauf-Einstiege liefern [`ChildUsage::default`].
    pub usage: ChildUsage,
}

/// Lebenszyklus-Status eines admittierten Kindes.
///
/// # Description
/// A-CHILD (F-072, F-182). Der Controller schreibt den Status in
/// [`ChildRecord::status`] fort:
/// - `Admitted` → `Running` beim Start eines Turns,
/// - `Running` → `Completed` **nur** bei `TurnOutcome::Completed` (und
///   eingehaltenem Budget, siehe [`ManagedAgentSpawner::run_child_with_budget`]),
/// - `Running` → `Paused` bei erlaubter Pause,
/// - `Running` → `Failed` bei Turn-Fehler, Budget-Verletzung oder verbotener Pause,
/// - `Running` → `Cancelled` bei Abbruch des Kind-Tokens oder gedropptem Lauf.
///
/// Ein abgelaufenes Kind hat keinen Record mehr (siehe [`ExpiredChild`]).
///
/// # Concurrency
/// `Copy`; kein geteilter Zustand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildStatus {
    /// Admittiert, noch nie ausgeführt.
    Admitted,
    /// Ein Turn läuft gerade; die Session ist aus dem Manager entnommen.
    Running,
    /// Der letzte Turn pausierte zulässig (Approval oder Enkel).
    Paused,
    /// Der letzte Turn endete terminal und im Budget.
    Completed,
    /// Der letzte Turn scheiterte (Fehler, Budget, verbotene Pause).
    Failed,
    /// Das Kind wurde abgebrochen; es wird nicht wieder ausgeführt.
    Cancelled,
}

/// Löst das Kontextfenster (Tokens) für eine Modell-ID auf (`None` =
/// Vorgabemodell). Siehe [`ManagedAgentSpawner::with_context_window_resolver`].
pub type ContextWindowResolver = dyn Fn(Option<&str>) -> u64 + Send + Sync;

/// Kontextfenster eines Kindes ohne Resolver (Addendum D).
pub const DEFAULT_CHILD_CONTEXT_WINDOW: u64 = 200_000;

/// Obergrenze eines einzelnen Werkzeugergebnisses in Kind-Turns (Bytes), die
/// jedes Kind als Vorgabe-Turn-Grenze erhält (Welle 3): 64 KiB.
pub const CHILD_TOOL_RESULT_MAX_BYTES: usize = 64 * 1024;

/// Fester Token-Overhead (System-Prompt, Tool-Spezifikationen), den die
/// Auto-Compact-Policy eines Kindes vom Fenster abzieht (Welle 3).
pub const CHILD_FIXED_OVERHEAD_TOKENS: u64 = 4_096;

/// Receives bounded lifecycle snapshots after controller locks have been
/// released. Implementations may persist, forward, or fan out the event but
/// must not make scheduling decisions inside the controller.
pub trait OrchestrationObserver: Send + Sync {
    fn on_orchestration_event(&self, event: AgentOrchestrationEvent);
}

impl ChildStatus {
    /// Liefert das stabile, maschinenlesbare Label dieses Status.
    ///
    /// # Returns
    /// `"admitted"`, `"running"`, `"paused"`, `"completed"`, `"failed"` oder `"cancelled"`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Admitted => "admitted",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Gibt an, ob der Status ein Endzustand ist (`Completed`, `Failed`, `Cancelled`).
    ///
    /// # Returns
    /// `true` für Endzustände.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Ergebnis eines Reaper-Laufs ([`ManagedAgentSpawner::reap`]).
///
/// # Description
/// A-CHILD. `expired` trägt die Korrelation abgelaufener Leases, die der
/// Aufrufer an den wartenden Elternteil zustellen muss; `released` nennt jedes
/// Kind, dessen Slot und Session der Lauf freigegeben hat (verwaist,
/// abgebrochen oder abgelaufen und nicht mehr laufend).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReapReport {
    /// Abgelaufene Leases mit Eltern-/Handoff-Korrelation.
    pub expired: Vec<ExpiredChild>,
    /// Freigegebene Kinder.
    pub released: Vec<SessionId>,
}

/// RAII-Halter eines admittierten Kindes: `Drop` gibt es frei.
///
/// # Description
/// A-CHILD (F-072, G-016). Solange der Guard lebt, bleibt das Kind admittiert
/// und sein Ergebnis abrufbar ([`ManagedAgentSpawner::child_final_assistant_text`]).
/// Fällt der Guard — regulär, per `?`-Early-Return oder beim Abwickeln einer
/// Panic —, ruft er [`ManagedAgentSpawner::release_child`]: Slot frei, Kind-Token
/// (samt Nachkommen) abgebrochen, Session aus dem Manager entfernt. Fehler beim
/// Freigeben werden in `Drop` nur geloggt; wer sie sehen will, ruft
/// [`Self::release`].
///
/// [`Self::keep`] entschärft den Guard, wenn das Kind bewusst weiterleben soll
/// (etwa ein pausiertes Kind, das später fortgesetzt wird).
///
/// # Concurrency
/// Leiht den Spawner (`&'a ManagedAgentSpawner`), ist damit `Send` und darf in
/// `async fn`s über `.await` gehalten werden. Für `'static`-Tasks den Spawner
/// als `Arc` in den Task verschieben und den Guard dort erzeugen.
///
/// # Examples
/// ```rust,no_run
/// use harw_core::child_controller::ManagedAgentSpawner;
/// # fn demo(spawner: &ManagedAgentSpawner, child: harw_types::SessionId)
/// #     -> Result<(), harw_extension_api::AgentSpawnError> {
/// let guard = spawner.guard_child(child);
/// let text = spawner.child_final_assistant_text(guard.child())?; // Early-Return gibt frei
/// let _record = guard.release()?;
/// # let _ = text;
/// # Ok(())
/// # }
/// ```
#[must_use = "dropping a ChildGuard releases the child immediately"]
pub struct ChildGuard<'a> {
    spawner: &'a ManagedAgentSpawner,
    child: SessionId,
    armed: bool,
}

impl ChildGuard<'_> {
    /// Liefert die ID des gehaltenen Kindes.
    ///
    /// # Returns
    /// Die geliehene [`SessionId`] des Kindes.
    #[must_use]
    pub fn child(&self) -> &SessionId {
        &self.child
    }

    /// Gibt das Kind jetzt frei und liefert das Ergebnis der Freigabe.
    ///
    /// # Returns
    /// Den letzten [`ChildRecord`] (mit finalem [`ChildStatus`]) oder `None`,
    /// wenn das Kind schon nicht mehr admittiert war.
    ///
    /// # Errors
    /// Wie [`ManagedAgentSpawner::release_child`]; der In-Memory-Slot ist auch
    /// im Fehlerfall bereits frei.
    pub fn release(mut self) -> Result<Option<ChildRecord>, AgentSpawnError> {
        self.armed = false;
        self.spawner.release_child(&self.child)
    }

    /// Entschärft den Guard, ohne das Kind freizugeben.
    ///
    /// # Returns
    /// Die ID des Kindes; der Aufrufer ist ab jetzt selbst für
    /// [`ManagedAgentSpawner::release_child`] verantwortlich.
    #[must_use]
    pub fn keep(mut self) -> SessionId {
        self.armed = false;
        self.child.clone()
    }
}

impl Drop for ChildGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        match self.spawner.release_child(&self.child) {
            Ok(record) => tracing::debug!(
                child = %self.child,
                status = record.as_ref().map(|record| record.status.as_str()),
                "child_guard.released",
            ),
            Err(error) => tracing::warn!(
                child = %self.child,
                error = %error,
                "child_guard.release_failed",
            ),
        }
    }
}

impl std::fmt::Debug for ChildGuard<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildGuard")
            .field("child", &self.child)
            .field("armed", &self.armed)
            .finish_non_exhaustive()
    }
}

/// Wie eine während eines Turns entnommene Session zurückgegeben wurde.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionReturn {
    /// Wieder im Manager.
    Restored,
    /// Verworfen, weil das Kind während des Laufs freigegeben wurde.
    Discarded,
}

/// Drop-Guard um eine für einen Kind-Turn entnommene Session.
///
/// Wird das Lauf-Future gedroppt (Eltern-Abbruch, Op-Timeout), markiert `Drop`
/// die Session als gescheitert, bricht den Kind-Token ab und legt sie zurück —
/// statt sie wie früher zu verlieren.
struct RunningSession<'a> {
    spawner: &'a ManagedAgentSpawner,
    child: SessionId,
    session: Option<AgentSession>,
}

impl RunningSession<'_> {
    fn session_mut(&mut self) -> Result<&mut AgentSession, AgentSpawnError> {
        let child = &self.child;
        self.session.as_mut().ok_or_else(|| {
            ManagedAgentSpawner::reject(format!("child session {child} was already returned"))
        })
    }

    fn fail_session(&mut self, reason: &str) {
        if let Some(session) = self.session.as_mut() {
            session.fail(reason.to_owned());
        }
    }

    fn finish(mut self) -> Result<SessionReturn, AgentSpawnError> {
        let session = self.session.take().ok_or_else(|| {
            ManagedAgentSpawner::reject(format!(
                "child session {} was already returned",
                self.child
            ))
        })?;
        self.spawner.return_session(session)
    }
}

impl Drop for RunningSession<'_> {
    fn drop(&mut self) {
        let Some(mut session) = self.session.take() else {
            return;
        };
        session.fail("child run was dropped before its turn completed".to_owned());
        if let Some(token) = self.spawner.child_cancel_token(&self.child) {
            token.cancel(CancelReason::Parent);
        }
        self.spawner.set_status(&self.child, ChildStatus::Cancelled);
        if let Err(error) = self.spawner.return_session(session) {
            tracing::warn!(
                child = %self.child,
                error = %error,
                "child_run.drop_return_failed",
            );
        }
    }
}

/// Cancel-Token eines Elternteils, der selbst kein admittiertes Kind ist.
#[derive(Debug, Clone)]
struct ParentToken {
    token: CancelToken,
    /// `true`, wenn der Aufrufer den Token ausdrücklich registriert hat
    /// ([`ManagedAgentSpawner::register_parent_cancel_token`]); solche
    /// Elternteile gelten für den Reaper als lebendig.
    registered: bool,
}

/// Aufgabenkomplexität eines Kind-Auftrags (Addendum D).
///
/// # Beschreibung
/// Rein additiv: `SpawnInput` selbst trägt kein eigenes Feld dafür (siehe
/// `harw-extension-api/src/capabilities.rs`). Der Spawner liest den Wert aus
/// `SpawnInput::context["complexity"]` (`"simple"` oder `"complex"`) und
/// reicht ihn an [`ChildRegistryFactory::model_for_task`] weiter, damit die
/// Modellstelle eines Workers von der Arbeit statt allein von der Rolle
/// abhängen kann.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskComplexity {
    /// Einfache Aufgabe — leichtes Modell.
    Simple,
    /// Komplexe Aufgabe (auch wenn klein) — Mittelklasse-Modell.
    Complex,
}

impl TaskComplexity {
    /// Liest `context["complexity"]` aus einem Spawn-Kontext.
    ///
    /// # Arguments
    /// - `context` (`&serde_json::Value`): der rohe `SpawnInput::context`.
    ///
    /// # Returns
    /// `Some(Self::Simple)` bei `"simple"`, `Some(Self::Complex)` bei
    /// `"complex"`; `None`, wenn das Feld fehlt, kein String ist oder einen
    /// anderen Wert trägt.
    #[must_use]
    pub fn from_spawn_context(context: &serde_json::Value) -> Option<Self> {
        match context
            .get("complexity")
            .and_then(serde_json::Value::as_str)
        {
            Some("simple") => Some(Self::Simple),
            Some("complex") => Some(Self::Complex),
            _ => None,
        }
    }
}

/// Rollenbasierte Reasoning-Effort-Gewichte (Addendum F+G).
///
/// # Beschreibung
/// Klammert das an ein Kind vererbte Effort-Level zusätzlich zur monotonen
/// Eltern-Vererbung ([`DEFAULT_CHILD_REASONING_EFFORT`]) nach der
/// organisatorischen Rolle des Kindes: UIA und ein Root-Orchestrator ohne
/// eigene Sub-Orchestrator-Freigaben bekommen `High`, ein Root-Orchestrator
/// MIT Sub-Orchestrator-Freigaben, ein Sub-Orchestrator und ein komplexer
/// Worker `Medium`, ein einfacher Worker `Low`. [`Self::for_child`] ist die
/// einzige Konsumstelle; [`ManagedAgentSpawner::admit`] klammert das
/// Ergebnis zusätzlich gegen das geerbte Eltern-Level (`min`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoleEffortWeights {
    pub uia: ReasoningEffort,
    pub root_orchestrator: ReasoningEffort,
    pub root_orchestrator_with_subs: ReasoningEffort,
    pub sub_orchestrator: ReasoningEffort,
    pub worker_complex: ReasoningEffort,
    pub worker_simple: ReasoningEffort,
}

impl Default for RoleEffortWeights {
    fn default() -> Self {
        Self {
            uia: ReasoningEffort::High,
            root_orchestrator: ReasoningEffort::High,
            root_orchestrator_with_subs: ReasoningEffort::Medium,
            sub_orchestrator: ReasoningEffort::Medium,
            worker_complex: ReasoningEffort::Medium,
            worker_simple: ReasoningEffort::Low,
        }
    }
}

impl RoleEffortWeights {
    /// Liefert das Rollengewicht für ein admittiertes Kind.
    ///
    /// # Arguments
    /// - `role` (`harw_agent_dsl::roles::AgentRoleId`): organisatorische
    ///   Rolle des Kindes.
    /// - `has_child_orchestrator_grants` (`bool`): ob das Kind selbst
    ///   mindestens eine `ChildOrchestrator`-Freigabe trägt (nur für
    ///   `RootOrchestrator` relevant).
    /// - `complexity` (`Option<TaskComplexity>`): bei `Worker` gelesene
    ///   Aufgabenkomplexität; `None` zählt wie `Complex`.
    ///
    /// # Returns
    /// Das für diese Rolle geltende [`ReasoningEffort`]-Gewicht, **vor** der
    /// Klammerung gegen das geerbte Eltern-Level.
    #[must_use]
    pub fn for_child(
        &self,
        role: harw_agent_dsl::roles::AgentRoleId,
        has_child_orchestrator_grants: bool,
        complexity: Option<TaskComplexity>,
    ) -> ReasoningEffort {
        match role {
            harw_agent_dsl::roles::AgentRoleId::UserInterface => self.uia,
            harw_agent_dsl::roles::AgentRoleId::RootOrchestrator => {
                if has_child_orchestrator_grants {
                    self.root_orchestrator_with_subs
                } else {
                    self.root_orchestrator
                }
            }
            harw_agent_dsl::roles::AgentRoleId::ChildOrchestrator => self.sub_orchestrator,
            harw_agent_dsl::roles::AgentRoleId::Worker => match complexity {
                Some(TaskComplexity::Simple) => self.worker_simple,
                Some(TaskComplexity::Complex) | None => self.worker_complex,
            },
            // Addendum J: `uia-worker` ist eine eigene, abgekapselte
            // Organisationsrolle, gewichtet aber wie ein einfacher Worker.
            harw_agent_dsl::roles::AgentRoleId::UiaWorker => self.worker_simple,
            // Addendum K: `agent-steward` ist eine eigene, interne
            // Organisationsrolle, gewichtet aber wie ein komplexer Worker.
            harw_agent_dsl::roles::AgentRoleId::AgentSteward => self.worker_complex,
        }
    }
}

/// Gemeinsame Delegations-Prüfung für [`ManagedAgentSpawner::admit`] und
/// [`crate::delegation_visibility::visible_delegation_targets`] — dieselben
/// zwei Prädikate an einer Stelle, damit keine Drift zwischen der
/// tatsächlichen Admission und der dem Modell angezeigten Zielliste
/// entstehen kann.
///
/// # Arguments
/// - `caller_role` (`harw_agent_dsl::roles::AgentRoleId`): organisatorische
///   Rolle des delegieren wollenden Agenten.
/// - `target_role` (`harw_agent_dsl::roles::AgentRoleId`): organisatorische
///   Rolle des Ziels.
/// - `target_role_name` (`&str`): exakter registrierter Rollenname des
///   Ziels — nur für die `ChildOrchestrator`-Freigabeliste relevant.
/// - `allowed_child_orchestrators` (`&[String]`): exakte Namen, die der
///   Aufrufer laut seiner eigenen, eingefrorenen Agentendefinition als
///   Kind-Orchestrator delegieren darf.
///
/// # Returns
/// `true`, wenn die Delegation laut geschlossener Spawn-Matrix
/// ([`harw_agent_dsl::roles::can_spawn`], Addendum J: `uia-worker` ist eine
/// eigene Rolle in der Matrix selbst, kein Exklusivitäts-Sonderfall mehr) und
/// — bei einem `ChildOrchestrator`-Ziel — der exakten Freigabeliste erlaubt
/// ist.
pub(crate) fn can_delegate_to(
    caller_role: harw_agent_dsl::roles::AgentRoleId,
    target_role: harw_agent_dsl::roles::AgentRoleId,
    target_role_name: &str,
    allowed_child_orchestrators: &[String],
) -> bool {
    if !harw_agent_dsl::roles::can_spawn(caller_role, target_role) {
        return false;
    }
    if target_role == harw_agent_dsl::roles::AgentRoleId::ChildOrchestrator
        && !allowed_child_orchestrators
            .iter()
            .any(|allowed| allowed == target_role_name)
    {
        return false;
    }
    true
}

/// Vertrauenswürdiger Auszug aus dem Eltern-Kontext einer Admission, wie ihn
/// [`ManagedAgentSpawner::admit`] unmittelbar aus den bereits geprüften
/// Eltern-Daten (Session-Rolle, Registry-Aktivierung, Sandbox, Tiefe,
/// Budget, Effort) bildet — nie aus rohem, modellgesteuertem Handoff-JSON.
///
/// # Beschreibung
/// Welle FANIN-K. Eine [`ChildRegistryFactory`], die eine Kind-Registry
/// enger als nur nach Rolle bauen will (z. B. Werkzeuge auf die tatsächlich
/// vom Elternteil nutzbaren beschränken), erhält hierüber die dafür nötigen
/// Fakten, ohne selbst den `SessionManager` oder die Aktivierung des
/// Elternteils lesen zu müssen.
///
/// # Felder
/// - `role`: die organisatorische Rolle des Elternteils, `None` nur wenn sie
///   sich nicht auflösen ließ.
/// - `tools`: Schnittmenge aus den von der Eltern-Registry tatsächlich
///   bereitgestellten Werkzeugnamen und der Eltern-Aktivierung — leer, wenn
///   keine Registry erreichbar war (fail-closed).
/// - `permissions`: die Sandbox-Rechte des Elternteils.
/// - `max_depth`: verbleibende Tiefe, die der Elternteil selbst noch an
///   Nachkommen vergeben darf.
/// - `budget_tokens`: das effektive Token-Gesamtbudget des Elternteils, `0`
///   wenn keines eingetragen ist.
/// - `reasoning_effort`: das Effort-Label des Elternteils in Kleinschreibung
///   (`"low"`, `"medium"`, …), `None` wenn der Elternteil keines gesetzt hat.
#[derive(Debug, Clone)]
pub struct ParentGrant {
    pub role: Option<harw_agent_dsl::roles::AgentRoleId>,
    pub tools: BTreeSet<String>,
    pub permissions: harw_authority::PermissionSet,
    pub max_depth: u32,
    pub budget_tokens: u64,
    pub reasoning_effort: Option<String>,
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

    /// Wie [`Self::build_registry_with_capabilities`], zusätzlich mit dem
    /// vertrauenswürdigen [`ParentGrant`] der admittierenden Elternsitzung
    /// (Welle FANIN-K).
    ///
    /// # Beschreibung
    /// Der Kompatibilitäts-Default delegiert unverändert an
    /// [`Self::build_registry_with_capabilities`] — mit demselben
    /// Capability-Snapshot, den [`Self::capability_snapshot`] für dieselbe
    /// Rolle/`input`-Kombination liefert, damit eine Factory, die nur
    /// `capability_snapshot`/`build_registry_with_capabilities` überschreibt
    /// (nicht diese Methode), bei der Admission genau dasselbe Verhalten
    /// zeigt wie vor Welle FANIN-K. `suggestions` und `parent` bleiben im
    /// Default ungenutzt — eine Factory, die eine Kind-Registry anhand des
    /// Eltern-Kontexts (Werkzeuge, Rechte, verbleibende Tiefe, Budget,
    /// Effort) enger bauen will, überschreibt stattdessen diese Methode
    /// direkt.
    ///
    /// # Arguments
    /// - `role` (`&str`): der exakte registrierte Rollenname.
    /// - `input` (`&SpawnInput`): der rohe Spawn-Auftrag.
    /// - `suggestions` (`Option<&AgentSuggestions>`): beratende
    ///   Katalog-Vorschläge für diesen Spawn, falls vorhanden.
    /// - `parent` (`&ParentGrant`): der geprüfte Eltern-Kontext dieser
    ///   Admission.
    ///
    /// # Returns
    /// Wie [`Self::build_registry_with_capabilities`].
    ///
    /// # Errors
    /// Wie [`Self::build_registry_with_capabilities`], zusätzlich alles, was
    /// [`Self::capability_snapshot`] selbst zurückgeben kann.
    fn build_registry_with_capabilities_for_parent(
        &self,
        role: &str,
        input: &SpawnInput,
        suggestions: Option<&AgentSuggestions>,
        parent: &ParentGrant,
    ) -> Result<ExtensionRegistry, AgentSpawnError> {
        let _ = suggestions;
        let _ = parent;
        let snapshot = self.capability_snapshot(role, input)?;
        self.build_registry_with_capabilities(role, input, snapshot.as_ref())
    }

    /// Returns the model provider selected for an admitted child role. The
    /// factory owns provider routing; the controller only owns lifecycle and
    /// concurrency boundaries.
    fn model_for(&self, role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError>;

    /// Wie [`Self::model_for`], zusätzlich mit der Aufgabenkomplexität
    /// (Addendum D) — erlaubt Factories, Worker-Modellstellen nach Arbeit
    /// statt allein nach Rolle zu routen. Der Default ignoriert `complexity`
    /// und delegiert an [`Self::model_for`], damit bestehende Factories ohne
    /// Anpassung gültig bleiben.
    ///
    /// # Arguments
    /// - `role` (`&str`): exakter registrierter Rollenname.
    /// - `complexity` (`Option<TaskComplexity>`): aus
    ///   [`TaskComplexity::from_spawn_context`] gelesene Einstufung des
    ///   Auftrags, `None` wenn nicht angegeben.
    ///
    /// # Returns
    /// Der Modell-Provider für diesen Kind-Start.
    ///
    /// # Errors
    /// Wie [`Self::model_for`].
    fn model_for_task(
        &self,
        role: &str,
        complexity: Option<TaskComplexity>,
    ) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
        let _ = complexity;
        self.model_for(role)
    }

    /// Modell-ID, die der Kind-Provider für `role`/`complexity` fest
    /// anspricht (Welle 3), z. B. die eines `PinnedModelProvider`.
    ///
    /// # Description
    /// Grundlage für das Kontextfenster, die Auto-Compact-Policy und die
    /// Ausgabe-Reserve des Kindes bei der Admission. Der Default baut den
    /// Provider über [`Self::model_for_task`] und liest
    /// [`ModelProvider::pinned_model_id`]; Factories, deren `model_for`
    /// Nebenwirkungen hat oder teuer ist, überschreiben diese Methode.
    ///
    /// # Arguments
    /// - `role` (`&str`): exakter registrierter Rollenname.
    /// - `complexity` (`Option<TaskComplexity>`): wie bei
    ///   [`Self::model_for_task`].
    ///
    /// # Returns
    /// Die gepinnte Modell-ID oder `None` (kein Pin bekannt oder
    /// `model_for_task` scheitert — dann gilt das `active_model` der Session).
    fn pinned_model_for_task(
        &self,
        role: &str,
        complexity: Option<TaskComplexity>,
    ) -> Option<String> {
        self.model_for_task(role, complexity)
            .ok()
            .and_then(|provider| provider.pinned_model_id())
    }

    /// Liefert Provider- und Modell-Standard-Reasoning-Effort für die
    /// Provider-/Modell-Zuordnung, die diese Factory für `role` tatsächlich
    /// auflöst (Welle 8: Rangfolge Provider > Modell > Agent > Rolle).
    ///
    /// # Beschreibung
    /// Der Kompatibilitäts-Default liefert `(None, None)`: eine Factory ohne
    /// Config-Zugriff (z. B. die Test-Mocks in diesem Modul) trifft damit auf
    /// diesen beiden Ebenen keine Aussage, und
    /// `resolve_child_default_reasoning_effort` fällt zur Agenten-/Rollen-Ebene
    /// durch. `harw-runtime`s `RuntimeChildRegistryFactory` (hat Zugriff auf
    /// `harw_config::ResolvedConfig`) überschreibt diese Methode mit den
    /// tatsächlich für die aufgelöste Provider-/Modell-ID hinterlegten
    /// `default_reasoning_effort`-Werten.
    ///
    /// # Arguments
    /// - `role` (`&str`): exakter registrierter Rollenname.
    ///
    /// # Returns
    /// `(provider_default, model_default)` — je `None`, wenn diese Ebene
    /// keine Aussage trifft (fehlende Config-Sektion, fehlendes Feld, oder
    /// kein Config-Zugriff).
    fn reasoning_effort_defaults_for_role(
        &self,
        role: &str,
    ) -> (Option<ReasoningEffort>, Option<ReasoningEffort>) {
        let _ = role;
        (None, None)
    }

    /// Wie [`Self::reasoning_effort_defaults_for_role`], zusätzlich mit der
    /// Aufgabenkomplexität (Addendum D) — erlaubt Factories, zwischen den
    /// Worker-Modellstufen `WorkerSimple`/`WorkerComplex` zu unterscheiden,
    /// genau wie [`Self::model_for_task`] gegenüber [`Self::model_for`]. Der
    /// Default ignoriert `complexity` und delegiert an
    /// [`Self::reasoning_effort_defaults_for_role`].
    ///
    /// # Arguments
    /// - `role` (`&str`): exakter registrierter Rollenname.
    /// - `complexity` (`Option<TaskComplexity>`): aus
    ///   [`TaskComplexity::from_spawn_context`] gelesene Einstufung.
    ///
    /// # Returns
    /// Wie [`Self::reasoning_effort_defaults_for_role`].
    fn reasoning_effort_defaults_for_role_task(
        &self,
        role: &str,
        complexity: Option<TaskComplexity>,
    ) -> (Option<ReasoningEffort>, Option<ReasoningEffort>) {
        let _ = complexity;
        self.reasoning_effort_defaults_for_role(role)
    }

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
    /// Geteilt über [`Self::progress_observer`] mit einem lock-freien
    /// [`crate::guard::ProgressObserver`], der nur diese Registry braucht,
    /// um eine Kind-Lease zu verlängern — ohne einen `Arc<Self>` zu
    /// benötigen (Addendum F+G).
    active: Arc<Mutex<BTreeMap<String, ChildRecord>>>,
    /// Lease-expired children which may still be unwinding a model/tool future.
    /// Tombstones make late completion fail closed instead of restoring an
    /// apparently healthy, untracked child session after its parent has been
    /// resumed with the expiry failure.
    expired: Mutex<BTreeMap<String, ExpiredChild>>,
    /// Per-child hierarchical cancellation tokens (C-CANCEL). Each is derived
    /// from its parent's token via [`CancelToken::child`]; `run_child` selects
    /// on it, and lease reaping, budgets, `/agent stop` and parent cancellation
    /// all cancel through it. Cancellation is terminal for the child.
    cancellations: Mutex<BTreeMap<String, CancelToken>>,
    /// Tokens of parents that are not themselves admitted children (roots).
    /// Registered explicitly or created lazily at the first admission.
    parent_tokens: Mutex<BTreeMap<String, ParentToken>>,
    /// Children released while a turn held their session. The returning run
    /// discards the session instead of restoring an untracked child.
    released: Mutex<BTreeSet<String>>,
    /// Optional durable lease ledger. When present, production runtimes use
    /// `reap_expired_durable`/`reconcile_expired_leases` rather than the
    /// compatibility in-memory reaper.
    lease_store: Option<Arc<ChildLeaseStore>>,
    /// Rollengewichte für die Kind-Effort-Klammerung (Addendum F+G,
    /// [`Self::with_role_effort_weights`]). `Default`, solange nichts
    /// explizit gesetzt wurde.
    role_effort_weights: RoleEffortWeights,
    /// Beobachter für Wächter-Ereignisse, die dieser Controller selbst
    /// erkennt (`DuplicateDelegation`, `ChildOverBudget`,
    /// `ChildLeaseExpired`). `None`: keine Beobachtung.
    drift_observer: Option<Arc<dyn crate::guard::DriftObserver>>,
    /// Turn-Wächter-Schwellenwerte, die jede admittierte Kind-Session erhält
    /// (Welle FANIN-K) — dieselben wie die Wurzel-Session dieses Controllers.
    /// `GuardPolicy::default()`, solange nichts explizit gesetzt wurde.
    guard_policy: crate::guard::GuardPolicy,
    /// Pitfall-Berater, den jede admittierte Kind-Session erhält (Welle
    /// FANIN-K), analog zu [`Self::drift_observer`]. `None`: keine Beratung.
    pitfall_advisor: Option<Arc<dyn crate::guard::PitfallAdvisor>>,
    /// Pro Elternteil die letzten 16 normalisierten Auftragstext-Hashes
    /// (Rolle + Anweisung, whitespace-normalisiert, kleingeschrieben) —
    /// erkennt eine doppelt vergebene Delegation (Addendum F+G).
    recent_delegation_hashes: Mutex<BTreeMap<String, VecDeque<u64>>>,
    /// Kontextfenster (Tokens) je Modell-ID des Kindes; `None`-Argument =
    /// Vorgabemodell. Ohne Resolver gilt [`DEFAULT_CHILD_CONTEXT_WINDOW`].
    context_window_resolver: Option<Arc<ContextWindowResolver>>,
    /// Absoluter Auto-Compact-Deckel (Input-Tokens) für Kind-Sessions aus der
    /// Konfiguration ([`Self::with_compaction_ceiling`]). `None`: es gilt
    /// [`crate::auto_compact::DEFAULT_ABSOLUTE_CEILING_TOKENS`].
    compaction_ceiling: Option<u64>,
    /// Optional sink for user-safe lifecycle snapshots. Invocation happens
    /// only after the active/cancellation/manager locks are released.
    orchestration_observer: Option<Arc<dyn OrchestrationObserver>>,
    /// Auftrag (`pending_task`, Kurzkopf) und Ergebnis-Kurzkopf je
    /// admittiertem Kind (Schlüssel: Kind-ID), siehe [`ChildTaskState`].
    child_tasks: Mutex<BTreeMap<String, ChildTaskState>>,
    /// Live-Kanäle der Elternteile je Kind für gedrosselte
    /// [`TurnEvent::ChildProgress`]-Meldungen; geteilt mit dem
    /// [`Self::progress_observer`].
    progress_sinks: ProgressSinks,
    /// Woken every time a slot in `active` is freed (`release_in_memory`,
    /// `reap_expired`, `reap_expired_durable` — every path that removes an
    /// entry from `active`). Lets [`Self::admit_or_wait`] wait for capacity
    /// instead of failing outright when
    /// `active_for_parent >= limits.max_active_children_per_parent` (four
    /// parallel `explore` calls against a limit of two used to fail two of
    /// them hard; see module docs). `notify_waiters` only wakes tasks already
    /// waiting, so every waiter re-registers via `Notified::enable` before
    /// re-checking admission, the same lost-wakeup-safe pattern as
    /// `admission::SubmissionLimiter::freed`, plus a bounded fallback sleep.
    freed: tokio::sync::Notify,
}

/// Anzahl der pro Elternteil vorgehaltenen Delegations-Brief-Hashes
/// (Addendum F+G).
const RECENT_DELEGATION_HASH_CAPACITY: usize = 16;

/// Token-Obergrenze (Summe aus Input- und Output-Tokens), ab der ein
/// abgeschlossenes Kind mit [`TaskComplexity::Simple`] als über dem Budget
/// gilt (Addendum F+G).
const CHILD_OVER_BUDGET_SIMPLE_TOKENS: u64 = 60_000;

/// Token-Obergrenze für ein abgeschlossenes Kind mit
/// [`TaskComplexity::Complex`] oder ohne eingestufte Komplexität
/// (Addendum F+G).
const CHILD_OVER_BUDGET_COMPLEX_TOKENS: u64 = 250_000;

/// Distinguishes why [`ManagedAgentSpawner::admit_inner`] rejected an
/// admission, without either side parsing the other's message text.
///
/// # Description
/// [`ManagedAgentSpawner::admit_or_wait`] only ever retries a `Capacity`
/// rejection (`active_for_parent >= limits.max_active_children_per_parent`);
/// every `Other` rejection — unknown role, spawn-matrix denial, sandbox
/// escalation, depth, lease/registry failure, poisoned lock, ... — is
/// surfaced on the very first attempt, exactly as
/// [`ManagedAgentSpawner::admit`] already did before this distinction
/// existed. The wrapped [`AgentSpawnError`] carries the same message in both
/// variants; [`ManagedAgentSpawner::admit`] discards the variant and returns
/// it unchanged, so its public behavior is unaffected.
enum AdmitRejection {
    /// The parent is already at
    /// [`ChildLimits::max_active_children_per_parent`]. The only rejection
    /// [`ManagedAgentSpawner::admit_or_wait`] waits out.
    Capacity(AgentSpawnError),
    /// Any rejection other than the active-child-per-parent limit. Returned
    /// immediately by [`ManagedAgentSpawner::admit_or_wait`], never retried.
    Other(AgentSpawnError),
}

impl AdmitRejection {
    /// Unwraps either variant into the plain [`AgentSpawnError`] `admit`'s
    /// callers have always seen; the message is never altered.
    fn into_error(self) -> AgentSpawnError {
        match self {
            Self::Capacity(error) | Self::Other(error) => error,
        }
    }
}

impl From<AgentSpawnError> for AdmitRejection {
    /// Every `?`-propagated [`AgentSpawnError`] inside
    /// [`ManagedAgentSpawner::admit_inner`] that is not the explicit
    /// capacity check becomes [`AdmitRejection::Other`] — the capacity
    /// rejection is always constructed explicitly as
    /// [`AdmitRejection::Capacity`], never through this conversion.
    fn from(error: AgentSpawnError) -> Self {
        Self::Other(error)
    }
}

/// Verlängert eine noch nicht abgelaufene Kind-Lease auf `now +
/// lease_seconds`, sofern das mehr ist als der aktuell eingetragene Wert.
///
/// # Beschreibung
/// Gemeinsame Kernlogik von [`ManagedAgentSpawner::renew_lease`] und dem
/// leichten [`ProgressObserver`](crate::guard::ProgressObserver), den
/// [`ManagedAgentSpawner::progress_observer`] liefert — beide dürfen eine
/// bereits abgelaufene Lease nicht wiederbeleben.
///
/// # Arguments
/// - `active` (`&Mutex<BTreeMap<String, ChildRecord>>`): die Aktiv-Registry.
/// - `lease_seconds` (`i64`): die konfigurierte Lease-Dauer.
/// - `child` (`&SessionId`): das zu verlängernde Kind.
/// - `now` (`Timestamp`): Referenzzeitpunkt.
fn renew_active_lease(
    active: &Mutex<BTreeMap<String, ChildRecord>>,
    lease_seconds: i64,
    child: &SessionId,
    now: Timestamp,
) {
    let Ok(mut active) = active.lock() else {
        tracing::warn!(child = %child, "child_lease_renew.lock_poisoned");
        return;
    };
    let Some(record) = active.get_mut(child.as_str()) else {
        return;
    };
    if now >= record.lease_expires_at {
        // Bereits abgelaufen: kein Wiederbeleben durch einen späten
        // Fortschritts-Event.
        return;
    }
    let Ok(candidate) = now.checked_add(SignedDuration::from_secs(lease_seconds)) else {
        tracing::warn!(child = %child, "child_lease_renew.overflow");
        return;
    };
    if candidate > record.lease_expires_at {
        record.lease_expires_at = candidate;
    }
}

/// Leichter [`crate::guard::ProgressObserver`], der nur die Aktiv-Registry
/// und die Lease-Dauer hält — kein `Arc<Self>` auf den vollen Controller
/// (Addendum F+G, [`ManagedAgentSpawner::progress_observer`]).
struct ActiveLeaseProgressObserver {
    active: Arc<Mutex<BTreeMap<String, ChildRecord>>>,
    lease_seconds: i64,
    orchestration_observer: Option<Arc<dyn OrchestrationObserver>>,
    /// Live-Kanäle der Elternteile für [`TurnEvent::ChildProgress`].
    progress_sinks: ProgressSinks,
}

impl ActiveLeaseProgressObserver {
    fn update_live(&self, session_id: &SessionId, update: impl FnOnce(&mut ChildLiveStats)) {
        if let Ok(mut active) = self.active.lock()
            && let Some(record) = active.get_mut(session_id.as_str())
        {
            update(&mut record.live);
        }
    }
}

impl crate::guard::ProgressObserver for ActiveLeaseProgressObserver {
    fn on_round_usage(&self, session_id: &SessionId, usage: &TokenUsage) {
        self.update_live(session_id, |live| live.usage.add(usage));
    }

    fn on_tool_call(&self, session_id: &SessionId) {
        self.update_live(session_id, |live| {
            live.tool_calls = live.tool_calls.saturating_add(1);
        });
    }

    fn on_progress(&self, session_id: &SessionId) {
        renew_active_lease(
            &self.active,
            self.lease_seconds,
            session_id,
            Timestamp::now(),
        );
        // The turn-loop calls this after every model round and tool result.
        // Snapshot under the registry lock, then notify after releasing it so
        // a durable/UI observer can never re-enter controller locking.
        let snapshot = self.active.lock().ok().and_then(|active| {
            let record = active.get(session_id.as_str())?.clone();
            let root = ManagedAgentSpawner::root_from_active(&active, session_id)?;
            Some((root, record))
        });
        if let Some((_, record)) = snapshot.as_ref() {
            emit_child_progress(&self.progress_sinks, record, std::time::Instant::now());
        }
        if let (Some(observer), Some((root, record))) = (&self.orchestration_observer, snapshot) {
            emit_orchestration_event(
                observer,
                root,
                &record,
                None,
                None,
                AgentOrchestrationStatus::Progress,
            );
        }
    }
}

/// Sendet gedrosselt ein [`TurnEvent::ChildProgress`] für `record` an die
/// registrierte Fortschritts-Senke seines Elternteils.
///
/// # Beschreibung
/// Höchstens eine Meldung je [`CHILD_PROGRESS_MIN_INTERVAL`] und Kind; eine
/// gedrosselte Meldung wird verworfen (die nächste trägt ohnehin die dann
/// aktuellen Zähler). Ohne Senke ist das ein No-op. Die Sperre der Registry
/// ist beim Senden bereits freigegeben.
///
/// # Arguments
/// - `sinks` (`&ProgressSinks`): die geteilte Senken-Registry.
/// - `record` (`&ChildRecord`): Schnappschuss des Kindes (Zähler aus `live`).
/// - `now` (`std::time::Instant`): Referenzzeitpunkt der Drosselung.
///
/// # Returns
/// `true`, wenn eine Meldung gesendet wurde.
fn emit_child_progress(
    sinks: &ProgressSinks,
    record: &ChildRecord,
    now: std::time::Instant,
) -> bool {
    let target = {
        let Ok(mut sinks) = sinks.lock() else {
            tracing::warn!(child = %record.child, "child_progress.lock_poisoned");
            return false;
        };
        let Some(sink) = sinks.get_mut(record.child.as_str()) else {
            return false;
        };
        let throttled = sink
            .last_emitted
            .is_some_and(|last| now.saturating_duration_since(last) < CHILD_PROGRESS_MIN_INTERVAL);
        if throttled {
            return false;
        }
        sink.last_emitted = Some(now);
        (sink.turn_id.clone(), sink.emitter.clone())
    };
    let (turn_id, emitter) = target;
    emitter.emit(TurnEvent::ChildProgress {
        turn_id,
        child: record.child.clone(),
        tool_calls: record.live.tool_calls,
        tokens: record.live.usage.total(),
    });
    true
}

/// Builds one user-safe lifecycle observation. Callers must invoke this only
/// after releasing controller locks; observers are allowed to persist or fan
/// out synchronously.
fn emit_orchestration_event(
    observer: &Arc<dyn OrchestrationObserver>,
    root_session_id: SessionId,
    record: &ChildRecord,
    task: Option<String>,
    detail: Option<String>,
    status: AgentOrchestrationStatus,
) {
    observer.on_orchestration_event(AgentOrchestrationEvent {
        schema_version: AgentOrchestrationEvent::CURRENT_SCHEMA_VERSION,
        event_id: Uuid::new_v4().to_string(),
        root_session_id,
        parent_session_id: record.parent.clone(),
        child_session_id: record.child.clone(),
        turn_id: None,
        role: record.role.clone(),
        depth: record.depth,
        task: AgentOrchestrationEvent::bounded_detail(task),
        status,
        usage: Some(record.live.usage.clone()),
        duration_ms: Some(elapsed_ms(record.admitted_at)),
        progress: None,
        detail: AgentOrchestrationEvent::bounded_detail(detail),
        tool_calls: Some(record.live.tool_calls),
        model: record.model.clone(),
    });
}

/// Millisekunden seit `since` (0 bei Uhrensprung).
fn elapsed_ms(since: Timestamp) -> u64 {
    u64::try_from(Timestamp::now().duration_since(since).as_millis()).unwrap_or(0)
}

/// Normalisiert einen Delegations-Auftragstext für den Duplikat-Vergleich:
/// Rolle + Anweisung, Whitespace zu einzelnen Leerzeichen zusammengefasst,
/// kleingeschrieben (Addendum F+G).
fn normalize_delegation_brief(role_name: &str, instructions: Option<&str>) -> String {
    let mut normalized = String::new();
    for ch in role_name.chars() {
        normalized.extend(ch.to_lowercase());
    }
    normalized.push('\u{0}');
    let mut last_was_space = false;
    for ch in instructions.unwrap_or_default().chars() {
        if ch.is_whitespace() {
            if !last_was_space {
                normalized.push(' ');
                last_was_space = true;
            }
        } else {
            normalized.extend(ch.to_lowercase());
            last_was_space = false;
        }
    }
    normalized
}

/// Hasht einen bereits normalisierten Delegations-Auftragstext.
fn hash_delegation_brief(normalized: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    normalized.hash(&mut hasher);
    hasher.finish()
}

impl ManagedAgentSpawner {
    fn root_from_active(
        active: &BTreeMap<String, ChildRecord>,
        session: &SessionId,
    ) -> Option<SessionId> {
        let mut cursor = session.clone();
        let mut seen = std::collections::HashSet::new();
        loop {
            if !seen.insert(cursor.as_str().to_owned()) {
                return None;
            }
            match active.get(cursor.as_str()) {
                Some(record) => cursor = record.parent.clone(),
                None => return Some(cursor),
            }
        }
    }

    fn observe_orchestration(
        &self,
        root_session_id: SessionId,
        record: &ChildRecord,
        task: Option<String>,
        status: AgentOrchestrationStatus,
    ) {
        let Some(observer) = &self.orchestration_observer else {
            return;
        };
        let state = self.child_task_state(&record.child);
        let task = task
            .as_deref()
            .and_then(orchestration_detail_head)
            .or_else(|| state.as_ref().and_then(|state| state.task.clone()));
        let detail = match status {
            AgentOrchestrationStatus::Completed | AgentOrchestrationStatus::Failed => {
                state.and_then(|state| state.outcome_detail)
            }
            _ => None,
        };
        emit_orchestration_event(observer, root_session_id, record, task, detail, status);
    }

    /// Kopie des Auftrags-/Ergebniszustands eines Kindes (`None`, wenn
    /// unbekannt oder die Sperre vergiftet ist).
    fn child_task_state(&self, child: &SessionId) -> Option<ChildTaskState> {
        self.child_tasks
            .lock()
            .ok()
            .and_then(|tasks| tasks.get(child.as_str()).cloned())
    }

    /// Hinterlegt den Kurzkopf der finalen Antwort bzw. des Fehlergrunds
    /// eines Kindes für das spätere `Completed`/`Failed`-Orchestrierungs-Event.
    fn set_outcome_detail(&self, child: &SessionId, text: &str) {
        match self.child_tasks.lock() {
            Ok(mut tasks) => {
                tasks
                    .entry(child.as_str().to_owned())
                    .or_default()
                    .outcome_detail = orchestration_detail_head(text);
            }
            Err(_) => tracing::warn!(child = %child, "child_task_state.lock_poisoned"),
        }
    }

    /// Markiert ein Kind als gescheitert und merkt sich den Grund als
    /// `detail` des späteren `Failed`-Orchestrierungs-Events.
    fn set_failed(&self, child: &SessionId, reason: &str) {
        self.set_outcome_detail(child, reason);
        self.set_status(child, ChildStatus::Failed);
    }

    /// Entnimmt den noch nicht verbrauchten Auftrag eines Kindes (einmalig).
    fn take_pending_task(&self, child: &SessionId) -> Option<String> {
        self.child_tasks
            .lock()
            .ok()
            .and_then(|mut tasks| tasks.get_mut(child.as_str())?.pending_task.take())
    }

    /// Meldet den Abschluss-/Freigabestatus eines eben freigegebenen Kindes
    /// und räumt danach dessen Auftrags- und Fortschrittszustand ab.
    fn observe_release(
        &self,
        child: &SessionId,
        root: Option<SessionId>,
        record: Option<&ChildRecord>,
    ) {
        if let (Some(root), Some(record)) = (root, record) {
            let status = match record.status {
                ChildStatus::Completed => AgentOrchestrationStatus::Completed,
                ChildStatus::Failed => AgentOrchestrationStatus::Failed,
                ChildStatus::Cancelled => AgentOrchestrationStatus::Cancelled,
                ChildStatus::Paused => AgentOrchestrationStatus::Paused,
                ChildStatus::Running => AgentOrchestrationStatus::Running,
                ChildStatus::Admitted => AgentOrchestrationStatus::Cancelled,
            };
            self.observe_orchestration(root, record, None, status);
        }
        self.forget_child_state(child);
    }

    /// Entfernt Auftrags- und Fortschrittszustand eines nicht mehr aktiven Kindes.
    fn forget_child_state(&self, child: &SessionId) {
        if let Ok(mut tasks) = self.child_tasks.lock() {
            tasks.remove(child.as_str());
        }
        if let Ok(mut sinks) = self.progress_sinks.lock() {
            sinks.remove(child.as_str());
        }
    }

    /// Registriert den Live-Kanal, über den dieses Kind gedrosselte
    /// [`TurnEvent::ChildProgress`]-Meldungen an seinen Elternteil sendet.
    ///
    /// # Beschreibung
    /// Die Admission registriert die Senke automatisch, wenn die
    /// Elternsitzung beim Spawn im [`SessionManager`] liegt und einen
    /// laufenden Turn hat (deren [`AgentSession::live_emitter`] und
    /// [`AgentSession::current_turn`]). Für Elternteile außerhalb des
    /// Managers (externe Wurzel, gerade laufende Kind-Sitzung) ruft der
    /// Aufrufer, der den Eltern-Turn fährt, diese Methode nach dem Spawn.
    /// Eine vorhandene Senke wird ersetzt. Gesendet wird aus dem
    /// [`Self::progress_observer`] nach jeder Modellrunde bzw. jedem
    /// Tool-Ergebnis, höchstens alle [`CHILD_PROGRESS_MIN_INTERVAL`].
    ///
    /// # Arguments
    /// - `child` (`&SessionId`): das admittierte Kind.
    /// - `turn_id` (`TurnId`): der Turn des Elternteils, der das Kind startete.
    /// - `emitter` (`LiveEmitter`): der Live-Kanal des Elternteils.
    ///
    /// # Returns
    /// `true`, wenn das Kind admittiert ist und die Senke registriert wurde.
    pub fn attach_child_progress_sink(
        &self,
        child: &SessionId,
        turn_id: TurnId,
        emitter: LiveEmitter,
    ) -> bool {
        if self.child_record(child).is_none() {
            return false;
        }
        match self.progress_sinks.lock() {
            Ok(mut sinks) => {
                sinks.insert(
                    child.as_str().to_owned(),
                    ChildProgressSink {
                        turn_id,
                        emitter,
                        last_emitted: None,
                    },
                );
                true
            }
            Err(_) => {
                tracing::warn!(child = %child, "child_progress.attach_lock_poisoned");
                false
            }
        }
    }

    /// Setzt den Resolver, der jedem Kind das Kontextfenster **seines**
    /// Modells zuweist (statt eines festen Werts). Grundlage für Auto-
    /// Compaction und das Byte-Budget des Kindes.
    #[must_use]
    pub fn with_context_window_resolver(mut self, resolver: Arc<ContextWindowResolver>) -> Self {
        self.context_window_resolver = Some(resolver);
        self
    }

    /// Setzt den absoluten Auto-Compact-Deckel (Input-Tokens) für jede
    /// admittierte Kind-Session (Welle 3).
    ///
    /// # Arguments
    /// - `ceiling` (`Option<u64>`): konfigurierter Deckel; `None` lässt den
    ///   Standard [`crate::auto_compact::DEFAULT_ABSOLUTE_CEILING_TOKENS`]
    ///   gelten.
    ///
    /// # Returns
    /// Den Spawner mit gesetztem Deckel (verbrauchender Builder).
    #[must_use]
    pub fn with_compaction_ceiling(mut self, ceiling: Option<u64>) -> Self {
        self.compaction_ceiling = ceiling;
        self
    }

    /// Effektiver absoluter Auto-Compact-Deckel für Kind-Sessions:
    /// konfigurierter Wert, sonst
    /// [`crate::auto_compact::DEFAULT_ABSOLUTE_CEILING_TOKENS`].
    #[must_use]
    pub fn compaction_ceiling(&self) -> u64 {
        self.compaction_ceiling
            .unwrap_or(crate::auto_compact::DEFAULT_ABSOLUTE_CEILING_TOKENS)
    }

    /// Liefert die globalen Admission-Limits dieses Controllers.
    #[must_use]
    pub fn limits(&self) -> ChildLimits {
        self.limits
    }

    fn context_window_for(&self, model: Option<&str>) -> u64 {
        self.context_window_resolver
            .as_ref()
            .map_or(DEFAULT_CHILD_CONTEXT_WINDOW, |resolve| resolve(model))
    }

    #[must_use]
    pub fn new(manager: Arc<Mutex<SessionManager>>, limits: ChildLimits) -> Self {
        Self {
            manager,
            limits,
            roles: BTreeMap::new(),
            external_root_parent: None,
            active: Arc::new(Mutex::new(BTreeMap::new())),
            expired: Mutex::new(BTreeMap::new()),
            cancellations: Mutex::new(BTreeMap::new()),
            parent_tokens: Mutex::new(BTreeMap::new()),
            released: Mutex::new(BTreeSet::new()),
            lease_store: None,
            role_effort_weights: RoleEffortWeights::default(),
            drift_observer: None,
            guard_policy: crate::guard::GuardPolicy::default(),
            pitfall_advisor: None,
            recent_delegation_hashes: Mutex::new(BTreeMap::new()),
            context_window_resolver: None,
            compaction_ceiling: None,
            orchestration_observer: None,
            child_tasks: Mutex::new(BTreeMap::new()),
            progress_sinks: Arc::new(Mutex::new(BTreeMap::new())),
            freed: tokio::sync::Notify::new(),
        }
    }

    /// Installs the runtime-owned lifecycle sink. The controller retains no
    /// persistence dependency; a runtime can attach a StateStore-backed sink.
    #[must_use]
    pub fn with_orchestration_observer(mut self, observer: Arc<dyn OrchestrationObserver>) -> Self {
        self.orchestration_observer = Some(observer);
        self
    }

    /// Setzt die Rollengewichte für die Kind-Effort-Klammerung.
    /// `None` klammert auf [`RoleEffortWeights::default`].
    #[must_use]
    pub fn with_role_effort_weights(mut self, weights: Option<RoleEffortWeights>) -> Self {
        self.role_effort_weights = weights.unwrap_or_default();
        self
    }

    /// Setzt den Beobachter für vom Controller selbst erkannte
    /// Wächter-Ereignisse (`DuplicateDelegation`, `ChildOverBudget`,
    /// `ChildLeaseExpired`). `None`: keine Beobachtung.
    #[must_use]
    pub fn with_drift_observer(
        mut self,
        observer: Option<Arc<dyn crate::guard::DriftObserver>>,
    ) -> Self {
        self.drift_observer = observer;
        self
    }

    /// Setzt die Turn-Wächter-Schwellenwerte, die jede über diesen
    /// Controller admittierte Kind-Session erhält (Welle FANIN-K) — dieselbe
    /// Politik wie die Wurzel-Session.
    #[must_use]
    pub fn with_guard_policy(mut self, policy: crate::guard::GuardPolicy) -> Self {
        self.guard_policy = policy;
        self
    }

    /// Setzt den Pitfall-Berater, den jede über diesen Controller
    /// admittierte Kind-Session erhält (Welle FANIN-K), analog zu
    /// [`Self::with_drift_observer`]. `None`: keine Beratung.
    #[must_use]
    pub fn with_pitfall_advisor(
        mut self,
        advisor: Option<Arc<dyn crate::guard::PitfallAdvisor>>,
    ) -> Self {
        self.pitfall_advisor = advisor;
        self
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

    /// Verlängert die Lease eines noch aktiven, nicht abgelaufenen Kindes auf
    /// `max(aktueller Wert, now + lease_seconds)` (Addendum F+G).
    ///
    /// # Beschreibung
    /// Wirkt nur auf die In-Memory-Registry — `harw-session-store` bietet
    /// (Stand dieses Vertrags) keine Verlängerung eines bereits admittierten
    /// durablen Leases, nur `admit`/`claim_expired`/`complete`. Eine bereits
    /// abgelaufene Lease wird nicht wiederbelebt: sie ist bereits Kandidat
    /// für [`Self::reap_expired_durable`].
    ///
    /// # Arguments
    /// - `child` (`&SessionId`): das zu verlängernde Kind.
    /// - `now` (`Timestamp`): Referenzzeitpunkt.
    ///
    /// # Returns
    /// `Ok(())` immer — unbekannte Kinder und eine vergiftete Sperre werden
    /// ignoriert (`tracing::warn!`, idempotent), nicht als Fehler propagiert;
    /// die `Result`-Hülle folgt der Vertragssignatur und bleibt damit
    /// erweiterbar, falls eine künftige durable Verlängerung fehlschlagen
    /// kann.
    ///
    /// # Errors
    /// Liefert derzeit nie `Err` — siehe `# Returns`.
    pub fn renew_lease(&self, child: &SessionId, now: Timestamp) -> Result<(), AgentSpawnError> {
        renew_active_lease(&self.active, self.limits.lease_seconds, child, now);
        // Welle FANIN-K: die In-Memory-Verlängerung oben bleibt maßgeblich für
        // den laufenden Prozess; ist zusätzlich ein durabler Lease-Store
        // konfiguriert, wird derselbe Fortschritt auch dorthin gespiegelt,
        // damit ein Neustart die verlängerte Fälligkeit sieht. Ein Fehler
        // dabei bricht die Verlängerung nicht ab — die In-Memory-Sicht bleibt
        // korrekt, nur die Durability hinkt bis zur nächsten Verlängerung
        // hinterher.
        if let Some(lease_store) = &self.lease_store {
            if let Err(error) = lease_store.renew(child, self.limits.lease_seconds, now) {
                tracing::warn!(
                    child = %child,
                    error = %error,
                    "child_lease_renew.durable_renew_failed"
                );
            }
        }
        Ok(())
    }

    /// Liefert einen [`crate::guard::ProgressObserver`], der Fortschritt in
    /// einer laufenden Kind-Session in eine Lease-Verlängerung übersetzt.
    ///
    /// # Beschreibung
    /// Hält nur einen `Arc`-Klon der geteilten Aktiv-Registry (kein
    /// `Arc<Self>` nötig) und die konfigurierte Lease-Dauer. Registriert
    /// über [`crate::session::AgentSession::with_progress_observer`] an
    /// jeder Kind-Session, ruft die dieselbe Renew-Logik wie
    /// [`Self::renew_lease`] mit dem aktuellen Zeitpunkt auf. Zusätzlich
    /// sendet er je Fortschritt höchstens alle
    /// [`CHILD_PROGRESS_MIN_INTERVAL`] ein `TurnEvent::ChildProgress` an die
    /// per [`Self::attach_child_progress_sink`] (oder automatisch bei der
    /// Admission) registrierte Senke des Elternteils.
    #[must_use]
    pub fn progress_observer(&self) -> Arc<dyn crate::guard::ProgressObserver> {
        Arc::new(ActiveLeaseProgressObserver {
            active: Arc::clone(&self.active),
            lease_seconds: self.limits.lease_seconds,
            orchestration_observer: self.orchestration_observer.clone(),
            progress_sinks: Arc::clone(&self.progress_sinks),
        })
    }

    /// Releases an admitted child in memory (compatibility entry point for
    /// [`AgentSpawner::child_finished`]). Unknown IDs are ignored so recovery
    /// can reconcile an already-closed record idempotently.
    ///
    /// Since A-CHILD this is a full in-memory release: slot, cancel token
    /// (cancelled), tombstones and the child session in the manager. Lock
    /// failures are logged; use [`Self::release_child`] to observe them.
    pub fn close_child(&self, child: &SessionId) {
        let root = self.root_for(child);
        match self.release_in_memory(child) {
            Ok(Some(record)) => {
                self.charge_released_child(&record);
                self.observe_release(child, root, Some(&record));
            }
            Ok(None) => self.forget_child_state(child),
            Err(error) => {
                tracing::warn!(child = %child, error = %error, "child_close.release_failed");
            }
        }
    }

    /// Marks the lease completed before releasing in-memory admission state.
    /// Call this only after the child's terminal result has been durably
    /// delivered to its parent.
    ///
    /// # Errors
    /// [`AgentSpawnError`] when the durable completion fails (nothing is
    /// released then, so a retry is possible) or an in-memory lock is poisoned.
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
        let root = self.root_for(child);
        let released = self.release_in_memory(child)?;
        if let Some(record) = released.as_ref() {
            self.charge_released_child(record);
        }
        self.observe_release(child, root, released.as_ref());
        Ok(())
    }

    /// Gibt ein admittiertes Kind vollständig frei.
    ///
    /// # Description
    /// A-CHILD (F-072, G-016). In dieser Reihenfolge, jeweils mit kurzer,
    /// einzeln genommener Sperre:
    /// 1. Record aus `active` entfernen — der Slot ist ab hier frei;
    /// 2. Lease-Tombstone entfernen;
    /// 3. Kind-Token entfernen und abbrechen ([`CancelReason::Parent`]) — ein
    ///    noch laufender Turn und alle Nachkommen brechen ab;
    /// 4. Session aus dem [`SessionManager`] entfernen. Hält gerade ein Turn die
    ///    Session, wird ein Freigabe-Tombstone gesetzt; der zurückkehrende Lauf
    ///    verwirft die Session dann, statt sie wiederherzustellen;
    /// 5. mit Lease-Store: den durablen Lease schließen (`complete`). Der
    ///    Store kennt noch keinen Abschlussstatus; der finale
    ///    [`ChildStatus`] steht im zurückgegebenen Record und im Log.
    ///
    /// Idempotent: ein zweiter Aufruf liefert `Ok(None)`.
    ///
    /// # Arguments
    /// - `child` (`&SessionId`): das freizugebende Kind.
    ///
    /// # Returns
    /// `Ok(Some(record))` mit dem letzten Record (inkl. Status), `Ok(None)`,
    /// wenn das Kind nicht (mehr) admittiert war.
    ///
    /// # Errors
    /// - [`AgentSpawnError`], wenn eine Sperre vergiftet ist (bereits
    ///   abgeschlossene Schritte bleiben wirksam).
    /// - [`AgentSpawnError`], wenn der durable Lease nicht geschlossen werden
    ///   konnte; der In-Memory-Slot ist dann trotzdem frei, der Lease wird
    ///   später vom Reconcile als abgelaufen eingesammelt.
    ///
    /// # Concurrency
    /// Nimmt nie zwei Sperren außer `manager` ⊃ `released`; darf aus `Drop`
    /// und parallel zu laufenden Turns gerufen werden.
    ///
    /// # Examples
    /// ```rust,no_run
    /// # fn demo(spawner: &harw_core::ManagedAgentSpawner, child: harw_types::SessionId) {
    /// let released = spawner.release_child(&child);
    /// # let _ = released;
    /// # }
    /// ```
    pub fn release_child(&self, child: &SessionId) -> Result<Option<ChildRecord>, AgentSpawnError> {
        // Snapshot the root while the record is still in the admission tree;
        // `release_in_memory` deliberately removes it before returning.
        let root = self.root_for(child);
        let record = self.release_in_memory(child)?;
        if let (Some(lease_store), Some(record)) = (&self.lease_store, record.as_ref()) {
            if let Err(error) = lease_store.complete(child, Timestamp::now()) {
                // Der Slot ist trotzdem frei: Auftrags-/Fortschrittszustand
                // abräumen, bevor der Fehler zurückgeht.
                self.forget_child_state(child);
                return Err(Self::reject(format!(
                    "child {child} was released but its durable lease could not be closed: {error}"
                )));
            }
            tracing::info!(child = %child, status = record.status.as_str(), "child_release.lease_closed");
        }
        self.observe_release(child, root, record.as_ref());
        Ok(record)
    }

    /// Nimmt ein bereits admittiertes Kind in einen [`ChildGuard`].
    ///
    /// # Arguments
    /// - `child` (`SessionId`): das admittierte Kind (wird verschoben).
    ///
    /// # Returns
    /// Einen scharfen Guard; sein `Drop` ruft [`Self::release_child`].
    pub fn guard_child(&self, child: SessionId) -> ChildGuard<'_> {
        ChildGuard {
            spawner: self,
            child,
            armed: true,
        }
    }

    /// Admittiert ein Kind und liefert es direkt in einem [`ChildGuard`].
    ///
    /// # Description
    /// Dieselbe Admission wie [`AgentSpawner::spawn_child`], aber ohne
    /// Zeitfenster, in dem ein admittiertes Kind ungeschützt wäre.
    ///
    /// # Arguments
    /// - `role` (`&str`): registrierter Rollenname.
    /// - `input` (`SpawnInput`): Eltern-Korrelation und Kontextwunsch.
    /// - `sandbox` (`SandboxSpec`): bereits reduzierte Kind-Sandbox.
    /// - `suggestions` (`Option<AgentSuggestions>`): beratende Vorschläge.
    ///
    /// # Returns
    /// Den Guard des neuen Kindes.
    ///
    /// # Errors
    /// Jede Ablehnung der Admission (Rolle, Matrix, Sandbox, Decke, Tiefe,
    /// Deckel, abgebrochener Elternteil, Registry-Fehler, Lease-Store).
    pub fn spawn_child_guarded(
        &self,
        role: &str,
        input: SpawnInput,
        sandbox: SandboxSpec,
        suggestions: Option<AgentSuggestions>,
    ) -> Result<ChildGuard<'_>, AgentSpawnError> {
        self.admit(role, input, sandbox, suggestions)
            .map(|child| self.guard_child(child))
    }

    /// Liefert den aktuellen [`ChildStatus`] eines admittierten Kindes.
    ///
    /// # Returns
    /// `Some(status)` solange das Kind admittiert ist, sonst `None`.
    #[must_use]
    pub fn child_status(&self, child: &SessionId) -> Option<ChildStatus> {
        self.child_record(child).map(|record| record.status)
    }

    /// Liefert den Cancel-Token eines admittierten Kindes.
    ///
    /// # Description
    /// Der Token ist bei der Admission als `parent_token.child()` entstanden.
    /// Aufrufer können ihn z. B. an den Turn-Loop weiterreichen.
    ///
    /// # Returns
    /// Einen Klon (teilt den Knoten) oder `None`, wenn das Kind nicht bekannt ist.
    #[must_use]
    pub fn child_cancel_token(&self, child: &SessionId) -> Option<CancelToken> {
        self.cancellations
            .lock()
            .ok()
            .and_then(|tokens| tokens.get(child.as_str()).cloned())
    }

    /// Registriert den Cancel-Token eines Elternteils, der kein admittiertes Kind ist.
    ///
    /// # Description
    /// A-CHILD. Kinder, die **danach** unter `parent` admittiert werden, erhalten
    /// `token.child()`; `token.cancel(..)` bricht sie samt Nachkommen ab. Ein
    /// ersetzter Token wirkt nicht rückwirkend auf bereits admittierte Kinder.
    /// Registrierte Elternteile gelten für den Reaper als lebendig, auch wenn
    /// sie nicht im Manager liegen (TUI-/CLI-Wurzeln).
    ///
    /// # Arguments
    /// - `parent` (`&SessionId`): Wurzel- oder externe Elternsitzung.
    /// - `token` (`CancelToken`): deren Abbruch-Token.
    ///
    /// # Errors
    /// [`AgentSpawnError`], wenn `parent` selbst ein admittiertes Kind ist
    /// (dessen Token ist controller-eigen) oder eine Sperre vergiftet ist.
    pub fn register_parent_cancel_token(
        &self,
        parent: &SessionId,
        token: CancelToken,
    ) -> Result<(), AgentSpawnError> {
        let is_child = self
            .cancellations
            .lock()
            .map_err(|_| Self::reject("child cancellation registry lock is poisoned"))?
            .contains_key(parent.as_str());
        if is_child {
            return Err(Self::reject(format!(
                "session {parent} is an admitted child; its cancel token is derived from its parent"
            )));
        }
        self.parent_tokens
            .lock()
            .map_err(|_| Self::reject("parent cancellation registry lock is poisoned"))?
            .insert(
                parent.as_str().to_owned(),
                ParentToken {
                    token,
                    registered: true,
                },
            );
        Ok(())
    }

    /// Räumt verwaiste, abgebrochene und abgelaufene Kinder ab.
    ///
    /// # Description
    /// A-CHILD, für periodische Aufrufer (Runtime-Tick). Zuerst werden
    /// abgelaufene Leases eingesammelt ([`Self::reap_expired_durable`], Kinder
    /// werden abgebrochen und ihre Sessions als gescheitert markiert), dann
    /// gibt der Lauf frei:
    /// - verwaiste Kinder (Elternteil weder im Manager, noch admittiertes Kind,
    ///   noch externe Wurzel, noch registrierter Elternteil),
    /// - Kinder mit abgebrochenem Token, deren Turn nicht mehr läuft,
    /// - abgelaufene Kinder, deren Session wieder im Manager liegt.
    ///
    /// Abgeschlossene, pausierte oder gescheiterte Kinder mit lebendem
    /// Elternteil bleiben stehen: ihr Ergebnis kann noch ausgewertet werden;
    /// sie gibt der [`ChildGuard`] bzw. [`Self::release_child`] frei.
    ///
    /// # Arguments
    /// - `now` (`Timestamp`): Referenzzeit für den Lease-Ablauf.
    ///
    /// # Returns
    /// Einen [`ReapReport`]; `expired` muss der Aufrufer an die Elternteile zustellen.
    ///
    /// # Errors
    /// [`AgentSpawnError`], wenn der durable Reaper scheitert oder eine Sperre
    /// vergiftet ist.
    ///
    /// # Concurrency
    /// Nimmt nur Schnappschüsse unter kurzen Sperren; sicher parallel zu Admission und Fan-out.
    pub fn reap(&self, now: Timestamp) -> Result<ReapReport, AgentSpawnError> {
        let expired = self.reap_expired_durable(now)?;
        let released = self.release_reapable()?;
        if !expired.is_empty() || !released.is_empty() {
            tracing::info!(
                expired = expired.len(),
                released = released.len(),
                "child_reaper.pass",
            );
        }
        Ok(ReapReport { expired, released })
    }

    /// Gibt alle Kinder frei, die ohne Ergebnisverlust freigegeben werden dürfen.
    fn release_reapable(&self) -> Result<Vec<SessionId>, AgentSpawnError> {
        let records: Vec<(SessionId, SessionId, ChildStatus)> = self
            .active
            .lock()
            .map_err(|_| Self::reject("child registry lock is poisoned"))?
            .values()
            .map(|record| (record.child.clone(), record.parent.clone(), record.status))
            .collect();
        let tombstones: Vec<SessionId> = self
            .expired
            .lock()
            .map_err(|_| Self::reject("expired-child registry lock is poisoned"))?
            .values()
            .map(|expired| expired.child.clone())
            .collect();
        let cancelled: BTreeSet<String> = self
            .cancellations
            .lock()
            .map_err(|_| Self::reject("child cancellation registry lock is poisoned"))?
            .iter()
            .filter(|(_, token)| token.is_cancelled())
            .map(|(id, _)| id.clone())
            .collect();
        let registered_parents: BTreeSet<String> = self
            .parent_tokens
            .lock()
            .map_err(|_| Self::reject("parent cancellation registry lock is poisoned"))?
            .iter()
            .filter(|(_, parent)| parent.registered)
            .map(|(id, _)| id.clone())
            .collect();
        let active_ids: BTreeSet<&str> =
            records.iter().map(|(child, _, _)| child.as_str()).collect();

        let mut candidates: Vec<SessionId> = Vec::new();
        {
            let manager = self
                .manager
                .lock()
                .map_err(|_| Self::reject("session manager lock is poisoned"))?;
            for (child, parent, status) in &records {
                let parent_alive = manager.contains(parent)
                    || active_ids.contains(parent.as_str())
                    || registered_parents.contains(parent.as_str())
                    || self
                        .external_root_parent
                        .as_ref()
                        .is_some_and(|root| &root.session_id == parent);
                let idle_cancelled =
                    cancelled.contains(child.as_str()) && *status != ChildStatus::Running;
                if !parent_alive || idle_cancelled {
                    candidates.push(child.clone());
                }
            }
            for child in tombstones {
                if manager.contains(&child) && !active_ids.contains(child.as_str()) {
                    candidates.push(child);
                }
            }
        }

        let mut released = Vec::with_capacity(candidates.len());
        for child in candidates {
            // Der In-Memory-Slot ist auch bei einem Fehler (etwa beim Schließen
            // des durablen Leases) bereits frei; der Fehler wird nur gemeldet.
            if let Err(error) = self.release_child(&child) {
                tracing::warn!(
                    child = %child,
                    error = %error,
                    "child_reaper.release_incomplete",
                );
            }
            released.push(child);
        }
        self.prune_parent_tokens();
        Ok(released)
    }

    /// Entfernt lazy angelegte Eltern-Tokens, unter denen nie mehr ein Kind
    /// admittiert werden kann (Elternteil weg, keine aktiven Kinder).
    fn prune_parent_tokens(&self) {
        let parents_in_use: BTreeSet<String> = match self.active.lock() {
            Ok(active) => active
                .values()
                .map(|record| record.parent.as_str().to_owned())
                .collect(),
            Err(_) => return,
        };
        let lazy: Vec<String> = match self.parent_tokens.lock() {
            Ok(tokens) => tokens
                .iter()
                .filter(|(id, parent)| !parent.registered && !parents_in_use.contains(*id))
                .map(|(id, _)| id.clone())
                .collect(),
            Err(_) => return,
        };
        let removable: Vec<String> = match self.manager.lock() {
            Ok(manager) => lazy
                .into_iter()
                .filter(|id| {
                    !manager.contains(&SessionId::from_str(id.as_str()))
                        && self
                            .external_root_parent
                            .as_ref()
                            .is_none_or(|root| root.session_id.as_str() != id)
                })
                .collect(),
            Err(_) => return,
        };
        if let Ok(mut tokens) = self.parent_tokens.lock() {
            for id in removable {
                tokens.remove(&id);
            }
        }
    }

    /// Meldet [`crate::guard::DriftKind::ChildOverBudget`], wenn ein
    /// abgeschlossenes Kind mehr Tokens verbraucht hat als sein
    /// Komplexitätsdeckel erlaubt (Addendum F+G). Meldet nicht blockierend —
    /// wird genau einmal aufgerufen, beim Verlassen des Managers.
    fn check_child_over_budget(&self, record: &ChildRecord, total_tokens: u64) {
        let threshold = match record.task_complexity {
            Some(TaskComplexity::Simple) => CHILD_OVER_BUDGET_SIMPLE_TOKENS,
            Some(TaskComplexity::Complex) | None => CHILD_OVER_BUDGET_COMPLEX_TOKENS,
        };
        if total_tokens <= threshold {
            return;
        }
        if let Some(observer) = &self.drift_observer {
            observer.on_drift(&crate::guard::DriftEvent {
                kind: crate::guard::DriftKind::ChildOverBudget,
                session_id: record.parent.as_str().to_owned(),
                detail: format!(
                    "child {} (role '{}') used {total_tokens} tokens, exceeding the {threshold} token budget",
                    record.child, record.role
                ),
                tool_name: None,
                child_role: Some(record.role.clone()),
            });
        }
    }

    /// In-Memory-Teil von [`Self::release_child`].
    fn release_in_memory(&self, child: &SessionId) -> Result<Option<ChildRecord>, AgentSpawnError> {
        let record = self
            .active
            .lock()
            .map_err(|_| Self::reject("child registry lock is poisoned"))?
            .remove(child.as_str());
        let tombstone = self
            .expired
            .lock()
            .map_err(|_| Self::reject("expired-child registry lock is poisoned"))?
            .remove(child.as_str());
        let token = self
            .cancellations
            .lock()
            .map_err(|_| Self::reject("child cancellation registry lock is poisoned"))?
            .remove(child.as_str());
        if let Some(token) = token {
            // Ein freigegebenes Kind darf nicht weiterlaufen; Nachkommen folgen
            // über den Token-Baum.
            token.cancel(CancelReason::Parent);
        }
        let may_be_running = tombstone.is_some()
            || record
                .as_ref()
                .is_some_and(|record| record.status == ChildStatus::Running);
        {
            let mut manager = self
                .manager
                .lock()
                .map_err(|_| Self::reject("session manager lock is poisoned"))?;
            let removed_session = manager.remove(child);
            // Addendum F+G: beim Abschluss (nicht bei jeder Freigabe — aber
            // hier ist der einzige Ort, an dem die Session noch verfügbar
            // ist, bevor sie den Manager verlässt) gegen den Rollen-/
            // Komplexitätsdeckel prüfen.
            if let (Some(session), Some(record)) = (removed_session.as_ref(), record.as_ref()) {
                self.check_child_over_budget(record, session.total_usage().total());
            }
            let removed = removed_session.is_some();
            if !removed && may_be_running {
                self.released
                    .lock()
                    .map_err(|_| Self::reject("released-child registry lock is poisoned"))?
                    .insert(child.as_str().to_owned());
            }
        }
        if record.is_some() || tombstone.is_some() {
            tracing::debug!(
                child = %child,
                status = record.as_ref().map(|record| record.status.as_str()),
                expired = tombstone.is_some(),
                "child_release.in_memory",
            );
        }
        if record.is_some() {
            // A parent's active-child slot only actually frees when an
            // `active` entry is removed (a tombstone alone does not — it was
            // already gone from `active`). Wakes every `admit_or_wait`
            // waiter; each re-checks admission itself, so a wakeup that
            // turns out to be for a different parent just costs one extra
            // poll, never an incorrect admission.
            self.freed.notify_waiters();
        }
        Ok(record)
    }

    /// Legt eine für einen Turn entnommene Session zurück oder verwirft sie,
    /// wenn das Kind während des Laufs freigegeben wurde (`manager` ⊃ `released`).
    fn return_session(&self, session: AgentSession) -> Result<SessionReturn, AgentSpawnError> {
        let child = session.id().clone();
        let mut manager = self
            .manager
            .lock()
            .map_err(|_| Self::reject("session manager lock is poisoned"))?;
        let released = self
            .released
            .lock()
            .map_err(|_| Self::reject("released-child registry lock is poisoned"))?
            .remove(child.as_str());
        if released {
            drop(manager);
            tracing::debug!(child = %child, "child_run.session_discarded_after_release");
            return Ok(SessionReturn::Discarded);
        }
        manager
            .restore(session)
            .map_err(|error| Self::reject(error.to_string()))?;
        Ok(SessionReturn::Restored)
    }

    /// Setzt den Status eines admittierten Kindes (No-op für unbekannte Kinder).
    fn set_status(&self, child: &SessionId, status: ChildStatus) {
        match self.active.lock() {
            Ok(mut active) => {
                if let Some(record) = active.get_mut(child.as_str()) {
                    record.status = status;
                }
            }
            Err(_) => tracing::warn!(
                child = %child,
                status = status.as_str(),
                "child_status.lock_poisoned",
            ),
        }
    }

    /// Markiert ein Kind als laufend; lehnt einen zweiten parallelen Lauf ab.
    fn mark_running(&self, child: &SessionId) -> Result<ChildStatus, AgentSpawnError> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| Self::reject("child registry lock is poisoned"))?;
        let record = active
            .get_mut(child.as_str())
            .ok_or_else(|| Self::reject(format!("child {child} is not admitted")))?;
        match record.status {
            ChildStatus::Running => Err(Self::reject(format!("child {child} is already running"))),
            ChildStatus::Cancelled => Err(Self::reject(format!(
                "child {child} was cancelled and cannot run again"
            ))),
            previous => {
                record.status = ChildStatus::Running;
                Ok(previous)
            }
        }
    }

    fn cancelled_error(child: &SessionId, reason: Option<CancelReason>) -> AgentSpawnError {
        Self::reject(format!(
            "child {child} was cancelled before its turn completed (reason: {reason:?})"
        ))
    }

    #[must_use]
    pub fn child_record(&self, child: &SessionId) -> Option<ChildRecord> {
        self.active
            .lock()
            .ok()
            .and_then(|active| active.get(child.as_str()).cloned())
    }

    /// Deckelt die zulässige Fan-out-Parallelität für einen **registrierten
    /// Rollennamen** — unabhängig vom aufrufer-seitigen `max_parallel`.
    ///
    /// # Beschreibung
    /// Lokales Äquivalent zu
    /// `harw_runtime::children::max_concurrent_instances_for_role`, hier aber
    /// **nicht** durch Aufruf jener Funktion implementiert: `harw-runtime`
    /// hängt von `harw-core` ab (siehe `harw-runtime/Cargo.toml`,
    /// `harw-core = { path = "../harw-core" }`), nie umgekehrt — ein Aufruf
    /// von hier aus wäre eine zirkuläre Crate-Abhängigkeit und würde nicht
    /// kompilieren. Diese Methode zieht dieselbe Schlussfolgerung
    /// (Organisationsrolle [`harw_agent_dsl::roles::AgentRoleId::UiaWorker`]
    /// ⇒ höchstens eine gleichzeitige Instanz) unabhängig, aus der lokal
    /// bereits vorhandenen Quelle: [`Self::roles`]
    /// (`ChildRoleDefinition::organizational_role`), derselben Zuordnung, die
    /// [`Self::run_child_with_approvals`] wenige Zeilen weiter unten für den
    /// Modell-Lookup liest und die [`Self::with_role`] beim Registrieren
    /// jeder Rolle bindet.
    ///
    /// Aufrufer außerhalb dieser Crate (z. B.
    /// `harw-core-bridge::agent_tool::fanout_children`) erhalten hier absichtlich
    /// nur ein `usize` zurück statt der
    /// [`harw_agent_dsl::roles::AgentRoleId`] selbst: `harw-core-bridge` führt
    /// `harw-agent-dsl` nur als `[dev-dependencies]`
    /// (`harw-core-bridge/Cargo.toml:24-28`), nicht als produktive
    /// Abhängigkeit — der Rollen-Enum-Typ ist dort im produktiven Build gar
    /// nicht benennbar. Diese Methode kapselt die Fallunterscheidung deshalb
    /// vollständig in `harw-core`.
    ///
    /// # Arguments
    /// - `role` (`&str`): der exakte registrierte Rollenname (`self.roles`-Schlüssel).
    ///
    /// # Returns
    /// `1`, wenn `role` registriert ist und ihre Organisationsrolle
    /// [`harw_agent_dsl::roles::AgentRoleId::UiaWorker`] ist; sonst
    /// `usize::MAX` — das schließt eine **unbekannte** Rolle ein (fail-open
    /// für die Deckelung selbst: eine nicht registrierte Rolle scheitert
    /// ohnehin kurz danach an der eigentlichen Admission/Ausführung mit einem
    /// eigenen Fehler, siehe [`Self::run_child_with_approvals`]).
    ///
    /// # Concurrency
    /// Nimmt keinen Lock: `self.roles` ist ein einfaches `BTreeMap`, nicht
    /// hinter einem `Mutex`, und nach der Konstruktion unveränderlich (nur
    /// [`Self::with_role`] schreibt, als Builder-Methode vor jeder
    /// Nebenläufigkeit). Sicher aus jedem Thread aufrufbar.
    #[must_use]
    pub fn max_concurrent_instances_for_role(&self, role: &str) -> usize {
        match self
            .roles
            .get(role)
            .map(|definition| definition.organizational_role)
        {
            Some(harw_agent_dsl::roles::AgentRoleId::UiaWorker) => 1,
            _ => usize::MAX,
        }
    }

    /// Liefert die organisatorische Rolle (§3 DSL-Spawn-Matrix) der Session,
    /// die `child` delegiert hat.
    ///
    /// # Beschreibung
    /// Ergänzung für die nicht mehr blockierende UIA-Delegation (Development-
    /// Auftrag "UIA-Delegation asynchron"): eine Approval-Schicht muss vor dem
    /// Antreiben eines Kindes wissen, ob dessen Elternsitzung die
    /// `UserInterface`-Rolle trägt, ohne dafür `Self::admit`s interne
    /// Prüfungen zu duplizieren. Liest denselben vertrauenswürdigen
    /// `SpawnContext`, den `Self::admit` bereits für `can_delegate_to`
    /// konsultiert — kein zweiter, potenziell abweichender Quelltext für
    /// dieselbe Information.
    ///
    /// # Argumente
    /// - `child` (`&SessionId`): das admittierte Kind, dessen Elternteil
    ///   befragt wird.
    ///
    /// # Rückgabe
    /// `Some(role)`, wenn `child` bekannt ist und sein Elternteil (noch)
    /// einen vertrauenswürdigen `SpawnContext` trägt (laufende Session oder
    /// der externe Wurzel-Elternteil). `None`, wenn das Kind unbekannt ist,
    /// der Elternteil nicht (mehr) auffindbar ist oder der Manager-Lock
    /// vergiftet ist — in jedem dieser Fälle behandelt der Aufrufer die
    /// Delegation konservativ wie eine Nicht-UIA-Delegation (unverändertes,
    /// synchrones Verhalten).
    ///
    /// # Panics
    /// Nie.
    ///
    /// # Nebenläufigkeit
    /// Nimmt kurz den `active`-Lock (über [`Self::child_record`]) und danach
    /// kurz den `manager`-Lock, jeweils nur für die Dauer der Abfrage; keine
    /// Sperre wird über den Rückgabewert hinaus gehalten oder über ein
    /// `.await` gehalten. Sicher aus mehreren Threads aufrufbar.
    ///
    /// # Examples
    /// ```rust,no_run
    /// # fn demo(spawner: &harw_core::child_controller::ManagedAgentSpawner, child: &harw_types::SessionId) {
    /// if let Some(role) = spawner.parent_organizational_role(child) {
    ///     // role == harw_agent_dsl::roles::AgentRoleId::UserInterface → UIA-Delegation
    ///     let _ = role;
    /// }
    /// # }
    /// ```
    #[must_use]
    pub fn parent_organizational_role(
        &self,
        child: &SessionId,
    ) -> Option<harw_agent_dsl::roles::AgentRoleId> {
        let record = self.child_record(child)?;
        if let Ok(manager) = self.manager.lock()
            && let Ok(parent) = manager.get(&record.parent)
            && let Some(context) = parent.spawn_context()
        {
            return Some(context.organizational_role);
        }
        self.external_root_parent
            .as_ref()
            .filter(|root| root.session_id == record.parent)
            .map(|root| root.spawn_context.organizational_role)
    }

    /// Liefert die organisatorische Rolle (§3 DSL-Spawn-Matrix) von `session`
    /// selbst — anders als [`Self::parent_organizational_role`], das die
    /// Rolle des Elternteils eines bereits admittierten Kindes liest, wird
    /// hier direkt die übergebene Session befragt.
    ///
    /// # Beschreibung
    /// Ergänzung für Aufrufstellen, die eine Zielrolle wählen müssen, bevor
    /// ein Kind überhaupt existiert (z. B. `/explore`/`/research-web` in
    /// `harw-ops`, die vor dem Spawn wissen müssen, ob die AUFRUFENDE Session
    /// die `UserInterface`-Rolle trägt). Liest dieselbe vertrauenswürdige
    /// Quelle wie `Self::admit` und `Self::parent_organizational_role`: den
    /// `SpawnContext` einer laufenden Manager-Session, oder — für die
    /// externe Wurzelsitzung ohne Manager-Spiegel — den `SpawnContext` aus
    /// `external_root_parent`. Damit funktioniert die Methode auch für die
    /// UIA-Root-Session, für die es (noch) keinen `ChildRecord` gibt.
    ///
    /// # Argumente
    /// - `session` (`&SessionId`): die zu befragende Session (laufendes Kind
    ///   oder die externe Root-Session).
    ///
    /// # Rückgabe
    /// `Some(role)`, wenn `session` eine laufende Manager-Session mit
    /// `SpawnContext` ist oder mit der externen Root-Session übereinstimmt;
    /// sonst `None` — in jedem dieser Fälle behandelt der Aufrufer die
    /// Zielwahl konservativ (unverändertes Verhalten ohne UIA-Rolle).
    ///
    /// # Panics
    /// Nie.
    ///
    /// # Nebenläufigkeit
    /// Nimmt kurz den `manager`-Lock, nur für die Dauer der Abfrage; keine
    /// Sperre wird über den Rückgabewert hinaus gehalten. Sicher aus
    /// mehreren Threads aufrufbar.
    ///
    /// # Examples
    /// ```rust,no_run
    /// # fn demo(spawner: &harw_core::child_controller::ManagedAgentSpawner, session: &harw_types::SessionId) {
    /// if let Some(role) = spawner.session_organizational_role(session) {
    ///     // role == harw_agent_dsl::roles::AgentRoleId::UserInterface → UIA-Zielwahl
    ///     let _ = role;
    /// }
    /// # }
    /// ```
    #[must_use]
    pub fn session_organizational_role(
        &self,
        session: &SessionId,
    ) -> Option<harw_agent_dsl::roles::AgentRoleId> {
        if let Ok(manager) = self.manager.lock()
            && let Ok(entry) = manager.get(session)
            && let Some(context) = entry.spawn_context()
        {
            return Some(context.organizational_role);
        }
        self.external_root_parent
            .as_ref()
            .filter(|root| &root.session_id == session)
            .map(|root| root.spawn_context.organizational_role)
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
    /// without receiving session or history access. The returned text is
    /// capped to [`CHILD_RETURN_MAX_BYTES`] via [`cap_child_return_text`] —
    /// the full, uncapped text stays available to anything that persists it
    /// independently (e.g. the child's own transcript/state store); this
    /// method only shrinks what is handed back to the parent.
    ///
    /// Für typisierte Rückgabe-Verträge liefert
    /// [`Self::child_final_assistant_text_full`] (bzw.
    /// [`ChildRunResult::full_text`]) denselben Text ungekürzt.
    ///
    /// # Errors
    /// Returns [`AgentSpawnError`] when the child is not admitted, its restored
    /// session is unavailable, or its history contains no assistant text.
    pub fn child_final_assistant_text(&self, child: &SessionId) -> Result<String, AgentSpawnError> {
        let text = self.child_final_assistant_text_full(child)?;
        Ok(cap_child_return_text(&text, CHILD_RETURN_MAX_BYTES))
    }

    /// Liefert die neueste Text-Antwort eines admittierten Kindes
    /// **ungekürzt**.
    ///
    /// # Beschreibung
    /// Gleiche Quelle und gleiche Fehler wie
    /// [`Self::child_final_assistant_text`], aber ohne
    /// [`cap_child_return_text`]. Gedacht für typisierte Rückgabe-Verträge,
    /// deren Parser an einem gekürzten JSON fälschlich scheitern würden; eine
    /// Freitext-Rückgabe an den Elternteil muss der Aufrufer selbst kappen.
    ///
    /// # Errors
    /// [`AgentSpawnError`], wenn das Kind nicht admittiert ist, seine Session
    /// nicht verfügbar ist oder sein Verlauf keinen Antworttext enthält.
    pub fn child_final_assistant_text_full(
        &self,
        child: &SessionId,
    ) -> Result<String, AgentSpawnError> {
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
        let text: String = session
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
            .ok_or_else(|| Self::reject(format!("child {child} has no assistant response text")))?;
        Ok(text)
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

    /// Lists every admitted descendant of `parent` in parent-before-child
    /// order. This is the read counterpart to [`Self::owns_descendant`] for
    /// `/agent list`; it exposes only the caller's own tree.
    #[must_use]
    pub fn list_descendants_for(&self, parent: &SessionId) -> Vec<ChildRecord> {
        let Ok(active) = self.active.lock() else {
            return Vec::new();
        };
        let mut pending = vec![parent.clone()];
        let mut seen = std::collections::HashSet::new();
        let mut descendants = Vec::new();
        while let Some(current) = pending.pop() {
            let mut children = active
                .values()
                .filter(|record| record.parent == current)
                .cloned()
                .collect::<Vec<_>>();
            children.sort_by_key(|record| std::cmp::Reverse(record.admitted_at));
            for child in children.into_iter().rev() {
                if seen.insert(child.child.as_str().to_owned()) {
                    pending.push(child.child.clone());
                    descendants.push(child);
                }
            }
        }
        descendants
    }

    /// Returns whether `target` is an admitted descendant of `ancestor`.
    ///
    /// This is the controller-owned authority check for product surfaces such
    /// as `/agent stop`: a caller may address any node in its own subtree, but
    /// can never use a guessed sibling or foreign session id. The lookup is
    /// read-only and cycle-safe even if a malformed restored registry were to
    /// contain a parent loop.
    #[must_use]
    pub fn owns_descendant(&self, ancestor: &SessionId, target: &SessionId) -> bool {
        let Ok(active) = self.active.lock() else {
            return false;
        };
        let mut cursor = target.as_str().to_owned();
        let mut seen = std::collections::HashSet::new();
        while seen.insert(cursor.clone()) {
            let Some(record) = active.get(&cursor) else {
                return false;
            };
            if &record.parent == ancestor {
                return true;
            }
            cursor = record.parent.as_str().to_owned();
        }
        false
    }

    /// Resolves the root session that owns `session`'s active subtree.
    ///
    /// A root is the first parent absent from the admitted-child registry;
    /// this covers both manager-owned roots and the explicitly registered
    /// external TUI/CLI root without adding mutable lineage to
    /// [`SpawnContext`]. `None` means the registry is poisoned or a parent
    /// cycle exists. The loop guard makes malformed restored
    /// parent cycles fail closed.
    #[must_use]
    pub fn root_for(&self, session: &SessionId) -> Option<SessionId> {
        let active = self.active.lock().ok()?;
        Self::root_from_active(&active, session)
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
    /// Since A-CHILD the signal is the child's [`CancelToken`] (reason
    /// [`CancelReason::User`]); it also cancels every descendant. Cancellation
    /// is terminal: the child is never run again and the reaper
    /// ([`Self::reap`], or admission at the parent's limit) releases it once
    /// its turn has unwound.
    ///
    /// # Returns
    /// `true` when `child` is currently admitted and has a live cancellation
    /// token to signal; `false` when `child` is unknown or already released,
    /// so the caller can report that no running turn was found.
    pub fn request_cancellation(&self, child: &SessionId) -> bool {
        self.request_cancellation_with_reason(child, CancelReason::User)
    }

    /// Wie [`Self::request_cancellation`], aber mit explizitem [`CancelReason`].
    ///
    /// # Description
    /// Budget-Abbrüche nutzen [`CancelReason::Budget`], die AnyTerminal-Welle
    /// [`CancelReason::Parent`] (der Orchestrator bricht Geschwister ab). Ein
    /// nicht laufendes, noch nicht abgeschlossenes Kind (`Admitted`/`Paused`)
    /// wechselt sofort auf [`ChildStatus::Cancelled`]; ein laufendes Kind
    /// bekommt den Status, sobald sein Turn den Abbruch sieht. `Completed`
    /// bleibt `Completed` — das Ergebnis war echt.
    ///
    /// # Arguments
    /// - `child` (`&SessionId`): das Kind.
    /// - `reason` (`CancelReason`): der Abbruchgrund (erster Grund gewinnt).
    ///
    /// # Returns
    /// `true`, wenn ein Token abgebrochen wurde.
    ///
    /// # Concurrency
    /// Kurze, nacheinander genommene Sperren; idempotent.
    pub fn request_cancellation_with_reason(
        &self,
        child: &SessionId,
        reason: CancelReason,
    ) -> bool {
        if self.child_record(child).is_none() {
            return false;
        }
        let Some(token) = self.child_cancel_token(child) else {
            return false;
        };
        token.cancel(reason);
        if let Ok(mut active) = self.active.lock() {
            if let Some(record) = active.get_mut(child.as_str()) {
                if matches!(record.status, ChildStatus::Admitted | ChildStatus::Paused) {
                    record.status = ChildStatus::Cancelled;
                }
            }
        }
        true
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
        if !expired.is_empty() {
            // Each expired child freed its parent's active-child slot; see
            // `release_in_memory`'s `self.freed.notify_waiters()` for why
            // this wakes `admit_or_wait` waiters rather than requiring them
            // to sleep out their full retry window.
            self.freed.notify_waiters();
        }
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
        if !expired.is_empty() {
            // See `reap_expired`: each claimed lease freed its parent's
            // active-child slot.
            self.freed.notify_waiters();
        }
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
        for record in expired {
            self.forget_child_state(&record.child);
        }
        if let Ok(mut tombstones) = self.expired.lock() {
            for record in expired {
                tombstones.insert(record.child.as_str().to_owned(), record.clone());
            }
        }
        if let Ok(cancellations) = self.cancellations.lock() {
            for record in expired {
                if let Some(token) = cancellations.get(record.child.as_str()) {
                    token.cancel(CancelReason::LeaseLost);
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
        // Addendum F+G: je abgelaufenem Kind ein ChildLeaseExpired-Ereignis.
        if let Some(observer) = &self.drift_observer {
            for record in expired {
                observer.on_drift(&crate::guard::DriftEvent {
                    kind: crate::guard::DriftKind::ChildLeaseExpired,
                    session_id: record.parent.as_str().to_owned(),
                    detail: format!(
                        "child {} (role '{}') lease expired at {}",
                        record.child, record.role, record.expired_at
                    ),
                    tool_name: None,
                    child_role: Some(record.role.clone()),
                });
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
        // Budget-Anrechnung: Ausgangsstand vor dem Lauf. Solange das Kind
        // nicht läuft, liegt seine Session im Manager; fehlt sie, zählt der
        // Lauf ab 0 (der Lauf selbst scheitert dann ohnehin).
        let baseline_tokens = self.child_token_usage(child).unwrap_or(0);
        let baseline_tool_calls = self.child_tool_call_count(child).unwrap_or(0);
        let started = std::time::Instant::now();
        let result = self
            .enforce_child_budget(child, store, approvals, input, budget, started)
            .await;
        // Auch ein gescheiterter oder über das Budget gelaufener Lauf hat
        // verbraucht — die Anrechnung erfolgt auf jedem Ausgang. Ist die
        // Session inzwischen verworfen, gilt der Ausgangsstand (Differenz 0).
        let usage = ChildUsage {
            tokens: self
                .child_token_usage(child)
                .unwrap_or(baseline_tokens)
                .saturating_sub(baseline_tokens),
            tool_calls: self
                .child_tool_call_count(child)
                .unwrap_or(baseline_tool_calls)
                .saturating_sub(baseline_tool_calls),
            wall_time_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        };
        self.charge_run_usage(child, usage);
        result.map(|mut run| {
            run.usage = usage;
            run
        })
    }

    /// Rechnet den Verbrauch eines Laufs dem Kind an und reicht die noch
    /// nicht verrechnete Differenz an den direkten Elternteil weiter.
    ///
    /// # Beschreibung
    /// Die Weitergabe geht genau **eine** Ebene nach oben: der Elternteil
    /// verrechnet seinerseits erst, wenn sein eigener Lauf endet oder er
    /// freigegeben wird. Ein Elternteil ohne Record (Wurzel) wird nicht
    /// belastet. Unbekannte Kinder sind ein No-op.
    ///
    /// # Concurrency
    /// Nimmt kurz den `active`-Lock.
    fn charge_run_usage(&self, child: &SessionId, run: ChildUsage) {
        let Ok(mut active) = self.active.lock() else {
            tracing::warn!(child = %child, "child_budget.charge_lock_poisoned");
            return;
        };
        let Some(record) = active.get_mut(child.as_str()) else {
            return;
        };
        record.consumed = record.consumed.saturating_add(run);
        let delta = record.consumed.saturating_sub(record.charged_to_parent);
        record.charged_to_parent = record.consumed;
        let parent = record.parent.as_str().to_owned();
        if let Some(parent_record) = active.get_mut(&parent) {
            parent_record.consumed = parent_record.consumed.saturating_add(delta);
        }
    }

    /// Rechnet einem Elternteil den noch offenen Verbrauch eines eben
    /// freigegebenen Kindes an (Nachkommen, die erst nach dem letzten Lauf
    /// des Kindes abgeschlossen wurden).
    fn charge_released_child(&self, record: &ChildRecord) {
        let delta = record.consumed.saturating_sub(record.charged_to_parent);
        if delta == ChildUsage::default() {
            return;
        }
        match self.active.lock() {
            Ok(mut active) => {
                if let Some(parent) = active.get_mut(record.parent.as_str()) {
                    parent.consumed = parent.consumed.saturating_add(delta);
                }
            }
            Err(_) => tracing::warn!(child = %record.child, "child_budget.charge_lock_poisoned"),
        }
    }

    /// Restbudget einer admittierten Sitzung: ihr Budget-Deckel abzüglich
    /// des bisher angerechneten Verbrauchs (eigener plus Nachkommen).
    ///
    /// # Argumente
    /// - `session` (`&SessionId`): ein admittiertes Kind.
    ///
    /// # Returns
    /// `Some(AgentBudget)` mit dimensionsweise abgezogenem Verbrauch (nicht
    /// gesetzte Dimensionen bleiben nicht gesetzt, gesetzte sättigen bei 0);
    /// `None` für unbekannte Sitzungen (auch Wurzeln ohne Record) oder bei
    /// vergifteter Registry-Sperre.
    ///
    /// # Concurrency
    /// Nimmt kurz den `active`-Lock.
    #[must_use]
    pub fn remaining_budget(&self, session: &SessionId) -> Option<AgentBudget> {
        self.child_record(session)
            .map(|record| budget_minus_usage(record.budget, record.consumed))
    }

    /// Setzt `budget` für einen Lauf durch (Kern von
    /// [`Self::run_child_with_budget`], ohne Verbrauchsanrechnung).
    async fn enforce_child_budget(
        &self,
        child: &SessionId,
        store: &dyn StateStore,
        approvals: Option<&ApprovalStore>,
        input: TurnInput,
        budget: AgentBudget,
        started: std::time::Instant,
    ) -> Result<ChildRunResult, AgentSpawnError> {
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
                        if self.request_cancellation_with_reason(child, CancelReason::Budget) {
                            // Der Turn sieht den Abbruch und legt die Session
                            // regulär zurück; sein Ergebnis ist bedeutungslos.
                            let _discarded = turn.await;
                        } else {
                            // Kein Token mehr (Kind bereits freigegeben): nicht
                            // unbegrenzt warten. Das Droppen ist sicher, weil
                            // der `RunningSession`-Guard die Session zurücklegt
                            // bzw. verwirft.
                            tracing::warn!(
                                child = %child,
                                "child_budget.cancel_token_missing",
                            );
                            drop(turn);
                        }
                        let used_ms =
                            u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                        let error =
                            Self::budget_exceeded(BudgetDimension::WallTime, limit_ms, used_ms);
                        self.set_failed(child, &error.message);
                        tracing::warn!(
                            child = %child,
                            dimension = BudgetDimension::WallTime.as_str(),
                            limit = limit_ms,
                            used = used_ms,
                            "child_budget.exceeded",
                        );
                        return Err(error);
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
                let error = Self::budget_exceeded(
                    BudgetDimension::ToolCalls,
                    u64::from(limit),
                    u64::from(used),
                );
                self.set_failed(child, &error.message);
                return Err(error);
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
                let error = Self::budget_exceeded(BudgetDimension::Tokens, limit, used);
                self.set_failed(child, &error.message);
                return Err(error);
            }
        }
        Ok(outcome)
    }

    /// Deckelt einen bereits auf `max(1)` angehobenen Slot-Wunsch auf die
    /// strengste Rollen-Grenze, die in `requests` vorkommt.
    ///
    /// # Beschreibung
    /// Löst für jede Anfrage die Organisationsrolle ihres bereits
    /// admittierten Kindes auf ([`Self::child_record`] → `role`-Name →
    /// [`Self::max_concurrent_instances_for_role`], dieselbe Quelle, die
    /// [`Self::run_child_with_approvals`] für den Modell-Lookup liest) und
    /// bildet das **Minimum** über alle Anfragen. Mischt eine Welle mehrere
    /// verschiedene Rollen (nicht nur eine einzige `uia-worker`-Rolle), gibt
    /// es dafür keine Pro-Rolle-Slotzahl — [`Self::run_children`]s Scheduler
    /// kennt nur einen einzigen globalen `slots`-Wert für die ganze Welle
    /// (siehe die `VecDeque<(usize, FanoutRequest)>`/`running: Vec<...>`-Struktur
    /// dort). Deshalb gilt hier bewusst die strengste Deckelung über alle in
    /// der Welle vorkommenden Rollen: eine Welle, die auch nur eine
    /// `UiaWorker`-Anfrage enthält, wird als Ganzes auf `1` gedeckelt, selbst
    /// wenn andere Rollen derselben Welle für sich unbeschränkt wären. Das
    /// ist strenger als für die Nicht-`UiaWorker`-Geschwister nötig, aber
    /// sicherer als eine Welle mit zwei UiaWorker-Instanzen gleichzeitig
    /// laufen zu lassen.
    ///
    /// Ein Kind, dessen [`Self::child_record`] nicht (mehr) auflösbar ist
    /// (z. B. bereits entfernt), trägt fail-**offen** keine zusätzliche
    /// Deckelung bei — seine eigentliche Ausführung scheitert ohnehin kurz
    /// danach in [`Self::run_child_with_approvals`] mit einem eigenen,
    /// aussagekräftigen Fehler.
    ///
    /// # Arguments
    /// - `requests` (`&[FanoutRequest]`): die Welle, vor dem Verbrauch in die
    ///   Scheduler-Queue.
    /// - `requested_slots` (`usize`): der bereits auf `max(1)` angehobene
    ///   Aufrufer-Wunsch.
    ///
    /// # Returns
    /// `requested_slots`, oder eine kleinere Zahl, wenn mindestens eine
    /// Anfrage einer Rolle mit einer strengeren
    /// [`Self::max_concurrent_instances_for_role`]-Grenze zugeordnet ist.
    fn effective_fanout_slots(&self, requests: &[FanoutRequest], requested_slots: usize) -> usize {
        requests.iter().fold(requested_slots, |slots, request| {
            let role_cap = self
                .child_record(&request.child)
                .map_or(usize::MAX, |record| {
                    self.max_concurrent_instances_for_role(&record.role)
                });
            slots.min(role_cap)
        })
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
    /// - Verschachtelt gehalten wird nur `manager` ⊃ `released` (Rückgabe
    ///   einer Session), immer in dieser Richtung; alle übrigen Pfade nehmen
    ///   ihre Sperren nacheinander. `admit` selbst hält `active` und
    ///   `cancellations` durchgehend bis zum Eintrag des Kindes (siehe die
    ///   Moduldoku „Nebenläufigkeit"), verschachtelt sie dabei aber mit
    ///   keiner weiteren Sperre dieser Methode. Damit gibt es keine Inversion
    ///   und keinen Deadlock zwischen Fan-out und Admission.
    /// - Wird ein Kind-Future gedroppt, legt ein Drop-Guard die Session als
    ///   gescheitert zurück (A-CHILD) — sie geht nicht mehr verloren.
    ///
    /// ## Join-Semantik
    /// - [`JoinSemantics::AnyTerminal`]: Sobald ein Kind `Ok` mit
    ///   `TurnOutcome::Completed` liefert (F-182: eine zulässige Pause gewinnt
    ///   nicht), wird für
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
    /// - `max_parallel` (`usize`): vom Aufrufer gewünschter Deckel gleichzeitig
    ///   laufender Kinder. **Kann nicht** eine `uia-worker`-Rollenfamilie
    ///   (Organisationsrolle
    ///   [`harw_agent_dsl::roles::AgentRoleId::UiaWorker`]) über eine
    ///   Instanz hinaus parallelisieren — siehe
    ///   [`Self::effective_fanout_slots`]: sobald mindestens eine Anfrage
    ///   dieser Welle einer solchen Rolle zugeordnet ist, wird die
    ///   **gesamte** Welle auf `1` gedeckelt, auch wenn andere Rollen
    ///   derselben Welle für sich unbeschränkt wären (`slots` ist heute ein
    ///   einziger globaler Wert für die ganze Welle, kein Wert je Rolle).
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
        let requested_slots = if max_parallel == 0 { 1 } else { max_parallel };
        let slots = self.effective_fanout_slots(&requests, requested_slots);
        if slots < requested_slots {
            tracing::info!(
                requested_max_parallel = requested_slots,
                effective_max_parallel = slots,
                "child_fanout.uia_worker_capped",
            );
        }
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
            // F-182: nur ein echt abgeschlossenes Kind gewinnt die Welle. Ein
            // zulässig pausiertes Ergebnis ist zwar `Ok`, aber kein Endergebnis
            // und darf die Geschwister nicht abbrechen.
            let completed = matches!(
                &value,
                Ok(ChildRunResult {
                    outcome: TurnOutcome::Completed,
                    ..
                })
            );
            if !winner_decided && matches!(join, JoinSemantics::AnyTerminal) && completed {
                winner_decided = true;
                for (_, sibling, _) in &running {
                    if !self.request_cancellation_with_reason(sibling, CancelReason::Parent) {
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
        mut input: TurnInput,
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
        let model = factory.model_for_task(&record.role, record.task_complexity)?;
        let token = self
            .child_cancel_token(child)
            .ok_or_else(|| Self::reject(format!("child {child} has no cancellation token")))?;
        // Cancel ist terminal (F-182): ein abgebrochenes Kind wird nicht erneut
        // gestartet, sondern sofort abgewiesen und später vom Reaper freigegeben.
        if token.is_cancelled() {
            self.set_status(child, ChildStatus::Cancelled);
            return Err(Self::cancelled_error(child, token.reason()));
        }
        // Erst `Running` markieren (unter `active`), dann die Session entnehmen:
        // `release_in_memory` liest den Status in derselben Reihenfolge und setzt
        // nur dann einen Freigabe-Tombstone, wenn ein Lauf die Session halten kann.
        let previous_status = self.mark_running(child)?;
        if let (Some(root), Some(record)) = (self.root_for(child), self.child_record(child)) {
            self.observe_orchestration(root, &record, None, AgentOrchestrationStatus::Running);
        }
        let session = match self.manager.lock() {
            Ok(mut manager) => manager.remove(child),
            Err(_) => {
                self.set_status(child, previous_status);
                return Err(Self::reject("session manager lock is poisoned"));
            }
        };
        let Some(session) = session else {
            self.set_status(child, previous_status);
            return Err(Self::reject(format!(
                "child session {child} is not available"
            )));
        };
        let mut running = RunningSession {
            spawner: self,
            child: child.clone(),
            session: Some(session),
        };
        // Der bei der Admission hinterlegte Auftrag wird genau einmal
        // verbraucht: vom ersten Lauf. Bringt dieser keinen eigenen User-Text
        // mit (z. B. `TurnInput::default()`), wird der Auftrag sein User-Turn;
        // der Steuerblock des Aufrufers bleibt erhalten.
        let pending_task = self.take_pending_task(child);
        let has_user_text = input
            .user_text
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty());
        if !has_user_text && let Some(task) = pending_task {
            input.user_text = Some(task);
        }

        let turn = {
            let session = running.session_mut()?;
            tokio::select! {
                biased;
                () = token.cancelled() => Err(Self::cancelled_error(child, token.reason())),
                outcome = async {
                    match approvals {
                        Some(approvals) => run_turn_durable(session, model.as_ref(), store, approvals, input).await,
                        None => run_turn(session, model.as_ref(), store, input).await,
                    }
                    .map_err(|error| Self::reject(error.to_string()))
                } => outcome,
            }
        };
        let expired = self
            .expired
            .lock()
            .map_err(|_| Self::reject("expired-child registry lock is poisoned"))?
            .get(child.as_str())
            .cloned();
        if expired.is_some() {
            running.fail_session("child lease expired while its turn was still running");
        }
        let returned = running.finish()?;
        if let Some(expired) = expired {
            return Err(Self::reject(format!(
                "child {child} completed after lease expiry at {}; result discarded",
                expired.expired_at
            )));
        }
        if returned == SessionReturn::Discarded {
            return Err(Self::reject(format!(
                "child {child} was released while its turn was running; result discarded"
            )));
        }
        let outcome = match turn {
            Ok(outcome) => outcome,
            Err(error) => {
                // Nur echter Abschluss ist `Completed`: Abbruch und Fehler
                // werden unterschieden und nie als Erfolg verbucht.
                if token.is_cancelled() {
                    self.set_status(child, ChildStatus::Cancelled);
                } else {
                    self.set_failed(child, &error.message);
                }
                return Err(error);
            }
        };
        // W2-19, fail-closed: ein Kind, dessen Lebenszyklus das Pausieren
        // verbietet, hat keinen Kanal, über den eine Freigabe je eintreffen
        // könnte. Die Session ist zu diesem Zeitpunkt bereits regulär
        // restauriert — nur das Ergebnis wird abgewiesen.
        // Das Label wird vor dem `match` gebunden, damit die Leihgabe an
        // `outcome` endet, bevor der Erfolgsarm es in das Ergebnis verschiebt.
        let pause_label = Self::pause_label(&outcome);
        match pause_label {
            Some(label) if !record.allow_pause => {
                let reason = format!("child paused but its lifecycle forbids pausing: {label}");
                self.set_failed(child, &reason);
                Err(Self::reject(reason))
            }
            Some(_) => {
                self.set_status(child, ChildStatus::Paused);
                Ok(ChildRunResult {
                    child: child.clone(),
                    outcome,
                    full_text: None,
                    usage: ChildUsage::default(),
                })
            }
            None => {
                // Ungekürzt für typisierte Verträge; fehlt Antworttext, bleibt
                // `full_text` leer (der Aufrufer fällt dann auf
                // `child_final_assistant_text` und dessen Fehler zurück).
                let full_text = match &outcome {
                    TurnOutcome::Completed => self.child_final_assistant_text_full(child).ok(),
                    _ => None,
                };
                match (&outcome, full_text.as_deref()) {
                    (_, Some(text)) => self.set_outcome_detail(child, text),
                    (TurnOutcome::Failed { reason }, None) => {
                        self.set_outcome_detail(child, reason);
                    }
                    (
                        TurnOutcome::Refused {
                            detail: Some(detail),
                        },
                        None,
                    ) => self.set_outcome_detail(child, detail),
                    _ => {}
                }
                self.set_status(child, ChildStatus::Completed);
                Ok(ChildRunResult {
                    child: child.clone(),
                    outcome,
                    full_text,
                    usage: ChildUsage::default(),
                })
            }
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
            TurnOutcome::Cancelled { .. } => None,
            TurnOutcome::Truncated => None,
            TurnOutcome::Refused { .. } => None,
            TurnOutcome::Failed { .. } => None,
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
                !SectionName::try_new(*name)
                    .is_ok_and(|section| ceiling.sections.contains(&section))
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

    /// Kern von [`AgentSpawner::delegation_target_names`] für
    /// [`ManagedAgentSpawner`] (Addendum F+G, Nachtrag F).
    ///
    /// # Beschreibung
    /// Holt den Eltern-`SpawnContext` genau wie [`Self::admit`] (Manager
    /// bzw. `external_root_parent`), baut die Kandidatenliste aus allen
    /// registrierten Rollen (Name + `organizational_role`) und berechnet die
    /// verbleibende Tiefe als `depth_ceiling(parent) − depth(parent)` —
    /// dieselbe Größe, gegen die [`Self::admit`] die Tiefe eines
    /// tatsächlichen Kindes prüft. Delegiert die eigentliche Projektion an
    /// [`crate::delegation_visibility::visible_delegation_targets`].
    ///
    /// # Errors
    /// [`AgentSpawnError`] bei unbekanntem Elternteil oder vergifteter
    /// Sperre; der öffentliche Trait-Pfad übersetzt das in eine leere Liste.
    fn visible_delegation_target_names(
        &self,
        parent_session_id: &SessionId,
    ) -> Result<Vec<String>, AgentSpawnError> {
        let manager = self
            .manager
            .lock()
            .map_err(|_| Self::reject("session manager lock is poisoned"))?;
        let (caller_role, allowed_child_orchestrators, parent_depth) =
            match manager.get(parent_session_id) {
                Ok(parent) => {
                    let context = parent.spawn_context().ok_or_else(|| {
                        Self::reject("delegation caller has no trusted sandbox context")
                    })?;
                    let depth = Self::parent_depth(&manager, parent_session_id)?;
                    (
                        context.organizational_role,
                        context.allowed_child_orchestrators.clone(),
                        depth,
                    )
                }
                Err(_) => {
                    let external_root = self
                        .external_root_parent
                        .as_ref()
                        .filter(|root| &root.session_id == parent_session_id)
                        .ok_or_else(|| {
                            Self::reject(format!("unknown delegation caller: {parent_session_id}"))
                        })?;
                    (
                        external_root.spawn_context.organizational_role,
                        external_root
                            .spawn_context
                            .allowed_child_orchestrators
                            .clone(),
                        0,
                    )
                }
            };
        drop(manager);
        let inherited_depth_ceiling = self
            .active
            .lock()
            .map_err(|_| Self::reject("child registry lock is poisoned"))?
            .get(parent_session_id.as_str())
            .map_or(self.limits.max_depth, |record| record.depth_ceiling);
        let remaining_depth = inherited_depth_ceiling.saturating_sub(parent_depth);
        let candidates: Vec<(String, harw_agent_dsl::roles::AgentRoleId)> = self
            .roles
            .iter()
            .map(|(name, definition)| (name.clone(), definition.organizational_role))
            .collect();
        let targets = crate::delegation_visibility::visible_delegation_targets(
            caller_role,
            &candidates,
            &allowed_child_orchestrators,
            remaining_depth,
        );
        Ok(targets.into_iter().map(|target| target.name).collect())
    }

    /// Admits a child. Unchanged public behavior and error messages — a thin
    /// wrapper over [`Self::admit_inner`] that discards the
    /// capacity-vs-other distinction [`Self::admit_or_wait`] needs.
    ///
    /// # Errors
    /// See [`Self::admit_inner`]; the returned [`AgentSpawnError`] is
    /// byte-identical either way.
    fn admit(
        &self,
        role_name: &str,
        input: SpawnInput,
        sandbox: SandboxSpec,
        suggestions: Option<AgentSuggestions>,
    ) -> Result<SessionId, AgentSpawnError> {
        self.admit_inner(role_name, input, sandbox, suggestions)
            .map_err(AdmitRejection::into_error)
    }

    /// Core admission logic (role, spawn-matrix, capacity, sandbox, depth,
    /// registry, lease, cancellation wiring). Identical to what `admit` did
    /// before [`Self::admit_or_wait`] was added, in every respect except its
    /// error type, which distinguishes a
    /// capacity rejection (`active_for_parent >=
    /// limits.max_active_children_per_parent`) from every other rejection so
    /// [`Self::admit_or_wait`] can retry only the former (development task:
    /// four parallel `explore` calls against a per-parent limit of two used
    /// to hard-fail two of them instead of queueing).
    ///
    /// # Errors
    /// [`AdmitRejection::Capacity`] exactly when the active-child-per-parent
    /// limit is reached; [`AdmitRejection::Other`] for every other rejection
    /// (unknown role, spawn-matrix denial, sandbox escalation, depth,
    /// lease/registry failure, poisoned lock, ...).
    fn admit_inner(
        &self,
        role_name: &str,
        input: SpawnInput,
        sandbox: SandboxSpec,
        suggestions: Option<AgentSuggestions>,
    ) -> Result<SessionId, AdmitRejection> {
        let definition = self
            .roles
            .get(role_name)
            .ok_or_else(|| Self::reject(format!("child role '{role_name}' is not registered")))?;

        // Addendum D: aus dem rohen Spawn-Kontext gelesen, bevor `input`
        // weiter unten feldweise in den `ChildRecord` verschoben wird.
        let task_complexity = TaskComplexity::from_spawn_context(&input.context);

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
        // Beide Prüfungen (geschlossene Rollenmatrix inkl. `uia-worker`,
        // Addendum J + exakte `ChildOrchestrator`-Freigabeliste) laufen über
        // `can_delegate_to`, dieselbe Hilfsfunktion, die auch
        // `delegation_visibility::visible_delegation_targets` verwendet —
        // damit kann die dem Modell gezeigte Zielliste nie von der
        // tatsächlichen Admission abweichen.
        if !can_delegate_to(
            parent_context.organizational_role,
            definition.organizational_role,
            role_name,
            &parent_context.allowed_child_orchestrators,
        ) {
            // Admission errors cross the model-facing spawn boundary. Do not
            // turn either check into an agent-catalog oracle by naming the
            // caller, target category, registered definition, or which of
            // the two predicates failed — a sibling/hidden sub-orchestrator
            // must stay unobservable either way.
            return Err(AdmitRejection::Other(Self::reject(
                "no delegation capability is available for this request",
            )));
        }

        // Addendum F+G: doppelte Delegation erkennen — nicht blockieren, nur
        // melden. Läuft vor den Kapazitätsprüfungen, weil die Erkennung
        // selbst keine Ressource verbraucht und auch für eine später
        // abgelehnte Admission aussagekräftig bleibt.
        {
            let normalized = normalize_delegation_brief(role_name, input.instructions.as_deref());
            let hash = hash_delegation_brief(&normalized);
            let mut recent = self
                .recent_delegation_hashes
                .lock()
                .map_err(|_| Self::reject("delegation-brief history lock is poisoned"))?;
            let entry = recent
                .entry(input.parent_session_id.as_str().to_owned())
                .or_default();
            if entry.contains(&hash) {
                if let Some(observer) = &self.drift_observer {
                    observer.on_drift(&crate::guard::DriftEvent {
                        kind: crate::guard::DriftKind::DuplicateDelegation,
                        session_id: input.parent_session_id.as_str().to_owned(),
                        detail: format!(
                            "parent already delegated an equivalent brief to role '{role_name}' recently"
                        ),
                        tool_name: None,
                        child_role: Some(role_name.to_owned()),
                    });
                }
            }
            entry.push_back(hash);
            if entry.len() > RECENT_DELEGATION_HASH_CAPACITY {
                entry.pop_front();
            }
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
            return Err(AdmitRejection::Capacity(Self::reject(format!(
                "parent {} reached its active child limit of {}",
                input.parent_session_id, self.limits.max_active_children_per_parent
            ))));
        }

        // Budget-Anrechnung: ein Kind darf nie mehr bekommen, als seinem
        // Elternteil noch bleibt (Budget minus angerechneter Verbrauch). Ein
        // Elternteil ohne Record (Wurzel) setzt keinen Deckel. Ist eine
        // gesetzte Dimension erschöpft, wird fail-closed abgelehnt.
        let child_budget = match active.get(input.parent_session_id.as_str()) {
            Some(parent_record) => {
                let remaining = budget_minus_usage(parent_record.budget, parent_record.consumed);
                let exhausted = [
                    (
                        BudgetDimension::Tokens,
                        parent_record.budget.max_tokens,
                        remaining.max_tokens,
                        parent_record.consumed.tokens,
                    ),
                    (
                        BudgetDimension::ToolCalls,
                        parent_record.budget.max_tool_calls.map(u64::from),
                        remaining.max_tool_calls.map(u64::from),
                        u64::from(parent_record.consumed.tool_calls),
                    ),
                    (
                        BudgetDimension::WallTime,
                        parent_record.budget.max_wall_time_ms,
                        remaining.max_wall_time_ms,
                        parent_record.consumed.wall_time_ms,
                    ),
                ]
                .into_iter()
                .find_map(|(dimension, limit, left, used)| match (limit, left) {
                    (Some(limit), Some(0)) => Some((dimension, limit, used)),
                    _ => None,
                });
                if let Some((dimension, limit, used)) = exhausted {
                    tracing::warn!(
                        parent = %input.parent_session_id,
                        dimension = dimension.as_str(),
                        limit = limit,
                        used = used,
                        "child_admission.parent_budget_exhausted",
                    );
                    return Err(AdmitRejection::Other(Self::budget_exceeded(
                        dimension, limit, used,
                    )));
                }
                tighten_agent_budget(child_budget, remaining)
            }
            None => child_budget,
        };

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
            if let Some(violation) = Self::describe_context_program_ceiling_violation(
                &child_ceiling,
                ir.context_program(),
            ) {
                return Err(AdmitRejection::Other(Self::reject(format!(
                    "child context program escalation rejected: {violation}"
                ))));
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
            return Err(AdmitRejection::Other(Self::reject(format!(
                "child depth {depth} exceeds maximum {inherited_depth_ceiling}"
            ))));
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
        // Welle FANIN-K: der ParentGrant wird ausschließlich aus bereits
        // geprüften, vertrauenswürdigen Eltern-Daten gebildet — derselbe
        // `parent_context`/`parent_activation`, der oben schon den
        // Rollenmatrix-Schnitt und den Aktivierungsschnitt trägt. Ohne
        // erreichbare Eltern-Registry (externe Wurzel ohne Manager-Eintrag)
        // bleibt `tools` fail-closed leer.
        let parent_tools: BTreeSet<String> = match manager.get(&input.parent_session_id) {
            Ok(parent_session) => parent_session
                .registry()
                .tool_providers()
                .iter()
                .flat_map(|provider| provider.tools())
                .filter(|spec| {
                    parent_activation.is_tool_enabled(&harw_tools::ToolName::new(spec.name()))
                })
                .map(|spec| spec.name().to_string())
                .collect(),
            Err(_) => BTreeSet::new(),
        };
        let parent_grant = ParentGrant {
            role: Some(parent_context.organizational_role),
            tools: parent_tools,
            permissions: parent_context.sandbox.permissions().clone(),
            max_depth: inherited_depth_ceiling.saturating_sub(parent_depth),
            budget_tokens: active
                .get(input.parent_session_id.as_str())
                .and_then(|record| record.budget.max_tokens)
                .unwrap_or(0),
            reasoning_effort: parent_reasoning_effort.map(|effort| effort.to_string()),
        };
        let registry = definition
            .registry_factory
            .build_registry_with_capabilities_for_parent(
                role_name,
                &input,
                child_suggestions.as_ref(),
                &parent_grant,
            )?;
        if self.limits.lease_seconds <= 0 {
            return Err(AdmitRejection::Other(Self::reject(
                "child lease duration must be positive",
            )));
        }
        let admitted_at = Timestamp::now();
        let lease_expires_at = admitted_at
            .checked_add(SignedDuration::from_secs(self.limits.lease_seconds))
            .map_err(|error| Self::reject(format!("child lease overflow: {error}")))?;
        // This grant belongs to the child definition, not its parent. An
        // absent IR/list is default-deny for its future child-orchestrator
        // delegation. Captured up front (Addendum F+G) so the effort-weight
        // lookup below can reuse it without a second IR read.
        let child_allowed_child_orchestrators: Vec<String> = executable_ir
            .map(|ir| ir.spawn_contract().child_orchestrators().to_vec())
            .unwrap_or_default();
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
                allowed_child_orchestrators: child_allowed_child_orchestrators.clone(),
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
        // Welle 3: das Modell, das der Kind-Provider fest anspricht (z. B. ein
        // `PinnedModelProvider` für Explorer/Worker). Die Factory ist die
        // einzige Stelle, die das Provider-Routing kennt. `None` blockiert
        // die Admission nicht — dann gilt das `active_model` der Session.
        let child_pinned_model: Option<String> = definition
            .registry_factory
            .pinned_model_for_task(role_name, task_complexity);
        // Für `ChildRecord::model`: gepinntes Modell, sonst `active_model`;
        // gesetzt im Auto-Compact-Block unten, der die Kind-Session ohnehin liest.
        let mut child_model: Option<String> = child_pinned_model.clone();
        // Addendum D: Auto-Compact-Policy des Kindes nach organisatorischer
        // Rolle. Root-/Sub-Orchestrator-Sessions bekommen zusätzlich zur
        // relativen Schwelle einen festen Deckel und ein Turn-Start-
        // Verdichtungsziel (sie leben lang und tragen die UIA→Root-Schicht
        // über Auftragsgrenzen hinweg); reine Worker-Kinder nur den festen
        // Deckel, kein Turn-Start-Ziel (sie sind kurzlebig und erledigen
        // genau einen Auftrag).
        {
            let child_session = manager.get_mut(&child).map_err(|error| {
                Self::reject(format!(
                    "child session {child} disappeared before the auto-compact policy was set: {error}"
                ))
            })?;
            // Kontextfenster des tatsächlich angesprochenen Kind-Modells
            // (Resolver aus der Runtime: Config → Modellkatalog → 200 000).
            // Ein gepinnter Kind-Provider (`ModelProvider::pinned_model_id`,
            // z. B. ein `PinnedModelProvider` für Explorer/Worker) überschreibt
            // das Modell jedes Requests — sein Fenster gilt, nicht das des
            // `active_model` der Session. Die relative 70 %-Schwelle gilt gegen
            // dieses Fenster; der feste Deckel (s. u., konfigurierbar über
            // `with_compaction_ceiling`) begrenzt zusätzlich die kumulierte
            // Input-Nutzung langer Sessions (Standard: 500 000).
            let active_model = child_session
                .active_model()
                .map(|model| model.as_str().to_owned());
            let model = child_pinned_model.clone().or(active_model);
            child_model.clone_from(&model);
            let window = self.context_window_for(model.as_deref());
            let history_budget = usize::try_from(window.saturating_mul(3)).unwrap_or(usize::MAX);
            let budget = child_session.context_budget();
            if history_budget > budget.max_history_bytes {
                child_session.set_context_budget(crate::context_budget::ContextBudget {
                    max_context_bytes: budget.max_context_bytes,
                    max_history_bytes: history_budget,
                });
            }
            // Ausgabe-Reserve (Welle 3): ohne Config-Zugriff gilt der
            // Standard (16 384, gedeckelt auf 15 % des Fensters); die Runtime
            // setzt für die Wurzel ggf. einen konfigurierten Wert.
            let reserve = crate::context_budget::output_reserve_tokens(window, None, None, false);
            child_session.set_max_output_tokens(Some(reserve));
            let base_policy = crate::auto_compact::AutoCompactPolicy::for_context_window(window)
                .with_absolute_ceiling(Some(self.compaction_ceiling()))
                .with_output_reserve(reserve)
                .with_fixed_overhead(CHILD_FIXED_OVERHEAD_TOKENS);
            let policy = match definition.organizational_role {
                harw_agent_dsl::roles::AgentRoleId::RootOrchestrator
                | harw_agent_dsl::roles::AgentRoleId::ChildOrchestrator => base_policy
                    .with_turn_start_target(Some(
                        crate::auto_compact::DEFAULT_ORCHESTRATOR_TURN_START_TARGET_TOKENS,
                    )),
                harw_agent_dsl::roles::AgentRoleId::UserInterface
                | harw_agent_dsl::roles::AgentRoleId::Worker
                | harw_agent_dsl::roles::AgentRoleId::UiaWorker
                | harw_agent_dsl::roles::AgentRoleId::AgentSteward => base_policy,
            };
            child_session.set_auto_compact(Some(policy));
        }
        // Addendum F+G: Fortschritts-Feedback für den Lease-Wächter. Dasselbe
        // Wieder-Einsetzen-Muster wie bei der IR-Aktivierung oben, weil
        // `with_progress_observer` eine verbrauchende Methode ist.
        //
        // Welle FANIN-K: dieselbe Gelegenheit setzt auch die Turn-Wächter der
        // Kind-Session — Guard-Policy, Drift-Observer, Pitfall-Advisor —,
        // dieselben, die die Wurzel-Session dieses Controllers trägt. Ein
        // Kind soll denselben Schutz vor Drift, Endlosschleifen und bekannten
        // Pitfalls haben wie die Wurzel, nicht weniger.
        {
            let child_session = manager.remove(&child).ok_or_else(|| {
                Self::reject(format!(
                    "child session {child} disappeared before the progress observer was attached"
                ))
            })?;
            // Welle 3: Vorgabe-Turn-Grenzen des Kindes. Bereits gesetzte
            // Grenzen bleiben erhalten; nur eine fehlende Kappung der
            // Werkzeugergebnisse wird auf `CHILD_TOOL_RESULT_MAX_BYTES` gesetzt.
            let mut limits = child_session
                .default_turn_limits()
                .unwrap_or_else(crate::turn_loop::TurnLimits::unlimited);
            if limits.tool_result_max_bytes == usize::MAX {
                limits.tool_result_max_bytes = CHILD_TOOL_RESULT_MAX_BYTES;
            }
            let child_session = child_session.with_default_turn_limits(limits);
            // Welle 3: Resolver an die Kind-Session, damit ein späterer
            // Modellwechsel (`set_active_model`) Policy und History-Budget neu
            // skaliert. Bei gepinntem Kind-Modell bleibt dessen Fenster
            // maßgeblich — der Pin überschreibt jedes Request-Modell.
            let child_session = match self.context_window_resolver.clone() {
                Some(resolver) => {
                    let pinned = child_pinned_model.clone();
                    let scoped: Arc<ContextWindowResolver> =
                        Arc::new(move |model: Option<&str>| resolver(pinned.as_deref().or(model)));
                    child_session.with_context_window_resolver(scoped)
                }
                None => child_session,
            };
            let child_session = child_session
                .with_progress_observer(Some(self.progress_observer()))
                .with_guard_policy(Some(self.guard_policy))
                .with_drift_observer(self.drift_observer.clone())
                .with_pitfall_advisor(self.pitfall_advisor.clone());
            manager.restore(child_session).map_err(|error| {
                Self::reject(format!(
                    "could not restore child {child} after attaching the progress observer: {error}"
                ))
            })?;
        }
        // Monotone Vererbung + Standard-Rangfolge (Welle 8: Provider > Modell
        // > Agent > Rolle; Addendum F+G): das Kind startet mit dem
        // Effort-Level des Parents, geklammert auf den nach dieser Rangfolge
        // aufgelösten Standard — nie höher als eines von beiden. Die Rollen-
        // Ebene bleibt der Boden der Rangfolge, wie vor Welle 8.
        {
            let role_weight = self.role_effort_weights.for_child(
                definition.organizational_role,
                !child_allowed_child_orchestrators.is_empty(),
                task_complexity,
            );
            let (provider_default, model_default) = definition
                .registry_factory
                .reasoning_effort_defaults_for_role_task(role_name, task_complexity);
            let agent_default = executable_ir
                .and_then(harw_agent_dsl::executable::ExecutableAgentIr::reasoning_effort);
            let resolved_default = resolve_child_default_reasoning_effort(
                provider_default,
                model_default,
                agent_default,
                role_weight,
            );
            let inherited = parent_reasoning_effort.unwrap_or(DEFAULT_CHILD_REASONING_EFFORT);
            let effective = inherited.min(resolved_default);
            if let Ok(child_session) = manager.get_mut(&child) {
                child_session.set_reasoning_effort(Some(effective));
            }
        }
        // Der Root wird aus dem bereits admittierten Parent-Teilbaum gelesen,
        // bevor das neue Kind eingefügt wird. Damit ist die Event-Korrelation
        // für externe wie manager-eigene Wurzeln exakt und braucht kein Feld
        // auf SpawnContext.
        let root_session_id = Self::root_from_active(&active, &input.parent_session_id)
            .ok_or_else(|| Self::reject("child parent lineage contains a cycle"))?;
        // Auftrag des Kindes: `instructions`, sonst ein nicht leerer
        // `context`. Der ungekürzte Text wird als `pending_task` für den
        // ersten Lauf mit leerem `TurnInput` hinterlegt, sein Kurzkopf
        // speist `AgentOrchestrationEvent::task`.
        let pending_task = spawn_task_text(input.instructions.as_deref(), &input.context);
        let task = pending_task.as_deref().and_then(orchestration_detail_head);
        // Liegt der Elternteil mit laufendem Turn im Manager, bekommt das
        // Kind sofort dessen Live-Kanal als Fortschritts-Senke; sonst muss
        // der Aufrufer `attach_child_progress_sink` nutzen.
        let parent_progress = manager
            .get(&input.parent_session_id)
            .ok()
            .and_then(|parent| {
                parent
                    .current_turn()
                    .cloned()
                    .map(|turn_id| (turn_id, parent.live_emitter()))
            });
        let record = ChildRecord {
            live: crate::child_controller::ChildLiveStats::default(),
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
            status: ChildStatus::Admitted,
            task_complexity,
            model: child_model,
            consumed: ChildUsage::default(),
            charged_to_parent: ChildUsage::default(),
        };
        if let Some(lease_store) = &self.lease_store {
            if let Err(error) = lease_store.admit(&record.durable_lease()) {
                let _ = manager.remove(&child);
                return Err(AdmitRejection::Other(Self::reject(format!(
                    "could not durably admit child lease: {error}"
                ))));
            }
        }
        // A-CHILD: das Kind erbt einen von seinem Elternteil abgeleiteten
        // Token (`CancelToken::child`), nicht einen unabhängigen — ein
        // Eltern-Abbruch muss das Kind samt Nachkommen erreichen (siehe
        // Moduldoku Z. 27-28). `cancellations` ist hier schon gesperrt
        // (oben), daher der direkte Blick in die Map statt eines erneuten
        // `child_cancel_token`-Aufrufs (der dieselbe Sperre erneut nähme).
        // Der Elternschlüssel wird vor dem Verschieben von `record` in
        // `active` geklont, da `record` danach nicht mehr lesbar ist.
        let event_record = record.clone();
        let parent_key = record.parent.as_str().to_owned();
        active.insert(child.as_str().to_owned(), record);
        let child_cancel = if let Some(parent_token) = cancellations.get(&parent_key) {
            parent_token.child()
        } else {
            let mut parent_tokens = self
                .parent_tokens
                .lock()
                .map_err(|_| Self::reject("parent cancellation registry lock is poisoned"))?;
            parent_tokens
                .entry(parent_key)
                .or_insert_with(|| ParentToken {
                    token: CancelToken::new(),
                    registered: false,
                })
                .token
                .child()
        };
        cancellations.insert(child.as_str().to_owned(), child_cancel);
        match self.child_tasks.lock() {
            Ok(mut tasks) => {
                tasks.insert(
                    child.as_str().to_owned(),
                    ChildTaskState {
                        pending_task,
                        task: task.clone(),
                        outcome_detail: None,
                    },
                );
            }
            Err(_) => tracing::warn!(child = %child, "child_task_state.lock_poisoned"),
        }
        if let Some((turn_id, emitter)) = parent_progress {
            match self.progress_sinks.lock() {
                Ok(mut sinks) => {
                    sinks.insert(
                        child.as_str().to_owned(),
                        ChildProgressSink {
                            turn_id,
                            emitter,
                            last_emitted: None,
                        },
                    );
                }
                Err(_) => tracing::warn!(child = %child, "child_progress.attach_lock_poisoned"),
            }
        }
        // Never invoke runtime code while controller locks are live: an
        // observer may persist synchronously or inspect the tree.
        drop(cancellations);
        drop(active);
        drop(manager);
        self.observe_orchestration(
            root_session_id,
            &event_record,
            task,
            AgentOrchestrationStatus::Admitted,
        );
        Ok(child)
    }

    /// Admits a child, waiting for capacity instead of rejecting outright
    /// when the parent is at [`ChildLimits::max_active_children_per_parent`].
    ///
    /// # Description
    /// Fixes the four-parallel-`explore` regression: with a per-parent limit
    /// of two, two of four concurrent [`Self::admit`] calls used to fail
    /// hard with "reached its active child limit". This wraps
    /// [`Self::admit_inner`] the way `admission::SubmissionLimiter`'s
    /// `wait_before_retry` wraps `JobAdmissionService::submit` for the
    /// analogous job-admission rate limiter: one attempt is made
    /// immediately, and only [`AdmitRejection::Capacity`] is retried.
    /// [`AdmitRejection::Other`] — unknown role, spawn-matrix denial,
    /// sandbox escalation, depth, lease/registry failure, poisoned lock,
    /// ... — is returned unchanged on the very first attempt, exactly as
    /// [`Self::admit`] always has.
    ///
    /// Each retry creates and [enables](tokio::sync::Notify::notified) a
    /// fresh [`Self::freed`] waiter *before* re-checking admission (the same
    /// order [`Self::release_in_memory`] relies on to guarantee a release
    /// landing between the failed check and the `.await` is still observed —
    /// `notify_waiters` does not remember a notification for a waiter
    /// created afterward). A bounded sleep (at most 500ms, less if `max_wait`
    /// is about to elapse) races alongside that wait regardless, so even a
    /// theoretically missed wakeup costs at most one extra poll, never a
    /// hang.
    ///
    /// # Arguments
    /// - `role_name`, `input`, `sandbox`, `suggestions`: identical to
    ///   [`Self::admit`]; `input`/`sandbox`/`suggestions` are cloned once per
    ///   retry attempt (the original caller's clone is never mutated).
    /// - `max_wait` (`std::time::Duration`): upper bound on the total time
    ///   spent waiting across every retry, starting from this call. Once it
    ///   elapses while still at capacity, the exact
    ///   [`AdmitRejection::Capacity`] error the next attempt would have
    ///   produced is returned.
    /// - `cancel` (`&CancelToken`): observed between attempts (not during
    ///   `admit_inner` itself, which never awaits). A token cancelled while
    ///   waiting ends the wait immediately with a distinct message — never
    ///   confusable with the capacity message, so a caller can match on
    ///   `error.message.contains(...)` to tell the two apart if it must.
    ///
    /// # Returns
    /// `Ok(SessionId)` of the newly admitted child — identical in shape to
    /// [`Self::admit`]'s success case.
    ///
    /// # Errors
    /// - Any [`AdmitRejection::Other`] rejection from the first attempt,
    ///   unchanged.
    /// - The original capacity [`AgentSpawnError`] once `max_wait` elapses.
    /// - An [`AgentSpawnError`] naming the cancellation if `cancel` is
    ///   cancelled before a slot freed.
    ///
    /// # Concurrency
    /// Requires a Tokio runtime (`tokio::select!`, `tokio::time::sleep`,
    /// [`tokio::sync::Notify`]). Runs entirely on the calling task — unlike
    /// `JobAdmissionService::submit_queued_async`, no background task is
    /// spawned, because callers such as the parallel `explore` fan-out in
    /// `harw-core-bridge::agent_tool` already run each spawn attempt on its
    /// own task and want to `.await` this call directly rather than poll a
    /// `oneshot::Receiver`.
    pub async fn admit_or_wait(
        &self,
        role_name: &str,
        input: SpawnInput,
        sandbox: SandboxSpec,
        suggestions: Option<AgentSuggestions>,
        max_wait: Duration,
        cancel: &CancelToken,
    ) -> Result<SessionId, AgentSpawnError> {
        let deadline = tokio::time::Instant::now() + max_wait;
        loop {
            // Registered *before* the admission attempt below: if a release
            // notifies between this attempt's capacity check and the
            // `tokio::select!` awaiting `notified`, the notification is
            // still observed (see the method doc and `Self::freed`).
            let notified = self.freed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match self.admit_inner(
                role_name,
                input.clone(),
                sandbox.clone(),
                suggestions.clone(),
            ) {
                Ok(child) => return Ok(child),
                Err(AdmitRejection::Other(error)) => return Err(error),
                Err(AdmitRejection::Capacity(error)) => {
                    let now = tokio::time::Instant::now();
                    if now >= deadline {
                        return Err(error);
                    }
                    let sleep_for = deadline
                        .saturating_duration_since(now)
                        .min(Duration::from_millis(500));
                    tokio::select! {
                        biased;
                        () = cancel.cancelled() => {
                            return Err(Self::reject(format!(
                                "waiting for parent {}'s child capacity was cancelled before a slot freed",
                                input.parent_session_id
                            )));
                        }
                        () = notified.as_mut() => {}
                        () = tokio::time::sleep(sleep_for) => {}
                    }
                }
            }
        }
    }

    /// [`Self::admit_or_wait`] followed by [`Self::guard_child`] — the
    /// capacity-waiting counterpart to [`Self::spawn_child_guarded`], for
    /// callers that want a [`ChildGuard`] without an unguarded window
    /// between admission and the guard taking ownership of the slot.
    ///
    /// # Arguments
    /// See [`Self::admit_or_wait`].
    ///
    /// # Returns
    /// The new child's [`ChildGuard`].
    ///
    /// # Errors
    /// See [`Self::admit_or_wait`].
    ///
    /// # Concurrency
    /// See [`Self::admit_or_wait`].
    pub async fn spawn_child_or_wait(
        &self,
        role_name: &str,
        input: SpawnInput,
        sandbox: SandboxSpec,
        suggestions: Option<AgentSuggestions>,
        max_wait: Duration,
        cancel: &CancelToken,
    ) -> Result<ChildGuard<'_>, AgentSpawnError> {
        self.admit_or_wait(role_name, input, sandbox, suggestions, max_wait, cancel)
            .await
            .map(|child| self.guard_child(child))
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

    fn delegation_target_names(&self, parent_session_id: &SessionId) -> Vec<String> {
        self.visible_delegation_target_names(parent_session_id)
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activation::ToolProfile;
    use crate::model::{EchoModelProvider, ModelFuture, ModelRequest, ModelResponse};
    use crate::session::AgentSession;
    use crate::state_store::InMemoryStateStore;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_agent_dsl::authority::AuthorityCeiling;
    use harw_agent_dsl::executable::lower;
    use harw_agent_dsl::parse::parse_toml;
    use harw_agent_dsl::resolved::{ResolutionTrace, ResolvedAgentDefinition};
    use harw_authority::{Permission, PermissionSet, WorkspaceRegistration, WorkspaceRegistry};
    use harw_context::SectionName;
    use harw_extension_api::{
        ApprovalDecision, ApprovalHandler, ExtFuture, ExtensionRegistryBuilder,
    };
    use harw_tools::{ToolCall, ToolName};
    use harw_types::{ApprovalActor, ItemId, TenantId, TokenUsage, WorkspaceId};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::mpsc;

    // ── cap_child_return_text (Vertrag CHILD_RETURN_MAX_BYTES) ─────────────

    #[test]
    fn test_cap_child_return_text_short_text_unchanged() {
        let text = "kurze Antwort";
        assert_eq!(cap_child_return_text(text, CHILD_RETURN_MAX_BYTES), text);
    }

    #[test]
    fn test_cap_child_return_text_exact_budget_unchanged() {
        let text = "x".repeat(64);
        assert_eq!(cap_child_return_text(&text, 64), text);
    }

    #[test]
    fn test_cap_child_return_text_long_ascii_truncated_with_marker() {
        let max_bytes = 100usize;
        let text = "a".repeat(1000);
        let capped = cap_child_return_text(&text, max_bytes);

        assert!(
            capped.len() < text.len(),
            "der gekürzte Text muss kürzer als das Original sein"
        );
        assert!(
            capped.contains("gekürzt"),
            "der gekürzte Text muss die Markierung enthalten"
        );
        // Kopf und Ende dürfen zusammen `max_bytes` nicht überschreiten; die
        // Markierung selbst kommt oben drauf, bleibt aber klein.
        assert!(
            capped.len() <= max_bytes + 128,
            "gekürzter Text (+ Markierung) muss nahe am Budget bleiben, war {}",
            capped.len()
        );
        assert!(capped.starts_with('a'), "der Kopf muss erhalten bleiben");
        assert!(capped.ends_with('a'), "das Ende muss erhalten bleiben");
    }

    #[test]
    fn test_cap_child_return_text_never_splits_multibyte_char() {
        // '€' ist 3 Bytes, '🦀' ist 4 Bytes UTF-8 — beide werden wiederholt,
        // damit ein naives Byte-Cutoff garantiert mitten in einem Zeichen läge.
        let text = "€🦀".repeat(200);
        let max_bytes = 97usize; // bewusst kein Vielfaches von 3 oder 4
        let capped = cap_child_return_text(&text, max_bytes);

        assert!(capped.len() < text.len());
        // `String` kann nur gültiges UTF-8 enthalten; wäre irgendwo ein
        // Zeichen zerschnitten worden, hätte `cap_child_return_text` selbst
        // nicht kompiliert/gebaut werden können, ohne zu paniken. Diese
        // Prüfung stellt zusätzlich sicher, dass kein Ersatzzeichen (U+FFFD)
        // durch eine fehlerhafte Byte-Slice-Operation entstanden ist.
        assert!(!capped.contains('\u{FFFD}'));
    }

    #[test]
    fn test_cap_child_return_text_reports_omitted_byte_count() {
        let text = "b".repeat(500);
        let capped = cap_child_return_text(&text, 50);
        assert!(
            capped.contains(" Bytes der Kind-Antwort gekürzt "),
            "die Markierung muss die Anzahl gekürzter Bytes nennen: {capped}"
        );
    }

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
                    ..Default::default()
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

    /// Registry-Factory, die einen festen Provider-/Modell-Reasoning-Effort-
    /// Standard liefert — für Tests der Welle-8-Rangfolge (Provider > Modell
    /// > Agent > Rolle) beim Kind-Spawn.
    struct FixedReasoningEffortDefaultsRegistry {
        provider_default: Option<ReasoningEffort>,
        model_default: Option<ReasoningEffort>,
    }

    impl ChildRegistryFactory for FixedReasoningEffortDefaultsRegistry {
        fn build_registry(
            &self,
            _role: &str,
            _input: &SpawnInput,
            _suggestions: Option<&AgentSuggestions>,
        ) -> Result<ExtensionRegistry, AgentSpawnError> {
            Ok(ExtensionRegistryBuilder::default().build())
        }

        fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
            Ok(Arc::new(EchoModelProvider::new("fixed defaults child")))
        }

        fn reasoning_effort_defaults_for_role(
            &self,
            _role: &str,
        ) -> (Option<ReasoningEffort>, Option<ReasoningEffort>) {
            (self.provider_default, self.model_default)
        }
    }

    /// Lowert eine Test-Agent-IR aus einem TOML-Fragment (Sektionen `[spawn]`,
    /// `[spawn.budget]`, `[lifecycle]`, `[tools]`).
    fn test_agent_ir(sections: &str) -> TestResult<ExecutableAgentIr> {
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
        .map_err(ctx("test agent definition must parse"))?;
        let resolved = ResolvedAgentDefinition {
            id: raw.id,
            version: raw.version,
            role: raw.role,
            specialization: raw.specialization,
            name: raw.name,
            description: raw.description,
            reasoning_effort: raw.reasoning_effort,
            authority: AuthorityCeiling::default(),
            trace: ResolutionTrace { steps: Vec::new() },
            config: raw.tables,
        };
        lower(&resolved).map_err(ctx("test agent definition must lower"))
    }

    fn test_sandbox(permissions: PermissionSet) -> TestResult<SandboxSpec> {
        let harness_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .ok_or(TestError::Missing("harw-core has a workspace parent"))?
            .to_path_buf();
        let registry = WorkspaceRegistry::build(
            &harness_root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("test-tenant"),
                workspace: WorkspaceId::from_str("external-root-controller-tests"),
                root: PathBuf::from("harw-core"),
            }],
        )
        .map_err(ctx("test workspace is registered"))?;
        Ok(SandboxSpec::from_resolved(
            registry
                .resolve(
                    &TenantId::from_str("test-tenant"),
                    &WorkspaceId::from_str("external-root-controller-tests"),
                )
                .map_err(ctx("test workspace resolves"))?,
            permissions,
        ))
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
            allowed_child_orchestrators: Vec::new(),
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

    /// Wie [`worker_spawner`], aber mit frei wählbaren [`ChildLimits`] — für
    /// die `admit_or_wait`-Tests, die einen engen
    /// `max_active_children_per_parent`-Deckel (typischerweise `1`) brauchen,
    /// um Kapazitätsdruck ohne acht parallele Kinder zu erzwingen.
    fn worker_spawner_with_limits(
        manager: Arc<Mutex<SessionManager>>,
        limits: ChildLimits,
    ) -> ManagedAgentSpawner {
        ManagedAgentSpawner::new(manager, limits).with_role(
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

    fn spawner_with_admitted_child() -> TestResult<(ManagedAgentSpawner, SessionId)> {
        spawner_with_admitted_child_and_lease_store(None)
    }

    fn spawner_with_admitted_child_and_lease_store(
        lease_store: Option<Arc<ChildLeaseStore>>,
    ) -> TestResult<(ManagedAgentSpawner, SessionId)> {
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
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .restore(child_session)
            .map_err(ctx("test child session restores"))?;

        let spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative());
        let spawner = if let Some(lease_store) = lease_store.as_ref() {
            spawner.with_lease_store(lease_store.clone())
        } else {
            spawner
        };
        let now = Timestamp::now();
        let record = ChildRecord {
            live: crate::child_controller::ChildLiveStats::default(),
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
            status: ChildStatus::Admitted,
            task_complexity: None,
        };
        if let Some(lease_store) = lease_store {
            lease_store
                .admit(&record.durable_lease())
                .map_err(ctx("test lease admission persists"))?;
        }
        spawner
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(child.as_str().to_owned(), record);
        Ok((spawner, child))
    }

    #[test]
    fn child_final_assistant_text_returns_the_newest_response() -> TestResult {
        let (spawner, child) = spawner_with_admitted_child()?;
        let mut manager = spawner
            .manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let history = manager
            .get_mut(&child)
            .map_err(ctx("admitted child session exists"))?
            .history_mut();
        history.push_assistant_text("intermediate response", None);
        history.push_user_text("continue");
        history.push_assistant_text("completed child response", None);
        drop(manager);

        assert_eq!(
            spawner
                .child_final_assistant_text(&child)
                .map_err(ctx("completed child text is available"))?,
            "completed child response"
        );
        Ok(())
    }

    #[test]
    fn child_final_assistant_text_rejects_unknown_child() -> TestResult {
        let (spawner, _child) = spawner_with_admitted_child()?;
        let Err(error) = spawner.child_final_assistant_text(&SessionId::new()) else {
            return Err(TestError::Unexpected(
                "unknown child must be rejected".to_owned(),
            ));
        };

        assert!(error.message.contains("is not admitted"));
        Ok(())
    }

    #[test]
    fn child_final_assistant_text_rejects_child_without_assistant_response() -> TestResult {
        let (spawner, child) = spawner_with_admitted_child()?;
        spawner
            .manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(&child)
            .map_err(ctx("admitted child session exists"))?
            .history_mut()
            .push_user_text("work on this");

        let Err(error) = spawner.child_final_assistant_text(&child) else {
            return Err(TestError::Unexpected(
                "child without an assistant response must be rejected".to_owned(),
            ));
        };

        assert!(error.message.contains("no assistant response text"));
        Ok(())
    }

    #[test]
    fn child_completed_durably_records_completion_before_releasing_admission() -> TestResult {
        let temporary_directory = tempfile::tempdir().map_err(ctx("temporary lease directory"))?;
        let lease_store = Arc::new(ChildLeaseStore::new(temporary_directory.path()));
        let (spawner, child) =
            spawner_with_admitted_child_and_lease_store(Some(lease_store.clone()))?;
        let record = spawner
            .child_record(&child)
            .ok_or(TestError::Missing("test child is admitted"))?;
        let completed_at = Timestamp::now();

        AgentSpawner::child_completed(&spawner, &child, completed_at)
            .map_err(ctx("durable child completion succeeds"))?;

        assert!(spawner.child_record(&child).is_none());
        assert!(
            lease_store
                .active()
                .map_err(ctx("active leases are readable"))?
                .is_empty()
        );
        let completion_path = lease_store
            .root()
            .join(format!("{}.completed.json", child.as_str()));
        let completion: harw_session_store::ChildLeaseCompletionRecord = serde_json::from_slice(
            &std::fs::read(completion_path).map_err(ctx("completion record is written"))?,
        )
        .map_err(ctx("completion record is valid JSON"))?;
        assert_eq!(completion.lease, record.durable_lease());
        assert_eq!(completion.completed_at, completed_at);
        Ok(())
    }

    #[test]
    fn external_root_parent_admits_a_child_without_a_manager_mirror() -> TestResult {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
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
            .map_err(ctx("trusted external root registers during construction"))?;

        let child = spawner
            .admit("worker", spawn_input(parent.clone()), sandbox, None)
            .map_err(ctx("registered external root admits a direct child"))?;

        let record = spawner
            .child_record(&child)
            .ok_or(TestError::Missing("child is tracked"))?;
        assert_eq!(record.parent, parent);
        assert_eq!(record.depth, 1);
        let manager = manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let child_session = manager.get(&child).map_err(ctx("child is manager-owned"))?;
        assert_eq!(child_session.parent_session_id(), Some(&record.parent));
        assert_eq!(
            child_session.reasoning_effort(),
            Some(ReasoningEffort::Medium)
        );
        assert_eq!(
            child_session
                .spawn_context()
                .ok_or(TestError::Missing("child has trusted context"))?
                .approval_actor,
            Some(ApprovalActor::Operator {
                id: "external-root-operator".to_owned(),
            })
        );
        Ok(())
    }

    #[test]
    fn external_root_parent_rejects_unknown_parent_ids() -> TestResult {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
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
            .map_err(ctx("trusted external root registers during construction"))?;

        let Err(error) = spawner.admit("worker", spawn_input(SessionId::new()), sandbox, None)
        else {
            return Err(TestError::Unexpected(
                "unregistered parent must be denied".to_owned(),
            ));
        };

        assert!(error.message.contains("unknown child parent"));
        Ok(())
    }

    #[test]
    fn external_root_parent_rejects_sandbox_escalation() -> TestResult {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let parent_sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
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
            .map_err(ctx("trusted external root registers during construction"))?;

        let Err(error) = spawner.admit(
            "worker",
            spawn_input(parent),
            test_sandbox(PermissionSet::from_policy([
                Permission::ReadWorkspace,
                Permission::WriteWorkspace,
            ]))?,
            None,
        ) else {
            return Err(TestError::Unexpected(
                "external root cannot escalate a child sandbox".to_owned(),
            ));
        };

        assert!(error.message.contains("child sandbox escalation rejected"));
        Ok(())
    }

    #[test]
    fn external_root_parent_enforces_the_role_spawn_matrix() -> TestResult {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let parent = SessionId::new();
        let spawner = worker_spawner(manager)
            .with_external_root_parent(
                parent.clone(),
                external_root_context(sandbox.clone(), harw_agent_dsl::roles::AgentRoleId::Worker),
                None,
                SessionActivation::default(),
            )
            .map_err(ctx("trusted external root registers during construction"))?;

        let Err(error) = spawner.admit("worker", spawn_input(parent), sandbox, None) else {
            return Err(TestError::Unexpected(
                "worker roots cannot spawn durable workers".to_owned(),
            ));
        };

        assert_eq!(
            error.message,
            "no delegation capability is available for this request"
        );
        Ok(())
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
    ) -> TestResult<(ManagedAgentSpawner, Vec<SessionId>)> {
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
            .map_err(ctx("test lease does not overflow"))?;
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
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
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .restore(session)
                .map_err(ctx("test child session restores"))?;
            spawner
                .active
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(
                    child.as_str().to_owned(),
                    ChildRecord {
                        live: crate::child_controller::ChildLiveStats::default(),
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
                        status: ChildStatus::Admitted,
                        task_complexity: None,
                    },
                );
            spawner
                .cancellations
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(child.as_str().to_owned(), CancelToken::new());
            ids.push(child);
        }
        Ok((spawner, ids))
    }

    /// Wie [`runnable_children`], aber mit **einer Rolle je Kind** statt
    /// einer einzigen gemeinsamen `"worker"`-Rolle für alle — Testinfrastruktur
    /// für die UiaWorker-Fan-out-Deckelung
    /// ([`ManagedAgentSpawner::max_concurrent_instances_for_role`],
    /// [`ManagedAgentSpawner::effective_fanout_slots`]), die eine **gemischte**
    /// Welle beweisen muss. `roles` trägt für jedes zu erzeugende Kind ein
    /// `(Rollenname, Organisationsrolle)`-Paar; die Reihenfolge der
    /// zurückgegebenen `SessionId`s entspricht der Reihenfolge von `roles`.
    fn runnable_children_with_roles(
        factory: Arc<dyn ChildRegistryFactory>,
        allow_pause: bool,
        roles: &[(&str, harw_agent_dsl::roles::AgentRoleId)],
        mut registry: impl FnMut() -> ExtensionRegistry,
    ) -> TestResult<(ManagedAgentSpawner, Vec<SessionId>)> {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events.clone())));
        let mut spawner =
            ManagedAgentSpawner::new(Arc::clone(&manager), ChildLimits::conservative());
        let mut registered_role_names: Vec<&str> = Vec::new();
        for (role_name, organizational_role) in roles {
            if registered_role_names.contains(role_name) {
                continue;
            }
            registered_role_names.push(role_name);
            spawner = spawner.with_role(
                *role_name,
                AgentRole::Agent {
                    name: (*role_name).to_owned(),
                },
                *organizational_role,
                Arc::clone(&factory),
            );
        }
        let parent = SessionId::new();
        let admitted_at = Timestamp::now();
        let lease_expires_at = admitted_at
            .checked_add(SignedDuration::from_secs(300))
            .map_err(ctx("test lease does not overflow"))?;
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let mut ids = Vec::with_capacity(roles.len());
        for (role_name, organizational_role) in roles {
            let session = AgentSession::new(
                AgentRole::Agent {
                    name: (*role_name).to_owned(),
                },
                Some(parent.clone()),
                registry(),
                events.clone(),
            )
            .with_spawn_context(external_root_context(sandbox.clone(), *organizational_role));
            let child = session.id().clone();
            manager
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .restore(session)
                .map_err(ctx("test child session restores"))?;
            spawner
                .active
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(
                    child.as_str().to_owned(),
                    ChildRecord {
                        live: crate::child_controller::ChildLiveStats::default(),
                        child: child.clone(),
                        parent: parent.clone(),
                        handoff_call_id: ToolCallId::new(),
                        role: (*role_name).to_owned(),
                        depth: 1,
                        admitted_at,
                        lease_expires_at,
                        budget: AgentBudget::default(),
                        allow_pause,
                        depth_ceiling: ChildLimits::conservative().max_depth,
                        trace: None,
                        status: ChildStatus::Admitted,
                        task_complexity: None,
                    },
                );
            spawner
                .cancellations
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(child.as_str().to_owned(), CancelToken::new());
            ids.push(child);
        }
        Ok((spawner, ids))
    }

    fn empty_registry() -> ExtensionRegistry {
        ExtensionRegistryBuilder::default().build()
    }

    fn child_is_manager_owned(spawner: &ManagedAgentSpawner, child: &SessionId) -> bool {
        spawner
            .manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(child)
            .is_ok()
    }

    // --- W2-17: Budget-Durchsetzung ---------------------------------------

    #[tokio::test]
    async fn wall_time_budget_cancels_cooperatively_and_returns_the_session() -> TestResult {
        let (spawner, children) =
            runnable_children(Arc::new(HangingChildRegistry), true, 1, empty_registry)?;
        let child = children[0].clone();
        let store = InMemoryStateStore::new();

        let Err(error) = spawner
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
        else {
            return Err(TestError::Unexpected(
                "an elapsed wall-time budget must reject the run".to_owned(),
            ));
        };

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
        Ok(())
    }

    #[tokio::test]
    async fn tool_call_budget_violation_reports_a_machine_readable_message() -> TestResult {
        let (spawner, children) = runnable_children(
            Arc::new(EchoChildRegistry {
                reply: "worker complete",
            }),
            true,
            1,
            empty_registry,
        )?;
        let child = children[0].clone();
        {
            let mut manager = spawner
                .manager
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let history = manager
                .get_mut(&child)
                .map_err(ctx("admitted child session exists"))?
                .history_mut();
            for _ in 0..3 {
                history.push_tool_call(ToolCallId::new(), "fs.read", serde_json::Value::Null);
            }
        }
        let store = InMemoryStateStore::new();

        let Err(error) = spawner
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
        else {
            return Err(TestError::Unexpected(
                "an exceeded tool-call budget must reject the run".to_owned(),
            ));
        };

        assert_eq!(
            error.message,
            "budget_exceeded: tool_calls (limit=1, used=3)"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_run_inside_its_tool_call_budget_succeeds() -> TestResult {
        let (spawner, children) = runnable_children(
            Arc::new(EchoChildRegistry {
                reply: "worker complete",
            }),
            true,
            1,
            empty_registry,
        )?;
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
            .map_err(ctx("a child inside its budget must complete"))?;

        assert_eq!(result.child, child);
        assert!(matches!(result.outcome, TurnOutcome::Completed));
        Ok(())
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

    #[test]
    fn max_concurrent_instances_for_role_caps_uia_worker_family_to_one() {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative()).with_role(
            "uia-worker",
            AgentRole::Agent {
                name: "uia-worker".to_owned(),
            },
            harw_agent_dsl::roles::AgentRoleId::UiaWorker,
            Arc::new(EmptyChildRegistry),
        );

        assert_eq!(spawner.max_concurrent_instances_for_role("uia-worker"), 1);
    }

    #[test]
    fn max_concurrent_instances_for_role_leaves_other_roles_unbounded() {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative())
            .with_role(
                "worker",
                AgentRole::Agent {
                    name: "worker".to_owned(),
                },
                harw_agent_dsl::roles::AgentRoleId::Worker,
                Arc::new(EmptyChildRegistry),
            )
            .with_role(
                "agent-steward",
                AgentRole::Agent {
                    name: "agent-steward".to_owned(),
                },
                harw_agent_dsl::roles::AgentRoleId::AgentSteward,
                Arc::new(EmptyChildRegistry),
            );

        assert_eq!(
            spawner.max_concurrent_instances_for_role("worker"),
            usize::MAX
        );
        assert_eq!(
            spawner.max_concurrent_instances_for_role("agent-steward"),
            usize::MAX
        );
        assert_eq!(
            spawner.max_concurrent_instances_for_role("unregistered-role"),
            usize::MAX,
            "an unregistered role must fail open here — admission/execution \
             rejects it separately with its own error"
        );
    }

    #[tokio::test]
    async fn run_children_returns_results_in_request_order() -> TestResult {
        let (spawner, children) = runnable_children(
            Arc::new(EchoChildRegistry {
                reply: "worker complete",
            }),
            true,
            3,
            empty_registry,
        )?;
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
            let run = result.map_err(ctx("every child of the wave completes"))?;
            assert_eq!(
                run.child, children[position],
                "result slot {position} must carry the child requested at that position"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn run_children_with_max_parallel_one_serialises_the_wave() -> TestResult {
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
        )?;
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
        Ok(())
    }

    /// Beweist die UiaWorker-Fan-out-Deckelung (Nutzerauftrag: `analyze(max_parallel:
    /// 4)` darf eine `uia-worker`-Rollenfamilie nicht umgehen können). Die Welle
    /// mischt eine einzige `uia-worker`-Anfrage unter zwei `worker`-Anfragen —
    /// [`ManagedAgentSpawner::effective_fanout_slots`] muss die **gesamte** Welle
    /// trotzdem auf `1` deckeln, obwohl `max_parallel = 4` angefordert wird und
    /// zwei der drei Anfragen für sich unbeschränkt wären.
    #[tokio::test]
    async fn run_children_caps_uia_worker_wave_to_one_regardless_of_max_parallel() -> TestResult {
        let inflight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (spawner, children) = runnable_children_with_roles(
            Arc::new(ConcurrencyProbeRegistry {
                inflight: Arc::clone(&inflight),
                peak: Arc::clone(&peak),
            }),
            true,
            &[
                ("worker", harw_agent_dsl::roles::AgentRoleId::Worker),
                ("uia-worker", harw_agent_dsl::roles::AgentRoleId::UiaWorker),
                ("worker", harw_agent_dsl::roles::AgentRoleId::Worker),
            ],
            empty_registry,
        )?;
        let store = InMemoryStateStore::new();

        let results = spawner
            .run_children(
                fanout_requests(&children),
                &store,
                4,
                JoinSemantics::AllTerminal,
            )
            .await;

        assert_eq!(results.len(), 3);
        assert!(results.iter().all(Result::is_ok));
        assert_eq!(
            peak.load(Ordering::SeqCst),
            1,
            "a wave containing a uia-worker request must serialise, \
             even though max_parallel = 4 and the sibling roles are unbounded"
        );
        Ok(())
    }

    /// Regressionsschutz für die UiaWorker-Deckelung: eine Welle **ohne** jede
    /// `uia-worker`-Anfrage darf weiterhin bis `max_parallel` überlappen —
    /// [`ManagedAgentSpawner::effective_fanout_slots`] darf Nicht-UiaWorker-Wellen
    /// nicht fälschlich auf `1` klemmen.
    #[tokio::test]
    async fn run_children_without_uia_worker_role_stays_unbounded_at_max_parallel() -> TestResult {
        let inflight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let (spawner, children) = runnable_children_with_roles(
            Arc::new(ConcurrencyProbeRegistry {
                inflight: Arc::clone(&inflight),
                peak: Arc::clone(&peak),
            }),
            true,
            &[
                ("worker", harw_agent_dsl::roles::AgentRoleId::Worker),
                (
                    "agent-steward",
                    harw_agent_dsl::roles::AgentRoleId::AgentSteward,
                ),
                ("worker", harw_agent_dsl::roles::AgentRoleId::Worker),
                (
                    "agent-steward",
                    harw_agent_dsl::roles::AgentRoleId::AgentSteward,
                ),
            ],
            empty_registry,
        )?;
        let store = InMemoryStateStore::new();

        let results = spawner
            .run_children(
                fanout_requests(&children),
                &store,
                4,
                JoinSemantics::Collect,
            )
            .await;

        assert_eq!(results.len(), 4);
        assert!(
            peak.load(Ordering::SeqCst) >= 2,
            "a wave without any uia-worker request must not be serialised by the \
             uia-worker cap"
        );
        Ok(())
    }

    #[tokio::test]
    async fn run_children_actually_overlaps_child_turns() -> TestResult {
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
        )?;
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
        Ok(())
    }

    #[tokio::test]
    async fn run_children_any_terminal_cancels_the_remaining_children() -> TestResult {
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

            // Die Admission darf den Sieger-Provider nicht vorab verbrauchen.
            fn pinned_model_for_task(
                &self,
                _role: &str,
                _complexity: Option<TaskComplexity>,
            ) -> Option<String> {
                None
            }
        }

        let (spawner, children) = runnable_children(
            Arc::new(FirstWinsRegistry {
                fast: Mutex::new(true),
            }),
            true,
            3,
            empty_registry,
        )?;
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
        let Ok(winner) = results[0].as_ref() else {
            return Err(TestError::Unexpected(
                "the first child wins the wave".to_owned(),
            ));
        };
        assert_eq!(winner.child, children[0]);
        for position in [1_usize, 2] {
            let Err(error) = results[position].as_ref() else {
                return Err(TestError::Unexpected(
                    "a cancelled sibling must not report a result".to_owned(),
                ));
            };
            assert_eq!(error.message, CANCELLED_BY_SIBLING);
        }
        Ok(())
    }

    #[tokio::test]
    async fn run_children_returns_an_empty_wave_unchanged() -> TestResult {
        let (spawner, _children) =
            runnable_children(Arc::new(EmptyChildRegistry), true, 0, empty_registry)?;
        let store = InMemoryStateStore::new();

        let results = spawner
            .run_children(Vec::new(), &store, 4, JoinSemantics::AllTerminal)
            .await;

        assert!(results.is_empty());
        Ok(())
    }

    // --- W2-19: IR-gesteuerte Kind-Konfiguration ---------------------------

    #[tokio::test]
    async fn a_child_forbidden_to_pause_fails_closed_on_a_paused_turn() -> TestResult {
        let (spawner, children) =
            runnable_children(Arc::new(ToolCallingChildRegistry), false, 1, || {
                ExtensionRegistryBuilder::default()
                    .approval_handler(Arc::new(AskUserApproval))
                    .build()
            })?;
        let child = children[0].clone();
        let store = InMemoryStateStore::new();

        let Err(error) = spawner
            .run_child(&child, &store, TurnInput::user("work"))
            .await
        else {
            return Err(TestError::Unexpected(
                "a pause-forbidden child must not report a waiting outcome".to_owned(),
            ));
        };

        assert_eq!(
            error.message,
            "child paused but its lifecycle forbids pausing: awaiting_approval"
        );
        assert!(
            child_is_manager_owned(&spawner, &child),
            "the rejected child session must still be restored regularly"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_child_allowed_to_pause_reports_the_waiting_outcome() -> TestResult {
        let (spawner, children) =
            runnable_children(Arc::new(ToolCallingChildRegistry), true, 1, || {
                ExtensionRegistryBuilder::default()
                    .approval_handler(Arc::new(AskUserApproval))
                    .build()
            })?;
        let child = children[0].clone();
        let store = InMemoryStateStore::new();

        let result = spawner
            .run_child(&child, &store, TurnInput::user("work"))
            .await
            .map_err(ctx("a pause-permitted child may report a waiting outcome"))?;

        assert!(matches!(
            result.outcome,
            TurnOutcome::AwaitingApproval { .. }
        ));
        Ok(())
    }

    fn ir_spawner(
        ir: ExecutableAgentIr,
    ) -> TestResult<(ManagedAgentSpawner, SessionId, SandboxSpec)> {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
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
            .map_err(ctx("trusted external root registers during construction"))?;
        Ok((spawner, parent, sandbox))
    }

    #[test]
    fn admit_carries_the_agent_ir_budget_pause_lock_and_tool_surface() -> TestResult {
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
        )?)?;

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .map_err(ctx("the IR-governed child is admitted"))?;

        let record = spawner
            .child_record(&child)
            .ok_or(TestError::Missing("child is tracked"))?;
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

        let manager = spawner
            .manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session = manager.get(&child).map_err(ctx("child is manager-owned"))?;
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
        Ok(())
    }

    #[test]
    fn admit_defaults_to_a_pause_lock_without_an_agent_ir() -> TestResult {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
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
            .map_err(ctx("trusted external root registers during construction"))?;

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .map_err(ctx("child admitted"))?;

        let record = spawner
            .child_record(&child)
            .ok_or(TestError::Missing("child is tracked"))?;
        assert!(
            !record.allow_pause,
            "without a lifecycle statement the controller stays fail-closed"
        );
        assert_eq!(spawner.child_budget(&child), Some(AgentBudget::default()));
        Ok(())
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
    ) -> TestResult<(ManagedAgentSpawner, SessionId, SandboxSpec)> {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events.clone())));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let root_session = AgentSession::new(
            AgentRole::Agent {
                name: "root".to_owned(),
            },
            None,
            ExtensionRegistryBuilder::default().build(),
            events,
        )
        .with_spawn_context({
            let mut context = external_root_context(
                sandbox.clone(),
                harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
            );
            context
                .allowed_child_orchestrators
                .push("manager".to_owned());
            context
        });
        let root = root_session.id().clone();
        manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .restore(root_session)
            .map_err(ctx("root session restores"))?;

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
        Ok((spawner, root, sandbox))
    }

    #[test]
    fn child_orchestrator_spawn_requires_an_exact_parent_grant() -> TestResult {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let root = SessionId::new();
        let spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative())
            .with_role(
                "specialist",
                AgentRole::Agent {
                    name: "specialist".to_owned(),
                },
                harw_agent_dsl::roles::AgentRoleId::ChildOrchestrator,
                Arc::new(EmptyChildRegistry),
            )
            .with_external_root_parent(
                root.clone(),
                external_root_context(
                    sandbox.clone(),
                    harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
                ),
                None,
                SessionActivation::default(),
            )
            .map_err(ctx("external root registers"))?;

        let Err(error) = spawner.admit("specialist", spawn_input(root), sandbox, None) else {
            return Err(TestError::Unexpected(
                "a missing exact grant must deny child orchestration".to_owned(),
            ));
        };
        assert_eq!(
            error.message,
            "no delegation capability is available for this request"
        );
        Ok(())
    }

    #[test]
    fn a_role_that_forbids_grandchildren_is_still_admissible_as_a_child() -> TestResult {
        // `max_depth = 0` heißt „diese Rolle darf keine Kinder erzeugen" — es
        // heißt nicht, dass sie selbst nicht als Kind laufen darf. Genau daran
        // scheiterten die `security-*-triage`-Rollen.
        let (spawner, parent, sandbox) = ir_spawner(test_agent_ir(
            r#"
[spawn]
max_depth = 0
"#,
        )?)?;

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .map_err(ctx(
                "a role that forbids its own children is still admissible as a child",
            ))?;

        let record = spawner
            .child_record(&child)
            .ok_or(TestError::Missing("child is tracked"))?;
        assert_eq!(record.depth, 1);
        assert_eq!(
            record.depth_ceiling, 1,
            "max_depth = 0 auf Tiefe 1 deckelt die Nachkommen auf Tiefe 1 — also keine"
        );
        Ok(())
    }

    #[test]
    fn the_agent_ir_can_only_tighten_the_depth_limit() -> TestResult {
        // Verschärfen: `max_depth = 0` am Elternteil verbietet den Enkel,
        // obwohl die Controller-Grenze (4) ihn zuließe.
        let (spawner, root, sandbox) = two_hop_spawner(test_agent_ir(
            r#"
[spawn]
max_depth = 0
"#,
        )?)?;
        let child = spawner
            .admit("manager", spawn_input(root), sandbox.clone(), None)
            .map_err(ctx("the depth-0 role is itself admissible"))?;
        let Err(error) = spawner.admit("worker", spawn_input(child), sandbox, None) else {
            return Err(TestError::Unexpected(
                "a parent contract of max_depth = 0 forbids any grandchild".to_owned(),
            ));
        };
        assert_eq!(error.message, "child depth 2 exceeds maximum 1");

        // Erweitern: `max_depth = 99` hebt nichts an — die geerbte Decke bleibt
        // `ChildLimits::conservative().max_depth`.
        let (spawner, root, sandbox) = two_hop_spawner(test_agent_ir(
            r#"
[spawn]
max_depth = 99
"#,
        )?)?;
        let child = spawner
            .admit("manager", spawn_input(root), sandbox.clone(), None)
            .map_err(ctx("depth 1 is inside the conservative limit"))?;
        assert_eq!(
            spawner
                .child_record(&child)
                .ok_or(TestError::Missing("child is tracked"))?
                .depth_ceiling,
            ChildLimits::conservative().max_depth,
            "an IR value above the controller limit must not raise the inherited ceiling"
        );
        let grandchild = spawner
            .admit("worker", spawn_input(child), sandbox, None)
            .map_err(ctx("depth 2 is inside the conservative limit"))?;
        assert_eq!(
            spawner
                .child_record(&grandchild)
                .ok_or(TestError::Missing("grandchild is tracked"))?
                .depth,
            2
        );
        Ok(())
    }

    #[test]
    fn the_agent_ir_cannot_raise_the_controller_depth_limit() -> TestResult {
        // `max_depth = 99` in der Definition darf die konservative Grenze
        // nicht anheben — die Admission auf Tiefe 1 gelingt trotzdem, weil
        // `min(4, 99) = 4`.
        let (spawner, parent, sandbox) = ir_spawner(test_agent_ir(
            r#"
[spawn]
max_depth = 99
"#,
        )?)?;

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .map_err(ctx("depth 1 is inside the conservative limit"))?;

        assert_eq!(
            spawner
                .child_record(&child)
                .ok_or(TestError::Missing("child is tracked"))?
                .depth,
            1
        );
        Ok(())
    }

    #[test]
    fn an_unknown_effort_cap_rejects_the_admission_fail_closed() -> TestResult {
        let (spawner, parent, sandbox) = ir_spawner(test_agent_ir(
            r#"
[spawn.budget]
effort_cap = "ludicrous"
"#,
        )?)?;

        let Err(error) = spawner.admit("worker", spawn_input(parent), sandbox, None) else {
            return Err(TestError::Unexpected(
                "an unknown effort label must not fall back to a default".to_owned(),
            ));
        };

        assert!(
            error
                .message
                .contains("unknown reasoning-effort cap 'ludicrous'"),
            "unexpected message: {}",
            error.message
        );
        Ok(())
    }

    // --- AW1-01b: Trace-Vererbung -------------------------------------------

    fn test_trace(trace_id: &str, span_id: &str) -> TestResult<TraceContext> {
        TraceContext::new(trace_id.to_owned(), span_id.to_owned())
            .map_err(ctx("test trace is valid hex"))
    }

    // --- AW2-02: Kontext-Decken-Vererbung ------------------------------------

    fn test_ceiling(
        sections: &[&str],
        max_trust: TrustClass,
        budget_total: u32,
    ) -> TestResult<ContextCeiling> {
        Ok(ContextCeiling {
            sections: sections
                .iter()
                .map(|name| SectionName::try_new(*name).map_err(ctx("valid section name")))
                .collect::<TestResult<_>>()?,
            max_trust,
            budget: ContextBudgetSpec {
                total: harw_lens_types::BudgetSpec {
                    total: budget_total,
                },
                per_section: BTreeMap::new(),
            },
        })
    }

    #[test]
    fn admit_inherits_the_parents_trace_id_but_assigns_a_fresh_span_id() -> TestResult {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let parent = SessionId::new();
        let parent_trace = test_trace(&"a".repeat(32), &"b".repeat(16))?;
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
            .map_err(ctx("trusted external root registers during construction"))?;

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .map_err(ctx("child is admitted with an inherited trace"))?;

        let manager = spawner
            .manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let child_trace = manager
            .get(&child)
            .map_err(ctx("child is manager-owned"))?
            .spawn_context()
            .ok_or(TestError::Missing("child has a trusted spawn context"))?
            .trace
            .clone()
            .ok_or(TestError::Missing("child inherits a trace from its parent"))?;

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
        Ok(())
    }

    #[test]
    fn admit_gives_a_traceless_parent_a_traceless_child() -> TestResult {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
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
            .map_err(ctx("trusted external root registers during construction"))?;

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .map_err(ctx("child is admitted"))?;

        let manager = spawner
            .manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let child_context = manager
            .get(&child)
            .map_err(ctx("child is manager-owned"))?
            .spawn_context()
            .ok_or(TestError::Missing("child has a trusted spawn context"))?;

        assert!(
            child_context.trace.is_none(),
            "a parent without a trace must not hand its child a fabricated root trace"
        );
        Ok(())
    }

    #[test]
    fn admit_propagates_the_root_trace_id_across_a_grandchild() -> TestResult {
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
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let root_trace = test_trace(&"c".repeat(32), &"d".repeat(16))?;
        let root_context = SpawnContext {
            sandbox: sandbox.clone(),
            suggestions: None,
            capability_snapshot: None,
            approval_actor: None,
            organizational_role: harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
            allowed_child_orchestrators: vec!["manager".to_owned()],
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
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .restore(root_session)
            .map_err(ctx("root session restores"))?;

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
            .map_err(ctx("child is admitted with an inherited trace"))?;
        let grandchild = spawner
            .admit("worker", spawn_input(child), sandbox, None)
            .map_err(ctx("grandchild is admitted with an inherited trace"))?;

        let manager = manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let grandchild_trace = manager
            .get(&grandchild)
            .map_err(ctx("grandchild is manager-owned"))?
            .spawn_context()
            .ok_or(TestError::Missing("grandchild has a trusted spawn context"))?
            .trace
            .clone()
            .ok_or(TestError::Missing(
                "grandchild inherits a trace across two admission hops",
            ))?;

        assert_eq!(
            grandchild_trace.trace_id, root_trace.trace_id,
            "the grandchild must still carry the root's trace_id two hops down"
        );
        Ok(())
    }

    #[test]
    fn admit_inherits_the_parents_cut_context_ceiling_when_the_child_requests_none() -> TestResult {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let parent = SessionId::new();
        let parent_ceiling =
            test_ceiling(&["history.tail", "plan.current"], TrustClass::Evidence, 750)?;
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
            .map_err(ctx("trusted external root registers during construction"))?;

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .map_err(ctx(
                "a child requesting no ceiling of its own is always admissible",
            ))?;

        let manager = spawner
            .manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let child_ceiling = manager
            .get(&child)
            .map_err(ctx("child is manager-owned"))?
            .spawn_context()
            .ok_or(TestError::Missing("child has a trusted spawn context"))?
            .ceiling
            .clone()
            .ok_or(TestError::Missing(
                "child inherits a ceiling from its parent",
            ))?;

        assert_eq!(
            child_ceiling, parent_ceiling,
            "a child that requests no ceiling of its own must inherit its parent's, unchanged"
        );
        Ok(())
    }

    #[test]
    fn admit_rejects_a_child_that_requests_a_section_outside_the_parents_ceiling() -> TestResult {
        // The most important AW2-02 test: an over-reaching request must be
        // refused outright, not silently narrowed to fit.
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let parent = SessionId::new();
        let mut parent_context = external_root_context(
            sandbox.clone(),
            harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
        );
        parent_context.ceiling = Some(test_ceiling(&["history.tail"], TrustClass::Evidence, 500)?);
        let spawner = worker_spawner(manager)
            .with_external_root_parent(
                parent.clone(),
                parent_context,
                None,
                SessionActivation::default(),
            )
            .map_err(ctx("trusted external root registers during construction"))?;

        let requested = test_ceiling(
            &["history.tail", "secrets.vault"],
            TrustClass::Evidence,
            500,
        )?;
        let Err(error) = spawner.admit(
            "worker",
            spawn_input_requesting(parent, requested),
            sandbox,
            None,
        ) else {
            return Err(TestError::Unexpected(
                "a child must never be granted a section its parent's ceiling does not carry"
                    .to_owned(),
            ));
        };

        assert!(
            error.message.contains("secrets.vault"),
            "the rejection must name the exact offending section, got: {}",
            error.message
        );
        Ok(())
    }

    // --- Folgeknoten zu AW2-01/AW2-02: `ContextProgram` je Sitzung ----------

    #[test]
    fn admit_binds_the_roles_declared_context_program_to_the_child_session() -> TestResult {
        // Der wichtigste Test dieses Knotens: eine Rolle, deren Agent-IR ein
        // `[context]`-Programm deklariert, muss dieses Programm tatsächlich
        // auf ihrer Sitzung tragen — nicht bloß eine IR mit einem gefüllten
        // Feld haben, das nirgends ankommt.
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let parent = SessionId::new();
        let mut parent_context = external_root_context(
            sandbox.clone(),
            harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
        );
        parent_context.ceiling = Some(test_ceiling(
            &["history.tail", "plan.current"],
            TrustClass::Evidence,
            750,
        )?);
        let ir = test_agent_ir(
            r#"
[context]
must_include = ["history.tail"]
"#,
        )?;
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
            .map_err(ctx("trusted external root registers during construction"))?;

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .map_err(ctx(
                "a program that fits within the cut ceiling is admissible",
            ))?;

        let manager = spawner
            .manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session = manager.get(&child).map_err(ctx("child is manager-owned"))?;
        assert_eq!(
            session.context_program(),
            Some(ir.context_program()),
            "the child session must carry exactly the program its role declared"
        );
        Ok(())
    }

    #[test]
    fn admit_leaves_context_program_none_for_a_role_without_one() -> TestResult {
        // Eine Rolle ohne `[context]`-Tabelle verhält sich exakt unverändert:
        // `context_program()` bleibt `None`, egal ob die IR sonst Politik
        // trägt (hier: Tool-Surface).
        let (spawner, parent, sandbox) = ir_spawner(test_agent_ir(
            r#"
[tools]
admitted = ["fs.read"]
"#,
        )?)?;

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .map_err(ctx(
                "child without a declared context program is still admitted",
            ))?;

        let manager = spawner
            .manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session = manager.get(&child).map_err(ctx("child is manager-owned"))?;
        assert!(
            session.context_program().is_none(),
            "a role without a declared [context] table must not gain one along the way"
        );
        Ok(())
    }

    #[test]
    fn admit_rejects_a_declared_context_program_that_widens_the_cut_ceiling() -> TestResult {
        // Ein Programm darf die Decke nie erweitern: hier verlangt es
        // `secrets.vault`, das die (bereits geschnittene) Kind-Decke nicht
        // enthält.
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let parent = SessionId::new();
        let mut parent_context = external_root_context(
            sandbox.clone(),
            harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
        );
        parent_context.ceiling = Some(test_ceiling(&["history.tail"], TrustClass::Evidence, 500)?);
        let ir = test_agent_ir(
            r#"
[context]
must_include = ["secrets.vault"]
"#,
        )?;
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
            .map_err(ctx("trusted external root registers during construction"))?;

        let active_before = spawner
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len();

        let Err(error) = spawner.admit("worker", spawn_input(parent), sandbox, None) else {
            return Err(TestError::Unexpected(
                "a declared program must never widen the child's cut context ceiling".to_owned(),
            ));
        };

        assert!(
            error.message.contains("secrets.vault"),
            "the rejection must name the exact offending section, got: {}",
            error.message
        );

        // Belegt, dass kein Kind mit dem überzogenen Programm entstanden ist —
        // die Ablehnung darf keine halbfertige Sitzung hinterlassen.
        let active_after = spawner
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len();
        assert_eq!(
            active_before, active_after,
            "a rejected context-program escalation must not leave a partially admitted child behind"
        );
        Ok(())
    }

    #[test]
    fn cut_ceiling_is_idempotent() -> TestResult {
        let parent = test_ceiling(
            &["history.tail", "plan.current"],
            TrustClass::Instruction,
            1_000,
        )?;
        let requested = test_ceiling(&["history.tail"], TrustClass::Evidence, 400)?;

        let once = ManagedAgentSpawner::cut_ceiling(Some(&parent), Some(&requested))
            .map_err(ctx("the requested ceiling fits within the parent's"))?;
        let twice = ManagedAgentSpawner::cut_ceiling(Some(&once), Some(&once))
            .map_err(ctx("a ceiling already cut against itself must still fit"))?;

        assert_eq!(
            once, twice,
            "cutting an already-cut ceiling again must be a no-op"
        );
        Ok(())
    }

    #[test]
    fn admit_grandchild_ceiling_never_exceeds_the_roots_ceiling() -> TestResult {
        // Same manager-owned root/manager-child/worker-grandchild shape as
        // `admit_propagates_the_root_trace_id_across_a_grandchild` above —
        // the only two-hop path the §3 spawn matrix allows.
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events.clone())));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let root_ceiling = test_ceiling(
            &["history.tail", "plan.current"],
            TrustClass::Instruction,
            1_000,
        )?;
        let root_context = SpawnContext {
            sandbox: sandbox.clone(),
            suggestions: None,
            capability_snapshot: None,
            approval_actor: None,
            organizational_role: harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
            allowed_child_orchestrators: vec!["manager".to_owned()],
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
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .restore(root_session)
            .map_err(ctx("root session restores"))?;

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

        let child_request = test_ceiling(&["history.tail"], TrustClass::Evidence, 400)?;
        let child = spawner
            .admit(
                "manager",
                spawn_input_requesting(root, child_request),
                sandbox.clone(),
                None,
            )
            .map_err(ctx("child is admitted with a narrower ceiling"))?;

        let grandchild_request = test_ceiling(&["history.tail"], TrustClass::Data, 100)?;
        let grandchild = spawner
            .admit(
                "worker",
                spawn_input_requesting(child, grandchild_request.clone()),
                sandbox,
                None,
            )
            .map_err(ctx("grandchild is admitted with a narrower ceiling still"))?;

        let manager = manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let grandchild_ceiling = manager
            .get(&grandchild)
            .map_err(ctx("grandchild is manager-owned"))?
            .spawn_context()
            .ok_or(TestError::Missing("grandchild has a trusted spawn context"))?
            .ceiling
            .clone()
            .ok_or(TestError::Missing(
                "grandchild inherits a ceiling across two admission hops",
            ))?;

        assert_eq!(
            grandchild_ceiling, grandchild_request,
            "the grandchild's own narrower request must be exactly what it ends up with"
        );
        assert!(
            grandchild_ceiling
                .sections
                .is_subset(&root_ceiling.sections),
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
        Ok(())
    }

    #[test]
    fn admit_cuts_the_ceiling_alongside_the_sandbox_in_one_admission() -> TestResult {
        // Proves the "same step" promise directly: one successful admission,
        // both the sandbox and the ceiling checked from its one result.
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let parent_sandbox = test_sandbox(PermissionSet::from_policy([
            Permission::ReadWorkspace,
            Permission::WriteWorkspace,
        ]))?;
        let child_sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let parent = SessionId::new();
        let mut parent_context = external_root_context(
            parent_sandbox.clone(),
            harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
        );
        parent_context.ceiling = Some(test_ceiling(
            &["history.tail", "plan.current"],
            TrustClass::Instruction,
            1_000,
        )?);
        let requested_ceiling = test_ceiling(&["history.tail"], TrustClass::Evidence, 200)?;
        let spawner = worker_spawner(manager)
            .with_external_root_parent(
                parent.clone(),
                parent_context,
                None,
                SessionActivation::default(),
            )
            .map_err(ctx("trusted external root registers during construction"))?;

        let child = spawner
            .admit(
                "worker",
                spawn_input_requesting(parent, requested_ceiling.clone()),
                child_sandbox,
                None,
            )
            .map_err(ctx(
                "a narrower sandbox and a narrower ceiling are both admissible together",
            ))?;

        let manager = spawner
            .manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let child_context = manager
            .get(&child)
            .map_err(ctx("child is manager-owned"))?
            .spawn_context()
            .ok_or(TestError::Missing("child has a trusted spawn context"))?;

        assert!(
            child_context
                .sandbox
                .ensure_child_of(&parent_sandbox)
                .is_ok(),
            "the child's sandbox must be a valid narrowing of the parent's"
        );
        assert_eq!(
            child_context.ceiling,
            Some(requested_ceiling),
            "the child's ceiling must be cut in the very same admission that narrows the sandbox"
        );
        Ok(())
    }

    #[test]
    fn admit_rejecting_a_ceiling_escalation_leaves_no_partial_child_behind() -> TestResult {
        // The sandbox check above the ceiling cut already passed by the time
        // the ceiling is checked; this proves that a ceiling rejection still
        // aborts the whole admission — no observable intermediate state.
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let parent = SessionId::new();
        let mut parent_context = external_root_context(
            sandbox.clone(),
            harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
        );
        parent_context.ceiling = Some(test_ceiling(&["history.tail"], TrustClass::Data, 100)?);
        let spawner = worker_spawner(manager)
            .with_external_root_parent(
                parent.clone(),
                parent_context,
                None,
                SessionActivation::default(),
            )
            .map_err(ctx("trusted external root registers during construction"))?;

        let active_before = spawner
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len();

        let escalating_request = test_ceiling(&["history.tail"], TrustClass::Instruction, 100)?;
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
        let active_after = spawner
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len();
        assert_eq!(
            active_before, active_after,
            "a rejected ceiling escalation must not leave a partially admitted child behind"
        );
        Ok(())
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
    fn context_escalation_attacker_fixture_is_rejected() -> TestResult {
        assert!(
            CONTEXT_ESCALATION_ATTACKER_TOML.contains("max_trust = \"instruction\""),
            "fixture drifted from what this test actually exercises"
        );

        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let parent = SessionId::new();
        let mut parent_context = external_root_context(
            sandbox.clone(),
            harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
        );
        parent_context.ceiling = Some(test_ceiling(&["history.tail"], TrustClass::Evidence, 500)?);
        let spawner = worker_spawner(manager)
            .with_external_root_parent(
                parent.clone(),
                parent_context,
                None,
                SessionActivation::default(),
            )
            .map_err(ctx("trusted external root registers during construction"))?;

        // Die von der Fixture beschriebene Forderung, in Rust nachgebaut
        // (siehe Doc-Kommentar oben): dieselben Sektionen, aber
        // `max_trust = "instruction"` statt der vom Elternteil erlaubten
        // `Evidence`-Obergrenze.
        let attacker_ceiling = test_ceiling(&["history.tail"], TrustClass::Instruction, 500)?;

        let Err(error) = spawner.admit(
            "worker",
            spawn_input_requesting(parent, attacker_ceiling),
            sandbox,
            None,
        ) else {
            return Err(TestError::Unexpected(
                "a child must never be admitted with a higher max_trust than its parent's ceiling"
                    .to_owned(),
            ));
        };

        assert!(
            error.message.contains("max_trust"),
            "the rejection must name the violated aspect (max_trust), got: {}",
            error.message
        );
        Ok(())
    }

    #[test]
    fn durable_lease_carries_the_inherited_trace_into_the_child_lease_record() -> TestResult {
        let trace = test_trace(&"e".repeat(32), &"f".repeat(16))?;
        let now = Timestamp::now();
        let record = ChildRecord {
            live: crate::child_controller::ChildLiveStats::default(),
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
            status: ChildStatus::Admitted,
            task_complexity: None,
        };

        let lease = record.durable_lease();

        assert_eq!(
            lease.trace,
            Some(trace),
            "durable_lease must carry the record's inherited trace, not drop it"
        );
        Ok(())
    }

    #[test]
    fn admit_with_a_lease_store_persists_the_inherited_trace_to_disk() -> TestResult {
        let temporary_directory = tempfile::tempdir().map_err(ctx("temporary lease directory"))?;
        let lease_store = Arc::new(ChildLeaseStore::new(temporary_directory.path()));
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let parent = SessionId::new();
        let parent_trace = test_trace(&"1".repeat(32), &"2".repeat(16))?;
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
            .map_err(ctx("trusted external root registers during construction"))?;

        spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .map_err(ctx("child is admitted and its lease is durably recorded"))?;

        let active_leases = lease_store
            .active()
            .map_err(ctx("active leases are readable"))?;
        assert_eq!(active_leases.len(), 1);
        let persisted_trace = active_leases[0].trace.clone().ok_or(TestError::Missing(
            "the durable lease on disk carries the inherited trace",
        ))?;
        assert_eq!(persisted_trace.trace_id, parent_trace.trace_id);
        assert_eq!(
            persisted_trace.parent_span_id.as_deref(),
            Some(parent_trace.span_id.as_str())
        );
        Ok(())
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
    ) -> TestResult<(ManagedAgentSpawner, SessionId, SandboxSpec)> {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
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
            .map_err(ctx("trusted external root registers during construction"))?;
        Ok((spawner, parent, sandbox))
    }

    /// Die Tool-Oberfläche einer Rolle, die `fs.read` **und** `shell.exec`
    /// zulässt — genug, um einen fehlenden Schnitt sichtbar zu machen.
    fn read_and_exec_ir() -> TestResult<ExecutableAgentIr> {
        test_agent_ir(
            r#"
[tools]
admitted = ["fs.read", "shell.exec"]
"#,
        )
    }

    #[test]
    fn child_activation_is_cut_with_the_parent_activation() -> TestResult {
        let mut parent_activation = SessionActivation::new(ToolProfile::Full);
        parent_activation.disable_tool(ToolName::new("shell.exec"));
        let (spawner, parent, sandbox) =
            ir_spawner_with_parent_activation(read_and_exec_ir()?, parent_activation)?;

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .map_err(ctx("the child is admitted"))?;

        let manager = spawner
            .manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let activation = manager
            .get(&child)
            .map_err(ctx("child is manager-owned"))?
            .activation();
        assert!(
            activation.is_tool_enabled(&ToolName::new("fs.read")),
            "ein vom Elternteil erlaubtes und von der Rolle zugelassenes Werkzeug bleibt offen"
        );
        assert!(
            !activation.is_tool_enabled(&ToolName::new("shell.exec")),
            "die Rolle lässt shell.exec zu, der Elternteil nicht — der Schnitt entscheidet"
        );
        Ok(())
    }

    #[test]
    fn parent_activation_cut_survives_a_later_mode_switch() -> TestResult {
        // Der Schnitt ist eine Autoritätsgrenze, kein Laufzeit-Override: er
        // liegt in der Basis und muss deshalb jeden `set_mode` überleben.
        // `Work` ist der schärfste Fall — `ToolProfile::Full` ohne
        // Allowlist, also die weiteste Modus-Decke überhaupt.
        let mut parent_activation = SessionActivation::new(ToolProfile::Full);
        parent_activation.disable_tool(ToolName::new("shell.exec"));
        let (spawner, parent, sandbox) =
            ir_spawner_with_parent_activation(read_and_exec_ir()?, parent_activation)?;

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .map_err(ctx("the child is admitted"))?;

        let mut manager = spawner
            .manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let child_session = manager
            .get_mut(&child)
            .map_err(ctx("child is manager-owned"))?;
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
        Ok(())
    }

    #[test]
    fn grandchild_activation_inherits_the_whole_intersection_chain() -> TestResult {
        // Die Wurzel ist hier bewusst **manager-eigen**: nur so ist sie für
        // `parent_depth` auflösbar, und nur so belegt der Test die zweite
        // Bezugsquelle des Schnitts — bei einem Kind eines Kindes ist der
        // Elternteil die Sitzung im Manager, deren Aktivierung `admit` über
        // `AgentSession::activation()` liest.
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events.clone())));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;

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
        .with_spawn_context({
            let mut context = external_root_context(
                sandbox.clone(),
                harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
            );
            context
                .allowed_child_orchestrators
                .push("middle".to_owned());
            context
        })
        .with_activation(root_activation);
        let root = root_session.id().clone();
        manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .restore(root_session)
            .map_err(ctx("test root session restores"))?;

        let spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative())
            .with_role(
                "middle",
                AgentRole::Agent {
                    name: "middle".to_owned(),
                },
                harw_agent_dsl::roles::AgentRoleId::ChildOrchestrator,
                Arc::new(IrChildRegistry {
                    ir: read_and_exec_ir()?,
                }),
            )
            .with_role(
                "worker",
                AgentRole::Agent {
                    name: "worker".to_owned(),
                },
                harw_agent_dsl::roles::AgentRoleId::Worker,
                Arc::new(IrChildRegistry {
                    ir: read_and_exec_ir()?,
                }),
            );

        let middle = spawner
            .admit("middle", spawn_input(root), sandbox.clone(), None)
            .map_err(ctx("the child orchestrator is admitted"))?;
        let grandchild = spawner
            .admit("worker", spawn_input(middle.clone()), sandbox, None)
            .map_err(ctx(
                "the grandchild is admitted below the child orchestrator",
            ))?;

        let manager = spawner
            .manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (label, session_id) in [("Kind", &middle), ("Enkel", &grandchild)] {
            let activation = manager
                .get(session_id)
                .map_err(ctx("session is manager-owned"))?
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
        Ok(())
    }

    #[test]
    fn parent_activation_cut_also_applies_without_an_agent_ir() -> TestResult {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
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
            .map_err(ctx("trusted external root registers during construction"))?;

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .map_err(ctx("the child is admitted"))?;

        let manager = spawner
            .manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let activation = manager
            .get(&child)
            .map_err(ctx("child is manager-owned"))?
            .activation();
        assert!(!activation.is_tool_enabled(&ToolName::new("shell.exec")));
        assert!(!activation.is_context_enabled("workspace_files"));
        assert!(activation.is_tool_enabled(&ToolName::new("fs.read")));
        Ok(())
    }

    #[test]
    fn effort_clamp_without_an_inherited_base_falls_back_to_the_default() -> TestResult {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
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
            .map_err(ctx("trusted external root registers during construction"))?;

        let capped = spawner
            .admit("worker", spawn_input(parent.clone()), sandbox.clone(), None)
            .map_err(ctx("first child is admitted"))?;
        // Seit Addendum F+G liefert `RoleEffortWeights` auch ohne geerbte
        // Eltern-Basis ein Rollengewicht (Default: `worker_complex` = Medium
        // für einen `Worker` ohne bekannte Komplexität) — die Erwartung wird
        // deshalb aus derselben Gewichtsquelle abgeleitet statt hart kodiert.
        let expected_role_weight = RoleEffortWeights::default().for_child(
            harw_agent_dsl::roles::AgentRoleId::Worker,
            false,
            None,
        );
        assert_eq!(
            spawner
                .manager
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&capped)
                .map_err(ctx("child is manager-owned"))?
                .reasoning_effort(),
            Some(expected_role_weight),
            "ohne Eltern-Level erbt das Kind bei der Admission das Rollengewicht aus RoleEffortWeights::default()"
        );
        assert_eq!(
            spawner
                .clamp_child_reasoning_effort(&capped, Some(ReasoningEffort::Low), None)
                .map_err(ctx("known child clamps cleanly"))?,
            Some(ReasoningEffort::Low),
            "der Deckel der Agent-IR greift jetzt auch ohne geerbte Basis"
        );

        let uncapped = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .map_err(ctx("second child is admitted"))?;
        assert_eq!(
            spawner
                .clamp_child_reasoning_effort(&uncapped, None, None)
                .map_err(ctx("known child clamps cleanly"))?,
            Some(DEFAULT_CHILD_REASONING_EFFORT),
            "ohne Deckel und ohne Basis gilt der Default, nicht der Provider-Default"
        );
        Ok(())
    }

    #[test]
    fn admit_uses_provider_default_reasoning_effort_over_role_weight() -> TestResult {
        // Welle 8: Provider > Modell > Agent > Rolle. Der Rollen-Standard für
        // `Worker` ohne bekannte Komplexität ist `Medium`
        // ([`RoleEffortWeights::default`]); ein Provider-Default `Xhigh`
        // (höher, aber die geerbte Eltern-Basis unten deckelt trotzdem nicht,
        // da sie selbst `Xhigh` führt) muss ihn bei der Admission
        // überschreiben.
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let parent = SessionId::new();
        let spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative())
            .with_role(
                "worker",
                AgentRole::Agent {
                    name: "worker".to_owned(),
                },
                harw_agent_dsl::roles::AgentRoleId::Worker,
                Arc::new(FixedReasoningEffortDefaultsRegistry {
                    provider_default: Some(ReasoningEffort::Xhigh),
                    model_default: None,
                }),
            )
            .with_external_root_parent(
                parent.clone(),
                external_root_context(
                    sandbox.clone(),
                    harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
                ),
                Some(ReasoningEffort::Xhigh),
                SessionActivation::default(),
            )
            .map_err(ctx("trusted external root registers during construction"))?;

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .map_err(ctx("child is admitted"))?;

        assert_eq!(
            spawner
                .manager
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&child)
                .map_err(ctx("child is manager-owned"))?
                .reasoning_effort(),
            Some(ReasoningEffort::Xhigh),
            "provider default (Xhigh) must win over the Worker role weight (Medium)"
        );
        Ok(())
    }

    #[test]
    fn admit_falls_back_to_role_weight_when_provider_and_model_are_silent() -> TestResult {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let parent = SessionId::new();
        let spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative())
            .with_role(
                "worker",
                AgentRole::Agent {
                    name: "worker".to_owned(),
                },
                harw_agent_dsl::roles::AgentRoleId::Worker,
                Arc::new(FixedReasoningEffortDefaultsRegistry {
                    provider_default: None,
                    model_default: None,
                }),
            )
            .with_external_root_parent(
                parent.clone(),
                external_root_context(
                    sandbox.clone(),
                    harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
                ),
                Some(ReasoningEffort::Xhigh),
                SessionActivation::default(),
            )
            .map_err(ctx("trusted external root registers during construction"))?;

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .map_err(ctx("child is admitted"))?;

        let expected_role_weight = RoleEffortWeights::default().for_child(
            harw_agent_dsl::roles::AgentRoleId::Worker,
            false,
            None,
        );
        assert_eq!(
            spawner
                .manager
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&child)
                .map_err(ctx("child is manager-owned"))?
                .reasoning_effort(),
            Some(expected_role_weight),
            "without a provider/model default, the role weight remains the floor"
        );
        Ok(())
    }

    #[test]
    fn effort_clamp_owner_override_still_beats_the_default_base() -> TestResult {
        let (spawner, child) = spawner_with_admitted_child()?;

        let effective = spawner
            .clamp_child_reasoning_effort(
                &child,
                Some(ReasoningEffort::Minimal),
                Some(ReasoningEffort::High),
            )
            .map_err(ctx("known child clamps cleanly"))?;

        assert_eq!(effective, Some(ReasoningEffort::High));
        Ok(())
    }

    // ── parent_organizational_role ──────────────────────────────────────

    /// Trägt einen [`ChildRecord`] mit `parent` direkt in `spawner.active`
    /// ein, ohne den üblichen `admit`-Pfad zu durchlaufen — für Tests von
    /// [`ManagedAgentSpawner::parent_organizational_role`], die nur die
    /// Eltern-Auflösung isoliert prüfen wollen.
    fn install_child_record(spawner: &ManagedAgentSpawner, parent: SessionId) -> SessionId {
        let child = SessionId::new();
        let now = Timestamp::now();
        spawner
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                child.as_str().to_owned(),
                ChildRecord {
                    live: crate::child_controller::ChildLiveStats::default(),
                    child: child.clone(),
                    parent,
                    handoff_call_id: ToolCallId::new(),
                    role: "worker".to_owned(),
                    depth: 1,
                    admitted_at: now,
                    lease_expires_at: now,
                    budget: AgentBudget::default(),
                    allow_pause: false,
                    depth_ceiling: ChildLimits::conservative().max_depth,
                    trace: None,
                    status: ChildStatus::Admitted,
                    task_complexity: None,
                },
            );
        child
    }

    #[test]
    fn test_parent_organizational_role_manager_owned_parent_returns_role() -> TestResult {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events.clone())));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let parent_session = AgentSession::new(
            AgentRole::Agent {
                name: "root".to_owned(),
            },
            None,
            ExtensionRegistryBuilder::default().build(),
            events,
        )
        .with_spawn_context(external_root_context(
            sandbox,
            harw_agent_dsl::roles::AgentRoleId::UserInterface,
        ));
        let parent_id = parent_session.id().clone();
        manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .restore(parent_session)
            .map_err(ctx("parent session restores"))?;

        let spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative());
        let child = install_child_record(&spawner, parent_id);

        assert_eq!(
            spawner.parent_organizational_role(&child),
            Some(harw_agent_dsl::roles::AgentRoleId::UserInterface)
        );
        Ok(())
    }

    #[test]
    fn test_parent_organizational_role_manager_parent_without_spawn_context_returns_none()
    -> TestResult {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events.clone())));
        // No `.with_spawn_context(...)`: a manager-owned parent that never
        // received trusted admission metadata.
        let parent_session = AgentSession::new(
            AgentRole::Agent {
                name: "root".to_owned(),
            },
            None,
            ExtensionRegistryBuilder::default().build(),
            events,
        );
        let parent_id = parent_session.id().clone();
        manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .restore(parent_session)
            .map_err(ctx("parent session restores"))?;

        let spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative());
        let child = install_child_record(&spawner, parent_id);

        assert_eq!(spawner.parent_organizational_role(&child), None);
        Ok(())
    }

    #[test]
    fn test_parent_organizational_role_external_root_parent_returns_role() -> TestResult {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
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
            .map_err(ctx("trusted external root registers during construction"))?;

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .map_err(ctx("registered external root admits a direct child"))?;

        assert_eq!(
            spawner.parent_organizational_role(&child),
            Some(harw_agent_dsl::roles::AgentRoleId::RootOrchestrator)
        );
        Ok(())
    }

    #[test]
    fn test_parent_organizational_role_orphaned_parent_returns_none() -> TestResult {
        // `spawner_with_admitted_child` records a random, never-registered
        // parent and registers no external root parent either.
        let (spawner, child) = spawner_with_admitted_child()?;

        assert_eq!(spawner.parent_organizational_role(&child), None);
        Ok(())
    }

    #[test]
    fn test_parent_organizational_role_unknown_child_returns_none() -> TestResult {
        let (spawner, _child) = spawner_with_admitted_child()?;

        assert_eq!(spawner.parent_organizational_role(&SessionId::new()), None);
        Ok(())
    }

    // ── admit_or_wait / spawn_child_or_wait ────────────────────────────────

    /// Baut einen [`ManagedAgentSpawner`] mit genau einem Kapazitäts-Slot je
    /// Elternteil (`max_active_children_per_parent = 1`) — der `explore`-
    /// Regressionsfall (mehrere parallele Kind-Aufrufe desselben Elternteils)
    /// mit dem kleinstmöglichen Deckel.
    fn single_slot_spawner() -> TestResult<(Arc<ManagedAgentSpawner>, SessionId, SandboxSpec)> {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let parent = SessionId::new();
        let limits = ChildLimits {
            max_active_children_per_parent: 1,
            ..ChildLimits::conservative()
        };
        let spawner = worker_spawner_with_limits(manager, limits)
            .with_external_root_parent(
                parent.clone(),
                external_root_context(
                    sandbox.clone(),
                    harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
                ),
                None,
                SessionActivation::default(),
            )
            .map_err(ctx("trusted external root registers during construction"))?;
        Ok((Arc::new(spawner), parent, sandbox))
    }

    #[tokio::test]
    async fn test_admit_or_wait_waits_and_succeeds_after_release() -> TestResult {
        let (spawner, parent, sandbox) = single_slot_spawner()?;
        let first_child = spawner
            .admit("worker", spawn_input(parent.clone()), sandbox.clone(), None)
            .map_err(ctx("the single slot admits the first child"))?;

        let waiter_spawner = Arc::clone(&spawner);
        let waiter_parent = parent.clone();
        let waiter_sandbox = sandbox.clone();
        let handle = tokio::spawn(async move {
            let cancel = CancelToken::new();
            waiter_spawner
                .admit_or_wait(
                    "worker",
                    spawn_input(waiter_parent),
                    waiter_sandbox,
                    None,
                    std::time::Duration::from_secs(5),
                    &cancel,
                )
                .await
        });

        // Let the waiter make its first (rejected) attempt and reach the
        // `tokio::select!` awaiting `freed.notified()` before the slot frees.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        spawner
            .release_child(&first_child)
            .map_err(ctx("releasing the first child frees the only slot"))?;

        // Bounded well under the 500ms fallback sleep inside `admit_or_wait`:
        // this only passes if `release_in_memory`'s `notify_waiters()`
        // actually woke the waiter, not the periodic poll.
        let second_child = tokio::time::timeout(std::time::Duration::from_millis(300), handle)
            .await
            .map_err(ctx(
                "admit_or_wait must be woken by the release notification, not time out",
            ))?
            .map_err(ctx("waiter task must not panic"))?
            .map_err(ctx(
                "capacity frees, so the second admission must eventually succeed",
            ))?;

        assert_ne!(second_child, first_child);
        assert!(spawner.child_record(&second_child).is_some());
        Ok(())
    }

    #[tokio::test]
    async fn test_admit_or_wait_times_out_with_unchanged_capacity_error() -> TestResult {
        let (spawner, parent, sandbox) = single_slot_spawner()?;
        let _first_child = spawner
            .admit("worker", spawn_input(parent.clone()), sandbox.clone(), None)
            .map_err(ctx("the single slot admits the first child"))?;

        // The immediate rejection `admit` itself would give — `admit_or_wait`
        // must return this exact message once it gives up, byte-identical.
        let Err(direct_rejection) =
            spawner.admit("worker", spawn_input(parent.clone()), sandbox.clone(), None)
        else {
            return Err(TestError::Unexpected(
                "admit itself must still reject immediately at capacity".to_owned(),
            ));
        };

        let cancel = CancelToken::new();
        let Err(waited_rejection) = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            spawner.admit_or_wait(
                "worker",
                spawn_input(parent),
                sandbox,
                None,
                std::time::Duration::from_millis(120),
                &cancel,
            ),
        )
        .await
        .map_err(ctx(
            "admit_or_wait must give up once max_wait elapses, not hang",
        ))?
        else {
            return Err(TestError::Unexpected(
                "capacity never frees in this test".to_owned(),
            ));
        };

        assert_eq!(waited_rejection.message, direct_rejection.message);
        Ok(())
    }

    #[tokio::test]
    async fn test_admit_or_wait_cancellation_returns_promptly() -> TestResult {
        let (spawner, parent, sandbox) = single_slot_spawner()?;
        let first_child = spawner
            .admit("worker", spawn_input(parent.clone()), sandbox.clone(), None)
            .map_err(ctx("the single slot admits the first child"))?;

        let cancel = CancelToken::new();
        let waiter_spawner = Arc::clone(&spawner);
        let waiter_cancel = cancel.clone();
        let waiter_parent = parent.clone();
        let waiter_sandbox = sandbox.clone();
        let handle = tokio::spawn(async move {
            waiter_spawner
                .admit_or_wait(
                    "worker",
                    spawn_input(waiter_parent),
                    waiter_sandbox,
                    None,
                    std::time::Duration::from_secs(30),
                    &waiter_cancel,
                )
                .await
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        cancel.cancel(CancelReason::User);

        let Err(rejection) = tokio::time::timeout(std::time::Duration::from_millis(300), handle)
            .await
            .map_err(ctx(
                "cancellation must end the wait promptly, not time out at max_wait (30s)",
            ))?
            .map_err(ctx("waiter task must not panic"))?
        else {
            return Err(TestError::Unexpected(
                "a cancelled wait must not admit a child".to_owned(),
            ));
        };

        assert!(
            rejection.message.contains("cancelled"),
            "cancellation rejection must name cancellation, not capacity: {}",
            rejection.message
        );
        // The first child's own slot is untouched by the second caller's
        // cancellation — cancellation only abandons the *waiting* attempt.
        assert!(spawner.child_record(&first_child).is_some());
        Ok(())
    }

    #[tokio::test]
    async fn test_admit_or_wait_returns_non_capacity_rejection_immediately() -> TestResult {
        // Reuses the depth-limit fixture from
        // `the_agent_ir_can_only_tighten_the_depth_limit`: `max_depth = 0` on
        // the admitted "manager" forbids any grandchild — a rejection that
        // has nothing to do with capacity and so must never be retried.
        let (spawner, root, sandbox) = two_hop_spawner(test_agent_ir(
            r#"
[spawn]
max_depth = 0
"#,
        )?)?;
        let child = spawner
            .admit("manager", spawn_input(root), sandbox.clone(), None)
            .map_err(ctx("the depth-0 role is itself admissible"))?;

        let cancel = CancelToken::new();
        let Err(rejection) = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            spawner.admit_or_wait(
                "worker",
                spawn_input(child),
                sandbox,
                None,
                std::time::Duration::from_secs(30),
                &cancel,
            ),
        )
        .await
        .map_err(ctx(
            "a non-capacity rejection must return immediately, not wait out max_wait (30s)",
        ))?
        else {
            return Err(TestError::Unexpected(
                "a parent contract of max_depth = 0 forbids any grandchild".to_owned(),
            ));
        };

        assert_eq!(rejection.message, "child depth 2 exceeds maximum 1");
        Ok(())
    }

    // --- Runde 2 / Welle 1: Auftrag, Detail, Volltext, ChildProgress -------

    #[test]
    fn admission_seeds_pending_task_from_instructions_and_context() -> TestResult {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let parent = SessionId::new();
        let spawner = worker_spawner(manager)
            .with_external_root_parent(
                parent.clone(),
                external_root_context(
                    sandbox.clone(),
                    harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
                ),
                Some(ReasoningEffort::Medium),
                SessionActivation::default(),
            )
            .map_err(ctx("trusted external root registers during construction"))?;

        let mut with_instructions = spawn_input(parent.clone());
        with_instructions.instructions = Some("Analysiere das Modul".to_owned());
        let first = spawner
            .admit("worker", with_instructions, sandbox.clone(), None)
            .map_err(ctx("child with instructions admits"))?;
        let state = spawner
            .child_task_state(&first)
            .ok_or(TestError::Missing("task state is seeded"))?;
        assert_eq!(state.pending_task.as_deref(), Some("Analysiere das Modul"));
        assert_eq!(state.task.as_deref(), Some("Analysiere das Modul"));

        let mut with_context = spawn_input(parent.clone());
        with_context.context = serde_json::json!({"question": "Wo liegt der Fehler?"});
        let second = spawner
            .admit("worker", with_context, sandbox.clone(), None)
            .map_err(ctx("child with context admits"))?;
        let state = spawner
            .child_task_state(&second)
            .ok_or(TestError::Missing("task state is seeded"))?;
        assert_eq!(
            state.pending_task.as_deref(),
            Some(r#"{"question":"Wo liegt der Fehler?"}"#)
        );

        let third = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .map_err(ctx("child without task admits"))?;
        let state = spawner
            .child_task_state(&third)
            .ok_or(TestError::Missing("task state exists"))?;
        assert_eq!(state.pending_task, None);
        assert_eq!(state.task, None);
        Ok(())
    }

    /// Liest alle User-Texte aus dem Verlauf eines Kindes.
    fn child_user_texts(
        spawner: &ManagedAgentSpawner,
        child: &SessionId,
    ) -> TestResult<Vec<String>> {
        let manager = spawner
            .manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session = manager.get(child).map_err(ctx("child is manager-owned"))?;
        Ok(session
            .history()
            .items()
            .iter()
            .filter_map(|item| match item {
                TurnItem::UserMessage(message) => Some(
                    message
                        .content
                        .iter()
                        .filter_map(|part| match part {
                            ContentPart::Text { text } => Some(text.as_str()),
                            ContentPart::ImageUrl { .. } => None,
                        })
                        .collect::<String>(),
                ),
                _ => None,
            })
            .collect())
    }

    fn seed_pending_task(spawner: &ManagedAgentSpawner, child: &SessionId, task: &str) {
        spawner
            .child_tasks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                child.as_str().to_owned(),
                ChildTaskState {
                    pending_task: Some(task.to_owned()),
                    task: orchestration_detail_head(task),
                    outcome_detail: None,
                },
            );
    }

    #[tokio::test]
    async fn first_run_with_empty_input_receives_the_pending_task() -> TestResult {
        let (spawner, children) = runnable_children(
            Arc::new(EchoChildRegistry { reply: "erledigt" }),
            true,
            1,
            empty_registry,
        )?;
        let child = children[0].clone();
        seed_pending_task(&spawner, &child, "Fasse die Datei zusammen");
        let store = InMemoryStateStore::new();

        let result = spawner
            .run_child(&child, &store, TurnInput::default())
            .await
            .map_err(ctx("child with a pending task runs"))?;

        assert!(matches!(result.outcome, TurnOutcome::Completed));
        let texts = child_user_texts(&spawner, &child)?;
        assert!(
            texts.iter().any(|text| text == "Fasse die Datei zusammen"),
            "pending task must become the user turn: {texts:?}"
        );
        let state = spawner
            .child_task_state(&child)
            .ok_or(TestError::Missing("task state remains until release"))?;
        assert_eq!(
            state.pending_task, None,
            "the task is consumed exactly once"
        );
        assert_eq!(result.full_text.as_deref(), Some("erledigt"));
        assert_eq!(state.outcome_detail.as_deref(), Some("erledigt"));
        Ok(())
    }

    #[tokio::test]
    async fn explicit_user_text_wins_and_still_consumes_the_pending_task() -> TestResult {
        let (spawner, children) = runnable_children(
            Arc::new(EchoChildRegistry { reply: "erledigt" }),
            true,
            1,
            empty_registry,
        )?;
        let child = children[0].clone();
        seed_pending_task(&spawner, &child, "Auftrag aus der Admission");
        let store = InMemoryStateStore::new();

        spawner
            .run_child(&child, &store, TurnInput::user("expliziter Auftrag"))
            .await
            .map_err(ctx("child runs with explicit text"))?;

        let texts = child_user_texts(&spawner, &child)?;
        assert!(texts.iter().any(|text| text == "expliziter Auftrag"));
        assert!(
            !texts
                .iter()
                .any(|text| text.contains("Auftrag aus der Admission")),
            "explicit user text must not be replaced: {texts:?}"
        );
        let state = spawner
            .child_task_state(&child)
            .ok_or(TestError::Missing("task state remains until release"))?;
        assert_eq!(state.pending_task, None);
        Ok(())
    }

    #[tokio::test]
    async fn full_text_is_uncapped_while_the_plain_return_is_capped() -> TestResult {
        let long: &'static str = Box::leak("x".repeat(CHILD_RETURN_MAX_BYTES * 2).into_boxed_str());
        let (spawner, children) = runnable_children(
            Arc::new(EchoChildRegistry { reply: long }),
            true,
            1,
            empty_registry,
        )?;
        let child = children[0].clone();
        let store = InMemoryStateStore::new();

        let result = spawner
            .run_child(&child, &store, TurnInput::user("liefere viel Text"))
            .await
            .map_err(ctx("child completes"))?;

        assert_eq!(result.full_text.as_deref(), Some(long));
        let capped = spawner
            .child_final_assistant_text(&child)
            .map_err(ctx("capped text is available"))?;
        assert!(capped.len() < long.len());
        assert!(capped.contains("gekürzt"));
        let detail = spawner
            .child_task_state(&child)
            .and_then(|state| state.outcome_detail)
            .ok_or(TestError::Missing("completion detail is recorded"))?;
        assert_eq!(detail.chars().count(), ORCHESTRATION_DETAIL_MAX_CHARS);
        Ok(())
    }

    #[test]
    fn orchestration_detail_head_truncates_on_char_boundaries() {
        assert_eq!(orchestration_detail_head(""), None);
        assert_eq!(orchestration_detail_head(" \n\t "), None);
        assert_eq!(
            orchestration_detail_head("  genau so  "),
            Some("genau so".to_owned())
        );

        let exact = "é".repeat(ORCHESTRATION_DETAIL_MAX_CHARS);
        assert_eq!(orchestration_detail_head(&exact), Some(exact.clone()));

        let long = "日本".repeat(ORCHESTRATION_DETAIL_MAX_CHARS);
        let head = orchestration_detail_head(&long).unwrap_or_default();
        assert_eq!(head.chars().count(), ORCHESTRATION_DETAIL_MAX_CHARS);
        assert!(head.ends_with('…'));
        assert!(long.starts_with(head.trim_end_matches('…')));
    }

    /// Beobachter, der jedes Orchestrierungs-Event aufzeichnet.
    #[derive(Default)]
    struct RecordingOrchestrationObserver {
        events: Mutex<Vec<AgentOrchestrationEvent>>,
    }

    impl OrchestrationObserver for RecordingOrchestrationObserver {
        fn on_orchestration_event(&self, event: AgentOrchestrationEvent) {
            self.events
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(event);
        }
    }

    #[tokio::test]
    async fn failed_release_event_carries_task_and_truncated_reason() -> TestResult {
        let (spawner, children) =
            runnable_children(Arc::new(ToolCallingChildRegistry), true, 1, empty_registry)?;
        let observer = Arc::new(RecordingOrchestrationObserver::default());
        let spawner = spawner.with_orchestration_observer(observer.clone());
        let child = children[0].clone();
        let long_task = format!("Aufgabe {}", "ü".repeat(900));
        seed_pending_task(&spawner, &child, &long_task);
        let long_reason = "grund ".repeat(300);
        spawner.set_failed(&child, &long_reason);

        spawner
            .release_child(&child)
            .map_err(ctx("failed child releases"))?;

        let events = observer
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let event = events
            .iter()
            .find(|event| event.status == AgentOrchestrationStatus::Failed)
            .ok_or(TestError::Missing("failed event is emitted"))?;
        let task = event
            .task
            .as_deref()
            .ok_or(TestError::Missing("task head is filled"))?;
        assert_eq!(task.chars().count(), ORCHESTRATION_DETAIL_MAX_CHARS);
        assert!(task.starts_with("Aufgabe ü"));
        let detail = event
            .detail
            .as_deref()
            .ok_or(TestError::Missing("failure reason is filled"))?;
        assert!(detail.chars().count() <= ORCHESTRATION_DETAIL_MAX_CHARS);
        assert!(detail.starts_with("grund grund"));
        assert!(
            spawner.child_task_state(&child).is_none(),
            "release forgets the task state"
        );
        Ok(())
    }

    #[test]
    fn child_progress_is_emitted_to_the_attached_sink_and_throttled() -> TestResult {
        let (spawner, child) = spawner_with_admitted_child()?;
        let (events, _session_events) = mpsc::unbounded_channel();
        let (turn_tx, mut turn_rx) = mpsc::unbounded_channel();
        let parent = AgentSession::new(
            AgentRole::Agent {
                name: "parent".to_owned(),
            },
            None,
            ExtensionRegistryBuilder::default().build(),
            events,
        )
        .with_turn_event_sink(turn_tx);
        let turn_id = TurnId::new();
        assert!(spawner.attach_child_progress_sink(&child, turn_id.clone(), parent.live_emitter()));
        assert!(!spawner.attach_child_progress_sink(
            &SessionId::new(),
            turn_id.clone(),
            parent.live_emitter()
        ));

        let record = spawner
            .child_record(&child)
            .ok_or(TestError::Missing("child is tracked"))?;
        let start = std::time::Instant::now();
        assert!(emit_child_progress(&spawner.progress_sinks, &record, start));
        assert!(!emit_child_progress(
            &spawner.progress_sinks,
            &record,
            start + Duration::from_millis(100)
        ));
        assert!(emit_child_progress(
            &spawner.progress_sinks,
            &record,
            start + CHILD_PROGRESS_MIN_INTERVAL
        ));

        let mut received = Vec::new();
        while let Ok(event) = turn_rx.try_recv() {
            received.push(event);
        }
        assert_eq!(received.len(), 2);
        match &received[0] {
            TurnEvent::ChildProgress {
                turn_id: got_turn,
                child: got_child,
                tool_calls,
                tokens,
            } => {
                assert_eq!(got_turn, &turn_id);
                assert_eq!(got_child, &child);
                assert_eq!(*tool_calls, 0);
                assert_eq!(*tokens, 0);
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "unerwartetes Event: {other:?}"
                )));
            }
        }

        spawner.close_child(&child);
        assert!(!emit_child_progress(
            &spawner.progress_sinks,
            &record,
            start + CHILD_PROGRESS_MIN_INTERVAL * 4
        ));
        Ok(())
    }

    // --- Welle 3: Kontextfenster, Ausgabe-Reserve und Turn-Grenzen ---

    /// Registry-Factory, deren Kind-Provider auf ein festes Modell gepinnt
    /// ist (wie `RuntimeChildRegistryFactory` für Explorer/Worker).
    struct PinnedChildRegistry {
        model: &'static str,
    }

    impl ChildRegistryFactory for PinnedChildRegistry {
        fn build_registry(
            &self,
            _role: &str,
            _input: &SpawnInput,
            _suggestions: Option<&AgentSuggestions>,
        ) -> Result<ExtensionRegistry, AgentSpawnError> {
            Ok(ExtensionRegistryBuilder::default().build())
        }

        fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
            let inner: Arc<dyn ModelProvider> = Arc::new(EchoModelProvider::new("pinned"));
            Ok(Arc::new(crate::pinned_model::PinnedModelProvider::new(
                inner,
                None,
                Some(harw_types::ModelId::from(self.model)),
            )))
        }
    }

    fn window_test_resolver() -> Arc<ContextWindowResolver> {
        Arc::new(|model: Option<&str>| match model {
            Some("small-model") => 32_000,
            Some("big-model") => 1_000_000,
            _ => 200_000,
        })
    }

    fn window_test_spawner(
        registry: Arc<dyn ChildRegistryFactory>,
        ceiling: Option<u64>,
    ) -> TestResult<(ManagedAgentSpawner, SessionId, SandboxSpec)> {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let parent = SessionId::new();
        let spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative())
            .with_role(
                "worker",
                AgentRole::Agent {
                    name: "worker".to_owned(),
                },
                harw_agent_dsl::roles::AgentRoleId::Worker,
                registry,
            )
            .with_context_window_resolver(window_test_resolver())
            .with_compaction_ceiling(ceiling)
            .with_external_root_parent(
                parent.clone(),
                external_root_context(
                    sandbox.clone(),
                    harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
                ),
                None,
                SessionActivation::default(),
            )
            .map_err(ctx("trusted external root registers during construction"))?;
        Ok((spawner, parent, sandbox))
    }

    #[test]
    fn compaction_ceiling_defaults_to_the_standard_ceiling() {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative());
        assert_eq!(
            spawner.compaction_ceiling(),
            crate::auto_compact::DEFAULT_ABSOLUTE_CEILING_TOKENS
        );
        let spawner = spawner.with_compaction_ceiling(Some(42_000));
        assert_eq!(spawner.compaction_ceiling(), 42_000);
    }

    #[test]
    fn admit_sizes_the_child_from_its_pinned_model_window() -> TestResult {
        let (spawner, parent, sandbox) = window_test_spawner(
            Arc::new(PinnedChildRegistry {
                model: "small-model",
            }),
            Some(123_456),
        )?;
        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .map_err(ctx("child is admitted"))?;

        let mut manager = spawner
            .manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session = manager
            .get_mut(&child)
            .map_err(ctx("child is manager-owned"))?;

        let reserve = crate::context_budget::output_reserve_tokens(32_000, None, None, false);
        let expected = crate::auto_compact::AutoCompactPolicy::for_context_window(32_000)
            .with_absolute_ceiling(Some(123_456))
            .with_output_reserve(reserve)
            .with_fixed_overhead(CHILD_FIXED_OVERHEAD_TOKENS);
        assert_eq!(
            session.auto_compact().copied(),
            Some(expected),
            "das Fenster des gepinnten Modells (32 000) und der konfigurierte Deckel gelten"
        );
        assert_eq!(session.max_output_tokens(), Some(reserve));

        let limits = session
            .default_turn_limits()
            .ok_or(TestError::Missing("child must carry default turn limits"))?;
        assert_eq!(limits.tool_result_max_bytes, CHILD_TOOL_RESULT_MAX_BYTES);
        assert_eq!(limits.tool_result_max_bytes, 65_536);
        assert_eq!(limits.max_model_rounds, u32::MAX);

        // Ein Modellwechsel darf das Fenster eines gepinnten Kindes nicht
        // verschieben — der Pin überschreibt jedes Request-Modell.
        session.set_active_model(Some(harw_types::ModelId::from("big-model")));
        assert_eq!(
            session
                .auto_compact()
                .map(crate::auto_compact::AutoCompactPolicy::context_window_tokens),
            Some(32_000)
        );
        Ok(())
    }

    #[test]
    fn admit_without_a_pin_uses_the_default_ceiling_and_rescales_on_model_change() -> TestResult {
        let (spawner, parent, sandbox) = window_test_spawner(Arc::new(EmptyChildRegistry), None)?;
        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .map_err(ctx("child is admitted"))?;

        let mut manager = spawner
            .manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session = manager
            .get_mut(&child)
            .map_err(ctx("child is manager-owned"))?;

        let reserve = crate::context_budget::output_reserve_tokens(200_000, None, None, false);
        let expected = crate::auto_compact::AutoCompactPolicy::for_context_window(200_000)
            .with_absolute_ceiling(Some(crate::auto_compact::DEFAULT_ABSOLUTE_CEILING_TOKENS))
            .with_output_reserve(reserve)
            .with_fixed_overhead(CHILD_FIXED_OVERHEAD_TOKENS);
        assert_eq!(session.auto_compact().copied(), Some(expected));
        assert_eq!(session.max_output_tokens(), Some(reserve));

        // Die Kind-Session trägt den Resolver: ein Modellwechsel skaliert.
        session.set_active_model(Some(harw_types::ModelId::from("big-model")));
        assert_eq!(
            session
                .auto_compact()
                .map(crate::auto_compact::AutoCompactPolicy::context_window_tokens),
            Some(1_000_000)
        );
        Ok(())
    }
}
