<!-- Arbeitsplan für #22. Wird vor dem finalen Push wieder entfernt. -->

# Plan R10 (#22): Agent IR v2 and a compiler from the agent DSL to binaries

## Context

Today the agent DSL (`definition.toml`) runs `parse → resolve → lower` into `ExecutableAgentIr` (`harw-agent-dsl`). The docs already call this a compiler (`docs/design/agent-definition-dsl.md` §17, `agent-ir-v1.md`), but it stops at an **in-memory IR**, which has four problems:
- **Not serializable.** The IR and its sub-structs have no serde, and there is no artifact format and no hash covering the version or metadata.
- **Many semantics sit in loose tables.** `[work]`, `[models]`, `[limits]`, `[delegation]`, `instructions_file` and the `section_detail`/trust of context programs are never lowered or are read out of band.
- **The runtime reads rights from role names, not from the IR.** Profile, reducer, delegation targets and knowledge all come from role names. Some IR fields are never read at all: `authority`, `validators`, `allow_rerun`, `max_attempts`, `goal_kind`, `workspace_hint`.
- **Silent failures.** Nested `[patch.x.y]` ops are ignored. An unknown return contract falls back to `Text`. The `schema` field is not checked. User definitions never get a context program bound.

**Goal:** a fully typed, serializable, versioned Agent IR, and a compiler `harw agent build` that turns a DSL definition into a **standalone binary**:
- The rights manifest is baked in.
- The user chooses the interfaces: CLI/REPL, MCP server, HTTP/JSON API, mini-TUI.
- There are two backends:
  - **Artifact + runner** (default): no Rust toolchain needed.
  - **`--native`** codegen plus cargo: only the tools and interfaces actually used are linked.

**Mia's decisions:**
- Build both backends.
- Interfaces: all four, each selected by the user per build.
- IR fully typed.
- Rights manifest baked in; at runtime the rule is min(manifest, flags).

This contradicts `agent-ir-v1.md` §2/§5 ("no codegen crate") and needs an ADR that amends it.

## Build rule (word for word in every agent prompt)
Subagents never run `cargo`/`rustc` in any form, including `make` targets that call them. They only read and edit, and at the end they report tests and commands. There is one central build after every wave, with `CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0` in the container. Fixes go to PR #21 first, which must be green and merged before R10 starts from `main`. R10 then gets its own branch and a new draft PR.

## Architecture

```
definition.toml ─┐
skills, knowledge ├─ harw-agent-compiler ─ frontend: parse/resolve (harw-agent-dsl)
context programs ─┘                        ├─ typed lowering → AgentIr v2 (harw-agent-dsl)
                                            ├─ passes: validate, rights, reachability, prune
                                            ├─ backend A: AgentArtifact (harw-agent-artifact) + runner = 1 binary
                                            └─ backend B: Rust crate codegen + cargo (only linked parts)
AgentArtifact ─ harw-agent-runner (feature-gated interfaces) ─ harw-runtime (EntryKind::CompiledAgent)
```

New crates:
- `harw-agent-artifact`: format, hash, reading an artifact from the binary's own tail.
- `harw-agent-compiler`: driver, passes, backends.
- `harw-agent-runner`: binary with the interfaces as cargo features.

IR v2 stays in `harw-agent-dsl` as a new module.

## Wave 1: IR v2 and correctness (parallel, disjoint)

**1A – Typed IR v2** (`harw-agent-dsl`)
- New `ir_v2` module with `AgentIr`, schema `harwness.agent-ir/v2`, full serde, `#[serde(deny_unknown_fields)]`. Typed sections:
  - identity: id, version, name, description, role, specialization
  - `Instructions`: text plus source hash, from `instructions_file` or `system.md`, resolved in the DSL crate
  - `ToolSurface`, `SpawnContract` (including delegation targets), `Budget`, `Lifecycle` (pause, rerun, max_attempts)
  - `ContextProgram` with sections: detail, trust, strength
  - `ReturnPipeline` with validators and a strict contract
  - `Models`: preferred provider/model/effort, fallbacks, required env vars
  - `Limits`, `Work`, `Verification`
  - `Skills`: names plus content hash
  - `Binary`: `interfaces = ["cli","repl","mcp","http","tui"]`, name, default interface
  - `Permissions` manifest: tools, network hosts/modes, write paths, shell, host, budgets
