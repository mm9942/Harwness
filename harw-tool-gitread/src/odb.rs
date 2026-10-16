//! Objektdatenbank: lose Objekte und Packs (idx v2, Pack v2/v3, Deltas).
//!
//! # Verantwortung
//! [`Odb::read`] liefert zu einer [`Oid`] das [`Object`]; [`Odb::expand_prefix`]
//! löst ein Hex-Präfix auf. Gelesen wird `objects/xx/yyyy…` (zlib) sowie jedes
//! `objects/pack/pack-*.idx` samt `.pack`. Deltas (`OFS_DELTA`, `REF_DELTA`) werden
//! aufgelöst ([`apply_delta`]).
//!
//! # Grenzen und Härtung
//! - Objekte höchstens [`MAX_OBJECT_BYTES`]; Deltaketten höchstens
//!   [`MAX_DELTA_DEPTH`] tief; Pack-Indizes höchstens [`MAX_IDX_BYTES`].
//! - Alle Offsets und Größen werden gegen die Dateilänge geprüft; kaputte
//!   Packs führen zu Fehlern, nie zu Panics.
//! - Dateizugriff nur über [`crate::repo::Repo`] (symlinkfrei, an die Wurzel gebunden).
//! - Die Objekt-ID wird beim Lesen **nicht** nachgerechnet (Integritätsprüfung
//!   ist `git fsck`, nicht Aufgabe eines lesenden Werkzeugs).
//! - `objects/info/alternates` wird nicht ausgewertet (Objekte dort fehlen
//!   mit klarer Meldung).

use crate::object::{Kind, Object};
use crate::oid::{OID_LEN, Oid, parse_hex_prefix};
use crate::repo::Repo;
use flate2::bufread::ZlibDecoder;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::rc::Rc;

/// Größtes lesbares Objekt.
pub const MAX_OBJECT_BYTES: u64 = 64 * 1024 * 1024;

/// Größte Tiefe einer Deltakette.
pub const MAX_DELTA_DEPTH: usize = 100;

/// Größte Pack-Indexdatei.
pub const MAX_IDX_BYTES: u64 = 128 * 1024 * 1024;

/// Obergrenze der Objekt-Cache-Größe in Bytes.
const CACHE_BYTES: usize = 64 * 1024 * 1024;

/// Wendet einen Git-Delta auf `base` an.
///
/// # Errors
/// Meldung bei ungültigem Delta (falsche Quellgröße, Kopier-/Einfügefehler,
/// Zielgröße über `limit`).
pub fn apply_delta(base: &[u8], delta: &[u8], limit: u64) -> Result<Vec<u8>, String> {
    let mut pos = 0usize;
    let source_size = read_varint(delta, &mut pos).ok_or("delta: truncated source size")?;
    let target_size = read_varint(delta, &mut pos).ok_or("delta: truncated target size")?;
    if source_size != base.len() as u64 {
        return Err(format!(
            "delta: base has {} bytes but the delta expects {source_size}",
            base.len()
        ));
    }
    if target_size > limit {
        return Err(format!(
            "delta: result of {target_size} bytes exceeds the limit"
        ));
    }
    let mut out =
        Vec::with_capacity(usize::try_from(target_size).map_err(|_| "delta: size overflow")?);
    while pos < delta.len() {
        let op = delta[pos];
        pos += 1;
        if op & 0x80 != 0 {
            let mut offset = 0u64;
            let mut size = 0u64;
            for bit in 0..4 {
                if op & (1 << bit) != 0 {
                    let byte = *delta.get(pos).ok_or("delta: truncated copy offset")?;
                    pos += 1;
                    offset |= u64::from(byte) << (8 * bit);
                }
            }
            for bit in 0..3 {
                if op & (0x10 << bit) != 0 {
                    let byte = *delta.get(pos).ok_or("delta: truncated copy size")?;
                    pos += 1;
                    size |= u64::from(byte) << (8 * bit);
                }
            }
            if size == 0 {
                size = 0x10000;
            }
            let end = offset
                .checked_add(size)
                .ok_or("delta: copy range overflow")?;
            let range = usize::try_from(offset)
                .ok()
                .zip(usize::try_from(end).ok())
                .ok_or("delta: copy range overflow")?;
            let chunk = base
                .get(range.0..range.1)
                .ok_or("delta: copy beyond the base object")?;
            out.extend_from_slice(chunk);
        } else if op != 0 {
            let len = usize::from(op);
            let chunk = delta.get(pos..pos + len).ok_or("delta: truncated insert")?;
            out.extend_from_slice(chunk);
            pos += len;
        } else {
            return Err("delta: reserved opcode 0".to_owned());
        }
        if out.len() as u64 > target_size {
            return Err("delta: result larger than announced".to_owned());
        }
    }
    if out.len() as u64 != target_size {
        return Err(format!(
            "delta: result has {} bytes but {target_size} were announced",
            out.len()
        ));
    }
    Ok(out)
}

