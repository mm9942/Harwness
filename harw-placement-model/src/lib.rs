//! Placement vocabulary, minimum slice (docs/planning/66-placement, P1).
//!
//! Only what offer producers need today: what a runtime backend *delivers*
//! per sandbox dimension ([`RuntimeOffer`]) and what a node currently
//! offers ([`NodeOffer`]). Requirements, grants, the filter and the score
//! are later rounds.
//!
//! # Fail closed (DEC-021)
//! An offer carries its observation time and a time to live. A stale offer,
//! an offer from a node that is not `Active`, and a backend that is
//! rootful without an explicit opt-in are never eligible. Public types use
//! [`UnixMillis`] instead of a time crate.

#![forbid(unsafe_code)]

mod offer;

pub use offer::{
    DimensionStates, Enforcement, NodeOffer, NodeState, RuntimeBackend, RuntimeOffer, UnixMillis,
};
