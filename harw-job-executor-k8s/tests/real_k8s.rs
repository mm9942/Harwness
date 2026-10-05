//! Runs `K8sExecutor` against a **real** `kube-apiserver` (no kubelet needed:
//! the pod is created, read back, ungated and deleted; it never runs).
//!
//! Ignored by default. Needs a namespace `tenant-a` and
//!
//! ```text
//! HARW_K8S_API=https://127.0.0.1:6443 HARW_K8S_TOKEN=<bearer> \
//! HARW_K8S_CA=/path/ca.crt \
//! cargo test -p harw-job-executor-k8s --test real_k8s -- --ignored --test-threads=1
//! ```

use std::path::PathBuf;
use std::time::Duration;

use harw_container_model::ImageDigest;
use harw_job_core::{
    AttemptId, ExitOutcome, JobSpec, ResourceRequest, RunnerId, SandboxProfileName,
    SandboxRequirement,
};
use harw_job_executor_k8s::{K8sConfig, K8sExecutor};
use harw_job_runtime::coordinator::{AttemptContext, AttemptEvent, Executor, Probe};
use harw_placement_model::{Enforcement, UnixMillis};
use harw_types::WorkId;

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn executor(attested: bool) -> R<K8sExecutor> {
    let image = ImageDigest::parse(&format!("registry.k8s.io/pause@sha256:{}", "ab".repeat(32)))?;
    let mut cfg = K8sConfig::new(
        &std::env::var("HARW_K8S_API")?,
        "tenant-a",
        "real-runner",
        "tenant-a",
    )
    .with_image(SandboxProfileName::NoNetwork, image);
    cfg.token = Some(std::env::var("HARW_K8S_TOKEN")?);
    cfg.ca_pem = Some(std::fs::read(std::env::var("HARW_K8S_CA")?)?);
    cfg.network_attested = attested;
    cfg.poll_interval = Duration::from_millis(200);
    Ok(K8sExecutor::new(cfg)?)
}

fn ctx(job: &str, epoch: u64) -> R<AttemptContext> {
    Ok(AttemptContext {
        job_id: WorkId::from_str(job),
        attempt_id: AttemptId::new(format!("{job}-e{epoch}"))?,
        runner_id: RunnerId::new("real-runner")?,
        lease_epoch: epoch,
        workspace_root: PathBuf::from("/"),
    })
}

fn spec(requirement: SandboxRequirement) -> R<JobSpec> {
    Ok(JobSpec::command("true")
        .sandbox(SandboxProfileName::NoNetwork)
        .sandbox_requirement(requirement)
        .resources(ResourceRequest {
            memory_max: Some(64 << 20),
            ..ResourceRequest::default()
        })
        .build()?)
}

#[test]
#[ignore = "needs a real kube-apiserver (see module docs)"]
fn a_real_api_server_admits_the_pod_and_the_canary_offer_reflects_it() -> R {
    let exec = executor(true)?;
    let offer = exec.probe_offer(SandboxProfileName::NoNetwork, UnixMillis(1_000), 30_000)?;
    println!("offer = {offer:?}");
    assert_eq!(offer.dimensions.filesystem, Enforcement::Enforced);
    assert_eq!(offer.dimensions.no_new_privs, Enforcement::Enforced);
    assert_eq!(offer.dimensions.capabilities, Enforcement::Enforced);
    assert_eq!(offer.dimensions.network, Enforcement::Enforced, "attested");
    let unattested =
        executor(false)?.probe_offer(SandboxProfileName::NoNetwork, UnixMillis(1), 1)?;
    assert_eq!(unattested.dimensions.network, Enforcement::Partial);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a real kube-apiserver (see module docs)"]
async fn gate_lift_probe_identity_cancel_and_cleanup_against_a_real_api_server() -> R {
    let exec = executor(true)?;
    let context = ctx("real-k8s-job", 3)?;
    let started = exec.start(&spec(SandboxRequirement::Required)?, &context)?;
    let report = started.sandbox.ok_or("no report")?;
    println!("report = {report:?}");
    assert!(report.satisfies(SandboxRequirement::Required));
    let identity = started.identity.clone().ok_or("no identity")?;
    // No kubelet: the pod stays Pending but exists and is ours.
    assert_eq!(exec.probe(&identity)?, Probe::Alive);
    assert!(
        exec.reattach(&identity, &ctx("real-k8s-job", 9)?).is_err(),
        "stale epoch"
    );
    let mut forged = identity.clone();
    forged.pod_uid = "00000000-0000-0000-0000-000000000000".to_owned();
    assert!(matches!(exec.probe(&forged)?, Probe::Mismatch(_)));

    // The gate really is gone (the real API accepted the JSON patch).
    started.run.cancel();
    let mut run = started.run;
    let end = tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(event) = run.next_event().await {
            if let AttemptEvent::Exited { outcome, .. } = event {
                return Some(outcome);
            }
        }
        None
    })
    .await?;
    assert_eq!(
        end,
        Some(ExitOutcome::Unknown),
        "an unscheduled pod is deleted outright"
    );
    assert_eq!(exec.probe(&identity)?, Probe::Exited);
    exec.finished(&context);
    Ok(())
}
