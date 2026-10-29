# harw-job-exec

The safe re-exec trampoline of the Harw job runtime
(`docs/planning/20-job-runtime/HARW_LINUX_JOB_RUNTIME_FOUNDATION.md` §9-§12,
§24). It replaces an `unsafe` `CommandExt::pre_exec` hook. The crate has
`#![forbid(unsafe_code)]` and is Linux-only; on other targets the binary
prints an error and exits `125`.

## Why

Cgroup membership, rlimits, `NO_NEW_PRIVS`, the capability drop and the
Landlock domain must be in place before the job runs its first instruction.
The only standard hook between `fork` and `exec` is `pre_exec`, which is
`unsafe`. So the supervisor spawns this small single-threaded binary instead.
It applies every control to itself with ordinary safe code and then replaces
itself with the job through the safe `CommandExt::exec`. All controls carry
over across `execve`.

## Flow

```text
harw-job-exec --plan <plan.json> [--report <report.json>]

 1. open plan (O_NOFOLLOW), require: regular file, owner == euid, mode 0600
 2. read ≤ 64 KiB, delete the plan file, parse ExecPlanV1 (version first)
 3. create report file (O_CREAT|O_EXCL|O_NOFOLLOW, 0600), still unrestricted
 4. cgroup membership         attach: CgroupV2Fs::open(root) → reopen(relative) → attach_self
                              verify: /proc/self/cgroup `0::<path>` == expected_proc_path
 5. setrlimit                 RlimitSet::apply_to_current_process
 6. sandbox                   NO_NEW_PRIVS → capability drop → Landlock restrict_self
 7. write SandboxReport JSON + "\n" into the open report fd
 8. verify failed             → exit 126, no exec (report: resource_limits not enforced)
 9. requirement check         Required and not fully enforced → exit 126, no exec
10. exec program + args       env, cwd and stdio inherited from the parent's Command
```

| Exit | Meaning |
|---|---|
| job's own | the job ran (the trampoline no longer exists) |
| 125 | setup failed: usage, plan file (insecure, too large, bad version, invalid), report file, cgroup join, rlimits, non-Linux |
| 126 | sandbox refused: applying the policy failed, the cgroup membership to verify does not hold, or the report does not satisfy `SandboxRequirement::Required` |
| 127 | `execve` of the job program failed |

This follows the `env(1)` convention. A job that itself exits with 125 to 127
cannot be told apart by status alone. The trampoline's own failures also
print `harw-job-exec: <typed message>` on stderr.

## Plan (`ExecPlanV1`)

```json
{
  "version": 1,
  "cgroup": { "root": "/sys/fs/cgroup/…/harw", "relative": "job-<attempt>", "limits": "enforced" },
  "rlimits": { "limits": { "Nofile": { "soft": 1024, "hard": 1024 } } },
  "sandbox": { "filesystem": { … }, "network": "deny", "capabilities": "drop_all",
               "no_new_privs": true, "landlock": "best_effort" },
  "sandbox_requirement": "best_effort",
  "program": "/usr/bin/cargo",
  "args": ["build"]
}
```

- The trampoline reads `version` first. Any version other than `1` is rejected
  before other fields are looked at. Unknown fields are rejected.
- `cgroup` is a `CgroupMembership`, serialized untagged. The attach form
  above (`CgroupJoin`) is unchanged, so older plans still parse. The verify
  form `{ "expected_proc_path": "/…/harw-job-<attempt>", "limits": … }`
  (`CgroupVerify`) is for a supervisor that already placed the process in the
  job cgroup, for example a Bubblewrap child spawned into it. The trampoline
  cannot reach cgroupfs from inside the namespace, so it only reads
  `/proc/self/cgroup`. If the unified-hierarchy line `0::<path>` is missing
  or names another cgroup (compared component-wise), it writes the report
  with `resource_limits` not enforced and exits 126 without running the job.
- `expected_proc_path` is the job cgroup's absolute `CgroupHandle::proc_path`:
  the path the supervisor itself sees, relative to its own cgroup-namespace
  root. `/` and paths with `.` or `..` components are invalid plans (exit
  125). The check proves membership only under two conditions:
  - The trampoline must not run in a fresh cgroup namespace. Inside one, the
    kernel prints paths relative to the namespace root, and `0::/` can then
    mean the job cgroup or the supervisor's own. A Bubblewrap launcher lists
    `--unshare-user --unshare-ipc --unshare-pid --unshare-net --unshare-uts`
    instead of `--unshare-all`, which includes `--unshare-cgroup-try`.
  - The process that becomes the trampoline must itself be attached. Writing
    a pid to `cgroup.procs` does not move descendants that already exist, so
    the launcher is held (for example with bwrap `--block-fd`) until its
    whole process tree is attached, or it is attached before it clones.
    Otherwise the check refuses the job: fail-closed, but the launch then
    fails at random.
