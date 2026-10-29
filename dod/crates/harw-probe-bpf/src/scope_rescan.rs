//! Periodic rescan of the `include_descendants = true` cgroup roots
//! (PL-97 E3b: DoD observes cgroups created after attach).
//!
//! # Purpose
//! The BPF programs admit an event only when `bpf_get_current_cgroup_id()`
//! is a key of their `SCOPE_CGROUP_IDS` map. That map is filled before
//! attach; a `harw-job-<attempt>` cgroup created afterwards would stay
//! invisible until a probe restart. [`ScopeRescan`] closes that gap in
//! userspace: on a bounded interval ([`SCOPE_RESCAN_INTERVAL`], 2 s) it walks
//! the profile roots ([`harw_dod_bpf::cgroup_walk::walk_descendants`]: no
//! symlink followed, bounded depth and entry count, vanished directories
//! tolerated), plans the change with [`harw_dod_bpf::DescendantScope`] and
//! applies the same [`harw_dod_bpf::ScopeDelta`] to every loaded object
//! through [`harw_dod_bpf::BpfScopeWriter`]: new descendants are inserted,
//! vanished ones retired, the explicit roots are never touched.
//!
//! # Never silent
//! The map capacity is fixed (`MAX_SCOPE_CGROUP_IDS`). A cgroup that does
//! not fit is not inserted; the round counts it in
//! [`ScopeRescanStats::overflow_total`], logs a warning and the caller
//! (`crate::collect::rescan_scope_once`) reports `SensorDegraded` for every
//! wire sensor. The same holds for directories beyond the depth bound, a
//! missing or re-created root, an incomplete walk and a rejected map update.
//! `SensorDegraded` is sent once, on the transition from a fully observed to
//! a degraded scope (it is meant to be a rare signal); the warning is logged
//! again whenever the shape of the degradation changes, and the counters
//! grow every round. A persisting condition therefore never repeats every
//! interval, and is never silent either.
//!
//! # Partial map updates
//! Writers are not transactional, even within one handle. A failed round keeps
//! the inverse delta until every handle has restored the last confirmed scope.
//! New plans are applied only after that reconciliation; failed reconciliation
//! remains degraded even if a fresh walk would otherwise produce an empty plan.
//! This state is bounded by one delta and never silently reports recovery while
//! maps may disagree. It does not make the sequential kernel updates atomic.
//!
//! # Roots missing at start
//! A configured root that did not resolve when the probe started
//! (`crate::profile::resolve_scope`) has no kernel id and is therefore not
//! part of the tracked scope. It is never re-resolved later (a cgroup created
//! under that path afterwards is not silently admitted, see
//! `harw_dod_config`), but it is not forgotten either:
//! [`ScopeRescan::with_startup`] carries the count into every round's
//! [`ScopeHealth::roots_unavailable`], so the scope stays degraded for the
//! lifetime of the process and never logs a false recovery. The startup
//! health is also the baseline of the first round: a start that was already
//! degraded (and reported once by `main::run`) is not reported again.
//!
//! # Privileges
//! No new capability: the walk needs `lstat` and directory listing on the
//! cgroup2 mount (`--cgroup-root`), the map update uses the object the probe
//! already holds. Because this rescan runs after `restrict_self`, `main::run`
//! adds `--cgroup-root` to the Landlock ruleset as a list-only root
//! (`crate::landlock::RootAccess::ListDirectories`) whenever a descendant
//! scope is active, and only then; see `crate::landlock`. Should listing
//! still fail (`EACCES`), the round is incomplete (degraded) and never
//! retires an id.
//!
//! # Concurrency
//! Owned and driven by the single collect thread (`main::collect_forever`).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use harw_dod_bpf::cgroup_walk::{self, HostCgroupFs, WalkLimits, WalkReport};
use harw_dod_bpf::{BpfHandle, BpfScopeWriter, DescendantScope, ScopeDelta};

/// Default interval between two rescans.
pub const SCOPE_RESCAN_INTERVAL: Duration = Duration::from_secs(2);

