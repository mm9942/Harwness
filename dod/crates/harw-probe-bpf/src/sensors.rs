//! Sensor-Aufbau: bindet die beiden geerbten Formungscrates gegen einen
//! injizierten `harw_dod_bpf::BpfLoader` — und gleicht dabei ihre
//! unterschiedlichen Schnittstellen aus.
//!
//! # Die geerbte Unstimmigkeit
//! `harw-dod-procmon` und `harw-dod-flow` sind unabhängig voneinander
//! gebaut worden (Invariante C7: Geschwistercrates, die einander nicht
//! kennen dürfen) und haben sich **verschieden** entschieden:
//!
//! | | `harw-dod-procmon` | `harw-dod-flow` |
//! |---|---|---|
//! | Schnittstelle | implementiert `harw_dod_signals::Sensor` (`ProcmonSensor`) | implementiert `Sensor` **bewusst nicht**, bietet `observe()` |
//! | Zeitstempel | `RawBpfEvent::observed_at` je Ereignis, `now` wird angenommen und ignoriert | `RawBpfEvent::observed_at` je Ereignis, kein `now`-Parameter überhaupt |
//! | Ergebnis | `SensorReading` mit `SecurityEvent`s | `Option<SecurityEvent>` nur für Verbindungen **außerhalb** des `NetworkScope` |
//!
//! `harw-dod-flow`s eigene Begründung (siehe dessen `report`-Moduldoku,
//! Abschnitt „Warum `harw-dod-fixtures` hier nicht passt"): ein `Sensor`,
//! der `handle.scope()` nie anfasst, bestünde die sechs
//! `harw_dod_fixtures::sensor_suite!`-Prüfungen trivial — grün, ohne dass
//! die eigentliche Payload-Parse- oder Melderegel-Logik je ausgeführt würde.
//! Also lieber gar nicht erst `Sensor` implementieren, als eine Zusage
//! machen, die eine Fixture-Prüfung nicht ehrlich einlösen kann.
//!
//! # Das Urteil dieser Sonde: `harw-dod-procmon`s Form ist die richtige
//! Diese Crate ist der einzige echte Aufrufer beider Formungscrates — kein
//! anderer Konsument existiert oder ist geplant. Als Konsument wiegt eine
//! erschwerte Ansteuerung schwerer als jedes andere Argument, und hier ist
//! sie eindeutig: [`crate::collect`] treibt `harw-dod-procmon`s
//! `ProcmonSensor` bereits mit einer einzigen, generischen Zeile
//! (`sensor.poll(now)`, iterierbar über `Vec<Arc<dyn Sensor>>`, genau wie
//! `harw-sentinel::sensors::build_sensors` es für seine eigenen Sensoren
//! tut). Für `harw-dod-flow` hätte diese Sonde ohne einen eigenen Adapter
//! stattdessen einen **zweiten, andersartigen** Pfad gebraucht: den
//! `BpfLoader` selbst mit einem eigenen `timeout` ansteuern, das
//! `NetworkScope` selbst durchreichen, `Option<SecurityEvent>` selbst in die
//! Sammlung einsortieren — dieselbe Poll-Semantik, die `Sensor::poll` schon
//! bereitstellt, noch einmal von Hand nachgebaut, nur ohne den gemeinsamen
//! Vertrag. Zwei Geschwistercrates mit verschiedener Schnittstelle bedeuten,
//! dass jeder künftige Konsument zwei Wege lernen muss — und hier gibt es
//! nur diesen einen Konsumenten, auf dessen Rücken beide Wege ohnehin
//! landen.
//!
//! Der von `harw-dod-flow` angeführte Grund (`sensor_suite!` ließe sich
//! austricksen) ist real, aber **bereits gelöst** — von `harw-dod-procmon`
//! selbst: dessen eigene Moduldoku (Abschnitt
//! „`harw_dod_fixtures::sensor_suite!` passt hier nicht") kommt zum
//! *gleichen* Befund (`ProcmonSensor` liest ebenfalls nie über
//! `handle.scope()`) und trifft die *gegenteilige* Konsequenz: `Sensor`
//! trotzdem implementieren, `sensor_suite!` bewusst nicht verwenden, und
//! stattdessen von Hand geschriebene, ehrliche Tests liefern (genau die
//! Tests, die auch `harw-dod-flow` bereits für sich selbst schreibt). Das
//! Fixture-Harness-Problem und die Frage „implementiert diese Crate den
//! Trait" sind zwei verschiedene Entscheidungen; `harw-dod-procmon` löst
//! beide sauber, `harw-dod-flow` löst nur die erste und gibt dafür die
//! zweite auf. Das ist der Punkt, an dem ich als Konsument widerspreche:
//! diese Sonde hätte sich gewünscht, dass `harw-dod-flow` genau wie
//! `harw-dod-procmon` `Sensor` implementiert (mit denselben von Hand
//! geschriebenen, `sensor_suite!`-freien Tests, die es ohnehin schon hat)
//! und `observe()`/`to_security_event()` als seine interne
//! Implementierung dahinter behält, statt den Trait ganz auszulassen.
//!
//! **Fälle kein Urteil „beide sind fein":** siehe oben — genau das wird hier
//! vermieden. `harw-dod-flow`s Entscheidung ist nicht falsch *für sich
//! genommen*, aber sie ist die falsche Entscheidung *für einen Konsumenten*,
//! und diese Sonde ist der einzige.
//!
//! # Was diese Sonde tut, weil sie `harw-dod-flow` nicht ändern darf
//! Diese Datei kann `harw-dod-flow` nicht umschreiben (außerhalb des
//! Schreibbereichs dieses Knotens). Sie baut deshalb stattdessen lokal
//! [`FlowSensor`] — einen dünnen Adapter, der `harw_dod_flow::observe`
//! hinter `harw_dod_signals::Sensor` verpackt, exakt nach dem Vorbild von
//! `harw_dod_procmon::ProcmonSensor` (eigener `Box<dyn
//! harw_dod_bpf::BpfLoader>`, eigener `harw_dod_bpf::BpfHandle`, `now`
//! ignoriert, `RawBpfEvent::observed_at` je Ereignis übernommen,
//! `debug_assert_eq!` auf die erwartete Fähigkeit). Damit treibt
//! [`crate::collect::run_once`]/[`crate::collect::run_forever`] **beide**
//! geerbten Quellen über denselben, einzigen, generischen Pfad — die
//! Unstimmigkeit bleibt in den beiden Formungscrates bestehen (die diese
//! Sonde nicht anfassen darf), wird aber an genau der einen Stelle
//! aufgefangen, an der sie sonst jeden künftigen Aufrufer dieser Sonde
//! getroffen hätte.
//!
//! # Genau eine Fähigkeit
//! Beide Sensoren dieser Sonde tragen `harw_dod_cap::Capability::LoadBpfProgram`
//! (Klasse `harw_dod_cap::CapabilityClass::Bpf`) — dieselbe Fähigkeit, die
//! `harw_dod_bpf::REQUIRED_CAPABILITY`, `harw_dod_procmon`s eigene
//! Erwartung und `harw_dod_flow::REQUIRED_CAPABILITY` benennen. Diese Datei
//! erfindet keine zweite Fähigkeit für den Verbindungs-Sensor.
//!
//! # Der Anknüpfungspunkt des Prozessstart-Programms
//! `harw-dod-flow` exportiert einen festen Anknüpfungspunkt
//! ([`harw_dod_flow::FLOW_TRACEPOINT_ATTACH_POINT`]) und einen Konstruktor
//! ([`harw_dod_flow::flow_program_spec`]). `harw-dod-procmon` tut das
//! **nicht** — es dokumentiert den Tracepoint, an dem sein Format orientiert
//! ist (`sched:sched_process_exec`, siehe dessen `event`-Moduldoku), legt
//! ihn aber nicht als Konstante fest. Diese Sonde muss den Anknüpfungspunkt
//! deshalb selbst wählen: [`PROCMON_TRACEPOINT_ATTACH_POINT`] übernimmt
//! genau den in `harw-dod-procmon`s eigener Dokumentation genannten Wert.
//! Ein `harw_dod_procmon::PROCMON_TRACEPOINT_ATTACH_POINT` plus ein
//! `procmon_program_spec()`-Konstruktor, symmetrisch zu `harw-dod-flow`s
//! eigenen, wären ein kleiner, wünschenswerter Ausgleich gewesen (siehe
//! Abschlussbericht dieses Knotens).
//!
//! # Exportierte Typen
//! [`PROCMON_TRACEPOINT_ATTACH_POINT`], [`FLOW_DEFAULT_READ_TIMEOUT`],
//! [`build_procmon_sensor`], [`build_flow_sensor`], [`FlowSensor`].
//!
//! # Nebenläufigkeit
//! [`FlowSensor`] ist `Send + Sync + Debug` (Anforderung von
//! `harw_dod_signals::Sensor`): `SensorHandle<Bound>`,
//! `Box<dyn harw_dod_bpf::BpfLoader>`, `harw_dod_bpf::BpfHandle`,
//! `harw_sandbox::NetworkScope` und `std::time::Duration` sind alle
//! `Send + Sync`. `poll` nimmt `&self` und führt keine innere
//! Veränderlichkeit — konkurrierende Polls auf demselben Sensor sind sicher
//! (auch wenn der gehaltene Lader selbst innere Veränderlichkeit einsetzen
//! darf, siehe `harw_dod_bpf::fixture::FixtureBpfLoader`).
//!
//! # Fehler
//! [`crate::error::ProbeError::BpfLoad`] aus [`build_procmon_sensor`]/
//! [`build_flow_sensor`], wenn `loader.load(&spec)` scheitert.
//! `harw_dod_cap::SensorError`, wie vom `Sensor`-Trait verlangt, aus
//! [`FlowSensor::poll`] — sowohl `harw_dod_bpf::BpfError` (aus dem
//! gehaltenen Lader) als auch `harw_dod_flow::FlowError` (aus
//! `harw_dod_flow::observe`) werden auf diesen einen Typ abgebildet (private
//! Abbildungsfunktionen in diesem Modul, Muster:
//! `harw_dod_procmon::sensor::bpf_error_to_sensor_error`).
//!
//! # Examples
//! ```rust,ignore
//! use crate::sensors::{build_flow_sensor, build_procmon_sensor};
//! use harw_dod_bpf::BpfProgramSource;
//! use harw_sandbox::NetworkScope;
//! use harw_types::SensorId;
//! use std::borrow::Cow;
//!
//! let source = BpfProgramSource::Embedded(Cow::Borrowed(&[]));
//! let procmon = build_procmon_sensor(
//!     Box::new(harw_dod_bpf::fixture::FixtureBpfLoader::new(Vec::new())),
//!     SensorId::from_str("probe-bpf-procmon-0"),
//!     source.clone(),
//! )?;
//! let flow = build_flow_sensor(
//!     Box::new(harw_dod_bpf::fixture::FixtureBpfLoader::new(Vec::new())),
//!     SensorId::from_str("probe-bpf-flow-0"),
//!     source,
//!     NetworkScope::empty(),
//! )?;
//! # Ok::<(), crate::error::ProbeError>(())
//! ```

