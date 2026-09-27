//! End-to-end tests over a real Unix socket in a temp directory.
//!
//! Each test binds `network.sock` in a fresh temp dir, serves it on the test
//! runtime and talks raw HTTP/1.1 to it (`Connection: close`), so the whole
//! stack — accept, `SO_PEERCRED`, Hyper, routing, store — is exercised.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harw_netsec::server::{NetsecService, ServerSettings, bind_socket, remove_socket, serve};
use harw_netsec::{NetsecConfig, NetsecStore};
use harw_types::SystemClock;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Daemon {
    _temp: tempfile::TempDir,
    socket: PathBuf,
    state_dir: PathBuf,
    stop: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
}

impl Daemon {
    async fn start(allowed_uids: BTreeSet<u32>, max_body_bytes: Option<usize>) -> TestResult<Self> {
        let temp = tempfile::tempdir()?;
        let socket = temp.path().join("network.sock");
        let state_dir = temp.path().join("state");
        let mut config = NetsecConfig::new(&socket, &state_dir, allowed_uids)?;
        if let Some(limit) = max_body_bytes {
            config.max_body_bytes = limit;
        }
        config.validate()?;
        let store = Arc::new(NetsecStore::open(&config.state_dir, config.max_nodes)?);
        store.ensure_zones(&config.zones)?;
        let listener = bind_socket(&config.socket_path).await?;
        let service = Arc::new(NetsecService::new(
            store,
            Arc::new(SystemClock),
            ServerSettings::from_config(&config),
        ));
        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let task = tokio::spawn(serve(listener, service, async move {
            let _ = stop_rx.await;
        }));
        Ok(Self {
            _temp: temp,
            socket,
            state_dir,
            stop: Some(stop_tx),
            task: Some(task),
        })
    }

    async fn stop(&mut self) -> TestResult {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.task.take() {
            tokio::time::timeout(std::time::Duration::from_secs(10), task).await??;
        }
        remove_socket(&self.socket)?;
        Ok(())
    }
}

/// The uid of this test process, read the same way the daemon reads peers.
fn own_uid() -> TestResult<u32> {
    let (left, _right) = std::os::unix::net::UnixStream::pair()?;
    left.set_nonblocking(true)?;
    let left = UnixStream::from_std(left)?;
    Ok(left.peer_cred()?.uid())
}

async fn call(
    socket: &Path,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> TestResult<(u16, Value)> {
    let mut stream = UnixStream::connect(socket).await?;
    let body = body.unwrap_or("");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await?;
    // Read until EOF. A server that answers before consuming the body (413)
    // may close with unread data, which Linux reports to this side as
    // ECONNRESET after the response bytes; treat that as EOF.
    let mut raw = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => raw.extend_from_slice(chunk.get(..read).unwrap_or_default()),
            Err(error)
                if error.kind() == std::io::ErrorKind::ConnectionReset && !raw.is_empty() =>
            {
                break;
            }
            Err(error) => return Err(error.into()),
        }
    }
    let text = String::from_utf8(raw)?;
    let (head, payload) = text
        .split_once("\r\n\r\n")
        .ok_or("response has no header terminator")?;
    let status: u16 = head
        .split(' ')
        .nth(1)
        .ok_or("response has no status")?
        .parse()?;
    let value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(payload)?
    };
    Ok((status, value))
}

fn field<'a>(value: &'a Value, key: &str) -> TestResult<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing string field {key} in {value}").into())
}

fn error_code(value: &Value) -> Option<&str> {
    value.pointer("/error/code").and_then(Value::as_str)
}

const REGISTER_N1: &str = r#"{"id":"node-1","display_name":"Node one",
    "addresses":["10.0.0.7:7443","node-1.fleet.example:7443"],
    "zone":"local","identity_key_ref":"authhub:node/node-1"}"#;

