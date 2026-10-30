//! The enforcement report the trampoline writes before `exec`, and the pure
//! decisions around it (requirement check, resource-limit dimension, merge
//! with a Bubblewrap launcher's predicted report).
//!
//! # Report file format
//! One [`SandboxReport`] as compact JSON followed by a single `\n`. The
//! newline marks the document complete: a reader that sees no trailing
//! newline (yet) gets `Ok(None)` from [`read_report`], never a half-parsed
//! report.

use std::io::{self, Read as _};
use std::path::Path;

use harw_job_core::{EnforcementState, SandboxReport, SandboxRequirement};

use crate::error::ExecError;

/// Upper bound for a report file; a real report is ~150 bytes.
const MAX_REPORT_BYTES: u64 = 4 * 1024;

/// The report when no sandbox policy was applied: every sandbox dimension
/// [`EnforcementState::NotEnforced`] (the platform supports them, the plan
/// did not ask). `resource_limits` is filled in separately by
/// [`resource_limits_state`].
#[must_use]
pub fn unsandboxed_report() -> SandboxReport {
    SandboxReport::uniform(EnforcementState::NotEnforced)
}

/// The `resource_limits` dimension of the report.
///
/// - `cgroup_limits`: `None` if the plan has no cgroup membership,
///   otherwise the state of the cgroup limits (from
///   `CgroupMembership::limits`; unknown counts as `NotEnforced`). A
///   membership that failed verification (`CgroupVerify`) passes
///   `NotEnforced`: the job would run outside the cgroup that carries the
///   limits, so this dimension records the refusal.
/// - `rlimits_applied`: whether the plan had rlimits (the trampoline fails
///   instead of continuing when `setrlimit` fails, so requested means
///   applied).
///
/// Nothing requested is reported as `Enforced`: there is no requested
/// limit that is missing (the same convention `harw-job-linux` uses for an
/// unrestricted network policy).
#[must_use]
pub fn resource_limits_state(
    cgroup_limits: Option<EnforcementState>,
    rlimits_applied: bool,
) -> EnforcementState {
    match (cgroup_limits, rlimits_applied) {
        (None, _) | (Some(EnforcementState::Enforced), _) => EnforcementState::Enforced,
        (Some(state), false) => state,
        // cgroup limits fell short, rlimits hold.
        (Some(_), true) => EnforcementState::Partial,
    }
}

/// Linux capability number of `CAP_SYS_ADMIN` (`linux/capability.h`).
const CAP_SYS_ADMIN: u32 = 21;
/// Linux capability number of `CAP_SYS_RESOURCE` (`linux/capability.h`).
const CAP_SYS_RESOURCE: u32 = 24;

/// Where [`ProcessIdentity::current`] reads the identity.
pub const PROC_SELF_STATUS: &str = "/proc/self/status";

/// Upper bound for reading `/proc/self/status` (about 1.5 KiB in practice).
const MAX_PROC_STATUS_BYTES: u64 = 64 * 1024;

/// The part of a process' identity that decides whether `RLIMIT_NPROC`
/// binds it (PL-97 E2g).
///
/// The kernel checks `RLIMIT_NPROC` on `fork`/`clone` but exempts a
/// process whose real UID is 0 and one with `CAP_SYS_ADMIN` or
/// `CAP_SYS_RESOURCE` (getrlimit(2)). A successful
/// `setrlimit(RLIMIT_NPROC)` therefore proves nothing for such a process:
/// the limit is set and never enforced.
///
/// The values come from `/proc/self/status` (`Uid:` real UID, `CapEff:`,
/// `CapPrm:`), as the process sees them in its own user namespace. A
/// capability held in a child user namespace never counts towards the
/// kernel's exemption, so counting it here only under-claims; a real UID
/// remapped away from 0 inside a user namespace is caught by the stage
/// that runs in the supervisor's namespace (the outer stage of the
/// Bubblewrap chain probes before `bwrap`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessIdentity {
    /// Real UID (first field of `Uid:`).
    pub real_uid: u32,
    /// Effective capability set (`CapEff:`, bit `n` = capability `n`).
    pub effective_caps: u64,
    /// Permitted capability set (`CapPrm:`).
    pub permitted_caps: u64,
}

