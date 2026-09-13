//! Turn-Loop: der eigentliche Agent-Zyklus.
//!
//! Schritt für Schritt:
//! 1. Context sammeln (ContextProvider)
//! 2. Instructions laden (InstructionsProvider)
//! 3. Observer benachrichtigen (TurnObserver::on_turn_start)
//! 4. Model Call (provider-neutral — noch nicht implementiert)
//! 5. Für jeden ToolCall:
//!    a. Guardrail (ApprovalHandler)
//!    b. Handoff? → AgentSpawner → WaitingForChild
//!    c. Sonst: ToolExecutor::execute
//!    d. ToolResult in History
//! 6. Keine ToolCalls mehr → Turn fertig
//! 7. Observer: on_turn_stop, state → Idle
//!
//! # Tracing
//!
//! Every public entry point (run_turn, run_turn_durable, resume_after_child,
//! resume_after_child_durable, resume_after_approval, resume_after_approval_durable)
//! is wrapped in an `agent.turn` span carrying `turn_id` and `session_id`.
//!
//! **Redaction policy**: no prompt text, no tool arguments JSON, no response
//! content is ever logged. Only IDs, names, byte-sizes, counters, and status
//! strings ("ok" | "err") appear in structured fields.
//!
//! Span/event hierarchy per turn:
//! ```text
//! agent.turn { turn_id, session_id }
//! ├─ model.request  { size_bytes }          ← info event
//! ├─ model.response { size_bytes, tool_call_count }   ← info event
//! ├─ tool.call      { tool_name }           ← info span
//! │    └─ tool.execute { duration_ms, status } ← info event
//! ├─ approval.wait  { approval_kind }       ← info span
//! └─ transcript.persist { records_written } ← debug event
//! ```
//!
//! # AW1-03: warum Schritt 1 noch der alte Pfad ist
//!
//! [`gather_context`] (Schritt 1) liefert bis heute
//! `Vec<harw_extension_api::ContextFragment>` — nur `label` + `content`,
//! ohne Sektion, `TrustClass`, `Stability` oder Kosten. `drive_turn` reicht
//! das unverändert an `ModelRequest::with_context_budget` weiter, das intern
//! die **Bestandsmontage** aus `crate::context_budget` (`ContextBudget`,
//! `ContextAssembly`, `assemble`) aufruft.
//!
//! Der neue, `ContextProgram`-bewusste, deterministisch geordnete Pfad aus
//! diesem Knoten — `context_budget::Assembly<Gathered|Admitted|Budgeted>`,
//! der `harw_context::Fragment` statt `ContextFragment` voraussetzt — ist
//! **absichtlich nicht** hier verdrahtet: es gibt noch keinen
//! `ContextProvider`, der ein `harw_context::Fragment` (mit Sektion,
//! Vertrauensklasse, Stabilität, bereits berechneten Kosten) produziert.
//! Eine Verdrahtung an dieser Stelle hätte bedeutet, diese fünf Angaben pro
//! Fragment zu erfinden — das wäre keine Durchsetzung gewesen, nur ihr
//! Anschein. Sobald ein Fragment-Provider existiert, ersetzt
//! `context_budget::Assembly::gather(fragments).admit(&program,
//! &ceiling)?.budget(&spec)?.render()` diesen Schritt.
//!
//! ## Nachtrag (Folgeknoten): eine der beiden Lücken hat sich seither geschlossen
//!
//! Eigene Prüfung von `crate::session`, nicht Übernahme des obigen Absatzes:
//! `SpawnContext` (`session.rs`) trägt inzwischen ein
//! `ceiling: Option<harw_context::ContextCeiling>`, erreichbar über
//! `session.spawn_context().and_then(|c| c.ceiling.clone())`. Eine
//! `ContextCeiling` ist damit produktiv vorhanden. **Kein** `ContextProgram`
//! ist es weiterhin: weder `AgentSession` noch `SpawnContext` tragen eines,
//! und `harw_agent_dsl::executable::ContextProgram` selbst hat (eigene
//! Prüfung von `harw-agent-dsl/src/executable.rs`) kein Feld für einen
//! `harw_context::DetailMode` je Sektion — nur `must_include`/`exclude`.
//! Der ursprüngliche Blocker "fünf Angaben pro Fragment erfinden" (Absatz
//! oben) bleibt deshalb unverändert bestehen: eine Ceiling allein liefert
//! weder Sektion noch Vertrauen noch Stabilität noch Kosten eines Fragments
//! — nur eine Erlaubnisgrenze dafür. Siehe `crate::context_budget`s
//! Modul-Abschnitt "Die nachgeholte Verdrahtung" für die vollständige,
//! aktuelle Bestandsaufnahme aller drei noch offenen Voraussetzungen
//! (Fragment-Provider, `ContextProgram` je Sitzung, ein Weg von hier zu
//! `harw_tools::context_load::ContextLoadExecutor::seed_turn`) sowie für die
//! beiden additiven Bausteine, die dieser Folgeknoten dafür bereits in
//! `context_budget.rs` bereitgestellt hat
//! (`ContextAssemblyV2::render_trust_blocks_with_detail`,
//! `ContextAssemblyV2::spent_per_section`) — beide ungenutzt von dieser
//! Datei, aus genau den hier genannten Gründen, nicht aus Versehen.
//!
//! ## Zweiter Nachtrag (dieser Knoten): `ContextProgram` ist jetzt erreichbar,
//! `DetailMode` auch — zwei der vier Lücken sind kleiner geworden, keine ist ganz zu
//!
//! Eigene Prüfung dieses Knotens gegen den obigen Absatz:
//!
//! 1. **`ContextProgram` je Sitzung ist jetzt erreichbar.**
//!    [`crate::session::AgentSession`] trägt seit diesem Knoten
//!    `context_program: Option<harw_agent_dsl::executable::ContextProgram>`
//!    über `AgentSession::with_context_program`/`AgentSession::context_program`
//!    (bewusst auf `AgentSession`, nicht auf [`SpawnContext`] — Begründung in
//!    `session.rs`s Moduldoku, Abschnitt "Folgeknoten zu AW2-01/AW2-02"). Eine
//!    Sitzung ohne diesen Aufruf verhält sich unverändert: `gather_context`
//!    unten liest dieses Feld nicht, also ändert seine bloße Existenz kein
//!    bestehendes Rendering.
//! 2. **`harw_agent_dsl::executable::ContextProgram` trägt jetzt den
//!    [`harw_context::DetailMode`] je Sektion.** `ContextProgram::section_detail`
//!    (Folgeknoten zu AW2-01) schließt genau die Lücke, die der Absatz oben
//!    ("kein Feld für einen `harw_context::DetailMode` je Sektion") noch offen
//!    ließ — `SNAPSHOT_HASH_DOMAIN` in `executable.rs` ist deshalb auf `v3`
//!    gestiegen.
//! 3. **`gather_context` (Schritt 1 unten) ist unverändert der alte Pfad.**
//!    Es liefert weiterhin `Vec<harw_extension_api::ContextFragment>`, nicht
//!    `harw_context::Fragment`. Diese Datei besitzt keinen `ContextProvider`
//!    — die Trait-Definition und ihre Implementierungen liegen in
//!    `harw-extension-api`/den jeweiligen Provider-Crates, außerhalb des
//!    Schreibbereichs dieses Knotens (`harw-agent-dsl/src/executable.rs`,
//!    `harw-core/src/session.rs`, `harw-core/src/turn_loop.rs`). Der
//!    ursprüngliche Blocker "kein `ContextProvider` erzeugt ein
//!    `harw_context::Fragment`" bleibt deshalb unverändert bestehen — **hier**
//!    bricht die Kette zu `FragmentReference`/`DetailMode::References`
//!    weiterhin, nicht erst bei der Montage.
//! 4. **Kein Weg von hier zu einer konkreten `ContextLoadExecutor`-Instanz.**
//!    [`find_executor`] liefert ausschließlich `Arc<dyn ToolExecutor>`
//!    (`harw_tools::executor::ToolExecutor`, definiert in
//!    `harw-tools/src/executor.rs:64`) — dieses Trait-Objekt bietet keine
//!    Möglichkeit, das konkrete `harw_tools::context_load::ContextLoadExecutor`
//!    dahinter zu erreichen. Geprüft und verworfen:
//!      - eine neue Methode auf `ToolExecutor` selbst (z. B.
//!        `fn as_context_load_executor(&self) -> Option<&ContextLoadExecutor> { None }`
//!        als Default-Methode in `harw-tools/src/executor.rs`, überschrieben
//!        in `impl ToolExecutor for ContextLoadExecutor`,
//!        `harw-tools/src/context_load.rs:539`) — berührt `harw-tools`, das
//!        nicht zum Schreibbereich dieses Knotens gehört;
//!      - ein `downcast` über `std::any::Any` in dieser Datei — technisch
//!        machbar (`ToolExecutor: Send + Sync` bräuchte zusätzlich
//!        `+ Any` bzw. eine `as_any`-Methode, ebenfalls eine Änderung in
//!        `harw-tools`), aber eine Typprüfung zur Laufzeit an einer Stelle,
//!        die sie mit der ersten Option nicht bräuchte — deshalb hier bewusst
//!        **nicht** eingebaut, um niemanden mit einem `downcast` zurückzulassen,
//!        den ein Folgeknoten erst wieder entfernen müsste.
//!
//!    Ein dritter, in diesem Schreibbereich baubarer Weg existiert nicht: jede
//!    Alternative läuft über die `Arc<dyn ToolExecutor>`-Grenze, die
//!    ausschließlich `harw-tools` definiert. Das ist der Befund für Punkt 3
//!    des Knotenauftrags — die obige Zeile ist die wörtliche Ergänzung, die in
//!    `harw-tools/src/executor.rs` nötig wäre.
//!
//! ## Dritter Nachtrag (dieser Knoten): Blocker 3 ist geschlossen — die Grenze
//! war der Schreibbereich, nicht die Machbarkeit
//!
//! `harw-tools/src/executor.rs` gehört jetzt zum Schreibbereich. Gebaut wurde
//! genau die Zeile, die Punkt 4 oben als Befund nannte:
//! `ToolExecutor::as_context_load_executor` als Default-Methode (`None`),
//! überschrieben in `impl ToolExecutor for ContextLoadExecutor`
//! (`harw-tools/src/context_load.rs`). Kein bestehender `ToolExecutor`
//! bricht dadurch — die Vorgabe ist dafür da. Kein `downcast` — aus demselben
//! Grund, den Punkt 4 oben bereits nannte.
//!
//! [`seed_context_load_ledger`] nutzt diesen Weg von einem echten
//! Eintrittspunkt aus: [`drive_turn`] ruft sie einmal je Turn auf, bevor die
//! Model-/Tool-Schleife beginnt. Sie erreicht **beide** Hälften — das
//! deklarierte `ContextProgram` über [`AgentSession::context_program`] und
//! die konkrete `ContextLoadExecutor`-Instanz über [`find_executor`] +
//! `as_context_load_executor` — und ruft `seed_turn` auf. Die dabei
//! übergebene `spent_per_section`-Map ist leer, weil Blocker 1 (kein
//! `ContextProvider` erzeugt `harw_context::Fragment`) unverändert offen ist:
//! ohne die neue Montage gibt es keine echten Kosten je Sektion zu
//! übergeben. **Hier, an genau dieser Stelle, bricht die Kette zu
//! `FragmentReference`/`DetailMode::References` jetzt weiterhin** — nicht
//! mehr am fehlenden Weg zum Ausführer.
//!
//! **Ergebnis (Stand vor diesem Knoten):** kein Produktionspfad erreicht
//! `FragmentReference` — Blocker 1 (`ContextProvider` → `harw_context::Fragment`)
//! lag außerhalb dieses Schreibbereichs.
//!
//! ## Vierter Nachtrag (dieser Knoten): Blocker 1 war eine Rückwandlung, kein
//! fehlender Erzeuger — die Montage läuft jetzt in Produktion
//!
//! Eigene Prüfung, dieser Knoten: **dieser** Knoten hat Schreibzugriff auf
//! `harw-core/src/model.rs` bekommen, den frühere Knoten nicht hatten (siehe
//! `context_budget.rs`s Moduldoku, Abschnitt „Warum der Bestandspfad
//! unangetastet bleibt" — dort ist die fehlende Berechtigung auf `model.rs`
//! als der Grund genannt, warum die Montage nicht angeschlossen wurde). Mit
//! diesem Zugriff zeigt sich: „Blocker 1" war nie ein fehlender
//! `harw_context::Fragment`-Erzeuger — [`gather_context`] ruft `contribute_v2`
//! bereits seit dem „Vierter Nachtrag" oben auf und bekommt dabei **echte**
//! `harw_context::Fragment`-Werte zurück (für jeden v1-Provider über
//! `harw_extension_api::fragment_from_v1`s dokumentierte, nicht erfundene
//! Abbildung: `trust = TrustClass::Data`, `stability = Stability::Fresh`,
//! `section` = eine feste v1-Sektion, `cost` über `BytesOverFour`). Der
//! Blocker war die **Rückwandlung**, die [`gather_context`] direkt danach auf
//! diese Fragmente anwandte, um `ModelRequest::with_context_budget`s
//! `Vec<ContextFragment>`-Signatur zu bedienen.
//!
//! [`gather_context`] liefert seit diesem Knoten `Vec<harw_context::Fragment>`
//! **ohne** diese Rückwandlung (siehe seine eigene Doku, „Fünfter Nachtrag").
//! [`drive_turn`] reicht das Ergebnis zusammen mit
//! [`AgentSession::context_program`] und
//! `AgentSession::spawn_context().and_then(|c| c.ceiling.as_ref())` an
//! [`crate::model::ModelRequest::with_context_program`] weiter
//! (`harw-core/src/model.rs`, Moduldoku „Zwei Wege zur Kontextmontage") —
//! **dort**, nicht hier, entscheidet sich, ob die AW1-03/AW4-01-Montage läuft
//! oder auf den alten Byte-Budget-Pfad zurückfällt. Diese Datei kennt die
//! Unterscheidung selbst nicht mehr; sie reicht beide Werte unbedingt durch.
//!
//! **Ergebnis:**
//! - Die Zwei-Block-Trennung nach Vertrauensklassen (AW4-01) läuft jetzt in
//!   Produktion, für jede Sitzung, die sowohl ein `ContextProgram` als auch
//!   eine geschnittene `ContextCeiling` trägt.
//! - Der Nullzähler `TRUST_BLOCK_VIOLATION` ist damit von [`drive_turn`] aus
//!   erreichbar — nicht mehr nur aus den Unit-Tests von `context_budget.rs`.
//! - `DetailMode::References`/`FragmentReference` sind ebenfalls erreichbar:
//!   `ModelRequest::with_context_program` liest
//!   `ContextProgram::section_detail()` (`model.rs`s `section_detail_map`)
//!   und reicht das Ergebnis an `render_trust_blocks_with_detail` weiter.
//! - **Weiterhin offen:** `seed_context_load_ledger` unten übergibt
//!   `seed_turn` weiterhin eine **leere** `spent_per_section`-Map, nicht das
//!   Ergebnis von `ContextAssemblyV2::spent_per_section()`. Der Grund ist
//!   jetzt ein struktureller, kein fehlender Baustein mehr:
//!   `seed_context_load_ledger` läuft **einmal**, vor der Schleife, bevor
//!   überhaupt ein `ModelRequest` gebaut wird — die Montage, die
//!   `spent_per_section` liefern würde, entsteht aber erst **innerhalb**
//!   dieser Schleife, in `with_context_program`. Sie vorzuziehen würde
//!   entweder die Montage doppelt laufen lassen (einmal zum Seeden, einmal
//!   für den ersten Model-Aufruf) oder `seed_turn` mehrfach je Turn aufrufen
//!   — beides verwirft genau die Buchhaltung, die `seed_turn`s eigene Doku
//!   ausdrücklich ausschließt (siehe oben, „ein Aufruf je Model-Runde würde
//!   … bereits verbuchte `context.load`-Aufrufe verwerfen"). Das ist ein
//!   struktureller Umbau der Aufrufreihenfolge dieser Schleife, größer als
//!   dieser Knoten rechtfertigt — dokumentiert als offener Folgeknoten, nicht
//!   halbfertig gebaut.
//!
use crate::error::{CoreError, CoreResult};
use crate::model::{ModelProvider, ModelRequest};
use crate::session::{AgentSession, SpawnContext, TurnHandle};
use crate::state_store::{StateStore, StateStoreError};
use harw_extension_api::{
    ApprovalDecision, LoadedInstructions, SpawnInput, ToolExecutor,
    TurnInputContext, TurnStartInput, TurnStopInput,
};
use harw_protocol::events::TurnEvent;
use harw_protocol::items::{AssistantMessageItem, ContentPart, ToolCallResult, TurnItem};
use harw_session_store::{ApprovalRecord, ApprovalStore};
use harw_tools::{
    ToolCall, ToolExecutionContext, ToolName, ToolOutput, ToolSpec, TracedToolExecutor,
};
use harw_types::{ApprovalActor, ReviewDecision, SessionId, ToolCallId};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use tracing::Instrument as _;

