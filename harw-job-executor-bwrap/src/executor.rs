//! Translation of a generic job request into a Bubblewrap launch.
//!
//! # Filesystem mapping
//! Every path of the [`FilesystemPolicy`] falls into exactly one class
//! (checked in this order):
//!
//! | Class | Paths | Effect in the `bwrap` plan |
//! |---|---|---|
//! | workspace | the workspace root or below | workspace bind, `--bind` if the root itself is read-write, else `--ro-bind` |
//! | sandbox-provided | `/proc…`, `/dev…`, exactly `/tmp` | nothing extra: `bwrap` mounts a fresh procfs, a minimal `/dev` and an empty `/tmp` tmpfs; host content is **not** visible |
//! | withheld | `/sys…`, `/run…`, ancestors of the workspace | **not bound** (narrower than requested); listed in [`BwrapJobPlan::withheld_paths`] |
//! | always bound | `/usr`, `/bin`, `/lib`, `/lib64` and below | nothing extra: `harw-sandbox` binds them read-only in every plan |
//! | other, read/exec | e.g. `/etc`, `/sbin`, `/lib32` | `--ro-bind-try` via [`BwrapLauncher::with_read_only_paths`] |
//! | other, read-write | anything else | refused: [`BwrapExecutorError::Unsupported`] |
//!
//! `/sys` and `/run` are withheld on purpose: a read-only bind of `/run`
//! would still expose host Unix sockets (e.g. a container daemon socket),
//! because `connect(2)` on a socket inode ignores read-only mounts.
//!
//! Mount namespaces distinguish *invisible*, *read-only* and *read-write*;
//! they do not distinguish *read* from *execute*. Read-only paths are
//! therefore executable inside the sandbox, whereas Landlock would forbid
//! it. Composing with the `harw-job-exec` trampoline (Landlock inside the
//! namespace, [`BwrapExecutor::plan_with_trampoline`]) restores that
//! distinction; see the crate docs.
//!
//! # Namespace view of the trampoline composition
//! [`BwrapExecutor::plan_with_trampoline`] describes the mounts of its plan
//! as a [`PathMap`], built from the same classification as the argv:
//!
//! | Entry | Host path | In the sandbox | Mode |
//! |---|---|---|---|
//! | workspace (canonical and as requested) | `<ws>` | `<ws>` | read-write if the workspace is writable, else read-only |
//! | always bound (if present) | `/usr`, `/bin`, `/lib`, `/lib64` | same | read-only |
//! | NSS files | `/etc/passwd`, `/etc/group`, `/etc/nsswitch.conf` | same | read-only |
//! | other read/exec policy paths | e.g. `/etc`, `/sbin` | same | read-only |
//! | provided by bwrap | `/proc` / `/dev`, `/tmp` | same | read-only / read-write |
//! | relay (proxy-only egress) | relay binary / proxy socket | `/run/harw/netns-relay` / `/run/harw/egress.sock` | read-only / read-write |
//! | trampoline | `harw-job-exec` binary | [`SANDBOX_TRAMPOLINE_PATH`] | read-only |
//! | attempt directory | per-attempt directory | [`SANDBOX_ATTEMPT_DIR`] | read-write mount (for the trampoline's report), no Landlock rule |
//! | profile modules, host `PATH` | Cargo toolchain and cache, tmux socket, `PATH` directories | as `harw-sandbox` binds them | as bound; see "Profile modules" |
//!
//! Withheld policy paths (`/sys`, `/run`, workspace ancestors) have no
//! entry, so the in-namespace Landlock policy drops them as well
//! ([`BwrapTrampolinePlan::unmapped_paths`]). No inner policy path keeps
//! write access to [`SANDBOX_TRAMPOLINE_DIR`]: the job cannot rewrite the
//! report once the inner trampoline has restricted itself.
//!
//! # Profile modules (PL-97 E2b)
//! [`BwrapExecutor::with_profile_modules`], [`BwrapExecutor::with_host_path`]
//! and [`BwrapExecutor::with_tmpfs_size`] carry what `shell.exec` and
//! `job.start` configure on their own launcher today: the Cargo toolchain
//! or tmux socket of the caller's [`SandboxProfile`], the host `PATH`
//! binding and the size of `/tmp`. `harw-sandbox` plans the binds exactly
//! as for those tools. The trampoline composition reads them back from the
//! launcher's argv ([`BwrapTrampolinePlan::module_binds`]), adds them to
//! the namespace view and gives each one Landlock rule inside: read and
//! execute for a read-only bind, read, write and execute for a read-write
//! one. That is what the mount grants without Landlock, so nothing the
//! modules allow today is lost. The policy paths stay capped as before.

use std::ffi::OsString;
use std::num::NonZeroU64;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use harw_authority::{
    Permission, PermissionSet, SandboxSpec, WorkspaceBinding, WorkspaceRegistration,
    WorkspaceRegistry,
};
use harw_job_core::{EnforcementState, JobSpec, SandboxReport};
use harw_job_linux::{
    BindMode, CapabilityPolicy, FilesystemPolicy, NetworkPolicy, PathBind, PathMap, PathMapError,
    SandboxPolicy,
};
use harw_sandbox::{
    BwrapLauncher, HostPathBinding, NetworkMode, RelaySpec, SANDBOX_PROXY_SOCKET_PATH,
    SANDBOX_RELAY_PATH, SandboxProfile,
};
use harw_types::{TenantId, WorkspaceId};

use crate::error::{BwrapExecutorError, Dimension};

/// Directory inside the sandbox that holds the trampoline composition.
pub const SANDBOX_TRAMPOLINE_DIR: &str = "/run/harw-job";
/// Where [`BwrapExecutor::plan_with_trampoline`] binds the `harw-job-exec`
/// binary (read-only) inside the sandbox.
pub const SANDBOX_TRAMPOLINE_PATH: &str = "/run/harw-job/exec";
/// Where [`BwrapExecutor::plan_with_trampoline`] binds the attempt
/// directory (read-write) inside the sandbox.
pub const SANDBOX_ATTEMPT_DIR: &str = "/run/harw-job/attempt";
/// File name of the inner trampoline plan in the attempt directory.
pub const INNER_PLAN_FILE: &str = "inner.json";
/// File name of the inner trampoline's report in the attempt directory.
pub const INNER_REPORT_FILE: &str = "report.json";