- `lower_v2(resolved, sources) -> Result<AgentIr, Diagnostics>`. Every loose table is lowered or rejected with a diagnostic; nothing is dropped silently.
- **Diagnostics:** stable codes `HARW-<AREA>-NNN` with file:line/column from `toml::Spanned` / `toml::de` spans, severity and a help text. Area catalog in `diagnostics.rs`.
- **Snapshot hash v7:** BLAKE3 over the canonical serialization of the IR, including version, instructions, skill hashes and the manifest; the trace is excluded. `ExecutableAgentIr` becomes a view built from `AgentIr` (`From<&AgentIr>`), so existing consumers keep working.
- **Correctness fixes:**
  - The `schema` field is checked.
  - Nested patch ops (`[patch.context.must_include]`) work.
  - `min` and `max-within-parent` are implemented.
  - An unknown return contract is a hard error.
  - Wrongly typed values produce diagnostics instead of `None`.
- `bind_context_program` moves from `harw-registry-defaults` into the DSL crate (a library of built-in programs plus user programs from layers), so user definitions get their program too.
- Tests: golden IR JSON for every built-in and bundled definition; hash stability; one diagnostic test per code; patch-op tests.

**1B – Runtime reads the IR** (`harw-runtime`, `harw-registry-defaults`, `harw-core`, `harw-config`)
- Registry profile, reducer, delegation targets, instructions and knowledge are derived from `AgentIr`, with role names only as a fallback for old paths.
- Roster, children and assembly use `AgentIr`.
- The previously ignored fields take effect:
  - `authority` → intersection with the profile
  - `validators` → return validation
  - `allow_rerun` / `max_attempts` → child_controller
  - `workspace_hint` → workspace selection
- The root also gets its context program.
- `harw-config` discovery produces `AgentIr` via `lower_v2` and `AgentDefinitionMeta` merges into the IR.
- Tests: existing role behavior unchanged, every built-in role yields an identical tool surface (compared against the snapshot from before the change), and the new fields are enforced.

**1C – ADR and docs**
- `docs/adr/00xx-agent-compiler.md`: the artifact and native backends, and why `agent-ir-v1.md` §2/§5 is amended.
- Update `agent-ir-v1.md`, the DSL doc (§16 CLI, §17 pipeline, §18 frozen snapshots) and add a new `docs/design/agent-artifact-v1.md` (format).

## Wave 2: artifact and compiler

**2A – `harw-agent-artifact`**
- Format: magic `HARWAGNT`, format version, header (`AgentIr` as canonical JSON), payload table (skills, knowledge, instructions, context programs, templates) and a BLAKE3 over everything.
- Optional ed25519 signature, if already in the lockfile; otherwise hash only plus a later ADR.
- `write_artifact`, `read_artifact(bytes)`, `verify`.
- `EmbeddedArtifact::from_current_exe()` reads a footer (magic, offset, length, hash) at the end of the binary. Fail-closed on a hash mismatch.
- Deterministic: sorted payloads, no timestamps. The same input gives the same bytes.
- Tests: roundtrip, tamper detection, determinism, oversize limits.

**2B – `harw-agent-compiler` + CLI**
- Driver:
  1. discovery (layers like `harw-config`, or an explicit folder)
  2. parse / resolve / `lower_v2`
  3. passes:
     - `ValidateRoles`
     - `RightsCheck` (manifest ≤ base role ≤ author ceiling)
     - `ResolveSkills` (from `SkillIndex`, embed the content)
     - `ReachableTools` (which tool providers are needed)
     - `PruneUnusedTools`
     - `ResolveModels` (required env vars in the manifest)
  4. backend
