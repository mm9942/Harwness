# PL-96 / 01 — Counterchecked evidence register

**Read-only audit date:** 2026-10-09. **CURRENT** = tracked source on dev commit `090a9d2b786a3a6c025182d8da9c7b28bb7cbece`; **RELEASE** = main commit `90160f4f2b88f80c6721c20469ed8322ddc8861a`; **BRANCH** means an explicitly named *unmerged* source, not deployed behavior. Use the permanent `blob/<SHA>` links below instead of a moving `dev` link for reproducibility.

## Evidence rules and falsification policy

**Observed (O):** source, parsed file content, tracked tree or PR metadata inspected at a pinned SHA. **Corroborated (C):** a source fact also named in an implemented-plan/test or independent PR review. **Hypothesis (H):** likely issue requiring a reproduction; do not present as an exploit or verified operational failure. **Target (T):** recommendation, not code.

A source-level `#[test]` is evidence that a test is *defined*, not that it passed on the pinned commit. A historical PR saying tests were green on a worktree is not a successful fresh checkout. CI conclusions apply only to the head SHA and job matrix that actually ran. “Merged” says nothing about the completeness of a feature. “Open” says nothing about whether another PR later implemented the same behavior.

### E01 — Main release lockfile contains unresolved merge-conflict blocks (O, P0)

