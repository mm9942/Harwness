//! Verdichtung (Compaction) der Session-Historie.
//!
//! # Wann läuft das?
//!
//! Anders als [`crate::history::ConversationHistory::tail_preserving_current_turn`]
//! (das bei **jeder** Modell-Runde die Sicht auf die Historie neu zuschneidet,
//! ohne die persistente Historie zu verändern) läuft dieses Modul nur an
//! **Compaction-Grenzen** — ausgelöst durch [`crate::auto_compact::AutoCompactPolicy`]
//! oder explizit vom Aufrufer. Der Grund ist die Provider-Prompt-Cache: jede
//! Änderung des Präfix-Inhalts (z. B. ein neu geschriebenes Summary-Item
//! anstelle vieler alter Items) invalidiert den Cache des Providers für den
//! gesamten Verlauf ab dieser Stelle. Würde diese Verdichtung bei jeder
//! Modell-Runde laufen, würde sie den Prompt-Cache bei praktisch jeder Runde
//! neu invalidieren — teuer und langsam. Deshalb wird die Historie hier
//! **mutiert und dauerhaft ersetzt**, höchstens einmal pro Compaction-Zyklus,
//! statt (wie `tail_preserving_current_turn`) bei jedem Request neu
//! projiziert zu werden.
//!
//! # Hybrid-Algorithmus
//!
//! 1. [`deterministic_pass`]: rein regelbasiert, ohne Modellaufruf. Arbeitet
//!    auf atomaren Gruppen ([`crate::history::ConversationHistory::atomic_groups`])
//!    und rührt den **aktuellen Turn** (alles ab der letzten `UserMessage`)
//!    nie an:
//!    - fehlgeschlagene Tool-Aufrufe fallen weg, wenn ein späterer Aufruf mit
//!      identischem Tool-Namen und kanonisch-gleichen Argumenten sie ersetzt
//!      hat (die Wiederholung war offensichtlich der eigentliche Versuch);
//!    - wiederholte, identische, erfolgreiche schreibgeschützte Lesezugriffe
//!      (`fs.read*`, `fs.list*`, `fs.search*`, `fs.grep*`, `fs.glob*`,
//!      `doc.read_pdf*`) werden bis auf den letzten dedupliziert;
//!    - übergroße alte Tool-Ergebnisse werden auf ihren Kopf plus einen
//!      Kürzungs-Hinweis reduziert.
//! 2. [`compact_session`]: führt zuerst [`deterministic_pass`] aus. Reicht das
//!    nicht, um unter [`CompactionPlan::target_history_bytes`] zu kommen,
//!    wird die ältere Hälfte des noch verbleibenden, nicht-aktuellen Teils
//!    der Historie durch **einen** Modellaufruf zu einer einzigen
//!    Zusammenfassung verdichtet. Schlägt der Modellaufruf fehl, bleibt das
//!    deterministische Ergebnis bestehen (kein Fehlschlag der gesamten
//!    Operation).
//!
//! # Persistenz
//!
//! `compact_session` ersetzt die Historie der übergebenen [`AgentSession`]
//! in-memory (`history_mut`). Das Schreiben auf einen `StateStore` ist
//! **nicht** Aufgabe dieses Moduls — das macht der Aufrufer.
//!
//! # Nebenläufigkeit
//!
//! [`deterministic_pass`] ist rein synchron und zustandslos.
//! [`compact_session`] ist `async`, weil es optional genau einen
//! Modellaufruf über `&dyn ModelProvider` macht; es hält dabei keine Locks.

use crate::auto_compact::CompactDecision;
use crate::context_budget::floor_char_boundary;
use crate::error::CoreResult;
use crate::history::ConversationHistory;
use crate::model::{ModelProvider, ModelRequest};
use crate::session::AgentSession;
use harw_extension_api::LoadedInstructions;
use harw_protocol::items::{AssistantMessageItem, ContentPart, ToolCallResult, TurnItem};
use harw_types::{ItemId, ModelId, SessionId, ToolCallId};
use std::collections::{BTreeSet, HashMap};

