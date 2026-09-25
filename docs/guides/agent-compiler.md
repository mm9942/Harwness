# Compiling agents into standalone binaries

> Status: **planned in #22.** The commands and flags in this guide are the
> decided interface from [ADR 0001](../adr/0001-agent-compiler.md); they are
> not in a release yet. Until they land, `harw agent build` and the runner
> flags shown here do not exist.

This guide shows how to turn an agent definition into a single executable
that runs one fixed agent with one fixed set of rights, without a harw
installation on the machine where it runs. The binary format is described
in [`agent-artifact-v1.md`](../design/agent-artifact-v1.md), the definition
language in [`agent-definition-dsl.md`](../design/agent-definition-dsl.md).

## 1. How it works

```text
definition.toml + system.md + skills
        │  harw agent check     (diagnostics only)
        │  harw agent build     (compile)
        ▼
AgentIr ─► artifact (IR + instructions + skills + knowledge, BLAKE3-hashed)
        │
        ├─ default:  appended to the prebuilt harw-agent-runner ─► ./ec
        └─ --native: generated Rust crate, cargo build           ─► ./ec
```

The built binary contains the agent's **rights manifest**: which tools it
may use, which hosts it may reach, where it may write, its budgets and the
environment variables it needs. At runtime you can narrow those rights with
flags; you can never widen them.

## 2. Write a definition

A compiled agent is an ordinary definition, usually in
`~/.harw/agents/<name>/definition.toml` or a project's `.harw/agents/`,
with its instructions in `system.md` next to it. Two tables matter for
compiling:

```toml
schema = "harwness.agent/v1"
id = "acme.agent.evidence-critic@1"
version = "1.0.0"
extends = { id = "harwness.agent.worker-base@1" }
role = "worker"
specialization = "evidence-critic"
name = "Evidence Critic"
description = "Reviews the evidence of an analysis product. Read-only."
instructions_file = "system.md"
skills = ["evidence-quality-review"]

[tools]
admitted = ["fs.read", "fs.list", "fs.grep", "skills.load"]
forbidden = ["fs.write", "shell.exec", "web.fetch"]

[spawn.budget]
max_tokens = 80000
max_tool_calls = 50
max_wall_secs = 600

[return]
contract = "harwness.return.execution-summary@1"

[models]
provider = "anthropic"
model = "claude-sonnet-5"
effort = "high"
fallbacks = ["openai/gpt-5.6-codex"]
required_env = ["ANTHROPIC_API_KEY"]

[binary]
name = "evidence-critic"
interfaces = ["cli", "mcp"]
default_interface = "cli"
```

