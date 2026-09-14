//! Continuous Improvement — Heartbeat-Tick für die Memory-Promotion-Pipeline.
//!
//! # Verantwortungsbereich
//! Implementiert §6 der `docs/design/memory-v2.md` sowie §5.4 (Verdrängung)
//! und §6 (Bedienung) der `docs/design/memory-v3-ltm.md`. Wertet
//! [`crate::store::Memory::stats()`] aus und leitet daraus einen
//! deterministischen [`HeartbeatReport`] ab — ohne direkten State-Eingriff.
//! Die tatsächliche HOT-Kürzung obliegt dem Store; dieser Modul meldet nur
//! das erwartete Delta.
//!
//! Zusätzlich erweitert dieses Modul den Wartungslauf um den
//! Fakten-Verfall aus §5.4: [`decay_facts`] ruft
//! [`crate::facts::FactStore::decay`] für eine oder mehrere Fakten-Wurzeln
//! (Projekt und/oder Global) auf und fasst das Ergebnis in einem
//! [`FactDecayReport`] zusammen; [`tick_with_fact_decay`] kombiniert dies mit
//! [`tick()`] zu einem vollständigen Wartungslauf-Ergebnis.
//!
//! # Schlüsseltypen
//! - [`HeartbeatConfig`] — Schwellwerte, per `Default` vorbelegt gemäß Design-Doc §6,
//!   inklusive `max_unused_days` (Fakten-Verfall, memory-v3-ltm.md §5.4).
//! - [`HeartbeatReport`] — Zählwerte einer `tick()`-Auswertung (HOT/WARM/COLD, unverändert).
//! - [`FactDecayReport`] — Zählwerte des Fakten-Verfalls über eine oder mehrere Wurzeln.
//! - [`tick()`] — idempotente Auswertungs-Funktion für HOT/WARM/COLD (unverändert).
//! - [`decay_facts()`] — Fakten-Verfall über gegebene [`crate::facts::FactStore`]-Wurzeln.
//! - [`tick_with_fact_decay()`] — kombiniert beides für einen vollständigen Wartungslauf.
//!
//! # Nebenläufigkeit
//! `tick()` und `decay_facts()` sind für sich zustandslos und damit thread-safe,
//! sofern der übergebene Store bzw. `FactStore` es ist. `decay_facts()` liest
//! `usage.json` je Fakt einmal zur Vorab-Zählung und ruft dann `decay()` auf —
//! zwischen beiden Schritten ist kein Lock gehalten; bei nebenläufigen
//! Schreibern auf dieselbe Wurzel kann `facts_decayed` daher geringfügig von
//! der tatsächlich von `decay()` mutierten Menge abweichen. Aufrufer, die das
//! ausschließen müssen, serialisieren `decay_facts()` extern (wie
//! `FileMemoryStore::maintain()` es für HOT/WARM/COLD bereits tut).
//!
//! # Fehler
//! Ausschließlich [`crate::error::MemoryError`]-Varianten aus dem Store- bzw.
//! `FactStore`-Aufruf.
//!
//! # Bezug zu philosophy.md
//! Invariante 8: „Memory ist eine Promotion-Pipeline, kein unkontrolliertes
//! Langzeit-Transcript."

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::error::MemoryResult;
use crate::facts::FactStore;

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
    /// Fenster in Tagen, ab dem ein ungenutzter Fakt verfällt
    /// (`memory-v3-ltm.md` §5.4): `last_used`/`updated` älter als dieser Wert
    /// und `usage_count == 0` → `confidence *= 0.5`.
    ///
    /// Default: `90` (Design §5.4, §6 Config-Feld `max_unused_days`).
    pub max_unused_days: i64,
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
            max_unused_days: 90,
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

// ─── Fakten-Verfall (memory-v3-ltm.md §5.4) ────────────────────────────────────