/// Präfix, an dem ein Tool-Call als Handoff erkannt wird (Agents-SDK-Muster
/// `transfer_to_<role>`).
pub const HANDOFF_PREFIX: &str = "transfer_to_";

/// Ergebnis-Slot für einen parallelen Tool-Call: (ID, Ergebnis, Wandzeit ms).
type ParallelCallSlot = Option<(ToolCallId, ToolCallResult, u64)>;

/// Eingabe, mit der ein Turn gestartet wird.
#[derive(Debug, Clone, Default)]
pub struct TurnInput {
    /// Optionaler User-Text, der zu Beginn in den Verlauf gespielt wird.
    pub user_text: Option<String>,
    /// Metadaten, die dem `TurnInputContext` mitgegeben werden.
    pub metadata: serde_json::Value,
}

impl TurnInput {
    #[must_use]
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            user_text: Some(text.into()),
            metadata: serde_json::Value::Null,
        }
    }
}

/// Wie ein Turn endete.
#[derive(Debug)]
pub enum TurnOutcome {
    /// Turn vollständig abgeschlossen (keine Tool-Calls mehr).
    Completed,
    /// Ein Handoff-Tool wurde gerufen — die Session wartet jetzt auf das Child.
    /// Wiederaufnahme über `resume_after_child`.
    AwaitingChild {
        child: SessionId,
        call_id: ToolCallId,
        role: String,
    },
    /// Ein Guardrail verlangt eine Nutzer-Entscheidung (`AskUser`). Der Turn
    /// pausiert, bis die Entscheidung über den Channel zurückkommt.
    AwaitingApproval {
        call_id: ToolCallId,
        request: harw_types::ItemId,
    },
}

/// Resolution supplied by the user for a paused approval request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalResolution {
    Approve,
    Reject { reason: String },
}

impl From<ReviewDecision> for ApprovalResolution {
    fn from(value: ReviewDecision) -> Self {
        match value {
            ReviewDecision::Approved | ReviewDecision::ApprovedOnce => Self::Approve,
            ReviewDecision::Rejected => Self::Reject {
                reason: "rejected by user".to_owned(),
            },
        }
    }
}

/// Sammelt Kontext von allen `ContextProvider`n und filtert nach der
/// session-level [`SessionActivation`][crate::activation::SessionActivation].
///
/// # Description
/// Iterates every registered `ContextProvider`, but calls
/// [`harw_extension_api::ContextProvider::contribute_v2`] instead of the
/// legacy `contribute` — **this is the production caller referenced in this
/// module's "Zweiter Nachtrag"/"Dritter Nachtrag" sections**: every provider
/// that does not override `contribute_v2` still runs through it via its
/// default bridge (`fragment_from_v1`), so `contribute_v2` is now reached on
/// every turn, not only from `harw-extension-api`'s own tests.
///
/// **Fünfter Nachtrag (dieser Knoten):** vor diesem Knoten projizierte diese
/// Funktion jedes zurückgegebene [`harw_context::Fragment`] sofort zurück auf
/// das alte [`ContextFragment`] (`label` + `content` ← `label.as_str()` +
/// `body`), weil `ModelRequest::with_context_budget`
/// (`harw-core/src/model.rs`) ausschließlich `ContextFragment` annahm. Dieser
/// Knoten hat Schreibzugriff auf `model.rs` bekommen und dort
/// `ModelRequest::with_context_program` ergänzt, das native
/// `harw_context::Fragment`s entgegennimmt (siehe dessen Moduldoku, Abschnitt
/// „Zwei Wege zur Kontextmontage") — die Rückwandlung ist deshalb **nicht
/// mehr hier** nötig; sie passiert (wenn überhaupt) jetzt in `model.rs`s
/// Rückfallpfad, für eine Sitzung ohne Programm oder ohne Decke. Diese
/// Funktion liefert seit diesem Knoten die von `contribute_v2` gelieferten
/// `harw_context::Fragment`s **unverändert** (nach dem Aktivierungsfilter)
/// zurück — keine Information geht mehr auf dem Weg zu `drive_turn` verloren.
///
/// Fragmente werden weiterhin exakt wie vor diesem Knoten gefiltert: ein
/// Fragment, dessen Label in der Aktivierung der Sitzung deaktiviert ist,
/// wird verworfen. Ein v1-Fragment mit **leerem** Label wird von
/// `contribute_v2`s Vorgabe-Bridge bereits vorher verworfen (bei der
/// `FragmentLabel::try_new`-Prüfung) — siehe
/// `harw_extension_api::fragment_from_v1`s eigene Dokumentation; kein
/// Provider oder Test in diesem Workspace konstruiert ein leeres Label.
///
/// # Arguments
/// - `session` (`&AgentSession`): session providing both the registry and the
///   activation filter.
/// - `ctx` (`&TurnInputContext`): turn metadata forwarded to each provider.
///
/// # Returns
/// Filtered list of [`harw_context::Fragment`]s that are model-visible, in
/// arrival order (order has no effect on the deterministic montage in
/// `crate::context_budget::Assembly`).
///
/// # Concurrency
/// `async`; no locks held across `.await` points.
pub async fn gather_context(
    session: &AgentSession,
    ctx: &TurnInputContext,
) -> Vec<harw_context::Fragment> {
    let activation = session.activation();
    let mut fragments = Vec::new();
    for provider in session.registry().context_providers() {
        let contributed = provider.contribute_v2(ctx).await;
        for fragment in contributed {
            let label_str = fragment.label.as_str();
            let filter_label = if label_str.is_empty() {
                "unlabeled"
            } else {
                label_str
            };
            if activation.is_context_enabled(filter_label) {
                fragments.push(fragment);
            }
        }
    }
    fragments
}

/// Lädt Instructions von allen `InstructionsProvider` und filtert nach der
/// session-level [`SessionActivation`][crate::activation::SessionActivation].
///
/// # Description
/// Iterates every registered `InstructionsProvider` and merges their output
/// into a single [`LoadedInstructions`]. The session activation is applied
/// using label conventions:
/// - The first non-empty `system_prompt` across all providers uses the label
///   `"baseline"`. Subsequent non-empty system prompts (if any provider
///   returned one and `combined.system_prompt` is already set) would use
///   `"extra_<index>"`, but the underlying merge already ignores them.
/// - Extra `fragments` from each provider use the label
///   `"fragment_<provider_index>_<fragment_index>"`.
///
/// If a label is disabled via [`SessionActivation::disable_instructions`], the
/// corresponding content is dropped.
///
/// # Arguments
/// - `session` (`&AgentSession`): session providing both the registry and the
///   activation filter.
///
/// # Returns
/// Merged, filtered [`LoadedInstructions`].
///
/// # Concurrency
/// `async`; no locks held across `.await` points.
pub async fn load_instructions(session: &AgentSession) -> LoadedInstructions {
    let activation = session.activation();
    let mut combined = LoadedInstructions::default();
    for (provider_idx, provider) in session
        .registry()
        .instructions_providers()
        .iter()
        .enumerate()
    {
        let loaded = provider.load().await;
        // System prompt: first non-empty one wins with label "baseline";
        // subsequent providers' system_prompts are implicitly labelled
        // "extra_<n>" and skipped if already combined (existing logic).
        if !loaded.system_prompt.is_empty()
            && combined.system_prompt.is_empty()
            && activation.is_instructions_enabled("baseline")
        {
            combined.system_prompt = loaded.system_prompt;
        }
        // Fragments: each gets a deterministic label for filtering.
        for (frag_idx, fragment) in loaded.fragments.into_iter().enumerate() {
            let label = format!("fragment_{provider_idx}_{frag_idx}");
            if activation.is_instructions_enabled(&label) {
                combined.fragments.push(fragment);
            }
        }
    }
    combined
}

/// Benachrichtigt alle Turn-Observer über Start.
pub fn notify_turn_start(session: &AgentSession, input: &TurnStartInput) {
    for observer in session.registry().turn_observers() {
        observer.on_turn_start(input);
    }
}

/// Benachrichtigt alle Turn-Observer über Stop.
pub fn notify_turn_stop(session: &AgentSession, input: &TurnStopInput) {
    for observer in session.registry().turn_observers() {
        observer.on_turn_stop(input);
    }
}

/// Sendet ein [`TurnEvent`] an den optionalen Live-Event-Sink der Session.
/// No-op, wenn kein Sink konfiguriert ist oder der Empfänger bereits
/// abgehängt hat (Best-Effort, blockiert den Turn-Hot-Path nie).
fn emit(session: &AgentSession, event: TurnEvent) {
    if let Some(tx) = session.turn_event_tx() {
        let _ = tx.send(event);
    }
}

/// Prüft einen `ToolCall` gegen alle `ApprovalHandler`.
///
/// Der erste Handler, der nicht `Allow` zurückgibt, gewinnt (Deny/AskUser
/// haben Vorrang). Ohne Handler gilt `Allow`.
///
/// # Invariante
/// Ein `ApprovalHandler` wird pro Tool-Call **höchstens einmal** befragt: er
/// darf Nutzer-Interaktion auslösen, eine Approval-`ItemId` vergeben oder einen
/// Audit-Eintrag schreiben. Wer diese Funktion außerhalb des sequenziellen
/// Tool-Loops aufruft (siehe [`PreparedApprovals`]), muss die Entscheidung
/// deshalb weiterreichen statt sie später erneut einzuholen.
pub async fn check_approval(session: &AgentSession, call: &ToolCall) -> ApprovalDecision {
    for handler in session.registry().approval_handlers() {
        let decision = handler.review(call).await;
        match &decision {
            ApprovalDecision::Allow => continue,
            _ => return decision,
        }
    }
    ApprovalDecision::Allow
}

/// Guardrail-Entscheidungen, die für eine Modellantwort bereits eingeholt
/// wurden, aber noch nicht ausgewertet sind.
///
/// # Beschreibung
/// Der Parallel-Pfad muss jeden Call **vor** dem Start prüfen — sonst würde ein
/// `Deny` erst bemerkt, wenn das Werkzeug bereits läuft. Der sequenzielle Pfad
/// prüft dagegen jeden Call an seiner natürlichen Stelle. Damit ein
/// `ApprovalHandler` nie zweimal zur selben Modellantwort befragt wird, legt die
/// Vorprüfung ihre Entscheidungen hier ab; der sequenzielle Fallback verbraucht
/// sie, statt erneut zu fragen (siehe [`check_approval`]).
///
/// Die Vorprüfung bricht beim ersten Nicht-`Allow` ab. Für die dahinter
/// liegenden Calls steht deshalb kein Eintrag bereit — die werden im
/// sequenziellen Pfad ganz normal, also ebenfalls genau einmal, geprüft.
///
/// # Nebenläufigkeit
/// Reiner Datenhalter ohne innere Veränderlichkeit; lebt genau eine
/// Modellantwort lang auf dem Stack von `drive_turn`.
#[derive(Debug, Default)]
struct PreparedApprovals {
    /// Entscheidung je Call-Position der Modellantwort. `None` heißt „noch
    /// nicht gefragt", ein verbrauchter Eintrag wird wieder zu `None`.
    decisions: Vec<Option<ApprovalDecision>>,
}

impl PreparedApprovals {
    /// Entnimmt die für `index` bereits eingeholte Entscheidung.
    ///
    /// # Returns
    /// `Some(decision)`, wenn die Vorprüfung diesen Call schon geprüft hat —
    /// der Eintrag ist danach verbraucht. `None`, wenn der Aufrufer selbst
    /// fragen muss.
    fn take(&mut self, index: usize) -> Option<ApprovalDecision> {
        self.decisions.get_mut(index).and_then(Option::take)
    }
}