/// Trees `harw-sandbox` binds read-only (and executable) in every plan.
const ALWAYS_BOUND: &[&str] = &["/usr", "/bin", "/lib", "/lib64"];
/// NSS files `harw-sandbox` binds read-only (`--ro-bind-try`) in every plan.
const NSS_FILES: &[&str] = &["/etc/passwd", "/etc/group", "/etc/nsswitch.conf"];
/// Trees `bwrap` provides itself (fresh procfs / minimal devtmpfs).
const SANDBOX_PROVIDED_TREES: &[&str] = &["/proc", "/dev"];
/// Exact paths `bwrap` provides itself (empty tmpfs).
const SANDBOX_PROVIDED_EXACT: &[&str] = &["/tmp"];
/// Host trees that are never bound, even when the policy lists them.
const WITHHELD_TREES: &[&str] = &["/sys", "/run"];
/// Tenant/workspace names of the single-workspace registry behind the
/// [`SandboxSpec`]; they never leave this crate.
const REGISTRY_TENANT: &str = "job";
const REGISTRY_WORKSPACE: &str = "workspace";
/// Length of the relay prefix `harw-sandbox` puts before the command in
/// proxy-only mode: `<relay> <port> <socket> --`.
const RELAY_PREFIX_LEN: usize = 4;

/// Bubblewrap job executor: plans a [`JobSpec`] under a [`SandboxPolicy`] as
/// a `bwrap` launch through [`harw_sandbox::BwrapLauncher`].
///
/// # Description
/// Holds only the trusted `bwrap` path and optional launch settings; it is a
/// pure value. Every [`plan`](Self::plan) starts from a fresh launcher, so no
/// Cargo/tmux/host-`PATH` module of `harw-sandbox` can leak into a job: the
/// policy is the only source of binds, plus the modules configured
/// explicitly on this value ([`with_profile_modules`](Self::with_profile_modules),
/// [`with_host_path`](Self::with_host_path), module docs).
///
/// # Concurrency
/// `Send + Sync`; planning has no side effects besides canonicalizing the
/// workspace root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BwrapExecutor {
    executable: PathBuf,
    relay: Option<RelaySpec>,
    identity: Option<(u32, u32)>,
    /// Cargo/tmux module of the caller's sandbox profile; never `Host`.
    modules: SandboxProfile,
    /// Host `PATH` binding of the caller.
    host_path: Option<HostPathBinding>,
    /// Size of the sandbox's `/tmp` tmpfs (`--size`).
    tmpfs_size: Option<NonZeroU64>,
}

impl BwrapExecutor {
    /// Finds a trusted `bwrap` at the fixed root-owned paths of
    /// `harw-sandbox` (never via `PATH`).
    ///
    /// # Errors
    /// [`BwrapExecutorError::Unsupported`] with [`Dimension::Backend`] when
    /// no trusted `bwrap` is installed.
    pub fn discover() -> Result<Self, BwrapExecutorError> {
        BwrapLauncher::discover()
            .map(|launcher| Self::from_executable(launcher.executable().to_path_buf()))
            .map_err(|error| BwrapExecutorError::Unsupported {
                dimension: Dimension::Backend,
                reason: error.to_string(),
            })
    }

    /// Uses exactly `executable` as `bwrap`. The caller vouches for the
    /// path; [`plan`](Self::plan) rejects a relative one.
    #[must_use]
    pub fn from_executable(executable: PathBuf) -> Self {
        Self {
            executable,
            relay: None,
            identity: None,
            modules: SandboxProfile::Strict,
            host_path: None,
            tmpfs_size: None,
        }
    }

    /// Enables proxy-only egress through the `harw-netns-relay` exec mode.
    ///
    /// # Description
    /// Without a relay the backend only supports
    /// [`NetworkPolicy::Deny`]. With it, [`NetworkPolicy::Allow`] and
    /// [`NetworkPolicy::ConnectTcpPorts`] run with
    /// [`NetworkMode::ProxyOnly`]: still an own network namespace, egress
    /// only through the host egress proxy socket.
    #[must_use]
    pub fn with_proxy_relay(mut self, relay: RelaySpec) -> Self {
        self.relay = Some(relay);
        self
    }

    /// Fixes the uid/gid inside the sandbox (default: the real identity of
    /// the calling process). Mainly for deterministic plans in tests.
    #[must_use]
    pub fn with_identity(mut self, uid: u32, gid: u32) -> Self {
        self.identity = Some((uid, gid));
        self
    }

    /// The `bwrap` executable this executor launches.
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// The configured proxy relay, if any.
    #[must_use]
    pub fn proxy_relay(&self) -> Option<&RelaySpec> {
        self.relay.as_ref()
    }

    /// Binds the modules of the caller's trusted sandbox profile into every
    /// plan, as `harw_sandbox::BwrapLauncher::with_profile` does for
    /// `shell.exec` (PL-97 E2b): the Cargo toolchain and cache of
    /// [`SandboxProfile::Cargo`], the socket of [`SandboxProfile::Tmux`].
    /// [`SandboxProfile::Strict`] and [`SandboxProfile::Host`] add nothing
    /// (`Host` is a marker for unsandboxed execution, which this backend
    /// never does).
    ///
    /// The profile is trusted runtime input, never a tool argument. A Cargo
    /// profile in `Fetch` mode needs network, which this backend's plans do
    /// not grant; planning fails then, as it does for `shell.exec`.
    #[must_use]
    pub fn with_profile_modules(mut self, profile: &SandboxProfile) -> Self {
        self.modules = match profile {
            SandboxProfile::Host => SandboxProfile::Strict,
            other => other.clone(),
        };
        self
    }

    /// Binds the host `PATH` directories (and the Cargo/rustup homes they
    /// need) read-only into every plan and sets the sandbox's `PATH` to
    /// the host's, as `harw_sandbox::BwrapLauncher::with_host_path` does
    /// for `shell.exec` (PL-97 E2b).
    #[must_use]
    pub fn with_host_path(mut self, binding: HostPathBinding) -> Self {
        self.host_path = Some(binding);
        self
    }

    /// Limits the sandbox's `/tmp` tmpfs to `bytes` (`bwrap --size`, PL-97
    /// E2b; `shell.exec` uses 256 MiB). Without it the kernel default
    /// applies.
    #[must_use]
    pub fn with_tmpfs_size(mut self, bytes: NonZeroU64) -> Self {
        self.tmpfs_size = Some(bytes);
        self
    }

    /// The profile modules bound into every plan
    /// ([`with_profile_modules`](Self::with_profile_modules)).
    #[must_use]
    pub fn profile_modules(&self) -> &SandboxProfile {
        &self.modules
    }

    /// The host `PATH` binding, if any ([`with_host_path`](Self::with_host_path)).
    #[must_use]
    pub fn host_path(&self) -> Option<&HostPathBinding> {
        self.host_path.as_ref()
    }

