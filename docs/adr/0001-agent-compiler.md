# ADR 0001 — Agent compiler: IR v2, artifacts and standalone binaries

> Status: accepted · partially implemented · Date: 2026-09-25 · Tracking: #22 (plan R10)

**Amends:** [`docs/design/agent-ir-v1.md`](../design/agent-ir-v1.md) §2 and §5.
**Binds to:** [`agent-definition-dsl.md`](../design/agent-definition-dsl.md)
§7, §16–§18, §20; [`agent-artifact-v1.md`](../design/agent-artifact-v1.md);
[`agent-composition-contract.md`](../design/agent-composition-contract.md) §6.
**User guide:** [`guides/agent-compiler.md`](../guides/agent-compiler.md).

This is the first architecture decision record in `docs/adr/`. ADRs are
numbered in order (`NNNN-short-title.md`), never renumbered, and carry the
sections *Context*, *Decision*, *Consequences*, *Security* and *Alternatives
considered*. A later ADR that changes this decision supersedes it by number;
this file is then marked `superseded by ADR NNNN` and otherwise left as is.

Implementation status: IR v2, the diagnostics catalog and the merge fixes
land in wave 1 of #22. The artifact crate, the compiler and the
`harw agent check|build|inspect|run` commands land in wave 2. The runner's
own interfaces (`cli`, `repl`, `mcp` over stdio, `http`, the mini `tui`)
and the child protocol between a compiled binary and its delegated
children land in wave 3 — see "Wave 3 consequences" in §3 for what wave 3
changed against what this record originally decided. Wave 5 closed the
two pieces wave 3 left half-wired (MCP's Streamable HTTP transport and
the automatic job-backed child execution) and enforced the mini TUI's
fixed-agent restrictions; §3 lists the details.

---

## 1. Context

An agent definition (`definition.toml`) runs through
`parse → resolve → lower` in `harw-agent-dsl` and ends as an in-memory
`ExecutableAgentIr`. The documentation already calls this a compiler
([`agent-ir-v1.md`](../design/agent-ir-v1.md), DSL §17), but the pipeline
stops at a value that lives only inside one `harw` process. Four problems
follow from that:

1. **The IR is not serializable.** `ExecutableAgentIr` and its sub-structs
   have no serde. There is no file format for a compiled agent, and the
   snapshot hash covers the content fields but not the definition version,
   the instructions or the skills an agent was built with. A run snapshot
   can therefore not be reproduced or audited outside the process that
   made it.
2. **Semantics live in loose tables.** `[work]`, `[models]`, `[limits]`,
   `[delegation]`, `instructions_file`, and the detail and trust settings
   of context-program sections are either never lowered or are read out of
   band from the raw TOML. Anything that is read out of band escapes the
   invariants the IR is supposed to enforce in one place (DSL §22).
3. **The runtime derives rights from role names.** Registry profile,
   reducer, delegation targets and knowledge are looked up by role name,
   not read from the IR. Several IR fields (`authority`, `validators`,
   `allow_rerun`, `max_attempts`, `goal_kind`, `workspace_hint`) are never
   read at all, so a definition can declare them without effect.
4. **Failures are silent.** Nested patch ops such as
   `[patch.context.must_include]` are ignored, an unknown return contract
   falls back to `Text`, the `schema` field is not checked, and user
   definitions never get a context program bound.

Separately, users want to **ship an agent**: hand a colleague, a CI job or
another tool a single file that runs one fixed agent with one fixed set of
rights, without installing and configuring a full `harw` profile. Today the
only way to run a definition is inside `harw`, with whatever rights the
local profile grants. That makes the definition's own rights declaration
advisory rather than binding, and it makes the agent impossible to embed as
an MCP server or HTTP service without the whole harness.

A serializable, fully typed IR solves the first four problems on its own.
Once the IR is serializable, freezing it into an artifact and attaching it
to a runner binary is a small step, and it is the step that turns the
declared rights into the rights the process actually has.

## 2. Decision

### 2.1 IR v2 in `harw-agent-dsl`

- A new module in `harw-agent-dsl` defines `AgentIr` with schema
  `harwness.agent-ir/v2`: full serde, `#[serde(deny_unknown_fields)]`,
  and typed sections for identity, instructions, tool surface, spawn
  contract (including delegation targets), budget, lifecycle, context
  program, return pipeline, models, limits, work, verification, skills,
  binary settings and the permissions manifest.
