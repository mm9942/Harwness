//! Pure hard filtering of existing observed offers; no grants or reservations.

use crate::{
    DimensionStates, Enforcement, NodeOffer, NodeState, RuntimeBackend, RuntimeOffer, UnixMillis,
};

/// Dimensions whose full enforcement is required by trusted admission.
/// A false flag does not grant permission or imply that enforcement exists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RequiredDimensions {
    pub filesystem: bool,
    pub network: bool,
    pub no_new_privs: bool,
    pub capabilities: bool,
    pub resource_limits: bool,
}

/// Requirements for one explicitly selected backend. This API never silently
/// substitutes a different backend, or turns an offer into execution authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeRequirements {
    pub backend: RuntimeBackend,
    pub required: RequiredDimensions,
    pub allow_rootful: bool,
    pub max_silence_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxDimension {
    Filesystem,
    Network,
    NoNewPrivs,
    Capabilities,
    ResourceLimits,
}

/// Deterministic hard-constraint diagnostics in node/runtime/dimension order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeRejection {
    NodeInactive(NodeState),
    NodeStale,
    RuntimeMissing,
    /// Duplicate observations for the selected backend cannot bind one offer.
    RuntimeAmbiguous,
    RuntimeStale,
    RootfulDisallowed,
    RequiredDimension {
        dimension: SandboxDimension,
        observed: Enforcement,
    },
}

/// Evaluate every applicable constraint; unknown or partially enforced required
/// dimensions refuse. A fresh eligible observation is still not a reservation
/// or proof of restrictions applied to a future attempt.
pub fn filter_runtime(
    node: &NodeOffer,
    requirements: &RuntimeRequirements,
    now: UnixMillis,
) -> Result<RuntimeOffer, Vec<RuntimeRejection>> {
    let mut refusals = Vec::new();
    if node.state != NodeState::Active {
        refusals.push(RuntimeRejection::NodeInactive(node.state));
    }
    if !now
        .0
        .checked_sub(node.last_seen.0)
        .is_some_and(|age| age <= requirements.max_silence_ms)
    {
        refusals.push(RuntimeRejection::NodeStale);
    }
    if requirements.backend.is_rootful() && !requirements.allow_rootful {
        refusals.push(RuntimeRejection::RootfulDisallowed);
    }
    let mut matching = node
        .runtimes
        .iter()
        .filter(|offer| offer.backend == requirements.backend);
    let Some(offer) = matching.next() else {
        refusals.push(RuntimeRejection::RuntimeMissing);
        return Err(refusals);
    };
    if matching.next().is_some() {
        refusals.push(RuntimeRejection::RuntimeAmbiguous);
        return Err(refusals);
    }
    if !offer.is_fresh(now) {
        refusals.push(RuntimeRejection::RuntimeStale);
    }
    append_dimensions(&mut refusals, requirements.required, offer.dimensions);
    if refusals.is_empty() {
        Ok(*offer)
    } else {
        Err(refusals)
    }
}

