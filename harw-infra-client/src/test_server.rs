//! A tiny in-test hub: hyper HTTP/1 on a tempdir Unix socket.
//!
//! It speaks just enough of the AuthHub surface to exercise the client:
//!
//! | Request | Answer |
//! |---|---|
//! | `GET /v1/health`, `/v1/version`, `/v1/capabilities` | JSON documents |
//! | `POST /v1/keys` (CGK1 ns,id,profile) | `KeyCreated` v1, public = `pk:<profile>` |
//! | `GET /v1/keys/{ns}/{id}[@v]` | `Metadata` (v or 1, enabled, `pq-hpke-default`) |
//! | `GET …/public` | octets `pk:<id>` |
//! | `POST …:rotate` (empty body) | `KeyCreated` v+1 |
//! | `POST …:wrap` (CGK1 info,aad,material) | octets `W` + reversed material |
//! | `POST …:unwrap` (CGK1 info,aad,wrapped) | the inverse of wrap |
//! | `POST …:rewrap` (CGK1, 8 fields) | octets `R:<to_ns>/<to_id>@<to_v>:` + wrapped |
//!
//! Special namespaces: `status/<code>` answers `<code>`; `slow/*` never
//! answers in time; `garbage/*` answers a malformed CGK1 body; `big/*`
//! answers 64 KiB of octets; `private/*` requires the bearer token.
//! Any request that carries an `Authorization` header other than
//! `Bearer test-token` is answered `401`.

use std::convert::Infallible;
use std::path::PathBuf;
use std::time::Duration;

use bytes::Bytes;
use crypt_guard_hyper::codec::{CONTENT_TYPE as FRAME, CodecError, FrameReader, FrameWriter};
use http::header::{AUTHORIZATION, CONTENT_TYPE};
use http::{HeaderValue, Method, Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use tokio::net::UnixListener;
use tokio::task::JoinHandle;

use crate::test_support::TestResult;

/// The token the mock hub accepts.
pub(crate) const TEST_TOKEN: &[u8] = b"test-token";

/// A running mock hub; stops accepting when dropped.
pub(crate) struct MockHub {
    _dir: tempfile::TempDir,
    pub(crate) socket: PathBuf,
    task: JoinHandle<()>,
}

impl MockHub {
    pub(crate) async fn start() -> TestResult<Self> {
        let dir = tempfile::tempdir()?;
        let socket = dir.path().join("hub.sock");
        let listener = UnixListener::bind(&socket)?;
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let _ = hyper::server::conn::http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service_fn(handle))
                        .await;
                });
            }
        });
        Ok(Self {
            _dir: dir,
            socket,
            task,
        })
    }

    /// A socket path in the same tempdir where nothing listens.
    pub(crate) fn missing_socket(&self) -> PathBuf {
        self.socket.with_file_name("missing.sock")
    }
}

impl Drop for MockHub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle(request: Request<Incoming>) -> Result<Response<Full<Bytes>>, Infallible> {
    let (parts, body) = request.into_parts();
    let Ok(collected) = body.collect().await else {
        return Ok(status(StatusCode::BAD_REQUEST));
    };
    let body = collected.to_bytes();
    let authorization = parts.headers.get(AUTHORIZATION).map(HeaderValue::as_bytes);
    if let Some(value) = authorization {
        let expected = [b"Bearer ".as_slice(), TEST_TOKEN].concat();
        if value != expected.as_slice() {
            return Ok(status(StatusCode::UNAUTHORIZED));
        }
    }
    let is_frame = parts
        .headers
        .get(CONTENT_TYPE)
        .is_some_and(|value| value.as_bytes() == FRAME.as_bytes());
    let request = Req {
        method: parts.method,
        path: parts.uri.path().to_owned(),
        authorized: authorization.is_some(),
        is_frame,
        body,
    };
    Ok(route(request).await)
}

struct Req {
    method: Method,
    path: String,
    authorized: bool,
    is_frame: bool,
    body: Bytes,
}

