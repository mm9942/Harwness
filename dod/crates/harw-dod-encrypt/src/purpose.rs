//! Harw key purposes (Masterplan v2 §4) and sign purposes (§5).
//!
//! A [`HarwKeyPurpose`] is the semantic class of a key. It decides the key's
//! [`KeyClass`] (what kind of cryptography it does), its namespace in the
//! crypto service (`harw.<purpose>`) and, for signing keys, the single
//! [`SignPurpose`] whose transcripts it may sign.
//!
//! The mapping purpose -> sign purpose is one-to-one on purpose: a
//! `NodeIdentity` key signs node-handshake transcripts and nothing else, so it
//! never becomes an arbitrary document-signing oracle (§4, last line).

use core::fmt;

/// The semantic purpose of a Harw key (§4).
///
/// # Description
/// Exhaustive on purpose (not `#[non_exhaustive]`): a new purpose must force
/// every policy `match` in the workspace to be revisited.
///
/// The wire code ([`Self::code`]) and namespace label ([`Self::label`]) are
/// stable: both appear in persisted key names and in
/// [`crate::SecureFrameHeader`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HarwKeyPurpose {
    /// Long-term identity of a node (host).
    NodeIdentity,
    /// Long-term identity of a local service.
    ServiceIdentity,
    /// Identity of an enrolled device.
    DeviceIdentity,
    /// Identity of a user.
    UserIdentity,
    /// Key-encryption key of `harw-secrets` (wraps data keys).
    SecretsKek,
    /// Recipient key binding a secure channel (HPKE encrypt/decrypt).
    ChannelBinding,
    /// Authenticates secure requests (signs secure frames).
    RequestAuthentication,
    /// Signs audit-log checkpoints.
    AuditCheckpoint,
    /// Signs artifact manifests.
    ArtifactSigning,
    /// Keyed pseudonymization of identifiers.
    Pseudonymization,
}

impl HarwKeyPurpose {
    /// Every purpose, in code order.
    pub const ALL: [Self; 10] = [
        Self::NodeIdentity,
        Self::ServiceIdentity,
        Self::DeviceIdentity,
        Self::UserIdentity,
        Self::SecretsKek,
        Self::ChannelBinding,
        Self::RequestAuthentication,
        Self::AuditCheckpoint,
        Self::ArtifactSigning,
        Self::Pseudonymization,
    ];

    /// Stable kebab-case label, used in the namespace and in logs.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::NodeIdentity => "node-identity",
            Self::ServiceIdentity => "service-identity",
            Self::DeviceIdentity => "device-identity",
            Self::UserIdentity => "user-identity",
            Self::SecretsKek => "secrets-kek",
            Self::ChannelBinding => "channel-binding",
            Self::RequestAuthentication => "request-authentication",
            Self::AuditCheckpoint => "audit-checkpoint",
            Self::ArtifactSigning => "artifact-signing",
            Self::Pseudonymization => "pseudonymization",
        }
    }

    /// The crypto-service namespace of keys with this purpose:
    /// `harw.<label>`.
    ///
    /// # Examples
    /// ```
    /// use harw_dod_encrypt::HarwKeyPurpose;
    /// assert_eq!(HarwKeyPurpose::NodeIdentity.namespace(), "harw.node-identity");
    /// ```
    #[must_use]
    pub const fn namespace(self) -> &'static str {
        match self {
            Self::NodeIdentity => "harw.node-identity",
            Self::ServiceIdentity => "harw.service-identity",
            Self::DeviceIdentity => "harw.device-identity",
            Self::UserIdentity => "harw.user-identity",
            Self::SecretsKek => "harw.secrets-kek",
            Self::ChannelBinding => "harw.channel-binding",
            Self::RequestAuthentication => "harw.request-authentication",
            Self::AuditCheckpoint => "harw.audit-checkpoint",
            Self::ArtifactSigning => "harw.artifact-signing",
            Self::Pseudonymization => "harw.pseudonymization",
        }
    }

    /// Reverse of [`Self::namespace`].
    #[must_use]
    pub fn from_namespace(namespace: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.namespace() == namespace)
    }

    /// Stable one-byte wire code (1..=10). `0` is never assigned.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::NodeIdentity => 1,
            Self::ServiceIdentity => 2,
            Self::DeviceIdentity => 3,
            Self::UserIdentity => 4,
            Self::SecretsKek => 5,
            Self::ChannelBinding => 6,
            Self::RequestAuthentication => 7,
            Self::AuditCheckpoint => 8,
            Self::ArtifactSigning => 9,
            Self::Pseudonymization => 10,
        }
    }

    /// Reverse of [`Self::code`]; `None` for an unknown code.
    #[must_use]
    pub fn from_code(code: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.code() == code)
    }

    /// The kind of cryptography a key of this purpose does.
    #[must_use]
    pub const fn class(self) -> KeyClass {
        match self {
            Self::NodeIdentity
            | Self::ServiceIdentity
            | Self::DeviceIdentity
            | Self::UserIdentity
            | Self::RequestAuthentication
            | Self::AuditCheckpoint
            | Self::ArtifactSigning => KeyClass::Signing,
            Self::ChannelBinding => KeyClass::Encryption,
            Self::SecretsKek => KeyClass::Wrapping,
            Self::Pseudonymization => KeyClass::Pseudonymization,
        }
    }

    /// The only [`SignPurpose`] a key of this purpose may sign, or `None`
    /// for a key that never signs.
    #[must_use]
    pub const fn sign_purpose(self) -> Option<SignPurpose> {
        match self {
            Self::NodeIdentity => Some(SignPurpose::NodeHandshake),
            Self::ServiceIdentity => Some(SignPurpose::ServiceHandshake),
            Self::DeviceIdentity => Some(SignPurpose::DeviceHandshake),
            Self::UserIdentity => Some(SignPurpose::UserAssertion),
            Self::RequestAuthentication => Some(SignPurpose::SecureFrame),
            Self::AuditCheckpoint => Some(SignPurpose::AuditCheckpoint),
            Self::ArtifactSigning => Some(SignPurpose::ArtifactManifest),
            Self::SecretsKek | Self::ChannelBinding | Self::Pseudonymization => None,
        }
    }
}

