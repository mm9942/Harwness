//! Zähler → Raten: die zustandsbehaftete Delta-Stufe vor den Regeln (PL-100
//! WP0.14).
//!
//! # Zweck
//! `harw-dod-cpu`, `harw-dod-netcounters` und `harw-dod-blockio` melden ihre
//! kumulativen Zähler bewusst unverändert und überlassen die Ratenbildung
//! dem Verbraucher (siehe deren Crate-Moduldoku, „Entscheidung 1 —
//! kumulative Zähler, keine Rate": `Sensor::poll` nimmt `&self`, und die
//! Fixture-Harness prüft einen einzelnen, eingefrorenen Zustand). Dieses
//! Modul ist dieser Verbraucher. [`RateStage::derive`] läuft in
//! [`crate::findings::RuleCycle::evaluate`] über die Samples jedes
//! eingefrorenen Deltas, **bevor** die Regeln laufen, und liefert je
//! kumulativem Zähler ein [`HostSample`] namens `"<metric>.rate"`
//! ([`RATE_SUFFIX`], Einheit: Zählereinheit je Sekunde). [`with_rates`]
//! hängt diese Raten hinter die rohen Samples des Deltas; beides zusammen
//! ist `RuleContext::samples`. Die rohen Zähler bleiben unverändert erhalten.
//!
//! # Welche Metriken kumulativ sind
//! Ermittelt aus den Sensor-Crates (Metriknamen dort deklariert, nicht zur
//! Laufzeit geraten; [`is_cumulative`]):
//!
//! | Sensor | kumulativ (Rate) | nicht kumulativ (keine Rate) |
//! |---|---|---|
//! | `harw-dod-cpu` (`/proc/stat`) | alle zehn `*_jiffies` der `cpu`-Aggregatzeile | — |
//! | `harw-dod-netcounters` (`/proc/net/dev`) | `rx_/tx_` `bytes`, `packets`, `errs`, `drop` je `_<iface>` | `netcounters.truncated` |
//! | `harw-dod-blockio` (`<gerät>/stat`) | alle Felder je `_<gerät>` außer `io_in_flight` | `io_in_flight_<gerät>` (aktuelle Warteschlange), `blockio.truncated` |
//! | `harw-dod-memory` (`/proc/meminfo`) | — | alle fünf `*_bytes` (Momentanwerte) |
//!
//! Alle anderen Sensoren (thermal, gpu, cgroup, listener, scanreport,
//! workspace-drift) bekommen keine Rate. Das Label hinter dem Präfix muss
//! aus ASCII-Buchstaben, Ziffern und `_` bestehen (so wie die Sensoren es
//! sanitisieren); eine abgeleitete Rate (`….rate`) ist damit selbst nie
//! wieder kumulativ.
//!
//! # Berechnung
//! `rate = (v_n − v_{n−1}) / (t_n − t_{n−1})`, `t` in Sekunden aus dem
//! `observed_at` der Samples (die injizierte Poll-Zeit, nie die Systemuhr).
//! Zustand je Zähler ist nur der vorige Wert samt Zeitpunkt, Schlüssel
//! (Sensor-ID, Metrikname).
//! - **Erste Beobachtung** eines Zählers: keine Rate, nur Zustand.
//! - **Negatives Delta** (Zählerreset nach Neustart eines Treibers oder
//!   Geräts, Überlauf eines 32-Bit-Zählers): keine Rate; der neue Wert wird
//!   der Bezugspunkt, der nächste Zyklus liefert wieder eine korrekte Rate.
//!   Eine Überlaufkorrektur (`+ 2^32` bzw. `+ 2^64`) wäre geraten, weil die
//!   Zählerbreite nicht bekannt ist; ein ausgelassener Zyklus ist ehrlicher
//!   als eine erfundene Spitze.
//! - **Zeit steht oder läuft rückwärts** (`t_n ≤ t_{n−1}`): wie ein Reset.
//! - **Nicht endlicher Wert**: ignoriert, Zustand unverändert.
//!
//! Der Zustand liegt nur im Speicher: nach einem Neustart des Sentinels
//! beginnt jeder Zähler mit einer ersten Beobachtung. Gingen Samples
//! verloren (Ringüberlauf, übersprungenes Delta), ist die nächste Rate der
//! Durchschnitt über die längere Spanne — nicht falsch, nur gröber.
//!
//! # Begrenzter Zustand
//! Höchstens [`MAX_TRACKED_COUNTERS`] Zähler. Ein neuer Zähler bei voller
//! Tabelle verdrängt den am längsten nicht beobachteten (ältestes
//! `observed_at`, bei Gleichstand der kleinste Schlüssel); so verschwinden
//! die Zähler entfernter Schnittstellen (etwa `veth*`) von selbst. Die
//! Sensoren begrenzen ihre Kardinalität bereits selbst (10 + 16 · 8 + 32 ·
//! 16 Zähler); die Grenze hier ist Verteidigung in der Tiefe.
//!
//! # Genauigkeit
//! Die Werte sind `f64` (wie `HostSample::value`): Zähler oberhalb von
//! `2^53` verlieren Ganzzahlgenauigkeit (bei Netzbytes nach rund 9 PB), die
//! Rate bleibt dann eine Näherung.
//!
//! # Journal
//! Nur Anzahlen (Raten, erste Beobachtungen, Resets, Verdrängungen,
//! gehaltene Zähler) auf `debug`; nie Metriknamen, denn sie tragen
//! Schnittstellen- und Gerätenamen.
//!
//! # Nebenläufigkeit
//! [`RateStage`] lebt in [`crate::findings::RuleCycle`] im Sammelthread;
//! jede Ableitung nimmt `&mut self`. Kein Locking, keine geteilten
//! Ressourcen.
//!
//! # Fehler
//! Keine: ein Sample, aus dem keine Rate entsteht, ist kein Fehler.
//!
//! # Examples
//! ```rust,ignore
//! let mut stage = RateStage::new();
//! let first = stage.derive(&delta_t0.samples); // erste Beobachtung: leer
//! let rates = stage.derive(&delta_t5.samples); // z. B. "user_jiffies.rate"
//! let samples = with_rates(&delta_t5.samples, &rates);
//! ```

