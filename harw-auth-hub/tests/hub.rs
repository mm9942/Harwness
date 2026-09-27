//! End-to-end tests of the Auth/Crypto Hub over a real `AF_UNIX` socket in
//! a temp dir: meta routes, a full generate → public key → wrap → unwrap →
//! rotate flow authenticated by `SO_PEERCRED`, rejection of unknown uids,
//! the bearer-token fallback, policy enforcement and socket-file handling.

mod support;

use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::time::Duration;

use crypt_guard_hyper::codec::{FrameReader, FrameWriter};
use harw_auth_hub::{HubError, HubListener};
use harw_dod_encrypt::{SignPurpose, SignTranscript};
use http::{Method, StatusCode};
use support::{RunningHub, SealedHub, TestError, TestResult, ctx, ensure, write_file};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::time::timeout;

const TOKEN: &str = "ops-token-0123456789abcdef0123456789";

/// Config granting the test process's own uid everything in `app`.
fn own_uid_config(_dir: &std::path::Path, uid: u32) -> String {
    format!(
        "[[peers]]\nuid = {uid}\nprincipal = \"harw-web\"\n\
         grants = [ {{ namespace = \"app\", ops = [\"all\"] }} ]\n"
    )
}

/// Config that lists only a uid that is *not* the test process's.
fn foreign_uid_config(_dir: &std::path::Path, uid: u32) -> String {
    let other = uid.wrapping_add(1);
    format!(
        "[[peers]]\nuid = {other}\nprincipal = \"someone-else\"\n\
         grants = [ {{ namespace = \"app\", ops = [\"all\"] }} ]\n"
    )
}

fn generate_body(namespace: &str, id: &str, profile: &str) -> TestResult<Vec<u8>> {
    let mut writer = FrameWriter::new();
    writer
        .field(namespace.as_bytes())
        .map_err(ctx("frame namespace"))?;
    writer.field(id.as_bytes()).map_err(ctx("frame id"))?;
    writer
        .field(profile.as_bytes())
        .map_err(ctx("frame profile"))?;
    Ok(writer.finish())
}

/// `wrap`/`unwrap` layout: `info`, `aad`, payload.
fn crypt_body(info: &[u8], aad: &[u8], payload: &[u8]) -> TestResult<Vec<u8>> {
    let mut writer = FrameWriter::new();
    writer.field(info).map_err(ctx("frame info"))?;
    writer.field(aad).map_err(ctx("frame aad"))?;
    writer.field(payload).map_err(ctx("frame payload"))?;
    Ok(writer.finish())
}

/// `sign`/`verify` layout: a single `message` field.
fn sign_body(message: &[u8]) -> TestResult<Vec<u8>> {
    let mut writer = FrameWriter::new();
    writer.field(message).map_err(ctx("frame message"))?;
    Ok(writer.finish())
}

struct KeyCreated {
    namespace: String,
    id: String,
    version: u32,
    public: Vec<u8>,
}

fn parse_key_created(bytes: &[u8]) -> TestResult<KeyCreated> {
    let mut reader = FrameReader::new(bytes).map_err(ctx("frame"))?;
    let namespace = reader.text().map_err(ctx("namespace"))?.to_owned();
    let id = reader.text().map_err(ctx("id"))?.to_owned();
    let version = reader.u32().map_err(ctx("version"))?;
    let public = reader.field().map_err(ctx("public"))?.to_vec();
    reader.finish().map_err(ctx("trailing bytes"))?;
    Ok(KeyCreated {
        namespace,
        id,
        version,
        public,
    })
}

fn json(bytes: &[u8]) -> TestResult<serde_json::Value> {
    serde_json::from_slice(bytes).map_err(ctx("json"))
}

