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

> **Ist-Stand (2026-09-24, deutsch):** Die §§1–7 sind der ursprüngliche
> Zielentwurf. Was der Code heute tatsächlich anbietet — Befehlsinventar
> (Operationen, TUI-lokale Befehle, Ersatz-Spezifikationen, geplante
> Befehle), Präfixe `#`/`@`, Tastenbelegung F1/F5–F9, `/models`,
> `/mode default` und die CLI-Flags `--mode`/`--approval`/`--model` — steht
> verbindlich in **§8**. Modelle je Rolle, Modus vs. Freigabe und die
> Start-Rangfolge beschreibt `tui-roles-models-modes.md`. Bei Widerspruch
> gilt §8.

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
| `/export` | `[--format md\|markdown\|json] [--tools\|--no-tools] [--reasoning-summary] [--datei <pfad>] [--max-chars <n>]` | Export a session transcript; `--max-chars <n>` caps the total rendered export length at `n` Unicode characters (whole export, not per entry) — implemented in `harw-ops/src/export.rs` (`ExportArgs::max_chars`) and consumed by `harw_tui::export::ExportOptions::max_chars`. **Ist-Stand 2026-09-21:** lesbares Startdatum (`jiff`, RFC-3339-Stil statt Unix-Sekunden); Tool-Argumente/-Ergebnisse als verschachteltes Pretty-JSON bzw. als ` ```text ` -Block statt einer einzelnen Riesenzeile, pro Eintrag gekappt auf `ExportOptions.max_chars_per_entry` (Default 4000 Zeichen, zusätzlich zur globalen `--max-chars`-Kappung); Überschriften in User-/Assistant-Text um zwei Ebenen herabgestuft; Tool-Einträge (ToolCall/ToolResult/Reasoning) gruppiert unter „## harw" statt „## Du"; scheitert das Kopieren in die Zwischenablage, schreibt `/export` stattdessen die Datei (`default_export_path`) und meldet den Pfad, die OSC-52-Ausgabe wird in tmux zusätzlich mit einer DCS-Passthrough-Hülle umschlossen — siehe `interaction-contract.md` §2.6.7 | Op | R |

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
| `/model` | `show \| list \| switch <id>` | Inspect the active model, or atomically switch provider+model together (implemented grammar; see `interaction-contract.md` §2.6.1 for the full Ist-Stand table, including the new `/uia-model`, `/uia-worker-model`, `/effort`, `/uia-effort`, `/provider-concurrency` siblings) | Op | Y |
| `/models` (neu) | `[show \| set <rolle> <ziel> \| reset <rolle> \| pick <rolle>]` | Modelle **je Rolle** (12 Rollen, siehe `tui-roles-models-modes.md`) anzeigen/setzen/zurücksetzen; wirkt **ab nächster Sitzung** — nur `/model` wechselt live. `pick` öffnet TUI-lokal den Modell-Picker der Rolle. Details §8.2 | Op | - |
| `/mode` (aktualisiert) | `[show \| <modus> \| default <modus>]` | Interaktionsmodus (`chat\|plan\|explore\|work\|shell`) anzeigen, für die laufende Sitzung anfordern (wirkt an der nächsten Turn-Grenze) oder als `[mode] default` persistieren (neue Sitzungen). Bare `/mode` öffnet in der TUI die Modus-/Freigabe-Auswahl (F7) | Op | - |
| `/provider` | `show \| list \| test` | Inspect provider status and credentials only — **`/provider switch` no longer exists**; a provider (and model) switch is exclusively driven by `/model switch <id>` (atomic, works even across providers) | Op | R |
| `/skills` | `[SkillRef] [--search=text] [--install]` | Browse, search, or install skills | Op (browse), Maint (`--install`) | R |
| `/tools` | `[ToolRef] [--enable \| --disable]` | Show or toggle tool availability for the session | Op | R |
| `/mcp` | `[McpServerRef] [--add \| --remove \| --status]` | Inspect or manage MCP server bindings | Maint | - |
| `/config` | `<path> [value]` | Read or write a config key on disk | Maint | - |
| `/policy` | `<PolicyRef> [value]` | Inspect or set a policy value | Maint (read: Op) | - |
| `/sandbox` | `[--status] [--profile=name]` | Inspect or switch the active sandbox profile | Maint | - |
| `/sandbox-lease` (neu, Nutzerentscheidung 2026-09-21) | `[status \| revoke]` | Host-Freigabe für `shell.exec`: `status` zeigt die aktive Freigabe (Sitzung/Einzelaufruf/aus), `revoke` beendet eine aktive Sitzungsfreigabe sofort (`busy = "immediate"`); die Freigabe selbst wird nicht über den Command, sondern über das gleichnamige Modell-Tool `sandbox-lease` (Argument `reason`) angefordert und im `HostPermitDialog` bestätigt — siehe `interaction-contract.md` §2.6 und `mediated-process-execution.md` | Maint | - |
| `/plugins` | `[--list \| --enable=name \| --disable=name]` | Manage extension-api plugins | Maint | - |

### 2.6 Knowledge surfaces

Ist-Grammatik Runde 4 (aus `harw-ops/src/{memory,diary,dream,palace,workbench,kanban,learn}.rs`
und `harw-ops/src/matrix/mod.rs`). Stufe laut `OperationMeta` ist bei allen
`operator`. Die volle Tabelle je Unterbefehl (Stufe, Sicht, busy, Hinweise)
steht in `interaction-contract.md` §2.2.

| Command | Ist-Grammatik | Tier | Parity |
|---|---|---|---|
| `/memory` | `list \| stats \| recall <stichwort…> \| record <text…> [--project\|--global] \| forget <name> \| promote <fakt-id> [--project\|--global] [--slug <slug>] \| maintain \| consolidate [--project\|--global]` \| topics | Op | Y |
| `/diary` | `today \| show [agent] [--date=YYYY-MM-DD \| --from=YYYY-MM-DD [--to=YYYY-MM-DD]] \| search <text> [--agent=<id>] [--from=…] [--to=…] \| note <text>`; `#text` ist Zucker für `note` (§3) \| agents | Op | R |
| `/dream` | `list \| show <id> \| run \| status \| review [<id>] \| review <id> accept\|reject <vorschlag-id> [grund…]` | Op | R |
| `/palace` | `list \| show <id> \| search <anfrage> [--max-hops=n] [--max=n] \| promote <thema> \| supersede <alt> <neu> [--confirm] \| edit <id> <text…> [--confirm] \| link <a> <b> [--confirm]` | Op | Y |
| `/workbench` | `show \| pin <pfad> [notiz] \| unpin <pfad> \| note <text> \| note edit <n> <text> \| note rm <n> \| hypothesis add\|confirm\|reject <text> \| retention [keep\|<tage>d]`; jeder Unterbefehl mit `[--scope=session\|project\|project:<slug>]` | Op | R |
| `/kanban` | `[--board=<id>] show [karte] \| list [--all] \| boards \| add\|create <titel> … \| edit <karte> <text…> \| comment <karte> <text…> \| evidence <karte> <pfad\|url> \| approve <karte> [notiz…] \| reject <karte> [grund…] \| move <karte> <todo\|ready\|running\|done\|blocked\|archived> \| todo\|ready\|claim\|unblock\|done\|archive <karte> \| block <karte> <grund>` | Op | Y |
| `/learn` | `[scan] \| note <text…> [--target memory\|skill\|agent] \| list [--all] \| show <id> \| accept <id> \| reject <id> [grund…]` — schlägt nur vor, übernimmt nie selbst | Op | R |
| `/matrix` | `start <szenario> [--seed N] [--package <id>] \| step \| auto N \| pause \| inject … \| override <json> \| veto <arg-id> [grund] \| reveal <id> \| fork <runde> \| end \| replay [--seed N] \| show \| list \| compare <lauf> <lauf> […]`; Panel mit F9 | Op | R |

