//! Job lanes: named, resizable concurrency limits shared between the job
//! worker and the operators that manage it.
//!
//! # Responsibility
//! [`JobLanes`] owns one [`ResizablePermits`] per lane (`work_driver` for
//! long agent runs, `memory` for memory maintenance). The worker runs inside
//! `harw serve`; an operator changes limits from another process (`/jobs
//! permits`), so the control channel is a pair of small files in the job
//! store root:
//!
//! - `lane-limits.json` — operator overrides (`{"work_driver": 4}`), written
//!   by [`write_limit`], applied by the worker on its next poll
//!   ([`JobLanes::apply_overrides`]);
//! - `lane-status.json` — the worker's last published usage
//!   ([`JobLanes::publish_status`]), read by [`read_status`].
//!
//! Both files are advisory and best-effort: a missing or corrupt file means
//! "no override" / "no worker status"; writes are atomic (temp + rename).
//! The `[jobs] max_running` config value is the default of the
//! `work_driver` lane, applied at worker start.

use std::collections::BTreeMap;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::permits::{MAX_PERMITS, MIN_PERMITS, PermitStatus, ResizablePermits};

/// Lane of long `work_driver` agent runs.
pub const LANE_WORK_DRIVER: &str = "work_driver";
/// Lane of `memory_maintenance` jobs.
pub const LANE_MEMORY: &str = "memory";
/// All lane names, in display order.
pub const LANE_NAMES: [&str; 2] = [LANE_WORK_DRIVER, LANE_MEMORY];
/// Default limit of the memory lane: maintenance runs serialise on the
/// consolidation lock anyway.
pub const DEFAULT_MEMORY_LANE_LIMIT: usize = 1;

const LIMITS_FILE: &str = "lane-limits.json";
const STATUS_FILE: &str = "lane-status.json";

/// One lane's usage, as published by the worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneStatus {
    /// Lane name.
    pub lane: String,
    /// Configured limit.
    pub limit: usize,
    /// Running jobs.
    pub in_use: usize,
    /// Free permits.
    pub available: usize,
}

impl LaneStatus {
    fn of(lane: &str, status: PermitStatus) -> Self {
        Self {
            lane: lane.to_owned(),
            limit: status.limit,
            in_use: status.in_use,
            available: status.available,
        }
    }
}

/// The resizable lanes of one worker.
#[derive(Debug, Clone)]
pub struct JobLanes {
    work_driver: ResizablePermits,
    memory: ResizablePermits,
}

impl JobLanes {
    /// Lanes with the given limits (each clamped to at least 1).
    #[must_use]
    pub fn new(work_driver: usize, memory: usize) -> Self {
        Self {
            work_driver: ResizablePermits::new(work_driver),
            memory: ResizablePermits::new(memory),
        }
    }

    /// The permit pool of `lane`, if the name is known.
    #[must_use]
    pub fn lane(&self, lane: &str) -> Option<&ResizablePermits> {
        match lane {
            LANE_WORK_DRIVER => Some(&self.work_driver),
            LANE_MEMORY => Some(&self.memory),
            _ => None,
        }
    }

    /// The `work_driver` pool.
    #[must_use]
    pub fn work_driver(&self) -> &ResizablePermits {
        &self.work_driver
    }

    /// The `memory` pool.
    #[must_use]
    pub fn memory(&self) -> &ResizablePermits {
        &self.memory
    }

    /// Current status of every lane.
    #[must_use]
    pub fn status(&self) -> Vec<LaneStatus> {
        LANE_NAMES
            .iter()
            .filter_map(|name| {
                self.lane(name)
                    .map(|pool| LaneStatus::of(name, pool.status()))
            })
            .collect()
    }

