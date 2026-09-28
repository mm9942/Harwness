<p align="center"><img src="docs/assets/harwness-logo.png" alt="Harwness" width="480"></p>

# Harwness

Harwness (`harw`) is a Rust workspace for running governed AI agents with explicit authority, typed tools, durable execution state, and optional host-security enforcement.

The runtime separates model output from authority. Agents may propose actions, but filesystem access, process execution, network access, spawning, secrets, approvals, and privileged host operations are controlled by Rust components outside the model context.

> **Status:** active development. The workspace version is defined in `Cargo.toml`. Interfaces and configuration may change between revisions.

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

## Detect · Orient · Defend

Detect · Orient · Defend is an optional security subsystem and a peer domain in the workspace. It is not an implicit privilege extension for ordinary agents.

Its crates cover host observation and security functions including eBPF integration, filesystem monitoring, process monitoring, cgroups, network policy, rules, findings, escalation, probes, Sentinel, and Warden components.

The privileged Warden is deliberately separated from model execution. Enforcement requests require the expected typed authorization/proof path before privileged actions are accepted.

DoD installation and enablement are explicit operations:

```bash
make dod-build
sudo make dod-install
sudo make dod-enable
```

See [docs/setup/dod.md](docs/setup/dod.md).

## Installation

### Source installer

```bash
curl -fsSL https://get.harw.dev/harw/install.sh | bash
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
| DoD | `dod/crates/harw-dod-*`, `harw-sentinel`, `harw-warden`, probe crates |

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
