//! Byte formats: the `CGKC` ciphertext frame and HPKE `info` binding
//! (replicated byte for byte from CryptGuard's private
//! `crypt_guard_service::memory::binding`), and the sealed store file.
//!
//! # `CGKC` frame
//!
//! ```text
//! magic "CGKC" (4) | format version u8 = 1 | purpose u8 | key version u32 BE | CGH3 envelope
//! ```
//!
//! # HPKE `info` binding
//!
//! ```text
//! lp32("crypt_guard/kms/v1") | purpose u8 | lp32(namespace) | lp32(id) | version u32 BE
//!   | suite.suite_id() (10 bytes) | lp32(caller info)
//! ```
//!
//! A ciphertext therefore only opens under the exact key version and purpose
//! it was made for. The store-file layout is specified in the parent module.

use std::collections::BTreeMap;

use chacha20poly1305::aead::{Aead as _, Payload};
use chacha20poly1305::{KeyInit as _, XChaCha20Poly1305, XNonce};
use crypt_guard_service::pq_hpke::{self, Suite};
use crypt_guard_service::{
    CryptoServiceError, KeyAlgorithm, KeyId, KeyNamespace, KeyState, KeyVersion, SignatureAlgorithm,
};
use zeroize::Zeroizing;

use super::SealedStoreError;
use super::store::{KeyEntry, KeyTable, Material, SEED_LEN, VersionEntry};

// ---------------------------------------------------------------------------
// CGKC frame and info binding
// ---------------------------------------------------------------------------

/// Frame magic.
pub(super) const FRAME_MAGIC: [u8; 4] = *b"CGKC";
/// Frame format version.
pub(super) const FRAME_VERSION: u8 = 1;
/// Fixed header length: magic + format version + purpose + key version.
pub(super) const FRAME_HEADER_LEN: usize = 10;
/// Domain label bound into every HPKE `info`.
pub(super) const DOMAIN_LABEL: &[u8] = b"crypt_guard/kms/v1";

/// What a ciphertext is for. Encrypt and wrap ciphertexts are not
/// interchangeable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum Purpose {
    /// Data encryption (`Encrypt` / `Decrypt`).
    Encrypt = 1,
    /// Key wrapping (`WrapKey` / `UnwrapKey` / `RewrapKey`).
    Wrap = 2,
}

impl Purpose {
    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            1 => Some(Self::Encrypt),
            2 => Some(Self::Wrap),
            _ => None,
        }
    }
}

/// A decoded frame, borrowing the envelope bytes.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Frame<'a> {
    /// Purpose byte.
    pub(super) purpose: Purpose,
    /// Key version the ciphertext was made under.
    pub(super) key_version: KeyVersion,
    /// Encoded `CGH3` envelope.
    pub(super) envelope: &'a [u8],
}

/// Encode a frame.
pub(super) fn encode_frame(purpose: Purpose, key_version: KeyVersion, envelope: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(FRAME_HEADER_LEN.saturating_add(envelope.len()));
    out.extend_from_slice(&FRAME_MAGIC);
    out.push(FRAME_VERSION);
    out.push(purpose as u8);
    out.extend_from_slice(&key_version.get().to_be_bytes());
    out.extend_from_slice(envelope);
    out
}

/// Decode a frame. Every error is `AuthenticationFailed`, so malformed and
/// tampered ciphertexts are indistinguishable.
pub(super) fn decode_frame(bytes: &[u8]) -> Result<Frame<'_>, CryptoServiceError> {
    const FAIL: CryptoServiceError = CryptoServiceError::AuthenticationFailed;
    let (header, envelope) = bytes.split_at_checked(FRAME_HEADER_LEN).ok_or(FAIL)?;
    if header.get(0..4) != Some(&FRAME_MAGIC[..]) {
        return Err(FAIL);
    }
    if header.get(4) != Some(&FRAME_VERSION) {
        return Err(FAIL);
    }
    let purpose = header
        .get(5)
        .copied()
        .and_then(Purpose::from_byte)
        .ok_or(FAIL)?;
    let version_bytes: [u8; 4] = header
        .get(6..10)
        .ok_or(FAIL)?
        .try_into()
        .map_err(|_| FAIL)?;
    let key_version = KeyVersion::new(u32::from_be_bytes(version_bytes)).map_err(|_| FAIL)?;
    Ok(Frame {
        purpose,
        key_version,
        envelope,
    })
}

