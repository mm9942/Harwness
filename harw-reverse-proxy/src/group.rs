//! Upstream groups: admission, deterministic weighted selection and retry rules.
//!
//! Pure and deterministic: the caller supplies the selection key (a request
//! counter for round robin, a hash for stateless stickiness). **Not for session
//! traffic.** A hosted session has exactly one owner node; session routes
//! resolve `SessionId -> owner` through the Cloud Home directory and fail closed
//! (hub plan sections 5 and 5.1). Selecting among backends for a session would
//! risk a second writer.

use std::collections::HashSet;
use std::fmt;
use std::net::SocketAddr;

use harw_netsec::NodeState;

use crate::route::UpstreamScope;

/// One backend of a group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backend {
    /// Literal address.
    pub addr: SocketAddr,
    /// Relative weight, 1..=1000.
    pub weight: u32,
    /// Lifecycle state of the node behind it (`harw-netsec`).
    pub node: NodeState,
    /// Result of the latest health check. Health never overrides node state.
    pub healthy: bool,
}

impl Backend {
    /// Only an `Active` node that passed its health check receives new traffic.
    /// `Pending` is not confirmed, `Draining` and `Drained` take no new traffic,
    /// `Revoked` is gone.
    #[must_use]
    pub fn routable(&self) -> bool {
        self.healthy && self.node == NodeState::Active
    }
}

/// Why a group was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupError {
    /// No backends.
    Empty,
    /// Address class not admitted by the group scope.
    NotAdmitted(SocketAddr),
    /// Same address twice.
    Duplicate(SocketAddr),
    /// Weight outside 1..=1000.
    BadWeight(SocketAddr),
    /// Port 0.
    BadPort(SocketAddr),
}

impl fmt::Display for GroupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("group has no backends"),
            Self::NotAdmitted(a) => write!(f, "backend {a} is not admitted by the group scope"),
            Self::Duplicate(a) => write!(f, "backend {a} appears twice"),
            Self::BadWeight(a) => write!(f, "backend {a}: weight must be 1..=1000"),
            Self::BadPort(a) => write!(f, "backend {a}: port must not be 0"),
        }
    }
}

impl std::error::Error for GroupError {}

/// A validated group of interchangeable backends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamGroup {
    backends: Vec<Backend>,
    scope: UpstreamScope,
}

impl UpstreamGroup {
    /// Validate the backends against `scope` (address class via
    /// `harw_egress::classify`), uniqueness, weights and ports.
    pub fn new(scope: UpstreamScope, backends: Vec<Backend>) -> Result<Self, GroupError> {
        if backends.is_empty() {
            return Err(GroupError::Empty);
        }
        let mut seen = HashSet::new();
        for b in &backends {
            if b.addr.port() == 0 {
                return Err(GroupError::BadPort(b.addr));
            }
            if !(1..=1000).contains(&b.weight) {
                return Err(GroupError::BadWeight(b.addr));
            }
            if !scope.admits_addr(b.addr) {
                return Err(GroupError::NotAdmitted(b.addr));
            }
            if !seen.insert(b.addr) {
                return Err(GroupError::Duplicate(b.addr));
            }
        }
        Ok(Self { backends, scope })
    }

    /// The group's scope.
    #[must_use]
    pub fn scope(&self) -> UpstreamScope {
        self.scope
    }

    /// All backends.
    #[must_use]
    pub fn backends(&self) -> &[Backend] {
        &self.backends
    }

    /// Record a node lifecycle change. `false` if the address is unknown.
    pub fn set_node_state(&mut self, addr: SocketAddr, state: NodeState) -> bool {
        self.backends
            .iter_mut()
            .find(|b| b.addr == addr)
            .map(|b| b.node = state)
            .is_some()
    }

    /// Record a health-check result. `false` if the address is unknown.
    pub fn set_health(&mut self, addr: SocketAddr, healthy: bool) -> bool {
        self.backends
            .iter_mut()
            .find(|b| b.addr == addr)
            .map(|b| b.healthy = healthy)
            .is_some()
    }

    /// Addresses whose node is `Revoked`: existing connections to them must be
    /// cut, not merely left to drain.
    #[must_use]
    pub fn connections_to_cut(&self) -> Vec<SocketAddr> {
        self.backends
            .iter()
            .filter(|b| b.node == NodeState::Revoked)
            .map(|b| b.addr)
            .collect()
    }

