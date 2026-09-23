//! `Sentinel`: hält Sensoren, ruft `poll`, führt den Automaten, puffert
//! (Knoten AW2-18).
//!
//! # Was `Sentinel` NICHT tut
//! Es deutet keine Quelle. Jeder Aufruf an einen Sensor geht über
//! [`harw_dod_signals::Sensor::poll`] — `Sentinel` kennt nur, ob dieser
//! Aufruf `Ok` oder `Err` lieferte und, im Fehlerfall,
//! [`harw_dod_cap::error::SensorError::permanence`]. Der Inhalt eines
//! `Ok`-Ergebnisses ([`harw_dod_signals::SensorReading`]) wird unverändert
//! durchgereicht — nie inspiziert, nie umgedeutet.
//!
//! # Aufbau
//! Ein `Sentinel` hält:
//! - je Sensor einen [`crate::health::SensorHealth`] (siehe `health`-Moduldoku
//!   für den vollständigen Automaten),
//! - genau eine [`crate::buffer::EvidenceBuffer`] für alle Sensoren
//!   gemeinsam (die Trennung nach Strom, nicht nach Sensor, ist bewusst:
//!   siehe `buffer`-Moduldoku),
//! - einen `Arc<dyn TelemetrySink>` für die in [`crate::metrics`]
//!   deklarierten Kennzahlen.
//!
//! # Zwei Schreibwege in den Puffer
//! [`Self::buffer`] hat zwei Schreibwege, nicht einen:
//!
//! - [`Self::poll_all`] — der einzige Weg, über den diese Crate selbst je
//!   ein Ereignis oder Sample **erzeugt**: es ruft `Sensor::poll` auf einen
//!   bei [`Self::new`] registrierten Sensor auf und puffert dessen
//!   Ergebnis, einschließlich der `SensorDegraded`-Ereignisse, die der
//!   Degradationsautomat (siehe `health`-Moduldoku) selbst auslöst.
//! - [`Self::record_external_event`] — für ein
//!   [`harw_dod_signals::SecurityEvent`], das anderswo bereits vollständig
//!   entstanden ist und diese Crate nur noch zitierfähig einfrieren soll:
//!   ein privilegierter Sondenprozess (`harw-probe-fs`, `harw-probe-bpf`,
//!   künftig ein Netlink-Warden), der es über einen
//!   `SOCK_SEQPACKET`-Socket an das Binary aus Knoten AW2-19 schickt
//!   (Vertrag Entscheidung Nr. 3), oder dieses Binary selbst, wenn seine
//!   eigene Landlock-Selbstbeschränkung beim Start degradiert.
//!
//! Beide Wege münden in denselben Event-Ring und verhalten sich bei
//! Kapazitätsüberlauf identisch — stille FIFO-Verdrängung des ältesten
//! Eintrags, ohne Rückmeldung an den Aufrufer (siehe `buffer`-Moduldoku,
//! Abschnitt "Die Puffergröße als Sicherheitsentscheidung"). Das ist
//! Absicht, nicht Zufall: zwei Schreibwege mit unterschiedlichem
//! Überlaufverhalten wären selbst eine Fehlerquelle. Der Unterschied liegt
//! ausschließlich darin, **wer** das Ereignis erzeugt und bewertet hat,
//! nie darin, wie es gepuffert wird. [`Self::record_external_event`]
//! berührt dabei bewusst **nicht** den Degradationsautomaten: der kennt nur
//! die bei [`Self::new`] registrierten Sensoren dieser Instanz, keine
//! externen Absender — siehe dortige Methodendoku für die vollständige
//! Begründung, einschließlich der Frage der Fähigkeits-Buchführung.
//!
//! # Nebenläufigkeit
//! Sensoren werden als `Arc<dyn harw_dod_signals::Sensor>` gehalten (`Sensor:
//! Send + Sync`, siehe dortige Moduldoku: „der Sentinel hält Sensoren hinter
//! `Arc` und ruft sie aus einem Sammelthread auf"). [`Sentinel`] selbst ist
//! `Send + Sync` (jedes Feld ist es), wird in dieser Crate aber mit
//! sequenziellem Zugriff aus einem einzigen Sammelthread entworfen:
//! [`Sentinel::poll_all`] nimmt `&mut self` und ruft jeden fälligen Sensor
//! nacheinander ab. Die `Send + Sync`-Bound auf `Sensor` hält die Tür für
//! eine künftige parallele Abrufschleife offen, ohne dass diese Crate sie
//! heute selbst bräuchte oder implementiert.
//!
//! # Fehler
//! [`crate::error::SentinelError`] — ausschließlich über [`Sentinel::freeze`],
//! das an [`crate::buffer::EvidenceBuffer::freeze`] delegiert. Ein
//! fehlgeschlagener Sensor-Abruf ist kein `Err` dieser Crate, sondern ein
//! Automaten-Übergang (siehe `health`-Moduldoku).
//!
//! # Examples
//! ```rust
//! use harw_dod_sentinel::{Sentinel, SentinelConfig};
//! use harw_dod_signals::{Sensor, SensorReading};
//! use harw_observe::NullSink;
//! use harw_types::SensorId;
//! use std::sync::Arc;
//!
//! #[derive(Debug)]
//! struct AlwaysEmpty {
//!     handle: harw_dod_cap::SensorHandle<harw_dod_cap::Bound>,
//! }
//!
//! impl Sensor for AlwaysEmpty {
//!     fn handle(&self) -> &harw_dod_cap::SensorHandle<harw_dod_cap::Bound> {
//!         &self.handle
//!     }
//!
//!     fn poll(&self, _now: jiff::Timestamp) -> Result<SensorReading, harw_dod_cap::SensorError> {
//!         Ok(SensorReading::default())
//!     }
//! }
//!
//! let scope = harw_dod_cap::ReadScope::from_roots([std::path::PathBuf::from("/proc/stat")]);
//! let handle = harw_dod_cap::SensorHandle::new(
//!     SensorId::from_str("proc-stat-0"),
//!     harw_dod_cap::Capability::ReadProcStat,
//! )
//! .bind(scope);
//! let sensor: Arc<dyn Sensor> = Arc::new(AlwaysEmpty { handle });
//!
//! let mut sentinel = Sentinel::new(vec![sensor], Arc::new(NullSink), SentinelConfig::default());
//! let reading = sentinel.poll_all(jiff::Timestamp::UNIX_EPOCH);
//! assert!(reading.samples.is_empty());
//! assert_eq!(sentinel.degraded_count(), 0);
//! ```

