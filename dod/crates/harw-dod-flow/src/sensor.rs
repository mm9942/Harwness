//! `FlowSensor`: die Brücke zwischen einem injizierten
//! `harw_dod_bpf::BpfLoader` und `harw_dod_signals::Sensor` (K52).
//!
//! # Warum diese Datei jetzt existiert
//! `harw-dod-procmon` implementiert `harw_dod_signals::Sensor`, diese Crate
//! tat es bislang bewusst nicht — beide Entscheidungen entstanden unabhängig
//! aus derselben Beobachtung (siehe unten), zogen aber die entgegengesetzte
//! Konsequenz. `harw-probe-bpf`, der einzige echte Konsument beider Crates,
//! hat sich für procmons Form entschieden: er sammelt `Arc<dyn Sensor>` und
//! ruft `poll(now)` einheitlich auf. Diese Datei zieht flows richtige
//! Beobachtung — `sensor_suite!` bestünde hier trivial — zur richtigen
//! Konsequenz nach: `Sensor` implementieren, `sensor_suite!` **weiterhin**
//! nicht benutzen, und die Parselogik ehrlich von Hand prüfen.
//!
//! # Warum `harw_dod_fixtures::sensor_suite!` hier weiterhin nicht passt
//! `sensor_suite!` verlangt einen Sensor, der über
//! `harw_dod_cap::SensorHandle<harw_dod_cap::Bound>::scope()` liest, und
//! führt seine sechs Prüfungen (Determinismus, Inhaltsfreiheit,
//! Scope-Dichtheit, Redaktion, Kardinalität, Fehlerfall) gegen einen echten
//! `fixtures/<fall>/tree`-Verzeichnisbaum aus, den es dafür anlegt.
//! [`FlowSensor`] liest **nie** über `handle.scope()` — seine einzige Quelle
//! ist der injizierte `Box<dyn harw_dod_bpf::BpfLoader>`. Ein Sensor, der
//! `handle.scope()` nie anfasst, bestünde jede der sechs Prüfungen
//! **trivial**: grün, weil an einer Stelle geprüft wird, die dieser Sensor
//! gar nicht benutzt — die eigentliche Payload-Parse- und Melderegel-Logik
//! ([`crate::event::parse_flow_payload`], [`crate::report::to_security_event`])
//! würde dabei kein einziges Mal ausgeführt. Genau das war K41s Befund bei
//! `harw-dod-authlog` und ist hier identisch. Die Tests in diesem Modul sind
//! deshalb von Hand geschrieben, laufen über
//! `harw_dod_bpf::fixture::FixtureBpfLoader` und führen die Parse- und
//! Melderegel-Logik tatsächlich aus, statt sie nur zu behaupten.
//!
//! # `observe()` bleibt — es ist nicht dieselbe Sache unter zwei Namen
//! [`crate::report::observe`] deutet **ein bereits gelesenes** `RawBpfEvent`
//! und wendet die Melderegel darauf an; es kennt keinen Lader, keinen
//! Zeitschritt, keine Sammlung. [`FlowSensor::poll`] kann das nicht
//! ausdrücken: es muss zusätzlich einen `Box<dyn harw_dod_bpf::BpfLoader>`
//! befragen, **mehrere** in einem Zyklus angefallene Rohereignisse einsammeln
//! und Lade-/Formfehler auf `harw_dod_cap::SensorError` abbilden — eine
//! andere Flughöhe, kein Alias. [`FlowSensor::poll`] ruft deshalb
//! [`crate::report::observe`] **einmal je gelesenem Rohereignis** auf: es ist
//! der Baustein, nicht die verdoppelte Fassade. Ein Aufrufer, der bereits ein
//! einzelnes `RawBpfEvent` in der Hand hält (etwa in einem Test oder einer
//! Offline-Auswertung eines aufgezeichneten Ereignisses) und keinen
//! `BpfLoader` konstruieren will, bleibt weiterhin an `observe()` direkt
//! adressierbar — dafür ist es öffentlich geblieben.
//!
//! # Welcher Zeitstempel gilt
//! Wie `harw-dod-procmon::sensor` wertet [`FlowSensor::poll`] das injizierte
//! `now` **nicht** aus: [`crate::report::observe`] verwendet bereits
//! `harw_dod_bpf::RawBpfEvent::observed_at` pro Ereignis (siehe
//! [`crate::report`]-Moduldoku, Abschnitt „Der Zeitstempel"). `now` bleibt
//! Teil der Signatur, weil der `Sensor`-Vertrag sie verlangt.
//!
//! # Genau eine Fähigkeit
//! [`crate::REQUIRED_CAPABILITY`] (`harw_dod_cap::Capability::LoadBpfProgram`,
//! Klasse `harw_dod_cap::CapabilityClass::Bpf`). [`FlowSensor::with_timeout`]
//! prüft das in Debug-Builds mit `debug_assert_eq!` gegen den übergebenen
//! Griff — dasselbe Muster wie `harw-dod-procmon::ProcmonSensor::with_timeout`
//! und `harw-dod-authlog::AuthlogSensor`.
//!
//! # Wer lädt das Programm, wer bindet den Scope?
//! Diese Crate lädt kein Programm selbst: der Aufrufer ruft
//! `loader.load(&spec)` auf (`spec` z. B. aus [`crate::flow_program_spec`])
//! und übergibt Lader, den entstandenen `harw_dod_bpf::BpfHandle` sowie den
//! für diesen Host autorisierten `harw_authority::NetworkScope` an
//! [`FlowSensor::new`]. Konstruktion bleibt dadurch unfehlbar (kein `Result`
//! nötig).
//!
//! # Der Inhalt bleibt heikel
//! `SensorReading::samples` bleibt leer — ein Verbindungsereignis ist ein
//! Ereignis, kein Messwert. Jedes gemeldete `SecurityEvent` durchläuft
//! [`crate::report::to_security_event`] und trägt ausschließlich
//! `EventKind::EgressFlow { destination, port }` samt `Actor { uid, .. }` —
//! siehe [`crate::report`]-Moduldoku für die vollständige Melderegel und die
//! Begründung, warum keine Nutzdaten je in ein Feld gelangen.
//!
//! # Exportierte Typen
//! [`FlowSensor`], [`DEFAULT_READ_TIMEOUT`].
//!
//! # Nebenläufigkeit
//! `FlowSensor: Send + Sync + Debug` (Anforderung von
//! `harw_dod_signals::Sensor`): `SensorHandle<Bound>`,
//! `Box<dyn harw_dod_bpf::BpfLoader>`, `harw_dod_bpf::BpfHandle`,
//! `harw_authority::NetworkScope` und `std::time::Duration` sind alle
//! `Send + Sync`. [`FlowSensor::poll`] nimmt `&self` und führt keine innere
//! Veränderlichkeit — konkurrierende Polls auf demselben Sensor sind sicher.
//!
//! # Fehler
//! `harw_dod_cap::SensorError`, wie vom `Sensor`-Vertrag verlangt — sowohl
//! `harw_dod_bpf::BpfError` (aus dem gehaltenen Lader; alle sechs Varianten
//! werden einzeln behandelt, siehe [`bpf_error_to_sensor_error`] für die
//! vollständige Zuordnung und ihre `permanence()`-Begründung) als auch
//! [`crate::error::FlowError`] (aus [`crate::report::observe`]) werden auf
//! diesen einen Typ abgebildet (private Abbildungsfunktionen in diesem
//! Modul, Muster: `harw-dod-procmon::sensor`).
//!
//! # Examples
//! ```rust
//! use harw_dod_bpf::fixture::FixtureBpfLoader;
//! use harw_dod_bpf::{BpfLoader, BpfProgramSource};
//! use harw_dod_cap::{Capability, ReadScope, SensorHandle};
//! use harw_dod_flow::FlowSensor;
//! use harw_dod_flow::flow_program_spec;
//! use harw_dod_signals::Sensor;
//! use harw_authority::NetworkScope;
//! use harw_types::SensorId;
//! use std::borrow::Cow;
//!
//! let handle = SensorHandle::new(SensorId::from_str("flow-0"), Capability::LoadBpfProgram)
//!     .bind(ReadScope::from_roots(Vec::<std::path::PathBuf>::new()));
//!
//! let loader = FixtureBpfLoader::new(Vec::new());
//! let spec = flow_program_spec(
//!     SensorId::from_str("flow-0"),
//!     BpfProgramSource::Embedded(Cow::Borrowed(b"\0asm".as_slice())),
//! );
//! let bpf_handle = loader.load(&spec).expect("fixture loader with capability always succeeds");
//!
//! let sensor = FlowSensor::new(handle, Box::new(loader), bpf_handle, NetworkScope::empty());
//! let reading = sensor.poll(jiff::Timestamp::UNIX_EPOCH).expect("fixture-backed sensor never fails");
//! assert!(reading.events.is_empty());
//! ```

