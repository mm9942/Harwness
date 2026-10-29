//! `shell.exec` and `job.start` through the job runtime's sandbox chain
//! (PL-97 E2b).
//!
//! # What is submitted
//! With [`SandboxExecution::Chain`] (the default) a sandboxed call no longer
//! pre-builds a `prlimit … -- bwrap …` argv and runs it as a host job. It
//! submits `/bin/sh -c <command>` (exactly the shell semantics of before)
//! as a [`CommandSandbox::Workspace`] request:
//!
//! | Field | Value |
//! |---|---|
//! | working directory and workspace root | the sandbox's canonical workspace root |
//! | profile | `no_network` with [`Permission::WriteWorkspace`], else `read_only_analysis`: the network stays denied, as in the legacy plan |
//! | requirement | `best_effort`; `required` with a write scope |
//! | modules | the trusted [`SandboxProfile`] (Cargo, tmux), the host `PATH` binding and the `/tmp` size of [`ShellLimits`] |
//! | resources | [`ShellLimits::job_resources`]; the timeout becomes the attempt's deadline |
//! | environment | empty: the sandbox sets its own baseline, as the legacy plan did |
//!
//! Output caps, truncation, cancel and exit codes stay with the command
//! port and the caller, unchanged.
//!
//! # Fail closed
//! The chain is used only when the command port runs workspace requests of
//! that profile on a Bubblewrap backend ([`route`]): the legacy path's
//! guarantees (mount and network namespace) never get weaker. Otherwise:
//! - a call with a write scope is refused with a tool error; nothing is
//!   submitted;
//! - an unscoped call keeps the legacy path, and the first one per session
//!   logs `runs without cgroup/landlock` ([`FallbackNotice`]).
//!
//! A call with a write scope needs more than a namespace: only the
//! `bwrap-trampoline` backend enforces a scope for these offline profiles
//! (plain `bwrap` cannot separate the scope from the workspace bind, the
//! Landlock trampoline has no network namespace). On any other backend it
//! is refused here, before anything is submitted ([`scoped_backend_refusal`]).
//!
//! The network: the legacy plan never had any (`NetworkMode::None`, an own
//! empty network namespace), so the chain profiles deny it as well. A
//! networked profile would get no Bubblewrap backend from the host port
//! (no egress relay is configured), so such a call could only fall back or,
//! with a write scope, be refused.
//!
//! [`SandboxExecution::Legacy`] keeps the legacy path for every call.

use std::collections::HashSet;
use std::num::NonZeroU64;
use std::sync::{Mutex, PoisonError};

use harw_authority::{Permission, SandboxSpec};
use harw_command::{
    BackendKind, CommandPort, CommandSandbox, JobWriteScope, ResourceRequest, SandboxProfileName,
    SandboxRequirement, WorkspaceSandbox,
};
use harw_sandbox::{HostPathBinding, SandboxProfile};
use tracing::warn;

use crate::limits::ShellLimits;

/// Most sessions [`FallbackNotice`] remembers before it starts over.
const MAX_NOTICED_SESSIONS: usize = 4096;

/// How `shell.exec` and `job.start` start sandboxed processes (config
/// `[sandbox] execution`, PL-97 E2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SandboxExecution {
    /// Through the job runtime's sandbox chain (module docs).
    #[default]
    Chain,
    /// The previous path: a pre-built `prlimit … -- bwrap …` argv run as a
    /// host job (no cgroup, no Landlock, no per-attempt report).
    Legacy,
}

/// A prepared chain submission: the request's sandbox and resources.
///
/// # Description
/// Built by `shell.exec` and [`crate::ShellToolProvider::prepare_background_launch`]
/// (`job.start`); the job manager puts both into its command request
/// ([`harw_command::CommandRequest::sandbox`] and
/// [`harw_command::CommandRequest::limits`]) instead of running the
/// prepared command as a host job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxedLaunch {
    sandbox: CommandSandbox,
    limits: ResourceRequest,
    backend: BackendKind,
}

impl SandboxedLaunch {
    /// A chain submission of `workspace` with `limits`, which the command
    /// port reported to run on `backend` (also for callers outside this
    /// crate, e.g. a child agent's start, PL-97 E2b).
    #[must_use]
    pub fn new(workspace: WorkspaceSandbox, limits: ResourceRequest, backend: BackendKind) -> Self {
        Self {
            sandbox: CommandSandbox::Workspace(Box::new(workspace)),
            limits,
            backend,
        }
    }

    /// The request's sandbox ([`CommandSandbox::Workspace`]).
    #[must_use]
    pub fn sandbox(&self) -> &CommandSandbox {
        &self.sandbox
    }

    /// The request's resources ([`ShellLimits::job_resources`]).
    #[must_use]
    pub fn limits(&self) -> &ResourceRequest {
        &self.limits
    }

    /// The backend the command port reported for the request.
    #[must_use]
    pub fn backend(&self) -> BackendKind {
        self.backend
    }

