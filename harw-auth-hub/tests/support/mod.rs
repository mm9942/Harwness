//! Shared helpers for the integration tests: `TestResult` (no
//! `unwrap`/`expect`/`panic`, Rust Coding Bible R087/R165/R182), a hub
//! running on a temp-dir socket, and a Hyper HTTP/1 client over
//! `tokio::net::UnixStream`.

use std::fmt;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use bytes::Bytes;
use harw_auth_hub::{HubConfig, HubError, HubListener, HubService};
use http::{Method, Request, StatusCode, header};
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1::SendRequest;
use hyper_util::rt::TokioIo;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

/// Error of a test; every failure is returned as `Err`.
pub enum TestError {
    /// An expected value was missing.
    Missing(&'static str),
    /// A result had an unexpected shape.
    Unexpected(String),
    /// A foreign error with context.
    Context {
        /// What was being attempted.
        context: &'static str,
        /// Rendered source error.
        source: String,
    },
}

/// Result of a test function or helper.
pub type TestResult<T = ()> = Result<T, TestError>;

impl fmt::Display for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(what) => write!(f, "expected value missing: {what}"),
            Self::Unexpected(message) => write!(f, "unexpected result: {message}"),
            Self::Context { context, source } => write!(f, "{context}: {source}"),
        }
    }
}

impl fmt::Debug for TestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for TestError {}

/// `map_err` adapter that attaches context to a foreign error.
pub fn ctx<E: fmt::Display>(context: &'static str) -> impl FnOnce(E) -> TestError {
    move |error| TestError::Context {
        context,
        source: error.to_string(),
    }
}

/// Fail with `message` unless `condition` holds.
pub fn ensure(condition: bool, message: &str) -> TestResult {
    if condition {
        Ok(())
    } else {
        Err(TestError::Unexpected(message.to_owned()))
    }
}

/// The effective uid of this test process, as the kernel will report it via
/// `SO_PEERCRED` (the owner of a directory we just created).
pub fn current_uid(dir: &Path) -> TestResult<u32> {
    Ok(fs::metadata(dir).map_err(ctx("stat temp dir"))?.uid())
}

/// Write `content` to `path` with `mode`.
pub fn write_file(path: &Path, content: &[u8], mode: u32) -> TestResult {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(ctx("create parent"))?;
    }
    let mut file = fs::File::create(path).map_err(ctx("create file"))?;
    file.write_all(content).map_err(ctx("write file"))?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(ctx("chmod"))?;
    Ok(())
}

/// A hub serving on `<tempdir>/run/secure.sock` (parent created by the hub).
pub struct RunningHub {
    /// Keeps the temp dir alive for the test's duration.
    _dir: TempDir,
    /// The socket path.
    pub socket: PathBuf,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<Result<(), HubError>>,
}

impl RunningHub {
    /// Start a hub. `config` receives the temp dir path and returns the TOML
    /// config text (without `socket_path`, which is set here).
    pub async fn start<F>(config: F) -> TestResult<Self>
    where
        F: FnOnce(&Path, u32) -> String,
    {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let uid = current_uid(dir.path())?;
        let socket = dir.path().join("run").join("secure.sock");
        let mut parsed =
            HubConfig::from_toml_str(&config(dir.path(), uid)).map_err(ctx("parse config"))?;
        parsed.socket_path = socket.clone();
        let bearer = parsed
            .load_bearer_tokens()
            .map_err(ctx("load bearer tokens"))?;
        let listener = HubListener::bind(&socket).map_err(ctx("bind"))?;
        let hub = HubService::new(&parsed, bearer);
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(harw_auth_hub::serve(listener, hub, async move {
            // Either an explicit stop or a dropped sender ends the server.
            let _ = stopped.await;
        }));
        Ok(Self {
            _dir: dir,
            socket,
            stop: Some(stop),
            task,
        })
    }

    /// Open a new HTTP/1 client connection to the hub.
    pub async fn client(&self) -> TestResult<Client> {
        let stream = UnixStream::connect(&self.socket)
            .await
            .map_err(ctx("connect"))?;
        let (sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(ctx("handshake"))?;
        tokio::spawn(connection);
        Ok(Client { sender })
    }

    /// Stop the hub, wait for the drain and check the socket file is gone.
    pub async fn stop(mut self) -> TestResult {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let task = self.task;
        task.await
            .map_err(ctx("join server task"))?
            .map_err(ctx("serve"))?;
        ensure(
            !self.socket.exists(),
            "socket file must be removed on shutdown",
        )
    }
}

/// One HTTP/1 client connection.
pub struct Client {
    sender: SendRequest<Full<Bytes>>,
}

impl Client {
    /// Send one request; returns status, content type and body. Every hub
    /// response must carry `Cache-Control: no-store`.
    pub async fn send(
        &mut self,
        method: Method,
        path: &str,
        body: Vec<u8>,
        bearer: Option<&str>,
    ) -> TestResult<(StatusCode, Bytes)> {
        let mut builder = Request::builder()
            .method(method)
            .uri(path)
            .header(header::HOST, "auth-hub.local");
        if let Some(token) = bearer {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let request = builder
            .body(Full::new(Bytes::from(body)))
            .map_err(ctx("build request"))?;
        self.sender.ready().await.map_err(ctx("client ready"))?;
        let response = self
            .sender
            .send_request(request)
            .await
            .map_err(ctx("send request"))?;
        let cache = response
            .headers()
            .get(header::CACHE_CONTROL)
            .ok_or(TestError::Missing("cache-control header"))?;
        ensure(cache == "no-store", "cache-control must be no-store")?;
        let status = response.status();
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(ctx("read body"))?
            .to_bytes();
        Ok((status, body))
    }

    /// `GET path` without a body.
    pub async fn get(&mut self, path: &str) -> TestResult<(StatusCode, Bytes)> {
        self.send(Method::GET, path, Vec::new(), None).await
    }
}
