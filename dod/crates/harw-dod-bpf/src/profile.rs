//! Host-resolved scope passed to the loader before any hook is attached.
//! Configuration owns path parsing; this module owns stable kernel identity
//! checks and the component-aware ancestry rule used by fixtures and BPF map
//! population.

use std::collections::BTreeSet;

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
    /// One or more stable cgroup ids.  For `include_descendants`, the config
    /// resolver expands every descendant that exists at attach time into
    /// `groups`; BPF can then make an exact ID lookup without using unsafe
    /// path prefixes or guessing ancestry in kernel context.  Newly created
    /// descendants require the documented ordered profile restart.
    Cgroups { groups: Vec<ResolvedCgroup>, include_descendants: bool },
}

impl BpfScope {
    pub fn validate(&self) -> Result<(), crate::BpfError> {
        match self {
            Self::Host => Ok(()),
            Self::Cgroups { groups, .. } if groups.is_empty() => Err(crate::BpfError::InvalidProgramContract),
            Self::Cgroups { groups, .. } => {
                let ids: BTreeSet<_> = groups.iter().map(|group| group.id).collect();
                if groups.len() > MAX_SCOPE_CGROUP_IDS
                    || ids.len() != groups.len()
                    || groups.iter().any(|group| group.id == 0 || !valid_cgroup_path(&group.path))
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
            Self::Cgroups { groups, include_descendants } => groups.iter().any(|group| {
                actual == group.path || (*include_descendants && is_descendant_path(actual, &group.path))
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
            Self::Cgroups { groups, .. } => groups.iter().any(|group| group.id == cgroup_id),
        }
    }
}

fn valid_cgroup_path(path: &str) -> bool {
    path.starts_with('/') && !path.split('/').any(|part| part == "." || part == "..")
}

/// `/a` is an ancestor of `/a/b`, never of `/ab`.
#[must_use]
pub fn is_descendant_path(candidate: &str, parent: &str) -> bool {
    candidate.strip_prefix(parent).is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::{is_descendant_path, BpfScope, ResolvedCgroup};

    #[test]
    fn component_aware_scope_excludes_siblings_and_prefix_collisions() {
        let scope = BpfScope::Cgroups { groups: vec![ResolvedCgroup { path: "/a".into(), id: 12 }], include_descendants: true };
        assert!(scope.contains_path("/a/child"));
        assert!(!scope.contains_path("/ab"));
        assert!(!scope.contains_path("/other"));
        assert!(is_descendant_path("/a/child", "/a"));
    }

    #[test]
    fn rejects_empty_and_duplicate_kernel_id_scopes() {
        assert!(BpfScope::Cgroups { groups: vec![], include_descendants: false }.validate().is_err());
        let groups = vec![
            ResolvedCgroup { path: "/a".into(), id: 1 },
            ResolvedCgroup { path: "/b".into(), id: 1 },
        ];
        assert!(BpfScope::Cgroups { groups, include_descendants: true }.validate().is_err());
    }

    #[test]
    fn recreated_path_never_inherits_the_previous_cgroup_identity() {
        let scope = BpfScope::Cgroups {
            groups: vec![ResolvedCgroup { path: "/selected".into(), id: 41 }],
            include_descendants: false,
        };
        assert!(scope.allows_cgroup_id(41));
        assert!(!scope.allows_cgroup_id(42));
    }
}