/// Liest die optionale Leitfrage eines Handoff-Calls aus dessen Argumenten.
///
/// # Beschreibung
/// Nur ein JSON-String unter dem Schlüssel `"question"` zählt. Jede andere Form
/// (Objekt, Zahl, fehlender Schlüssel) ergibt `None`: die Argumente stammen vom
/// Modell, und ein Ereignisfeld darf keinen Wert behaupten, den das Modell so
/// nicht geliefert hat.
///
/// # Arguments
/// - `arguments` (`&serde_json::Value`): die Argumente des Handoff-Calls.
///
/// # Returns
/// `Some(frage)` als eigener `String`, sonst `None`.
fn child_question(arguments: &serde_json::Value) -> Option<String> {
    arguments
        .get("question")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
}

/// Sammelt alle Tool-Specs über alle `ToolProvider` und filtert nach der
/// session-level [`SessionActivation`][crate::activation::SessionActivation].
///
/// # Description
/// Iterates every registered `ToolProvider`, collects their [`ToolSpec`]s, and
/// removes any tool whose name is disabled by the session activation. Duplicate
/// tool names across providers are still rejected with
/// [`CoreError::DuplicateTool`] regardless of activation state (a disabled tool
/// that shares a name with an enabled one is still a registry error).
///
/// # Arguments
/// - `session` (`&AgentSession`): session providing both the registry and the
///   activation filter.
///
/// # Returns
/// Filtered, de-duplicated list of [`ToolSpec`]s, or a
/// [`CoreError::DuplicateTool`] if any tool name appears more than once.
///
/// # Errors
/// - [`CoreError::DuplicateTool`]: two or more providers registered a tool
///   with the same name.
pub fn collect_tools(session: &AgentSession) -> CoreResult<Vec<ToolSpec>> {
    let activation = session.activation();
    let mut specs = Vec::new();
    let mut names = BTreeSet::new();
    for provider in session.registry().tool_providers() {
        for spec in provider.tools() {
            let name = match &spec {
                ToolSpec::Function(function) => function.name.as_str(),
            };
            if !names.insert(name.to_owned()) {
                return Err(CoreError::DuplicateTool {
                    name: name.to_owned(),
                });
            }
            let tool_name = match &spec {
                ToolSpec::Function(f) => &f.name,
            };
            if activation.is_tool_enabled(tool_name) {
                specs.push(spec);
            }
        }
    }
    Ok(specs)
}

/// Sucht den zuständigen `ToolExecutor` für einen Tool-Namen.
///
/// # Description
/// Returns a cloned `Arc` so no borrow on the session is held across the
/// (async) tool execution boundary. Returns `None` if the tool is disabled by
/// the session's [`SessionActivation`][crate::activation::SessionActivation] or
/// if no provider claims the tool.
///
/// # Arguments
/// - `session` (`&AgentSession`): session providing both the registry and the
///   activation filter.
/// - `name` (`&ToolName`): the tool to look up.
///
/// # Returns
/// `Some(executor)` if the tool is enabled and a provider declares an executor
/// for it; `None` otherwise.
///
/// # Concurrency
/// Synchronous; safe to call from any thread while holding a shared reference
/// to the session.
pub fn find_executor(session: &AgentSession, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
    if !session.activation().is_tool_enabled(name) {
        return None;
    }
    session
        .registry()
        .tool_providers()
        .iter()
        .find_map(|p| p.executor(name))
}

/// Resolve an executor only when its provider explicitly declares the tool
/// safe for concurrent execution.
///
/// # Description
/// Like [`find_executor`] but additionally requires the provider to declare
/// the tool as `parallel_safe`. Also checks the session's
/// [`SessionActivation`][crate::activation::SessionActivation]: a disabled tool
/// is never returned even if the provider marks it parallel-safe.
///
/// # Arguments
/// - `session` (`&AgentSession`): session providing both the registry and the
///   activation filter.
/// - `name` (`&ToolName`): the tool to look up.
///
/// # Returns
/// `Some(executor)` if the tool is enabled, registered, and parallel-safe;
/// `None` otherwise.
///
/// # Concurrency
/// Synchronous; safe to call from any thread while holding a shared reference
/// to the session.
pub fn find_parallel_executor(
    session: &AgentSession,
    name: &ToolName,
) -> Option<Arc<dyn ToolExecutor>> {
    if !session.activation().is_tool_enabled(name) {
        return None;
    }
    session
        .registry()
        .tool_providers()
        .iter()
        .find_map(|provider| {
            provider
                .parallel_safe(name)
                .then(|| provider.executor(name))
                .flatten()
        })
}

/// Sucht den registrierten `context.load`-Ausführer über [`find_executor`] und
/// belegt sein Kassenbuch für den laufenden Turn vor, sofern die Sitzung ein
/// `ContextProgram` deklariert.
///
/// # Beschreibung
///
/// Schließt den in der Moduldoku (§ "Zweiter Nachtrag", Punkt 4) benannten
/// Weg: von einem echten Eintrittspunkt — [`drive_turn`], vor dem ersten
/// Model-Aufruf eines Turns — über [`find_executor`] und die neue
/// `harw_tools::ToolExecutor::as_context_load_executor`-Methode zu einer
/// konkreten [`harw_tools::ContextLoadExecutor`]-Instanz, und über
/// [`AgentSession::context_program`] zum deklarierten `ContextProgram`
/// derselben Sitzung. **Beide Hälften sind damit von hier aus erreichbar** —
/// die Frage aus dem Knotenauftrag ist positiv beantwortet.
///
/// Aufgerufen wird `seed_turn` mit einer **leeren** `spent_per_section`-Map,
/// nicht mit tatsächlich verbrauchten Kosten je Sektion:
/// [`gather_context`] läuft weiterhin über den alten
/// `Vec<harw_extension_api::ContextFragment>`-Pfad (§ Moduldoku, "Zweiter
/// Nachtrag", Punkt 3) — die neue, `harw_context::Fragment`-bewusste Montage
/// (`crate::context_budget::Assembly<..>`), die tatsächliche Kosten je
/// Sektion kennen würde, läuft in dieser Datei nicht. Eine leere Map ist
/// deshalb keine Vereinfachung, sondern die exakte Angabe: die Montage
/// dieses Turns hat im neuen Kosten-Schema bislang nichts ausgegeben, weil
/// sie das neue Kosten-Schema noch nicht bedient. **Hier bricht die Kette zu
/// `DetailMode::References` deshalb weiterhin** — nicht mehr am fehlenden
/// Weg zum Ausführer (das schließt diese Funktion), sondern unverändert am
/// fehlenden `ContextProvider`, der `harw_context::Fragment` produziert
/// (Blocker 1 der Moduldoku, außerhalb dieses Schreibbereichs). Sobald dieser
/// Provider existiert, ersetzt die tatsächliche `spent_per_section` diese
/// leere Map.
///
/// Eine Sitzung ohne deklariertes `ContextProgram`
/// (`AgentSession::context_program() == None`) bricht diese Funktion sofort
/// ab, ohne `find_executor` überhaupt aufzurufen, und verhält sich damit
/// unverändert — die Auflage, unter der der Vorgängerknoten
/// `with_context_program` eingeführt hat, bleibt gewahrt. Ebenso ein no-op,
/// wenn `context.load` gar nicht registriert oder per
/// `SessionActivation` deaktiviert ist ([`find_executor`] liefert dann
/// `None`).
///
/// # Arguments
/// - `session` (`&AgentSession`): liefert sowohl das optionale
///   `ContextProgram` als auch (über [`find_executor`]) den registrierten
///   `context.load`-Ausführer.
/// - `turn_id` (`&harw_types::TurnId`): der Turn, dessen Kassenbuch vorbelegt
///   wird.
///
/// # Concurrency
/// Synchron; `ContextLoadExecutor::seed_turn` serialisiert intern über ein
/// `Mutex`.
fn seed_context_load_ledger(session: &AgentSession, turn_id: &harw_types::TurnId) {
    if session.context_program().is_none() {
        return;
    }
    let tool_name = ToolName::new(harw_tools::CONTEXT_LOAD_TOOL_NAME);
    let Some(executor) = find_executor(session, &tool_name) else {
        return;
    };
    let Some(context_load_executor) = executor.as_context_load_executor() else {
        return;
    };
    let spent_per_section: BTreeMap<harw_context::SectionName, u32> = BTreeMap::new();
    context_load_executor.seed_turn(turn_id.clone(), spent_per_section);
}

/// Erkennt, ob ein Tool-Call ein Handoff ist (`transfer_to_<role>`).
#[must_use]
pub fn handoff_role(name: &ToolName) -> Option<String> {
    name.as_str()
        .strip_prefix(HANDOFF_PREFIX)
        .filter(|rest| !rest.is_empty())
        .map(ToOwned::to_owned)
}

/// Übersetzt eine `ToolOutput` in das explizite Wire-Ergebnis eines
/// `ToolResultItem`.
fn output_to_result(output: ToolOutput) -> ToolCallResult {
    match output {
        ToolOutput::Text { content } => ToolCallResult::success(serde_json::Value::String(content)),
        ToolOutput::Json { content } => ToolCallResult::success(content),
        ToolOutput::Error { message } => ToolCallResult::error(message),
    }
}

/// Builds the authority passed across the final tool-execution boundary.
///
/// This is deliberately derived from session state rather than `TurnInput`
/// or `ToolCall.arguments`: both of those can originate with untrusted model
/// or channel input. A session with tools but no resolved sandbox is rejected
/// instead of falling back to ambient host permissions.
fn tool_execution_context(
    session: &AgentSession,
    ctx: &TurnInputContext,
) -> CoreResult<ToolExecutionContext> {
    let sandbox = session
        .spawn_context()
        .map(|context| context.sandbox.clone())
        .ok_or_else(|| CoreError::MissingToolExecutionContext {
            session_id: session.id().to_string(),
        })?;
    Ok(ToolExecutionContext::new(
        ctx.session_id.clone(),
        ctx.turn_id.clone(),
        sandbox,
    ))
}

/// Converts the one recoverable authority-boundary rejection into a result
/// the requesting model can observe and react to. The typed error is retained
/// for every other caller; no executor is ever reached without this context.
fn missing_tool_execution_context_result(error: CoreError) -> CoreResult<ToolCallResult> {
    match error {
        missing @ CoreError::MissingToolExecutionContext { .. } => {
            Ok(ToolCallResult::error(missing.to_string()))
        }
        other => Err(other),
    }
}

/// Retrieves immutable child authority. Handoffs are also execution: allowing
/// one without a resolved parent context would let a legacy spawner create a
/// child with ambient permissions.
fn governed_spawn_context(session: &AgentSession) -> CoreResult<SpawnContext> {
    session
        .spawn_context()
        .cloned()
        .ok_or_else(|| CoreError::MissingToolExecutionContext {
            session_id: session.id().to_string(),
        })
}

// ---------------------------------------------------------------------------
// Der eigentliche Turn-Loop
// ---------------------------------------------------------------------------

/// Führt einen kompletten Turn aus: von `Idle` über den Model-/Tool-Zyklus bis
/// zum Turn-Ende — oder bis zu einem Handoff-/Approval-Pausepunkt.
///
/// Schritte: Context → Instructions → Observer → Model-Call → Tool-Call-Loop
/// (Guardrail → Handoff-Detection → Execute → ToolResult → next Model-Call)
/// → Turn-Ende. A normal `ToolExecutor`, including one that wraps a bounded
/// micro-agent, returns its output to this requesting session as a
/// `ToolResult`; the loop then invokes this same session's model again. A
/// `transfer_to_*` call is deliberately different: it transfers ownership and
/// pauses this session for [`resume_after_child`].
pub async fn run_turn(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    input: TurnInput,
) -> CoreResult<TurnOutcome> {
    run_turn_with_approvals(session, model, store, None, input).await
}

/// Runs a turn while durably recording every approval request before it is
/// exposed to a channel. Production channels must use this entrypoint; the
/// plain [`run_turn`] function remains a lightweight test/demo seam.
pub async fn run_turn_durable(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    approvals: &ApprovalStore,
    input: TurnInput,
) -> CoreResult<TurnOutcome> {
    run_turn_with_approvals(session, model, store, Some(approvals), input).await
}

async fn run_turn_with_approvals(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    approvals: Option<&ApprovalStore>,
    input: TurnInput,
) -> CoreResult<TurnOutcome> {
    let handle = session
        .try_start_turn()
        .map_err(|r| CoreError::TurnRejected(r.to_string()))?;
    let turn_id = handle.turn_id.clone();
    let session_id = handle.session_id.clone();

    // Outer span covering the entire turn's lifecycle.
    let turn_span = tracing::info_span!(
        "agent.turn",
        turn_id = %turn_id,
        session_id = %session_id,
    );
    let _turn_guard = turn_span.enter();

    let ctx = TurnInputContext {
        session_id: session_id.clone(),
        turn_id: turn_id.clone(),
        metadata: input.metadata,
    };

    // A freshly-created in-memory session may be resuming a durable session.
    // Hydrate it before appending the new user item so the model sees the
    // complete transcript and the live session remains the single append
    // authority for the rest of this turn.
    if input.user_text.is_some() && session.history().is_empty() {
        let history = match store.load_history(session.id()).await {
            Ok(history) => history,
            Err(error) => {
                let error = state_store_error(error, "history load");
                transition_after_turn_failure(session, &ctx, &error);
                return Err(error);
            }
        };
        *session.history_mut() = history;
    }

    // User-Input in den Verlauf spielen und persistieren.
    if let Some(text) = input.user_text {
        session.history_mut().push_user_text(text);
        if let Err(error) = persist_last(session, store).await {
            transition_after_turn_failure(session, &ctx, &error);
            return Err(error);
        }
    }

    notify_turn_start(
        session,
        &TurnStartInput {
            session_id,
            turn_id,
        },
    );

    let result = drive_turn(session, model, store, approvals, &ctx, handle).await;
    if let Err(error) = &result {
        transition_after_turn_failure(session, &ctx, error);
    }
    result
}

