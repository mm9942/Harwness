# Harwness

**A local, security-first agent runtime written in Rust.** Harwness runs language-model agents as supervised work: models propose messages, tool calls, plans and sub-tasks; Rust code decides what is permitted, records what happened, and owns every side effect.

> **Status: early development.** Harwness is intended for developers who want to inspect, embed, or contribute to a security-oriented harness. Interfaces and configuration may change. Treat it as a local development system, not as an unattended production service.

## What Harwness is for

Most agent loops give a model a prompt and a collection of tools. Harwness starts from a different boundary: a prompt is not authority. A model cannot grant itself filesystem access, process execution, network access, child-agent privileges, secret access, or a privileged host action. Those decisions belong to typed runtime components.

The `harw` binary provides an interactive terminal UI, one-shot runs, durable plans and jobs, an optional local web surface, an MCP listener and MCP client connections, and a gateway for external channels. The workspace also includes an embeddable SDK and a Defense-on-Device subsystem for collecting and acting on host-security findings under separate privilege boundaries.

The current workspace version is **0.3.0**.

## Core principles

- **Authority is derived, never prompted.** Every runtime entry point has an `EntryKind`; its tool surface, permission tier, approval behavior, context ceiling, and spawning ability are derived from a central runtime profile.
- **Definitions are compiled.** Agent definitions are versioned TOML documents. They are parsed, resolved through layers, checked for authority elevation, lowered into an executable IR, and content-addressed with a snapshot ID.
- **Privileges only shrink.** Derived definitions cannot add capabilities beyond their base. Modes and child agents intersect with existing rights rather than replacing them.
- **Side effects are governed.** Tools are deny-by-default, permissions are checked before model-controlled arguments are parsed, and approval handlers can restrict but never expand a decision.
- **State is durable.** Sessions, transcripts, pairing records, plans, goals, jobs, and selected knowledge artifacts are persisted so restarts do not silently lose their meaning.
- **Secrets stay out of ordinary configuration.** Provider and channel configuration use references such as `env:NAME` or supported secret boundaries, never literal tokens.
- **Host enforcement is separated.** DoD sensors, triage, authorization, and the privileged Warden are separate components. A model verdict is a proposal; an authorized enforcement action is a distinct typed state.

## Architecture at a glance

```text
CLI / TUI / Web / MCP / Gateway
             │
             ▼
       RuntimeAssembly
             │
   ┌─────────┼──────────────────────┐
   ▼         ▼                      ▼
Agent     Policy & approval     Durable stores
session   Tool registry         sessions, plans,
context   Sandbox rights        jobs, pairing, knowledge
   │
   ▼
Model provider and governed tools
```

The composition root constructs the runtime. Libraries expose typed capabilities and do not silently create elevated principals from external data. In particular, `Principal` can be serialized for auditing but cannot be deserialized from an untrusted wire message.

## Agent Definition DSL

An agent definition is a compiled contract rather than a free-form system prompt. Definitions declare a closed role, specialization, work contract, lifecycle constraints, context program, tool surface, spawn budget, and return contract.

```toml
schema = "harwness.agent/v1"
id = "example.agent.explorer@1"
version = "1.0.0"
role = "worker"
specialization = "explorer"
name = "Explorer"
description = "Read-only workspace exploration with evidenced findings."

[tools]
admitted = ["fs.read", "fs.list", "fs.search"]
forbidden = ["fs.write", "shell.exec", "web.fetch"]

[spawn]
max_depth = 1

[spawn.budget]
max_tokens = 60000
max_tool_calls = 40
max_wall_secs = 180

[return]
contract = "harwness.return.research-finding@1"
```

Definitions resolve in ordered layers. A derived definition may remove or intersect authority but cannot elevate it. The compiled `ExecutableAgentIr` is hashed under a versioned BLAKE3 domain, giving runs an auditable definition snapshot.

Roles are a closed Rust enum. The spawn matrix is enforced by the runtime and cannot be expanded from TOML. A worker therefore cannot become a root orchestrator because a prompt, plugin, or local config says so.

The full specification is in [docs/design/agent-definition-dsl.md](docs/design/agent-definition-dsl.md).

## Context and model interaction

Context is assembled as a typed program. Sections identify their source, trust class, strength, and detail level. This lets the runtime distinguish instructions from evidence and ordinary data, apply context ceilings, and reduce context predictably when budgets are tight.

