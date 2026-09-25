//! [`Artifact`], [`ArtifactBuilder`] and the `agent-artifact-v1` codec.
//!
//! The byte layout is documented at the crate root.

use std::cmp::Ordering;
use std::fmt;

use serde_json::Value;

use crate::canonical::canonical_json;
use crate::digest::ArtifactDigest;
use crate::error::{ArtifactError, Limit, PathRejection, TamperScope};
use crate::kind::{MAX_KIND_NAME_LEN, PayloadKind, TAG_OTHER};
use crate::path::{MAX_PATH_LEN, normalize_path, require_normalized};

/// First eight bytes of every artifact.
pub const ARTIFACT_MAGIC: &[u8; 8] = b"HARWAGNT";
/// The artifact format version this crate writes and the only one it reads.
pub const FORMAT_VERSION: u16 = 1;
/// Reserved flag bits; must be `0` in v1 (a future signature block sets one).
pub const FORMAT_FLAGS: u16 = 0;
/// Maximum bytes of canonical header JSON (1 MiB).
pub const MAX_HEADER_LEN: usize = 1024 * 1024;
/// Maximum number of entries in the payload table.
pub const MAX_PAYLOAD_COUNT: usize = 4096;
/// Maximum bytes of one payload (16 MiB).
pub const MAX_PAYLOAD_LEN: usize = 16 * 1024 * 1024;
/// Maximum bytes of a whole encoded artifact, trailer included (64 MiB).
pub const MAX_ARTIFACT_LEN: usize = 64 * 1024 * 1024;

/// Length of the BLAKE3 trailer and of every recorded hash.
pub(crate) const HASH_LEN: usize = 32;
/// Magic + format version + flags (the u32 header length follows).
const PREAMBLE_LEN: usize = ARTIFACT_MAGIC.len() + 2 + 2;

/// The limits a codec run enforces. Public API always uses
/// [`Limits::DEFAULT`] (the `MAX_*` constants); tests shrink them to
/// exercise the checks without allocating hundreds of megabytes.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub(crate) header_len: usize,
    pub(crate) payload_count: usize,
    pub(crate) payload_len: usize,
    pub(crate) artifact_len: usize,
}

impl Limits {
    pub(crate) const DEFAULT: Self = Self {
        header_len: MAX_HEADER_LEN,
        payload_count: MAX_PAYLOAD_COUNT,
        payload_len: MAX_PAYLOAD_LEN,
        artifact_len: MAX_ARTIFACT_LEN,
    };
}

fn exceeded(limit: Limit, actual: usize, max: usize) -> ArtifactError {
    ArtifactError::LimitExceeded {
        limit,
        actual: actual as u64,
        max: max as u64,
    }
}

/// One entry of the payload table together with its bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct Payload {
    kind: PayloadKind,
    path: String,
    data: Vec<u8>,
    blake3: [u8; HASH_LEN],
}

impl Payload {
    fn new(kind: PayloadKind, path: String, data: Vec<u8>) -> Self {
        let blake3 = *blake3::hash(&data).as_bytes();
        Self {
            kind,
            path,
            data,
            blake3,
        }
    }

    /// What the payload is.
    #[must_use]
    pub fn kind(&self) -> &PayloadKind {
        &self.kind
    }

    /// Normalized relative path inside the artifact.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The payload bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// Length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Whether the payload has no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// BLAKE3 of [`Payload::bytes`], as recorded in the table.
    #[must_use]
    pub fn blake3(&self) -> &[u8; HASH_LEN] {
        &self.blake3
    }

    fn sort_key(&self) -> (&PayloadKind, &str) {
        (&self.kind, &self.path)
    }
}

impl fmt::Debug for Payload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Payload")
            .field("kind", &self.kind)
            .field("path", &self.path)
            .field("len", &self.data.len())
            .field("blake3", &ArtifactDigest::from_bytes(self.blake3))
            .finish()
    }
}

