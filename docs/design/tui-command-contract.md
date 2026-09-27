# Harwness TUI Interaction Contract

> Status: partially implemented · Last reviewed: 2026-09-24

Owning crates: `harw-tui`, `harw-config`, `harw-types`, `harw-ops`
Scope: the typed contract for slash commands, prefix shortcuts, keybindings, and the
command registry architecture that the Harwness TUI (and, where noted, channel
front-ends) must implement. This document defines the *contract*, not the
implementation; a handful of type sketches are included to keep the contract
compilable-shaped, but no crate wiring is performed here.

This document was authored for Harwness. It takes structural inspiration from
prior art surveyed under `inspirations/openclaw` and Hermes's
`reference/slash-commands.md`, but every name, type, grammar rule, and prefix
semantic below is Harwness-original and must not be assumed compatible with
either source.

> **Current state**: §§1–7 are the original target design. What the code
> actually provides today — the command inventory (operations, TUI-local
> commands, fallback specs, planned commands), the `#`/`@` prefixes, the
> F1/F5–F9 key bindings, `/models`, `/mode default`, and the CLI flags
> `--mode`/`--approval`/`--model` — is authoritative in **§8**. Models per
> role, mode vs. approval, and startup precedence are described in
> `tui-roles-models-modes.md`. On any conflict, §8 wins.

---

## 1. Formal Command Grammar

### 1.1 Lexical shape

A command line is exactly one of:

```
"/" CommandName [ArgToken...]
```

where `CommandName` is `[a-z][a-z0-9-]*` (kebab-case, max 32 chars), and
`ArgToken` is either:

- a bare positional token (whitespace-delimited, quotable with `"…"` or `'…'`,
  backslash-escapes `\"`, `\\`, `\ `),
- a `--flag` / `--flag=value` long option,
- a `-f` short option (boolean flags only; no bundling),
- a typed sigil-prefixed reference (see §1.3), which the parser resolves to a
  domain type before the command handler ever sees it.

A colon after the command name (`/work: approve W-104`) is accepted and
stripped, matching the "optional separator" convention used by both surveyed
systems, purely for muscle-memory reasons — not treated as meaningful syntax.

### 1.2 CommandSpec shape (conceptual, typed)

Every command in the inventory (§2) is described by one `CommandSpec` value:

| Field | Type | Meaning |
|---|---|---|
| `name` | `CommandName` | canonical kebab-case name, no leading `/` |
| `aliases` | `&'static [&'static str]` | alternate invocation tokens |
| `args` | `&'static [ArgSpec]` | ordered/typed argument descriptors |
| `scope` | `CommandScope` | `TuiOnly \| ChannelParity \| ChannelReduced` |
| `permission` | `PermissionTier` | minimum tier required to invoke |
| `output` | `OutputSurface` | `Inline \| Panel \| Pager \| Toast` |
| `domain` | `CommandDomain` | grouping used for §2 and completion |

`ArgSpec` carries a `name`, an `ArgType` (see §1.3), `required: bool`, and an
optional `default`. Argument *order* is fixed per command; flags may appear in
any position after positionals.

### 1.3 Typed argument references

Harwness commands never accept "just a string" where a domain identifier is
meant. The parser resolves sigil-prefixed or shape-matched tokens into these
newtypes (defined in `harw-types`, already home to the crate's core domain
structs):

| Type | Surface form | Example | Resolved by |
|---|---|---|---|
| `SessionKey` | `s:<ulid>` or bare ULID | `s:01J8…` | session store lookup |
| `WorkId` | `w-<n>` | `w-104` | work-graph node lookup |
| `ProcessId` | `p:<pid>` or `#<slot>` | `p:88213`, `#3` | process table lookup |
| `AgentRef` | `@<agent-name>` or `@<session-key>/<child-index>` | `@planner`, `@s:01J8…/2` | topology lookup |
| `ProviderRef` | bare name | `anthropic`, `openrouter` | provider registry |
| `ToolRef` | bare name, optionally `pkg::tool` | `fs::read_file` | tool catalog |
| `SkillRef` | bare name | `rust-error-designer` | skill catalog |
| `McpServerRef` | bare name | `crew-chat` | MCP registry |
| `ChannelRef` | `<provider>:<binding-id>` | `telegram:ops-room` | channel binder |
| `PolicyRef` | dotted path | `sandbox.network.default` | policy tree |
| `Duration` | `\d+[smhd]` | `30m`, `2h` | free-standing parser, not a lookup |
| `Glob` / `Path` | shell-glob or path literal | `src/**/*.rs` | filesystem, no lookup |

A token that looks like a typed reference but fails to resolve produces a
structured `CommandError::UnresolvedRef { arg, kind, input }` rather than
falling through to a generic parse error — this is what powers "did you mean
`w-104`?" style completions.

### 1.4 Scopes

- **TuiOnly** — command manipulates local terminal/session state that has no
  meaningful channel analogue (pane focus, keybinding help, pager scrollback).
- **ChannelParity** — the command is fully available, with identical
  semantics and argument grammar, when invoked from a bound channel (e.g.
  Telegram) as a `/`-prefixed message.
- **ChannelReduced** — available from channels, but with a narrower argument
  surface or read-only projection (e.g. `/ps` on a channel lists but cannot
  page interactively).

### 1.5 Permission tiers

```
PermissionTier::Observer   // read-only: status, logs, list views
PermissionTier::Operator   // can drive work: approve/deny/retry/cancel own work
PermissionTier::Maintainer // can mutate config/policy/catalog, bind channels
PermissionTier::Owner      // sandbox/policy root changes, provider credential mgmt
```

Tiers are strictly ordered (`Observer < Operator < Maintainer < Owner`) and a
`CommandSpec.permission` names the *minimum* tier. Enforcement is centralized
in the registry dispatch path (§5), never re-implemented per command.

### 1.6 Output surfaces

- **Inline** — a short result rendered directly in the transcript/log pane.
- **Panel** — opens or updates a persistent side/bottom panel (e.g. the work
  graph panel), remains interactive.
- **Pager** — full-screen scrollable buffer for long output (logs, diffs),
  dismissed with `q` or `Esc`.
- **Toast** — a transient one-line acknowledgement (e.g. "cancelled w-104"),
  auto-dismisses.

---

## 2. Full Slash-Command Inventory

Legend for **Parity**: `Y` = ChannelParity, `R` = ChannelReduced, `-` = TuiOnly.
Legend for **Tier**: `Obs` `Op` `Maint` `Own`.

### 2.1 Session lifecycle

