//! Begrenzter Netlink-Schreiber: Nachrichtenköpfe, `nfgenmsg`, Attribute und
//! verschachtelte Attribute in einem Puffer mit fester Obergrenze.
//!
//! # Verantwortungsbereich
//! [`NlWriter`] kennt nur das Netlink-Rahmenformat, nicht nf_tables. Er
//! hängt Bytes an einen einzigen, einmal mit der Obergrenze reservierten
//! `Vec<u8>` an und weigert sich, darüber hinaus zu wachsen
//! ([`crate::NftError::Encoding`]). Längenfelder werden erst beim Schließen
//! einer Nachricht bzw. eines verschachtelten Attributs eingetragen
//! ([`MessageStart`], [`NestStart`]); dafür wird nie mit `[]` indiziert,
//! sondern über `get_mut` geschrieben.
//!
//! # Rahmenregeln (`/usr/include/linux/netlink.h`)
//! - `struct nlmsghdr` (`:52-58`): `nlmsg_len` (`u32`, samt Kopf und aller
//!   Ausrichtung), `nlmsg_type` (`u16`), `nlmsg_flags` (`u16`), `nlmsg_seq`
//!   (`u32`), `nlmsg_pid` (`u32`) — 16 Byte, Host-Reihenfolge. Jede Nachricht
//!   beginnt und endet auf `NLMSG_ALIGNTO` (4, `:98`).
//! - `struct nfgenmsg` (`netfilter/nfnetlink.h:34-38`): `nfgen_family`
//!   (`u8`), `version` (`u8`), `res_id` (`__be16`) — 4 Byte direkt nach dem
//!   Kopf.
//! - `struct nlattr` (`:225-228`): `nla_len` (`u16`), `nla_type` (`u16`),
//!   Host-Reihenfolge. `nla_len` zählt Kopf und Nutzlast, **nicht** die
//!   abschließende Füllung (Skizze `:216-223`); danach wird auf
//!   `NLA_ALIGNTO` (4, `:244`) mit Nullbytes aufgefüllt. Ein verschachteltes
//!   Attribut trägt `NLA_F_NESTED` (`:240`) und zählt seine (ausgerichteten)
//!   Kinder vollständig mit.
//!
//! # Nebenläufigkeit
//! Ein Schreiber gehört genau einem Batch-Aufbau; kein Teilen, kein Locking.
//!
//! # Fehler
//! Nur [`crate::NftError::Encoding`] mit festem Grund: Obergrenze erreicht,
//! Längenfeld zu klein, NUL in einer Zeichenkette.

use crate::error::{NftError, NftResult};
use crate::uapi::{NFNETLINK_V0, NLA_ALIGNTO, NLA_F_NESTED, NLMSG_ALIGNTO};

/// Nullbytes für Ausrichtungslücken; eine Lücke ist höchstens
/// `ALIGNTO - 1` = 3 Byte lang.
const PADDING: [u8; 3] = [0; 3];

/// Größe von `struct nlattr` (`netlink.h:225-228`: zwei `__u16`).
const NLATTR_LEN: usize = 4;

/// Abschließendes NUL eines Zeichenkettenattributs.
const NUL: &[u8] = &[0];

/// Kopf einer Netlink-Nachricht samt `nfgenmsg`.
///
/// # Description
/// `nlmsg_len` fehlt: er wird beim Schließen eingetragen. `nlmsg_pid` ist
/// immer `0` (der Kernel ordnet über den Socket zu; `nft` sendet ebenso `0`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MessageHeader {
    /// `nlmsg_type`, z. B. `NFNL_SUBSYS_NFTABLES << 8 | NFT_MSG_NEWRULE`.
    pub(crate) msg_type: u16,
    /// `nlmsg_flags`.
    pub(crate) flags: u16,
    /// `nlmsg_seq`.
    pub(crate) seq: u32,
    /// `nfgenmsg.nfgen_family`.
    pub(crate) family: u8,
    /// `nfgenmsg.res_id` (wird Big Endian geschrieben).
    pub(crate) res_id: u16,
}

/// Offen gehaltene Nachricht; nur [`NlWriter::end_message`] schließt sie.
#[must_use = "a message must be closed with NlWriter::end_message"]
#[derive(Debug)]
pub(crate) struct MessageStart(usize);

