//! Linux Bubblewrap launch backend.
//!
//! The core only carries a [`SandboxSpec`](crate::SandboxSpec); this module is
//! the syscall-adjacent consumer that turns it into a minimal `bwrap` command.
//! It never mounts a host home, parent workspace, or arbitrary environment.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command};

use crate::{Permission, SandboxError, SandboxResult, SandboxSpec};

/// A fully determined Bubblewrap invocation. Keeping it inspectable makes
/// policy tests possible without launching a process on the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BwrapCommandPlan {
    args: Vec<OsString>,
}

impl BwrapCommandPlan {
    #[must_use]
    pub fn args(&self) -> &[OsString] {
        &self.args
    }
}

/// Builds and launches process sandboxes for tools and stdio MCP servers.
#[derive(Debug, Clone)]
pub struct BwrapLauncher {
    executable: PathBuf,
}

impl Default for BwrapLauncher {
    fn default() -> Self {
        Self::new(PathBuf::from("bwrap"))
    }
}

impl BwrapLauncher {
    #[must_use]
    pub fn new(executable: PathBuf) -> Self {
        Self { executable }
    }

    /// Produces a hermetic command plan. `command` must include the program as
    /// its first item and is only allowed when the sandbox granted process
    /// execution. Network stays unshared unless specifically authorized.
    pub fn plan(
        &self,
        sandbox: &SandboxSpec,
        command: &[OsString],
    ) -> SandboxResult<BwrapCommandPlan> {
        if !sandbox.permissions().contains(Permission::ExecuteProcess) {
            return Err(SandboxError::ProcessExecutionDenied);
        }
        if command.is_empty() {
            return Err(SandboxError::MissingSandboxCommand);
        }
        let workspace = sandbox.workspace().canonical_root();
        let mut args = vec![
            OsString::from("--die-with-parent"),
            OsString::from("--new-session"),
            OsString::from("--unshare-all"),
        ];
        if sandbox.permissions().contains(Permission::NetworkAccess) {
            args.push(OsString::from("--share-net"));
        }
        args.extend([
            OsString::from("--clearenv"),
            OsString::from("--proc"),
            OsString::from("/proc"),
        ]);
        args.extend([OsString::from("--dev"), OsString::from("/dev")]);
        args.extend([OsString::from("--tmpfs"), OsString::from("/tmp")]);
        args.extend([OsString::from("--dir"), OsString::from("/tmp/home")]);
        args.extend([
            OsString::from("--setenv"),
            OsString::from("HOME"),
            OsString::from("/tmp/home"),
            OsString::from("--setenv"),
            OsString::from("PATH"),
            OsString::from("/usr/local/bin:/usr/bin:/bin"),
        ]);

        for directory in ["/usr", "/bin", "/lib", "/lib64"] {
            let path = Path::new(directory);
            if path.exists() {
                args.extend([
                    OsString::from("--ro-bind"),
                    path.as_os_str().to_owned(),
                    path.as_os_str().to_owned(),
                ]);
            }
        }
        append_destination_dirs(&mut args, workspace)?;
        args.push(
            if sandbox.permissions().contains(Permission::WriteWorkspace) {
                OsString::from("--bind")
            } else {
                OsString::from("--ro-bind")
            },
        );
        args.push(workspace.as_os_str().to_owned());
        args.push(workspace.as_os_str().to_owned());
        args.extend([OsString::from("--chdir"), workspace.as_os_str().to_owned()]);
        args.push(OsString::from("--"));
        args.extend(command.iter().cloned());
        Ok(BwrapCommandPlan { args })
    }

    /// Executes a previously checked command through Bubblewrap. The plan is
    /// deliberately built first so callers can audit/log its non-secret mount
    /// shape before process creation.
    pub fn spawn(&self, plan: &BwrapCommandPlan) -> SandboxResult<Child> {
        Command::new(&self.executable)
            .args(&plan.args)
            .spawn()
            .map_err(|error| SandboxError::SandboxProcessSpawn {
                executable: self.executable.clone(),
                reason: error.to_string(),
            })
    }
}

fn append_destination_dirs(args: &mut Vec<OsString>, destination: &Path) -> SandboxResult<()> {
    if !destination.is_absolute() {
        return Err(SandboxError::InvalidSandboxWorkspaceDestination {
            path: destination.to_path_buf(),
        });
    }
    let mut current = PathBuf::new();
    for component in destination.components() {
        match component {
            Component::RootDir => current.push(component.as_os_str()),
            Component::Normal(part) => {
                current.push(part);
                args.extend([OsString::from("--dir"), current.as_os_str().to_owned()]);
            }
            _ => {
                return Err(SandboxError::InvalidSandboxWorkspaceDestination {
                    path: destination.to_path_buf(),
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PermissionSet, WorkspaceRegistration, WorkspaceRegistry};
    use harw_types::{TenantId, WorkspaceId};

    fn sandbox(permissions: PermissionSet) -> SandboxSpec {
        let root = std::env::temp_dir().join(format!("harwness-bwrap-{}", std::process::id()));
        std::fs::create_dir_all(root.join("workspace")).unwrap();
        let registry = WorkspaceRegistry::build(
            &root,
            [WorkspaceRegistration {
                tenant: TenantId::from_str("tenant"),
                workspace: WorkspaceId::from_str("workspace"),
                root: PathBuf::from("workspace"),
            }],
        )
        .unwrap();
        SandboxSpec::from_resolved(
            registry
                .resolve(
                    &TenantId::from_str("tenant"),
                    &WorkspaceId::from_str("workspace"),
                )
                .unwrap(),
            permissions,
        )
    }

    fn strings(plan: &BwrapCommandPlan) -> Vec<String> {
        plan.args()
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn readonly_plan_has_no_network_or_workspace_write_mount() {
        let plan = BwrapLauncher::default()
            .plan(
                &sandbox(PermissionSet::from_policy([
                    Permission::ReadWorkspace,
                    Permission::ExecuteProcess,
                ])),
                &[OsString::from("/bin/true")],
            )
            .unwrap();
        let args = strings(&plan);
        assert!(args.contains(&"--unshare-all".to_owned()));
        assert!(!args.contains(&"--share-net".to_owned()));
        assert!(args.contains(&"--ro-bind".to_owned()));
    }

    #[test]
    fn writable_networked_plan_requires_each_explicit_permission() {
        let plan = BwrapLauncher::default()
            .plan(
                &sandbox(PermissionSet::from_policy([
                    Permission::ReadWorkspace,
                    Permission::WriteWorkspace,
                    Permission::ExecuteProcess,
                    Permission::NetworkAccess,
                ])),
                &[OsString::from("/bin/true")],
            )
            .unwrap();
        let args = strings(&plan);
        assert!(args.contains(&"--share-net".to_owned()));
        assert!(args.contains(&"--bind".to_owned()));
    }

    #[test]
    fn process_start_is_denied_without_execute_permission() {
        let error = BwrapLauncher::default()
            .plan(
                &sandbox(PermissionSet::from_policy([Permission::ReadWorkspace])),
                &[OsString::from("/bin/true")],
            )
            .unwrap_err();
        assert!(matches!(error, SandboxError::ProcessExecutionDenied));
    }
}
