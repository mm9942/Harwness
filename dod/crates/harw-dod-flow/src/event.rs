//! Formungsteil: der art-spezifische `payload`-Rest eines Verbindungsereignisses.
//!
//! # Verantwortungsbereich
//! Wie `harw_dod_bpf::event` ist dieses Modul reine Funktionen auf `&[u8]` —
//! **kein Kernel, keine Berechtigung, keine eBPF-Toolchain** nötig, um es zu
//! testen. [`harw_dod_bpf::event::parse_raw_event`] deutet bereits den
//! gemeinsamen, 28 Byte langen Kopf jedes Ereignisses (`pid`, Zeitstempel,
//! `comm`) und liefert den Rest als [`harw_dod_bpf::RawBpfEvent::payload`] —
//! genau dieser Rest ist die Eingabe von [`parse_flow_payload`].
//!
//! # Das Flow-Payload-Layout
//! Fest, 32 Byte lang, **unabhängig von der Adressfamilie** — siehe unten,
//! warum die konstante Länge kein Zufall, sondern der ganze Witz des
//! Familienfeldes ist:
//!
//! | Byte-Bereich | Feld           | Deutung                                    |
//! |-------------:|----------------|---------------------------------------------|
//! | `0..4`        | `pid`          | `u32`, Little-Endian                       |
//! | `4..8`        | `uid`          | `u32`, Little-Endian                       |
//! | `8`           | `protocol`     | `u8`: `0` = TCP, `1` = UDP                 |
//! | `9`           | `direction`    | `u8`: `0` = eingehend, `1` = ausgehend     |
//! | `10`          | `family`       | `u8`: `0` = IPv4, `1` = IPv6                |
//! | `11`          | *(Füllbyte)*   | Ausrichtung auf eine 2-Byte-Grenze für `remote_port` — nie gelesen |
//! | `12..14`      | `remote_port`  | `u16`, **Network Byte Order (Big-Endian)** |
//! | `14..16`      | *(Füllbytes)*  | Ausrichtung des Adressfeldes — nie gelesen |
//! | `16..32`      | `remote_addr`  | 16 Byte, roh; bei `family = 0` sind nur die ersten 4 Byte die Adresse, der Rest ist Füllung |
//!
//! Zwei Fallen, wörtlich aus dem Auftrag:
//!
//! 1. **Byte-Reihenfolge.** `pid`/`uid` sind Little-Endian wie der Rest des
//!    gemeinsamen Kopfes — dafür wird
//!    [`harw_dod_bpf::event::read_u32_le`] wiederverwendet, statt eine
//!    eigene Leseroutine zu schreiben. `remote_port` dagegen ist **Network
//!    Byte Order (Big-Endian)** — die klassische Verwechslung an genau
//!    dieser Stelle. `harw-dod-bpf` bietet dafür keinen Leser an (dort gibt
//!    es nur `_le`-Funktionen), deshalb liest [`read_port_be`] die zwei
//!    Bytes selbst, statt sie fälschlich durch `read_u32_le` zu jagen. Der
//!    Test [`tests::test_parse_flow_payload_reads_port_in_network_byte_order`]
//!    verwendet einen Port über 255, damit eine vertauschte Reihenfolge
//!    nicht zufällig unentdeckt bliebe.
//! 2. **Familienfeld statt Länge.** Das Adressfeld ist **immer** 16 Byte
//!    breit, unabhängig davon, ob `family` IPv4 oder IPv6 bedeutet — die
//!    Gesamtlänge des Payloads unterscheidet sich also **nicht** zwischen
//!    beiden Fällen. Genau das macht das Familienfeld unumgänglich, statt
//!    nur eine Redundanz zu sein: ein Leser, der stattdessen aus der Länge
//!    raten wollte, hätte in diesem Layout gar keine Information, aus der er
//!    raten könnte. [`parse_remote_addr`] liest deshalb ausschließlich das
//!    `family`-Byte, nie `payload.len()`.
//!
//! # Offsets, nicht Summen
//! Die Offset-Konstanten unten sind **einzeln benannt**, nicht durch
//! fortlaufendes Addieren von Feldbreiten hergeleitet — der Kernel richtet
//! Felder aus (Füllbytes bei `11` und `14..16`), und eine Implementierung,
//! die Breiten stattdessen aufsummiert, würde diese Füllbytes verschlucken
//! und ab `remote_port` versehentlich verschobene Werte lesen.
//!
//! # Fehler
//! [`crate::error::FlowError::MalformedEvent`] für jeden zu kurzen Puffer
//! oder jeden Protokoll-/Richtungs-/Familienwert außerhalb der bekannten
//! Menge — **inhaltsfrei**, siehe [`crate::error::FlowError`].
//!
//! # Keine Nutzdaten
//! [`FlowEvent`] hat keinerlei Feld, das beliebig lange Verbindungsinhalte
//! aufnehmen könnte — nur die sechs hier gelisteten, breitenfesten
//! Metadatenfelder. Bytes jenseits von Offset `32` (etwa weil ein
//! erzeugendes eBPF-Programm zusätzliche, hier nicht vorgesehene Daten
//! anhängt) werden von [`parse_flow_payload`] nie gelesen und tauchen damit
//! strukturell nirgends im Ergebnis auf — siehe
//! [`tests::test_parse_flow_payload_ignores_bytes_beyond_the_fixed_layout`].
//!
//! # Nebenläufigkeit
//! Alle Funktionen sind zustandslos und aus beliebig vielen Threads sicher
//! aufrufbar.
//!
//! # Examples
//! ```rust
//! use harw_dod_flow::event::{parse_flow_payload, Direction, Protocol};
//!
//! let mut payload = vec![0u8; 32];
//! payload[0..4].copy_from_slice(&4_242u32.to_le_bytes()); // pid
//! payload[4..8].copy_from_slice(&1_000u32.to_le_bytes()); // uid
//! payload[8] = 0; // TCP
//! payload[9] = 1; // ausgehend
//! payload[10] = 0; // IPv4
//! payload[12..14].copy_from_slice(&443u16.to_be_bytes()); // Network Byte Order
//! payload[16..20].copy_from_slice(&[198, 51, 100, 7]);
//!
//! let event = parse_flow_payload(&payload).expect("well-formed fixture payload");
//! assert_eq!(event.protocol, Protocol::Tcp);
//! assert_eq!(event.direction, Direction::Outbound);
//! assert_eq!(event.remote_port, 443);
//! assert_eq!(event.remote_addr.to_string(), "198.51.100.7");
//! ```

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use harw_dod_bpf::event::read_u32_le;
use harw_dod_bpf::{TaskIdentity, WireEvent, WireEventType};

