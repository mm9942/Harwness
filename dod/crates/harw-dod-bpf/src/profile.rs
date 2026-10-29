//! Host-resolved scope passed to the loader before any hook is attached.
//! Configuration owns path parsing; this module owns stable kernel identity
//! checks and the component-aware ancestry rule used by fixtures and BPF map
//! population.
//!
//! # Dynamic descendant scope (PL-97 E3b)
//! For `include_descendants = true` the attach-time expansion is no longer
//! final: [`DescendantScope`] keeps the explicit roots apart from the
//! descendants found by a walk ([`crate::cgroup_walk`]) and turns every new
//! [`ScopeObservation`] into an incremental [`ScopeDelta`] for the
//! `SCOPE_CGROUP_IDS` map of each loaded object
//! ([`crate::loader::BpfScopeWriter`]). The planner is pure: it performs no
//! I/O and never touches the ids of the explicit roots. The map capacity is
//! fixed; cgroups that do not fit are not inserted and are counted as
//! overflow instead of being dropped silently.

use std::collections::{BTreeMap, BTreeSet};

/// Must remain identical to `SCOPE_CGROUP_IDS.max_entries` in the C objects.
/// The profile resolver rejects a broader expansion before the loader can
/// create a partially populated map.
pub const MAX_SCOPE_CGROUP_IDS: usize = 16_384;

/// A cgroup identity resolved from cgroup v2 immediately before attach.
/// `id` is the kernel cgroup id used in the BPF map; `path` is retained only
/// to detect a removal/recreation before the map is populated.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ResolvedCgroup {
    pub path: String,
    pub id: u64,
}

/// The scope BPF programs must enforce before reserving ring-buffer space.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BpfScope {
    /// Explicit host observation, selected by config rather than inferred from
    /// an empty cgroup list.
    Host,
    /// One or more stable cgroup ids.  `groups` are the explicit profile
    /// roots.  For `include_descendants`, every descendant that exists at
    /// attach time is expanded into `descendants` (see
    /// [`DescendantScope::bpf_scope`]); BPF can then make an exact ID
    /// lookup without using unsafe path prefixes or guessing ancestry in
    /// kernel context.  Descendants created after attach are admitted by a
    /// bounded userspace rescan that applies [`ScopeDelta`]s to the loaded
    /// maps; until the next rescan (default interval: 2 s in
    /// `harw-probe-bpf`) a brand-new cgroup is not yet observed.
    ///
    /// The scope also fixes what that rescan may change after attach
    /// ([`crate::loader::BpfScopeWriter`]): without `include_descendants`
    /// the map is final and every delta is refused; with it, descendants
    /// may be inserted and retired, but an id from `groups` is never
    /// removed.
    Cgroups {
        groups: Vec<ResolvedCgroup>,
        include_descendants: bool,
        /// Descendants of `groups` that exist at attach time.  Must be empty
        /// without `include_descendants`.
        descendants: Vec<ResolvedCgroup>,
    },
}

impl BpfScope {
    pub fn validate(&self) -> Result<(), crate::BpfError> {
        match self {
            Self::Host => Ok(()),
            Self::Cgroups {
                groups,
                include_descendants,
                descendants,
            } => {
                let total = groups.len().saturating_add(descendants.len());
                let ids: BTreeSet<_> = groups
                    .iter()
                    .chain(descendants)
                    .map(|group| group.id)
                    .collect();
                if groups.is_empty()
                    || (!include_descendants && !descendants.is_empty())
                    || total > MAX_SCOPE_CGROUP_IDS
                    || ids.len() != total
                    || groups
                        .iter()
                        .chain(descendants)
                        .any(|group| group.id == 0 || !valid_cgroup_path(&group.path))
                {
                    Err(crate::BpfError::InvalidProgramContract)
                } else {
                    Ok(())
                }
            }
        }
    }

    #[must_use]
    pub fn contains_path(&self, actual: &str) -> bool {
        match self {
            Self::Host => true,
            Self::Cgroups {
                groups,
                include_descendants,
                ..
            } => groups.iter().any(|group| {
                actual == group.path
                    || (*include_descendants && is_descendant_path(actual, &group.path))
            }),
        }
    }

