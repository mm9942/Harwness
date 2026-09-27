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
 4. join job cgroup           CgroupV2Fs::open(root) → reopen(relative) → attach_self
 5. setrlimit                 RlimitSet::apply_to_current_process
 6. sandbox                   NO_NEW_PRIVS → capability drop → Landlock restrict_self
 7. write SandboxReport JSON + "\n" into the open report fd
 8. requirement check         Required and not fully enforced → exit 126, no exec
 9. exec program + args       env, cwd and stdio inherited from the parent's Command
```

| Exit | Meaning |
|---|---|
| job's own | the job ran (the trampoline no longer exists) |
| 125 | setup failed: usage, plan file (insecure, too large, bad version, invalid), report file, cgroup join, rlimits, non-Linux |
| 126 | sandbox refused: applying the policy failed, or the report does not satisfy `SandboxRequirement::Required` |
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
- A cgroup was joined: the parent passes the state of the cgroup limits in
  `CgroupJoin::limits` (from `CgroupHandle::limits_enforcement`). An unknown
  state counts as `NotEnforced`. If the cgroup limits fall short and rlimits
  hold, the result is `Partial`.

## Tests

- Unit tests cover plan round trip, version rejection, the size cap, unknown
  fields and the validation rules. They also cover the requirement matrix, the
  `resource_limits` table, report-file states, argument parsing, plan-file
  checks (mode, symlink, removal) and the parent command builder.
- `tests/trampoline.rs` runs the real binary. It checks that the exit status
  passes through, that rlimits reach the job, and that Landlock
  `ReadOnlyAnalysis` behaviour is consistent with the report on this kernel.
  It checks that `Required` gives 126 without running the job, and it covers
  the 127 and 125 paths.

```sh
cargo test -p harw-job-exec
```
