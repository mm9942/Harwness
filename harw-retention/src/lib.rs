//! Retention core for ephemeral data (logs, caches, spools).
//!
//! See `README.md` for the frozen API.

// Lets the `retention_classes!` expansion refer to `::harw_retention::..`
// paths from inside this crate as well.
extern crate self as harw_retention;

mod policy;

pub use policy::{
    ItemError, NameMatch, Removal, RemovalReason, Report, RetentionError, RetentionPolicy,
    SweepMode,
};