/// Append `lp32(bytes)`.
fn push_lp32(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), CryptoServiceError> {
    let len = u32::try_from(bytes.len()).map_err(|_| CryptoServiceError::Malformed)?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}

/// Exact encoded length of `lp32(bytes)`.
fn lp32_len(bytes: &[u8]) -> Result<usize, CryptoServiceError> {
    let _: u32 = u32::try_from(bytes.len()).map_err(|_| CryptoServiceError::Malformed)?;
    4usize
        .checked_add(bytes.len())
        .ok_or(CryptoServiceError::Malformed)
}

/// Build the HPKE `info` binding (see the module docs). Allocated with its
/// exact final capacity. Errors: any length overflow → `Malformed`.
pub(super) fn bind_info(
    purpose: Purpose,
    namespace: &KeyNamespace,
    id: &KeyId,
    version: KeyVersion,
    suite: Suite,
    user_info: &[u8],
) -> Result<Zeroizing<Vec<u8>>, CryptoServiceError> {
    let namespace_bytes = namespace.as_str().as_bytes();
    let id_bytes = id.as_str().as_bytes();
    let suite_id = suite.suite_id();

    let exact_total = lp32_len(DOMAIN_LABEL)?
        .checked_add(1)
        .and_then(|n| n.checked_add(lp32_len(namespace_bytes).ok()?))
        .and_then(|n| n.checked_add(lp32_len(id_bytes).ok()?))
        .and_then(|n| n.checked_add(4))
        .and_then(|n| n.checked_add(suite_id.len()))
        .and_then(|n| n.checked_add(lp32_len(user_info).ok()?))
        .ok_or(CryptoServiceError::Malformed)?;
    let _: u32 = u32::try_from(exact_total).map_err(|_| CryptoServiceError::Malformed)?;

    let mut out = Zeroizing::new(Vec::with_capacity(exact_total));
    push_lp32(&mut out, DOMAIN_LABEL)?;
    out.push(purpose as u8);
    push_lp32(&mut out, namespace_bytes)?;
    push_lp32(&mut out, id_bytes)?;
    out.extend_from_slice(&version.get().to_be_bytes());
    out.extend_from_slice(&suite_id);
    push_lp32(&mut out, user_info)?;

    if out.len() != exact_total {
        return Err(CryptoServiceError::Malformed);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Store file: header + AEAD
// ---------------------------------------------------------------------------

/// Store-file magic.
pub(super) const FILE_MAGIC: [u8; 4] = *b"HAKS";
/// Store-file format version.
pub(super) const FILE_FORMAT_VERSION: u16 = 1;
/// AEAD id: XChaCha20-Poly1305.
pub(super) const AEAD_XCHACHA20POLY1305: u16 = 1;
/// XChaCha20 nonce length.
const NONCE_LEN: usize = 24;
/// Poly1305 tag length.
const TAG_LEN: usize = 16;
/// Header length: magic + format + aead + nonce.
pub(super) const FILE_HEADER_LEN: usize = 4 + 2 + 2 + NONCE_LEN;

/// Body magic.
const BODY_MAGIC: [u8; 4] = *b"HAKB";
/// Body version.
const BODY_VERSION: u16 = 1;

/// Algorithm tags.
const ALG_HPKE: u8 = 1;
const ALG_ML_DSA: u8 = 2;

/// Seal an encoded body into a complete store file with a fresh nonce.
pub(super) fn seal_file(file_key: &[u8; 32], body: &[u8]) -> Result<Vec<u8>, SealedStoreError> {
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|_| SealedStoreError::Kek("OS RNG failed (nonce)"))?;

    let mut header = [0u8; FILE_HEADER_LEN];
    write_header(&mut header, &nonce);

    let cipher = XChaCha20Poly1305::new_from_slice(file_key)
        .map_err(|_| SealedStoreError::Corrupt("file key length"))?;
    let ciphertext = cipher
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: body,
                aad: &header,
            },
        )
        .map_err(|_| SealedStoreError::Corrupt("AEAD seal failed"))?;

    let mut out = Vec::with_capacity(FILE_HEADER_LEN.saturating_add(ciphertext.len()));
    out.extend_from_slice(&header);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

