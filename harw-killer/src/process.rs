//! Read procfs metadata, match selectors, and bind selections to kernel handles.
//!
//! Metadata is an owned observation, not an atomic kernel snapshot. Selection
//! brackets pidfd acquisition with identity checks and retains the handle. This
//! module creates no threads or locks and never sends a signal. Optional metadata
//! failures are logged; missing required identity data prevents selection.
//!
//! # Examples
//! ```no_run
//! let status = std::process::Command::new("killer")
//!     .args(["-p", "cargo", "--dry-run", "--uid", "1000"])
//!     .status()?;
//! # Ok::<(), std::io::Error>(())
//! ```

use crate::{
    cli::Cli,
    error::{Error, Result},
    pidfd::PidFd,
    types::{Process, Target},
};
use std::{collections::HashSet, fs, io, path::Path};

/// Parse PPID, kernel start ticks and comm without splitting the comm contents.
///
/// The final closing parenthesis delimits comm, which may itself contain spaces,
/// parentheses or newlines. Input is borrowed; only the resulting comm is owned.
/// # Errors
/// Returns ProcFormat for missing fields, Parse for invalid integers, and
/// IdentityChanged if the record identifies a different PID.
/// # Returns and ownership
/// Returns owned `(parent_pid, start_ticks, comm)` values; `text` remains borrowed.
/// Performs no I/O and creates no threads or locks.
/// # Examples
/// ```
/// let record = format!("42 (cargo) S 7 {} 123", ["0"; 17].join(" "));
/// let (parent, ticks, name) = crate::process::stat(42, &record)?;
/// assert_eq!((parent, ticks, name.as_str()), (7, 123, "cargo"));
/// # Ok::<(), crate::error::Error>(())
/// ```
pub(crate) fn stat(pid: u32, text: &str) -> Result<(u32, u64, String)> {
    let open = text.find('(').ok_or(Error::ProcFormat {
        pid,
        field: "comm opening delimiter",
    })?;
    let close = text.rfind(')').ok_or(Error::ProcFormat {
        pid,
        field: "comm closing delimiter",
    })?;
    if close <= open {
        return Err(Error::ProcFormat {
            pid,
            field: "comm delimiters",
        });
    }
    let input = text[..open].trim();
    let record_pid: u32 = input.parse().map_err(|source| Error::Parse {
        field: "stat PID",
        input: input.to_owned(),
        source,
    })?;
    if record_pid != pid {
        return Err(Error::IdentityChanged { pid });
    }
    let mut fields = text[close + 1..].split_whitespace();
    let state = fields.next().ok_or(Error::ProcFormat {
        pid,
        field: "state",
    })?;
    if state.len() != 1 {
        return Err(Error::ProcFormat {
            pid,
            field: "state",
        });
    }
    let ppid_input = fields
        .next()
        .ok_or(Error::ProcFormat { pid, field: "PPID" })?;
    let ppid = ppid_input.parse().map_err(|source| Error::Parse {
        field: "PPID",
        input: ppid_input.to_owned(),
        source,
    })?;
    // After state (field 3) and PPID (field 4), starttime (field 22) is offset 17.
    let start_input = fields.nth(17).ok_or(Error::ProcFormat {
        pid,
        field: "start ticks",
    })?;
    let start_ticks = start_input.parse().map_err(|source| Error::Parse {
        field: "start ticks",
        input: start_input.to_owned(),
        source,
    })?;
    Ok((ppid, start_ticks, text[open + 1..close].to_owned()))
}

/// Read required identity fields and best-effort display metadata for one PID.
///
/// Executable access may be denied for foreign owners or absent for kernel
/// threads; then the kernel comm is used. Unreadable argv becomes an empty string,
/// so command-line selectors cannot accidentally match the executable fallback.
/// # Errors
/// Required stat/status I/O and parsing failures are contextual application errors.
/// # Returns and ownership
/// Returns an owned process snapshot; the numeric PID is only an inspection key.
/// Blocking procfs reads execute on the calling thread without locks or workers.
/// # Examples
/// ```no_run
/// let snapshot = crate::process::read(std::process::id())?;
/// assert_eq!(snapshot.pid, std::process::id());
/// # Ok::<(), crate::error::Error>(())
/// ```
pub(crate) fn read(pid: u32) -> Result<Process> {
    let base = Path::new("/proc").join(pid.to_string());
    let stat_path = base.join("stat");
    let contents = fs::read_to_string(&stat_path)
        .map_err(|source| Error::io("read process stat", Some(&stat_path), source))?;
    let (ppid, start_ticks, comm) = stat(pid, &contents)?;
    let status_path = base.join("status");
    let status = fs::read_to_string(&status_path)
        .map_err(|source| Error::io("read process status", Some(&status_path), source))?;
    let input = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|line| line.split_whitespace().nth(1))
        .ok_or(Error::ProcFormat {
            pid,
            field: "effective UID",
        })?;
    let uid = input.parse().map_err(|source| Error::Parse {
        field: "effective UID",
        input: input.to_owned(),
        source,
    })?;
    let exe_path = base.join("exe");
    let executable = match fs::read_link(&exe_path) {
        Ok(path) => Some(path.to_string_lossy().into_owned()),
        Err(source)
            if matches!(
                source.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied
            ) =>
        {
            tracing::debug!(pid, error = %source, "executable unavailable; using kernel comm");
            None
        }
        Err(source) => {
            return Err(Error::io(
                "read process executable",
                Some(&exe_path),
                source,
            ));
        }
    };
    let name = executable
        .as_ref()
        .and_then(|path| Path::new(path.strip_suffix(" (deleted)").unwrap_or(path)).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or(comm);
    let command_path = base.join("cmdline");
    let command = match fs::read(&command_path) {
        Ok(bytes) => String::from_utf8_lossy(&bytes)
            .trim_end_matches('\0')
            .replace('\0', " "),
        Err(source) if source.kind() == io::ErrorKind::PermissionDenied => {
            tracing::debug!(pid, error = %source, "command line unavailable; full matching disabled for this process");
            String::new()
        }
        Err(source) => {
            return Err(Error::io(
                "read process command line",
                Some(&command_path),
                source,
            ));
        }
    };
    Ok(Process {
        pid,
        ppid,
        uid,
        start_ticks,
        name,
        executable,
        command,
    })
}

