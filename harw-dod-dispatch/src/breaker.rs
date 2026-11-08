//! Breaker je Partition mit Hysterese: [`BreakerPartition`], [`BreakerSet`].
//!
//! # Zweck
//! Ein Breaker zählt die angebotenen KI-fähigen Befunde seiner Partition
//! `(uid, cgroup_kid, sensor)` in festen Fenstern. Übersteigt die Zahl in
//! einem Fenster `trip_threshold`, löst er aus: genau **einmal** je
//! Auslösung meldet er [`BreakerObservation::Tripped`], danach
//! [`BreakerObservation::Open`]. Der Dispatcher macht daraus
//! [`crate::DispatchAction::Starved`]; der Aufrufer bildet daraus einen
//! `High`-Befund `dispatch-starved` (I10). Befunde anderer Partitionen
//! laufen unverändert weiter.
//!
//! # Hysterese
//! Ein offener Breaker schließt erst nach einem **ruhigen Fenster**: einem
//! Zeitraum von `quiet_secs`, in dem höchstens `quiet_threshold` Befunde
//! kamen (oder einem doppelt so langen ganz ohne Befund). Ein Angreifer,
//! der knapp unter der Auslöseschwelle weiterfeuert, hält seinen eigenen
//! Breaker offen, nicht den anderer Partitionen.
//!
//! # Fenster
//! Die Zählfenster sind fest (Beginn beim ersten Befund nach Ablauf des
//! vorigen). Die Dauerrate einer Partition ohne Auslösung ist damit höchstens
//! `trip_threshold` je `window_secs`; eine Spitze, die genau über eine
//! Fenstergrenze fällt, kann kurzzeitig bis zum Doppelten erreichen. Das ist
//! bewusst so: der Breaker meldet Stürme, die Kosten begrenzt das Budget
//! ([`crate::PartitionedBudget`]) unabhängig davon.
//!
//! # Zeit
//! Unix-Sekunden vom Aufrufer. Läuft die Zeit rückwärts, beginnt das
//! jeweilige Fenster neu.
//!
//! # Speicher
//! Höchstens `max_partitions` Breaker. Ist die Tabelle voll, fallen zuerst
//! ruhende (gleichwertig zu neuen), dann geschlossene, zuletzt offene, je
//! der am längsten unbenutzte zuerst. Beim Laden eines Zustands mit mehr
//! Breakern ([`crate::Dispatcher::restore`]) gilt dieselbe Ordnung ohne die
//! Stufe „ruhend" (ohne Uhr nicht bestimmbar): ein offener Breaker überlebt
//! einen Neustart mit kleinerer Grenze, solange es nicht mehr offene als die
//! Grenze gibt.
//!
//! # Nebenläufigkeit und Fehler
//! `&mut self`, deterministisch; Fehler nur beim Laden eines Zustands.
//!
//! # Examples
//! ```
//! use harw_dod_dispatch::{BreakerConfig, BreakerObservation, BreakerPartition};
//!
//! let config = BreakerConfig { trip_threshold: 2, ..BreakerConfig::default() };
//! let mut breaker = BreakerPartition::new(0);
//! assert_eq!(breaker.observe(&config, 0), BreakerObservation::Closed);
//! assert_eq!(breaker.observe(&config, 1), BreakerObservation::Closed);
//! assert_eq!(breaker.observe(&config, 2), BreakerObservation::Tripped);
//! assert_eq!(breaker.observe(&config, 3), BreakerObservation::Open);
//! ```

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::DispatchError;
use crate::finding::{PartitionKey, is_sensor_id};
use crate::prune::retain_greatest;