use std::time::Duration;

use harw_dod_bpf::{BpfError, BpfHandle, BpfLoader, BpfProgramKind, BpfProgramSource, BpfProgramSpec};
use harw_dod_cap::{Bound, Capability, ReadScope, SensorError, SensorHandle};
use harw_dod_flow::FlowError;
use harw_dod_procmon::ProcmonSensor;
use harw_dod_signals::{Sensor, SensorReading};
use harw_sandbox::NetworkScope;
use harw_types::SensorId;
use jiff::Timestamp;

use crate::error::ProbeError;

/// Anknüpfungspunkt des Prozessstart-Programms, das diese Sonde lädt.
///
/// # Description
/// `harw-dod-procmon` exportiert diesen Wert nicht selbst — siehe
/// Moduldoku, Abschnitt „Der Anknüpfungspunkt des Prozessstart-Programms".
/// Übernommen aus `harw_dod_procmon`s eigener Dokumentation
/// (`event`-Moduldoku: „orientiert am `sched_process_exec`-Tracepoint").
pub const PROCMON_TRACEPOINT_ATTACH_POINT: &str = "sched:sched_process_exec";

/// Vorgabe-Wartezeit für [`FlowSensor::poll`]s `read_events`-Aufruf:
/// **200 Millisekunden**.
///
/// # Description
/// Wie `harw_dod_procmon::DEFAULT_READ_TIMEOUT` ein bewusst gewählter
/// Platzhalter, kein aus einem festen Poll-Abstand abgeleiteter Wert —
/// dieser Workspace legt keinen programmweiten Poll-Abstand fest. Diese
/// Sonde wählt denselben Zahlenwert unabhängig (nicht als Import aus
/// `harw-dod-procmon`, da die beiden Zeitgeber unterschiedliche Quellen
/// bedienen), weil beide Sensoren dieselbe Ringpuffer-Familie lesen und
/// dieselbe Abwägung — kurz genug für einen ungestörten Sammelzyklus, lang
/// genug für kurz zuvor angefallene Ereignisse — gleichermaßen gilt.
pub const FLOW_DEFAULT_READ_TIMEOUT: Duration = Duration::from_millis(200);

