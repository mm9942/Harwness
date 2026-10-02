//! Self-cloud ingress: from a transport-authenticated peer to a
//! [`ClientIdentity`] (PL-68 §2.2, §6.2, W06).
//!
//! The node transport authenticates the *node* (PQ-TLS plus an ML-DSA
//! transcript signature) and inserts its `AuthenticatedPeer` into every
//! request. That proves a node id and nothing else. This module maps the node
//! id to **one enrolled device** with a tenant, a tier and a label, and builds
//! the identity the host sees:
//!
//! - the tenant is mandatory (a device record cannot exist without one);
//! - one node id is one device, and one device is one node id;
//! - an address plays no part: nothing here sees an IP;
//! - the caps are the tier's *session* ceiling only: no `gateway_*`, never
//!   `tool_call`, and **no `approve`** unless the record opts in
//!   (`may_approve`), because high-risk approval stays local by default
//!   (PL-68 §13, decision 4);
//! - an unknown node is refused with 403 before anything is bound.
//!
//! Revocation stays with the host: `SessionHost::revoke_device` refuses the
//! device on its next `connect` and closes its live streams.
//!
//! The layer is generic over the peer type so this crate does not depend on
//! the node transport (and its TLS stack): the composition root names
//! `AuthenticatedPeer` and how to read its node id.

use std::collections::HashMap;
use std::convert::Infallible;
use std::future::{Future, ready};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use harw_session_host::{ClientIdentity, ConnectionId, caps_for_tier};
use harw_types::{
    ApprovalActor, AuthStrength, DeviceId, IngressSurface, NodeId, PermissionTier, Principal,
    PrincipalKind, TenantId, TrustZone,
};
use http_body_util::Full;
use hyper::body::Bytes;
use hyper::{Request, Response};
use tower::{Layer, Service};

use crate::layer::TrustedPeer;
use crate::refusal::{ComRefusal, plain};

/// An enrolled device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRecord {
    /// The device id (also the principal id `device:<id>`).
    pub device: DeviceId,
    /// Tenant scope. Every remote caller is tenant-scoped.
    pub tenant: TenantId,
    /// Permission tier; narrows the session caps.
    pub tier: PermissionTier,
    /// Presence label.
    pub label: String,
    /// Whether this device may answer approvals. Default policy: no.
    pub may_approve: bool,
}

/// Why an enrollment was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollError {
    /// The node id already belongs to a device.
    NodeAlreadyEnrolled,
    /// The device id is already bound to another node id.
    DeviceAlreadyEnrolled,
}

impl std::fmt::Display for EnrollError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NodeAlreadyEnrolled => f.write_str("node id is already enrolled"),
            Self::DeviceAlreadyEnrolled => f.write_str("device id is already enrolled"),
        }
    }
}

impl std::error::Error for EnrollError {}

/// Pinned devices: node id -> device record. Built from trusted config.
#[derive(Debug, Clone, Default)]
pub struct DeviceRegistry {
    by_node: HashMap<NodeId, DeviceRecord>,
}

impl DeviceRegistry {
    /// An empty registry: every node is unknown.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Enrolls `node` as `record`.
    ///
    /// # Errors
    /// The node or the device is already enrolled.
    pub fn enroll(&mut self, node: NodeId, record: DeviceRecord) -> Result<(), EnrollError> {
        if self.by_node.contains_key(&node) {
            return Err(EnrollError::NodeAlreadyEnrolled);
        }
        if self.by_node.values().any(|r| r.device == record.device) {
            return Err(EnrollError::DeviceAlreadyEnrolled);
        }
        self.by_node.insert(node, record);
        Ok(())
    }

    /// Number of enrolled devices.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_node.len()
    }

    /// `true` when nothing is enrolled.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_node.is_empty()
    }

    /// The identity for `node`, or `None` when it is not enrolled.
    #[must_use]
    pub fn identity_for(&self, node: &NodeId) -> Option<ClientIdentity> {
        let record = self.by_node.get(node)?;
        let principal_id = format!("device:{}", record.device.as_str());
        let mut caps = caps_for_tier(record.tier);
        if !record.may_approve {
            caps.approve = false;
        }
        Some(ClientIdentity {
            principal: Principal::trusted_ingress(
                PrincipalKind::Human,
                principal_id.clone(),
                IngressSurface::Gateway,
                record.tier,
            ),
            tenant: Some(record.tenant.clone()),
            caps,
            device: Some(record.device.clone()),
            actor: ApprovalActor::Operator { id: principal_id },
            label: record.label.clone(),
            zone: TrustZone::Remote,
            strength: AuthStrength::MutualTls,
            connection: ConnectionId::next(),
            agent: None,
        })
    }
}

type Resolve<P> = dyn Fn(&P) -> Result<ClientIdentity, ComRefusal> + Send + Sync;

/// Resolves the transport's peer extension `P` into the trusted identity.
pub struct RemoteLayer<P> {
    resolve: Arc<Resolve<P>>,
}

impl<P> Clone for RemoteLayer<P> {
    fn clone(&self) -> Self {
        Self {
            resolve: Arc::clone(&self.resolve),
        }
    }
}

