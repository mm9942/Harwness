//! Harw-native sandbox policy and its Linux enforcement (Job-Runtime-Doc
//! §3.5, §12, Phase 5).
//!
//! The policy types ([`SandboxPolicy`] and friends) and the pure rule
//! planning ([`plan_landlock`]) are always available. Enforcement
//! (`apply_to_current_process`, `build_ruleset`) needs feature
//! `linux-sandbox` (Landlock).
//!
//! # Inside a mount namespace
//! A trampoline running inside a Bubblewrap namespace sees bind
//! destinations, not host paths. [`PathMap`] describes that view
//! completely: binds (host path → in-sandbox destination, read-only or
//! read-write) and the mounts the namespace provides itself (`/proc`,
//! `/dev`, `/tmp`). [`SandboxPolicy::mapped`] rewrites a policy to that
//! view and [`plan_landlock_mapped`] plans its rules. Policy paths the map
//! does not cover are dropped (fail-closed). Policy entries beneath a
//! read-only bind lose write and, outside the system exec trees, execute —
//! the distinction a mount namespace cannot make. Landlock rights are
//! inherited, so a bind nested beneath a kept read-write or exec
//! destination is not covered by that cap;
//! [`PathMap::shadowed_read_only_binds`] reports such plans.
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
//!
//! # Host capability probe (side-effect free)
//! For runtime admission (PL-90) the host is probed without changing it:
//! [`landlock_lsm_active`] / [`lsm_list_contains_landlock`] read the LSM
//! list, [`userns_available`] / [`userns_from_sysctls`] read the user
//! namespace sysctls, and `landlock_support` (feature `linux-sandbox`)
//! determines the kernel Landlock ABI on a throwaway thread, so the calling
//! thread is never restricted. All results are [`LandlockSupport`] /
//! `Option<bool>` plain data.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use harw_job_core::{EnforcementState, SandboxProfileName};
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
    ///
    /// Every rule that grants write must also open exactly the object its
    /// path names: the ruleset builder reads the opened fd back through
    /// `/proc/self/fd` and refuses the policy when a symlink or a moved
    /// directory lies on the way. The write paths of such a policy must
    /// therefore be canonical (a per-job write scope, PL-97 E2, resolves
    /// them before planning).
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

    /// This policy with its write grant on `workspace_root` **replaced** by
    /// `paths` (a per-job write scope, PL-97 E2): every `read_write` entry
    /// at or beneath `workspace_root` is removed, the workspace root becomes
    /// `read_only` (its `exec` entry, if any, stays: execute never grants
    /// write), and each of `paths` becomes `read_write`. Network,
    /// capabilities and `NO_NEW_PRIVS` are unchanged; the Landlock mode
    /// becomes [`LandlockMode::HardRequirement`].
    ///
    /// Pure and lexical (component-wise, no symlink resolution): the caller
    /// passes canonical paths and checks existence. A path that does not
    /// exist gets no Landlock rule ([`plan_landlock`] keeps it, the ruleset
    /// builder skips it), so new files need an entry for an existing
    /// parent directory.
    ///
    /// Hard mode closes the window between that check and the ruleset
    /// build: a scope path lies in a directory other processes may write,
    /// so it could be swapped for a symlink in between. The ruleset builder
    /// then refuses every write rule whose opened object is not named by
    /// its planned path ([`LandlockMode::HardRequirement`]). A scope is a
    /// requirement anyway: scoped jobs are always `required`.
    ///
    /// # Errors
    /// - [`WriteScopeConflict::NoWorkspaceWrite`] when the policy grants no
    ///   write on `workspace_root` itself: a scope narrows a grant and never
    ///   widens the policy.
    /// - [`WriteScopeConflict::OutsideWorkspace`] for a path not at or
    ///   beneath `workspace_root`.
    /// - [`WriteScopeConflict::WritableAncestor`] when a remaining
    ///   `read_write` entry is an ancestor of `workspace_root` (e.g. `/tmp`
    ///   for a workspace below `/tmp`): Landlock rights are inherited, so the
    ///   whole workspace would stay writable.
    pub fn with_workspace_write_scope(
        &self,
        workspace_root: &Path,
        paths: &[PathBuf],
    ) -> Result<Self, WriteScopeConflict> {
        if !self
            .filesystem
            .read_write
            .iter()
            .any(|path| path == workspace_root)
        {
            return Err(WriteScopeConflict::NoWorkspaceWrite);
        }
        if let Some(outside) = paths.iter().find(|path| !path.starts_with(workspace_root)) {
            return Err(WriteScopeConflict::OutsideWorkspace(outside.clone()));
        }
        let mut policy = self.clone();
        let filesystem = &mut policy.filesystem;
        filesystem
            .read_write
            .retain(|path| !path.starts_with(workspace_root));
        if let Some(ancestor) = filesystem
            .read_write
            .iter()
            .find(|path| workspace_root.starts_with(path))
        {
            return Err(WriteScopeConflict::WritableAncestor(ancestor.clone()));
        }
        if !filesystem
            .read_only
            .iter()
            .any(|path| path == workspace_root)
        {
            filesystem.read_only.push(workspace_root.to_path_buf());
        }
        for path in paths {
            if !filesystem.read_write.contains(path) {
                filesystem.read_write.push(path.clone());
            }
        }
        Ok(policy.hard_requirement())
    }
}

/// Why [`SandboxPolicy::with_workspace_write_scope`] cannot narrow a
/// policy to a write scope.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum WriteScopeConflict {
    /// The policy grants no write access on the workspace root, so a scope
    /// would widen it.
    NoWorkspaceWrite,
    /// A scope path is not at or beneath the workspace root.
    OutsideWorkspace(PathBuf),
    /// A `read_write` entry outside the scope is an ancestor of the
    /// workspace root and would keep the whole workspace writable.
    WritableAncestor(PathBuf),
}

impl std::fmt::Display for WriteScopeConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoWorkspaceWrite => f.write_str(
                "the policy grants no write access on the workspace for a write scope to narrow",
            ),
            Self::OutsideWorkspace(path) => {
                write!(f, "write scope path {path:?} lies outside the workspace")
            }
            Self::WritableAncestor(path) => write!(
                f,
                "{path:?} stays writable and contains the workspace, so a write scope cannot \
                 narrow it (Landlock rights are inherited)"
            ),
        }
    }
}

impl std::error::Error for WriteScopeConflict {}

/// The Landlock ABI with which the filesystem dimension counts as fully
/// enforced (V5, Linux 6.10: the ABI this crate targets). Older ABIs give
/// [`EnforcementState::Partial`].
pub const LANDLOCK_TARGET_ABI: u8 = 5;

/// What Landlock can enforce on the filesystem dimension with the usable
/// ABI `abi` ([`LandlockSupport::usable_abi`]), predicted before anything
/// runs: `Unsupported` without Landlock, `Partial` below
/// [`LANDLOCK_TARGET_ABI`], `Enforced` from it on. Matches what
/// `apply_to_current_process` reports for a ruleset it could restrict.
#[must_use]
pub fn landlock_filesystem_state(abi: Option<u8>) -> EnforcementState {
    match abi {
        None | Some(0) => EnforcementState::Unsupported,
        Some(abi) if abi < LANDLOCK_TARGET_ABI => EnforcementState::Partial,
        Some(_) => EnforcementState::Enforced,
    }
}