use std::sync::Arc;

use harw_dod_signals::{EventKind, SecurityEvent, SecurityEvidence, Sensor, SensorReading};
use harw_observe::TelemetrySink;
use harw_types::SensorId;
use jiff::Timestamp;

use crate::buffer::EvidenceBuffer;
use crate::error::SentinelResult;
use crate::health::{RetryPolicy, SensorHealth};
use crate::metrics::{self, BufferKind};

/// Konstruktionsparameter eines [`Sentinel`].
///
/// # Description
/// Bündelt die drei in dieser Crate bewusst offen konfigurierbaren
/// Sicherheits-/Kapazitätsentscheidungen (siehe `health`- und
/// `buffer`-Moduldoku für die jeweilige Begründung) an einer Stelle, statt
/// sie als drei lose Konstruktorparameter zu führen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SentinelConfig {
    /// Obergrenze und Wartezeit für Rückversuche nach einem
    /// `Transient`-Fehler.
    pub retry_policy: RetryPolicy,
    /// Kapazität des Sample-Rings ([`EvidenceBuffer::sample_capacity`]).
    pub sample_capacity: usize,
    /// Kapazität des Event-Rings ([`EvidenceBuffer::event_capacity`]).
    pub event_capacity: usize,
}

impl SentinelConfig {
    /// Baut eine Konfiguration aus expliziten Werten.
    ///
    /// # Arguments
    /// - `retry_policy` (`RetryPolicy`): siehe [`Self::retry_policy`].
    /// - `sample_capacity` (`usize`): siehe [`Self::sample_capacity`].
    /// - `event_capacity` (`usize`): siehe [`Self::event_capacity`].
    ///
    /// # Returns
    /// Eine `SentinelConfig` mit genau diesen Werten.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_sentinel::health::RetryPolicy;
    /// use harw_dod_sentinel::SentinelConfig;
    ///
    /// let config = SentinelConfig::new(RetryPolicy::default(), 128, 256);
    /// assert_eq!(config.sample_capacity, 128);
    /// ```
    #[must_use]
    pub const fn new(
        retry_policy: RetryPolicy,
        sample_capacity: usize,
        event_capacity: usize,
    ) -> Self {
        Self {
            retry_policy,
            sample_capacity,
            event_capacity,
        }
    }
}

impl Default for SentinelConfig {
    /// Siehe [`RetryPolicy::default`], [`EvidenceBuffer::DEFAULT_SAMPLE_CAPACITY`]
    /// und [`EvidenceBuffer::DEFAULT_EVENT_CAPACITY`] für die jeweilige
    /// Begründung.
    fn default() -> Self {
        Self {
            retry_policy: RetryPolicy::default(),
            sample_capacity: EvidenceBuffer::DEFAULT_SAMPLE_CAPACITY,
            event_capacity: EvidenceBuffer::DEFAULT_EVENT_CAPACITY,
        }
    }
}

/// Ein Sensor mit seinem aktuellen Gesundheitszustand.
///
/// Privat: Aufrufer sehen den Zustand nur über [`Sentinel::health_snapshot`],
/// nie direkt diese Struktur.
#[derive(Debug)]
struct SensorSlot {
    sensor: Arc<dyn Sensor>,
    health: SensorHealth,
}

/// Die Sammelstelle: hält Sensoren, ruft `poll`, führt den
/// Degradationsautomaten, puffert die Ergebnisse.
///
/// # Description
/// Siehe Moduldoku für Aufbau, Nebenläufigkeit und Fehlerverhalten.
#[derive(Debug)]
pub struct Sentinel {
    slots: Vec<SensorSlot>,
    buffer: EvidenceBuffer,
    sink: Arc<dyn TelemetrySink>,
    retry_policy: RetryPolicy,
}

impl Sentinel {
    /// Baut einen Sentinel aus einer festen Sensorenliste.
    ///
    /// # Description
    /// Jeder Sensor startet im Zustand [`SensorHealth::Bound`]. Die
    /// Sensorenliste ist ab hier fest — es gibt keine Methode, um später
    /// einen Sensor hinzuzufügen oder zu entfernen; ein Neustart mit einer
    /// neuen `sensors`-Liste ist der einzige vorgesehene Weg, die Menge zu
    /// ändern (derselbe Mechanismus, über den ein `Degraded`-Sensor wieder
    /// `Bound` werden kann — siehe `health`-Moduldoku, Abschnitt "Der
    /// Rückweg aus `Degraded`").
    ///
    /// # Arguments
    /// - `sensors` (`Vec<Arc<dyn harw_dod_signals::Sensor>>`): die
    ///   abzurufenden Sensoren, in der Reihenfolge, in der
    ///   [`Self::poll_all`] sie abruft.
    /// - `sink` (`Arc<dyn TelemetrySink>`): das Ziel der in
    ///   [`crate::metrics`] deklarierten Kennzahlen.
    /// - `config` (`SentinelConfig`): Rückversuchsrichtlinie und
    ///   Puffergrößen.
    ///
    /// # Returns
    /// Einen `Sentinel` ohne bisherige Abrufe: leerer Puffer, jeder Sensor
    /// `Bound`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_sentinel::{Sentinel, SentinelConfig};
    /// use harw_observe::NullSink;
    /// use std::sync::Arc;
    ///
    /// let sentinel = Sentinel::new(vec![], Arc::new(NullSink), SentinelConfig::default());
    /// assert_eq!(sentinel.sensor_count(), 0);
    /// ```
    #[must_use]
    pub fn new(
        sensors: Vec<Arc<dyn Sensor>>,
        sink: Arc<dyn TelemetrySink>,
        config: SentinelConfig,
    ) -> Self {
        let slots = sensors
            .into_iter()
            .map(|sensor| SensorSlot {
                sensor,
                health: SensorHealth::Bound,
            })
            .collect();
        Self {
            slots,
            buffer: EvidenceBuffer::new(config.sample_capacity, config.event_capacity),
            sink,
            retry_policy: config.retry_policy,
        }
    }