    /// Exact kernel-ID predicate used to populate and reason about the BPF
    /// map.  A re-created path necessarily has a different resolved ID and
    /// is therefore not admitted by an old map entry.
    #[must_use]
    pub fn allows_cgroup_id(&self, cgroup_id: u64) -> bool {
        match self {
            Self::Host => true,
            Self::Cgroups {
                groups,
                descendants,
                ..
            } => groups
                .iter()
                .chain(descendants)
                .any(|group| group.id == cgroup_id),
        }
    }

    /// Every id the `SCOPE_CGROUP_IDS` map holds at attach time: the host
    /// marker `0` for [`Self::Host`], otherwise the roots followed by the
    /// attach-time descendants.
    #[must_use]
    pub(crate) fn map_ids(&self) -> Vec<u64> {
        match self {
            Self::Host => vec![0],
            Self::Cgroups {
                groups,
                descendants,
                ..
            } => groups
                .iter()
                .chain(descendants)
                .map(|group| group.id)
                .collect(),
        }
    }
}

/// What [`crate::loader::BpfScopeWriter`] may change in the scope map of an
/// object after attach, fixed from the object's [`BpfScope`] when it is
/// loaded. Shared by the real loader and the fixture so both refuse the
/// same deltas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScopeWritePolicy {
    /// [`BpfScope::Host`] or cgroups without descendants: the map is final
    /// at attach, every delta is refused (a fixed scope is never widened or
    /// narrowed at runtime).
    Fixed,
    /// `include_descendants = true`: descendants may be inserted and
    /// retired; the explicit roots in `pinned` are never removed.
    Descendants { pinned: BTreeSet<u64> },
}

impl ScopeWritePolicy {
    /// The policy for an object loaded with `scope`.
    pub(crate) fn for_scope(scope: &BpfScope) -> Self {
        match scope {
            BpfScope::Cgroups {
                groups,
                include_descendants: true,
                ..
            } => Self::Descendants {
                pinned: groups.iter().map(|group| group.id).collect(),
            },
            BpfScope::Host | BpfScope::Cgroups { .. } => Self::Fixed,
        }
    }

    /// Checks an already validated `delta` against this policy.
    ///
    /// # Errors
    /// [`crate::BpfError::InvalidProgramContract`] for a fixed scope (also
    /// for an empty delta) and for a delta that removes a pinned root.
    pub(crate) fn admit(&self, delta: &ScopeDelta) -> Result<(), crate::BpfError> {
        match self {
            Self::Descendants { pinned } if !delta.remove.iter().any(|id| pinned.contains(id)) => {
                Ok(())
            }
            Self::Fixed | Self::Descendants { .. } => Err(crate::BpfError::InvalidProgramContract),
        }
    }
}

/// One incremental change to the `SCOPE_CGROUP_IDS` map of a loaded object.
///
/// Removals are applied before insertions so that a retirement frees map
/// capacity for a new descendant within the same round. Both operations are
/// idempotent (removing an absent id and re-inserting a present id are
/// no-ops), so a delta that failed half-way can be applied again as a whole.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeDelta {
    /// Kernel cgroup ids to add to the map.
    pub insert: Vec<u64>,
    /// Kernel cgroup ids to remove from the map.
    pub remove: Vec<u64>,
}

impl ScopeDelta {
    /// `true` when the delta changes nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.insert.is_empty() && self.remove.is_empty()
    }

    /// Rejects a delta that could widen or corrupt a cgroup scope.
    ///
    /// # Errors
    /// [`crate::BpfError::InvalidProgramContract`] when either list contains
    /// the host marker `0` (inserting it would turn a cgroup scope into host
    /// observation), an id appears twice, or an id is both inserted and
    /// removed.
    pub fn validate(&self) -> Result<(), crate::BpfError> {
        let mut seen = BTreeSet::new();
        let all_unique_and_nonzero = self
            .insert
            .iter()
            .chain(&self.remove)
            .all(|id| *id != 0 && seen.insert(*id));
        if all_unique_and_nonzero {
            Ok(())
        } else {
            Err(crate::BpfError::InvalidProgramContract)
        }
    }
}

