//! Runtime admission (PL-90): can this host run the embedded agent with the
//! guarantees its compiled [`ExecutionRequirements`] ask for?
//!
//! # Description
//! [`check`] compares the root agent's requirements (which already union
//! every embedded child, `ChildClosure`) with a [`HostReport`]
//! ([`HostReport::probe`], taken once per start) and either admits the
//! agent — possibly *degraded* — or refuses it with every unmet item.
//!
//! | Requirement | Admitted when | Unmet |
//! |---|---|---|
//! | `targets` | empty (any), or one entry matches the host's OS (and arch, if named) | refused |
//! | sandbox level `required` | host state `enforced` or `partial` | refused, never overridable (`--allow-degraded` does not help) |
//! | sandbox level `best_effort` | host state `enforced` or `partial` | refused, or degraded with `--allow-degraded` |
//! | sandbox level `not_needed` | always | — |
//! | `kernel.landlock` | Landlock usable (ABI >= 1) | refused, never overridable |
//! | `kernel.cgroup_v2` | cgroup v2 root delegated | refused, or degraded with `--allow-degraded` |
//! | `kernel.user_namespaces` | unprivileged user namespaces known available | refused, or degraded with `--allow-degraded` |
//! | `dod_ebpf` | never (the runner has no DoD eBPF monitor) | refused, never overridable |
//!
//! `required` means the dimension must actually be enforced by some backend
//! — fully or partially (e.g. bwrap's mount isolation, which cannot separate
//! read from execute and so reports the filesystem as `partial`). A host
//! state of `not_enforced` or `unsupported` refuses it, fail-closed. The
//! difference to `best_effort` is only the override: a `best_effort` gap may
//! be accepted degraded, a `required` one never. A stricter "Strict =
//! `enforced` only" level may come later.
//!
//! The host side is the strongest state *any* backend reaches
//! ([`HostReport::best_report`]): an upper bound, which is exactly what a
//! start-up gate can know before a job runs.
//!
//! `--requirements` ([`run_requirements`]) prints requirements, host report
//! and verdict without starting anything.

use std::fmt;
use std::process::ExitCode;

use harw_agent_artifact::{Artifact, Bundle};
use harw_agent_dsl::ir_v2::{ExecutionRequirements, RequirementLevel};
use harw_job_runtime::{EnforcementState, HostReport};
use serde::Serialize;

use crate::error::{EXIT_ADMISSION, RunnerError};

/// One requirement the host does not (fully) meet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdmissionFinding {
    /// Which requirement: `target`, `sandbox.<dimension>`,
    /// `kernel.<feature>` or `dod_ebpf`.
    pub item: String,
    /// What the agent asks for (a level, a target list, `true`).
    pub required: String,
    /// What this host offers.
    pub available: String,
    /// `true` if `--allow-degraded` may accept the gap (a best-effort
    /// item); `false` for a hard requirement.
    pub overridable: bool,
    /// One human-readable line.
    pub message: String,
}

impl fmt::Display for AdmissionFinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// A successful admission: the agent may start.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct AdmissionReport {
    /// Best-effort items the host cannot meet, accepted because
    /// `--allow-degraded` was given. Empty for a full admission.
    pub degraded: Vec<AdmissionFinding>,
}

impl AdmissionReport {
    /// Whether any item was accepted degraded.
    #[must_use]
    pub fn is_degraded(&self) -> bool {
        !self.degraded.is_empty()
    }
}

/// A refused admission: every unmet item, hard and best-effort alike.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AdmissionRefused {
    /// Every item the host does not meet, in check order (target, sandbox
    /// dimensions, kernel features, DoD eBPF).
    pub missing: Vec<AdmissionFinding>,
    /// Whether `--allow-degraded` was given (then only the
    /// non-overridable items caused the refusal).
    pub allow_degraded: bool,
}