/// Baut einen leeren, ungebundenen [`harw_dod_cap::ReadScope`].
///
/// # Description
/// Beide Sensoren dieser Sonde lesen nie über `handle.scope()` — ihre
/// einzige Quelle ist der injizierte `harw_dod_bpf::BpfLoader` (siehe
/// Moduldoku). `harw_dod_cap::SensorHandle::bind` verlangt trotzdem einen
/// `ReadScope`; ein leerer ist dieselbe Wahl, die `harw_dod_procmon`s eigene
/// Beispiele und Tests treffen.
fn empty_scope() -> ReadScope {
    ReadScope::from_roots(Vec::<std::path::PathBuf>::new())
}

/// Baut den Prozessstart-Sensor: lädt das konfigurierte Programm über
/// `loader` und verpackt das Ergebnis in `harw_dod_procmon::ProcmonSensor`
/// (implementiert bereits `harw_dod_signals::Sensor`, siehe Moduldoku).
///
/// # Arguments
/// - `loader` (`Box<dyn harw_dod_bpf::BpfLoader>`): die Ladeschicht dieses
///   Sensors — eigene Instanz, unabhängig vom Lader des Verbindungs-Sensors
///   (`harw_dod_procmon::ProcmonSensor::new` nimmt den Lader als Eigentum
///   entgegen; ein Sensor liest laut `harw_dod_signals::sensor`-Moduldoku
///   ohnehin genau eine Quelle).
/// - `sensor_id` (`harw_types::SensorId`): die Kennung, unter der dieser
///   Sensor seine Ereignisse meldet. Siehe `crate`-Moduldoku, Abschnitt „Das
///   `SensorId`-Schema".
/// - `source` (`harw_dod_bpf::BpfProgramSource`): woher der Programmrumpf
///   kommt.
///
/// # Returns
/// Einen fertig konstruierten `ProcmonSensor`.
///
/// # Errors
/// - [`ProbeError::BpfLoad`]: wenn `loader.load(&spec)` scheitert (z. B.
///   [`harw_dod_bpf::BpfError::CapabilityUnavailable`] auf einem Host ohne
///   `CAP_BPF`).
pub fn build_procmon_sensor(
    loader: Box<dyn BpfLoader>,
    sensor_id: SensorId,
    source: BpfProgramSource,
) -> Result<ProcmonSensor, ProbeError> {
    let spec = BpfProgramSpec::new(
        sensor_id.clone(),
        BpfProgramKind::Tracepoint,
        PROCMON_TRACEPOINT_ATTACH_POINT,
        source,
    );
    let bpf_handle = loader.load(&spec)?;
    let handle = SensorHandle::new(sensor_id, Capability::LoadBpfProgram).bind(empty_scope());
    Ok(ProcmonSensor::new(handle, loader, bpf_handle))
}

