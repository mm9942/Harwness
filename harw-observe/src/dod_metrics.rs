//! Produzenten-Handles der DoD-/Dispatch-Meta-Metriken (PL-100 Phase 4).
//!
//! # Verantwortungsbereich
//! Trägt [`DodMetrics`] mit seinen drei Rollen-Ausprägungen
//! [`DispatchMetrics`], [`SensorMetrics`] und [`WardenMetrics`], die
//! Rollenmarker ([`DodRole`]), [`BreakerPartitionKind`], den
//! Verstoß-Nullzähler [`WARDEN_UNAUDITED_VIOLATIONS`] und
//! [`dod_null_counters`]. Ein Handle ist **der eine** Einstiegspunkt je
//! Prozess, über den die Produzenten seiner [`DodEmitter`]-Rolle die
//! Metriken aus [`crate::zero_registry`] melden: Dispatch-Seite
//! (Verdict-Bindung, Override-Pfad und die Ereignisse, die `harw-dod-watch`
//! über seine Gateway-Verbindung liefert), Sentinel und Warden. Jede Methode
//! geht über [`RoutingSink::record_meta`] und erreicht damit **jedes**
//! Standardziel (File-Sink, Prometheus, OTLP), nie ein Exportziel des
//! [`crate::SecurityExportGate`]. Ein gewöhnliches
//! [`crate::TelemetrySink::record`] würde nur ein Ziel bedienen; die übrigen
//! blieben für immer auf dem Nullwert stehen (siehe
//! [`crate::routing`]-Moduldoku, Abschnitt „Meta-Metriken").
//!
//! # Rollen sind Typen
//! Jede Serie gehört genau einer Rolle (siehe
//! [`crate::zero_registry`], Abschnitt „Je Prozess nur die eigenen Serien").
//! Ein Handle trägt seine Rolle als Typparameter und bietet **nur** die
//! Meldemethoden der Serien dieser Rolle an: [`DispatchMetrics`] kennt
//! `verdict_recorded`, `human_override`, `unbound_verdict`, `breaker_trip`
//! und `set_untriaged_backlog`; [`SensorMetrics`] nur
//! `sensor_parse_error[s]`; [`WardenMetrics`] nur `warden_unaudited_action`.
//! Ein fehlgeleiteter Produzentenaufruf — etwa eine Warden-Meldung über das
//! Dispatch-Handle des Gateways — ist damit ein Compile-Fehler, kein
//! Laufzeitfall: es kann weder eine fremde Serie ohne vorherigen Nullwert
//! entstehen noch der prozessweite [`WARDEN_UNAUDITED_VIOLATIONS`] in einem
//! Prozess steigen, der ihn nie registriert. [`DodRole`] ist versiegelt;
//! andere Crates können keine vierte Rolle erfinden.
//!
//! ```compile_fail,E0599
//! use std::sync::Arc;
//! use harw_observe::{DispatchMetrics, NullSink, ObserveError, RoutingSink};
//!
//! # fn main() -> Result<(), ObserveError> {
//! let (_sink, dod) = DispatchMetrics::install(RoutingSink::new(Arc::new(NullSink)))?;
//! dod.warden_unaudited_action(); // nur WardenMetrics kennt diese Methode
//! # Ok(())
//! # }
//! ```
//!
//! # Warum absolute Stände
//! Die Sinks halten je Serie den zuletzt gemeldeten Wert: der
//! Prometheus-Sink überschreibt, OTLP meldet Zähler als kumulative Summe, die
//! JSONL-Datei protokolliert jeden Stand. Ein Zähler wird deshalb immer mit
//! seinem **neuen Gesamtstand** gemeldet (`Count(total)`), nie mit einem
//! Inkrement — dasselbe Muster wie [`crate::NullCounter::violated`]. Ein
//! `Count(1)` je Ereignis ließe Prometheus dauerhaft `1` zeigen. Weil jede
//! Meldung den vollen Stand trägt, ist auch eine wiederholte Meldung
//! desselben Stands harmlos (siehe [`DodMetrics::republish_to`]).
//!
//! # Installation: [`DodMetrics::install`]
//! Die Kompositionsstelle des Prozesses übergibt den fertig gerouteten
//! [`RoutingSink`] **als Wert**; die Rolle steckt im Typ
//! (`DispatchMetrics::install(routing)`). Weil der Sink danach nur noch
//! geteilt existiert, kann kein `route()` und keine Exportfreigabe mehr
//! folgen: die Null-Serien der Rolle erreichen jedes Ziel, das je echte
//! Werte bekommt. `install` emittiert genau diese Serien, prüft ihre Anzahl
//! und liefert Sink und Handle zurück. Es gibt keinen zweiten öffentlichen
//! Konstruktor; zwei Handles im selben Prozess führten getrennte Stände und
//! überschrieben sich gegenseitig in den Sinks.
//!
//! `install` **flusht nicht**. Die Kompositionsstelle flusht danach nur
//! ihre lokalen Ziele (File-Sink; Prometheus ist rein speicherresident).
//! Ein Netzwerkziel wie OTLP puffert die Nullwerte und stellt sie mit
//! seinem ersten regulären Flush zu — ein Flush beim Start hinge sonst an
//! der Erreichbarkeit des Collectors.
//!
//! # Verlustbehaftete Ziele: [`DodMetrics::republish_to`]
//! `OtlpSink` leert seinen Puffer vor dem Senden und zählt einen
//! gescheiterten Stapel nur als Sendefehler. Ohne Nachlieferung gingen so
//! die Nullwerte (Collector beim Start noch nicht erreichbar) oder ein
//! späterer Stand bis zum nächsten Ereignis verloren — im OTLP-Ziel wäre
//! „null" wieder nicht von „nie verdrahtet" zu unterscheiden.
//! [`DodMetrics::republish_to`] meldet deshalb die aktuellen Stände aller
//! Serien der Rolle erneut, aber **nur** an das übergebene Ziel. Die
//! Kompositionsstelle ruft es vor einem Flush auf, wenn der vorige Flush
//! dieses Ziels Sendefehler hatte (im Gateway:
//! `harw-cli/src/observe.rs`, `FlushTicker::tick`). File-Sink und Prometheus
//! verlieren nichts und bekommen keine Wiederholung.
//!
//! # Massenereignisse
//! Jede Meldung ist ein synchroner Fan-out (eine JSONL-Zeile, ein
//! OTLP-Pufferplatz). Produzenten, die ein Angreifer fluten kann (etwa
//! verworfene IPC-Datagramme im Sentinel, Bedrohung B2/B10), zählen lokal
//! und melden den Block einmal je Takt über
//! [`DodMetrics::sensor_parse_errors`]. Weil absolute Stände gemeldet
//! werden, verliert das Zusammenfassen keine Information.
//!
//! # Der Verstoß-Nullzähler des Wardens
//! `warden_unaudited_actions` zählt Verstöße gegen die Invariante S2 (siehe
//! [`crate::zero_registry::DOD_VIOLATION_SERIES`]). Zusätzlich zur Meldung an
//! alle Ziele erhöht [`DodMetrics::warden_unaudited_action`] deshalb den
//! prozessweiten [`WARDEN_UNAUDITED_VIOLATIONS`]. Der Prozess der Rolle
//! [`DodEmitter::Warden`] nimmt diesen Zähler über [`dod_null_counters`] in
//! seine [`crate::NullCounterRegistry`] auf, damit [`crate::assert_all_zero`]
//! anschlägt. Ein Prozess, der nie Warden-Aktionen ausführt (z. B. das
//! Gateway), registriert ihn **nicht**: ein nie erreichbarer Nullzähler sähe
//! wie eine eingehaltene Invariante aus (siehe [`crate::null_counter`]-
//! Moduldoku, „Die Falle"). Weil nur [`WardenMetrics`] die Methode hat, kann
//! ein solcher Prozess den Zähler auch nicht versehentlich erhöhen.
//!
//! # Nebenläufigkeit
//! Jedes Handle ist `Send + Sync + Debug` und über `Arc` teilbar. Jede
//! Meldung hält eine interne Sperre über Erhöhen **und** Zustellen: so sieht
//! jedes Ziel dieselbe, monoton steigende Folge. Ohne Sperre könnte ein
//! älterer Stand einen neueren überholen, und Prometheus zeigte einen
//! Zählerrücksprung. [`DodMetrics::republish_to`] meldet unter derselben
//! Sperre. Die Meta-Metriken sind selten (je Verdict, Trip, Override,
//! Backlog-Änderung; Parse-Fehler gebündelt, siehe oben); die Sperre liegt
//! nicht auf einem heißen Pfad. Sinks rufen nie in dieses Modul zurück, eine
//! Verklemmung ist deshalb ausgeschlossen. Eine vergiftete Sperre wird
//! übernommen (Muster `harw-observe-file::FileSink::lock_state`). Die
//! Methoden schreiben synchron in den File-Sink: aus async-Code über
//! `tokio::task::spawn_blocking` aufrufen.
//!
//! # Fehler
//! Nur [`DodMetrics::install`] kann scheitern
//! ([`ObserveError::InvalidZeroSeries`],
//! [`ObserveError::IncompleteZeroSeries`]). Die Meldemethoden scheitern nie:
//! Telemetrie lässt die beobachtete Operation nie scheitern (Vertrag A.3).
//! Stände sättigen bei `u64::MAX`, der Backlog bei `u32::MAX`.
//!
//! # Examples
//! ```
//! use std::sync::Arc;
//! use harw_observe::{
//!     BreakerPartitionKind, DispatchMetrics, NullSink, ObserveError, RoutingSink,
//!     TelemetrySink,
//! };
//!
//! # fn main() -> Result<(), ObserveError> {
//! // Kompositionsstelle: erst alle Routen, dann installieren.
//! let file: Arc<dyn TelemetrySink> = Arc::new(NullSink);
//! let mut routing = RoutingSink::new(Arc::clone(&file));
//! routing.route("app.", Arc::new(NullSink));
//! let (sink, dod) = DispatchMetrics::install(routing)?;
//! file.flush(); // nur lokale Ziele beim Start flushen
//!
//! // Produzenten:
//! dod.verdict_recorded();
//! dod.breaker_trip(BreakerPartitionKind::Cgroup);
//! dod.set_untriaged_backlog(3);
//! sink.flush(); // regulärer (periodischer) Flush der Kompositionsstelle
//! # Ok(())
//! # }
//! ```