impl fmt::Display for AdmissionRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "admission refused: this host cannot run the agent with its required guarantees: ",
        )?;
        let lines: Vec<String> = self
            .missing
            .iter()
            .map(|finding| {
                if finding.overridable && self.allow_degraded {
                    format!("{} (accepted by --allow-degraded)", finding.message)
                } else {
                    finding.message.clone()
                }
            })
            .collect();
        f.write_str(&lines.join("; "))?;
        let blocking_soft =
            !self.allow_degraded && self.missing.iter().any(|item| item.overridable);
        let hard = self.missing.iter().any(|item| !item.overridable);
        if blocking_soft && !hard {
            f.write_str(" — pass --allow-degraded to run without the best-effort guarantees")?;
        }
        Ok(())
    }
}

impl std::error::Error for AdmissionRefused {}

/// Checks `req` against `host` (see the module docs for the table).
///
/// # Errors
/// [`AdmissionRefused`] with every unmet item if a hard requirement is
/// unmet, or a best-effort one is unmet and `allow_degraded` is `false`.
pub fn check(
    req: &ExecutionRequirements,
    host: &HostReport,
    allow_degraded: bool,
) -> Result<AdmissionReport, AdmissionRefused> {
    let missing = findings(req, host);
    let refused = missing
        .iter()
        .any(|finding| !finding.overridable || !allow_degraded);
    if refused {
        Err(AdmissionRefused {
            missing,
            allow_degraded,
        })
    } else {
        Ok(AdmissionReport { degraded: missing })
    }
}

/// Every unmet item, hard and soft, in check order.
fn findings(req: &ExecutionRequirements, host: &HostReport) -> Vec<AdmissionFinding> {
    let mut out = Vec::new();

    if !req.targets.is_empty() {
        let matches = req.targets.iter().any(|target| {
            target.os == host.target_os
                && target
                    .arch
                    .as_ref()
                    .is_none_or(|arch| *arch == host.target_arch)
        });
        if !matches {
            let wanted = req
                .targets
                .iter()
                .map(harw_agent_dsl::ir_v2::TargetSpec::label)
                .collect::<Vec<_>>()
                .join(", ");
            let here = format!("{}/{}", host.target_os, host.target_arch);
            out.push(AdmissionFinding {
                item: "target".to_owned(),
                message: format!(
                    "target: the agent was compiled for {wanted}, this host is {here}"
                ),
                required: wanted,
                available: here,
                overridable: false,
            });
        }
    }

    let best = &host.best_report;
    let dimensions = [
        ("filesystem", req.sandbox.filesystem, best.filesystem),
        ("network", req.sandbox.network, best.network),
        ("no_new_privs", req.sandbox.no_new_privs, best.no_new_privs),
        ("capabilities", req.sandbox.capabilities, best.capabilities),
        (
            "resource_limits",
            req.sandbox.resource_limits,
            best.resource_limits,
        ),
    ];
    for (name, level, state) in dimensions {
        if let Some(finding) = sandbox_finding(name, level, state) {
            out.push(finding);
        }
    }

    if req.kernel.landlock && host.landlock.usable_abi().is_none() {
        let available = format!(
            "abi {}, lsm {}",
            host.landlock
                .abi
                .map_or_else(|| "unknown".to_owned(), |abi| abi.to_string()),
            match host.landlock.lsm_active {
                Some(true) => "active",
                Some(false) => "inactive",
                None => "unknown",
            }
        );
        out.push(AdmissionFinding {
            item: "kernel.landlock".to_owned(),
            required: "true".to_owned(),
            message: format!(
                "kernel landlock: required, but Landlock is not usable on this host ({available})"
            ),
            available,
            overridable: false,
        });
    }
    if req.kernel.cgroup_v2 && !host.cgroup_v2_delegated {
        out.push(AdmissionFinding {
            item: "kernel.cgroup_v2".to_owned(),
            required: "true".to_owned(),
            available: "not delegated".to_owned(),
            overridable: true,
            message: "kernel cgroup_v2: memory/pids limits need a delegated cgroup v2 root, \
                      this host has none (limits fall back to rlimits)"
                .to_owned(),
        });
    }
    if req.kernel.user_namespaces && host.user_namespaces != Some(true) {
        let available = match host.user_namespaces {
            Some(false) => "disabled",
            _ => "unknown",
        };
        out.push(AdmissionFinding {
            item: "kernel.user_namespaces".to_owned(),
            required: "true".to_owned(),
            available: available.to_owned(),
            overridable: true,
            message: format!(
                "kernel user_namespaces: unprivileged user namespaces are {available} on this host"
            ),
        });
    }
    if req.dod_ebpf {
        out.push(AdmissionFinding {
            item: "dod_ebpf".to_owned(),
            required: "true".to_owned(),
            available: "unsupported".to_owned(),
            overridable: false,
            message: "dod_ebpf: the agent requires the DoD eBPF monitor, which this runner does \
                      not support"
                .to_owned(),
        });
    }
    out
}