    /// Anzahl der gehaltenen Sensoren, unabhängig von ihrem Zustand.
    #[must_use]
    pub fn sensor_count(&self) -> usize {
        self.slots.len()
    }

    /// Aktuelle Zahl der Sensoren im Zustand [`SensorHealth::Degraded`].
    ///
    /// # Returns
    /// Die Anzahl — der Wert, den [`crate::metrics::record_degraded_sensors`]
    /// nach jedem [`Self::poll_all`]-Aufruf emittiert.
    #[must_use]
    pub fn degraded_count(&self) -> usize {
        self.slots
            .iter()
            .filter(|slot| slot.health.is_degraded())
            .count()
    }

    /// Der Gesundheitszustand jedes gehaltenen Sensors, in
    /// Registrierungsreihenfolge.
    ///
    /// # Returns
    /// Ein `Vec` aus `(Kennung, Zustand)`, geklont aus dem aktuellen Stand.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_sentinel::{Sentinel, SentinelConfig};
    /// use harw_observe::NullSink;
    /// use std::sync::Arc;
    ///
    /// let sentinel = Sentinel::new(vec![], Arc::new(NullSink), SentinelConfig::default());
    /// assert!(sentinel.health_snapshot().is_empty());
    /// ```
    #[must_use]
    pub fn health_snapshot(&self) -> Vec<(SensorId, SensorHealth)> {
        self.slots
            .iter()
            .map(|slot| (slot.sensor.handle().id().clone(), slot.health.clone()))
            .collect()
    }

    /// Referenz auf den internen Ringpuffer.
    ///
    /// # Returns
    /// Eine Referenz auf die [`EvidenceBuffer`], in die [`Self::poll_all`]
    /// jedes beobachtete Sample und Event einträgt.
    #[must_use]
    pub fn buffer(&self) -> &EvidenceBuffer {
        &self.buffer
    }

    /// Ruft jeden fälligen Sensor einmal ab, führt den Automaten fort und
    /// puffert das Ergebnis.
    ///
    /// # Description
    /// Für jeden Sensor, dessen [`SensorHealth::is_due`] bei `now` `true`
    /// liefert (also jeden `Bound`-Sensor und jeden `Retrying`-Sensor, dessen
    /// `next_attempt` erreicht ist — nie einen `Degraded`-Sensor):
    ///
    /// 1. [`harw_dod_signals::Sensor::poll`] wird aufgerufen;
    ///    [`crate::metrics::record_poll`] wird emittiert.
    /// 2. Bei `Ok(reading)`: der Zustand geht über [`SensorHealth::advance`]
    ///    zu `Bound`; `reading.samples`/`reading.events` werden dem
    ///    Rückgabewert dieses Aufrufs angehängt.
    /// 3. Bei `Err(err)`: [`crate::metrics::record_error`] wird mit
    ///    `err.permanence()` emittiert; der Zustand geht über
    ///    [`SensorHealth::advance`] weiter. Führt dieser Übergang zu
    ///    `Degraded` (egal ob wegen `Permanent` oder ausgeschöpfter
    ///    Rückversuche), wird ein
    ///    [`harw_dod_signals::EventKind::SensorDegraded`]-Ereignis erzeugt
    ///    und ebenfalls dem Rückgabewert angehängt — **das ist der Punkt,
    ///    an dem eine stille Abmeldung zu einer Meldung wird** (siehe
    ///    Crate-Moduldoku).
    ///
    /// Nach der Abrufrunde werden alle in diesem Aufruf beobachteten
    /// Samples und Events (einschließlich neu erzeugter
    /// `SensorDegraded`-Events) in [`Self::buffer`] übernommen, und
    /// [`crate::metrics::record_degraded_sensors`] sowie
    /// [`crate::metrics::record_buffer_utilization`] (für beide Ringe)
    /// werden emittiert.
    ///
    /// # Arguments
    /// - `now` (`jiff::Timestamp`): injizierte Zeit, unverändert an jeden
    ///   abgerufenen Sensor sowie an [`SensorHealth::advance`]
    ///   weitergereicht. Diese Methode liest nie die Systemuhr selbst.
    ///
    /// # Returns
    /// Ein [`SensorReading`] mit allen in dieser Runde beobachteten Samples
    /// und Events, einschließlich etwaiger `SensorDegraded`-Events.
    ///
    /// # Concurrency
    /// Erfordert `&mut self`; ruft Sensoren sequenziell in der Reihenfolge
    /// von [`Self::new`] ab (siehe Moduldoku, Abschnitt "Nebenläufigkeit").
    ///
    /// # Examples
    /// Siehe Moduldoku für ein vollständiges Beispiel mit einem Mock-Sensor.
    pub fn poll_all(&mut self, now: Timestamp) -> SensorReading {
        let mut aggregate = SensorReading::default();
        let retry_policy = self.retry_policy;
        let sink = Arc::clone(&self.sink);

        for slot in &mut self.slots {
            if !slot.health.is_due(now) {
                continue;
            }

            let id = slot.sensor.handle().id().clone();
            metrics::record_poll(sink.as_ref(), &id);

            match slot.sensor.poll(now) {
                Ok(reading) => {
                    slot.health = slot.health.advance(Ok(()), now, retry_policy);
                    aggregate.samples.extend(reading.samples);
                    aggregate.events.extend(reading.events);
                }
                Err(err) => {
                    let permanence = err.permanence();
                    metrics::record_error(sink.as_ref(), &id, permanence);
                    slot.health = slot.health.advance(Err(permanence), now, retry_policy);

                    if slot.health.is_degraded() {
                        aggregate.events.push(SecurityEvent {
                            sensor: id.clone(),
                            observed_at: now,
                            actor: None,
                            kind: EventKind::SensorDegraded { sensor: id },
                        });
                    }
                }
            }
        }

        for sample in &aggregate.samples {
            self.buffer.push_sample(sample.clone());
        }
        for event in &aggregate.events {
            self.buffer.push_event(event.clone());
        }

        metrics::record_degraded_sensors(sink.as_ref(), self.degraded_count());
        metrics::record_buffer_utilization(
            sink.as_ref(),
            BufferKind::Samples,
            self.buffer.sample_utilization(),
        );
        metrics::record_buffer_utilization(
            sink.as_ref(),
            BufferKind::Events,
            self.buffer.event_utilization(),
        );

        aggregate
    }

