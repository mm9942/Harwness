//! Resolves raw kernel cgroup ids forwarded by the BPF probe to cgroup paths
//! beneath `--cgroup-root` (PL-97 E4, DoD side).
//!
//! # Why the sentinel resolves, not the probe
//! The privileged probe only sees the numeric cgroup id in its wire header
//! and forwards it unchanged as `harw_dod_signals::Actor::raw_cgroup`
//! (`"cgroup-id:<n>"`). Turning it into a path needs nothing but metadata
//! reads of the cgroup tree, which this **unprivileged** process can do
//! itself: on cgroup v2 a cgroup's id is the inode number of its directory.
//! No capability, no `*-sys` crate and no `unsafe` are involved — only
//! `std::fs::read_dir` and `std::fs::symlink_metadata`, the same kind of
//! bounded, scope-confined read `harw-dod-readfs` performs for file contents.
//!
//! # What a resolved path looks like
//! The resolved value is the **absolute** path of the cgroup directory as
//! seen under the configured root: `<cgroup-root>/<relative path>` (the
//! root itself resolves to `<cgroup-root>`). This is the spelling the export
//! line's `scope` requires (absolute, see `crate::export::sanitize_scope`)
//! and the one `harw-dod-client::correlate` compares after stripping the
//! conventional `/sys/fs/cgroup` mount point. `Actor.cgroup` carries the same
//! string after [`resolve_actor_cgroup`].
//!
//! # Never guess
//! - An unknown id resolves to `None`; the actor then carries no cgroup.
//! - A cache hit is re-verified (`symlink_metadata` of the cached path must
//!   still be a directory with that inode on the root's device); a stale
//!   entry triggers a refresh instead of being reported.
//! - Symbolic links are never followed: directory entries are classified
//!   with `DirEntry::file_type` (no follow) and the root must itself be a
//!   real directory. Directories on another device (a mount point inside the
//!   tree) are not entered, so the walk cannot leave the root's filesystem.
//!
//! # Bounds
//! The cache holds at most [`ResolverLimits::max_entries`] directories and
//! the walk descends at most [`ResolverLimits::max_depth`] levels; a tree
//! larger than that is indexed breadth-first up to the bound (shallow
//! cgroups first) and deeper ids stay unresolved. A miss refreshes the cache
//! at most once per batch (see [`CgroupResolver::begin_batch`]), so a burst
//! of unknown ids costs one walk, not one walk per event. An id that missed
//! after a walk is remembered in a bounded negative cache (at most
//! [`ResolverLimits::max_entries`] ids) and does not trigger another walk
//! until [`MISS_RETRY_AFTER_REFRESHES`] further walks have happened for
//! other ids. Ids that can never resolve (a removed short-lived cgroup, a
//! cgroup outside a narrower root, or one past the bounds) therefore cost
//! no steady background I/O in later batches.
//!
//! # Concurrency
//! [`CgroupResolver`] is owned by the collection thread (`&mut self`).

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};

use harw_dod_signals::Actor;
use harw_types::CgroupId;

/// Default upper bound of cached cgroup directories.
pub const DEFAULT_MAX_ENTRIES: usize = 8192;

/// Default maximum depth below the root that the walk descends.
pub const DEFAULT_MAX_DEPTH: usize = 32;

/// How many walks (triggered by other ids) must pass before an id that
/// missed may trigger a walk again.
pub const MISS_RETRY_AFTER_REFRESHES: u64 = 4;

/// Size bounds of a [`CgroupResolver`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolverLimits {
    /// Most directories kept in the cache (the root counts as one).
    pub max_entries: usize,
    /// Deepest level below the root that is indexed (the root is level 0).
    pub max_depth: usize,
}

impl Default for ResolverLimits {
    fn default() -> Self {
        Self {
            max_entries: DEFAULT_MAX_ENTRIES,
            max_depth: DEFAULT_MAX_DEPTH,
        }
    }
}

/// Inode and device of a directory, read without following symlinks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DirIdentity {
    inode: u64,
    device: u64,
}

#[cfg(unix)]
fn identity_of(meta: &fs::Metadata) -> Option<DirIdentity> {
    use std::os::unix::fs::MetadataExt as _;
    meta.is_dir().then(|| DirIdentity {
        inode: meta.ino(),
        device: meta.dev(),
    })
}

#[cfg(not(unix))]
fn identity_of(_meta: &fs::Metadata) -> Option<DirIdentity> {
    None
}

/// Reads the identity of `path` itself (a symlink is not a directory).
fn dir_identity(path: &Path) -> Option<DirIdentity> {
    fs::symlink_metadata(path)
        .ok()
        .and_then(|meta| identity_of(&meta))
}

