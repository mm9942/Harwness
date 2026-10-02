//! The server: bounded connections, a Tower stack per connection, drain.
//!
//! A listener (Unix socket, node transport, a test duplex stream) accepts a
//! stream, derives a [`ClientIdentity`] from **transport facts**, and hands
//! both to [`ComServer::serve_io`]. Everything after that is the same for
//! every ingress.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use harw_session_host::ClientIdentity;
use http_body_util::Full;
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::{Request, Response};
use hyper_util::rt::{TokioIo, TokioTimer};
use hyper_util::service::TowerToHyperService;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Semaphore, watch};
use tokio::task::JoinHandle;
use tower::util::BoxCloneService;
use tower::{Layer, Service, ServiceBuilder};

use crate::binder::ComBinder;
use crate::config::ComConfig;
use crate::error::ComError;
use crate::layer::PeerLayer;
use crate::remote::RemoteLayer;
use crate::service::UpgradeService;

/// The erased service type extra layers wrap.
pub type BoxedService = BoxCloneService<Request<Incoming>, Response<Full<Bytes>>, Infallible>;

type LayerFn = Arc<dyn Fn(BoxedService) -> BoxedService + Send + Sync>;

/// Serves session WebSocket connections over any ingress.
pub struct ComServer {
    config: ComConfig,
    binder: Arc<dyn ComBinder>,
    permits: Arc<Semaphore>,
    shutdown: watch::Receiver<bool>,
    layers: Vec<LayerFn>,
}

impl ComServer {
    /// A server that binds identities with `binder`.
    ///
    /// `shutdown` flipping to `true` closes every live WebSocket with the
    /// `Draining` reason; the host-side drain is the caller's.
    #[must_use]
    pub fn new(
        config: &ComConfig,
        binder: Arc<dyn ComBinder>,
        shutdown: watch::Receiver<bool>,
    ) -> Self {
        let config = config.normalized();
        Self {
            permits: Arc::new(Semaphore::new(config.max_connections)),
            config,
            binder,
            shutdown,
            layers: Vec::new(),
        }
    }

    /// Adds an ingress-specific Tower layer between [`PeerLayer`] and the
    /// upgrade. The first layer added is the outermost of the added ones. A
    /// layer reads the identity from the [`crate::TrustedPeer`] extension and
    /// may answer with a status of its own; the binder is only reached when
    /// every layer passes the request on.
    #[must_use]
    pub fn layer<L>(mut self, layer: L) -> Self
    where
        L: Layer<BoxedService> + Send + Sync + 'static,
        L::Service: Service<Request<Incoming>, Response = Response<Full<Bytes>>, Error = Infallible>
            + Clone
            + Send
            + 'static,
        <L::Service as Service<Request<Incoming>>>::Future: Send + 'static,
    {
        self.layers.push(Arc::new(move |inner| {
            BoxCloneService::new(layer.layer(inner))
        }));
        self
    }

    /// Connections currently held (HTTP phase and WebSocket sessions).
    #[must_use]
    pub fn live(&self) -> usize {
        self.config
            .max_connections
            .saturating_sub(self.permits.available_permits())
    }

    /// Serves one accepted stream for `identity`.
    ///
    /// Returns at once; the connection runs on its own task. The task ends
    /// when the HTTP exchange is refused, or when the upgraded WebSocket
    /// session ends.
    ///
    /// # Errors
    /// [`ComError::InvalidIdentity`] when the identity breaks a structural
    /// invariant, [`ComError::Busy`] when `max_connections` are in use. In both
    /// cases the stream is dropped without reading a byte.
    pub fn serve_io<IO>(&self, io: IO, identity: ClientIdentity) -> Result<JoinHandle<()>, ComError>
    where
        IO: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        identity.validate().map_err(|error| {
            tracing::error!(%error, "session com: listener built an invalid identity");
            ComError::InvalidIdentity
        })?;
        let permit = Arc::clone(&self.permits)
            .try_acquire_owned()
            .map_err(|_| ComError::Busy)?;
        let hold = Arc::new(permit);
        let connection = identity.connection;
        let stack = self.stack(identity, Arc::clone(&hold));