use std::borrow::Cow;
use std::collections::BTreeMap;

use harw_dod_signals::HostSample;
use jiff::Timestamp;

/// Suffix, das [`RateStage::derive`] an den Metriknamen des Zählers hängt:
/// aus `"rx_bytes_eth0"` wird `"rx_bytes_eth0.rate"`.
///
/// # Description
/// Der Punkt kommt in keinem sanitisierten Sensorlabel vor; eine Rate
/// kollidiert damit mit keinem rohen Metriknamen und wird von
/// [`is_cumulative`] nie als Zähler erkannt.
pub const RATE_SUFFIX: &str = ".rate";

/// Höchstzahl der Zähler, deren voriger Wert gehalten wird.
///
/// # Description
/// Siehe Moduldoku, Abschnitt „Begrenzter Zustand".
pub const MAX_TRACKED_COUNTERS: usize = 4096;

/// Die zehn kumulativen Spalten der `cpu`-Aggregatzeile von `/proc/stat`.
///
/// Spiegel der privaten `FIELD_METRICS` in `harw-dod-cpu/src/sensor.rs`;
/// der Fixture-Test dieses Moduls prüft den Abgleich.
const CPU_COUNTERS: [&str; 10] = [
    "user_jiffies",
    "nice_jiffies",
    "system_jiffies",
    "idle_jiffies",
    "iowait_jiffies",
    "irq_jiffies",
    "softirq_jiffies",
    "steal_jiffies",
    "guest_jiffies",
    "guest_nice_jiffies",
];

/// Die acht kumulativen Spalten von `/proc/net/dev`, gemeldet als
/// `<spalte>_<iface>`.
///
/// Spiegel der privaten `FIELD_METRICS` in
/// `harw-dod-netcounters/src/sensor.rs`; der Fixture-Test prüft den Abgleich.
const NETCOUNTER_COUNTERS: [&str; 8] = [
    "rx_bytes",
    "rx_packets",
    "rx_errs",
    "rx_drop",
    "tx_bytes",
    "tx_packets",
    "tx_errs",
    "tx_drop",
];

