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
//! The optional Tor onion service is only a transport and adds no authority.

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
/// assert!(!section.onion);
/// assert_eq!(section.share_profile_sessions, "off");
/// assert_eq!(section.share_legacy_rows, 100);
/// assert!(section.share_tenant.is_none() && section.share_roots.is_empty());
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
    /// Enrolled device authorized to manage this gateway’s ChatGPT account.
    #[serde(default)]
    pub provider_link_device: Option<String>,
    /// Bind the requested local session socket to this tenant on the remote host.
    #[serde(default)]
    pub shared_socket_tenant: Option<String>,
    /// Also publish this listener as a Tor onion service (embedded arti, in-process; needs a harw build with the `onion` cargo feature). Tor is only a transport: the node handshake and node-devices.conf stay the only authority. Default `false`.
    #[serde(default)]
    pub onion: bool,
    /// Absolute directory for the onion service's Tor state (the persistent onion identity, i.e. a stable .onion address), cache and the public `locator` file. `None`: `<profile>/session-host/onion`.
    #[serde(default)]
    pub onion_dir: Option<String>,
    /// `"off" | "read" | "continue"`. Default `"off"`. Whether the gateway
    /// shares the profile's TUI sessions with enrolled devices.
    #[serde(default = "default_share_profile_sessions")]
    pub share_profile_sessions: String,
    /// Tenant of the shared profile rows. Required when
    /// `share_profile_sessions` is not `"off"`.
    #[serde(default)]
    pub share_tenant: Option<String>,
    /// Number of profile rows shown to legacy (wire minor 2) clients. Default 100.
    #[serde(default = "default_share_legacy_rows")]
    pub share_legacy_rows: u32,
    /// `YYYY-MM-DD`: share only sessions newer than this date.
    #[serde(default)]
    pub share_since: Option<String>,
    /// Absolute paths that narrow the allowed working directories.
    #[serde(default)]
    pub share_roots: Vec<String>,
    /// Absolute directory under which a remote client may create new projects as
    /// `<dir>/<slug>` (only with `share_profile_sessions = "continue"`). `None` = off.
    #[serde(default)]
    pub share_new_project_root: Option<String>,
}

/// Default for `share_profile_sessions`.
pub const DEFAULT_SHARE_PROFILE_SESSIONS: &str = "off";
/// Default for `share_legacy_rows`.
pub const DEFAULT_SHARE_LEGACY_ROWS: u32 = 100;

fn default_share_profile_sessions() -> String {
    DEFAULT_SHARE_PROFILE_SESSIONS.to_owned()
}

fn default_share_legacy_rows() -> u32 {
    DEFAULT_SHARE_LEGACY_ROWS
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
            provider_link_device: None,
            shared_socket_tenant: None,
            onion: false,
            onion_dir: None,
            share_profile_sessions: default_share_profile_sessions(),
            share_tenant: None,
            share_legacy_rows: default_share_legacy_rows(),
            share_since: None,
            share_roots: Vec::new(),
            share_new_project_root: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_defaults_are_off() {
        let d = SessionListenerSection::default();
        assert_eq!(d.share_profile_sessions, "off");
        assert_eq!(d.share_legacy_rows, 100);
        assert!(d.share_tenant.is_none() && d.share_since.is_none());
        assert!(d.share_roots.is_empty());
        let parsed: SessionListenerSection = toml::from_str("").expect("empty parses");
        assert_eq!(parsed, d);
    }

    #[test]
    fn share_keys_parse() {
        let toml = "share_profile_sessions = \"read\"\nshare_tenant = \"tenant-x\"\n\
                    share_legacy_rows = 5\nshare_since = \"2026-01-01\"\n\
                    share_roots = [\"/srv/a\"]";
        let s: SessionListenerSection = toml::from_str(toml).expect("valid");
        assert_eq!(s.share_profile_sessions, "read");
        assert_eq!(s.share_tenant.as_deref(), Some("tenant-x"));
        assert_eq!(s.share_legacy_rows, 5);
        assert_eq!(s.share_since.as_deref(), Some("2026-01-01"));
        assert_eq!(s.share_roots, vec!["/srv/a".to_owned()]);
    }

    #[test]
    fn unknown_key_still_rejected() {
        assert!(toml::from_str::<SessionListenerSection>("share_bogus = 1").is_err());
    }
}