- **Additional CLI commands** (Mia: "build a lot into the CLI"). All of them offer `--json`, and they are also available in the TUI as `/agent <cmd>`:
  - `harw agent graph [name|--all] [--format text|dot|mermaid|json]`:
    - the delegation/family graph (who can spawn whom, with depth and read-only/writing markers)
    - the resolution graph (`extends` → mixins → patches per layer)
    - rights flow (manifest ≤ base ≤ ceiling) as edges with narrowings
  - `harw agent explain <name> [field]`: explains the provenance of every IR value (which layer, file:line, which patch op set it, and why a tool is admitted or refused). `harw agent explain HARW-PATCH-003` explains a diagnostic code with an example and a fix.
  - `harw agent new <name> [--role worker|child-orchestrator] [--extends …]`: scaffold with a commented `definition.toml` and `system.md`.
  - `harw agent fmt [--check]`: canonical TOML formatting of definitions.
  - `harw agent diff <a> <b>`: IR diff between two definitions, versions or artifacts (rights delta highlighted).
  - `harw agent test [name]`: runs the definition's golden or example tests with the offline echo.
  - `harw agent list` (already exists) additionally shows the snapshot hash and the build state.
- CLI `harw agent check|build|inspect|run`:
  - `check` shows diagnostics.
  - `build <name|path> [--interface cli,mcp,http,tui,repl] [--native] [-o out] [--target]`.
  - `inspect <binary|artifact>` shows the manifest, hash, interfaces and skills.
  - `run` executes an artifact directly without building.
- Builds run as a **job** (`harw-tool-job`), with progress in the panel. Inside harw there is a `/agent build` op; for agents there is an `agents.build` tool (rights like `shell.exec`, only with approval).
- **Backend A:**
  - Copy the runner binary for the target and append the artifact.
  - Where the runner comes from: installed next to `harw` (release archive) or from `~/.harw/runners/<target>/<version>`.
  - If the runner's interface features don't cover the requested interfaces, this is a clear error, with the hint to use `--native` or install the full runner.
- **Backend B `--native`:**
  - Generate a Rust crate (template in the compiler: `Cargo.toml` with only the needed interface and tool features, `main.rs`, `include_bytes!` of the artifact) and build it with `cargo build --release` as a job.
  - Needs a Rust toolchain and path dependencies on the harw sources; if either is missing, a clear error.
- Needs a **capability catalog** name → tool provider (composition contract §6) in `harw-registry-defaults`, so that native builds and `PruneUnusedTools` link exactly the admitted tools.
- Tests: `check` diagnostics; `build` of an analysis-family agent → artifact hash stable; inspect output; the native codegen crate snapshot (generated files as golden; no cargo run in tests).

## Wave 3: runner and interfaces

**3A – Runtime entry for compiled agents**
- New `EntryKind::CompiledAgent`. Its profile is derived from the **manifest**, not from a table row.
- `RuntimeSpec.embedded: Option<EmbeddedAgent>`: config, agent, skills and knowledge come **from memory**. Add a `load_config_embedded` path so nothing has to be written to `~/.harw`. An optional state dir is used only for sessions and logs.
- Rights: min(manifest, CLI flags, local deny rules).
  - The manifest is shown at startup.
  - `--full-access` only explicitly, and only within the manifest.
  - Network: only the hosts/modes named in the manifest.
  - Secrets: `env:` from the manifest's required list; a missing variable is a clear error.
- Sandbox as in harw: bwrap from the system; a missing bwrap fails closed for shell tools, with a message.

**3B – `harw-agent-runner`**, with cargo features `cli` (one-shot), `repl`, `mcp`, `http` and `tui`. The default runner in the release has all of them.
- **CLI/REPL:** `./agent "task"`, interactive, approval dialogs in the terminal, `--json` output.
- **MCP:** the agent as one tool `run` (plus `status`/`cancel`) over stdio and Streamable HTTP, reusing parts of `harw-mcp-server`.
- **HTTP/JSON:** `POST /run`, `GET /runs/{id}`, SSE events (the `SdkEvent` schema), bearer token from env.
- **Mini-TUI:** `harw-tui` with the agent fixed. The UIA switch, agent selection and model switch are hidden or restricted to the manifest.
- **Interface selection:**
  - Order: the DSL `[binary].interfaces` sets the default, `harw agent build --interface` overrides it, and at runtime `./agent --interface mcp` chooses among the built-in interfaces.
  - Non-built-in interfaces give a clear error.
