//! Owner-label garbage collection with lease-epoch fencing.
//!
//! A container carries `harw.owner`, `harw.work_id` and `harw.lease_epoch`
//! labels (set at create time). Labels are **not authenticated**, so the
//! planner only ever acts on containers whose labels parse completely and
//! whose owner and tenant are ours, and it never removes anything a live
//! attempt of the current epoch still uses.

use std::collections::BTreeMap;
use std::time::Duration;

use harw_container_model::{ContainerId, OwnerLabels};

use crate::executor::OciExecutor;

/// What the runner's store says about attempts.
pub trait AttemptView {
    /// The job's current lease epoch, `None` for a job the store does not
    /// know (finished and purged, or never ours).
    fn current_epoch(&self, work_id: &str) -> Option<u64>;
    /// Whether an attempt of `work_id` under `epoch` is still running.
    fn is_live(&self, work_id: &str, epoch: u64) -> bool;
}

/// One container as the engine lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    /// Engine-assigned id.
    pub id: ContainerId,
    /// Labels as reported (unauthenticated).
    pub labels: BTreeMap<String, String>,
    /// Creation time from the engine (not from a label).
    pub created_unix: i64,
    /// Whether the container runs.
    pub running: bool,
}

/// GC knobs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GcPolicy {
    /// Containers younger than this are never removed.
    pub grace: Duration,
}

impl Default for GcPolicy {
    fn default() -> Self {
        Self {
            grace: Duration::from_secs(120),
        }
    }
}

/// Why a container is to be removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Its lease epoch is older than the job's current one (a lost lease).
    StaleEpoch,
    /// Same epoch, but the attempt is no longer live.
    Finished,
    /// The job is unknown to the store.
    Orphaned,
}

/// One planned removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal {
    /// The container (by id, never by name).
    pub id: ContainerId,
    /// Why.
    pub reason: Reason,
}

/// Plans removals. Pure: the caller lists, this decides, the caller removes.
#[must_use]
pub fn plan_gc(
    listed: &[Listed],
    view: &dyn AttemptView,
    now_unix: i64,
    owner: &str,
    tenant: &str,
    policy: GcPolicy,
) -> Vec<Removal> {
    let grace = i64::try_from(policy.grace.as_secs()).unwrap_or(i64::MAX);
    let mut out = Vec::new();
    for item in listed {
        // Unparseable or foreign labels are not ours to touch.
        let Ok(labels) = OwnerLabels::from_map(&item.labels) else {
            continue;
        };
        if labels.owner != owner || labels.tenant != tenant {
            continue;
        }
        if now_unix.saturating_sub(item.created_unix) < grace {
            continue;
        }
        let reason = match view.current_epoch(&labels.work_id) {
            // A higher epoch than the store knows: the store may be behind.
            Some(current) if labels.lease_epoch > current => continue,
            Some(current) if labels.lease_epoch < current => Reason::StaleEpoch,
            Some(_) => {
                // Live per the store wins even when the engine already shows
                // the container as exited (the outcome is not durable yet).
                if view.is_live(&labels.work_id, labels.lease_epoch) {
                    continue;
                }
                Reason::Finished
            }
            // A lost or reset store must not cost running containers: an
            // unknown job is only collected once its container is not running.
            None if item.running => continue,
            None => Reason::Orphaned,
        };
        out.push(Removal {
            id: item.id.clone(),
            reason,
        });
    }
    out
}

/// What a collection pass did.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GcReport {
    /// Containers inspected.
    pub seen: usize,
    /// Containers removed.
    pub removed: Vec<Removal>,
    /// Removals that failed (the container stays for the next pass).
    pub failed: usize,
}