- **Positive evidence:** direct inspection of `main/Cargo.lock` found 129 `<<<<<<<` + 129 `=======` + 129 `>>>>>>` markers (129 full conflict blocks). `dev/Cargo.lock` has zero such markers. The first conflict is a `version = "0.9.0"` versus `"0.9.1"` change.
- **Countercheck:** [PR #132](https://github.com/mm9942/Harwness/pull/132) is merged to **dev**, not **main**. A passing older main CI is not evidence that `main/Cargo.lock` parses.
- **Proof requirement:** clean checkout at each branch HEAD; reject any marker and run `cargo metadata --locked --no-deps`; independently stage a minimal main hotfix. No blind regenerate without package and dependency diff.

### E02 — Five referenced cloud members are absent from dev's tracked tree (O, P0)

- **Evidence:** [root Cargo.toml L137-L141](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/Cargo.toml#L137-L141) declares `harw-cloud/harw-cloudctl`, `harw-cloud/harw-cloud-enroll`, `harw-cloud/harw-cloud-gateway`, `harw-cloud/harw-cloud-ops` and `harw-cloud`. The tracked recursive tree has **no harw-cloud/ path**.
- **Countercheck:** [PR #136](https://github.com/mm9942/Harwness/pull/136) explicitly describes untracked stub manifests used during verification. Therefore a claimed green stub-assisted build does **not** prove a clean Git checkout builds.
- **Competing explanations:** accidental missing commit, deliberately private component, or stale members; all must be resolved **without inventing local stubs**. Decision: commit legitimate packages or remove unavailable members with an explicit dependency and consumer audit. Re-run gates from a clean tree.

### E03 — DoD is already in the root Cargo workspace (O/C)

- **Evidence:** [root manifest L156-L200](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/Cargo.toml#L156-L200) lists DoD crates; [PL-60 status](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/docs/planning/50-dod-integration/README.md) and [workspace-merge record](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/docs/architecture/dod-workspace-merge-plan.md) describe the completed migration.
- **Countercheck:** older ecosystem plans and planning snapshots predating PL-60 may still state separate lockfiles. Treat that as history, not CURRENT.
- **Retained invariant:** same workspace/lockfile does not mean the Warden can depend on model providers, browser, runtime composition or arbitrary job crates. Verify `xtask gates warden-deps`, `warden-cbuild`, `arch` and package-scoped builds.

### E04 — PL-93 42 tool implementations lack effective model registration (O/C, P1)

- **Evidence:** [PL-93 CURRENT and §8](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/docs/planning/93-common-command-tools/README.md#L117-L133) describes four source crates with 16 fsread, 12 sys, 8 cargo and 6 gitread tools. [PR #127](https://github.com/mm9942/Harwness/pull/127) is merged but explicitly says no agent sees them yet.
- **Countercheck:** [profile.rs](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-registry-defaults/src/profile.rs) *does* have an existing CargoToolProvider; do not mistake it for registration of these **new** families. Check exact `TOOL_NAMES` and executor identities rather than crate-name similarity.
- **Tests to add:** real mounted profile and model ToolSpec list; allowed/denied runtime invocations; bounded output; truncation; sandboxed Cargo; no unmanaged spawn; worktree path isolation; no hidden shell fallback.
- **Limit:** 42 is an implementation count, **not** an accessible-tools count.

### E05 — Triggered-skill selector exists but injection is not wired (O/C, P1)

- **Evidence:** [skill_select.rs L1-L36](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-catalog/src/skill_select.rs#L1-L36), [Injection::into_fragment L149-L181](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-catalog/src/skill_select.rs#L149-L181), [PL-95 stages S1–S4](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/docs/planning/95-cli-tools-skills/README.md#L151-L165), [PR #126](https://github.com/mm9942/Harwness/pull/126).
- **Countercheck:** pure selector tests do not prove an active TurnLoop reads user/tool/command events and injects fragments. No model-call context consumer was confirmed by this audit.
- **Tests:** synthetic user task/tool error/first tool use, exactly once within bounded session; stable prefix unchanged; triggered content arrives only on next admissible model turn; compaction resets dedupe only for dropped material; untrusted skill cannot elevate authority.

### E06 — `ModelRequest::data_block` is lost on Anthropic Messages (O, P0)

- **Evidence:** [Anthropic build_messages_body L564-L652](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-provider-http/src/anthropic.rs#L564-L652) assembles system/history/tools but does not read `data_block`; [OpenAI Responses L2893-L2900](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-provider-http/src/lib.rs#L2893-L2900) and [Chat L3704-L3706](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-provider-http/src/lib.rs#L3704-L3706) append it as a user item.
- **Countercheck:** a provider supporting system instructions does not guarantee the separate data block is serialized. [Control-effectiveness F2](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/docs/planning/76-control-effectiveness/README.md) independently records this gap.
- **Impact:** requested selected context can differ across providers despite identical internal request. **Do not claim an observed production leak**; this is a directly demonstrated serialization omission.
- **Tests:** wire-level golden fixtures for present/absent/empty data blocks with images, tool calls and cache markers across all supported schemas. No accidental doubling or trust-class promotion.

### E07 — Some context compilation/omissions remain insufficiently end-to-end proven (O/H)

- **Evidence:** [turn_loop.rs documented fallback/ContextProvider paths](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-core/src/turn_loop.rs), [context_budget.rs](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-core/src/context_budget.rs), [F2 findings](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/docs/planning/76-control-effectiveness/README.md).
- **Countercheck:** typestate assembly and cost budgets exist, but the correctness question is whether *actual* turn requests include intended admitted fragments under each context ceiling and provider. Potential loss on empty/default programs requires executable reproduction.
- **Tests:** matrix of Closed/LocalRoot/other relevant ceilings, program absent/present, instruction vs data, must-include, compaction, omission reason, 32k unknown-model fallback. Measure before/after prompt token counts and quality.

### E08 — Current local SessionHost is real; remote ToolHost remains disabled in UDS composition (O, P1)

- **Evidence:** [session_serve.rs](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-cli/src/session_serve.rs) mounts a production `CoreTurnDriver` and durable transcripts/approvals. [session-daemon compose.rs L69-L85](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-session-daemon/src/compose.rs#L69-L85) calls `SessionHost::open`; [host.rs L179-L202](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-session-host/src/host.rs#L179-L202) defaults to `ToolHost::disabled()`; [disabled implementation](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-session-host/src/tool_host.rs#L454-L468) has no gateway sandbox/tools.
- **Countercheck:** `PortOffer::All` in [server.rs](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-session-daemon/src/server.rs#L225-L250) exposes method tables, not execution capability. [PR #91](https://github.com/mm9942/Harwness/pull/91) describes a stale no-op driver; CURRENT is more advanced. `tool.call` must fail closed absent explicit wiring.
- **Tests:** authenticated UDS identity, wrong UID denial, no-tool refusal, authorized ToolHost with scoped grant, changed/revoked device denial and approval binding.

### E09 — Local and remote ingress do not share an owner/session store (O, P1)

- **Evidence:** [session_serve_remote.rs L1-L32](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-cli/src/session_serve_remote.rs#L1-L32) explicitly states separate hosts, state directories and default Observer remote tier. Local UID and device principals differ; a remote approval device must opt in.
- **Countercheck:** “two authenticated ingresses” is not “shared Cloud Home”. Conversely, a shared storage directory alone is not a safe same-session design.
- **Tests:** one writer lease per SessionId, two device reads, per-principal caps, recovery under expired fencing token, revocation, reconnect/dedup, no remote authority inflation.

### E10 — Unmerged Telegram `ChannelReduced` policy matches only first token (O for BRANCH, P0 before integration)

- **Evidence:** branch `feature/telegram-bot-api-10-3-command-registry` (head beginning `91db7b5904`), [CommandAdapter::allows_channel_invocation](https://github.com/mm9942/Harwness/blob/feature/telegram-bot-api-10-3-command-registry/harw-operations/src/adapter/command.rs#L231-L249) checks first token or `-`. [dream.rs](https://github.com/mm9942/Harwness/blob/feature/telegram-bot-api-10-3-command-registry/harw-ops/src/dream.rs#L132-L179) allows `review`; [run_review_as](https://github.com/mm9942/Harwness/blob/feature/telegram-bot-api-10-3-command-registry/harw-ops/src/dream.rs#L710-L773) interprets nested `accept` and performs a write.
- **Countercheck:** [op_bridge.rs L184-L290](https://github.com/mm9942/Harwness/blob/feature/telegram-bot-api-10-3-command-registry/harw-cli/src/op_bridge.rs#L184-L290) still checks an admitted Human Principal, minimum Operation permission and runtime sandbox. Therefore this is a **policy-expression gap**, not proven privilege escalation.
- **Negative test:** `/dream review <id> accept <proposal>` must be rejected by a read-only channel grant even when the sender has Operator tier. Test aliases, `/op`, nested verbs, safe `review <id>`, parser normalization and authorization on both discover and dispatch. No generic “all operations via Telegram” switch.

### E11 — Built-in agent discovery and spawnability differ (O, P1)

- **Evidence:** [role_names::ALL rationale L95-L152](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-registry-defaults/src/profile.rs#L95-L152) lists deliberately pending discovered roles; [AgentRoster](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-registry-defaults/src/roster.rs) clamps custom definitions; [roles.rs](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-agent-dsl/src/roles.rs) seals six authority roles; [delegation_visibility.rs](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-core/src/delegation_visibility.rs) filters parent-to-child targets.
- **Countercheck:** the tree has 33 home agent definition/agent TOMLs and 52 registry agents TOML files, but they overlap and include base/family/context-program files. They are **not** 85 runnable workers.
- **Tests:** same list for effective spawner admission and UI listing; reason codes for unavailable role, missing parent grant, forbidden tool, policy clamp or model missing. Custom role must never bypass six-role spawn matrix.

### E12 — Warden v2 signed proof exists, but privileged crate still uses v1 path (O, P0)

- **Evidence:** [SignedAuthorization v2 protocol](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/dod/crates/harw-dod-warden-proto/src/signed.rs#L1-L32) supports keyring, lifetime and replay nonce; [v1 proof warning](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/dod/crates/harw-dod-warden-proto/src/proof.rs#L1-L21) says no MAC/nonce/expiry; [Warden::handle](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/dod/crates/harw-dod-warden/src/warden.rs#L230-L249) calls `request.proof.verify` on the v1 request. The v2 documentation itself says binary migration is pending.
- **Countercheck:** presence of secure v2 *types* does not mean the privileged action path enforces v2. This is a source-proven migration gap; whether a vulnerable network path can reach it in a given deployment is **not** claimed here.
- **Acceptance:** reject all v1 requests, verify signature/version/issuer/key purpose/action digest/stage/cgroup/time/nonce before action, durable replay ledger, atomic key rotation, audit-before-execute, malformed-frame fuzz, package-scoped Warden gates, disposable integration tests. Do not deploy privileged enforcement with v1 as production authorization.

### E13 — Project trust has documented TOCTOU and update races (O/H)

- **Evidence:** [trust.rs L1-L55](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-home/src/trust.rs#L1-L55), [restricted config discovery](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-config/src/discovery.rs#L598-L632).
- **Countercheck:** digest, symlink-free read, UID and conservative untrusted merge are **already implemented**. A race warning does not establish real-world exploitability.
- **Next check:** capture/parse exactly verified bytes, protect trust-store writes from lost updates, test file exchange between check/use, verify path root/UID changes and denial of untrusted provider/MCP/approval broadening.

### E14 — Open PR reconciliation cannot be based on titles (O, P1)

- **Evidence:** [#91](https://github.com/mm9942/Harwness/pull/91) is partly superseded by dev composition; [#89](https://github.com/mm9942/Harwness/pull/89) index remains unmerged and is catalog-only; [#90](https://github.com/mm9942/Harwness/pull/90) deployment profile remains unmounted; [#142](https://github.com/mm9942/Harwness/pull/142) has five changed files but promises further salvage. [#95](https://github.com/mm9942/Harwness/pull/95) touches 433 files while claiming provider-lifecycle docs.
- **Countercheck:** an open PR can have a later implementation already on dev; a closed/merged PR may be partial. Compare the exact head and base merge ancestor; classify exclusive hunks, not just commits or filenames.
- **Tests:** one merge/close decision per PR with preservation evidence and branch ref; do not delete branches until a responsible reviewer signs the rescue ledger.

### E15 — Async-first job-backed delegation landed with unresolved verification claims (O/C)

- **Evidence:** [PR #131](https://github.com/mm9942/Harwness/pull/131) is merged into dev, describing durable WorkIds, child jobs, lease/fencing and fail-closed handoffs. Its description also says “Not merge-ready yet”, requests a green exact SHA and a crash/restart test. [PR #141](https://github.com/mm9942/Harwness/pull/141) has three portability follow-ups; latest CI on its head was failing when inspected.
- **Countercheck:** no claim that the merged code is definitely broken; no claim the stated historical CI passed after merge. Re-run exact-head gates, native agent e2e, crash recovery, nextest/macOS and no-bypass job-spawn tests.

### E16 — Cloudflare adaptive lanes and caching must not be conflated (O/C)

- **Evidence:** [PR #138](https://github.com/mm9942/Harwness/pull/138) adds bounded model-lane policy/route-local failover but explicitly **does not deploy** it; [cache_strategy.rs](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-provider-http/src/cache_strategy.rs) decides prompt prefix markers for provider requests; [PR #129](https://github.com/mm9942/Harwness/pull/129) concerns the mias-lab Worker cache and concurrency policy.
- **Countercheck:** provider prefix caching, exact-response caching, admission concurrency and gateway failover have different correctness keys and metrics. A successful Worker unit test is not proof of deployed savings or safe cross-tenant reuse.
- **Tests:** exact-response eligibility, affinity separation, cache-key version/policy/tenant isolation, streaming bypass, 429 handling, shared-capacity no fallback storm, repeated prompt-prefix hit rate and cost per verified task.

### E17 — Pattern projection and tool-expert ML remain research, not operational guarantees (O/C)

- **Evidence:** [PL-71 semantic patterns](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/docs/planning/71-semantic-activity-patterns/README.md) says planning-only; [PR #142](https://github.com/mm9942/Harwness/pull/142) rescues a pure AgentTree model without the runtime producer. [PR #139](https://github.com/mm9942/Harwness/pull/139) is an untrained 12-micro-expert / Qwen tool-student scaffolding project, not a deployed trained model.
- **Countercheck:** existing TUI `ToolCell` / `AgentMonitor` and raw events are real. Neither semantic activity graph nor weights/behavior wins follow from plans.
- **Tests:** raw-event provenance and replay, projection revisioning, independent failure identities, bounded UI output, holdout leakage and training/evaluation artifacts if ML is later pursued.

### E18 — Live-session knowledge checkpointing is missing by design, not “no memory” (O/C)

- **Evidence:** [PL-90 06](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/docs/planning/90-intent-driven-composed-agents/06-live-session-dream-diary-learning-wiring.md) labels itself planning-only and traces Diary on compaction/close, Gateway idle/cron Dream, manual /learn, optional extraction and startup sweep. [memory_wiring.rs](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-runtime/src/memory_wiring.rs#L1-L35) has real capture/flush/recovery seams.
- **Countercheck:** long-term memory and indexes exist. The narrower claim is that a continuously active conversation can miss *automatic timely* proposed lessons/checkpoints and global promotion. Indexing alone does not grant another agent access to private content.
- **Tests:** sustained busy TUI/Telegram session, committed milestone events, idempotent scoped checkpointing, proposal-only outcome, operator acceptance, search scope/isolation, conflict/expiry handling, no background provider fallback to wider rights.

### E19 — Research paper is methodologically cautious, but baseline differs from dev (O)

- **Evidence:** [paper intro/method L1-L150](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/docs/research/harwness-paper.md#L1-L150) pins main at `90160f4`; [experimental hypotheses](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/docs/research/harwness-paper.md#L935-L956); [status matrix/limitations](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/docs/research/harwness-paper.md#L1265-L1322) acknowledges no published benchmark wins.
- **Countercheck:** replacing its pinned baseline with “latest dev” without validated gates would **reduce** reproducibility. Keep original claims scoped, add an explicit dev delta and experiment manifests.
- **Required evidence:** verified code/release SHA, exact model IDs/digests/config, test corpus/seed, independent reviewer, hidden holdout, results with confidence intervals and known failure modes.

### E20 — Planned capabilities are not permission grants (invariant)

- **Evidence:** [authority.rs](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-registry-defaults/src/authority.rs) maps tool permission and monotone reducers; [ApprovalChain](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-runtime/src/approval.rs) combines explicit policies; [Role spawn matrix](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-agent-dsl/src/roles.rs#L76-L125); [SessionHost tool gateway](https://github.com/mm9942/Harwness/blob/090a9d2b786a3a6c025182d8da9c7b28bb7cbece/harw-session-host/src/tool_host.rs).
- **Crosscheck invariant:** `FullAccess` changes human confirmation behavior, **not** filesystem/network sandbox ceilings. Approval never increases an authority envelope, agent role declarations may not introduce new sealed roles, and a tools index may not mint privileges.
- **Tests:** property-based permission subset checks over every entry/child/delegation, remote channel, role and model skill injection; direct invocation bypass attempts must fail even if advertised menus or prompts are forged.

## Evidence review questions

1. Is the source pinned? Does a newer producer or consumer supersede it?
2. Is the fact about existence, composition, admission, execution, persisted outcome, or deployment?
3. Could tests/docs refer to a different branch or untracked file?
4. What alternative explanation would falsify the alleged gap?
5. What negative test would make an accidental privilege widening observable?
6. What would explicitly block a merge/release and who can approve an exception?

**Ledger status:** evidence collected from connected GitHub, no local runtime test or exploit validation performed. Findings requiring reproductions remain open until a separately reviewed implementation PR supplies exact-run evidence.
