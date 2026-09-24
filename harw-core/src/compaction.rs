//! Verdichtung (Compaction) der Session-Historie.
//!
//! # Wann läuft das?
//!
//! Anders als [`crate::history::ConversationHistory::tail_preserving_current_turn`]
//! (das bei **jeder** Modell-Runde die Sicht auf die Historie neu zuschneidet,
//! ohne die persistente Historie zu verändern) läuft dieses Modul nur an
//! **Compaction-Grenzen** — ausgelöst durch [`crate::auto_compact::AutoCompactPolicy`],
//! durch die Pre-flight-Prüfung vor einem Modell-Request oder explizit vom
//! Aufrufer. Der Grund ist die Provider-Prompt-Cache: jede Änderung des
//! Präfix-Inhalts (z. B. ein neu geschriebenes Summary-Item anstelle vieler
//! alter Items) invalidiert den Cache des Providers für den gesamten Verlauf
//! ab dieser Stelle. Deshalb wird die Historie hier **mutiert und dauerhaft
//! ersetzt**, höchstens einmal pro Compaction-Zyklus, statt (wie
//! `tail_preserving_current_turn`) bei jedem Request neu projiziert zu werden.
//!
//! # Hybrid-Algorithmus
//!
//! 1. [`deterministic_pass`]: rein regelbasiert, ohne Modellaufruf. Arbeitet
//!    auf atomaren Gruppen ([`crate::history::ConversationHistory::atomic_groups`])
//!    und rührt den **aktuellen Turn** (alles ab der letzten `UserMessage`)
//!    nie an:
//!    - Reasoning-Items älterer Turns fallen weg (sie sind kein Modell-Input
//!      und zählen auch in keiner Budget-Schätzung dieses Moduls);
//!    - fehlgeschlagene Tool-Aufrufe fallen weg, wenn ein späterer Aufruf mit
//!      identischem Tool-Namen und kanonisch-gleichen Argumenten sie ersetzt
//!      hat (die Wiederholung war offensichtlich der eigentliche Versuch);
//!    - wiederholte, identische, erfolgreiche schreibgeschützte Lesezugriffe
//!      (`fs.read*`, `fs.list*`, `fs.search*`, `fs.grep*`, `fs.glob*`,
//!      `doc.read_pdf*`) werden bis auf den letzten dedupliziert;
//!    - übergroße alte Tool-Ergebnisse werden auf ihren Kopf plus einen
//!      Kürzungs-Hinweis reduziert.
//! 2. Zusammenfassung per Modellaufruf: [`compact_session`] verdichtet die
//!    ältere Hälfte des nicht-aktuellen Teils, [`compact_for_budget`] den
//!    gesamten nicht-aktuellen Teil. Eine bereits vorhandene Zusammenfassung
//!    ([`SUMMARY_MARKER`], [`is_pinned_summary`]) wird dabei **zusammengeführt**
//!    statt verschachtelt neu zusammengefasst. Die Eingabe ist auf das
//!    Fenster des Zusammenfassungs-Modells gekappt, die Ausgabe auf
//!    [`SUMMARY_MAX_OUTPUT_TOKENS`] begrenzt; ein abgeschnittenes Ergebnis
//!    (`StopReason::MaxTokens`) bleibt erhalten, wird aber markiert. Schlägt
//!    der Modellaufruf fehl, bleibt das deterministische Ergebnis bestehen.
//! 3. Nur [`compact_for_budget`] mit [`CompactionScope::CurrentTurn`]:
//!    gestufte Elision **im aktuellen Turn** (wichtig für Worker, deren
//!    gesamte Arbeit in einem einzigen Turn stattfindet). Die auslösende
//!    `UserMessage`, die letzten `keep_recent_rounds` Tool-Runden und die
//!    Call/Ergebnis-Paarung bleiben immer erhalten; ältere Runden werden
//!    stufenweise verkleinert, bis die Schätzung unter dem Ziel liegt:
//!    (a) Ergebnisse > 4 KiB auf Kopf/Ende gekürzt, (b) Ergebnisse durch den
//!    Platzhalter `[ausgelassen: N Bytes, tool, Ziel; erneut ausführen für
//!    Details]` ersetzt, (c) die betroffenen Runden ganz durch eine
//!    [`PROGRESS_MARKER`]-Fortschrittsnotiz ersetzt.
//!
//! # Schätzung
//!
//! Budgets rechnen in Tokens über [`estimate_history_tokens`]: dieselbe
//! Nutzlast-Zählung wie [`crate::context_budget::estimate_request_bytes`]
//! (Reasoning- und Fehler-Items zählen nicht), geteilt durch die kalibrierte
//! Rate der Session ([`crate::context_budget::TokenCalibration`]).
//!
//! # Persistenz
//!
//! Beide Einstiegspunkte ersetzen die Historie der übergebenen
//! [`AgentSession`] in-memory (`history_mut`). Das Schreiben auf einen
//! `StateStore` ist **nicht** Aufgabe dieses Moduls — das macht der Aufrufer.
//! Ein Lauf, der nichts verändert ([`CompactionOutcome::no_op`]), ersetzt die
//! Historie nicht und sendet kein `CompactionApplied`-Event; der
//! [`CompactionObserver`] wird dennoch (mit `no_op = true`) benachrichtigt.
//!
//! # Nebenläufigkeit
//!
//! [`deterministic_pass`] ist rein synchron und zustandslos.
//! [`compact_session`] und [`compact_for_budget`] sind `async`, weil sie
//! optional genau einen Modellaufruf über `&dyn ModelProvider` machen; sie
//! halten dabei keine Locks.

use crate::auto_compact::CompactDecision;
use crate::context_budget::{TokenCalibration, floor_char_boundary};
use crate::error::CoreResult;
use crate::history::ConversationHistory;
use crate::model::{ModelProvider, ModelRequest, StopReason};
use crate::session::AgentSession;
use harw_extension_api::LoadedInstructions;
use harw_protocol::items::{AssistantMessageItem, ContentPart, ToolCallResult, TurnItem};
use harw_types::{ItemId, ModelId, ProviderId, SessionId, TokenUsage, ToolCallId};
use std::collections::{BTreeSet, HashMap, HashSet};

/// Präfix, das jede vom Modell erzeugte Zusammenfassung markiert — so bleibt
/// ein verdichteter Block im Verlauf für spätere Leser (Menschen wie Modell)
/// eindeutig als solcher erkennbar. Items mit diesem Präfix gelten als
/// angeheftet ([`is_pinned_summary`]).
pub const SUMMARY_MARKER: &str = "[Verdichteter Verlauf]\n";

/// Präfix der Fortschrittsnotiz, die [`compact_for_budget`] anstelle
/// ausgelassener Runden des aktuellen Turns einfügt.
pub const PROGRESS_MARKER: &str = "[Fortschrittsnotiz]\n";

/// Obergrenze der Ausgabe-Tokens für den Zusammenfassungs-Aufruf.
pub const SUMMARY_MAX_OUTPUT_TOKENS: u32 = 4096;

/// Standardanzahl der jüngsten Tool-Runden des aktuellen Turns, die die
/// Current-Turn-Elision nie anfasst — außer unter Notdruck
/// ([`CompactionBudget::emergency`]), dann sinkt sie auf `0`.
pub const DEFAULT_KEEP_RECENT_ROUNDS: usize = 2;

/// Behaltene Bytes (Kopf + Ende) eines übergroßen Ergebnisses der jüngsten,
/// sonst geschützten Runden unter Notdruck (Teil C). Größer als
/// [`ELISION_HEAD_TAIL_BYTES`], weil diese Ergebnisse das Modell gleich
/// weiterverwenden will.
const EMERGENCY_RECENT_RESULT_BYTES: usize = 8 * 1024;

/// Angenommenes Kontextfenster des Zusammenfassungs-Modells, wenn der
/// Aufrufer keines angibt (dieselbe konservative Annahme wie für unbekannte
/// Modelle).
pub const DEFAULT_SUMMARY_WINDOW_TOKENS: u64 = 32_768;

/// Token-Reserve für Systeminstruktion und Rahmen des Zusammenfassungs-Prompts.
const SUMMARY_PROMPT_OVERHEAD_TOKENS: u64 = 1024;

/// Untergrenze des Eingabe-Budgets eines Zusammenfassungs-Aufrufs in Bytes —
/// auch bei winzigen Fenstern bleibt so ein sinnvoller Ausschnitt übrig.
const MIN_SUMMARY_INPUT_BYTES: usize = 8 * 1024;

/// Maximale Bytes eines einzelnen Tool-Ergebnisses im Zusammenfassungs-Prompt.
const SUMMARY_RENDER_RESULT_MAX_BYTES: usize = 2048;

/// Behaltene Bytes (Kopf + Ende) eines Tool-Ergebnisses in Elisionsstufe (a).
const ELISION_HEAD_TAIL_BYTES: usize = 4096;

/// Maximale Anzahl Einträge einer Fortschrittsnotiz (die jüngsten bleiben).
const PROGRESS_NOTE_MAX_ENTRIES: usize = 60;

/// Maximale Bytes eines einzelnen Textauszugs in einer Fortschrittsnotiz.
const PROGRESS_NOTE_TEXT_MAX_BYTES: usize = 240;

/// Pauschaler Rahmen-Aufschlag pro Modellnachricht in Bytes — derselbe Wert
/// wie in [`crate::context_budget::estimate_request_bytes`].
const MESSAGE_FRAMING_BYTES: u64 = 16;

/// Hinweis, der an eine wegen des Ausgabelimits abgeschnittene
/// Zusammenfassung angehängt wird.
const SUMMARY_TRUNCATED_NOTE: &str =
    "\n[Hinweis: Zusammenfassung wegen des Ausgabelimits abgeschnitten]";

/// Deutsche Systeminstruktion für den Zusammenfassungs-Aufruf.
const SUMMARY_INSTRUCTION: &str = "Fasse den bisherigen Verlauf knapp zusammen: Ziel, getroffene \
    Entscheidungen, erledigte und offene Punkte, berührte Dateien. Keine Floskeln. Liegt eine \
    bisherige Zusammenfassung vor, führe sie mit dem neuen Verlauf zu genau einer \
    aktualisierten Zusammenfassung zusammen, ohne ihre wesentlichen Punkte zu verlieren.";

/// Konfiguration eines Compaction-Laufs von [`compact_session`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionPlan {
    /// Ziel-Obergrenze in Bytes für die verdichtete Historie. Wird
    /// [`deterministic_pass`] allein nicht erreicht, greift die
    /// Zusammenfassung in [`compact_session`].
    pub target_history_bytes: usize,
    /// Wie viele Bytes eines alten Tool-Ergebnisses als Kopf erhalten
    /// bleiben, bevor der Kürzungs-Hinweis angehängt wird.
    pub tool_result_head_bytes: usize,
    /// Modell-ID für den Zusammenfassungs-Aufruf. `None` lässt den Provider
    /// seinen Katalog-Default wählen.
    pub summary_model: Option<ModelId>,
    /// Provider-ID für den Zusammenfassungs-Aufruf. `None` lässt den Aufrufer
    /// seinen konfigurierten Default-Provider verwenden (Addendum C: interne
    /// Modellstellen).
    pub summary_provider: Option<ProviderId>,
    /// Kontextfenster des Zusammenfassungs-Modells in Tokens; die Eingabe des
    /// Zusammenfassungs-Aufrufs wird darauf gekappt. `None` ⇒
    /// [`DEFAULT_SUMMARY_WINDOW_TOKENS`].
    pub summary_window_tokens: Option<u64>,
}

impl CompactionPlan {
    /// Leitet einen Plan aus dem Kontextfenster des aktiven Modells ab.
    ///
    /// # Description
    /// `target_history_bytes` = 30 % des Fensters in Tokens, grob mit 4
    /// Bytes/Token umgerechnet — derselbe Anteil, den
    /// [`crate::auto_compact::AutoCompactPolicy::task_end_threshold_tokens`]
    /// als Lohn-Schwelle für eine Verdichtung ansetzt. `tool_result_head_bytes`
    /// ist mit 4096 fest verdrahtet; `summary_model` bleibt `None`
    /// (Katalog-Default); `summary_window_tokens` ist das übergebene Fenster
    /// (das Zusammenfassungs-Modell ist ohne Override das Session-Modell).
    ///
    /// # Arguments
    /// - `tokens` (`u64`): effektives Kontextfenster in Tokens.
    ///
    /// # Returns
    /// Ein Plan mit saturierender Arithmetik — kein Überlauf bei sehr großen
    /// Fenstern.
    #[must_use]
    pub fn for_context_window(tokens: u64) -> Self {
        let target_tokens = tokens.saturating_mul(30) / 100;
        let target_bytes_u64 = target_tokens.saturating_mul(4);
        let target_history_bytes = usize::try_from(target_bytes_u64).unwrap_or(usize::MAX);
        Self {
            target_history_bytes,
            tool_result_head_bytes: 4096,
            summary_model: None,
            summary_provider: None,
            summary_window_tokens: Some(tokens),
        }
    }

    /// Leitet einen Plan direkt aus einer Ziel-Token-Zahl ab, statt aus
    /// einem Kontextfenster-Anteil (Addendum D: harte Verdichtung am
    /// Auftragsbeginn für Orchestrator-Sessions).
    ///
    /// # Arguments
    /// - `tokens` (`u64`): Ziel-Token-Zahl für die verdichtete Historie.
    ///
    /// # Returns
    /// Ein Plan mit `target_history_bytes = tokens × 4` (dieselbe
    /// Bytes/Token-Faustregel wie [`Self::for_context_window`]),
    /// `tool_result_head_bytes = 4096`, `summary_model`/`summary_provider`/
    /// `summary_window_tokens` = `None`. Saturierende Arithmetik — kein
    /// Überlauf.
    #[must_use]
    pub fn for_target_tokens(tokens: u64) -> Self {
        let target_bytes_u64 = tokens.saturating_mul(4);
        let target_history_bytes = usize::try_from(target_bytes_u64).unwrap_or(usize::MAX);
        Self {
            target_history_bytes,
            tool_result_head_bytes: 4096,
            summary_model: None,
            summary_provider: None,
            summary_window_tokens: None,
        }
    }
}

