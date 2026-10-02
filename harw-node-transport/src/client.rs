//! Dialling side: TLS + node handshake, then an HTTP/1 connection.

use std::error::Error as StdError;
use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use http::{Request, Response};
use http_body_util::BodyExt;
use http_body_util::combinators::BoxBody;
use hyper::body::Incoming;
use hyper::client::conn::http1::SendRequest;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::task::JoinHandle;
use tokio_rustls::TlsConnector;

use crate::error::TransportError;
use crate::handshake::{
    AuthenticatedPeer, CHANNEL_BINDING_LEN, ClientFinish, ClientHandshake, HandshakePolicy,
    ServerHello, decode_server_accept, now_unix_ms,
};
use crate::identity::{LocalNode, NodeVerifier};
use crate::server::random_nonce;
use crate::tls;
use crate::wire::{read_frame, write_frame};

/// Boxed error of request bodies.
pub type BoxError = Box<dyn StdError + Send + Sync>;

/// Request body type of [`NodeTransportClient`].
pub type NodeBody = BoxBody<Bytes, BoxError>;

/// A complete in-memory request body.
#[must_use]
pub fn full_body(bytes: impl Into<Bytes>) -> NodeBody {
    http_body_util::Full::new(bytes.into())
        .map_err(|never| match never {})
        .boxed()
}

/// An empty request body.
#[must_use]
pub fn empty_body() -> NodeBody {
    http_body_util::Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed()
}

harw_types::limits_struct! {
    /// Tunables of the dialling side.
    #[derive(Debug, Clone)]
    pub struct ClientOptions {
        /// Deadline for TCP connect + TLS + node handshake.
        pub handshake_timeout: Duration = DEFAULT_CLIENT_HANDSHAKE_TIMEOUT = Duration::from_secs(10),
        /// Freshness policy.
        pub policy: HandshakePolicy = DEFAULT_CLIENT_POLICY = HandshakePolicy::DEFAULT,
    }
}

/// An authenticated HTTP/1 connection to one remote node.
#[derive(Debug)]
pub struct NodeTransportClient {
    sender: SendRequest<NodeBody>,
    peer: AuthenticatedPeer,
    connection: JoinHandle<()>,
}

impl NodeTransportClient {
    /// Dials `addr`, expecting the node `expected_peer` there, and
    /// authenticates both sides. The address is only where to dial; the
    /// peer is accepted only if it proves `expected_peer` with the key
    /// `verifier` pins for it.
    ///
    /// # Errors
    /// Connect, TLS, handshake or timeout failures.
    pub async fn connect(
        addr: SocketAddr,
        local: &LocalNode,
        expected_peer: &harw_types::NodeId,
        verifier: &dyn NodeVerifier,
    ) -> Result<Self, TransportError> {
        Self::connect_with(
            addr,
            local,
            expected_peer,
            verifier,
            &ClientOptions::default(),
        )
        .await
    }

    /// [`Self::connect`] with explicit options.
    ///
    /// # Errors
    /// Connect, TLS, handshake or timeout failures.
    pub async fn connect_with(
        addr: SocketAddr,
        local: &LocalNode,
        expected_peer: &harw_types::NodeId,
        verifier: &dyn NodeVerifier,
        options: &ClientOptions,
    ) -> Result<Self, TransportError> {
        let work = async {
            let tcp = TcpStream::connect(addr).await?;
            let _ = tcp.set_nodelay(true);
            let connector = TlsConnector::from(tls::client_config()?);
            let mut stream = connector.connect(tls::server_name()?, tcp).await?;
            let binding = tls::client_channel_binding(stream.get_ref().1)?;
            let peer = client_handshake(
                &mut stream,
                binding,
                local,
                expected_peer,
                verifier,
                &options.policy,
            )
            .await?;
            Ok::<_, TransportError>((peer, stream))
        };
        let (peer, stream) = tokio::time::timeout(options.handshake_timeout, work)
            .await
            .map_err(|_| TransportError::HandshakeTimeout)??;

        let (sender, connection) =
            hyper::client::conn::http1::handshake::<_, NodeBody>(TokioIo::new(stream)).await?;
        let connection = tokio::spawn(async move {
            // `with_upgrades` is inert unless a request asks for an upgrade
            // ([`NodeTransportClient::upgrade`]); plain requests behave as before.
            if let Err(err) = connection.with_upgrades().await {
                tracing::debug!(error = %err, "node transport client connection ended");
            }
        });
        Ok(Self {
            sender,
            peer,
            connection,
        })
    }

    /// The authenticated remote node.
    #[must_use]
    pub fn peer(&self) -> &AuthenticatedPeer {
        &self.peer
    }

    /// Sends `request` and hands back the response plus the authenticated
    /// peer without dropping (aborting) the connection task. Used by
    /// `upgrade`, which must keep the connection alive until the upgraded
    /// I/O has been taken over.
    pub(crate) async fn send_for_upgrade(
        &mut self,
        request: Request<NodeBody>,
    ) -> Result<(AuthenticatedPeer, Response<Incoming>), hyper::Error> {
        self.sender.ready().await?;
        let response = self.sender.send_request(request).await?;
        Ok((self.peer.clone(), response))
    }

    /// Sends one request on the authenticated connection.
    ///
    /// # Errors
    /// HTTP failures, including a closed connection.
    pub async fn send_request(
        &mut self,
        request: Request<NodeBody>,
    ) -> Result<Response<Incoming>, TransportError> {
        self.sender.ready().await?;
        Ok(self.sender.send_request(request).await?)
    }
}

impl Drop for NodeTransportClient {
    fn drop(&mut self) {
        self.connection.abort();
    }
}

async fn client_handshake<S>(
    stream: &mut S,
    binding: [u8; CHANNEL_BINDING_LEN],
    local: &LocalNode,
    expected_peer: &harw_types::NodeId,
    verifier: &dyn NodeVerifier,
    policy: &HandshakePolicy,
) -> Result<AuthenticatedPeer, TransportError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (handshake, hello) = ClientHandshake::start(
        local.node_id(),
        expected_peer,
        random_nonce()?,
        now_unix_ms(),
    );
    write_frame(stream, &hello.encode()).await?;
    let server_hello = ServerHello::decode(&read_frame(stream).await?)?;
    let pending =
        handshake.on_server_hello(&server_hello, binding, verifier, policy, now_unix_ms())?;
    let signature = local.signer.sign(&pending.signing_payload()).await?;
    write_frame(stream, &ClientFinish { signature }.encode()).await?;
    decode_server_accept(&read_frame(stream).await?)?;
    Ok(pending.peer())
}