/// Offen gehaltenes verschachteltes Attribut; nur
/// [`NlWriter::end_nested`] schließt es.
#[must_use = "a nested attribute must be closed with NlWriter::end_nested"]
#[derive(Debug)]
pub(crate) struct NestStart(usize);

/// Schreiber für Netlink-Rahmen mit fester Obergrenze.
#[derive(Debug)]
pub(crate) struct NlWriter {
    buf: Vec<u8>,
    limit: usize,
}

// Fester Fehler: die Obergrenze des Puffers ist erreicht.
const fn over_limit() -> NftError {
    NftError::Encoding {
        reason: "batch exceeds MAX_BATCH_LEN",
    }
}

// Fester Fehler: ein Attribut passt nicht in sein 16-Bit-Längenfeld.
const fn attribute_too_long() -> NftError {
    NftError::Encoding {
        reason: "attribute exceeds the 16-bit nla_len",
    }
}

// Fester Fehler: ein Längenfeld zeigt hinter das Pufferende.
const fn bad_offset() -> NftError {
    NftError::Encoding {
        reason: "length field offset outside the buffer",
    }
}

impl NlWriter {
    /// Baut einen leeren Schreiber, der höchstens `limit` Byte aufnimmt.
    ///
    /// # Description
    /// Reserviert den Puffer einmal in voller Größe; danach wächst er nie
    /// über `limit` hinaus.
    pub(crate) fn with_limit(limit: usize) -> Self {
        Self {
            buf: Vec::with_capacity(limit),
            limit,
        }
    }

    /// Gibt den fertigen Puffer heraus.
    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    // Hängt Bytes an, ohne die Obergrenze zu überschreiten.
    fn put(&mut self, bytes: &[u8]) -> NftResult<()> {
        let new_len = self
            .buf
            .len()
            .checked_add(bytes.len())
            .ok_or_else(over_limit)?;
        if new_len > self.limit {
            return Err(over_limit());
        }
        self.buf.extend_from_slice(bytes);
        Ok(())
    }

    // Füllt mit Nullbytes bis zur nächsten Vielfachen von `align` auf.
    fn pad_to(&mut self, align: usize) -> NftResult<()> {
        let len = self.buf.len();
        let aligned = len.checked_next_multiple_of(align).ok_or_else(over_limit)?;
        let gap = aligned.checked_sub(len).ok_or_else(over_limit)?;
        let padding = PADDING.get(..gap).ok_or_else(over_limit)?;
        self.put(padding)
    }

    // Überschreibt bereits geschriebene Bytes ab `at` (Längenfelder).
    fn patch(&mut self, at: usize, bytes: &[u8]) -> NftResult<()> {
        let end = at.checked_add(bytes.len()).ok_or_else(bad_offset)?;
        let target = self.buf.get_mut(at..end).ok_or_else(bad_offset)?;
        // `get_mut(at..end)` liefert genau `bytes.len()` Byte.
        target.copy_from_slice(bytes);
        Ok(())
    }

    /// Beginnt eine Nachricht: `nlmsghdr` (Länge vorerst `0`) und `nfgenmsg`.
    ///
    /// # Errors
    /// [`NftError::Encoding`], wenn die Obergrenze erreicht ist.
    pub(crate) fn begin_message(&mut self, header: MessageHeader) -> NftResult<MessageStart> {
        self.pad_to(NLMSG_ALIGNTO)?;
        let start = self.buf.len();
        self.put(&0_u32.to_ne_bytes())?; // nlmsg_len, beim Schließen gesetzt
        self.put(&header.msg_type.to_ne_bytes())?;
        self.put(&header.flags.to_ne_bytes())?;
        self.put(&header.seq.to_ne_bytes())?;
        self.put(&0_u32.to_ne_bytes())?; // nlmsg_pid
        self.put(&[header.family, NFNETLINK_V0])?;
        self.put(&header.res_id.to_be_bytes())?;
        Ok(MessageStart(start))
    }

    /// Schließt eine Nachricht: auf `NLMSG_ALIGNTO` auffüllen und
    /// `nlmsg_len` eintragen.
    ///
    /// # Errors
    /// [`NftError::Encoding`], wenn die Obergrenze erreicht ist oder die
    /// Länge nicht in `u32` passt.
    pub(crate) fn end_message(&mut self, start: MessageStart) -> NftResult<()> {
        self.pad_to(NLMSG_ALIGNTO)?;
        let len = self.buf.len().checked_sub(start.0).ok_or_else(bad_offset)?;
        let len = u32::try_from(len).map_err(|_| over_limit())?;
        self.patch(start.0, &len.to_ne_bytes())
    }

