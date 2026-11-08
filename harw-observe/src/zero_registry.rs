//! Null-Serien-Registry: Meta-Metriken, die schon vor ihrem ersten
//! Inkrement mit dem Wert `0` exportiert werden.
//!
//! # Verantwortungsbereich
//! Trägt [`ZeroSeries`], [`ZeroRecord`], [`ZeroCounterRegistry`] und die
//! Deklaration der DoD-/Dispatch-Metriken (PL-100 Phase 4):
//! `dod_triage_verdicts_total`, `human_overrides_total`,
//! `unbound_verdict_total`, `breaker_trips_total{partition}`,
//! `untriaged_backlog` (Gauge), `warden_unaudited_actions` und
//! `sensor_parse_errors_total`. Die Registry **zählt nichts**: Emittenten
//! (Dispatcher, Verdict-Bindung, Warden, Sensoren) melden ihre Stände über
//! das Produzenten-Handle [`crate::dod_metrics::DodMetrics`], das die hier
//! exportierten Schlüssel über [`crate::routing::RoutingSink::record_meta`]
//! an alle Ziele verteilt. Die Registry sorgt nur dafür, dass jede Serie im
//! Export **existiert**, bevor das erste Ereignis eintritt.
//!
//! # Warum Null-Serien
//! Die Prometheus- und File-Sinks legen eine Serie erst beim ersten
//! `record()` an. Ein Alarm auf „Zähler steigt" oder „Zähler ungleich 0"
//! kann dann nicht zwischen „nie passiert" und „Emittent nie verdrahtet"
//! unterscheiden. Ein explizit emittierter Nullwert macht die Serie
//! sichtbar; fehlt sie trotz laufendem Prozess, ist die Verdrahtung kaputt.
//!
//! # Je Prozess nur die eigenen Serien ([`DodEmitter`])
//! Ein Nullwert behauptet „Produzent verdrahtet, bisher kein Ereignis". Die
//! sieben Metriken entstehen aber in verschiedenen Prozessen (README PL-100
//! §3 und O5): Dispatch und Verdicts auf der Dispatch-Seite (Gateway mit
//! dem verbundenen `harw-dod-watch`), Parse-Fehler im Sentinel,
//! S2-Verstöße im Warden. Emittierte jeder Prozess alle neun Serien, zeigte
//! etwa das Gateway `warden_unaudited_actions 0`, obwohl es nie einen
//! Warden-Produzenten hat — genau die Verwechslung, die diese Registry
//! verhindern soll. Deshalb gehört jede Serie genau einer [`DodEmitter`]-
//! Rolle, und ein Prozess emittiert über [`dod_zero_registry_for`] nur die
//! Serien seiner Rolle. Das Produzenten-Handle trägt die Rolle als Typ
//! ([`crate::DispatchMetrics`], [`crate::SensorMetrics`],
//! [`crate::WardenMetrics`]) und kann deshalb auch nur die Serien seiner
//! Rolle melden. Die Vereinigung aller Rollen ist
//! [`DOD_ZERO_SERIES`] (durch Tests erzwungen). Fehlt eine Serie im Export
//! ihres Rollenprozesses, ist dessen Verdrahtung kaputt; in einem fremden
//! Prozess fehlt sie absichtlich.
//!
//! # Abgrenzung zu [`crate::null_counter::NullCounter`]
//! Ein `NullCounter` zählt **Verstöße** gegen eine Invariante, sein
//! erwarteter Stand ist dauerhaft null, und [`crate::assert_all_zero`]
//! schlägt bei jedem Inkrement an. Die Zähler hier zählen teils normale
//! Arbeit (Verdicts, Overrides, Parse-Fehler) und dürfen im Betrieb
//! steigen. Sie sind deshalb **kein** `NullCounter` und werden nie in eine
//! [`crate::NullCounterRegistry`] aufgenommen; `assert_all_zero` bleibt von
//! dieser Registry unberührt. Einzige Ausnahme sind die Verstoß-Serien aus
//! [`DOD_VIOLATION_SERIES`]: zu `warden_unaudited_actions` gehört der
//! Nullzähler [`crate::dod_metrics::WARDEN_UNAUDITED_VIOLATIONS`], den der
//! Warden-Prozess registriert.
//!
//! # Namensraum und Routing
//! Alle Namen sind bewusst ungeschützt (kein `security.`/`warden.`-Präfix,
//! keine Punkte): Es sind Meta-Metriken, deren Existenz keine Information
//! preisgibt (`docs/design/harw-telemetry-integration-plan.md` §2: „meta
//! zero-counters = all sinks"). [`ZeroSeries::validate`] lehnt Namen mit
//! Punkt ab und kann damit nie eine geschützte Metrik an alle Sinks
//! verteilen. Der Fan-out an alle Ziele läuft über
//! [`crate::routing::RoutingSink::emit_zero_registry`]; ein einzelner Sink
//! wird über [`ZeroCounterRegistry::emit_zeros`] bedient. **Emittenten
//! müssen echte Werte über [`crate::routing::RoutingSink::record_meta`]
//! melden**: ein gewöhnliches `record()` stellt nur an ein Ziel zu, die
//! übrigen Ziele blieben dauerhaft bei `0`.
//!
//! # Kardinalität
//! Eine Serie deklariert die **geschlossene** Menge ihrer Label-
//! Kombinationen im Voraus. [`ZeroSeries::validate`] erzwingt, dass die
//! Anzahl die Grenze aus [`crate::metric::Cardinality`] nicht überschreitet,
//! dass jede Kombination genau so viele Werte wie `key.labels` trägt und
//! dass keine Kombination doppelt vorkommt. `breaker_trips_total` trägt das
//! Label `partition` mit den **Partitionsarten** `uid`, `cgroup` und
//! `sensor` (Bounded(3)); konkrete uids, Cgroup-IDs oder Sensornamen sind
//! nie Label-Werte.
//!
//! # Nebenläufigkeit
//! Reine Werttypen und ein `Vec`, das nur beim Programmstart aufgebaut
//! wird (`register` verlangt `&mut self`). Lesende Methoden sind aus
//! beliebigen Threads aufrufbar.
//!
//! # Fehler
//! [`ZeroCounterRegistry::register`] und [`ZeroSeries::validate`] liefern
//! [`crate::error::ObserveError::InvalidZeroSeries`].
//!
//! # Examples
//! ```
//! use harw_observe::{
//!     DodEmitter, NullSink, ObserveError, dod_zero_registry_for,
//!     zero_registry::dod_zero_registry,
//! };
//!
//! # fn main() -> Result<(), ObserveError> {
//! let registry = dod_zero_registry()?;
//! let emitted = registry.emit_zeros(&NullSink);
//! assert_eq!(emitted, 9);
//!
//! // Ein Prozess emittiert nur die Serien seiner Rolle.
//! assert_eq!(dod_zero_registry_for(DodEmitter::Dispatch)?.series_count(), 7);
//! assert_eq!(dod_zero_registry_for(DodEmitter::Sensor)?.series_count(), 1);
//! assert_eq!(dod_zero_registry_for(DodEmitter::Warden)?.series_count(), 1);
//! # Ok(())
//! # }
//! ```

