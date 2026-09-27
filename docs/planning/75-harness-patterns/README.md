---
id: HP-CATALOG
title: Harness-Muster — Claude Code × Codex × OpenClaw × harw
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

> Opus-Synthese der Rechercheblätter, danach ein gegnerischer Opus-Kritiker.
> Seine Korrekturen sind im Text eingearbeitet (Abschnitt „Kritiker-Korrekturen“
> am Ende). Wo Katalog und Korrektur sich widersprechen, **gilt die Korrektur**.

# Cross-harness pattern catalog: Claude Code, Codex, OpenClaw → harw

Read-only pass. No files were edited, no tests were added and no build was run, so the central build has nothing new to run. Every harw claim below was checked against HEAD. `docs/planning/75-harness-patterns/README.md` is referenced by all three sheets but does not exist; this text is meant to become that file.

## 1. Summary: five insights

**1. The hooks wire format is converging, but harw should adopt the veto semantics, not the wire format.** Codex deliberately copies Claude Code's hook JSON: same camelCase output, same `permission_mode` enum, `CLAUDE_PLUGIN_ROOT`, and a type called `ClaudeHooksEngine`. harw has no hook engine; a grep for `PreToolUse` or `hook_event_name` finds nothing. It does have the stronger core: in `harw-extension-api/src/allow_rules.rs:63` Deny always wins, and a poisoned lock fails closed to Deny (`:803-822`). Claude Code's contract is fail-open: any exit code other than 0 or 2 does not block, and `--bare` skips all hooks. harw should build hooks as an in-process veto fold and speak the Claude Code/Codex wire only through an adapter.

**2. Goal loops are stop gates, and harw's goal loop is already stricter than both.** Claude Code's `/goal` is a prompt-based Stop hook where a small model judges `met`, `not yet met` or `impossible`. Codex's `ext/goal` gives the model an `update_goal` tool. In harw the model can never declare a goal achieved or abandoned (`harw-ops/src/goal.rs:10-20`). The judge verdict is `passed: bool` and fails closed (DEC-001), and judge output is capped at 256 tokens (DEC-002). The missing piece is only a way to use this gate from chat sessions.

**3. Per-run concurrency caps multiply; harw needs one ledger.** Claude Code workflows cap concurrency per run (default 16, depends on CPU count). The gap-hunt kit states the same per-run cap as CPUs − 2 (`85-gap-hunt/kit/skills/gap-hunt/SKILL.md:24-27`), which is why pattern P6 exists. Separate runs multiply the cap, and nested runs share it. harw already has one provider authority (DEC-003: `ProviderRateLimiter`, `pacing_wait` at `harw-core/src/model.rs:647`). But the work driver keeps its own `parallel_ceiling` with `RESERVED_PROVIDER_SLOTS = 1` (`harw-cli/src/job_worker_work_driver.rs:164,374`), and subagents have their own `ChildLimits` (depth 4, 8 per parent; `harw-core/src/child_controller.rs:686-691`). Workflows, subagents and placement must all take leases from a single ledger (DEC-023) instead of adding caps (pattern M2).

**4. Cost must be cache-aware and computed from data.** Claude Code reports `total_cost_usd` and survives resume. Codex reports tokens only. OpenClaw treats the provider's own usage API as the primary source. harw normalizes tokens well: `TokenUsage` has `cache_separate` (`harw-types/src/usage.rs:8-22`) and `UsageRound` records usage per provider and model (`harw-core/src/state_store.rs:287`). But harw computes no dollar amount anywhere. The catalog's `Pricing` type is `f32` and has no cache-write price (`harw-model-catalog/src/descriptor.rs:282-288`). With about 98 % cache reads, the cache-read price decides cost (for GLM-5.3 Flash: $0.03 vs $0.15 input). So prices need cache-read and cache-write fields, in `MicroUsd`, as data. Cache affinity follows the prefix group (`x-harw-cache-affinity`), not the identity.

**5. A single daemon should be the control plane.** OpenClaw's Gateway and Codex's `app-server` both confirm the shape PL-65 is aiming for. harw already has the daemon: `harw-cli/src/gateway.rs` runs Telegram turns, the dream scheduler and the audit-chain scheduler. But the TUI still embeds the runtime (`harw-tui/src/app.rs`, 14,943 lines). The scheduler, the headless stream and hooks should run through that daemon, not through each client.

