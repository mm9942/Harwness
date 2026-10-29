# Mediated Process Execution and Sandbox Modules

> Status: implemented · Last reviewed: 2026-10-29

## Goal

`/bin/sh`, Cargo and tmux are not ordinary model tools. They are only ever
used by dedicated execution workers whose process right the parent issues
case by case, with a bounded mandate. A planner, explorer, UIA agent or
ordinary coding worker never gets `shell.exec` directly.

The separation is deliberately two-stage:

1. A **delegation capability** determines whether a parent may request a
   given execution worker at all.
2. An **`ExecutionPermit`** determines which single process mandate that
   child may carry out.

A role alone, or a parent's claim in model text, is never sufficient.

## Worker classes

| Definition | Environment | Permitted purpose | Direct host access |
| --- | --- | --- | --- |
| `sandbox-shell-worker` | strict bubblewrap sandbox | bounded shell mandates inside the project | never |
| `cargo-worker` | bubblewrap with the toolchain module explicitly enabled | cargo/rustfmt/rustc inside the project | never |
| `tmux-inspector-worker` | bubblewrap with a single, validated tmux user socket | `list-sessions`, `capture-pane`, explicitly allowed tmux queries | never |
| `host-process-worker` | separate, local, only after a confirmed host-process mode | unavoidable local host processes | only for the approved mandate |
| `uia-shell-worker` | role `uia-worker`; `shell.exec` plus read-only `fs.*`; always `SandboxProfile::Host`; permit still required | host-side shell work delegated by the UIA | only for the approved mandate |

`host-process-worker` is not a general escape hatch from the sandbox. It is
never registered at remote/gateway/MCP entry points or by a child worker.
Its capability can only be handed to the root by the local UIA, after clear
user confirmation.

## `ExecutionPermit`

Before a process runs, the runtime checks a permit the model cannot
construct:

```text
ExecutionPermit {
  parent_session,
  worker_definition,
  command_digest,
  sandbox_profile,
  approved_modules,
  workspace_and_scope,
  expires_at,
  remaining_uses = 1,
  approval_proof,
}
```

The permit is bound to the concrete, canonical command request. An
execution worker may not start a second or modified command under the same
permit. Expiry, reuse, a different worker definition, a different
workspace, or a broader sandbox profile are all rejected fail-closed.

A parent can only create a permit if its own sandbox and authority already
contain every right requested — the child's sandbox plan remains a true
narrowing of the parent's.

## Cargo module

Binds only the validated toolchain plus the required cargo/rustup
directories, at fixed sandbox targets. It does not broadly widen `$HOME`,
`/tmp`, or network access. Fetching additionally requires the
separately-checked proxy network mode and a bounded network scope.

## tmux module

Accepts only a single socket of the current user, validated during local
UIA approval. It does not bind all of `/tmp` or `$HOME`. The permit carries
socket identity, allowed tmux operations, and a short lifetime. Write-mode
tmux actions are a separate, consent-gated module mode; the default is
inspection.

## Host mode

Host mode is **not** a CLI flag or a startup option. A process cannot
preselect it at program start or via a repeatable command line.

**Direct request path (implemented):** the `sandbox-lease` model tool
(`harw-ops/src/sandbox_lease.rs`, a `model_tool` with no extra approval
step of its own — the confirmation dialog itself *is* the approval) offers
actions `request`/`status` with a `reason` argument. `revoke` exists only
for the user (see below); a model call with `action = "revoke"` is
rejected. A `request`
triggers a `HostPermitPrompt` in the local UI
(`worker_definition = "sandbox-lease"`, `command = reason`, preselecting
the session-lease variant) and waits up to 300 seconds for a decision:

- **`SessionLease`** (`mark_global_approval`, no expiry): from confirmation
  onward, every `shell.exec` call in this harw session **and all its child
  agents** runs on the host — no bubblewrap, with the user environment
  inherited from harw (including `PATH`, `HOME`, `CARGO_HOME`/`RUSTUP_HOME`),
  `cwd` at the workspace root, still under the existing `prlimit` limits;
  tool output carries `"executed_on": "host"` to distinguish it. The lease
  stays active until **the user** ends it (user decision 2026-09-24):
  `Ctrl+H` in the TUI (`ChatApp::end_host_mode`) or the typed
  `/sandbox-lease revoke` slash command end it immediately and process-wide
  (see below); the next call then runs in the strict sandbox again. There
  is no time-based expiry — neither the registry grant nor the
  `SessionLease` permits in the `ProcessPermitLedger` (issued with
  `ttl = None`) expire — and the model cannot end the lease.
- **`SingleExecution`** (`mark_global_single_use`): only the immediately
  next `shell.exec` call, from **any session of this process**, runs on the
  host; the strict default profile applies automatically afterward.
- Rejection or timeout return an error to the model instead of a grant; no
  permission is created.

The tool itself activates nothing — it only sends the structured request to
the same unchangeable local confirmation view that indirect classification
(below) also triggers; the grant remains exclusively a deliberate "yes" in
the UI. `/sandbox-lease status`/`revoke` likewise activate nothing; they
only read status or end an existing grant.

**Only the user can revoke.** The `sandbox-lease` op serves both the
command surface and the model-tool surface with one body. `revoke` is
accepted only when the `OpContext` carries the `HostLeaseUserControl`
marker (`harw-tool-shell`), which `RuntimeServices::service_map` places
exclusively on `ServiceSurface::Slash` — the service map of a user-typed
slash command. Model-tool, web and job contexts never carry it, and since
it is a service rather than an argument, no tool argument can forge it.
Without it, `revoke` fails with "only the user can end host mode (Ctrl+H
or /sandbox-lease revoke)" and changes nothing.

