//! Formungsteil: rohe Ereignisbytes aus dem Ringpuffer deuten.
//!
//! # Verantwortungsbereich
//! Der gesamte Inhalt dieses Moduls sind reine Funktionen auf `&[u8]` —
//! **kein Kernel, keine Berechtigung, keine eBPF-Toolchain** nötig, um ihn
//! zu testen. Hier soll laut Auftrag der größte Teil des Codes dieser Crate
//! liegen, und hier liegt er: Feldbreiten, Byte-Reihenfolge (kleinteilig,
//! `_le`-Suffix macht das explizit), Zeichenketten fester Länge mit
//! optionaler Nullterminierung.
//!
//! # Das gemeinsame Kopf-Layout
//! [`parse_raw_event`] deutet jeden Ereignispuffer nach einem festen,
//! 28 Byte langen Kopf, gefolgt von einem art-spezifischen Rest, den diese
//! Crate nicht kennt:
//!
//! | Byte-Bereich | Feld           | Deutung                                  |
//! |-------------:|----------------|-------------------------------------------|
//! | `0..4`        | `pid`          | `u32`, Little-Endian                     |
//! | `4..12`       | Zeitstempel    | `u64`, Little-Endian, Nanosekunden seit der Unix-Epoche |
//! | `12..28`      | `comm`         | 16 Byte, nullterminiert oder pufferfüllend |
//! | `28..`        | `payload`      | roh, art-spezifisch (Sache von `harw-dod-procmon`/`harw-dod-flow`) |
//!
//! Die Zeitstempel-Deutung setzt voraus, dass das erzeugende eBPF-Programm
//! bereits Unix-Epoche-Nanosekunden schreibt (z. B. über einen vom
//! Nutzerraum injizierten Zeit-Offset zu `bpf_ktime_get_ns()`, nicht über
//! `bpf_ktime_get_ns()` allein — dessen Nullpunkt ist der Systemstart, nicht
//! die Epoche). Diese Crate erzwingt das nicht; sie dokumentiert nur die
//! Annahme, unter der [`RawBpfEvent::observed_at`] sinnvoll ist.
//!
//! Ein `pid`/`comm` ohne Sinn (z. B. bei [`crate::spec::BpfProgramKind::SocketFilter`],
//! wo kein Task-Kontext existiert) wird von der erzeugenden Seite als `0`
//! bzw. leerer String geschrieben — diese Crate trifft dazu keine
//! Annahme, sie liest nur, was dasteht.
//!
//! Die drei exportierten Grundfunktionen [`read_u32_le`], [`read_u64_le`],
//! [`read_fixed_c_str`] sind bewusst wiederverwendbar: `harw-dod-procmon`
//! und `harw-dod-flow` deuten den art-spezifischen `payload`-Rest mit
//! denselben Grundfunktionen, statt sie zu duplizieren.
//!
//! # Exportierte Typen und Funktionen
//! [`RawBpfEvent`], [`parse_raw_event`], [`read_u32_le`], [`read_u64_le`],
//! [`read_fixed_c_str`].
//!
//! # Nebenläufigkeit
//! Alle Funktionen sind zustandslos und `Send + Sync`-frei nutzbar aus
//! beliebig vielen Threads.
//!
//! # Fehler
//! [`crate::error::BpfError::MalformedEvent`] für jeden zu kurzen Puffer
//! oder jeden nicht darstellbaren Zeitstempel — **inhaltsfrei**, siehe
//! [`crate::error::BpfError`].
//!
//! # Examples
//! ```rust
//! use harw_dod_bpf::event::parse_raw_event;
//!
//! let mut bytes = Vec::new();
//! bytes.extend_from_slice(&1_234u32.to_le_bytes()); // pid
//! bytes.extend_from_slice(&0u64.to_le_bytes()); // Zeitstempel: Unix-Epoche
//! bytes.extend_from_slice(b"sshd\0\0\0\0\0\0\0\0\0\0\0\0"); // comm, 16 Byte
//! bytes.extend_from_slice(b"payload");
//!
//! let event = parse_raw_event(&bytes).expect("well-formed fixture buffer");
//! assert_eq!(event.pid, 1_234);
//! assert_eq!(event.comm, "sshd");
//! assert_eq!(event.payload, b"payload");
//! ```

use crate::error::BpfError;

/// Länge des gemeinsamen Kopfes: 4 (`pid`) + 8 (Zeitstempel) + 16 (`comm`).
const HEADER_LEN: usize = 28;
/// Feste Breite des `comm`-Feldes (Linux' `TASK_COMM_LEN`).
const COMM_LEN: usize = 16;

