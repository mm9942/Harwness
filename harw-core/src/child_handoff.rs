//! Übergabe-Verdichtung am Budget-Ende eines Kindes (Runde 5, Teil J).
//!
//! # Problem
//! Ein Kind mit Token-Budget bekommt ab 80 % die Abschluss-Anweisung und
//! endet beim Limit ohne harten Fehler (Teil C, Runde 4). Bisher ging dann
//! die **letzte Assistant-Antwort** als Teilergebnis an den Elternteil —
//! oft nur ein Zwischenstand („Ich lese jetzt noch …") oder ein halber Satz.
//! Die eigentliche Arbeit des Kindes (gelesene Dateien, Befunde, offene
//! Punkte) ging dem Elternteil verloren.
//!
//! # Lösung
//! Der Spawner hält vom Token-Budget eine kleine **Reserve** zurück
//! ([`handoff_reserve_tokens`]): der Turn des Kindes endet schon bei
//! `limit − reserve`. Mit der Reserve macht der Spawner genau **einen**
//! abschließenden Verdichtungsaufruf über dasselbe Kind-Modell
//! ([`run_handoff_compaction`]). Der Prompt ist der gesamte Kind-Verlauf im
//! Format der Compaction ([`crate::compaction`]), gekappt auf die Reserve;
//! die Ausgabe ist auf [`HANDOFF_MAX_OUTPUT_TOKENS`] gedeckelt. Die Antwort
//! ist eine strukturierte Übergabe ([`HANDOFF_SECTIONS`]), die als Ergebnis
//! an den Elternteil geht — klar markiert ([`HANDOFF_MARKER`],
//! [`format_handoff_text`]).
//!
//! Scheitert die Verdichtung (Modellfehler, Zeitlimit
//! [`HANDOFF_TIMEOUT`], leere oder unbrauchbare Antwort), gilt das bisherige
//! Verhalten: die letzte Assistant-Antwort als Teilergebnis. Nie ein
//! Absturz, nie ein Hängen.
//!
//! # Fortsetzen
//! Jede Budget-Übergabe landet im [`HandoffLedger`] des Spawners. Der
//! Elternteil kann ein solches Kind über `delegate_wave`
//! (`targets[].continue_from`) mit **derselben Rolle** fortsetzen: das neue
//! Kind bekommt die Übergabe als ersten Kontext ([`continuation_task`]) und
//! ein frisches Budget. Höchstens [`MAX_CONTINUATIONS`] Fortsetzungen je
//! ursprünglichem Kind; Rechte, Sandbox und Spawn-Matrix bleiben unverändert
//! (die Fortsetzung durchläuft die normale Admission, zusätzlich darf ihre
//! Sandbox nicht weiter sein als die des Vorgängers).
//!
//! # Nebenläufigkeit
//! Alle Funktionen sind zustandslos bis auf den [`HandoffLedger`], den der
//! Spawner hinter einem `Mutex` hält (nie über ein `.await`).

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

use harw_authority::SandboxSpec;
use harw_extension_api::LoadedInstructions;
use harw_protocol::items::TurnItem;
use harw_types::{ModelId, ProviderId, SessionId, TokenUsage};

use crate::compaction::{elide_middle, render_transcript};
use crate::history::ConversationHistory;
use crate::model::{ModelProvider, ModelRequest, StopReason};

/// Anteil (Prozent) des Token-Budgets, der für die Übergabe-Verdichtung
/// zurückgehalten wird.
pub const HANDOFF_RESERVE_PERCENT: u64 = 5;

/// Mindestreserve (Tokens) für die Übergabe-Verdichtung: so viel braucht
/// ein sinnvoller Verlaufsauszug plus die gedeckelte Ausgabe.
pub const HANDOFF_MIN_RESERVE_TOKENS: u64 = 8_000;

/// Höchstanteil (Prozent) des Budgets, den die Reserve belegen darf — bei
/// kleinen Budgets geht die Mindestreserve nicht zu Lasten der Arbeit.
pub const HANDOFF_MAX_RESERVE_PERCENT: u64 = 25;

/// Obergrenze der Ausgabe-Tokens des Verdichtungsaufrufs.
pub const HANDOFF_MAX_OUTPUT_TOKENS: u32 = 3_072;

/// Untergrenze der Ausgabe-Tokens, auch wenn die Reserve sehr klein ist.
const HANDOFF_MIN_OUTPUT_TOKENS: u32 = 512;

/// Token-Reserve für Systeminstruktion und Rahmen des Verdichtungs-Prompts.
const HANDOFF_PROMPT_OVERHEAD_TOKENS: u64 = 1_024;

/// Untergrenze des Verlaufsauszugs in Bytes — auch bei winziger Reserve
/// bleibt ein brauchbarer Ausschnitt (Auftrag am Kopf, Stand am Ende).
const HANDOFF_MIN_INPUT_BYTES: usize = 4 * 1024;

/// Harte Obergrenze des Übergabe-Texts in Bytes (unabhängig davon, ob der
/// Provider `max_output_tokens` einhält).
pub const HANDOFF_MAX_TEXT_BYTES: usize = 16 * 1024;

/// Zeitlimit des Verdichtungsaufrufs.
pub const HANDOFF_TIMEOUT: Duration = Duration::from_secs(60);

/// Höchstzahl Fortsetzungen je ursprünglichem Kind.
pub const MAX_CONTINUATIONS: u32 = 3;

/// Präfix jeder Ablehnung einer Fortsetzung. Der Turn-Loop macht aus einem
/// abgelehnten `transfer_to_<rolle>` mit `continue_from` daran erkennbar
/// einen Werkzeugfehler statt eines Turn-Abbruchs.
pub const CONTINUATION_REJECTION_PREFIX: &str = "continue_from:";

/// `true`, wenn `message` eine abgelehnte Fortsetzung meldet
/// ([`CONTINUATION_REJECTION_PREFIX`]).
#[must_use]
pub fn is_continuation_rejection(message: &str) -> bool {
    message
        .trim_start()
        .starts_with(CONTINUATION_REJECTION_PREFIX)
}

