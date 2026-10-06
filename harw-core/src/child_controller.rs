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
use harw_agent_dsl::executable::{
    BudgetSpec, ContextProgram, ExecutableAgentIr, ReferencedSnapshotId, SectionDetail, SnapshotId,
};
use harw_authority::{AuthoritySnapshot, SandboxSpec};
use harw_catalog::{AgentSuggestions, SpawnCapabilitySnapshot, SuggestionKind};
use harw_context::{ContextBudgetSpec, ContextCeiling, SectionName, TrustClass};
use harw_extension_api::{
    AgentSpawnError, AgentSpawner, ExtensionRegistry, SpawnFuture, SpawnInput,
};
use harw_observe::TraceContext;
use harw_protocol::items::{ContentPart, TurnItem};
use harw_protocol::{AgentOrchestrationEvent, AgentOrchestrationStatus, TurnEvent};
use harw_session_store::{ApprovalStore, ChildLeaseRecord, ChildLeaseStore};
use harw_types::{
    AgentRole, ApprovalActor, ReasoningEffort, SessionId, TokenUsage, ToolCallId, TurnId,
};
use jiff::{SignedDuration, Timestamp};
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;
use uuid::Uuid;

/// Meldungstext für ein Geschwisterkind, das von [`JoinSemantics::AnyTerminal`]
/// abgebrochen wurde, weil ein anderes Kind zuerst fertig war.
const CANCELLED_BY_SIBLING: &str = "cancelled: sibling completed first";

/// Runde 7, Teil A7: ob der Start eines Hintergrund-Kindes einen bereits
/// gesetzten Abbruch überlebt.
///
/// # Beschreibung
/// Nur ein **geerbter** Abbruch ([`CancelReason::Parent`]) des startenden
/// Turns, dessen Elternteil nicht herunterfährt bzw. seine Lease verlor.
/// Ein Abbruch des Kindes selbst bleibt immer bestehen.
///
/// # Argumente
/// - `child_reason` (`Option<CancelReason>`): Grund am Token des Kindes.
/// - `parent_reason` (`Option<CancelReason>`): Grund am Token des Elternteils.
///
/// # Returns
/// `true`, wenn der Start mit frischem Token abgeschlossen wird.
fn detached_start_survives(
    child_reason: Option<CancelReason>,
    parent_reason: Option<CancelReason>,
) -> bool {
    matches!(child_reason, Some(CancelReason::Parent))
        && !matches!(
            parent_reason,
            Some(CancelReason::Shutdown | CancelReason::LeaseLost)
        )
}

/// Das Effort-Level, auf das ein Kind geklammert wird, wenn der Elternteil
/// selbst keines gesetzt hat.
///
/// # Beschreibung
/// F-017/E3b. Ein fehlendes Eltern-Level (`None`) heißt **nicht** „unbegrenzt":
/// ohne diesen Deckel hob ein `None` beim Elternteil auch den `effort_cap` der
/// Agent-IR auf, und das Kind lief mit dem Provider-Default — ein früherer
/// Befund. Statt eines Provider-Defaults
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
/// **Provider > Modell > Agent > Rolle** auf.
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
///
/// Runde 5 (Bugreport „Kind-Antwort in der Mitte gekürzt“): früher 8 KiB
/// (≈ 2k Token, ≈ 1 % eines 200k-Fensters) — ein gewöhnlicher
/// Orchestrator-Bericht von ~18 KB verlor damit über die Hälfte. 32 KiB
/// (≈ 8k Token, ≈ 4 % eines 200k-Fensters) fassen solche Berichte ganz und
/// bleiben deutlich unter der Werkzeugergebnis-Grenze der Wurzel
/// (`TOOL_RESULT_MAX_BYTES`, 64 KiB, `harw-runtime/src/assembly.rs`) und der
/// Kind-Turns ([`CHILD_TOOL_RESULT_MAX_BYTES`]), damit dort nicht ein
/// zweites Mal gekürzt wird.
pub const CHILD_RETURN_MAX_BYTES: usize = 32 * 1024;

/// Kürzt einen Text auf höchstens `max_bytes`, ohne einen UTF-8-Zeichen zu zerschneiden.
///
/// # Beschreibung
/// Ist `text.len() <= max_bytes`, wird `text` unverändert zurückgegeben.
/// Andernfalls werden ein Kopf und ein Ende zu **je der Hälfte** des Budgets
/// behalten, getrennt durch eine Markierung `\n[… {n} Bytes der
/// Kind-Antwort gekürzt …]\n`, wobei `{n}` die Anzahl der weggelassenen
/// Bytes ist. Die Markierung sagt zusätzlich, dass Anfang und Schluss
/// vollständig sind und nur der Mittelteil fehlt (Wortlaut:
/// `[… {n} Bytes der Kind-Antwort gekürzt; Anfang und Schluss vollständig …]`).
///
/// Das Ende ist bewusst genauso groß wie der Kopf (früher nur ~1/4): bei
/// einem Bericht stehen Fazit und Empfehlungen am Schluss und dürfen nicht
/// verloren gehen. Beide Schnittstellen rücken, wenn möglich, auf eine
/// Zeilengrenze (höchstens `CAP_LINE_SNAP_BYTES` weit, nie über das
/// Budget hinaus), damit keine halbe Zeile stehen bleibt; ansonsten auf die
/// nächstliegende gültige UTF-8-Zeichengrenze, damit niemals ein
/// Mehrbyte-Zeichen mittendrin geteilt wird.
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
/// assert!(capped.len() <= 100 + 128);
/// assert!(capped.contains("gekürzt"));
/// ```
#[must_use]
pub fn cap_child_return_text(text: &str, max_bytes: usize) -> String {
    cap_child_return_text_inner(text, max_bytes, None)
}

/// Name des rein lesenden Werkzeugs, das den ungekürzten Antworttext eines
/// eigenen, abgeschlossenen Kind-Laufs liefert (Runde 5, Teil H;
/// `harw-core-bridge::agent_result`). Die Kürzungsmarke von
/// [`cap_child_return_text_for_child`] nennt es.
pub const AGENT_RESULT_TOOL: &str = "agent.result";

/// Wie [`cap_child_return_text`], aber die Kürzungsmarke nennt zusätzlich
/// das Werkzeug [`AGENT_RESULT_TOOL`] mit der `child_id` des Kindes
/// (Runde 5, Teil H).
///
/// # Beschreibung
/// Die Markierung lautet dann
/// `[… {n} Bytes der Kind-Antwort gekürzt; Anfang und Schluss vollständig;
/// ungekürzt über agent.result {"child_id":"<id>"} …]`. Der Elternteil kann
/// den vollständigen Text damit gezielt (auch seitenweise über
/// `offset`/`max_bytes`) nachladen, statt den Auftrag zu wiederholen.
///
/// # Argumente
/// - `text` (`&str`): der ungekürzte Text.
/// - `max_bytes` (`usize`): das Byte-Budget für Kopf + Ende.
/// - `child` (`&SessionId`): das Kind, dessen Antwort gekürzt wird.
///
/// # Rückgabe
/// Wie [`cap_child_return_text`]; ein Text innerhalb des Budgets bleibt
/// unverändert (ohne Markierung).
///
/// # Beispiele
/// ```rust
/// use harw_core::child_controller::cap_child_return_text_for_child;
/// use harw_types::SessionId;
///
/// let child = SessionId::new();
/// assert_eq!(cap_child_return_text_for_child("kurz", 100, &child), "kurz");
/// let capped = cap_child_return_text_for_child(&"a".repeat(200), 100, &child);
/// assert!(capped.contains("agent.result"));
/// assert!(capped.contains(child.as_str()));
/// ```
#[must_use]
pub fn cap_child_return_text_for_child(text: &str, max_bytes: usize, child: &SessionId) -> String {
    cap_child_return_text_inner(text, max_bytes, Some(child))
}

/// Gemeinsamer Kern von [`cap_child_return_text`] und
/// [`cap_child_return_text_for_child`].
fn cap_child_return_text_inner(text: &str, max_bytes: usize, child: Option<&SessionId>) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }

    let head_budget = max_bytes / 2;
    let tail_budget = max_bytes - head_budget;

    let head_end = floor_char_boundary(text, head_budget);
    // Kopf auf das Ende der letzten vollständigen Zeile zurückziehen (der
    // Zeilenumbruch bleibt im Kopf), sofern sie nahe genug liegt.
    let head_end = text[..head_end]
        .rfind('\n')
        .map(|newline| newline + 1)
        .filter(|&snapped| head_end - snapped <= CAP_LINE_SNAP_BYTES)
        .unwrap_or(head_end);
    let tail_start_min = text.len().saturating_sub(tail_budget);
    let tail_start = ceil_char_boundary(text, tail_start_min);
    // Ende auf den Anfang der nächsten vollständigen Zeile vorziehen.
    let tail_start = text[tail_start..]
        .find('\n')
        .map(|newline| tail_start + newline + 1)
        .filter(|&snapped| snapped - tail_start <= CAP_LINE_SNAP_BYTES && snapped < text.len())
        .unwrap_or(tail_start);
    // Kopf und Ende dürfen sich nicht überlappen; bei sehr kleinen Budgets
    // (oder sehr großen UTF-8-Zeichen an der Grenze) wird das Ende notfalls
    // hinter das Kopfende gezogen.
    let tail_start = tail_start.max(head_end);

    let omitted = text.len().saturating_sub(head_end) - (text.len() - tail_start);
    // Runde 5, Teil H: mit bekannter Kind-ID nennt die Markierung den Weg
    // zum ungekürzten Text.
    let marker = match child {
        Some(child) => format!(
            "\n[… {omitted} Bytes der Kind-Antwort gekürzt; Anfang und Schluss vollständig; \
             ungekürzt über {AGENT_RESULT_TOOL} {{\"child_id\":\"{child}\"}} …]\n"
        ),
        None => format!(
            "\n[… {omitted} Bytes der Kind-Antwort gekürzt; Anfang und Schluss vollständig …]\n"
        ),
    };

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

/// Wie weit [`cap_child_return_text`] eine Schnittstelle höchstens verschiebt,
/// um auf einer Zeilengrenze zu landen (Bytes). Die Verschiebung verkleinert
/// Kopf bzw. Ende nur, überschreitet das Budget also nie.
const CAP_LINE_SNAP_BYTES: usize = 512;

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

/// #22 Welle 1B: die Nummer des Versuchs, den eine Fortsetzung nach `used`
/// bereits gebundenen Fortsetzungen wäre (der erste Lauf ist Versuch 1).
fn next_attempt(used: u32) -> u32 {
    used.saturating_add(2)
}

/// Rechnet ein Token-Budget des Auftrags mit der kalibrierten Rate in Bytes
/// um (abgerundet, mindestens 1 KiB).
fn task_tokens_to_bytes(
    tokens: u64,
    calibration: &crate::context_budget::TokenCalibration,
) -> usize {
    let bytes = (tokens as f64 * calibration.bytes_per_token()).floor();
    let bytes = if bytes.is_finite() && bytes > 0.0 {
        // `as` sättigt bei übergroßen Werten.
        bytes as u64
    } else {
        0
    };
    usize::try_from(bytes).unwrap_or(usize::MAX).max(1024)
}

/// Kürzt einen Auftrag in der Mitte auf höchstens `max_bytes` (Kopf 60 %,
/// Ende 40 %), mit sichtbarer Markierung der Auslassung (Teil C).
/// Runde 9, E3: kleinste Wartezeit, nach der das Zeitbudget eines Kindes
/// erneut gegen seine Fragen-Wartezeit geprüft wird (verhindert ein
/// Leerlaufen der Schleife in `enforce_child_budget`).
const QUESTION_BUDGET_RECHECK: Duration = Duration::from_millis(10);

/// Runde 9, E3: Deckel der letzten Antwort in der Übergabe eines regulär
/// beendeten Kindes (Fortsetzungs-Buch).
const COMPLETED_HANDOFF_ANSWER_MAX_BYTES: usize = 8 * 1024;

fn cap_task_text(text: &str, max_bytes: usize) -> String {
    const NOTICE: &str = "[Hinweis: Der Auftrag war zu lang für das Kontextfenster und wurde \
                          in der Mitte gekürzt.]\n";
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let cut = crate::compaction::elide_middle(text, max_bytes.saturating_sub(NOTICE.len()), 60);
    format!("{NOTICE}{cut}")
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
    /// Warnung aus der Admission (z. B. unbekanntes Modell mit
    /// Rückfallfenster, gekürzter Auftrag) für
    /// `AgentOrchestrationEvent::detail` bei `Admitted`/`Running` (Teil C).
    admission_warning: Option<String>,
    /// Höchstlänge (Bytes) des Auftrags-Texts für jeden Lauf dieses Kindes;
    /// `None` = unbegrenzt (Teil C).
    task_max_bytes: Option<usize>,
    /// Runde 5, Teil J: gesetzt, wenn dieses Kind die Fortsetzung eines
    /// budget-beendeten Vorgängers ist ([`ManagedAgentSpawner::bind_continuation`]).
    continuation: Option<crate::child_handoff::ContinuationLink>,
    /// Wave 3C: der zuletzt vom angebundenen [`crate::child_backend::ChildBackend`]
    /// gemeldete Fortsetzungs-Token ([`crate::child_backend::ChildRunOutcome::continuation`]),
    /// für [`crate::child_backend::ChildRunSpec::continue_from`] eines erneuten
    /// Laufs desselben Kindes. `None` ohne Backend oder ohne meldbare Fortsetzung.
    backend_resume: Option<String>,
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
    /// Der Provider, den dieses Kind anspricht (Anzeige `<provider>/<modell>`):
    /// gepinnter Provider, sonst `active_provider` der Kind-Session, sonst
    /// Hauptprovider der Factory, sonst der des Elternteils. `None`, wenn
    /// unbekannt.
    pub provider: Option<String>,
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

/// Persistierbarer Budget-Snapshot eines bereits admittierten Kindes.
///
/// Dieser Typ ist reine Wiederanlauf-Metadaten. Er kann weder ein Budget
/// erweitern noch eine Session starten; ein späterer Rehydrator muss ihn
/// gegen die frisch geladene Agent-IR und die aktuelle Policy prüfen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildRecoveryBudget {
    pub max_tokens: Option<u64>,
    pub max_tool_calls: Option<u32>,
    pub max_wall_time_ms: Option<u64>,
    pub reasoning_effort: Option<ReasoningEffort>,
}

impl From<AgentBudget> for ChildRecoveryBudget {
    fn from(value: AgentBudget) -> Self {
        Self {
            max_tokens: value.max_tokens,
            max_tool_calls: value.max_tool_calls,
            max_wall_time_ms: value.max_wall_time_ms,
            reasoning_effort: value.reasoning_effort,
        }
    }
}

impl From<ChildRecoveryBudget> for AgentBudget {
    fn from(value: ChildRecoveryBudget) -> Self {
        Self {
            max_tokens: value.max_tokens,
            max_tool_calls: value.max_tool_calls,
            max_wall_time_ms: value.max_wall_time_ms,
            reasoning_effort: value.reasoning_effort,
        }
    }
}

/// Nicht-autorisierender Verweis auf eine aktivierte Capability.
///
/// Nur Name, Art und der beim Spawn berechnete Definition-Digest werden
/// persistiert. Credentials, laufende MCP-Verbindungen oder executable grants
/// gehören ausdrücklich nicht in den Recovery-Vertrag.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildRecoveryCapability {
    pub kind: String,
    pub name: String,
    pub definition_sha256: String,
}

/// Rekonstruktionsinput für einen durablen Agent-Job.
///
/// Der Snapshot ist kein Grant. authority ist absichtlich nur ein
/// AuthoritySnapshot; eine wiederhergestellte Session muss daraus über die
/// aktuelle vertrauenswürdige Policy einen neuen Grant ausstellen lassen.
/// Ebenso ist executable_snapshot_id nur der bei der Admission berechnete
/// Digest: Recovery darf nur fortfahren, wenn die aktuell aufgelöste IR ihn
/// bestätigt. Capability-Digests, Kontextdecke und Routing-Metadaten dürfen
/// beim Wiederanlauf nur bestätigt oder verengt, nie still erweitert werden.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildRecoveryView {
    pub child: SessionId,
    pub parent: SessionId,
    pub handoff_call_id: ToolCallId,
    pub role: String,
    pub depth: u32,
    pub depth_ceiling: u32,
    pub budget: ChildRecoveryBudget,
    pub task_complexity: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub authority: AuthoritySnapshot,
    pub approval_actor: Option<ApprovalActor>,
    pub organizational_role: harw_agent_dsl::roles::AgentRoleId,
    pub allowed_child_orchestrators: Vec<String>,
    pub context_ceiling: Option<ContextCeiling>,
    pub mode: String,
    /// Untrusted wire/reference form. Recovery must confirm this against the
    /// freshly resolved current IR before the stored transcript may run.
    pub executable_snapshot_id: Option<ReferencedSnapshotId>,
    pub capability_agent: Option<String>,
    pub activated_capabilities: Vec<ChildRecoveryCapability>,
    pub trace: Option<TraceContext>,
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

/// Wave 3C: maps this crate's own [`AgentBudget`] (dimension-wise remaining
/// budget, [`ManagedAgentSpawner::remaining_budget`]) onto the DSL's
/// [`harw_agent_dsl::ir_v2::Budget`] — the shape
/// [`crate::child_backend::ChildRunSpec::budget`] carries. `reasoning_effort`
/// has no counterpart in that DSL type here and is dropped;
/// `max_wall_time_ms` rounds up to whole seconds.
fn agent_budget_to_dsl_budget(budget: AgentBudget) -> harw_agent_dsl::ir_v2::Budget {
    harw_agent_dsl::ir_v2::Budget {
        max_tokens: budget.max_tokens,
        max_tool_calls: budget.max_tool_calls,
        max_wall_secs: budget.max_wall_time_ms.map(|ms| ms.div_ceil(1_000)),
        effort_cap: None,
    }
}

/// Wave 3C: [`crate::child_backend::ChildIo`] for a backend-driven child —
/// relays activity into the same progress/journal machinery a locally run
/// child uses ([`ManagedAgentSpawner::progress_observer`]), and questions /
/// approval requests into the existing parent relays
/// ([`ManagedAgentSpawner::ask_parent`], [`crate::child_approval::ChildApprovalRelay`]).
/// This keeps a compiled parent's TUI panel and `agent.status`/`agent.result`
/// working the same regardless of where the child actually runs.
struct ControllerChildIo<'a> {
    spawner: &'a ManagedAgentSpawner,
    child: SessionId,
    role: String,
}

impl crate::child_backend::ChildIo for ControllerChildIo<'_> {
    fn on_event(&self, event_json: serde_json::Value) {
        let observer = self.spawner.progress_observer();
        observer.on_progress(&self.child);
        if let Some(text) = event_json.get("assistant_text").and_then(|v| v.as_str()) {
            observer.on_assistant_text(&self.child, text);
        }
    }

    fn on_question<'a>(
        &'a self,
        id: String,
        text: String,
    ) -> crate::child_backend::ChildAnswerFuture<'a> {
        let spawner = self.spawner;
        let child = self.child.clone();
        Box::pin(async move {
            // `id` ist die Korrelations-ID des Backends für sein eigenes
            // Protokoll; der bestehende Frage-Relais führt seinen eigenen
            // Zähler (`ManagedAgentSpawner::ask_parent`).
            let _ = id;
            spawner
                .ask_parent(&child, &text, crate::child_comms::PARENT_QUESTION_TIMEOUT)
                .await
                .map(|(answer, _delivered)| answer)
                .unwrap_or_else(|_| crate::child_comms::NO_ANSWER_REPLY.to_owned())
        })
    }

    fn on_approval_request<'a>(
        &'a self,
        id: String,
        tool: String,
        args_summary: String,
    ) -> crate::child_backend::ChildAnswerFuture<'a> {
        let spawner = self.spawner;
        let child = self.child.clone();
        let role = self.role.clone();
        Box::pin(async move {
            let call = harw_tools::ToolCall {
                id: ToolCallId::from_str(id),
                name: harw_tools::ToolName::new(tool),
                arguments: serde_json::json!({ "summary": args_summary }),
            };
            let request = crate::child_approval::ChildApprovalRequest {
                child: child.clone(),
                role,
                tree_path: spawner.child_tree_path(&child),
                call,
                timeout: spawner.child_approval_relay().timeout(),
            };
            match spawner.child_approval_relay().resolve(request).await {
                crate::turn_loop::ApprovalResolution::Approve => "approve".to_owned(),
                crate::turn_loop::ApprovalResolution::Reject { reason } => {
                    format!("deny: {reason}")
                }
            }
        })
    }
}

/// Laufende Zähler eines Kindes, fortgeschrieben vom Progress-Observer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChildLiveStats {
    pub usage: TokenUsage,
    pub tool_calls: u32,
}

impl ChildRecord {
    /// Provider und Modell dieses Kindes als `<provider>/<modell>` (ohne
    /// Provider nur das Modell); `None`, wenn beides unbekannt ist.
    #[must_use]
    pub fn model_route(&self) -> Option<String> {
        match (self.provider.as_deref(), self.model.as_deref()) {
            (Some(provider), Some(model)) => Some(format!("{provider}/{model}")),
            (None, Some(model)) => Some(model.to_owned()),
            (Some(provider), None) => Some(provider.to_owned()),
            (None, None) => None,
        }
    }

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
    /// `true`, wenn das Token-Budget den Lauf beendet hat (Teil C). Dann ist
    /// `outcome` [`TurnOutcome::Completed`] und `full_text` die **letzte**
    /// Assistant-Antwort des Kindes als Teilergebnis (oder `None`, wenn es
    /// noch keine gab) — kein Fehler, das Ergebnis geht nicht verloren. Seit
    /// Runde 5, Teil J ist `full_text` stattdessen die Übergabe-Verdichtung,
    /// wenn [`Self::budget_handoff`] `Compacted` ist.
    pub budget_exhausted: bool,
    /// Runde 5, Teil J: wie das Ergebnis eines budget-beendeten Laufs
    /// entstand. `Some(Compacted)`: `full_text` ist die markierte
    /// Übergabe-Verdichtung (die rohe letzte Antwort liegt im Archiv für
    /// `agent.result`); `Some(LastAnswer)`: `full_text` ist die letzte
    /// Assistant-Antwort (Modus `LastAnswer` oder Rückfall nach
    /// gescheiterter Verdichtung). `None`, wenn das Budget nicht griff.
    pub budget_handoff: Option<crate::child_handoff::BudgetHandoff>,
}

/// Hinweis, der ein budget-beendetes Teilergebnis für den Elternteil markiert
/// (Runde 7, Teil A1).
pub const TRANSFER_BUDGET_NOTE: &str =
    "Budget des Kind-Agenten erschöpft – Teilergebnis bzw. Übergabe-Zusammenfassung";

impl ChildRunResult {
    /// Text für den Elternteil eines budget-beendeten Laufs.
    ///
    /// # Beschreibung
    /// Runde 7, Teil A1: Transfers geben das Ergebnis eines vom Token-Budget
    /// beendeten Kindes als Freitext zurück. Eine verdichtete Übergabe trägt
    /// ihre Markierung selbst; eine rohe letzte Antwort bekommt den Präfix
    /// `[budget_exhausted: true]`. Der Text ist wie im regulären Rückweg auf
    /// [`CHILD_RETURN_MAX_BYTES`] gekappt (die Kürzungsmarke nennt
    /// `agent.result`).
    ///
    /// # Returns
    /// `Some(text)`, wenn [`Self::budget_exhausted`] gesetzt ist; sonst
    /// `None`.
    #[must_use]
    pub fn budget_exhausted_parent_text(&self) -> Option<String> {
        if !self.budget_exhausted {
            return None;
        }
        let partial = self.full_text.as_deref().unwrap_or_default();
        let partial = cap_child_return_text_for_child(partial, CHILD_RETURN_MAX_BYTES, &self.child);
        if self.budget_handoff == Some(crate::child_handoff::BudgetHandoff::Compacted) {
            return Some(partial);
        }
        Some(if partial.trim().is_empty() {
            format!("[budget_exhausted: true] {TRANSFER_BUDGET_NOTE}; keine Antwort vorhanden.")
        } else {
            format!("[budget_exhausted: true] {TRANSFER_BUDGET_NOTE}.\n\n{partial}")
        })
    }
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

/// Meldet, ob der Resolver das Kontextfenster eines Modells tatsächlich
/// kennt (`false` = konservativer Rückfallwert). Siehe
/// [`ManagedAgentSpawner::with_model_known_probe`].
pub type ModelKnownProbe = dyn Fn(Option<&str>) -> bool + Send + Sync;

/// Kontextfenster eines Kindes ohne Resolver (Addendum D).
pub const DEFAULT_CHILD_CONTEXT_WINDOW: u64 = 200_000;

/// Runde 7, Teil L9: unter diesem Kontextfenster (Tokens) bekommt ein Kind
/// kompakte Werkzeugschemas.
pub const COMPACT_TOOL_SCHEMA_WINDOW_TOKENS: u64 = 64_000;

/// Höchster Anteil (Prozent) des Kind-Fensters, den der Auftrag
/// (`task`/`context`) belegen darf (Teil C). Ein längerer Auftrag wird in
/// der Mitte gekürzt, mit sichtbarer Markierung.
pub const CHILD_TASK_MAX_WINDOW_PERCENT: u64 = 25;

/// Höchster Anteil (Prozent) des Kind-Fensters für die Grundlast aus
/// System-Prompt, Werkzeugschemata und Auftrag (Teil C). Darüber wird der
/// Auftrag weiter gekürzt; reicht das nicht, scheitert die Admission mit
/// [`ChildContextOverload`].
pub const CHILD_BASE_LOAD_MAX_WINDOW_PERCENT: u64 = 50;

/// Mindestgröße (Tokens), die für den Auftrag innerhalb der Grundlast übrig
/// bleiben muss; sonst gilt die Grundlast als zu groß (Teil C).
pub const CHILD_MIN_TASK_TOKENS: u64 = 512;

/// Die Grundlast eines Kindes passt nicht in sein Kontextfenster (Teil C).
///
/// # Beschreibung
/// Typisierter Admission-Befund mit allen Zahlen: System-Prompt
/// (geschätzt als [`CHILD_FIXED_OVERHEAD_TOKENS`]) plus Werkzeugschemata
/// lassen innerhalb von [`CHILD_BASE_LOAD_MAX_WINDOW_PERCENT`] % des Fensters
/// keine [`CHILD_MIN_TASK_TOKENS`] für den Auftrag übrig. Über die
/// Spawn-Grenze reist er als [`AgentSpawnError`] mit
/// [`std::fmt::Display`]-Text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildContextOverload {
    /// Die Rolle des abgewiesenen Kindes.
    pub role: String,
    /// Das Modell, dessen Fenster gilt (falls bekannt).
    pub model: Option<String>,
    /// Kontextfenster des Kindes in Tokens.
    pub window_tokens: u64,
    /// Geschätzte Tokens aus System-Prompt und Werkzeugschemata.
    pub base_tokens: u64,
    /// Obergrenze der Grundlast in Tokens.
    pub limit_tokens: u64,
}

impl std::fmt::Display for ChildContextOverload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "child_context_overload: role '{}' (model {}) needs ~{} tokens for system prompt \
             and tools, more than the {}-token base-load limit ({} % of a {}-token window) \
             leaves for a task of at least {} tokens",
            self.role,
            self.model.as_deref().unwrap_or("<default>"),
            self.base_tokens,
            self.limit_tokens,
            CHILD_BASE_LOAD_MAX_WINDOW_PERCENT,
            self.window_tokens,
            CHILD_MIN_TASK_TOKENS
        )
    }
}

impl std::error::Error for ChildContextOverload {}

/// Fehler eines budgetierten Kind-Laufs ([`ManagedAgentSpawner::run_child_with_budget`]).
///
/// # Beschreibung
/// Trennt einen echten Spawn-/Ausführungsfehler von einer erschöpften
/// Budget-Dimension (Teil C): letztere ist ein Laufzeitbefund des Kindes und
/// trägt deshalb nicht mehr die irreführende Meldung „agent spawn failed".
/// Das Token-Budget liefert keinen Fehler, sondern ein Teilergebnis
/// ([`ChildRunResult::budget_exhausted`]).
#[derive(Debug)]
pub enum ChildRunError {
    /// Spawn-, Lease-, Registry- oder Turn-Fehler.
    Spawn(AgentSpawnError),
    /// Eine Budget-Dimension (Wanduhr, Werkzeugaufrufe) ist erschöpft.
    BudgetExhausted(harw_extension_api::ChildBudgetExhausted),
}

impl ChildRunError {
    /// Die Fehlermeldung ohne Typ-Präfix des Spawn-Fehlers (für Tests und
    /// maschinenlesbare Auswertung).
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Spawn(error) => error.message.clone(),
            Self::BudgetExhausted(exhausted) => exhausted.to_string(),
        }
    }
}

impl std::fmt::Display for ChildRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => error.fmt(f),
            Self::BudgetExhausted(exhausted) => exhausted.fmt(f),
        }
    }
}

impl std::error::Error for ChildRunError {}

impl From<AgentSpawnError> for ChildRunError {
    fn from(error: AgentSpawnError) -> Self {
        Self::Spawn(error)
    }
}

impl From<harw_extension_api::ChildBudgetExhausted> for ChildRunError {
    fn from(exhausted: harw_extension_api::ChildBudgetExhausted) -> Self {
        Self::BudgetExhausted(exhausted)
    }
}

impl From<ChildRunError> for AgentSpawnError {
    /// Für Aufrufer, die nur [`AgentSpawnError`] transportieren können: die
    /// Meldung bleibt die des Befunds.
    fn from(error: ChildRunError) -> Self {
        match error {
            ChildRunError::Spawn(error) => error,
            ChildRunError::BudgetExhausted(exhausted) => Self {
                message: exhausted.to_string(),
            },
        }
    }
}

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

/// Beobachter, die eine [`ChildRegistryFactory`] an die Sitzung eines
/// admittierten Kindes hängt (Runde 5, Teil C: Diary der Kind-Agenten).
///
/// # Felder
/// - `compaction`: landet über
///   [`AgentSession::with_compaction_observer`] im Verdichtungs-Slot.
/// - `tool_outcome`: landet über
///   [`AgentSession::with_tool_outcome_observer`] im Werkzeug-/Runden-Slot.
///
/// `None` lässt den jeweiligen Slot der Kind-Sitzung unverändert.
#[derive(Clone, Default)]
pub struct ChildSessionObservers {
    pub compaction: Option<Arc<dyn crate::compaction::CompactionObserver>>,
    pub tool_outcome: Option<Arc<dyn crate::capture::ToolOutcomeObserver>>,
}

impl std::fmt::Debug for ChildSessionObservers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChildSessionObservers")
            .field("compaction", &self.compaction.is_some())
            .field("tool_outcome", &self.tool_outcome.is_some())
            .finish()
    }
}

/// Supplies a fresh, role-specific extension registry for an admitted child.
/// It is intentionally fallible: a role must not start with a partial plugin,
/// skill, or MCP activation.
pub trait ChildRegistryFactory: Send + Sync {
    /// Beobachter für die Sitzung eines gerade admittierten Kindes (Runde 5,
    /// Teil C). Der Controller ruft das genau einmal je Admission, nachdem
    /// die Kind-Sitzung existiert, und hängt die gelieferten Beobachter an.
    /// Der Aufruf geschieht unter dem Lock des `SessionManager`; die
    /// Implementierung darf den Controller deshalb nicht wieder betreten.
    ///
    /// # Arguments
    /// - `role` (`&str`): der exakte registrierte Rollenname.
    /// - `child` (`&SessionId`): die Sitzung des Kindes.
    ///
    /// # Returns
    /// Der Default liefert keine Beobachter — bestehende Factories bleiben
    /// unverändert.
    fn child_session_observers(&self, role: &str, child: &SessionId) -> ChildSessionObservers {
        let _ = (role, child);
        ChildSessionObservers::default()
    }

    /// Meldet die Freigabe eines Kindes (Runde 5, Teil C): Abschluss,
    /// Abbruch oder Fehler — einmal je Kind-Lauf, nachdem der Controller
    /// seine Sperren freigegeben hat. Der Default tut nichts.
    ///
    /// # Arguments
    /// - `role` (`&str`): der exakte registrierte Rollenname.
    /// - `child` (`&SessionId`): die freigegebene Sitzung.
    fn child_session_released(&self, role: &str, child: &SessionId) {
        let _ = (role, child);
    }

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

