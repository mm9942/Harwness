//! Persistent, sealed key provider ([`SealedProvider`]).
//!
//! CryptGuard's `InMemoryProvider` loses every key when the hub restarts.
//! [`SealedProvider`] implements the same [`CryptoProvider`] contract
//! (same operations, same lifecycle, same error table, same `CGKC`
//! ciphertext format) **outside** CryptGuard, and persists every key
//! version's 32-byte seed in a store file that is encrypted and
//! authenticated with a key-encryption key ([`Kek`]) kept outside the store.
//!
//! [`CryptoProvider`]: crypt_guard_service::CryptoProvider
//!
//! # Why re-implemented
//! `crypt_guard_service::memory` (key table, `CGKC` framing, HPKE `info`
//! binding, HPKE and ML-DSA operations) is private to CryptGuard. Only the
//! public building blocks are reused here: the provider trait and operation
//! types, `pq_hpke::{generate_recipient_seed, derive_recipient_key_pair,
//! HpkeEnvelope, Suite}` and the ML-DSA `SignAlgorithm` implementations. The
//! wire semantics are replicated byte for byte (see `format.rs` and the
//! cross-compatibility tests, which let `InMemoryProvider` decrypt
//! ciphertexts framed by this module).
//!
//! # What is persisted
//! Per key version only the **provenance seed**:
//! - HPKE: the 32-byte seed from `pq_hpke::generate_recipient_seed`; the key
//!   pair is re-derived with `derive_recipient_key_pair(suite.kem(), seed)`.
//!   `RecipientPrivateKey::as_seed_bytes` is deliberately **not** persisted
//!   (for pure ML-KEM its `public_key()` fails, and it is not the documented
//!   provenance format).
//! - ML-DSA: the 32-byte FIPS 204 signing seed (`MlDsaSigningKey::as_bytes`)
//!   plus the public verifying key (no public derivation API exists for it).
//!
//! Destroyed versions are stored as tombstones without any key material.
//!
//! # Store file format (version 1)
//!
//! All integers are big-endian. `lp16(x)` = `len(x)` as `u16`, then `x`;
//! `lp32(x)` likewise with a `u32` length.
//!
//! ```text
//! file   = "HAKS" | format u16 = 1 | aead u16 = 1 (XChaCha20-Poly1305)
//!        | nonce [24] | AEAD(file_key, nonce, aad = the 32 header bytes, body)
//! body   = "HAKB" | body version u16 = 1 | generation u64 | key_count u32 | key*
//! key    = lp16(namespace) | lp16(id) | algorithm | primary u32
//!        | version_count u32 | version*          (keys sorted by (namespace, id))
//! algorithm = 0x01 | kem u16 | kdf u16 | aead u16        (PQ HPKE suite ids)
//!           | 0x02 | 0x01 = ML-DSA-44 / 0x02 = ML-DSA-65 / 0x03 = ML-DSA-87
//! version = version u32 (ascending, non-zero) | state u8 | present u8 | material?
//! state   = 1 Enabled | 2 Disabled | 3 PendingDestruction | 4 Destroyed
//! present = 0 (tombstone, iff state = Destroyed) | 1
//! material (HPKE)   = seed [32]
//! material (ML-DSA) = signing seed [32] | lp32(verifying key)
//! file_key = BLAKE3-derive_key("harw-auth-hub sealed-key-store v1 file key", KEK)
//! ```
//!
//! A fresh random nonce is drawn for every write. The whole body (metadata
//! **and** seeds) is authenticated, so any modified, truncated or
//! re-assembled file, and any wrong KEK, fails to load (fail closed:
//! [`SealedStoreError::Authentication`]). Writes are atomic (temporary file,
//! `fsync`, `rename`, directory `fsync` via `harw_fsutil::write_atomic`) with
//! mode `0600`; a sibling `<store>.lock` file holds an exclusive `flock` for
//! the provider's lifetime, so two hubs never share one store.
//!
//! # Limits
//! - **Rollback**: an attacker with write access to the store directory can
//!   replace the file with an *older* authentic version (re-enabling a
//!   destroyed version). `generation` is persisted for diagnostics, but no
//!   external monotonic counter exists yet.
//! - **Destroy** removes the seed from the current file and from memory
//!   (zeroized). Earlier file generations may survive in freed filesystem
//!   blocks; rotate the KEK (re-seal under a new KEK and destroy the old
//!   one) when crypto-shredding must be guaranteed.
//! - A failed persist rolls the in-memory change back and returns
//!   `Unavailable`. Only a failed directory `fsync` *after* the rename can
//!   leave the new state on disk while the caller saw an error.