| Command | Args | Description | Tier | Parity |
|---|---|---|---|---|
| `/new` | `[--provider=ProviderRef] [--template=name]` | Start a fresh session, optionally pinned to a provider/template | Op | Y |
| `/resume` | `<SessionKey>` | Reattach to an existing session by key | Op | Y |
| `/rename` | `<SessionKey?> <title>` | Rename current or given session | Op | Y |
| `/compact` | `[--keep=Duration]` | Summarize and shrink session context, keeping recent window | Op | R |
| `/rewind` | `<n \| --to=turn-id>` | Roll session state back N turns or to a marked turn | Op | Y |
| `/fork` | `[--from=turn-id]` | Branch a new session from current or a past turn | Op | Y |
| `/archive` | `<SessionKey>` | Move a session out of the active list | Op | Y |
| `/sessions` | `[--all] [--agent=AgentRef]` | List sessions, optionally across agents | Obs | Y |
| `/export` | `[--format md\|markdown\|json] [--tools\|--no-tools] [--reasoning-summary] [--file <path>] [--max-chars <n>]` | Export a session transcript; `--max-chars <n>` caps the total rendered export length at `n` Unicode characters (whole export, not per entry) — implemented in `harw-ops/src/export.rs` (`ExportArgs::max_chars`) and consumed by `harw_tui::export::ExportOptions::max_chars`. **Current state**: a readable start date (`jiff`, RFC-3339 style instead of Unix seconds); tool arguments/results render as nested pretty JSON or as a ` ```text ` block instead of one giant line, each entry capped at `ExportOptions.max_chars_per_entry` (default 4000 characters, on top of the global `--max-chars` cap); headings inside user/assistant text are demoted by two levels; tool entries (ToolCall/ToolResult/Reasoning) are grouped under "## harw" rather than "## You"; if copying to the clipboard fails, `/export` writes the file instead (`default_export_path`) and reports the path — under tmux the OSC-52 output is additionally wrapped in a DCS passthrough envelope — see `interaction-contract.md` §2.6.7 | Op | R |

### 2.2 Agent & topology

| Command | Args | Description | Tier | Parity |
|---|---|---|---|---|
| `/agent` | `[AgentRef] [--tree] [--history]` | Show agent/session history and child topology; bare form shows current agent | Obs | Y |
| `/spawn` | `<template> [--parent=AgentRef] [--budget=Duration]` | Spawn a child agent under the current or given parent | Op | R |
| `/adopt` | `<AgentRef> <template>` | Attach an existing background process as a tracked child agent | Maint | - |
| `/whoami` | — | Show the calling identity, session key, and effective permission tier | Obs | Y |
| `/mention` | `@AgentRef <message>` | Route a message directly to a specific agent in the topology (see §3.6) | Op | Y |

### 2.3 Work governance

| Command | Args | Description | Tier | Parity |
|---|---|---|---|---|
| `/work` | `[WorkId] [--graph] [--mine] [--blocked]` | Inspect the run/work graph: dependencies, leases, retries, reviews, approvals | Obs | Y |
| `/approve` | `<WorkId> [--note=text]` | Approve a work item pending review | Op | Y |
| `/deny` | `<WorkId> --reason=text` | Deny a work item pending review, reason required | Op | Y |
| `/cancel` | `<WorkId> [--force]` | Cancel a running or queued work item | Op | Y |
| `/retry` | `<WorkId> [--from-step=n]` | Retry a failed work item, optionally from a specific step | Op | Y |
| `/lease` | `<WorkId> [--release \| --extend=Duration]` | Inspect or manage a work item's execution lease | Maint | R |
| `/review` | `<WorkId>` | Open the review surface (diff/plan) for a work item awaiting approval | Op | Y |
| `/priority` | `<WorkId> <n>` | Change scheduling priority of a queued work item | Maint | R |
| `/depends` | `<WorkId> --on=WorkId` | Add or inspect an explicit dependency edge | Maint | R |

### 2.4 Execution (background processes)

| Command | Args | Description | Tier | Parity |
|---|---|---|---|---|
| `/ps` | `[--agent=AgentRef] [--mine]` | List background execution processes | Obs | R |
| `/kill` | `<ProcessId> [--signal=term\|kill]` | Terminate a background process | Op | R |
| `/logs` | `<ProcessId \| WorkId> [--follow] [--since=Duration]` | View process/work logs | Obs | R (no `--follow` on channels) |
| `/attach` | `<ProcessId>` | Attach the TUI to a running process's live output pane | Obs | - |
| `/signal` | `<ProcessId> <name>` | Send an arbitrary POSIX signal | Maint | - |

### 2.5 Catalog & configuration

| Command | Args | Description | Tier | Parity |
|---|---|---|---|---|
| `/doctor` | `[--fix] [--section=catalog\|config\|provider\|mcp\|sandbox\|policy]` | Health check across catalog/config/provider/MCP/sandbox/policy | Obs (plain), Maint (`--fix`) | Y |
| `/model` | `show \| list \| switch <id>` | Inspect the active model, or atomically switch provider+model together (implemented grammar; see `interaction-contract.md` §2.6.1 for the full current-state table, including the `/uia-model`, `/uia-worker-model`, `/effort`, `/uia-effort`, `/provider-concurrency` siblings) | Op | Y |
| `/models` | `[show \| set <role> <target> \| reset <role> \| pick <role> \| worker [<role\|all> <uia\|target>]]` | Show/set/reset models **per role** (12 roles, see `tui-roles-models-modes.md`); takes effect **from the next session** — only `/model` switches live. `pick` opens the role's model picker locally in the TUI. Details in §8.2 | Op | - |
| `/mode` | `[show \| <mode> \| default <mode>]` | Show the interaction mode (`chat\|plan\|explore\|work\|shell`), request it for the running session (takes effect at the next turn boundary), or persist it as `[mode] default` (new sessions). Bare `/mode` opens the mode/approval picker in the TUI (F7) | Op | - |
| `/provider` | `show \| list \| test` | Inspect provider status and credentials only — **`/provider switch` no longer exists**; a provider (and model) switch is exclusively driven by `/model switch <id>` (atomic, works even across providers) | Op | R |
| `/skills` | `[SkillRef] [--search=text] [--install]` | Browse, search, or install skills | Op (browse), Maint (`--install`) | R |
| `/tools` | `[ToolRef] [--enable \| --disable]` | Show or toggle tool availability for the session | Op | R |
| `/mcp` | `[McpServerRef] [--add \| --remove \| --status]` | Inspect or manage MCP server bindings | Maint | - |
| `/config` | `<path> [value]` | Read or write a config key on disk | Maint | - |
| `/policy` | `<PolicyRef> [value]` | Inspect or set a policy value | Maint (read: Op) | - |
| `/sandbox` | `[--status] [--profile=name]` | Inspect or switch the active sandbox profile | Maint | - |
| `/sandbox-lease` | `[status \| revoke]` | Host permission for `shell.exec`: `status` shows the active grant (session/single-call/off), `revoke` ends an active session grant immediately (`busy = "immediate"`). A session grant has no expiry and only the user ends it — `Ctrl+H` or this typed `revoke`; the model tool cannot revoke; the grant itself is not requested through this command but through the identically named model tool `sandbox-lease` (argument `reason`) and confirmed in the `HostPermitDialog` — see `interaction-contract.md` §2.6 and `mediated-process-execution.md` | Maint | - |
| `/plugins` | `[--list \| --enable=name \| --disable=name]` | Manage extension-api plugins | Maint | - |

### 2.6 Knowledge surfaces

Current grammar (from `harw-ops/src/{memory,diary,dream,palace,workbench,kanban,learn}.rs`
and `harw-ops/src/matrix/mod.rs`). The tier per `OperationMeta` is `operator`
for all of them. The full table per subcommand (tier, visibility, busy
class, notes) is in `interaction-contract.md` §2.2.

| Command | Current grammar | Tier | Parity |
|---|---|---|---|
| `/memory` | `list \| stats \| recall <keyword…> \| record <text…> [--project\|--global] \| forget <name> \| promote <fact-id> [--project\|--global] [--slug <slug>] \| maintain \| consolidate [--project\|--global]` \| topics | Op | Y |
| `/diary` | `today \| show [agent] [--date=YYYY-MM-DD \| --from=YYYY-MM-DD [--to=YYYY-MM-DD]] \| search <text> [--agent=<id>] [--from=…] [--to=…] \| note <text>`; `#text` is sugar for `note` (§3) \| agents | Op | R |
| `/dream` | `list \| show <id> \| run \| status \| review [<id>] \| review <id> accept\|reject <proposal-id> [reason…]` | Op | R |
| `/palace` | `list \| show <id> \| search <query> [--max-hops=n] [--max=n] \| promote <topic> \| supersede <old> <new> [--confirm] \| edit <id> <text…> [--confirm] \| link <a> <b> [--confirm]` | Op | Y |
| `/workbench` | `show \| pin <path> [note] \| unpin <path> \| note <text> \| note edit <n> <text> \| note rm <n> \| hypothesis add\|confirm\|reject <text> \| retention [keep\|<days>d]`; every subcommand takes `[--scope=session\|project\|project:<slug>]` | Op | R |
| `/kanban` | `[--board=<id>] show [card] \| list [--all] \| boards \| add\|create <title> … \| edit <card> <text…> \| comment <card> <text…> \| evidence <card> <path\|url> \| approve <card> [note…] \| reject <card> [reason…] \| move <card> <todo\|ready\|running\|done\|blocked\|archived> \| todo\|ready\|claim\|unblock\|done\|archive <card> \| block <card> <reason>` | Op | Y |
| `/learn` | `[scan] \| note <text…> [--target memory\|skill\|agent] \| list [--all] \| show <id> \| accept <id> \| reject <id> [reason…]` — only suggests, never applies on its own | Op | R |
| `/matrix` | `show \| list \| replay [--seed N] \| compare <run> <run> […]`; panel via F9. Read-only — see §7 of `matrix-game.md` for why `start`/`step`/`auto`/`inject`/`override`/`veto`/`reveal`/`fork`/`end` are no longer part of this command's grammar (game control now goes through the `matrix-game-master` orchestrator) | Op | R |

