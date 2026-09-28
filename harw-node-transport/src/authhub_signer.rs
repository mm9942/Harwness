//! AuthHub-backed node signer and the matching transcript-wrapping verifier.
//!
//! # Why the transcript is wrapped
//! The node-identity key lives in the Auth/Crypto Hub under
//! `harw.node-identity/<node_id>` (naming: `harw_dod_encrypt::HarwKeyRef`,
//! `HarwKeyPurpose::NodeIdentity`, mapped by `harw_dod_encrypt::cg::key_ref`).
//! The hub's `HarwUsageAuthorizer` refuses every `Sign` on a `harw.*` key
//! whose message is not a well-formed `harw_dod_encrypt::SignTranscript`
//! of that key's own sign purpose (here `SignPurpose::NodeHandshake`), and
//! caps it at `MAX_SIGNABLE_TRANSCRIPT_LEN`. The raw node-transport
//! transcript ([`crate::handshake::Transcript::signing_bytes`], starting with
//! [`TRANSCRIPT_DOMAIN`]) is not such a transcript, so it is embedded
//! **whole** as one field:
//!
//! ```text
//! SignTranscript::builder(SignPurpose::NodeHandshake)
//!     .field("transport", <raw node-transport transcript>)
//!     .build_for_kms()
//! ```
//!
//! [`wrap_transcript`] is that one pure function; signer and verifier both
//! call it, so they agree byte for byte.
//!
//! Why not `SignTranscript::node_handshake(node, challenge, peer,
//! issued_at)`: the transport transcript binds more than that §5 example
//! (signing role, both nonces, both timestamps, protocol version and the TLS
//! exporter). Embedding the canonical transport bytes keeps every binding,
//! needs no parser on the verifier side (it recomputes the wrap from the
//! transcript it already built) and keeps [`TRANSCRIPT_DOMAIN`] inside the
//! signed bytes as a second, transport-level domain separator.
//!
//! # Compatibility decision
//! Wrapping changes the bytes under the ML-DSA-65 signature; the handshake
//! messages, their codec and [`crate::PROTOCOL_VERSION`] are unchanged.
//! - A node whose key is in the AuthHub signs through
//!   [`AuthHubNodeSigner`]; every peer that pins it must verify through
//!   [`TranscriptWrappedVerifier`]. Plain [`crate::PinnedPeers`] refuses those
//!   signatures (`VerifyError::BadSignature`) and vice versa: a mismatch
//!   fails **closed**, it never authenticates.
//! - The wrapped form is a fleet-wide setting, not negotiated per
//!   connection and not accepted side by side with the raw form. There is
//!   deliberately no "try wrapped, then raw" verifier: a single accepted
//!   encoding per deployment keeps the verdict a pure function of the
//!   configuration and gives an operator one thing to audit.
//! - In-process signers (development, tests) join a wrapped fleet through
//!   [`TranscriptWrappedSigner`].
//!
//! # AuthHub call seam
//! [`AuthHubSign`] is the one call the signer needs: "sign `message` with
//! the hub key `namespace/key_id`". `harw_infra_client::AuthHubClient` does
//! not expose the `…:sign` route yet (drift: it has generate/describe/public/
//! rotate/wrap/unwrap/rewrap only), and `harw-infra-client` is the only crate
//! that may open `secure.sock` (masterplan §11/§24/§34 H4), so no socket is
//! dialled here. Once the client grows `sign`, the production adapter is a
//! one-line [`AuthHubSign`] impl at the composition root (or here, behind a
//! `harw-infra-client` dependency).

use std::fmt;
use std::sync::Arc;

use aws_lc_rs::signature::{ML_DSA_65, UnparsedPublicKey};
use harw_dod_encrypt::{
    HarwKeyPurpose, HarwKeyRef, KeyUsagePolicy, NodeId as HarwNodeId, SignPurpose, SignTranscript,
};
use harw_types::NodeId;