impl ProcessIdentity {
    /// Parses the content of `/proc/<pid>/status`. Pure.
    ///
    /// `None` when `Uid:`, `CapEff:` or `CapPrm:` is missing or malformed;
    /// callers treat an unknown identity as exempt (fail-closed,
    /// [`nproc_exemption`]).
    #[must_use]
    pub fn parse_status(status: &str) -> Option<Self> {
        let mut real_uid = None;
        let mut effective_caps = None;
        let mut permitted_caps = None;
        for line in status.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match key {
                "Uid" => real_uid = value.split_whitespace().next()?.parse().ok(),
                "CapEff" => effective_caps = u64::from_str_radix(value, 16).ok(),
                "CapPrm" => permitted_caps = u64::from_str_radix(value, 16).ok(),
                _ => {}
            }
        }
        Some(Self {
            real_uid: real_uid?,
            effective_caps: effective_caps?,
            permitted_caps: permitted_caps?,
        })
    }

    /// The identity of the calling process, read from
    /// [`PROC_SELF_STATUS`] (bounded). `None` when it cannot be read or
    /// parsed.
    #[must_use]
    pub fn current() -> Option<Self> {
        let mut content = String::new();
        std::fs::File::open(PROC_SELF_STATUS)
            .and_then(|file| {
                file.take(MAX_PROC_STATUS_BYTES)
                    .read_to_string(&mut content)
            })
            .ok()?;
        Self::parse_status(&content)
    }

    /// Why `RLIMIT_NPROC` would not bind this identity, `None` when it
    /// does.
    #[must_use]
    pub fn nproc_exemption(&self) -> Option<&'static str> {
        let exempting = (1_u64 << CAP_SYS_ADMIN) | (1_u64 << CAP_SYS_RESOURCE);
        if self.real_uid == 0 {
            Some("the process that execs the job has real uid 0")
        } else if (self.effective_caps | self.permitted_caps) & exempting != 0 {
            Some("the process that execs the job keeps CAP_SYS_ADMIN or CAP_SYS_RESOURCE")
        } else {
            None
        }
    }
}

/// Why `RLIMIT_NPROC` would not bind a process of `identity`, `None` when
/// it does. An unknown identity counts as exempt (fail-closed).
#[must_use]
pub fn nproc_exemption(identity: Option<ProcessIdentity>) -> Option<&'static str> {
    match identity {
        Some(identity) => identity.nproc_exemption(),
        None => Some("the identity of the process that execs the job is unknown"),
    }
}

/// How the process count cap of a job (`process_count_max`) holds
/// (PL-97 E2g).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessCountCap {
    /// `RLIMIT_NPROC` binds the process that execs the job.
    Rlimit,
    /// The rlimit does not bind (exempt identity), but the job cgroup's
    /// `pids.max` enforces at most the requested count.
    CgroupPids,
    /// Neither holds: the job's process count is not capped.
    NotEnforced,
}

impl ProcessCountCap {
    /// The cap for an identity that is (`exempt`) or is not exempt from
    /// `RLIMIT_NPROC`, with or without an enforcing `pids.max` in a
    /// verified job cgroup (`cgroup_pids_cover`). Pure.
    #[must_use]
    pub fn decide(exempt: bool, cgroup_pids_cover: bool) -> Self {
        match (exempt, cgroup_pids_cover) {
            (false, _) => Self::Rlimit,
            (true, true) => Self::CgroupPids,
            (true, false) => Self::NotEnforced,
        }
    }
}

/// The `resource_limits` dimension once the process count cap is known.
/// Pure.
///
/// `SandboxReport` has no own rlimit dimension yet, so an uncapped process
/// count lowers `resource_limits`: to [`EnforcementState::NotEnforced`]
/// when nothing else of the request holds, otherwise to at most
/// [`EnforcementState::Partial`] (`other_limits_hold`: another rlimit is
/// applied, or the job cgroup enforces at least part of its limits). A cap
/// that holds leaves `state` unchanged. Never `Enforced` for an uncapped
/// process count.
#[must_use]
pub fn with_process_count(
    state: EnforcementState,
    cap: ProcessCountCap,
    other_limits_hold: bool,
) -> EnforcementState {
    match cap {
        ProcessCountCap::Rlimit | ProcessCountCap::CgroupPids => state,
        ProcessCountCap::NotEnforced if !other_limits_hold => EnforcementState::NotEnforced,
        ProcessCountCap::NotEnforced => match state {
            EnforcementState::Enforced => EnforcementState::Partial,
            other => other,
        },
    }
}