/// Baut den Verbindungs-Sensor: lädt das konfigurierte Programm über
/// `loader` und verpackt `harw_dod_flow::observe` hinter dem lokalen
/// [`FlowSensor`]-Adapter (siehe Moduldoku, mein Urteil).
///
/// # Arguments
/// - `loader` (`Box<dyn harw_dod_bpf::BpfLoader>`): die Ladeschicht dieses
///   Sensors — eigene Instanz, siehe [`build_procmon_sensor`].
/// - `sensor_id` (`harw_types::SensorId`): die Kennung, unter der dieser
///   Sensor seine Ereignisse meldet.
/// - `source` (`harw_dod_bpf::BpfProgramSource`): woher der Programmrumpf
///   kommt.
/// - `scope` (`harw_sandbox::NetworkScope`): der Zielbereich, den
///   `harw_dod_flow::observe` gegen jede beobachtete Verbindung prüft.
///
/// # Returns
/// Einen fertig konstruierten [`FlowSensor`].
///
/// # Errors
/// - [`ProbeError::BpfLoad`]: wenn `loader.load(&spec)` scheitert.
pub fn build_flow_sensor(
    loader: Box<dyn BpfLoader>,
    sensor_id: SensorId,
    source: BpfProgramSource,
    scope: NetworkScope,
) -> Result<FlowSensor, ProbeError> {
    let spec = harw_dod_flow::flow_program_spec(sensor_id.clone(), source);
    let bpf_handle = loader.load(&spec)?;
    let handle = SensorHandle::new(sensor_id, Capability::LoadBpfProgram).bind(empty_scope());
    Ok(FlowSensor::new(handle, loader, bpf_handle, scope))
}