The lease wiring reaches child registries too (`build_registry`,
`harw-runtime/src/children.rs`), not only the root — `uia-shell-worker` and
`host-process-worker` can execute on the host under an active lease as
children as well. The TUI polls `host_permit_prompts` during a running turn
too, so the dialog is not limited to appearing only between turns.

**Grant scope is process-wide, not just profile-wide.** A grant is not
scoped to the single session that requested it: `shell.exec` calls from a
child session (a different session ID, e.g. `uia-shell-worker`) are covered
too, for the whole harw process (root session and all child agents), until
the user ends it with `Ctrl+H` or `/sandbox-lease revoke` (or the process
exits — the state is in-memory only). `HostPermitSessionRegistry`
(`harw-sandbox/src/host_permit_session.rs`) carries this as a *global*
approval state (`mark_global_approval`/`has_global_approval`/
`mark_global_single_use`/`has_global_single_use`/`revoke_global_approval`),
consulted by `is_session_approved`, `take_single_use` and
`has_single_use` in addition to any per-session grant (a global
single-use grant is consumed atomically only after the per-session one). `/sandbox-lease request` sets only the global
grant; `revoke` clears both.

The wiring reaches every constructed `ShellToolProvider`, not only ones
built with `SandboxProfile::Host` — the root session runs with
`SandboxProfile::Strict`, so without this the registry/ledger/question
channel would never reach it and a lease granted at the root could never
take effect there. For Strict/Cargo/Tmux profiles, the sandbox remains the
actual boundary regardless: `determine_effective_host`
(`harw-tool-shell/src/exec.rs`) checks only the registry grant for a
non-host profile, never the permit ledger itself.

### Indirect (natural-language) requests

A user may express the need naturally and indirectly — "show me my running
tmux session," or "this has to run on my real system." The UIA or root
orchestrator may classify such a statement as a **host-mode candidate**.
That classification has exactly one effect: sending a structured request to
the local UI. It is not itself a permission and switches nothing on.

The local UI offers two selectable grant variants for any such request. The
agent may **suggest** a fitting variant but never select or activate one.
On indirect or ambiguous user intent, it may only request variant 1.
Variant 2 may only be suggested when the user's statement unambiguously
calls for an extended host work phase — it is never the agent's first or
automatic recommendation:

1. **One-time, for this mandate.** An `ExecutionPermit` bound to a
   canonical mandate/command hash, consumed after one use.
2. **Bounded host work phase.** A session-bound `HostModeLease` allows
   several individually approval- and scope-checked `host-process-worker`
   mandates until the local session ends or the user explicitly
   re-enables isolation. The lease does not transfer to other sessions,
   parents, workers, workspaces, or remote entry points.

Even variant 2 is not a global "sandbox off": it only permits the
already-confirmed class of local host execution workers. Filesystem,
network, secrets and process boundaries are still checked per mandate; a
`HostModeLease` cannot grant access beyond the local user's own rights and
cannot create a new delegation capability. The UI shows `HOST MODE ACTIVE`
continuously and unmissably during the phase, along with the remaining
scope and session binding.

Only a deliberate "yes" in the UI creates a one-time, session-bound host
`ExecutionPermit` or `HostModeLease`. Cancellation, timeout, disconnect,
mandate completion (variant 1), and session end all discard the request or
grant. The user can end variant 2 explicitly via the UI at any time; the
strict default sandbox profile applies immediately afterward.

The resulting security chain:

```text
natural user intent
→ agent recognizes a possible host need and suggests a grant variant
→ unchangeable local confirmation view
→ user selects one-time or host work phase and confirms explicitly
→ bounded permit or lease
→ specialized host worker
```

The agent may surface uncertainty and ask for confirmation, but can never
turn a misclassification into host execution itself.


## Root commands (`host.sudo_exec`)

sudo **is possible** in harw — through one dedicated tool, never through
`shell.exec`. The short version the model should give the user: *"sudo
works: I request the command, you confirm the exact command in the approval
window and enter your password there if sudo asks for one."*

- **Tool.** `host.sudo_exec {argv, reason}` (`harw-tool-shell/src/sudo.rs`)
  runs exactly one `argv` via a pinned `/usr/bin/sudo` (`-k`, `--`), never
  through a shell; `argv[0]` must not itself be `sudo`/`doas`/`pkexec`/`su`.
  It is in `ALWAYS_ASK_TOOLS`, so in `ask` and `auto` the normal approval
  also asks. Under `full` neither the normal approval nor the sudo window
  asks; the window only appears to take the password when `sudo` needs one.
- **Who has it.** Only `uia-shell-worker` and `host-process-worker`
  (`SUDO_ROLES`/`sudo_tools_for_role` in
  `harw-registry-defaults/src/profile.rs`, admitted in their agent TOMLs).
  Every other role — the UIA itself, `uia-worker`, `executor`, the
  orchestrators — delegates or hands the step back:
  - the UIA delegates with `transfer_to_uia-shell-worker` (exact command +
    reason); the handoff tool's description says so
    (`harw-core/src/turn_loop.rs::handoff_role_hint`), as do the UIA rules
    (`knowledge/roles/uia.md`, section "sudo / Root-Befehle") and the
    `shell` mode prompt (`harw-core/src/mode.rs::SHELL_PROMPT`);
  - workers and orchestrators return the step with the exact `argv` and a
    reason as a blocker to their parent, up to the UIA
    (`knowledge/roles/{worker,uia-worker,root-orchestrator,sub-orchestrator}.md`).