use std::time::Duration;

use harw_dod_bpf::{BpfError, BpfHandle, BpfLoader};
use harw_dod_cap::{Bound, SensorError, SensorHandle};
use harw_dod_signals::{Sensor, SensorReading};
use harw_authority::NetworkScope;
use jiff::Timestamp;

use crate::error::FlowError;
use crate::report::observe;

/// Voreingestellte Wartezeit für [`FlowSensor::new`]: **200 Millisekunden**.
///
/// # Beschreibung
/// Wie `harw-dod-procmon::DEFAULT_READ_TIMEOUT` ein bewusst gewählter
/// Platzhalter, kein aus einem festen Poll-Abstand abgeleiteter Wert — dieser
/// Workspace legt keinen programmweiten Poll-Abstand fest. Ein Aufrufer mit
/// anderen Anforderungen verwendet [`FlowSensor::with_timeout`].
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_millis(200);

/// Ein Sensor für ausgehende Verbindungen außerhalb eines autorisierten
/// Bereichs, über einen injizierten [`harw_dod_bpf::BpfLoader`].
///
/// # Description
/// Hält einen gebundenen Griff, einen Lader, einen bereits geladenen
/// [`harw_dod_bpf::BpfHandle`] und den für diesen Host autorisierten
/// [`harw_authority::NetworkScope`] als unabhängige, bei der Konstruktion
/// übergebene Werte. Siehe Moduldoku für die Fähigkeits-Invarianz zwischen
/// Griff und Lader, für die `observe()`-Beziehung und für die
/// `sensor_suite!`-Ausnahme.
pub struct FlowSensor {
    handle: SensorHandle<Bound>,
    loader: Box<dyn BpfLoader>,
    bpf_handle: BpfHandle,
    scope: NetworkScope,
    timeout: Duration,
}