#[tokio::test]
async fn meta_routes_answer_and_socket_is_0660() -> TestResult {
    let hub = RunningHub::start(own_uid_config).await?;

    let mode = fs::metadata(&hub.socket)
        .map_err(ctx("stat socket"))?
        .permissions()
        .mode();
    ensure(mode & 0o777 == 0o660, "socket mode must be 0660")?;
    let parent = hub.socket.parent().ok_or(TestError::Missing("parent"))?;
    let parent_mode = fs::metadata(parent)
        .map_err(ctx("stat parent"))?
        .permissions()
        .mode();
    ensure(
        parent_mode & 0o027 == 0,
        "created parent dir must not be group-writable or world-accessible",
    )?;

    let mut client = hub.client().await?;

    // The §38 contract `harw-infra-client` (`src/info.rs`) deserializes.
    let (status, body) = client.get("/v1/health").await?;
    ensure(status == StatusCode::OK, "health status")?;
    let health = json(&body)?;
    ensure(health["status"] == "ok", "health.status")?;
    ensure(health["service"] == "harw-auth-hub", "health.service")?;

    let (status, body) = client.get("/v1/version").await?;
    ensure(status == StatusCode::OK, "version status")?;
    let version = json(&body)?;
    ensure(version["service"] == "harw-auth-hub", "version.service")?;
    ensure(
        version["version"] == env!("CARGO_PKG_VERSION"),
        "version.version",
    )?;
    ensure(version["protocol"] == 1, "version.protocol")?;
    ensure(version["cryptguard"].is_string(), "version.cryptguard")?;

    let (status, body) = client.get("/v1/capabilities").await?;
    ensure(status == StatusCode::OK, "capabilities status")?;
    let caps = json(&body)?;
    ensure(caps["service"] == "harw-auth-hub", "capabilities.service")?;
    ensure(
        caps["persistence"] == "in-memory",
        "capabilities.persistence",
    )?;
    ensure(caps["protocol"] == 1, "capabilities.protocol")?;
    // The wire names of `harw_infra_client::KeyProfile`.
    let profiles = caps["crypto_profiles"]
        .as_array()
        .ok_or(TestError::Missing("crypto_profiles"))?;
    for name in ["pq-hpke-default", "ml-dsa-44", "ml-dsa-65", "ml-dsa-87"] {
        ensure(
            profiles.iter().any(|v| v == name),
            "capabilities.crypto_profiles",
        )?;
    }
    let ops = caps["operations"]
        .as_array()
        .ok_or(TestError::Missing("operations"))?;
    for op in ["generate", "public_key", "wrap", "unwrap", "rotate", "sign"] {
        ensure(ops.iter().any(|v| v == op), "capabilities.operations")?;
    }
    let algorithms = caps["algorithms"]
        .as_array()
        .ok_or(TestError::Missing("algorithms"))?;
    ensure(
        algorithms.iter().any(|v| v == "pq-hpke-default"),
        "capabilities.algorithms",
    )?;
    let auth = caps["authentication"]
        .as_array()
        .ok_or(TestError::Missing("authentication"))?;
    ensure(
        auth.iter().all(|v| v != "bearer"),
        "no bearer without tokens",
    )?;

    let (status, _) = client
        .send(Method::POST, "/v1/health", Vec::new(), None)
        .await?;
    ensure(
        status == StatusCode::METHOD_NOT_ALLOWED,
        "POST health → 405",
    )?;

    drop(client);
    hub.stop().await
}

