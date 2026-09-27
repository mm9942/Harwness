//! `DeriveRequirements` (PL-90): what the compiled agent needs from the
//! machine that executes it.
//!
//! # Derivation
//! From the pruned, catalog-classified permission manifest (`RightsCheck`
//! and `PruneUnusedTools` re-classify it from the capability catalog, so
//! `shell`/`host`/`filesystem`/`network` reflect the [`CapabilityClass`] of
//! every admitted tool) plus `[limits]`/`[spawn.budget]`:
//!
//! | Field | Rule |
//! |---|---|
//! | `targets` | the build target (`--target`, else the host) |
//! | `process_exec` | a `Shell`-class tool is admitted (`shell.exec`, `job.start`, `latex.*`, `agents.build`, `process.*`) |
//! | `host_access` | a `Host`-class tool is admitted (`host.sudo_exec`) |
//! | `network` | manifest network off → `none`; on without hosts → `proxy_only`; on with `[network] hosts` → `hosts` |
//! | `filesystem_write` | a `Write`- or `WriteOther`-class tool is admitted |
//! | `sandbox` | see [`sandbox_levels`] |
//! | `resources.wall_timeout_secs` | min(`[limits] max_wall_time_seconds`, `[spawn.budget] max_wall_secs`) |
//! | `resources.memory_max_bytes`, `pids_max` | none (no DSL key yet) |
//! | `kernel.landlock` | `false` (see below) |
//! | `kernel.cgroup_v2` | a memory or pids limit is set |
//! | `kernel.user_namespaces`, `dod_ebpf` | `false` (no source yet) |
//!
//! `kernel.landlock` is deliberately *not* derived from `process_exec`:
//! which backend confines a started process (the Landlock trampoline or
//! bwrap) is the runtime's choice, and the `sandbox` levels already say what
//! must be enforced. The field stays for a definition that explicitly
//! demands Landlock (no DSL syntax for that yet); admission then requires a
//! usable Landlock ABI.
//!
//! Children are unioned into the parent by `ChildClosure`
//! ([`ExecutionRequirements::union_child`]).
//!
//! [`CapabilityClass`]: harw_registry_defaults::capability_catalog::CapabilityClass

use harw_agent_dsl::Diagnostics;
use harw_agent_dsl::ir_v2::{
    Budget, ExecutionRequirements, KernelRequirements, Limits, NetworkMode, NetworkRequirement,
    Permissions, RequirementLevel, ResourceRequirements, SandboxRequirementLevels, TargetSpec,
};

use super::Pass;
use crate::unit::CompileUnit;

/// Fills `ir.requirements` from the manifest (see the module docs).
#[derive(Debug, Clone, Copy)]
pub struct DeriveRequirements<'a> {
    /// The build target triple (`--target`, else the host).
    pub target: &'a str,
}

impl Pass for DeriveRequirements<'_> {
    fn name(&self) -> &'static str {
        "derive-requirements"
    }

    fn run(&self, unit: &mut CompileUnit) -> Diagnostics {
        unit.ir.requirements = derive_requirements(
            &unit.ir.permissions,
            unit.ir.limits.as_ref(),
            unit.ir.spawn.budget.as_ref(),
            Some(self.target),
        );
        Diagnostics::new()
    }
}

/// Derives the execution requirements (module docs, "Derivation").
///
/// # Description
/// `target` is a rustc triple; `None` or an unparsable triple leaves
/// `targets` empty (any target).
#[must_use]
pub fn derive_requirements(
    permissions: &Permissions,
    limits: Option<&Limits>,
    budget: Option<&Budget>,
    target: Option<&str>,
) -> ExecutionRequirements {
    let process_exec = permissions.shell;
    let host_access = permissions.host;
    let filesystem_write =
        permissions.filesystem.write || !permissions.filesystem.other_write_tools.is_empty();
    let network = match permissions.network.mode {
        NetworkMode::Off => NetworkRequirement::None,
        NetworkMode::Allowlist if permissions.network.hosts.is_empty() => {
            NetworkRequirement::ProxyOnly
        }
        NetworkMode::Allowlist => {
            let mut hosts = permissions.network.hosts.clone();
            hosts.sort();
            hosts.dedup();
            NetworkRequirement::Hosts(hosts)
        }
    };
    let resources = ResourceRequirements {
        memory_max_bytes: None,
        pids_max: None,
        wall_timeout_secs: min_option(
            limits.and_then(|limits| limits.max_wall_time_seconds),
            budget.and_then(|budget| budget.max_wall_secs),
        ),
    };
    let sandbox = sandbox_levels(process_exec, host_access, filesystem_write, &network);
    ExecutionRequirements {
        targets: target
            .and_then(TargetSpec::from_triple)
            .into_iter()
            .collect(),
        process_exec,
        host_access,
        network,
        filesystem_write,
        sandbox,
        resources,
        kernel: KernelRequirements {
            // The sandbox backend is the runtime's choice; only an explicit
            // demand (no DSL syntax yet) sets this (module docs).
            landlock: false,
            cgroup_v2: resources.needs_cgroup(),
            user_namespaces: false,
        },
        dod_ebpf: false,
    }
}

