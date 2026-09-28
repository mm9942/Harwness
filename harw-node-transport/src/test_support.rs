//! Test scaffolding of this crate: `TestResult` (replaces panic/unwrap/expect
//! in tests) and the **test-only** deterministic node signer.

#![allow(dead_code)] // Shared scaffolding: not every test uses every helper.

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use aws_lc_rs::signature::{KeyPair, ML_DSA_65_SIGNING, PqdsaKeyPair};
use bytes::Bytes;
use harw_types::NodeId;
use http::{Request, Response};
use http_body_util::Full;
use hyper::body::Incoming;

use crate::error::{HandshakeError, SignerError, TransportError, UplinkError};
use crate::identity::{
    LocalNode, ML_DSA_65_SIGNATURE_LEN, NodeIdentity, NodeSigner, PinnedPeers, SignFuture,
};

/// Failure of a test; returned as `Err` instead of panicking.
pub(crate) enum TestError {
    /// An expected value was absent.
    Missing(&'static str),
    /// A result had an unexpected shape.
    Unexpected(String),
    /// A foreign error with context (replaces `expect("…")`).
    Context {
        context: &'static str,
        source: String,
    },
    /// Error of this crate.
    Transport(TransportError),
    /// Handshake verdict.
    Handshake(HandshakeError),
    /// Uplink error.
    Uplink(UplinkError),
}

/// Result type of this crate's tests.
pub(crate) type TestResult<T = ()> = Result<T, TestError>;

impl std::fmt::Display for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TestError::Missing(what) => write!(f, "test: expected value missing: {what}"),
            TestError::Unexpected(detail) => write!(f, "test: unexpected result: {detail}"),
            TestError::Context { context, source } => write!(f, "test: {context}: {source}"),
            TestError::Transport(err) => write!(f, "test: transport error: {err}"),
            TestError::Handshake(err) => write!(f, "test: handshake error: {err}"),
            TestError::Uplink(err) => write!(f, "test: uplink error: {err}"),
        }
    }
}

impl std::fmt::Debug for TestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for TestError {}

impl From<TransportError> for TestError {
    fn from(err: TransportError) -> Self {
        TestError::Transport(err)
    }
}

impl From<HandshakeError> for TestError {
    fn from(err: HandshakeError) -> Self {
        TestError::Handshake(err)
    }
}

impl From<UplinkError> for TestError {
    fn from(err: UplinkError) -> Self {
        TestError::Uplink(err)
    }
}

impl From<SignerError> for TestError {
    fn from(err: SignerError) -> Self {
        TestError::Transport(TransportError::Signer(err))
    }
}

/// Maps a foreign error into [`TestError::Context`].
pub(crate) fn ctx<E: std::fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |source| TestError::Context {
        context,
        source: source.to_string(),
    }
}

/// Fails the test with `detail` unless `condition` holds.
pub(crate) fn ensure(condition: bool, detail: impl Into<String>) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(TestError::Unexpected(detail.into()))
    }
}

// --- test-only signer ---------------------------------------------------------

/// **TEST ONLY.** A [`NodeSigner`] holding an ML-DSA-65 key derived from a
/// fixed 32-byte seed, so every test run has the same node keys. It stands
/// in for the AuthHub adapter; a deterministic, in-process private key is
/// exactly what production must never have. Compiled only under
/// `#[cfg(test)]`.
pub(crate) struct DeterministicTestSigner {
    key: PqdsaKeyPair,
}

impl DeterministicTestSigner {
    /// Key from the seed `[seed; 32]`.
    pub(crate) fn from_seed(seed: u8) -> TestResult<Self> {
        let key = PqdsaKeyPair::from_seed(&ML_DSA_65_SIGNING, &[seed; 32])
            .map_err(ctx("ML-DSA-65 key from seed"))?;
        Ok(Self { key })
    }

    /// Raw ML-DSA-65 public key.
    pub(crate) fn public_key(&self) -> Vec<u8> {
        self.key.public_key().as_ref().to_vec()
    }
}

impl NodeSigner for DeterministicTestSigner {
    fn sign<'a>(&'a self, transcript: &'a [u8]) -> SignFuture<'a> {
        let mut signature = vec![0u8; ML_DSA_65_SIGNATURE_LEN];
        let result = self
            .key
            .sign(transcript, &mut signature)
            .map(|written| {
                signature.truncate(written);
                signature
            })
            .map_err(|_| SignerError::new("test signer failed"));
        Box::pin(std::future::ready(result))
    }
}

/// A test node: identity, local signer bundle.
pub(crate) struct TestNode {
    pub(crate) id: NodeId,
    pub(crate) identity: NodeIdentity,
    pub(crate) local: LocalNode,
}

/// Builds a test node named `name` with the key of `seed`.
pub(crate) fn test_node(name: &str, seed: u8) -> TestResult<TestNode> {
    let id = NodeId::try_from_str(name).map_err(ctx("node id"))?;
    let signer = DeterministicTestSigner::from_seed(seed)?;
    let identity = NodeIdentity {
        node_id: id.clone(),
        public_key: signer.public_key(),
        key_ref: format!("test-only/{name}"),
    };
    let local = LocalNode::new(identity.clone(), Arc::new(signer));
    Ok(TestNode {
        id,
        identity,
        local,
    })
}

/// A trust store pinning `nodes`.
pub(crate) fn pinned(nodes: &[&NodeIdentity]) -> TestResult<PinnedPeers> {
    let mut peers = PinnedPeers::new();
    for node in nodes {
        peers.pin_identity(node).map_err(ctx("pin"))?;
    }
    Ok(peers)
}

// --- test tower service -------------------------------------------------------

type HandlerFuture = Pin<Box<dyn Future<Output = Response<Full<Bytes>>> + Send>>;
type Handler = Arc<dyn Fn(Request<Incoming>) -> HandlerFuture + Send + Sync>;

/// A cloneable Tower service around an async closure (tower's `service_fn`
/// without the `tower` dependency).
#[derive(Clone)]
pub(crate) struct TestService {
    handler: Handler,
}

impl TestService {
    pub(crate) fn new<F, Fut>(handler: F) -> Self
    where
        F: Fn(Request<Incoming>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Response<Full<Bytes>>> + Send + 'static,
    {
        Self {
            handler: Arc::new(move |request| -> HandlerFuture { Box::pin(handler(request)) }),
        }
    }
}

impl tower_service::Service<Request<Incoming>> for TestService {
    type Response = Response<Full<Bytes>>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Infallible>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<Incoming>) -> Self::Future {
        let response = (self.handler)(request);
        Box::pin(async move { Ok(response.await) })
    }
}

/// A response with `status` and a text body.
pub(crate) fn text_response(
    status: http::StatusCode,
    text: impl Into<String>,
) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(text.into())));
    *response.status_mut() = status;
    response
}
