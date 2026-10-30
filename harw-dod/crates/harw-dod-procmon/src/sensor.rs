//! `ProcmonSensor`: die Brücke zwischen einem injizierten
//! `harw_dod_bpf::BpfLoader` und `harw_dod_signals::Sensor`.
//!
//! # Warum diese Datei existiert
//! `harw-dod-sentinel` sammelt ausschließlich `Box<dyn harw_dod_signals::Sensor>`
//! (bzw. `Arc<dyn Sensor>`). Ein `harw_dod_bpf::BpfLoader` allein wäre von
//! dieser Sammelstelle nie erreichbar. [`ProcmonSensor`] schließt die Lücke:
//! er hält einen geladenen [`harw_dod_bpf::BpfHandle`] und einen
//! `Box<dyn harw_dod_bpf::BpfLoader>` und implementiert `Sensor` darüber.
//!
//! # Genau eine Fähigkeit: `Capability::LoadBpfProgram`
//! Diese Crate trägt **eine** Fähigkeit,
//! `harw_dod_bpf::REQUIRED_CAPABILITY` (`harw_dod_cap::Capability::LoadBpfProgram`,
//! Klasse `harw_dod_cap::CapabilityClass::Bpf`). [`ProcmonSensor::with_timeout`]
//! prüft das in Debug-Builds mit `debug_assert_eq!` gegen den übergebenen
//! Griff — dasselbe Muster wie `harw-dod-authlog::AuthlogSensor` für die
//! Übereinstimmung von Griff und Backend. In Release-Builds entfällt die
//! Prüfung; der Aufrufer bleibt dokumentiert dafür verantwortlich, einen
//! Griff mit dieser Fähigkeit zu übergeben.
//!
//! # Wer lädt das BPF-Programm?
//! Diese Crate lädt kein Programm selbst: der Aufrufer ruft
//! `loader.load(&spec)` auf und übergibt sowohl den Lader als auch den
//! entstandenen [`harw_dod_bpf::BpfHandle`] an [`ProcmonSensor::new`]. Das
//! hält die Konstruktion dieses Sensors unfehlbar (kein `Result` nötig) und
//! trennt „ein Programm laden" (eine `harw-dod-bpf`-Aufgabe, potenziell
//! fehlschlagend mit `harw_dod_bpf::BpfError::CapabilityUnavailable`) von
//! „einen Sensor bauen" (eine reine Werkonstruktion).
//!
//! # Welcher Zeitstempel gilt: `RawBpfEvent::observed_at`, nicht das injizierte `now`
//! `harw_dod_signals::Sensor::poll` bekommt `now` injiziert — kein Sensor
//! liest die Systemuhr selbst (siehe dortige Moduldoku). Für diesen Sensor
//! trägt aber bereits jedes einzelne
//! [`harw_dod_bpf::RawBpfEvent::observed_at`] den tatsächlichen
//! Beobachtungszeitpunkt, den das erzeugende eBPF-Programm beim Schreiben in
//! den Ringpuffer festgehalten hat (siehe `harw-dod-bpf::event`-Moduldoku zur
//! Zeitstempel-Annahme). Dieser Sensor übernimmt deshalb **`RawBpfEvent::observed_at`
//! pro Ereignis**, nicht das einmalige `now` des `poll`-Aufrufs, für
//! `SecurityEvent::observed_at`:
//!
//! - **Genauigkeit:** ein Ringpuffer kann mehrere Ereignisse zwischen zwei
//!   Polls ansammeln; sie alle mit demselben `now` zu stempeln verwischt die
//!   tatsächliche Reihenfolge und den tatsächlichen Abstand zwischen ihnen.
//! - **Determinismus bleibt gewahrt:** `RawBpfEvent::observed_at` stammt aus
//!   der injizierten Fixture (in Tests) bzw. aus dem eBPF-Programm, das
//!   selbst nie die Systemuhr dieses Sensors befragt — die Regel „keine
//!   Systemuhr" bleibt eingehalten, sie verschiebt sich nur auf die Quelle,
//!   die den tatsächlichen Zeitpunkt tatsächlich kennt.
//!
//! `now` bleibt Teil der Signatur (Vertrag von `Sensor::poll`), wird aber von
//! [`ProcmonSensor::poll`] nicht ausgewertet — derselbe Fall wie
//! `harw_dod_signals::sensor`'s eigenes `MockSensor`-Beispiel, das `_now`
//! ebenfalls ignoriert, weil dieser Sensor kein Rückschaufenster braucht: er
//! liest, was der Ringpuffer seit dem letzten Abruf angesammelt hat, nicht
//! „alles seit einem Zeitpunkt".
//!
//! # `harw_dod_fixtures::sensor_suite!` greift hier absichtlich nicht
//! `sensor_suite!` verlangt `From<harw_dod_cap::SensorHandle<harw_dod_cap::Bound>>`
//! — ein gebundener Griff hinein, eine Sensor-Instanz heraus. Das lässt sich
//! für [`ProcmonSensor`] nicht sinnvoll implementieren: die Konstruktion
//! braucht zusätzlich einen `Box<dyn harw_dod_bpf::BpfLoader>` und einen
//! bereits geladenen `harw_dod_bpf::BpfHandle`, die aus einem bloßen Griff
//! nicht ableitbar sind (derselbe Fall wie `harw-dod-authlog::AuthlogSensor`,
//! das zusätzlich ein `Box<dyn AuthBackend>` braucht — siehe dortige
//! Moduldoku für die ausführliche Begründung dieser Ausnahme).
//!
//! Selbst ein direkter Aufruf der zugrunde liegenden
//! `harw_dod_fixtures::harness::assert_*`-Funktionen — generisch über
//! `F: Fn(SensorHandle<Bound>) -> S`, nicht hart an `From` gebunden — würde
//! hier nichts Sinnvolles prüfen: dieses Fixture-Modell kopiert einen
//! `fixtures/<fall>/tree`-Verzeichnisbaum, bindet ihn als `ReadScope` und
//! schreibt Kanarien in seine Dateien. [`ProcmonSensor`] liest **nie** über
//! `handle.scope()`/`ReadScope` — seine einzige Quelle ist der injizierte
//! `Box<dyn harw_dod_bpf::BpfLoader>`. Jede `assert_*`-Prüfung dieses
//! Fixture-Modells würde deshalb trivial bestehen, ohne die eigentliche
//! Payload-Parse- oder Inhaltsfreiheits-Logik dieser Crate je auszuführen —
//! genau der Fall, der bei `harw-dod-authlog` bereits aufgefallen ist. Die
//! Tests in diesem Modul und in [`crate::event`] sind der inhaltlich richtige
//! Ersatz: sie prüfen genau das, was `sensor_suite!` hier nicht prüfen
//! könnte.
//!
//! # Determinismus trotz drainendem Lader
//! `harw_dod_bpf::fixture::FixtureBpfLoader::read_events` **entleert** seine
//! Warteschlange bei jedem Aufruf — wie ein echter Ringpuffer, der einmal
//! gelesen leer ist. Zwei Polls auf **derselben** Sensor-Instanz sind deshalb
//! bewusst **nicht** idempotent: der zweite liefert eine leere
//! `SensorReading`, unabhängig von `now`. Das unterscheidet diesen Sensor von
//! `harw-dod-authlog::AuthlogSensor`, dessen Backend bei jedem Aufruf
//! dieselbe (durch `since` gefilterte) Liste erneut liefert.
//!
//! „Determinismus" bedeutet hier deshalb: die Abbildung von Bytes auf
//! `SecurityEvent` hat keinen versteckten Zustand und keine
//! Systemuhr-Abhängigkeit. Der Test in diesem Modul prüft das, indem er
//! **zwei unabhängig konstruierte** Sensoren mit identischem Ereignisinhalt
//! je einmal mit demselben `now` pollt und identische Ergebnisse erwartet —
//! nicht, indem er denselben Sensor zweimal pollt.
//!
//! # Der Inhalt bleibt heikel
//! `SensorReading::samples` bleibt leer — ein Prozessstart ist ein Ereignis,
//! kein Messwert. Jedes erzeugte `SecurityEvent` trägt
//! `kind: EventKind::ProcessExec { path, argv_digest }`, **nie** die rohe
//! Kommandozeile (siehe [`crate::event`]-Moduldoku für die volle Begründung).
//! `pid`, `ppid` und `comm` aus [`crate::event::ExecEvent`] haben in der
//! heutigen `harw_dod_signals::EventKind::ProcessExec`-Form kein eigenes
//! Feld und gehen an dieser Abbildung bewusst verloren — eine Eigenschaft des
//! von dieser Crate nicht besessenen Wire-Vokabulars, nicht ein Versehen
//! dieses Sensors.
//!
//! # Exportierte Typen
//! [`ProcmonSensor`], [`DEFAULT_READ_TIMEOUT`].
//!
//! # Nebenläufigkeit
//! `ProcmonSensor: Send + Sync + Debug` (Anforderung von
//! `harw_dod_signals::Sensor`): `SensorHandle<Bound>` und
//! `Box<dyn harw_dod_bpf::BpfLoader>` sind beide `Send + Sync`,
//! `harw_dod_bpf::BpfHandle` und `std::time::Duration` sind es trivial.
//! [`ProcmonSensor::poll`] nimmt `&self` und führt keine innere
//! Veränderlichkeit — konkurrierende Polls auf demselben Sensor sind sicher
//! (auch wenn der gehaltene Lader selbst innere Veränderlichkeit einsetzen
//! darf, siehe `harw_dod_bpf::fixture::FixtureBpfLoader`).
//!
//! # Fehler
//! `harw_dod_cap::SensorError`, wie vom `Sensor`-Trait verlangt — sowohl
//! `harw_dod_bpf::BpfError` (aus dem gehaltenen Lader; alle sechs Varianten
//! werden einzeln behandelt, siehe [`bpf_error_to_sensor_error`] für die
//! vollständige Zuordnung und ihre `permanence()`-Begründung) als auch
//! [`crate::error::ProcmonError`] (aus [`crate::event::parse_exec_payload`])
//! werden auf diesen einen Typ abgebildet (private Abbildungsfunktionen in
//! diesem Modul, Muster: `harw-dod-authlog/src/sensor.rs::into_sensor_error`).
//!
//! # Examples
//! ```rust
//! use harw_dod_bpf::fixture::FixtureBpfLoader;
//! use harw_dod_bpf::{BpfLoader, BpfProgramKind, BpfProgramSource, BpfProgramSpec};
//! use harw_dod_cap::{Capability, ReadScope, SensorHandle};
//! use harw_dod_procmon::ProcmonSensor;
//! use harw_dod_signals::Sensor;
//! use harw_types::SensorId;
//! use std::borrow::Cow;
//!
//! let handle = SensorHandle::new(SensorId::from_str("procmon-0"), Capability::LoadBpfProgram)
//!     .bind(ReadScope::from_roots(Vec::<std::path::PathBuf>::new()));
//!
//! let loader = FixtureBpfLoader::new(Vec::new());
//! let spec = BpfProgramSpec::new(
//!     SensorId::from_str("procmon-0"),
//!     BpfProgramKind::Tracepoint,
//!     "sched:sched_process_exec",
//!     BpfProgramSource::Embedded(Cow::Borrowed(b"\0asm".as_slice())),
//! );
//! let bpf_handle = loader.load(&spec).expect("fixture loader with capability always succeeds");
//!
//! let sensor = ProcmonSensor::new(handle, Box::new(loader), bpf_handle);
//! let reading = sensor.poll(jiff::Timestamp::UNIX_EPOCH).expect("fixture-backed sensor never fails");
//! assert!(reading.events.is_empty());
//! ```

