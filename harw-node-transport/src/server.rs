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
    Role, ServerHandshake, encode_server_accept, now_unix_ms,
};
use crate::identity::{LocalNode, NodeVerifier};
use crate::owner_process_channel::{LiveOwnerProcessChannel, ServerOwnerProcessBinding};
use crate::tls;
use crate::wire::{read_frame, write_frame};

/// The TLS stream of an accepted, authenticated connection.
pub type ServerTlsStream = tokio_rustls::server::TlsStream<TcpStream>;

harw_types::limits_struct! {
    /// Tunables of the accepting side.
    #[derive(Debug, Clone)]
    pub struct ServerOptions {
        /// Deadline for TLS + node handshake of one connection.
        pub handshake_timeout: Duration = DEFAULT_SERVER_HANDSHAKE_TIMEOUT = Duration::from_secs(10),
        /// Freshness policy.
        pub policy: HandshakePolicy = DEFAULT_SERVER_POLICY = HandshakePolicy::DEFAULT,
        /// Replay cache capacity.
        pub replay_capacity: usize = DEFAULT_REPLAY_CAPACITY = ReplayCache::DEFAULT_CAPACITY,
        /// Concurrent connections; further connections are closed at once.
        pub max_connections: usize = DEFAULT_MAX_CONNECTIONS = 256,
        /// HTTP/1 header read deadline (slow-loris bound).
        pub header_read_timeout: Duration = DEFAULT_HEADER_READ_TIMEOUT = Duration::from_secs(30),
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
        self.serve_loop(listener, service, shutdown, false, || true)
            .await
    }

    /// Shared accept loop of [`Self::serve_with_shutdown`] and
    /// `serve_upgradable`; `upgrades` enables HTTP/1 upgrades on connections.
    pub(crate) async fn serve_loop<S, B, F, G>(
        &self,
        listener: TcpListener,
        service: S,
        shutdown: F,
        upgrades: bool,
        admit: G,
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
        G: Fn() -> bool + Clone + Send + 'static,
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
            // Close refused sockets without consuming a transport slot,
            // spawning work, or reading transport bytes.
            if !admit() {
                drop(tcp);
                continue;
            }
            let Ok(permit) = Arc::clone(&self.inner.slots).try_acquire_owned() else {
                tracing::warn!(%remote, "node transport connection limit reached");
                drop(tcp);
                continue;
            };
            let _ = tcp.set_nodelay(true);
            let inner = Arc::clone(&self.inner);
            let service = service.clone();
            let timeout = inner.options.handshake_timeout;
            let admit = admit.clone();
            tokio::spawn(async move {
                let _permit = permit;
                // The task might only have been scheduled after disable.
                if !admit() {
                    return;
                }
                if let Err(err) = handle_connection(inner, tcp, service, upgrades, timeout).await {
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
        let _ = tcp.set_nodelay(true);
        let accepted = accept_node(&self.inner, tcp, self.inner.options.handshake_timeout).await?;
        Ok((accepted.peer, accepted.stream))
    }

    /// Serves one established stream without offering HTTP/1 upgrades.
    ///
    /// TLS, pinned node authentication, replay checks and connection limits
    /// are the same as TCP. Dropping this future closes the stream and
    /// invalidates any connection-bound witnesses. An exhausted connection
    /// budget drops the stream and returns `Ok(())`.
    ///
    /// # Errors
    /// Handshake, timeout or HTTP failures.
    pub async fn serve_io<I, S, B>(
        &self,
        io: I,
        service: S,
        handshake_timeout: Duration,
    ) -> Result<(), TransportError>
    where
        I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
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
        let Ok(_permit) = Arc::clone(&self.inner.slots).try_acquire_owned() else {
            tracing::warn!("node transport connection limit reached");
            return Ok(());
        };
        handle_connection(Arc::clone(&self.inner), io, service, false, handshake_timeout).await
    }

    /// Serves one already-connected byte stream (e.g. an accepted onion
    /// stream) through the identical TLS + node handshake, replay cache,
    /// connection limit and [`AuthenticatedPeer`] injection as TCP
    /// connections, with HTTP/1 upgrades enabled. The stream's origin is
    /// never identity: only the node handshake authenticates the peer.
    ///
    /// When the connection limit is reached the stream is dropped and
    /// `Ok(())` is returned. Otherwise this returns when the connection ends.
    ///
    /// # Errors
    /// Handshake, timeout or HTTP failures ([`UpgradeError::Transport`]).
    ///
    /// [`UpgradeError::Transport`]: crate::upgrade::UpgradeError::Transport
    pub async fn serve_upgradable_io<I, S, B>(
        &self,
        io: I,
        service: S,
        handshake_timeout: Duration,
    ) -> Result<(), crate::upgrade::UpgradeError>
    where
        I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
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
        let Ok(permit) = Arc::clone(&self.inner.slots).try_acquire_owned() else {
            tracing::warn!("node transport connection limit reached");
            drop(io);
            return Ok(());
        };
        let inner = Arc::clone(&self.inner);
        let result = handle_connection(inner, io, service, true, handshake_timeout).await;
        drop(permit);
        result.map_err(crate::upgrade::UpgradeError::Transport)
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

struct AcceptedNode<I> {
    peer: AuthenticatedPeer,
    peer_key_fingerprint: Option<[u8; 32]>,
    stream: tokio_rustls::server::TlsStream<I>,
}

async fn accept_node<I>(
    inner: &ServerInner,
    io: I,
    timeout: Duration,
) -> Result<AcceptedNode<I>, TransportError>
where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    tokio::time::timeout(timeout, accept_untimed(inner, io))
        .await
        .map_err(|_| TransportError::HandshakeTimeout)?
}

async fn accept_untimed<I>(
    inner: &ServerInner,
    io: I,
) -> Result<AcceptedNode<I>, TransportError>
where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut stream = inner.acceptor.accept(io).await?;
    let binding = tls::server_channel_binding(stream.get_ref().1)?;
    match server_handshake(inner, &mut stream, binding).await {
        Ok((peer, peer_key_fingerprint)) => Ok(AcceptedNode {
            peer,
            peer_key_fingerprint,
            stream,
        }),
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
) -> Result<(AuthenticatedPeer, Option<[u8; 32]>), TransportError>
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
    let verified = pending.finish_with_key_fingerprint(
        &finish,
        inner.verifier.as_ref(),
        &inner.replay,
        now_unix_ms(),
    )?;
    write_frame(stream, &encode_server_accept()).await?;
    Ok(verified)
}

async fn handle_connection<I, S, B>(
    inner: Arc<ServerInner>,
    io: I,
    service: S,
    upgrades: bool,
    handshake_timeout: Duration,
) -> Result<(), TransportError>
where
    I: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    S: tower_service::Service<Request<Incoming>, Response = Response<B>> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::Error: Into<Box<dyn StdError + Send + Sync>>,
    B: http_body::Body + Send + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn StdError + Send + Sync>>,
{
    let AcceptedNode { peer, peer_key_fingerprint, stream } =
        accept_node(&inner, io, handshake_timeout).await?;
    tracing::debug!(peer = %peer.node_id, "node transport peer authenticated");
    let exporter = tls::server_owner_process_exporter(stream.get_ref().1)?;
    let (_live_channel, owner_process_channel) =
        LiveOwnerProcessChannel::new(peer.clone(), exporter, Role::Server, peer_key_fingerprint);
    let service = TowerToHyperService::new(InjectPeer {
        inner: service,
        peer,
        owner_process_channel: ServerOwnerProcessBinding(owner_process_channel),
    });
    let mut builder = hyper::server::conn::http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(inner.options.header_read_timeout);
    let connection = builder.serve_connection(TokioIo::new(stream), service);
    if upgrades {
        connection.with_upgrades().await?;
    } else {
        connection.await?;
    }
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
/// every request, alongside the private connection-bound proof witness.
/// Both overwrite any existing value; nothing a client sends influences them.
#[derive(Clone)]
struct InjectPeer<S> {
    inner: S,
    peer: AuthenticatedPeer,
    owner_process_channel: ServerOwnerProcessBinding,
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
        let extensions = request.extensions_mut();
        extensions.insert(self.peer.clone());
        extensions.insert(self.owner_process_channel.clone());
        self.inner.call(request)
    }
}
