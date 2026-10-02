//! `[session_listener]` — the own-cloud (remote) session ingress of
//! `harw gateway`: a TCP listener behind the mutual node-transport handshake.
//!
//! Every field is **global-only** (home layer): a profile or an untrusted
//! project can neither enable the listener nor move it, change the node
//! identity or raise the tier. Default: off.
//!
//! Enforcement lives in `harw-cli` (`session_listener.rs`); this module is
//! declarative. Operator-written files next to the host state
//! (`<profile>/session-host/remote/`): `node-devices.conf` (enrolled devices,
//! tenant, tier, approval opt-in) and `node-peers.conf` (pinned node keys).

use serde::{Deserialize, Serialize};

/// Default listen address: loopback only.
pub const DEFAULT_SESSION_LISTENER_ADDR: &str = "127.0.0.1:7443";

/// `[session_listener]`.
///
/// # Examples
/// ```rust
/// use harw_config::SessionListenerSection;
///
/// let section: SessionListenerSection =
///     toml::from_str("enabled = true\nnode_id = \"gateway-a\"").expect("valid");
/// assert!(section.enabled);
/// assert_eq!(section.listen, "127.0.0.1:7443");
/// assert_eq!(section.tier, "observer");
/// assert!(!SessionListenerSection::default().enabled);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionListenerSection {
    /// Starts the listener with `harw gateway`. Default `false`.
    #[serde(default)]
    pub enabled: bool,
    /// `host:port` to bind. A non-loopback address additionally needs
    /// [`Self::allow_non_loopback`].
    #[serde(default = "default_listen")]
    pub listen: String,
    /// Allows a non-loopback bind. Default `false`.
    #[serde(default)]
    pub allow_non_loopback: bool,
    /// This gateway's node id: names the AuthHub key
    /// `harw.node-identity/<node_id>`. Required when enabled.
    #[serde(default)]
    pub node_id: Option<String>,
    /// Tier the remote sessions' sandbox rights are cut to
    /// (`observer|operator|maintainer|owner`). Default `observer`.
    #[serde(default = "default_tier")]
    pub tier: String,
    /// The one enrolled device allowed to resolve approvals of remote
    /// sessions (it also needs the `approve` opt-in in `node-devices.conf`).
    /// `None`: approvals park.
    #[serde(default)]
    pub approval_device: Option<String>,
}

fn default_listen() -> String {
    DEFAULT_SESSION_LISTENER_ADDR.to_owned()
}

fn default_tier() -> String {
    "observer".to_owned()
}

impl Default for SessionListenerSection {
    fn default() -> Self {
        Self {
            enabled: false,
            listen: default_listen(),
            allow_non_loopback: false,
            node_id: None,
            tier: default_tier(),
            approval_device: None,
        }
    }
}
