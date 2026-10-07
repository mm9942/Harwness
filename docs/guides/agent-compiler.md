# Compiling agents into standalone binaries

> Status: implemented (#22; decisions in `docs/adr/0001-agent-compiler.md`).
> `harw agent check/build/inspect/graph/explain/new/fmt/diff/test/versions/
> use/clean/doctor` and the `~/.harw/bin` version store described below are
> in `harw-agent-compiler` and wired into the CLI. The runner's own
> interfaces — `cli`, `repl`, `mcp` (stdio and Streamable HTTP via
> `--listen`), `http` and the mini `tui` — run an actual task end to end
> today; §5 documents each one and the flags they share. A compiled root
> agent delegates to its bundled children as separate runner processes
> automatically when `[binary].child_execution = "job"` (the default),
> and the mini TUI's fixed-agent restrictions (hidden UIA/agent-switch
> commands, disabled `/model switch`) are enforced by `harw-tui`'s
> command registry. See [ADR 0001](../adr/0001-agent-compiler.md) for the
> full design.

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

An agent can build agents itself through the `agents.build` tool
(arguments `name_or_path`, `interfaces`, `native`). It is registered only
for an explicitly chosen agent whose definition names `agents.build`
literally in `[tools].admitted` (and not in `forbidden`); built-in roles
and a UIA without an explicit definition never get it. It also needs
`ExecuteProcess` in the agent's sandbox and a session with a job system:
the build always runs as a background job, the call returns the job id,
and progress and result come from `job.status`/`job.logs`. It is not on
the list of automatically approved tools, so each call asks for approval
like `shell.exec`. If a condition other than the definition is missing,
the tool is left out and the reason is logged
(`runtime.agents_build.withheld`). Child agents do not get `agents.build`
yet, even when their definition admits it.

Building the same definition twice with the same harw version gives the
same artifact hash.

## 5. Run it

```sh
export ANTHROPIC_API_KEY=…

./ec "Review the sources in report.md"        # default interface (cli)
./ec --json "Review the sources in report.md" # machine-readable output
./ec --interface mcp                           # MCP server over stdio
```

