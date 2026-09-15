//! Prozessstart-Ereignisse über eBPF: wer hat was gestartet (Knoten **AW7-01b**).
//!
//! # Verantwortungsbereich
//! Diese Crate deutet das art-spezifische `payload` eines
//! Prozessstart-Ereignisses aus dem Ringpuffer ([`event::parse_exec_payload`])
//! und bindet einen injizierten `harw_dod_bpf::BpfLoader` an
//! `harw_dod_signals::Sensor` ([`sensor::ProcmonSensor`]). Sie ist die
//! **Deutung**, nicht das Laden: der eigentliche `bpf()`-Syscall lebt in
//! `harw-dod-bpf` (dort dokumentiert **nicht** Teil dieser Lieferung — siehe
//! dortige Moduldoku, Abschnitt „Abweichung vom Auftrag"), diese Crate baut
//! ausschließlich auf dem `harw_dod_bpf::BpfLoader`-Trait auf, nie auf einer
//! echten Bindung. [`harw_dod_bpf::FixtureBpfLoader`] ist Teil der normalen
//! API dieser Abhängigkeit — kein Kernel, keine Berechtigung, keine
//! eBPF-Toolchain nötig, um diese Crate zu testen.
//!
//! Geschwister-Crate von `harw-dod-flow` (AW7-01c): die beiden kennen
//! einander **nicht** (Invarianz C7, CI-geprüft). Diese Crate hängt
//! ausschließlich an `harw-dod-bpf`, `harw-dod-cap`, `harw-dod-signals`,
//! `harw-types`, `harw-macros` und `jiff`.
//!
//! # Genau eine Fähigkeit
//! [`harw_dod_bpf::REQUIRED_CAPABILITY`]
//! (`harw_dod_cap::Capability::LoadBpfProgram`, Klasse
//! `harw_dod_cap::CapabilityClass::Bpf`). [`sensor::ProcmonSensor`] erfindet
//! keine eigene, zweite Fähigkeit — siehe dortige Moduldoku für die
//! Debug-Prüfung der Übereinstimmung zwischen Griff und Fähigkeit.
//!
//! # Warum `argv_digest`, nie die rohe Kommandozeile
//! `harw_dod_signals::EventKind::ProcessExec` trägt `path` und
//! `argv_digest`, **nicht** die rohe Kommandozeile — deren eigene Doku
//! begründet das: eine Kommandozeile ist angreiferkontrolliert und enthält
//! regelmäßig Geheimnisse (Zugangstoken als Argument, eingebettete
//! Passwörter). Ein Digest belegt Gleichheit — zwei Ereignisse hatten
//! dieselbe Kommandozeile — ohne den Inhalt selbst zu transportieren oder
//! dauerhaft zu speichern. [`event::parse_exec_payload`] liest die rohen
//! `argv`-Bytes genau einmal, ausschließlich zur Digestbildung; danach
//! existiert keine Kopie mehr — weder in [`event::ExecEvent`] noch in
//! [`error::ProcmonError`] noch in einem `harw_dod_signals::SecurityEvent`,
//! das [`sensor::ProcmonSensor::poll`] daraus baut. Siehe [`event`]-Moduldoku
//! für die volle Begründung und den strukturellen Test dieser Zusage.
//!
//! # Das Payload-Format
//! [`event::parse_exec_payload`] deutet `harw_dod_bpf::RawBpfEvent::payload`
//! als festen, gepackten Feldbereich (`pid`, `ppid`, `uid`, `comm`,
//! `filename`, je nach eigener Wahl dieser Crate — nicht identisch mit dem
//! realen `sched_process_exec`-Tracepoint, der z. B. kein `ppid`/`uid`
//! kennt), gefolgt von `argv` als Rest des Puffers. Siehe [`event`]-Moduldoku
//! für die vollständige Byte-Tabelle und die Begründung jeder Breite.
//!
//! # Die Programmkonstante: das Gegenstück zu `harw_dod_flow::flow_program_spec`
//! `harw-dod-flow` exportiert `flow_program_spec()` — eine Funktion, die als
//! Code sichtbar und testbar macht, mit welcher `harw_dod_bpf::BpfProgramSpec`
//! ein echter Lader das Programm dieser Geschwister-Crate laden würde. Diese
//! Crate hatte bislang kein Gegenstück: ein Aufrufer musste sich Programmart
//! und Anknüpfungspunkt (`syscalls:sys_enter_execve`, ein
//! `harw_dod_bpf::BpfProgramKind::Tracepoint`, der wie
//! `harw_dod_flow::FLOW_TRACEPOINT_ATTACH_POINT` ohne bereits offenen Socket
//! auskommt) **aus dieser Moduldoku abschreiben**, statt sie aus Code zu
//! übernehmen — derselbe Fehlertyp wie K40, wo eine Sicherheitsregel ihre
//! Einstufung aus Textmustern statt aus einem Typ rekonstruierte.
//! [`PROCMON_TRACEPOINT_ATTACH_POINT`] und [`procmon_program_spec`] schließen
//! diese Lücke, in derselben Form wie `harw_dod_flow::flow_program_spec`:
//! Programmart und Anknüpfungspunkt sind fest, nur die Rumpfquelle
//! (`harw_dod_bpf::BpfProgramSource`) bleibt dem Aufrufer überlassen.
//!
//! # Exportierte Typen
//! [`event::ExecEvent`], [`event::parse_exec_payload`],
//! [`error::ProcmonError`], [`error::ProcmonResult`], [`sensor::ProcmonSensor`],
//! [`sensor::DEFAULT_READ_TIMEOUT`], [`PROCMON_TRACEPOINT_ATTACH_POINT`],
//! [`procmon_program_spec`].
//!
//! # Nebenläufigkeit
//! [`event::parse_exec_payload`] ist eine zustandslose, reine Funktion,
//! sicher aus beliebig vielen Threads aufrufbar. [`sensor::ProcmonSensor`]
//! ist `Send + Sync + std::fmt::Debug` (Anforderung von
//! `harw_dod_signals::Sensor`) — siehe [`sensor`]-Moduldoku für die
//! Begründung je Feld.
//!
//! # Fehler
//! [`error::ProcmonError`] ist der eine Fehlertyp dieser Crate — inhaltsfrei,
//! insbesondere ohne jedes `argv`-Byte (siehe [`error`]-Moduldoku).
//! [`sensor::ProcmonSensor::poll`] entpackt daraus und aus
//! `harw_dod_bpf::BpfError` das vom `Sensor`-Vertrag verlangte
//! `harw_dod_cap::SensorError`.
//!
//! # `harw_dod_fixtures::sensor_suite!` passt hier nicht
//! Siehe [`sensor`]-Moduldoku, Abschnitt „`harw_dod_fixtures::sensor_suite!`
//! greift hier absichtlich nicht", für die volle Begründung: die
//! Konstruktion von [`sensor::ProcmonSensor`] braucht mehr als einen
//! gebundenen Griff, und das Fixture-Modell von `harw-dod-fixtures` prüft
//! ausschließlich dateisystemlesende Sensoren über `handle.scope()` — ein
//! Weg, den dieser Sensor nie benutzt. Alle Prüfungen dieser Crate sind
//! deshalb von Hand geschrieben, in [`event`] und [`sensor`].
//!
//! # Examples
//! ```rust
//! use harw_dod_bpf::event::RawBpfEvent;
//! use harw_dod_bpf::fixture::FixtureBpfLoader;
//! use harw_dod_bpf::{BpfLoader, BpfProgramKind, BpfProgramSource, BpfProgramSpec};
//! use harw_dod_cap::{Capability, ReadScope, SensorHandle};
//! use harw_dod_procmon::ProcmonSensor;
//! use harw_dod_signals::{EventKind, Sensor};
//! use harw_types::SensorId;
//! use std::borrow::Cow;
//!
//! let mut payload = Vec::new();
//! payload.extend_from_slice(&4_242u32.to_le_bytes()); // pid
//! payload.extend_from_slice(&1u32.to_le_bytes()); // ppid
//! payload.extend_from_slice(&0u32.to_le_bytes()); // uid
//! payload.extend_from_slice(&[0u8; 16]); // comm (leer in diesem Beispiel)
//! let mut filename = [0u8; 256];
//! filename[.."/usr/sbin/sshd".len()].copy_from_slice(b"/usr/sbin/sshd");
//! payload.extend_from_slice(&filename);
//! payload.extend_from_slice(b"-D"); // argv: nur zum Digest gehasht
//!
//! let event = RawBpfEvent {
//!     pid: 4_242,
//!     comm: String::new(),
//!     observed_at: jiff::Timestamp::UNIX_EPOCH,
//!     payload,
//! };
//! let loader = FixtureBpfLoader::new(vec![event]);
//! let spec = BpfProgramSpec::new(
//!     SensorId::from_str("procmon-0"),
//!     BpfProgramKind::Tracepoint,
//!     "sched:sched_process_exec",
//!     BpfProgramSource::Embedded(Cow::Borrowed(b"\0asm".as_slice())),
//! );
//! let bpf_handle = loader.load(&spec).expect("fixture loader with capability always succeeds");
//!
//! let handle = SensorHandle::new(SensorId::from_str("procmon-0"), Capability::LoadBpfProgram)
//!     .bind(ReadScope::from_roots(Vec::<std::path::PathBuf>::new()));
//! let sensor = ProcmonSensor::new(handle, Box::new(loader), bpf_handle);
//!
//! let reading = sensor.poll(jiff::Timestamp::UNIX_EPOCH).expect("fixture-backed sensor never fails");
//! assert_eq!(reading.events.len(), 1);
//! assert!(matches!(reading.events[0].kind, EventKind::ProcessExec { .. }));
//! ```

