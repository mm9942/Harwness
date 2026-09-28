# harw-killer

A Linux process terminator built around **pidfd identity**: send SIGKILL
immediately, wait for exit, and send a second SIGKILL to any survivors on the
same pidfd. There is no SIGTERM phase — this double-KILL behavior is
intentional, not a bug.

Both signal attempts use the same kernel `pidfd`, opened once at selection
time. Comparing the process start time before and after opening the pidfd
prevents accidentally signalling a reused PID; a pidfd that already reports
exit is never selected.

## Entry points

`harw-killer` reaches users and agents through three paths:

1. **`harw kill …`** — the `harw` CLI forwards its `kill` subcommand
   arguments unchanged to `harw_killer::run_cli` (see
   `harw-cli/src/main.rs`). This is the interactive/manual path with the
   full CLI surface below, including the sudo helper for foreign-owned
   targets.
2. **The agent tool `process.kill`** (crate `harw-tool-process`, see
   `harw-tool-process/src/provider.rs`) — used by the agent runtime through
   `harw_killer::api::kill_own`. In the approval modes `ask` and `auto`
   this path is always approval-gated: it is listed in `ALWAYS_ASK_TOOLS`
   (`harw-registry-defaults`) and is never auto-approved, even with a
   matching allow rule. Under `FullAccess` nothing asks, including this
   tool (the user chose "no confirmations at all"). It
   **never invokes sudo**: only processes owned by the current effective UID
   can be killed. A process owned by another user is reported as an error
   result (`"fremder Prozess: sudo nicht erlaubt"`), never signalled, even
   when the caller runs as root — root only kills targets with effective UID
   0 through this API. The companion read-only tool `process.list` previews
   the same selection without sending any signal.
