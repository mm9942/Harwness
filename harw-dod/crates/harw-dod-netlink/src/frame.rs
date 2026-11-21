//! Netlink-Rahmenzerlegung: einen Byte-Puffer in einzelne Nachrichten teilen.
//!
//! # Verantwortungsbereich
//! Ein `recv()` auf einem `AF_NETLINK`-Socket kann mehrere `nlmsghdr`-
//! gerahmte Nachrichten in einem einzigen Puffer liefern. [`split_datagram`]
//! prüft zuerst, ob das Datagramm vollständig im Puffer angekommen ist, und
//! [`split_messages`] trennt die Nachrichten: 16-Byte-Kopf lesen (`nlmsg_len`, `nlmsg_type`, ...), die
//! Nutzlast als Text interpretieren und als [`crate::record::RawRecord`]
//! zurückgeben — **mit** dem `nlmsg_type`, denn die Kernel-Nutzlast trägt
//! kein `type=` (siehe [`crate::record`], „Zwei Eingabeformen").
//! Kontrollnachrichten (`nlmsg_type < NLMSG_MIN_TYPE`, z. B.
//! `NLMSG_NOOP`/`NLMSG_ERROR`/`NLMSG_DONE`) tragen keinen Audit-Record-Text
//! und werden verworfen.
//!
//! **Braucht keine Berechtigung und keinen Kernel.** Diese Funktion ist reine
//! Byte-Arithmetik auf einem übergebenen Puffer — Teil des Formungsteils
//! dieser Crate, nicht des Bindungsteils. Der einzige Konsument, der
//! tatsächlich von einem Socket liest, ist [`crate::socket`]
//! (`#[cfg(target_os = "linux")]`).
//!
//! # Umgang mit unvollständigen Rahmen: Verlust wird gemeldet
//! Ein `recv()` **kann** ein Datagramm nur teilweise liefern: ist es größer
//! als der Lesepuffer, schneidet der Kernel den Rest ab und verwirft ihn.
//! Ein Audit-Record kann größer sein als ein knapp bemessener Puffer
//! (`MAX_AUDIT_MESSAGE_LENGTH` = 8970 Byte Text plus 16 Byte Kopf; ein
//! `USER_AUTH` mit langem, hex-kodiertem `acct=` kommt dem nahe). Der
//! Aufrufer liest deshalb mit `MSG_TRUNC` und übergibt die volle
//! Datagrammlänge; [`split_datagram`] meldet ein abgeschnittenes Datagramm
//! als [`NetlinkError::Truncated`].
//!
//! Dieselbe Meldung gilt, wenn die Rahmung selbst nicht aufgeht: ein Kopf,
//! dessen `nlmsg_len` kleiner als der Kopf ist oder mehr Bytes verspricht,
//! als das Datagramm trägt. Dann ist der Rest des Datagramms nicht mehr
//! zuverlässig zu rahmen. Die bis dahin vollständig erkannten Nachrichten
//! desselben Datagramms gehen dabei mit verloren; der Kernel schickt jeden
//! Audit-Record der Multicast-Gruppe aber als eigenes Datagramm, sodass das
//! in der Praxis genau den einen kaputten Record betrifft. Entscheidend ist:
//! der Verlust wird gemeldet, nie still übergangen. Nur ein Rest, der
//! kürzer als ein Kopf ist (Ausrichtungsbytes), wird ohne Meldung ignoriert —
//! er kann keinen Record tragen.
//!
//! # Exportierte Typen
//! Keine eigenen Typen; die freien Funktionen [`split_datagram`] und
//! [`split_messages`].
//!
//! # Nebenläufigkeit
//! Zustandslose freie Funktion ohne innere Veränderlichkeit: `Send + Sync`,
//! aus beliebig vielen Threads gleichzeitig aufrufbar.
//!
//! # Fehler
//! [`NetlinkError::Truncated`] für ein abgeschnittenes oder nicht
//! rahmbares Datagramm (siehe oben). Beide Funktionen sind panikfrei (kein
//! Indexieren, nur `split_first_chunk`/`split_at_checked` und geprüfte
//! Arithmetik).
//!
//! # Examples
//! ```rust,ignore
//! // Nicht Teil der öffentlichen API dieser Crate (crate-privat); siehe die
//! // Tests in diesem Modul für ein vollständiges Beispiel eines gerahmten
//! // Puffers.
//! ```

