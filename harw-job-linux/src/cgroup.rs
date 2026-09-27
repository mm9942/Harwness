//! cgroup v2 job boundary (Job-Runtime-Doc §2.2, §3.6, Phase 4).
//!
//! # Why in-house instead of libcgroups
//! The Job-Runtime-Doc names Youki's `libcgroups` as the first candidate.
//! This crate implements the small subset it needs directly over cgroupfs
//! interface files instead: no MSRV risk from a large dependency tree, no
//! optional BPF/C surface, and the whole backend is ~one module of safe
//! Rust. The project-owned [`CgroupBackend`] trait keeps a `libcgroups` (or
//! fake) backend possible later without API changes.
//!
//! # Authority
//! [`CgroupV2Fs`] is rooted at one *delegated* directory, held as a
//! `cap_std` directory handle. Every operation resolves only single-component
//! child names beneath it; there is no way to reach another part of the
//! cgroup tree through the API. Limits are written from typed values only
//! ([`JobResources`]).
//!
//! # Delegation requirements
//! The root must be on a `cgroup2` mount, writable by this process, and
//! should contain no processes of its own (the "no internal processes"
//! rule): otherwise controllers cannot be enabled in `cgroup.subtree_control`
//! and limits for those controllers are reported as not enforced instead of
//! being silently dropped.

use std::collections::BTreeSet;
use std::io::{self, Write as _};
use std::os::fd::AsFd as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cap_std::fs::{Dir, OpenOptions};
use harw_job_core::EnforcementState;

use crate::error::{CgroupError, CgroupUnavailableReason};
use crate::process::LinuxProcess;
use crate::resources::{CgroupController, JobResources};

/// `CGROUP2_SUPER_MAGIC` from `linux/magic.h`.
const CGROUP2_SUPER_MAGIC: i128 = 0x6367_7270;

/// What to create for one job attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CgroupSpec {
    /// Child name beneath the delegated root: one path component of
    /// `[A-Za-z0-9_.-]`, not starting with `.`, at most 200 bytes. Include
    /// the attempt id so recovery can find it again.
    pub name: String,
    /// Limits to write after creation.
    pub resources: JobResources,
}

/// A job cgroup created (or reopened) by a [`CgroupBackend`].
///
/// Plain data: the backend re-resolves `name` beneath its own root for
/// every operation, so a handle grants nothing outside that root. Not
/// `Clone`: [`CgroupBackend::remove`] consumes it.
#[derive(Debug, PartialEq, Eq)]
pub struct CgroupHandle {
    name: String,
    proc_path: String,
    limits: Option<EnforcementState>,
}

impl CgroupHandle {
    /// Constructor for backend implementations (including test fakes).
    #[must_use]
    pub fn new(name: String, proc_path: String, limits: Option<EnforcementState>) -> Self {
        Self {
            name,
            proc_path,
            limits,
        }
    }

    /// Child name beneath the delegated root.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Path as it appears in `/proc/<pid>/cgroup` (`0::<path>`); persist it
    /// as [`crate::recovery::LinuxRecoveryIdentity::cgroup_path`].
    #[must_use]
    pub fn proc_path(&self) -> &str {
        &self.proc_path
    }

    /// Whether the requested limits were all written. `None` for a handle
    /// reopened during recovery (limits unknown).
    #[must_use]
    pub fn limits_enforcement(&self) -> Option<EnforcementState> {
        self.limits
    }
}

/// Point-in-time accounting of a job cgroup. `None` when the controller is
/// not enabled for the cgroup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CgroupStats {
    /// `memory.current` in bytes.
    pub memory_current: Option<u64>,
    /// `pids.current`.
    pub pids_current: Option<u64>,
    /// `cpu.stat` `usage_usec`.
    pub cpu_usage_usec: Option<u64>,
    /// `cgroup.events` `populated`.
    pub populated: Option<bool>,
}

