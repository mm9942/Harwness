//! DoD-Taint je Turn (PL-100 Phase 4, Bedrohung B22, Entscheidung O1).
//!
//! # Zweck
//! Sobald ein Turn ein `dod.*`-Werkzeug abgefertigt hat, stehen
//! Sensordaten im Kontext des Modells — Text, den ein Angreifer auf dem Host
//! steuern kann (Prozessnamen, Pfade, Peers). Für den Rest **dieses** Turns
//! verlangen die Eingriffswerkzeuge ([`is_taint_gated`]: `process.kill`,
//! `host.sudo_exec` und jedes verändernde `job.*`) deshalb eine ausdrückliche
//! Rückfrage, auch wenn Auto-Modus, eine Sitzungsfreigabe oder eine gemerkte
//! Regel sie sonst durchließen. Unter Full Access gilt die Regel nicht
//! (Entscheidung O1: Full Access fragt nie); das entscheidet die
//! [`crate::policy::DodTaintPolicy`], die den Modus kennt.
//!
//! # Verantwortung
//! Dieses Modul hält nur das Vokabular und den Zustand:
//! - die **eine** Quelle der Wahrheit für Auslöser ([`is_dod_tool`]) und
//!   betroffene Werkzeuge ([`is_taint_gated`], [`JOB_READ_ONLY_TOOLS`],
//!   [`TAINT_GATED_HOST_TOOLS`]);
//! - [`DodTaint`], den geteilten Zustand eines Turns (er liegt im
//!   [`crate::turn_loop::TurnControl`] des Turns);
//! - den Prüfkontext ([`with_review_taint`], [`review_turn_tainted`]), über
//!   den `check_approval` den Freigabe-Handlern mitteilt, ob der geprüfte
//!   Aufruf zu einem getainteten Turn gehört;
//! - den Grundtext für die Rückfrage ([`TAINT_REASON`], [`ask_reason`],
//!   [`pending_ask_reason`]) und für die Ablehnung, wo niemand antworten
//!   kann ([`unanswerable_reason`]).
//!
//! # Zuschreibung der Rückfrage
//! Den Grund „nach DoD-Abfrage in diesem Turn" zeigt eine Oberfläche nur,
//! wenn der Taint die offene Rückfrage **verursacht** hat: `check_approval`
//! vermerkt dazu genau die Call-Id, bei der die
//! [`crate::policy::DodTaintPolicy`] (oder ihr modusblinder Fallback)
//! `AskUser` lieferte ([`DodTaint::note_ask`]). Fragt unter Full Access ein
//! anderer Handler, oder fragt `job.stop` im Modus `ask` ohne Taint, gibt
//! [`pending_ask_reason`] `None`.
//!
//! # Lebensdauer des Taints
//! - **Gesetzt**, wenn ein `dod.*`-Aufruf abgefertigt wurde — auch wenn er
//!   scheiterte oder abgelehnt wurde (der Turn-Loop meldet jedes Ergebnis
//!   über `notify_tool_outcome`).
//! - **Gilt** nur für den Turn, in dem er entstand: der Zustand ist nach
//!   [`TurnId`] geschlüsselt und liegt im Steuerblock dieses Turns. Ein neuer
//!   Turn hat eine neue Id (und normalerweise einen frischen Steuerblock) und
//!   ist damit nie getaintet — auch dann nicht, wenn ein Aufrufer denselben
//!   Steuerblock wiederverwendet.
//! - **Überlebt** eine Rückfrage- oder Handoff-Pause, weil die Session den
//!   Steuerblock bis `complete_turn` aufbewahrt.
//! - **Erreicht nie ein Kind:** Kind-Sitzungen laufen eigene Turns mit
//!   eigenen Ids und eigenen Steuerblöcken.
//!
//! # Wichtige Typen
//! [`DodTaint`] (Zustand), [`crate::policy::DodTaintPolicy`] (Entscheidung).
//!
//! # Nebenläufigkeit
//! [`DodTaint`] teilt seinen Zustand über `Arc<Mutex<_>>`; Klone sehen
//! denselben Stand. Ein vergifteter Lock wird aufgelöst — der Zustand sind
//! zwei Listen von Ids, die auch nach einer Panik gültig bleiben
//! (fail-closed: ein einmal gesetzter Taint geht dabei nicht verloren). Der
//! Prüfkontext ist ein Tokio-Task-Local und gilt genau für das Future, das
//! [`with_review_taint`] umschließt.
//!
//! # Fehler
//! Keine: alle Funktionen sind total.
//!
//! # Examples
//! ```rust
//! use harw_core::dod_taint::{DodTaint, is_taint_gated};
//! use harw_types::TurnId;
//!
//! let taint = DodTaint::new();
//! let turn = TurnId::new();
//! assert!(!taint.is_tainted_in(&turn));
//! assert!(taint.note_dispatch(&turn, "dod.findings"));
//! assert!(taint.is_tainted_in(&turn));
//! assert!(!taint.is_tainted_in(&TurnId::new()));
//! assert!(is_taint_gated("job.stop"));
//! assert!(!is_taint_gated("job.logs"));
//! ```

