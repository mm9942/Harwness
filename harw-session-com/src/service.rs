//! The core service: validate, bind, upgrade.
//!
//! Order matters. The request is validated first (no side effects), then the
//! identity is bound through the [`ComBinder`], and only then is the 101
//! response sent. A refusal at any step is an HTTP status. The connection
//! permit is carried into the upgraded task, so the connection limit also
//! covers the whole WebSocket session.

use std::convert::Infallible;
use std::future::{Ready, ready};
use std::sync::Arc;
use std::task::{Context, Poll};

use harw_session_ws::upgrade::{rejection_response, upgrade_server, validate_upgrade};
use harw_session_ws::{WsLimits, serve_connection_with};
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::{Request, Response, StatusCode};
use tokio::sync::{OwnedSemaphorePermit, watch};
use tower::Service;

use crate::binder::ComBinder;
use crate::layer::TrustedPeer;
use crate::refusal::plain;

/// Upgrades an HTTP/1 request to `harw.session.v1` for the trusted peer.
#[derive(Clone)]
pub struct UpgradeService {
    binder: Arc<dyn ComBinder>,
    limits: WsLimits,
    shutdown: watch::Receiver<bool>,
    hold: Arc<OwnedSemaphorePermit>,
}

impl UpgradeService {
    pub(crate) fn new(
        binder: Arc<dyn ComBinder>,
        limits: WsLimits,
        shutdown: watch::Receiver<bool>,
        hold: Arc<OwnedSemaphorePermit>,
    ) -> Self {
        Self {
            binder,
            limits,
            shutdown,
            hold,
        }
    }

    fn handle<B>(&self, mut request: Request<B>) -> Response<Full<Bytes>>
    where
        B: Send + 'static,
    {
        let Some(TrustedPeer(identity)) = request.extensions_mut().remove::<TrustedPeer>() else {
            // The listener did not attach an identity: fail closed.
            tracing::error!("session com: request without a trusted peer");
            return plain(
                StatusCode::INTERNAL_SERVER_ERROR,
                "connection has no identity",
            );
        };
        if let Err(rejection) = validate_upgrade(&request) {
            return rejection_response(&rejection);
        }
        let ports = match self.binder.bind(identity) {
            Ok(ports) => ports,
            Err(refusal) => return refusal.response(),
        };
        let limits = self.limits;
        let shutdown = self.shutdown.clone();
        let hold = Arc::clone(&self.hold);
        match upgrade_server(request, limits.tungstenite_config(), move |ws| async move {
            // The permit lives as long as the WebSocket session.
            let _hold = hold;
            let end = serve_connection_with(ws, ports, limits, shutdown).await;
            tracing::debug!(?end, "session com: websocket connection ended");
        }) {
            Ok(response) => response.map(|_| Full::new(Bytes::new())),
            Err(rejection) => rejection_response(&rejection),
        }
    }
}

impl<B> Service<Request<B>> for UpgradeService
where
    B: Send + 'static,
{
    type Response = Response<Full<Bytes>>;
    type Error = Infallible;
    type Future = Ready<Result<Self::Response, Infallible>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: Request<B>) -> Self::Future {
        ready(Ok(self.handle(request)))
    }
}
