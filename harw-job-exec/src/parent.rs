//! Supervisor side: write the plan file and build the trampoline command.
//!
//! ```text
//! let mut launch = TrampolineCommand::new(trampoline_bin, &plan, private_dir)?;
//! launch.command_mut().current_dir(workdir).stdout(Stdio::piped());
//! let child = launch.spawn()?;          // then pidfd_open, persist, ...
//! // after the job exited (or later): launch.read_report()?
//! ```
//!
//! # Directory requirements
//! `dir` should be private to the supervisor's user (mode `0700`): plan and
//! report files are created `0600` there, and the report name is only
//! reserved, not created, until the trampoline creates it with `O_EXCL`.
//! In a shared directory another user could pre-create the report name and
//! make the launch fail (the trampoline never writes into a file it did not
//! create).

use std::fs::OpenOptions;
use std::io::{self, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use harw_job_core::SandboxReport;

use crate::error::ExecError;
use crate::plan::ExecPlanV1;
use crate::report::read_report;

/// Attempts to find an unused file name before giving up.
const NAME_ATTEMPTS: u32 = 16;

/// Per-process sequence number for plan file names.
static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A prepared trampoline launch: the plan file is written, the command is
/// built (`<trampoline> --plan <plan> --report <report>`), nothing runs yet.
///
/// The plan file is **not** removed on drop: the trampoline may still be
/// about to read it. Call [`TrampolineCommand::discard_plan`] when the
/// command is never spawned; [`TrampolineCommand::spawn`] does so itself on
/// a spawn failure.
#[derive(Debug)]
pub struct TrampolineCommand {
    command: Command,
    plan_path: PathBuf,
    report_path: PathBuf,
}

impl TrampolineCommand {
    /// Validates `plan`, writes it as a `0600` file into `dir` and builds
    /// the command for the trampoline binary at `trampoline`.
    ///
    /// # Errors
    /// The errors of [`ExecPlanV1::to_json`]; [`ExecError::PlanIo`] when
    /// the plan file cannot be created.
    pub fn new(trampoline: &Path, plan: &ExecPlanV1, dir: &Path) -> Result<Self, ExecError> {
        let bytes = plan.to_json()?;
        let (plan_path, report_path) = write_plan_file(dir, &bytes)?;
        let mut command = Command::new(trampoline);
        command
            .arg("--plan")
            .arg(&plan_path)
            .arg("--report")
            .arg(&report_path);
        Ok(Self {
            command,
            plan_path,
            report_path,
        })
    }

    /// The command, for stdio, working directory and environment. Do not
    /// add arguments: the trampoline accepts only `--plan`/`--report`.
    pub fn command_mut(&mut self) -> &mut Command {
        &mut self.command
    }

    /// Path of the plan file (deleted by the trampoline once read).
    #[must_use]
    pub fn plan_path(&self) -> &Path {
        &self.plan_path
    }

    /// Path the trampoline writes its report to.
    #[must_use]
    pub fn report_path(&self) -> &Path {
        &self.report_path
    }

    /// Spawns the trampoline. On failure the plan file is removed.
    ///
    /// # Errors
    /// The spawn error.
    pub fn spawn(&mut self) -> io::Result<Child> {
        self.command.spawn().inspect_err(|_| self.discard_plan())
    }

    /// Removes the plan file if it still exists (never spawned, or spawn
    /// failed). Best effort: a missing file is the normal case.
    pub fn discard_plan(&self) {
        // Intentionally ignored: NotFound means the trampoline consumed it;
        // any other failure leaves a 0600 file in the private directory.
        let _ = std::fs::remove_file(&self.plan_path);
    }

    /// Reads the trampoline's report ([`read_report`]).
    ///
    /// # Errors
    /// As [`read_report`].
    pub fn read_report(&self) -> Result<Option<SandboxReport>, ExecError> {
        read_report(&self.report_path)
    }

    /// Removes the report file (after it was read). Best effort.
    pub fn remove_report(&self) {
        // Intentionally ignored: the file may never have been created.
        let _ = std::fs::remove_file(&self.report_path);
    }
}

/// Creates `<dir>/harw-job-exec-<pid>-<nanos>-<seq>.plan.json` with mode
/// `0600` (`O_EXCL`) and reserves the matching `.report.json` name.
fn write_plan_file(dir: &Path, bytes: &[u8]) -> Result<(PathBuf, PathBuf), ExecError> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    let mut last_path = dir.to_path_buf();
    for _ in 0..NAME_ATTEMPTS {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let stem = format!("harw-job-exec-{}-{nanos:09}-{sequence}", std::process::id());
        let plan_path = dir.join(format!("{stem}.plan.json"));
        let report_path = dir.join(format!("{stem}.report.json"));
        if report_path.symlink_metadata().is_ok() {
            last_path = report_path;
            continue;
        }
        let mut file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&plan_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                last_path = plan_path;
                continue;
            }
            Err(source) => {
                return Err(ExecError::PlanIo {
                    path: plan_path,
                    source,
                });
            }
        };
        if let Err(source) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            drop(file);
            // Intentionally ignored: the write error is the one to report.
            let _ = std::fs::remove_file(&plan_path);
            return Err(ExecError::PlanIo {
                path: plan_path,
                source,
            });
        }
        return Ok((plan_path, report_path));
    }
    Err(ExecError::PlanIo {
        path: last_path,
        source: io::Error::new(io::ErrorKind::AlreadyExists, "no unused plan file name"),
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::os::unix::fs::PermissionsExt as _;

    use super::*;
    use crate::test_support::{TestResult, ctx};

    #[test]
    fn test_new_writes_private_plan_and_builds_command() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let plan = ExecPlanV1::new("/bin/sh").with_args(["-c", "exit 7"]);
        let launch = TrampolineCommand::new(Path::new("/opt/harw-job-exec"), &plan, dir.path())
            .map_err(ctx("new"))?;

        let metadata = std::fs::metadata(launch.plan_path()).map_err(ctx("metadata"))?;
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        let bytes = std::fs::read(launch.plan_path()).map_err(ctx("read plan"))?;
        assert_eq!(ExecPlanV1::from_json(&bytes).map_err(ctx("parse"))?, plan);
        assert!(!launch.report_path().exists());
        assert_eq!(launch.read_report().map_err(ctx("report"))?, None);

        let args: Vec<&OsStr> = launch.command.get_args().collect();
        let expected = vec![
            OsStr::new("--plan"),
            launch.plan_path().as_os_str(),
            OsStr::new("--report"),
            launch.report_path().as_os_str(),
        ];
        assert_eq!(args, expected);
        assert_eq!(
            launch.command.get_program(),
            OsStr::new("/opt/harw-job-exec")
        );

        launch.discard_plan();
        assert!(!launch.plan_path().exists());
        Ok(())
    }

    #[test]
    fn test_new_rejects_invalid_plan_without_writing() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let result = TrampolineCommand::new(Path::new("/x"), &ExecPlanV1::new(""), dir.path());
        assert!(matches!(result, Err(ExecError::InvalidPlan { .. })));
        let entries = std::fs::read_dir(dir.path())
            .map_err(ctx("read_dir"))?
            .count();
        assert_eq!(entries, 0);
        Ok(())
    }

    #[test]
    fn test_names_are_unique() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let plan = ExecPlanV1::new("/bin/true");
        let first = TrampolineCommand::new(Path::new("/x"), &plan, dir.path()).map_err(ctx("1"))?;
        let second =
            TrampolineCommand::new(Path::new("/x"), &plan, dir.path()).map_err(ctx("2"))?;
        assert_ne!(first.plan_path(), second.plan_path());
        assert_ne!(first.report_path(), second.report_path());
        Ok(())
    }
}