use crate::error::FlowError;

/// Offset des `pid`-Feldes (`u32`, LE).
const PID_OFFSET: usize = 0;
/// Offset des `uid`-Feldes (`u32`, LE).
const UID_OFFSET: usize = 4;
/// Offset des `protocol`-Feldes (`u8`).
const PROTOCOL_OFFSET: usize = 8;
/// Offset des `direction`-Feldes (`u8`).
const DIRECTION_OFFSET: usize = 9;
/// Offset des `family`-Feldes (`u8`). Byte `11` ist Füllung und wird nie
/// gelesen.
const FAMILY_OFFSET: usize = 10;
/// Offset des `remote_port`-Feldes (`u16`, **Big-Endian**). Bytes `14..16`
/// sind Füllung und werden nie gelesen.
const PORT_OFFSET: usize = 12;
/// Offset des 16 Byte breiten Adressfeldes.
const ADDR_OFFSET: usize = 16;
/// Anzahl der für IPv4 tatsächlich gültigen Adress-Bytes.
const ADDR_V4_LEN: usize = 4;
/// Breite des vollständigen Adressfeldes (auch für IPv4, siehe Moduldoku).
const ADDR_FIELD_LEN: usize = 16;
/// Feste Gesamtlänge eines Flow-Payloads.
const PAYLOAD_LEN: usize = ADDR_OFFSET + ADDR_FIELD_LEN;

/// Übertragungsprotokoll eines beobachteten Verbindungsereignisses.
///
/// # Description
/// Geschlossen: ein unbekannter Protokollwert im Payload ist
/// [`crate::error::FlowError::MalformedEvent`], keine dritte Variante.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// TCP (verbindungsorientiert).
    Tcp,
    /// UDP (verbindungslos).
    Udp,
}

impl Protocol {
    /// Deutet das `protocol`-Byte des Flow-Payloads.
    ///
    /// # Arguments
    /// - `byte` (`u8`): `0` für TCP, `1` für UDP.
    ///
    /// # Returns
    /// Das gedeutete [`Protocol`].
    ///
    /// # Errors
    /// - [`crate::error::FlowError::MalformedEvent`] für jeden anderen Wert.
    fn from_byte(byte: u8) -> Result<Self, FlowError> {
        match byte {
            0 => Ok(Self::Tcp),
            1 => Ok(Self::Udp),
            _ => Err(FlowError::MalformedEvent),
        }
    }
}