use crate::error::{SignerError, VerifyError};
use crate::handshake::TRANSCRIPT_DOMAIN;
use crate::identity::{
    ML_DSA_65_PUBLIC_KEY_LEN, ML_DSA_65_SIGNATURE_LEN, NodeSigner, NodeVerifier, SignFuture,
};

/// Tag of the one field that carries the raw transport transcript inside
/// the wrapping `SignTranscript`.
pub const WRAPPED_TRANSCRIPT_FIELD: &str = "transport";

/// Why a transcript could not be wrapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrapError {
    /// The payload does not start with [`TRANSCRIPT_DOMAIN`]; it is not a
    /// node-transport transcript and must not be signed with a node key.
    NotATransportTranscript,
    /// The wrapped transcript exceeds what the KMS sign/verify routes
    /// accept (`harw_dod_encrypt::MAX_SIGNABLE_TRANSCRIPT_LEN`).
    TooLarge,
}

impl fmt::Display for WrapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotATransportTranscript => {
                f.write_str("payload is not a node-transport handshake transcript")
            }
            Self::TooLarge => f.write_str("wrapped node transcript exceeds the KMS sign limit"),
        }
    }
}

impl std::error::Error for WrapError {}

/// Wraps a raw node-transport transcript into the canonical
/// `SignTranscript` bytes of purpose `NodeHandshake` that are actually
/// signed and verified (see the module docs).
///
/// # Errors
/// - [`WrapError::NotATransportTranscript`] unless `transcript` starts with
///   [`TRANSCRIPT_DOMAIN`] (defense in depth: the node key is never a
///   general signing oracle, masterplan §5).
/// - [`WrapError::TooLarge`] above the KMS transcript limit.
pub fn wrap_transcript(transcript: &[u8]) -> Result<Vec<u8>, WrapError> {
    wrap(transcript).map(SignTranscript::into_bytes)
}

fn wrap(transcript: &[u8]) -> Result<SignTranscript, WrapError> {
    if !transcript.starts_with(TRANSCRIPT_DOMAIN) {
        return Err(WrapError::NotATransportTranscript);
    }
    SignTranscript::builder(SignPurpose::NodeHandshake)
        .field(WRAPPED_TRANSCRIPT_FIELD, transcript)
        .build_for_kms()
        .map_err(|_| WrapError::TooLarge)
}

/// The AuthHub `Sign` call the node signer needs.
///
/// `namespace` / `key_id` name the hub key (for node identities
/// `harw.node-identity` / `<node_id>`, latest version); `message` is the
/// complete byte string to sign (a wrapped `SignTranscript`). The
/// implementation returns the raw signature bytes and maps every transport
/// or hub error to a [`SignerError`] without key material or payload bytes.
pub trait AuthHubSign: Send + Sync {
    /// `POST /v1/keys/{namespace}/{key_id}:sign` with `message`.
    fn sign_with_key<'a>(
        &'a self,
        namespace: &'a str,
        key_id: &'a str,
        message: Vec<u8>,
    ) -> SignFuture<'a>;
}

/// [`NodeSigner`] backed by the node-identity key in the Auth/Crypto Hub.
///
/// For every payload it
/// 1. refuses anything that does not start with [`TRANSCRIPT_DOMAIN`]
///    (locally, before any hub call),
/// 2. wraps it with [`wrap_transcript`] and re-checks the result with
///    `KeyUsagePolicy::authorize_sign(NodeIdentity, …)` — the same rule
///    the hub's authorizer applies,
/// 3. asks the hub to sign under `harw.node-identity/<node_id>`,
/// 4. checks the signature length and, when a pinned public key was given
///    ([`Self::with_self_check`]), verifies the signature locally so a hub
///    key that no longer matches the published identity is caught here
///    instead of as a peer's `BadSignature`.
///
/// Peers must verify with [`TranscriptWrappedVerifier`].
pub struct AuthHubNodeSigner {
    hub: Arc<dyn AuthHubSign>,
    key: HarwKeyRef,
    expected_public_key: Option<Vec<u8>>,
}

