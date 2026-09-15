//! Netlink-Rahmenzerlegung: einen Byte-Puffer in einzelne Nachrichten teilen.
//!
//! # Verantwortungsbereich
//! Ein `recv()` auf einem `AF_NETLINK`-Socket kann mehrere `nlmsghdr`-
//! gerahmte Nachrichten in einem einzigen Puffer liefern. [`split_messages`]
//! trennt sie: 16-Byte-Kopf lesen (`nlmsg_len`, `nlmsg_type`, ...), die
//! Nutzlast als Text interpretieren und als [`crate::record::RawRecord`]
//! zurückgeben. Kontrollnachrichten (`nlmsg_type < NLMSG_MIN_TYPE`, z. B.
//! `NLMSG_NOOP`/`NLMSG_ERROR`/`NLMSG_DONE`) tragen keinen Audit-Record-Text
//! und werden verworfen.
//!
//! **Braucht keine Berechtigung und keinen Kernel.** Diese Funktion ist reine
//! Byte-Arithmetik auf einem übergebenen Puffer — Teil des Formungsteils
//! dieser Crate, nicht des Bindungsteils. Der einzige Konsument, der
//! tatsächlich von einem Socket liest, ist [`crate::socket`]
//! (`#[cfg(target_os = "linux")]`).
//!
//! # Umgang mit unvollständigen Rahmen
//! Ein Puffer, dessen letzte Nachricht abgeschnitten ist (weniger Bytes
//! übrig als der deklarierte Kopf oder die deklarierte Länge), bricht die
//! Zerlegung an dieser Stelle ab und gibt die bis dahin vollständig
//! erkannten Nachrichten zurück, statt einen Fehler zu erzeugen — ein
//! einzelnes `recv()` liefert nach dem Netlink-Vertrag ohnehin nie eine
//! Nachricht nur teilweise; diese Regel ist eine Verteidigung gegen
//! offensichtlich unplausible Puffer, kein erwarteter Normalfall.
//!
//! # Exportierte Typen
//! Keine eigenen Typen; die freie Funktion [`split_messages`].
//!
//! # Nebenläufigkeit
//! Zustandslose freie Funktion ohne innere Veränderlichkeit: `Send + Sync`,
//! aus beliebig vielen Threads gleichzeitig aufrufbar.
//!
//! # Fehler
//! Keine — [`split_messages`] ist total und panikfrei; siehe oben zum Umgang
//! mit unvollständigen Rahmen.
//!
//! # Examples
//! ```rust,ignore
//! // Nicht Teil der öffentlichen API dieser Crate (crate-privat); siehe die
//! // Tests in diesem Modul für ein vollständiges Beispiel eines gerahmten
//! // Puffers.
//! ```

use crate::record::RawRecord;

/// Länge eines `nlmsghdr`-Kopfs in Bytes: `nlmsg_len`(4) + `nlmsg_type`(2) +
/// `nlmsg_flags`(2) + `nlmsg_seq`(4) + `nlmsg_pid`(4).
const NLMSG_HDR_LEN: usize = 16;

/// Alle Netlink-Nachrichten sind auf 4 Byte ausgerichtet (`NLMSG_ALIGNTO`).
const NLMSG_ALIGN_TO: usize = 4;

/// Nachrichtentypen unterhalb dieser Schwelle sind generische
/// Netlink-Kontrollnachrichten (`NLMSG_NOOP` = 1, `NLMSG_ERROR` = 2,
/// `NLMSG_DONE` = 3, `NLMSG_OVERRUN` = 4), keine `AUDIT`-Records.
const NLMSG_MIN_TYPE: u16 = 0x10;

/// Rundet `len` auf das nächste Vielfache von [`NLMSG_ALIGN_TO`] auf.
const fn nlmsg_align(len: usize) -> usize {
    (len + NLMSG_ALIGN_TO - 1) & !(NLMSG_ALIGN_TO - 1)
}

/// Liest ein natives `u32` aus den ersten (bis zu vier) Bytes von `bytes`.
///
/// Panikfrei für jede Eingabelänge: fehlende Bytes zählen als `0`. Wird nur
/// mit garantiert ausreichend langen Teilstücken aufgerufen; die Toleranz ist
/// reine Verteidigung, kein erwarteter Pfad.
fn read_u32_ne(bytes: &[u8]) -> u32 {
    let mut buf = [0u8; 4];
    let n = bytes.len().min(4);
    buf[..n].copy_from_slice(&bytes[..n]);
    u32::from_ne_bytes(buf)
}

/// Liest ein natives `u16` aus den ersten (bis zu zwei) Bytes von `bytes`.
///
/// Panikfrei für jede Eingabelänge; siehe [`read_u32_ne`].
fn read_u16_ne(bytes: &[u8]) -> u16 {
    let mut buf = [0u8; 2];
    let n = bytes.len().min(2);
    buf[..n].copy_from_slice(&bytes[..n]);
    u16::from_ne_bytes(buf)
}

