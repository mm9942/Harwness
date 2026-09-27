//! Node lifecycle state machine.
//!
//! # Responsibility
//! The complete, explicit transition table for [`NodeState`]. Every state
//! change in the daemon goes through [`next_state`]; there is no second path
//! that assigns a state directly.
//!
//! ```text
//! Pending --activate--> Active --drain--> Draining --complete_drain--> Drained
//!                         ^                  |                            |
//!                         +----- undrain ----+----------- undrain --------+
//!
//! Pending | Active | Draining | Drained --revoke--> Revoked (terminal)
//! ```
//!
//! `Revoked` is terminal: a revoked node must re-enrol under a new id through
//! the Auth/Crypto Hub (masterplan §18: NetSec is not an identity authority).
//!
//! `Pending → Active` is the hand-off point from enrolment: in H7 the local
//! operator (an allow-listed peer uid) performs it; with H3 the AuthHub
//! verification result will drive it.

use serde::{Deserialize, Serialize};

use crate::error::{NetsecError, NetsecResult};

/// Lifecycle state of a registered node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    /// Registered, identity not yet confirmed; receives no traffic.
    Pending,
    /// Confirmed and eligible for routing.
    Active,
    /// No new traffic; existing traffic is winding down.
    Draining,
    /// Drain finished; the node carries no traffic.
    Drained,
    /// Permanently removed from the topology (terminal).
    Revoked,
}

impl NodeState {
    /// Every state, in lifecycle order.
    pub const ALL: [Self; 5] = [
        Self::Pending,
        Self::Active,
        Self::Draining,
        Self::Drained,
        Self::Revoked,
    ];

    /// Wire name of the state.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Draining => "draining",
            Self::Drained => "drained",
            Self::Revoked => "revoked",
        }
    }

    /// Whether no event leaves this state.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        NodeEvent::ALL
            .iter()
            .all(|event| next_state(self, *event).is_err())
    }
}

/// An event requested against a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeEvent {
    /// Confirm a pending node.
    Activate,
    /// Stop sending new traffic to an active node.
    Drain,
    /// Record that a draining node carries no traffic any more.
    CompleteDrain,
    /// Return a draining or drained node to service.
    Undrain,
    /// Remove the node permanently.
    Revoke,
}

impl NodeEvent {
    /// Every event.
    pub const ALL: [Self; 5] = [
        Self::Activate,
        Self::Drain,
        Self::CompleteDrain,
        Self::Undrain,
        Self::Revoke,
    ];

    /// Wire name of the event (the JSON/serde form).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Activate => "activate",
            Self::Drain => "drain",
            Self::CompleteDrain => "complete_drain",
            Self::Undrain => "undrain",
            Self::Revoke => "revoke",
        }
    }

    /// The URL action suffix (`POST /v1/nodes/{id}:<action>`).
    #[must_use]
    pub const fn action(self) -> &'static str {
        match self {
            Self::Activate => "activate",
            Self::Drain => "drain",
            Self::CompleteDrain => "complete-drain",
            Self::Undrain => "undrain",
            Self::Revoke => "revoke",
        }
    }

    /// Parses a URL action suffix.
    #[must_use]
    pub fn from_action(action: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|event| event.action() == action)
    }
}

/// The transition table: `(from, event, to)`. The only source of truth.
pub const TRANSITIONS: &[(NodeState, NodeEvent, NodeState)] = &[
    (NodeState::Pending, NodeEvent::Activate, NodeState::Active),
    (NodeState::Pending, NodeEvent::Revoke, NodeState::Revoked),
    (NodeState::Active, NodeEvent::Drain, NodeState::Draining),
    (NodeState::Active, NodeEvent::Revoke, NodeState::Revoked),
    (
        NodeState::Draining,
        NodeEvent::CompleteDrain,
        NodeState::Drained,
    ),
    (NodeState::Draining, NodeEvent::Undrain, NodeState::Active),
    (NodeState::Draining, NodeEvent::Revoke, NodeState::Revoked),
    (NodeState::Drained, NodeEvent::Undrain, NodeState::Active),
    (NodeState::Drained, NodeEvent::Revoke, NodeState::Revoked),
];