- `lower_v2` lowers every table or rejects it with a diagnostic. Nothing is
  dropped silently.
- Diagnostics carry stable codes `HARW-<AREA>-NNN` with file, line and
  column, a severity and a help text.
- The snapshot hash moves to **v7**: BLAKE3 over the canonical
  serialization of `AgentIr`, including version, instructions, skill
  content hashes and the manifest. The resolution trace stays excluded.
- `ExecutableAgentIr` remains as a view built from `AgentIr`, so existing
  consumers keep working while they migrate.
- The runtime reads profile, reducer, delegation targets, instructions and
  knowledge from `AgentIr`; role names are only a fallback for old paths.

Details: [`agent-ir-v1.md`](../design/agent-ir-v1.md) §6.

### 2.2 Three new crates

| Crate | Responsibility |
|---|---|
| `harw-agent-artifact` | The artifact format ([`agent-artifact-v1.md`](../design/agent-artifact-v1.md)): write, read, verify, and locate an artifact at the tail of the running executable. No runtime dependencies. |
| `harw-agent-compiler` | The driver: discovery, parse/resolve/`lower_v2`, the compiler passes, and both backends. |
| `harw-agent-runner` | The runner binary. Interfaces are cargo features; the release runner enables all of them. |

`harw-cli` exposes the compiler as `harw agent check|build|inspect|run`.
Builds run as a job (`harw-tool-job`) with progress in the panel.

### 2.3 Compiler passes

After lowering, the compiler runs, in order: `ValidateRoles`, `RightsCheck`
(manifest ≤ base role ≤ author ceiling), `ResolveSkills` (embed skill
content from the `SkillIndex`), `ReachableTools` (which tool providers are
needed), `PruneUnusedTools`, and `ResolveModels` (required environment
variables go into the manifest). `ReachableTools` and `PruneUnusedTools`
use a capability catalog (tool name → provider) in
`harw-registry-defaults`, as foreseen in the composition contract §6.

### 2.4 Two backends

- **Backend A — artifact plus prebuilt runner (default).** The compiler
  serializes the artifact and appends it, with a fixed-size footer, to a
  copy of the prebuilt `harw-agent-runner` for the target. No Rust
  toolchain is needed. The runner is taken from next to the `harw` binary
  (release archive) or from `~/.harw/runners/<target>/<version>`. If the
  available runner lacks a requested interface feature, the build fails
  with a hint to use `--native` or install the full runner.
- **Backend B — `--native`.** The compiler generates a small Rust crate
  whose `Cargo.toml` enables only the interface and tool features the agent
  needs, embeds the same artifact with `include_bytes!`, and runs
  `cargo build --release` as a job. It needs a Rust toolchain and path
  dependencies on the harw sources and fails with a clear error without
  them. The result is smaller and links only reachable code.

Both backends embed the **same** artifact. The artifact hash of a build is
independent of the backend.

### 2.5 Interfaces per build

A compiled agent can expose `cli` (one-shot), `repl`, `mcp` (stdio and
Streamable HTTP), `http` (JSON API with SSE events) and `tui`. Selection is
layered:

1. `[binary].interfaces` in the definition sets the default set, and
   `[binary].default_interface` the one used when none is chosen.
2. `harw agent build --interface …` overrides the set for one build.
3. At runtime, `./agent --interface mcp` picks one of the built-in
   interfaces. Asking for an interface that was not built in is an error.

### 2.6 Rights manifest, min(manifest, flags)

The permissions section of `AgentIr` is the **rights manifest**: admitted
tools, network hosts and modes, write paths, shell, host access, budgets
and required environment variables. It is baked into the artifact. At
runtime the effective rights are

```text
effective = min(manifest, command-line flags, local deny rules)
```

A flag can only narrow. `--full-access` removes approval prompts only
*within* the manifest; it never adds a tool, host or path. The runner
prints the manifest at startup and on `--manifest`. A compiled agent runs
under a new `EntryKind::CompiledAgent` whose profile is derived from the
manifest, not from a table row, and loads config, agent, skills and
knowledge from memory instead of `~/.harw`.