/// Höchstzahl vorgehaltener Übergaben je Spawner (älteste fallen heraus).
pub const HANDOFF_LEDGER_MAX_ENTRIES: usize = 64;

/// Markierung am Anfang jeder verdichteten Übergabe.
pub const HANDOFF_MARKER: &str = "[budget_exhausted: true, handoff: compacted]";

/// Die Abschnitte der Übergabe, in dieser Reihenfolge.
pub const HANDOFF_SECTIONS: [&str; 6] = [
    "## Auftrag",
    "## Erledigt",
    "## Befunde mit Belegen",
    "## Offene Punkte",
    "## Empfohlene nächste Schritte",
    "## Stand beim Abbruch",
];

/// Erkennungszeichen der Systeminstruktion (für Tests und Protokolle).
pub const HANDOFF_INSTRUCTION_MARKER: &str = "Übergabe-Zusammenfassung";

/// Systeminstruktion des Verdichtungsaufrufs.
const HANDOFF_INSTRUCTION: &str = "Du schreibst die Übergabe-Zusammenfassung eines \
Kind-Agenten, dessen Token-Budget erschöpft ist. Der Elternteil soll damit ohne den Verlauf \
weiterarbeiten können. Antworte ausschließlich mit genau diesen Markdown-Abschnitten in dieser \
Reihenfolge:\n\
## Auftrag\n(der ursprüngliche Auftrag in ein bis drei Sätzen)\n\
## Erledigt\n(was abgeschlossen ist, knapp als Liste)\n\
## Befunde mit Belegen\n(jeder Befund mit Beleg: Datei:Zeile, ausgeführter Befehl oder Quelle; \
nichts erfinden, was nicht im Verlauf steht)\n\
## Offene Punkte\n(was noch fehlt oder unsicher ist)\n\
## Empfohlene nächste Schritte\n(konkret und priorisiert)\n\
## Stand beim Abbruch\n(woran das Kind zuletzt gearbeitet hat)\n\
Keine Floskeln, keine Wiederholung des Verlaufs, keine Werkzeugaufrufe.";

/// Hinweis an eine wegen des Ausgabelimits abgeschnittene Übergabe.
const HANDOFF_TRUNCATED_NOTE: &str = "\n[Hinweis: Übergabe wegen des Ausgabelimits abgeschnitten]";

// ── Modus und Ergebnisart ─────────────────────────────────────────────────────

/// Was der Spawner am Budget-Ende eines Kindes liefert.
///
/// # Concurrency
/// `Copy`, zustandslos.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BudgetHandoffMode {
    /// Reserve zurückhalten und eine Übergabe-Verdichtung liefern (Vorgabe).
    #[default]
    Compact,
    /// Bisheriges Verhalten: die letzte Assistant-Antwort, keine Reserve.
    LastAnswer,
}

/// Wie die Rückgabe eines budget-beendeten Kindes entstand.
///
/// # Concurrency
/// `Copy`, zustandslos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetHandoff {
    /// Übergabe-Verdichtung (`handoff: compacted`).
    Compacted,
    /// Letzte Assistant-Antwort (`handoff: last_answer`), auch als Rückfall.
    LastAnswer,
}

impl BudgetHandoff {
    /// Das stabile, maschinenlesbare Label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Compacted => "compacted",
            Self::LastAnswer => "last_answer",
        }
    }
}

// ── Reserve und Plan ──────────────────────────────────────────────────────────

/// Die für die Verdichtung zurückgehaltene Reserve eines Token-Budgets.
///
/// # Beschreibung
/// `max(limit · 5 %, 8 000)`, höchstens aber 25 % des Limits. Der Turn des
/// Kindes endet bei `limit − reserve`; die Verdichtung verbraucht höchstens
/// etwa die Reserve (Eingabe gekappt, Ausgabe gedeckelt), sodass das Limit
/// insgesamt eingehalten bzw. nur um Schätzfehler überschritten wird.
///
/// # Returns
/// Die Reserve in neuen Tokens (`0` bei `limit = 0`).
#[must_use]
pub fn handoff_reserve_tokens(limit: u64) -> u64 {
    let percent = limit.saturating_mul(HANDOFF_RESERVE_PERCENT) / 100;
    let ceiling = limit.saturating_mul(HANDOFF_MAX_RESERVE_PERCENT) / 100;
    percent.max(HANDOFF_MIN_RESERVE_TOKENS).min(ceiling)
}

/// Größen eines Verdichtungsaufrufs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandoffPlan {
    /// Obergrenze der Ausgabe-Tokens (≤ [`HANDOFF_MAX_OUTPUT_TOKENS`]).
    pub max_output_tokens: u32,
    /// Byte-Budget des Verlaufsauszugs im Prompt.
    pub input_budget_bytes: usize,
}

/// Plant Ausgabe-Deckel und Eingabe-Budget aus der Reserve.
///
/// # Argumente
/// - `reserve_tokens` (`u64`): die Reserve ([`handoff_reserve_tokens`]).
/// - `window_tokens` (`Option<u64>`): Kontextfenster des Kind-Modells, falls
///   bekannt; der Auszug passt dann zusätzlich in 90 % davon.
/// - `bytes_per_token` (`f64`): kalibrierte Rate der Kind-Session.
///
/// # Returns
/// Ausgabe: die halbe Reserve, geklammert auf
/// `512 ..= HANDOFF_MAX_OUTPUT_TOKENS`. Eingabe: Reserve minus Ausgabe minus
/// Prompt-Rahmen, in Bytes, mindestens 4 KiB.
#[must_use]
pub fn plan_handoff(
    reserve_tokens: u64,
    window_tokens: Option<u64>,
    bytes_per_token: f64,
) -> HandoffPlan {
    let half = u32::try_from(reserve_tokens / 2).unwrap_or(u32::MAX);
    let max_output_tokens = half.clamp(HANDOFF_MIN_OUTPUT_TOKENS, HANDOFF_MAX_OUTPUT_TOKENS);
    let reserved = u64::from(max_output_tokens) + HANDOFF_PROMPT_OVERHEAD_TOKENS;
    let mut input_tokens = reserve_tokens.saturating_sub(reserved);
    if let Some(window) = window_tokens.filter(|window| *window > 0) {
        input_tokens = input_tokens.min(window.saturating_sub(reserved).saturating_mul(90) / 100);
    }
    let rate = if bytes_per_token.is_finite() && bytes_per_token > 0.0 {
        bytes_per_token
    } else {
        crate::context_budget::DEFAULT_BYTES_PER_TOKEN
    };
    // `as` sättigt bei f64 → usize statt überzulaufen.
    let bytes = (input_tokens as f64 * rate) as usize;
    HandoffPlan {
        max_output_tokens,
        input_budget_bytes: bytes.max(HANDOFF_MIN_INPUT_BYTES),
    }
}