/// Returns the session to `Idle` after a provider-declared retryable failure,
/// so a caller can submit the same valid session again. All other failures are
/// terminal because they may indicate a broken invariant or unsafe execution
/// boundary.
fn transition_after_turn_failure(
    session: &mut AgentSession,
    ctx: &TurnInputContext,
    error: &CoreError,
) {
    let retryable = matches!(
        error,
        CoreError::Model(crate::model::ModelError::RateLimited { .. })
    );

    if retryable {
        session.complete_turn(
            TurnHandle {
                turn_id: ctx.turn_id.clone(),
                session_id: ctx.session_id.clone(),
            },
            harw_types::TokenUsage::default(),
        );
    } else {
        session.fail(error.to_string());
    }

    emit(
        session,
        TurnEvent::TurnFailed {
            turn_id: ctx.turn_id.clone(),
            reason: error.to_string(),
            retryable,
        },
    );
}

/// Nimmt einen pausierten Turn nach Abschluss eines Child-Handoffs wieder auf.
///
/// Das Child-Ergebnis wird als `ToolResult` in den Eltern-Verlauf gespielt
/// (Agents-SDK-Handoff-Semantik), dann läuft der Model-/Tool-Zyklus weiter.
pub async fn resume_after_child(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    child: SessionId,
    call_id: ToolCallId,
    child_result: ToolCallResult,
) -> CoreResult<TurnOutcome> {
    resume_after_child_with_approvals(session, model, store, None, child, call_id, child_result)
        .await
}

/// Durable counterpart to [`resume_after_child`]. Use it when the parent was
/// started through [`run_turn_durable`] so any subsequent approval pause is
/// persisted in the same authority store.
pub async fn resume_after_child_durable(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    approvals: &ApprovalStore,
    child: SessionId,
    call_id: ToolCallId,
    child_result: ToolCallResult,
) -> CoreResult<TurnOutcome> {
    resume_after_child_with_approvals(
        session,
        model,
        store,
        Some(approvals),
        child,
        call_id,
        child_result,
    )
    .await
}

async fn resume_after_child_with_approvals(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    approvals: Option<&ApprovalStore>,
    child: SessionId,
    call_id: ToolCallId,
    child_result: ToolCallResult,
) -> CoreResult<TurnOutcome> {
    let turn_id = session
        .current_turn()
        .cloned()
        .ok_or_else(|| CoreError::TurnRejected("resume without an active turn".to_owned()))?;
    let session_id = session.id().clone();

    // Re-enter the same logical turn span after child completion.
    let turn_span = tracing::info_span!(
        "agent.turn",
        turn_id = %turn_id,
        session_id = %session_id,
    );
    let _turn_guard = turn_span.enter();

    let ctx = TurnInputContext {
        session_id: session_id.clone(),
        turn_id: turn_id.clone(),
        metadata: serde_json::Value::Null,
    };

    let call_id_for_completion = call_id.clone();
    session
        .history_mut()
        .push_tool_result(call_id, child_result, 0);
    if let Err(error) = persist_last(session, store).await {
        transition_after_turn_failure(session, &ctx, &error);
        return Err(error);
    }

    // Do not release the child's admission until its terminal ToolResult is
    // durable in the parent transcript. A failed write leaves the lease intact
    // for recovery. The durable path lets the spawner durably complete that
    // lease before releasing it; the legacy path retains child_finished.
    session.child_completed(&child, &call_id_for_completion)?;

    // Erst hier melden: das Ergebnis ist persistiert *und* die Session hat die
    // Korrelation gegen ihren offenen Handoff bestätigt. Vorher wäre es die
    // Meldung eines Kindes, das gar nicht das erwartete sein muss.
    //
    // `duration_ms = 0`: die Laufzeit des Kindes ist an dieser Stelle nicht
    // verfügbar. Der Eltern-Turn kennt weder den Spawn-Zeitpunkt noch eine vom
    // Spawner durchgereichte Dauer; geraten wird nichts. `outcome` ist aus
    // demselben Grund konstant "completed" — dieser Pfad wird nur betreten,
    // wenn das Kind ein terminales Ergebnis geliefert hat. Ein Abbruch oder
    // Budget-Überlauf müsste vom Spawner explizit durchgereicht werden.
    emit(
        session,
        TurnEvent::ChildCompleted {
            turn_id: turn_id.clone(),
            child: child.clone(),
            outcome: "completed".to_owned(),
            duration_ms: 0,
        },
    );
    if let Some(spawner) = session.registry().spawner() {
        if approvals.is_some() {
            let completion = spawner
                .child_completed(&child, jiff::Timestamp::now())
                .map_err(|error| CoreError::HandoffFailed {
                    role: "child completion".to_owned(),
                    reason: error.to_string(),
                });
            if let Err(error) = completion {
                transition_after_turn_failure(session, &ctx, &error);
                return Err(error);
            }
        } else {
            spawner.child_finished(&child);
        }
    }

    let handle = TurnHandle {
        turn_id,
        session_id,
    };

    let result = drive_turn(session, model, store, approvals, &ctx, handle).await;
    if let Err(error) = &result {
        transition_after_turn_failure(session, &ctx, error);
    }
    result
}

/// Resume an approval pause using the exact tool call captured by the session.
/// This prevents an approval callback from substituting different arguments.
pub async fn resume_after_approval(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    actor: ApprovalActor,
    resolution: ApprovalResolution,
) -> CoreResult<TurnOutcome> {
    resume_after_approval_with_store(session, model, store, None, actor, resolution).await
}

/// Resumes an approval only after atomically consuming the durable request.
/// Duplicate callbacks, mismatched actors, and post-restart replays therefore
/// cannot execute the stored tool call a second time.
pub async fn resume_after_approval_durable(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    approvals: &ApprovalStore,
    actor: ApprovalActor,
    resolution: ApprovalResolution,
) -> CoreResult<TurnOutcome> {
    resume_after_approval_with_store(session, model, store, Some(approvals), actor, resolution)
        .await
}

async fn resume_after_approval_with_store(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    approvals: Option<&ApprovalStore>,
    actor: ApprovalActor,
    resolution: ApprovalResolution,
) -> CoreResult<TurnOutcome> {
    if let Some(approvals) = approvals {
        let pending = session.pending_approval().ok_or_else(|| {
            CoreError::TurnRejected("durable approval resume without a pending request".to_owned())
        })?;
        let decision = match &resolution {
            ApprovalResolution::Approve => ReviewDecision::Approved,
            ApprovalResolution::Reject { .. } => ReviewDecision::Rejected,
        };
        let comment = match &resolution {
            ApprovalResolution::Approve => None,
            ApprovalResolution::Reject { reason } => Some(reason.clone()),
        };
        approvals.resolve(
            session.id(),
            &pending.request,
            &actor,
            decision,
            comment,
            jiff::Timestamp::now(),
        )?;
    }
    let pending = session.resolve_approval(&actor)?;
    let turn_id = session.current_turn().cloned().ok_or_else(|| {
        CoreError::TurnRejected("approval resume without an active turn".to_owned())
    })?;
    let session_id = session.id().clone();

    // Re-enter the logical turn span after approval resolution.
    let turn_span = tracing::info_span!(
        "agent.turn",
        turn_id = %turn_id,
        session_id = %session_id,
    );
    let _turn_guard = turn_span.enter();

    let ctx = TurnInputContext {
        session_id: session_id.clone(),
        turn_id: turn_id.clone(),
        metadata: serde_json::Value::Null,
    };
    let handle = TurnHandle {
        turn_id,
        session_id,
    };

    match resolution {
        ApprovalResolution::Reject { reason } => {
            session.history_mut().push_tool_result(
                pending.call.id,
                ToolCallResult::error(format!("denied by user: {reason}")),
                0,
            );
            persist_last(session, store).await?;
            drive_turn(session, model, store, approvals, &ctx, handle).await
        }
        ApprovalResolution::Approve => {
            if let Some(role) = handoff_role(&pending.call.name) {
                let spawner = session.registry().spawner().cloned().ok_or_else(|| {
                    CoreError::HandoffFailed {
                        role: role.clone(),
                        reason: "no AgentSpawner registered".to_owned(),
                    }
                })?;
                // Vor dem Verschieben der Argumente in den SpawnInput lesen.
                let question = child_question(&pending.call.arguments);
                let input = SpawnInput {
                    parent_session_id: session.id().clone(),
                    handoff_call_id: pending.call.id.clone(),
                    instructions: None,
                    context: pending.call.arguments,
                    // Hereditär, nie neu erfunden: dieser Handoff deklariert
                    // selbst keine eigene Kontextdecke (`call.arguments`
                    // trägt keine), also reicht dieses Feld exakt die bereits
                    // geschnittene Decke des laufenden Turns durch — dieselbe
                    // Regel, mit der `ManagedAgentSpawner::admit`
                    // (`child_controller.rs`) Sandbox und Trace im selben
                    // Schritt vererbt. `ContextCeiling::default()` stünde
                    // hier für eine erfundene Grenze, die nichts mit der
                    // tatsächlichen Autorität dieser Sitzung zu tun hätte.
                    ceiling: session
                        .spawn_context()
                        .and_then(|spawn_context| spawn_context.ceiling.clone()),
                };
                let context = match governed_spawn_context(session) {
                    Ok(context) => context,
                    Err(error) => {
                        let result = missing_tool_execution_context_result(error)?;
                        session
                            .history_mut()
                            .push_tool_result(pending.call.id, result, 0);
                        persist_last(session, store).await?;
                        return drive_turn(session, model, store, approvals, &ctx, handle).await;
                    }
                };
                let child = spawner
                    .spawn_child(&role, input, context.sandbox, context.suggestions)
                    .await
                    .map_err(|error| CoreError::HandoffFailed {
                        role: role.clone(),
                        reason: error.to_string(),
                    })?;
                session.begin_handoff(child.clone(), pending.call.id.clone(), role.clone())?;
                // Ein genehmigungspflichtiger Handoff wird ausschließlich hier
                // gespawnt (`drive_turn` kehrt vorher mit `AwaitingApproval`
                // zurück). Ohne dieses Event bliebe ein solches Kind für jeden
                // Beobachter unsichtbar; doppelt gemeldet wird es nicht.
                emit(
                    session,
                    TurnEvent::ChildSpawned {
                        turn_id: ctx.turn_id.clone(),
                        child: child.clone(),
                        role: role.clone(),
                        question,
                    },
                );
                Ok(TurnOutcome::AwaitingChild {
                    child,
                    call_id: pending.call.id,
                    role,
                })
            } else {
                let tool_name = pending.call.name.to_string();
                let tool_span = tracing::info_span!("tool.call", tool_name = %tool_name);
                emit(
                    session,
                    TurnEvent::ToolCallRequested {
                        turn_id: ctx.turn_id.clone(),
                        call_id: pending.call.id.clone(),
                        tool_name: tool_name.clone(),
                        arguments: pending.call.arguments.clone(),
                    },
                );
                let result: (ToolCallResult, u64) = async {
                    match find_executor(session, &pending.call.name) {
                        Some(executor) => {
                            let (result, duration_ms) = match tool_execution_context(session, &ctx)
                            {
                                Ok(execution_context) => {
                                    let started = std::time::Instant::now();
                                    let output = executor
                                        .traced_execute(&execution_context, &pending.call)
                                        .await;
                                    let duration_ms = started.elapsed().as_millis() as u64;
                                    let result =
                                        output.map(output_to_result).unwrap_or_else(|error| {
                                            ToolCallResult::error(error.to_string())
                                        });
                                    (result, duration_ms)
                                }
                                Err(error) => (missing_tool_execution_context_result(error)?, 0),
                            };
                            tracing::info!(
                                duration_ms = duration_ms,
                                status = if result.is_success() { "ok" } else { "err" },
                                "tool.execute",
                            );
                            Ok::<_, CoreError>((result, duration_ms))
                        }
                        None => {
                            tracing::info!(duration_ms = 0u64, status = "err", "tool.execute",);
                            Ok::<_, CoreError>((
                                ToolCallResult::error(format!(
                                    "no executor for tool '{}'",
                                    pending.call.name
                                )),
                                0u64,
                            ))
                        }
                    }
                }
                .instrument(tool_span)
                .await?;
                emit(
                    session,
                    TurnEvent::ToolCallCompleted {
                        turn_id: ctx.turn_id.clone(),
                        call_id: pending.call.id.clone(),
                        result: result.0.clone(),
                        duration_ms: result.1,
                    },
                );
                session
                    .history_mut()
                    .push_tool_result(pending.call.id, result.0, result.1);
                persist_last(session, store).await?;
                drive_turn(session, model, store, approvals, &ctx, handle).await
            }
        }
    }
}