use crate::error::NetlinkError;
use crate::record::RawRecord;

/// Länge eines `nlmsghdr`-Kopfs in Bytes: `nlmsg_len`(4) + `nlmsg_type`(2) +
/// `nlmsg_flags`(2) + `nlmsg_seq`(4) + `nlmsg_pid`(4).
const NLMSG_HDR_LEN: usize = 16;

/// Alle Netlink-Nachrichten sind auf 4 Byte ausgerichtet (`NLMSG_ALIGNTO`);
/// dies ist die zugehörige Maske der unteren Bits.
const NLMSG_ALIGN_MASK: usize = 3;

/// Nachrichtentypen unterhalb dieser Schwelle sind generische
/// Netlink-Kontrollnachrichten (`NLMSG_NOOP` = 1, `NLMSG_ERROR` = 2,
/// `NLMSG_DONE` = 3, `NLMSG_OVERRUN` = 4), keine `AUDIT`-Records.
const NLMSG_MIN_TYPE: u16 = 0x10;

/// Die zwei Kopffelder, die diese Crate auswertet.
struct NlMsgHeader {
    /// `nlmsg_len`: Länge der Nachricht samt Kopf, ohne Ausrichtung.
    len: usize,
    /// `nlmsg_type`: bei Audit-Records die Recordart (z. B. 1100).
    msg_type: u16,
}

/// Liest `nlmsg_len` und `nlmsg_type` aus einem vollständigen Kopf.
///
/// # Returns
/// `None`, wenn `nlmsg_len` nicht in `usize` passt (nur auf Plattformen
/// unter 32 Bit denkbar) — die Zerlegung bricht dann ab.
fn parse_header(header: &[u8; NLMSG_HDR_LEN]) -> Option<NlMsgHeader> {
    let (len_bytes, rest) = header.split_first_chunk::<4>()?;
    let (type_bytes, _) = rest.split_first_chunk::<2>()?;
    let len = usize::try_from(u32::from_ne_bytes(*len_bytes)).ok()?;
    Some(NlMsgHeader {
        len,
        msg_type: u16::from_ne_bytes(*type_bytes),
    })
}

/// Rundet `len` auf das nächste Vielfache von 4 auf (`NLMSG_ALIGN`).
///
/// # Returns
/// `None` bei einem Überlauf.
fn nlmsg_align(len: usize) -> Option<usize> {
    len.checked_add(NLMSG_ALIGN_MASK)
        .map(|padded| padded & !NLMSG_ALIGN_MASK)
}

/// Zerlegt ein mit `MSG_TRUNC` gelesenes Datagramm in Audit-Records.
///
/// # Description
/// Ist `datagram_len` größer als die Zahl der tatsächlich empfangenen Bytes,
/// war das Datagramm größer als der Lesepuffer und der Kernel hat den Rest
/// verworfen: das ergibt [`NetlinkError::Truncated`], ohne den Anfang zu
/// zerlegen. Sonst zerlegt [`split_messages`] die empfangenen Bytes.
///
/// # Arguments
/// - `received` (`&[u8]`): die tatsächlich in den Puffer geschriebenen
///   Bytes (höchstens Pufferlänge).
/// - `datagram_len` (`usize`): die volle Länge des Datagramms, wie `recv()`
///   sie mit `MSG_TRUNC` zurückgibt.
///
/// # Returns
/// Die Records des Datagramms, in Empfangsreihenfolge.
///
/// # Errors
/// [`NetlinkError::Truncated`] für ein abgeschnittenes Datagramm oder eine
/// Rahmung, die nicht aufgeht (siehe [`split_messages`]).
///
/// # Examples
/// ```rust,ignore
/// // Crate-intern; siehe `test_split_datagram_cut_mid_message_is_truncated`.
/// ```
pub(crate) fn split_datagram(
    received: &[u8],
    datagram_len: usize,
) -> Result<Vec<RawRecord>, NetlinkError> {
    if datagram_len > received.len() {
        return Err(NetlinkError::Truncated);
    }
    split_messages(received)
}

