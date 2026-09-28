# harw-job

Public facade of the Harw job runtime
(`docs/planning/20-job-runtime/HARW_LINUX_JOB_RUNTIME_FOUNDATION.md` §16,
Phase 9). The common case is small:

```rust
let runtime = JobRuntime::builder()
    .store(FsJobRecordStore::create_ambient(Path::new("/var/lib/app/jobs"))?)
    .executor(LinuxExecutor::default())
    .runner_id(RunnerId::new("build-runner-1")?)
    .workspace_root("/srv/workspace")
    .build()?;

let job = runtime
    .submit(
        JobSpec::command("cargo")
            .arg("test")
            .workspace(WorkspacePath::new("crates/app")?)
            .resources(ResourceRequest::default())
            .sandbox(SandboxProfile::WorkspaceBuild),
    )
    .await?;

let outcome = job.wait().await?; // JobResult: state, exit, reason, sandbox report, output
```

## What happens underneath

| Step | Crate |
|---|---|
| Job record (`Queued`), claim with runner id + lease TTL, heartbeat every TTL/3, fenced finalization | `harw-job-store` via `harw-job-runtime` |
| Attempt lifecycle `Claimed → Starting → Running → Succeeded / Failed / TimedOut / Cancelled / Lost`, persisted per attempt (`attempts/<attempt>.json`) | `harw-job-core`, `harw-job-runtime` |
| Process group + pidfd authority, optional cgroup v2 job boundary, recovery identity | `harw-job-linux` |
| Sandbox backend: none, Landlock trampoline (`harw-job-exec`), or Bubblewrap (feature `bwrap`) | `harw-job-exec`, `harw-job-executor-bwrap` |
| Async supervision (exit, stdout/stderr, termination) | `harw-job-tokio` |
| macOS (process group, `sandbox-exec`, no reattach) | `harw-job-darwin` |

## Guarantees

- **Sandbox requirement.** A `SandboxRequirement::Required` job whose
  enforcement is known to fall short fails with a `policy:` reason
  *before* its body runs. The Landlock trampoline checks the full
  requirement itself and exits `126` without running the job.
- **Fencing.** The job record is only ever written with the current lease.
  A runner that loses its lease stops the process and records `Lost` in
  its own attempt record; it cannot finalize the job.
- **Recovery.** After a restart, `runtime.recover()` (same runner id)
  reattaches verified live processes, finalizes processes that exited
  while the runner was down (with their recorded status when the
  exit-status shim is enabled, otherwise `Lost`), and marks identity
  mismatches `Lost`. A PID alone never authorizes a signal: an unrelated
  process that reused the PID is never killed.

## Configuration

- `LinuxExecutorOptions::cgroup_root` — a delegated cgroup v2 directory;
  each attempt gets its own child cgroup with the requested
  `ResourceRequest` limits. Without it, requested limits are reported as
  `not_enforced`.
- `LinuxExecutorOptions::sandbox` — `LinuxSandboxBackend::None` (default),
  `LandlockTrampoline { trampoline, plan_dir }` or `Bwrap(..)` (feature
  `bwrap`).
- `LinuxExecutorOptions::exit_status_dir` — enables the `/bin/sh`
  exit-status shim so a job's exit status survives a runtime restart.

## Layering

`harw-job` is job infrastructure (layer J): it depends only on `J`/`F`
crates and exposes only first-party types. The lower crates stay usable on
their own for expert use.