/// The first Landlock ABI with TCP `bind`/`connect` rules (V4, Linux 6.7).
const LANDLOCK_NETWORK_ABI: u8 = 4;

/// What Landlock can enforce on the network dimension of `network` with
/// the usable ABI `abi`, predicted before anything runs (as
/// [`landlock_filesystem_state`]): `Enforced` when `network` restricts
/// nothing; otherwise `Unsupported` without Landlock, `NotEnforced` below
/// ABI 4 (no network rules) and `Partial` from ABI 4 on (TCP only: UDP,
/// raw and abstract sockets stay reachable). Matches what
/// `apply_to_current_process` reports. Only a network namespace enforces a
/// restricting network policy fully.
#[must_use]
pub fn landlock_network_state(network: &NetworkPolicy, abi: Option<u8>) -> EnforcementState {
    if *network == NetworkPolicy::Allow {
        return EnforcementState::Enforced;
    }
    match abi {
        None | Some(0) => EnforcementState::Unsupported,
        Some(abi) if abi < LANDLOCK_NETWORK_ABI => EnforcementState::NotEnforced,
        Some(_) => EnforcementState::Partial,
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

/// How a hierarchy is mounted into a mount namespace (one entry of a
/// [`PathMap`]).
///
/// A mount namespace distinguishes read from write (`--ro-bind` vs.
/// `--bind`) but not read from execute; Landlock can. The mode caps the
/// Landlock rights a policy path beneath the entry keeps in
/// [`FilesystemPolicy::mapped`].
///
/// # Inheritance limit
/// Landlock rights are inherited down the tree, and the cap applies to the
/// policy entries **beneath** a read-only bind, not to the bind as a whole.
/// A kept `exec` or `read_write` rule on an **ancestor** of the bind's
/// destination still reaches into it, e.g. a read-only `.git` bind nested
/// inside the read-write workspace destination keeps execute.
/// [`PathMap::shadowed_read_only_binds`] reports such binds; a launcher
/// must refuse the plan (or not nest the bind).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindMode {
    /// Read-only (`--ro-bind`, or a read-only mount the namespace provides
    /// itself). Policy paths beneath it keep read access only: write is
    /// dropped (the mount refuses it anyway) and execute is dropped too,
    /// except where the in-sandbox destination lies in a system exec tree
    /// (`/usr`, `/bin`, `/sbin`, `/lib`, `/lib64`, `/lib32`). See the
    /// inheritance limit above.
    ReadOnly,
    /// Read-write (`--bind`, a tmpfs): the policy's rights are kept.
    ReadWrite,
}

/// A [`PathMap`] path that cannot describe a mount: relative, or with a
/// `..` component.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathMapError {
    /// The rejected path.
    pub path: PathBuf,
}

impl std::fmt::Display for PathMapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "path map entry {:?}: must be absolute without `..` components",
            self.path
        )
    }
}

impl std::error::Error for PathMapError {}

/// Whether `path` is absolute without `..` components: the only form a
/// [`PathMap`] maps. Component-wise prefix matching is sound only for such
/// paths (`/repo/../../etc` lies "beneath" `/repo` textually, not on disk).
fn is_mappable(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|component| !matches!(component, Component::ParentDir))
}

fn mappable(path: PathBuf) -> Result<PathBuf, PathMapError> {
    if is_mappable(&path) {
        Ok(path)
    } else {
        Err(PathMapError { path })
    }
}

/// One entry of a [`PathMap`]: the host hierarchy `host` is visible inside
/// the sandbox at `sandbox`. Both paths are absolute without `..`
/// components; [`PathMap::try_with_bind`] and deserialization reject
/// anything else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawPathBind")]
pub struct PathBind {
    /// Host path.
    pub host: PathBuf,
    /// Destination inside the mount namespace.
    pub sandbox: PathBuf,
    /// Access the mount grants.
    pub mode: BindMode,
}

/// Wire form of a [`PathBind`] before validation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPathBind {
    host: PathBuf,
    sandbox: PathBuf,
    mode: BindMode,
}

impl TryFrom<RawPathBind> for PathBind {
    type Error = PathMapError;

    fn try_from(raw: RawPathBind) -> Result<Self, Self::Error> {
        Ok(Self {
            host: mappable(raw.host)?,
            sandbox: mappable(raw.sandbox)?,
            mode: raw.mode,
        })
    }
}

/// The view of a mount namespace (e.g. a Bubblewrap plan): which host
/// hierarchies are visible where inside it, and which mounts the namespace
/// provides itself (Bubblewrap's fresh `/proc`, minimal `/dev`, empty
/// `/tmp`). Used to plan Landlock rules against the view the sandboxed
/// process actually has ([`plan_landlock_mapped`]).
///
/// The map is the **complete** description of that view. A policy path no
/// entry covers is not visible inside the namespace, or something else is
/// mounted there, so [`FilesystemPolicy::mapped`] drops it instead of
/// keeping its host spelling ([`PathMap::unmapped_paths`] lists what was
/// dropped). An empty map describes an empty view.
///
/// Paths are compared component-wise (`/work/repo2` is not beneath
/// `/work/repo`). When entries nest on the host side, the one with the
/// longest host path wins; of two with the same host path the later one
/// wins. Every entry is absolute without `..` components; a policy path
/// that is not is unmappable and dropped as well. Symlinks are not
/// resolved.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathMap {
    binds: Vec<PathBind>,
}

impl PathMap {
    /// An empty map: an empty view, nothing is reachable.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a bind: the host hierarchy `host` is visible at `sandbox`
    /// (builder style).
    ///
    /// # Errors
    /// [`PathMapError`] when `host` or `sandbox` is relative (an empty path
    /// included) or has a `..` component.
    pub fn try_with_bind(
        mut self,
        host: impl Into<PathBuf>,
        sandbox: impl Into<PathBuf>,
        mode: BindMode,
    ) -> Result<Self, PathMapError> {
        self.binds.push(PathBind {
            host: mappable(host.into())?,
            sandbox: mappable(sandbox.into())?,
            mode,
        });
        Ok(self)
    }

    /// Declares a mount the namespace provides itself at `path`, such as
    /// Bubblewrap's fresh procfs at `/proc`, its minimal `/dev` or its empty
    /// `/tmp` tmpfs (builder style). Policy paths beneath `path` keep their
    /// spelling, capped by `mode`; they then name the namespace's own
    /// mount, not the host content. Recorded as a bind of `path` onto
    /// itself.
    ///
    /// # Errors
    /// As [`PathMap::try_with_bind`].
    pub fn try_with_provided(
        self,
        path: impl Into<PathBuf>,
        mode: BindMode,
    ) -> Result<Self, PathMapError> {
        let path = path.into();
        self.try_with_bind(path.clone(), path, mode)
    }

    /// The entries in insertion order.
    #[must_use]
    pub fn binds(&self) -> &[PathBind] {
        &self.binds
    }