The interface actually used is chosen in this order: `--interface`, else
the manifest's `[binary] default_interface`, else the first entry of
`[binary] interfaces`. Asking for an interface the manifest does not list
is an error; asking for one the manifest lists but this runner build was
not compiled with (`--interface`'s cargo feature) is also an error, naming
what this build does have.

### 5.1 Flags every built binary understands

| Flag | Effect |
|---|---|
| `--manifest` | print the rights manifest (`AgentIr::permissions`) as text, or as JSON with `--json`, then exit. Never starts a session. |
| `--verify` | recompute and check the embedded artifact's hashes; prints the digest and exits `0` if intact, non-zero (with the failing check) otherwise. Never starts a session. |
| `--version` | print the runner's own version (`CARGO_PKG_VERSION`) and exit. |
| `--capabilities` | print the `harwness.agent-runner.capabilities/v1` JSON `harw-agent-compiler` uses to pick a runner (schema, target, artifact formats, IR schema, compiled interfaces and provider features, child protocol). Never starts a session. |
| `--interface <name>` | choose one of `cli`, `repl`, `mcp`, `http`, `tui` for this run, overriding the manifest's default. |
| `--json` | machine-readable output: one JSON object per SDK event on stdout for `cli`, JSON for `--manifest`. |
| `--full-access` | approve every tool call automatically, but only for rights already inside the (possibly narrowed) manifest — it never adds a tool, host or path. |
| `--deny-tool <name>` | remove one tool from the effective rights; repeatable. |
| `--no-network` | clear network access entirely for this run, regardless of the manifest. |
| `--read-only` | drop write and shell rights for this run. |
| `--max-tokens <n>` | cap the token budget at `n`, tightening (never loosening) the manifest's own budget. |
| `--listen <addr>` | bind address for `http` (default `127.0.0.1:8787`) and for `mcp`'s Streamable HTTP transport (§5.3, §5.4). |
| `--child <id>` / `--child-protocol <label>` | run as a delegated child instead of a top-level interface (§5.7); both flags are required together. |
| `--offline-echo` | answer every model call locally (reply text from `HARW_OFFLINE_ECHO`, if set) and say so in one stderr line; not a rights flag. Release builds ignore `HARW_OFFLINE_ECHO` without this flag. |

Every flag above except `--manifest`/`--verify`/`--version`/`--capabilities`/`--offline-echo`
narrows the manifest before the chosen interface starts
(`harw_runtime::embedded::EffectiveRights::from_manifest(&permissions)
.narrowed_by(&flags)`): the effective right for tools, network, write,
shell, host and budget is always `min(manifest, flags)`, one right at a
time, never the other way around. The manifest is also printed as a
one-line banner at startup (suppressed by `--json`, MCP over stdio and `--child`), so you can always see
what the agent is allowed to do before it does anything.

### 5.2 `cli` — one-shot

Runs exactly one turn. The prompt is the positional command-line words, or
all of stdin if none are given. The answer streams to stdout as it
arrives; tool calls and their results are dim lines on stderr, so stdout
stays exactly the agent's answer for a shell pipeline. `--json` replaces
both with one JSON object per SDK event on stdout instead. Approval asks
interactively at the terminal (`[j]a / [n]ein / [i]mmer für diese
Sitzung`) unless `--full-access` is given or no terminal is attached, in
which case every call is denied.

Exit codes: `0` completed, `1` the turn failed/refused/was truncated (or no
prompt was given), `2` cancelled, `3` at least one tool call was denied
approval (even if the turn otherwise finished).

### 5.3 `repl` — interactive session

One session for the whole process: every line is a new turn on the same
conversation, so history carries across turns (unlike `cli`, which exits
after one). Commands, read one per line:

- `/exit` — ends the REPL (also end-of-input: piped stdin, Ctrl+D).
- `/manifest` — prints the embedded agent's permissions as JSON; not a
  turn.
- `/cancel` — cancels the turn currently running. Typed while nothing is
  running, it has nothing to cancel and says so.
- **Ctrl+C** (`SIGINT`) cancels a turn that is actually in flight, the same
  way `/cancel` does, without needing a second line of input.

Approval is the same terminal handler `cli` uses, and remembers an
"immer"/always answer for the rest of the session, not just the current
turn. The REPL itself exits `0` on `/exit` or end-of-input regardless of
how the last turn ended — a failed or cancelled turn is not a failed
session.

### 5.4 `mcp` — Model Context Protocol (stdio or Streamable HTTP)

Implements `initialize`, `tools/list`, `tools/call` (`run`, `status`,
`cancel`), `notifications/cancelled` and `ping` over newline-delimited
JSON-RPC 2.0 on stdin/stdout, protocol version `2025-06-18`. `run` takes
`{"prompt": …, "context": …, "background": …}`; a foreground call streams
`notifications/progress` and returns the final text plus usage, a
`background: true` call returns a `run_id` immediately for `status`/
`cancel` to poll. There is no interactive approval channel here: without
`--full-access`, a call that needed approval is denied and the response
says so; `--full-access` approves everything already inside the manifest.

**Two transports are implemented.** Without `--listen`, the stdio
JSON-RPC loop runs to completion (client closed stdin, or a fatal I/O
error). With `--listen <addr>`, the interface runs its own small
loopback-oriented HTTP transport (`POST /mcp` plus `GET /healthz`),
carrying the same JSON-RPC messages: a single active session (this is a
single-agent, effectively single-client server), the same approval
story, and the same auth rule as `http` — a bearer token from
`HARW_AGENT_HTTP_TOKEN` is required on any non-loopback address and
still checked (constant-time) on loopback if set. It deliberately does
not reuse `harw-mcp-server`'s Streamable HTTP transport, which is built
around a durable job supervisor and tenant/workspace principals a
compiled, embedded single-agent runner has none of — see
`harw-agent-runner/src/iface/mcp.rs`'s module docs for that reasoning.

### 5.5 `http` — JSON API

- `POST /run` — `{"prompt": …, "background": …}`; synchronous by default,
  or `background: true` for an immediate `202` with a `run_id`.
- `GET /runs/{id}` — status (`pending`/`running`/`completed`/`failed`/
  `cancelled`), text and usage of a run.
- `GET /runs/{id}/events` — a server-sent-events stream of the run's SDK
  events, one JSON object per `data:` line, `event:` set to the event's
  kind, ending in a `run.finished` marker.
- `POST /runs/{id}/cancel` — cancel a running (or not yet finished) run.
- `GET /manifest` — the root IR's permissions, declared interfaces and
  artifact digest.
- `GET /healthz` — liveness; never behind auth.

Runs live in an in-memory map capped at 64; once over the cap, the oldest
*finished* run is evicted — a run still in flight is never evicted.

**Auth and the loopback rule.** A bearer token from `HARW_AGENT_HTTP_TOKEN`
is required to bind any **non-loopback** address (anything but
`127.0.0.1`/`::1`); starting without one there is a startup error, not a
silent open listener. On loopback, the token is optional but still checked
(constant-time comparison) if the variable happens to be set. `/healthz`
is exempt either way, so liveness probes never need the token. Same
approval story as `mcp`: no interactive channel, `--full-access` is the
only way to let a manifest-permitted tool call through unattended.

### 5.6 `tui` — mini terminal UI

The normal `harw` terminal UI, restricted to this one embedded agent: same
renderer and command loop, with the title bar set from the manifest's
`name` (falling back to `specialization`) and the manifest's model plus
its fallbacks as the only allowed models. The restrictions are enforced:
`run_fixed_agent` feeds `harw_tui::fixed_agent::hidden_command_names`
into `harw-tui`'s command registry (`ChatApp::with_hidden_commands`), so
the UIA-switch and agent-selection commands a fixed session should hide
are removed from the popup, tab-completion and dispatch, and a
`/model switch` attempt is refused by the local-command gate
(`harw-tui/src/local_commands.rs`) before it can run. Today the compiled
runner passes `allow_model_switch: false`, so `/model switch` stays
disabled outright rather than restricted-but-open — the allowlist
(`allowed_models`) is already carried and wired, and flipping the flag
at the single call site (`harw-agent-runner/src/iface/tui.rs`) is the
only change a restricted-but-open switch needs. See
`harw-tui/src/fixed_agent.rs`'s module docs for the exact spots.

### 5.7 Delegated children (`--child`)

A compiled binary's bundle can hold more than the root agent (the
delegation closure). `[binary] child_execution` decides how the root runs
those children: `"job"` (the default) means each child runs as its own OS
process; `"in-process"` keeps the legacy in-process spawner, the same way
harw itself always runs its children. A child process is started as
`harw-agent-runner --child <agent-id> --child-protocol stdio` and speaks
the JSON-lines protocol `harwness.agent-child/v1` on its own stdio
(stderr stays plain logs, never a protocol frame). `stdio` is only the
transport label; the protocol version is checked in the child's first
frame (`Hello`). See
[`agent-child-protocol-v1.md`](../design/agent-child-protocol-v1.md) for
every frame.

