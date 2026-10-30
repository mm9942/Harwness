//! Pingora-based reverse proxy for the cloud gateway.
//!
//! This module is gated behind the `proxy` feature. It exposes a placeholder
//! that refuses every upstream selection with HTTP 501 (Not Implemented).
//! TLS listeners, routing and per-tenant enrollment checks remain unimplemented
//! until the cloud restoration work (PL-90).

use pingora::prelude::{Error, ErrorType, HttpPeer, ProxyHttp, Session};

/// Placeholder proxy service type; the real routing logic arrives later.
#[derive(Debug, Default)]
pub struct GatewayProxyService;

#[async_trait::async_trait]
impl ProxyHttp for GatewayProxyService {
    type CTX = ();
    fn new_ctx(&self) -> Self::CTX {}

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        _ctx: &mut Self::CTX,
    ) -> pingora::Result<Box<HttpPeer>> {
        Error::e_explain(
            ErrorType::HTTPStatus(501),
            "cloud gateway proxy is not implemented",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn placeholder_refuses_upstream_with_not_implemented() {
        let (_client, stream) = tokio::io::duplex(64);
        let mut session = Session::new_h1(Box::new(stream));
        let service = GatewayProxyService;
        let error = service
            .upstream_peer(&mut session, &mut ())
            .await
            .expect_err("the placeholder must never select an upstream");

        assert_eq!(error.etype, ErrorType::HTTPStatus(501));
    }
}