    /// Nimmt ein anderswo bereits entstandenes Ereignis in den Event-Ring
    /// auf.
    ///
    /// # Description
    /// Der zweite Schreibweg in [`Self::buffer`], neben [`Self::poll_all`]
    /// (siehe Moduldoku, Abschnitt "Zwei Schreibwege in den Puffer", für die
    /// vollständige Begründung, warum es zwei sind statt eines).
    /// `poll_all` erzeugt seine Ereignisse ausschließlich aus dem Ergebnis
    /// eines `Sensor::poll`-Aufrufs auf einen bei [`Self::new`]
    /// registrierten Sensor; diese Methode ist für jedes
    /// [`harw_dod_signals::SecurityEvent`] gedacht, das auf einem anderen
    /// Weg entstanden ist — heute zwei belegte Fälle im Binary aus Knoten
    /// AW2-19: ein privilegierter Sondenprozess, der sein Ereignis über
    /// einen `SOCK_SEQPACKET`-Socket an dieses Binary schickt (Vertrag
    /// Entscheidung Nr. 3), und dieses Binary selbst, wenn seine eigene
    /// Landlock-Selbstbeschränkung beim Start degradiert
    /// (`EventKind::SensorDegraded`, bislang nur über `tracing::warn!`
    /// beobachtbar, weil dieser Schreibweg fehlte).
    ///
    /// Diese Methode deutet `event` nicht — genau wie `poll_all` reicht sie
    /// es unverändert an [`crate::buffer::EvidenceBuffer::push_event`]
    /// weiter (siehe Crate-Moduldoku, Abschnitt "Die härteste Auflage:
    /// keine Parselogik"). Sie fasst `event.kind` insbesondere nie an: ein
    /// [`harw_dod_signals::EventKind::SensorDegraded`], das von außen
    /// hereinkommt, landet im selben Ring wie eines, das `poll_all` selbst
    /// erzeugt hat — ununterscheidbar im Puffer und in einem danach
    /// gezogenen [`Self::freeze`].
    ///
    /// **Was diese Methode bewusst NICHT tut:** sie berührt weder den
    /// Degradationsautomaten (die `SensorHealth` je Slot aus [`Self::new`])
    /// noch [`Self::degraded_count`] noch die
    /// [`crate::metrics::SENSORS_DEGRADED`]-Kennzahl. Beide beschreiben
    /// ausschließlich den Zustand der bei [`Self::new`] registrierten
    /// `Sensor`-Trait-Objekte dieser konkreten `Sentinel`-Instanz; ein
    /// extern erzeugtes Ereignis trägt keine solche Registrierung — sein
    /// `sensor`-Feld kann auf einen Sondenprozess oder auf dieses Binary
    /// selbst zeigen, nie auf einen Eintrag in [`Self::health_snapshot`].
    /// Es dort einzuordnen, müsste entweder raten (bei zufälliger
    /// Namensgleichheit) oder ein Feld erfinden, das
    /// [`harw_dod_signals::SecurityEvent`] nicht trägt — beides schlechter
    /// als die ehrliche Auskunft, dass dieser Automat ein externes Ereignis
    /// nicht erfasst. Die einzige tatsächlich vorhandene
    /// Fähigkeits-Buchführung dieses Ausbauprogramms
    /// (`harw_sentinel::sensors::all_unprivileged`, außerhalb dieser Crate)
    /// prüft ohnehin die Sensorenliste **vor** der Konstruktion eines
    /// `Sentinel` und liest nie dessen Laufzeitzustand — sie wird von dieser
    /// Methode weder verändert noch stillschweigend umgangen, weil sie gar
    /// nichts an dieser `Sentinel`-Instanz einsieht, das diese Methode
    /// verändern könnte.
    ///
    /// Die Herkunft geht dabei nicht verloren: statt ein zusätzliches Feld
    /// zu erfinden, trägt [`harw_dod_signals::SecurityEvent::sensor`]
    /// ("Welcher Sensor das Ereignis erzeugt hat") diese Auskunft bereits —
    /// dieselbe Kennung, die `poll_all` für intern erzeugte Ereignisse
    /// setzt. Der Aufrufer setzt `event.sensor` vor der Übergabe auf eine
    /// für den externen Absender sprechende Kennung (z. B. die Sensorkennung
    /// der sendenden Sonde, oder eine für die Selbstdiagnose des Binaries
    /// reservierte Kennung wie im Landlock-Fall oben).
    ///
    /// # Arguments
    /// - `event` (`harw_dod_signals::SecurityEvent`): das extern erzeugte
    ///   Ereignis, unverändert übernommen.
    ///
    /// # Returns
    /// `()`. Absichtlich ohne Rückgabewert: identisch zu
    /// [`crate::buffer::EvidenceBuffer::push_event`] und zu der Art, wie
    /// [`Self::poll_all`] selbst mit dem Event-Ring umgeht — beide melden
    /// dem Aufrufer nie, ob ein Eintrag beim Erreichen der Kapazität
    /// verdrängt wurde. Zwei Schreibwege mit unterschiedlichem
    /// Überlaufverhalten wären eine eigene Fehlerquelle; diese Methode
    /// übernimmt deshalb exakt das Verhalten von `poll_all`, statt ein
    /// zweites zu erfinden.
    ///
    /// # Errors
    /// Keine — diese Methode ist total. Der einzige Fehlerpfad dieser Crate
    /// bleibt [`Self::freeze`]/[`crate::buffer::EvidenceBuffer::freeze`]
    /// (siehe Crate-Moduldoku, Abschnitt "Fehler").
    ///
    /// # Concurrency
    /// Erfordert `&mut self`, exakt wie [`Self::poll_all`] — diese Crate legt
    /// kein zweites Schutzmodell für denselben Puffer an (siehe
    /// [`crate::buffer`]-Moduldoku, Abschnitt "Nebenläufigkeit": kein
    /// Interior Mutability, kein internes Locking). Empfängt ein Aufrufer
    /// externe Ereignisse auf einem anderen Thread als dem, der `poll_all`
    /// aufruft (z. B. ein IPC-Empfangsthread parallel zur Sammelschleife),
    /// muss **er** den exklusiven Zugriff auf diese `Sentinel`-Instanz
    /// sicherstellen — etwa, indem er eingehende Ereignisse zunächst in eine
    /// eigene, unabhängig gesperrte Zwischenablage einreiht (wie
    /// `harw_sentinel::ipc::IpcInbox` hinter ihrem eigenen `Mutex`) und sie
    /// von demselben Thread, der auch `poll_all` aufruft, über diese
    /// Methode abholt — statt diese `Sentinel`-Instanz selbst mit einem
    /// `Mutex` zu umschließen und von mehreren Threads gleichzeitig
    /// `&mut self` zu verlangen (ein zweites Schutzmodell, das diese Crate
    /// bewusst nicht anlegt).
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_sentinel::{Sentinel, SentinelConfig};
    /// use harw_dod_signals::{EventKind, SecurityEvent};
    /// use harw_observe::NullSink;
    /// use harw_types::SensorId;
    /// use std::sync::Arc;
    ///
    /// let mut sentinel = Sentinel::new(vec![], Arc::new(NullSink), SentinelConfig::default());
    /// let sensor = SensorId::from_str("probe-fs-0");
    /// sentinel.record_external_event(SecurityEvent {
    ///     sensor: sensor.clone(),
    ///     observed_at: jiff::Timestamp::UNIX_EPOCH,
    ///     actor: None,
    ///     kind: EventKind::SensorDegraded { sensor },
    /// });
    /// assert_eq!(sentinel.buffer().event_len(), 1);
    /// ```
    pub fn record_external_event(&mut self, event: SecurityEvent) {
        self.buffer.push_event(event);
        metrics::record_buffer_utilization(
            self.sink.as_ref(),
            BufferKind::Events,
            self.buffer.event_utilization(),
        );
    }