Visibility of `/workbench`: the operation carries exactly one visibility,
`channel_reduced`. `pin`/`unpin` and the bare panel form are nonetheless
only meaningful locally (path resolution against the TUI's working
directory or the panel); `note` and `hypothesis` are the forms actually
usable in reduced form. §6 is aligned accordingly.

### 2.7 Channels

| Command | Args | Description | Tier | Parity |
|---|---|---|---|---|
| `/bind` | `<ChannelRef>` | Bind the current session to a channel (e.g. a Telegram chat) | Maint | - |
| `/unbind` | `<ChannelRef>` | Remove a channel binding | Maint | - |
| `/channels` | `[--status]` | List active channel bindings and health | Obs | Y |
| `/broadcast` | `<message> [--channels=ChannelRef,...]` | Send a message to one or more bound channels | Op | - |

### 2.8 Miscellaneous

| Command | Args | Description | Tier | Parity |
|---|---|---|---|---|
| `/help` | `[CommandName]` | Show command help, or full command index | Obs | Y |
| `/status` | — | Show session, provider, sandbox, and work-queue summary | Obs | Y |
| `/quit` | `[--force]` | Exit the TUI (or end a bound channel session) | Op | Y |
| `/export` | *(see §2.1)* | | | |
| `/feedback` | `<message>` | Send structured feedback/bug report to the maintainers | Obs | Y |
| `/keys` | `[--chord]` | Show current keybinding map (§4) | Obs | - |
| `/clear` | — | Clear the visible transcript pane (does not touch session state) | Obs | - |
| `/theme` | `<name>` | Switch TUI color theme | Obs | - |

**Inventory totals**: 9 session-lifecycle, 5 agent/topology, 9 work-governance,
5 execution, 10 catalog/config, 6 knowledge-surface (reserved), 4 channel, 8
misc = **56 commands** (counting `/export` once).

---

## 3. Prefix Shortcut System

Harwness defines five original, non-overlapping input prefixes. Exactly one
prefix rule applies per input line; a line with no recognized prefix is
ordinary chat input routed to the active agent.

| Prefix | Name | Behavior |
|---|---|---|
| `/` | **Command** | Structured command per §1–§2. |
| `!` | **Shell** | Runs the remainder as a host shell command through the sandboxed execution surface; output streams to an Inline or Pager surface depending on length. Requires `Operator` tier and an enabled `commands.shell` capability flag. **Current state**: `!` always runs on the host (`harw_tool_shell::run_operator_command`): real `HOME`, cwd is the project root, generous rlimits, timeout `[shell] max_timeout_secs`. There is no approval prompt, but there is an audit event `shell.operator_exec`; `sudo`/`doas`/`pkexec` are rejected. A typed `!` automatically gets a trailing space. The command also runs immediately during a turn (busy class Immediate); the result reaches the chat only after the turn ends. This is different from the model's own `shell.exec`, which stays sandboxed inside the project sandbox with a minimal PATH and no host access, independent of any active `sandbox-lease` — see `mediated-process-execution.md`. |
| `!!` | **Shell-repeat** | Re-runs the last `!` shell command verbatim. Bare `!!` with no trailing text; any trailing text after `!!` is an error (`CommandError::TrailingTokens`), to avoid ambiguity with `!! <new cmd>` meaning something else. |
| `#` | **Note** (current) | `#text` becomes `/diary note text` if the `diary` operation is registered, otherwise `/memory record text`. No model turn; runs through the same operation path (a single audit trail). Details in §8.3. |
| `@` | **Mention** (current) | `@role text`: **a delegation request to the UIA** — the text goes to the UIA as ordinary chat, prefixed with "[Delegation request for role "role"]"; the UIA decides whether to delegate (no direct routing). `@path` attaches a project file as a `<file path="…">…</file>` block (64 KiB per file, 256 KiB total, at most 8 files, a blocklist). A known role takes precedence; a same-named file is reached via `./name`. Details in §8.3. |

Two forms deliberately **not** used, to keep the prefix set minimal and
unambiguous: no `?` prefix (help is `/help` or the dedicated `Help` key, §4)
and no bare `$` prefix (reserved, unassigned — see open question in §7).

### 3.1 Escaping

A literal leading prefix character that should not trigger prefix handling is
escaped with a leading backslash: `\/not-a-command`, `\!not-shell`, `\#not-a-note`,
`\@not-a-mention`. The backslash is stripped and the remainder is treated as
plain chat text.

### 3.2 Prefix behavior in channels vs. TUI

- **Command (`/`)**: full parity in `ChannelParity`/`ChannelReduced` commands;
  `TuiOnly` commands typed into a channel return a structured
  `CommandError::TuiOnlyCommand` reply rather than silently failing.
- **Shell (`!`)**: TUI-only by default. Channel exposure requires an explicit
  per-channel opt-in (`channel.<id>.allow_shell = true`) plus `Operator` tier,
  because shell access from a remote messaging surface is a materially larger
  attack surface than from a local terminal.
- **Note (`#`)**: available on both surfaces; on channels it is the only way
  to add a diary entry without triggering an agent turn.
- **Mention (`@`)**: available on both surfaces. On channels, `@AgentRef` is
  additionally recognized as a native mention if the channel supports its own
  `@`-mention UI (e.g. Telegram reply-to); Harwness's parser still owns
  resolution so behavior is identical either way.

---

## 4. Keybinding Model

Harwness TUI keybindings follow a ratatui-style global/view-local split with
optional two-key chords, entirely configurable via TOML in `harw-config`.

> **Current state**: the actual default bindings (`Ctrl+K` = delete line,
> `Shift+Tab` = approval cycle, F1 help, F5 workbench, F6 kanban, F7 mode
> picker, F8 models, among others) and the real file format
> (`[tui].keybindings_file`, a flat table `action = "chord"`) are in §8.4.
> The tables below and the TOML schema in §4.4 are the original design.

### 4.1 Global keys (active in every view)