    /// Whether the map has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.binds.is_empty()
    }

    /// Where `host_path` is visible inside the sandbox, and through which
    /// kind of mount; `None` if no entry covers it or it is unmappable
    /// (relative, or with a `..` component).
    #[must_use]
    pub fn resolve(&self, host_path: &Path) -> Option<(PathBuf, BindMode)> {
        if !is_mappable(host_path) {
            return None;
        }
        let mut best: Option<(&PathBind, &Path, usize)> = None;
        for bind in &self.binds {
            let Ok(rest) = host_path.strip_prefix(&bind.host) else {
                continue;
            };
            let depth = bind.host.components().count();
            // `>=`: of two entries with the same host path the later wins.
            if best.is_none_or(|(_, _, best_depth)| depth >= best_depth) {
                best = Some((bind, rest, depth));
            }
        }
        best.map(|(bind, rest, _)| {
            // `join("")` would append a trailing separator, which turns a
            // file path into an invalid directory path (`/dev/null/`).
            let destination = if rest.as_os_str().is_empty() {
                bind.sandbox.clone()
            } else {
                bind.sandbox.join(rest)
            };
            (destination, bind.mode)
        })
    }

    /// The paths of `policy` that [`FilesystemPolicy::mapped`] drops: not
    /// covered by any entry, or unmappable. Sorted, without duplicates.
    #[must_use]
    pub fn unmapped_paths(&self, policy: &FilesystemPolicy) -> Vec<PathBuf> {
        let mut unmapped: Vec<PathBuf> = policy
            .read_only
            .iter()
            .chain(&policy.read_write)
            .chain(&policy.exec)
            .filter(|path| self.resolve(path).is_none())
            .cloned()
            .collect();
        unmapped.sort();
        unmapped.dedup();
        unmapped
    }

    /// Read-only binds whose Landlock access would **not** be read-only:
    /// their destination lies at or beneath a destination that keeps
    /// `read_write`, or `exec` outside the system exec trees, in
    /// `policy.mapped(self)` (see the inheritance limit on
    /// [`BindMode`]). Write stays refused by the read-only mount, execute
    /// does not.
    ///
    /// A launcher must refuse a plan for which this is not empty (or
    /// re-arrange its binds); the mapping itself cannot narrow an
    /// ancestor's rule without taking it away from the ancestor.
    #[must_use]
    pub fn shadowed_read_only_binds(&self, policy: &FilesystemPolicy) -> Vec<&PathBind> {
        let mapped = policy.mapped(self);
        let reaching: Vec<&Path> = mapped
            .read_write
            .iter()
            .chain(mapped.exec.iter().filter(|path| !in_system_exec_tree(path)))
            .map(PathBuf::as_path)
            .collect();
        self.binds
            .iter()
            .filter(|bind| bind.mode == BindMode::ReadOnly)
            .filter(|bind| {
                reaching
                    .iter()
                    .any(|ancestor| bind.sandbox.starts_with(ancestor))
            })
            .collect()
    }
}

/// Whether `path` is one of the system exec trees or beneath one.
fn in_system_exec_tree(path: &Path) -> bool {
    SYSTEM_EXEC.iter().any(|tree| path.starts_with(tree))
}

impl FilesystemPolicy {
    /// This policy as seen from inside the mount namespace `map` describes.
    ///
    /// Every path is rewritten to its in-sandbox destination
    /// ([`PathMap::resolve`]). A path no entry covers, or an unmappable one
    /// (relative, or with a `..` component), is dropped: it gets no rule,
    /// so Landlock denies it ([`PathMap::unmapped_paths`] lists them).
    /// Rights are then capped by the covering entry ([`BindMode`]):
    /// - read-write entries keep the policy's rights;
    /// - read-only entries turn `read_write` and `exec` entries into
    ///   `read_only` ones, except `exec` entries whose destination lies in
    ///   a system exec tree.
    ///
    /// Mapping never grants a path the policy does not name, and never
    /// more than the policy grants. The cap holds for the policy entries
    /// beneath a read-only bind; for rules inherited from an ancestor see
    /// [`PathMap::shadowed_read_only_binds`].
    #[must_use]
    pub fn mapped(&self, map: &PathMap) -> Self {
        let mut mapped = Self::default();
        for path in &self.read_only {
            if let Some((destination, _)) = map.resolve(path) {
                mapped.read_only.push(destination);
            }
        }
        for path in &self.read_write {
            match map.resolve(path) {
                Some((destination, BindMode::ReadWrite)) => mapped.read_write.push(destination),
                Some((destination, BindMode::ReadOnly)) => mapped.read_only.push(destination),
                None => {}
            }
        }
        for path in &self.exec {
            match map.resolve(path) {
                Some((destination, BindMode::ReadWrite)) => mapped.exec.push(destination),
                Some((destination, BindMode::ReadOnly)) if in_system_exec_tree(&destination) => {
                    mapped.exec.push(destination);
                }
                Some((destination, BindMode::ReadOnly)) => mapped.read_only.push(destination),
                None => {}
            }
        }
        mapped
    }
}

impl SandboxPolicy {
    /// This policy with its filesystem lists rewritten to the namespace
    /// view ([`FilesystemPolicy::mapped`]); network, capability,
    /// `NO_NEW_PRIVS` and Landlock-mode settings are unchanged.
    ///
    /// This is the policy to hand to a trampoline running **inside** the
    /// namespace: `build_ruleset` / `apply_to_current_process` plan exactly
    /// [`plan_landlock_mapped`]`(self, map)` from it.
    #[must_use]
    pub fn mapped(&self, map: &PathMap) -> Self {
        Self {
            filesystem: self.filesystem.mapped(map),
            ..self.clone()
        }
    }
}

/// Plans the Landlock rules for `policy` as seen from inside the mount
/// namespace `map` describes: [`plan_landlock`] of
/// [`SandboxPolicy::mapped`]. A process without its own mount namespace
/// uses [`plan_landlock`] directly; so does a map that binds `/` onto
/// itself read-write, which yields the identical plan.
#[must_use]
pub fn plan_landlock_mapped(policy: &SandboxPolicy, map: &PathMap) -> LandlockPlan {
    plan_landlock(&policy.mapped(map))
}

/// Kernel LSM list (`/sys/kernel/security/lsm`, needs securityfs).
const LSM_LIST: &str = "/sys/kernel/security/lsm";
/// Debian/Ubuntu switch for unprivileged user namespaces.
const USERNS_CLONE_SWITCH: &str = "/proc/sys/kernel/unprivileged_userns_clone";
/// Upstream per-user namespace limit (`0` disables user namespaces).
const USERNS_MAX: &str = "/proc/sys/user/max_user_namespaces";

/// Landlock support of the running host (host capability probe for
/// runtime admission). Plain data; no landlock type leaks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LandlockSupport {
    /// Whether `landlock` is in the active LSM list
    /// (`/sys/kernel/security/lsm`). `None` if that file is unreadable
    /// (securityfs not mounted, or no permission).
    pub lsm_active: Option<bool>,
    /// Landlock ABI of the running kernel: `Some(n)` with `n >= 1` when
    /// Landlock works at ABI `n` (a kernel newer than the `landlock` crate
    /// reports its own, higher number); `Some(0)` when the kernel reports
    /// Landlock as not implemented or not enabled; `None` when it could not
    /// be determined (feature `linux-sandbox` off, or the probe thread
    /// failed).
    pub abi: Option<u8>,
}

impl LandlockSupport {
    /// The usable Landlock ABI (`>= 1`), if any.
    #[must_use]
    pub fn usable_abi(&self) -> Option<u8> {
        self.abi.filter(|abi| *abi >= 1)
    }
}

/// Whether an LSM list (the comma-separated content of
/// `/sys/kernel/security/lsm`) names `landlock`. Pure.
#[must_use]
pub fn lsm_list_contains_landlock(content: &str) -> bool {
    content
        .trim()
        .split(',')
        .any(|name| name.trim() == "landlock")
}