/// The descendants found beneath the explicit roots in one walk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeObservation {
    /// Descendant cgroups in walk order (roots themselves are not listed;
    /// if they are, the planner ignores them).
    pub descendants: Vec<ResolvedCgroup>,
    /// `true` only when the walk saw every descendant that the walk limits
    /// allow. An incomplete walk (I/O error other than a vanished directory,
    /// entry budget exhausted) never retires an id: absence is then no
    /// evidence of removal.
    pub complete: bool,
}

/// The result of [`DescendantScope::plan`] for one rescan round.
///
/// The plan is applied to every loaded object first and committed with
/// [`DescendantScope::commit`] only afterwards, so a failed map update leaves
/// the tracked state unchanged and the next round plans it again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopePlan {
    /// The map change for every object loaded with this scope.
    pub delta: ScopeDelta,
    /// Observed descendants that were not inserted because the map is full.
    pub overflowed: usize,
    generation: u64,
    next: BTreeMap<u64, String>,
}

/// Cumulative counters of a [`DescendantScope`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DescendantScopeStats {
    /// Committed rescan rounds.
    pub rounds: u64,
    /// Descendant ids inserted over all committed rounds.
    pub inserted_total: u64,
    /// Descendant ids retired over all committed rounds.
    pub retired_total: u64,
    /// Sum of [`ScopePlan::overflowed`] over all committed rounds: every
    /// round in which a cgroup did not fit increments this counter again.
    pub overflow_total: u64,
    /// [`ScopePlan::overflowed`] of the last committed round.
    pub overflowed_last_round: usize,
}

/// The incrementally maintained scope of a profile with
/// `include_descendants = true`.
///
/// Holds the explicit roots (never removed, never re-resolved) separately
/// from the descendants currently installed in the BPF maps. The installed
/// set (roots plus descendants) never exceeds [`Self::capacity`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DescendantScope {
    roots: Vec<ResolvedCgroup>,
    root_ids: BTreeSet<u64>,
    descendants: BTreeMap<u64, String>,
    capacity: usize,
    generation: u64,
    stats: DescendantScopeStats,
}

impl DescendantScope {
    /// Builds a scope from its explicit roots, without any descendant yet.
    ///
    /// `capacity` is the number of map entries this scope may occupy; it is
    /// clamped to [`MAX_SCOPE_CGROUP_IDS`] (the `max_entries` of the C map).
    ///
    /// # Errors
    /// [`crate::BpfError::InvalidProgramContract`] when `roots` is empty,
    /// contains the host marker `0`, a duplicate id or an invalid path, or
    /// when the roots alone exceed the capacity.
    pub fn new(roots: Vec<ResolvedCgroup>, capacity: usize) -> Result<Self, crate::BpfError> {
        let capacity = capacity.min(MAX_SCOPE_CGROUP_IDS);
        let root_ids: BTreeSet<u64> = roots.iter().map(|root| root.id).collect();
        if roots.is_empty()
            || roots.len() > capacity
            || root_ids.len() != roots.len()
            || roots
                .iter()
                .any(|root| root.id == 0 || !valid_cgroup_path(&root.path))
        {
            return Err(crate::BpfError::InvalidProgramContract);
        }
        Ok(Self {
            roots,
            root_ids,
            descendants: BTreeMap::new(),
            capacity,
            generation: 0,
            stats: DescendantScopeStats::default(),
        })
    }

    /// The explicit roots, in configuration order.
    #[must_use]
    pub fn roots(&self) -> &[ResolvedCgroup] {
        &self.roots
    }

    /// The number of map entries this scope may occupy.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Every id currently installed: the roots plus the tracked descendants.
    #[must_use]
    pub fn installed_ids(&self) -> BTreeSet<u64> {
        self.root_ids
            .iter()
            .chain(self.descendants.keys())
            .copied()
            .collect()
    }