| Key | Action |
|---|---|
| `Ctrl+C` (×2 within 2s) | Idle: quit (double-tap guard against accidental exit). Busy (during a running turn): **hard interrupt** — first press cancels the running model call, shell/sandbox subprocesses (SIGKILL), and child agents, and arms the same double-tap state as idle; a second press within the window quits. Model-call and subprocess cancellation (`harw-core/src/turn_loop.rs`, `harw-tool-shell/src/exec.rs`), child-agent cancellation wiring (`register_parent_cancel_token` called from `run_turn_streaming` in `harw-tui/src/app.rs`), and the unified idle/busy double-tap state (`pending_quit`/`hard_quit_requested`, `QuitArm`/`QUIT_HINT_WINDOW`) are all implemented — see `interaction-contract.md` §2.6.4. Interrupting no longer drops the queue — messages and commands submitted during the turn stay queued and are delivered right after the interrupt (status line: "sending queue"), including when an approval or host-permit dialog was open. |
| `Ctrl+K` | Open command palette (fuzzy `/` command search) |
| `Ctrl+L` | Clear visible transcript pane |
| `F1` | Toggle help overlay (keybinding + command cheat sheet) |
| `Tab` / `Shift+Tab` | Cycle focus between panels |
| `Ctrl+P` / `Ctrl+N` | Previous / next session in session switcher |
| `Esc` | Dismiss active popup/pager/panel/toast first (composer text stays). Busy with nothing open: interrupts the running turn like a first `Ctrl+C`, keeps the queue, and **never** arms quit. Idle with nothing open: clear input line |

### 4.2 View-local keys

**Transcript view**
| Key | Action |
|---|---|
| `Up` / `Down` | Scroll transcript by line |
| `PgUp` / `PgDn` | Scroll transcript by page |
| `g` `g` (chord) | Jump to top |
| `G` | Jump to bottom (follow mode) |

**Work graph panel**
| Key | Action |
|---|---|
| `j` / `k` | Move selection down/up |
| `Enter` | Open `/review` for selected work item |
| `a` | `/approve` selected item |
| `d` | `/deny` selected item (prompts for reason) |
| `r` | `/retry` selected item |
| `c` | `/cancel` selected item |

**Process list panel**
| Key | Action |
|---|---|
| `j` / `k` | Move selection |
| `Enter` | `/attach` to selected process |
| `x` | `/kill` selected process |
| `l` | Open `/logs` pager for selected process |

**Pager**
| Key | Action |
|---|---|
| `/` | Search within pager buffer (not a command — pager-local search mode) |
| `n` / `N` | Next / previous search match |
| `q` | Close pager |

### 4.3 Chord support

Chords are two-key sequences within a configurable timeout (default 600 ms),
declared as `KeyBinding::Chord([Key, Key])`. Only single-level chords are
supported (no 3+ key sequences) to keep discoverability high; `/keys --chord`
lists all registered chords.

### 4.4 Typed `KeyBinding` config schema (TOML, `harw-config`)

```toml
# ~/.config/harwness/keybindings.toml

[global]
quit = { keys = ["ctrl+c", "ctrl+c"], within_ms = 2000 }
command-palette = { keys = ["ctrl+k"] }
clear-transcript = { keys = ["ctrl+l"] }
help-overlay = { keys = ["f1"] }
focus-next = { keys = ["tab"] }
focus-prev = { keys = ["shift+tab"] }
session-prev = { keys = ["ctrl+p"] }
session-next = { keys = ["ctrl+n"] }
dismiss = { keys = ["esc"] }

[view.transcript]
scroll-up = { keys = ["up"] }
scroll-down = { keys = ["down"] }
page-up = { keys = ["pgup"] }
page-down = { keys = ["pgdn"] }
jump-top = { keys = ["g", "g"], within_ms = 600 }
jump-bottom = { keys = ["shift+g"] }

[view.work-graph]
select-down = { keys = ["j"] }
select-up = { keys = ["k"] }
open-review = { keys = ["enter"] }
approve = { keys = ["a"] }
deny = { keys = ["d"] }
retry = { keys = ["r"] }
cancel = { keys = ["c"] }

[view.process-list]
select-down = { keys = ["j"] }
select-up = { keys = ["k"] }
attach = { keys = ["enter"] }
kill = { keys = ["x"] }
open-logs = { keys = ["l"] }

[view.pager]
search = { keys = ["/"] }
search-next = { keys = ["n"] }
search-prev = { keys = ["shift+n"] }
close = { keys = ["q"] }
```

Corresponding Rust types (sketch, `harw-config::keybindings`):

```rust
/// One configured key or key-chord binding for a single logical action.
pub struct KeyBinding {
    pub keys: Vec<KeyToken>,
    pub within_ms: Option<u64>, // required when keys.len() > 1
}

/// A single key press in canonical lowercase-modifier-prefixed form,
/// e.g. "ctrl+k", "shift+g", "g".
pub struct KeyToken(String);

/// Global keybindings, always active regardless of focused view.
pub struct GlobalKeyMap {
    pub quit: KeyBinding,
    pub command_palette: KeyBinding,
    pub clear_transcript: KeyBinding,
    pub help_overlay: KeyBinding,
    pub focus_next: KeyBinding,
    pub focus_prev: KeyBinding,
    pub session_prev: KeyBinding,
    pub session_next: KeyBinding,
    pub dismiss: KeyBinding,
}

/// Per-view keybinding tables, keyed by view identifier.
pub struct ViewKeyMap {
    pub transcript: TranscriptKeys,
    pub work_graph: WorkGraphKeys,
    pub process_list: ProcessListKeys,
    pub pager: PagerKeys,
}

/// Top-level keybinding configuration loaded from keybindings.toml.
pub struct KeybindingConfig {
    pub global: GlobalKeyMap,
    pub view: ViewKeyMap,
}
```

---

## 5. Command Registry Architecture (Rust sketch)

Lives in the future `harw-tui` crate, depending on `harw-types` (for the typed
arg references), `harw-protocol` (for dispatching resolved commands into
session/work/agent operations), and `harw-config` (for permission tier and
capability flags).

### 5.1 Core types

```rust
/// Canonical, validated command name (kebab-case, no leading slash).
pub struct CommandName(String);

/// One argument's type contract, used for parsing and completion.
pub enum ArgType {
    Session,
    Work,
    Process,
    Agent,
    Provider,
    Tool,
    Skill,
    McpServer,
    Channel,
    Policy,
    Duration,
    Path,
    Text,
    Flag,
}

/// Declared shape of one positional or flag argument.
pub struct ArgSpec {
    pub name: &'static str,
    pub ty: ArgType,
    pub required: bool,
    pub default: Option<&'static str>,
}

/// Where a command's invocation is legal.
pub enum CommandScope {
    TuiOnly,
    ChannelParity,
    ChannelReduced,
}

/// Where a command's result is rendered.
pub enum OutputSurface {
    Inline,
    Panel,
    Pager,
    Toast,
}

/// Minimum caller permission required to invoke a command.
#[derive(PartialEq, PartialOrd, Eq, Ord)]
pub enum PermissionTier {
    Observer,
    Operator,
    Maintainer,
    Owner,
}

/// Grouping used for help text and completion sectioning.
pub enum CommandDomain {
    SessionLifecycle,
    AgentTopology,
    WorkGovernance,
    Execution,
    CatalogConfig,
    Knowledge,
    Channels,
    Identity,
    Network,
    Security,
    Crypto,
    Misc,
}

/// Full static description of one registrable command.
pub struct CommandSpec {
    pub name: CommandName,
    pub aliases: &'static [&'static str],
    pub args: &'static [ArgSpec],
    pub scope: CommandScope,
    pub permission: PermissionTier,
    pub output: OutputSurface,
    pub domain: CommandDomain,
}
```

### 5.2 Registry