pub mod error;
pub mod event;
pub mod sensor;

pub use error::{ProcmonError, ProcmonResult};
pub use event::{parse_exec_payload, ExecEvent};
pub use sensor::{ProcmonSensor, DEFAULT_READ_TIMEOUT};

/// Der Kernel-Tracepoint, an den das eBPF-Programm dieser Crate angehängt
/// wird.
///
/// # Description
/// `syscalls:sys_enter_execve` feuert beim Eintritt in den
/// `execve`-Systemaufruf und trägt bereits Prozesskontext und den
/// auszuführenden Pfad — alles, was [`event::parse_exec_payload`] deutet
/// (siehe dortige Moduldoku für die vollständige Byte-Tabelle). Ein
/// `harw_dod_bpf::BpfProgramKind::Tracepoint` darauf braucht keinen bereits
/// offenen Socket oder ein anderes vorab beschafftes Objekt — derselbe Grund,
/// aus dem `harw_dod_flow::FLOW_TRACEPOINT_ATTACH_POINT` einen Tracepoint statt
/// eines `SocketFilter` wählt (siehe dortige Moduldoku, Abschnitt „Warum ein
/// Tracepoint").
///
/// # Examples
/// ```rust
/// assert_eq!(harw_dod_procmon::PROCMON_TRACEPOINT_ATTACH_POINT, "syscalls:sys_enter_execve");
/// ```
pub const PROCMON_TRACEPOINT_ATTACH_POINT: &str = "syscalls:sys_enter_execve";