/// Baut den Verlaufsauszug für die Verdichtung.
///
/// # Beschreibung
/// Derselbe Reintext wie der Compaction-Prompt; ist er größer als
/// `plan.input_budget_bytes`, wird die Mitte ausgelassen (ein Drittel Kopf
/// mit dem Auftrag, zwei Drittel Ende mit dem jüngsten Stand).
///
/// # Returns
/// `None`, wenn der Verlauf keinen verwertbaren Text enthält.
#[must_use]
pub fn build_handoff_prompt(items: &[TurnItem], plan: &HandoffPlan) -> Option<String> {
    let transcript = render_transcript(items);
    if transcript.trim().is_empty() {
        return None;
    }
    Some(elide_middle(&transcript, plan.input_budget_bytes, 34))
}

// ── Verdichtungsaufruf ────────────────────────────────────────────────────────

/// Ergebnis eines erfolgreichen Verdichtungsaufrufs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffSummary {
    /// Der (gekappte) Übergabe-Text ohne Markierung.
    pub text: String,
    /// `true`, wenn die Ausgabe am Limit abgeschnitten wurde.
    pub truncated: bool,
    /// Nutzung des Aufrufs.
    pub usage: TokenUsage,
}

/// Warum die Verdichtung nichts Brauchbares lieferte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandoffFailure {
    /// Kein verwertbarer Verlauf.
    NothingToCompact,
    /// Der Modellaufruf schlug fehl.
    Model(String),
    /// Das Zeitlimit lief ab.
    Timeout,
    /// Der Kind-Lauf wurde während der Verdichtung abgebrochen.
    Cancelled,
    /// Die Antwort war leer oder unbrauchbar (Ablehnung, Filter, Fenster).
    Unusable(String),
}

impl std::fmt::Display for HandoffFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NothingToCompact => f.write_str("kein verwertbarer Verlauf"),
            Self::Model(error) => write!(f, "Modellfehler: {error}"),
            Self::Timeout => write!(
                f,
                "Zeitlimit von {} s abgelaufen",
                HANDOFF_TIMEOUT.as_secs()
            ),
            Self::Cancelled => f.write_str("Kind-Lauf abgebrochen"),
            Self::Unusable(reason) => write!(f, "unbrauchbare Antwort: {reason}"),
        }
    }
}

/// Modell und Grenzen eines Verdichtungsaufrufs.
#[derive(Debug, Clone)]
pub struct HandoffCall {
    /// Modell-ID des Kindes (`None` = Provider-Default).
    pub model_id: Option<ModelId>,
    /// Provider-ID des Kindes.
    pub provider_id: Option<ProviderId>,
    /// Ausgabe-Deckel.
    pub max_output_tokens: u32,
    /// Zeitlimit (produktiv [`HANDOFF_TIMEOUT`]).
    pub timeout: Duration,
}

/// Führt genau **einen** Verdichtungsaufruf aus.
///
/// # Beschreibung
/// Keine Werkzeuge, Systeminstruktion mit den Pflichtabschnitten
/// ([`HANDOFF_SECTIONS`]), Ausgabe auf `call.max_output_tokens` gedeckelt,
/// das Ganze unter `call.timeout`. Ein am Limit abgeschnittenes Ergebnis
/// bleibt erhalten und wird markiert; der Text wird zusätzlich auf
/// [`HANDOFF_MAX_TEXT_BYTES`] gekappt. Kein Wiederholungsversuch.
///
/// # Errors
/// [`HandoffFailure`] bei leerem Auszug, Modellfehler, Zeitlimit, leerer
/// Antwort oder Ablehnung/Filter/Fensterüberlauf. Der Aufrufer fällt dann
/// auf die letzte Assistant-Antwort zurück.
///
/// # Concurrency
/// `async`; hält keine Locks. Das Future ist abbrechbar (Drop).
pub async fn run_handoff_compaction(
    model: &dyn ModelProvider,
    transcript: String,
    call: &HandoffCall,
) -> Result<HandoffSummary, HandoffFailure> {
    if transcript.trim().is_empty() {
        return Err(HandoffFailure::NothingToCompact);
    }
    let mut history = ConversationHistory::new();
    history.push_user_text(format!(
        "Verlauf des Kind-Agenten (Mitte ggf. ausgelassen):\n\n{transcript}\n\nSchreibe jetzt die \
         Übergabe mit genau den verlangten Abschnitten."
    ));
    let instructions = LoadedInstructions {
        system_prompt: HANDOFF_INSTRUCTION.to_owned(),
        fragments: Vec::new(),
    };
    let request = ModelRequest::new(instructions, Vec::new(), history, Vec::new())
        .with_model_id(call.model_id.clone())
        .with_provider_id(call.provider_id.clone())
        .with_max_output_tokens(Some(call.max_output_tokens));

    let response = match tokio::time::timeout(call.timeout, model.respond(request)).await {
        Err(_elapsed) => return Err(HandoffFailure::Timeout),
        Ok(Err(error)) => return Err(HandoffFailure::Model(error.to_string())),
        Ok(Ok(response)) => response,
    };
    let truncated = match &response.stop {
        StopReason::MaxTokens => true,
        StopReason::Refusal { .. }
        | StopReason::ContentFilter
        | StopReason::ContextWindowExceeded => {
            return Err(HandoffFailure::Unusable(format!("{:?}", response.stop)));
        }
        _ => false,
    };
    let body = response
        .message
        .as_deref()
        .map(str::trim)
        .unwrap_or_default();
    if body.is_empty() {
        return Err(HandoffFailure::Unusable("leere Antwort".to_owned()));
    }
    let mut text = elide_middle(body, HANDOFF_MAX_TEXT_BYTES, 60);
    if truncated {
        text.push_str(HANDOFF_TRUNCATED_NOTE);
    }
    Ok(HandoffSummary {
        text,
        truncated,
        usage: response.usage,
    })
}