```rust
/// Owns the full set of registered CommandSpecs plus their handlers.
/// Built once at TUI startup from a static table; extension crates may
/// register additional specs through `harw-extension-api` before the
/// registry is sealed.
pub struct CommandRegistry {
    specs: Vec<CommandSpec>,
    handlers: Vec<Box<dyn CommandHandler>>, // heterogeneous handlers -> trait object is appropriate here
}

/// Behavior every command handler implements.
pub trait CommandHandler {
    /// Executes the resolved command against session/work/agent state.
    fn run(&self, ctx: &CommandContext, args: ResolvedArgs) -> Result<CommandOutcome>;
}

/// Caller identity, permission tier, and current session/agent context
/// available to every handler.
pub struct CommandContext<'a> {
    pub caller_tier: PermissionTier,
    pub session: &'a SessionKey,
    pub agent: &'a AgentRef,
    pub surface: InvocationSurface, // Tui | Channel(ChannelRef)
}

/// Parsed and type-resolved arguments handed to a handler.
pub struct ResolvedArgs {
    pub positionals: Vec<ResolvedArg>,
    pub flags: std::collections::HashMap<String, ResolvedArg>,
}

/// One fully resolved argument value (already looked up if it was a
/// typed reference).
pub enum ResolvedArg {
    Session(SessionKey),
    Work(WorkId),
    Process(ProcessId),
    Agent(AgentRef),
    Provider(ProviderRef),
    Tool(ToolRef),
    Skill(SkillRef),
    McpServer(McpServerRef),
    Channel(ChannelRef),
    Policy(PolicyRef),
    Duration(std::time::Duration),
    Path(std::path::PathBuf),
    Text(String),
    Flag(bool),
}

/// What a handler produces; the registry routes this to the declared
/// OutputSurface for rendering.
pub enum CommandOutcome {
    Rendered(RenderPayload),
    Deferred(WorkId), // long-running: handler enqueued work, UI shows a toast + panel link
}
```

### 5.3 Parsing and completion

```rust
/// Tokenizes one input line into a prefix-classified `Invocation`.
pub fn classify_input(line: &str) -> Invocation { /* … */ }

pub enum Invocation {
    Command { name: String, raw_args: Vec<String> },
    Shell(String),
    ShellRepeat,
    Note(String),
    Mention { target: String, body: String },
    Chat(String),
}

impl CommandRegistry {
    /// Looks up a CommandSpec by name or alias.
    pub fn find(&self, name: &str) -> Option<&CommandSpec> { /* … */ }

    /// Parses raw arg tokens against a CommandSpec's ArgSpec list,
    /// resolving typed references via the provided lookup context.
    /// Returns CommandError::UnresolvedRef / ::MissingArg / ::TrailingTokens
    /// on failure rather than a generic parse error.
    pub fn parse_args(
        &self,
        spec: &CommandSpec,
        raw: &[String],
        lookup: &dyn ReferenceResolver,
    ) -> Result<ResolvedArgs> { /* … */ }

    /// Produces ranked completion candidates for a partial input line,
    /// used by the command palette (Ctrl+K) and inline tab-completion.
    pub fn complete(&self, partial: &str, ctx: &CommandContext) -> Vec<Completion> { /* … */ }

    /// Dispatches a fully classified, permission-checked Invocation.
    /// Enforces PermissionTier and CommandScope centrally — individual
    /// handlers never re-check permission.
    pub fn dispatch(&self, ctx: &CommandContext, invocation: Invocation) -> Result<CommandOutcome> { /* … */ }
}

/// Resolves surface tokens (e.g. "w-104", "@planner") into domain types
/// by querying the session store / work graph / topology / catalogs.
/// A trait so tests can supply a stub resolver instead of touching real
/// stores (per the crate's no-network/no-filesystem test policy).
pub trait ReferenceResolver {
    fn resolve_session(&self, token: &str) -> Option<SessionKey>;
    fn resolve_work(&self, token: &str) -> Option<WorkId>;
    fn resolve_process(&self, token: &str) -> Option<ProcessId>;
    fn resolve_agent(&self, token: &str) -> Option<AgentRef>;
    // … one method per ArgType that requires a lookup
}
```

### 5.4 Error surface

Consistent with the workspace-wide ban on `anyhow`/`thiserror`, command errors
are a hand-written enum living alongside the registry (or folded into a
crate-central `harw-tui::Error` once the crate exists):

```rust
pub enum CommandError {
    UnknownCommand { input: String, suggestion: Option<String> },
    MissingArg { command: String, arg: &'static str },
    TrailingTokens { command: String, extra: Vec<String> },
    UnresolvedRef { arg: &'static str, kind: ArgType, input: String },
    PermissionDenied { command: String, required: PermissionTier, actual: PermissionTier },
    TuiOnlyCommand { command: String },
    ScopeReduced { command: String, reason: &'static str },
    HandlerFailed { command: String, source: Box<dyn std::error::Error> },
}
```

`Display` renders a human sentence per variant (no internal jargon);
`Debug` delegates to `Display`; `impl std::error::Error for CommandError`
with `source()` returning the boxed cause for `HandlerFailed`. `From` impls
are intentionally *not* blanket — each foreign error a handler can produce
gets an explicit conversion at the call site, since `CommandError` is a
terminal, presentation-facing type rather than a propagation type.

---

## 6. Channel-Command Parity Table

| Command | TUI | Channel | Reason |
|---|---|---|---|
| `/new`, `/resume`, `/rename`, `/rewind`, `/fork`, `/archive`, `/sessions` | Y | Y | Pure session-store operations, no local-terminal dependency |
| `/compact` | Y | Reduced (no interactive keep-window picker) | Full form needs a scroll preview only the TUI renders |
| `/export` | Y | Reduced (format flag only, no local file picker) | Channel export streams the file as an attachment instead |
| `/agent`, `/whoami`, `/mention`, `/sessions` | Y | Y | Topology and identity are session-store facts, not terminal facts |
| `/spawn` | Y | Reduced (no `--budget` interactive confirm) | Budget confirmation dialog is TUI-only; channel form requires the flag up front |
| `/adopt` | Y | TUI-only | Adopting a raw OS process onto tracked topology assumes local process visibility |
| `/work`, `/approve`, `/deny`, `/cancel`, `/retry`, `/review` | Y | Y | Governance actions are the primary reason channel binding (e.g. mobile approvals) exists |
| `/lease`, `/priority`, `/depends` | Y | Reduced | Scheduling internals; exposed read-mostly on channels |
| `/ps` | Y | Reduced (snapshot only, no live refresh) | Channels are push-based, not a live TUI redraw loop |
| `/kill` | Y | Reduced (no `--signal` choice, TERM only) | Limits blast radius of a mistyped channel command |
| `/logs` | Y | Reduced (no `--follow`, bounded tail only) | Streaming logs into a chat thread is not viable UX |
| `/attach`, `/signal` | Y | TUI-only | Requires a live local output pane / raw signal delivery |
| `/doctor`, `/model`, `/channels` | Y | Y | Health/status/model selection are safe, high-value remote actions |
| `/provider`, `/skills`, `/tools` | Y | Reduced | Browsing is safe remotely; installs/credential entry require Maintainer+ and stay TUI-first |
| `/mcp`, `/config`, `/policy`, `/sandbox`, `/plugins` | Y | TUI-only | Root-of-trust config surfaces; remote mutation risk outweighs convenience |
| `/memory`, `/kanban`, `/palace` | Y | Y | Read/append operations with clear, bounded blast radius; palace writes on `established` nodes need `--confirm`, kanban workers start only after `/kanban approve` |
| `/diary`, `/workbench`, `/learn`, `/matrix` | Y | Reduced (one visibility per operation; `/workbench pin`/`unpin` and the bare panel are only meaningful locally) | see §2.6 and `interaction-contract.md` §2.2 |
| `/dream` | Y | Reduced | `list`/`show`/`status`/`review` are safe to read remotely; `run` needs a dream launcher, which only interactive entries (TUI, one-shot) provide |
| `/bind`, `/unbind`, `/broadcast` | Y | TUI-only | Channel topology itself must not be mutable from within a channel it controls |
| `/help`, `/status`, `/quit`, `/feedback` | Y | Y | Universal, low-risk |
| `/keys`, `/clear`, `/theme` | Y | TUI-only | Terminal-rendering concerns only |

