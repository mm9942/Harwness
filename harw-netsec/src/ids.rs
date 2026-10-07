//! NetSec-local identifiers and the stricter token rule for node ids.
//!
//! # Responsibility
//! `NodeId` is shared vocabulary and lives in `harw-types`; its own rule only
//! rejects blank values. NetSec addresses nodes inside URL paths
//! (`/v1/nodes/{id}:drain`) and as JSON keys, so every id it accepts must
//! additionally be a *token*: 1..=128 bytes of `[A-Za-z0-9._-]`, not starting
//! with `.`. That keeps `/`, `:`, `%`, whitespace and control characters out
//! of paths and makes percent-decoding unnecessary.
//!
//! `ZoneId` and `RouteId` are NetSec-owned (no other crate names zones or
//! routes yet) and enforce the token rule in their constructor and in
//! `Deserialize`, so an invalid value cannot exist in memory.

use harw_macros::HarwId;
use harw_types::NodeId;
use serde::{Deserialize, Deserializer, Serialize};

use crate::error::{NetsecError, NetsecResult};

/// Maximum length of any NetSec identifier in bytes.
pub const MAX_ID_LEN: usize = 128;

/// Whether `value` satisfies the NetSec token rule.
#[must_use]
pub fn is_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_LEN
        && !value.starts_with('.')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// Parses a node id from untrusted input (URL path segment, JSON).
///
/// # Errors
/// [`NetsecError::InvalidIdentifier`] if the value is blank or not a token.
pub fn parse_node_id(value: &str) -> NetsecResult<NodeId> {
    let id = NodeId::parse(value).map_err(|_| NetsecError::invalid_identifier("node", value))?;
    check_node_id(&id)?;
    Ok(id)
}

/// Checks an already constructed `NodeId` against the token rule.
///
/// # Errors
/// [`NetsecError::InvalidIdentifier`] if the id is not a token.
pub fn check_node_id(id: &NodeId) -> NetsecResult<()> {
    if is_token(id.as_str()) {
        Ok(())
    } else {
        Err(NetsecError::invalid_identifier("node", id.as_str()))
    }
}

/// Token-rule check for [`ZoneId`], used by `#[derive(HarwId)]`.
fn validate_zone_id(value: &str) -> NetsecResult<()> {
    check_token("zone", value)
}

/// Token-rule check for [`RouteId`], used by `#[derive(HarwId)]`.
fn validate_route_id(value: &str) -> NetsecResult<()> {
    check_token("route", value)
}

fn check_token(kind: &'static str, value: &str) -> NetsecResult<()> {
    if is_token(value) {
        Ok(())
    } else {
        Err(NetsecError::invalid_identifier(kind, value))
    }
}

macro_rules! netsec_id {
    ($(#[$doc:meta])* $name:ident, $validate:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, HarwId)]
        #[serde(transparent)]
        #[harw_id(error = "crate::error::NetsecError", validate = $validate)]
        pub struct $name(String);

        impl $name {
            /// Parses and validates an identifier.
            ///
            /// # Errors
            /// [`NetsecError::InvalidIdentifier`] if `value` is not a token.
            pub fn parse(value: impl Into<String>) -> NetsecResult<Self> {
                Self::try_new(value)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::parse(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

netsec_id!(
    /// Identifies a network trust zone (e.g. `local`, `lan`, `dmz`).
    ZoneId,
    "validate_zone_id"
);
netsec_id!(
    /// Identifies one route entry in the topology.
    RouteId,
    "validate_route_id"
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    #[test]
    fn test_token_rule_accepts_plain_ids() {
        for value in ["node-1", "a", "zone_A.b", "0"] {
            assert!(is_token(value), "{value}");
        }
    }

    #[test]
    fn test_token_rule_rejects_path_and_control_characters() {
        let long = "x".repeat(MAX_ID_LEN + 1);
        for value in [
            "", " ", "a/b", "a:b", "a%2F", "a b", "a\nb", ".hidden", "ü", &long,
        ] {
            assert!(!is_token(value), "{value:?}");
        }
    }

    #[test]
    fn test_parse_node_id_rejects_blank_and_colon() {
        assert!(parse_node_id("").is_err());
        assert!(parse_node_id("node:drain").is_err());
        assert!(parse_node_id("node-7").is_ok());
    }

    #[test]
    fn test_zone_id_deserialize_enforces_rule() -> TestResult {
        let zone: ZoneId = serde_json::from_str("\"lan\"")?;
        assert_eq!(zone.as_str(), "lan");
        assert!(serde_json::from_str::<ZoneId>("\"../etc\"").is_err());
        assert!(serde_json::from_str::<RouteId>("\"\"").is_err());
        Ok(())
    }

    #[test]
    fn test_invalid_identifier_echo_is_truncated() {
        let long = "/".repeat(500);
        match ZoneId::parse(long) {
            Err(NetsecError::InvalidIdentifier { value, .. }) => assert!(value.len() <= 64),
            other => assert!(other.is_err()),
        }
    }
}