The parent sends `Rights`, then `Budget` (if the run has one), then
`Mode`, then `Task`. `Rights` is mandatory: a child that receives `Task`
first answers with an `Error` frame and exits. A child's effective rights
are `its own ceiling ∩ the parent's Rights frame`, where the ceiling is
the child's manifest narrowed by the child's own runner flags. The parent
sends the child's real current rights, exactly what the child would have
in-process (tools, sandbox rights, allowed hosts); if it cannot determine
them it sends empty rights. Network targets given as public DNS or CIDR
ranges have no form on the wire and are dropped for a job child. Every
boolean right is AND'd. `full_access` is the one exception on the child's
side: the child's command line never carries it, so it comes only from
the parent, which grants it when the root runs in full-access approval
mode; the child's session then approves tool calls itself instead of
relaying them. The result is applied to the child's session
(`EmbeddedAgent::with_rights`), so a parent can only narrow a child below
its own manifest, never grant it more. A `Budget` frame likewise only
tightens the child's budget (wall time travels in milliseconds and
becomes whole seconds in the child). `live` mode runs the child's
session in work mode. A child that runs out of budget reports
`budget_exhausted` with a `continuation` token (its session id); a later
`Task` with that token in `continue_from` resumes the stored session from
the same `HARW_HOME`, and a token the child cannot resume ends with an
`Error` frame. Each protocol line is bounded: at most 64 levels of JSON
nesting and at most `MAX_FRAME_BYTES` (1 MiB); an oversized or non-UTF-8
line ends the run on either side. A parent started with `--offline-echo`
passes the flag on to its children.