Sichtbarkeit `/workbench`: die Operation trägt genau eine Sichtbarkeit,
`channel_reduced`. `pin`/`unpin` und die bare Panel-Form sind trotzdem nur
lokal sinnvoll (Pfadauflösung gegen das Arbeitsverzeichnis der TUI bzw. das
Panel); `note` und `hypothesis` sind die eigentlich reduziert freigegebenen
Formen. §6 ist entsprechend angeglichen.

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
| `!` | **Shell** | Runs the remainder as a host shell command through the sandboxed execution surface; output streams to an Inline or Pager surface depending on length. Requires `Operator` tier and an enabled `commands.shell` capability flag. **Ist-Stand 2026-09-21:** läuft mit dem beim Programmstart gelesenen zsh-PATH des harw-Prozesses — jedes existierende PATH-Verzeichnis außerhalb von `/usr /bin /lib /lib64` und dem Workspace wird per `--ro-bind-try` in die Sandbox gebunden (ausgenommen `/` und exakt `$HOME`), `~/.cargo/bin` zieht zusätzlich `RUSTUP_HOME`/`CARGO_HOME` nach sich; der Prozess läuft weiterhin in bwrap, nicht auf dem Host, und zwar unabhängig von einem aktiven `sandbox-lease`. Die normale Modell-`shell.exec` in der Projekt-Sandbox bleibt ohne Lease hermetisch mit Minimal-PATH — siehe `mediated-process-execution.md`. |
| `!!` | **Shell-repeat** | Re-runs the last `!` shell command verbatim. Bare `!!` with no trailing text; any trailing text after `!!` is an error (`CommandError::TrailingTokens`), to avoid ambiguity with `!! <new cmd>` meaning something else. |
| `#` | **Notiz** (Ist) | `#text` wird zu `/diary note text`, wenn die `diary`-Operation registriert ist, sonst zu `/memory record text`. Kein Modell-Turn; läuft über denselben Op-Pfad (eine Audit-Spur). Details §8.3. |
| `@` | **Erwähnung** (Ist) | `@rolle text`: **Delegationswunsch an die UIA** — der Text geht als normaler Chat an die UIA, eingeleitet mit „[Delegationswunsch an Rolle „rolle“]“; die UIA entscheidet, ob sie delegiert (kein direktes Routing). `@pfad` hängt eine Projektdatei als `<datei pfad="…">…</datei>`-Block an (64 KiB je Datei, 256 KiB gesamt, max. 8 Dateien, Sperrliste). Eine bekannte Rolle hat Vorrang; eine gleichnamige Datei per `./name`. Details §8.3. |

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