/// Welcher Teil der Historie [`compact_for_budget`] verändern darf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionScope {
    /// Nur ältere Turns (deterministischer Durchgang + Zusammenfassung); der
    /// aktuelle Turn bleibt unangetastet.
    OlderTurns,
    /// Zusätzlich gestufte Elision älterer Tool-Runden des aktuellen Turns.
    /// Die letzten `keep_recent_rounds` Runden bleiben immer vollständig.
    CurrentTurn {
        /// Anzahl der jüngsten Tool-Runden, die nie verändert werden.
        keep_recent_rounds: usize,
    },
}

impl Default for CompactionScope {
    fn default() -> Self {
        Self::CurrentTurn {
            keep_recent_rounds: DEFAULT_KEEP_RECENT_ROUNDS,
        }
    }
}

/// Konfiguration eines token-budgetierten Laufs von [`compact_for_budget`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionBudget {
    /// Ziel: geschätzte Request-Tokens (Historie + `overhead_tokens`) nach
    /// der Verdichtung. Der Lauf hört auf, sobald die Schätzung darunter liegt.
    pub target_tokens: u64,
    /// Geschätzte Tokens des Requests außerhalb der Historie
    /// (Systemprompt, Fragmente, Datenblock, Tool-Schemas). `0` ⇒ das Ziel
    /// gilt für die Historie allein.
    pub overhead_tokens: u64,
    /// Welcher Teil der Historie verändert werden darf.
    pub scope: CompactionScope,
    /// Kopf-Kappungsgrenze alter Tool-Ergebnisse im deterministischen
    /// Durchgang.
    pub tool_result_head_bytes: usize,
    /// Kontextfenster des Zusammenfassungs-Modells in Tokens. `None` ⇒
    /// [`DEFAULT_SUMMARY_WINDOW_TOKENS`].
    pub summary_window_tokens: Option<u64>,
    /// Modell-ID des Zusammenfassungs-Aufrufs. Sind Modell und Provider
    /// beide `None`, gilt [`AgentSession::compaction_summary_model`].
    pub summary_model: Option<ModelId>,
    /// Provider-ID des Zusammenfassungs-Aufrufs (siehe `summary_model`).
    pub summary_provider: Option<ProviderId>,
    /// Notdruck (Teil C): reichen die regulären Stufen nicht, werden
    /// zusätzlich übergroße Ergebnisse der geschützten jüngsten Runden auf
    /// Kopf und Ende gekürzt und danach `keep_recent_rounds` auf `0` gesenkt.
    /// Nur bei [`CompactionScope::CurrentTurn`] wirksam.
    pub emergency: bool,
}

impl CompactionBudget {
    /// Budget mit Ziel `target_tokens`, ohne Overhead, mit
    /// [`CompactionScope::default`] (Current-Turn-Elision, 2 Runden
    /// geschützt), 4096 Bytes Kopf-Kappung und ohne Summary-Overrides.
    #[must_use]
    pub fn new(target_tokens: u64) -> Self {
        Self {
            target_tokens,
            overhead_tokens: 0,
            scope: CompactionScope::default(),
            tool_result_head_bytes: 4096,
            summary_window_tokens: None,
            summary_model: None,
            summary_provider: None,
            emergency: false,
        }
    }

    /// Schaltet die Notdruck-Stufen ein oder aus (siehe
    /// [`Self::emergency`]).
    #[must_use]
    pub fn with_emergency(mut self, emergency: bool) -> Self {
        self.emergency = emergency;
        self
    }

    /// Setzt den Nicht-Historien-Anteil des Requests in Tokens.
    #[must_use]
    pub fn with_overhead_tokens(mut self, overhead_tokens: u64) -> Self {
        self.overhead_tokens = overhead_tokens;
        self
    }

    /// Leitet den Nicht-Historien-Anteil aus einem gebauten Request ab:
    /// `estimate_request_tokens(request) − estimate_history_tokens(request.history)`.
    #[must_use]
    pub fn with_request_overhead(
        self,
        request: &ModelRequest,
        calibration: &TokenCalibration,
    ) -> Self {
        let total = crate::context_budget::estimate_request_tokens(request, calibration);
        let history = estimate_history_tokens(&request.history, calibration);
        self.with_overhead_tokens(total.saturating_sub(history))
    }

    /// Setzt den erlaubten Wirkungsbereich.
    #[must_use]
    pub fn with_scope(mut self, scope: CompactionScope) -> Self {
        self.scope = scope;
        self
    }

    /// Setzt das Kontextfenster des Zusammenfassungs-Modells.
    #[must_use]
    pub fn with_summary_window_tokens(mut self, tokens: Option<u64>) -> Self {
        self.summary_window_tokens = tokens;
        self
    }

    /// Setzt Provider und Modell des Zusammenfassungs-Aufrufs.
    #[must_use]
    pub fn with_summary_model(
        mut self,
        provider: Option<ProviderId>,
        model: Option<ModelId>,
    ) -> Self {
        self.summary_provider = provider;
        self.summary_model = model;
        self
    }
}

/// Ergebnis eines Compaction-Laufs — von [`deterministic_pass`] allein, von
/// [`compact_session`] und von [`compact_for_budget`] zurückgegeben.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompactionOutcome {
    /// Geschätzte Bytes der Historie vor der Verdichtung (ohne Reasoning).
    pub bytes_before: usize,
    /// Geschätzte Bytes der Historie nach der Verdichtung (ohne Reasoning).
    pub bytes_after: usize,
    /// Geschätzte Tokens vor der Verdichtung (Historie über
    /// [`estimate_history_tokens`]; bei [`compact_for_budget`] zuzüglich
    /// [`CompactionBudget::overhead_tokens`]). Bleibt bei
    /// [`deterministic_pass`] allein `0`.
    pub tokens_before: u64,
    /// Geschätzte Tokens nach der Verdichtung (wie `tokens_before`).
    pub tokens_after: u64,
    /// Anzahl vollständig entfernter Items (fehlgeschlagene, wiederholte
    /// Tool-Aufrufe — Schritt (a) — sowie Reasoning-Items älterer Turns).
    pub items_dropped: usize,
    /// Anzahl auf Kopf + Hinweis gekürzter Tool-Ergebnisse (Schritt (c)).
    pub results_truncated: usize,
    /// Anzahl deduplizierter, identischer, erfolgreicher Lesezugriffe
    /// (Schritt (b)).
    pub calls_deduplicated: usize,
    /// Anzahl der Tool-Ergebnisse des aktuellen Turns, die die
    /// Current-Turn-Elision gekürzt, ersetzt oder in eine Fortschrittsnotiz
    /// überführt hat.
    pub elided_results: u32,
    /// `true`, wenn die Current-Turn-Elision Runden durch eine
    /// Fortschrittsnotiz ([`PROGRESS_MARKER`]) ersetzt hat.
    pub progress_note: bool,
    /// `true`, wenn eine Modell-Zusammenfassung erzeugt wurde.
    pub summarized: bool,
    /// `true`, wenn die Zusammenfassung wegen des Ausgabelimits
    /// abgeschnitten wurde (sie bleibt dennoch erhalten und ist markiert).
    pub summary_truncated: bool,
    /// Der reine Zusammenfassungstext (ohne [`SUMMARY_MARKER`]-Präfix), falls
    /// `summarized`.
    pub summary_text: Option<String>,
    /// Token-Nutzung des Zusammenfassungs-Aufrufs (auch eines gescheiterten
    /// oder verworfenen). Wird zusätzlich als
    /// `AgentEventKind::InternalUsage { purpose: "compaction" }` gemeldet;
    /// Aufrufer können sie auf die Turn-Nutzung addieren.
    pub summary_usage: TokenUsage,
    /// `true`, wenn der Lauf die Historie nicht verändert hat. Dann wurde
    /// weder die Historie ersetzt noch ein `CompactionApplied`-Event
    /// gesendet (der [`CompactionObserver`] sieht den No-op dennoch).
    pub no_op: bool,
    /// Der Auslöser dieses Compaction-Laufs, sofern vom Aufrufer angegeben.
    pub reason: Option<CompactDecision>,
}

impl CompactionOutcome {
    /// `true`, wenn keiner der Zähler eine Veränderung meldet.
    fn changed_nothing(&self) -> bool {
        !self.summarized
            && !self.progress_note
            && self.items_dropped == 0
            && self.results_truncated == 0
            && self.calls_deduplicated == 0
            && self.elided_results == 0
    }
}

/// Beobachter, der über jede abgeschlossene Verdichtung informiert wird —
/// z. B. um ein Handoff-Artefakt zu aktualisieren.
pub trait CompactionObserver: Send + Sync {
    /// Wird am Ende jedes Laufs von [`compact_session`] bzw.
    /// [`compact_for_budget`] aufgerufen, nach dem Ersetzen der
    /// Session-Historie — auch für einen No-op-Lauf (`outcome.no_op`).
    fn on_compacted(&self, session_id: &SessionId, outcome: &CompactionOutcome);
}

/// `true`, wenn `item` eine angeheftete Zusammenfassung ist: eine
/// `AssistantMessage`, deren erster Textteil mit [`SUMMARY_MARKER`] beginnt.
///
/// # Description
/// Angeheftete Zusammenfassungen tragen den gesamten verdichteten Verlauf
/// vor ihnen. Budget-Trimmer (z. B. der Tail-Zuschnitt in
/// [`crate::history`]) dürfen sie nie verwerfen; eine erneute Verdichtung
/// führt sie mit dem neuen Verlauf zusammen, statt sie zu verschachteln.
#[must_use]
pub fn is_pinned_summary(item: &TurnItem) -> bool {
    let TurnItem::AssistantMessage(message) = item else {
        return false;
    };
    matches!(
        message.content.first(),
        Some(ContentPart::Text { text }) if text.starts_with(SUMMARY_MARKER)
    )
}

/// Geschätzte Nutzlast-Bytes einer Item-Liste, wie sie ein Provider
/// tatsächlich sendet — dieselbe Zählung wie
/// [`crate::context_budget::estimate_request_bytes`] für den Historien-Teil:
/// Texte als Rohbytes, Tool-Argumente/-Ergebnisse als kompaktes JSON, plus
/// Rahmen-Aufschlag pro Nachricht. Reasoning- und Fehler-Items zählen nicht.
#[must_use]
pub fn estimate_history_bytes(items: &[TurnItem]) -> u64 {
    items
        .iter()
        .map(model_item_bytes)
        .fold(0_u64, u64::saturating_add)
}

/// Geschätzte Tokens einer Historie: [`estimate_history_bytes`] geteilt
/// durch die kalibrierte Rate (aufgerundet).
#[must_use]
pub fn estimate_history_tokens(
    history: &ConversationHistory,
    calibration: &TokenCalibration,
) -> u64 {
    calibration.bytes_to_tokens(estimate_history_bytes(history.items()))
}

/// Rein regelbasierter, modellfreier Verdichtungsschritt (siehe Moduldoku,
/// Abschnitt „Hybrid-Algorithmus", Punkt 1).
///
/// # Description
/// Arbeitet auf [`crate::history::ConversationHistory::atomic_groups`], damit
/// ein Tool-Call/Ergebnis-Paar nie getrennt wird. Der **aktuelle Turn**
/// (die letzte `UserMessage`-Gruppe und alles danach) bleibt unverändert;
/// nur der ältere Teil der Historie wird bearbeitet:
///
/// (r) Reasoning-Items fallen weg.
/// (a) Ein fehlgeschlagenes Tool-Ergebnis (`ToolCallResult::Error`), dessen
///     Aufruf später (in einer chronologisch nachfolgenden Gruppe) mit
///     identischem Tool-Namen und kanonisch-gleichen Argumenten wiederholt
///     wurde, fällt samt seinem Aufruf weg.
/// (b) Wiederholte, identische, erfolgreiche Aufrufe eines
///     schreibgeschützten Lesetools (`fs.read*`, `fs.list*`, `fs.search*`,
///     `fs.grep*`, `fs.glob*`) werden bis auf das letzte Vorkommen entfernt.
/// (c) Der verbleibende Inhalt alter Tool-Ergebnisse, die
///     `plan.tool_result_head_bytes` überschreiten, wird auf ihren
///     UTF-8-sicheren Kopf plus einen Kürzungs-Hinweis reduziert. Bei
///     `fs.read`-Ergebnissen enthält der Hinweis, wenn ermittelbar, den
///     gelesenen Pfad.
///
/// Angeheftete Zusammenfassungen ([`is_pinned_summary`]) bleiben unverändert.
///
/// # Arguments
/// - `history` (`&ConversationHistory`): die zu verdichtende Historie.
/// - `plan` (`&CompactionPlan`): liefert `tool_result_head_bytes`.
///
/// # Returns
/// Die verdichtete Historie sowie ein [`CompactionOutcome`] mit den
/// Zählern dieses Laufs (Token-, Zusammenfassungs- und Elisionsfelder
/// bleiben leer — die füllen [`compact_session`]/[`compact_for_budget`]).
#[must_use]
pub fn deterministic_pass(
    history: &ConversationHistory,
    plan: &CompactionPlan,
) -> (ConversationHistory, CompactionOutcome) {
    let bytes_before = total_bytes(history.items());

    let (mut older, current) = split_current_turn(history);

    let mut items_dropped = 0_usize;
    let mut calls_deduplicated = 0_usize;
    let mut results_truncated = 0_usize;

    drop_reasoning(&mut older, &mut items_dropped);
    drop_failed_retried_pairs(&mut older, &mut items_dropped);
    dedupe_read_only_calls(&mut older, &mut calls_deduplicated);
    truncate_large_results(
        &mut older,
        plan.tool_result_head_bytes,
        &mut results_truncated,
    );

    let items = flatten_groups(older.into_iter().chain(current));
    let bytes_after = total_bytes(&items);

    (
        ConversationHistory::from_items(items),
        CompactionOutcome {
            bytes_before,
            bytes_after,
            items_dropped,
            results_truncated,
            calls_deduplicated,
            ..CompactionOutcome::default()
        },
    )
}

