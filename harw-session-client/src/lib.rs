//! `harw-session-client`: the client-side logic every session client needs,
//! once.
//!
//! # Why this crate exists
//! A client of the session control plane (`harw.session.v1`) has to get a
//! handful of small things exactly right, and each of them was found by a
//! failure: a prompt resent after a restart must not be swallowed as a
//! duplicate, an event on the attach head is new while replay lies before it,
//! an approval must be answered once, a stale head must be retried. Written
//! per client they drift apart (the SDK, the phone client and the TUI each had
//! their own copy). Here they are named functions and small types with tests,
//! and every client sits on top.
//!
//! # What is in it
//! - [`keys`]: [`IdempotencyKeys`], restart-safe keys for `turn.submit`.
//! - [`stream`]: [`StreamTracker`], which events are new and which are replay.
//! - [`submit`]: [`interpret_submit`], what to do with a `turn.submit` answer.
//! - [`respond`]: [`interpret_respond`], [`AnsweredSet`], [`review`] for
//!   `approval.respond`.
//! - [`approval`]: [`describe`], an approval request as tool and arguments.
//! - [`tally`]: [`TurnTally`], what one turn amounted to.
//! - [`state`]: [`hosted_state_word`], a stable word for a hosted state.
//!
//! # What is not in it
//! No transport, no async, no runtime, no I/O. A client brings those (see
//! `harw-session-remote`) and calls into this crate with what it received.

#![forbid(unsafe_code)]

pub mod approval;
pub mod keys;
pub mod respond;
pub mod state;
pub mod stream;
pub mod submit;
pub mod tally;
#[cfg(test)]
mod test_support;

pub use approval::{ApprovalSummary, describe};
pub use keys::IdempotencyKeys;
pub use respond::{AnsweredSet, RespondStep, interpret_respond, review};
pub use state::hosted_state_word;
pub use stream::StreamTracker;
pub use submit::{MAX_SUBMIT_ATTEMPTS, SubmitRefusal, SubmitStep, interpret_submit};
pub use tally::{TurnEnd, TurnSummary, TurnTally, message_text};