        let mut http = http1::Builder::new();
        http.timer(TokioTimer::new())
            .header_read_timeout(self.config.header_timeout)
            .max_buf_size(self.config.max_header_bytes)
            .max_headers(self.config.max_headers);
        // Do not call `keep_alive(false)`: hyper 1.11 then rewrites the 101
        // response's `Connection: Upgrade` to `close` and every strict
        // WebSocket client refuses the handshake. A refusal closes the
        // connection through an explicit `Connection: close` (see `stack`).

        Ok(tokio::spawn(async move {
            // Held with the upgrade service's copy: released when both end.
            let _hold = hold;
            let served = http
                .serve_connection(TokioIo::new(io), TowerToHyperService::new(stack))
                .with_upgrades()
                .await;
            if let Err(error) = served {
                tracing::debug!(connection = connection.0, %error, "session com: http phase ended");
            }
        }))
    }

    /// Waits up to the configured grace for every connection to finish and
    /// returns how many are still live. Call after flipping the shutdown
    /// signal passed to [`ComServer::new`].
    pub async fn drain(&self) -> usize {
        let all = u32::try_from(self.config.max_connections).unwrap_or(u32::MAX);
        let waited =
            tokio::time::timeout(self.config.shutdown_grace, self.permits.acquire_many(all)).await;
        match waited {
            Ok(Ok(_all)) => 0,
            _ => {
                let live = self.live();
                tracing::warn!(
                    live,
                    "session com: connections still open after the grace period"
                );
                live
            }
        }
    }

    /// The core service with the added layers applied: the first added layer
    /// is the outermost of them.
    fn core(&self, hold: Option<Arc<tokio::sync::OwnedSemaphorePermit>>) -> BoxedService {
        let core = UpgradeService::new(
            Arc::clone(&self.binder),
            self.config.limits,
            self.shutdown.clone(),
            hold,
        );
        let mut inner: BoxedService = BoxCloneService::new(core);
        for layer in self.layers.iter().rev() {
            inner = layer(inner);
        }
        inner
    }

    /// The per-connection stack: `PeerLayer` -> trace -> added layers -> upgrade.
    fn stack(
        &self,
        identity: ClientIdentity,
        hold: Arc<tokio::sync::OwnedSemaphorePermit>,
    ) -> impl Service<
        Request<Incoming>,
        Response = Response<Full<Bytes>>,
        Error = Infallible,
        Future: Send,
    > + Clone
    + Send
    + 'static {
        let connection = identity.connection.0;
        ServiceBuilder::new()
            .layer(PeerLayer::new(identity))
            .map_response(move |response| finish(Some(connection), response))
            .service(self.core(Some(hold)))
    }

    /// A service for an ingress that owns its accept loop and its connection
    /// limit, such as the node transport's `NodeTransportServer::serve`.
    ///
    /// `layer` resolves the transport's peer extension into the identity
    /// (see [`crate::remote`]); the rest of the stack is the same as for
    /// [`ComServer::serve_io`]. `max_connections` of this server does not
    /// apply, because the ingress counts its own connections.
    #[must_use]
    pub fn service_for<P>(
        &self,
        layer: RemoteLayer<P>,
    ) -> impl Service<
        Request<Incoming>,
        Response = Response<Full<Bytes>>,
        Error = Infallible,
        Future: Send,
    > + Clone
    + Send
    + 'static
    where
        P: Clone + Send + Sync + 'static,
    {
        let traced = ServiceBuilder::new()
            .map_response(|response| finish(None, response))
            .service(self.core(None));
        layer.layer(traced)
    }
}

/// Logs the handshake outcome; a refusal also closes the connection (one
/// purpose per connection). Never use `keep_alive(false)` for this: hyper
/// 1.11 would also rewrite the 101's `Connection: Upgrade`.
fn finish(connection: Option<u64>, mut response: Response<Full<Bytes>>) -> Response<Full<Bytes>> {
    let status = response.status();
    if status == hyper::StatusCode::SWITCHING_PROTOCOLS {
        tracing::debug!(?connection, "session com: upgrade accepted");
    } else {
        tracing::warn!(?connection, %status, "session com: handshake refused");
        response.headers_mut().insert(
            hyper::header::CONNECTION,
            hyper::header::HeaderValue::from_static("close"),
        );
    }
    response
}

/// How long a test or caller should allow [`ComServer::drain`] by default.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(5);