    /// Cumulative counters of all committed rounds.
    #[must_use]
    pub const fn stats(&self) -> DescendantScopeStats {
        self.stats
    }

    /// The attach-time scope for [`crate::BpfObjectContract`]: the roots as
    /// `groups` (pinned for the rest of the object's life) and the tracked
    /// descendants (ordered by id) as `descendants`.
    #[must_use]
    pub fn bpf_scope(&self) -> BpfScope {
        BpfScope::Cgroups {
            groups: self.roots.clone(),
            include_descendants: true,
            descendants: self
                .descendants
                .iter()
                .map(|(id, path)| ResolvedCgroup {
                    path: path.clone(),
                    id: *id,
                })
                .collect(),
        }
    }

    /// Plans the map change that brings the installed set in line with
    /// `observation`.
    ///
    /// - Explicit roots and the host marker `0` are ignored if observed and
    ///   are never part of [`ScopeDelta::remove`].
    /// - A complete observation retires every tracked descendant it no
    ///   longer contains; an incomplete one retires nothing.
    /// - New descendants are inserted in observation order while capacity
    ///   remains; the rest is reported as [`ScopePlan::overflowed`].
    #[must_use]
    pub fn plan(&self, observation: &ScopeObservation) -> ScopePlan {
        let mut observed: BTreeMap<u64, &str> = BTreeMap::new();
        let mut order: Vec<u64> = Vec::new();
        for group in &observation.descendants {
            if group.id == 0 || self.root_ids.contains(&group.id) || !valid_cgroup_path(&group.path)
            {
                continue;
            }
            if observed.insert(group.id, group.path.as_str()).is_none() {
                order.push(group.id);
            }
        }

        let remove: Vec<u64> = if observation.complete {
            self.descendants
                .keys()
                .filter(|id| !observed.contains_key(id))
                .copied()
                .collect()
        } else {
            Vec::new()
        };
        let retired: BTreeSet<u64> = remove.iter().copied().collect();

        // Kept descendants take the observed path, so a rename is tracked
        // without a map change (the kernel id is unchanged).
        let mut next: BTreeMap<u64, String> = self
            .descendants
            .iter()
            .filter(|(id, _)| !retired.contains(id))
            .map(|(id, path)| {
                let path = observed
                    .get(id)
                    .map_or_else(|| path.clone(), |p| (*p).to_owned());
                (*id, path)
            })
            .collect();

        let mut free = self
            .capacity
            .saturating_sub(self.roots.len().saturating_add(next.len()));
        let mut insert = Vec::new();
        let mut overflowed = 0usize;
        for id in order {
            if next.contains_key(&id) {
                continue;
            }
            if free == 0 {
                overflowed = overflowed.saturating_add(1);
                continue;
            }
            if let Some(path) = observed.get(&id) {
                next.insert(id, (*path).to_owned());
                insert.push(id);
                free -= 1;
            }
        }

        ScopePlan {
            delta: ScopeDelta { insert, remove },
            overflowed,
            generation: self.generation,
            next,
        }
    }

    /// Adopts a plan after its delta was applied to every loaded object.
    ///
    /// # Errors
    /// [`crate::BpfError::InvalidProgramContract`] when `plan` was not
    /// produced by the immediately preceding [`Self::plan`] call on this
    /// scope (another plan was committed in between); the state is then
    /// left unchanged.
    pub fn commit(&mut self, plan: ScopePlan) -> Result<(), crate::BpfError> {
        if plan.generation != self.generation {
            return Err(crate::BpfError::InvalidProgramContract);
        }
        let to_u64 = |count: usize| u64::try_from(count).unwrap_or(u64::MAX);
        self.stats.rounds = self.stats.rounds.saturating_add(1);
        self.stats.inserted_total = self
            .stats
            .inserted_total
            .saturating_add(to_u64(plan.delta.insert.len()));
        self.stats.retired_total = self
            .stats
            .retired_total
            .saturating_add(to_u64(plan.delta.remove.len()));
        self.stats.overflow_total = self
            .stats
            .overflow_total
            .saturating_add(to_u64(plan.overflowed));
        self.stats.overflowed_last_round = plan.overflowed;
        self.descendants = plan.next;
        self.generation = self.generation.wrapping_add(1);
        Ok(())
    }
}