- **TUI only.** The provider needs the sudo prompt channel, which
  `harw-runtime/src/assembly.rs` creates only for `EntryKind::Tui`;
  `RuntimeChildRegistryFactory::with_sudo_exec` mounts the tool only then.
  In `serve`, `telegram`, one-shot, jobs and every other entry the tool is
  absent (fail-closed); the model should then give the user the exact
  command to run themselves. With a TUI attached it must not do that.
- **Password flow.** Before running, `host.sudo_exec` probes
  `sudo -n -k true`. Passwordless sudo: the window
  (`harw-tui/src/sudo_dialog.rs`) only shows the exact argv and reason with
  approve/deny, and the command runs as `sudo -n -k -- argv…`. Otherwise
  the user types the password into the masked field of that window; it is
  written to sudo's stdin only after sudo prints a one-time prompt marker
  (`-S -p <marker>`), and never reaches the model, the transcript or the
  audit record. The model must never ask for a password in chat, put one
  into a command, or use `sudo -S`/`echo … | sudo` itself.
- **Refusals stay actionable.** `shell.exec` rejects `sudo`/`doas`/`pkexec`/
  `su`/`run0` in command position (`escalation_program`) with
  `shell_escalation_message`, which states that sudo works via
  `host.sudo_exec`, and tells a role without the tool to delegate to
  `uia-shell-worker` or to hand the exact `argv` back to its parent/the UIA.
  The auto-mode prefilter (`harw-runtime/src/auto_classifier.rs`,
  category `privilege-escalation`) uses the same detection and wording.
- **Command position only.** Detection tokenizes the command shell-style:
  quotes, `;`, `&&`, `||`, `|`, `(`, `$(…)`, backticks, wrappers such as
  `env`/`exec`/`nohup`/`time`/`xargs`/`timeout`, and `sh -c '…'`. A `sudo`
  inside a quoted argument (`rg 'sudo_exec|sudo -' …`,
  `git commit -m 'use sudo'`) or as part of another word (`sudo_exec`,
  `/etc/sudoers`) is not a hit.

## User consent

Activating a module shows, before granting, at minimum:

- the worker type;
- the effective sandbox (strict, cargo, or tmux socket);
- the exact resource, e.g. toolchain paths or socket;
- the permitted operation and its lifetime;
- whether a host process mode is being requested.

Every grant is persisted as a session event. It does not transfer to other
sessions, siblings, or remote entry points, and can never be widened by a
child.

## Composition: Bubblewrap ∘ trampoline ∘ cgroup (job runtime)

The job runtime (`harw-job-runtime`) has four Linux sandbox backends.
Bubblewrap alone gives namespaces and mounts but cannot separate *read*
from *execute*, enforces no rlimits, and its child runs a few
microseconds outside the job cgroup between spawn and attach. The Landlock
trampoline (`harw-job-exec`) alone gives Landlock, `NO_NEW_PRIVS`, the
capability drop and rlimits, but no namespaces. The
`LinuxSandboxBackend::BwrapTrampoline` backend (PL-97 E1) composes both:
namespaces outside, Landlock inside.

```text
LinuxExecutor (job cgroup R/harw-job-<attempt> created with its limits)
 └─ harw-job-exec --plan <attempt>/outer.json        outer stage, process-group leader
      cgroup attach_self (first) → rlimits → exec bwrap   (no Landlock here)
    └─ bwrap --die-with-parent --new-session
             --unshare-user --unshare-ipc --unshare-pid --unshare-net --unshare-uts
             … --ro-bind <harw-job-exec> /run/harw-job/exec
               --bind <attempt> /run/harw-job/attempt
             -- [relay prefix] /run/harw-job/exec --plan /run/harw-job/attempt/inner.json
                                                 --report /run/harw-job/attempt/report.json
        └─ inner stage: verify /proc/self/cgroup → rlimits → NO_NEW_PRIVS →
           capability drop → Landlock (namespace view) → exec <program>
```

### Cgroup membership without a race

The outer trampoline joins the job cgroup **before** it `exec`s `bwrap`,
so `bwrap`, its init and every descendant are born inside the cgroup.
There is no spawn-to-attach window and no need to hold `bwrap` with
`--block-fd`. The supervisor's own attach after spawn
(`LinuxJobGroup::spawn_in_cgroup`) is kept and is a no-op for the
already joined process. The inner stage only verifies membership
(`CgroupJoin::Verify` with `CgroupHandle::proc_path`) and refuses with
exit `126` on a mismatch. That check needs the caller's cgroup namespace,
so this backend lists the namespaces explicitly
(`BwrapLauncher::with_shared_cgroup_namespace`) instead of
`--unshare-all`, which in bubblewrap 0.9 includes `--unshare-cgroup-try`.
Without a configured cgroup root, neither stage has a cgroup step.

### Landlock against the namespace view

