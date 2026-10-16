//! Server-derived reach for durable job mutations.
//!
//! This is a typed service marker, deliberately not a tool argument. The
//! runtime composition root chooses the variant from the trusted operation
//! surface before dispatch, so model output cannot widen mutation reach.

#![forbid(unsafe_code)]

/// How far a job-mutating operation may resolve a supplied WorkId.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobMutationScope {
    /// Human/operator command surface: historical tenant-visible reach.
    TenantVisible,
    /// Model-tool surface: exact trusted tenant + workspace binding.
    BoundWorkspace,
}
