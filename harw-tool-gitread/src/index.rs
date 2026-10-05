//! Leser für `<git-dir>/index` (Versionen 2, 3 und 4).
//!
//! # Verantwortung
//! [`parse`] zerlegt die Index-Datei in [`IndexEntry`]-Werte (Pfad, Modus,
//! Objekt-ID, Größe, mtime, Stage und die Flags *assume-valid*,
//! *skip-worktree*, *intent-to-add*). Version 4 (Pfadpräfix-Kompression) wird
//! unterstützt. Die Prüfsumme am Dateiende wird verifiziert.
//!
//! # Härtung
//! - Jeder Zugriff ist längengeprüft; abgeschnittene oder inkonsistente
//!   Dateien sind ein Fehler mit Meldung, nie ein Panic.
//! - Eintragszahl ([`MAX_INDEX_ENTRIES`]) und Pfadlänge sind begrenzt.
//! - Pfade mit `..`, `.git`, absolutem Anfang oder NUL werden abgelehnt.
//! - Erweiterungen mit kleingeschriebener Signatur (`link` = geteilter Index,
//!   `sdir` = Sparse-Index) sind laut Format „erforderlich“; da sie hier nicht
//!   verstanden werden, ist das ein klarer Fehler statt falscher Ergebnisse.
//!   Optionale (großgeschriebene) Erweiterungen werden übersprungen.

use crate::oid::{OID_LEN, Oid};
use crate::repo::Repo;
use crate::tree::safe_name;
use sha1::{Digest, Sha1};

/// Höchstzahl Index-Einträge.
pub const MAX_INDEX_ENTRIES: usize = 2_000_000;

/// Längster Pfad im Index.
pub const MAX_INDEX_PATH: usize = 4096;

/// Ein Index-Eintrag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    /// Pfad (Bytes, `/`-getrennt).
    pub path: Vec<u8>,
    /// Modus (`0o100644`, `0o100755`, `0o120000`, `0o160000`).
    pub mode: u32,
    /// Objekt-ID.
    pub oid: Oid,
    /// Dateigröße (auf 32 Bit gekürzt, wie im Index).
    pub size: u32,
    /// mtime-Sekunden.
    pub mtime_sec: u32,
    /// mtime-Nanosekunden.
    pub mtime_nsec: u32,
    /// Stage (0 = normal, 1–3 = Konflikt).
    pub stage: u8,
    /// `assume-valid`-Flag.
    pub assume_valid: bool,
    /// `skip-worktree`-Flag (Sparse-Checkout).
    pub skip_worktree: bool,
    /// `intent-to-add`-Flag (`git add -N`).
    pub intent_to_add: bool,
}

/// Der gelesene Index.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Index {
    /// Formatversion (2–4).
    pub version: u32,
    /// Einträge in Dateireihenfolge (nach Pfad, dann Stage).
    pub entries: Vec<IndexEntry>,
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(len).ok_or("index: offset overflow")?;
        let slice = self
            .data
            .get(self.pos..end)
            .ok_or("index: unexpected end of file")?;
        self.pos = end;
        Ok(slice)
    }

    fn u32(&mut self) -> Result<u32, String> {
        let bytes: [u8; 4] = self.take(4)?.try_into().map_err(|_| "index: short read")?;
        Ok(u32::from_be_bytes(bytes))
    }

    fn u16(&mut self) -> Result<u16, String> {
        let bytes: [u8; 2] = self.take(2)?.try_into().map_err(|_| "index: short read")?;
        Ok(u16::from_be_bytes(bytes))
    }

    fn byte(&mut self) -> Result<u8, String> {
        Ok(*self.take(1)?.first().ok_or("index: short read")?)
    }

    /// Git-Varint mit Offset-Kodierung (Pfad-Präfixlänge in Version 4).
    fn offset_varint(&mut self) -> Result<usize, String> {
        let mut c = self.byte()?;
        let mut value = usize::from(c & 0x7f);
        while c & 0x80 != 0 {
            value = value.checked_add(1).ok_or("index: varint overflow")?;
            c = self.byte()?;
            value = value
                .checked_mul(128)
                .ok_or("index: varint overflow")?
                .checked_add(usize::from(c & 0x7f))
                .ok_or("index: varint overflow")?;
        }
        Ok(value)
    }
}

