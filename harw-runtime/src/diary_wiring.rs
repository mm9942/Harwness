//! Automatische Diary-Einträge (Plan D3): Verdichtung und Sitzungsende.
//!
//! # Verantwortung
//! [`DiaryRecorder`] schreibt über `harw_knowledge::diary::record` in das
//! Tagebuch des Agenten einer Sitzung:
//! - **`compaction`** — nach einer Verdichtung mit Modell-Zusammenfassung
//!   eine kurze Notiz (Anlass, Tokens vorher/nachher, Zusammenfassung). Der
//!   Kern kennt `harw-knowledge` nicht; der Weg ist der vorhandene
//!   [`CompactionObserver`]-Haken. Weil `AgentSession` genau einen Observer
//!   trägt (heute [`crate::handoff::HandoffWriter`]), kettet
//!   [`DiaryRecorder::compaction_observer`] den bisherigen Observer vor.
//! - **`end-of-session`** — beim Schließen der Sitzung
//!   ([`crate::assembly::RuntimeAssembly::close_session`], TUI-Quit,
//!   `/new`/`/resume`-Wechsel, `OneShot`-Ende) über
//!   [`SessionLifecycleHook`]: Dauer, Zahl der Turns in diesem Lauf, Zahl
//!   der Verdichtungen und die letzte Zusammenfassung. Die Turns zählt
//!   [`DiaryRecorder::tool_outcome_observer`] über
//!   `ToolOutcomeObserver::on_turn_finished` (ebenfalls vorgekettet, der Slot
//!   trägt heute die Gedächtnis-Erfassung).
//!
//! Nur Sitzungen, die der Recorder kennt ([`DiaryRecorder::session_started`]
//! oder eine beobachtete Runde/Verdichtung), bekommen einen
//! Sitzungsende-Eintrag, und nur, wenn in ihnen etwas geschah — eine sofort
//! wieder geschlossene Sitzung erzeugt keinen Eintrag.
//!
//! # Fehler
//! Best-effort wie der Handoff: ein Schreibfehler wird nur mit
//! `tracing::warn!` gemeldet und bricht weder Verdichtung noch Sitzungsende.
//!
//! # Nebenläufigkeit
//! `Send + Sync`; der Sitzungszähler liegt hinter einem `Mutex`, die
//! Dateischreibvorgänge serialisiert `harw_knowledge::diary` prozessweit.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use harw_core::AgentEventHub;
use harw_core::capture::{ToolOutcome, ToolOutcomeObserver};
use harw_core::compaction::{CompactionObserver, CompactionOutcome};
use harw_knowledge::diary::{self, DiaryTrigger};
use harw_knowledge::{AgentId, KnowledgeStore};
use harw_types::SessionId;
use jiff::Timestamp;

use crate::assembly::SessionLifecycleHook;

/// Fläche im `AgentEventKind::Knowledge`-Event (wie `harw_ops::AREA_DIARY`).
const AREA_DIARY: &str = "diary";

/// Obergrenze der Zusammenfassung in einem Compaction-Eintrag (Bytes).
pub const COMPACTION_SUMMARY_BYTES: usize = 2 * 1024;

/// Obergrenze der letzten Zusammenfassung im Sitzungsende-Eintrag (Bytes).
pub const END_OF_SESSION_SUMMARY_BYTES: usize = 1536;

/// Uhr des Recorders (austauschbar für Tests).
pub type DiaryClock = Arc<dyn Fn() -> Timestamp + Send + Sync>;

/// Das wirksame Aufbewahrungsfenster aus `[knowledge.diary] retention_days`
/// für `harw_knowledge::diary::maintain` (Default 90).
#[must_use]
pub fn diary_retention_days(config: &harw_config::HarnessConfig) -> i64 {
    i64::from(config.knowledge.diary.effective_retention_days())
}

