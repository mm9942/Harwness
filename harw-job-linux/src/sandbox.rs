//! Harw-native sandbox policy and its Linux enforcement (Job-Runtime-Doc
//! §3.5, §12, Phase 5).
//!
//! The policy types ([`SandboxPolicy`] and friends) and the pure rule
//! planning ([`plan_landlock`]) are always available. Enforcement
//! (`apply_to_current_process`, `build_ruleset`) needs feature
//! `linux-sandbox` (Landlock).
//!
//! # Scope: the calling thread, before `exec`
//! `NO_NEW_PRIVS`, capability sets and Landlock domains are per-thread and
//! inherited across `fork`/`execve`. `apply_to_current_process` restricts
//! the **calling thread irreversibly**. It is meant for the planned
//! `harw-job-exec` trampoline: a single-threaded helper that applies the
//! policy to itself and then `exec`s the job. Never call it in a
//! long-running supervisor.
//!
//! # Honest reporting
//! The result is a per-dimension `SandboxReport`, never a single `bool`:
//! - Filesystem: `Enforced` only with the full target Landlock ABI (V5,
//!   Linux 6.10); older ABIs give `Partial`; no Landlock gives
//!   `Unsupported`.
//! - Network: Landlock only restricts TCP `bind`/`connect` (ABI ≥ V4). UDP,
//!   raw and abstract sockets remain possible, so a denying network policy
//!   is at best `Partial`. Full network isolation needs a network namespace
//!   (a future container executor).
//! - `resource_limits` is always `NotEnforced` here: rlimits and cgroup
//!   limits are applied by other steps, whose results the caller merges in.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use harw_job_core::SandboxProfileName;
use serde::{Deserialize, Serialize};

/// Paths every profile may read and execute (toolchains, shells, libc).
const SYSTEM_EXEC: &[&str] = &["/usr", "/bin", "/sbin", "/lib", "/lib64", "/lib32"];
/// Paths every profile may read (configuration, kernel interfaces).
const SYSTEM_READ: &[&str] = &["/etc", "/proc", "/sys", "/dev", "/run"];
/// Paths every profile may write (scratch space and the null device).
const SYSTEM_WRITE: &[&str] = &["/tmp", "/dev/null", "/dev/zero", "/dev/tty"];
/// TCP ports a network-restricted job may connect to (HTTP/HTTPS).
const RESTRICTED_CONNECT_PORTS: &[u16] = &[80, 443];

/// Filesystem access lists. Paths are explicit policy data; nothing else
/// is reachable once Landlock is enforced.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemPolicy {
    /// Read-only hierarchies (read files, list directories).
    pub read_only: Vec<PathBuf>,
    /// Read-write hierarchies (create, write, remove, truncate).
    pub read_write: Vec<PathBuf>,
    /// Hierarchies whose files may be executed (implies read).
    pub exec: Vec<PathBuf>,
}

/// Network access.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum NetworkPolicy {
    /// No TCP bind/connect (Landlock ABI ≥ V4; see module docs for UDP).
    Deny,
    /// Unrestricted.
    Allow,
    /// TCP `connect` only to these ports, no `bind` (used by
    /// [`SandboxProfileName::NetworkRestricted`]).
    ConnectTcpPorts(Vec<u16>),
}

/// Capabilities left to the job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityPolicy {
    /// Drop every capability.
    DropAll,
    /// Drop all but these (names like `CAP_NET_BIND_SERVICE`).
    Keep(Vec<String>),
}

/// How to treat missing Landlock support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LandlockMode {
    /// Enforce what the kernel supports and report the rest honestly.
    BestEffort,
    /// Fail with [`crate::error::SandboxError`] unless the filesystem rules
    /// are fully enforced and (if requested) Landlock network rules exist.
    HardRequirement,
}

/// A complete sandbox policy (Job-Runtime-Doc §12).
///
/// Serializable so the `harw-job-exec` trampoline can receive it inside its
/// versioned launch plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxPolicy {
    /// Filesystem access lists.
    pub filesystem: FilesystemPolicy,
    /// Network access.
    pub network: NetworkPolicy,
    /// Capability reduction.
    pub capabilities: CapabilityPolicy,
    /// Set `PR_SET_NO_NEW_PRIVS`.
    pub no_new_privs: bool,
    /// Landlock strictness.
    pub landlock: LandlockMode,
}