use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use harw_tools::ToolCall;
use harw_types::{ToolCallId, TurnId};

use crate::session::AgentSession;

/// Namenspräfix der DoD-Werkzeuge, deren Abfertigung einen Turn taintet.
///
/// Deckt die drei Root-Werkzeuge (`dod.status`, `dod.findings`,
/// `dod.sensors`) und die Triage-Werkzeuge aus Phase 1.2 (`dod.finding`,
/// `dod.related`, `dod.labels`) ab, ohne sie einzeln aufzuzählen.
pub const DOD_TOOL_PREFIX: &str = "dod.";

/// Namenspräfix der Job-Werkzeuge (`harw-tool-job`).
pub const JOB_TOOL_PREFIX: &str = "job.";

/// Die rein lesenden Job-Werkzeuge; der Taint lässt sie unberührt.
///
/// # Beschreibung
/// Muss genau `harw_tool_job::JOB_READ_TOOLS` entsprechen (`harw-core`
/// hängt nicht an `harw-tool-job`; der Abgleich steht als Test in
/// `harw-runtime/src/permission_rules.rs`). Jedes andere `job.*`-Werkzeug —
/// heute `job.start` und `job.stop`, nach Phase 3.5 auch `job.suspend` und
/// `job.resume` — gilt als verändernd und ist taint-pflichtig (fail-closed:
/// ein neues, hier nicht gelistetes Job-Werkzeug fragt nach).
pub const JOB_READ_ONLY_TOOLS: &[&str] = &["job.status", "job.logs", "job.list"];

/// Host-Eingriffe, die nach einer DoD-Abfrage immer nachfragen.
///
/// Beide stehen zusätzlich in `harw_registry_defaults::ALWAYS_ASK_TOOLS`
/// und fragen unter `ask`/`auto` deshalb ohnehin; nach einer DoD-Abfrage
/// erzwingt der Taint die Rückfrage auch in Freigabeketten ohne diese
/// Standardpolitik.
pub const TAINT_GATED_HOST_TOOLS: &[&str] = &["process.kill", "host.sudo_exec"];

/// Der Grund der Rückfrage, wie ihn die Oberfläche anzeigt.
pub const TAINT_REASON: &str = "nach DoD-Abfrage in diesem Turn";

/// Höchstlänge (Zeichen) eines Werkzeugnamens in Grundtext und Log.
const MAX_TOOL_NAME_CHARS: usize = 64;

/// Wie viele getaintete Turns ein [`DodTaint`] höchstens behält.
///
/// Ein Steuerblock gehört normalerweise genau einem Turn (höchstens ein
/// Eintrag). Die Grenze hält den Speicher auch dann klein, wenn ein Aufrufer
/// denselben Steuerblock über viele Turns wiederverwendet; verdrängt wird
/// der älteste, also ein längst beendeter Turn.
const MAX_TRACKED_TURNS: usize = 16;