// ── Darstellung ───────────────────────────────────────────────────────────────

/// Kennzahlen, die die markierte Übergabe nennt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandoffFacts<'a> {
    /// Das Kind.
    pub child: &'a SessionId,
    /// Seine Rolle (für den Fortsetzungs-Hinweis).
    pub role: &'a str,
    /// Modellrunden des beendeten Laufs.
    pub model_rounds: u32,
    /// Werkzeugaufrufe des beendeten Laufs.
    pub tool_calls: u32,
    /// Token-Budget des Kindes.
    pub limit: u64,
    /// Verbrauch (neue Tokens) beim Abbruch, ohne Verdichtung.
    pub used: u64,
    /// Ob die ungekürzte letzte Antwort über `agent.result` abrufbar ist.
    pub raw_available: bool,
    /// Noch mögliche Fortsetzungen dieser Kette.
    pub continuations_left: u32,
}

/// Baut den markierten Ergebnistext einer verdichteten Übergabe.
///
/// # Returns
/// Kopfzeile mit [`HANDOFF_MARKER`] und den Kennzahlen, Hinweise auf
/// `agent.result` und die Fortsetzung, dann die Übergabe.
#[must_use]
pub fn format_handoff_text(summary: &str, facts: &HandoffFacts<'_>) -> String {
    let child = facts.child;
    let mut text = format!(
        "{HANDOFF_MARKER} Budget erreicht – Übergabe-Zusammenfassung (verdichtet aus {} \
         Modellrunden, {} Tool-Aufrufen; Token-Budget {}, verbraucht {}).\n",
        facts.model_rounds, facts.tool_calls, facts.limit, facts.used
    );
    if facts.raw_available {
        text.push_str(&format!(
            "Die ungekürzte letzte Antwort des Kindes liefert {} {{\"child_id\":\"{child}\"}}.\n",
            crate::child_controller::AGENT_RESULT_TOOL
        ));
    }
    if facts.continuations_left > 0 {
        text.push_str(&format!(
            "Fortsetzen (noch {} von {MAX_CONTINUATIONS} Fortsetzungen möglich), mit derselben \
             Rolle und frischem Budget: {HANDOFF_PREFIX}{role} {{\"task\": …, \
             \"continue_from\": \"{child}\"}} oder ein delegate_wave-Ziel {{\"role\": \
             \"{role}\", \"continue_from\": \"{child}\"}}.\n",
            facts.continuations_left,
            HANDOFF_PREFIX = crate::turn_loop::HANDOFF_PREFIX,
            role = facts.role,
        ));
    } else {
        text.push_str(&format!(
            "Keine weitere Fortsetzung möglich (Kettengrenze {MAX_CONTINUATIONS} erreicht).\n"
        ));
    }
    text.push('\n');
    text.push_str(summary);
    text
}

// ── Fortsetzungs-Buch ─────────────────────────────────────────────────────────

/// Wie der fortgesetzte Vorgänger endete — bestimmt den Einleitungssatz von
/// [`continuation_task`].
///
/// # Beschreibung
/// Früher hieß es immer „dessen Budget erschöpft war", auch nach einem
/// Provider-Fehler (Export 429: HTTP 429 vor dem ersten Arbeitsschritt).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PredecessorEnd {
    /// Token-, Zeit- oder Werkzeugbudget erschöpft.
    BudgetExhausted,
    /// Mit einem Fehler beendet (Provider, Lease, Turn-Ergebnis …).
    Failed {
        /// Grund in Kurzform (wie im Endbericht).
        reason: String,
    },
    /// An einer Turn-Grenze oder von einem Turn-Wächter beendet.
    Stopped {
        /// Die konkrete Grenze bzw. der Wächter.
        reason: String,
    },
    /// Abgebrochen (Nutzerin, Elternteil, Herunterfahren).
    Cancelled,
    /// Runde 9, E3: regulär abgeschlossen; der Elternteil nimmt es mit einem
    /// neuen Auftrag wieder auf (z. B. `agent.message` an ein beendetes Kind).
    Completed,
}

impl PredecessorEnd {
    /// Der Satz, mit dem die Fortsetzung ihren Vorgänger einordnet.
    #[must_use]
    fn sentence(&self) -> String {
        match self {
            Self::BudgetExhausted => "Du setzt die Arbeit eines Vorgängers derselben Rolle fort, \
                                      dessen Budget erschöpft war."
                .to_owned(),
            Self::Failed { reason } => format!(
                "Du setzt die Arbeit eines Vorgängers derselben Rolle fort, der mit einem Fehler \
                 endete ({}). Prüfe zuerst, ob die Ursache noch besteht; bis dahin Erreichtes \
                 bleibt gültig.",
                reason.trim()
            ),
            Self::Stopped { reason } => format!(
                "Du setzt die Arbeit eines Vorgängers derselben Rolle fort, der vorzeitig \
                 beendet wurde ({}).",
                reason.trim()
            ),
            Self::Cancelled => "Du setzt die Arbeit eines Vorgängers derselben Rolle fort, der \
                                abgebrochen wurde."
                .to_owned(),
            Self::Completed => "Du setzt die Arbeit eines Vorgängers derselben Rolle fort, der \
                                regulär abgeschlossen hatte; der Auftrag oben ist die neue \
                                Nachricht des Elternteils."
                .to_owned(),
        }
    }
}