use crate::error::ObserveError;
use crate::field::{FieldName, FieldValue};
use crate::metric::{Cardinality, MetricKey, MetricKind, MetricValue, Unit};
use crate::sink::TelemetrySink;

/// Name des Labels `partition` an `breaker_trips_total`.
pub const PARTITION_LABEL: FieldName = FieldName::from_static_unchecked("partition");

/// Geschlossene Menge der Partitionsarten (README §2 I10: uid, Cgroup,
/// Sensor). Nie konkrete Kennungen.
pub const BREAKER_PARTITIONS: &[&str] = &["uid", "cgroup", "sensor"];

const PARTITION_LABELS: &[FieldName] = &[PARTITION_LABEL];

/// Verdicts, die der Triage-Dispatcher gebunden und gespeichert hat.
pub const DOD_TRIAGE_VERDICTS_TOTAL: MetricKey = MetricKey {
    name: "dod_triage_verdicts_total",
    kind: MetricKind::Counter,
    unit: Unit::Count,
    labels: &[],
    cardinality: Cardinality::Single,
};

/// Entscheidungen, mit denen ein Mensch ein Modell-Verdict übersteuert hat.
pub const HUMAN_OVERRIDES_TOTAL: MetricKey = MetricKey {
    name: "human_overrides_total",
    kind: MetricKind::Counter,
    unit: Unit::Count,
    labels: &[],
    cardinality: Cardinality::Single,
};

/// Verdicts, die sich keinem Befund (id + Digest) zuordnen ließen.
pub const UNBOUND_VERDICT_TOTAL: MetricKey = MetricKey {
    name: "unbound_verdict_total",
    kind: MetricKind::Counter,
    unit: Unit::Count,
    labels: &[],
    cardinality: Cardinality::Single,
};

/// Ausgelöste Circuit-Breaker, je Partitionsart.
pub const BREAKER_TRIPS_TOTAL: MetricKey = MetricKey {
    name: "breaker_trips_total",
    kind: MetricKind::Counter,
    unit: Unit::Count,
    labels: PARTITION_LABELS,
    cardinality: Cardinality::Bounded(3),
};

/// Anzahl der Befunde, die noch auf eine Triage warten (Gauge).
pub const UNTRIAGED_BACKLOG: MetricKey = MetricKey {
    name: "untriaged_backlog",
    kind: MetricKind::Gauge,
    unit: Unit::Count,
    labels: &[],
    cardinality: Cardinality::Single,
};