**Parity summary**: of 56 inventoried commands, 25 are full `ChannelParity`,
17 are `ChannelReduced`, 14 are `TuiOnly`.

### 6.1 Busy availability (immediate, staged, deferred)

Independent of channel parity, every `CommandSpec` also carries a
`busy: BusyAvailability` plus optional per-subcommand overrides
(`busy_subcommands`), both taken from the operation's `#[operation]`
declaration (`busy = "immediate|staged"`,
`busy_subcommands = “show=immediate, -=immediate”`, `-` = bare form). There
are three classes:

- **Immediate** — runs during a running turn as its own task (60 s limit);
  output appears as a system line “<command> (during turn)”.
- **Staged** — runs during the turn but only records the change in the
  session controller/config cell; it takes effect from the next turn
  (`/model` including the bare picker form, `/effort`, `/mode`,
  `/uia-model`, `/uia-worker-model`, `/uia-provider`, `/uia-effort`).
- **DeferredUntilTurnEnd** (default) — queued and run after the turn.

Busy-safe TUI-local intercepts (overlays, model/effort pickers, agent tree,
panels, verbose, system lines, rename) apply immediately; session picker,
`/clear` and rewrite stay deferred. Overlays receive keys during a turn.
There is no `/provider switch`. `/cancel`/`/stop` act on the `JobStore`
(background jobs), not the running turn itself — `Ctrl+C` and `Esc` (§4.1)
interrupt a turn in progress. Messages submitted during a turn are shown in
a “waiting for the next turn” block above the composer until delivered.
Full table and status: `interaction-contract.md` §2.6.3; the CI table test
is `EXPECTED_BUSY_CLASSES` in `harw-tui/src/command_exec.rs`.

### 6.2 Model/provider picker consolidation

The former four separate dialog variants (`ProviderChoice`/`ModelChoice`/
`UiaProviderChoice`/`UiaModelChoice`) are gone. `harw-tui/src/app.rs`'s
`Overlay` enum now has a single `Overlay::ModelSwitch(ModelSwitchPicker)`
variant, backed by the consolidated `ModelSwitchPicker` widget
(`harw-tui/src/model_switch_picker.rs`: provider stage → model stage, `Left`
to go back, `Esc` cancels). Bare `/model`, `/uia-model` and
`/uia-worker-model` (and their argless `switch`) open the picker; for
`/uia-worker-model` the picker skips the provider stage entirely
(`PickerTarget::UiaWorker { fixed_provider }`) since the worker is bound to
the UIA's own provider. `/model switch <id>` with an explicit argument stays
a plain text dispatch. Bare `/provider`/`/uia-provider` no longer open any
picker — they run `show`. A parallel single-stage `Overlay::EffortChoice`
opens for bare `/effort`/`/uia-effort`, listing all six reasoning-effort
levels plus a "provider default (reset)" entry that emits `clear`.

---

## 7. Open Design Questions

1. **`$` prefix** is reserved but unassigned. Candidate uses considered:
   variable/env interpolation in command args (`$SESSION`), or a
   "quick-approve" shorthand for `/approve`. No decision made here —
   flagging for a follow-up design pass.
2. **`/spawn` budget accounting** — how a child agent's token/time budget
   is charged against the parent's is a `harw-job-runtime` concern not yet
   designed; the command surface above assumes a `--budget=Duration` flag
   but the semantics (hard cap vs. soft warning) are undecided.
3. ~~Knowledge-surface commands~~ — done: grammar in §2.6, storage model in
   `knowledge-surfaces.md`. `/dream` is a registered operation
   (`list|show|run|status|review`).
4. **Command versioning across channel native-command registration** (e.g.
   if Harwness later registers native Telegram bot commands) is not
   addressed; this document only defines the internal contract that any
   such native registration would be generated from.
5. **`/broadcast` fan-out ordering and partial-failure reporting** across
   multiple `ChannelRef`s is not specified — needs a decision once
   `harw-catalog`'s channel registry exists.
6. **Chord timeout tunability per-action** vs. one global default — the
   TOML schema in §4.4 allows per-binding `within_ms`, but no design
   guidance yet on when a command author should override the default.

---

## 8. Current state of the command surface (2026-09-24)

This section describes what the code delivers and, on any conflict, wins
over §§1–7. Sources: `harw-ops/src/lib.rs` (`register_all`,
`register_plan_tools`), `harw-tui/src/command_catalog.rs`
(`local_command_specs`, `FALLBACK_COMMANDS`, `PLANNED_COMMANDS`,
`HINT_TABLE`), `harw-tui/src/keybindings.rs`, `harw-tui/src/mention.rs`,
`harw-cli/src/cli/global.rs`.

### 8.1 Where a command comes from

The TUI registry is built from the operations' command adapters
(`CommandRegistry::from_command_adapters`) and then merged with the local
specs (`with_local_specs`). On a name collision, **the operation always
wins**; the local entry is dropped. Every spec carries `summary`, `usage`,
`subcommands` (from `HINT_TABLE`), and `origin: Operation | TuiLocal`; the
`/`-popup and help (F1) display them.

| Origin | Meaning |
|---|---|
| **Operation** | defined in `harw-ops` and registered via `register_all` (45 ops) or, behind `[tools.plan] enabled`, via `register_plan_tools` (7 ops) |
| **TUI-local** | intercepted only in the TUI, no operation behind it (`CommandScope::TuiOnly`) |
| **Fallback** | a TUI spec for a command whose operation is not (yet) registered (`FALLBACK_COMMANDS`: `mode`, `matrix`); automatically drops out once the operation is registered |
| **Planned** | listed only in help (`PLANNED_COMMANDS`), not runnable |

Registration status: `workbench`, `kanban`, `diary`, `palace`, `dream`,
`learn`, `matrix`, and `models` are entered in `register_all`; `mode` and
`matrix` are registered, so their fallback specs have no effect.

### 8.2 Inventory

Columns: tier per `OperationMeta.permission` (Obs/Op/Maint), visibility
(`Y` channel_parity, `R` channel_reduced, `-` tui_only), `busy`
(`immediate` = `Immediate`, `staged` = `Staged`, takes effect from the next
turn; otherwise deferred until the turn ends; details in §6.1).

**Operations (`register_all`)**