> **Ist-Stand:** Die tatsächliche Standardbelegung (u. a. `Ctrl+K` = Zeile
> löschen, `Shift+Tab` = Freigabe-Zyklus, F1 Hilfe, F5 Werkbank, F6 Kanban,
> F7 Modusauswahl, F8 Modelle) und das reale Dateiformat
> (`[tui].keybindings_file`, flache Tabelle `aktion = "chord"`) stehen in
> §8.4. Die folgenden Tabellen und das TOML-Schema in §4.4 sind der
> ursprüngliche Entwurf.

### 4.1 Global keys (active in every view)

| Key | Action |
|---|---|
| `Ctrl+C` (×2 within 2s) | Idle: quit (double-tap guard against accidental exit). Busy (during a running turn): **hard interrupt** — first press cancels the running model call, shell/sandbox subprocesses (SIGKILL), and child agents, and arms the same double-tap state as idle; a second press within the window quits. Model-call and subprocess cancellation (`harw-core/src/turn_loop.rs`, `harw-tool-shell/src/exec.rs`), child-agent cancellation wiring (`register_parent_cancel_token` called from `run_turn_streaming` in `harw-tui/src/app.rs`), and the unified idle/busy double-tap state (`pending_quit`/`hard_quit_requested`, `QuitArm`/`QUIT_HINT_WINDOW`) are all implemented — see `interaction-contract.md` §2.6.4. **Runde 4 (Teil F):** interrupting no longer drops the queue — messages and commands submitted during the turn stay queued and are delivered right after the interrupt (status line: „Warteschlange wird gesendet“), also when an approval or host-permit dialog was open. |
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
| `/diary`, `/workbench`, `/learn`, `/matrix` | Y | Reduced (one visibility per operation; `/workbench pin`/`unpin` and the bare panel are only meaningful locally) | Ist-Stand Runde 3/4, see §2.6 and `interaction-contract.md` §2.2 |
| `/dream` | Y | Reduced | Ist-Stand Runde 4: `list`/`show`/`status`/`review` are safe to read remotely; `run` needs a dream launcher, which only interactive entries (TUI, one-shot) provide |
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
`busy_subcommands = "show=immediate, -=immediate"`, `-` = bare form). Since
Runde 4 (Teil H) there are three classes:

