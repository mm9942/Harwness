---
id: HP-CATALOG
title: Harness patterns — Claude Code × Codex × OpenClaw × harw
status: living
date: 2026-09-27
tags: [harness-patterns, catalog, roadmap, multi-provider]
related:
  - claude-code.md
  - codex.md
  - openclaw.md
  - gateway-contract.md
  - cost-model.md
  - ../66-placement/README.md
  - ../67-containers/README.md
  - ../85-gap-hunt/patterns.md
---

> Opus synthesis of the research sheets, followed by an adversarial Opus critic.
> Its corrections are incorporated into the text (section "Critic corrections"
> at the end). Where the catalog and a correction contradict each other, **the correction wins**.

# Cross-harness pattern catalog: Claude Code, Codex, OpenClaw → harw

Every harw claim below was checked against HEAD. The sources are the five sheets in this folder (`claude-code.md`, `codex.md`, `openclaw.md`, `gateway-contract.md`, `cost-model.md`), placement (PL-66), cloud sessions (PL-65) and the gap-hunt patterns. An adversarial review spot-checked the citations; its corrections are applied in place.

## 1. Summary: five insights

**1. The hooks wire format is converging, but harw should adopt the veto semantics, not the wire format.** Codex deliberately copies Claude Code's hook JSON: same camelCase output, same `permission_mode` enum, `CLAUDE_PLUGIN_ROOT`, and a type called `ClaudeHooksEngine`. harw has no hook engine; a grep for `PreToolUse` or `hook_event_name` finds nothing. It does have the stronger core: in `harw-extension-api/src/allow_rules.rs:63` Deny always wins, and a poisoned lock fails closed to Deny (`:803-822`). Claude Code's contract is fail-open: any exit code other than 0 or 2 does not block, and `--bare` skips all hooks. harw should build hooks as an in-process veto fold and speak the Claude Code/Codex wire only through an adapter.

**2. Goal loops are stop gates, and harw's goal loop is already stricter than both.** Claude Code's `/goal` is a prompt-based Stop hook where a small model judges `met`, `not yet met` or `impossible`. Codex's `ext/goal` gives the model an `update_goal` tool. In harw the model can never declare a goal achieved or abandoned (`harw-ops/src/goal.rs:10-20`). The judge verdict is `passed: bool` and fails closed (DEC-001), and judge output is capped at 256 tokens (DEC-002). The missing piece is only a way to use this gate from chat sessions.

**3. Per-run caps are the wrong unit; harw needs one ledger.** Claude Code workflows cap concurrency per run (default 16, depends on CPU count). The gap-hunt kit states the cap as CPUs − 2 per workflow and starts one top-level run per area *in order to* multiply it (`85-gap-hunt/kit/skills/gap-hunt/SKILL.md:24-27`); that parallel cut is P6's remedy. harw already has one provider authority (DEC-003: `ProviderRateLimiter`, `pacing_wait` at `harw-core/src/model.rs:647`). But the work driver keeps its own `parallel_ceiling` with `RESERVED_PROVIDER_SLOTS = 1` (`harw-cli/src/job_worker_work_driver.rs:164,374`), and subagents have their own `ChildLimits` (conservative defaults depth 4, 8 per parent in `harw-core/src/child_controller.rs:686-691`; the production values are set in `harw-runtime/src/budget.rs:268-277`, where depth is configurable from 1 to 6). The ledger holds provider slots (DEC-003/023) *and* node capacity (CPUs − 2, plus a `BuildSlot` of 1 per workspace, DEC-004; placement §6 and "Additions from the session"). Per-run caps go away, so the parallel cut stops being a workaround and becomes a placement decision (pattern M2).

**4. Cost must be cache-aware, computed from data, and computed by harw.** Claude Code reports `total_cost_usd` and survives resume. Codex reports tokens only. OpenClaw treats the provider's usage API as primary; harw's own sheets decide the opposite (D10). The usage fields are right: `TokenUsage` has `cache_separate` (`harw-types/src/usage.rs:8-22`) and `UsageRound` records usage per provider and model (`harw-core/src/state_store.rs:287`). But mixed-provider sums double-count (`cache_separate |=`, `harw-types/src/usage.rs:85`), and the Anthropic hit rate prints about 7000 % (`harw-ops/src/usage.rs:113`). harw computes no dollar amount anywhere. The catalog's `Pricing` type is `f32` and has no cache-write price (`harw-model-catalog/src/descriptor.rs:282-288`). With about 98 % cache reads, the cache-read price decides cost (GLM-5.3 Flash on Workers AI: $0.03 vs $0.15 input; its cache-write price is unpublished, `cost-model.md:72`). So prices live in a route-keyed `prices.toml`, never in code, with exact integer types (`RatePerMTok`, `PicoUsd`, `MicroUsd`) in a new ring-I crate `harw-cost` (`cost-model.md` §2.2, §2.5, §2.6). Cache affinity follows the prefix group (`x-harw-cache-affinity`), not the identity.

**5. A single daemon should be the control plane.** OpenClaw's Gateway and Codex's `app-server` both confirm the shape PL-65 is aiming for. harw already has the daemon: `harw-cli/src/gateway.rs` runs Telegram turns, the dream scheduler and the audit-chain scheduler. But the TUI still embeds the runtime (`harw-tui/src/app.rs`, 14,943 lines). The scheduler, the headless stream and hooks should run through that daemon, not through each client.

