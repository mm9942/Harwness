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
//! - `memory_high` → not mapped (no rlimit throttles memory; the same XNU
//!   caveat as `memory_max` applies).
//! - `cpu_weight` → not mapped (no rlimit; `nice` is a different contract).
//! - `cpu_max` → not mapped. `RLIMIT_CPU` caps the total CPU time of one
//!   process, not the job's bandwidth per period.
//! - `io_weight` → not mapped (no rlimit; Darwin has no per-job I/O weight).
//! - `cpu_time_max` → `RLIMIT_CPU`, `file_size_max` → `RLIMIT_FSIZE`,
//!   `open_files_max` → `RLIMIT_NOFILE` (soft = hard = the request). These
//!   are per-process limits XNU enforces as asked.
//! - `process_count_max` → `RLIMIT_NPROC` as asked (it is the per-user
//!   limit by definition); with `pids_max` the smaller value is set.
//! - `address_space_max` → not mapped (`RLIMIT_AS`; the same XNU caveat as
//!   `memory_max` applies).
//! - `wall_timeout` → not an rlimit; enforced by the supervisor through
//!   [`crate::TerminationPolicy`], not reported here.
//! - `require_rlimits` → not part of the plan; the caller refuses a job that
//!   sets it while no trampoline applies the set.

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
        // Destructured without a rest pattern: a new `ResourceRequest`
        // field does not compile until it is classified here.
        let ResourceRequest {
            memory_max,
            memory_high,
            cpu_weight,
            cpu_max,
            pids_max,
            io_weight,
            address_space_max,
            process_count_max,
            cpu_time_max,
            file_size_max,
            open_files_max,
            // Enforced by the supervisor (`crate::TerminationPolicy`).
            wall_timeout: _,
            // Not a limit: the runtime's Darwin executor refuses a job that
            // sets it with any rlimit while no trampoline applies the set.
            require_rlimits: _,
        } = request;
        let mut set = RlimitSet::new();
        let mut unmapped = Vec::new();
        // Mapped, but weaker than requested.
        let mut weaker = false;
        // Mapped exactly as requested.
        let mut faithful = false;
        // `pids_max` and `process_count_max` share `RLIMIT_NPROC`: the
        // smaller one wins.
        let processes = match (pids_max.map(u64::from), *process_count_max) {
            (Some(pids), Some(count)) => Some(pids.min(count)),
            (pids, count) => pids.or(count),
        };
        if let Some(processes) = processes {
            set.set(RlimitResource::Processes, Some(processes), Some(processes))?;
        }
        if pids_max.is_some() {
            // RLIMIT_NPROC is per user, never per job.
            weaker = true;
        }
        if process_count_max.is_some() {
            // Asked as the per-user RLIMIT_NPROC: applied as asked.
            faithful = true;
        }
        for (resource, limit) in [
            (RlimitResource::Cpu, *cpu_time_max),
            (RlimitResource::FileSize, *file_size_max),
            (RlimitResource::OpenFiles, open_files_max.map(u64::from)),
        ] {
            if let Some(limit) = limit {
                set.set(resource, Some(limit), Some(limit))?;
                faithful = true;
            }
        }
        // Every requested limit without a faithful rlimit is listed, so a
        // request of only these is never `Enforced`.
        for (field, requested) in [
            ("memory_max", memory_max.is_some()),
            ("memory_high", memory_high.is_some()),
            ("cpu_weight", cpu_weight.is_some()),
            ("cpu_max", cpu_max.is_some()),
            ("io_weight", io_weight.is_some()),
            ("address_space_max", address_space_max.is_some()),
        ] {
            if requested {
                unmapped.push(field);
            }
        }
        let resource_limits = if weaker || (faithful && !unmapped.is_empty()) {
            // Part of the request is applied, or only weaker than asked.
            EnforcementState::Partial
        } else if unmapped.is_empty() {
            // Nothing requested, or only limits applied as asked
            // (wall_timeout is the supervisor's job).
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
    use harw_job_core::spec::CpuBandwidth;
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

    /// PL-97 E2f: `process_count_max` is the per-user `RLIMIT_NPROC` and is
    /// applied as asked; with `pids_max` the smaller value wins and the
    /// per-job `pids_max` keeps the dimension `Partial`.
    #[test]
    fn test_plan_maps_process_count_max_to_nproc() -> TestResult {
        let cases = [
            (None, Some(100), 100, EnforcementState::Enforced),
            (Some(64), Some(100), 64, EnforcementState::Partial),
            (Some(200), Some(100), 100, EnforcementState::Partial),
            (Some(64), None, 64, EnforcementState::Partial),
        ];
        for (pids_max, process_count_max, expected, state) in cases {
            let request = ResourceRequest {
                pids_max,
                process_count_max,
                ..ResourceRequest::default()
            };
            let plan = RlimitPlan::from_request(&request).map_err(ctx("plan"))?;
            assert_eq!(
                plan.set.get(RlimitResource::Processes),
                Some(RlimitValue {
                    soft: Some(expected),
                    hard: Some(expected)
                }),
                "{pids_max:?}/{process_count_max:?}"
            );
            assert_eq!(
                plan.resource_limits, state,
                "{pids_max:?}/{process_count_max:?}"
            );
            assert!(plan.unmapped.is_empty(), "{:?}", plan.unmapped);
        }
        Ok(())
    }

    /// PL-97 E2a follow-up: `memory_high`, `cpu_max` and `io_weight` have no
    /// Darwin rlimit; a request of only these is unmapped, never `Enforced`.
    #[test]
    fn test_plan_lists_memory_high_cpu_max_and_io_weight_as_unmapped() -> TestResult {
        let cases = [
            (
                "memory_high",
                ResourceRequest {
                    memory_high: Some(1 << 28),
                    ..ResourceRequest::default()
                },
            ),
            (
                "cpu_max",
                ResourceRequest {
                    cpu_max: Some(CpuBandwidth::cpus(1)),
                    ..ResourceRequest::default()
                },
            ),
            (
                "io_weight",
                ResourceRequest {
                    io_weight: Some(50),
                    ..ResourceRequest::default()
                },
            ),
        ];
        for (field, request) in cases {
            let plan = RlimitPlan::from_request(&request).map_err(ctx(field))?;
            assert_eq!(
                plan.resource_limits,
                EnforcementState::Unsupported,
                "{field}"
            );
            assert_eq!(plan.unmapped, vec![field]);
            assert!(plan.set.is_empty(), "{field}");
        }

        let all = ResourceRequest {
            memory_high: Some(1 << 28),
            cpu_max: Some(CpuBandwidth::cpus(2)),
            pids_max: Some(32),
            io_weight: Some(10),
            ..ResourceRequest::default()
        };
        let plan = RlimitPlan::from_request(&all).map_err(ctx("mixed plan"))?;
        assert_eq!(plan.resource_limits, EnforcementState::Partial);
        assert_eq!(plan.unmapped, vec!["memory_high", "cpu_max", "io_weight"]);
        Ok(())
    }

    /// PL-97 E2c follow-up: `cpu_time_max`, `file_size_max` and
    /// `open_files_max` become their rlimit (soft = hard); `address_space_max`
    /// has no faithful Darwin rlimit and is listed as unmapped.
    #[test]
    fn test_plan_maps_the_per_process_rlimits() -> TestResult {
        let fixed = |limit| {
            Some(RlimitValue {
                soft: Some(limit),
                hard: Some(limit),
            })
        };
        let cases = [
            (
                RlimitResource::Cpu,
                60,
                ResourceRequest {
                    cpu_time_max: Some(60),
                    ..ResourceRequest::default()
                },
            ),
            (
                RlimitResource::FileSize,
                1 << 20,
                ResourceRequest {
                    file_size_max: Some(1 << 20),
                    ..ResourceRequest::default()
                },
            ),
            (
                RlimitResource::OpenFiles,
                256,
                ResourceRequest {
                    open_files_max: Some(256),
                    ..ResourceRequest::default()
                },
            ),
        ];
        for (resource, limit, request) in cases {
            let plan = RlimitPlan::from_request(&request).map_err(ctx("faithful plan"))?;
            assert_eq!(plan.set.get(resource), fixed(limit), "{resource:?}");
            assert_eq!(plan.set.iter().count(), 1, "{resource:?}");
            assert!(plan.unmapped.is_empty(), "{resource:?}");
            assert_eq!(
                plan.resource_limits,
                EnforcementState::Enforced,
                "{resource:?}"
            );
        }

        let address_space = ResourceRequest {
            address_space_max: Some(1 << 30),
            ..ResourceRequest::default()
        };
        let plan = RlimitPlan::from_request(&address_space).map_err(ctx("address space"))?;
        assert_eq!(plan.resource_limits, EnforcementState::Unsupported);
        assert_eq!(plan.unmapped, vec!["address_space_max"]);
        assert!(plan.set.is_empty());

        // Every rlimit at once: the faithful ones are set, the address space
        // stays unmapped, so the verdict is partial.
        let all = ResourceRequest {
            address_space_max: Some(1 << 30),
            cpu_time_max: Some(60),
            file_size_max: Some(1 << 20),
            open_files_max: Some(256),
            require_rlimits: true,
            ..ResourceRequest::default()
        };
        let plan = RlimitPlan::from_request(&all).map_err(ctx("all rlimits"))?;
        assert_eq!(plan.resource_limits, EnforcementState::Partial);
        assert_eq!(plan.unmapped, vec!["address_space_max"]);
        assert_eq!(plan.set.get(RlimitResource::Cpu), fixed(60));
        assert_eq!(plan.set.get(RlimitResource::FileSize), fixed(1 << 20));
        assert_eq!(plan.set.get(RlimitResource::OpenFiles), fixed(256));
        assert_eq!(plan.set.iter().count(), 3);

        // Next to a pids ceiling (per user) the verdict stays partial.
        let with_pids = ResourceRequest {
            pids_max: Some(32),
            open_files_max: Some(256),
            ..ResourceRequest::default()
        };
        let plan = RlimitPlan::from_request(&with_pids).map_err(ctx("pids and nofile"))?;
        assert_eq!(plan.resource_limits, EnforcementState::Partial);
        assert!(plan.unmapped.is_empty());
        assert_eq!(plan.set.iter().count(), 2);

        let zero = ResourceRequest {
            open_files_max: Some(0),
            ..ResourceRequest::default()
        };
        assert!(matches!(
            RlimitPlan::from_request(&zero),
            Err(ProcessError::InvalidArgument { .. })
        ));
        Ok(())
    }
}
