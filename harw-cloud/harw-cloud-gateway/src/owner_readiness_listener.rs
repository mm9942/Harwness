//! Fixed proof-only ingress for an explicitly protected local Unix socket.
//! It never binds the socket, upgrades HTTP, or handles ordinary sessions.

use std::convert::Infallible;
use std::future::Future;
use std::io;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use harw_node_transport::{
    NodeBody, NodeTransportServer, OwnerProcessChannelBinding, TransportError, empty_body, full_body,
    owner_process_channel_binding_of,
};
use harw_session_host::HostError;
use harw_session_host::owner_creation::ML_DSA_65_SIGNATURE_LEN;
use harw_session_host::owner_readiness_contract::{
    MAX_OWNER_READINESS_REQUEST_BYTES, MAX_OWNER_READINESS_RESPONSE_BYTES, OWNER_READINESS_PATH,
    OwnerReadinessChallenge, OwnerReadinessResponse,
};
use http::{Request, Response, StatusCode, header};
use http_body_util::BodyExt;
use tokio::net::UnixStream;
use tower_service::Service;

use crate::owner_report::{read_json, request_len};

const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(3);
const PROOF_DEADLINE: Duration = Duration::from_secs(3);
// A verifier may still sample NetSec after the proof response. Do not close
// its exact connection before those checks, but cap all idle connection time.
const STREAM_DEADLINE: Duration = Duration::from_secs(10);

/// Only trusted composition supplies the guarded producer, never a request.
/// The producer checks its private exact scope, NetSec and key custody.
pub trait OwnerReadinessHandler: Send + Sync {
    fn answer<'a>(
        &'a self,
        channel: &'a OwnerProcessChannelBinding,
        challenge: &'a OwnerReadinessChallenge,
    ) -> Pin<Box<dyn Future<Output = Result<OwnerReadinessResponse, HostError>> + Send + 'a>>;
}

/// Independently selected proof-only access policy. The socket must already be
/// bound in an operator-controlled private parent. This type does not install
/// trust, open paths or inspect an arbitrary remote address.
pub struct OwnerReadinessUnixLane<'a> {
    pub local_socket: &'a Path,
    pub expected_peer_uid: u32,
    pub hub_node: &'a str,
    pub hub_key_fingerprint: [u8; 32],
}

#[derive(Clone)]
struct ReadinessService {
    hub_node: Arc<str>,
    hub_key_fingerprint: [u8; 32],
    handler: Arc<dyn OwnerReadinessHandler>,
    attempted: Arc<AtomicBool>,
}

impl ReadinessService {
    fn accepted_binding<B>(&self, request: &Request<B>) -> Option<OwnerProcessChannelBinding> {
        owner_process_channel_binding_of(request).filter(|binding| {
            !binding.is_dialer()
                && binding.peer().node_id.as_str() == self.hub_node.as_ref()
                && binding.verified_peer_key_fingerprint() == Some(self.hub_key_fingerprint)
        })
    }
}

fn refusal() -> Response<NodeBody> {
    let mut response = Response::new(empty_body());
    *response.status_mut() = StatusCode::NOT_FOUND;
    response
}

impl<B> Service<Request<B>> for ReadinessService
where
    B: BodyExt + Unpin + Send + 'static,
    B::Data: AsRef<[u8]> + Send,
{
    type Response = Response<NodeBody>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _ctx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, mut request: Request<B>) -> Self::Future {
        let service = self.clone();
        Box::pin(async move {
            let proof = tokio::time::timeout(PROOF_DEADLINE, async {
                let binding = service.accepted_binding(&request)?;
                // One attempt per accepted connection, including malformed
                // input. No route or legacy probe survives an unsuccessful proof.
                if service.attempted.swap(true, Ordering::AcqRel)
                    || request.headers().contains_key(header::UPGRADE)
                    || request.headers().contains_key(header::CONNECTION)
                {
                    return None;
                }
                let size = request_len(
                    &request, OWNER_READINESS_PATH, MAX_OWNER_READINESS_REQUEST_BYTES,
                ).ok()?;
                let encoded = read_json(
                    request.body_mut(), size, MAX_OWNER_READINESS_REQUEST_BYTES,
                ).await.ok()?;
                let challenge: OwnerReadinessChallenge = serde_json::from_slice(&encoded).ok()?;
                if challenge.nonce == [0; 32] || challenge.receipt_digest == [0; 32]
                    || !binding.same_live_channel(&service.accepted_binding(&request)?)
                {
                    return None;
                }
                let answer = service.handler.answer(&binding, &challenge).await.ok()?;
                if answer.signature.len() != ML_DSA_65_SIGNATURE_LEN
                    || !binding.same_live_channel(&service.accepted_binding(&request)?)
                {
                    return None;
                }
                let body = serde_json::to_vec(&answer).ok()?;
                if body.len() > MAX_OWNER_READINESS_RESPONSE_BYTES {
                    return None;
                }
                Response::builder().status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::CONTENT_LENGTH, body.len().to_string())
                    .body(full_body(body)).ok()
            }).await.ok().flatten();
            Ok(proof.unwrap_or_else(refusal))
        })
    }
}

/// Serve a single accepted proof-only Unix stream: check local endpoint, its
/// kernel socket peer UID, then require the exact accepted TLS/node witness.
/// The transport is HTTP/1 without upgrade and the proof has a hard deadline.
/// Dropping this future or completing `shutdown` closes the exact channel.
pub async fn serve_guarded_owner_readiness_io<F: Future<Output = ()>>(
    server: &NodeTransportServer,
    stream: UnixStream,
    lane: OwnerReadinessUnixLane<'_>,
    handler: Arc<dyn OwnerReadinessHandler>,
    shutdown: F,
) -> Result<(), TransportError> {
    let valid_hub = !lane.hub_node.is_empty()
        && lane.hub_node.len() <= 128
        && !lane.hub_node.starts_with('.')
        && lane.hub_node.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if !lane.local_socket.is_absolute() || !valid_hub || lane.hub_key_fingerprint == [0; 32]
        || stream.local_addr()?.as_pathname() != Some(lane.local_socket)
        || stream.peer_cred()?.uid() != lane.expected_peer_uid
    {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "proof-only Unix lane refused").into());
    }
    let service = ReadinessService {
        hub_node: Arc::from(lane.hub_node),
        hub_key_fingerprint: lane.hub_key_fingerprint,
        handler,
        attempted: Arc::new(AtomicBool::new(false)),
    };
    tokio::select! {
        biased;
        () = shutdown => Ok(()),
        () = tokio::time::sleep(STREAM_DEADLINE) => Ok(()),
        outcome = server.serve_io(stream, service, HANDSHAKE_DEADLINE) => outcome,
    }
}