/// Lokaler Adapter: verpackt `harw_dod_flow::observe` hinter
/// `harw_dod_signals::Sensor`, damit diese Sonde beide geerbten Quellen über
/// dieselbe, generische Sammelschleife treibt wie
/// `harw_dod_procmon::ProcmonSensor` — siehe Moduldoku, mein Urteil zur
/// Schnittstellen-Unstimmigkeit.
pub struct FlowSensor {
    handle: SensorHandle<Bound>,
    loader: Box<dyn BpfLoader>,
    bpf_handle: BpfHandle,
    scope: NetworkScope,
    timeout: Duration,
}

impl FlowSensor {
    /// Baut einen `FlowSensor` mit [`FLOW_DEFAULT_READ_TIMEOUT`].
    ///
    /// # Arguments
    /// - `handle` (`harw_dod_cap::SensorHandle<harw_dod_cap::Bound>`): der
    ///   gebundene Griff dieses Sensors. Sollte
    ///   `harw_dod_flow::REQUIRED_CAPABILITY` (`Capability::LoadBpfProgram`)
    ///   tragen (siehe [`Self::with_timeout`]).
    /// - `loader` (`Box<dyn harw_dod_bpf::BpfLoader>`): die Ladeschicht,
    ///   über die [`Sensor::poll`] Rohereignisse liest.
    /// - `bpf_handle` (`harw_dod_bpf::BpfHandle`): der Griff eines bereits
    ///   erfolgreich geladenen Programms.
    /// - `scope` (`harw_sandbox::NetworkScope`): die Melderegel-Eingabe für
    ///   `harw_dod_flow::observe`.
    ///
    /// # Returns
    /// Einen `FlowSensor`, dessen [`Sensor::poll`] `loader.read_events` mit
    /// [`FLOW_DEFAULT_READ_TIMEOUT`] aufruft.
    ///
    /// # Panics
    /// In Debug-Builds, wenn
    /// `handle.capability() != harw_dod_flow::REQUIRED_CAPABILITY`. In
    /// Release-Builds keine Prüfung.
    #[must_use]
    pub fn new(
        handle: SensorHandle<Bound>,
        loader: Box<dyn BpfLoader>,
        bpf_handle: BpfHandle,
        scope: NetworkScope,
    ) -> Self {
        Self::with_timeout(handle, loader, bpf_handle, scope, FLOW_DEFAULT_READ_TIMEOUT)
    }