fn read_varint(data: &[u8], pos: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    let mut shift = 0u32;
    loop {
        let byte = *data.get(*pos)?;
        *pos += 1;
        if shift >= 63 {
            return None;
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
    }
}

/// Eine Packdatei samt (vollständig geladenem) Index.
struct Pack {
    idx: Vec<u8>,
    count: usize,
    pack_path: String,
    file: RefCell<Option<File>>,
}

const IDX_HEADER: &[u8; 8] = b"\xfftOc\0\0\0\x02";

impl Pack {
    fn new(idx: Vec<u8>, pack_path: String) -> Result<Self, String> {
        if idx.len() < 8 + 256 * 4 || !idx.starts_with(IDX_HEADER) {
            return Err("unsupported pack index (only version 2 is supported)".to_owned());
        }
        let count = usize::try_from(be32(&idx, 8 + 255 * 4).ok_or("pack index: short fanout")?)
            .map_err(|_| "pack index: count overflow")?;
        let needed = 8 + 256 * 4 + count * (OID_LEN + 4 + 4);
        if idx.len() < needed {
            return Err("pack index: truncated".to_owned());
        }
        Ok(Self {
            idx,
            count,
            pack_path,
            file: RefCell::new(None),
        })
    }

    fn fanout(&self, byte: usize) -> usize {
        be32(&self.idx, 8 + byte * 4)
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(0)
            .min(self.count)
    }

    fn sha_at(&self, index: usize) -> &[u8] {
        let start = 8 + 256 * 4 + index * OID_LEN;
        self.idx.get(start..start + OID_LEN).unwrap_or(&[])
    }

    fn offset_of(&self, index: usize) -> Option<u64> {
        let table = 8 + 256 * 4 + self.count * (OID_LEN + 4);
        let raw = be32(&self.idx, table + index * 4)?;
        if raw & 0x8000_0000 == 0 {
            return Some(u64::from(raw));
        }
        let big = 8 + 256 * 4 + self.count * (OID_LEN + 4 + 4);
        let slot = usize::try_from(raw & 0x7fff_ffff).ok()?;
        be64(&self.idx, big + slot * 8)
    }

    /// Index des ersten Eintrags, dessen SHA ≥ `needle` ist (Binärsuche im Fanout-Bereich).
    fn lower_bound(&self, needle: &[u8]) -> usize {
        let first = usize::from(needle.first().copied().unwrap_or(0));
        let mut lo = if first == 0 {
            0
        } else {
            self.fanout(first - 1)
        };
        let mut hi = self.fanout(first);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if self.sha_at(mid) < needle {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }

    fn find(&self, oid: &Oid) -> Option<u64> {
        let index = self.lower_bound(oid.as_bytes());
        (index < self.count && self.sha_at(index) == oid.as_bytes())
            .then(|| self.offset_of(index))
            .flatten()
    }
}

fn be32(data: &[u8], at: usize) -> Option<u32> {
    let raw: [u8; 4] = data.get(at..at + 4)?.try_into().ok()?;
    Some(u32::from_be_bytes(raw))
}

fn be64(data: &[u8], at: usize) -> Option<u64> {
    let raw: [u8; 8] = data.get(at..at + 8)?.try_into().ok()?;
    Some(u64::from_be_bytes(raw))
}

/// Die Objektdatenbank eines Repositories.
pub struct Odb<'a> {
    repo: &'a Repo,
    packs: RefCell<Option<Vec<Rc<Pack>>>>,
    cache: RefCell<HashMap<Oid, Rc<Object>>>,
    cache_bytes: Cell<usize>,
}

impl<'a> Odb<'a> {
    /// Neue Datenbank über `repo` (Packs werden beim ersten Zugriff geladen).
    #[must_use]
    pub fn new(repo: &'a Repo) -> Self {
        Self {
            repo,
            packs: RefCell::new(None),
            cache: RefCell::new(HashMap::new()),
            cache_bytes: Cell::new(0),
        }
    }

    fn packs(&self) -> Vec<Rc<Pack>> {
        if let Some(packs) = self.packs.borrow().as_ref() {
            return packs.clone();
        }
        let mut loaded = Vec::new();
        for entry in self.repo.list_git_dir("objects/pack") {
            let Some(stem) = entry.name.strip_suffix(".idx") else {
                continue;
            };
            let Ok(idx) = self
                .repo
                .read_git_file(&format!("objects/pack/{}", entry.name), MAX_IDX_BYTES)
            else {
                continue;
            };
            if let Ok(pack) = Pack::new(idx, format!("objects/pack/{stem}.pack")) {
                loaded.push(Rc::new(pack));
            }
        }
        *self.packs.borrow_mut() = Some(loaded.clone());
        loaded
    }

    /// Liest ein Objekt.
    ///
    /// # Errors
    /// Meldung, wenn das Objekt fehlt oder beschädigt ist.
    pub fn read(&self, oid: &Oid) -> Result<Rc<Object>, String> {
        if let Some(hit) = self.cache.borrow().get(oid) {
            return Ok(hit.clone());
        }
        let object = self.read_uncached(oid, 0)?;
        let object = Rc::new(object);
        if self.cache_bytes.get() + object.data.len() > CACHE_BYTES {
            self.cache.borrow_mut().clear();
            self.cache_bytes.set(0);
        }
        self.cache_bytes
            .set(self.cache_bytes.get() + object.data.len());
        self.cache.borrow_mut().insert(*oid, object.clone());
        Ok(object)
    }

    /// Liest ein Objekt und verlangt eine bestimmte Art.
    ///
    /// # Errors
    /// Meldung, wenn es fehlt oder eine andere Art hat.
    pub fn read_kind(&self, oid: &Oid, kind: Kind) -> Result<Rc<Object>, String> {
        let object = self.read(oid)?;
        if object.kind == kind {
            Ok(object)
        } else {
            Err(format!(
                "object {} is a {}, not a {}",
                oid.short(),
                object.kind.name(),
                kind.name()
            ))
        }
    }

    fn read_uncached(&self, oid: &Oid, depth: usize) -> Result<Object, String> {
        if depth > MAX_DELTA_DEPTH {
            return Err("delta chain is too deep".to_owned());
        }
        if let Some(object) = self.read_loose(oid)? {
            return Ok(object);
        }
        for pack in self.packs() {
            if let Some(offset) = pack.find(oid) {
                return self
                    .read_packed(&pack, offset, depth)
                    .map(|(kind, data)| Object { kind, data });
            }
        }
        Err(format!(
            "object {} not found (shallow clone, alternates or damaged repository?)",
            oid.short()
        ))
    }

    fn read_loose(&self, oid: &Oid) -> Result<Option<Object>, String> {
        let hex = oid.hex();
        let path = format!("objects/{}/{}", &hex[..2], &hex[2..]);
        let file = match self.repo.open_git_file(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("cannot read loose object {}: {error}", oid.short())),
        };
        let mut decoder = ZlibDecoder::new(BufReader::new(file)).take(MAX_OBJECT_BYTES + 64);
        let mut raw = Vec::new();
        decoder
            .read_to_end(&mut raw)
            .map_err(|e| format!("loose object {} is corrupt: {e}", oid.short()))?;
        let nul = raw
            .iter()
            .position(|b| *b == 0)
            .ok_or("loose object without header")?;
        let header =
            std::str::from_utf8(&raw[..nul]).map_err(|_| "loose object header is not text")?;
        let (kind_name, size_text) = header
            .split_once(' ')
            .ok_or("loose object header without size")?;
        let kind = Kind::from_name(kind_name)
            .ok_or_else(|| format!("unknown object type '{kind_name}'"))?;
        let size: u64 = size_text
            .parse()
            .map_err(|_| "loose object size is not a number")?;
        if size > MAX_OBJECT_BYTES {
            return Err(format!(
                "object {} is larger than {MAX_OBJECT_BYTES} bytes",
                oid.short()
            ));
        }
        let data = raw[nul + 1..].to_vec();
        if data.len() as u64 != size {
            return Err(format!("loose object {} has the wrong size", oid.short()));
        }
        Ok(Some(Object { kind, data }))
    }

    /// Liest den Eintrag bei `offset` im Pack (rekursiv für Deltas).
    fn read_packed(
        &self,
        pack: &Pack,
        offset: u64,
        depth: usize,
    ) -> Result<(Kind, Vec<u8>), String> {
        if depth > MAX_DELTA_DEPTH {
            return Err("delta chain is too deep".to_owned());
        }
        let mut file_slot = pack.file.borrow_mut();
        if file_slot.is_none() {
            *file_slot = Some(
                self.repo
                    .open_git_file(&pack.pack_path)
                    .map_err(|e| format!("cannot open pack: {e}"))?,
            );
        }
        let file = file_slot.as_mut().ok_or("pack file unavailable")?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| e.to_string())?;
        let mut reader = BufReader::with_capacity(16 * 1024, &*file);
        let (type_code, size) = read_entry_header(&mut reader)?;
        if size > MAX_OBJECT_BYTES {
            return Err(format!("packed object of {size} bytes exceeds the limit"));
        }
        match type_code {
            1..=4 => {
                let kind =
                    [Kind::Commit, Kind::Tree, Kind::Blob, Kind::Tag][usize::from(type_code) - 1];
                Ok((kind, inflate_exact(&mut reader, size)?))
            }
            6 => {
                let relative = read_ofs(&mut reader)?;
                let base_offset = offset
                    .checked_sub(relative)
                    .filter(|b| *b > 0)
                    .ok_or("pack: invalid delta base offset")?;
                let delta = inflate_exact(&mut reader, size)?;
                drop(reader);
                drop(file_slot);
                let (kind, base) = self.read_packed(pack, base_offset, depth + 1)?;
                Ok((kind, apply_delta(&base, &delta, MAX_OBJECT_BYTES)?))
            }
            7 => {
                let mut base_id = [0u8; OID_LEN];
                reader
                    .read_exact(&mut base_id)
                    .map_err(|e| format!("pack: truncated ref-delta base: {e}"))?;
                let delta = inflate_exact(&mut reader, size)?;
                drop(reader);
                drop(file_slot);
                let base_oid = Oid::from_bytes(&base_id).ok_or("pack: bad ref-delta base id")?;
                let base = self.read_uncached(&base_oid, depth + 1)?;
                Ok((
                    base.kind,
                    apply_delta(&base.data, &delta, MAX_OBJECT_BYTES)?,
                ))
            }
            other => Err(format!("pack: unknown object type {other}")),
        }
    }

    /// Wählt aus einem Hex-Präfix (4–40 Zeichen) genau ein Objekt.
    ///
    /// # Errors
    /// Meldung bei unbekanntem oder mehrdeutigem Präfix.
    pub fn expand_prefix(&self, prefix: &str) -> Result<Oid, String> {
        if let Some(full) = Oid::from_hex(prefix) {
            return Ok(full);
        }
        let (full_bytes, half) = parse_hex_prefix(prefix)
            .ok_or_else(|| format!("'{prefix}' is not a hex prefix of 4 to 40 characters"))?;
        let matches_prefix = |candidate: &[u8]| -> bool {
            candidate.len() == OID_LEN
                && candidate.starts_with(&full_bytes)
                && half.is_none_or(|nibble| {
                    candidate
                        .get(full_bytes.len())
                        .is_some_and(|b| b >> 4 == nibble)
                })
        };
        let mut found: Vec<Oid> = Vec::new();
        // lose Objekte
        let dir_name = &prefix.to_ascii_lowercase()[..2];
        for entry in self.repo.list_git_dir(&format!("objects/{dir_name}")) {
            if entry.name.len() == 38 {
                if let Some(oid) = Oid::from_hex(&format!("{dir_name}{}", entry.name)) {
                    if matches_prefix(oid.as_bytes()) {
                        found.push(oid);
                    }
                }
            }
        }
        // Packs
        let mut needle = full_bytes.clone();
        if let Some(nibble) = half {
            needle.push(nibble << 4);
        }
        for pack in self.packs() {
            let mut index = pack.lower_bound(&needle);
            while index < pack.count && matches_prefix(pack.sha_at(index)) {
                if let Some(oid) = Oid::from_bytes(pack.sha_at(index)) {
                    found.push(oid);
                }
                index += 1;
                if found.len() > 8 {
                    break;
                }
            }
        }
        found.sort_unstable();
        found.dedup();
        match found.as_slice() {
            [] => Err(format!("unknown revision or object '{prefix}'")),
            [one] => Ok(*one),
            _ => Err(format!(
                "ambiguous object name '{prefix}' matches {} objects",
                found.len()
            )),
        }
    }
}