fn write_header(header: &mut [u8; FILE_HEADER_LEN], nonce: &[u8; NONCE_LEN]) {
    let fields: [&[u8]; 4] = [
        &FILE_MAGIC,
        &FILE_FORMAT_VERSION.to_be_bytes(),
        &AEAD_XCHACHA20POLY1305.to_be_bytes(),
        nonce,
    ];
    let mut pos = 0usize;
    for field in fields {
        let end = pos.saturating_add(field.len());
        if let Some(slot) = header.get_mut(pos..end) {
            slot.copy_from_slice(field);
        }
        pos = end;
    }
}

/// Check the header and open the AEAD. Returns the plaintext body in
/// zeroizing memory.
///
/// Errors: bad magic/version/aead or a file shorter than header + tag →
/// [`SealedStoreError::UnsupportedFormat`]; tag mismatch (tampering, wrong
/// KEK) → [`SealedStoreError::Authentication`].
pub(super) fn open_file(
    file_key: &[u8; 32],
    bytes: &[u8],
) -> Result<Zeroizing<Vec<u8>>, SealedStoreError> {
    let (header, ciphertext) = bytes
        .split_at_checked(FILE_HEADER_LEN)
        .ok_or(SealedStoreError::UnsupportedFormat("truncated header"))?;
    if header.get(0..4) != Some(&FILE_MAGIC[..]) {
        return Err(SealedStoreError::UnsupportedFormat(
            "not a sealed key store",
        ));
    }
    if header.get(4..6) != Some(&FILE_FORMAT_VERSION.to_be_bytes()[..]) {
        return Err(SealedStoreError::UnsupportedFormat(
            "unknown format version",
        ));
    }
    if header.get(6..8) != Some(&AEAD_XCHACHA20POLY1305.to_be_bytes()[..]) {
        return Err(SealedStoreError::UnsupportedFormat("unknown AEAD"));
    }
    if ciphertext.len() < TAG_LEN {
        return Err(SealedStoreError::UnsupportedFormat("truncated ciphertext"));
    }
    let nonce: [u8; NONCE_LEN] = header
        .get(8..FILE_HEADER_LEN)
        .and_then(|n| n.try_into().ok())
        .ok_or(SealedStoreError::UnsupportedFormat("truncated header"))?;

    let cipher = XChaCha20Poly1305::new_from_slice(file_key)
        .map_err(|_| SealedStoreError::Corrupt("file key length"))?;
    cipher
        .decrypt(
            &XNonce::from(nonce),
            Payload {
                msg: ciphertext,
                aad: header,
            },
        )
        .map(Zeroizing::new)
        .map_err(|_| SealedStoreError::Authentication)
}

// ---------------------------------------------------------------------------
// Store file: body codec
// ---------------------------------------------------------------------------

/// Writer into a buffer of fixed, pre-computed capacity: it refuses to grow,
/// so no reallocation leaves unzeroized copies of seeds behind.
struct Writer {
    buf: Zeroizing<Vec<u8>>,
}

impl Writer {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            buf: Zeroizing::new(Vec::with_capacity(capacity)),
        }
    }

    fn put(&mut self, bytes: &[u8]) -> Result<(), SealedStoreError> {
        let needed = self
            .buf
            .len()
            .checked_add(bytes.len())
            .ok_or(SealedStoreError::Corrupt("body length overflow"))?;
        if needed > self.buf.capacity() {
            return Err(SealedStoreError::Corrupt("body length miscomputed"));
        }
        self.buf.extend_from_slice(bytes);
        Ok(())
    }

    fn u8(&mut self, value: u8) -> Result<(), SealedStoreError> {
        self.put(&[value])
    }

    fn u16(&mut self, value: u16) -> Result<(), SealedStoreError> {
        self.put(&value.to_be_bytes())
    }

    fn u32(&mut self, value: u32) -> Result<(), SealedStoreError> {
        self.put(&value.to_be_bytes())
    }

    fn lp16(&mut self, bytes: &[u8]) -> Result<(), SealedStoreError> {
        let len =
            u16::try_from(bytes.len()).map_err(|_| SealedStoreError::Corrupt("name too long"))?;
        self.u16(len)?;
        self.put(bytes)
    }

    fn lp32(&mut self, bytes: &[u8]) -> Result<(), SealedStoreError> {
        let len =
            u32::try_from(bytes.len()).map_err(|_| SealedStoreError::Corrupt("field too long"))?;
        self.u32(len)?;
        self.put(bytes)
    }
}

