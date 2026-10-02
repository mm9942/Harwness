//! Revocation closes live connections, not just future handshakes (W00 §7,
//! REV-01, REV-02, S08). Driven over in-memory duplex streams.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use harw_node_listener::{
    DeviceRecord, DeviceRegistry, HostRevoker, IdentityMapper, RegistryIdentityMapper,
    RevocationReport, RevocationSink, UpgradeHandler,
};
use harw_node_transport::{AuthenticatedPeer, UpgradeError};
use harw_protocol::methods::{METHOD_SESSION_ATTACH, METHOD_SESSION_CREATE, METHOD_SESSION_HELLO};
use harw_protocol::session_wire::SessionSummary;
use harw_protocol::{ProtocolVersion, RequestEnvelope, ResponseEnvelope, WireMessage};
use harw_session_host::approvals::MemoryApprovals;
use harw_session_host::driver::{
    CancelSignal, DriverFuture, EventSink, Setting, TurnDriver, TurnInput, TurnOutcome,
};
use harw_session_host::replay::MemoryTranscripts;
use harw_session_host::{HostConfig, SessionHost};
use harw_session_ws::{WsLimits, upgrade};
use harw_types::{DeviceId, NodeId, PermissionTier, SessionId, TenantId};
use http_body_util::Empty;
use hyper::body::{Bytes, Incoming};
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::Role;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
type Ws = WebSocketStream<TokioIo<hyper::upgrade::Upgraded>>;

/// Close code of `CloseReason::Revoked` (wire contract).
const CLOSE_REVOKED: u16 = 4004;

struct IdleDriver;

impl TurnDriver for IdleDriver {
    fn create_session(&self, _: &SessionId, _: Option<&str>) -> DriverFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn run_turn(
        &self,
        _: TurnInput,
        _: CancelSignal,
        _: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        Box::pin(async { Ok(TurnOutcome::Completed) })
    }
    fn resume_after_approval(
        &self,
        _: &SessionId,
        _: CancelSignal,
        _: Arc<dyn EventSink>,
    ) -> DriverFuture<'_, TurnOutcome> {
        Box::pin(async { Ok(TurnOutcome::Completed) })
    }
    fn apply_setting(&self, _: &SessionId, _: Setting) -> DriverFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    fn model_name(&self, _: &SessionId) -> Option<String> {
        None
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    handler: Arc<UpgradeHandler>,
    revoker: HostRevoker,
}

fn enroll(registry: &DeviceRegistry, node: &str, device: &str) -> TestResult {
    registry.enroll(&DeviceRecord {
        node_id: NodeId::from_str(node),
        device: DeviceId::from_str(device),
        tenant: TenantId::from_str("acme"),
        tier: PermissionTier::Operator,
        revoked: false,
        label: device.to_owned(),
    })?;
    Ok(())
}

fn fixture() -> TestResult<Fixture> {
    let dir = tempfile::tempdir()?;
    let registry = DeviceRegistry::new(dir.path());
    enroll(&registry, "node-a", "dev-a")?;
    enroll(&registry, "node-b", "dev-b")?;
    let host = Arc::new(SessionHost::open(
        HostConfig::new(dir.path().join("host")),
        Arc::new(IdleDriver),
        Arc::new(MemoryTranscripts::new()),
        Arc::new(MemoryApprovals::new()),
    )?);
    let mapper: Arc<dyn IdentityMapper> = Arc::new(RegistryIdentityMapper::new(dir.path()));
    let handler = Arc::new(UpgradeHandler::new(
        Arc::clone(&host),
        mapper,
        WsLimits::default(),
    ));
    let revoker = HostRevoker::new(host)
        .with_live(handler.live().clone())
        .with_registry(dir.path())
        .with_grace(Duration::from_millis(100));
    Ok(Fixture {
        _dir: dir,
        handler,
        revoker,
    })
}

fn peer(node: &str) -> AuthenticatedPeer {
    AuthenticatedPeer {
        node_id: NodeId::from_str(node),
        protocol_version: 1,
    }
}

/// `Ok(Ok(ws))` upgraded; `Ok(Err(status))` refused with that HTTP status.
async fn connect(handler: &Arc<UpgradeHandler>, node: &str) -> TestResult<Result<Ws, u16>> {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let handler = Arc::clone(handler);
    let peer = peer(node);
    tokio::spawn(async move {
        let service = service_fn(move |request: hyper::Request<Incoming>| {
            let handler = Arc::clone(&handler);
            let peer: Result<AuthenticatedPeer, UpgradeError> = Ok(peer.clone());
            async move { Ok::<_, std::convert::Infallible>(handler.handle(request, peer).await) }
        });
        let _ = hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(server_io), service)
            .with_upgrades()
            .await;
    });
    let (request, _key) = upgrade::client_request("localhost")?;
    let request: hyper::Request<Empty<Bytes>> = request;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(client_io)).await?;
    tokio::spawn(async move {
        let _ = connection.with_upgrades().await;
    });
    let response = sender.send_request(request).await?;
    if response.status() != hyper::StatusCode::SWITCHING_PROTOCOLS {
        return Ok(Err(response.status().as_u16()));
    }
    let upgraded = hyper::upgrade::on(response).await?;
    Ok(Ok(WebSocketStream::from_raw_socket(
        TokioIo::new(upgraded),
        Role::Client,
        None,
    )
    .await))
}

