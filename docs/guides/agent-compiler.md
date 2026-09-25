# Compiling agents into standalone binaries

> Status: implemented (#22, wave 4: `docs/plans/r10-agent-compiler.md`).
> `harw agent check/build/inspect/graph/explain/new/fmt/diff/test/versions/
> use/clean/doctor` and the `~/.harw/bin` version store described below are
> in `harw-agent-compiler` and wired into the CLI today. **Only the
> runner's own interfaces** (an actual `./ec` binary serving `cli`, `repl`,
> `mcp`, `http` or `tui` at runtime, wave 3 of the plan) are still in
> progress — until that lands, a built binary's `--manifest` and `--verify`
> work, but running a task through it does not yet. `--native` builds are
> also wave-3-dependent (they embed the same runner). See
> [ADR 0001](../adr/0001-agent-compiler.md) for the full design.

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
- Without `-o` the binary is installed as `~/.harw/bin/<name>` (the name
  from `[binary].name`, else the specialization). Every build is kept as a
  version under `~/.harw/bin/.versions/<name>/<version>-<digest>/` with a
  `build.json`; `~/.harw/bin/<name>` is a symlink to the current one.
  `harw agent versions <name>` lists them, `harw agent use <name>
  <version|digest>` switches back.
- `-o` additionally copies the result to a file or directory.
- `--artifact-only` writes only the `.harwa` artifact (no runner needed).
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

> The interfaces below (`cli`, `repl`, `mcp`, `http`, `tui`) are the wave-3
> runner contract; `--manifest` and `--verify` work against any built
> binary today, but a built binary does not yet run a task end to end.

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

## 9. Command reference

Every command accepts `--json`; in the TUI the same commands run as
`/agent <command> …` (a build then runs as a job, see `/jobs`).

| Command | Does |
|---|---|
| `harw agent check [name\|path]…` | parse, resolve, lower and run the compiler passes; diagnostics with code, `file:line:column`, a source excerpt with a caret and a help line. Exit code 1 on errors. Without arguments: every definition of your layers. |
| `harw agent build <name\|path> [--interface …] [--native] [--artifact-only] [--runner P] [-o OUT] [--target T] [--harw-src DIR]` | compile and install (section 4). |
| `harw agent inspect <binary\|artifact\|name>` | manifest, artifact hash, snapshot, interfaces, skills, every agent of the bundle with its references, pool dedup savings, models, `required_env`. |
| `harw agent graph [name\|--all] [--format text\|dot\|mermaid\|json] [--kind delegation\|resolution\|rights\|all]` | who can start whom (depth, read-only or writing), the resolution chain (`extends` → mixins → patches per layer), the rights flow (manifest ≤ base role ≤ author ceiling). |
| `harw agent explain <name> [field]` | where a value comes from (layer, `file:line`, patch operator); `tool:<name>` explains why a tool is admitted, forbidden or pruned. `harw agent explain HARW-PATCH-003` explains a diagnostic code with an example and the fix. |
| `harw agent new <name> [--role worker\|child-orchestrator] [--extends ID] [--dir DIR]` | a commented `definition.toml` plus `system.md` (default `~/.harw/agents/<name>`). Nothing is built. |
| `harw agent fmt [paths] [--check]` | canonical formatting (below). |
| `harw agent diff <a> <b>` | IR difference of two definitions, installed versions (`name@version`) or artifacts; widening rights are marked `!!`. |
| `harw agent test [name]` | validate, build in memory, run the cases in `tests/*.toml` next to the definition. |
| `harw agent run <artifact\|name> [prompt]` | runs an artifact directly; needs the runner (wave 3) and exits 69 until then. |
| `harw agent versions <name>` / `harw agent use <name> <version\|digest>` | installed versions, switch the current one. |
| `harw agent clean [--all] [--older-than DAYS] [--keep N] [--dry-run]` | trims the native build cache and old versions (keeps the current version plus N, default 3). |
| `harw agent doctor` | runner and its capabilities, native prerequisites, install record, cache size, `~/.harw/bin` on `PATH` (with the line for your shell rc), last automatic UIA build. |
| `harw agent list` | the roster: `eingebaut`, `eigene Definition`, and the compiled copies in `~/.harw/bin`, with snapshot and build state (current or stale). |

### Formatting (`fmt`)

`fmt` works on the `toml_edit` document model and keeps every comment. It
only changes layout: the header keys first in DSL order (`schema`, `id`,
`version`, `extends`, `mixins`, `role`, `specialization`, `name`,
`description`, `reasoning_effort`, `instructions_file`, `skills`), one space
around `=`, no trailing whitespace, single blank lines, one final newline.
A comment above a key moves with the key. Arrays, inline tables and strings
stay as written. The result must parse to the same values; `fmt` is
idempotent.

### Test cases

```toml
# tests/reviews-a-claim.toml
name = "reviews a sourced claim"
prompt = "Prüfe die Quelle in report.md"

[expect]
tools = ["fs.read"]            # must be in the manifest (checked now)
not_tools = ["shell.exec"]     # must not be (checked now)
contains = ["Behalten"]        # answer checks: pending until the runner exists
```

## 10. Where things are found

- **Runner** (default backend), in this order: `--runner`, next to `harw`,
  the `bindir` of `~/.harw/install.toml`,
  `~/.harw/bin/.runners/<target>/<version>/`, `~/.harw/runners/<target>/<version>/`.
  A missing runner names every path and the fix (`make install`,
  `--artifact-only`, `--native`).
- **Runner capabilities**: the build calls `harw-agent-runner --capabilities`,
  which prints one JSON object (`schema`
  `harwness.agent-runner.capabilities/v1`, `runner_version`, `target`,
  `artifact_formats`, `ir_schema`, `interfaces`, `features`,
  `child_protocol`). A runner that lacks an interface or a tool-provider
  feature the agent needs is rejected with a hint to `--native`.
- **harw sources** (`--native`): `--harw-src`, `HARW_SRC`, else the
  `source_dir` of `~/.harw/install.toml`, which `make install` writes
  (`harw agent install-record --source-dir … --bindir …`).
- **cargo**: every `PATH` entry, then `~/.cargo/bin`. Without it: install
  with `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
  (harw never installs it for you).
- **Native build cache**: `~/.harw/cache/agent-builds/` — one shared cargo
  target dir, one generated crate per build, a blob store. It is capped at
  5 GiB (`[agent_compiler] cache_max_bytes`) and collected after every
  native build; `[agent_compiler] keep_versions` also trims old versions
  then. Uninstalling removes it.

## 11. User-interface agents

- The active UIA (`active_uia_definition`) is the one agent harw builds on
  its own: on a TUI start and after `harw agent uia-new`, in the background,
  with the artifact backend only, and only when its definition, its bundle
  files, harw or the runner changed. The result is
  `~/.harw/bin/<name>` (`[binary].name`, else `harw-uia-<specialization>`)
  with the interfaces `tui`, `repl`, `cli` unless `[binary]` says
  otherwise. `[agent_compiler] auto_build_uia = false` turns it off.
- `harw agent build <uia> --native` builds a **complete, personalized harw**
  (all of harw with the UIA as its fixed root) named `harw-<name>`. It uses
  its own home `~/.<name>` and project directory `.<name>` (no inheritance
  from `~/.harw`); on its first start it offers to import provider settings
  and credentials from `~/.harw`, and in a project with a `.harw` it offers
  once to copy the project configuration (never state).
- Built-in roles stay embedded in harw. Building one (`harw agent build
  explorer`) only makes a copy in `~/.harw/bin`.
