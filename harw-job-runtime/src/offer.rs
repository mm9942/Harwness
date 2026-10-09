//! Turns an observed [`SandboxReport`] into a placement [`RuntimeOffer`].
//!
//! The report must come from a real read-back (an executor's inspect of a
//! created-but-not-started instance); this module only translates.

use harw_job_core::{EnforcementState, SandboxReport};
use harw_placement_model::{
    DimensionStates, Enforcement, RuntimeBackend, RuntimeOffer, UnixMillis,
};

fn enforcement(state: EnforcementState) -> Enforcement {
    match state {
        EnforcementState::Enforced => Enforcement::Enforced,
        EnforcementState::Partial => Enforcement::Partial,
        EnforcementState::NotEnforced => Enforcement::NotEnforced,
        // `EnforcementState` is closed today; anything new is not enforced.
        _ => Enforcement::Unsupported,
    }
}

/// The placement view of a report.
#[must_use]
pub fn dimensions_of(report: &SandboxReport) -> DimensionStates {
    DimensionStates {
        filesystem: enforcement(report.filesystem),
        network: enforcement(report.network),
        no_new_privs: enforcement(report.no_new_privs),
        capabilities: enforcement(report.capabilities),
        resource_limits: enforcement(report.resource_limits),
    }
}

/// An offer for `backend` from a read-back taken at `observed`.
#[must_use]
pub fn runtime_offer(
    backend: RuntimeBackend,
    report: &SandboxReport,
    observed: UnixMillis,
    ttl_ms: u64,
) -> RuntimeOffer {
    RuntimeOffer {
        backend,
        dimensions: dimensions_of(report),
        observed,
        ttl_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_dimension_maps_one_to_one() {
        let report = SandboxReport {
            filesystem: EnforcementState::Enforced,
            network: EnforcementState::Partial,
            no_new_privs: EnforcementState::NotEnforced,
            capabilities: EnforcementState::Unsupported,
            resource_limits: EnforcementState::Enforced,
        };
        let offer = runtime_offer(RuntimeBackend::K8sPod, &report, UnixMillis(7), 500);
        assert_eq!(offer.dimensions.network, Enforcement::Partial);
        assert_eq!(offer.dimensions.no_new_privs, Enforcement::NotEnforced);
        assert_eq!(offer.dimensions.capabilities, Enforcement::Unsupported);
        assert!(!offer.dimensions.all_enforced());
        let full = dimensions_of(&SandboxReport::uniform(EnforcementState::Enforced));
        assert!(full.all_enforced());
    }
}