/// Eine vorgehaltene Budget-Übergabe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffRecord {
    /// Das budget-beendete Kind.
    pub child: SessionId,
    /// Sein Elternteil — nur er darf fortsetzen.
    pub parent: SessionId,
    /// Seine Rolle — die Fortsetzung muss dieselbe tragen.
    pub role: String,
    /// Seine Sandbox — die Fortsetzung darf nicht weiter sein. `None`, wenn
    /// sie nicht lesbar war (dann ist keine Fortsetzung möglich).
    pub sandbox: Option<SandboxSpec>,
    /// Der Übergabe-Text (Verdichtung bzw. letzte Antwort).
    pub handoff: String,
    /// Wie der Text entstand.
    pub kind: BudgetHandoff,
    /// Das ursprüngliche Kind der Kette.
    pub origin: SessionId,
    /// Wie das Kind endete (Budget, Fehler, Abbruch).
    pub end: PredecessorEnd,
}

/// Verknüpfung einer Fortsetzung mit ihrem Vorgänger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuationLink {
    /// Das fortgesetzte Kind.
    pub of: SessionId,
    /// Das ursprüngliche Kind der Kette.
    pub origin: SessionId,
    /// Nummer dieser Fortsetzung in der Kette (1 ..= [`MAX_CONTINUATIONS`]).
    pub number: u32,
}

/// Geprüfte Grundlage einer Fortsetzung ([`HandoffLedger::prepare`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuationSeed {
    /// Das fortgesetzte Kind.
    pub of: SessionId,
    /// Das ursprüngliche Kind der Kette.
    pub origin: SessionId,
    /// Der Elternteil (Aufrufer).
    pub parent: SessionId,
    /// Die Rolle.
    pub role: String,
    /// Die Sandbox des Vorgängers (Obergrenze der Fortsetzung).
    pub sandbox: Option<SandboxSpec>,
    /// Die Übergabe des Vorgängers.
    pub handoff: String,
    /// Wie der Vorgänger endete.
    pub end: PredecessorEnd,
}

/// Abgewiesene Fortsetzung; die Meldung geht an das Modell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuationError(pub String);

impl std::fmt::Display for ContinuationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ContinuationError {}

/// Buch der Budget-Übergaben und Fortsetzungsketten eines Spawners.
///
/// # Beschreibung
/// Hält höchstens [`HANDOFF_LEDGER_MAX_ENTRIES`] Übergaben (älteste fallen
/// heraus) und je ursprünglichem Kind die Zahl gebundener Fortsetzungen.
///
/// # Concurrency
/// Kein interner Lock; der Spawner hält das Buch hinter einem `Mutex`.
#[derive(Debug, Default)]
pub struct HandoffLedger {
    records: VecDeque<HandoffRecord>,
    continuations: BTreeMap<String, u32>,
}

impl HandoffLedger {
    /// Legt eine Übergabe ab; eine ältere desselben Kindes wird ersetzt.
    pub fn record(&mut self, record: HandoffRecord) {
        self.records.retain(|entry| entry.child != record.child);
        self.records.push_back(record);
        while self.records.len() > HANDOFF_LEDGER_MAX_ENTRIES {
            self.records.pop_front();
        }
    }

    /// Die Übergabe eines Kindes, falls vorgehalten.
    #[must_use]
    pub fn get(&self, child: &SessionId) -> Option<&HandoffRecord> {
        self.records
            .iter()
            .rev()
            .find(|entry| &entry.child == child)
    }

    /// Bereits gebundene Fortsetzungen der Kette mit Ursprung `origin`.
    #[must_use]
    pub fn continuations_used(&self, origin: &SessionId) -> u32 {
        self.continuations
            .get(origin.as_str())
            .copied()
            .unwrap_or(0)
    }

    /// Noch mögliche Fortsetzungen der Kette mit Ursprung `origin`.
    #[must_use]
    pub fn continuations_left(&self, origin: &SessionId) -> u32 {
        MAX_CONTINUATIONS.saturating_sub(self.continuations_used(origin))
    }

    /// Prüft, ob `caller` das Kind `from` mit der Rolle `role` fortsetzen darf.
    ///
    /// # Errors
    /// [`ContinuationError`], wenn
    /// - `from` kein am Budget beendetes Kind von `caller` ist (immer dieselbe
    ///   Meldung — kein Orakel über fremde Kinder),
    /// - `role` nicht die Rolle des Vorgängers ist,
    /// - die Kette schon [`MAX_CONTINUATIONS`] Fortsetzungen hat.
    pub fn prepare(
        &self,
        caller: &SessionId,
        from: &SessionId,
        role: &str,
    ) -> Result<ContinuationSeed, ContinuationError> {
        let record = self
            .get(from)
            .filter(|record| &record.parent == caller)
            .ok_or_else(|| {
                ContinuationError(format!(
                    "continue_from: kein eigenes, beendetes Kind mit der ID {from} \
                     (nur solche Kinder lassen sich fortsetzen)"
                ))
            })?;
        if record.role != role {
            return Err(ContinuationError(format!(
                "continue_from: eine Fortsetzung muss dieselbe Rolle tragen wie das \
                 fortgesetzte Kind ('{}'), nicht '{role}'",
                record.role
            )));
        }
        if self.continuations_used(&record.origin) >= MAX_CONTINUATIONS {
            return Err(ContinuationError(format!(
                "continue_from: Kettengrenze erreicht — höchstens {MAX_CONTINUATIONS} \
                 Fortsetzungen je ursprünglichem Kind ({}). Führe die Übergaben selbst zusammen \
                 oder schneide den Auftrag neu und kleiner zu.",
                record.origin
            )));
        }
        Ok(ContinuationSeed {
            of: record.child.clone(),
            origin: record.origin.clone(),
            parent: record.parent.clone(),
            role: record.role.clone(),
            sandbox: record.sandbox.clone(),
            handoff: record.handoff.clone(),
            end: record.end.clone(),
        })
    }