/// Reads `/sys/kernel/security/lsm`: `Some(true)` if Landlock is an active
/// LSM, `Some(false)` if not, `None` if the file is unreadable. Read-only.
#[must_use]
pub fn landlock_lsm_active() -> Option<bool> {
    std::fs::read_to_string(LSM_LIST)
        .ok()
        .map(|content| lsm_list_contains_landlock(&content))
}

/// Combines the two user-namespace sysctls into availability of
/// *unprivileged* user namespaces (what an unprivileged `bwrap` needs).
/// Pure.
///
/// - `unprivileged_userns_clone` (Debian/Ubuntu patch): `0` → `false`.
/// - `max_user_namespaces`: `0` → `false`.
/// - Otherwise `true` if at least one of them was readable and parsed,
///   `None` if neither says anything.
///
/// LSM policies (e.g. Ubuntu's AppArmor
/// `apparmor_restrict_unprivileged_userns`) can still deny namespace
/// creation for individual binaries; this is a host-level upper bound.
#[must_use]
pub fn userns_from_sysctls(
    unprivileged_userns_clone: Option<&str>,
    max_user_namespaces: Option<&str>,
) -> Option<bool> {
    let parse = |value: Option<&str>| value.and_then(|text| text.trim().parse::<u64>().ok());
    let clone = parse(unprivileged_userns_clone);
    let max = parse(max_user_namespaces);
    if clone == Some(0) || max == Some(0) {
        Some(false)
    } else if clone.is_some() || max.is_some() {
        Some(true)
    } else {
        None
    }
}

/// Whether unprivileged user namespaces are available on this host (see
/// [`userns_from_sysctls`]). Read-only: only reads two sysctl files and
/// never creates a namespace.
#[must_use]
pub fn userns_available() -> Option<bool> {
    let clone = std::fs::read_to_string(USERNS_CLONE_SWITCH).ok();
    let max = std::fs::read_to_string(USERNS_MAX).ok();
    userns_from_sysctls(clone.as_deref(), max.as_deref())
}

#[cfg(feature = "linux-sandbox")]
pub use enforce::{PreparedRuleset, apply_to_current_process, build_ruleset, landlock_support};

#[cfg(feature = "linux-sandbox")]
mod enforce {
    use std::os::fd::{AsFd as _, AsRawFd as _};
    use std::path::{Path, PathBuf};

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

    /// Whether the object `fd` refers to is a directory (`fstat` on the fd,
    /// not a second lookup of `path`, which could name another object by
    /// now).
    fn opened_is_dir(fd: &PathFd, path: &Path) -> Result<bool, SandboxError> {
        rustix::fs::fstat(fd)
            .map(|stat| rustix::fs::FileType::from_raw_mode(stat.st_mode).is_dir())
            .map_err(|errno| SandboxError::InvalidPath {
                path: path.to_path_buf(),
                source: errno.into(),
            })
    }

    /// Refuses a rule whose opened object is not the one `path` names
    /// without symlinks: `/proc/self/fd/N` reads back the path of the
    /// object the fd refers to, so a symlink or a moved directory anywhere
    /// on the way (planted after the path was checked) makes the two
    /// differ. `path` must be canonical.
    fn verify_opened_path(fd: &PathFd, path: &Path) -> Result<(), SandboxError> {
        let invalid = |source: std::io::Error| SandboxError::InvalidPath {
            path: path.to_path_buf(),
            source,
        };
        let magic = format!("/proc/self/fd/{}", fd.as_fd().as_raw_fd());
        let opened = std::fs::read_link(magic).map_err(invalid)?;
        if opened == path {
            Ok(())
        } else {
            Err(invalid(std::io::Error::other(format!(
                "opened {}, not the planned path: a symlink or a moved directory lies on the \
                 way, and a hard-requirement write rule must name its target exactly",
                opened.display()
            ))))
        }
    }

    /// Builds the Landlock ruleset for `policy` without enforcing it.
    ///
    /// Safe to call anywhere (tests included): it only opens `O_PATH` fds
    /// and creates a ruleset fd. A rule's rights follow the object its fd
    /// refers to (directory or not). In [`LandlockMode::HardRequirement`]
    /// every rule that grants write is read back after opening and must
    /// name exactly its planned path (no symlink, no moved directory on the
    /// way): a per-job write scope (PL-97 E2) is checked when the attempt
    /// is prepared and opened here later, and a path swapped in between
    /// must not carry the write grant elsewhere.
    ///
    /// # Errors
    /// [`SandboxError::Unsupported`] in hard mode when the kernel lacks the
    /// requested Landlock features; [`SandboxError::InvalidPath`] for a
    /// policy path that exists but cannot be opened, or (hard mode) a write
    /// rule that opened another object than its path names;
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
            if access.write && mode == LandlockMode::HardRequirement {
                verify_opened_path(&fd, &path)?;
            }
            let bits = fs_bits(access, opened_is_dir(&fd, &path)?);
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

    /// Number of a Landlock ABI (`0` for unsupported).
    fn abi_number(abi: ABI) -> u8 {
        match abi {
            ABI::Unsupported => 0,
            ABI::V1 => 1,
            ABI::V2 => 2,
            ABI::V3 => 3,
            ABI::V4 => 4,
            ABI::V5 => 5,
            ABI::V6 => 6,
            ABI::V7 => 7,
            ABI::V8 => 8,
            // Newer variants of a future `landlock` release: at least V9.
            _ => 9,
        }
    }

    /// Kernel Landlock ABI from a restriction status (see
    /// [`super::LandlockSupport::abi`]).
    fn kernel_abi_of(status: &RestrictionStatus) -> u8 {
        match status.landlock {
            LandlockStatus::Available {
                effective_abi,
                kernel_abi,
            } => kernel_abi
                .and_then(|raw| u8::try_from(raw).ok())
                .unwrap_or_else(|| abi_number(effective_abi)),
            LandlockStatus::NotEnabled | LandlockStatus::NotImplemented => 0,
        }
    }

    /// Probes Landlock support **without restricting the calling thread**.
    ///
    /// The ABI is only reported by the `landlock` crate through the
    /// [`RestrictionStatus`] of `restrict_self`. Landlock domains and
    /// `NO_NEW_PRIVS` are per-thread (the crate never requests
    /// `LANDLOCK_RESTRICT_SELF_TSYNC`), so the probe spawns a throwaway
    /// thread that builds a minimal ruleset (all ABI V1 filesystem rights
    /// handled, no rules), restricts *itself*, returns the status and
    /// exits; the caller joins it. The calling thread and every other
    /// thread of the process keep their unrestricted state; nothing the
    /// probe thread did survives it (it spawns no children).
    ///
    /// Never fails: an unreadable LSM list gives `lsm_active: None`, a
    /// failed probe thread gives `abi: None`.
    #[must_use]
    pub fn landlock_support() -> super::LandlockSupport {
        super::LandlockSupport {
            lsm_active: super::landlock_lsm_active(),
            abi: probe_abi(),
        }
    }

