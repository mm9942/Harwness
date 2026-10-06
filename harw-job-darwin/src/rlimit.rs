//! Resource limits on Darwin: a validated rlimit plan.
//!
//! `setrlimit(2)` only applies to the calling process; Darwin has no
//! `prlimit`. Applying limits to a job therefore needs the job process to set
//! them on itself before `exec` — the planned exec trampoline. A `pre_exec`
//! hook would need `unsafe` (forbidden), so this crate only offers
//! [`RlimitSet`] (pure data) and, on macOS,
//! `apply_rlimits_to_current_process` for that trampoline.
//!
//! # What a [`ResourceRequest`] maps to (honestly)
//! - `pids_max` → `RLIMIT_NPROC`. On Darwin this counts **all processes of
//!   the real user**, not the job's: at best [`EnforcementState::Partial`].
//! - `memory_max` → not mapped. XNU accepts `RLIMIT_AS`/`RLIMIT_DATA` but does
//!   not enforce them against `mmap`-based allocation; claiming a memory
//!   ceiling would be a lie.
//! - `cpu_weight` → not mapped (no rlimit; `nice` is a different contract).
//! - `wall_timeout` → not an rlimit; enforced by the supervisor through
//!   [`crate::TerminationPolicy`], not reported here.

use std::collections::BTreeMap;

use harw_job_core::{EnforcementState, ResourceRequest};

use crate::error::ProcessError;

/// Darwin rlimit resources this crate sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum RlimitResource {
    /// `RLIMIT_CPU`: CPU seconds.
    Cpu,
    /// `RLIMIT_FSIZE`: largest file the process may create, in bytes.
    FileSize,
    /// `RLIMIT_CORE`: core file size in bytes.
    Core,
    /// `RLIMIT_NOFILE`: open file descriptors.
    OpenFiles,
    /// `RLIMIT_NPROC`: processes of the real user id (per user, not per job).
    Processes,
    /// `RLIMIT_STACK`: main thread stack in bytes.
    Stack,
}

/// Soft/hard pair; `None` means `RLIM_INFINITY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RlimitValue {
    /// Soft (current) limit.
    pub soft: Option<u64>,
    /// Hard (maximum) limit.
    pub hard: Option<u64>,
}

/// A validated set of rlimits (soft ≤ hard for every entry).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RlimitSet {
    entries: BTreeMap<RlimitResource, RlimitValue>,
}

impl RlimitSet {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets `resource` to `soft`/`hard` (`None` = unlimited).
    ///
    /// # Errors
    /// [`ProcessError::InvalidArgument`] if the soft limit exceeds the hard
    /// limit (including a finite hard limit with an unlimited soft limit).
    pub fn set(
        &mut self,
        resource: RlimitResource,
        soft: Option<u64>,
        hard: Option<u64>,
    ) -> Result<&mut Self, ProcessError> {
        let valid = match (soft, hard) {
            (_, None) => true,
            (None, Some(_)) => false,
            (Some(soft), Some(hard)) => soft <= hard,
        };
        if !valid {
            return Err(ProcessError::InvalidArgument {
                reason: format!("{resource:?}: soft limit {soft:?} exceeds hard limit {hard:?}"),
            });
        }
        self.entries.insert(resource, RlimitValue { soft, hard });
        Ok(self)
    }

    /// The limit for `resource`, if set.
    #[must_use]
    pub fn get(&self, resource: RlimitResource) -> Option<RlimitValue> {
        self.entries.get(&resource).copied()
    }

    /// All entries in a fixed order.
    pub fn iter(&self) -> impl Iterator<Item = (RlimitResource, RlimitValue)> + '_ {
        self.entries
            .iter()
            .map(|(resource, value)| (*resource, *value))
    }

    /// Whether no limit is set.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The rlimit translation of a [`ResourceRequest`] plus the honest
/// enforcement verdict for `SandboxReport::resource_limits`, **assuming the
/// set is actually applied** by a trampoline. Without a trampoline the
/// report stays `NotEnforced` (see [`crate::DarwinSandbox`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RlimitPlan {
    /// Limits to apply in the job process.
    pub set: RlimitSet,
    /// Enforcement achievable with this set.
    pub resource_limits: EnforcementState,
    /// Requested fields that have no faithful Darwin rlimit.
    pub unmapped: Vec<&'static str>,
}