    /// Schreibt ein Attribut mit beliebiger Nutzlast und füllt danach auf.
    ///
    /// # Errors
    /// [`NftError::Encoding`]: Obergrenze erreicht oder `nla_len` > `u16`.
    pub(crate) fn attr_bytes(&mut self, attr_type: u16, payload: &[u8]) -> NftResult<()> {
        self.attr_parts(attr_type, &[payload])
    }

    /// Schreibt ein Zeichenkettenattribut samt abschließendem NUL
    /// (`NLA_STRING`/`NLA_NUL_STRING`, so wie `nft` es sendet).
    ///
    /// # Errors
    /// [`NftError::Encoding`]: die Zeichenkette enthält ein NUL, die
    /// Obergrenze ist erreicht oder `nla_len` > `u16`.
    pub(crate) fn attr_cstr(&mut self, attr_type: u16, value: &str) -> NftResult<()> {
        if value.bytes().any(|byte| byte == 0) {
            return Err(NftError::Encoding {
                reason: "string attribute contains NUL",
            });
        }
        self.attr_parts(attr_type, &[value.as_bytes(), NUL])
    }

    /// Schreibt ein `NLA_U32`-Attribut in Netzreihenfolge (Big Endian),
    /// wie nf_tables es für alle numerischen Attribute erwartet.
    ///
    /// # Errors
    /// [`NftError::Encoding`], wenn die Obergrenze erreicht ist.
    pub(crate) fn attr_be32(&mut self, attr_type: u16, value: u32) -> NftResult<()> {
        self.attr_bytes(attr_type, &value.to_be_bytes())
    }

    // Kopf, Nutzlast aus mehreren Teilen, Füllung.
    fn attr_parts(&mut self, attr_type: u16, parts: &[&[u8]]) -> NftResult<()> {
        let payload_len = parts
            .iter()
            .try_fold(0_usize, |sum, part| sum.checked_add(part.len()))
            .ok_or_else(attribute_too_long)?;
        let nla_len = NLATTR_LEN
            .checked_add(payload_len)
            .and_then(|len| u16::try_from(len).ok())
            .ok_or_else(attribute_too_long)?;
        self.put(&nla_len.to_ne_bytes())?;
        self.put(&attr_type.to_ne_bytes())?;
        for part in parts {
            self.put(part)?;
        }
        self.pad_to(NLA_ALIGNTO)
    }

    /// Beginnt ein verschachteltes Attribut (`attr_type | NLA_F_NESTED`,
    /// Länge vorerst `0`).
    ///
    /// # Errors
    /// [`NftError::Encoding`], wenn die Obergrenze erreicht ist.
    pub(crate) fn begin_nested(&mut self, attr_type: u16) -> NftResult<NestStart> {
        let start = self.buf.len();
        self.put(&0_u16.to_ne_bytes())?; // nla_len, beim Schließen gesetzt
        self.put(&(attr_type | NLA_F_NESTED).to_ne_bytes())?;
        Ok(NestStart(start))
    }