use std::time::Duration;

use harw_dod_bpf::{BpfError, BpfHandle, BpfLoader};
use harw_dod_cap::{Bound, SensorError, SensorHandle};
use harw_dod_signals::{Actor, EventKind, SecurityEvent, Sensor, SensorReading};
use jiff::Timestamp;

use crate::error::ProcmonError;
use crate::event::parse_exec_payload;

/// Voreingestellte Wartezeit für [`ProcmonSensor::new`]: **200 Millisekunden**.
///
/// # Beschreibung
/// Wie `harw-dod-authlog::DEFAULT_READ_TIMEOUT` ein bewusst gewählter
/// Platzhalter, kein aus einem festen Poll-Abstand abgeleiteter Wert — dieser
/// Workspace legt keinen programmweiten Poll-Abstand fest. Kurz genug, um
/// einen Poll-Zyklus nicht spürbar zu verzögern; lang genug, um einem echten
/// Ringpuffer Zeit für kurz zuvor angefallene Ereignisse zu geben. Ein
/// Aufrufer mit anderen Anforderungen verwendet [`ProcmonSensor::with_timeout`].
pub const DEFAULT_READ_TIMEOUT: Duration = Duration::from_millis(200);

/// Ein Sensor für Prozessstart-Ereignisse über einen injizierten
/// [`harw_dod_bpf::BpfLoader`].
///
/// # Description
/// Hält einen gebundenen Griff, einen Lader und einen bereits geladenen
/// [`harw_dod_bpf::BpfHandle`] als unabhängige, bei der Konstruktion
/// übergebene Werte. Siehe Moduldoku für die Fähigkeits-Invarianz zwischen
/// Griff und Lader, für die `observed_at`-Entscheidung und für die
/// `sensor_suite!`-Ausnahme.
pub struct ProcmonSensor {
    handle: SensorHandle<Bound>,
    loader: Box<dyn BpfLoader>,
    bpf_handle: BpfHandle,
    timeout: Duration,
}