mod format;
mod provider;
mod store;

#[cfg(test)]
mod tests;

use std::fmt;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crypt_guard_service::CryptoServiceError;
use zeroize::Zeroizing;

pub use provider::SealedProvider;

/// Length of a [`Kek`] in bytes.
pub const KEK_LEN: usize = 32;

/// BLAKE3 `derive_key` context for the store-file AEAD key.
const FILE_KEY_CONTEXT: &str = "harw-auth-hub sealed-key-store v1 file key";

/// The key-encryption key that seals the store file.
///
/// Never stored next to the store: load it from a `0600` key file
/// ([`Kek::from_key_file`]) or a systemd credential
/// ([`Kek::from_systemd_credential`]). Zeroized on drop, never printed, not
/// `Clone`.
pub struct Kek(Zeroizing<[u8; KEK_LEN]>);

impl fmt::Debug for Kek {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Kek([REDACTED])")
    }
}

impl Kek {
    /// Wrap raw KEK bytes.
    pub fn from_bytes(bytes: Zeroizing<[u8; KEK_LEN]>) -> Self {
        Self(bytes)
    }

    /// A fresh random KEK from the OS CSPRNG.
    ///
    /// # Errors
    /// [`SealedStoreError::Kek`] if the OS RNG fails.
    pub fn generate() -> Result<Self, SealedStoreError> {
        let mut bytes = Zeroizing::new([0u8; KEK_LEN]);
        getrandom::fill(&mut bytes[..]).map_err(|_| SealedStoreError::Kek("OS RNG failed"))?;
        Ok(Self(bytes))
    }

    /// Load exactly [`KEK_LEN`] raw bytes from `path`.
    ///
    /// The last path component must not be a symlink; the file must be a
    /// regular file owned by the effective uid with no group/other bits
    /// (`0600`/`0400`), checked on the opened descriptor.
    ///
    /// # Errors
    /// [`SealedStoreError::Io`] (open, permissions, read) or
    /// [`SealedStoreError::Kek`] (not exactly 32 bytes). The key bytes never
    /// appear in an error.
    pub fn from_key_file(path: &Path) -> Result<Self, SealedStoreError> {
        let mut file = harw_fsutil::open_nofollow(path, harw_fsutil::OpenMode::read_only())
            .map_err(SealedStoreError::io("open KEK file", path))?;
        harw_fsutil::ensure_private_regular(&file)
            .map_err(SealedStoreError::io("check KEK file permissions", path))?;

        // One spare byte detects over-long files without an unbounded read;
        // a fixed zeroizing buffer avoids reallocation copies.
        let mut buf = Zeroizing::new([0u8; KEK_LEN + 1]);
        let mut filled = 0usize;
        while filled < buf.len() {
            let rest = buf
                .get_mut(filled..)
                .ok_or(SealedStoreError::Kek("KEK buffer overrun"))?;
            match file.read(rest) {
                Ok(0) => break,
                Ok(n) => filled = filled.saturating_add(n),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(SealedStoreError::io("read KEK file", path)(error)),
            }
        }
        if filled != KEK_LEN {
            return Err(SealedStoreError::Kek(
                "KEK file must hold exactly 32 raw bytes",
            ));
        }
        let mut bytes = Zeroizing::new([0u8; KEK_LEN]);
        let source = buf
            .get(..KEK_LEN)
            .ok_or(SealedStoreError::Kek("KEK buffer too short"))?;
        bytes.copy_from_slice(source);
        Ok(Self(bytes))
    }

    /// Create a new `0600` KEK file at `path` with a fresh random key and
    /// return the key. Never overwrites: an existing path is an error.
    ///
    /// # Errors
    /// [`SealedStoreError::Io`] (e.g. `AlreadyExists`) or
    /// [`SealedStoreError::Kek`] (RNG failure).
    pub fn create_key_file(path: &Path) -> Result<Self, SealedStoreError> {
        use std::io::Write;

        let kek = Self::generate()?;
        let mut file =
            harw_fsutil::open_nofollow(path, harw_fsutil::OpenMode::write_create_new(0o600))
                .map_err(SealedStoreError::io("create KEK file", path))?;
        file.write_all(&kek.0[..])
            .and_then(|()| file.sync_all())
            .map_err(SealedStoreError::io("write KEK file", path))?;
        Ok(kek)
    }

