//! Continuous Improvement — Heartbeat-Tick für die Memory-Promotion-Pipeline.
//!
//! # Verantwortungsbereich
//! Implementiert §6 der `docs/design/memory-v2.md`. Wertet [`crate::store::Memory::stats()`]
//! aus und leitet daraus einen deterministischen [`HeartbeatReport`] ab — ohne direkten
//! State-Eingriff. Die tatsächliche HOT-Kürzung obliegt dem Store; dieser Modul meldet
//! nur das erwartete Delta.
//!
//! # Schlüsseltypen
//! - [`HeartbeatConfig`] — Schwellwerte, per `Default` vorbelegt gemäß Design-Doc §6.
//! - [`HeartbeatReport`] — Zählwerte einer `tick()`-Auswertung.
//! - [`tick()`] — idempotente Auswertungs-Funktion.
//!
//! # Nebenläufigkeit
//! `tick()` ist zustandslos und damit thread-safe, sofern der übergebene Store es ist.
//!
//! # Fehler
//! Ausschließlich [`crate::error::MemoryError`]-Varianten aus dem Store-Aufruf.
//!
//! # Bezug zu philosophy.md
//! Invariante 8: „Memory ist eine Promotion-Pipeline, kein unkontrolliertes
//! Langzeit-Transcript."

use serde::{Deserialize, Serialize};

/// Ergebnis eines [`tick()`]-Aufrufes.
///
/// # Beschreibung
/// Fasst die aus [`crate::store::Memory::stats()`] abgeleiteten Deltas zusammen.
/// Kein Feld beschreibt bereits angewendete Änderungen — der Store ist für die
/// tatsächliche Mutation verantwortlich.
///
/// # Serializierung
/// Vollständig JSON-roundtrip-fähig (`serde_json`).
///
/// # Beispiel
/// ```rust
/// use harw_memory::heartbeat::HeartbeatReport;
/// let r = HeartbeatReport::default();
/// assert_eq!(r.promoted, 0);
/// ```
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HeartbeatReport {
    /// Anzahl potenzielle PatternHint-Promotions (Signale / Schwellwert).
    pub promoted: usize,
    /// HOT-Overshoot: `max(0, hot_lines - hot_max_lines)`.
    ///
    /// Die tatsächliche Kürzung übernimmt der Store; dieser Wert ist das
    /// erwartete Delta.
    pub demoted: usize,
    /// Reserviert für WARM→COLD-Cold-Sweep in Folge-Fanout. Immer `0` in dieser
    /// Iteration.
    pub archived: usize,
    /// HOT-Zeilenzahl nach virtuellem Trim: `min(hot_lines, hot_max_lines)`.
    pub hot_lines_after: usize,
}

/// Konfiguration eines Heartbeat-Laufs.
///
/// # Beschreibung
/// Alle Felder entsprechen den Schwellwerten aus `memory-v2.md §6`. Der
/// `Default`-Impl bildet die kanonischen Werte des Design-Vertrags ab.
///
/// # Beispiel
/// ```rust
/// use harw_memory::heartbeat::HeartbeatConfig;
/// let cfg = HeartbeatConfig::default();
/// assert_eq!(cfg.promote_threshold, 3);
/// ```
#[derive(Clone, Debug)]
pub struct HeartbeatConfig {
    /// Anzahl offener Signale, ab der eine Promotion ausgelöst wird.
    ///
    /// Default: `3` (memory-v2.md §6, §5).
    pub promote_threshold: u32,
    /// Zeitfenster in Tagen für Promotion-Auswertung (nicht für Stats-Derivation
    /// in dieser Iteration verwendet — Placeholder für künftige Timestamp-Prüfung).
    ///
    /// Default: `7`.
    pub promote_window_days: i64,
    /// HOT-Eintrag gilt als inaktiv nach dieser vielen Tagen ohne Zugriff
    /// (Dokumentationswert; Demotions-Regel in dieser Iteration via Overflow).
    ///
    /// Default: `30`.
    pub demote_hot_days: i64,
    /// WARM-Eintrag gilt als kalt nach dieser vielen Tagen ohne Zugriff.
    ///
    /// Default: `90`.
    pub archive_warm_days: i64,
    /// Harte HOT-Zeilen-Obergrenze. Überschreitung → `demoted` im Report.
    ///
    /// Default: `100` (Invariante: `Tier::Hot.max_lines() == Some(100)`).
    pub hot_max_lines: usize,
}

