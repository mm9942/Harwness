//! `network.sock` server: Hyper HTTP/1 over `AF_UNIX`/`SOCK_STREAM`.
//!
//! # Authorization
//! The caller is identified exclusively by the kernel-verified peer
//! credentials of the socket (`SO_PEERCRED`, read once right after
//! `accept()` via `tokio::net::UnixStream::peer_cred`), never by anything in
//! the request. A peer whose uid is not in the configured allowlist gets
//! `403 peer_not_allowed` on **every** request, including `/v1/health`, and
//! nothing it sends reaches the store.
//!
//! # Limits
//! - request bodies are read through [`http_body_util::Limited`]
//!   (`max_body_bytes`); a larger `Content-Length` is refused before reading;
//! - header and body read timeouts;
//! - a bound on concurrently served connections (excess connections are
//!   closed immediately);
//! - JSON bodies use `deny_unknown_fields` throughout.
//!
//! # Shutdown
//! [`serve`] stops accepting as soon as its `shutdown` future resolves, asks
//! every open connection to finish its in-flight request
//! (`graceful_shutdown`) and aborts what is still running after
//! `shutdown_grace`.

use std::collections::BTreeSet;
use std::convert::Infallible;
use std::future::Future;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use harw_types::Clock;
use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::Incoming;
use hyper::header::{CACHE_CONTROL, CONTENT_LENGTH, CONTENT_TYPE, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};
use tokio::task::JoinSet;

use crate::api::{
    self, CapabilitiesResponse, Endpoint, ErrorBody, ErrorDetail, HealthResponse,
    NodeActionRequest, NodeList, PROTOCOL_VERSION, ResolveError, RouteList, SERVICE_NAME,
    VersionResponse, ZoneList,
};
use crate::config::NetsecConfig;
use crate::error::{NetsecError, NetsecResult};
use crate::ids::parse_node_id;
use crate::model::NodeRegistration;
use crate::store::NetsecStore;

/// Response type of every handler.
pub type NetsecResponse = Response<Full<Bytes>>;

/// Mode of a socket this daemon binds itself (owner + group read/write).
pub const SOCKET_MODE: u32 = 0o660;
/// Deadline for a client to send the complete request head.
pub const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// Deadline for a client to send the complete (bounded) body.
pub const BODY_READ_TIMEOUT: Duration = Duration::from_secs(10);
/// How long in-flight requests may run after shutdown was requested.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// Pause after a failed `accept()`.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);
/// Deadline of the liveness probe against an existing socket file.
const STALE_SOCKET_PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// Kernel-verified identity of a connected peer (`SO_PEERCRED`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerIdentity {
    /// Effective uid of the peer.
    pub uid: u32,
    /// Effective gid of the peer.
    pub gid: u32,
    /// Process id of the peer, if the kernel reported one.
    pub pid: Option<i32>,
}

/// Reads the peer credentials of an accepted stream.
///
/// # Errors
/// The I/O error of `getsockopt(SO_PEERCRED)`.
pub fn peer_identity(stream: &UnixStream) -> std::io::Result<PeerIdentity> {
    let credentials = stream.peer_cred()?;
    Ok(PeerIdentity {
        uid: credentials.uid(),
        gid: credentials.gid(),
        pid: credentials.pid(),
    })
}

/// Runtime settings of the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSettings {
    /// Peer uids allowed to use the API.
    pub allowed_uids: BTreeSet<u32>,
    /// Maximum request body in bytes.
    pub max_body_bytes: usize,
    /// Maximum concurrently served connections.
    pub max_connections: usize,
    /// See [`HEADER_READ_TIMEOUT`].
    pub header_read_timeout: Duration,
    /// See [`BODY_READ_TIMEOUT`].
    pub body_read_timeout: Duration,
    /// See [`SHUTDOWN_GRACE`].
    pub shutdown_grace: Duration,
}

impl ServerSettings {
    /// Settings derived from a validated configuration.
    #[must_use]
    pub fn from_config(config: &NetsecConfig) -> Self {
        Self {
            allowed_uids: config.allowed_uids.clone(),
            max_body_bytes: config.max_body_bytes,
            max_connections: config.max_connections,
            header_read_timeout: HEADER_READ_TIMEOUT,
            body_read_timeout: BODY_READ_TIMEOUT,
            shutdown_grace: SHUTDOWN_GRACE,
        }
    }
}

/// An HTTP-level error: status, stable code, message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    /// The HTTP status.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The stable error code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
    }

    fn into_response(self) -> NetsecResponse {
        json_response(
            self.status,
            &ErrorBody {
                error: ErrorDetail {
                    code: self.code.to_owned(),
                    message: self.message,
                },
            },
        )
    }
}