/// Ergebnis des Fakten-Verfalls über eine oder mehrere [`FactStore`]-Wurzeln.
///
/// # Beschreibung
/// Von [`decay_facts`] erzeugt. Fasst zusammen, wie viele Fakten in diesem
/// Lauf verfallen sind (Confidence halbiert) und welche davon unter die
/// Löschschwelle `0.2` gefallen sind — Letztere werden laut Design §5.4
/// gemeldet, nicht automatisch gelöscht.
///
/// # Serialisierung
/// Vollständig JSON-roundtrip-fähig (`serde_json`).
///
/// # Beispiel
/// ```rust
/// use harw_memory::heartbeat::FactDecayReport;
/// let r = FactDecayReport::default();
/// assert_eq!(r.facts_decayed, 0);
/// assert!(r.facts_below_threshold.is_empty());
/// ```
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FactDecayReport {
    /// Anzahl Fakten, deren `confidence` in diesem Lauf halbiert wurde.
    pub facts_decayed: usize,
    /// Namen der Fakten, deren `confidence` nach dem Verfall unter `0.2`
    /// gefallen ist (über alle übergebenen Wurzeln hinweg).
    pub facts_below_threshold: Vec<String>,
}

/// Zählt, wie viele Fakten in `store` beim nächsten [`FactStore::decay`]-Aufruf
/// mit denselben Parametern verfallen würden.
///
/// # Beschreibung
/// Spiegelt exakt das Prädikat aus [`FactStore::decay`]: ein Fakt zählt, wenn
/// sein Nutzungszähler `0` ist (kein `usage.json`-Eintrag oder Zähler `0`) und
/// sein Alter — gemessen ab `last_used`, ersatzweise ab `fact.updated` —
/// mindestens `max_unused_days` beträgt. Reine Vorab-Zählung ohne Mutation;
/// [`decay_facts`] ruft direkt danach `store.decay(...)` auf, das dieselbe
/// Menge tatsächlich mutiert.
///
/// # Fehler
/// Fehler von [`FactStore::list`].
fn count_decay_candidates(
    store: &FactStore,
    max_unused_days: i64,
    now: OffsetDateTime,
) -> MemoryResult<usize> {
    let facts = store.list()?;
    let mut count = 0usize;
    for fact in &facts {
        let (used_count, last_used) = store.usage(&fact.name).unwrap_or((0, fact.updated));
        if used_count != 0 {
            continue;
        }
        let age_days = (now - last_used).whole_days();
        if age_days >= max_unused_days {
            count += 1;
        }
    }
    Ok(count)
}

/// Wendet den Fakten-Verfall aus Design §5.4 auf eine oder mehrere
/// [`FactStore`]-Wurzeln an (typischerweise Projekt- und Global-Wurzel).
///
/// # Beschreibung
/// Ruft für jede Wurzel [`FactStore::decay`] auf — ungenutzte Fakten
/// (`usage_count == 0`), die seit mindestens `max_unused_days` nicht genutzt
/// wurden, bekommen ihre `confidence` halbiert; Fakten, deren neue
/// `confidence` unter `0.2` fällt, werden **nicht** gelöscht, sondern nur in
/// [`FactDecayReport::facts_below_threshold`] gemeldet. Bestehendes
/// HOT/WARM/COLD-Verhalten aus [`tick`] bleibt davon unberührt — diese
/// Funktion rührt ausschließlich Fakten-Wurzeln an.
///
/// # Argumente
/// - `fact_stores` (`&[&FactStore]`): die zu verfallenden Wurzeln, z. B.
///   `&[&project_store, &global_store]`.
/// - `max_unused_days` (`i64`): Fenster aus [`HeartbeatConfig::max_unused_days`].
/// - `now` (`OffsetDateTime`): Zeitstempel des Laufs.
///
/// # Rückgabe
/// `Ok(FactDecayReport)` mit der Summe aller verfallenen Fakten und den
/// Namen aller Fakten, die dabei unter `0.2` gefallen sind.
///
/// # Fehler
/// Fehler von [`FactStore::list`]/[`FactStore::decay`] (I/O, Serde,
/// Frontmatter) werden durchgereicht; der Lauf bricht dann für die
/// verbleibenden Wurzeln ab.
///
/// # Concurrency
/// Siehe Moduldoku: kein Lock zwischen Vorab-Zählung und `decay()`-Aufruf je
/// Wurzel. Aufrufer mit mehreren Schreibern serialisieren extern.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_memory::facts::{FactScope, FactStore};
/// use harw_memory::heartbeat::decay_facts;
/// use time::OffsetDateTime;
///
/// let project = FactStore::open("/tmp/harw-decay-example/project", FactScope::Project).unwrap();
/// let report = decay_facts(&[&project], 90, OffsetDateTime::now_utc()).unwrap();
/// assert_eq!(report.facts_decayed, 0);
/// ```
pub fn decay_facts(
    fact_stores: &[&FactStore],
    max_unused_days: i64,
    now: OffsetDateTime,
) -> MemoryResult<FactDecayReport> {
    let mut facts_decayed = 0usize;
    let mut facts_below_threshold = Vec::new();
    for store in fact_stores {
        facts_decayed += count_decay_candidates(store, max_unused_days, now)?;
        let below = store.decay(max_unused_days, now)?;
        facts_below_threshold.extend(below);
    }
    Ok(FactDecayReport {
        facts_decayed,
        facts_below_threshold,
    })
}

