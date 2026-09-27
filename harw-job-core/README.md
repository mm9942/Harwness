# harw-job-core

Platform-neutral job model: the lowest layer of the job subsystem
(`docs/planning/20-job-runtime/HARW_LINUX_JOB_RUNTIME_FOUNDATION.md` §6).

- `lifecycle` — `LifecycleState` and the single validated transition table.
- `ids` — `AttemptId`, `RunnerId`, `FencingToken`, `IdempotencyKey`, `JobScopeId`.
- `outcome` — `ExitOutcome`, `CancellationCause`, `Deadline`.
- `spec` — typed `JobSpec` in a versioned `JobSpecEnvelope`.
- `enforcement` — per-dimension `SandboxReport` (never a `bool sandboxed`).
- `budget`, `error`, `job`, `lease`, `retry`, `stored` — governance
  primitives moved from `harw-job-runtime`, which re-exports this crate.

No async runtime, no `rustix`, no `procfs`; `#![forbid(unsafe_code)]`.
The moved modules still use `harw-types`, `harw-macros` and `harw-observe`;
see "Known inversions" in `src/lib.rs`.
