# Changelog

All notable changes to this workspace are recorded here. Format follows
[Keep a Changelog](https://keepachangelog.com/) and this project uses
Semantic Versioning within the 0.x pre-release range.

## [Unreleased]

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
