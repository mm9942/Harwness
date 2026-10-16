//! A stable word for a hosted session state.

use harw_protocol::session_wire::HostedState;

/// `idle`, `running`, `waiting_for_approval`, `waiting_for_child`, `queued`,
/// `interrupted`, `failed`, `closed`, or `unknown` for a state a newer host
/// may add. Clients show or compare these words instead of the enum's debug
/// form, which is not a contract.
#[must_use]
pub fn hosted_state_word(state: &HostedState) -> &'static str {
    match state {
        HostedState::Idle => "idle",
        HostedState::Running => "running",
        HostedState::WaitingForApproval => "waiting_for_approval",
        HostedState::WaitingForChild => "waiting_for_child",
        HostedState::Queued { .. } => "queued",
        HostedState::Interrupted => "interrupted",
        HostedState::Failed => "failed",
        HostedState::Closed => "closed",
        #[allow(unreachable_patterns)]
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ensure};

    #[test]
    fn every_state_has_its_word() -> TestResult {
        ensure(hosted_state_word(&HostedState::Idle) == "idle", "idle")?;
        ensure(
            hosted_state_word(&HostedState::WaitingForApproval) == "waiting_for_approval",
            "waiting",
        )?;
        ensure(
            hosted_state_word(&HostedState::Queued { depth: 3 }) == "queued",
            "queued",
        )
    }
}
