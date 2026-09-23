//! Sudo boundary using duplicated pidfds, not a second process search.
//! The parent holds all descriptors while synchronously waiting for the helper.
//! No worker threads or locks. Kernel, transport and subprocess failures return
//! structured errors; there is deliberately no numeric-PID fallback.
//! Binary-crate examples are illustrative and are not executed by Cargo doctests.
//! # Examples
//! ```no_run
//! std::process::Command::new("killer").args(["--pid", "1234", "--no-sudo", "--dry-run"]).status()?;
//! # Ok::<(), std::io::Error>(())
//! ```
use crate::{
    cli::Cli,
    engine,
    error::{Error, Result},
    pidfd::PidFd,
    process,
    types::{Outcome, Target},
};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

/// Subprocess boundary that can be substituted in pure tests.
pub(crate) trait CommandRunner {
    /// Borrow program/arguments and wait for captured stdout with inherited stderr.
    /// # Errors
    /// Io if spawning or waiting fails; ownership of arguments remains with caller.
    /// # Returns and concurrency
    /// An owned child status and captured output. Implementations must wait for
    /// child completion before returning, so borrowed target handles stay alive.
    /// The native implementation blocks the calling thread and creates no workers.
    /// # Examples
    /// ```no_run
    /// use crate::privilege::CommandRunner;
    /// let output = crate::privilege::SystemRunner.output(
    ///     std::path::Path::new("/usr/bin/sudo"), &["--version".into()])?;
    /// # Ok::<(), crate::error::Error>(())
    /// ```
    fn output(&self, program: &Path, args: &[OsString]) -> Result<Output>;
}
/// Executes the actual sudo command synchronously.
pub(crate) struct SystemRunner;
/// Native subprocess implementation; no shell interpolation is used.
impl CommandRunner for SystemRunner {
    /// Implements the trait contract with inherited stdin/stderr and owned stdout.
    fn output(&self, program: &Path, args: &[OsString]) -> Result<Output> {
        Command::new(program)
            .args(args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| Error::io("execute sudo helper", Some(program), e))
    }
}
// Resolve sudo only in standard system locations, never in a user-controlled PATH.
fn sudo_path() -> Result<PathBuf> {
    for path in ["/usr/bin/sudo", "/bin/sudo"] {
        let path = Path::new(path);
        match path.try_exists() {
            Ok(true) => return Ok(path.to_path_buf()),
            Ok(false) => {}
            Err(e) => return Err(Error::io("locate sudo", Some(path), e)),
        }
    }
    Err(Error::Helper {
        reason: "sudo is not installed in /usr/bin or /bin".to_owned(),
    })
}
/// Terminate only the supplied foreign handles via the privileged helper.
/// Borrows targets so their descriptors stay open for the complete child lifetime.
/// # Errors
/// Io for kernel/subprocess failures or resolving the executable/sudo path;
/// ProcFormat/Parse for parent metadata; Helper or Json for invalid helper output.
/// # Arguments and ownership
/// `targets` borrows the previously selected foreign-owned targets. `cli` borrows
/// wait/escalation options. `runner` borrows the synchronous process boundary.
/// Descriptors must remain open throughout this call; no selection is repeated.
/// # Returns and concurrency
/// Owned reports, one per target in input order. Blocks through sudo interaction
/// and termination; creates no worker threads or locks of its own.
/// # Examples
/// The corresponding user-facing path requests elevation only for foreign owners:
/// ```no_run
/// std::process::Command::new("killer").args(["--pid", "1234", "--yes"]).status()?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub(crate) fn terminate_foreign<R: CommandRunner>(
    targets: &[&Target],
    cli: &Cli,
    runner: &R,
) -> Result<Vec<Outcome>> {
    let executable =
        std::env::current_exe().map_err(|e| Error::io("resolve killer executable", None, e))?;
    let parent = process::read(std::process::id())?;
    let mut args = vec![
        OsString::from("--"),
        executable.into_os_string(),
        OsString::from("--timeout"),
        OsString::from(cli.timeout.as_secs_f64().to_string()),
        OsString::from("--kill-wait"),
        OsString::from(cli.kill_wait.as_secs_f64().to_string()),
        OsString::from("--log"),
        OsString::from(cli.log.to_string()),
    ];
    args.extend([
        OsString::from("--helper"),
        OsString::from(parent.pid.to_string()),
        OsString::from(parent.start_ticks.to_string()),
    ]);
    args.extend(
        targets
            .iter()
            .map(|t| OsString::from(format!("{}:{}", t.process.pid, t.fd.raw()))),
    );
    tracing::info!(count = targets.len(), "requesting sudo for foreign targets");
    decode(runner.output(&sudo_path()?, &args)?, targets)
}
// Validate subprocess status and the one-to-one report mapping before merging.
fn decode(output: Output, targets: &[&Target]) -> Result<Vec<Outcome>> {
    if output.stdout.is_empty() {
        return Err(Error::Helper {
            reason: format!("sudo/helper returned {} without a report", output.status),
        });
    }
    let reports: Vec<Outcome> = serde_json::from_slice(&output.stdout)?;
    if reports.len() != targets.len()
        || reports
            .iter()
            .zip(targets)
            .any(|(r, t)| r.pid != t.process.pid)
    {
        return Err(Error::Helper {
            reason: "helper target/report mismatch".to_owned(),
        });
    }
    let expected = if reports.iter().all(Outcome::success) {
        0
    } else {
        1
    };
    if output.status.code() != Some(expected) {
        return Err(Error::Helper {
            reason: format!("helper exit status {} disagrees with report", output.status),
        });
    }
    Ok(reports)
}
// Parse a numeric helper value while retaining exact field and cause.
fn number<T>(value: &str, field: &'static str) -> Result<T>
where
    T: std::str::FromStr<Err = std::num::ParseIntError>,
{
    value.parse().map_err(|source| Error::Parse {
        field,
        input: value.to_owned(),
        source,
    })
}
/// Run the internal helper only as root, copying held descriptors before signals.
/// The parent process start time is checked after opening its pidfd; its pidfd
/// must still be alive. Actual target identity comes from duplicated descriptors.
/// # Errors
/// Permission for non-root; InvalidInput/Parse for protocol errors; Io for policy
/// restrictions (notably CAP_SYS_PTRACE/seccomp) and IdentityChanged for stale parent.
/// Helper covers missing/malformed protocol items and invalid descriptor events;
/// ProcFormat covers malformed parent metadata. Target signalling failures become
/// per-target `Outcome` values rather than failing the complete returned vector.
/// # Arguments and ownership
/// Borrows parsed `cli`; the caller must supply the private protocol generated by
/// `terminate_foreign`. Acquires owned duplicated descriptors and drops them only
/// after the synchronous engine finishes. No numeric-PID signalling is used.
/// # Returns and concurrency
/// Owned per-target reports in protocol order. Blocks for configured wait bounds;
/// no threads or locks are started here. This is an internal root-only entrypoint,
/// not a separate authorization mechanism for untrusted requests.
/// # Examples
/// Let the normal CLI construct and authorize the private helper protocol:
/// ```no_run
/// std::process::Command::new("killer").args(["--pid", "1234", "--yes"]).status()?;
/// # Ok::<(), std::io::Error>(())
/// ```
pub(crate) fn helper(cli: &Cli) -> Result<Vec<Outcome>> {
    if cli.dry_run {
        return Err(Error::Permission {
            reason: "helper mode cannot be combined with dry-run",
        });
    }
    if !rustix::process::geteuid().is_root() {
        return Err(Error::Permission {
            reason: "internal helper requires root",
        });
    }
    let mut args = cli.helper.iter();
    let parent_text = args.next().ok_or_else(|| Error::Helper {
        reason: "missing parent PID".to_owned(),
    })?;
    let start_text = args.next().ok_or_else(|| Error::Helper {
        reason: "missing parent start time".to_owned(),
    })?;
    let parent_id = number::<u32>(parent_text, "parent PID")?;
    let start = number::<u64>(start_text, "parent start time")?;
    let parent = PidFd::open(parent_id)?;
    if process::read(parent_id)?.start_ticks != start || parent.exited()? {
        return Err(Error::IdentityChanged { pid: parent_id });
    }
    let protected = process::protected()?;
    let mut handles = Vec::new();
    for token in args {
        let (pid, fd) = token.split_once(':').ok_or_else(|| Error::Helper {
            reason: "expected PID:FD".to_owned(),
        })?;
        let pid = number::<u32>(pid, "target PID")?;
        let fd = number::<i32>(fd, "parent descriptor")?;
        if pid <= 1 || fd < 0 {
            return Err(Error::Helper {
                reason: "invalid target PID or descriptor".to_owned(),
            });
        }
        let handle = PidFd::duplicate_from(&parent, fd)?;
        let path = PathBuf::from(format!("/proc/self/fdinfo/{}", handle.raw()));
        let info = std::fs::read_to_string(&path)
            .map_err(|source| Error::io("read duplicated pidfd identity", Some(&path), source))?;
        validate_handle(&info, pid, &protected)?;
        handles.push((pid, handle));
    }
    if handles.is_empty() {
        return Err(Error::Helper {
            reason: "empty helper target set".to_owned(),
        });
    }
    let borrowed: Vec<_> = handles.iter().map(|(pid, fd)| (*pid, fd)).collect();
    Ok(engine::terminate(&borrowed, cli.timeout, cli.kill_wait))
}
// Verify that kernel fdinfo identifies the same unprotected process label.
// Pid=-1 means the held process has been reaped; no new process can receive its signals.
fn validate_handle(
    info: &str,
    expected: u32,
    protected: &std::collections::HashSet<u32>,
) -> Result<()> {
    let actual = info
        .lines()
        .find_map(|line| line.strip_prefix("Pid:"))
        .ok_or_else(|| Error::Helper {
            reason: "received descriptor is not a pidfd".to_owned(),
        })?;
    let actual = number::<i32>(actual.trim(), "pidfd PID")?;
    if protected.contains(&expected) || expected <= 1 {
        return Err(Error::Permission {
            reason: "helper target is protected",
        });
    }
    if actual != -1 && i64::from(actual) != i64::from(expected) {
        return Err(Error::IdentityChanged { pid: expected });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    // Reject non-pidfds and misleading target labels before any signal is issued.
    #[test]
    fn test_validate_handle_rejects_wrong_and_protected_identity() {
        let protected = std::collections::HashSet::from([1, 55]);
        assert!(validate_handle("Pid:\t123\n", 123, &protected).is_ok());
        assert!(validate_handle("Pid:\t-1\n", 123, &protected).is_ok());
        assert!(matches!(
            validate_handle("Pid:\t1\n", 123, &protected),
            Err(Error::IdentityChanged { .. })
        ));
        assert!(matches!(
            validate_handle("Pid:\t55\n", 55, &protected),
            Err(Error::Permission { .. })
        ));
        assert!(matches!(
            validate_handle("pos:\t0\n", 123, &protected),
            Err(Error::Helper { .. })
        ));
    }

    // Stub proves the boundary accepts substitute subprocess implementations.
    struct Stub;
    impl CommandRunner for Stub {
        fn output(&self, program: &Path, args: &[OsString]) -> Result<Output> {
            assert_eq!(program, Path::new("/usr/bin/sudo"));
            assert_eq!(args, [OsString::from("--")]);
            Ok(Output {
                status: std::process::ExitStatus::from_raw(0),
                stdout: b"[]".to_vec(),
                stderr: Vec::new(),
            })
        }
    }
    #[test]
    fn test_decode_validates_helper_protocol() {
        let output = Stub
            .output(Path::new("/usr/bin/sudo"), &[OsString::from("--")])
            .unwrap();
        assert!(decode(output, &[]).unwrap().is_empty());
        let output = Output {
            status: std::process::ExitStatus::from_raw(256),
            stdout: b"[]".to_vec(),
            stderr: Vec::new(),
        };
        assert!(matches!(decode(output, &[]), Err(Error::Helper { .. })));
        assert!(matches!(
            number::<u32>("bad", "pid"),
            Err(Error::Parse { field: "pid", .. })
        ));
    }
}