/// The sandbox levels.
///
/// # Description
/// - **Host access** (a tool acts outside the sandbox, e.g. via `sudo`):
///   the sandbox cannot confine it, and `no_new_privs`/dropped capabilities
///   would break privilege escalation — both `not_needed`; filesystem and
///   network `best_effort` (for the other tools), resource limits
///   `best_effort` if a process tool is admitted.
/// - **Process execution** without host access: filesystem,
///   `no_new_privs` and capabilities `required`; network `required` if the
///   agent needs no network, else `best_effort`; resource limits
///   `best_effort`.
/// - **No process execution**: everything `not_needed`, except filesystem
///   `best_effort` when a write tool is admitted (writes happen in-process
///   through path-checked tools; a filesystem sandbox is defense in depth).
#[must_use]
pub fn sandbox_levels(
    process_exec: bool,
    host_access: bool,
    filesystem_write: bool,
    network: &NetworkRequirement,
) -> SandboxRequirementLevels {
    use RequirementLevel::{BestEffort, NotNeeded, Required};
    if host_access {
        return SandboxRequirementLevels {
            filesystem: BestEffort,
            network: BestEffort,
            no_new_privs: NotNeeded,
            capabilities: NotNeeded,
            resource_limits: if process_exec { BestEffort } else { NotNeeded },
        };
    }
    if process_exec {
        return SandboxRequirementLevels {
            filesystem: Required,
            network: if *network == NetworkRequirement::None {
                Required
            } else {
                BestEffort
            },
            no_new_privs: Required,
            capabilities: Required,
            resource_limits: BestEffort,
        };
    }
    SandboxRequirementLevels {
        filesystem: if filesystem_write {
            BestEffort
        } else {
            NotNeeded
        },
        ..SandboxRequirementLevels::default()
    }
}