    /// The `/tmp` size limit, if any ([`with_tmpfs_size`](Self::with_tmpfs_size)).
    #[must_use]
    pub fn tmpfs_size(&self) -> Option<NonZeroU64> {
        self.tmpfs_size
    }

    /// Plans `spec` under `policy` with `workspace_root` as the job's
    /// workspace.
    ///
    /// # Description
    /// - Filesystem: see the module docs for the path classes. The
    ///   workspace must appear in at least one list; it is writable only if
    ///   the workspace root itself is in `read_write`.
    /// - Network: `Deny` → [`NetworkMode::None`]; `Allow` /
    ///   `ConnectTcpPorts` → [`NetworkMode::ProxyOnly`] if a relay is
    ///   configured, else refused.
    /// - Capabilities: `DropAll` (or an empty keep-list) → enforced, since
    ///   `bwrap` drops every capability before `exec`; a non-empty keep-list
    ///   is refused.
    /// - `no_new_privs`: `bwrap` always sets `PR_SET_NO_NEW_PRIVS`.
    /// - Environment: `--clearenv`, then the `harw-sandbox` baseline
    ///   (`HOME`, `PATH`, `USER`/`LOGNAME`, and what the profile modules
    ///   set), then `spec.env` (which may override the baseline).
    /// - Profile modules, host `PATH` and `/tmp` size: as configured on
    ///   this executor (module docs).
    /// - Working directory: `<workspace>/<spec.working_dir>`.
    ///
    /// The returned report is a **prediction** for the `bwrap` layer only;
    /// `resource_limits` is always [`EnforcementState::NotEnforced`] because
    /// limits come from the cgroup/trampoline layer, whose result the caller
    /// merges in. The caller also decides whether the merged report
    /// satisfies `spec.sandbox`.
    ///
    /// # Errors
    /// - [`BwrapExecutorError::RelativeExecutable`] for a relative `bwrap`.
    /// - [`BwrapExecutorError::InvalidSpec`] if `spec` fails validation.
    /// - [`BwrapExecutorError::Workspace`] if `workspace_root` is not an
    ///   existing directory.
    /// - [`BwrapExecutorError::InvalidPath`] for a relative or non-normal
    ///   policy path.
    /// - [`BwrapExecutorError::Unsupported`] for requests the backend cannot
    ///   express (see above), and with [`Dimension::Filesystem`] for a
    ///   workspace that is (or resolves to) the host root `/`: every policy
    ///   path would be a workspace subpath and the whole host would be
    ///   bound read-write.
    /// - [`BwrapExecutorError::Sandbox`] if `harw-sandbox` rejects the plan.
    pub fn plan(
        &self,
        spec: &JobSpec,
        policy: &SandboxPolicy,
        workspace_root: &Path,
    ) -> Result<BwrapJobPlan, BwrapExecutorError> {
        self.plan_launch(spec, policy, workspace_root, None)
            .map(|(plan, _)| plan)
    }

    /// Plans `spec` under `policy` with the `harw-job-exec` trampoline as
    /// the sandboxed command (PL-97 E1, bwrap ∘ trampoline).
    ///
    /// # Description
    /// The `bwrap` plan is the one of [`plan`](Self::plan), with these
    /// differences:
    /// - Namespaces: `--unshare-user --unshare-ipc --unshare-pid
    ///   --unshare-net --unshare-uts` instead of `--unshare-all`
    ///   ([`BwrapLauncher::with_shared_cgroup_namespace`]): the sandbox
    ///   keeps the caller's cgroup namespace, so the trampoline's
    ///   `/proc/self/cgroup` check sees the job cgroup's real path.
    /// - Binds: `mount.exec_path` read-only at [`SANDBOX_TRAMPOLINE_PATH`],
    ///   `mount.attempt_dir` read-write at [`SANDBOX_ATTEMPT_DIR`] (after
    ///   the workspace and relay binds, so neither can overlay them).
    /// - Command: `/run/harw-job/exec --plan /run/harw-job/attempt/inner.json
    ///   --report /run/harw-job/attempt/report.json`, behind the relay
    ///   prefix in proxy-only mode. The job itself is the program of the
    ///   inner plan the caller writes to
    ///   [`TrampolineMount::inner_plan_path`].
    ///
    /// The returned [`BwrapTrampolinePlan`] also carries the namespace view
    /// ([`PathMap`], module docs) and the policy for the inner trampoline:
    /// `policy.mapped(&path_map)` with the network rewritten to what the
    /// namespace leaves to Landlock (own empty netns or proxy-only egress
    /// → `Allow`, a port allowlist behind the relay → TCP connect to the
    /// relay port only), plus one rule per profile-module bind
    /// ([`BwrapTrampolinePlan::module_binds`]). The namespace enforces the
    /// network dimension; the inner layer must not report a stronger
    /// network state than the namespace provides, and must not report a
    /// weaker one that would make its own requirement check refuse an
    /// enforced job.
    ///
    /// The attempt directory is bound read-write only so the inner
    /// trampoline can create its report before it restricts itself. The
    /// inner policy never grants write access at, beneath or above
    /// [`SANDBOX_TRAMPOLINE_DIR`] (checked, see Errors), so inside the
    /// namespace only Landlock keeps the job from rewriting that report: a
    /// caller must not use this plan on a host without a usable Landlock
    /// ABI (the job runtime refuses to).
    ///
    /// The inner policy requires full Landlock filesystem enforcement,
    /// independently of the job requirement, to protect the report from the
    /// job. Unsupported/partial enforcement refuses execution, not merely
    /// backend selection; the host probe is not a security boundary.
    ///
    /// # Errors
    /// Every error of [`plan`](Self::plan), and:
    /// - [`BwrapExecutorError::InvalidPath`] for a relative or non-normal
    ///   `mount` path.
    /// - [`BwrapExecutorError::Unsupported`] with [`Dimension::Filesystem`]
    ///   when the workspace overlaps [`SANDBOX_TRAMPOLINE_DIR`] or contains
    ///   `mount.exec_path` / `mount.attempt_dir` (a job could replace the
    ///   trampoline the next attempt runs on the host, or rewrite its own
    ///   plan and report), when a read-only bind would keep write or
    ///   execute through a broader Landlock rule on an ancestor
    ///   ([`PathMap::shadowed_read_only_binds`]), or when the inner policy
    ///   would keep write access to [`SANDBOX_TRAMPOLINE_DIR`].
    pub fn plan_with_trampoline(
        &self,
        spec: &JobSpec,
        policy: &SandboxPolicy,
        workspace_root: &Path,
        mount: &TrampolineMount,
    ) -> Result<BwrapTrampolinePlan, BwrapExecutorError> {
        for path in [&mount.exec_path, &mount.attempt_dir] {
            if !is_absolute_normal(path) {
                return Err(BwrapExecutorError::InvalidPath { path: path.clone() });
            }
        }
        let (bwrap, filesystem) = self.plan_launch(spec, policy, workspace_root, Some(mount))?;
        refuse_mount_overlaps(bwrap.workspace(), workspace_root, mount)?;
        let (path_map, module_binds) = namespace_view(&bwrap, workspace_root, &filesystem, mount)?;
        // A module bind beneath a writable policy path (a host `PATH`
        // entry below `/tmp`) gets read and execute by design; the mount
        // keeps it read-only.
        let shadowed: Vec<_> = path_map
            .shadowed_read_only_binds(&policy.filesystem)
            .into_iter()
            .filter(|bind| !module_binds.contains(*bind))
            .collect();
        if !shadowed.is_empty() {
            let names: Vec<String> = shadowed
                .iter()
                .map(|bind| bind.sandbox.display().to_string())
                .collect();
            return Err(BwrapExecutorError::Unsupported {
                dimension: Dimension::Filesystem,
                reason: format!(
                    "read-only binds {names:?} lie beneath a policy path that keeps write or \
                     execute inside the namespace, so Landlock would not keep them read-only; \
                     the bubblewrap trampoline backend refuses such a plan"
                ),
            });
        }
        let unmapped = path_map.unmapped_paths(&policy.filesystem);
        // The attempt directory is writable for the trusted trampoline to
        // publish its report. It must become inaccessible before the job
        // executes, even for None/BestEffort jobs or stale host probes.
        // Partial old ABIs are not sufficient (e.g. unmediated truncate).
        let mut inner_policy = policy.mapped(&path_map).hard_requirement();
        inner_policy.network = inner_network(&policy.network, bwrap.network_mode());
        add_module_rules(&mut inner_policy.filesystem, &module_binds);
        refuse_writable_trampoline_dir(&inner_policy.filesystem)?;
        Ok(BwrapTrampolinePlan {
            bwrap,
            inner_policy,
            path_map,
            unmapped,
            module_binds,
        })
    }

