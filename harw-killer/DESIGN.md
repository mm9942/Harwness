# harw-killer — design and implementation

> Status: implemented · Last reviewed: 2026-09-24

Goal: a precise, Linux-only process terminator with clap-based argument
parsing and a deliberate double-SIGKILL termination sequence, usable both as
a standalone binary and as a library integrated into the `harw` agent
harness.

## Decisions

- Linux-only, `/proc` plus kernel pidfds; no shell-based process search.
- Multiple exact executable names via `-p`/`--process` (falls back to
  `comm`); combinable with multiple explicit `--pid` values. No regex.
- PID 1, the calling process itself, and its ancestors are always excluded.
- Open pidfds bind the selection to the process *instance*. Start time is
  checked before/after opening; the pidfd is also checked for an already
  observed exit before the selection is accepted.
- Preview, interactive confirmation or `--yes`, `--dry-run`, `--json`.
- KILL to owned targets, a shared timeout, a second KILL to survivors, and a
  bounded follow-up check. Zombies count as terminated.
- Foreign-owned targets: `sudo` runs an internal helper, which duplicates
  the parent's already-held pidfd via `pidfd_getfd`. No re-search by
  name/PID, no signalling through bare numeric target PIDs.
- If a kernel/ptrace/sudo policy blocks that path, the result is an error,
  never an unsafe fallback. A caller already running as root needs no
  helper.
- No process groups, descendant selection, or persistent tracking of
  newly-started processes in v0.1. A pidfd binds the process instance, not
  its program: a later `exec` remains the same instance.
- SIGKILL as the first signal, plus the retry, are an explicit, deliberate
  choice — there is no SIGTERM phase and no `--no-kill`/`--no-retry` switch.
  The default algorithm always runs both KILL phases while targets remain
  alive; a reused PID never receives a second signal it wasn't selected for.
- The CLI is intentionally generic: `-p`/`--process` takes exact executable
  names, `--pid` takes positive PIDs; both form a union, `--uid` filters
  it. No Rust-specific flags, regex, substrings, or positional arguments.

## Module boundaries

1. `cli.rs` — selection, options, validation.
2. `process.rs` — procfs snapshot, start time, ancestry, selection.
3. `pidfd.rs` — safe rustix boundary; `OwnedFd` and poll.
4. `engine.rs` — KILL, timeout, KILL, and result states.
5. `main.rs` / `lib.rs` — preview, confirmation, privilege escalation,
   output/exit codes.
6. Integration tests with isolated, self-owned child processes.

No setuid binary and no installed privileged service. `sudo` runs ordinary
executable code as root: only a self-built/trusted binary is used, never a
blanket `NOPASSWD` rule on a file other users can write.

## Integration into Harwness

`harw-killer` is a regular workspace crate (`harw-killer`); its termination
semantics are unchanged from the standalone design above.

- `src/lib.rs`: all CLI orchestration (`run_cli`, `HelperInvocation`).
  `main.rs` is only a thin entry point for the `killer` binary.
- `HelperInvocation::Standalone` starts the sudo helper as before, as
  `<exe> --helper …`; `HelperInvocation::Subcommand(["kill"])` starts it as
  `<exe> kill --helper …`, so that `harw kill …` takes the same path.
- `src/api.rs`: a programmatic interface for agents (`preview`,
  `kill_own`). It uses the same selection and the same KILL/wait/KILL
  engine, but **never** starts sudo: foreign-owned targets get an error
  result instead.
- `harw kill` (in `harw-cli`) forwards all arguments unchanged to
  `run_cli`.
- The agent tools `process.list` (preview only) and `process.kill`
  (approval-gated) live in the `harw-tool-process` crate and call into
  `src/api.rs`. See `harw-tool-process/src/provider.rs` for the exact
  security contract: `process.kill` requires `Permission::ExecuteProcess`,
  is never in `AUTO_APPROVED_TOOLS`, and is always in `ALWAYS_ASK_TOOLS`
  (`harw-registry-defaults`) — it asks for approval every time, even under
  `FullAccess` and even with a matching allow rule.

## Design principles

These principles, distilled from the crate's internal style guide, apply
project-wide beyond this crate's own scope:

- **Module split by responsibility.** Error domain, data types, CLI
  parsing, low-level kernel access, and orchestration logic each live in
  their own module (see the layout table in `README.md`), rather than one
  large file.
- **A handwritten crate error type.** `Error` in `src/error.rs` is a plain
  enum with named variants and preserved source chains (no `anyhow`,
  `thiserror`, `failure`, or general-purpose logging crate in the
  production dependency graph). Context is attached at I/O boundaries; no
  production code uses `unwrap`/`expect`.
- **Typestate for confirmation.** `Plan<Selected>` becomes `Plan<Approved>`
  only by consuming `self` (`src/typestate/`), so a plan cannot be acted on
  before it has been explicitly approved, and the type system — not a
  runtime flag — enforces the ordering.
- **`#![forbid(unsafe_code)]`.** Low-level operations (`pidfd_open`,
  `pidfd_getfd`, signalling, polling) are delegated to the safe `rustix`
  API rather than hand-written `unsafe` blocks.
- **No panics in production paths.** Fallible operations return `Result`;
  optional procfs metadata that is missing gets a documented fallback and
  diagnostic instead of a panic, while required values fail explicitly.
- **Structured tracing, not ad-hoc printing.** Operational diagnostics go
  through `tracing` with a single installed subscriber, `--log LEVEL`, and
  structured fields; CLI report data (tables, JSON) is kept separate from
  diagnostic output.
- **Synchronous design.** No threads or async tasks are spawned; the CLI
  and engine block the calling thread deliberately, which keeps the
  KILL/wait/KILL sequence easy to reason about and test.