    /// Schließt ein verschachteltes Attribut und trägt `nla_len` ein (Kopf
    /// plus alle ausgerichteten Kinder; ein leeres Nest hat Länge 4).
    ///
    /// # Errors
    /// [`NftError::Encoding`]: Obergrenze erreicht oder `nla_len` > `u16`.
    pub(crate) fn end_nested(&mut self, start: NestStart) -> NftResult<()> {
        self.pad_to(NLA_ALIGNTO)?;
        let len = self.buf.len().checked_sub(start.0).ok_or_else(bad_offset)?;
        let len = u16::try_from(len).map_err(|_| attribute_too_long())?;
        self.patch(start.0, &len.to_ne_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::{MessageHeader, NlWriter};
    use crate::error::NftError;
    use crate::test_support::{TestError, TestResult};

    // Little helpers built from the header layout, independent of the writer.
    fn ne16(value: u16) -> [u8; 2] {
        value.to_ne_bytes()
    }

    #[test]
    fn test_attribute_length_excludes_trailing_padding() -> TestResult {
        let mut writer = NlWriter::with_limit(64);
        writer.attr_cstr(7, "filter")?;
        let mut expected = Vec::new();
        expected.extend_from_slice(&ne16(11)); // 4 + "filter" + NUL
        expected.extend_from_slice(&ne16(7));
        expected.extend_from_slice(b"filter\0");
        expected.push(0); // padding to 12
        assert_eq!(writer.into_bytes(), expected);
        Ok(())
    }

    #[test]
    fn test_padding_is_zero_for_every_payload_length() -> TestResult {
        for payload_len in 0_usize..=16 {
            let payload = vec![0xff_u8; payload_len];
            let mut writer = NlWriter::with_limit(64);
            writer.attr_bytes(1, &payload)?;
            let bytes = writer.into_bytes();
            let nla_len = 4_usize
                .checked_add(payload_len)
                .ok_or(TestError::Missing("length"))?;
            let aligned = nla_len
                .checked_next_multiple_of(4)
                .ok_or(TestError::Missing("aligned"))?;
            assert_eq!(bytes.len(), aligned, "payload {payload_len}");
            let (head, _) = bytes
                .split_first_chunk::<2>()
                .ok_or(TestError::Missing("nla_len"))?;
            assert_eq!(usize::from(u16::from_ne_bytes(*head)), nla_len);
            let padding = bytes.get(nla_len..).ok_or(TestError::Missing("pad"))?;
            assert!(padding.iter().all(|byte| *byte == 0));
        }
        Ok(())
    }

    #[test]
    fn test_empty_nest_has_length_four_and_nested_flag() -> TestResult {
        let mut writer = NlWriter::with_limit(16);
        let nest = writer.begin_nested(2)?;
        writer.end_nested(nest)?;
        let mut expected = Vec::new();
        expected.extend_from_slice(&ne16(4));
        expected.extend_from_slice(&ne16(0x8002));
        assert_eq!(writer.into_bytes(), expected);
        Ok(())
    }

    #[test]
    fn test_message_header_layout_and_length() -> TestResult {
        let mut writer = NlWriter::with_limit(64);
        let message = writer.begin_message(MessageHeader {
            msg_type: 0x0a03,
            flags: 0x0401,
            seq: 0x0102_0304,
            family: 1,
            res_id: 0x000a,
        })?;
        writer.attr_be32(5, 1)?;
        writer.end_message(message)?;
        let mut expected = Vec::new();
        expected.extend_from_slice(&28_u32.to_ne_bytes());
        expected.extend_from_slice(&ne16(0x0a03));
        expected.extend_from_slice(&ne16(0x0401));
        expected.extend_from_slice(&0x0102_0304_u32.to_ne_bytes());
        expected.extend_from_slice(&0_u32.to_ne_bytes());
        expected.extend_from_slice(&[1, 0, 0x00, 0x0a]); // nfgenmsg, res_id BE
        expected.extend_from_slice(&ne16(8));
        expected.extend_from_slice(&ne16(5));
        expected.extend_from_slice(&[0, 0, 0, 1]); // BE32
        assert_eq!(writer.into_bytes(), expected);
        Ok(())
    }

    #[test]
    fn test_writer_refuses_to_grow_past_the_limit() -> TestResult {
        let mut writer = NlWriter::with_limit(8);
        writer.attr_bytes(1, &[1, 2, 3, 4])?;
        let refused = writer.attr_bytes(1, &[]);
        assert_eq!(
            refused,
            Err(NftError::Encoding {
                reason: "batch exceeds MAX_BATCH_LEN"
            })
        );
        assert_eq!(writer.into_bytes().len(), 8);
        Ok(())
    }

    #[test]
    fn test_attribute_longer_than_u16_is_refused() -> TestResult {
        let payload = vec![0_u8; usize::from(u16::MAX)];
        let mut writer = NlWriter::with_limit(1 << 17);
        assert_eq!(
            writer.attr_bytes(1, &payload),
            Err(NftError::Encoding {
                reason: "attribute exceeds the 16-bit nla_len"
            })
        );
        Ok(())
    }

    #[test]
    fn test_nul_inside_a_string_attribute_is_refused() -> TestResult {
        let mut writer = NlWriter::with_limit(64);
        assert_eq!(
            writer.attr_cstr(1, "a\u{0}b"),
            Err(NftError::Encoding {
                reason: "string attribute contains NUL"
            })
        );
        Ok(())
    }
}