/// Schwellen des Breakers.
///
/// # Examples
/// ```
/// use harw_dod_dispatch::BreakerConfig;
///
/// let config = BreakerConfig::default();
/// assert!(config.quiet_threshold < config.trip_threshold);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BreakerConfig {
    /// Mehr Befunde als diese Zahl in einem Fenster lösen aus.
    pub trip_threshold: u32,
    /// Fensterbreite in Sekunden.
    pub window_secs: u32,
    /// Breite des ruhigen Fensters in Sekunden.
    pub quiet_secs: u32,
    /// Höchstzahl der Befunde in einem ruhigen Fenster.
    pub quiet_threshold: u32,
    /// Höchstzahl geführter Partitionen.
    pub max_partitions: usize,
}

impl Default for BreakerConfig {
    fn default() -> Self {
        Self {
            trip_threshold: 60,
            window_secs: 60,
            quiet_secs: 300,
            quiet_threshold: 6,
            max_partitions: 4_096,
        }
    }
}

impl BreakerConfig {
    // Prüft die Grenzen (von `DispatchConfig::validate` aufgerufen).
    pub(crate) fn validate(&self) -> Result<(), DispatchError> {
        let invalid = |field: &'static str, reason: &'static str| {
            Err(DispatchError::InvalidConfig { field, reason })
        };
        if self.trip_threshold == 0 {
            return invalid("breaker.trip_threshold", "must be at least 1");
        }
        if self.window_secs == 0 {
            return invalid("breaker.window_secs", "must be at least 1");
        }
        if self.quiet_secs == 0 {
            return invalid("breaker.quiet_secs", "must be at least 1");
        }
        if self.quiet_threshold >= self.trip_threshold {
            return invalid(
                "breaker.quiet_threshold",
                "must be below breaker.trip_threshold",
            );
        }
        if self.max_partitions == 0 || self.max_partitions > 1 << 20 {
            return invalid("breaker.max_partitions", "must be between 1 and 1048576");
        }
        Ok(())
    }
}

/// Ergebnis von [`BreakerPartition::observe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BreakerObservation {
    /// Geschlossen: der Befund darf weiter.
    Closed,
    /// Soeben ausgelöst (einmal je Auslösung); der Befund wird verworfen.
    Tripped,
    /// Offen: der Befund wird verworfen.
    Open,
}

/// Breaker einer Partition (siehe Moduldoku).
///
/// # Examples
/// Siehe Moduldoku.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BreakerPartition {
    // Beginn des Zählfensters.
    window_start_unix: i64,
    // Befunde im Zählfenster.
    window_count: u32,
    // Ausgelöst und noch nicht zurückgesetzt.
    tripped: bool,
    // Beginn des ruhigen Fensters (nur bei `tripped`).
    quiet_start_unix: i64,
    // Befunde im ruhigen Fenster.
    quiet_count: u32,
    // Letzter Befund (für die Speichergrenze).
    last_offer_unix: i64,
}

impl BreakerPartition {
    /// Geschlossener Breaker.
    ///
    /// # Arguments
    /// - `now_unix` (`i64`): Beginn des ersten Fensters.
    ///
    /// # Returns
    /// Den Breaker.
    #[must_use]
    pub fn new(now_unix: i64) -> Self {
        Self {
            window_start_unix: now_unix,
            window_count: 0,
            tripped: false,
            quiet_start_unix: now_unix,
            quiet_count: 0,
            last_offer_unix: now_unix,
        }
    }

    /// Ob der Breaker offen ist.
    #[must_use]
    pub fn is_tripped(&self) -> bool {
        self.tripped
    }

