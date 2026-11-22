//! `[peer_bridge]` — the file-based bridge between harw sessions and external
//! peer agents (other coding agents reached via MCP, spool file or webhook).
//!
//! Every field is **global-only** (home layer) and security critical: a
//! profile or an untrusted project can neither enable the bridge nor add a
//! peer, widen the size limits or point a webhook elsewhere. Default: off.
//!
//! Enforcement lives in `harw-mcp-server` (`peer.rs`, the peer MCP tools) and
//! `harw-runtime` (`peer_bridge_wiring.rs`, the `peer.send` operation); this
//! module is declarative and only validates the shape. The peer id syntax
//! (`[A-Za-z0-9_-]{1,64}`) is checked locally because this crate must not
//! depend on `harw-session-store`.

use serde::{Deserialize, Serialize};

use crate::auth_toml::SecretRef;

/// Default maximum message text size in bytes.
pub const DEFAULT_PEER_MAX_BYTES: u32 = 16384;
/// Upper bound for `max_bytes`.
pub const MAX_PEER_MAX_BYTES: u32 = 32768;
/// Default maximum number of queued inbound messages per session.
pub const DEFAULT_PEER_QUEUE_CAP: u32 = 64;
/// Upper bound for `queue_cap`.
pub const MAX_PEER_QUEUE_CAP: u32 = 1024;
/// Default rotation size of the outbox and spool files in bytes.
pub const DEFAULT_PEER_OUTBOX_MAX_BYTES: u64 = 1_048_576;
/// Smallest accepted `outbox_max_bytes`.
pub const MIN_PEER_OUTBOX_MAX_BYTES: u64 = 65_536;
/// Default wake-up burst per peer.
pub const DEFAULT_PEER_WAKE_BURST: u32 = 3;
/// Default seconds until one wake-up token is refilled.
pub const DEFAULT_PEER_WAKE_INTERVAL_SECS: u64 = 30;
/// Default sends per minute per peer.
pub const DEFAULT_PEER_MAX_SENDS_PER_MINUTE: u32 = 30;

/// Longest accepted peer id in bytes.
const MAX_PEER_ID_LEN: usize = 64;

/// `[peer_bridge]`.
///
/// # Examples
/// ```rust
/// use harw_config::PeerBridgeSection;
///
/// let section: PeerBridgeSection = toml::from_str(
///     "enabled = true\n[[peers]]\nid = \"codex\"\nprincipal = \"codex-mcp\"",
/// )
/// .expect("valid");
/// assert!(section.enabled);
/// assert_eq!(section.max_bytes, 16384);
/// assert!(section.peer("codex").is_some());
/// assert!(section.validate(&["codex-mcp"]).is_ok());
/// assert!(!PeerBridgeSection::default().enabled);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerBridgeSection {
    /// Switches the bridge on. Default `false`.
    #[serde(default)]
    pub enabled: bool,
    /// Maximum text bytes of one message (`1..=32768`). Default 16384.
    #[serde(default = "default_max_bytes")]
    pub max_bytes: u32,
    /// Maximum queued inbound messages per session (`1..=1024`). Default 64.
    #[serde(default = "default_queue_cap")]
    pub queue_cap: u32,
    /// Rotation size of outbox and spool files (`>= 65536`). Default 1 MiB.
    #[serde(default = "default_outbox_max_bytes")]
    pub outbox_max_bytes: u64,
    /// Non-loopback hosts a webhook connector may call.
    #[serde(default)]
    pub webhook_allow_hosts: Vec<String>,
    /// The configured peers.
    #[serde(default)]
    pub peers: Vec<PeerToml>,
}