/// Präfix, das jede vom Modell erzeugte Zusammenfassung markiert — so bleibt
/// ein verdichteter Block im Verlauf für spätere Leser (Menschen wie Modell)
/// eindeutig als solcher erkennbar.
pub const SUMMARY_MARKER: &str = "[Verdichteter Verlauf]\n";

/// Deutsche Systeminstruktion für den Zusammenfassungs-Aufruf in
/// [`compact_session`].
const SUMMARY_INSTRUCTION: &str = "Fasse den bisherigen Verlauf knapp zusammen: Ziel, getroffene \
    Entscheidungen, erledigte und offene Punkte, berührte Dateien. Keine Floskeln.";

/// Konfiguration eines Compaction-Laufs.
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
    /// seinen konfigurierten Default-Provider verwenden. Wird zusammen mit
    /// `summary_model` an den `ModelRequest` in [`summarize_older_half`]
    /// weitergegeben (Addendum C: interne Modellstellen).
    pub summary_provider: Option<harw_types::ProviderId>,
}

impl CompactionPlan {
    /// Leitet einen Plan aus dem Kontextfenster des aktiven Modells ab.
    ///
    /// # Description
    /// `target_history_bytes` = 30 % des Fensters in Tokens, grob mit 4
    /// Bytes/Token umgerechnet (dieselbe Faustregel wie an anderen Stellen
    /// des Crates) — derselbe Anteil, den
    /// [`crate::auto_compact::AutoCompactPolicy::task_end_threshold_tokens`]
    /// als Lohn-Schwelle für eine Verdichtung ansetzt. `tool_result_head_bytes`
    /// ist mit 4096 fest verdrahtet; `summary_model` bleibt `None`
    /// (Katalog-Default).
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
    /// `tool_result_head_bytes = 4096`, `summary_model`/`summary_provider`
    /// = `None`. Saturierende Arithmetik — kein Überlauf.
    #[must_use]
    pub fn for_target_tokens(tokens: u64) -> Self {
        let target_bytes_u64 = tokens.saturating_mul(4);
        let target_history_bytes = usize::try_from(target_bytes_u64).unwrap_or(usize::MAX);
        Self {
            target_history_bytes,
            tool_result_head_bytes: 4096,
            summary_model: None,
            summary_provider: None,
        }
    }
}

/// Ergebnis eines Compaction-Laufs — sowohl von [`deterministic_pass`] allein
/// als auch von [`compact_session`] (das die Zusammenfassungs-Felder
/// zusätzlich befüllt) zurückgegeben.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompactionOutcome {
    /// Geschätzte Bytes der Historie vor der Verdichtung.
    pub bytes_before: usize,
    /// Geschätzte Bytes der Historie nach der Verdichtung.
    pub bytes_after: usize,
    /// Anzahl vollständig entfernter Items (fehlgeschlagene, wiederholte
    /// Tool-Aufrufe — Schritt (a)).
    pub items_dropped: usize,
    /// Anzahl auf Kopf + Hinweis gekürzter Tool-Ergebnisse (Schritt (c)).
    pub results_truncated: usize,
    /// Anzahl deduplizierter, identischer, erfolgreicher Lesezugriffe
    /// (Schritt (b)).
    pub calls_deduplicated: usize,
    /// `true`, wenn [`compact_session`] zusätzlich eine Modell-Zusammenfassung
    /// erzeugt hat.
    pub summarized: bool,
    /// Der reine Zusammenfassungstext (ohne [`SUMMARY_MARKER`]-Präfix), falls
    /// `summarized`.
    pub summary_text: Option<String>,
    /// Der Auslöser dieses Compaction-Laufs, sofern vom Aufrufer angegeben.
    pub reason: Option<CompactDecision>,
}

/// Beobachter, der über jede abgeschlossene Verdichtung informiert wird —
/// z. B. um ein Handoff-Artefakt zu aktualisieren.
pub trait CompactionObserver: Send + Sync {
    /// Wird von [`compact_session`] nach dem Ersetzen der Session-Historie
    /// aufgerufen, bevor `compact_session` zurückkehrt.
    fn on_compacted(&self, session_id: &SessionId, outcome: &CompactionOutcome);
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
/// # Arguments
/// - `history` (`&ConversationHistory`): die zu verdichtende Historie.
/// - `plan` (`&CompactionPlan`): liefert `tool_result_head_bytes`.
///
/// # Returns
/// Die verdichtete Historie sowie ein [`CompactionOutcome`] mit den
/// Zählern dieses Laufs (`summarized`/`summary_text`/`reason` bleiben leer —
/// die füllt nur [`compact_session`]).
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