- **Immediate** — runs during a running turn as its own task (60 s limit);
  output appears as a system line „<befehl> (während Turn)“.
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
a „Wartet auf den nächsten Turn“ block above the composer until delivered.
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
levels plus a "Provider-Default (zurücksetzen)" entry that emits `clear`.

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
3. ~~Knowledge-surface commands~~ — erledigt: Grammatik in §2.6, Speicher-
   modell in `knowledge-surfaces.md`. Seit Runde 4 ist auch `/dream` eine
   registrierte Operation (`list|show|run|status|review`).
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

## 8. Ist-Stand der Befehlsfläche (2026-09-24, Runde 4)

Dieser Abschnitt beschreibt, was der Code liefert, und hat bei Widerspruch
Vorrang vor §§1–7. Quellen: `harw-ops/src/lib.rs` (`register_all`,
`register_plan_tools`), `harw-tui/src/command_catalog.rs`
(`local_command_specs`, `FALLBACK_COMMANDS`, `PLANNED_COMMANDS`,
`HINT_TABLE`), `harw-tui/src/keybindings.rs`, `harw-tui/src/mention.rs`,
`harw-cli/src/cli/global.rs`.

### 8.1 Woher ein Befehl kommt

Die TUI-Registry wird aus den Command-Adaptern der Operationen gebaut
(`CommandRegistry::from_command_adapters`) und danach mit den lokalen
Spezifikationen gemischt (`with_local_specs`). Bei Namensgleichheit gewinnt
**immer die Operation**; der lokale Eintrag entfällt. Jede Spezifikation
trägt `summary`, `usage`, `subcommands` (aus `HINT_TABLE`) und
`origin: Operation | TuiLocal`; das `/`-Popup und die Hilfe (F1) zeigen sie.

| Herkunft | Bedeutung |
|---|---|
| **Operation** | in `harw-ops` definiert und über `register_all` (45 Ops) bzw. hinter `[tools.plan] enabled` über `register_plan_tools` (7 Ops) registriert |
| **TUI-lokal** | nur in der TUI abgefangen, keine Operation (`CommandScope::TuiOnly`) |
| **Ersatz** | TUI-Spezifikation für einen Befehl, dessen Operation (noch) nicht registriert ist (`FALLBACK_COMMANDS`: `mode`, `matrix`); entfällt automatisch, sobald die Operation registriert ist |
| **geplant** | nur in der Hilfe gelistet (`PLANNED_COMMANDS`), nicht ausführbar |

Stand der Registrierung (Runde 4): `workbench`, `kanban`, `diary`,
`palace`, `dream`, `learn`, `matrix` und `models` sind in `register_all`
eingetragen; `mode` und `matrix` sind registriert, ihr Ersatz ist damit
wirkungslos.

### 8.2 Inventar

Spalten: Stufe laut `OperationMeta.permission` (Obs/Op/Maint), Sichtbarkeit
(`Y` channel_parity, `R` channel_reduced, `-` tui_only), `busy`
(`sofort` = `Immediate`, `vorgemerkt` = `Staged`, gilt ab dem nächsten
Turn; sonst bis Turn-Ende zurückgestellt; Einzelheiten §6.1).

**Operationen (`register_all`)**

