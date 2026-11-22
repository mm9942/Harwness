//! UIA-only outbound `peer.send`: a short text from this harw session to a
//! configured external agent peer, delivered through its connectors — the
//! durable outbox (read by the `harw_peer_poll` MCP tool), a spool file
//! watched with `tail -F`, and a signed webhook.
//!
//! Secrets (webhook secret, signature) and the webhook URL are never logged
//! and never shown in `Debug`; logs carry only peer, seq, status and attempt.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use harw_agent_dsl::roles::AgentRoleId;
use harw_config::{PeerBridgeSection, PeerConnectorToml, PeerToml};
use harw_egress::{AddrClass, EgressHost, EgressPolicy, EgressUrl, build_client, classify};
use harw_operations::context::OpContext;
use harw_operations::error::OpError;
use harw_operations::op_schema::{object_schema, string_schema};
use harw_operations::operation::{
    ApprovalPolicy, ArgsSchemaFn, OpFuture, OpInput, OpOutput, Operation, OperationCategory,
    OperationDomain, OperationMeta, PermissionTier, Surface,
};
use harw_session_store::peer_bridge::{
    PEER_BRIDGE_DIR, PeerBridgeLayout, PeerOutboundMessage, append_outbound, append_spool_line,
    is_valid_message_id,
};
use serde_json::{Value, json};

use crate::contributors::{AssemblyContributor, AssemblyInputs, AssemblyParts};
use crate::error::RuntimeResult;
use crate::spec::OperationSurface;

/// Name of the outbound peer operation (model tool).
pub const PEER_SEND_TOOL: &str = "peer.send";
/// Maximum webhook delivery attempts per message.
pub const WEBHOOK_MAX_ATTEMPTS: u32 = 3;
/// Timeout of one webhook request.
pub(crate) const WEBHOOK_TIMEOUT: Duration = Duration::from_secs(10);
/// Sleep before the second and third webhook attempt.
pub(crate) const WEBHOOK_BACKOFF: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(2)];
/// Sliding window of the per-peer send rate limit.
pub(crate) const SEND_WINDOW: Duration = Duration::from_secs(60);

const ERR_ARGS: &str = "peer.send: arguments must be an object with to, text and optional reply_to";
const ERR_UNKNOWN_PEER: &str = "peer.send: unknown peer";
const ERR_TEXT_SIZE: &str = "peer.send: text must be 1..=peer_bridge.max_bytes bytes";
const ERR_REPLY_TO: &str = "peer.send: invalid reply_to";
const ERR_RATE: &str = "peer.send: rate limit exceeded";
const ERR_STORAGE: &str = "peer.send: storage error";
const ERR_HOST: &str =
    "peer.send: webhook host is not loopback and not in peer_bridge.webhook_allow_hosts";

/// `<profile>/peer-bridge` of the active profile under `home`.
///
/// # Returns
/// `None` when the profile directory cannot be resolved.
#[must_use]
pub fn peer_bridge_root(home: &Path) -> Option<PathBuf> {
    let profile = harw_home::paths::active_profile_name(home);
    Some(
        harw_home::paths::profile_dir(home, &profile)
            .ok()?
            .join(PEER_BRIDGE_DIR),
    )
}