impl FlowSensor {
    /// Baut einen Sensor mit [`DEFAULT_READ_TIMEOUT`].
    ///
    /// # Arguments
    /// - `handle` (`harw_dod_cap::SensorHandle<harw_dod_cap::Bound>`): der
    ///   gebundene Griff dieses Sensors. Sollte [`crate::REQUIRED_CAPABILITY`]
    ///   tragen (siehe Moduldoku).
    /// - `loader` (`Box<dyn harw_dod_bpf::BpfLoader>`): die Ladeschicht, über
    ///   die [`Sensor::poll`] Rohereignisse liest.
    /// - `bpf_handle` (`harw_dod_bpf::BpfHandle`): der Griff eines bereits
    ///   erfolgreich geladenen Programms (aus einem vorherigen
    ///   `loader.load(&spec)`-Aufruf, z. B. mit [`crate::flow_program_spec`]).
    /// - `scope` (`harw_authority::NetworkScope`): der für diesen Host
    ///   autorisierte Zielbereich, an [`crate::report::observe`]
    ///   durchgereicht.
    ///
    /// # Returns
    /// Einen `FlowSensor`, dessen [`Sensor::poll`] `loader.read_events` mit
    /// [`DEFAULT_READ_TIMEOUT`] aufruft.
    ///
    /// # Panics
    /// In Debug-Builds, wenn `handle.capability() != crate::REQUIRED_CAPABILITY`
    /// (siehe Moduldoku). In Release-Builds keine Prüfung.
    ///
    /// # Examples
    /// Siehe Moduldoku.
    #[must_use]
    pub fn new(
        handle: SensorHandle<Bound>,
        loader: Box<dyn BpfLoader>,
        bpf_handle: BpfHandle,
        scope: NetworkScope,
    ) -> Self {
        Self::with_timeout(handle, loader, bpf_handle, scope, DEFAULT_READ_TIMEOUT)
    }

