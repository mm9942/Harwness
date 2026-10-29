//! Bounded, symlink-free walk of cgroup v2 descendants (PL-97 E3b).
//!
//! # Purpose
//! Resolves the kernel ids of the cgroups beneath the explicit roots of a
//! profile with `include_descendants = true`. The id of a cgroup v2
//! directory is its inode number on the cgroup2 mount (with a 64-bit
//! `ino_t`, `st_ino` equals the kernfs node id that
//! `bpf_get_current_cgroup_id` returns), so the walk
//! needs nothing but `lstat` and directory listing — no new capability and
//! no write access.
//!
//! # Safety properties
//! - **No symlink is followed.** Entry types come from the directory listing
//!   without following links, every candidate is re-checked with `lstat`,
//!   and a directory on another device (a mount point inside the tree) is
//!   not entered either.
//! - **Bounded work.** [`WalkLimits::max_depth`] bounds the depth below a
//!   root and [`WalkLimits::max_entries`] the number of directory entries
//!   examined per walk over all roots. Hitting the entry budget makes the
//!   observation incomplete, so the planner retires nothing that round.
//! - **Vanishing directories are tolerated.** A cgroup removed during the
//!   walk (`ENOENT` on `lstat` or on listing) is simply absent from this
//!   round. Every other I/O error is counted and makes the observation
//!   incomplete; the walk itself never fails.
//! - **Root identity is checked.** A root that is missing, no longer a
//!   directory, or was re-created with a different id contributes no
//!   descendants (a re-created root is a different cgroup and must not be
//!   re-admitted silently); it is counted in
//!   [`WalkReport::roots_unavailable`].
//!
//! # Concurrency
//! Stateless; [`walk_descendants`] performs blocking file-system I/O on the
//! calling thread.

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fs::{Metadata, ReadDir};
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use crate::profile::{ResolvedCgroup, ScopeObservation};

/// Default maximum depth below an explicit root (root = depth 0).
pub const DEFAULT_MAX_WALK_DEPTH: usize = 16;

/// Default maximum number of directory entries (files included) examined
/// per walk over all roots. A cgroup v2 directory carries a few dozen
/// interface files, so this admits well over 16 384 cgroups.
pub const DEFAULT_MAX_WALK_ENTRIES: usize = 1 << 20;

/// Bounds of one walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalkLimits {
    /// Deepest descendant level that is admitted (a direct child of a root
    /// has depth 1). Deeper directories are counted in
    /// [`WalkReport::depth_limited`] and never admitted.
    pub max_depth: usize,
    /// Directory entries examined per walk; reaching it stops the walk and
    /// marks the observation incomplete.
    pub max_entries: usize,
}

impl Default for WalkLimits {
    fn default() -> Self {
        Self {
            max_depth: DEFAULT_MAX_WALK_DEPTH,
            max_entries: DEFAULT_MAX_WALK_ENTRIES,
        }
    }
}

/// The outcome of one walk: the observation for the planner plus the
/// counters a caller must surface.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WalkReport {
    /// Descendants found and whether the walk was complete.
    pub observation: ScopeObservation,
    /// Directory entries examined.
    pub entries_examined: usize,
    /// Symlinks and foreign mounts that were not followed.
    pub not_followed: usize,
    /// Directories deeper than [`WalkLimits::max_depth`] (not admitted).
    pub depth_limited: usize,
    /// Directories that disappeared during the walk (absent this round).
    pub vanished: usize,
    /// Explicit roots that are missing or were re-created.
    pub roots_unavailable: usize,
    /// Other I/O errors (e.g. `EACCES`); each makes the walk incomplete.
    pub errors: usize,
    /// `true` when [`WalkLimits::max_entries`] stopped the walk.
    pub truncated: bool,
}

/// The two file-system operations the walk uses.
///
/// Production uses [`HostCgroupFs`]; the indirection exists so tests can
/// remove a directory deterministically between its discovery and its
/// listing.
pub trait CgroupFs {
    /// `lstat(2)`: metadata without following a final symlink.
    ///
    /// # Errors
    /// The underlying I/O error.
    fn symlink_metadata(&self, path: &Path) -> std::io::Result<Metadata>;

    /// Opens a directory listing.
    ///
    /// # Errors
    /// The underlying I/O error.
    fn read_dir(&self, path: &Path) -> std::io::Result<ReadDir>;
}

/// [`CgroupFs`] backed by `std::fs`.
#[derive(Debug, Clone, Copy, Default)]
pub struct HostCgroupFs;

impl CgroupFs for HostCgroupFs {
    fn symlink_metadata(&self, path: &Path) -> std::io::Result<Metadata> {
        std::fs::symlink_metadata(path)
    }

    fn read_dir(&self, path: &Path) -> std::io::Result<ReadDir> {
        std::fs::read_dir(path)
    }
}

