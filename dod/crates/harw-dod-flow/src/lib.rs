//! Verbindungsereignisse über eBPF — Knoten **AW7-01c**.
//!
//! > **V1-Betriebsvertrag.** Der ältere `FlowSensor`/`RawBpfEvent`-Pfad
//! > weiter unten ist Fixture- und Übergangskompatibilität, nicht der Weg
//! > für den realen BPF-Lader. Produktionscode erstellt die beiden
//! > [`flow_contracts`] und verarbeitet `TcpConnectEventV1` aus
//! > versionierten Wire-Ereignissen. Der Hook beschreibt den
//! > **ausgehenden Connect-Versuch**, nicht einen späteren Socket-State.
//!
//! # Verantwortungsbereich
//! Geschwister-Crate von `harw-dod-procmon` (AW7-01b): beide bauen auf
//! `harw-dod-bpf` (AW7-01a) auf, beide melden über `harw-dod-signals`, aber
//! sie kennen einander nicht — Invariante **C7**, von einem CI-Gate geprüft.
//! Genau deshalb sind es zwei Knoten und nicht einer: ein Verbindungssensor
//! und ein Prozesssensor haben unterschiedliche Melderegeln und dürfen sich
//! nicht gegenseitig beeinflussen können.
//!
//! Diese Crate deutet den art-spezifischen `payload`-Rest eines
//! `harw_dod_bpf::RawBpfEvent` als [`event::FlowEvent`]
//! ([`event::parse_flow_payload`]) und entscheidet, welche dieser Ereignisse
//! als `harw_dod_signals::SecurityEvent` mit
//! `harw_dod_signals::EventKind::EgressFlow` gemeldet werden
//! ([`report::to_security_event`], [`report::observe`]).
//!
//! # Die eine Fähigkeit
//! [`REQUIRED_CAPABILITY`] — `harw_dod_cap::Capability::LoadBpfProgram`
//! (Klasse `harw_dod_cap::CapabilityClass::Bpf`). Diese Crate setzt sie
//! nicht selbst durch; das bleibt Sache eines echten
//! `harw_dod_bpf::BpfLoader` und des Sentinels, der ihn registriert.
//!
//! # Warum ein Tracepoint — und kein roher Socket
//! `harw-dod-bpf` hat den echten Ladeteil bewusst nicht gebaut, unter anderem
//! weil `aya::programs::SocketFilter::attach` einen **bereits offenen
//! Socket** braucht (`T: AsFd`), den ein generischer Loader nicht besitzen
//! darf — das Öffnen eines rohen Sockets ist ein eigener, privilegierter
//! Vorgang, der beim Aufrufer liegen sollte, der ihn tatsächlich braucht
//! (siehe `harw_dod_bpf`-Moduldoku, Abschnitt „Abweichung vom Auftrag").
//!
//! Diese Crate **braucht** ihn nicht: Verbindungszustandswechsel (Verbindung
//! aufgebaut, geschlossen, …) lassen sich vollständig über den statischen
//! BTF-Funktionseinstieg `tcp_v4_connect` und `tcp_v6_connect` beobachten
//! ([`FLOW_TCP_V4_CONNECT_ATTACH_POINT`]) — ein `harw_dod_bpf::BpfProgramKind::FEntry`,
//! der wie `sched:sched_process_exec` in `harw-dod-procmon` ohne einen
//! bereits offenen Socket auskommt. Ein `SocketFilter` (der einen solchen
//! Socket bräuchte) wäre nötig, um den *Inhalt* von Paketen auf einer
//! Schnittstelle zu sehen — aber diese Crate meldet ausdrücklich keine
//! Nutzdaten (siehe unten) und braucht dafür keinen Paketinhalt, nur
//! Zustandswechsel mit Metadaten. Diese Crate öffnet deshalb **keinen** rohen
//! Socket und baut ausschließlich gegen `harw_dod_bpf::BpfLoader` (den
//! Trait), nie gegen eine echte Bindung — [`flow_program_spec`] macht diese
//! Entscheidung als Code sichtbar und testbar, nicht nur als Prosa.
//!
//! # Das Payload-Format
//! Siehe [`event`]-Moduldoku für die vollständige Feldtabelle mit
//! Byte-Bereichen und Byte-Reihenfolge je Feld. Kurzfassung: 32 Byte fest,
//! `pid`/`uid` Little-Endian (wiederverwendet `harw_dod_bpf::event::read_u32_le`),
//! `remote_port` **Network Byte Order (Big-Endian)** — die klassische Falle
//! an genau dieser Stelle —, ein `family`-Byte entscheidet zwischen 4 und 16
//! gültigen Adress-Bytes innerhalb eines stets 16 Byte breiten Adressfeldes,
//! damit sich IPv4 und IPv6 **nicht** an der Payload-Länge unterscheiden
//! lassen und das Familienfeld nicht optional ist.
//!
//! # Die Melderegel
//! Siehe [`report`]-Moduldoku für die vollständige Begründung. Kurzfassung:
//! gemeldet wird die **volle** Zieladresse (nicht ein Netz), aber **nur**,
//! wenn die Verbindung ausgehend ist **und** ihr Ziel außerhalb des
//! übergebenen `harw_authority::NetworkScope` liegt — „meldet nur, was den
//! erlaubten Bereich verlässt". Diese Crate hängt dafür bewusst zusätzlich
//! an `harw-sandbox`, über die im Auftrag nicht ausdrücklich vorgesehene,
//! aber geprüfte Kantenrichtung: `harw-sandbox` hängt selbst an nichts aus
//! dem `harw-dod-*`-Teilbaum, eine Abhängigkeit von hier aus schließt also
//! keinen Zyklus. Loopback und private Bereiche bekommen **keine**
//! eingebaute Ausnahme — was als Ziel gilt, entscheidet ausschließlich der
//! übergebene `NetworkScope`.
//!
//! # Der Zeitstempel
//! [`report::observe`] verwendet `harw_dod_bpf::RawBpfEvent::observed_at`
//! (den vom erzeugenden eBPF-Programm pro Ereignis injizierten Zeitpunkt),
//! **nicht** ein zusätzliches, grobkörniges Poll-`now`. Beide Werte sind
//! bereits injiziert, keiner liest die Systemuhr dieser Crate — siehe
//! [`report`]-Moduldoku für die volle Begründung, warum die
//! Pro-Ereignis-Präzision hier die informativere und nicht die riskantere
//! Wahl ist.
//!
//! # Keine Nutzdaten
//! Verbindungs*inhalte* verlassen den Kernel über diese Crate nie —
//! [`event::FlowEvent`] und die daraus gebaute `SecurityEvent` tragen
//! ausschließlich Metadaten (`pid`, `uid`, Protokoll, Richtung, Zieladresse,
//! Zielport). [`event::parse_flow_payload`] liest nie mehr als die festen 32
//! Byte des Layouts; Bytes danach — und damit jeder mögliche
//! Nutzdaten-Anhang — werden strukturell nie in ein Feld übernommen. Belegt
//! durch Tests in [`event`] (Debug-Form) und [`report`]
//! (serialisierte `SecurityEvent`).
//!
//! # Diese Crate implementiert `harw_dod_signals::Sensor` (K52)
//! [`sensor::FlowSensor`] bindet einen injizierten `harw_dod_bpf::BpfLoader`
//! und einen `harw_authority::NetworkScope` an
//! `harw_dod_signals::Sensor::poll` — nach demselben Muster wie
//! `harw-dod-procmon::ProcmonSensor`, damit ein Konsument wie
//! `harw-probe-bpf` beide Sensoren einheitlich über `Arc<dyn Sensor>` und
//! `poll(now)` einsammeln kann. Diese Crate liest dabei weiterhin **nie**
//! über `handle.scope()`; siehe [`sensor`]-Moduldoku, Abschnitt „Warum
//! `harw_dod_fixtures::sensor_suite!` hier weiterhin nicht passt", für die
//! Begründung, warum diese Crate ihre Tests bewusst von Hand schreibt statt
//! über `sensor_suite!` zu prüfen, sowie für die Begründung, warum
//! [`report::observe`] neben `poll()` erhalten bleibt.
//!
//! # Warum `harw-dod-fixtures` hier nicht passt
//! Siehe [`report`]- und [`sensor`]-Moduldoku für die volle Begründung:
//! `sensor_suite!` prüft gegen einen echten `fixtures/<fall>/tree`-
//! Verzeichnisbaum über `harw_dod_cap::SensorHandle::scope()` — diese Crate
//! liest nie ein Dateisystem. Ereignisse kommen ausschließlich über einen
//! injizierten `harw_dod_bpf::BpfLoader` herein. Alle Tests dieser Crate sind
//! deshalb von Hand geschrieben und laufen über
//! `harw_dod_bpf::fixture::FixtureBpfLoader`, nie über `sensor_suite!`.
//!
//! # Exportierte Typen
//! [`event::Protocol`], [`event::Direction`], [`event::FlowEvent`],
//! [`event::parse_flow_payload`], [`report::to_security_event`],
//! [`report::observe`], [`sensor::FlowSensor`], [`sensor::DEFAULT_READ_TIMEOUT`],
//! [`error::FlowError`], [`error::FlowResult`],
//! [`REQUIRED_CAPABILITY`], [`FLOW_TCP_V4_CONNECT_ATTACH_POINT`],
//! [`flow_program_spec`].
//!
//! # Nebenläufigkeit
//! [`event`] und [`report`] sind ausschließlich zustandslose, reine
//! Funktionen und Werttypen: `Send + Sync`, keine innere Veränderlichkeit,
//! beliebig aus mehreren Threads aufrufbar. [`sensor::FlowSensor`] ist
//! `Send + Sync + std::fmt::Debug` (Anforderung von
//! `harw_dod_signals::Sensor`) und führt keine innere Veränderlichkeit —
//! siehe [`sensor`]-Moduldoku für die Begründung je Feld.
//!
//! # Fehler
//! [`error::FlowError`] ist der eine Fehlertyp dieser Crate.
//! [`sensor::FlowSensor::poll`] entpackt daraus und aus
//! `harw_dod_bpf::BpfError` das vom `harw_dod_signals::Sensor`-Vertrag
//! verlangte `harw_dod_cap::SensorError`.
//!
//! # Examples
//! ```rust
//! use harw_dod_flow::{event::parse_flow_payload, report::to_security_event};
//! use harw_authority::NetworkScope;
//! use harw_types::SensorId;
//!
//! let mut payload = vec![0u8; 32];
//! payload[0..4].copy_from_slice(&100u32.to_le_bytes());
//! payload[4..8].copy_from_slice(&1_000u32.to_le_bytes());
//! payload[8] = 0; // TCP
//! payload[9] = 1; // ausgehend
//! payload[10] = 0; // IPv4
//! payload[12..14].copy_from_slice(&443u16.to_be_bytes());
//! payload[16..20].copy_from_slice(&[203, 0, 113, 9]);
//!
//! let event = parse_flow_payload(&payload).expect("well-formed fixture payload");
//! let reported = to_security_event(
//!     &event,
//!     &SensorId::from_str("flow-0"),
//!     jiff::Timestamp::UNIX_EPOCH,
//!     &NetworkScope::empty(),
//! );
//! assert!(reported.is_some());
//! ```

