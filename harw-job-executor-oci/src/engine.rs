//! Typed calls of the Docker-compatible engine API.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::Arc;

use harw_container_model::{ContainerId, ImageDigest};
use serde::Deserialize;

use crate::config::OciConfig;
use crate::error::OciError;
use crate::http::{self, API, Body};

/// The parts of `GET /containers/{id}/json` the executor reads back.
/// Every field is optional: a field the engine did not report is never
/// treated as enforced.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct InspectDoc {
    /// Engine-assigned id.
    pub id: String,
    /// Creation time (RFC 3339).
    pub created: String,
    /// Container configuration.
    pub config: ConfigDoc,
    /// Applied host configuration.
    pub host_config: HostConfigDoc,
    /// Runtime state.
    pub state: StateDoc,
    /// Mounts actually attached (`None` when not reported).
    pub mounts: Option<Vec<MountDoc>>,
}

/// `Config` of an inspect document.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ConfigDoc {
    /// The image reference the container was created from.
    pub image: Option<String>,
    /// Labels.
    pub labels: Option<BTreeMap<String, String>>,
}

/// `HostConfig` of an inspect document.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct HostConfigDoc {
    /// Network mode.
    pub network_mode: Option<String>,
    /// Read-only root filesystem.
    pub readonly_rootfs: Option<bool>,
    /// Privileged mode.
    pub privileged: Option<bool>,
    /// Dropped capabilities.
    pub cap_drop: Option<Vec<String>>,
    /// Added capabilities.
    pub cap_add: Option<Vec<String>>,
    /// Security options.
    pub security_opt: Option<Vec<String>>,
    /// Memory limit in bytes (0 = none).
    pub memory: Option<u64>,
    /// Pids limit (0 or -1 = none).
    pub pids_limit: Option<i64>,
    /// CPU shares.
    pub cpu_shares: Option<u64>,
    /// Bind mounts.
    pub binds: Option<Vec<String>>,
}

/// `State` of an inspect document.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct StateDoc {
    /// Whether the container runs.
    pub running: Option<bool>,
    /// Exit code of the last run.
    pub exit_code: Option<i64>,
}

/// One entry of `Mounts`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct MountDoc {
    /// `bind`, `volume`, `tmpfs`.
    pub r#type: String,
    /// Destination in the container.
    pub destination: String,
}

fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The engine client.
#[derive(Debug, Clone)]
pub(crate) struct Engine {
    cfg: Arc<OciConfig>,
}

impl Engine {
    pub(crate) fn new(cfg: Arc<OciConfig>) -> Self {
        Self { cfg }
    }

    pub(crate) fn config(&self) -> &OciConfig {
        &self.cfg
    }

    fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
    ) -> Result<http::Response, OciError> {
        let stream = http::connect(
            &self.cfg.socket,
            self.cfg.peer,
            Some(self.cfg.control_timeout),
        )?;
        http::call(stream, method, path, body, self.cfg.max_body_bytes)
    }

    fn expect(
        &self,
        response: http::Response,
        ok: &[u16],
        what: &str,
    ) -> Result<Vec<u8>, OciError> {
        if ok.contains(&response.status) {
            Ok(response.body)
        } else if response.status == 404 {
            Err(OciError::NotFound)
        } else {
            let text = String::from_utf8_lossy(&response.body);
            let text: String = text.chars().take(200).collect();
            Err(OciError::Protocol(format!(
                "{what}: engine answered {} {text}",
                response.status
            )))
        }
    }

    /// Verifies the pinned image exists locally with exactly this manifest
    /// digest (`RepoDigests`). Nothing is pulled.
    pub(crate) fn require_image(&self, image: &ImageDigest) -> Result<(), OciError> {
        let reference = image.reference();
        if !reference.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b':' | b'-' | b'@')
        }) {
            return Err(OciError::Config(
                "image reference has unsafe characters".into(),
            ));
        }
        let response = self.call("GET", &format!("{API}/images/{reference}/json"), None)?;
        let body = match self.expect(response, &[200], "image inspect") {
            Err(OciError::NotFound) => return Err(OciError::ImageMissing(reference)),
            other => other?,
        };
        #[derive(Deserialize, Default)]
        #[serde(rename_all = "PascalCase", default)]
        struct ImageDoc {
            repo_digests: Option<Vec<String>>,
        }
        let doc: ImageDoc = serde_json::from_slice(&body)
            .map_err(|e| OciError::Protocol(format!("image json: {e}")))?;
        let suffix = format!("@{}", image.digest());
        if doc
            .repo_digests
            .unwrap_or_default()
            .iter()
            .any(|d| d.ends_with(&suffix))
        {
            Ok(())
        } else {
            Err(OciError::ImageMissing(format!(
                "{reference} (local image has another digest)"
            )))
        }
    }

    pub(crate) fn create(&self, body: &serde_json::Value) -> Result<ContainerId, OciError> {
        let bytes = serde_json::to_vec(body).map_err(|e| OciError::Protocol(e.to_string()))?;
        let response = self.call("POST", &format!("{API}/containers/create"), Some(&bytes))?;
        let body = self.expect(response, &[201], "create")?;
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Created {
            id: String,
        }
        let created: Created = serde_json::from_slice(&body)
            .map_err(|e| OciError::Protocol(format!("create json: {e}")))?;
        ContainerId::new(&created.id)
            .map_err(|_| OciError::Protocol("engine returned a malformed container id".into()))
    }

    pub(crate) fn inspect(&self, id: &ContainerId) -> Result<InspectDoc, OciError> {
        let response = self.call("GET", &format!("{API}/containers/{id}/json"), None)?;
        let body = self.expect(response, &[200], "inspect")?;
        serde_json::from_slice(&body).map_err(|e| OciError::Protocol(format!("inspect json: {e}")))
    }

    pub(crate) fn start(&self, id: &ContainerId) -> Result<(), OciError> {
        let response = self.call("POST", &format!("{API}/containers/{id}/start"), None)?;
        self.expect(response, &[204, 304], "start").map(drop)
    }

    pub(crate) fn stop(&self, id: &ContainerId, grace_secs: u32) -> Result<(), OciError> {
        let response = self.call(
            "POST",
            &format!("{API}/containers/{id}/stop?t={grace_secs}"),
            None,
        )?;
        self.expect(response, &[204, 304], "stop").map(drop)
    }

    pub(crate) fn kill(&self, id: &ContainerId) -> Result<(), OciError> {
        let response = self.call(
            "POST",
            &format!("{API}/containers/{id}/kill?signal=KILL"),
            None,
        )?;
        // 409 = not running: already gone, which is what was wanted.
        if response.status == 409 {
            return Ok(());
        }
        self.expect(response, &[204], "kill").map(drop)
    }

    pub(crate) fn remove(&self, id: &ContainerId) -> Result<(), OciError> {
        let response = self.call(
            "DELETE",
            &format!("{API}/containers/{id}?force=1&v=1"),
            None,
        )?;
        match self.expect(response, &[204], "remove") {
            Err(OciError::NotFound) => Ok(()),
            other => other.map(drop),
        }
    }

    /// Blocks until the container is not running; returns the exit code.
    pub(crate) fn wait(&self, id: &ContainerId) -> Result<i64, OciError> {
        let stream = http::connect(&self.cfg.socket, self.cfg.peer, None)?;
        let response = http::call(
            stream,
            "POST",
            &format!("{API}/containers/{id}/wait?condition=not-running"),
            None,
            self.cfg.max_body_bytes,
        )?;
        let body = self.expect(response, &[200], "wait")?;
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Waited {
            status_code: i64,
        }
        let waited: Waited = serde_json::from_slice(&body)
            .map_err(|e| OciError::Protocol(format!("wait json: {e}")))?;
        Ok(waited.status_code)
    }

    /// Opens the followed, multiplexed log stream (no read timeout).
    pub(crate) fn logs(&self, id: &ContainerId, tail: &str) -> Result<LogStream, OciError> {
        let stream = http::connect(&self.cfg.socket, self.cfg.peer, None)?;
        let (status, body) = http::open(
            stream,
            "GET",
            &format!("{API}/containers/{id}/logs?follow=1&stdout=1&stderr=1&tail={tail}"),
            None,
        )?;
        if status != 200 {
            return Err(OciError::Protocol(format!(
                "logs: engine answered {status}"
            )));
        }
        Ok(LogStream { body })
    }

    /// Ids of containers carrying every `key=value` label.
    pub(crate) fn list_by_labels(
        &self,
        labels: &[(String, String)],
    ) -> Result<Vec<ContainerId>, OciError> {
        let filter = serde_json::json!({
            "label": labels.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>()
        });
        let path = format!(
            "{API}/containers/json?all=1&filters={}",
            encode(&filter.to_string())
        );
        let response = self.call("GET", &path, None)?;
        let body = self.expect(response, &[200], "list")?;
        #[derive(Deserialize)]
        #[serde(rename_all = "PascalCase")]
        struct Entry {
            id: String,
        }
        let entries: Vec<Entry> = serde_json::from_slice(&body)
            .map_err(|e| OciError::Protocol(format!("list json: {e}")))?;
        entries
            .iter()
            .map(|e| {
                ContainerId::new(&e.id)
                    .map_err(|_| OciError::Protocol("list returned a malformed id".into()))
            })
            .collect()
    }
}

/// Which stream a log frame belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Channel {
    Stdout,
    Stderr,
}

/// A demultiplexing reader over the engine's log framing
/// (`[stream, 0, 0, 0, len_be32]` + payload).
pub(crate) struct LogStream {
    body: Body,
}

/// Largest accepted single frame.
const MAX_FRAME: usize = 1024 * 1024;

impl LogStream {
    /// The next frame; `None` at a clean end of stream.
    pub(crate) fn next_frame(&mut self) -> Result<Option<(Channel, Vec<u8>)>, OciError> {
        let mut header = [0u8; 8];
        let mut read = 0;
        while read < header.len() {
            let n = self
                .body
                .read(&mut header[read..])
                .map_err(|e| OciError::Io(format!("log stream: {e}")))?;
            if n == 0 {
                return if read == 0 {
                    Ok(None)
                } else {
                    Err(OciError::Protocol("truncated log frame header".into()))
                };
            }
            read += n;
        }
        let channel = match header[0] {
            1 => Channel::Stdout,
            2 => Channel::Stderr,
            // stdin echo (0) and anything unknown are not job output.
            _ => return Err(OciError::Protocol("unknown log stream id".into())),
        };
        let len = u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize;
        if len > MAX_FRAME {
            return Err(OciError::Protocol("log frame too large".into()));
        }
        let mut payload = vec![0u8; len];
        self.body
            .read_exact(&mut payload)
            .map_err(|e| OciError::Io(format!("log frame: {e}")))?;
        Ok(Some((channel, payload)))
    }
}