/// A verified, immutable `agent-artifact-v1` artifact.
///
/// Only two ways lead here: [`ArtifactBuilder::build`] and
/// [`Artifact::from_bytes`]; both enforce every rule of the format, so an
/// `Artifact` value is always well-formed.
#[derive(Clone, PartialEq, Eq)]
pub struct Artifact {
    header: Vec<u8>,
    payloads: Vec<Payload>,
    encoded: Vec<u8>,
    digest: ArtifactDigest,
}

impl Artifact {
    /// Parses and verifies an encoded artifact.
    ///
    /// Checks, in this order: total size limit, magic, format version,
    /// flags, the BLAKE3 trailer over every preceding byte, then the
    /// structure (header size limit, valid and canonical header JSON without
    /// floating-point numbers, payload count limit,
    /// kinds, normalized paths, per-payload size limit, strict `(kind,
    /// path)` order without duplicates), each payload's BLAKE3 and finally
    /// that no bytes are left over.
    ///
    /// # Errors
    /// Any [`ArtifactError`] variant except `NotEmbedded` and `Io`; a hash
    /// mismatch is [`ArtifactError::Tampered`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, ArtifactError> {
        parse(bytes, Limits::DEFAULT)
    }

    /// The encoded artifact (a copy of [`Artifact::as_bytes`]).
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        self.encoded.clone()
    }

    /// The encoded artifact, borrowed.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.encoded
    }

    /// The artifact hash: BLAKE3 over every byte before the trailer, i.e.
    /// the trailer itself. The identity of a build.
    #[must_use]
    pub fn digest(&self) -> ArtifactDigest {
        self.digest
    }

    /// The header as canonical JSON bytes, exactly as stored.
    #[must_use]
    pub fn header_json(&self) -> &[u8] {
        &self.header
    }

    /// The header parsed into a `serde_json::Value`.
    ///
    /// # Errors
    /// Cannot fail for a verified artifact in practice; the `Result` only
    /// avoids a panic path. Returns [`ArtifactError::InvalidHeader`].
    pub fn header_value(&self) -> Result<Value, ArtifactError> {
        serde_json::from_slice(&self.header).map_err(|error| ArtifactError::InvalidHeader {
            reason: error.to_string(),
        })
    }

    /// All payloads, sorted by `(kind, path)`.
    #[must_use]
    pub fn payloads(&self) -> &[Payload] {
        &self.payloads
    }

    /// Looks a payload up by kind and path. `path` is normalized first, so
    /// `./skills/a.md` finds `skills/a.md`; an invalid path finds nothing.
    #[must_use]
    pub fn payload(&self, kind: &PayloadKind, path: &str) -> Option<&Payload> {
        let path = normalize_path(path).ok()?;
        self.payloads
            .binary_search_by(|payload| payload.sort_key().cmp(&(kind, path.as_str())))
            .ok()
            .and_then(|index| self.payloads.get(index))
    }
}

impl fmt::Debug for Artifact {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Artifact")
            .field("digest", &self.digest)
            .field("header_len", &self.header.len())
            .field("payloads", &self.payloads)
            .field("len", &self.encoded.len())
            .finish()
    }
}

/// Builds an [`Artifact`] deterministically: the output bytes depend only on
/// the header value and the set of payloads, never on insertion order, time
/// or environment.
#[derive(Debug, Clone)]
pub struct ArtifactBuilder {
    header: Vec<u8>,
    has_float: bool,
    payloads: Vec<(PayloadKind, String, Vec<u8>)>,
}

impl ArtifactBuilder {
    /// Starts an artifact whose header is `header` (for the compiler: the
    /// `AgentIr` as JSON), stored as [`canonical_json`]. The header must not
    /// contain floating-point numbers; [`ArtifactBuilder::build`] rejects it
    /// otherwise.
    #[must_use]
    pub fn new(header: &Value) -> Self {
        Self {
            header: canonical_json(header),
            has_float: contains_float(header),
            payloads: Vec::new(),
        }
    }

    /// Adds a payload. The path is normalized and all checks run in
    /// [`ArtifactBuilder::build`].
    #[must_use]
    pub fn add_payload(
        mut self,
        kind: PayloadKind,
        path: impl Into<String>,
        bytes: impl Into<Vec<u8>>,
    ) -> Self {
        self.payloads.push((kind, path.into(), bytes.into()));
        self
    }