use std::marker::PhantomData;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::error::ObserveError;
use crate::field::{FieldName, FieldValue};
use crate::metric::{MetricKey, MetricValue};
use crate::null_counter::NullCounter;
use crate::routing::RoutingSink;
use crate::sink::{NullSink, TelemetrySink};
use crate::zero_registry::{
    BREAKER_TRIPS_TOTAL, DOD_TRIAGE_VERDICTS_TOTAL, DodEmitter, HUMAN_OVERRIDES_TOTAL,
    PARTITION_LABEL, SENSOR_PARSE_ERRORS_TOTAL, UNBOUND_VERDICT_TOTAL, UNTRIAGED_BACKLOG,
    WARDEN_UNAUDITED_ACTIONS, dod_zero_registry_for,
};

/// Verstoß-Nullzähler für `warden_unaudited_actions` (Invariante S2).
///
/// # Description
/// Erwarteter Stand im Betrieb: null. Erhöht sich ausschließlich über
/// [`DodMetrics::warden_unaudited_action`] (nur auf [`WardenMetrics`]), das
/// zugleich den neuen Stand an alle Ziele meldet. Nie direkt
/// `violated(&routing, …)` aufrufen: über einen [`RoutingSink`] erreichte das
/// nur **ein** Ziel. Der Prozess der Rolle [`DodEmitter::Warden`] registriert
/// diesen Zähler in seiner [`crate::NullCounterRegistry`]; Prozesse ohne
/// Warden-Aktionen tun das nicht (siehe Moduldoku).
///
/// # Concurrency
/// `static` und `Sync` (siehe [`NullCounter`]); prozessweit geteilt.
///
/// # Examples
/// ```
/// use harw_observe::WARDEN_UNAUDITED_VIOLATIONS;
///
/// assert_eq!(WARDEN_UNAUDITED_VIOLATIONS.name(), "warden_unaudited_actions");
/// ```
pub static WARDEN_UNAUDITED_VIOLATIONS: NullCounter = NullCounter::new(
    &WARDEN_UNAUDITED_ACTIONS,
    "jede Warden-Aktion hat vor ihrer Wirkung einen Audit-Eintrag (Invariante S2)",
);

// Die Verstoß-Nullzähler der Warden-Rolle (siehe `dod_null_counters`).
static WARDEN_NULL_COUNTERS: [&NullCounter; 1] = [&WARDEN_UNAUDITED_VIOLATIONS];

/// Die Verstoß-Nullzähler, die der Prozess einer Rolle in seine
/// [`crate::NullCounterRegistry`] aufnimmt.
///
/// # Description
/// Nur [`DodEmitter::Warden`] führt einen
/// ([`WARDEN_UNAUDITED_VIOLATIONS`]); Dispatch- und Sensor-Rolle haben keine
/// Verstoß-Serie. So registriert kein Prozess einen Nullzähler, den er nie
/// erreichen kann (siehe Moduldoku, „Der Verstoß-Nullzähler des Wardens").
///
/// # Arguments
/// - `emitter` (`DodEmitter`): die Rolle des Prozesses.
///
/// # Returns
/// Die (möglicherweise leere) Liste der Zähler.
///
/// # Examples
/// ```
/// use harw_observe::{DodEmitter, NullCounterRegistry, dod_null_counters};
///
/// let mut registry = NullCounterRegistry::new();
/// for counter in dod_null_counters(DodEmitter::Warden) {
///     registry.register(counter);
/// }
/// assert_eq!(registry.all().len(), 1);
/// assert!(dod_null_counters(DodEmitter::Dispatch).is_empty());
/// ```
#[must_use]
pub fn dod_null_counters(emitter: DodEmitter) -> &'static [&'static NullCounter] {
    match emitter {
        DodEmitter::Warden => &WARDEN_NULL_COUNTERS,
        DodEmitter::Dispatch | DodEmitter::Sensor => &[],
    }
}

