//! Harw-side key naming: [`NodeId`], [`HarwKeyVersion`], [`HarwKeyRef`].
//!
//! # Naming convention
//! A Harw key lives in the crypto-service namespace `harw.<purpose-label>`
//! (see [`HarwKeyPurpose::namespace`]) under the key id of its owner — the
//! node, service, device or user it belongs to:
//!
//! ```text
//! harw.node-identity/node-a          latest version
//! harw.secrets-kek/vault-main@3      version 3
//! ```
//!
//! The owner name grammar is a strict subset of CryptGuard's `KeyId`
//! grammar (at most 64 instead of 128 bytes), so every [`HarwKeyRef`] maps to
//! a valid service `KeyRef` (see [`crate::cg::key_ref`]).
//!
//! A `HarwKeyRef` is an opaque handle: there is no way to reach private key
//! bytes through it (§4).

use core::{fmt, num::NonZeroU32};

use crate::error::EncryptError;
use crate::purpose::HarwKeyPurpose;

/// Maximum length of a [`NodeId`] in bytes. Fits the one-byte length
/// prefix of [`crate::SecureFrameHeader`].
pub const MAX_NODE_ID_LEN: usize = 64;

/// Name of a key owner: node, service, device or user.
///
/// 1..=[`MAX_NODE_ID_LEN`] bytes of `[A-Za-z0-9._-]`, not starting with `.`
/// — safe in paths, logs and as a crypto-service key id.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(Box<str>);

impl NodeId {
    /// Validate and wrap a name.
    ///
    /// # Errors
    /// [`EncryptError::InvalidName`] if the grammar is violated.
    pub fn new(name: &str) -> Result<Self, EncryptError> {
        if is_valid_name(name.as_bytes()) {
            Ok(Self(name.into()))
        } else {
            Err(EncryptError::InvalidName)
        }
    }

    /// Validate raw wire bytes (UTF-8 is implied by the ASCII grammar).
    ///
    /// # Errors
    /// [`EncryptError::InvalidName`] if the grammar is violated.
    pub(crate) fn from_wire(bytes: &[u8]) -> Result<Self, EncryptError> {
        if !is_valid_name(bytes) {
            return Err(EncryptError::InvalidName);
        }
        core::str::from_utf8(bytes)
            .map(|s| Self(s.into()))
            .map_err(|_| EncryptError::InvalidName)
    }

    /// Borrow the name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn is_valid_name(name: &[u8]) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NODE_ID_LEN
        && name.first() != Some(&b'.')
        && name
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// A key version, starting at 1 (same semantics as CryptGuard's
/// `KeyVersion`, which is the canonical generation type — drift report D4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HarwKeyVersion(NonZeroU32);

impl HarwKeyVersion {
    /// The first version of every key.
    pub const FIRST: Self = Self(NonZeroU32::MIN);

    /// Wrap a version number.
    ///
    /// # Errors
    /// [`EncryptError::InvalidKeyVersion`] for `0`.
    pub fn new(version: u32) -> Result<Self, EncryptError> {
        NonZeroU32::new(version)
            .map(Self)
            .ok_or(EncryptError::InvalidKeyVersion)
    }

    /// The version number.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

impl fmt::Display for HarwKeyVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A purpose-typed reference to a Harw key.
///
/// # Description
/// Carries the purpose explicitly so that the usage policy is checked
/// *before* dispatch (§4, "Key purpose is checked before dispatch").
/// Clone is fine: a reference is a name, not key material (§10).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HarwKeyRef {
    purpose: HarwKeyPurpose,
    owner: NodeId,
    version: Option<HarwKeyVersion>,
}

impl HarwKeyRef {
    /// The current primary version of `owner`'s key of `purpose`.
    #[must_use]
    pub fn latest(purpose: HarwKeyPurpose, owner: NodeId) -> Self {
        Self {
            purpose,
            owner,
            version: None,
        }
    }

    /// One specific version.
    #[must_use]
    pub fn versioned(purpose: HarwKeyPurpose, owner: NodeId, version: HarwKeyVersion) -> Self {
        Self {
            purpose,
            owner,
            version: Some(version),
        }
    }

    /// The key purpose.
    #[must_use]
    pub const fn purpose(&self) -> HarwKeyPurpose {
        self.purpose
    }

    /// The owner, which is also the crypto-service key id.
    #[must_use]
    pub const fn owner(&self) -> &NodeId {
        &self.owner
    }

    /// The version, or `None` for "current primary".
    #[must_use]
    pub const fn version(&self) -> Option<HarwKeyVersion> {
        self.version
    }

    /// The crypto-service namespace, `harw.<purpose>`.
    #[must_use]
    pub const fn namespace(&self) -> &'static str {
        self.purpose.namespace()
    }
}

impl fmt::Display for HarwKeyRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.namespace(), self.owner)?;
        match self.version {
            Some(v) => write!(f, "@{v}"),
            None => Ok(()),
        }
    }
}