/// Protect PID 1, this application and every observable invoking ancestor.
/// # Errors
/// An unreadable ancestor or cyclic ancestry fails closed instead of returning an
/// incomplete protection set. A namespace root with PPID zero ends the chain.
/// # Returns and ownership
/// Returns an owned set of observed ancestor PIDs, including this process and 1.
/// Ancestry is an observation, not a guarantee against later reparenting. Procfs
/// reads block the calling thread; no locks or worker threads are created.
/// # Examples
/// ```no_run
/// let ids = crate::process::protected()?;
/// assert!(ids.contains(&1) && ids.contains(&std::process::id()));
/// # Ok::<(), crate::error::Error>(())
/// ```
pub(crate) fn protected() -> Result<HashSet<u32>> {
    let mut ids = HashSet::from([1]);
    let mut pid = std::process::id();
    while pid > 1 {
        if !ids.insert(pid) {
            return Err(Error::ProcFormat {
                pid,
                field: "acyclic process ancestry",
            });
        }
        pid = read(pid)?.ppid;
    }
    Ok(ids)
}

/// Compare a borrowed metadata snapshot against UID and selector constraints.
/// Names and PIDs are additive; the optional UID constraint applies to both.
/// # Returns and ownership
/// Returns whether a name or PID matches and the UID filter permits it.
/// Borrows every input; no I/O, allocation, locks or threads.
/// # Examples
/// ```no_run
/// use clap::Parser;
/// let cli = crate::cli::Cli::parse_from(["killer", "-p", "cargo"]);
/// let snapshot = crate::process::read(std::process::id())?;
/// let selected = crate::process::matches(&cli, &snapshot);
/// # Ok::<(), crate::error::Error>(())
/// ```
pub(crate) fn matches(cli: &Cli, process: &Process) -> bool {
    if cli.uid.is_some_and(|uid| uid != process.uid) {
        return false;
    }
    cli.pid.contains(&process.pid) || cli.process.iter().any(|name| name == &process.name)
}