`[models]` and `[binary]` are documented in
[DSL §8.1 and §8.2](../design/agent-definition-dsl.md#81-model-preferences-models).
The bundled `evidence-critic` definition works as is; `[binary]` is
optional and defaults to the one-shot CLI.

## 3. Check it

```sh
harw agent check evidence-critic
```

`check` runs parsing, resolution, lowering and the compiler passes and
prints diagnostics with a stable code, the file and line, and a hint:

```text
error[HARW-BINARY-002]: default_interface `http` is not in `interfaces`
  --> ~/.harw/agents/evidence-critic/definition.toml:31:21
  help: add "http" to [binary].interfaces or choose one of: cli, mcp
```

A clean definition prints no diagnostics and exits `0`. The code list is
in [DSL §20](../design/agent-definition-dsl.md#20-diagnostics).

## 4. Build it

```sh
harw agent build evidence-critic --interface cli,mcp -o ./ec
```

- `<name|path>` is a definition name from your layers or a path to a
  definition folder.
- `--interface` sets the built-in interfaces for this build and overrides
  `[binary].interfaces`. Choices: `cli` (one-shot), `repl`, `mcp`, `http`,
  `tui`.
- `-o` is the output file.
- `--target` picks a target triple other than the host (default backend:
  a runner for that target must be installed).

The default backend copies the prebuilt `harw-agent-runner` (installed
next to `harw` by the release archive or `make install`, or found in
`~/.harw/runners/<target>/<version>`) and appends the artifact. It needs
no Rust toolchain. If the installed runner lacks
an interface you asked for, the build fails and suggests `--native` or
installing the full runner.

Inside the TUI, `/agent build` runs the same build as a job with progress
in the panel.

Building the same definition twice with the same harw version gives the
same artifact hash.

## 5. Run it

```sh
export ANTHROPIC_API_KEY=…

./ec "Review the sources in report.md"        # default interface (cli)
./ec --json "Review the sources in report.md" # machine-readable output
./ec --interface mcp                           # MCP server over stdio
```

Useful flags of every built binary:

| Flag | Effect |
|---|---|
| `--manifest` | print the rights manifest, the artifact hash and the built-in interfaces, then exit |
| `--verify` | check the embedded artifact's hashes and exit `0` if intact |
| `--version` | print the agent name and version, the harw version and the artifact hash |
| `--interface <name>` | choose one of the built-in interfaces |
| `--full-access` | skip approval prompts, but only for rights inside the manifest |

The manifest is also printed at startup, so you can always see what the
agent is allowed to do.

### Interfaces

- **`cli`** — one task from the command line, answer on stdout, approvals
  in the terminal.
- **`repl`** — interactive session in the terminal.
- **`mcp`** — the agent as an MCP server with a `run` tool (plus `status`
  and `cancel`), over stdio or Streamable HTTP. Register it with any MCP
  client like other stdio servers.
- **`http`** — JSON API: `POST /run`, `GET /runs/{id}`, and server-sent
  events in the SDK event schema. Requires a bearer token from an
  environment variable.
- **`tui`** — the harw terminal UI fixed to this agent; agent and model
  switching is hidden or limited to the manifest.

Asking for an interface that was not built in is an error; rebuild with
`--interface`.

### Inspect a binary

```sh
harw agent inspect ./ec
```

shows the manifest, artifact hash, snapshot hash, interfaces and embedded
skills of a binary or artifact file, after verifying it. `harw agent run
<artifact>` runs an artifact file directly without building a binary.

## 6. Native builds

```sh
harw agent build evidence-critic --interface cli --native -o ./ec
```

`--native` generates a small Rust crate that enables only the interfaces
and tool providers the agent actually reaches, embeds the same artifact,
and compiles it with `cargo build --release`. The result is smaller and
links less code. Requirements:

- a Rust toolchain (`cargo`, the target installed with `rustup` for
  `--target`);
- the harw sources, because the generated crate uses path dependencies on
  them;
- time: a native build compiles harw's crates and can take several
  minutes the first time.

If either the toolchain or the sources are missing, the build stops with
a clear error. The artifact, and so the artifact hash, is identical to the
default backend's.

## 7. Secrets

A built binary never contains secrets. The definition names the variables
it needs in `[models].required_env`; the compiler copies them into the
manifest, and the binary reads them from the environment at runtime
(`env:NAME` references). A missing variable is reported by name before the
agent starts. The runner does not use the OS keyring.

Pass secrets the usual way for your deployment: an environment file for a
systemd unit, a secret mount in a container, or `export` in a shell. Do
not bake them into wrapper scripts that you distribute with the binary.

## 8. Limits and security notes

- **Rights are the author's ceiling.** Effective rights are
  min(manifest, flags, local deny rules). `--full-access` only removes
  prompts within the manifest. Network access is limited to the hosts and
  modes the manifest names.
- **Tamper detection, not authorship.** Changing any byte of the embedded
  artifact makes the binary refuse to start; `--verify` reports it. A hash
  does not prove who built the binary. Only run binaries from sources you
  trust; signatures are future work.
- **Do not post-process the binary.** `strip`, packers, and anything that
  rewrites the end of the file destroy the embedded artifact. On macOS,
  re-sign after building.
- **Sandbox.** Shell tools run under `bwrap` from the system, as in harw.
  Without `bwrap`, shell tools refuse to run.
- **Size limits.** An artifact is limited to 64 MiB, with 16 MiB per
  embedded file (see [`agent-artifact-v1.md`](../design/agent-artifact-v1.md) §8).
  Large knowledge bases belong in a retrieval tool, not in the binary.
- **Frozen.** A binary does not read `~/.harw` or your definition files.
  Editing the definition changes nothing until you build again.
- **State.** Sessions and logs go to an optional state directory; without
  one, nothing is written except what the agent's tools write.
