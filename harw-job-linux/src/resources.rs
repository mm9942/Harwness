//! Resource policy in our own types (Job-Runtime-Doc §11).
//!
//! [`JobResources`] is translated into cgroup v2 interface files by the
//! cgroup backend ([`JobResources::cgroup_writes`], typed values only — a
//! caller can never write an arbitrary string into a cgroup file) and
//! [`RlimitSet`] into `setrlimit(2)`.
//!
//! # rlimits apply to the *calling* process
//! [`RlimitSet::apply_to_current_process`] changes the limits of the process
//! that calls it. It exists for the planned `harw-job-exec` trampoline,
//! which applies the set to itself right before `execve` so that only the
//! job inherits it. Setting rlimits of a child from the parent would need
//! either `prlimit` after spawn (a window in which the job runs unlimited)
//! or a `pre_exec` hook (`unsafe`); both are rejected (Job-Runtime-Doc §24).

use std::collections::BTreeMap;
use std::fmt::Write as _;

use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
use serde::{Deserialize, Serialize};

use crate::error::{CgroupError, ProcessError};

/// CPU bandwidth limit (`cpu.max`): `quota_us` of CPU time per `period_us`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuQuota {
    /// Allowed CPU time per period in microseconds (may exceed the period
    /// for multi-core allowances).
    pub quota_us: u64,
    /// Period length in microseconds (kernel range 1 000 ..= 1 000 000).
    pub period_us: u64,
}

impl CpuQuota {
    /// Default cgroup v2 period (100 ms).
    pub const DEFAULT_PERIOD_US: u64 = 100_000;

    /// A quota of `cpus` whole CPUs at the default period.
    #[must_use]
    pub fn cpus(cpus: u32) -> Self {
        Self {
            quota_us: u64::from(cpus).saturating_mul(Self::DEFAULT_PERIOD_US),
            period_us: Self::DEFAULT_PERIOD_US,
        }
    }
}

/// Per-device I/O limit (`io.max`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IoDeviceLimit {
    /// Block device major number.
    pub major: u32,
    /// Block device minor number.
    pub minor: u32,
    /// Read bytes per second.
    pub rbps: Option<u64>,
    /// Write bytes per second.
    pub wbps: Option<u64>,
    /// Read I/O operations per second.
    pub riops: Option<u64>,
    /// Write I/O operations per second.
    pub wiops: Option<u64>,
}

/// I/O controller limits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IoLimits {
    /// Default proportional weight (`io.weight`, 1 ..= 10 000).
    pub weight: Option<u16>,
    /// Absolute per-device limits (`io.max`).
    pub devices: Vec<IoDeviceLimit>,
}

/// Resource limits a process can apply to itself via `setrlimit(2)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum RlimitResource {
    /// `RLIMIT_CPU` (seconds).
    Cpu,
    /// `RLIMIT_FSIZE` (bytes).
    FileSize,
    /// `RLIMIT_DATA` (bytes).
    Data,
    /// `RLIMIT_STACK` (bytes).
    Stack,
    /// `RLIMIT_CORE` (bytes).
    Core,
    /// `RLIMIT_NPROC` (processes of the real UID).
    Nproc,
    /// `RLIMIT_NOFILE` (open descriptors).
    Nofile,
    /// `RLIMIT_MEMLOCK` (bytes).
    Memlock,
    /// `RLIMIT_AS` (bytes of address space).
    AddressSpace,
}

impl RlimitResource {
    fn to_rustix(self) -> Resource {
        match self {
            Self::Cpu => Resource::Cpu,
            Self::FileSize => Resource::Fsize,
            Self::Data => Resource::Data,
            Self::Stack => Resource::Stack,
            Self::Core => Resource::Core,
            Self::Nproc => Resource::Nproc,
            Self::Nofile => Resource::Nofile,
            Self::Memlock => Resource::Memlock,
            Self::AddressSpace => Resource::As,
        }
    }
}

/// One rlimit: `None` means `RLIM_INFINITY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RlimitValue {
    /// Soft limit.
    pub soft: Option<u64>,
    /// Hard limit.
    pub hard: Option<u64>,
}

