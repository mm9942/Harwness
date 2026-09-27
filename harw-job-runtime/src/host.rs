//! Side-effect-free host capability probe (PL-90 runtime admission).
//!
//! [`HostReport::probe`] answers "what is the strongest sandbox this host
//! can give a job?" **before** any job runs, so admission can refuse a
//! [`harw_job_core::SandboxRequirement::Required`] job up front instead of
//! failing it later.
//!
//! # No side effects
//! The probe only reads: sysctls, the LSM list, `fstatfs`/`faccessat` and
//! the controller files of the own cgroup (never `cgroup.subtree_control`
//! writes, never `mkdir`), and `PATH` entries. The Landlock ABI is
//! determined on a throwaway thread that restricts only itself (see
//! `harw_job_linux::sandbox::landlock_support`); the calling thread and the
//! rest of the process stay unrestricted. No user namespace is created and
//! no process is spawned.
//!
//! # Never fails
//! Anything that cannot be determined is reported conservatively
//! (`false`, `None`, and in [`HostReport::best_report`] `NotEnforced` /
//! `Unsupported`), never guessed upwards.
//!
//! # Best achievable report
//! [`HostReport::best_report_from`] is pure: per sandbox dimension it takes
//! the strongest state any available backend reaches
//! (`Enforced > Partial > NotEnforced > Unsupported`).
//!
//! | Linux backend          | filesystem              | network                 | nnp      | caps     |
//! |------------------------|-------------------------|-------------------------|----------|----------|
//! | none                   | `Unsupported`           | `Unsupported`           | Enforced | Enforced |
//! | Landlock ABI 1-3       | `Partial`               | `NotEnforced`           | Enforced | Enforced |
//! | Landlock ABI 4         | `Partial`               | `Partial` (TCP only)    | Enforced | Enforced |
//! | Landlock ABI >= 5      | `Enforced`              | `Partial` (TCP only)    | Enforced | Enforced |
//! | Bubblewrap             | `Partial`               | `Enforced` (netns deny) | Enforced | Enforced |
//!
//! The Landlock rows mirror `landlock_states` in `harw-job-linux`
//! (`sandbox.rs`). `NO_NEW_PRIVS` and the capability drop are always
//! possible on Linux. Bubblewrap counts only when a `bwrap` executable is
//! on `PATH` and unprivileged user namespaces are not known to be disabled.
//! `resource_limits` is `Enforced` with a delegated cgroup v2 root (the
//! own cgroup, or the root passed to [`HostReport::probe_with_cgroup_root`]),
//! otherwise `Partial` (rlimits only).
//!
//! The report is a kernel/host upper bound: the Landlock trampoline
//! backend additionally needs the `harw-job-exec` binary, which the probe
//! does not look for.
//!
//! On macOS the report is `harw_job_darwin::DarwinSandbox::report_for` for
//! `sandbox-exec` if `/usr/bin/sandbox-exec` exists, else for the
//! unsandboxed mode. Every other OS gets a report in which nothing is
//! enforced.

use std::path::Path;

use harw_job_core::{EnforcementState, SandboxProfileName, SandboxReport};
use serde::{Deserialize, Serialize};

/// Landlock support of the host (mirror of
/// `harw_job_linux::LandlockSupport`, so no Linux-crate type appears in this
/// crate's public API).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostLandlock {
    /// `landlock` is in `/sys/kernel/security/lsm`; `None` if unreadable.
    pub lsm_active: Option<bool>,
    /// Kernel Landlock ABI: `Some(n >= 1)` usable, `Some(0)` not
    /// implemented / not enabled, `None` undetermined.
    pub abi: Option<u8>,
}

impl HostLandlock {
    /// The usable ABI (`>= 1`), if any.
    #[must_use]
    pub fn usable_abi(&self) -> Option<u8> {
        self.abi.filter(|abi| *abi >= 1)
    }
}

/// Raw host facts: the input of [`HostReport::best_report_from`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostFacts {
    /// `std::env::consts::OS` (`linux`, `macos`, ...).
    pub target_os: String,
    /// `std::env::consts::ARCH`.
    pub target_arch: String,
    /// Landlock support (Linux; default elsewhere).
    pub landlock: HostLandlock,
    /// A `bwrap` executable is on `PATH` (Linux).
    pub bwrap_available: bool,
    /// The cgroup v2 root is on a cgroup2 mount and writable (Linux).
    pub cgroup_v2_delegated: bool,
    /// Unprivileged user namespaces: `None` if unknown (Linux).
    pub user_namespaces: Option<bool>,
    /// `/usr/bin/sandbox-exec` exists (macOS).
    pub sandbox_exec_available: bool,
}