/// Richtung eines beobachteten Verbindungsereignisses.
///
/// # Description
/// Geschlossen. Nur [`Self::Outbound`]-Ereignisse sind für
/// [`crate::report::to_security_event`] überhaupt Kandidaten für eine
/// Meldung als `harw_dod_signals::EventKind::EgressFlow` — siehe
/// [`crate::report`]-Moduldoku für die vollständige Melderegel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Eingehende Verbindung.
    Inbound,
    /// Ausgehende Verbindung.
    Outbound,
}

impl Direction {
    /// Deutet das `direction`-Byte des Flow-Payloads.
    ///
    /// # Arguments
    /// - `byte` (`u8`): `0` für eingehend, `1` für ausgehend.
    ///
    /// # Returns
    /// Die gedeutete [`Direction`].
    ///
    /// # Errors
    /// - [`crate::error::FlowError::MalformedEvent`] für jeden anderen Wert.
    fn from_byte(byte: u8) -> Result<Self, FlowError> {
        match byte {
            0 => Ok(Self::Inbound),
            1 => Ok(Self::Outbound),
            _ => Err(FlowError::MalformedEvent),
        }
    }
}

/// Ein aus dem Flow-Payload gedeutetes Verbindungsereignis.
///
/// # Description
/// Trägt ausschließlich Metadaten — siehe Moduldoku, Abschnitt „Keine
/// Nutzdaten". `pid` bleibt erhalten, obwohl
/// [`crate::report::to_security_event`] es aktuell nicht in die erzeugte
/// `harw_dod_signals::SecurityEvent` übernimmt (weder `EventKind::EgressFlow`
/// noch `Actor` haben ein `pid`-Feld): ein künftiger Konsument, der den
/// vollständigen geparsten Datensatz statt der bereits reduzierten
/// `SecurityEvent`-Form braucht, findet ihn hier vollständig vor.
///
/// # Errors
/// Keine eigenen Fehler — entsteht ausschließlich über
/// [`parse_flow_payload`].
///
/// # Examples
/// ```rust
/// use harw_dod_flow::event::{Direction, FlowEvent, Protocol};
/// use std::net::{IpAddr, Ipv4Addr};
///
/// let event = FlowEvent {
///     pid: 1,
///     uid: 0,
///     protocol: Protocol::Tcp,
///     direction: Direction::Outbound,
///     remote_addr: IpAddr::V4(Ipv4Addr::new(198, 51, 100, 7)),
///     remote_port: 443,
/// };
/// assert_eq!(event.remote_port, 443);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlowEvent {
    /// Die Prozess-ID, die das erzeugende eBPF-Programm für diese
    /// Verbindung beobachtet hat.
    pub pid: u32,
    /// Die Ausführungs-UID des Prozesses zum Zeitpunkt der Beobachtung.
    pub uid: u32,
    /// Das Übertragungsprotokoll.
    pub protocol: Protocol,
    /// Eingehend oder ausgehend.
    pub direction: Direction,
    /// Die Gegenstelle der Verbindung.
    pub remote_addr: IpAddr,
    /// Der Port der Gegenstelle, bereits in Host-Reihenfolge (aus Network
    /// Byte Order gewandelt).
    pub remote_port: u16,
}

/// A TCP connect emitted at the initiating task's connect hook.  The v1 body
/// is `family:u8 | port:u16-be | address:[u8;16]`; it has no direction or
/// protocol byte because both are fixed by the program contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcpConnectEventV1 {
    pub task: TaskIdentity,
    pub sequence: u64,
    pub remote_addr: IpAddr,
    pub remote_port: u16,
}

pub fn parse_tcp_connect_v1(event: &WireEvent) -> Result<TcpConnectEventV1, FlowError> {
    if event.event_type != WireEventType::TcpConnect || event.flags != 0 || event.payload.len() != 19 {
        return Err(FlowError::MalformedEvent);
    }
    let family = event.payload[0];
    let remote_port = u16::from_be_bytes(event.payload[1..3].try_into().map_err(|_| FlowError::MalformedEvent)?);
    let addr: [u8; 16] = event.payload[3..19].try_into().map_err(|_| FlowError::MalformedEvent)?;
    let remote_addr = match family {
        4 if addr[4..].iter().all(|byte| *byte == 0) => IpAddr::V4(Ipv4Addr::from([addr[0], addr[1], addr[2], addr[3]])),
        6 => IpAddr::V6(Ipv6Addr::from(addr)),
        _ => return Err(FlowError::MalformedEvent),
    };
    Ok(TcpConnectEventV1 { task: event.task, sequence: event.sequence, remote_addr, remote_port })
}