fn add(total: usize, n: usize) -> Result<usize, SealedStoreError> {
    total
        .checked_add(n)
        .ok_or(SealedStoreError::Corrupt("body length overflow"))
}

fn algorithm_len(algorithm: KeyAlgorithm) -> Result<usize, SealedStoreError> {
    match algorithm {
        KeyAlgorithm::Hpke { .. } => Ok(1 + 6),
        KeyAlgorithm::Signature(_) => Ok(1 + 1),
        _ => Err(SealedStoreError::Corrupt("unsupported key algorithm")),
    }
}

fn material_len(material: &Material) -> Result<usize, SealedStoreError> {
    match material {
        Material::Hpke { .. } => Ok(SEED_LEN),
        Material::MlDsa { verifying, .. } => add(SEED_LEN + 4, verifying.as_bytes().len()),
    }
}

/// Exact encoded body length.
fn body_len(table: &KeyTable) -> Result<usize, SealedStoreError> {
    let mut total = 4 + 2 + 8 + 4;
    for ((namespace, id), entry) in table.entries() {
        total = add(total, 2 + namespace.as_str().len())?;
        total = add(total, 2 + id.as_str().len())?;
        total = add(total, algorithm_len(entry.algorithm)?)?;
        total = add(total, 4 + 4)?;
        for version in entry.versions.values() {
            total = add(total, 4 + 1 + 1)?;
            if let Some(material) = &version.material {
                total = add(total, material_len(material)?)?;
            }
        }
    }
    Ok(total)
}

fn state_byte(state: KeyState) -> Result<u8, SealedStoreError> {
    match state {
        KeyState::Enabled => Ok(1),
        KeyState::Disabled => Ok(2),
        KeyState::PendingDestruction => Ok(3),
        KeyState::Destroyed => Ok(4),
        _ => Err(SealedStoreError::Corrupt("unsupported key state")),
    }
}

fn state_from_byte(byte: u8) -> Result<KeyState, SealedStoreError> {
    match byte {
        1 => Ok(KeyState::Enabled),
        2 => Ok(KeyState::Disabled),
        3 => Ok(KeyState::PendingDestruction),
        4 => Ok(KeyState::Destroyed),
        _ => Err(SealedStoreError::Corrupt("unknown key state")),
    }
}

fn signature_byte(algorithm: SignatureAlgorithm) -> Result<u8, SealedStoreError> {
    match algorithm {
        SignatureAlgorithm::MlDsa44 => Ok(1),
        SignatureAlgorithm::MlDsa65 => Ok(2),
        SignatureAlgorithm::MlDsa87 => Ok(3),
        _ => Err(SealedStoreError::Corrupt("unsupported signature algorithm")),
    }
}

fn signature_from_byte(byte: u8) -> Result<SignatureAlgorithm, SealedStoreError> {
    match byte {
        1 => Ok(SignatureAlgorithm::MlDsa44),
        2 => Ok(SignatureAlgorithm::MlDsa65),
        3 => Ok(SignatureAlgorithm::MlDsa87),
        _ => Err(SealedStoreError::Corrupt("unknown signature algorithm")),
    }
}

fn write_algorithm(w: &mut Writer, algorithm: KeyAlgorithm) -> Result<(), SealedStoreError> {
    match algorithm {
        KeyAlgorithm::Hpke { suite } => {
            w.u8(ALG_HPKE)?;
            w.u16(suite.kem().id())?;
            w.u16(suite.kdf().id())?;
            w.u16(suite.aead().id())
        }
        KeyAlgorithm::Signature(signature) => {
            w.u8(ALG_ML_DSA)?;
            w.u8(signature_byte(signature)?)
        }
        _ => Err(SealedStoreError::Corrupt("unsupported key algorithm")),
    }
}

