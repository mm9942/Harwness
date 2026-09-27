//! Topology records: nodes, zones and routes.
//!
//! # Responsibility
//! Plain data plus field validation. NetSec answers *where* a node is and
//! *through which zone/route* it is reachable (masterplan §18). It never
//! answers *who* a node is: `identity_key_ref` is an opaque reference to a
//! key held by the Auth/Crypto Hub, never key material, and no field here is
//! ever used to infer identity from an address.

use std::fmt;
use std::net::SocketAddr;
use std::str::FromStr;

use harw_types::NodeId;
use jiff::Timestamp;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{NetsecError, NetsecResult};
use crate::ids::{RouteId, ZoneId};
use crate::state_machine::{NodeEvent, NodeState};

/// Maximum length of a display name in characters.
pub const MAX_DISPLAY_NAME_CHARS: usize = 128;
/// Maximum number of addresses per node.
pub const MAX_ADDRESSES: usize = 16;
/// Maximum length of an identity key reference in bytes.
pub const MAX_KEY_REF_LEN: usize = 256;
/// Maximum length of a transition reason in characters.
pub const MAX_REASON_CHARS: usize = 512;
/// Maximum length of a host name in an address in bytes (RFC 1035 limit).
const MAX_HOST_LEN: usize = 253;

/// Validates a human-readable text field: not blank, bounded, no control
/// characters.
///
/// # Errors
/// [`NetsecError::InvalidField`] naming `field`.
pub fn check_text(field: &'static str, value: &str, max_chars: usize) -> NetsecResult<()> {
    if value.trim().is_empty() {
        return Err(NetsecError::InvalidField {
            field,
            reason: "must not be blank",
        });
    }
    if value.chars().count() > max_chars {
        return Err(NetsecError::InvalidField {
            field,
            reason: "too long",
        });
    }
    if value.chars().any(char::is_control) {
        return Err(NetsecError::InvalidField {
            field,
            reason: "must not contain control characters",
        });
    }
    Ok(())
}

/// A network address at which a node may be reached.
///
/// On the wire it is a plain string: either a socket address
/// (`10.0.0.7:7443`, `[fd00::7]:7443`) or `host:port` with a DNS-style host
/// name. Reachability only — never evidence of identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeAddress {
    /// A literal IP socket address.
    Socket(SocketAddr),
    /// A DNS host name plus port.
    Host {
        /// Host name (`[A-Za-z0-9.-]`, at most 253 bytes).
        host: String,
        /// TCP/UDP port.
        port: u16,
    },
}

impl FromStr for NodeAddress {
    type Err = NetsecError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Ok(socket) = value.parse::<SocketAddr>() {
            return Ok(Self::Socket(socket));
        }
        let invalid = NetsecError::InvalidField {
            field: "addresses",
            reason: "expected `ip:port`, `[ipv6]:port` or `host:port`",
        };
        let Some((host, port)) = value.rsplit_once(':') else {
            return Err(invalid);
        };
        let Ok(port) = port.parse::<u16>() else {
            return Err(invalid);
        };
        let host_ok = !host.is_empty()
            && host.len() <= MAX_HOST_LEN
            && !host.starts_with(['.', '-'])
            && host
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'));
        if !host_ok {
            return Err(invalid);
        }
        Ok(Self::Host {
            host: host.to_ascii_lowercase(),
            port,
        })
    }
}

impl fmt::Display for NodeAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Socket(socket) => write!(f, "{socket}"),
            Self::Host { host, port } => write!(f, "{host}:{port}"),
        }
    }
}

impl Serialize for NodeAddress {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for NodeAddress {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

/// Audit trail of the most recent state change of a node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransitionRecord {
    /// State before the event.
    pub from: NodeState,
    /// State after the event.
    pub to: NodeState,
    /// The applied event.
    pub event: NodeEvent,
    /// When it was applied (daemon clock).
    pub at: Timestamp,
    /// Kernel-verified uid (`SO_PEERCRED`) of the requesting peer; `None`
    /// for library callers without a socket peer.
    #[serde(default)]
    pub by_uid: Option<u32>,
    /// Optional operator-supplied reason.
    #[serde(default)]
    pub reason: Option<String>,
}

/// One node in the local topology.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeRecord {
    /// Node identity reference (shared `harw-types` vocabulary).
    pub id: NodeId,
    /// Operator-facing name.
    pub display_name: String,
    /// Where the node may be reached. Topology only, never identity.
    pub addresses: Vec<NodeAddress>,
    /// Trust zone the node is placed in.
    pub zone: ZoneId,
    /// Lifecycle state (see [`crate::state_machine`]).
    pub state: NodeState,
    /// Reference to the node's identity key in the Auth/Crypto Hub. Never
    /// key material.
    #[serde(default)]
    pub identity_key_ref: Option<String>,
    /// Registration time.
    pub joined_at: Timestamp,
    /// Last heartbeat; `None` until the node has been seen.
    #[serde(default)]
    pub last_seen: Option<Timestamp>,
    /// Most recent state transition.
    #[serde(default)]
    pub last_transition: Option<TransitionRecord>,
}

impl NodeRecord {
    /// Validates every field of a record (used on registration and when a
    /// state file is loaded).
    ///
    /// # Errors
    /// [`NetsecError::InvalidIdentifier`] or [`NetsecError::InvalidField`].
    pub fn validate(&self) -> NetsecResult<()> {
        crate::ids::check_node_id(&self.id)?;
        check_text("display_name", &self.display_name, MAX_DISPLAY_NAME_CHARS)?;
        if self.addresses.len() > MAX_ADDRESSES {
            return Err(NetsecError::InvalidField {
                field: "addresses",
                reason: "too many addresses",
            });
        }
        if let Some(key_ref) = &self.identity_key_ref {
            check_key_ref(key_ref)?;
        }
        let reason = self
            .last_transition
            .as_ref()
            .and_then(|transition| transition.reason.as_deref());
        if let Some(reason) = reason {
            check_text("reason", reason, MAX_REASON_CHARS)?;
        }
        Ok(())
    }
}