/// Project-owned cgroup backend contract (Job-Runtime-Doc §3.6).
pub trait CgroupBackend: Send + Sync {
    /// Creates the job cgroup and writes its limits.
    ///
    /// # Errors
    /// [`CgroupError::InvalidName`], [`CgroupError::InvalidLimit`],
    /// [`CgroupError::AlreadyExists`], permission and I/O errors.
    fn create(&self, spec: &CgroupSpec) -> Result<CgroupHandle, CgroupError>;

    /// Reopens an existing job cgroup by name (recovery).
    ///
    /// # Errors
    /// [`CgroupError::NotFound`], [`CgroupError::InvalidName`].
    fn reopen(&self, name: &str) -> Result<CgroupHandle, CgroupError>;

    /// Moves `process` into the cgroup (`cgroup.procs`).
    ///
    /// # Errors
    /// Permission and I/O errors.
    fn attach(&self, group: &CgroupHandle, process: &LinuxProcess) -> Result<(), CgroupError>;

    /// Moves the *calling* process into the cgroup (writes `0`); used by a
    /// trampoline before `exec`.
    ///
    /// # Errors
    /// Permission and I/O errors.
    fn attach_self(&self, group: &CgroupHandle) -> Result<(), CgroupError>;

    /// Freezes all members (`cgroup.freeze` = 1).
    ///
    /// # Errors
    /// [`CgroupError::Unsupported`] before Linux 5.2.
    fn freeze(&self, group: &CgroupHandle) -> Result<(), CgroupError>;

    /// Thaws all members (`cgroup.freeze` = 0).
    ///
    /// # Errors
    /// [`CgroupError::Unsupported`] before Linux 5.2.
    fn thaw(&self, group: &CgroupHandle) -> Result<(), CgroupError>;

    /// SIGKILLs every member, descendants included (`cgroup.kill`, with a
    /// freeze + kill + thaw fallback before Linux 5.14).
    ///
    /// # Errors
    /// [`CgroupError::Unsupported`] if neither mechanism exists.
    fn kill(&self, group: &CgroupHandle) -> Result<(), CgroupError>;

    /// Reads accounting counters.
    ///
    /// # Errors
    /// [`CgroupError::NotFound`] if the cgroup is gone.
    fn stats(&self, group: &CgroupHandle) -> Result<CgroupStats, CgroupError>;

    /// Removes the (empty) cgroup.
    ///
    /// # Errors
    /// [`CgroupError::Busy`] while members remain.
    fn remove(&self, group: CgroupHandle) -> Result<(), CgroupError>;
}

/// Validates a job cgroup name (single safe path component).
pub(crate) fn validate_name(name: &str) -> Result<(), CgroupError> {
    let valid = !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(CgroupError::InvalidName {
            name: name.to_owned(),
        })
    }
}

/// Parses a flat-keyed cgroup file (`key value` per line) for `key`.
pub(crate) fn parse_keyed(content: &str, key: &str) -> Option<u64> {
    content.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        (parts.next() == Some(key))
            .then(|| parts.next())
            .flatten()
            .and_then(|value| value.parse().ok())
    })
}

/// Parses a space-separated controller list (`cgroup.controllers`,
/// `cgroup.subtree_control`).
pub(crate) fn parse_controllers(content: &str) -> BTreeSet<CgroupController> {
    content
        .split_whitespace()
        .filter_map(|name| {
            CgroupController::ALL
                .into_iter()
                .find(|controller| controller.name() == name)
        })
        .collect()
}

/// Read-only view of a (candidate) delegated cgroup v2 root, as produced by
/// [`detect`]. Used by host capability probes (runtime admission) that must
/// not change the host.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CgroupDetection {
    /// The root lies on a `cgroup2` mount.
    pub mounted_cgroup2: bool,
    /// The root directory, its `cgroup.procs` and its
    /// `cgroup.subtree_control` are writable by this process (the
    /// delegation precondition of [`CgroupV2Fs::open`]).
    pub delegated_writable: bool,
    /// Controllers listed in the root's `cgroup.controllers`, sorted.
    pub available_controllers: Vec<CgroupController>,
    /// Controllers currently enabled for children (`cgroup.subtree_control`),
    /// sorted. [`CgroupV2Fs::open`] would *try* to enable every available
    /// controller; `detect` only reports the current state.
    pub enabled_controllers: Vec<CgroupController>,
}

