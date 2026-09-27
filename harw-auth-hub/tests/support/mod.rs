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
use harw_auth_hub::sealed::{Kek, SealedProvider};
use harw_auth_hub::service::KeyStore;
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
    /// Keeps the temp dir alive for the test's duration. `None` when the
    /// temp dir is owned elsewhere (see `SealedHub`, which must outlive a
    /// restart).
    _dir: Option<TempDir>,
    /// The socket path.
    pub socket: PathBuf,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<Result<(), HubError>>,
}

impl RunningHub {
    /// Parse `config_text`, bind the socket and spawn the server task on
    /// `store`. Shared by `RunningHub::start` and `SealedHub`.
    async fn spawn(
        socket: PathBuf,
        config_text: &str,
        store: KeyStore,
    ) -> TestResult<(oneshot::Sender<()>, JoinHandle<Result<(), HubError>>)> {
        let mut parsed = HubConfig::from_toml_str(config_text).map_err(ctx("parse config"))?;
        parsed.socket_path = socket.clone();
        let bearer = parsed
            .load_bearer_tokens()
            .map_err(ctx("load bearer tokens"))?;
        let listener = HubListener::bind(&socket).map_err(ctx("bind"))?;
        let hub = HubService::build(&parsed, bearer, store).map_err(ctx("build hub"))?;
        let (stop, stopped) = oneshot::channel::<()>();
        let task = tokio::spawn(harw_auth_hub::serve(listener, hub, async move {
            // Either an explicit stop or a dropped sender ends the server.
            let _ = stopped.await;
        }));
        Ok((stop, task))
    }

    /// Start a hub with an in-memory key store. `config` receives the temp
    /// dir path and returns the TOML config text (without `socket_path`,
    /// which is set here).
    pub async fn start<F>(config: F) -> TestResult<Self>
    where
        F: FnOnce(&Path, u32) -> String,
    {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let uid = current_uid(dir.path())?;
        let socket = dir.path().join("run").join("secure.sock");
        let text = config(dir.path(), uid);
        let (stop, task) = Self::spawn(socket.clone(), &text, KeyStore::InMemory).await?;
        Ok(Self {
            _dir: Some(dir),
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

/// A hub backed by a sealed on-disk key store. Unlike `RunningHub`, the
/// temp dir, store path and KEK are kept alive here (not inside the inner
/// `RunningHub`) so the same store can be reopened by `restart` after the
/// hub has been stopped, to test persistence across a restart.
pub struct SealedHub {
    /// The currently running hub.
    pub hub: RunningHub,
    /// Keeps the temp dir (and thus the store file) alive across restarts.
    dir: TempDir,
    /// Path of the sealed store, inside `dir`.
    pub store_path: PathBuf,
    kek: Kek,
}

impl SealedHub {
    /// Start a hub over a freshly created sealed store, in a new temp dir,
    /// under a freshly generated KEK. `config` receives the temp dir path,
    /// the store path and the uid, and returns the TOML config text
    /// (without `socket_path`, which is set here).
    pub async fn start<F>(config: F) -> TestResult<Self>
    where
        F: FnOnce(&Path, &Path, u32) -> String,
    {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let uid = current_uid(dir.path())?;
        let store_path = dir.path().join("keys.store");
        let kek = Kek::generate().map_err(ctx("generate kek"))?;
        let provider =
            SealedProvider::create(&store_path, &kek).map_err(ctx("create sealed store"))?;
        let socket = dir.path().join("run").join("secure.sock");
        let text = config(dir.path(), &store_path, uid);
        let (stop, task) =
            RunningHub::spawn(socket.clone(), &text, KeyStore::Sealed(provider)).await?;
        Ok(Self {
            hub: RunningHub {
                _dir: None,
                socket,
                stop: Some(stop),
                task,
            },
            dir,
            store_path,
            kek,
        })
    }

    /// Stop the hub and start it again by reopening the same store under
    /// the same KEK (rather than recreating it), to test persistence
    /// across a restart. `config` is as for `start`.
    pub async fn restart<F>(self, config: F) -> TestResult<Self>
    where
        F: FnOnce(&Path, &Path, u32) -> String,
    {
        self.hub.stop().await?;
        let uid = current_uid(self.dir.path())?;
        let provider = SealedProvider::open(&self.store_path, &self.kek)
            .map_err(ctx("reopen sealed store"))?;
        let socket = self.dir.path().join("run").join("secure.sock");
        let text = config(self.dir.path(), &self.store_path, uid);
        let (stop, task) =
            RunningHub::spawn(socket.clone(), &text, KeyStore::Sealed(provider)).await?;
        Ok(Self {
            hub: RunningHub {
                _dir: None,
                socket,
                stop: Some(stop),
                task,
            },
            dir: self.dir,
            store_path: self.store_path,
            kek: self.kek,
        })
    }

    /// Open a new HTTP/1 client connection to the currently running hub.
    pub async fn client(&self) -> TestResult<Client> {
        self.hub.client().await
    }

    /// Stop the hub, keeping the temp dir (and thus the store file) alive
    /// in case the caller wants to inspect it.
    pub async fn stop(self) -> TestResult {
        self.hub.stop().await
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