### 2.7 Amendment of `agent-ir-v1.md` §2 and §5

`agent-ir-v1.md` §2 argues that there are no backend crates because the
runtime crates *are* the backend, and §5 rules out a codegen crate and a
multi-backend executor. This ADR **amends both sections**:

- The runtime crates remain the backend for *executing* an agent. That
  part of §2 still holds, and `harw-agent-runner` reuses them rather than
  replacing them.
- What is new is a second kind of output: a *distributable* agent. For
  that output there is now one compiler crate with two backends (artifact
  plus runner, and native Rust codegen). The argument in §5 was that no
  consumer needed a second backend; shipping agents as binaries is that
  consumer.
- The rest of §5 stands: no separate `harw-ir` crate (IR v2 stays in
  `harw-agent-dsl`), no LLVM or MLIR analogy, no SSA, no optimizer
  beyond the pruning passes listed above.

## 3. Consequences

- **One source of truth for rights.** The runtime, the compiler and the
  runner all read rights from `AgentIr`. Fields that were declared but
  ignored now take effect, which can change behavior for definitions that
  set them. Every built-in role must keep an identical tool surface; this
  is checked against a snapshot taken before the change.
- **All snapshot hashes change.** Moving to v7 invalidates every digest and
  golden snapshot computed under v6. Frozen run snapshots from before the
  change are recognisably from an older domain and are not silently
  compared with new ones.
- **Stricter definitions.** Definitions that relied on silent fallbacks
  (unknown return contract, wrongly typed values, unknown tables) now fail
  with a diagnostic. Bundled definitions are fixed in the same change.
- **More crates and a second release binary.** The release archive ships
  `harw-agent-runner` for x86_64 and aarch64 next to `harw`. It gets its
  own `[profile.release-runner]` (fat LTO, one codegen unit, stripped,
  `panic = abort`) and a row in the privileges gate as an unprivileged
  binary.
- **Two backends to maintain.** The native backend's generated crate is
  covered by golden tests; tests never run cargo on it.
- **Reproducible builds.** The same definition, sources and compiler
  version give the same artifact bytes and hash.

### 3.1 Wave 3 consequences

Wave 3 built the runner this record described in §2.5–§2.6 and, along the
way, made two decisions this record did not anticipate:

- **`[binary] child_execution`.** §2.5 said nothing about how a compiled
  binary runs the children it can delegate to; wave 3 adds a `Binary`
  field (`"job"`, the default, or `"in-process"`) and a matching wire
  protocol, `harwness.agent-child/v1`
  ([`agent-child-protocol-v1.md`](../design/agent-child-protocol-v1.md)):
  a job-executed child is a separate `harw-agent-runner --child <id>
  --child-protocol stdio` process (`stdio` is the transport label; the
  protocol version is checked in the child's `Hello` frame), run as a job
  in its own process group, stopped as a group on cancel. The parent must
  send a `Rights` frame before the `Task`; without it the child sends
  `Error` and exits. The child's rights are `its own manifest ∩ the
  runner flags ∩ the parent's Rights frame`, where the parent sends the
  child's real current rights and `full_access` is AND'd at every step
  (#22 wave 6); the child applies the result to its own session
  (`EmbeddedAgent::with_rights`). This is additive to §2.6's
  `min(manifest, flags)` rule, not a change to it: a child's rights are
  still never wider than its own manifest, only possibly narrower still
  because of its parent. A `Budget` frame likewise only tightens.
- **The runner's own interfaces are one crate, `cli`/`repl`/`mcp`/`http`
  behind cargo features, `tui` reusing `harw-tui`'s existing renderer**
  (`harw_tui::fixed_agent`) rather than a new one — §2.5 left this
  unspecified beyond naming the five interface labels.

Two pieces §2.5–§2.6 implied were not fully wired by the end of wave 3;
wave 5 closed both, and they are recorded here rather than left to be
rediscovered from the code:

- **MCP Streamable HTTP is implemented.** Wave 5 added the `mcp`
  interface's own small loopback HTTP transport (`--listen <addr>`,
  `POST /mcp` plus `GET /healthz`, one active session, the same bearer
  token and loopback rule as the `http` interface) instead of reusing
  `harw-mcp-server`'s transport, which stays built around a durable job
  supervisor and tenant/workspace principals a compiled, embedded
  single-agent runner has none of (`harw-agent-runner/src/iface/mcp.rs`).
- **`child_execution = "job"` is automatic.** The process-driving side
  (`harw-agent-runner::job_child_backend::JobChildBackend`) and the
  child-process side (`harw-agent-runner::child::run_child`) both exist
  and are tested against real spawned processes, and
  `harw-agent-runner::context::RunnerContext::builder` now sets
  `RuntimeSpec::child_backend` for `EntryKind::CompiledAgent` whenever
  the current agent's `[binary].child_execution` is `"job"` (the
  default) — a root agent's own delegation reaches a separate child
  process without any explicit invocation, and a `--child` process whose
  agent delegates further does the same, so isolation holds at every
  depth. The mini TUI (`harw-agent-runner/src/iface/tui.rs`) builds
  its own `RuntimeSpec` and sets the same backend under the same
  condition. `harw-runtime/src/assembly.rs` passes the backend through
  to the `ManagedAgentSpawner`.