- `Required` without a `sandbox` is an invalid plan (exit 125), because
  nothing could ever satisfy it.
- `args` are UTF-8 strings, because the plan is JSON.

## Why files and not inherited descriptors

- `--plan-fd N` would have to adopt a raw fd number (`File::from_raw_fd`).
  That is `unsafe`.
- An environment variable leaks into `/proc/<pid>/environ` of the job unless
  every exec path scrubs it.
- So the plan is a `0600` file in a directory the supervisor owns. The
  trampoline checks the owner and mode, then deletes the file before it does
  anything else.
- The report file is opened before the sandbox is applied and written after
  it. Landlock restricts `open`, not `write` on an open fd, so this works even
  when the report directory is not writable for the job. It is not an atomic
  rename, because a rename after `restrict_self` can be denied. Instead, a
  trailing newline marks the document as complete. `read_report` returns
  `Ok(None)` until the newline is there.

## Parent side

```rust,ignore
let plan = ExecPlanV1::new("/bin/sh")
    .with_args(["-c", "make test"])
    .with_rlimits(rlimits)
    .with_sandbox(SandboxPolicy::from_profile(profile, &workspace), SandboxRequirement::BestEffort);
let mut launch = TrampolineCommand::new(&trampoline_bin, &plan, &private_dir)?;
launch.command_mut().current_dir(&workspace);
let child = launch.spawn()?;          // then pidfd_open, persist identity, …
// later: launch.read_report()? → Option<SandboxReport>, then launch.remove_report()
```

`private_dir` should be mode `0700`. The report name is only reserved, and
the trampoline refuses to write into a file that already exists.

## `resource_limits` in the report

- No cgroup and no rlimits requested: `Enforced`, because no requested limit
  is missing.
- rlimits requested: `Enforced`. A failed `setrlimit` aborts the launch.
- A cgroup was joined or verified: the parent passes the state of the cgroup
  limits in `limits` (from `CgroupHandle::limits_enforcement`). An unknown
  state counts as `NotEnforced`. If the cgroup limits fall short and rlimits
  hold, the result is `Partial`.
- A verification failed: the cgroup limits count as `NotEnforced` whatever
  the parent passed, so the report names this dimension (`Partial` if
  rlimits hold), and the job is refused. A later rlimit or sandbox failure,
  or a failed report write, does not replace this refusal: it came first.

## Merging with a Bubblewrap launcher

`merge_reports(bwrap_predicted, trampoline)` combines the report a Bubblewrap
launcher predicts for its own layer with the trampoline's report from inside
the namespace. Both layers restrict the same process, so `filesystem`,
`network`, `no_new_privs` and `capabilities` take the stronger of the two
states. `resource_limits` comes from the trampoline report only: the launcher
enforces no limits, and the trampoline reports `Enforced` only when the cgroup
limits and the rlimits both hold. No dimension ends up stronger than one of
the two layers reports.

## Tests

- Unit tests cover plan round trip, version rejection, the size cap, unknown
  fields and the validation rules (including the rejected `/`), that
  pre-`Verify` plan JSON still parses and serializes byte for byte, the
  `/proc/self/cgroup` parser and the verify check. They also cover the
  requirement matrix, the `resource_limits` table, the report merge table,
  report-file states, argument parsing, plan-file checks (mode, symlink,
  removal), the refusal-over-report-write precedence and the parent command
  builder.
- `tests/trampoline.rs` runs the real binary. It checks that the exit status
  passes through, that rlimits reach the job, and that Landlock
  `ReadOnlyAnalysis` behaviour is consistent with the report on this kernel.
  It checks that `Required` gives 126 without running the job, that a verified
  cgroup membership runs the job (skipped when the test process sits at a
  cgroup root, `0::/`), that a mismatched one gives 126 with
  `resource_limits` recorded (`Partial` with rlimits), that the mismatch
  outranks a later `setrlimit` failure, and it covers the 127 and 125 paths.

```sh
cargo test -p harw-job-exec
```