/// Cumulative counters of the rescan, part of every degradation log line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScopeRescanStats {
    /// Rescan rounds run.
    pub rounds: u64,
    /// Descendant ids in committed insertion deltas (excluding rollback writes).
    pub inserted_total: u64,
    /// Descendant ids in committed retirement deltas (excluding rollback writes).
    pub retired_total: u64,
    /// Cgroups not inserted because the map was full, summed over rounds.
    pub overflow_total: u64,
    /// Cgroups not inserted in the last round.
    pub overflowed_last_round: usize,
    /// Rounds whose walk was incomplete (nothing retired then).
    pub incomplete_rounds: u64,
    /// Rounds in which at least one map update was rejected.
    pub apply_failures: u64,
    /// Ids currently installed (roots plus descendants).
    pub installed: usize,
    /// Map entries this scope may occupy.
    pub capacity: usize,
}

/// What kept a round from observing the whole profile scope.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScopeHealth {
    /// Observed descendants not inserted because the map was full.
    pub overflowed: usize,
    /// Descendants beyond the walk's depth bound.
    pub depth_limited: usize,
    /// Explicit roots that are missing or were re-created.
    pub roots_unavailable: usize,
    /// The walk was incomplete (I/O error or entry budget exhausted).
    pub incomplete: bool,
    /// A map update was rejected; the round was not committed.
    pub apply_failed: bool,
}

impl ScopeHealth {
    /// `true` when part of the profile scope is not observed.
    #[must_use]
    pub const fn is_degraded(&self) -> bool {
        self.overflowed > 0
            || self.depth_limited > 0
            || self.roots_unavailable > 0
            || self.incomplete
            || self.apply_failed
    }
}

/// The outcome of one rescan round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RescanRound {
    /// The delta planned (and, unless `health.apply_failed`, applied).
    pub delta: ScopeDelta,
    /// Degradation state of this round.
    pub health: ScopeHealth,
    /// `true` when this round turned a fully observed scope degraded: the
    /// caller reports `SensorDegraded` exactly then.
    pub newly_degraded: bool,
}

/// The rescan state of one probe: the tracked scope plus scheduling and
/// counters.
#[derive(Debug)]
pub struct ScopeRescan {
    scope: DescendantScope,
    cgroup_root: PathBuf,
    interval: Duration,
    limits: WalkLimits,
    next_due: Instant,
    stats: ScopeRescanStats,
    last_health: ScopeHealth,
    /// Configured roots that did not resolve at start; counted as
    /// unavailable in every round.
    unresolved_roots: usize,
    /// Inverse of the last uncommitted delta. A writer may fail after changing
    /// entries, so all handles must acknowledge this rollback before replanning.
    /// Bounded by one scope delta, even under persistent failure and cgroup churn.
    pending_rollback: Option<ScopeDelta>,
}

impl ScopeRescan {
    /// Starts rescanning `scope` (already expanded and attached) below the
    /// cgroup2 mount `cgroup_root`; the first round is due one `interval`
    /// after `now`.
    #[must_use]
    pub fn new(
        scope: DescendantScope,
        cgroup_root: PathBuf,
        interval: Duration,
        limits: WalkLimits,
        now: Instant,
    ) -> Self {
        let stats = ScopeRescanStats {
            installed: scope.installed_ids().len(),
            capacity: scope.capacity(),
            ..ScopeRescanStats::default()
        };
        Self {
            next_due: now.checked_add(interval).unwrap_or(now),
            scope,
            cgroup_root,
            interval,
            limits,
            stats,
            last_health: ScopeHealth::default(),
            unresolved_roots: 0,
            pending_rollback: None,
        }
    }

    /// Carries the attach-time state into the rescan.
    ///
    /// - `unresolved_roots`: configured roots that did not resolve at start.
    ///   They are added to [`ScopeHealth::roots_unavailable`] of every round,
    ///   so the scope never reports itself fully observed while part of the
    ///   profile is missing (see the module docs, "Roots missing at start").
    /// - `startup`: the health of the attach-time expansion. It is the
    ///   baseline of the first round, so a degradation the caller already
    ///   reported at start is not reported again as `newly_degraded`.
    #[must_use]
    pub fn with_startup(mut self, unresolved_roots: usize, startup: ScopeHealth) -> Self {
        self.unresolved_roots = unresolved_roots;
        self.last_health = startup;
        self
    }

