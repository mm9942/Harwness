//! Tower layers of the communication layer.
//!
//! The composition is `PeerLayer` (outermost) -> any layers added with
//! [`crate::ComServer::layer`] -> the core [`crate::UpgradeService`]. A layer
//! sees the trusted identity as the [`TrustedPeer`] request extension, so an
//! ingress can add policy (refuse agent callers, require a device, ...)
//! without touching the upgrade or the host.

use std::task::{Context, Poll};

use harw_session_host::ClientIdentity;
use hyper::Request;
use tower::{Layer, Service};

/// The identity a **listener** built from transport facts (kernel peer
/// credentials, the node transport's authenticated handshake). It travels as
/// a request extension: a client cannot send an extension, and [`PeerLayer`]
/// overwrites any value that an earlier layer might have set.
#[derive(Clone, Debug)]
pub struct TrustedPeer(pub ClientIdentity);

/// Attaches one connection's [`TrustedPeer`] to every request.
#[derive(Clone, Debug)]
pub struct PeerLayer {
    peer: TrustedPeer,
}

impl PeerLayer {
    /// A layer bound to `identity`.
    #[must_use]
    pub fn new(identity: ClientIdentity) -> Self {
        Self {
            peer: TrustedPeer(identity),
        }
    }
}

impl<S> Layer<S> for PeerLayer {
    type Service = PeerService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        PeerService {
            inner,
            peer: self.peer.clone(),
        }
    }
}

/// Service produced by [`PeerLayer`].
#[derive(Clone, Debug)]
pub struct PeerService<S> {
    inner: S,
    peer: TrustedPeer,
}

impl<S, B> Service<Request<B>> for PeerService<S>
where
    S: Service<Request<B>>,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut request: Request<B>) -> Self::Future {
        // `insert` replaces an existing value of the same type.
        request.extensions_mut().insert(self.peer.clone());
        self.inner.call(request)
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::future::{Ready, ready};
    use std::sync::{Arc, Mutex};

    use hyper::Response;
    use tower::ServiceExt;

    use super::*;
    use crate::test_support::identity;

    /// Inner service that records the label of the peer it sees.
    #[derive(Clone)]
    struct Probe(Arc<Mutex<Vec<String>>>);

    impl Service<Request<()>> for Probe {
        type Response = Response<()>;
        type Error = Infallible;
        type Future = Ready<Result<Response<()>, Infallible>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, request: Request<()>) -> Self::Future {
            let label = request
                .extensions()
                .get::<TrustedPeer>()
                .map_or_else(|| "none".to_owned(), |p| p.0.label.clone());
            if let Ok(mut seen) = self.0.lock() {
                seen.push(label);
            }
            ready(Ok(Response::new(())))
        }
    }

    #[tokio::test]
    async fn the_layer_attaches_the_listener_identity() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let service = PeerLayer::new(identity("listener")).layer(Probe(Arc::clone(&seen)));
        let response = service.oneshot(Request::new(())).await;
        assert!(response.is_ok());
        assert_eq!(
            seen.lock().map(|s| s.clone()).unwrap_or_default(),
            ["listener"]
        );
    }

    #[tokio::test]
    async fn a_forged_extension_is_overwritten() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let service = PeerLayer::new(identity("listener")).layer(Probe(Arc::clone(&seen)));
        let mut request = Request::new(());
        request
            .extensions_mut()
            .insert(TrustedPeer(identity("forged")));
        let response = service.oneshot(request).await;
        assert!(response.is_ok());
        assert_eq!(
            seen.lock().map(|s| s.clone()).unwrap_or_default(),
            ["listener"]
        );
    }
}
