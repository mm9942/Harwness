//! Outcome-Zyklus über Fakten (X5): `OutcomeTracker` an der Fakten-Rückmeldung.
//!
//! Ein Fakt, der geliefert wurde, bekommt eine vorgemerkte Prüfung
//! ([`PendingOutcomeCheck`], Frist [`OUTCOME_WINDOW_DAYS`]). Beim nächsten
//! Konsolidierungslauf wird sie anhand der Rückmeldung (`used`/`corrected`,
//! siehe `FactStore::feedback`) aufgelöst:
//!
//! - genutzt (≥ [`CONFIRM_MIN_USED`]) und nie korrigiert → `Confirmed`, die
//!   Konfidenz steigt leicht ([`CONFIDENCE_STEP`], höchstens
//!   [`CONFIDENCE_CEILING`]);
//! - öfter korrigiert als genutzt → `Refuted` (Demotion übernimmt der
//!   reguläre Verfall, siehe `FactStore::decay`);
//! - Frist ohne eindeutige Rückmeldung → `Inconclusive` (`auto_close_overdue`).
//!
//! Der Zustand liegt als `outcomes.json` neben den Fakten (nur Fakt-Namen und
//! Verdikte, kein Inhalt). Eine unlesbare Datei beginnt leer (best-effort).

use std::path::{Path, PathBuf};

use time::OffsetDateTime;

use crate::epistemic::OutcomeVerdict;
use crate::facts::{FactStore, Feedback};
use crate::outcome_tracker::{OutcomeReport, OutcomeTracker, PendingOutcomeCheck};

/// Dateiname des Tracker-Zustands in der Fakten-Wurzel.
pub const OUTCOMES_FILE: &str = "outcomes.json";
/// Beobachtungsfenster bis zur automatischen Auflösung (Tage).
pub const OUTCOME_WINDOW_DAYS: i64 = 14;
/// Mindestzahl Nutzungen für `Confirmed`.
pub const CONFIRM_MIN_USED: u64 = 2;
/// Konfidenzzuwachs je bestätigtem Fakt.
pub const CONFIDENCE_STEP: f32 = 0.05;
/// Obergrenze, bis zu der Bestätigung die Konfidenz anhebt.
pub const CONFIDENCE_CEILING: f32 = 0.95;
/// So lange bleiben aufgelöste Reports in der History (Tage).
const HISTORY_MAX_AGE_DAYS: i64 = 90;

/// Zähler eines Zyklus.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OutcomeCycleReport {
    /// Neu vorgemerkte Prüfungen.
    pub tracked: usize,
    /// Als bestätigt aufgelöst.
    pub confirmed: usize,
    /// Als widerlegt aufgelöst.
    pub refuted: usize,
    /// Per Fristablauf ohne klares Ergebnis geschlossen.
    pub inconclusive: usize,
}

fn state_path(memories_root: &Path) -> PathBuf {
    memories_root.join(OUTCOMES_FILE)
}