/// Liest den Port aus dem Flow-Payload — **Network Byte Order (Big-Endian)**,
/// anders als der Rest des Layouts.
///
/// # Description
/// `harw-dod-bpf` bietet ausschließlich `_le`-Leser an (siehe Moduldoku,
/// Falle 1); für dieses eine, bewusst gegenläufige Feld schreibt diese Crate
/// deshalb ihre eigene, bounds-geprüfte Leseroutine, statt `read_u32_le`
/// fälschlich auf zwei Bytes anzuwenden.
///
/// # Arguments
/// - `bytes` (`&[u8]`): der Payload-Puffer.
/// - `offset` (`usize`): die Startposition des 2-Byte-Feldes.
///
/// # Returns
/// Den gelesenen `u16`-Wert, bereits in Host-Reihenfolge.
///
/// # Errors
/// - [`FlowError::MalformedEvent`], wenn `offset + 2` außerhalb von `bytes`
///   liegt.
fn read_port_be(bytes: &[u8], offset: usize) -> Result<u16, FlowError> {
    let slice = bytes
        .get(offset..offset + 2)
        .ok_or(FlowError::MalformedEvent)?;
    let array: [u8; 2] = slice.try_into().map_err(|_| FlowError::MalformedEvent)?;
    Ok(u16::from_be_bytes(array))
}

/// Liest die Gegenstellen-Adresse anhand des Familienfeldes — nie anhand der
/// Puffergröße (siehe Moduldoku, Falle 2).
///
/// # Arguments
/// - `bytes` (`&[u8]`): der Payload-Puffer.
/// - `family_byte` (`u8`): `0` für IPv4 (liest die ersten 4 der 16
///   Adress-Bytes), `1` für IPv6 (liest alle 16).
///
/// # Returns
/// Die gedeutete [`IpAddr`].
///
/// # Errors
/// - [`FlowError::MalformedEvent`] für jeden anderen Familienwert oder einen
///   zu kurzen Puffer.
fn parse_remote_addr(bytes: &[u8], family_byte: u8) -> Result<IpAddr, FlowError> {
    match family_byte {
        0 => {
            let slice = bytes
                .get(ADDR_OFFSET..ADDR_OFFSET + ADDR_V4_LEN)
                .ok_or(FlowError::MalformedEvent)?;
            let octets: [u8; 4] = slice.try_into().map_err(|_| FlowError::MalformedEvent)?;
            Ok(IpAddr::V4(Ipv4Addr::from(octets)))
        }
        1 => {
            let slice = bytes
                .get(ADDR_OFFSET..ADDR_OFFSET + ADDR_FIELD_LEN)
                .ok_or(FlowError::MalformedEvent)?;
            let octets: [u8; 16] = slice.try_into().map_err(|_| FlowError::MalformedEvent)?;
            Ok(IpAddr::V6(Ipv6Addr::from(octets)))
        }
        _ => Err(FlowError::MalformedEvent),
    }
}