/// One `[[peer_bridge.peers]]` entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerToml {
    /// Peer id: `[A-Za-z0-9_-]{1,64}`, unique.
    pub id: String,
    /// Id of the `[[mcp_listener.principals]]` entry that acts as this peer.
    pub principal: String,
    /// Enabled delivery connectors for messages from harw to the peer.
    #[serde(default)]
    pub connectors: Vec<PeerConnectorToml>,
    /// Webhook target (`http://` or `https://`); required by `webhook`.
    #[serde(default)]
    pub webhook_url: Option<String>,
    /// HMAC secret reference for the webhook; required by `webhook`.
    #[serde(default)]
    pub secret_ref: Option<SecretRef>,
    /// Whether inbound messages may wake an idle session. Default `true`.
    #[serde(default = "default_true")]
    pub wake: bool,
    /// Wake-up token bucket size (`>= 1`). Default 3.
    #[serde(default = "default_wake_burst")]
    pub wake_burst: u32,
    /// Seconds per refilled wake-up token (`>= 1`). Default 30.
    #[serde(default = "default_wake_interval_secs")]
    pub wake_interval_secs: u64,
    /// Sends allowed per 60 s window (`>= 1`). Default 30.
    #[serde(default = "default_max_sends_per_minute")]
    pub max_sends_per_minute: u32,
}

/// Delivery connector of a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeerConnectorToml {
    /// Append-only spool file watched with `tail -F`.
    Spool,
    /// Signed HTTP POST to `webhook_url`.
    Webhook,
    /// The peer polls via the `harw_peer_poll` MCP tool.
    Mcp,
}

fn default_true() -> bool {
    true
}

fn default_max_bytes() -> u32 {
    DEFAULT_PEER_MAX_BYTES
}

fn default_queue_cap() -> u32 {
    DEFAULT_PEER_QUEUE_CAP
}

fn default_outbox_max_bytes() -> u64 {
    DEFAULT_PEER_OUTBOX_MAX_BYTES
}

fn default_wake_burst() -> u32 {
    DEFAULT_PEER_WAKE_BURST
}

fn default_wake_interval_secs() -> u64 {
    DEFAULT_PEER_WAKE_INTERVAL_SECS
}

fn default_max_sends_per_minute() -> u32 {
    DEFAULT_PEER_MAX_SENDS_PER_MINUTE
}

impl Default for PeerBridgeSection {
    fn default() -> Self {
        Self {
            enabled: false,
            max_bytes: default_max_bytes(),
            queue_cap: default_queue_cap(),
            outbox_max_bytes: default_outbox_max_bytes(),
            webhook_allow_hosts: Vec::new(),
            peers: Vec::new(),
        }
    }
}