/// Resolves the target state of `event` applied in state `from`.
///
/// # Errors
/// [`NetsecError::InvalidTransition`] when the table has no such edge
/// (including every event on a revoked node and repeated events such as
/// draining a node that is already draining).
pub fn next_state(from: NodeState, event: NodeEvent) -> NetsecResult<NodeState> {
    TRANSITIONS
        .iter()
        .find(|(state, candidate, _)| *state == from && *candidate == event)
        .map(|(_, _, to)| *to)
        .ok_or(NetsecError::InvalidTransition { from, event })
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, VecDeque};

    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    /// The expected table, written out as a full matrix so a changed edge
    /// shows up as a diff here and not only in the table above.
    fn expected(from: NodeState, event: NodeEvent) -> Option<NodeState> {
        use NodeEvent as E;
        use NodeState as S;
        match (from, event) {
            (S::Pending, E::Activate) => Some(S::Active),
            (S::Active, E::Drain) => Some(S::Draining),
            (S::Draining, E::CompleteDrain) => Some(S::Drained),
            (S::Draining | S::Drained, E::Undrain) => Some(S::Active),
            (S::Pending | S::Active | S::Draining | S::Drained, E::Revoke) => Some(S::Revoked),
            _ => None,
        }
    }

    #[test]
    fn test_every_state_event_pair_matches_the_expected_matrix() -> TestResult {
        for from in NodeState::ALL {
            for event in NodeEvent::ALL {
                let actual = next_state(from, event).ok();
                if actual != expected(from, event) {
                    return Err(TestError::Unexpected(format!(
                        "{from:?} + {event:?} gave {actual:?}"
                    )));
                }
            }
        }
        Ok(())
    }

    #[test]
    fn test_table_has_no_duplicate_state_event_pairs() {
        let pairs: BTreeSet<(NodeState, NodeEvent)> = TRANSITIONS
            .iter()
            .map(|(from, event, _)| (*from, *event))
            .collect();
        assert_eq!(pairs.len(), TRANSITIONS.len());
    }

    #[test]
    fn test_revoked_is_the_only_terminal_state() {
        for state in NodeState::ALL {
            assert_eq!(
                state.is_terminal(),
                state == NodeState::Revoked,
                "{state:?}"
            );
        }
    }

    #[test]
    fn test_every_state_is_reachable_from_pending() {
        let mut seen = BTreeSet::from([NodeState::Pending]);
        let mut queue = VecDeque::from([NodeState::Pending]);
        while let Some(state) = queue.pop_front() {
            for next in NodeEvent::ALL
                .iter()
                .filter_map(|event| next_state(state, *event).ok())
            {
                if seen.insert(next) {
                    queue.push_back(next);
                }
            }
        }
        assert_eq!(seen.len(), NodeState::ALL.len());
    }

    #[test]
    fn test_every_non_terminal_state_can_be_revoked() {
        for state in NodeState::ALL {
            if state != NodeState::Revoked {
                assert_eq!(
                    next_state(state, NodeEvent::Revoke).ok(),
                    Some(NodeState::Revoked)
                );
            }
        }
    }

    #[test]
    fn test_repeated_drain_is_rejected_not_idempotent() {
        assert!(matches!(
            next_state(NodeState::Draining, NodeEvent::Drain),
            Err(NetsecError::InvalidTransition {
                from: NodeState::Draining,
                event: NodeEvent::Drain
            })
        ));
    }

    #[test]
    fn test_pending_node_cannot_be_drained() {
        assert!(next_state(NodeState::Pending, NodeEvent::Drain).is_err());
    }

    #[test]
    fn test_actions_round_trip_and_unknown_action_is_none() {
        for event in NodeEvent::ALL {
            assert_eq!(NodeEvent::from_action(event.action()), Some(event));
        }
        assert_eq!(NodeEvent::from_action("complete_drain"), None);
        assert_eq!(NodeEvent::from_action(""), None);
    }

    #[test]
    fn test_state_wire_names_match_serde() -> TestResult {
        for state in NodeState::ALL {
            let json = serde_json::to_string(&state).map_err(ctx("serialize"))?;
            assert_eq!(json, format!("\"{}\"", state.as_str()));
        }
        for event in NodeEvent::ALL {
            let json = serde_json::to_string(&event).map_err(ctx("serialize"))?;
            assert_eq!(json, format!("\"{}\"", event.as_str()));
        }
        Ok(())
    }
}