impl From<ResolveError> for ApiError {
    fn from(error: ResolveError) -> Self {
        match error {
            ResolveError::NotFound => {
                Self::new(StatusCode::NOT_FOUND, "not_found", "no such endpoint")
            }
            ResolveError::MethodNotAllowed => Self::new(
                StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
                "method not allowed on this endpoint",
            ),
        }
    }
}

impl From<NetsecError> for ApiError {
    fn from(error: NetsecError) -> Self {
        let message = error.to_string();
        match error {
            NetsecError::InvalidIdentifier { .. } | NetsecError::InvalidField { .. } => {
                Self::new(StatusCode::BAD_REQUEST, "invalid_request", message)
            }
            NetsecError::InvalidTransition { .. } => {
                Self::new(StatusCode::CONFLICT, "invalid_transition", message)
            }
            NetsecError::NodeExists { .. } => {
                Self::new(StatusCode::CONFLICT, "node_exists", message)
            }
            NetsecError::NodeNotFound { .. } => {
                Self::new(StatusCode::NOT_FOUND, "node_not_found", message)
            }
            NetsecError::UnknownZone { .. } => {
                Self::new(StatusCode::UNPROCESSABLE_ENTITY, "unknown_zone", message)
            }
            NetsecError::InvalidRoute { .. } => {
                Self::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_route", message)
            }
            NetsecError::CapacityExhausted { .. } => Self::new(
                StatusCode::INSUFFICIENT_STORAGE,
                "capacity_exhausted",
                message,
            ),
            // Store, I/O and configuration failures may carry paths: log
            // them, answer generically.
            other => {
                tracing::error!(error = %other, "internal error while serving a request");
                Self::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "internal error",
                )
            }
        }
    }
}

/// The request handler: store, clock and settings.
pub struct NetsecService {
    store: Arc<NetsecStore>,
    clock: Arc<dyn Clock>,
    settings: ServerSettings,
}

impl NetsecService {
    /// Builds the handler.
    #[must_use]
    pub fn new(store: Arc<NetsecStore>, clock: Arc<dyn Clock>, settings: ServerSettings) -> Self {
        Self {
            store,
            clock,
            settings,
        }
    }

    /// The server settings.
    #[must_use]
    pub fn settings(&self) -> &ServerSettings {
        &self.settings
    }

    /// Whether `peer` may use the API.
    #[must_use]
    pub fn is_authorized(&self, peer: &PeerIdentity) -> bool {
        self.settings.allowed_uids.contains(&peer.uid)
    }

    /// Handles one request from `peer`.
    pub async fn handle(
        self: Arc<Self>,
        request: Request<Incoming>,
        peer: PeerIdentity,
    ) -> NetsecResponse {
        if !self.is_authorized(&peer) {
            tracing::warn!(uid = peer.uid, pid = ?peer.pid, "peer uid not in allowlist; request refused");
            return ApiError::new(
                StatusCode::FORBIDDEN,
                "peer_not_allowed",
                "peer uid is not allowed to use this socket",
            )
            .into_response();
        }
        match self.dispatch(request, peer).await {
            Ok(response) => response,
            Err(error) => error.into_response(),
        }
    }

    async fn dispatch(
        &self,
        request: Request<Incoming>,
        peer: PeerIdentity,
    ) -> Result<NetsecResponse, ApiError> {
        let endpoint = api::resolve(request.method(), request.uri().path())?;
        match endpoint {
            Endpoint::Health => Ok(json_response(
                StatusCode::OK,
                &HealthResponse {
                    status: "ok".to_owned(),
                },
            )),
            Endpoint::Version => Ok(json_response(
                StatusCode::OK,
                &VersionResponse {
                    service: SERVICE_NAME.to_owned(),
                    version: env!("CARGO_PKG_VERSION").to_owned(),
                    protocol: PROTOCOL_VERSION,
                },
            )),
            Endpoint::Capabilities => Ok(json_response(
                StatusCode::OK,
                &CapabilitiesResponse::current(),
            )),
            Endpoint::ListNodes => {
                let snapshot = self.run(|store| store.snapshot()).await?;
                let nodes = snapshot.nodes().cloned().collect();
                Ok(json_response(StatusCode::OK, &NodeList { nodes }))
            }
            Endpoint::ListZones => {
                let snapshot = self.run(|store| store.snapshot()).await?;
                let zones = snapshot.zones().cloned().collect();
                Ok(json_response(StatusCode::OK, &ZoneList { zones }))
            }
            Endpoint::ListRoutes => {
                let snapshot = self.run(|store| store.snapshot()).await?;
                let routes = snapshot.routes().cloned().collect();
                Ok(json_response(StatusCode::OK, &RouteList { routes }))
            }
            Endpoint::GetNode(raw) => {
                let id = parse_node_id(&raw)?;
                let record = self.run(move |store| store.node(&id)).await?;
                Ok(json_response(StatusCode::OK, &record))
            }
            Endpoint::RegisterNode => {
                let body = self.read_body(request).await?;
                let registration: NodeRegistration = parse_json(&body)?;
                let now = self.clock.now();
                let record = self
                    .run(move |store| store.register_node(registration, now))
                    .await?;
                tracing::info!(node = %record.id, zone = %record.zone, uid = peer.uid, "node registered");
                Ok(json_response(StatusCode::CREATED, &record))
            }
            Endpoint::NodeAction(raw, event) => {
                let id = parse_node_id(&raw)?;
                let body = self.read_body(request).await?;
                let action = if body.is_empty() {
                    NodeActionRequest::default()
                } else {
                    parse_json::<NodeActionRequest>(&body)?
                };
                let now = self.clock.now();
                let uid = peer.uid;
                let record = self
                    .run(move |store| store.apply_event(&id, event, now, Some(uid), action.reason))
                    .await?;
                tracing::info!(
                    node = %record.id,
                    event = event.as_str(),
                    state = record.state.as_str(),
                    uid,
                    "node transition"
                );
                Ok(json_response(StatusCode::OK, &record))
            }
        }
    }