    /// Zählt einen angebotenen Befund.
    ///
    /// # Arguments
    /// - `config` (`&BreakerConfig`): Schwellen.
    /// - `now_unix` (`i64`): aktuelle Zeit in Unix-Sekunden.
    ///
    /// # Returns
    /// [`BreakerObservation::Closed`], genau einmal je Auslösung
    /// [`BreakerObservation::Tripped`], sonst [`BreakerObservation::Open`].
    pub fn observe(&mut self, config: &BreakerConfig, now_unix: i64) -> BreakerObservation {
        self.last_offer_unix = now_unix;
        if self.tripped {
            let quiet = i64::from(config.quiet_secs);
            let elapsed = now_unix.saturating_sub(self.quiet_start_unix);
            if elapsed < 0 {
                self.quiet_start_unix = now_unix;
                self.quiet_count = 0;
            } else if self.would_close(config, now_unix) {
                self.tripped = false;
                self.window_start_unix = now_unix;
                self.window_count = 0;
            } else if elapsed >= quiet {
                self.quiet_start_unix = now_unix;
                self.quiet_count = 0;
            }
            if self.tripped {
                self.quiet_count = self.quiet_count.saturating_add(1);
                return BreakerObservation::Open;
            }
        }
        let since = now_unix.saturating_sub(self.window_start_unix);
        if since < 0 || since >= i64::from(config.window_secs) {
            self.window_start_unix = now_unix;
            self.window_count = 0;
        }
        self.window_count = self.window_count.saturating_add(1);
        if self.window_count > config.trip_threshold {
            self.tripped = true;
            self.quiet_start_unix = now_unix;
            self.quiet_count = 0;
            return BreakerObservation::Tripped;
        }
        BreakerObservation::Closed
    }

    // Ein offener Breaker, der beim nächsten Befund zu `now_unix` schlösse:
    // ein ruhiges Fenster ist vorbei (höchstens `quiet_threshold` Befunde)
    // oder ein ganzes weiteres Fenster kam gar kein Befund.
    fn would_close(&self, config: &BreakerConfig, now_unix: i64) -> bool {
        let quiet = i64::from(config.quiet_secs);
        let elapsed = now_unix.saturating_sub(self.quiet_start_unix);
        self.tripped
            && elapsed >= 0
            && (elapsed >= quiet.saturating_mul(2)
                || (elapsed >= quiet && self.quiet_count <= config.quiet_threshold))
    }

    // Gleichwertig zu einem neuen Breaker: geschlossen mit abgelaufenem
    // Fenster oder offen, aber beim nächsten Befund wieder geschlossen.
    fn is_idle(&self, config: &BreakerConfig, now_unix: i64) -> bool {
        let since = now_unix.saturating_sub(self.window_start_unix);
        let closed_and_expired =
            !self.tripped && (since < 0 || since >= i64::from(config.window_secs));
        closed_and_expired || self.would_close(config, now_unix)
    }
}

// Persistierbarer Inhalt eines `BreakerSet`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BreakerState {
    // Breaker je Partition.
    #[serde(with = "crate::serde_map")]
    partitions: BTreeMap<PartitionKey, BreakerPartition>,
}

/// Alle Breaker, je Partition einer.
///
/// # Examples
/// ```
/// use harw_dod_dispatch::{BreakerConfig, BreakerObservation, BreakerSet, PartitionKey};
///
/// let mut set = BreakerSet::new(BreakerConfig { trip_threshold: 1, quiet_threshold: 0, ..BreakerConfig::default() });
/// let noisy = PartitionKey { uid: Some(1), cgroup_kid: Some(1), sensor: "probe-bpf-0".to_owned() };
/// let quiet = PartitionKey { cgroup_kid: Some(2), ..noisy.clone() };
/// assert_eq!(set.observe(&noisy, 0), BreakerObservation::Closed);
/// assert_eq!(set.observe(&noisy, 0), BreakerObservation::Tripped);
/// assert_eq!(set.observe(&quiet, 0), BreakerObservation::Closed);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BreakerSet {
    // Schwellen.
    config: BreakerConfig,
    // Breaker je Partition.
    partitions: BTreeMap<PartitionKey, BreakerPartition>,
}

impl BreakerSet {
    /// Leere Menge.
    ///
    /// # Arguments
    /// - `config` (`BreakerConfig`): Schwellen (vorher geprüft).
    ///
    /// # Returns
    /// Die Menge ohne Breaker.
    #[must_use]
    pub fn new(config: BreakerConfig) -> Self {
        Self {
            config,
            partitions: BTreeMap::new(),
        }
    }

