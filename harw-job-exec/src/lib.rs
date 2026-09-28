//! `harw-job-exec` — the safe re-exec trampoline of the job runtime
//! (Job-Runtime-Doc §9-§12, §24).
//!
//! # Why a trampoline
//! Some controls must be in place **before** the job's first instruction:
//! cgroup membership, rlimits, `NO_NEW_PRIVS`, the capability drop and the
//! Landlock domain. `std::process::Command` offers exactly one hook between
//! `fork` and `exec` — `pre_exec` — and it is `unsafe` (only
//! async-signal-safe code may run there). The workspace forbids `unsafe`.
//!
//! Instead the supervisor spawns this small, single-threaded binary. It
//! applies every control **to itself** with ordinary safe code and then
//! replaces itself with the job via [`std::os::unix::process::CommandExt::exec`].
//! All controls are inherited across `execve`, so the job never runs a
//! single instruction without them.
//!
//! # Flow of the binary (Linux only)
//! 1. Parse `--plan <path> [--report <path>]`.
//! 2. Open the plan file without following symlinks, check it is a regular
//!    file owned by the effective uid with mode `0600` (no group/other
//!    bits), read at most [`plan::MAX_PLAN_BYTES`], **delete it**, parse
//!    and validate the [`plan::ExecPlanV1`].
//! 3. Create the report file (`O_CREAT|O_EXCL|O_NOFOLLOW`, `0600`) while
//!    the process is still unrestricted.
//! 4. Security-sensitive ordering (§24): join the job cgroup
//!    (`CgroupBackend::attach_self`) → `setrlimit` → sandbox
//!    (`NO_NEW_PRIVS`, capability drop, Landlock `restrict_self`).
//! 5. Write the [`harw_job_core::SandboxReport`] into the already open
//!    report file.
//! 6. Check the plan's [`harw_job_core::SandboxRequirement`]; if it is not
//!    met, exit [`exit_code::SANDBOX_REFUSED`] without ever running the job.
//! 7. `exec` the job program with its arguments. The environment, working
//!    directory and stdio are inherited from the supervisor's `Command`.
//!
//! # Why files and not inherited descriptors
//! Passing the plan over an inherited pipe fd (`--plan-fd N`) would require
//! adopting a raw descriptor number (`File::from_raw_fd`), which is
//! `unsafe`. An environment variable would leak the plan into
//! `/proc/<pid>/environ` of the job unless every exec path scrubbed it. The
//! plan is therefore a `0600` file in a directory the supervisor owns; the
//! trampoline verifies owner and mode and deletes it before doing anything
//! else. The report travels the same way in the opposite direction.
//!
//! The report file is opened **before** the sandbox is applied and written
//! **after** it: Landlock restricts `open(2)`, not `write(2)` on an already
//! open descriptor, so this works even when the report directory is outside
//! the job's writable paths. It is not written via rename (a rename after
//! `restrict_self` could be denied); instead the JSON document ends with a
//! newline that marks it complete — [`report::read_report`] returns
//! `Ok(None)` until that newline is present.
//!
//! # Exit codes
//! See [`exit_code`]: `125` setup failed, `126` sandbox refused, `127`
//! exec failed (the `env(1)` convention). Any other status is the job's
//! own. A job that itself exits with 125..=127 is indistinguishable by
//! status alone; the trampoline additionally prints a typed message on
//! stderr for its own failures.
//!
//! # Platform
//! The plan types and the parent-side builder exist on Linux only. On other
//! targets [`run_trampoline`] fails with
//! [`ExecError::UnsupportedPlatform`] and the binary exits `125`.

#![forbid(unsafe_code)]

pub mod error;
pub mod exit_code;
#[cfg(target_os = "linux")]
pub mod parent;
#[cfg(target_os = "linux")]
pub mod plan;
#[cfg(target_os = "linux")]
pub mod report;
#[cfg(target_os = "linux")]
mod trampoline;

#[cfg(all(test, target_os = "linux"))]
mod test_support;

pub use error::ExecError;
#[cfg(target_os = "linux")]
pub use parent::TrampolineCommand;
#[cfg(target_os = "linux")]
pub use plan::{CgroupJoin, ExecPlanV1, MAX_PLAN_BYTES, PLAN_VERSION};
#[cfg(target_os = "linux")]
pub use report::{check_requirement, read_report, resource_limits_state, unsandboxed_report};
#[cfg(target_os = "linux")]
pub use trampoline::{TrampolineArgs, run_trampoline};

/// On targets other than Linux the trampoline cannot apply any control and
/// refuses to run.
///
/// # Errors
/// Always [`ExecError::UnsupportedPlatform`].
#[cfg(not(target_os = "linux"))]
pub fn run_trampoline<I>(args: I) -> Result<std::convert::Infallible, ExecError>
where
    I: IntoIterator<Item = std::ffi::OsString>,
{
    drop(args.into_iter());
    Err(ExecError::UnsupportedPlatform)
}