    /// Shared core of [`plan`](Self::plan) and
    /// [`plan_with_trampoline`](Self::plan_with_trampoline): with a mount,
    /// the trampoline is the sandboxed command and its binds are spliced in.
    fn plan_launch(
        &self,
        spec: &JobSpec,
        policy: &SandboxPolicy,
        workspace_root: &Path,
        mount: Option<&TrampolineMount>,
    ) -> Result<(BwrapJobPlan, FilesystemMapping), BwrapExecutorError> {
        if !self.executable.is_absolute() {
            return Err(BwrapExecutorError::RelativeExecutable {
                path: self.executable.clone(),
            });
        }
        spec.validate()?;
        let binding = resolve_workspace(workspace_root)?;
        let workspace = binding.canonical_root().to_path_buf();
        if workspace == Path::new("/") {
            // Every policy path would count as a workspace subpath: the
            // whole host would be bound read-write.
            return Err(BwrapExecutorError::Unsupported {
                dimension: Dimension::Filesystem,
                reason: "the workspace is the host root; the bubblewrap backends need a \
                         workspace below it (the whole host would be bound read-write)"
                    .to_owned(),
            });
        }
        let filesystem = map_filesystem(&policy.filesystem, workspace_root, &workspace)?;
        let (network_mode, network_state) = self.map_network(&policy.network)?;
        let capabilities_state = map_capabilities(&policy.capabilities)?;

        let mut permissions = vec![Permission::ReadWorkspace, Permission::ExecuteProcess];
        if filesystem.workspace_writable {
            permissions.push(Permission::WriteWorkspace);
        }
        let relay_prefix = match network_mode {
            NetworkMode::ProxyOnly(_) => {
                permissions.push(Permission::NetworkAccess);
                RELAY_PREFIX_LEN
            }
            NetworkMode::None => 0,
        };
        let sandbox = SandboxSpec::from_resolved(binding, PermissionSet::from_policy(permissions));

        let mut launcher = BwrapLauncher::new(self.executable.clone())
            .with_network_mode(network_mode.clone())
            .with_read_only_paths(filesystem.extra_read_only.clone())
            .with_profile(&self.modules);
        if let Some(binding) = &self.host_path {
            launcher = launcher.with_host_path(binding.clone());
        }
        if let Some(size) = self.tmpfs_size {
            launcher = launcher.with_tmpfs_size(size);
        }
        if let Some((uid, gid)) = self.identity {
            launcher = launcher.with_identity(uid, gid);
        }
        let (command, extra_binds) = match mount {
            Some(mount) => {
                launcher = launcher.with_shared_cgroup_namespace();
                (trampoline_command(), trampoline_binds(mount))
            }
            None => (
                std::iter::once(&spec.program)
                    .chain(&spec.args)
                    .map(OsString::from)
                    .collect(),
                Vec::new(),
            ),
        };
        let planned = launcher.plan(&sandbox, &command)?;
        // The launcher's own binds, before the job context is spliced in
        // (its environment values never reach the parser).
        let mut filesystem = filesystem;
        filesystem.launcher_binds = launcher_binds(planned.args())?;
        let args = splice_job_context(
            planned.args(),
            &workspace,
            spec,
            command.len() + relay_prefix,
            &extra_binds,
        )?;

        let plan = BwrapJobPlan {
            executable: self.executable.clone(),
            args,
            report: SandboxReport {
                // Partial: mounts confine reads/writes, but bwrap cannot separate
                // read from execute the way Landlock can.
                filesystem: EnforcementState::Partial,
                network: network_state,
                // bwrap always sets PR_SET_NO_NEW_PRIVS before exec.
                no_new_privs: EnforcementState::Enforced,
                capabilities: capabilities_state,
                // cgroup / trampoline layer, not bwrap.
                resource_limits: EnforcementState::NotEnforced,
            },
            network_mode,
            workspace,
            workspace_writable: filesystem.workspace_writable,
            withheld: filesystem.withheld.clone(),
        };
        Ok((plan, filesystem))
    }

