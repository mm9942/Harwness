//! The trampoline itself: plan → cgroup → rlimits → sandbox → report →
//! requirement check → `exec` (Job-Runtime-Doc §24).
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
use crate::plan::{CgroupJoin, ExecPlanV1, MAX_PLAN_BYTES};
use crate::report::{check_requirement, render_report, resource_limits_state, unsandboxed_report};

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
    let report = apply_controls(&plan)?;
    if let Some((file, path)) = report_file {
        write_report(file, path, &report)?;
    }
    check_requirement(plan.sandbox_requirement, &report)?;
    Err(exec_job(&plan))
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

/// Applies the controls to the calling process in the §24 order and
/// returns the merged report: cgroup join → rlimits → sandbox.
///
/// cgroup first so that every later allocation is already accounted to the
/// job; rlimits before the sandbox because Landlock/capability drop do not
/// affect `setrlimit` but a dropped `CAP_SYS_RESOURCE` would; the sandbox
/// last because `restrict_self` is irreversible and must see the final
/// state.
fn apply_controls(plan: &ExecPlanV1) -> Result<SandboxReport, ExecError> {
    let cgroup_limits = match &plan.cgroup {
        Some(join) => {
            join_cgroup(join)?;
            Some(join.limits.unwrap_or(EnforcementState::NotEnforced))
        }
        None => None,
    };
    plan.rlimits
        .apply_to_current_process()
        .map_err(|error| ExecError::Rlimit {
            message: error.to_string(),
        })?;
    let mut report = match &plan.sandbox {
        Some(policy) => apply_to_current_process(policy).map_err(|error| ExecError::Sandbox {
            message: error.to_string(),
        })?,
        None => unsandboxed_report(),
    };
    report.resource_limits = resource_limits_state(cgroup_limits, !plan.rlimits.is_empty());
    Ok(report)
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
        let report = apply_controls(&ExecPlanV1::new("/bin/true")).map_err(ctx("apply"))?;
        assert_eq!(report.filesystem, EnforcementState::NotEnforced);
        assert_eq!(report.resource_limits, EnforcementState::Enforced);
        Ok(())
    }
}