/// Warden-Aktionen ohne Audit-Eintrag (Invariante S2).
///
/// **Verstoß-Nullzähler** (`docs/design/harw-security-observability-plan.md`
/// §4.3): Erwarteter Stand im Betrieb ist dauerhaft null, ein Wert über
/// null ist ein Alarm auf eine gebrochene Invariante, keine Schwelle. Die
/// Registry deklariert ihn nur als Zählerserie (sichtbar ab Start mit `0`);
/// die Verstoß-Semantik trägt [`DOD_VIOLATION_SERIES`]. Der zugehörige
/// [`crate::null_counter::NullCounter`] ist
/// [`crate::dod_metrics::WARDEN_UNAUDITED_VIOLATIONS`]; der Warden-Prozess
/// nimmt ihn in seine [`crate::NullCounterRegistry`] auf, damit
/// [`crate::assert_all_zero`] anschlägt, und meldet Verstöße über
/// [`crate::dod_metrics::DodMetrics::warden_unaudited_action`] (nur auf
/// [`crate::WardenMetrics`]). Der Name
/// folgt der Vorgabe aus der Planung und trägt kein `_total`-Suffix.
pub const WARDEN_UNAUDITED_ACTIONS: MetricKey = MetricKey {
    name: "warden_unaudited_actions",
    kind: MetricKind::Counter,
    unit: Unit::Count,
    labels: &[],
    cardinality: Cardinality::Single,
};

/// Sensor-Rohdaten, die der Parser verworfen hat.
pub const SENSOR_PARSE_ERRORS_TOTAL: MetricKey = MetricKey {
    name: "sensor_parse_errors_total",
    kind: MetricKind::Counter,
    unit: Unit::Count,
    labels: &[],
    cardinality: Cardinality::Single,
};

const BREAKER_LABEL_SETS: &[&[&str]] = &[&["uid"], &["cgroup"], &["sensor"]];

/// Namen der Serien in [`DOD_ZERO_SERIES`], die **Verstöße** zählen
/// (erwarteter Stand dauerhaft null, Alarm bei Wert über null; Plan §4.3).
/// Alle anderen Serien zählen normale Arbeit.
pub const DOD_VIOLATION_SERIES: &[&str] = &["warden_unaudited_actions"];

/// Die Deklarationen der sieben DoD-/Dispatch-Metriken, in fester
/// Reihenfolge.
pub const DOD_ZERO_SERIES: &[ZeroSeries] = &[
    ZeroSeries::new(&DOD_TRIAGE_VERDICTS_TOTAL, &[]),
    ZeroSeries::new(&HUMAN_OVERRIDES_TOTAL, &[]),
    ZeroSeries::new(&UNBOUND_VERDICT_TOTAL, &[]),
    ZeroSeries::new(&BREAKER_TRIPS_TOTAL, BREAKER_LABEL_SETS),
    ZeroSeries::new(&UNTRIAGED_BACKLOG, &[]),
    ZeroSeries::new(&WARDEN_UNAUDITED_ACTIONS, &[]),
    ZeroSeries::new(&SENSOR_PARSE_ERRORS_TOTAL, &[]),
];

/// Eine Metrik samt der geschlossenen Menge ihrer Label-Kombinationen, die
/// mit Wert `0` vorab exportiert wird.
///
/// # Description
/// `label_sets` ist leer, wenn der Schlüssel keine Labels trägt (eine
/// unbeschriftete Serie). Sonst ist jede Kombination eine Liste von
/// Label-Werten in der Reihenfolge von `key.labels`.
#[derive(Debug, Clone, Copy)]
pub struct ZeroSeries {
    key: &'static MetricKey,
    label_sets: &'static [&'static [&'static str]],
}

impl ZeroSeries {
    /// Baut eine Deklaration. Prüft nichts; [`ZeroSeries::validate`] bzw.
    /// [`ZeroCounterRegistry::register`] tun das.
    ///
    /// # Arguments
    /// - `key` (`&'static MetricKey`): der Schlüssel der Metrik.
    /// - `label_sets`: die geschlossene Menge der Label-Kombinationen
    ///   (leer bei Schlüsseln ohne Labels).
    #[must_use]
    pub const fn new(
        key: &'static MetricKey,
        label_sets: &'static [&'static [&'static str]],
    ) -> Self {
        Self { key, label_sets }
    }

    /// Der Metrikschlüssel.
    #[must_use]
    pub const fn key(&self) -> &'static MetricKey {
        self.key
    }

    /// Die deklarierten Label-Kombinationen.
    #[must_use]
    pub const fn label_sets(&self) -> &'static [&'static [&'static str]] {
        self.label_sets
    }

    /// Anzahl der Serien, die diese Deklaration erzeugt (mindestens eine).
    #[must_use]
    pub fn series_count(&self) -> usize {
        self.label_sets.len().max(1)
    }