pub mod error;
pub mod event;
pub mod report;
pub mod sensor;

pub use error::{FlowError, FlowResult};
pub use event::{parse_flow_payload, parse_tcp_connect_v1, Direction, FlowEvent, Protocol, TcpConnectEventV1};
pub use report::{observe, to_security_event};
pub use sensor::{FlowSensor, DEFAULT_READ_TIMEOUT};

/// Die Fähigkeit, die ein echter (Nicht-Fixture-)Lader für diese Crate
/// braucht.
///
/// # Description
/// Diese Crate setzt die Fähigkeit nicht selbst durch — das bleibt Sache
/// eines echten `harw_dod_bpf::BpfLoader` und des Sentinels, der ihn
/// registriert. Diese Konstante benennt sie nur.
///
/// # Examples
/// ```rust
/// use harw_dod_flow::REQUIRED_CAPABILITY;
/// use harw_dod_cap::CapabilityClass;
///
/// assert_eq!(REQUIRED_CAPABILITY.class(), CapabilityClass::Bpf);
/// ```
pub const REQUIRED_CAPABILITY: harw_dod_cap::Capability = harw_dod_cap::Capability::LoadBpfProgram;

/// Der Kernel-Tracepoint, an den das eBPF-Programm dieser Crate angehängt
/// wird.
///
/// # Description
/// `tcp_v4_connect` feuert im Kontext des aufrufenden Tasks vor dem
/// Sockets (u. a. Verbindungsaufbau und -abbau) und trägt dabei bereits
/// Prozesskontext, Adressfamilie, lokale und entfernte Adresse/Port — alles,
/// was [`event::parse_tcp_connect_v1`] deutet. Ein `BpfProgramKind::FEntry`
/// darauf braucht **keinen** bereits offenen Socket, anders als ein
/// `BpfProgramKind::SocketFilter` — siehe Moduldoku, Abschnitt „Warum ein
/// Tracepoint".
///
/// # Examples
/// ```rust
/// assert_eq!(harw_dod_flow::FLOW_TCP_V4_CONNECT_ATTACH_POINT, "tcp_v4_connect");
/// ```
/// The target BTF function used for IPv4 outbound TCP attempts.  Unlike the
/// former socket-state tracepoint, fentry runs in the initiating task.
pub const FLOW_TCP_V4_CONNECT_ATTACH_POINT: &str = harw_dod_bpf::TCP_V4_CONNECT_ATTACH_POINT;
/// The target BTF function used for IPv6 outbound TCP attempts.
pub const FLOW_TCP_V6_CONNECT_ATTACH_POINT: &str = harw_dod_bpf::TCP_V6_CONNECT_ATTACH_POINT;
pub const FLOW_TCP_V4_CONNECT_PROGRAM_NAME: &str = harw_dod_bpf::TCP_V4_CONNECT_PROGRAM_NAME;
pub const FLOW_TCP_V6_CONNECT_PROGRAM_NAME: &str = harw_dod_bpf::TCP_V6_CONNECT_PROGRAM_NAME;