/// Zerlegt den Inhalt einer Index-Datei.
///
/// # Errors
/// Meldung bei Formatfehlern, falscher Prüfsumme, nicht unterstützten
/// Erweiterungen oder unzulässigen Pfaden.
pub fn parse(data: &[u8]) -> Result<Index, String> {
    if data.len() < 12 + OID_LEN {
        return Err("index: file is too short".to_owned());
    }
    let (body, checksum) = data.split_at(data.len() - OID_LEN);
    if Sha1::digest(body).as_slice() != checksum {
        return Err("index: checksum mismatch (file is corrupt or being written)".to_owned());
    }
    let mut cur = Cursor { data: body, pos: 0 };
    if cur.take(4)? != b"DIRC" {
        return Err("index: bad signature".to_owned());
    }
    let version = cur.u32()?;
    if !(2..=4).contains(&version) {
        return Err(format!("index: unsupported version {version}"));
    }
    let count = usize::try_from(cur.u32()?).map_err(|_| "index: entry count overflow")?;
    if count > MAX_INDEX_ENTRIES {
        return Err(format!("index: more than {MAX_INDEX_ENTRIES} entries"));
    }
    let mut entries: Vec<IndexEntry> = Vec::with_capacity(count.min(65_536));
    let mut previous: Vec<u8> = Vec::new();
    let mut deferred: Option<String> = None;
    for _ in 0..count {
        let start = cur.pos;
        cur.take(8)?; // ctime
        let mtime_sec = cur.u32()?;
        let mtime_nsec = cur.u32()?;
        cur.take(8)?; // dev, ino
        let mode = cur.u32()?;
        cur.take(8)?; // uid, gid
        let size = cur.u32()?;
        let oid = Oid::from_bytes(cur.take(OID_LEN)?).ok_or("index: bad object id")?;
        let flags = cur.u16()?;
        let stage = u8::try_from((flags >> 12) & 0x3).unwrap_or(0);
        let mut skip_worktree = false;
        let mut intent_to_add = false;
        if flags & 0x4000 != 0 {
            if version < 3 {
                return Err("index: extended flags in a version 2 index".to_owned());
            }
            let extended = cur.u16()?;
            skip_worktree = extended & 0x4000 != 0;
            intent_to_add = extended & 0x2000 != 0;
        }
        let path: Vec<u8> = if version == 4 {
            let strip = cur.offset_varint()?;
            let rest_start = cur.pos;
            let nul = body
                .get(rest_start..)
                .and_then(|rest| rest.iter().position(|b| *b == 0))
                .ok_or("index: unterminated path")?;
            let suffix = cur.take(nul)?;
            cur.take(1)?;
            let keep = previous
                .len()
                .checked_sub(strip)
                .ok_or("index: path prefix strip is longer than the previous path")?;
            let mut path = previous
                .get(..keep)
                .ok_or("index: bad path prefix")?
                .to_vec();
            path.extend_from_slice(suffix);
            path
        } else {
            let declared = usize::from(flags & 0xFFF);
            let name_start = cur.pos;
            let name_len = if declared == 0xFFF {
                body.get(name_start..)
                    .and_then(|rest| rest.iter().position(|b| *b == 0))
                    .ok_or("index: unterminated path")?
            } else {
                declared
            };
            let name = cur.take(name_len)?.to_vec();
            let used = cur.pos - start;
            let padded = (used + 8) & !7;
            cur.take(padded - used)?;
            name
        };
        if path.is_empty()
            || path.len() > MAX_INDEX_PATH
            || path.starts_with(b"/")
            || path.split(|b| *b == b'/').any(|part| !safe_name(part))
        {
            // Erst die Erweiterungen prüfen: ein geteilter Index (`link`) hat
            // absichtlich leere Pfade und soll als solcher gemeldet werden.
            deferred.get_or_insert_with(|| "index: unsafe or invalid path".to_owned());
        }
        previous.clone_from(&path);
        entries.push(IndexEntry {
            path,
            mode,
            oid,
            size,
            mtime_sec,
            mtime_nsec,
            stage,
            assume_valid: flags & 0x8000 != 0,
            skip_worktree,
            intent_to_add,
        });
    }
    // Erweiterungen: 4 Byte Signatur, 4 Byte Länge, Daten.
    while cur.pos < body.len() {
        let signature = cur.take(4)?;
        let len = usize::try_from(cur.u32()?).map_err(|_| "index: extension size overflow")?;
        cur.take(len)?;
        if signature.first().is_some_and(u8::is_ascii_lowercase) {
            return Err(format!(
                "index: unsupported required extension '{}' (split or sparse index)",
                String::from_utf8_lossy(signature)
            ));
        }
    }
    if let Some(message) = deferred {
        return Err(message);
    }
    Ok(Index { version, entries })
}