**Where the sheets disagree with each other or with the code**
- **D1, headless mode.** The Claude Code sheet rates `-p` mode as "teilweise" (partial, `claude-code.md:100`). `harw exec` exists (`harw-cli/src/cli/mod.rs:142` → `EntryKind::OneShot`, `harw-cli/src/lib.rs:716`). It prints only the last assistant text (`harw-cli/src/chat.rs:1082`), and the global `--json` flag (`harw-cli/src/cli/global.rs:80-82`) is not passed to it (`harw-cli/src/lib.rs:739-743`). Both texts miss that `harw-agent-runner` already has a headless contract: `--json` with one object per `SdkEvent` and exit codes 0/1/2/3 (`harw-agent-runner/src/iface/cli.rs:9-10,16-26`), plus an SSE event stream (`iface/http.rs:8-9`).
- **D2, scheduler.** The Claude Code and OpenClaw sheets both say "no scheduler". A dependency-free 5-field `CronSchedule` exists (`harw-knowledge/src/context_steward.rs:811,843,899`). So does a `[dream] schedule` setting (`harw-config/src/harness_config.rs:239-258`), persisted `DreamSchedulerState` (`harw-knowledge/src/dream.rs:533`), `audit_chain_scheduler` (`harw-cli/src/gateway.rs:1103`) and `dream_scheduler` (`:2287`). What is missing is a general, user-facing scheduler.
- **D3, hooks.** The Claude Code sheet says build `harw-hooks`; the Codex sheet says don't build a hooks engine. Resolved as H7 below.
- **D4, approvals.** The Codex sheet says harw has "no per-session policy". `ApprovalMode{AlwaysAsk, Delegated, FullAccess}` exists (`harw-extension-api/src/approval_mode.rs:51-59`), set by `--approval` (`harw-cli/src/cli/global.rs:102`).
- **D5, pricing.** Placement says `Pricing` is "unpopulated". It is filled for Anthropic (15 entries through the `ClaudeSpec` price tuple, `vendor_anthropic.rs:72`), Mistral (10) and Z.AI (14) in the `vendor_*.rs` files. The built-in entries have `pricing: None` with a comment saying the authoritative prices live in `harw-provider` (`descriptor.rs:418`). `harw-provider/src` contains no pricing code at all. GLM-5.3 Flash is not in `vendor_zai.rs`; it appears only in `harw-model-catalog/src/providers.toml:453-455,754-755`.
- **D6, schema type.** The Codex sheet proposes `serde_json::Value` on `ModelRequest`. Placement §5 bans `serde_json::Value` from its public APIs. A harw-owned `JsonSchema` exists (`harw-tools/src/schema.rs:24`) and should be used.
- **D7, DEC numbers.** Placement says DEC-001 to DEC-019 are "taken". Only DEC-001 to DEC-008 are written. DEC-009 and DEC-010 are proposals (Copilot tasks C-08 and C-10), and DEC-011 to DEC-019 are reserved by PL-65 (`65-cloud-sessions/README.md:758-766`).
- **D8, file location.** `SandboxRequirement` is defined in `harw-job-core/src/spec.rs:249`, not in `harw-job-exec/src/plan.rs` as the Codex sheet says; `plan.rs` only uses it (`:13,62`).
- **D9, workflow cap figure: not a real disagreement.** "default 16, CPU-dependent" (`claude-code.md:81`) is compatible with P6's CPUs − 2 per workflow. Both are per-run caps.
- **D10, cost authority.** OpenClaw treats the provider's usage API as primary and its own estimate as fallback (`openclaw.md:173`). `cost-model.md` §3 and `gateway-contract.md:78-79` make harw's own computation authoritative. Resolved in DEC 5 (§6).

## 2. Pattern table