fn is_valid_peer_id(id: &str) -> bool {
    (1..=MAX_PEER_ID_LEN).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl PeerBridgeSection {
    /// Checks ranges and per-peer rules. `mcp_principal_ids` are the ids of
    /// `[[mcp_listener.principals]]`; peers must reference one of them, but
    /// only while the bridge is enabled.
    ///
    /// # Errors
    /// A message starting with `peer_bridge.` naming the offending field.
    pub fn validate(&self, mcp_principal_ids: &[&str]) -> Result<(), String> {
        if !(1..=MAX_PEER_MAX_BYTES).contains(&self.max_bytes) {
            return Err(format!(
                "peer_bridge.max_bytes must be within 1..={MAX_PEER_MAX_BYTES}"
            ));
        }
        if !(1..=MAX_PEER_QUEUE_CAP).contains(&self.queue_cap) {
            return Err(format!(
                "peer_bridge.queue_cap must be within 1..={MAX_PEER_QUEUE_CAP}"
            ));
        }
        if self.outbox_max_bytes < MIN_PEER_OUTBOX_MAX_BYTES {
            return Err(format!(
                "peer_bridge.outbox_max_bytes must be at least {MIN_PEER_OUTBOX_MAX_BYTES}"
            ));
        }
        if self
            .webhook_allow_hosts
            .iter()
            .any(|h| h.is_empty() || h.contains('/'))
        {
            return Err(
                "peer_bridge.webhook_allow_hosts entries must be non-empty hosts without '/'"
                    .to_owned(),
            );
        }
        for (index, peer) in self.peers.iter().enumerate() {
            peer.validate()?;
            if self.peers[..index].iter().any(|p| p.id == peer.id) {
                return Err(format!("peer_bridge.peers: duplicate id '{}'", peer.id));
            }
            if self.peers[..index].iter().any(|p| p.principal == peer.principal) {
                return Err(format!(
                    "peer_bridge.peers: duplicate principal '{}'",
                    peer.principal
                ));
            }
            if self.enabled && !mcp_principal_ids.contains(&peer.principal.as_str()) {
                return Err(format!(
                    "peer_bridge.peers: principal '{}' of peer '{}' is not in mcp_listener.principals",
                    peer.principal, peer.id
                ));
            }
        }
        Ok(())
    }

    /// The peer with this id.
    #[must_use]
    pub fn peer(&self, id: &str) -> Option<&PeerToml> {
        self.peers.iter().find(|p| p.id == id)
    }

    /// The peer bound to this MCP principal id.
    #[must_use]
    pub fn peer_for_principal(&self, principal: &str) -> Option<&PeerToml> {
        self.peers.iter().find(|p| p.principal == principal)
    }
}

impl PeerToml {
    /// Whether this peer has the given connector enabled.
    #[must_use]
    pub fn has_connector(&self, connector: PeerConnectorToml) -> bool {
        self.connectors.contains(&connector)
    }

    fn validate(&self) -> Result<(), String> {
        if !is_valid_peer_id(&self.id) {
            return Err(format!(
                "peer_bridge.peers: id '{}' must be 1..=64 chars of [A-Za-z0-9_-]",
                self.id
            ));
        }
        if self.principal.trim().is_empty() {
            return Err(format!(
                "peer_bridge.peers: peer '{}' needs a non-empty principal",
                self.id
            ));
        }
        for (index, connector) in self.connectors.iter().enumerate() {
            if self.connectors[..index].contains(connector) {
                return Err(format!(
                    "peer_bridge.peers: peer '{}' lists a connector twice",
                    self.id
                ));
            }
        }
        if self.has_connector(PeerConnectorToml::Webhook) {
            let url_ok = self
                .webhook_url
                .as_deref()
                .is_some_and(|u| u.starts_with("http://") || u.starts_with("https://"));
            if !url_ok {
                return Err(format!(
                    "peer_bridge.peers: peer '{}' webhook needs webhook_url starting with http:// or https://",
                    self.id
                ));
            }
            if self.secret_ref.is_none() {
                return Err(format!(
                    "peer_bridge.peers: peer '{}' webhook needs secret_ref",
                    self.id
                ));
            }
        }
        if self.wake_burst == 0 {
            return Err(format!(
                "peer_bridge.peers: peer '{}' wake_burst must be >= 1",
                self.id
            ));
        }
        if self.wake_interval_secs == 0 {
            return Err(format!(
                "peer_bridge.peers: peer '{}' wake_interval_secs must be >= 1",
                self.id
            ));
        }
        if self.max_sends_per_minute == 0 {
            return Err(format!(
                "peer_bridge.peers: peer '{}' max_sends_per_minute must be >= 1",
                self.id
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn peer(id: &str, principal: &str) -> PeerToml {
        PeerToml {
            id: id.to_owned(),
            principal: principal.to_owned(),
            connectors: vec![PeerConnectorToml::Spool],
            webhook_url: None,
            secret_ref: None,
            wake: true,
            wake_burst: DEFAULT_PEER_WAKE_BURST,
            wake_interval_secs: DEFAULT_PEER_WAKE_INTERVAL_SECS,
            max_sends_per_minute: DEFAULT_PEER_MAX_SENDS_PER_MINUTE,
        }
    }

    fn section() -> PeerBridgeSection {
        PeerBridgeSection {
            enabled: true,
            peers: vec![peer("codex", "codex-mcp")],
            ..PeerBridgeSection::default()
        }
    }

    const IDS: &[&str] = &["codex-mcp", "other-mcp"];

    fn assert_rejected(s: &PeerBridgeSection) -> TestResult {
        let err = s
            .validate(IDS)
            .err()
            .ok_or(TestError::Missing("validation error"))?;
        assert!(err.starts_with("peer_bridge."), "{err}");
        Ok(())
    }

    fn first_peer(s: &mut PeerBridgeSection) -> Result<&mut PeerToml, TestError> {
        s.peers.first_mut().ok_or(TestError::Missing("peer"))
    }

    #[test]
    fn test_defaults_match_serde_defaults() -> TestResult {
        let parsed: PeerBridgeSection = toml::from_str("").map_err(ctx("parse"))?;
        assert_eq!(parsed, PeerBridgeSection::default());
        assert!(!parsed.enabled);
        assert_eq!(parsed.max_bytes, 16384);
        assert_eq!(parsed.queue_cap, 64);
        assert_eq!(parsed.outbox_max_bytes, 1_048_576);
        assert!(parsed.peers.is_empty());
        assert!(parsed.validate(&[]).is_ok());
        let p: PeerToml =
            toml::from_str("id = \"a\"\nprincipal = \"b\"").map_err(ctx("parse peer"))?;
        assert!(p.wake);
        assert_eq!((p.wake_burst, p.wake_interval_secs), (3, 30));
        assert_eq!(p.max_sends_per_minute, 30);
        Ok(())
    }

    #[test]
    fn test_valid_section_passes() {
        assert!(section().validate(IDS).is_ok());
    }

    #[test]
    fn test_max_bytes_out_of_range() -> TestResult {
        for v in [0, 32769] {
            let mut s = section();
            s.max_bytes = v;
            assert_rejected(&s)?;
        }
        Ok(())
    }

    #[test]
    fn test_queue_cap_out_of_range() -> TestResult {
        for v in [0, 1025] {
            let mut s = section();
            s.queue_cap = v;
            assert_rejected(&s)?;
        }
        Ok(())
    }

    #[test]
    fn test_outbox_max_bytes_too_small() -> TestResult {
        let mut s = section();
        s.outbox_max_bytes = 65535;
        assert_rejected(&s)?;
        s.outbox_max_bytes = 65536;
        assert!(s.validate(IDS).is_ok());
        Ok(())
    }

    #[test]
    fn test_invalid_peer_ids_rejected() -> TestResult {
        for id in ["", "a b", "a/b", "ä", &"x".repeat(65)] {
            let mut s = section();
            first_peer(&mut s)?.id = id.to_owned();
            assert_rejected(&s)?;
        }
        Ok(())
    }

    #[test]
    fn test_duplicate_peer_id_rejected() -> TestResult {
        let mut s = section();
        s.peers.push(peer("codex", "other-mcp"));
        assert_rejected(&s)
    }

    #[test]
    fn test_empty_principal_rejected() -> TestResult {
        let mut s = section();
        first_peer(&mut s)?.principal = " ".to_owned();
        assert_rejected(&s)
    }

    #[test]
    fn test_duplicate_principal_rejected() -> TestResult {
        let mut s = section();
        s.peers.push(peer("second", "codex-mcp"));
        assert_rejected(&s)
    }

    #[test]
    fn test_duplicate_connector_rejected() -> TestResult {
        let mut s = section();
        first_peer(&mut s)?.connectors = vec![PeerConnectorToml::Mcp, PeerConnectorToml::Mcp];
        assert_rejected(&s)
    }

    #[test]
    fn test_webhook_requires_url_scheme_and_secret() -> TestResult {
        let mut s = section();
        {
            let p = first_peer(&mut s)?;
            p.connectors = vec![PeerConnectorToml::Webhook];
            p.secret_ref = Some(SecretRef::Env("HARW_PEER_SECRET".to_owned()));
        }
        assert_rejected(&s)?; // no url
        first_peer(&mut s)?.webhook_url = Some("ftp://example.invalid/hook".to_owned());
        assert_rejected(&s)?; // bad scheme
        {
            let p = first_peer(&mut s)?;
            p.webhook_url = Some("https://example.invalid/hook".to_owned());
            p.secret_ref = None;
        }
        assert_rejected(&s)?; // no secret
        first_peer(&mut s)?.secret_ref = Some(SecretRef::Env("HARW_PEER_SECRET".to_owned()));
        assert!(s.validate(IDS).is_ok());
        Ok(())
    }

    #[test]
    fn test_zero_rates_rejected() -> TestResult {
        let mut s = section();
        first_peer(&mut s)?.wake_burst = 0;
        assert_rejected(&s)?;
        let mut s = section();
        first_peer(&mut s)?.wake_interval_secs = 0;
        assert_rejected(&s)?;
        let mut s = section();
        first_peer(&mut s)?.max_sends_per_minute = 0;
        assert_rejected(&s)
    }

    #[test]
    fn test_allow_hosts_rules() -> TestResult {
        for host in ["", "example.invalid/path"] {
            let mut s = section();
            s.webhook_allow_hosts = vec![host.to_owned()];
            assert_rejected(&s)?;
        }
        let mut s = section();
        s.webhook_allow_hosts = vec!["hooks.example.invalid".to_owned()];
        assert!(s.validate(IDS).is_ok());
        Ok(())
    }

    #[test]
    fn test_unknown_principal_only_rejected_when_enabled() -> TestResult {
        let mut s = section();
        first_peer(&mut s)?.principal = "missing".to_owned();
        assert_rejected(&s)?;
        s.enabled = false;
        assert!(s.validate(IDS).is_ok());
        Ok(())
    }

    #[test]
    fn test_full_toml_parses_and_validates() -> TestResult {
        let src = r#"
enabled = true
webhook_allow_hosts = ["hooks.example.invalid"]

[[peers]]
id = "codex"
principal = "codex-mcp"
connectors = ["spool", "mcp"]

[[peers]]
id = "hook"
principal = "other-mcp"
connectors = ["webhook"]
webhook_url = "https://example.invalid/hook"
secret_ref = "env:HARW_PEER_SECRET"
wake = false
"#;
        let s: PeerBridgeSection = toml::from_str(src).map_err(ctx("parse"))?;
        s.validate(IDS).map_err(TestError::Unexpected)?;
        let codex = s.peer("codex").ok_or(TestError::Missing("codex"))?;
        assert!(codex.has_connector(PeerConnectorToml::Spool));
        assert!(codex.has_connector(PeerConnectorToml::Mcp));
        assert!(!codex.has_connector(PeerConnectorToml::Webhook));
        let hook = s.peer("hook").ok_or(TestError::Missing("hook"))?;
        assert!(hook.has_connector(PeerConnectorToml::Webhook));
        assert!(!hook.wake);
        assert_eq!(
            hook.secret_ref,
            Some(SecretRef::Env("HARW_PEER_SECRET".to_owned()))
        );
        Ok(())
    }

    #[test]
    fn test_unknown_fields_rejected() {
        assert!(toml::from_str::<PeerBridgeSection>("bogus = 1").is_err());
        let peer_src = "[[peers]]\nid = \"a\"\nprincipal = \"b\"\nbogus = 1";
        assert!(toml::from_str::<PeerBridgeSection>(peer_src).is_err());
    }

    #[test]
    fn test_peer_lookups() -> TestResult {
        let mut s = section();
        s.peers.push(peer("second", "other-mcp"));
        assert_eq!(
            s.peer("second").map(|p| p.principal.as_str()),
            Some("other-mcp")
        );
        assert_eq!(
            s.peer_for_principal("codex-mcp").map(|p| p.id.as_str()),
            Some("codex")
        );
        assert!(s.peer("nope").is_none());
        assert!(s.peer_for_principal("nope").is_none());
        Ok(())
    }
}