impl fmt::Display for HarwKeyPurpose {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// What kind of cryptography a key does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyClass {
    /// Asymmetric signing key (sign / verify / public key).
    Signing,
    /// HPKE recipient key for data (encrypt / decrypt / public key).
    Encryption,
    /// HPKE recipient key for key material (wrap / unwrap / rewrap).
    Wrapping,
    /// Keyed pseudonymization (PRF). No CryptGuard operation exists for it
    /// yet; such keys only support lifecycle operations.
    Pseudonymization,
}

/// The structured purpose of a signature (§5).
///
/// # Description
/// Each sign purpose has a versioned domain label (`harw:<name>:v1`) that
/// starts every [`crate::SignTranscript`]. Two transcripts of different sign
/// purposes can therefore never be byte-equal, and a signature made for one
/// purpose never verifies for another.
///
/// §5 lists `NodeHandshake`, `ServiceHandshake`, `ArtifactManifest`,
/// `AuditCheckpoint` and `SecureFrame`. `DeviceHandshake` and
/// `UserAssertion` are added so that every signing [`HarwKeyPurpose`] has
/// exactly one sign purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SignPurpose {
    /// Node-to-node handshake.
    NodeHandshake,
    /// Service handshake.
    ServiceHandshake,
    /// Device enrollment/handshake.
    DeviceHandshake,
    /// User assertion (login proof).
    UserAssertion,
    /// Sender authentication of a secure frame (§8).
    SecureFrame,
    /// Audit-log checkpoint.
    AuditCheckpoint,
    /// Artifact manifest.
    ArtifactManifest,
}

impl SignPurpose {
    /// Every sign purpose, in code order.
    pub const ALL: [Self; 7] = [
        Self::NodeHandshake,
        Self::ServiceHandshake,
        Self::DeviceHandshake,
        Self::UserAssertion,
        Self::SecureFrame,
        Self::AuditCheckpoint,
        Self::ArtifactManifest,
    ];

    /// The versioned domain-separation label.
    #[must_use]
    pub const fn domain(self) -> &'static str {
        match self {
            Self::NodeHandshake => "harw:node-handshake:v1",
            Self::ServiceHandshake => "harw:service-handshake:v1",
            Self::DeviceHandshake => "harw:device-handshake:v1",
            Self::UserAssertion => "harw:user-assertion:v1",
            Self::SecureFrame => "harw:secure-frame:v1",
            Self::AuditCheckpoint => "harw:audit-checkpoint:v1",
            Self::ArtifactManifest => "harw:artifact-manifest:v1",
        }
    }

    /// Stable one-byte code (1..=7).
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::NodeHandshake => 1,
            Self::ServiceHandshake => 2,
            Self::DeviceHandshake => 3,
            Self::UserAssertion => 4,
            Self::SecureFrame => 5,
            Self::AuditCheckpoint => 6,
            Self::ArtifactManifest => 7,
        }
    }

    /// Reverse of [`Self::code`].
    #[must_use]
    pub fn from_code(code: u8) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.code() == code)
    }

    /// The one key purpose whose keys may sign this purpose.
    #[must_use]
    pub const fn key_purpose(self) -> HarwKeyPurpose {
        match self {
            Self::NodeHandshake => HarwKeyPurpose::NodeIdentity,
            Self::ServiceHandshake => HarwKeyPurpose::ServiceIdentity,
            Self::DeviceHandshake => HarwKeyPurpose::DeviceIdentity,
            Self::UserAssertion => HarwKeyPurpose::UserIdentity,
            Self::SecureFrame => HarwKeyPurpose::RequestAuthentication,
            Self::AuditCheckpoint => HarwKeyPurpose::AuditCheckpoint,
            Self::ArtifactManifest => HarwKeyPurpose::ArtifactSigning,
        }
    }
}

impl fmt::Display for SignPurpose {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.domain())
    }
}
