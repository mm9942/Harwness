# harw-job-executor-bwrap

Bubblewrap as an optional, stronger Linux job executor backend (planning
README §16, PL-40).

```text
generic SandboxPolicy (harw-job-linux)
        │
        ├── native safe-Rust Linux enforcement (Landlock)
        │
        └── Bubblewrap executor (this crate)
              namespaces · mount isolation · no host network namespace
```

The crate translates a generic `harw_job_core::JobSpec` and a
`harw_job_linux::SandboxPolicy` into a launch of the **existing**
`harw-sandbox` Bubblewrap backend (`BwrapLauncher`). It adds no new
mechanism: `bwrap` stays an external executable, found only at fixed
root-owned paths (never through `PATH`). There is no C and no `unsafe`.

- The whole implementation is `#[cfg(target_os = "linux")]`. On other
  targets the crate is empty.
- `#![forbid(unsafe_code)]`, like the rest of the workspace.

## Usage

```rust,ignore
use harw_job_executor_bwrap::BwrapExecutor;
use harw_job_linux::{LinuxProcess, SandboxPolicy};

let executor = BwrapExecutor::discover()?;          // Unsupported if no bwrap
let policy = SandboxPolicy::from_profile(spec.sandbox_profile, &workspace);
let plan = executor.plan(&spec, &policy, &workspace)?;
tracing::info!(args = ?plan.args(), report = ?plan.report(), "bwrap plan");
let mut command = plan.into_command();              // std::process::Command
let (process, stdio) = LinuxProcess::spawn(&mut command)?;
```

`into_command` clears the outer environment and sets nothing else. The
caller chooses stdio and the job boundary: `LinuxProcess` (pidfd),
`LinuxJobGroup` (process group) or `LinuxJobGroup::spawn_in_cgroup`
(cgroup v2 limits around the `bwrap` process).

## Mapping

| Policy | Bubblewrap plan | Predicted report |
|---|---|---|
| workspace in `read_write` | `--bind <ws> <ws>` | filesystem `Partial` (mounts enforced; read and execute cannot be separated) |
| workspace only in `read_only`/`exec` | `--ro-bind <ws> <ws>` | filesystem `Partial` (mounts enforced; read and execute cannot be separated) |
| `/usr`, `/bin`, `/lib`, `/lib64` | always `--ro-bind` by `harw-sandbox` | — |
| other read/exec paths (`/etc`, `/sbin`, `/lib32`, …) | `--ro-bind-try` | — |
| `/proc`, `/dev`, `/tmp` | bwrap's own fresh procfs, minimal `/dev`, empty tmpfs | — |
| `/sys`, `/run`, ancestors of the workspace | **withheld** (not bound), see `withheld_paths()` | — |
| read-write path outside workspace/`/tmp`/`/dev` | refused: `Unsupported(Filesystem)` | — |
| workspace is (or resolves to) the host root `/` | refused: `Unsupported(Filesystem)` (the whole host would be bound read-write) | — |
| `NetworkPolicy::Deny` | `NetworkMode::None` (own empty netns) | network `Enforced` |
| `NetworkPolicy::Allow` | `NetworkMode::ProxyOnly` if a relay is set, else `Unsupported(Network)` | network `Enforced` |
| `NetworkPolicy::ConnectTcpPorts` | `NetworkMode::ProxyOnly` if a relay is set, else `Unsupported(Network)` | network `Partial` |
| `CapabilityPolicy::DropAll` / empty `Keep` | bwrap drops every capability | capabilities `Enforced` |
| non-empty `CapabilityPolicy::Keep` | refused: `Unsupported(Capabilities)` | — |
| `no_new_privs` | bwrap always sets `PR_SET_NO_NEW_PRIVS` | no_new_privs `Enforced` |
| `ResourceRequest` | not handled by bwrap | resource_limits `NotEnforced` |

Every plan contains `--die-with-parent --new-session --unshare-all
--unshare-net --clearenv` (the trampoline composition lists the namespaces
explicitly instead, see below). The job's environment (`spec.env`) is added after
the `harw-sandbox` baseline (`HOME`, `PATH`, `USER`/`LOGNAME`), and the
working directory is `<workspace>/<spec.working_dir>`.

