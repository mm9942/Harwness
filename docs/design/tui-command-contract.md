# Harwness TUI Interaction Contract

Status: draft design document
Owning crates (planned): `harw-tui` (new), `harw-config`, `harw-types`, `harw-protocol`
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
| `/export` | `<SessionKey?> [--format=md\|json]` | Export a session transcript | Op | R |

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
| `/model` | `[ProviderRef/model \| --status]` | Select or inspect the active model/provider | Op | Y |
| `/provider` | `[ProviderRef] [--auth] [--status]` | Inspect or switch provider, manage credentials | Maint (`--auth`), Op (else) | R |
| `/skills` | `[SkillRef] [--search=text] [--install]` | Browse, search, or install skills | Op (browse), Maint (`--install`) | R |
| `/tools` | `[ToolRef] [--enable \| --disable]` | Show or toggle tool availability for the session | Op | R |
| `/mcp` | `[McpServerRef] [--add \| --remove \| --status]` | Inspect or manage MCP server bindings | Maint | - |
| `/config` | `<path> [value]` | Read or write a config key on disk | Maint | - |
| `/policy` | `<PolicyRef> [value]` | Inspect or set a policy value | Maint (read: Op) | - |
| `/sandbox` | `[--status] [--profile=name]` | Inspect or switch the active sandbox profile | Maint | - |
| `/plugins` | `[--list \| --enable=name \| --disable=name]` | Manage extension-api plugins | Maint | - |

### 2.6 Knowledge surfaces (names reserved here; semantics detailed by another design worker)

| Command | One-line semantics | Tier | Parity |
|---|---|---|---|
| `/memory` | Query/append durable cross-session agent memory | Op | Y |
| `/diary` | Per-session narrative log of what the agent did and why | Obs | R |
| `/dream` | Offline/idle-time consolidation pass over memory | Maint | - |
| `/palace` | Spatial/associative index over long-term memory ("memory palace") | Obs | - |
| `/workbench` | Scratch surface for in-progress artifacts before promotion to work items | Op | R |
| `/kanban` | Visual board projection of the work graph (§2.3) | Obs | Y |

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
| `!` | **Shell** | Runs the remainder as a host shell command through the sandboxed execution surface; output streams to an Inline or Pager surface depending on length. Requires `Operator` tier and an enabled `commands.shell` capability flag. |
| `!!` | **Shell-repeat** | Re-runs the last `!` shell command verbatim. Bare `!!` with no trailing text; any trailing text after `!!` is an error (`CommandError::TrailingTokens`), to avoid ambiguity with `!! <new cmd>` meaning something else. |
| `#` | **Note** | Appends the remainder as a timestamped entry to `/diary` (§2.6) without invoking the agent. Never sent to the model as a prompt — purely a local annotation. |
| `@` | **Mention** | Addresses a specific `AgentRef` or file. `@planner <text>` routes `<text>` to the named agent (equivalent to `/mention`). `@./path/to/file` (leading `./`, `../`, or `/`) attaches file content as context instead. Disambiguation rule: a token is an `AgentRef` if it resolves in the topology; otherwise, if it looks path-like, it is a file mention; otherwise it is a literal `@` character passed through as chat text with a warning toast. |

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

### 4.1 Global keys (active in every view)

| Key | Action |
|---|---|
| `Ctrl+C` (×2 within 1s) | Quit (double-tap guard against accidental exit) |
| `Ctrl+K` | Open command palette (fuzzy `/` command search) |
| `Ctrl+L` | Clear visible transcript pane |
| `F1` | Toggle help overlay (keybinding + command cheat sheet) |
| `Tab` / `Shift+Tab` | Cycle focus between panels |
| `Ctrl+P` / `Ctrl+N` | Previous / next session in session switcher |
| `Esc` | Dismiss active pager/panel/toast, or clear input line |

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
quit = { keys = ["ctrl+c", "ctrl+c"], within_ms = 1000 }
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
| `/memory`, `/kanban` | Y | Y | Read/append operations with clear, bounded blast radius |
| `/diary`, `/palace`, `/workbench` | Y | Reduced / TUI-only (see per-command note in the knowledge-surface design doc) | Deferred to the dedicated knowledge-surface design |
| `/dream` | Y | TUI-only | Maintainer-only offline batch job, not a conversational action |
| `/bind`, `/unbind`, `/broadcast` | Y | TUI-only | Channel topology itself must not be mutable from within a channel it controls |
| `/help`, `/status`, `/quit`, `/feedback` | Y | Y | Universal, low-risk |
| `/keys`, `/clear`, `/theme` | Y | TUI-only | Terminal-rendering concerns only |

**Parity summary**: of 56 inventoried commands, 25 are full `ChannelParity`,
17 are `ChannelReduced`, 14 are `TuiOnly`.

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
3. **Knowledge-surface commands** (`/memory`, `/diary`, `/dream`, `/palace`,
   `/workbench`, `/kanban`) are reserved names + one-liners only per the
   task brief; their full argument grammar, storage model, and parity
   detail belong to a dedicated design document and are out of scope here.
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