fn paths(list: &[&str]) -> Vec<PathBuf> {
    list.iter().map(PathBuf::from).collect()
}

impl SandboxPolicy {
    /// The policy for a named profile, rooted at `workspace_root`.
    ///
    /// | profile              | workspace       | network          |
    /// |----------------------|-----------------|------------------|
    /// | `WorkspaceBuild`     | read/write/exec | allow            |
    /// | `ReadOnlyAnalysis`   | read only       | deny             |
    /// | `NoNetwork`          | read/write/exec | deny             |
    /// | `NetworkRestricted`  | read/write/exec | TCP connect 80/443 |
    ///
    /// All profiles: system paths readable, toolchain paths executable,
    /// `/tmp` and `/dev/null` writable, all capabilities dropped,
    /// `NO_NEW_PRIVS`, Landlock best effort. Tool caches outside the
    /// workspace (e.g. `~/.cargo`) must be added by the caller.
    #[must_use]
    pub fn from_profile(profile: SandboxProfileName, workspace_root: &Path) -> Self {
        let workspace = workspace_root.to_path_buf();
        let mut filesystem = FilesystemPolicy {
            read_only: paths(SYSTEM_READ),
            read_write: paths(SYSTEM_WRITE),
            exec: paths(SYSTEM_EXEC),
        };
        let network = match profile {
            SandboxProfileName::WorkspaceBuild => {
                filesystem.read_write.push(workspace.clone());
                filesystem.exec.push(workspace);
                NetworkPolicy::Allow
            }
            SandboxProfileName::ReadOnlyAnalysis => {
                filesystem.read_only.push(workspace);
                NetworkPolicy::Deny
            }
            SandboxProfileName::NoNetwork => {
                filesystem.read_write.push(workspace.clone());
                filesystem.exec.push(workspace);
                NetworkPolicy::Deny
            }
            SandboxProfileName::NetworkRestricted => {
                filesystem.read_write.push(workspace.clone());
                filesystem.exec.push(workspace);
                NetworkPolicy::ConnectTcpPorts(RESTRICTED_CONNECT_PORTS.to_vec())
            }
        };
        Self {
            filesystem,
            network,
            capabilities: CapabilityPolicy::DropAll,
            no_new_privs: true,
            landlock: LandlockMode::BestEffort,
        }
    }

    /// Adds a read-only hierarchy (builder style).
    #[must_use]
    pub fn with_read_only(mut self, path: impl Into<PathBuf>) -> Self {
        self.filesystem.read_only.push(path.into());
        self
    }

    /// Adds a read-write hierarchy (builder style).
    #[must_use]
    pub fn with_read_write(mut self, path: impl Into<PathBuf>) -> Self {
        self.filesystem.read_write.push(path.into());
        self
    }

    /// Switches to [`LandlockMode::HardRequirement`] (builder style).
    #[must_use]
    pub fn hard_requirement(mut self) -> Self {
        self.landlock = LandlockMode::HardRequirement;
        self
    }
}

/// Effective access class of one path after merging all lists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FsAccess {
    /// Read files, list directories.
    pub read: bool,
    /// Create, write, remove, truncate.
    pub write: bool,
    /// Execute files.
    pub exec: bool,
}

/// Landlock rules derived from a policy (pure data, no syscalls).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandlockPlan {
    /// One entry per distinct path, sorted by path.
    pub rules: Vec<(PathBuf, FsAccess)>,
    /// Whether TCP bind/connect are handled (denied unless a port rule
    /// allows them).
    pub restrict_network: bool,
    /// TCP ports allowed for `connect`.
    pub connect_ports: Vec<u16>,
}