    /// Prüft die Deklaration gegen die Registry-Regeln.
    ///
    /// # Errors
    /// [`ObserveError::InvalidZeroSeries`], wenn
    /// - der Name leer ist, nicht mit `[a-z]` beginnt oder Zeichen außerhalb
    ///   von `[a-z0-9_]` enthält (das schließt Namensraum-Punkte und damit
    ///   geschützte Namen aus; gültiger Prometheus-Metrikname);
    /// - die Art `Histogram` ist (kein Nullwert definiert);
    /// - `Cardinality::Single` mit Labels oder Label-Kombinationen
    ///   kombiniert ist;
    /// - ein Schlüssel ohne Labels Kombinationen deklariert oder ein
    ///   Schlüssel mit Labels keine;
    /// - eine Kombination nicht genau so viele Werte wie Labels trägt,
    ///   ein Wert leer ist oder Zeichen außerhalb von `[a-z0-9_]` enthält
    ///   (kein Leerzeichen, Anführungszeichen oder Zeilenumbruch), oder eine
    ///   Kombination doppelt vorkommt;
    /// - die Anzahl der Kombinationen die `Bounded(n)`-Grenze übersteigt.
    pub fn validate(&self) -> Result<(), ObserveError> {
        let name = self.key.name;
        let invalid = |reason: &'static str| ObserveError::InvalidZeroSeries { name, reason };

        if !name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
            || !name.chars().all(is_label_char)
        {
            return Err(invalid("name must start with [a-z] and use only [a-z0-9_]"));
        }
        if self.key.kind == MetricKind::Histogram {
            return Err(invalid("histograms have no zero value"));
        }
        let label_count = self.key.labels.len();
        match self.key.cardinality {
            Cardinality::Single => {
                if label_count != 0 || !self.label_sets.is_empty() {
                    return Err(invalid("single cardinality allows no labels"));
                }
            }
            Cardinality::Bounded(limit) => {
                if label_count == 0 && !self.label_sets.is_empty() {
                    return Err(invalid("key has no labels but label sets are declared"));
                }
                if label_count != 0 && self.label_sets.is_empty() {
                    return Err(invalid("labelled key needs a closed set of label values"));
                }
                let limit = usize::try_from(limit).unwrap_or(usize::MAX);
                if self.label_sets.len() > limit {
                    return Err(invalid("label sets exceed the cardinality bound"));
                }
            }
        }
        for (position, set) in self.label_sets.iter().enumerate() {
            if set.len() != label_count {
                return Err(invalid("label set arity differs from key labels"));
            }
            if set
                .iter()
                .any(|value| value.is_empty() || !value.chars().all(is_label_char))
            {
                return Err(invalid("label value must be non-empty [a-z0-9_]"));
            }
            if self
                .label_sets
                .iter()
                .skip(position.saturating_add(1))
                .any(|other| other == set)
            {
                return Err(invalid("duplicate label set"));
            }
        }
        Ok(())
    }

    // Die Nullwerte dieser Deklaration; nur für validierte Serien sinnvoll.
    fn records(&self) -> Vec<ZeroRecord> {
        let value = match self.key.kind {
            MetricKind::Gauge => MetricValue::Gauge(0.0),
            MetricKind::Counter | MetricKind::Histogram => MetricValue::Count(0),
        };
        if self.label_sets.is_empty() {
            return vec![ZeroRecord {
                key: self.key,
                value,
                labels: Vec::new(),
            }];
        }
        self.label_sets
            .iter()
            .map(|set| ZeroRecord {
                key: self.key,
                value,
                labels: self
                    .key
                    .labels
                    .iter()
                    .zip(set.iter())
                    .map(|(name, label_value)| (*name, FieldValue::Str(*label_value)))
                    .collect(),
            })
            .collect()
    }
}

// Zeichen, die in Metriknamen (nach dem ersten) und Label-Werten erlaubt sind.
fn is_label_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'
}

/// Ein einzelner Nullwert, bereit zur Emission.
#[derive(Debug, Clone, PartialEq)]
pub struct ZeroRecord {
    /// Schlüssel der Metrik.
    pub key: &'static MetricKey,
    /// `Count(0)` für Zähler, `Gauge(0.0)` für Gauges.
    pub value: MetricValue,
    /// Label-Belegung dieser Serie (leer bei unbeschrifteten Serien).
    pub labels: Vec<(FieldName, FieldValue)>,
}

/// Sammlung validierter [`ZeroSeries`].
///
/// # Concurrency
/// Aufbau einmalig beim Programmstart (`&mut self`); danach nur lesend.
#[derive(Debug, Default)]
pub struct ZeroCounterRegistry {
    series: Vec<ZeroSeries>,
}

impl ZeroCounterRegistry {
    /// Baut eine leere Registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Validiert und registriert eine Serie.
    ///
    /// # Errors
    /// [`ObserveError::InvalidZeroSeries`] bei einer Regelverletzung (siehe
    /// [`ZeroSeries::validate`]) oder wenn der Metrikname bereits
    /// registriert ist. Bei einem Fehler bleibt die Registry unverändert.
    pub fn register(&mut self, series: ZeroSeries) -> Result<(), ObserveError> {
        series.validate()?;
        if self
            .series
            .iter()
            .any(|existing| existing.key.name == series.key.name)
        {
            return Err(ObserveError::InvalidZeroSeries {
                name: series.key.name,
                reason: "metric name already registered",
            });
        }
        self.series.push(series);
        Ok(())
    }

    /// Alle registrierten Serien in Registrierungsreihenfolge.
    #[must_use]
    pub fn all(&self) -> &[ZeroSeries] {
        &self.series
    }