impl<P: 'static> RemoteLayer<P> {
    /// A layer with a custom resolver.
    #[must_use]
    pub fn new(
        resolve: impl Fn(&P) -> Result<ClientIdentity, ComRefusal> + Send + Sync + 'static,
    ) -> Self {
        Self {
            resolve: Arc::new(resolve),
        }
    }

    /// A layer that looks the peer's node id up in `registry`.
    /// `node_of` reads the node id out of the transport's peer type.
    #[must_use]
    pub fn from_registry(
        registry: Arc<DeviceRegistry>,
        node_of: impl Fn(&P) -> NodeId + Send + Sync + 'static,
    ) -> Self {
        Self::new(move |peer| {
            registry
                .identity_for(&node_of(peer))
                .ok_or(ComRefusal::Denied)
        })
    }
}

impl<S, P> Layer<S> for RemoteLayer<P> {
    type Service = RemoteService<S, P>;

    fn layer(&self, inner: S) -> Self::Service {
        RemoteService {
            inner,
            resolve: Arc::clone(&self.resolve),
        }
    }
}

/// Service produced by [`RemoteLayer`].
pub struct RemoteService<S, P> {
    inner: S,
    resolve: Arc<Resolve<P>>,
}

impl<S: Clone, P> Clone for RemoteService<S, P> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            resolve: Arc::clone(&self.resolve),
        }
    }
}

type BoxedFuture = Pin<Box<dyn Future<Output = Result<Response<Full<Bytes>>, Infallible>> + Send>>;

impl<S, P, B> Service<Request<B>> for RemoteService<S, P>
where
    S: Service<Request<B>, Response = Response<Full<Bytes>>, Error = Infallible>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    P: Send + Sync + 'static,
    B: Send + 'static,
{
    type Response = Response<Full<Bytes>>;
    type Error = Infallible;
    type Future = BoxedFuture;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Infallible>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut request: Request<B>) -> Self::Future {
        let Some(resolved) = request
            .extensions()
            .get::<P>()
            .map(|peer| (self.resolve)(peer))
        else {
            tracing::error!("session com: request without a transport peer");
            return Box::pin(ready(Ok(plain(
                hyper::StatusCode::INTERNAL_SERVER_ERROR,
                "connection has no transport identity",
            ))));
        };
        match resolved {
            Err(refusal) => Box::pin(ready(Ok(refusal.response()))),
            Ok(identity) => {
                // Overwrites anything an earlier layer might have set.
                request.extensions_mut().insert(TrustedPeer(identity));
                let clone = self.inner.clone();
                let mut inner = std::mem::replace(&mut self.inner, clone);
                Box::pin(async move { inner.call(request).await })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(device: &str, tier: PermissionTier) -> Result<DeviceRecord, harw_types::InvalidId> {
        Ok(DeviceRecord {
            device: DeviceId::try_from_str(device)?,
            tenant: TenantId::try_from_str("tenant-a")?,
            tier,
            label: format!("phone-{device}"),
            may_approve: false,
        })
    }

    fn node(name: &str) -> Result<NodeId, harw_types::InvalidId> {
        NodeId::try_from_str(name)
    }

    #[test]
    fn enrollment_is_one_to_one() -> Result<(), Box<dyn std::error::Error>> {
        let mut reg = DeviceRegistry::new();
        reg.enroll(node("node-a")?, record("dev-1", PermissionTier::Operator)?)?;
        assert_eq!(
            reg.enroll(node("node-a")?, record("dev-2", PermissionTier::Operator)?),
            Err(EnrollError::NodeAlreadyEnrolled)
        );
        assert_eq!(
            reg.enroll(node("node-b")?, record("dev-1", PermissionTier::Operator)?),
            Err(EnrollError::DeviceAlreadyEnrolled)
        );
        assert_eq!(reg.len(), 1);
        Ok(())
    }

    #[test]
    fn a_known_node_gets_a_remote_tenant_scoped_identity_without_gateway_or_approve()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut reg = DeviceRegistry::new();
        reg.enroll(node("node-a")?, record("dev-1", PermissionTier::Owner)?)?;
        let id = reg.identity_for(&node("node-a")?).ok_or("enrolled")?;
        assert!(id.validate().is_ok());
        assert!(id.is_remote());
        assert_eq!(id.tenant, Some(TenantId::try_from_str("tenant-a")?));
        assert_eq!(id.device, Some(DeviceId::try_from_str("dev-1")?));
        assert!(id.caps.control && id.caps.steer && id.caps.observe);
        assert!(!id.caps.approve, "remote approval is opt-in");
        assert!(!id.caps.gateway_read && !id.caps.gateway_admin && !id.caps.tool_call);
        Ok(())
    }

    #[test]
    fn approval_is_granted_only_when_the_record_opts_in() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut reg = DeviceRegistry::new();
        let mut rec = record("dev-1", PermissionTier::Operator)?;
        rec.may_approve = true;
        reg.enroll(node("node-a")?, rec)?;
        let id = reg.identity_for(&node("node-a")?).ok_or("enrolled")?;
        assert!(id.caps.approve);
        Ok(())
    }

    #[test]
    fn an_unknown_node_has_no_identity() -> Result<(), Box<dyn std::error::Error>> {
        let reg = DeviceRegistry::new();
        assert!(reg.identity_for(&node("node-x")?).is_none());
        Ok(())
    }
}