/// Whether some backend enforces the dimension at all (fully or partially).
fn enforced_at_all(state: EnforcementState) -> bool {
    matches!(
        state,
        EnforcementState::Enforced | EnforcementState::Partial
    )
}

/// One sandbox dimension: `None` if met.
fn sandbox_finding(
    name: &str,
    level: RequirementLevel,
    state: EnforcementState,
) -> Option<AdmissionFinding> {
    let (met, needs, overridable) = match level {
        RequirementLevel::NotNeeded => return None,
        RequirementLevel::Required => (enforced_at_all(state), "enforced or partial", false),
        RequirementLevel::BestEffort => (enforced_at_all(state), "enforced or partial", true),
    };
    if met {
        return None;
    }
    Some(AdmissionFinding {
        item: format!("sandbox.{name}"),
        required: level.as_str().to_owned(),
        available: state.to_string(),
        overridable,
        message: format!(
            "sandbox {name}: {}, but this host reaches only `{state}` (needs {needs})",
            level.as_str()
        ),
    })
}

/// The `--requirements` verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Verdict {
    /// Whether the agent would start.
    pub admitted: bool,
    /// Whether `--allow-degraded` was given.
    pub allow_degraded: bool,
    /// Every unmet item (hard and best-effort).
    pub missing: Vec<AdmissionFinding>,
}

impl Verdict {
    /// Evaluates [`check`] into a printable verdict.
    #[must_use]
    pub fn evaluate(req: &ExecutionRequirements, host: &HostReport, allow_degraded: bool) -> Self {
        match check(req, host, allow_degraded) {
            Ok(report) => Self {
                admitted: true,
                allow_degraded,
                missing: report.degraded,
            },
            Err(refused) => Self {
                admitted: false,
                allow_degraded,
                missing: refused.missing,
            },
        }
    }

    /// `admitted`, `admitted (degraded)` or `refused`.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match (self.admitted, self.missing.is_empty()) {
            (true, true) => "admitted",
            (true, false) => "admitted (degraded)",
            (false, _) => "refused",
        }
    }
}

/// The `--requirements --json` document.
#[derive(Debug, Serialize)]
struct RequirementsDoc<'a> {
    agent: &'a str,
    requirements: &'a ExecutionRequirements,
    host: &'a HostReport,
    verdict: &'a Verdict,
}

/// Renders `--requirements --json` as pretty JSON; `agent` is the root
/// agent's id.
///
/// # Errors
/// [`RunnerError::Json`] if encoding fails (never in practice).
pub fn render_json(
    agent: &str,
    req: &ExecutionRequirements,
    host: &HostReport,
    verdict: &Verdict,
) -> Result<String, RunnerError> {
    let doc = RequirementsDoc {
        agent,
        requirements: req,
        host,
        verdict,
    };
    Ok(serde_json::to_string_pretty(&doc)?)
}