**Where the sheets disagree with each other or with the code**
- **D1, headless mode.** The Claude Code sheet says `-p` mode is missing. `harw exec` exists (`harw-cli/src/cli/mod.rs:142` → `EntryKind::OneShot`, `harw-cli/src/lib.rs:716`). It prints only the last assistant text (`harw-cli/src/chat.rs:1082`), and the global `--json` flag (`harw-cli/src/cli/global.rs:80-82`) is not passed to it (`harw-cli/src/lib.rs:739-743`).
- **D2, scheduler.** The Claude Code and OpenClaw sheets both say "no scheduler". A dependency-free 5-field `CronSchedule` exists (`harw-knowledge/src/context_steward.rs:811,843,899`). So does a `[dream] schedule` setting (`harw-config/src/harness_config.rs:239-258`), persisted `DreamSchedulerState` (`harw-knowledge/src/dream.rs:533`) and `audit_chain_scheduler` (`harw-cli/src/gateway.rs:52`). What is missing is a general, user-facing scheduler.
- **D3, hooks.** The Claude Code sheet says build `harw-hooks`; the Codex sheet says don't build a hooks engine. Resolved as H7 below.
- **D4, approvals.** The Codex sheet says harw has "no per-session policy". `ApprovalMode{AlwaysAsk, Delegated, FullAccess}` exists (`harw-extension-api/src/approval_mode.rs:51-59`), set by `--approval` (`harw-cli/src/cli/global.rs:102`).
- **D5, pricing.** Placement says `Pricing` is "unpopulated". It is filled for Anthropic (3 entries), Mistral (10) and Z.AI (14) in the `vendor_*.rs` files. The built-in entries have `pricing: None` with a comment saying the authoritative prices live in `harw-provider` (`descriptor.rs:418`). `harw-provider/src` contains no pricing code at all.
- **D6, schema type.** The Codex sheet proposes `serde_json::Value` on `ModelRequest`. Placement §5 bans `serde_json::Value` from its public APIs. A harw-owned `JsonSchema` exists (`harw-tools/src/schema.rs:24`) and should be used.
- **D7, DEC numbers.** Placement says DEC-001 to DEC-019 are "taken". Only DEC-001 to DEC-008 are written. DEC-009 and DEC-010 are proposals (Copilot tasks C-08 and C-10), and DEC-011 to DEC-019 are reserved by PL-65 (`65-cloud-sessions/README.md:758-766`).
- **D8, file location.** `SandboxRequirement` is defined in `harw-job-core/src/spec.rs:249`, not in `harw-job-exec/src/plan.rs` as the Codex sheet says; `plan.rs` only uses it (`:13,62`).
- **D9, workflow cap figure.** The per-run cap is "default 16" in the Claude Code sheet and "CPUs − 2" in pattern P6. Both are per run; the exact figure is unverified.

## 2. Pattern table