/// HMAC-SHA256 of `data` under `key`, as 64 lowercase hex characters.
///
/// # Errors
/// `"peer.send: invalid hmac key"` when the key is rejected.
pub fn hmac_sha256_hex(key: &[u8], data: &[u8]) -> Result<String, String> {
    use hmac::{Hmac, KeyInit, Mac};
    type HmacSha256 = Hmac<sha2::Sha256>;

    let mut mac = <HmacSha256 as KeyInit>::new_from_slice(key)
        .map_err(|_| "peer.send: invalid hmac key".to_owned())?;
    mac.update(data);
    let digest = mac.finalize().into_bytes();
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Webhook signature header value: `sha256=<hex hmac of "<timestamp>.<body>">`.
///
/// # Errors
/// See [`hmac_sha256_hex`].
pub fn sign_webhook(secret: &[u8], timestamp: &str, body: &[u8]) -> Result<String, String> {
    let mut signed = Vec::with_capacity(timestamp.len() + 1 + body.len());
    signed.extend_from_slice(timestamp.as_bytes());
    signed.push(b'.');
    signed.extend_from_slice(body);
    Ok(format!("sha256={}", hmac_sha256_hex(secret, &signed)?))
}

/// Checks that the webhook host is loopback or listed in `allow_hosts`.
///
/// # Returns
/// The host (IPv6 without brackets) on success.
///
/// # Errors
/// `"peer.send: invalid webhook url"` or the not-allowed host error.
pub(crate) fn webhook_host_allowed(url: &str, allow_hosts: &[String]) -> Result<String, String> {
    let parsed = EgressUrl::parse(url).map_err(|_| "peer.send: invalid webhook url".to_owned())?;
    let host = parsed.host_str();
    let listed = allow_hosts.iter().any(|entry| {
        entry
            .trim_start_matches('[')
            .trim_end_matches(']')
            .eq_ignore_ascii_case(&host)
    });
    let loopback = match parsed.host() {
        EgressHost::Ip(ip) => classify(*ip) == AddrClass::Loopback,
        EgressHost::Domain(name) => name == "localhost",
    };
    if listed || loopback {
        Ok(host)
    } else {
        Err(ERR_HOST.to_owned())
    }
}

/// Runtime configuration of [`PeerSendOperation`].
#[derive(Clone)]
pub struct PeerSendConfig {
    /// The `peer-bridge` root directory.
    pub root: PathBuf,
    /// Maximum text bytes of one message.
    pub max_bytes: usize,
    /// Rotation size of outbox and spool files.
    pub outbox_max_bytes: u64,
    /// Non-loopback hosts a webhook may call.
    pub webhook_allow_hosts: Vec<String>,
    /// The configured peers.
    pub peers: Vec<PeerToml>,
}

impl PeerSendConfig {
    /// Builds the configuration from `[peer_bridge]`.
    #[must_use]
    pub fn from_section(root: PathBuf, section: &PeerBridgeSection) -> Self {
        Self {
            root,
            max_bytes: usize::try_from(section.max_bytes).unwrap_or(usize::MAX),
            outbox_max_bytes: section.outbox_max_bytes,
            webhook_allow_hosts: section.webhook_allow_hosts.clone(),
            peers: section.peers.clone(),
        }
    }
}

impl std::fmt::Debug for PeerSendConfig {
    /// Redacts webhook URLs and secret references: only ids and sizes.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let peer_ids: Vec<&str> = self.peers.iter().map(|peer| peer.id.as_str()).collect();
        f.debug_struct("PeerSendConfig")
            .field("root", &self.root)
            .field("max_bytes", &self.max_bytes)
            .field("outbox_max_bytes", &self.outbox_max_bytes)
            .field("webhook_allow_hosts", &self.webhook_allow_hosts.len())
            .field("peers", &peer_ids)
            .finish()
    }
}

/// Resolved webhook target of one send (checked before anything is written).
struct WebhookPlan {
    policy: EgressPolicy,
    url: String,
    secret: Vec<u8>,
}