/// Build both task-context TCP contracts.  `tcp_v4_connect` and
/// `tcp_v6_connect` are separate target-BTF fentry attachments so a host
/// missing either one cannot quietly claim full dual-stack coverage.
#[must_use]
pub fn flow_contracts(
    sensor: harw_types::SensorId,
    ipv4_source: harw_dod_bpf::BpfProgramSource,
    ipv6_source: harw_dod_bpf::BpfProgramSource,
    scope: harw_dod_bpf::BpfScope,
) -> [harw_dod_bpf::BpfObjectContract; 2] {
    [
        harw_dod_bpf::BpfObjectContract::new(
            sensor.clone(),
            FLOW_TCP_V4_CONNECT_PROGRAM_NAME,
            harw_dod_bpf::BpfProgramKind::FEntry,
            FLOW_TCP_V4_CONNECT_ATTACH_POINT,
            ipv4_source,
            scope.clone(),
        ),
        harw_dod_bpf::BpfObjectContract::new(
            sensor,
            FLOW_TCP_V6_CONNECT_PROGRAM_NAME,
            harw_dod_bpf::BpfProgramKind::FEntry,
            FLOW_TCP_V6_CONNECT_ATTACH_POINT,
            ipv6_source,
            scope,
        ),
    ]
}

/// Baut die Programmbeschreibung, mit der ein echter `harw_dod_bpf::BpfLoader`
/// das eBPF-Programm dieser Crate laden würde.
///
/// # Description
/// Macht die Entscheidung „Tracepoint, kein roher Socket" (siehe Moduldoku)
/// als Code sichtbar und testbar: die Programmart ist fest
/// `harw_dod_bpf::BpfProgramKind::Tracepoint`, der Anknüpfungspunkt fest
/// [`FLOW_TCP_V4_CONNECT_ATTACH_POINT`]. Nur die Rumpfquelle bleibt dem Aufrufer
/// überlassen — sie hängt davon ab, ob das aufrufende Binary den Bytecode
/// einbettet oder von einem Pfad lädt (siehe
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
/// use harw_dod_flow::flow_program_spec;
/// use harw_types::SensorId;
/// use std::borrow::Cow;
///
/// let spec = flow_program_spec(
///     SensorId::from_str("flow-0"),
///     BpfProgramSource::Embedded(Cow::Borrowed(b"\0asm".as_slice())),
/// );
/// assert_eq!(spec.kind, BpfProgramKind::FEntry);
/// assert_eq!(spec.attach_point, "tcp_v4_connect");
/// ```
#[must_use]
pub fn flow_program_spec(
    sensor: harw_types::SensorId,
    source: harw_dod_bpf::BpfProgramSource,
) -> harw_dod_bpf::BpfProgramSpec {
    harw_dod_bpf::BpfProgramSpec::new(
        sensor,
        harw_dod_bpf::BpfProgramKind::FEntry,
        FLOW_TCP_V4_CONNECT_ATTACH_POINT,
        source,
    )
}