impl AuthHubNodeSigner {
    /// Signer for the node-identity key of `node` (latest version).
    ///
    /// # Errors
    /// [`SignerError`] when `node` does not satisfy the hub key-name grammar
    /// (1..=64 bytes of `[A-Za-z0-9._-]`, not starting with `.`).
    pub fn new(hub: Arc<dyn AuthHubSign>, node: &NodeId) -> Result<Self, SignerError> {
        let owner = HarwNodeId::new(node.as_str())
            .map_err(|_| SignerError::new("node id is not a valid AuthHub key id"))?;
        Ok(Self {
            hub,
            key: HarwKeyRef::latest(HarwKeyPurpose::NodeIdentity, owner),
            expected_public_key: None,
        })
    }

    /// Additionally verify every hub signature against `public_key` (the
    /// node's published [`crate::NodeIdentity::public_key`]) before
    /// returning it.
    ///
    /// # Errors
    /// [`SignerError`] if `public_key` is not ML-DSA-65-sized.
    pub fn with_self_check(mut self, public_key: Vec<u8>) -> Result<Self, SignerError> {
        if public_key.len() != ML_DSA_65_PUBLIC_KEY_LEN {
            return Err(SignerError::new(
                "self-check public key is not an ML-DSA-65 public key",
            ));
        }
        self.expected_public_key = Some(public_key);
        Ok(self)
    }

    /// The hub key this signer uses, e.g. `harw.node-identity/node-a`.
    #[must_use]
    pub fn key(&self) -> &HarwKeyRef {
        &self.key
    }

    async fn sign_wrapped(&self, transcript: &[u8]) -> Result<Vec<u8>, SignerError> {
        let wrapped = wrap(transcript).map_err(|err| SignerError::new(err.to_string()))?;
        // Mirror of the hub's `HarwUsageAuthorizer`: the node-identity key
        // signs only `NodeHandshake` transcripts.
        KeyUsagePolicy::authorize_sign(self.key.purpose(), &wrapped).map_err(|_| {
            SignerError::new("wrapped transcript purpose does not match the node-identity key")
        })?;
        let wrapped = wrapped.into_bytes();
        let signature = self
            .hub
            .sign_with_key(
                self.key.namespace(),
                self.key.owner().as_str(),
                wrapped.clone(),
            )
            .await?;
        if signature.len() != ML_DSA_65_SIGNATURE_LEN {
            return Err(SignerError::new(
                "AuthHub returned a signature that is not ML-DSA-65-sized",
            ));
        }
        if let Some(public_key) = &self.expected_public_key {
            UnparsedPublicKey::new(&ML_DSA_65, public_key.as_slice())
                .verify(&wrapped, &signature)
                .map_err(|_| {
                    SignerError::new(
                        "AuthHub signature does not verify under the node's published key",
                    )
                })?;
        }
        Ok(signature)
    }
}

impl NodeSigner for AuthHubNodeSigner {
    fn sign<'a>(&'a self, transcript: &'a [u8]) -> SignFuture<'a> {
        Box::pin(self.sign_wrapped(transcript))
    }
}

impl fmt::Debug for AuthHubNodeSigner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthHubNodeSigner")
            .field("key", &self.key.to_string())
            .field("self_check", &self.expected_public_key.is_some())
            .finish_non_exhaustive()
    }
}

/// Adapts an in-process [`NodeSigner`] (development, tests) to the wrapped
/// transcript form, so it interoperates with [`TranscriptWrappedVerifier`].
/// It refuses non-transport payloads exactly like [`AuthHubNodeSigner`].
pub struct TranscriptWrappedSigner<S> {
    inner: S,
}

impl<S: NodeSigner> TranscriptWrappedSigner<S> {
    /// Wraps `inner`.
    pub const fn new(inner: S) -> Self {
        Self { inner }
    }