    /// Validates, sorts and encodes.
    ///
    /// # Errors
    /// [`ArtifactError::InvalidHeader`] when the header contains a
    /// floating-point number, [`ArtifactError::LimitExceeded`] for a limit
    /// violation,
    /// [`ArtifactError::InvalidPath`] / [`ArtifactError::InvalidKind`] for a
    /// rejected path or kind name, [`ArtifactError::DuplicatePayload`] when
    /// two payloads normalize to the same `(kind, path)`.
    pub fn build(self) -> Result<Artifact, ArtifactError> {
        self.build_with(Limits::DEFAULT)
    }

    pub(crate) fn build_with(self, limits: Limits) -> Result<Artifact, ArtifactError> {
        if self.has_float {
            return Err(float_in_header());
        }
        if self.header.len() > limits.header_len {
            return Err(exceeded(
                Limit::HeaderLen,
                self.header.len(),
                limits.header_len,
            ));
        }
        if self.payloads.len() > limits.payload_count {
            return Err(exceeded(
                Limit::PayloadCount,
                self.payloads.len(),
                limits.payload_count,
            ));
        }
        let mut payloads = Vec::with_capacity(self.payloads.len());
        for (kind, path, data) in self.payloads {
            kind.validate()?;
            let path = normalize_path(&path)?;
            if data.len() > limits.payload_len {
                return Err(exceeded(Limit::PayloadLen, data.len(), limits.payload_len));
            }
            payloads.push(Payload::new(kind, path, data));
        }
        payloads.sort_by(|a, b| a.sort_key().cmp(&b.sort_key()));
        let duplicate = payloads
            .windows(2)
            .find(|pair| pair[0].sort_key() == pair[1].sort_key());
        if let Some(pair) = duplicate {
            return Err(ArtifactError::DuplicatePayload {
                kind: pair[1].kind.clone(),
                path: pair[1].path.clone(),
            });
        }
        let total = encoded_len(&self.header, &payloads);
        if total > limits.artifact_len {
            return Err(exceeded(Limit::ArtifactLen, total, limits.artifact_len));
        }
        let (encoded, digest) = encode(&self.header, &payloads);
        Ok(Artifact {
            header: self.header,
            payloads,
            encoded,
            digest,
        })
    }
}

/// Floats are excluded from the header: their text form is the one part of
/// JSON whose canonical rendering differs between implementations.
fn contains_float(value: &Value) -> bool {
    match value {
        Value::Number(number) => number.is_f64(),
        Value::Array(items) => items.iter().any(contains_float),
        Value::Object(map) => map.values().any(contains_float),
        Value::Null | Value::Bool(_) | Value::String(_) => false,
    }
}

fn float_in_header() -> ArtifactError {
    ArtifactError::InvalidHeader {
        reason: "header contains a floating-point number".to_owned(),
    }
}

fn table_entry_len(payload: &Payload) -> usize {
    let kind_len = match payload.kind {
        PayloadKind::Other(ref name) => 1 + 2 + name.len(),
        _ => 1,
    };
    kind_len + 2 + payload.path.len() + 8 + HASH_LEN
}

fn encoded_len(header: &[u8], payloads: &[Payload]) -> usize {
    let table: usize = payloads.iter().map(table_entry_len).sum();
    let data: usize = payloads.iter().map(Payload::len).sum();
    PREAMBLE_LEN + 4 + header.len() + 4 + table + data + HASH_LEN
}