/// The refusal of a job that requires its rlimits (`require_rlimits`)
/// while its process count is not capped (`exemption` from
/// [`nproc_exemption`]): exit [`crate::exit_code::SANDBOX_REFUSED`].
#[must_use]
pub fn process_count_refusal(exemption: &str) -> ExecError {
    ExecError::Sandbox {
        message: format!(
            "process_count_max (RLIMIT_NPROC) is required (require_rlimits) but not enforced: \
             {exemption}, which the kernel exempts from RLIMIT_NPROC, and no verified job \
             cgroup pids.max at or below the requested count covers it"
        ),
    }
}

/// Merges the report a Bubblewrap launcher predicts for its own layer with
/// the report the trampoline wrote from inside that namespace. Pure.
///
/// Both layers restrict the same process at once, so a dimension is as
/// strong as its stronger layer: `Enforced` over `Partial` over
/// `NotEnforced` over `Unsupported`. Per dimension:
/// - `network`: primarily the namespace (an own network namespace,
///   proxy-only egress); the trampoline's Landlock TCP rules add to it.
/// - `filesystem`, `no_new_privs`, `capabilities`: primarily the
///   trampoline (Landlock separates read from execute, a mount namespace
///   cannot); the launcher's mounts, its own `NO_NEW_PRIVS` and its
///   capability drop still hold underneath.
/// - `resource_limits`: the trampoline's report only. The launcher enforces
///   no limits, and the trampoline reports `Enforced` only when the cgroup
///   limits and the rlimits both hold ([`resource_limits_state`]).
///
/// No dimension ends up stronger than what one of the two layers reports;
/// one that neither layer enforces stays unenforced.
#[must_use]
pub fn merge_reports(bwrap_predicted: &SandboxReport, trampoline: &SandboxReport) -> SandboxReport {
    SandboxReport {
        filesystem: stronger(bwrap_predicted.filesystem, trampoline.filesystem),
        network: stronger(bwrap_predicted.network, trampoline.network),
        no_new_privs: stronger(bwrap_predicted.no_new_privs, trampoline.no_new_privs),
        capabilities: stronger(bwrap_predicted.capabilities, trampoline.capabilities),
        resource_limits: trampoline.resource_limits,
    }
}

/// The stronger of two layers' states for the same dimension.
fn stronger(left: EnforcementState, right: EnforcementState) -> EnforcementState {
    let rank = |state: EnforcementState| match state {
        EnforcementState::Unsupported => 0_u8,
        EnforcementState::NotEnforced => 1,
        EnforcementState::Partial => 2,
        EnforcementState::Enforced => 3,
    };
    if rank(right) > rank(left) {
        right
    } else {
        left
    }
}

/// Decides whether `report` satisfies `requirement`
/// ([`SandboxReport::satisfies`]).
///
/// # Errors
/// [`ExecError::RequirementNotMet`] naming the dimensions that are not
/// fully enforced.
pub fn check_requirement(
    requirement: SandboxRequirement,
    report: &SandboxReport,
) -> Result<(), ExecError> {
    if report.satisfies(requirement) {
        Ok(())
    } else {
        Err(ExecError::RequirementNotMet {
            requirement,
            shortfalls: report.shortfalls(),
        })
    }
}

/// Renders the report file content (JSON + completion newline).
pub(crate) fn render_report(report: &SandboxReport) -> Vec<u8> {
    // Serializing five unit enums cannot fail; fall back to an empty
    // (never complete) document instead of panicking.
    let mut bytes = serde_json::to_vec(report).unwrap_or_default();
    if !bytes.is_empty() {
        bytes.push(b'\n');
    }
    bytes
}