impl CgroupDetection {
    /// Whether [`CgroupV2Fs::open`] would accept this root (cgroup2 and
    /// writable).
    #[must_use]
    pub const fn is_delegated(&self) -> bool {
        self.mounted_cgroup2 && self.delegated_writable
    }
}

/// Probes `root` without changing anything: no `mkdir`, no write to
/// `cgroup.subtree_control` (unlike [`CgroupV2Fs::open`], which enables
/// controllers). Only `fstatfs`, `faccessat(W_OK)` and reads of
/// `cgroup.controllers` / `cgroup.subtree_control`.
///
/// A directory that is not on a `cgroup2` mount is not an error: it yields
/// a detection with `mounted_cgroup2 == false`.
///
/// # Errors
/// [`CgroupError::CgroupUnavailable`] with
/// [`CgroupUnavailableReason::RootMissing`] if `root` does not exist;
/// I/O errors if it cannot be opened or `fstatfs` fails.
pub fn detect(root: &Path) -> Result<CgroupDetection, CgroupError> {
    let (dir, _canonical) = open_root(root)?;
    if !is_cgroup2(&dir)? {
        return Ok(CgroupDetection::default());
    }
    let controllers = |file: &str| -> Vec<CgroupController> {
        dir.read_to_string(file)
            .map(|content| parse_controllers(&content).into_iter().collect())
            .unwrap_or_default()
    };
    Ok(CgroupDetection {
        mounted_cgroup2: true,
        delegated_writable: is_delegated_writable(&dir),
        available_controllers: controllers("cgroup.controllers"),
        enabled_controllers: controllers("cgroup.subtree_control"),
    })
}

/// [`detect`] for the cgroup this process runs in (the root
/// [`CgroupV2Fs::open_own_cgroup`] would use).
///
/// # Errors
/// [`CgroupError::CgroupUnavailable`] with
/// [`CgroupUnavailableReason::NotCgroup2`] if this process has no cgroup v2
/// membership; otherwise as [`detect`].
pub fn detect_own_cgroup() -> Result<CgroupDetection, CgroupError> {
    detect(&own_cgroup_path()?)
}

/// `/sys/fs/cgroup/<own cgroup v2 path>` of this process.
fn own_cgroup_path() -> Result<PathBuf, CgroupError> {
    let own = crate::proc::snapshot(std::process::id())
        .ok()
        .and_then(|snap| snap.cgroup_v2_path)
        .ok_or_else(|| CgroupError::CgroupUnavailable {
            root: PathBuf::from("/sys/fs/cgroup"),
            reason: CgroupUnavailableReason::NotCgroup2,
        })?;
    Ok(Path::new("/sys/fs/cgroup").join(own.trim_start_matches('/')))
}

/// Canonicalizes `path` and opens it as a directory capability. The one
/// place where ambient filesystem authority is used.
fn open_root(path: &Path) -> Result<(Dir, PathBuf), CgroupError> {
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            CgroupError::CgroupUnavailable {
                root: path.to_path_buf(),
                reason: CgroupUnavailableReason::RootMissing,
            }
        } else {
            CgroupError::from_io("canonicalize", &path.display().to_string(), error)
        }
    })?;
    let root = Dir::open_ambient_dir(&canonical, cap_std::ambient_authority())
        .map_err(|error| CgroupError::from_io("open", &canonical.display().to_string(), error))?;
    Ok((root, canonical))
}