fn append_dimensions(
    refusals: &mut Vec<RuntimeRejection>,
    required: RequiredDimensions,
    observed: DimensionStates,
) {
    for (needed, state, dimension) in [
        (
            required.filesystem,
            observed.filesystem,
            SandboxDimension::Filesystem,
        ),
        (
            required.network,
            observed.network,
            SandboxDimension::Network,
        ),
        (
            required.no_new_privs,
            observed.no_new_privs,
            SandboxDimension::NoNewPrivs,
        ),
        (
            required.capabilities,
            observed.capabilities,
            SandboxDimension::Capabilities,
        ),
        (
            required.resource_limits,
            observed.resource_limits,
            SandboxDimension::ResourceLimits,
        ),
    ] {
        if needed && !state.is_enforced() {
            refusals.push(RuntimeRejection::RequiredDimension {
                dimension,
                observed: state,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (NodeOffer, RuntimeRequirements) {
        (
            NodeOffer {
                node: "node-1".into(),
                state: NodeState::Active,
                last_seen: UnixMillis(100),
                runtimes: vec![RuntimeOffer {
                    backend: RuntimeBackend::Bwrap,
                    dimensions: DimensionStates::uniform(Enforcement::Enforced),
                    observed: UnixMillis(100),
                    ttl_ms: 20,
                }],
            },
            RuntimeRequirements {
                backend: RuntimeBackend::Bwrap,
                required: RequiredDimensions {
                    filesystem: true,
                    network: true,
                    no_new_privs: true,
                    capabilities: true,
                    resource_limits: true,
                },
                allow_rootful: false,
                max_silence_ms: 20,
            },
        )
    }

    #[test]
    fn every_required_dimension_must_be_fully_enforced() {
        for observed in [
            Enforcement::Partial,
            Enforcement::NotEnforced,
            Enforcement::Unsupported,
        ] {
            let (mut node, requirements) = fixture();
            node.runtimes[0].dimensions = DimensionStates::uniform(observed);
            let errors = filter_runtime(&node, &requirements, UnixMillis(101))
                .expect_err("required constraints");
            assert_eq!(errors.len(), 5);
            assert_eq!(
                errors[0],
                RuntimeRejection::RequiredDimension {
                    dimension: SandboxDimension::Filesystem,
                    observed
                }
            );
            assert_eq!(
                errors[4],
                RuntimeRejection::RequiredDimension {
                    dimension: SandboxDimension::ResourceLimits,
                    observed
                }
            );
        }
        let (node, requirements) = fixture();
        assert_eq!(
            filter_runtime(&node, &requirements, UnixMillis(101)),
            Ok(node.runtimes[0])
        );
    }

    #[test]
    fn all_applicable_refusals_are_reported_in_stable_order() {
        let (mut node, mut requirements) = fixture();
        node.state = NodeState::Draining;
        requirements.backend = RuntimeBackend::OciDocker;
        node.runtimes[0].backend = RuntimeBackend::OciDocker;
        node.runtimes[0].dimensions.network = Enforcement::Partial;
        assert_eq!(
            filter_runtime(&node, &requirements, UnixMillis(121)),
            Err(vec![
                RuntimeRejection::NodeInactive(NodeState::Draining),
                RuntimeRejection::NodeStale,
                RuntimeRejection::RootfulDisallowed,
                RuntimeRejection::RuntimeStale,
                RuntimeRejection::RequiredDimension {
                    dimension: SandboxDimension::Network,
                    observed: Enforcement::Partial
                },
            ])
        );
    }

    #[test]
    fn absent_or_ambiguous_backend_never_falls_back() {
        let (mut node, requirements) = fixture();
        node.runtimes[0].backend = RuntimeBackend::Landlock;
        assert_eq!(
            filter_runtime(&node, &requirements, UnixMillis(101)),
            Err(vec![RuntimeRejection::RuntimeMissing])
        );
        node.runtimes[0].backend = RuntimeBackend::Bwrap;
        node.runtimes.push(node.runtimes[0]);
        assert_eq!(
            filter_runtime(&node, &requirements, UnixMillis(101)),
            Err(vec![RuntimeRejection::RuntimeAmbiguous])
        );
    }

    #[test]
    fn future_observations_refuse_and_explicit_rootful_opt_in_is_required() {
        let (mut node, mut requirements) = fixture();
        assert_eq!(
            filter_runtime(&node, &requirements, UnixMillis(99)),
            Err(vec![
                RuntimeRejection::NodeStale,
                RuntimeRejection::RuntimeStale
            ])
        );
        requirements.backend = RuntimeBackend::OciDocker;
        node.runtimes[0].backend = RuntimeBackend::OciDocker;
        assert_eq!(
            filter_runtime(&node, &requirements, UnixMillis(101)),
            Err(vec![RuntimeRejection::RootfulDisallowed])
        );
        requirements.allow_rootful = true;
        assert!(filter_runtime(&node, &requirements, UnixMillis(101)).is_ok());
    }
}