/// Führt [`deterministic_pass`] aus und verdichtet, falls das allein nicht
/// unter `plan.target_history_bytes` kommt, zusätzlich die ältere Hälfte des
/// verbleibenden, nicht-aktuellen Teils per Modellaufruf (siehe Moduldoku,
/// Abschnitt „Hybrid-Algorithmus", Punkt 2).
///
/// # Arguments
/// - `session` (`&mut AgentSession`): deren Historie ersetzt wird
///   (`history_mut`) und deren [`CompactionObserver`] (falls gesetzt)
///   benachrichtigt wird.
/// - `model` (`&dyn ModelProvider`): für den optionalen
///   Zusammenfassungs-Aufruf (keine Tools, deutsche Systeminstruktion,
///   höchstens [`SUMMARY_MAX_OUTPUT_TOKENS`] Ausgabe-Tokens).
/// - `plan` (`&CompactionPlan`): Zielgröße, Kopf-Kappungsgrenze,
///   Zusammenfassungs-Modell und -Fenster.
/// - `reason` (`Option<CompactDecision>`): der Auslöser, ins
///   [`CompactionOutcome`] übernommen.
///
/// # Returns
/// `Ok(outcome)` — immer, auch wenn der Zusammenfassungs-Aufruf fehlschlägt
/// (dann bleibt das deterministische Ergebnis bestehen). Ändert der Lauf
/// nichts, ist `outcome.no_op` gesetzt und es wird kein Event gesendet.
///
/// # Errors
/// Dieser Aufruf schlägt derzeit nicht fehl: ein Fehler des Modells beim
/// Zusammenfassen wird abgefangen (`tracing::warn!`) statt propagiert. Der
/// `CoreResult`-Rückgabetyp bleibt für echte Fehlerpfade reserviert.
///
/// # Concurrency
/// `async`; hält während des optionalen `model.respond(..).await` keine
/// Locks auf `session`.
pub async fn compact_session(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    plan: &CompactionPlan,
    reason: Option<CompactDecision>,
) -> CoreResult<CompactionOutcome> {
    let calibration = *session.token_calibration();
    let items_before = session.history().len();
    let tokens_before = estimate_history_tokens(session.history(), &calibration);
    let (mut history, mut outcome) = deterministic_pass(session.history(), plan);
    let mut summary_usage = TokenUsage::default();

    if outcome.bytes_after > plan.target_history_bytes {
        let config = SummaryConfig {
            model: plan.summary_model.clone(),
            provider: plan.summary_provider.clone(),
            window_tokens: plan
                .summary_window_tokens
                .unwrap_or(DEFAULT_SUMMARY_WINDOW_TOKENS),
            bytes_per_token: calibration.bytes_per_token(),
        };
        if let Some((summarized, result)) = summarize_older(
            &history,
            SummarySpan::OlderHalf,
            &config,
            model,
            &mut summary_usage,
        )
        .await
        {
            history = summarized;
            outcome.summarized = true;
            outcome.summary_truncated = result.truncated;
            outcome.summary_text = Some(result.text);
        }
    }

    outcome.reason = reason;
    outcome.tokens_before = tokens_before;
    outcome.tokens_after = estimate_history_tokens(&history, &calibration);
    outcome.bytes_after = total_bytes(history.items());
    outcome.summary_usage = summary_usage;
    Ok(finish_compaction(
        session,
        Some(history),
        outcome,
        items_before,
    ))
}

/// Verdichtet die Historie der Session, bis der geschätzte Request unter
/// `budget.target_tokens` liegt (Pre-flight-/Notfall-Compaction, Welle 3).
///
/// # Description
/// Läuft in Stufen und hört auf, sobald die Schätzung (Historie über
/// [`estimate_history_tokens`] mit der Kalibrierung der Session, plus
/// `budget.overhead_tokens`) das Ziel erreicht:
/// 1. Liegt die Schätzung schon unter dem Ziel ⇒ No-op (keine Änderung, kein
///    Event).
/// 2. [`deterministic_pass`] auf älteren Turns (inkl. Entfernen alter
///    Reasoning-Items).
/// 3. Zusammenfassung **aller** älteren Turns in einem Modellaufruf; eine
///    vorhandene Zusammenfassung wird zusammengeführt. Nur, wenn es eine
///    `UserMessage` gibt (sonst ist kein aktueller Turn abgrenzbar).
/// 4. Bei [`CompactionScope::CurrentTurn`]: gestufte Elision älterer
///    Tool-Runden des aktuellen Turns (Kopf/Ende-Kürzung → Platzhalter →
///    Fortschrittsnotiz). Die auslösende `UserMessage`, die letzten
///    `keep_recent_rounds` Runden und die Call/Ergebnis-Paarung bleiben
///    erhalten.
///
/// Reichen alle Stufen nicht, liefert der Lauf trotzdem `Ok` mit
/// `tokens_after > target_tokens` — der Aufrufer entscheidet (z. B.
/// `ContextExhausted`).
///
/// # Arguments
/// - `session` (`&mut AgentSession`): deren Historie ersetzt wird.
/// - `model` (`&dyn ModelProvider`): für den optionalen
///   Zusammenfassungs-Aufruf.
/// - `target_tokens` (`u64`): Ziel für die geschätzten Request-Tokens
///   (Historie plus `overhead_tokens`).
///   Wirkungsbereich ist [`CompactionScope::default`] (Current-Turn-Elision,
///   2 Runden geschützt); das Zusammenfassungs-Modell kommt aus
///   [`AgentSession::compaction_summary_model`], sein Fenster aus der
///   Auto-Compact-Policy der Session (sonst
///   [`DEFAULT_SUMMARY_WINDOW_TOKENS`]). Weitere Optionen: siehe
///   [`compact_for_budget_with`].
/// - `overhead_tokens` (`u64`): geschätzte Tokens des Requests außerhalb der
///   Historie (System-Prompt, Fragmente, Werkzeugschemata; Teil C). Ohne
///   diesen Anteil hielt die Verdichtung eine Historie für passend, die
///   zusammen mit dem festen Rahmen das Fenster weiter sprengte. `0` ⇒ das
///   Ziel gilt für die Historie allein.
/// - `reason` (`Option<CompactDecision>`): der Auslöser. Bei
///   [`CompactDecision::Emergency`] gelten zusätzlich die Notdruck-Stufen
///   ([`CompactionBudget::emergency`]).
///
/// # Returns
/// Das [`CompactionOutcome`] dieses Laufs (`no_op`, `tokens_before`,
/// `tokens_after`, `summarized`, `elided_results`, `summary_usage`, …).
///
/// # Errors
/// Derzeit keine: Modellfehler der Zusammenfassung werden geloggt und
/// übersprungen.
///
/// # Concurrency
/// `async`; hält während des optionalen Modellaufrufs keine Locks.
pub async fn compact_for_budget(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    target_tokens: u64,
    overhead_tokens: u64,
    reason: Option<CompactDecision>,
) -> CoreResult<CompactionOutcome> {
    let summary_window = session
        .auto_compact()
        .map(crate::auto_compact::AutoCompactPolicy::context_window_tokens)
        .filter(|window| *window > 0);
    let budget = CompactionBudget::new(target_tokens)
        .with_summary_window_tokens(summary_window)
        .with_overhead_tokens(overhead_tokens)
        .with_emergency(matches!(reason, Some(CompactDecision::Emergency)));
    compact_for_budget_with(session, model, &budget, reason).await
}

/// Wie [`compact_for_budget`], aber mit vollständiger Konfiguration
/// ([`CompactionBudget`]: Overhead, Wirkungsbereich, Zusammenfassungs-Modell
/// und -Fenster).
///
/// # Errors
/// Derzeit keine (siehe [`compact_for_budget`]).
pub async fn compact_for_budget_with(
    session: &mut AgentSession,
    model: &dyn ModelProvider,
    budget: &CompactionBudget,
    reason: Option<CompactDecision>,
) -> CoreResult<CompactionOutcome> {
    let calibration = *session.token_calibration();
    let overhead = budget.overhead_tokens;
    let target = budget.target_tokens;
    let tokens_for_bytes = |bytes: u64| overhead.saturating_add(calibration.bytes_to_tokens(bytes));

    let items_before = session.history().len();
    let bytes_before = total_bytes(session.history().items());
    let tokens_before = tokens_for_bytes(estimate_history_bytes(session.history().items()));
    if tokens_before <= target {
        let outcome = CompactionOutcome {
            bytes_before,
            bytes_after: bytes_before,
            tokens_before,
            tokens_after: tokens_before,
            reason,
            ..CompactionOutcome::default()
        };
        return Ok(finish_compaction(session, None, outcome, items_before));
    }

    let plan = CompactionPlan {
        target_history_bytes: usize::MAX,
        tool_result_head_bytes: budget.tool_result_head_bytes,
        summary_model: None,
        summary_provider: None,
        summary_window_tokens: None,
    };
    let (mut history, mut outcome) = deterministic_pass(session.history(), &plan);
    let fits_items = |items: &[TurnItem]| tokens_for_bytes(estimate_history_bytes(items)) <= target;
    let mut summary_usage = TokenUsage::default();

    if !fits_items(history.items()) && has_user_message(history.items()) {
        let (summary_provider, summary_model) =
            if budget.summary_model.is_none() && budget.summary_provider.is_none() {
                let (provider, model_id) = session.compaction_summary_model();
                (provider.cloned(), model_id.cloned())
            } else {
                (
                    budget.summary_provider.clone(),
                    budget.summary_model.clone(),
                )
            };
        let config = SummaryConfig {
            model: summary_model,
            provider: summary_provider,
            window_tokens: budget
                .summary_window_tokens
                .unwrap_or(DEFAULT_SUMMARY_WINDOW_TOKENS),
            bytes_per_token: calibration.bytes_per_token(),
        };
        if let Some((summarized, result)) = summarize_older(
            &history,
            SummarySpan::AllOlder,
            &config,
            model,
            &mut summary_usage,
        )
        .await
        {
            history = summarized;
            outcome.summarized = true;
            outcome.summary_truncated = result.truncated;
            outcome.summary_text = Some(result.text);
        }
    }

    if let CompactionScope::CurrentTurn { keep_recent_rounds } = budget.scope
        && !fits_items(history.items())
    {
        let fits_groups = |groups: &[Vec<TurnItem>]| {
            let bytes = groups
                .iter()
                .flatten()
                .map(model_item_bytes)
                .fold(0_u64, u64::saturating_add);
            tokens_for_bytes(bytes) <= target
        };
        if let Some((elided, stats)) =
            elide_current_turn(&history, keep_recent_rounds, &fits_groups)
        {
            history = elided;
            outcome.elided_results = u32::try_from(stats.elided_results).unwrap_or(u32::MAX);
            outcome.progress_note = stats.progress_note;
        }
        // Teil C, Notdruck: (e1) übergroße Ergebnisse auch der geschützten
        // jüngsten Runden auf Kopf/Ende kürzen, (e2) danach keine Runde mehr
        // schützen. Die Call/Ergebnis-Paarung und die auslösende
        // `UserMessage` bleiben in beiden Stufen erhalten.
        if budget.emergency && !fits_items(history.items()) {
            let (trimmed, count) =
                trim_current_turn_results(&history, EMERGENCY_RECENT_RESULT_BYTES);
            if count > 0 {
                history = trimmed;
                outcome.elided_results = outcome
                    .elided_results
                    .saturating_add(u32::try_from(count).unwrap_or(u32::MAX));
            }
            if !fits_items(history.items())
                && keep_recent_rounds > 0
                && let Some((elided, stats)) = elide_current_turn(&history, 0, &fits_groups)
            {
                history = elided;
                outcome.elided_results = outcome
                    .elided_results
                    .saturating_add(u32::try_from(stats.elided_results).unwrap_or(u32::MAX));
                outcome.progress_note |= stats.progress_note;
            }
        }
    }

    outcome.reason = reason;
    outcome.bytes_before = bytes_before;
    outcome.bytes_after = total_bytes(history.items());
    outcome.tokens_before = tokens_before;
    outcome.tokens_after = tokens_for_bytes(estimate_history_bytes(history.items()));
    outcome.summary_usage = summary_usage;
    Ok(finish_compaction(
        session,
        Some(history),
        outcome,
        items_before,
    ))
}

/// Gemeinsamer Abschluss beider Einstiegspunkte: meldet die
/// Zusammenfassungs-Nutzung, setzt `no_op` und — nur bei einer echten
/// Änderung — ersetzt die Historie, sendet `CompactionApplied` und
/// benachrichtigt den Observer (dieser auch beim No-op).
fn finish_compaction(
    session: &mut AgentSession,
    history: Option<ConversationHistory>,
    mut outcome: CompactionOutcome,
    items_before: usize,
) -> CompactionOutcome {
    // Kosten des Zusammenfassungs-Aufrufs immer melden — auch wenn sein
    // Ergebnis verworfen wurde, hat er Tokens verbraucht.
    if outcome.summary_usage != TokenUsage::default() {
        session.publish_agent_event(crate::agent_events::AgentEventKind::InternalUsage {
            purpose: "compaction".to_owned(),
            usage: outcome.summary_usage.clone(),
        });
    }

    outcome.no_op = outcome.changed_nothing();
    let history = match history {
        Some(history) if !outcome.no_op => history,
        _ => {
            outcome.no_op = true;
            tracing::debug!(
                tokens_before = outcome.tokens_before,
                "session compaction changed nothing (no-op)"
            );
            // Der Observer erfährt auch vom No-op (er sieht `no_op = true`);
            // Historie und Event bleiben unberührt.
            if let Some(observer) = session.compaction_observer() {
                observer.on_compacted(session.id(), &outcome);
            }
            return outcome;
        }
    };
    *session.history_mut() = history;

    let reason = outcome.reason;
    session
        .live_emitter()
        .emit(harw_protocol::TurnEvent::CompactionApplied {
            turn_id: session.current_turn().cloned(),
            reason: reason.map_or_else(|| "manual".to_owned(), |r| format!("{r:?}")),
            items_before: u32::try_from(items_before).unwrap_or(u32::MAX),
            items_after: u32::try_from(session.history().len()).unwrap_or(u32::MAX),
            tokens_before: Some(outcome.tokens_before),
            tokens_after: Some(outcome.tokens_after),
            summarized: outcome.summarized,
            elided_results: outcome.elided_results,
        });

    if let Some(observer) = session.compaction_observer() {
        observer.on_compacted(session.id(), &outcome);
    }

    tracing::info!(
        bytes_before = outcome.bytes_before,
        bytes_after = outcome.bytes_after,
        tokens_before = outcome.tokens_before,
        tokens_after = outcome.tokens_after,
        items_dropped = outcome.items_dropped,
        results_truncated = outcome.results_truncated,
        calls_deduplicated = outcome.calls_deduplicated,
        elided_results = outcome.elided_results,
        progress_note = outcome.progress_note,
        summarized = outcome.summarized,
        summary_truncated = outcome.summary_truncated,
        "session compaction complete"
    );

    outcome
}