fn load(memories_root: &Path) -> OutcomeTracker {
    std::fs::read(state_path(memories_root))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save(memories_root: &Path, tracker: &OutcomeTracker) {
    let Ok(bytes) = serde_json::to_vec(tracker) else {
        return;
    };
    if let Err(error) = std::fs::write(state_path(memories_root), bytes) {
        tracing::warn!(%error, "memory.outcomes.save_failed");
    }
}

fn decide(feedback: &Feedback) -> Option<OutcomeVerdict> {
    if feedback.corrected > feedback.used {
        Some(OutcomeVerdict::Refuted)
    } else if feedback.used >= CONFIRM_MIN_USED && feedback.corrected == 0 {
        Some(OutcomeVerdict::Confirmed)
    } else {
        None
    }
}

/// Führt einen Zyklus aus (best-effort, schreibt höchstens `outcomes.json` und
/// die Konfidenz bestätigter Fakten).
pub fn run_outcome_cycle(
    store: &FactStore,
    memories_root: &Path,
    now: OffsetDateTime,
) -> OutcomeCycleReport {
    let mut report = OutcomeCycleReport::default();
    let mut tracker = load(memories_root);
    let feedback = store.feedback_all();

    // 1. Neu gelieferte Fakten vormerken (nicht, wenn schon vorgemerkt oder
    //    in der History).
    for (name, fb) in &feedback {
        if fb.delivered == 0
            || tracker.pending.contains_key(name)
            || tracker.history.iter().any(|r| &r.signal_id == name)
        {
            continue;
        }
        tracker.track(PendingOutcomeCheck {
            signal_id: name.clone(),
            context: "geliefert in einer Sitzung".to_owned(),
            applied_at: now,
            deadline: now + time::Duration::days(OUTCOME_WINDOW_DAYS),
        });
        report.tracked += 1;
    }

    // 2. Entscheidbare Prüfungen auflösen.
    let pending_names: Vec<String> = tracker.pending.keys().cloned().collect();
    for name in pending_names {
        let Some((_, fb)) = feedback.iter().find(|(n, _)| n == &name) else {
            continue;
        };
        let Some(verdict) = decide(fb) else { continue };
        tracker.report(OutcomeReport {
            signal_id: name.clone(),
            verdict,
            note: None,
            reported_at: now,
        });
        match verdict {
            OutcomeVerdict::Confirmed => {
                report.confirmed += 1;
                bump_confidence(store, &name, now);
            }
            OutcomeVerdict::Refuted => report.refuted += 1,
            OutcomeVerdict::Inconclusive => {}
        }
    }

    // 3. Überfälliges ohne Ergebnis schließen, Altes aus der History werfen.
    report.inconclusive = tracker.auto_close_overdue(now, "Frist ohne eindeutige Rückmeldung");
    tracker.prune_history(now, HISTORY_MAX_AGE_DAYS);
    save(memories_root, &tracker);
    report
}

fn bump_confidence(store: &FactStore, name: &str, now: OffsetDateTime) {
    let Ok(Some(mut fact)) = store.read(name) else {
        return;
    };
    if fact.confidence >= CONFIDENCE_CEILING {
        return;
    }
    fact.confidence = (fact.confidence + CONFIDENCE_STEP).min(CONFIDENCE_CEILING);
    fact.updated = now;
    if let Err(error) = store.write(&fact) {
        tracing::warn!(%error, "memory.outcomes.bump_failed");
    }
}

/// Aggregierte Verdikte aus `outcomes.json` (leer, wenn die Datei fehlt).
#[must_use]
pub fn verdict_stats(memories_root: &Path) -> crate::outcome_tracker::VerdictStats {
    load(memories_root).verdict_stats()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::facts::{Fact, FactScope, FactType};
    use crate::test_support::{TestResult, ctx};

    fn store(tag: &str) -> TestResult<(FactStore, PathBuf)> {
        let root = std::env::temp_dir().join(format!("harw-outcomes-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = FactStore::open(&root, FactScope::Project).map_err(ctx("open"))?;
        let now = OffsetDateTime::now_utc();
        for name in ["good-fact", "bad-fact", "quiet-fact"] {
            store
                .write(&Fact {
                    name: name.to_owned(),
                    description: format!("Beschreibung {name}"),
                    fact_type: FactType::Preference,
                    scope: FactScope::Project,
                    created: now,
                    updated: now,
                    confidence: 0.6,
                    sources: Vec::new(),
                    tags: Vec::new(),
                    body: "x".to_owned(),
                })
                .map_err(ctx("write"))?;
        }
        Ok((store, root))
    }

    #[test]
    fn feedback_resolves_pending_checks_and_confirmation_raises_confidence() -> TestResult {
        let (store, root) = store("cycle")?;
        let now = OffsetDateTime::now_utc();
        for name in ["good-fact", "bad-fact", "quiet-fact"] {
            store.record_usage(&[name]).map_err(ctx("usage"))?;
        }
        // First cycle only tracks.
        let first = run_outcome_cycle(&store, &root, now);
        assert_eq!(first.tracked, 3);
        assert_eq!((first.confirmed, first.refuted), (0, 0));

        store
            .record_used(&["good-fact", "good-fact"])
            .map_err(ctx("used"))?;
        store.record_used(&["good-fact"]).map_err(ctx("used"))?;
        store
            .record_corrected(&["bad-fact"])
            .map_err(ctx("corrected"))?;
        let second = run_outcome_cycle(&store, &root, now);
        assert_eq!(
            (second.confirmed, second.refuted, second.tracked),
            (1, 1, 0)
        );
        let good = store
            .read("good-fact")
            .map_err(ctx("read"))?
            .map(|f| f.confidence);
        assert!(good.is_some_and(|c| (c - 0.65).abs() < 0.001), "{good:?}");

        // The undecided fact is closed as inconclusive once its window passed.
        let later = now + time::Duration::days(OUTCOME_WINDOW_DAYS + 1);
        let third = run_outcome_cycle(&store, &root, later);
        assert_eq!(third.inconclusive, 1);
        let stats = verdict_stats(&root);
        assert_eq!(
            (stats.confirmed, stats.refuted, stats.inconclusive),
            (1, 1, 1)
        );
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn a_resolved_fact_is_not_tracked_again_and_a_corrupt_state_file_starts_empty() -> TestResult {
        let (store, root) = store("again")?;
        let now = OffsetDateTime::now_utc();
        store.record_usage(&["good-fact"]).map_err(ctx("usage"))?;
        store
            .record_used(&["good-fact", "good-fact"])
            .map_err(ctx("used"))?;
        let _ = run_outcome_cycle(&store, &root, now);
        let again = run_outcome_cycle(&store, &root, now);
        assert_eq!(again.tracked, 0, "already resolved");
        std::fs::write(state_path(&root), b"not json").map_err(ctx("corrupt"))?;
        let recovered = run_outcome_cycle(&store, &root, now);
        assert_eq!(recovered.tracked, 1, "starts empty instead of failing");
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }
}