fn opt(value: Option<u64>) -> String {
    value.map_or_else(|| "-".to_owned(), |value| value.to_string())
}

/// Renders `--requirements` as human-readable text; `agent` is the root
/// agent's label (`id (specialization)`).
#[must_use]
pub fn render_text(
    agent: &str,
    req: &ExecutionRequirements,
    host: &HostReport,
    verdict: &Verdict,
) -> String {
    let targets = if req.targets.is_empty() {
        "any".to_owned()
    } else {
        req.targets
            .iter()
            .map(harw_agent_dsl::ir_v2::TargetSpec::label)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let best = &host.best_report;
    let mut lines = vec![
        format!("agent: {agent}"),
        "requirements:".to_owned(),
        format!("  targets: {targets}"),
        format!(
            "  process_exec: {} host_access: {} filesystem_write: {}",
            req.process_exec, req.host_access, req.filesystem_write
        ),
        format!("  network: {}", req.network.label()),
        format!(
            "  sandbox: filesystem={} network={} no_new_privs={} capabilities={} resource_limits={}",
            req.sandbox.filesystem.as_str(),
            req.sandbox.network.as_str(),
            req.sandbox.no_new_privs.as_str(),
            req.sandbox.capabilities.as_str(),
            req.sandbox.resource_limits.as_str()
        ),
        format!(
            "  resources: memory_max_bytes={} pids_max={} wall_timeout_secs={}",
            opt(req.resources.memory_max_bytes),
            opt(req.resources.pids_max),
            opt(req.resources.wall_timeout_secs)
        ),
        format!(
            "  kernel: landlock={} cgroup_v2={} user_namespaces={}",
            req.kernel.landlock, req.kernel.cgroup_v2, req.kernel.user_namespaces
        ),
        format!("  dod_ebpf: {}", req.dod_ebpf),
        "host:".to_owned(),
        format!("  target: {}/{}", host.target_os, host.target_arch),
        format!(
            "  landlock: abi={} lsm_active={}",
            host.landlock
                .abi
                .map_or_else(|| "unknown".to_owned(), |abi| abi.to_string()),
            host.landlock
                .lsm_active
                .map_or_else(|| "unknown".to_owned(), |active| active.to_string())
        ),
        format!(
            "  bwrap: {} cgroup_v2_delegated: {} user_namespaces: {} sandbox_exec: {}",
            host.bwrap_available,
            host.cgroup_v2_delegated,
            host.user_namespaces
                .map_or_else(|| "unknown".to_owned(), |userns| userns.to_string()),
            host.sandbox_exec_available
        ),
        format!(
            "  enforcement: filesystem={} network={} no_new_privs={} capabilities={} resource_limits={}",
            best.filesystem,
            best.network,
            best.no_new_privs,
            best.capabilities,
            best.resource_limits
        ),
        format!("verdict: {}", verdict.label()),
    ];
    for finding in &verdict.missing {
        let tag = match (finding.overridable, verdict.allow_degraded) {
            (false, _) => "missing",
            (true, true) => "degraded",
            (true, false) => "missing (--allow-degraded accepts it)",
        };
        lines.push(format!("  {tag}: {}", finding.message));
    }
    lines.join("\n")
}

/// Runs `--requirements`: loads and verifies via `load`, probes the host,
/// and prints requirements, host report and verdict (text, or JSON with
/// `json`).
///
/// # Description
/// Exit `0` if the agent would be admitted (possibly degraded with
/// `allow_degraded`), [`EXIT_ADMISSION`] if it would be refused — so a
/// script can test a host without starting the agent.
///
/// # Errors
/// Whatever `load` and [`crate::verify::root_ir`] return.
pub fn run_requirements(
    load: impl FnOnce() -> Result<(Artifact, Bundle), RunnerError>,
    json: bool,
    allow_degraded: bool,
) -> Result<ExitCode, RunnerError> {
    let (_artifact, bundle) = load()?;
    let ir = crate::verify::root_ir(&bundle)?;
    let host = HostReport::probe();
    let verdict = Verdict::evaluate(&ir.requirements, &host, allow_degraded);
    if json {
        let agent = ir.id.to_string();
        println!(
            "{}",
            render_json(&agent, &ir.requirements, &host, &verdict)?
        );
    } else {
        let agent = format!("{} ({})", ir.id, ir.specialization);
        println!("{}", render_text(&agent, &ir.requirements, &host, &verdict));
    }
    Ok(if verdict.admitted {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(EXIT_ADMISSION)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use harw_agent_dsl::ir_v2::{
        KernelRequirements, NetworkRequirement, SandboxRequirementLevels, TargetSpec,
    };
    use harw_job_runtime::{HostFacts, HostLandlock, SandboxReport};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A host whose best report is `report`, on linux/x86_64.
    fn host_with(report: SandboxReport) -> HostReport {
        HostReport {
            target_os: "linux".to_owned(),
            target_arch: "x86_64".to_owned(),
            landlock: HostLandlock {
                lsm_active: Some(true),
                abi: Some(5),
            },
            bwrap_available: false,
            cgroup_v2_delegated: true,
            user_namespaces: Some(true),
            sandbox_exec_available: false,
            best_report: report,
        }
    }

    /// A host that offers nothing at all (unknown OS, nothing enforced).
    fn empty_host() -> HostReport {
        HostReport::from_facts(HostFacts {
            target_os: "plan9".to_owned(),
            target_arch: "mips".to_owned(),
            ..HostFacts::default()
        })
    }

    fn sandbox(filesystem: RequirementLevel) -> ExecutionRequirements {
        ExecutionRequirements {
            sandbox: SandboxRequirementLevels {
                filesystem,
                ..SandboxRequirementLevels::default()
            },
            ..ExecutionRequirements::default()
        }
    }

    fn refused(
        result: Result<AdmissionReport, AdmissionRefused>,
    ) -> Result<AdmissionRefused, Box<dyn std::error::Error>> {
        match result {
            Ok(report) => Err(format!("expected a refusal, got {report:?}").into()),
            Err(refused) => Ok(refused),
        }
    }

    #[test]
    fn test_not_needed_passes_on_an_empty_host() -> TestResult {
        let report = check(&ExecutionRequirements::default(), &empty_host(), false)?;
        assert!(!report.is_degraded());
        Ok(())
    }

    #[test]
    fn test_required_vs_partial_is_admitted() -> TestResult {
        // bwrap reports the filesystem `partial` (no read/execute split):
        // that is still enforcement, so `required` is met, not degraded.
        let host = host_with(SandboxReport::uniform(EnforcementState::Partial));
        let report = check(&sandbox(RequirementLevel::Required), &host, false)?;
        assert!(!report.is_degraded());
        Ok(())
    }

    #[test]
    fn test_required_vs_not_enforced_is_refused_even_with_allow_degraded() -> TestResult {
        for state in [EnforcementState::NotEnforced, EnforcementState::Unsupported] {
            let host = host_with(SandboxReport::uniform(state));
            let req = sandbox(RequirementLevel::Required);
            for allow in [false, true] {
                let refusal = refused(check(&req, &host, allow))?;
                assert_eq!(refusal.missing.len(), 1);
                let finding = refusal.missing.first().ok_or("one finding")?;
                assert_eq!(finding.item, "sandbox.filesystem");
                assert_eq!(finding.required, "required");
                assert_eq!(finding.available, state.to_string());
                assert!(!finding.overridable);
                let text = refusal.to_string();
                assert!(text.contains("sandbox filesystem"), "{text}");
                assert!(!text.contains("accepted by --allow-degraded"), "{text}");
            }
        }
        Ok(())
    }

    #[test]
    fn test_required_vs_enforced_is_admitted() -> TestResult {
        let host = host_with(SandboxReport::uniform(EnforcementState::Enforced));
        let report = check(&sandbox(RequirementLevel::Required), &host, false)?;
        assert!(!report.is_degraded());
        Ok(())
    }

    #[test]
    fn test_best_effort_vs_partial_is_admitted() -> TestResult {
        let host = host_with(SandboxReport::uniform(EnforcementState::Partial));
        let report = check(&sandbox(RequirementLevel::BestEffort), &host, false)?;
        assert!(!report.is_degraded());
        Ok(())
    }

    #[test]
    fn test_best_effort_vs_not_enforced_needs_allow_degraded() -> TestResult {
        let host = host_with(SandboxReport::uniform(EnforcementState::NotEnforced));
        let req = sandbox(RequirementLevel::BestEffort);
        let refusal = refused(check(&req, &host, false))?;
        assert_eq!(refusal.missing.len(), 1);
        assert!(refusal.missing.iter().all(|finding| finding.overridable));
        assert!(
            refusal.to_string().contains("--allow-degraded"),
            "{refusal}"
        );

        let report = check(&req, &host, true)?;
        assert!(report.is_degraded());
        let finding = report.degraded.first().ok_or("one degraded item")?;
        assert_eq!(finding.item, "sandbox.filesystem");
        assert_eq!(finding.available, "not_enforced");
        Ok(())
    }

    #[test]
    fn test_hard_and_soft_items_are_all_listed() -> TestResult {
        let host = host_with(SandboxReport::uniform(EnforcementState::Unsupported));
        let req = ExecutionRequirements {
            sandbox: SandboxRequirementLevels {
                filesystem: RequirementLevel::Required,
                network: RequirementLevel::BestEffort,
                ..SandboxRequirementLevels::default()
            },
            ..ExecutionRequirements::default()
        };
        let refusal = refused(check(&req, &host, true))?;
        let items: Vec<&str> = refusal.missing.iter().map(|f| f.item.as_str()).collect();
        assert_eq!(items, ["sandbox.filesystem", "sandbox.network"]);
        let text = refusal.to_string();
        assert!(text.contains("accepted by --allow-degraded"), "{text}");
        Ok(())
    }

    #[test]
    fn test_target_mismatch_is_refused() -> TestResult {
        let host = host_with(SandboxReport::uniform(EnforcementState::Enforced));
        let req = ExecutionRequirements {
            targets: vec![TargetSpec {
                os: "linux".to_owned(),
                arch: Some("aarch64".to_owned()),
            }],
            ..ExecutionRequirements::default()
        };
        let refusal = refused(check(&req, &host, true))?;
        let finding = refusal.missing.first().ok_or("one finding")?;
        assert_eq!(finding.item, "target");
        assert!(!finding.overridable);
        assert!(finding.message.contains("linux/aarch64"), "{finding}");
        assert!(finding.message.contains("linux/x86_64"), "{finding}");
        Ok(())
    }

    #[test]
    fn test_target_without_arch_matches_any_arch() -> TestResult {
        let host = host_with(SandboxReport::uniform(EnforcementState::Enforced));
        let req = ExecutionRequirements {
            targets: vec![
                TargetSpec {
                    os: "macos".to_owned(),
                    arch: None,
                },
                TargetSpec {
                    os: "linux".to_owned(),
                    arch: None,
                },
            ],
            ..ExecutionRequirements::default()
        };
        check(&req, &host, false)?;
        Ok(())
    }

    #[test]
    fn test_dod_ebpf_is_refused() -> TestResult {
        let host = host_with(SandboxReport::uniform(EnforcementState::Enforced));
        let req = ExecutionRequirements {
            dod_ebpf: true,
            ..ExecutionRequirements::default()
        };
        let refusal = refused(check(&req, &host, true))?;
        let finding = refusal.missing.first().ok_or("one finding")?;
        assert_eq!(finding.item, "dod_ebpf");
        assert!(!finding.overridable);
        Ok(())
    }

    #[test]
    fn test_kernel_landlock_is_hard_and_cgroup_is_soft() -> TestResult {
        let mut host = host_with(SandboxReport::uniform(EnforcementState::Enforced));
        host.landlock = HostLandlock {
            lsm_active: Some(false),
            abi: Some(0),
        };
        host.cgroup_v2_delegated = false;
        let landlock = ExecutionRequirements {
            kernel: KernelRequirements {
                landlock: true,
                ..KernelRequirements::default()
            },
            ..ExecutionRequirements::default()
        };
        let refusal = refused(check(&landlock, &host, true))?;
        let finding = refusal.missing.first().ok_or("one finding")?;
        assert_eq!(finding.item, "kernel.landlock");
        assert!(!finding.overridable);

        let cgroup = ExecutionRequirements {
            kernel: KernelRequirements {
                cgroup_v2: true,
                ..KernelRequirements::default()
            },
            ..ExecutionRequirements::default()
        };
        refused(check(&cgroup, &host, false))?;
        let report = check(&cgroup, &host, true)?;
        let finding = report.degraded.first().ok_or("one degraded item")?;
        assert_eq!(finding.item, "kernel.cgroup_v2");
        Ok(())
    }

    #[test]
    fn test_requirements_text_output() -> TestResult {
        let req = ExecutionRequirements {
            targets: vec![TargetSpec {
                os: "linux".to_owned(),
                arch: Some("x86_64".to_owned()),
            }],
            network: NetworkRequirement::ProxyOnly,
            sandbox: SandboxRequirementLevels {
                filesystem: RequirementLevel::BestEffort,
                ..SandboxRequirementLevels::default()
            },
            ..ExecutionRequirements::default()
        };
        let host = host_with(SandboxReport::uniform(EnforcementState::NotEnforced));
        let verdict = Verdict::evaluate(&req, &host, true);
        let text = render_text("acme.agent.req@1 (req)", &req, &host, &verdict);
        for expected in [
            "agent: acme.agent.req@1 (req)",
            "  targets: linux/x86_64",
            "  network: proxy_only",
            "  sandbox: filesystem=best_effort network=not_needed",
            "  target: linux/x86_64",
            "  landlock: abi=5 lsm_active=true",
            "  enforcement: filesystem=not_enforced",
            "verdict: admitted (degraded)",
            "  degraded: sandbox filesystem: best_effort",
        ] {
            assert!(text.contains(expected), "missing `{expected}` in:\n{text}");
        }

        let refused_verdict = Verdict::evaluate(&req, &host, false);
        let text = render_text("acme.agent.req@1 (req)", &req, &host, &refused_verdict);
        assert!(text.contains("verdict: refused"), "{text}");
        assert!(
            text.contains("missing (--allow-degraded accepts it)"),
            "{text}"
        );
        Ok(())
    }

    #[test]
    fn test_requirements_json_output() -> TestResult {
        let req = ExecutionRequirements {
            dod_ebpf: true,
            ..ExecutionRequirements::default()
        };
        let host = host_with(SandboxReport::uniform(EnforcementState::Enforced));
        let verdict = Verdict::evaluate(&req, &host, false);
        let value: serde_json::Value =
            serde_json::from_str(&render_json("acme.agent.req@1", &req, &host, &verdict)?)?;
        assert_eq!(value["agent"], "acme.agent.req@1");
        assert_eq!(value["requirements"]["dod_ebpf"], true);
        assert_eq!(value["host"]["target_os"], "linux");
        assert_eq!(value["host"]["best_report"]["filesystem"], "enforced");
        assert_eq!(value["verdict"]["admitted"], false);
        assert_eq!(value["verdict"]["missing"][0]["item"], "dod_ebpf");
        assert_eq!(value["verdict"]["missing"][0]["overridable"], false);
        Ok(())
    }
}
