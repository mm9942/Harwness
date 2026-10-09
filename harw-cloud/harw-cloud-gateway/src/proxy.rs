//! Pingora-based reverse proxy for the cloud gateway.
//!
//! This module is gated behind the `proxy` feature. When enabled, the
//! gateway will use `pingora` to terminate TLS on the configured listener
//! address, route requests to the mapped upstream ports by service name,
//! and apply per-tenant enrollment checks (see `harw-cloud-enroll`) before
//! forwarding. For now this is a placeholder; the actual proxy service
//! implementation lands with the cloud restoration work (PL-90).

#![allow(unused_imports)]

use pingora::prelude::*;

/// Placeholder proxy service type; the real routing logic arrives later.
#[derive(Debug, Default)]
pub struct GatewayProxyService;

#[async_trait::async_trait]
impl ProxyHttp for GatewayProxyService {
    type CTX = ();
    fn new_ctx(&self) -> Self::CTX {}
}