    /// Friert den aktuellen Pufferinhalt als zitierfähigen Beleg ein.
    ///
    /// # Description
    /// Bequemlichkeitsmethode, die an [`EvidenceBuffer::freeze`] auf
    /// [`Self::buffer`] delegiert — siehe dortige Doku für die vollständige
    /// Beschreibung (Reihenfolge, Digest-Bildung, Determinismus).
    ///
    /// # Arguments
    /// - `now` (`jiff::Timestamp`): injizierter Einfrierzeitpunkt.
    ///
    /// # Returns
    /// Ein [`SecurityEvidence`] mit dem aktuellen Pufferinhalt.
    ///
    /// # Errors
    /// - [`crate::error::SentinelError::Evidence`] — siehe
    ///   [`EvidenceBuffer::freeze`].
    ///
    /// # Concurrency
    /// Reine Lesefunktion über [`Self::buffer`].
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_sentinel::{Sentinel, SentinelConfig};
    /// use harw_observe::NullSink;
    /// use std::sync::Arc;
    ///
    /// let sentinel = Sentinel::new(vec![], Arc::new(NullSink), SentinelConfig::default());
    /// let evidence = sentinel
    ///     .freeze(jiff::Timestamp::UNIX_EPOCH)
    ///     .expect("empty buffer always encodes");
    /// assert!(evidence.samples.is_empty());
    /// ```
    pub fn freeze(&self, now: Timestamp) -> SentinelResult<SecurityEvidence> {
        self.buffer.freeze(now)
    }
}

#[cfg(test)]
mod tests {
    use super::{Sentinel, SentinelConfig};
    use crate::health::{DegradeReason, RetryPolicy, SensorHealth};
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_dod_cap::{Bound, Capability, ReadScope, SensorError, SensorHandle};
    use harw_dod_signals::{EventKind, SecurityEvent, Sensor, SensorReading};
    use harw_observe::NullSink;
    use harw_types::SensorId;
    use jiff::{SignedDuration, Timestamp};
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// Ein Sensor, dessen Abrufergebnisse vorab festgelegt sind (FIFO).
    ///
    /// Ist das Skript erschöpft, liefert jeder weitere Abruf
    /// `Ok(SensorReading::default())` — das genügt für Tests, die die exakte
    /// Anzahl der Abrufe selbst zählen (`calls()`) und deshalb wissen, wann
    /// das Skript zu Ende ist.
    #[derive(Debug)]
    struct ScriptedSensor {
        handle: SensorHandle<Bound>,
        script: Mutex<VecDeque<Result<SensorReading, SensorError>>>,
        calls: AtomicUsize,
    }