    fn map_network(
        &self,
        network: &NetworkPolicy,
    ) -> Result<(NetworkMode, EnforcementState), BwrapExecutorError> {
        let unsupported = |what: &str| BwrapExecutorError::Unsupported {
            dimension: Dimension::Network,
            reason: format!(
                "{what}: the bubblewrap backend only supports no network or proxy-only \
                 egress through a configured relay (it never shares the host network \
                 namespace); configure BwrapExecutor::with_proxy_relay or use a \
                 network-denying profile"
            ),
        };
        match network {
            // Own, empty network namespace: stronger than Landlock's TCP-only
            // rules (UDP, raw and abstract sockets are cut off too).
            NetworkPolicy::Deny => Ok((NetworkMode::None, EnforcementState::Enforced)),
            // Narrowed to proxy-only egress, which satisfies "allow".
            NetworkPolicy::Allow => match &self.relay {
                Some(relay) => Ok((
                    NetworkMode::ProxyOnly(relay.clone()),
                    EnforcementState::Enforced,
                )),
                None => Err(unsupported("unrestricted network requested")),
            },
            // No direct egress at all; the port allowlist itself is not
            // enforced by the sandbox but replaced by the egress proxy's
            // host policy, hence Partial.
            NetworkPolicy::ConnectTcpPorts(ports) => match &self.relay {
                Some(relay) => Ok((
                    NetworkMode::ProxyOnly(relay.clone()),
                    EnforcementState::Partial,
                )),
                None => Err(unsupported(&format!(
                    "TCP connect to ports {ports:?} requested"
                ))),
            },
            _ => Err(BwrapExecutorError::Unsupported {
                dimension: Dimension::Network,
                reason: "unknown network policy variant".to_owned(),
            }),
        }
    }
}

/// A fully planned Bubblewrap job launch.
///
/// # Description
/// Inspectable before anything runs: the complete `bwrap` argv, the
/// network mode, paths that were deliberately not bound, and the predicted
/// per-dimension [`SandboxReport`]. [`into_command`](Self::into_command)
/// turns it into a [`Command`] for `harw-job-linux`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BwrapJobPlan {
    executable: PathBuf,
    args: Vec<OsString>,
    report: SandboxReport,
    network_mode: NetworkMode,
    workspace: PathBuf,
    workspace_writable: bool,
    withheld: Vec<PathBuf>,
}

impl BwrapJobPlan {
    /// The `bwrap` executable (absolute).
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// The `bwrap` arguments (everything after the executable), ending in
    /// `-- [relay prefix] <program> <args…>`.
    #[must_use]
    pub fn args(&self) -> &[OsString] {
        &self.args
    }

    /// Predicted enforcement of the `bwrap` layer (see
    /// [`BwrapExecutor::plan`]).
    #[must_use]
    pub fn report(&self) -> SandboxReport {
        self.report
    }

    /// The network mode of the sandbox.
    #[must_use]
    pub fn network_mode(&self) -> &NetworkMode {
        &self.network_mode
    }

    /// The canonical workspace root mounted into the sandbox.
    #[must_use]
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// Whether the workspace is mounted read-write.
    #[must_use]
    pub fn workspace_writable(&self) -> bool {
        self.workspace_writable
    }

    /// Policy paths that were deliberately **not** bound (narrower than
    /// requested), e.g. `/sys`, `/run` or an ancestor of the workspace.
    #[must_use]
    pub fn withheld_paths(&self) -> &[PathBuf] {
        &self.withheld
    }

    /// Converts the plan into a [`Command`] running `bwrap`.
    ///
    /// # Description
    /// The outer environment is cleared (`bwrap` needs none; the sandbox
    /// environment is set by `--clearenv`/`--setenv` in the argv). Stdio and
    /// process-group settings are left to the caller, e.g.
    /// `harw_job_linux::LinuxProcess::spawn(&mut command)` or
    /// `harw_job_linux::LinuxJobGroup::spawn(&mut command)`.
    #[must_use]
    pub fn into_command(self) -> Command {
        let mut command = Command::new(self.executable);
        command.args(self.args).env_clear();
        command
    }
}

/// Where the trampoline composition comes from on the host
/// ([`BwrapExecutor::plan_with_trampoline`]).
///
/// Both paths are absolute host paths. The attempt directory should be a
/// fresh, private (`0700`) directory per attempt: the caller writes the
/// inner plan into it ([`Self::inner_plan_path`]) and the inner trampoline
/// creates its report there ([`Self::inner_report_path`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrampolineMount {
    /// The trusted `harw-job-exec` binary, bound read-only at
    /// [`SANDBOX_TRAMPOLINE_PATH`].
    pub exec_path: PathBuf,
    /// The per-attempt directory, bound read-write at
    /// [`SANDBOX_ATTEMPT_DIR`].
    pub attempt_dir: PathBuf,
}

impl TrampolineMount {
    /// Host path of the inner plan (`<attempt_dir>/inner.json`).
    #[must_use]
    pub fn inner_plan_path(&self) -> PathBuf {
        self.attempt_dir.join(INNER_PLAN_FILE)
    }

    /// Host path of the inner trampoline's report
    /// (`<attempt_dir>/report.json`).
    #[must_use]
    pub fn inner_report_path(&self) -> PathBuf {
        self.attempt_dir.join(INNER_REPORT_FILE)
    }
}

/// A Bubblewrap launch whose sandboxed command is the `harw-job-exec`
/// trampoline ([`BwrapExecutor::plan_with_trampoline`]).
///
/// # Description
/// - [`bwrap`](Self::bwrap): the full `bwrap` argv and the **predicted**
///   report of the namespace layer.
/// - [`inner_policy`](Self::inner_policy): the policy the inner trampoline
///   applies, planned against the namespace view
///   ([`path_map`](Self::path_map)).
/// - [`unmapped_paths`](Self::unmapped_paths): policy paths that have no
///   rule inside the namespace (not visible there); usually the
///   [`withheld_paths`](Self::withheld_paths).
///
/// The report of the composition is
/// `harw_job_exec::merge_reports(&plan.bwrap().report(), &inner_report)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BwrapTrampolinePlan {
    bwrap: BwrapJobPlan,
    inner_policy: SandboxPolicy,
    path_map: PathMap,
    unmapped: Vec<PathBuf>,
    module_binds: Vec<PathBind>,
}

impl BwrapTrampolinePlan {
    /// The `bwrap` launch (argv, predicted namespace-layer report).
    #[must_use]
    pub fn bwrap(&self) -> &BwrapJobPlan {
        &self.bwrap
    }

    /// The sandbox policy for the inner trampoline, in in-namespace paths.
    #[must_use]
    pub fn inner_policy(&self) -> &SandboxPolicy {
        &self.inner_policy
    }

    /// The namespace view the inner policy was planned against.
    #[must_use]
    pub fn path_map(&self) -> &PathMap {
        &self.path_map
    }