/// Liest Typ und Größe eines Pack-Eintrags.
fn read_entry_header<R: Read>(reader: &mut R) -> Result<(u8, u64), String> {
    let mut byte = [0u8; 1];
    reader
        .read_exact(&mut byte)
        .map_err(|e| format!("pack: truncated entry header: {e}"))?;
    let type_code = (byte[0] >> 4) & 0x07;
    let mut size = u64::from(byte[0] & 0x0f);
    let mut shift = 4u32;
    let mut more = byte[0] & 0x80 != 0;
    while more {
        reader
            .read_exact(&mut byte)
            .map_err(|e| format!("pack: truncated entry size: {e}"))?;
        if shift > 56 {
            return Err("pack: entry size overflow".to_owned());
        }
        size |= u64::from(byte[0] & 0x7f) << shift;
        shift += 7;
        more = byte[0] & 0x80 != 0;
    }
    Ok((type_code, size))
}

/// Liest den relativen Basis-Offset eines `OFS_DELTA`.
fn read_ofs<R: Read>(reader: &mut R) -> Result<u64, String> {
    let mut byte = [0u8; 1];
    reader
        .read_exact(&mut byte)
        .map_err(|e| format!("pack: truncated delta offset: {e}"))?;
    let mut offset = u64::from(byte[0] & 0x7f);
    let mut more = byte[0] & 0x80 != 0;
    while more {
        reader
            .read_exact(&mut byte)
            .map_err(|e| format!("pack: truncated delta offset: {e}"))?;
        offset = offset
            .checked_add(1)
            .and_then(|o| o.checked_mul(128))
            .ok_or("pack: delta offset overflow")?
            | u64::from(byte[0] & 0x7f);
        more = byte[0] & 0x80 != 0;
    }
    Ok(offset)
}

