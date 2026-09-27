//! Exit codes of the `harw-job-exec` binary for its own failures.
//!
//! They follow the `env(1)` / `chroot(1)` convention. Every other status is
//! the job's own exit status (the trampoline `exec`s the job, it never
//! waits for it).

/// The trampoline could not prepare the launch: bad arguments, an invalid
/// or insecure plan file, a report file that could not be created, a cgroup
/// join or `setrlimit` that failed, or an unsupported platform. The job was
/// not started.
pub const SETUP_FAILED: u8 = 125;

/// The sandbox could not be applied, or the achieved
/// [`harw_job_core::SandboxReport`] does not satisfy the plan's
/// [`harw_job_core::SandboxRequirement`]. The job was not started.
pub const SANDBOX_REFUSED: u8 = 126;

/// Every control was applied, but `execve` of the job program failed (not
/// found, not executable, denied by Landlock, ...).
pub const EXEC_FAILED: u8 = 127;