/// Reads a report file written by the trampoline.
///
/// # Returns
/// `Ok(None)` while the report is missing or incomplete (the trampoline has
/// not reached the report step, or failed before it); `Ok(Some(report))`
/// once it is complete.
///
/// # Errors
/// [`ExecError::ReportIo`] for I/O errors other than "not found" or a file
/// above 4 KiB; [`ExecError::ReportParse`] for a complete file that is not a
/// valid report.
pub fn read_report(path: &Path) -> Result<Option<SandboxReport>, ExecError> {
    let io_error = |source: io::Error| ExecError::ReportIo {
        path: path.to_path_buf(),
        source,
    };
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io_error(error)),
    };
    let mut bytes = Vec::new();
    file.take(MAX_REPORT_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_REPORT_BYTES {
        return Err(io_error(io::Error::new(
            io::ErrorKind::InvalidData,
            "report file too large",
        )));
    }
    let Some(document) = bytes.strip_suffix(b"\n") else {
        return Ok(None);
    };
    serde_json::from_slice(document)
        .map(Some)
        .map_err(|error| ExecError::ReportParse {
            path: path.to_path_buf(),
            message: error.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    use EnforcementState as S;

    #[test]
    fn test_requirement_matrix() -> TestResult {
        let full = SandboxReport::uniform(S::Enforced);
        let mut partial = full;
        partial.network = S::Partial;
        let unsandboxed = unsandboxed_report();

        for requirement in [
            SandboxRequirement::None,
            SandboxRequirement::BestEffort,
            SandboxRequirement::Required,
        ] {
            check_requirement(requirement, &full).map_err(ctx("full report"))?;
        }
        for report in [partial, unsandboxed] {
            check_requirement(SandboxRequirement::None, &report).map_err(ctx("none"))?;
            check_requirement(SandboxRequirement::BestEffort, &report)
                .map_err(ctx("best effort"))?;
        }
        match check_requirement(SandboxRequirement::Required, &partial) {
            Err(ExecError::RequirementNotMet {
                requirement: SandboxRequirement::Required,
                shortfalls,
            }) => assert_eq!(shortfalls, vec!["network"]),
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        match check_requirement(SandboxRequirement::Required, &unsandboxed) {
            Err(ExecError::RequirementNotMet { shortfalls, .. }) => {
                assert_eq!(shortfalls.len(), 5, "{shortfalls:?}");
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        Ok(())
    }

    #[test]
    fn test_resource_limits_state_table() {
        assert_eq!(resource_limits_state(None, false), S::Enforced);
        assert_eq!(resource_limits_state(None, true), S::Enforced);
        assert_eq!(resource_limits_state(Some(S::Enforced), false), S::Enforced);
        assert_eq!(resource_limits_state(Some(S::Enforced), true), S::Enforced);
        assert_eq!(resource_limits_state(Some(S::Partial), false), S::Partial);
        assert_eq!(
            resource_limits_state(Some(S::NotEnforced), false),
            S::NotEnforced
        );
        assert_eq!(
            resource_limits_state(Some(S::NotEnforced), true),
            S::Partial
        );
        assert_eq!(
            resource_limits_state(Some(S::Unsupported), true),
            S::Partial
        );
    }

    /// `/proc/self/status` of a root shell in a container, trimmed.
    const ROOT_STATUS: &str = "Name:\tcat\nUmask:\t0022\nState:\tR (running)\n\
        Uid:\t0\t0\t0\t0\nGid:\t0\t0\t0\t0\nCapInh:\t0000000000000000\n\
        CapPrm:\t00000000a80425fb\nCapEff:\t00000000a80425fb\nCapBnd:\t00000000a80425fb\n";

    #[test]
    fn test_process_identity_parsing_and_exemption() -> TestResult {
        let root = ProcessIdentity::parse_status(ROOT_STATUS).ok_or(TestError::Missing("root"))?;
        assert_eq!(root.real_uid, 0);
        assert_eq!(root.effective_caps, 0xa804_25fb);
        assert!(root.nproc_exemption().is_some());

        // Real uid 1000 with effective uid 0 (setuid): the real uid counts.
        let user = "Uid:\t1000\t0\t0\t0\nCapPrm:\t0000000000000000\nCapEff:\t0000000000000000\n";
        let user = ProcessIdentity::parse_status(user).ok_or(TestError::Missing("user"))?;
        assert_eq!(user.real_uid, 1000);
        assert_eq!(user.nproc_exemption(), None);
        assert_eq!(nproc_exemption(Some(user)), None);

        // CAP_SYS_ADMIN (21) effective, CAP_SYS_RESOURCE (24) permitted.
        for caps in [
            "CapPrm:\t0\nCapEff:\t200000\n",
            "CapPrm:\t1000000\nCapEff:\t0\n",
        ] {
            let status = format!("Uid:\t1000\t1000\t1000\t1000\n{caps}");
            let capable =
                ProcessIdentity::parse_status(&status).ok_or(TestError::Missing("capable"))?;
            assert!(capable.nproc_exemption().is_some(), "{status}");
        }
        // Other capabilities do not exempt (CAP_NET_ADMIN = 12).
        let net = ProcessIdentity::parse_status("Uid:\t7 7 7 7\nCapPrm:\t1000\nCapEff:\t1000\n")
            .ok_or(TestError::Missing("net"))?;
        assert_eq!(net.nproc_exemption(), None);

        // Missing or malformed fields: unknown, which counts as exempt.
        for broken in [
            "",
            "Uid:\t0\n",
            "Uid:\tx\nCapPrm:\t0\nCapEff:\t0\n",
            "Uid:\t0\nCapPrm:\tzz\nCapEff:\t0\n",
        ] {
            assert_eq!(ProcessIdentity::parse_status(broken), None, "{broken:?}");
        }
        assert!(nproc_exemption(None).is_some());
        // The real file parses on Linux.
        assert!(ProcessIdentity::current().is_some());
        Ok(())
    }

    #[test]
    fn test_process_count_cap_and_dimension() {
        assert_eq!(
            ProcessCountCap::decide(false, false),
            ProcessCountCap::Rlimit
        );
        assert_eq!(
            ProcessCountCap::decide(false, true),
            ProcessCountCap::Rlimit
        );
        assert_eq!(
            ProcessCountCap::decide(true, true),
            ProcessCountCap::CgroupPids
        );
        assert_eq!(
            ProcessCountCap::decide(true, false),
            ProcessCountCap::NotEnforced
        );
        for state in STATES {
            for others in [false, true] {
                assert_eq!(
                    with_process_count(state, ProcessCountCap::Rlimit, others),
                    state
                );
                assert_eq!(
                    with_process_count(state, ProcessCountCap::CgroupPids, others),
                    state
                );
                let uncapped = with_process_count(state, ProcessCountCap::NotEnforced, others);
                assert_ne!(uncapped, S::Enforced, "{state:?} {others}");
            }
            assert_eq!(
                with_process_count(state, ProcessCountCap::NotEnforced, false),
                S::NotEnforced
            );
        }
        assert_eq!(
            with_process_count(S::Enforced, ProcessCountCap::NotEnforced, true),
            S::Partial
        );
        let refusal = process_count_refusal("the process that execs the job has real uid 0");
        assert_eq!(refusal.exit_code(), crate::exit_code::SANDBOX_REFUSED);
        assert!(refusal.to_string().contains("real uid 0"));
    }

    const STATES: [S; 4] = [S::Enforced, S::Partial, S::NotEnforced, S::Unsupported];

    /// The report with `dimension` (index into
    /// [`SandboxReport::dimensions`]) set to `state` and every other
    /// dimension `rest`.
    fn with_dimension(dimension: usize, state: S, rest: S) -> SandboxReport {
        let mut report = SandboxReport::uniform(rest);
        match dimension {
            0 => report.filesystem = state,
            1 => report.network = state,
            2 => report.no_new_privs = state,
            3 => report.capabilities = state,
            _ => report.resource_limits = state,
        }
        report
    }

    fn dimension_of(report: &SandboxReport, dimension: usize) -> Option<S> {
        report.dimensions().get(dimension).map(|(_, state)| *state)
    }

    #[test]
    fn test_merge_reports_table() -> TestResult {
        // Expected merge of (bwrap, trampoline) for the four layered
        // dimensions: the stronger layer, written out by hand.
        let layered = |bwrap: S, trampoline: S| match (bwrap, trampoline) {
            (S::Enforced, _) | (_, S::Enforced) => S::Enforced,
            (S::Partial, _) | (_, S::Partial) => S::Partial,
            (S::NotEnforced, _) | (_, S::NotEnforced) => S::NotEnforced,
            (S::Unsupported, S::Unsupported) => S::Unsupported,
        };
        for (dimension, (name, _)) in unsandboxed_report().dimensions().iter().enumerate() {
            for bwrap in STATES {
                for trampoline in STATES {
                    for rest in STATES {
                        let merged = merge_reports(
                            &with_dimension(dimension, bwrap, rest),
                            &with_dimension(dimension, trampoline, rest),
                        );
                        let expected = if *name == "resource_limits" {
                            trampoline
                        } else {
                            layered(bwrap, trampoline)
                        };
                        let got = dimension_of(&merged, dimension)
                            .ok_or(TestError::Missing("merged dimension"))?;
                        assert_eq!(got, expected, "{name}: {bwrap:?} + {trampoline:?}");
                        // Never stronger than one of the layers.
                        assert!(got == bwrap || got == trampoline, "{name}");
                        // The other dimensions are untouched.
                        for other in (0..5).filter(|other| *other != dimension) {
                            assert_eq!(dimension_of(&merged, other), Some(rest), "{name}");
                        }
                    }
                }
            }
        }
        Ok(())
    }

    #[test]
    fn test_merge_reports_bwrap_trampoline_chain() {
        // What the bubblewrap executor predicts for a network-denying
        // policy, and a trampoline with Landlock V5, verified cgroup and
        // rlimits.
        let bwrap = SandboxReport {
            filesystem: S::Partial,
            network: S::Enforced,
            no_new_privs: S::Enforced,
            capabilities: S::Enforced,
            resource_limits: S::NotEnforced,
        };
        let mut trampoline = SandboxReport::uniform(S::Enforced);
        trampoline.network = S::Partial;
        assert_eq!(
            merge_reports(&bwrap, &trampoline),
            SandboxReport::uniform(S::Enforced)
        );
        // No Landlock on the kernel: the mounts still confine (Partial);
        // a refused cgroup verification stays visible.
        let mut weak = unsandboxed_report();
        weak.no_new_privs = S::Enforced;
        weak.capabilities = S::Enforced;
        weak.filesystem = S::Unsupported;
        weak.resource_limits = S::NotEnforced;
        let merged = merge_reports(&bwrap, &weak);
        assert_eq!(merged.filesystem, S::Partial);
        assert_eq!(merged.network, S::Enforced);
        assert_eq!(merged.resource_limits, S::NotEnforced);
        assert!(!merged.satisfies(SandboxRequirement::Required));
    }

    #[test]
    fn test_read_report_states() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("report.json");
        assert_eq!(read_report(&path).map_err(ctx("missing"))?, None);

        let mut report = SandboxReport::uniform(S::Enforced);
        report.network = S::Partial;
        let rendered = render_report(&report);
        assert_eq!(rendered.last(), Some(&b'\n'));

        let incomplete = rendered
            .get(..rendered.len() - 1)
            .ok_or(TestError::Missing("rendered report body"))?;
        std::fs::write(&path, incomplete).map_err(ctx("write incomplete"))?;
        assert_eq!(read_report(&path).map_err(ctx("incomplete"))?, None);
        std::fs::write(&path, b"").map_err(ctx("write empty"))?;
        assert_eq!(read_report(&path).map_err(ctx("empty"))?, None);

        std::fs::write(&path, &rendered).map_err(ctx("write complete"))?;
        assert_eq!(read_report(&path).map_err(ctx("complete"))?, Some(report));

        std::fs::write(&path, b"{\"sandboxed\":true}\n").map_err(ctx("write bogus"))?;
        assert!(matches!(
            read_report(&path),
            Err(ExecError::ReportParse { .. })
        ));
        std::fs::write(&path, vec![b' '; 8192]).map_err(ctx("write huge"))?;
        assert!(matches!(
            read_report(&path),
            Err(ExecError::ReportIo { .. })
        ));
        Ok(())
    }
}
