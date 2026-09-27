//! Error types of the node transport.
//!
//! The split mirrors the layers: [`HandshakeError`] is the sans-IO
//! authentication verdict (pure, comparable, testable without sockets),
//! [`TransportError`] adds I/O, TLS and HTTP failures around it, and
//! [`UplinkError`] covers the DoD uplink encoder/receiver.

use harw_types::NodeId;

/// A node signer (normally the AuthHub-backed adapter) could not sign.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("node signer failed: {reason}")]
pub struct SignerError {
    reason: String,
}

impl SignerError {
    /// Builds a signer error with a human-readable reason. The reason must
    /// not contain key material.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    /// The reason given by the signer.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// Why a [`crate::NodeVerifier`] refused a peer signature.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerifyError {
    /// The node id has no pinned public key.
    #[error("node `{0}` is not pinned")]
    UnknownPeer(NodeId),
    /// The signature does not verify under the pinned key.
    #[error("signature of node `{0}` does not verify under its pinned key")]
    BadSignature(NodeId),
}

/// Rejecting a pin: the public key is not an ML-DSA-65 public key.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("public key for node `{node}` has {actual} bytes, ML-DSA-65 needs {expected}")]
pub struct PinError {
    /// Node whose key was rejected.
    pub node: NodeId,
    /// Required length ([`crate::ML_DSA_65_PUBLIC_KEY_LEN`]).
    pub expected: usize,
    /// Length that was offered.
    pub actual: usize,
}

/// The authentication verdict of the node handshake (sans-IO).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum HandshakeError {
    /// The peer speaks a protocol version this build does not.
    #[error("unsupported node-transport protocol version {0}")]
    UnsupportedVersion(u16),
    /// The client addressed a different node than this server.
    #[error("client hello is addressed to node `{addressed}`, this node is `{local}`")]
    WrongRecipient {
        /// Node the client asked for.
        addressed: NodeId,
        /// This node.
        local: NodeId,
    },
    /// The server presented a different node id than the client expected.
    #[error("expected peer node `{expected}`, peer presented `{actual}`")]
    PeerMismatch {
        /// Node the client dialled.
        expected: NodeId,
        /// Node the server claimed to be.
        actual: NodeId,
    },
    /// The peer's signature was refused by the verifier.
    #[error(transparent)]
    Verify(#[from] VerifyError),
    /// The peer's timestamp lies outside the accepted window.
    #[error("peer timestamp {peer_ms} ms is outside ±{max_skew_ms} ms of local time {local_ms} ms")]
    ClockSkew {
        /// Peer's claimed time (Unix ms).
        peer_ms: u64,
        /// Local time (Unix ms).
        local_ms: u64,
        /// Accepted skew.
        max_skew_ms: u64,
    },
    /// The (client node, nonce) pair was already used inside the window.
    #[error("replayed handshake nonce from node `{0}`")]
    Replay(NodeId),
    /// The replay cache is full; the handshake is refused (fail closed).
    #[error("replay cache is full; refusing new handshakes until entries expire")]
    ReplayCacheFull,
    /// A handshake message could not be decoded.
    #[error("malformed handshake message: {0}")]
    Malformed(&'static str),
    /// A handshake frame exceeded the size bound.
    #[error("handshake frame of {0} bytes exceeds the limit")]
    FrameTooLarge(usize),
    /// The peer closed the channel instead of accepting.
    #[error("peer rejected the handshake")]
    RejectedByPeer,
}

/// Any failure of the node transport.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TransportError {
    /// Socket or TLS record I/O failed.
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    /// The rustls configuration could not be built.
    #[error("tls configuration: {0}")]
    TlsConfig(#[from] rustls::Error),
    /// Generating the ephemeral TLS key or a nonce failed.
    #[error("crypto backend: {0}")]
    Crypto(&'static str),
    /// Node authentication failed.
    #[error("node handshake: {0}")]
    Handshake(#[from] HandshakeError),
    /// The TLS + node handshake did not finish in time.
    #[error("node handshake timed out")]
    HandshakeTimeout,
    /// The local node signer failed.
    #[error(transparent)]
    Signer(#[from] SignerError),
    /// HTTP/1 connection error.
    #[error("http: {0}")]
    Http(#[from] hyper::Error),
    /// Building an HTTP request failed.
    #[error("http request: {0}")]
    Request(#[from] http::Error),
}

/// Failures of the DoD uplink encoder and receiver.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum UplinkError {
    /// One encoded event line (including its `\n`) exceeds the line limit.
    #[error("uplink line of {actual} bytes exceeds the limit of {limit}")]
    LineTooLong {
        /// Bytes seen (for a partial line: bytes buffered so far).
        actual: usize,
        /// `UplinkLimits::max_line_bytes`.
        limit: usize,
    },
    /// The stream carried more events than allowed.
    #[error("uplink stream exceeds {limit} events")]
    TooManyEvents {
        /// `UplinkLimits::max_events`.
        limit: u64,
    },
    /// The stream carried more bytes than allowed.
    #[error("uplink stream exceeds {limit} bytes")]
    StreamTooLarge {
        /// `UplinkLimits::max_stream_bytes`.
        limit: u64,
    },
    /// A line is not a valid [`crate::UplinkEvent`].
    #[error("malformed uplink event on line {line}: {reason}")]
    Malformed {
        /// 1-based line number.
        line: u64,
        /// Decoder message.
        reason: String,
    },
    /// The stream ended in the middle of a line.
    #[error("uplink stream ended inside a line")]
    TruncatedLine,
    /// The body failed while being read.
    #[error("uplink body: {0}")]
    Body(String),
    /// The sender queue is full; the event was dropped (sensors never block).
    #[error("uplink queue is full; event dropped")]
    Backpressure,
    /// The receiving end of the sender queue is gone.
    #[error("uplink stream is closed")]
    Closed,
}

impl UplinkError {
    /// HTTP status a receiver should answer with for this error.
    #[must_use]
    pub fn http_status(&self) -> http::StatusCode {
        match self {
            Self::LineTooLong { .. } | Self::TooManyEvents { .. } | Self::StreamTooLarge { .. } => {
                http::StatusCode::PAYLOAD_TOO_LARGE
            }
            Self::Malformed { .. } | Self::TruncatedLine | Self::Body(_) => {
                http::StatusCode::BAD_REQUEST
            }
            Self::Backpressure | Self::Closed => http::StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}