// ---------------------------------------------------------------------
// Interne Hilfsfunktionen
// ---------------------------------------------------------------------

/// Geschätzte Serialisierungsgröße eines Items in Bytes — dieselbe Schätzung
/// wie `harw_core::history`s privates `item_bytes` (`serde_json`-Länge,
/// `usize::MAX` bei Serialisierungsfehler). Reasoning-Items zählen `0`: sie
/// sind kein Modell-Input.
fn item_bytes(item: &TurnItem) -> usize {
    if matches!(item, TurnItem::Reasoning(_)) {
        return 0;
    }
    serde_json::to_vec(item).map_or(usize::MAX, |bytes| bytes.len())
}

/// Summe der [`item_bytes`] über eine Item-Liste (Reasoning ausgenommen).
///
/// `pub(crate)`, damit `turn_loop` daraus die geschätzte Verlaufs-Token-Zahl
/// ableiten kann, ohne die Schätzung ein zweites Mal zu implementieren.
pub(crate) fn total_bytes(items: &[TurnItem]) -> usize {
    items
        .iter()
        .map(item_bytes)
        .fold(0_usize, usize::saturating_add)
}

/// Schätzt die Token-Zahl der gesamten Historie aus ihrer Byte-Größe (4
/// Bytes/Token, dieselbe Faustregel wie [`CompactionPlan::for_context_window`]
/// und [`CompactionPlan::for_target_tokens`]); Reasoning zählt nicht.
/// Für kalibrierte Schätzungen siehe [`estimate_history_tokens`].
///
/// # Arguments
/// - `history` (`&ConversationHistory`): die zu schätzende Historie.
///
/// # Returns
/// Geschätzte Token-Zahl, saturierend berechnet (kein Überlauf).
#[must_use]
pub(crate) fn estimated_history_tokens(history: &ConversationHistory) -> u64 {
    let bytes = total_bytes(history.items());
    u64::try_from(bytes).unwrap_or(u64::MAX) / 4
}

/// Länge der kompakten `serde_json`-Serialisierung in Bytes (`0` bei einem
/// praktisch unmöglichen Serialisierungsfehler).
fn json_len<T: serde::Serialize + ?Sized>(value: &T) -> u64 {
    serde_json::to_vec(value).map_or(0, |bytes| bytes.len() as u64)
}

/// Nutzlast-Bytes von Nachrichteninhalt; Bilder zählen wie ihr
/// `[image]`-Platzhalter in `to_model_messages`.
fn content_bytes(parts: &[ContentPart]) -> u64 {
    parts
        .iter()
        .map(|part| match part {
            ContentPart::Text { text } => text.len() as u64,
            ContentPart::ImageUrl { .. } => "[image]".len() as u64,
        })
        .fold(0_u64, u64::saturating_add)
}

/// Nutzlast-Bytes eines Items, wie sie [`estimate_history_bytes`] zählt.
fn model_item_bytes(item: &TurnItem) -> u64 {
    let payload = match item {
        TurnItem::UserMessage(message) => content_bytes(&message.content),
        TurnItem::AssistantMessage(message) => content_bytes(&message.content),
        TurnItem::ToolCall(call) => (call.call_id.as_str().len() as u64)
            .saturating_add(call.tool_name.len() as u64)
            .saturating_add(json_len(&call.arguments)),
        TurnItem::ToolResult(result) => {
            (result.call_id.as_str().len() as u64).saturating_add(json_len(&result.result))
        }
        TurnItem::Reasoning(_) | TurnItem::Error(_) => return 0,
    };
    payload.saturating_add(MESSAGE_FRAMING_BYTES)
}

/// Hängt Gruppen wieder zu einer flachen Item-Liste zusammen.
fn flatten_groups(groups: impl IntoIterator<Item = Vec<TurnItem>>) -> Vec<TurnItem> {
    groups.into_iter().flatten().collect()
}

/// `true`, wenn die Gruppe genau eine `UserMessage` ist.
fn is_user_group(group: &[TurnItem]) -> bool {
    matches!(group, [TurnItem::UserMessage(_)])
}

/// `true`, wenn `items` mindestens eine `UserMessage` enthält.
fn has_user_message(items: &[TurnItem]) -> bool {
    items
        .iter()
        .any(|item| matches!(item, TurnItem::UserMessage(_)))
}

/// Eigene Kopien der atomaren Gruppen von `history`.
fn owned_groups(history: &ConversationHistory) -> Vec<Vec<TurnItem>> {
    history
        .atomic_groups()
        .into_iter()
        .map(|group| group.into_iter().cloned().collect())
        .collect()
}

/// Zerlegt die Historie in atomare Gruppen (eigene Kopien, siehe
/// [`crate::history::ConversationHistory::atomic_groups`]) und trennt sie an
/// der letzten `UserMessage`-Gruppe: alles ab dort (einschließlich) ist der
/// aktuelle Turn und wird von [`deterministic_pass`] nie verändert. Fehlt
/// eine `UserMessage`, gilt die gesamte Historie als „älter" (kein aktueller
/// Turn abgrenzbar).
fn split_current_turn(history: &ConversationHistory) -> (Vec<Vec<TurnItem>>, Vec<Vec<TurnItem>>) {
    let mut groups = owned_groups(history);
    let user_idx = groups.iter().rposition(|group| is_user_group(group));
    match user_idx {
        Some(idx) => {
            let current = groups.split_off(idx);
            (groups, current)
        }
        None => (groups, Vec::new()),
    }
}

/// Schritt (r): entfernt alle Reasoning-Items aus den älteren Gruppen.
fn drop_reasoning(groups: &mut Vec<Vec<TurnItem>>, items_dropped: &mut usize) {
    for group in groups.iter_mut() {
        let before = group.len();
        group.retain(|item| !matches!(item, TurnItem::Reasoning(_)));
        *items_dropped += before - group.len();
    }
    groups.retain(|group| !group.is_empty());
}

/// Kanonisiert einen JSON-Wert für den Vergleich: Objekt-Schlüssel werden
/// rekursiv sortiert, Array-Reihenfolge bleibt erhalten. Grundlage für den
/// „kanonisch-gleiche Argumente"-Vergleich in Schritt (a)/(b).
fn canonical_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(&String, &serde_json::Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            let mut out = serde_json::Map::new();
            for (key, val) in entries {
                out.insert(key.clone(), canonical_json(val));
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonical_json).collect())
        }
        other => other.clone(),
    }
}

/// Ein Tool-Call/Ergebnis-Paar innerhalb der älteren Gruppen, mit genug
/// Metadaten für die Schritte (a) und (b), ohne die Items selbst zu klonen.
struct PairRef {
    group_index: usize,
    call_item_index: usize,
    result_item_index: usize,
    tool_name: String,
    canonical_args: serde_json::Value,
    is_error: bool,
}

/// Sammelt alle vollständigen Call/Ergebnis-Paare aus `groups`, in
/// chronologischer Reihenfolge (Gruppen-Index, dann Position innerhalb der
/// Gruppe). Ein Call ohne passendes Ergebnis in derselben Gruppe wird
/// übersprungen (kann in einer abgeschlossenen, älteren Runde nicht
/// vorkommen, siehe `group_indices` in `history.rs`).
fn collect_pairs(groups: &[Vec<TurnItem>]) -> Vec<PairRef> {
    let mut pairs = Vec::new();
    for (group_index, group) in groups.iter().enumerate() {
        for (call_item_index, item) in group.iter().enumerate() {
            let TurnItem::ToolCall(call) = item else {
                continue;
            };
            let Some(result_item_index) = group.iter().position(|candidate| {
                matches!(candidate, TurnItem::ToolResult(result) if result.call_id == call.call_id)
            }) else {
                continue;
            };
            let TurnItem::ToolResult(result_item) = &group[result_item_index] else {
                continue;
            };
            pairs.push(PairRef {
                group_index,
                call_item_index,
                result_item_index,
                tool_name: call.tool_name.clone(),
                canonical_args: canonical_json(&call.arguments),
                is_error: matches!(result_item.result, ToolCallResult::Error { .. }),
            });
        }
    }
    pairs
}

/// Entfernt die in `positions` benannten Items (`(group_index, item_index)`)
/// aus `groups`, entfernt danach leer gewordene Gruppen und zählt jedes
/// entfernte Item in `dropped`.
fn remove_positions(
    groups: &mut Vec<Vec<TurnItem>>,
    positions: &BTreeSet<(usize, usize)>,
    dropped: &mut usize,
) {
    if positions.is_empty() {
        return;
    }
    for (group_index, group) in groups.iter_mut().enumerate() {
        let mut item_index = 0_usize;
        group.retain(|_| {
            let keep = !positions.contains(&(group_index, item_index));
            if !keep {
                *dropped += 1;
            }
            item_index += 1;
            keep
        });
    }
    groups.retain(|group| !group.is_empty());
}

/// Schritt (a): fehlgeschlagene Tool-Aufrufe fallen weg, wenn eine
/// chronologisch spätere Gruppe denselben Tool-Namen mit kanonisch-gleichen
/// Argumenten erneut aufruft — die Wiederholung war der eigentliche Versuch,
/// das fehlgeschlagene Paar trägt keine Information mehr.
fn drop_failed_retried_pairs(groups: &mut Vec<Vec<TurnItem>>, items_dropped: &mut usize) {
    let pairs = collect_pairs(groups);
    let mut to_remove: BTreeSet<(usize, usize)> = BTreeSet::new();

    for pair in &pairs {
        if !pair.is_error {
            continue;
        }
        let retried = pairs.iter().any(|other| {
            other.group_index > pair.group_index
                && other.tool_name == pair.tool_name
                && other.canonical_args == pair.canonical_args
        });
        if retried {
            to_remove.insert((pair.group_index, pair.call_item_index));
            to_remove.insert((pair.group_index, pair.result_item_index));
        }
    }

    remove_positions(groups, &to_remove, items_dropped);
}

/// `true`, wenn `tool_name` einem der schreibgeschützten Lesetool-Präfixe aus
/// dem Vertrag entspricht (`fs.read`, `fs.list`, `fs.search`, `fs.grep`,
/// `fs.glob`, `doc.read_pdf`).
fn is_read_only_tool(tool_name: &str) -> bool {
    const PREFIXES: [&str; 6] = [
        "fs.read",
        "fs.list",
        "fs.search",
        "fs.grep",
        "fs.glob",
        "doc.read_pdf",
    ];
    PREFIXES.iter().any(|prefix| tool_name.starts_with(prefix))
}

/// Schritt (b): wiederholte, identische, erfolgreiche Aufrufe eines
/// schreibgeschützten Lesetools werden bis auf das letzte Vorkommen entfernt
/// — ältere Duplikate liefern denselben Inhalt wie das letzte, tragen aber
/// keine zusätzliche Information.
fn dedupe_read_only_calls(groups: &mut Vec<Vec<TurnItem>>, calls_deduplicated: &mut usize) {
    let pairs = collect_pairs(groups);
    let mut to_remove: BTreeSet<(usize, usize)> = BTreeSet::new();

    for (index, pair) in pairs.iter().enumerate() {
        if pair.is_error || !is_read_only_tool(&pair.tool_name) {
            continue;
        }
        let has_later_duplicate = pairs.iter().skip(index + 1).any(|other| {
            !other.is_error
                && is_read_only_tool(&other.tool_name)
                && other.tool_name == pair.tool_name
                && other.canonical_args == pair.canonical_args
        });
        if has_later_duplicate {
            to_remove.insert((pair.group_index, pair.call_item_index));
            to_remove.insert((pair.group_index, pair.result_item_index));
        }
    }

    let mut dropped_unused = 0_usize;
    remove_positions(groups, &to_remove, &mut dropped_unused);
    *calls_deduplicated += to_remove.len() / 2;
}

/// Reintext-Sicht auf ein `ToolCallResult` — eigene, kleine Kopie von
/// `history.rs`s privater `tool_result_text`, weil das Original modul-privat
/// ist.
fn tool_result_text(result: &ToolCallResult) -> String {
    match result {
        ToolCallResult::Success {
            value: serde_json::Value::String(text),
        } => text.clone(),
        ToolCallResult::Success { value } => serde_json::to_string(value).unwrap_or_default(),
        ToolCallResult::Error { message } => message.clone(),
    }
}

/// Ersetzt den Inhalt eines `ToolCallResult` durch `text`, unter Beibehaltung
/// der `Success`/`Error`-Variante.
fn tool_result_with_text(result: &ToolCallResult, text: String) -> ToolCallResult {
    match result {
        ToolCallResult::Success { .. } => ToolCallResult::Success {
            value: serde_json::Value::String(text),
        },
        ToolCallResult::Error { .. } => ToolCallResult::Error { message: text },
    }
}