The inner stage sees bind destinations, not host paths.
`BwrapExecutor::plan_with_trampoline` therefore describes its own mounts
as a `harw_job_linux::PathMap` built from the same classification as the
argv: the workspace (read-write or read-only), the always-bound system
trees, the NSS files and other read-only policy paths (read-only), the
relay binary and proxy socket, the trampoline binary (read-only) and the
attempt directory (read-write), plus bwrap's own `/proc` (read-only),
`/dev` and `/tmp` (read-write). The inner policy is `policy.mapped(&map)`:
read-only binds lose write and, outside the system exec trees, execute,
which is the distinction the mounts cannot make. Policy paths without an
entry (`/sys`, `/run`, workspace ancestors — the withheld paths) get no
rule. A plan in which a read-only bind lies beneath a path that keeps write
or execute (`PathMap::shadowed_read_only_binds`) is refused. One example is
a read-only workspace beneath `/tmp`, because every profile keeps `/tmp`
read-write.

The network dimension stays with the namespace: an own empty network
namespace (`Deny`) or proxy-only egress (`Allow`) leave nothing to
Landlock, so the inner policy asks for no network rules. A port allowlist
behind the relay becomes TCP `connect` to the relay port only.

### Report and requirement

The attempt report is `harw_job_exec::merge_reports(bwrap predicted,
inner report)`: per dimension the stronger layer, resource limits from the
trampoline only, capped by the cgroup domain. It is known after the job
ran, as with the Landlock trampoline. A `Required` job is checked against
the namespace layer's network and the resource domain before anything
runs; the inner stage checks its own dimensions and exits `126` instead of
running the job.

### Files and trust

Each attempt gets a fresh `0700` directory below the backend's `plan_dir`
(`harw-job-<attempt>.<pid>.<seq>/`) with both `0600` plans and the report.
Both trampolines delete their plan after reading it, and the directory is
removed after the job ended or a failed start. The plan is refused when the
workspace contains the trampoline binary or the attempt directory, or
overlaps `/run/harw-job`. Otherwise a job could replace the binary that the
next attempt runs on the host, or rewrite its own plan or report. Both
Bubblewrap backends also refuse a workspace that is (or resolves to) the
host root `/`, which would bind the whole host read-write.

The attempt directory is bound read-write so the inner trampoline can
create its report (`O_EXCL`, written through the open descriptor after it
restricted itself), and only the inner Landlock layer keeps the job out of
it. The inner policy never grants write access at, beneath or above
`/run/harw-job`; `plan_with_trampoline` refuses a plan that would. Without
a usable Landlock ABI the job could rewrite its report to claim a stronger
state than any layer enforced (`merge_reports` keeps the stronger layer
per dimension), or replace the report with a FIFO that blocks the reader.
So the backend never runs without one: `select_backend` does not choose it
(`auto` falls through to Bubblewrap alone, an explicit request ends in
none), and a `BwrapTrampoline` backend configured explicitly refuses every
sandboxed job with `RuntimeError::Unsupported` before an attempt directory
exists. As defense in depth, the runtime reads the report only when it is
a regular file (no symlink, FIFO or directory); by then the pid namespace
has ended, so no job process can swap it. Landlock ABI 1 and 2 do not
mediate `truncate(2)`: there the job can at most empty the report, which
the runtime reads as "no report" (unknown), never as a stronger state.

- Trust boundary: the outer stage runs unsandboxed as the harw user, like
  `bwrap` itself; it applies only controls that narrow.
- Failure mode: fail-closed. A plan the backend cannot express, a failed
  cgroup verification or an unmet requirement means the job does not run.
- Persistence: plans and reports exist only in the private attempt
  directory while the attempt runs.

### Backend selection

`harw_job_runtime::select_backend(&HostReport, &BackendPreferences)` is the
one resolver. With `auto` it chooses Bubblewrap ∘ trampoline (which also
needs a usable Landlock ABI, see above), else Bubblewrap, else the Landlock
trampoline, else none, and records every step in
`BackendSelection::reasons`. An explicitly requested backend that is not
available ends in none; there is no fallback. "Trusted `bwrap`" means one at the
fixed root-owned paths. The trampoline is looked up next to the running
executable, or at an explicit path, and never on `PATH`. `make install`
installs `harw-job-exec` next to `harw`.

- `harw_command::JobCommandPort::host` uses it with a private
  `<state_dir>/sandbox-plans` directory. Host commands
  (`CommandSandbox::Host`) run exactly as before; the backend applies only
  to sandboxed requests.
- The sandboxed verify runner (`harw-cli/src/verify_sandbox.rs`) sets
  `needs_network`, because its `WorkspaceBuild` jobs need network and
  this path configures no egress relay. The Bubblewrap backends are then
  always skipped. `HARW_VERIFY_BACKEND=auto|bwrap-trampoline|bwrap|landlock|none`
  chooses the request (unknown values fail closed). An explicit
  `bwrap-trampoline` or `bwrap` therefore ends in none, and no verify
  runner is built (the work driver keeps its fallback verifier). The
  runner logs the choice with a per-dimension table (namespace,
  filesystem, network, `no_new_privs`, capabilities, resource limits,
  cgroup; `harw_job_runtime::predicted_dimensions`).

### Per-job write scope and fail-closed fallback (PL-97 E2)

A `JobSpec` may carry a versioned write scope
(`write_scope: Option<JobWriteScope>`, `{ version, paths }`, absent in old
records). It is the typed hook for per-worker file isolation (PR #153
W2). The scope **replaces** the workspace write grant of the job's
sandbox profile and never widens it. The workspace becomes read-only
(read and execute stay), and only the listed paths stay writable. What
the profile grants outside the workspace (`/tmp`, `/dev/null`) is
unchanged.

