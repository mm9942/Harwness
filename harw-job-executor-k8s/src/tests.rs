use std::path::PathBuf;
use std::time::Duration;

use harw_container_model::{ImageDigest, PodInstance};
use harw_job_core::{
    AttemptId, ExitOutcome, JobSpec, ResourceRequest, RunnerId, SandboxProfileName,
    SandboxRequirement,
};
use harw_job_runtime::RuntimeError;
use harw_job_runtime::coordinator::{AttemptContext, AttemptEvent, Executor, Probe};
use harw_types::WorkId;

use crate::test_support::{Behavior, FakeApi, TestError, TestResult, ctx};
use crate::{K8sConfig, K8sExecutor};

const HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn config(api: &FakeApi, attested: bool) -> TestResult<K8sConfig> {
    let image = ImageDigest::parse(&format!("rust@sha256:{HEX}")).map_err(ctx("image"))?;
    let mut cfg = K8sConfig::new(&api.base, "tenant-a", "runner-1", "tenant-a")
        .with_image(SandboxProfileName::NoNetwork, image);
    cfg.network_attested = attested;
    cfg.poll_interval = Duration::from_millis(10);
    Ok(cfg)
}

fn executor(api: &FakeApi, attested: bool) -> TestResult<K8sExecutor> {
    K8sExecutor::new(config(api, attested)?).map_err(ctx("executor"))
}

fn context(epoch: u64) -> TestResult<AttemptContext> {
    Ok(AttemptContext {
        job_id: WorkId::from_str("job-1"),
        attempt_id: AttemptId::new(format!("job-1-e{epoch}")).map_err(ctx("attempt"))?,
        runner_id: RunnerId::new("runner-1").map_err(ctx("runner"))?,
        lease_epoch: epoch,
        workspace_root: PathBuf::from("/"),
    })
}

fn spec(requirement: SandboxRequirement, memory: Option<u64>) -> TestResult<JobSpec> {
    JobSpec::command("echo")
        .arg("hi")
        .sandbox(SandboxProfileName::NoNetwork)
        .sandbox_requirement(requirement)
        .resources(ResourceRequest {
            memory_max: memory,
            ..ResourceRequest::default()
        })
        .build()
        .map_err(ctx("spec"))
}

#[tokio::test(flavor = "current_thread")]
async fn gated_create_readback_then_lift_and_stream() -> TestResult {
    let api = FakeApi::start(Behavior {
        normalize_memory: true,
        ..Behavior::default()
    })?;
    let exec = executor(&api, true)?;
    let started = exec
        .start(
            &spec(SandboxRequirement::Required, Some(256 << 20))?,
            &context(3)?,
        )
        .map_err(ctx("start"))?;
    let report = started.sandbox.ok_or(TestError::Missing("report"))?;
    assert!(report.satisfies(SandboxRequirement::Required), "{report:?}");
    let identity = started.identity.ok_or(TestError::Missing("identity"))?;
    assert_eq!(identity.namespace, "tenant-a");

    let mut run = started.run;
    let (mut out, mut exit) = (Vec::new(), None);
    while let Some(event) = run.next_event().await {
        match event {
            AttemptEvent::Stdout(b) => out.extend(b),
            AttemptEvent::Exited { outcome, .. } => {
                exit = Some(outcome);
                break;
            }
            _ => {}
        }
    }
    assert_eq!(out, b"hello\n");
    assert_eq!(exit, Some(ExitOutcome::Exited(0)));
    // The gate is lifted only after the pod was created (and read back).
    let reqs = api.requests();
    let post = reqs.iter().position(|(m, _)| m == "POST");
    let patch = reqs.iter().position(|(m, _)| m == "PATCH");
    assert!(post < patch && patch.is_some());
    Ok(())
}

#[test]
fn unattested_network_fails_required_before_the_gate_is_lifted() -> TestResult {
    let api = FakeApi::start(Behavior::default())?;
    let exec = executor(&api, false)?;
    let result = exec.start(&spec(SandboxRequirement::Required, None)?, &context(1)?);
    assert!(
        matches!(result, Err(RuntimeError::SandboxRequirementNotMet { ref shortfalls, .. })
            if shortfalls == &["network"]),
        "{result:?}"
    );
    assert!(!api.saw("PATCH", "/pods/"), "gate never lifted");
    assert_eq!(api.pod_count(), 0, "gated pod deleted");
    // BestEffort accepts the partial report.
    assert!(
        exec.start(&spec(SandboxRequirement::BestEffort, None)?, &context(1)?)
            .is_ok()
    );
    Ok(())
}

#[test]
fn a_webhook_that_privileges_the_pod_is_caught_by_the_readback() -> TestResult {
    let api = FakeApi::start(Behavior {
        webhook_privileged: true,
        ..Behavior::default()
    })?;
    let exec = executor(&api, true)?;
    let result = exec.start(&spec(SandboxRequirement::Required, None)?, &context(1)?);
    assert!(matches!(
        result,
        Err(RuntimeError::SandboxRequirementNotMet { .. })
    ));
    assert!(!api.saw("PATCH", "/pods/"));
    Ok(())
}