/// Maps a mount-relative cgroup path (`/harw.slice`) below `cgroup_root`
/// (the cgroup2 mount, e.g. `/sys/fs/cgroup`).
#[must_use]
pub fn host_path(cgroup_root: &Path, cgroup_path: &str) -> PathBuf {
    let relative = cgroup_path.trim_start_matches('/');
    if relative.is_empty() {
        cgroup_root.to_path_buf()
    } else {
        cgroup_root.join(relative)
    }
}

/// Resolves one explicit root to its kernel id.
///
/// # Errors
/// - The `lstat` error when the path cannot be inspected.
/// - [`ErrorKind::InvalidInput`] when `cgroup_path` is not absolute or the
///   entry is not a directory (a symlink is never accepted as a root).
pub fn resolve_cgroup(
    fs: &dyn CgroupFs,
    cgroup_root: &Path,
    cgroup_path: &str,
) -> std::io::Result<ResolvedCgroup> {
    if !cgroup_path.starts_with('/') {
        return Err(ErrorKind::InvalidInput.into());
    }
    let meta = fs.symlink_metadata(&host_path(cgroup_root, cgroup_path))?;
    if !meta.file_type().is_dir() {
        return Err(ErrorKind::InvalidInput.into());
    }
    Ok(ResolvedCgroup {
        path: cgroup_path.to_owned(),
        id: meta.ino(),
    })
}

/// Walks the descendants of every root breadth-first (names sorted per
/// directory, so the order is deterministic) and reports what it found.
#[must_use]
pub fn walk_descendants(
    fs: &dyn CgroupFs,
    cgroup_root: &Path,
    roots: &[ResolvedCgroup],
    limits: WalkLimits,
) -> WalkReport {
    let mut report = WalkReport {
        observation: ScopeObservation {
            descendants: Vec::new(),
            complete: true,
        },
        ..WalkReport::default()
    };
    for root in roots {
        if report.truncated {
            break;
        }
        walk_root(fs, cgroup_root, root, limits, &mut report);
    }
    report
}

/// A directory waiting to be listed.
struct Pending {
    host: PathBuf,
    path: String,
    id: u64,
    depth: usize,
}

/// How listing one directory ended.
enum Listing {
    /// Every entry was read.
    Done,
    /// The directory disappeared while it was listed.
    Vanished,
    /// Another I/O error; the listing may be partial.
    Failed,
    /// The entry budget ran out.
    Truncated,
}

fn walk_root(
    fs: &dyn CgroupFs,
    cgroup_root: &Path,
    root: &ResolvedCgroup,
    limits: WalkLimits,
    report: &mut WalkReport,
) {
    let root_host = host_path(cgroup_root, &root.path);
    let root_dev = match fs.symlink_metadata(&root_host) {
        Ok(meta) if meta.file_type().is_dir() && meta.ino() == root.id => meta.dev(),
        Ok(_) => {
            report.roots_unavailable += 1;
            return;
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            report.roots_unavailable += 1;
            return;
        }
        Err(_) => {
            report.errors += 1;
            report.observation.complete = false;
            return;
        }
    };

    let mut queue = VecDeque::from([Pending {
        host: root_host,
        path: root.path.clone(),
        id: root.id,
        depth: 0,
    }]);
    while let Some(dir) = queue.pop_front() {
        let mut children: Vec<(OsString, PathBuf)> = Vec::new();
        let listing = list_children(fs, &dir, limits, report, &mut children);
        if matches!(listing, Listing::Vanished) {
            report.vanished += 1;
            continue;
        }
        if dir.depth > 0 {
            report.observation.descendants.push(ResolvedCgroup {
                path: dir.path.clone(),
                id: dir.id,
            });
        }
        match listing {
            Listing::Done => {}
            Listing::Failed => {
                report.errors += 1;
                report.observation.complete = false;
            }
            Listing::Truncated => {
                report.truncated = true;
                report.observation.complete = false;
                return;
            }
            Listing::Vanished => continue,
        }

        children.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        for (name, child_host) in children {
            match fs.symlink_metadata(&child_host) {
                Ok(meta) if !meta.file_type().is_dir() => {
                    // Zwischen Auflistung und `lstat` ersetzt (z. B. durch
                    // einen Symlink): nicht folgen.
                    report.not_followed += 1;
                }
                Ok(meta) if meta.dev() != root_dev => report.not_followed += 1,
                Ok(meta) => queue.push_back(Pending {
                    host: child_host,
                    path: child_path(&dir.path, &name),
                    id: meta.ino(),
                    depth: dir.depth + 1,
                }),
                Err(error) if error.kind() == ErrorKind::NotFound => report.vanished += 1,
                Err(_) => {
                    report.errors += 1;
                    report.observation.complete = false;
                }
            }
        }
    }
}