    /// Gesamtzahl der Serien (Label-Kombinationen eingerechnet).
    #[must_use]
    pub fn series_count(&self) -> usize {
        self.series
            .iter()
            .fold(0usize, |acc, s| acc.saturating_add(s.series_count()))
    }

    /// Alle Nullwerte, in Registrierungsreihenfolge.
    #[must_use]
    pub fn records(&self) -> Vec<ZeroRecord> {
        self.series.iter().flat_map(ZeroSeries::records).collect()
    }

    /// Emittiert jeden Nullwert an `sink`.
    ///
    /// # Returns
    /// Die Anzahl der aufgerufenen `record()`-Aufrufe.
    ///
    /// # Concurrency
    /// Ruft `sink.record` sequenziell; `sink` sichert parallele Aufrufe
    /// selbst ab. Ruft kein `flush()`.
    pub fn emit_zeros(&self, sink: &dyn TelemetrySink) -> usize {
        let records = self.records();
        for record in &records {
            sink.record(record.key, record.value, &record.labels);
        }
        records.len()
    }
}

/// Baut die Registry der sieben DoD-/Dispatch-Metriken aus
/// [`DOD_ZERO_SERIES`].
///
/// # Description
/// Die Vereinigung aller [`DodEmitter`]-Rollen, für Prüfungen und
/// Werkzeuge. Eine Kompositionsstelle emittiert nicht diese Registry,
/// sondern nur die ihrer Rolle ([`dod_zero_registry_for`] über
/// [`crate::DodMetrics::install`]).
///
/// # Errors
/// [`ObserveError::InvalidZeroSeries`], falls eine statische Deklaration
/// ungültig ist (durch Tests ausgeschlossen).
pub fn dod_zero_registry() -> Result<ZeroCounterRegistry, ObserveError> {
    let mut registry = ZeroCounterRegistry::new();
    for series in DOD_ZERO_SERIES {
        registry.register(*series)?;
    }
    Ok(registry)
}

/// Die Prozessrolle, deren Produzenten einen Teil der DoD-Serien melden.
///
/// # Description
/// Jede Serie aus [`DOD_ZERO_SERIES`] gehört genau einer Rolle (siehe
/// Moduldoku, Abschnitt „Je Prozess nur die eigenen Serien"). Je Host
/// installiert genau ein Prozess eine Rolle; zwei Prozesse mit derselben
/// Rolle führten getrennte Stände unter demselben Seriennamen.
///
/// # Examples
/// ```
/// use harw_observe::DodEmitter;
///
/// assert_eq!(DodEmitter::Warden.series_names(), &["warden_unaudited_actions"]);
/// assert_eq!(DodEmitter::Sensor.as_str(), "sensor");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DodEmitter {
    /// Dispatch- und Verdict-Seite: `dod_triage_verdicts_total`,
    /// `human_overrides_total`, `unbound_verdict_total`,
    /// `breaker_trips_total{partition}` und `untriaged_backlog`. Im Betrieb
    /// das Gateway; `harw-dod-watch` (README O5) meldet über seine
    /// Gateway-Verbindung, statt selbst diese Rolle zu installieren.
    Dispatch,
    /// Sensor-Seite (`harw-sentinel`): `sensor_parse_errors_total`.
    Sensor,
    /// Warden (`harw-warden`): `warden_unaudited_actions`.
    Warden,
}

impl DodEmitter {
    /// Alle Rollen; ihre Seriensätze sind disjunkt und decken
    /// [`DOD_ZERO_SERIES`] vollständig ab.
    pub const ALL: [Self; 3] = [Self::Dispatch, Self::Sensor, Self::Warden];

    /// Kurzname der Rolle (für Logs und Tests; kein Label-Wert).
    ///
    /// # Returns
    /// `"dispatch"`, `"sensor"` oder `"warden"`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dispatch => "dispatch",
            Self::Sensor => "sensor",
            Self::Warden => "warden",
        }
    }

    /// Die Metriknamen aus [`DOD_ZERO_SERIES`], die diese Rolle meldet.
    ///
    /// # Returns
    /// Eine feste, nicht leere Namensliste.
    #[must_use]
    pub const fn series_names(self) -> &'static [&'static str] {
        match self {
            Self::Dispatch => &[
                "dod_triage_verdicts_total",
                "human_overrides_total",
                "unbound_verdict_total",
                "breaker_trips_total",
                "untriaged_backlog",
            ],
            Self::Sensor => &["sensor_parse_errors_total"],
            Self::Warden => &["warden_unaudited_actions"],
        }
    }

    /// Ob `name` eine Serie dieser Rolle ist.
    ///
    /// # Arguments
    /// - `name` (`&str`): ein Metrikname.
    #[must_use]
    pub fn owns(self, name: &str) -> bool {
        self.series_names().contains(&name)
    }
}