/// Baut die Programmbeschreibung, mit der ein echter `harw_dod_bpf::BpfLoader`
/// das eBPF-Programm dieser Crate laden würde.
///
/// # Description
/// Das zu `harw_dod_flow::flow_program_spec` symmetrische Gegenstück (siehe
/// Moduldoku, Abschnitt „Die Programmkonstante"): Programmart ist fest
/// `harw_dod_bpf::BpfProgramKind::Tracepoint`, der Anknüpfungspunkt fest
/// [`PROCMON_TRACEPOINT_ATTACH_POINT`]. Nur die Rumpfquelle bleibt dem
/// Aufrufer überlassen — sie hängt davon ab, ob das aufrufende Binary den
/// Bytecode einbettet oder von einem Pfad lädt (siehe
/// `harw_dod_bpf::BpfProgramSource`-Moduldoku).
///
/// # Arguments
/// - `sensor` (`harw_types::SensorId`): der Sensor, dem das geladene
///   Programm dient.
/// - `source` (`harw_dod_bpf::BpfProgramSource`): woher der Bytecode-Rumpf
///   kommt.
///
/// # Returns
/// Eine `harw_dod_bpf::BpfProgramSpec`, bereit für `BpfLoader::load`.
///
/// # Examples
/// ```rust
/// use harw_dod_bpf::{BpfProgramKind, BpfProgramSource};
/// use harw_dod_procmon::procmon_program_spec;
/// use harw_types::SensorId;
/// use std::borrow::Cow;
///
/// let spec = procmon_program_spec(
///     SensorId::from_str("procmon-0"),
///     BpfProgramSource::Embedded(Cow::Borrowed(b"\0asm".as_slice())),
/// );
/// assert_eq!(spec.kind, BpfProgramKind::Tracepoint);
/// assert_eq!(spec.attach_point, "syscalls:sys_enter_execve");
/// ```
#[must_use]
pub fn procmon_program_spec(
    sensor: harw_types::SensorId,
    source: harw_dod_bpf::BpfProgramSource,
) -> harw_dod_bpf::BpfProgramSpec {
    harw_dod_bpf::BpfProgramSpec::new(
        sensor,
        harw_dod_bpf::BpfProgramKind::Tracepoint,
        PROCMON_TRACEPOINT_ATTACH_POINT,
        source,
    )
}

#[cfg(test)]
mod tests {
    use super::{procmon_program_spec, PROCMON_TRACEPOINT_ATTACH_POINT};

    #[test]
    fn test_procmon_program_spec_uses_a_tracepoint_at_the_documented_attach_point() {
        let dir = tempfile::tempdir().expect("tempdir for program body");
        let body_path = dir.path().join("procmon.bpf.o");
        std::fs::write(&body_path, b"bytecode-bytes").expect("write fixture program body");

        let spec = procmon_program_spec(
            harw_types::SensorId::from_str("procmon-0"),
            harw_dod_bpf::BpfProgramSource::Path(body_path),
        );

        assert_eq!(spec.kind, harw_dod_bpf::BpfProgramKind::Tracepoint);
        assert_eq!(spec.attach_point, PROCMON_TRACEPOINT_ATTACH_POINT);
        assert_eq!(spec.attach_point, "syscalls:sys_enter_execve");
    }

    #[test]
    fn test_procmon_tracepoint_attach_point_is_the_documented_syscall_tracepoint() {
        assert_eq!(PROCMON_TRACEPOINT_ATTACH_POINT, "syscalls:sys_enter_execve");
    }
}
