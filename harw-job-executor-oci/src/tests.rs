use std::path::PathBuf;

use harw_container_model::{ContainerInstance, EngineKind, ImageDigest};
use harw_job_core::{
    AttemptId, ExitOutcome, JobSpec, ResourceRequest, RunnerId, SandboxProfileName,
    SandboxRequirement,
};
use harw_job_runtime::RuntimeError;
use harw_job_runtime::coordinator::{AttemptContext, AttemptEvent, Executor, Probe};
use harw_types::WorkId;

use crate::test_support::{Behavior, FakeEngine, IMAGE_HEX, TestError, TestResult, ctx};
use crate::{OciConfig, OciExecutor, PeerPolicy};

fn image() -> TestResult<ImageDigest> {
    ImageDigest::parse(&format!("rust@sha256:{IMAGE_HEX}")).map_err(ctx("image"))
}

fn config(engine: &FakeEngine) -> TestResult<OciConfig> {
    Ok(
        OciConfig::new(&engine.socket, EngineKind::Podman, "runner-1", "tenant-a")
            .with_image(SandboxProfileName::NoNetwork, image()?)
            .with_image(SandboxProfileName::WorkspaceBuild, image()?),
    )
}

fn executor(engine: &FakeEngine) -> TestResult<OciExecutor> {
    OciExecutor::new(config(engine)?).map_err(ctx("executor"))
}