#[cfg(test)]
mod tests {
    use super::{flow_contracts, flow_program_spec, FLOW_TCP_V4_CONNECT_ATTACH_POINT, REQUIRED_CAPABILITY};

    #[test]
    fn test_required_capability_is_load_bpf_program_in_the_bpf_class() {
        assert_eq!(REQUIRED_CAPABILITY, harw_dod_cap::Capability::LoadBpfProgram);
        assert_eq!(REQUIRED_CAPABILITY.class(), harw_dod_cap::CapabilityClass::Bpf);
    }

    #[test]
    fn test_flow_program_spec_uses_an_fentry_and_needs_no_open_socket() {
        let dir = tempfile::tempdir().expect("tempdir for program body");
        let body_path = dir.path().join("flow.bpf.o");
        std::fs::write(&body_path, b"bytecode-bytes").expect("write fixture program body");

        let spec = flow_program_spec(
            harw_types::SensorId::from_str("flow-0"),
            harw_dod_bpf::BpfProgramSource::Path(body_path),
        );

        assert_eq!(spec.kind, harw_dod_bpf::BpfProgramKind::FEntry);
        assert_eq!(spec.attach_point, FLOW_TCP_V4_CONNECT_ATTACH_POINT);
        assert_eq!(spec.attach_point, "tcp_v4_connect");
    }

    #[test]
    fn v1_contracts_require_separate_v4_and_v6_task_context_hooks() {
        use std::borrow::Cow;

        let contracts = flow_contracts(
            harw_types::SensorId::from_str("flow-0"),
            harw_dod_bpf::BpfProgramSource::Embedded(Cow::Borrowed(b"v4")),
            harw_dod_bpf::BpfProgramSource::Embedded(Cow::Borrowed(b"v6")),
            harw_dod_bpf::BpfScope::Host,
        );
        assert_eq!(contracts[0].program_name, "dod_tcp_v4_connect");
        assert_eq!(contracts[1].program_name, "dod_tcp_v6_connect");
    }

    /// Hält die Entscheidung „kein `aya` in dieser Crate" strukturell fest,
    /// analog zu `harw-dod-bpf`.
    #[test]
    fn test_cargo_toml_declares_no_aya_dependency() {
        let manifest = include_str!("../Cargo.toml");
        assert!(
            !manifest.contains("aya"),
            "harw-dod-flow baut ausschließlich gegen den BpfLoader-Trait, nie gegen eine echte Bindung"
        );
    }
}