/// What this host can enforce for a job, plus the raw facts behind it.
/// Serializable so a runner can print it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostReport {
    /// `std::env::consts::OS`.
    pub target_os: String,
    /// `std::env::consts::ARCH`.
    pub target_arch: String,
    /// Landlock support.
    pub landlock: HostLandlock,
    /// A `bwrap` executable is on `PATH`.
    pub bwrap_available: bool,
    /// The cgroup v2 root is delegated (cgroup2 and writable).
    pub cgroup_v2_delegated: bool,
    /// Unprivileged user namespaces (`None`: unknown).
    pub user_namespaces: Option<bool>,
    /// `/usr/bin/sandbox-exec` exists (macOS).
    pub sandbox_exec_available: bool,
    /// Strongest achievable per-dimension report across all backends.
    pub best_report: SandboxReport,
}

impl HostReport {
    /// Probes this host (own cgroup as cgroup root). Never fails and has no
    /// side effects (see the module docs).
    #[must_use]
    pub fn probe() -> Self {
        Self::probe_with_cgroup_root(None)
    }

    /// As [`HostReport::probe`], but checks delegation of `cgroup_root`
    /// (e.g. `LinuxExecutorOptions::cgroup_root`) instead of the own
    /// cgroup when given.
    #[must_use]
    pub fn probe_with_cgroup_root(cgroup_root: Option<&Path>) -> Self {
        Self::from_facts(probe_facts(cgroup_root))
    }

    /// Builds the report from already collected facts (pure).
    #[must_use]
    pub fn from_facts(facts: HostFacts) -> Self {
        let best_report = Self::best_report_from(&facts);
        let HostFacts {
            target_os,
            target_arch,
            landlock,
            bwrap_available,
            cgroup_v2_delegated,
            user_namespaces,
            sandbox_exec_available,
        } = facts;
        Self {
            target_os,
            target_arch,
            landlock,
            bwrap_available,
            cgroup_v2_delegated,
            user_namespaces,
            sandbox_exec_available,
            best_report,
        }
    }

    /// The raw facts of this report.
    #[must_use]
    pub fn facts(&self) -> HostFacts {
        HostFacts {
            target_os: self.target_os.clone(),
            target_arch: self.target_arch.clone(),
            landlock: self.landlock,
            bwrap_available: self.bwrap_available,
            cgroup_v2_delegated: self.cgroup_v2_delegated,
            user_namespaces: self.user_namespaces,
            sandbox_exec_available: self.sandbox_exec_available,
        }
    }

    /// Strongest achievable report for `facts` (pure; table in the module
    /// docs).
    #[must_use]
    pub fn best_report_from(facts: &HostFacts) -> SandboxReport {
        match facts.target_os.as_str() {
            "linux" => linux_best(facts),
            "macos" => darwin_best(facts.sandbox_exec_available),
            _ => SandboxReport {
                filesystem: EnforcementState::Unsupported,
                network: EnforcementState::Unsupported,
                no_new_privs: EnforcementState::Unsupported,
                capabilities: EnforcementState::Unsupported,
                resource_limits: EnforcementState::NotEnforced,
            },
        }
    }
}

/// Strength rank for the per-dimension maximum.
const fn rank(state: EnforcementState) -> u8 {
    match state {
        EnforcementState::Unsupported => 0,
        EnforcementState::NotEnforced => 1,
        EnforcementState::Partial => 2,
        EnforcementState::Enforced => 3,
    }
}

fn stronger(left: EnforcementState, right: EnforcementState) -> EnforcementState {
    if rank(right) > rank(left) {
        right
    } else {
        left
    }
}

fn merge(left: SandboxReport, right: SandboxReport) -> SandboxReport {
    SandboxReport {
        filesystem: stronger(left.filesystem, right.filesystem),
        network: stronger(left.network, right.network),
        no_new_privs: stronger(left.no_new_privs, right.no_new_privs),
        capabilities: stronger(left.capabilities, right.capabilities),
        resource_limits: stronger(left.resource_limits, right.resource_limits),
    }
}