/// Die geschlossene Menge der Partitions**arten** von
/// `breaker_trips_total`.
///
/// # Description
/// Spiegelt [`crate::zero_registry::BREAKER_PARTITIONS`] (README §2 I10:
/// uid, Cgroup, Sensor). Konkrete uids, Cgroup-IDs oder Sensornamen sind nie
/// Label-Werte: ein geschlossenes Enum hält die Kardinalität bei drei. Nicht
/// zu verwechseln mit dem `BreakerPartition` aus `harw-dod-dispatch` (eine
/// Breaker-**Instanz** mit Hysterese, README §2.1): dieser Typ ist nur die
/// Label-Art, unter der eine solche Instanz ihren Trip meldet.
///
/// # Examples
/// ```
/// use harw_observe::BreakerPartitionKind;
///
/// assert_eq!(BreakerPartitionKind::Cgroup.as_str(), "cgroup");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BreakerPartitionKind {
    /// Partition nach Benutzer-ID.
    Uid,
    /// Partition nach Cgroup.
    Cgroup,
    /// Partition nach Sensor.
    Sensor,
}

impl BreakerPartitionKind {
    /// Alle Partitionsarten in der Reihenfolge von
    /// [`crate::zero_registry::BREAKER_PARTITIONS`].
    pub const ALL: [Self; 3] = [Self::Uid, Self::Cgroup, Self::Sensor];

    /// Der Label-Wert dieser Partitionsart.
    ///
    /// # Returns
    /// `"uid"`, `"cgroup"` oder `"sensor"`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Uid => "uid",
            Self::Cgroup => "cgroup",
            Self::Sensor => "sensor",
        }
    }
}

// Versiegeltes Supertrait: nur die drei Marker dieses Moduls implementieren
// `DodRole`. Eine fremde Crate kann so keine vierte Rolle mit eigenem
// Seriensatz erfinden (Muster: `harw-dod-cap/src/handle.rs`).
mod sealed {
    pub trait Sealed {}
    impl Sealed for super::DispatchRole {}
    impl Sealed for super::SensorRole {}
    impl Sealed for super::WardenRole {}
}

/// Die Rolle eines [`DodMetrics`]-Handles als Typ (versiegelt).
///
/// # Description
/// Bindet einen Marker ([`DispatchRole`], [`SensorRole`], [`WardenRole`]) an
/// seine [`DodEmitter`]-Rolle. Bestimmt, welche Null-Serien
/// [`DodMetrics::install`] emittiert, welche Serien
/// [`DodMetrics::republish_to`] nachliefert und welche Meldemethoden das
/// Handle überhaupt hat (siehe Moduldoku, „Rollen sind Typen").
///
/// # Examples
/// ```
/// use harw_observe::{DodEmitter, DodRole, WardenRole};
///
/// assert_eq!(WardenRole::EMITTER, DodEmitter::Warden);
/// ```
pub trait DodRole: sealed::Sealed + std::fmt::Debug + Send + Sync + 'static {
    /// Die Prozessrolle dieses Markers.
    const EMITTER: DodEmitter;
}

/// Marker der Rolle [`DodEmitter::Dispatch`] (Gateway, siehe
/// [`DispatchMetrics`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DispatchRole;

/// Marker der Rolle [`DodEmitter::Sensor`] (`harw-sentinel`, siehe
/// [`SensorMetrics`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SensorRole;

/// Marker der Rolle [`DodEmitter::Warden`] (`harw-warden`, siehe
/// [`WardenMetrics`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WardenRole;

impl DodRole for DispatchRole {
    const EMITTER: DodEmitter = DodEmitter::Dispatch;
}

impl DodRole for SensorRole {
    const EMITTER: DodEmitter = DodEmitter::Sensor;
}

impl DodRole for WardenRole {
    const EMITTER: DodEmitter = DodEmitter::Warden;
}

/// Das Handle der Dispatch-Seite: `dod_triage_verdicts_total`,
/// `human_overrides_total`, `unbound_verdict_total`,
/// `breaker_trips_total{partition}` und `untriaged_backlog`.
pub type DispatchMetrics = DodMetrics<DispatchRole>;

/// Das Handle der Sensor-Seite: `sensor_parse_errors_total`.
pub type SensorMetrics = DodMetrics<SensorRole>;

/// Das Handle des Wardens: `warden_unaudited_actions` samt
/// [`WARDEN_UNAUDITED_VIOLATIONS`].
pub type WardenMetrics = DodMetrics<WardenRole>;

// Ein Zähler; bestimmt Schlüssel, Label und Speicherplatz.
#[derive(Debug, Clone, Copy)]
enum Counter {
    Verdicts,
    HumanOverrides,
    UnboundVerdicts,
    Breaker(BreakerPartitionKind),
    SensorParseErrors,
    WardenUnauditedActions,
}

impl Counter {
    // Der Metrikschlüssel, unter dem der Stand gemeldet wird.
    fn key(self) -> &'static MetricKey {
        match self {
            Self::Verdicts => &DOD_TRIAGE_VERDICTS_TOTAL,
            Self::HumanOverrides => &HUMAN_OVERRIDES_TOTAL,
            Self::UnboundVerdicts => &UNBOUND_VERDICT_TOTAL,
            Self::Breaker(_) => &BREAKER_TRIPS_TOTAL,
            Self::SensorParseErrors => &SENSOR_PARSE_ERRORS_TOTAL,
            Self::WardenUnauditedActions => &WARDEN_UNAUDITED_ACTIONS,
        }
    }
}

// Eine gemeldete Serie: ein Zähler oder die Backlog-Gauge.
#[derive(Debug, Clone, Copy)]
enum Series {
    Counter(Counter),
    Backlog,
}

impl Series {
    // Der Metrikschlüssel der Serie.
    fn key(self) -> &'static MetricKey {
        match self {
            Self::Counter(counter) => counter.key(),
            Self::Backlog => &UNTRIAGED_BACKLOG,
        }
    }

    // Das eine Label der Serie; nur `breaker_trips_total` trägt `partition`.
    fn label(self) -> Option<(FieldName, FieldValue)> {
        match self {
            Self::Counter(Counter::Breaker(kind)) => {
                Some((PARTITION_LABEL, FieldValue::Str(kind.as_str())))
            }
            Self::Counter(_) | Self::Backlog => None,
        }
    }
}

// Die Serien der Dispatch-Rolle in der Reihenfolge von `DOD_ZERO_SERIES`
// (durch `test_republish_matches_the_role_zero_registry` abgesichert).
const DISPATCH_SERIES: &[Series] = &[
    Series::Counter(Counter::Verdicts),
    Series::Counter(Counter::HumanOverrides),
    Series::Counter(Counter::UnboundVerdicts),
    Series::Counter(Counter::Breaker(BreakerPartitionKind::Uid)),
    Series::Counter(Counter::Breaker(BreakerPartitionKind::Cgroup)),
    Series::Counter(Counter::Breaker(BreakerPartitionKind::Sensor)),
    Series::Backlog,
];

// Die Serie der Sensor-Rolle.
const SENSOR_SERIES: &[Series] = &[Series::Counter(Counter::SensorParseErrors)];