/// Die sechzehn kumulativen Felder einer Blockgeräte-`stat`-Zeile, gemeldet
/// als `<feld>_<gerät>`.
///
/// `harw_dod_blockio::sensor::FIELD_METRICS` ohne `io_in_flight`: das ist
/// die Zahl der gerade laufenden Anfragen, ein Momentanwert, kein Zähler.
const BLOCKIO_COUNTERS: [&str; 16] = [
    "read_ios",
    "read_merges",
    "read_sectors",
    "read_ticks_ms",
    "write_ios",
    "write_merges",
    "write_sectors",
    "write_ticks_ms",
    "io_ticks_ms",
    "time_in_queue_ms",
    "discard_ios",
    "discard_merges",
    "discard_sectors",
    "discard_ticks_ms",
    "flush_ios",
    "flush_ticks_ms",
];

/// Ob `metric` ein kumulativer Zähler eines der Sensoren aus der Moduldoku
/// ist (Abschnitt „Welche Metriken kumulativ sind").
///
/// # Arguments
/// - `metric` (`&str`): `HostSample::metric` eines rohen Samples.
///
/// # Returns
/// `true` für die zehn CPU-Spalten sowie für `<zähler>_<label>` der Netz- und
/// Blockgerätezähler mit einem nicht leeren Label aus ASCII-Buchstaben,
/// Ziffern und `_`; sonst `false`.
///
/// # Examples
/// ```rust,ignore
/// assert!(is_cumulative("rx_bytes_eth0"));
/// assert!(!is_cumulative("io_in_flight_sda"));
/// assert!(!is_cumulative("rx_bytes_eth0.rate"));
/// ```
#[must_use]
pub fn is_cumulative(metric: &str) -> bool {
    CPU_COUNTERS.contains(&metric)
        || NETCOUNTER_COUNTERS
            .iter()
            .chain(BLOCKIO_COUNTERS.iter())
            .copied()
            .any(|counter| has_label_after(metric, counter))
}

/// Ob `metric` die Form `<counter>_<label>` mit einem sanitisierten Label
/// hat (nicht leer, nur ASCII-Buchstaben, Ziffern und `_`).
fn has_label_after(metric: &str, counter: &str) -> bool {
    metric
        .strip_prefix(counter)
        .and_then(|rest| rest.strip_prefix('_'))
        .is_some_and(|label| {
            !label.is_empty()
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
}

/// Die rohen Samples eines Deltas, gefolgt von seinen abgeleiteten Raten,
/// als ein Schnitt für `RuleContext::samples`.
///
/// # Arguments
/// - `raw` (`&[HostSample]`): die Samples des eingefrorenen Deltas.
/// - `rates` (`&[HostSample]`): das Ergebnis von [`RateStage::derive`] über
///   genau diese Samples.
///
/// # Returns
/// `raw` unverändert geliehen, wenn es keine Rate gibt; sonst eine Kopie von
/// `raw` mit `rates` dahinter (Reihenfolge erhalten).
///
/// # Examples
/// ```rust,ignore
/// let samples = with_rates(&delta.evidence.samples, &rates);
/// let ctx = RuleContext { samples: &samples, ..ctx };
/// ```
#[must_use]
pub fn with_rates<'a>(raw: &'a [HostSample], rates: &[HostSample]) -> Cow<'a, [HostSample]> {
    if rates.is_empty() {
        Cow::Borrowed(raw)
    } else {
        Cow::Owned(raw.iter().chain(rates).cloned().collect())
    }
}

/// Schlüssel eines Zählers im Zustand: Sensor-ID und Metrikname.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CounterKey {
    /// `HostSample::sensor` als Text.
    sensor: String,
    /// `HostSample::metric` des rohen Zählers.
    metric: String,
}

/// Der zuletzt beobachtete Stand eines Zählers.
#[derive(Debug, Clone, Copy)]
struct CounterPoint {
    /// Der Zählerstand (endlich).
    value: f64,
    /// `observed_at` des Samples, das diesen Stand trug.
    at: Timestamp,
}

