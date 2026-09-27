# harw-job-linux

Linux mechanics of the Harw job runtime: Phases 2 to 5 of
`docs/planning/20-job-runtime/HARW_LINUX_JOB_RUNTIME_FOUNDATION.md`. The crate
has `#![forbid(unsafe_code)]`. All Linux ABI edges go through `rustix`,
`procfs`, `landlock` and `cap-std`, and none of their types appear in the
public API.

| Module | Doc § | What it does |
|---|---|---|
| `process` | §3.1, §9 | `LinuxProcess`: controls a process through its pidfd (`pidfd_open`, `pidfd_send_signal`, `waitid(P_PIDFD)`, `poll`). It has no `Clone` and never exposes the raw fd; `readiness_fd()` hands out only a CLOEXEC duplicate for async exit readiness (harw-job-tokio). |
| `group` | §2.2, §10 | `LinuxJobGroup`: the primary process, its own process group and optionally a job cgroup. `TerminationPolicy` runs graceful signal, then the grace period, then a hard kill. |
| `proc` | §3.3 | Reads `/proc` through `procfs`: start ticks, state, exe and the v2 cgroup path. |
| `recovery` | §15, §21.4 | `LinuxRecoveryIdentity`: a process is signalled only after its identity has been verified with the pidfd held. A PID on its own is never proof. |
| `resources` | §11 | `JobResources` becomes typed cgroup writes. `RlimitSet` is applied to the calling process. |
| `cgroup` | §3.6, Phase 4 | The `CgroupBackend` trait and `CgroupV2Fs`, an in-house cgroupfs backend that is rooted at a delegated directory. |
| `capabilities` | §4.2 | Drops capabilities through rustix. No second capability crate. |
| `sandbox` | §3.5, §12 | `SandboxPolicy`, the profiles, Landlock planning and `apply_to_current_process`, which returns a `SandboxReport`. |
| `filesystem` | §3.4 | `CapDir`: filesystem authority as a directory capability. |

## Features

```toml
default         = ["linux-basic"]
linux-basic     = ["dep:procfs"]                                    # proc, recovery
linux-sandbox   = ["linux-basic", "dep:landlock", "dep:cap-std"]    # enforcement, CapDir
linux-cgroup-v2 = ["linux-basic", "dep:cap-std"]                    # CgroupV2Fs
```

`rustix` is always a dependency. On targets other than Linux the crate
compiles to an empty library.

## Design decisions and known limits

- **Spawn and pidfd.** A child is spawned with `std::process::Command`, and
  `pidfd_open(child.id())` is called straight away. This is not a PID-reuse
  race: the child is our own and has not been reaped, and the only way it is
  ever reaped is through the pidfd.
- **cgroup attach window.** The child is attached right after the pidfd is
  opened, so it runs outside its cgroup for a few microseconds. Closing that
  window needs the child to attach itself before `exec` (see
  `CgroupBackend::attach_self`, planned `harw-job-exec` trampoline). Doing it
  in `pre_exec` would require `unsafe` (§24).
- **Process-group signals.** These are sent only while the primary process
  has not been reaped, because only then can its group id not belong to
  anyone else.
- **Sandbox scope.** The sandbox and capability code acts on the calling
  thread, and the change cannot be undone. `apply_to_current_process` is
  meant for a single-threaded trampoline that calls it right before `exec`.
  It is never called in tests.
- **Honest reporting.**
  - Filesystem: `Enforced` requires Landlock ABI V5.
  - Network: a network deny is at most `Partial`, because Landlock covers
    only TCP bind and connect.
  - `resource_limits`: this module leaves it at `NotEnforced`; the caller
    fills it in from the cgroup and rlimit steps.
- **`NetworkRestricted`.** This profile maps to
  `NetworkPolicy::ConnectTcpPorts([80, 443])`.
- **In-house cgroup v2 backend.** It replaces Youki's `libcgroups`, which
  avoids MSRV risk and any optional BPF or C surface. Controllers that are
  not delegated are reported through `CgroupHandle::limits_enforcement`
  instead of being dropped silently.

## Tests

- **Unprivileged tests** (always run): pidfd wait outcomes (`true`,
  `exit 3`), `SIGTERM` through the pidfd, escalation from TERM to KILL,
  killing a grandchild through the group, `/proc` identity of the process
  itself and of PID 1, identity mismatch (PID reuse), exited identities,
  profile mapping, building the Landlock ruleset without enforcing it, and
  escape attempts against `CapDir`.
- **cgroup tests** are `#[ignore = "needs delegated cgroup v2"]`. Run them
  with `HARW_TEST_CGROUP_ROOT=/sys/fs/cgroup/<delegated>`, for example
  inside `systemd-run --user -p Delegate=yes --scope`:

```sh
cargo test -p harw-job-linux --all-features
HARW_TEST_CGROUP_ROOT=... cargo test -p harw-job-linux --all-features -- --ignored
```