    /// Baut einen Sensor mit einer eigenen Wartezeit für `read_events`.
    ///
    /// # Arguments
    /// - `handle` (`harw_dod_cap::SensorHandle<harw_dod_cap::Bound>`): der
    ///   gebundene Griff dieses Sensors.
    /// - `loader` (`Box<dyn harw_dod_bpf::BpfLoader>`): die Ladeschicht.
    /// - `bpf_handle` (`harw_dod_bpf::BpfHandle`): der Griff des geladenen
    ///   Programms.
    /// - `scope` (`harw_authority::NetworkScope`): der autorisierte
    ///   Zielbereich.
    /// - `timeout` (`std::time::Duration`): wie lange
    ///   [`harw_dod_bpf::BpfLoader::read_events`] bei jedem Poll auf
    ///   mindestens ein Ereignis warten darf.
    ///
    /// # Returns
    /// Einen `FlowSensor`, dessen [`Sensor::poll`] `loader.read_events` mit
    /// `timeout` aufruft.
    ///
    /// # Panics
    /// In Debug-Builds, wenn `handle.capability() != crate::REQUIRED_CAPABILITY`.
    /// In Release-Builds keine Prüfung.
    #[must_use]
    pub fn with_timeout(
        handle: SensorHandle<Bound>,
        loader: Box<dyn BpfLoader>,
        bpf_handle: BpfHandle,
        scope: NetworkScope,
        timeout: Duration,
    ) -> Self {
        debug_assert_eq!(
            handle.capability(),
            crate::REQUIRED_CAPABILITY,
            "FlowSensor: Griff muss crate::REQUIRED_CAPABILITY (Capability::LoadBpfProgram) tragen"
        );
        Self {
            handle,
            loader,
            bpf_handle,
            scope,
            timeout,
        }
    }
}

impl std::fmt::Debug for FlowSensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Zeigt Griff, geladenen Griff und Wartezeit — der Lader ist ein
        // Trait-Objekt ohne `Debug`-Zusage; `NetworkScope` bleibt ebenfalls
        // ungezeigt, um keine Zielbereichs-Details in Logs zu spiegeln, die
        // über `Debug` mitgeschnitten werden.
        f.debug_struct("FlowSensor")
            .field("handle", &self.handle)
            .field("bpf_handle", &self.bpf_handle)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl Sensor for FlowSensor {
    /// Der gebundene Griff dieses Sensors.
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    /// Liest neu eingetroffene Ringpuffer-Einträge über den gehaltenen Lader
    /// und wendet [`crate::report::observe`] auf jedes einzelne an — Parsen
    /// und Melderegel in einem Schritt je Rohereignis.
    ///
    /// # Description
    /// `now` wird nicht ausgewertet — siehe Moduldoku, Abschnitt „Welcher
    /// Zeitstempel gilt". Jedes gemeldete `SecurityEvent::observed_at` ist
    /// das `observed_at` des jeweiligen `harw_dod_bpf::RawBpfEvent`.
    ///
    /// # Errors
    /// `harw_dod_cap::SensorError`, entpackt aus `harw_dod_bpf::BpfError`
    /// (wenn der Lader nicht lesbar ist oder ein Ringpuffer-Eintrag nicht die
    /// erwartete Kopf-Form hat) oder aus [`crate::error::FlowError`] (wenn
    /// ein `payload` nicht die in [`crate::event`] dokumentierte Form hat).
    fn poll(&self, _now: Timestamp) -> Result<SensorReading, SensorError> {
        let raw_events = self
            .loader
            .read_events(&self.bpf_handle, self.timeout)
            .map_err(bpf_error_to_sensor_error)?;

        let mut events = Vec::with_capacity(raw_events.len());
        for raw in &raw_events {
            if let Some(event) =
                observe(raw, self.handle.id(), &self.scope).map_err(flow_error_to_sensor_error)?
            {
                events.push(event);
            }
        }

        Ok(SensorReading {
            samples: Vec::new(),
            events,
        })
    }
}