/// Lists one directory and collects the subdirectories that may be
/// entered (symlinks and too-deep directories are only counted).
fn list_children(
    fs: &dyn CgroupFs,
    dir: &Pending,
    limits: WalkLimits,
    report: &mut WalkReport,
    children: &mut Vec<(OsString, PathBuf)>,
) -> Listing {
    let entries = match fs.read_dir(&dir.host) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Listing::Vanished,
        Err(_) => return Listing::Failed,
    };
    for entry in entries {
        if report.entries_examined >= limits.max_entries {
            return Listing::Truncated;
        }
        report.entries_examined += 1;
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if error.kind() == ErrorKind::NotFound => return Listing::Vanished,
            Err(_) => return Listing::Failed,
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                report.vanished += 1;
                continue;
            }
            Err(_) => {
                report.errors += 1;
                report.observation.complete = false;
                continue;
            }
        };
        if file_type.is_symlink() {
            report.not_followed += 1;
            continue;
        }
        if !file_type.is_dir() {
            continue;
        }
        if dir.depth >= limits.max_depth {
            report.depth_limited += 1;
            continue;
        }
        children.push((entry.file_name(), entry.path()));
    }
    Listing::Done
}

fn child_path(parent: &str, name: &OsStr) -> String {
    let name = name.to_string_lossy();
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{parent}/{name}")
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs::{Metadata, ReadDir};
    use std::path::{Path, PathBuf};

    use super::{
        CgroupFs, HostCgroupFs, WalkLimits, child_path, host_path, resolve_cgroup, walk_descendants,
    };
    use crate::profile::ResolvedCgroup;
    use crate::test_support::{TestResult, ctx};

    /// Builds `<tmp>/harw.slice` with interface files, like a cgroup root.
    fn fake_root() -> TestResult<(tempfile::TempDir, ResolvedCgroup)> {
        let tmp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        mkcg(tmp.path(), "/harw.slice")?;
        let root = resolve_cgroup(&HostCgroupFs, tmp.path(), "/harw.slice")
            .map_err(ctx("resolve fake root"))?;
        Ok((tmp, root))
    }

    fn mkcg(cgroup_root: &Path, path: &str) -> TestResult<u64> {
        let dir = host_path(cgroup_root, path);
        std::fs::create_dir_all(&dir).map_err(ctx("create fake cgroup"))?;
        std::fs::write(dir.join("cgroup.procs"), b"").map_err(ctx("interface file"))?;
        let id = resolve_cgroup(&HostCgroupFs, cgroup_root, path)
            .map_err(ctx("resolve fake cgroup"))?
            .id;
        Ok(id)
    }

    fn paths(report: &super::WalkReport) -> Vec<&str> {
        report
            .observation
            .descendants
            .iter()
            .map(|group| group.path.as_str())
            .collect()
    }

    #[test]
    fn walk_finds_descendants_breadth_first_with_their_inode_ids() -> TestResult {
        let (tmp, root) = fake_root()?;
        let b = mkcg(tmp.path(), "/harw.slice/harw-job-b")?;
        let a = mkcg(tmp.path(), "/harw.slice/harw-job-a")?;
        let nested = mkcg(tmp.path(), "/harw.slice/harw-job-a/inner")?;
        let report = walk_descendants(
            &HostCgroupFs,
            tmp.path(),
            std::slice::from_ref(&root),
            WalkLimits::default(),
        );
        assert!(report.observation.complete);
        assert_eq!(
            paths(&report),
            vec![
                "/harw.slice/harw-job-a",
                "/harw.slice/harw-job-b",
                "/harw.slice/harw-job-a/inner"
            ]
        );
        let found: Vec<u64> = report
            .observation
            .descendants
            .iter()
            .map(|g| g.id)
            .collect();
        assert_eq!(found, vec![a, b, nested]);
        assert!(!found.contains(&root.id));
        Ok(())
    }

    #[test]
    fn symlink_to_outside_the_root_is_not_followed() -> TestResult {
        let (tmp, root) = fake_root()?;
        mkcg(tmp.path(), "/outside/secret")?;
        std::os::unix::fs::symlink(
            tmp.path().join("outside"),
            tmp.path().join("harw.slice/escape"),
        )
        .map_err(ctx("symlink"))?;
        let report = walk_descendants(
            &HostCgroupFs,
            tmp.path(),
            std::slice::from_ref(&root),
            WalkLimits::default(),
        );
        assert!(report.observation.descendants.is_empty());
        assert_eq!(report.not_followed, 1);
        assert!(report.observation.complete);
        Ok(())
    }

    #[test]
    fn symlinked_root_is_rejected() -> TestResult {
        let tmp = tempfile::tempdir().map_err(ctx("tempdir"))?;
        mkcg(tmp.path(), "/real")?;
        std::os::unix::fs::symlink(tmp.path().join("real"), tmp.path().join("link"))
            .map_err(ctx("symlink"))?;
        assert!(resolve_cgroup(&HostCgroupFs, tmp.path(), "/link").is_err());
        assert!(resolve_cgroup(&HostCgroupFs, tmp.path(), "relative").is_err());
        Ok(())
    }

    #[test]
    fn depth_is_bounded_and_counted() -> TestResult {
        let (tmp, root) = fake_root()?;
        mkcg(tmp.path(), "/harw.slice/a/b/c")?;
        let report = walk_descendants(
            &HostCgroupFs,
            tmp.path(),
            std::slice::from_ref(&root),
            WalkLimits {
                max_depth: 2,
                ..WalkLimits::default()
            },
        );
        assert_eq!(paths(&report), vec!["/harw.slice/a", "/harw.slice/a/b"]);
        assert_eq!(report.depth_limited, 1);
        assert!(report.observation.complete);
        Ok(())
    }

    #[test]
    fn entry_budget_truncates_and_marks_the_walk_incomplete() -> TestResult {
        let (tmp, root) = fake_root()?;
        for job in 0..4 {
            mkcg(tmp.path(), &format!("/harw.slice/harw-job-{job}"))?;
        }
        let report = walk_descendants(
            &HostCgroupFs,
            tmp.path(),
            std::slice::from_ref(&root),
            WalkLimits {
                max_entries: 3,
                ..WalkLimits::default()
            },
        );
        assert!(report.truncated);
        assert!(!report.observation.complete);
        assert_eq!(report.entries_examined, 3);
        Ok(())
    }

    #[test]
    fn missing_or_recreated_root_contributes_nothing_and_is_counted() -> TestResult {
        let (tmp, root) = fake_root()?;
        mkcg(tmp.path(), "/harw.slice/harw-job-1")?;
        // Gleicher Pfad, andere Kernel-Identitaet: so sieht eine neu angelegte
        // Wurzel aus. Ihre Nachfahren werden nicht zugelassen.
        let recreated = ResolvedCgroup {
            path: root.path.clone(),
            id: root.id.wrapping_add(1),
        };
        let report = walk_descendants(
            &HostCgroupFs,
            tmp.path(),
            std::slice::from_ref(&recreated),
            WalkLimits::default(),
        );
        assert!(report.observation.descendants.is_empty());
        assert_eq!(report.roots_unavailable, 1);
        assert!(report.observation.complete);

        std::fs::remove_dir_all(tmp.path().join("harw.slice")).map_err(ctx("remove root"))?;
        let report = walk_descendants(
            &HostCgroupFs,
            tmp.path(),
            std::slice::from_ref(&root),
            WalkLimits::default(),
        );
        assert!(report.observation.descendants.is_empty());
        assert_eq!(report.roots_unavailable, 1);
        assert!(report.observation.complete);
        Ok(())
    }

    /// Removes `victim` right before it is listed: the deterministic form of
    /// a cgroup that disappears between discovery and listing.
    struct VanishingFs {
        victim: PathBuf,
    }

    impl CgroupFs for VanishingFs {
        fn symlink_metadata(&self, path: &Path) -> std::io::Result<Metadata> {
            HostCgroupFs.symlink_metadata(path)
        }

        fn read_dir(&self, path: &Path) -> std::io::Result<ReadDir> {
            if path == self.victim {
                std::fs::remove_dir_all(path)?;
            }
            HostCgroupFs.read_dir(path)
        }
    }

    #[test]
    fn directory_vanishing_mid_walk_is_tolerated_and_absent() -> TestResult {
        let (tmp, root) = fake_root()?;
        mkcg(tmp.path(), "/harw.slice/harw-job-1/inner")?;
        mkcg(tmp.path(), "/harw.slice/harw-job-2")?;
        let fs = VanishingFs {
            victim: tmp.path().join("harw.slice/harw-job-1"),
        };
        let report = walk_descendants(
            &fs,
            tmp.path(),
            std::slice::from_ref(&root),
            WalkLimits::default(),
        );
        assert_eq!(paths(&report), vec!["/harw.slice/harw-job-2"]);
        assert_eq!(report.vanished, 1);
        assert_eq!(report.errors, 0);
        assert!(report.observation.complete);
        Ok(())
    }

    #[test]
    fn paths_map_below_the_mount_root() {
        assert_eq!(
            host_path(Path::new("/sys/fs/cgroup"), "/harw.slice"),
            PathBuf::from("/sys/fs/cgroup/harw.slice")
        );
        assert_eq!(
            host_path(Path::new("/sys/fs/cgroup"), "/"),
            PathBuf::from("/sys/fs/cgroup")
        );
        assert_eq!(child_path("/", OsStr::new("a")), "/a");
        assert_eq!(child_path("/a", OsStr::new("b")), "/a/b");
    }
}