The model produces intents. The runtime turns permitted intents into messages or governed tool calls. Unknown tools, unknown permission labels, invalid authority references, and missing required approvals fail closed.

## Tools, approvals, and sandboxing

Tools are registered through explicit Rust interfaces. The tool macro performs permission evaluation before deserializing model-controlled arguments. File access uses containment and no-follow mechanisms; shell, filesystem, dependency, web, browser, and planning operations each receive a scoped policy surface.

Targeted edits use `fs.edit` (replace one exact match, or all with `replace_all`) under the same limits and approval as `fs.write`. `shell.exec` refuses `sudo`, `doas` and `pkexec`; root commands go only through the approval window of `host.sudo_exec`.

Approval results combine conservatively: deny wins over ask, and ask wins over allow. Adding an approval handler can only make execution stricter. Session modes similarly reduce the base tool surface and cannot restore tools forbidden by an executable definition.

## Durable planning, jobs, and knowledge

Harwness supports durable goals, plans, verification criteria, dependencies, execution waves, and evidence. Plans can fan out into child agents while preserving read/write scopes and dependency order. Jobs have budgets, retry policies, leases, and cancellation paths.

Knowledge and transcript components keep local artifacts separate from the model context. A session history can be compacted without splitting atomic tool-call/result groups, and the resulting history can be persisted for later resume.

## Defense-on-Device

The Defense-on-Device subsystem is an optional host-security plane. It contains unprivileged sensors, typed findings and verdicts, an escalation layer, privileged probes, and a socket-activated Warden.

A finding follows typed states such as raw, rule-checked, and triaged. Only the rules and escalation layers can construct the next security state. The Warden verifies a proof bound to the exact action content before accepting a privileged request. This prevents a model response or arbitrary deserialized payload from becoming an enforcement action.

Secrets use a hybrid cryptographic policy, while the audit system maintains tamper-evident chained records with signed checkpoints. The repository forbids `unsafe` workspace-wide.

## Quick start

The toolchain is pinned to Rust **1.98.0** in `rust-toolchain.toml`; rustup installs it on the first `cargo` call, and CI uses the same file. The manifest MSRV floor stays at 1.85. Details: [docs/setup/build-prerequisites.md](docs/setup/build-prerequisites.md).

```bash
cargo build -p harw-cli
cargo run -p harw-cli -- init
cargo run -p harw-cli -- onboard
cargo run -p harw-cli --
```

`harw init` creates the local root space. `harw onboard` configures a provider and model. Running `harw` (or `harw chat`) without further arguments starts the interactive terminal client in `chat` mode, optionally with a first prompt; `harw exec PROMPT` runs a single non-interactive request and exits. Pick another start mode with `--mode` (or `[mode] default` in the profile configuration), and another root agent definition with `--agent NAME`; `/agent use NAME` inside the TUI stores that choice for the next session.

Shell completions: `harw completions --install` (detects the shell from `$SHELL`; pass `bash`, `zsh`, `fish`, `elvish` or `powershell` explicitly, add `--dry-run` to preview). Open a new shell afterwards.

Useful commands include:

```bash
harw doctor
harw config                          # interactive settings menu (alias: settings)
harw session list                    # saved sessions; resume with `harw session resume ID` or `harw -r`
harw provider scan                   # query providers for available models
harw model default ID                # set the default model (alias: models)
harw jobs list                       # background jobs; approve/deny with --note/--reason
harw analyze --order top-down --dry-run
harw gateway
```

Global flags (`--home`, `--profile`, `-C/--cwd`, `--log`, `-v`, `--json`) work with every command. Session flags (`--mode`, `--approval ask|auto|full`, `--model`, `--goal`, `--agent`, `--add-dir`) apply only to `chat`, `exec` and `analyze` and are rejected elsewhere. Commands without a JSON form fail with `--json` instead of ignoring it. Older spellings such as `harw settings`, `harw models`, `harw connect`, `harw lens` or `harw run` still work and print a hint to the new name.

The full command reference (in German), including the old-to-new mapping, is in [docs/cli.md](docs/cli.md). Run `harw --help` and `harw <subcommand> --help` for the exact grammar supported by the checked-out version.