/// Entpackt die passende `SensorError`-Variante aus `harw_dod_bpf::BpfError`.
///
/// # Description
/// Beide Fehlertypen sind bereits inhaltsfrei; diese Abbildung überträgt nur
/// die Fallunterscheidung, keinen Inhalt — identisches Muster zu
/// `harw-dod-procmon::sensor::bpf_error_to_sensor_error`, mit derselben
/// Begründung je Variante. Bewusst **kein** `_ =>`-Sammelzweig: eine künftige
/// `BpfError`-Variante soll den Compiler stoppen, nicht still hier
/// mitlaufen.
///
/// Zuordnung, mit `harw_dod_cap::SensorError::permanence()` als Maßstab:
/// - [`BpfError::CapabilityUnavailable`] → `SensorError::SourceUnavailable`
///   (`Permanent`): der Host bietet die Lade-Fähigkeit nicht an.
/// - [`BpfError::MalformedEvent`] → `SensorError::MalformedSource`
///   (`Transient`): ein einzelner Ringpuffer-Eintrag hat nicht die erwartete
///   Form, der nächste könnte es wieder haben.
/// - [`BpfError::Io`] → `SensorError::Io` (`Transient`), unverändert
///   durchgereicht.
/// - [`BpfError::ProgramLoadFailed`] → `SensorError::SourceUnavailable`
///   (`Permanent`): entsteht ausschließlich in `BpfLoader::load`, das
///   [`FlowSensor`] nie selbst aufruft (siehe Moduldoku „Wer lädt das
///   Programm, wer bindet den Scope?" — der Aufrufer übergibt einen bereits
///   geladenen `BpfHandle`). Ein fehlgeschlagenes Laden ändert sich nicht
///   dadurch, dass derselbe Griff später erneut abgefragt wird.
/// - [`BpfError::UnsupportedProgramKind`] → `SensorError::SourceUnavailable`
///   (`Permanent`): ein vom realen Ladeteil nicht unterstützter
///   Programmtyp bleibt es auch beim nächsten Versuch.
/// - [`BpfError::UnknownHandle`] → `SensorError::SourceUnavailable`
///   (`Permanent`): [`FlowSensor`] hält genau einen unveränderlichen
///   `BpfHandle` über seine gesamte Lebensdauer; kennt der reale Ladeteil ihn
///   einmal nicht mehr, erzeugt derselbe Griff bei jedem künftigen
///   [`FlowSensor::poll`] denselben Fehler — `Permanent`, nicht `Transient`.
fn bpf_error_to_sensor_error(err: BpfError) -> SensorError {
    // Die Abbildung lebt seit K73 an genau einer Stelle: `impl From<BpfError>
    // for SensorError` in `harw-dod-bpf/src/error.rs`. Diese Funktion bleibt
    // als benannter Aufrufpunkt bestehen, damit die vorhandenen Tests und
    // Aufrufstellen unverändert weiterlesen — sie trägt die Regel nicht mehr.
    SensorError::from(err)
}

/// Entpackt die eine `SensorError`-Variante aus [`FlowError`].
///
/// # Description
/// [`FlowError`] hat heute nur eine Variante, [`FlowError::MalformedEvent`],
/// die auf `SensorError::MalformedSource` abgebildet wird.
fn flow_error_to_sensor_error(err: FlowError) -> SensorError {
    match err {
        FlowError::MalformedEvent => SensorError::MalformedSource,
    }
}

#[cfg(test)]
mod tests {
    use std::net::IpAddr;

    use harw_dod_bpf::event::RawBpfEvent;
    use harw_dod_bpf::fixture::FixtureBpfLoader;
    use harw_dod_bpf::{BpfError, BpfLoader, BpfProgramKind, BpfProgramSource, BpfProgramSpec};
    use harw_dod_cap::{Bound, Capability, ReadScope, SensorHandle, SensorError};
    use harw_dod_signals::{EventKind, Sensor};
    use harw_authority::{EgressTarget, NetworkScope};
    use harw_types::SensorId;
    use jiff::Timestamp;
    use std::borrow::Cow;

    use super::{bpf_error_to_sensor_error, flow_error_to_sensor_error, FlowSensor};
    use crate::error::FlowError;
    use crate::report::observe;

    fn handle_with(capability: Capability) -> SensorHandle<Bound> {
        SensorHandle::new(SensorId::from_str("flow-test"), capability)
            .bind(ReadScope::from_roots(Vec::<std::path::PathBuf>::new()))
    }

    fn sample_spec() -> BpfProgramSpec {
        BpfProgramSpec::new(
            SensorId::from_str("flow-test"),
            BpfProgramKind::Tracepoint,
            "sock:inet_sock_set_state",
            BpfProgramSource::Embedded(Cow::Borrowed(b"\0asm".as_slice())),
        )
    }

    fn scope_allowing_10_0_0_0_24() -> NetworkScope {
        let cidr: ipnet::IpNet = "10.0.0.0/24".parse().expect("valid test CIDR literal");
        NetworkScope::from_targets([EgressTarget::Cidr(cidr)])
    }