    /// Sandbox and resources.
    #[must_use]
    pub fn into_parts(self) -> (CommandSandbox, ResourceRequest) {
        (self.sandbox, self.limits)
    }
}

/// The chain profile of a call: the legacy plan never had network, so only
/// the workspace grant decides.
#[must_use]
pub(crate) fn chain_profile(sandbox: &SandboxSpec) -> SandboxProfileName {
    if sandbox.permissions().contains(Permission::WriteWorkspace) {
        SandboxProfileName::NoNetwork
    } else {
        SandboxProfileName::ReadOnlyAnalysis
    }
}

/// Where a sandboxed call goes in chain mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ChainRoute {
    /// Submit to the chain; the port runs it on this backend.
    Chain(BackendKind),
    /// The chain would be weaker than the legacy path, for this reason.
    Fallback(String),
}

/// Decides the route of a call with `profile` (module docs).
#[must_use]
pub(crate) fn route(port: Option<&dyn CommandPort>, profile: SandboxProfileName) -> ChainRoute {
    let Some(port) = port else {
        return ChainRoute::Fallback("no job runtime is installed".to_owned());
    };
    match port.workspace_backend(profile) {
        Some(kind) if kind.uses_bwrap() => ChainRoute::Chain(kind),
        Some(kind) => ChainRoute::Fallback(format!(
            "the job runtime would run it on the '{kind}' backend, which has no bubblewrap \
             namespace"
        )),
        None => {
            ChainRoute::Fallback("the job runtime runs no workspace sandbox requests".to_owned())
        }
    }
}

/// Why the chain cannot take a call with a write scope on `backend`, or
/// `None` when it can (`bwrap-trampoline` only, module docs).
#[must_use]
pub(crate) fn scoped_backend_refusal(backend: BackendKind) -> Option<String> {
    (backend != BackendKind::BwrapTrampoline).then(|| {
        format!(
            "the job runtime would run it on the '{backend}' backend, which cannot enforce a \
             write scope for this profile (only 'bwrap-trampoline' can)"
        )
    })
}

/// The refusal of a scoped call that cannot use the chain (fail closed).
#[must_use]
pub(crate) fn scoped_refusal(tool: &str, reason: &str) -> String {
    format!(
        "{tool}: refused: this call has a write scope, which only the job runtime's sandbox \
         chain enforces, and {reason}; nothing was started"
    )
}

/// The chain submission of a call over `sandbox` (module docs).
#[must_use]
pub(crate) fn sandboxed_launch(
    sandbox: &SandboxSpec,
    modules: &SandboxProfile,
    host_path: Option<&HostPathBinding>,
    limits: &ShellLimits,
    tmpfs_size: NonZeroU64,
    write_scope: Option<&JobWriteScope>,
    backend: BackendKind,
) -> SandboxedLaunch {
    let mut workspace = WorkspaceSandbox::new(
        sandbox.workspace().canonical_root(),
        chain_profile(sandbox),
        SandboxRequirement::BestEffort,
    );
    workspace.modules = modules.clone();
    workspace.host_path = host_path.cloned();
    workspace.tmpfs_size = Some(tmpfs_size);
    if let Some(scope) = write_scope {
        workspace.write_scope = Some(scope.clone());
        workspace.requirement = SandboxRequirement::Required;
    }
    SandboxedLaunch::new(workspace, limits.job_resources(), backend)
}

/// Logs the legacy fallback once per session (module docs).
#[derive(Debug, Default)]
pub(crate) struct FallbackNotice {
    sessions: Mutex<HashSet<String>>,
}

impl FallbackNotice {
    /// Logs `runs without cgroup/landlock` for the first fallback of
    /// `session_id`; returns whether it logged.
    pub(crate) fn note(&self, session_id: &str, tool: &str, reason: &str) -> bool {
        let mut sessions = self.sessions.lock().unwrap_or_else(PoisonError::into_inner);
        if sessions.len() >= MAX_NOTICED_SESSIONS {
            sessions.clear();
        }
        if !sessions.insert(session_id.to_owned()) {
            return false;
        }
        warn!(
            session_id,
            tool,
            reason,
            "{tool} runs without cgroup/landlock: the legacy bubblewrap path is used because \
             {reason}"
        );
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ChainRoute, FallbackNotice, SandboxExecution, route, sandboxed_launch,
        scoped_backend_refusal,
    };
    use crate::limits::ShellLimits;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_authority::{
        Permission, PermissionSet, SandboxSpec, WorkspaceRegistration, WorkspaceRegistry,
    };
    use harw_command::{
        BackendKind, CommandPort, CommandRequest, CommandSandbox, JobWriteScope, RunFuture,
        SandboxProfileName, SandboxRequirement, StartFuture,
    };
    use harw_sandbox::SandboxProfile;
    use harw_types::cancel::CancelToken;
    use harw_types::{TenantId, WorkspaceId};
    use std::num::NonZeroU64;

    #[derive(Debug)]
    struct Reporting(Option<BackendKind>);