impl ProcmonSensor {
    /// Baut einen Sensor mit [`DEFAULT_READ_TIMEOUT`].
    ///
    /// # Arguments
    /// - `handle` (`harw_dod_cap::SensorHandle<harw_dod_cap::Bound>`): der
    ///   gebundene Griff dieses Sensors. Sollte
    ///   `harw_dod_bpf::REQUIRED_CAPABILITY` tragen (siehe Moduldoku).
    /// - `loader` (`Box<dyn harw_dod_bpf::BpfLoader>`): die Ladeschicht, über
    ///   die [`Sensor::poll`] Ereignisse liest.
    /// - `bpf_handle` (`harw_dod_bpf::BpfHandle`): der Griff eines bereits
    ///   erfolgreich geladenen Programms (aus einem vorherigen
    ///   `loader.load(&spec)`-Aufruf).
    ///
    /// # Returns
    /// Einen `ProcmonSensor`, dessen [`Sensor::poll`] `loader.read_events`
    /// mit [`DEFAULT_READ_TIMEOUT`] aufruft.
    ///
    /// # Panics
    /// In Debug-Builds, wenn `handle.capability() != harw_dod_bpf::REQUIRED_CAPABILITY`
    /// (siehe Moduldoku). In Release-Builds keine Prüfung.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_bpf::fixture::FixtureBpfLoader;
    /// use harw_dod_bpf::{BpfLoader, BpfProgramKind, BpfProgramSource, BpfProgramSpec};
    /// use harw_dod_cap::{Capability, ReadScope, SensorHandle};
    /// use harw_dod_procmon::ProcmonSensor;
    /// use harw_types::SensorId;
    /// use std::borrow::Cow;
    ///
    /// let handle = SensorHandle::new(SensorId::from_str("procmon-0"), Capability::LoadBpfProgram)
    ///     .bind(ReadScope::from_roots(Vec::<std::path::PathBuf>::new()));
    /// let loader = FixtureBpfLoader::new(Vec::new());
    /// let spec = BpfProgramSpec::new(
    ///     SensorId::from_str("procmon-0"),
    ///     BpfProgramKind::Tracepoint,
    ///     "sched:sched_process_exec",
    ///     BpfProgramSource::Embedded(Cow::Borrowed(b"\0asm".as_slice())),
    /// );
    /// let bpf_handle = loader.load(&spec).expect("fixture loader with capability always succeeds");
    /// let _sensor = ProcmonSensor::new(handle, Box::new(loader), bpf_handle);
    /// ```
    #[must_use]
    pub fn new(
        handle: SensorHandle<Bound>,
        loader: Box<dyn BpfLoader>,
        bpf_handle: BpfHandle,
    ) -> Self {
        Self::with_timeout(handle, loader, bpf_handle, DEFAULT_READ_TIMEOUT)
    }

