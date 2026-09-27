# harw-job-darwin

Darwin/macOS-native job mechanics for Harwness: process supervision, exit
watching, group termination, rlimit plans and `sandbox-exec` wrapping. It
reports exactly what it enforces and marks the rest as not enforced. It does
not emulate Linux primitives (Eco-Doc §15, §37, §62).

- The whole implementation is `#[cfg(target_os = "macos")]` (module `macos`).
- The pure parts compile and are tested on every target, so Linux CI covers
  them: the SBPL generator, the report mapping, rlimit plans, recovery
  identity and errors.
- `#![forbid(unsafe_code)]`, like the rest of the workspace.

## Enforced on Darwin vs Linux

| Concern | Linux (`harw-job-linux`) | Darwin (this crate) | Darwin report state |
|---|---|---|---|
| Live process authority | pidfd | own **unreaped** child (PID pinned until reaped; signals refused after reap) | — |
| Exit wait with timeout | `poll` on pidfd | watcher thread in `waitid(WEXITED\|WNOWAIT)` + channel `recv_timeout` (no polling) | — |
| Job boundary | cgroup v2 (`cgroup.kill`) | process group (`killpg`); `setsid` escapes it | — |
| Group termination | cgroup kill | TERM to group → grace → KILL to group + leader, before reaping | — |
| Recovery identity | `/proc/<pid>/stat` start time | **none**: `sysctl KERN_PROC` needs `unsafe`, so the identity is `Unverifiable` and is **never signalled** | — |
| Filesystem restriction | Landlock | `sandbox-exec` + generated SBPL (deprecated, undocumented) | `Partial` with `sandbox-exec`, else `NotEnforced` |
| Network restriction | Landlock net / netns | SBPL `(deny network*)`; no allowlist for `NetworkRestricted` (it fails closed to no network) | `Partial` with `sandbox-exec`, else `NotEnforced` |
| `no_new_privs` | `PR_SET_NO_NEW_PRIVS` | no such concept | `Unsupported` |
| Capabilities | capability sets | no such concept | `Unsupported` |
| Memory ceiling | cgroup `memory.max` | not enforced: XNU ignores `RLIMIT_AS`/`RLIMIT_DATA` for `mmap` | `NotEnforced` (`Unsupported` in the plan) |
| Process count | cgroup `pids.max` | `RLIMIT_NPROC` counts per **user**, not per job | `Partial` at best, via a trampoline |
| rlimits in the job | `prlimit` / pre-exec | `apply_rlimits_to_current_process` for a future exec trampoline (`setrlimit` works only on the calling process; `pre_exec` would need `unsafe`) | `NotEnforced` until a trampoline runs |
| Wall timeout | supervisor | supervisor (`TerminationPolicy`) | — |

A `SandboxRequirement::Required` job never passes on Darwin. This is
intended: the report does not overclaim.

## Why not kqueue?

`kqueue` with `EVFILT_PROC`/`NOTE_EXIT` is the native Darwin tool for this.
In rustix 1.1, `rustix::event::kqueue::kevent` is an `unsafe fn`, and the
workspace sets `unsafe_code = "forbid"`, so this crate can't call it. The
watcher thread gets the same result without busy polling.

One Darwin quirk: `waitid` can also return for a *stopped* child (golang
issue 19314). While a child is stopped, the watcher re-checks at most every
100 ms. Exit tokens are only hints; reaping decides the outcome.

## `sandbox-exec`

`sbpl_profile_for(profile, workspace_root)` generates a profile:

- `(deny default)`
- process basics and `mach-lookup`
- read access to system paths
- write access to `/dev/null` and a few other devices
- read access to the workspace, plus write access for every profile except
  `ReadOnlyAnalysis`
- network allowed for `WorkspaceBuild` and `ReadOnlyAnalysis`, denied
  otherwise

`wrap_with_sandbox_exec` and `DarwinSandbox::wrap` build the command
`/usr/bin/sandbox-exec -p <profile> <program> <args…>`.
`DarwinSandbox::wrap` also canonicalizes the workspace root, because Seatbelt
matches resolved paths such as `/private/var/...`.

`$TMPDIR` is not writable inside the sandbox. Point it into the workspace.