- **The mini TUI's restrictions are enforced.** `run_fixed_agent`
  (`harw-tui/src/fixed_agent.rs`) feeds
  `hidden_command_names` into the command registry via
  `ChatApp::with_hidden_commands` and gates `/model switch` through the
  local-command intercept (`harw-tui/src/local_commands.rs`); the
  compiled runner currently passes `allow_model_switch: false`, so a
  restricted-but-open switch remains a one-flag change at the call site,
  not a missing hook.

## 4. Security

- **Tamper detection.** The artifact ends in a BLAKE3 hash over all of its
  preceding bytes. The `HARWAEN2` executable footer additionally hashes the
  runner, artifact, offset and length; legacy `HARWAEND` binaries require a
  rebuild. The runner recomputes
  it before it parses the header and refuses to start on any mismatch
  (fail closed). `--verify` performs the same check and exits.
- **The manifest cannot widen rights.** `RightsCheck` rejects a manifest
  that exceeds the base role or the author's ceiling at build time. At
  runtime flags and local deny rules can only narrow it, and the
  `CompiledAgent` profile is built from the manifest alone.
- **Secrets only via `env:`.** An artifact never contains a secret. Model
  credentials and tokens (for example the HTTP bearer token) are named as
  required environment variables and read from `env:` references at
  runtime; a missing variable is a clear error. The runner is built without
  the OS keyring dependency.
- **Sandbox as in harw.** Shell tools use `bwrap` from the system. Without
  `bwrap`, shell tools fail closed with a message.
- **What the hash does not prove.** A hash detects accidental and naive
  modification. It does not prove *who* built the binary: an attacker who
  can rewrite the file can recompute the hash. Authorship needs a
  signature, which is future work (see
  [`agent-artifact-v1.md`](../design/agent-artifact-v1.md) §9) and will
  get its own ADR.

## 5. Alternatives considered

- **Keep the IR in memory and ship definitions as TOML.** Rejected: the
  receiver needs a full harw installation, rights depend on the receiver's
  profile, and nothing freezes the resolved state.
- **Native codegen only.** Rejected as the default: it requires a Rust
  toolchain and the harw sources on every build machine and takes minutes
  per build. It stays available as `--native`.
- **Artifact only, no native backend.** Rejected: the prebuilt runner links
  every interface and tool provider. Users who embed an agent in a small
  image or want the minimal attack surface need a build that links only
  what the agent reaches.
- **Artifact next to the runner as a separate file, or loaded by path.**
  Rejected: two files can drift apart, and a path argument lets the caller
  swap the agent (and its rights) under a trusted runner. Appending the
  artifact keeps agent and runner one unit.
- **A separate `harw-ir` crate for IR v2.** Rejected for the reasons in
  `agent-ir-v1.md` §5, which still apply: the IR belongs to the DSL crate.
- **A custom binary header format instead of canonical JSON.** Rejected:
  canonical JSON is inspectable with standard tools, is already what
  `serde` produces, and its size is irrelevant next to the runner.
- **Rights from command-line flags only, no baked manifest.** Rejected:
  the person running the binary could grant the agent more than its author
  intended. The author's manifest is the ceiling; flags only narrow it.