/// Validates an identity key reference: printable ASCII, no spaces, bounded.
///
/// # Errors
/// [`NetsecError::InvalidField`].
pub fn check_key_ref(value: &str) -> NetsecResult<()> {
    if value.is_empty()
        || value.len() > MAX_KEY_REF_LEN
        || !value.bytes().all(|byte| byte.is_ascii_graphic())
    {
        return Err(NetsecError::InvalidField {
            field: "identity_key_ref",
            reason: "must be 1..=256 printable ASCII characters without spaces",
        });
    }
    Ok(())
}

/// Input of a node registration (`POST /v1/nodes`). A registered node
/// starts in [`NodeState::Pending`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeRegistration {
    /// Node id (token rule, see [`crate::ids`]).
    pub id: NodeId,
    /// Operator-facing name.
    pub display_name: String,
    /// Reachability addresses.
    #[serde(default)]
    pub addresses: Vec<NodeAddress>,
    /// Zone the node is placed in; must be configured.
    pub zone: ZoneId,
    /// Reference to the node's identity key in the Auth/Crypto Hub.
    #[serde(default)]
    pub identity_key_ref: Option<String>,
}

impl NodeRegistration {
    /// Builds the pending record for this registration.
    ///
    /// # Errors
    /// Any validation error of [`NodeRecord::validate`].
    pub fn into_record(self, now: Timestamp) -> NetsecResult<NodeRecord> {
        let record = NodeRecord {
            id: self.id,
            display_name: self.display_name,
            addresses: self.addresses,
            zone: self.zone,
            state: NodeState::Pending,
            identity_key_ref: self.identity_key_ref,
            joined_at: now,
            last_seen: None,
            last_transition: None,
        };
        record.validate()?;
        Ok(record)
    }
}

/// A trust zone. Zones are declared in the daemon configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Zone {
    /// Zone id.
    pub id: ZoneId,
    /// Operator-facing name.
    pub display_name: String,
}

impl Zone {
    /// Validates the zone.
    ///
    /// # Errors
    /// [`NetsecError::InvalidField`].
    pub fn validate(&self) -> NetsecResult<()> {
        check_text("display_name", &self.display_name, MAX_DISPLAY_NAME_CHARS)
    }
}

/// A route: how `target` is reached through `zone`, optionally via a relay
/// node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    /// Route id.
    pub id: RouteId,
    /// The node this route leads to.
    pub target: NodeId,
    /// The zone the route runs through.
    pub zone: ZoneId,
    /// Optional relay node.
    #[serde(default)]
    pub via: Option<NodeId>,
    /// Preference; lower is preferred.
    pub metric: u32,
}

impl Route {
    /// Whether the route references `node` as target or relay.
    #[must_use]
    pub fn references(&self, node: &NodeId) -> bool {
        &self.target == node || self.via.as_ref() == Some(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    #[test]
    fn test_address_parses_socket_and_host_forms() -> TestResult {
        let v4: NodeAddress = "10.0.0.7:7443".parse()?;
        assert!(matches!(v4, NodeAddress::Socket(_)));
        let v6: NodeAddress = "[fd00::7]:7443".parse()?;
        assert!(matches!(v6, NodeAddress::Socket(_)));
        let host: NodeAddress = "Node-7.Fleet.example:443".parse()?;
        assert_eq!(host.to_string(), "node-7.fleet.example:443");
        Ok(())
    }

    #[test]
    fn test_address_rejects_garbage() {
        for value in [
            "",
            "host",
            ":443",
            "host:",
            "host:99999",
            "ho st:1",
            "a/b:1",
            "-x:1",
            "fd00::7",
        ] {
            assert!(value.parse::<NodeAddress>().is_err(), "{value:?}");
        }
    }

    #[test]
    fn test_address_serializes_as_plain_string() -> TestResult {
        let address: NodeAddress = "10.0.0.7:7443".parse()?;
        assert_eq!(serde_json::to_string(&address)?, "\"10.0.0.7:7443\"");
        let back: NodeAddress = serde_json::from_str("\"10.0.0.7:7443\"")?;
        assert_eq!(back, address);
        Ok(())
    }

    #[test]
    fn test_check_text_rules() {
        assert!(check_text("f", "ok", 8).is_ok());
        assert!(check_text("f", "  ", 8).is_err());
        assert!(check_text("f", "123456789", 8).is_err());
        assert!(check_text("f", "a\u{7}b", 8).is_err());
    }

    #[test]
    fn test_key_ref_rules() {
        assert!(check_key_ref("authhub:node/7#v1").is_ok());
        assert!(check_key_ref("").is_err());
        assert!(check_key_ref("has space").is_err());
        assert!(check_key_ref(&"k".repeat(MAX_KEY_REF_LEN + 1)).is_err());
    }

    #[test]
    fn test_node_record_rejects_unknown_fields() {
        let json = r#"{"id":"n1","display_name":"n","addresses":[],"zone":"local",
            "state":"pending","joined_at":"2026-01-01T00:00:00Z","secret":"x"}"#;
        assert!(serde_json::from_str::<NodeRecord>(json).is_err());
    }
}