`harw-agent-runner` ships the process-driving side of this
(`JobChildBackend`, tested standalone against real spawned processes:
hello/result, two crash shapes, the job-managed path and cancel) and the
child-process side (`crate::child::run_child`). The wiring is automatic:
when `RunnerContext::builder` assembles the embedded runtime and the
current agent's `[binary].child_execution` is `"job"` (the default), it
sets the job-backed child backend, so a root agent's own delegation
starts each child as a separate runner process — no explicit `--child`
invocation needed. A `--child` process runs its agent as the embedded
root, so a child that delegates further starts its own children the same
way. The seam is `RuntimeSpec::child_backend`
(`harw-runtime/src/assembly.rs` passes it through to the
`ManagedAgentSpawner`); `[binary] child_execution = "in-process"` keeps
the legacy in-process spawner.

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

### Verifying the native backend

The unit tests only compare the generated `Cargo.toml` and `src/main.rs`
with golden files (`harw-agent-compiler/tests/golden/native/`). A real
native build runs in two places, both against
[`examples/agents/hello-analyst`](../../examples/agents/hello-analyst):

```sh
sh scripts/native-e2e.sh
cargo test -p harw-agent-compiler --test native_e2e -- --ignored native
```

`scripts/native-e2e.sh` builds `harw` (`cargo build -p harw-cli --bin
harw`), runs `harw agent build <example> --native --harw-src <checkout> -o
<tmp>/agent` and then checks the binary: `--verify` (and `--verify --json`)
report the digest the build printed, `--manifest` names the agent and
`fs.read`, `--requirements --json` reports `"admitted": true`, and a
one-shot with `--offline-echo` exits `0` with the `HARW_OFFLINE_ECHO` reply
on stdout. The ignored test `native_e2e` does the same through the
compiler API (`Compiler` plus `harw_agent_compiler::build` with `native:
true`) and `std::process`. Both use a fresh temporary harw home unless
`NATIVE_E2E_HOME` names one; a persistent home keeps the native build cache
(`<home>/cache/agent-builds/target`), so later runs only rebuild what
changed. The script also takes `HARW_BIN` to skip building `harw`.

The one-shot needs `--offline-echo` because the native binary is a release
build, which ignores `HARW_OFFLINE_ECHO` on its own; it sets a dummy
`ANTHROPIC_API_KEY` because the example's `[models].required_env` names it
and the runtime checks that it is set, although the echo never calls a
provider.

CI runs the script in the `native-agent` job on pushes to `main` and on
manual runs (`workflow_dispatch`), not on pull requests: the first build
compiles harw's runner crates in release mode. While the push trigger is
commented out in `ci.yml` (see `CONTRIBUTING.md`), only manual runs execute
it. The job caches the native
build's target directory next to the usual cargo cache.

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
| `harw agent test [name]` | validate, build in memory, run the cases in `tests/*.toml` next to the definition; manifest checks run immediately, answer checks (`contains`/`not_contains`) run the located runner as a throwaway one-shot subprocess. |
| `harw agent run <artifact\|name> [prompt]` | runs an artifact or an installed agent directly, out of process (§10); exits 69 (`EXIT_RUNNER_UNAVAILABLE`) if no matching runner can be located. |
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
prompt = "Check the source in report.md"

[expect]
tools = ["fs.read"]            # must be in the manifest (checked now)
not_tools = ["shell.exec"]     # must not be (checked now)
contains = ["Keep"]            # answer check: runs the located runner as a subprocess
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
