//! Placement offer from a canary read-back.

use std::path::PathBuf;

use harw_container_model::EngineKind;
use harw_job_core::{JobSpec, ResourceRequest, SandboxProfileName, SandboxRequirement};
use harw_job_runtime::RuntimeError;
use harw_job_runtime::coordinator::AttemptContext;
use harw_job_runtime::offer::runtime_offer;
use harw_placement_model::{RuntimeBackend, RuntimeOffer, UnixMillis};
use harw_types::WorkId;

use crate::config::PeerPolicy;
use crate::executor::OciExecutor;

impl OciExecutor {
    /// The backend kind this executor would offer.
    #[must_use]
    pub fn backend(&self) -> RuntimeBackend {
        let cfg = self.engine_config();
        match (cfg.engine, cfg.peer) {
            (EngineKind::Docker, _) => RuntimeBackend::OciDocker,
            (_, PeerPolicy::Uid(0)) => RuntimeBackend::OciPodmanRootful,
            _ => RuntimeBackend::OciPodmanRootless,
        }
    }

    /// Creates a canary container for `profile` (labelled `harw-canary`,
    /// never started), reads its applied configuration back and removes it.
    /// The offer's dimensions are what the engine really applied: limits are
    /// requested so that a dropped limit shows up as not enforced.
    ///
    /// # Errors
    /// The engine is unreachable, the image is missing or the canary could
    /// not be created; there is then no offer (fail closed).
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
            attempt_id: harw_job_core::AttemptId::new("harw-canary-e0").map_err(|e| {
                RuntimeError::Unsupported {
                    operation: "canary attempt id",
                    detail: e.to_string(),
                }
            })?,
            runner_id: harw_job_core::RunnerId::new("harw-canary").map_err(|e| {
                RuntimeError::Unsupported {
                    operation: "canary runner id",
                    detail: e.to_string(),
                }
            })?,
            lease_epoch: 0,
            workspace_root: PathBuf::from("/"),
            output_files: None,
        };
        let prepared = self.create_and_readback(&spec, &ctx)?;
        self.discard_container(&prepared.id);
        Ok(runtime_offer(self.backend(), &prepared.report, now, ttl_ms))
    }
}

#[cfg(test)]
mod tests {
    use harw_container_model::{EngineKind, ImageDigest};
    use harw_job_core::SandboxProfileName;
    use harw_placement_model::{Enforcement, RuntimeBackend, UnixMillis};

    use crate::test_support::{Behavior, FakeEngine, IMAGE_HEX, TestResult, ctx};
    use crate::{OciConfig, OciExecutor, PeerPolicy};

    fn exec(engine: &FakeEngine, kind: EngineKind, peer: PeerPolicy) -> TestResult<OciExecutor> {
        let image =
            ImageDigest::parse(&format!("rust@sha256:{IMAGE_HEX}")).map_err(ctx("image"))?;
        let mut cfg = OciConfig::new(&engine.socket, kind, "runner-1", "tenant-a")
            .with_image(SandboxProfileName::NoNetwork, image);
        cfg.peer = peer;
        OciExecutor::new(cfg).map_err(ctx("executor"))
    }

    #[test]
    fn canary_read_back_becomes_a_fresh_offer_and_leaves_nothing_behind() -> TestResult {
        let engine = FakeEngine::start(Behavior::default())?;
        let exec = exec(&engine, EngineKind::Podman, PeerPolicy::SameUser)?;
        let offer = exec
            .probe_offer(SandboxProfileName::NoNetwork, UnixMillis(1_000), 30_000)
            .map_err(ctx("offer"))?;
        assert_eq!(offer.backend, RuntimeBackend::OciPodmanRootless);
        assert!(offer.dimensions.all_enforced(), "{offer:?}");
        assert!(offer.is_eligible(UnixMillis(5_000), false));
        assert!(!offer.is_fresh(UnixMillis(40_000)));
        assert!(!engine.saw("POST", "/start"), "the canary never runs");
        assert_eq!(engine.container_count(), 0, "canary removed");
        Ok(())
    }

    #[test]
    fn a_dropped_limit_shows_up_in_the_offer() -> TestResult {
        let engine = FakeEngine::start(Behavior {
            drop_memory: true,
            ..Behavior::default()
        })?;
        let exec = exec(&engine, EngineKind::Podman, PeerPolicy::SameUser)?;
        let offer = exec
            .probe_offer(SandboxProfileName::NoNetwork, UnixMillis(1), 1_000)
            .map_err(ctx("offer"))?;
        assert_eq!(offer.dimensions.resource_limits, Enforcement::Partial);
        Ok(())
    }

    #[test]
    fn rootful_and_docker_offers_are_marked_rootful() -> TestResult {
        let engine = FakeEngine::start(Behavior::default())?;
        let docker = exec(&engine, EngineKind::Docker, PeerPolicy::SameUser)?;
        assert_eq!(docker.backend(), RuntimeBackend::OciDocker);
        assert!(docker.backend().is_rootful());
        let rootful = exec(&engine, EngineKind::Podman, PeerPolicy::Uid(0))?;
        assert_eq!(rootful.backend(), RuntimeBackend::OciPodmanRootful);
        Ok(())
    }

    #[test]
    fn an_unreachable_engine_yields_no_offer() -> TestResult {
        let engine = FakeEngine::start(Behavior::default())?;
        let exec = exec(&engine, EngineKind::Podman, PeerPolicy::SameUser)?;
        drop(engine);
        let result = exec.probe_offer(SandboxProfileName::NoNetwork, UnixMillis(1), 1_000);
        assert!(result.is_err());
        Ok(())
    }
}