    /// Weighted selection among routable backends. The same `key` and state
    /// always give the same backend. Round robin: pass an increasing counter.
    /// Returns `None` when no backend is routable (the caller answers 503; it
    /// does not fall back to an unadmitted backend).
    #[must_use]
    pub fn select(&self, key: u64) -> Option<SocketAddr> {
        self.select_excluding(key, &[])
    }

    /// Like [`select`](Self::select) but skipping `tried` (bounded retry).
    #[must_use]
    pub fn select_excluding(&self, key: u64, tried: &[SocketAddr]) -> Option<SocketAddr> {
        let candidates: Vec<&Backend> = self
            .backends
            .iter()
            .filter(|b| b.routable() && !tried.contains(&b.addr))
            .collect();
        let total: u64 = candidates.iter().map(|b| u64::from(b.weight)).sum();
        if total == 0 {
            return None;
        }
        let mut slot = key % total;
        for b in candidates {
            let w = u64::from(b.weight);
            if slot < w {
                return Some(b.addr);
            }
            slot -= w;
        }
        None
    }
}

/// Whether a failed attempt may be retried on another backend: only `GET`,
/// `HEAD` and `OPTIONS` (deliberately narrower than "idempotent" in the RFC),
/// never an upgrade, never once any request body byte was sent, and at most
/// `max_attempts` attempts in total.
#[must_use]
pub fn may_retry(
    method: &str,
    body_started: bool,
    is_upgrade: bool,
    attempts_so_far: u32,
    max_attempts: u32,
) -> bool {
    matches!(method, "GET" | "HEAD" | "OPTIONS")
        && !body_started
        && !is_upgrade
        && attempts_so_far < max_attempts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ensure};

    fn addr(s: &str) -> Result<SocketAddr, std::net::AddrParseError> {
        s.parse()
    }

    fn be(a: &str, weight: u32) -> Result<Backend, std::net::AddrParseError> {
        Ok(Backend {
            addr: addr(a)?,
            weight,
            node: NodeState::Active,
            healthy: true,
        })
    }

    fn group(backends: Vec<Backend>) -> Result<UpstreamGroup, GroupError> {
        UpstreamGroup::new(UpstreamScope::Loopback, backends)
    }

    #[test]
    fn selection_follows_the_weights_over_a_full_cycle() -> TestResult {
        let g = group(vec![be("127.0.0.1:1", 1)?, be("127.0.0.1:2", 3)?])?;
        let mut counts = [0u32; 2];
        for key in 0..4 {
            match g.select(key) {
                Some(a) if a.port() == 1 => counts[0] += 1,
                Some(a) if a.port() == 2 => counts[1] += 1,
                other => return Err(TestError(format!("unexpected {other:?}"))),
            }
        }
        ensure(
            counts == [1, 3],
            "weights 1:3 over one cycle of total weight 4",
        )?;
        ensure(g.select(7) == g.select(7), "deterministic for a given key")
    }

    #[test]
    fn only_active_and_healthy_backends_are_routable() -> TestResult {
        // Exhaustive over the lifecycle: a new NodeState forces a decision here.
        for state in NodeState::ALL {
            let b = Backend {
                addr: addr("127.0.0.1:1")?,
                weight: 1,
                node: state,
                healthy: true,
            };
            let expected = match state {
                NodeState::Active => true,
                NodeState::Pending
                | NodeState::Draining
                | NodeState::Drained
                | NodeState::Revoked => false,
            };
            ensure(b.routable() == expected, state.as_str())?;
        }
        let unhealthy = Backend {
            addr: addr("127.0.0.1:1")?,
            weight: 1,
            node: NodeState::Active,
            healthy: false,
        };
        ensure(!unhealthy.routable(), "unhealthy is not routable")
    }

    #[test]
    fn non_routable_backends_never_get_traffic() -> TestResult {
        let mut g = group(vec![
            be("127.0.0.1:1", 1)?,
            be("127.0.0.1:2", 1)?,
            be("127.0.0.1:3", 1)?,
        ])?;
        ensure(
            g.set_node_state(addr("127.0.0.1:1")?, NodeState::Draining),
            "known address",
        )?;
        ensure(g.set_health(addr("127.0.0.1:2")?, false), "known address")?;
        for key in 0..50 {
            ensure(
                g.select(key) == Some(addr("127.0.0.1:3")?),
                "only the active, healthy backend",
            )?;
        }
        ensure(
            !g.set_node_state(addr("127.0.0.1:9")?, NodeState::Active),
            "unknown address",
        )
    }

    #[test]
    fn nothing_routable_means_none_not_a_fallback() -> TestResult {
        let mut g = group(vec![be("127.0.0.1:1", 1)?])?;
        g.set_node_state(addr("127.0.0.1:1")?, NodeState::Revoked);
        ensure(g.select(0).is_none(), "no routable backend")?;
        ensure(
            g.connections_to_cut() == vec![addr("127.0.0.1:1")?],
            "revoked connections are cut",
        )
    }

    #[test]
    fn health_does_not_override_revocation() -> TestResult {
        let mut g = group(vec![be("127.0.0.1:1", 1)?])?;
        g.set_node_state(addr("127.0.0.1:1")?, NodeState::Revoked);
        g.set_health(addr("127.0.0.1:1")?, true);
        ensure(
            g.select(0).is_none(),
            "a healthy but revoked node is not routable",
        )
    }

    #[test]
    fn retry_selection_skips_tried_backends() -> TestResult {
        let g = group(vec![be("127.0.0.1:1", 1)?, be("127.0.0.1:2", 1)?])?;
        let first = g.select(0).ok_or_else(|| TestError("none".into()))?;
        let second = g
            .select_excluding(0, &[first])
            .ok_or_else(|| TestError("none".into()))?;
        ensure(first != second, "a different backend")?;
        ensure(
            g.select_excluding(0, &[first, second]).is_none(),
            "all tried",
        )
    }

    #[test]
    fn groups_are_validated() -> TestResult {
        ensure(group(vec![]) == Err(GroupError::Empty), "empty")?;
        ensure(
            matches!(
                group(vec![be("127.0.0.1:1", 0)?]),
                Err(GroupError::BadWeight(_))
            ),
            "weight 0",
        )?;
        ensure(
            matches!(
                group(vec![be("127.0.0.1:1", 1001)?]),
                Err(GroupError::BadWeight(_))
            ),
            "weight above 1000",
        )?;
        ensure(
            matches!(
                group(vec![be("127.0.0.1:0", 1)?]),
                Err(GroupError::BadPort(_))
            ),
            "port 0",
        )?;
        ensure(
            matches!(
                group(vec![be("127.0.0.1:1", 1)?, be("127.0.0.1:1", 2)?]),
                Err(GroupError::Duplicate(_))
            ),
            "duplicate",
        )?;
        for a in [
            "169.254.169.254:80",
            "10.0.0.1:80",
            "8.8.8.8:80",
            "0.0.0.0:80",
        ] {
            ensure(
                matches!(group(vec![be(a, 1)?]), Err(GroupError::NotAdmitted(_))),
                a,
            )?;
        }
        ensure(
            UpstreamGroup::new(UpstreamScope::Private, vec![be("10.0.0.1:80", 1)?]).is_ok(),
            "private under private scope",
        )?;
        ensure(
            matches!(
                UpstreamGroup::new(UpstreamScope::Public, vec![be("169.254.169.254:80", 1)?]),
                Err(GroupError::NotAdmitted(_))
            ),
            "metadata is never admitted",
        )
    }

    #[test]
    fn retry_rules_are_conservative() -> TestResult {
        ensure(may_retry("GET", false, false, 0, 2), "get")?;
        ensure(
            may_retry("HEAD", false, false, 1, 2),
            "head, second attempt",
        )?;
        ensure(!may_retry("GET", false, false, 2, 2), "bounded")?;
        for m in ["POST", "PUT", "DELETE", "PATCH", "CONNECT"] {
            ensure(!may_retry(m, false, false, 0, 3), m)?;
        }
        ensure(!may_retry("GET", true, false, 0, 3), "body started")?;
        ensure(!may_retry("GET", false, true, 0, 3), "upgrade")
    }
}
