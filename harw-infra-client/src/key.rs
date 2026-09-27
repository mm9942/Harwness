//! Key vocabulary of the AuthHub client.
//!
//! These are this crate's own types; no CryptGuard type crosses the public
//! API. They mirror the reference KMS wire model of `crypt_guard_hyper`:
//! a key is addressed as `{namespace}/{id}[@version]`, where names are
//! non-empty, at most [`MAX_KEY_NAME_LEN`] bytes of `[A-Za-z0-9._-]` and do
//! not start with `.` — which makes them safe to embed in a request path
//! without any escaping.

use core::fmt;
use core::num::NonZeroU32;

/// Maximum length of a key namespace or id in bytes (same bound as the hub).
pub const MAX_KEY_NAME_LEN: usize = 128;

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_KEY_NAME_LEN
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// Why a [`KeyRef`] could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyRefError {
    /// The namespace is empty, too long or has characters outside
    /// `[A-Za-z0-9._-]` (or starts with `.`).
    InvalidNamespace,
    /// The id is invalid (same rules as the namespace).
    InvalidId,
    /// Version `0` (versions start at 1; "latest" is expressed by `None`).
    InvalidVersion,
}

impl fmt::Display for KeyRefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidNamespace => "invalid key namespace",
            Self::InvalidId => "invalid key id",
            Self::InvalidVersion => "invalid key version",
        })
    }
}

impl std::error::Error for KeyRefError {}

/// Reference to a key held by the AuthHub: `namespace/id[@version]`.
///
/// `version == None` addresses the latest version.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyRef {
    namespace: String,
    id: String,
    version: Option<NonZeroU32>,
}

impl KeyRef {
    /// The latest version of `namespace/id`.
    pub fn latest(namespace: &str, id: &str) -> Result<Self, KeyRefError> {
        if !valid_name(namespace) {
            return Err(KeyRefError::InvalidNamespace);
        }
        if !valid_name(id) {
            return Err(KeyRefError::InvalidId);
        }
        Ok(Self {
            namespace: namespace.to_owned(),
            id: id.to_owned(),
            version: None,
        })
    }

    /// A fixed version (`>= 1`) of `namespace/id`.
    pub fn versioned(namespace: &str, id: &str, version: u32) -> Result<Self, KeyRefError> {
        Self::latest(namespace, id)?.with_version(version)
    }

    /// The same key pinned to `version` (`>= 1`).
    pub fn with_version(mut self, version: u32) -> Result<Self, KeyRefError> {
        self.version = Some(NonZeroU32::new(version).ok_or(KeyRefError::InvalidVersion)?);
        Ok(self)
    }

    /// The same key, addressing its latest version.
    pub fn into_latest(mut self) -> Self {
        self.version = None;
        self
    }

    /// Namespace.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Id within the namespace.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Pinned version, `None` for "latest".
    pub fn version(&self) -> Option<u32> {
        self.version.map(NonZeroU32::get)
    }

    /// Version on the wire: `0` means "latest".
    pub(crate) fn wire_version(&self) -> u32 {
        self.version().unwrap_or(0)
    }

    /// Build from a hub response (`version == 0` → latest). Invalid names
    /// from the hub are a protocol violation, reported by the caller.
    pub(crate) fn from_wire(namespace: &str, id: &str, version: u32) -> Result<Self, KeyRefError> {
        let key = Self::latest(namespace, id)?;
        if version == 0 {
            Ok(key)
        } else {
            key.with_version(version)
        }
    }

    /// `/v1/keys/{namespace}/{id}[@version]`. Safe without escaping because
    /// the names are validated to `[A-Za-z0-9._-]`.
    pub(crate) fn path(&self) -> String {
        match self.version {
            Some(version) => format!("/v1/keys/{}/{}@{version}", self.namespace, self.id),
            None => format!("/v1/keys/{}/{}", self.namespace, self.id),
        }
    }
}

impl fmt::Display for KeyRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.namespace, self.id)?;
        if let Some(version) = self.version {
            write!(f, "@{version}")?;
        }
        Ok(())
    }
}

/// Key profile (algorithm) names the hub accepts for `generate`.
///
/// A closed set of fixed wire names; the client never builds a profile from
/// free text (no silent downgrade via capability discovery, masterplan §38).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum KeyProfile {
    /// `pq-hpke-default`: post-quantum HPKE (encrypt/decrypt, wrap/unwrap).
    PqHpkeDefault,
    /// `ml-dsa-44`.
    MlDsa44,
    /// `ml-dsa-65`.
    MlDsa65,
    /// `ml-dsa-87`.
    MlDsa87,
}

impl KeyProfile {
    /// Stable wire name.
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::PqHpkeDefault => "pq-hpke-default",
            Self::MlDsa44 => "ml-dsa-44",
            Self::MlDsa65 => "ml-dsa-65",
            Self::MlDsa87 => "ml-dsa-87",
        }
    }

    /// Profile for a wire name, `None` if unknown to this client.
    pub fn from_wire_name(name: &str) -> Option<Self> {
        Some(match name {
            "pq-hpke-default" => Self::PqHpkeDefault,
            "ml-dsa-44" => Self::MlDsa44,
            "ml-dsa-65" => Self::MlDsa65,
            "ml-dsa-87" => Self::MlDsa87,
            _ => return None,
        })
    }
}

/// Context bound into a wrap/unwrap: HPKE `info` and `aad`.
///
/// Not secret; both sides of an unwrap must present the same values that
/// were used to wrap, otherwise the hub answers `422`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyContext {
    /// HPKE `info` (domain separation).
    pub info: Vec<u8>,
    /// Additional authenticated data.
    pub aad: Vec<u8>,
}