impl RlimitValue {
    /// Soft and hard limit set to the same finite value.
    #[must_use]
    pub fn fixed(value: u64) -> Self {
        Self {
            soft: Some(value),
            hard: Some(value),
        }
    }

    fn validate(self) -> Result<(), ProcessError> {
        match (self.soft, self.hard) {
            (Some(soft), Some(hard)) if soft > hard => Err(ProcessError::InvalidArgument {
                reason: format!("soft rlimit {soft} exceeds hard rlimit {hard}"),
            }),
            (None, Some(hard)) => Err(ProcessError::InvalidArgument {
                reason: format!("infinite soft rlimit exceeds hard rlimit {hard}"),
            }),
            _ => Ok(()),
        }
    }
}

/// Reads the calling process' current limit for `resource`.
#[must_use]
pub fn current_rlimit(resource: RlimitResource) -> RlimitValue {
    let Rlimit { current, maximum } = getrlimit(resource.to_rustix());
    RlimitValue {
        soft: current,
        hard: maximum,
    }
}

/// A set of rlimits, applied in a deterministic order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RlimitSet {
    limits: BTreeMap<RlimitResource, RlimitValue>,
}

impl RlimitSet {
    /// An empty set (changes nothing).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or replaces the limit for `resource` (builder style).
    #[must_use]
    pub fn with(mut self, resource: RlimitResource, value: RlimitValue) -> Self {
        self.limits.insert(resource, value);
        self
    }

    /// Adds or replaces the limit for `resource`.
    pub fn set(&mut self, resource: RlimitResource, value: RlimitValue) {
        self.limits.insert(resource, value);
    }

    /// The configured limit for `resource`.
    #[must_use]
    pub fn get(&self, resource: RlimitResource) -> Option<RlimitValue> {
        self.limits.get(&resource).copied()
    }

    /// Whether no limit is configured.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.limits.is_empty()
    }

    /// Iterates the configured limits in application order.
    pub fn iter(&self) -> impl Iterator<Item = (RlimitResource, RlimitValue)> + '_ {
        self.limits
            .iter()
            .map(|(resource, value)| (*resource, *value))
    }

    /// Validates every entry (soft ≤ hard) without changing anything.
    ///
    /// # Errors
    /// [`ProcessError::InvalidArgument`] for the first invalid entry.
    pub fn validate(&self) -> Result<(), ProcessError> {
        self.limits.values().try_for_each(|value| value.validate())
    }

    /// Applies all limits to the **calling process** (`setrlimit(2)`).
    ///
    /// Validates the whole set first, so an invalid set changes nothing.
    /// Raising a hard limit needs `CAP_SYS_RESOURCE`.
    ///
    /// # Errors
    /// [`ProcessError::InvalidArgument`] for an invalid set;
    /// [`ProcessError::PermissionDenied`] when a hard limit would be raised
    /// without privilege; otherwise the classified OS error. Limits applied
    /// before a failing entry stay applied.
    pub fn apply_to_current_process(&self) -> Result<(), ProcessError> {
        self.validate()?;
        for (resource, value) in self.iter() {
            setrlimit(
                resource.to_rustix(),
                Rlimit {
                    current: value.soft,
                    maximum: value.hard,
                },
            )
            .map_err(|errno| {
                ProcessError::from_errno("setrlimit", std::process::id(), errno.raw_os_error())
            })?;
        }
        Ok(())
    }
}

/// Resource policy of one job attempt (Job-Runtime-Doc §11).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobResources {
    /// Hard memory limit in bytes (`memory.max`).
    pub memory_max: Option<u64>,
    /// Memory throttling threshold in bytes (`memory.high`).
    pub memory_high: Option<u64>,
    /// Proportional CPU weight (`cpu.weight`, 1 ..= 10 000).
    pub cpu_weight: Option<u64>,
    /// CPU bandwidth limit (`cpu.max`).
    pub cpu_max: Option<CpuQuota>,
    /// Maximum number of tasks (`pids.max`).
    pub pids_max: Option<u64>,
    /// I/O limits (`io.weight`, `io.max`).
    pub io: Option<IoLimits>,
    /// Per-process rlimits (applied by the trampoline to itself).
    pub rlimits: RlimitSet,
}