    /// Baut einen `FlowSensor` mit einer eigenen Wartezeit für
    /// `read_events`.
    ///
    /// # Arguments
    /// Siehe [`Self::new`], zusätzlich `timeout`
    /// (`std::time::Duration`): wie lange
    /// `harw_dod_bpf::BpfLoader::read_events` bei jedem Poll auf mindestens
    /// ein Ereignis warten darf.
    ///
    /// # Returns
    /// Einen `FlowSensor`, dessen [`Sensor::poll`] `loader.read_events` mit
    /// `timeout` aufruft.
    ///
    /// # Panics
    /// In Debug-Builds, wenn
    /// `handle.capability() != harw_dod_flow::REQUIRED_CAPABILITY`.
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
            harw_dod_flow::REQUIRED_CAPABILITY,
            "FlowSensor: Griff muss harw_dod_flow::REQUIRED_CAPABILITY (Capability::LoadBpfProgram) tragen"
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
        // Trait-Objekt ohne `Debug`-Zusage, `NetworkScope` trägt
        // Politikinhalt, den eine Debug-Ausgabe nicht wiederholen muss.
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

    /// Liest neu eingetroffene Ringpuffer-Einträge über den gehaltenen
    /// Lader und wendet `harw_dod_flow::observe` (Formung plus Melderegel)
    /// auf jeden davon an.
    ///
    /// # Description
    /// `now` wird nicht ausgewertet — wie bei `ProcmonSensor` trägt bereits
    /// jedes einzelne `harw_dod_bpf::RawBpfEvent::observed_at` den
    /// tatsächlichen Beobachtungszeitpunkt (siehe `harw_dod_flow::report`-
    /// Moduldoku, Abschnitt „Der Zeitstempel").
    ///
    /// # Errors
    /// `harw_dod_cap::SensorError`, entpackt aus `harw_dod_bpf::BpfError`
    /// (wenn der Lader nicht lesbar ist) oder aus `harw_dod_flow::FlowError`
    /// (wenn ein Rohereignis nicht die von `harw-dod-flow` dokumentierte
    /// Form hat).
    fn poll(&self, _now: Timestamp) -> Result<SensorReading, SensorError> {
        let raw_events = self
            .loader
            .read_events(&self.bpf_handle, self.timeout)
            .map_err(bpf_error_to_sensor_error)?;

        let mut events = Vec::with_capacity(raw_events.len());
        for raw in &raw_events {
            let observed = harw_dod_flow::observe(raw, self.handle.id(), &self.scope)
                .map_err(flow_error_to_sensor_error)?;
            if let Some(event) = observed {
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
/// Identische Abbildung wie
/// `harw_dod_procmon::sensor::bpf_error_to_sensor_error`, für denselben
/// Fehlertyp erneut geschrieben, weil diese private Funktion dort nicht
/// exportiert ist: [`BpfError::CapabilityUnavailable`] wird zu
/// `SensorError::SourceUnavailable`, [`BpfError::MalformedEvent`] zu
/// `SensorError::MalformedSource`, [`BpfError::Io`] unverändert zu
/// `SensorError::Io` durchgereicht.
fn bpf_error_to_sensor_error(err: BpfError) -> SensorError {
    // Die Abbildung lebt seit K73 an genau einer Stelle: `impl From<BpfError>
    // for SensorError` in `harw-dod-bpf/src/error.rs`. Diese Funktion bleibt
    // als benannter Aufrufpunkt bestehen, damit die vorhandenen Tests und
    // Aufrufstellen unverändert weiterlesen — sie trägt die Regel nicht mehr.
    SensorError::from(err)
}

/// Entpackt die eine `SensorError`-Variante aus `harw_dod_flow::FlowError`.
///
/// # Description
/// `FlowError` hat heute nur eine Variante, `MalformedEvent`, die auf
/// `SensorError::MalformedSource` abgebildet wird — exhaustiv gematcht,
/// damit eine künftig hinzukommende Variante hier einen Compilefehler
/// erzwingt statt still falsch eingeordnet zu werden.
fn flow_error_to_sensor_error(err: FlowError) -> SensorError {
    match err {
        FlowError::MalformedEvent => SensorError::MalformedSource,
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::net::{IpAddr, Ipv4Addr};

    use harw_dod_bpf::event::RawBpfEvent;
    use harw_dod_bpf::fixture::FixtureBpfLoader;
    use harw_dod_bpf::{BpfError, BpfProgramSource};
    use harw_dod_cap::{Capability, SensorError};
    use harw_dod_signals::{EventKind, Sensor};
    use harw_sandbox::{EgressTarget, NetworkScope};
    use harw_types::SensorId;
    use jiff::Timestamp;

    use super::{
        bpf_error_to_sensor_error, build_flow_sensor, build_procmon_sensor, flow_error_to_sensor_error,
        FlowSensor,
    };
    use crate::error::ProbeError;

    fn placeholder_source() -> BpfProgramSource {
        BpfProgramSource::Embedded(Cow::Borrowed(&[]))
    }

    #[test]
    fn test_build_procmon_sensor_with_capable_fixture_loader_succeeds() {
        let loader = FixtureBpfLoader::new(Vec::new());
        let sensor = build_procmon_sensor(
            Box::new(loader),
            SensorId::from_str("probe-bpf-procmon-0"),
            placeholder_source(),
        )
        .expect("fixture loader with capability always succeeds");
        assert_eq!(sensor.handle().capability(), Capability::LoadBpfProgram);
    }

    #[test]
    fn test_build_procmon_sensor_without_capability_maps_to_bpf_load() {
        let loader = FixtureBpfLoader::without_capability();
        let err = build_procmon_sensor(
            Box::new(loader),
            SensorId::from_str("probe-bpf-procmon-0"),
            placeholder_source(),
        )
        .expect_err("a loader without the bpf capability must fail to load");
        assert!(matches!(
            err,
            ProbeError::BpfLoad(BpfError::CapabilityUnavailable)
        ));
    }

    #[test]
    fn test_build_flow_sensor_with_capable_fixture_loader_succeeds() {
        let loader = FixtureBpfLoader::new(Vec::new());
        let sensor = build_flow_sensor(
            Box::new(loader),
            SensorId::from_str("probe-bpf-flow-0"),
            placeholder_source(),
            NetworkScope::empty(),
        )
        .expect("fixture loader with capability always succeeds");
        assert_eq!(sensor.handle().capability(), Capability::LoadBpfProgram);
    }

    #[test]
    fn test_build_flow_sensor_without_capability_maps_to_bpf_load() {
        let loader = FixtureBpfLoader::without_capability();
        let err = build_flow_sensor(
            Box::new(loader),
            SensorId::from_str("probe-bpf-flow-0"),
            placeholder_source(),
            NetworkScope::empty(),
        )
        .expect_err("a loader without the bpf capability must fail to load");
        assert!(matches!(
            err,
            ProbeError::BpfLoad(BpfError::CapabilityUnavailable)
        ));
    }

    /// Baut einen wohlgeformten, 32 Byte breiten Flow-Payload — dasselbe
    /// Layout wie `harw_dod_flow::event`s eigene Tests.
    fn well_formed_flow_payload(port: u16, addr: [u8; 4]) -> Vec<u8> {
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

    fn flow_raw_event(payload: Vec<u8>) -> RawBpfEvent {
        RawBpfEvent {
            pid: 100,
            comm: "curl".to_owned(),
            observed_at: Timestamp::new(1_700_000_000, 0).expect("valid timestamp"),
            payload,
        }
    }

    #[test]
    fn test_flow_sensor_poll_reports_a_connection_outside_the_allowed_scope() {
        let loader = FixtureBpfLoader::new(vec![flow_raw_event(well_formed_flow_payload(
            443,
            [203, 0, 113, 9],
        ))]);
        let sensor = build_flow_sensor(
            Box::new(loader),
            SensorId::from_str("probe-bpf-flow-0"),
            placeholder_source(),
            NetworkScope::empty(),
        )
        .expect("fixture loader with capability always succeeds");

        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("fixture-backed sensor never fails");
        assert_eq!(reading.events.len(), 1);
        assert_eq!(reading.events[0].sensor, SensorId::from_str("probe-bpf-flow-0"));
        assert!(matches!(reading.events[0].kind, EventKind::EgressFlow { .. }));
    }

    #[test]
    fn test_flow_sensor_poll_does_not_report_a_connection_inside_the_allowed_scope() {
        let loader = FixtureBpfLoader::new(vec![flow_raw_event(well_formed_flow_payload(
            443,
            [10, 0, 0, 5],
        ))]);
        let cidr: ipnet::IpNet = "10.0.0.0/24".parse().expect("valid test CIDR literal");
        let scope = NetworkScope::from_targets([EgressTarget::Cidr(cidr)]);
        let sensor = build_flow_sensor(
            Box::new(loader),
            SensorId::from_str("probe-bpf-flow-0"),
            placeholder_source(),
            scope,
        )
        .expect("fixture loader with capability always succeeds");

        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("fixture-backed sensor never fails");
        assert!(reading.events.is_empty());
    }

    #[test]
    fn test_flow_sensor_poll_maps_malformed_payload_to_malformed_source() {
        let loader = FixtureBpfLoader::new(vec![flow_raw_event(vec![1, 2, 3])]);
        let sensor = build_flow_sensor(
            Box::new(loader),
            SensorId::from_str("probe-bpf-flow-0"),
            placeholder_source(),
            NetworkScope::empty(),
        )
        .expect("fixture loader with capability always succeeds");

        let err = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect_err("a too-short flow payload must fail");
        assert!(matches!(err, SensorError::MalformedSource));
    }

    #[test]
    fn test_flow_sensor_poll_two_independently_built_sensors_yield_identical_readings() {
        let make_loader =
            || FixtureBpfLoader::new(vec![flow_raw_event(well_formed_flow_payload(443, [203, 0, 113, 9]))]);

        let first = build_flow_sensor(
            Box::new(make_loader()),
            SensorId::from_str("probe-bpf-flow-0"),
            placeholder_source(),
            NetworkScope::empty(),
        )
        .expect("fixture loader with capability always succeeds")
        .poll(Timestamp::UNIX_EPOCH)
        .expect("first poll");
        let second = build_flow_sensor(
            Box::new(make_loader()),
            SensorId::from_str("probe-bpf-flow-0"),
            placeholder_source(),
            NetworkScope::empty(),
        )
        .expect("fixture loader with capability always succeeds")
        .poll(Timestamp::UNIX_EPOCH)
        .expect("second poll, independent sensor instance");

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
    fn test_flow_error_to_sensor_error_maps_malformed_event_to_malformed_source() {
        assert!(matches!(
            flow_error_to_sensor_error(harw_dod_flow::FlowError::MalformedEvent),
            SensorError::MalformedSource
        ));
    }

    #[test]
    fn test_flow_sensor_debug_format_does_not_panic() {
        let loader = FixtureBpfLoader::new(Vec::new());
        let sensor: FlowSensor = build_flow_sensor(
            Box::new(loader),
            SensorId::from_str("probe-bpf-flow-0"),
            placeholder_source(),
            NetworkScope::empty(),
        )
        .expect("fixture loader with capability always succeeds");
        let debug = format!("{sensor:?}");
        assert!(debug.contains("FlowSensor"));
    }

    /// Belegt, dass eine egress-Meldung tatsächlich eine sinnvolle Adresse
    /// trägt — nicht nur, dass irgendein Ereignis ankommt.
    #[test]
    fn test_flow_sensor_reported_destination_matches_the_observed_address() {
        let loader = FixtureBpfLoader::new(vec![flow_raw_event(well_formed_flow_payload(
            443,
            [203, 0, 113, 9],
        ))]);
        let sensor = build_flow_sensor(
            Box::new(loader),
            SensorId::from_str("probe-bpf-flow-0"),
            placeholder_source(),
            NetworkScope::empty(),
        )
        .expect("fixture loader with capability always succeeds");

        let reading = sensor.poll(Timestamp::UNIX_EPOCH).expect("fixture-backed sensor never fails");
        match &reading.events[0].kind {
            EventKind::EgressFlow { destination, port } => {
                assert_eq!(destination, &IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9)).to_string());
                assert_eq!(*port, 443);
            }
            other => panic!("expected EventKind::EgressFlow, got {other:?}"),
        }
    }
}