| ID | Pattern (problem → solution) | Claude Code | Codex | OpenClaw | harw today | Rec. |
|---|---|---|---|---|---|---|
| H1 | Many clients split state → one daemon owns sessions | none (CLI per session) | `app-server` JSON-RPC daemon | Gateway daemon | **partial**: `harw-cli/src/gateway.rs` (Telegram, dream, audit); TUI embedded (`app.rs`); PL-65 RS3/RS4 | adopt (via PL-65) |
| H2 | Automation needs machine output → typed event stream + terminal result | `stream-json`, `system/init`, `api_retry`, `result{total_cost_usd}` | `exec --json` ThreadEvent; exit code unreliable | typed WS schema | **partial**: `harw exec` text only (`chat.rs:994-1083`); `harw-agent-runner` has `--json` per `SdkEvent`, exit codes 0/1/2/3 and SSE (`iface/cli.rs:9-26`, `iface/http.rs:8-9`) through two hand-written encoders (`child.rs:479`, `iface/cli.rs:248`); `SdkEvent` is `#[non_exhaustive]` and deliberately not `Serialize` (`harwness-sdk/src/event.rs:135-136`, `iface/cli.rs:245-247`) | adapt: one shared encoder framed per DEC-012 + `result`, reusing the runner's exit codes |
| H3 | Free text breaks parsers → schema at the wire | `--json-schema` → `structured_output` | `--output-schema` → `text.format` (caveats #19816, #22998) | – | **missing at wire**: `ModelRequest` (`model.rs:117`) has no schema; post-hoc `ReturnPipeline` (`harw-agent-dsl/src/executable.rs:572`) | adopt, keep post-hoc validator as backstop |
| H4 | Lost/concurrent sessions → durable resume, fork, write fencing | `--resume`, cost restored | rollout jsonl(.zst), resume, fork, `--ephemeral` | per-session lanes, `activeWriterRunId` | **present**: `session resume` (`cli/session.rs:22`), leases with epochs; fork only in matrix runner (`harw-ops/src/matrix/runner.rs:2399`); exec cannot resume (`chat.rs:95`) | keep; chat fork later |
| H5 | Runaway fan-out → spawn limits | 20 concurrent, depth 3 | `spawn_agent`, `[agents]` | composite session keys | **present**: fan-out ≤ 8 and only narrows (`with_max_children`, `child_controller.rs:734`); depth defaults to 4 and is configurable from 1 to 6 (`harw-runtime/src/budget.rs:271-277`, `harw-config/src/agent_limits.rs:47-49`); fields are `pub` (`child_controller.rs:675-682`); 15 min lease | keep; fan-out = `min(ChildLimits, ledger.available())` |
| H6 | Big jobs need determinism → scripted workflows with journal/resume | JS `agent/parallel/pipeline`, journal, per-run cap | – | – | **missing**; WorkDriver is a fixed algorithm (`harw-plan-bridge/src/work_driver.rs:980`) | adapt: Rust builder API, cap = ledger |
| H7 | Policy extension points → hook catalog + JSON contract + veto | ~20 events; exit 2 blocks, **other codes fail open** | 12 events, Claude Code wire | `before_tool_call` terminal veto | **missing** generically; deny-wins fold present (`allow_rules.rs:63,803`); observer-only `SessionLifecycleHook` (`harw-runtime/src/assembly.rs:259`) | adapt: in-process, veto-only, fail-closed; wire as adapter |
| H8 | Premature stop → goal/stop gate | `/goal` prompt Stop hook | `update_goal` tool + `continuation.md` | – | **stronger**: achieve/abandon command-only (`harw-ops/src/goal.rs:10-20`); `GoalContextProvider` (`goal_context.rs:78`); DEC-001/002 | keep; expose as Stop gate |
| H9 | Proactive work → cron/heartbeat | `CronCreate`, 1 s tick, jitter, 7-day expiry | none | cron + cheap heartbeat | **partial**: `CronSchedule` + dream/audit schedulers (D2) | adopt generic scheduler, reuse `CronSchedule` |
| H10 | Tool bridges bypass policy/typing → MCP with `outputSchema`, one policy | client + `mcp serve`, elicitation, output caps | client only (2025-06-18) | MCP tools use the same policy | **partial**: client and server on 2025-06-18 (`harw-mcp-client/src/lib.rs:17`, `harw-mcp-server/src/lib.rs:15`); `McpTool` has only `input_schema` (`lib.rs:117`); output caps 64 KiB to the model (`tool_bridge.rs:49`) and 10 MiB per response (`harw-mcp-client/src/lib.rs:24`); no elicitation; server spawned outside sandbox check (`tool_bridge.rs:21-23`) | adopt `outputSchema` + equivalence test; elicitation later |
| H11 | Isolation → sandbox modes vs requirement levels | permission modes | read-only / workspace-write / danger-full-access; bwrap | tool policy before sandbox; path denylist | **stronger for jobs**: `SandboxRequirement` (`spec.rs:249`), `RequirementLevel` (`ir_v2.rs:961`); the job executor withholds `/sys`, `/run` (`harw-job-executor-bwrap/src/executor.rs:48`). **present for the shell tool**: its sandbox binds the `/run/harw` sockets (`harw-sandbox/src/bwrap.rs:62-68`) and, as an opt-in, host `PATH` directories (`bwrap.rs:5-7`, used at `harw-tool-shell/src/exec.rs:711`). Both find bwrap only at fixed paths (`bwrap.rs:59,212`; `executor.rs:78-83`) | keep; mapping table; fix IR vs job-core `Required` (DEC-027) |
| H12 | Who approves → approval policy | 5 permission modes | untrusted / on-request / granular / never | dmPolicy | **present**: session `ApprovalMode` + per-op `approval="always"` (DEC-007). exec turns down every question that would go to a human (`chat.rs:1045-1063`). Calls that need no question still run: all of them under `--approval full` (only a warning, `chat.rs:402-406`), and under `Delegated` whatever the classifier clears (`approval_mode.rs:84-86`). Deny rules win in every mode (`harw-runtime/src/approval.rs:1465`) | keep |
| H13 | Project context → instruction files | CLAUDE.md | AGENTS.md(+override) | not covered | **present**: HARW/AGENTS/CLAUDE.md additive, 32/128 KiB (`harw-project-discovery/src/discovery.rs:254-260`) | keep; record DEC |
| H14 | Cost is invisible → normalized usage, price data, harw-computed cost (provider as cross-check) | `total_cost_usd`, `modelUsage`, OTel cost | tokens only | provider usage API primary | **partial**: tokens only (`harw-ops/src/usage.rs`); `Pricing` `f32`, no cache-write (D5); mixed-provider sums double-count (`harw-types/src/usage.rs:85`); Anthropic hit rate about 7000 % (`harw-ops/src/usage.rs:113`) | adopt per `cost-model.md` C1–C3 (R1) |
| H15 | 429 storms → pacing + wait budget + visible retries | `system/api_retry` | rate limits absent in exec (#14728) | error taxonomy | **stronger**: header families (`rate_limiter.rs:12-19,350`), budget (`budget.rs:653,714`), 429 wait budget (`retry.rs:21-33`); no retry event | keep; emit events |
| H16 | Bad key or model → classified failover, credential pool | – | OAuth refresh | turn-scoped failover, `model_fallback_decision` | **present**: `should_failover` only on Auth/Quota (`credential_pool.rs:341-344`); `codex_refresh.rs` | keep; add decision event |
| H17 | Device trust → pairing codes, signed identity, revocation | – | – | pairing, ≤3 pending | **mixed**: single-use redemption (`redeem_once`, `harw-channel/src/pairing_store.rs:301`) and a 15-min TTL (`pairing.rs:41`); PQ mTLS for nodes. Codes are stored in clear text (journal `pairing.rs:187`, store `pairing_store.rs:69,77`), compared in non-constant time (`pairing.rs:114`) and repeated in error messages (`:117,122`); no pending or attempt cap. Channel-binding revocation exists (`pairing_store.rs:5`); device revocation is missing (PL-65 §4.4) | adopt: hash codes, constant-time compare, caps, device revocation (R11) |
| H18 | Supply chain → skill/plugin/config-layer trust | plugins (not read in detail) | `[otel]` blocked in project-local config | ClawHub manifest-vs-code check | **present**: BLAKE3 + owner uid, `Changed` → restricted (`harw-home/src/trust.rs:15,158`); skills carry no rights (`harw-agent-dsl/src/skills.rs:21`); `TRUST_DIGEST_DIRS` has no hooks directory (`trust.rs:94-110`) | keep; key denylist for untrusted layers; hooks dir in the digest (R6) |
| H19 | Cold caches, wrong node → placement + prefix-group affinity | – | – | – | **partial**: identity headers opt-in; `x-session-affinity` never set on the own-Worker route (`harw-provider-http/src/lib.rs:3986`; test `:8486`); the direct Workers AI route does not send it yet (`cost-model.md` F7); no engine, no `x-harw-cache-affinity` | adopt P1–P5 + header; direct route may set `x-session-affinity` from the tenant-scoped prefix group (cost-model C4) |
| H20 | Silent misconfig → strict schema, atomic writes | – | project-local denylist | refuse to start on unknown keys | **present**: all 17 `harw-config/src/*_toml.rs` files use `deny_unknown_fields` | keep; add gate |
| H21 | Context overflow → boundary-aware compaction | Pre/PostCompact hooks | Pre/PostCompact | pairs kept together, safeguard retries | **stronger**: `atomic_groups` (`harw-core/src/compaction.rs:20,1041`), deterministic pass (`:505`), 70 %/30 % thresholds (`auto_compact.rs:30-55`) | keep; add compact hook events |
| H22 | Slow turns block transport → async ticket + idempotency key | background agents | – | `runId` + `agent.wait` | **stronger**: BLAKE3 idempotency key, `Duplicate` (`harw-core/src/admission.rs:17,88-92`) | keep; reuse for scheduler |
| H23 | Opaque ops → OTel metrics | cost/token metrics | `codex-otel` | – | **present**: `harw-observe` with otlp, prom and file sinks | keep; add cost metric |

### 2a. Also present, and follow-ups

Present in harw (keep):
- Versioned protocol: `ProtocolVersion` (`harw-protocol/src/wire.rs:12`), as in OpenClaw's typed schema.
- Composite session key plus admission gate: `SessionKey` (`harw-channel/src/ids.rs:21`), `admit` (`harw-channel/src/adapter.rs:95`).
- Memory provenance and single-writer consolidation: `Provenance` (`harw-memory/src/epistemic.rs:79`), `ConsolidationLock` (`harw-memory/src/consolidation.rs:665`).
- Secrets by reference: `SecretRef` (`harw-config/src/auth_toml.rs:23`).
- Agent definitions as DSL/IR, with `agents` in the trust digest (`harw-home/src/trust.rs:104`), as in Claude Code's subagent files.
- Tool-name mapping for provider limits: `ToolNameCodec` (`harw-provider-http/src/tool_names.rs:49`), as in Codex.

Follow-ups:
- MCP elicitation (Claude Code) is missing in harw.
- Check that model-written compaction summaries are validated before they replace history (OpenClaw).
- Check config strings that reach `process::Command`, e.g. MCP `command`/`args` (`harw-config/src/mcp_toml.rs:16`), against the CVE-2026-25593 class (§4).
- Running the Codex CLI as a sandboxed worker: deferred.
- From the cost model (C3, C5): `CostLimiter` (reserve the worst case, then reconcile); a gateway spend-limit 429 maps to `CostExhausted` and a re-place, not a retry storm; an optional AI Gateway profile with `cf-aig-collect-log-payload: false` and `cf-aig-no-wholesale`.

## 3. harw strengths to keep

| Strength | Invariant | How to protect it |
|---|---|---|
| Rights only ever narrow: `AuthorityCeiling::intersect` (`harw-agent-dsl/src/authority.rs:102`), `ContextCeiling::intersect` (`harw-context/src/ceiling.rs:233`), `with_max_children`, `narrow_spec` (DEC-007) | no `union`, `widen` or merge that grows a ceiling | keep the example-based law tests (`ceiling.rs:375-403`; property tests are a follow-up); new xtask gate forbidding `pub fn (union\|widen)` in these modules, exempting `ExecutionRequirements::union_child` (`harw-agent-dsl/src/ir_v2.rs:1106`), which covers children's requirements by design; `PlacementGrant::meet` gets the same laws (P1) |
| Deny wins, fail-closed (`allow_rules.rs:63,803-822`) | hooks and MCP go through this same fold; a hook can never un-deny | hook tests plus an MCP ≡ native test (R3, R6) |
| The model can never finish a goal (`harw-ops/src/goal.rs:10-20`); judge fails closed (DEC-001) | a stop gate may only block stopping | test at `CallSurface::Model`; stop-gate enum without an "Achieved" variant |
| One provider authority (DEC-003) | exactly one limiter per provider per process; the ledger wraps it (DEC-023) | cross-layer invariant test suite (M2) |
| `harw-web` binds only a Unix socket with `SO_PEERCRED` (`harw-web/src/peer.rs:1-10`, `server.rs:543`). Two other local surfaces use loopback TCP: `harw-agent-runner --listen` (default `127.0.0.1:8787`, `iface/http.rs:85`; bearer token optional on loopback, Origin check only, `http.rs:20-24`, `iface/net.rs:45-58`) and `harw-mcp-server` (bearer token, `transport.rs:1,196`) | no new loopback-TCP control port, and a token is required on loopback too (protects against ClawJacked) | DEC + review rule; test that the web listener is a `UnixListener`; R11 makes the runner token mandatory |
| bwrap safety: fixed-path discovery in both backends (`harw-sandbox/src/bwrap.rs:59,212`); the job executor withholds `/sys` and `/run` (`harw-job-executor-bwrap/src/executor.rs:48`) | bwrap never found via PATH; job sandboxes never see `/sys` or `/run`; the shell sandbox binds only `/run/harw` and host `PATH` dirs only on opt-in | named TOCTOU gap-hunt pass over `harw-job-executor-bwrap`, `harw-sandbox`, `harw-tool-shell` and the job crates |
| exec never asks a human (`chat.rs:1045-1063`); deny rules win in every mode (`harw-runtime/src/approval.rs:1465`) | exec never approves a question implicitly; unasked calls follow the session mode (`full`, `Delegated` classifier) | keep when adding `--output-format`; the `result` event reports denied calls |
| `x-session-affinity` never set on the own-Worker route (`lib.rs:3986`, test `:8486`) | affinity comes from prefix groups within one tenant; the direct Workers AI route may set it from the tenant-scoped prefix group | extend the test when adding `x-harw-cache-affinity` and the direct-route header |
| Strict config (`deny_unknown_fields`) | no unknown key accepted | xtask gate that scans `*_toml.rs` (none exists today; the five gates are edges, privileges, warden-deps, warden-cbuild and arch, `xtask/src/gates.rs:156-162`) |

## 4. Anti-patterns and incidents, and how harw avoids them

- **OpenClaw CVE-2026-25253** (the UI auto-connected to a `gatewayUrl` from the query string): harw has no browser client that follows URLs, and PL-65 §4.2 pins the host fingerprint in the pairing string.
- **OpenClaw CVE-2026-25593** (unauthenticated `config.apply` set `cliPath`, leading to command injection): any hook command or process path must come only from trusted layers (`trust.rs`), never from a runtime RPC. `TRUST_DIGEST_DIRS` has no hooks directory yet (`harw-home/src/trust.rs:94-110`), so R6 adds one. Needs a DEC.
- **OpenClaw CVE-2026-24763 / CVE-2026-25475** (sandbox escape via PATH; model output used as a file path): bwrap discovery uses fixed paths only, but the shell sandbox can bind host `PATH` directories as an opt-in (`harw-sandbox/src/bwrap.rs:5-7`). Add "tool turns model output into a path" to the M1/M5 checklist for `harw-tool-fs`, `harw-tool-doc` and `harw-tool-browser`.
- **OpenClaw ClawJacked** (loopback treated as trusted): `harw-web` is a Unix socket, but `harw-agent-runner --listen` and `harw-mcp-server` are loopback TCP (see §3). With no token set, the runner lets every request through that passes the Origin check; R11 makes the token mandatory on loopback.
- **OpenClaw Claw Chain** (a TOCTOU chain across sandbox mounts): run a recurring named M1 pass over `harw-job-executor-bwrap`, `harw-sandbox` and `harw-tool-shell`.
- **Claude Code: hooks fail open** (exit codes other than 0/2 don't block) and `--bare` skips hooks: in harw a hook error or timeout on a Pre-event means Deny, and no flag disables deny rules.
- **Claude Code / P6: per-run caps multiply** when separate runs are started, which the gap-hunt kit does on purpose as its parallel cut: harw uses ledger leases over provider slots and node capacity instead (R9, R10).
- **Codex: exit codes are unreliable** (#4721, #41984): harw exec should reuse the runner's typed exit codes plus a mandatory terminal `result` event.
- **Codex: rate limits are `null` in exec** (#14728): configured caps must stay the control that carries the load (DEC-003).
- **Codex: `--output-schema` gaps** (#19816, #22998): keep the post-hoc `ReturnPipeline` validator.
- **Codex: near-daily wire drift and ToS exposure** (`originator` in `harw-provider-http/src/codex.rs:34-81`): mark wire code with "verified against commit X".
- **Two conflicting caps across layers** (M2): `RESERVED_PROVIDER_SLOTS` plus `ChildLimits` plus a future workflow cap would repeat this. The ledger removes it.

## 5. Consolidated roadmap

Rings follow `xtask/arch-policy.toml`. Rules: one agent per file; agents never build; shared files (`lib.rs` exports, `Cargo.toml`, `arch-policy.toml`) go into a separate sequential scribe wave (PL-65 §11.1); one central build after each wave.

| Round | Content | Files and crates (ring) | Patterns | Depends on |
|---|---|---|---|---|
| R0 | DECs (§6), this catalog as README, placement P0 | `docs/planning/70-decisions/*`, `docs/planning/75-harness-patterns/README.md` | all | – |
| R1 | Cost state per `cost-model.md` C1+C2 (merges the cost items of the Claude Code, OpenClaw and placement docs): new crate `harw-cost` (I) with `RatePerMTok`, `PicoUsd`, `MicroUsd`, `normalize`, `cost` (not `harw-types`, which `arch-policy.toml` already marks as overloaded, "Aufteilung folgt"); route-keyed `prices.toml` with `source`/`retrieved` in `harw-model-catalog`, `descriptor::Pricing` becomes a derived view; `UsageRound` breakdown; fix F3 (`harw-types/src/usage.rs:85`) and F4 (`harw-ops/src/usage.rs:113`); cost totals in the `SessionMeta` sidecar, because the snapshot uses `deny_unknown_fields` (`state_store.rs:262`); `[pricing]` override. F2 (`harw-provider-http/src/lib.rs:3911`) is done in R2, which owns that file | `harw-cost/src/*` (new, I), `harw-model-catalog/src/{prices.toml,price_book.rs}` (new) + `descriptor.rs`, `harw-types/src/usage.rs`, `harw-ops/src/usage.rs`, `harw-core/src/state_store.rs`, `harw-session-store/src/meta.rs`, `harw-config/src/pricing_toml.rs` (new) | H14, H23 | R0 (DEC-025 note) |
| R2 | Schema at the wire: `ModelRequest.output_schema: Option<JsonSchema>` (harw-owned type), mapped per adapter; the judge uses it. Also owns cost-model F2 (the Responses `cache_write_tokens` path, `lib.rs:3911`, with the Luna fixture test) and the "verified against commit X" marker in `codex.rs` | `harw-core/src/model.rs`, `harw-provider-http/src/{lib,codex,anthropic}.rs`, `harw-cli/src/job_worker_work_driver.rs` | H3, H14 | – |
| R3 | MCP `outputSchema`/`structuredContent`; test that an MCP tool is treated like a native tool under `allow_rules` | `harw-mcp-client/src/{lib,tool_bridge}.rs`, `harw-mcp-server/src/session.rs` | H10 | – |
| R4 | General scheduler (merges the Claude Code and OpenClaw items): schedule create/list/delete, a daemon tick, reuse of `CronSchedule`, each fire goes through `JobAdmissionService` with key `schedule:<id>:<fire_ts>`, `write_atomic` store under `HARW_STATE_DIR` with one writer, jitter, expiry. No project-local schedule files; if they come later, they need a trust-digest entry | `harw-ops/src/schedule.rs` (new), `harw-cli/src/gateway/scheduler.rs` (new), `harw-cli/src/gateway.rs` wiring (A) | H9, H22 | R0: DEC-016 (store in `HARW_STATE_DIR`) and DEC-010 (single-writer job store); otherwise two hosts fire the same schedule twice (PL-65:738) |
| R5 | `harw exec --output-format text\|json\|stream-json`: one shared encoder next to DEC-012's framing in `harw-protocol` (F) (`TurnEvent` is serde, `events.rs:50-52`), replacing the two hand-written encoders; terminal `result` with cost; the runner's exit codes 0/1/2/3; pass `--json` through (`harw-cli/src/lib.rs:739-743`); additive `SdkEvent::Retry` | `harw-protocol/src/headless.rs` (new), `harw-cli/src/{chat.rs,cli/global.rs,headless.rs,lib.rs}`, `harwness-sdk/src/event.rs`, `harw-agent-runner/src/{child.rs,iface/cli.rs}` | H2, H15 | R1 (R2 optional) |
| R6 | Hooks, veto-only: vocabulary and trait in `harw-extension-api/src/hooks.rs` (I); `CommandHook` speaking the Claude Code/Codex wire in `harw-runtime` (A), sandboxed, built on the observer-only `SessionLifecycleHook` (`harw-runtime/src/assembly.rs:259`); turn-loop wiring; judge as Stop gate with a bounded number of continuations; hook files enter the trust digest | `harw-extension-api/src/hooks.rs` (new), `harw-config/src/hooks_toml.rs` (new), `harw-runtime/src/command_hook.rs` (new) + `assembly.rs`, `harw-core/src/turn_loop.rs`, `harw-plan-bridge/src/goal_gate.rs` (new), `harw-home/src/trust.rs` | H7, H8, H21 | R0, R2 |
| R7 | Placement P1+P2, including the cost score over `harw-cost` (cost-model C4) | new `harw-placement-model` (I): `requirements.rs`, `offer.rs`, `decision.rs`, `filter.rs`, `score.rs` | H19, H14 | R0, R1 |
| R8 | Placement P3: offers and a ledger over the limiter; optional gateway response headers `x-harw-lane`, `x-harw-affinity` and capacity signals | `harw-job-runtime/src/offer.rs` (J→I), `harw-model-catalog/src/offer.rs`, `harw-provider-http/src/{ledger,rate_limiter}.rs`, `harw-netsec/src/offer.rs` | H15, H19 | R7 |
| R9 | Placement P4+P5 plus `x-harw-cache-affinity` (prefix group on `RequestIdentity`); `parallel_ceiling`, `effective_cap` and `ChildLimits` read the ledger, which may only lower fan-out: `min(ChildLimits, ledger.available())`. Placement P6 (DSL keys) can run in parallel | new `harw-placement` (A), `harw-agent-runner/src/admission.rs`, `harw-cli/src/job_worker_work_driver.rs`, `harw-plan-bridge/src/work_driver.rs` (`effective_cap`, `:980`), `harw-cli/src/verify_sandbox.rs`, `harw-runtime/src/budget.rs` (`ChildLimits` production site, `:268`), `harw-core/src/model.rs`, `harw-provider-http/src/lib.rs`; placement P6: `harw-agent-compiler/src/passes/placement.rs` | H5, H19 | R8 |
| R10 | `harw-workflow` (A): builder API (`agent`, `parallel`, `pipeline`, `phase`, `log`), schema retries, journal in `harw-job-store`, resume from the journal, concurrency through ledger leases; port the gap-hunt kit | new crate, `docs/planning/85-gap-hunt/kit/*` | H6 | R2, R9 (R5 for events); DEC-004 (`BuildSlot`) and DEC-007 rights, so "never builds" is compiled in rather than prompted (`claude-code.md:101`) |
| R11 | Security and ops slices: `DoctorCheck` self-audit (trait at `harw-install/src/doctor.rs:73`, registered in `health_checks`, `harw-cli/src/lifecycle.rs:71`); fallback-decision event; pairing hardening (codes hashed in store and journal, constant-time compare, no code in error messages, caps on pending codes and attempts); token required on loopback for `harw-agent-runner --listen`; denylist of config keys for untrusted layers; live device revocation after PL-65 RS2 | `harw-cli/src/lifecycle.rs`, `harw-provider-http/src/credential_pool.rs`, one `harw-observe` file, `harw-channel/src/{pairing,pairing_store}.rs`, `harw-agent-runner/src/iface/{net,http}.rs`, `harw-config/src/discovery.rs` | H16, H17, H18, H20 | – (revocation: RS2) |

Placement P7 (remote `HostFacts`) stays tied to PL-65 RS2/RS3.

**Safe to parallelize**
- **Wave A:** R1, R2, R3, R4 and R11. They are file-disjoint because F2 (`harw-provider-http/src/lib.rs`, 8,770 lines, M4) and the `codex.rs` marker both sit in R2. R1 and R2 share only the crate `harw-core` (`state_store.rs` vs `model.rs`); R1 and R4 share only `harw-ops` (`usage.rs` vs `schedule.rs`).
- **Scribe A (sequential, one agent):** `lib.rs` exports, `Cargo.toml` members and deps, and `arch-policy.toml` for Wave A: crate `harw-cost` (R1); modules `price_book`, `pricing_toml` (R1) and `schedule` (R4). Then the central build.
- **Wave B:** R5, R6 and R7. Disjoint: R5 owns `harw-protocol`, `harw-cli` headless files and `harw-agent-runner` encoders; R6 owns the hooks files, `harw-runtime/src/assembly.rs` and `harw-home/src/trust.rs`; R7 a new crate.
- **Scribe B (sequential):** modules `headless` (R5), `hooks`, `hooks_toml`, `command_hook`, `goal_gate` (R6), and crate `harw-placement-model` (R7). Then the central build.
- **Serial:** R8 → R9 → R10, each followed by its scribe step (`harw-placement`, `harw-workflow`) and a central build. R2 and R9 both touch `harw-provider-http/src/lib.rs` and `harw-core/src/model.rs`, so they never run in the same wave.

## 6. Decisions to record

Proposed titles without final numbers. They would start at DEC-028 or later, and the container design may claim numbers too. Status today: DEC-009 and DEC-010 are proposals, DEC-011 to DEC-019 are reserved by PL-65 and DEC-020 to DEC-027 by placement.

1. Hooks are veto-only and fail closed. The Claude Code/Codex wire is an adapter, not the core.
2. Stop gates can only block stopping. Achieve and abandon stay command-only, and continuations are bounded.
3. Structured output at the wire uses a harw-owned schema type, with the post-hoc validator kept as a backstop. Contract: [structured-output.md](structured-output.md).
4. The headless contract: one shared encoder, framed per DEC-012, plus a terminal `result`, reusing the runner's exit codes. exec never asks a human; deny rules win in every mode.
5. Cost authority: per-response usage × price data is authoritative, because reserve/reconcile needs it synchronously. Provider billing APIs and AI Gateway figures are asynchronous cross-checks. Prices are route-keyed data (`prices.toml`, `harw-cost` types) with cache-read and cache-write fields, and the cost state survives resume. This extends DEC-025.
6. No per-run concurrency caps. The ledger holds provider slots (DEC-003/023) *and* node capacity (CPUs − 2, plus a `BuildSlot` of 1 per workspace, DEC-004). All fan-out (workflows, subagents, schedules) takes ledger leases, and the ledger may only lower `ChildLimits` fan-out. Per-run caps go away, so the parallel cut (P6) stops being a workaround.
7. The workflow engine is a Rust builder API first. No embedded script engine appears in public types, and the journal lives in the job store.
8. One daemon scheduler: it reuses `CronSchedule`, fires through admission idempotency, stores under `HARW_STATE_DIR` with one writer, and has jitter and expiry.
9. Instruction files HARW.md, AGENTS.md and CLAUDE.md are read additively under byte budgets.
10. Cache affinity follows prefix groups within a tenant. `x-session-affinity` is never set on the own-Worker route; the direct Workers AI route may set it from the tenant-scoped prefix group. The gateway contract is versioned. This extends DEC-026.
11. `harw-web` stays a Unix socket with `SO_PEERCRED`. No new loopback-TCP control port, and existing loopback listeners require a token on loopback too.
12. Untrusted config layers cannot set keys that build processes or send telemetry out; hook files are part of the trust digest.
13. Code that encodes another tool's wire details carries a "verified against commit X" marker and a recheck cadence.
14. MCP tools are ordinary tools under the same policy fold, and starting an MCP server is a trust decision made in config.

## Critic corrections (incorporated)

- **S1** applied: §3 control-surface row, §4 ClawJacked, DEC 11, R11 (runner token on loopback).
- **S2** applied: H12, §3 headless row, DEC 4.
- **S3** applied: H17 rated "mixed"; R11 now edits `pairing.rs` and `pairing_store.rs`.
- **S4** applied: H11, §3 bwrap row, §4 CVE-2026-24763 and Claw Chain; TOCTOU pass covers `harw-sandbox` and `harw-tool-shell`.
- **S5** applied: H5, insight 3, R9 (`min(ChildLimits, ledger.available())`).
- **S6** applied: H19, §3 affinity row, DEC 10.
- **S7** applied: R6 includes `trust.rs`, H18, §4 CVE-2026-25593. Not added to R4, because R4 stores in `HARW_STATE_DIR` (DEC-016), not in `.harw/`.
- **B1** applied: R1 per cost-model C1+C2 (`harw-cost` in ring I, `prices.toml`, sidecar), insight 4, H14, D5 (GLM-5.3 Flash only in `providers.toml`). Deviation: F2 moved into R2 so Wave A stays file-disjoint.
- **B2** applied: insight 3, DEC 6, §4 P6 bullet.
- **B3** applied: D1 and H2 (runner `--json`, exit codes, SSE).
- **B4** applied: R5, H2, DEC 4. Minor: the `SdkEvent` derive is at `event.rs:135`, not `:134`.
- **B5** applied: D10 added, DEC 5 replaced.
- **B6** applied with a corrected count: 15 priced Anthropic entries, not 16 (the 16th `ClaudeSpec` match is the struct definition, `:58`; `:72` is the field type).
- **B7** applied: D9 marked as not a real disagreement.
- **C, Wave A overlap** applied: `codex.rs` marker and F2 moved into R2.
- **C, scribe wave** applied: Scribe A/B and per-step scribes in §5.
- **C, R9 path** applied: `harw-runtime/src/budget.rs:268`; also named `effective_cap`'s file (`harw-plan-bridge/src/work_driver.rs:980`).
- **C, R10 deps** applied: DEC-004 and DEC-007.
- **C, R4 deps** applied: DEC-016 and DEC-010 (PL-65:738).
- **C, R6 files** applied: `harw-runtime/src/command_hook.rs` + `assembly.rs` (`SessionLifecycleHook`).
- **D** applied as §2a; MCP caps and elicitation also in H10.
- **E1** applied: D2 cites `gateway.rs:1103` and `:2287`.
- **E2** applied: R11 cites `harw-install/src/doctor.rs:73` and `health_checks`.
- **E3** applied: §3 says example-based law tests.
- **E4** applied: five gates, `gates.rs:156-162`.
- **E5** applied: the gate exempts `union_child`. Its `fn` is at `ir_v2.rs:1106`; `:1086` is the start of its doc comment.
- **Editorial:** replaced the stale opening line, which said this README did not exist and that nothing had been edited.