/// Höchstgröße der Index-Datei.
pub const MAX_INDEX_BYTES: u64 = 512 * 1024 * 1024;

/// Liest den Index des Repositories; `None`, wenn es keinen gibt.
///
/// # Errors
/// Meldung bei unlesbarer oder beschädigter Datei.
pub fn read(repo: &Repo) -> Result<Option<Index>, String> {
    match repo.read_git_file("index", MAX_INDEX_BYTES) {
        Ok(bytes) => parse(&bytes).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("cannot read the index: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, build_index};

    fn oid(byte: u8) -> Oid {
        Oid::from_bytes(&[byte; OID_LEN]).unwrap_or(Oid::ZERO)
    }

    fn seal(mut body: Vec<u8>) -> Vec<u8> {
        let checksum = Sha1::digest(&body);
        body.extend_from_slice(checksum.as_slice());
        body
    }

    #[test]
    fn parses_version_two_entries() -> TestResult {
        let data = build_index(&[
            ("b.txt", 0o100_644, oid(2), 5, 100, 7),
            ("a/x.rs", 0o100_755, oid(1), 99, 200, 0),
        ]);
        let index = parse(&data)?;
        assert_eq!(index.version, 2);
        assert_eq!(index.entries.len(), 2);
        assert_eq!(index.entries[0].path, b"a/x.rs");
        assert_eq!(index.entries[0].mode, 0o100_755);
        assert_eq!(index.entries[0].size, 99);
        assert_eq!(index.entries[0].mtime_sec, 200);
        assert_eq!(index.entries[1].path, b"b.txt");
        assert_eq!(index.entries[1].oid, oid(2));
        assert_eq!(index.entries[1].mtime_nsec, 7);
        assert_eq!(index.entries[1].stage, 0);
        assert!(!index.entries[1].skip_worktree);
        Ok(())
    }

    /// Baut einen v4-Index von Hand: Pfade mit Präfixkompression.
    fn v4(entries: &[(&[u8], u8, usize)]) -> Vec<u8> {
        let mut out = b"DIRC".to_vec();
        out.extend_from_slice(&4u32.to_be_bytes());
        out.extend_from_slice(&u32::try_from(entries.len()).unwrap_or(0).to_be_bytes());
        for (suffix, byte, strip) in entries {
            out.extend_from_slice(&[0u8; 8]);
            out.extend_from_slice(&1u32.to_be_bytes());
            out.extend_from_slice(&2u32.to_be_bytes());
            out.extend_from_slice(&[0u8; 8]);
            out.extend_from_slice(&0o100_644u32.to_be_bytes());
            out.extend_from_slice(&[0u8; 8]);
            out.extend_from_slice(&3u32.to_be_bytes());
            out.extend_from_slice(&[*byte; OID_LEN]);
            out.extend_from_slice(&0u16.to_be_bytes());
            // Offset-Varint für kleine Werte < 128: ein Byte.
            out.push(u8::try_from(*strip).unwrap_or(0));
            out.extend_from_slice(suffix);
            out.push(0);
        }
        seal(out)
    }

    #[test]
    fn parses_version_four_prefix_compression() -> TestResult {
        let data = v4(&[
            (b"src/lib.rs", 1, 0),
            (b"main.rs", 2, 6),
            (b"docs/a.md", 3, 7),
        ]);
        let index = parse(&data)?;
        let paths: Vec<String> = index
            .entries
            .iter()
            .map(|e| String::from_utf8_lossy(&e.path).into_owned())
            .collect();
        assert_eq!(paths, vec!["src/lib.rs", "src/main.rs", "src/docs/a.md"]);
        assert_eq!(index.version, 4);
        // Strip länger als der Vorgänger
        let bad = v4(&[(b"a", 1, 0), (b"b", 2, 9)]);
        assert!(parse(&bad).is_err());
        Ok(())
    }

    #[test]
    fn extended_flags_set_skip_worktree_and_intent_to_add() -> TestResult {
        let mut out = b"DIRC".to_vec();
        out.extend_from_slice(&3u32.to_be_bytes());
        out.extend_from_slice(&1u32.to_be_bytes());
        out.extend_from_slice(&[0u8; 8]);
        out.extend_from_slice(&[0u8; 8]);
        out.extend_from_slice(&[0u8; 8]);
        out.extend_from_slice(&0o100_644u32.to_be_bytes());
        out.extend_from_slice(&[0u8; 8]);
        out.extend_from_slice(&0u32.to_be_bytes());
        out.extend_from_slice(&[9u8; OID_LEN]);
        let name = b"f";
        out.extend_from_slice(&(0x4000u16 | 1u16).to_be_bytes());
        out.extend_from_slice(&(0x4000u16 | 0x2000).to_be_bytes());
        out.extend_from_slice(name);
        let used = out.len() - 12;
        let pad = ((used + 8) & !7) - used;
        out.extend(std::iter::repeat_n(0u8, pad));
        let index = parse(&seal(out))?;
        assert!(index.entries[0].skip_worktree);
        assert!(index.entries[0].intent_to_add);
        assert_eq!(index.entries[0].stage, 0);
        Ok(())
    }

    #[test]
    fn optional_extensions_are_skipped_and_required_ones_rejected() -> TestResult {
        let base = build_index(&[("a", 0o100_644, oid(1), 1, 1, 1)]);
        let body = base[..base.len() - OID_LEN].to_vec();
        let mut with_tree = body.clone();
        with_tree.extend_from_slice(b"TREE");
        with_tree.extend_from_slice(&3u32.to_be_bytes());
        with_tree.extend_from_slice(b"abc");
        assert_eq!(parse(&seal(with_tree))?.entries.len(), 1);
        let mut with_link = body;
        with_link.extend_from_slice(b"link");
        with_link.extend_from_slice(&0u32.to_be_bytes());
        let error = parse(&seal(with_link))
            .err()
            .ok_or(TestError::Missing("error"))?;
        assert!(error.contains("required extension"), "{error}");
        Ok(())
    }

    #[test]
    fn corruption_is_an_error_not_a_panic() -> TestResult {
        let good = build_index(&[
            ("a", 0o100_644, oid(1), 1, 1, 1),
            ("b", 0o100_644, oid(2), 1, 1, 1),
        ]);
        // Prüfsumme
        let mut flipped = good.clone();
        flipped[20] ^= 0xff;
        assert!(
            parse(&flipped)
                .err()
                .is_some_and(|e| e.contains("checksum"))
        );
        // Abschneiden an jeder Stelle (mit korrekter Prüfsumme neu versiegelt)
        for cut in 0..good.len() - OID_LEN {
            assert!(parse(&seal(good[..cut].to_vec())).is_err(), "cut at {cut}");
        }
        // Zu kurz, falsche Signatur, falsche Version
        assert!(parse(b"").is_err());
        assert!(parse(&seal(b"XXXX\0\0\0\x02\0\0\0\0".to_vec())).is_err());
        assert!(parse(&seal(b"DIRC\0\0\0\x09\0\0\0\0".to_vec())).is_err());
        // absurde Eintragszahl
        assert!(parse(&seal(b"DIRC\0\0\0\x02\xff\xff\xff\xff".to_vec())).is_err());
        Ok(())
    }

    #[test]
    fn unsafe_paths_are_rejected() -> TestResult {
        for bad in ["../x", ".git/config", "a/../b", "/abs", "a//b"] {
            let data = build_index(&[(bad, 0o100_644, oid(1), 1, 1, 1)]);
            assert!(parse(&data).is_err(), "{bad} must be rejected");
        }
        Ok(())
    }

    #[test]
    fn stage_bits_are_reported() -> TestResult {
        let mut data = build_index(&[("c", 0o100_644, oid(1), 1, 1, 1)]);
        // Flags liegen 62 Bytes nach Eintragsbeginn (12 Byte Kopf); Stage 2 setzen.
        let flags_at = 12 + 60;
        data[flags_at] |= 0x20;
        let body = data[..data.len() - OID_LEN].to_vec();
        let index = parse(&seal(body))?;
        assert_eq!(index.entries[0].stage, 2);
        Ok(())
    }
}