// Die Serie der Warden-Rolle.
const WARDEN_SERIES: &[Series] = &[Series::Counter(Counter::WardenUnauditedActions)];

// Alle Serien (mit Label-Kombination), die ein Handle der Rolle meldet.
fn role_series(emitter: DodEmitter) -> &'static [Series] {
    match emitter {
        DodEmitter::Dispatch => DISPATCH_SERIES,
        DodEmitter::Sensor => SENSOR_SERIES,
        DodEmitter::Warden => WARDEN_SERIES,
    }
}

// Die laufenden Gesamtstände eines Handles.
#[derive(Debug, Default)]
struct Totals {
    verdicts: u64,
    human_overrides: u64,
    unbound_verdicts: u64,
    breaker_uid: u64,
    breaker_cgroup: u64,
    breaker_sensor: u64,
    sensor_parse_errors: u64,
    warden_unaudited_actions: u64,
    backlog: u32,
}

impl Totals {
    // Der Speicherplatz des Stands von `counter`.
    fn slot(&mut self, counter: Counter) -> &mut u64 {
        match counter {
            Counter::Verdicts => &mut self.verdicts,
            Counter::HumanOverrides => &mut self.human_overrides,
            Counter::UnboundVerdicts => &mut self.unbound_verdicts,
            Counter::Breaker(BreakerPartitionKind::Uid) => &mut self.breaker_uid,
            Counter::Breaker(BreakerPartitionKind::Cgroup) => &mut self.breaker_cgroup,
            Counter::Breaker(BreakerPartitionKind::Sensor) => &mut self.breaker_sensor,
            Counter::SensorParseErrors => &mut self.sensor_parse_errors,
            Counter::WardenUnauditedActions => &mut self.warden_unaudited_actions,
        }
    }

    // Der aktuelle Stand von `counter`.
    fn count(&self, counter: Counter) -> u64 {
        match counter {
            Counter::Verdicts => self.verdicts,
            Counter::HumanOverrides => self.human_overrides,
            Counter::UnboundVerdicts => self.unbound_verdicts,
            Counter::Breaker(BreakerPartitionKind::Uid) => self.breaker_uid,
            Counter::Breaker(BreakerPartitionKind::Cgroup) => self.breaker_cgroup,
            Counter::Breaker(BreakerPartitionKind::Sensor) => self.breaker_sensor,
            Counter::SensorParseErrors => self.sensor_parse_errors,
            Counter::WardenUnauditedActions => self.warden_unaudited_actions,
        }
    }

    // Der zu meldende Wert einer Serie.
    fn value(&self, series: Series) -> MetricValue {
        match series {
            Series::Counter(counter) => MetricValue::Count(self.count(counter)),
            Series::Backlog => MetricValue::Gauge(f64::from(self.backlog)),
        }
    }
}

/// Produzenten-Handle für die DoD-/Dispatch-Meta-Metriken einer Rolle `R`.
///
/// # Description
/// Hält den fertig aufgebauten [`RoutingSink`] des Prozesses und je Serie
/// den laufenden Stand. Jede Meldemethode erhöht bzw. setzt den Stand und
/// meldet ihn über [`RoutingSink::record_meta`] an alle Standardziele
/// (siehe Moduldoku). Genau ein Handle je Prozess, gebaut über
/// [`DodMetrics::install`]. Die Meldemethoden existieren nur für die Rolle,
/// der ihre Serie gehört; in der Regel benutzt man die Aliasse
/// [`DispatchMetrics`], [`SensorMetrics`] und [`WardenMetrics`].
///
/// # Concurrency
/// `Send + Sync`; siehe Moduldoku, Abschnitt „Nebenläufigkeit".
#[derive(Debug)]
pub struct DodMetrics<R: DodRole> {
    routing: Arc<RoutingSink>,
    totals: Mutex<Totals>,
    // `fn() -> R`: das Handle besitzt kein `R`; Send/Sync hängen nicht am
    // Marker.
    _role: PhantomData<fn() -> R>,
}

impl<R: DodRole> DodMetrics<R> {
    /// Installiert das eine Handle der Rolle `R` über dem fertig gerouteten
    /// Sink des Prozesses und emittiert die Null-Serien dieser Rolle.
    ///
    /// # Description
    /// Der einzige öffentliche Konstruktor (siehe Moduldoku, Abschnitt
    /// „Installation"). Nimmt `routing` als Wert, damit danach weder
    /// `route()` noch eine Exportfreigabe folgen kann. Schritte:
    /// [`dod_zero_registry_for`] für `R::EMITTER`, Verteilen über
    /// [`RoutingSink::emit_zero_registry`] an jedes Standardziel (nie an ein
    /// Exportziel), Prüfen der Anzahl, Verpacken in `Arc`. Flusht **nicht**:
    /// die Kompositionsstelle flusht danach nur ihre lokalen Ziele, ein
    /// Netzwerkziel bekommt die Nullwerte mit seinem ersten regulären Flush.
    ///
    /// # Arguments
    /// - `routing` (`RoutingSink`): fertig aufgebaut — alle Routen und
    ///   gegebenenfalls Exportfreigaben hängen bereits.
    ///
    /// # Returns
    /// Den nun geteilten Sink (für gewöhnliche Messwerte und `flush`) und
    /// das Handle, dessen Stände bei null beginnen — passend zu den eben
    /// emittierten Nullwerten.
    ///
    /// # Errors
    /// [`ObserveError::InvalidZeroSeries`], falls eine statische Deklaration
    /// ungültig ist (durch Tests ausgeschlossen), und
    /// [`ObserveError::IncompleteZeroSeries`], wenn nicht jede Null-Serie
    /// der Rolle verteilt wurde.
    ///
    /// # Concurrency
    /// Einmal je Prozess in der Aufbauphase aufrufen; danach ist das Handle
    /// `Send + Sync`.
    ///
    /// # Examples
    /// ```
    /// use std::sync::Arc;
    /// use harw_observe::{NullSink, ObserveError, RoutingSink, SensorMetrics};
    ///
    /// # fn main() -> Result<(), ObserveError> {
    /// let routing = RoutingSink::new(Arc::new(NullSink));
    /// let (_sink, dod) = SensorMetrics::install(routing)?;
    /// dod.sensor_parse_errors(3);
    /// # Ok(())
    /// # }
    /// ```
    pub fn install(routing: RoutingSink) -> Result<(Arc<RoutingSink>, Self), ObserveError> {
        let registry = dod_zero_registry_for(R::EMITTER)?;
        let expected = registry.series_count();
        let emitted = routing.emit_zero_registry(&registry);
        if emitted != expected {
            return Err(ObserveError::IncompleteZeroSeries { emitted, expected });
        }
        let routing = Arc::new(routing);
        Ok((Arc::clone(&routing), Self::new(routing)))
    }

    // Baut das Handle ohne Null-Emission; nur `install` und Tests.
    fn new(routing: Arc<RoutingSink>) -> Self {
        Self {
            routing,
            totals: Mutex::new(Totals::default()),
            _role: PhantomData,
        }
    }

    /// Die Prozessrolle dieses Handles.
    ///
    /// # Returns
    /// `R::EMITTER` (für Logs der Kompositionsstelle).
    #[must_use]
    pub fn emitter(&self) -> DodEmitter {
        R::EMITTER
    }