    /// Runs [`Self::rescan`] when the interval has elapsed.
    pub fn rescan_if_due(
        &mut self,
        writer: &dyn BpfScopeWriter,
        handles: &[&BpfHandle],
        now: Instant,
    ) -> Option<RescanRound> {
        (now >= self.next_due).then(|| self.rescan(writer, handles, now))
    }

    /// Walks, plans, applies the delta to every handle and commits it when
    /// every handle accepted it. A rejected update leaves the tracked scope
    /// unchanged and retains its inverse delta. Before the next new update,
    /// every handle must acknowledge that rollback (including the failed handle,
    /// which may have partially mutated). Only then can a fresh plan be applied.
    ///
    /// `handles` must be the same attached objects for the lifetime of this
    /// rescan. An empty handle set cannot acknowledge a nonempty update.
    pub fn rescan(
        &mut self,
        writer: &dyn BpfScopeWriter,
        handles: &[&BpfHandle],
        now: Instant,
    ) -> RescanRound {
        self.next_due = now.checked_add(self.interval).unwrap_or(now);
        // Restore the last confirmed state first: re-planning directly after a
        // partial failure can forget an inserted id that has since disappeared,
        // or a removed id that is observed again. No new writes while uncertain.
        let rollback_failed = self
            .pending_rollback
            .as_ref()
            .is_some_and(|delta| !Self::apply_to_all(writer, handles, delta));
        if !rollback_failed {
            self.pending_rollback = None;
        }
        let report = cgroup_walk::walk_descendants(
            &HostCgroupFs,
            &self.cgroup_root,
            self.scope.roots(),
            self.limits,
        );
        let plan = self.scope.plan(&report.observation);
        let delta = plan.delta.clone();
        let overflowed = plan.overflowed;

        let mut apply_failed = rollback_failed;
        if !rollback_failed {
            // Retain the inverse BEFORE calling the writer: even Err may mean
            // some entries changed. Keep it until all handles and the tracker
            // have accepted this round. Roots never occur in either list.
            if !delta.is_empty() {
                self.pending_rollback = Some(ScopeDelta {
                    insert: delta.remove.clone(),
                    remove: delta.insert.clone(),
                });
            }
            apply_failed = !Self::apply_to_all(writer, handles, &delta);
            if !apply_failed && self.scope.commit(plan).is_err() {
                // The plan belongs to this state; still fail closed if that
                // invariant is ever violated, retaining the rollback.
                apply_failed = true;
            }
            if !apply_failed {
                self.pending_rollback = None;
            }
        }

        let health = ScopeHealth {
            overflowed,
            depth_limited: report.depth_limited,
            roots_unavailable: report
                .roots_unavailable
                .saturating_add(self.unresolved_roots),
            incomplete: !report.observation.complete,
            apply_failed,
        };
        self.record(&delta, health);
        self.log_round(&delta, health, &report);

        let newly_degraded = health.is_degraded() && !self.last_health.is_degraded();
        self.last_health = health;
        RescanRound {
            delta,
            health,
            newly_degraded,
        }
    }

    /// Attempts every handle, treating even partial per-object writes as
    /// uncertain until all acknowledge success. Rollbacks use this same path.
    fn apply_to_all(
        writer: &dyn BpfScopeWriter,
        handles: &[&BpfHandle],
        delta: &ScopeDelta,
    ) -> bool {
        if delta.is_empty() {
            return true;
        }
        let mut succeeded = !handles.is_empty();
        for handle in handles {
            if let Err(error) = writer.apply_scope_delta(handle, delta) {
                succeeded = false;
                tracing::debug!(
                    attach_point = handle.attach_point(),
                    error = %error,
                    "bpf scope map update rejected; reconciliation required"
                );
            }
        }
        succeeded
    }

    /// Updates the cumulative counters for one round.
    fn record(&mut self, delta: &ScopeDelta, health: ScopeHealth) {
        let to_u64 = |count: usize| u64::try_from(count).unwrap_or(u64::MAX);
        let stats = &mut self.stats;
        stats.rounds = stats.rounds.saturating_add(1);
        if !health.apply_failed {
            stats.inserted_total = stats
                .inserted_total
                .saturating_add(to_u64(delta.insert.len()));
            stats.retired_total = stats
                .retired_total
                .saturating_add(to_u64(delta.remove.len()));
        }
        stats.overflow_total = stats
            .overflow_total
            .saturating_add(to_u64(health.overflowed));
        stats.overflowed_last_round = health.overflowed;
        if health.incomplete {
            stats.incomplete_rounds = stats.incomplete_rounds.saturating_add(1);
        }
        if health.apply_failed {
            stats.apply_failures = stats.apply_failures.saturating_add(1);
        }
        stats.installed = self.scope.installed_ids().len();
    }