fn linux_best(facts: &HostFacts) -> SandboxReport {
    let resource_limits = if facts.cgroup_v2_delegated {
        EnforcementState::Enforced
    } else {
        // rlimits are always available, cgroup limits are not.
        EnforcementState::Partial
    };
    // No filesystem/network backend: NO_NEW_PRIVS and the capability drop
    // are always possible on Linux.
    let mut best = SandboxReport {
        filesystem: EnforcementState::Unsupported,
        network: EnforcementState::Unsupported,
        no_new_privs: EnforcementState::Enforced,
        capabilities: EnforcementState::Enforced,
        resource_limits,
    };
    if let Some(abi) = facts.landlock.usable_abi() {
        let landlock = SandboxReport {
            filesystem: if abi >= 5 {
                EnforcementState::Enforced
            } else {
                EnforcementState::Partial
            },
            // TCP bind/connect only (ABI >= 4); UDP/raw stay reachable.
            network: if abi >= 4 {
                EnforcementState::Partial
            } else {
                EnforcementState::NotEnforced
            },
            no_new_privs: EnforcementState::Enforced,
            capabilities: EnforcementState::Enforced,
            resource_limits,
        };
        best = merge(best, landlock);
    }
    if facts.bwrap_available && facts.user_namespaces != Some(false) {
        let bwrap = SandboxReport {
            filesystem: EnforcementState::Partial,
            network: EnforcementState::Enforced,
            no_new_privs: EnforcementState::Enforced,
            capabilities: EnforcementState::Enforced,
            resource_limits,
        };
        best = merge(best, bwrap);
    }
    best
}

fn darwin_best(sandbox_exec_available: bool) -> SandboxReport {
    let sandbox = if sandbox_exec_available {
        harw_job_darwin::DarwinSandbox::sandbox_exec(std::env::temp_dir())
    } else {
        harw_job_darwin::DarwinSandbox::unsandboxed()
    };
    // Every profile maps to the same states today; take the strictest.
    sandbox.report_for(&SandboxProfileName::NoNetwork)
}

fn probe_facts(cgroup_root: Option<&Path>) -> HostFacts {
    let mut facts = HostFacts {
        target_os: std::env::consts::OS.to_owned(),
        target_arch: std::env::consts::ARCH.to_owned(),
        ..HostFacts::default()
    };
    probe_platform(&mut facts, cgroup_root);
    facts
}

#[cfg(target_os = "linux")]
fn probe_platform(facts: &mut HostFacts, cgroup_root: Option<&Path>) {
    let landlock = harw_job_linux::sandbox::landlock_support();
    facts.landlock = HostLandlock {
        lsm_active: landlock.lsm_active,
        abi: landlock.abi,
    };
    facts.user_namespaces = harw_job_linux::sandbox::userns_available();
    facts.bwrap_available = find_executable("bwrap", std::env::var_os("PATH").as_deref());
    let detection = match cgroup_root {
        Some(root) => harw_job_linux::cgroup::detect(root),
        None => harw_job_linux::cgroup::detect_own_cgroup(),
    };
    facts.cgroup_v2_delegated = detection.is_ok_and(|detection| detection.is_delegated());
}