    /// Meldet die aktuellen Stände **aller** Serien dieser Rolle erneut,
    /// aber nur an `target`.
    ///
    /// # Description
    /// Nachlieferung für ein verlustbehaftetes Ziel (siehe Moduldoku,
    /// „Verlustbehaftete Ziele"): nach einem gescheiterten OTLP-Versand ruft
    /// die Kompositionsstelle dies mit ihrem `OtlpSink` auf und flusht
    /// danach. Gemeldet wird jede Serie samt Label-Kombination in der
    /// Reihenfolge der Null-Registry der Rolle, Zähler mit ihrem absoluten
    /// Stand, `untriaged_backlog` mit dem zuletzt gesetzten Wert. Weil alle
    /// Werte absolut sind, ist die Wiederholung idempotent. Die übrigen
    /// Ziele bekommen nichts. `target` ist das konkrete Ziel, **nicht** der
    /// `RoutingSink`: dessen `record` erreichte nur den File-Sink.
    ///
    /// # Arguments
    /// - `target` (`&dyn TelemetrySink`): das Ziel, das Stände verloren
    ///   haben kann.
    ///
    /// # Returns
    /// Die Anzahl der gemeldeten Serien (Dispatch: 7, Sensor und Warden: 1).
    ///
    /// # Concurrency
    /// Meldet unter der Sperre des Handles: ein gleichzeitiges Ereignis
    /// erreicht `target` entweder vor oder nach der Nachlieferung, nie mit
    /// einem älteren Stand danach.
    ///
    /// # Examples
    /// ```
    /// use std::sync::Arc;
    /// use harw_observe::{DispatchMetrics, NullSink, ObserveError, RoutingSink};
    ///
    /// # fn main() -> Result<(), ObserveError> {
    /// let (_sink, dod) = DispatchMetrics::install(RoutingSink::new(Arc::new(NullSink)))?;
    /// dod.verdict_recorded();
    /// assert_eq!(dod.republish_to(&NullSink), 7);
    /// # Ok(())
    /// # }
    /// ```
    pub fn republish_to(&self, target: &dyn TelemetrySink) -> usize {
        let totals = self.lock();
        let series = role_series(R::EMITTER);
        for item in series {
            let label = item.label();
            target.record(item.key(), totals.value(*item), label.as_slice());
        }
        series.len()
    }

    // Erhöht den Stand von `counter` um eins.
    fn bump(&self, counter: Counter) {
        self.bump_by(counter, 1);
    }

    // Erhöht den Stand von `counter` um `count` und meldet ihn unter der
    // Sperre an alle Standardziele; `0` meldet nichts.
    fn bump_by(&self, counter: Counter, count: u64) {
        if count == 0 {
            return;
        }
        let mut totals = self.lock();
        let slot = totals.slot(counter);
        *slot = slot.saturating_add(count);
        self.report(Series::Counter(counter), &totals);
    }

    // Meldet den Stand einer Serie an alle Standardziele. Unter der Sperre
    // aufrufen, damit jedes Ziel die Stände in aufsteigender Folge sieht.
    // `record_meta` liefert nur bei geschützten Namen `false`; die
    // DoD-Namen sind nach `ZeroSeries::validate` nie geschützt.
    fn report(&self, series: Series, totals: &Totals) {
        let label = series.label();
        self.routing
            .record_meta(series.key(), totals.value(series), label.as_slice());
    }

    // Nimmt die Sperre; eine vergiftete Sperre wird übernommen.
    fn lock(&self) -> MutexGuard<'_, Totals> {
        self.totals.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl DodMetrics<DispatchRole> {
    /// Ein gebundenes und gespeichertes Triage-Verdict
    /// (`dod_triage_verdicts_total`).
    ///
    /// # Description
    /// Aufrufort: `accept_security_verdict()` nach erfolgreichem `binds()`
    /// und Persistieren im `VerdictStore`.
    ///
    /// # Concurrency
    /// Siehe Moduldoku; aus beliebigen Threads aufrufbar.
    pub fn verdict_recorded(&self) {
        self.bump(Counter::Verdicts);
    }

    /// Ein Mensch hat ein Modell-Verdict übersteuert
    /// (`human_overrides_total`).
    ///
    /// # Concurrency
    /// Siehe Moduldoku; aus beliebigen Threads aufrufbar.
    pub fn human_override(&self) {
        self.bump(Counter::HumanOverrides);
    }

    /// Ein Verdict ließ sich keinem Befund (id + `record_digest`) zuordnen
    /// (`unbound_verdict_total`).
    ///
    /// # Concurrency
    /// Siehe Moduldoku; aus beliebigen Threads aufrufbar.
    pub fn unbound_verdict(&self) {
        self.bump(Counter::UnboundVerdicts);
    }

    /// Ein Circuit-Breaker bzw. Budget hat ausgelöst
    /// (`breaker_trips_total{partition}`).
    ///
    /// # Arguments
    /// - `kind` (`BreakerPartitionKind`): die Partitionsart, nie eine
    ///   konkrete Kennung.
    ///
    /// # Concurrency
    /// Siehe Moduldoku; aus beliebigen Threads aufrufbar.
    pub fn breaker_trip(&self, kind: BreakerPartitionKind) {
        self.bump(Counter::Breaker(kind));
    }

    /// Setzt die Zahl der Befunde, die noch auf eine Triage warten
    /// (`untriaged_backlog`, Gauge).
    ///
    /// # Arguments
    /// - `backlog` (`usize`): aktuelle Länge der Warteschlange; Werte über
    ///   `u32::MAX` werden auf `u32::MAX` gesättigt.
    ///
    /// # Concurrency
    /// Siehe Moduldoku; die Sperre sorgt dafür, dass alle Ziele denselben
    /// zuletzt gesetzten Wert halten.
    pub fn set_untriaged_backlog(&self, backlog: usize) {
        let mut totals = self.lock();
        totals.backlog = u32::try_from(backlog).unwrap_or(u32::MAX);
        self.report(Series::Backlog, &totals);
    }
}

impl DodMetrics<SensorRole> {
    /// Rohdaten eines Sensors wurden vom Parser verworfen
    /// (`sensor_parse_errors_total`).
    ///
    /// # Description
    /// Für Einzelereignisse; flutbare Quellen bündeln über
    /// [`DodMetrics::sensor_parse_errors`].
    ///
    /// # Concurrency
    /// Siehe Moduldoku; aus beliebigen Threads aufrufbar.
    pub fn sensor_parse_error(&self) {
        self.sensor_parse_errors(1);
    }