    fn probe_abi() -> Option<u8> {
        let probe = std::thread::Builder::new()
            .name("harw-landlock-probe".to_owned())
            .spawn(|| -> Option<u8> {
                let status = Ruleset::default()
                    .set_compatibility(CompatLevel::BestEffort)
                    .handle_access(AccessFs::from_all(ABI::V1))
                    .ok()?
                    .create()
                    .ok()?
                    .restrict_self()
                    .ok()?;
                Some(kernel_abi_of(&status))
            })
            .ok()?;
        probe.join().ok().flatten()
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
        use crate::sandbox::{BindMode, LANDLOCK_TARGET_ABI, PathMap};
        use crate::test_support::{TestResult, ctx};
        use harw_job_core::SandboxProfileName;

        #[test]
        fn test_target_abi_constant_matches_the_enforced_abi() {
            assert_eq!(abi_number(TARGET_ABI), LANDLOCK_TARGET_ABI);
        }

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
        fn test_build_ruleset_plans_the_mapped_view() -> TestResult {
            // The mapped policy is what a trampoline inside the namespace
            // enforces; on the host, the namespace-only destination does
            // not exist and is skipped (never an error).
            let workspace = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let destination = std::path::Path::new("/nonexistent/harw-job-linux/workspace");
            let map = PathMap::new()
                .try_with_bind(workspace.path(), destination, BindMode::ReadWrite)
                .map_err(ctx("bind"))?
                .try_with_bind("/usr", "/usr", BindMode::ReadOnly)
                .map_err(ctx("bind /usr"))?;
            let policy =
                SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, workspace.path())
                    .mapped(&map);
            let prepared = build_ruleset(&policy).map_err(ctx("build mapped ruleset"))?;
            assert!(
                prepared
                    .skipped_paths()
                    .iter()
                    .any(|path| path == destination)
            );
            // Paths the map does not cover got no rule at all.
            let etc = std::path::Path::new("/etc");
            assert!(!policy.filesystem.read_only.iter().any(|path| path == etc));
            Ok(())
        }

        /// A canonical scratch directory with `real/sub`, `link -> real`
        /// and `file.txt`.
        fn swap_scene() -> TestResult<(tempfile::TempDir, PathBuf)> {
            let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
            let root = dir.path().canonicalize().map_err(ctx("canonical"))?;
            std::fs::create_dir_all(root.join("real/sub")).map_err(ctx("real"))?;
            std::fs::write(root.join("file.txt"), b"f").map_err(ctx("file"))?;
            std::os::unix::fs::symlink(root.join("real"), root.join("link"))
                .map_err(ctx("symlink"))?;
            Ok((dir, root))
        }

        #[test]
        fn test_verify_opened_path_refuses_symlinks_on_the_way() -> TestResult {
            let (_dir, root) = swap_scene()?;
            for exact in [
                root.join("real"),
                root.join("real/sub"),
                root.join("file.txt"),
            ] {
                let fd = PathFd::new(&exact).map_err(ctx("open"))?;
                verify_opened_path(&fd, &exact).map_err(ctx("canonical path"))?;
            }
            // The final component or a directory on the way is a symlink:
            // the fd names `real`, not the planned path.
            for swapped in [root.join("link"), root.join("link/sub")] {
                let fd = PathFd::new(&swapped).map_err(ctx("open swapped"))?;
                assert!(
                    matches!(
                        verify_opened_path(&fd, &swapped),
                        Err(SandboxError::InvalidPath { ref path, .. }) if *path == swapped
                    ),
                    "{swapped:?}"
                );
            }
            let dir = PathFd::new(root.join("real")).map_err(ctx("dir"))?;
            assert!(opened_is_dir(&dir, &root.join("real")).map_err(ctx("fstat dir"))?);
            let file = PathFd::new(root.join("file.txt")).map_err(ctx("file"))?;
            assert!(!opened_is_dir(&file, &root.join("file.txt")).map_err(ctx("fstat file"))?);
            Ok(())
        }

        #[test]
        fn test_hard_mode_refuses_a_write_rule_swapped_for_a_symlink() -> TestResult {
            let (_dir, root) = swap_scene()?;
            let swapped = root.join("link");
            let best_effort =
                SandboxPolicy::from_profile(SandboxProfileName::ReadOnlyAnalysis, &root)
                    .with_read_write(&swapped);
            // Best effort keeps following the path (legacy behaviour).
            build_ruleset(&best_effort).map_err(ctx("best effort"))?;
            if landlock_support().usable_abi().unwrap_or(0) < LANDLOCK_TARGET_ABI {
                // Hard mode cannot even create the ruleset here.
                return Ok(());
            }
            let hard = best_effort.clone().hard_requirement();
            assert!(
                matches!(
                    build_ruleset(&hard),
                    Err(SandboxError::InvalidPath { ref path, .. }) if *path == swapped
                ),
                "a write rule through a symlink must be refused"
            );
            // Read-only rules through a symlink stay allowed; a canonical
            // write rule passes.
            let fine = SandboxPolicy::from_profile(SandboxProfileName::ReadOnlyAnalysis, &root)
                .with_read_only(&swapped)
                .with_read_write(root.join("real/sub"))
                .hard_requirement();
            build_ruleset(&fine).map_err(ctx("canonical write rule"))?;
            Ok(())
        }

        #[test]
        fn test_abi_number_is_monotonic() {
            assert_eq!(abi_number(ABI::Unsupported), 0);
            assert_eq!(abi_number(ABI::V1), 1);
            assert_eq!(abi_number(ABI::V4), 4);
            assert_eq!(abi_number(ABI::V5), 5);
        }

        #[test]
        fn test_landlock_support_leaves_calling_thread_unrestricted() -> TestResult {
            let nnp_before = rustix::thread::no_new_privs().map_err(ctx("nnp before"))?;
            let first = landlock_support();
            let second = landlock_support();
            assert_eq!(first, second, "probe is not idempotent");
            if first.lsm_active == Some(false) {
                // Not an active LSM: the kernel cannot report a usable ABI.
                assert_eq!(first.usable_abi(), None);
            }
            // The probe thread restricted itself to "no filesystem access";
            // this thread must still be able to create and write files.
            let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
            std::fs::write(temp.path().join("after-probe"), b"ok")
                .map_err(ctx("write after probe"))?;
            // NO_NEW_PRIVS of this thread is unchanged (the probe thread set
            // its own).
            let nnp_after = rustix::thread::no_new_privs().map_err(ctx("nnp after"))?;
            assert_eq!(nnp_before, nnp_after);
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
    use crate::test_support::{TestError, TestResult, ctx};

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
    fn test_lsm_list_parsing() {
        assert!(lsm_list_contains_landlock(
            "lockdown,capability,landlock,yama,apparmor\n"
        ));
        assert!(lsm_list_contains_landlock("landlock"));
        assert!(!lsm_list_contains_landlock("lockdown,capability,yama"));
        assert!(!lsm_list_contains_landlock("notlandlock,landlocked"));
        assert!(!lsm_list_contains_landlock(""));
    }

    #[test]
    fn test_userns_from_sysctls() {
        assert_eq!(userns_from_sysctls(None, None), None);
        assert_eq!(
            userns_from_sysctls(Some("0\n"), Some("63000\n")),
            Some(false)
        );
        assert_eq!(userns_from_sysctls(Some("1\n"), Some("0\n")), Some(false));
        assert_eq!(
            userns_from_sysctls(Some("1\n"), Some("63000\n")),
            Some(true)
        );
        assert_eq!(userns_from_sysctls(None, Some("63000")), Some(true));
        assert_eq!(userns_from_sysctls(Some("garbage"), None), None);
    }

    #[test]
    fn test_landlock_support_usable_abi() {
        let none = LandlockSupport::default();
        assert_eq!(none.usable_abi(), None);
        let zero = LandlockSupport {
            lsm_active: Some(false),
            abi: Some(0),
        };
        assert_eq!(zero.usable_abi(), None);
        let five = LandlockSupport {
            lsm_active: Some(true),
            abi: Some(5),
        };
        assert_eq!(five.usable_abi(), Some(5));
    }

    #[test]
    fn test_connect_ports_are_deduplicated() {
        let mut policy =
            SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, Path::new("/w"));
        policy.network = NetworkPolicy::ConnectTcpPorts(vec![443, 80, 443]);
        assert_eq!(plan_landlock(&policy).connect_ports, vec![80, 443]);
    }