    /// Baut ein wohlgeformtes 32-Byte-Flow-`payload`, wie
    /// `crate::event::parse_flow_payload` es erwartet.
    fn flow_payload(port: u16, addr: [u8; 4]) -> Vec<u8> {
        let mut bytes = vec![0u8; 32];
        bytes[0..4].copy_from_slice(&100u32.to_le_bytes()); // pid
        bytes[4..8].copy_from_slice(&1_000u32.to_le_bytes()); // uid
        bytes[8] = 0; // TCP
        bytes[9] = 1; // ausgehend
        bytes[10] = 0; // IPv4
        bytes[12..14].copy_from_slice(&port.to_be_bytes());
        bytes[16..20].copy_from_slice(&addr);
        bytes
    }

    fn raw_event(observed_at: Timestamp, payload: Vec<u8>) -> RawBpfEvent {
        RawBpfEvent {
            pid: 100,
            comm: "curl".to_owned(),
            observed_at,
            payload,
        }
    }

    fn build_sensor(events: Vec<RawBpfEvent>, scope: NetworkScope) -> FlowSensor {
        let loader = FixtureBpfLoader::new(events);
        let bpf_handle = loader
            .load(&sample_spec())
            .expect("fixture loader with capability always succeeds");
        FlowSensor::new(handle_with(Capability::LoadBpfProgram), Box::new(loader), bpf_handle, scope)
    }

    #[test]
    fn test_handle_returns_bound_handle_with_load_bpf_program_capability() {
        let sensor = build_sensor(Vec::new(), NetworkScope::empty());
        assert_eq!(sensor.handle().capability(), Capability::LoadBpfProgram);
    }

    #[test]
    fn test_poll_with_no_events_returns_empty_reading() {
        let sensor = build_sensor(Vec::new(), NetworkScope::empty());
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("fixture-backed sensor never fails");
        assert!(reading.samples.is_empty());
        assert!(reading.events.is_empty());
    }

    #[test]
    fn test_poll_yields_the_same_event_that_observe_would_yield_for_the_same_raw_event() {
        // Der wichtigste Test dieser Datei: `poll()` über einen `FlowSensor`
        // muss dasselbe Ergebnis liefern wie ein direkter `observe()`-Aufruf
        // auf demselben Rohereignis — die Parselogik läuft dabei tatsächlich,
        // nicht nur behauptet.
        let observed_at = Timestamp::new(1_700_000_000, 0).expect("gültiger Zeitstempel");
        let payload = flow_payload(443, [203, 0, 113, 9]);
        let raw = raw_event(observed_at, payload);
        let scope = scope_allowing_10_0_0_0_24();
        let sensor_id = SensorId::from_str("flow-test");

        let expected = observe(&raw, &sensor_id, &scope)
            .expect("well-formed payload must parse")
            .expect("destination outside scope must be reported");

        let sensor = build_sensor(vec![raw], scope);
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("fixture-backed sensor never fails");

        assert_eq!(reading.events, vec![expected]);
    }

    #[test]
    fn test_poll_actually_parses_the_payload_and_applies_the_report_rule() {
        // Belegt, dass die Parse- und Melderegel-Logik tatsächlich ausgeführt
        // wird (nicht nur trivial über einen `sensor_suite!`-Pfad besteht,
        // der hier nie greifen würde): eine Verbindung innerhalb des
        // erlaubten Bereichs erzeugt kein Ereignis.
        let payload = flow_payload(80, [10, 0, 0, 5]);
        let sensor = build_sensor(vec![raw_event(Timestamp::UNIX_EPOCH, payload)], scope_allowing_10_0_0_0_24());

        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("fixture-backed sensor never fails");
        assert!(reading.events.is_empty());
    }

    #[test]
    fn test_poll_reports_egress_flow_kind_with_destination_and_port() {
        let payload = flow_payload(8_443, [198, 51, 100, 7]);
        let sensor = build_sensor(
            vec![raw_event(Timestamp::UNIX_EPOCH, payload)],
            NetworkScope::empty(),
        );

        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("fixture-backed sensor never fails");
        assert_eq!(reading.events.len(), 1);

        match &reading.events[0].kind {
            EventKind::EgressFlow { destination, port } => {
                assert_eq!(destination, "198.51.100.7");
                assert_eq!(*port, 8_443);
                assert_eq!(destination.parse::<IpAddr>().expect("valid ip"), IpAddr::from([198, 51, 100, 7]));
            }
            other => panic!("expected EventKind::EgressFlow, got {other:?}"),
        }
    }