/// Encode the whole key table (deterministic: keys and versions in
/// ascending order) into zeroizing memory of exact size.
pub(super) fn encode_body(
    table: &KeyTable,
    generation: u64,
) -> Result<Zeroizing<Vec<u8>>, SealedStoreError> {
    let total = body_len(table)?;
    let mut w = Writer::with_capacity(total);
    w.put(&BODY_MAGIC)?;
    w.u16(BODY_VERSION)?;
    w.put(&generation.to_be_bytes())?;
    let key_count =
        u32::try_from(table.len()).map_err(|_| SealedStoreError::Corrupt("too many keys"))?;
    w.u32(key_count)?;

    for ((namespace, id), entry) in table.entries() {
        w.lp16(namespace.as_str().as_bytes())?;
        w.lp16(id.as_str().as_bytes())?;
        write_algorithm(&mut w, entry.algorithm)?;
        w.u32(entry.primary.get())?;
        let count = u32::try_from(entry.versions.len())
            .map_err(|_| SealedStoreError::Corrupt("too many versions"))?;
        w.u32(count)?;
        for (version, version_entry) in &entry.versions {
            w.u32(version.get())?;
            w.u8(state_byte(version_entry.state)?)?;
            match &version_entry.material {
                None => w.u8(0)?,
                Some(material) => {
                    w.u8(1)?;
                    w.put(material.seed_bytes()?)?;
                    if let Material::MlDsa { verifying, .. } = material {
                        w.lp32(verifying.as_bytes())?;
                    }
                }
            }
        }
    }

    if w.buf.len() != total {
        return Err(SealedStoreError::Corrupt("body length miscomputed"));
    }
    Ok(w.buf)
}

/// Bounds-checked reader over the decrypted body.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], SealedStoreError> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or(SealedStoreError::Corrupt("length overflow"))?;
        let slice = self
            .buf
            .get(self.pos..end)
            .ok_or(SealedStoreError::Corrupt("truncated body"))?;
        self.pos = end;
        Ok(slice)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], SealedStoreError> {
        self.take(N)?
            .try_into()
            .map_err(|_| SealedStoreError::Corrupt("truncated body"))
    }

    fn u8(&mut self) -> Result<u8, SealedStoreError> {
        Ok(u8::from_be_bytes(self.array::<1>()?))
    }

    fn u16(&mut self) -> Result<u16, SealedStoreError> {
        Ok(u16::from_be_bytes(self.array::<2>()?))
    }

    fn u32(&mut self) -> Result<u32, SealedStoreError> {
        Ok(u32::from_be_bytes(self.array::<4>()?))
    }

    fn u64(&mut self) -> Result<u64, SealedStoreError> {
        Ok(u64::from_be_bytes(self.array::<8>()?))
    }

    fn lp16_str(&mut self) -> Result<&'a str, SealedStoreError> {
        let len = usize::from(self.u16()?);
        std::str::from_utf8(self.take(len)?)
            .map_err(|_| SealedStoreError::Corrupt("name not UTF-8"))
    }

    fn lp32(&mut self) -> Result<&'a [u8], SealedStoreError> {
        let len = usize::try_from(self.u32()?)
            .map_err(|_| SealedStoreError::Corrupt("length overflow"))?;
        self.take(len)
    }

    fn is_empty(&self) -> bool {
        self.pos == self.buf.len()
    }
}

fn read_algorithm(r: &mut Reader<'_>) -> Result<KeyAlgorithm, SealedStoreError> {
    match r.u8()? {
        ALG_HPKE => {
            let kem = r.u16()?;
            let kdf = r.u16()?;
            let aead = r.u16()?;
            let suite = pq_hpke::suite_from_ids(kem, kdf, aead)
                .map_err(|_| SealedStoreError::Corrupt("unknown HPKE suite"))?;
            Ok(KeyAlgorithm::Hpke { suite })
        }
        ALG_ML_DSA => Ok(KeyAlgorithm::Signature(signature_from_byte(r.u8()?)?)),
        _ => Err(SealedStoreError::Corrupt("unknown key algorithm")),
    }
}

