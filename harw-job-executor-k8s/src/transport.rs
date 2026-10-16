//! The API transport: a trait plus a reqwest-based implementation.
//!
//! `reqwest::blocking` must not run on an async worker thread, and
//! `Executor::start` is called from one; every request therefore runs on its
//! own short-lived thread.

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use crate::config::K8sConfig;
use crate::error::K8sError;

/// One API request.
#[derive(Debug, Clone)]
pub struct Request {
    /// HTTP method.
    pub method: &'static str,
    /// Absolute path with query.
    pub path: String,
    /// `Content-Type` of the body.
    pub content_type: &'static str,
    /// Body, if any.
    pub body: Option<Vec<u8>>,
}

/// A buffered reply.
#[derive(Debug, Clone)]
pub struct Reply {
    /// Status code.
    pub status: u16,
    /// Body (capped).
    pub body: Vec<u8>,
}

/// The API access seam.
pub trait KubeTransport: Send + Sync + 'static {
    /// Sends a request and buffers the reply.
    ///
    /// # Errors
    /// [`K8sError::Transport`] or [`K8sError::Protocol`] (oversized body).
    fn send(&self, request: &Request) -> Result<Reply, K8sError>;

    /// Opens a streaming `GET` (pod logs).
    ///
    /// # Errors
    /// As [`KubeTransport::send`], plus non-200 statuses.
    fn stream(&self, path: &str) -> Result<Box<dyn Read + Send>, K8sError>;
}

/// Real transport over HTTP(S).
pub struct HttpTransport {
    client: Option<Arc<reqwest::blocking::Client>>,
    base: String,
    token: Option<String>,
    max_body: usize,
}

impl std::fmt::Debug for HttpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpTransport")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

fn on_thread<T: Send + 'static>(job: impl FnOnce() -> T + Send + 'static) -> Result<T, K8sError> {
    std::thread::Builder::new()
        .name("harw-k8s-http".into())
        .spawn(job)
        .map_err(|e| K8sError::Transport(e.to_string()))?
        .join()
        .map_err(|_| K8sError::Transport("http worker panicked".into()))
}

impl HttpTransport {
    /// Builds the client (on its own thread).
    ///
    /// # Errors
    /// [`K8sError::Config`] for a bad CA bundle, [`K8sError::Transport`].
    pub fn new(config: &K8sConfig) -> Result<Self, K8sError> {
        let timeout = config.request_timeout;
        let ca = config.ca_pem.clone();
        let client = on_thread(move || -> Result<reqwest::blocking::Client, K8sError> {
            let mut builder = reqwest::blocking::Client::builder()
                .timeout(timeout)
                .redirect(reqwest::redirect::Policy::none());
            if let Some(pem) = ca {
                let cert = reqwest::Certificate::from_pem(&pem)
                    .map_err(|e| K8sError::Config(format!("CA bundle: {e}")))?;
                builder = builder.add_root_certificate(cert);
            }
            builder
                .build()
                .map_err(|e| K8sError::Transport(e.to_string()))
        })??;
        Ok(Self {
            client: Some(Arc::new(client)),
            base: config.api_base.clone(),
            token: config.token.clone(),
            max_body: config.max_body_bytes,
        })
    }

    fn client(&self) -> Result<Arc<reqwest::blocking::Client>, K8sError> {
        self.client
            .clone()
            .ok_or_else(|| K8sError::Transport("client closed".into()))
    }
}

impl Drop for HttpTransport {
    fn drop(&mut self) {
        // A blocking client must not be dropped on an async worker thread.
        if let Some(client) = self.client.take() {
            let _ = std::thread::Builder::new()
                .name("harw-k8s-drop".into())
                .spawn(move || drop(client));
        }
    }
}

impl KubeTransport for HttpTransport {
    fn send(&self, request: &Request) -> Result<Reply, K8sError> {
        let client = self.client()?;
        let url = format!("{}{}", self.base, request.path);
        let token = self.token.clone();
        let request = request.clone();
        let max = self.max_body;
        on_thread(move || {
            let method = reqwest::Method::from_bytes(request.method.as_bytes())
                .map_err(|e| K8sError::Protocol(e.to_string()))?;
            let mut call = client.request(method, url);
            if let Some(token) = token {
                call = call.bearer_auth(token);
            }
            if let Some(body) = request.body {
                call = call.header("Content-Type", request.content_type).body(body);
            }
            let response = call
                .send()
                .map_err(|e| K8sError::Transport(e.to_string()))?;
            let status = response.status().as_u16();
            let mut body = Vec::new();
            response
                .take(max as u64 + 1)
                .read_to_end(&mut body)
                .map_err(|e| K8sError::Transport(e.to_string()))?;
            if body.len() > max {
                return Err(K8sError::Protocol(format!(
                    "response larger than {max} bytes"
                )));
            }
            Ok(Reply { status, body })
        })?
    }

    fn stream(&self, path: &str) -> Result<Box<dyn Read + Send>, K8sError> {
        let client = self.client()?;
        let url = format!("{}{path}", self.base);
        let token = self.token.clone();
        let response = on_thread(move || {
            let mut call = client.get(url).timeout(Duration::from_secs(60 * 60 * 24));
            if let Some(token) = token {
                call = call.bearer_auth(token);
            }
            call.send().map_err(|e| K8sError::Transport(e.to_string()))
        })??;
        if response.status().as_u16() != 200 {
            return Err(K8sError::Protocol(format!(
                "log stream answered {}",
                response.status().as_u16()
            )));
        }
        Ok(Box::new(response))
    }
}
