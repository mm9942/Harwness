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

use harw_session_ws::upgrade::rejection_response;
use harw_session_ws::{WsLimits, serve_connection_with};
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::{Request, Response, StatusCode};
use tokio::sync::{OwnedSemaphorePermit, watch};
use tower::Service;

use crate::binder::ComBinder;
use crate::handshake;
use crate::layer::TrustedPeer;
use crate::refusal::plain;
use crate::registry::LiveConnections;

/// Upgrades an HTTP/1 request to `harw.session.v1` for the trusted peer.
#[derive(Clone)]
pub struct UpgradeService {
    binder: Arc<dyn ComBinder>,
    limits: WsLimits,
    shutdown: watch::Receiver<bool>,
    hold: Option<Arc<OwnedSemaphorePermit>>,
    live: LiveConnections,
}

impl UpgradeService {
    pub(crate) fn new(
        binder: Arc<dyn ComBinder>,
        limits: WsLimits,
        shutdown: watch::Receiver<bool>,
        hold: Option<Arc<OwnedSemaphorePermit>>,
        live: LiveConnections,
    ) -> Self {
        Self {
            binder,
            limits,
            shutdown,
            hold,
            live,
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
        // Cheap policy checks first, so a bad request never binds an identity.
        if let Err(rejection) = harw_session_ws::upgrade::validate_upgrade(&request) {
            return rejection_response(&rejection);
        }
        // Register before binding: a revocation then either finds the entry
        // or the host refuses the revoked device at `connect`.
        let (live_guard, closer, closed) = self.live.register(
            identity.connection,
            identity.device.clone(),
            identity.label.clone(),
        );
        let ports = match self.binder.bind(identity) {
            Ok(ports) => ports,
            Err(refusal) => return refusal.response(),
        };
        // The connection closes on its own signal or on the global shutdown.
        let mut global = self.shutdown.clone();
        if *global.borrow() {
            let _ = closer.send(true);
        }
        let merged = Arc::clone(&closer);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    changed = global.changed() => {
                        if changed.is_err() || *global.borrow() {
                            let _ = merged.send(true);
                            break;
                        }
                    }
                    () = merged.closed() => break,
                }
            }
        });
        let limits = self.limits;
        let hold = self.hold.clone();
        match handshake::accept(request, &limits, move |ws| async move {
            // The permit and the table entry live as long as the session.
            let _hold = hold;
            let _live = live_guard;
            let end = serve_connection_with(ws, ports, limits, closed).await;
            tracing::debug!(?end, "session com: websocket connection ended");
        }) {
            Ok(response) => response,
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