/// Plans the Landlock rules for `policy`: duplicate paths are merged (a
/// workspace in both `read_write` and `exec` gets both), `exec` implies
/// `read`.
#[must_use]
pub fn plan_landlock(policy: &SandboxPolicy) -> LandlockPlan {
    let mut merged: BTreeMap<PathBuf, FsAccess> = BTreeMap::new();
    let fs = &policy.filesystem;
    for path in &fs.read_only {
        merged.entry(path.clone()).or_default().read = true;
    }
    for path in &fs.read_write {
        let entry = merged.entry(path.clone()).or_default();
        entry.read = true;
        entry.write = true;
    }
    for path in &fs.exec {
        let entry = merged.entry(path.clone()).or_default();
        entry.read = true;
        entry.exec = true;
    }
    let (restrict_network, mut connect_ports) = match &policy.network {
        NetworkPolicy::Allow => (false, Vec::new()),
        NetworkPolicy::Deny => (true, Vec::new()),
        NetworkPolicy::ConnectTcpPorts(ports) => (true, ports.clone()),
    };
    connect_ports.sort_unstable();
    connect_ports.dedup();
    LandlockPlan {
        rules: merged.into_iter().collect(),
        restrict_network,
        connect_ports,
    }
}

#[cfg(feature = "linux-sandbox")]
pub use enforce::{PreparedRuleset, apply_to_current_process, build_ruleset};

#[cfg(feature = "linux-sandbox")]
mod enforce {
    use std::path::PathBuf;

    use harw_job_core::{EnforcementState, SandboxReport};
    use landlock::{
        ABI, Access, AccessFs, AccessNet, BitFlags, CompatLevel, Compatible, LandlockStatus,
        NetPort, PathBeneath, PathFd, PathFdError, RestrictionStatus, Ruleset, RulesetAttr,
        RulesetCreated, RulesetCreatedAttr, RulesetStatus,
    };

    use super::{CapabilityPolicy, FsAccess, LandlockMode, SandboxPolicy, plan_landlock};
    use crate::capabilities::{drop_capabilities, parse_keep_list};
    use crate::error::{SandboxComponent, SandboxError};

    /// Landlock ABI this crate is written and tested against (Linux 6.10).
    const TARGET_ABI: ABI = ABI::V5;

    /// A created (not yet enforced) Landlock ruleset. Building it opens the
    /// policy paths and creates a ruleset fd; it restricts nothing.
    #[derive(Debug)]
    pub struct PreparedRuleset {
        created: RulesetCreated,
        skipped: Vec<PathBuf>,
        mode: LandlockMode,
        restrict_network: bool,
    }

    impl PreparedRuleset {
        /// Policy paths that do not exist on this host and got no rule
        /// (e.g. `/lib64` on some distributions).
        #[must_use]
        pub fn skipped_paths(&self) -> &[PathBuf] {
            &self.skipped
        }
    }

    fn fs_bits(access: FsAccess, is_dir: bool) -> BitFlags<AccessFs> {
        let mut bits = BitFlags::<AccessFs>::empty();
        if access.read {
            bits |= AccessFs::ReadFile | AccessFs::ReadDir;
        }
        if access.write {
            bits |= AccessFs::from_write(TARGET_ABI);
        }
        if access.exec {
            bits |= AccessFs::Execute;
        }
        if is_dir {
            bits
        } else {
            bits & AccessFs::from_file(TARGET_ABI)
        }
    }

    fn landlock_error(
        mode: LandlockMode,
        component: SandboxComponent,
        error: &dyn std::fmt::Display,
    ) -> SandboxError {
        match mode {
            LandlockMode::HardRequirement => SandboxError::Unsupported { component },
            LandlockMode::BestEffort => SandboxError::Landlock {
                message: error.to_string(),
            },
        }
    }