#[tokio::test]
async fn generate_public_wrap_unwrap_rotate_over_unix_socket() -> TestResult {
    let hub = RunningHub::start(own_uid_config).await?;
    let mut client = hub.client().await?;

    // generate → KeyCreated v1 with a public key.
    let (status, body) = client
        .send(
            Method::POST,
            "/v1/keys",
            generate_body("app", "k1", "pq-hpke-default")?,
            None,
        )
        .await?;
    ensure(status == StatusCode::OK, "generate status")?;
    let created = parse_key_created(&body)?;
    ensure(created.namespace == "app", "created namespace")?;
    ensure(created.id == "k1", "created id")?;
    ensure(created.version == 1, "created version")?;
    ensure(!created.public.is_empty(), "public key present")?;

    // public key over HTTP equals the one returned by generate.
    let (status, body) = client.get("/v1/keys/app/k1/public").await?;
    ensure(status == StatusCode::OK, "public key status")?;
    ensure(
        body.as_ref() == created.public.as_slice(),
        "public key matches",
    )?;

    // wrap → unwrap round trip.
    let material = b"data-encryption-key-0123456789ab";
    let (status, wrapped) = client
        .send(
            Method::POST,
            "/v1/keys/app/k1:wrap",
            crypt_body(b"dek", b"tenant-a", material)?,
            None,
        )
        .await?;
    ensure(status == StatusCode::OK, "wrap status")?;
    ensure(
        !wrapped.windows(material.len()).any(|w| w == material),
        "wrapped blob must not contain the material",
    )?;

    let (status, unwrapped) = client
        .send(
            Method::POST,
            "/v1/keys/app/k1:unwrap",
            crypt_body(b"dek", b"tenant-a", &wrapped)?,
            None,
        )
        .await?;
    ensure(status == StatusCode::OK, "unwrap status")?;
    ensure(unwrapped.as_ref() == material, "unwrap recovers material")?;

    // wrong aad → opaque 422.
    let (status, _) = client
        .send(
            Method::POST,
            "/v1/keys/app/k1:unwrap",
            crypt_body(b"dek", b"tenant-b", &wrapped)?,
            None,
        )
        .await?;
    ensure(
        status == StatusCode::UNPROCESSABLE_ENTITY,
        "wrong aad → 422",
    )?;

    // rotate → v2; the v1-wrapped blob still unwraps.
    let (status, body) = client
        .send(Method::POST, "/v1/keys/app/k1:rotate", Vec::new(), None)
        .await?;
    ensure(status == StatusCode::OK, "rotate status")?;
    let rotated = parse_key_created(&body)?;
    ensure(rotated.version == 2, "rotated version")?;
    ensure(
        rotated.public != created.public,
        "rotation yields a new key",
    )?;

    let (status, unwrapped) = client
        .send(
            Method::POST,
            "/v1/keys/app/k1:unwrap",
            crypt_body(b"dek", b"tenant-a", &wrapped)?,
            None,
        )
        .await?;
    ensure(status == StatusCode::OK, "unwrap after rotate status")?;
    ensure(unwrapped.as_ref() == material, "unwrap after rotate")?;

    drop(client);
    hub.stop().await
}

#[tokio::test]
async fn unknown_uid_is_rejected_with_401() -> TestResult {
    let hub = RunningHub::start(foreign_uid_config).await?;
    let mut client = hub.client().await?;

    let (status, _) = client
        .send(
            Method::POST,
            "/v1/keys",
            generate_body("app", "k1", "pq-hpke-default")?,
            None,
        )
        .await?;
    ensure(status == StatusCode::UNAUTHORIZED, "unknown uid → 401")?;

    // Without a bearer fallback a token does not help either.
    let (status, _) = client
        .send(
            Method::POST,
            "/v1/keys",
            generate_body("app", "k1", "pq-hpke-default")?,
            Some(TOKEN),
        )
        .await?;
    ensure(
        status == StatusCode::UNAUTHORIZED,
        "token without fallback → 401",
    )?;

    // Meta routes stay readable for anyone allowed to connect.
    let (status, _) = client.get("/v1/health").await?;
    ensure(status == StatusCode::OK, "health for unknown uid")?;

    drop(client);
    hub.stop().await
}