/// Was der Recorder je Sitzung mitzählt.
#[derive(Debug, Clone)]
struct SessionTally {
    agent: AgentId,
    started_at: Timestamp,
    turns: u32,
    compactions: u32,
    last_summary: Option<String>,
}

/// Schreibt automatische Diary-Einträge (siehe Moduldoku).
pub struct DiaryRecorder {
    store: Arc<KnowledgeStore>,
    default_agent: AgentId,
    events: Option<AgentEventHub>,
    clock: DiaryClock,
    sessions: Mutex<HashMap<SessionId, SessionTally>>,
}

impl std::fmt::Debug for DiaryRecorder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiaryRecorder")
            .field("store", &self.store.root())
            .field("default_agent", &self.default_agent)
            .finish_non_exhaustive()
    }
}

impl DiaryRecorder {
    /// Baut einen Recorder.
    ///
    /// # Argumente
    /// - `store` (`Arc<KnowledgeStore>`): `RuntimeServices::knowledge_store`.
    /// - `default_agent` (`AgentId`): Agent für Sitzungen ohne eigene Angabe
    ///   in [`Self::session_started`] — in der Montage der Wurzel-Agent
    ///   (aktive UIA-Definition bzw. `agent_name`); dieselbe Id bekommt
    ///   `DiaryToolProvider::new`.
    #[must_use]
    pub fn new(store: Arc<KnowledgeStore>, default_agent: AgentId) -> Self {
        Self {
            store,
            default_agent,
            events: None,
            clock: Arc::new(Timestamp::now),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Meldet jeden Schreibvorgang als `AgentEventKind::Knowledge
    /// { area: "diary" }` über den Hub (Live-Update der TUI).
    #[must_use]
    pub fn with_agent_events(mut self, hub: AgentEventHub) -> Self {
        self.events = Some(hub);
        self
    }

    /// Ersetzt die Uhr (Tests).
    #[must_use]
    pub fn with_clock(mut self, clock: DiaryClock) -> Self {
        self.clock = clock;
        self
    }

    /// Registriert den Beginn einer Sitzung (Startzeit für die Dauer).
    ///
    /// # Argumente
    /// - `agent` — Agent der Sitzung; `None` → Vorgabe-Agent.
    pub fn session_started(&self, id: &SessionId, agent: Option<AgentId>) {
        let now = (self.clock)();
        let agent = agent.unwrap_or_else(|| self.default_agent.clone());
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(
                id.clone(),
                SessionTally {
                    agent,
                    started_at: now,
                    turns: 0,
                    compactions: 0,
                    last_summary: None,
                },
            );
        }
    }

    /// Zählt eine abgeschlossene Runde.
    pub fn turn_finished(&self, id: &SessionId) {
        self.with_tally(id, |tally| tally.turns = tally.turns.saturating_add(1));
    }

    /// Verarbeitet eine abgeschlossene Verdichtung: merkt die
    /// Zusammenfassung und schreibt, falls es eine gibt, einen
    /// `compaction`-Eintrag. No-op-Läufe und rein deterministische
    /// Verdichtungen schreiben nichts.
    pub fn compacted(&self, id: &SessionId, outcome: &CompactionOutcome) {
        if outcome.no_op {
            return;
        }
        let summary = outcome
            .summary_text
            .as_deref()
            .map(str::trim)
            .filter(|summary| outcome.summarized && !summary.is_empty())
            .map(str::to_owned);
        let agent = self.with_tally(id, |tally| {
            tally.compactions = tally.compactions.saturating_add(1);
            if summary.is_some() {
                tally.last_summary.clone_from(&summary);
            }
            tally.agent.clone()
        });
        let Some(summary) = summary else {
            return;
        };
        let reason = outcome
            .reason
            .as_ref()
            .map(|reason| format!("{reason:?}"))
            .unwrap_or_else(|| "manuell".to_owned());
        let text = format!(
            "Verdichtung ({reason}): ~{} → ~{} Tokens{}.\n\n{}",
            outcome.tokens_before,
            outcome.tokens_after,
            if outcome.summary_truncated {
                ", Zusammenfassung abgeschnitten"
            } else {
                ""
            },
            diary::truncate_entry_text(&summary, COMPACTION_SUMMARY_BYTES)
        );
        self.write(id, &agent, DiaryTrigger::Compaction, &text);
    }

    /// Schreibt den `end-of-session`-Eintrag und vergisst die Sitzung.
    pub fn session_closed(&self, id: &SessionId) {
        let tally = self
            .sessions
            .lock()
            .ok()
            .and_then(|mut sessions| sessions.remove(id));
        let Some(tally) = tally else {
            return;
        };
        if tally.turns == 0 && tally.compactions == 0 {
            return;
        }
        let now = (self.clock)();
        let mut text = format!(
            "Sitzung {id} beendet nach {} ({} Turns, {} Verdichtungen).",
            format_duration(now.duration_since(tally.started_at)),
            tally.turns,
            tally.compactions
        );
        if let Some(summary) = tally.last_summary.as_deref() {
            text.push_str("\n\nLetzte Zusammenfassung:\n");
            text.push_str(&diary::truncate_entry_text(
                summary,
                END_OF_SESSION_SUMMARY_BYTES,
            ));
        }
        self.write(id, &tally.agent, DiaryTrigger::EndOfSession, &text);
    }

    /// Ein [`CompactionObserver`], der zuerst `inner` (z. B. den
    /// `HandoffWriter`) und dann [`Self::compacted`] aufruft.
    #[must_use]
    pub fn compaction_observer(
        self: &Arc<Self>,
        inner: Option<Arc<dyn CompactionObserver>>,
    ) -> Arc<dyn CompactionObserver> {
        Arc::new(DiaryCompactionObserver {
            recorder: Arc::clone(self),
            inner,
        })
    }

    /// Ein [`ToolOutcomeObserver`], der alles an `inner` (z. B. die
    /// Gedächtnis-Erfassung) weiterreicht und Runden zählt.
    #[must_use]
    pub fn tool_outcome_observer(
        self: &Arc<Self>,
        inner: Option<Arc<dyn ToolOutcomeObserver>>,
    ) -> Arc<dyn ToolOutcomeObserver> {
        Arc::new(DiaryTurnCounter {
            recorder: Arc::clone(self),
            inner,
        })
    }

    /// Wendet `update` auf den Zähler der Sitzung an; eine unbekannte
    /// Sitzung wird mit Vorgabe-Agent und jetziger Startzeit angelegt.
    fn with_tally<R>(&self, id: &SessionId, update: impl FnOnce(&mut SessionTally) -> R) -> R {
        let mut fallback = SessionTally {
            agent: self.default_agent.clone(),
            started_at: (self.clock)(),
            turns: 0,
            compactions: 0,
            last_summary: None,
        };
        match self.sessions.lock() {
            Ok(mut sessions) => update(sessions.entry(id.clone()).or_insert(fallback)),
            Err(_) => update(&mut fallback),
        }
    }

    fn write(&self, id: &SessionId, agent: &AgentId, trigger: DiaryTrigger, text: &str) {
        let now = (self.clock)();
        match diary::record(&self.store, agent, trigger, text, now) {
            Ok(_) => {
                if let Some(hub) = &self.events {
                    hub.publish_knowledge(
                        id.clone(),
                        AREA_DIARY,
                        Some(format!("{agent}/{}", now.strftime("%Y-%m-%d"))),
                    );
                }
            }
            Err(error) => tracing::warn!(
                session_id = %id,
                agent = %agent,
                trigger = trigger.label(),
                %error,
                "diary.auto_entry.failed"
            ),
        }
    }
}

impl SessionLifecycleHook for DiaryRecorder {
    fn on_session_closed(&self, id: &SessionId) {
        self.session_closed(id);
    }
}

/// Kette „bisheriger Observer, dann Diary" für den Verdichtungs-Slot.
struct DiaryCompactionObserver {
    recorder: Arc<DiaryRecorder>,
    inner: Option<Arc<dyn CompactionObserver>>,
}

impl CompactionObserver for DiaryCompactionObserver {
    fn on_compacted(&self, session_id: &SessionId, outcome: &CompactionOutcome) {
        if let Some(inner) = &self.inner {
            inner.on_compacted(session_id, outcome);
        }
        self.recorder.compacted(session_id, outcome);
    }
}

/// Kette „bisheriger Observer, dann Turn-Zähler" für den Werkzeug-Slot.
struct DiaryTurnCounter {
    recorder: Arc<DiaryRecorder>,
    inner: Option<Arc<dyn ToolOutcomeObserver>>,
}

impl ToolOutcomeObserver for DiaryTurnCounter {
    fn on_tool_outcome(&self, session_id: &SessionId, outcome: &ToolOutcome<'_>) {
        if let Some(inner) = &self.inner {
            inner.on_tool_outcome(session_id, outcome);
        }
    }

