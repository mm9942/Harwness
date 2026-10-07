//! Placement offer from a canary pod read-back.

use std::path::PathBuf;

use harw_job_core::{
    AttemptId, JobSpec, ResourceRequest, RunnerId, SandboxProfileName, SandboxRequirement,
};
use harw_job_runtime::RuntimeError;
use harw_job_runtime::coordinator::AttemptContext;
use harw_job_runtime::offer::runtime_offer;
use harw_placement_model::{RuntimeBackend, RuntimeOffer, UnixMillis};
use harw_types::WorkId;

use crate::executor::K8sExecutor;

fn id_error(what: &'static str, error: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Unsupported {
        operation: what,
        detail: error.to_string(),
    }
}

impl K8sExecutor {
    /// Creates a canary pod for `profile` behind its scheduling gate (the
    /// gate is never lifted, so nothing runs), reads the admitted pod back
    /// and deletes it. CPU weight and pids ceilings are requested too, so
    /// the offer shows they are not expressible per pod.
    ///
    /// # Errors
    /// The API is unreachable, the credential is refused or the canary was
    /// not admitted as asked; there is then no offer (fail closed).
    pub fn probe_offer(
        &self,
        profile: SandboxProfileName,
        now: UnixMillis,
        ttl_ms: u64,
    ) -> Result<RuntimeOffer, RuntimeError> {
        let spec = JobSpec::command("true")
            .sandbox(profile)
            .sandbox_requirement(SandboxRequirement::BestEffort)
            .resources(ResourceRequest {
                memory_max: Some(64 << 20),
                cpu_weight: Some(100),
                pids_max: Some(64),
                ..ResourceRequest::default()
            })
            .build()?;
        let ctx = AttemptContext {
            job_id: WorkId::from_str("harw-canary"),
            attempt_id: AttemptId::new("harw-canary-e0")
                .map_err(|e| id_error("canary attempt", e))?,
            runner_id: RunnerId::new("harw-canary").map_err(|e| id_error("canary runner", e))?,
            lease_epoch: 0,
            workspace_root: PathBuf::from("/"),
            output_files: None,
            stdio_handoff: None,
        };
        let prepared = self.create_and_readback(&spec, &ctx)?;
        self.delete_now(&prepared.name);
        Ok(runtime_offer(
            RuntimeBackend::K8sPod,
            &prepared.report,
            now,
            ttl_ms,
        ))
    }

    /// The namespace this executor offers (one per tenant).
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.config().namespace
    }
}

#[cfg(test)]
mod tests {
    use harw_job_core::SandboxProfileName;
    use harw_placement_model::{Enforcement, RuntimeBackend, UnixMillis};

    use crate::test_support::{Behavior, FakeApi, TestResult, ctx};
    use crate::{K8sConfig, K8sExecutor};

    fn exec(api: &FakeApi, attested: bool) -> TestResult<K8sExecutor> {
        let image = harw_container_model::ImageDigest::parse(&format!(
            "rust@sha256:{}",
            "0123456789abcdef".repeat(4)
        ))
        .map_err(ctx("image"))?;
        let mut cfg = K8sConfig::new(&api.base, "tenant-a", "runner-1", "tenant-a")
            .with_image(SandboxProfileName::NoNetwork, image);
        cfg.network_attested = attested;
        K8sExecutor::new(cfg).map_err(ctx("executor"))
    }

    #[test]
    fn canary_pod_read_back_is_the_offer_and_nothing_is_scheduled() -> TestResult {
        let api = FakeApi::start(Behavior::default())?;
        let offer = exec(&api, true)?
            .probe_offer(SandboxProfileName::NoNetwork, UnixMillis(1_000), 30_000)
            .map_err(ctx("offer"))?;
        assert_eq!(offer.backend, RuntimeBackend::K8sPod);
        // Not expressible per pod, whatever the cluster does.
        assert_eq!(offer.dimensions.resource_limits, Enforcement::Partial);
        assert_eq!(offer.dimensions.network, Enforcement::Enforced);
        assert!(!api.saw("PATCH", "/pods/"), "gate never lifted");
        assert_eq!(api.pod_count(), 0, "canary deleted");
        Ok(())
    }

    #[test]
    fn without_attestation_the_network_offer_is_partial() -> TestResult {
        let api = FakeApi::start(Behavior::default())?;
        let offer = exec(&api, false)?
            .probe_offer(SandboxProfileName::NoNetwork, UnixMillis(1), 1_000)
            .map_err(ctx("offer"))?;
        assert_eq!(offer.dimensions.network, Enforcement::Partial);
        Ok(())
    }

    #[test]
    fn a_webhook_that_breaks_the_canary_shows_up_or_refuses_the_offer() -> TestResult {
        let api = FakeApi::start(Behavior {
            webhook_privileged: true,
            ..Behavior::default()
        })?;
        let offer = exec(&api, true)?
            .probe_offer(SandboxProfileName::NoNetwork, UnixMillis(1), 1_000)
            .map_err(ctx("offer"))?;
        assert_eq!(offer.dimensions.capabilities, Enforcement::NotEnforced);
        assert!(!offer.dimensions.all_enforced());
        Ok(())
    }

    #[test]
    fn an_unreachable_api_yields_no_offer() -> TestResult {
        let api = FakeApi::start(Behavior::default())?;
        let exec = exec(&api, true)?;
        drop(api);
        assert!(
            exec.probe_offer(SandboxProfileName::NoNetwork, UnixMillis(1), 1_000)
                .is_err()
        );
        Ok(())
    }
}