/// Der Model-/Tool-Zyklus. Setzt `Running`-State voraus.
///
/// Emits the `model.request`, `model.response`, `tool.call`, `tool.execute`,
/// and `approval.wait` tracing events and spans described in the module doc.
/// Must be called while an `agent.turn` span is already entered by the caller.
async fn drive_turn(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    store: &dyn StateStore,
    approvals: Option<&ApprovalStore>,
    ctx: &TurnInputContext,
    handle: TurnHandle,
) -> CoreResult<TurnOutcome> {
    let mut total_usage = harw_types::TokenUsage::default();

    // Einmal je Turn, vor dem ersten Model-Aufruf: das Kassenbuch des
    // `context.load`-Ausführers vorbelegen, sofern die Sitzung ein
    // `ContextProgram` deklariert (siehe `seed_context_load_ledger`-Doku).
    // Bewusst außerhalb der Schleife unten — `ContextLoadExecutor::seed_turn`
    // überschreibt das Kassenbuch eines Turns vollständig (auch
    // `loads_used`), ein Aufruf je Model-Runde würde also innerhalb desselben
    // Turns bereits verbuchte `context.load`-Aufrufe verwerfen.
    seed_context_load_ledger(session, &ctx.turn_id);

    loop {
        // 1./2. Context + Instructions.
        let fragments = gather_context(session, ctx).await;
        let instructions = load_instructions(session).await;
        let tools = collect_tools(session)?;

        // Programm- und Decken-bewusste Montage (siehe
        // `ModelRequest::with_context_program`s Moduldoku, Abschnitt „Zwei
        // Wege zur Kontextmontage"): läuft nur, wenn die Sitzung **beide**
        // deklariert; sonst fällt sie intern auf denselben Byte-Budget-Pfad
        // zurück, den diese Datei vor diesem Knoten direkt aufgerufen hat —
        // eine Sitzung ohne `ContextProgram` sieht dadurch keine Änderung.
        let program = session.context_program();
        let ceiling = session
            .spawn_context()
            .and_then(|spawn_context| spawn_context.ceiling.as_ref());

        // 4. Model-Call (provider-neutral).
        let request = ModelRequest::with_context_program(
            instructions,
            fragments,
            session.history().clone(),
            tools,
            session.context_budget(),
            program,
            ceiling,
        )?
        .with_reasoning_effort(session.reasoning_effort())
        .with_model_id(session.active_model().cloned())
        .with_provider_id(session.active_provider().cloned());

        // Emit model.request event: byte-count proxy via system_prompt +
        // instruction fragments length (ModelRequest is not serde::Serialize).
        let request_size_bytes: usize = request.system_prompt.len()
            + request
                .instruction_fragments
                .iter()
                .map(|s| s.len())
                .sum::<usize>();
        tracing::info!(size_bytes = request_size_bytes, "model.request");

        let response = model.respond(request).await?;
        total_usage.add(&response.usage);

        // Emit model.response event: size_bytes from tool_calls JSON +
        // optional message length; tool_call_count for scheduling insight.
        let response_size_bytes: usize = response.message.as_deref().map_or(0, |m| m.len())
            + serde_json::to_vec(&response.tool_calls)
                .map(|v| v.len())
                .unwrap_or(0);
        tracing::info!(
            size_bytes = response_size_bytes,
            tool_call_count = response.tool_calls.len(),
            "model.response",
        );

        if let Some(text) = response.message {
            let phase = if response.tool_calls.is_empty() {
                Some(harw_types::MessagePhase::FinalAnswer)
            } else {
                Some(harw_types::MessagePhase::Commentary)
            };
            let text_for_event = text.clone();
            session.history_mut().push_assistant_text(text, phase);
            persist_last(session, store).await?;
            emit(
                session,
                TurnEvent::ItemAdded {
                    turn_id: handle.turn_id.clone(),
                    item: TurnItem::AssistantMessage(AssistantMessageItem {
                        id: harw_types::ItemId::new(),
                        content: vec![ContentPart::Text {
                            text: text_for_event,
                        }],
                        phase,
                    }),
                },
            );
        }

        // 6. Keine Tool-Calls mehr ⇒ Turn fertig.
        if response.tool_calls.is_empty() {
            break;
        }

        // Die Vorprüfung des Parallel-Pfads kann Guardrail-Entscheidungen
        // bereits eingeholt haben. Sie leben genau eine Modellantwort lang und
        // werden unten verbraucht, damit kein Handler doppelt gefragt wird.
        let mut prepared = PreparedApprovals::default();
        if try_execute_parallel_calls(session, store, ctx, &response.tool_calls, &mut prepared)
            .await?
        {
            continue;
        }

        // 5. Tool-Call-Loop.
        for (position, call) in response.tool_calls.into_iter().enumerate() {
            session.history_mut().push_tool_call(
                call.id.clone(),
                call.name.to_string(),
                call.arguments.clone(),
            );
            persist_last(session, store).await?;
            emit(
                session,
                TurnEvent::ToolCallRequested {
                    turn_id: handle.turn_id.clone(),
                    call_id: call.id.clone(),
                    tool_name: call.name.to_string(),
                    arguments: call.arguments.clone(),
                },
            );

            // a. Guardrail. Eine Entscheidung aus der Parallel-Vorprüfung wird
            // verbraucht statt neu eingeholt: ein Approval-Handler darf zu
            // demselben Call nicht zweimal befragt werden.
            let decision = match prepared.take(position) {
                Some(decision) => decision,
                None => check_approval(session, &call).await,
            };
            match decision {
                ApprovalDecision::Allow => {}
                ApprovalDecision::Deny(reason) => {
                    session.history_mut().push_tool_result(
                        call.id.clone(),
                        ToolCallResult::error(format!("denied: {reason}")),
                        0,
                    );
                    persist_last(session, store).await?;
                    continue;
                }
                ApprovalDecision::AskUser(request) => {
                    // Turn pausiert bis zur Nutzer-Entscheidung.
                    let actor = session
                        .spawn_context()
                        .and_then(|context| context.approval_actor.clone())
                        .ok_or_else(|| CoreError::MissingApprovalActor {
                            session_id: session.id().to_string(),
                        })?;

                    // approval.wait span: records the pause point before the
                    // turn suspends; kind is the Display form of the request ID.
                    let approval_span = tracing::info_span!(
                        "approval.wait",
                        approval_kind = %request,
                    );
                    let _approval_guard = approval_span.enter();

                    if let Some(approvals) = approvals {
                        approvals.issue(&ApprovalRecord {
                            request: request.clone(),
                            session: session.id().clone(),
                            call_id: call.id.clone(),
                            actor: actor.clone(),
                            issued_at: jiff::Timestamp::now(),
                        })?;
                    }
                    session.begin_approval(call.clone(), request.clone(), actor)?;
                    return Ok(TurnOutcome::AwaitingApproval {
                        call_id: call.id,
                        request,
                    });
                }
            }

            // b. Handoff-Detection.
            if let Some(role) = handoff_role(&call.name) {
                let spawner = session.registry().spawner().cloned().ok_or_else(|| {
                    CoreError::HandoffFailed {
                        role: role.clone(),
                        reason: "no AgentSpawner registered".to_owned(),
                    }
                })?;
                let spawn_input = SpawnInput {
                    parent_session_id: session.id().clone(),
                    handoff_call_id: call.id.clone(),
                    instructions: None,
                    context: call.arguments.clone(),
                    // Siehe die Begründung an der Schwester-Konstruktionsstelle
                    // in `resume_after_approval_with_store`: hereditär, nie
                    // neu erfunden.
                    ceiling: session
                        .spawn_context()
                        .and_then(|spawn_context| spawn_context.ceiling.clone()),
                };
                let context = match governed_spawn_context(session) {
                    Ok(context) => context,
                    Err(error) => {
                        let result = missing_tool_execution_context_result(error)?;
                        session.history_mut().push_tool_result(call.id, result, 0);
                        persist_last(session, store).await?;
                        continue;
                    }
                };
                let spawned = spawner
                    .spawn_child(&role, spawn_input, context.sandbox, context.suggestions)
                    .await;
                let child = spawned.map_err(|e| CoreError::HandoffFailed {
                    role: role.clone(),
                    reason: e.to_string(),
                })?;
                // state ⇒ WaitingForChild; Loop pausiert.
                session.begin_handoff(child.clone(), call.id.clone(), role.clone())?;
                emit(
                    session,
                    TurnEvent::ChildSpawned {
                        turn_id: handle.turn_id.clone(),
                        child: child.clone(),
                        role: role.clone(),
                        question: child_question(&call.arguments),
                    },
                );
                return Ok(TurnOutcome::AwaitingChild {
                    child,
                    call_id: call.id,
                    role,
                });
            }

            // c. Normale Tool-Ausführung — instrumented with tool.call span.
            let tool_name = call.name.to_string();
            let tool_span = tracing::info_span!("tool.call", tool_name = %tool_name);
            let result: (ToolCallResult, u64) = async {
                match find_executor(session, &call.name) {
                    Some(executor) => {
                        let (result, duration_ms) = match tool_execution_context(session, ctx) {
                            Ok(execution_context) => {
                                let started = std::time::Instant::now();
                                let outcome =
                                    executor.traced_execute(&execution_context, &call).await;
                                let duration_ms = started.elapsed().as_millis() as u64;
                                let result = match outcome {
                                    Ok(output) => output_to_result(output),
                                    Err(e) => ToolCallResult::error(e.to_string()),
                                };
                                (result, duration_ms)
                            }
                            Err(error) => (missing_tool_execution_context_result(error)?, 0),
                        };
                        tracing::info!(
                            duration_ms = duration_ms,
                            status = if result.is_success() { "ok" } else { "err" },
                            "tool.execute",
                        );
                        Ok::<_, CoreError>((result, duration_ms))
                    }
                    None => {
                        tracing::info!(duration_ms = 0u64, status = "err", "tool.execute",);
                        Ok::<_, CoreError>((
                            ToolCallResult::error(format!("no executor for tool '{}'", call.name)),
                            0u64,
                        ))
                    }
                }
            }
            .instrument(tool_span)
            .await?;
            let call_id_for_event = call.id.clone();
            emit(
                session,
                TurnEvent::ToolCallCompleted {
                    turn_id: handle.turn_id.clone(),
                    call_id: call_id_for_event,
                    result: result.0.clone(),
                    duration_ms: result.1,
                },
            );
            // d. ToolResult in History.
            session
                .history_mut()
                .push_tool_result(call.id, result.0, result.1);
            persist_last(session, store).await?;
        }
        // Zurück zu Schritt 4 (nächster Model-Call).
    }

    // 7. Observer + Turn-Ende.
    emit(
        session,
        TurnEvent::TurnCompleted {
            turn_id: handle.turn_id.clone(),
            usage: Some(total_usage.clone()),
        },
    );
    notify_turn_stop(
        session,
        &TurnStopInput {
            session_id: handle.session_id.clone(),
            turn_id: handle.turn_id.clone(),
            token_usage: total_usage.clone(),
        },
    );
    session.complete_turn(handle, total_usage);
    Ok(TurnOutcome::Completed)
}

/// Execute a complete model response concurrently only when every call is an
/// explicitly parallel-safe ordinary tool and every guardrail allows it. Any
/// ambiguity falls back to the sequential path below; handoffs and approvals
/// are never raced.
///
/// # Beschreibung
/// Die Bedingungen werden in dieser Reihenfolge geprüft, und die Reihenfolge ist
/// Absicht:
/// 1. Mindestens zwei Calls, kein Handoff darunter.
/// 2. Für **jeden** Call ein `parallel_safe` markierter Executor
///    ([`find_parallel_executor`]).
/// 3. Ein aufgelöster Sandbox-Kontext.
/// 4. Erst danach die Approval-Vorprüfung über [`check_approval`].
///
/// Die Guardrail-Prüfung steht bewusst zuletzt: sie ist der einzige Schritt mit
/// Außenwirkung (Nutzer-Interaktion, Audit-Eintrag, vergebene `ItemId`). Würde
/// sie vor einer Bedingung stehen, die den Parallel-Pfad noch verwirft, hätte
/// ein Handler umsonst gefragt.
///
/// Früher genügte ein einziger registrierter `ApprovalHandler`, um die
/// Parallelisierung komplett abzuschalten. Da `harw-registry-defaults`
/// produktiv immer eine Policy registriert, war der `JoinSet`-Pfad damit nie
/// aktiv. Statt des Pauschalausschlusses prüft die Vorprüfung nun Call für
/// Call: nur wenn **alle** `Allow` liefern, wird parallel ausgeführt.
///
/// # Invariante (kein doppelter Handler-Aufruf)
/// Im Parallel-Pfad läuft [`check_approval`] genau einmal je Call und danach
/// nicht mehr — der sequenzielle Loop wird gar nicht erst betreten. Liefert ein
/// Call `Deny` oder `AskUser`, bricht die Vorprüfung ab und legt die bereits
/// eingeholten Entscheidungen in `prepared` ab; der sequenzielle Pfad verbraucht
/// sie dort, statt dieselben Handler ein zweites Mal zu fragen. In beiden
/// Fällen gilt: höchstens ein Handler-Aufruf pro Call und Modellantwort.
///
/// # Arguments
/// - `prepared` (`&mut PreparedApprovals`): Ausgabekanal für die Entscheidungen
///   der Vorprüfung. Wird nur beschrieben, wenn Schritt 4 erreicht wurde.
///
/// # Returns
/// `true`, wenn die Antwort vollständig parallel ausgeführt und persistiert
/// wurde; `false`, wenn der Aufrufer den sequenziellen Pfad nehmen muss.
async fn try_execute_parallel_calls(
    session: &mut AgentSession,
    store: &dyn StateStore,
    ctx: &TurnInputContext,
    calls: &[ToolCall],
    prepared: &mut PreparedApprovals,
) -> CoreResult<bool> {
    if calls.len() < 2 || calls.iter().any(|call| handoff_role(&call.name).is_some()) {
        return Ok(false);
    }
    let mut jobs = Vec::with_capacity(calls.len());
    for call in calls {
        let Some(executor) = find_parallel_executor(session, &call.name) else {
            return Ok(false);
        };
        jobs.push((call.clone(), executor));
    }
    let execution_context = match tool_execution_context(session, ctx) {
        Ok(context) => context,
        Err(error) => {
            missing_tool_execution_context_result(error)?;
            return Ok(false);
        }
    };
    if !preflight_approvals(session, calls, prepared).await {
        return Ok(false);
    }

    for (call, _) in &jobs {
        session.history_mut().push_tool_call(
            call.id.clone(),
            call.name.to_string(),
            call.arguments.clone(),
        );
        persist_last(session, store).await?;
        emit(
            session,
            TurnEvent::ToolCallRequested {
                turn_id: ctx.turn_id.clone(),
                call_id: call.id.clone(),
                tool_name: call.name.to_string(),
                arguments: call.arguments.clone(),
            },
        );
    }

    let mut joins = tokio::task::JoinSet::new();
    for (position, (call, executor)) in jobs.into_iter().enumerate() {
        let execution_context = execution_context.clone();
        let tool_name = call.name.to_string();
        let tool_span = tracing::info_span!("tool.call", tool_name = %tool_name);
        joins.spawn(
            async move {
                let started = std::time::Instant::now();
                let output = executor.traced_execute(&execution_context, &call).await;
                let result = output
                    .map(output_to_result)
                    .unwrap_or_else(|error| ToolCallResult::error(error.to_string()));
                let duration_ms = started.elapsed().as_millis() as u64;
                tracing::info!(
                    duration_ms = duration_ms,
                    status = if result.is_success() { "ok" } else { "err" },
                    "tool.execute",
                );
                (position, call.id, result, duration_ms)
            }
            .instrument(tool_span),
        );
    }
    let mut results: Vec<ParallelCallSlot> = vec![None; calls.len()];
    while let Some(joined) = joins.join_next().await {
        let (position, call_id, result, duration_ms) =
            joined.map_err(|error| CoreError::ToolFailed(error.to_string()))?;
        results[position] = Some((call_id, result, duration_ms));
    }
    for result in results {
        let (call_id, value, duration_ms) = result.ok_or_else(|| {
            CoreError::ToolFailed("parallel tool scheduler lost a completed call".to_owned())
        })?;
        emit(
            session,
            TurnEvent::ToolCallCompleted {
                turn_id: ctx.turn_id.clone(),
                call_id: call_id.clone(),
                result: value.clone(),
                duration_ms,
            },
        );
        session
            .history_mut()
            .push_tool_result(call_id, value, duration_ms);
        persist_last(session, store).await?;
    }
    Ok(true)
}