    /// Load the systemd credential `name` (`LoadCredential=` /
    /// `LoadCredentialEncrypted=`) from `$CREDENTIALS_DIRECTORY`.
    ///
    /// # Errors
    /// [`SealedStoreError::Kek`] if `$CREDENTIALS_DIRECTORY` is unset or
    /// `name` is not a plain file name; otherwise as
    /// [`Kek::from_credentials_dir`].
    pub fn from_systemd_credential(name: &str) -> Result<Self, SealedStoreError> {
        let dir = std::env::var_os("CREDENTIALS_DIRECTORY").ok_or(SealedStoreError::Kek(
            "CREDENTIALS_DIRECTORY is not set (no systemd credentials)",
        ))?;
        Self::from_credentials_dir(Path::new(&dir), name)
    }

    /// Load credential `name` from the credentials directory `dir` (the
    /// testable half of [`Kek::from_systemd_credential`]).
    ///
    /// # Errors
    /// [`SealedStoreError::Kek`] for an invalid `name` (empty, `.`/`..`,
    /// containing `/` or NUL); otherwise as [`Kek::from_key_file`].
    pub fn from_credentials_dir(dir: &Path, name: &str) -> Result<Self, SealedStoreError> {
        let valid = !name.is_empty()
            && name != "."
            && name != ".."
            && !name.bytes().any(|b| b == b'/' || b == 0);
        if !valid {
            return Err(SealedStoreError::Kek("invalid credential name"));
        }
        Self::from_key_file(&dir.join(name))
    }

    /// The store-file AEAD key, domain-separated from the raw KEK so the
    /// same KEK bytes can never collide with another use.
    fn file_key(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(blake3::derive_key(FILE_KEY_CONTEXT, &self.0[..]))
    }
}

/// Why a sealed store could not be created, opened or written.
///
/// Never contains key material.
#[derive(Debug)]
#[non_exhaustive]
pub enum SealedStoreError {
    /// A file operation failed (includes too-wide permissions on the store or
    /// KEK file, reported by `harw_fsutil::ensure_private_regular`).
    Io {
        /// What was being done.
        context: &'static str,
        /// The file.
        path: PathBuf,
        /// The I/O error.
        source: io::Error,
    },
    /// The KEK source is unusable.
    Kek(&'static str),
    /// Another process holds the store lock.
    Locked(PathBuf),
    /// [`SealedProvider::open`] on a path without a store file.
    NotFound(PathBuf),
    /// [`SealedProvider::create`] on a path that already exists.
    AlreadyExists(PathBuf),
    /// The store file exceeds the size limit.
    TooLarge {
        /// The file.
        path: PathBuf,
        /// Its size in bytes.
        len: u64,
    },
    /// The unauthenticated header is not a supported store file.
    UnsupportedFormat(&'static str),
    /// The store failed authentication: the file was modified or truncated,
    /// or the KEK is wrong. Deliberately carries no detail.
    Authentication,
    /// The authenticated body is inconsistent (written by a buggy or foreign
    /// writer holding the KEK), or the in-memory state cannot be encoded.
    Corrupt(&'static str),
    /// Re-deriving or generating key material failed.
    Crypto(CryptoServiceError),
}

impl SealedStoreError {
    /// `map_err` adapter for I/O errors on `path`.
    fn io<'a>(context: &'static str, path: &'a Path) -> impl FnOnce(io::Error) -> Self + 'a {
        move |source| Self::Io {
            context,
            path: path.to_path_buf(),
            source,
        }
    }
}

impl fmt::Display for SealedStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                context,
                path,
                source,
            } => write!(
                f,
                "sealed key store: {context} '{}': {source}",
                path.display()
            ),
            Self::Kek(reason) => write!(f, "sealed key store: KEK unavailable: {reason}"),
            Self::Locked(path) => write!(
                f,
                "sealed key store: '{}' is locked by another process",
                path.display()
            ),
            Self::NotFound(path) => {
                write!(f, "sealed key store: '{}' does not exist", path.display())
            }
            Self::AlreadyExists(path) => {
                write!(f, "sealed key store: '{}' already exists", path.display())
            }
            Self::TooLarge { path, len } => write!(
                f,
                "sealed key store: '{}' is too large ({len} bytes)",
                path.display()
            ),
            Self::UnsupportedFormat(reason) => {
                write!(f, "sealed key store: unsupported file format: {reason}")
            }
            Self::Authentication => {
                f.write_str("sealed key store: authentication failed (file modified or wrong KEK)")
            }
            Self::Corrupt(reason) => write!(f, "sealed key store: corrupt: {reason}"),
            Self::Crypto(error) => write!(f, "sealed key store: key material: {error}"),
        }
    }
}

impl std::error::Error for SealedStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Crypto(error) => Some(error),
            _ => None,
        }
    }
}