    /// Runs blocking store work off the async executor.
    async fn run<T, F>(&self, work: F) -> Result<T, ApiError>
    where
        F: FnOnce(&NetsecStore) -> NetsecResult<T> + Send + 'static,
        T: Send + 'static,
    {
        let store = Arc::clone(&self.store);
        let joined = tokio::task::spawn_blocking(move || work(store.as_ref())).await;
        match joined {
            Ok(result) => result.map_err(ApiError::from),
            Err(_) => Err(ApiError::from(NetsecError::TaskFailed)),
        }
    }

    /// Reads a bounded body; a non-empty body must be `application/json`.
    async fn read_body(&self, request: Request<Incoming>) -> Result<Bytes, ApiError> {
        let max = self.settings.max_body_bytes;
        let too_large = || {
            ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "body_too_large",
                format!("request body exceeds {max} bytes"),
            )
        };
        let declared = request
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok());
        if declared.is_some_and(|length| length > u64::try_from(max).unwrap_or(u64::MAX)) {
            return Err(too_large());
        }
        let is_json = request
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value
                    .split(';')
                    .next()
                    .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
            });
        let limited = Limited::new(request.into_body(), max);
        let bytes =
            match tokio::time::timeout(self.settings.body_read_timeout, limited.collect()).await {
                Err(_) => {
                    return Err(ApiError::new(
                        StatusCode::REQUEST_TIMEOUT,
                        "body_timeout",
                        "request body not received in time",
                    ));
                }
                Ok(Err(error)) => {
                    if error.downcast_ref::<LengthLimitError>().is_some() {
                        return Err(too_large());
                    }
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "body_unreadable",
                        "request body could not be read",
                    ));
                }
                Ok(Ok(collected)) => collected.to_bytes(),
            };
        if !bytes.is_empty() && !is_json {
            return Err(ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "unsupported_media_type",
                "request body must be application/json",
            ));
        }
        Ok(bytes)
    }
}

fn parse_json<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(bytes)
        .map_err(|error| ApiError::new(StatusCode::BAD_REQUEST, "invalid_json", error.to_string()))
}

fn json_response<T: Serialize>(status: StatusCode, value: &T) -> NetsecResponse {
    let (status, bytes) = match serde_json::to_vec(value) {
        Ok(bytes) => (status, Bytes::from(bytes)),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Bytes::from_static(
                br#"{"error":{"code":"internal","message":"serialization failed"}}"#,
            ),
        ),
    };
    let mut response = Response::new(Full::new(bytes));
    *response.status_mut() = status;
    let headers = response.headers_mut();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Binds the control socket at `path` without ever deleting foreign files.
///
/// # Description
/// If something exists at `path` (checked without following symlinks): a
/// non-socket is [`NetsecError::SocketPathOccupied`]; a socket is probed
/// with `connect` and removed **only** on `ECONNREFUSED` (a stale socket of
/// a crashed run); anything else is [`NetsecError::SocketInUse`]. Same rule
/// as `harw-web`. The parent directory must exist and belong to the
/// operator (systemd `RuntimeDirectory=`). The socket gets mode `0660`.
///
/// # Errors
/// See above, plus [`NetsecError::Io`] for bind/chmod failures.
pub async fn bind_socket(path: &Path) -> NetsecResult<UnixListener> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_socket() {
                return Err(NetsecError::SocketPathOccupied {
                    path: path.to_path_buf(),
                });
            }
            let probe =
                tokio::time::timeout(STALE_SOCKET_PROBE_TIMEOUT, UnixStream::connect(path)).await;
            match probe {
                Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                    std::fs::remove_file(path).map_err(NetsecError::io("remove stale socket"))?;
                }
                _ => {
                    return Err(NetsecError::SocketInUse {
                        path: path.to_path_buf(),
                    });
                }
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(NetsecError::Io {
                context: "inspect socket path",
                source,
            });
        }
    }
    let listener = UnixListener::bind(path).map_err(NetsecError::io("bind socket"))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(SOCKET_MODE))
        .map_err(NetsecError::io("set socket mode"))?;
    Ok(listener)
}