| ID | Pattern (problem → solution) | Claude Code | Codex | OpenClaw | harw today | Rec. |
|---|---|---|---|---|---|---|
| H1 | Many clients split state → one daemon owns sessions | none (CLI per session) | `app-server` JSON-RPC daemon | Gateway daemon | **partial**: `harw-cli/src/gateway.rs` (Telegram, dream, audit); TUI embedded (`app.rs`); PL-65 RS3/RS4 | adopt (via PL-65) |
| H2 | Automation needs machine output → typed event stream + terminal result | `stream-json`, `system/init`, `api_retry`, `result{total_cost_usd}` | `exec --json` ThreadEvent; exit code unreliable | typed WS schema | **partial**: `harw exec` text only (`chat.rs:994-1083`); `SdkEvent` `#[non_exhaustive]` (`harwness-sdk/src/event.rs:136`) | adapt: NDJSON = serialized `SdkEvent` + `result` |
| H3 | Free text breaks parsers → schema at the wire | `--json-schema` → `structured_output` | `--output-schema` → `text.format` (caveats #19816, #22998) | – | **missing at wire**: `ModelRequest` (`model.rs:117`) has no schema; post-hoc `ReturnPipeline` (`harw-agent-dsl/src/executable.rs:572`) | adopt, keep post-hoc validator as backstop |
| H4 | Lost/concurrent sessions → durable resume, fork, write fencing | `--resume`, cost restored | rollout jsonl(.zst), resume, fork, `--ephemeral` | per-session lanes, `activeWriterRunId` | **present**: `session resume` (`cli/session.rs:22`), leases with epochs; fork only in matrix runner (`harw-ops/src/matrix/runner.rs:2399`); exec cannot resume (`chat.rs:95`) | keep; chat fork later |
| H5 | Runaway fan-out → spawn limits | 20 concurrent, depth 3 | `spawn_agent`, `[agents]` | composite session keys | **stronger**: `ChildLimits` depth 4, 8 per parent, 15 min lease, only narrows (`child_controller.rs:686-738`) | keep; fan-out from ledger |
| H6 | Big jobs need determinism → scripted workflows with journal/resume | JS `agent/parallel/pipeline`, journal, per-run cap | – | – | **missing**; WorkDriver is a fixed algorithm (`harw-plan-bridge/src/work_driver.rs:980`) | adapt: Rust builder API, cap = ledger |
| H7 | Policy extension points → hook catalog + JSON contract + veto | ~20 events; exit 2 blocks, **other codes fail open** | 12 events, Claude Code wire | `before_tool_call` terminal veto | **missing** generically; deny-wins fold present (`allow_rules.rs:63,803`) | adapt: in-process, veto-only, fail-closed; wire as adapter |
| H8 | Premature stop → goal/stop gate | `/goal` prompt Stop hook | `update_goal` tool + `continuation.md` | – | **stronger**: achieve/abandon command-only (`harw-ops/src/goal.rs:10-20`); `GoalContextProvider` (`goal_context.rs:78`); DEC-001/002 | keep; expose as Stop gate |
| H9 | Proactive work → cron/heartbeat | `CronCreate`, 1 s tick, jitter, 7-day expiry | none | cron + cheap heartbeat | **partial**: `CronSchedule` + dream/audit schedulers (D2) | adopt generic scheduler, reuse `CronSchedule` |
| H10 | Tool bridges bypass policy/typing → MCP with `outputSchema`, one policy | client + `mcp serve`, elicitation, output caps | client only (2025-06-18) | MCP tools use the same policy | **partial**: client and server on 2025-06-18 (`harw-mcp-client/src/lib.rs:17`, `harw-mcp-server/src/lib.rs:15`); `McpTool` has only `input_schema` (`lib.rs:117`); server spawned outside sandbox check (`tool_bridge.rs:21-23`) | adopt `outputSchema` + equivalence test |
| H11 | Isolation → sandbox modes vs requirement levels | permission modes | read-only / workspace-write / danger-full-access; bwrap | tool policy before sandbox; path denylist | **stronger**: `SandboxRequirement` (`spec.rs:249`), `RequirementLevel` (`ir_v2.rs:961`); bwrap withholds `/sys`, `/run` (`executor.rs:48`), never found via PATH (`:78-83`) | keep; mapping table; fix IR vs job-core `Required` (DEC-027) |
| H12 | Who approves → approval policy | 5 permission modes | untrusted / on-request / granular / never | dmPolicy | **present**: session `ApprovalMode` + per-op `approval="always"` (DEC-007); exec rejects every approval, fail-closed (`chat.rs:1045-1063`) | keep |
| H13 | Project context → instruction files | CLAUDE.md | AGENTS.md(+override) | not covered | **present**: HARW/AGENTS/CLAUDE.md additive, 32/128 KiB (`harw-project-discovery/src/discovery.rs:254-260`) | keep; record DEC |
| H14 | Cost is invisible → normalized usage, price data, provider as authority | `total_cost_usd`, `modelUsage`, OTel cost | tokens only | provider usage API primary | **partial**: tokens only (`harw-ops/src/usage.rs`); `Pricing` `f32`, no cache-write (D5) | adopt |
| H15 | 429 storms → pacing + wait budget + visible retries | `system/api_retry` | rate limits absent in exec (#14728) | error taxonomy | **stronger**: header families (`rate_limiter.rs:12-19,350`), budget (`budget.rs:653,714`), 429 wait budget (`retry.rs:21-33`); no retry event | keep; emit events |
| H16 | Bad key or model → classified failover, credential pool | – | OAuth refresh | turn-scoped failover, `model_fallback_decision` | **present**: `should_failover` only on Auth/Quota (`credential_pool.rs:341-344`); `codex_refresh.rs` | keep; add decision event |
| H17 | Device trust → pairing codes, signed identity, revocation | – | – | pairing, ≤3 pending | **stronger**: `PairingCode` 15 min TTL, single use (`harw-channel/src/pairing.rs:41,73`); PQ mTLS; UDS with `SO_PEERCRED` (`harw-web/src/peer.rs:1`); live revocation missing (PL-65 §4.4) | adopt revocation; hash codes |
| H18 | Supply chain → skill/plugin/config-layer trust | plugins (not read in detail) | `[otel]` blocked in project-local config | ClawHub manifest-vs-code check | **present**: BLAKE3 + owner uid, `Changed` → restricted (`harw-home/src/trust.rs:15,158`); skills carry no rights (`harw-agent-dsl/src/skills.rs:21`) | keep; key denylist for untrusted layers |
| H19 | Cold caches, wrong node → placement + prefix-group affinity | – | – | – | **partial**: identity headers opt-in, never `x-session-affinity` (`harw-provider-http/src/lib.rs:3974-3996`; test `:8445-8486`); no engine, no `x-harw-cache-affinity` | adopt P1–P5 + header |
| H20 | Silent misconfig → strict schema, atomic writes | – | project-local denylist | refuse to start on unknown keys | **present**: all 17 `harw-config/src/*_toml.rs` files use `deny_unknown_fields` | keep; add gate |
| H21 | Context overflow → boundary-aware compaction | Pre/PostCompact hooks | Pre/PostCompact | pairs kept together, safeguard retries | **stronger**: `atomic_groups` (`harw-core/src/compaction.rs:20,1041`), deterministic pass (`:505`), 70 %/30 % thresholds (`auto_compact.rs:30-55`) | keep; add compact hook events |
| H22 | Slow turns block transport → async ticket + idempotency key | background agents | – | `runId` + `agent.wait` | **stronger**: BLAKE3 idempotency key, `Duplicate` (`harw-core/src/admission.rs:17,88-92`) | keep; reuse for scheduler |
| H23 | Opaque ops → OTel metrics | cost/token metrics | `codex-otel` | – | **present**: `harw-observe` with otlp, prom and file sinks | keep; add cost metric |

## 3. harw strengths to keep

| Strength | Invariant | How to protect it |
|---|---|---|
| Rights only ever narrow: `AuthorityCeiling::intersect` (`harw-agent-dsl/src/authority.rs:102`), `ContextCeiling::intersect` (`harw-context/src/ceiling.rs:233`), `with_max_children`, `narrow_spec` (DEC-007) | no `union`, `widen` or merge that grows a ceiling | keep the property tests (`ceiling.rs:375-403`); new xtask gate forbidding `pub fn (union\|widen)` in these modules; `PlacementGrant::meet` gets the same laws (P1) |
| Deny wins, fail-closed (`allow_rules.rs:63,803-822`) | hooks and MCP go through this same fold; a hook can never un-deny | hook tests plus an MCP ≡ native test (R3, R6) |
| The model can never finish a goal (`harw-ops/src/goal.rs:10-20`); judge fails closed (DEC-001) | a stop gate may only block stopping | test at `CallSurface::Model`; stop-gate enum without an "Achieved" variant |
| One provider authority (DEC-003) | exactly one limiter per provider per process; the ledger wraps it (DEC-023) | cross-layer invariant test suite (M2) |
| Local control surface is a Unix socket with `SO_PEERCRED` (`peer.rs`, `harw-web/src/server.rs:543`) | never a loopback TCP control port (protects against ClawJacked) | DEC + review rule; test that the web listener is a `UnixListener` |
| bwrap safety (`executor.rs:48,78-83`) | bwrap never found via PATH; `/sys` and `/run` withheld | named TOCTOU gap-hunt pass over the sandbox and job crates |
| Headless mode rejects approvals (`chat.rs:1045-1063`) | exec never approves implicitly | keep when adding `--output-format` |
| Never `x-session-affinity` (`lib.rs:8486`) | affinity comes from prefix groups within one tenant | extend the test when adding `x-harw-cache-affinity` |
| Strict config (`deny_unknown_fields`) | no unknown key accepted | xtask gate that scans `*_toml.rs` (none exists today; gates are only edges, privileges, arch and warden, `xtask/src/gates.rs:156-173`) |

## 4. Anti-patterns and incidents, and how harw avoids them

- **OpenClaw CVE-2026-25253** (the UI auto-connected to a `gatewayUrl` from the query string): harw has no browser client that follows URLs, and PL-65 §4.2 pins the host fingerprint in the pairing string.
- **OpenClaw CVE-2026-25593** (unauthenticated `config.apply` set `cliPath`, leading to command injection): any hook command or process path must come only from trusted layers (`trust.rs`), never from a runtime RPC. Needs a DEC.
- **OpenClaw CVE-2026-24763 / CVE-2026-25475** (sandbox escape via PATH; model output used as a file path): bwrap discovery already ignores PATH. Add "tool turns model output into a path" to the M1/M5 checklist for `harw-tool-fs`, `harw-tool-doc` and `harw-tool-browser`.
- **OpenClaw ClawJacked** (loopback treated as trusted): harw's control surface is a Unix socket (see §3).
- **OpenClaw Claw Chain** (a TOCTOU chain across sandbox mounts): run a recurring named M1 pass over the sandbox crates.
- **Claude Code: hooks fail open** (exit codes other than 0/2 don't block) and `--bare` skips hooks: in harw a hook error or timeout on a Pre-event means Deny, and no flag disables deny rules.
- **Claude Code / P6: per-run caps multiply**: harw uses a ledger lease instead (R9, R10).
- **Codex: exit codes are unreliable** (#4721, #41984): harw exec should use typed exit codes plus a mandatory terminal `result` event.
- **Codex: rate limits are `null` in exec** (#14728): configured caps must stay the control that carries the load (DEC-003).
- **Codex: `--output-schema` gaps** (#19816, #22998): keep the post-hoc `ReturnPipeline` validator.
- **Codex: near-daily wire drift and ToS exposure** (`originator` in `harw-provider-http/src/codex.rs:34-81`): mark wire code with "verified against commit X".
- **Two conflicting caps across layers** (M2): `RESERVED_PROVIDER_SLOTS` plus `ChildLimits` plus a future workflow cap would repeat this. The ledger removes it.

## 5. Consolidated roadmap

Rings follow `xtask/arch-policy.toml`. Rules: one agent per file, and a central build after each round.

| Round | Content | Files and crates (ring) | Patterns | Depends on |
|---|---|---|---|---|
| R0 | DECs (§6), this catalog as README, placement P0 | `docs/planning/70-decisions/*`, `docs/planning/75-harness-patterns/README.md` | all | – |
| R1 | Cost state (merges the cost items of the Claude Code, OpenClaw and placement docs): `MicroUsd` in `harw-types` (F), because the cost state in `harw-core` (A) and placement (I) both need it, rather than in `harw-placement-model` as placement §5 sketches; `Pricing` gets cache-read and cache-write in `MicroUsd`, including GLM-5.3 Flash data; `CostSummary` survives resume | `harw-types/src/cost.rs` (new), `harw-model-catalog/src/descriptor.rs` + `vendor_zai.rs`, `harw-core/src/state_store.rs`, `harw-session-store/src/meta.rs`, `harw-ops/src/usage.rs` | H14, H23 | R0 (DEC-025) |
| R2 | Schema at the wire: `ModelRequest.output_schema: Option<JsonSchema>` (harw-owned type), mapped per adapter; the judge uses it | `harw-core/src/model.rs`, `harw-provider-http/src/{lib,codex,anthropic}.rs`, `job_worker_work_driver.rs` | H3 | – |
| R3 | MCP `outputSchema`/`structuredContent`; test that an MCP tool is treated like a native tool under `allow_rules` | `harw-mcp-client/src/{lib,tool_bridge}.rs`, `harw-mcp-server/src/session.rs` | H10 | – |
| R4 | General scheduler (merges the Claude Code and OpenClaw items): schedule create/list/delete, a daemon tick, reuse of `CronSchedule`, each fire goes through `JobAdmissionService` with key `schedule:<id>:<fire_ts>`, `write_atomic` store, jitter, expiry | `harw-ops/src/schedule.rs` (new), `harw-cli/src/gateway/scheduler.rs` (new), `gateway.rs` wiring (A) | H9, H22 | – |
| R5 | `harw exec --output-format text\|json\|stream-json`, `result` with cost, typed exit codes, additive `SdkEvent::Retry` | `harw-cli/src/{chat.rs,cli/global.rs,headless.rs}`, `harwness-sdk/src/event.rs` | H2, H15 | R1 (R2 optional) |
| R6 | Hooks, veto-only: vocabulary and trait in `harw-extension-api/src/hooks.rs` (I); `CommandHook` speaking the Claude Code/Codex wire in `harw-runtime` (A), sandboxed; turn-loop wiring; judge as Stop gate with a bounded number of continuations | `harw-extension-api/src/hooks.rs`, `harw-config/src/hooks_toml.rs`, `harw-core/src/turn_loop.rs`, `harw-plan-bridge/src/goal_gate.rs` | H7, H8, H21 | R0, R2 |
| R7 | Placement P1+P2 | new `harw-placement-model` (I): `requirements.rs`, `offer.rs`, `decision.rs`, `filter.rs`, `score.rs`; arch-policy entry | H19, H14 | R0, R1 |
| R8 | Placement P3: offers and a ledger over the limiter; optional gateway response headers `x-harw-lane`, `x-harw-affinity` and capacity signals | `harw-job-runtime/src/offer.rs` (J→I), `harw-model-catalog/src/offer.rs`, `harw-provider-http/src/{ledger,rate_limiter}.rs`, `harw-netsec/src/offer.rs` | H15, H19 | R7 |
| R9 | Placement P4+P5 plus `x-harw-cache-affinity` (prefix group on `RequestIdentity`); `parallel_ceiling`, `effective_cap` and `ChildLimits` fan-out read `ledger.available()`. P6 (DSL keys) can run in parallel | new `harw-placement` (A), `harw-agent-runner/src/admission.rs`, `job_worker_work_driver.rs`, `verify_sandbox.rs`, `harw-core/src/model.rs`, `harw-provider-http/src/lib.rs`; P6: `harw-agent-compiler/src/passes/placement.rs` | H5, H19 | R8 |
| R10 | `harw-workflow` (A): builder API (`agent`, `parallel`, `pipeline`, `phase`, `log`), schema retries, journal in `harw-job-store`, resume from the journal, concurrency through ledger leases; port the gap-hunt kit | new crate, `docs/planning/85-gap-hunt/kit/*` | H6 | R2, R9 (R5 for events) |
| R11 | Security and ops slices: `DoctorCheck` self-audit (`harw-cli/src/lifecycle.rs:71`), fallback-decision event (`credential_pool.rs` + `harw-observe`), hashed pairing codes (`pairing.rs`), denylist of config keys for untrusted layers, commit markers in `codex.rs`; live revocation after PL-65 RS2 | disjoint single files | H16, H17, H18, H20 | – (revocation: RS2) |

Placement P7 (remote `HostFacts`) stays tied to PL-65 RS2/RS3.

**Safe to parallelize**
- **Wave A:** R1, R2, R3, R4 and R11. Their files are disjoint; R1 and R2 are in the same crate `harw-core` but touch different files.
- **Wave B:** R5, R6 and R7.
- **Serial:** R8 → R9 → R10. R2 and R9 both touch `harw-provider-http/src/lib.rs` and `harw-core/src/model.rs`, so they never run in the same wave.

## 6. Decisions to record

Proposed titles without final numbers. They would start at DEC-028 or later, and the container design may claim numbers too. Status today: DEC-009 and DEC-010 are proposals, DEC-011 to DEC-019 are reserved by PL-65 and DEC-020 to DEC-027 by placement.

1. Hooks are veto-only and fail closed. The Claude Code/Codex wire is an adapter, not the core.
2. Stop gates can only block stopping. Achieve and abandon stay command-only, and continuations are bounded.
3. Structured output at the wire uses a harw-owned schema type, with the post-hoc validator kept as a backstop.
4. The headless contract: NDJSON = serialized `SdkEvent` plus a terminal `result`, typed exit codes, and approvals are always rejected.
5. Cost authority: prices are data in `MicroUsd` with cache-read and cache-write fields, provider-reported usage comes first, and the cost state survives resume. This extends DEC-025.
6. No per-run concurrency caps. All fan-out (workflows, subagents, schedules) takes ledger leases. This extends DEC-003 and DEC-023 and closes P6.
7. The workflow engine is a Rust builder API first. No embedded script engine appears in public types, and the journal lives in the job store.
8. One daemon scheduler: it reuses `CronSchedule`, fires through admission idempotency, and has jitter and expiry.
9. Instruction files HARW.md, AGENTS.md and CLAUDE.md are read additively under byte budgets.
10. Cache affinity follows prefix groups within a tenant. harw never sets `x-session-affinity`, and the gateway contract is versioned. This extends DEC-026.
11. The local control surface stays a Unix socket with `SO_PEERCRED`, never loopback TCP.
12. Untrusted config layers cannot set keys that build processes or send telemetry out.
13. Code that encodes another tool's wire details carries a "verified against commit X" marker and a recheck cadence.
14. MCP tools are ordinary tools under the same policy fold, and starting an MCP server is a trust decision made in config.