    /// Logs an applied change, and a degradation whenever its shape changes
    /// (plus the recovery), so a persisting condition does not repeat every
    /// interval.
    fn log_round(&self, delta: &ScopeDelta, health: ScopeHealth, report: &WalkReport) {
        let stats = self.stats;
        if !delta.is_empty() && !health.apply_failed {
            tracing::info!(
                inserted = delta.insert.len(),
                retired = delta.remove.len(),
                inserted_total = stats.inserted_total,
                retired_total = stats.retired_total,
                installed = stats.installed,
                capacity = stats.capacity,
                "bpf cgroup scope updated by rescan"
            );
        }
        if health == self.last_health {
            return;
        }
        if health.is_degraded() {
            tracing::warn!(
                overflowed = stats.overflowed_last_round,
                overflow_total = stats.overflow_total,
                depth_limited = health.depth_limited,
                roots_unavailable = health.roots_unavailable,
                walk_incomplete = health.incomplete,
                walk_errors = report.errors,
                walk_truncated = report.truncated,
                incomplete_rounds = stats.incomplete_rounds,
                apply_failed = health.apply_failed,
                apply_failures = stats.apply_failures,
                installed = stats.installed,
                capacity = stats.capacity,
                "bpf cgroup scope degraded: part of the profile scope is not observed"
            );
        } else {
            tracing::info!(
                rounds = stats.rounds,
                installed = stats.installed,
                capacity = stats.capacity,
                overflow_total = stats.overflow_total,
                "bpf cgroup scope recovered: the whole profile scope is observed again"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use harw_dod_bpf::cgroup_walk::{HostCgroupFs, WalkLimits, host_path, resolve_cgroup};
    use harw_dod_bpf::{
        BpfError, BpfHandle, BpfProgramSource, BpfScopeWriter, DescendantScope, FixtureBpfLoader,
        MAX_SCOPE_CGROUP_IDS, ScopeDelta, ScopeMapOp,
    };
    use harw_types::SensorId;

    use super::{SCOPE_RESCAN_INTERVAL, ScopeHealth, ScopeRescan};
    use crate::test_support::{TestError, TestResult, ctx};

    /// Injects failure after one entry has changed, mirroring a kernel map
    /// syscall failure halfway through an otherwise valid delta.
    struct PartialWriter<'a> {
        loader: &'a FixtureBpfLoader,
        fail_calls: BTreeSet<usize>,
        calls: AtomicUsize,
    }

    impl BpfScopeWriter for PartialWriter<'_> {
        fn apply_scope_delta(
            &self,
            handle: &BpfHandle,
            delta: &ScopeDelta,
        ) -> Result<(), BpfError> {
            let call = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
            if self.fail_calls.contains(&call) {
                let partial = if let Some(id) = delta.remove.first() {
                    ScopeDelta {
                        remove: vec![*id],
                        insert: Vec::new(),
                    }
                } else {
                    ScopeDelta {
                        insert: delta.insert.first().copied().into_iter().collect(),
                        remove: Vec::new(),
                    }
                };
                self.loader.apply_scope_delta(handle, &partial)?;
                return Err(BpfError::ScopeMapUpdateFailed);
            }
            self.loader.apply_scope_delta(handle, delta)
        }
    }

    /// Gefälschter cgroup-Baum unter einem Tempdir, wie die Sonde ihn über
    /// `--cgroup-root` sähe.
    struct FakeTree {
        dir: tempfile::TempDir,
    }

    impl FakeTree {
        fn new() -> TestResult<Self> {
            let tree = Self {
                dir: tempfile::tempdir().map_err(ctx("tempdir"))?,
            };
            tree.mkcg("/harw.slice")?;
            Ok(tree)
        }

        fn root(&self) -> &Path {
            self.dir.path()
        }

        fn mkcg(&self, path: &str) -> TestResult<u64> {
            let dir = host_path(self.root(), path);
            std::fs::create_dir_all(&dir).map_err(ctx("create fake cgroup"))?;
            std::fs::write(dir.join("cgroup.procs"), b"").map_err(ctx("interface file"))?;
            self.id(path)
        }

        fn rmcg(&self, path: &str) -> TestResult {
            std::fs::remove_dir_all(host_path(self.root(), path)).map_err(ctx("remove cgroup"))
        }

        fn id(&self, path: &str) -> TestResult<u64> {
            Ok(resolve_cgroup(&HostCgroupFs, self.root(), path)
                .map_err(ctx("resolve fake cgroup"))?
                .id)
        }
    }

    /// Ein Sondenaufbau wie in `main::run`: Scope vor dem Anheften expandiert,
    /// beide procmon-Objekte mit diesem Scope über die Fixture geladen.
    struct Probe {
        loader: FixtureBpfLoader,
        handles: Vec<BpfHandle>,
        rescan: ScopeRescan,
        start: Instant,
    }

    impl Probe {
        fn attach(tree: &FakeTree, capacity: usize) -> TestResult<Self> {
            let root = resolve_cgroup(&HostCgroupFs, tree.root(), "/harw.slice")
                .map_err(ctx("resolve root"))?;
            let mut scope = DescendantScope::new(vec![root], capacity).map_err(ctx("scope"))?;
            let report = harw_dod_bpf::cgroup_walk::walk_descendants(
                &HostCgroupFs,
                tree.root(),
                scope.roots(),
                WalkLimits::default(),
            );
            let plan = scope.plan(&report.observation);
            scope.commit(plan).map_err(ctx("attach-time expansion"))?;

            let loader = FixtureBpfLoader::new(Vec::new());
            let source = || BpfProgramSource::Embedded(Cow::Borrowed(b"ELF".as_slice()));
            let contracts = harw_dod_procmon::procmon_contracts(
                SensorId::from_str("probe-bpf-procmon-0"),
                source(),
                source(),
                scope.bpf_scope(),
            );
            let handles = contracts
                .iter()
                .map(|contract| loader.load_contract(contract))
                .collect::<Result<Vec<_>, _>>()
                .map_err(ctx("fixture loads the contracts"))?;
            let start = Instant::now();
            let rescan = ScopeRescan::new(
                scope,
                tree.root().to_path_buf(),
                SCOPE_RESCAN_INTERVAL,
                WalkLimits::default(),
                start,
            );
            Ok(Self {
                loader,
                handles,
                rescan,
                start,
            })
        }

        fn rescan(&mut self) -> super::RescanRound {
            let handles: Vec<&BpfHandle> = self.handles.iter().collect();
            let now = self.start + SCOPE_RESCAN_INTERVAL * 10;
            self.rescan.rescan(&self.loader, &handles, now)
        }

        /// Inhalt der Scope-Map; beide Objekte müssen denselben tragen.
        fn map(&self) -> TestResult<BTreeSet<u64>> {
            let mut maps = self.handles.iter().map(|handle| {
                self.loader
                    .scope_ids(handle)
                    .ok_or(TestError::Missing("map"))
            });
            let first = maps.next().ok_or(TestError::Missing("handle"))??;
            for other in maps {
                if other? != first {
                    return Err(TestError::Unexpected("scope maps diverged".into()));
                }
            }
            Ok(first)
        }
    }

    #[test]
    fn cgroup_created_after_attach_is_in_scope_after_one_rescan() -> TestResult {
        let tree = FakeTree::new()?;
        let root = tree.id("/harw.slice")?;
        let job1 = tree.mkcg("/harw.slice/harw-job-1")?;
        let mut probe = Probe::attach(&tree, MAX_SCOPE_CGROUP_IDS)?;
        assert_eq!(probe.map()?, BTreeSet::from([root, job1]));

        let job2 = tree.mkcg("/harw.slice/harw-job-2")?;
        let nested = tree.mkcg("/harw.slice/harw-job-2/step")?;
        let round = probe.rescan();
        assert_eq!(round.delta.insert, vec![job2, nested]);
        assert!(!round.health.is_degraded());
        assert_eq!(probe.map()?, BTreeSet::from([root, job1, job2, nested]));
        let inserts = probe
            .loader
            .scope_map_ops()
            .into_iter()
            .filter(|op| matches!(op, ScopeMapOp::Insert { cgroup_id, .. } if *cgroup_id == job2))
            .count();
        assert_eq!(inserts, probe.handles.len());
        assert_eq!(probe.rescan.stats.inserted_total, 2);
        Ok(())
    }

    #[test]
    fn removed_cgroup_is_retired_and_explicit_root_is_never_removed() -> TestResult {
        let tree = FakeTree::new()?;
        let root = tree.id("/harw.slice")?;
        let job1 = tree.mkcg("/harw.slice/harw-job-1")?;
        let job2 = tree.mkcg("/harw.slice/harw-job-2")?;
        let mut probe = Probe::attach(&tree, MAX_SCOPE_CGROUP_IDS)?;

        tree.rmcg("/harw.slice/harw-job-1")?;
        let round = probe.rescan();
        assert_eq!(round.delta.remove, vec![job1]);
        assert_eq!(probe.map()?, BTreeSet::from([root, job2]));

        // Auch eine verschwundene Wurzel bleibt in der Map; ihre Nachfahren
        // sind mit ihr verschwunden und werden zurückgezogen.
        tree.rmcg("/harw.slice")?;
        let round = probe.rescan();
        assert_eq!(round.delta.remove, vec![job2]);
        assert!(!round.delta.remove.contains(&root));
        assert_eq!(round.health.roots_unavailable, 1);
        assert!(round.newly_degraded);
        assert_eq!(probe.map()?, BTreeSet::from([root]));
        let root_removed = probe
            .loader
            .scope_map_ops()
            .into_iter()
            .any(|op| matches!(op, ScopeMapOp::Remove { cgroup_id, .. } if cgroup_id == root));
        assert!(!root_removed);
        Ok(())
    }

    #[test]
    fn capacity_overflow_is_counted_reported_once_and_map_stays_at_capacity() -> TestResult {
        let tree = FakeTree::new()?;
        let mut probe = Probe::attach(&tree, 3)?;
        for job in 1..=4 {
            tree.mkcg(&format!("/harw.slice/harw-job-{job}"))?;
        }
        let first = probe.rescan();
        assert_eq!(first.delta.insert.len(), 2);
        assert_eq!(first.health.overflowed, 2);
        assert!(first.newly_degraded);
        assert_eq!(probe.map()?.len(), 3);
        assert_eq!(probe.rescan.stats.overflow_total, 2);

        let second = probe.rescan();
        assert!(second.delta.is_empty());
        assert_eq!(second.health.overflowed, 2);
        assert!(
            !second.newly_degraded,
            "a persisting overflow is reported once"
        );
        assert_eq!(probe.rescan.stats.overflow_total, 4);
        assert_eq!(probe.rescan.stats.overflowed_last_round, 2);
        assert_eq!(probe.map()?.len(), 3);
        Ok(())
    }

    #[test]
    fn symlink_to_outside_the_root_is_not_admitted() -> TestResult {
        let tree = FakeTree::new()?;
        let outside = tree.mkcg("/outside")?;
        let mut probe = Probe::attach(&tree, MAX_SCOPE_CGROUP_IDS)?;
        std::os::unix::fs::symlink(
            host_path(tree.root(), "/outside"),
            host_path(tree.root(), "/harw.slice/escape"),
        )
        .map_err(ctx("symlink"))?;
        let round = probe.rescan();
        assert!(round.delta.is_empty());
        assert!(!probe.map()?.contains(&outside));
        Ok(())
    }

    #[test]
    fn rejected_map_update_is_retried_in_the_next_round() -> TestResult {
        let tree = FakeTree::new()?;
        let mut probe = Probe::attach(&tree, MAX_SCOPE_CGROUP_IDS)?;
        let job = tree.mkcg("/harw.slice/harw-job-1")?;
        probe.loader.fail_next_scope_updates(1);
        let failed = probe.rescan();
        assert!(failed.health.apply_failed);
        assert!(failed.newly_degraded);
        assert_eq!(probe.rescan.stats.apply_failures, 1);
        assert_eq!(probe.rescan.stats.inserted_total, 0);

        let retried = probe.rescan();
        assert_eq!(retried.delta.insert, vec![job]);
        assert!(!retried.health.is_degraded());
        assert!(probe.map()?.contains(&job));
        Ok(())
    }

    #[test]
    fn failed_insert_then_disappearance_reconciles_every_handle() -> TestResult {
        let tree = FakeTree::new()?;
        let mut probe = Probe::attach(&tree, MAX_SCOPE_CGROUP_IDS)?;
        let before = probe.map()?;
        let job = tree.mkcg("/harw.slice/harw-job-1")?;
        probe.loader.fail_next_scope_updates(1);
        assert!(probe.rescan().health.apply_failed);
        assert!(probe.map().is_err(), "fixture must actually diverge");
        tree.rmcg("/harw.slice/harw-job-1")?;

        let recovered = probe.rescan();
        assert!(!recovered.health.is_degraded());
        assert_eq!(probe.map()?, before);
        assert!(!probe.map()?.contains(&job));
        Ok(())
    }

    #[test]
    fn failed_reconciliation_cannot_report_recovery_on_an_empty_new_plan() -> TestResult {
        let tree = FakeTree::new()?;
        let mut probe = Probe::attach(&tree, MAX_SCOPE_CGROUP_IDS)?;
        let before = probe.map()?;
        tree.mkcg("/harw.slice/harw-job-1")?;
        probe.loader.fail_next_scope_updates(1);
        assert!(probe.rescan().health.apply_failed);
        tree.rmcg("/harw.slice/harw-job-1")?;
        probe.loader.fail_next_scope_updates(probe.handles.len());
        let failed_again = probe.rescan();
        assert!(failed_again.health.apply_failed);
        assert!(!failed_again.newly_degraded, "degradation persists");
        assert!(probe.map().is_err());
        assert!(!probe.rescan().health.is_degraded());
        assert_eq!(probe.map()?, before);
        Ok(())
    }

    #[test]
    fn failed_removal_is_restored_before_an_incomplete_walk() -> TestResult {
        let tree = FakeTree::new()?;
        let job = tree.mkcg("/harw.slice/harw-job-1")?;
        let mut probe = Probe::attach(&tree, MAX_SCOPE_CGROUP_IDS)?;
        let before = probe.map()?;
        tree.rmcg("/harw.slice/harw-job-1")?;
        probe.loader.fail_next_scope_updates(1);
        assert!(probe.rescan().health.apply_failed);
        assert!(probe.map().is_err());
        // An incomplete walk must preserve the last confirmed membership.
        probe.rescan.limits.max_entries = 0;
        let incomplete = probe.rescan();
        assert!(incomplete.health.incomplete);
        assert!(!incomplete.health.apply_failed);
        assert_eq!(probe.map()?, before);
        assert!(probe.map()?.contains(&job));
        probe.rescan.limits = WalkLimits::default();
        assert!(!probe.rescan().health.is_degraded());
        assert!(!probe.map()?.contains(&job));
        Ok(())
    }

    #[test]
    fn partial_entry_failure_and_failed_fresh_plan_keep_their_own_rollback() -> TestResult {
        let tree = FakeTree::new()?;
        let mut probe = Probe::attach(&tree, MAX_SCOPE_CGROUP_IDS)?;
        let root = tree.id("/harw.slice")?;
        tree.mkcg("/harw.slice/old-1")?;
        tree.mkcg("/harw.slice/old-2")?;
        let writer = PartialWriter {
            loader: &probe.loader,
            fail_calls: BTreeSet::from([1, 5]),
            calls: AtomicUsize::new(0),
        };
        let handles: Vec<_> = probe.handles.iter().collect();
        let first = probe.rescan.rescan(&writer, &handles, probe.start);
        assert!(first.health.apply_failed);
        assert!(probe.map().is_err(), "partial entry writes must diverge");
        // Keep the inode distinct from the old IDs while changing desired scope.
        let new = tree.mkcg("/harw.slice/new")?;
        tree.rmcg("/harw.slice/old-1")?;
        tree.rmcg("/harw.slice/old-2")?;
        let second = probe.rescan.rescan(&writer, &handles, probe.start);
        assert!(
            second.health.apply_failed,
            "fresh plan also failed after rollback"
        );
        assert!(!second.newly_degraded);
        assert_eq!(second.delta.insert, vec![new]);
        assert!(probe.rescan.pending_rollback.is_some());
        tree.rmcg("/harw.slice/new")?;
        let third = probe.rescan.rescan(&writer, &handles, probe.start);
        assert!(!third.health.is_degraded());
        assert_eq!(probe.map()?, BTreeSet::from([root]));
        assert!(probe.rescan.pending_rollback.is_none());
        Ok(())
    }

    #[test]
    fn failed_removal_then_same_cgroup_returns_restores_all_maps() -> TestResult {
        let tree = FakeTree::new()?;
        tree.mkcg("/harw.slice/job")?;
        let mut probe = Probe::attach(&tree, MAX_SCOPE_CGROUP_IDS)?;
        let before = probe.map()?;
        let source = host_path(tree.root(), "/harw.slice/job");
        let away = host_path(tree.root(), "/away");
        std::fs::rename(&source, &away).map_err(ctx("move cgroup out of scope"))?;
        probe.loader.fail_next_scope_updates(1);
        assert!(probe.rescan().health.apply_failed);
        assert!(probe.map().is_err());
        std::fs::rename(&away, &source).map_err(ctx("return same cgroup"))?;
        let recovered = probe.rescan();
        assert!(recovered.delta.is_empty(), "tracker already knows this ID");
        assert!(!recovered.health.is_degraded());
        assert_eq!(probe.map()?, before);
        Ok(())
    }

    #[test]
    fn incomplete_walk_never_retires() -> TestResult {
        let tree = FakeTree::new()?;
        tree.mkcg("/harw.slice/harw-job-1")?;
        let mut probe = Probe::attach(&tree, MAX_SCOPE_CGROUP_IDS)?;
        let before = probe.map()?;
        probe.rescan.limits = WalkLimits {
            max_entries: 1,
            ..WalkLimits::default()
        };
        tree.rmcg("/harw.slice/harw-job-1")?;
        tree.mkcg("/harw.slice/a")?;
        let round = probe.rescan();
        assert!(round.health.incomplete);
        assert!(round.delta.remove.is_empty());
        assert_eq!(probe.map()?, before);
        Ok(())
    }

    /// Eine beim Start nicht auflösbare Wurzel bleibt in jeder Runde als
    /// nicht verfügbar gezählt; der bereits beim Start gemeldete Zustand wird
    /// weder erneut gemeldet noch fälschlich als erholt geloggt.
    #[test]
    fn root_missing_at_start_keeps_every_round_degraded_and_is_not_reported_again() -> TestResult {
        let tree = FakeTree::new()?;
        let Probe {
            loader,
            handles,
            rescan,
            start,
        } = Probe::attach(&tree, MAX_SCOPE_CGROUP_IDS)?;
        let startup = ScopeHealth {
            roots_unavailable: 1,
            ..ScopeHealth::default()
        };
        let mut probe = Probe {
            loader,
            handles,
            rescan: rescan.with_startup(1, startup),
            start,
        };
        let job = tree.mkcg("/harw.slice/harw-job-1")?;
        let first = probe.rescan();
        assert_eq!(first.health, startup);
        assert!(first.health.is_degraded());
        assert!(!first.newly_degraded, "already reported at start");
        assert_eq!(first.delta.insert, vec![job], "resolved roots keep working");
        let second = probe.rescan();
        assert_eq!(second.health.roots_unavailable, 1);
        assert!(!second.newly_degraded);
        Ok(())
    }

    #[test]
    fn rescan_waits_for_the_interval() -> TestResult {
        let tree = FakeTree::new()?;
        let mut probe = Probe::attach(&tree, MAX_SCOPE_CGROUP_IDS)?;
        let handles: Vec<&BpfHandle> = probe.handles.iter().collect();
        let early = probe.start + Duration::from_millis(500);
        assert!(
            probe
                .rescan
                .rescan_if_due(&probe.loader, &handles, early)
                .is_none()
        );
        let due = probe.start + SCOPE_RESCAN_INTERVAL;
        assert!(
            probe
                .rescan
                .rescan_if_due(&probe.loader, &handles, due)
                .is_some()
        );
        assert!(
            probe
                .rescan
                .rescan_if_due(&probe.loader, &handles, due)
                .is_none()
        );
        assert_eq!(probe.rescan.stats.rounds, 1);
        Ok(())
    }
}