/// Deutscher Kürzungs-Hinweis; enthält bei `fs.read`-Ergebnissen den Pfad,
/// falls im Aufruf-Argument `path` ermittelbar.
fn truncation_marker(elided_bytes: usize, path_hint: Option<&str>) -> String {
    match path_hint {
        Some(path) => {
            format!("\n[gekürzt: {elided_bytes} Bytes ({path}) — bei Bedarf erneut lesen]")
        }
        None => format!("\n[gekürzt: {elided_bytes} Bytes — bei Bedarf erneut lesen]"),
    }
}

/// Schritt (c): Inhalt alter Tool-Ergebnisse, die `head_bytes` überschreiten,
/// wird auf ihren UTF-8-sicheren Kopf plus [`truncation_marker`] reduziert.
fn truncate_large_results(
    groups: &mut [Vec<TurnItem>],
    head_bytes: usize,
    results_truncated: &mut usize,
) {
    let mut call_meta: HashMap<ToolCallId, (String, serde_json::Value)> = HashMap::new();
    for group in groups.iter() {
        for item in group {
            if let TurnItem::ToolCall(call) = item {
                call_meta.insert(
                    call.call_id.clone(),
                    (call.tool_name.clone(), call.arguments.clone()),
                );
            }
        }
    }

    for group in groups.iter_mut() {
        for item in group.iter_mut() {
            let TurnItem::ToolResult(result_item) = item else {
                continue;
            };
            let text = tool_result_text(&result_item.result);
            if text.len() <= head_bytes {
                continue;
            }
            let elided = text.len() - head_bytes;
            let head_end = floor_char_boundary(&text, head_bytes);
            let path_hint = call_meta
                .get(&result_item.call_id)
                .and_then(|(name, args)| {
                    // `fs.read` und `doc.read_pdf` tragen beide `{"path": …}`
                    // als Argumentform — deshalb derselbe Pfad-Hinweis.
                    if name.starts_with("fs.read") || name.starts_with("doc.read_pdf") {
                        args.get("path")
                            .and_then(|value| value.as_str())
                            .map(str::to_owned)
                    } else {
                        None
                    }
                });
            let marker = truncation_marker(elided, path_hint.as_deref());

            let mut truncated = String::with_capacity(head_end + marker.len());
            truncated.push_str(&text[..head_end]);
            truncated.push_str(&marker);

            result_item.result = tool_result_with_text(&result_item.result, truncated);
            *results_truncated += 1;
        }
    }
}

/// Kleinster Index `>= index`, der auf einer UTF-8-Zeichengrenze liegt
/// (höchstens `value.len()`).
fn ceil_char_boundary(value: &str, index: usize) -> usize {
    let mut i = index.min(value.len());
    while i < value.len() && !value.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// Kürzt `text` auf höchstens etwa `max_bytes`, indem die Mitte durch einen
/// Auslassungs-Hinweis ersetzt wird. `head_percent` bestimmt den Anteil des
/// Kopfes am behaltenen Rest (0..=100). UTF-8-sicher.
pub(crate) fn elide_middle(text: &str, max_bytes: usize, head_percent: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    // Platz für den Hinweis selbst freihalten.
    let keep = max_bytes.saturating_sub(64);
    let head_len = keep.saturating_mul(head_percent.min(100)) / 100;
    let tail_len = keep - head_len;
    let head_end = floor_char_boundary(text, head_len);
    let tail_start = ceil_char_boundary(text, text.len().saturating_sub(tail_len)).max(head_end);
    let elided = tail_start - head_end;
    format!(
        "{}\n[… {elided} Bytes ausgelassen …]\n{}",
        &text[..head_end],
        &text[tail_start..]
    )
}

/// Kürzt `text` auf seinen Kopf von höchstens `max_bytes` Bytes plus `…`.
fn shorten(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    let end = floor_char_boundary(text, max_bytes);
    format!("{}…", &text[..end])
}

/// Reintext-Rendering eines Items für den Zusammenfassungs-Prompt;
/// Tool-Ergebnisse werden auf [`SUMMARY_RENDER_RESULT_MAX_BYTES`] gekappt.
fn render_item(out: &mut String, item: &TurnItem) {
    match item {
        TurnItem::UserMessage(message) => {
            out.push_str("User: ");
            out.push_str(&render_content(&message.content));
            out.push('\n');
        }
        TurnItem::AssistantMessage(message) => {
            out.push_str("Assistant: ");
            out.push_str(&render_content(&message.content));
            out.push('\n');
        }
        TurnItem::ToolCall(call) => {
            out.push_str("Tool-Call ");
            out.push_str(&call.tool_name);
            out.push('(');
            out.push_str(&shorten(
                &call.arguments.to_string(),
                SUMMARY_RENDER_RESULT_MAX_BYTES,
            ));
            out.push_str(")\n");
        }
        TurnItem::ToolResult(result) => {
            let status = if result.result.is_success() {
                "ok"
            } else {
                "error"
            };
            out.push_str("Tool-Result [");
            out.push_str(status);
            out.push_str("]: ");
            out.push_str(&elide_middle(
                &tool_result_text(&result.result),
                SUMMARY_RENDER_RESULT_MAX_BYTES,
                50,
            ));
            out.push('\n');
        }
        TurnItem::Error(error) => {
            out.push_str("Error: ");
            out.push_str(&error.message);
            out.push('\n');
        }
        TurnItem::Reasoning(_) => {}
    }
}

/// Reintext-Sicht auf `ContentPart`s — Bilder werden zu `[image]`.
fn render_content(parts: &[ContentPart]) -> String {
    let mut buf = String::new();
    for part in parts {
        match part {
            ContentPart::Text { text } => buf.push_str(text),
            ContentPart::ImageUrl { .. } => buf.push_str("[image]"),
        }
    }
    buf
}

/// Entfernt führende (auch mehrfach verschachtelte) [`SUMMARY_MARKER`] —
/// Grundlage dafür, dass Zusammenfassungen nie ineinander geschachtelt
/// werden.
fn strip_summary_markers(text: &str) -> &str {
    let marker = SUMMARY_MARKER.trim_end();
    let mut rest = text.trim_start();
    while let Some(stripped) = rest.strip_prefix(marker) {
        rest = stripped.trim_start();
    }
    rest
}

/// Baut das angeheftete Zusammenfassungs-Item.
fn summary_item(text: &str) -> TurnItem {
    TurnItem::AssistantMessage(AssistantMessageItem {
        id: ItemId::new(),
        content: vec![ContentPart::Text {
            text: format!("{SUMMARY_MARKER}{text}"),
        }],
        phase: None,
    })
}

/// Einstellungen eines Zusammenfassungs-Aufrufs.
struct SummaryConfig {
    model: Option<ModelId>,
    provider: Option<ProviderId>,
    /// Kontextfenster des Zusammenfassungs-Modells in Tokens.
    window_tokens: u64,
    /// Kalibrierte Rate für die Umrechnung des Fensters in Bytes.
    bytes_per_token: f64,
}

impl SummaryConfig {
    /// Eingabe-Budget in Bytes: 90 % von (Fenster − Ausgabe − Prompt-Rahmen),
    /// mindestens [`MIN_SUMMARY_INPUT_BYTES`].
    fn input_budget_bytes(&self) -> usize {
        let reserved = u64::from(SUMMARY_MAX_OUTPUT_TOKENS) + SUMMARY_PROMPT_OVERHEAD_TOKENS;
        let tokens = self
            .window_tokens
            .saturating_sub(reserved)
            .saturating_mul(90)
            / 100;
        let rate = if self.bytes_per_token.is_finite() && self.bytes_per_token > 0.0 {
            self.bytes_per_token
        } else {
            crate::context_budget::DEFAULT_BYTES_PER_TOKEN
        };
        // `as` sättigt bei f64 → usize statt überzulaufen.
        let bytes = (tokens as f64 * rate) as usize;
        bytes.max(MIN_SUMMARY_INPUT_BYTES)
    }
}

/// Welcher Anteil der älteren Turns zusammengefasst wird.
#[derive(Clone, Copy)]
enum SummarySpan {
    /// Die ältere Hälfte der (nicht angehefteten) älteren Gruppen.
    OlderHalf,
    /// Alle älteren Gruppen.
    AllOlder,
}

/// Ergebnis eines erfolgreichen Zusammenfassungs-Aufrufs.
struct SummaryResult {
    /// Zusammenfassungstext ohne [`SUMMARY_MARKER`].
    text: String,
    /// `true`, wenn die Ausgabe am Ausgabelimit abgeschnitten wurde.
    truncated: bool,
}

/// Trennt angeheftete Zusammenfassungen aus `groups` heraus: liefert deren
/// Texte (ohne Marker) und die übrigen Gruppen in ursprünglicher Reihenfolge.
fn partition_pinned(groups: Vec<Vec<TurnItem>>) -> (Vec<String>, Vec<Vec<TurnItem>>) {
    let mut prior = Vec::new();
    let mut rest = Vec::with_capacity(groups.len());
    for group in groups {
        if let [item] = group.as_slice()
            && is_pinned_summary(item)
            && let TurnItem::AssistantMessage(message) = item
        {
            let text = render_content(&message.content);
            let body = strip_summary_markers(&text).trim();
            if !body.is_empty() {
                prior.push(body.to_owned());
            }
            continue;
        }
        rest.push(group);
    }
    (prior, rest)
}

/// Fasst den gewählten Anteil der älteren Turns von `history` per
/// Modellaufruf zusammen; vorhandene angeheftete Zusammenfassungen werden
/// dabei zusammengeführt und durch genau eine neue ersetzt.
///
/// # Returns
/// `None`, wenn es nichts zusammenzufassen gibt oder der Modellaufruf
/// fehlschlägt bzw. unbrauchbar antwortet — dann bleibt `history` des
/// Aufrufers bestehen. Sonst die neue Historie (Zusammenfassung zuerst, dann
/// die behaltenen älteren Gruppen, dann der aktuelle Turn).
async fn summarize_older(
    history: &ConversationHistory,
    span: SummarySpan,
    config: &SummaryConfig,
    model: &dyn ModelProvider,
    usage_out: &mut TokenUsage,
) -> Option<(ConversationHistory, SummaryResult)> {
    let (older, current) = split_current_turn(history);
    if older.is_empty() {
        return None;
    }
    let (prior, mut rest) = partition_pinned(older);
    if rest.is_empty() {
        return None;
    }
    let keep_older = match span {
        SummarySpan::OlderHalf => {
            let split = rest.len().div_ceil(2);
            rest.split_off(split)
        }
        SummarySpan::AllOlder => Vec::new(),
    };

    let result = run_summary(&prior, &rest, config, model, usage_out).await?;

    let items = flatten_groups(
        std::iter::once(vec![summary_item(&result.text)])
            .chain(keep_older)
            .chain(current),
    );
    Some((ConversationHistory::from_items(items), result))
}

/// Baut den (auf das Fenster des Zusammenfassungs-Modells gekappten)
/// Prompt, ruft das Modell mit höchstens [`SUMMARY_MAX_OUTPUT_TOKENS`]
/// Ausgabe-Tokens auf und prüft den Stop-Grund.
async fn run_summary(
    prior: &[String],
    groups: &[Vec<TurnItem>],
    config: &SummaryConfig,
    model: &dyn ModelProvider,
    usage_out: &mut TokenUsage,
) -> Option<SummaryResult> {
    let mut transcript = String::new();
    for item in groups.iter().flatten() {
        render_item(&mut transcript, item);
    }
    if transcript.trim().is_empty() {
        return None;
    }

    let budget = config.input_budget_bytes();
    let prompt = if prior.is_empty() {
        // Jüngere Anteile sind wertvoller: zwei Drittel des Budgets fürs Ende.
        elide_middle(&transcript, budget, 34)
    } else {
        let prior_text = elide_middle(&prior.join("\n\n"), budget / 3, 50);
        let transcript_budget = budget.saturating_sub(prior_text.len()).max(budget / 2);
        let transcript = elide_middle(&transcript, transcript_budget, 34);
        format!(
            "Bisherige Zusammenfassung:\n{prior_text}\n\nNeuer Verlauf seit dieser \
             Zusammenfassung:\n{transcript}"
        )
    };

    let mut request_history = ConversationHistory::new();
    request_history.push_user_text(prompt);
    let instructions = LoadedInstructions {
        system_prompt: SUMMARY_INSTRUCTION.to_owned(),
        fragments: Vec::new(),
    };
    let request = ModelRequest::new(instructions, Vec::new(), request_history, Vec::new())
        .with_model_id(config.model.clone())
        .with_provider_id(config.provider.clone())
        .with_max_output_tokens(Some(SUMMARY_MAX_OUTPUT_TOKENS));

    let response = match model.respond(request).await {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(error = %error, "compaction summary model call failed; keeping deterministic result");
            return None;
        }
    };
    usage_out.add(&response.usage);

    let truncated = match &response.stop {
        StopReason::MaxTokens => true,
        StopReason::Refusal { .. }
        | StopReason::ContentFilter
        | StopReason::ContextWindowExceeded => {
            tracing::warn!(
                stop = ?response.stop,
                "compaction summary call stopped without usable summary; keeping deterministic result"
            );
            return None;
        }
        _ => false,
    };

    let Some(text) = response.message.as_deref() else {
        tracing::warn!("compaction summary call returned no text; keeping deterministic result");
        return None;
    };
    let body = strip_summary_markers(text).trim();
    if body.is_empty() {
        tracing::warn!(
            "compaction summary call returned no usable text; keeping deterministic result"
        );
        return None;
    }
    if truncated {
        tracing::warn!("compaction summary hit the output limit; keeping it marked as truncated");
    }
    let text = if truncated {
        format!("{body}{SUMMARY_TRUNCATED_NOTE}")
    } else {
        body.to_owned()
    };
    Some(SummaryResult { text, truncated })
}

// ---------------------------------------------------------------------
// Current-Turn-Elision (Stufe 4 von `compact_for_budget`)
// ---------------------------------------------------------------------