/// cgroup v2 controllers this crate writes limits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum CgroupController {
    /// `memory`.
    Memory,
    /// `cpu`.
    Cpu,
    /// `pids`.
    Pids,
    /// `io`.
    Io,
}

impl CgroupController {
    /// Name as it appears in `cgroup.controllers`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Cpu => "cpu",
            Self::Pids => "pids",
            Self::Io => "io",
        }
    }

    /// All controllers, in `subtree_control` order.
    pub const ALL: [Self; 4] = [Self::Memory, Self::Cpu, Self::Pids, Self::Io];
}

/// One rendered cgroup interface write.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(not(feature = "linux-cgroup-v2"), allow(dead_code))]
pub(crate) struct CgroupWrite {
    pub(crate) controller: CgroupController,
    pub(crate) file: &'static str,
    pub(crate) value: String,
}

impl JobResources {
    /// Whether any cgroup limit is requested (rlimits excluded).
    #[must_use]
    pub fn has_cgroup_limits(&self) -> bool {
        self.memory_max.is_some()
            || self.memory_high.is_some()
            || self.cpu_weight.is_some()
            || self.cpu_max.is_some()
            || self.pids_max.is_some()
            || self.io.is_some()
    }

    /// Validates every cgroup limit against its kernel domain.
    ///
    /// # Errors
    /// [`CgroupError::InvalidLimit`] for the first out-of-domain value.
    pub fn validate(&self) -> Result<(), CgroupError> {
        if let Some(weight) = self
            .cpu_weight
            .filter(|weight| !(1..=10_000).contains(weight))
        {
            return Err(CgroupError::InvalidLimit {
                field: "cpu_weight",
                reason: format!("{weight} outside 1..=10000"),
            });
        }
        if let Some(quota) = self.cpu_max {
            if !(1_000..=1_000_000).contains(&quota.period_us) {
                return Err(CgroupError::InvalidLimit {
                    field: "cpu_max.period_us",
                    reason: format!("{} outside 1000..=1000000", quota.period_us),
                });
            }
            if quota.quota_us < 1_000 {
                return Err(CgroupError::InvalidLimit {
                    field: "cpu_max.quota_us",
                    reason: format!("{} below 1000", quota.quota_us),
                });
            }
        }
        if let (Some(high), Some(max)) = (self.memory_high, self.memory_max) {
            if high > max {
                return Err(CgroupError::InvalidLimit {
                    field: "memory_high",
                    reason: format!("{high} exceeds memory_max {max}"),
                });
            }
        }
        if let Some(io) = &self.io {
            if let Some(weight) = io.weight.filter(|weight| !(1..=10_000).contains(weight)) {
                return Err(CgroupError::InvalidLimit {
                    field: "io.weight",
                    reason: format!("{weight} outside 1..=10000"),
                });
            }
            if io.devices.iter().any(|device| {
                device.rbps.is_none()
                    && device.wbps.is_none()
                    && device.riops.is_none()
                    && device.wiops.is_none()
            }) {
                return Err(CgroupError::InvalidLimit {
                    field: "io.devices",
                    reason: "device entry without any limit".to_owned(),
                });
            }
        }
        Ok(())
    }