/// The `peer.send` model tool.
pub struct PeerSendOperation {
    config: PeerSendConfig,
    layout: PeerBridgeLayout,
    windows: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl std::fmt::Debug for PeerSendOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerSendOperation")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

const PEER_SEND_ARGS_SCHEMA: ArgsSchemaFn = || {
    object_schema(
        vec![
            (
                "to",
                string_schema("Id of the configured peer ([[peer_bridge.peers]].id)."),
            ),
            (
                "text",
                string_schema("Message text, 1..=peer_bridge.max_bytes bytes of UTF-8."),
            ),
            (
                "reply_to",
                string_schema(
                    "Optional: id of the peer message this answers (1-128 chars of [A-Za-z0-9._:-]).",
                ),
            ),
        ],
        &["to", "text"],
    )
};

fn invalid(message: &str) -> OpError {
    OpError::InvalidArguments(message.to_owned())
}

fn execution(message: &str) -> OpError {
    OpError::Execution(message.to_owned())
}

fn connector_state(on: bool, value: &'static str) -> &'static str {
    if on { value } else { "off" }
}

impl PeerSendOperation {
    /// Builds the operation; the layout is rooted at `config.root`.
    #[must_use]
    pub fn new(config: PeerSendConfig) -> Self {
        let layout = PeerBridgeLayout::new(config.root.clone());
        Self {
            config,
            layout,
            windows: Mutex::new(HashMap::new()),
        }
    }

    /// Validates, rate-limits and delivers one message from `session`.
    ///
    /// # Errors
    /// [`OpError::InvalidArguments`] for bad arguments, [`OpError::Execution`]
    /// for webhook preflight, rate limit and storage failures.
    pub(crate) fn send(&self, session: &str, args: &Value) -> Result<OpOutput, OpError> {
        // 1. Closed argument object.
        let object = args.as_object().ok_or_else(|| invalid(ERR_ARGS))?;
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "to" | "text" | "reply_to"))
        {
            return Err(invalid(ERR_ARGS));
        }
        // 2. Peer.
        let to = object
            .get("to")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid(ERR_UNKNOWN_PEER))?;
        let peer = self
            .config
            .peers
            .iter()
            .find(|peer| peer.id == to)
            .ok_or_else(|| invalid(ERR_UNKNOWN_PEER))?;
        // 3. Text.
        let text = object
            .get("text")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty() && text.len() <= self.config.max_bytes)
            .ok_or_else(|| invalid(ERR_TEXT_SIZE))?;
        // 4. reply_to.
        let reply_to = match object.get("reply_to") {
            None | Some(Value::Null) => None,
            Some(Value::String(id)) if is_valid_message_id(id) => Some(id.clone()),
            Some(_) => return Err(invalid(ERR_REPLY_TO)),
        };
        // 5. Webhook preflight.
        let webhook = if peer.has_connector(PeerConnectorToml::Webhook) {
            Some(self.webhook_preflight(peer)?)
        } else {
            None
        };
        // 6. Rate limit.
        self.admit(peer)?;
        // 7. Outbox (for every peer).
        let message = PeerOutboundMessage {
            v: 1,
            seq: 0,
            id: String::new(),
            from: "harw".to_owned(),
            to: peer.id.clone(),
            session: session.to_owned(),
            sent_at: jiff::Timestamp::now().to_string(),
            text: text.to_owned(),
            reply_to,
        };
        let stored = append_outbound(&self.layout, message, self.config.outbox_max_bytes)
            .map_err(|error| {
                tracing::warn!(peer = %peer.id, %error, "peer_bridge.outbox_write_failed");
                execution(ERR_STORAGE)
            })?;
        // 8. Wire line.
        let line = serde_json::to_vec(&stored).map_err(|error| {
            tracing::warn!(peer = %peer.id, %error, "peer_bridge.encode_failed");
            execution(ERR_STORAGE)
        })?;
        // 9. Spool.
        let spool = peer.has_connector(PeerConnectorToml::Spool);
        if spool {
            self.layout
                .spool_path(&peer.id)
                .and_then(|path| append_spool_line(&path, &line, self.config.outbox_max_bytes))
                .map_err(|error| {
                    tracing::warn!(peer = %peer.id, %error, "peer_bridge.spool_write_failed");
                    execution(ERR_STORAGE)
                })?;
        }
        // 10. Webhook (detached).
        let webhook_queued = match webhook {
            Some(plan) => match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    let _detached = handle.spawn(deliver_webhook(
                        Arc::new(plan.policy),
                        plan.url,
                        plan.secret,
                        line.clone(),
                        peer.id.clone(),
                        stored.seq,
                        &WEBHOOK_BACKOFF,
                    ));
                    true
                }
                Err(_) => {
                    tracing::warn!(peer = %peer.id, seq = stored.seq, "peer_bridge.webhook_no_runtime");
                    false
                }
            },
            None => false,
        };
        // 11. MCP poll.
        let mcp = peer.has_connector(PeerConnectorToml::Mcp);
        // 12. Result.
        Ok(OpOutput {
            text: format!("peer.send: sent seq {} to {}", stored.seq, peer.id),
            data: Some(json!({
                "status": "sent",
                "seq": stored.seq,
                "id": stored.id,
                "connectors": {
                    "spool": connector_state(spool, "written"),
                    "webhook": connector_state(webhook_queued, "queued"),
                    "mcp": connector_state(mcp, "available"),
                },
            })),
        })
    }

    /// Resolves and checks the webhook target before anything is written.
    fn webhook_preflight(&self, peer: &PeerToml) -> Result<WebhookPlan, OpError> {
        let url = peer
            .webhook_url
            .clone()
            .ok_or_else(|| execution("peer.send: webhook url missing"))?;
        let host = webhook_host_allowed(&url, &self.config.webhook_allow_hosts)
            .map_err(OpError::Execution)?;
        let secret = peer
            .secret_ref
            .as_ref()
            .and_then(crate::mcp_wiring::resolve_token)
            .ok_or_else(|| execution("peer.send: webhook secret is not resolvable"))?;
        let refused = || execution("peer.send: webhook url refused by egress policy");
        let policy = EgressPolicy::new(vec![host], true).map_err(|_| refused())?;
        policy.check_url(&url).map_err(|_| refused())?;
        Ok(WebhookPlan {
            policy,
            url,
            secret,
        })
    }

    /// Sliding 60 s send window per peer.
    fn admit(&self, peer: &PeerToml) -> Result<(), OpError> {
        let mut windows = self
            .windows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let window = windows.entry(peer.id.clone()).or_default();
        let now = Instant::now();
        while window
            .front()
            .is_some_and(|sent| now.duration_since(*sent) >= SEND_WINDOW)
        {
            window.pop_front();
        }
        let max = usize::try_from(peer.max_sends_per_minute).unwrap_or(usize::MAX);
        if window.len() >= max {
            return Err(execution(ERR_RATE));
        }
        window.push_back(now);
        Ok(())
    }
}