/// Zerlegt einen `recv()`-Puffer in einzelne Audit-Records.
///
/// # Description
/// Iteriert `nlmsghdr`-gerahmte Nachrichten im Puffer. Je Nachricht wird die
/// Nutzlast (nach dem 16-Byte-Kopf, bis `nlmsg_len`) verlustbehaftet als
/// UTF-8 interpretiert (`String::from_utf8_lossy`), von umgebendem Leerraum
/// sowie Nullbytes befreit und mit ihrem `nlmsg_type` als
/// [`RawRecord::from_netlink`] zurückgegeben. Kontrollnachrichten
/// (`nlmsg_type < NLMSG_MIN_TYPE`) und leere Nutzlasten werden verworfen.
///
/// # Arguments
/// - `buf` (`&[u8]`): die von `recv()` gelieferten Rohbytes, exakt auf die
///   tatsächlich empfangene Länge begrenzt (kein Nullpolster danach).
///
/// # Returns
/// Die im Puffer gefundenen Records als [`RawRecord`], in Empfangsreihenfolge.
/// Ein leerer oder ausschließlich aus Kontrollnachrichten bestehender Puffer
/// liefert einen leeren `Vec`; ein Rest kürzer als ein Kopf wird ignoriert.
///
/// # Errors
/// [`NetlinkError::Truncated`], wenn ein vollständiger Kopf eine Länge
/// unterhalb der Kopflänge oder jenseits des Puffers deklariert (siehe
/// Moduldokumentation): der Rest des Datagramms ist dann verloren.
///
/// # Examples
/// ```rust,ignore
/// // Siehe die Tests dieses Moduls: `test_split_messages_single_message`
/// // baut einen synthetischen `nlmsghdr`-Puffer von Hand auf.
/// ```
pub(crate) fn split_messages(buf: &[u8]) -> Result<Vec<RawRecord>, NetlinkError> {
    let mut records = Vec::new();
    let mut rest = buf;

    while let Some((header, _)) = rest.split_first_chunk::<NLMSG_HDR_LEN>() {
        let header = parse_header(header).ok_or(NetlinkError::Truncated)?;
        if header.len < NLMSG_HDR_LEN {
            // Unplausible Länge: der Rest ist nicht rahmbar (siehe
            // Moduldokumentation) — gemeldet, nicht still verworfen.
            return Err(NetlinkError::Truncated);
        }
        // Abgeschnittene Nachricht: die deklarierte Länge übersteigt den
        // Puffer (siehe Moduldokumentation).
        let (message, _) = rest
            .split_at_checked(header.len)
            .ok_or(NetlinkError::Truncated)?;

        if header.msg_type >= NLMSG_MIN_TYPE
            && let Some((_, payload)) = message.split_at_checked(NLMSG_HDR_LEN)
        {
            let text = String::from_utf8_lossy(payload);
            let trimmed = text.trim_matches(|c: char| c == '\0' || c.is_whitespace());
            if !trimmed.is_empty() {
                records.push(RawRecord::from_netlink(header.msg_type, trimmed));
            }
        }

        // Die Ausrichtungsbytes der letzten Nachricht dürfen fehlen; dann ist
        // der Rest leer und die Schleife endet.
        rest = nlmsg_align(header.len)
            .and_then(|aligned| rest.get(aligned..))
            .unwrap_or_default();
    }

    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::{NLMSG_HDR_LEN, split_datagram, split_messages};
    use crate::error::NetlinkError;
    use crate::test_support::{TestError, TestResult, ctx};

    /// Baut einen einzelnen gerahmten `nlmsghdr`-Puffer für Tests, auf 4 Byte
    /// aufgefüllt.
    fn build_message(msg_type: u16, payload: &[u8]) -> TestResult<Vec<u8>> {
        let len = NLMSG_HDR_LEN
            .checked_add(payload.len())
            .ok_or(TestError::Missing("Nachrichtenlänge ohne Überlauf"))?;
        let len = u32::try_from(len).map_err(ctx("Nachrichtenlänge passt in u32"))?;
        Ok(build_message_with_len(len, msg_type, payload))
    }

    /// Wie [`build_message`], aber mit frei gewählter Längenangabe im Kopf.
    fn build_message_with_len(len: u32, msg_type: u16, payload: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&len.to_ne_bytes());
        buf.extend_from_slice(&msg_type.to_ne_bytes());
        // flags (2 Byte), seq (4 Byte), pid (4 Byte): vom Kernel stammende
        // Multicast-Nachrichten tragen hier 0.
        buf.extend_from_slice(&0u16.to_ne_bytes());
        buf.extend_from_slice(&0u32.to_ne_bytes());
        buf.extend_from_slice(&0u32.to_ne_bytes());
        buf.extend_from_slice(payload);
        while !buf.len().is_multiple_of(4) {
            buf.push(0);
        }
        buf
    }

    #[test]
    fn test_split_messages_single_message_returns_one_record() -> TestResult {
        let buf = build_message(1300, b"type=SYSCALL msg=audit(1699999999.123:456):")?;
        let records = split_messages(&buf)?;
        assert_eq!(records.len(), 1);
        let record = records.first().ok_or(TestError::Missing("Record"))?;
        assert_eq!(
            record.as_str(),
            "type=SYSCALL msg=audit(1699999999.123:456):"
        );
        assert_eq!(record.msg_type(), Some(1300));
        Ok(())
    }

    #[test]
    fn test_split_messages_two_messages_in_one_buffer_keep_their_types() -> TestResult {
        // 1300 = AUDIT_SYSCALL, 1100 = AUDIT_USER_AUTH (libaudit), jeweils
        // als Kernel-Nutzlast ohne `type=`.
        let mut buf = build_message(1300, b"audit(1:1): success=yes")?;
        buf.extend(build_message(1100, b"audit(2:2): msg='res=failed'")?);
        let records = split_messages(&buf)?;
        let types: Vec<Option<u16>> = records.iter().map(|r| r.msg_type()).collect();
        let texts: Vec<&str> = records.iter().map(|r| r.as_str()).collect();
        assert_eq!(types, vec![Some(1300), Some(1100)]);
        assert_eq!(
            texts,
            vec!["audit(1:1): success=yes", "audit(2:2): msg='res=failed'"]
        );
        Ok(())
    }

    #[test]
    fn test_split_messages_trailing_nul_and_whitespace_are_trimmed() -> TestResult {
        let buf = build_message(1112, b"audit(3:3): uid=0\n\0\0")?;
        let records = split_messages(&buf)?;
        let record = records.first().ok_or(TestError::Missing("Record"))?;
        assert_eq!(record.as_str(), "audit(3:3): uid=0");
        Ok(())
    }

    #[test]
    fn test_split_messages_control_message_is_discarded() -> TestResult {
        // NLMSG_DONE = 3, unterhalb von NLMSG_MIN_TYPE.
        let buf = build_message(3, b"")?;
        assert!(split_messages(&buf)?.is_empty());
        Ok(())
    }

    #[test]
    fn test_split_messages_empty_payload_is_discarded() -> TestResult {
        let buf = build_message(1320, b"\0\0\0\0")?;
        assert!(split_messages(&buf)?.is_empty());
        Ok(())
    }

    #[test]
    fn test_split_messages_truncated_trailing_message_is_reported_without_panic() -> TestResult {
        let mut buf = build_message(1300, b"type=SYSCALL msg=audit(1:1):")?;
        // Ein vollständiger 16-Byte-Kopf für eine zweite Nachricht, der eine
        // Länge (64 Byte) behauptet, die im Puffer gar nicht mehr vorhanden
        // ist — ein Verlust, der gemeldet wird statt still zu verschwinden
        // (siehe Moduldokumentation).
        buf.extend(build_message_with_len(64, 1300, b""));

        assert!(matches!(split_messages(&buf), Err(NetlinkError::Truncated)));
        Ok(())
    }

    #[test]
    fn test_split_messages_length_below_header_is_reported_without_panic() {
        let buf = build_message_with_len(4, 1300, b"audit(1:1): uid=0");
        assert!(matches!(split_messages(&buf), Err(NetlinkError::Truncated)));
    }

    #[test]
    fn test_split_messages_huge_declared_length_is_reported_without_panic() {
        let buf = build_message_with_len(u32::MAX, 1300, b"audit(1:1): uid=0");
        assert!(matches!(split_messages(&buf), Err(NetlinkError::Truncated)));
    }

    #[test]
    fn test_split_messages_short_header_is_ignored() -> TestResult {
        // 15 Byte: ein Byte weniger als ein vollständiger Kopf; kann keinen
        // Record tragen und ist deshalb kein Verlust.
        assert!(split_messages(&[0u8; 15])?.is_empty());
        Ok(())
    }

    #[test]
    fn test_split_datagram_complete_datagram_is_split() -> TestResult {
        let buf = build_message(1112, b"audit(5:5): uid=0 msg='res=success'")?;
        let records = split_datagram(&buf, buf.len())?;
        let record = records.first().ok_or(TestError::Missing("Record"))?;
        assert_eq!(record.msg_type(), Some(1112));
        Ok(())
    }

    #[test]
    fn test_split_datagram_cut_mid_message_is_truncated() -> TestResult {
        // Ein USER_AUTH, das größer ist als der Lesepuffer: recv() mit
        // MSG_TRUNC liefert nur den Anfang, meldet aber die volle Länge.
        let payload = format!(
            "audit(6:6): pid=1 uid=0 msg='op=PAM:authentication acct={} res=failed'",
            "41".repeat(4_200)
        );
        let full = build_message(1100, payload.as_bytes())?;
        let (received, _) = full
            .split_at_checked(1024)
            .ok_or(TestError::Missing("Puffer länger als 1024 Byte"))?;
        assert!(matches!(
            split_datagram(received, full.len()),
            Err(NetlinkError::Truncated)
        ));
        Ok(())
    }

    #[test]
    fn test_split_datagram_cut_inside_the_header_is_truncated() -> TestResult {
        let full = build_message(1100, b"audit(7:7): uid=0")?;
        let (received, _) = full
            .split_at_checked(8)
            .ok_or(TestError::Missing("Puffer länger als 8 Byte"))?;
        assert!(matches!(
            split_datagram(received, full.len()),
            Err(NetlinkError::Truncated)
        ));
        Ok(())
    }

    #[test]
    fn test_split_messages_unaligned_last_message_without_padding_is_kept() -> TestResult {
        // Die letzte Nachricht darf ohne Ausrichtungsbytes enden.
        let payload = b"audit(4:4): uid=0 x";
        let len = NLMSG_HDR_LEN
            .checked_add(payload.len())
            .ok_or(TestError::Missing("Nachrichtenlänge ohne Überlauf"))?;
        let len = u32::try_from(len).map_err(ctx("Nachrichtenlänge passt in u32"))?;
        let mut buf = build_message_with_len(len, 1112, payload);
        while buf.last() == Some(&0) {
            buf.pop();
        }
        let records = split_messages(&buf)?;
        let record = records.first().ok_or(TestError::Missing("Record"))?;
        assert_eq!(record.as_str(), "audit(4:4): uid=0 x");
        Ok(())
    }

    #[test]
    fn test_split_messages_empty_buffer_returns_empty_vec() -> TestResult {
        assert!(split_messages(&[])?.is_empty());
        Ok(())
    }
}