## Working in the terminal UI

Slash commands are typed operations with the same permission checks on every front-end; `/help` (or `F1`) lists what the checked-out version registers. A few behaviors worth knowing:

- **Commands during a running turn.** Read-only commands such as `/status`, `/usage`, `/diff` or `/agent` run immediately without blocking the turn. Changes such as `/model`, `/effort` or `/mode` are accepted during the turn but take effect from the next turn. Everything else waits until the turn ends. Messages you send while a turn runs are shown above the composer as "Wartet auf den nächsten Turn" until they are delivered.
- **Interrupting.** `Ctrl+C` or `Esc` stops the running turn (model call, shell subprocesses, child agents). Messages and commands already queued are kept and delivered right afterwards. `Esc` closes an open popup first and never quits; pressing `Ctrl+C` twice quits. While child agents are running, the first `Esc` only asks for confirmation and a second `Esc` interrupts. `Enter` during a turn always queues the message and never interrupts.
- **Side questions.** `/btw QUESTION` asks a quick question about the conversation without interrupting the agent. It uses no tools, is not added to the history, and can be cancelled with `Esc`.
- **Auto mode.** With approval `auto`, calls outside the fixed auto-approved list go through a deterministic pre-filter and a small classifier model (`[internal_models.auto_classifier]`). Anything unclear, failing or slow falls back to asking; tools that always ask (e.g. `process.kill`, `host.sudo_exec`) are never auto-approved. After 3 denials in a row or 20 in a session, the session drops back to `ask`. Own rules with argument patterns: `/permissions allow|deny TOOL [PATTERN]`, `/permissions rules`, `/permissions log`.
- **Plan mode.** `Shift+Tab` cycles `ask → auto → full → plan`. In plan mode (“⏸ plan mode on”) writing and executing tools are blocked immediately; the agent explores, may ask you structured questions (`ask_user`), writes its plan to `.harw/plans/` and asks to leave plan mode with `plan.exit`. You choose: implement with `auto`, implement with `ask`, or keep planning with feedback. The approved plan stays pinned in the context. `/plan show|list|open|edit` manages plan files.
- **Root commands.** Host shell workers can request a root command through `host.sudo_exec`. The TUI shows the exact command in its own window; the password is typed there, masked, and never reaches the model, the history, logs or disk. Choose “once” or “for this session” (kept in memory for `[host] sudo_session_minutes`, default 10). Every root command still needs its own approval.
- **Research.** `/research QUESTION` hands a bounded question to a read-only child agent and validates the result against a typed finding contract. `/research-deps` checks Rust dependencies; `/research-deps --generic` uses an ecosystem-neutral dependency researcher. These commands are available when the planning surface (`[tools.plan] enabled`) is on.
- **Child agents.** A child agent gets the context window of the model it actually calls, falling back to the parent's model rather than a small default. Its token budget counts only new, uncached input plus output. Near the limit the child is asked to wrap up; at the limit it writes a structured handoff (falling back to its last answer), and the parent can continue it with `continue_from`.
- **Background orchestrators.** An orchestrator started by the UIA in the TUI runs in the background; the UIA's turn ends right away and a new turn starts when the result arrives. The UIA can query (`agent.status`, `agent.result`), message (`agent.message`) and cancel (`agent.cancel`) its orchestrators, and children can report back with `parent.message`. `/agent` shows the live agent tree, `/agent bg` lists background runs, `/agent stream` controls the live output of child agents. Limits are set in `[agents]`. Guide (German): [docs/guides/hintergrund-agenten.md](docs/guides/hintergrund-agenten.md).
- **Learning loop.** `/learn` scans the session for durable insights and files them as proposals only. Nothing is written to memory, skills or agent definitions until you accept a proposal and apply it yourself.

## Knowledge surfaces

Harwness keeps local knowledge in one store under the active profile (`<profile>/knowledge`), with one visibility model and file locks for concurrent processes. Agents can read these surfaces through read-only tools (`workbench.show`, `diary.read` for their own entries, `palace.search`/`palace.recall` for established nodes). `kanban.list`/`kanban.show` are offered only to the UIA root and the root orchestrator, always ask for approval, and are used only when the user explicitly asks — nothing is put on the board automatically. All writes are operator commands or go through a review step.