/// Deutet den art-spezifischen `payload`-Rest eines Verbindungsereignisses.
///
/// # Description
/// Prüft zuerst die feste Mindestlänge ([`PAYLOAD_LEN`], 32 Byte); ein
/// kürzerer Puffer erzeugt sofort [`FlowError::MalformedEvent`], ohne die
/// vorhandenen Bytes weiter anzufassen. Bytes jenseits von [`PAYLOAD_LEN`]
/// werden nie gelesen (siehe Moduldoku, „Keine Nutzdaten"). Reine Funktion,
/// kein Kernel, keine Berechtigung.
///
/// # Arguments
/// - `payload` (`&[u8]`): der art-spezifische Rest eines
///   `harw_dod_bpf::RawBpfEvent::payload` (nach dem bereits von
///   `harw_dod_bpf::event::parse_raw_event` gedeuteten 28-Byte-Kopf).
///
/// # Returns
/// Das gedeutete [`FlowEvent`].
///
/// # Errors
/// - [`FlowError::MalformedEvent`], wenn `payload` kürzer als [`PAYLOAD_LEN`]
///   ist oder ein Protokoll-/Richtungs-/Familienbyte außerhalb der bekannten
///   Werte trägt. **Inhaltsfrei:** die Fehlermeldung nennt nie die Länge
///   oder den Inhalt von `payload`.
///
/// # Examples
/// ```rust
/// use harw_dod_flow::error::FlowError;
/// use harw_dod_flow::event::parse_flow_payload;
///
/// assert!(matches!(parse_flow_payload(&[0u8; 4]), Err(FlowError::MalformedEvent)));
/// ```
pub fn parse_flow_payload(payload: &[u8]) -> Result<FlowEvent, FlowError> {
    if payload.len() < PAYLOAD_LEN {
        return Err(FlowError::MalformedEvent);
    }

    let pid = read_u32_le(payload, PID_OFFSET).map_err(|_| FlowError::MalformedEvent)?;
    let uid = read_u32_le(payload, UID_OFFSET).map_err(|_| FlowError::MalformedEvent)?;
    let protocol_byte = *payload.get(PROTOCOL_OFFSET).ok_or(FlowError::MalformedEvent)?;
    let direction_byte = *payload.get(DIRECTION_OFFSET).ok_or(FlowError::MalformedEvent)?;
    let family_byte = *payload.get(FAMILY_OFFSET).ok_or(FlowError::MalformedEvent)?;

    let protocol = Protocol::from_byte(protocol_byte)?;
    let direction = Direction::from_byte(direction_byte)?;
    let remote_port = read_port_be(payload, PORT_OFFSET)?;
    let remote_addr = parse_remote_addr(payload, family_byte)?;

    Ok(FlowEvent {
        pid,
        uid,
        protocol,
        direction,
        remote_addr,
        remote_port,
    })
}

#[cfg(test)]
mod tests {
    use super::{Direction, Protocol, ADDR_OFFSET, PAYLOAD_LEN};
    use crate::error::FlowError;
    use crate::event::parse_flow_payload;
    use harw_dod_bpf::{TaskIdentity, WireEvent, WireEventType};

    #[test]
    fn v1_tcp_connect_keeps_initiating_task_and_ipv6_port() {
        let mut payload = vec![6, 0x20, 0xfb];
        payload.extend_from_slice(&Ipv6Addr::LOCALHOST.octets());
        let event = WireEvent { event_type: WireEventType::TcpConnect, flags: 0, ktime_ns: 1, sequence: 1, task: TaskIdentity { tgid: 9, pid: 10, ppid: 8, uid: 1000, cgroup_id: 77 }, payload };
        let parsed = super::parse_tcp_connect_v1(&event).unwrap();
        assert_eq!(parsed.task.cgroup_id, 77);
        assert_eq!(parsed.sequence, 1);
        assert_eq!(parsed.remote_port, 8443);
        assert_eq!(parsed.remote_addr, IpAddr::V6(Ipv6Addr::LOCALHOST));
    }

    /// Baut einen wohlgeformten 32-Byte-Payload von Hand, ohne die
    /// Produktionsfunktion selbst zu wiederholen.
    fn well_formed_payload(
        pid: u32,
        uid: u32,
        protocol_byte: u8,
        direction_byte: u8,
        family_byte: u8,
        port: u16,
        addr_bytes: &[u8],
    ) -> Vec<u8> {
        let mut bytes = vec![0u8; PAYLOAD_LEN];
        bytes[0..4].copy_from_slice(&pid.to_le_bytes());
        bytes[4..8].copy_from_slice(&uid.to_le_bytes());
        bytes[8] = protocol_byte;
        bytes[9] = direction_byte;
        bytes[10] = family_byte;
        // Byte 11 bleibt Füllung (0), wie im echten Layout.
        bytes[12..14].copy_from_slice(&port.to_be_bytes());
        // Bytes 14..16 bleiben Füllung (0).
        bytes[ADDR_OFFSET..ADDR_OFFSET + addr_bytes.len()].copy_from_slice(addr_bytes);
        bytes
    }