impl KeyContext {
    /// A context from `info` and `aad`.
    pub fn new(info: impl Into<Vec<u8>>, aad: impl Into<Vec<u8>>) -> Self {
        Self {
            info: info.into(),
            aad: aad.into(),
        }
    }
}

/// A wrapped (encrypted) key blob as returned by the hub. Opaque, not secret.
#[derive(Clone, PartialEq, Eq)]
pub struct WrappedKey(Vec<u8>);

impl WrappedKey {
    /// Wrap stored bytes (e.g. read back from a secrets record).
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Borrow the blob.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Take the blob.
    pub fn into_vec(self) -> Vec<u8> {
        self.0
    }
}

impl From<Vec<u8>> for WrappedKey {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }
}

impl fmt::Debug for WrappedKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WrappedKey({} bytes)", self.0.len())
    }
}

/// Public key bytes in the hub's encoding for the key's profile.
#[derive(Clone, PartialEq, Eq)]
pub struct PublicKeyBytes(Vec<u8>);

impl PublicKeyBytes {
    /// Wrap bytes.
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Borrow the bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Take the bytes.
    pub fn into_vec(self) -> Vec<u8> {
        self.0
    }
}

impl fmt::Debug for PublicKeyBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKeyBytes({} bytes)", self.0.len())
    }
}

/// Result of `generate` and `rotate`: the concrete new key version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedKey {
    /// The created key, pinned to its version.
    pub key: KeyRef,
    /// Its public key, if the profile has one.
    pub public: Option<PublicKeyBytes>,
}

/// Lifecycle state of a key version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum KeyState {
    /// Usable.
    Enabled,
    /// Temporarily unusable.
    Disabled,
    /// Scheduled for destruction.
    PendingDestruction,
    /// Material destroyed.
    Destroyed,
    /// A state code this client does not know.
    Unknown(u8),
}

impl KeyState {
    /// From the wire code (1 enabled, 2 disabled, 3 pending destruction,
    /// 4 destroyed).
    pub(crate) fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Enabled,
            2 => Self::Disabled,
            3 => Self::PendingDestruction,
            4 => Self::Destroyed,
            other => Self::Unknown(other),
        }
    }
}

/// Metadata of a key version (`describe`). Never contains key material.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyDescription {
    /// The described key, pinned to the resolved version.
    pub key: KeyRef,
    /// Lifecycle state.
    pub state: KeyState,
    /// Profile wire name as reported by the hub (`None` if unnamed). Kept as
    /// text so a profile newer than this client is still visible.
    pub profile_name: Option<String>,
}

impl KeyDescription {
    /// The profile, if it is one this client knows.
    pub fn profile(&self) -> Option<KeyProfile> {
        self.profile_name
            .as_deref()
            .and_then(KeyProfile::from_wire_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    #[test]
    fn test_key_ref_validation() {
        assert!(KeyRef::latest("app", "k1").is_ok());
        assert!(KeyRef::latest("a.b_c-D9", "x").is_ok());
        assert_eq!(KeyRef::latest("", "k"), Err(KeyRefError::InvalidNamespace));
        assert_eq!(
            KeyRef::latest(".hidden", "k"),
            Err(KeyRefError::InvalidNamespace)
        );
        assert_eq!(
            KeyRef::latest("a/b", "k"),
            Err(KeyRefError::InvalidNamespace)
        );
        assert_eq!(
            KeyRef::latest("app", "k:rotate"),
            Err(KeyRefError::InvalidId)
        );
        assert_eq!(KeyRef::latest("app", "k@2"), Err(KeyRefError::InvalidId));
        assert_eq!(KeyRef::latest("app", "k%2F"), Err(KeyRefError::InvalidId));
        let long = "a".repeat(MAX_KEY_NAME_LEN + 1);
        assert_eq!(KeyRef::latest("app", &long), Err(KeyRefError::InvalidId));
        assert_eq!(
            KeyRef::versioned("app", "k", 0),
            Err(KeyRefError::InvalidVersion)
        );
    }

    #[test]
    fn test_key_ref_path_and_display() -> TestResult {
        let latest = KeyRef::latest("app", "k1")?;
        assert_eq!(latest.path(), "/v1/keys/app/k1");
        assert_eq!(latest.to_string(), "app/k1");
        assert_eq!(latest.wire_version(), 0);

        let pinned = KeyRef::versioned("app", "k1", 7)?;
        assert_eq!(pinned.path(), "/v1/keys/app/k1@7");
        assert_eq!(pinned.to_string(), "app/k1@7");
        assert_eq!(pinned.version(), Some(7));
        assert_eq!(pinned.clone().into_latest(), latest);
        Ok(())
    }

    #[test]
    fn test_profile_wire_names_roundtrip() {
        for profile in [
            KeyProfile::PqHpkeDefault,
            KeyProfile::MlDsa44,
            KeyProfile::MlDsa65,
            KeyProfile::MlDsa87,
        ] {
            assert_eq!(
                KeyProfile::from_wire_name(profile.wire_name()),
                Some(profile)
            );
        }
        assert_eq!(KeyProfile::from_wire_name("aes-please"), None);
    }

    #[test]
    fn test_blob_debug_prints_length_only() {
        let wrapped = WrappedKey::new(vec![0xAA; 3]);
        assert_eq!(format!("{wrapped:?}"), "WrappedKey(3 bytes)");
        let public = PublicKeyBytes::new(vec![1, 2]);
        assert_eq!(format!("{public:?}"), "PublicKeyBytes(2 bytes)");
    }
}