    /// Zählt einen Befund in seiner Partition.
    ///
    /// # Arguments
    /// - `partition` (`&PartitionKey`): Partition des Befunds.
    /// - `now_unix` (`i64`): aktuelle Zeit in Unix-Sekunden.
    ///
    /// # Returns
    /// Wie [`BreakerPartition::observe`].
    pub fn observe(&mut self, partition: &PartitionKey, now_unix: i64) -> BreakerObservation {
        if !self.partitions.contains_key(partition) {
            self.make_room(now_unix);
        }
        let config = self.config;
        self.partitions
            .entry(partition.clone())
            .or_insert_with(|| BreakerPartition::new(now_unix))
            .observe(&config, now_unix)
    }

    /// Ob der Breaker einer Partition offen ist.
    ///
    /// # Arguments
    /// - `partition` (`&PartitionKey`): die Partition.
    ///
    /// # Returns
    /// `false` für eine unbekannte Partition.
    #[must_use]
    pub fn is_tripped(&self, partition: &PartitionKey) -> bool {
        self.partitions
            .get(partition)
            .is_some_and(BreakerPartition::is_tripped)
    }

    // Macht Platz: ruhende zuerst, dann der am längsten unbenutzte
    // geschlossene, zuletzt der am längsten unbenutzte überhaupt.
    fn make_room(&mut self, now_unix: i64) {
        if self.partitions.len() < self.config.max_partitions {
            return;
        }
        let config = self.config;
        self.partitions
            .retain(|_, breaker| !breaker.is_idle(&config, now_unix));
        while self.partitions.len() >= config.max_partitions {
            let victim = self
                .partitions
                .iter()
                .min_by_key(|(_, breaker)| (breaker.tripped, breaker.last_offer_unix))
                .map(|(key, _)| key.clone());
            match victim {
                Some(key) => {
                    self.partitions.remove(&key);
                }
                None => break,
            }
        }
    }

    // Persistierbarer Inhalt.
    pub(crate) fn export(&self) -> BreakerState {
        BreakerState {
            partitions: self.partitions.clone(),
        }
    }