    /// The wrapped signer.
    pub const fn inner(&self) -> &S {
        &self.inner
    }
}

impl<S: NodeSigner> NodeSigner for TranscriptWrappedSigner<S> {
    fn sign<'a>(&'a self, transcript: &'a [u8]) -> SignFuture<'a> {
        Box::pin(async move {
            let wrapped =
                wrap_transcript(transcript).map_err(|err| SignerError::new(err.to_string()))?;
            self.inner.sign(&wrapped).await
        })
    }
}

impl<S> fmt::Debug for TranscriptWrappedSigner<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TranscriptWrappedSigner")
            .finish_non_exhaustive()
    }
}

/// [`NodeVerifier`] for peers whose keys sign wrapped transcripts
/// ([`AuthHubNodeSigner`], [`TranscriptWrappedSigner`]): recomputes
/// [`wrap_transcript`] over the transcript the handshake built and hands
/// those bytes to `inner` (normally [`crate::PinnedPeers`]).
///
/// A transcript that cannot be wrapped (no [`TRANSCRIPT_DOMAIN`] prefix,
/// over the KMS limit) is `BadSignature`: nothing a peer signed can match
/// it.
#[derive(Clone, Debug, Default)]
pub struct TranscriptWrappedVerifier<V> {
    inner: V,
}

impl<V: NodeVerifier> TranscriptWrappedVerifier<V> {
    /// Wraps `inner`.
    pub const fn new(inner: V) -> Self {
        Self { inner }
    }

    /// The wrapped verifier (e.g. to pin or unpin keys).
    pub const fn inner(&self) -> &V {
        &self.inner
    }

    /// Mutable access to the wrapped verifier.
    pub fn inner_mut(&mut self) -> &mut V {
        &mut self.inner
    }
}

impl<V: NodeVerifier> NodeVerifier for TranscriptWrappedVerifier<V> {
    fn is_known(&self, node: &NodeId) -> bool {
        self.inner.is_known(node)
    }

    fn verify(
        &self,
        node: &NodeId,
        transcript: &[u8],
        signature: &[u8],
    ) -> Result<(), VerifyError> {
        if !self.inner.is_known(node) {
            return Err(VerifyError::UnknownPeer(node.clone()));
        }
        let wrapped =
            wrap_transcript(transcript).map_err(|_| VerifyError::BadSignature(node.clone()))?;
        self.inner.verify(node, &wrapped, signature)
    }
}

