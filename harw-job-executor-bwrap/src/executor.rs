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
//! namespace) restores that distinction; see the crate docs.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use harw_authority::{
    Permission, PermissionSet, SandboxSpec, WorkspaceBinding, WorkspaceRegistration,
    WorkspaceRegistry,
};
use harw_job_core::{EnforcementState, JobSpec, SandboxReport};
use harw_job_linux::{CapabilityPolicy, FilesystemPolicy, NetworkPolicy, SandboxPolicy};
use harw_sandbox::{BwrapLauncher, NetworkMode, RelaySpec};
use harw_types::{TenantId, WorkspaceId};

use crate::error::{BwrapExecutorError, Dimension};

/// Trees `harw-sandbox` binds read-only (and executable) in every plan.
const ALWAYS_BOUND: &[&str] = &["/usr", "/bin", "/lib", "/lib64"];
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
/// policy is the only source of binds.
///
/// # Concurrency
/// `Send + Sync`; planning has no side effects besides canonicalizing the
/// workspace root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BwrapExecutor {
    executable: PathBuf,
    relay: Option<RelaySpec>,
    identity: Option<(u32, u32)>,
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
    ///   (`HOME`, `PATH`, `USER`/`LOGNAME`), then `spec.env` (which may
    ///   override the baseline).
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
    ///   express (see above).
    /// - [`BwrapExecutorError::Sandbox`] if `harw-sandbox` rejects the plan.
    pub fn plan(
        &self,
        spec: &JobSpec,
        policy: &SandboxPolicy,
        workspace_root: &Path,
    ) -> Result<BwrapJobPlan, BwrapExecutorError> {
        if !self.executable.is_absolute() {
            return Err(BwrapExecutorError::RelativeExecutable {
                path: self.executable.clone(),
            });
        }
        spec.validate()?;
        let binding = resolve_workspace(workspace_root)?;
        let workspace = binding.canonical_root().to_path_buf();
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
            .with_read_only_paths(filesystem.extra_read_only);
        if let Some((uid, gid)) = self.identity {
            launcher = launcher.with_identity(uid, gid);
        }
        let command: Vec<OsString> = std::iter::once(&spec.program)
            .chain(&spec.args)
            .map(OsString::from)
            .collect();
        let planned = launcher.plan(&sandbox, &command)?;
        let args = splice_job_context(
            planned.args(),
            &workspace,
            spec,
            command.len() + relay_prefix,
        )?;

        Ok(BwrapJobPlan {
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
            withheld: filesystem.withheld,
        })
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

/// Result of classifying the filesystem lists.
#[derive(Debug, Default)]
struct FilesystemMapping {
    workspace_writable: bool,
    extra_read_only: Vec<PathBuf>,
    withheld: Vec<PathBuf>,
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
/// job's working directory and inserts the job environment before it.
///
/// `tail_len` is the number of argv items after the `--` separator (relay
/// prefix plus command). The plan is expected to end in
/// `--chdir <workspace> -- <tail…>`.
fn splice_job_context(
    planned: &[OsString],
    workspace: &Path,
    spec: &JobSpec,
    tail_len: usize,
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