    impl ScriptedSensor {
        /// Baut den Mock als konkreten `Arc<ScriptedSensor>` — nicht als
        /// `Arc<dyn Sensor>` — damit Testcode nach der Übergabe an
        /// `Sentinel::new` (über [`as_sensor`]) weiterhin `.calls()` auf
        /// demselben `Arc` aufrufen kann. `Sensor` ist nicht `Any`; ein
        /// bereits zu `Arc<dyn Sensor>` entsizigtes Trait-Objekt ließe sich
        /// nicht mehr zurück auf `ScriptedSensor` abbilden.
        fn new(id: &str, script: Vec<Result<SensorReading, SensorError>>) -> Arc<Self> {
            let handle = SensorHandle::new(SensorId::from_str(id), Capability::ReadProcStat)
                .bind(ReadScope::from_roots([PathBuf::from("/proc")]));
            Arc::new(Self {
                handle,
                script: Mutex::new(script.into()),
                calls: AtomicUsize::new(0),
            })
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl Sensor for ScriptedSensor {
        fn handle(&self) -> &SensorHandle<Bound> {
            &self.handle
        }

        fn poll(&self, _now: Timestamp) -> Result<SensorReading, SensorError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            // Ein vergifteter Mutex (Panik eines anderen Testthreads waehrend
            // des Locks) wird hier statt einer weiteren Panik einfach
            // wiederhergestellt (kein Testthread haelt den Lock ueber eine
            // Panik hinweg).
            self.script
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .pop_front()
                .unwrap_or_else(|| Ok(SensorReading::default()))
        }
    }

    /// Entsizigt den konkreten Mock zum Trait-Objekt-`Arc`, den
    /// `Sentinel::new` erwartet, ohne den konkreten `Arc<ScriptedSensor>` im
    /// Aufrufer zu verbrauchen (Rückgabetyp-Koerzion: `Arc<ScriptedSensor>`
    /// -> `Arc<dyn Sensor>`).
    fn as_sensor(mock: &Arc<ScriptedSensor>) -> Arc<dyn Sensor> {
        // Die Zwischenbindung ist noetig: `Arc::clone` leitet sein `T` sonst aus
        // dem Rueckgabetyp ab und erwartet dann `&Arc<dyn Sensor>`. Erst mit
        // festgelegtem `T` greift die Unsizing-Koerzion an der Rueckgabe.
        let cloned: Arc<ScriptedSensor> = Arc::clone(mock);
        cloned
    }

    fn config_with(max_retries: u32, backoff_secs: i64) -> SentinelConfig {
        SentinelConfig::new(
            RetryPolicy::new(max_retries, SignedDuration::from_secs(backoff_secs)),
            16,
            16,
        )
    }

    /// Baut ein extern erzeugtes `SensorDegraded`-Ereignis mit `sensor_id`
    /// als Herkunftskennung — das Muster, das
    /// [`Sentinel::record_external_event`] erwartet.
    fn external_event(sensor_id: &str) -> SecurityEvent {
        let sensor = SensorId::from_str(sensor_id);
        SecurityEvent {
            sensor: sensor.clone(),
            observed_at: Timestamp::UNIX_EPOCH,
            actor: None,
            kind: EventKind::SensorDegraded { sensor },
        }
    }