| Command | Grammar (short form) | Tier | Parity | busy |
|---|---|---|---|---|
| `/help` | `[command]` | Obs | Y | immediate |
| `/status` | — | Obs | Y | immediate |
| `/quit` | — | Op | - | |
| `/new` | — | Op | Y | |
| `/work` | — (job overview) | Obs | Y | immediate |
| `/ps` | — | Obs | R | immediate |
| `/attach` | — | Op | - | immediate |
| `/stop` | `[job-id]` | Op | Y | immediate |
| `/diff` | — | Obs | Y | immediate |
| `/agent` | `[list \| stop <agent-id> \| budget [agent-id] \| use <name> \| use --clear \| stream <orchestrators\|all\|none> \| bg \| cancel <agent-id>]`; bare opens the agent tree with live values (root "UIA · <name>"); `use` sets the root agent from the next session (profile `active_agent_definition`); `stream`, `bg`, and `cancel` are intercepted locally by the TUI (see below) | Op | Y | immediate (including `stream`/`bg`/`cancel`) |
| `/skills` | `[list \| show <name> \| …]` | Op | R | immediate (bare/`list`/`show`) |
| `/plugins` | — | Maint | - | immediate (except `install`/`activate`/`uninstall`) |
| `/model` | `[show \| list \| switch <model-id>]` — **live** from the next turn | Op | - | immediate (`show`/`list`), otherwise staged |
| `/provider` | `[show \| list \| test]` (no `switch`) | Op | - | immediate (except `test`) |
| `/uia-model` | `[show \| list \| switch <model-id>]` — from the next session | Op | - | immediate (bare/`show`/`list`), otherwise staged |
| `/uia-provider` | `[show \| list \| test]` (no `switch`) | Op | - | immediate (bare/`show`/`list`), `test` deferred |
| `/uia-worker-model` | `[show \| list \| switch <model-id>]` — the older family pin `uia_worker_model`, from the next session; no longer coupled to the UIA provider (a provider comes from the catalog). Per-role: `/models worker` | Op | - | immediate (bare/`show`/`list`), otherwise staged |
| `/effort` | `[show \| minimal \| low \| medium \| high \| xhigh \| max \| clear]` | Op | - | immediate (bare/`show`), otherwise staged |
| `/uia-effort` | like `/effort`, from the next session | Op | - | immediate (bare/`show`), otherwise staged |
| `/permissions` | `[show \| mode <ask\|auto\|full> [--session\|--project\|--global] \| allow <tool> [pattern] [--session\|--project\|--user] \| deny <tool> [pattern] [--session\|--project\|--user] \| rules \| remove <n> \| rm <n> \| log [count]]`; `--user` is an alias for `--global`; `rules` lists the rules with their origin, `log` the most recent auto-mode decisions plus the cap state; allow rules for `ALWAYS_ASK_TOOLS` are rejected | Op | - | immediate (bare/`show`/`mode`/`set`/`rules`/`log`) |
| `/mode` | `[show \| <mode> \| default <mode>]` | Op | - | immediate (bare/`show`), otherwise staged |
| `/compact` | — | Op | Y | |
| `/memory` | see §2.6 | Op | Y | |
| `/workbench` | see §2.6 | Op | R | immediate (bare/`show`) |
| `/kanban` | see §2.6 | Op | Y | immediate (bare/`list`/`show`/`boards`), still deferred for writes |
| `/diary` | see §2.6 | Op | R | immediate (bare/`show`/`today`/`search`) |
| `/palace` | see §2.6 | Op | Y | immediate (bare/`list`/`show`/`search`) |
| `/dream` | see §2.6 | Op | R | immediate (bare/`list`/`show`/`status`), still deferred for writes |
| `/learn` | see §2.6 | Op | R | |
| `/matrix` | see §2.6 | Op | R | immediate (`show`/`list`) |
| `/models` | `[show \| set <role> <target> \| reset <role> \| worker [<role\|all> <uia\|target>]]`; `pick <role>` is TUI-local (opens the picker); `worker` shows or sets the choice per UIA-worker role (`uia` = same as UIA) | Op | - | immediate |
| `/context-proposal` | `list \| view \| accept \| reject` | Op | Y | |
| `/add-workdir` | `<path>` | Op | - | |
| `/export` | `[--format md\|json] [--file <path>] [--tools\|--no-tools] [--reasoning-summary] [--max-chars <n>]` | Op | - | |
| `/usage` | — | Obs | Y | immediate |
| `/bug-report` | — | Op | - | |
| `/approve`, `/deny`, `/cancel` | `<id> …` | Op | Y | immediate |
| `/review` | `<id>` | Obs | Y | immediate |
| `/retry` | `<id>` | Op | Y | |
| `/provider-concurrency` | — | Op | - | immediate |
| `/sandbox-lease` | `[status \| revoke]` | Op | - | immediate |

`approval.pending`/`approval.resolve` have only a web surface, no slash
command.

**Operations behind `[tools.plan] enabled`**: `/plan`, `/goal`, `/explore`,
`/research`, `/research-deps` (with `--generic` or `ecosystem=<x>` for
ecosystem-neutral use through `dependency-researcher`), `/research-web`,
`/analyze` (all Op). `/plan` (bare/`inspect`/`ready`/`waves`/`list`) and
`/goal` (bare/`show`/`check`) run immediately during a turn.

`/plan` has two areas. The plan **files** of plan mode are intercepted
locally by the TUI: `/plan` (turns plan mode on), `/plan show` (the current
or pinned plan), `/plan list` (plans under `.harw/plans`), `/plan open
<name>` (load an earlier plan to keep planning), `/plan edit` (in
`$EDITOR`). The plan **graphs** of the `plan` operation: `/plan plans` (all
graphs in the plan store), `/plan switch <id>` (make another graph active),
`/plan archive <id>` (hide, not delete), `/plan inspect [id]`, `/plan
submit [id]` (opens the confirmation window in the TUI; only after that is
the plan binding and tied to a goal), and `/plan step <id>
<open|running|done|blocked> [evidence]` (`done` only with evidence). On the
model side the graph list is called `plan list`. A confirmed, bound plan
shows "◎ Goal: … · n/m" in the status line.

`/models`: `<role>` is a key from `ModelRole::key` (`uia`, `uia-worker`,
`orchestrator`, `sub-orchestrator`, `worker-simple`, `worker-complex`,
`explorer`, `research`, `compaction`, `title`, `memory`, `dream`; `_`/`-`
and case don't matter, including keys of internal model slots like
`session_title`). `<target>` is a model ID/alias/catalog key, or
`provider/model` (split at the **first** `/`; the prefix must be an
enabled provider). `set uia-worker` rejects models from a provider other
than the UIA's own. `reset` removes the explicit choice (UIA:
`uia_provider`/`uia_model`; internal slots: the whole
`[internal_models.<slot>]` table). No model tool. `data` contract: `show` →
`{"roles":[{"role","label","provider","model","source","effort"}],"live":{"provider","model"}}`.

`/mode default <mode>` writes `[mode] default` to the profile's
`config.toml` (best effort, an error becomes a notice) and doesn't touch
the running session; `/mode <mode>` is staged and applied at the next turn
boundary (the response says `requested`, not `applied`). No model tool.

**TUI-local** (`local_command_specs`, all `tui_only`)

| Command | Effect | Tier | busy |
|---|---|---|---|
| `/tools` | `[on <name> \| off <name> \| reset [name] \| profile <minimal\|coding\|full>]` | Op | |
| `/resume` | `[session-id]`, without an ID opens a picker | Op | |
| `/sessions` | opens the session picker | Obs | |
| `/exit` | quits the TUI (like `/quit`) | Obs | |
| `/clear` | clears the display, the session stays | Obs | |
| `/verbose` | toggles verbose tool display | Obs | immediate |
| `/keys` | shows the keybinding map | Obs | immediate |
| `/whoami` | session, permission, active model | Obs | immediate |
| `/rename` | `<title>` | Op | immediate |
| `/btw` | `<question>` — an off-the-record side question with no tools; doesn't interrupt the agent, lands in neither the transcript nor the history; Esc cancels, 60s timeout | Op | immediate |
| `/plan` | turns plan mode on; `show \| list \| open <name> \| edit` (plan files, see above); a fallback spec while the `plan` operation is absent | Op | immediate (`edit` waits on the terminal) |
| `/agent stream` | `<orchestrators\|all\|none>` — a live stream of child agents into the transcript for this session (default from `[tui] child_stream`) | Obs | immediate |
| `/agent bg` | lists background agents with progress | Obs | immediate |
| `/agent cancel` | `<agent-id>` — cancel your own background agent | Op | immediate |