/// Zähler der Current-Turn-Elision.
struct ElisionStats {
    /// Anzahl betroffener Tool-Ergebnisse (gekürzt, ersetzt oder in die
    /// Fortschrittsnotiz überführt).
    elided_results: usize,
    /// `true`, wenn Runden durch eine Fortschrittsnotiz ersetzt wurden.
    progress_note: bool,
}

/// Kurzer, menschenlesbarer Ziel-Hinweis eines Tool-Aufrufs (Pfad, URL,
/// Kommando, Suchmuster …) aus seinen Argumenten.
fn call_target(arguments: &serde_json::Value) -> String {
    const KEYS: [&str; 10] = [
        "path",
        "file_path",
        "file",
        "url",
        "command",
        "cmd",
        "query",
        "pattern",
        "name",
        "id",
    ];
    for key in KEYS {
        if let Some(value) = arguments.get(key).and_then(serde_json::Value::as_str) {
            return shorten(value, 120);
        }
    }
    match arguments {
        serde_json::Value::Null => "—".to_owned(),
        serde_json::Value::Object(map) if map.is_empty() => "—".to_owned(),
        other => shorten(&other.to_string(), 80),
    }
}

/// Platzhalter der Elisionsstufe (b).
fn elision_placeholder(original_bytes: usize, tool: &str, target: &str) -> String {
    format!("[ausgelassen: {original_bytes} Bytes, {tool}, {target}; erneut ausführen für Details]")
}

/// `true`, wenn `text` bereits ein Platzhalter der Stufe (b) ist.
fn is_elision_placeholder(text: &str) -> bool {
    text.starts_with("[ausgelassen: ")
}

/// Gestufte Elision älterer Tool-Runden des aktuellen Turns (siehe
/// [`compact_for_budget`]).
///
/// # Description
/// Der aktuelle Turn beginnt nach der letzten `UserMessage` (fehlt sie, ist
/// die gesamte Historie der aktuelle Turn). Eine „Runde" ist eine atomare
/// Gruppe mit mindestens einem `ToolCall`. Die letzten `keep_recent_rounds`
/// Runden und alles ab der ersten davon bleiben unverändert; die älteren
/// Runden (Kandidaten) werden — älteste zuerst, nach jeder Runde gegen
/// `fits` geprüft — (a) auf Kopf/Ende gekürzt, (b) durch Platzhalter
/// ersetzt und (c) samt aller Gruppen zwischen `UserMessage` und erster
/// geschützter Runde durch eine Fortschrittsnotiz ersetzt. Gruppen werden
/// nur als Ganzes entfernt, Call/Ergebnis-Paare bleiben also gepaart.
///
/// # Returns
/// `None`, wenn es keine Kandidaten gibt (nichts verändert); sonst die neue
/// Historie und die Zähler.
fn elide_current_turn(
    history: &ConversationHistory,
    keep_recent_rounds: usize,
    fits: &dyn Fn(&[Vec<TurnItem>]) -> bool,
) -> Option<(ConversationHistory, ElisionStats)> {
    let mut groups = owned_groups(history);
    let first_current = groups
        .iter()
        .rposition(|group| is_user_group(group))
        .map_or(0, |idx| idx + 1);
    let rounds: Vec<usize> = (first_current..groups.len())
        .filter(|&idx| {
            groups[idx]
                .iter()
                .any(|item| matches!(item, TurnItem::ToolCall(_)))
        })
        .collect();
    if rounds.len() <= keep_recent_rounds {
        return None;
    }
    let candidate_count = rounds.len() - keep_recent_rounds;
    let candidates = &rounds[..candidate_count];
    let protected_from = rounds.get(candidate_count).copied().unwrap_or(groups.len());

    // Metadaten der Kandidaten-Aufrufe und Originalgrößen der Ergebnisse,
    // bevor Stufe (a) sie verändert.
    let mut call_meta: HashMap<ToolCallId, (String, String)> = HashMap::new();
    let mut original_sizes: HashMap<ToolCallId, usize> = HashMap::new();
    for &idx in candidates {
        for item in &groups[idx] {
            match item {
                TurnItem::ToolCall(call) => {
                    call_meta.insert(
                        call.call_id.clone(),
                        (call.tool_name.clone(), call_target(&call.arguments)),
                    );
                }
                TurnItem::ToolResult(result) => {
                    original_sizes.insert(
                        result.call_id.clone(),
                        tool_result_text(&result.result).len(),
                    );
                }
                _ => {}
            }
        }
    }

    let mut affected: HashSet<ToolCallId> = HashSet::new();
    let finish = |groups: Vec<Vec<TurnItem>>,
                  affected: &HashSet<ToolCallId>,
                  note: bool|
     -> (ConversationHistory, ElisionStats) {
        (
            ConversationHistory::from_items(flatten_groups(groups)),
            ElisionStats {
                elided_results: affected.len(),
                progress_note: note,
            },
        )
    };

    // Stufe (a): große Ergebnisse auf Kopf/Ende kürzen.
    for &idx in candidates {
        for item in &mut groups[idx] {
            let TurnItem::ToolResult(result) = item else {
                continue;
            };
            let text = tool_result_text(&result.result);
            if text.len() <= ELISION_HEAD_TAIL_BYTES {
                continue;
            }
            let cut = elide_middle(&text, ELISION_HEAD_TAIL_BYTES, 50);
            result.result = tool_result_with_text(&result.result, cut);
            affected.insert(result.call_id.clone());
        }
        if fits(groups.as_slice()) {
            return Some(finish(groups, &affected, false));
        }
    }

    // Stufe (b): Ergebnisse durch Platzhalter ersetzen.
    for &idx in candidates {
        for item in &mut groups[idx] {
            let TurnItem::ToolResult(result) = item else {
                continue;
            };
            let text = tool_result_text(&result.result);
            if is_elision_placeholder(&text) {
                continue;
            }
            let original = original_sizes
                .get(&result.call_id)
                .copied()
                .unwrap_or(text.len());
            let (tool, target) = call_meta
                .get(&result.call_id)
                .map_or(("tool", "—"), |(tool, target)| {
                    (tool.as_str(), target.as_str())
                });
            let placeholder = elision_placeholder(original, tool, target);
            if placeholder.len() >= text.len() {
                continue;
            }
            result.result = tool_result_with_text(&result.result, placeholder);
            affected.insert(result.call_id.clone());
        }
        if fits(groups.as_slice()) {
            return Some(finish(groups, &affected, false));
        }
    }

    // Stufe (c): Runden durch eine Fortschrittsnotiz ersetzen.
    let region: Vec<Vec<TurnItem>> = groups.drain(first_current..protected_from).collect();
    for item in region.iter().flatten() {
        if let TurnItem::ToolResult(result) = item {
            affected.insert(result.call_id.clone());
        }
    }
    let note = progress_note_item(&region, &original_sizes);
    groups.insert(first_current, vec![note]);
    Some(finish(groups, &affected, true))
}

/// Kürzt jedes Tool-Ergebnis des aktuellen Turns (nach der letzten
/// `UserMessage`), das größer als `max_bytes` ist, auf Kopf und Ende
/// (Notdruck-Stufe (e1), Teil C).
///
/// # Returns
/// Die neue Historie und die Anzahl gekürzter Ergebnisse (`0` ⇒ unverändert).
fn trim_current_turn_results(
    history: &ConversationHistory,
    max_bytes: usize,
) -> (ConversationHistory, usize) {
    let mut items: Vec<TurnItem> = history.items().to_vec();
    let first_current = items
        .iter()
        .rposition(|item| matches!(item, TurnItem::UserMessage(_)))
        .map_or(0, |idx| idx + 1);
    let mut trimmed = 0_usize;
    for item in items.iter_mut().skip(first_current) {
        let TurnItem::ToolResult(result) = item else {
            continue;
        };
        let text = tool_result_text(&result.result);
        if text.len() <= max_bytes {
            continue;
        }
        let cut = elide_middle(&text, max_bytes, 50);
        result.result = tool_result_with_text(&result.result, cut);
        trimmed += 1;
    }
    (ConversationHistory::from_items(items), trimmed)
}