    /// Policy paths without a rule inside the namespace (sorted).
    #[must_use]
    pub fn unmapped_paths(&self) -> &[PathBuf] {
        &self.unmapped
    }

    /// Policy paths deliberately not bound (see
    /// [`BwrapJobPlan::withheld_paths`]).
    #[must_use]
    pub fn withheld_paths(&self) -> &[PathBuf] {
        self.bwrap.withheld_paths()
    }

    /// The binds of the profile modules and the host `PATH` binding (module
    /// docs, PL-97 E2b), in argv order; empty without modules. Each one is
    /// in [`path_map`](Self::path_map) and has its own rule in
    /// [`inner_policy`](Self::inner_policy). A caller that builds its own
    /// inner policy from the path map (a per-job write scope) has to add
    /// these rules itself, or the job cannot use the modules.
    #[must_use]
    pub fn module_binds(&self) -> &[PathBind] {
        &self.module_binds
    }
}

/// Adds the Landlock rules of the module binds (in-namespace paths): read
/// and execute for a read-only bind, read, write and execute for a
/// read-write one, which is what the mount alone grants.
fn add_module_rules(filesystem: &mut FilesystemPolicy, binds: &[PathBind]) {
    for bind in binds {
        if bind.mode == BindMode::ReadWrite {
            push_unique(&mut filesystem.read_write, &bind.sandbox);
        }
        push_unique(&mut filesystem.exec, &bind.sandbox);
    }
}

/// Every `--bind`, `--ro-bind` and `--ro-bind-try` of a `harw-sandbox`
/// launcher argv, up to its `--` separator. The argv is parsed option by
/// option (every value is skipped by the option's arity), so a value that
/// looks like an option is never read as one. An option the launcher is
/// not known to emit fails the plan (fail closed).
fn launcher_binds(args: &[OsString]) -> Result<Vec<PathBind>, BwrapExecutorError> {
    let mut binds = Vec::new();
    let mut rest = args.iter();
    while let Some(option) = rest.next() {
        let Some(option) = option.to_str() else {
            return Err(BwrapExecutorError::PlanShape);
        };
        let (arity, mode) = match option {
            "--" => return Ok(binds),
            "--die-with-parent" | "--new-session" | "--unshare-all" | "--unshare-net"
            | "--unshare-user" | "--unshare-ipc" | "--unshare-pid" | "--unshare-uts"
            | "--unshare-cgroup" | "--clearenv" => (0, None),
            "--uid" | "--gid" | "--proc" | "--dev" | "--size" | "--tmpfs" | "--dir" | "--chdir" => {
                (1, None)
            }
            "--setenv" => (2, None),
            "--bind" => (2, Some(BindMode::ReadWrite)),
            "--ro-bind" | "--ro-bind-try" => (2, Some(BindMode::ReadOnly)),
            _ => return Err(BwrapExecutorError::PlanShape),
        };
        let values: Vec<&OsString> = rest.by_ref().take(arity).collect();
        if values.len() != arity {
            return Err(BwrapExecutorError::PlanShape);
        }
        if let (Some(mode), [host, sandbox]) = (mode, values.as_slice()) {
            let invalid = |path: &OsString| BwrapExecutorError::InvalidPath {
                path: PathBuf::from(path),
            };
            if !Path::new(host).is_absolute() {
                return Err(invalid(host));
            }
            if !Path::new(sandbox).is_absolute() {
                return Err(invalid(sandbox));
            }
            binds.push(PathBind {
                host: PathBuf::from(host),
                sandbox: PathBuf::from(sandbox),
                mode,
            });
        }
    }
    Err(BwrapExecutorError::PlanShape)
}

/// The sandboxed command of the trampoline composition.
fn trampoline_command() -> Vec<OsString> {
    let attempt = Path::new(SANDBOX_ATTEMPT_DIR);
    vec![
        OsString::from(SANDBOX_TRAMPOLINE_PATH),
        OsString::from("--plan"),
        attempt.join(INNER_PLAN_FILE).into_os_string(),
        OsString::from("--report"),
        attempt.join(INNER_REPORT_FILE).into_os_string(),
    ]
}

/// `--dir` for every component of [`SANDBOX_TRAMPOLINE_DIR`], then the
/// trampoline (read-only) and attempt directory (read-write) binds.
fn trampoline_binds(mount: &TrampolineMount) -> Vec<OsString> {
    let mut args = Vec::new();
    let mut current = PathBuf::from("/");
    for component in Path::new(SANDBOX_TRAMPOLINE_DIR).components() {
        if let Component::Normal(part) = component {
            current.push(part);
            args.extend([OsString::from("--dir"), current.clone().into_os_string()]);
        }
    }
    args.extend([
        OsString::from("--ro-bind"),
        mount.exec_path.clone().into_os_string(),
        OsString::from(SANDBOX_TRAMPOLINE_PATH),
        OsString::from("--bind"),
        mount.attempt_dir.clone().into_os_string(),
        OsString::from(SANDBOX_ATTEMPT_DIR),
    ]);
    args
}