/// The hub authorizer's rule, for the in-test fake hub: whether `message`
/// is something the hub would let a node-identity key sign.
#[cfg(test)]
fn hub_would_sign(message: &[u8]) -> bool {
    SignTranscript::validate_for_kms(message)
        .is_ok_and(|purpose| Some(purpose) == HarwKeyPurpose::NodeIdentity.sign_purpose())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::handshake::{
        ClientFinish, ClientHandshake, HandshakePolicy, ReplayCache, ServerHandshake,
    };
    use crate::identity::{LocalNode, NodeIdentity, PinnedPeers};
    use crate::test_support::{DeterministicTestSigner, TestError, TestResult, ctx, ensure};

    const NOW: u64 = 1_700_000_000_000;

    fn node_id(name: &str) -> TestResult<NodeId> {
        NodeId::try_from_str(name).map_err(ctx("node id"))
    }

    /// A raw transport transcript as `Transcript::signing_bytes` emits it
    /// (prefix is what matters to the signer).
    fn transport_transcript(tail: &[u8]) -> Vec<u8> {
        let mut out = TRANSCRIPT_DOMAIN.to_vec();
        out.extend_from_slice(tail);
        out
    }

    /// In-process fake AuthHub: one deterministic ML-DSA-65 key under
    /// `harw.node-identity/<owner>`, enforcing the hub's usage rule (only
    /// `NodeHandshake` sign transcripts) and recording every call.
    struct FakeHub {
        owner: String,
        key: DeterministicTestSigner,
        calls: Mutex<Vec<(String, String, Vec<u8>)>>,
    }

    impl FakeHub {
        fn new(owner: &str, seed: u8) -> TestResult<Self> {
            Ok(Self {
                owner: owner.to_owned(),
                key: DeterministicTestSigner::from_seed(seed)?,
                calls: Mutex::new(Vec::new()),
            })
        }

        fn calls(&self) -> Vec<(String, String, Vec<u8>)> {
            self.calls
                .lock()
                .map(|calls| calls.clone())
                .unwrap_or_default()
        }
    }

    impl AuthHubSign for FakeHub {
        fn sign_with_key<'a>(
            &'a self,
            namespace: &'a str,
            key_id: &'a str,
            message: Vec<u8>,
        ) -> SignFuture<'a> {
            Box::pin(async move {
                if let Ok(mut calls) = self.calls.lock() {
                    calls.push((namespace.to_owned(), key_id.to_owned(), message.clone()));
                }
                if namespace != "harw.node-identity" || key_id != self.owner {
                    return Err(SignerError::new("not found"));
                }
                if !hub_would_sign(&message) {
                    return Err(SignerError::new("forbidden"));
                }
                self.key.sign(&message).await
            })
        }
    }

    fn hub_signer(hub: &Arc<FakeHub>, node: &str) -> TestResult<AuthHubNodeSigner> {
        let hub: Arc<dyn AuthHubSign> = hub.clone();
        Ok(AuthHubNodeSigner::new(hub, &node_id(node)?)?)
    }

    #[test]
    fn test_wrap_is_a_node_handshake_sign_transcript_and_deterministic() -> TestResult {
        let raw = transport_transcript(b"client-signs|...");
        let wrapped = wrap_transcript(&raw).map_err(ctx("wrap"))?;
        assert_eq!(
            SignTranscript::validate_for_kms(&wrapped).map_err(ctx("validate"))?,
            SignPurpose::NodeHandshake
        );
        assert!(hub_would_sign(&wrapped));
        // Byte-exact with the canonical builder, and the raw transcript is
        // carried whole (suffix of the encoding).
        let expected = SignTranscript::builder(SignPurpose::NodeHandshake)
            .field(WRAPPED_TRANSCRIPT_FIELD, &raw)
            .build()
            .map_err(ctx("builder"))?;
        assert_eq!(wrapped, expected.as_bytes());
        ensure(wrapped.ends_with(&raw), "raw transcript not embedded whole")?;
        assert_eq!(wrap_transcript(&raw).map_err(ctx("wrap again"))?, wrapped);
        // The raw transcript itself is not signable by the hub.
        assert!(!hub_would_sign(&raw));
        Ok(())
    }

    #[test]
    fn test_wrap_refuses_non_domain_and_oversize_payloads() -> TestResult {
        assert_eq!(
            wrap_transcript(b"anything else"),
            Err(WrapError::NotATransportTranscript)
        );
        assert_eq!(
            wrap_transcript(b""),
            Err(WrapError::NotATransportTranscript)
        );
        // A foreign sign transcript is not a transport transcript either.
        let foreign = SignTranscript::builder(SignPurpose::ArtifactManifest)
            .field("digest", &[1; 32])
            .build()
            .map(SignTranscript::into_bytes)
            .map_err(ctx("foreign transcript"))?;
        assert_eq!(
            wrap_transcript(&foreign),
            Err(WrapError::NotATransportTranscript)
        );
        let huge = transport_transcript(&vec![0u8; 64 * 1024]);
        assert_eq!(wrap_transcript(&huge), Err(WrapError::TooLarge));
        Ok(())
    }

    #[tokio::test]
    async fn test_signer_refuses_non_domain_payloads_without_calling_the_hub() -> TestResult {
        let hub = Arc::new(FakeHub::new("node-a", 1)?);
        let signer = hub_signer(&hub, "node-a")?;
        let payloads: [&[u8]; 3] = [
            b"not a transcript",
            b"",
            b"harw-node-transport/handshake/v1", // prefix without the NUL
        ];
        for payload in payloads {
            match signer.sign(payload).await {
                Err(_) => {}
                Ok(_) => {
                    return Err(TestError::Unexpected(
                        "non-domain payload was signed".to_owned(),
                    ));
                }
            }
        }
        ensure(
            hub.calls().is_empty(),
            "hub was called for refused payloads",
        )
    }

    #[tokio::test]
    async fn test_signer_uses_node_identity_key_and_sends_wrapped_transcript() -> TestResult {
        let hub = Arc::new(FakeHub::new("node-a", 1)?);
        let signer = hub_signer(&hub, "node-a")?;
        assert_eq!(signer.key().to_string(), "harw.node-identity/node-a");

        let raw = transport_transcript(b"payload");
        let signature = signer.sign(&raw).await?;
        assert_eq!(signature.len(), ML_DSA_65_SIGNATURE_LEN);

        let calls = hub.calls();
        let [(namespace, key_id, message)] = calls.as_slice() else {
            return Err(TestError::Unexpected(format!("{} hub calls", calls.len())));
        };
        assert_eq!(namespace, "harw.node-identity");
        assert_eq!(key_id, "node-a");
        assert_eq!(message, &wrap_transcript(&raw).map_err(ctx("wrap"))?);
        Ok(())
    }

    #[tokio::test]
    async fn test_signer_rejects_invalid_key_id_and_hub_errors() -> TestResult {
        let hub = Arc::new(FakeHub::new("node-a", 1)?);
        let as_dyn: Arc<dyn AuthHubSign> = hub.clone();
        ensure(
            AuthHubNodeSigner::new(as_dyn, &NodeId("../x".to_owned())).is_err(),
            "invalid key id accepted",
        )?;
        // Hub has no key for node-b: the hub error surfaces as SignerError.
        let signer = hub_signer(&hub, "node-b")?;
        ensure(
            signer.sign(&transport_transcript(b"x")).await.is_err(),
            "missing hub key produced a signature",
        )
    }

    #[tokio::test]
    async fn test_self_check_catches_a_hub_key_that_does_not_match() -> TestResult {
        let hub = Arc::new(FakeHub::new("node-a", 1)?);
        let published = DeterministicTestSigner::from_seed(1)?.public_key();
        let other = DeterministicTestSigner::from_seed(9)?.public_key();
        let raw = transport_transcript(b"payload");

        let good = hub_signer(&hub, "node-a")?.with_self_check(published)?;
        good.sign(&raw).await?;

        let stale = hub_signer(&hub, "node-a")?.with_self_check(other)?;
        ensure(
            stale.sign(&raw).await.is_err(),
            "self-check accepted a signature under a different key",
        )?;
        ensure(
            hub_signer(&hub, "node-a")?
                .with_self_check(vec![0; 10])
                .is_err(),
            "short self-check key accepted",
        )
    }

    #[tokio::test]
    async fn test_wrapped_sign_and_verify_round_trip_with_deterministic_key() -> TestResult {
        let hub = Arc::new(FakeHub::new("node-a", 1)?);
        let signer = hub_signer(&hub, "node-a")?;
        let a = node_id("node-a")?;
        let public_key = DeterministicTestSigner::from_seed(1)?.public_key();

        let mut pins = PinnedPeers::new();
        pins.pin(a.clone(), public_key).map_err(ctx("pin"))?;
        let wrapped_verifier = TranscriptWrappedVerifier::new(pins.clone());

        let raw = transport_transcript(b"server-signs|binding");
        let signature = signer.sign(&raw).await?;
        wrapped_verifier
            .verify(&a, &raw, &signature)
            .map_err(ctx("wrapped verify"))?;

        // Compatibility: a plain verifier over the raw bytes refuses it
        // (fails closed), and a tampered transcript never verifies.
        assert_eq!(
            pins.verify(&a, &raw, &signature),
            Err(VerifyError::BadSignature(a.clone()))
        );
        let mut tampered = raw.clone();
        if let Some(last) = tampered.last_mut() {
            *last ^= 1;
        }
        assert_eq!(
            wrapped_verifier.verify(&a, &tampered, &signature),
            Err(VerifyError::BadSignature(a.clone()))
        );
        // Unknown peers stay unknown; non-domain transcripts are bad.
        let b = node_id("node-b")?;
        assert!(!wrapped_verifier.is_known(&b));
        assert_eq!(
            wrapped_verifier.verify(&b, &raw, &signature),
            Err(VerifyError::UnknownPeer(b.clone()))
        );
        assert_eq!(
            wrapped_verifier.verify(&a, b"no domain", &signature),
            Err(VerifyError::BadSignature(a.clone()))
        );
        // The in-process adapter produces the same verifiable form.
        let local = TranscriptWrappedSigner::new(DeterministicTestSigner::from_seed(1)?);
        let local_sig = local.sign(&raw).await?;
        wrapped_verifier
            .verify(&a, &raw, &local_sig)
            .map_err(ctx("in-process wrapped verify"))?;
        ensure(
            local.sign(b"no domain").await.is_err(),
            "wrapped in-process signer signed a non-domain payload",
        )
    }

    #[tokio::test]
    async fn test_full_handshake_with_authhub_signer_and_wrapped_verifiers() -> TestResult {
        // node-a (server) signs through the fake AuthHub, node-b (client)
        // through the in-process wrapped adapter; both verify wrapped.
        let a = node_id("node-a")?;
        let b = node_id("node-b")?;
        let hub = Arc::new(FakeHub::new("node-a", 1)?);
        let a_public = DeterministicTestSigner::from_seed(1)?.public_key();
        let a_signer = hub_signer(&hub, "node-a")?.with_self_check(a_public.clone())?;
        let b_key = DeterministicTestSigner::from_seed(2)?;
        let b_public = b_key.public_key();
        let a_local = LocalNode::new(
            NodeIdentity {
                node_id: a.clone(),
                public_key: a_public.clone(),
                key_ref: a_signer.key().to_string(),
            },
            Arc::new(a_signer),
        );
        let b_signer = TranscriptWrappedSigner::new(b_key);

        let mut server_pins = PinnedPeers::new();
        server_pins.pin(b.clone(), b_public).map_err(ctx("pin b"))?;
        let mut client_pins = PinnedPeers::new();
        client_pins.pin(a.clone(), a_public).map_err(ctx("pin a"))?;
        let server_trust = TranscriptWrappedVerifier::new(server_pins);
        let client_trust = TranscriptWrappedVerifier::new(client_pins);
        let policy = HandshakePolicy::default();
        let replay = ReplayCache::new(&policy, 4);
        let binding = [7; 32];

        let (client, hello) = ClientHandshake::start(&b, &a, [1; 32], NOW);
        let pending = ServerHandshake::accept_hello(
            &a,
            &hello,
            [0xA5; 32],
            NOW,
            binding,
            &policy,
            &replay,
            &server_trust,
        )?;
        let server_sig = a_local.signer.sign(&pending.signing_payload()).await?;
        let server_hello = pending.server_hello(server_sig);
        let client_pending =
            client.on_server_hello(&server_hello, binding, &client_trust, &policy, NOW)?;
        assert_eq!(client_pending.peer().node_id, a);
        let finish = ClientFinish {
            signature: b_signer.sign(&client_pending.signing_payload()).await?,
        };
        let peer = pending.finish(&finish, &server_trust, &replay, NOW)?;
        assert_eq!(peer.node_id, b);

        // Exactly one hub call, for the server role, in wrapped form.
        let calls = hub.calls();
        ensure(calls.len() == 1, format!("{} hub calls", calls.len()))?;
        Ok(())
    }
}
