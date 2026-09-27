//! Honest per-dimension sandbox reports for Darwin.
//!
//! | Dimension         | Unsandboxed   | `sandbox-exec` (SBPL)           |
//! |-------------------|---------------|---------------------------------|
//! | `filesystem`      | `NotEnforced` | `Partial`                       |
//! | `network`         | `NotEnforced` | `Partial`                       |
//! | `no_new_privs`    | `Unsupported` | `Unsupported`                   |
//! | `capabilities`    | `Unsupported` | `Unsupported`                   |
//! | `resource_limits` | `NotEnforced` | `NotEnforced`                   |
//!
//! Why:
//! - `sandbox-exec` is deprecated and undocumented; the generated profile has
//!   coarse allowances (`mach-lookup`, metadata reads everywhere, system
//!   paths) and `NetworkRestricted` cannot express an allowlist. That is
//!   real, kernel-enforced restriction, but not "fully as requested" →
//!   `Partial`, never `Enforced`.
//! - `no_new_privs` and Linux capability sets do not exist on Darwin. They
//!   are `Unsupported`, not emulated under another name (Eco-Doc §37).
//! - rlimits need a trampoline in the job process (see [`crate::rlimit`]);
//!   until one runs, resource limits are `NotEnforced`. Wall-clock timeouts
//!   are the supervisor's job ([`crate::TerminationPolicy`]).
//!
//! A [`harw_job_core::SandboxRequirement::Required`] job therefore never
//! passes on Darwin — by design.

use std::path::{Path, PathBuf};
use std::process::Command;

use harw_job_core::{EnforcementState, SandboxProfileName, SandboxReport};

use crate::error::SandboxError;
use crate::sbpl::{copy_env_and_cwd, sbpl_profile_for, wrap_with_sandbox_exec};

/// How a Darwin job is (or is not) sandboxed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DarwinSandboxMode {
    /// The job runs without a sandbox.
    Unsandboxed,
    /// The job runs under `/usr/bin/sandbox-exec` with a generated SBPL
    /// profile scoped to `workspace_root`.
    SandboxExec {
        /// Workspace root (canonicalized when wrapping).
        workspace_root: PathBuf,
    },
}

/// Darwin sandbox selection plus its honest report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DarwinSandbox {
    mode: DarwinSandboxMode,
}

impl DarwinSandbox {
    /// No sandbox.
    #[must_use]
    pub const fn unsandboxed() -> Self {
        Self {
            mode: DarwinSandboxMode::Unsandboxed,
        }
    }

    /// `sandbox-exec` with a profile scoped to `workspace_root`.
    #[must_use]
    pub fn sandbox_exec(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            mode: DarwinSandboxMode::SandboxExec {
                workspace_root: workspace_root.into(),
            },
        }
    }

    /// The selected mode.
    #[must_use]
    pub const fn mode(&self) -> &DarwinSandboxMode {
        &self.mode
    }

    /// What this mode enforces for `profile` (see the module table).
    #[must_use]
    pub fn report_for(&self, _profile: &SandboxProfileName) -> SandboxReport {
        // Every profile maps to the same states today; the profile stays in
        // the signature because the SBPL content (and a future verdict) is
        // per profile.
        let (filesystem, network) = match self.mode {
            DarwinSandboxMode::Unsandboxed => {
                (EnforcementState::NotEnforced, EnforcementState::NotEnforced)
            }
            DarwinSandboxMode::SandboxExec { .. } => {
                (EnforcementState::Partial, EnforcementState::Partial)
            }
        };
        SandboxReport {
            filesystem,
            network,
            no_new_privs: EnforcementState::Unsupported,
            capabilities: EnforcementState::Unsupported,
            resource_limits: EnforcementState::NotEnforced,
        }
    }

    /// The command to spawn for `command` under this mode: for
    /// [`DarwinSandboxMode::SandboxExec`] the `sandbox-exec` wrapper with a
    /// profile for the canonicalized workspace root; for
    /// [`DarwinSandboxMode::Unsandboxed`] a copy of `command` (program, args,
    /// env changes, cwd — the same fields the wrapper copies).
    ///
    /// # Errors
    /// [`SandboxError::Canonicalize`] if the workspace root cannot be
    /// resolved, plus the errors of [`sbpl_profile_for`] and
    /// [`wrap_with_sandbox_exec`].
    pub fn wrap(
        &self,
        profile: &SandboxProfileName,
        command: &Command,
    ) -> Result<Command, SandboxError> {
        match &self.mode {
            DarwinSandboxMode::Unsandboxed => Ok(copy_command(command)),
            DarwinSandboxMode::SandboxExec { workspace_root } => {
                let root = canonical_root(workspace_root)?;
                let text = sbpl_profile_for(profile, &root)?;
                wrap_with_sandbox_exec(command, &text)
            }
        }
    }
}