/// Wie viele vom Taint verursachte Rückfragen ein [`DodTaint`] höchstens
/// behält.
///
/// Ein Turn pausiert bei der ersten Rückfrage; mehr als eine Handvoll
/// Einträge entsteht nur bei einem über viele Turns wiederverwendeten
/// Steuerblock. Verdrängt wird der älteste Eintrag.
const MAX_TRACKED_ASKS: usize = 16;

tokio::task_local! {
    // Ob der gerade geprüfte Werkzeugaufruf zu einem getainteten Turn
    // gehört; gesetzt von `with_review_taint` um die Handler-Befragung.
    static REVIEW_TAINT: bool;
}

/// Ob `name` ein DoD-Werkzeug ist, dessen Abfertigung den Turn taintet.
///
/// # Arguments
/// - `name` (`&str`): Werkzeugname des Aufrufs.
///
/// # Returns
/// `true` genau für Namen mit dem Präfix [`DOD_TOOL_PREFIX`].
#[must_use]
pub fn is_dod_tool(name: &str) -> bool {
    name.starts_with(DOD_TOOL_PREFIX)
}

/// Ob `name` nach einer DoD-Abfrage im selben Turn ausdrücklich gefragt
/// werden muss.
///
/// # Arguments
/// - `name` (`&str`): Werkzeugname des Aufrufs.
///
/// # Returns
/// `true` für [`TAINT_GATED_HOST_TOOLS`] und für jedes `job.*`-Werkzeug
/// außerhalb von [`JOB_READ_ONLY_TOOLS`]; sonst `false`.
///
/// # Examples
/// ```rust
/// use harw_core::dod_taint::is_taint_gated;
///
/// assert!(is_taint_gated("process.kill"));
/// assert!(is_taint_gated("job.suspend"));
/// assert!(!is_taint_gated("job.status"));
/// assert!(!is_taint_gated("dod.findings"));
/// ```
#[must_use]
pub fn is_taint_gated(name: &str) -> bool {
    TAINT_GATED_HOST_TOOLS.contains(&name)
        || (name.starts_with(JOB_TOOL_PREFIX) && !JOB_READ_ONLY_TOOLS.contains(&name))
}

/// Ob eine Modellantwort einen `dod.*`-Aufruf mit einem taint-pflichtigen
/// Aufruf mischt.
///
/// # Beschreibung
/// Der Taint entsteht erst, wenn der `dod.*`-Aufruf abgefertigt ist. Die
/// Parallel-Vorprüfung prüft aber alle Aufrufe einer Antwort **vor** jeder
/// Ausführung; eine solche Antwort muss deshalb sequenziell laufen, damit
/// der taint-pflichtige Aufruf nach dem DoD-Aufruf geprüft wird.
///
/// # Arguments
/// - `calls` (`&[ToolCall]`): die Aufrufe der Modellantwort.
///
/// # Returns
/// `true`, wenn mindestens ein `dod.*`-Aufruf und mindestens ein
/// taint-pflichtiger Aufruf darin stehen.
#[must_use]
pub fn batch_mixes_dod_and_gated(calls: &[ToolCall]) -> bool {
    calls.iter().any(|call| is_dod_tool(call.name.as_str()))
        && calls.iter().any(|call| is_taint_gated(call.name.as_str()))
}

/// Der vollständige Grundtext einer Taint-Rückfrage für `tool`.
///
/// # Arguments
/// - `tool` (`&str`): Werkzeugname aus dem Aufruf. Er stammt vom Modell und
///   wird deshalb mit `harw_untrusted_text::escape_line` einzeilig escapt
///   und gekürzt.
///
/// # Returns
/// Z. B. `'job.stop' braucht eine ausdrückliche Freigabe: nach DoD-Abfrage
/// in diesem Turn`.
#[must_use]
pub fn ask_reason(tool: &str) -> String {
    format!(
        "'{}' braucht eine ausdrückliche Freigabe: {TAINT_REASON}",
        display_tool_name(tool)
    )
}

