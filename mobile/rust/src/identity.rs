use std::net::SocketAddr;
use std::sync::Arc;

use aws_lc_rs::signature::{KeyPair, ML_DSA_65_SIGNING, PqdsaKeyPair};
use harw_node_transport::{
    LocalNode, ML_DSA_65_PUBLIC_KEY_LEN, ML_DSA_65_SIGNATURE_LEN, NodeIdentity, NodeSigner,
    PinnedPeers, SignFuture, SignerError, TranscriptWrappedSigner, TranscriptWrappedVerifier,
};
use harw_session_remote::NodeEndpoint;
use harw_types::NodeId;
use zeroize::Zeroizing;

use crate::{AdminEnrollmentProof, DeviceEnrollment, IdentityMaterial, MobileError, ServerProfile};

const PROFILE_VERSION: u32 = 1;
const SEED_BYTES: usize = 32;
const MAX_PUBLIC_DOCUMENT_BYTES: usize = 16 * 1024;

struct PhoneSigner(PqdsaKeyPair);

impl NodeSigner for PhoneSigner {
    fn sign<'a>(&'a self, transcript: &'a [u8]) -> SignFuture<'a> {
        Box::pin(async move {
            let mut signature = vec![0_u8; ML_DSA_65_SIGNATURE_LEN];
            let length = self
                .0
                .sign(transcript, &mut signature)
                .map_err(|_| SignerError::new("phone signing failed"))?;
            signature.truncate(length);
            Ok(signature)
        })
    }
}

#[uniffi::export]
pub fn generate_identity(label: String) -> Result<IdentityMaterial, MobileError> {
    let mut seed = Zeroizing::new([0_u8; SEED_BYTES]);
    aws_lc_rs::rand::fill(seed.as_mut())
        .map_err(|_| invalid_identity("secure random generation failed"))?;
    let (enrollment, _) = restore_identity(seed.as_ref(), label)?;
    Ok(IdentityMaterial {
        private_key: seed.to_vec(),
        enrollment,
    })
}

pub(crate) fn restore_identity(
    seed: &[u8],
    label: String,
) -> Result<(DeviceEnrollment, LocalNode), MobileError> {
    check_label(&label).map_err(invalid_identity)?;
    if seed.len() != SEED_BYTES {
        return Err(invalid_identity("wrong private key length"));
    }
    let key = PqdsaKeyPair::from_seed(&ML_DSA_65_SIGNING, seed)
        .map_err(|_| invalid_identity("could not load ML-DSA-65 private key"))?;
    let public_key = key.public_key().as_ref().to_vec();
    let fingerprint = blake3::hash(&public_key);
    let node_id = format!("phone-{}", hex::encode(&fingerprint.as_bytes()[..16]));
    let identity = NodeIdentity {
        node_id: NodeId::try_from_str(&node_id)
            .map_err(|_| invalid_identity("could not create phone node ID"))?,
        key_ref: format!("mobile-keystore/{node_id}"),
        public_key,
    };
    let enrollment = DeviceEnrollment {
        version: PROFILE_VERSION,
        node_id,
        public_key_hex: hex::encode(&identity.public_key),
        label,
    };
    let local = LocalNode::new(
        identity,
        Arc::new(TranscriptWrappedSigner::new(PhoneSigner(key))),
    );
    Ok((enrollment, local))
}

pub fn parse_device_enrollment(json: &str) -> Result<DeviceEnrollment, MobileError> {
    let parse = |detail: &str| invalid_identity(detail);
    if json.len() > MAX_PUBLIC_DOCUMENT_BYTES {
        return Err(parse("enrollment document is too large"));
    }
    let mut enrollment: DeviceEnrollment = serde_json::from_str(json).map_err(|error| {
        invalid_identity(format!(
            "invalid enrollment JSON at line {}, column {}",
            error.line(),
            error.column()
        ))
    })?;
    if enrollment.version != PROFILE_VERSION {
        return Err(parse("unsupported enrollment version"));
    }
    check_node_id(&enrollment.node_id).map_err(parse)?;
    check_label(&enrollment.label).map_err(parse)?;
    let key = decode_public_key(&enrollment.public_key_hex).map_err(parse)?;
    enrollment.public_key_hex = hex::encode(key);
    Ok(enrollment)
}

#[uniffi::export]
pub fn parse_server_profile(json: String) -> Result<ServerProfile, MobileError> {
    if json.len() > MAX_PUBLIC_DOCUMENT_BYTES {
        return Err(invalid_profile("profile document is too large"));
    }
    let profile: ServerProfile = serde_json::from_str(&json).map_err(|error| {
        invalid_profile(format!(
            "invalid profile JSON at line {}, column {}",
            error.line(),
            error.column()
        ))
    })?;
    Ok(ValidatedProfile::new(profile)?.profile)
}

#[derive(Clone)]
pub(crate) struct ValidatedProfile {
    pub profile: ServerProfile,
    pub endpoint: NodeEndpoint,
    pub verifier: TranscriptWrappedVerifier<PinnedPeers>,
    pub onion: Option<harw_onion::OnionLocator>,
}

impl ValidatedProfile {
    pub fn same_authority(&self, other: &Self) -> bool {
        self.endpoint == other.endpoint
            && self.profile.server_public_key_hex == other.profile.server_public_key_hex
    }