/// Maps cgroup ids (directory inodes) to absolute cgroup paths beneath one
/// root. See the module documentation.
#[derive(Debug)]
pub struct CgroupResolver {
    root: PathBuf,
    limits: ResolverLimits,
    by_inode: HashMap<u64, PathBuf>,
    /// Negative cache: id -> value of `refreshes` when it last missed.
    misses: HashMap<u64, u64>,
    refreshed_this_batch: bool,
    refreshes: u64,
}

impl CgroupResolver {
    /// Builds a resolver for `root` with [`ResolverLimits::default`].
    ///
    /// Nothing is read until the first [`Self::resolve`].
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self::with_limits(root, ResolverLimits::default())
    }

    /// Builds a resolver for `root` with explicit `limits`.
    #[must_use]
    pub fn with_limits(root: PathBuf, limits: ResolverLimits) -> Self {
        Self {
            root,
            limits,
            by_inode: HashMap::new(),
            misses: HashMap::new(),
            refreshed_this_batch: false,
            refreshes: 0,
        }
    }

    /// How many directories are cached right now.
    #[must_use]
    pub fn cached(&self) -> usize {
        self.by_inode.len()
    }

    /// How many times the tree was walked since construction.
    #[must_use]
    pub const fn refreshes(&self) -> u64 {
        self.refreshes
    }

    /// Starts a new batch: the next miss may refresh the cache again.
    pub fn begin_batch(&mut self) {
        self.refreshed_this_batch = false;
    }

    /// Resolves `cgroup_id` to the absolute path of its cgroup directory.
    ///
    /// # Returns
    /// `Some(path)` only when a directory beneath the root with inode
    /// `cgroup_id` was found (and, for a cache hit, still exists); `None`
    /// otherwise — never a guess.
    pub fn resolve(&mut self, cgroup_id: u64) -> Option<PathBuf> {
        if cgroup_id == 0 {
            return None;
        }
        if let Some(path) = self.verified_hit(cgroup_id) {
            return Some(path);
        }
        if self.missed_recently(cgroup_id) {
            return None;
        }
        if !self.refreshed_this_batch {
            self.refresh();
            if let Some(path) = self.verified_hit(cgroup_id) {
                return Some(path);
            }
        }
        // Der Cache ist in diesem Batch frisch gelesen: ein Fehlschlag ist echt.
        self.record_miss(cgroup_id);
        None
    }

    /// Whether `cgroup_id` missed within the last
    /// [`MISS_RETRY_AFTER_REFRESHES`] walks.
    fn missed_recently(&self, cgroup_id: u64) -> bool {
        self.misses
            .get(&cgroup_id)
            .is_some_and(|at| self.refreshes.saturating_sub(*at) < MISS_RETRY_AFTER_REFRESHES)
    }

    /// Remembers a miss of `cgroup_id`, keeping the negative cache bounded.
    fn record_miss(&mut self, cgroup_id: u64) {
        let bound = self.limits.max_entries.max(1);
        if self.misses.len() >= bound && !self.misses.contains_key(&cgroup_id) {
            self.prune_misses();
            if self.misses.len() >= bound {
                self.misses.clear();
            }
        }
        self.misses.insert(cgroup_id, self.refreshes);
    }

    /// Drops negative entries that may retry already.
    fn prune_misses(&mut self) {
        let now = self.refreshes;
        self.misses
            .retain(|_, at| now.saturating_sub(*at) < MISS_RETRY_AFTER_REFRESHES);
    }

    /// A cache hit whose directory still carries `cgroup_id` on the root's
    /// device; a stale entry is dropped.
    fn verified_hit(&mut self, cgroup_id: u64) -> Option<PathBuf> {
        let path = self.by_inode.get(&cgroup_id)?.clone();
        let root_device = dir_identity(&self.root).map(|id| id.device);
        let current = dir_identity(&path);
        match (current, root_device) {
            (Some(id), Some(device)) if id.inode == cgroup_id && id.device == device => Some(path),
            _ => {
                self.by_inode.remove(&cgroup_id);
                None
            }
        }
    }

    /// Rebuilds the cache with a bounded breadth-first walk of the root.
    fn refresh(&mut self) {
        self.refreshed_this_batch = true;
        self.refreshes = self.refreshes.saturating_add(1);
        self.by_inode.clear();
        self.prune_misses();
        if self.limits.max_entries == 0 {
            return;
        }
        let Some(root_id) = dir_identity(&self.root) else {
            return;
        };
        self.by_inode.insert(root_id.inode, self.root.clone());
        let mut queue: VecDeque<(PathBuf, usize)> = VecDeque::from([(self.root.clone(), 0)]);
        while let Some((dir, depth)) = queue.pop_front() {
            if depth >= self.limits.max_depth {
                continue;
            }
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                if self.by_inode.len() >= self.limits.max_entries {
                    return;
                }
                // `file_type` folgt keinem Symlink: ein Link auf ein
                // Verzeichnis ist hier kein Verzeichnis.
                let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
                if !is_dir {
                    continue;
                }
                let path = entry.path();
                let Some(id) = dir_identity(&path) else {
                    continue;
                };
                if id.device != root_id.device {
                    continue;
                }
                self.by_inode
                    .entry(id.inode)
                    .or_insert_with(|| path.clone());
                queue.push_back((path, depth + 1));
            }
        }
    }
}