    /// Bindet eine Fortsetzung verbindlich an ihre Kette (zählt sie).
    ///
    /// # Errors
    /// [`ContinuationError`], wenn die Kette inzwischen voll ist (nebenläufige
    /// Fortsetzungen desselben Ursprungs).
    pub fn bind(&mut self, seed: &ContinuationSeed) -> Result<ContinuationLink, ContinuationError> {
        let used = self.continuations_used(&seed.origin);
        if used >= MAX_CONTINUATIONS {
            return Err(ContinuationError(format!(
                "continue_from: Kettengrenze erreicht — höchstens {MAX_CONTINUATIONS} \
                 Fortsetzungen je ursprünglichem Kind ({})",
                seed.origin
            )));
        }
        let number = used.saturating_add(1);
        self.continuations
            .insert(seed.origin.as_str().to_owned(), number);
        Ok(ContinuationLink {
            of: seed.of.clone(),
            origin: seed.origin.clone(),
            number,
        })
    }
}

/// Der Auftrag einer Fortsetzung: Kennzeichnung, Übergabe als erster
/// Kontext, dann der (optionale) neue Auftrag.
///
/// # Beschreibung
/// Beginnt mit „Fortsetzung von <id>", damit Agent-Panel und Ereignisse die
/// Fortsetzung als solche zeigen (Kurzkopf des Auftrags). Der
/// Einleitungssatz folgt dem tatsächlichen Ende des Vorgängers (`end`).
#[must_use]
pub fn continuation_task(
    of: &SessionId,
    end: &PredecessorEnd,
    handoff: &str,
    task: Option<&str>,
) -> String {
    let task = task
        .map(str::trim)
        .filter(|task| !task.is_empty())
        .unwrap_or("Arbeite die offenen Punkte der Übergabe ab.");
    format!(
        "Fortsetzung von {of}: {task}\n\n{} Fang nicht von vorn an: stütze dich auf seine \
         Übergabe, prüfe Befunde nur, wo nötig, und arbeite an den offenen Punkten und \
         empfohlenen nächsten Schritten weiter.\n\n--- Übergabe des Vorgängers ---\n{handoff}\n\
         --- Ende der Übergabe ---",
        end.sentence()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ModelFuture, ModelResponse};
    use crate::test_support::{TestError, TestResult};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Modell, das eine feste Antwort gibt und jede Anfrage aufzeichnet.
    struct RecordingModel {
        reply: Option<String>,
        stop: StopReason,
        calls: AtomicUsize,
        max_output: Mutex<Vec<Option<u32>>>,
        tools: Mutex<Vec<usize>>,
    }

    impl RecordingModel {
        fn new(reply: Option<&str>, stop: StopReason) -> Self {
            Self {
                reply: reply.map(ToOwned::to_owned),
                stop,
                calls: AtomicUsize::new(0),
                max_output: Mutex::new(Vec::new()),
                tools: Mutex::new(Vec::new()),
            }
        }
    }

    impl ModelProvider for RecordingModel {
        fn respond<'a>(&'a self, request: ModelRequest) -> ModelFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.max_output
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(request.max_output_tokens);
                self.tools
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(request.tools.len());
                Ok(ModelResponse {
                    message: self.reply.clone(),
                    stop: self.stop.clone(),
                    usage: TokenUsage {
                        input_tokens: 2_000,
                        output_tokens: 500,
                        ..TokenUsage::default()
                    },
                    ..Default::default()
                })
            })
        }
    }

    /// Modell, das nie antwortet.
    struct HangingModel;

    impl ModelProvider for HangingModel {
        fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
            Box::pin(std::future::pending::<
                Result<ModelResponse, crate::model::ModelError>,
            >())
        }
    }

    fn call(timeout: Duration) -> HandoffCall {
        HandoffCall {
            model_id: None,
            provider_id: None,
            max_output_tokens: HANDOFF_MAX_OUTPUT_TOKENS,
            timeout,
        }
    }

    fn structured_reply() -> String {
        HANDOFF_SECTIONS
            .iter()
            .map(|section| format!("{section}\n- Inhalt"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_reserve_is_five_percent_with_a_floor_and_a_ceiling() {
        // 5 % von 1 000 000 = 50 000 (über der Mindestreserve).
        assert_eq!(handoff_reserve_tokens(1_000_000), 50_000);
        // 5 % von 60 000 = 3 000 < 8 000 ⇒ Mindestreserve.
        assert_eq!(handoff_reserve_tokens(60_000), HANDOFF_MIN_RESERVE_TOKENS);
        // Kleines Budget: höchstens 25 %.
        assert_eq!(handoff_reserve_tokens(1_000), 250);
        assert_eq!(handoff_reserve_tokens(0), 0);
    }

    #[test]
    fn the_plan_caps_the_output_and_fits_the_input_into_the_reserve() {
        let plan = plan_handoff(50_000, None, 4.0);
        assert_eq!(plan.max_output_tokens, HANDOFF_MAX_OUTPUT_TOKENS);
        let reserved = u64::from(HANDOFF_MAX_OUTPUT_TOKENS) + HANDOFF_PROMPT_OVERHEAD_TOKENS;
        assert_eq!(plan.input_budget_bytes, ((50_000 - reserved) * 4) as usize);

        // Kleine Reserve: Ausgabe nie unter der Untergrenze, Eingabe nie
        // unter dem Mindestauszug.
        let small = plan_handoff(250, None, 4.0);
        assert_eq!(small.max_output_tokens, HANDOFF_MIN_OUTPUT_TOKENS);
        assert_eq!(small.input_budget_bytes, HANDOFF_MIN_INPUT_BYTES);

        // Ein kleines Fenster kappt den Auszug zusätzlich.
        let windowed = plan_handoff(50_000, Some(16_000), 4.0);
        assert!(windowed.input_budget_bytes < plan.input_budget_bytes);
    }

    #[test]
    fn the_prompt_keeps_head_and_tail_of_a_long_history() -> TestResult {
        let mut history = ConversationHistory::new();
        history.push_user_text("AUFTRAG: finde alle Aufrufer");
        for index in 0..400 {
            history.push_assistant_text(format!("Zwischenstand {index} {}", "x".repeat(200)), None);
        }
        history.push_assistant_text("LETZTER STAND", None);
        let plan = HandoffPlan {
            max_output_tokens: HANDOFF_MAX_OUTPUT_TOKENS,
            input_budget_bytes: 8 * 1024,
        };
        let prompt =
            build_handoff_prompt(history.items(), &plan).ok_or(TestError::Missing("prompt"))?;
        assert!(prompt.len() <= plan.input_budget_bytes, "{}", prompt.len());
        assert!(prompt.contains("AUFTRAG"));
        assert!(prompt.contains("LETZTER STAND"));
        assert!(prompt.contains("ausgelassen"));
        assert!(build_handoff_prompt(&[], &plan).is_none());
        Ok(())
    }

    #[tokio::test]
    async fn one_call_without_tools_and_with_capped_output_yields_the_sections() -> TestResult {
        let model = RecordingModel::new(Some(&structured_reply()), StopReason::EndTurn);
        let summary =
            run_handoff_compaction(&model, "User: Auftrag\n".to_owned(), &call(HANDOFF_TIMEOUT))
                .await
                .map_err(|error| TestError::Unexpected(error.to_string()))?;
        for section in HANDOFF_SECTIONS {
            assert!(summary.text.contains(section), "{section}");
        }
        assert!(!summary.truncated);
        assert_eq!(summary.usage.fresh_tokens(), 2_500);
        assert_eq!(model.calls.load(Ordering::SeqCst), 1, "genau ein Aufruf");
        let max_output = model
            .max_output
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert_eq!(max_output, vec![Some(HANDOFF_MAX_OUTPUT_TOKENS)]);
        let tools = model
            .tools
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert_eq!(tools, vec![0], "die Verdichtung bietet keine Werkzeuge an");
        Ok(())
    }

    #[tokio::test]
    async fn an_oversized_reply_is_capped_and_a_truncated_one_is_marked() -> TestResult {
        let huge = "y".repeat(HANDOFF_MAX_TEXT_BYTES * 3);
        let model = RecordingModel::new(Some(&huge), StopReason::MaxTokens);
        let summary =
            run_handoff_compaction(&model, "User: Auftrag\n".to_owned(), &call(HANDOFF_TIMEOUT))
                .await
                .map_err(|error| TestError::Unexpected(error.to_string()))?;
        assert!(summary.truncated);
        assert!(
            summary.text.len() <= HANDOFF_MAX_TEXT_BYTES + HANDOFF_TRUNCATED_NOTE.len(),
            "{}",
            summary.text.len()
        );
        assert!(summary.text.ends_with(HANDOFF_TRUNCATED_NOTE));
        Ok(())
    }

    #[tokio::test]
    async fn failures_are_typed_and_never_hang() -> TestResult {
        let empty = RecordingModel::new(Some("   "), StopReason::EndTurn);
        let refused = RecordingModel::new(Some("nein"), StopReason::Refusal { detail: None });
        let transcript = || "User: Auftrag\n".to_owned();
        assert!(matches!(
            run_handoff_compaction(&empty, transcript(), &call(HANDOFF_TIMEOUT)).await,
            Err(HandoffFailure::Unusable(_))
        ));
        assert!(matches!(
            run_handoff_compaction(&refused, transcript(), &call(HANDOFF_TIMEOUT)).await,
            Err(HandoffFailure::Unusable(_))
        ));
        assert!(matches!(
            run_handoff_compaction(&empty, "  ".to_owned(), &call(HANDOFF_TIMEOUT)).await,
            Err(HandoffFailure::NothingToCompact)
        ));
        let started = std::time::Instant::now();
        let hung = run_handoff_compaction(
            &HangingModel,
            transcript(),
            &call(Duration::from_millis(50)),
        )
        .await;
        assert_eq!(hung, Err(HandoffFailure::Timeout));
        assert!(started.elapsed() < Duration::from_secs(5));
        Ok(())
    }

    #[test]
    fn the_formatted_handoff_carries_marker_counts_and_hints() {
        let child = SessionId::new();
        let text = format_handoff_text(
            "## Auftrag\nX",
            &HandoffFacts {
                child: &child,
                role: "explorer",
                model_rounds: 7,
                tool_calls: 12,
                limit: 60_000,
                used: 52_000,
                raw_available: true,
                continuations_left: 3,
            },
        );
        assert!(text.starts_with(HANDOFF_MARKER), "{text}");
        assert!(text.contains("Budget erreicht – Übergabe-Zusammenfassung"));
        assert!(text.contains("7 Modellrunden, 12 Tool-Aufrufen"));
        assert!(text.contains(&format!("agent.result {{\"child_id\":\"{child}\"}}")));
        assert!(text.contains("continue_from"));
        // Beide Wege: Handoff-Werkzeug des Elternteils und delegate_wave.
        assert!(text.contains("transfer_to_explorer"), "{text}");
        assert!(text.contains("delegate_wave"), "{text}");
        assert!(text.ends_with("## Auftrag\nX"));

        let last = format_handoff_text(
            "S",
            &HandoffFacts {
                child: &child,
                role: "explorer",
                model_rounds: 1,
                tool_calls: 0,
                limit: 1,
                used: 1,
                raw_available: false,
                continuations_left: 0,
            },
        );
        assert!(!last.contains("agent.result"));
        assert!(last.contains("Kettengrenze"));
    }

    fn record(child: &SessionId, parent: &SessionId, origin: &SessionId) -> HandoffRecord {
        HandoffRecord {
            child: child.clone(),
            parent: parent.clone(),
            role: "explorer".to_owned(),
            sandbox: None,
            handoff: "## Offene Punkte\n- Rest".to_owned(),
            kind: BudgetHandoff::Compacted,
            origin: origin.clone(),
            end: PredecessorEnd::BudgetExhausted,
        }
    }

    #[test]
    fn only_own_budget_ended_children_of_the_same_role_can_be_continued() -> TestResult {
        let parent = SessionId::new();
        let stranger = SessionId::new();
        let child = SessionId::new();
        let unknown = SessionId::new();
        let mut ledger = HandoffLedger::default();
        ledger.record(record(&child, &parent, &child));

        let seed = ledger
            .prepare(&parent, &child, "explorer")
            .map_err(|error| TestError::Unexpected(error.to_string()))?;
        assert_eq!(seed.of, child);
        assert_eq!(seed.origin, child);
        assert!(seed.handoff.contains("Offene Punkte"));

        let foreign = ledger.prepare(&stranger, &child, "explorer");
        let missing = ledger.prepare(&parent, &unknown, "explorer");
        assert!(foreign.is_err() && missing.is_err());
        // Fremdes und unbekanntes Kind bekommen dieselbe Meldung (kein Orakel).
        let foreign = foreign
            .err()
            .map(|error| error.0.replace(child.as_str(), "<id>"));
        let missing = missing
            .err()
            .map(|error| error.0.replace(unknown.as_str(), "<id>"));
        assert_eq!(foreign, missing);

        let other_role = ledger.prepare(&parent, &child, "coding-orchestrator");
        assert!(
            other_role.is_err_and(|error| error.0.contains("dieselbe Rolle")),
            "eine andere Rolle erweitert womöglich die Rechte"
        );
        Ok(())
    }

    #[test]
    fn the_chain_allows_at_most_three_continuations_per_origin() -> TestResult {
        let parent = SessionId::new();
        let origin = SessionId::new();
        let mut ledger = HandoffLedger::default();
        ledger.record(record(&origin, &parent, &origin));
        let mut previous = origin.clone();
        for number in 1..=MAX_CONTINUATIONS {
            let seed = ledger
                .prepare(&parent, &previous, "explorer")
                .map_err(|error| TestError::Unexpected(error.to_string()))?;
            let link = ledger
                .bind(&seed)
                .map_err(|error| TestError::Unexpected(error.to_string()))?;
            assert_eq!(link.number, number);
            assert_eq!(link.origin, origin);
            // Die Fortsetzung endet ihrerseits am Budget.
            let next = SessionId::new();
            ledger.record(record(&next, &parent, &origin));
            previous = next;
        }
        assert_eq!(ledger.continuations_left(&origin), 0);
        let refused = ledger.prepare(&parent, &previous, "explorer");
        assert!(
            refused.is_err_and(|error| error.0.contains("Kettengrenze")),
            "die vierte Fortsetzung wird abgewiesen"
        );
        Ok(())
    }

    #[test]
    fn a_race_past_the_chain_limit_is_caught_at_bind() -> TestResult {
        let parent = SessionId::new();
        let origin = SessionId::new();
        let mut ledger = HandoffLedger::default();
        ledger.record(record(&origin, &parent, &origin));
        let seeds = (0..=MAX_CONTINUATIONS)
            .map(|_| ledger.prepare(&parent, &origin, "explorer"))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| TestError::Unexpected(error.to_string()))?;
        let bound = seeds
            .iter()
            .filter(|seed| ledger.bind(seed).is_ok())
            .count();
        assert_eq!(bound, MAX_CONTINUATIONS as usize);
        Ok(())
    }

    /// Runde 9, E3: die Fortsetzung eines regulär beendeten Kindes nennt die
    /// Nachricht des Elternteils als neuen Auftrag und trägt die Übergabe.
    #[test]
    fn a_completed_predecessor_is_resumed_with_the_message_as_task() {
        let of = SessionId::new();
        let text = continuation_task(
            &of,
            &PredecessorEnd::Completed,
            "Letzte Antwort:\nEntwurf validiert",
            Some("Szenario ist freigegeben, starte."),
        );
        assert!(text.starts_with(&format!("Fortsetzung von {of}: Szenario ist freigegeben")));
        assert!(text.contains("regulär abgeschlossen"), "{text}");
        assert!(text.contains("Entwurf validiert"), "{text}");
    }

    #[test]
    fn continuation_rejections_are_recognisable_by_their_prefix() {
        assert!(is_continuation_rejection(
            "continue_from: kein eigenes, beendetes Kind"
        ));
        assert!(!is_continuation_rejection(
            "child role 'x' is not registered"
        ));
    }

    #[test]
    fn the_continuation_task_names_its_predecessor_and_carries_the_handoff() {
        let of = SessionId::new();
        let task = continuation_task(
            &of,
            &PredecessorEnd::BudgetExhausted,
            "## Offene Punkte\n- B prüfen",
            None,
        );
        assert!(
            task.starts_with(&format!("Fortsetzung von {of}:")),
            "{task}"
        );
        assert!(task.contains("## Offene Punkte\n- B prüfen"));
        assert!(task.contains("nicht von vorn"));
        assert!(task.contains("dessen Budget erschöpft war"));
        let explicit = continuation_task(
            &of,
            &PredecessorEnd::BudgetExhausted,
            "H",
            Some("nur noch Modul C"),
        );
        assert!(explicit.contains("nur noch Modul C"));
    }

    /// Export 429: nach einem Fehler (nicht Budget) darf der Auftrag nicht
    /// behaupten, das Budget sei erschöpft gewesen.
    #[test]
    fn the_continuation_task_names_the_real_end_cause() {
        let of = SessionId::new();
        let failed = continuation_task(
            &of,
            &PredecessorEnd::Failed {
                reason: "Fehler: rate limited by provider (HTTP 429)".to_owned(),
            },
            "H",
            None,
        );
        assert!(!failed.contains("Budget erschöpft"), "{failed}");
        assert!(failed.contains("mit einem Fehler endete"), "{failed}");
        assert!(failed.contains("HTTP 429"), "{failed}");
        let cancelled = continuation_task(&of, &PredecessorEnd::Cancelled, "H", None);
        assert!(cancelled.contains("abgebrochen wurde"), "{cancelled}");
        assert!(!cancelled.contains("Budget"), "{cancelled}");
    }
}
