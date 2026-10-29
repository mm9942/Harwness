//! The trampoline itself: plan → cgroup → rlimits → sandbox → report →
//! requirement check → `exec` (Job-Runtime-Doc §24).
//!
//! The cgroup step either joins the job cgroup ([`CgroupMembership::Attach`])
//! or verifies `/proc/self/cgroup` ([`CgroupMembership::Verify`]). A failed
//! verification is recorded, not raised at once: the remaining controls
//! still run, the report shows `resource_limits` as the dimension that fell
//! short, and the job is refused right after the report is written.
//!
//! Every step before `exec` is a separate function so the pure parts
//! (argument parsing, plan parsing, requirement check) are unit-testable;
//! the steps that restrict the calling process are exercised only by the
//! integration tests, which run the real binary.

use std::convert::Infallible;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use harw_job_core::{EnforcementState, SandboxReport};
use harw_job_linux::cgroup::{CgroupBackend as _, CgroupV2Fs};
use harw_job_linux::sandbox::apply_to_current_process;
use rustix::fs::{Mode, OFlags};

use crate::error::ExecError;
use crate::plan::{
    CgroupJoin, CgroupMembership, CgroupVerify, ExecPlanV1, MAX_PLAN_BYTES, PROC_SELF_CGROUP,
};
use crate::report::{check_requirement, render_report, resource_limits_state, unsandboxed_report};

/// Upper bound for reading `/proc/self/cgroup` (a few lines in practice).
const MAX_PROC_CGROUP_BYTES: u64 = 64 * 1024;

/// Parsed command line of the binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrampolineArgs {
    /// `--plan <path>`: the plan file (required, deleted after reading).
    pub plan: PathBuf,
    /// `--report <path>`: where to create the report (optional).
    pub report: Option<PathBuf>,
}

impl TrampolineArgs {
    /// Parses the arguments **after** `argv[0]`.
    ///
    /// # Errors
    /// [`ExecError::Usage`] for unknown, duplicate or value-less flags and a
    /// missing `--plan`.
    pub fn parse<I>(args: I) -> Result<Self, ExecError>
    where
        I: IntoIterator<Item = OsString>,
    {
        let usage = |message: String| ExecError::Usage { message };
        let mut plan = None;
        let mut report = None;
        let mut args = args.into_iter();
        while let Some(flag) = args.next() {
            let slot = match flag.to_str() {
                Some("--plan") => &mut plan,
                Some("--report") => &mut report,
                _ => return Err(usage(format!("unexpected argument {flag:?}"))),
            };
            let Some(value) = args.next() else {
                return Err(usage(format!("{flag:?} needs a path")));
            };
            if value.is_empty() {
                return Err(usage(format!("{flag:?} needs a non-empty path")));
            }
            if slot.replace(PathBuf::from(value)).is_some() {
                return Err(usage(format!("{flag:?} given twice")));
            }
        }
        let plan = plan.ok_or_else(|| usage("missing --plan".to_owned()))?;
        Ok(Self { plan, report })
    }
}

/// Runs the trampoline with the arguments after `argv[0]`. Returns only on
/// failure: on success the process image is replaced by the job.
///
/// # Errors
/// Every [`ExecError`]; [`ExecError::exit_code`] gives the exit status.
pub fn run_trampoline<I>(args: I) -> Result<Infallible, ExecError>
where
    I: IntoIterator<Item = OsString>,
{
    let args = TrampolineArgs::parse(args)?;
    let plan = load_plan(&args.plan)?;
    // Created while still unrestricted (see crate docs).
    let report_file = match &args.report {
        Some(path) => Some((create_report_file(path)?, path.as_path())),
        None => None,
    };
    let Controls { report, refusal } = apply_controls(&plan)?;
    let written = match report_file {
        Some((file, path)) => write_report(file, path, &report),
        None => Ok(()),
    };
    refusal_first(refusal, written)?;
    check_requirement(plan.sandbox_requirement, &report)?;
    Err(exec_job(&plan))
}

/// A refused cgroup membership outranks a failed report write: it came
/// first in the §24 order and is the reason the job does not run.
fn refusal_first(
    refusal: Option<ExecError>,
    written: Result<(), ExecError>,
) -> Result<(), ExecError> {
    match refusal {
        Some(refusal) => Err(refusal),
        None => written,
    }
}