#[cfg(target_os = "macos")]
fn probe_platform(facts: &mut HostFacts, _cgroup_root: Option<&Path>) {
    facts.sandbox_exec_available = Path::new(harw_job_darwin::SANDBOX_EXEC_PATH).is_file();
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn probe_platform(_facts: &mut HostFacts, _cgroup_root: Option<&Path>) {}

/// Whether `name` is an executable regular file in one of the `path`
/// (`PATH`-syntax) directories. Relative `PATH` entries are ignored.
#[cfg(target_os = "linux")]
fn find_executable(name: &str, path: Option<&std::ffi::OsStr>) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    let Some(path) = path else {
        return false;
    };
    std::env::split_paths(path)
        .filter(|dir| dir.is_absolute())
        .any(|dir| {
            std::fs::metadata(dir.join(name))
                .is_ok_and(|meta| meta.is_file() && (meta.permissions().mode() & 0o111) != 0)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    fn linux(landlock_abi: Option<u8>, bwrap: bool, cgroup: bool) -> HostFacts {
        HostFacts {
            target_os: "linux".to_owned(),
            target_arch: "x86_64".to_owned(),
            landlock: HostLandlock {
                lsm_active: Some(landlock_abi.is_some()),
                abi: landlock_abi,
            },
            bwrap_available: bwrap,
            cgroup_v2_delegated: cgroup,
            user_namespaces: Some(true),
            sandbox_exec_available: false,
        }
    }

    #[test]
    fn test_probe_is_idempotent_and_leaves_thread_unrestricted() -> TestResult {
        let first = HostReport::probe();
        let second = HostReport::probe();
        assert_eq!(first, second);
        assert_eq!(first.target_os, std::env::consts::OS);
        assert_eq!(
            first.best_report,
            HostReport::best_report_from(&first.facts())
        );
        // The Landlock probe thread restricted itself to "no filesystem
        // access"; this thread must still write outside any ruleset.
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let file = temp.path().join("after-host-probe");
        std::fs::write(&file, b"ok").map_err(ctx("write after probe"))?;
        let content = std::fs::read(&file).map_err(ctx("read after probe"))?;
        assert_eq!(content, b"ok");
        Ok(())
    }

    #[test]
    fn test_report_serde_round_trip() -> TestResult {
        let report = HostReport::from_facts(linux(Some(5), true, true));
        let json = serde_json::to_string(&report).map_err(ctx("serialize"))?;
        let back: HostReport = serde_json::from_str(&json).map_err(ctx("deserialize"))?;
        assert_eq!(back, report);
        Ok(())
    }

    #[test]
    fn test_best_report_no_backends() {
        let report = HostReport::best_report_from(&linux(None, false, false));
        assert_eq!(report.filesystem, EnforcementState::Unsupported);
        assert_eq!(report.network, EnforcementState::Unsupported);
        assert_eq!(report.no_new_privs, EnforcementState::Enforced);
        assert_eq!(report.capabilities, EnforcementState::Enforced);
        assert_eq!(report.resource_limits, EnforcementState::Partial);
        // ABI 0 (kernel says: no Landlock) is the same as no backend.
        assert_eq!(
            HostReport::best_report_from(&linux(Some(0), false, false)),
            report
        );
    }

    #[test]
    fn test_best_report_landlock_abis() {
        let v5 = HostReport::best_report_from(&linux(Some(5), false, false));
        assert_eq!(v5.filesystem, EnforcementState::Enforced);
        assert_eq!(v5.network, EnforcementState::Partial);
        let v4 = HostReport::best_report_from(&linux(Some(4), false, false));
        assert_eq!(v4.filesystem, EnforcementState::Partial);
        assert_eq!(v4.network, EnforcementState::Partial);
        let v3 = HostReport::best_report_from(&linux(Some(3), false, false));
        assert_eq!(v3.filesystem, EnforcementState::Partial);
        assert_eq!(v3.network, EnforcementState::NotEnforced);
    }

    #[test]
    fn test_best_report_bwrap_only() {
        let report = HostReport::best_report_from(&linux(None, true, false));
        assert_eq!(report.filesystem, EnforcementState::Partial);
        assert_eq!(report.network, EnforcementState::Enforced);
        assert_eq!(report.no_new_privs, EnforcementState::Enforced);
        assert_eq!(report.capabilities, EnforcementState::Enforced);
        // Without user namespaces bwrap does not count.
        let mut facts = linux(None, true, false);
        facts.user_namespaces = Some(false);
        let report = HostReport::best_report_from(&facts);
        assert_eq!(report.filesystem, EnforcementState::Unsupported);
        assert_eq!(report.network, EnforcementState::Unsupported);
    }

    #[test]
    fn test_best_report_takes_per_dimension_max() {
        let report = HostReport::best_report_from(&linux(Some(5), true, false));
        assert_eq!(report.filesystem, EnforcementState::Enforced);
        assert_eq!(report.network, EnforcementState::Enforced);
    }

    #[test]
    fn test_best_report_cgroup_delegated() {
        let report = HostReport::best_report_from(&linux(None, false, true));
        assert_eq!(report.resource_limits, EnforcementState::Enforced);
    }

    #[test]
    fn test_best_report_macos_and_other() {
        let mut facts = HostFacts {
            target_os: "macos".to_owned(),
            ..HostFacts::default()
        };
        let plain = HostReport::best_report_from(&facts);
        assert_eq!(plain.filesystem, EnforcementState::NotEnforced);
        assert_eq!(plain.no_new_privs, EnforcementState::Unsupported);
        facts.sandbox_exec_available = true;
        let sandboxed = HostReport::best_report_from(&facts);
        assert_eq!(sandboxed.filesystem, EnforcementState::Partial);
        assert_eq!(sandboxed.network, EnforcementState::Partial);
        let other = HostReport::best_report_from(&HostFacts {
            target_os: "windows".to_owned(),
            ..HostFacts::default()
        });
        assert!(!other.filesystem.is_enforced());
        assert!(!other.capabilities.is_enforced());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_find_executable() -> TestResult {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let tool = dir.path().join("harw-fake-tool");
        std::fs::write(&tool, b"#!/bin/sh\n").map_err(ctx("write tool"))?;
        let path = std::env::join_paths([dir.path()]).map_err(ctx("join paths"))?;
        assert!(!find_executable("harw-fake-tool", Some(&path)));
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755))
            .map_err(ctx("chmod"))?;
        assert!(find_executable("harw-fake-tool", Some(&path)));
        assert!(!find_executable("harw-missing-tool", Some(&path)));
        assert!(!find_executable("harw-fake-tool", None));
        Ok(())
    }
}
