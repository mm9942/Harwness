//! Compile-time lifecycle boundaries; delegates selected/approved plans to `plan`.
//! No threads, locks, I/O or fallible operations. The transition consumes ownership.
//! Binary-crate examples are illustrative and are not executed as Cargo doctests.
//! # Examples
//! ```ignore
//! let selected = crate::typestate::plan::Plan::new(Vec::new());
//! let approved = selected.approve();
//! assert!(approved.targets().is_empty());
//! ```
/// Owns target handles across the selected-to-approved lifecycle transition.
pub(crate) mod plan;
