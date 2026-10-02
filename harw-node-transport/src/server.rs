//! Accepting side: TLS + node handshake, then a Hyper/Tower service with the
//! authenticated peer as a request extension.

use std::error::Error as StdError;
use std::future::Future;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use aws_lc_rs::rand::SecureRandom;
use http::{Request, Response};
use hyper::body::Incoming;
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio_rustls::TlsAcceptor;

use crate::error::TransportError;
use crate::handshake::{
    AuthenticatedPeer, ClientFinish, ClientHello, HandshakePolicy, NONCE_LEN, ReplayCache,
    ServerHandshake, encode_server_accept, now_unix_ms,
};
use crate::identity::{LocalNode, NodeVerifier};
use crate::tls;
use crate::wire::{read_frame, write_frame};

/// The TLS stream of an accepted, authenticated connection.
pub type ServerTlsStream = tokio_rustls::server::TlsStream<TcpStream>;

/// Tunables of the accepting side.
#[derive(Debug, Clone)]
pub struct ServerOptions {
    /// Deadline for TLS + node handshake of one connection.
    pub handshake_timeout: Duration,
    /// Freshness policy.
    pub policy: HandshakePolicy,
    /// Replay cache capacity.
    pub replay_capacity: usize,
    /// Concurrent connections; further connections are closed at once.
    pub max_connections: usize,
    /// HTTP/1 header read deadline (slow-loris bound).
    pub header_read_timeout: Duration,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            handshake_timeout: Duration::from_secs(10),
            policy: HandshakePolicy::default(),
            replay_capacity: ReplayCache::DEFAULT_CAPACITY,
            max_connections: 256,
            header_read_timeout: Duration::from_secs(30),
        }
    }
}

struct ServerInner {
    local: LocalNode,
    verifier: Arc<dyn NodeVerifier>,
    options: ServerOptions,
    acceptor: TlsAcceptor,
    replay: ReplayCache,
    slots: Arc<Semaphore>,
}

/// A node-transport server. Cheap to clone; clones share the replay cache
/// and the connection limit.
#[derive(Clone)]
pub struct NodeTransportServer {
    inner: Arc<ServerInner>,
}

impl std::fmt::Debug for NodeTransportServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeTransportServer")
            .field("local", &self.inner.local)
            .field("options", &self.inner.options)
            .finish_non_exhaustive()
    }
}

impl NodeTransportServer {
    /// Builds a server for `local`, trusting the peers `verifier` knows.
    ///
    /// # Errors
    /// TLS configuration or ephemeral key generation failures.
    pub fn new(
        local: LocalNode,
        verifier: Arc<dyn NodeVerifier>,
        options: ServerOptions,
    ) -> Result<Self, TransportError> {
        let acceptor = TlsAcceptor::from(tls::server_config()?);
        let replay = ReplayCache::new(&options.policy, options.replay_capacity);
        let slots = Arc::new(Semaphore::new(options.max_connections));
        Ok(Self {
            inner: Arc::new(ServerInner {
                local,
                verifier,
                options,
                acceptor,
                replay,
                slots,
            }),
        })
    }

    /// Serves `service` on `listener` until an accept error that is not
    /// per-connection. Every request handed to `service` carries an
    /// [`AuthenticatedPeer`] extension.
    ///
    /// # Errors
    /// Listener failures.
    pub async fn serve<S, B>(&self, listener: TcpListener, service: S) -> Result<(), TransportError>
    where
        S: tower_service::Service<Request<Incoming>, Response = Response<B>>
            + Clone
            + Send
            + 'static,
        S::Future: Send + 'static,
        S::Error: Into<Box<dyn StdError + Send + Sync>>,
        B: http_body::Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<Box<dyn StdError + Send + Sync>>,
    {
        self.serve_with_shutdown(listener, service, std::future::pending())
            .await
    }