impl Default for HeartbeatConfig {
    /// Gibt die kanonischen Schwellwerte gemäß `memory-v2.md §6` zurück.
    ///
    /// # Rückgabe
    /// `HeartbeatConfig { promote_threshold: 3, promote_window_days: 7,
    ///  demote_hot_days: 30, archive_warm_days: 90, hot_max_lines: 100 }`.
    fn default() -> Self {
        Self {
            promote_threshold: 3,
            promote_window_days: 7,
            demote_hot_days: 30,
            archive_warm_days: 90,
            hot_max_lines: 100,
        }
    }
}

/// Berechnet einen deterministischen Heartbeat-Report aus aktuellen Store-Statistiken.
///
/// # Beschreibung
/// Ruft [`crate::store::Memory::stats()`] einmal auf und leitet daraus die
/// Promotion/Demotion-Deltas ab, ohne den Store zu mutieren. Die Funktion ist
/// idempotent: gleiche `stats()`-Werte → gleicher Report.
///
/// ## Regeln (memory-v2.md §6)
/// - **Promotion**: Bei `promote_threshold > 0` und
///   `pending_signals >= promote_threshold` gilt
///   `promoted = pending_signals / promote_threshold` (ganzzahlig). Ein
///   Schwellwert von `0` deaktiviert die Promotion-Berechnung, damit ein
///   fehlerhaftes Runtime-Override keinen Panic auslösen kann.
/// - **HOT-Overflow**: `hot_lines > hot_max_lines` →
///   `demoted = hot_lines - hot_max_lines`; sonst `demoted = 0`.
/// - **Archivierung**: `archived = 0` (reserviert für Cold-Sweep in Folge-Fanout).
/// - **hot_lines_after**: `min(hot_lines, hot_max_lines)`.
///
/// # Argumente
/// - `store` (`&M`): beliebige Memory-Implementierung; muss `stats()` anbieten.
/// - `now` (`time::OffsetDateTime`): Zeitstempel des Laufs (für künftige Timestamp-Checks
///   bereitgehalten; in dieser Iteration nicht für die Stats-Ableitung verwendet).
/// - `cfg` (`HeartbeatConfig`): Konfiguration der Schwellwerte.
///
/// # Rückgabe
/// `Ok(HeartbeatReport)` mit den berechneten Deltas.
///
/// # Fehler
/// - [`crate::error::MemoryError::Io`]: wenn `stats()` einen I/O-Fehler meldet.
/// - [`crate::error::MemoryError::TierOverflow`]: wenn `stats()` einen Overflow zurückgibt.
/// - [`crate::error::MemoryError::LockContention`]: wenn der Store gesperrt ist.
///
/// # Concurrency
/// `tick` ist zustandslos und hält keine eigenen Locks. Thread-Safety hängt
/// ausschließlich vom übergebenen Store ab.
///
/// # Idempotenz
/// Zwei Aufrufe mit identischen `stats()`-Ergebnissen produzieren denselben Report.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_memory::heartbeat::{tick, HeartbeatConfig};
/// use harw_memory::file_store::FileMemoryStore;
/// use time::OffsetDateTime;
///
/// let store = FileMemoryStore::open("/tmp/heartbeat-example").unwrap();
/// let report = tick(&store, OffsetDateTime::now_utc(), HeartbeatConfig::default()).unwrap();
/// assert!(report.hot_lines_after <= 100);
/// ```
pub fn tick<M: crate::store::Memory>(
    store: &M,
    _now: time::OffsetDateTime,
    cfg: HeartbeatConfig,
) -> crate::error::MemoryResult<HeartbeatReport> {
    let stats = store.stats()?;

    let promote_threshold = cfg.promote_threshold as usize;
    let promoted = if promote_threshold > 0 && stats.pending_signals >= promote_threshold {
        stats.pending_signals / promote_threshold
    } else {
        0
    };

    let demoted = stats.hot_lines.saturating_sub(cfg.hot_max_lines);

    let archived = 0_usize;

    let hot_lines_after = stats.hot_lines.min(cfg.hot_max_lines);

    Ok(HeartbeatReport {
        promoted,
        demoted,
        archived,
        hot_lines_after,
    })
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::MemoryResult;
    use crate::store::Memory;
    use crate::types::{Entry, MaintenanceReport, RecallQuery, Signal, Stats};
    use time::OffsetDateTime;

    // ── MockStore ──────────────────────────────────────────────────────────────

    /// Minimaler Mock-Store für Heartbeat-Tests.
    ///
    /// Gibt voreingestellte `Stats` zurück; alle anderen Trait-Methoden sind
    /// `unimplemented!()`, da sie in diesen Tests nicht aufgerufen werden.
    struct MockStore {
        stats: Stats,
    }

    impl MockStore {
        fn new(stats: Stats) -> Self {
            Self { stats }
        }
    }

    impl Memory for MockStore {
        fn hot(&self) -> MemoryResult<String> {
            unimplemented!("not needed for heartbeat tests")
        }

        fn recall<'a>(&self, _query: RecallQuery<'a>) -> MemoryResult<Vec<Entry>> {
            unimplemented!("not needed for heartbeat tests")
        }

        fn record(&self, _signal: Signal) -> MemoryResult<()> {
            unimplemented!("not needed for heartbeat tests")
        }

        fn maintain(&self) -> MemoryResult<MaintenanceReport> {
            unimplemented!("not needed for heartbeat tests")
        }

        fn stats(&self) -> MemoryResult<Stats> {
            Ok(self.stats.clone())
        }
    }

    // ── Helpers ────────────────────────────────────────────────────────────────

    fn now() -> OffsetDateTime {
        // Fixed instant — tests must not depend on wall clock.
        time::macros::datetime!(2026-07-16 12:00:00 UTC)
    }

    // ── Test 1: HOT-Overflow → demoted delta ──────────────────────────────────

    /// Prüft, dass `demoted` das exakte Overshoot-Delta zurückgibt.
    ///
    /// Design-Doc §6: HOT-Overflow (>100 Zeilen) → älteste per `last_used` → WARM.
    /// Der Report nennt das Delta; der Store führt die Kürzung aus.
    #[test]
    fn test_overflow_reports_demote_delta() {
        let store = MockStore::new(Stats {
            hot_lines: 120,
            ..Stats::default()
        });
        let report = tick(&store, now(), HeartbeatConfig::default()).unwrap();
        assert_eq!(report.demoted, 20, "overshoot should be 120 - 100 = 20");
        assert_eq!(
            report.hot_lines_after, 100,
            "after virtual trim hot_lines_after == max"
        );
    }

    // ── Test 2: No overflow → demoted stays zero ───────────────────────────────

    /// Prüft, dass kein Overflow kein `demoted` erzeugt.
    #[test]
    fn test_no_overflow_reports_zero_demote() {
        let store = MockStore::new(Stats {
            hot_lines: 50,
            ..Stats::default()
        });
        let report = tick(&store, now(), HeartbeatConfig::default()).unwrap();
        assert_eq!(report.demoted, 0);
        assert_eq!(report.hot_lines_after, 50);
    }

    // ── Test 3: Pending signals → promotions ──────────────────────────────────

    /// Prüft die Ganzzahl-Division: 9 Signale / Schwellwert 3 → 3 Promotions.
    ///
    /// Design-Doc §6: PatternHint 3× in 7d → nach WARM.
    #[test]
    fn test_pending_signals_yield_promotions() {
        let store = MockStore::new(Stats {
            pending_signals: 9,
            ..Stats::default()
        });
        let report = tick(&store, now(), HeartbeatConfig::default()).unwrap();
        assert_eq!(report.promoted, 3, "9 / 3 = 3 promotion slots");
    }

    // ── Test 4: Idempotency ────────────────────────────────────────────────────

    /// Prüft Invariante 8 (philosophy.md §16): zwei identische Stats → gleicher Report.
    #[test]
    fn test_idempotent_when_stats_stable() {
        let stats = Stats {
            hot_lines: 80,
            pending_signals: 6,
            warm_namespaces: 2,
            warm_total_lines: 40,
            cold_namespaces: 1,
        };
        let store = MockStore::new(stats);
        let first = tick(&store, now(), HeartbeatConfig::default()).unwrap();
        let second = tick(&store, now(), HeartbeatConfig::default()).unwrap();
        assert_eq!(first, second, "tick must be idempotent for stable stats");
    }

    // ── Test 5: Default config matches design-doc ─────────────────────────────

    /// Verifiziert, dass die `Default`-Werte exakt der Design-Doc §6 entsprechen.
    #[test]
    fn test_default_config_matches_design_doc() {
        let cfg = HeartbeatConfig::default();
        assert_eq!(cfg.promote_threshold, 3);
        assert_eq!(cfg.promote_window_days, 7);
        assert_eq!(cfg.demote_hot_days, 30);
        assert_eq!(cfg.archive_warm_days, 90);
        assert_eq!(cfg.hot_max_lines, 100);
    }

    // ── Test 6: HeartbeatReport serde roundtrip ───────────────────────────────

    /// Stellt sicher, dass `HeartbeatReport` verlustfrei JSON-serialisiert werden kann.
    #[test]
    fn test_report_serde_roundtrip() {
        let original = HeartbeatReport {
            promoted: 2,
            demoted: 15,
            archived: 0,
            hot_lines_after: 85,
        };
        let json = serde_json::to_string(&original).expect("serialize should not fail");
        let restored: HeartbeatReport =
            serde_json::from_str(&json).expect("deserialize should not fail");
        assert_eq!(original, restored);
    }

    // ── Additional edge: signals below threshold → no promotion ───────────────

    /// Randfall: Signale unter dem Schwellwert → keine Promotions.
    #[test]
    fn test_signals_below_threshold_yield_no_promotions() {
        let store = MockStore::new(Stats {
            pending_signals: 2,
            ..Stats::default()
        });
        let report = tick(&store, now(), HeartbeatConfig::default()).unwrap();
        assert_eq!(report.promoted, 0, "2 < 3 threshold — no promotion");
    }

    // ── Additional edge: exact threshold → exactly one promotion slot ──────────

    /// Randfall: exakt Schwellwert-viele Signale → exakt 1 Promotion-Slot.
    #[test]
    fn test_signals_at_exact_threshold_yield_one_promotion() {
        let store = MockStore::new(Stats {
            pending_signals: 3,
            ..Stats::default()
        });
        let report = tick(&store, now(), HeartbeatConfig::default()).unwrap();
        assert_eq!(report.promoted, 1);
    }

    // ── Additional edge: disabled promotion avoids division by zero ──────────

    /// Ein Runtime-Override mit Schwellwert `0` darf den Heartbeat nicht
    /// panicken lassen; er deaktiviert stattdessen nur die Promotion-Berechnung.
    #[test]
    fn test_zero_promotion_threshold_disables_promotions() {
        let store = MockStore::new(Stats {
            pending_signals: 9,
            ..Stats::default()
        });
        let cfg = HeartbeatConfig {
            promote_threshold: 0,
            ..HeartbeatConfig::default()
        };

        let report = tick(&store, now(), cfg).unwrap();

        assert_eq!(report.promoted, 0);
    }

    // ── Additional edge: archived is always zero in this iteration ─────────────

    /// Randfall: `archived` ist immer 0 — reserviert für Cold-Sweep in Folge-Fanout.
    #[test]
    fn test_archived_always_zero() {
        let store = MockStore::new(Stats {
            hot_lines: 200,
            pending_signals: 100,
            warm_namespaces: 50,
            warm_total_lines: 5000,
            cold_namespaces: 10,
        });
        let report = tick(&store, now(), HeartbeatConfig::default()).unwrap();
        assert_eq!(report.archived, 0, "Cold-Sweep reserved for future fanout");
    }
}