/// Liest ein `u32` in Little-Endian-Reihenfolge aus `bytes` ab `offset`.
///
/// # Description
/// Reine Funktion, keine Dateisystem- oder Kernel-Interaktion. Prüft die
/// Puffergrenzen selbst — Aufrufer müssen die Länge nicht vorab prüfen.
///
/// # Arguments
/// - `bytes` (`&[u8]`): der zu lesende Puffer.
/// - `offset` (`usize`): die Startposition des 4-Byte-Feldes.
///
/// # Returns
/// Den gelesenen `u32`-Wert.
///
/// # Errors
/// - [`BpfError::MalformedEvent`], wenn `offset + 4` außerhalb von `bytes`
///   liegt oder überläuft.
///
/// # Examples
/// ```rust
/// use harw_dod_bpf::event::read_u32_le;
///
/// let bytes = 300u32.to_le_bytes();
/// assert_eq!(read_u32_le(&bytes, 0).unwrap(), 300);
/// ```
pub fn read_u32_le(bytes: &[u8], offset: usize) -> Result<u32, BpfError> {
    let end = offset.checked_add(4).ok_or(BpfError::MalformedEvent)?;
    let slice = bytes.get(offset..end).ok_or(BpfError::MalformedEvent)?;
    let mut buf = [0u8; 4];
    buf.copy_from_slice(slice);
    Ok(u32::from_le_bytes(buf))
}

/// Liest ein `u64` in Little-Endian-Reihenfolge aus `bytes` ab `offset`.
///
/// # Description
/// Reine Funktion, siehe [`read_u32_le`] für das Muster.
///
/// # Arguments
/// - `bytes` (`&[u8]`): der zu lesende Puffer.
/// - `offset` (`usize`): die Startposition des 8-Byte-Feldes.
///
/// # Returns
/// Den gelesenen `u64`-Wert.
///
/// # Errors
/// - [`BpfError::MalformedEvent`], wenn `offset + 8` außerhalb von `bytes`
///   liegt oder überläuft.
///
/// # Examples
/// ```rust
/// use harw_dod_bpf::event::read_u64_le;
///
/// let bytes = 70_000u64.to_le_bytes();
/// assert_eq!(read_u64_le(&bytes, 0).unwrap(), 70_000);
/// ```
pub fn read_u64_le(bytes: &[u8], offset: usize) -> Result<u64, BpfError> {
    let end = offset.checked_add(8).ok_or(BpfError::MalformedEvent)?;
    let slice = bytes.get(offset..end).ok_or(BpfError::MalformedEvent)?;
    let mut buf = [0u8; 8];
    buf.copy_from_slice(slice);
    Ok(u64::from_le_bytes(buf))
}

/// Liest eine nullterminierte Zeichenkette fester Länge.
///
/// # Description
/// Deutet `len` Bytes ab `offset`. Enthält das Feld ein Nullbyte, endet die
/// Zeichenkette dort; enthält es keines, füllt sie das gesamte Feld — genau
/// das Verhalten von Linux' `TASK_COMM_LEN`-Feld, das bei maximaler Länge
/// keinen Terminator mehr trägt. Ungültige UTF-8-Sequenzen werden
/// verlustbehaftet ersetzt (`String::from_utf8_lossy`) statt einen Fehler auszulösen
/// oder zu jammern — Kernel-Bezeichner sind in der Praxis ASCII, aber diese
/// Funktion verlangt es nicht und darf dafür nie einen Panic auslösen.
///
/// # Arguments
/// - `bytes` (`&[u8]`): der zu lesende Puffer.
/// - `offset` (`usize`): die Startposition des Feldes.
/// - `len` (`usize`): die feste Breite des Feldes in Byte.
///
/// # Returns
/// Die gelesene Zeichenkette, ohne Nullterminator.
///
/// # Errors
/// - [`BpfError::MalformedEvent`], wenn `offset + len` außerhalb von
///   `bytes` liegt oder überläuft.
///
/// # Examples
/// ```rust
/// use harw_dod_bpf::event::read_fixed_c_str;
///
/// // Nullterminiert, mit Füllbytes danach:
/// assert_eq!(read_fixed_c_str(b"sshd\0\0\0\0", 0, 8).unwrap(), "sshd");
///
/// // Füllt den Puffer vollständig aus, ohne Terminator:
/// assert_eq!(read_fixed_c_str(b"exactly1", 0, 8).unwrap(), "exactly1");
/// ```
pub fn read_fixed_c_str(bytes: &[u8], offset: usize, len: usize) -> Result<String, BpfError> {
    let end = offset.checked_add(len).ok_or(BpfError::MalformedEvent)?;
    let slice = bytes.get(offset..end).ok_or(BpfError::MalformedEvent)?;
    let raw = match slice.iter().position(|&b| b == 0) {
        Some(nul_at) => &slice[..nul_at],
        None => slice,
    };
    Ok(String::from_utf8_lossy(raw).into_owned())
}