    /// Like [`Self::serve`], returning `Ok(())` once `shutdown` completes.
    /// Connections already running are left to finish on their own tasks.
    ///
    /// # Errors
    /// Listener failures.
    pub async fn serve_with_shutdown<S, B, F>(
        &self,
        listener: TcpListener,
        service: S,
        shutdown: F,
    ) -> Result<(), TransportError>
    where
        S: tower_service::Service<Request<Incoming>, Response = Response<B>>
            + Clone
            + Send
            + 'static,
        S::Future: Send + 'static,
        S::Error: Into<Box<dyn StdError + Send + Sync>>,
        B: http_body::Body + Send + 'static,
        B::Data: Send,
        B::Error: Into<Box<dyn StdError + Send + Sync>>,
        F: Future<Output = ()>,
    {
        let mut shutdown = std::pin::pin!(shutdown);
        loop {
            let accepted = tokio::select! {
                () = shutdown.as_mut() => return Ok(()),
                accepted = listener.accept() => accepted,
            };
            let (tcp, remote) = match accepted {
                Ok(pair) => pair,
                Err(err) if is_per_connection(&err) => continue,
                Err(err) => {
                    // Resource exhaustion (EMFILE, ENOBUFS, …): back off
                    // instead of spinning, then keep serving.
                    tracing::warn!(error = %err, "node transport accept failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            };
            let Ok(permit) = Arc::clone(&self.inner.slots).try_acquire_owned() else {
                tracing::warn!(%remote, "node transport connection limit reached");
                drop(tcp);
                continue;
            };
            let inner = Arc::clone(&self.inner);
            let service = service.clone();
            tokio::spawn(async move {
                let _permit = permit;
                if let Err(err) = handle_connection(inner, tcp, service).await {
                    // `remote` is logged for operators only; it is never
                    // used as identity.
                    tracing::debug!(%remote, error = %err, "node transport connection ended");
                }
            });
        }
    }

    /// Runs TLS + node handshake on an accepted socket and returns the
    /// authenticated peer and the stream (exposed for tests and for callers
    /// that drive their own protocol instead of HTTP).
    ///
    /// # Errors
    /// Handshake failures or timeout.
    pub async fn accept(
        &self,
        tcp: TcpStream,
    ) -> Result<(AuthenticatedPeer, ServerTlsStream), TransportError> {
        accept_node(&self.inner, tcp).await
    }
}

fn is_per_connection(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::Interrupted
    )
}

async fn accept_node(
    inner: &ServerInner,
    tcp: TcpStream,
) -> Result<(AuthenticatedPeer, ServerTlsStream), TransportError> {
    tokio::time::timeout(inner.options.handshake_timeout, accept_untimed(inner, tcp))
        .await
        .map_err(|_| TransportError::HandshakeTimeout)?
}

async fn accept_untimed(
    inner: &ServerInner,
    tcp: TcpStream,
) -> Result<(AuthenticatedPeer, ServerTlsStream), TransportError> {
    let _ = tcp.set_nodelay(true);
    let mut stream = inner.acceptor.accept(tcp).await?;
    let binding = tls::server_channel_binding(stream.get_ref().1)?;
    match server_handshake(inner, &mut stream, binding).await {
        Ok(peer) => Ok((peer, stream)),
        Err(err) => {
            // Close with close_notify so the client sees a clean rejection;
            // the reason stays server-side.
            let _ = stream.shutdown().await;
            Err(err)
        }
    }
}

async fn server_handshake<S>(
    inner: &ServerInner,
    stream: &mut S,
    binding: [u8; crate::handshake::CHANNEL_BINDING_LEN],
) -> Result<AuthenticatedPeer, TransportError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let hello = ClientHello::decode(&read_frame(stream).await?)?;
    let server_nonce = random_nonce()?;
    let pending = ServerHandshake::accept_hello(
        inner.local.node_id(),
        &hello,
        server_nonce,
        now_unix_ms(),
        binding,
        &inner.options.policy,
        &inner.replay,
        inner.verifier.as_ref(),
    )?;
    let signature = inner.local.signer.sign(&pending.signing_payload()).await?;
    write_frame(stream, &pending.server_hello(signature).encode()).await?;
    let finish = ClientFinish::decode(&read_frame(stream).await?)?;
    let peer = pending.finish(
        &finish,
        inner.verifier.as_ref(),
        &inner.replay,
        now_unix_ms(),
    )?;
    write_frame(stream, &encode_server_accept()).await?;
    Ok(peer)
}

async fn handle_connection<S, B>(
    inner: Arc<ServerInner>,
    tcp: TcpStream,
    service: S,
) -> Result<(), TransportError>
where
    S: tower_service::Service<Request<Incoming>, Response = Response<B>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Box<dyn StdError + Send + Sync>>,
    B: http_body::Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn StdError + Send + Sync>>,
{
    let (peer, stream) = accept_node(&inner, tcp).await?;
    tracing::debug!(peer = %peer.node_id, "node transport peer authenticated");
    let service = TowerToHyperService::new(InjectPeer {
        inner: service,
        peer,
    });
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(inner.options.header_read_timeout);
    // `with_upgrades` lets a handler take over the authenticated stream (a
    // WebSocket session, PL-68 W02). The `AuthenticatedPeer` extension is
    // already on the upgrade request; nothing else about the connection
    // changes, and the upgraded stream still ends with this connection.
    builder
        .serve_connection(TokioIo::new(stream), service)
        .with_upgrades()
        .await?;
    Ok(())
}

/// Generates a fresh handshake nonce from the aws-lc-rs system RNG.
pub(crate) fn random_nonce() -> Result<[u8; NONCE_LEN], TransportError> {
    let mut nonce = [0u8; NONCE_LEN];
    aws_lc_rs::rand::SystemRandom::new()
        .fill(&mut nonce)
        .map_err(|_| TransportError::Crypto("system randomness unavailable"))?;
    Ok(nonce)
}

/// Tower middleware inserting the connection's [`AuthenticatedPeer`] into
/// every request. It overwrites any value already present, so nothing a
/// client sends can influence it.
#[derive(Debug, Clone)]
struct InjectPeer<S> {
    inner: S,
    peer: AuthenticatedPeer,
}

impl<S, B> tower_service::Service<Request<B>> for InjectPeer<S>
where
    S: tower_service::Service<Request<B>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut request: Request<B>) -> Self::Future {
        request.extensions_mut().insert(self.peer.clone());
        self.inner.call(request)
    }
}