`/agents` no longer exists; typing it shows a hint ("/agents no longer
exists – use /agent") and is not forwarded. `/btw` exists only in the TUI;
a channel like Telegram answers with a plain refusal.

The fallback specs (`/mode`, `/matrix`, `/plan`) have no effect as long as
their operations are registered; the local `/plan` forms for plan files
are unaffected by that.

Bare forms open views instead of text: `/model`, `/uia-model`,
`/uia-worker-model` → model picker; `/effort`, `/uia-effort` → effort
picker; `/models` → the per-role model view (F8); `/mode` → the mode/
approval picker (F7); `/kanban` → the board (F6); `/workbench` → the
workbench panel (F5); `/palace`, `/dream`, `/diary` → the knowledge
browser (diary by date/agent with search and a range view; palace with
links/backlinks, search, and promote/supersede with confirmation;
dream review with accept/reject per proposal); `/matrix` → the matrix
panel (F9). Views never write on their own; they emit slash lines (which
the operations validate and save); their data comes from `OpOutput.data`.

**Planned** (`PLANNED_COMMANDS`, help only): `/rewind`, `/fork`, `/archive`,
`/spawn`, `/mention`, `/kill`, `/logs`, `/doctor`, `/config`, `/mcp`,
`/channels`, `/lease`, `/priority`, `/depends`, `/theme`.

### 8.3 Prefixes

| Input | Current behavior |
|---|---|
| `/command` | a slash command; the popup completes the name and subcommand |
| `! command` / `!!` | a shell command on the host, or a repeat (see §3) |
| `#text` | `/diary note text` if the `diary` operation is registered, else `/memory record text`. No model turn. |
| `@role text` | a delegation request to the UIA: the chat text becomes "[Delegation request for role "role"] Please delegate … This is a request from the user to you (the UIA), not direct routing …" + `text`. Role = an exact (case-insensitive) match in the list of known agent roles; takes precedence over files. |
| `@path` | a file is attached to the message as `<file path="rel/path">…</file>`. Limits (`MentionLimits::default`): **64 KiB per file, 256 KiB total, at most 8 files**. The path is canonicalized and must stay under the canonical project root (no `..`, no symlinks pointing outside). **Blocklist**: `.env*`, `*.pem`, `*.key`, `id_*`, anything under `.git/` — checked on both the typed and the resolved path. Binary files (a NUL byte) and invalid UTF-8 are rejected. Trailing punctuation on a token is stripped if the full token doesn't resolve. Rejections show a reason; a same-named file instead of a role is reached via `./name`. |
| `\/`, `\!`, `\#`, `\@`, `\$` | escapes the prefix, the rest is chat |
| `$` | reserved |

Typing `@` opens a popup with candidates (roles and project files).

### 8.4 Keybindings (default)

Rebindable via `[tui].keybindings_file` (a flat TOML table `action =
"chord"`, or a list; an empty list clears a binding).

| Key | Action (`name`) |
|---|---|
| `F1` | help, with tabs for commands / keys / prefixes (`show_help`) |
| `F2` | toggle the explorer (`toggle_explorer`) |
| `F3` | toggle the agent panel (`toggle_agents`) |
| `F4` | cycle focus (`cycle_focus`) |
| `F5` | toggle the workbench panel (`toggle_workbench`) |
| `F6` | open the kanban board (`open_kanban`) |
| `F7` | mode and approval picker (`open_mode_picker`) |
| `F8` | models per role (`open_models`) |
| `F9` | open the matrix-game panel (`open_matrix`) |
| `F11` | maximize the panel (`maximize_panel`) |
| `Ctrl+E` | focus the explorer (`focus_explorer`) |
| `Ctrl+O` | toggle tool cells (`toggle_tool_cells`), also mid-turn |
| `Ctrl+H` | end a host work phase (`end_host_mode`), also a process-wide one from `/sandbox-lease` or `request_host`; together with the typed `/sandbox-lease revoke` the only way to end one (no expiry, the model cannot end it) |
| `Ctrl+K` | delete the input line (`delete_line`) |
| `Ctrl+J` | insert a newline (`insert_newline`) |
| `Shift+Tab` | approval cycle `ask → auto → full → plan → ask` (`cycle_permission_mode`); the `plan` step is plan mode (marker "⏸ plan mode on (shift+tab to cycle)", the lock is immediate); has no effect while a `/`-popup is open |
| `Ctrl+↑` / `Ctrl+↓` | scroll without changing focus (`scroll_panel_up` / `scroll_panel_down`): the body of an open dialog (approval, sudo, host permit, plan, `ask_user`), otherwise the visible agent panel (or its detail view); with neither visible the key goes to the composer |

Agent panel, when focused (`F4`, fixed): `↑`/`↓`, `k`/`j` and `w`/`s`
select, `PageUp`/`PageDown` page, `Home`/`End` jump, `Enter` opens the
detail view or expands the finished-agents summary line, `f` expands/
collapses it, `c` acknowledges failed agents. `w`/`s` only apply there;
in the composer they stay plain letters. The mouse wheel scrolls whatever
is under the pointer: an open dialog's body, the agent panel, otherwise
the chat history. Dialog options and the key-hint line stay pinned and
always visible; the dialog body scrolls (`Ctrl+↑↓`, wheel), and `↑`/`↓`
stay option selection.

Small windows: below 100 columns the agent panel collapses to a one-line
summary above the status line (from 16 rows up; below that it is hidden),
so the chat keeps its width. The status line drops token details first,
then the context gauge and hints, and keeps mode, approval and model.
Dialogs keep options and hints visible down to 40×12.

Not rebindable: `Ctrl+C`/`Ctrl+D`, `Esc`, `Enter` including
`Shift/Alt+Enter`, and keys inside dialogs/overlays. The `/`-popup also
responds to up/down/Tab/Esc/Enter mid-turn; the input history also
contains `/`-commands (never `/btw`). While child agents are running,
`Esc` mid-turn asks for confirmation first ("press Esc again to confirm");
`Enter` queues during a turn and never aborts one. The status line shows
`Mode: <mode> · Approval: <ask|auto|full>` (with "(pending)" appended while
a mode change is waiting for the turn boundary) and the active model.

### 8.5 CLI flags for session startup

Allowed only for `harw`/`harw chat`, `harw exec`, and `harw analyze`
(otherwise an error; `--add-dir` is not allowed for `analyze`).

| Flag | Effect | Precedence |
|---|---|---|
| `--mode <mode>` | the root session's interaction mode | `--mode` > `[mode] default` (defaults to `chat`); an unknown name is an error in both cases |
| `--agent <name>` | starts the session with this agent definition as root (`RuntimeSpec::active_agent`) instead of the configured one; `/agent use <name>` sets the same choice persistently from the next session | `--agent` > the profile's `active_agent_definition` |
| `--approval <ask\|auto\|full>` | the session's approval mode (`RuntimeSpec.approval_override`) | `--approval` > project `[permissions].default_mode` > global > the entry point's default (`auto`) |
| `--model <id>` | the session's model (`RuntimeSpec.model_override`) | checked against `config.models` (key, then ID, then alias); unknown = a configuration error. Sets `default_model`/`default_provider` and lifts a UIA pin for this run |

Details and rationale: `tui-roles-models-modes.md`.