- Versions: the runtime checks `version` before anything else. An
  unknown version is refused with `SpecError::UnsupportedWriteScopeVersion`
  at admission (`JobSpec::validate`, `JobSpecEnvelope::into_spec`) and
  again at start, never ignored. A scope with unknown fields does not
  deserialize.
- Paths: absolute, normalised (no empty, `.` or `..` segment, no trailing
  `/`), not `/`, no duplicates, at most 256. When the attempt starts, the
  Linux executor resolves them against the workspace. A path outside the
  workspace is refused (`WriteScopeOutsideWorkspace`), and so is one that
  resolves outside it through a symlink. An existing path gets a rule for
  its canonical form. The Landlock planner skips paths that do not exist
  (`PreparedRuleset::skipped_paths`). A path that does not exist yet (a file
  the job will create) therefore gets no rule of its own. It must be covered
  by an entry for an existing ancestor directory, usually its parent, whose
  rule grants the write; an uncovered one is refused (`WriteScopeUncovered`).
- Profiles: `read_only_analysis` grants no workspace write and takes no
  scope. A read-write rule that contains the workspace would keep the
  whole workspace writable, because Landlock rights are inherited. The
  common case is `/tmp`, which is writable in every profile, for a
  workspace below it. Such a scoped job is refused.
- `LandlockTrampoline`: the scoped policy (host paths) is the trampoline's
  policy.
- `BwrapTrampoline`: bwrap binds the workspace as the profile says, as a
  whole, because a mount namespace cannot make a path below a read-only
  bind writable. The scoped policy, mapped onto the namespace view
  (`PathMap`), becomes the inner stage's Landlock policy. Every scope path
  must keep a read-write bind there. Otherwise the plan is refused instead
  of the mapping dropping the path as unmapped. The rule from the security
  review (finding 4) holds for the policy that is actually used: no inner
  policy grants write at, beneath or above `/run/harw-job`, where the
  attempt directory is bound.

Scoped jobs always carry `SandboxRequirement::Required`
(`JobSpecBuilder::write_scope` sets it, `JobSpec::validate` refuses
anything else). A `required` job is refused **before any process starts**
when the selected backend cannot enforce the filesystem dimension, or, on
`landlock`, the network dimension of the job's profile:

| Backend | Filesystem dimension | Scoped / `required` job |
|---|---|---|
| `none` | not enforced | refused (`SandboxRequirementNotMet`) |
| `bwrap` | partial (mounts cannot separate read from execute) | refused before planning |
| `landlock` | Landlock ABI ≥ 5 enforced, 1–4 partial, none unsupported; no network namespace, so a profile that restricts the network is partial at best (Landlock restricts TCP only) | runs only with ABI ≥ 5 **and** a network-allowing profile (`workspace_build`); `no_network` and `network_restricted` scoped jobs are refused (use `bwrap-trampoline`); refused before the plan file exists |
| `bwrap-trampoline` | as `landlock` (needs a usable ABI at all); the network namespace enforces the network | runs only with ABI ≥ 5 (`no_network` as is; `workspace_build` and `network_restricted` through a configured egress relay, as for unscoped jobs); refused before the attempt directory exists |

Only legacy unscoped `best_effort` jobs run on a weaker backend. The
executor logs that once ("runs without full cgroup/landlock enforcement").

- Trust boundary: the scope is policy data of the job spec. The runtime
  narrows with it and never grants a path the profile does not already
  grant for writing.
- Failure mode: fail-closed. An unknown version, an unresolvable or
  uncovered path, a writable ancestor or a backend that cannot enforce the
  scope means the job does not run.
- Persistence: the scope travels in the job record (`JobSpecEnvelope`
  version 1, additive optional field) and in the trampoline plan of the
  attempt.
- Swapped paths: scope paths are resolved when the attempt is prepared
  and opened again by the trampoline just before `exec`. A process that
  can write an ancestor directory of a scope path (another job, a sibling
  worker) could swap the path for a symlink in that window. The scoped
  policy is therefore in Landlock hard-requirement mode: the trampoline
  reads every write rule back through `/proc/self/fd` after opening it and
  refuses the job when the opened object is not exactly the planned
  canonical path (`SandboxError::InvalidPath`). The rights of a rule follow
  the opened fd (`fstat`), not a second lookup of the path. Every write
  rule of a scoped policy must therefore be canonical where the trampoline
  runs, `/tmp` and `/dev/null` included; on a host where `/tmp` is a
  symlink, scoped jobs are refused. Read and execute rules grant no write
  and are not read back.

The same work package maps every job limit of `ResourceRequest` into the
job cgroup (`memory_max`, `memory_high`, `cpu_weight`, `cpu_max`,
`pids_max`, `io_weight`) and the per-process limits into the trampoline's
rlimits. A limit the host cannot apply is reported, never dropped: without
a cgroup root every job limit counts as `not_enforced`, and a controller
that is not delegated makes the cgroup report `partial` or `not_enforced`.
The OCI and Kubernetes executors do not map or report `memory_high`,
`cpu_max` and `io_weight` yet (an open PL-97 follow-up); no producer may
set them for those executors before it lands. Those executors never mount
the host workspace, so a write scope has nothing to narrow there.
`[sandbox] execution = "chain" | "legacy"` (default `chain`,
profile-scoped, see `config-structure.md`) selects between this path and
the previous pre-built `prlimit … -- bwrap …` host job. It is parsed here
and consumed by `shell.exec`, `job.start` and the child agents in PL-97
E2b.

### `shell.exec`, `job.start` and child agents on the chain (PL-97 E2b)