    /// Builds the Landlock ruleset for `policy` without enforcing it.
    ///
    /// Safe to call anywhere (tests included): it only opens `O_PATH` fds
    /// and creates a ruleset fd.
    ///
    /// # Errors
    /// [`SandboxError::Unsupported`] in hard mode when the kernel lacks the
    /// requested Landlock features; [`SandboxError::InvalidPath`] for a
    /// policy path that exists but cannot be opened;
    /// [`SandboxError::Landlock`] otherwise.
    pub fn build_ruleset(policy: &SandboxPolicy) -> Result<PreparedRuleset, SandboxError> {
        let plan = plan_landlock(policy);
        let mode = policy.landlock;
        let level = match mode {
            LandlockMode::BestEffort => CompatLevel::BestEffort,
            LandlockMode::HardRequirement => CompatLevel::HardRequirement,
        };
        let mut ruleset = Ruleset::default()
            .set_compatibility(level)
            .handle_access(AccessFs::from_all(TARGET_ABI))
            .map_err(|error| landlock_error(mode, SandboxComponent::Filesystem, &error))?;
        if plan.restrict_network {
            ruleset = ruleset
                .handle_access(AccessNet::from_all(TARGET_ABI))
                .map_err(|error| landlock_error(mode, SandboxComponent::Network, &error))?;
        }
        let mut created = ruleset
            .create()
            .map_err(|error| landlock_error(mode, SandboxComponent::Filesystem, &error))?;
        let mut skipped = Vec::new();
        for (path, access) in plan.rules {
            let fd = match PathFd::new(&path) {
                Ok(fd) => fd,
                Err(PathFdError::OpenCall { source, .. })
                    if source.kind() == std::io::ErrorKind::NotFound =>
                {
                    skipped.push(path);
                    continue;
                }
                Err(PathFdError::OpenCall { source, .. }) => {
                    return Err(SandboxError::InvalidPath { path, source });
                }
                Err(other) => {
                    return Err(SandboxError::Landlock {
                        message: other.to_string(),
                    });
                }
            };
            let bits = fs_bits(access, path.is_dir());
            if bits.is_empty() {
                continue;
            }
            created = created
                .add_rule(PathBeneath::new(fd, bits))
                .map_err(|error| SandboxError::Landlock {
                    message: format!("{}: {error}", path.display()),
                })?;
        }
        for port in plan.connect_ports {
            created = created
                .add_rule(NetPort::new(port, AccessNet::ConnectTcp))
                .map_err(|error| SandboxError::Landlock {
                    message: format!("tcp port {port}: {error}"),
                })?;
        }
        let created = created.no_new_privs(policy.no_new_privs);
        Ok(PreparedRuleset {
            created,
            skipped,
            mode,
            restrict_network: plan.restrict_network,
        })
    }

    fn effective_abi(status: &RestrictionStatus) -> Option<ABI> {
        match status.landlock {
            LandlockStatus::Available { effective_abi, .. } => Some(effective_abi),
            _ => None,
        }
    }

    /// Maps a Landlock restriction status to per-dimension states.
    fn landlock_states(
        status: &RestrictionStatus,
        restrict_network: bool,
    ) -> (EnforcementState, EnforcementState) {
        let Some(abi) = effective_abi(status) else {
            let network = if restrict_network {
                EnforcementState::Unsupported
            } else {
                EnforcementState::Enforced
            };
            return (EnforcementState::Unsupported, network);
        };
        let filesystem = match status.ruleset {
            RulesetStatus::NotEnforced => EnforcementState::NotEnforced,
            _ if abi >= TARGET_ABI => EnforcementState::Enforced,
            _ => EnforcementState::Partial,
        };
        let network = if !restrict_network {
            EnforcementState::Enforced
        } else if matches!(status.ruleset, RulesetStatus::NotEnforced) || abi < ABI::V4 {
            EnforcementState::NotEnforced
        } else {
            // TCP only: UDP/raw sockets are outside Landlock's reach.
            EnforcementState::Partial
        };
        (filesystem, network)
    }