    #[test]
    fn test_parse_flow_payload_decodes_a_well_formed_ipv4_tcp_outbound_payload() {
        let bytes = well_formed_payload(4_242, 1_000, 0, 1, 0, 443, &[198, 51, 100, 7]);

        let event = parse_flow_payload(&bytes).expect("well-formed payload must parse");
        assert_eq!(event.pid, 4_242);
        assert_eq!(event.uid, 1_000);
        assert_eq!(event.protocol, Protocol::Tcp);
        assert_eq!(event.direction, Direction::Outbound);
        assert_eq!(event.remote_port, 443);
        assert_eq!(event.remote_addr.to_string(), "198.51.100.7");
    }

    #[test]
    fn test_parse_flow_payload_reads_port_in_network_byte_order() {
        // 4444 = 0x115C — über 255, damit eine vertauschte Byte-Reihenfolge
        // (die LE-Deutung ergäbe 0x5C11 = 23569) nicht unentdeckt bliebe.
        let bytes = well_formed_payload(1, 0, 1, 1, 0, 4_444, &[10, 0, 0, 1]);
        let event = parse_flow_payload(&bytes).expect("well-formed payload must parse");
        assert_eq!(event.remote_port, 4_444);
        assert_eq!(event.protocol, Protocol::Udp);
    }

    #[test]
    fn test_parse_flow_payload_distinguishes_ipv4_and_ipv6_by_family_byte_not_length() {
        // Beide Puffer haben exakt dieselbe Länge (PAYLOAD_LEN) — der
        // einzige Unterschied ist das Familienbyte an Offset 10.
        let v4 = well_formed_payload(1, 0, 0, 1, 0, 80, &[192, 0, 2, 1]);
        let v6 = well_formed_payload(
            1,
            0,
            0,
            1,
            1,
            80,
            &[
                0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1,
            ],
        );
        assert_eq!(v4.len(), v6.len(), "beide Puffer müssen gleich lang sein");

        let v4_event = parse_flow_payload(&v4).expect("ipv4 payload must parse");
        let v6_event = parse_flow_payload(&v6).expect("ipv6 payload must parse");

        assert!(v4_event.remote_addr.is_ipv4());
        assert_eq!(v4_event.remote_addr.to_string(), "192.0.2.1");
        assert!(v6_event.remote_addr.is_ipv6());
        assert_eq!(v6_event.remote_addr.to_string(), "2001:db8::1");
    }

    #[test]
    fn test_parse_flow_payload_ignores_bytes_beyond_the_fixed_layout() {
        let mut bytes = well_formed_payload(1, 0, 0, 1, 0, 22, &[203, 0, 113, 5]);
        // Simuliert ein erzeugendes Programm, das (fälschlich oder nicht)
        // zusätzliche Bytes anhängt — etwa mitgeschnittene Verbindungsdaten.
        bytes.extend_from_slice(b"HARW-FLOW-PAYLOAD-CANARY-CONTENT");

        let event = parse_flow_payload(&bytes).expect("payload with trailing bytes must still parse");
        let rendered = format!("{event:?}");
        assert!(!rendered.contains("HARW-FLOW-PAYLOAD-CANARY-CONTENT"));
    }

    #[test]
    fn test_parse_flow_payload_too_short_buffer_returns_malformed_event_without_panicking() {
        let err = parse_flow_payload(&[1, 2, 3]).expect_err("a 3-byte buffer is far too short");
        assert!(matches!(err, FlowError::MalformedEvent));
        // Inhaltsfrei: die Meldung ist ein fester String, kann die Rohbytes
        // strukturell nicht enthalten.
        assert_eq!(err.to_string(), "flow event payload is malformed");
    }

    #[test]
    fn test_parse_flow_payload_empty_buffer_returns_malformed_event_without_panicking() {
        let err = parse_flow_payload(&[]).expect_err("an empty buffer must not panic");
        assert!(matches!(err, FlowError::MalformedEvent));
    }

    #[test]
    fn test_parse_flow_payload_unknown_protocol_byte_returns_malformed_event() {
        let bytes = well_formed_payload(1, 0, 9, 1, 0, 22, &[127, 0, 0, 1]);
        let err = parse_flow_payload(&bytes).expect_err("protocol byte 9 is unknown");
        assert!(matches!(err, FlowError::MalformedEvent));
    }

    #[test]
    fn test_parse_flow_payload_unknown_family_byte_returns_malformed_event() {
        let bytes = well_formed_payload(1, 0, 0, 1, 9, 22, &[127, 0, 0, 1]);
        let err = parse_flow_payload(&bytes).expect_err("family byte 9 is unknown");
        assert!(matches!(err, FlowError::MalformedEvent));
    }
}