    /// Renders the cgroup v2 interface writes for these limits (validated).
    ///
    /// # Errors
    /// As [`JobResources::validate`].
    #[cfg_attr(not(feature = "linux-cgroup-v2"), allow(dead_code))]
    pub(crate) fn cgroup_writes(&self) -> Result<Vec<CgroupWrite>, CgroupError> {
        self.validate()?;
        let mut writes = Vec::new();
        let mut push = |controller: CgroupController, file: &'static str, value: String| {
            writes.push(CgroupWrite {
                controller,
                file,
                value,
            });
        };
        if let Some(max) = self.memory_max {
            push(CgroupController::Memory, "memory.max", max.to_string());
        }
        if let Some(high) = self.memory_high {
            push(CgroupController::Memory, "memory.high", high.to_string());
        }
        if let Some(weight) = self.cpu_weight {
            push(CgroupController::Cpu, "cpu.weight", weight.to_string());
        }
        if let Some(quota) = self.cpu_max {
            push(
                CgroupController::Cpu,
                "cpu.max",
                format!("{} {}", quota.quota_us, quota.period_us),
            );
        }
        if let Some(pids) = self.pids_max {
            push(CgroupController::Pids, "pids.max", pids.to_string());
        }
        if let Some(io) = &self.io {
            if let Some(weight) = io.weight {
                push(
                    CgroupController::Io,
                    "io.weight",
                    format!("default {weight}"),
                );
            }
            for device in &io.devices {
                let mut line = format!("{}:{}", device.major, device.minor);
                for (key, value) in [
                    ("rbps", device.rbps),
                    ("wbps", device.wbps),
                    ("riops", device.riops),
                    ("wiops", device.wiops),
                ] {
                    if let Some(value) = value {
                        // Writing into a String cannot fail.
                        let _ = write!(line, " {key}={value}");
                    }
                }
                push(CgroupController::Io, "io.max", line);
            }
        }
        Ok(writes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_cgroup_writes_render_typed_values() -> TestResult {
        let resources = JobResources {
            memory_max: Some(512 * 1024 * 1024),
            memory_high: Some(256 * 1024 * 1024),
            cpu_weight: Some(200),
            cpu_max: Some(CpuQuota::cpus(2)),
            pids_max: Some(64),
            io: Some(IoLimits {
                weight: Some(50),
                devices: vec![IoDeviceLimit {
                    major: 8,
                    minor: 0,
                    rbps: Some(1_000_000),
                    wbps: None,
                    riops: None,
                    wiops: Some(100),
                }],
            }),
            rlimits: RlimitSet::new(),
        };
        let writes = resources.cgroup_writes().map_err(ctx("render"))?;
        let rendered: Vec<(&str, &str)> = writes
            .iter()
            .map(|write| (write.file, write.value.as_str()))
            .collect();
        assert_eq!(
            rendered,
            vec![
                ("memory.max", "536870912"),
                ("memory.high", "268435456"),
                ("cpu.weight", "200"),
                ("cpu.max", "200000 100000"),
                ("pids.max", "64"),
                ("io.weight", "default 50"),
                ("io.max", "8:0 rbps=1000000 wiops=100"),
            ]
        );
        assert!(resources.has_cgroup_limits());
        Ok(())
    }

    #[test]
    fn test_validate_rejects_out_of_domain_limits() {
        let weight = JobResources {
            cpu_weight: Some(0),
            ..JobResources::default()
        };
        assert!(matches!(
            weight.validate(),
            Err(CgroupError::InvalidLimit {
                field: "cpu_weight",
                ..
            })
        ));
        let period = JobResources {
            cpu_max: Some(CpuQuota {
                quota_us: 50_000,
                period_us: 10,
            }),
            ..JobResources::default()
        };
        assert!(period.validate().is_err());
        let memory = JobResources {
            memory_max: Some(10),
            memory_high: Some(20),
            ..JobResources::default()
        };
        assert!(memory.validate().is_err());
        assert!(!JobResources::default().has_cgroup_limits());
    }

    #[test]
    fn test_rlimit_set_rejects_soft_above_hard() {
        let set = RlimitSet::new().with(
            RlimitResource::Nofile,
            RlimitValue {
                soft: Some(10),
                hard: Some(5),
            },
        );
        assert!(matches!(
            set.validate(),
            Err(ProcessError::InvalidArgument { .. })
        ));
        // An invalid set changes nothing and is rejected before setrlimit.
        assert!(set.apply_to_current_process().is_err());
    }

    #[test]
    fn test_rlimit_apply_current_value_is_noop() -> TestResult {
        // Re-applying the current NOFILE limit exercises the setrlimit path
        // without changing the test runner's limits.
        let current = current_rlimit(RlimitResource::Nofile);
        let set = RlimitSet::new().with(RlimitResource::Nofile, current);
        set.apply_to_current_process()
            .map_err(ctx("re-apply current NOFILE"))?;
        assert_eq!(current_rlimit(RlimitResource::Nofile), current);
        assert_eq!(set.get(RlimitResource::Nofile), Some(current));
        Ok(())
    }
}