/// Der Grundtext einer Ablehnung, wenn der Taint fragen müsste, aber
/// niemand antworten kann.
///
/// # Beschreibung
/// `check_approval` lehnt einen taint-pflichtigen Aufruf damit ab, statt eine
/// Rückfrage zu öffnen, die keine Person beantworten kann (Sitzung ohne
/// Freigabe-Akteur, nicht interaktiver Einstieg). Der Text geht als
/// `denied: …` an das Modell; es kann ohne den Eingriff weiterarbeiten.
///
/// # Arguments
/// - `tool` (`&str`): Werkzeugname aus dem Aufruf; wie in [`ask_reason`]
///   escapt und gekürzt.
///
/// # Returns
/// [`ask_reason`] plus den Hinweis, dass hier niemand freigeben kann.
#[must_use]
pub fn unanswerable_reason(tool: &str) -> String {
    format!(
        "{}; hier kann niemand freigeben, der Aufruf wird abgelehnt",
        ask_reason(tool)
    )
}

/// Der escapte, gekürzte Werkzeugname für Grundtext und Log.
pub(crate) fn display_tool_name(tool: &str) -> String {
    harw_untrusted_text::escape_line(tool, MAX_TOOL_NAME_CHARS)
}

// Der geteilte Zustand eines [`DodTaint`].
#[derive(Default)]
struct TaintState {
    // Getaintete Turns, älteste zuerst; höchstens `MAX_TRACKED_TURNS`.
    turns: Vec<TurnId>,
    // Rückfragen, die der Taint verursacht hat, als (Turn, Call), älteste
    // zuerst; höchstens `MAX_TRACKED_ASKS`.
    asks: Vec<(TurnId, ToolCallId)>,
}

/// Der DoD-Taint eines Turns: welche Turns ein `dod.*`-Werkzeug abgefertigt
/// haben und bei welchen Aufrufen der Taint eine Rückfrage verursacht hat.
///
/// # Beschreibung
/// Liegt im [`crate::turn_loop::TurnControl`] eines Turns; Klone teilen den
/// Zustand. Geschlüsselt nach [`TurnId`], damit ein wiederverwendeter
/// Steuerblock den Taint nie in einen späteren Turn trägt.
///
/// # Nebenläufigkeit
/// `Send + Sync`; jede Methode nimmt kurz einen `Mutex`.
#[derive(Clone, Default)]
pub struct DodTaint {
    // Geteilter Zustand; Klone sehen denselben Stand.
    state: Arc<Mutex<TaintState>>,
}

impl std::fmt::Debug for DodTaint {
    /// Zeigt nur Anzahlen, keine Ids.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state();
        f.debug_struct("DodTaint")
            .field("tainted_turns", &state.turns.len())
            .field("taint_asks", &state.asks.len())
            .finish()
    }
}

impl DodTaint {
    /// Erzeugt einen Zustand ohne getainteten Turn.
    ///
    /// # Returns
    /// Ein frischer [`DodTaint`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // Ein vergifteter Lock hält nur Ids: der letzte Stand bleibt gültig.
    fn state(&self) -> MutexGuard<'_, TaintState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Hält fest, dass `turn` das Werkzeug `tool` abgefertigt hat.
    ///
    /// # Beschreibung
    /// Nur ein `dod.*`-Werkzeug ([`is_dod_tool`]) taintet; jedes andere
    /// lässt den Zustand unverändert. Ergebnis und Erfolg des Aufrufs spielen
    /// keine Rolle.
    ///
    /// # Arguments
    /// - `turn` (`&TurnId`): der laufende Turn.
    /// - `tool` (`&str`): Name des abgefertigten Werkzeugs.
    ///
    /// # Returns
    /// `true`, wenn `tool` ein DoD-Werkzeug ist (der Turn ist danach
    /// getaintet), sonst `false`.
    pub fn note_dispatch(&self, turn: &TurnId, tool: &str) -> bool {
        if !is_dod_tool(tool) {
            return false;
        }
        let mut state = self.state();
        if !state.turns.contains(turn) {
            state.turns.push(turn.clone());
            let excess = state.turns.len().saturating_sub(MAX_TRACKED_TURNS);
            if excess > 0 {
                state.turns.drain(..excess);
            }
        }
        true
    }

