//! Selected targets cannot reach execution until their plan is approved.
//! The marker is compile-time only; no parallel runtime state. Approval is a
//! caller responsibility after the CLI prompt or an explicit --yes flag.
//! These infallible operations create no I/O, locks or threads. Binary-crate
//! documentation examples are illustrative and are not executed by Cargo doctests.
//! # Examples
//! ```no_run
//! std::process::Command::new("killer").args(["-p", "cargo", "--yes"]).status()?;
//! # Ok::<(), std::io::Error>(())
//! ```
use crate::types::Target;
use std::marker::PhantomData;

/// A previewable selection not yet authorized for execution.
pub(crate) struct Selected;
/// A selection authorized by the CLI interaction policy.
pub(crate) struct Approved;
/// Owns every target handle throughout the approval and execution phases.
pub(crate) struct Plan<State> {
    // Keep process handles alive until execution (including sudo) has completed.
    targets: Vec<Target>,
    // Express ownership of the state marker; no runtime state discriminant.
    _state: PhantomData<State>,
}
/// Construction and consuming approval of a selected snapshot.
impl Plan<Selected> {
    /// Take ownership of a target collection without authorizing signals.
    /// `targets` transfers its vector and all kernel handles to the returned
    /// selected plan. Empty collections are allowed. Infallible; does no I/O.
    /// # Examples
    /// ```no_run
    /// let selected = crate::typestate::plan::Plan::new(Vec::new());
    /// assert!(selected.targets().is_empty());
    /// ```
    pub(crate) fn new(targets: Vec<Target>) -> Self {
        Self {
            targets,
            _state: PhantomData,
        }
    }
    /// Consume the snapshot after the caller obtained explicit user intent.
    /// Moves `self` and its target handles into a returned `Plan<Approved>`;
    /// the original plan becomes unavailable. The caller must have completed
    /// confirmation or accepted `--yes`; this method does not prompt or signal.
    /// Infallible and does not clone descriptors or spawn threads.
    /// # Examples
    /// ```no_run
    /// let selected = crate::typestate::plan::Plan::new(Vec::new());
    /// let approved: crate::typestate::plan::Plan<crate::typestate::plan::Approved>
    ///     = selected.approve();
    /// assert!(approved.targets().is_empty());
    /// ```
    pub(crate) fn approve(self) -> Plan<Approved> {
        Plan {
            targets: self.targets,
            _state: PhantomData,
        }
    }
}
/// Read-only preview is valid in either state without duplicating handles.
impl<State> Plan<State> {
    /// Borrow the immutable selection; never transfer or clone its handles.
    /// Borrows `self` and returns a slice valid for the same borrow duration.
    /// Available in either state. Infallible, allocation-free and lock-free.
    /// # Examples
    /// ```no_run
    /// let plan = crate::typestate::plan::Plan::new(Vec::new());
    /// let targets = plan.targets();
    /// assert!(targets.is_empty());
    /// ```
    pub(crate) fn targets(&self) -> &[Target] {
        &self.targets
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    // Pure test: no process handles are acquired for an empty plan.
    #[test]
    fn test_approve_consumes_selected_plan() {
        let selected = Plan::new(Vec::new());
        assert!(selected.targets().is_empty());
        let approved: Plan<Approved> = selected.approve();
        assert!(approved.targets().is_empty());
    }
}