/// Whether one of the two paths lies at or beneath the other.
fn overlaps(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

/// Refuses an inner policy that keeps write access at, beneath or above
/// [`SANDBOX_TRAMPOLINE_DIR`]. The attempt directory is bound read-write
/// only so the inner trampoline can create its report; the inner Landlock
/// layer must keep the job out of it, or the job could rewrite the report
/// that `merge_reports` trusts (PL-97 security review, finding 4).
/// `exec` entries grant read and execute, never write.
fn refuse_writable_trampoline_dir(filesystem: &FilesystemPolicy) -> Result<(), BwrapExecutorError> {
    let dir = Path::new(SANDBOX_TRAMPOLINE_DIR);
    match filesystem
        .read_write
        .iter()
        .find(|path| overlaps(path, dir))
    {
        Some(path) => Err(BwrapExecutorError::Unsupported {
            dimension: Dimension::Filesystem,
            reason: format!(
                "the inner policy would grant write access to '{}', which covers \
                 {SANDBOX_TRAMPOLINE_DIR} (trampoline, plan and report); the bubblewrap \
                 trampoline backend never lets the job write there",
                path.display()
            ),
        }),
        None => Ok(()),
    }
}

/// Refuses a workspace that collides with the trampoline composition: its
/// in-sandbox destination overlapping [`SANDBOX_TRAMPOLINE_DIR`], or a
/// trampoline binary / attempt directory inside it (reachable, and with a
/// writable workspace replaceable, by the job).
fn refuse_mount_overlaps(
    workspace: &Path,
    requested_root: &Path,
    mount: &TrampolineMount,
) -> Result<(), BwrapExecutorError> {
    let refused = |what: &str, path: &Path| BwrapExecutorError::Unsupported {
        dimension: Dimension::Filesystem,
        reason: format!(
            "{what} '{}' overlaps the workspace '{}'; the bubblewrap trampoline backend \
             keeps the trampoline and its plan/report files out of the job's reach",
            path.display(),
            workspace.display()
        ),
    };
    if overlaps(workspace, Path::new(SANDBOX_TRAMPOLINE_DIR)) {
        return Err(refused(
            "the sandbox trampoline directory",
            Path::new(SANDBOX_TRAMPOLINE_DIR),
        ));
    }
    for (what, path) in [
        ("the trampoline binary", &mount.exec_path),
        ("the attempt directory", &mount.attempt_dir),
    ] {
        let canonical = path.canonicalize().ok();
        let candidates = std::iter::once(path.as_path()).chain(canonical.as_deref());
        for candidate in candidates {
            if overlaps(candidate, workspace) || overlaps(candidate, requested_root) {
                return Err(refused(what, path));
            }
        }
    }
    Ok(())
}

/// The [`PathMap`] of a trampoline plan (table in the module docs), and the
/// binds of the profile modules in it: every launcher bind that is not one
/// of the classified entries.
fn namespace_view(
    plan: &BwrapJobPlan,
    requested_root: &Path,
    filesystem: &FilesystemMapping,
    mount: &TrampolineMount,
) -> Result<(PathMap, Vec<PathBind>), BwrapExecutorError> {
    let invalid = |error: PathMapError| BwrapExecutorError::InvalidPath { path: error.path };
    let classified = classified_view(plan, requested_root, filesystem, mount)?;
    let mut map = classified.clone();
    let mut modules = Vec::new();
    for bind in &filesystem.launcher_binds {
        if classified.binds().contains(bind) || modules.contains(bind) {
            continue;
        }
        map = map
            .try_with_bind(&bind.host, &bind.sandbox, bind.mode)
            .map_err(invalid)?;
        modules.push(bind.clone());
    }
    Ok((map, modules))
}

/// The classified part of the namespace view: workspace, system trees,
/// policy read-only paths, bwrap's own mounts, relay and trampoline.
fn classified_view(
    plan: &BwrapJobPlan,
    requested_root: &Path,
    filesystem: &FilesystemMapping,
    mount: &TrampolineMount,
) -> Result<PathMap, BwrapExecutorError> {
    let invalid = |error: PathMapError| BwrapExecutorError::InvalidPath { path: error.path };
    let workspace_mode = if plan.workspace_writable() {
        BindMode::ReadWrite
    } else {
        BindMode::ReadOnly
    };
    let workspace = plan.workspace();
    let mut map = PathMap::new()
        .try_with_bind(workspace, workspace, workspace_mode)
        .map_err(invalid)?;
    if requested_root != workspace {
        // Policy paths may be spelled with the requested (non-canonical)
        // root; the bind is the same mount.
        map = map
            .try_with_bind(requested_root, workspace, workspace_mode)
            .map_err(invalid)?;
    }
    // `harw-sandbox` binds a system tree only where it exists.
    for tree in ALWAYS_BOUND.iter().filter(|tree| Path::new(tree).exists()) {
        map = map
            .try_with_bind(tree, tree, BindMode::ReadOnly)
            .map_err(invalid)?;
    }
    for path in NSS_FILES
        .iter()
        .map(PathBuf::from)
        .chain(filesystem.extra_read_only.iter().cloned())
    {
        map = map
            .try_with_bind(path.clone(), path, BindMode::ReadOnly)
            .map_err(invalid)?;
    }
    map = map
        .try_with_provided("/proc", BindMode::ReadOnly)
        .and_then(|map| map.try_with_provided("/dev", BindMode::ReadWrite))
        .and_then(|map| map.try_with_provided("/tmp", BindMode::ReadWrite))
        .map_err(invalid)?;
    if let NetworkMode::ProxyOnly(relay) = plan.network_mode() {
        map = map
            .try_with_bind(&relay.binary, SANDBOX_RELAY_PATH, BindMode::ReadOnly)
            .and_then(|map| {
                map.try_with_bind(
                    &relay.proxy_socket,
                    SANDBOX_PROXY_SOCKET_PATH,
                    BindMode::ReadWrite,
                )
            })
            .map_err(invalid)?;
    }
    map.try_with_bind(
        &mount.exec_path,
        SANDBOX_TRAMPOLINE_PATH,
        BindMode::ReadOnly,
    )
    .and_then(|map| map.try_with_bind(&mount.attempt_dir, SANDBOX_ATTEMPT_DIR, BindMode::ReadWrite))
    .map_err(invalid)
}

/// The network policy the inner trampoline applies inside the namespace.
///
/// The namespace already enforces the policy: an own empty network
/// namespace (`Deny`) or proxy-only egress (`Allow`) leave nothing to
/// Landlock, so the inner layer requests nothing (and reports `Enforced`
/// for "nothing requested", exactly what the namespace provides). A port
/// allowlist behind the relay is only `Partial` in the namespace; the
/// inner layer narrows TCP `connect` to the relay port, the only egress
/// the namespace has, and reports what Landlock achieves.
fn inner_network(policy: &NetworkPolicy, mode: &NetworkMode) -> NetworkPolicy {
    match (policy, mode) {
        (NetworkPolicy::ConnectTcpPorts(_), NetworkMode::ProxyOnly(relay)) => {
            NetworkPolicy::ConnectTcpPorts(vec![relay.listen_port])
        }
        _ => NetworkPolicy::Allow,
    }
}

/// Result of classifying the filesystem lists.
#[derive(Debug, Default)]
struct FilesystemMapping {
    workspace_writable: bool,
    extra_read_only: Vec<PathBuf>,
    withheld: Vec<PathBuf>,
    /// Every bind of the launcher's argv ([`launcher_binds`]).
    launcher_binds: Vec<PathBind>,
}

fn resolve_workspace(workspace_root: &Path) -> Result<WorkspaceBinding, BwrapExecutorError> {
    let workspace_error = |reason: String| BwrapExecutorError::Workspace {
        path: workspace_root.to_path_buf(),
        reason,
    };
    let canonical = workspace_root
        .canonicalize()
        .map_err(|error| workspace_error(error.to_string()))?;
    let tenant = TenantId::from_str(REGISTRY_TENANT);
    let workspace = WorkspaceId::from_str(REGISTRY_WORKSPACE);
    let registry = WorkspaceRegistry::build(
        &canonical,
        [WorkspaceRegistration {
            tenant: tenant.clone(),
            workspace: workspace.clone(),
            root: canonical.clone(),
        }],
    )
    .map_err(|error| workspace_error(error.to_string()))?;
    registry
        .resolve(&tenant, &workspace)
        .map_err(|error| workspace_error(error.to_string()))
}

fn is_absolute_normal(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::RootDir | Component::Normal(_)))
}

