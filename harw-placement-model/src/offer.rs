//! Runtime and node offers.

use serde::{Deserialize, Serialize};

/// Milliseconds since the Unix epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UnixMillis(pub u64);

/// How well one sandbox dimension is enforced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Enforcement {
    /// Fully enforced as requested.
    Enforced,
    /// Enforced, but weaker than requested.
    Partial,
    /// Supported, but not applied.
    NotEnforced,
    /// Cannot be enforced at all.
    Unsupported,
}

impl Enforcement {
    /// Whether the dimension is fully enforced.
    #[must_use]
    pub const fn is_enforced(self) -> bool {
        matches!(self, Self::Enforced)
    }
}

/// The five sandbox dimensions, as observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DimensionStates {
    /// Filesystem restriction.
    pub filesystem: Enforcement,
    /// Network restriction.
    pub network: Enforcement,
    /// `no_new_privs` or equivalent.
    pub no_new_privs: Enforcement,
    /// Capability dropping.
    pub capabilities: Enforcement,
    /// Resource limits.
    pub resource_limits: Enforcement,
}

impl DimensionStates {
    /// Every dimension in `state`.
    #[must_use]
    pub const fn uniform(state: Enforcement) -> Self {
        Self {
            filesystem: state,
            network: state,
            no_new_privs: state,
            capabilities: state,
            resource_limits: state,
        }
    }

    /// Whether every dimension is fully enforced.
    #[must_use]
    pub const fn all_enforced(&self) -> bool {
        self.filesystem.is_enforced()
            && self.network.is_enforced()
            && self.no_new_privs.is_enforced()
            && self.capabilities.is_enforced()
            && self.resource_limits.is_enforced()
    }
}

/// Which execution backend an offer is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeBackend {
    /// Landlock trampoline.
    Landlock,
    /// Bubblewrap.
    Bwrap,
    /// macOS seatbelt.
    Darwin,
    /// Rootless Podman.
    OciPodmanRootless,
    /// Rootful Podman (root-equivalent for the runner).
    OciPodmanRootful,
    /// Docker.
    OciDocker,
    /// A Kubernetes pod.
    K8sPod,
}

impl RuntimeBackend {
    /// Stable name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Landlock => "landlock",
            Self::Bwrap => "bwrap",
            Self::Darwin => "darwin",
            Self::OciPodmanRootless => "oci-podman-rootless",
            Self::OciPodmanRootful => "oci-podman-rootful",
            Self::OciDocker => "oci-docker",
            Self::K8sPod => "k8s-pod",
        }
    }

    /// Whether the backend gives the runner root-equivalent power (a rootful
    /// engine socket, or Docker whose daemon is root by default).
    #[must_use]
    pub const fn is_rootful(self) -> bool {
        matches!(self, Self::OciPodmanRootful | Self::OciDocker)
    }
}

/// What one backend delivers, as last observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeOffer {
    /// The backend.
    pub backend: RuntimeBackend,
    /// Per-dimension states from a real read-back.
    pub dimensions: DimensionStates,
    /// When the read-back happened.
    pub observed: UnixMillis,
    /// How long the observation stays valid.
    pub ttl_ms: u64,
}

impl RuntimeOffer {
    /// Whether the observation is still valid at `now`. A clock that went
    /// backwards (observation in the future) is not fresh.
    #[must_use]
    pub const fn is_fresh(&self, now: UnixMillis) -> bool {
        match now.0.checked_sub(self.observed.0) {
            Some(age) => age <= self.ttl_ms,
            None => false,
        }
    }

    /// Eligible at `now`: fresh, and not rootful unless allowed.
    #[must_use]
    pub const fn is_eligible(&self, now: UnixMillis, allow_rootful: bool) -> bool {
        self.is_fresh(now) && (allow_rootful || !self.backend.is_rootful())
    }
}

/// Liveness of a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    /// Takes new work.
    Active,
    /// Finishes running work only.
    Draining,
    /// Unreachable or unknown.
    Down,
}