impl Operation for PeerSendOperation {
    fn meta(&self) -> &OperationMeta {
        static META: OnceLock<OperationMeta> = OnceLock::new();
        META.get_or_init(|| OperationMeta {
            name: PEER_SEND_TOOL,
            summary: "Sends a short text message from this harw session to a configured \
                      external agent peer (Claude Code, Codex) through its connectors: spool \
                      file, signed webhook, MCP poll outbox.",
            domain: OperationDomain::Agents,
            permission: PermissionTier::Operator,
            surfaces: vec![Surface::ModelTool {
                readonly: false,
                approval: ApprovalPolicy::None,
            }],
            category: OperationCategory::Agent,
            args_schema: Some(PEER_SEND_ARGS_SCHEMA),
            ..OperationMeta::default()
        })
    }

    fn run<'a>(&'a self, ctx: &'a OpContext, input: OpInput) -> OpFuture<'a> {
        Box::pin(async move {
            if !input.invocation.is_model_tool() {
                return Err(invalid("peer.send: is only available as a model tool"));
            }
            self.send(ctx.session_id().as_str(), input.invocation.json_args())
        })
    }
}

fn unix_seconds() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
        .to_string()
}

/// Posts `body` to `url` with HMAC headers, retrying transport errors, 429
/// and 5xx up to [`WEBHOOK_MAX_ATTEMPTS`] times.
///
/// # Returns
/// The number of attempts made: 0 if the client or url fails, else 1..=3.
pub(crate) async fn deliver_webhook(
    policy: Arc<EgressPolicy>,
    url: String,
    secret: Vec<u8>,
    body: Vec<u8>,
    peer: String,
    seq: u64,
    backoff: &'static [Duration],
) -> u32 {
    let client = match build_client(policy.clone()) {
        Ok(client) => client,
        Err(error) => {
            tracing::warn!(%peer, seq, %error, "peer_bridge.webhook_client_failed");
            return 0;
        }
    };
    let checked = match policy.check_url(&url) {
        Ok(checked) => checked,
        Err(_) => {
            tracing::warn!(%peer, seq, "peer_bridge.webhook_url_refused");
            return 0;
        }
    };
    for attempt in 1..=WEBHOOK_MAX_ATTEMPTS {
        let timestamp = unix_seconds();
        let signature = match sign_webhook(&secret, &timestamp, &body) {
            Ok(signature) => signature,
            Err(_) => {
                tracing::warn!(%peer, seq, attempt, "peer_bridge.webhook_sign_failed");
                return attempt.saturating_sub(1);
            }
        };
        let outcome = client
            .post(checked.as_url().clone())
            .header("Content-Type", "application/json")
            .header("X-Harw-Timestamp", timestamp)
            .header("X-Harw-Signature", signature)
            .body(body.clone())
            .timeout(WEBHOOK_TIMEOUT)
            .send()
            .await;
        let status = match outcome {
            Ok(response) => response.status().as_u16(),
            Err(_) => 0,
        };
        if status != 0 && status != 429 && status < 500 {
            if status >= 400 {
                tracing::warn!(%peer, seq, status, attempt, "peer_bridge.webhook_delivered");
            } else {
                tracing::info!(%peer, seq, status, attempt, "peer_bridge.webhook_delivered");
            }
            return attempt;
        }
        tracing::warn!(%peer, seq, status, attempt, "peer_bridge.webhook_retry");
        if attempt >= WEBHOOK_MAX_ATTEMPTS {
            return attempt;
        }
        let pause = usize::try_from(attempt - 1)
            .ok()
            .and_then(|index| backoff.get(index));
        if let Some(pause) = pause {
            tokio::time::sleep(*pause).await;
        }
    }
    WEBHOOK_MAX_ATTEMPTS
}