fn under_any(path: &Path, trees: &[&str]) -> bool {
    trees.iter().any(|tree| path.starts_with(tree))
}

fn push_unique(list: &mut Vec<PathBuf>, path: &Path) {
    if !list.iter().any(|existing| existing == path) {
        list.push(path.to_path_buf());
    }
}

/// `Some(relative)` if `path` is the workspace (empty relative path) or
/// below it, matched against both the requested and the canonical root.
fn workspace_relative<'a>(
    path: &'a Path,
    requested_root: &Path,
    workspace: &Path,
) -> Option<&'a Path> {
    path.strip_prefix(workspace)
        .or_else(|_| path.strip_prefix(requested_root))
        .ok()
}

fn map_filesystem(
    policy: &FilesystemPolicy,
    requested_root: &Path,
    workspace: &Path,
) -> Result<FilesystemMapping, BwrapExecutorError> {
    let unsupported = |path: &Path, why: &str| BwrapExecutorError::Unsupported {
        dimension: Dimension::Filesystem,
        reason: format!("'{}': {why}", path.display()),
    };
    let mut mapping = FilesystemMapping::default();
    let mut workspace_granted = false;
    let mut writable_subpaths: Vec<PathBuf> = Vec::new();
    let entries = policy
        .read_only
        .iter()
        .chain(&policy.exec)
        .map(|path| (path, false))
        .chain(policy.read_write.iter().map(|path| (path, true)));
    for (path, write) in entries {
        if let Some(relative) = workspace_relative(path, requested_root, workspace) {
            workspace_granted = true;
            if write {
                if relative.as_os_str().is_empty() {
                    mapping.workspace_writable = true;
                } else {
                    writable_subpaths.push(path.clone());
                }
            }
            continue;
        }
        if !is_absolute_normal(path) {
            return Err(BwrapExecutorError::InvalidPath { path: path.clone() });
        }
        if path == Path::new("/") {
            return Err(unsupported(
                path.as_path(),
                "binding the whole host root is refused",
            ));
        }
        if under_any(path, SANDBOX_PROVIDED_TREES)
            || SANDBOX_PROVIDED_EXACT
                .iter()
                .any(|exact| path == Path::new(exact))
        {
            continue;
        }
        if under_any(path, WITHHELD_TREES) {
            push_unique(&mut mapping.withheld, path);
            continue;
        }
        if under_any(path, ALWAYS_BOUND) {
            if write {
                return Err(unsupported(
                    path.as_path(),
                    "system trees are always mounted read-only by the bubblewrap backend",
                ));
            }
            continue;
        }
        if write {
            return Err(unsupported(
                path.as_path(),
                "the bubblewrap backend can only make the workspace (and its own /tmp and \
                 /dev) writable; extra read-write host paths are not supported",
            ));
        }
        if workspace.starts_with(path) || requested_root.starts_with(path) {
            // Binding an ancestor would overlay the workspace bind;
            // harw-sandbox skips it, so report it honestly as withheld.
            push_unique(&mut mapping.withheld, path);
            continue;
        }
        push_unique(&mut mapping.extra_read_only, path);
    }
    if !workspace_granted {
        return Err(BwrapExecutorError::Unsupported {
            dimension: Dimension::Filesystem,
            reason: format!(
                "the policy grants no access to the workspace '{}', but the bubblewrap \
                 backend always mounts it",
                workspace.display()
            ),
        });
    }
    if let (false, Some(path)) = (mapping.workspace_writable, writable_subpaths.first()) {
        return Err(unsupported(
            path.as_path(),
            "a read-write path below a read-only workspace is not supported; the \
             bubblewrap backend binds the workspace as a whole",
        ));
    }
    Ok(mapping)
}

fn map_capabilities(policy: &CapabilityPolicy) -> Result<EnforcementState, BwrapExecutorError> {
    match policy {
        CapabilityPolicy::DropAll => Ok(EnforcementState::Enforced),
        CapabilityPolicy::Keep(keep) if keep.is_empty() => Ok(EnforcementState::Enforced),
        CapabilityPolicy::Keep(keep) => Err(BwrapExecutorError::Unsupported {
            dimension: Dimension::Capabilities,
            reason: format!(
                "keeping capabilities {keep:?} is not supported; the bubblewrap backend \
                 always drops every capability"
            ),
        }),
    }
}

/// Replaces the `--chdir <workspace>` of the `harw-sandbox` plan by the
/// job's working directory and inserts `extra` (additional mounts) and the
/// job environment before it.
///
/// `tail_len` is the number of argv items after the `--` separator (relay
/// prefix plus command). The plan is expected to end in
/// `--chdir <workspace> -- <tail…>`.
fn splice_job_context(
    planned: &[OsString],
    workspace: &Path,
    spec: &JobSpec,
    tail_len: usize,
    extra: &[OsString],
) -> Result<Vec<OsString>, BwrapExecutorError> {
    let separator = planned
        .len()
        .checked_sub(tail_len + 1)
        .ok_or(BwrapExecutorError::PlanShape)?;
    let chdir = separator
        .checked_sub(2)
        .ok_or(BwrapExecutorError::PlanShape)?;
    let shape_ok = planned.get(chdir).is_some_and(|arg| arg == "--chdir")
        && planned
            .get(chdir + 1)
            .is_some_and(|arg| arg.as_os_str() == workspace.as_os_str())
        && planned.get(separator).is_some_and(|arg| arg == "--");
    if !shape_ok {
        return Err(BwrapExecutorError::PlanShape);
    }
    let head = planned.get(..chdir).ok_or(BwrapExecutorError::PlanShape)?;
    let rest = planned
        .get(separator..)
        .ok_or(BwrapExecutorError::PlanShape)?;
    let mut args = head.to_vec();
    args.extend_from_slice(extra);
    for (name, value) in &spec.env {
        args.extend([
            OsString::from("--setenv"),
            OsString::from(name),
            OsString::from(value),
        ]);
    }
    let working_dir = match spec.working_dir.as_str() {
        "." => workspace.to_path_buf(),
        relative => workspace.join(relative),
    };
    args.extend([OsString::from("--chdir"), working_dir.into_os_string()]);
    args.extend_from_slice(rest);
    Ok(args)
}

#[cfg(test)]
mod tests;