    impl CommandPort for Reporting {
        fn run(&self, _request: CommandRequest, _cancel: CancelToken) -> RunFuture<'_> {
            Box::pin(std::future::pending())
        }

        fn start(&self, _request: CommandRequest) -> StartFuture<'_> {
            Box::pin(async { Err("unused".to_owned()) })
        }

        fn workspace_backend(&self, _profile: SandboxProfileName) -> Option<BackendKind> {
            self.0
        }
    }

    fn sandbox(dir: &tempfile::TempDir, permissions: Vec<Permission>) -> TestResult<SandboxSpec> {
        let root = dir.path().join("project");
        std::fs::create_dir_all(&root).map_err(ctx("project"))?;
        let registry = WorkspaceRegistry::build(
            dir.path(),
            [WorkspaceRegistration {
                tenant: TenantId::from_str("t"),
                workspace: WorkspaceId::from_str("project"),
                root,
            }],
        )
        .map_err(ctx("registry"))?;
        let binding = registry
            .resolve(&TenantId::from_str("t"), &WorkspaceId::from_str("project"))
            .map_err(ctx("resolve"))?;
        Ok(SandboxSpec::from_resolved(
            binding,
            PermissionSet::from_policy(permissions),
        ))
    }

    #[test]
    fn the_default_is_the_chain() {
        assert_eq!(SandboxExecution::default(), SandboxExecution::Chain);
    }

    #[test]
    fn only_a_bubblewrap_backend_takes_the_chain() {
        assert!(matches!(
            route(None, SandboxProfileName::NoNetwork),
            ChainRoute::Fallback(_)
        ));
        for kind in [BackendKind::BwrapTrampoline, BackendKind::Bwrap] {
            assert_eq!(
                route(Some(&Reporting(Some(kind))), SandboxProfileName::NoNetwork),
                ChainRoute::Chain(kind)
            );
        }
        for kind in [
            Some(BackendKind::LandlockTrampoline),
            Some(BackendKind::None),
            None,
        ] {
            assert!(matches!(
                route(Some(&Reporting(kind)), SandboxProfileName::NoNetwork),
                ChainRoute::Fallback(_)
            ));
        }
    }

    #[test]
    fn the_launch_mirrors_the_legacy_plan() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let size = NonZeroU64::new(256 * 1024 * 1024).ok_or(TestError::Missing("size"))?;
        let writable = sandbox(
            &dir,
            vec![Permission::ExecuteProcess, Permission::WriteWorkspace],
        )?;
        let launch = sandboxed_launch(
            &writable,
            &SandboxProfile::Strict,
            None,
            &ShellLimits::default(),
            size,
            None,
            BackendKind::Bwrap,
        );
        let CommandSandbox::Workspace(workspace) = launch.sandbox() else {
            return Err(TestError::Unexpected(format!("{:?}", launch.sandbox())));
        };
        assert_eq!(workspace.root, writable.workspace().canonical_root());
        assert_eq!(workspace.profile, SandboxProfileName::NoNetwork);
        assert_eq!(workspace.requirement, SandboxRequirement::BestEffort);
        assert_eq!(workspace.tmpfs_size, Some(size));
        assert_eq!(workspace.write_scope, None);
        assert_eq!(launch.limits(), &ShellLimits::default().job_resources());
        assert_eq!(launch.backend(), BackendKind::Bwrap);

        let read_only = sandbox(&dir, vec![Permission::ExecuteProcess])?;
        let scope = JobWriteScope::new([read_only.workspace().canonical_root().join("src")])
            .map_err(ctx("scope"))?;
        let launch = sandboxed_launch(
            &read_only,
            &SandboxProfile::Strict,
            None,
            &ShellLimits::default(),
            size,
            Some(&scope),
            BackendKind::Bwrap,
        );
        let CommandSandbox::Workspace(workspace) = launch.sandbox() else {
            return Err(TestError::Unexpected(format!("{:?}", launch.sandbox())));
        };
        assert_eq!(workspace.profile, SandboxProfileName::ReadOnlyAnalysis);
        assert_eq!(workspace.requirement, SandboxRequirement::Required);
        assert_eq!(workspace.write_scope, Some(scope));
        Ok(())
    }

    #[test]
    fn only_bwrap_trampoline_takes_a_scoped_call() {
        assert_eq!(scoped_backend_refusal(BackendKind::BwrapTrampoline), None);
        for kind in [
            BackendKind::Bwrap,
            BackendKind::LandlockTrampoline,
            BackendKind::None,
        ] {
            assert!(
                scoped_backend_refusal(kind).is_some_and(|reason| reason.contains(kind.as_str())),
                "{kind}"
            );
        }
    }

    #[test]
    fn the_fallback_is_logged_once_per_session() {
        let notice = FallbackNotice::default();
        assert!(notice.note("s1", "shell.exec", "why"));
        assert!(!notice.note("s1", "job.start", "why"));
        assert!(notice.note("s2", "shell.exec", "why"));
    }
}