/// Obtain deduplicated candidates from procfs or the explicit CLI PID list.
/// Directory names that are not numeric are metadata entries, not process errors.
/// # Errors
/// Directory traversal failures and malformed explicit PIDs are reported.
fn candidate_ids(cli: &Cli) -> Result<Vec<u32>> {
    let mut ids = if !cli.process.is_empty() {
        let directory = Path::new("/proc");
        let entries = fs::read_dir(directory)
            .map_err(|source| Error::io("enumerate processes", Some(directory), source))?;
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| {
                Error::io("read process directory entry", Some(directory), source)
            })?;
            let name = entry.file_name();
            let Some(text) = name.to_str() else {
                continue;
            };
            if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
                continue;
            }
            let pid = text.parse().map_err(|source| Error::Parse {
                field: "procfs PID",
                input: text.to_owned(),
                source,
            })?;
            ids.push(pid);
        }
        ids
    } else {
        // Own a copy because sorting must not mutate the parsed CLI arguments.
        cli.pid.to_vec()
    };
    if ids.iter().any(|&pid| pid == 0 || pid > i32::MAX as u32) {
        return Err(Error::InvalidInput {
            field: "pid",
            reason: "must be within 1..=2147483647".to_owned(),
        });
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

/// Select matching process instances while retaining their original pidfds.
///
/// Vanished processes are traced and skipped. Inaccessible processes discovered
/// during enumeration are logged and skipped; explicit PID access failures and
/// malformed required identity records are errors. Identity changes never select
/// a replacement process. Protected ancestors are excluded even for explicit IDs.
/// # Errors
/// Invalid selectors, ancestry failures, enumeration errors and unavailable pidfd
/// support fail the selection without a numeric-PID signalling fallback.
/// # Returns and ownership
/// Borrows CLI inputs and returns an owned vector of snapshots plus live pidfds.
/// Callers must retain these handles through preview, approval and execution.
/// Blocks for procfs/kernel inspection; no threads or locks are created.
/// # Examples
/// ```no_run
/// use clap::Parser;
/// let cli = crate::cli::Cli::parse_from(["killer", "-p", "cargo", "--dry-run"]);
/// let targets = crate::process::select(&cli)?;
/// assert!(targets.iter().all(|target| target.process.name == "cargo"));
/// # Ok::<(), crate::error::Error>(())
/// ```
pub(crate) fn select(cli: &Cli) -> Result<Vec<Target>> {
    let excluded = protected()?;
    let mut targets = Vec::new();
    for pid in candidate_ids(cli)? {
        if excluded.contains(&pid) {
            if cli.pid.contains(&pid) {
                tracing::warn!(pid, "protected process excluded");
            }
            continue;
        }
        let before = match read(pid) {
            Ok(process) => process,
            Err(error) if error.is_gone() => {
                tracing::trace!(pid, error = %error, "process vanished before selection");
                continue;
            }
            Err(Error::Io { source, .. })
                if source.kind() == io::ErrorKind::PermissionDenied && cli.pid.is_empty() =>
            {
                tracing::debug!(pid, error = %source, "process identity inaccessible; skipped");
                continue;
            }
            Err(error) => return Err(error),
        };
        if !matches(cli, &before) {
            continue;
        }
        let fd = match PidFd::open(pid) {
            Ok(fd) => fd,
            Err(error) if error.is_gone() => {
                tracing::trace!(pid, error = %error, "process vanished before pidfd acquisition");
                continue;
            }
            Err(error) => return Err(error),
        };
        let after = match read(pid) {
            Ok(process) => process,
            Err(error) if error.is_gone() => {
                tracing::trace!(pid, error = %error, "process vanished after pidfd acquisition");
                continue;
            }
            Err(error) => return Err(error),
        };
        if before.start_ticks != after.start_ticks || before.uid != after.uid {
            tracing::warn!(pid, "process identity changed during selection; skipped");
            continue;
        }
        if fd.exited()? {
            tracing::trace!(pid, "selected process already exited");
            continue;
        }
        if matches(cli, &after) {
            targets.push(Target { process: after, fd });
        }
    }
    Ok(targets)
}

#[cfg(test)]
mod tests {
    //! Pure procfs parser and selector tests without host process dependencies.
    use super::{matches, stat};
    use crate::{cli::Cli, error::Error, types::Process};
    use clap::Parser;

    /// Assemble field 3..22 with controlled comm and starttime.
    fn stat_record(name: &str, start: &str) -> String {
        format!("42 ({name}) S 7 {} {start}", ["0"; 17].join(" "))
    }

    /// Parentheses and line breaks inside comm are data, not record delimiters.
    #[test]
    fn test_stat_handles_complex_comm() {
        for name in ["cargo", "a (b) c)", "one\ntwo", ")((", ""] {
            let parsed = stat(42, &stat_record(name, "98765")).unwrap();
            assert_eq!(parsed, (7, 98765, name.to_owned()));
        }
    }

    /// Required delimiters, fields and numbers cannot silently default.
    #[test]
    fn test_stat_rejects_malformed_records() {
        for input in [
            "",
            "42 cargo S 7",
            "42 )x( S 7",
            "42 (x)",
            "42 (x) S",
            "42 (x) S 7",
        ] {
            assert!(stat(42, input).is_err());
        }
        assert!(matches!(
            stat(42, &stat_record("x", "oops")),
            Err(Error::Parse {
                field: "start ticks",
                ..
            })
        ));
        assert!(matches!(
            stat(43, &stat_record("x", "10")),
            Err(Error::IdentityChanged { pid: 43 })
        ));
    }

    /// Stable in-memory process fixture for pure selector tests.
    fn process() -> Process {
        Process {
            pid: 42,
            ppid: 7,
            uid: 1000,
            start_ticks: 55,
            name: "cargo".to_owned(),
            executable: Some("/usr/bin/cargo".to_owned()),
            command: "cargo build --release".to_owned(),
        }
    }

    // The union of exact names and IDs is then restricted by effective UID.
    #[test]
    fn test_matching_union_and_uid() {
        let mut snapshot = process();
        for (args, expected) in [
            (vec!["killer", "-p", "cargo", "node"], true),
            (vec!["killer", "-p", "car"], false),
            (vec!["killer", "-p", ".*"], false),
            (vec!["killer", "-p", "node", "--pid", "42"], true),
            (vec!["killer", "-p", "cargo", "--pid", "43"], true),
            (vec!["killer", "-p", "cargo", "--uid", "1001"], false),
            (vec!["killer", "--pid", "42"], true),
            (vec!["killer", "--pid", "43"], false),
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert_eq!(matches(&cli, &snapshot), expected);
        }
        snapshot.name = "rustc-wrapper".to_owned();
        let cli = Cli::try_parse_from(["killer", "-p", "rustc", "rust-analyzer"]).unwrap();
        assert!(!matches(&cli, &snapshot));
        snapshot.name = "rust-analyzer".to_owned();
        assert!(matches(&cli, &snapshot));
    }
}
