//! Runs the executor against a **real** Podman/Docker engine.
//!
//! Ignored by default (needs an engine socket and a local image). Run with
//!
//! ```text
//! HARW_OCI_SOCKET=/run/podman/podman.sock \
//! HARW_OCI_IMAGE=docker.io/library/alpine@sha256:<manifest digest> \
//! HARW_OCI_ENGINE=podman HARW_OCI_PEER_UID=0 \
//! cargo test -p harw-job-executor-oci --test real_podman -- --ignored
//! ```
//!
//! `HARW_OCI_PEER_UID` is the uid that owns the socket (default: this user).
//! The image must already be present locally; nothing is pulled.

#![cfg(unix)]

use std::path::PathBuf;
use std::time::Duration;

use harw_container_model::{EngineKind, ImageDigest};
use harw_job_core::{
    AttemptId, ExitOutcome, JobSpec, ResourceRequest, RunnerId, SandboxProfileName,
    SandboxRequirement,
};
use harw_job_executor_oci::{GcPolicy, OciConfig, OciExecutor, PeerPolicy};
use harw_job_runtime::coordinator::{AttemptContext, AttemptEvent, Executor, Probe};
use harw_placement_model::UnixMillis;
use harw_types::WorkId;

type R<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn executor() -> R<OciExecutor> {
    let socket = std::env::var("HARW_OCI_SOCKET")?;
    let image = ImageDigest::parse(&std::env::var("HARW_OCI_IMAGE")?)?;
    let engine = match std::env::var("HARW_OCI_ENGINE").as_deref() {
        Ok("docker") => EngineKind::Docker,
        _ => EngineKind::Podman,
    };
    let mut cfg = OciConfig::new(PathBuf::from(socket), engine, "real-runner", "real-tenant")
        .with_image(SandboxProfileName::NoNetwork, image);
    if let Ok(uid) = std::env::var("HARW_OCI_PEER_UID") {
        cfg.peer = PeerPolicy::Uid(uid.parse()?);
    }
    Ok(OciExecutor::new(cfg)?)
}

fn ctx(job: &str, epoch: u64) -> R<AttemptContext> {
    Ok(AttemptContext {
        job_id: WorkId::from_str(job),
        attempt_id: AttemptId::new(format!("{job}-e{epoch}"))?,
        runner_id: RunnerId::new("real-runner")?,
        lease_epoch: epoch,
        workspace_root: PathBuf::from("/"),
        output_files: None,
        stdio_handoff: None,
    })
}

fn spec(program: &str, args: &[&str], requirement: SandboxRequirement) -> R<JobSpec> {
    Ok(JobSpec::command(program)
        .args(args.iter().copied())
        .sandbox(SandboxProfileName::NoNetwork)
        .sandbox_requirement(requirement)
        .resources(ResourceRequest {
            memory_max: Some(64 << 20),
            cpu_weight: Some(100),
            pids_max: Some(64),
            ..ResourceRequest::default()
        })
        .build()?)
}