    /// Meldet `count` verworfene Sensor-Rohdaten mit **einer** Zustellung
    /// (`sensor_parse_errors_total`).
    ///
    /// # Description
    /// Der Weg für flutbare Quellen (siehe Moduldoku, Abschnitt
    /// „Massenereignisse"): der Produzent zählt lokal und meldet den Block
    /// einmal je Takt. `count == 0` meldet nichts.
    ///
    /// # Arguments
    /// - `count` (`u64`): Anzahl der seit der letzten Meldung verworfenen
    ///   Rohdaten; der Gesamtstand sättigt bei `u64::MAX`.
    ///
    /// # Concurrency
    /// Siehe Moduldoku; aus beliebigen Threads aufrufbar.
    pub fn sensor_parse_errors(&self, count: u64) {
        self.bump_by(Counter::SensorParseErrors, count);
    }
}

impl DodMetrics<WardenRole> {
    /// Eine Warden-Aktion hat ohne vorherigen Audit-Eintrag gewirkt
    /// (`warden_unaudited_actions`, Verstoß gegen S2).
    ///
    /// # Description
    /// Erhöht zusätzlich den prozessweiten [`WARDEN_UNAUDITED_VIOLATIONS`],
    /// damit die [`crate::NullCounterRegistry`] des Warden-Prozesses den
    /// Verstoß sieht. Die gemeldete Serie trägt den Stand dieses Handles.
    ///
    /// # Concurrency
    /// Siehe Moduldoku; aus beliebigen Threads aufrufbar.
    pub fn warden_unaudited_action(&self) {
        // `&NullSink`: die Zustellung an alle Ziele übernimmt `bump` über
        // `record_meta`; `violated` auf dem `RoutingSink` erreichte nur ein
        // Ziel.
        WARDEN_UNAUDITED_VIOLATIONS.violated(&NullSink, &[]);
        self.bump(Counter::WardenUnauditedActions);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;

    use super::*;
    use crate::null_counter::{NullCounterRegistry, assert_all_zero};
    use crate::routing::{ExportApproval, SecurityExportGate};
    use crate::test_support::{TestError, TestResult, ctx};
    use crate::zero_registry::BREAKER_PARTITIONS;

    type Seen = (&'static str, MetricValue, Vec<(FieldName, FieldValue)>);

    // Aufzeichnender Test-Sink: hält Name, Wert und Labels jedes Aufrufs und
    // zählt `flush()`-Aufrufe.
    #[derive(Debug, Default)]
    struct CaptureSink {
        seen: Mutex<Vec<Seen>>,
        flushes: AtomicUsize,
    }

    impl CaptureSink {
        fn all(&self) -> Vec<Seen> {
            self.seen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        }

        fn values_of(&self, name: &str) -> Vec<MetricValue> {
            self.all()
                .into_iter()
                .filter(|(seen, _, _)| *seen == name)
                .map(|(_, value, _)| value)
                .collect()
        }

        fn names(&self) -> Vec<&'static str> {
            self.all().into_iter().map(|(name, _, _)| name).collect()
        }

        fn flush_count(&self) -> usize {
            self.flushes.load(Ordering::Relaxed)
        }
    }

    impl TelemetrySink for CaptureSink {
        fn record(&self, key: &MetricKey, value: MetricValue, labels: &[(FieldName, FieldValue)]) {
            self.seen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((key.name, value, labels.to_vec()));
        }

        fn flush(&self) {
            self.flushes.fetch_add(1, Ordering::Relaxed);
        }

        fn name(&self) -> &'static str {
            "capture"
        }
    }

    // `file` als default_sink, `app` unter "app." — zwei Standardziele; das
    // Handle der Rolle `R` ohne Null-Emission.
    fn fixture<R: DodRole>() -> (Arc<CaptureSink>, Arc<CaptureSink>, DodMetrics<R>) {
        let file = Arc::new(CaptureSink::default());
        let app = Arc::new(CaptureSink::default());
        let mut routing = RoutingSink::new(Arc::clone(&file) as Arc<dyn TelemetrySink>);
        routing.route("app.", Arc::clone(&app) as Arc<dyn TelemetrySink>);
        let dod = DodMetrics::<R>::new(Arc::new(routing));
        (file, app, dod)
    }

    // Serialisiert die Tests, die den prozessweiten
    // `WARDEN_UNAUDITED_VIOLATIONS` lesen oder erhöhen (gleiche Begründung
    // wie `SECURITY_COUNTER_LOCK` in `routing.rs`).
    static WARDEN_COUNTER_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_counter_reports_absolute_total_to_every_target() {
        let (file, app, dod) = fixture::<DispatchRole>();
        dod.verdict_recorded();
        dod.verdict_recorded();
        for target in [&file, &app] {
            assert_eq!(
                target.values_of("dod_triage_verdicts_total"),
                vec![MetricValue::Count(1), MetricValue::Count(2)],
                "running total, never an increment of 1"
            );
        }
    }

    #[test]
    fn test_each_work_counter_uses_its_own_series() {
        let (file, app, dod) = fixture::<DispatchRole>();
        dod.human_override();
        dod.unbound_verdict();
        dod.unbound_verdict();
        for target in [&file, &app] {
            assert_eq!(
                target.values_of("human_overrides_total"),
                vec![MetricValue::Count(1)]
            );
            assert_eq!(
                target.values_of("unbound_verdict_total"),
                vec![MetricValue::Count(1), MetricValue::Count(2)]
            );
            assert!(target.values_of("dod_triage_verdicts_total").is_empty());
        }
    }

    #[test]
    fn test_breaker_trip_counts_per_partition_kind_with_label() {
        let (file, app, dod) = fixture::<DispatchRole>();
        dod.breaker_trip(BreakerPartitionKind::Sensor);
        dod.breaker_trip(BreakerPartitionKind::Uid);
        dod.breaker_trip(BreakerPartitionKind::Sensor);
        for target in [&file, &app] {
            let breaker: Vec<(MetricValue, Vec<(FieldName, FieldValue)>)> = target
                .all()
                .into_iter()
                .filter(|(name, _, _)| *name == "breaker_trips_total")
                .map(|(_, value, labels)| (value, labels))
                .collect();
            assert_eq!(
                breaker,
                vec![
                    (
                        MetricValue::Count(1),
                        vec![(PARTITION_LABEL, FieldValue::Str("sensor"))]
                    ),
                    (
                        MetricValue::Count(1),
                        vec![(PARTITION_LABEL, FieldValue::Str("uid"))]
                    ),
                    (
                        MetricValue::Count(2),
                        vec![(PARTITION_LABEL, FieldValue::Str("sensor"))]
                    ),
                ]
            );
        }
    }

    #[test]
    fn test_breaker_partition_kinds_match_the_zero_registry_label_set() {
        let names: Vec<&str> = BreakerPartitionKind::ALL
            .iter()
            .map(|kind| kind.as_str())
            .collect();
        assert_eq!(names, BREAKER_PARTITIONS);
    }

    #[test]
    fn test_untriaged_backlog_is_a_gauge_and_saturates() {
        let (file, app, dod) = fixture::<DispatchRole>();
        dod.set_untriaged_backlog(7);
        dod.set_untriaged_backlog(0);
        dod.set_untriaged_backlog(usize::MAX);
        for target in [&file, &app] {
            assert_eq!(
                target.values_of("untriaged_backlog"),
                vec![
                    MetricValue::Gauge(7.0),
                    MetricValue::Gauge(0.0),
                    MetricValue::Gauge(f64::from(u32::MAX)),
                ]
            );
        }
    }

