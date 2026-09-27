//! The TLS 1.3 channel under the node handshake.
//!
//! Stack: rustls 0.23 with the aws-lc-rs provider — the one the workspace
//! already locks (see README). Key exchange is restricted to the hybrid
//! post-quantum group `X25519MLKEM768`; only TLS 1.3 is offered.
//!
//! TLS here provides confidentiality, integrity and a channel binding; it
//! does **not** authenticate nodes. The server presents an ephemeral
//! Ed25519 raw public key (RFC 7250, generated per server instance) and the
//! client accepts any such key after checking the TLS handshake signature
//! made with it. Node authentication is the ML-DSA-65 handshake in
//! [`crate::handshake`], whose transcript contains the TLS exporter value:
//! a man-in-the-middle terminating TLS on either side ends up with two
//! different exporter values, so neither node's signature verifies on the
//! other leg.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{
    CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, SubjectPublicKeyInfoDer,
    UnixTime,
};
use rustls::server::AlwaysResolvesServerRawPublicKeys;
use rustls::sign::CertifiedKey;
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, ServerConfig, ServerConnection,
    SignatureScheme,
};

use crate::error::TransportError;
use crate::handshake::CHANNEL_BINDING_LEN;

/// RFC 5705 / RFC 8446 §7.5 exporter label for the channel binding.
pub const TLS_EXPORTER_LABEL: &[u8] = b"EXPORTER-harw-node-transport-v1";

/// SNI-free placeholder name; never used for authentication.
const SERVER_NAME: &str = "harw-node.invalid";

/// The aws-lc-rs provider restricted to the hybrid PQ key exchange.
pub(crate) fn provider() -> Arc<CryptoProvider> {
    let mut provider = rustls::crypto::aws_lc_rs::default_provider();
    provider.kx_groups = vec![rustls::crypto::aws_lc_rs::kx_group::X25519MLKEM768];
    Arc::new(provider)
}

/// Server config with a fresh ephemeral Ed25519 raw public key.
pub(crate) fn server_config() -> Result<Arc<ServerConfig>, TransportError> {
    let provider = provider();
    let rng = aws_lc_rs::rand::SystemRandom::new();
    let pkcs8 = aws_lc_rs::signature::Ed25519KeyPair::generate_pkcs8(&rng)
        .map_err(|_| TransportError::Crypto("ephemeral Ed25519 key generation failed"))?;
    let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(pkcs8.as_ref().to_vec()));
    let signing_key = provider.key_provider.load_private_key(key_der)?;
    let spki = signing_key
        .public_key()
        .ok_or(TransportError::Crypto("ephemeral key has no public key"))?
        .as_ref()
        .to_vec();
    let certified = CertifiedKey::new(vec![CertificateDer::from(spki)], signing_key);
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(AlwaysResolvesServerRawPublicKeys::new(Arc::new(
            certified,
        ))));
    // HTTP/1.1 only; no session tickets or resumption: every connection
    // runs the full node handshake.
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    config.send_tls13_tickets = 0;
    Ok(Arc::new(config))
}

/// Client config accepting the server's ephemeral raw public key.
pub(crate) fn client_config() -> Result<Arc<ClientConfig>, TransportError> {
    let provider = provider();
    let verifier = ChannelOnlyVerifier {
        algorithms: provider.signature_verification_algorithms,
    };
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    config.enable_sni = false;
    config.resumption = rustls::client::Resumption::disabled();
    Ok(Arc::new(config))
}

/// The placeholder server name handed to the TLS connector.
pub(crate) fn server_name() -> Result<ServerName<'static>, TransportError> {
    ServerName::try_from(SERVER_NAME)
        .map_err(|_| TransportError::Crypto("invalid placeholder server name"))
}

/// Derives the channel binding from an established TLS session.
pub(crate) fn server_channel_binding(
    conn: &ServerConnection,
) -> Result<[u8; CHANNEL_BINDING_LEN], TransportError> {
    Ok(conn.export_keying_material([0u8; CHANNEL_BINDING_LEN], TLS_EXPORTER_LABEL, None)?)
}

/// Client-side twin of [`server_channel_binding`].
pub(crate) fn client_channel_binding(
    conn: &ClientConnection,
) -> Result<[u8; CHANNEL_BINDING_LEN], TransportError> {
    Ok(conn.export_keying_material([0u8; CHANNEL_BINDING_LEN], TLS_EXPORTER_LABEL, None)?)
}

/// Checks the TLS 1.3 handshake signature made with the server's raw public
/// key but pins nothing: node identity is established one layer up.
#[derive(Debug)]
struct ChannelOnlyVerifier {
    algorithms: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for ChannelOnlyVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        // Deliberate: the raw key is ephemeral and carries no identity. The
        // ML-DSA node handshake, bound to this session's exporter, decides.
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 is not offered".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature_with_raw_key(
            message,
            &SubjectPublicKeyInfoDer::from(cert.as_ref()),
            dss,
            &self.algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }

    fn requires_raw_public_keys(&self) -> bool {
        true
    }
}