| Befehl | Grammatik (Kurzform) | Stufe | Sicht | busy |
|---|---|---|---|---|
| `/help` | `[befehl]` | Obs | Y | sofort |
| `/status` | — | Obs | Y | sofort |
| `/quit` | — | Op | - | |
| `/new` | — | Op | Y | |
| `/work` | — (Job-Übersicht) | Obs | Y | sofort |
| `/ps` | — | Obs | R | sofort |
| `/attach` | — | Op | - | sofort |
| `/stop` | `[job-id]` | Op | Y | sofort |
| `/diff` | — | Obs | Y | sofort |
| `/agent` | `[list \| stop <agent-id> \| budget [agent-id] \| use <name> \| use --clear]`; `use` setzt den Wurzel-Agenten ab der nächsten Sitzung (Profil-`active_agent_definition`) | Op | Y | sofort |
| `/skills` | `[list \| show <name> \| …]` | Op | R | sofort (bare/`list`/`show`) |
| `/plugins` | — | Maint | - | sofort (außer `install`/`activate`/`uninstall`) |
| `/model` | `[show \| list \| switch <modell-id>]` — **live** ab dem nächsten Turn | Op | - | sofort (`show`/`list`), sonst vorgemerkt |
| `/provider` | `[show \| list \| test]` (kein `switch`) | Op | - | sofort (außer `test`) |
| `/uia-model` | `[show \| list \| switch <modell-id>]` — ab nächster Sitzung | Op | - | sofort (bare/`show`/`list`), sonst vorgemerkt |
| `/uia-provider` | `[show \| list \| test]` (kein `switch`) | Op | - | sofort (bare/`show`/`list`), `test` zurückgestellt |
| `/uia-worker-model` | `[show \| list \| switch <modell-id>]` — ab nächster Sitzung, nur Modelle des UIA-Providers | Op | - | sofort (bare/`show`/`list`), sonst vorgemerkt |
| `/effort` | `[show \| minimal \| low \| medium \| high \| xhigh \| max \| clear]` | Op | - | sofort (bare/`show`), sonst vorgemerkt |
| `/uia-effort` | wie `/effort`, ab nächster Sitzung | Op | - | sofort (bare/`show`), sonst vorgemerkt |
| `/permissions` | `[show \| mode <ask\|auto\|full> [--session\|--project\|--global] \| allow <tool> [muster] … \| deny <tool> [muster] … \| remove <nr>]` | Op | - | sofort (bare/`show`/`mode`/`set`) |
| `/mode` | `[show \| <modus> \| default <modus>]` | Op | - | sofort (bare/`show`), sonst vorgemerkt |
| `/compact` | — | Op | Y | |
| `/memory` | siehe §2.6 | Op | Y | |
| `/workbench` | siehe §2.6 | Op | R | sofort (bare/`show`) |
| `/kanban` | siehe §2.6 | Op | Y | geplant sofort (bare/`list`/`show`/`boards`), beim Schreiben noch zurückgestellt |
| `/diary` | siehe §2.6 | Op | R | sofort (bare/`show`/`today`/`search`) |
| `/palace` | siehe §2.6 | Op | Y | sofort (bare/`list`/`show`/`search`) |
| `/dream` | siehe §2.6 | Op | R | geplant sofort (bare/`list`/`show`/`status`), beim Schreiben noch zurückgestellt |
| `/learn` | siehe §2.6 | Op | R | |
| `/matrix` | siehe §2.6 | Op | R | sofort (`show`/`list`) |
| `/models` | `[show \| set <rolle> <ziel> \| reset <rolle>]`; `pick <rolle>` ist TUI-lokal (öffnet den Picker) | Op | - | sofort |
| `/context-proposal` | `list \| view \| accept \| reject` | Op | Y | |
| `/add-workdir` | `<pfad>` | Op | - | |
| `/export` | `[--format md\|json] [--datei <pfad>] [--tools\|--no-tools] [--reasoning-summary] [--max-chars <n>]` | Op | - | |
| `/usage` | — | Obs | Y | sofort |
| `/bug-report` | — | Op | - | |
| `/approve`, `/deny`, `/cancel` | `<id> …` | Op | Y | sofort |
| `/review` | `<id>` | Obs | Y | sofort |
| `/retry` | `<id>` | Op | Y | |
| `/provider-concurrency` | — | Op | - | sofort |
| `/sandbox-lease` | `[status \| revoke]` | Op | - | sofort |

`approval.pending`/`approval.resolve` haben nur eine Web-Fläche, keinen
Slash-Befehl.

**Operationen hinter `[tools.plan] enabled`**: `/plan`, `/goal`, `/explore`,
`/research`, `/research-deps` (mit `--generic` bzw. `ecosystem=<x>`
ökosystem-neutral über `dependency-researcher`), `/research-web`, `/analyze`
(alle Op). `/plan` (bare/`inspect`/`ready`/`waves`) und `/goal`
(bare/`show`/`check`) laufen während eines Turns sofort.

`/models`: `<rolle>` ist ein Schlüssel aus `ModelRole::key`
(`uia`, `uia-worker`, `orchestrator`, `sub-orchestrator`, `worker-simple`,
`worker-complex`, `explorer`, `research`, `compaction`, `title`, `memory`,
`dream`; `_`/`-` und Groß-/Kleinschreibung egal, auch Schlüssel der internen
Modellstellen wie `session_title`). `<ziel>` ist eine Modell-ID/ein Alias/ein
Katalogschlüssel oder `provider/modell` (Trennung am **ersten** `/`, Präfix
muss ein aktivierter Provider sein). `set uia-worker` lehnt Modelle eines
anderen Providers als dem der UIA ab. `reset` entfernt die explizite Wahl
(UIA: `uia_provider`/`uia_model`; interne Stellen: ganze Tabelle
`[internal_models.<stelle>]`). Kein Modell-Werkzeug. `data`-Vertrag:
`show` → `{"roles":[{"role","label","provider","model","source","effort"}],"live":{"provider","model"}}`.

