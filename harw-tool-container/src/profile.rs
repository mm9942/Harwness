//! Run profiles: what a container may do. Chosen by name, set by config.
//!
//! A profile fixes every knob the model must not control: workspace access,
//! network, memory, process count and the maximum wall time. The model picks
//! only the profile name and (optionally) a shorter timeout.

use std::fmt;

/// A named run profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Profile {
    /// Read-only workspace, no network. Run a tool, lint, inspect.
    Hermetic,
    /// Read-write workspace plus an optional cache volume, no network.
    Build,
}

/// Fixed resource ceiling of a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Memory limit in MiB.
    pub memory_mib: u32,
    /// Maximum number of processes.
    pub pids: u32,
    /// Maximum wall time in seconds.
    pub timeout_s: u32,
}

impl Profile {
    /// Every profile, for exhaustive tests and `container.engine` output.
    pub const ALL: [Self; 2] = [Self::Hermetic, Self::Build];

    /// Stable lowercase name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hermetic => "hermetic",
            Self::Build => "build",
        }
    }

    /// Parses the exact lowercase name (no aliases, case sensitive).
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == raw)
    }

    /// Resource ceiling.
    #[must_use]
    pub const fn limits(self) -> Limits {
        match self {
            Self::Hermetic => Limits {
                memory_mib: 1024,
                pids: 256,
                timeout_s: 300,
            },
            Self::Build => Limits {
                memory_mib: 4096,
                pids: 512,
                timeout_s: 1800,
            },
        }
    }

    /// `true` when the workspace is mounted read-only.
    #[must_use]
    pub const fn workspace_read_only(self) -> bool {
        matches!(self, Self::Hermetic)
    }

    /// `true` when a cache volume may be mounted.
    #[must_use]
    pub const fn allows_cache(self) -> bool {
        matches!(self, Self::Build)
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ensure};

    #[test]
    fn names_round_trip_and_are_exact() -> TestResult {
        for p in Profile::ALL {
            ensure(Profile::parse(p.as_str()) == Some(p), "round trip")?;
        }
        ensure(Profile::parse("Hermetic").is_none(), "case sensitive")?;
        ensure(Profile::parse("fetch").is_none(), "no network profile yet")
    }

    #[test]
    fn hermetic_is_read_only_and_build_is_not() -> TestResult {
        ensure(Profile::Hermetic.workspace_read_only(), "hermetic ro")?;
        ensure(!Profile::Build.workspace_read_only(), "build rw")?;
        ensure(!Profile::Hermetic.allows_cache(), "no cache in hermetic")?;
        ensure(Profile::Build.allows_cache(), "cache in build")
    }

    #[test]
    fn every_profile_has_a_positive_ceiling() -> TestResult {
        for p in Profile::ALL {
            let l = p.limits();
            ensure(
                l.memory_mib > 0 && l.pids > 0 && l.timeout_s > 0,
                "positive",
            )?;
        }
        ensure(
            Profile::Build.limits().timeout_s >= Profile::Hermetic.limits().timeout_s,
            "build is not shorter",
        )
    }
}