async fn call(
    ws: &mut Ws,
    id: &str,
    method: &str,
    params: serde_json::Value,
) -> TestResult<ResponseEnvelope> {
    let envelope = RequestEnvelope {
        jsonrpc: "2.0".into(),
        id: id.into(),
        method: method.into(),
        params,
        protocol: ProtocolVersion::default(),
    };
    ws.send(Message::text(serde_json::to_string(&envelope)?))
        .await?;
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await?
            .ok_or("closed")??;
        if let Message::Text(text) = message {
            if let WireMessage::Response(response) = serde_json::from_str(text.as_str())? {
                if response.id == id {
                    return Ok(response);
                }
            }
        }
    }
}

async fn hello(ws: &mut Ws) -> TestResult {
    let ack = call(
        ws,
        "h",
        METHOD_SESSION_HELLO,
        serde_json::json!({"client_label": "t", "wire_minor": 1}),
    )
    .await?;
    assert!(ack.error.is_none(), "{ack:?}");
    Ok(())
}

/// Read until the socket ends; `Some(code)` for a close frame carrying one.
async fn closed_with(ws: &mut Ws) -> TestResult<Option<u16>> {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next()).await? {
            None | Some(Err(_)) => return Ok(None),
            Some(Ok(Message::Close(frame))) => {
                return Ok(frame.map(|frame| u16::from(frame.code)));
            }
            Some(Ok(_)) => {}
        }
    }
}

#[tokio::test]
async fn revoking_a_device_closes_its_attached_connection_with_revoked() -> TestResult {
    let f = fixture()?;
    let Ok(mut ws) = connect(&f.handler, "node-a").await? else {
        return Err("upgrade refused".into());
    };
    hello(&mut ws).await?;
    let created = call(&mut ws, "c", METHOD_SESSION_CREATE, serde_json::json!({})).await?;
    let summary: SessionSummary = serde_json::from_value(created.result.ok_or("no result")?)?;
    let attached = call(
        &mut ws,
        "a",
        METHOD_SESSION_ATTACH,
        serde_json::json!({"session_id": summary.session_id.as_str()}),
    )
    .await?;
    assert!(attached.error.is_none(), "{attached:?}");
    assert_eq!(f.handler.live().len(), 1);

    let report = f.revoker.revoke_device(&DeviceId::from_str("dev-a"))?;

    assert_eq!(
        report,
        RevocationReport {
            connections_closed: 1,
            inputs_dropped: 0,
            contexts_revoked: 0,
        }
    );
    assert_eq!(closed_with(&mut ws).await?, Some(CLOSE_REVOKED));
    // The connection leaves the live table once its task ends.
    for _ in 0..50 {
        if f.handler.live().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(f.handler.live().is_empty());
    Ok(())
}

#[tokio::test]
async fn revoking_closes_an_unattached_connection_too_and_refuses_reconnect() -> TestResult {
    let f = fixture()?;
    let Ok(mut ws) = connect(&f.handler, "node-a").await? else {
        return Err("upgrade refused".into());
    };
    hello(&mut ws).await?;

    let report = f.revoker.revoke_device(&DeviceId::from_str("dev-a"))?;
    assert_eq!(report.connections_closed, 1);
    // Closed by the listener, not left open.
    assert!(closed_with(&mut ws).await?.is_some());

    // Reconnect is refused (registry marked revoked, host refuses too).
    let refused = connect(&f.handler, "node-a").await?;
    assert!(matches!(refused, Err(403)), "{:?}", refused.map(|_| ()));
    Ok(())
}

#[tokio::test]
async fn revoking_one_device_leaves_other_devices_connected() -> TestResult {
    let f = fixture()?;
    let Ok(mut a) = connect(&f.handler, "node-a").await? else {
        return Err("a refused".into());
    };
    let Ok(mut b) = connect(&f.handler, "node-b").await? else {
        return Err("b refused".into());
    };
    hello(&mut a).await?;
    hello(&mut b).await?;
    assert_eq!(f.handler.live().len(), 2);

    let report = f.revoker.revoke_device(&DeviceId::from_str("dev-a"))?;
    assert_eq!(report.connections_closed, 1);
    assert!(closed_with(&mut a).await?.is_some());

    // dev-b still answers.
    let listed = call(&mut b, "l", "session.list", serde_json::json!({})).await?;
    assert!(listed.error.is_none(), "{listed:?}");
    Ok(())
}

#[tokio::test]
async fn revoking_an_unknown_device_is_a_quiet_no_op() -> TestResult {
    let f = fixture()?;
    let report = f.revoker.revoke_device(&DeviceId::from_str("dev-nobody"))?;
    assert_eq!(report, RevocationReport::default());
    Ok(())
}
