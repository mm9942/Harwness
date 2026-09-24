# Mediated Process Execution and Sandbox Modules

> Status: implemented · Last reviewed: 2026-09-24

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
