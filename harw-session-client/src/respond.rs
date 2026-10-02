//! `approval.respond`: deciding once, and what the answer means.
//!
//! The host binds a request to the actor that was current when the turn
//! parked, and the first decision wins. A client must therefore (1) answer
//! each request once even if its frame arrives again after a re-attach
//! ([`AnsweredSet`]), (2) turn its verdict into the wire decision
//! ([`review`]), and (3) not treat "somebody else was faster" as a failure
//! ([`interpret_respond`]).

use std::collections::HashSet;

use harw_protocol::session_wire::RespondResult;
use harw_types::ReviewDecision;

/// The request ids this client has already answered.
#[derive(Debug, Clone, Default)]
pub struct AnsweredSet {
    ids: HashSet<String>,
}

impl AnsweredSet {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` the first time `request_id` is seen; `false` afterwards. Use it
    /// to decide whether to ask the user (or the handler) at all.
    pub fn first_time(&mut self, request_id: &str) -> bool {
        self.ids.insert(request_id.to_owned())
    }
}

/// The wire decision for a verdict: approve, or reject with the reason the
/// model will see.
#[must_use]
pub fn review(approve: bool, reason: Option<String>) -> (ReviewDecision, Option<String>) {
    if approve {
        (ReviewDecision::Approved, None)
    } else {
        (ReviewDecision::Rejected, reason)
    }
}

/// What the host's answer to `approval.respond` means for the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RespondStep {
    /// The turn goes on: the decision was taken, another device was faster,
    /// or the request had expired. None of these is a failure of this client.
    Settled,
    /// This client may not decide; the host's reason.
    Refused(String),
    /// An answer this client does not know (a newer host).
    Unknown,
}

/// Interprets one answer.
#[must_use]
pub fn interpret_respond(result: RespondResult) -> RespondStep {
    match result {
        RespondResult::Resolved
        | RespondResult::AlreadyResolved { .. }
        | RespondResult::Expired => RespondStep::Settled,
        RespondResult::Denied { reason } => RespondStep::Refused(reason),
        #[allow(unreachable_patterns)]
        _ => RespondStep::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ensure};

    #[test]
    fn a_request_is_answered_once() -> TestResult {
        let mut answered = AnsweredSet::new();
        ensure(answered.first_time("a1"), "first sight")?;
        ensure(!answered.first_time("a1"), "the frame arrives again")?;
        ensure(answered.first_time("a2"), "another request")
    }

    #[test]
    fn a_rejection_carries_its_reason_and_an_approval_none() -> TestResult {
        ensure(
            review(false, Some("no".to_owned()))
                == (ReviewDecision::Rejected, Some("no".to_owned())),
            "reject",
        )?;
        ensure(
            review(true, Some("ignored".to_owned())) == (ReviewDecision::Approved, None),
            "approve",
        )
    }

    #[test]
    fn losing_the_race_is_not_a_failure() -> TestResult {
        ensure(
            interpret_respond(RespondResult::Resolved) == RespondStep::Settled,
            "won",
        )?;
        ensure(
            interpret_respond(RespondResult::AlreadyResolved {
                by: "tablet".to_owned(),
            }) == RespondStep::Settled,
            "another device was faster",
        )?;
        ensure(
            interpret_respond(RespondResult::Expired) == RespondStep::Settled,
            "expired",
        )?;
        ensure(
            interpret_respond(RespondResult::Denied {
                reason: "no approve right".to_owned(),
            }) == RespondStep::Refused("no approve right".to_owned()),
            "refused",
        )
    }
}