/// Whether `dir` lies on a `cgroup2` mount (`fstatfs`).
fn is_cgroup2(dir: &Dir) -> Result<bool, CgroupError> {
    let fs = rustix::fs::fstatfs(dir.as_fd())
        .map_err(|errno| CgroupError::from_io("fstatfs", ".", errno.into()))?;
    Ok(i128::from(fs.f_type) == CGROUP2_SUPER_MAGIC)
}

/// Whether the directory and its delegation files are writable
/// (`faccessat(W_OK)`, no write).
fn is_delegated_writable(dir: &Dir) -> bool {
    [".", "cgroup.procs", "cgroup.subtree_control"]
        .into_iter()
        .all(|entry| {
            rustix::fs::accessat(
                dir.as_fd(),
                entry,
                rustix::fs::Access::WRITE_OK,
                rustix::fs::AtFlags::empty(),
            )
            .is_ok()
        })
}

/// cgroup v2 backend over cgroupfs files, rooted at a delegated directory.
#[derive(Debug)]
pub struct CgroupV2Fs {
    root: Dir,
    /// Root as seen in `/proc/<pid>/cgroup`.
    root_proc_path: String,
    /// Controllers enabled for children of the root.
    enabled: BTreeSet<CgroupController>,
    /// Display path of the root (diagnostics only).
    root_display: PathBuf,
}

impl CgroupV2Fs {
    /// Opens the delegated root `path` and probes cgroup v2 availability.
    ///
    /// This is the one place where ambient filesystem authority is used:
    /// the caller names the delegated directory once; afterwards the backend
    /// only holds a directory handle. Tries to enable all available
    /// controllers for children (best effort, see module docs).
    ///
    /// # Errors
    /// [`CgroupError::CgroupUnavailable`] with a typed
    /// [`CgroupUnavailableReason`].
    pub fn open(path: &Path) -> Result<Self, CgroupError> {
        let unavailable = |reason| CgroupError::CgroupUnavailable {
            root: path.to_path_buf(),
            reason,
        };
        let (root, canonical) = open_root(path)?;
        if !is_cgroup2(&root)? {
            return Err(unavailable(CgroupUnavailableReason::NotCgroup2));
        }
        if !is_delegated_writable(&root) {
            return Err(unavailable(CgroupUnavailableReason::NotDelegated));
        }
        let root_proc_path = proc_path_of(&canonical)
            .ok_or_else(|| unavailable(CgroupUnavailableReason::NotCgroup2))?;
        let mut backend = Self {
            root,
            root_proc_path,
            enabled: BTreeSet::new(),
            root_display: canonical,
        };
        backend.enable_controllers();
        Ok(backend)
    }

    /// Opens the default delegated root: the cgroup this process runs in,
    /// resolved beneath `/sys/fs/cgroup`. Suitable when the supervisor was
    /// started in a delegated scope (e.g. `systemd-run --user -p
    /// Delegate=yes`).
    ///
    /// # Errors
    /// As [`CgroupV2Fs::open`].
    pub fn open_own_cgroup() -> Result<Self, CgroupError> {
        Self::open(&own_cgroup_path()?)
    }

    /// Controllers enabled for job cgroups created beneath the root.
    #[must_use]
    pub fn enabled_controllers(&self) -> &BTreeSet<CgroupController> {
        &self.enabled
    }

    /// Root path (diagnostics only; no authority).
    #[must_use]
    pub fn root_path(&self) -> &Path {
        &self.root_display
    }

    fn enable_controllers(&mut self) {
        let available = self
            .root
            .read_to_string("cgroup.controllers")
            .map(|content| parse_controllers(&content))
            .unwrap_or_default();
        let wanted: Vec<String> = available
            .iter()
            .map(|controller| format!("+{}", controller.name()))
            .collect();
        if !wanted.is_empty() {
            // Best effort: EBUSY if the root has processes of its own.
            let _ = write_file(&self.root, "cgroup.subtree_control", &wanted.join(" "));
        }
        self.enabled = self
            .root
            .read_to_string("cgroup.subtree_control")
            .map(|content| parse_controllers(&content))
            .unwrap_or_default();
    }