    const READ: FsAccess = FsAccess {
        read: true,
        write: false,
        exec: false,
    };
    const READ_EXEC: FsAccess = FsAccess {
        read: true,
        write: false,
        exec: true,
    };
    const READ_WRITE_EXEC: FsAccess = FsAccess {
        read: true,
        write: true,
        exec: true,
    };

    const PROFILES: [SandboxProfileName; 4] = [
        SandboxProfileName::WorkspaceBuild,
        SandboxProfileName::ReadOnlyAnalysis,
        SandboxProfileName::NoNetwork,
        SandboxProfileName::NetworkRestricted,
    ];

    /// The view of a typical Bubblewrap plan: system trees read-only at
    /// their own path, the workspace at `/workspace`, the namespace's own
    /// `/proc`, `/dev` and `/tmp`, the attempt directory read-write. `/sys`
    /// and `/run` are withheld (not in the map).
    fn bwrap_like_map(workspace: BindMode) -> TestResult<PathMap> {
        PathMap::new()
            .try_with_bind("/usr", "/usr", BindMode::ReadOnly)
            .and_then(|map| map.try_with_bind("/bin", "/bin", BindMode::ReadOnly))
            .and_then(|map| map.try_with_bind("/lib", "/lib", BindMode::ReadOnly))
            .and_then(|map| map.try_with_bind("/lib64", "/lib64", BindMode::ReadOnly))
            .and_then(|map| map.try_with_bind("/etc", "/etc", BindMode::ReadOnly))
            .and_then(|map| map.try_with_provided("/proc", BindMode::ReadOnly))
            .and_then(|map| map.try_with_provided("/dev", BindMode::ReadWrite))
            .and_then(|map| map.try_with_provided("/tmp", BindMode::ReadWrite))
            .and_then(|map| map.try_with_bind("/home/u/repo", "/workspace", workspace))
            .and_then(|map| {
                map.try_with_bind(
                    "/var/lib/harw/attempt-1",
                    "/run/harw-job/attempt",
                    BindMode::ReadWrite,
                )
            })
            .map_err(ctx("bwrap-like map"))
    }

    fn workspace_build() -> SandboxPolicy {
        SandboxPolicy::from_profile(
            SandboxProfileName::WorkspaceBuild,
            Path::new("/home/u/repo"),
        )
    }

    #[test]
    fn test_identity_map_keeps_plan_landlock_identical() -> TestResult {
        // `/` bound onto itself read-write is the host view: same policy,
        // same plan, for every profile.
        let identity = PathMap::new()
            .try_with_bind("/", "/", BindMode::ReadWrite)
            .map_err(ctx("identity map"))?;
        for profile in PROFILES {
            let policy = SandboxPolicy::from_profile(profile, Path::new("/work/repo"))
                .with_read_only("/opt/data")
                .with_read_only("/work/repo")
                .with_read_write("/var/cache/harw");
            assert_eq!(policy.mapped(&identity), policy, "{profile:?}");
            assert_eq!(
                plan_landlock_mapped(&policy, &identity),
                plan_landlock(&policy),
                "{profile:?}"
            );
            assert!(identity.unmapped_paths(&policy.filesystem).is_empty());
            let hard = policy.hard_requirement();
            assert_eq!(plan_landlock_mapped(&hard, &identity), plan_landlock(&hard));
        }
        Ok(())
    }

    #[test]
    fn test_empty_path_map_is_an_empty_view() {
        let empty = PathMap::new();
        assert!(empty.is_empty());
        assert_eq!(empty.resolve(Path::new("/usr")), None);
        let policy = workspace_build();
        let mapped = policy.mapped(&empty);
        assert_eq!(mapped.filesystem, FilesystemPolicy::default());
        assert_eq!(mapped.network, policy.network);
        let plan = plan_landlock_mapped(&policy, &empty);
        assert!(plan.rules.is_empty(), "{:?}", plan.rules);
        // Everything the policy names is reported as dropped.
        let unmapped = empty.unmapped_paths(&policy.filesystem);
        assert!(unmapped.contains(&PathBuf::from("/home/u/repo")));
        assert!(unmapped.contains(&PathBuf::from("/usr")));
        assert!(
            unmapped
                .windows(2)
                .all(|pair| matches!(pair, [left, right] if left < right))
        );
    }

    #[test]
    fn test_mapped_plan_uses_the_namespace_view() -> TestResult {
        let policy = workspace_build().with_read_write("/home/u/repo/target");
        let map = bwrap_like_map(BindMode::ReadWrite)?;
        let plan = plan_landlock_mapped(&policy, &map);

        assert_eq!(
            access_of(&plan, Path::new("/workspace")),
            Some(READ_WRITE_EXEC)
        );
        assert_eq!(
            access_of(&plan, Path::new("/workspace/target")).map(|access| access.write),
            Some(true)
        );
        assert_eq!(
            access_of(&plan, Path::new("/home/u/repo")),
            None,
            "the host spelling of a bound path is gone"
        );
        // System exec trees keep read + execute through their read-only
        // binds; other read-only binds stay read-only.
        assert_eq!(access_of(&plan, Path::new("/usr")), Some(READ_EXEC));
        assert_eq!(access_of(&plan, Path::new("/lib64")), Some(READ_EXEC));
        assert_eq!(access_of(&plan, Path::new("/etc")), Some(READ));
        // The namespace's own mounts keep the policy spelling.
        assert_eq!(access_of(&plan, Path::new("/proc")), Some(READ));
        assert_eq!(
            access_of(&plan, Path::new("/tmp")).map(|access| (access.write, access.exec)),
            Some((true, false))
        );
        assert_eq!(
            access_of(&plan, Path::new("/dev/null")).map(|access| access.write),
            Some(true)
        );
        // Withheld trees are not in the view: their rules are dropped and
        // cannot cover what the namespace mounts there (the attempt dir).
        assert_eq!(access_of(&plan, Path::new("/run")), None);
        assert_eq!(access_of(&plan, Path::new("/sys")), None);
        assert_eq!(access_of(&plan, Path::new("/sbin")), None);
        assert_eq!(
            map.unmapped_paths(&policy.filesystem),
            vec![
                PathBuf::from("/lib32"),
                PathBuf::from("/run"),
                PathBuf::from("/sbin"),
                PathBuf::from("/sys"),
            ]
        );
        assert!(map.shadowed_read_only_binds(&policy.filesystem).is_empty());

        // Everything but the filesystem lists is unchanged.
        let mapped = policy.mapped(&map);
        assert_eq!(mapped.network, policy.network);
        assert_eq!(mapped.capabilities, policy.capabilities);
        assert_eq!(mapped.no_new_privs, policy.no_new_privs);
        assert_eq!(mapped.landlock, policy.landlock);
        let unmapped = plan_landlock(&policy);
        assert_eq!(plan.restrict_network, unmapped.restrict_network);
        assert_eq!(plan.connect_ports, unmapped.connect_ports);
        Ok(())
    }