fn canonical_root(path: &Path) -> Result<PathBuf, SandboxError> {
    std::fs::canonicalize(path).map_err(|source| SandboxError::Canonicalize {
        path: path.to_path_buf(),
        source,
    })
}

fn copy_command(command: &Command) -> Command {
    let mut copy = Command::new(command.get_program());
    copy.args(command.get_args());
    copy_env_and_cwd(command, &mut copy);
    copy
}

#[cfg(test)]
mod tests {
    use super::{DarwinSandbox, DarwinSandboxMode};
    use crate::error::SandboxError;
    use crate::sbpl::SANDBOX_EXEC_PATH;
    use crate::test_support::{TestError, TestResult, ctx};
    use harw_job_core::{
        EnforcementState as S, SandboxProfileName, SandboxReport, SandboxRequirement,
    };
    use std::ffi::OsStr;
    use std::process::Command;

    const ALL: [SandboxProfileName; 4] = [
        SandboxProfileName::WorkspaceBuild,
        SandboxProfileName::ReadOnlyAnalysis,
        SandboxProfileName::NoNetwork,
        SandboxProfileName::NetworkRestricted,
    ];

    #[test]
    fn test_unsandboxed_report_claims_nothing() {
        for profile in ALL {
            let report = DarwinSandbox::unsandboxed().report_for(&profile);
            assert_eq!(
                report,
                SandboxReport {
                    filesystem: S::NotEnforced,
                    network: S::NotEnforced,
                    no_new_privs: S::Unsupported,
                    capabilities: S::Unsupported,
                    resource_limits: S::NotEnforced,
                }
            );
            assert_eq!(report.overall(), S::NotEnforced);
        }
    }

    #[test]
    fn test_sandbox_exec_report_is_partial_and_never_satisfies_required() {
        let sandbox = DarwinSandbox::sandbox_exec("/ws");
        for profile in ALL {
            let report = sandbox.report_for(&profile);
            assert_eq!(report.filesystem, S::Partial);
            assert_eq!(report.network, S::Partial);
            assert_eq!(report.no_new_privs, S::Unsupported);
            assert_eq!(report.capabilities, S::Unsupported);
            assert_eq!(report.resource_limits, S::NotEnforced);
            assert_eq!(report.overall(), S::Partial);
            assert!(!report.satisfies(SandboxRequirement::Required));
            assert!(report.satisfies(SandboxRequirement::BestEffort));
        }
    }

    #[test]
    fn test_wrap_uses_canonical_workspace_and_sandbox_exec() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let canonical = std::fs::canonicalize(dir.path()).map_err(ctx("canonicalize"))?;
        let sandbox = DarwinSandbox::sandbox_exec(dir.path());
        assert!(matches!(
            sandbox.mode(),
            DarwinSandboxMode::SandboxExec { .. }
        ));
        let mut command = Command::new("/usr/bin/true");
        command.arg("x");
        let wrapped = sandbox
            .wrap(&SandboxProfileName::NoNetwork, &command)
            .map_err(ctx("wrap"))?;
        assert_eq!(wrapped.get_program(), OsStr::new(SANDBOX_EXEC_PATH));
        let args: Vec<&OsStr> = wrapped.get_args().collect();
        let [flag, profile, program, arg] = args.as_slice() else {
            return Err(TestError::Unexpected(format!("argv {args:?}")));
        };
        assert_eq!(*flag, "-p");
        assert_eq!(*program, "/usr/bin/true");
        assert_eq!(*arg, "x");
        let profile = profile
            .to_str()
            .ok_or(TestError::Missing("utf-8 profile"))?;
        let canonical = canonical.to_str().ok_or(TestError::Missing("utf-8 dir"))?;
        assert!(
            profile.contains(&format!("(subpath \"{canonical}\")")),
            "{profile}"
        );

        let unsandboxed = DarwinSandbox::unsandboxed()
            .wrap(&SandboxProfileName::NoNetwork, &command)
            .map_err(ctx("copy"))?;
        assert_eq!(unsandboxed.get_program(), OsStr::new("/usr/bin/true"));

        let missing = DarwinSandbox::sandbox_exec(dir.path().join("does-not-exist"));
        assert!(matches!(
            missing.wrap(&SandboxProfileName::NoNetwork, &command),
            Err(SandboxError::Canonicalize { .. })
        ));
        Ok(())
    }
}