    fn child(&self, name: &str) -> Result<Dir, CgroupError> {
        validate_name(name)?;
        self.root
            .open_dir(name)
            .map_err(|error| CgroupError::from_io("open", name, error))
    }

    fn child_proc_path(&self, name: &str) -> String {
        format!("{}/{name}", self.root_proc_path.trim_end_matches('/'))
    }

    fn write(
        &self,
        group: &CgroupHandle,
        file: &'static str,
        value: &str,
    ) -> Result<(), CgroupError> {
        let dir = self.child(group.name())?;
        write_file(&dir, file, value).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound && dir_exists(&self.root, group.name()) {
                CgroupError::Unsupported { feature: file }
            } else {
                CgroupError::from_io("write", &format!("{}/{file}", group.name()), error)
            }
        })
    }

    fn read_optional(dir: &Dir, file: &str) -> Option<String> {
        dir.read_to_string(file).ok()
    }

    /// Whether the cgroup still has member processes (`cgroup.events`).
    ///
    /// # Errors
    /// [`CgroupError::NotFound`] if the cgroup is gone.
    pub fn is_populated(&self, group: &CgroupHandle) -> Result<bool, CgroupError> {
        let dir = self.child(group.name())?;
        let events = dir
            .read_to_string("cgroup.events")
            .map_err(|error| CgroupError::from_io("read", group.name(), error))?;
        parse_keyed(&events, "populated")
            .map(|value| value != 0)
            .ok_or_else(|| CgroupError::Parse {
                file: "cgroup.events",
                content: events.trim().chars().take(120).collect(),
            })
    }

    /// Polls until the cgroup is empty or `timeout` elapses; returns whether
    /// it became empty.
    ///
    /// # Errors
    /// As [`CgroupV2Fs::is_populated`].
    pub fn wait_empty(&self, group: &CgroupHandle, timeout: Duration) -> Result<bool, CgroupError> {
        let deadline = Instant::now() + timeout;
        loop {
            if !self.is_populated(group)? {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn member_pids(&self, group: &CgroupHandle) -> Result<Vec<u32>, CgroupError> {
        let dir = self.child(group.name())?;
        let content = dir
            .read_to_string("cgroup.procs")
            .map_err(|error| CgroupError::from_io("read", group.name(), error))?;
        Ok(content
            .lines()
            .filter_map(|line| line.trim().parse().ok())
            .collect())
    }

    /// Fallback kill before Linux 5.14: freeze, SIGKILL every listed member,
    /// thaw, repeat until empty.
    ///
    /// Signals are sent by PID here. That is acceptable only because the
    /// cgroup is frozen: frozen members cannot exit, fork or be reaped on
    /// their own, so a listed PID cannot be recycled before it is signalled.
    fn kill_by_freezing(&self, group: &CgroupHandle) -> Result<(), CgroupError> {
        for _ in 0..16 {
            self.freeze(group)?;
            let pids = self.member_pids(group)?;
            for pid in &pids {
                if let Ok(rustix_pid) = crate::process::to_pid(*pid) {
                    // ESRCH: already gone. Other errors surface below as a
                    // cgroup that does not empty.
                    let _ =
                        rustix::process::kill_process(rustix_pid, rustix::process::Signal::KILL);
                }
            }
            self.thaw(group)?;
            if pids.is_empty() || self.wait_empty(group, Duration::from_millis(200))? {
                return Ok(());
            }
        }
        Err(CgroupError::Busy {
            path: group.name().to_owned(),
        })
    }
}

impl CgroupBackend for CgroupV2Fs {
    fn create(&self, spec: &CgroupSpec) -> Result<CgroupHandle, CgroupError> {
        validate_name(&spec.name)?;
        let writes = spec.resources.cgroup_writes()?;
        self.root.create_dir(&spec.name).map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                CgroupError::AlreadyExists {
                    path: spec.name.clone(),
                }
            } else {
                CgroupError::from_io("mkdir", &spec.name, error)
            }
        })?;
        let dir = self.child(&spec.name)?;
        let (mut applied, mut skipped) = (0usize, 0usize);
        for write in &writes {
            if !self.enabled.contains(&write.controller) {
                skipped += 1;
                tracing::warn!(
                    cgroup = spec.name.as_str(),
                    controller = write.controller.name(),
                    file = write.file,
                    "cgroup controller not delegated; limit not enforced"
                );
                continue;
            }
            if let Err(error) = write_file(&dir, write.file, &write.value) {
                // Never leave a half-configured cgroup behind.
                let _ = self.root.remove_dir(&spec.name);
                return Err(CgroupError::from_io(
                    "write",
                    &format!("{}/{}", spec.name, write.file),
                    error,
                ));
            }
            applied += 1;
        }
        let limits = match (applied, skipped) {
            (_, 0) => EnforcementState::Enforced,
            (0, _) => EnforcementState::NotEnforced,
            _ => EnforcementState::Partial,
        };
        Ok(CgroupHandle::new(
            spec.name.clone(),
            self.child_proc_path(&spec.name),
            Some(limits),
        ))
    }

    fn reopen(&self, name: &str) -> Result<CgroupHandle, CgroupError> {
        self.child(name)?;
        Ok(CgroupHandle::new(
            name.to_owned(),
            self.child_proc_path(name),
            None,
        ))
    }

    fn attach(&self, group: &CgroupHandle, process: &LinuxProcess) -> Result<(), CgroupError> {
        self.write(group, "cgroup.procs", &process.pid().to_string())
    }

    fn attach_self(&self, group: &CgroupHandle) -> Result<(), CgroupError> {
        self.write(group, "cgroup.procs", "0")
    }

    fn freeze(&self, group: &CgroupHandle) -> Result<(), CgroupError> {
        self.write(group, "cgroup.freeze", "1")
    }

    fn thaw(&self, group: &CgroupHandle) -> Result<(), CgroupError> {
        self.write(group, "cgroup.freeze", "0")
    }

    fn kill(&self, group: &CgroupHandle) -> Result<(), CgroupError> {
        match self.write(group, "cgroup.kill", "1") {
            Err(CgroupError::Unsupported { .. }) => self.kill_by_freezing(group),
            other => other,
        }
    }

    fn stats(&self, group: &CgroupHandle) -> Result<CgroupStats, CgroupError> {
        let dir = self.child(group.name())?;
        let number = |file: &str| {
            Self::read_optional(&dir, file).and_then(|content| content.trim().parse().ok())
        };
        Ok(CgroupStats {
            memory_current: number("memory.current"),
            pids_current: number("pids.current"),
            cpu_usage_usec: Self::read_optional(&dir, "cpu.stat")
                .and_then(|content| parse_keyed(&content, "usage_usec")),
            populated: Self::read_optional(&dir, "cgroup.events")
                .and_then(|content| parse_keyed(&content, "populated"))
                .map(|value| value != 0),
        })
    }

    fn remove(&self, group: CgroupHandle) -> Result<(), CgroupError> {
        validate_name(group.name())?;
        self.root
            .remove_dir(group.name())
            .map_err(|error| CgroupError::from_io("rmdir", group.name(), error))
    }
}