/// Kombiniert [`tick`] (HOT/WARM/COLD, unverändert) mit [`decay_facts`]
/// (Fakten-Verfall, memory-v3-ltm.md §5.4) zu einem vollständigen
/// Wartungslauf-Ergebnis.
///
/// # Beschreibung
/// Ruft zuerst [`tick`] mit `cfg` auf, danach [`decay_facts`] mit
/// `cfg.max_unused_days` über `fact_stores`. Ändert nichts am bestehenden
/// HOT/WARM/COLD-Verhalten von [`tick`] — beide Teilergebnisse werden nur
/// zusammen zurückgegeben, nicht vermischt. Ein Aufrufer, der
/// [`crate::types::MaintenanceReport`] befüllt, überträgt
/// `FactDecayReport::facts_decayed`/`facts_below_threshold` in dessen
/// gleichnamige Felder.
///
/// # Argumente
/// - `store` (`&M`): beliebige Memory-Implementierung für [`tick`].
/// - `now` (`OffsetDateTime`): Zeitstempel des Laufs, für beide Teilschritte identisch.
/// - `cfg` (`HeartbeatConfig`): Schwellwerte inklusive `max_unused_days`.
/// - `fact_stores` (`&[&FactStore]`): Fakten-Wurzeln für den Verfall.
///
/// # Rückgabe
/// `Ok((HeartbeatReport, FactDecayReport))`.
///
/// # Fehler
/// Fehler von [`tick`] oder [`decay_facts`] werden durchgereicht.
///
/// # Beispiel
/// ```rust,no_run
/// use harw_memory::facts::{FactScope, FactStore};
/// use harw_memory::file_store::FileMemoryStore;
/// use harw_memory::heartbeat::{tick_with_fact_decay, HeartbeatConfig};
/// use time::OffsetDateTime;
///
/// let store = FileMemoryStore::open("/tmp/harw-tick-facts-example").unwrap();
/// let project = FactStore::open("/tmp/harw-tick-facts-example/facts-root", FactScope::Project).unwrap();
/// let (heartbeat, decay) = tick_with_fact_decay(
///     &store,
///     OffsetDateTime::now_utc(),
///     HeartbeatConfig::default(),
///     &[&project],
/// )
/// .unwrap();
/// assert!(heartbeat.hot_lines_after <= 100);
/// assert_eq!(decay.facts_decayed, 0);
/// ```
pub fn tick_with_fact_decay<M: crate::store::Memory>(
    store: &M,
    now: OffsetDateTime,
    cfg: HeartbeatConfig,
    fact_stores: &[&FactStore],
) -> MemoryResult<(HeartbeatReport, FactDecayReport)> {
    let max_unused_days = cfg.max_unused_days;
    let heartbeat_report = tick(store, now, cfg)?;
    let decay_report = decay_facts(fact_stores, max_unused_days, now)?;
    Ok((heartbeat_report, decay_report))
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Memory;
    use crate::types::{Entry, MaintenanceReport, RecallQuery, Signal, Stats};

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
        assert_eq!(cfg.max_unused_days, 90);
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

    // ── Fakten-Verfall (memory-v3-ltm.md §5.4) ────────────────────────────────

    use crate::facts::{Fact, FactScope, FactType};
    use time::Duration;

    /// Erzeugt eine frische, isolierte `FactStore`-Wurzel für einen Test.
    fn tmp_fact_store(tag: &str) -> (std::path::PathBuf, FactStore) {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "harw-heartbeat-facts-{tag}-{}-{id}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let store = FactStore::open(&root, FactScope::Project).unwrap();
        (root, store)
    }

    fn sample_decay_fact(name: &str, confidence: f32) -> Fact {
        let now = OffsetDateTime::now_utc();
        Fact {
            name: name.to_owned(),
            description: "Testfakt für Verfall".to_owned(),
            fact_type: FactType::Fact,
            scope: FactScope::Project,
            created: now,
            updated: now,
            confidence,
            sources: Vec::new(),
            tags: Vec::new(),
            body: "Testinhalt.\n".to_owned(),
        }
    }

    /// Design §5.4: Verfall halbiert `confidence` erst, nachdem das Fenster
    /// `max_unused_days` vollständig abgelaufen ist — davor bleibt der Fakt
    /// unverändert.
    #[test]
    fn decay_facts_halves_confidence_only_after_window_elapses() {
        let (root, store) = tmp_fact_store("window");
        store.write(&sample_decay_fact("alte-entscheidung", 0.8)).unwrap();
        let written = store.read("alte-entscheidung").unwrap().unwrap();

        // Knapp unter dem Fenster: keine Änderung.
        let before_window = written.updated + Duration::days(89);
        let report = decay_facts(&[&store], 90, before_window).unwrap();
        assert_eq!(report.facts_decayed, 0, "89 Tage < 90 Tage Fenster");
        let unchanged = store.read("alte-entscheidung").unwrap().unwrap();
        assert!(
            (unchanged.confidence - 0.8).abs() < f32::EPSILON,
            "confidence darf vor Fensterablauf nicht sinken, war {}",
            unchanged.confidence
        );

        // Fenster genau erreicht: Verfall greift.
        let at_window = written.updated + Duration::days(90);
        let report = decay_facts(&[&store], 90, at_window).unwrap();
        assert_eq!(report.facts_decayed, 1, "90 Tage == Fenster muss verfallen");
        let decayed = store.read("alte-entscheidung").unwrap().unwrap();
        assert!(
            (decayed.confidence - 0.4).abs() < f32::EPSILON,
            "confidence muss halbiert sein (0.8 -> 0.4), war {}",
            decayed.confidence
        );
        assert!(report.facts_below_threshold.is_empty(), "0.4 liegt über 0.2");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Design §5.4: ein Fakt mit `usage_count > 0` verfällt nicht, egal wie
    /// alt sein letzter Zugriff liegt.
    #[test]
    fn decay_facts_skips_used_facts() {
        let (root, store) = tmp_fact_store("used");
        store.write(&sample_decay_fact("oft-genutzt", 0.9)).unwrap();
        store.record_usage(&["oft-genutzt"]).unwrap();
        let (count, last_used) = store.usage("oft-genutzt").unwrap();
        assert_eq!(count, 1);

        let far_future = last_used + Duration::days(10_000);
        let report = decay_facts(&[&store], 90, far_future).unwrap();

        assert_eq!(report.facts_decayed, 0, "benutzte Fakten dürfen nicht verfallen");
        let unchanged = store.read("oft-genutzt").unwrap().unwrap();
        assert!(
            (unchanged.confidence - 0.9).abs() < f32::EPSILON,
            "confidence eines genutzten Fakts darf sich nicht ändern"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// Design §5.4: `facts_decayed` zählt alle verfallenen Fakten,
    /// `facts_below_threshold` nennt nur die, deren neue `confidence` unter
    /// `0.2` fällt — beide Felder müssen exakt stimmen, über mehrere Wurzeln
    /// hinweg aufsummiert.
    #[test]
    fn decay_facts_report_fields_are_exact() {
        let (root_a, store_a) = tmp_fact_store("report-a");
        let (root_b, store_b) = tmp_fact_store("report-b");

        // Fällt nach Halbierung nicht unter 0.2 (0.8 -> 0.4).
        store_a.write(&sample_decay_fact("bleibt-drueber", 0.8)).unwrap();
        // Fällt nach Halbierung unter 0.2 (0.3 -> 0.15).
        store_b.write(&sample_decay_fact("faellt-drunter", 0.3)).unwrap();

        let old_a = store_a.read("bleibt-drueber").unwrap().unwrap();
        let old_b = store_b.read("faellt-drunter").unwrap().unwrap();
        let now = old_a.updated.max(old_b.updated) + Duration::days(200);

        let report = decay_facts(&[&store_a, &store_b], 90, now).unwrap();

        assert_eq!(report.facts_decayed, 2, "beide Fakten müssen verfallen sein");
        assert_eq!(
            report.facts_below_threshold,
            vec!["faellt-drunter".to_owned()],
            "nur der zweite Fakt darf unter 0.2 gemeldet werden"
        );
        let still_present = store_b.read("faellt-drunter").unwrap();
        assert!(
            still_present.is_some(),
            "Fakten unter der Schwelle werden gemeldet, nicht gelöscht (§5.4)"
        );

        let _ = std::fs::remove_dir_all(&root_a);
        let _ = std::fs::remove_dir_all(&root_b);
    }

    /// `tick_with_fact_decay` liefert unverändertes HOT/WARM/COLD-Verhalten
    /// (identisch zu [`tick`]) zusammen mit dem Fakten-Verfall-Report.
    #[test]
    fn tick_with_fact_decay_combines_both_reports_without_changing_hot_behaviour() {
        let (root, fact_store) = tmp_fact_store("combined");
        fact_store.write(&sample_decay_fact("kombi-fakt", 0.6)).unwrap();
        let written = fact_store.read("kombi-fakt").unwrap().unwrap();
        let far_future = written.updated + Duration::days(365);

        let mem_store = MockStore::new(Stats {
            hot_lines: 120,
            pending_signals: 9,
            ..Stats::default()
        });
        let cfg = HeartbeatConfig::default();

        let (heartbeat_report, decay_report) =
            tick_with_fact_decay(&mem_store, far_future, cfg.clone(), &[&fact_store]).unwrap();

        let plain_tick = tick(&mem_store, far_future, cfg).unwrap();
        assert_eq!(
            heartbeat_report, plain_tick,
            "tick_with_fact_decay darf das HOT/WARM/COLD-Ergebnis nicht verändern"
        );
        assert_eq!(decay_report.facts_decayed, 1);
        assert!(decay_report.facts_below_threshold.is_empty(), "0.3 liegt über 0.2");

        let _ = std::fs::remove_dir_all(&root);
    }

    /// `FactDecayReport::default()` und JSON-Roundtrip verhalten sich wie
    /// `HeartbeatReport` — leer bzw. verlustfrei.
    #[test]
    fn fact_decay_report_default_and_serde_roundtrip() {
        let default = FactDecayReport::default();
        assert_eq!(default.facts_decayed, 0);
        assert!(default.facts_below_threshold.is_empty());

        let original = FactDecayReport {
            facts_decayed: 3,
            facts_below_threshold: vec!["a".to_owned(), "b".to_owned()],
        };
        let json = serde_json::to_string(&original).expect("serialize should not fail");
        let restored: FactDecayReport =
            serde_json::from_str(&json).expect("deserialize should not fail");
        assert_eq!(original, restored);
    }
}