/// Anzahlen einer [`RateStage::derive`]-Runde, nur für das Journal.
#[derive(Debug, Default)]
struct DeriveCounts {
    /// Zähler, die zum ersten Mal (oder nach Verdrängung erneut) auftraten.
    first_observations: u64,
    /// Beobachtungen ohne Rate wegen negativen Deltas oder stehender Zeit.
    resets: u64,
    /// Wegen voller Tabelle verdrängte Zähler.
    evicted: u64,
}

/// Leitet aus kumulativen Zählern Raten je Sekunde ab und hält dafür den
/// vorigen Stand jedes Zählers (siehe Moduldoku).
///
/// # Description
/// Eine Instanz lebt über alle Zyklen in [`crate::findings::RuleCycle`];
/// jedes Delta läuft genau einmal durch [`Self::derive`].
///
/// # Concurrency
/// Nur im Sammelthread; `&mut self` für jede Ableitung.
///
/// # Examples
/// ```rust,ignore
/// let mut stage = RateStage::new();
/// let rates = stage.derive(&delta.evidence.samples);
/// ```
#[derive(Debug)]
pub struct RateStage {
    /// Voriger Stand je Zähler, höchstens `capacity` Einträge.
    last: BTreeMap<CounterKey, CounterPoint>,
    /// Obergrenze von `last` (mindestens 1).
    capacity: usize,
}

impl Default for RateStage {
    fn default() -> Self {
        Self::new()
    }
}

impl RateStage {
    /// Leere Stufe mit der Obergrenze [`MAX_TRACKED_COUNTERS`].
    #[must_use]
    pub fn new() -> Self {
        Self::with_capacity(MAX_TRACKED_COUNTERS)
    }

    /// Leere Stufe mit eigener Obergrenze der gehaltenen Zähler.
    ///
    /// # Arguments
    /// - `capacity` (`usize`): Höchstzahl gehaltener Zähler; `0` wird zu `1`.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            last: BTreeMap::new(),
            capacity: capacity.max(1),
        }
    }

    /// Leitet die Raten der kumulativen Zähler in `samples` ab und merkt sich
    /// deren Stand für das nächste Delta.
    ///
    /// # Description
    /// Läuft in Reihenfolge über `samples`; nicht kumulative Metriken
    /// ([`is_cumulative`]) und nicht endliche Werte werden übergangen. Je
    /// Zähler mit vorigem Stand entsteht eine Rate, außer bei negativem
    /// Delta oder nicht fortschreitender Zeit (dann wird der neue Stand nur
    /// Bezugspunkt). Ein Zähler ohne vorigen Stand wird nur gemerkt; bei
    /// voller Tabelle verdrängt er den am längsten nicht beobachteten.
    ///
    /// # Arguments
    /// - `samples` (`&[HostSample]`): die rohen Samples eines Deltas, in
    ///   Erfassungsreihenfolge.
    ///
    /// # Returns
    /// Je abgeleiteter Rate ein `HostSample` mit dem Sensor und dem
    /// `observed_at` des Zählersamples, dem Namen `"<metric>.rate"` und dem
    /// Wert in Zählereinheit je Sekunde; leer, wenn keine Rate entstand.
    ///
    /// # Concurrency
    /// Siehe Typ-Doku.
    pub fn derive(&mut self, samples: &[HostSample]) -> Vec<HostSample> {
        let mut rates = Vec::new();
        let mut counts = DeriveCounts::default();
        for sample in samples {
            let metric: &str = &sample.metric;
            if !is_cumulative(metric) || !sample.value.is_finite() {
                continue;
            }
            let point = CounterPoint {
                value: sample.value,
                at: sample.observed_at,
            };
            let key = CounterKey {
                sensor: sample.sensor.as_str().to_owned(),
                metric: metric.to_owned(),
            };
            if let Some(previous) = self.last.get_mut(&key) {
                match rate_between(*previous, point) {
                    Some(rate) => rates.push(HostSample {
                        sensor: sample.sensor.clone(),
                        observed_at: sample.observed_at,
                        metric: Cow::Owned(format!("{metric}{RATE_SUFFIX}")),
                        value: rate,
                    }),
                    None => counts.resets = counts.resets.saturating_add(1),
                }
                *previous = point;
                continue;
            }
            if self.last.len() >= self.capacity && self.evict_oldest() {
                counts.evicted = counts.evicted.saturating_add(1);
            }
            self.last.insert(key, point);
            counts.first_observations = counts.first_observations.saturating_add(1);
        }
        tracing::debug!(
            rates = rates.len(),
            first_observations = counts.first_observations,
            resets = counts.resets,
            evicted = counts.evicted,
            tracked = self.last.len(),
            "counter rates derived from this cycle's delta"
        );
        rates
    }

    /// Entfernt den am längsten nicht beobachteten Zähler (ältestes `at`, bei
    /// Gleichstand der kleinste Schlüssel); `true`, wenn einer entfernt wurde.
    fn evict_oldest(&mut self) -> bool {
        let oldest = self
            .last
            .iter()
            .min_by_key(|(_, point)| point.at)
            .map(|(key, _)| key.clone());
        oldest.is_some_and(|key| self.last.remove(&key).is_some())
    }

    /// Zahl der gehaltenen Zähler (nur für Tests).
    #[cfg(test)]
    pub(crate) fn tracked(&self) -> usize {
        self.last.len()
    }
}