    /// Baut einen Sensor mit einer eigenen Wartezeit für `read_events`.
    ///
    /// # Arguments
    /// - `handle` (`harw_dod_cap::SensorHandle<harw_dod_cap::Bound>`): der
    ///   gebundene Griff dieses Sensors.
    /// - `loader` (`Box<dyn harw_dod_bpf::BpfLoader>`): die Ladeschicht.
    /// - `bpf_handle` (`harw_dod_bpf::BpfHandle`): der Griff des geladenen
    ///   Programms.
    /// - `timeout` (`std::time::Duration`): wie lange
    ///   [`harw_dod_bpf::BpfLoader::read_events`] bei jedem Poll auf
    ///   mindestens ein Ereignis warten darf.
    ///
    /// # Returns
    /// Einen `ProcmonSensor`, dessen [`Sensor::poll`] `loader.read_events`
    /// mit `timeout` aufruft.
    ///
    /// # Panics
    /// In Debug-Builds, wenn `handle.capability() != harw_dod_bpf::REQUIRED_CAPABILITY`.
    /// In Release-Builds keine Prüfung.
    #[must_use]
    pub fn with_timeout(
        handle: SensorHandle<Bound>,
        loader: Box<dyn BpfLoader>,
        bpf_handle: BpfHandle,
        timeout: Duration,
    ) -> Self {
        debug_assert_eq!(
            handle.capability(),
            harw_dod_bpf::REQUIRED_CAPABILITY,
            "ProcmonSensor: Griff muss harw_dod_bpf::REQUIRED_CAPABILITY (Capability::LoadBpfProgram) tragen"
        );
        Self {
            handle,
            loader,
            bpf_handle,
            timeout,
        }
    }
}