/// Opens the plan file without following symlinks, verifies it is a
/// private regular file of the effective user, reads it and **always**
/// removes it, then parses it.
fn load_plan(path: &Path) -> Result<ExecPlanV1, ExecError> {
    let io_error = |source: io::Error| ExecError::PlanIo {
        path: path.to_path_buf(),
        source,
    };
    // NONBLOCK: a FIFO planted at the path must not hang the trampoline.
    let fd = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|errno| io_error(errno.into()))?;
    let file = File::from(fd);
    let read = read_private_file(&file, path);
    drop(file);
    let removed = std::fs::remove_file(path);
    let bytes = read?;
    removed.map_err(io_error)?;
    ExecPlanV1::from_json(&bytes)
}

fn read_private_file(file: &File, path: &Path) -> Result<Vec<u8>, ExecError> {
    let insecure = |reason: String| ExecError::PlanInsecure {
        path: path.to_path_buf(),
        reason,
    };
    let io_error = |source: io::Error| ExecError::PlanIo {
        path: path.to_path_buf(),
        source,
    };
    let metadata = file.metadata().map_err(io_error)?;
    if !metadata.is_file() {
        return Err(insecure("not a regular file".to_owned()));
    }
    let euid = rustix::process::geteuid().as_raw();
    if metadata.uid() != euid {
        return Err(insecure(format!(
            "owned by uid {}, expected {euid}",
            metadata.uid()
        )));
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(insecure(format!(
            "mode {:o} grants group/other access (expected 0600)",
            metadata.mode() & 0o777
        )));
    }
    let limit = u64::try_from(MAX_PLAN_BYTES).unwrap_or(u64::MAX);
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > MAX_PLAN_BYTES {
        return Err(ExecError::PlanTooLarge {
            limit: MAX_PLAN_BYTES,
        });
    }
    Ok(bytes)
}

/// Creates the report file `0600` with `O_EXCL|O_NOFOLLOW`: the trampoline
/// never writes into a file it did not create.
fn create_report_file(path: &Path) -> Result<File, ExecError> {
    rustix::fs::open(
        path,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::RUSR | Mode::WUSR,
    )
    .map(File::from)
    .map_err(|errno| ExecError::ReportIo {
        path: path.to_path_buf(),
        source: errno.into(),
    })
}

fn write_report(mut file: File, path: &Path, report: &SandboxReport) -> Result<(), ExecError> {
    file.write_all(&render_report(report))
        .and_then(|()| file.flush())
        .map_err(|source| ExecError::ReportIo {
            path: path.to_path_buf(),
            source,
        })
}

/// Result of [`apply_controls`].
#[derive(Debug)]
struct Controls {
    /// What is enforced for the process that would become the job.
    report: SandboxReport,
    /// Set when the cgroup membership could not be verified: the report is
    /// still written, then the job is refused with this error.
    refusal: Option<ExecError>,
}

/// Applies the controls to the calling process in the §24 order and
/// returns the merged report: cgroup membership → rlimits → sandbox.
///
/// cgroup first so that every later allocation is already accounted to the
/// job; rlimits before the sandbox because Landlock/capability drop do not
/// affect `setrlimit` but a dropped `CAP_SYS_RESOURCE` would; the sandbox
/// last because `restrict_self` is irreversible and must see the final
/// state.
///
/// A failed attach aborts at once (as before); a failed verification is
/// returned in [`Controls::refusal`] with `resource_limits` computed for
/// cgroup limits that are not in effect. When a later step fails as well,
/// the verification failure is returned: the first failure in §24 order
/// is the reason the job does not run.
fn apply_controls(plan: &ExecPlanV1) -> Result<Controls, ExecError> {
    let (cgroup_limits, refusal) = match &plan.cgroup {
        Some(CgroupMembership::Attach(join)) => {
            join_cgroup(join)?;
            (Some(limits_state(join.limits)), None)
        }
        Some(CgroupMembership::Verify(verify)) => match verify_cgroup(verify) {
            Ok(()) => (Some(limits_state(verify.limits)), None),
            Err(error) => (Some(EnforcementState::NotEnforced), Some(error)),
        },
        None => (None, None),
    };
    let mut report = match apply_process_controls(plan) {
        Ok(report) => report,
        Err(error) => return Err(refusal.unwrap_or(error)),
    };
    report.resource_limits = resource_limits_state(cgroup_limits, !plan.rlimits.is_empty());
    Ok(Controls { report, refusal })
}

/// The steps after the cgroup membership: rlimits → sandbox.
fn apply_process_controls(plan: &ExecPlanV1) -> Result<SandboxReport, ExecError> {
    plan.rlimits
        .apply_to_current_process()
        .map_err(|error| ExecError::Rlimit {
            message: error.to_string(),
        })?;
    match &plan.sandbox {
        Some(policy) => apply_to_current_process(policy).map_err(|error| ExecError::Sandbox {
            message: error.to_string(),
        }),
        None => Ok(unsandboxed_report()),
    }
}