#[tokio::test]
async fn bearer_fallback_authenticates_unknown_uid() -> TestResult {
    let hub = RunningHub::start(|dir, uid| {
        let token_file = dir.join("tokens").join("ops.token");
        // A failure here surfaces as a token-file error in `start`.
        let _ = write_file(&token_file, format!("{TOKEN}\n").as_bytes(), 0o600);
        format!(
            "{}\n[[bearer_tokens]]\nprincipal = \"ops\"\ntoken_file = \"{}\"\n\
             grants = [ {{ namespace = \"app\", ops = [\"admin\", \"read-public\"] }} ]\n",
            foreign_uid_config(dir, uid),
            token_file.display()
        )
    })
    .await?;
    let mut client = hub.client().await?;

    let (status, _) = client
        .send(
            Method::POST,
            "/v1/keys",
            generate_body("app", "k1", "pq-hpke-default")?,
            Some(TOKEN),
        )
        .await?;
    ensure(status == StatusCode::OK, "valid bearer → 200")?;

    let (status, _) = client
        .send(
            Method::POST,
            "/v1/keys",
            generate_body("app", "k2", "pq-hpke-default")?,
            Some("wrong-token-wrong-token-wrong"),
        )
        .await?;
    ensure(status == StatusCode::UNAUTHORIZED, "wrong bearer → 401")?;

    let (status, _) = client
        .send(
            Method::POST,
            "/v1/keys",
            generate_body("app", "k3", "pq-hpke-default")?,
            None,
        )
        .await?;
    ensure(status == StatusCode::UNAUTHORIZED, "no bearer → 401")?;

    let (status, body) = client.get("/v1/capabilities").await?;
    ensure(status == StatusCode::OK, "capabilities")?;
    let caps = json(&body)?;
    let auth = caps["authentication"]
        .as_array()
        .ok_or(TestError::Missing("authentication"))?;
    ensure(auth.iter().any(|v| v == "bearer"), "bearer advertised")?;

    drop(client);
    hub.stop().await
}

#[tokio::test]
async fn policy_denies_ops_outside_grants() -> TestResult {
    let hub = RunningHub::start(|_dir, uid| {
        format!(
            "[[peers]]\nuid = {uid}\nprincipal = \"harw-web\"\n\
             grants = [\n\
               {{ namespace = \"own\", ops = [\"admin\", \"encrypt\", \"read-public\"] }},\n\
               {{ namespace = \"shared\", ops = [\"read-public\"] }},\n\
             ]\n"
        )
    })
    .await?;
    let mut client = hub.client().await?;

    // Allowed: generate + wrap in `own`.
    let (status, _) = client
        .send(
            Method::POST,
            "/v1/keys",
            generate_body("own", "k1", "pq-hpke-default")?,
            None,
        )
        .await?;
    ensure(status == StatusCode::OK, "generate in own")?;
    let (status, wrapped) = client
        .send(
            Method::POST,
            "/v1/keys/own/k1:wrap",
            crypt_body(b"i", b"a", b"material-material-material")?,
            None,
        )
        .await?;
    ensure(status == StatusCode::OK, "wrap in own")?;

    // Denied (no secret-egress): unwrap is hidden as 404.
    let (status, _) = client
        .send(
            Method::POST,
            "/v1/keys/own/k1:unwrap",
            crypt_body(b"i", b"a", &wrapped)?,
            None,
        )
        .await?;
    ensure(
        status == StatusCode::NOT_FOUND,
        "unwrap without grant → 404",
    )?;

    // Denied (no admin in `shared`): generate hidden as 404.
    let (status, _) = client
        .send(
            Method::POST,
            "/v1/keys",
            generate_body("shared", "k1", "pq-hpke-default")?,
            None,
        )
        .await?;
    ensure(status == StatusCode::NOT_FOUND, "generate in shared → 404")?;

    drop(client);
    hub.stop().await
}