    /// Applies `policy` to the **calling thread** (irreversible) and reports
    /// what is actually enforced.
    ///
    /// Order: validate capability names → build the Landlock ruleset (opens
    /// paths while still unrestricted) → `NO_NEW_PRIVS` → drop capabilities
    /// (bounding set first, while `CAP_SETPCAP` may still be effective) →
    /// Landlock `restrict_self`.
    ///
    /// # Errors
    /// Build errors as [`build_ruleset`]; capability errors; in
    /// [`LandlockMode::HardRequirement`], [`SandboxError::PartiallyEnforced`]
    /// or [`SandboxError::Unsupported`] when the result falls short — the
    /// thread may then already be partially restricted and must not `exec`
    /// the job.
    pub fn apply_to_current_process(policy: &SandboxPolicy) -> Result<SandboxReport, SandboxError> {
        let keep = match &policy.capabilities {
            CapabilityPolicy::DropAll => Vec::new(),
            CapabilityPolicy::Keep(names) => names.clone(),
        };
        parse_keep_list(&keep)?;
        let prepared = build_ruleset(policy)?;

        let no_new_privs = if policy.no_new_privs {
            rustix::thread::set_no_new_privs(true).map_err(|errno| SandboxError::Os {
                operation: "PR_SET_NO_NEW_PRIVS",
                errno: errno.raw_os_error(),
            })?;
            if rustix::thread::no_new_privs().unwrap_or(false) {
                EnforcementState::Enforced
            } else {
                EnforcementState::NotEnforced
            }
        } else {
            EnforcementState::NotEnforced
        };

        let capabilities = drop_capabilities(&keep, no_new_privs == EnforcementState::Enforced)?;

        let PreparedRuleset {
            created,
            mode,
            restrict_network,
            ..
        } = prepared;
        let status = created.restrict_self().map_err(|error| match error {
            landlock::RulesetError::RestrictSelf(_) if mode == LandlockMode::HardRequirement => {
                SandboxError::Unsupported {
                    component: SandboxComponent::Filesystem,
                }
            }
            other => SandboxError::Landlock {
                message: other.to_string(),
            },
        })?;
        let (filesystem, network) = landlock_states(&status, restrict_network);

        if mode == LandlockMode::HardRequirement {
            if filesystem != EnforcementState::Enforced {
                return Err(SandboxError::PartiallyEnforced {
                    component: SandboxComponent::Filesystem,
                    state: filesystem,
                });
            }
            if restrict_network && network != EnforcementState::Partial {
                return Err(SandboxError::PartiallyEnforced {
                    component: SandboxComponent::Network,
                    state: network,
                });
            }
        }

        let report = SandboxReport {
            filesystem,
            network,
            no_new_privs,
            capabilities,
            resource_limits: EnforcementState::NotEnforced,
        };
        tracing::info!(?report, "sandbox applied to calling thread");
        Ok(report)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::test_support::{TestResult, ctx};
        use harw_job_core::SandboxProfileName;

        #[test]
        fn test_fs_bits_for_files_drop_directory_rights() {
            let all = FsAccess {
                read: true,
                write: true,
                exec: true,
            };
            let dir = fs_bits(all, true);
            let file = fs_bits(all, false);
            assert!(dir.contains(AccessFs::ReadDir));
            assert!(dir.contains(AccessFs::MakeDir));
            assert!(!file.contains(AccessFs::ReadDir));
            assert!(!file.contains(AccessFs::MakeDir));
            assert!(file.contains(AccessFs::ReadFile | AccessFs::WriteFile | AccessFs::Execute));
            let read_only = fs_bits(
                FsAccess {
                    read: true,
                    ..FsAccess::default()
                },
                true,
            );
            assert!(!read_only.contains(AccessFs::WriteFile));
            assert!(!read_only.contains(AccessFs::Execute));
        }

        #[test]
        fn test_build_ruleset_does_not_restrict() -> TestResult {
            // Building only creates fds; restrict_self is never called here
            // (it would sandbox the test runner thread).
            let workspace = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let policy =
                SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, workspace.path())
                    .with_read_only("/nonexistent/harw-job-linux");
            let prepared = build_ruleset(&policy).map_err(ctx("build ruleset"))?;
            assert!(
                prepared
                    .skipped_paths()
                    .iter()
                    .any(|path| path == std::path::Path::new("/nonexistent/harw-job-linux"))
            );
            drop(prepared);
            // Still unrestricted: the workspace stays writable.
            std::fs::write(workspace.path().join("probe"), b"ok").map_err(ctx("write probe"))?;
            Ok(())
        }

