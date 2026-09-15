//! Policy-first domain core for the Harwness local Android agent.
//!
//! This crate deliberately contains no Android framework calls and no model
//! runtime. Android adapters may enumerate files and render notifications, but
//! only this policy layer may turn an explicit user decision into a file
//! disposition. A model can propose a [`CleanupCandidate`]; it never receives
//! authority to delete user content.

#![forbid(unsafe_code)]

mod cleanup;
mod notification;

pub use cleanup::{
    CandidateKind, CleanupCandidate, CleanupError, CleanupProposal, Decision, Disposition,
    ProposalId, ProposalStatus, ReviewedProposal,
};
pub use notification::{ApprovalNotification, NotificationAction};