// `HubListener::bind` registers with the Tokio reactor, hence a runtime.
#[tokio::test]
async fn stale_socket_is_replaced_live_socket_and_files_are_kept() -> TestResult {
    let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;

    // Stale: a socket file nobody accepts on any more.
    let stale = dir.path().join("stale.sock");
    drop(std::os::unix::net::UnixListener::bind(&stale).map_err(ctx("bind stale"))?);
    ensure(stale.exists(), "stale socket file left behind")?;
    let listener = HubListener::bind(&stale).map_err(ctx("rebind stale"))?;
    let file_type = fs::symlink_metadata(&stale)
        .map_err(ctx("stat rebound"))?
        .file_type();
    ensure(file_type.is_socket(), "rebound path is a socket")?;
    drop(listener);

    // Live: another listener still accepts on the path.
    let live = dir.path().join("live.sock");
    let _other = std::os::unix::net::UnixListener::bind(&live).map_err(ctx("bind live"))?;
    match HubListener::bind(&live) {
        Err(HubError::SocketInUse(_)) => {}
        other => return Err(TestError::Unexpected(format!("live socket: {other:?}"))),
    }

    // Not a socket: never removed.
    let regular = dir.path().join("regular.sock");
    write_file(&regular, b"do not delete", 0o600)?;
    match HubListener::bind(&regular) {
        Err(HubError::NotASocket(_)) => {}
        other => return Err(TestError::Unexpected(format!("regular file: {other:?}"))),
    }
    let kept = fs::read(&regular).map_err(ctx("read regular"))?;
    ensure(kept == b"do not delete", "regular file untouched")
}

/// Config granting the test process's own uid everything in `app`, for a
/// [`SealedHub`] (whose config closure also receives the store path, unused
/// here: the store is already opened by the caller before the config text
/// is even parsed).
fn sealed_own_uid_config(
    _dir: &std::path::Path,
    _store_path: &std::path::Path,
    uid: u32,
) -> String {
    format!(
        "[[peers]]\nuid = {uid}\nprincipal = \"harw-web\"\n\
         grants = [ {{ namespace = \"app\", ops = [\"all\"] }} ]\n"
    )
}

#[tokio::test]
async fn sealed_hub_keeps_keys_and_epoch_across_restart() -> TestResult {
    let hub = SealedHub::start(sealed_own_uid_config).await?;
    let mut client = hub.client().await?;

    let (status, body) = client
        .send(
            Method::POST,
            "/v1/keys",
            generate_body("app", "k1", "pq-hpke-default")?,
            None,
        )
        .await?;
    ensure(status == StatusCode::OK, "generate status")?;
    parse_key_created(&body)?;

    let material = b"data-encryption-key-0123456789ab";
    let (status, wrapped) = client
        .send(
            Method::POST,
            "/v1/keys/app/k1:wrap",
            crypt_body(b"dek", b"tenant-a", material)?,
            None,
        )
        .await?;
    ensure(status == StatusCode::OK, "wrap status")?;

    let (status, body) = client.get("/v1/capabilities").await?;
    ensure(status == StatusCode::OK, "capabilities status")?;
    let caps = json(&body)?;
    ensure(
        caps["persistence"] == "sealed-file",
        "capabilities.persistence before restart",
    )?;
    let epoch_before = caps["store_epoch"]
        .as_str()
        .ok_or(TestError::Missing("store_epoch"))?
        .to_owned();

    drop(client);
    let hub = hub.restart(sealed_own_uid_config).await?;
    let mut client = hub.client().await?;

    let (status, body) = client.get("/v1/capabilities").await?;
    ensure(
        status == StatusCode::OK,
        "capabilities status after restart",
    )?;
    let caps = json(&body)?;
    ensure(
        caps["persistence"] == "sealed-file",
        "capabilities.persistence after restart",
    )?;
    let epoch_after = caps["store_epoch"]
        .as_str()
        .ok_or(TestError::Missing("store_epoch"))?
        .to_owned();
    ensure(
        epoch_before == epoch_after,
        "store_epoch survives a restart",
    )?;

    let (status, unwrapped) = client
        .send(
            Method::POST,
            "/v1/keys/app/k1:unwrap",
            crypt_body(b"dek", b"tenant-a", &wrapped)?,
            None,
        )
        .await?;
    ensure(status == StatusCode::OK, "unwrap after restart status")?;
    ensure(
        unwrapped.as_ref() == material,
        "unwrap after restart recovers material wrapped before the restart",
    )?;

    drop(client);
    hub.stop().await
}