3. **The standalone `killer` binary** (`harw-killer/src/main.rs`, built from
   this crate's `[[bin]]` target) — the same CLI as `harw kill`, usable
   independently of `harw`.

## Requirements

- Linux with a mounted `/proc`. Kernel **5.3+** for ordinary pidfd use.
- Kernel **5.6+**, `sudo`, and suitable ptrace permission for the automatic
  cross-user helper (`harw kill` / `killer` CLI path only — the agent tool
  never uses this). Seccomp, Yama, container capabilities, or sudo rules can
  block this path; that produces an explicit error, never a PID-based
  fallback.
- Python 3, only for the opt-in system tests (see `TESTING.md`).

## CLI flags (`harw kill` / `killer`)

Defined in `src/cli.rs`:

| Flag | Meaning |
| --- | --- |
| `-p, --process NAME...` | Exact executable basenames to match (repeatable, multiple values). Falls back to `comm` when `/proc/PID/exe` is unreadable. |
| `--pid PID...` | Explicit positive PIDs (repeatable, multiple values). |
| `--uid UID` | Restrict the selection to this effective UID. Not a selector on its own — at least one of `--process`/`--pid` is required. |
| `-t, --timeout SECS` | Seconds to wait after the first SIGKILL before retrying survivors (default 5, 0..86400). |
| `--kill-wait SECS` | Seconds to wait after the second SIGKILL before reporting a survivor (default 2, 0..86400). |
| `-n, --dry-run` | Preview the selection without signalling, confirmation, or sudo. |
| `-y, --yes` | Skip the interactive confirmation prompt. |
| `--json` | Machine-readable report on stdout; diagnostics stay on stderr. |
| `--no-sudo` | Never invoke sudo for foreign-owned processes. |
| `--log LEVEL` | Diagnostic verbosity: `error`, `warn`, `info`, `debug`, or `trace`. |

`-p`/`--process` and `--pid` are a union: a process must match at least one
name or PID; `--uid` then filters the whole selection. Multiple matches are
deduplicated. No regex, globs, or positional arguments; a name like `rustc`
does not match `rustc-wrapper`. Without any selector, `killer` reports an
argument error — it never falls back to "select everything".

## Examples

```sh
killer -p rustc rust-analyzer cargo -n     # preview several names
killer --process node python3              # preview, then confirm interactively
killer -p rustc rust-analyzer -y           # kill immediately
killer -p cargo -y -t 3                    # 3 seconds before the second KILL
killer --pid 12345 12346 -y                # multiple explicit PIDs
killer -p cargo --pid 12345 -n             # names OR pids, combined
killer -p node -p python3 -n               # -p may repeat
killer -p cargo --uid 1000 -n --json       # filter all matches by UID
killer -p cargo --no-sudo -y               # never escalate to sudo
```

The same subcommand works through `harw`, e.g. `harw kill -p cargo -y`.

## Output and exit codes

JSON output (`--json`) stays on stdout; structured tracing diagnostics go to
stderr. The report schema carries `schema_version`, `dry_run`, `targets`, and
`results`. Per-target results are one of `killed`, `killed_after_retry`,
`already_exited`, `survived`, or `error`. A fatal error before the report is
built is printed to stderr only — no full JSON report is promised in that
case.

| Code | Meaning |
| --- | --- |
| 0 | Preview succeeded, or every selected process is confirmed terminated |
| 1 | An error occurred, or at least one target was not observed as terminated |
| 2 | No matching process (a report is still printed), or invalid/missing selector (clap usage error) |
| 3 | Interactively aborted |

## Behavior and limits

1. One-shot snapshot of visible processes. PID 1, `killer` itself, and all
   discoverable ancestors (e.g. the invoking shell) are always protected.
2. Selection conditions and start time are compared before/after
   `pidfd_open`; already-exited handles are discarded. Kernel handles then
   stay open for the rest of the run.
3. Preview and confirmation are the default; `--yes` is required in
   pipes/scripts. JSON termination also requires `--yes`; JSON preview needs
   no confirmation.
4. Own targets get KILL first, followed by a shared wait phase (`--timeout`,
   default 5s). Only survivors get a second KILL, followed by `--kill-wait`
   (default 2s). Already-exited targets are not re-signalled; the wait phase
   ends early once all targets are gone.
5. Foreign-owned targets (`harw kill`/`killer` only, never the agent tool)
   are then handled as their own group through sudo, so the timeout applies
   **per group**, not as a single ceiling that also covers the password
   prompt. A caller already running as root needs no helper.
6. The root helper takes ownership of the already-open target handles via
   `pidfd_getfd` from the waiting parent process. It never re-searches for
   targets by name/PID, and it duplicates every handle it receives before
   sending its first signal.

A zombie is never treated as still running. KILL success does not mean a
process returns immediately from an uninterruptible kernel operation;
`survived` therefore only reports that no exit was observed within the wait
window. `killed` reports an observed exit after the first attempt,
`killed_after_retry` after the second. An exit between the two phases cannot
be causally attributed to one signal. The retry is a deliberate second
delivery — it does not make SIGKILL stronger and does not skip
uninterruptible kernel work.

A pidfd pins a process instance, not its current program image: a later
`exec` or credential change on the same instance can still happen. There is
no recursive child-process selection, no process-group kill, and no
automatic retry for newly started processes. `/proc` visibility is limited
by namespace, `hidepid`, and permissions. Windows and macOS are not
implemented.

`sudo` runs the installed `killer` binary as root. Use a trusted binary; do
not add a blanket `NOPASSWD` rule to a file that other users can write.
Blocked `pidfd_getfd` permissions surface as a clearly reported error.

## Project layout

| File | Responsibility |
| --- | --- |
| `src/cli.rs` | Clap parsing and argument validation |
| `src/process.rs` | Procfs, matching, protection, and snapshotting |
| `src/pidfd.rs` | Safe rustix handles, signalling, exit observation |
| `src/engine.rs` | KILL/wait/KILL; a replaceable boundary for tests |
| `src/privilege.rs` | sudo protocol and a replaceable `CommandRunner` |
| `src/types.rs` | Metadata and typed results |
| `src/error.rs` | Handwritten error domain and source chains |
| `src/typestate/` | `Plan<Selected>` → `Plan<Approved>` |
| `src/api.rs` | Programmatic interface for agents (`preview`, `kill_own`); never invokes sudo |
| `src/lib.rs` | CLI orchestration (`run_cli`, `HelperInvocation`) shared by `harw kill` and the standalone binary |
| `src/main.rs` | Thin entry point for the `killer` binary |

Details: `DESIGN.md`, `TESTING.md`.

## Build and test

`harw-killer` is a regular workspace member. `make install` in the repository
root builds and installs `killer` together with `harw` (see
[the installation guide](../docs/setup/install.md)). For work on the crate
itself, the usual workspace commands apply:

```sh
cargo build -p harw-killer
cargo test -p harw-killer
```

The unit tests run by default. The Linux system tests in `tests/cli.rs` are
`#[ignore]`d and need Python 3 plus the ability to spawn/signal owned child
processes; opt in explicitly:

```sh
cargo test -p harw-killer -- --ignored
```

See `TESTING.md` for what each layer covers and what remains
environment-dependent (foreign-owner sudo paths, a kernel without pidfd
support, and similar cases that need an isolated test VM).