    fn on_turn_finished(&self, session_id: &SessionId) {
        if let Some(inner) = &self.inner {
            inner.on_turn_finished(session_id);
        }
        self.recorder.turn_finished(session_id);
    }
}

/// `1h 05m`, `3m 07s` oder `42s`.
fn format_duration(duration: jiff::SignedDuration) -> String {
    let total = duration.as_secs().max(0);
    let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicI64, Ordering};

    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn temporary_store(label: &str) -> TestResult<Arc<KnowledgeStore>> {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(ctx("system clock is after epoch"))?
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "harw-runtime-diary-{label}-{}-{nonce}",
            std::process::id()
        ));
        Ok(Arc::new(KnowledgeStore::new(&root)))
    }

    /// Eine Uhr, die man vorstellen kann (Sekunden seit Epoche).
    fn manual_clock(start: i64) -> (Arc<AtomicI64>, DiaryClock) {
        let seconds = Arc::new(AtomicI64::new(start));
        let reader = Arc::clone(&seconds);
        let clock: DiaryClock = Arc::new(move || {
            Timestamp::from_second(reader.load(Ordering::SeqCst)).unwrap_or(Timestamp::UNIX_EPOCH)
        });
        (seconds, clock)
    }

    fn summarized(text: &str) -> CompactionOutcome {
        CompactionOutcome {
            tokens_before: 90_000,
            tokens_after: 12_000,
            summarized: true,
            summary_text: Some(text.to_owned()),
            ..CompactionOutcome::default()
        }
    }

    fn entries(
        store: &KnowledgeStore,
        agent: &str,
        at: i64,
    ) -> TestResult<Vec<diary::DiaryRecord>> {
        let date = Timestamp::from_second(at)
            .map_err(ctx("valid timestamp"))?
            .strftime("%Y-%m-%d")
            .to_string();
        Ok(diary::read_day_entries(store, &AgentId::new(agent), &date)
            .map_err(ctx("read day"))?
            .map(|day| day.entries)
            .unwrap_or_default())
    }

    struct CountingInner(AtomicI64);

    impl CompactionObserver for CountingInner {
        fn on_compacted(&self, _: &SessionId, _: &CompactionOutcome) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl ToolOutcomeObserver for CountingInner {
        fn on_tool_outcome(&self, _: &SessionId, _: &ToolOutcome<'_>) {}

        fn on_turn_finished(&self, _: &SessionId) {
            self.0.fetch_add(10, Ordering::SeqCst);
        }
    }

    #[test]
    fn compaction_with_summary_writes_an_entry_and_chains_the_inner_observer() -> TestResult {
        let store = temporary_store("compaction")?;
        let start = 1_758_715_200;
        let (_, clock) = manual_clock(start);
        let recorder = Arc::new(
            DiaryRecorder::new(Arc::clone(&store), AgentId::new("explorer")).with_clock(clock),
        );
        let inner = Arc::new(CountingInner(AtomicI64::new(0)));
        let observer = recorder.compaction_observer(Some(inner.clone()));
        let session = SessionId::from_str("s-1");

        observer.on_compacted(&session, &summarized("Wir haben X gebaut."));
        observer.on_compacted(
            &session,
            &CompactionOutcome {
                no_op: true,
                ..CompactionOutcome::default()
            },
        );
        observer.on_compacted(&session, &CompactionOutcome::default());

        assert_eq!(inner.0.load(Ordering::SeqCst), 3);
        let written = entries(&store, "explorer", start)?;
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].trigger, DiaryTrigger::Compaction);
        assert!(written[0].text.contains("Wir haben X gebaut."));
        assert!(written[0].text.contains("90000"), "{}", written[0].text);
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn session_end_records_duration_turns_and_last_summary() -> TestResult {
        let store = temporary_store("end")?;
        let start = 1_758_715_200;
        let (seconds, clock) = manual_clock(start);
        let recorder = Arc::new(
            DiaryRecorder::new(Arc::clone(&store), AgentId::new("explorer")).with_clock(clock),
        );
        let session = SessionId::from_str("s-2");
        recorder.session_started(&session, Some(AgentId::new("uia")));
        let inner = Arc::new(CountingInner(AtomicI64::new(0)));
        let turns = recorder.tool_outcome_observer(Some(inner.clone()));
        turns.on_turn_finished(&session);
        turns.on_turn_finished(&session);
        recorder.compacted(&session, &summarized("Stand: Parser fertig."));
        seconds.store(start + 3_900, Ordering::SeqCst);
        SessionLifecycleHook::on_session_closed(recorder.as_ref(), &session);

        assert_eq!(inner.0.load(Ordering::SeqCst), 20);
        let written = entries(&store, "uia", start)?;
        let end = written
            .iter()
            .find(|entry| entry.trigger == DiaryTrigger::EndOfSession)
            .ok_or(TestError::Missing("end-of-session entry"))?;
        assert!(end.text.contains("1h 05m"), "{}", end.text);
        assert!(end.text.contains("2 Turns"), "{}", end.text);
        assert!(end.text.contains("Parser fertig"), "{}", end.text);
        assert!(entries(&store, "explorer", start)?.is_empty());

        // Zweites Schließen: die Sitzung ist vergessen, kein zweiter Eintrag.
        recorder.session_closed(&session);
        assert_eq!(entries(&store, "uia", start)?.len(), written.len());
        std::fs::remove_dir_all(store.root()).ok();
        Ok(())
    }

    #[test]
    fn idle_or_unknown_sessions_write_nothing() -> TestResult {
        let store = temporary_store("idle")?;
        let start = 1_758_715_200;
        let (_, clock) = manual_clock(start);
        let recorder =
            DiaryRecorder::new(Arc::clone(&store), AgentId::new("explorer")).with_clock(clock);
        let session = SessionId::from_str("s-3");
        recorder.session_started(&session, None);
        recorder.session_closed(&session);
        recorder.session_closed(&SessionId::from_str("never-seen"));
        assert!(entries(&store, "explorer", start)?.is_empty());
        Ok(())
    }

    #[test]
    fn retention_comes_from_the_config_with_a_default() {
        let mut config = harw_config::HarnessConfig::default();
        assert_eq!(diary_retention_days(&config), 90);
        config.knowledge.diary.retention_days = Some(14);
        assert_eq!(diary_retention_days(&config), 14);
    }

    #[test]
    fn durations_are_short_and_readable() {
        assert_eq!(format_duration(jiff::SignedDuration::from_secs(42)), "42s");
        assert_eq!(
            format_duration(jiff::SignedDuration::from_secs(187)),
            "3m 07s"
        );
        assert_eq!(format_duration(jiff::SignedDuration::from_secs(-5)), "0s");
    }
}
