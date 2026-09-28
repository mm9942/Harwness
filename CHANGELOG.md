# Changelog

All notable changes to this workspace are recorded here. Format follows
[Keep a Changelog](https://keepachangelog.com/) and this project uses
Semantic Versioning within the 0.x pre-release range.

## [Unreleased]

### Fixed

- `curl -fsSL https://get.harw.dev/harw/install.sh | bash` no longer fails
  with a bare 404 on `Harwness-main.zip`. Without an argument it now installs
  the prebuilt release from the mirror (`latest` + `<tag>/SHA256SUMS`) when
  one exists for this machine (x86_64 or aarch64), and only otherwise builds
  from source. `--binary` reads the mirror instead of the private GitHub
  releases. Missing files fail with a clear message.
- `install.sh --binary` no longer aborts with `work_dir: unbound variable`
  at exit after a successful install.
- The release `mirror` job also uploads `Harwness-main.zip`, which the
  source path of the installer downloads.

## [0.9.0] — Unreleased

### Added
- Tool gateway, round R18a (contract
  `docs/planning/65-cloud-sessions/contracts/R18-tool-gateway.md`):
  - Tools execute in the gateway inside the sandbox; only a UIA agent
    principal is admitted to `tool.call`, sub-agents reach tools only
    through delegation that can only narrow rights. Admission is
    fail-closed in a fixed order and never falls back to host execution;
    sessions belong to their owning agent.
  - `harw-protocol`: `tool.*` / `gateway.*` wire (wire minor 2),
    `ToolPlacement`, `ToolPort` / `GatewayPort`.
  - `harw-session-host`: tool host, agent registry with cascading
    narrowing and revocation; `harw-session-ws` routes the new methods
    behind the caps granted at hello.
  - New crate `harw-tool-remote`: remote tool proxy; runtime mode without
    local tools.
  - `gateway.*` operations for the UIA: nine reads without approval (incl.
    `gateway.health` and `gateway.logs`), five mutations that always ask;
    key operations keep no model surface.
  - WorkDriver: `work_driver.report` replaces free-text parsing, writable
    workspace scope for command-only criteria, central artifact
    verification, role instructions, judge retry then escalate, lane size
    from config, the UIA may enqueue (with approval).
  - TUI: `Shell(...)` instead of `Bash(...)`, placement badge (host /
    sandbox / gateway), `Job(name)` labels, plan cells show only the delta,
    job failure reasons and originating tool call.
  - Auto-mode classifier retries once on an empty reply, then falls back to
    the main model, otherwise asks with a named cause.
- Session control plane, W00 round 1 (PL-65, contract
  `docs/planning/65-cloud-sessions/contracts/W00-websocket-control-plane.md`):
  - `harw-protocol`: `session_wire` (cursor, caps, frames with an additive
    `Unknown` fallback, typed params/results for the closed method table)
    and `session_port` (`SessionPort`/`FrameSource`, std futures only).
  - New crate `harw-session-host`: single writer of hosted sessions with
    tenant/capability admission, durable records and restart recovery,
    transcript replay by cursor, a live ring with snapshots, bounded
    per-attachment queues with coalescing and `Lagged`, compare-and-swap
    input arbitration with idempotent `client_msg_id`, first-writer-wins
    approvals with host-derived actors, presence, revocation and drain.
  - New crate `harw-session-ws`: `harw.session.v1` WebSocket transport
    (upgrade validation incl. `Origin` refusal, codec and limits, hello
    gate, request correlation, a fair frame multiplexer that keeps
    responses ahead of delta floods).
  - Arch gate: `[[forbidden_crates]]` rules (tungstenite only in ring A;
    no transport crates in `harw-protocol`).
  Not wired into `harw gateway`/TUI yet (W04–W06).
- Tailscale access: `harw tailscale status` and `harw web --tailnet
  [--tailnet-port]`. The control plane listens on the node's tailnet
  address only, admits peers that `tailscaled` identifies via `whois`, and
  gives every tailnet device the tier Operator through a dedicated
  `tailnet.sock` (new crate `harw-tailscale`, `ForwardedPeerResolver` in
  `harw-web`). See `docs/setup/tailscale.md`.

### Changed
- `job.wait` is a short poll: `timeout_secs` is limited to 1..=60 (larger
  values are refused); job completion arrives as a notification.
- `shell.exec` documents POSIX `/bin/sh` and steers file edits to `fs.*`.

### `harw update` installs updates

- `harw update` checks the latest GitHub release and installs it: tarball
  plus `SHA256SUMS`, checksum-verified, binaries replaced by rename with the
  previous ones kept as `<name>.old`. Without a published release it updates
  from the recorded sources (`git pull --ff-only` + `make install`, or the
  archive's `scripts/install.sh`).
- `--check` only checks (exit code 10 = newer release), `--yes` skips the
  question, `--dismiss` hides the start notice for the known version.
- The chat start names a newer version from `~/.harw/version.json` and, at
  most every 20 hours, checks in the background (`HARW_NO_UPDATE_CHECK=1`
  turns this off).

## [0.8.0] — 2026-09-28

### One systemd source of truth (Crypto Masterplan v2 H10)

- `deploy/` is the only source of system units, `sysusers.d` and
  `tmpfiles.d`. `dod/packaging/{systemd,sysusers.d,tmpfiles.d}` are gone;
  `dod/scripts/install.sh` installs from `deploy/` and refuses unresolved
  `@PLACEHOLDER@`s.
- **Breaking for DoD hosts:** units are renamed (`harw-dod-sentinel` →
  `harw-sentinel`, `harw-dod-bpf` → `harw-probe-bpf`, `harw-dod-warden` →
  `harw-warden`; `harw-dod.target` stays), every binary has its own system
  account (the Warden no longer runs as root: `CAP_DAC_OVERRIDE
  CAP_NET_ADMIN`), and the sockets live in `/run/harw/` instead of
  `/run/harw-dod/`. Re-run `sudo make dod-install` and `make dod-enable`;
  the old unit files are removed, the old accounts are left for manual
  cleanup.
- New infrastructure units: `harw-infra.target`, socket-activated
  `harw-auth-hub` and `harw-netsec` (`.socket` + `.service`, run with
  `--systemd-socket`), and the self-binding `harw-control.service`
  (`harw web`) and `harw-security-hub.service`; all sockets under
  `/run/harw/infra/`.
- `harw-install` embeds every `deploy/` file in production code;
  `harw install --print-systemd [UNIT]` prints them.

### One Cargo workspace (PL-60)

- The DoD crates under `dod/crates/` are now members of the root Cargo
  workspace. `dod/Cargo.toml` (the nested `[workspace]`) and `dod/Cargo.lock`
  are gone; the root `Cargo.lock` is the single resolution record, and
  `exclude = ["dod"]` was removed. `semver` moved into the root
  `[workspace.dependencies]`. CI, `dod/Makefile`, `xtask gates` and the docs
  build DoD from the root with `-p` selections. See
  `docs/architecture/dod-workspace-merge-plan.md`.

### Publication cleanup

Preparing the repository for its first public release under
`MIT OR Apache-2.0`:

- Added `LICENSE-MIT` and `LICENSE-APACHE`, and set `license`/`repository`
  on the root and `dod/` workspaces and all member crates.
- Added `CONTRIBUTING.md`, `SECURITY.md`, `CODE_OF_CONDUCT.md`, issue and
  pull request templates.
- Translated the documentation under `docs/` to English, added
  `docs/README.md` as a documentation index, and added a status header
  (implemented / partially implemented / proposal) to design and
  architecture docs.
- Documented `harw kill` / `process.kill` (engine: the standalone
  `harw-killer` crate — pidfd-based identity, immediate SIGKILL, a second
  SIGKILL for survivors, no SIGTERM phase) in the README and linked
  `harw-killer/README.md`.
- Removed internal working material not meant for a public repository
  (session transcripts, planning and remediation notes, internal round/wave
  labels and ledger references) from the tracked tree.

## History

The sections below summarize development before the public release, in
roughly chronological order. They are condensed from internal working notes;
internal round/wave numbering, agent nicknames, and session references have
been dropped in favor of the underlying change.

### Round 7 — background orchestration polish

- `transfer_to_*` now respects the spawning agent definition's token budget,
  synchronously and in the background, with a wrap-up pass at 80% and a
  handoff summary at the budget end.
- Orchestrators self-read at most a handful of times before they must
  delegate to a worker wave (configurable, root session exempted).
- A provider error now always produces a short summary instead of ending a
  run with no result at all.
- `agent.status` reports live token usage and context occupancy for a
  running child; repeated polling within a short window is called out.
- The auto-mode classifier is now told the originating child agent's actual
  task, so an explicitly requested action is no longer misclassified as
  off-task.
- Background agents survive a new message arriving mid-start. Optional tool
  arguments sent as the literal string `"null"` are dropped; child agents
  with small context windows get compact tool schemas.
- Added a LaTeX report skill and template with report/business-paper/manual
  scaffolds, a `latex.template` tool (never overwrites) and a `latex.check`
  tool (validates class, packages, fonts and languages up front).
  `latex.build` now also builds without `latexmk`, and reports pages,
  overfull/underfull boxes, missing characters and hyphenation warnings.
- The matrix-game flow now runs through a dedicated background orchestrator
  (draft scenario from free text, get it approved, play it out, produce an
  after-action report); the interactive `/matrix` command became
  display-only (`show|list|replay|compare`), fixing a TUI hang.

### Round 6 — auto-mode, host commands, export

- A classifier `deny` becomes a question instead of a hard block whenever
  someone can be asked (TUI root, or a child with a relay channel back to a
  human); a hard deny remains only where nobody can be asked.
- The auto-mode classifier now knows about the active host-command lease and
  recent conversation context, so an explicit request against an active
  lease resolves to allow rather than ask.
- The deterministic prefilter recognizes file-moving commands
  (`mv`/`cp`/`rsync`/`install`) targeting outside the workspace as a
  distinct category.
- Denial counters now only count genuine denials, deduplicating identical
  calls within a short window, and the resulting message names the limit
  that was hit.
- `!`-prefixed operator commands (`!cmd`, `! cmd`, `!!`) now always run on
  the host rather than in the sandbox, with the real `$HOME`, generous
  limits, and an audit event — no approval needed, but `sudo`/`doas`/`pkexec`
  are still rejected (use the dedicated sudo window instead). They run
  immediately even mid-turn, with output delivered once the turn ends; Ctrl-C
  cancels a running one without losing partial output.
- The final answer of a turn is no longer lost when auto-compaction fires
  mid-turn. `/export` now also includes command output, `!` results, system
  lines, and background-agent completions.

### Round 5 — sudo approval window, tool surface, background orchestrators

- Pinned the Rust toolchain via `rust-toolchain.toml` so local and CI builds
  use the same compiler, `rustfmt` and `clippy`; added a CI path filter so
  documentation-only changes skip the Rust jobs, and `actionlint` for the
  workflows themselves.
- Added `host.sudo_exec {argv, reason}`: a dedicated approval window shows
  the exact command, cwd and worker; the password is entered there, masked,
  and never reaches the model, history, logs, traces or disk. Supports
  one-time or session-scoped approval (capped, cleared on new/resumed
  session, exit, or a wrong password). `shell.exec` rejects commands
  starting with `sudo`/`doas`/`pkexec` outright and points to this tool.
  Every decision is audit-logged without the secret.
- Added `fs.edit` for targeted string replacement (single match or
  `replace_all`) under the same approval and limits as `fs.write`.
- Added background orchestrators: an orchestrator started by the root agent
  in the TUI keeps running after the root's turn ends, with tools to query
  (`agent.status`, `agent.result`), message (`agent.message`,
  `parent.message`), and cancel it, plus configurable spawn-depth and
  fan-out limits, a token-budget reserve for a structured handoff near the
  budget end, and a per-child activity journal.
- Added a plan-mode tool surface (`plan.write`, `plan.exit`, `plan.enter`)
  and a plan graph (multiple named plans, `plan submit`/`step`), a
  structured `ask_user` tool, and TUI support for background-agent streaming,
  a goal marker in the status line, and a plan-mode key cycle.
- Numerous fixes: normalized credential input (stripped whitespace/CRLF),
  sessions without a user turn no longer saved, compaction keeps the last
  two user turns, `analyze` from a root session now spawns the correct
  explorer role, child response truncation preserves head and tail, and
  `shell.exec` timeouts now scale by command class.
- Documentation: added the background-agents guide
  (`docs/guides/background-agents.md`).

### Round 4 — knowledge surfaces, LaTeX worker, interrupts

- Reworked onboarding so switching credential source (interactive login vs.
  API key) replaces the provider's credential pool instead of only
  appending to it, fixing a startup failure after switching.
- Long tool output stays collapsed by default in the TUI, computed in
  wrapped screen lines with an expand toggle.
- A child agent's context window now comes from the model it actually calls
  (falling back to the parent's model rather than a fixed default); the
  token budget counts only new, uncached input plus output, with a wrap-up
  instruction near the limit and a graceful partial result at the limit.
- Added the knowledge surfaces: **Workbench** (pinned files/notes per
  session or project), **Kanban** (cards with comments, evidence and
  approval before a worker picks one up), **Diary** (an automatic per-agent
  log), **Palace** (a long-term memory graph with promote/supersede behind a
  review gate), and **Dream** (an idle-time reflection run that only
  produces proposals). All backed by cross-process file locks and read-only
  agent tools; nothing is written automatically without a review step.
- Interrupt and queueing model: Ctrl-C/Esc cancel only the running turn;
  already-sent messages and commands stay queued and are delivered right
  after. Commands during a turn are classified as immediate, staged (take
  effect next turn), or deferred until the turn ends.
- Added a LaTeX writer role and a sandboxed `latex.build` tool (runs only
  `latexmk`, no shell escape, fixed argv, per-call approval).

### Round 3 — matrix game, command tree, learning loop

- Added `/matrix`: an umpire-run multiplayer scenario with a deterministic
  Rust-side game master, sealed submissions, a replayable journal, and
  seats that see only their own projection.
- Added `--agent NAME` / `/agent use NAME` to pick the root agent
  definition, `/research` and `/research-deps` for bounded read-only
  research, and `/learn` for session-derived proposals that require
  explicit acceptance before anything is written to memory or skills.
- Reorganized the `harw` command line into task-grouped subcommands (`chat`,
  `exec`, `analyze`, `session`, `config`, `provider`, `model`, `auth`,
  `project`, `agent`, `knowledge`, `jobs`, `gateway`, `serve`, `web`,
  `service`, `mcp`, `channel`, plus system commands), with new global flags
  (`--profile`, `--cwd`, `--json`) and old command names kept as hidden
  aliases. Full old-to-new mapping in `docs/cli.md`.
- Added `harw project trust|untrust|status` to make loading a project's
  repo-local `.harw` configuration an explicit, per-project decision.
- Added `harw completions` (bash/zsh/fish/elvish/powershell), with install
  and uninstall of the canonical shell-specific completion location.
- Added persistent permission scopes (session / project / global) and
  `/permissions` to manage allow/deny rules with argument patterns at any of
  those scopes, plus project discovery and a per-project `.harw` state
  directory.
- Added session metadata (title, timestamps, project) for the session
  picker, an approval dialog with "always allow this exact call" and
  "switch to auto mode" options, `/export` to Markdown, and addressable
  long-term memory facts with confidence decay and secret-pattern redaction
  before write.
- Fixed a context-budget bug where a long tool-heavy turn could push the
  triggering user message itself out of the history sent to the model; the
  triggering message is now always preserved, with older tool results
  trimmed first. Raised the auto-compaction trigger from 120k to 500k input
  tokens.
- Security: closed a gap where the closed UIA/Root/Child/Worker spawn matrix
  was implemented but never actually consulted by the runtime, so a spawn
  request's organizational role was not checked; added an integration test.
  Added `#[serde(deny_unknown_fields)]` across configuration and wire-payload
  structs that deserialize untrusted input, closing a route for silent
  authority injection via extra keys. Removed two redundant, unnecessary
  `unsafe impl Send/Sync` blocks.
- TUI input hardening: grapheme-cluster-aware cursor movement (fixes
  corruption with combining diacritics and ZWJ emoji), display-width-aware
  line wrapping for wide glyphs, a stricter history-recall trigger, CRLF
  normalization on paste, and a taller, dynamically sized composer.

### [0.2.0] — 2026-07-16

First tagged pre-release. Highlights:

- **Command surface**: alias support and duplicate-registration detection in
  the operation registry; strict compile-time validation for the
  `FromRawArgs` derive macro.
- **Live session controls**: `/model`, `/provider` and `/effort` now switch
  the running session for real (validated against catalog and credential
  state, atomic fail-close on incompatibility) instead of only affecting the
  next session.
- **`harw` SDK facade crate**: re-exports the canonical public types from
  the runtime crates behind a single `harw::prelude`, with a runnable
  example.
- **Agent DSL**: compiled agent definitions gain a deterministic BLAKE3
  snapshot ID; DSL errors carry a field-path location.
- **Type consolidation**: shared ID newtypes (`ProviderId`, `ModelId`, ...)
  gained ergonomic string-like trait impls, replacing ad hoc `String`
  aliases across the model catalog.
- **Fixes**: an argument-tokenization bug that dropped the second token in
  multi-word subcommands; a long-lived session controller replacing a
  fresh-per-command one that was resetting state; a chat-turn failure (for
  example an expired credential) no longer hard-crashes the TUI process.
- **Known unstable** in this milestone: the fluent `Harness::builder()` API,
  a typed parent-child return pipeline, full IR-to-runtime consumption, the
  goal graph, `DurableJobRunner`'s CLI wiring, and the memory cognition
  loop's end-to-end wiring.

See `docs/migration/0.1.0-to-0.2.0.md` for the upgrade notes from the
previous, untagged state.