/// Baut die Fortschrittsnotiz für die ausgelassenen Gruppen `region`. Eine
/// bereits vorhandene Fortschrittsnotiz in `region` wird zusammengeführt
/// (ihre Einträge zuerst), nie verschachtelt.
fn progress_note_item(
    region: &[Vec<TurnItem>],
    original_sizes: &HashMap<ToolCallId, usize>,
) -> TurnItem {
    let mut entries: Vec<String> = Vec::new();
    for group in region {
        for item in group {
            match item {
                TurnItem::AssistantMessage(message) => {
                    let text = render_content(&message.content);
                    if let Some(body) = text.strip_prefix(PROGRESS_MARKER) {
                        entries.extend(
                            body.lines()
                                .filter(|line| line.starts_with("- "))
                                .map(str::to_owned),
                        );
                    } else if !text.trim().is_empty() {
                        entries.push(format!(
                            "- Notiz: {}",
                            shorten(text.trim(), PROGRESS_NOTE_TEXT_MAX_BYTES).replace('\n', " ")
                        ));
                    }
                }
                TurnItem::ToolCall(call) => {
                    let result = group.iter().find_map(|candidate| match candidate {
                        TurnItem::ToolResult(result) if result.call_id == call.call_id => {
                            Some(result)
                        }
                        _ => None,
                    });
                    let status = match result {
                        Some(result) if result.result.is_success() => "ok",
                        Some(_) => "Fehler",
                        None => "ohne Ergebnis",
                    };
                    let bytes = result.map_or(0, |result| {
                        original_sizes
                            .get(&result.call_id)
                            .copied()
                            .unwrap_or_else(|| tool_result_text(&result.result).len())
                    });
                    entries.push(format!(
                        "- {}({}) → {status}, {bytes} Bytes",
                        call.tool_name,
                        call_target(&call.arguments)
                    ));
                }
                TurnItem::Error(error) => {
                    entries.push(format!(
                        "- Fehler: {}",
                        shorten(error.message.trim(), PROGRESS_NOTE_TEXT_MAX_BYTES)
                            .replace('\n', " ")
                    ));
                }
                TurnItem::UserMessage(_) | TurnItem::ToolResult(_) | TurnItem::Reasoning(_) => {}
            }
        }
    }

    let mut body = String::from(PROGRESS_MARKER);
    body.push_str(
        "Frühere Runden dieses Auftrags wurden aus Platzgründen ausgelassen; ihre Ergebnisse \
         stehen nicht mehr im Verlauf und müssen bei Bedarf erneut ermittelt werden.\n",
    );
    let skipped = entries.len().saturating_sub(PROGRESS_NOTE_MAX_ENTRIES);
    if skipped > 0 {
        body.push_str(&format!("- … {skipped} ältere Einträge ausgelassen\n"));
    }
    for entry in entries.iter().skip(skipped) {
        body.push_str(entry);
        body.push('\n');
    }

    TurnItem::AssistantMessage(AssistantMessageItem {
        id: ItemId::new(),
        content: vec![ContentPart::Text { text: body }],
        phase: None,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;
    use harw_protocol::items::{ToolCallItem, ToolResultItem, UserMessageItem};

    #[test]
    fn plan_for_target_tokens_uses_four_bytes_per_token() {
        let plan = CompactionPlan::for_target_tokens(24_000);
        assert_eq!(plan.target_history_bytes, 96_000);
        assert_eq!(plan.tool_result_head_bytes, 4096);
        assert_eq!(plan.summary_model, None);
        assert_eq!(plan.summary_provider, None);
        assert_eq!(plan.summary_window_tokens, None);
    }

    fn user(text: &str) -> TurnItem {
        TurnItem::UserMessage(UserMessageItem {
            id: ItemId::new(),
            content: vec![ContentPart::Text {
                text: text.to_owned(),
            }],
        })
    }

    fn call(id: &str, name: &str, args: serde_json::Value) -> TurnItem {
        TurnItem::ToolCall(ToolCallItem {
            id: ItemId::new(),
            call_id: ToolCallId::from_str(id),
            tool_name: name.to_owned(),
            arguments: args,
        })
    }

    fn ok_result(id: &str, value: serde_json::Value) -> TurnItem {
        TurnItem::ToolResult(ToolResultItem {
            id: ItemId::new(),
            call_id: ToolCallId::from_str(id),
            result: ToolCallResult::success(value),
            duration_ms: 1,
            trust: harw_protocol::items::ResultTrust::Untrusted,
        })
    }

    fn err_result(id: &str, message: &str) -> TurnItem {
        TurnItem::ToolResult(ToolResultItem {
            id: ItemId::new(),
            call_id: ToolCallId::from_str(id),
            result: ToolCallResult::error(message),
            duration_ms: 1,
            trust: harw_protocol::items::ResultTrust::Untrusted,
        })
    }

    fn default_plan() -> CompactionPlan {
        CompactionPlan {
            target_history_bytes: usize::MAX,
            tool_result_head_bytes: 32,
            summary_model: None,
            summary_provider: None,
            summary_window_tokens: None,
        }
    }

    #[test]
    fn test_deterministic_pass_drops_failed_and_retried_pair() {
        let mut history = ConversationHistory::new();
        history.push(user("do something"));
        history.push(call("a", "shell.exec", serde_json::json!({"cmd": "x"})));
        history.push(err_result("a", "boom"));
        history.push(call("b", "shell.exec", serde_json::json!({"cmd": "x"})));
        history.push(ok_result("b", serde_json::json!({"ok": true})));
        // Current turn: neue UserMessage, danach nichts mehr aus dem alten Teil.
        history.push(user("next turn"));

        let (result, outcome) = deterministic_pass(&history, &default_plan());

        assert_eq!(
            outcome.items_dropped, 2,
            "call+result des fehlgeschlagenen Paars entfernt"
        );
        let call_ids: Vec<&str> = result
            .items()
            .iter()
            .filter_map(|item| match item {
                TurnItem::ToolCall(c) => Some(c.call_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            call_ids,
            vec!["b"],
            "nur der wiederholte, erfolgreiche Aufruf bleibt"
        );
    }

    #[test]
    fn test_deterministic_pass_never_touches_current_turn() {
        let mut history = ConversationHistory::new();
        history.push(user("older turn"));
        history.push(call("a", "shell.exec", serde_json::json!({"cmd": "x"})));
        history.push(err_result("a", "boom"));
        history.push(call("b", "shell.exec", serde_json::json!({"cmd": "x"})));
        history.push(ok_result("b", serde_json::json!({"ok": true})));

        // Aktueller Turn: enthält denselben fehlgeschlagenen Aufruf-Namen +
        // Argumente wie oben — würde Schritt (a) im älteren Teil greifen,
        // aber dieser hier gehört zum aktuellen Turn und darf nicht
        // angefasst werden.
        history.push(user("current trigger"));
        history.push(call("c", "shell.exec", serde_json::json!({"cmd": "x"})));
        history.push(err_result("c", "boom again"));

        let (result, _outcome) = deterministic_pass(&history, &default_plan());

        let has_current_failed_pair = result
            .items()
            .iter()
            .any(|item| matches!(item, TurnItem::ToolCall(c) if c.call_id.as_str() == "c"))
            && result
                .items()
                .iter()
                .any(|item| matches!(item, TurnItem::ToolResult(r) if r.call_id.as_str() == "c"));
        assert!(
            has_current_failed_pair,
            "der fehlgeschlagene Aufruf im aktuellen Turn wurde entfernt, obwohl er unangetastet bleiben muss"
        );

        let has_current_user = result.items().iter().any(|item| {
            matches!(item, TurnItem::UserMessage(m) if matches!(&m.content[0], ContentPart::Text { text } if text == "current trigger"))
        });
        assert!(
            has_current_user,
            "die auslösende UserMessage des aktuellen Turns fehlt"
        );
    }

    #[test]
    fn test_deterministic_pass_truncates_oversized_old_result_with_marker() -> TestResult {
        let mut history = ConversationHistory::new();
        history.push(user("older turn"));
        history.push(call(
            "a",
            "fs.read",
            serde_json::json!({"path": "/tmp/foo.txt"}),
        ));
        let big_text = "y".repeat(200);
        history.push(ok_result("a", serde_json::json!(big_text)));
        history.push(user("current trigger"));

        let plan = default_plan();
        let (result, outcome) = deterministic_pass(&history, &plan);

        assert_eq!(outcome.results_truncated, 1);
        let truncated_text = result
            .items()
            .iter()
            .find_map(|item| match item {
                TurnItem::ToolResult(r) if r.call_id.as_str() == "a" => match &r.result {
                    ToolCallResult::Success {
                        value: serde_json::Value::String(text),
                    } => Some(text.clone()),
                    _ => None,
                },
                _ => None,
            })
            .ok_or(crate::test_support::TestError::Missing(
                "gekürztes Ergebnis bleibt als Erfolg mit Text erhalten",
            ))?;

        assert!(truncated_text.starts_with(&"y".repeat(plan.tool_result_head_bytes)));
        assert!(truncated_text.contains("gekürzt"));
        assert!(
            truncated_text.contains("/tmp/foo.txt"),
            "Pfad-Hinweis für fs.read fehlt"
        );
        assert!(truncated_text.len() < big_text.len());
        Ok(())
    }

    #[test]
    fn test_is_read_only_tool_covers_every_read_only_prefix() {
        for tool_name in [
            "fs.read",
            "fs.list",
            "fs.search",
            "fs.grep",
            "fs.glob",
            "doc.read_pdf",
            "fs.read.chunk",
            "doc.read_pdf.page",
        ] {
            assert!(
                is_read_only_tool(tool_name),
                "'{tool_name}' muss als schreibgeschütztes Lesetool gelten"
            );
        }
        for tool_name in ["fs.write", "shell.exec", "web.fetch", "doc.write_pdf"] {
            assert!(
                !is_read_only_tool(tool_name),
                "'{tool_name}' darf nicht als schreibgeschütztes Lesetool gelten"
            );
        }
    }

    #[test]
    fn test_deterministic_pass_truncates_oversized_doc_read_pdf_result_with_path_hint() -> TestResult
    {
        let mut history = ConversationHistory::new();
        history.push(user("older turn"));
        history.push(call(
            "a",
            "doc.read_pdf",
            serde_json::json!({"path": "/tmp/report.pdf"}),
        ));
        let big_text = "z".repeat(200);
        history.push(ok_result("a", serde_json::json!(big_text)));
        history.push(user("current trigger"));

        let plan = default_plan();
        let (result, outcome) = deterministic_pass(&history, &plan);

        assert_eq!(outcome.results_truncated, 1);
        let truncated_text = result
            .items()
            .iter()
            .find_map(|item| match item {
                TurnItem::ToolResult(r) if r.call_id.as_str() == "a" => match &r.result {
                    ToolCallResult::Success {
                        value: serde_json::Value::String(text),
                    } => Some(text.clone()),
                    _ => None,
                },
                _ => None,
            })
            .ok_or(crate::test_support::TestError::Missing(
                "gekürztes Ergebnis bleibt als Erfolg mit Text erhalten",
            ))?;

        assert!(
            truncated_text.contains("/tmp/report.pdf"),
            "Pfad-Hinweis für doc.read_pdf fehlt"
        );
        Ok(())
    }

    #[test]
    fn test_deterministic_pass_dedupes_repeated_doc_read_pdf_calls_keeping_last() {
        let mut history = ConversationHistory::new();
        history.push(user("older turn"));
        history.push(call(
            "a",
            "doc.read_pdf",
            serde_json::json!({"path": "/x.pdf"}),
        ));
        history.push(ok_result("a", serde_json::json!("first read")));
        history.push(call(
            "b",
            "doc.read_pdf",
            serde_json::json!({"path": "/x.pdf"}),
        ));
        history.push(ok_result("b", serde_json::json!("second read")));
        history.push(user("current trigger"));

        let (result, outcome) = deterministic_pass(&history, &default_plan());

        assert_eq!(outcome.calls_deduplicated, 1);
        let call_ids: Vec<&str> = result
            .items()
            .iter()
            .filter_map(|item| match item {
                TurnItem::ToolCall(c) => Some(c.call_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            call_ids,
            vec!["b"],
            "nur das letzte Vorkommen bleibt erhalten"
        );
    }

    #[test]
    fn test_deterministic_pass_dedupes_repeated_read_only_calls_keeping_last() {
        let mut history = ConversationHistory::new();
        history.push(user("older turn"));
        history.push(call("a", "fs.read", serde_json::json!({"path": "/x"})));
        history.push(ok_result("a", serde_json::json!("first read")));
        history.push(call("b", "fs.read", serde_json::json!({"path": "/x"})));
        history.push(ok_result("b", serde_json::json!("second read")));
        history.push(user("current trigger"));

        let (result, outcome) = deterministic_pass(&history, &default_plan());

        assert_eq!(outcome.calls_deduplicated, 1);
        let call_ids: Vec<&str> = result
            .items()
            .iter()
            .filter_map(|item| match item {
                TurnItem::ToolCall(c) => Some(c.call_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            call_ids,
            vec!["b"],
            "nur das letzte Vorkommen bleibt erhalten"
        );
    }

    // ------------------------------------------------------------------
    // Welle 3: Budget-Compaction, Summarizer-Härtung, angeheftete Summaries
    // ------------------------------------------------------------------

    use crate::model::{ModelFuture, ModelResponse};
    use crate::test_support::TestError;
    use harw_protocol::items::ReasoningItem;
    use std::sync::{Mutex, PoisonError};

    fn assistant(text: &str) -> TurnItem {
        TurnItem::AssistantMessage(AssistantMessageItem {
            id: ItemId::new(),
            content: vec![ContentPart::Text {
                text: text.to_owned(),
            }],
            phase: None,
        })
    }

    fn reasoning(text: &str) -> TurnItem {
        TurnItem::Reasoning(ReasoningItem {
            id: ItemId::new(),
            summary_text: vec![text.to_owned()],
            raw_content: Vec::new(),
        })
    }

    fn test_session() -> AgentSession {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        AgentSession::new(
            harw_types::AgentRole::Assistant,
            None,
            harw_extension_api::empty_extension_registry(),
            tx,
        )
    }

    /// Zeichnet jeden Zusammenfassungs-Prompt (erste `UserMessage`) samt
    /// `max_output_tokens` auf und antwortet mit festem Text und Stop-Grund.
    struct RecordingSummaryModel {
        reply: String,
        stop: StopReason,
        prompts: Mutex<Vec<(String, Option<u32>)>>,
    }

    impl RecordingSummaryModel {
        fn new(reply: &str, stop: StopReason) -> Self {
            Self {
                reply: reply.to_owned(),
                stop,
                prompts: Mutex::new(Vec::new()),
            }
        }

        fn prompts(&self) -> Vec<(String, Option<u32>)> {
            self.prompts
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }
    }

    impl ModelProvider for RecordingSummaryModel {
        fn respond<'a>(&'a self, request: ModelRequest) -> ModelFuture<'a> {
            let prompt = request
                .history
                .items()
                .iter()
                .find_map(|item| match item {
                    TurnItem::UserMessage(message) => Some(render_content(&message.content)),
                    _ => None,
                })
                .unwrap_or_default();
            self.prompts
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((prompt, request.max_output_tokens));
            let mut response = ModelResponse::text(self.reply.clone());
            response.stop = self.stop.clone();
            response.usage.input_tokens = 10;
            response.usage.output_tokens = 5;
            Box::pin(async move { Ok(response) })
        }
    }

    fn result_text(history: &ConversationHistory, call_id: &str) -> Option<String> {
        history.items().iter().find_map(|item| match item {
            TurnItem::ToolResult(result) if result.call_id.as_str() == call_id => {
                Some(tool_result_text(&result.result))
            }
            _ => None,
        })
    }

    fn pinned_texts(history: &ConversationHistory) -> Vec<String> {
        history
            .items()
            .iter()
            .filter(|item| is_pinned_summary(item))
            .filter_map(|item| match item {
                TurnItem::AssistantMessage(message) => Some(render_content(&message.content)),
                _ => None,
            })
            .collect()
    }

    fn progress_notes(history: &ConversationHistory) -> Vec<String> {
        history
            .items()
            .iter()
            .filter_map(|item| match item {
                TurnItem::AssistantMessage(message) => {
                    let text = render_content(&message.content);
                    text.starts_with(PROGRESS_MARKER).then_some(text)
                }
                _ => None,
            })
            .collect()
    }

    /// Jeder Call hat genau ein Ergebnis und umgekehrt, und jedes Ergebnis
    /// folgt seinem Call.
    fn assert_pairing(history: &ConversationHistory) -> TestResult {
        let items = history.items();
        for (index, item) in items.iter().enumerate() {
            match item {
                TurnItem::ToolCall(call) => {
                    let results = items[index..]
                        .iter()
                        .filter(|candidate| {
                            matches!(candidate, TurnItem::ToolResult(r) if r.call_id == call.call_id)
                        })
                        .count();
                    if results != 1 {
                        return Err(TestError::Unexpected(format!(
                            "Call {} hat {results} nachfolgende Ergebnisse",
                            call.call_id.as_str()
                        )));
                    }
                }
                TurnItem::ToolResult(result) => {
                    let has_call = items[..index].iter().any(|candidate| {
                        matches!(candidate, TurnItem::ToolCall(c) if c.call_id == result.call_id)
                    });
                    if !has_call {
                        return Err(TestError::Unexpected(format!(
                            "Ergebnis {} ohne vorausgehenden Call",
                            result.call_id.as_str()
                        )));
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Ein Worker-Turn: eine `UserMessage`, danach `rounds` Runden mit je
    /// einer Notiz, einem Reasoning-Item und einem `fs.read` mit
    /// `result_bytes` großem Ergebnis.
    fn push_worker_rounds(
        history: &mut ConversationHistory,
        range: std::ops::Range<usize>,
        result_bytes: usize,
    ) {
        for i in range {
            history.push(assistant(&format!("Ich lese Datei {i}")));
            history.push(reasoning("interne Überlegung"));
            history.push(call(
                &format!("c{i}"),
                "fs.read",
                serde_json::json!({"path": format!("/src/f{i}.rs")}),
            ));
            history.push(ok_result(
                &format!("c{i}"),
                serde_json::json!("x".repeat(result_bytes)),
            ));
        }
    }

    #[tokio::test]
    async fn test_compact_for_budget_single_turn_worker_shrinks_below_target() -> TestResult {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut session = test_session().with_turn_event_sink(event_tx);
        session
            .history_mut()
            .push(user("Analysiere das Repository"));
        push_worker_rounds(session.history_mut(), 0..8, 20_000);

        let target = 20_000_u64;
        let before = estimate_history_tokens(session.history(), session.token_calibration());
        assert!(
            before > target,
            "Ausgangslage muss über dem Ziel liegen ({before})"
        );

        let outcome = compact_for_budget(
            &mut session,
            &crate::model::EchoModelProvider::default(),
            target,
            0,
            None,
        )
        .await?;

        assert!(!outcome.no_op);
        assert!(
            !outcome.summarized,
            "kein älterer Turn ⇒ keine Zusammenfassung"
        );
        assert!(outcome.elided_results > 0);
        assert_eq!(outcome.tokens_before, before);
        assert!(
            outcome.tokens_after <= target,
            "Schätzung nach der Elision ({}) muss unter dem Ziel liegen",
            outcome.tokens_after
        );
        assert_eq!(
            outcome.tokens_after,
            estimate_history_tokens(session.history(), session.token_calibration())
        );

        // Auslösende UserMessage und die letzten zwei Runden bleiben unverändert.
        assert!(matches!(
            session.history().items().first(),
            Some(TurnItem::UserMessage(_))
        ));
        for id in ["c6", "c7"] {
            let text = result_text(session.history(), id)
                .ok_or(TestError::Missing("geschützte Runde fehlt"))?;
            assert_eq!(text.len(), 20_000, "Runde {id} darf nicht verändert werden");
        }
        let oldest = result_text(session.history(), "c0").ok_or(TestError::Missing(
            "älteste Runde bleibt vor Stufe (c) gepaart",
        ))?;
        assert!(oldest.len() < 20_000);
        assert_pairing(session.history())?;

        let mut saw_applied = false;
        while let Ok(event) = event_rx.try_recv() {
            if matches!(event, harw_protocol::TurnEvent::CompactionApplied { .. }) {
                saw_applied = true;
            }
        }
        assert!(
            saw_applied,
            "eine echte Verdichtung meldet CompactionApplied"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_compact_for_budget_preserves_pairing_and_merges_progress_notes() -> TestResult {
        let mut session = test_session();
        session
            .history_mut()
            .push(user("Analysiere das Repository"));
        push_worker_rounds(session.history_mut(), 0..8, 20_000);
        let model = crate::model::EchoModelProvider::default();

        // Unerreichbares Ziel ⇒ alle Stufen bis zur Fortschrittsnotiz.
        let outcome = compact_for_budget(&mut session, &model, 1, 0, None).await?;
        assert!(outcome.progress_note);
        assert_eq!(outcome.elided_results, 6);
        assert_pairing(session.history())?;
        assert!(matches!(
            session.history().items().first(),
            Some(TurnItem::UserMessage(_))
        ));
        for id in ["c0", "c5"] {
            assert!(result_text(session.history(), id).is_none());
        }
        for id in ["c6", "c7"] {
            assert!(result_text(session.history(), id).is_some());
        }
        let notes = progress_notes(session.history());
        assert_eq!(notes.len(), 1);
        assert!(notes.iter().any(|note| note.contains("/src/f0.rs")));

        // Weitere Runden, erneute Verdichtung: die Notiz wird zusammengeführt,
        // nicht verschachtelt.
        push_worker_rounds(session.history_mut(), 8..11, 20_000);
        let outcome = compact_for_budget(&mut session, &model, 1, 0, None).await?;
        assert!(outcome.progress_note);
        assert_pairing(session.history())?;
        let notes = progress_notes(session.history());
        assert_eq!(notes.len(), 1, "genau eine Fortschrittsnotiz");
        let note = notes.first().ok_or(TestError::Missing("Notiz"))?;
        assert!(note.contains("/src/f0.rs") && note.contains("/src/f8.rs"));
        assert_eq!(note.matches(PROGRESS_MARKER.trim_end()).count(), 1);
        for id in ["c9", "c10"] {
            assert!(result_text(session.history(), id).is_some());
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_compact_for_budget_keeps_pinned_summary() -> TestResult {
        let mut session = test_session();
        session
            .history_mut()
            .push(summary_item("alte Zusammenfassung"));
        session.history_mut().push(user("Auftrag"));
        push_worker_rounds(session.history_mut(), 0..5, 20_000);
        let model = RecordingSummaryModel::new("unbenutzt", StopReason::EndTurn);

        let outcome = compact_for_budget(&mut session, &model, 1, 0, None).await?;

        assert!(!outcome.no_op);
        assert!(
            model.prompts().is_empty(),
            "nur eine angeheftete Summary als älterer Teil ⇒ kein Modellaufruf"
        );
        let first = session
            .history()
            .items()
            .first()
            .ok_or(TestError::Missing("Historie leer"))?;
        assert!(is_pinned_summary(first));
        assert_eq!(pinned_texts(session.history()).len(), 1);
        assert!(
            pinned_texts(session.history())
                .iter()
                .any(|text| text.contains("alte Zusammenfassung"))
        );

        // Auch der deterministische Durchgang lässt sie unangetastet.
        let (after, _) = deterministic_pass(session.history(), &default_plan());
        assert_eq!(pinned_texts(&after), pinned_texts(session.history()));
        Ok(())
    }

    #[test]
    fn test_is_pinned_summary_recognizes_only_marked_assistant_messages() {
        assert!(is_pinned_summary(&summary_item("x")));
        assert!(!is_pinned_summary(&assistant("normale Antwort")));
        assert!(!is_pinned_summary(&user(SUMMARY_MARKER)));
        assert!(!is_pinned_summary(&reasoning(SUMMARY_MARKER)));
    }

    fn summarizing_plan() -> CompactionPlan {
        CompactionPlan {
            target_history_bytes: 0,
            tool_result_head_bytes: 4096,
            summary_model: None,
            summary_provider: None,
            summary_window_tokens: Some(100_000),
        }
    }

    #[tokio::test]
    async fn test_repeated_compaction_merges_summary_instead_of_nesting() -> TestResult {
        let mut session = test_session();
        session.history_mut().push(summary_item("ALT-SUMMARY"));
        for i in 0..6 {
            session.history_mut().push(user(&format!("frage {i}")));
            session
                .history_mut()
                .push(assistant(&format!("antwort {i}")));
        }
        session.history_mut().push(user("aktuell"));

        // Das Modell gibt den Marker selbst mit aus — er darf nicht doppelt
        // im Verlauf landen.
        let first_model = RecordingSummaryModel::new(
            &format!("{SUMMARY_MARKER}NEU-SUMMARY"),
            StopReason::EndTurn,
        );
        let outcome =
            compact_session(&mut session, &first_model, &summarizing_plan(), None).await?;
        assert!(outcome.summarized);
        assert_eq!(outcome.summary_text.as_deref(), Some("NEU-SUMMARY"));
        assert_ne!(outcome.summary_usage, TokenUsage::default());

        let prompts = first_model.prompts();
        let (prompt, max_output) = prompts.first().ok_or(TestError::Missing("Prompt"))?;
        assert!(prompt.contains("Bisherige Zusammenfassung:\nALT-SUMMARY"));
        assert!(!prompt.contains(SUMMARY_MARKER.trim_end()));
        assert_eq!(*max_output, Some(SUMMARY_MAX_OUTPUT_TOKENS));

        let pinned = pinned_texts(session.history());
        assert_eq!(pinned, vec![format!("{SUMMARY_MARKER}NEU-SUMMARY")]);

        // Zweite Verdichtung: wieder genau eine Summary, die vorige wird
        // als bisherige Zusammenfassung mitgegeben.
        let second_model = RecordingSummaryModel::new("NEU2", StopReason::EndTurn);
        let outcome =
            compact_session(&mut session, &second_model, &summarizing_plan(), None).await?;
        assert!(outcome.summarized);
        let prompts = second_model.prompts();
        let (prompt, _) = prompts.first().ok_or(TestError::Missing("Prompt"))?;
        assert!(prompt.contains("Bisherige Zusammenfassung:\nNEU-SUMMARY"));
        let pinned = pinned_texts(session.history());
        assert_eq!(pinned, vec![format!("{SUMMARY_MARKER}NEU2")]);
        assert!(matches!(
            session.history().items().last(),
            Some(TurnItem::UserMessage(_))
        ));
        Ok(())
    }

    #[tokio::test]
    async fn test_summary_stop_reason_is_checked() -> TestResult {
        fn history() -> ConversationHistory {
            let mut history = ConversationHistory::new();
            for i in 0..4 {
                history.push(user(&format!("frage {i}")));
                history.push(assistant(&format!("antwort {i}")));
            }
            history.push(user("aktuell"));
            history
        }

        // Abgeschnitten ⇒ behalten, aber markiert.
        let mut session = test_session();
        *session.history_mut() = history();
        let model = RecordingSummaryModel::new("halbe Summary", StopReason::MaxTokens);
        let outcome = compact_session(&mut session, &model, &summarizing_plan(), None).await?;
        assert!(outcome.summarized);
        assert!(outcome.summary_truncated);
        let text = outcome
            .summary_text
            .ok_or(TestError::Missing("Summary-Text"))?;
        assert!(text.ends_with(SUMMARY_TRUNCATED_NOTE));

        // Abgelehnt ⇒ verworfen, Nutzung trotzdem erfasst.
        let mut session = test_session();
        *session.history_mut() = history();
        let model = RecordingSummaryModel::new("nein", StopReason::Refusal { detail: None });
        let outcome = compact_session(&mut session, &model, &summarizing_plan(), None).await?;
        assert!(!outcome.summarized);
        assert!(outcome.no_op);
        assert_ne!(outcome.summary_usage, TokenUsage::default());
        assert!(pinned_texts(session.history()).is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_summary_input_is_capped_to_summary_window() -> TestResult {
        let mut session = test_session();
        for i in 0..200 {
            session.history_mut().push(user(&format!("frage {i}")));
            session
                .history_mut()
                .push(assistant(&format!("antwort {i} {}", "z".repeat(1000))));
        }
        session.history_mut().push(user("aktuell"));
        let mut plan = summarizing_plan();
        plan.summary_window_tokens = Some(10_000);
        let model = RecordingSummaryModel::new("kurz", StopReason::EndTurn);

        compact_session(&mut session, &model, &plan, None).await?;

        let config = SummaryConfig {
            model: None,
            provider: None,
            window_tokens: 10_000,
            bytes_per_token: session.token_calibration().bytes_per_token(),
        };
        let prompts = model.prompts();
        let (prompt, _) = prompts.first().ok_or(TestError::Missing("Prompt"))?;
        assert!(
            prompt.len() <= config.input_budget_bytes() + 256,
            "Prompt ({} Bytes) überschreitet das Eingabe-Budget ({})",
            prompt.len(),
            config.input_budget_bytes()
        );
        assert!(prompt.contains("Bytes ausgelassen"));
        Ok(())
    }

    #[tokio::test]
    async fn test_compact_for_budget_under_target_is_noop_without_event() -> TestResult {
        let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut session = test_session().with_turn_event_sink(event_tx);
        session.history_mut().push(user("hallo"));
        session.history_mut().push(assistant("hi"));
        let items_before = session.history().len();

        let outcome = compact_for_budget(
            &mut session,
            &crate::model::EchoModelProvider::default(),
            1_000_000,
            0,
            None,
        )
        .await?;

        assert!(outcome.no_op);
        assert_eq!(outcome.tokens_before, outcome.tokens_after);
        assert_eq!(session.history().len(), items_before);
        assert!(
            event_rx.try_recv().is_err(),
            "ein No-op darf kein CompactionApplied senden"
        );
        Ok(())
    }

    #[test]
    fn test_reasoning_is_excluded_from_estimates_and_dropped_from_older_turns() {
        let mut history = ConversationHistory::new();
        history.push(user("alt"));
        history.push(reasoning(&"r".repeat(5000)));
        history.push(assistant("antwort"));
        history.push(user("aktuell"));
        history.push(reasoning("aktuelles Denken"));

        let mut without = ConversationHistory::new();
        without.push(user("alt"));
        without.push(assistant("antwort"));
        without.push(user("aktuell"));

        assert!(total_bytes(history.items()) < total_bytes(without.items()) + 100);
        assert_eq!(
            estimate_history_bytes(history.items()),
            estimate_history_bytes(without.items())
        );

        let (after, outcome) = deterministic_pass(&history, &default_plan());
        assert_eq!(
            outcome.items_dropped, 1,
            "nur das ältere Reasoning fällt weg"
        );
        let reasoning_left = after
            .items()
            .iter()
            .filter(|item| matches!(item, TurnItem::Reasoning(_)))
            .count();
        assert_eq!(reasoning_left, 1, "Reasoning im aktuellen Turn bleibt");
    }

    #[test]
    fn test_estimate_history_bytes_matches_request_estimate() {
        let mut history = ConversationHistory::new();
        history.push(user("frage"));
        history.push(assistant("ich lese"));
        history.push(call("a", "fs.read", serde_json::json!({"path": "/x"})));
        history.push(ok_result("a", serde_json::json!("inhalt")));
        history.push(reasoning("denken"));
        let expected = estimate_history_bytes(history.items());
        let request = ModelRequest::new(
            LoadedInstructions {
                system_prompt: String::new(),
                fragments: Vec::new(),
            },
            Vec::new(),
            history,
            Vec::new(),
        );
        assert_eq!(
            crate::context_budget::estimate_request_bytes(&request),
            expected
        );
    }

    /// Teil C: ein Kind im 32k-Fenster mit drei 64-KiB-Ergebnissen in den
    /// jüngsten Runden. Mit echtem Overhead (System + Werkzeuge) und ohne
    /// Notdruck bleibt die Historie zu groß; unter Notdruck passt der ganze
    /// Request (Historie + Overhead) ins Ziel, und die Paarung bleibt heil.
    #[tokio::test]
    async fn test_compact_for_budget_fits_a_32k_window_with_overhead() -> TestResult {
        const WINDOW: u64 = 32_768;
        const RESERVE: u64 = 4_915;
        const OVERHEAD: u64 = 8_000;
        let target = (WINDOW - RESERVE) * 60 / 100;
        let model = crate::model::EchoModelProvider::default();
        let build = || {
            let mut session = test_session();
            session.history_mut().push(user("Untersuche das Modul"));
            push_worker_rounds(session.history_mut(), 0..3, 64 * 1024);
            session
        };

        let mut regular = build();
        let outcome = compact_for_budget(&mut regular, &model, target, OVERHEAD, None).await?;
        assert!(
            outcome.tokens_after > target,
            "ohne Notdruck bleiben die zwei geschützten 64-KiB-Runden stehen"
        );
        assert!(outcome.tokens_after >= OVERHEAD, "der Overhead zählt mit");

        let mut pressed = build();
        let outcome = compact_for_budget(
            &mut pressed,
            &model,
            target,
            OVERHEAD,
            Some(CompactDecision::Emergency),
        )
        .await?;
        assert!(
            outcome.tokens_after <= target,
            "unter Notdruck passt der Request: {} > {target}",
            outcome.tokens_after
        );
        assert!(outcome.tokens_after + RESERVE <= WINDOW);
        assert_pairing(pressed.history())?;
        assert!(matches!(
            pressed.history().items().first(),
            Some(TurnItem::UserMessage(_))
        ));
        Ok(())
    }
}