#[tokio::test]
async fn in_memory_hub_reports_in_memory_persistence() -> TestResult {
    let hub = RunningHub::start(own_uid_config).await?;
    let mut client = hub.client().await?;

    let (status, body) = client.get("/v1/capabilities").await?;
    ensure(status == StatusCode::OK, "capabilities status")?;
    let caps = json(&body)?;
    ensure(
        caps["persistence"] == "in-memory",
        "capabilities.persistence",
    )?;
    ensure(caps["store_epoch"].is_string(), "capabilities.store_epoch")?;

    let (status, body) = client.get("/v1/version").await?;
    ensure(status == StatusCode::OK, "version status")?;
    let version = json(&body)?;
    ensure(version["store_epoch"].is_string(), "version.store_epoch")?;

    drop(client);
    hub.stop().await
}

#[tokio::test]
async fn harw_sign_rejects_a_non_transcript_message() -> TestResult {
    let hub = RunningHub::start(|_dir, uid| {
        format!(
            "[[peers]]\nuid = {uid}\nprincipal = \"harw-web\"\n\
             grants = [ {{ namespace = \"harw.artifact-signing\", ops = [\"admin\", \"sign\"] }} ]\n"
        )
    })
    .await?;
    let mut client = hub.client().await?;

    // Generate an artifact-signing key (ML-DSA-87 is the default profile
    // for `ArtifactSigning`, see `HarwCryptoProfile::default_for`).
    let (status, body) = client
        .send(
            Method::POST,
            "/v1/keys",
            generate_body("harw.artifact-signing", "node1", "ml-dsa-87")?,
            None,
        )
        .await?;
    ensure(status == StatusCode::OK, "generate artifact-signing key")?;
    parse_key_created(&body)?;

    // A raw, non-transcript message is refused by `HarwUsageAuthorizer`'s
    // transcript-binding check: `hide_forbidden_keys` may report it as an
    // opaque 404 rather than 403.
    let (status, _) = client
        .send(
            Method::POST,
            "/v1/keys/harw.artifact-signing/node1:sign",
            sign_body(b"just some raw bytes, not a canonical transcript")?,
            None,
        )
        .await?;
    ensure(
        status == StatusCode::FORBIDDEN || status == StatusCode::NOT_FOUND,
        "raw message → 403 or 404",
    )?;

    // A canonical transcript of the key's own sign purpose is accepted.
    let transcript = SignTranscript::builder(SignPurpose::ArtifactManifest)
        .field("manifest", b"artifact-manifest-digest-bytes")
        .build_for_kms()
        .map_err(ctx("build transcript"))?;
    let (status, signature) = client
        .send(
            Method::POST,
            "/v1/keys/harw.artifact-signing/node1:sign",
            sign_body(transcript.as_bytes())?,
            None,
        )
        .await?;
    ensure(status == StatusCode::OK, "valid transcript → 200")?;
    ensure(!signature.is_empty(), "signature present")?;

    drop(client);
    hub.stop().await
}

#[tokio::test]
async fn stalled_header_is_disconnected() -> TestResult {
    let hub = RunningHub::start(own_uid_config).await?;

    let mut stream = UnixStream::connect(&hub.socket)
        .await
        .map_err(ctx("connect"))?;
    stream
        .write_all(b"GET / HTTP/1.1\r\n")
        .await
        .map_err(ctx("write partial header"))?;

    // The server must close the connection within
    // `HEADER_READ_TIMEOUT` (10s); give it a margin up to 15s total.
    let mut buf = [0u8; 1];
    let read = timeout(Duration::from_secs(15), stream.read(&mut buf))
        .await
        .map_err(|_| TestError::Unexpected("connection was not closed within 15s".to_owned()))?
        .map_err(ctx("read after stall"))?;
    ensure(
        read == 0,
        "stalled connection must be closed (EOF), not answered",
    )?;

    hub.stop().await
}