### Why the network is restricted this way

The Bubblewrap backend only knows two modes: **no network** or
**proxy-only egress** through `harw-netns-relay` and the host egress proxy
socket (`BwrapExecutor::with_proxy_relay`). It never shares the host network
namespace. A port allowlist (`NetworkRestricted`) cannot be expressed as-is:
with a relay, direct egress is impossible and the proxy's host policy applies
instead, so network is reported `Partial`. Without a relay the request is
refused rather than silently widened or narrowed.

### Honest caveats

- A mount namespace separates *invisible*, *read-only* and *read-write*. It
  does not separate *read* from *execute*: read-only paths are executable in
  the sandbox, whereas Landlock would forbid that.
- `/run` is withheld because `connect(2)` on a Unix socket ignores read-only
  mounts. A read-only bind would still expose host daemon sockets.
- The report is a **prediction** for the `bwrap` layer only. The caller
  merges the cgroup/rlimit results and decides whether the merged report
  meets `spec.sandbox` (`SandboxRequirement::Required`).

## Composition with the trampoline (PL-97 E1)

`BwrapExecutor::plan_with_trampoline(spec, policy, workspace, &TrampolineMount
{ exec_path, attempt_dir })` runs the `harw-job-exec` trampoline as the
sandboxed command, which adds rlimits and a Landlock layer (read vs. exec)
inside the namespace:

- `--ro-bind <exec_path> /run/harw-job/exec` and
  `--bind <attempt_dir> /run/harw-job/attempt`, after the workspace and
  relay binds;
- command `/run/harw-job/exec --plan /run/harw-job/attempt/inner.json
  --report /run/harw-job/attempt/report.json`, behind the relay prefix in
  proxy-only mode;
- `--unshare-user --unshare-ipc --unshare-pid --unshare-net --unshare-uts`
  instead of `--unshare-all`, so `/proc/self/cgroup` inside shows the job
  cgroup's real path for the trampoline's cgroup verification.

The returned `BwrapTrampolinePlan` holds the `bwrap` plan, the namespace
view as a `harw_job_linux::PathMap` (module docs of `executor.rs`), the
inner policy `policy.mapped(&path_map).hard_requirement()` (network left to the namespace) and
the policy paths without a rule inside (`unmapped_paths`, usually the
withheld ones). It refuses a plan whose read-only binds would keep write or
execute through an ancestor's Landlock rule (e.g. a read-only workspace
beneath `/tmp`), and a workspace that contains the trampoline binary or the
attempt directory. The job runtime's `LinuxSandboxBackend::BwrapTrampoline`
writes the inner plan, starts `bwrap` through an outer trampoline that joins
the job cgroup first, and merges this crate's predicted report with the
inner report (`harw_job_exec::merge_reports`).

The inner policy requires fully enforced Landlock filesystem rules even for
`None`/`BestEffort` jobs. The writable report directory must become inaccessible
before the job runs: a host probe alone cannot establish that. Unsupported or
partial actual enforcement aborts before `exec`. Backend selection therefore
requires ABI 5 or later for the composition; `auto` can still select plain
Bubblewrap on older kernels. An explicit unavailable composition selects no
backend (not a weaker sandbox). Existing runtime semantics still apply:
`Required` refuses, while `None`/`BestEffort` can run with backend `None`.
Such unsandboxed jobs do not use a composed trampoline report.

## Tests

- Unit tests (`src/executor/tests.rs`): pure argv/report mapping for all four
  `SandboxProfileName`s via `SandboxPolicy::from_profile`, network refusal
  without a relay, proxy-only with a relay, report prediction, env/working-dir
  splicing, and the refused filesystem/capability cases; for the trampoline
  composition the binds, the inner command, the namespace list, the relay
  prefix order, path-map completeness against the argv, the inner network
  and the shadow/overlap refusals. No process is started.
- Integration test (`tests/bwrap_run.rs`): runs `echo ok` (succeeds),
  `sh -c 'touch /etc/x'` (fails), and writes to the workspace (succeeds
  read-write, fails read-only) through the real `bwrap`. The test skips
  itself when no trusted `bwrap` is installed or when the host cannot create
  the sandbox.

```sh
cargo test -p harw-job-executor-bwrap
```