    #[test]
    fn test_read_only_binds_drop_write_and_execute_outside_system_trees() -> TestResult {
        // WorkspaceBuild asks for read/write/execute on the workspace.
        let policy = workspace_build();
        let read_only = bwrap_like_map(BindMode::ReadOnly)?;
        assert_eq!(
            access_of(
                &plan_landlock_mapped(&policy, &read_only),
                Path::new("/workspace")
            ),
            Some(READ)
        );
        let mapped = policy.mapped(&read_only).filesystem;
        let workspace = PathBuf::from("/workspace");
        assert!(!mapped.read_write.contains(&workspace));
        assert!(!mapped.exec.contains(&workspace));
        assert!(mapped.read_only.contains(&workspace));

        // The system exec trees are judged by the in-sandbox destination.
        let mut tools =
            SandboxPolicy::from_profile(SandboxProfileName::ReadOnlyAnalysis, Path::new("/w"));
        tools.filesystem.exec.push(PathBuf::from("/opt/tools"));
        tools.filesystem.exec.push(PathBuf::from("/opt/scripts"));
        let map = PathMap::new()
            .try_with_bind("/opt/tools", "/usr/local/tools", BindMode::ReadOnly)
            .and_then(|map| map.try_with_bind("/opt/scripts", "/scripts", BindMode::ReadOnly))
            .map_err(ctx("tool map"))?;
        let plan = plan_landlock_mapped(&tools, &map);
        assert_eq!(
            access_of(&plan, Path::new("/usr/local/tools")),
            Some(READ_EXEC)
        );
        assert_eq!(access_of(&plan, Path::new("/scripts")), Some(READ));
        Ok(())
    }