impl std::fmt::Debug for ProcmonSensor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Zeigt Griff, geladenen Griff und Wartezeit — der Lader ist ein
        // Trait-Objekt ohne `Debug`-Zusage.
        f.debug_struct("ProcmonSensor")
            .field("handle", &self.handle)
            .field("bpf_handle", &self.bpf_handle)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl Sensor for ProcmonSensor {
    /// Der gebundene Griff dieses Sensors.
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    /// Liest neu eingetroffene Ringpuffer-Einträge über den gehaltenen Lader
    /// und bildet jeden erfolgreich gedeuteten
    /// [`crate::event::ExecEvent`] auf ein `SecurityEvent` mit
    /// `EventKind::ProcessExec { path, argv_digest }` ab.
    ///
    /// # Description
    /// `now` wird nicht ausgewertet — siehe Moduldoku, Abschnitt „Welcher
    /// Zeitstempel gilt". Jedes erzeugte `SecurityEvent::observed_at` ist das
    /// `observed_at` des jeweiligen `harw_dod_bpf::RawBpfEvent`, nicht `now`.
    ///
    /// # Errors
    /// `harw_dod_cap::SensorError`, entpackt aus `harw_dod_bpf::BpfError`
    /// (wenn der Lader nicht lesbar ist oder ein Ringpuffer-Eintrag nicht die
    /// erwartete Kopf-Form hat) oder aus [`crate::error::ProcmonError`] (wenn
    /// ein `payload` nicht die in [`crate::event`] dokumentierte Form hat).
    fn poll(&self, _now: Timestamp) -> Result<SensorReading, SensorError> {
        let raw_events = self
            .loader
            .read_events(&self.bpf_handle, self.timeout)
            .map_err(bpf_error_to_sensor_error)?;

        let mut events = Vec::with_capacity(raw_events.len());
        for raw in raw_events {
            let exec = parse_exec_payload(&raw.payload).map_err(procmon_error_to_sensor_error)?;
            events.push(SecurityEvent {
                sensor: self.handle.id().clone(),
                observed_at: raw.observed_at,
                actor: Some(Actor {
                    uid: exec.uid,
                    auid: None,
                    cgroup: None,
                }),
                kind: EventKind::ProcessExec {
                    path: exec.filename,
                    argv_digest: exec.argv_digest,
                },
            });
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
/// die Fallunterscheidung, keinen Inhalt. Bewusst **kein** `_ =>`-Sammelzweig:
/// eine künftige, hier nicht aufgeführte `BpfError`-Variante soll den
/// Compiler mit einem nicht-erschöpfenden `match` stoppen, nicht still in
/// eine der sechs Zeilen unten fallen (derselbe Grund, aus dem
/// `harw_dod_cap::SensorError::permanence` selbst erschöpfend matcht).
///
/// Zuordnung, mit `harw_dod_cap::SensorError::permanence()` als Maßstab für
/// die Wahl der Zielvariante:
/// - [`BpfError::CapabilityUnavailable`] → `SensorError::SourceUnavailable`
///   (`Permanent`): der Host bietet die Fähigkeit zum Laden nicht an —
///   dieselbe Aussage aus Sicht des allgemeinen Zugriffsvokabulars.
/// - [`BpfError::MalformedEvent`] → `SensorError::MalformedSource`
///   (`Transient`): ein einzelner Ringpuffer-Eintrag hat nicht die erwartete
///   Form, aber der nächste könnte es wieder haben.
/// - [`BpfError::Io`] → `SensorError::Io` (`Transient`), unverändert
///   durchgereicht.
/// - [`BpfError::ProgramLoadFailed`] → `SensorError::SourceUnavailable`
///   (`Permanent`): entsteht ausschließlich in `BpfLoader::load`, das dieser
///   Sensor nie selbst aufruft (er bekommt einen bereits geladenen
///   `BpfHandle` übergeben, siehe Moduldoku „Wer lädt das BPF-Programm?") —
///   erreicht diese Abbildung also nur über einen Aufrufer, der einen
///   fehlgeschlagenen Ladeversuch trotzdem hier durchreicht. Ein
///   fehlgeschlagenes Laden (defektes Bytecode-Objekt, inkompatibler Kernel,
///   fehlende Ringpuffer-Map) ändert sich nicht dadurch, dass derselbe Griff
///   erneut abgefragt wird — dieselbe Eigenschaft, die
///   `SensorError::SourceUnavailable` laut `permanence()`-Doku zu `Permanent`
///   macht.
/// - [`BpfError::UnsupportedProgramKind`] → `SensorError::SourceUnavailable`
///   (`Permanent`): ein Programmtyp, den der reale Ladeteil nicht kennt,
///   wird auch beim nächsten Versuch nicht unterstützt (Einstufung durch den
///   Auftrag vorgegeben).
/// - [`BpfError::UnknownHandle`] → `SensorError::SourceUnavailable`
///   (`Permanent`): dieser Sensor hält genau einen unveränderlichen
///   `BpfHandle` über seine gesamte Lebensdauer (siehe [`ProcmonSensor`]-
///   Felder); kennt der reale Ladeteil ihn einmal nicht mehr, ändert sich
///   das nicht dadurch, dass [`ProcmonSensor::poll`] denselben Griff erneut
///   vorlegt — derselbe Griff erzeugt bei jedem künftigen Poll denselben
///   Fehler, also `Permanent`, nicht `Transient`.
fn bpf_error_to_sensor_error(err: BpfError) -> SensorError {
    // Die Abbildung lebt seit K73 an genau einer Stelle: `impl From<BpfError>
    // for SensorError` in `harw-dod-bpf/src/error.rs`. Diese Funktion bleibt
    // als benannter Aufrufpunkt bestehen, damit die vorhandenen Tests und
    // Aufrufstellen unverändert weiterlesen — sie trägt die Regel nicht mehr.
    SensorError::from(err)
}

/// Entpackt die eine `SensorError`-Variante aus [`ProcmonError`].
///
/// # Description
/// [`ProcmonError`] hat heute nur eine Variante,
/// [`ProcmonError::MalformedEvent`], die auf `SensorError::MalformedSource`
/// abgebildet wird.
fn procmon_error_to_sensor_error(err: ProcmonError) -> SensorError {
    match err {
        ProcmonError::MalformedEvent => SensorError::MalformedSource,
    }
}

#[cfg(test)]
mod tests {
    use super::{ProcmonSensor, bpf_error_to_sensor_error, procmon_error_to_sensor_error};
    use crate::error::ProcmonError;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_dod_bpf::event::RawBpfEvent;
    use harw_dod_bpf::fixture::FixtureBpfLoader;
    use harw_dod_bpf::{BpfError, BpfLoader, BpfProgramKind, BpfProgramSource, BpfProgramSpec};
    use harw_dod_cap::{Bound, Capability, ReadScope, SensorError, SensorHandle};
    use harw_dod_signals::{EventKind, Sensor};
    use harw_types::{ContentDigest, SensorId};
    use jiff::Timestamp;
    use std::borrow::Cow;

    const COMM_LEN: usize = 16;
    const FILENAME_LEN: usize = 256;

    fn handle_with(capability: Capability) -> SensorHandle<Bound> {
        SensorHandle::new(SensorId::from_str("procmon-test"), capability)
            .bind(ReadScope::from_roots(Vec::<std::path::PathBuf>::new()))
    }

    fn sample_spec() -> BpfProgramSpec {
        BpfProgramSpec::new(
            SensorId::from_str("procmon-test"),
            BpfProgramKind::Tracepoint,
            "sched:sched_process_exec",
            BpfProgramSource::Embedded(Cow::Borrowed(b"\0asm".as_slice())),
        )
    }

    /// Baut ein wohlgeformtes `payload` für ein Prozessstart-Ereignis, wie
    /// es `crate::event::parse_exec_payload` erwartet (siehe dortige Doku
    /// für das Byte-Layout).
    fn exec_payload(
        pid: u32,
        ppid: u32,
        uid: u32,
        comm: &str,
        filename: &str,
        argv: &[u8],
    ) -> Vec<u8> {
        let mut payload = Vec::new();
        payload.extend_from_slice(&pid.to_le_bytes());
        payload.extend_from_slice(&ppid.to_le_bytes());
        payload.extend_from_slice(&uid.to_le_bytes());

        let mut comm_field = [0u8; COMM_LEN];
        comm_field[..comm.len()].copy_from_slice(comm.as_bytes());
        payload.extend_from_slice(&comm_field);

        let mut filename_field = [0u8; FILENAME_LEN];
        filename_field[..filename.len()].copy_from_slice(filename.as_bytes());
        payload.extend_from_slice(&filename_field);

        payload.extend_from_slice(argv);
        payload
    }

    fn raw_event(pid: u32, comm: &str, observed_at: Timestamp, payload: Vec<u8>) -> RawBpfEvent {
        RawBpfEvent {
            pid,
            comm: comm.to_owned(),
            observed_at,
            payload,
        }
    }

    fn build_sensor(events: Vec<RawBpfEvent>) -> TestResult<ProcmonSensor> {
        let loader = FixtureBpfLoader::new(events);
        let bpf_handle = loader
            .load(&sample_spec())
            .map_err(ctx("fixture loader with capability always succeeds"))?;
        Ok(ProcmonSensor::new(
            handle_with(Capability::LoadBpfProgram),
            Box::new(loader),
            bpf_handle,
        ))
    }

    #[test]
    fn test_handle_returns_bound_handle_with_load_bpf_program_capability() -> TestResult {
        let sensor = build_sensor(Vec::new())?;
        assert_eq!(sensor.handle().capability(), Capability::LoadBpfProgram);
        Ok(())
    }

    #[test]
    fn test_poll_with_no_events_returns_empty_reading() -> TestResult {
        let sensor = build_sensor(Vec::new())?;
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("fixture-backed sensor never fails"))?;
        assert!(reading.samples.is_empty());
        assert!(reading.events.is_empty());
        Ok(())
    }

    #[test]
    fn test_poll_maps_exec_event_to_security_event_process_exec() -> TestResult {
        let observed_at = Timestamp::new(1_700_000_000, 0).map_err(ctx("gültiger Zeitstempel"))?;
        let payload = exec_payload(4_242, 1, 0, "sshd", "/usr/sbin/sshd", b"-D\0-e");
        let sensor = build_sensor(vec![raw_event(4_242, "sshd", observed_at, payload)])?;

        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("fixture-backed sensor never fails"))?;
        assert!(reading.samples.is_empty());
        assert_eq!(reading.events.len(), 1);

        let event = &reading.events[0];
        // `observed_at` kommt vom `RawBpfEvent`, nicht vom injizierten `now`
        // des `poll`-Aufrufs (siehe Moduldoku).
        assert_eq!(event.observed_at, observed_at);
        assert_ne!(event.observed_at, Timestamp::UNIX_EPOCH);

        let actor = event
            .actor
            .as_ref()
            .ok_or(TestError::Missing("ProcessExec trägt einen Actor"))?;
        assert_eq!(actor.uid, 0);
        assert_eq!(actor.auid, None);

        match &event.kind {
            EventKind::ProcessExec { path, argv_digest } => {
                assert_eq!(path, "/usr/sbin/sshd");
                assert_eq!(*argv_digest, ContentDigest::of(b"-D\0-e"));
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected EventKind::ProcessExec, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_poll_propagates_malformed_payload_as_malformed_source() -> TestResult {
        let sensor = build_sensor(vec![raw_event(
            1,
            "x",
            Timestamp::UNIX_EPOCH,
            vec![1, 2, 3],
        )])?;
        let Err(err) = sensor.poll(Timestamp::UNIX_EPOCH) else {
            return Err(TestError::Unexpected(
                "zu kurzes payload muss scheitern".into(),
            ));
        };
        assert!(matches!(err, SensorError::MalformedSource));
        Ok(())
    }

    #[test]
    fn test_poll_two_independently_built_sensors_with_same_now_yield_identical_readings()
    -> TestResult {
        // `FixtureBpfLoader::read_events` entleert seine Warteschlange —
        // ein zweiter Poll auf demselben Sensor ist bewusst nicht
        // idempotent (siehe Moduldoku, Abschnitt „Determinismus trotz
        // drainendem Lader"). Determinismus bedeutet hier: zwei unabhängig
        // konstruierte Sensoren mit identischem Ereignisinhalt liefern bei
        // gleichem `now` dasselbe Ergebnis.
        let observed_at = Timestamp::new(1_000, 0).map_err(ctx("gültiger Zeitstempel"))?;
        let make_events = || {
            vec![raw_event(
                7,
                "init",
                observed_at,
                exec_payload(7, 1, 0, "init", "/sbin/init", b""),
            )]
        };

        let first = build_sensor(make_events())?
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("erster Poll"))?;
        let second = build_sensor(make_events())?
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("zweiter Poll, unabhängige Sensor-Instanz"))?;

        assert_eq!(first, second);
        Ok(())
    }

    #[test]
    fn test_poll_argv_bytes_never_appear_in_the_resulting_reading() -> TestResult {
        let secret = b"--password=SuperSecretSharedToken123!";
        let payload = exec_payload(1, 0, 0, "curl", "/usr/bin/curl", secret);
        let sensor = build_sensor(vec![raw_event(1, "curl", Timestamp::UNIX_EPOCH, payload)])?;

        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .map_err(ctx("fixture-backed sensor never fails"))?;
        let debug_output = format!("{reading:?}");
        assert!(!debug_output.contains("SuperSecretSharedToken123"));
        assert!(!debug_output.contains("--password="));
        Ok(())
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
        assert!(matches!(
            bpf_error_to_sensor_error(BpfError::Io(source)),
            SensorError::Io(_)
        ));
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
    fn test_procmon_error_to_sensor_error_maps_malformed_event_to_malformed_source() {
        assert!(matches!(
            procmon_error_to_sensor_error(ProcmonError::MalformedEvent),
            SensorError::MalformedSource
        ));
    }
}