    /// Ob `turn` ein `dod.*`-Werkzeug abgefertigt hat.
    ///
    /// # Arguments
    /// - `turn` (`&TurnId`): der zu prüfende Turn.
    ///
    /// # Returns
    /// `true` genau dann, wenn [`Self::note_dispatch`] für diesen Turn mit
    /// einem DoD-Werkzeug aufgerufen wurde.
    #[must_use]
    pub fn is_tainted_in(&self, turn: &TurnId) -> bool {
        self.state().turns.contains(turn)
    }

    /// Hält fest, dass der Taint die Rückfrage für `call` in `turn`
    /// verursacht hat.
    ///
    /// # Beschreibung
    /// `check_approval` ruft das genau dann, wenn die
    /// [`crate::policy::DodTaintPolicy`] (oder ihr Fallback) für `call`
    /// `AskUser` geliefert hat und das Ergebnis eine Rückfrage ist.
    /// [`pending_ask_reason`] zeigt den Grund nur für so vermerkte Aufrufe.
    ///
    /// # Arguments
    /// - `turn` (`&TurnId`): der laufende Turn.
    /// - `call` (`&ToolCallId`): der Aufruf, auf dessen Freigabe der Turn
    ///   wartet.
    ///
    /// # Returns
    /// `true`, wenn vermerkt wurde; `false`, wenn `turn` gar nicht getaintet
    /// ist (dann kann der Taint keine Rückfrage verursacht haben).
    pub fn note_ask(&self, turn: &TurnId, call: &ToolCallId) -> bool {
        let mut state = self.state();
        if !state.turns.contains(turn) {
            return false;
        }
        let already = state
            .asks
            .iter()
            .any(|(asked_turn, asked_call)| asked_turn == turn && asked_call == call);
        if !already {
            state.asks.push((turn.clone(), call.clone()));
            let excess = state.asks.len().saturating_sub(MAX_TRACKED_ASKS);
            if excess > 0 {
                state.asks.drain(..excess);
            }
        }
        true
    }

    /// Ob der Taint die Rückfrage für `call` in `turn` verursacht hat.
    ///
    /// # Arguments
    /// - `turn` (`&TurnId`): der Turn des Aufrufs.
    /// - `call` (`&ToolCallId`): der Aufruf.
    ///
    /// # Returns
    /// `true` genau dann, wenn [`Self::note_ask`] für dieses Paar Erfolg
    /// meldete (und der Eintrag noch nicht verdrängt ist).
    #[must_use]
    pub fn asked_in(&self, turn: &TurnId, call: &ToolCallId) -> bool {
        self.state()
            .asks
            .iter()
            .any(|(asked_turn, asked_call)| asked_turn == turn && asked_call == call)
    }
}

/// Führt `review` mit dem Taint-Stand des geprüften Aufrufs im Prüfkontext
/// aus.
///
/// # Beschreibung
/// `check_approval` umschließt damit die Befragung aller Freigabe-Handler.
/// Ein Handler liest den Stand über [`review_turn_tainted`]; er braucht dafür
/// weder die Session noch den Turn. Der Kontext gilt genau für `review` und
/// endet mit ihm.
///
/// # Arguments
/// - `tainted` (`bool`): ob der Turn des geprüften Aufrufs getaintet ist.
/// - `review` (`F`): das Future der Befragung.
///
/// # Returns
/// Das Ergebnis von `review`.
///
/// # Concurrency
/// Tokio-Task-Local: nur Code, der innerhalb von `review` gepollt wird,
/// sieht den Wert; ein aus `review` heraus gestarteter Task nicht.
pub async fn with_review_taint<F>(tainted: bool, review: F) -> F::Output
where
    F: Future,
{
    REVIEW_TAINT.scope(tainted, review).await
}