fn valid_cgroup_path(path: &str) -> bool {
    path.starts_with('/') && !path.split('/').any(|part| part == "." || part == "..")
}

/// `/a` is an ancestor of `/a/b`, never of `/ab`.
#[must_use]
pub fn is_descendant_path(candidate: &str, parent: &str) -> bool {
    candidate
        .strip_prefix(parent)
        .is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::{
        BpfScope, DescendantScope, MAX_SCOPE_CGROUP_IDS, ResolvedCgroup, ScopeDelta,
        ScopeObservation, ScopeWritePolicy, is_descendant_path,
    };
    use crate::test_support::{TestResult, ctx};
    use std::collections::BTreeSet;

    fn cg(path: &str, id: u64) -> ResolvedCgroup {
        ResolvedCgroup {
            path: path.into(),
            id,
        }
    }

    fn complete(descendants: Vec<ResolvedCgroup>) -> ScopeObservation {
        ScopeObservation {
            descendants,
            complete: true,
        }
    }

    fn ids(values: &[u64]) -> BTreeSet<u64> {
        values.iter().copied().collect()
    }

    fn scope_with(descendants: Vec<ResolvedCgroup>) -> TestResult<DescendantScope> {
        let mut scope = DescendantScope::new(vec![cg("/harw.slice", 10)], MAX_SCOPE_CGROUP_IDS)
            .map_err(ctx("valid root"))?;
        let plan = scope.plan(&complete(descendants));
        scope.commit(plan).map_err(ctx("fresh plan commits"))?;
        Ok(scope)
    }

    #[test]
    fn plan_inserts_new_descendants_and_retires_vanished_ones() -> TestResult {
        let scope = scope_with(vec![cg("/harw.slice/harw-job-1", 11)])?;
        let plan = scope.plan(&complete(vec![cg("/harw.slice/harw-job-2", 12)]));
        assert_eq!(
            plan.delta,
            ScopeDelta {
                insert: vec![12],
                remove: vec![11],
            }
        );
        assert_eq!(plan.overflowed, 0);
        Ok(())
    }

    #[test]
    fn explicit_roots_are_never_removed_even_when_absent_or_observed() -> TestResult {
        let mut scope = scope_with(vec![cg("/harw.slice/harw-job-1", 11)])?;
        // Leere, vollstaendige Beobachtung: nur der Nachfahre wird entfernt.
        let plan = scope.plan(&complete(Vec::new()));
        assert_eq!(plan.delta.remove, vec![11]);
        assert!(!plan.delta.remove.contains(&10));
        scope.commit(plan).map_err(ctx("commit retirement"))?;
        assert_eq!(scope.installed_ids(), ids(&[10]));

        // Eine als Nachfahre beobachtete Wurzel (ueberlappende Wurzeln) und die
        // Hostmarke werden ignoriert.
        let plan = scope.plan(&complete(vec![cg("/harw.slice", 10), cg("/x", 0)]));
        assert!(plan.delta.is_empty());
        Ok(())
    }

    #[test]
    fn incomplete_observation_never_retires() -> TestResult {
        let scope = scope_with(vec![cg("/harw.slice/harw-job-1", 11)])?;
        let plan = scope.plan(&ScopeObservation {
            descendants: vec![cg("/harw.slice/harw-job-2", 12)],
            complete: false,
        });
        assert_eq!(plan.delta.insert, vec![12]);
        assert!(plan.delta.remove.is_empty());
        Ok(())
    }

    #[test]
    fn overflow_is_counted_every_round_and_installed_set_stays_at_capacity() -> TestResult {
        let mut scope =
            DescendantScope::new(vec![cg("/r", 1)], 3).map_err(ctx("valid capacity"))?;
        let observation = complete(vec![
            cg("/r/a", 2),
            cg("/r/b", 3),
            cg("/r/c", 4),
            cg("/r/d", 5),
        ]);
        let plan = scope.plan(&observation);
        assert_eq!(plan.delta.insert, vec![2, 3]);
        assert_eq!(plan.overflowed, 2);
        scope.commit(plan).map_err(ctx("commit first round"))?;
        assert_eq!(scope.installed_ids().len(), scope.capacity());
        assert_eq!(scope.stats().overflow_total, 2);
        assert_eq!(scope.stats().overflowed_last_round, 2);

        let plan = scope.plan(&observation);
        assert!(plan.delta.is_empty());
        assert_eq!(plan.overflowed, 2);
        scope.commit(plan).map_err(ctx("commit second round"))?;
        assert_eq!(scope.stats().overflow_total, 4);
        assert_eq!(scope.installed_ids(), ids(&[1, 2, 3]));
        Ok(())
    }

    #[test]
    fn retirement_frees_capacity_for_a_waiting_descendant_in_the_same_round() -> TestResult {
        let mut scope =
            DescendantScope::new(vec![cg("/r", 1)], 2).map_err(ctx("valid capacity"))?;
        let plan = scope.plan(&complete(vec![cg("/r/a", 2), cg("/r/b", 3)]));
        assert_eq!(plan.overflowed, 1);
        scope.commit(plan).map_err(ctx("commit"))?;

        let plan = scope.plan(&complete(vec![cg("/r/b", 3)]));
        assert_eq!(
            plan.delta,
            ScopeDelta {
                insert: vec![3],
                remove: vec![2],
            }
        );
        assert_eq!(plan.overflowed, 0);
        scope.commit(plan).map_err(ctx("commit"))?;
        assert_eq!(scope.stats().overflowed_last_round, 0);
        assert_eq!(scope.stats().inserted_total, 2);
        assert_eq!(scope.stats().retired_total, 1);
        Ok(())
    }

    #[test]
    fn stale_plan_is_not_committed() -> TestResult {
        let mut scope = scope_with(Vec::new())?;
        let first = scope.plan(&complete(vec![cg("/harw.slice/a", 11)]));
        let second = scope.plan(&complete(vec![cg("/harw.slice/b", 12)]));
        scope.commit(second).map_err(ctx("current plan commits"))?;
        assert!(scope.commit(first).is_err());
        assert_eq!(scope.installed_ids(), ids(&[10, 12]));
        Ok(())
    }

    #[test]
    fn bpf_scope_lists_roots_and_descendants_and_validates() -> TestResult {
        let scope = scope_with(vec![cg("/harw.slice/harw-job-1", 11)])?;
        let bpf_scope = scope.bpf_scope();
        bpf_scope
            .validate()
            .map_err(ctx("expanded scope validates"))?;
        assert_eq!(
            bpf_scope,
            BpfScope::Cgroups {
                groups: vec![cg("/harw.slice", 10)],
                include_descendants: true,
                descendants: vec![cg("/harw.slice/harw-job-1", 11)],
            }
        );
        assert_eq!(bpf_scope.map_ids(), vec![10, 11]);
        assert!(bpf_scope.allows_cgroup_id(10));
        assert!(bpf_scope.allows_cgroup_id(11));
        assert!(!bpf_scope.allows_cgroup_id(12));
        Ok(())
    }

    #[test]
    fn scope_rejects_descendants_without_include_descendants_and_shared_ids() {
        let fixed_with_descendants = BpfScope::Cgroups {
            groups: vec![cg("/r", 1)],
            include_descendants: false,
            descendants: vec![cg("/r/a", 2)],
        };
        assert!(fixed_with_descendants.validate().is_err());
        let root_listed_again = BpfScope::Cgroups {
            groups: vec![cg("/r", 1)],
            include_descendants: true,
            descendants: vec![cg("/r/a", 1)],
        };
        assert!(root_listed_again.validate().is_err());
        let host_marker_descendant = BpfScope::Cgroups {
            groups: vec![cg("/r", 1)],
            include_descendants: true,
            descendants: vec![cg("/r/a", 0)],
        };
        assert!(host_marker_descendant.validate().is_err());
        assert_eq!(BpfScope::Host.map_ids(), vec![0]);
    }

    #[test]
    fn write_policy_refuses_fixed_scopes_and_pinned_root_removal() -> TestResult {
        let delta = ScopeDelta {
            insert: vec![3],
            remove: vec![2],
        };
        for fixed in [
            BpfScope::Host,
            BpfScope::Cgroups {
                groups: vec![cg("/r", 1), cg("/r/a", 2)],
                include_descendants: false,
                descendants: Vec::new(),
            },
        ] {
            let policy = ScopeWritePolicy::for_scope(&fixed);
            assert_eq!(policy, ScopeWritePolicy::Fixed);
            assert!(policy.admit(&delta).is_err());
            assert!(policy.admit(&ScopeDelta::default()).is_err());
        }

        let dynamic = BpfScope::Cgroups {
            groups: vec![cg("/r", 1)],
            include_descendants: true,
            descendants: vec![cg("/r/a", 2)],
        };
        let policy = ScopeWritePolicy::for_scope(&dynamic);
        policy
            .admit(&delta)
            .map_err(ctx("descendant retirement is admitted"))?;
        let remove_root = ScopeDelta {
            insert: Vec::new(),
            remove: vec![1],
        };
        assert!(policy.admit(&remove_root).is_err());
        Ok(())
    }

    #[test]
    fn descendant_scope_rejects_invalid_roots() {
        assert!(DescendantScope::new(Vec::new(), 8).is_err());
        assert!(DescendantScope::new(vec![cg("/a", 0)], 8).is_err());
        assert!(DescendantScope::new(vec![cg("/a", 1), cg("/b", 1)], 8).is_err());
        assert!(DescendantScope::new(vec![cg("relative", 1)], 8).is_err());
        assert!(DescendantScope::new(vec![cg("/a", 1), cg("/b", 2)], 1).is_err());
    }

    #[test]
    fn scope_delta_rejects_host_marker_duplicates_and_overlap() {
        let valid = ScopeDelta {
            insert: vec![1],
            remove: vec![2],
        };
        assert!(valid.validate().is_ok());
        for invalid in [
            ScopeDelta {
                insert: vec![0],
                remove: vec![],
            },
            ScopeDelta {
                insert: vec![],
                remove: vec![0],
            },
            ScopeDelta {
                insert: vec![3, 3],
                remove: vec![],
            },
            ScopeDelta {
                insert: vec![4],
                remove: vec![4],
            },
        ] {
            assert!(invalid.validate().is_err());
        }
    }

    #[test]
    fn component_aware_scope_excludes_siblings_and_prefix_collisions() {
        let scope = BpfScope::Cgroups {
            groups: vec![ResolvedCgroup {
                path: "/a".into(),
                id: 12,
            }],
            include_descendants: true,
            descendants: Vec::new(),
        };
        assert!(scope.contains_path("/a/child"));
        assert!(!scope.contains_path("/ab"));
        assert!(!scope.contains_path("/other"));
        assert!(is_descendant_path("/a/child", "/a"));
    }

    #[test]
    fn rejects_empty_and_duplicate_kernel_id_scopes() {
        assert!(
            BpfScope::Cgroups {
                groups: vec![],
                include_descendants: false,
                descendants: Vec::new(),
            }
            .validate()
            .is_err()
        );
        let groups = vec![
            ResolvedCgroup {
                path: "/a".into(),
                id: 1,
            },
            ResolvedCgroup {
                path: "/b".into(),
                id: 1,
            },
        ];
        assert!(
            BpfScope::Cgroups {
                groups,
                include_descendants: true,
                descendants: Vec::new(),
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn recreated_path_never_inherits_the_previous_cgroup_identity() {
        let scope = BpfScope::Cgroups {
            groups: vec![ResolvedCgroup {
                path: "/selected".into(),
                id: 41,
            }],
            include_descendants: false,
            descendants: Vec::new(),
        };
        assert!(scope.allows_cgroup_id(41));
        assert!(!scope.allows_cgroup_id(42));
    }
}