/// Minimum of two optional upper bounds; a missing side is no bound.
fn min_option(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use harw_agent_dsl::classify::reclassify;
    use harw_agent_dsl::ir_v2::{FilesystemPermissions, NetworkPermissions, SpawnPermissions};
    use harw_registry_defaults::capability_catalog::CapabilityCatalog;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A manifest admitting exactly `tools`, classified from the catalog.
    fn manifest(tools: &[&str], hosts: &[&str]) -> Result<Permissions, String> {
        let mut permissions = Permissions {
            tools: Vec::new(),
            forbidden_tools: Vec::new(),
            capabilities: Vec::new(),
            filesystem: FilesystemPermissions::default(),
            network: NetworkPermissions {
                mode: NetworkMode::Off,
                tools: Vec::new(),
                hosts: hosts.iter().map(|host| (*host).to_owned()).collect(),
            },
            shell: false,
            host: false,
            spawn: SpawnPermissions::default(),
            budget: None,
            required_env: Vec::new(),
        };
        let effective: Vec<String> = tools.iter().map(|tool| (*tool).to_owned()).collect();
        let unknown = reclassify(&mut permissions, &effective, None, &CapabilityCatalog);
        if !unknown.is_empty() {
            return Err(format!("unknown tools: {unknown:?}"));
        }
        Ok(permissions)
    }

    fn derive(tools: &[&str], hosts: &[&str]) -> Result<ExecutionRequirements, String> {
        Ok(derive_requirements(
            &manifest(tools, hosts)?,
            None,
            None,
            Some("x86_64-unknown-linux-gnu"),
        ))
    }

    #[test]
    fn test_read_only_agent_needs_no_process_and_no_sandbox() -> TestResult {
        let requirements = derive(&["fs.read"], &[])?;
        assert!(!requirements.process_exec);
        assert!(!requirements.host_access);
        assert!(!requirements.filesystem_write);
        assert_eq!(requirements.network, NetworkRequirement::None);
        assert_eq!(requirements.sandbox, SandboxRequirementLevels::default());
        assert_eq!(requirements.kernel, KernelRequirements::default());
        assert_eq!(
            requirements.targets,
            [TargetSpec {
                os: "linux".to_owned(),
                arch: Some("x86_64".to_owned()),
            }]
        );
        Ok(())
    }

    #[test]
    fn test_writing_agent_asks_for_a_best_effort_filesystem_sandbox() -> TestResult {
        let requirements = derive(&["fs.read", "fs.write"], &[])?;
        assert!(requirements.filesystem_write && !requirements.process_exec);
        assert_eq!(
            requirements.sandbox.filesystem,
            RequirementLevel::BestEffort
        );
        assert_eq!(
            requirements.sandbox.no_new_privs,
            RequirementLevel::NotNeeded
        );
        assert!(!requirements.kernel.landlock);
        Ok(())
    }

    #[test]
    fn test_shell_agent_requires_the_sandbox_but_not_landlock() -> TestResult {
        let requirements = derive(&["fs.read", "shell.exec"], &[])?;
        assert!(requirements.process_exec && !requirements.host_access);
        let sandbox = requirements.sandbox;
        assert_eq!(sandbox.filesystem, RequirementLevel::Required);
        assert_eq!(sandbox.no_new_privs, RequirementLevel::Required);
        assert_eq!(sandbox.capabilities, RequirementLevel::Required);
        assert_eq!(
            sandbox.network,
            RequirementLevel::Required,
            "no network needed → network confinement required"
        );
        assert_eq!(sandbox.resource_limits, RequirementLevel::BestEffort);
        assert!(
            !requirements.kernel.landlock,
            "the backend (Landlock or bwrap) is the runtime's choice"
        );
        assert!(!requirements.kernel.cgroup_v2);
        Ok(())
    }

    #[test]
    fn test_every_shell_class_tool_means_process_exec() -> TestResult {
        for tool in ["job.start", "latex.build", "agents.build", "process.list"] {
            assert!(derive(&[tool], &[])?.process_exec, "{tool}");
        }
        Ok(())
    }

    #[test]
    fn test_host_agent_has_host_access_and_no_landlock() -> TestResult {
        let requirements = derive(&["host.sudo_exec"], &[])?;
        assert!(requirements.host_access);
        assert!(!requirements.kernel.landlock);
        assert_eq!(
            requirements.sandbox.no_new_privs,
            RequirementLevel::NotNeeded
        );
        assert_eq!(
            requirements.sandbox.capabilities,
            RequirementLevel::NotNeeded
        );
        let both = derive(&["host.sudo_exec", "shell.exec"], &[])?;
        assert!(both.process_exec && both.host_access);
        assert!(!both.kernel.landlock);
        assert_eq!(both.sandbox.filesystem, RequirementLevel::BestEffort);
        Ok(())
    }

    #[test]
    fn test_network_follows_the_manifest() -> TestResult {
        let proxied = derive(&["web.fetch"], &[])?;
        assert_eq!(proxied.network, NetworkRequirement::ProxyOnly);
        let hosts = derive(&["web.fetch"], &["docs.rs", "crates.io"])?;
        assert_eq!(
            hosts.network,
            NetworkRequirement::Hosts(vec!["crates.io".to_owned(), "docs.rs".to_owned()])
        );
        let shell_with_net = derive(&["shell.exec", "web.fetch"], &[])?;
        assert_eq!(shell_with_net.sandbox.network, RequirementLevel::BestEffort);
        Ok(())
    }

    #[test]
    fn test_wall_timeout_is_the_lower_of_limits_and_budget() -> TestResult {
        let limits = Limits {
            max_wall_time_seconds: Some(600),
            ..Limits::default()
        };
        let budget = Budget {
            max_wall_secs: Some(300),
            ..Budget::default()
        };
        let requirements = derive_requirements(
            &manifest(&["fs.read"], &[])?,
            Some(&limits),
            Some(&budget),
            None,
        );
        assert_eq!(requirements.resources.wall_timeout_secs, Some(300));
        assert!(!requirements.kernel.cgroup_v2, "a timer needs no cgroup");
        assert!(requirements.targets.is_empty(), "no target → any");
        Ok(())
    }

    #[test]
    fn test_child_union_carries_the_shell_child_into_a_read_only_parent() -> TestResult {
        let parent = derive(&["fs.read"], &[])?;
        let child = derive(&["shell.exec"], &[])?;
        let union = parent.union_child(&child);
        assert!(union.process_exec);
        assert_eq!(union.sandbox.filesystem, RequirementLevel::Required);
        assert_eq!(union.sandbox.no_new_privs, RequirementLevel::Required);
        assert!(
            !union.kernel.landlock,
            "no child demands Landlock explicitly"
        );
        assert_eq!(union.targets, parent.targets);
        Ok(())
    }
}
