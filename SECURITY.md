# Security Policy

## Reporting a vulnerability

Please report suspected security vulnerabilities privately, using GitHub's
private security advisories for this repository (repository **Security**
tab → **Report a vulnerability**). Do not open a public issue, discussion, or
pull request for a suspected vulnerability.

Include, where possible:

- the affected version or commit,
- a description of the issue and its impact,
- steps to reproduce or a proof of concept, and
- whether you believe it is already being exploited.

We aim to acknowledge new reports promptly and will work with you on a fix
and a coordinated disclosure timeline.

## Supported versions

Harwness is pre-1.0 (`0.x`) and moving quickly. Only the latest release
receives security fixes; older `0.x` releases are not backported. If you are
running an older version, please upgrade before reporting, or note the
version in your report so we can confirm whether it still applies to the
latest release.

## Core security invariants

These are invariants the project maintains across the runtime, the terminal
UI, and the Detect · Orient · Defend host-security plane. A change that weakens one
of them is a security regression even if it is otherwise a passing change,
and should be flagged rather than merged silently — see
[CONTRIBUTING.md](CONTRIBUTING.md#security-sensitive-changes).

- **Full Access means no confirmations at all.** In the approval mode
  `full` (Full Access, Shift+Tab in the TUI) harw never shows an approval
  dialog — not for `process.kill`, not for `host.sudo_exec`, not for
  `agent.cancel` or agent/skill commits, not for remote OCR uploads, not for
  host escalation (a host request is granted as a session lease that lasts
  until Ctrl+H or `/sandbox-lease revoke`), and the auto-mode classifier is
  not consulted. Child agents follow the root into `full` and relay no
  questions either. The only remaining interaction is the sudo password
  field, and only when `sudo` itself needs a password. An explicit
  `/permissions deny` rule still applies: under `full` it refuses the call
  instead of asking. If you want prompts, use `ask` or `auto`.
- **`process.kill` requires approval in `ask` and `auto`, and never runs via
  sudo.** The agent tool only ever targets processes owned by the calling
  user's own effective UID; it is registered in the always-ask tool set and
  cannot be auto-approved by a rule or the classifier (only Full Access skips
  the question, see above). It never escalates through `sudo` or any other
  privileged path — a process outside the caller's own user is reported,
  never terminated.
- **The sudo password never reaches the model, the transcript, logs, or
  disk.** Root commands go through a dedicated approval window
  (`host.sudo_exec`) separate from the ordinary approval dialog (under Full
  Access the window only asks for the password, and only when `sudo`
  requires one). The password
  is typed there, masked, held only in memory for the configured session
  window, and is never included in the message history sent to the model,
  the exported transcript, application logs, traces, or any file on disk.
- **`shell.exec` rejects `sudo`, `doas`, and `pkexec` outright.** Commands
  that start with one of these are refused before execution; a root command
  can only be requested through the dedicated `host.sudo_exec` approval path,
  never smuggled in through the ordinary shell tool.
- **Read-only roles never get network or write or exec capability.** An
  agent role scoped to read-only exploration (for example an explorer or
  researcher) cannot reach `fs.write`, `shell.exec`, or network-capable
  tools; that boundary is enforced by the runtime's derived tool surface, not
  by prompt instructions.
- **A child's network scope is never broader than its parent's.** Privileges
  only shrink on the way down to spawned child agents — a child can never end
  up with more network (or filesystem, or tool) access than the session that
  spawned it.
- **Workspace files leave the machine for remote OCR only as configured.**
  Remote OCR is off by default: `doc.read_pdf` extracts locally unless
  `[tools.doc].remote_ocr` is set and a Mistral provider is configured. `ask`
  requires an approval naming the host for every upload in the approval
  modes `ask` and `auto` (Full Access uploads without asking); `off` never
  uploads; an untrusted project config can only tighten the setting, never
  loosen it.
- **Always-ask tools are never auto-approved in `ask` or `auto`.** Tools
  registered in the always-ask set (destructive or high-impact operations
  such as `process.kill` and `host.sudo_exec`) are excluded from the
  deterministic auto-approval prefilter and from the auto-mode classifier
  outright; no allow rule or classifier verdict can move them into automatic
  approval. Only choosing Full Access removes the question.

### Harw-DoD (Detect · Orient · Defend) and AI integration

The following invariants belong to the PL-100 integration of Harw-DoD with
the model-driven runtime. The wording is binding for new code. Where the
implementation is still being rolled out (PL-100 Phase 3), the item is a
**target state**: code that contradicts it is a defect, and the statement
must not be read as a guarantee for builds that lack the component. The
authoritative plan and threat model are in
[`docs/planning/100-dod-ai-integration/`](docs/planning/100-dod-ai-integration/README.md)
(see also its [THREAT-MODEL.md](docs/planning/100-dod-ai-integration/THREAT-MODEL.md)).

- **S1 — harw never calls the escalation ladder or the warden.** The harw
  runtime, ops and TUI, and every agent or model-driven code path, never
  reach the escalation ladder (`harw-dod-escalate`, planned name
  `harw-escalator`), never talk to the Warden and never write Harw-DoD
  enforcement state. The only exception is the root-only, human-invoked CLI
  command `harw dod warden enable`, which a model cannot trigger. Enforcement
  stays on the human path. Today this is documented at `harw-ops/src/dod.rs`
  (trust boundary S1, covering the read-only ops module).
- **Model output never authorizes anything; verdicts are advisory and never
  suppress notices.** A triage verdict annotates a finding; it never decides.
  A model-asserted `Confirmed` is capped to `NeedsReview` and never yields an
  escalation stage. Every high-severity finding always produces a
  deterministic notice; only human-signed labels with an expiry date may
  suppress. DoD data never starts a model turn by itself, except for the
  dedicated triage child. Untrusted text (sensor strings, file and process
  names, model output) reaches terminals and models only through the shared
  escaper.
- **Operator authority lies outside harw (target state).** A compromised
  harw process controls its own terminal, so it can supply neither the
  displayed proposal nor the confirmation. An approval is therefore valid
  only through one of two paths: polkit, with the details (action, cgroup,
  rule, short hash, digest) supplied by the escalator and not by harw, or an
  independently started `harw-dod-approve` in a session harw does not
  control. Harw may only ask the escalator to request an approval; it never
  starts `pkexec` or an approval dialog itself, and `isatty` is not proof of a
  human. An approval is bound to the proposal digest that the trusted
  surface displayed. The optional same-TTY mode (`[dod.approve] tui_tty`)
  has a weaker binding and covers only the reversible actions freeze and
  thaw. Kill and network isolation need the explicit `irreversible_ack`
  confirmation and a separate polkit action, and are refused in the same-TTY
  mode.
- **The TCB policy lives in root-owned files, not in harw's config.** The
  policy that governs enforcement is read from `/etc/harw-dod/config.toml`
  (`load_system_config`, which requires root-trusted ownership of the path)
  and, once the Warden reads its own policy file, `/etc/harw-dod/warden.toml`
  (planned; it does not exist yet). The `[dod]` section of harw's own config
  (`auto_freeze`, `kill_requires_human`, `allowed_cgroup_prefixes`) is
  advisory only: it never authorizes or enables an action and is used by the
  doctor and for display. `kill_requires_human` is validated at load and
  must stay `true`.
- **The triage model is the owner's explicit choice.** Without a configured
  `[dod.triage].model`, no automatic triage runs and there is no silent
  fallback to the session model. The model can only be set from trusted
  config layers, never by a project. `harw dod doctor` shows provider, model
  and locality (loopback or Unix socket is local, anything else is remote);
  the locality is shown, not enforced. With a remote model the provider
  sees bounded evidence cards, which is the owner's deliberate decision.
- **The finding spool is read only by the escalator (target state).** Sensor
  records in the spool are verified by the escalator itself (owner, mode,
  digest, and the kernel cgroup ID again at signing time). Harw works from
  the findings export and never from the spool. Until the triage reader is
  moved off the spool, the module documentation in
  `harw-dod-sentinel/src/spool.rs` describes the current behaviour.

This list describes the invariants as maintained; it is not an exhaustive
threat model. See `docs/design/` for the fuller security and runtime
architecture documentation, and `docs/planning/100-dod-ai-integration/` for
the Harw-DoD AI-integration plan, principles and threat model.