#[test]
fn a_removed_gate_is_refused_even_for_best_effort() -> TestResult {
    let api = FakeApi::start(Behavior {
        strip_gate: true,
        ..Behavior::default()
    })?;
    let exec = executor(&api, true)?;
    assert!(
        exec.start(&spec(SandboxRequirement::None, None)?, &context(1)?)
            .is_err()
    );
    assert!(!api.saw("PATCH", "/pods/"));
    Ok(())
}

#[test]
fn oversized_api_answers_are_rejected() -> TestResult {
    let api = FakeApi::start(Behavior {
        huge_create: Some(64 * 1024),
        ..Behavior::default()
    })?;
    let mut cfg = config(&api, true)?;
    cfg.max_body_bytes = 8 * 1024;
    let exec = K8sExecutor::new(cfg).map_err(ctx("executor"))?;
    assert!(
        exec.start(&spec(SandboxRequirement::None, None)?, &context(1)?)
            .is_err()
    );
    assert!(!api.saw("PATCH", "/pods/"));
    Ok(())
}

fn running(api: &FakeApi) -> TestResult<(K8sExecutor, PodInstance)> {
    api.set_behavior(|b| b.run_forever = true);
    let exec = executor(api, true)?;
    let started = exec
        .start(&spec(SandboxRequirement::None, None)?, &context(2)?)
        .map_err(ctx("start"))?;
    Ok((
        exec,
        started.identity.ok_or(TestError::Missing("identity"))?,
    ))
}

#[test]
fn probe_checks_the_uid_and_reattach_the_epoch() -> TestResult {
    let api = FakeApi::start(Behavior::default())?;
    let (exec, identity) = running(&api)?;
    assert_eq!(exec.probe(&identity).map_err(ctx("probe"))?, Probe::Alive);
    let stale = exec.reattach(&identity, &context(9)?);
    assert!(
        matches!(stale, Err(RuntimeError::IdentityMismatch { .. })),
        "{stale:?}"
    );
    assert!(exec.reattach(&identity, &context(2)?).is_ok());

    api.recreate_pods();
    assert!(matches!(exec.probe(&identity), Ok(Probe::Mismatch(_))));

    let gone = PodInstance::new("tenant-a", "99999999-aaaa", "harw-missing").map_err(ctx("id"))?;
    assert_eq!(exec.probe(&gone).map_err(ctx("probe"))?, Probe::Exited);
    Ok(())
}

#[test]
fn recorded_exit_and_finished_use_the_labels() -> TestResult {
    let api = FakeApi::start(Behavior {
        exit_code: 3,
        ..Behavior::default()
    })?;
    let exec = executor(&api, true)?;
    let started = exec
        .start(&spec(SandboxRequirement::None, None)?, &context(2)?)
        .map_err(ctx("start"))?;
    // Wait until the fake reports the pod terminated.
    for _ in 0..200 {
        if exec.recorded_exit(&context(2)?).is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        exec.recorded_exit(&context(2)?),
        Some(ExitOutcome::Exited(3))
    );
    drop(started);
    exec.finished(&context(2)?);
    assert_eq!(api.pod_count(), 0);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn cancel_deletes_the_pod_gracefully_once() -> TestResult {
    let api = FakeApi::start(Behavior {
        run_forever: true,
        ..Behavior::default()
    })?;
    let exec = executor(&api, true)?;
    let mut run = exec
        .start(&spec(SandboxRequirement::None, None)?, &context(1)?)
        .map_err(ctx("start"))?
        .run;
    run.cancel();
    run.cancel();
    let mut exited = None;
    while let Some(event) = run.next_event().await {
        if let AttemptEvent::Exited { outcome, .. } = event {
            exited = Some(outcome);
            break;
        }
    }
    assert_eq!(exited, Some(ExitOutcome::Exited(137)));
    let deletes = api
        .requests()
        .iter()
        .filter(|(m, p)| m == "DELETE" && p.contains("gracePeriodSeconds=10"))
        .count();
    assert_eq!(deletes, 1, "cancel is idempotent");
    Ok(())
}

#[test]
fn configuration_is_validated_and_the_token_is_never_printed() -> TestResult {
    let ok = K8sConfig::new(
        "https://k8s.example:6443",
        "tenant-a",
        "runner-1",
        "tenant-a",
    );
    assert!(ok.validate().is_ok());
    let cleartext = K8sConfig::new(
        "http://k8s.example:6443",
        "tenant-a",
        "runner-1",
        "tenant-a",
    );
    assert!(cleartext.validate().is_err());
    let loopback = K8sConfig::new("http://127.0.0.1:8001", "tenant-a", "runner-1", "tenant-a");
    assert!(loopback.validate().is_ok());
    let bad_ns = K8sConfig::new("https://k8s.example", "Bad_NS", "runner-1", "tenant-a");
    assert!(bad_ns.validate().is_err());
    let mut with_token = ok;
    with_token.token = Some("s3cr3t-token".into());
    assert!(!format!("{with_token:?}").contains("s3cr3t"));
    assert!(ImageDigest::parse("rust:latest").is_err());
    Ok(())
}