    /// Applies the operator overrides found in `root`. Returns the lanes whose
    /// limit changed. Raising wakes waiters, lowering never stops a job.
    pub fn apply_overrides(&self, root: &Path) -> Vec<&'static str> {
        let overrides = read_limits(root);
        let mut changed = Vec::new();
        for name in LANE_NAMES {
            let (Some(pool), Some(wanted)) = (self.lane(name), overrides.get(name)) else {
                continue;
            };
            if pool.limit() != (*wanted).clamp(MIN_PERMITS, MAX_PERMITS) {
                pool.set_limit(*wanted);
                changed.push(name);
            }
        }
        changed
    }

    /// Publishes the current status to `root` (best-effort).
    ///
    /// # Errors
    /// The I/O error of the atomic write.
    pub fn publish_status(&self, root: &Path) -> io::Result<()> {
        write_json_atomic(&root.join(STATUS_FILE), &self.status())
    }
}

/// The operator overrides stored in `root` (empty when absent or corrupt).
#[must_use]
pub fn read_limits(root: &Path) -> BTreeMap<String, usize> {
    std::fs::read(root.join(LIMITS_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Records an operator override for `lane` (clamped to the valid range) and
/// returns the stored value. The worker applies it on its next poll.
///
/// # Errors
/// `InvalidInput` for an unknown lane, otherwise the I/O error of the write.
pub fn write_limit(root: &Path, lane: &str, limit: usize) -> io::Result<usize> {
    if !LANE_NAMES.contains(&lane) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unknown lane `{lane}` (known: {})", LANE_NAMES.join(", ")),
        ));
    }
    let limit = limit.clamp(MIN_PERMITS, MAX_PERMITS);
    let mut limits = read_limits(root);
    limits.insert(lane.to_owned(), limit);
    write_json_atomic(&root.join(LIMITS_FILE), &limits)?;
    Ok(limit)
}

/// The status last published by a worker on `root` (empty when none).
#[must_use]
pub fn read_status(root: &Path) -> Vec<LaneStatus> {
    std::fs::read(root.join(STATUS_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::{JobLanes, LANE_MEMORY, LANE_WORK_DRIVER, read_limits, read_status, write_limit};
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn an_override_is_applied_clamped_and_reported_as_changed() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("dir"))?;
        let lanes = JobLanes::new(2, 1);
        assert!(
            lanes.apply_overrides(dir.path()).is_empty(),
            "no file, no change"
        );

        assert_eq!(
            write_limit(dir.path(), LANE_WORK_DRIVER, 5).map_err(ctx("write"))?,
            5
        );
        assert_eq!(lanes.apply_overrides(dir.path()), vec![LANE_WORK_DRIVER]);
        assert_eq!(lanes.work_driver().limit(), 5);
        assert!(lanes.apply_overrides(dir.path()).is_empty(), "idempotent");

        assert_eq!(
            write_limit(dir.path(), LANE_MEMORY, 0).map_err(ctx("write"))?,
            1
        );
        assert_eq!(lanes.memory().limit(), 1);
        Ok(())
    }

    #[test]
    fn an_unknown_lane_is_rejected_and_a_corrupt_file_reads_as_empty() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("dir"))?;
        assert!(write_limit(dir.path(), "nope", 3).is_err());
        std::fs::write(dir.path().join("lane-limits.json"), b"{not json")
            .map_err(ctx("corrupt"))?;
        assert!(read_limits(dir.path()).is_empty());
        let lanes = JobLanes::new(2, 1);
        assert!(lanes.apply_overrides(dir.path()).is_empty());
        Ok(())
    }

    #[test]
    fn published_status_shows_limit_and_permits_in_use() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("dir"))?;
        let lanes = JobLanes::new(3, 1);
        let _held = lanes
            .work_driver()
            .try_acquire()
            .ok_or(TestError::Missing("permit"))?;
        lanes.publish_status(dir.path()).map_err(ctx("publish"))?;
        let status = read_status(dir.path());
        let work = status
            .iter()
            .find(|s| s.lane == LANE_WORK_DRIVER)
            .ok_or(TestError::Missing("work_driver status"))?;
        assert_eq!((work.limit, work.in_use, work.available), (3, 1, 2));
        assert_eq!(status.len(), 2);
        Ok(())
    }
}