`/mode default <modus>` schreibt `[mode] default` in die Profil-`config.toml`
(bestes Bemühen, Fehler als Hinweis) und berührt die laufende Sitzung nicht;
`/mode <modus>` wird vorgemerkt und an der nächsten Turn-Grenze angewandt
(Antwort `requested`, nicht `applied`). Kein Modell-Werkzeug.

**TUI-lokal** (`local_command_specs`, alle `tui_only`)

| Befehl | Wirkung | Stufe | busy |
|---|---|---|---|
| `/tools` | `[on <name> \| off <name> \| reset [name] \| profile <minimal\|coding\|full>]` | Op | |
| `/resume` | `[sitzungs-id]`, ohne ID Auswahl | Op | |
| `/sessions` | Sitzungsauswahl öffnen | Obs | |
| `/exit` | TUI beenden (wie `/quit`) | Obs | |
| `/clear` | Anzeige leeren, Sitzung bleibt | Obs | |
| `/verbose` | ausführliche Werkzeuganzeige umschalten | Obs | sofort |
| `/keys` | Tastenbelegung anzeigen | Obs | sofort |
| `/whoami` | Sitzung, Berechtigung, aktives Modell | Obs | sofort |
| `/rename` | `<titel>` | Op | sofort |
| `/agents` | Agenten-Panel ein-/ausblenden | Obs | sofort |

Die Ersatz-Spezifikationen (`/mode`, `/matrix`) sind wirkungslos, solange die
Operationen registriert sind.

Bare-Formen öffnen Ansichten statt Text: `/model`, `/uia-model`,
`/uia-worker-model` → Modell-Picker; `/effort`, `/uia-effort` →
Effort-Auswahl; `/models` → Rollen-Modell-Ansicht (F8); `/mode` →
Modus-/Freigabe-Auswahl (F7); `/kanban` → Board (F6); `/workbench` →
Werkbank-Panel (F5); `/palace`, `/dream`, `/diary` → Wissens-Browser
(Diary nach Datum/Agent mit Suche und Bereichsansicht, Palace mit
Links/Backlinks, Suche und Promote/Supersede mit Bestätigung, Dream-Review
mit Annehmen/Ablehnen je Vorschlag); `/matrix` → Matrix-Panel (F9).
Ansichten schreiben nie selbst, sondern erzeugen Slash-Zeilen (die Ops
prüfen und speichern); ihre Daten kommen aus `OpOutput.data`.

**Geplant** (`PLANNED_COMMANDS`, nur Hilfe): `/rewind`, `/fork`, `/archive`,
`/spawn`, `/mention`, `/kill`, `/logs`, `/doctor`, `/config`, `/mcp`,
`/channels`, `/lease`, `/priority`, `/depends`, `/theme`.

### 8.3 Präfixe

| Eingabe | Ist-Verhalten |
|---|---|
| `/befehl` | Slash-Befehl; Popup vervollständigt Name und Unterkommando |
| `!befehl` / `!!` | Shell-Befehl bzw. Wiederholung (siehe §3) |
| `#text` | `/diary note text`, falls die `diary`-Operation registriert ist, sonst `/memory record text`. Kein Modell-Turn. |
| `@rolle text` | Delegationswunsch an die UIA: Chattext wird zu „[Delegationswunsch an Rolle „rolle“] Bitte delegiere … Dies ist eine Bitte des Nutzers an dich (UIA), kein direktes Routing …“ + `text`. Rolle = exakter (case-insensitiver) Treffer in der Liste bekannter Agentenrollen; hat Vorrang vor Dateien. |
| `@pfad` | Datei wird als `<datei pfad="rel/pfad">…</datei>` an die Nachricht angehängt. Grenzen (`MentionLimits::default`): **64 KiB je Datei, 256 KiB gesamt, höchstens 8 Dateien**. Pfad wird kanonisiert und muss unter der kanonischen Projektwurzel liegen (kein `..`, keine Symlinks nach außen). **Sperrliste**: `.env*`, `*.pem`, `*.key`, `id_*`, alles unter `.git/` — geprüft auf getipptem *und* aufgelöstem Pfad. Binärdateien (NUL-Byte) und ungültiges UTF-8 werden abgelehnt. Satzzeichen am Tokenende werden abgeschnitten, wenn das volle Token nicht auflösbar ist. Ablehnungen erscheinen mit deutschem Grund; eine gleichnamige Datei statt Rolle per `./name`. |
| `\/`, `\!`, `\#`, `\@`, `\$` | Präfix maskieren, Rest als Chat |
| `$` | reserviert |