fn context(epoch: u64) -> TestResult<AttemptContext> {
    Ok(AttemptContext {
        job_id: WorkId::from_str("job-1"),
        attempt_id: AttemptId::new(format!("job-1-e{epoch}")).map_err(ctx("attempt"))?,
        runner_id: RunnerId::new("runner-1").map_err(ctx("runner"))?,
        lease_epoch: epoch,
        workspace_root: PathBuf::from("/"),
        output_files: None,
        stdio_handoff: None,
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
async fn happy_path_streams_output_and_reports_all_enforced() -> TestResult {
    let engine = FakeEngine::start(Behavior::default())?;
    let exec = executor(&engine)?;
    let started = exec
        .start(
            &spec(SandboxRequirement::Required, Some(1 << 28))?,
            &context(3)?,
        )
        .map_err(ctx("start"))?;
    let report = started.sandbox.ok_or(TestError::Missing("report"))?;
    assert!(report.satisfies(SandboxRequirement::Required), "{report:?}");
    let identity = started.identity.ok_or(TestError::Missing("identity"))?;
    assert_eq!(identity.engine, EngineKind::Podman);
    assert_eq!(identity.created_unix, 1_791_021_600);

    let mut run = started.run;
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
    assert_eq!(out, b"hello\n");
    assert_eq!(err, b"oops\n");
    assert_eq!(exit, Some(ExitOutcome::Exited(0)));

    // The create asked for a locked-down container.
    let host = engine.host_config()?;
    assert_eq!(host["Privileged"], false);
    assert_eq!(host["CapDrop"][0], "ALL");
    assert_eq!(host["ReadonlyRootfs"], true);
    assert_eq!(host["NetworkMode"], "none");
    assert!(host.get("Binds").is_none(), "no host mounts");
    // create precedes start
    let reqs = engine.requests();
    let create = reqs
        .iter()
        .position(|(_, p)| p.ends_with("/containers/create"));
    let start = reqs.iter().position(|(_, p)| p.contains("/start"));
    assert!(create < start && start.is_some());
    Ok(())
}

#[test]
fn unmet_required_fails_before_the_body_starts() -> TestResult {
    let engine = FakeEngine::start(Behavior {
        drop_memory: true,
        ..Behavior::default()
    })?;
    let exec = executor(&engine)?;
    let result = exec.start(
        &spec(SandboxRequirement::Required, Some(1 << 28))?,
        &context(1)?,
    );
    assert!(
        matches!(result, Err(RuntimeError::SandboxRequirementNotMet { ref shortfalls, .. })
            if shortfalls == &["resource_limits"]),
        "{result:?}"
    );
    assert!(!engine.saw("POST", "/start"), "no start call");
    assert_eq!(engine.container_count(), 0, "unused container removed");
    // BestEffort accepts the partial report and runs.
    let ok = exec.start(
        &spec(SandboxRequirement::BestEffort, Some(1 << 28))?,
        &context(1)?,
    );
    assert!(ok.is_ok());
    Ok(())
}

#[test]
fn dropped_labels_are_refused_before_start() -> TestResult {
    let engine = FakeEngine::start(Behavior {
        spoof_owner: true,
        ..Behavior::default()
    })?;
    let result = executor(&engine)?.start(&spec(SandboxRequirement::None, None)?, &context(1)?);
    assert!(result.is_err());
    assert!(!engine.saw("POST", "/start"));
    Ok(())
}

#[test]
fn missing_image_is_refused_without_creating() -> TestResult {
    let engine = FakeEngine::start(Behavior {
        image_missing: true,
        ..Behavior::default()
    })?;
    let result = executor(&engine)?.start(&spec(SandboxRequirement::None, None)?, &context(1)?);
    assert!(result.is_err());
    assert!(!engine.saw("POST", "/containers/create"));
    Ok(())
}

#[test]
fn unconfigured_profile_and_bad_config_are_refused() -> TestResult {
    let engine = FakeEngine::start(Behavior::default())?;
    let spec = JobSpec::command("echo")
        .sandbox(SandboxProfileName::ReadOnlyAnalysis)
        .build()
        .map_err(ctx("spec"))?;
    assert!(matches!(
        executor(&engine)?.start(&spec, &context(1)?),
        Err(RuntimeError::Unsupported { .. })
    ));
    // A tag cannot even be expressed as an image.
    assert!(ImageDigest::parse("rust:latest").is_err());
    let mut bad = config(&engine)?;
    bad.engine = EngineKind::Kubernetes;
    assert!(OciExecutor::new(bad).is_err());
    let mut bad = config(&engine)?;
    bad.owner = "has space".into();
    assert!(OciExecutor::new(bad).is_err());
    Ok(())
}

#[test]
fn a_socket_of_another_user_is_refused() -> TestResult {
    let engine = FakeEngine::start(Behavior::default())?;
    let mut cfg = config(&engine)?;
    cfg.peer = PeerPolicy::Uid(rustix::process::getuid().as_raw().wrapping_add(1));
    let exec = OciExecutor::new(cfg).map_err(ctx("executor"))?;
    let result = exec.start(&spec(SandboxRequirement::None, None)?, &context(1)?);
    assert!(
        matches!(result, Err(RuntimeError::PermissionDenied { .. })),
        "{result:?}"
    );
    assert!(engine.requests().is_empty(), "nothing was sent");
    Ok(())
}

#[test]
fn oversized_engine_bodies_are_rejected() -> TestResult {
    let engine = FakeEngine::start(Behavior {
        huge_inspect: Some(64 * 1024),
        ..Behavior::default()
    })?;
    let mut cfg = config(&engine)?;
    cfg.max_body_bytes = 4 * 1024;
    let exec = OciExecutor::new(cfg).map_err(ctx("executor"))?;
    assert!(
        exec.start(&spec(SandboxRequirement::None, None)?, &context(1)?)
            .is_err()
    );
    assert!(!engine.saw("POST", "/start"));
    Ok(())
}

fn started_identity(
    engine: &FakeEngine,
    behavior: Behavior,
) -> TestResult<(OciExecutor, ContainerInstance)> {
    engine.set_behavior(|b| *b = behavior);
    let exec = executor(engine)?;
    let started = exec
        .start(&spec(SandboxRequirement::None, None)?, &context(2)?)
        .map_err(ctx("start"))?;
    Ok((
        exec,
        started.identity.ok_or(TestError::Missing("identity"))?,
    ))
}

#[test]
fn probe_verifies_identity_before_it_says_alive() -> TestResult {
    let engine = FakeEngine::start(Behavior::default())?;
    let (exec, identity) = started_identity(
        &engine,
        Behavior {
            run_forever: true,
            ..Behavior::default()
        },
    )?;
    assert_eq!(exec.probe(&identity).map_err(ctx("probe"))?, Probe::Alive);

    let mut wrong_time = identity.clone();
    wrong_time.created_unix += 1;
    assert!(matches!(exec.probe(&wrong_time), Ok(Probe::Mismatch(_))));

    engine.set_behavior(|b| b.spoof_owner = true);
    assert!(matches!(exec.probe(&identity), Ok(Probe::Mismatch(_))));
    engine.set_behavior(|b| b.spoof_owner = false);

    // Stale epoch: reattach under another lease epoch must not adopt it.
    let stale = exec.reattach(&identity, &context(9)?);
    assert!(
        matches!(stale, Err(RuntimeError::IdentityMismatch { .. })),
        "{stale:?}"
    );
    assert!(exec.reattach(&identity, &context(2)?).is_ok());

    // Unknown container id → exited.
    let mut gone = identity;
    gone.container_id =
        harw_container_model::ContainerId::new(&"b".repeat(64)).map_err(ctx("id"))?;
    assert_eq!(exec.probe(&gone).map_err(ctx("probe"))?, Probe::Exited);
    Ok(())
}

#[test]
fn recorded_exit_and_finished_use_the_labels() -> TestResult {
    let engine = FakeEngine::start(Behavior::default())?;
    let (exec, _identity) = started_identity(
        &engine,
        Behavior {
            exit_code: 3,
            ..Behavior::default()
        },
    )?;
    assert_eq!(
        exec.recorded_exit(&context(2)?),
        Some(ExitOutcome::Exited(3))
    );
    exec.finished(&context(2)?);
    assert_eq!(engine.container_count(), 0);
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn cancel_stops_the_container_and_ends_the_stream_once() -> TestResult {
    let engine = FakeEngine::start(Behavior {
        run_forever: true,
        ..Behavior::default()
    })?;
    let exec = executor(&engine)?;
    let mut run = exec
        .start(&spec(SandboxRequirement::None, None)?, &context(1)?)
        .map_err(ctx("start"))?
        .run;
    run.cancel();
    run.cancel();
    let mut exited = false;
    while let Some(event) = run.next_event().await {
        if let AttemptEvent::Exited { outcome, .. } = event {
            assert_eq!(outcome, ExitOutcome::Exited(137));
            exited = true;
            break;
        }
    }
    assert!(exited);
    let stops = engine
        .requests()
        .iter()
        .filter(|(_, p)| p.contains("/stop"))
        .count();
    assert_eq!(stops, 1, "cancel is idempotent");
    Ok(())
}

struct StoreView(u64);

impl crate::AttemptView for StoreView {
    fn current_epoch(&self, _: &str) -> Option<u64> {
        Some(self.0)
    }
    fn is_live(&self, _: &str, _: u64) -> bool {
        false
    }
}

#[test]
fn garbage_collection_removes_stale_epochs_only_after_grace() -> TestResult {
    let engine = FakeEngine::start(Behavior::default())?;
    let exec = executor(&engine)?;
    for epoch in [1, 2, 3] {
        exec.start(&spec(SandboxRequirement::None, None)?, &context(epoch)?)
            .map_err(ctx("start"))?;
    }
    assert_eq!(engine.container_count(), 3);
    let created = 1_791_021_600;
    let policy = crate::GcPolicy::default();
    // Inside the grace window nothing goes.
    let early = exec
        .collect_garbage(&StoreView(2), created + 5, policy)
        .map_err(ctx("gc"))?;
    assert!(early.removed.is_empty() && early.seen == 3);
    // After it: epoch 1 (stale) and epoch 2 (finished) go, epoch 3 (ahead) stays.
    let late = exec
        .collect_garbage(&StoreView(2), created + 600, policy)
        .map_err(ctx("gc"))?;
    assert_eq!(late.removed.len(), 2, "{late:?}");
    assert_eq!(engine.container_count(), 1);
    Ok(())
}

#[test]
fn a_container_of_another_image_is_refused_and_probe_notices() -> TestResult {
    let engine = FakeEngine::start(Behavior::default())?;
    let (exec, identity) = started_identity(
        &engine,
        Behavior {
            run_forever: true,
            ..Behavior::default()
        },
    )?;
    engine.set_behavior(|b| b.wrong_image = true);
    assert!(matches!(exec.probe(&identity), Ok(Probe::Mismatch(_))));
    let result = exec.start(&spec(SandboxRequirement::None, None)?, &context(7)?);
    assert!(result.is_err(), "created from another image than the pin");
    let starts = engine
        .requests()
        .iter()
        .filter(|(_, p)| p.contains("/start"))
        .count();
    assert_eq!(
        starts, 1,
        "only the first (valid) container was ever started"
    );
    Ok(())
}