    #[test]
    fn test_poll_propagates_malformed_payload_as_malformed_source() {
        let sensor = build_sensor(
            vec![raw_event(Timestamp::UNIX_EPOCH, vec![1, 2, 3])],
            NetworkScope::empty(),
        );
        let err = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect_err("zu kurzes payload muss scheitern");
        assert!(matches!(err, SensorError::MalformedSource));
    }

    #[test]
    fn test_poll_two_independently_built_sensors_with_same_now_yield_identical_readings() {
        // Wie `harw-dod-procmon`: `FixtureBpfLoader::read_events` entleert
        // seine Warteschlange, ein zweiter Poll auf derselben Instanz ist
        // deshalb bewusst nicht idempotent. Determinismus bedeutet hier: zwei
        // unabhängig konstruierte Sensoren mit identischem Ereignisinhalt und
        // identischem Scope liefern bei gleichem `now` dasselbe Ergebnis.
        let observed_at = Timestamp::new(1_000, 0).expect("gültiger Zeitstempel");
        let make_events = || vec![raw_event(observed_at, flow_payload(443, [203, 0, 113, 9]))];

        let first = build_sensor(make_events(), scope_allowing_10_0_0_0_24())
            .poll(Timestamp::UNIX_EPOCH)
            .expect("erster Poll");
        let second = build_sensor(make_events(), scope_allowing_10_0_0_0_24())
            .poll(Timestamp::UNIX_EPOCH)
            .expect("zweiter Poll, unabhängige Sensor-Instanz");

        assert_eq!(first, second);
    }

    #[test]
    fn test_bpf_error_to_sensor_error_maps_capability_unavailable_to_source_unavailable() {
        assert!(matches!(
            bpf_error_to_sensor_error(BpfError::CapabilityUnavailable),
            SensorError::SourceUnavailable
        ));
    }

    #[test]
    fn test_bpf_error_to_sensor_error_maps_malformed_event_to_malformed_source() {
        assert!(matches!(
            bpf_error_to_sensor_error(BpfError::MalformedEvent),
            SensorError::MalformedSource
        ));
    }

    #[test]
    fn test_bpf_error_to_sensor_error_maps_io_to_io() {
        let source = std::io::Error::other("boom");
        assert!(matches!(bpf_error_to_sensor_error(BpfError::Io(source)), SensorError::Io(_)));
    }

    #[test]
    fn test_bpf_error_to_sensor_error_maps_program_load_failed_to_source_unavailable() {
        assert!(matches!(
            bpf_error_to_sensor_error(BpfError::ProgramLoadFailed),
            SensorError::SourceUnavailable
        ));
    }

    #[test]
    fn test_bpf_error_to_sensor_error_maps_unsupported_program_kind_to_source_unavailable() {
        assert!(matches!(
            bpf_error_to_sensor_error(BpfError::UnsupportedProgramKind),
            SensorError::SourceUnavailable
        ));
    }

    #[test]
    fn test_bpf_error_to_sensor_error_maps_unknown_handle_to_source_unavailable() {
        assert!(matches!(
            bpf_error_to_sensor_error(BpfError::UnknownHandle),
            SensorError::SourceUnavailable
        ));
    }

    #[test]
    fn test_program_load_failed_and_unsupported_program_kind_and_unknown_handle_are_permanent() {
        // Alle drei sind laut `bpf_error_to_sensor_error`-Doku `Permanent`
        // eingestuft — dieser Test belegt das über das tatsächlich
        // resultierende `SensorError`, nicht nur in Prosa.
        for err in [
            BpfError::ProgramLoadFailed,
            BpfError::UnsupportedProgramKind,
            BpfError::UnknownHandle,
        ] {
            assert_eq!(
                bpf_error_to_sensor_error(err).permanence(),
                harw_dod_cap::error::Permanence::Permanent
            );
        }
    }

    #[test]
    fn test_flow_error_to_sensor_error_maps_malformed_event_to_malformed_source() {
        assert!(matches!(
            flow_error_to_sensor_error(FlowError::MalformedEvent),
            SensorError::MalformedSource
        ));
    }
}
