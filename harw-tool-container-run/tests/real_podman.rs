//! Runs `PodmanEngine` against a **real** `podman` binary.
//!
//! Ignored by default. Run with
//!
//! ```text
//! HARW_OCI_IMAGE=docker.io/library/alpine@sha256:<manifest digest> \
//! cargo test -p harw-tool-container-run --test real_podman -- --ignored
//! ```
//!
//! The image must already be present locally (the engine never pulls).

use harw_tool_container::{
    ContainerPlan, HostPath, ImageRef, Profile, RunConfig, RunRequest, engine_environment,
};
use harw_tool_container_run::{ContainerEngine, PodmanEngine};

type R<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[tokio::test]
#[ignore = "needs a real podman and a local image (see module docs)"]
async fn a_hermetic_run_works_and_the_read_back_verifies_against_real_podman() -> R {
    let image = ImageRef::parse(&std::env::var("HARW_OCI_IMAGE")?)?;
    let workspace = tempfile::tempdir()?;
    let workspace = HostPath::canonicalize(workspace.path().to_str().ok_or("path")?)?;
    let config = RunConfig::new("/usr/bin/podman", &workspace, "t1", "ses1")?;
    let request = RunRequest::new(
        image,
        Profile::Hermetic,
        vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "echo hello; echo oops >&2".to_owned(),
        ],
    )?
    .with_timeout_s(60);
    let plan = ContainerPlan::build(&config, &request)?;
    let env = engine_environment(&|name| std::env::var(name).ok(), false);
    let run = PodmanEngine::new(env).run(&plan, None).await?;
    assert_eq!(run.output.exit_code, Some(0), "{:?}", run.output);
    assert_eq!(run.output.stdout.trim(), "hello");
    assert_eq!(run.output.stderr.trim(), "oops");
    // The read-back must have happened and verified (not "ended before inspect").
    println!("readback = {:?}", run.readback);
    assert!(
        run.readback.is_some(),
        "the container must live long enough to be inspected"
    );
    Ok(())
}