#[tokio::test]
async fn test_health_version_capabilities() -> TestResult {
    let mut daemon = Daemon::start(BTreeSet::from([own_uid()?]), None).await?;
    let (status, health) = call(&daemon.socket, "GET", "/v1/health", None).await?;
    assert_eq!(status, 200);
    assert_eq!(field(&health, "status")?, "ok");

    let (status, version) = call(&daemon.socket, "GET", "/v1/version", None).await?;
    assert_eq!(status, 200);
    assert_eq!(field(&version, "service")?, "harw-netsec");
    assert_eq!(version.get("protocol").and_then(Value::as_u64), Some(1));

    let (status, caps) = call(&daemon.socket, "GET", "/v1/capabilities", None).await?;
    assert_eq!(status, 200);
    let operations = caps
        .get("operations")
        .and_then(Value::as_array)
        .ok_or("no operations")?;
    assert!(operations.iter().any(|op| op == "node.drain"));

    let (status, zones) = call(&daemon.socket, "GET", "/v1/zones", None).await?;
    assert_eq!(status, 200);
    assert_eq!(
        zones.pointer("/zones/0/id").and_then(Value::as_str),
        Some("local")
    );
    daemon.stop().await
}

#[tokio::test]
async fn test_register_list_drain_drained_lifecycle() -> TestResult {
    let uid = own_uid()?;
    let mut daemon = Daemon::start(BTreeSet::from([uid]), None).await?;
    let socket = daemon.socket.clone();

    let (status, created) = call(&socket, "POST", "/v1/nodes", Some(REGISTER_N1)).await?;
    assert_eq!(status, 201, "{created}");
    assert_eq!(field(&created, "state")?, "pending");
    assert!(created.get("last_seen").is_some_and(Value::is_null));

    let (status, list) = call(&socket, "GET", "/v1/nodes", None).await?;
    assert_eq!(status, 200);
    let nodes = list
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or("no nodes")?;
    assert_eq!(nodes.len(), 1);
    assert_eq!(
        nodes.first().map(|n| field(n, "id")).transpose()?,
        Some("node-1")
    );

    // A pending node carries no traffic: draining it is a conflict.
    let (status, error) = call(&socket, "POST", "/v1/nodes/node-1:drain", None).await?;
    assert_eq!(status, 409);
    assert_eq!(error_code(&error), Some("invalid_transition"));

    let (status, active) = call(&socket, "POST", "/v1/nodes/node-1:activate", None).await?;
    assert_eq!(status, 200, "{active}");
    assert_eq!(field(&active, "state")?, "active");

    let (status, draining) = call(
        &socket,
        "POST",
        "/v1/nodes/node-1:drain",
        Some(r#"{"reason":"kernel update"}"#),
    )
    .await?;
    assert_eq!(status, 200, "{draining}");
    assert_eq!(field(&draining, "state")?, "draining");
    assert_eq!(
        draining
            .pointer("/last_transition/reason")
            .and_then(Value::as_str),
        Some("kernel update")
    );
    assert_eq!(
        draining
            .pointer("/last_transition/by_uid")
            .and_then(Value::as_u64),
        Some(u64::from(uid))
    );

    let (status, again) = call(&socket, "POST", "/v1/nodes/node-1:drain", None).await?;
    assert_eq!(status, 409, "repeated drain is rejected: {again}");

    let (status, drained) = call(&socket, "POST", "/v1/nodes/node-1:complete-drain", None).await?;
    assert_eq!(status, 200, "{drained}");
    assert_eq!(field(&drained, "state")?, "drained");

    let (status, fetched) = call(&socket, "GET", "/v1/nodes/node-1", None).await?;
    assert_eq!(status, 200);
    assert_eq!(field(&fetched, "state")?, "drained");

    let (status, revoked) = call(&socket, "POST", "/v1/nodes/node-1:revoke", None).await?;
    assert_eq!(status, 200);
    assert_eq!(field(&revoked, "state")?, "revoked");
    let (status, _) = call(&socket, "POST", "/v1/nodes/node-1:undrain", None).await?;
    assert_eq!(status, 409, "revoked is terminal");

    // Graceful shutdown, then the state must be on disk.
    let state_dir = daemon.state_dir.clone();
    daemon.stop().await?;
    assert!(!socket.exists(), "socket removed after shutdown");
    let store = NetsecStore::open(&state_dir, 16)?;
    let snapshot = store.snapshot()?;
    let node = snapshot
        .node("node-1")
        .ok_or("node missing after restart")?;
    assert_eq!(node.state, harw_netsec::NodeState::Revoked);
    Ok(())
}

#[tokio::test]
async fn test_unauthorized_uid_is_rejected_and_changes_nothing() -> TestResult {
    let uid = own_uid()?;
    let other = uid.wrapping_add(1);
    let mut daemon = Daemon::start(BTreeSet::from([other]), None).await?;

    for (method, path, body) in [
        ("GET", "/v1/health", None),
        ("GET", "/v1/nodes", None),
        ("POST", "/v1/nodes", Some(REGISTER_N1)),
        ("POST", "/v1/nodes/node-1:drain", None),
    ] {
        let (status, error) = call(&daemon.socket, method, path, body).await?;
        assert_eq!(status, 403, "{method} {path}");
        assert_eq!(error_code(&error), Some("peer_not_allowed"));
    }
    let state_dir = daemon.state_dir.clone();
    daemon.stop().await?;
    let store = NetsecStore::open(&state_dir, 16)?;
    assert_eq!(store.snapshot()?.node_count(), 0);
    Ok(())
}

#[tokio::test]
async fn test_request_validation_and_limits() -> TestResult {
    let mut daemon = Daemon::start(BTreeSet::from([own_uid()?]), Some(512)).await?;
    let socket = daemon.socket.clone();

    let (status, error) = call(&socket, "GET", "/v1/nope", None).await?;
    assert_eq!((status, error_code(&error)), (404, Some("not_found")));

    let (status, error) = call(&socket, "DELETE", "/v1/nodes", None).await?;
    assert_eq!(
        (status, error_code(&error)),
        (405, Some("method_not_allowed"))
    );

    let big = format!(
        r#"{{"id":"n2","display_name":"{}","zone":"local"}}"#,
        "x".repeat(1024)
    );
    let (status, error) = call(&socket, "POST", "/v1/nodes", Some(&big)).await?;
    assert_eq!((status, error_code(&error)), (413, Some("body_too_large")));

    let unknown_field = r#"{"id":"n2","display_name":"n","zone":"local","trusted":true}"#;
    let (status, error) = call(&socket, "POST", "/v1/nodes", Some(unknown_field)).await?;
    assert_eq!((status, error_code(&error)), (400, Some("invalid_json")));

    let bad_id = r#"{"id":"a:b","display_name":"n","zone":"local"}"#;
    let (status, error) = call(&socket, "POST", "/v1/nodes", Some(bad_id)).await?;
    assert_eq!((status, error_code(&error)), (400, Some("invalid_request")));

    let bad_zone = r#"{"id":"n2","display_name":"n","zone":"dmz"}"#;
    let (status, error) = call(&socket, "POST", "/v1/nodes", Some(bad_zone)).await?;
    assert_eq!((status, error_code(&error)), (422, Some("unknown_zone")));

    let (status, _) = call(&socket, "POST", "/v1/nodes", Some(REGISTER_N1)).await?;
    assert_eq!(status, 201);
    let (status, error) = call(&socket, "POST", "/v1/nodes", Some(REGISTER_N1)).await?;
    assert_eq!((status, error_code(&error)), (409, Some("node_exists")));

    let (status, error) = call(&socket, "GET", "/v1/nodes/ghost", None).await?;
    assert_eq!((status, error_code(&error)), (404, Some("node_not_found")));

    let (status, error) = call(
        &socket,
        "POST",
        "/v1/nodes/node-1:activate",
        Some(r#"{"force":true}"#),
    )
    .await?;
    assert_eq!((status, error_code(&error)), (400, Some("invalid_json")));
    daemon.stop().await
}

#[tokio::test]
async fn test_live_socket_is_not_replaced() -> TestResult {
    let mut daemon = Daemon::start(BTreeSet::from([own_uid()?]), None).await?;
    let second = bind_socket(&daemon.socket).await;
    assert!(
        matches!(second, Err(harw_netsec::NetsecError::SocketInUse { .. })),
        "a second bind must not steal a live socket"
    );
    daemon.stop().await
}