/// Unknown cgroup-limit state counts as not enforced.
fn limits_state(limits: Option<EnforcementState>) -> EnforcementState {
    limits.unwrap_or(EnforcementState::NotEnforced)
}

/// Reads `/proc/self/cgroup` (bounded) and checks it against the plan.
fn verify_cgroup(verify: &CgroupVerify) -> Result<(), ExecError> {
    let mut content = String::new();
    let read = File::open(PROC_SELF_CGROUP).and_then(|file| {
        file.take(MAX_PROC_CGROUP_BYTES)
            .read_to_string(&mut content)
    });
    match read {
        Ok(_) => verify.check(&content),
        Err(error) => Err(ExecError::CgroupMismatch {
            expected: verify.expected_proc_path.clone(),
            found: format!("{PROC_SELF_CGROUP} unreadable: {error}"),
        }),
    }
}

fn join_cgroup(join: &CgroupJoin) -> Result<(), ExecError> {
    let cgroup_error = |error: harw_job_linux::CgroupError| ExecError::Cgroup {
        message: error.to_string(),
    };
    let backend = CgroupV2Fs::open(&join.root).map_err(cgroup_error)?;
    let handle = backend.reopen(&join.relative).map_err(cgroup_error)?;
    backend.attach_self(&handle).map_err(cgroup_error)
}