/// Encodes already validated and sorted parts and returns the bytes with
/// their artifact hash (the trailer). Lengths fit their fields because the
/// limits (checked by the caller) are far below the `u16`/`u32` maxima for
/// paths, kind names, header and count.
pub(crate) fn encode(header: &[u8], payloads: &[Payload]) -> (Vec<u8>, ArtifactDigest) {
    let mut out = Vec::with_capacity(encoded_len(header, payloads));
    out.extend_from_slice(ARTIFACT_MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&FORMAT_FLAGS.to_le_bytes());
    out.extend_from_slice(&(header.len() as u32).to_le_bytes());
    out.extend_from_slice(header);
    out.extend_from_slice(&(payloads.len() as u32).to_le_bytes());
    for payload in payloads {
        out.push(payload.kind.tag());
        if let PayloadKind::Other(name) = &payload.kind {
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(name.as_bytes());
        }
        out.extend_from_slice(&(payload.path.len() as u16).to_le_bytes());
        out.extend_from_slice(payload.path.as_bytes());
        out.extend_from_slice(&(payload.data.len() as u64).to_le_bytes());
        out.extend_from_slice(&payload.blake3);
    }
    for payload in payloads {
        out.extend_from_slice(&payload.data);
    }
    let trailer = *blake3::hash(&out).as_bytes();
    out.extend_from_slice(&trailer);
    (out, ArtifactDigest::from_bytes(trailer))
}

/// Little-endian reader over a byte slice; every read is bounds-checked.
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize, what: &'static str) -> Result<&'a [u8], ArtifactError> {
        let slice = self
            .pos
            .checked_add(len)
            .and_then(|end| self.bytes.get(self.pos..end))
            .ok_or(ArtifactError::Truncated { what })?;
        let end = self.pos + slice.len();
        self.pos = end;
        Ok(slice)
    }

    fn array<const N: usize>(&mut self, what: &'static str) -> Result<[u8; N], ArtifactError> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N, what)?);
        Ok(out)
    }

    fn u8(&mut self, what: &'static str) -> Result<u8, ArtifactError> {
        Ok(self.array::<1>(what)?[0])
    }

    fn u16(&mut self, what: &'static str) -> Result<u16, ArtifactError> {
        Ok(u16::from_le_bytes(self.array(what)?))
    }

    fn u32(&mut self, what: &'static str) -> Result<u32, ArtifactError> {
        Ok(u32::from_le_bytes(self.array(what)?))
    }

    fn u64(&mut self, what: &'static str) -> Result<u64, ArtifactError> {
        Ok(u64::from_le_bytes(self.array(what)?))
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }
}

/// Converts a declared length to `usize` and checks it against `max`.
fn declared(value: u64, limit: Limit, max: usize) -> Result<usize, ArtifactError> {
    match usize::try_from(value) {
        Ok(len) if len <= max => Ok(len),
        _ => Err(ArtifactError::LimitExceeded {
            limit,
            actual: value,
            max: max as u64,
        }),
    }
}

struct TableEntry {
    kind: PayloadKind,
    path: String,
    len: usize,
    blake3: [u8; HASH_LEN],
}

fn read_kind(reader: &mut Reader<'_>) -> Result<PayloadKind, ArtifactError> {
    let tag = reader.u8("payload kind")?;
    if tag != TAG_OTHER {
        return PayloadKind::from_builtin_tag(tag).ok_or_else(|| ArtifactError::InvalidKind {
            detail: format!("unknown kind tag {tag}"),
        });
    }
    let name_len = usize::from(reader.u16("payload kind name length")?);
    if name_len > MAX_KIND_NAME_LEN {
        return Err(ArtifactError::InvalidKind {
            detail: format!("kind name length {name_len} exceeds {MAX_KIND_NAME_LEN}"),
        });
    }
    let raw = reader.take(name_len, "payload kind name")?;
    let name = std::str::from_utf8(raw).map_err(|_| ArtifactError::InvalidKind {
        detail: "kind name is not UTF-8".to_owned(),
    })?;
    let kind = PayloadKind::Other(name.to_owned());
    kind.validate()?;
    Ok(kind)
}

fn read_entry(reader: &mut Reader<'_>, limits: Limits) -> Result<TableEntry, ArtifactError> {
    let kind = read_kind(reader)?;
    let path_len = usize::from(reader.u16("payload path length")?);
    if path_len > MAX_PATH_LEN {
        return Err(exceeded(Limit::PathLen, path_len, MAX_PATH_LEN));
    }
    let raw = reader.take(path_len, "payload path")?;
    let path = std::str::from_utf8(raw)
        .map_err(|_| ArtifactError::InvalidPath {
            path: String::from_utf8_lossy(raw).into_owned(),
            reason: PathRejection::NotUtf8,
        })?
        .to_owned();
    require_normalized(&path)?;
    let len = declared(
        reader.u64("payload length")?,
        Limit::PayloadLen,
        limits.payload_len,
    )?;
    let blake3 = reader.array::<HASH_LEN>("payload hash")?;
    Ok(TableEntry {
        kind,
        path,
        len,
        blake3,
    })
}