/// Ob der gerade geprüfte Aufruf zu einem getainteten Turn gehört.
///
/// # Returns
/// Den Wert aus [`with_review_taint`]; `false` außerhalb eines solchen
/// Kontexts (z. B. wenn ein Handler außerhalb von `check_approval` befragt
/// wird).
#[must_use]
pub fn review_turn_tainted() -> bool {
    REVIEW_TAINT.try_with(|tainted| *tainted).unwrap_or(false)
}

/// Ob der aktuelle Turn von `session` getaintet ist.
///
/// # Arguments
/// - `session` (`&AgentSession`): die Sitzung; gelesen werden ihr laufender
///   Turn und dessen hinterlegter Steuerblock.
///
/// # Returns
/// `false` ohne laufenden Turn oder ohne hinterlegten Steuerblock (eine
/// Sitzung, deren Turn nicht über `run_turn`/`run_turn_durable` begann).
#[must_use]
pub fn session_turn_tainted(session: &AgentSession) -> bool {
    match (session.active_turn_control(), session.current_turn()) {
        (Some(control), Some(turn)) => control.dod_taint().is_tainted_in(turn),
        _ => false,
    }
}

/// Meldet die Abfertigung von `tool` an den Taint des laufenden Turns.
///
/// # Arguments
/// - `session` (`&AgentSession`): die Sitzung des Turns.
/// - `tool` (`&str`): Name des abgefertigten Werkzeugs.
pub(crate) fn note_session_dispatch(session: &AgentSession, tool: &str) {
    let (Some(control), Some(turn)) = (session.active_turn_control(), session.current_turn())
    else {
        return;
    };
    if control.dod_taint().note_dispatch(turn, tool) {
        tracing::debug!(
            tool = %display_tool_name(tool),
            "dod_taint.turn_tainted"
        );
    }
}

/// Vermerkt, dass der Taint die Rückfrage für `call` im laufenden Turn von
/// `session` verursacht hat (siehe [`DodTaint::note_ask`]).
///
/// # Arguments
/// - `session` (`&AgentSession`): die Sitzung des Turns.
/// - `call` (`&ToolCallId`): der Aufruf, für den gefragt wird.
///
/// # Returns
/// `true`, wenn vermerkt wurde; `false` ohne laufenden Turn, ohne
/// hinterlegten Steuerblock oder ohne Taint.
pub(crate) fn note_session_ask(session: &AgentSession, call: &ToolCallId) -> bool {
    match (session.active_turn_control(), session.current_turn()) {
        (Some(control), Some(turn)) => control.dod_taint().note_ask(turn, call),
        _ => false,
    }
}