/// Zerlegt einen `recv()`-Puffer in einzelne Audit-Records.
///
/// # Description
/// Iteriert `nlmsghdr`-gerahmte Nachrichten im Puffer. Je Nachricht wird die
/// Nutzlast (nach dem 16-Byte-Kopf, vor dem nächsten 4-Byte-ausgerichteten
/// Versatz) verlustbehaftet als UTF-8 interpretiert
/// (`String::from_utf8_lossy`) und von umgebendem Leerraum sowie
/// eingebetteten Nullbytes befreit. Kontrollnachrichten
/// (`nlmsg_type < NLMSG_MIN_TYPE`) und leere Nutzlasten werden verworfen.
///
/// # Arguments
/// - `buf` (`&[u8]`): die von `recv()` gelieferten Rohbytes, exakt auf die
///   tatsächlich empfangene Länge begrenzt (kein Nullpolster danach).
///
/// # Returns
/// Die im Puffer gefundenen Records als [`RawRecord`], in Empfangsreihenfolge.
/// Ein leerer oder ausschließlich aus Kontrollnachrichten bestehender Puffer
/// liefert einen leeren `Vec`.
///
/// # Examples
/// ```rust,ignore
/// // Siehe die Tests dieses Moduls: `test_split_messages_single_message`
/// // baut einen synthetischen `nlmsghdr`-Puffer von Hand auf.
/// ```
pub(crate) fn split_messages(buf: &[u8]) -> Vec<RawRecord> {
    let mut records = Vec::new();
    let mut offset = 0usize;

    while offset + NLMSG_HDR_LEN <= buf.len() {
        let header = &buf[offset..offset + NLMSG_HDR_LEN];
        let len = read_u32_ne(&header[0..4]) as usize;
        let msg_type = read_u16_ne(&header[4..6]);

        if len < NLMSG_HDR_LEN || offset + len > buf.len() {
            // Abgeschnittene oder unplausible Nachricht: hier abbrechen statt
            // einen Fehler zu erzeugen (siehe Moduldokumentation).
            break;
        }

        if msg_type >= NLMSG_MIN_TYPE {
            let payload = &buf[offset + NLMSG_HDR_LEN..offset + len];
            let text = String::from_utf8_lossy(payload);
            let trimmed = text.trim_matches(|c: char| c == '\0' || c.is_whitespace());
            if !trimmed.is_empty() {
                records.push(RawRecord::new(trimmed.to_owned()));
            }
        }

        offset += nlmsg_align(len);
    }

    records
}

#[cfg(test)]
mod tests {
    use super::{NLMSG_HDR_LEN, split_messages};

    /// Baut einen einzelnen gerahmten `nlmsghdr`-Puffer für Tests.
    fn build_message(msg_type: u16, payload: &[u8]) -> Vec<u8> {
        let len = NLMSG_HDR_LEN + payload.len();
        let aligned = (len + 3) & !3;
        let mut buf = vec![0u8; aligned];
        buf[0..4].copy_from_slice(&(len as u32).to_ne_bytes());
        buf[4..6].copy_from_slice(&msg_type.to_ne_bytes());
        // flags (2 Byte), seq (4 Byte), pid (4 Byte) bleiben absichtlich 0.
        buf[NLMSG_HDR_LEN..NLMSG_HDR_LEN + payload.len()].copy_from_slice(payload);
        buf
    }

    #[test]
    fn test_split_messages_single_message_returns_one_record() {
        let buf = build_message(1300, b"type=SYSCALL msg=audit(1699999999.123:456):");
        let records = split_messages(&buf);
        assert_eq!(records.len(), 1);
        assert_eq!(
            records[0].as_str(),
            "type=SYSCALL msg=audit(1699999999.123:456):"
        );
    }

    #[test]
    fn test_split_messages_two_messages_in_one_buffer() {
        let mut buf = build_message(1300, b"type=SYSCALL msg=audit(1:1):");
        buf.extend(build_message(1327, b"type=USER_AUTH msg=audit(2:2):"));
        let records = split_messages(&buf);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].as_str(), "type=SYSCALL msg=audit(1:1):");
        assert_eq!(records[1].as_str(), "type=USER_AUTH msg=audit(2:2):");
    }

    #[test]
    fn test_split_messages_control_message_is_discarded() {
        // NLMSG_DONE = 3, unterhalb von NLMSG_MIN_TYPE.
        let buf = build_message(3, b"");
        let records = split_messages(&buf);
        assert!(records.is_empty());
    }

    #[test]
    fn test_split_messages_truncated_trailing_message_is_dropped_without_panic() {
        let mut buf = build_message(1300, b"type=SYSCALL msg=audit(1:1):");
        // Ein vollständiger 16-Byte-Kopf für eine zweite Nachricht, der eine
        // Länge (64 Byte) behauptet, die im Puffer gar nicht mehr vorhanden
        // ist — muss ohne Panik verworfen werden, statt einen Fehler zu
        // erzeugen (siehe Moduldokumentation).
        let mut bogus_header = vec![0u8; NLMSG_HDR_LEN];
        bogus_header[0..4].copy_from_slice(&(64u32).to_ne_bytes());
        bogus_header[4..6].copy_from_slice(&(1300u16).to_ne_bytes());
        buf.extend_from_slice(&bogus_header);

        let records = split_messages(&buf);
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn test_split_messages_empty_buffer_returns_empty_vec() {
        assert!(split_messages(&[]).is_empty());
    }
}