- **Workbench** (`/workbench`, `F5`): pinned files, notes and hypotheses for the current session or project (`--scope`). Notes can be edited or removed (`note edit|rm`), and each scope has its own retention (`retention`).
- **Kanban** (`/kanban`, `F6`): cards on boards with comments, evidence links and history. Cards in a worker lane are picked up by the job worker of `harw serve`, but a worker agent starts only after the card has been approved (`/kanban approve`); the result is written back to the card.
- **Diary** (`/diary`): a per-agent log. Entries are written automatically after a compaction and at the end of a session, and can be added by hand (`/diary note` or the `#` prefix). It supports date ranges and search. Retention is configured with `[knowledge.diary] retention_days`.
- **Palace** (`/palace`): a linked long-term memory graph. `/memory promote FACT-ID` turns a stored fact into a provisional topic; `/palace promote` makes it established. `supersede`, `edit` and `link` change nodes behind an operator review gate; nodes are never deleted.
- **Dream** (`/dream run|status|review`): an idle-time reflection run without tools. It produces structured suggestions (topics, diary reflections, skill or agent ideas, follow-ups) and does knowledge maintenance. Suggestions only take effect when accepted with `/dream review`. The gateway schedules runs according to the `[dream]` section (`enabled`, `budget`, `idle_minutes`, `cooldown_minutes`, `schedule`).

The design and its remaining open points are described in [docs/design/knowledge-surfaces.md](docs/design/knowledge-surfaces.md); the full command grammar is in [docs/design/interaction-contract.md](docs/design/interaction-contract.md) §2.2.

## Matrix game

`/matrix` runs an umpired, multi-seat matrix game with agents in the seats. A deterministic game master in Rust owns turns, dice, visibility and the journal; seats only see their own projection of the game. Scenarios can include behavior profiles, a Red Cell seat and inject packages (`/matrix start SCENARIO --package ID`); `/matrix compare` compares runs. `F9` opens the matrix panel. Design notes: [docs/design/matrix-game.md](docs/design/matrix-game.md).

## Bundled roles and skills

The binary ships a starter set of agent definitions and skills that `harw init` writes into the Harwness home. Recent additions include:

- **LaTeX worker.** The role `uia-latex-writer` (and the bundle agent `latex-writer`) writes and edits LaTeX in the workspace. It has no shell, network or dependency tools. It can build only through the typed `latex.build` tool, which runs `latexmk` inside the sandbox without shell escape and asks for approval on every call. TeX must already be installed; if it is missing, the tool reports which programs are missing and how to install them, and installs nothing itself.
- **Skills:** `business-writing-pyramid`, `latex-writing`, `xelatex-compile`, `learning-loop`, `author-review-pipeline` and `matrix-scenario-design`.

## MCP connectors

Besides the MCP listener (`harw serve`), Harwness can act as an MCP client. Each connector is a declarative file under the active profile's `mcps/` directory with a transport (`stdio` or `streamable_http`), a command or URL, an optional credential reference such as `env:NAME`, and an allowlist of tools. Enabled connectors are connected when the runtime is assembled, and their tools appear as `mcp.<server>.<tool>`. They are governed like any other tool: HTTP connectors need network access to that host, stdio connectors need process execution. A server that cannot be reached is left out and logged; it does not stop the session.

`harw mcp setup NAME` writes a connector file and `harw mcp check NAME` performs MCP `initialize` and `tools/list` and prints the advertised tool names without printing the credential. At present these two helpers know one preset:

