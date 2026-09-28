# harw-job-tokio

The async runtime layer of the Harw job runtime: §3.2, §13 and Phase 6 of
`docs/planning/20-job-runtime/HARW_LINUX_JOB_RUNTIME_FOUNDATION.md`. It adapts
the synchronous pidfd handles of `harw-job-linux` to Tokio. The crate has
`#![forbid(unsafe_code)]` and is Linux-only; on other targets it compiles to
an empty library.

| Type | Doc § | What it does |
|---|---|---|
| `AsyncLinuxProcess` | §3.2 | Wraps a `LinuxProcess` or a `LinuxJobGroup` (`SupervisedTarget`). Exit readiness is awaited with `tokio::io::unix::AsyncFd<OwnedFd>` on a duplicate of the pidfd. `wait`, `wait_timeout` and `terminate` are async. |
| `Supervisor` / `SupervisorHandle` | §13, Phase 6 | One task per process that fans in pidfd exit readiness, stdout, stderr, a deadline and cancellation, and sends `ProcessEvent`s over a bounded `mpsc` channel. |
| `ProcessEvent` | §13 | `Stdout(Bytes)`, `Stderr(Bytes)`, `Exited(ExitOutcome)`, `Timeout`, `CancelRequested`, `SupervisorError(ProcessError)`. |
| `Canceller` | §13 | A clonable handle that requests cancellation (`tokio::sync::watch`, no `tokio-util`). |

## Usage

```rust,ignore
let (process, stdio) = LinuxProcess::spawn(&mut command)?;
let process = AsyncLinuxProcess::new(process)?;
let config = SupervisorConfig {
    deadline: Some(Duration::from_secs(60)),
    ..SupervisorConfig::default()
};
let (handle, mut events) = Supervisor::spawn(process, stdio.stdout, stdio.stderr, config);
while let Some(event) = events.recv().await {
    // The runtime decides state transitions; this layer only reports.
}
let process = handle.join().await?; // reaped
```

## Event contract

- The layer reports events and does not own the durable job state machine
  (§13). The runtime decides transitions.
- `Stdout` and `Stderr` chunks keep pipe order within each stream. Chunk
  boundaries are arbitrary, and there is no line framing.
- `Timeout` or `CancelRequested` is sent at most once in total (the first
  trigger wins), and only while the primary is still running. The
  supervisor then applies the `TerminationPolicy` in the same loop: the
  graceful signal, then a timer for the grace period, then `SIGKILL`.
  Output keeps being drained during this shutdown.
- `Exited` is always the **last** event. It is sent after both streams reach
  EOF, or after `drain_timeout` (counted from the exit) when a surviving
  descendant holds a pipe open.
- `SupervisorError` is terminal (no `Exited` follows) only if observing the
  exit itself failed. `join` then returns the unreaped process to the
  caller. Signal and read failures are reported, and supervision continues.

## Design decisions and known limits

- **Pidfd duplicate.** `AsyncFd` has to own the descriptor it registers, so
  `harw-job-linux` provides `LinuxProcess::readiness_fd()`, a `CLOEXEC`
  duplicate. The duplicate is used only for readability. Signals and
  `waitid` still go through the `LinuxProcess`, which caches the outcome.
- **Adoption failure.** If registering with the reactor fails,
  `AdoptError` hands the target back unharmed. Nothing is killed on the
  caller's behalf.
- **Groups.** For a `LinuxJobGroup`, job-wide signals go to the process group
  and, through the pidfd, to the primary. With `hard_kill`, surviving group
  members are killed before the primary is reaped, because the group id is
  only safe to use until then. Job cgroups are not handled here; use
  `LinuxJobGroup::terminate` for them.
- **Backpressure.** Events are sent with `send().await`, so a consumer that
  stops reading also pauses the loop's timers.
- **Abandonment.** If the event receiver is dropped, the supervisor notices
  immediately (`Sender::closed`). It closes the pipes, terminates the job
  with a forced hard kill after the grace period, and reaps it. Dropping the
  `SupervisorHandle` does not cancel the job.
- **Deadline** is counted from `Supervisor::spawn`.

## Tests

`cargo test -p harw-job-tokio` runs:

- unit tests in `process.rs`: exit code, `wait_timeout`, escalation when
  TERM is ignored, graceful group termination, errno mapping
- unit tests in `supervisor.rs`: exit event, stdout/stderr order with
  `Exited` last, 200 KB output under backpressure, deadline, cancellation,
  escalation to `SIGKILL`, the drain bound, abandonment, clamped config
- `tests/fd_leak.rs`: its own binary that runs 50 concurrent supervisors and
  compares `/proc/self/fd` before and after