/// Replaces the process image with the job; returns only on failure.
fn exec_job(plan: &ExecPlanV1) -> ExecError {
    let source = Command::new(&plan.program).args(&plan.args).exec();
    ExecError::Exec {
        program: plan.program.clone(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn test_parse_args() -> TestResult {
        let parsed = TrampolineArgs::parse(args(&["--plan", "/p", "--report", "/r"]))
            .map_err(ctx("full"))?;
        assert_eq!(parsed.plan, PathBuf::from("/p"));
        assert_eq!(parsed.report, Some(PathBuf::from("/r")));
        let parsed = TrampolineArgs::parse(args(&["--plan", "/p"])).map_err(ctx("plan only"))?;
        assert_eq!(parsed.report, None);

        for bad in [
            &[][..],
            &["--report", "/r"][..],
            &["--plan"][..],
            &["--plan", ""][..],
            &["--plan", "/p", "--plan", "/q"][..],
            &["--plan", "/p", "extra"][..],
            &["--plan-fd", "3"][..],
        ] {
            match TrampolineArgs::parse(args(bad)) {
                Err(ExecError::Usage { .. }) => {}
                other => return Err(TestError::Unexpected(format!("{bad:?}: {other:?}"))),
            }
        }
        Ok(())
    }

    fn write_plan(dir: &Path, name: &str, mode: u32, bytes: &[u8]) -> TestResult<PathBuf> {
        let path = dir.join(name);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&path)
            .map_err(ctx("create plan"))?;
        file.write_all(bytes).map_err(ctx("write plan"))?;
        Ok(path)
    }

    #[test]
    fn test_load_plan_reads_and_removes() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let plan = ExecPlanV1::new("/bin/true").with_args(["a"]);
        let bytes = plan.to_json().map_err(ctx("json"))?;
        let path = write_plan(dir.path(), "plan.json", 0o600, &bytes)?;
        assert_eq!(load_plan(&path).map_err(ctx("load"))?, plan);
        assert!(!path.exists());
        Ok(())
    }

    #[test]
    fn test_load_plan_rejects_insecure_and_invalid_files() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let bytes = ExecPlanV1::new("/bin/true")
            .to_json()
            .map_err(ctx("json"))?;

        let open = write_plan(dir.path(), "open.json", 0o644, &bytes)?;
        // Independent of the umask: force group/other read.
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o644))
            .map_err(ctx("chmod"))?;
        assert!(matches!(
            load_plan(&open),
            Err(ExecError::PlanInsecure { .. })
        ));
        assert!(!open.exists(), "rejected plan files are removed too");

        let v2 = write_plan(dir.path(), "v2.json", 0o600, br#"{"version":2}"#)?;
        assert!(matches!(
            load_plan(&v2),
            Err(ExecError::UnsupportedVersion { found: 2 })
        ));

        let target = write_plan(dir.path(), "target.json", 0o600, &bytes)?;
        let link = dir.path().join("link.json");
        std::os::unix::fs::symlink(&target, &link).map_err(ctx("symlink"))?;
        assert!(matches!(load_plan(&link), Err(ExecError::PlanIo { .. })));
        assert!(target.exists(), "a symlink target is never consumed");

        let missing = dir.path().join("missing.json");
        assert!(matches!(load_plan(&missing), Err(ExecError::PlanIo { .. })));
        Ok(())
    }

    #[test]
    fn test_report_file_is_exclusive_and_private() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("report.json");
        let file = create_report_file(&path).map_err(ctx("create"))?;
        let mode = file.metadata().map_err(ctx("metadata"))?.mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(matches!(
            create_report_file(&path),
            Err(ExecError::ReportIo { .. })
        ));
        let report = unsandboxed_report();
        write_report(file, &path, &report).map_err(ctx("write"))?;
        assert_eq!(
            crate::report::read_report(&path).map_err(ctx("read"))?,
            Some(report)
        );
        Ok(())
    }

    #[test]
    fn test_apply_controls_without_controls_changes_nothing() -> TestResult {
        // No cgroup, no rlimits, no sandbox: safe to run in the test
        // process (restricts nothing).
        let controls = apply_controls(&ExecPlanV1::new("/bin/true")).map_err(ctx("apply"))?;
        assert!(controls.refusal.is_none());
        assert_eq!(controls.report.filesystem, EnforcementState::NotEnforced);
        assert_eq!(controls.report.resource_limits, EnforcementState::Enforced);
        Ok(())
    }

    fn verify_plan(expected: &str, limits: Option<EnforcementState>) -> ExecPlanV1 {
        ExecPlanV1::new("/bin/true").with_cgroup(CgroupVerify {
            expected_proc_path: PathBuf::from(expected),
            limits,
        })
    }

    #[test]
    fn test_apply_controls_verifies_the_own_cgroup() -> TestResult {
        // Verify only reads /proc/self/cgroup: safe in the test process.
        let content = std::fs::read_to_string(PROC_SELF_CGROUP).map_err(ctx("read cgroup"))?;
        let wrong = verify_plan(
            "/harw-job-exec-test/not-this-cgroup",
            Some(EnforcementState::Enforced),
        );
        let controls = apply_controls(&wrong).map_err(ctx("apply wrong"))?;
        match &controls.refusal {
            Some(ExecError::CgroupMismatch { expected, .. }) => {
                assert_eq!(expected, Path::new("/harw-job-exec-test/not-this-cgroup"));
            }
            other => return Err(TestError::Unexpected(format!("{other:?}"))),
        }
        // Recorded: the supervisor's `Enforced` limits do not count.
        assert_eq!(
            controls.report.resource_limits,
            EnforcementState::NotEnforced
        );

        // Skipped, not passed: a cgroup v1-only host has no unified entry
        // to match, and a process at the root (`0::/`, e.g. at a cgroup
        // namespace root) has no job cgroup a plan may name.
        let Some(own) = crate::plan::unified_cgroup_path(&content).filter(|own| *own != "/") else {
            eprintln!("skipped: own cgroup {content:?} is not a verifiable job cgroup");
            return Ok(());
        };
        let right = verify_plan(own, Some(EnforcementState::Enforced));
        let controls = apply_controls(&right).map_err(ctx("apply right"))?;
        assert!(controls.refusal.is_none(), "{:?}", controls.refusal);
        assert_eq!(controls.report.resource_limits, EnforcementState::Enforced);
        let unknown = apply_controls(&verify_plan(own, None)).map_err(ctx("apply unknown"))?;
        assert_eq!(
            unknown.report.resource_limits,
            EnforcementState::NotEnforced
        );
        Ok(())
    }

    #[test]
    fn test_refused_membership_outranks_a_failed_report_write() -> TestResult {
        let mismatch = || ExecError::CgroupMismatch {
            expected: PathBuf::from("/harw.slice/harw-job-1"),
            found: "/user.slice".to_owned(),
        };
        let write_failed = || {
            Err(ExecError::ReportIo {
                path: PathBuf::from("/r"),
                source: io::Error::from(io::ErrorKind::StorageFull),
            })
        };
        assert!(matches!(
            refusal_first(Some(mismatch()), write_failed()),
            Err(ExecError::CgroupMismatch { .. })
        ));
        assert!(matches!(
            refusal_first(Some(mismatch()), Ok(())),
            Err(ExecError::CgroupMismatch { .. })
        ));
        assert!(matches!(
            refusal_first(None, write_failed()),
            Err(ExecError::ReportIo { .. })
        ));
        refusal_first(None, Ok(())).map_err(ctx("nothing failed"))?;
        Ok(())
    }
}