pub(crate) fn parse(bytes: &[u8], limits: Limits) -> Result<Artifact, ArtifactError> {
    if bytes.len() > limits.artifact_len {
        return Err(exceeded(
            Limit::ArtifactLen,
            bytes.len(),
            limits.artifact_len,
        ));
    }
    let mut preamble = Reader { bytes, pos: 0 };
    if preamble.array::<8>("magic")? != *ARTIFACT_MAGIC {
        return Err(ArtifactError::BadMagic);
    }
    let version = preamble.u16("format version")?;
    if version != FORMAT_VERSION {
        return Err(ArtifactError::UnsupportedVersion { found: version });
    }
    let flags = preamble.u16("flags")?;
    if flags != FORMAT_FLAGS {
        return Err(ArtifactError::UnsupportedFlags { flags });
    }
    let body_len = bytes
        .len()
        .checked_sub(HASH_LEN)
        .filter(|len| *len >= PREAMBLE_LEN)
        .ok_or(ArtifactError::Truncated { what: "trailer" })?;
    let (body, trailer) = bytes.split_at(body_len);
    let computed = *blake3::hash(body).as_bytes();
    if computed.as_slice() != trailer {
        return Err(ArtifactError::Tampered(TamperScope::Artifact));
    }

    let mut reader = Reader {
        bytes: body,
        pos: PREAMBLE_LEN,
    };
    let header_len = declared(
        u64::from(reader.u32("header length")?),
        Limit::HeaderLen,
        limits.header_len,
    )?;
    let header = reader.take(header_len, "header")?.to_vec();
    let value: Value =
        serde_json::from_slice(&header).map_err(|error| ArtifactError::InvalidHeader {
            reason: error.to_string(),
        })?;
    if canonical_json(&value) != header {
        return Err(ArtifactError::NonCanonicalHeader);
    }
    if contains_float(&value) {
        return Err(float_in_header());
    }

    let count = declared(
        u64::from(reader.u32("payload count")?),
        Limit::PayloadCount,
        limits.payload_count,
    )?;
    let mut entries: Vec<TableEntry> = Vec::with_capacity(count);
    let mut data_total: usize = 0;
    for _ in 0..count {
        let entry = read_entry(&mut reader, limits)?;
        if let Some(previous) = entries.last() {
            match (&previous.kind, previous.path.as_str()).cmp(&(&entry.kind, entry.path.as_str()))
            {
                Ordering::Less => {}
                Ordering::Equal => {
                    return Err(ArtifactError::DuplicatePayload {
                        kind: entry.kind,
                        path: entry.path,
                    });
                }
                Ordering::Greater => return Err(ArtifactError::UnsortedPayloads),
            }
        }
        data_total = data_total.saturating_add(entry.len);
        entries.push(entry);
    }
    if data_total > reader.remaining() {
        return Err(ArtifactError::Truncated {
            what: "payload data",
        });
    }

    let mut payloads = Vec::with_capacity(entries.len());
    for entry in entries {
        let data = reader.take(entry.len, "payload data")?;
        if *blake3::hash(data).as_bytes() != entry.blake3 {
            return Err(ArtifactError::Tampered(TamperScope::Payload {
                kind: entry.kind,
                path: entry.path,
            }));
        }
        payloads.push(Payload {
            kind: entry.kind,
            path: entry.path,
            data: data.to_vec(),
            blake3: entry.blake3,
        });
    }
    if reader.remaining() != 0 {
        return Err(ArtifactError::TrailingBytes {
            extra: reader.remaining(),
        });
    }

    Ok(Artifact {
        header,
        payloads,
        encoded: bytes.to_vec(),
        digest: ArtifactDigest::from_bytes(computed),
    })
}

#[cfg(test)]
mod tests;
