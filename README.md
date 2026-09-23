# Harwness

**A local, security-first agent runtime written in Rust.** Harwness runs language-model agents as supervised work: models propose messages, tool calls, plans and sub-tasks; Rust code decides what is permitted, records what happened, and owns every side effect.

> **Status: early development.** Harwness is intended for developers who want to inspect, embed, or contribute to a security-oriented harness. Interfaces and configuration may change. Treat it as a local development system, not as an unattended production service.

## What Harwness is for

Most agent loops give a model a prompt and a collection of tools. Harwness starts from a different boundary: a prompt is not authority. A model cannot grant itself filesystem access, process execution, network access, child-agent privileges, secret access, or a privileged host action. Those decisions belong to typed runtime components.

The `harw` binary provides an interactive terminal UI, one-shot runs, durable plans and jobs, an optional local web surface, an MCP listener, and a gateway for external channels. The workspace also includes an embeddable SDK and a Defense-on-Device subsystem for collecting and acting on host-security findings under separate privilege boundaries.

### Cloudflare MCP

The managed Cloudflare API MCP server can be configured and checked from the
CLI. Set `CLOUDFLARE_API_TOKEN` in the environment, then run:

```text
harw mcp setup cloudflare
harw mcp check cloudflare
```

The setup writes only a declarative `streamable_http` entry under the active
profile's `mcps/` directory. The check performs MCP `initialize` and
`tools/list` against `https://mcp.cloudflare.com/mcp` and prints the advertised
tool names without printing the token.

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

## Context and model interaction

Context is assembled as a typed program. Sections identify their source, trust class, strength, and detail level. This lets the runtime distinguish instructions from evidence and ordinary data, apply context ceilings, and reduce context predictably when budgets are tight.

The model produces intents. The runtime turns permitted intents into messages or governed tool calls. Unknown tools, unknown permission labels, invalid authority references, and missing required approvals fail closed.

## Tools, approvals, and sandboxing

Tools are registered through explicit Rust interfaces. The tool macro performs permission evaluation before deserializing model-controlled arguments. File access uses containment and no-follow mechanisms; shell, filesystem, dependency, web, browser, and planning operations each receive a scoped policy surface.

Approval results combine conservatively: deny wins over ask, and ask wins over allow. Adding an approval handler can only make execution stricter. Session modes similarly reduce the base tool surface and cannot restore tools forbidden by an executable definition.

## Durable planning, jobs, and knowledge

Harwness supports durable goals, plans, verification criteria, dependencies, execution waves, and evidence. Plans can fan out into child agents while preserving read/write scopes and dependency order. Jobs have budgets, retry policies, leases, and cancellation paths.

Knowledge and transcript components keep local artifacts separate from the model context. A session history can be compacted without splitting atomic tool-call/result groups, and the resulting history can be persisted for later resume.

## Defense-on-Device

The Defense-on-Device subsystem is an optional host-security plane. It contains unprivileged sensors, typed findings and verdicts, an escalation layer, privileged probes, and a socket-activated Warden.

A finding follows typed states such as raw, rule-checked, and triaged. Only the rules and escalation layers can construct the next security state. The Warden verifies a proof bound to the exact action content before accepting a privileged request. This prevents a model response or arbitrary deserialized payload from becoming an enforcement action.

Secrets use a hybrid cryptographic policy, while the audit system maintains tamper-evident chained records with signed checkpoints. The repository forbids `unsafe` workspace-wide.

## Quick start

Build requirements are Rust **1.85** or newer and a normal Rust/Cargo toolchain.

```bash
cargo build -p harw-cli
cargo run -p harw-cli -- init
cargo run -p harw-cli -- onboard
cargo run -p harw-cli --
```

`harw init` creates the local root space. `harw onboard` configures a provider and model. Running `harw` without a subcommand starts the interactive terminal client; passing a prompt runs a one-shot interaction.

Shell completions: `harw completions --install` (detects the shell from `$SHELL`; pass `bash`, `zsh`, `fish`, `elvish` or `powershell` explicitly, add `--dry-run` to preview). Open a new shell afterwards.

Useful commands include:

```bash
harw doctor
harw settings
harw --resume
harw analyze --dry-run
harw gateway
```

Run `harw --help` and `harw <subcommand> --help` for the exact grammar supported by the checked-out version.

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

## Contributing

Contributions should preserve the central security model: do not move authority decisions into prompts, configuration strings, or deserialized wire values; do not add a bypass around runtime assembly; and do not turn a model proposal into a host action without an explicit governed transition.

Keep changes narrow, test the affected crate, and document user-visible configuration or lifecycle changes. Security-sensitive changes benefit from an explanation of the trust boundary, failure behavior, and persistence behavior.

## License

See the repository license files for licensing terms.