fn dir_exists(root: &Dir, name: &str) -> bool {
    root.is_dir(name)
}

/// Writes `value` to an existing cgroup interface file (no create, no
/// truncate — cgroupfs files are written as one `write(2)`).
fn write_file(dir: &Dir, file: &str, value: &str) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true);
    let mut handle = dir.open_with(file, &options)?;
    handle.write_all(value.as_bytes())
}

/// Maps a canonical path on a cgroup2 mount to its `/proc/<pid>/cgroup`
/// form using `/proc/self/mountinfo`.
fn proc_path_of(canonical: &Path) -> Option<String> {
    let mounts = procfs::process::Process::myself().ok()?.mountinfo().ok()?;
    let mount = mounts
        .into_iter()
        .filter(|mount| mount.fs_type == "cgroup2" && canonical.starts_with(&mount.mount_point))
        .max_by_key(|mount| mount.mount_point.as_os_str().len())?;
    let relative = canonical.strip_prefix(&mount.mount_point).ok()?;
    let joined = Path::new(&mount.root).join(relative);
    let text = joined.to_str()?.to_owned();
    Some(if text.is_empty() {
        "/".to_owned()
    } else {
        text
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_validate_name() {
        assert!(validate_name("job-attempt_1.a").is_ok());
        let long = "x".repeat(201);
        for bad in ["", ".", "..", ".hidden", "a/b", "a b", "ä", long.as_str()] {
            assert!(
                matches!(validate_name(bad), Err(CgroupError::InvalidName { .. })),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn test_parse_keyed_and_controllers() {
        let stat = "usage_usec 1234\nuser_usec 1000\nsystem_usec 234\n";
        assert_eq!(parse_keyed(stat, "usage_usec"), Some(1234));
        assert_eq!(parse_keyed(stat, "missing"), None);
        assert_eq!(parse_keyed("populated 1\nfrozen 0\n", "populated"), Some(1));
        let controllers = parse_controllers("cpuset cpu io memory hugetlb pids rdma misc\n");
        assert_eq!(controllers.len(), 4);
        assert!(controllers.contains(&CgroupController::Memory));
        assert!(parse_controllers("").is_empty());
    }

    #[test]
    fn test_open_plain_directory_is_not_cgroup2() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        match CgroupV2Fs::open(temp.path()) {
            Err(CgroupError::CgroupUnavailable {
                reason: CgroupUnavailableReason::NotCgroup2,
                ..
            }) => Ok(()),
            other => Err(TestError::Unexpected(format!("{other:?}"))),
        }
    }

    #[test]
    fn test_detect_plain_directory_is_not_cgroup2() -> TestResult {
        let temp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let detection = detect(temp.path()).map_err(ctx("detect"))?;
        assert_eq!(detection, CgroupDetection::default());
        assert!(!detection.is_delegated());
        // Nothing was created or written beneath the probed directory.
        let entries = std::fs::read_dir(temp.path())
            .map_err(ctx("read_dir"))?
            .count();
        assert_eq!(entries, 0);
        Ok(())
    }

    #[test]
    fn test_detect_missing_root() {
        assert!(matches!(
            detect(Path::new("/nonexistent/harw-cgroup-root")),
            Err(CgroupError::CgroupUnavailable {
                reason: CgroupUnavailableReason::RootMissing,
                ..
            })
        ));
    }

    #[test]
    fn test_detect_own_cgroup_is_consistent() {
        // Host dependent: only the invariants are checked. Enabled
        // controllers are always a subset of the available ones, and a
        // non-cgroup2 detection carries no controllers.
        if let Ok(detection) = detect_own_cgroup() {
            if detection.mounted_cgroup2 {
                assert!(
                    detection
                        .enabled_controllers
                        .iter()
                        .all(|controller| detection.available_controllers.contains(controller))
                );
            } else {
                assert!(detection.available_controllers.is_empty());
                assert!(!detection.delegated_writable);
            }
        }
    }

    #[test]
    fn test_open_missing_root() {
        assert!(matches!(
            CgroupV2Fs::open(Path::new("/nonexistent/harw-cgroup-root")),
            Err(CgroupError::CgroupUnavailable {
                reason: CgroupUnavailableReason::RootMissing,
                ..
            })
        ));
    }

    /// Delegated root for integration tests: `HARW_TEST_CGROUP_ROOT`, else
    /// this process' own cgroup.
    fn delegated() -> TestResult<CgroupV2Fs> {
        match std::env::var_os("HARW_TEST_CGROUP_ROOT") {
            Some(root) => {
                CgroupV2Fs::open(Path::new(&root)).map_err(ctx("open HARW_TEST_CGROUP_ROOT"))
            }
            None => CgroupV2Fs::open_own_cgroup().map_err(ctx("open own cgroup")),
        }
    }

    fn unique(prefix: &str) -> String {
        format!("{prefix}-{}-{}", std::process::id(), nonce())
    }

    fn nonce() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default()
    }

    #[test]
    #[ignore = "needs delegated cgroup v2"]
    fn test_create_attach_stats_kill_remove() -> TestResult {
        use std::process::Command;
        let backend = delegated()?;
        let spec = CgroupSpec {
            name: unique("harw-test-job"),
            resources: JobResources {
                pids_max: Some(32),
                memory_max: Some(256 * 1024 * 1024),
                ..JobResources::default()
            },
        };
        let handle = backend.create(&spec).map_err(ctx("create"))?;
        assert!(handle.limits_enforcement().is_some());
        // A shell with a grandchild: cgroup kill must reach descendants.
        let (mut process, _stdio) =
            LinuxProcess::spawn(Command::new("sh").arg("-c").arg("sleep 30 & wait"))
                .map_err(ctx("spawn"))?;
        backend.attach(&handle, &process).map_err(ctx("attach"))?;
        let identity = crate::proc::snapshot(process.pid()).map_err(ctx("snapshot"))?;
        assert_eq!(identity.cgroup_v2_path.as_deref(), Some(handle.proc_path()));
        let stats = backend.stats(&handle).map_err(ctx("stats"))?;
        assert_eq!(stats.populated, Some(true));
        backend.freeze(&handle).map_err(ctx("freeze"))?;
        backend.thaw(&handle).map_err(ctx("thaw"))?;
        backend.kill(&handle).map_err(ctx("kill"))?;
        let outcome = process.wait().map_err(ctx("wait"))?;
        assert!(matches!(
            outcome,
            harw_job_core::ExitOutcome::Signaled { signal: 9, .. }
        ));
        let empty = backend
            .wait_empty(&handle, Duration::from_secs(5))
            .map_err(ctx("wait empty"))?;
        assert!(empty, "descendants survived cgroup kill");
        backend.remove(handle).map_err(ctx("remove"))?;
        Ok(())
    }

    #[test]
    #[ignore = "needs delegated cgroup v2"]
    fn test_pids_max_is_enforced() -> TestResult {
        use std::process::Command;
        let backend = delegated()?;
        if !backend
            .enabled_controllers()
            .contains(&CgroupController::Pids)
        {
            return Ok(()); // Controller not delegated: nothing to prove here.
        }
        let spec = CgroupSpec {
            name: unique("harw-test-pids"),
            resources: JobResources {
                pids_max: Some(1),
                ..JobResources::default()
            },
        };
        let handle = backend.create(&spec).map_err(ctx("create"))?;
        assert_eq!(
            handle.limits_enforcement(),
            Some(EnforcementState::Enforced)
        );
        let (mut process, _stdio) = LinuxProcess::spawn(
            Command::new("sh")
                .arg("-c")
                .arg("sleep 0.2; /bin/true && exit 0 || exit 7"),
        )
        .map_err(ctx("spawn"))?;
        backend.attach(&handle, &process).map_err(ctx("attach"))?;
        let outcome = process.wait().map_err(ctx("wait"))?;
        // Forking a second task must fail under pids.max = 1.
        assert!(
            !matches!(outcome, harw_job_core::ExitOutcome::Exited(0)),
            "{outcome:?}"
        );
        backend
            .wait_empty(&handle, Duration::from_secs(5))
            .map_err(ctx("wait empty"))?;
        backend.remove(handle).map_err(ctx("remove"))?;
        Ok(())
    }
}