/// Holt für jeden Call die Guardrail-Entscheidung ein, bis eine davon nicht
/// `Allow` ist.
///
/// # Beschreibung
/// Jede eingeholte Entscheidung — auch die abschlägige — wird in `prepared`
/// hinterlegt, damit der sequenzielle Fallback sie verbrauchen kann statt
/// denselben Handler erneut zu fragen. Ohne registrierten `ApprovalHandler`
/// liefert [`check_approval`] `Allow`, ohne dass irgendetwas aufgerufen wird;
/// die Vorprüfung ist dann ein reiner Durchlauf.
///
/// # Arguments
/// - `calls` (`&[ToolCall]`): die Calls der Modellantwort in Originalreihenfolge.
/// - `prepared` (`&mut PreparedApprovals`): wird überschrieben.
///
/// # Returns
/// `true`, wenn **alle** Calls `Allow` bekommen haben.
async fn preflight_approvals(
    session: &AgentSession,
    calls: &[ToolCall],
    prepared: &mut PreparedApprovals,
) -> bool {
    prepared.decisions = Vec::with_capacity(calls.len());
    for call in calls {
        let decision = check_approval(session, call).await;
        let allowed = matches!(decision, ApprovalDecision::Allow);
        prepared.decisions.push(Some(decision));
        if !allowed {
            return false;
        }
    }
    true
}

/// Persistiert das zuletzt angehängte History-Item über den `StateStore`.
///
/// Emits a `transcript.persist` debug event with `records_written = 1` when a
/// record is actually saved, or `records_written = 0` when the history is empty.
/// Never logs the content of the record (redaction-by-default).
async fn persist_last(session: &AgentSession, store: &dyn StateStore) -> CoreResult<()> {
    if let Some(item) = session.history().last() {
        store
            .save_turn(session.id(), item)
            .await
            .map_err(|error| state_store_error(error, "persistence"))?;
        tracing::debug!(records_written = 1u64, "transcript.persist");
    } else {
        tracing::debug!(records_written = 0u64, "transcript.persist");
    }
    Ok(())
}