    #[test]
    fn test_read_only_bind_nested_in_writable_workspace_is_reported() -> TestResult {
        // A read-only `.git` inside the read-write workspace: the policy
        // entry beneath the bind is capped, but Landlock still lets the
        // workspace rule (write + execute) reach into the bind.
        let map = bwrap_like_map(BindMode::ReadWrite)?
            .try_with_bind("/home/u/repo/.git", "/workspace/.git", BindMode::ReadOnly)
            .map_err(ctx("git bind"))?;
        let policy = workspace_build().with_read_write("/home/u/repo/.git");
        let mapped = policy.mapped(&map).filesystem;
        let git = PathBuf::from("/workspace/.git");
        assert!(mapped.read_only.contains(&git));
        assert!(!mapped.read_write.contains(&git));
        assert!(!mapped.exec.contains(&git));
        let shadowed = map.shadowed_read_only_binds(&policy.filesystem);
        assert_eq!(
            shadowed
                .iter()
                .map(|bind| &bind.sandbox)
                .collect::<Vec<_>>(),
            vec![&git]
        );

        // A read-only workspace has no such ancestor; read-only binds
        // beneath a system exec tree inherit only the sanctioned execute.
        let analysis = SandboxPolicy::from_profile(
            SandboxProfileName::ReadOnlyAnalysis,
            Path::new("/home/u/repo"),
        );
        let read_only = bwrap_like_map(BindMode::ReadOnly)?
            .try_with_bind("/home/u/repo/.git", "/workspace/.git", BindMode::ReadOnly)
            .and_then(|map| map.try_with_bind("/opt/tools", "/usr/local/tools", BindMode::ReadOnly))
            .map_err(ctx("read-only map"))?;
        assert!(
            read_only
                .shadowed_read_only_binds(&analysis.filesystem)
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn test_unmappable_paths_and_invalid_binds_are_refused() -> TestResult {
        let map = bwrap_like_map(BindMode::ReadWrite)?;
        // `..` escapes the bind on disk although it is "beneath" it
        // textually: never rebased, never kept with the bind's rights.
        let escape = PathBuf::from("/home/u/repo/../../etc");
        assert_eq!(map.resolve(&escape), None);
        let mut policy = workspace_build();
        policy.filesystem.exec.push(escape.clone());
        policy.filesystem.read_write.push(PathBuf::from("repo/src"));
        let mapped = policy.mapped(&map).filesystem;
        let all: Vec<&PathBuf> = mapped
            .read_only
            .iter()
            .chain(&mapped.read_write)
            .chain(&mapped.exec)
            .collect();
        assert!(
            all.iter().all(|path| is_mappable(path)),
            "{all:?} contains an unmappable path"
        );
        assert!(!mapped.exec.contains(&PathBuf::from("/etc")));
        let unmapped = map.unmapped_paths(&policy.filesystem);
        assert!(unmapped.contains(&escape));
        assert!(unmapped.contains(&PathBuf::from("repo/src")));
        assert_eq!(map.resolve(Path::new("repo/src")), None);

        // Binds: an empty host, a relative destination and `..` anywhere
        // are refused, by the builder and by deserialization.
        for (host, sandbox) in [
            ("", "/x"),
            ("/a", ""),
            ("/a", "x"),
            ("a", "/x"),
            ("/a/../b", "/x"),
            ("/a", "/x/.."),
        ] {
            let built = PathMap::new().try_with_bind(host, sandbox, BindMode::ReadOnly);
            assert!(built.is_err(), "{host:?} -> {sandbox:?}: {built:?}");
            let json = format!(
                r#"{{"binds":[{{"host":"{host}","sandbox":"{sandbox}","mode":"read_only"}}]}}"#
            );
            assert!(
                serde_json::from_str::<PathMap>(&json).is_err(),
                "{json} was accepted"
            );
        }
        assert!(
            PathMap::new()
                .try_with_provided("tmp", BindMode::ReadWrite)
                .is_err()
        );
        let error = PathMap::new()
            .try_with_bind("/a", "rel", BindMode::ReadWrite)
            .err()
            .ok_or(TestError::Missing("relative destination refused"))?;
        assert_eq!(error.path, PathBuf::from("rel"));
        assert!(error.to_string().contains("absolute"), "{error}");
        Ok(())
    }

    #[test]
    fn test_path_map_resolution_is_component_wise_and_longest_prefix() -> TestResult {
        let map = PathMap::new()
            .try_with_bind("/home/u", "/home/u", BindMode::ReadOnly)
            .and_then(|map| map.try_with_bind("/home/u/repo", "/workspace", BindMode::ReadWrite))
            .map_err(ctx("map"))?;
        assert_eq!(
            map.resolve(Path::new("/home/u/repo")),
            Some((PathBuf::from("/workspace"), BindMode::ReadWrite))
        );
        assert_eq!(
            map.resolve(Path::new("/home/u/repo/src/main.rs")),
            Some((PathBuf::from("/workspace/src/main.rs"), BindMode::ReadWrite))
        );
        // `/home/u/repo2` is a sibling of the workspace, not inside it.
        assert_eq!(
            map.resolve(Path::new("/home/u/repo2")),
            Some((PathBuf::from("/home/u/repo2"), BindMode::ReadOnly))
        );
        assert_eq!(map.resolve(Path::new("/home/other")), None);
        assert_eq!(map.resolve(Path::new("/home")), None);

        // An exact match carries no trailing separator (files stay files).
        let file = PathMap::new()
            .try_with_bind("/dev/null", "/dev/null", BindMode::ReadWrite)
            .map_err(ctx("file map"))?;
        assert_eq!(
            file.resolve(Path::new("/dev/null"))
                .map(|(path, _)| path.into_os_string()),
            Some(std::ffi::OsString::from("/dev/null"))
        );

        // Same host path twice: the later bind wins.
        let twice = PathMap::new()
            .try_with_bind("/data", "/a", BindMode::ReadOnly)
            .and_then(|map| map.try_with_bind("/data", "/b", BindMode::ReadWrite))
            .map_err(ctx("twice"))?;
        assert_eq!(twice.binds().len(), 2);
        assert_eq!(
            twice.resolve(Path::new("/data/x")),
            Some((PathBuf::from("/b/x"), BindMode::ReadWrite))
        );
        Ok(())
    }

    #[test]
    fn test_path_map_serde_round_trip() -> TestResult {
        let map = bwrap_like_map(BindMode::ReadOnly)?;
        let json = serde_json::to_string(&map).map_err(ctx("serialize"))?;
        assert!(json.contains(r#""mode":"read_only""#), "{json}");
        let back: PathMap = serde_json::from_str(&json).map_err(ctx("parse"))?;
        assert_eq!(back, map);
        assert!(serde_json::from_str::<PathMap>(r#"{"binds":[],"extra":1}"#).is_err());
        assert!(
            serde_json::from_str::<PathMap>(
                r#"{"binds":[{"host":"/a","sandbox":"/b","mode":"read_only","x":1}]}"#
            )
            .is_err()
        );
        // The removed read-only-exec mode is not accepted.
        assert!(
            serde_json::from_str::<PathMap>(
                r#"{"binds":[{"host":"/a","sandbox":"/b","mode":"read_only_exec"}]}"#
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn test_write_scope_replaces_the_workspace_write_grant() -> TestResult {
        let root = Path::new("/work/repo");
        let scope = [
            PathBuf::from("/work/repo/src"),
            PathBuf::from("/work/repo/out/a.txt"),
        ];
        for profile in [
            SandboxProfileName::WorkspaceBuild,
            SandboxProfileName::NoNetwork,
            SandboxProfileName::NetworkRestricted,
        ] {
            let base = SandboxPolicy::from_profile(profile, root);
            let scoped = base
                .with_workspace_write_scope(root, &scope)
                .map_err(ctx("scope"))?;
            // Only the scope is writable inside the workspace; the rest of
            // the policy is untouched.
            assert!(!scoped.filesystem.read_write.iter().any(|path| path == root));
            assert!(scoped.filesystem.read_only.iter().any(|path| path == root));
            assert!(scoped.filesystem.exec.iter().any(|path| path == root));
            for path in &scope {
                assert!(scoped.filesystem.read_write.contains(path), "{path:?}");
            }
            assert_eq!(scoped.network, base.network);
            // Hard mode: the ruleset builder re-checks every write rule.
            assert_eq!(base.landlock, LandlockMode::BestEffort);
            assert_eq!(scoped.landlock, LandlockMode::HardRequirement);
            assert!(
                scoped
                    .filesystem
                    .read_write
                    .contains(&PathBuf::from("/tmp"))
            );
            let plan = plan_landlock(&scoped);
            let workspace = access_of(&plan, root).ok_or(TestError::Missing("workspace rule"))?;
            assert!(workspace.read && workspace.exec && !workspace.write);
            let src = access_of(&plan, Path::new("/work/repo/src"))
                .ok_or(TestError::Missing("scope rule"))?;
            assert!(src.read && src.write);
        }
        // An empty scope leaves the whole workspace read-only.
        let none = SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, root)
            .with_workspace_write_scope(root, &[])
            .map_err(ctx("empty scope"))?;
        assert!(
            !none
                .filesystem
                .read_write
                .iter()
                .any(|path| path.starts_with(root))
        );
        Ok(())
    }

    #[test]
    fn test_write_scope_never_widens_and_refuses_writable_ancestors() {
        let root = Path::new("/work/repo");
        let read_only = SandboxPolicy::from_profile(SandboxProfileName::ReadOnlyAnalysis, root);
        assert_eq!(
            read_only.with_workspace_write_scope(root, &[PathBuf::from("/work/repo/src")]),
            Err(WriteScopeConflict::NoWorkspaceWrite)
        );
        let build = SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, root);
        for outside in ["/work/repo2/src", "/work", "/etc/passwd"] {
            assert_eq!(
                build.with_workspace_write_scope(root, &[PathBuf::from(outside)]),
                Err(WriteScopeConflict::OutsideWorkspace(PathBuf::from(outside)))
            );
        }
        // `/tmp` stays read-write in every profile: a workspace below it
        // cannot be narrowed.
        let under_tmp = Path::new("/tmp/job/repo");
        let tmp = SandboxPolicy::from_profile(SandboxProfileName::NoNetwork, under_tmp);
        assert_eq!(
            tmp.with_workspace_write_scope(under_tmp, &[under_tmp.join("src")]),
            Err(WriteScopeConflict::WritableAncestor(PathBuf::from("/tmp")))
        );
        assert!(
            WriteScopeConflict::WritableAncestor(PathBuf::from("/tmp"))
                .to_string()
                .contains("inherited")
        );
    }

    #[test]
    fn test_landlock_filesystem_state_by_abi() {
        use harw_job_core::EnforcementState as S;
        assert_eq!(landlock_filesystem_state(None), S::Unsupported);
        assert_eq!(landlock_filesystem_state(Some(0)), S::Unsupported);
        for abi in 1..LANDLOCK_TARGET_ABI {
            assert_eq!(landlock_filesystem_state(Some(abi)), S::Partial, "{abi}");
        }
        assert_eq!(
            landlock_filesystem_state(Some(LANDLOCK_TARGET_ABI)),
            S::Enforced
        );
        assert_eq!(landlock_filesystem_state(Some(9)), S::Enforced);
    }

    #[test]
    fn test_landlock_network_state_by_policy_and_abi() {
        use harw_job_core::EnforcementState as S;
        for abi in [None, Some(0), Some(1), Some(4), Some(LANDLOCK_TARGET_ABI)] {
            assert_eq!(
                landlock_network_state(&NetworkPolicy::Allow, abi),
                S::Enforced,
                "{abi:?}"
            );
        }
        for network in [
            NetworkPolicy::Deny,
            NetworkPolicy::ConnectTcpPorts(RESTRICTED_CONNECT_PORTS.to_vec()),
        ] {
            assert_eq!(landlock_network_state(&network, None), S::Unsupported);
            assert_eq!(landlock_network_state(&network, Some(0)), S::Unsupported);
            for abi in 1..LANDLOCK_NETWORK_ABI {
                assert_eq!(
                    landlock_network_state(&network, Some(abi)),
                    S::NotEnforced,
                    "{abi}"
                );
            }
            // TCP only, whatever the ABI: never `enforced` without a
            // network namespace.
            for abi in [LANDLOCK_NETWORK_ABI, LANDLOCK_TARGET_ABI, 9] {
                assert_eq!(
                    landlock_network_state(&network, Some(abi)),
                    S::Partial,
                    "{abi}"
                );
            }
        }
    }
}