With `execution = "chain"` the sandboxed path of `shell.exec`
and `job.start` no longer builds a `prlimit … -- bwrap …` argv and runs it
as a host job. It submits one job-runtime request
(`harw_command::CommandSandbox::Workspace`, built in
`harw-tool-shell/src/job_spec.rs`):

| Field | Value |
|---|---|
| program | `/bin/sh -c <command>`, exactly the shell semantics of before |
| working directory and workspace root | the sandbox's canonical workspace root |
| profile | `no_network` with `WriteWorkspace`, else `read_only_analysis` |
| requirement | `best_effort`; `required` with a write scope |
| modules | the trusted `SandboxProfile` (Cargo, tmux), the host `PATH` binding and the `/tmp` size of `ShellLimits` |
| resources | `ShellLimits::job_resources` of the provider's limits; for the `shell.exec` defaults 2 GiB `RLIMIT_AS` (per process; no job-wide `memory_max`, which the legacy path never had), the CPU seconds (grown with the granted timeout), 256 MiB `RLIMIT_FSIZE`, 256 `RLIMIT_NOFILE` and 4096 `pids_max`; `require_rlimits` as configured |
| deadline | `shell.exec`: the effective timeout; `job.start`: none of its own (stop is explicit), as before |
| environment | empty: the sandbox sets its own baseline, as the legacy plan did with `--clearenv` |
| stdio | piped through the command port, which keeps the output caps, truncation, cancel and exit codes of before |

`RLIMIT_NPROC` has no field in the request: the process count is capped by
the job cgroup's `pids.max` instead, so without a cgroup root it is
reported as not enforced rather than capped. `prlimit` is looked up only
where it is needed: on the plain `bwrap` backend the runtime runs its own
`prlimit` in front of `bwrap` (same check and warning as before); the
trampoline backends set the rlimits themselves, so a host without
`prlimit` does not block them. The route is chosen before `prlimit` is
looked up, for `shell.exec` and `job.start` alike: with `require_rlimits =
true` and no fixed-path `prlimit`, a `bwrap-trampoline` route still runs
and its job spec carries the rlimits in its `ResourceRequest`, while a
plain `bwrap` route is refused before anything starts (regression tests
in `harw-tool-shell/src/exec.rs` and `exec/background.rs`).

**Composition.** The runtime assembly passes `[sandbox] execution` (default
`chain`) through `HostPermitWiring::with_sandbox_execution` to every shell
provider it builds (`harw-registry-defaults` `build_shell`, root and child
registries); `job.start` clones that provider, so it takes the same path.
A compiled agent reads no `[sandbox]` table (`load_config_embedded` starts
from the config defaults), so `harw-agent-runner` passes the default,
`chain`, to `JobChildBackend::with_execution`
(`compiled_agent_sandbox_execution`), together with its canonical working
directory (`with_workspace_root`). `execution = "legacy"` is the opt-out.
`SandboxExecution::default()` stays `Legacy`: it is the safe fallback of
a provider or backend built without any config (tests, the job-control
provider of orchestrators, which never starts a job).

**The workspace root.** The host port's runtime has `/` as its workspace
root, and both Bubblewrap backends refuse a host-root workspace. A
workspace request therefore carries its own root. `JobCommandPort::host`
registers root and modules under a fresh idempotency key before it
submits, and its executor (`HostCommandExecutor`) starts the attempt with
that root on a Linux executor per module set and network class (at most
16). An attempt whose key has no registration does not run.
`CommandPort::workspace_backend(profile)` tells a caller which backend its
request would get. Host commands and `CommandSandbox::Profile` requests
run exactly as before.

**Profile modules.** `BwrapExecutor::with_profile_modules`, `with_host_path`
and `with_tmpfs_size` (`harw-job-executor-bwrap`) bind what the legacy
launcher bound: the Cargo toolchain and cache, the tmux socket, the host
`PATH` directories, the `/tmp` size. `harw-sandbox` plans these binds as
before. The trampoline composition reads them back from the launcher argv
(`BwrapTrampolinePlan::module_binds`), maps them into the namespace view
and gives each one Landlock rule inside: read and execute for a read-only
bind, read, write and execute for a read-write one. That is what the mount
alone grants, so nothing the modules allowed is lost.

**Network, per `NetworkMode`.** The host port selects twice
(`select_backend`): once for profiles that deny the network, once with
`needs_network`. No egress relay is configured there, so the second
selection always skips both Bubblewrap backends (`landlock`, else `none`).

| Mode | Profiles | Backend from the host port | `shell.exec` / `job.start` |
|---|---|---|---|
| `NetworkMode::None` (own empty network namespace) | `no_network`, `read_only_analysis` | `bwrap-trampoline`, else `bwrap`, else `landlock`, else `none` | chain on `bwrap-trampoline` or `bwrap`. On `landlock` or `none` the chain would be weaker than the legacy namespace: an unscoped call keeps the legacy path, a scoped one is refused |
| `NetworkMode::ProxyOnly` (relay) | `workspace_build`, `network_restricted` | `landlock`, else `none` (no relay configured) | not used: the legacy plan never had network, so the chain profiles never ask for it. A networked call would get no namespace, so it could only fall back or, with a write scope or `required`, be refused |