async fn drain(
    mut run: harw_job_runtime::coordinator::AttemptRun,
) -> (Vec<u8>, Vec<u8>, Option<ExitOutcome>) {
    let (mut out, mut err, mut exit) = (Vec::new(), Vec::new(), None);
    while let Some(event) = run.next_event().await {
        match event {
            AttemptEvent::Stdout(b) => out.extend(b),
            AttemptEvent::Stderr(b) => err.extend(b),
            AttemptEvent::Exited { outcome, .. } => {
                exit = Some(outcome);
                break;
            }
            _ => {}
        }
    }
    (out, err, exit)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a real engine socket and a local image (see module docs)"]
async fn a_job_runs_in_a_real_container_with_the_read_back_report() -> R {
    let exec = executor()?;
    let context = ctx("real-echo", 1)?;
    let started = exec.start(
        &spec(
            "sh",
            &["-c", "echo hello; echo oops >&2; exit 3"],
            SandboxRequirement::Required,
        )?,
        &context,
    )?;
    let report = started.sandbox.ok_or("no report")?;
    assert!(
        report.satisfies(SandboxRequirement::Required),
        "real engine read-back: {report:?}"
    );
    assert!(started.identity.is_some());
    let (out, err, exit) = drain(started.run).await;
    assert_eq!(out, b"hello\n");
    assert_eq!(err, b"oops\n");
    assert_eq!(exit, Some(ExitOutcome::Exited(3)));
    assert_eq!(exec.recorded_exit(&context), Some(ExitOutcome::Exited(3)));
    exec.finished(&context);
    assert_eq!(exec.recorded_exit(&context), None, "finished removed it");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a real engine socket and a local image (see module docs)"]
async fn the_container_really_is_locked_down() -> R {
    let exec = executor()?;
    let context = ctx("real-lockdown", 1)?;
    // Effective capabilities, no_new_privs, read-only root, no network, as the
    // process inside sees them.
    let script = "grep -E '^(CapEff|NoNewPrivs):' /proc/self/status; \
                  touch /etc/x 2>&1 | head -1; \
                  (wget -q -T 2 -O- http://1.1.1.1 >/dev/null 2>&1 && echo NET=yes || echo NET=no)";
    let started = exec.start(
        &spec("sh", &["-c", script], SandboxRequirement::Required)?,
        &context,
    )?;
    let (out, _, exit) = drain(started.run).await;
    let text = String::from_utf8_lossy(&out).into_owned();
    exec.finished(&context);
    assert_eq!(exit, Some(ExitOutcome::Exited(0)), "{text}");
    assert!(
        text.contains("CapEff:\t0000000000000000"),
        "capabilities dropped: {text}"
    );
    assert!(text.contains("NoNewPrivs:\t1"), "no_new_privs: {text}");
    assert!(
        text.to_lowercase().contains("read-only"),
        "read-only root: {text}"
    );
    assert!(text.contains("NET=no"), "no network: {text}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a real engine socket and a local image (see module docs)"]
async fn probe_reattach_cancel_and_gc_work_against_a_real_engine() -> R {
    let exec = executor()?;
    let context = ctx("real-sleep", 4)?;
    let started = exec.start(
        &spec("sleep", &["60"], SandboxRequirement::BestEffort)?,
        &context,
    )?;
    let identity = started.identity.clone().ok_or("no identity")?;
    assert_eq!(exec.probe(&identity)?, Probe::Alive);
    let mut wrong = identity.clone();
    wrong.created_unix += 1;
    assert!(matches!(exec.probe(&wrong)?, Probe::Mismatch(_)));
    assert!(
        exec.reattach(&identity, &ctx("real-sleep", 9)?).is_err(),
        "stale epoch"
    );

    started.run.cancel();
    let (_, _, exit) = tokio::time::timeout(Duration::from_secs(40), drain(started.run)).await?;
    assert!(exit.is_some(), "the stream ends after cancel");
    assert_eq!(exec.probe(&identity)?, Probe::Exited);

    // GC: a store that knows epoch 5 makes epoch 4 stale; younger than the
    // grace nothing goes, afterwards it is removed.
    struct View;
    impl harw_job_executor_oci::AttemptView for View {
        fn current_epoch(&self, _: &str) -> Option<u64> {
            Some(5)
        }
        fn is_live(&self, _: &str, _: u64) -> bool {
            false
        }
    }
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs(),
    )?;
    let early = exec.collect_garbage(&View, now, GcPolicy::default())?;
    assert!(early.removed.is_empty(), "inside the grace: {early:?}");
    let late = exec.collect_garbage(&View, now + 3600, GcPolicy::default())?;
    assert!(!late.removed.is_empty(), "stale epoch removed: {late:?}");
    assert_eq!(exec.probe(&identity)?, Probe::Exited, "gone");
    Ok(())
}

#[test]
#[ignore = "needs a real engine socket and a local image (see module docs)"]
fn a_real_canary_offer_shows_what_the_engine_applied() -> R {
    let exec = executor()?;
    let offer = exec.probe_offer(SandboxProfileName::NoNetwork, UnixMillis(1_000), 30_000)?;
    assert!(offer.dimensions.all_enforced(), "{offer:?}");
    Ok(())
}