/// Registers `peer.send` for a UIA root when `[peer_bridge]` is enabled.
#[derive(Debug, Default)]
pub struct PeerBridgeContributor;

impl AssemblyContributor for PeerBridgeContributor {
    fn contribute(
        &self,
        inputs: &AssemblyInputs<'_>,
        parts: &mut AssemblyParts,
    ) -> RuntimeResult<()> {
        let section = &inputs.config.harness.peer_bridge;
        if inputs.spawn_context.organizational_role != AgentRoleId::UserInterface
            || inputs.profile.operations == OperationSurface::None
            || !section.enabled
            || section.peers.is_empty()
        {
            return Ok(());
        }
        let Some(root) = peer_bridge_root(&inputs.spec.home) else {
            return Ok(());
        };
        let operation = PeerSendOperation::new(PeerSendConfig::from_section(root, section));
        if let Err(error) = parts.operations.try_register(Arc::new(operation)) {
            tracing::warn!(%error, "runtime.peer_bridge_operation_not_registered");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_config::SecretRef;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn peer(id: &str, connectors: Vec<PeerConnectorToml>) -> PeerToml {
        PeerToml {
            id: id.to_owned(),
            principal: id.to_owned(),
            connectors,
            webhook_url: None,
            secret_ref: None,
            wake: true,
            wake_burst: 3,
            wake_interval_secs: 30,
            max_sends_per_minute: 30,
        }
    }

    fn config(root: &Path, peers: Vec<PeerToml>) -> PeerSendConfig {
        PeerSendConfig {
            root: root.to_path_buf(),
            max_bytes: 16,
            outbox_max_bytes: 1_048_576,
            webhook_allow_hosts: Vec::new(),
            peers,
        }
    }

    fn data(output: &OpOutput) -> Result<&Value, Box<dyn std::error::Error>> {
        output.data.as_ref().ok_or_else(|| "missing data".into())
    }

    fn lines(path: &Path) -> Result<Vec<PeerOutboundMessage>, Box<dyn std::error::Error>> {
        let raw = std::fs::read_to_string(path)?;
        let mut out = Vec::new();
        for line in raw.lines() {
            out.push(serde_json::from_str(line)?);
        }
        Ok(out)
    }

    fn expect_err(result: Result<OpOutput, OpError>, needle: &str) -> TestResult {
        match result {
            Err(OpError::InvalidArguments(msg) | OpError::Execution(msg)) if msg == needle => {
                Ok(())
            }
            other => Err(format!("expected {needle:?}, got {other:?}").into()),
        }
    }

    #[test]
    fn hmac_matches_rfc4231_case_2() -> TestResult {
        assert_eq!(
            hmac_sha256_hex(b"Jefe", b"what do ya want for nothing?")?,
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        Ok(())
    }

    #[test]
    fn sign_webhook_signs_timestamp_dot_body() -> TestResult {
        let key = b"key";
        assert_eq!(
            sign_webhook(key, "123", b"{}")?,
            format!("sha256={}", hmac_sha256_hex(key, b"123.{}")?)
        );
        Ok(())
    }

    #[test]
    fn webhook_host_allowed_admits_loopback_and_listed_hosts() -> TestResult {
        for url in [
            "http://127.0.0.1:9/x",
            "http://[::1]:9/x",
            "http://localhost:9/x",
        ] {
            assert!(webhook_host_allowed(url, &[]).is_ok(), "{url}");
        }
        for url in ["http://10.0.0.1/x", "https://example.org/x"] {
            assert_eq!(webhook_host_allowed(url, &[]), Err(ERR_HOST.to_owned()));
        }
        let listed = vec!["10.0.0.1".to_owned(), "example.org".to_owned()];
        assert_eq!(
            webhook_host_allowed("http://10.0.0.1/x", &listed)?,
            "10.0.0.1"
        );
        assert_eq!(
            webhook_host_allowed("https://example.org/x", &listed)?,
            "example.org"
        );
        Ok(())
    }

    #[test]
    fn spool_connector_writes_outbox_and_spool() -> TestResult {
        let dir = tempfile::tempdir()?;
        let op = PeerSendOperation::new(config(
            dir.path(),
            vec![peer(
                "claude",
                vec![PeerConnectorToml::Spool, PeerConnectorToml::Mcp],
            )],
        ));
        let args = json!({"to": "claude", "text": "hi", "reply_to": "m-1"});
        let output = op.send("sess-1", &args)?;
        let value = data(&output)?;
        assert_eq!(value["status"], "sent");
        assert_eq!(value["seq"], 1);
        assert_eq!(value["id"], "harw-1");
        assert_eq!(
            value["connectors"],
            json!({"spool": "written", "webhook": "off", "mcp": "available"})
        );
        for path in [
            dir.path().join("outbox").join("claude.jsonl"),
            dir.path().join("spool").join("claude.jsonl"),
        ] {
            let records = lines(&path)?;
            assert_eq!(records.len(), 1);
            let record = &records[0];
            assert_eq!(record.v, 1);
            assert_eq!(record.from, "harw");
            assert_eq!(record.to, "claude");
            assert_eq!(record.session, "sess-1");
            assert_eq!(record.text, "hi");
            assert_eq!(record.reply_to.as_deref(), Some("m-1"));
        }
        let second = op.send("sess-1", &json!({"to": "claude", "text": "again"}))?;
        assert_eq!(data(&second)?["seq"], 2);
        Ok(())
    }

    #[test]
    fn peer_without_spool_writes_only_the_outbox() -> TestResult {
        let dir = tempfile::tempdir()?;
        let op = PeerSendOperation::new(config(
            dir.path(),
            vec![peer("codex", vec![PeerConnectorToml::Mcp])],
        ));
        let output = op.send("sess-1", &json!({"to": "codex", "text": "hi"}))?;
        assert_eq!(data(&output)?["connectors"]["spool"], "off");
        assert!(!dir.path().join("spool").join("codex.jsonl").exists());
        assert_eq!(
            lines(&dir.path().join("outbox").join("codex.jsonl"))?.len(),
            1
        );
        Ok(())
    }

    #[test]
    fn send_rejects_bad_arguments_and_rate_limits() -> TestResult {
        let dir = tempfile::tempdir()?;
        let mut limited = peer("claude", vec![PeerConnectorToml::Mcp]);
        limited.max_sends_per_minute = 1;
        let op = PeerSendOperation::new(config(dir.path(), vec![limited]));
        expect_err(
            op.send("s", &json!({"to": "nobody", "text": "hi"})),
            ERR_UNKNOWN_PEER,
        )?;
        expect_err(
            op.send("s", &json!({"to": "claude", "text": ""})),
            ERR_TEXT_SIZE,
        )?;
        expect_err(
            op.send("s", &json!({"to": "claude", "text": "x".repeat(17)})),
            ERR_TEXT_SIZE,
        )?;
        expect_err(
            op.send("s", &json!({"to": "claude", "text": "hi", "reply_to": "a b"})),
            ERR_REPLY_TO,
        )?;
        expect_err(
            op.send("s", &json!({"to": "claude", "text": "hi", "extra": 1})),
            ERR_ARGS,
        )?;
        op.send("s", &json!({"to": "claude", "text": "hi"}))?;
        expect_err(op.send("s", &json!({"to": "claude", "text": "hi"})), ERR_RATE)?;
        Ok(())
    }

    #[test]
    fn debug_never_shows_secret_or_webhook_url() -> TestResult {
        let dir = tempfile::tempdir()?;
        let secret_path = dir.path().join("secret");
        std::fs::write(&secret_path, "s3cr3t-value")?;
        let mut hooked = peer("claude", vec![PeerConnectorToml::Webhook]);
        hooked.webhook_url = Some("http://127.0.0.1:9/hook-path".to_owned());
        hooked.secret_ref = Some(SecretRef::File(
            secret_path.to_string_lossy().into_owned(),
        ));
        let cfg = config(dir.path(), vec![hooked]);
        let op = PeerSendOperation::new(cfg.clone());
        for rendered in [format!("{cfg:?}"), format!("{op:?}")] {
            assert!(!rendered.contains("s3cr3t-value"), "{rendered}");
            assert!(!rendered.contains("hook-path"), "{rendered}");
        }
        Ok(())
    }

    type Recorded = Arc<Mutex<Vec<(HashMap<String, String>, Vec<u8>)>>>;

    async fn serve_one(
        mut stream: tokio::net::TcpStream,
        response: &'static str,
        recorded: &Recorded,
    ) -> std::io::Result<()> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos;
            }
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&chunk[..read]);
        };
        let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
        let mut headers = HashMap::new();
        for line in head.split("\r\n").skip(1) {
            if let Some((name, value)) = line.split_once(':') {
                headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
            }
        }
        let length: usize = headers
            .get("content-length")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let mut body = buf[header_end + 4..].to_vec();
        while body.len() < length {
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..read]);
        }
        body.truncate(length);
        if let Ok(mut guard) = recorded.lock() {
            guard.push((headers, body));
        }
        stream.write_all(response.as_bytes()).await?;
        stream.shutdown().await
    }

    async fn spawn_server(
        response: &'static str,
    ) -> Result<(String, Recorded), Box<dyn std::error::Error>> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let recorded: Recorded = Arc::new(Mutex::new(Vec::new()));
        let sink = recorded.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                // A client that hangs up early only ends this connection.
                let _ = serve_one(stream, response, &sink).await;
            }
        });
        Ok((format!("http://{addr}/hook"), recorded))
    }

    async fn deliver_to(url: String, body: &[u8]) -> Result<u32, Box<dyn std::error::Error>> {
        let policy = Arc::new(EgressPolicy::new(vec!["127.0.0.1".into()], true)?);
        Ok(deliver_webhook(
            policy,
            url,
            b"key".to_vec(),
            body.to_vec(),
            "claude".into(),
            1,
            &[],
        )
        .await)
    }

    #[tokio::test]
    async fn webhook_retries_5xx_and_signs_every_attempt() -> TestResult {
        tokio::time::timeout(Duration::from_secs(20), async {
            let body = br#"{"v":1,"seq":1}"#;
            let (url, recorded) = spawn_server(
                "HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            )
            .await?;
            assert_eq!(deliver_to(url, body).await?, 3);
            let requests = recorded
                .lock()
                .map_err(|_| "poisoned")?
                .clone();
            assert_eq!(requests.len(), 3);
            for (headers, got) in &requests {
                assert_eq!(got.as_slice(), body.as_slice());
                assert_eq!(
                    headers.get("content-type").map(String::as_str),
                    Some("application/json")
                );
                let ts = headers.get("x-harw-timestamp").ok_or("missing timestamp")?;
                let expected = sign_webhook(b"key", ts, body)?;
                assert_eq!(headers.get("x-harw-signature"), Some(&expected));
            }

            let (ok_url, ok_recorded) =
                spawn_server("HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                    .await?;
            assert_eq!(deliver_to(ok_url, body).await?, 1);
            assert_eq!(ok_recorded.lock().map_err(|_| "poisoned")?.len(), 1);
            Ok::<(), Box<dyn std::error::Error>>(())
        })
        .await?
    }
}