/// Ein aus dem Ringpuffer gedeutetes Rohereignis.
///
/// # Description
/// Trägt den gemeinsamen Kopf (`pid`, `comm`, Zeitstempel) entschlüsselt und
/// den art-spezifischen Rest roh in `payload`. Diese Crate trifft keine
/// Entscheidung darüber, was `payload` bedeutet — das ist Sache der
/// Sensor-Crate, die dieses Ereignis konsumiert.
///
/// # Errors
/// Keine eigenen Fehler — entsteht ausschließlich über [`parse_raw_event`].
///
/// # Examples
/// ```rust
/// use harw_dod_bpf::event::RawBpfEvent;
///
/// let event = RawBpfEvent {
///     pid: 1,
///     comm: "init".to_owned(),
///     observed_at: jiff::Timestamp::UNIX_EPOCH,
///     payload: vec![],
/// };
/// assert_eq!(event.pid, 1);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct RawBpfEvent {
    /// Die Prozess-ID, die das erzeugende eBPF-Programm beobachtet hat
    /// (`0`, wenn kein Task-Kontext existiert, z. B. bei
    /// [`crate::spec::BpfProgramKind::SocketFilter`]).
    pub pid: u32,
    /// Der Prozessname (`comm`), gelesen über [`read_fixed_c_str`].
    pub comm: String,
    /// Der Beobachtungszeitpunkt, unter der Annahme, dass das erzeugende
    /// Programm Unix-Epoche-Nanosekunden schreibt (siehe Moduldoku).
    pub observed_at: jiff::Timestamp,
    /// Der art-spezifische Rest des Puffers, roh und unausgewertet.
    pub payload: Vec<u8>,
}

/// Deutet einen rohen Ringpuffer-Eintrag als [`RawBpfEvent`].
///
/// # Description
/// Prüft zuerst die Mindestlänge ([`HEADER_LEN`] Byte); ein kürzerer Puffer
/// erzeugt sofort [`BpfError::MalformedEvent`], ohne die vorhandenen Bytes
/// weiter anzufassen. Reine Funktion, kein Kernel, keine Berechtigung.
///
/// # Arguments
/// - `bytes` (`&[u8]`): ein roher Ringpuffer-Eintrag, wie ihn
///   [`crate::loader::BpfLoader::read_events`] liefert.
///
/// # Returns
/// Das gedeutete [`RawBpfEvent`].
///
/// # Errors
/// - [`BpfError::MalformedEvent`], wenn `bytes` kürzer als der gemeinsame
///   Kopf ist oder der Zeitstempel außerhalb des darstellbaren Bereichs
///   liegt. **Inhaltsfrei:** die Fehlermeldung nennt nie die Länge oder den
///   Inhalt von `bytes`.
///
/// # Examples
/// ```rust
/// use harw_dod_bpf::error::BpfError;
/// use harw_dod_bpf::event::parse_raw_event;
///
/// assert!(matches!(parse_raw_event(&[0u8; 4]), Err(BpfError::MalformedEvent)));
/// ```
pub fn parse_raw_event(bytes: &[u8]) -> Result<RawBpfEvent, BpfError> {
    if bytes.len() < HEADER_LEN {
        return Err(BpfError::MalformedEvent);
    }

    let pid = read_u32_le(bytes, 0)?;
    let epoch_nanos = read_u64_le(bytes, 4)?;
    let comm = read_fixed_c_str(bytes, 12, COMM_LEN)?;
    let observed_at = jiff::Timestamp::from_nanosecond(i128::from(epoch_nanos))
        .map_err(|_| BpfError::MalformedEvent)?;
    let payload = bytes[HEADER_LEN..].to_vec();

    Ok(RawBpfEvent {
        pid,
        comm,
        observed_at,
        payload,
    })
}

#[cfg(test)]
mod tests {
    use super::{HEADER_LEN, parse_raw_event, read_fixed_c_str, read_u32_le, read_u64_le};
    use crate::error::BpfError;
    use crate::test_support::{TestError, TestResult, ctx};

