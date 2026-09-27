//! `harw-node-transport` — layer A remote node transport (masterplan v2 §19,
//! §28, §29, H11; WP-15 DoD uplink).
//!
//! # Layers
//! 1. **TLS 1.3** ([`tls`]) — rustls 0.23 with the aws-lc-rs provider the
//!    workspace already locks, key exchange restricted to the hybrid
//!    `X25519MLKEM768`. Confidentiality, integrity, channel binding. No
//!    identity: the server key is an ephemeral raw public key.
//! 2. **Node handshake** ([`handshake`]) — both nodes sign a versioned,
//!    domain-separated transcript (both node ids, nonces, timestamps,
//!    protocol version, TLS exporter) with their long-term ML-DSA-65 key via
//!    [`NodeSigner`]; the peer checks it against [`PinnedPeers`]. Replay
//!    protection: nonce cache + time window + exporter binding. Keys held
//!    in the AuthHub sign through [`AuthHubNodeSigner`], which wraps the
//!    transcript in a `NodeHandshake` sign transcript; their peers verify
//!    with [`TranscriptWrappedVerifier`] ([`authhub_signer`]).
//! 3. **Service exposure** — [`NodeTransportServer::serve`] runs a Tower
//!    service over Hyper HTTP/1 with the [`AuthenticatedPeer`] as request
//!    extension; [`NodeTransportClient::connect`] dials one node.
//! 4. **DoD uplink** ([`uplink`]) — read-only JSON-lines event stream.
//!
//! The remote node's IP address is never identity (H11 exit criterion).
//! See `README.md` for the security model.

pub mod authhub_signer;
pub mod client;
pub mod error;
pub mod handshake;
pub mod identity;
pub mod server;
pub mod tls;
pub mod uplink;
mod wire;

#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

pub use authhub_signer::{
    AuthHubNodeSigner, AuthHubSign, TranscriptWrappedSigner, TranscriptWrappedVerifier,
    WRAPPED_TRANSCRIPT_FIELD, WrapError, wrap_transcript,
};
pub use client::{BoxError, ClientOptions, NodeBody, NodeTransportClient, empty_body, full_body};
pub use error::{HandshakeError, PinError, SignerError, TransportError, UplinkError, VerifyError};
pub use handshake::{
    AuthenticatedPeer, HandshakePolicy, PROTOCOL_VERSION, ReplayCache, TRANSCRIPT_DOMAIN,
};
pub use identity::{
    LocalNode, ML_DSA_65_PUBLIC_KEY_LEN, ML_DSA_65_SIGNATURE_LEN, NodeIdentity, NodeSigner,
    NodeVerifier, PinnedPeers, SignFuture,
};
pub use server::{NodeTransportServer, ServerOptions, ServerTlsStream};
pub use tls::TLS_EXPORTER_LABEL;
pub use uplink::{
    DodUplink, FindingSeverity, HealthStatus, UPLINK_CONTENT_TYPE, UPLINK_PATH, UplinkBody,
    UplinkEvent, UplinkLimits, UplinkSender, UplinkStats, encode_line, receive_uplink,
    uplink_channel, uplink_request,
};