/// What a node offers right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeOffer {
    /// Node name (opaque here).
    pub node: String,
    /// Liveness.
    pub state: NodeState,
    /// When the node was last heard from.
    pub last_seen: UnixMillis,
    /// Runtime offers of the node.
    pub runtimes: Vec<RuntimeOffer>,
}

impl NodeOffer {
    /// The runtimes that may take work at `now`. Empty unless the node is
    /// `Active` and was seen within `max_silence_ms`.
    #[must_use]
    pub fn eligible_runtimes(
        &self,
        now: UnixMillis,
        max_silence_ms: u64,
        allow_rootful: bool,
    ) -> Vec<&RuntimeOffer> {
        let seen_ok = now
            .0
            .checked_sub(self.last_seen.0)
            .is_some_and(|silence| silence <= max_silence_ms);
        if self.state != NodeState::Active || !seen_ok {
            return Vec::new();
        }
        self.runtimes
            .iter()
            .filter(|r| r.is_eligible(now, allow_rootful))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(backend: RuntimeBackend, observed: u64) -> RuntimeOffer {
        RuntimeOffer {
            backend,
            dimensions: DimensionStates::uniform(Enforcement::Enforced),
            observed: UnixMillis(observed),
            ttl_ms: 1_000,
        }
    }

    #[test]
    fn freshness_is_bounded_and_a_backwards_clock_is_not_fresh() {
        let o = offer(RuntimeBackend::Bwrap, 10_000);
        assert!(o.is_fresh(UnixMillis(10_000)));
        assert!(o.is_fresh(UnixMillis(11_000)));
        assert!(!o.is_fresh(UnixMillis(11_001)), "stale");
        assert!(!o.is_fresh(UnixMillis(9_999)), "observation in the future");
    }

    #[test]
    fn rootful_backends_need_an_explicit_opt_in() {
        let now = UnixMillis(10_500);
        for backend in [RuntimeBackend::OciPodmanRootful, RuntimeBackend::OciDocker] {
            let o = offer(backend, 10_000);
            assert!(!o.is_eligible(now, false), "{backend:?}");
            assert!(o.is_eligible(now, true), "{backend:?}");
        }
        assert!(offer(RuntimeBackend::OciPodmanRootless, 10_000).is_eligible(now, false));
        assert!(offer(RuntimeBackend::K8sPod, 10_000).is_eligible(now, false));
    }

    #[test]
    fn only_active_recently_seen_nodes_offer_anything() {
        let mut node = NodeOffer {
            node: "n1".into(),
            state: NodeState::Active,
            last_seen: UnixMillis(10_000),
            runtimes: vec![
                offer(RuntimeBackend::Bwrap, 10_000),
                offer(RuntimeBackend::OciDocker, 10_000),
                offer(RuntimeBackend::K8sPod, 1),
            ],
        };
        let now = UnixMillis(10_400);
        // docker is rootful, the pod offer is stale.
        assert_eq!(node.eligible_runtimes(now, 5_000, false).len(), 1);
        assert_eq!(node.eligible_runtimes(now, 5_000, true).len(), 2);
        assert!(
            node.eligible_runtimes(UnixMillis(20_000), 5_000, true)
                .is_empty(),
            "silent node"
        );
        node.state = NodeState::Draining;
        assert!(node.eligible_runtimes(now, 5_000, true).is_empty());
        node.state = NodeState::Down;
        assert!(node.eligible_runtimes(now, 5_000, true).is_empty());
    }

    #[test]
    fn all_enforced_needs_every_dimension_and_serde_round_trips() -> Result<(), serde_json::Error> {
        let mut d = DimensionStates::uniform(Enforcement::Enforced);
        assert!(d.all_enforced());
        d.network = Enforcement::Partial;
        assert!(!d.all_enforced());
        let o = offer(RuntimeBackend::OciPodmanRootless, 5);
        let json = serde_json::to_string(&o)?;
        assert!(json.contains("\"oci-podman-rootless\""), "{json}");
        let back: RuntimeOffer = serde_json::from_str(&json)?;
        assert_eq!(back, o);
        assert!(serde_json::from_str::<RuntimeOffer>(&json.replace('}', ",\"x\":1}")).is_err());
        Ok(())
    }
}