async fn route(req: Req) -> Response<Full<Bytes>> {
    match (&req.method, req.path.as_str()) {
        (&Method::GET, "/v1/health") => json(r#"{"status":"ok","service":"mock-hub"}"#),
        (&Method::GET, "/v1/version") => {
            json(r#"{"service":"mock-hub","version":"0.3.0","protocol":1}"#)
        }
        (&Method::GET, "/v1/capabilities") => json(
            r#"{"service":"mock-hub","protocol":1,"crypto_profiles":["harw-strong-v1"],
                "operations":["key.wrap","key.unwrap","key.rotate"],"future_field":{"x":1}}"#,
        ),
        (&Method::POST, "/v1/keys") => generate(&req),
        (_, path) => match path.strip_prefix("/v1/keys/") {
            Some(rest) => key_route(&req, rest.to_owned()).await,
            None => status(StatusCode::NOT_FOUND),
        },
    }
}

fn generate(req: &Req) -> Response<Full<Bytes>> {
    if !req.is_frame {
        return status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    let Some((namespace, id, profile)) = read_generate(&req.body) else {
        return status(StatusCode::BAD_REQUEST);
    };
    key_created(&namespace, &id, 1, format!("pk:{profile}").as_bytes())
}

fn read_generate(body: &[u8]) -> Option<(String, String, String)> {
    let mut reader = FrameReader::new(body).ok()?;
    let namespace = reader.text().ok()?.to_owned();
    let id = reader.text().ok()?.to_owned();
    let profile = reader.text().ok()?.to_owned();
    reader.finish().ok()?;
    Some((namespace, id, profile))
}

async fn key_route(req: &Req, rest: String) -> Response<Full<Bytes>> {
    let mut segments = rest.split('/');
    let (Some(namespace), Some(key_segment)) = (segments.next(), segments.next()) else {
        return status(StatusCode::NOT_FOUND);
    };
    let tail = segments.next();
    let (key_part, verb) = match key_segment.split_once(':') {
        Some((key, verb)) => (key, Some(verb)),
        None => (key_segment, None),
    };
    let (id, version) = match key_part.split_once('@') {
        Some((id, version)) => (id, version.parse::<u32>().unwrap_or(0)),
        None => (key_part, 0),
    };

    match namespace {
        "status" => {
            return status(
                id.parse::<u16>()
                    .ok()
                    .and_then(|code| StatusCode::from_u16(code).ok())
                    .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            );
        }
        "slow" => {
            tokio::time::sleep(Duration::from_secs(30)).await;
            return status(StatusCode::OK);
        }
        "garbage" => return typed(FRAME, b"CGK1\x01\xff".to_vec()),
        "big" => return typed("application/octet-stream", vec![0x42; 64 * 1024]),
        "private" if !req.authorized => return status(StatusCode::UNAUTHORIZED),
        _ => {}
    }

    match (&req.method, verb, tail) {
        (&Method::GET, None, None) => metadata(namespace, id, version.max(1)),
        (&Method::GET, None, Some("public")) => {
            typed("application/octet-stream", format!("pk:{id}").into_bytes())
        }
        (&Method::POST, Some("rotate"), None) if req.body.is_empty() => {
            key_created(namespace, id, version.max(1) + 1, b"")
        }
        (&Method::POST, Some(verb), None) if req.is_frame => crypt_op(verb, &req.body),
        (&Method::POST, Some(_), None) => status(StatusCode::UNSUPPORTED_MEDIA_TYPE),
        _ => status(StatusCode::NOT_FOUND),
    }
}

fn crypt_op(verb: &str, body: &[u8]) -> Response<Full<Bytes>> {
    let Ok(mut reader) = FrameReader::new(body) else {
        return status(StatusCode::BAD_REQUEST);
    };
    let answer = match verb {
        "wrap" => fake_wrap(&mut reader),
        "unwrap" => fake_unwrap(&mut reader),
        "rewrap" => fake_rewrap(&mut reader),
        _ => return status(StatusCode::NOT_FOUND),
    };
    match answer {
        Some(bytes) => typed("application/octet-stream", bytes),
        None => status(StatusCode::UNPROCESSABLE_ENTITY),
    }
}

/// `info` `aad` `material` → `W` + reversed material.
fn fake_wrap(reader: &mut FrameReader<'_>) -> Option<Vec<u8>> {
    let _info = reader.field().ok()?;
    let _aad = reader.field().ok()?;
    let material = reader.field().ok()?;
    let mut out = vec![b'W'];
    out.extend(material.iter().rev());
    Some(out)
}

/// Inverse of [`fake_wrap`].
fn fake_unwrap(reader: &mut FrameReader<'_>) -> Option<Vec<u8>> {
    let _info = reader.field().ok()?;
    let _aad = reader.field().ok()?;
    let wrapped = reader.field().ok()?;
    let inner = wrapped.strip_prefix(b"W")?;
    Some(inner.iter().rev().copied().collect())
}

/// 8-field rewrap frame → `R:<to_ns>/<to_id>@<to_v>:` + wrapped.
fn fake_rewrap(reader: &mut FrameReader<'_>) -> Option<Vec<u8>> {
    let _from_info = reader.field().ok()?;
    let _from_aad = reader.field().ok()?;
    let to_namespace = reader.text().ok()?;
    let to_id = reader.text().ok()?;
    let to_version = reader.u32().ok()?;
    let _to_info = reader.field().ok()?;
    let _to_aad = reader.field().ok()?;
    let wrapped = reader.field().ok()?;
    let mut out = format!("R:{to_namespace}/{to_id}@{to_version}:").into_bytes();
    out.extend_from_slice(wrapped);
    Some(out)
}

fn key_created(namespace: &str, id: &str, version: u32, public: &[u8]) -> Response<Full<Bytes>> {
    frame_or_500(encode_key_created(namespace, id, version, public))
}

fn encode_key_created(
    namespace: &str,
    id: &str,
    version: u32,
    public: &[u8],
) -> Result<Vec<u8>, CodecError> {
    let mut writer = FrameWriter::new();
    writer.field(namespace.as_bytes())?;
    writer.field(id.as_bytes())?;
    writer.u32(version);
    writer.field(public)?;
    Ok(writer.finish())
}

fn metadata(namespace: &str, id: &str, version: u32) -> Response<Full<Bytes>> {
    frame_or_500(encode_metadata(namespace, id, version))
}

fn encode_metadata(namespace: &str, id: &str, version: u32) -> Result<Vec<u8>, CodecError> {
    let mut writer = FrameWriter::new();
    writer.field(namespace.as_bytes())?;
    writer.field(id.as_bytes())?;
    writer.u32(version);
    writer.u8(1);
    writer.field(b"pq-hpke-default")?;
    Ok(writer.finish())
}

fn frame_or_500(encoded: Result<Vec<u8>, CodecError>) -> Response<Full<Bytes>> {
    match encoded {
        Ok(bytes) => typed(FRAME, bytes),
        Err(_) => status(StatusCode::INTERNAL_SERVER_ERROR),
    }
}

fn json(body: &'static str) -> Response<Full<Bytes>> {
    typed("application/json", body.as_bytes().to_vec())
}

fn typed(content_type: &'static str, body: Vec<u8>) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from(body)));
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

fn status(code: StatusCode) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(Bytes::from_static(b"error")));
    *response.status_mut() = code;
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}