- `./agent --manifest`, `--version`, `--verify` (checks the hash).
- Tests:
  - e2e with offline echo for each interface: CLI one-shot, a REPL script, an MCP `tools/call` over stdio, and HTTP `POST /run` plus SSE.
  - The manifest limit is enforced: a tool outside the manifest is refused, and `--full-access` does not widen it.
  - A tampered binary refuses to start.

**3C – Child agents of a compiled parent via the job system** (Mia's decision)
- **Build:** a compiled orchestrator bundles its reachable child agents (`delegation_targets` / `child_orchestrators`, transitively) as sub-artifacts: new `PayloadKind::Agent` (a nested artifact per child) in `harw-agent-artifact`. The compiler resolves the family closure, and its rights check verifies child ≤ parent.
- **Content-addressed shared payload pool** (a review suggestion):
  - The parent artifact stores every payload exactly once, under `pool/<blake3>`. Agents are small entries (IR plus `payload_refs`) with no copies of their own, so several children sharing skills store them only once.
  - The runner resolves refs from the pool and fails closed on a missing or tampered blob.
  - The build cache and the GC also work on shared blobs.
  - `inspect` shows the dedup savings.
- **Runtime:** instead of in-process `ManagedAgentSpawner` children, the runner starts each child as a **job** via `harw-tool-job` (`JobManager`). The child is the same binary with `--child <agent-id> --child-protocol stdio`.
  - The JobManager provides process group, start/stop/status, logs, progress and end events to the parent (`JobNotifier`), with no tmux, systemd or shell in between.
  - The direct control channel is a JSON-lines protocol over the job's stdin/stdout. Parent → child: `task`, `message`, `answer`, `cancel`, `budget`, `mode`. Child → parent: `event` (in the `SdkEvent` format), `question`, `approval_request`, `result`, `usage`. `ManagedAgentSpawner` gets a new backend `JobChildBackend`, so `transfer_to_*`, `agents.delegate`, `delegate_wave`, `agent.message/status/result` and the TUI panel work unchanged.
  - The rights of each child are min(child manifest, the parent's current rights). The live mode and the budget are passed down through the protocol. Approvals from the child go to the parent (relay as today).
  - Crash containment: a crashed child becomes a `ChildEnd` with a cause. Its logs stay in the job directory.
- **Configurable:** `[binary] child_execution = "job" | "in-process"`. Default for compiled binaries is `job`; inside harw itself it stays in-process.
- **Tests:**
  - A compiled parent with two workers runs both as jobs.
  - A question and an approval relay work.
  - `cancel` kills the process group.
  - A child crash is reported cleanly.
  - A child's rights never exceed the parent's.

## Must work after `make install` (Mia)
- `make install` builds and installs `harw-agent-runner` next to `harw`, and writes an install record (`source_dir`, `version`, `target`).
- `harw agent build` then works with no further setup: the runner is found next to `harw`.
- `--native` fills in everything itself from the install record: the harw sources, cargo detection, `rust-toolchain.toml`, a build cache under `~/.harw/cache/agent-builds/`, and running as a job. Only a missing Rust toolchain gives a clear hint (rustup one-liner); nothing is installed without approval.
- `harw agent doctor` checks the runner, the native prerequisites and the install record.
- **Standard bin in `.harw`** (Mia):
  - Compiled binaries go to `~/.harw/bin/<name>`, the current version as a symlink.
  - Every version is kept under `~/.harw/bin/.versions/<name>/<version>-<digest>/` with a `build.json`.
  - `harw agent versions|use`; `-o` copies in addition.
  - `harw agent doctor` checks `PATH`.
  - Runner copies go under `~/.harw/bin/.runners/`.
  - The cleaner keeps the current version plus N older ones.
- **Built-in agents stay embedded** (28 roles, bases, bundled agents) and are never built automatically. The compiler is opt-in per agent, and there is no `--all`.
- **Exception, the UIA is built automatically:**
  - Triggers: the UIA definition changed (digest comparison), and after `make install` or a version change.
  - Artifact backend only. It runs in the background, never blocks, is skipped without a runner, and is skipped when the digest is unchanged.
  - Target is `~/.harw/bin/<name>`; interfaces default to `tui`, `repl` and `cli`.
  - It can be switched off with `[agent_compiler] auto_build_uia`.
- **The UIA with `--native` becomes a complete harw:**
  - The full program (TUI, all roles, tools, commands) with the UIA baked in as the fixed root, i.e. a personalized harw named `harw-<uia-name>`.
  - It is built from the harw-cli entry plus the embedded UIA artifact.
  - The auto-build stays artifact-only.
  - **Own, completely independent home `~/.<uia_name>`** (Mia): no inheritance from `~/.harw`. The first start scaffolds and runs onboarding; providers and credentials can be imported from `~/.harw` on request. It has its own `bin/`, sessions and memory.
  - **Project folders too:**
    - Every per-project path derived from the home (project settings, trust store, state dir, jobs, plans, build cache) follows `~/.<uia_name>`.
    - In the repo it uses `<project>/.<uia_name>/` instead of `.harw/`, from one central `project_dir_name()`.
    - On first use in a project that has a `.harw`, it offers once to copy only the project configuration; trust is asked again.
- **Build-cache cleaner:**
  - Native builds share one cargo target dir, so dependencies compile only once.
  - After every build an automatic GC runs: a size cap (default 5 GB, configurable), LRU across build dirs, `cargo clean` when the cap is still exceeded, and removal of stale versions. `-o` outputs are never touched.
  - `harw agent clean [--all|--older-than|--keep|--dry-run]` with a size report.
  - Uninstall removes the cache.

## Wave 4: build, release, gates
- `Cargo.toml` workspace members; `[profile.release-runner]` (lto fat, `codegen-units = 1`, `strip`, `panic = abort`).
- Release workflow: `harw-agent-runner` for x86_64 and aarch64 in the archive next to `harw`. Makefile `install` also installs the runner.
- xtask privileges gate: a row for `harw-agent-runner` in `MONITORED_BINARIES`/`CRATE_PRIVILEGE`, Unprivileged.
- `cargo deny` without new C dependencies. The runner must avoid the `keyring`/dbus dependency through a feature (secrets only via `env:`).
- Docs: README section "Agenten kompilieren", a guide `docs/guides/agent-compiler.md`, and an example `examples/agents/hello-analyst/` with a build.

## Critical files
- `harw-agent-dsl/src/{executable.rs, resolve.rs, merge.rs, parse.rs, context_program.rs}` plus new `ir_v2.rs`, `diagnostics.rs`, `bind.rs`
- `harw-registry-defaults/src/{embedded_agents.rs, roster.rs, profile.rs, agent_definition_tools.rs}` plus the new capability catalog
- `harw-config/src/discovery.rs`, `harw-runtime/src/{assembly.rs, children.rs, spec.rs, config.rs}`, `harw-core/src/{session.rs, child_controller.rs}`
- New crates `harw-agent-artifact`, `harw-agent-compiler`, `harw-agent-runner`; `harw-cli/src/cli/agent.rs`, `agent_cmd.rs`
- Reused: `harwness-sdk` (session and event model), `harw-mcp-server` (transport), `harw-tool-job` (build as a job), `SkillIndex`, `harw-tui`

## Verification
- The central build after each wave: fmt, clippy `-D warnings`, tests including doc tests, gates, deny, dod, actionlint.
- End to end, in a test and by hand:
  1. `harw agent check evidence-critic` gives 0 diagnostics; a broken definition gives codes with file:line.
  2. `harw agent build evidence-critic --interface cli,mcp -o /tmp/ec` produces one binary; `./ec --manifest` shows tools, network and budget; `./ec --verify` is OK.
  3. `HARW_OFFLINE_ECHO=… ./ec "Prüfe diese Quelle"` answers; `./ec --interface mcp` answers `tools/call` over stdio.
  4. Change one byte in the binary → start refused.
  5. The same build twice gives the same artifact hash.
  6. `--native` generates the crate (golden) and, with a toolchain present, builds a smaller binary.
- Regression: every existing role behaves identically (tool-surface snapshot before and after 1B).