/// Die Rate zwischen zwei Ständen desselben Zählers, je Sekunde.
///
/// `None` bei nicht fortschreitender Zeit, negativem Delta (Reset oder
/// Überlauf) oder nicht endlichem Ergebnis.
fn rate_between(previous: CounterPoint, current: CounterPoint) -> Option<f64> {
    let seconds = current.at.duration_since(previous.at).as_secs_f64();
    if seconds <= 0.0 {
        return None;
    }
    let delta = current.value - previous.value;
    if delta < 0.0 {
        return None;
    }
    let rate = delta / seconds;
    rate.is_finite().then_some(rate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx, missing};
    use harw_types::SensorId;

    /// Beliebiger, fester Bezugszeitpunkt der Tests (Sekunden seit Epoche).
    const T0: i64 = 1_700_000_000;

    fn at(offset_secs: i64) -> TestResult<Timestamp> {
        let secs = T0
            .checked_add(offset_secs)
            .ok_or(TestError::Missing("timestamp overflow"))?;
        Timestamp::from_second(secs).map_err(ctx("valid timestamp"))
    }

    fn sample(sensor: &str, metric: &str, offset_secs: i64, value: f64) -> TestResult<HostSample> {
        Ok(HostSample {
            sensor: SensorId::from_str(sensor),
            observed_at: at(offset_secs)?,
            metric: Cow::Owned(metric.to_owned()),
            value,
        })
    }

    fn assert_close(actual: f64, expected: f64) {
        assert!(
            (actual - expected).abs() < 1e-9,
            "rate {actual} differs from {expected}"
        );
    }

    #[test]
    fn test_first_observation_yields_no_rate() -> TestResult {
        let mut stage = RateStage::new();
        let rates = stage.derive(&[sample("cpu-0", "user_jiffies", 0, 100.0)?]);
        assert!(rates.is_empty(), "no previous value, no rate");
        assert_eq!(stage.tracked(), 1, "the first value is remembered");
        Ok(())
    }

    #[test]
    fn test_rate_is_the_delta_per_second() -> TestResult {
        let mut stage = RateStage::new();
        stage.derive(&[sample("netcounters-0", "rx_bytes_eth0", 0, 1_000.0)?]);
        let rates = stage.derive(&[sample("netcounters-0", "rx_bytes_eth0", 5, 6_000.0)?]);

        let rate = rates.first().ok_or_else(missing("one rate sample"))?;
        assert_eq!(rates.len(), 1);
        assert_eq!(rate.metric, "rx_bytes_eth0.rate");
        assert_eq!(rate.sensor.as_str(), "netcounters-0");
        assert_eq!(rate.observed_at, at(5)?);
        assert_close(rate.value, 1_000.0);

        // Ein unveränderter Zähler hat Rate 0, nicht keine Rate.
        let idle = stage.derive(&[sample("netcounters-0", "rx_bytes_eth0", 10, 6_000.0)?]);
        let rate = idle.first().ok_or_else(missing("zero rate sample"))?;
        assert_close(rate.value, 0.0);
        Ok(())
    }

    #[test]
    fn test_counter_reset_yields_no_rate_and_rebases() -> TestResult {
        let mut stage = RateStage::new();
        stage.derive(&[sample("blockio-0", "read_ios_sda", 0, 5_000.0)?]);
        let after_reset = stage.derive(&[sample("blockio-0", "read_ios_sda", 5, 20.0)?]);
        assert!(after_reset.is_empty(), "a negative delta is skipped");

        let rates = stage.derive(&[sample("blockio-0", "read_ios_sda", 10, 70.0)?]);
        let rate = rates.first().ok_or_else(missing("rate after the reset"))?;
        assert_close(rate.value, 10.0);
        Ok(())
    }

    /// Plan-Test WP0.14: die Rate bleibt bei einem Zählerüberlauf korrekt —
    /// keine erfundene Spitze, danach wieder die echte Rate.
    #[test]
    fn test_rate_stays_correct_across_a_counter_wraparound() -> TestResult {
        let near_wrap = f64::from(u32::MAX) - 100.0;
        let mut stage = RateStage::new();
        stage.derive(&[sample(
            "netcounters-0",
            "tx_bytes_eth0",
            0,
            near_wrap - 500.0,
        )?]);
        let before = stage.derive(&[sample("netcounters-0", "tx_bytes_eth0", 5, near_wrap)?]);
        let rate = before.first().ok_or_else(missing("rate before the wrap"))?;
        assert_close(rate.value, 100.0);

        // Der 32-Bit-Zähler läuft über: der Wert fällt auf 400.
        let wrapped = stage.derive(&[sample("netcounters-0", "tx_bytes_eth0", 10, 400.0)?]);
        assert!(wrapped.is_empty(), "no rate across the wrap");

        let after = stage.derive(&[sample("netcounters-0", "tx_bytes_eth0", 15, 900.0)?]);
        let rate = after.first().ok_or_else(missing("rate after the wrap"))?;
        assert_close(rate.value, 100.0);
        Ok(())
    }

    #[test]
    fn test_time_that_does_not_advance_yields_no_rate() -> TestResult {
        let mut stage = RateStage::new();
        stage.derive(&[sample("cpu-0", "idle_jiffies", 5, 100.0)?]);
        assert!(
            stage
                .derive(&[sample("cpu-0", "idle_jiffies", 5, 200.0)?])
                .is_empty()
        );
        assert!(
            stage
                .derive(&[sample("cpu-0", "idle_jiffies", 0, 300.0)?])
                .is_empty()
        );

        let rates = stage.derive(&[sample("cpu-0", "idle_jiffies", 2, 310.0)?]);
        let rate = rates
            .first()
            .ok_or_else(missing("rate after the clock step"))?;
        assert_close(rate.value, 5.0);
        Ok(())
    }

    #[test]
    fn test_gauges_and_rates_are_never_rated() -> TestResult {
        let mut stage = RateStage::new();
        for offset in [0, 5] {
            let rates = stage.derive(&[
                sample("memory-0", "mem_free_bytes", offset, 1_000.0)?,
                sample("blockio-0", "io_in_flight_sda", offset, 3.0)?,
                sample("blockio-0", "blockio.truncated", offset, 0.0)?,
                sample("netcounters-0", "netcounters.truncated", offset, 0.0)?,
                sample("netcounters-0", "rx_bytes_eth0.rate", offset, 10.0)?,
                sample("cgroup-0", "cpu_usage_usec_system_slice", offset, 10.0)?,
                sample("thermal-0", "temperature_celsius_zone0", offset, 40.0)?,
            ]);
            assert!(rates.is_empty());
        }
        assert_eq!(stage.tracked(), 0, "gauges hold no state");
        Ok(())
    }

    #[test]
    fn test_non_finite_values_are_ignored() -> TestResult {
        let mut stage = RateStage::new();
        stage.derive(&[sample("cpu-0", "user_jiffies", 0, 100.0)?]);
        assert!(
            stage
                .derive(&[sample("cpu-0", "user_jiffies", 5, f64::NAN)?])
                .is_empty()
        );

        let rates = stage.derive(&[sample("cpu-0", "user_jiffies", 10, 200.0)?]);
        let rate = rates
            .first()
            .ok_or_else(missing("rate over the ignored sample"))?;
        assert_close(rate.value, 10.0);
        Ok(())
    }

    #[test]
    fn test_counters_are_kept_apart_per_sensor_and_metric() -> TestResult {
        let mut stage = RateStage::new();
        stage.derive(&[
            sample("netcounters-0", "rx_bytes_eth0", 0, 0.0)?,
            sample("netcounters-1", "rx_bytes_eth0", 0, 1_000.0)?,
            sample("netcounters-0", "rx_bytes_eth1", 0, 50.0)?,
        ]);
        let rates = stage.derive(&[
            sample("netcounters-0", "rx_bytes_eth0", 10, 100.0)?,
            sample("netcounters-1", "rx_bytes_eth0", 10, 3_000.0)?,
            sample("netcounters-0", "rx_bytes_eth1", 10, 50.0)?,
        ]);
        let values: Vec<(&str, &str, f64)> = rates
            .iter()
            .map(|rate| (rate.sensor.as_str(), rate.metric.as_ref(), rate.value))
            .collect();
        assert_eq!(values.len(), 3);
        for ((sensor, metric, value), (want_sensor, want_metric, want_value)) in
            values.iter().zip([
                ("netcounters-0", "rx_bytes_eth0.rate", 10.0),
                ("netcounters-1", "rx_bytes_eth0.rate", 200.0),
                ("netcounters-0", "rx_bytes_eth1.rate", 0.0),
            ])
        {
            assert_eq!(*sensor, want_sensor);
            assert_eq!(*metric, want_metric);
            assert_close(*value, want_value);
        }
        Ok(())
    }

    #[test]
    fn test_full_table_evicts_the_counter_seen_longest_ago() -> TestResult {
        let mut stage = RateStage::with_capacity(2);
        stage.derive(&[
            sample("cpu-0", "user_jiffies", 0, 0.0)?,
            sample("cpu-0", "nice_jiffies", 0, 0.0)?,
        ]);
        // `nice` wird aufgefrischt, `system` ist neu und verdrängt `user`.
        let rates = stage.derive(&[
            sample("cpu-0", "nice_jiffies", 5, 50.0)?,
            sample("cpu-0", "system_jiffies", 5, 0.0)?,
        ]);
        assert_eq!(rates.len(), 1);
        assert_eq!(stage.tracked(), 2, "the table never exceeds its bound");

        // `user` war verdrängt: wieder eine erste Beobachtung, keine Rate.
        let rates = stage.derive(&[sample("cpu-0", "user_jiffies", 10, 100.0)?]);
        assert!(rates.is_empty());
        assert_eq!(stage.tracked(), 2);
        Ok(())
    }

    #[test]
    fn test_default_bound_holds_at_most_max_tracked_counters() -> TestResult {
        let mut stage = RateStage::default();
        let overflow = MAX_TRACKED_COUNTERS
            .checked_add(1)
            .ok_or(TestError::Missing("bound overflow"))?;
        let samples = (0..overflow)
            .map(|index| sample("netcounters-0", &format!("rx_bytes_if{index}"), 0, 1.0))
            .collect::<TestResult<Vec<_>>>()?;
        assert!(stage.derive(&samples).is_empty());
        assert_eq!(stage.tracked(), MAX_TRACKED_COUNTERS);
        Ok(())
    }

    #[test]
    fn test_with_rates_borrows_without_rates_and_appends_them_otherwise() -> TestResult {
        let raw = [sample("cpu-0", "user_jiffies", 5, 600.0)?];
        assert!(matches!(with_rates(&raw, &[]), Cow::Borrowed(_)));

        let rates = [sample("cpu-0", "user_jiffies.rate", 5, 100.0)?];
        let combined = with_rates(&raw, &rates);
        let metrics: Vec<&str> = combined.iter().map(|s| s.metric.as_ref()).collect();
        assert_eq!(metrics, ["user_jiffies", "user_jiffies.rate"]);
        Ok(())
    }

    /// Ein `expect.json` der Fixture-Harness (`harw_dod_fixtures`): die
    /// Samples, die der echte Sensor für diesen Baum erzeugt.
    #[derive(serde::Deserialize)]
    struct Fixture {
        reading: FixtureReading,
    }

    /// Der `reading`-Teil eines `expect.json`.
    #[derive(serde::Deserialize)]
    struct FixtureReading {
        samples: Vec<HostSample>,
    }

    /// Teilt die Metriken eines Fixtures in (kumulativ, nicht kumulativ).
    fn classify(json: &str) -> TestResult<(Vec<String>, Vec<String>)> {
        let fixture: Fixture = serde_json::from_str(json).map_err(ctx("expect.json parses"))?;
        let (cumulative, other): (Vec<String>, Vec<String>) = fixture
            .reading
            .samples
            .into_iter()
            .map(|sample| sample.metric.into_owned())
            .partition(|metric| is_cumulative(metric));
        Ok((cumulative, other))
    }

    /// Abgleich mit den Erzeugern: die Metriknamen, die die echten Sensoren
    /// laut ihren Fixtures melden, werden genau wie in der Moduldoku
    /// klassifiziert.
    #[test]
    fn test_classification_matches_the_sensor_fixtures() -> TestResult {
        let (cumulative, other) = classify(include_str!(
            "../../harw-dod-cpu/fixtures/single-core/expect.json"
        ))?;
        assert_eq!(cumulative, CPU_COUNTERS);
        assert!(other.is_empty());

        let (cumulative, other) = classify(include_str!(
            "../../harw-dod-netcounters/fixtures/single-interface/expect.json"
        ))?;
        let expected: Vec<String> = NETCOUNTER_COUNTERS
            .iter()
            .map(|counter| format!("{counter}_lo"))
            .collect();
        assert_eq!(cumulative, expected);
        assert_eq!(other, ["netcounters.truncated"]);

        let (cumulative, other) = classify(include_str!(
            "../../harw-dod-blockio/fixtures/seventeen-fields/expect.json"
        ))?;
        let expected: Vec<String> = BLOCKIO_COUNTERS
            .iter()
            .map(|counter| format!("{counter}_sda"))
            .collect();
        assert_eq!(cumulative, expected);
        assert_eq!(other, ["io_in_flight_sda", "blockio.truncated"]);

        let (cumulative, other) = classify(include_str!(
            "../../harw-dod-memory/fixtures/typical-host/expect.json"
        ))?;
        assert!(cumulative.is_empty(), "meminfo carries gauges only");
        assert_eq!(other.len(), 5);
        Ok(())
    }

    #[test]
    fn test_blockio_counters_are_the_sensor_fields_without_in_flight() {
        let expected: Vec<&str> = harw_dod_blockio::sensor::FIELD_METRICS
            .iter()
            .copied()
            .filter(|field| *field != "io_in_flight")
            .collect();
        assert_eq!(expected, BLOCKIO_COUNTERS);
    }

    #[test]
    fn test_is_cumulative_requires_a_sanitized_label() {
        assert!(is_cumulative("rx_bytes_eth0"));
        assert!(is_cumulative("read_ios_nvme0n1"));
        assert!(is_cumulative("user_jiffies"));
        assert!(!is_cumulative("rx_bytes"), "no label");
        assert!(!is_cumulative("rx_bytes_"), "empty label");
        assert!(!is_cumulative("rx_bytes_eth0.rate"), "a rate is no counter");
        assert!(!is_cumulative("rx_bytesX"), "no separator");
        assert!(
            !is_cumulative("user_jiffies_x"),
            "cpu metrics carry no label"
        );
    }
}