        #[test]
        fn test_build_ruleset_all_profiles() -> TestResult {
            let workspace = tempfile::tempdir().map_err(ctx("tempdir"))?;
            for profile in [
                SandboxProfileName::WorkspaceBuild,
                SandboxProfileName::ReadOnlyAnalysis,
                SandboxProfileName::NoNetwork,
                SandboxProfileName::NetworkRestricted,
            ] {
                let policy = SandboxPolicy::from_profile(profile, workspace.path());
                build_ruleset(&policy).map_err(ctx("build ruleset"))?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn access_of(plan: &LandlockPlan, path: &Path) -> Option<FsAccess> {
        plan.rules
            .iter()
            .find(|(candidate, _)| candidate == path)
            .map(|(_, access)| *access)
    }

    #[test]
    fn test_from_profile_workspace_build() {
        let root = Path::new("/work/repo");
        let policy = SandboxPolicy::from_profile(SandboxProfileName::WorkspaceBuild, root);
        assert_eq!(policy.network, NetworkPolicy::Allow);
        assert_eq!(policy.capabilities, CapabilityPolicy::DropAll);
        assert!(policy.no_new_privs);
        assert_eq!(policy.landlock, LandlockMode::BestEffort);
        let plan = plan_landlock(&policy);
        assert_eq!(
            access_of(&plan, root),
            Some(FsAccess {
                read: true,
                write: true,
                exec: true
            })
        );
        assert!(!plan.restrict_network);
    }

    #[test]
    fn test_from_profile_read_only_analysis() {
        let root = Path::new("/work/repo");
        let policy = SandboxPolicy::from_profile(SandboxProfileName::ReadOnlyAnalysis, root);
        assert_eq!(policy.network, NetworkPolicy::Deny);
        let plan = plan_landlock(&policy);
        assert_eq!(
            access_of(&plan, root),
            Some(FsAccess {
                read: true,
                write: false,
                exec: false
            })
        );
        assert!(plan.restrict_network);
        assert!(plan.connect_ports.is_empty());
    }

    #[test]
    fn test_from_profile_no_network_and_restricted() {
        let root = Path::new("/work/repo");
        let no_net = SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, root);
        assert_eq!(no_net.network, NetworkPolicy::Deny);
        assert_eq!(
            access_of(&plan_landlock(&no_net), root).map(|access| access.write),
            Some(true)
        );
        let restricted = SandboxPolicy::from_profile(SandboxProfileName::NetworkRestricted, root);
        let plan = plan_landlock(&restricted);
        assert!(plan.restrict_network);
        assert_eq!(plan.connect_ports, vec![80, 443]);
    }

    #[test]
    fn test_plan_merges_duplicates_and_system_paths() {
        let policy =
            SandboxPolicy::from_profile(SandboxProfileName::WorkspaceBuild, Path::new("/w"))
                .with_read_only("/w")
                .with_read_write("/data")
                .hard_requirement();
        let plan = plan_landlock(&policy);
        let count = plan
            .rules
            .iter()
            .filter(|(path, _)| path == Path::new("/w"))
            .count();
        assert_eq!(count, 1);
        assert_eq!(
            access_of(&plan, Path::new("/usr")),
            Some(FsAccess {
                read: true,
                write: false,
                exec: true
            })
        );
        assert_eq!(
            access_of(&plan, Path::new("/tmp")).map(|access| (access.write, access.exec)),
            Some((true, false))
        );
        assert_eq!(
            access_of(&plan, Path::new("/etc")).map(|access| access.write),
            Some(false)
        );
        assert!(access_of(&plan, Path::new("/data")).is_some());
        assert_eq!(policy.landlock, LandlockMode::HardRequirement);
        // Sorted, deterministic order.
        let mut sorted = plan.rules.clone();
        sorted.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(sorted, plan.rules);
    }

    #[test]
    fn test_connect_ports_are_deduplicated() {
        let mut policy =
            SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, Path::new("/w"));
        policy.network = NetworkPolicy::ConnectTcpPorts(vec![443, 80, 443]);
        assert_eq!(plan_landlock(&policy).connect_ports, vec![80, 443]);
    }
}