**Fail closed.** Only an unscoped call may keep the weaker legacy path.
A call with a write scope never takes the host path: when host execution
applies (an approved session or single-use lease, a `Host` sandbox
profile, `HostWithoutPrompt` on a platform without sandbox), `shell.exec`
and `job.start` refuse it before anything starts ("host execution cannot
enforce a write scope"). A write scope also needs `WriteWorkspace`: a
scoped read-only call is refused with a clear tool error instead of
failing later in the job spec.
The first such call of a session logs `runs without cgroup/landlock` with
the reason. A call with a write scope (`ShellToolProvider::with_write_scope`,
trusted runtime input, never a tool argument) needs `bwrap-trampoline` for
these offline profiles: plain `bwrap` cannot narrow the workspace bind, and
the Landlock trampoline has no network namespace. On any other backend, or
with `execution = "legacy"`, the call is refused with a tool error, and
nothing is submitted. The runtime checks the same again before a process
starts. `execution = "legacy"` keeps the previous path for every unscoped
call, byte for byte. Approvals are unchanged: the host-permit path
(`run_host_command`, `!` host mode) stays the only `CommandSandbox::Host`
submission of a model call.

**Child agents.** `JobChildBackend` (`harw-agent-runner`) submits each child
through `harw_tool_job::launcher::child_agent_launch` when it runs in
chain mode and the composition passed a canonical workspace root
(`JobChildBackend::with_workspace_root`; the backend never falls back to
the process working directory on its own). The runner passes its own
canonical working directory: the directory its session runs in, from
which the parent's sandbox root is discovered, and the directory the
previous host submission let the child inherit.
The child is a workspace request over that root with the
`workspace_build` profile, no longer a host submission over `/`. Without
a root the child keeps the host submission (logged once).

This is an accepted deviation from plan §3.4 E2, which asked for a
confined child with `executed_on_host = false`: the child process itself
runs with requirement `none` and `executed_on_host = true`, so it is as
unconfined as before; only the record's root and executor change. The
follow-up (grants for the harw home and the runner path, the child's tool
processes delegated to the parent runtime) is tracked for PL-97. No
backend can confine the child today:

- A Landlock domain (both trampoline backends) forbids `mount(2)` and
  `pivot_root(2)`. The Bubblewrap sandboxes the child starts for its own
  tool calls would fail inside it.
- The `workspace_build` policy grants neither the harw home (the child's
  session store and job state) nor the runner executable outside the
  system trees (`~/.local/bin` by default).
- A networked profile gets no Bubblewrap backend from the host port.

The child keeps what every runtime job has: the job cgroup (once the
runtime has a cgroup root), the process group and the attempt record. Its
tool processes are confined one by one, as before. Where the job runtime
runs no workspace requests (no router, a non-Linux host), the child keeps
the host submission, logged once per backend.

**Known gaps (open PL-97 follow-ups).**
- A command that starts its own `bwrap` fails under `bwrap-trampoline`
  (Landlock forbids `mount(2)`), where the legacy plain `bwrap` allowed
  nesting with user namespaces.
- A scoped job with profile modules on `bwrap-trampoline` gets no Landlock
  rules for the modules: the coordinator's scoped inner policy does not add
  `module_binds` yet.
- `RLIMIT_NPROC` on the chain: `nproc` maps to `pids_max`, which needs the
  job cgroup. Without a cgroup root the process count is not capped
  (fork-bomb protection the legacy `prlimit` gave). Now that `chain` is the
  production default, `execution = "legacy"` is the workaround where that
  matters. Fix: carry `RLIMIT_NPROC` through `ResourceRequest` and the
  trampoline (`harw-job-core`, `harw-job-exec`).
- The runner's child root is its canonical working directory, not the
  parent's bound sandbox root (the discovered project root, narrowed by a
  compiled write path), which only the runtime assembly computes. That is
  harmless while the child runs with requirement `none`; a confined child
  needs the bound root passed through instead.
- The host port carries the per-request root and modules through an
  idempotency-key registration (`HostCommandExecutor`). A workspace-marked
  key never deduplicates, and a persisted record is refused after a
  restart. A typed per-spec workspace root is the intended replacement;
  this needs owner approval as a plan §8 amendment.
- The host port has no cgroup root yet (E3), so no job, child or not,
  gets a job cgroup in production.

## Implementation

### `SandboxProfile` (`harw-sandbox/src/profile.rs`)

An enum with four variants:

- `Strict` — hermetic bubblewrap sandbox, the default.
- `Cargo(CargoSandboxProfile)` — isolated sandbox with the toolchain.
- `Tmux(TmuxSandboxProfile)` — isolated sandbox with one socket.
- `Host` — local host execution, requires a process permit.

The profile is a trusted runtime input: built from configuration and UI
grants when the runtime is assembled, never from a tool call.

### `TmuxSandboxProfile` (`harw-sandbox/src/tmux.rs`)

Validates a single socket path (absolute, normalized, existing, a Unix
socket). Binds only that socket at `/run/harw/tmux.sock` inside the
sandbox. `Inspect` binds read-only; `Write` binds read-write.

### `BwrapLauncher` (`harw-sandbox/src/bwrap.rs`)

- `with_shared_cgroup_namespace()` replaces `--unshare-all` by the explicit
  list `--unshare-user --unshare-ipc --unshare-pid --unshare-net
  --unshare-uts` (no new cgroup namespace) for the trampoline composition
  above; every other plan is unchanged.
- `with_profile(&SandboxProfile)` sets all modules in one call;
  `with_cargo_profile()`/`with_tmux_profile()` remain for individual use.