/// Der Taint-Grund der offenen Rückfrage von `session`, falls es einen gibt.
///
/// # Beschreibung
/// Für Oberflächen (TUI-Freigabedialog, Kanäle): liefert den Text aus
/// [`ask_reason`], wenn die Sitzung auf eine Freigabe wartet, die der Taint
/// verursacht hat ([`DodTaint::note_ask`]). Hat ein anderer Handler gefragt
/// (etwa unter Full Access, wo die Taint-Regel nicht gilt), bleibt es bei
/// `None` — die Oberfläche zeigt dann ihren eigenen Grund.
///
/// # Arguments
/// - `session` (`&AgentSession`): die pausierte Sitzung.
///
/// # Returns
/// `Some(grund)` oder `None` (keine offene Rückfrage, kein laufender Turn
/// oder eine Rückfrage, die der Taint nicht verursacht hat).
#[must_use]
pub fn pending_ask_reason(session: &AgentSession) -> Option<String> {
    let pending = session.pending_approval()?;
    let control = session.active_turn_control()?;
    let turn = session.current_turn()?;
    control
        .dod_taint()
        .asked_in(turn, &pending.call.id)
        .then(|| ask_reason(pending.call.name.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};
    use harw_tools::ToolName;

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: ToolCallId::new(),
            name: ToolName::new(name),
            arguments: serde_json::Value::Null,
        }
    }

    /// Alle heutigen und geplanten DoD-Werkzeuge lösen aus, nichts sonst.
    #[test]
    fn every_dod_tool_triggers_and_nothing_else() -> TestResult {
        for tool in [
            "dod.status",
            "dod.findings",
            "dod.sensors",
            "dod.finding",
            "dod.related",
            "dod.labels",
        ] {
            assert!(is_dod_tool(tool), "{tool}");
        }
        for tool in ["dodge", "dod", "job.stop", "process.list", "fs.read"] {
            assert!(!is_dod_tool(tool), "{tool}");
        }
        Ok(())
    }

    /// Host-Eingriffe und jedes verändernde `job.*` sind taint-pflichtig;
    /// lesende Job-Werkzeuge und alles andere nicht.
    #[test]
    fn gated_set_is_host_tools_plus_mutating_job_tools() -> TestResult {
        for tool in [
            "process.kill",
            "host.sudo_exec",
            "job.start",
            "job.stop",
            "job.suspend",
            "job.resume",
            "job.some_future_mutation",
        ] {
            assert!(is_taint_gated(tool), "{tool}");
        }
        for tool in [
            "job.status",
            "job.logs",
            "job.list",
            "dod.findings",
            "process.list",
            "shell.exec",
            "fs.read",
        ] {
            assert!(!is_taint_gated(tool), "{tool}");
        }
        Ok(())
    }

    /// Der Taint gilt nur für den Turn, in dem der DoD-Aufruf lief; ein
    /// anderes Werkzeug taintet nie.
    #[test]
    fn taint_is_keyed_by_turn_and_set_only_by_dod_tools() -> TestResult {
        let taint = DodTaint::new();
        let turn = TurnId::new();
        assert!(!taint.note_dispatch(&turn, "job.logs"));
        assert!(!taint.is_tainted_in(&turn));
        assert!(taint.note_dispatch(&turn, "dod.findings"));
        assert!(taint.is_tainted_in(&turn));
        let next_turn = TurnId::new();
        assert!(
            !taint.is_tainted_in(&next_turn),
            "nächster Turn bleibt sauber"
        );
        Ok(())
    }

    /// Klone teilen den Zustand (der Steuerblock wird über Pausen geklont).
    #[test]
    fn clones_share_the_taint() -> TestResult {
        let taint = DodTaint::new();
        let clone = taint.clone();
        let turn = TurnId::new();
        clone.note_dispatch(&turn, "dod.status");
        assert!(taint.is_tainted_in(&turn));
        Ok(())
    }

    /// Ein wiederverwendeter Steuerblock behält nur die jüngsten Turns.
    #[test]
    fn tracking_is_bounded_and_keeps_the_newest_turns() -> TestResult {
        let taint = DodTaint::new();
        let turns: Vec<TurnId> = (0..MAX_TRACKED_TURNS.saturating_add(4))
            .map(|_| TurnId::new())
            .collect();
        for turn in &turns {
            taint.note_dispatch(turn, "dod.findings");
        }
        let newest = turns.last().ok_or(TestError::Missing("jüngster Turn"))?;
        let oldest = turns.first().ok_or(TestError::Missing("ältester Turn"))?;
        assert!(taint.is_tainted_in(newest));
        assert!(!taint.is_tainted_in(oldest));
        assert_eq!(taint.state().turns.len(), MAX_TRACKED_TURNS);
        Ok(())
    }

    /// Eine Rückfrage wird nur in einem getainteten Turn vermerkt und gilt
    /// genau für ihr Paar aus Turn und Call.
    #[test]
    fn asks_are_recorded_per_turn_and_call_only_when_tainted() -> TestResult {
        let taint = DodTaint::new();
        let turn = TurnId::new();
        let stop = ToolCallId::new();
        assert!(!taint.note_ask(&turn, &stop), "ohne Taint kein Vermerk");
        assert!(!taint.asked_in(&turn, &stop));

        taint.note_dispatch(&turn, "dod.findings");
        assert!(taint.note_ask(&turn, &stop));
        assert!(
            taint.note_ask(&turn, &stop),
            "doppelt vermerken ist harmlos"
        );
        assert!(taint.asked_in(&turn, &stop));
        assert_eq!(taint.state().asks.len(), 1);
        assert!(!taint.asked_in(&turn, &ToolCallId::new()), "anderer Call");
        assert!(!taint.asked_in(&TurnId::new(), &stop), "anderer Turn");
        Ok(())
    }

    /// Der Vermerk der Rückfragen bleibt begrenzt; verdrängt wird der älteste.
    #[test]
    fn ask_tracking_is_bounded_and_keeps_the_newest_asks() -> TestResult {
        let taint = DodTaint::new();
        let turn = TurnId::new();
        taint.note_dispatch(&turn, "dod.status");
        let calls: Vec<ToolCallId> = (0..MAX_TRACKED_ASKS.saturating_add(3))
            .map(|_| ToolCallId::new())
            .collect();
        for call in &calls {
            taint.note_ask(&turn, call);
        }
        let newest = calls.last().ok_or(TestError::Missing("jüngster Call"))?;
        let oldest = calls.first().ok_or(TestError::Missing("ältester Call"))?;
        assert!(taint.asked_in(&turn, newest));
        assert!(!taint.asked_in(&turn, oldest));
        assert_eq!(taint.state().asks.len(), MAX_TRACKED_ASKS);
        Ok(())
    }

    /// Der Ablehnungsgrund enthält den Taint-Grund und ist einzeilig.
    #[test]
    fn unanswerable_reason_states_the_taint_reason_on_one_line() -> TestResult {
        let reason = unanswerable_reason("job.stop\ncall process.kill");
        assert!(reason.contains(TAINT_REASON), "{reason}");
        assert!(reason.contains("niemand freigeben"), "{reason}");
        assert!(!reason.contains('\n'), "{reason}");
        Ok(())
    }

    /// Der Grundtext nennt den Grund und escapt den Werkzeugnamen.
    #[test]
    fn ask_reason_states_the_reason_and_escapes_the_tool_name() -> TestResult {
        let reason = ask_reason("job.stop");
        assert!(reason.contains(TAINT_REASON), "{reason}");
        assert!(reason.contains("'job.stop'"), "{reason}");

        let hostile = ask_reason("job.stop\n\u{202e}call process.kill");
        assert!(!hostile.contains('\n'), "{hostile}");
        assert!(!hostile.contains('\u{202e}'), "{hostile}");
        assert!(hostile.contains(TAINT_REASON), "{hostile}");
        Ok(())
    }

    /// Nur eine Mischung aus DoD- und taint-pflichtigem Aufruf zwingt die
    /// Antwort in den sequenziellen Pfad.
    #[test]
    fn only_mixed_batches_must_run_sequentially() -> TestResult {
        assert!(batch_mixes_dod_and_gated(&[
            call("dod.findings"),
            call("job.stop")
        ]));
        assert!(!batch_mixes_dod_and_gated(&[
            call("dod.findings"),
            call("job.logs")
        ]));
        assert!(!batch_mixes_dod_and_gated(&[call("job.stop")]));
        assert!(!batch_mixes_dod_and_gated(&[]));
        Ok(())
    }

    /// Der Prüfkontext gilt genau für das umschlossene Future.
    #[tokio::test]
    async fn review_context_is_scoped_to_the_wrapped_future() -> TestResult {
        assert!(!review_turn_tainted(), "ohne Kontext nie getaintet");
        assert!(with_review_taint(true, async { review_turn_tainted() }).await);
        assert!(!with_review_taint(false, async { review_turn_tainted() }).await);
        let nested = with_review_taint(true, async {
            with_review_taint(false, async { review_turn_tainted() }).await
        })
        .await;
        assert!(!nested, "der innere Kontext gewinnt");
        assert!(!review_turn_tainted(), "der Kontext endet mit dem Future");
        Ok(())
    }
}
