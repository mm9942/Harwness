<p align="center">
  <img src="docs/assets/harwness-icon.svg" alt="Harwness lion mark" width="180">
  &nbsp;&nbsp;&nbsp;
  <img src="docs/assets/harwness-logo.png" alt="Harwness" width="520">
</p>

# Harwness

[![Rust toolkit](https://img.shields.io/badge/Rust-toolkit-blue.svg?style=for-the-badge)](#build-your-own-harness)
[![License: MIT or Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-green.svg?style=for-the-badge)](#license)
[![Documentation](https://img.shields.io/badge/docs-Harwness-yellow.svg?style=for-the-badge)](docs/README.md)
[![CI](https://img.shields.io/github/actions/workflow/status/mm9942/Harwness/ci.yml?branch=dev&style=for-the-badge&label=CI)](https://github.com/mm9942/Harwness/actions/workflows/ci.yml?query=branch%3Adev)
[![GitHub Library](https://img.shields.io/badge/github-lib-black.svg?style=for-the-badge)](https://github.com/mm9942/Harwness/tree/dev)

**Harwness is a modular Rust toolkit for building your own coding harness.** It also ships a ready-to-use harness (`harw`) for running AI agents with explicit permissions, typed tools, durable execution state, and optional host security. The repository contains **180 first-party Cargo packages** at the [`dev` snapshot counted here](https://github.com/mm9942/Harwness/tree/42b85317b15bc2baef362ebd00b51239d7c89324): 179 in the root workspace and one in the separate mobile Rust workspace. [Embed the agent runtime through the SDK or pick individual crates](#build-your-own-harness) to build an application of your own.

The runtime separates model output from authority. Agents may propose actions, but filesystem access, process execution, network access, spawning, secrets, approvals, and privileged host operations are controlled by Rust components outside the model context.

> **Status:** active development. The workspace version is defined in `Cargo.toml`. Interfaces and configuration may change between revisions.

## What Harwness is for

Harwness is for developers running coding, research, and operational agents that need explicit tool permissions, approvals, durable jobs, and traceable execution. Use the `harw` command to run the built-in harness, or [start with the SDK and crates](#build-your-own-harness) to build an application of your own.

The workspace also contains Detect · Orient · Defend (DoD), an optional host-security domain with its own sensors and privileged enforcement components. Running the agent harness does not require enabling DoD.

[Build your own harness](#build-your-own-harness) · [Get started](#installation) · [CLI reference](docs/cli.md) · [Documentation](docs/README.md) · [Contribute](CONTRIBUTING.md) · [Security policy](SECURITY.md)

## Build your own harness

Use Harwness at the level that fits your application. Embed a session runtime in a GUI, service, or developer tool; build a distributable agent; or take only the execution or transport layer you need. Depending on the integration, your application can supply its own interface, model configuration, tools, context, and approval flow. It does not have to launch the `harw` CLI to use the libraries.

| If you want to… | Start here |
| --- | --- |
| Embed agents in a Rust application | [`harwness-sdk`](harwness-sdk/src/lib.rs): build or resume sessions, send turns, stream events, cancel work, and supply tools, context, and an approval handler. The optional `remote` feature exposes remote sessions. |
| Turn Rust functions into tools and operations | [`harw-macros`](harw-macros/src/lib.rs) and [`harw-operations`](harw-operations/src/lib.rs): declare tools with typed arguments, or declare which command, model-tool, agent-tool, and web surfaces an operation supports. |
| Run durable jobs without an agent application | [`harw-job`](harw-job/README.md): submit jobs, collect output, cancel, and recover verified work. Backend selection and reports describe the isolation actually available on the host. |
| Build a standalone agent | [`harw-agent-compiler` and `harw-agent-runner`](docs/guides/agent-compiler.md): compile a versioned definition into an artifact with a rights manifest, optionally packaged into a standalone executable. |
| Host sessions or connect nodes | [`harw-local-host`](harw-local-host/src/lib.rs) composes an in-process session host; [`harw-node-transport`](harw-node-transport/src/lib.rs) provides an authenticated node transport. The transport does not itself grant access to a session. |

### Run an agent from your own Rust binary

Add the SDK as a path dependency from a local Harwness checkout. This example assumes the checkout is next to your application directory.

```toml
[dependencies]
harwness-sdk = { path = "../Harwness/harwness-sdk", default-features = false }
tokio = { version = "1", features = ["macros", "rt", "time"] }
```

Put this in your application's `src/main.rs`:

```rust
use harwness_sdk::prelude::*;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), SdkError> {
    let harwness = Harwness::builder().cwd(".").build()?;
    let mut session = harwness.session()?;
    let report = session.send("Which tests cover this workspace?").await?;

    println!("{:?}: {}", report.status, report.text.unwrap_or_default());
    Ok(())
}
```

Run `cargo run` from your application directory. The SDK runs the session in your process without launching the `harw` executable or connecting to a gateway. The example reads an existing profile from `HARW_HOME` or `~/.harw`, with an active agent and a configured model provider. For a model-free smoke test on a configured profile, see the [offline echo option in the SDK example](harwness-sdk/examples/minimal_chat.rs).

The host application owns the user experience. It can display the turn report, consume live events, and decide how to present approval requests. The SDK is the documented embedding boundary; direct dependencies on lower-level workspace crates do not carry the same compatibility promise. The workspace currently sets `publish = false`; see the source rather than assuming these crates are published independently on crates.io.

### Declare a tool once

In [`harw-macros`](harw-macros/src/lib.rs), `#[derive(Tool)]` derives tool metadata and the argument schema from a Rust type. `#[tool]` builds an executor wrapper from an async function and adds the permission or host checks selected in its declaration. For an operation shared across surfaces, [the `status` operation](harw-ops/src/status.rs) shows how one function serves a slash command, a read-only model tool, and a web route. The declaration is the source of truth for those bindings; registering an operation still does not grant a caller authority.

The built-in harness is one composition of these crates. The [Android on-device host](mobile/rust/src/local.rs) is another: it uses the Rust session host in the app process and attaches platform-specific tools through its own integration. It does not need a separate `harw` executable or a gateway for the on-device host. The exact tool set depends on the app build, host configuration, and model route.

## What you can do with it

### For coding

- **Delegate real work to an agent team.** Describe the goal in the interactive session. The root agent hands it to a background orchestrator that plans, fans out read-only explorers and writing workers, and reports back while you keep talking.
- **Plan first, then build.** Plan mode (`Shift+Tab` or `/mode`) lets the agent explore and ask structured questions without changing files; you approve the plan before any edit is applied.
- **Stay in control of side effects.** Edits, shell commands, network access and host operations go through approvals and sandboxing. Shell commands run in a Bubblewrap sandbox on supported Linux systems; root commands (`host.sudo_exec`) need their own approval window with masked password entry, and the password never reaches the model.
- **Research and review.** Dependency research against crates.io and docs.rs (`/research-deps`), read-only research with a typed finding contract (`/research`), and bundled Rust skills for ownership, error design, typestate and more.
- **Run your own commands.** The `!` operator runs a shell command on the host from the interactive prompt, with its output returned to the conversation.

### For business and analysis

- **Strategic matrix games.** Ask for a matrix game about a strategic question, for example launching a product. The `matrix-game-master` drafts a scenario with actors, goals and rules, asks for your approval, and plays it through with isolated agents in the seats. Dice are rolled by the Rust engine (`harw-matrix-game`), not by the model, and the run ends with an after-action review.
- **Papers and reports as PDF.** The LaTeX skills write structured documents with German or English typography (KOMA-Script, BibLaTeX), build them with XeLaTeX, and check the log for errors and overfull lines.
- **Research with sources.** Questions are handed to a read-only researcher whose answer is validated against a typed finding contract before it is used.

### Every day

- **Knowledge that stays yours:** a workbench for the current task, a kanban board, per-agent diaries, a long-term memory palace and optional idle-time dream reflections, all stored locally and reviewable.
- **Talk to it from your phone** through a paired Telegram bot, with approvals for writes.
- **Use the models you want:** Anthropic, OpenAI, OpenRouter and others, or local models via vLLM, LM Studio and Ollama. Mix them per role, for example local explorers with a cloud orchestrator ([docs/setup/local-models.md](docs/setup/local-models.md)).
- **Extend it:** MCP connectors, your own agent definitions and skills, or embed the runtime with the `harwness-sdk` crate.

## What a session looks like

```text
# illustrative
> Analyse this workspace and harden the key-file permissions, with tests.

  harw  I'll hand this to an orchestrator in the background.
  - root-orchestrator · running · 3 tool calls
    |- explorer   "map key handling in src/key_control"      done
    `- worker     "write 0600 permissions + regression test"  waiting for approval

  Approval · fs.edit · src/key_control/file.rs
  +  permissions.set_mode(0o600);
  [once] [session] [deny]

> ! cargo test -p key_control
  ! cargo test -p key_control  -> exit 0, ran on the host

  harw  Done: permissions are now 0600, one new test, all tests pass.
        Changed: src/key_control/file.rs, tests/key_permissions.rs
```

`/export` writes the session, including tool calls, command output and background activity, to Markdown or JSON.

## Architecture

```text
CLI / TUI / Web / MCP / Gateway
              |
              v
        RuntimeAssembly
              |
      +-------+--------------------+
      |       |                    |
      v       v                    v
   Agent   Authority /         Durable state
  runtime  approvals           sessions, plans,
      |     sandbox            jobs, knowledge
      |
      +----------+-------------+
                 |
                 v
       providers + governed tools

Optional host-security plane:
Sensors -> typed findings -> escalation -> Warden
```

The workspace is organized around a few boundaries:

- **Agent runtime** — session execution, context assembly, model interaction, tool dispatch, child-agent execution, and runtime composition.
- **Agent DSL and IR** — versioned agent definitions are parsed, resolved, authority-checked, lowered to executable IR, and content-addressed.
- **Authority and approvals** — runtime entry points derive their available authority from typed profiles. Child agents and modes can reduce authority but cannot expand it.
- **Governed tools** — model-controlled operations use explicit Rust interfaces and scoped permissions.
- **Sandbox and job runtime** — long-running work, process execution, cancellation, resource policy, and platform-specific execution are separate runtime concerns.
- **Durable state** — sessions, plans, jobs, pairing state, transcripts, and selected knowledge artifacts survive process restarts.
- **Providers and interfaces** — provider adapters are separated from the agent runtime; the workspace exposes CLI/TUI, web, MCP, channel, and SDK integration points.
- **Detect · Orient · Defend (DoD)** — an optional peer security domain with separate sensors, escalation, privileged probes, and Warden enforcement.

## Security model

Harwness treats prompts, model responses, configuration data, and external messages as data rather than authority.

The main invariants are:

1. **Authority is derived, not prompted.** Runtime entry points have typed authority profiles.
2. **Agent definitions are compiled.** Definitions are checked before becoming executable IR.
3. **Privileges shrink across derivation.** A derived definition, mode, or child agent cannot restore authority removed by its parent boundary.
4. **Side effects are governed.** Tool permission and approval checks are performed outside model reasoning.
5. **Privileged host actions are separate.** Ordinary shell execution does not become root execution because a model requested it.
6. **Untrusted data does not construct privileged principals.** Serialization for audit does not imply deserialization is accepted as authority.
7. **DoD enforcement is a separate transition.** A model verdict or finding is not itself a privileged host action.
8. **First-party Rust forbids unsafe code.** The workspace sets `unsafe_code = "forbid"`.

See [SECURITY.md](SECURITY.md) for the operational security invariants and vulnerability reporting process.

## Agent definitions

Agent definitions use a versioned TOML DSL. They describe role, specialization, lifecycle constraints, context policy, admitted and forbidden tools, spawn limits, budgets, and return contracts.

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

Definitions resolve through ordered layers. The compiler rejects authority elevation and lowers the resolved definition to `ExecutableAgentIr`. Executable snapshots are content-addressed with a versioned BLAKE3 domain.

The role set and spawn relationships are enforced by Rust runtime policy rather than arbitrary TOML values.

See [docs/design/agent-definition-dsl.md](docs/design/agent-definition-dsl.md).

## Agent compiler

`harw agent build` compiles an agent definition into a standalone artifact with its rights manifest and runtime interface configuration.

```bash
harw agent check evidence-critic
harw agent build evidence-critic --interface cli,mcp -o ./ec
./ec --manifest
```

The compiler and runner support constrained standalone agent execution. Runtime options may narrow baked-in rights but do not expand them.

See [docs/guides/agent-compiler.md](docs/guides/agent-compiler.md) and [ADR 0001](docs/adr/0001-agent-compiler.md).

## Tools and approvals

Tools are registered through explicit Rust interfaces. Permission evaluation occurs before model-controlled arguments are accepted by the tool implementation.

The workspace includes tool surfaces for filesystem operations, shell execution, dependencies, web access, browser operations, planning, jobs, process control, research, and other runtime capabilities.

Approval decisions combine conservatively:

```text
deny > ask > allow
```

An additional approval layer can make a decision stricter, not more permissive. Session modes similarly restrict the authority already available to the executable agent definition.

Examples of enforced separation:

- `fs.edit` is subject to the same scoped write policy and approval boundary as other filesystem writes.
- `shell.exec` does not accept `sudo`, `doas`, or `pkexec` as an implicit privilege path.
- privileged host execution uses a separate host operation and approval boundary.
- `process.kill` is governed independently from ordinary shell execution.

## Jobs and process execution

Harwness contains a durable job subsystem rather than treating background execution as an ad-hoc terminal concern.

The job crates separate core job state, persistence, platform execution, Tokio integration, Bubblewrap execution, and higher-level orchestration. Jobs can carry budgets, lifecycle state, cancellation, retry policy, and leases.

Process control is implemented separately in `harw-killer` and `harw-tool-process`. On supported Linux systems, process identity uses pidfds to avoid targeting a reused PID. The agent-facing process tool does not use privilege escalation to kill another user's process.

See [harw-killer/README.md](harw-killer/README.md).

## Durable planning and knowledge

Plans, goals, verification criteria, dependencies, execution waves, and evidence can be represented independently from a single model turn. Plans may fan out into child work while retaining dependency and scope constraints.

Session and knowledge components persist state outside the transient model context. Transcript compaction preserves atomic tool-call/result groups so context reduction does not corrupt the execution record.

## Providers and interfaces

Harwness separates provider integration from runtime authority.

The workspace contains provider abstractions and HTTP-backed provider integration, together with model catalog support. Local or remote model deployment is therefore a provider/configuration concern rather than a change to the authority model.

User and integration surfaces include:

- `harw` CLI
- interactive TUI
- web components
- MCP server and client
- external channels, including Telegram components
- `harwness-sdk` for embedding runtime functionality

See [docs/setup/local-models.md](docs/setup/local-models.md) for local model configuration.

## Harw-DoD (Detect · Orient · Defend)

Harw-DoD (Detect · Orient · Defend) is an optional security suite and a peer domain in the workspace. It is not an implicit privilege extension for ordinary agents. "Harw-DoD" is the suite name; its crates, binaries, paths and units keep their `harw-dod`/`harw-*` names.

Its crates cover host observation and security functions including eBPF integration, filesystem monitoring, process monitoring, cgroups, network policy, rules, findings, escalation, probes, Sentinel, and Warden components.

The privileged Warden is deliberately separated from model execution. Enforcement requests require the expected typed authorization/proof path before privileged actions are accepted.

The unprivileged sentinel exports findings and its own health to `/var/lib/harw-dod/export`, readable by members of the (empty by default) `harw-dod-readers` group; harw only reads these files.

Harw-DoD installation and enablement are explicit operations:

```bash
make dod-build
sudo make dod-install
sudo make dod-enable
```

See [docs/setup/dod.md](docs/setup/dod.md).

## Installation

### Source installer

```bash
curl -fsSL https://raw.githubusercontent.com/mm9942/Harwness/main/scripts/install.sh | bash
```

The source-install path downloads the repository archive, installs required build tooling when needed, and runs the project installation target from a persistent source directory.

### Build from source

```bash
git clone https://github.com/mm9942/Harwness.git
cd Harwness
make install
harw init
harw onboard
```

The Rust toolchain is pinned by the repository. By default, `make install` installs `harw`, `harw-agent-runner`, and, on supported non-Android systems, `killer` into `~/.local/bin`.

Use `make help` for the available build and installation targets.

For complete installation details, see [docs/setup/install.md](docs/setup/install.md).

### Android

Harw Mobile connects an enrolled Android device to a Harw gateway. Onion profiles use embedded Arti on Android and on the gateway. Gateway restarts reuse the enrolled Onion identity when its key is available.

On builds with on-device UIA enabled, the Assistant offers a model picker in local and cloud sessions. The local Library also offers a model picker for image descriptions. Choose a vision-capable local GGUF or a configured provider model with image input.

Use the [phone setup and test guide](mobile/PHONE-TEST.md) for enrollment and device checks, and the [Android release guide](mobile/RELEASE-APK.md) to build and sign an APK.

## Basic usage

Start an interactive session:

```bash
harw
```

Run a non-interactive request:

```bash
harw exec "Inspect this workspace and report the relevant architecture."
```

Common administrative and inspection commands include:

```bash
harw doctor
harw config
harw session list
harw provider scan
harw jobs list
harw gateway
```

The checked-out binary is the source of truth for exact command grammar:

```bash
harw --help
harw <subcommand> --help
```

See [docs/cli.md](docs/cli.md) for the maintained command reference.

## Development

The repository's canonical verification target is:

```bash
make clippy-tests
```

It runs:

```text
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
make gates
```

`make gates` runs the `xtask` architecture gates, including dependency-graph, privilege, and Warden structure checks.

Other useful targets:

```bash
make fmt
make check
make clippy
make tests
make gates
make build
```

See [CONTRIBUTING.md](CONTRIBUTING.md) before changing runtime authority, workspace dependency direction, persistence semantics, or security boundaries.

## Repository structure

The workspace is intentionally split into small crates with explicit ownership boundaries. Important groups include:

| Area | Crates |
| --- | --- |
| CLI / UI | `harw`, `harw-cli`, `harw-tui`, `harw-tui-layout`, `harw-completions` |
| Runtime | `harw-runtime`, `harw-core`, `harw-operations`, `harw-ops` |
| Agent definitions | `harw-agent-dsl`, `harw-agent-compiler`, `harw-agent-artifact`, `harw-agent-runner` |
| Authority / tools | `harw-authority`, `harw-tools`, `harw-registry-defaults`, `harw-tool-*` |
| Jobs | `harw-job-core`, `harw-job-runtime`, `harw-job-store`, platform/executor crates |
| State / knowledge | `harw-session-store`, `harw-plan`, `harw-memory`, `harw-knowledge` |
| Providers | `harw-provider`, `harw-provider-http`, `harw-model-catalog` |
| Channels / MCP | `harw-channel*`, `harw-mcp-server`, `harw-mcp-client` |
| Browser / web | `harw-browser*`, `harw-web`, `harw-egress` |
| SDK / extensions | `harwness-sdk`, `harw-extension-api` |
| DoD | `harw-dod/crates/harw-dod-*`, `harw-sentinel`, `harw-warden`, probe crates |

Workspace membership does not imply arbitrary dependency permission. Architecture gates enforce dependency and privileged-TCB constraints separately.

## Current vs planned architecture

Repository documentation contains both implemented design and forward planning. Planning documents are not evidence that a feature is active in a production entry path.

In particular, control-plane and transport work described under planning directories should be treated as planned until the corresponding runtime code, composition path, and verification gates are present.

When changing Harwness, prefer this source-of-truth order:

1. current source and Cargo metadata
2. tests and executable architecture gates
3. implemented operational/design documentation
4. accepted ADRs
5. planning documents

## Documentation

- [docs/README.md](docs/README.md) — documentation index
- [docs/cli.md](docs/cli.md) — CLI reference
- [docs/design/agent-definition-dsl.md](docs/design/agent-definition-dsl.md) — agent DSL
- [docs/guides/agent-compiler.md](docs/guides/agent-compiler.md) — standalone agent compiler
- [docs/setup/install.md](docs/setup/install.md) — installation
- [docs/setup/local-models.md](docs/setup/local-models.md) — local models
- [docs/setup/dod.md](docs/setup/dod.md) — Detect · Orient · Defend
- [docs/philosophy/philosophy.md](docs/philosophy/philosophy.md) — project principles
- [docs/philosophy/coding-philosophy.md](docs/philosophy/coding-philosophy.md) — coding principles

## Contributing

Keep authority decisions in typed runtime code rather than prompts or untrusted configuration. Do not introduce bypasses around runtime assembly or convert model output directly into privileged host actions.

Changes should preserve the workspace's dependency and privilege boundaries, include tests for affected behavior, and document user-visible lifecycle or configuration changes.

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Harwness is dual-licensed under either:

- [MIT](LICENSE-MIT)
- [Apache License 2.0](LICENSE-APACHE)

at your option.
