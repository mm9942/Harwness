//! The enforcement report the trampoline writes before `exec`, and the pure
//! decisions around it (requirement check, resource-limit dimension).
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
/// - `cgroup_limits`: `None` if the plan joins no cgroup, otherwise the
///   state of the cgroup limits (from `CgroupJoin::limits`; unknown counts
///   as `NotEnforced`).
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