Beim Tippen von `@` öffnet ein Popup Kandidaten (Rollen und Projektdateien).

### 8.4 Tastenbelegung (Standard)

Umbelegbar über `[tui].keybindings_file` (flache TOML-Tabelle
`aktion = "chord"` oder Liste; leere Liste hebt auf).

| Taste | Aktion (`name`) |
|---|---|
| `F1` | Hilfe mit Reitern Befehle / Tasten / Präfixe (`show_help`) |
| `F2` | Explorer ein/aus (`toggle_explorer`) |
| `F3` | Agenten-Panel ein/aus (`toggle_agents`) |
| `F4` | Fokus reihum (`cycle_focus`) |
| `F5` | Werkbank-Panel ein/aus (`toggle_workbench`) |
| `F6` | Kanban-Board öffnen (`open_kanban`) |
| `F7` | Modus- und Freigabe-Auswahl (`open_mode_picker`) |
| `F8` | Modelle je Rolle (`open_models`) |
| `F9` | Matrix-Game-Panel öffnen (`open_matrix`) |
| `F11` | Panel im Vollbild (`maximize_panel`) |
| `Ctrl+E` | Explorer fokussieren (`focus_explorer`) |
| `Ctrl+O` | Werkzeugzellen auf/zu (`toggle_tool_cells`) |
| `Ctrl+H` | Host-Arbeitsphase beenden (`end_host_mode`) |
| `Ctrl+K` | Eingabezeile löschen (`delete_line`) |
| `Ctrl+J` | neue Zeile (`insert_newline`) |
| `Shift+Tab` | Freigabe-Zyklus `ask → auto → full → plan → ask` (`cycle_permission_mode`); nicht bei offenem `/`-Popup |

Nicht umbelegbar: `Ctrl+C`/`Ctrl+D`, `Esc`, `Enter` samt `Shift/Alt+Enter`,
Tasten in Dialogen/Overlays. Die Statuszeile zeigt
`Modus: <modus> · Freigabe: <ask|auto|full>` (Zusatz „(ausstehend)“, solange
ein Moduswechsel auf die Turn-Grenze wartet) und das aktive Modell.

### 8.5 CLI-Flags für den Sitzungsstart

Nur bei `harw`/`harw chat`, `harw exec` und `harw analyze` erlaubt (sonst
Fehler; `--add-dir` nicht bei `analyze`).

| Flag | Wirkung | Rangfolge |
|---|---|---|
| `--mode <modus>` | Interaktionsmodus der Wurzelsitzung | `--mode` > `[mode] default` (Vorgabe `chat`); unbekannter Name ist in beiden Fällen ein Fehler |
| `--agent <name>` | Startet die Sitzung mit dieser Agentendefinition als Wurzel (`RuntimeSpec::active_agent`) statt der konfigurierten; `/agent use <name>` setzt dieselbe Wahl dauerhaft ab der nächsten Sitzung | `--agent` > Profil-`active_agent_definition` |
| `--approval <ask\|auto\|full>` | Freigabemodus der Sitzung (`RuntimeSpec.approval_override`) | `--approval` > Projekt-`[permissions].default_mode` > globales > Vorgabe des Einstiegs (`auto`) |
| `--model <id>` | Modell der Sitzung (`RuntimeSpec.model_override`) | wird gegen `config.models` geprüft (Schlüssel, dann ID, dann Alias); unbekannt = Konfigurationsfehler. Setzt `default_model`/`default_provider` und hebt einen UIA-Pin für diesen Lauf auf |

Details und Begründung: `tui-roles-models-modes.md`.