- `cloudflare` (Cloudflare's managed MCP server, token from `CLOUDFLARE_API_TOKEN`).

Other servers are configured by writing their file under `mcps/` directly.

## Telegram setup and pairing

Telegram is deliberately opt-in. The channel does not start merely because a token exists, and the gateway only accepts an enabled binding with at least one pinned identity.

First create a bot with BotFather. Export the token in the shell where Harwness will run; do not paste the token into a TOML file:

```bash
export HARW_TELEGRAM_BOT_TOKEN='123456:replace-with-your-bot-token'
harw connect --channel telegram
```

The setup command verifies the bot with Telegram, writes a disabled channel configuration containing only `env:HARW_TELEGRAM_BOT_TOKEN`, and prints a short-lived pairing code. Send the exact command printed by Harwness to the bot from the private account you want to authorize:

```text
/pair ABCD-EFGH
```

Then redeem that code locally:

```bash
harw connect --channel telegram --pair ABCD-EFGH
```

Redemption fetches the bot updates, requires an exact private `/pair CODE` message, and atomically binds the one-time code to that Telegram identity. It then pins the identity, enables the binding, and persists the pairing record. Start the gateway only after that succeeds:

```bash
harw gateway
```

Keep the bot token environment variable available to the gateway process as well. If it is started by a service manager, configure the environment in the service context rather than relying on an interactive shell. During the manual pairing step, do not let another gateway instance consume the bot updates.

Local TUI shell commands use the `!command` syntax and are enabled by default for the local operator console. They execute only through the Bubblewrap-bound `shell.exec` executor and still require the runtime sandbox to grant process execution. To disable this surface for one Harwness process, start it with `HARW_DISABLE_SHELL=1 harw`. External channels remain unable to invoke the local shell unless their separate channel policy explicitly grants that capability.

A Telegram chat bound to a workspace gets a narrow tool profile: reading files runs without asking, every write asks for approval through an inline button in the chat, and there is no shell, process, network or knowledge-surface tool.

The spelling is `telegram`; `telegaram` is intentionally rejected rather than silently configuring an unexpected channel.

## Configuration model

Configuration is layered. Built-in defaults, user configuration, active profile configuration, and a trusted project layer are merged in a controlled precedence order. Catalog references are validated before a runtime is constructed.

The active profile lives under the Harwness home. Its exact location is determined by `--home`, `HARW_HOME`, or the platform home convention. Provider, model, channel, agent-definition, and selected state files are kept in separate directories to make their lifetimes explicit.

Repository-local configuration is not automatically trusted. Use the project trust command when a project should be permitted to contribute its local `.harw` layer:

```bash
harw project trust .
harw project status .
```

Use secret references rather than literal credentials. The configuration validator rejects known plaintext credential fields and invalid channel security settings.

## Development

The workspace is a large multi-crate Rust repository. Prefer focused checks while working on one component:

```bash
cargo test -p harw-channel
cargo test -p harw-channel-telegram
cargo test -p harw-cli
cargo fmt --check
cargo clippy --workspace --all-targets
```

The project has substantial inline documentation and tests. When changing a public boundary, keep the relevant crate documentation, configuration validation, runtime composition, and tests aligned. A component described as implemented is not necessarily reachable from every production entry point; verify the specific entry path you are changing.

## Repository layout

- `harw-cli` contains the `harw` binary, command grammar, setup paths, and composition roots.
- `harw-tui` implements the interactive terminal interface.
- `harw-runtime`, `harw-core`, `harw-operations`, and `harw-tools` provide runtime assembly, sessions, operations, and governed tools.
- `harw-agent-dsl` compiles agent definitions and context programs.
- `harw-channel`, `harw-channel-telegram`, and `harw-channel-telegram-transport` provide channel policy, Telegram policy, and transport integration.
- `harw-plan`, `harw-job-runtime`, `harw-knowledge`, and `harw-session-store` provide durable work and state.
- `harw-secrets` implements protected secret storage and audit-chain support.
- `harw-dod-*`, `harw-sentinel`, `harw-probe-*`, and `harw-warden` implement the optional host-security plane.
- `docs` contains design notes, operational documentation, and remediation material.

## Further reading

- [docs/philosophy/philosophy.md](docs/philosophy/philosophy.md) — the principles behind the harness.
- [docs/philosophy/coding-philosophy.md](docs/philosophy/coding-philosophy.md) — how code in this repository is written.
- [docs/design/agent-definition-dsl.md](docs/design/agent-definition-dsl.md) — the agent definition DSL in full.
- [docs/cli.md](docs/cli.md) — command-line reference (German).
- `docs/sessions/` — transcripts and reports of past working sessions.

## Contributing

Contributions should preserve the central security model: do not move authority decisions into prompts, configuration strings, or deserialized wire values; do not add a bypass around runtime assembly; and do not turn a model proposal into a host action without an explicit governed transition.

Keep changes narrow, test the affected crate, and document user-visible configuration or lifecycle changes. Security-sensitive changes benefit from an explanation of the trust boundary, failure behavior, and persistence behavior.

## License

See the repository license files for licensing terms.