    drop_failed_retried_pairs(&mut older, &mut items_dropped);
    dedupe_read_only_calls(&mut older, &mut calls_deduplicated);
    truncate_large_results(
        &mut older,
        plan.tool_result_head_bytes,
        &mut results_truncated,
    );

    let mut items = Vec::new();
    for group in older {
        items.extend(group);
    }
    for group in current {
        items.extend(group);
    }

    let bytes_after = total_bytes(&items);

    (
        ConversationHistory::from_items(items),
        CompactionOutcome {
            bytes_before,
            bytes_after,
            items_dropped,
            results_truncated,
            calls_deduplicated,
            summarized: false,
            summary_text: None,
            reason: None,
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
///   Zusammenfassungs-Aufruf (keine Tools, deutsche Systeminstruktion).
/// - `plan` (`&CompactionPlan`): Zielgröße, Kopf-Kappungsgrenze,
///   Zusammenfassungs-Modell.
/// - `reason` (`Option<CompactDecision>`): der Auslöser, ins
///   [`CompactionOutcome`] übernommen.
///
/// # Returns
/// `Ok(outcome)` — immer, auch wenn der Zusammenfassungs-Aufruf fehlschlägt
/// (dann bleibt das deterministische Ergebnis bestehen und der Fehler wird
/// nur geloggt, siehe `# Errors`).
///
/// # Errors
/// Dieser Aufruf schlägt derzeit nicht fehl: ein Fehler des Modells beim
/// Zusammenfassen wird abgefangen (`tracing::warn!`) statt propagiert. Der
/// `CoreResult`-Rückgabetyp bleibt für zukünftige, echte Fehlerpfade
/// (z. B. Persistenzfehler des Aufrufers) reserviert.
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
    let (deterministic_history, mut outcome) = deterministic_pass(session.history(), plan);
    let mut final_history = deterministic_history;

    if outcome.bytes_after > plan.target_history_bytes {
        if let Some((summarized_history, summary_text, bytes_after)) =
            summarize_older_half(&final_history, plan, model).await
        {
            final_history = summarized_history;
            outcome.summarized = true;
            outcome.summary_text = Some(summary_text);
            outcome.bytes_after = bytes_after;
        }
    }

    outcome.reason = reason;
    *session.history_mut() = final_history;

    if let Some(observer) = session.compaction_observer() {
        observer.on_compacted(session.id(), &outcome);
    }

    tracing::info!(
        bytes_before = outcome.bytes_before,
        bytes_after = outcome.bytes_after,
        items_dropped = outcome.items_dropped,
        results_truncated = outcome.results_truncated,
        calls_deduplicated = outcome.calls_deduplicated,
        summarized = outcome.summarized,
        "session compaction complete"
    );

    Ok(outcome)
}

// ---------------------------------------------------------------------
// Interne Hilfsfunktionen
// ---------------------------------------------------------------------

/// Geschätzte Serialisierungsgröße eines Items in Bytes — dieselbe Schätzung
/// wie `harw_core::history`s privates `item_bytes` (`serde_json`-Länge,
/// `usize::MAX` bei Serialisierungsfehler). Hier neu geschrieben, weil das
/// Original modul-privat ist.
fn item_bytes(item: &TurnItem) -> usize {
    serde_json::to_vec(item).map_or(usize::MAX, |bytes| bytes.len())
}

/// Summe der [`item_bytes`] über eine Item-Liste.
///
/// `pub(crate)`, damit `turn_loop` daraus die geschätzte Verlaufs-Token-Zahl
/// für die harte Turn-Start-Verdichtung ableiten kann (Addendum D), ohne die
/// Byte/Token-Faustregel ein zweites Mal zu implementieren.
pub(crate) fn total_bytes(items: &[TurnItem]) -> usize {
    items
        .iter()
        .map(item_bytes)
        .fold(0_usize, usize::saturating_add)
}

/// Schätzt die Token-Zahl der gesamten Historie aus ihrer Byte-Größe (4
/// Bytes/Token, dieselbe Faustregel wie [`CompactionPlan::for_context_window`]
/// und [`CompactionPlan::for_target_tokens`]).
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

/// Zerlegt die Historie in atomare Gruppen (eigene Kopien, siehe
/// [`crate::history::ConversationHistory::atomic_groups`]) und trennt sie an
/// der letzten `UserMessage`-Gruppe: alles ab dort (einschließlich) ist der
/// aktuelle Turn und wird von [`deterministic_pass`] nie verändert. Fehlt
/// eine `UserMessage`, gilt die gesamte Historie als „älter" (kein aktueller
/// Turn abgrenzbar).
fn split_current_turn(history: &ConversationHistory) -> (Vec<Vec<TurnItem>>, Vec<Vec<TurnItem>>) {
    let mut groups: Vec<Vec<TurnItem>> = history
        .atomic_groups()
        .into_iter()
        .map(|group| group.into_iter().cloned().collect())
        .collect();

    let user_idx = groups
        .iter()
        .rposition(|group| matches!(group.as_slice(), [TurnItem::UserMessage(_)]));

    match user_idx {
        Some(idx) => {
            let current = groups.split_off(idx);
            (groups, current)
        }
        None => (groups, Vec::new()),
    }
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

/// Reintext-Rendering eines Items für den Zusammenfassungs-Prompt.
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
            out.push_str(&call.arguments.to_string());
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
            out.push_str(&tool_result_text(&result.result));
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

/// Verdichtet die ältere Hälfte des nicht-aktuellen Teils von `history` per
/// Modellaufruf zu einer einzigen Zusammenfassung.
///
/// # Returns
/// `None`, wenn es nichts zu verdichten gibt (leerer älterer Teil, leerer
/// gerenderter Text) oder der Modellaufruf fehlschlägt bzw. leer antwortet —
/// in beiden Fällen bleibt das deterministische Ergebnis des Aufrufers
/// bestehen. `Some((history, summary_text, bytes_after))` bei Erfolg.
async fn summarize_older_half(
    history: &ConversationHistory,
    plan: &CompactionPlan,
    model: &dyn ModelProvider,
) -> Option<(ConversationHistory, String, usize)> {
    let (mut older, current) = split_current_turn(history);
    if older.is_empty() {
        return None;
    }

    let split = older.len().div_ceil(2);
    let keep_older = older.split_off(split);
    let to_summarize = older;

    let mut rendered = String::new();
    for group in &to_summarize {
        for item in group {
            render_item(&mut rendered, item);
        }
    }
    if rendered.trim().is_empty() {
        return None;
    }

    let mut request_history = ConversationHistory::new();
    request_history.push_user_text(rendered);

    let instructions = LoadedInstructions {
        system_prompt: SUMMARY_INSTRUCTION.to_owned(),
        fragments: Vec::new(),
    };

    let request = ModelRequest::new(instructions, Vec::new(), request_history, Vec::new())
        .with_model_id(plan.summary_model.clone())
        .with_provider_id(plan.summary_provider.clone());

    match model.respond(request).await {
        Ok(response) => {
            let Some(text) = response.message.filter(|text| !text.trim().is_empty()) else {
                tracing::warn!(
                    "compaction summary call returned no usable text; keeping deterministic result"
                );
                return None;
            };

            let summary_item = TurnItem::AssistantMessage(AssistantMessageItem {
                id: ItemId::new(),
                content: vec![ContentPart::Text {
                    text: format!("{SUMMARY_MARKER}{text}"),
                }],
                phase: None,
            });

            let mut items = vec![summary_item];
            for group in keep_older {
                items.extend(group);
            }
            for group in current {
                items.extend(group);
            }
            let bytes_after = total_bytes(&items);

            Some((ConversationHistory::from_items(items), text, bytes_after))
        }
        Err(error) => {
            tracing::warn!(error = %error, "compaction summary model call failed; keeping deterministic result");
            None
        }
    }
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
}