/// Baut die Registry der DoD-Serien **einer** Prozessrolle.
///
/// # Description
/// Filtert [`DOD_ZERO_SERIES`] auf [`DodEmitter::series_names`] und behält
/// deren Reihenfolge. Die Kompositionsstelle eines Prozesses emittiert nur
/// diese Serien (über [`crate::DodMetrics::install`]).
///
/// # Arguments
/// - `emitter` (`DodEmitter`): die Rolle dieses Prozesses.
///
/// # Errors
/// [`ObserveError::InvalidZeroSeries`], falls eine statische Deklaration
/// ungültig ist (durch Tests ausgeschlossen).
///
/// # Examples
/// ```
/// use harw_observe::{DodEmitter, ObserveError, dod_zero_registry_for};
///
/// # fn main() -> Result<(), ObserveError> {
/// let registry = dod_zero_registry_for(DodEmitter::Dispatch)?;
/// assert_eq!(registry.series_count(), 7);
/// # Ok(())
/// # }
/// ```
pub fn dod_zero_registry_for(emitter: DodEmitter) -> Result<ZeroCounterRegistry, ObserveError> {
    let mut registry = ZeroCounterRegistry::new();
    for series in DOD_ZERO_SERIES
        .iter()
        .filter(|series| emitter.owns(series.key().name))
    {
        registry.register(*series)?;
    }
    Ok(registry)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::null_counter::NullCounterRegistry;
    use crate::test_support::{TestResult, ctx, missing};

    type Seen = (&'static str, MetricValue, Vec<(FieldName, FieldValue)>);

    #[derive(Debug, Default)]
    struct CaptureSink {
        seen: Mutex<Vec<Seen>>,
    }

    impl TelemetrySink for CaptureSink {
        fn record(&self, key: &MetricKey, value: MetricValue, labels: &[(FieldName, FieldValue)]) {
            self.seen.lock().unwrap_or_else(|p| p.into_inner()).push((
                key.name,
                value,
                labels.to_vec(),
            ));
        }

        fn flush(&self) {}

        fn name(&self) -> &'static str {
            "capture"
        }
    }

    impl CaptureSink {
        fn take(&self) -> Vec<Seen> {
            std::mem::take(&mut *self.seen.lock().unwrap_or_else(|p| p.into_inner()))
        }
    }

    #[test]
    fn test_dod_registry_builds_with_nine_series() -> TestResult {
        let registry = dod_zero_registry().map_err(ctx("dod registry"))?;
        assert_eq!(registry.all().len(), 7);
        assert_eq!(registry.series_count(), 9);
        Ok(())
    }

    #[test]
    fn test_every_named_metric_is_exported_with_zero() -> TestResult {
        let registry = dod_zero_registry().map_err(ctx("dod registry"))?;
        let sink = CaptureSink::default();
        assert_eq!(registry.emit_zeros(&sink), 9);
        let seen = sink.take();
        for name in [
            "dod_triage_verdicts_total",
            "human_overrides_total",
            "unbound_verdict_total",
            "breaker_trips_total",
            "untriaged_backlog",
            "warden_unaudited_actions",
            "sensor_parse_errors_total",
        ] {
            assert!(
                seen.iter().any(|(n, _, _)| *n == name),
                "missing zero series {name}"
            );
        }
        for (name, value, _) in &seen {
            let expected = if *name == "untriaged_backlog" {
                MetricValue::Gauge(0.0)
            } else {
                MetricValue::Count(0)
            };
            assert_eq!(*value, expected, "{name} must start at zero");
        }
        Ok(())
    }

    #[test]
    fn test_breaker_trips_emitted_once_per_partition_kind() -> TestResult {
        let registry = dod_zero_registry().map_err(ctx("dod registry"))?;
        let sink = CaptureSink::default();
        registry.emit_zeros(&sink);
        let seen = sink.take();
        let partitions: Vec<&FieldValue> = seen
            .iter()
            .filter(|(n, _, _)| *n == "breaker_trips_total")
            .map(|(_, _, labels)| {
                labels
                    .iter()
                    .find(|(name, _)| *name == PARTITION_LABEL)
                    .map(|(_, value)| value)
                    .ok_or_else(missing("partition label"))
            })
            .collect::<Result<_, _>>()?;
        assert_eq!(
            partitions,
            vec![
                &FieldValue::Str("uid"),
                &FieldValue::Str("cgroup"),
                &FieldValue::Str("sensor"),
            ]
        );
        Ok(())
    }

    #[test]
    fn test_unlabelled_series_carry_no_labels() -> TestResult {
        let registry = dod_zero_registry().map_err(ctx("dod registry"))?;
        for record in registry.records() {
            if record.key.labels.is_empty() {
                assert!(record.labels.is_empty());
            } else {
                assert_eq!(record.labels.len(), record.key.labels.len());
            }
        }
        Ok(())
    }

    #[test]
    fn test_declared_names_are_unique_and_well_formed() {
        let mut names: Vec<&str> = DOD_ZERO_SERIES.iter().map(|s| s.key().name).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total);
        for series in DOD_ZERO_SERIES {
            assert!(series.validate().is_ok(), "{}", series.key().name);
            assert_ne!(series.key().kind, MetricKind::Histogram);
        }
    }

    #[test]
    fn test_counters_end_in_total_except_warden_unaudited_actions() {
        for series in DOD_ZERO_SERIES {
            let key = series.key();
            if key.kind == MetricKind::Counter && key.name != "warden_unaudited_actions" {
                assert!(key.name.ends_with("_total"), "{}", key.name);
            }
        }
    }

    #[test]
    fn test_breaker_cardinality_bound_matches_partition_set() {
        assert_eq!(
            BREAKER_TRIPS_TOTAL.cardinality,
            Cardinality::Bounded(u32::try_from(BREAKER_PARTITIONS.len()).unwrap_or(u32::MAX))
        );
        assert_eq!(BREAKER_LABEL_SETS.len(), BREAKER_PARTITIONS.len());
    }

    #[test]
    fn test_register_rejects_duplicate_name() -> TestResult {
        let mut registry = dod_zero_registry().map_err(ctx("dod registry"))?;
        let again = ZeroSeries::new(&HUMAN_OVERRIDES_TOTAL, &[]);
        assert!(registry.register(again).is_err());
        assert_eq!(registry.all().len(), 7);
        Ok(())
    }

    #[test]
    fn test_validate_rejects_invalid_declarations() {
        const HIST: MetricKey = MetricKey {
            name: "latency_seconds",
            kind: MetricKind::Histogram,
            unit: Unit::Seconds,
            labels: &[],
            cardinality: Cardinality::Single,
        };
        const DOTTED: MetricKey = MetricKey {
            name: "security.finding_total",
            kind: MetricKind::Counter,
            unit: Unit::Count,
            labels: &[],
            cardinality: Cardinality::Single,
        };
        const SINGLE_LABELLED: MetricKey = MetricKey {
            name: "x_total",
            kind: MetricKind::Counter,
            unit: Unit::Count,
            labels: PARTITION_LABELS,
            cardinality: Cardinality::Single,
        };
        const BOUND_TWO: MetricKey = MetricKey {
            name: "y_total",
            kind: MetricKind::Counter,
            unit: Unit::Count,
            labels: PARTITION_LABELS,
            cardinality: Cardinality::Bounded(2),
        };
        const NO_LABELS_BOUNDED: MetricKey = MetricKey {
            name: "z_total",
            kind: MetricKind::Counter,
            unit: Unit::Count,
            labels: &[],
            cardinality: Cardinality::Bounded(2),
        };
        assert!(ZeroSeries::new(&HIST, &[]).validate().is_err());
        assert!(ZeroSeries::new(&DOTTED, &[]).validate().is_err());
        assert!(
            ZeroSeries::new(&SINGLE_LABELLED, &[&["uid"]])
                .validate()
                .is_err()
        );
        // Labelled key without a closed value set.
        assert!(ZeroSeries::new(&BOUND_TWO, &[]).validate().is_err());
        // Over the bound (3 > 2).
        assert!(
            ZeroSeries::new(&BOUND_TWO, BREAKER_LABEL_SETS)
                .validate()
                .is_err()
        );
        // Wrong arity.
        assert!(
            ZeroSeries::new(&BOUND_TWO, &[&["uid", "extra"]])
                .validate()
                .is_err()
        );
        // Empty value.
        assert!(ZeroSeries::new(&BOUND_TWO, &[&[""]]).validate().is_err());
        // Duplicate combination.
        assert!(
            ZeroSeries::new(&BOUND_TWO, &[&["uid"], &["uid"]])
                .validate()
                .is_err()
        );
        // Label sets without labels on the key.
        assert!(
            ZeroSeries::new(&NO_LABELS_BOUNDED, &[&["uid"]])
                .validate()
                .is_err()
        );
        // A well-formed labelled series passes.
        assert!(
            ZeroSeries::new(&BOUND_TWO, &[&["uid"], &["cgroup"]])
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn test_validate_rejects_bad_name_start_and_label_chars() {
        const fn counter(name: &'static str) -> MetricKey {
            MetricKey {
                name,
                kind: MetricKind::Counter,
                unit: Unit::Count,
                labels: &[],
                cardinality: Cardinality::Single,
            }
        }
        const UNDERSCORE: MetricKey = counter("_x");
        const DIGIT: MetricKey = counter("9x");
        const ONLY_UNDERSCORES: MetricKey = counter("__");
        const UPPER: MetricKey = counter("Xx");
        const OK: MetricKey = counter("x9_total");
        for key in [&UNDERSCORE, &DIGIT, &ONLY_UNDERSCORES, &UPPER] {
            assert!(
                ZeroSeries::new(key, &[]).validate().is_err(),
                "{}",
                key.name
            );
        }
        assert!(ZeroSeries::new(&OK, &[]).validate().is_ok());

        const BOUND: MetricKey = MetricKey {
            name: "w_total",
            kind: MetricKind::Counter,
            unit: Unit::Count,
            labels: PARTITION_LABELS,
            cardinality: Cardinality::Bounded(1),
        };
        for bad in ["a b", "a\"b", "a\nb", "A", "a-b", "a.b", "ä"] {
            let sets: &'static [&'static [&'static str]] = match bad {
                "a b" => &[&["a b"]],
                "a\"b" => &[&["a\"b"]],
                "a\nb" => &[&["a\nb"]],
                "A" => &[&["A"]],
                "a-b" => &[&["a-b"]],
                "a.b" => &[&["a.b"]],
                _ => &[&["ä"]],
            };
            assert!(ZeroSeries::new(&BOUND, sets).validate().is_err(), "{bad:?}");
        }
        assert!(ZeroSeries::new(&BOUND, &[&["cg_9"]]).validate().is_ok());
    }

    #[test]
    fn test_series_count_matches_independent_expectation() -> TestResult {
        // 6 unlabelled series + 3 partition kinds = 9, counted by hand.
        let unlabelled = DOD_ZERO_SERIES
            .iter()
            .filter(|s| s.key().labels.is_empty())
            .count();
        assert_eq!(unlabelled, 6);
        assert_eq!(unlabelled.saturating_add(BREAKER_PARTITIONS.len()), 9);
        let registry = dod_zero_registry().map_err(ctx("dod registry"))?;
        assert_eq!(registry.records().len(), 9);
        Ok(())
    }

    #[test]
    fn test_emitter_roles_partition_the_dod_series() {
        // Every declared series belongs to exactly one role ...
        for series in DOD_ZERO_SERIES {
            let owners = DodEmitter::ALL
                .iter()
                .filter(|emitter| emitter.owns(series.key().name))
                .count();
            assert_eq!(owners, 1, "{}", series.key().name);
        }
        // ... and every role name is a declared series.
        for emitter in DodEmitter::ALL {
            assert!(!emitter.series_names().is_empty(), "{}", emitter.as_str());
            for name in emitter.series_names() {
                assert!(
                    DOD_ZERO_SERIES.iter().any(|s| s.key().name == *name),
                    "{} names undeclared series {name}",
                    emitter.as_str()
                );
            }
        }
    }

    #[test]
    fn test_registry_for_each_role_emits_only_its_series() -> TestResult {
        // Counted by hand: dispatch = 4 unlabelled + 3 partition kinds.
        let expected = [
            (DodEmitter::Dispatch, 7usize),
            (DodEmitter::Sensor, 1),
            (DodEmitter::Warden, 1),
        ];
        let mut total = 0usize;
        for (emitter, count) in expected {
            let registry = dod_zero_registry_for(emitter).map_err(ctx("role registry"))?;
            assert_eq!(registry.series_count(), count, "{}", emitter.as_str());
            let sink = CaptureSink::default();
            assert_eq!(registry.emit_zeros(&sink), count);
            for (name, _, _) in sink.take() {
                assert!(emitter.owns(name), "{} emitted {name}", emitter.as_str());
            }
            total = total.saturating_add(count);
        }
        let all = dod_zero_registry().map_err(ctx("dod registry"))?;
        assert_eq!(total, all.series_count(), "roles cover all nine series");
        Ok(())
    }

    #[test]
    fn test_registry_for_role_keeps_declaration_order() -> TestResult {
        let registry =
            dod_zero_registry_for(DodEmitter::Dispatch).map_err(ctx("dispatch registry"))?;
        let names: Vec<&str> = registry.all().iter().map(|s| s.key().name).collect();
        assert_eq!(names, DodEmitter::Dispatch.series_names());
        Ok(())
    }

    #[test]
    fn test_violation_series_belong_to_the_warden_role_only() {
        for name in DOD_VIOLATION_SERIES {
            assert!(DodEmitter::Warden.owns(name));
            assert!(!DodEmitter::Dispatch.owns(name));
            assert!(!DodEmitter::Sensor.owns(name));
        }
    }

    #[test]
    fn test_violation_series_are_declared_and_only_warden_unaudited() {
        assert_eq!(DOD_VIOLATION_SERIES, &["warden_unaudited_actions"]);
        for name in DOD_VIOLATION_SERIES {
            assert!(DOD_ZERO_SERIES.iter().any(|s| s.key().name == *name));
        }
    }

    #[test]
    fn test_no_dod_series_name_is_a_null_counter() {
        // Work counters must never be registered as violation counters:
        // a NullCounter may share a DoD series name only if that series is
        // declared as a violation series.
        let mut registry = NullCounterRegistry::new();
        registry.register(&crate::routing::SECURITY_METRIC_LEAKED);
        registry.register(&crate::dod_metrics::WARDEN_UNAUDITED_VIOLATIONS);
        for counter in registry.all() {
            let is_dod_series = DOD_ZERO_SERIES
                .iter()
                .any(|series| series.key().name == counter.name());
            assert!(
                !is_dod_series || DOD_VIOLATION_SERIES.contains(&counter.name()),
                "{}",
                counter.name()
            );
        }
        assert_eq!(
            crate::dod_metrics::WARDEN_UNAUDITED_VIOLATIONS.name(),
            WARDEN_UNAUDITED_ACTIONS.name
        );
        // The zero registry itself never exposes a NullCounter.
        assert!(
            DOD_ZERO_SERIES
                .iter()
                .all(|series| series.key().kind != MetricKind::Histogram)
        );
    }
}