- `plan()` binds the cargo toolchain and/or tmux socket per the profile.
- `plan()` sets `--unshare-user` plus `--uid`/`--gid` to the harw process's
  effective IDs (from `/proc/self` metadata, injectable for tests) for
  **every** profile, and binds `/etc/passwd`, `/etc/group` and
  `/etc/nsswitch.conf` read-only via `--ro-bind-try`; `USER`/`LOGNAME` are
  set from the harw environment. This makes `whoami`/`id` work inside the
  strict sandbox too. Host execution already runs as a normal user process
  and is unaffected by this.
- `with_host_path(path: &str)` binds, for each directory that exists in the
  given `PATH` and is not already under `/usr /bin /lib /lib64` or the
  workspace, a `--ro-bind-try dir dir` (excluding `/` and exactly `$HOME`)
  and sets `--setenv PATH <host PATH>`. If the `PATH` includes
  `~/.cargo/bin`, `$RUSTUP_HOME`/`$CARGO_HOME` are also bound and set. Not
  calling it leaves the hermetic minimal-`PATH` behavior unchanged. Used by
  the TUI's `!` command; ordinary model `shell.exec` in the project sandbox
  never calls this and stays hermetic.

### `ShellExecutor`/`ShellToolProvider` (`harw-tool-shell/src/exec.rs`)

- `sandbox_profile: SandboxProfile` and
  `permit_ledger: Option<Arc<ProcessPermitLedger>>` fields.
- Host profile with no ledger → fail-closed. Strict/Cargo/Tmux with no
  ledger → normal (the sandbox is the boundary).
- `run_command` computes `effective_host = sandbox_profile.is_host() ||
  registry.is_session_approved(session_id) || registry.take_single_use(session_id)`.
  When true, the call runs, after `authorize_host_command`, **without
  bubblewrap**: `tokio::process::Command::new("/bin/sh").args(["-c", cmd])`,
  `cwd` at the workspace root, the harw environment fully inherited
  (including the user's shell context — `PATH`/`HOME`/`CARGO_HOME`), still
  under the existing `prlimit` limits. Timeout, cancel and
  `terminate()`/output truncation follow the same paths as the sandboxed
  case. Model `shell.exec` with no active session or single-use grant stays
  hermetic in bubblewrap with a minimal `PATH` — `effective_host` becomes
  true only via `SandboxProfile::Host`, an active `sandbox-lease`, or a
  consumed single-use grant, never via the tool call itself.
  `is_session_approved`/`take_single_use` also consult the process-wide
  global grant state described above; `ShellExecutor` itself doesn't
  distinguish global from per-session, it only asks "is this session_id
  allowed" — whether it can ask at all depends on
  `self.host_permit_registry.is_some()` (see the registry wiring note
  below).
- `with_host_path()` on `ShellToolProvider` passes through to
  `BwrapLauncher::with_host_path`. Called by the TUI's `!` command
  (`harw-tui/src/command_exec.rs::execute_shell`), which supplies the
  `PATH` read from harw's own shell environment at startup, independent of
  any `sandbox-lease`. `!` commands therefore always run with harw's shell
  `PATH` bound in, but still inside bubblewrap — never on the host, even
  under an active lease.

### Registry (`harw-registry-defaults/src/profile.rs`)

- `profile_tool_providers()` takes `sandbox_profile: &SandboxProfile`.
- `assemble_registry_for_sandbox_with_definition_access_and_sandbox_profile()`
  takes the profile explicitly; the existing function delegates with
  `Strict` as the default.
- `build_shell_provider` wires the permit ledger, session registry and
  question-channel sender from `HostPermitWiring` onto **every** constructed
  `ShellToolProvider` whenever `host_permits` is supplied, regardless of
  whether `sandbox_profile.is_host()` — previously this only happened for a
  `SandboxProfile::Host` profile, which meant a `/sandbox-lease` grant never
  reached the root session (which runs `Strict`). The security boundary is
  unchanged by this: for Strict/Cargo/Tmux, `determine_effective_host`
  still checks only the registry grant, never the permit ledger itself, and
  with no active grant the sandbox remains the boundary.

### Runtime (`harw-runtime/src/assembly.rs`)

- `sandbox_profile_from_config()` builds the profile from `[sandbox]`
  configuration.
- Cargo → `Cargo(CargoSandboxProfile::new(...))`, Tmux →
  `Tmux(TmuxSandboxProfile::new(...))`, otherwise `Strict`.
- On a validation error: warns and falls back to `Strict` (fail-safe).

### Configuration (`harw-config/src/harness_config.rs`)

```toml
[sandbox.cargo]
mode = "build_offline"
cargo_bin = "/opt/harw/toolchain/bin/cargo"
rustup_home = "/opt/harw/rustup"
cargo_home = "/var/cache/harw/cargo"

[sandbox.tmux]
mode = "inspect"
socket_path = "/tmp/tmux-1000/default"
```

All sections use `deny_unknown_fields`. A missing section leaves the
sandbox hermetic.

### Worker definitions (`harw-registry-defaults/agents/`)

- `sandbox-shell-worker.toml` — strict profile.
- `cargo-worker.toml` — cargo profile.
- `tmux-inspector-worker.toml` — tmux profile.
- `host-process-worker.toml` — host profile, permit required.
- `uia-shell-worker.toml` — host profile, `shell.exec` plus read-only
  `fs.*`, permit required.

All extend `worker-base@1`, have `shell.exec` as their only tool (plus
read-only `fs.*` for `uia-shell-worker`), and `max_depth = 0`.