    pub fn new(mut profile: ServerProfile) -> Result<Self, MobileError> {
        if profile.version != PROFILE_VERSION {
            return Err(invalid_profile("unsupported connection profile version"));
        }
        check_label(&profile.label).map_err(invalid_profile)?;
        let expected_node = check_node_id(&profile.server_node_id).map_err(invalid_profile)?;
        let addr = profile
            .endpoint
            .parse::<SocketAddr>()
            .map_err(|_| invalid_profile("endpoint must be an IP address with a port"))?;
        if addr.port() == 0 || addr.ip().is_unspecified() || addr.ip().is_multicast() {
            return Err(invalid_profile(
                "endpoint must identify a unicast host and nonzero port",
            ));
        }
        let key = decode_public_key(&profile.server_public_key_hex).map_err(invalid_profile)?;
        profile.endpoint = addr.to_string();
        profile.server_public_key_hex = hex::encode(&key);
        let onion = match profile.onion.as_deref() {
            Some(text) => {
                let locator = text.parse::<harw_onion::OnionLocator>().map_err(|_| {
                    invalid_profile("onion must be a v3 address like <56 characters>.onion:<port>")
                })?;
                profile.onion = Some(locator.to_string());
                Some(locator)
            }
            None => None,
        };
        if let Some(url) = profile.admin_url.as_deref() {
            profile.admin_url = Some(check_admin_url(url).map_err(invalid_profile)?);
        }
        let mut pins = PinnedPeers::new();
        pins.pin(expected_node.clone(), key)
            .map_err(|_| invalid_profile("invalid pinned ML-DSA-65 public key"))?;
        Ok(Self {
            profile,
            endpoint: NodeEndpoint {
                addr,
                expected_node,
            },
            verifier: TranscriptWrappedVerifier::new(pins),
            onion,
        })
    }
}

/// Validates and normalizes a public admin base URL: `https://host[:port][/path]` (or plain
/// `http://` only for loopback), printable ASCII, no credentials, query or fragment.
pub(crate) fn check_admin_url(value: &str) -> Result<String, &'static str> {
    const BAD: &str = "admin URL must be https://host (http only for localhost)";
    let value = value.trim().trim_end_matches('/');
    if value.is_empty()
        || value.len() > 256
        || !value.bytes().all(|b| b.is_ascii_graphic())
        || value.contains(['@', '?', '#', '\\'])
    {
        return Err(BAD);
    }
    let valid = if let Some(rest) = value.strip_prefix("https://") {
        !rest.is_empty() && !rest.starts_with('/') && !rest.starts_with(':')
    } else if let Some(rest) = value
        .strip_prefix("http://127.0.0.1")
        .or_else(|| value.strip_prefix("http://localhost"))
    {
        // Only a bare loopback host (optionally with port or path) counts as loopback.
        rest.is_empty() || rest.starts_with(':') || rest.starts_with('/')
    } else {
        false
    };
    if !valid {
        return Err(BAD);
    }
    Ok(value.to_owned())
}

/// Builds the enrollment fields and the proof-of-possession signature the admin service
/// verifies: the key signs the server-issued nonce together with the public fields.
#[uniffi::export]
pub fn sign_admin_enrollment(
    private_key: Vec<u8>,
    label: String,
    nonce_hex: String,
) -> Result<AdminEnrollmentProof, MobileError> {
    let seed = Zeroizing::new(private_key);
    let (enrollment, _) = restore_identity(&seed, label)?;
    let nonce = hex::decode(nonce_hex.trim())
        .ok()
        .filter(|bytes| bytes.len() == 32)
        .ok_or_else(|| invalid_identity("the server nonce is invalid"))?;
    let key = PqdsaKeyPair::from_seed(&ML_DSA_65_SIGNING, seed.as_ref())
        .map_err(|_| invalid_identity("could not load ML-DSA-65 private key"))?;
    let public_key = key.public_key().as_ref().to_vec();
    let message = harw_node_transport::enrollment_proof_message(
        &nonce,
        &enrollment.node_id,
        &public_key,
        &enrollment.label,
    );
    let mut signature = vec![0_u8; ML_DSA_65_SIGNATURE_LEN];
    let length = key
        .sign(&message, &mut signature)
        .map_err(|_| invalid_identity("enrollment signing failed"))?;
    signature.truncate(length);
    Ok(AdminEnrollmentProof {
        node_id: enrollment.node_id,
        public_key_hex: enrollment.public_key_hex,
        label: enrollment.label,
        signature_hex: hex::encode(signature),
    })
}

fn check_node_id(value: &str) -> Result<NodeId, &'static str> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err("invalid node ID");
    }
    NodeId::try_from_str(value).map_err(|_| "invalid node ID")
}

fn check_label(value: &str) -> Result<(), &'static str> {
    if value.trim().is_empty() || value.len() > 128 || value.chars().any(char::is_control) {
        return Err("label must contain between 1 and 128 bytes without control characters");
    }
    Ok(())
}

fn decode_public_key(value: &str) -> Result<Vec<u8>, &'static str> {
    if value.len() != ML_DSA_65_PUBLIC_KEY_LEN * 2 {
        return Err("wrong ML-DSA-65 public key length");
    }
    hex::decode(value).map_err(|_| "public key must be hexadecimal")
}

fn invalid_identity(detail: impl Into<String>) -> MobileError {
    MobileError::InvalidIdentity {
        detail: detail.into(),
    }
}

fn invalid_profile(detail: impl Into<String>) -> MobileError {
    MobileError::InvalidProfile {
        detail: detail.into(),
    }
}