/// Rewrites `actor.cgroup` of an event received from a probe.
///
/// # Description
/// A raw cgroup id (`Actor::raw_cgroup_id`) becomes the resolved absolute
/// path when `resolver` finds it; every other value — an unresolved id, no
/// resolver, or a value the probe sent that is not a raw id — becomes
/// `None`. The sentinel only reports a cgroup path it resolved itself.
pub fn resolve_actor_cgroup(actor: &mut Actor, resolver: Option<&mut CgroupResolver>) {
    let Some(raw) = actor.raw_cgroup_id() else {
        actor.cgroup = None;
        return;
    };
    actor.cgroup = resolver
        .and_then(|resolver| resolver.resolve(raw))
        .and_then(|path| path.to_str().map(str::to_owned))
        .and_then(|path| CgroupId::try_from_str(path).ok());
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};
    use std::os::unix::fs::MetadataExt as _;

    fn inode(path: &Path) -> TestResult<u64> {
        Ok(fs::symlink_metadata(path)
            .map_err(ctx("fixture metadata"))?
            .ino())
    }

    #[test]
    fn test_resolver_matches_directory_inode() -> TestResult {
        let tree = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let job = tree.path().join("harw.slice").join("harw-job-1");
        fs::create_dir_all(&job).map_err(ctx("fixture tree"))?;
        let mut resolver = CgroupResolver::new(tree.path().to_path_buf());

        assert_eq!(resolver.resolve(inode(&job)?), Some(job.clone()));
        assert_eq!(
            resolver.resolve(inode(tree.path())?),
            Some(tree.path().to_path_buf()),
            "the root resolves to itself"
        );
        assert_eq!(resolver.refreshes(), 1, "the second lookup is a cache hit");
        Ok(())
    }

    #[test]
    fn test_resolver_refreshes_on_a_miss() -> TestResult {
        let tree = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut resolver = CgroupResolver::new(tree.path().to_path_buf());
        assert!(resolver.resolve(inode(tree.path())?).is_some());

        let late = tree.path().join("harw-job-2");
        fs::create_dir(&late).map_err(ctx("late cgroup"))?;
        resolver.begin_batch();
        assert_eq!(resolver.resolve(inode(&late)?), Some(late));
        assert_eq!(resolver.refreshes(), 2);
        Ok(())
    }

    #[test]
    fn test_unknown_id_resolves_to_none_and_refreshes_once_per_batch() -> TestResult {
        let tree = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut resolver = CgroupResolver::new(tree.path().to_path_buf());
        assert_eq!(resolver.resolve(u64::MAX), None);
        assert_eq!(resolver.resolve(u64::MAX - 1), None);
        assert_eq!(resolver.refreshes(), 1, "one walk per batch");
        assert_eq!(resolver.resolve(0), None);
        Ok(())
    }

    #[test]
    fn test_unknown_id_does_not_rewalk_in_the_next_batch() -> TestResult {
        let tree = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut resolver = CgroupResolver::new(tree.path().to_path_buf());
        assert_eq!(resolver.resolve(u64::MAX), None);
        assert_eq!(resolver.refreshes(), 1);

        resolver.begin_batch();
        assert_eq!(resolver.resolve(u64::MAX), None);
        assert_eq!(resolver.refreshes(), 1, "a known miss does not walk again");

        // Neue Ids lösen weiterhin Walks aus; nach genug Walks darf die alte
        // Id erneut einen Walk auslösen.
        for offset in 1..=MISS_RETRY_AFTER_REFRESHES {
            resolver.begin_batch();
            assert_eq!(resolver.resolve(u64::MAX - offset), None);
        }
        let walks = resolver.refreshes();
        assert_eq!(walks, 1 + MISS_RETRY_AFTER_REFRESHES);
        resolver.begin_batch();
        assert_eq!(resolver.resolve(u64::MAX), None);
        assert_eq!(resolver.refreshes(), walks + 1, "an expired miss retries");
        Ok(())
    }

    #[test]
    fn test_negative_cache_is_bounded() -> TestResult {
        let tree = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let mut resolver = CgroupResolver::with_limits(
            tree.path().to_path_buf(),
            ResolverLimits {
                max_entries: 2,
                max_depth: 8,
            },
        );
        for id in 1_000_000..1_000_010 {
            assert_eq!(resolver.resolve(id), None);
            assert!(resolver.misses.len() <= 2, "negative cache stays bounded");
        }
        Ok(())
    }

    #[test]
    fn test_stale_cache_entry_is_not_reported() -> TestResult {
        let tree = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let gone = tree.path().join("harw-job-3");
        fs::create_dir(&gone).map_err(ctx("cgroup"))?;
        let id = inode(&gone)?;
        let mut resolver = CgroupResolver::new(tree.path().to_path_buf());
        assert_eq!(resolver.resolve(id), Some(gone.clone()));

        fs::remove_dir(&gone).map_err(ctx("remove cgroup"))?;
        resolver.begin_batch();
        assert_eq!(resolver.resolve(id), None);
        Ok(())
    }

    #[test]
    fn test_symlinks_are_not_followed() -> TestResult {
        let outside = tempfile::tempdir().map_err(ctx("outside"))?;
        let target = outside.path().join("elsewhere");
        fs::create_dir(&target).map_err(ctx("outside dir"))?;
        let tree = tempfile::tempdir().map_err(ctx("tempdir"))?;
        std::os::unix::fs::symlink(outside.path(), tree.path().join("link"))
            .map_err(ctx("symlink"))?;
        let mut resolver = CgroupResolver::new(tree.path().to_path_buf());

        assert_eq!(resolver.resolve(inode(outside.path())?), None);
        assert_eq!(resolver.resolve(inode(&target)?), None);
        assert_eq!(resolver.cached(), 1, "only the root is indexed");

        // Eine Wurzel, die selbst ein Symlink ist, wird nicht betreten.
        let mut via_link = CgroupResolver::new(tree.path().join("link"));
        assert_eq!(via_link.resolve(inode(&target)?), None);
        assert_eq!(via_link.cached(), 0);
        Ok(())
    }

    #[test]
    fn test_cache_is_bounded() -> TestResult {
        let tree = tempfile::tempdir().map_err(ctx("tempdir"))?;
        for name in ["a", "b", "c", "d"] {
            fs::create_dir_all(tree.path().join(name).join("deep")).map_err(ctx("fixture"))?;
        }
        let mut resolver = CgroupResolver::with_limits(
            tree.path().to_path_buf(),
            ResolverLimits {
                max_entries: 3,
                max_depth: 8,
            },
        );
        assert!(resolver.resolve(inode(tree.path())?).is_some());
        assert_eq!(resolver.cached(), 3);

        let mut shallow = CgroupResolver::with_limits(
            tree.path().to_path_buf(),
            ResolverLimits {
                max_entries: 100,
                max_depth: 1,
            },
        );
        assert_eq!(
            shallow.resolve(inode(&tree.path().join("a").join("deep"))?),
            None
        );
        assert_eq!(shallow.cached(), 5, "root plus four first-level cgroups");
        Ok(())
    }

    #[test]
    fn test_resolve_actor_cgroup_sets_only_resolved_paths() -> TestResult {
        let tree = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let job = tree.path().join("harw-job-4");
        fs::create_dir(&job).map_err(ctx("cgroup"))?;
        let mut resolver = CgroupResolver::new(tree.path().to_path_buf());

        let mut actor = Actor {
            uid: 1,
            auid: None,
            cgroup: Actor::raw_cgroup(inode(&job)?),
        };
        resolve_actor_cgroup(&mut actor, Some(&mut resolver));
        assert_eq!(
            actor.cgroup.as_ref().map(CgroupId::as_str),
            job.to_str(),
            "a resolved id becomes the absolute cgroup path"
        );

        let mut unknown = Actor {
            uid: 1,
            auid: None,
            cgroup: Actor::raw_cgroup(u64::MAX),
        };
        resolve_actor_cgroup(&mut unknown, Some(&mut resolver));
        assert_eq!(unknown.cgroup, None);

        let mut claimed = Actor {
            uid: 1,
            auid: None,
            cgroup: Some(CgroupId::from_str("/claimed/by/probe")),
        };
        resolve_actor_cgroup(&mut claimed, Some(&mut resolver));
        assert_eq!(claimed.cgroup, None, "a probe-supplied path is not trusted");

        let mut no_resolver = Actor {
            uid: 1,
            auid: None,
            cgroup: Actor::raw_cgroup(inode(&job)?),
        };
        resolve_actor_cgroup(&mut no_resolver, None);
        assert_eq!(no_resolver.cgroup, None);
        Ok(())
    }
}