impl RlimitPlan {
    /// Translates `request` (see module docs for the mapping).
    ///
    /// # Errors
    /// [`ProcessError::InvalidArgument`] if the request fails
    /// [`ResourceRequest::validate`].
    pub fn from_request(request: &ResourceRequest) -> Result<Self, ProcessError> {
        request
            .validate()
            .map_err(|error| ProcessError::InvalidArgument {
                reason: error.to_string(),
            })?;
        let mut set = RlimitSet::new();
        let mut unmapped = Vec::new();
        let mut mapped_any = false;
        if let Some(pids) = request.pids_max {
            let pids = u64::from(pids);
            set.set(RlimitResource::Processes, Some(pids), Some(pids))?;
            mapped_any = true;
        }
        if request.memory_max.is_some() {
            unmapped.push("memory_max");
        }
        if request.cpu_weight.is_some() {
            unmapped.push("cpu_weight");
        }
        let resource_limits = if mapped_any {
            // RLIMIT_NPROC is per user, never per job.
            EnforcementState::Partial
        } else if unmapped.is_empty() {
            // Nothing requested (wall_timeout is the supervisor's job).
            EnforcementState::Enforced
        } else {
            EnforcementState::Unsupported
        };
        Ok(Self {
            set,
            resource_limits,
            unmapped,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{RlimitPlan, RlimitResource, RlimitSet, RlimitValue};
    use crate::error::ProcessError;
    use crate::test_support::{TestResult, ctx};
    use harw_job_core::{EnforcementState, ResourceRequest};

    #[test]
    fn test_rlimit_set_rejects_soft_above_hard() -> TestResult {
        let mut set = RlimitSet::new();
        set.set(RlimitResource::OpenFiles, Some(64), Some(128))
            .map_err(ctx("valid pair"))?;
        set.set(RlimitResource::Core, Some(0), None)
            .map_err(ctx("finite soft, infinite hard"))?;
        assert!(matches!(
            set.set(RlimitResource::Cpu, Some(10), Some(5)),
            Err(ProcessError::InvalidArgument { .. })
        ));
        assert!(matches!(
            set.set(RlimitResource::Cpu, None, Some(5)),
            Err(ProcessError::InvalidArgument { .. })
        ));
        assert_eq!(
            set.get(RlimitResource::Cpu),
            None,
            "rejected pair not stored"
        );
        assert_eq!(
            set.get(RlimitResource::OpenFiles),
            Some(RlimitValue {
                soft: Some(64),
                hard: Some(128)
            })
        );
        assert_eq!(set.iter().count(), 2);
        Ok(())
    }

    #[test]
    fn test_plan_maps_pids_partially_and_refuses_to_claim_memory() -> TestResult {
        let request = ResourceRequest {
            memory_max: Some(1 << 30),
            cpu_weight: Some(100),
            pids_max: Some(64),
            wall_timeout: None,
            ..ResourceRequest::default()
        };
        let plan = RlimitPlan::from_request(&request).map_err(ctx("plan"))?;
        assert_eq!(plan.resource_limits, EnforcementState::Partial);
        assert_eq!(plan.unmapped, vec!["memory_max", "cpu_weight"]);
        assert_eq!(
            plan.set.get(RlimitResource::Processes),
            Some(RlimitValue {
                soft: Some(64),
                hard: Some(64)
            })
        );

        let memory_only = ResourceRequest {
            memory_max: Some(1 << 20),
            ..ResourceRequest::default()
        };
        let plan = RlimitPlan::from_request(&memory_only).map_err(ctx("memory plan"))?;
        assert_eq!(plan.resource_limits, EnforcementState::Unsupported);
        assert!(plan.set.is_empty());

        let plan =
            RlimitPlan::from_request(&ResourceRequest::default()).map_err(ctx("empty plan"))?;
        assert_eq!(plan.resource_limits, EnforcementState::Enforced);

        let invalid = ResourceRequest {
            pids_max: Some(0),
            ..ResourceRequest::default()
        };
        assert!(matches!(
            RlimitPlan::from_request(&invalid),
            Err(ProcessError::InvalidArgument { .. })
        ));
        Ok(())
    }
}