fn read_material(
    r: &mut Reader<'_>,
    algorithm: KeyAlgorithm,
) -> Result<Material, SealedStoreError> {
    let mut seed = Zeroizing::new([0u8; SEED_LEN]);
    seed.copy_from_slice(r.take(SEED_LEN)?);
    match algorithm {
        KeyAlgorithm::Hpke { suite } => {
            Material::hpke_from_seed(suite, seed).map_err(SealedStoreError::Crypto)
        }
        KeyAlgorithm::Signature(signature) => {
            let verifying = r.lp32()?.to_vec();
            Material::ml_dsa_from_parts(signature, &seed, verifying)
                .map_err(SealedStoreError::Crypto)
        }
        _ => Err(SealedStoreError::Corrupt("unsupported key algorithm")),
    }
}

fn read_key(r: &mut Reader<'_>) -> Result<((KeyNamespace, KeyId), KeyEntry), SealedStoreError> {
    let namespace = KeyNamespace::new(r.lp16_str()?)
        .map_err(|_| SealedStoreError::Corrupt("invalid namespace"))?;
    let id = KeyId::new(r.lp16_str()?).map_err(|_| SealedStoreError::Corrupt("invalid key id"))?;
    let algorithm = read_algorithm(r)?;
    let primary =
        KeyVersion::new(r.u32()?).map_err(|_| SealedStoreError::Corrupt("primary version 0"))?;
    let count = r.u32()?;
    if count == 0 {
        return Err(SealedStoreError::Corrupt("key without versions"));
    }

    let mut versions = BTreeMap::new();
    let mut previous: Option<KeyVersion> = None;
    for _ in 0..count {
        let version =
            KeyVersion::new(r.u32()?).map_err(|_| SealedStoreError::Corrupt("version 0"))?;
        if previous.is_some_and(|p| p >= version) {
            return Err(SealedStoreError::Corrupt("versions not strictly ascending"));
        }
        previous = Some(version);
        let state = state_from_byte(r.u8()?)?;
        let material = match (r.u8()?, state) {
            (0, KeyState::Destroyed) => None,
            (1, KeyState::Destroyed) | (0, _) => {
                return Err(SealedStoreError::Corrupt(
                    "tombstone must match the Destroyed state",
                ));
            }
            (1, _) => Some(read_material(r, algorithm)?),
            _ => return Err(SealedStoreError::Corrupt("unknown material flag")),
        };
        versions.insert(
            version,
            VersionEntry {
                version,
                state,
                material,
            },
        );
    }
    if !versions.contains_key(&primary) {
        return Err(SealedStoreError::Corrupt("primary version missing"));
    }

    Ok((
        (namespace, id),
        KeyEntry {
            algorithm,
            primary,
            versions,
        },
    ))
}

/// Decode an authenticated body into `(generation, table)`. Strict: unknown
/// tags, duplicates, unsorted entries and trailing bytes are all
/// [`SealedStoreError::Corrupt`].
pub(super) fn decode_body(body: &[u8]) -> Result<(u64, KeyTable), SealedStoreError> {
    let mut r = Reader { buf: body, pos: 0 };
    if r.array::<4>()? != BODY_MAGIC {
        return Err(SealedStoreError::Corrupt("bad body magic"));
    }
    if r.u16()? != BODY_VERSION {
        return Err(SealedStoreError::Corrupt("unknown body version"));
    }
    let generation = r.u64()?;
    let count = r.u32()?;

    let mut table = KeyTable::default();
    let mut previous: Option<(KeyNamespace, KeyId)> = None;
    for _ in 0..count {
        let (name, entry) = read_key(&mut r)?;
        if previous.as_ref().is_some_and(|p| *p >= name) {
            return Err(SealedStoreError::Corrupt("keys not strictly ascending"));
        }
        previous = Some(name.clone());
        table.insert_loaded(name, entry);
    }
    if !r.is_empty() {
        return Err(SealedStoreError::Corrupt("trailing bytes"));
    }
    Ok((generation, table))
}