    #[test]
    fn test_meta_updates_never_reach_an_approved_export_target() -> TestResult {
        let file = Arc::new(CaptureSink::default());
        let export = Arc::new(CaptureSink::default());
        let mut routing = RoutingSink::new(Arc::clone(&file) as Arc<dyn TelemetrySink>);
        let approval =
            ExportApproval::try_granted("security.", "alice").map_err(ctx("approval"))?;
        assert!(
            SecurityExportGate::open(approval)
                .release(&mut routing, Arc::clone(&export) as Arc<dyn TelemetrySink>)
        );
        let routing = Arc::new(routing);
        let dispatch = DodMetrics::<DispatchRole>::new(Arc::clone(&routing));
        let sensor = DodMetrics::<SensorRole>::new(Arc::clone(&routing));
        dispatch.verdict_recorded();
        dispatch.human_override();
        dispatch.unbound_verdict();
        dispatch.breaker_trip(BreakerPartitionKind::Cgroup);
        dispatch.set_untriaged_backlog(1);
        sensor.sensor_parse_error();
        assert_eq!(file.all().len(), 6);
        assert!(
            export.all().is_empty(),
            "export targets never see meta series"
        );
        Ok(())
    }

    #[test]
    fn test_warden_unaudited_action_fans_out_and_trips_the_null_counter() -> TestResult {
        let _guard = WARDEN_COUNTER_LOCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let before = WARDEN_UNAUDITED_VIOLATIONS.count();
        let (file, app, dod) = fixture::<WardenRole>();

        dod.warden_unaudited_action();

        for target in [&file, &app] {
            assert_eq!(
                target.values_of("warden_unaudited_actions"),
                vec![MetricValue::Count(1)]
            );
        }
        assert_eq!(
            WARDEN_UNAUDITED_VIOLATIONS.count(),
            before.saturating_add(1)
        );
        let mut registry = NullCounterRegistry::new();
        registry.register(&WARDEN_UNAUDITED_VIOLATIONS);
        let Err(error) = assert_all_zero(&registry) else {
            return Err(TestError::Unexpected(
                "a registered warden violation counter must trip assert_all_zero".to_owned(),
            ));
        };
        assert!(error.to_string().contains("warden_unaudited_actions"));
        Ok(())
    }

    // Die Dispatch- und Sensor-Handles können `warden_unaudited_action` gar
    // nicht aufrufen (Compile-Fehler, siehe `compile_fail`-Beispiel in der
    // Moduldoku). Dieser Test belegt die Laufzeitseite: jede Meldung, die
    // diese Handles haben, lässt den Warden-Nullzähler und die
    // Warden-Serie unberührt.
    #[test]
    fn test_dispatch_and_sensor_handles_never_touch_the_warden_counter() {
        let _guard = WARDEN_COUNTER_LOCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let before = WARDEN_UNAUDITED_VIOLATIONS.count();
        let (dispatch_file, _dispatch_app, dispatch) = fixture::<DispatchRole>();
        dispatch.verdict_recorded();
        dispatch.human_override();
        dispatch.unbound_verdict();
        dispatch.breaker_trip(BreakerPartitionKind::Uid);
        dispatch.set_untriaged_backlog(2);
        let (sensor_file, _sensor_app, sensor) = fixture::<SensorRole>();
        sensor.sensor_parse_error();
        sensor.sensor_parse_errors(4);
        assert_eq!(WARDEN_UNAUDITED_VIOLATIONS.count(), before);
        for target in [&dispatch_file, &sensor_file] {
            assert!(target.values_of("warden_unaudited_actions").is_empty());
        }
    }