fn state_store_error(error: StateStoreError, operation_name: &str) -> CoreError {
    match error {
        StateStoreError::PoisonedMutex { operation } => CoreError::TurnRejected(format!(
            "state store {operation_name} failed during {operation}"
        )),
        StateStoreError::SessionStore(error) => CoreError::SessionStore(error),
        StateStoreError::SequenceExhausted { session } => CoreError::TurnRejected(format!(
            "state store {operation_name} failed: transcript sequence exhausted for session {}",
            session.as_str()
        )),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::activation::{SessionActivation, ToolProfile};
    use crate::session::AgentSession;
    use harw_catalog::AgentSuggestions;
    use harw_extension_api::contributors::ToolProvider;
    use harw_extension_api::{
        AgentSpawnError, AgentSpawner, ExtensionRegistryBuilder, SpawnFuture, SpawnInput,
    };
    use harw_sandbox::SandboxSpec;
    use harw_tools::{
        FunctionToolSpec, JsonSchema, ToolCall, ToolExecutionContext, ToolExecutor,
        ToolExecutorFuture, ToolName, ToolOutput, ToolSpec, ToolsError,
    };
    use harw_types::AgentRole;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc;

    struct LoadFailStore;

    #[test]
    fn sequence_exhaustion_rejects_the_turn() {
        let session = SessionId::new();
        let error = state_store_error(
            StateStoreError::SequenceExhausted {
                session: session.clone(),
            },
            "persistence",
        );

        assert!(matches!(
            error,
            CoreError::TurnRejected(reason)
                if reason.contains("state store persistence failed")
                    && reason.contains(session.as_str())
        ));
    }

    impl StateStore for LoadFailStore {
        fn save_turn<'a>(
            &'a self,
            _sid: &'a SessionId,
            _item: &'a TurnItem,
        ) -> harw_extension_api::ExtFuture<'a, crate::state_store::StateStoreResult<()>> {
            Box::pin(async { Ok(()) })
        }

        fn load_history<'a>(
            &'a self,
            _sid: &'a SessionId,
        ) -> harw_extension_api::ExtFuture<
            'a,
            crate::state_store::StateStoreResult<crate::history::ConversationHistory>,
        > {
            Box::pin(async {
                Err(crate::state_store::StateStoreError::PoisonedMutex {
                    operation: "test_load_history",
                })
            })
        }
    }

    struct RateLimitedModel;

    impl ModelProvider for RateLimitedModel {
        fn respond<'a>(&'a self, _request: ModelRequest) -> crate::model::ModelFuture<'a> {
            Box::pin(async {
                Err(crate::model::ModelError::RateLimited {
                    retry_after_secs: 30,
                    message: "test provider limit".to_owned(),
                })
            })
        }
    }

    struct MissingContextRecoveryModel {
        call_id: ToolCallId,
        responses: AtomicUsize,
        saw_rejected_tool_result: AtomicBool,
    }

    impl ModelProvider for MissingContextRecoveryModel {
        fn respond<'a>(&'a self, request: ModelRequest) -> crate::model::ModelFuture<'a> {
            let response_index = self.responses.fetch_add(1, Ordering::SeqCst);
            let call_id = self.call_id.clone();
            if response_index > 0 {
                self.saw_rejected_tool_result.store(
                    request.history.items().iter().any(|item| {
                        matches!(item, TurnItem::ToolResult(result) if !result.result.is_success())
                    }),
                    Ordering::SeqCst,
                );
            }
            Box::pin(async move {
                if response_index == 0 {
                    Ok(crate::model::ModelResponse {
                        message: None,
                        tool_calls: vec![ToolCall {
                            id: call_id,
                            name: ToolName::new("fs.read"),
                            arguments: serde_json::json!({"path": "untrusted"}),
                        }],
                        usage: Default::default(),
                    })
                } else {
                    Ok(crate::model::ModelResponse::text(
                        "I can continue without that tool.",
                    ))
                }
            })
        }
    }

    struct CompletionTrackingSpawner {
        child_completed: AtomicUsize,
        child_finished: AtomicUsize,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl AgentSpawner for CompletionTrackingSpawner {
        fn spawn_child<'a>(
            &'a self,
            _role: &'a str,
            _input: SpawnInput,
            _sandbox: SandboxSpec,
            _suggestions: Option<AgentSuggestions>,
        ) -> SpawnFuture<'a> {
            Box::pin(async { Ok(SessionId::new()) })
        }

        fn child_finished(&self, _child: &SessionId) {
            self.child_finished.fetch_add(1, Ordering::SeqCst);
        }

        fn child_completed(
            &self,
            _child: &SessionId,
            _completed_at: jiff::Timestamp,
        ) -> Result<(), AgentSpawnError> {
            let mut events = self.events.lock().expect("test event lock is not poisoned");
            assert_eq!(events.as_slice(), ["persisted_child_result"]);
            events.push("durably_completed_child");
            self.child_completed.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct ChildResultStore {
        fail_child_result: bool,
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl StateStore for ChildResultStore {
        fn save_turn<'a>(
            &'a self,
            _sid: &'a SessionId,
            item: &'a TurnItem,
        ) -> harw_extension_api::ExtFuture<'a, crate::state_store::StateStoreResult<()>> {
            let is_child_result = matches!(item, TurnItem::ToolResult(_));
            let fail_child_result = self.fail_child_result;
            let events = self.events.clone();
            Box::pin(async move {
                if is_child_result && fail_child_result {
                    return Err(crate::state_store::StateStoreError::PoisonedMutex {
                        operation: "child_result_persistence",
                    });
                }
                if is_child_result {
                    events
                        .lock()
                        .expect("test event lock is not poisoned")
                        .push("persisted_child_result");
                }
                Ok(())
            })
        }

        fn load_history<'a>(
            &'a self,
            _sid: &'a SessionId,
        ) -> harw_extension_api::ExtFuture<
            'a,
            crate::state_store::StateStoreResult<crate::history::ConversationHistory>,
        > {
            Box::pin(async { Ok(crate::history::ConversationHistory::new()) })
        }
    }

    fn paused_child_session(
        spawner: Arc<CompletionTrackingSpawner>,
    ) -> (AgentSession, SessionId, ToolCallId) {
        let (tx, _rx) = mpsc::unbounded_channel();
        let registry = ExtensionRegistryBuilder::default().spawner(spawner).build();
        let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx);
        session.try_start_turn().expect("test turn starts");
        let child = SessionId::new();
        let call_id = ToolCallId::new();
        session
            .begin_handoff(child.clone(), call_id.clone(), "worker".to_owned())
            .expect("test handoff pauses");
        (session, child, call_id)
    }

    #[tokio::test]
    async fn durable_child_completion_follows_persisted_child_result() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let spawner = Arc::new(CompletionTrackingSpawner {
            child_completed: AtomicUsize::new(0),
            child_finished: AtomicUsize::new(0),
            events: events.clone(),
        });
        let (mut session, child, call_id) = paused_child_session(spawner.clone());
        let store = ChildResultStore {
            fail_child_result: false,
            events: events.clone(),
        };
        let temp = tempfile::tempdir().expect("temporary approval directory");
        let approvals = ApprovalStore::new(temp.path());

        let outcome = resume_after_child_durable(
            &mut session,
            &crate::model::EchoModelProvider::new("parent resumed"),
            &store,
            &approvals,
            child,
            call_id,
            ToolCallResult::success(serde_json::json!({"child": "done"})),
        )
        .await
        .expect("durable child result resumes the parent");

        assert!(matches!(outcome, TurnOutcome::Completed));
        assert_eq!(spawner.child_completed.load(Ordering::SeqCst), 1);
        assert_eq!(spawner.child_finished.load(Ordering::SeqCst), 0);
        assert_eq!(
            events
                .lock()
                .expect("test event lock is not poisoned")
                .as_slice(),
            ["persisted_child_result", "durably_completed_child"]
        );
    }

    #[tokio::test]
    async fn failed_child_result_persistence_does_not_release_durable_child() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let spawner = Arc::new(CompletionTrackingSpawner {
            child_completed: AtomicUsize::new(0),
            child_finished: AtomicUsize::new(0),
            events: events.clone(),
        });
        let (mut session, child, call_id) = paused_child_session(spawner.clone());
        let store = ChildResultStore {
            fail_child_result: true,
            events: events.clone(),
        };
        let temp = tempfile::tempdir().expect("temporary approval directory");
        let approvals = ApprovalStore::new(temp.path());

        let error = resume_after_child_durable(
            &mut session,
            &crate::model::EchoModelProvider::new("must not run"),
            &store,
            &approvals,
            child,
            call_id,
            ToolCallResult::success(serde_json::json!({"child": "done"})),
        )
        .await
        .expect_err("failed child-result persistence rejects the resume");

        assert!(matches!(error, CoreError::TurnRejected(_)));
        assert_eq!(spawner.child_completed.load(Ordering::SeqCst), 0);
        assert_eq!(spawner.child_finished.load(Ordering::SeqCst), 0);
        assert!(
            events
                .lock()
                .expect("test event lock is not poisoned")
                .is_empty(),
            "a failed persistence attempt must not release the child"
        );
    }

    // ------------------------------------------------------------------
    // Minimal stub ToolProvider
    // ------------------------------------------------------------------

    struct StubToolProvider {
        specs: Vec<ToolSpec>,
    }

    impl StubToolProvider {
        fn with_names(names: &[&str]) -> Arc<Self> {
            let specs = names
                .iter()
                .map(|n| {
                    ToolSpec::Function(FunctionToolSpec {
                        name: ToolName::new(*n),
                        description: format!("stub tool {n}"),
                        parameters: JsonSchema::default(),
                        strict: false,
                    })
                })
                .collect();
            Arc::new(Self { specs })
        }
    }

    impl ToolProvider for StubToolProvider {
        fn tools(&self) -> Vec<ToolSpec> {
            self.specs.clone()
        }
        fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
            if self.specs.iter().any(|s| s.name() == name.as_str()) {
                Some(Arc::new(StubExecutor))
            } else {
                None
            }
        }
    }

    struct StubExecutor;

    impl ToolExecutor for StubExecutor {
        fn execute<'a>(
            &'a self,
            _ctx: &'a ToolExecutionContext,
            _call: &'a ToolCall,
        ) -> ToolExecutorFuture<'a> {
            Box::pin(async {
                Ok(ToolOutput::Text {
                    content: "ok".to_owned(),
                })
            })
        }
    }

    fn make_session(
        provider: Arc<dyn ToolProvider>,
        activation: SessionActivation,
    ) -> AgentSession {
        let (tx, _rx) = mpsc::unbounded_channel();
        let registry = ExtensionRegistryBuilder::default()
            .tool_provider(provider)
            .build();
        AgentSession::new(AgentRole::Assistant, None, registry, tx).with_activation(activation)
    }

    // ------------------------------------------------------------------
    // collect_tools filter tests
    // ------------------------------------------------------------------

    #[test]
    fn test_collect_tools_full_profile_includes_all() {
        let provider = StubToolProvider::with_names(&["fs.read", "custom.tool"]);
        let session = make_session(provider, SessionActivation::new(ToolProfile::Full));
        let tools = collect_tools(&session).expect("collect_tools should not fail");
        let names: Vec<_> = tools.iter().map(|t| t.name().to_owned()).collect();
        assert!(names.contains(&"fs.read".to_owned()));
        assert!(names.contains(&"custom.tool".to_owned()));
    }

    #[test]
    fn test_collect_tools_coding_profile_excludes_unlisted() {
        let provider = StubToolProvider::with_names(&["fs.read", "custom.tool"]);
        let session = make_session(provider, SessionActivation::new(ToolProfile::Coding));
        let tools = collect_tools(&session).expect("collect_tools should not fail");
        let names: Vec<_> = tools.iter().map(|t| t.name().to_owned()).collect();
        assert!(
            names.contains(&"fs.read".to_owned()),
            "fs.read must be visible"
        );
        assert!(
            !names.contains(&"custom.tool".to_owned()),
            "custom.tool must be hidden by Coding profile"
        );
    }

    #[test]
    fn test_collect_tools_disable_override_hides_allowlisted_tool() {
        let provider = StubToolProvider::with_names(&["fs.read", "fs.write"]);
        let mut activation = SessionActivation::new(ToolProfile::Full);
        activation.disable_tool(ToolName::new("fs.write"));
        let session = make_session(provider, activation);
        let tools = collect_tools(&session).expect("collect_tools should not fail");
        let names: Vec<_> = tools.iter().map(|t| t.name().to_owned()).collect();
        assert!(names.contains(&"fs.read".to_owned()));
        assert!(
            !names.contains(&"fs.write".to_owned()),
            "fs.write must be hidden by override"
        );
    }

    // ------------------------------------------------------------------
    // find_executor filter tests
    // ------------------------------------------------------------------

    #[test]
    fn test_find_executor_returns_none_when_tool_disabled() {
        let provider = StubToolProvider::with_names(&["shell.exec"]);
        let mut activation = SessionActivation::new(ToolProfile::Full);
        activation.disable_tool(ToolName::new("shell.exec"));
        let session = make_session(provider, activation);
        let result = find_executor(&session, &ToolName::new("shell.exec"));
        assert!(result.is_none(), "disabled tool must have no executor");
    }

    #[test]
    fn test_find_executor_returns_some_when_tool_enabled() {
        let provider = StubToolProvider::with_names(&["fs.read"]);
        let session = make_session(provider, SessionActivation::new(ToolProfile::Full));
        let result = find_executor(&session, &ToolName::new("fs.read"));
        assert!(result.is_some(), "enabled tool must return an executor");
    }

    #[tokio::test]
    async fn rate_limited_model_failure_leaves_the_session_retryable() {
        let provider = StubToolProvider::with_names(&[]);
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
        let store = crate::state_store::InMemoryStateStore::new();

        let error = run_turn(
            &mut session,
            &RateLimitedModel,
            &store,
            TurnInput::user("retry me"),
        )
        .await
        .expect_err("rate limiting reaches the caller");

        assert!(matches!(
            error,
            CoreError::Model(crate::model::ModelError::RateLimited { .. })
        ));
        assert!(
            matches!(session.state(), crate::session::SessionState::Idle),
            "a retryable provider failure must not terminalize the session"
        );

        let outcome = run_turn(
            &mut session,
            &crate::model::EchoModelProvider::new("recovered"),
            &store,
            TurnInput::user("try again"),
        )
        .await
        .expect("the same session accepts a later retry");
        assert!(matches!(outcome, TurnOutcome::Completed));
    }

    #[tokio::test]
    async fn missing_tool_execution_context_is_model_visible_and_recoverable() {
        let provider = StubToolProvider::with_names(&["fs.read"]);
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
        let store = crate::state_store::InMemoryStateStore::new();
        let model = MissingContextRecoveryModel {
            call_id: ToolCallId::new(),
            responses: AtomicUsize::new(0),
            saw_rejected_tool_result: AtomicBool::new(false),
        };

        let outcome = run_turn(&mut session, &model, &store, TurnInput::user("read this"))
            .await
            .expect("the model can recover from a rejected tool boundary");

        assert!(matches!(outcome, TurnOutcome::Completed));
        assert_eq!(model.responses.load(Ordering::SeqCst), 2);
        assert!(
            model.saw_rejected_tool_result.load(Ordering::SeqCst),
            "the follow-up model request must include the rejected tool result"
        );
        assert!(matches!(
            session.state(),
            crate::session::SessionState::Idle
        ));
        assert_eq!(session.history().len(), 4);
    }

    #[tokio::test]
    async fn empty_live_history_is_hydrated_before_a_new_user_turn() {
        let provider = StubToolProvider::with_names(&[]);
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));
        let store = crate::state_store::InMemoryStateStore::new();
        let mut persisted = crate::history::ConversationHistory::new();
        persisted.push_user_text("previous question");
        persisted.push_assistant_text("previous answer", None);
        store
            .save_history(session.id(), &persisted)
            .await
            .expect("seed history");

        run_turn(
            &mut session,
            &crate::model::EchoModelProvider::new("new answer"),
            &store,
            TurnInput::user("new question"),
        )
        .await
        .expect("hydrated turn runs");

        assert_eq!(session.history().len(), 4);
        assert_eq!(store.turn_count(session.id()), 4);
    }

    #[tokio::test]
    async fn history_load_error_fails_closed_before_user_item_is_admitted() {
        let provider = StubToolProvider::with_names(&[]);
        let mut session = make_session(provider, SessionActivation::new(ToolProfile::Full));

        let error = run_turn(
            &mut session,
            &crate::model::EchoModelProvider::new("must not run"),
            &LoadFailStore,
            TurnInput::user("must not be persisted"),
        )
        .await
        .expect_err("history load failure must reject the turn");

        assert!(matches!(error, CoreError::TurnRejected(_)));
        assert!(session.history().is_empty());
        assert!(matches!(
            session.state(),
            crate::session::SessionState::Failed(_)
        ));
    }

    #[tokio::test]
    async fn duplicate_tool_catalog_failure_remains_terminal() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let registry = ExtensionRegistryBuilder::default()
            .tool_provider(StubToolProvider::with_names(&["duplicate"]))
            .tool_provider(StubToolProvider::with_names(&["duplicate"]))
            .build();
        let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx);
        let store = crate::state_store::InMemoryStateStore::new();

        let error = run_turn(
            &mut session,
            &crate::model::EchoModelProvider::new("must not run"),
            &store,
            TurnInput::user("go"),
        )
        .await
        .expect_err("duplicate tools are an invariant violation");

        assert!(matches!(error, CoreError::DuplicateTool { .. }));
        assert!(matches!(
            session.state(),
            crate::session::SessionState::Failed(_)
        ));
    }

    // ------------------------------------------------------------------
    // W2-14: Parallel-Dispatch mit Approval-Vorprüfung
    // ------------------------------------------------------------------

    /// Liefert eine vorprogrammierte Folge von Antworten, eine pro Model-Call.
    struct ScriptedModel {
        responses: Mutex<std::collections::VecDeque<crate::model::ModelResponse>>,
    }

    impl ScriptedModel {
        fn new(responses: Vec<crate::model::ModelResponse>) -> Self {
            Self {
                responses: Mutex::new(responses.into_iter().collect()),
            }
        }
    }

    impl ModelProvider for ScriptedModel {
        fn respond<'a>(&'a self, _request: ModelRequest) -> crate::model::ModelFuture<'a> {
            let next = self
                .responses
                .lock()
                .expect("test model lock is not poisoned")
                .pop_front();
            Box::pin(async move { next.ok_or(crate::model::ModelError::EmptyResponse) })
        }
    }

    /// Approval-Handler, der jede Befragung protokolliert und je Call-Id eine
    /// vorprogrammierte Entscheidung liefert. Nicht gelistete Calls → `Allow`.
    struct CountingApproval {
        scripted: Vec<(ToolCallId, ApprovalDecision)>,
        reviews: Mutex<Vec<ToolCallId>>,
    }

    impl CountingApproval {
        fn new(scripted: Vec<(ToolCallId, ApprovalDecision)>) -> Self {
            Self {
                scripted,
                reviews: Mutex::new(Vec::new()),
            }
        }

        fn allow_everything() -> Self {
            Self::new(Vec::new())
        }

        fn reviews_for(&self, call_id: &ToolCallId) -> usize {
            self.reviews
                .lock()
                .expect("test review lock is not poisoned")
                .iter()
                .filter(|reviewed| *reviewed == call_id)
                .count()
        }

        fn total_reviews(&self) -> usize {
            self.reviews
                .lock()
                .expect("test review lock is not poisoned")
                .len()
        }
    }

    impl harw_extension_api::ApprovalHandler for CountingApproval {
        fn review<'a>(
            &'a self,
            call: &'a ToolCall,
        ) -> harw_extension_api::ExtFuture<'a, ApprovalDecision> {
            self.reviews
                .lock()
                .expect("test review lock is not poisoned")
                .push(call.id.clone());
            let decision = self
                .scripted
                .iter()
                .find(|(id, _)| id == &call.id)
                .map_or(ApprovalDecision::Allow, |(_, decision)| decision.clone());
            Box::pin(async move { decision })
        }
    }

    /// Werkzeug, das seine Aufrufe zählt und — mit Barriere — erst zurückkehrt,
    /// wenn die geforderte Zahl an Aufrufen gleichzeitig in Flight ist. Damit
    /// unterscheidet ein Test echte Nebenläufigkeit von serieller Ausführung.
    struct BarrierExecutor {
        executions: Arc<AtomicUsize>,
        barrier: Option<Arc<tokio::sync::Barrier>>,
    }

    impl ToolExecutor for BarrierExecutor {
        fn execute<'a>(
            &'a self,
            _ctx: &'a ToolExecutionContext,
            _call: &'a ToolCall,
        ) -> ToolExecutorFuture<'a> {
            self.executions.fetch_add(1, Ordering::SeqCst);
            let barrier = self.barrier.clone();
            Box::pin(async move {
                if let Some(barrier) = barrier {
                    barrier.wait().await;
                }
                Ok(ToolOutput::Text {
                    content: "ok".to_owned(),
                })
            })
        }
    }

    struct StubParallelProvider {
        names: Vec<ToolName>,
        /// Teilmenge von `names`, die als `parallel_safe` gemeldet wird.
        parallel: Vec<ToolName>,
        executions: Arc<AtomicUsize>,
        barrier: Option<Arc<tokio::sync::Barrier>>,
    }

    impl StubParallelProvider {
        fn instant(names: &[&str], executions: Arc<AtomicUsize>) -> Arc<Self> {
            let names: Vec<ToolName> = names.iter().map(|name| ToolName::new(*name)).collect();
            Arc::new(Self {
                parallel: names.clone(),
                names,
                executions,
                barrier: None,
            })
        }

        fn joined(names: &[&str], executions: Arc<AtomicUsize>, parties: usize) -> Arc<Self> {
            let names: Vec<ToolName> = names.iter().map(|name| ToolName::new(*name)).collect();
            Arc::new(Self {
                parallel: names.clone(),
                names,
                executions,
                barrier: Some(Arc::new(tokio::sync::Barrier::new(parties))),
            })
        }

        fn with_serial_tool(
            names: &[&str],
            serial: &str,
            executions: Arc<AtomicUsize>,
        ) -> Arc<Self> {
            let names: Vec<ToolName> = names.iter().map(|name| ToolName::new(*name)).collect();
            let parallel = names
                .iter()
                .filter(|name| name.as_str() != serial)
                .cloned()
                .collect();
            Arc::new(Self {
                names,
                parallel,
                executions,
                barrier: None,
            })
        }
    }

    impl ToolProvider for StubParallelProvider {
        fn tools(&self) -> Vec<ToolSpec> {
            self.names
                .iter()
                .map(|name| {
                    ToolSpec::Function(FunctionToolSpec {
                        name: name.clone(),
                        description: "stub tool".to_owned(),
                        parameters: JsonSchema::default(),
                        strict: false,
                    })
                })
                .collect()
        }

        fn executor(&self, name: &ToolName) -> Option<Arc<dyn ToolExecutor>> {
            self.names.contains(name).then(|| {
                Arc::new(BarrierExecutor {
                    executions: Arc::clone(&self.executions),
                    barrier: self.barrier.clone(),
                }) as Arc<dyn ToolExecutor>
            })
        }

        fn parallel_safe(&self, name: &ToolName) -> bool {
            self.parallel.contains(name)
        }
    }

    /// Sandbox auf dem echten Harness-Verzeichnis. Ohne aufgelösten
    /// Spawn-Kontext lehnt der Turn-Loop jede Tool-Ausführung ab, der
    /// Parallel-Pfad wäre also gar nicht erreichbar.
    fn test_sandbox() -> SandboxSpec {
        let harness_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("harw-core hat ein Workspace-Elternverzeichnis")
            .to_path_buf();
        let registry = harw_sandbox::WorkspaceRegistry::build(
            &harness_root,
            [harw_sandbox::WorkspaceRegistration {
                tenant: harw_types::TenantId::from_str("test-tenant"),
                workspace: harw_types::WorkspaceId::from_str("core-parallel-tests"),
                root: std::path::PathBuf::from("harw-core"),
            }],
        )
        .expect("Test-Workspace ist registrierbar");
        SandboxSpec::from_resolved(
            registry
                .resolve(
                    &harw_types::TenantId::from_str("test-tenant"),
                    &harw_types::WorkspaceId::from_str("core-parallel-tests"),
                )
                .expect("Test-Workspace löst auf"),
            harw_sandbox::PermissionSet::from_policy([
                harw_sandbox::Permission::ReadWorkspace,
                harw_sandbox::Permission::WriteWorkspace,
                harw_sandbox::Permission::ExecuteProcess,
            ]),
        )
    }

    fn test_spawn_context() -> SpawnContext {
        SpawnContext {
            sandbox: test_sandbox(),
            suggestions: None,
            capability_snapshot: None,
            approval_actor: Some(ApprovalActor::Operator {
                id: "test-operator".to_owned(),
            }),
            organizational_role: harw_agent_dsl::roles::AgentRoleId::RootOrchestrator,
            trace: None,
            // `None` reads fail-closed (most restrictive ceiling), not
            // "unbounded" — see `SpawnContext::ceiling`'s own doc. Existing
            // tests that build a session via this helper never exercised a
            // ceiling before this field existed, so `None` keeps their
            // behavior unchanged. Tests that need a real ceiling build their
            // own `SpawnContext` literal instead of this shared helper.
            ceiling: None,
        }
    }

    fn guarded_session(
        provider: Arc<dyn ToolProvider>,
        handler: Arc<CountingApproval>,
    ) -> AgentSession {
        let (tx, _rx) = mpsc::unbounded_channel();
        let registry = ExtensionRegistryBuilder::default()
            .tool_provider(provider)
            .approval_handler(handler)
            .build();
        AgentSession::new(AgentRole::Assistant, None, registry, tx)
            .with_spawn_context(test_spawn_context())
    }

    fn call(id: &ToolCallId, name: &str) -> ToolCall {
        ToolCall {
            id: id.clone(),
            name: ToolName::new(name),
            arguments: serde_json::json!({"part": name}),
        }
    }

    fn response_with(calls: Vec<ToolCall>) -> crate::model::ModelResponse {
        crate::model::ModelResponse {
            message: None,
            tool_calls: calls,
            usage: Default::default(),
        }
    }

    fn drain_turn_events(rx: &mut mpsc::UnboundedReceiver<TurnEvent>) -> Vec<TurnEvent> {
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events
    }

    #[tokio::test]
    async fn a_registered_approval_handler_no_longer_disables_parallel_dispatch() {
        // Regression W2-14: früher genügte ein registrierter Handler, um den
        // JoinSet-Pfad abzuschalten. Die Barriere über zwei Parteien beweist,
        // dass beide Calls tatsächlich gleichzeitig laufen — seriell würde der
        // erste Aufruf nie zurückkehren und der Timeout zuschlagen.
        let first = ToolCallId::new();
        let second = ToolCallId::new();
        let executions = Arc::new(AtomicUsize::new(0));
        let handler = Arc::new(CountingApproval::allow_everything());
        let provider = StubParallelProvider::joined(&["lookup"], Arc::clone(&executions), 2);
        let mut session = guarded_session(provider, Arc::clone(&handler));
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![
            response_with(vec![call(&first, "lookup"), call(&second, "lookup")]),
            crate::model::ModelResponse::text("beide Ergebnisse liegen vor"),
        ]);

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_turn(&mut session, &model, &store, TurnInput::user("parallel")),
        )
        .await
        .expect("die Calls müssen gleichzeitig laufen, sonst blockiert die Barriere")
        .expect("der Turn läuft durch");

        assert!(matches!(outcome, TurnOutcome::Completed));
        assert_eq!(executions.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn parallel_preflight_asks_every_handler_exactly_once() {
        let first = ToolCallId::new();
        let second = ToolCallId::new();
        let executions = Arc::new(AtomicUsize::new(0));
        let handler = Arc::new(CountingApproval::allow_everything());
        let provider = StubParallelProvider::instant(&["lookup"], Arc::clone(&executions));
        let mut session = guarded_session(provider, Arc::clone(&handler));
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![
            response_with(vec![call(&first, "lookup"), call(&second, "lookup")]),
            crate::model::ModelResponse::text("fertig"),
        ]);

        let outcome = run_turn(&mut session, &model, &store, TurnInput::user("parallel"))
            .await
            .expect("der Turn läuft durch");

        assert!(matches!(outcome, TurnOutcome::Completed));
        assert_eq!(executions.load(Ordering::SeqCst), 2);
        assert_eq!(handler.reviews_for(&first), 1);
        assert_eq!(handler.reviews_for(&second), 1);
        assert_eq!(
            handler.total_reviews(),
            2,
            "im Parallel-Pfad darf kein Handler ein zweites Mal gefragt werden"
        );
    }

    #[tokio::test]
    async fn ask_user_falls_back_to_the_sequential_path_and_pauses_the_turn() {
        let allowed = ToolCallId::new();
        let asked = ToolCallId::new();
        let request = harw_types::ItemId::new();
        let executions = Arc::new(AtomicUsize::new(0));
        let handler = Arc::new(CountingApproval::new(vec![(
            asked.clone(),
            ApprovalDecision::AskUser(request.clone()),
        )]));
        let provider = StubParallelProvider::instant(&["lookup"], Arc::clone(&executions));
        let mut session = guarded_session(provider, Arc::clone(&handler));
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![response_with(vec![
            call(&allowed, "lookup"),
            call(&asked, "lookup"),
        ])]);

        let outcome = run_turn(&mut session, &model, &store, TurnInput::user("ask me"))
            .await
            .expect("der Turn pausiert statt zu scheitern");

        match &outcome {
            TurnOutcome::AwaitingApproval {
                call_id,
                request: paused_request,
            } => {
                assert_eq!(call_id, &asked);
                assert_eq!(paused_request, &request);
            }
            other => panic!("erwartet wurde eine Approval-Pause, nicht {other:?}"),
        }
        assert!(matches!(
            session.state(),
            crate::session::SessionState::WaitingForApproval
        ));
        assert_eq!(
            executions.load(Ordering::SeqCst),
            1,
            "nur der erlaubte Call darf ausgeführt worden sein"
        );
        assert_eq!(handler.reviews_for(&allowed), 1);
        assert_eq!(handler.reviews_for(&asked), 1);
        assert_eq!(
            handler.total_reviews(),
            2,
            "die Vorprüfung reicht ihre Entscheidungen an den sequenziellen \
             Pfad weiter, statt erneut zu fragen"
        );
    }

    #[tokio::test]
    async fn denied_call_falls_back_to_the_sequential_path_without_asking_twice() {
        let denied = ToolCallId::new();
        let allowed = ToolCallId::new();
        let executions = Arc::new(AtomicUsize::new(0));
        let handler = Arc::new(CountingApproval::new(vec![(
            denied.clone(),
            ApprovalDecision::Deny("policy".to_owned()),
        )]));
        let provider = StubParallelProvider::instant(&["lookup"], Arc::clone(&executions));
        let mut session = guarded_session(provider, Arc::clone(&handler));
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![
            response_with(vec![call(&denied, "lookup"), call(&allowed, "lookup")]),
            crate::model::ModelResponse::text("weiter ohne das verbotene Werkzeug"),
        ]);

        let outcome = run_turn(&mut session, &model, &store, TurnInput::user("deny one"))
            .await
            .expect("ein Deny beendet den Turn nicht");

        assert!(matches!(outcome, TurnOutcome::Completed));
        assert_eq!(
            executions.load(Ordering::SeqCst),
            1,
            "der verbotene Call darf nicht ausgeführt werden"
        );
        assert_eq!(handler.reviews_for(&denied), 1);
        assert_eq!(handler.reviews_for(&allowed), 1);
        assert_eq!(handler.total_reviews(), 2);
    }

    #[tokio::test]
    async fn a_non_parallel_safe_call_skips_the_preflight_entirely() {
        // Die Vorprüfung steht bewusst hinter der Executor-Auflösung: ein
        // Handler darf nicht für einen Parallel-Pfad gefragt werden, der ohnehin
        // verworfen wird.
        let parallel = ToolCallId::new();
        let serial = ToolCallId::new();
        let executions = Arc::new(AtomicUsize::new(0));
        let handler = Arc::new(CountingApproval::allow_everything());
        let provider = StubParallelProvider::with_serial_tool(
            &["lookup", "mutate"],
            "mutate",
            Arc::clone(&executions),
        );
        let mut session = guarded_session(provider, Arc::clone(&handler));
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![
            response_with(vec![call(&parallel, "lookup"), call(&serial, "mutate")]),
            crate::model::ModelResponse::text("seriell erledigt"),
        ]);

        let outcome = run_turn(&mut session, &model, &store, TurnInput::user("mixed"))
            .await
            .expect("der gemischte Fall läuft seriell durch");

        assert!(matches!(outcome, TurnOutcome::Completed));
        assert_eq!(executions.load(Ordering::SeqCst), 2);
        assert_eq!(handler.reviews_for(&parallel), 1);
        assert_eq!(handler.reviews_for(&serial), 1);
        assert_eq!(handler.total_reviews(), 2);
    }

    #[tokio::test]
    async fn a_single_call_never_enters_the_parallel_path() {
        let only = ToolCallId::new();
        let executions = Arc::new(AtomicUsize::new(0));
        let handler = Arc::new(CountingApproval::allow_everything());
        let provider = StubParallelProvider::instant(&["lookup"], Arc::clone(&executions));
        let mut session = guarded_session(provider, Arc::clone(&handler));
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![
            response_with(vec![call(&only, "lookup")]),
            crate::model::ModelResponse::text("einzeln erledigt"),
        ]);

        let outcome = run_turn(&mut session, &model, &store, TurnInput::user("single"))
            .await
            .expect("ein einzelner Call läuft seriell");

        assert!(matches!(outcome, TurnOutcome::Completed));
        assert_eq!(handler.total_reviews(), 1);
    }

    #[test]
    fn prepared_approvals_yield_each_decision_at_most_once() {
        let mut prepared = PreparedApprovals {
            decisions: vec![Some(ApprovalDecision::Allow), None],
        };

        assert!(matches!(prepared.take(0), Some(ApprovalDecision::Allow)));
        assert!(
            prepared.take(0).is_none(),
            "eine verbrauchte Entscheidung darf nicht erneut geliefert werden"
        );
        assert!(prepared.take(1).is_none());
        assert!(prepared.take(99).is_none(), "ein Index außerhalb ist None");
    }

    // ------------------------------------------------------------------
    // W2-14: Child-Events
    // ------------------------------------------------------------------

    #[test]
    fn child_question_accepts_only_a_string_field() {
        assert_eq!(
            child_question(&serde_json::json!({"question": "welche Datei?"})),
            Some("welche Datei?".to_owned())
        );
        assert_eq!(child_question(&serde_json::json!({"question": 7})), None);
        assert_eq!(
            child_question(&serde_json::json!({"question": {"text": "x"}})),
            None
        );
        assert_eq!(child_question(&serde_json::json!({"task": "x"})), None);
        assert_eq!(child_question(&serde_json::Value::Null), None);
    }

    struct FixedChildSpawner {
        child: SessionId,
    }

    impl AgentSpawner for FixedChildSpawner {
        fn spawn_child<'a>(
            &'a self,
            _role: &'a str,
            _input: SpawnInput,
            _sandbox: SandboxSpec,
            _suggestions: Option<AgentSuggestions>,
        ) -> SpawnFuture<'a> {
            let child = self.child.clone();
            Box::pin(async move { Ok(child) })
        }
    }

    #[tokio::test]
    async fn handoff_emits_child_spawned_with_role_and_question() {
        let child = SessionId::new();
        let (tx, _rx) = mpsc::unbounded_channel();
        let (turn_tx, mut turn_rx) = mpsc::unbounded_channel();
        let registry = ExtensionRegistryBuilder::default()
            .spawner(Arc::new(FixedChildSpawner {
                child: child.clone(),
            }))
            .build();
        let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx)
            .with_spawn_context(test_spawn_context())
            .with_turn_event_sink(turn_tx);
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![response_with(vec![ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("transfer_to_worker"),
            arguments: serde_json::json!({"question": "wo liegt der Fehler?"}),
        }])]);

        let outcome = run_turn(&mut session, &model, &store, TurnInput::user("handoff"))
            .await
            .expect("der Handoff pausiert den Turn");

        assert!(matches!(outcome, TurnOutcome::AwaitingChild { .. }));
        let spawned = drain_turn_events(&mut turn_rx)
            .into_iter()
            .find(|event| matches!(event, TurnEvent::ChildSpawned { .. }))
            .expect("ein Handoff meldet ein ChildSpawned");
        match spawned {
            TurnEvent::ChildSpawned {
                child: spawned_child,
                role,
                question,
                ..
            } => {
                assert_eq!(spawned_child, child);
                assert_eq!(role, "worker");
                assert_eq!(question.as_deref(), Some("wo liegt der Fehler?"));
            }
            other => panic!("erwartet wurde ChildSpawned, nicht {other:?}"),
        }
    }

    #[tokio::test]
    async fn handoff_without_a_question_argument_reports_none() {
        let child = SessionId::new();
        let (tx, _rx) = mpsc::unbounded_channel();
        let (turn_tx, mut turn_rx) = mpsc::unbounded_channel();
        let registry = ExtensionRegistryBuilder::default()
            .spawner(Arc::new(FixedChildSpawner {
                child: child.clone(),
            }))
            .build();
        let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx)
            .with_spawn_context(test_spawn_context())
            .with_turn_event_sink(turn_tx);
        let store = crate::state_store::InMemoryStateStore::new();
        let model = ScriptedModel::new(vec![response_with(vec![ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new("transfer_to_reviewer"),
            arguments: serde_json::json!({"task": "review"}),
        }])]);

        run_turn(&mut session, &model, &store, TurnInput::user("handoff"))
            .await
            .expect("der Handoff pausiert den Turn");

        let spawned = drain_turn_events(&mut turn_rx)
            .into_iter()
            .find(|event| matches!(event, TurnEvent::ChildSpawned { .. }))
            .expect("ein Handoff meldet ein ChildSpawned");
        assert!(matches!(
            spawned,
            TurnEvent::ChildSpawned { question: None, role, .. } if role == "reviewer"
        ));
    }

    #[tokio::test]
    async fn child_completion_emits_child_completed_after_the_result_is_persisted() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let (turn_tx, mut turn_rx) = mpsc::unbounded_channel();
        let child = SessionId::new();
        let registry = ExtensionRegistryBuilder::default()
            .spawner(Arc::new(FixedChildSpawner {
                child: child.clone(),
            }))
            .build();
        let mut session = AgentSession::new(AgentRole::Assistant, None, registry, tx)
            .with_spawn_context(test_spawn_context())
            .with_turn_event_sink(turn_tx);
        session.try_start_turn().expect("Turn startet");
        let call_id = ToolCallId::new();
        session
            .begin_handoff(child.clone(), call_id.clone(), "worker".to_owned())
            .expect("Handoff pausiert");
        let store = crate::state_store::InMemoryStateStore::new();

        let outcome = resume_after_child(
            &mut session,
            &crate::model::EchoModelProvider::new("Eltern-Turn läuft weiter"),
            &store,
            child.clone(),
            call_id,
            ToolCallResult::success(serde_json::json!({"child": "done"})),
        )
        .await
        .expect("das Kind-Ergebnis nimmt den Eltern-Turn wieder auf");

        assert!(matches!(outcome, TurnOutcome::Completed));
        let completed = drain_turn_events(&mut turn_rx)
            .into_iter()
            .find(|event| matches!(event, TurnEvent::ChildCompleted { .. }))
            .expect("ein terminiertes Kind meldet ChildCompleted");
        match completed {
            TurnEvent::ChildCompleted {
                child: completed_child,
                outcome,
                duration_ms,
                ..
            } => {
                assert_eq!(completed_child, child);
                assert_eq!(outcome, "completed");
                assert_eq!(
                    duration_ms, 0,
                    "die Laufzeit des Kindes ist an dieser Stelle nicht bekannt \
                     und wird deshalb nicht geraten"
                );
            }
            other => panic!("erwartet wurde ChildCompleted, nicht {other:?}"),
        }
    }

    // Keep the unused import lint quiet — ToolsError is used in the
    // ToolExecutorFuture return type via the type alias.
    #[allow(dead_code)]
    fn _assert_tools_error_used(_: ToolsError) {}
}