    // Baut die Menge aus einem Zustand: ungültige Sensor-Kennungen werden
    // abgelehnt (auch in Breakern, die danach fallen), dann wird auf
    // `max_partitions` gekürzt (Grenze der aktiven Konfiguration). Wie in
    // `make_room` fallen geschlossene Breaker vor offenen, innerhalb davon
    // die am längsten unbenutzten, bei Gleichstand der kleinere Schlüssel.
    // Ein offener Breaker fällt also erst, wenn mehr offene als die Grenze
    // da sind; sonst würde ein Neustart mit kleinerer Grenze einen Sturm
    // wieder durchlassen (B10). Ohne Uhr lässt sich „ruhend" nicht
    // bestimmen; diese Stufe aus `make_room` entfällt.
    pub(crate) fn import(
        config: BreakerConfig,
        state: BreakerState,
    ) -> Result<Self, DispatchError> {
        let BreakerState { mut partitions } = state;
        if !partitions.keys().all(|key| is_sensor_id(&key.sensor)) {
            return Err(DispatchError::InvalidState { what: "breakers" });
        }
        retain_greatest(&mut partitions, config.max_partitions, |breaker| {
            (breaker.tripped, breaker.last_offer_unix)
        });
        Ok(Self { config, partitions })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    fn config() -> BreakerConfig {
        BreakerConfig {
            trip_threshold: 3,
            window_secs: 60,
            quiet_secs: 100,
            quiet_threshold: 1,
            max_partitions: 16,
        }
    }

    #[test]
    fn test_trips_once_and_stays_open_until_a_quiet_window() -> TestResult {
        let config = config();
        let mut breaker = BreakerPartition::new(0);
        let observed: Vec<BreakerObservation> =
            (0..6).map(|t| breaker.observe(&config, t)).collect();
        assert_eq!(
            observed,
            [
                BreakerObservation::Closed,
                BreakerObservation::Closed,
                BreakerObservation::Closed,
                BreakerObservation::Tripped,
                BreakerObservation::Open,
                BreakerObservation::Open,
            ]
        );
        // Ein Fenster mit drei Befunden ist nicht ruhig: bleibt offen.
        assert_eq!(breaker.observe(&config, 50), BreakerObservation::Open);
        assert_eq!(breaker.observe(&config, 103), BreakerObservation::Open);
        assert!(breaker.is_tripped());
        // Danach ein ruhiges Fenster (nur der Befund bei 103): schließt.
        assert_eq!(breaker.observe(&config, 203), BreakerObservation::Closed);
        assert!(!breaker.is_tripped());
        Ok(())
    }

    #[test]
    fn test_new_trip_after_reset_reports_tripped_again() -> TestResult {
        let config = config();
        let mut breaker = BreakerPartition::new(0);
        let trips = |breaker: &mut BreakerPartition, start: i64| {
            (start..start.saturating_add(10))
                .map(|t| breaker.observe(&config, t))
                .filter(|observation| *observation == BreakerObservation::Tripped)
                .count()
        };
        assert_eq!(trips(&mut breaker, 0), 1);
        // Zwei leere ruhige Fenster: zurückgesetzt.
        assert_eq!(trips(&mut breaker, 1_000), 1);
        Ok(())
    }

    #[test]
    fn test_window_rollover_keeps_slow_traffic_closed() -> TestResult {
        let config = config();
        let mut breaker = BreakerPartition::new(0);
        for minute in 0..20_i64 {
            for second in 0..3_i64 {
                let now = minute.saturating_mul(60).saturating_add(second);
                assert_eq!(breaker.observe(&config, now), BreakerObservation::Closed);
            }
        }
        Ok(())
    }

    #[test]
    fn test_quiet_window_boundary_is_quiet_threshold() -> TestResult {
        let config = config();
        // Genau `quiet_threshold` (1) Befund im ruhigen Fenster: schließt.
        let mut calm = BreakerPartition::new(0);
        let trips = (0..4)
            .map(|t| calm.observe(&config, t))
            .filter(|observation| *observation == BreakerObservation::Tripped)
            .count();
        assert_eq!(trips, 1);
        assert_eq!(calm.observe(&config, 50), BreakerObservation::Open);
        assert_eq!(calm.observe(&config, 103), BreakerObservation::Closed);
        // Einer mehr: bleibt offen, das nächste ruhige Fenster beginnt.
        let mut noisy = BreakerPartition::new(0);
        let observed: Vec<BreakerObservation> = (0..4).map(|t| noisy.observe(&config, t)).collect();
        assert_eq!(observed.last(), Some(&BreakerObservation::Tripped));
        assert_eq!(noisy.observe(&config, 50), BreakerObservation::Open);
        assert_eq!(noisy.observe(&config, 60), BreakerObservation::Open);
        assert_eq!(noisy.observe(&config, 103), BreakerObservation::Open);
        assert!(noisy.is_tripped());
        assert_eq!(noisy.observe(&config, 203), BreakerObservation::Closed);
        Ok(())
    }

    #[test]
    fn test_backwards_clock_keeps_an_open_breaker_open() -> TestResult {
        let config = config();
        let mut breaker = BreakerPartition::new(1_000);
        let observed: Vec<BreakerObservation> = (1_000..1_004)
            .map(|t| breaker.observe(&config, t))
            .collect();
        assert_eq!(observed.last(), Some(&BreakerObservation::Tripped));
        // Rückwärts: das ruhige Fenster beginnt neu, kein neuer Trip.
        assert_eq!(breaker.observe(&config, 500), BreakerObservation::Open);
        assert_eq!(breaker.observe(&config, 599), BreakerObservation::Open);
        assert!(breaker.is_tripped());
        // Ein ganzes ruhiges Fenster ohne Befund (599 bis 800): geschlossen.
        assert_eq!(breaker.observe(&config, 800), BreakerObservation::Closed);
        Ok(())
    }

    #[test]
    fn test_eviction_prefers_idle_and_closed_breakers() -> TestResult {
        let config = BreakerConfig {
            max_partitions: 2,
            ..config()
        };
        let partition = |cgroup: u64| PartitionKey {
            uid: Some(1),
            cgroup_kid: Some(cgroup),
            sensor: "probe-bpf-0".to_owned(),
        };
        let mut set = BreakerSet::new(config);
        let observed: Vec<BreakerObservation> =
            (0..4).map(|_| set.observe(&partition(1), 0)).collect();
        assert_eq!(observed.last(), Some(&BreakerObservation::Tripped));
        assert!(set.is_tripped(&partition(1)));
        assert_eq!(set.observe(&partition(2), 0), BreakerObservation::Closed);
        // Platz für Partition 3: der offene Breaker bleibt, der geschlossene fällt.
        assert_eq!(set.observe(&partition(3), 1), BreakerObservation::Closed);
        assert!(set.is_tripped(&partition(1)));
        assert!(!set.partitions.contains_key(&partition(2)));
        // Nach einem ruhigen Fenster ohne Befund ist auch der offene
        // gleichwertig zu neu und fällt vor dem aktiven geschlossenen.
        assert_eq!(set.observe(&partition(3), 199), BreakerObservation::Closed);
        assert_eq!(set.observe(&partition(4), 203), BreakerObservation::Closed);
        assert!(!set.partitions.contains_key(&partition(1)));
        assert!(set.partitions.contains_key(&partition(3)));
        Ok(())
    }

    #[test]
    fn test_state_round_trip_keeps_the_trip() -> TestResult {
        let mut set = BreakerSet::new(config());
        let noisy = PartitionKey {
            uid: Some(1),
            cgroup_kid: None,
            sensor: "probe-bpf-0".to_owned(),
        };
        let tripped = (0..10)
            .map(|_| set.observe(&noisy, 0))
            .filter(|observation| *observation == BreakerObservation::Tripped)
            .count();
        assert_eq!(tripped, 1);
        let json = serde_json::to_string(&set.export())?;
        let state: BreakerState = serde_json::from_str(&json)?;
        let mut restored = BreakerSet::import(config(), state)?;
        assert_eq!(restored, set);
        // Ein Neustart erzeugt keinen zweiten Trip derselben Auslösung.
        assert_eq!(restored.observe(&noisy, 1), BreakerObservation::Open);
        let mut tampered = set.export();
        tampered.partitions.insert(
            PartitionKey {
                sensor: "bad\u{202e}".to_owned(),
                ..noisy
            },
            BreakerPartition::new(0),
        );
        assert_eq!(
            BreakerSet::import(config(), tampered).err(),
            Some(DispatchError::InvalidState { what: "breakers" })
        );
        Ok(())
    }

    // Die Partitionen einer Menge in aufsteigender Reihenfolge.
    fn keys_of(set: &BreakerSet) -> Vec<PartitionKey> {
        set.partitions.keys().cloned().collect()
    }

    #[test]
    fn test_import_prunes_to_the_active_bound_keeping_open_breakers() -> TestResult {
        let partition = |cgroup: u64| PartitionKey {
            uid: Some(1),
            cgroup_kid: Some(cgroup),
            sensor: "probe-bpf-0".to_owned(),
        };
        let mut set = BreakerSet::new(config());
        // Partition 1 löst früh aus (bei 0), danach melden sich 2 bis 5 je
        // einmal, Partition 5 zuletzt.
        let observed: Vec<BreakerObservation> =
            (0..4).map(|_| set.observe(&partition(1), 0)).collect();
        assert_eq!(observed.last(), Some(&BreakerObservation::Tripped));
        for cgroup in 2..=5_u64 {
            let now = i64::try_from(cgroup.saturating_mul(10)).unwrap_or(0);
            assert_eq!(
                set.observe(&partition(cgroup), now),
                BreakerObservation::Closed
            );
        }
        // Unverändert unter derselben Grenze.
        let same = BreakerSet::import(config(), set.export())?;
        assert_eq!(same, set);
        // Grenze 2: der offene Breaker bleibt trotz seines Alters, dazu der
        // zuletzt benutzte geschlossene.
        let two = BreakerConfig {
            max_partitions: 2,
            ..config()
        };
        let mut restored = BreakerSet::import(two, set.export())?;
        assert_eq!(keys_of(&restored), [partition(1), partition(5)]);
        // Kein zweiter Trip derselben Auslösung nach dem Neustart.
        assert_eq!(
            restored.observe(&partition(1), 60),
            BreakerObservation::Open
        );
        // Grenze 1: nur der offene.
        let one = BreakerConfig {
            max_partitions: 1,
            ..config()
        };
        let restored = BreakerSet::import(one, set.export())?;
        assert_eq!(keys_of(&restored), [partition(1)]);
        assert!(restored.is_tripped(&partition(1)));
        Ok(())
    }

    #[test]
    fn test_import_breaks_ties_by_the_greater_partition() -> TestResult {
        let mut set = BreakerSet::new(config());
        for cgroup in [7_u64, 3, 9, 1] {
            let key = PartitionKey {
                uid: Some(1),
                cgroup_kid: Some(cgroup),
                sensor: "probe-bpf-0".to_owned(),
            };
            assert_eq!(set.observe(&key, 5), BreakerObservation::Closed);
        }
        let two = BreakerConfig {
            max_partitions: 2,
            ..config()
        };
        let restored = BreakerSet::import(two, set.export())?;
        let kept: Vec<Option<u64>> = keys_of(&restored)
            .into_iter()
            .map(|key| key.cgroup_kid)
            .collect();
        assert_eq!(kept, [Some(7), Some(9)]);
        Ok(())
    }

    #[test]
    fn test_random_arrivals_never_trip_twice_without_closing() -> TestResult {
        // Deterministische Pseudo-Zufallsfolge (kein proptest im Workspace):
        // meist dichte Schübe, selten lange Pausen.
        let config = config();
        let mut breaker = BreakerPartition::new(0);
        let mut now = 0_i64;
        let mut state = 0x1234_5678_u32;
        let mut open = false;
        let mut trips = 0_u32;
        for _ in 0..20_000 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let bits = state.checked_shr(8).unwrap_or(0);
            let modulus = if state.checked_shr(28) == Some(0) {
                400
            } else {
                3
            };
            now = now.saturating_add(i64::from(bits.checked_rem(modulus).unwrap_or(0)));
            match breaker.observe(&config, now) {
                BreakerObservation::Tripped => {
                    assert!(!open, "second trip without closing at {now}");
                    open = true;
                    trips = trips.saturating_add(1);
                }
                BreakerObservation::Open => assert!(open, "open without trip at {now}"),
                BreakerObservation::Closed => open = false,
            }
            assert_eq!(breaker.is_tripped(), open);
        }
        assert!(trips > 1, "{trips}");
        Ok(())
    }

    #[test]
    fn test_other_partitions_are_unaffected() -> TestResult {
        let mut set = BreakerSet::new(config());
        let noisy = PartitionKey {
            uid: Some(1),
            cgroup_kid: Some(1),
            sensor: "probe-bpf-0".to_owned(),
        };
        let other = PartitionKey {
            uid: Some(2),
            ..noisy.clone()
        };
        let tripped = (0..100)
            .map(|_| set.observe(&noisy, 0))
            .filter(|observation| *observation == BreakerObservation::Tripped)
            .count();
        assert_eq!(tripped, 1);
        assert!(set.is_tripped(&noisy));
        assert_eq!(set.observe(&other, 0), BreakerObservation::Closed);
        assert!(!set.is_tripped(&other));
        Ok(())
    }
}
