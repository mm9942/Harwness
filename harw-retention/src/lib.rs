//! Retention core for ephemeral data (logs, caches, spools).
//!
//! Two layers:
//!
//! - [`sweep`]: a symlink-safe, deadline-bounded sweep of one directory
//!   under a [`RetentionPolicy`] (age, then count, then bytes; the newest
//!   files are kept; unlink only; `*.lock` and dot-temp files skipped).
//! - Data classes: each class of ephemeral data is declared **once** (id,
//!   directory resolver, name matcher, [`ClassKind`], default limits); the
//!   config struct, the `CLASSES` registry and `policy_for` are derived from
//!   that declaration by `harw_macros::retention_classes!`.
//!
//! Security-relevant classes are opt-in: never deleted by default, dry runs
//! only report, and applying is refused without explicit opt-in.
//!
//! See `README.md` for the frozen API.

// Lets the `retention_classes!` expansion refer to `::harw_retention::..`
// paths from inside this crate as well.
extern crate self as harw_retention;

mod class;
mod policy;
mod sweep;

#[doc(hidden)]
pub use serde;

pub use class::{
    CLASS_CONFIG_FIELDS, Class, ClassConfig, ClassDefaults, ClassKind, DirOutcome, DirResolver,
    ResolvedClass, Roots,
};
pub use policy::{
    ItemError, NameMatch, Removal, RemovalReason, Report, RetentionError, RetentionPolicy,
    SweepMode,
};
pub use sweep::sweep;