/// Removes the socket file at `path` if (and only if) it is a socket.
///
/// # Errors
/// [`NetsecError::Io`] if the removal fails.
pub fn remove_socket(path: &Path) -> NetsecResult<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            std::fs::remove_file(path).map_err(NetsecError::io("remove socket"))
        }
        _ => Ok(()),
    }
}

/// Serves `listener` until `shutdown` resolves, then drains connections.
pub async fn serve<F>(listener: UnixListener, service: Arc<NetsecService>, shutdown: F)
where
    F: Future<Output = ()>,
{
    let (stop_tx, stop_rx) = watch::channel(false);
    let permits = Arc::new(Semaphore::new(service.settings.max_connections));
    let mut connections = JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            () = &mut shutdown => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            accepted = listener.accept() => {
                let stream = match accepted {
                    Ok((stream, _address)) => stream,
                    Err(error) => {
                        tracing::warn!(%error, "accept failed; backing off");
                        tokio::time::sleep(ACCEPT_BACKOFF).await;
                        continue;
                    }
                };
                let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
                    tracing::warn!("connection limit reached; closing new connection");
                    continue;
                };
                let peer = match peer_identity(&stream) {
                    Ok(peer) => peer,
                    Err(error) => {
                        // No kernel-verified identity, no HTTP at all.
                        tracing::warn!(%error, "SO_PEERCRED unavailable; closing connection");
                        continue;
                    }
                };
                connections.spawn(serve_connection(
                    stream,
                    peer,
                    Arc::clone(&service),
                    stop_rx.clone(),
                    permit,
                ));
            }
        }
    }
    drop(listener);
    let _previous = stop_tx.send_replace(true);
    let grace = service.settings.shutdown_grace;
    let drained = tokio::time::timeout(grace, async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    if drained.is_err() {
        tracing::warn!("shutdown grace period elapsed; aborting open connections");
        connections.abort_all();
        while connections.join_next().await.is_some() {}
    }
}

async fn serve_connection(
    stream: UnixStream,
    peer: PeerIdentity,
    service: Arc<NetsecService>,
    mut stop: watch::Receiver<bool>,
    _permit: OwnedSemaphorePermit,
) {
    let mut builder = http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(service.settings.header_read_timeout)
        .keep_alive(true);
    let handler = Arc::clone(&service);
    let connection = builder.serve_connection(
        TokioIo::new(stream),
        service_fn(move |request| {
            let handler = Arc::clone(&handler);
            async move { Ok::<_, Infallible>(handler.handle(request, peer).await) }
        }),
    );
    tokio::pin!(connection);
    tokio::select! {
        result = connection.as_mut() => {
            if let Err(error) = result {
                tracing::debug!(%error, "connection closed with error");
            }
        }
        _ = stop.changed() => {
            connection.as_mut().graceful_shutdown();
            if let Err(error) = connection.as_mut().await {
                tracing::debug!(%error, "connection closed with error during shutdown");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_domain_errors_map_to_client_statuses() {
        use crate::state_machine::{NodeEvent, NodeState};
        let cases = [
            (
                NetsecError::InvalidTransition {
                    from: NodeState::Revoked,
                    event: NodeEvent::Drain,
                },
                StatusCode::CONFLICT,
            ),
            (
                NetsecError::NodeNotFound { id: "x".to_owned() },
                StatusCode::NOT_FOUND,
            ),
            (
                NetsecError::NodeExists { id: "x".to_owned() },
                StatusCode::CONFLICT,
            ),
            (
                NetsecError::UnknownZone {
                    zone: "z".to_owned(),
                },
                StatusCode::UNPROCESSABLE_ENTITY,
            ),
            (
                NetsecError::CapacityExhausted { limit: 1 },
                StatusCode::INSUFFICIENT_STORAGE,
            ),
            (
                NetsecError::invalid_identifier("node", "a/b"),
                StatusCode::BAD_REQUEST,
            ),
        ];
        for (error, status) in cases {
            assert_eq!(ApiError::from(error).status(), status);
        }
    }

    #[test]
    fn test_internal_errors_do_not_leak_paths() {
        let error = ApiError::from(NetsecError::StoreCorrupt {
            path: "/var/lib/harw-netsec/state.json".into(),
            reason: "x".to_owned(),
        });
        assert_eq!(error.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(error.code(), "internal");
        assert!(!error.message.contains("/var/lib"));
    }
}