    #[test]
    fn test_successful_poll_keeps_sensor_bound() {
        let mock = ScriptedSensor::new("mock-0", vec![Ok(SensorReading::default())]);
        let mut sentinel = Sentinel::new(
            vec![as_sensor(&mock)],
            Arc::new(NullSink),
            config_with(5, 0),
        );

        sentinel.poll_all(Timestamp::UNIX_EPOCH);

        let snapshot = sentinel.health_snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].1, SensorHealth::Bound);
    }

    #[test]
    fn test_transient_error_then_success_returns_to_bound() {
        let mock = ScriptedSensor::new(
            "mock-0",
            vec![
                Err(SensorError::MalformedSource),
                Ok(SensorReading::default()),
            ],
        );
        // `backoff = 0` haelt den zweiten Abruf faellig, ohne `now`
        // vorruecken zu muessen.
        let mut sentinel = Sentinel::new(
            vec![as_sensor(&mock)],
            Arc::new(NullSink),
            config_with(5, 0),
        );

        sentinel.poll_all(Timestamp::UNIX_EPOCH);
        assert!(matches!(
            sentinel.health_snapshot()[0].1,
            SensorHealth::Retrying { failures: 1, .. }
        ));

        sentinel.poll_all(Timestamp::UNIX_EPOCH);
        assert_eq!(sentinel.health_snapshot()[0].1, SensorHealth::Bound);
        assert_eq!(mock.calls(), 2);
    }

    #[test]
    fn test_permanent_error_degrades_immediately_without_intermediate_attempts() {
        let mock = ScriptedSensor::new("mock-0", vec![Err(SensorError::OutsideScope)]);
        let mut sentinel = Sentinel::new(
            vec![as_sensor(&mock)],
            Arc::new(NullSink),
            config_with(5, 0),
        );

        sentinel.poll_all(Timestamp::UNIX_EPOCH);

        assert_eq!(
            sentinel.health_snapshot()[0].1,
            SensorHealth::Degraded {
                reason: DegradeReason::Permanent
            }
        );
        assert_eq!(mock.calls(), 1);
    }

    #[test]
    fn test_degraded_transition_emits_sensor_degraded_event_in_reading() -> TestResult {
        let mock = ScriptedSensor::new("mock-0", vec![Err(SensorError::OutsideScope)]);
        let mut sentinel = Sentinel::new(
            vec![as_sensor(&mock)],
            Arc::new(NullSink),
            config_with(5, 0),
        );

        let reading = sentinel.poll_all(Timestamp::UNIX_EPOCH);

        assert_eq!(reading.events.len(), 1);
        let EventKind::SensorDegraded { sensor } = &reading.events[0].kind else {
            return Err(TestError::Unexpected(format!(
                "expected SensorDegraded, got {:?}",
                reading.events[0].kind
            )));
        };
        assert_eq!(sensor.as_str(), "mock-0");
        Ok(())
    }

    #[test]
    fn test_degraded_sensor_is_never_polled_again() {
        let mock = ScriptedSensor::new("mock-0", vec![Err(SensorError::OutsideScope)]);
        let mut sentinel = Sentinel::new(
            vec![as_sensor(&mock)],
            Arc::new(NullSink),
            config_with(5, 0),
        );

        sentinel.poll_all(Timestamp::UNIX_EPOCH);
        sentinel.poll_all(Timestamp::UNIX_EPOCH);
        sentinel.poll_all(Timestamp::UNIX_EPOCH);

        assert_eq!(sentinel.degraded_count(), 1);
        assert_eq!(mock.calls(), 1);
    }

    #[test]
    fn test_retrying_sensor_degrades_after_max_retries_exhausted() {
        let mock = ScriptedSensor::new(
            "mock-0",
            vec![
                Err(SensorError::MalformedSource),
                Err(SensorError::MalformedSource),
            ],
        );
        let mut sentinel = Sentinel::new(
            vec![as_sensor(&mock)],
            Arc::new(NullSink),
            config_with(2, 0),
        );

        let first = sentinel.poll_all(Timestamp::UNIX_EPOCH);
        assert!(first.events.is_empty());
        assert!(matches!(
            sentinel.health_snapshot()[0].1,
            SensorHealth::Retrying { failures: 1, .. }
        ));

        let second = sentinel.poll_all(Timestamp::UNIX_EPOCH);
        assert_eq!(second.events.len(), 1);
        assert_eq!(
            sentinel.health_snapshot()[0].1,
            SensorHealth::Degraded {
                reason: DegradeReason::RetriesExhausted { attempts: 2 }
            }
        );
    }

    #[test]
    fn test_retrying_sensor_is_not_polled_before_next_attempt() -> TestResult {
        let mock = ScriptedSensor::new(
            "mock-0",
            vec![
                Err(SensorError::MalformedSource),
                Ok(SensorReading::default()),
            ],
        );
        let mut sentinel = Sentinel::new(
            vec![as_sensor(&mock)],
            Arc::new(NullSink),
            config_with(5, 30),
        );

        let t0 = Timestamp::UNIX_EPOCH;
        sentinel.poll_all(t0);
        let health = sentinel.health_snapshot()[0].1.clone();
        let SensorHealth::Retrying { next_attempt, .. } = health.clone() else {
            return Err(TestError::Unexpected(format!(
                "expected Retrying state, got {health:?}"
            )));
        };

        // Erneuter Aufruf vor `next_attempt`: der Sensor wird übersprungen,
        // der Zustand bleibt unverändert, kein weiterer Abruf zählt.
        sentinel.poll_all(t0);
        assert_eq!(mock.calls(), 1);
        assert_eq!(
            sentinel.health_snapshot()[0].1,
            SensorHealth::Retrying {
                failures: 1,
                next_attempt
            }
        );

        // Erst ab `next_attempt` wird erneut abgerufen.
        sentinel.poll_all(next_attempt);
        assert_eq!(mock.calls(), 2);
        assert_eq!(sentinel.health_snapshot()[0].1, SensorHealth::Bound);
    }

    #[test]
    fn test_buffer_receives_samples_and_events_from_poll_cycle() {
        let sample = harw_dod_signals::HostSample {
            sensor: SensorId::from_str("mock-0"),
            observed_at: Timestamp::UNIX_EPOCH,
            metric: std::borrow::Cow::Borrowed("cpu_util_percent"),
            value: 1.0,
        };
        let reading = SensorReading {
            samples: vec![sample],
            events: vec![],
        };
        let mock = ScriptedSensor::new("mock-0", vec![Ok(reading)]);
        let mut sentinel = Sentinel::new(
            vec![as_sensor(&mock)],
            Arc::new(NullSink),
            config_with(5, 0),
        );

        sentinel.poll_all(Timestamp::UNIX_EPOCH);

        assert_eq!(sentinel.buffer().sample_len(), 1);
    }

    #[test]
    fn test_two_runs_with_equal_now_and_equal_mock_scripts_are_deterministic() -> TestResult {
        let successful_reading = || SensorReading {
            samples: vec![harw_dod_signals::HostSample {
                sensor: SensorId::from_str("mock-0"),
                observed_at: Timestamp::UNIX_EPOCH,
                metric: std::borrow::Cow::Borrowed("cpu_util_percent"),
                value: 3.5,
            }],
            events: vec![],
        };
        let build = || {
            let mock = ScriptedSensor::new(
                "mock-0",
                vec![Err(SensorError::MalformedSource), Ok(successful_reading())],
            );
            Sentinel::new(
                vec![as_sensor(&mock)],
                Arc::new(NullSink),
                config_with(5, 0),
            )
        };

        let mut first = build();
        let mut second = build();

        let r1a = first.poll_all(Timestamp::UNIX_EPOCH);
        let r2a = second.poll_all(Timestamp::UNIX_EPOCH);
        assert_eq!(r1a, r2a);

        let r1b = first.poll_all(Timestamp::UNIX_EPOCH);
        let r2b = second.poll_all(Timestamp::UNIX_EPOCH);
        assert_eq!(r1b, r2b);

        let e1 = first
            .freeze(Timestamp::UNIX_EPOCH)
            .map_err(ctx("well-formed buffer content always encodes"))?;
        let e2 = second
            .freeze(Timestamp::UNIX_EPOCH)
            .map_err(ctx("well-formed buffer content always encodes"))?;
        assert_eq!(e1, e2);
        Ok(())
    }

    #[test]
    fn test_sentinel_config_default_uses_documented_defaults() {
        let config = SentinelConfig::default();
        assert_eq!(config.retry_policy, RetryPolicy::default());
        assert_eq!(
            config.sample_capacity,
            crate::buffer::EvidenceBuffer::DEFAULT_SAMPLE_CAPACITY
        );
    }

    #[test]
    fn test_record_external_event_appears_in_buffer_and_freeze() -> TestResult {
        let mut sentinel = Sentinel::new(vec![], Arc::new(NullSink), config_with(5, 0));

        sentinel.record_external_event(external_event("probe-fs-0"));

        assert_eq!(sentinel.buffer().event_len(), 1);
        let evidence = sentinel
            .freeze(Timestamp::UNIX_EPOCH)
            .map_err(ctx("well-formed buffer content always encodes"))?;
        assert_eq!(evidence.events.len(), 1);
        let EventKind::SensorDegraded { sensor } = &evidence.events[0].kind else {
            return Err(TestError::Unexpected(format!(
                "expected SensorDegraded, got {:?}",
                evidence.events[0].kind
            )));
        };
        assert_eq!(sensor.as_str(), "probe-fs-0");
        Ok(())
    }

    #[test]
    fn test_record_external_event_interleaves_with_poll_all_events_in_recording_order() {
        // Ein permanenter Fehler degradiert den registrierten Sensor sofort
        // und erzeugt dabei selbst ein `SensorDegraded`-Ereignis (siehe
        // `test_degraded_transition_emits_sensor_degraded_event_in_reading`).
        let mock = ScriptedSensor::new("mock-0", vec![Err(SensorError::OutsideScope)]);
        let mut sentinel = Sentinel::new(
            vec![as_sensor(&mock)],
            Arc::new(NullSink),
            config_with(5, 0),
        );

        sentinel.record_external_event(external_event("probe-fs-0"));
        sentinel.poll_all(Timestamp::UNIX_EPOCH);
        sentinel.record_external_event(external_event("probe-bpf-0"));

        // Extern und per Poll aufgezeichnete Ereignisse stehen nebeneinander
        // in Aufzeichnungsreihenfolge, nicht in getrennten Blöcken.
        let ids: Vec<String> = sentinel
            .buffer()
            .events()
            .map(|event| event.sensor.as_str().to_owned())
            .collect();
        assert_eq!(
            ids,
            vec![
                "probe-fs-0".to_owned(),
                "mock-0".to_owned(),
                "probe-bpf-0".to_owned(),
            ]
        );
    }

    #[test]
    fn test_record_external_event_overflow_matches_poll_all_silent_eviction() {
        // Event-Ringkapazität 1: dasselbe stille FIFO-Verdrängungsverhalten
        // wie `EvidenceBuffer::push_event`, das `poll_all` selbst nutzt
        // (siehe `test_ring_buffer_capacity_bounds_survive_a_full_lifecycle`
        // in `tests/lifecycle.rs` für den analogen Fall über `poll_all`).
        let mut sentinel = Sentinel::new(
            vec![],
            Arc::new(NullSink),
            SentinelConfig::new(RetryPolicy::default(), 4, 1),
        );

        sentinel.record_external_event(external_event("probe-fs-0"));
        sentinel.record_external_event(external_event("probe-fs-1"));

        assert_eq!(sentinel.buffer().event_len(), 1);
        let remaining: Vec<String> = sentinel
            .buffer()
            .events()
            .map(|event| event.sensor.as_str().to_owned())
            .collect();
        assert_eq!(remaining, vec!["probe-fs-1".to_owned()]);
    }

    #[test]
    fn test_record_external_event_does_not_affect_registered_sensor_health_or_degraded_count() {
        let mock = ScriptedSensor::new("mock-0", vec![Ok(SensorReading::default())]);
        let mut sentinel = Sentinel::new(
            vec![as_sensor(&mock)],
            Arc::new(NullSink),
            config_with(5, 0),
        );

        // Trägt absichtlich dieselbe Kennung wie der registrierte Sensor:
        // der Degradationsautomat darf trotzdem unberührt bleiben, weil er
        // ausschließlich `Sensor::poll`-Ergebnisse aus `poll_all` auswertet,
        // nie extern übergebene Ereignisse.
        sentinel.record_external_event(external_event("mock-0"));

        assert_eq!(sentinel.degraded_count(), 0);
        assert_eq!(sentinel.health_snapshot()[0].1, SensorHealth::Bound);
    }

    #[test]
    fn test_record_external_event_carries_a_landlock_shaped_degraded_event_through() -> TestResult {
        // Bildet den Formfall aus `harw_sentinel::sandbox::landlock_degraded_event`
        // nach (ohne von jenem Binary-Crate abhängig zu sein): ein
        // `SensorDegraded`-Ereignis, dessen `sensor`-Feld keinen
        // registrierten Sensor dieser `Sentinel`-Instanz bezeichnet, sondern
        // die Selbstbeschränkung des Binaries selbst.
        let mut sentinel = Sentinel::new(vec![], Arc::new(NullSink), config_with(5, 0));

        sentinel.record_external_event(external_event("landlock-self-restriction-status"));

        assert_eq!(sentinel.buffer().event_len(), 1);
        let recorded = sentinel
            .buffer()
            .events()
            .next()
            .ok_or(TestError::Missing("one event recorded"))?;
        let EventKind::SensorDegraded { sensor } = &recorded.kind else {
            return Err(TestError::Unexpected(format!(
                "expected SensorDegraded, got {:?}",
                recorded.kind
            )));
        };
        assert_eq!(sensor.as_str(), "landlock-self-restriction-status");
        assert_eq!(
            sentinel.degraded_count(),
            0,
            "no registered sensor exists to degrade"
        );
        Ok(())
    }
}
