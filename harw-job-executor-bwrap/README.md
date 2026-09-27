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
| `NetworkPolicy::Deny` | `NetworkMode::None` (own empty netns) | network `Enforced` |
| `NetworkPolicy::Allow` | `NetworkMode::ProxyOnly` if a relay is set, else `Unsupported(Network)` | network `Enforced` |
| `NetworkPolicy::ConnectTcpPorts` | `NetworkMode::ProxyOnly` if a relay is set, else `Unsupported(Network)` | network `Partial` |
| `CapabilityPolicy::DropAll` / empty `Keep` | bwrap drops every capability | capabilities `Enforced` |
| non-empty `CapabilityPolicy::Keep` | refused: `Unsupported(Capabilities)` | — |
| `no_new_privs` | bwrap always sets `PR_SET_NO_NEW_PRIVS` | no_new_privs `Enforced` |
| `ResourceRequest` | not handled by bwrap | resource_limits `NotEnforced` |

Every plan contains `--die-with-parent --new-session --unshare-all
--unshare-net --clearenv`. The job's environment (`spec.env`) is added after
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

## Future: trampoline inside the sandbox

Running the `harw-job-exec` trampoline as the sandboxed command
(`bwrap … -- harw-job-exec … -- <program>`) would add rlimits and a Landlock
layer (which restores read vs. exec) inside the namespace. This is not wired
up yet.

## Tests

- Unit tests (`src/executor/tests.rs`): pure argv/report mapping for all four
  `SandboxProfileName`s via `SandboxPolicy::from_profile`, network refusal
  without a relay, proxy-only with a relay, report prediction, env/working-dir
  splicing, and the refused filesystem/capability cases. No process is
  started.
- Integration test (`tests/bwrap_run.rs`): runs `echo ok` (succeeds),
  `sh -c 'touch /etc/x'` (fails), and writes to the workspace (succeeds
  read-write, fails read-only) through the real `bwrap`. The test skips
  itself when no trusted `bwrap` is installed or when the host cannot create
  the sandbox.

```sh
cargo test -p harw-job-executor-bwrap
```