    fn well_formed_buffer(pid: u32, epoch_nanos: u64, comm: &[u8; 16], payload: &[u8]) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(HEADER_LEN + payload.len());
        bytes.extend_from_slice(&pid.to_le_bytes());
        bytes.extend_from_slice(&epoch_nanos.to_le_bytes());
        bytes.extend_from_slice(comm);
        bytes.extend_from_slice(payload);
        bytes
    }

    #[test]
    fn test_read_u32_le_honors_byte_order_for_a_multi_byte_value() -> TestResult {
        // 300 braucht zwei Bytes (0x2C, 0x01) — ein Test mit einem Wert
        // unter 256 könnte eine vertauschte Byte-Reihenfolge nicht erkennen.
        let bytes = 300u32.to_le_bytes();
        assert_eq!(read_u32_le(&bytes, 0).map_err(ctx("u32 le lesen"))?, 300);
        Ok(())
    }

    #[test]
    fn test_read_u64_le_honors_byte_order_for_a_multi_byte_value() -> TestResult {
        let bytes = 70_000u64.to_le_bytes();
        assert_eq!(read_u64_le(&bytes, 0).map_err(ctx("u64 le lesen"))?, 70_000);
        Ok(())
    }

    #[test]
    fn test_read_u32_le_out_of_range_returns_malformed_event() -> TestResult {
        let outcome = read_u32_le(&[0u8; 2], 0);
        let Err(err) = outcome else {
            return Err(TestError::Unexpected(
                "2 bytes cannot hold a u32".to_owned(),
            ));
        };
        assert!(matches!(err, BpfError::MalformedEvent));
        Ok(())
    }

    #[test]
    fn test_read_fixed_c_str_stops_at_nul_terminator() -> TestResult {
        assert_eq!(
            read_fixed_c_str(b"sshd\0\0\0\0", 0, 8).map_err(ctx("fixed c-str lesen"))?,
            "sshd"
        );
        Ok(())
    }

    #[test]
    fn test_read_fixed_c_str_fills_buffer_completely_without_terminator() -> TestResult {
        // 8 Byte, keine Null irgendwo darin: die gesamte Breite ist der Name.
        assert_eq!(
            read_fixed_c_str(b"exactly1", 0, 8).map_err(ctx("fixed c-str lesen"))?,
            "exactly1"
        );
        Ok(())
    }

    #[test]
    fn test_read_fixed_c_str_out_of_range_returns_malformed_event() -> TestResult {
        let outcome = read_fixed_c_str(b"short", 0, 16);
        let Err(err) = outcome else {
            return Err(TestError::Unexpected(
                "buffer shorter than the field".to_owned(),
            ));
        };
        assert!(matches!(err, BpfError::MalformedEvent));
        Ok(())
    }

    #[test]
    fn test_parse_raw_event_decodes_a_well_formed_buffer() -> TestResult {
        let mut comm = [0u8; 16];
        comm[..4].copy_from_slice(b"sshd");
        let bytes = well_formed_buffer(4_242, 1_000_000_000, &comm, b"rest");

        let event = parse_raw_event(&bytes).map_err(ctx("well-formed buffer must parse"))?;
        assert_eq!(event.pid, 4_242);
        assert_eq!(event.comm, "sshd");
        assert_eq!(
            event.observed_at,
            jiff::Timestamp::from_nanosecond(1_000_000_000)
                .map_err(ctx("timestamp from nanosecond"))?
        );
        assert_eq!(event.payload, b"rest");
        Ok(())
    }

    #[test]
    fn test_parse_raw_event_too_short_buffer_returns_malformed_event_without_panicking()
    -> TestResult {
        let outcome = parse_raw_event(&[1, 2, 3]);
        let Err(err) = outcome else {
            return Err(TestError::Unexpected(
                "a 3-byte buffer is far too short".to_owned(),
            ));
        };
        assert!(matches!(err, BpfError::MalformedEvent));
        // Inhaltsfrei: die Meldung ist ein fester String, kann die Rohbytes
        // strukturell nicht enthalten.
        assert_eq!(err.to_string(), "bpf event buffer is malformed");
        Ok(())
    }

    #[test]
    fn test_parse_raw_event_empty_buffer_returns_malformed_event_without_panicking() -> TestResult {
        let outcome = parse_raw_event(&[]);
        let Err(err) = outcome else {
            return Err(TestError::Unexpected(
                "an empty buffer must not panic".to_owned(),
            ));
        };
        assert!(matches!(err, BpfError::MalformedEvent));
        Ok(())
    }
}