/// Inflate genau `size` Bytes aus einem zlib-Strom.
fn inflate_exact<R: BufRead>(reader: &mut R, size: u64) -> Result<Vec<u8>, String> {
    let mut decoder = ZlibDecoder::new(reader).take(size.saturating_add(1));
    let mut out = Vec::with_capacity(usize::try_from(size.min(1 << 20)).unwrap_or(0));
    decoder
        .read_to_end(&mut out)
        .map_err(|e| format!("pack: corrupt entry: {e}"))?;
    if out.len() as u64 != size {
        return Err(format!(
            "pack: entry inflates to {} bytes, expected {size}",
            out.len()
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestRepo, TestResult};

    fn delta(source: u64, target: u64, ops: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for mut value in [source, target] {
            loop {
                let byte = u8::try_from(value & 0x7f).unwrap_or(0);
                value >>= 7;
                if value == 0 {
                    out.push(byte);
                    break;
                }
                out.push(byte | 0x80);
            }
        }
        out.extend_from_slice(ops);
        out
    }

    #[test]
    fn delta_copy_and_insert() -> TestResult {
        let base = b"hello world, hello git";
        // kopiere 5 Bytes ab 0 ("hello"), füge " there" ein, kopiere 4 ab 11 ("o, h"): offset=11,size=4
        let mut ops = vec![0x90, 5]; // copy size=5 (bit 4 set), offset 0
        ops.push(6);
        ops.extend_from_slice(b" there");
        ops.extend_from_slice(&[0x91, 11, 4]); // copy offset=11 (bit0), size=4 (bit 4)
        let result =
            apply_delta(base, &delta(22, 15, &ops), 1000).map_err(TestError::Unexpected)?;
        assert_eq!(result, b"hello there, he");
        Ok(())
    }

    #[test]
    fn delta_size_zero_means_0x10000() -> TestResult {
        let base = vec![7u8; 0x10000];
        let result = apply_delta(&base, &delta(0x10000, 0x10000, &[0x80]), 1 << 20)
            .map_err(TestError::Unexpected)?;
        assert_eq!(result.len(), 0x10000);
        Ok(())
    }

    #[test]
    fn hostile_deltas_are_errors() -> TestResult {
        let base = b"0123456789";
        let cases: Vec<Vec<u8>> = vec![
            vec![],                               // leer
            delta(9, 1, &[1, b'x']),              // falsche Quellgröße
            delta(10, 1, &[0x91, 8, 5]),          // Kopie über das Ende
            delta(10, 1, &[0x90]),                // abgeschnittene Kopiergröße
            delta(10, 3, &[5, b'a']),             // abgeschnittenes Einfügen
            delta(10, 2, &[1, b'a']),             // zu kurzes Ergebnis
            delta(10, 1, &[2, b'a', b'b']),       // zu langes Ergebnis
            delta(10, 1, &[0]),                   // reservierter Opcode
            delta(10, u64::MAX >> 1, &[1, b'a']), // riesige Zielgröße
            vec![0xff; 20],                       // endlose Varint-Folge
        ];
        for (index, bad) in cases.iter().enumerate() {
            assert!(apply_delta(base, bad, 1 << 20).is_err(), "case {index}");
        }
        Ok(())
    }

    #[test]
    fn reads_loose_objects_of_every_kind() -> TestResult {
        let repo = TestRepo::new()?;
        let blob = repo.blob("hello\n")?;
        let tree = repo.tree(&[(0o100_644, "a.txt", blob)])?;
        let commit = repo.commit(tree, &[], "first", 1_700_000_000)?;
        let opened = Repo::open(&repo.ws)?;
        let odb = Odb::new(&opened);
        assert_eq!(
            odb.read(&blob).map_err(TestError::Unexpected)?.data,
            b"hello\n"
        );
        assert_eq!(
            odb.read_kind(&tree, Kind::Tree)
                .map_err(TestError::Unexpected)?
                .kind,
            Kind::Tree
        );
        assert_eq!(
            odb.read(&commit).map_err(TestError::Unexpected)?.kind,
            Kind::Commit
        );
        assert!(odb.read_kind(&blob, Kind::Tree).is_err());
        // zweites Lesen kommt aus dem Cache und liefert dasselbe
        assert_eq!(
            odb.read(&blob).map_err(TestError::Unexpected)?.data,
            b"hello\n"
        );
        Ok(())
    }

    #[test]
    fn missing_and_corrupt_objects_are_errors() -> TestResult {
        let repo = TestRepo::new()?;
        let opened = Repo::open(&repo.ws)?;
        let odb = Odb::new(&opened);
        let missing = crate::oid::hash_object("blob", b"not stored");
        assert!(odb.read(&missing).is_err());
        let good = repo.blob("data")?;
        let hex = good.hex();
        let path = format!("objects/{}/{}", &hex[..2], &hex[2..]);
        repo.write_git_bytes(&path, b"this is not zlib")?;
        assert!(Odb::new(&opened).read(&good).is_err());
        repo.write_git_bytes(&path, &[])?;
        assert!(Odb::new(&opened).read(&good).is_err());
        Ok(())
    }

    #[test]
    fn prefix_expansion_finds_unique_and_rejects_ambiguous() -> TestResult {
        let repo = TestRepo::new()?;
        let a = repo.blob("alpha")?;
        let opened = Repo::open(&repo.ws)?;
        let odb = Odb::new(&opened);
        assert_eq!(
            odb.expand_prefix(&a.hex()[..8])
                .map_err(TestError::Unexpected)?,
            a
        );
        assert_eq!(
            odb.expand_prefix(&a.hex()[..5])
                .map_err(TestError::Unexpected)?,
            a
        );
        assert_eq!(
            odb.expand_prefix(&a.hex()).map_err(TestError::Unexpected)?,
            a
        );
        assert!(odb.expand_prefix("abc").is_err());
        assert!(odb.expand_prefix("zzzzzz").is_err());
        assert!(odb.expand_prefix("0000").is_err());
        // Mehrdeutigkeit: zwei Objekte mit gleichem 4er-Präfix (per Brute-Force suchen).
        let mut seen: std::collections::HashMap<String, Oid> = std::collections::HashMap::new();
        let mut pair = None;
        for index in 0..20_000 {
            let oid = crate::oid::hash_object("blob", format!("ambiguous-{index}").as_bytes());
            let key = oid.hex()[..4].to_owned();
            if let Some(first) = seen.insert(key, oid) {
                pair = Some((first, oid));
                break;
            }
        }
        let (first, second) = pair.ok_or(TestError::Missing("colliding prefix"))?;
        for index in 0..20_000 {
            let text = format!("ambiguous-{index}");
            let oid = crate::oid::hash_object("blob", text.as_bytes());
            if oid == first || oid == second {
                repo.blob(&text)?;
            }
        }
        let odb = Odb::new(&opened);
        let error = odb
            .expand_prefix(&first.hex()[..4])
            .err()
            .ok_or(TestError::Missing("ambiguity"))?;
        assert!(error.contains("ambiguous"), "{error}");
        Ok(())
    }

    #[test]
    fn pack_index_lookup_with_synthetic_data() -> TestResult {
        // Index mit drei Einträgen (sortiert), Offsets 12, 100 und 64-Bit-Tabelle.
        let oids = [[0x00u8; 20], [0x10u8; 20], [0xf0u8; 20]];
        let mut idx = Vec::new();
        idx.extend_from_slice(IDX_HEADER);
        let mut fanout = [0u32; 256];
        for oid in &oids {
            for slot in fanout.iter_mut().skip(usize::from(oid[0])) {
                *slot += 1;
            }
        }
        for value in fanout {
            idx.extend_from_slice(&value.to_be_bytes());
        }
        for oid in &oids {
            idx.extend_from_slice(oid);
        }
        idx.extend_from_slice(&[0u8; 12]); // crc
        for offset in [12u32, 100, 0x8000_0000] {
            idx.extend_from_slice(&offset.to_be_bytes());
        }
        idx.extend_from_slice(&(1u64 << 40).to_be_bytes());
        let pack = Pack::new(idx, "objects/pack/x.pack".into()).map_err(TestError::Unexpected)?;
        assert_eq!(
            pack.find(&Oid::from_bytes(&oids[0]).ok_or(TestError::Missing("oid"))?),
            Some(12)
        );
        assert_eq!(
            pack.find(&Oid::from_bytes(&oids[1]).ok_or(TestError::Missing("oid"))?),
            Some(100)
        );
        assert_eq!(
            pack.find(&Oid::from_bytes(&oids[2]).ok_or(TestError::Missing("oid"))?),
            Some(1 << 40)
        );
        assert_eq!(
            pack.find(&Oid::from_bytes(&[0x11u8; 20]).ok_or(TestError::Missing("oid"))?),
            None
        );
        assert!(Pack::new(vec![0; 10], String::new()).is_err());
        let mut truncated = IDX_HEADER.to_vec();
        truncated.extend_from_slice(&[0xff; 256 * 4]);
        assert!(Pack::new(truncated, String::new()).is_err());
        Ok(())
    }
}