impl OciExecutor {
    /// Lists this runner's containers, plans and removes the garbage.
    ///
    /// # Errors
    /// The listing failed (nothing was removed).
    pub fn collect_garbage(
        &self,
        view: &dyn AttemptView,
        now_unix: i64,
        policy: GcPolicy,
    ) -> Result<GcReport, crate::OciError> {
        let (owner, tenant) = self.owner_tenant();
        let ids = self.list_owned()?;
        let mut listed = Vec::with_capacity(ids.len());
        for id in &ids {
            // A container that vanished between list and inspect is gone.
            if let Some(item) = self.describe(id)? {
                listed.push(item);
            }
        }
        let plan = plan_gc(&listed, view, now_unix, &owner, &tenant, policy);
        let mut report = GcReport {
            seen: listed.len(),
            ..GcReport::default()
        };
        for removal in plan {
            if self.remove_by_id(&removal.id).is_ok() {
                report.removed.push(removal);
            } else {
                report.failed += 1;
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestResult, ctx};

    struct View {
        epoch: Option<u64>,
        live: bool,
    }

    impl AttemptView for View {
        fn current_epoch(&self, _: &str) -> Option<u64> {
            self.epoch
        }
        fn is_live(&self, _: &str, _: u64) -> bool {
            self.live
        }
    }

    fn item(owner: &str, epoch: u64, created: i64, running: bool) -> TestResult<Listed> {
        let labels = OwnerLabels::new(owner, "job-1", epoch, epoch, "tenant-a", "no_network")
            .map_err(ctx("labels"))?;
        Ok(Listed {
            id: ContainerId::new(&"a".repeat(64)).map_err(ctx("id"))?,
            labels: labels.to_map(),
            created_unix: created,
            running,
        })
    }

    fn plan(listed: &[Listed], view: &View) -> Vec<Removal> {
        plan_gc(listed, view, 10_000, "me", "tenant-a", GcPolicy::default())
    }

    #[test]
    fn lower_epoch_is_removed_equal_finished_is_removed_higher_is_kept() -> TestResult {
        let view = View {
            epoch: Some(5),
            live: false,
        };
        let lower = plan(&[item("me", 4, 0, true)?], &view);
        assert_eq!(lower.first().map(|r| r.reason), Some(Reason::StaleEpoch));
        let equal = plan(&[item("me", 5, 0, false)?], &view);
        assert_eq!(equal.first().map(|r| r.reason), Some(Reason::Finished));
        assert!(plan(&[item("me", 6, 0, false)?], &view).is_empty());
        Ok(())
    }

    #[test]
    fn a_live_attempt_of_the_current_epoch_is_never_removed() -> TestResult {
        let view = View {
            epoch: Some(5),
            live: true,
        };
        assert!(plan(&[item("me", 5, 0, true)?], &view).is_empty());
        // ...but a stale epoch is removed even while it still runs.
        assert_eq!(plan(&[item("me", 4, 0, true)?], &view).len(), 1);
        Ok(())
    }

    #[test]
    fn grace_foreign_owner_and_forged_labels_are_kept() -> TestResult {
        let view = View {
            epoch: Some(5),
            live: false,
        };
        assert!(
            plan(&[item("me", 4, 9_990, false)?], &view).is_empty(),
            "within grace"
        );
        assert!(
            plan(&[item("other", 4, 0, false)?], &view).is_empty(),
            "foreign owner"
        );
        let mut forged = item("me", 4, 0, false)?;
        forged
            .labels
            .insert("harw.work_id".into(), "bad id; rm -rf".into());
        assert!(plan(&[forged], &view).is_empty(), "bad charset ignored");
        let mut partial = item("me", 4, 0, false)?;
        partial.labels.remove("harw.lease_epoch");
        assert!(
            plan(&[partial], &view).is_empty(),
            "incomplete labels ignored"
        );
        Ok(())
    }

    #[test]
    fn unknown_jobs_are_orphans() -> TestResult {
        let view = View {
            epoch: None,
            live: false,
        };
        let out = plan(&[item("me", 1, 0, false)?], &view);
        assert_eq!(out.first().map(|r| r.reason), Some(Reason::Orphaned));
        assert!(
            plan(&[item("me", 1, 0, true)?], &view).is_empty(),
            "a running container of an unknown job is kept"
        );
        Ok(())
    }
}