    /// Builds from an already frozen capability snapshot plus the trusted
    /// immediate-parent grant.
    ///
    /// This is the single-snapshot seam: callers that already resolved a
    /// capability contract (notably recovery) pass that exact immutable value
    /// to registry construction instead of asking the factory to resolve live
    /// catalog state a second time.
    fn build_registry_from_capability_snapshot_for_parent(
        &self,
        role: &str,
        input: &SpawnInput,
        snapshot: Option<&SpawnCapabilitySnapshot>,
        parent: &ParentGrant,
    ) -> Result<ExtensionRegistry, AgentSpawnError> {
        let _ = parent;
        self.build_registry_with_capabilities(role, input, snapshot)
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
        let snapshot = self.capability_snapshot(role, input)?;
        self.build_registry_from_capability_snapshot_for_parent(
            role,
            input,
            snapshot.as_ref(),
            parent,
        )
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

    /// Modell-ID, die der **ungepinnte** Kind-Provider für `role`/`complexity`
    /// ohne eigenes `active_model` anspricht — das Hauptmodell dieser Factory
    /// (Teil C).
    ///
    /// # Description
    /// Grundlage für das Kontextfenster eines Kindes ohne Pin
    /// ([`Self::pinned_model_for_task`] = `None`). Ohne diese Angabe fiel das
    /// Fenster auf das Vorgabemodell des Resolvers zurück — für Rollen, deren
    /// Factory ein anderes Hauptmodell fährt (z. B. die UIA-Worker-Familie),
    /// oder ohne gesetztes Vorgabemodell auf das konservative 32k-Fenster.
    /// Der Default liefert `None`; dann folgt der Controller dem Modell des
    /// Elternteils.
    ///
    /// # Arguments
    /// - `role` (`&str`): exakter registrierter Rollenname.
    /// - `complexity` (`Option<TaskComplexity>`): wie bei
    ///   [`Self::model_for_task`].
    ///
    /// # Returns
    /// Die Modell-ID oder `None`, wenn die Factory keine Aussage trifft.
    fn main_model_for_task(
        &self,
        role: &str,
        complexity: Option<TaskComplexity>,
    ) -> Option<String> {
        let _ = (role, complexity);
        None
    }

    /// Provider-ID, die der Kind-Provider für `role`/`complexity` fest
    /// anspricht — Gegenstück zu [`Self::pinned_model_for_task`] für die
    /// Anzeige `<provider>/<modell>`.
    ///
    /// # Description
    /// Der Default liefert `None`, statt [`Self::model_for_task`] ein
    /// weiteres Mal aufzurufen (Factories mit Nebenwirkungen in `model_for`
    /// bleiben unberührt); Factories mit Provider-Routing überschreiben ihn.
    ///
    /// # Returns
    /// Die gepinnte Provider-ID oder `None` (kein Pin bekannt).
    fn pinned_provider_for_task(
        &self,
        role: &str,
        complexity: Option<TaskComplexity>,
    ) -> Option<String> {
        let _ = (role, complexity);
        None
    }

    /// Provider-ID, die ein **ungepinntes** Kind dieser Factory anspricht
    /// (Gegenstück zu [`Self::main_model_for_task`]). Der Default liefert
    /// `None`; dann gilt der Provider des Elternteils.
    fn main_provider_for_task(
        &self,
        role: &str,
        complexity: Option<TaskComplexity>,
    ) -> Option<String> {
        let _ = (role, complexity);
        None
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

/// Runde 9, E7: Eltern-Sicht einer admittierten Kind-Sitzung, deren Session
/// für einen laufenden Turn aus dem [`SessionManager`] entnommen ist.
///
/// # Beschreibung
/// [`ManagedAgentSpawner::run_child`] nimmt die Session eines Kindes für die
/// Dauer seines Turns aus dem Manager. Startet dieses Kind **während** seines
/// Turns selbst Kinder (ein Werkzeug wie `matrix.run`, das Sitz-Agenten
/// startet, oder ein Handoff), fand die Admission den Elternteil nicht mehr
/// und lehnte jedes Enkelkind sofort mit „unknown child parent“ ab — der
/// Matrix-Game-Master als Kind der UIA ließ so jeden Sitz passen.
///
/// Der Schnappschuss entsteht unter dem Manager-Lock im selben Schritt wie
/// die Entnahme und verschwindet in `return_session` wieder; er enthält genau
/// die Felder, die die Admission sonst aus der Eltern-Session liest. Er ist
/// vertrauenswürdig wie die Session selbst (dieselbe Quelle, kein
/// Modell-Output) und kann die Autorität nie erweitern: Sandbox, Aktivierung,
/// Kontextdecke und Tiefe werden unverändert weiter geschnitten.
#[derive(Debug, Clone)]
struct CheckedOutParent {
    parent_session_id: Option<SessionId>,
    spawn_context: Option<SpawnContext>,
    reasoning_effort: Option<ReasoningEffort>,
    activation: SessionActivation,
    active_model: Option<String>,
    active_provider: Option<String>,
    /// Alle Werkzeugnamen der Registry (ungefiltert; die Admission filtert
    /// wie bei einer Manager-Session über `activation`).
    tool_names: Vec<String>,
    /// Plan R9, E1: der Interaktionsmodus der Session bei der Entnahme
    /// (Plan-Modus-Vererbung an ihre Kinder).
    mode: crate::mode::InteractionMode,
    /// Plan R9, E1/E6: Basis-Aktivierung und Basis-Sandbox (ohne Modus-
    /// Schnitt) für den Eltern-Schnitt eines Kindes, das dem Live-Modus folgt.
    base_activation: SessionActivation,
    base_sandbox: Option<SandboxSpec>,
}

impl CheckedOutParent {
    fn of(session: &AgentSession) -> Self {
        Self {
            parent_session_id: session.parent_session_id().cloned(),
            spawn_context: session.spawn_context().cloned(),
            reasoning_effort: session.reasoning_effort(),
            activation: session.activation().clone(),
            active_model: session
                .active_model()
                .map(|model| model.as_str().to_owned()),
            active_provider: session
                .active_provider()
                .map(|provider| provider.as_str().to_owned()),
            tool_names: session
                .registry()
                .tool_providers()
                .iter()
                .flat_map(|provider| provider.tools())
                .map(|spec| spec.name().to_string())
                .collect(),
            mode: session.mode(),
            base_activation: session.base_activation().clone(),
            base_sandbox: session.base_sandbox().cloned(),
        }
    }
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
    /// Runde 9, E7: Eltern-Sicht der Kinder, deren Session gerade für einen
    /// Turn aus dem Manager entnommen ist (Schlüssel: Kind-ID), siehe
    /// [`CheckedOutParent`]. Lock-Reihenfolge: stets nach `manager`.
    checked_out: Mutex<BTreeMap<String, CheckedOutParent>>,
    /// Runde 9, E6: Live-Modus des Baums; jedes admittierte Kind folgt ihm
    /// (siehe [`Self::live_mode`]).
    live_mode: crate::live_mode::LiveModeBroadcast,
    /// Kontextfenster (Tokens) je Modell-ID des Kindes; `None`-Argument =
    /// Vorgabemodell. Ohne Resolver gilt [`DEFAULT_CHILD_CONTEXT_WINDOW`].
    context_window_resolver: Option<Arc<ContextWindowResolver>>,
    /// Ob der Resolver das Fenster eines Modells kennt (Teil C,
    /// [`Self::with_model_known_probe`]). `None`: keine Warnung.
    model_known_probe: Option<Arc<ModelKnownProbe>>,
    /// Das Modell der extern gefahrenen Wurzel (Teil C,
    /// [`Self::with_root_model`]) — Rückfall für das Kind-Modell, wenn weder
    /// Pin noch Factory-Hauptmodell bekannt sind.
    root_model: Option<String>,
    /// Provider der extern gefahrenen Wurzel ([`Self::with_root_provider`]) —
    /// Rückfall für die Provider-Anzeige eines Kindes.
    root_provider: Option<String>,
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
    /// Exakte Rollennamen, die ein `UserInterface`-Elternteil trotz
    /// `can_spawn(UserInterface, Worker) == false` als `Worker`-Kind
    /// admittieren darf ([`Self::with_uia_spawnable_roles`]). Leer, solange
    /// nichts explizit gesetzt wurde — dann gilt die Spawn-Matrix unverändert.
    uia_spawnable_roles: HashSet<String>,
    /// Plan R9, Teil C/E1: Katalogdaten der registrierten Rollen
    /// ([`Self::with_delegation_catalog`]) — Beschreibung, Skills, Profil und
    /// vor allem die Lese-Eigenschaft für die Plan-Modus-Regel
    /// ([`crate::delegation_visibility::delegable_in_mode`]). Eine Rolle ohne
    /// Eintrag gilt als nicht lesend (fail-closed im Plan-Modus).
    delegation_catalog: BTreeMap<String, harw_extension_api::DelegationTargetInfo>,
    /// Plan R9, E1: der zuletzt gemeldete Plan-Modus der extern gefahrenen
    /// Wurzelsitzung ([`AgentSpawner::note_caller_mode`]); sie liegt nicht im
    /// Manager, ihr Modus ist sonst unsichtbar.
    external_root_plan_mode: std::sync::atomic::AtomicBool,
    /// Runde 5, Teil H: die ungekürzten Antworttexte abgeschlossener
    /// Kind-Läufe samt Elternteil (älteste zuerst), für
    /// [`Self::child_result_text`] bzw. das Werkzeug [`AGENT_RESULT_TOOL`].
    /// Überlebt die Freigabe des Kindes; gedeckelt über
    /// [`CHILD_RESULT_ARCHIVE_MAX_ENTRIES`]/[`CHILD_RESULT_ARCHIVE_MAX_BYTES`].
    child_results: Mutex<VecDeque<ArchivedChildResult>>,
    /// Runde 5, Teil J: was ein Kind am Ende seines Token-Budgets liefert
    /// (Vorgabe: Übergabe-Verdichtung mit Reserve).
    budget_handoff_mode: crate::child_handoff::BudgetHandoffMode,
    /// Runde 5, Teil J: Budget-Übergaben und Fortsetzungsketten.
    handoff_ledger: Mutex<crate::child_handoff::HandoffLedger>,
    /// Runde 5, Teil K: Hintergrund-Läufe und Orchestrierungsgrenzen
    /// (`crate::background_children`).
    pub(crate) background: Arc<crate::background_children::BackgroundChildren>,
    /// Runde 5, Teil M: Aktivitätsjournale, Endberichte und
    /// Eltern-Kind-Nachrichten (`crate::child_comms`); geteilt mit dem
    /// [`Self::progress_observer`].
    pub(crate) comms: Arc<crate::child_comms::ChildComms>,
    /// Runde 5, Teil O: optionaler Freigabe-Kanal zur Oberfläche
    /// (`crate::child_approval`); leer = bisheriges fail-closed-Verhalten.
    pub(crate) child_approvals: Arc<crate::child_approval::ChildApprovalRelay>,
    /// Wave 3, part 3C: when set, a compiled parent's children run through
    /// this backend (e.g. as jobs, see `crate::child_backend`) instead of
    /// the in-process session below. `None` (the default) keeps every
    /// existing behavior unchanged.
    child_backend: Option<Arc<dyn crate::child_backend::ChildBackend>>,
    /// Freigabemodus der Wurzel dieses Baums (dieselbe Zelle, der die
    /// Kind-Ketten über `ApprovalModeCell::follower` folgen). Liest er
    /// [`harw_extension_api::ApprovalMode::FullAccess`], bekommt ein über [`Self::child_backend`]
    /// laufendes Kind `full_access` (siehe [`Self::backend_rights`]).
    /// `None` = fail-closed: nie automatische Freigabe.
    approval_mode: Option<harw_extension_api::approval_mode::ApprovalModeCell>,
}

/// Ein archivierter, ungekürzter Antworttext eines abgeschlossenen
/// Kind-Laufs (Runde 5, Teil H).
#[derive(Debug, Clone)]
struct ArchivedChildResult {
    /// Kind-ID ([`SessionId::as_str`]).
    child: String,
    /// Der Elternteil, der das Kind gestartet hat — nur er darf den Text lesen.
    parent: SessionId,
    /// Der ungekürzte Text.
    text: String,
}

/// Höchstzahl archivierter Kind-Antworten je Spawner (Runde 5, Teil H);
/// darüber fällt die älteste heraus.
pub const CHILD_RESULT_ARCHIVE_MAX_ENTRIES: usize = 64;

/// Gesamtobergrenze (Bytes) der archivierten Kind-Antworten je Spawner
/// (Runde 5, Teil H); darüber fallen die ältesten heraus. Ein einzelner
/// Text über dieser Grenze wird gar nicht archiviert.
pub const CHILD_RESULT_ARCHIVE_MAX_BYTES: usize = 8 * 1024 * 1024;

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

/// Runde 5, Teil M: die admittierten Vorfahren von `child` (Elternteil
/// zuerst), höchstens so viele wie Einträge in der Registry (zyklenfest).
fn active_ancestors(
    active: &Mutex<BTreeMap<String, ChildRecord>>,
    child: &SessionId,
) -> Vec<SessionId> {
    let Ok(active) = active.lock() else {
        return Vec::new();
    };
    let mut ancestors = Vec::new();
    let mut cursor = child.as_str().to_owned();
    for _ in 0..active.len() {
        let Some(parent) = active.get(&cursor).map(|record| record.parent.clone()) else {
            break;
        };
        if !active.contains_key(parent.as_str()) {
            break;
        }
        cursor = parent.as_str().to_owned();
        ancestors.push(parent);
    }
    ancestors
}

/// Runde 5, Teil M: schreibt jeden Werkzeugaufruf eines Kindes in sein
/// Aktivitätsjournal und reicht ihn an den Beobachter der Fabrik weiter.
struct JournalToolObserver {
    comms: Arc<crate::child_comms::ChildComms>,
    inner: Option<Arc<dyn crate::capture::ToolOutcomeObserver>>,
}

impl crate::capture::ToolOutcomeObserver for JournalToolObserver {
    fn on_tool_outcome(&self, session_id: &SessionId, outcome: &crate::capture::ToolOutcome<'_>) {
        self.comms.record_tool_outcome(
            session_id,
            outcome.tool_name,
            outcome.arguments,
            outcome.status == crate::capture::ToolOutcomeStatus::Success,
            outcome.output_text,
        );
        if let Some(inner) = &self.inner {
            inner.on_tool_outcome(session_id, outcome);
        }
    }

    fn on_turn_finished(&self, session_id: &SessionId) {
        if let Some(inner) = &self.inner {
            inner.on_turn_finished(session_id);
        }
    }

    fn on_user_message(&self, session_id: &SessionId, text: &str) {
        if let Some(inner) = &self.inner {
            inner.on_user_message(session_id, text);
        }
    }

    fn on_assistant_message(&self, session_id: &SessionId, text: &str) {
        if let Some(inner) = &self.inner {
            inner.on_assistant_message(session_id, text);
        }
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
    /// Runde 5, Teil M: Journale und Postfächer.
    comms: Arc<crate::child_comms::ChildComms>,
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

    fn on_assistant_text(&self, session_id: &SessionId, text: &str) {
        // Runde 5, Teil M: letzter Assistententext ins Aktivitätsjournal.
        self.comms.set_last_assistant(session_id, text);
    }

    fn take_inbound_messages(&self, session_id: &SessionId) -> Vec<String> {
        // Runde 5, Teil M: Postfach (`agent.message`/`parent.message`).
        self.comms.take_inbound(session_id)
    }

    fn on_progress(&self, session_id: &SessionId) {
        let now = Timestamp::now();
        renew_active_lease(&self.active, self.lease_seconds, session_id, now);
        // Runde 5, Teil M: ein Elternteil, der auf ein arbeitendes Kind
        // wartet, lebt ebenfalls — seine Lease (und die aller Vorfahren)
        // läuft nicht ab, solange ein Nachkomme Fortschritt meldet.
        for ancestor in active_ancestors(&self.active, session_id) {
            renew_active_lease(&self.active, self.lease_seconds, &ancestor, now);
        }
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
        provider: record.provider.clone(),
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
            // Teil C: Admission-Warnungen (unbekanntes Modell, gekürzter
            // Auftrag) erscheinen im Agent-Panel in der Spur des Kindes.
            AgentOrchestrationStatus::Admitted => state.and_then(|state| state.admission_warning),
            _ => None,
        };
        emit_orchestration_event(observer, root_session_id, record, task, detail, status);
    }

    /// Höchstlänge (Bytes) des Auftrags-Texts eines Kindes aus der
    /// Admission (Teil C); `None`, wenn unbekannt oder unbegrenzt.
    fn child_task_max_bytes(&self, child: &SessionId) -> Option<usize> {
        self.child_task_state(child)
            .and_then(|state| state.task_max_bytes)
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

    /// Wave 3C: entnimmt den zuletzt vom [`crate::child_backend::ChildBackend`]
    /// gemeldeten Fortsetzungs-Token eines Kindes (einmalig), für
    /// [`crate::child_backend::ChildRunSpec::continue_from`] seines nächsten
    /// Laufs.
    fn take_backend_resume(&self, child: &SessionId) -> Option<String> {
        self.child_tasks
            .lock()
            .ok()
            .and_then(|mut tasks| tasks.get_mut(child.as_str())?.backend_resume.take())
    }

    /// Wave 3C: hinterlegt den Fortsetzungs-Token, den ein
    /// [`crate::child_backend::ChildBackend`] für einen erneuten Lauf
    /// desselben Kindes gemeldet hat.
    fn set_backend_resume(&self, child: &SessionId, token: String) {
        match self.child_tasks.lock() {
            Ok(mut tasks) => {
                tasks
                    .entry(child.as_str().to_owned())
                    .or_default()
                    .backend_resume = Some(token);
            }
            Err(_) => tracing::warn!(child = %child, "child_task_state.lock_poisoned"),
        }
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
        // Runde 5, Teil C: die Fabrik der Rolle erfährt die Freigabe (Diary-
        // Sitzungsende des Kindes). Nur mit Record — ein zweiter, leerer
        // Freigabeversuch meldet nichts, also höchstens einmal je Kind-Lauf.
        if let Some(record) = record
            && let Some(definition) = self.roles.get(&record.role)
        {
            definition
                .registry_factory
                .child_session_released(&record.role, child);
        }
        // Runde 5, Teil M: das Ende des Kindes im Journal seines
        // Elternteils (Enkel-Kurzform), bevor der Auftragszustand fällt.
        if let Some(record) = record {
            let detail = self
                .child_task_state(child)
                .and_then(|state| state.outcome_detail);
            let outcome = match (self.comms.end_report(child), detail) {
                (Some(end), _) => format!("{}: {}", end.status.as_str(), end.reason),
                (None, Some(detail)) => format!("{}: {detail}", record.status.as_str()),
                (None, None) => record.status.as_str().to_owned(),
            };
            self.comms
                .record_grandchild_end(&record.parent, child, &record.role, &outcome);
        }
        self.forget_child_state(child);
    }

    /// Entfernt Auftrags- und Fortschrittszustand eines nicht mehr aktiven Kindes.
    fn forget_child_state(&self, child: &SessionId) {
        // Runde 5, Teil M: Journal schließen (bleibt begrenzt vorrätig),
        // Postfach und offene Frage fallen weg.
        self.comms.close_journal(child);
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

    /// Setzt die Prüfung, ob der Resolver das Fenster eines Modells kennt
    /// (Teil C).
    ///
    /// # Beschreibung
    /// Liefert sie für das Modell eines Kindes `false`, gilt dort nur das
    /// konservative Rückfallfenster: die Admission meldet das laut
    /// (`tracing::warn!` und `detail` des `Admitted`-Ereignisses, das das
    /// Agent-Panel in die Spur des Kindes schreibt).
    #[must_use]
    pub fn with_model_known_probe(mut self, probe: Arc<ModelKnownProbe>) -> Self {
        self.model_known_probe = Some(probe);
        self
    }

    /// Setzt das Modell der extern gefahrenen Wurzel (Teil C).
    ///
    /// # Beschreibung
    /// Rückfall in der Kette für das Kind-Modell: festgelegtes Rollenmodell
    /// (Pin) → Hauptmodell der Factory → Modell des Elternteils (eines
    /// admittierten Kindes: dessen Modell; der Wurzel: dieser Wert) →
    /// Vorgabe des Resolvers.
    #[must_use]
    pub fn with_root_model(mut self, model: Option<String>) -> Self {
        self.root_model = model;
        self
    }

    /// Setzt den Provider der extern gefahrenen Wurzel (Anzeige
    /// `<provider>/<modell>` eines Kindes, Rückfall wie
    /// [`Self::with_root_model`]).
    #[must_use]
    pub fn with_root_provider(mut self, provider: Option<String>) -> Self {
        self.root_provider = provider;
        self
    }

    /// Runde 5, Teil J: legt fest, was ein Kind am Ende seines Token-Budgets
    /// liefert.
    ///
    /// # Arguments
    /// - `mode` ([`crate::child_handoff::BudgetHandoffMode`]): `Compact`
    ///   (Vorgabe) hält eine Reserve zurück und liefert eine
    ///   Übergabe-Verdichtung; `LastAnswer` ist das Verhalten vor Runde 5
    ///   (letzte Assistant-Antwort, keine Reserve).
    #[must_use]
    pub fn with_budget_handoff(mut self, mode: crate::child_handoff::BudgetHandoffMode) -> Self {
        self.budget_handoff_mode = mode;
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
            checked_out: Mutex::new(BTreeMap::new()),
            live_mode: crate::live_mode::LiveModeBroadcast::default(),
            context_window_resolver: None,
            model_known_probe: None,
            root_model: None,
            root_provider: None,
            compaction_ceiling: None,
            orchestration_observer: None,
            child_tasks: Mutex::new(BTreeMap::new()),
            progress_sinks: Arc::new(Mutex::new(BTreeMap::new())),
            freed: tokio::sync::Notify::new(),
            uia_spawnable_roles: HashSet::new(),
            delegation_catalog: BTreeMap::new(),
            external_root_plan_mode: std::sync::atomic::AtomicBool::new(false),
            child_results: Mutex::new(VecDeque::new()),
            budget_handoff_mode: crate::child_handoff::BudgetHandoffMode::default(),
            handoff_ledger: Mutex::new(crate::child_handoff::HandoffLedger::default()),
            // Runde 5, Teil K.
            background: Arc::new(crate::background_children::BackgroundChildren::default()),
            // Runde 5, Teil M.
            comms: Arc::new(crate::child_comms::ChildComms::default()),
            // Runde 5, Teil O.
            child_approvals: Arc::new(crate::child_approval::ChildApprovalRelay::default()),
            // Wave 3, part 3C.
            child_backend: None,
            approval_mode: None,
        }
    }

    /// Wires a [`crate::child_backend::ChildBackend`]: every child this
    /// spawner admits from now on runs through it instead of the in-process
    /// session (plan §3C). `None` by default, which keeps every existing
    /// behavior unchanged; callers that delegate to it are responsible for
    /// mapping [`crate::child_backend::ChildRunOutcome`] onto the same
    /// journal/end-report path the in-process run uses.
    #[must_use]
    pub fn with_child_backend(
        mut self,
        backend: Arc<dyn crate::child_backend::ChildBackend>,
    ) -> Self {
        self.child_backend = Some(backend);
        self
    }

    /// Binds the approval-mode cell of this tree's root: while it reads
    /// [`harw_extension_api::ApprovalMode::FullAccess`], a child run through
    /// [`Self::with_child_backend`] gets
    /// [`crate::child_backend::ChildBackendRights::full_access`] — the same
    /// "no confirmations" an in-process child gets through its follower
    /// cell. Without a cell a backend-run child never auto-approves.
    #[must_use]
    pub fn with_approval_mode(
        mut self,
        mode: harw_extension_api::approval_mode::ApprovalModeCell,
    ) -> Self {
        self.approval_mode = Some(mode);
        self
    }

    /// The wired [`crate::child_backend::ChildBackend`], if any (see
    /// [`Self::with_child_backend`]).
    #[must_use]
    pub(crate) fn child_backend(&self) -> Option<Arc<dyn crate::child_backend::ChildBackend>> {
        self.child_backend.clone()
    }

    /// Erlaubt einem `UserInterface`-Elternteil (der UIA-Wurzelsitzung), die
    /// hier exakt benannten Rollen als `Worker`-Kind zu admittieren — eine
    /// enge, explizite Ausnahme von
    /// `harw_agent_dsl::roles::can_spawn(UserInterface, Worker) == false`.
    ///
    /// # Beschreibung
    /// Die Ausnahme greift ausschließlich, wenn **alle drei** Bedingungen
    /// gelten: Elternrolle `UserInterface`, Kindrolle `Worker` und der exakte
    /// registrierte Rollenname des Kindes steht in dieser Liste. Jede andere
    /// Kombination (anderer Elternteil, andere Kindrolle, nicht gelisteter
    /// Name) läuft unverändert durch [`can_delegate_to`] — die allgemeine
    /// Spawn-Matrix wird nicht aufgeweicht. Mehrfache Aufrufe ergänzen die
    /// Liste.
    ///
    /// Die Ausnahme wirkt nur auf die Admission, nicht auf die dem Modell
    /// gezeigte Delegationszielliste (`delegation_visibility`): gedacht ist
    /// sie für Runtime-Operationen wie `/matrix`, die aus der UIA-Wurzel
    /// heraus je Sitz genau ein Kind mit fester Rolle erzeugen.
    ///
    /// # Sicherheit
    /// Die UIA-Wurzel darf regulär keine `Worker` erzeugen, weil ein Worker
    /// Werkzeuge (Schreiben, Shell, Netz) tragen kann, die die UIA selbst
    /// bewusst nicht direkt delegieren soll. In diese Liste gehören daher
    /// **nur lesende Rollen ohne Netz, Schreiben oder Exec** (für die
    /// Matrix-Sitze Registry-Profil `MatrixReader`: höchstens lesende
    /// Datei-Werkzeuge wie `fs.read`/`fs.list`/`fs.search`/`fs.glob`/
    /// `fs.grep`/`doc.read_pdf`; Authority-Reducer `ReadOnly`), deren
    /// Kind-Sitzung nichts schreiben, ausführen oder ins Netz tragen kann.
    /// Diese Eigenschaft prüft der Controller nicht selbst — er kennt nur
    /// Namen und Organisationsrollen. Sie sicherzustellen ist Verantwortung des
    /// Aufrufers (Runtime-Montage); für die Matrix-Rollen belegt das
    /// `harw-registry-defaults/tests/uia_spawn_authority.rs`.
    ///
    /// # Arguments
    /// - `roles` (`impl IntoIterator<Item = String>`): exakte Rollennamen.
    ///
    /// # Returns
    /// Den Spawner mit ergänzter Freigabeliste.
    ///
    /// # Beispiel
    /// ```ignore
    /// let spawner = spawner.with_uia_spawnable_roles(
    ///     role_names::MATRIX_ROLES.iter().map(|s| (*s).to_owned()),
    /// );
    /// ```
    #[must_use]
    pub fn with_uia_spawnable_roles(mut self, roles: impl IntoIterator<Item = String>) -> Self {
        self.uia_spawnable_roles.extend(roles);
        self
    }

    /// Plan R9, Teil C/E1: hinterlegt die Katalogdaten der registrierten
    /// Rollen (aus dem Agenten-Roster der Runtime).
    ///
    /// # Beschreibung
    /// Die Daten verleihen keine Rechte. Sie speisen `agents.catalog`, die
    /// Beschreibungen der `transfer_to_*`-Werkzeuge und die Plan-Modus-Regel:
    /// nur Einträge mit `read_only = true` bleiben im Plan-Modus delegierbar.
    /// Ohne Katalog (Tests, eingebettete Spawner) gilt im Plan-Modus kein
    /// Ziel als lesend — die Sichtbarkeit ist dann leer wie vor Plan R9, und
    /// die Admission prüft den Plan-Modus nicht zusätzlich (die Sandbox des
    /// Elternteils im Plan-Modus schneidet das Kind ohnehin).
    ///
    /// # Arguments
    /// - `entries`: Katalogdaten; der Schlüssel ist `DelegationTargetInfo::name`.
    #[must_use]
    pub fn with_delegation_catalog(
        mut self,
        entries: impl IntoIterator<Item = harw_extension_api::DelegationTargetInfo>,
    ) -> Self {
        for entry in entries {
            self.delegation_catalog.insert(entry.name.clone(), entry);
        }
        self
    }

    /// Katalogdaten eines registrierten Ziels; ohne Katalogeintrag nur der
    /// Name und die Organisationsrolle (nicht lesend).
    fn delegation_target_info(
        &self,
        name: &str,
        role: harw_agent_dsl::roles::AgentRoleId,
    ) -> harw_extension_api::DelegationTargetInfo {
        self.delegation_catalog
            .get(name)
            .cloned()
            .unwrap_or_else(|| harw_extension_api::DelegationTargetInfo {
                role: crate::delegation_visibility::role_label(role).to_owned(),
                ..harw_extension_api::DelegationTargetInfo::named(name.to_owned())
            })
    }

    /// Prüft die Spawn-Berechtigung für eine Admission: die allgemeine
    /// [`can_delegate_to`]-Regel, ergänzt nur um die enge UIA-Freigabeliste
    /// aus [`Self::with_uia_spawnable_roles`].
    ///
    /// # Arguments
    /// - `caller_role`: Organisationsrolle des Elternteils.
    /// - `target_role`: Organisationsrolle des Kindes.
    /// - `target_role_name`: exakter registrierter Rollenname des Kindes.
    /// - `allowed_child_orchestrators`: Kind-Orchestrator-Freigabeliste des
    ///   Elternteils.
    ///
    /// # Returns
    /// `true`, wenn die Admission organisatorisch erlaubt ist.
    fn spawn_permitted(
        &self,
        caller_role: harw_agent_dsl::roles::AgentRoleId,
        target_role: harw_agent_dsl::roles::AgentRoleId,
        target_role_name: &str,
        allowed_child_orchestrators: &[String],
    ) -> bool {
        if caller_role == harw_agent_dsl::roles::AgentRoleId::UserInterface
            && target_role == harw_agent_dsl::roles::AgentRoleId::Worker
            && self.uia_spawnable_roles.contains(target_role_name)
        {
            return true;
        }
        can_delegate_to(
            caller_role,
            target_role,
            target_role_name,
            allowed_child_orchestrators,
        )
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

    /// Runde 9, E6: der Live-Modus dieses Agentenbaums.
    ///
    /// # Beschreibung
    /// Die Oberfläche veröffentlicht darüber jeden Moduswechsel der Wurzel
    /// (`/mode`, Shift+Tab, Planfreigabe). Jedes über diesen Spawner
    /// admittierte Kind startet im zuletzt veröffentlichten Modus und
    /// übernimmt spätere Wechsel an seiner nächsten Runden-Grenze — auch
    /// Hintergrund-Kinder und Enkel. Ohne Veröffentlichung behalten Kinder
    /// ihren eigenen Modus.
    #[must_use]
    pub fn live_mode(&self) -> &crate::live_mode::LiveModeBroadcast {
        &self.live_mode
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
            comms: Arc::clone(&self.comms),
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

    /// Runde 5, Teil O: gibt einem abgekoppelten (Hintergrund-)Kind einen
    /// eigenen, vom Eltern-Turn unabhängigen Cancel-Token.
    ///
    /// # Beschreibung
    /// Ein Hintergrund-Kind überlebt den Turn, der es gestartet hat; ein
    /// Abbruch jenes Turns (Esc/Ctrl+C, nachdem das Werkzeug schon „running“
    /// zurückgab) darf es nicht mitreißen. Abbrechen lässt es sich danach nur
    /// noch gezielt über [`Self::request_cancellation`] (`agent.cancel`,
    /// `/agent cancel`, `/new`, `/resume`, Beenden) oder sein Budget. Muss
    /// **vor** dem ersten Lauf aufgerufen werden: später admittierte
    /// Nachkommen leiten sich dann vom neuen Token ab.
    ///
    /// Runde 7, Teil A7: traf den startenden UIA-Turn **während des
    /// Starts** ein Abbruch (Turn-Grenze/Budget oder Esc), trägt der Token
    /// des Kindes nur den geerbten Grund [`CancelReason::Parent`]. Der Start
    /// wird dann trotzdem abgeschlossen (frischer Token) — das Kind gehört
    /// ab jetzt nicht mehr zum Turn. Ein Abbruch des Kindes selbst
    /// (`agent.cancel`, Budget, Lease) und ein geerbtes Herunterfahren
    /// ([`CancelReason::Shutdown`]/[`CancelReason::LeaseLost`] am
    /// Elternteil) werden nie zurückgenommen.
    ///
    /// # Returns
    /// `true`, wenn der Token ersetzt wurde; `false` für unbekannte oder
    /// selbst abgebrochene Kinder.
    pub(crate) fn detach_cancel_token(&self, child: &SessionId) -> bool {
        let parent_reason = self
            .child_record(child)
            .and_then(|record| self.parent_cancel_reason(&record.parent));
        let Ok(mut tokens) = self.cancellations.lock() else {
            return false;
        };
        let (replace, revived) = match tokens.get(child.as_str()) {
            Some(current) if !current.is_cancelled() => (true, false),
            Some(current) => {
                let survives = detached_start_survives(current.reason(), parent_reason);
                (survives, survives)
            }
            None => (false, false),
        };
        if revived {
            tracing::warn!(
                child = %child,
                ?parent_reason,
                "background_child.start_survives_turn_abort"
            );
        }
        if replace {
            tokens.insert(child.as_str().to_owned(), CancelToken::new());
        }
        replace
    }

    /// Abbruchgrund des registrierten Tokens eines Elternteils (Wurzel)
    /// bzw. eines admittierten Eltern-Kindes (Runde 7, Teil A7).
    fn parent_cancel_reason(&self, parent: &SessionId) -> Option<CancelReason> {
        if let Some(reason) = self.parent_tokens.lock().ok().and_then(|tokens| {
            tokens
                .get(parent.as_str())
                .map(|entry| entry.token.reason())
        }) {
            return reason;
        }
        self.child_cancel_token(parent)
            .and_then(|token| token.reason())
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
                self.check_child_over_budget(record, session.total_usage().fresh_tokens());
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
        // Runde 9, E7: die Eltern-Sicht gilt nur, solange die Session fehlt.
        self.check_in(&child);
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

    /// Friert die minimalen, nicht-autorisierenden Rekonstruktionsdaten eines
    /// bereits admittierten Kindes ein.
    ///
    /// Der Aufruf ist für den kurzen Zeitraum zwischen Admission und Start des
    /// Kind-Turns gedacht. Während eines laufenden Turns ist die Session aus
    /// dem Manager ausgecheckt; dann liefert diese Methode bewusst None
    /// statt aus teilweise sichtbarem Zustand einen Snapshot zu erfinden.
    #[must_use]
    pub fn child_recovery_view(&self, child: &SessionId) -> Option<ChildRecoveryView> {
        let record = self.child_record(child)?;
        let manager = self.manager.lock().ok()?;
        let session = manager.get(child).ok()?;
        let context = session.spawn_context()?;
        let base_sandbox = session.base_sandbox().unwrap_or(&context.sandbox);

        let (capability_agent, mut activated_capabilities) =
            if let Some(snapshot) = context.capability_snapshot.as_ref() {
                let activated = snapshot
                    .activated
                    .iter()
                    .map(|capability| ChildRecoveryCapability {
                        kind: match &capability.kind {
                            SuggestionKind::Skill => "skill",
                            SuggestionKind::Plugin => "plugin",
                            SuggestionKind::Mcp => "mcp",
                        }
                        .to_owned(),
                        name: capability.name.clone(),
                        definition_sha256: capability.definition_sha256.clone(),
                    })
                    .collect::<Vec<_>>();
                (Some(snapshot.agent.clone()), activated)
            } else {
                (None, Vec::new())
            };
        activated_capabilities.sort_by(|left, right| {
            (&left.kind, &left.name, &left.definition_sha256).cmp(&(
                &right.kind,
                &right.name,
                &right.definition_sha256,
            ))
        });

        let mut allowed_child_orchestrators = context.allowed_child_orchestrators.clone();
        allowed_child_orchestrators.sort();

        Some(ChildRecoveryView {
            child: record.child,
            parent: record.parent,
            handoff_call_id: record.handoff_call_id,
            role: record.role,
            depth: record.depth,
            depth_ceiling: record.depth_ceiling,
            budget: record.budget.into(),
            task_complexity: record.task_complexity.map(|complexity| match complexity {
                TaskComplexity::Simple => "simple".to_owned(),
                TaskComplexity::Complex => "complex".to_owned(),
            }),
            model: record.model,
            provider: record.provider,
            authority: base_sandbox.authority().snapshot(),
            approval_actor: context.approval_actor.clone(),
            organizational_role: context.organizational_role,
            allowed_child_orchestrators,
            context_ceiling: context.ceiling.clone(),
            mode: session.mode().as_str().to_owned(),
            executable_snapshot_id: session
                .executable_snapshot_id()
                .map(ReferencedSnapshotId::from_computed),
            capability_agent,
            activated_capabilities,
            trace: record.trace,
        })
    }

    /// Validates and narrows the currently frozen capability contract against
    /// a persisted recovery envelope. Persisted capabilities can only remove
    /// current activations; every persisted activation must still exist with
    /// the same definition digest.
    fn recovery_capability_snapshot(
        mut current: Option<SpawnCapabilitySnapshot>,
        recovery: &ChildRecoveryView,
    ) -> Result<Option<SpawnCapabilitySnapshot>, AdmitRejection> {
        let kind = |kind: &SuggestionKind| match kind {
            SuggestionKind::Skill => "skill",
            SuggestionKind::Plugin => "plugin",
            SuggestionKind::Mcp => "mcp",
        };
        match (&mut current, recovery.capability_agent.as_deref()) {
            (None, None) if recovery.activated_capabilities.is_empty() => Ok(None),
            (Some(snapshot), None) if recovery.activated_capabilities.is_empty() => {
                snapshot.activated.clear();
                Ok(current)
            }
            (Some(snapshot), Some(agent)) if snapshot.agent == agent => {
                for expected in &recovery.activated_capabilities {
                    let present = snapshot.activated.iter().any(|capability| {
                        kind(&capability.kind) == expected.kind
                            && capability.name == expected.name
                            && capability.definition_sha256 == expected.definition_sha256
                    });
                    if !present {
                        return Err(AdmitRejection::Other(Self::reject(format!(
                            "agent recovery capability '{}:{}' changed or disappeared",
                            expected.kind, expected.name
                        ))));
                    }
                }
                snapshot.activated.retain(|capability| {
                    recovery.activated_capabilities.iter().any(|expected| {
                        kind(&capability.kind) == expected.kind
                            && capability.name == expected.name
                            && capability.definition_sha256 == expected.definition_sha256
                    })
                });
                Ok(current)
            }
            _ => Err(AdmitRejection::Other(Self::reject(
                "agent recovery capability contract no longer matches current trusted catalog",
            ))),
        }
    }

    /// Rehydrates an already-admitted direct child of the durable external
    /// root under today's policy, retaining its stable child id and transcript.
    ///
    /// Nested recovery is intentionally fail-closed in this wave: descendant
    /// aggregate budget accounting is not yet durable enough to prove that
    /// reattaching a nested child cannot restore spent delegation budget.
    pub async fn recover_root_child(
        &self,
        recovery: &ChildRecoveryView,
        store: &dyn StateStore,
    ) -> Result<SessionId, AgentSpawnError> {
        let root = self
            .external_root_parent
            .as_ref()
            .filter(|root| root.session_id == recovery.parent)
            .ok_or_else(|| {
                Self::reject(
                    "agent recovery requires the same durable external root; nested recovery is not enabled",
                )
            })?;
        if !recovery
            .authority
            .workspace()
            .matches(root.spawn_context.sandbox.workspace())
        {
            return Err(Self::reject(
                "agent recovery workspace does not match the current durable root",
            ));
        }

        let mut context = serde_json::Map::new();
        if let Some(complexity) = recovery.task_complexity.as_deref() {
            context.insert(
                "complexity".to_owned(),
                serde_json::Value::String(complexity.to_owned()),
            );
        }
        let input = SpawnInput {
            parent_session_id: recovery.parent.clone(),
            handoff_call_id: recovery.handoff_call_id.clone(),
            instructions: None,
            context: serde_json::Value::Object(context),
            ceiling: recovery.context_ceiling.clone(),
        };
        let sandbox = root
            .spawn_context
            .sandbox
            .restrict(recovery.authority.request());
        let child = self
            .admit_inner(&recovery.role, input, sandbox, None, Some(recovery))
            .map_err(AdmitRejection::into_error)?;

        let mut session = {
            let mut manager = self
                .manager
                .lock()
                .map_err(|_| Self::reject("session manager lock is poisoned"))?;
            manager.remove(&child).ok_or_else(|| {
                Self::reject(format!(
                    "recovered child {child} disappeared before transcript hydration"
                ))
            })?
        };
        if let Err(error) = session.hydrate_from_store(store).await {
            let _ = self.release_child(&child);
            return Err(Self::reject(format!(
                "could not hydrate recovered child {child}: {error}"
            )));
        }
        {
            let mut manager = self
                .manager
                .lock()
                .map_err(|_| Self::reject("session manager lock is poisoned"))?;
            manager.restore(session).map_err(|error| {
                Self::reject(format!(
                    "could not restore hydrated child {child}: {error}"
                ))
            })?;
        }

        // Restore the dimensions that can be proven from durable session
        // state/history. Wall time of the interrupted process is not durable;
        // exhaust a finite inherited wall budget rather than guessing low.
        let tokens = self.child_token_usage(&child)?;
        let tool_calls = self.child_tool_call_count(&child)?;
        let mut active = self
            .active
            .lock()
            .map_err(|_| Self::reject("child registry lock is poisoned"))?;
        let record = active.get_mut(child.as_str()).ok_or_else(|| {
            Self::reject(format!(
                "recovered child {child} disappeared before usage restoration"
            ))
        })?;
        let restored_usage = ChildUsage {
            tokens,
            tool_calls,
            wall_time_ms: record.budget.max_wall_time_ms.unwrap_or(0),
        };
        record.consumed = restored_usage;
        record.charged_to_parent = restored_usage;
        drop(active);

        tracing::info!(
            child = %child,
            tokens,
            tool_calls,
            "agent_recovery.root_child_rehydrated"
        );
        Ok(child)
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
    /// Runde 5, Teil H: die Kürzungsmarke nennt [`AGENT_RESULT_TOOL`] mit der
    /// Kind-ID ([`cap_child_return_text_for_child`]); der ungekürzte Text
    /// steht über [`Self::child_result_text`] bereit.
    ///
    /// # Errors
    /// Returns [`AgentSpawnError`] when the child is not admitted, its restored
    /// session is unavailable, or its history contains no assistant text.
    pub fn child_final_assistant_text(&self, child: &SessionId) -> Result<String, AgentSpawnError> {
        let text = self.child_final_assistant_text_full(child)?;
        Ok(cap_child_return_text_for_child(
            &text,
            CHILD_RETURN_MAX_BYTES,
            child,
        ))
    }

    /// Runde 5, Teil H: legt den ungekürzten Antworttext eines
    /// abgeschlossenen Kind-Laufs im Ergebnisarchiv ab.
    ///
    /// # Beschreibung
    /// Ein erneuter Lauf desselben Kindes ersetzt den älteren Eintrag. Danach
    /// fallen die ältesten Einträge heraus, bis höchstens
    /// [`CHILD_RESULT_ARCHIVE_MAX_ENTRIES`] Einträge mit zusammen höchstens
    /// [`CHILD_RESULT_ARCHIVE_MAX_BYTES`] Bytes übrig sind. Ein einzelner
    /// Text über der Byte-Grenze wird nicht archiviert. Best-effort: eine
    /// vergiftete Sperre wird nur protokolliert.
    ///
    /// # Argumente
    /// - `child` (`&SessionId`): das abgeschlossene Kind.
    /// - `parent` (`&SessionId`): sein Elternteil (aus dem Admission-Record).
    /// - `text` (`&str`): der ungekürzte Antworttext.
    fn archive_child_result(&self, child: &SessionId, parent: &SessionId, text: &str) {
        if text.is_empty() || text.len() > CHILD_RESULT_ARCHIVE_MAX_BYTES {
            return;
        }
        let Ok(mut archive) = self.child_results.lock() else {
            tracing::warn!(child = %child, "child_result_archive.lock_poisoned");
            return;
        };
        archive.retain(|entry| entry.child != child.as_str());
        archive.push_back(ArchivedChildResult {
            child: child.as_str().to_owned(),
            parent: parent.clone(),
            text: text.to_owned(),
        });
        let mut total: usize = archive.iter().map(|entry| entry.text.len()).sum();
        while archive.len() > CHILD_RESULT_ARCHIVE_MAX_ENTRIES
            || total > CHILD_RESULT_ARCHIVE_MAX_BYTES
        {
            let Some(evicted) = archive.pop_front() else {
                break;
            };
            total = total.saturating_sub(evicted.text.len());
        }
    }

    /// Runde 5, Teil H: archiviert den Text eines abgeschlossenen Kindes,
    /// sofern es (noch) einen Admission-Record mit Elternteil hat.
    fn archive_completed_child(&self, child: &SessionId, text: Option<&str>) {
        if let (Some(text), Some(record)) = (text, self.child_record(child)) {
            self.archive_child_result(child, &record.parent, text);
        }
    }

    /// Liefert den **ungekürzten** Antworttext eines eigenen, abgeschlossenen
    /// Kind-Laufs (Runde 5, Teil H; Werkzeug [`AGENT_RESULT_TOOL`]).
    ///
    /// # Beschreibung
    /// Quelle ist zuerst das Ergebnisarchiv (überlebt die Freigabe des
    /// Kindes), sonst — solange das Kind noch admittiert ist und
    /// abgeschlossen hat — seine letzte Assistenten-Antwort. Der Text wird nur
    /// herausgegeben, wenn `caller` der Elternteil des Kindes ist.
    ///
    /// # Argumente
    /// - `caller` (`&SessionId`): die aufrufende Sitzung (aus dem
    ///   Ausführungskontext, nie aus Modell-Argumenten).
    /// - `child` (`&SessionId`): die angefragte Kind-ID.
    ///
    /// # Errors
    /// [`AgentSpawnError`] mit immer derselben Meldung, wenn das Kind
    /// unbekannt ist, einem anderen Elternteil gehört, noch nicht
    /// abgeschlossen hat oder kein Antworttext vorliegt — die Meldung verrät
    /// nicht, ob eine fremde Kind-ID existiert.
    pub fn child_result_text(
        &self,
        caller: &SessionId,
        child: &SessionId,
    ) -> Result<String, AgentSpawnError> {
        let unavailable = || {
            Self::reject(format!(
                "kein abgeschlossenes eigenes Kind mit der ID {child} (oder sein Ergebnis ist \
                 nicht mehr vorrätig)"
            ))
        };
        let archived = self
            .child_results
            .lock()
            .map_err(|_| Self::reject("child result archive lock is poisoned"))?
            .iter()
            .rev()
            .find(|entry| entry.child == child.as_str())
            .cloned();
        if let Some(entry) = archived {
            return if &entry.parent == caller {
                Ok(entry.text)
            } else {
                Err(unavailable())
            };
        }
        match self.child_record(child) {
            Some(record) if &record.parent == caller && record.status == ChildStatus::Completed => {
                self.child_final_assistant_text_full(child)
                    .map_err(|_| unavailable())
            }
            _ => Err(unavailable()),
        }
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
                            ContentPart::ImageUrl { .. } | ContentPart::Media { .. } => None,
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
            // Runde 5, Teil M: Lease-Ablauf im Journal des Elternteils.
            self.comms.record_grandchild_end(
                &record.parent,
                &record.child,
                &record.role,
                "timeout: Lease abgelaufen",
            );
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

    /// Führt einen Kind-Turn unter dem **bei der Admission hinterlegten**
    /// Budget aus.
    ///
    /// # Beschreibung
    /// Runde 7, Teil A1: Transfers (`transfer_to_*`, synchron wie im
    /// Hintergrund) liefen bisher über [`Self::run_child`] und damit ohne
    /// jede Budgetprüfung — die Token-, Aufruf- und Zeitgrenzen aus
    /// `[spawn.budget]` der Agent-TOML griffen nie, ebenso wenig die
    /// Abschlussrunde bei 80 % und die Übergabe-Verdichtung. Diese Methode
    /// liest den Deckel aus dem [`ChildRecord`] (dieselbe Quelle, die
    /// `delegate_wave` über [`Self::child_budget`] nutzt) und läuft über
    /// [`Self::run_child_with_budget`], also denselben
    /// `enforce_child_budget`-Pfad. Ein Kind ohne Agent-IR hat
    /// [`AgentBudget::default`] (kein Deckel) — das Verhalten ist dann
    /// unverändert.
    ///
    /// # Argumente
    /// - `child` (`&SessionId`): das admittierte Kind.
    /// - `store` (`&dyn StateStore`): Transkript-Persistenz des Kind-Turns.
    /// - `approvals` (`Option<&ApprovalStore>`): durabler Approval-Ledger oder
    ///   `None`.
    /// - `input` (`TurnInput`): der Turn-Input.
    ///
    /// # Returns
    /// Das Ergebnis von [`Self::run_child_with_budget`].
    ///
    /// # Errors
    /// Wie [`Self::run_child_with_budget`].
    pub async fn run_child_with_declared_budget(
        &self,
        child: &SessionId,
        store: &dyn StateStore,
        approvals: Option<&ApprovalStore>,
        input: TurnInput,
    ) -> Result<ChildRunResult, ChildRunError> {
        let budget = self.child_budget(child).unwrap_or_default();
        tracing::info!(
            child = %child,
            max_tokens = ?budget.max_tokens,
            max_tool_calls = ?budget.max_tool_calls,
            max_wall_time_ms = ?budget.max_wall_time_ms,
            "child_budget.transfer_enforced"
        );
        self.run_child_with_budget(child, store, approvals, input, budget)
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
    /// `max_tokens` zählt nur **neue** Tokens (Teil C):
    /// [`harw_types::TokenUsage::fresh_tokens`], also ungecachte Eingabe plus
    /// Ausgabe, über [`crate::session::AgentSession::total_usage`]. Wie bei
    /// den Tool-Aufrufen ist die Abrechnungseinheit die **gesamte
    /// Kind-Session**. Geprüft wird vor jeder Modellrunde über den
    /// `TurnControl` ([`crate::turn_loop::TurnTokenBudget`]): ab 80 % bekommt
    /// das Kind die Anweisung, jetzt zusammenzufassen; beim Limit endet der
    /// Turn, und diese Methode liefert die letzte Assistant-Antwort als
    /// Teilergebnis (`Ok` mit [`ChildRunResult::budget_exhausted`] = `true`)
    /// statt eines Fehlers.
    ///
    /// Runde 5, Teil J (Modus [`crate::child_handoff::BudgetHandoffMode::Compact`],
    /// Vorgabe): der Turn endet schon bei `max_tokens − Reserve`
    /// ([`crate::child_handoff::handoff_reserve_tokens`]); aus der Reserve
    /// fasst genau **ein** Verdichtungsaufruf den Kind-Verlauf strukturiert
    /// zusammen, und diese markierte Übergabe ist das Ergebnis
    /// ([`ChildRunResult::budget_handoff`] = `Compacted`). Scheitert die
    /// Verdichtung, bleibt es bei der letzten Assistant-Antwort (`LastAnswer`).
    /// Die rohe letzte Antwort liegt in beiden Fällen im Ergebnisarchiv
    /// (`agent.result`); die Tokens der Verdichtung zählen zu
    /// [`ChildRunResult::usage`]. Das Zeitbudget liefert weiterhin einen
    /// Fehler.
    ///
    /// Runde 5, Teil M: bei **jedem** nicht regulären Ende (Zeitbudget,
    /// Werkzeug-Budget, Lease, Abbruch, Turn-/Provider-Fehler) legt der
    /// Spawner zusätzlich einen Endbericht mit Aktivitätsjournal ab
    /// ([`Self::child_end_report`]); bei Zeit- und Werkzeug-Budget versucht er
    /// eine Übergabe-Verdichtung mit eigenem Zeitlimit
    /// ([`crate::child_comms::END_HANDOFF_TIMEOUT`]).
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
    /// `Ok(ChildRunResult)`, wenn der Turn im Budget blieb oder das
    /// Token-Budget ein Teilergebnis erzwang (`budget_exhausted`).
    ///
    /// # Errors
    /// - [`ChildRunError::BudgetExhausted`] mit
    ///   `"budget_exceeded: wall_time (limit=…, used=…)"`, wenn die
    ///   Wanduhrfrist ablief.
    /// - [`ChildRunError::BudgetExhausted`] mit
    ///   `"budget_exceeded: tool_calls (limit=…, used=…)"`, wenn das Kind mehr
    ///   Werkzeugaufrufe verbraucht hat als erlaubt.
    /// - [`ChildRunError::Spawn`]: jeder Fehler aus dem darunterliegenden
    ///   Ausführungspfad (nicht admittiert, Lease abgelaufen, Cancellation,
    ///   Turn-Fehler, Pause-Sperre).
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
    ) -> Result<ChildRunResult, ChildRunError> {
        // Budget-Anrechnung: Ausgangsstand vor dem Lauf. Solange das Kind
        // nicht läuft, liegt seine Session im Manager; fehlt sie, zählt der
        // Lauf ab 0 (der Lauf selbst scheitert dann ohnehin).
        let baseline_tokens = self.child_token_usage(child).unwrap_or(0);
        let baseline_tool_calls = self.child_tool_call_count(child).unwrap_or(0);
        let started = std::time::Instant::now();
        let result = self
            .enforce_child_budget(child, store, approvals, input, budget, started)
            .await;
        if self.child_backend().is_some() {
            // Wave 3C: a backend-run child's local session never changes
            // (its turn history lives with the backend), so the
            // session-manager deltas below would always read as zero.
            // `run.usage` already carries the backend's own accounting
            // (`crate::child_backend::ChildRunOutcome::usage`, mapped in
            // `Self::finish_backend_outcome`) — charge and keep it as-is.
            if let Ok(run) = &result {
                self.charge_run_usage(child, run.usage);
            }
            return result;
        }
        // Auch ein gescheiterter oder über das Budget gelaufener Lauf hat
        // verbraucht — die Anrechnung erfolgt auf jedem Ausgang. Ist die
        // Session inzwischen verworfen, gilt der Ausgangsstand (Differenz 0).
        // Runde 5, Teil J: die Übergabe-Verdichtung läuft außerhalb eines
        // Turns und steht deshalb nicht in `total_usage` der Kind-Session;
        // `enforce_child_budget` meldet ihre neuen Tokens über `run.usage`.
        let handoff_tokens = result.as_ref().map_or(0, |run| run.usage.tokens);
        let usage = ChildUsage {
            tokens: self
                .child_token_usage(child)
                .unwrap_or(baseline_tokens)
                .saturating_sub(baseline_tokens)
                .saturating_add(handoff_tokens),
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
        mut input: TurnInput,
        budget: AgentBudget,
        started: std::time::Instant,
    ) -> Result<ChildRunResult, ChildRunError> {
        // Teil C: das Token-Budget wird je Modellrunde über den
        // `TurnControl` geprüft (Abschluss-Anweisung ab 80 %, Abbruch beim
        // Limit), nicht erst nach dem ganzen Turn.
        // Runde 5, Teil J: im Modus `Compact` endet der Turn schon bei
        // `limit − Reserve`; die Reserve trägt die Übergabe-Verdichtung.
        let handoff_reserve = match (budget.max_tokens, self.budget_handoff_mode) {
            (Some(limit), crate::child_handoff::BudgetHandoffMode::Compact) => {
                crate::child_handoff::handoff_reserve_tokens(limit)
            }
            _ => 0,
        };
        if let Some(limit) = budget.max_tokens {
            let used_before = self.child_token_usage(child).unwrap_or(0);
            input.control =
                input
                    .control
                    .clone()
                    .with_token_budget(crate::turn_loop::TurnTokenBudget::new(
                        limit.saturating_sub(handoff_reserve),
                        used_before,
                    ));
        }
        // Werkzeug-Budget vor dem Dispatch (Ladybird-Export: „25 von 16“):
        // der Turn führt aus einer Antwort höchstens den Rest aus.
        if let Some(limit) = budget.max_tool_calls {
            let used_before = self.child_tool_call_count(child).unwrap_or(0);
            input.control = input
                .control
                .clone()
                .with_tool_call_budget(limit, used_before);
        }
        // Klon mit geteiltem Zähler: Modellrunden und Werkzeugaufrufe dieses
        // Laufs für die Kopfzeile der Übergabe.
        let control = input.control.clone();
        let mut turn = Box::pin(self.run_child_with_approvals(child, store, approvals, input));

        let outcome = match budget.max_wall_time_ms {
            Some(limit_ms) => {
                // Das Ergebnis wird gebunden, damit der `timeout`-Temporary vor
                // den Armen fällt — sonst bliebe `turn` bis zum Ende des
                // `match` ausgeliehen und wäre unten nicht mehr erwartbar.
                //
                // Runde 9, E3: die Wartezeit auf eine Antwort des Elternteils
                // (`parent.message {kind: "question"}`) ruht im Zeitbudget —
                // ein fragendes Kind bleibt am Leben, bis die Frage
                // beantwortet, abgebrochen oder abgelaufen ist.
                let limit = Duration::from_millis(limit_ms);
                let timed = loop {
                    let waited = self.comms.question_wait(child);
                    let remaining = limit
                        .saturating_add(waited)
                        .saturating_sub(started.elapsed())
                        .max(QUESTION_BUDGET_RECHECK);
                    match tokio::time::timeout(remaining, turn.as_mut()).await {
                        Ok(outcome) => break Ok(outcome),
                        Err(elapsed) => {
                            if self.comms.has_pending_question(child)
                                || self.comms.question_wait(child) > waited
                            {
                                continue;
                            }
                            break Err(elapsed);
                        }
                    }
                };
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
                        let used_ms = u64::try_from(
                            started
                                .elapsed()
                                .saturating_sub(self.comms.question_wait(child))
                                .as_millis(),
                        )
                        .unwrap_or(u64::MAX);
                        let error =
                            Self::budget_exhausted(BudgetDimension::WallTime, limit_ms, used_ms);
                        self.set_failed(child, &error.to_string());
                        tracing::warn!(
                            child = %child,
                            dimension = BudgetDimension::WallTime.as_str(),
                            limit = limit_ms,
                            used = used_ms,
                            "child_budget.exceeded",
                        );
                        // Runde 5, Teil M: Journal plus (wenn möglich, mit
                        // eigenem kurzem Zeitlimit) Übergabe-Verdichtung.
                        self.finalize_child_end(
                            child,
                            crate::child_comms::ChildEndCause::WallTime { limit_ms, used_ms },
                        )
                        .await;
                        return Err(error.into());
                    }
                }
            }
            None => turn.await,
        };
        let outcome = outcome.map_err(ChildRunError::Spawn)?;

        if let Some(limit) = budget.max_tool_calls {
            // Nicht ausgeführte (budgetbedingt beantwortete) Aufrufe stehen im
            // Verlauf, zählen aber nicht als verbraucht.
            let skipped = control.tool_budget_skipped();
            let used = self.child_tool_call_count(child)?.saturating_sub(skipped);
            if used > limit || skipped > 0 {
                tracing::warn!(
                    child = %child,
                    dimension = BudgetDimension::ToolCalls.as_str(),
                    limit = limit,
                    used = used,
                    "child_budget.exceeded",
                );
                let error = Self::budget_exhausted(
                    BudgetDimension::ToolCalls,
                    u64::from(limit),
                    u64::from(used),
                );
                self.set_failed(child, &error.to_string());
                // Runde 5, Teil M: Journal plus Übergabe-Verdichtung.
                self.finalize_child_end(
                    child,
                    crate::child_comms::ChildEndCause::ToolBudget {
                        limit: u64::from(limit),
                        used: u64::from(used),
                    },
                )
                .await;
                return Err(error.into());
            }
        }
        if let Some(limit) = budget.max_tokens {
            let used = self.child_token_usage(child)?;
            let stopped_by_budget = matches!(
                outcome.outcome,
                TurnOutcome::Cancelled {
                    reason: CancelReason::Budget
                }
            );
            if stopped_by_budget && used >= limit.saturating_sub(handoff_reserve) {
                // Teil C: kein harter Fehler — die letzte Assistant-Antwort
                // ist das Teilergebnis, ausdrücklich als solches markiert.
                let partial = self.child_last_assistant_text(child);
                tracing::warn!(
                    child = %child,
                    dimension = BudgetDimension::Tokens.as_str(),
                    limit = limit,
                    used = used,
                    reserve = handoff_reserve,
                    partial_bytes = partial.as_deref().map_or(0, str::len),
                    "child_budget.exhausted_partial_result",
                );
                self.set_status(child, ChildStatus::Completed);
                // Runde 5, Teil M: das Token-Budget endet regulär mit
                // Übergabe (Teil J) — kein Endbericht eines Abbruchs.
                self.comms.clear_end(child);
                // Runde 5, Teil H: auch das Teilergebnis ist über
                // `agent.result` ungekürzt abrufbar — auch dann, wenn unten
                // eine Verdichtung das Ergebnis an den Elternteil ersetzt.
                self.archive_completed_child(child, partial.as_deref());
                // Runde 5, Teil J: genau ein Verdichtungsaufruf aus der
                // Reserve; scheitert er, bleibt die letzte Antwort.
                let compacted = if handoff_reserve > 0 {
                    self.compact_budget_handoff(
                        child,
                        handoff_reserve,
                        &control,
                        limit,
                        used,
                        partial.is_some(),
                    )
                    .await
                } else {
                    None
                };
                let (full_text, kind, handoff_tokens) = match compacted {
                    Some((text, tokens)) => (
                        Some(text),
                        crate::child_handoff::BudgetHandoff::Compacted,
                        tokens,
                    ),
                    None => (
                        partial.clone(),
                        crate::child_handoff::BudgetHandoff::LastAnswer,
                        0,
                    ),
                };
                let detail = match kind {
                    crate::child_handoff::BudgetHandoff::Compacted => format!(
                        "Token-Budget erreicht (limit={limit}, used={used}); \
                         Übergabe-Zusammenfassung zurückgegeben"
                    ),
                    crate::child_handoff::BudgetHandoff::LastAnswer => format!(
                        "Token-Budget erschöpft (limit={limit}, used={used}); Teilergebnis \
                         zurückgegeben"
                    ),
                };
                match (kind, partial.as_deref()) {
                    (crate::child_handoff::BudgetHandoff::LastAnswer, Some(text)) => {
                        self.set_outcome_detail(child, text);
                    }
                    _ => self.set_outcome_detail(child, &detail),
                }
                self.record_budget_handoff(child, kind, full_text.as_deref());
                return Ok(ChildRunResult {
                    child: outcome.child,
                    outcome: TurnOutcome::Completed,
                    full_text,
                    // Nur die Verdichtung; `run_child_with_budget` addiert
                    // den Verbrauch des Turns.
                    usage: ChildUsage {
                        tokens: handoff_tokens,
                        ..ChildUsage::default()
                    },
                    budget_exhausted: true,
                    budget_handoff: Some(kind),
                });
            }
            if used > limit {
                // Der Turn schloss regulär ab; die letzte Runde hat das
                // Limit nur überschritten. Das Ergebnis ist vollständig.
                tracing::warn!(
                    child = %child,
                    dimension = BudgetDimension::Tokens.as_str(),
                    limit = limit,
                    used = used,
                    "child_budget.exceeded_after_completion",
                );
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
        // Wave 3C: a wired `ChildBackend` runs this child somewhere other
        // than in-process (`crate::child_backend`); everything below this
        // branch is the in-process path and stays untouched when no
        // backend is wired (the default inside `harw`).
        if let Some(backend) = self.child_backend() {
            return self
                .run_child_via_backend(child, &record, backend, input)
                .await;
        }
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
            // Runde 5, Teil M: auch hier bleibt das Journal abrufbar.
            if let Some(cause) = Self::cancel_cause(token.reason()) {
                self.finalize_child_end(child, cause).await;
            }
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
            Ok(mut manager) => {
                let session = manager.remove(child);
                // Runde 9, E7: unter demselben Lock die Eltern-Sicht ablegen,
                // damit Kinder dieses Kindes während seines Turns admittiert
                // werden können (siehe `CheckedOutParent`).
                if let Some(session) = &session {
                    self.check_out(child, session);
                }
                session
            }
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
        // Teil C: derselbe Auftragsdeckel gilt für einen direkt übergebenen
        // User-Text (z. B. Fan-out-Fragen), nicht nur für `pending_task`.
        if let Some(max_bytes) = self.child_task_max_bytes(child)
            && let Some(text) = input.user_text.as_deref()
            && text.len() > max_bytes
        {
            tracing::warn!(
                child = %child,
                original_bytes = text.len(),
                max_bytes,
                "child_run.task_truncated"
            );
            input.user_text = Some(cap_task_text(text, max_bytes));
        }
        // Klon mit geteiltem Zähler: nach dem Turn liefert er den konkreten
        // Grund eines `CancelReason::Budget`-Endes (`stop_detail`).
        let turn_control = input.control.clone();

        let turn = {
            let session = running.session_mut()?;
            // Lease-Herzschlag, solange der Lauf lebt, und jede Approval-Pause
            // über den zentralen Kind-Approval-Pfad. Ist kein Responder
            // erreichbar, wird die konkrete Operation abgelehnt und der Agent
            // läuft weiter; ein fehlendes Surface darf keinen Job blockieren.
            crate::child_lease_heartbeat::with_lease_heartbeat(self, child, async {
                tokio::select! {
                    biased;
                    () = token.cancelled() => Err(Self::cancelled_error(child, token.reason())),
                    outcome = async {
                        let first = match approvals {
                            Some(approvals) => run_turn_durable(session, model.as_ref(), store, approvals, input).await,
                            None => run_turn(session, model.as_ref(), store, input).await,
                        };
                        match first {
                            Ok(outcome) => {
                                crate::child_approval::relay_child_approvals(
                                    self, child, session, model.as_ref(), store, approvals, outcome,
                                )
                                .await
                            }
                            other => other,
                        }
                        .map_err(|error| Self::reject(error.to_string()))
                    } => outcome,
                }
            })
            .await
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
            // Runde 5, Teil M: Journal statt nackter Meldung (ohne
            // Verdichtung — die Sitzung ist verworfen).
            self.finalize_child_end(child, crate::child_comms::ChildEndCause::LeaseExpired)
                .await;
            return Err(Self::reject(format!(
                "child {child} completed after lease expiry at {}; result discarded",
                expired.expired_at
            )));
        }
        if returned == SessionReturn::Discarded {
            self.finalize_child_end(child, crate::child_comms::ChildEndCause::Released)
                .await;
            return Err(Self::reject(format!(
                "child {child} was released while its turn was running; result discarded"
            )));
        }
        let outcome = match turn {
            Ok(outcome) => outcome,
            Err(error) => {
                // Nur echter Abschluss ist `Completed`: Abbruch und Fehler
                // werden unterschieden und nie als Erfolg verbucht.
                // Runde 5, Teil M: in beiden Fällen ein Endbericht mit
                // Journal; ein Budget-Abbruch (Zeitbudget) wird vom
                // Budget-Pfad (`enforce_child_budget`) berichtet. Runde 7,
                // Teil A3: ein Provider-/Turn-Fehler wird mit eigenem
                // Zeitlimit verdichtet (sonst Journal + letzter Text).
                let cause = if token.is_cancelled() {
                    self.set_status(child, ChildStatus::Cancelled);
                    Self::cancel_cause(token.reason())
                } else {
                    self.set_failed(child, &error.message);
                    Some(crate::child_comms::ChildEndCause::TurnError(
                        error.message.clone(),
                    ))
                };
                if let Some(cause) = cause {
                    self.finalize_child_end(child, cause).await;
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
                // Runde 5, Teil M.
                self.finalize_child_end(
                    child,
                    crate::child_comms::ChildEndCause::PauseForbidden(reason.clone()),
                )
                .await;
                Err(Self::reject(reason))
            }
            Some(_) => {
                self.set_status(child, ChildStatus::Paused);
                Ok(ChildRunResult {
                    child: child.clone(),
                    outcome,
                    full_text: None,
                    usage: ChildUsage::default(),
                    budget_exhausted: false,
                    budget_handoff: None,
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
                // Runde 5, Teil M: letzter Text ins Journal; ein terminales,
                // nicht erfolgreiches Ergebnis bekommt einen Endbericht.
                if let Some(text) = full_text.as_deref() {
                    self.comms.set_last_assistant(child, text);
                }
                if let Some(cause) = Self::outcome_end_cause(&outcome, turn_control.stop_detail()) {
                    self.finalize_child_end(child, cause).await;
                }
                self.set_status(child, ChildStatus::Completed);
                // Runde 5, Teil H: den ungekürzten Text für `agent.result`
                // aufheben, bevor der Aufrufer das Kind freigibt.
                self.archive_completed_child(child, full_text.as_deref());
                // Runde 9, E3: auch ein regulär beendetes Kind lässt sich
                // fortsetzen (`agent.message` an ein eigenes, beendetes Kind).
                if matches!(outcome, TurnOutcome::Completed) {
                    self.record_completed_continuation(child, full_text.as_deref());
                }
                Ok(ChildRunResult {
                    child: child.clone(),
                    outcome,
                    full_text,
                    usage: ChildUsage::default(),
                    budget_exhausted: false,
                    budget_handoff: None,
                })
            }
        }
    }

    /// Wave 3C: runs `child` through the wired
    /// [`crate::child_backend::ChildBackend`] instead of the in-process turn
    /// loop above. Called from [`Self::run_child_with_approvals`] once
    /// [`Self::child_backend`] is `Some`; mirrors that function's own
    /// admission bookkeeping (running status, pending-task consumption, task
    /// truncation) so a caller sees the same [`ChildRunResult`] shape
    /// whichever path ran the child.
    ///
    /// # Beschreibung
    /// The child's local [`AgentSession`] stays parked in the manager —
    /// unlike the in-process path it is never checked out — because a
    /// backend-run child keeps its own turn history elsewhere; leaving the
    /// session in place is what keeps sandbox/continuation bookkeeping
    /// ([`Self::child_sandbox`], [`Self::finalize_child_end`]) working
    /// unchanged. Cancellation is the backend's own job (see
    /// [`crate::child_backend::ChildRunSpec::cancel`]'s docs): this method
    /// only derives the token and hands it over.
    ///
    /// # Errors
    /// Never returns an error from the backend itself — every
    /// [`crate::child_backend::ChildRunStatus`] maps to either an
    /// `Ok(ChildRunResult)` (`Completed`, `BudgetExhausted`) or an
    /// [`AgentSpawnError`] carrying the finalized end cause (see
    /// [`Self::finish_backend_outcome`]). Only admission bookkeeping ahead
    /// of the run (child already running, already cancelled) can fail.
    async fn run_child_via_backend(
        &self,
        child: &SessionId,
        record: &ChildRecord,
        backend: Arc<dyn crate::child_backend::ChildBackend>,
        mut input: TurnInput,
    ) -> Result<ChildRunResult, AgentSpawnError> {
        let token = self
            .child_cancel_token(child)
            .ok_or_else(|| Self::reject(format!("child {child} has no cancellation token")))?;
        // Cancel ist terminal (F-182), wie im in-process Pfad oben.
        if token.is_cancelled() {
            self.set_status(child, ChildStatus::Cancelled);
            if let Some(cause) = Self::cancel_cause(token.reason()) {
                self.finalize_child_end(child, cause).await;
            }
            return Err(Self::cancelled_error(child, token.reason()));
        }
        self.mark_running(child)?;
        if let (Some(root), Some(snapshot)) = (self.root_for(child), self.child_record(child)) {
            self.observe_orchestration(root, &snapshot, None, AgentOrchestrationStatus::Running);
        }
        // Derselbe Auftragsdeckel wie im in-process Pfad (siehe dort).
        let pending_task = self.take_pending_task(child);
        let has_user_text = input
            .user_text
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty());
        if !has_user_text && let Some(task) = pending_task {
            input.user_text = Some(task);
        }
        if let Some(max_bytes) = self.child_task_max_bytes(child)
            && let Some(text) = input.user_text.as_deref()
            && text.len() > max_bytes
        {
            input.user_text = Some(cap_task_text(text, max_bytes));
        }
        let task = input.user_text.clone().unwrap_or_default();
        let context = (!input.metadata.is_null()).then(|| input.metadata.clone());
        let continue_from = self.take_backend_resume(child);
        let plan_mode = match self.manager.lock() {
            Ok(manager) => self.lineage_in_plan_mode(&manager, child),
            Err(_) => false,
        };
        let spec = crate::child_backend::ChildRunSpec {
            child: child.clone(),
            parent: record.parent.clone(),
            agent_id: record.role.clone(),
            task,
            context,
            continue_from,
            // Die aktuellen effektiven Rechte des Kindes, wie sie der
            // in-process Pfad hätte (Werkzeugfläche nach dem Eltern-Schnitt,
            // Sandbox, Freigabemodus; siehe `Self::backend_rights`). Das
            // Backend reicht sie unverändert weiter (`JobChildBackend` in
            // `harw-agent-runner` verengt nicht selbst); das Kind schneidet
            // sie nur noch mit seinem Manifest. Leere Rechte hießen: ein
            // job-gestartetes Kind darf gar nichts.
            rights: self.backend_rights(child, &record.parent),
            budget: self.remaining_budget(child).map(agent_budget_to_dsl_budget),
            live_mode: !plan_mode,
            cancel: token.child(),
        };
        let io = ControllerChildIo {
            spawner: self,
            child: child.clone(),
            role: record.role.clone(),
        };
        let outcome = backend.run(spec, &io).await;
        self.finish_backend_outcome(child, outcome).await
    }

    /// Die Rechte, mit denen ein über den [`crate::child_backend::ChildBackend`]
    /// laufendes Kind startet: genau seine aktuelle in-process Fläche.
    ///
    /// # Beschreibung
    /// - `tools`: die Werkzeuge, die [`crate::turn_loop::collect_tools`] dem
    ///   Kind jetzt zeigen würde (Registry ∩ Aktivierung nach Agent-IR,
    ///   Eltern-Schnitt und Modus), zusätzlich geschnitten mit der
    ///   Eltern-Aktivierung, gegen die die Admission das Kind geschnitten hat
    ///   (Basis bei Live-Modus, sonst die aktuelle; siehe
    ///   [`Self::parent_rights_ceiling`]).
    /// - `write`/`shell`/`network_open`: die Rechte der aktuellen
    ///   Kind-Sandbox (`WriteWorkspace`, `ExecuteProcess`, `NetworkAccess`
    ///   aus [`harw_authority::Permission`]), die auch die Eltern-Sandbox
    ///   hält.
    /// - `network_hosts`: die Hosts des Kind-Netz-Scopes, die der Eltern-Scope
    ///   zulässt. `PublicDns`- und CIDR-Ziele haben in
    ///   [`crate::child_backend::ChildBackendRights`] keine Entsprechung und
    ///   fallen weg (fail-closed).
    /// - `host`: ein Host-Werkzeug (`host.*`,
    ///   [`harw_agent_dsl::classify::LabelClassifier`]) ist in `tools`.
    /// - `full_access`: die Zelle aus [`Self::with_approval_mode`] liest
    ///   [`harw_extension_api::ApprovalMode::FullAccess`].
    ///
    /// Fail-closed: ist das Kind, seine Sandbox oder der Elternteil nicht
    /// auffindbar oder ein Lock vergiftet, sind die Rechte leer.
    ///
    /// # Concurrency
    /// Nimmt `manager`, darunter kurz `checked_out` und `active` (dieselbe
    /// Reihenfolge wie die Admission).
    fn backend_rights(
        &self,
        child: &SessionId,
        parent: &SessionId,
    ) -> crate::child_backend::ChildBackendRights {
        use crate::child_backend::ChildBackendRights;
        use harw_agent_dsl::classify::{LabelClassifier, ToolClassifier};
        use harw_authority::Permission;

        let Ok(manager) = self.manager.lock() else {
            return ChildBackendRights::default();
        };
        let Ok(session) = manager.get(child) else {
            return ChildBackendRights::default();
        };
        let Some(child_sandbox) = session
            .spawn_context()
            .map(|context| context.sandbox.clone())
        else {
            return ChildBackendRights::default();
        };
        let Ok(specs) = crate::turn_loop::collect_tools(session) else {
            return ChildBackendRights::default();
        };
        let Some((parent_activation, parent_sandbox)) =
            self.parent_rights_ceiling(&manager, parent)
        else {
            return ChildBackendRights::default();
        };
        drop(manager);

        let tools: BTreeSet<String> = specs
            .iter()
            .map(|spec| spec.name().to_owned())
            .filter(|name| {
                parent_activation.is_tool_enabled(&harw_tools::ToolName::new(name.as_str()))
            })
            .collect();
        let granted = |permission: Permission| {
            child_sandbox.permissions().contains(permission)
                && parent_sandbox.permissions().contains(permission)
        };
        let network_hosts: BTreeSet<String> = child_sandbox
            .network_scope()
            .hosts()
            .filter(|host| parent_sandbox.network_scope().allows(host))
            .map(ToOwned::to_owned)
            .collect();
        let host = tools.iter().any(|tool| {
            LabelClassifier
                .classify(tool)
                .is_some_and(|classes| classes.host)
        });
        let full_access = self.approval_mode.as_ref().is_some_and(|mode| {
            mode.get() == harw_extension_api::approval_mode::ApprovalMode::FullAccess
        });
        ChildBackendRights {
            network_open: granted(Permission::NetworkAccess),
            write: granted(Permission::WriteWorkspace),
            shell: granted(Permission::ExecuteProcess),
            host,
            full_access,
            network_hosts,
            tools,
        }
    }

    /// Aktivierung und Sandbox von `parent`, gegen die die Admission ein Kind
    /// schneidet: bei veröffentlichtem Live-Modus die Basis (ohne
    /// Modus-Schnitt), sonst der aktuelle Stand — wie in [`Self::admit`].
    /// Deckt Manager-Sitzungen, Elternteile mit laufendem Turn
    /// ([`CheckedOutParent`]) und die externe Wurzel ab; `None` für einen
    /// unbekannten Elternteil oder einen ohne Sandbox-Kontext.
    fn parent_rights_ceiling(
        &self,
        manager: &SessionManager,
        parent: &SessionId,
    ) -> Option<(SessionActivation, SandboxSpec)> {
        let follows_live_mode = self.live_mode.current().is_some();
        if let Ok(session) = manager.get(parent) {
            let sandbox = session.spawn_context().map(|context| &context.sandbox)?;
            return Some(if follows_live_mode {
                (
                    session.base_activation().clone(),
                    session.base_sandbox().unwrap_or(sandbox).clone(),
                )
            } else {
                (session.activation().clone(), sandbox.clone())
            });
        }
        if let Some(view) = self.checked_out_parent(parent) {
            let sandbox = view.spawn_context.map(|context| context.sandbox)?;
            return Some(if follows_live_mode {
                (view.base_activation, view.base_sandbox.unwrap_or(sandbox))
            } else {
                (view.activation, sandbox)
            });
        }
        self.external_root_parent
            .as_ref()
            .filter(|root| &root.session_id == parent)
            .map(|root| (root.activation.clone(), root.spawn_context.sandbox.clone()))
    }

    /// Wave 3C: maps a finished [`crate::child_backend::ChildRunOutcome`]
    /// onto the existing [`ChildRunResult`], finalizing a non-regular end
    /// through [`Self::finalize_child_end`] exactly like the in-process
    /// path. Never panics: every
    /// [`crate::child_backend::ChildRunStatus`] variant — including
    /// [`crate::child_backend::ChildRunStatus::Crashed`] — becomes a
    /// `ChildEnd` with a cause instead.
    async fn finish_backend_outcome(
        &self,
        child: &SessionId,
        outcome: crate::child_backend::ChildRunOutcome,
    ) -> Result<ChildRunResult, AgentSpawnError> {
        use crate::child_backend::ChildRunStatus;
        let crate::child_backend::ChildRunOutcome {
            status,
            text,
            usage,
            continuation,
        } = outcome;
        if let Some(token) = continuation {
            self.set_backend_resume(child, token);
        }
        match status {
            ChildRunStatus::Completed => {
                if let Some(text) = text.as_deref() {
                    self.set_outcome_detail(child, text);
                    self.comms.set_last_assistant(child, text);
                }
                self.set_status(child, ChildStatus::Completed);
                self.archive_completed_child(child, text.as_deref());
                self.record_completed_continuation(child, text.as_deref());
                Ok(ChildRunResult {
                    child: child.clone(),
                    outcome: TurnOutcome::Completed,
                    full_text: text,
                    usage,
                    budget_exhausted: false,
                    budget_handoff: None,
                })
            }
            ChildRunStatus::BudgetExhausted => {
                if let Some(text) = text.as_deref() {
                    self.set_outcome_detail(child, text);
                    self.comms.set_last_assistant(child, text);
                }
                self.set_status(child, ChildStatus::Completed);
                self.comms.clear_end(child);
                self.archive_completed_child(child, text.as_deref());
                self.record_budget_handoff(
                    child,
                    crate::child_handoff::BudgetHandoff::LastAnswer,
                    text.as_deref(),
                );
                Ok(ChildRunResult {
                    child: child.clone(),
                    outcome: TurnOutcome::Completed,
                    full_text: text,
                    usage,
                    budget_exhausted: true,
                    budget_handoff: Some(crate::child_handoff::BudgetHandoff::LastAnswer),
                })
            }
            other => {
                let cause = other.to_child_end_cause();
                let message = match &cause {
                    Some(crate::child_comms::ChildEndCause::Cancelled { reason }) => {
                        self.set_status(child, ChildStatus::Cancelled);
                        format!(
                            "child {child} was cancelled before its turn completed (reason: {reason})"
                        )
                    }
                    Some(crate::child_comms::ChildEndCause::TurnError(reason)) => {
                        self.set_failed(child, reason);
                        reason.clone()
                    }
                    _ => {
                        let reason = "child backend run ended unexpectedly".to_owned();
                        self.set_failed(child, &reason);
                        reason
                    }
                };
                if let Some(cause) = cause {
                    self.finalize_child_end(child, cause).await;
                }
                Err(Self::reject(message))
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
            .map_err(AgentSpawnError::from)
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
        // Teil C: nur neue, ungecachte Eingabe plus Ausgabe — ein erneut
        // gesendeter, gecachter Verlauf zählt nicht jede Runde erneut.
        Ok(session.total_usage().fresh_tokens())
    }

    /// Die letzte nicht leere Assistant-Antwort eines Kindes, ungekürzt
    /// (Teilergebnis nach erschöpftem Token-Budget, Teil C). `None`, wenn es
    /// keine gibt oder die Session nicht verfügbar ist.
    fn child_last_assistant_text(&self, child: &SessionId) -> Option<String> {
        let manager = self.manager.lock().ok()?;
        let session = manager.get(child).ok()?;
        session.history().items().iter().rev().find_map(|item| {
            let TurnItem::AssistantMessage(message) = item else {
                return None;
            };
            let text: String = message
                .content
                .iter()
                .filter_map(|part| match part {
                    ContentPart::Text { text } => Some(text.as_str()),
                    ContentPart::ImageUrl { .. } | ContentPart::Media { .. } => None,
                })
                .collect();
            (!text.trim().is_empty()).then_some(text)
        })
    }

    /// Runde 5, Teil J: die Übergabe-Verdichtung am Budget-Ende eines Kindes.
    ///
    /// # Beschreibung
    /// Genau **ein** Aufruf über das Modell des Kindes
    /// ([`ChildRegistryFactory::model_for_task`], Modell-/Provider-ID der
    /// Kind-Session): Verlaufsauszug gekappt auf die Reserve, Ausgabe
    /// gedeckelt, Zeitlimit [`crate::child_handoff::HANDOFF_TIMEOUT`], und
    /// abbrechbar über den Cancel-Token des Kindes. Die Nutzung erscheint
    /// als `InternalUsage { purpose: "budget_handoff" }` auf dem Bus des
    /// Kindes.
    ///
    /// # Returns
    /// `Some((markierter Übergabe-Text, neue Tokens des Aufrufs))`, sonst
    /// `None` (Fehler, Zeitlimit, Abbruch, leere Antwort, fehlende
    /// Session/Rolle) — der Aufrufer liefert dann die letzte Antwort.
    ///
    /// # Concurrency
    /// Hält keinen Lock über ein `.await`.
    async fn compact_budget_handoff(
        &self,
        child: &SessionId,
        reserve: u64,
        control: &crate::turn_loop::TurnControl,
        limit: u64,
        used: u64,
        raw_available: bool,
    ) -> Option<(String, u64)> {
        use crate::child_handoff::{
            HANDOFF_TIMEOUT, HandoffCall, HandoffFacts, HandoffFailure, build_handoff_prompt,
            format_handoff_text, plan_handoff, run_handoff_compaction,
        };
        let record = self.child_record(child)?;
        let model = match self.roles.get(&record.role) {
            Some(definition) => match definition
                .registry_factory
                .model_for_task(&record.role, record.task_complexity)
            {
                Ok(model) => model,
                Err(error) => {
                    tracing::warn!(child = %child, error = %error, "child_budget.handoff_model_unavailable");
                    return None;
                }
            },
            None => {
                tracing::warn!(child = %child, "child_budget.handoff_role_unavailable");
                return None;
            }
        };
        let (prompt, call) = {
            let manager = self.manager.lock().ok()?;
            let session = manager.get(child).ok()?;
            let window = session
                .auto_compact()
                .map(crate::auto_compact::AutoCompactPolicy::context_window_tokens);
            let plan = plan_handoff(
                reserve,
                window,
                session.token_calibration().bytes_per_token(),
            );
            let call = HandoffCall {
                model_id: session.active_model().cloned(),
                provider_id: session.active_provider().cloned(),
                max_output_tokens: plan.max_output_tokens,
                timeout: HANDOFF_TIMEOUT,
            };
            (build_handoff_prompt(session.history().items(), &plan), call)
        };
        let Some(prompt) = prompt else {
            tracing::info!(child = %child, "child_budget.handoff_nothing_to_compact");
            return None;
        };
        let compaction = run_handoff_compaction(model.as_ref(), prompt, &call);
        let result = match self.child_cancel_token(child) {
            Some(token) => tokio::select! {
                biased;
                () = token.cancelled() => Err(HandoffFailure::Cancelled),
                result = compaction => result,
            },
            None => compaction.await,
        };
        let summary = match result {
            Ok(summary) => summary,
            Err(failure) => {
                tracing::warn!(
                    child = %child,
                    failure = %failure,
                    "child_budget.handoff_failed_falling_back_to_last_answer",
                );
                return None;
            }
        };
        if let Ok(manager) = self.manager.lock()
            && let Ok(session) = manager.get(child)
        {
            session.publish_agent_event(crate::agent_events::AgentEventKind::InternalUsage {
                purpose: "budget_handoff".to_owned(),
                usage: summary.usage.clone(),
            });
        }
        let origin = self.continuation_origin(child);
        let continuations_left = self
            .handoff_ledger
            .lock()
            .map_or(0, |ledger| ledger.continuations_left(&origin));
        let text = format_handoff_text(
            &summary.text,
            &HandoffFacts {
                child,
                role: &record.role,
                model_rounds: control.model_rounds(),
                tool_calls: control.tool_calls(),
                limit,
                used,
                raw_available,
                continuations_left,
            },
        );
        tracing::info!(
            child = %child,
            handoff_bytes = text.len(),
            handoff_tokens = summary.usage.fresh_tokens(),
            truncated = summary.truncated,
            "child_budget.handoff_compacted",
        );
        Some((text, summary.usage.fresh_tokens()))
    }

    /// Runde 5, Teil M: die Endursache eines abgebrochenen Kind-Tokens.
    ///
    /// # Returns
    /// `None` für [`CancelReason::Budget`] — das Zeitbudget berichtet
    /// [`Self::enforce_child_budget`] selbst (mit Grenze und Verbrauch).
    fn cancel_cause(reason: Option<CancelReason>) -> Option<crate::child_comms::ChildEndCause> {
        use crate::child_comms::ChildEndCause;
        match reason {
            Some(CancelReason::Budget) => None,
            Some(CancelReason::LeaseLost) => Some(ChildEndCause::LeaseExpired),
            Some(CancelReason::User) => Some(ChildEndCause::Cancelled {
                reason: "Abbruch durch Nutzerin oder Elternteil".to_owned(),
            }),
            Some(CancelReason::Parent) => Some(ChildEndCause::Cancelled {
                reason: "der Elternteil wurde abgebrochen".to_owned(),
            }),
            Some(CancelReason::Shutdown) => Some(ChildEndCause::Cancelled {
                reason: "Harness fährt herunter".to_owned(),
            }),
            None => Some(ChildEndCause::Cancelled {
                reason: "ohne Grund".to_owned(),
            }),
        }
    }

    /// Runde 5, Teil M: die Endursache eines terminalen, nicht
    /// erfolgreichen Turn-Ergebnisses (`None` für `Completed` und Pausen).
    ///
    /// `stop_detail` ist der vom Turn vermerkte konkrete Grund eines
    /// `CancelReason::Budget`-Endes ([`crate::turn_loop::TurnControl::stop_detail`]:
    /// welche Grenze mit Wert und Verbrauch bzw. welcher Turn-Wächter).
    fn outcome_end_cause(
        outcome: &TurnOutcome,
        stop_detail: Option<String>,
    ) -> Option<crate::child_comms::ChildEndCause> {
        use crate::child_comms::ChildEndCause;
        match outcome {
            TurnOutcome::Failed { reason } => {
                Some(ChildEndCause::Outcome(format!("gescheitert: {reason}")))
            }
            TurnOutcome::Truncated => Some(ChildEndCause::Outcome(
                "Modellausgabe abgeschnitten (max_tokens/Kontextfenster)".to_owned(),
            )),
            TurnOutcome::Refused { detail } => Some(ChildEndCause::Outcome(match detail {
                Some(detail) => format!("Antwort abgelehnt: {detail}"),
                None => "Antwort abgelehnt".to_owned(),
            })),
            // Token-Budget (Teil C/J, dann verwirft `enforce_child_budget`
            // den Bericht wieder), eine Turn-Grenze oder ein Turn-Wächter —
            // der Turn vermerkt, welche(r) (`stop_detail`).
            TurnOutcome::Cancelled {
                reason: CancelReason::Budget,
            } => Some(ChildEndCause::TurnStopped(stop_detail.unwrap_or_else(
                || "Turn-Grenze erreicht (ohne vermerkten Grund)".to_owned(),
            ))),
            TurnOutcome::Cancelled { reason } => Self::cancel_cause(Some(*reason)),
            TurnOutcome::Completed
            | TurnOutcome::AwaitingApproval { .. }
            | TurnOutcome::AwaitingChild { .. } => None,
        }
    }

    /// Runde 5, Teil M: legt den Endbericht eines nicht regulär beendeten
    /// Kindes ab.
    ///
    /// # Beschreibung
    /// Immer, deterministisch und ohne Modell: Aktivitätsjournal (samt
    /// letztem Assistententext aus dem Verlauf, falls die Sitzung wieder im
    /// Manager liegt) und Grund. Zusätzlich, nur bei Budget-Enden
    /// ([`crate::child_comms::ChildEndCause::allows_compaction`]), genau ein
    /// Verdichtungsaufruf aus `child_handoff` mit eigenem Zeitlimit
    /// [`crate::child_comms::END_HANDOFF_TIMEOUT`]. Außer bei einem gewollten
    /// Abbruch wird die Übergabe (bzw. das Journal) im Fortsetzungs-Buch
    /// abgelegt, damit der Elternteil mit `continue_from` weitermachen kann.
    ///
    /// # Returns
    /// Den Bericht (`None` ohne Journal).
    ///
    /// # Concurrency
    /// Hält keinen Lock über ein `.await`.
    async fn finalize_child_end(
        &self,
        child: &SessionId,
        cause: crate::child_comms::ChildEndCause,
    ) -> Option<crate::child_comms::ChildEndReport> {
        use crate::child_comms::ChildEndCause;
        if let Some(text) = self.child_last_assistant_text(child) {
            self.comms.set_last_assistant(child, &text);
        }
        // Ohne einen einzigen Arbeitsschritt gibt es nichts zu verdichten.
        let has_work = self
            .comms
            .journal(child)
            .is_some_and(|journal| journal.steps() > 0 || journal.last_assistant().is_some());
        let (handoff, note) = if cause.allows_compaction() && !has_work {
            (None, Some("noch keine Arbeitsschritte".to_owned()))
        } else if cause.allows_compaction() {
            match self.compact_end_handoff(child, &cause).await {
                Ok(text) => (Some(text), None),
                Err(note) => (None, Some(note)),
            }
        } else {
            let note = match &cause {
                ChildEndCause::Cancelled { .. } | ChildEndCause::Released => "abgebrochen",
                ChildEndCause::LeaseExpired => "die Sitzung ist nach dem Lease-Ablauf verworfen",
                cause if cause.is_rate_limited() => {
                    "Provider-Rate-Limit (HTTP 429) — eine Verdichtung liefe in dasselbe Limit"
                }
                _ => "bei diesem Ende nicht vorgesehen",
            };
            (None, Some(note.to_owned()))
        };
        let handoff_available = handoff.is_some();
        let report = self.comms.finalize_end(child, &cause, handoff, note)?;
        tracing::warn!(
            child = %child,
            status = report.status.as_str(),
            reason = %report.reason,
            handoff = handoff_available,
            steps = report.steps,
            files = report.files.len(),
            "child_end.report"
        );
        if cause.allows_continuation()
            && let Some(sandbox) = self.child_sandbox(child)
        {
            // Runde 9, E2: Endgrund, geänderte Dateien und Verdichtung bzw.
            // Journal — die Fortsetzung beginnt nicht von vorn.
            let text = report.continuation_text();
            let entry = crate::child_handoff::HandoffRecord {
                child: child.clone(),
                parent: report.parent.clone(),
                role: report.role.clone(),
                sandbox: Some(sandbox),
                handoff: text,
                kind: if handoff_available {
                    crate::child_handoff::BudgetHandoff::Compacted
                } else {
                    crate::child_handoff::BudgetHandoff::LastAnswer
                },
                origin: self.continuation_origin(child),
                end: cause.predecessor_end(),
            };
            match self.handoff_ledger.lock() {
                Ok(mut ledger) => {
                    ledger.record(entry);
                    return self.comms.mark_continuation(child);
                }
                Err(_) => tracing::warn!(child = %child, "child_handoff_ledger.lock_poisoned"),
            }
        }
        Some(report)
    }

    /// Runde 5, Teil M: Übergabe-Verdichtung nach einem Budget-Ende
    /// (Zeit, Werkzeuge) — dieselben Bausteine wie
    /// [`Self::compact_budget_handoff`], aber mit eigenem, kurzem Zeitlimit
    /// und **ohne** den (bereits abgebrochenen) Cancel-Token des Kindes.
    ///
    /// # Errors
    /// Der Grund, warum es keine Verdichtung gibt (für den Bericht).
    async fn compact_end_handoff(
        &self,
        child: &SessionId,
        cause: &crate::child_comms::ChildEndCause,
    ) -> Result<String, String> {
        use crate::child_comms::{END_HANDOFF_RESERVE_TOKENS, END_HANDOFF_TIMEOUT};
        use crate::child_handoff::{
            HANDOFF_MIN_RESERVE_TOKENS, HandoffCall, HandoffFailure, build_handoff_prompt,
            handoff_reserve_tokens, plan_handoff, run_handoff_compaction,
        };
        let record = self
            .child_record(child)
            .ok_or_else(|| "kein Admission-Record mehr".to_owned())?;
        let model = self
            .roles
            .get(&record.role)
            .ok_or_else(|| "Rolle nicht mehr registriert".to_owned())?
            .registry_factory
            .model_for_task(&record.role, record.task_complexity)
            .map_err(|error| format!("Modell nicht verfügbar: {error}"))?;
        let reserve = record
            .budget
            .max_tokens
            .map_or(END_HANDOFF_RESERVE_TOKENS, handoff_reserve_tokens)
            .max(HANDOFF_MIN_RESERVE_TOKENS);
        let (prompt, call) = {
            let manager = self
                .manager
                .lock()
                .map_err(|_| "Session-Manager gesperrt".to_owned())?;
            let session = manager
                .get(child)
                .map_err(|_| "Kind-Sitzung nicht verfügbar".to_owned())?;
            let window = session
                .auto_compact()
                .map(crate::auto_compact::AutoCompactPolicy::context_window_tokens);
            let plan = plan_handoff(
                reserve,
                window,
                session.token_calibration().bytes_per_token(),
            );
            let call = HandoffCall {
                model_id: session.active_model().cloned(),
                provider_id: session.active_provider().cloned(),
                max_output_tokens: plan.max_output_tokens,
                timeout: END_HANDOFF_TIMEOUT,
            };
            (build_handoff_prompt(session.history().items(), &plan), call)
        };
        let prompt = prompt.ok_or_else(|| "kein verwertbarer Verlauf".to_owned())?;
        let summary = run_handoff_compaction(model.as_ref(), prompt, &call)
            .await
            .map_err(|failure| match failure {
                HandoffFailure::Timeout => format!(
                    "Verdichtung nach {} s abgebrochen",
                    END_HANDOFF_TIMEOUT.as_secs()
                ),
                other => format!("Verdichtung gescheitert: {other}"),
            })?;
        if let Ok(manager) = self.manager.lock()
            && let Ok(session) = manager.get(child)
        {
            session.publish_agent_event(crate::agent_events::AgentEventKind::InternalUsage {
                purpose: "end_handoff".to_owned(),
                usage: summary.usage.clone(),
            });
        }
        // Die Verdichtung zählt zum Verbrauch des Kindes.
        self.charge_run_usage(
            child,
            ChildUsage {
                tokens: summary.usage.fresh_tokens(),
                ..ChildUsage::default()
            },
        );
        tracing::info!(
            child = %child,
            handoff_bytes = summary.text.len(),
            truncated = summary.truncated,
            "child_end.handoff_compacted"
        );
        Ok(format!(
            "[handoff: compacted, Ende: {}] Übergabe-Zusammenfassung nach nicht regulärem Ende \
             ({}).\n\n{}",
            cause.status().as_str(),
            cause.reason_de(),
            summary.text
        ))
    }

    /// Runde 5, Teil J: das ursprüngliche Kind der Fortsetzungskette von
    /// `child` (das Kind selbst, wenn es keine Fortsetzung ist).
    fn continuation_origin(&self, child: &SessionId) -> SessionId {
        self.child_task_state(child)
            .and_then(|state| state.continuation)
            .map_or_else(|| child.clone(), |link| link.origin)
    }

    /// Die Sandbox einer Kind-Session aus ihrem vertrauenswürdigen
    /// Spawn-Kontext (`None`, wenn nicht verfügbar).
    fn child_sandbox(&self, child: &SessionId) -> Option<SandboxSpec> {
        let manager = self.manager.lock().ok()?;
        let session = manager.get(child).ok()?;
        session
            .spawn_context()
            .map(|context| context.sandbox.clone())
    }

    /// Runde 5, Teil J: legt die Budget-Übergabe eines Kindes im
    /// Fortsetzungs-Buch ab (Elternteil, Rolle, Sandbox, Kette).
    /// Best-effort: ohne Admission-Record oder bei vergifteter Sperre wird
    /// nur protokolliert.
    fn record_budget_handoff(
        &self,
        child: &SessionId,
        kind: crate::child_handoff::BudgetHandoff,
        text: Option<&str>,
    ) {
        let Some(record) = self.child_record(child) else {
            return;
        };
        let entry = crate::child_handoff::HandoffRecord {
            child: child.clone(),
            parent: record.parent.clone(),
            role: record.role.clone(),
            sandbox: self.child_sandbox(child),
            handoff: text.map_or_else(
                || "(Das Kind hat vor dem Budget-Ende keine Antwort geliefert.)".to_owned(),
                ToOwned::to_owned,
            ),
            kind,
            origin: self.continuation_origin(child),
            end: crate::child_handoff::PredecessorEnd::BudgetExhausted,
        };
        match self.handoff_ledger.lock() {
            Ok(mut ledger) => ledger.record(entry),
            Err(_) => tracing::warn!(child = %child, "child_handoff_ledger.lock_poisoned"),
        }
    }

    /// Runde 9, E3: legt ein regulär beendetes Kind im Fortsetzungs-Buch ab
    /// (Übergabe: letzte Antwort, gekappt, plus Journal-Kurzfassung mit den
    /// geänderten Dateien), damit der Elternteil es mit `continue_from` bzw.
    /// `agent.message` wieder aufnehmen kann. Best-effort wie
    /// [`Self::record_budget_handoff`].
    fn record_completed_continuation(&self, child: &SessionId, text: Option<&str>) {
        let Some(record) = self.child_record(child) else {
            return;
        };
        let answer = text.map_or_else(
            || "(keine Antwort)".to_owned(),
            |text| cap_task_text(text, COMPLETED_HANDOFF_ANSWER_MAX_BYTES),
        );
        let journal = self
            .comms
            .journal(child)
            .map(|journal| journal.summary(crate::child_comms::END_SUMMARY_MAX_BYTES))
            .unwrap_or_default();
        let entry = crate::child_handoff::HandoffRecord {
            child: child.clone(),
            parent: record.parent.clone(),
            role: record.role.clone(),
            sandbox: self.child_sandbox(child),
            handoff: format!("Letzte Antwort:\n{answer}\n\n{journal}"),
            kind: crate::child_handoff::BudgetHandoff::LastAnswer,
            origin: self.continuation_origin(child),
            end: crate::child_handoff::PredecessorEnd::Completed,
        };
        match self.handoff_ledger.lock() {
            Ok(mut ledger) => ledger.record(entry),
            Err(_) => tracing::warn!(child = %child, "child_handoff_ledger.lock_poisoned"),
        }
    }

    /// Runde 9, E3: die Rolle, mit der `caller` sein beendetes Kind
    /// `child_id` fortsetzen kann (`agent.message` wird dann zur Fortsetzung
    /// über `continue_from`).
    ///
    /// # Returns
    /// `None`, wenn das Kind nicht `caller` gehört, noch läuft, abgebrochen
    /// wurde, unbekannt ist oder seine Kette voll ist — dieselbe Antwort in
    /// allen Fällen, kein Orakel über fremde Kinder.
    #[must_use]
    pub fn resumable_child_role(&self, caller: &SessionId, child_id: &str) -> Option<String> {
        let child = SessionId::try_from_str(child_id.trim().to_owned()).ok()?;
        if let Some(record) = self.child_record(&child)
            && (&record.parent != caller || !record.status.is_terminal())
        {
            return None;
        }
        if self.background.is_detached(&child) {
            return None;
        }
        let ledger = self.handoff_ledger.lock().ok()?;
        let entry = ledger.get(&child).filter(|entry| &entry.parent == caller)?;
        // #22 Welle 1B: auch der Lebenszyklus der Rolle muss eine weitere
        // Ausführung zulassen (`allow_rerun`, `max_attempts`).
        let lifecycle_admits = match self.lifecycle_max_attempts(&entry.role) {
            Ok(None) => true,
            Ok(Some(max_attempts)) => {
                next_attempt(ledger.continuations_used(&entry.origin)) <= max_attempts
            }
            Err(_) => false,
        };
        (ledger.continuations_left(&entry.origin) > 0 && lifecycle_admits)
            .then(|| entry.role.clone())
    }

    /// #22 Welle 1B: der Lebenszyklus der Agent-IR einer Rolle für
    /// Fortsetzungen (`continue_from` ist eine erneute Ausführung desselben
    /// Auftrags).
    ///
    /// # Returns
    /// `Ok(None)` ohne IR oder ohne `max_attempts` (dann gilt allein
    /// [`crate::child_handoff::MAX_CONTINUATIONS`]), sonst
    /// `Ok(Some(max_attempts))` — Versuche **einschließlich** des ersten.
    ///
    /// # Errors
    /// [`AgentSpawnError`] mit Präfix `continue_from:`, wenn die IR
    /// `allow_rerun = false` trägt.
    fn lifecycle_max_attempts(&self, role_name: &str) -> Result<Option<u32>, AgentSpawnError> {
        let Some(ir) = self
            .roles
            .get(role_name)
            .and_then(|definition| definition.registry_factory.executable_agent_ir(role_name))
        else {
            return Ok(None);
        };
        let lifecycle = ir.lifecycle_machine();
        if !lifecycle.allow_rerun() {
            return Err(Self::reject(format!(
                "continue_from: die Rolle '{role_name}' erlaubt keine erneute Ausführung \
                 ([lifecycle] allow_rerun = false) — starte statt einer Fortsetzung ein neues \
                 Kind mit vollständigem Auftrag"
            )));
        }
        Ok(lifecycle.max_attempts())
    }

    /// #22 Welle 1B: prüft, ob nach `used` Fortsetzungen der Kette mit
    /// Ursprung `origin` noch ein Versuch innerhalb von `max_attempts` liegt.
    ///
    /// # Errors
    /// [`AgentSpawnError`] mit Präfix `continue_from:` und der Grenze.
    fn check_attempts(
        role_name: &str,
        max_attempts: u32,
        used: u32,
        origin: &SessionId,
    ) -> Result<(), AgentSpawnError> {
        let attempt = next_attempt(used);
        if attempt > max_attempts {
            return Err(Self::reject(format!(
                "continue_from: die Rolle '{role_name}' erlaubt höchstens {max_attempts} \
                 Versuche ([lifecycle] max_attempts = {max_attempts}); die Fortsetzung wäre \
                 Versuch {attempt} des ursprünglichen Kindes {origin}. Führe die Übergaben \
                 selbst zusammen oder schneide den Auftrag neu und kleiner zu."
            )));
        }
        Ok(())
    }

    /// Runde 5, Teil J: prüft, ob `caller` das budget-beendete Kind `from`
    /// mit der Rolle `role` fortsetzen darf.
    ///
    /// # Beschreibung
    /// Nur eigene Kinder (`caller` ist ihr Elternteil), deren Lauf mit
    /// `budget_exhausted` endete, nur mit derselben Rolle und höchstens
    /// [`crate::child_handoff::MAX_CONTINUATIONS`] Fortsetzungen je
    /// ursprünglichem Kind. Die Prüfung erteilt **keine** Rechte: die
    /// Fortsetzung durchläuft danach die normale Admission (Spawn-Matrix,
    /// Sandbox-Schnitt, Kapazität) und wird erst mit
    /// [`Self::bind_continuation`] verbindlich.
    ///
    /// # Arguments
    /// - `caller` (`&SessionId`): die aufrufende Sitzung (aus dem
    ///   Ausführungskontext, nie aus Modell-Argumenten).
    /// - `from` (`&SessionId`): das fortzusetzende Kind.
    /// - `role` (`&str`): die Rolle der Fortsetzung.
    ///
    /// # Errors
    /// [`AgentSpawnError`] mit der Meldung aus
    /// [`crate::child_handoff::HandoffLedger::prepare`].
    pub fn prepare_continuation(
        &self,
        caller: &SessionId,
        from: &SessionId,
        role: &str,
    ) -> Result<crate::child_handoff::ContinuationSeed, AgentSpawnError> {
        let ledger = self
            .handoff_ledger
            .lock()
            .map_err(|_| Self::reject("child handoff ledger lock is poisoned"))?;
        let seed = ledger
            .prepare(caller, from, role)
            .map_err(|error| Self::reject(error.0))?;
        // #22 Welle 1B: erst nach der Eigentumsprüfung (kein Orakel über
        // fremde Kinder) — dann der Lebenszyklus der Rolle.
        if let Some(max_attempts) = self.lifecycle_max_attempts(role)? {
            Self::check_attempts(
                role,
                max_attempts,
                ledger.continuations_used(&seed.origin),
                &seed.origin,
            )?;
        }
        Ok(seed)
    }

    /// Runde 5, Teil J: bindet ein frisch admittiertes Kind als Fortsetzung.
    ///
    /// # Beschreibung
    /// Prüft vor dem ersten Lauf, dass das Kind demselben Elternteil gehört,
    /// dieselbe Rolle trägt und seine Sandbox nicht weiter ist als die des
    /// Vorgängers (`SandboxSpec::ensure_child_of`), zählt die Fortsetzung in
    /// der Kette und kennzeichnet das Kind für Agent-Panel und Ereignisse als
    /// „Fortsetzung von <id>".
    ///
    /// # Errors
    /// [`AgentSpawnError`], wenn eine der Prüfungen scheitert oder die
    /// Kettengrenze inzwischen erreicht ist. Der Aufrufer gibt das Kind dann
    /// frei, ohne es laufen zu lassen.
    pub fn bind_continuation(
        &self,
        child: &SessionId,
        seed: &crate::child_handoff::ContinuationSeed,
    ) -> Result<crate::child_handoff::ContinuationLink, AgentSpawnError> {
        let record = self
            .child_record(child)
            .ok_or_else(|| Self::reject(format!("continue_from: child {child} is not admitted")))?;
        if record.parent != seed.parent {
            return Err(Self::reject(
                "continue_from: die Fortsetzung gehört nicht demselben Elternteil",
            ));
        }
        if record.role != seed.role {
            return Err(Self::reject(format!(
                "continue_from: eine Fortsetzung muss dieselbe Rolle tragen wie das \
                 fortgesetzte Kind ('{}'), nicht '{}'",
                seed.role, record.role
            )));
        }
        let prior = seed.sandbox.as_ref().ok_or_else(|| {
            Self::reject(format!(
                "continue_from: die Sandbox von {} ist nicht bekannt; keine Fortsetzung möglich",
                seed.of
            ))
        })?;
        let sandbox = self.child_sandbox(child).ok_or_else(|| {
            Self::reject(format!(
                "continue_from: child {child} has no trusted sandbox context"
            ))
        })?;
        sandbox.ensure_child_of(prior).map_err(|error| {
            Self::reject(format!(
                "continue_from: die Sandbox der Fortsetzung ist weiter als die von {} ({error})",
                seed.of
            ))
        })?;
        // #22 Welle 1B: der Lebenszyklus der Rolle wird unter derselben
        // Sperre wie die Kettenzählung geprüft — zwei gleichzeitige
        // Fortsetzungen können `max_attempts` nicht gemeinsam überschreiten.
        let max_attempts = self.lifecycle_max_attempts(&record.role)?;
        let link = {
            let mut ledger = self
                .handoff_ledger
                .lock()
                .map_err(|_| Self::reject("child handoff ledger lock is poisoned"))?;
            if let Some(max_attempts) = max_attempts {
                Self::check_attempts(
                    &record.role,
                    max_attempts,
                    ledger.continuations_used(&seed.origin),
                    &seed.origin,
                )?;
            }
            ledger.bind(seed).map_err(|error| Self::reject(error.0))?
        };
        match self.child_tasks.lock() {
            Ok(mut tasks) => {
                let state = tasks.entry(child.as_str().to_owned()).or_default();
                let mark = format!(
                    "Fortsetzung von {} ({}/{})",
                    link.of,
                    link.number,
                    crate::child_handoff::MAX_CONTINUATIONS
                );
                // Der Kurzkopf des Auftrags trägt die Kennzeichnung, damit
                // jedes weitere Orchestrierungs-Event die Fortsetzung zeigt.
                state.task = orchestration_detail_head(&match state.task.take() {
                    Some(task) => format!("{mark}: {task}"),
                    None => mark.clone(),
                });
                state.admission_warning = Some(match state.admission_warning.take() {
                    Some(warning) => format!("{mark}; {warning}"),
                    None => mark,
                });
                state.continuation = Some(link.clone());
            }
            Err(_) => tracing::warn!(child = %child, "child_task_state.lock_poisoned"),
        }
        tracing::info!(
            child = %child,
            continues = %link.of,
            origin = %link.origin,
            number = link.number,
            "child_handoff.continuation_bound",
        );
        Ok(link)
    }

    /// Runde 5, Teil J: die Fortsetzungs-Verknüpfung eines admittierten
    /// Kindes (`None`, wenn es keine Fortsetzung ist).
    #[must_use]
    pub fn continuation_link(
        &self,
        child: &SessionId,
    ) -> Option<crate::child_handoff::ContinuationLink> {
        self.child_task_state(child)
            .and_then(|state| state.continuation)
    }

    /// Runde 5, Teil J: die vorgehaltene Budget-Übergabe eines Kindes
    /// (Kopie), z. B. für Tests und Diagnose.
    #[must_use]
    pub fn budget_handoff_record(
        &self,
        child: &SessionId,
    ) -> Option<crate::child_handoff::HandoffRecord> {
        self.handoff_ledger
            .lock()
            .ok()
            .and_then(|ledger| ledger.get(child).cloned())
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

    /// Typisierter Laufzeit-Befund einer erschöpften Budget-Dimension
    /// (Teil C) — dasselbe Format wie [`Self::budget_exceeded`], aber ohne
    /// die Spawn-Fehler-Hülle.
    fn budget_exhausted(
        dimension: BudgetDimension,
        limit: u64,
        used: u64,
    ) -> harw_extension_api::ChildBudgetExhausted {
        harw_extension_api::ChildBudgetExhausted {
            dimension: dimension.as_str().to_owned(),
            limit,
            used,
        }
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

    // ── Runde 9, E7: Eltern mit laufendem Turn ─────────────────────────────

    /// Legt die Eltern-Sicht eines eben für seinen Turn entnommenen Kindes
    /// ab. Der Aufrufer hält den Manager-Lock (Lock-Reihenfolge `manager` →
    /// `checked_out`).
    fn check_out(&self, child: &SessionId, session: &AgentSession) {
        match self.checked_out.lock() {
            Ok(mut checked_out) => {
                checked_out.insert(child.as_str().to_owned(), CheckedOutParent::of(session));
            }
            Err(_) => tracing::warn!(child = %child, "child_checkout.lock_poisoned"),
        }
    }

    /// Entfernt die Eltern-Sicht wieder (Session zurückgelegt oder verworfen).
    fn check_in(&self, child: &SessionId) {
        match self.checked_out.lock() {
            Ok(mut checked_out) => {
                checked_out.remove(child.as_str());
            }
            Err(_) => tracing::warn!(child = %child, "child_checkin.lock_poisoned"),
        }
    }

    /// Eltern-Sicht eines Kindes, dessen Session gerade für einen Turn
    /// entnommen ist; `None` für jede andere Session und für ein Kind, das
    /// inzwischen freigegeben wurde (kein Admission-Record mehr — ein
    /// freigegebenes Kind darf keine Kinder mehr bekommen).
    ///
    /// # Concurrency
    /// Nimmt kurz `checked_out` und `active`; der Aufrufer darf `active`
    /// nicht halten.
    fn checked_out_parent(&self, session: &SessionId) -> Option<CheckedOutParent> {
        let view = self
            .checked_out
            .lock()
            .ok()
            .and_then(|checked_out| checked_out.get(session.as_str()).cloned())?;
        self.child_record(session).map(|_| view)
    }

    /// Tiefe von `parent` unterhalb der Wurzel (Zahl der Kanten bis zur
    /// Wurzel). Runde 5: eine extern registrierte Wurzel (die UIA-Sitzung der
    /// TUI) liegt nicht im `SessionManager`; die Kette endet dort — sonst
    /// scheiterte jeder Enkel unter der UIA mit „unknown child parent“.
    /// Runde 9, E7: die Kette darf außerdem durch Sitzungen mit laufendem
    /// Turn führen (ihre Eltern-Sicht aus [`CheckedOutParent`]). Der Aufrufer
    /// hält den Manager-Lock.
    ///
    /// # Errors
    /// „unknown child parent“, wenn ein Glied weder im Manager noch
    /// entnommen ist, oder bei einer (nie erwarteten) zyklischen Kette.
    fn lineage_depth(
        &self,
        manager: &SessionManager,
        parent: &SessionId,
    ) -> Result<u32, AgentSpawnError> {
        const MAX_HOPS: u32 = 256;
        let external_root = self
            .external_root_parent
            .as_ref()
            .map(|root| &root.session_id);
        let mut depth = 0_u32;
        let mut cursor = parent.clone();
        while depth <= MAX_HOPS {
            if depth > 0 && external_root == Some(&cursor) {
                return Ok(depth);
            }
            let next = match manager.get(&cursor) {
                Ok(session) => session.parent_session_id().cloned(),
                Err(error) => match self.checked_out_parent(&cursor) {
                    Some(view) => view.parent_session_id,
                    None => {
                        return Err(Self::reject(format!("unknown child parent: {error}")));
                    }
                },
            };
            let Some(next) = next else {
                return Ok(depth);
            };
            depth = depth.saturating_add(1);
            cursor = next;
        }
        Err(Self::reject("child parent lineage contains a cycle"))
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

    /// Kern von [`AgentSpawner::delegation_targets`] (und damit von
    /// [`AgentSpawner::delegation_target_names`]) für [`ManagedAgentSpawner`]
    /// (Addendum F+G, Nachtrag F; Plan R9, Teil C/E1).
    ///
    /// # Beschreibung
    /// Holt den Eltern-`SpawnContext` genau wie [`Self::admit`] — aus dem
    /// Manager, aus der Eltern-Sicht einer Sitzung mit laufendem Turn
    /// ([`CheckedOutParent`], Runde 9 E7: ohne sie sah jedes Kind **während**
    /// seines eigenen Turns kein einziges Ziel, `delegate_wave` meldete dann
    /// „no delegation capability“) oder von der externen Wurzel. Die
    /// Kandidaten sind alle registrierten Rollen (Name +
    /// `organizational_role`), die Resttiefe ist
    /// `depth_ceiling(parent) − depth(parent)` wie bei der Admission. Die
    /// Projektion leistet
    /// [`crate::delegation_visibility::visible_delegation_targets`], danach
    /// teilt [`crate::delegation_visibility::delegable_in_mode`] im
    /// Plan-Modus (angefragt oder geerbt, [`Self::lineage_in_plan_mode`]) in
    /// delegierbare und zurückgehaltene Ziele.
    ///
    /// # Errors
    /// - [`harw_extension_api::DelegationUnavailable::NoSpawnContext`]:
    ///   unbekannter Aufrufer, Aufrufer ohne vertrauenswürdigen Kontext oder
    ///   vergiftete Sperre.
    /// - [`harw_extension_api::DelegationUnavailable::DepthExhausted`]:
    ///   Resttiefe 0.
    fn delegation_targets_for(
        &self,
        parent_session_id: &SessionId,
        plan_requested: bool,
    ) -> Result<harw_extension_api::DelegationTargets, harw_extension_api::DelegationUnavailable>
    {
        use harw_extension_api::DelegationUnavailable;
        let no_context = |detail: String| DelegationUnavailable::NoSpawnContext { detail };
        let manager = self
            .manager
            .lock()
            .map_err(|_| no_context("session manager lock is poisoned".to_owned()))?;
        let external_root = self
            .external_root_parent
            .as_ref()
            .filter(|root| &root.session_id == parent_session_id);
        let (caller_role, allowed_child_orchestrators, parent_depth) = match manager
            .get(parent_session_id)
        {
            Ok(parent) => {
                let context = parent.spawn_context().ok_or_else(|| {
                    no_context("delegation caller has no trusted sandbox context".to_owned())
                })?;
                let depth = self
                    .lineage_depth(&manager, parent_session_id)
                    .map_err(|error| no_context(error.message))?;
                (
                    context.organizational_role,
                    context.allowed_child_orchestrators.clone(),
                    depth,
                )
            }
            Err(_) => match (self.checked_out_parent(parent_session_id), external_root) {
                // Runde 9, E7 / Plan R9: Aufrufer mit laufendem Turn.
                (Some(view), _) => {
                    let context = view.spawn_context.ok_or_else(|| {
                        no_context("delegation caller has no trusted sandbox context".to_owned())
                    })?;
                    let depth = self
                        .lineage_depth(&manager, parent_session_id)
                        .map_err(|error| no_context(error.message))?;
                    (
                        context.organizational_role,
                        context.allowed_child_orchestrators,
                        depth,
                    )
                }
                (None, Some(root)) => (
                    root.spawn_context.organizational_role,
                    root.spawn_context.allowed_child_orchestrators.clone(),
                    0,
                ),
                (None, None) => {
                    return Err(no_context(format!(
                        "unknown delegation caller: {parent_session_id}"
                    )));
                }
            },
        };
        let plan_mode = plan_requested || self.lineage_in_plan_mode(&manager, parent_session_id);
        drop(manager);
        let inherited_depth_ceiling = self
            .active
            .lock()
            .map_err(|_| no_context("child registry lock is poisoned".to_owned()))?
            .get(parent_session_id.as_str())
            .map_or(self.limits.max_depth, |record| record.depth_ceiling);
        let remaining_depth = inherited_depth_ceiling.saturating_sub(parent_depth);
        if remaining_depth == 0 {
            return Err(DelegationUnavailable::DepthExhausted);
        }
        let candidates: Vec<(String, harw_agent_dsl::roles::AgentRoleId)> = self
            .roles
            .iter()
            .map(|(name, definition)| (name.clone(), definition.organizational_role))
            .collect();
        let visible = crate::delegation_visibility::visible_delegation_targets(
            caller_role,
            &candidates,
            &allowed_child_orchestrators,
            remaining_depth,
        );
        let mut result = harw_extension_api::DelegationTargets {
            plan_mode,
            ..harw_extension_api::DelegationTargets::default()
        };
        for target in visible {
            let info = self.delegation_target_info(&target.name, target.role);
            if crate::delegation_visibility::delegable_in_mode(plan_mode, info.read_only) {
                result.targets.push(info);
            } else {
                result.withheld_by_plan_mode.push(info);
            }
        }
        Ok(result)
    }

    /// Die Namen aus [`Self::delegation_targets_for`] (ohne angefragten
    /// Plan-Modus) mit dem Grund als [`AgentSpawnError`] — für Tests, die
    /// einen fehlenden Spawn-Kontext nicht verschlucken dürfen.
    ///
    /// # Errors
    /// Der benannte Grund aus [`Self::delegation_targets_for`].
    #[cfg(test)]
    fn visible_delegation_target_names(
        &self,
        parent_session_id: &SessionId,
    ) -> Result<Vec<String>, AgentSpawnError> {
        self.delegation_targets_for(parent_session_id, false)
            .map(|targets| targets.names())
            .map_err(|error| Self::reject(error.to_string()))
    }

    /// Plan R9, E1: ob `session` selbst oder einer ihrer Vorfahren im
    /// Plan-Modus ist — Kinder erben den Plan-Modus ihres Elternteils.
    ///
    /// # Beschreibung
    /// Hat die Oberfläche einen Live-Modus veröffentlicht (Runde 9, E6,
    /// [`Self::live_mode`]), ist er die Antwort für den ganzen Baum — so
    /// können Sichtbarkeit und Live-Modus der Kinder nie auseinanderlaufen
    /// (eine entnommene Eltern-Sicht trüge sonst einen veralteten Modus).
    /// Ohne Veröffentlichung läuft die Kette wie [`Self::lineage_depth`] über Manager-Sitzungen und
    /// Eltern-Sichten laufender Turns bis zur externen Wurzel, deren Modus
    /// über [`AgentSpawner::note_caller_mode`] gemeldet wird. Ein Modus-
    /// wechsel (auch ein live weitergereichter) wirkt damit sofort auf alle
    /// Nachkommen. Eine unbekannte Sitzung beendet die Suche (kein
    /// Plan-Modus bekannt). Der Aufrufer hält den Manager-Lock.
    fn lineage_in_plan_mode(&self, manager: &SessionManager, session: &SessionId) -> bool {
        const MAX_HOPS: u32 = 256;
        if let Some(mode) = self.live_mode.current() {
            return mode == crate::mode::InteractionMode::Plan;
        }
        let external_root = self
            .external_root_parent
            .as_ref()
            .map(|root| &root.session_id);
        let root_in_plan = || {
            self.external_root_plan_mode
                .load(std::sync::atomic::Ordering::Acquire)
        };
        let mut cursor = session.clone();
        for _ in 0..MAX_HOPS {
            let (mode, next) = match manager.get(&cursor) {
                Ok(current) => (current.mode(), current.parent_session_id().cloned()),
                Err(_) => match self.checked_out_parent(&cursor) {
                    Some(view) => (view.mode, view.parent_session_id),
                    None => return external_root == Some(&cursor) && root_in_plan(),
                },
            };
            if mode == crate::mode::InteractionMode::Plan {
                return true;
            }
            match next {
                Some(next) => cursor = next,
                None => return false,
            }
        }
        false
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
        mut input: SpawnInput,
        sandbox: SandboxSpec,
        suggestions: Option<AgentSuggestions>,
    ) -> Result<SessionId, AgentSpawnError> {
        // Runde 5, Teil J: `continue_from` im Spawn-Kontext (Handoff
        // `transfer_to_<rolle>`, Agent-Werkzeug).
        let seed = self.requested_continuation(role_name, &mut input)?;
        let child = self
            .admit_inner(role_name, input, sandbox, suggestions, None)
            .map_err(AdmitRejection::into_error)?;
        self.bind_requested_continuation(child, seed.as_ref())
    }

    /// Runde 5, Teil J: liest ein optionales `continue_from` aus dem
    /// Spawn-Kontext (Handoff `transfer_to_<rolle>`, Agent-Werkzeug) und prüft
    /// es über [`Self::prepare_continuation`]. Der Elternteil ist
    /// `input.parent_session_id` (vom Turn-Loop gesetzt, nie vom Modell).
    /// Bei einer Fortsetzung wird der Auftrag durch
    /// [`crate::child_handoff::continuation_task`] ersetzt (Übergabe als
    /// erster Kontext, bisheriger Auftrag als Arbeitsauftrag).
    ///
    /// # Errors
    /// [`AgentSpawnError`] mit Präfix
    /// [`crate::child_handoff::CONTINUATION_REJECTION_PREFIX`], wenn das Feld
    /// kein gültiger Kind-Verweis ist oder die Fortsetzung nicht zulässig ist.
    fn requested_continuation(
        &self,
        role_name: &str,
        input: &mut SpawnInput,
    ) -> Result<Option<crate::child_handoff::ContinuationSeed>, AgentSpawnError> {
        let from = match input.context.get("continue_from") {
            None | Some(serde_json::Value::Null) => return Ok(None),
            Some(value) => value
                .as_str()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .and_then(|id| SessionId::try_from_str(id.to_owned()).ok())
                .ok_or_else(|| {
                    Self::reject("continue_from: erwartet die ID eines eigenen Kindes")
                })?,
        };
        let seed = self.prepare_continuation(&input.parent_session_id, &from, role_name)?;
        let task = input
            .instructions
            .clone()
            .or_else(|| {
                input
                    .context
                    .get("task")
                    .and_then(serde_json::Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .filter(|task| !task.trim().is_empty());
        input.instructions = Some(crate::child_handoff::continuation_task(
            &seed.of,
            &seed.end,
            &seed.handoff,
            task.as_deref(),
        ));
        Ok(Some(seed))
    }

    /// Runde 5, Teil J: bindet ein frisch admittiertes Kind als Fortsetzung,
    /// falls eine verlangt war; scheitert die Bindung, wird das Kind sofort
    /// wieder freigegeben (es lief nie).
    ///
    /// # Errors
    /// Der Fehler von [`Self::bind_continuation`].
    fn bind_requested_continuation(
        &self,
        child: SessionId,
        seed: Option<&crate::child_handoff::ContinuationSeed>,
    ) -> Result<SessionId, AgentSpawnError> {
        let Some(seed) = seed else {
            return Ok(child);
        };
        match self.bind_continuation(&child, seed) {
            Ok(_) => Ok(child),
            Err(error) => {
                if let Err(release) = self.release_child(&child) {
                    tracing::warn!(child = %child, error = %release, "child_continuation.release_failed");
                }
                Err(error)
            }
        }
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
        recovery: Option<&ChildRecoveryView>,
    ) -> Result<SessionId, AdmitRejection> {
        let definition = self
            .roles
            .get(role_name)
            .ok_or_else(|| Self::reject(format!("child role '{role_name}' is not registered")))?;

        // Addendum D: aus dem rohen Spawn-Kontext gelesen, bevor `input`
        // weiter unten feldweise in den `ChildRecord` verschoben wird.
        let task_complexity = TaskComplexity::from_spawn_context(&input.context);

        // A recovery envelope is evidence, never authority. Identity fields
        // must bind exactly to the fresh admission request before any mutable
        // controller state is created.
        if let Some(recovery) = recovery {
            if recovery.child.as_str().is_empty()
                || recovery.parent != input.parent_session_id
                || recovery.handoff_call_id != input.handoff_call_id
                || recovery.role != role_name
            {
                return Err(AdmitRejection::Other(Self::reject(
                    "agent recovery identity does not match the current admission",
                )));
            }
        }

        // W2-19: die Agent-IR dieser Rolle wird vor jeder Prüfung aufgelöst,
        // weil sie die Tiefengrenze verschärfen darf. Ein fehlerhaftes Budget
        // (unbekanntes Effort-Label) lehnt die Admission fail-closed ab, bevor
        // irgendein Zustand entsteht.
        let executable_ir = definition.registry_factory.executable_agent_ir(role_name);
        let child_budget = match executable_ir.and_then(|ir| ir.spawn_contract().budget()) {
            Some(spec) => Self::budget_from_spec(spec)?,
            None => AgentBudget::default(),
        };
        let child_budget = recovery.map_or(child_budget, |recovery| {
            tighten_agent_budget(child_budget, recovery.budget.into())
        });
        if let Some(recovery) = recovery {
            if recovery.organizational_role != definition.organizational_role {
                return Err(AdmitRejection::Other(Self::reject(
                    "agent recovery role authority no longer matches the current definition",
                )));
            }
            let ir_matches = match (recovery.executable_snapshot_id.as_ref(), executable_ir) {
                (None, None) => true,
                (Some(reference), Some(ir)) => reference.confirm(ir.snapshot_id()).is_some(),
                _ => false,
            };
            if !ir_matches {
                return Err(AdmitRejection::Other(Self::reject(
                    "agent recovery executable snapshot is not the current trusted IR",
                )));
            }
        }
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
        // Teil C: das Modell des Elternteils (sein `active_model`; bei der
        // extern gefahrenen Wurzel `Self::root_model`) ist der Rückfall für
        // das Kind-Modell, wenn weder Pin noch Factory-Hauptmodell bekannt
        // sind.
        // Runde 9, E7: Eltern-Sicht, falls der Elternteil gerade selbst einen
        // Turn fährt (seine Session ist dann nicht im Manager).
        let checked_out_parent = if manager.contains(&input.parent_session_id) {
            None
        } else {
            self.checked_out_parent(&input.parent_session_id)
        };
        let (
            parent_context,
            parent_reasoning_effort,
            parent_activation,
            parent_depth,
            mut parent_model,
        ) = match manager.get(&input.parent_session_id) {
            Ok(parent) => (
                parent
                    .spawn_context()
                    .cloned()
                    .ok_or_else(|| Self::reject("child parent has no trusted sandbox context"))?,
                parent.reasoning_effort(),
                parent.activation().clone(),
                // Runde 9, E7: die Kette darf durch Vorfahren mit laufendem
                // Turn führen (`lineage_depth`).
                self.lineage_depth(&manager, &input.parent_session_id)?,
                parent.active_model().map(|model| model.as_str().to_owned()),
            ),
            // Runde 9, E7: der Elternteil ist ein Kind, dessen Turn gerade
            // läuft (Session entnommen) — z. B. der Matrix-Game-Master, der
            // aus `matrix.run` heraus seine Sitz-Agenten startet.
            Err(_) if checked_out_parent.is_some() => {
                let view = checked_out_parent.clone().ok_or_else(|| {
                    Self::reject(format!("unknown child parent: {}", input.parent_session_id))
                })?;
                (
                    view.spawn_context.ok_or_else(|| {
                        Self::reject("child parent has no trusted sandbox context")
                    })?,
                    view.reasoning_effort,
                    view.activation,
                    self.lineage_depth(&manager, &input.parent_session_id)?,
                    view.active_model,
                )
            }
            Err(_) => {
                let external_root = self
                    .external_root_parent
                    .as_ref()
                    .filter(|root| root.session_id == input.parent_session_id)
                    .ok_or_else(|| {
                        Self::reject(format!("unknown child parent: {}", input.parent_session_id))
                    })?;
                (
                    external_root.spawn_context.clone(),
                    external_root.reasoning_effort,
                    external_root.activation.clone(),
                    0,
                    self.root_model.clone(),
                )
            }
        };
        // Plan R9, E1 (mit E6): folgt der Baum einem veröffentlichten
        // Live-Modus, schneidet das Kind gegen die **Basis** des Elternteils
        // (Aktivierung und Sandbox ohne Modus-Schnitt) — der Live-Modus
        // schneidet dann das Kind selbst dynamisch. Sonst behielte ein im
        // Plan-Modus gestarteter (lesender) Orchestrator nach dem Wechsel zu
        // `work` für immer die Plan-Decke und könnte seinen schreibenden
        // Kindern nichts mehr geben. Ohne Live-Modus folgt das Kind keinem
        // Wechsel; dann bleibt der Schnitt gegen den aktuellen Stand.
        let follows_live_mode = self.live_mode.current().is_some();
        let (parent_base_activation, parent_base_sandbox) = if follows_live_mode {
            match manager.get(&input.parent_session_id) {
                Ok(parent) => (
                    Some(parent.base_activation().clone()),
                    parent.base_sandbox().cloned(),
                ),
                Err(_) => checked_out_parent.as_ref().map_or((None, None), |view| {
                    (
                        Some(view.base_activation.clone()),
                        view.base_sandbox.clone(),
                    )
                }),
            }
        } else {
            (None, None)
        };
        let parent_activation = parent_base_activation.unwrap_or(parent_activation);
        // Nur eine unverändert durchgereichte Eltern-Sandbox (Handoff) wird auf
        // die Basis gehoben; eine bewusst engere Anfrage (Reducer, Sitz-
        // Sandbox) bleibt, wie sie ist, und wird gegen den aktuellen Stand
        // geprüft. Grenzfall: schneidet der Plan-Modus die Eltern-Sandbox
        // schon auf genau das, was ein Reducer ergäbe, ist die Anfrage davon
        // nicht zu unterscheiden und wird ebenfalls gehoben. Das bleibt in
        // der Basis des Elternteils (Kind ⊆ Eltern-Basis), und die Werkzeuge
        // des Kindes begrenzt weiter seine eigene Rolle (Registry-Profil,
        // Agent-IR) — ein lesendes Kind bekommt dadurch kein Schreibwerkzeug.
        let (sandbox, parent_sandbox_ceiling) = if let Some(recovery) = recovery {
            if !recovery
                .authority
                .workspace()
                .matches(parent_context.sandbox.workspace())
            {
                return Err(AdmitRejection::Other(Self::reject(
                    "agent recovery workspace no longer matches the trusted parent",
                )));
            }
            let parent = parent_base_sandbox.unwrap_or_else(|| parent_context.sandbox.clone());
            (
                parent.restrict(recovery.authority.request()),
                parent,
            )
        } else {
            match parent_base_sandbox {
                Some(base) if sandbox == parent_context.sandbox => (base.clone(), base),
                _ => (sandbox, parent_context.sandbox.clone()),
            }
        };
        // Beide Prüfungen (geschlossene Rollenmatrix inkl. `uia-worker`,
        // Addendum J + exakte `ChildOrchestrator`-Freigabeliste) laufen über
        // `can_delegate_to`, dieselbe Hilfsfunktion, die auch
        // `delegation_visibility::visible_delegation_targets` verwendet —
        // damit kann die dem Modell gezeigte Zielliste nie von der
        // tatsächlichen Admission abweichen. Einzige Ergänzung: die enge
        // UIA-Freigabeliste für nur lesende Worker-Rollen ohne Netz,
        // Schreiben oder Exec (`Self::with_uia_spawnable_roles`, z. B.
        // `/matrix`-Sitze).
        if !self.spawn_permitted(
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
        // Plan R9, E1: im Plan-Modus (eigener oder geerbter) nur lesende
        // Ziele — dieselbe Regel wie die Sichtbarkeit
        // (`delegation_visibility::delegable_in_mode`). Ohne Katalog kennt der
        // Controller keine Lese-Eigenschaft und prüft nicht zusätzlich (siehe
        // `with_delegation_catalog`). Die Meldung nennt nur lesende Ziele,
        // die der Aufrufer laut Spawn-Matrix ohnehin sieht.
        if !self.delegation_catalog.is_empty()
            && self.lineage_in_plan_mode(&manager, &input.parent_session_id)
        {
            let read_only = |name: &str| {
                self.delegation_catalog
                    .get(name)
                    .is_some_and(|entry| entry.read_only)
            };
            if !crate::delegation_visibility::delegable_in_mode(true, read_only(role_name)) {
                let allowed: Vec<String> = self
                    .roles
                    .iter()
                    .filter(|(name, role)| {
                        read_only(name.as_str())
                            && can_delegate_to(
                                parent_context.organizational_role,
                                role.organizational_role,
                                name.as_str(),
                                &parent_context.allowed_child_orchestrators,
                            )
                    })
                    .map(|(name, _)| name.clone())
                    .collect();
                return Err(AdmitRejection::Other(Self::reject(
                    crate::delegation_visibility::plan_mode_refusal(&allowed),
                )));
            }
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
        // Ein Kind als Elternteil: sein bei der Admission festgestelltes
        // Modell, falls seine Session kein eigenes `active_model` trägt.
        if parent_model.is_none() {
            parent_model = active
                .get(input.parent_session_id.as_str())
                .and_then(|record| record.model.clone());
        }
        // Dasselbe für den Provider (nur Anzeige): eigene Wahl der
        // Eltern-Session, sonst ihr Admission-Wert, sonst der der Wurzel.
        let parent_provider: Option<String> = manager
            .get(&input.parent_session_id)
            .ok()
            .and_then(|parent| parent.active_provider().map(|p| p.as_str().to_owned()))
            .or_else(|| {
                checked_out_parent
                    .as_ref()
                    .and_then(|view| view.active_provider.clone())
            })
            .or_else(|| {
                active
                    .get(input.parent_session_id.as_str())
                    .and_then(|record| record.provider.clone())
            })
            .or_else(|| self.root_provider.clone());
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
        // Runde 5, Teil K: Orchestrierungsgrenzen (`[agents]`), fail-closed
        // und mit einer Meldung, die das Modell lesen soll.
        {
            let nodes: Vec<crate::background_children::AdmittedNode<'_>> = active
                .values()
                .map(|record| crate::background_children::AdmittedNode {
                    child: record.child.as_str(),
                    parent: record.parent.as_str(),
                    role_name: record.role.as_str(),
                    organizational_role: self
                        .roles
                        .get(&record.role)
                        .map(|definition| definition.organizational_role),
                })
                .collect();
            crate::background_children::check_orchestration_admission(
                &nodes,
                input.parent_session_id.as_str(),
                parent_context.organizational_role,
                definition.organizational_role,
                self.background.limits(),
            )
            .map_err(|message| AdmitRejection::Other(Self::reject(message)))?;
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
            .ensure_child_of(&parent_sandbox_ceiling)
            .map_err(|error| Self::reject(format!("child sandbox escalation rejected: {error}")))?;
        let approval_actor = parent_context.approval_actor.clone();
        // AW1-01b: Trace wird vererbt, nie neu erzeugt — dieselbe `trace_id`,
        // eine frische `span_id` für das Kind, siehe `inherit_trace`.
        let child_trace = match recovery {
            Some(recovery) => recovery.trace.clone(),
            None => inherit_trace(parent_context.trace.as_ref())?,
        };
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
        let mut child_depth_ceiling = executable_ir
            .and_then(|ir| ir.spawn_contract().max_depth())
            .map_or(inherited_depth_ceiling, |ir_depth| {
                inherited_depth_ceiling.min(depth.saturating_add(ir_depth))
            });
        if let Some(recovery) = recovery {
            if depth != recovery.depth {
                return Err(AdmitRejection::Other(Self::reject(
                    "agent recovery depth no longer matches the current parent lineage",
                )));
            }
            child_depth_ceiling = child_depth_ceiling.min(recovery.depth_ceiling);
        }

        let mut capability_snapshot = definition
            .registry_factory
            .capability_snapshot(role_name, &input)?;
        if let Some(recovery) = recovery {
            capability_snapshot = Self::recovery_capability_snapshot(
                capability_snapshot,
                recovery,
            )?;
        }
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
            // Runde 9, E7: Elternteil mit laufendem Turn — dieselbe Filterung
            // über die Werkzeugnamen seiner Eltern-Sicht.
            Err(_) => checked_out_parent
                .as_ref()
                .map(|view| {
                    view.tool_names
                        .iter()
                        .filter(|name| {
                            parent_activation
                                .is_tool_enabled(&harw_tools::ToolName::new(name.as_str()))
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default(),
        };
        let parent_grant = ParentGrant {
            role: Some(parent_context.organizational_role),
            tools: parent_tools,
            permissions: parent_sandbox_ceiling.permissions().clone(),
            max_depth: inherited_depth_ceiling.saturating_sub(parent_depth),
            budget_tokens: active
                .get(input.parent_session_id.as_str())
                .and_then(|record| record.budget.max_tokens)
                .unwrap_or(0),
            reasoning_effort: parent_reasoning_effort.map(|effort| effort.to_string()),
        };
        let registry = definition
            .registry_factory
            .build_registry_from_capability_snapshot_for_parent(
                role_name,
                &input,
                capability_snapshot.as_ref(),
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
        let mut child_allowed_child_orchestrators: Vec<String> = executable_ir
            .map(|ir| ir.spawn_contract().child_orchestrators().to_vec())
            .unwrap_or_default();
        if let Some(recovery) = recovery {
            child_allowed_child_orchestrators.retain(|name| {
                recovery.allowed_child_orchestrators.contains(name)
            });
        }
        let spawn_context = SpawnContext {
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
            };
        let child = if let Some(recovery) = recovery {
            let child = recovery.child.clone();
            manager
                .create_governed_session_with_id(
                    child.clone(),
                    definition.role.clone(),
                    Some(input.parent_session_id.clone()),
                    registry,
                    spawn_context,
                )
                .map_err(|error| {
                    Self::reject(format!(
                        "could not restore governed child identity {child}: {error}"
                    ))
                })?;
            child
        } else {
            manager.create_governed_session(
                definition.role.clone(),
                Some(input.parent_session_id.clone()),
                registry,
                spawn_context,
            )
        };
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
            // Runde 9, E6: das Kind startet im aktuellen Live-Modus des
            // Baums und folgt späteren Wechseln (von seiner Basis aus).
            child_session.attach_live_mode(self.live_mode.follower());
        }
        // Welle 3: das Modell, das der Kind-Provider fest anspricht (z. B. ein
        // `PinnedModelProvider` für Explorer/Worker). Die Factory ist die
        // einzige Stelle, die das Provider-Routing kennt. `None` blockiert
        // die Admission nicht — dann gilt das `active_model` der Session.
        let child_pinned_model: Option<String> = definition
            .registry_factory
            .pinned_model_for_task(role_name, task_complexity);
        // Teil C: das Modell, das ein ungepinntes Kind ohne `active_model`
        // wirklich ruft (Hauptmodell seiner Factory).
        let child_main_model: Option<String> = definition
            .registry_factory
            .main_model_for_task(role_name, task_complexity);
        // Anzeige `<provider>/<modell>`: der Provider, den das Kind anspricht.
        let child_pinned_provider: Option<String> = definition
            .registry_factory
            .pinned_provider_for_task(role_name, task_complexity);
        let child_main_provider: Option<String> = definition
            .registry_factory
            .main_provider_for_task(role_name, task_complexity);
        let child_provider: Option<String>;
        // Für `ChildRecord::model`: gepinntes Modell, sonst `active_model`;
        // gesetzt im Auto-Compact-Block unten, der die Kind-Session ohnehin liest.
        let mut child_model: Option<String> = child_pinned_model.clone();
        // Teil C: Warnungen der Admission (unbekanntes Modell, gekürzter
        // Auftrag) und die Höchstlänge des Auftrags, beide aus dem
        // Auto-Compact-Block unten.
        let mut admission_warnings: Vec<String> = Vec::new();
        let task_max_bytes: Option<usize>;
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
            // Teil C, Rückfallkette: festgelegtes Rollenmodell (Pin) →
            // eigenes `active_model` → Hauptmodell der Factory → Modell des
            // Elternteils → Vorgabe des Resolvers (`None`). Ohne Pin bekommt
            // das Kind das gefundene Modell als `active_model`, damit es genau
            // das Modell ruft, dessen Fenster hier budgetiert wird.
            let active_model = child_session
                .active_model()
                .map(|model| model.as_str().to_owned());
            let model = child_pinned_model
                .clone()
                .or_else(|| active_model.clone())
                .or_else(|| child_main_model.clone())
                .or_else(|| parent_model.clone());
            if child_pinned_model.is_none()
                && active_model.is_none()
                && let Some(model) = model.as_deref()
            {
                child_session.set_active_model(Some(harw_types::ModelId::from(model)));
            }
            child_model.clone_from(&model);
            child_provider = child_pinned_provider
                .clone()
                .or_else(|| {
                    child_session
                        .active_provider()
                        .map(|provider| provider.as_str().to_owned())
                })
                .or_else(|| child_main_provider.clone())
                .or_else(|| parent_provider.clone());
            if let Some(recovery) = recovery
                && (recovery.model != child_model || recovery.provider != child_provider)
            {
                let _ = manager.remove(&child);
                return Err(AdmitRejection::Other(Self::reject(
                    "agent recovery model/provider routing changed since admission",
                )));
            }
            let window = self.context_window_for(model.as_deref());
            let window_known = self
                .model_known_probe
                .as_ref()
                .is_none_or(|probe| probe(model.as_deref()));
            if !window_known {
                tracing::warn!(
                    child = %child,
                    role = role_name,
                    model = model.as_deref().unwrap_or("<default>"),
                    window_tokens = window,
                    "child_admission.unknown_model_window: das Kind-Modell ist weder \
                     konfiguriert noch im Modellkatalog; konservatives Rückfallfenster aktiv"
                );
                admission_warnings.push(format!(
                    "Warnung: Modell '{}' unbekannt — Kontextfenster auf {window} Tokens \
                     zurückgefallen ([models.<id>].context_window setzen)",
                    model.as_deref().unwrap_or("<default>")
                ));
            }
            // Teil C: Grundlast aus System-Prompt (geschätzt) und
            // Werkzeugschemata gegen das Kind-Fenster. Der Auftrag bekommt
            // höchstens `CHILD_TASK_MAX_WINDOW_PERCENT` % des Fensters und
            // höchstens, was bis `CHILD_BASE_LOAD_MAX_WINDOW_PERCENT` % übrig
            // bleibt; bleibt dafür nicht einmal `CHILD_MIN_TASK_TOKENS`,
            // scheitert die Admission mit Zahlen statt später am Kontextlimit.
            // Runde 7, Teil L9: kleine Kontextfenster (lokale Modelle)
            // bekommen kompakte Werkzeugschemas, damit die Grundlast die
            // 50-%-Prüfung nicht sprengt. Die Werkzeugauswahl ist bereits
            // auf die Rolle geschnitten (Aktivierung, s. o.).
            if window < COMPACT_TOOL_SCHEMA_WINDOW_TOKENS {
                child_session.set_compact_tool_schemas(true);
                tracing::info!(
                    child = %child,
                    role = role_name,
                    window_tokens = window,
                    "child_admission.compact_tool_schemas"
                );
            }
            let calibration = *child_session.token_calibration();
            let tool_tokens = crate::turn_loop::collect_tools(child_session)
                .ok()
                .and_then(|tools| serde_json::to_vec(&tools).ok())
                .map_or(0, |bytes| {
                    calibration.bytes_to_tokens(u64::try_from(bytes.len()).unwrap_or(u64::MAX))
                });
            let base_tokens = tool_tokens.saturating_add(CHILD_FIXED_OVERHEAD_TOKENS);
            let limit_tokens = window.saturating_mul(CHILD_BASE_LOAD_MAX_WINDOW_PERCENT) / 100;
            let room_tokens = limit_tokens.saturating_sub(base_tokens);
            if room_tokens < CHILD_MIN_TASK_TOKENS {
                let overload = ChildContextOverload {
                    role: role_name.to_owned(),
                    model: model.clone(),
                    window_tokens: window,
                    base_tokens,
                    limit_tokens,
                };
                tracing::warn!(child = %child, %overload, "child_admission.context_overload");
                let _ = manager.remove(&child);
                return Err(AdmitRejection::Other(Self::reject(overload.to_string())));
            }
            let task_tokens = room_tokens
                .min(window.saturating_mul(CHILD_TASK_MAX_WINDOW_PERCENT) / 100)
                .max(CHILD_MIN_TASK_TOKENS);
            task_max_bytes = Some(task_tokens_to_bytes(task_tokens, &calibration));
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
            // Runde 5, Teil C: Beobachter der Fabrik (Diary des Kindes) —
            // nur gesetzte Slots, sonst bleibt die Kind-Sitzung unverändert.
            let observers = definition
                .registry_factory
                .child_session_observers(role_name, &child);
            let child_session = match observers.compaction {
                Some(observer) => child_session.with_compaction_observer(Some(observer)),
                None => child_session,
            };
            // Runde 5, Teil M: jede Kind-Sitzung schreibt ihr
            // Aktivitätsjournal; der Beobachter der Fabrik bleibt dahinter.
            let child_session =
                child_session.with_tool_outcome_observer(Some(Arc::new(JournalToolObserver {
                    comms: Arc::clone(&self.comms),
                    inner: observers.tool_outcome,
                })));
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
        let mut pending_task = if recovery.is_some() {
            None
        } else {
            spawn_task_text(input.instructions.as_deref(), &input.context)
        };
        // Teil C: Auftragsdeckel. Ein übergroßer Auftrag wird in der Mitte
        // gekürzt (mit Markierung), statt das Kind später am Kontextlimit
        // scheitern zu lassen.
        if let (Some(max_bytes), Some(text)) = (task_max_bytes, pending_task.as_deref())
            && text.len() > max_bytes
        {
            let original = text.len();
            pending_task = Some(cap_task_text(text, max_bytes));
            tracing::warn!(
                child = %child,
                role = role_name,
                original_bytes = original,
                max_bytes,
                "child_admission.task_truncated"
            );
            admission_warnings.push(format!(
                "Auftrag gekürzt: {original} Bytes > {max_bytes} Bytes Anteil am Kontextfenster"
            ));
        }
        let task = pending_task.as_deref().and_then(orchestration_detail_head);
        let admission_warning =
            (!admission_warnings.is_empty()).then(|| admission_warnings.join("; "));
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
            provider: child_provider,
            consumed: ChildUsage::default(),
            charged_to_parent: ChildUsage::default(),
        };
        if let Some(lease_store) = &self.lease_store {
            let durable = record.durable_lease();
            let persisted = match recovery {
                Some(_) => lease_store.recover(&durable),
                None => lease_store.admit(&durable),
            };
            if let Err(error) = persisted {
                let _ = manager.remove(&child);
                return Err(AdmitRejection::Other(Self::reject(format!(
                    "could not durably persist child lease: {error}"
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
        // Runde 5, Teil M: Aktivitätsjournal ab der Admission (Auftrag,
        // Start beim Journal des Elternteils). Der Comms-Lock ist ein Blatt.
        self.comms.open_journal(
            &child,
            &event_record.parent,
            role_name,
            pending_task.as_deref(),
        );
        match self.child_tasks.lock() {
            Ok(mut tasks) => {
                tasks.insert(
                    child.as_str().to_owned(),
                    ChildTaskState {
                        pending_task,
                        task: task.clone(),
                        outcome_detail: None,
                        admission_warning,
                        task_max_bytes,
                        continuation: None,
                        backend_resume: None,
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
        // Runde 5, Teil J: `continue_from` einmal vor dem Warten prüfen,
        // nach der Admission binden.
        let mut input = input;
        let seed = self.requested_continuation(role_name, &mut input)?;
        let child = self
            .admit_or_wait_inner(role_name, input, sandbox, suggestions, max_wait, cancel)
            .await?;
        self.bind_requested_continuation(child, seed.as_ref())
    }

    /// Kern von [`Self::admit_or_wait`] ohne Fortsetzungs-Behandlung.
    async fn admit_or_wait_inner(
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
                None,
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
        // Runde 5, Teil K: ein abgekoppeltes Kind läuft weiter; es wird erst
        // von `finish_background_child` freigegeben.
        if self.background.is_detached(child) {
            tracing::debug!(child = %child, "child_finished.background_child_kept");
            return;
        }
        self.close_child(child);
    }

    fn child_completed(
        &self,
        child: &SessionId,
        completed_at: Timestamp,
    ) -> Result<(), AgentSpawnError> {
        // Runde 5, Teil K: siehe `child_finished`.
        if self.background.is_detached(child) {
            tracing::debug!(child = %child, "child_completed.background_child_kept");
            return Ok(());
        }
        self.close_child_durable(child, completed_at)
    }

    fn delegation_target_names(&self, parent_session_id: &SessionId) -> Vec<String> {
        // Plan R9, Teil C: früher `unwrap_or_default()` — ein fehlender
        // Spawn-Kontext verschwand dabei spurlos in einer leeren Liste. Die
        // Liste bleibt leer, aber der benannte Grund landet im Log; wer den
        // Grund dem Modell zeigen muss, nutzt `delegation_targets`.
        match self.delegation_targets_for(parent_session_id, false) {
            Ok(targets) => targets.names(),
            Err(harw_extension_api::DelegationUnavailable::DepthExhausted) => Vec::new(),
            Err(error) => {
                tracing::warn!(
                    caller = %parent_session_id,
                    reason = %error,
                    "child_spawner.delegation_targets_unavailable"
                );
                Vec::new()
            }
        }
    }

    fn delegation_targets(
        &self,
        parent_session_id: &SessionId,
        plan_mode: bool,
    ) -> Result<harw_extension_api::DelegationTargets, harw_extension_api::DelegationUnavailable>
    {
        self.delegation_targets_for(parent_session_id, plan_mode)
    }

    fn note_caller_mode(&self, caller: &SessionId, plan_mode: bool) {
        // Nur die extern gefahrene Wurzel braucht die Meldung: jede andere
        // Sitzung liegt im Manager (oder als Eltern-Sicht vor) und trägt
        // ihren Modus selbst.
        if self
            .external_root_parent
            .as_ref()
            .is_some_and(|root| &root.session_id == caller)
        {
            self.external_root_plan_mode
                .store(plan_mode, std::sync::atomic::Ordering::Release);
        }
    }

    // Runde 5, Teil K.
    fn child_runs_in_background(&self, child: &SessionId) -> bool {
        self.background.is_detached(child)
    }

    fn resumable_child_role(&self, caller: &SessionId, child_id: &str) -> Option<String> {
        ManagedAgentSpawner::resumable_child_role(self, caller, child_id)
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

    // Runde 5, Teil O: Kind-Freigaben, Lease-Herzschlag, Hintergrund-Token
    // (`child_controller/tests/teil_o.rs`).
    mod teil_o;

    // Runde 9, E7: Kinder eines Kindes mit laufendem Turn
    // (`child_controller/tests/running_parent.rs`).
    mod running_parent;

    // Runde 9, E6: Live-Moduswechsel erreichen laufende Kinder
    // (`child_controller/tests/live_mode.rs`).
    mod live_mode;

    // Plan R9, Teil C/E1: Sichtbarkeit mit Plan-Modus-Regel, benannte
    // Gründe, Eltern-Schnitt gegen die Basis
    // (`child_controller/tests/plan_delegation.rs`).
    mod plan_delegation;

    // Welle 6: ein über den `ChildBackend` laufendes Kind bekommt seine
    // in-process Rechte (`child_controller/tests/child_backend_run.rs`).
    mod child_backend_run;

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
    fn test_cap_child_return_text_keeps_the_report_conclusion() {
        // Bugreport Runde 5: ein Bericht, dessen Mitte gekürzt wird, muss
        // Fazit und Empfehlungen am Schluss vollständig behalten.
        let mut report = String::from("# Bericht\n");
        for index in 0..400 {
            report.push_str(&format!("Befund {index}: Detailzeile mit Belegen.\n"));
        }
        report.push_str("## Fazit\nDie Testlandschaft ist lückenhaft.\n");
        report.push_str("## Empfehlungen\n1. Integrationstests ergänzen.\n");
        let capped = cap_child_return_text(&report, 4096);
        assert!(capped.starts_with("# Bericht\n"));
        assert!(capped.contains("## Fazit\nDie Testlandschaft ist lückenhaft.\n"));
        assert!(capped.ends_with("## Empfehlungen\n1. Integrationstests ergänzen.\n"));
        assert!(capped.contains("Anfang und Schluss vollständig"));
        // Beide Schnittstellen liegen auf Zeilengrenzen: die Markierung
        // folgt auf einen Zeilenumbruch und endet mit einem, dahinter
        // beginnt eine vollständige Befundzeile.
        let marker_start = capped.find("\n[…").unwrap_or(0);
        assert!(marker_start > 0, "Markierung fehlt: {capped}");
        assert!(
            capped[..marker_start].ends_with(".\n"),
            "Kopf endet mitten in einer Zeile"
        );
        let after_marker = capped[marker_start..]
            .split_once("…]\n")
            .map(|(_, rest)| rest)
            .unwrap_or_default();
        assert!(
            after_marker.starts_with("Befund "),
            "Ende beginnt mitten in einer Zeile: {after_marker}"
        );
    }

    #[test]
    fn test_cap_child_return_text_split_gives_the_tail_half_the_budget() {
        let text = format!("{}{}", "k".repeat(1000), "e".repeat(1000));
        let capped = cap_child_return_text(&text, 200);
        let tail = capped
            .rsplit_once("…]\n")
            .map(|(_, tail)| tail)
            .unwrap_or_default();
        assert_eq!(
            tail.len(),
            100,
            "das Ende muss die Hälfte des Budgets bekommen"
        );
    }

    #[test]
    fn test_child_return_budget_fits_a_typical_orchestrator_report() {
        // Der Bericht aus dem Bugreport war ~17.6 KB lang und verlor 9475
        // Bytes. Er muss jetzt ungekürzt durchgehen, und die Grenze muss
        // unter der Werkzeugergebnis-Grenze der Kind-Turns bleiben.
        let report = "x".repeat(17_667);
        assert_eq!(
            cap_child_return_text(&report, CHILD_RETURN_MAX_BYTES),
            report
        );
        const { assert!(CHILD_RETURN_MAX_BYTES < CHILD_TOOL_RESULT_MAX_BYTES) };
    }

    #[test]
    fn test_cap_child_return_text_reports_omitted_byte_count() {
        let text = "b".repeat(500);
        let capped = cap_child_return_text(&text, 50);
        assert!(
            capped.contains("450 Bytes der Kind-Antwort gekürzt"),
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
            model: None,
            provider: None,
            consumed: ChildUsage::default(),
            charged_to_parent: ChildUsage::default(),
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

    /// Runde 5, Teil K: ein abgekoppeltes Kind überlebt `child_finished`/
    /// `child_completed` seines Elternteils und wird erst von
    /// `finish_background_child` freigegeben — mit Benachrichtigung.
    #[test]
    fn a_background_child_survives_child_finished_until_it_is_finished() -> TestResult {
        let (spawner, child) = spawner_with_admitted_child()?;
        let run = spawner
            .detach_for_background(&child, Some("Auftrag"))
            .map_err(ctx("das admittierte Kind lässt sich abkoppeln"))?;
        assert_eq!(run.child, child);
        assert!(spawner.detach_for_background(&child, None).is_err());

        AgentSpawner::child_finished(&spawner, &child);
        assert!(
            spawner.child_record(&child).is_some(),
            "child_finished schließt es nicht"
        );
        AgentSpawner::child_completed(&spawner, &child, Timestamp::now()).map_err(ctx(
            "child_completed ist für ein abgekoppeltes Kind ein No-op",
        ))?;
        assert!(spawner.child_record(&child).is_some());

        let notice = spawner.finish_background_child(
            &child,
            crate::background_children::BackgroundStatus::Completed,
            "fertig".to_owned(),
        );
        assert!(notice.is_some());
        assert!(
            spawner.child_record(&child).is_none(),
            "erst jetzt freigegeben"
        );
        assert_eq!(
            spawner
                .background_children()
                .take_notices(&run.parent)
                .len(),
            1
        );
        Ok(())
    }

    /// Runde 5, Teil K: Spawner mit UIA-Wurzel (extern) und den Rollen
    /// `root-orchestrator`, `uia-worker`, `worker`.
    fn uia_spawner(
        limits: Option<crate::background_children::OrchestrationLimits>,
    ) -> TestResult<(ManagedAgentSpawner, SessionId, SandboxSpec)> {
        use harw_agent_dsl::roles::AgentRoleId;
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let uia = SessionId::new();
        let role = |name: &str| AgentRole::Agent {
            name: name.to_owned(),
        };
        let spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative())
            .with_role(
                "root-orchestrator",
                role("root-orchestrator"),
                AgentRoleId::RootOrchestrator,
                Arc::new(EmptyChildRegistry),
            )
            .with_role(
                "uia-worker",
                role("uia-worker"),
                AgentRoleId::UiaWorker,
                Arc::new(EmptyChildRegistry),
            )
            .with_role(
                "worker",
                role("worker"),
                AgentRoleId::Worker,
                Arc::new(EmptyChildRegistry),
            );
        let spawner = match limits {
            Some(limits) => spawner.with_orchestration_limits(limits),
            None => spawner,
        }
        .with_external_root_parent(
            uia.clone(),
            external_root_context(sandbox.clone(), AgentRoleId::UserInterface),
            None,
            SessionActivation::default(),
        )
        .map_err(ctx("die UIA-Wurzel registriert sich"))?;
        Ok((spawner, uia, sandbox))
    }

    /// Runde 5, Teil K: die UIA darf höchstens einen Root-Orchestrator
    /// gleichzeitig laufen lassen (Vorgabe); UIA-Worker bleiben erlaubt,
    /// Orchestrator-interne Spawns sind unberührt, nach dem Ende geht es
    /// wieder.
    #[test]
    fn the_spawner_admits_only_one_root_orchestrator_for_the_uia() -> TestResult {
        let (spawner, uia, sandbox) = uia_spawner(None)?;
        let first = spawner
            .admit(
                "root-orchestrator",
                spawn_input(uia.clone()),
                sandbox.clone(),
                None,
            )
            .map_err(ctx("erster Root-Orchestrator"))?;
        match spawner.admit(
            "root-orchestrator",
            spawn_input(uia.clone()),
            sandbox.clone(),
            None,
        ) {
            Err(error) => {
                assert!(
                    crate::background_children::is_orchestration_limit_rejection(&error.message),
                    "{}",
                    error.message
                );
                assert!(error.message.contains(first.as_str()));
            }
            Ok(child) => {
                return Err(TestError::Unexpected(format!(
                    "zweiter Root-Orchestrator {child} wurde zugelassen"
                )));
            }
        }
        let helper = spawner
            .admit(
                "uia-worker",
                spawn_input(uia.clone()),
                sandbox.clone(),
                None,
            )
            .map_err(ctx("ein UIA-Worker bleibt erlaubt"))?;
        let internal = spawner
            .admit("worker", spawn_input(first.clone()), sandbox.clone(), None)
            .map_err(ctx("Orchestrator-interne Spawns sind unberührt"))?;

        spawner.close_child(&internal);
        spawner.close_child(&helper);
        spawner.close_child(&first);
        spawner
            .admit("root-orchestrator", spawn_input(uia.clone()), sandbox, None)
            .map_err(ctx("nach dem Ende ist ein neuer Root-Orchestrator erlaubt"))?;
        Ok(())
    }

    /// Runde 5, Teil K: `[agents] max_root_orchestrators = 2` lässt genau zwei
    /// zu; auch ein abgekoppelter (Hintergrund-)Lauf zählt.
    #[test]
    fn a_root_limit_of_two_admits_two_and_counts_background_runs() -> TestResult {
        let limits = crate::background_children::OrchestrationLimits {
            max_root_orchestrators: 2,
            ..crate::background_children::OrchestrationLimits::default()
        };
        let (spawner, uia, sandbox) = uia_spawner(Some(limits))?;
        let first = spawner
            .admit(
                "root-orchestrator",
                spawn_input(uia.clone()),
                sandbox.clone(),
                None,
            )
            .map_err(ctx("erster"))?;
        spawner
            .detach_for_background(&first, None)
            .map_err(ctx("erster läuft im Hintergrund"))?;
        spawner
            .admit(
                "root-orchestrator",
                spawn_input(uia.clone()),
                sandbox.clone(),
                None,
            )
            .map_err(ctx("zweiter"))?;
        assert!(
            spawner
                .admit("root-orchestrator", spawn_input(uia), sandbox, None)
                .is_err(),
            "der dritte überschreitet die Grenze"
        );
        Ok(())
    }

    /// Runde 5, Teil K: nur der eigene Elternteil darf einen Hintergrund-Lauf
    /// abbrechen; eine fremde Sitzung bekommt dieselbe Meldung wie für eine
    /// unbekannte ID.
    #[test]
    fn a_background_child_is_cancelled_only_by_its_own_parent() -> TestResult {
        let (spawner, child) = spawner_with_admitted_child()?;
        let run = spawner
            .detach_for_background(&child, None)
            .map_err(ctx("abkoppeln"))?;
        let foreign = spawner.cancel_background_child(&SessionId::new(), child.as_str());
        let unknown = spawner.cancel_background_child(&run.parent, "gibt-es-nicht");
        match (foreign, unknown) {
            (Err(foreign), Err(unknown)) => {
                assert_eq!(
                    foreign.message.replace(child.as_str(), "<id>"),
                    unknown.message.replace("gibt-es-nicht", "<id>")
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "fremd/unbekannt muss abgelehnt werden: {other:?}"
                )));
            }
        }
        spawner
            .cancel_background_child(&run.parent, child.as_str())
            .map_err(ctx("der eigene Elternteil darf abbrechen"))?;
        let cancelled = spawner.cancel_all_background(&run.parent, "test");
        assert_eq!(cancelled.len(), 1);
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

    // --- Runde 5, Teil C: Fabrik-Beobachter an der Kind-Sitzung ---

    /// Zählt Beobachter-Anfragen und Freigaben je Rolle.
    #[derive(Default)]
    struct ObservingChildRegistry {
        observed: Mutex<Vec<(String, SessionId)>>,
        released: Mutex<Vec<(String, SessionId)>>,
    }

    struct NoopChildObserver;

    impl crate::compaction::CompactionObserver for NoopChildObserver {
        fn on_compacted(&self, _: &SessionId, _: &crate::compaction::CompactionOutcome) {}
    }

    impl crate::capture::ToolOutcomeObserver for NoopChildObserver {
        fn on_tool_outcome(&self, _: &SessionId, _: &crate::capture::ToolOutcome<'_>) {}
    }

    impl ChildRegistryFactory for ObservingChildRegistry {
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

        fn child_session_observers(&self, role: &str, child: &SessionId) -> ChildSessionObservers {
            self.observed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((role.to_owned(), child.clone()));
            ChildSessionObservers {
                compaction: Some(Arc::new(NoopChildObserver)),
                tool_outcome: Some(Arc::new(NoopChildObserver)),
            }
        }

        fn child_session_released(&self, role: &str, child: &SessionId) {
            self.released
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((role.to_owned(), child.clone()));
        }
    }

    #[test]
    fn factory_observers_reach_the_child_session_and_release_is_reported_once() -> TestResult {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let parent = SessionId::new();
        let factory = Arc::new(ObservingChildRegistry::default());
        let spawner = ManagedAgentSpawner::new(manager.clone(), ChildLimits::conservative())
            .with_role(
                "worker",
                AgentRole::Agent {
                    name: "worker".to_owned(),
                },
                harw_agent_dsl::roles::AgentRoleId::Worker,
                factory.clone(),
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

        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .map_err(ctx("child is admitted"))?;
        {
            let manager = manager
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let session = manager.get(&child).map_err(ctx("child is manager-owned"))?;
            assert!(session.compaction_observer().is_some());
            assert!(session.tool_outcome_observer().is_some());
        }
        assert_eq!(
            factory
                .observed
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone(),
            vec![("worker".to_owned(), child.clone())]
        );

        spawner.close_child(&child);
        spawner.close_child(&child);
        assert_eq!(
            factory
                .released
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone(),
            vec![("worker".to_owned(), child)]
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

    // --- UIA-Freigabeliste (`with_uia_spawnable_roles`) ---------------------

    /// Spawner mit den Worker-Rollen `matrix-player` (gelistet) und
    /// `worker` (nicht gelistet) unter einer `UserInterface`-Wurzel.
    fn uia_allowlist_spawner(
        manager: Arc<Mutex<SessionManager>>,
        parent: SessionId,
        sandbox: SandboxSpec,
    ) -> TestResult<ManagedAgentSpawner> {
        worker_spawner(manager)
            .with_role(
                "matrix-player",
                AgentRole::Agent {
                    name: "matrix-player".to_owned(),
                },
                harw_agent_dsl::roles::AgentRoleId::Worker,
                Arc::new(EmptyChildRegistry),
            )
            .with_uia_spawnable_roles(["matrix-player".to_owned()])
            .with_external_root_parent(
                parent,
                external_root_context(sandbox, harw_agent_dsl::roles::AgentRoleId::UserInterface),
                None,
                SessionActivation::default(),
            )
            .map_err(ctx("trusted external root registers during construction"))
    }

    #[test]
    fn uia_allowlist_admits_a_listed_worker_role() -> TestResult {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let parent = SessionId::new();
        let spawner = uia_allowlist_spawner(manager, parent.clone(), sandbox.clone())?;

        let child = spawner
            .admit("matrix-player", spawn_input(parent.clone()), sandbox, None)
            .map_err(ctx(
                "listed read-only worker role is admitted for a UIA parent",
            ))?;

        let record = spawner
            .child_record(&child)
            .ok_or(TestError::Missing("child is tracked"))?;
        assert_eq!(record.parent, parent);
        assert_eq!(record.role, "matrix-player");
        Ok(())
    }

    #[test]
    fn uia_allowlist_still_refuses_an_unlisted_worker_role() -> TestResult {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let sandbox = test_sandbox(PermissionSet::from_policy([Permission::ReadWorkspace]))?;
        let parent = SessionId::new();
        let spawner = uia_allowlist_spawner(manager, parent.clone(), sandbox.clone())?;

        let Err(error) = spawner.admit("worker", spawn_input(parent), sandbox, None) else {
            return Err(TestError::Unexpected(
                "an unlisted worker role must stay refused for a UIA parent".to_owned(),
            ));
        };

        assert_eq!(
            error.message,
            "no delegation capability is available for this request"
        );
        Ok(())
    }

    #[test]
    fn uia_allowlist_leaves_other_role_combinations_unchanged() {
        use harw_agent_dsl::roles::AgentRoleId;
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative())
            .with_uia_spawnable_roles(["matrix-player".to_owned()]);
        let all = [
            AgentRoleId::RootOrchestrator,
            AgentRoleId::ChildOrchestrator,
            AgentRoleId::UserInterface,
            AgentRoleId::Worker,
            AgentRoleId::UiaWorker,
            AgentRoleId::AgentSteward,
        ];
        let allowed_orchestrators = ["matrix-player".to_owned()];

        for caller in all {
            for target in all {
                for name in ["matrix-player", "worker"] {
                    let listed_uia_worker = caller == AgentRoleId::UserInterface
                        && target == AgentRoleId::Worker
                        && name == "matrix-player";
                    let expected = listed_uia_worker
                        || can_delegate_to(caller, target, name, &allowed_orchestrators);
                    assert_eq!(
                        spawner.spawn_permitted(caller, target, name, &allowed_orchestrators),
                        expected,
                        "{caller:?} -> {target:?} ({name})"
                    );
                }
            }
        }
        // Die Ausnahme ist wirklich eine Ausnahme: ohne Liste bleibt
        // UserInterface -> Worker verboten.
        assert!(!can_delegate_to(
            AgentRoleId::UserInterface,
            AgentRoleId::Worker,
            "matrix-player",
            &[],
        ));
        // Ein Worker-Elternteil gewinnt durch die Liste nichts.
        assert!(!spawner.spawn_permitted(
            AgentRoleId::Worker,
            AgentRoleId::Worker,
            "matrix-player",
            &[],
        ));
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
                        model: None,
                        provider: None,
                        consumed: ChildUsage::default(),
                        charged_to_parent: ChildUsage::default(),
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
                        model: None,
                        provider: None,
                        consumed: ChildUsage::default(),
                        charged_to_parent: ChildUsage::default(),
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
            matches!(error, ChildRunError::BudgetExhausted(_)),
            "a wall-time stop is a typed budget finding, not a spawn error"
        );
        assert!(
            error
                .message()
                .starts_with("budget_exceeded: wall_time (limit=25, used="),
            "unexpected message: {}",
            error.message()
        );
        assert!(
            !error.to_string().contains("agent spawn failed"),
            "a runtime budget stop must not read like a spawn failure: {error}"
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
            error.message(),
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
            model: None,
            provider: None,
            consumed: ChildUsage::default(),
            charged_to_parent: ChildUsage::default(),
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
                    model: None,
                    provider: None,
                    consumed: ChildUsage::default(),
                    charged_to_parent: ChildUsage::default(),
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
                            ContentPart::ImageUrl { .. } | ContentPart::Media { .. } => None,
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
                    admission_warning: None,
                    task_max_bytes: None,
                    continuation: None,
                    backend_resume: None,
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

    // --- Runde 5, Teil H: `agent.result` ------------------------------------

    #[test]
    fn test_cap_marker_names_agent_result_with_the_child_id() {
        let child = SessionId::new();
        let capped = cap_child_return_text_for_child(&"z".repeat(500), 100, &child);
        assert!(capped.contains("Anfang und Schluss vollständig"));
        assert!(capped.contains(AGENT_RESULT_TOOL), "{capped}");
        assert!(
            capped.contains(&format!("{{\"child_id\":\"{child}\"}}")),
            "{capped}"
        );
        // Innerhalb des Budgets: unverändert, ohne Hinweis.
        assert_eq!(cap_child_return_text_for_child("kurz", 100, &child), "kurz");
        // Die Variante ohne Kind nennt kein Werkzeug.
        assert!(!cap_child_return_text(&"z".repeat(500), 100).contains(AGENT_RESULT_TOOL));
    }

    #[tokio::test]
    async fn child_result_text_returns_the_uncapped_text_only_to_the_parent() -> TestResult {
        let long: &'static str = Box::leak("y".repeat(CHILD_RETURN_MAX_BYTES * 2).into_boxed_str());
        let (spawner, children) = runnable_children(
            Arc::new(EchoChildRegistry { reply: long }),
            true,
            1,
            empty_registry,
        )?;
        let child = children[0].clone();
        let parent = spawner
            .child_record(&child)
            .map(|record| record.parent)
            .ok_or(TestError::Missing("admitted child has a parent"))?;
        let store = InMemoryStateStore::new();
        spawner
            .run_child(&child, &store, TurnInput::user("liefere viel Text"))
            .await
            .map_err(ctx("child completes"))?;

        // Der gekürzte Rückgabetext verweist auf `agent.result`.
        let capped = spawner
            .child_final_assistant_text(&child)
            .map_err(ctx("capped text is available"))?;
        assert!(capped.contains(AGENT_RESULT_TOOL), "{capped}");
        assert!(capped.contains(child.as_str()), "{capped}");

        // Der Elternteil bekommt den ungekürzten Text — auch nach der Freigabe.
        assert_eq!(
            spawner
                .child_result_text(&parent, &child)
                .map_err(ctx("parent reads its child"))?,
            long
        );
        spawner
            .release_child(&child)
            .map_err(ctx("child releases"))?;
        assert_eq!(
            spawner
                .child_result_text(&parent, &child)
                .map_err(ctx("archived text survives the release"))?,
            long
        );

        // Fremde Aufrufer und unbekannte Kinder: dieselbe Ablehnung.
        let stranger = SessionId::new();
        let foreign = spawner
            .child_result_text(&stranger, &child)
            .err()
            .ok_or(TestError::Missing("a foreign caller is refused"))?;
        let unknown = spawner
            .child_result_text(&parent, &SessionId::new())
            .err()
            .ok_or(TestError::Missing("an unknown child is refused"))?;
        assert!(
            foreign
                .message
                .contains("kein abgeschlossenes eigenes Kind")
        );
        assert!(
            unknown
                .message
                .contains("kein abgeschlossenes eigenes Kind")
        );
        Ok(())
    }

    #[test]
    fn child_result_archive_is_bounded_and_keeps_the_newest_entries() {
        let (events, _receiver) = mpsc::unbounded_channel();
        let manager = Arc::new(Mutex::new(SessionManager::new(events)));
        let spawner = ManagedAgentSpawner::new(manager, ChildLimits::conservative());
        let parent = SessionId::new();
        let children: Vec<SessionId> = (0..CHILD_RESULT_ARCHIVE_MAX_ENTRIES + 3)
            .map(|_| SessionId::new())
            .collect();
        for (index, child) in children.iter().enumerate() {
            spawner.archive_child_result(child, &parent, &format!("Ergebnis {index}"));
        }
        let archived = spawner
            .child_results
            .lock()
            .map(|archive| archive.len())
            .unwrap_or_default();
        assert_eq!(archived, CHILD_RESULT_ARCHIVE_MAX_ENTRIES);
        assert!(spawner.child_result_text(&parent, &children[0]).is_err());
        let newest = children.len() - 1;
        assert_eq!(
            spawner
                .child_result_text(&parent, &children[newest])
                .ok()
                .as_deref(),
            Some(format!("Ergebnis {newest}").as_str())
        );
        // Ein erneuter Lauf desselben Kindes ersetzt den Eintrag.
        spawner.archive_child_result(&children[newest], &parent, "neu");
        assert_eq!(
            spawner
                .child_result_text(&parent, &children[newest])
                .ok()
                .as_deref(),
            Some("neu")
        );
        // Ein Text über der Gesamtgrenze wird nicht archiviert.
        let huge = SessionId::new();
        spawner.archive_child_result(
            &huge,
            &parent,
            &"h".repeat(CHILD_RESULT_ARCHIVE_MAX_BYTES + 1),
        );
        assert!(spawner.child_result_text(&parent, &huge).is_err());
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

    // --- Budget-Anrechnung an den Elternteil --------------------------------

    fn edit_record(
        spawner: &ManagedAgentSpawner,
        child: &SessionId,
        edit: impl FnOnce(&mut ChildRecord),
    ) -> TestResult {
        let mut active = spawner
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let record = active
            .get_mut(child.as_str())
            .ok_or(TestError::Missing("child record exists"))?;
        edit(record);
        Ok(())
    }

    fn consumed_of(spawner: &ManagedAgentSpawner, child: &SessionId) -> TestResult<ChildUsage> {
        spawner
            .child_record(child)
            .map(|record| record.consumed)
            .ok_or(TestError::Missing("child record exists"))
    }

    #[test]
    fn remaining_budget_subtracts_consumption_and_keeps_unset_dimensions() -> TestResult {
        let (spawner, children) = runnable_children(
            Arc::new(EchoChildRegistry { reply: "done" }),
            true,
            1,
            empty_registry,
        )?;
        let child = children[0].clone();
        edit_record(&spawner, &child, |record| {
            record.budget = AgentBudget {
                max_tokens: Some(100),
                max_wall_time_ms: Some(50),
                ..AgentBudget::default()
            };
            record.consumed = ChildUsage {
                tokens: 30,
                tool_calls: 7,
                wall_time_ms: 80,
            };
        })?;

        let remaining = spawner
            .remaining_budget(&child)
            .ok_or(TestError::Missing("admitted child has a remaining budget"))?;
        assert_eq!(remaining.max_tokens, Some(70));
        assert_eq!(remaining.max_tool_calls, None, "unset stays unset");
        assert_eq!(remaining.max_wall_time_ms, Some(0), "saturates at zero");
        assert_eq!(spawner.remaining_budget(&SessionId::new()), None);
        Ok(())
    }

    #[tokio::test]
    async fn a_finished_run_reports_its_usage_and_charges_the_direct_parent() -> TestResult {
        let (spawner, children) = runnable_children(
            Arc::new(EchoChildRegistry { reply: "done" }),
            true,
            2,
            empty_registry,
        )?;
        let (parent, child) = (children[0].clone(), children[1].clone());
        edit_record(&spawner, &child, |record| record.parent = parent.clone())?;
        let store = InMemoryStateStore::new();

        let result = spawner
            .run_child_with_budget(
                &child,
                &store,
                None,
                TurnInput::user("work"),
                AgentBudget::default(),
            )
            .await
            .map_err(ctx("child run completes"))?;

        assert_eq!(consumed_of(&spawner, &child)?, result.usage);
        assert_eq!(consumed_of(&spawner, &parent)?, result.usage);
        Ok(())
    }

    #[test]
    fn usage_propagates_one_level_at_a_time_without_double_counting() -> TestResult {
        let (spawner, children) = runnable_children(
            Arc::new(EchoChildRegistry { reply: "done" }),
            true,
            3,
            empty_registry,
        )?;
        let (top, middle, leaf) = (
            children[0].clone(),
            children[1].clone(),
            children[2].clone(),
        );
        edit_record(&spawner, &middle, |record| record.parent = top.clone())?;
        edit_record(&spawner, &leaf, |record| record.parent = middle.clone())?;

        let run = ChildUsage {
            tokens: 10,
            tool_calls: 2,
            wall_time_ms: 5,
        };
        spawner.charge_run_usage(&leaf, run);
        assert_eq!(consumed_of(&spawner, &leaf)?, run);
        assert_eq!(consumed_of(&spawner, &middle)?, run);
        assert_eq!(
            consumed_of(&spawner, &top)?,
            ChildUsage::default(),
            "only the direct parent is charged"
        );

        spawner.charge_run_usage(&middle, ChildUsage::default());
        assert_eq!(consumed_of(&spawner, &top)?, run);
        // Ein zweiter Lauf ohne neuen Verbrauch verbucht nichts doppelt.
        spawner.charge_run_usage(&middle, ChildUsage::default());
        assert_eq!(consumed_of(&spawner, &top)?, run);

        // Nachträglicher Verbrauch des Blatts kommt über `close_child` nach oben.
        edit_record(&spawner, &leaf, |record| {
            record.consumed = record.consumed.saturating_add(ChildUsage {
                tokens: 5,
                ..ChildUsage::default()
            });
        })?;
        spawner.close_child(&leaf);
        assert_eq!(consumed_of(&spawner, &middle)?.tokens, 15);
        spawner.close_child(&middle);
        assert_eq!(consumed_of(&spawner, &top)?.tokens, 15);
        assert_eq!(consumed_of(&spawner, &top)?.tool_calls, 2);
        Ok(())
    }

    #[test]
    fn admission_caps_the_child_budget_by_the_parents_remainder() -> TestResult {
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
        .with_spawn_context(SpawnContext {
            sandbox: sandbox.clone(),
            suggestions: None,
            capability_snapshot: None,
            approval_actor: None,
            organizational_role: harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
            allowed_child_orchestrators: vec!["manager".to_owned()],
            trace: None,
            ceiling: None,
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
            .admit("manager", spawn_input(root.clone()), sandbox.clone(), None)
            .map_err(ctx("child is admitted"))?;
        assert_eq!(
            spawner.remaining_budget(&root),
            None,
            "a root has no record"
        );
        edit_record(&spawner, &child, |record| {
            record.budget = AgentBudget {
                max_tokens: Some(100),
                ..AgentBudget::default()
            };
            record.consumed = ChildUsage {
                tokens: 40,
                ..ChildUsage::default()
            };
        })?;

        let grandchild = spawner
            .admit("worker", spawn_input(child.clone()), sandbox.clone(), None)
            .map_err(ctx("grandchild is admitted inside the remainder"))?;
        let capped = spawner
            .child_budget(&grandchild)
            .ok_or(TestError::Missing("grandchild is tracked"))?;
        assert_eq!(capped.max_tokens, Some(60));
        assert_eq!(capped.max_tool_calls, None);

        edit_record(&spawner, &child, |record| {
            record.consumed.tokens = 100;
        })?;
        let Err(error) = spawner.admit("worker", spawn_input(child), sandbox, None) else {
            return Err(TestError::Unexpected(
                "an exhausted parent budget must reject the admission".to_owned(),
            ));
        };
        assert_eq!(
            error.message,
            "budget_exceeded: tokens (limit=100, used=100)"
        );
        Ok(())
    }

    // --- Teil C: Kind-Fenster, Auftragsdeckel, Token-Budget -----------------

    /// Factory ohne Pin, deren ungepinnter Provider ein festes Hauptmodell ruft.
    struct MainModelChildRegistry {
        model: &'static str,
    }

    impl ChildRegistryFactory for MainModelChildRegistry {
        fn build_registry(
            &self,
            _role: &str,
            _input: &SpawnInput,
            _suggestions: Option<&AgentSuggestions>,
        ) -> Result<ExtensionRegistry, AgentSpawnError> {
            Ok(ExtensionRegistryBuilder::default().build())
        }

        fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
            Ok(Arc::new(EchoModelProvider::new("ok")))
        }

        fn main_model_for_task(
            &self,
            _role: &str,
            _complexity: Option<TaskComplexity>,
        ) -> Option<String> {
            Some(self.model.to_owned())
        }
    }

    fn child_window(spawner: &ManagedAgentSpawner, child: &SessionId) -> TestResult<Option<u64>> {
        let manager = spawner
            .manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session = manager.get(child).map_err(ctx("child is manager-owned"))?;
        Ok(session
            .auto_compact()
            .map(crate::auto_compact::AutoCompactPolicy::context_window_tokens))
    }

    fn child_active_model(
        spawner: &ManagedAgentSpawner,
        child: &SessionId,
    ) -> TestResult<Option<String>> {
        let manager = spawner
            .manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session = manager.get(child).map_err(ctx("child is manager-owned"))?;
        Ok(session
            .active_model()
            .map(|model| model.as_str().to_owned()))
    }

    #[test]
    fn a_child_without_a_pinned_model_gets_the_parent_model_window() -> TestResult {
        let (spawner, parent, sandbox) = window_test_spawner(Arc::new(EmptyChildRegistry), None)?;
        let spawner = spawner.with_root_model(Some("big-model".to_owned()));
        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .map_err(ctx("child is admitted"))?;

        assert_eq!(child_window(&spawner, &child)?, Some(1_000_000));
        assert_eq!(
            child_active_model(&spawner, &child)?.as_deref(),
            Some("big-model"),
            "das Kind ruft das Modell, dessen Fenster es bekommt"
        );
        let record = spawner
            .child_record(&child)
            .ok_or(TestError::Missing("child record exists"))?;
        assert_eq!(record.model.as_deref(), Some("big-model"));
        Ok(())
    }

    #[test]
    fn the_factory_main_model_beats_the_parent_model() -> TestResult {
        let (spawner, parent, sandbox) = window_test_spawner(
            Arc::new(MainModelChildRegistry {
                model: "small-model",
            }),
            None,
        )?;
        let spawner = spawner.with_root_model(Some("big-model".to_owned()));
        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .map_err(ctx("child is admitted"))?;
        assert_eq!(child_window(&spawner, &child)?, Some(32_000));
        assert_eq!(
            child_active_model(&spawner, &child)?.as_deref(),
            Some("small-model")
        );
        Ok(())
    }

    #[test]
    fn an_unknown_child_model_warns_and_falls_back_to_32k() -> TestResult {
        let (spawner, parent, sandbox) = window_test_spawner(Arc::new(EmptyChildRegistry), None)?;
        let observer = Arc::new(RecordingOrchestrationObserver::default());
        let spawner = spawner
            .with_root_model(Some("mystery-model".to_owned()))
            .with_context_window_resolver(Arc::new(|model: Option<&str>| match model {
                Some("mystery-model") | None => 32_768,
                _ => 200_000,
            }))
            .with_model_known_probe(Arc::new(|model: Option<&str>| {
                !matches!(model, Some("mystery-model") | None)
            }))
            .with_orchestration_observer(observer.clone());
        let child = spawner
            .admit("worker", spawn_input(parent), sandbox, None)
            .map_err(ctx("child is admitted"))?;

        assert_eq!(child_window(&spawner, &child)?, Some(32_768));
        let events = observer
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let admitted = events
            .iter()
            .find(|event| event.status == AgentOrchestrationStatus::Admitted)
            .ok_or(TestError::Missing("admitted event"))?;
        let detail = admitted
            .detail
            .as_deref()
            .ok_or(TestError::Missing("admitted event carries the warning"))?;
        assert!(detail.contains("mystery-model"), "{detail}");
        assert!(detail.contains("unbekannt"), "{detail}");
        assert!(detail.contains("32768"), "{detail}");
        Ok(())
    }

    #[test]
    fn an_oversized_task_is_truncated_instead_of_overflowing_the_window() -> TestResult {
        let (spawner, parent, sandbox) = window_test_spawner(Arc::new(EmptyChildRegistry), None)?;
        let observer = Arc::new(RecordingOrchestrationObserver::default());
        let spawner = spawner
            .with_root_model(Some("small-model".to_owned()))
            .with_orchestration_observer(observer.clone());
        let mut input = spawn_input(parent);
        let task = format!("ANFANG {} ENDE", "mitte ".repeat(40_000));
        input.instructions = Some(task.clone());
        let child = spawner
            .admit("worker", input, sandbox, None)
            .map_err(ctx("an oversized task does not block the admission"))?;

        let pending = spawner
            .child_task_state(&child)
            .and_then(|state| state.pending_task)
            .ok_or(TestError::Missing("pending task"))?;
        // 25 % von 32 000 Tokens bei 3 Bytes/Token.
        let cap = task_tokens_to_bytes(
            32_000 * CHILD_TASK_MAX_WINDOW_PERCENT / 100,
            &Default::default(),
        );
        assert!(pending.len() <= cap, "{} > {cap}", pending.len());
        assert!(pending.len() < task.len());
        assert!(pending.contains("gekürzt"));
        assert!(pending.contains("ANFANG"), "der Kopf bleibt");
        assert!(pending.contains("ENDE"), "das Ende bleibt");
        let detail = observer
            .events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .find_map(|event| event.detail.clone())
            .ok_or(TestError::Missing("admission warning"))?;
        assert!(detail.contains("Auftrag gekürzt"), "{detail}");
        Ok(())
    }

    /// Runde 7, Teil L9: ein Kind mit Fenster < 64k bekommt kompakte
    /// Werkzeugschemas, ein großes nicht.
    #[test]
    fn a_small_window_child_gets_compact_tool_schemas() -> TestResult {
        for (window, expected) in [(32_000_u64, true), (200_000_u64, false)] {
            let (spawner, parent, sandbox) =
                window_test_spawner(Arc::new(EmptyChildRegistry), None)?;
            let spawner =
                spawner.with_context_window_resolver(Arc::new(move |_model: Option<&str>| window));
            let child = spawner
                .admit("worker", spawn_input(parent), sandbox, None)
                .map_err(|error| TestError::Unexpected(error.message))?;
            let manager = spawner
                .manager
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let session = manager.get(&child).map_err(ctx("child is manager-owned"))?;
            assert_eq!(session.compact_tool_schemas(), expected, "window {window}");
        }
        Ok(())
    }

    #[test]
    fn a_base_load_beyond_half_the_window_is_a_typed_error_with_numbers() -> TestResult {
        let (spawner, parent, sandbox) = window_test_spawner(Arc::new(EmptyChildRegistry), None)?;
        let spawner = spawner.with_context_window_resolver(Arc::new(|_model: Option<&str>| 8_000));
        let Err(error) = spawner.admit("worker", spawn_input(parent), sandbox, None) else {
            return Err(TestError::Unexpected(
                "a window too small for system prompt and tools must reject".to_owned(),
            ));
        };
        assert!(
            error.message.starts_with("child_context_overload:"),
            "{}",
            error.message
        );
        assert!(
            error.message.contains("8000-token window"),
            "{}",
            error.message
        );
        assert!(error.message.contains("4000-token"), "{}", error.message);
        Ok(())
    }

    /// Modell, das jede Runde einen Zwischenstand plus Tool-Call liefert und
    /// meldet, ob die Anfrage die Abschluss-Anweisung enthielt. 1 000 Input
    /// (davon 900 gecacht) + 100 Output ⇒ 200 neue Tokens je Runde.
    struct BudgetProbeModel {
        rounds: AtomicUsize,
        wrap_up_seen: Mutex<Vec<bool>>,
    }

    impl ModelProvider for BudgetProbeModel {
        fn respond<'a>(&'a self, request: ModelRequest) -> ModelFuture<'a> {
            Box::pin(async move {
                let round = self.rounds.fetch_add(1, Ordering::SeqCst) + 1;
                let marker = "Token-Budget für diesen Auftrag";
                let seen = request.system_prompt.contains(marker)
                    || request
                        .instruction_fragments
                        .iter()
                        .any(|fragment| fragment.contains(marker))
                    || format!("{:?}", request.context).contains(marker);
                self.wrap_up_seen
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .push(seen);
                Ok(ModelResponse {
                    message: Some(format!("Zwischenstand {round}")),
                    tool_calls: vec![ToolCall {
                        id: ToolCallId::new(),
                        name: ToolName::new("fs.read"),
                        arguments: serde_json::json!({ "path": format!("/src/f{round}.rs") }),
                    }],
                    usage: TokenUsage {
                        input_tokens: 1_000,
                        output_tokens: 100,
                        cached_tokens: Some(900),
                        ..TokenUsage::default()
                    },
                    ..Default::default()
                })
            })
        }
    }

    struct BudgetProbeRegistry {
        model: Arc<BudgetProbeModel>,
    }

    impl ChildRegistryFactory for BudgetProbeRegistry {
        fn build_registry(
            &self,
            _role: &str,
            _input: &SpawnInput,
            _suggestions: Option<&AgentSuggestions>,
        ) -> Result<ExtensionRegistry, AgentSpawnError> {
            Ok(ExtensionRegistryBuilder::default().build())
        }

        fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
            Ok(self.model.clone())
        }
    }

    #[tokio::test]
    async fn an_exhausted_token_budget_returns_the_last_answer_as_partial_result() -> TestResult {
        let model = Arc::new(BudgetProbeModel {
            rounds: AtomicUsize::new(0),
            wrap_up_seen: Mutex::new(Vec::new()),
        });
        let (spawner, children) = runnable_children(
            Arc::new(BudgetProbeRegistry {
                model: model.clone(),
            }),
            true,
            1,
            empty_registry,
        )?;
        // Runde 5, Teil J: dieser Test prüft das Verhalten ohne Reserve und
        // ohne Verdichtung (Modus `LastAnswer`).
        let spawner =
            spawner.with_budget_handoff(crate::child_handoff::BudgetHandoffMode::LastAnswer);
        let child = children[0].clone();
        let store = InMemoryStateStore::new();

        let result = spawner
            .run_child_with_budget(
                &child,
                &store,
                None,
                TurnInput::user("erkunde"),
                AgentBudget {
                    max_tokens: Some(1_000),
                    ..AgentBudget::default()
                },
            )
            .await
            .map_err(|error| TestError::Unexpected(format!("partial result expected: {error}")))?;

        assert!(
            result.budget_exhausted,
            "outcome {:?}, rounds {}, usage {:?}",
            result.outcome,
            model.rounds.load(Ordering::SeqCst),
            result.usage
        );
        assert!(matches!(result.outcome, TurnOutcome::Completed));
        // Gecachte Eingabe zählt nicht: 200 neue Tokens je Runde ⇒ fünf
        // Runden (0, 200, 400, 600, 800 vor dem Aufruf), die sechste wird
        // beim Stand 1 000 verweigert. Mit dem alten Maß (1 100 je Runde)
        // wäre schon vor Runde 2 Schluss gewesen.
        assert_eq!(model.rounds.load(Ordering::SeqCst), 5);
        assert_eq!(result.full_text.as_deref(), Some("Zwischenstand 5"));
        let seen = model
            .wrap_up_seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert_eq!(
            seen,
            vec![false, false, false, false, true],
            "die Abschluss-Anweisung kommt ab 80 % (800 von 1 000)"
        );
        assert_eq!(result.usage.tokens, 1_000);
        assert_eq!(
            result.budget_handoff,
            Some(crate::child_handoff::BudgetHandoff::LastAnswer)
        );
        Ok(())
    }

    // --- Runde 7, Teil A7: Start überlebt einen geerbten Turn-Abbruch ---

    #[test]
    fn only_an_inherited_turn_abort_is_survived_by_a_background_start() {
        use CancelReason::{Budget, LeaseLost, Parent, Shutdown, User};
        assert!(detached_start_survives(Some(Parent), Some(User)));
        assert!(detached_start_survives(Some(Parent), Some(Budget)));
        assert!(detached_start_survives(Some(Parent), None));
        assert!(!detached_start_survives(Some(Parent), Some(Shutdown)));
        assert!(!detached_start_survives(Some(Parent), Some(LeaseLost)));
        // Ein Abbruch des Kindes selbst bleibt bestehen.
        assert!(!detached_start_survives(Some(User), None));
        assert!(!detached_start_survives(Some(Budget), None));
        assert!(!detached_start_survives(None, None));
    }

    // --- Runde 7, Teil A1: Budget auch für Transfers --------------------

    /// Setzt den bei der Admission hinterlegten Budget-Deckel eines
    /// Test-Kindes (wie ihn die Agent-IR liefern würde).
    fn set_declared_budget(spawner: &ManagedAgentSpawner, child: &SessionId, budget: AgentBudget) {
        if let Some(record) = spawner
            .active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get_mut(child.as_str())
        {
            record.budget = budget;
        }
    }

    #[tokio::test]
    async fn a_transfer_child_keeps_its_declared_token_budget_with_a_wrap_up_round() -> TestResult {
        let model = Arc::new(BudgetProbeModel {
            rounds: AtomicUsize::new(0),
            wrap_up_seen: Mutex::new(Vec::new()),
        });
        let (spawner, children) = runnable_children(
            Arc::new(BudgetProbeRegistry {
                model: model.clone(),
            }),
            true,
            1,
            empty_registry,
        )?;
        let spawner =
            spawner.with_budget_handoff(crate::child_handoff::BudgetHandoffMode::LastAnswer);
        let child = children[0].clone();
        set_declared_budget(
            &spawner,
            &child,
            AgentBudget {
                max_tokens: Some(1_000),
                ..AgentBudget::default()
            },
        );
        let store = InMemoryStateStore::new();

        let result = spawner
            .run_child_with_declared_budget(&child, &store, None, TurnInput::user("erkunde"))
            .await
            .map_err(|error| TestError::Unexpected(format!("partial result expected: {error}")))?;

        assert!(result.budget_exhausted, "the declared budget must apply");
        assert_eq!(model.rounds.load(Ordering::SeqCst), 5);
        let seen = model
            .wrap_up_seen
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert_eq!(seen.last(), Some(&true), "wrap-up instruction at 80 %");
        let text = result
            .budget_exhausted_parent_text()
            .ok_or(TestError::Missing("budget-ended text for the parent"))?;
        assert!(text.starts_with("[budget_exhausted: true]"), "{text}");
        assert!(text.contains("Zwischenstand 5"), "{text}");
        Ok(())
    }

    #[tokio::test]
    async fn a_transfer_child_keeps_its_declared_wall_time_budget() -> TestResult {
        let (spawner, children) =
            runnable_children(Arc::new(HangingChildRegistry), true, 1, empty_registry)?;
        let child = children[0].clone();
        set_declared_budget(
            &spawner,
            &child,
            AgentBudget {
                max_wall_time_ms: Some(25),
                ..AgentBudget::default()
            },
        );
        let store = InMemoryStateStore::new();

        let Err(error) = spawner
            .run_child_with_declared_budget(&child, &store, None, TurnInput::user("work"))
            .await
        else {
            return Err(TestError::Unexpected(
                "the declared wall-time budget must stop the transfer child".to_owned(),
            ));
        };
        assert!(matches!(error, ChildRunError::BudgetExhausted(_)));
        assert!(
            error.message().starts_with("budget_exceeded: wall_time"),
            "{}",
            error.message()
        );
        assert!(child_is_manager_owned(&spawner, &child));
        Ok(())
    }

    #[test]
    fn a_regular_run_has_no_budget_exhausted_parent_text() {
        let result = ChildRunResult {
            child: SessionId::new(),
            outcome: TurnOutcome::Completed,
            full_text: Some("fertig".to_owned()),
            usage: ChildUsage::default(),
            budget_exhausted: false,
            budget_handoff: None,
        };
        assert!(result.budget_exhausted_parent_text().is_none());
    }

    // --- Runde 5, Teil J: Übergabe-Verdichtung am Budget-Ende ------------

    /// Wie das Modell die Verdichtung beantwortet.
    #[derive(Clone, Copy)]
    enum HandoffReply {
        Structured,
        Fails,
        Empty,
    }

    /// Modell für die Übergabe-Tests: Arbeitsrunden liefern einen
    /// Zwischenstand plus Tool-Call (1 000 ungecachte Eingabe + 1 000
    /// Ausgabe = 2 000 neue Tokens je Runde); der Verdichtungsaufruf
    /// (erkennbar an seiner Systeminstruktion) antwortet je nach `reply`
    /// mit 2 000 + 500 neuen Tokens.
    struct HandoffProbeModel {
        reply: HandoffReply,
        work_rounds: AtomicUsize,
        handoff_calls: AtomicUsize,
        handoff_max_output: Mutex<Vec<Option<u32>>>,
    }

    impl HandoffProbeModel {
        fn new(reply: HandoffReply) -> Arc<Self> {
            Arc::new(Self {
                reply,
                work_rounds: AtomicUsize::new(0),
                handoff_calls: AtomicUsize::new(0),
                handoff_max_output: Mutex::new(Vec::new()),
            })
        }
    }

    fn structured_handoff_reply() -> String {
        crate::child_handoff::HANDOFF_SECTIONS
            .iter()
            .map(|section| format!("{section}\n- src/lib.rs:12 geprüft"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    impl ModelProvider for HandoffProbeModel {
        fn respond<'a>(&'a self, request: ModelRequest) -> ModelFuture<'a> {
            Box::pin(async move {
                if request
                    .system_prompt
                    .contains(crate::child_handoff::HANDOFF_INSTRUCTION_MARKER)
                {
                    self.handoff_calls.fetch_add(1, Ordering::SeqCst);
                    self.handoff_max_output
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .push(request.max_output_tokens);
                    return match self.reply {
                        HandoffReply::Structured => Ok(ModelResponse {
                            message: Some(structured_handoff_reply()),
                            usage: TokenUsage {
                                input_tokens: 2_000,
                                output_tokens: 500,
                                ..TokenUsage::default()
                            },
                            ..Default::default()
                        }),
                        HandoffReply::Fails => Err(crate::model::ModelError::RequestFailed(
                            "überlastet".to_owned(),
                        )),
                        HandoffReply::Empty => Ok(ModelResponse {
                            message: Some(String::new()),
                            ..Default::default()
                        }),
                    };
                }
                let round = self.work_rounds.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(ModelResponse {
                    message: Some(format!("Zwischenstand {round}")),
                    tool_calls: vec![ToolCall {
                        id: ToolCallId::new(),
                        name: ToolName::new("fs.read"),
                        arguments: serde_json::json!({ "path": format!("/src/f{round}.rs") }),
                    }],
                    usage: TokenUsage {
                        input_tokens: 1_000,
                        output_tokens: 1_000,
                        ..TokenUsage::default()
                    },
                    ..Default::default()
                })
            })
        }
    }

    struct HandoffProbeRegistry {
        model: Arc<HandoffProbeModel>,
    }

    impl ChildRegistryFactory for HandoffProbeRegistry {
        fn build_registry(
            &self,
            _role: &str,
            _input: &SpawnInput,
            _suggestions: Option<&AgentSuggestions>,
        ) -> Result<ExtensionRegistry, AgentSpawnError> {
            Ok(ExtensionRegistryBuilder::default().build())
        }

        fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
            Ok(self.model.clone())
        }
    }

    /// Budget der Übergabe-Tests: Reserve = 25 % von 20 000 = 5 000, der
    /// Turn endet also bei 15 000 (acht Runden à 2 000 statt zehn).
    const HANDOFF_TEST_LIMIT: u64 = 20_000;

    /// Zwei Kinder desselben Elternteils; das erste läuft bis ans Budget.
    async fn run_first_child_to_budget_end(
        reply: HandoffReply,
    ) -> TestResult<(
        ManagedAgentSpawner,
        Vec<SessionId>,
        Arc<HandoffProbeModel>,
        ChildRunResult,
    )> {
        let model = HandoffProbeModel::new(reply);
        let (spawner, children) = runnable_children(
            Arc::new(HandoffProbeRegistry {
                model: model.clone(),
            }),
            true,
            2,
            empty_registry,
        )?;
        let store = InMemoryStateStore::new();
        let result = spawner
            .run_child_with_budget(
                &children[0],
                &store,
                None,
                TurnInput::user("finde alle Aufrufer von foo"),
                AgentBudget {
                    max_tokens: Some(HANDOFF_TEST_LIMIT),
                    ..AgentBudget::default()
                },
            )
            .await
            .map_err(|error| TestError::Unexpected(format!("partial result expected: {error}")))?;
        Ok((spawner, children, model, result))
    }

    #[tokio::test]
    async fn a_budget_end_returns_one_marked_compacted_handoff_with_all_sections() -> TestResult {
        let (spawner, children, model, result) =
            run_first_child_to_budget_end(HandoffReply::Structured).await?;
        let child = &children[0];

        assert!(result.budget_exhausted);
        assert!(matches!(result.outcome, TurnOutcome::Completed));
        assert_eq!(
            result.budget_handoff,
            Some(crate::child_handoff::BudgetHandoff::Compacted)
        );
        let text = result
            .full_text
            .as_deref()
            .ok_or(TestError::Missing("handoff text"))?;
        assert!(
            text.starts_with(crate::child_handoff::HANDOFF_MARKER),
            "{text}"
        );
        assert!(text.contains("Budget erreicht – Übergabe-Zusammenfassung"));
        assert!(text.contains("verdichtet aus 8 Modellrunden"), "{text}");
        for section in crate::child_handoff::HANDOFF_SECTIONS {
            assert!(text.contains(section), "{section} fehlt: {text}");
        }
        assert!(text.contains(&format!("agent.result {{\"child_id\":\"{child}\"}}")));
        assert!(text.contains("continue_from"));

        // Genau ein Verdichtungsaufruf, Ausgabe gedeckelt.
        assert_eq!(model.handoff_calls.load(Ordering::SeqCst), 1);
        let max_output = model
            .handoff_max_output
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        assert_eq!(max_output.len(), 1);
        assert!(max_output.iter().all(|max| {
            max.is_some_and(|max| max <= crate::child_handoff::HANDOFF_MAX_OUTPUT_TOKENS)
        }));

        // Die Reserve greift: acht statt zehn Arbeitsrunden, und Turn plus
        // Verdichtung bleiben im Limit.
        assert_eq!(model.work_rounds.load(Ordering::SeqCst), 8);
        assert_eq!(result.usage.tokens, 16_000 + 2_500);
        assert!(result.usage.tokens <= HANDOFF_TEST_LIMIT);

        // `agent.result` liefert weiterhin den Rohtext, nicht die Übergabe.
        let parent = spawner
            .child_record(child)
            .ok_or(TestError::Missing("child record"))?
            .parent;
        let raw = spawner
            .child_result_text(&parent, child)
            .map_err(|error| TestError::Unexpected(error.message))?;
        assert_eq!(raw, "Zwischenstand 8");

        let record = spawner
            .budget_handoff_record(child)
            .ok_or(TestError::Missing("handoff record"))?;
        assert_eq!(record.kind, crate::child_handoff::BudgetHandoff::Compacted);
        assert_eq!(&record.origin, child);
        assert!(record.sandbox.is_some());
        Ok(())
    }

    #[tokio::test]
    async fn a_failing_or_empty_compaction_falls_back_to_the_last_answer() -> TestResult {
        for reply in [HandoffReply::Fails, HandoffReply::Empty] {
            let (_spawner, _children, model, result) = run_first_child_to_budget_end(reply).await?;
            assert!(result.budget_exhausted);
            assert_eq!(
                result.budget_handoff,
                Some(crate::child_handoff::BudgetHandoff::LastAnswer)
            );
            assert_eq!(result.full_text.as_deref(), Some("Zwischenstand 8"));
            // Kein Wiederholungsversuch, kein zweiter Aufruf.
            assert_eq!(model.handoff_calls.load(Ordering::SeqCst), 1);
            // Nur der Turn wird angerechnet (die leere Antwort hat keine Nutzung,
            // der Fehler keine Antwort).
            assert_eq!(result.usage.tokens, 16_000);
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_budget_ended_child_can_be_continued_by_its_parent_with_the_handoff() -> TestResult {
        let (spawner, children, model, _result) =
            run_first_child_to_budget_end(HandoffReply::Structured).await?;
        let (first, second) = (&children[0], &children[1]);
        let parent = spawner
            .child_record(first)
            .ok_or(TestError::Missing("child record"))?
            .parent;

        let seed = spawner
            .prepare_continuation(&parent, first, "worker")
            .map_err(|error| TestError::Unexpected(error.message))?;
        assert!(seed.handoff.contains("## Offene Punkte"));
        let link = spawner
            .bind_continuation(second, &seed)
            .map_err(|error| TestError::Unexpected(error.message))?;
        assert_eq!(&link.of, first);
        assert_eq!(link.number, 1);
        let task = spawner
            .child_task_state(second)
            .and_then(|state| state.task)
            .unwrap_or_default();
        assert!(
            task.starts_with(&format!("Fortsetzung von {first}")),
            "{task}"
        );

        // Die Fortsetzung arbeitet mit der Übergabe als erstem Kontext und
        // einem frischen Budget; endet sie ihrerseits am Budget, gehört ihre
        // Übergabe zur selben Kette.
        let store = InMemoryStateStore::new();
        let continued = spawner
            .run_child_with_budget(
                second,
                &store,
                None,
                TurnInput::user(crate::child_handoff::continuation_task(
                    first,
                    &seed.end,
                    &seed.handoff,
                    None,
                )),
                AgentBudget {
                    max_tokens: Some(HANDOFF_TEST_LIMIT),
                    ..AgentBudget::default()
                },
            )
            .await
            .map_err(|error| TestError::Unexpected(error.to_string()))?;
        assert_eq!(
            continued.budget_handoff,
            Some(crate::child_handoff::BudgetHandoff::Compacted)
        );
        assert_eq!(model.handoff_calls.load(Ordering::SeqCst), 2);
        let text = continued.full_text.unwrap_or_default();
        assert!(text.contains("noch 2 von 3 Fortsetzungen"), "{text}");
        let record = spawner
            .budget_handoff_record(second)
            .ok_or(TestError::Missing("continued handoff record"))?;
        assert_eq!(&record.origin, first);
        Ok(())
    }

    #[tokio::test]
    async fn a_continuation_is_refused_for_foreign_unfinished_or_other_role_children() -> TestResult
    {
        let (spawner, children, _model, _result) =
            run_first_child_to_budget_end(HandoffReply::Structured).await?;
        let (first, second) = (&children[0], &children[1]);
        let parent = spawner
            .child_record(first)
            .ok_or(TestError::Missing("child record"))?
            .parent;

        // Fremder Elternteil.
        let stranger = SessionId::new();
        assert!(
            spawner
                .prepare_continuation(&stranger, first, "worker")
                .is_err()
        );
        // Nicht am Budget beendet (das zweite Kind lief nie).
        assert!(
            spawner
                .prepare_continuation(&parent, second, "worker")
                .is_err()
        );
        // Andere Rolle.
        let other = spawner.prepare_continuation(&parent, first, "orchestrator");
        assert!(
            other.is_err_and(|error| error.message.contains("dieselbe Rolle")),
            "eine andere Rolle könnte mehr Rechte tragen"
        );
        Ok(())
    }

    #[tokio::test]
    async fn a_continuation_never_widens_the_sandbox_and_the_chain_is_capped() -> TestResult {
        let (spawner, children, _model, _result) =
            run_first_child_to_budget_end(HandoffReply::Structured).await?;
        let (first, second) = (&children[0], &children[1]);
        let parent = spawner
            .child_record(first)
            .ok_or(TestError::Missing("child record"))?
            .parent;
        let seed = spawner
            .prepare_continuation(&parent, first, "worker")
            .map_err(|error| TestError::Unexpected(error.message))?;

        // Rechte nicht erweitert: hatte der Vorgänger eine engere Sandbox als
        // das neue Kind (ReadWorkspace), scheitert die Bindung.
        let mut narrow = seed.clone();
        narrow.sandbox = Some(test_sandbox(PermissionSet::from_policy(
            std::iter::empty::<Permission>(),
        ))?);
        let widened = spawner.bind_continuation(second, &narrow);
        assert!(
            widened.is_err_and(|error| error.message.contains("Sandbox")),
            "eine Fortsetzung darf nie mehr dürfen als ihr Vorgänger"
        );

        // Kettengrenze: höchstens drei Fortsetzungen je ursprünglichem Kind.
        for number in 1..=crate::child_handoff::MAX_CONTINUATIONS {
            let seed = spawner
                .prepare_continuation(&parent, first, "worker")
                .map_err(|error| TestError::Unexpected(error.message))?;
            let link = spawner
                .bind_continuation(second, &seed)
                .map_err(|error| TestError::Unexpected(error.message))?;
            assert_eq!(link.number, number);
        }
        let exhausted = spawner.prepare_continuation(&parent, first, "worker");
        assert!(
            exhausted.is_err_and(|error| error.message.contains("Kettengrenze")),
            "die vierte Fortsetzung wird mit Hinweis abgewiesen"
        );
        Ok(())
    }

    /// Echte Admission unter einer externen Wurzel; das erste Kind läuft
    /// bis ans Budget und wird danach freigegeben.
    async fn admitted_child_at_budget_end() -> TestResult<(
        ManagedAgentSpawner,
        SessionId,
        SandboxSpec,
        SessionId,
        ChildRunResult,
    )> {
        let model = HandoffProbeModel::new(HandoffReply::Structured);
        let (spawner, parent, sandbox) =
            window_test_spawner(Arc::new(HandoffProbeRegistry { model }), None)?;
        let first = spawner
            .admit("worker", spawn_input(parent.clone()), sandbox.clone(), None)
            .map_err(|error| TestError::Unexpected(error.message))?;
        let store = InMemoryStateStore::new();
        let result = spawner
            .run_child_with_budget(
                &first,
                &store,
                None,
                TurnInput::user("finde alle Aufrufer von foo"),
                AgentBudget {
                    max_tokens: Some(HANDOFF_TEST_LIMIT),
                    ..AgentBudget::default()
                },
            )
            .await
            .map_err(|error| TestError::Unexpected(error.to_string()))?;
        spawner
            .release_child(&first)
            .map_err(|error| TestError::Unexpected(error.message))?;
        Ok((spawner, parent, sandbox, first, result))
    }

    fn continuation_input(parent: &SessionId, from: &str, task: &str) -> SpawnInput {
        SpawnInput {
            parent_session_id: parent.clone(),
            handoff_call_id: ToolCallId::new(),
            instructions: Some(task.to_owned()),
            // So reicht der Turn-Loop `transfer_to_worker {task, continue_from}`
            // bzw. das Agent-Werkzeug seine Argumente weiter.
            context: serde_json::json!({ "task": task, "continue_from": from }),
            ceiling: None,
        }
    }

    #[tokio::test]
    async fn continue_from_on_the_handoff_and_agent_tool_paths_binds_a_continuation() -> TestResult
    {
        let (spawner, parent, sandbox, first, result) = admitted_child_at_budget_end().await?;
        assert_eq!(
            result.budget_handoff,
            Some(crate::child_handoff::BudgetHandoff::Compacted)
        );
        // Der Hinweis nennt beide Wege, auch das Handoff-Werkzeug.
        let text = result.full_text.unwrap_or_default();
        assert!(text.contains("transfer_to_worker"), "{text}");
        assert!(text.contains("delegate_wave"), "{text}");

        // Handoff `transfer_to_worker` (UIA → Orchestrator): `spawn_child`.
        let second = harw_extension_api::AgentSpawner::spawn_child(
            &spawner,
            "worker",
            continuation_input(&parent, first.as_str(), "nur noch Modul C"),
            sandbox.clone(),
            None,
        )
        .await
        .map_err(|error| TestError::Unexpected(error.message))?;
        let link = spawner
            .continuation_link(&second)
            .ok_or(TestError::Missing("continuation link"))?;
        assert_eq!(link.of, first);
        assert_eq!(link.number, 1);
        let pending = spawner
            .child_task_state(&second)
            .and_then(|state| state.pending_task)
            .unwrap_or_default();
        assert!(
            pending.starts_with(&format!("Fortsetzung von {first}: nur noch Modul C")),
            "{pending}"
        );
        assert!(pending.contains("## Offene Punkte"), "{pending}");
        spawner
            .release_child(&second)
            .map_err(|error| TestError::Unexpected(error.message))?;

        // Agent-Werkzeug (`AgentToolAdapter`): `spawn_child_or_wait`.
        let guard = spawner
            .spawn_child_or_wait(
                "worker",
                continuation_input(&parent, first.as_str(), "Rest prüfen"),
                sandbox,
                None,
                Duration::from_secs(1),
                &CancelToken::new(),
            )
            .await
            .map_err(|error| TestError::Unexpected(error.message))?;
        let link = spawner
            .continuation_link(guard.child())
            .ok_or(TestError::Missing("continuation link (agent tool)"))?;
        assert_eq!(link.number, 2);
        Ok(())
    }

    #[tokio::test]
    async fn continue_from_on_the_handoff_path_rejects_foreign_and_unknown_children() -> TestResult
    {
        let (spawner, parent, sandbox, first, _result) = admitted_child_at_budget_end().await?;
        // Fremder Elternteil: dieselbe Ablehnung wie ein unbekanntes Kind,
        // erkennbar am Präfix (der Turn-Loop macht daraus einen
        // Werkzeugfehler statt eines Turn-Abbruchs).
        let stranger = SessionId::new();
        for input in [
            continuation_input(&stranger, first.as_str(), "weiter"),
            continuation_input(&parent, SessionId::new().as_str(), "weiter"),
        ] {
            let refused = harw_extension_api::AgentSpawner::spawn_child(
                &spawner,
                "worker",
                input,
                sandbox.clone(),
                None,
            )
            .await;
            assert!(
                refused.is_err_and(|error| crate::child_handoff::is_continuation_rejection(
                    &error.message
                )),
                "fremde oder unbekannte Kinder lassen sich nicht fortsetzen"
            );
        }
        // Kein gültiger Verweis.
        let mut input = continuation_input(&parent, "", "weiter");
        input.context = serde_json::json!({ "task": "weiter", "continue_from": 7 });
        let refused = spawner.admit("worker", input, sandbox, None);
        assert!(
            refused.is_err_and(|error| crate::child_handoff::is_continuation_rejection(
                &error.message
            ))
        );
        // Keine Fortsetzung wurde gezählt.
        assert!(
            spawner
                .prepare_continuation(&parent, &first, "worker")
                .is_ok()
        );
        Ok(())
    }

    // --- Runde 5, Teil M: Journal, Endbericht, Nachrichten ----------------

    /// Wie das Test-Modell nach der ersten Arbeitsrunde weitermacht.
    #[derive(Clone, Copy)]
    enum EndProbeTurn {
        /// Hängt (nur Abbruch/Zeitbudget beendet den Turn).
        Hang,
        /// Provider-Fehler.
        Fail,
        /// Runde 7, Teil A3: Provider-Fehler, und auch die Verdichtung
        /// scheitert (Provider ganz weg).
        FailEverything,
        /// Schließt mit einer Antwort ab.
        Finish,
    }

    /// Erste Runde: Zwischenstand plus Werkzeugaufruf `fs.read`; danach je
    /// nach `turn`. Eine Übergabe-Verdichtung (erkennbar an ihrer
    /// Systeminstruktion) wird immer strukturiert beantwortet und gezählt.
    struct EndProbeModel {
        turn: EndProbeTurn,
        work_calls: AtomicUsize,
        compaction_calls: AtomicUsize,
        second_request: Mutex<Option<String>>,
    }

    impl EndProbeModel {
        fn new(turn: EndProbeTurn) -> Arc<Self> {
            Arc::new(Self {
                turn,
                work_calls: AtomicUsize::new(0),
                compaction_calls: AtomicUsize::new(0),
                second_request: Mutex::new(None),
            })
        }
    }

    impl ModelProvider for EndProbeModel {
        fn respond<'a>(&'a self, request: ModelRequest) -> ModelFuture<'a> {
            Box::pin(async move {
                if request
                    .system_prompt
                    .contains(crate::child_handoff::HANDOFF_INSTRUCTION_MARKER)
                {
                    self.compaction_calls.fetch_add(1, Ordering::SeqCst);
                    if matches!(self.turn, EndProbeTurn::FailEverything) {
                        return Err(crate::model::ModelError::RequestFailed(
                            "provider unreachable".to_owned(),
                        ));
                    }
                    return Ok(ModelResponse {
                        message: Some(structured_handoff_reply()),
                        ..Default::default()
                    });
                }
                let call = self.work_calls.fetch_add(1, Ordering::SeqCst) + 1;
                if call == 1 {
                    return Ok(ModelResponse {
                        message: Some("Zwischenstand: lib.rs wird angepasst".to_owned()),
                        tool_calls: vec![ToolCall {
                            id: ToolCallId::new(),
                            name: ToolName::new("fs.read"),
                            arguments: serde_json::json!({ "path": "src/lib.rs" }),
                        }],
                        ..Default::default()
                    });
                }
                *self
                    .second_request
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) =
                    Some(format!("{:?}", request.history.items()));
                match self.turn {
                    EndProbeTurn::Hang => {
                        std::future::pending::<()>().await;
                        Ok(ModelResponse::text("unreachable"))
                    }
                    EndProbeTurn::Fail | EndProbeTurn::FailEverything => {
                        Err(crate::model::ModelError::RequestFailed(
                            "provider 529 overloaded".to_owned(),
                        ))
                    }
                    EndProbeTurn::Finish => Ok(ModelResponse::text("fertig")),
                }
            })
        }
    }

    struct EndProbeRegistry {
        model: Arc<EndProbeModel>,
    }

    impl ChildRegistryFactory for EndProbeRegistry {
        fn build_registry(
            &self,
            _role: &str,
            _input: &SpawnInput,
            _suggestions: Option<&AgentSuggestions>,
        ) -> Result<ExtensionRegistry, AgentSpawnError> {
            Ok(ExtensionRegistryBuilder::default().build())
        }

        fn model_for(&self, _role: &str) -> Result<Arc<dyn ModelProvider>, AgentSpawnError> {
            Ok(self.model.clone())
        }
    }

    /// Hängt Journal, Fortschritts- und Journal-Beobachter an ein über
    /// [`runnable_children`] (ohne Admission) angelegtes Kind — wie es die
    /// echte Admission tut.
    fn wire_comms(spawner: &ManagedAgentSpawner, child: &SessionId, task: &str) -> TestResult {
        let parent = spawner
            .child_record(child)
            .ok_or(TestError::Missing("child record"))?
            .parent;
        spawner
            .comms
            .open_journal(child, &parent, "worker", Some(task));
        let mut manager = spawner
            .manager
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let session = manager
            .remove(child)
            .ok_or(TestError::Missing("child session"))?
            .with_progress_observer(Some(spawner.progress_observer()))
            .with_tool_outcome_observer(Some(Arc::new(JournalToolObserver {
                comms: Arc::clone(&spawner.comms),
                inner: None,
            })));
        manager
            .restore(session)
            .map_err(ctx("child session restores"))?;
        Ok(())
    }

    async fn end_probe_child(
        turn: EndProbeTurn,
    ) -> TestResult<(ManagedAgentSpawner, SessionId, Arc<EndProbeModel>)> {
        let model = EndProbeModel::new(turn);
        let (spawner, children) = runnable_children(
            Arc::new(EndProbeRegistry {
                model: model.clone(),
            }),
            true,
            1,
            empty_registry,
        )?;
        let child = children[0].clone();
        wire_comms(&spawner, &child, "Setze das Feature um")?;
        Ok((spawner, child, model))
    }

    /// Wall-Time-Ende: der Elternteil bekommt Journal **und** (mit eigenem
    /// kurzem Zeitlimit) eine Übergabe-Verdichtung; `continue_from` ist
    /// möglich.
    #[tokio::test]
    async fn a_wall_time_end_delivers_the_journal_and_a_handoff() -> TestResult {
        let (spawner, child, model) = end_probe_child(EndProbeTurn::Hang).await?;
        let store = InMemoryStateStore::new();
        let result = spawner
            .run_child_with_budget(
                &child,
                &store,
                None,
                TurnInput::user("Setze das Feature um"),
                AgentBudget {
                    max_wall_time_ms: Some(300),
                    ..AgentBudget::default()
                },
            )
            .await;
        assert!(
            matches!(result, Err(ChildRunError::BudgetExhausted(_))),
            "{result:?}"
        );
        let report = spawner
            .child_end_report(&child)
            .ok_or(TestError::Missing("Endbericht"))?;
        assert_eq!(report.status, crate::child_comms::ChildEndStatus::Timeout);
        assert!(report.reason.starts_with("Zeitbudget"), "{}", report.reason);
        assert!(report.handoff.is_some(), "Verdichtung war möglich");
        assert_eq!(model.compaction_calls.load(Ordering::SeqCst), 1);
        assert!(report.steps >= 1);
        assert!(report.journal_summary.contains("fs.read(src/lib.rs)"));
        assert!(report.continuation, "continue_from ist möglich");
        let text = report.to_parent_text();
        let header =
            crate::child_comms::parse_child_end(&text).ok_or(TestError::Missing("Kopfzeile"))?;
        assert!(header.handoff);
        assert!(
            spawner
                .prepare_continuation(&report.parent, &child, "worker")
                .is_ok()
        );
        Ok(())
    }

    /// Runde 7, Teil A3: Provider-Fehler — Journal **und** ein kurzer
    /// Zusammenfassungsaufruf mit eigenem Zeitlimit; der Lauf endet nie mehr
    /// ohne Ergebnis.
    #[tokio::test]
    async fn a_provider_error_delivers_the_journal_and_a_summary() -> TestResult {
        let (spawner, child, model) = end_probe_child(EndProbeTurn::Fail).await?;
        let store = InMemoryStateStore::new();
        let _result = spawner
            .run_child_with_budget(
                &child,
                &store,
                None,
                TurnInput::user("Setze das Feature um"),
                AgentBudget::default(),
            )
            .await;
        let report = spawner
            .child_end_report(&child)
            .ok_or(TestError::Missing("Endbericht"))?;
        assert_eq!(report.status, crate::child_comms::ChildEndStatus::Failed);
        assert!(
            report.handoff.is_some(),
            "Zusammenfassung nach Providerfehler"
        );
        assert_eq!(model.compaction_calls.load(Ordering::SeqCst), 1);
        assert!(report.journal_summary.contains("fs.read"));
        let text = report.to_parent_text();
        assert!(text.contains("--- Übergabe ---"), "{text}");
        Ok(())
    }

    /// Runde 7, Teil A3: scheitert auch die Zusammenfassung, bleiben Journal
    /// und letzter Assistententext.
    #[tokio::test]
    async fn a_provider_error_without_summary_keeps_journal_and_last_text() -> TestResult {
        let (spawner, child, model) = end_probe_child(EndProbeTurn::FailEverything).await?;
        let store = InMemoryStateStore::new();
        let _result = spawner
            .run_child_with_budget(
                &child,
                &store,
                None,
                TurnInput::user("Setze das Feature um"),
                AgentBudget::default(),
            )
            .await;
        let report = spawner
            .child_end_report(&child)
            .ok_or(TestError::Missing("Endbericht"))?;
        assert_eq!(report.status, crate::child_comms::ChildEndStatus::Failed);
        assert!(report.handoff.is_none());
        assert_eq!(model.compaction_calls.load(Ordering::SeqCst), 1);
        assert!(
            report
                .handoff_note
                .as_deref()
                .is_some_and(|note| note.contains("Verdichtung")),
            "{:?}",
            report.handoff_note
        );
        assert!(report.journal_summary.contains("fs.read"));
        assert!(
            report
                .journal_summary
                .contains("Zwischenstand: lib.rs wird angepasst"),
            "letzter Assistententext im Journal: {}",
            report.journal_summary
        );
        Ok(())
    }

    /// Abbruch: das Journal bleibt erhalten, ohne Verdichtung und ohne
    /// Fortsetzungsangebot.
    #[tokio::test]
    async fn a_cancel_delivers_the_journal() -> TestResult {
        let (spawner, child, model) = end_probe_child(EndProbeTurn::Hang).await?;
        let store = InMemoryStateStore::new();
        let run = spawner.run_child_with_budget(
            &child,
            &store,
            None,
            TurnInput::user("Setze das Feature um"),
            AgentBudget::default(),
        );
        let cancel = async {
            for _ in 0..200 {
                if spawner
                    .comms
                    .journal(&child)
                    .is_some_and(|journal| journal.steps() >= 1)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            spawner.request_cancellation(&child)
        };
        let (result, requested) = tokio::join!(run, cancel);
        assert!(requested);
        assert!(result.is_err(), "{result:?}");
        let report = spawner
            .child_end_report(&child)
            .ok_or(TestError::Missing("Endbericht"))?;
        assert_eq!(report.status, crate::child_comms::ChildEndStatus::Cancelled);
        assert!(report.handoff.is_none());
        assert!(!report.continuation);
        assert_eq!(model.compaction_calls.load(Ordering::SeqCst), 0);
        assert!(report.journal_summary.contains("fs.read"));
        Ok(())
    }

    /// `agent.message` erreicht das Kind an seiner nächsten Runden-Grenze
    /// als „[Nachricht von <rolle>] …".
    #[tokio::test]
    async fn a_message_reaches_the_child_at_its_next_round_boundary() -> TestResult {
        let (spawner, child, model) = end_probe_child(EndProbeTurn::Finish).await?;
        let parent = spawner
            .child_record(&child)
            .ok_or(TestError::Missing("record"))?
            .parent;
        let delivery = spawner
            .send_message_to_child(&parent, child.as_str(), "Bitte nur src/ anfassen")
            .map_err(|error| TestError::Unexpected(error.message))?;
        assert_eq!(delivery, crate::child_comms::MessageDelivery::Queued);
        let store = InMemoryStateStore::new();
        let result = spawner
            .run_child_with_budget(
                &child,
                &store,
                None,
                TurnInput::user("Setze das Feature um"),
                AgentBudget::default(),
            )
            .await
            .map_err(|error| TestError::Unexpected(error.to_string()))?;
        assert!(matches!(result.outcome, TurnOutcome::Completed));
        let second = model
            .second_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .ok_or(TestError::Missing("zweite Modellrunde"))?;
        assert!(
            second.contains("[Nachricht von uia] Bitte nur src/ anfassen"),
            "{second}"
        );
        assert_eq!(spawner.comms.pending_inbound(&child), 0, "genau einmal");
        assert!(spawner.child_end_report(&child).is_none(), "reguläres Ende");
        Ok(())
    }

    /// Nachrichten an fremde, unbekannte oder beendete Kinder werden mit
    /// derselben Meldung abgewiesen; ein Großelternteil ist kein Elternteil.
    #[tokio::test]
    async fn a_message_to_a_foreign_or_finished_child_is_rejected() -> TestResult {
        let (spawner, child, _model) = end_probe_child(EndProbeTurn::Finish).await?;
        let parent = spawner
            .child_record(&child)
            .ok_or(TestError::Missing("record"))?
            .parent;
        let foreign = spawner.send_message_to_child(&SessionId::new(), child.as_str(), "x");
        let unknown = spawner.send_message_to_child(&parent, SessionId::new().as_str(), "x");
        let (Err(foreign), Err(unknown)) = (foreign, unknown) else {
            return Err(TestError::Unexpected("beide müssen scheitern".to_owned()));
        };
        assert!(foreign.message.starts_with("kein eigenes, laufendes Kind"));
        assert!(unknown.message.starts_with("kein eigenes, laufendes Kind"));
        let store = InMemoryStateStore::new();
        spawner
            .run_child_with_budget(
                &child,
                &store,
                None,
                TurnInput::user("x"),
                AgentBudget::default(),
            )
            .await
            .map_err(|error| TestError::Unexpected(error.to_string()))?;
        assert_eq!(spawner.child_status(&child), Some(ChildStatus::Completed));
        let finished = spawner.send_message_to_child(&parent, child.as_str(), "noch was");
        assert!(
            finished.is_err_and(|error| error.message.starts_with("kein eigenes, laufendes Kind")),
            "ein beendetes Kind nimmt keine Nachricht mehr an"
        );
        // Runde 9, E3: … aber es lässt sich fortsetzen — nur vom eigenen
        // Elternteil, mit derselben Rolle; die Übergabe trägt die letzte
        // Antwort und das Journal.
        let role = spawner
            .child_record(&child)
            .ok_or(TestError::Missing("record"))?
            .role;
        assert_eq!(
            spawner.resumable_child_role(&parent, child.as_str()),
            Some(role.clone())
        );
        assert_eq!(
            spawner.resumable_child_role(&SessionId::new(), child.as_str()),
            None,
            "fremde Aufrufer setzen nicht fort"
        );
        assert_eq!(
            spawner.resumable_child_role(&parent, SessionId::new().as_str()),
            None
        );
        let seed = spawner
            .prepare_continuation(&parent, &child, &role)
            .map_err(|error| TestError::Unexpected(error.message))?;
        assert_eq!(seed.end, crate::child_handoff::PredecessorEnd::Completed);
        assert!(
            seed.handoff.starts_with("Letzte Antwort:"),
            "{}",
            seed.handoff
        );
        // Längendeckel.
        let (spawner, child, _model) = end_probe_child(EndProbeTurn::Finish).await?;
        let parent = spawner
            .child_record(&child)
            .ok_or(TestError::Missing("record"))?
            .parent;
        let long = "x".repeat(crate::child_comms::MESSAGE_MAX_BYTES + 1);
        assert!(
            spawner
                .send_message_to_child(&parent, child.as_str(), &long)
                .is_err()
        );
        Ok(())
    }

    /// `parent.message {kind: "question"}` wartet und bekommt die Antwort
    /// über `agent.message`; ohne Antwort liefert der Zeitablauf den
    /// Hinweis. Die Frage erscheint im Eingang der Wurzel.
    #[tokio::test]
    async fn a_parent_question_waits_for_the_answer_or_times_out_with_a_hint() -> TestResult {
        let (spawner, child, _model) = end_probe_child(EndProbeTurn::Finish).await?;
        let parent = spawner
            .child_record(&child)
            .ok_or(TestError::Missing("record"))?
            .parent;
        let ask = spawner.ask_parent(&child, "Tabelle A oder B?", Duration::from_secs(10));
        let answer = async {
            for _ in 0..200 {
                if spawner.comms.has_pending_question(&child) {
                    break;
                }
                tokio::task::yield_now().await;
            }
            spawner.send_message_to_child(&parent, child.as_str(), "B")
        };
        let (asked, delivered) = tokio::join!(ask, answer);
        assert_eq!(
            delivered.map_err(|error| TestError::Unexpected(error.message))?,
            crate::child_comms::MessageDelivery::AnsweredQuestion
        );
        let (reply, answered) = asked.map_err(|error| TestError::Unexpected(error.message))?;
        assert_eq!((reply.as_str(), answered), ("B", true));
        let inbox = spawner.comms.take_parent_messages(&parent);
        assert_eq!(inbox.len(), 1);
        assert_eq!(
            inbox[0].kind,
            crate::child_comms::ParentMessageKind::Question
        );

        let (reply, answered) = spawner
            .ask_parent(&child, "Noch da?", Duration::from_millis(30))
            .await
            .map_err(|error| TestError::Unexpected(error.message))?;
        assert!(!answered);
        assert_eq!(reply, crate::child_comms::NO_ANSWER_REPLY);
        assert!(!spawner.comms.has_pending_question(&child));
        Ok(())
    }

    /// Runde 9, E3: solange eine Frage an den Elternteil offen ist, ruht das
    /// Zeitbudget — das Kind bleibt am Leben, bis die Frage beantwortet,
    /// abgebrochen oder abgelaufen ist; danach greift das Budget wieder.
    #[tokio::test]
    async fn a_pending_question_pauses_the_wall_time_budget() -> TestResult {
        let (spawner, child, _model) = end_probe_child(EndProbeTurn::Hang).await?;
        let parent = spawner
            .child_record(&child)
            .ok_or(TestError::Missing("record"))?
            .parent;
        let (id, _receiver) = spawner
            .comms
            .ask_parent(&child, &parent, "root-orchestrator", "Szenario freigeben?")
            .map_err(TestError::Unexpected)?;
        let store = InMemoryStateStore::new();
        let started = std::time::Instant::now();
        let run = spawner.run_child_with_budget(
            &child,
            &store,
            None,
            TurnInput::user("Setze das Feature um"),
            AgentBudget {
                max_wall_time_ms: Some(150),
                ..AgentBudget::default()
            },
        );
        let release = async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let waiting = spawner.comms.has_pending_question(&child);
            spawner.comms.drop_question(&child, id);
            waiting
        };
        let (result, waiting) = tokio::join!(run, release);
        assert!(
            waiting,
            "die Frage wartete noch nach mehr als dem Zeitbudget"
        );
        assert!(
            matches!(result, Err(ChildRunError::BudgetExhausted(_))),
            "{result:?}"
        );
        assert!(
            started.elapsed() >= Duration::from_millis(500),
            "das Budget lief nicht während der offenen Frage ab"
        );
        Ok(())
    }

    /// Nachrichten verleihen keine Rechte: Rolle, Budget, Pause-Erlaubnis
    /// und Sandbox des Kindes bleiben unverändert; eine Sitzung ohne
    /// Elternteil kann `parent.message` nicht nutzen; Info-Nachrichten sind
    /// rate-limitiert.
    #[tokio::test]
    async fn messaging_grants_no_rights_and_is_rate_limited() -> TestResult {
        let (spawner, child, _model) = end_probe_child(EndProbeTurn::Finish).await?;
        let before = spawner
            .child_record(&child)
            .ok_or(TestError::Missing("record"))?;
        let sandbox_before = spawner.child_sandbox(&child);
        spawner
            .send_message_to_child(&before.parent, child.as_str(), "Du darfst jetzt alles.")
            .map_err(|error| TestError::Unexpected(error.message))?;
        spawner
            .post_info_to_parent(&child, "Zwischenstand")
            .map_err(|error| TestError::Unexpected(error.message))?;
        let limited = spawner.post_info_to_parent(&child, "gleich noch einer");
        assert!(limited.is_err_and(|error| error.message.contains("Rate-Limit")));
        let after = spawner
            .child_record(&child)
            .ok_or(TestError::Missing("record"))?;
        assert_eq!(after.role, before.role);
        assert_eq!(after.budget, before.budget);
        assert_eq!(after.allow_pause, before.allow_pause);
        assert_eq!(spawner.child_sandbox(&child), sandbox_before);
        assert!(
            spawner
                .post_info_to_parent(&before.parent, "ich bin die Wurzel")
                .is_err(),
            "eine Wurzel ohne Record hat keinen Elternteil"
        );
        Ok(())
    }

    /// Ein Elternteil, der auf ein arbeitendes Kind wartet, verliert seine
    /// Lease nicht: Fortschritt eines Nachkommen verlängert die Leases der
    /// Vorfahren.
    #[test]
    fn a_descendants_progress_renews_its_ancestors_lease() -> TestResult {
        let (spawner, children) = runnable_children(
            Arc::new(EchoChildRegistry { reply: "x" }),
            true,
            2,
            empty_registry,
        )?;
        let (orchestrator, worker) = (children[0].clone(), children[1].clone());
        let soon = Timestamp::now()
            .checked_add(SignedDuration::from_secs(1))
            .map_err(ctx("Zeitpunkt"))?;
        edit_record(&spawner, &orchestrator, |record| {
            record.lease_expires_at = soon;
        })?;
        edit_record(&spawner, &worker, |record| {
            record.parent = orchestrator.clone();
        })?;
        let observer = spawner.progress_observer();
        crate::guard::ProgressObserver::on_progress(observer.as_ref(), &worker);
        let renewed = spawner
            .child_record(&orchestrator)
            .ok_or(TestError::Missing("record"))?
            .lease_expires_at;
        assert!(renewed > soon, "{renewed} > {soon}");
        Ok(())
    }

    // --- #22 Welle 1B: Lebenszyklus der Agent-IR für Fortsetzungen -------

    /// Ein Spawner mit der Rolle `worker`, deren IR `lifecycle` trägt, und
    /// ein Übergabe-Eintrag für ein (fiktives) beendetes Kind `first` des
    /// Elternteils — ohne Modelllauf, direkt im Fortsetzungs-Buch.
    fn lifecycle_spawner(
        lifecycle: &str,
    ) -> TestResult<(ManagedAgentSpawner, SessionId, SandboxSpec, SessionId)> {
        let ir = test_agent_ir(&format!("[tools]\nadmitted = [\"fs.read\"]\n\n{lifecycle}"))?;
        let (spawner, parent, sandbox) =
            window_test_spawner(Arc::new(IrChildRegistry { ir }), None)?;
        let first = SessionId::new();
        spawner
            .handoff_ledger
            .lock()
            .map_err(|_| TestError::Unexpected("ledger lock poisoned".to_owned()))?
            .record(crate::child_handoff::HandoffRecord {
                child: first.clone(),
                parent: parent.clone(),
                role: "worker".to_owned(),
                sandbox: Some(sandbox.clone()),
                handoff: "Zwischenstand".to_owned(),
                kind: crate::child_handoff::BudgetHandoff::LastAnswer,
                origin: first.clone(),
                end: crate::child_handoff::PredecessorEnd::BudgetExhausted,
            });
        Ok((spawner, parent, sandbox, first))
    }

    #[test]
    fn continue_from_is_refused_when_the_lifecycle_forbids_a_rerun() -> TestResult {
        let (spawner, parent, _sandbox, first) =
            lifecycle_spawner("[lifecycle]\nallow_rerun = false\nmax_attempts = 5\n")?;
        let refused = spawner.prepare_continuation(&parent, &first, "worker");
        let message = match refused {
            Ok(_) => return Err(TestError::Unexpected("rerun must be refused".to_owned())),
            Err(error) => error.message,
        };
        assert!(message.contains("allow_rerun = false"), "{message}");
        assert!(crate::child_handoff::is_continuation_rejection(&message));
        assert_eq!(spawner.resumable_child_role(&parent, first.as_str()), None);
        // Fremde Kinder bekommen weiterhin dieselbe Antwort wie unbekannte —
        // der Lebenszyklus verrät nichts vor der Eigentumsprüfung.
        let stranger = spawner.prepare_continuation(&SessionId::new(), &first, "worker");
        assert!(stranger.is_err_and(|error| error.message.contains("kein eigenes")));
        Ok(())
    }

    #[test]
    fn continue_from_is_refused_beyond_max_attempts() -> TestResult {
        let (spawner, parent, sandbox, first) =
            lifecycle_spawner("[lifecycle]\nallow_rerun = true\nmax_attempts = 2\n")?;
        assert_eq!(
            spawner.resumable_child_role(&parent, first.as_str()),
            Some("worker".to_owned())
        );
        // Versuch 2 (die erste Fortsetzung) liegt innerhalb von max_attempts.
        let seed = spawner
            .prepare_continuation(&parent, &first, "worker")
            .map_err(|error| TestError::Unexpected(error.message))?;
        let second = spawner
            .admit("worker", spawn_input(parent.clone()), sandbox.clone(), None)
            .map_err(|error| TestError::Unexpected(error.message))?;
        let link = spawner
            .bind_continuation(&second, &seed)
            .map_err(|error| TestError::Unexpected(error.message))?;
        assert_eq!(link.number, 1);
        spawner
            .release_child(&second)
            .map_err(|error| TestError::Unexpected(error.message))?;

        // Versuch 3 überschreitet max_attempts = 2 — obwohl die allgemeine
        // Kettengrenze (MAX_CONTINUATIONS = 3) noch Platz hätte.
        let refused = spawner.prepare_continuation(&parent, &first, "worker");
        assert!(
            refused.is_err_and(|error| error.message.contains("max_attempts = 2")
                && crate::child_handoff::is_continuation_rejection(&error.message)),
        );
        let third = spawner
            .admit("worker", spawn_input(parent.clone()), sandbox, None)
            .map_err(|error| TestError::Unexpected(error.message))?;
        let late_bind = spawner.bind_continuation(&third, &seed);
        assert!(
            late_bind.is_err_and(|error| error.message.contains("max_attempts")),
            "auch ein früher vorbereiteter Seed bindet nicht über die Grenze"
        );
        assert_eq!(spawner.resumable_child_role(&parent, first.as_str()), None);
        Ok(())
    }
}