    #[test]
    fn test_concurrent_updates_reach_every_target_in_ascending_order() -> TestResult {
        const THREADS: u64 = 4;
        const PER_THREAD: u64 = 50;
        let (file, app, dod) = fixture::<DispatchRole>();
        let dod = Arc::new(dod);
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let dod = Arc::clone(&dod);
                thread::spawn(move || {
                    for _ in 0..PER_THREAD {
                        dod.verdict_recorded();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle
                .join()
                .map_err(|_| TestError::Unexpected("worker thread panicked".to_owned()))?;
        }
        let expected: Vec<MetricValue> = (1..=THREADS.saturating_mul(PER_THREAD))
            .map(MetricValue::Count)
            .collect();
        for target in [&file, &app] {
            assert_eq!(
                target.values_of("dod_triage_verdicts_total"),
                expected,
                "every target sees the same strictly ascending totals"
            );
        }
        Ok(())
    }

    // Ergebnis von `installed`: drei Ziele, der geteilte Sink und das Handle.
    struct Installed<R: DodRole> {
        file: Arc<CaptureSink>,
        app: Arc<CaptureSink>,
        export: Arc<CaptureSink>,
        sink: Arc<RoutingSink>,
        dod: DodMetrics<R>,
    }

    // `file` als default_sink, `app` unter "app.", dazu ein freigegebenes
    // Exportziel; danach `install` für die Rolle `R`.
    fn installed<R: DodRole>() -> TestResult<Installed<R>> {
        let file = Arc::new(CaptureSink::default());
        let app = Arc::new(CaptureSink::default());
        let export = Arc::new(CaptureSink::default());
        let mut routing = RoutingSink::new(Arc::clone(&file) as Arc<dyn TelemetrySink>);
        routing.route("app.", Arc::clone(&app) as Arc<dyn TelemetrySink>);
        let approval =
            ExportApproval::try_granted("security.", "alice").map_err(ctx("approval"))?;
        assert!(
            SecurityExportGate::open(approval)
                .release(&mut routing, Arc::clone(&export) as Arc<dyn TelemetrySink>)
        );
        let (sink, dod) = DodMetrics::<R>::install(routing).map_err(ctx("install"))?;
        Ok(Installed {
            file,
            app,
            export,
            sink,
            dod,
        })
    }

    #[test]
    fn test_install_emits_only_the_role_zero_series_to_every_standard_target() -> TestResult {
        let Installed {
            file, app, export, ..
        } = installed::<DispatchRole>()?;
        let expected = vec![
            "dod_triage_verdicts_total",
            "human_overrides_total",
            "unbound_verdict_total",
            "breaker_trips_total",
            "breaker_trips_total",
            "breaker_trips_total",
            "untriaged_backlog",
        ];
        for target in [&file, &app] {
            assert_eq!(target.names(), expected);
            for (name, value, _) in target.all() {
                let zero = if name == "untriaged_backlog" {
                    MetricValue::Gauge(0.0)
                } else {
                    MetricValue::Count(0)
                };
                assert_eq!(value, zero, "{name} must start at zero");
            }
        }
        assert!(
            export.all().is_empty(),
            "export targets never see zero series"
        );
        Ok(())
    }

    #[test]
    fn test_install_for_sensor_and_warden_emits_only_their_series() -> TestResult {
        let sensor = installed::<SensorRole>()?;
        let warden = installed::<WardenRole>()?;
        for (target, name) in [
            (&sensor.file, "sensor_parse_errors_total"),
            (&sensor.app, "sensor_parse_errors_total"),
            (&warden.file, "warden_unaudited_actions"),
            (&warden.app, "warden_unaudited_actions"),
        ] {
            assert_eq!(target.names(), vec![name]);
        }
        assert!(sensor.export.all().is_empty());
        assert!(warden.export.all().is_empty());
        assert_eq!(sensor.dod.emitter(), DodEmitter::Sensor);
        assert_eq!(warden.dod.emitter(), DodEmitter::Warden);
        Ok(())
    }

    #[test]
    fn test_install_does_not_flush_any_target() -> TestResult {
        let Installed {
            file, app, export, ..
        } = installed::<DispatchRole>()?;
        for target in [&file, &app, &export] {
            assert_eq!(
                target.flush_count(),
                0,
                "network targets must not be contacted during install"
            );
        }
        Ok(())
    }

    #[test]
    fn test_install_returns_the_sink_the_handle_reports_through() -> TestResult {
        let Installed {
            file,
            app,
            export,
            sink,
            dod,
        } = installed::<DispatchRole>()?;
        assert!(Arc::ptr_eq(&sink, &dod.routing));
        assert_eq!(dod.emitter(), DodEmitter::Dispatch);
        dod.verdict_recorded();
        for target in [&file, &app] {
            assert_eq!(
                target.values_of("dod_triage_verdicts_total"),
                vec![MetricValue::Count(0), MetricValue::Count(1)],
                "zero first, then the running total"
            );
        }
        assert!(export.all().is_empty());
        sink.flush();
        for target in [&file, &app, &export] {
            assert_eq!(target.flush_count(), 1);
        }
        Ok(())
    }

    #[test]
    fn test_sensor_parse_errors_batches_into_one_delivery() {
        let (file, app, dod) = fixture::<SensorRole>();
        dod.sensor_parse_errors(0);
        dod.sensor_parse_errors(5);
        dod.sensor_parse_error();
        dod.sensor_parse_errors(u64::MAX);
        for target in [&file, &app] {
            assert_eq!(
                target.values_of("sensor_parse_errors_total"),
                vec![
                    MetricValue::Count(5),
                    MetricValue::Count(6),
                    MetricValue::Count(u64::MAX),
                ],
                "zero reports nothing, a batch is one delivery, totals saturate"
            );
        }
    }

    // Was ein frisches Handle der Rolle `R` an ein separates Ziel
    // nachliefert.
    fn republished_fresh<R: DodRole>() -> Vec<Seen> {
        let (_file, _app, dod) = fixture::<R>();
        let target = CaptureSink::default();
        let count = dod.republish_to(&target);
        let seen = target.all();
        assert_eq!(count, seen.len(), "{}", R::EMITTER.as_str());
        seen
    }

    // Die Null-Registry der Rolle als (Name, Wert, Labels).
    fn zero_records(emitter: DodEmitter) -> TestResult<Vec<Seen>> {
        Ok(dod_zero_registry_for(emitter)
            .map_err(ctx("role registry"))?
            .records()
            .into_iter()
            .map(|record| (record.key.name, record.value, record.labels))
            .collect())
    }

    #[test]
    fn test_republish_matches_the_role_zero_registry() -> TestResult {
        // A fresh handle republishes exactly its role's zero series, with the
        // same labels and in the same order as `install` emitted them.
        assert_eq!(
            republished_fresh::<DispatchRole>(),
            zero_records(DodEmitter::Dispatch)?
        );
        assert_eq!(
            republished_fresh::<SensorRole>(),
            zero_records(DodEmitter::Sensor)?
        );
        assert_eq!(
            republished_fresh::<WardenRole>(),
            zero_records(DodEmitter::Warden)?
        );
        Ok(())
    }

    #[test]
    fn test_role_series_name_exactly_the_role_metrics() {
        for emitter in DodEmitter::ALL {
            let mut names: Vec<&str> = role_series(emitter)
                .iter()
                .map(|series| series.key().name)
                .collect();
            names.dedup();
            assert_eq!(names, emitter.series_names(), "{}", emitter.as_str());
        }
    }

    #[test]
    fn test_republish_to_sends_current_totals_only_to_the_target() {
        let (file, app, dod) = fixture::<DispatchRole>();
        dod.verdict_recorded();
        dod.verdict_recorded();
        dod.breaker_trip(BreakerPartitionKind::Cgroup);
        dod.set_untriaged_backlog(9);
        let before = (file.all().len(), app.all().len());

        let otlp = CaptureSink::default();
        assert_eq!(dod.republish_to(&otlp), 7);

        assert_eq!(
            (file.all().len(), app.all().len()),
            before,
            "the other targets get no repetition"
        );
        assert_eq!(
            otlp.all(),
            vec![
                ("dod_triage_verdicts_total", MetricValue::Count(2), vec![]),
                ("human_overrides_total", MetricValue::Count(0), vec![]),
                ("unbound_verdict_total", MetricValue::Count(0), vec![]),
                (
                    "breaker_trips_total",
                    MetricValue::Count(0),
                    vec![(PARTITION_LABEL, FieldValue::Str("uid"))]
                ),
                (
                    "breaker_trips_total",
                    MetricValue::Count(1),
                    vec![(PARTITION_LABEL, FieldValue::Str("cgroup"))]
                ),
                (
                    "breaker_trips_total",
                    MetricValue::Count(0),
                    vec![(PARTITION_LABEL, FieldValue::Str("sensor"))]
                ),
                ("untriaged_backlog", MetricValue::Gauge(9.0), vec![]),
            ]
        );
    }

    #[test]
    fn test_republish_for_warden_does_not_count_a_new_violation() {
        let _guard = WARDEN_COUNTER_LOCK
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (_file, _app, dod) = fixture::<WardenRole>();
        dod.warden_unaudited_action();
        let before = WARDEN_UNAUDITED_VIOLATIONS.count();
        let target = CaptureSink::default();
        assert_eq!(dod.republish_to(&target), 1);
        assert_eq!(
            target.values_of("warden_unaudited_actions"),
            vec![MetricValue::Count(1)]
        );
        assert_eq!(WARDEN_UNAUDITED_VIOLATIONS.count(), before);
    }

    #[test]
    fn test_only_the_warden_role_carries_a_null_counter() {
        assert!(dod_null_counters(DodEmitter::Dispatch).is_empty());
        assert!(dod_null_counters(DodEmitter::Sensor).is_empty());
        let warden = dod_null_counters(DodEmitter::Warden);
        assert_eq!(warden.len(), 1);
        let mut registry = NullCounterRegistry::new();
        for counter in warden {
            assert!(DodEmitter::Warden.owns(counter.name()));
            registry.register(counter);
        }
        assert_eq!(
            registry
                .all()
                .iter()
                .map(|counter| counter.name())
                .collect::<Vec<_>>(),
            vec!["warden_unaudited_actions"]
        );
    }

    #[test]
    fn test_role_markers_map_to_their_emitter() {
        assert_eq!(DispatchRole::EMITTER, DodEmitter::Dispatch);
        assert_eq!(SensorRole::EMITTER, DodEmitter::Sensor);
        assert_eq!(WardenRole::EMITTER, DodEmitter::Warden);
    }

    #[test]
    fn test_dod_metrics_is_send_sync_debug() {
        fn assert_bounds<T: Send + Sync + std::fmt::Debug>() {}
        assert_bounds::<DispatchMetrics>();
        assert_bounds::<SensorMetrics>();
        assert_bounds::<WardenMetrics>();
    }
}
