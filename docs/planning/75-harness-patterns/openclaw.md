---
id: HP-OC
title: Rechercheblatt — OpenClaw (Muster)
status: research
date: 2026-09-27
tags: [harness-patterns, research, openclaw, security, gateway]
related:
  - README.md
  - claude-code.md
  - gateway-contract.md
  - ../65-cloud-sessions/README.md
  - ../85-gap-hunt/patterns.md
---

> Research workflow `openclaw-harw-research`, read-only:
> - 3 Sonnet researchers with 63 sourced facts; sources are the official
>   repo `github.com/openclaw/openclaw` (commit of 2026-09-27), docs.openclaw.ai,
>   as well as advisories and press.
> - Then a pattern extractor that cross-checked against the harw code.
>
> Figures on reach and volume (stars, CVE count, exposed instances)
> are not verified and are not used anywhere as evidence.

# OpenClaw → harw: Pattern Catalog

*Read-only research. All harw paths below were confirmed by reading the repository at the current HEAD (2026-09-27). OpenClaw claims are taken from the pre-gathered research handed to me in the task (docs.openclaw.ai, github.com/openclaw/openclaw, CVE/advisory write-ups); I did not re-fetch those URLs myself, so where the upstream researcher marked a claim "likely"/"unconfirmed" I carry that label forward.*

## A. What OpenClaw is

OpenClaw is an open-source, self-hosted "personal AI assistant" / agent gateway (TypeScript/Node.js core, two thin Rust crates) that runs one long-lived local **Gateway** daemon bridging one agent identity to 20–29+ chat channels (WhatsApp, Telegram, Slack, Discord, Signal, iMessage, Matrix, …) plus native apps and a web/TUI/CLI Control UI. Released Nov 2025 by Peter Steinberger under an earlier name, renamed twice (trademark pressure, then a critical RCE disclosure) before settling on "OpenClaw" on 2026-01-30; now stewarded by the independent OpenClaw Foundation (501(c)(3)), MIT-licensed. Repo: `github.com/openclaw/openclaw`; docs: `docs.openclaw.ai`. Sources for identity/history: docs.openclaw.ai/concepts/architecture, CNBC 2026-02-02, Forbes 2026-02-06 — version and exact rename dates vary slightly between the three source blocks handed to me, so treat exact dates as press-corroborated but not primary-verified.

## B. Pattern catalog (18 patterns)

For each: **Problem → OpenClaw solution (source) → Trade-offs → harw today → Recommendation.**

**1. Singleton daemon / trusted control plane**
Problem: N clients (CLI, TUI, web, mobile) each owning session/auth state duplicates logic and creates split-brain state.
OpenClaw: one Gateway daemon owns every channel connection and session; everything else is a thin client of it (docs/gateway/index.md).
Trade-offs: single point of failure/scaling; but one place to secure and audit.
harw today: **Present, partial.** `harw-cli/src/gateway.rs` runs a persistent daemon that already executes Telegram turns; but `harw-tui` still **embeds** the runtime in-process (`docs/planning/65-cloud-sessions/README.md` §0: `app.rs` drives `run_turn_streaming` directly). The daemon-as-single-owner model exists for one channel, not yet for the TUI.
Recommendation: **Adopt** — this is exactly the direction `docs/planning/65-cloud-sessions/README.md` (Remote Sessions, program RS) is already driving; treat OpenClaw as independent confirmation the target shape is right.

**2. Schema-typed, single-transport, role-differentiated protocol**
Problem: N client types tempt N bespoke protocols.
OpenClaw: one WebSocket, one TypeBox-generated schema, clients differ only by a declared `role` at handshake (docs/gateway/protocol.md).
Trade-offs: versioning discipline required (major/minor negotiation) or old clients break.
harw today: **Present.** `harw-protocol` already has typed `RequestEnvelope`/`ResponseEnvelope`/`NotificationEnvelope` with `ProtocolVersion{major,minor}` rejecting any other major; `harw-node-transport/src/wire.rs` + `handshake.rs` do role/capability declaration at connect. The session-specific extension (`session_wire.rs`, `session_port.rs`) is speced in PL-65 §3 but not yet built.
Recommendation: **Adopt** (already in motion via PL-65). Reuse OpenClaw's explicit `#[serde(other)]`-style unknown-variant fallback discipline for `SessionFrame` — PL-65 already flags this as **(verify)**.

**3. Optimistic-accept async job-ticket + idempotency key**
Problem: a slow/streaming turn must not block the transport, and a disconnect must not force a replay.
OpenClaw: `agent` RPC returns `{runId, acceptedAt}` immediately; a separate `agent.wait` polls the terminal outcome (docs/concepts/agent-loop.md).
Trade-offs: caller must track/re-wait on `runId`; a timed-out wait doesn't cancel the run.
harw today: **Present, and arguably stronger.** `harw-core/src/admission.rs`'s `JobAdmissionService::submit` derives a BLAKE3 idempotency key from `(tenant, workspace, submitter, key)`; a repeat returns `Duplicate` rather than re-running; `submit_queued_async` decouples admission from a capacity wait via `tokio::sync::Notify`.
Recommendation: **Adopt/reuse pattern**, no new work — already implemented for durable jobs; extend to `session.submit` in PL-65 (`SubmitParams.client_msg_id`, already specced §2.4).

**4. Per-session serialization + durable write fencing**
Problem: two concurrent runs on the same conversation, or a superseded run finishing late, must not corrupt the transcript.
OpenClaw: per-session lanes plus a durable `activeWriterRunId` claim checked at every transcript commit (docs/concepts/agent-loop.md; verified in OpenClaw source by the upstream researcher).
harw today: **Present.** `harw-session-store/src/durability.rs`, `freeze.rs`, `child_lease.rs` implement fenced leases with epochs; `harw-job-runtime`'s coordinator restart-reattaches a running job by lease epoch (`coordinator/tests.rs::restart_reattaches_the_running_job_and_finishes_it`, per PL-65 §0).
Recommendation: **Adopt pattern as-is** — already present; PL-65's `Cursor{generation, durable, live}` (§3.2) is the direct session-level extension.

**5. Composite session-key routing + admission gate**
Problem: routing an inbound event to a conversation, and deciding whether to trust it, are two different concerns that get conflated.
OpenClaw: `agent:<agentId>:<sessionKey>` composite key; per-source-type default isolation (DM shared, group isolated, cron fresh-per-run) (docs/concepts/session.md, docs/concepts/multi-agent.md).
harw today: **Present, close match.** `harw-channel/src/ids.rs::SessionKey{tenant, channel, peer, thread}` is exactly this composite address; `harw-channel/src/dispatch.rs::dispatch_inbound` runs `derive_session_key` then `admit()` → `Admitted | Rejected | Deferred`, and rejected/deferred events **never touch session-store state** — stronger than a config table alone, since it's a typed pipeline result.
Recommendation: **Adopt pattern, already present.** Consider adopting OpenClaw's most-specific-wins binding-tier order explicitly for multi-agent routing if/when harw grows multiple concurrent agent personas per Gateway.

**6. Channel pairing: single-use, TTL, channel-scoped codes (default-deny for unknown senders)**
Problem: an unknown inbound sender on a public channel is a first-contact/DoS/spam vector.
OpenClaw: `dmPolicy=pairing` (default) issues a time-limited code, ≤3 pending per channel, no resend until it expires (docs/gateway/security/access-control.md).
harw today: **Present, near 1:1.** `harw-channel/src/pairing.rs::PairingCode` — Crockford-base32, 15-minute default TTL, single-use, scoped to one `ChannelId`, persisted via `harw-session-store`'s append-only log.
Recommendation: **Adopt/keep as-is.** One gap noted in PL-65 §4.5: "(verify) whether `PairingRegistry` journals the clear code today... If it does, store only the hash" — worth closing that specifically, since OpenClaw's own severity model treats pairing-code brute force as a named threat.

**7. Veto-chain tool policy — deny always wins, fail-closed**
Problem: multiple independent gates (plugins, hooks) deciding on one tool call must not let a later "allow" silently override an earlier "deny."
OpenClaw: `before_tool_call` hooks are terminal-veto: `{block:true}` stops lower-priority handlers; `{block:false}` is a no-op, never un-blocks (docs/concepts/agent-loop.md).
harw today: **Present, stronger.** `harw-extension-api/src/allow_rules.rs`: "`Deny` always wins over `Allow`, even if another matching rule..."; a poisoned lock fails closed to `Deny` (tested: `a poisoned lock must fail closed to Deny`).
Recommendation: **Adopt pattern, already present and test-enforced** — no action needed beyond keeping the invariant test in the gate suite.

**8. Capability/context ceiling — monotone reduction, structurally un-widenable**
Problem: a child agent (or a summarized context) must never be able to regain a permission or a context section its parent withheld — and this must not depend on someone remembering to check.
OpenClaw: memory provenance is a SQLite column the model can't write through prose; untrusted-origin facts are excluded from promotion as a hard precondition (docs/concepts/memory-architecture.md).
harw today: **Present, and structurally stronger.** `harw-authority::PermissionSet`, `harw-agent-dsl::authority::AuthorityCeiling::intersect` (only `intersect`, no `union`/`widen`), and `harw-context::ContextCeiling`/`ContextBudgetSpec::tighten` — the module doc for `ceiling.rs` states plainly "there is deliberately no `union`, no `widen`, no public constructor that merges two ceilings into a bigger one." This is a type-level guarantee, not a runtime label a reviewer has to check.
Recommendation: **No action — already ahead of OpenClaw's own design here.** Worth writing up as a harw-specific strength when comparing harnesses.

**9. Layered, tool-scoped sandboxing with explicit path/mount denylist**
Problem: sandboxing must not silently re-grant a globally denied tool, and read-only binds of certain host paths (`/run`, `/sys`) can still leak host state through socket inodes.
OpenClaw: tool policy evaluated before and independently of sandbox placement; network default `none`; path denylist blocks `/etc`, `/proc`, `/sys`, `~/.ssh`, Docker-socket aliases (docs/gateway/sandboxing.md).
harw today: **Present, and arguably stronger.** `harw-sandbox`/`harw-job-executor-bwrap::executor.rs` explicitly **withholds** `/sys` and `/run` from every plan and documents why: "a read-only bind of `/run` would still expose host Unix sockets... `connect(2)` on a socket inode ignores read-only mounts" — i.e. harw's own code comment already states the exact class of risk behind the Claw-Chain sandbox-escape family (see §C). `BwrapExecutor::discover()` finds `bwrap` only at fixed root-owned paths, "never via `PATH`" — directly forecloses the PATH-manipulation escape (CVE-2026-24763). `harw-job-runtime/src/host.rs` probes Landlock/bwrap capability without ever over-reporting enforcement.
Recommendation: **No action needed on the core model** — already stronger than OpenClaw's documented backends. Recommend a dedicated M1-lens gap-hunt pass specifically over `harw-sandbox`/`harw-job-executor-bwrap`/`harw-job-linux` to keep it that way (see §C, Claw Chain).

**10. Config: schema-strict, fail-closed, atomic writes**
Problem: an unknown/malformed config key should never be silently ignored or coerced.
OpenClaw: unknown keys or type mismatches make the Gateway refuse to start; atomic rename-onto-path writes (docs/gateway/configuration.md).
harw today: **Present, comprehensively.** Every `harw-config/src/*_toml.rs` module uses `#[serde(deny_unknown_fields)]`; `discovery.rs` documents the same fail-closed rationale almost verbatim ("unknown/malformed config would otherwise let discovery fail wide open").
Recommendation: **No action — already matched.**

**11. Boundary-aware hybrid compaction with hysteresis**
Problem: a naive token-count compaction can split a `tool_call`/`tool_result` pair, corrupting replay for any model that expects them adjacent.
OpenClaw: compaction always moves the split point so a tool_call/result pair stays together; safeguard mode validates the summary structurally with bounded retries, aborting without writing rather than committing a degraded summary (docs/concepts/compaction.md).
harw today: **Present, and arguably stronger.** `harw-core/src/compaction.rs` operates on `atomic_groups` (call+result as one unit) from the ground up — pairing can't be split because the algorithm never sees unpaired items — plus a deterministic pass (dedupe read-only repeats, drop superseded failed calls) *before* any model call, and `auto_compact.rs` uses relative-to-window thresholds (70%/30%/absolute-ceiling) with hysteresis to avoid compaction loops. If the summarization model call fails, the deterministic result stands (fallback preserved, matches OpenClaw's "abort without writing, keep original" contract).
Recommendation: **No action needed on the core algorithm.** Worth double-checking `harw-core` has an explicit structural-validation retry loop for the model-generated summary text itself (OpenClaw's "safeguard mode" checks required headings/verbatim identifiers survive) — I did not confirm this specific check exists; flag as a small follow-up, not urgent.

**12. Epistemic memory provenance + single-writer offline consolidation**
Problem: content-level scanning can't reliably catch a poisoned/untrusted memory; and concurrent memory writers can race.
OpenClaw: five-tier memory with an unforgeable provenance column (owner/agent/untrusted/system), gated at write time; a single scheduled "dreaming" pass is the only long-term writer, using optimistic concurrency + audit diff (docs/concepts/memory-architecture.md).
harw today: **Present, and structurally comparable or stronger.** `harw-memory/src/epistemic.rs` types `Provenance`, `Confidence`, `Validity`, `MemoryScope`, `OutcomeVerdict` as first-class enums (not a bolt-on label); `consolidation.rs::ConsolidationLock` enforces "one consolidation per root" via exclusive `create_new` lock file with 30-min expiry; `workflow.rs::WorkflowStep` is a commit-based, resumable state machine (`Idle→Claimed→WorkspaceSynced→AgentCompleted→BaselineCommitted→DbCommitted→Idle`) so a crash mid-consolidation resumes exactly where it left off rather than double-applying effects.
Recommendation: **No action needed on the model** — already matches or exceeds the OpenClaw pattern. Worth confirming the promotion gate checks provenance **before** any recall-frequency scoring (OpenClaw's explicit ordering) — `promote.rs`'s own doc notes some promotion paths (`FileMemoryStore::promote_pattern_hints`) are unfenced by time window; low-priority follow-up.

**13. Classified-error model/provider failover + credential pool**
Problem: blanket "retry on any error" wastes quota and can mask permanent failures; naive multi-key rotation can retry into the same bad key.
OpenClaw: a fixed error taxonomy (auth/rate-limit/timeout/billing/overload/not-found) decides retry-vs-rotate-vs-fallback; fallback is turn-scoped, never sticky (docs/concepts/model-failover.md).
harw today: **Present.** `harw-provider-http/src/credential_pool.rs::should_failover` rotates only on `ModelError::Auth`/`QuotaExceeded`, deliberately leaving transient/timeout/rate-limit errors to `retry.rs`'s backoff on the *same* credential; `rate_limiter.rs::ProviderRateLimiter` proactively paces from response headers (Anthropic- and OpenAI/DashScope-style families) rather than waiting for a hard 429 — this is the OpenClaw pattern already implemented, plus DEC-003 codifies "never exceed provider limits" as a binding decision with tests.
Recommendation: **No action — already matched, well-documented (DEC-003).**

**14. SecretRef credential indirection**
Problem: config files with plaintext secrets are a supply-chain and backup risk.
OpenClaw: every credential field accepts `env|file|exec|store` instead of plaintext (docs/gateway/secrets.md).
harw today: **Present.** `harw-config/src/auth_toml.rs::SecretRef{Env, File, Keyring, Secrets, FileJson{path,pointer}}`, backed by `harw-secrets` (envelope/KEK/DEK). Slightly broader address space than OpenClaw's four (adds `keyring` and JSON-pointer addressing).
Recommendation: **No action — already matched or exceeded.**

**15. Device pairing decoupled from network trust, mutual PQ handshake, revocation**
Problem: "reachable over a private network" is not the same as "authorized"; revocation must not require a restart.
OpenClaw: every WS connect (local or remote) needs device identity + signed challenge; approval is explicit regardless of network trust (docs/concepts/architecture.md, docs/channels/pairing.md).
harw today: **Present, and cryptographically stronger.** `harw-node-transport` already restricts TLS to **X25519MLKEM768** (post-quantum KEX) with **mutual ML-DSA-65** transcript signatures bound to the TLS exporter, and `NodeVerifier::is_known` refuses unknown clients *before the server signs anything* (PL-65 §0, §4.1–4.2). Live revocation (`is_known` reads the registry live, no restart) is **specced but not yet built** — PL-65 §4.4 describes it in detail as future work (RS4).
Recommendation: **Adopt the revocation slice specifically** (see §D item 4) — the crypto/handshake foundation is already ahead of OpenClaw; only the "revoke closes live streams + fails is_known immediately" behavior is missing.

**16. Thin-client UI vs. embedded-runtime (gap)**
Problem: N frontends each embedding session logic causes behavior drift between them.
OpenClaw: Control UI, TUI, CLI and native apps are all pure clients of the Gateway WS API; local/offline mode is an explicit, clearly-labeled degraded fallback (docs/web/tui.md).
harw today: **Partial/Missing.** `harw-tui`'s `app.rs` (14,943 lines per PL-65 §0) still drives the turn loop directly via `ChatGateway::borrow_turn_ctx()`; the thin-client split (`harw attach`, `SessionPort`) is fully designed in PL-65 §3–5 but not implemented. This is the one clear structural gap where OpenClaw's shape is already the acknowledged target.
Recommendation: **Adopt** (already scoped as PL-65's RS9 goal) — no new design needed, just execution.

**17. Local supply-chain trust pinning for config/agent layers**
Problem: a repo-local or synced config/agent layer can silently change and gain authority it was never re-approved for.
OpenClaw: ClawHub cross-checks a skill's declared frontmatter capabilities against what its code actually references, at publish time (docs.openclaw.ai/clawhub/skill-format).
harw today: **Different but present — content-digest trust, not behavior-drift detection.** `harw-home/src/trust.rs`: a repo-local `.harw` gets full authority only after explicit user approval bound to canonical root + owner UID + a BLAKE3 digest over security-relevant files; any change flips status to `Changed` and the layer falls back to restricted authority until re-approved. `harw-agent-dsl/src/skills.rs` documents that harw skills are **explicitly authority-free instruction fragments** — "a union therefore cannot widen any rights" (union can never widen rights) — so the specific risk ClawHub's check targets (a skill secretly needing an undeclared tool/env var) is largely foreclosed by design, not by a runtime scanner.
Recommendation: **Adapt, don't copy verbatim.** harw's digest-pinning model is a good fit for config/agent-definition layers (already present); a ClawHub-style manifest-vs-code drift scanner would only become relevant if harw's "skills" concept ever grows executable content beyond instruction fragments — not needed today.

**18. Durable scheduled jobs / proactive heartbeat (missing, corroborated)**
Problem: "remind me to do X" or periodic proactive checks need a deterministic scheduler, not free-text the model has to re-notice.
OpenClaw: time-based intents compile to cron jobs; event-based intents compile to a matcher table; a cheap periodic heartbeat lets an agent act without a human message, short-circuiting almost for free when there's nothing to do (docs/automation/cron-jobs.md).
harw today: **Missing at the agent/session level.** No cron/scheduler subsystem was found (`grep` across `harw-cli`, `harw-job-*` found no scheduler, only incidental "cron" substring hits). `harw-memory/src/heartbeat.rs` exists but is a *memory-maintenance* tick (HOT/WARM/COLD demotion, fact decay), not a proactive conversational heartbeat. This corroborates the same finding already flagged independently by the parallel Claude Code research sheet (`docs/planning/75-harness-patterns/claude-code.md`, item 4: "`/loop` and cron: **missing**").
Recommendation: **Adopt**, but treat as one shared roadmap item across both sibling research sheets rather than duplicating design work — see also that sheet's proposed `schedule` operation.

## C. Anti-patterns / incidents to avoid

All items below are OpenClaw's own disclosed incidents (upstream researcher confidence: "likely" unless noted) — useful precisely because several map onto harw's *own* already-catalogued review lenses (`docs/planning/85-gap-hunt/patterns.md`, M1/M5/M6/M7/M8).

1. **Auto-connect to an attacker-controlled target (CVE-2026-25253, CVSS 8.8).** The Control UI read `gatewayUrl` from the browser query string and opened a WS connection to it automatically, leaking the stored auth token to whatever host the link named. Fix: confirm-before-connect. *Sources: wiz.io/vulnerability-database/cve/cve-2026-25253; ccb.belgium.be advisory.* harw note: structurally avoided today — harw's local control surface is a Unix domain socket (`harw-web`), not a URL a browser page can redirect a connection to, and PL-65's remote-device design (§4.2, §4.6) pins the host key fingerprint out-of-band in the pairing string before a new device trusts a target. Keep this invariant explicitly if a browser-facing remote client is ever added.

2. **Unauthenticated control-plane write → command injection (CVE-2026-25593).** A local WS caller could call `config.apply` without authentication and set an unsanitized `cliPath` later used for command discovery. *Source: github.com/advisories/GHSA-g55j-c2v4-pjcg.* Lesson: control-plane write RPCs need both authentication *and* sanitization of any config value that later flows into process construction — missing either one alone is enough. harw note: `harw-config`'s `deny_unknown_fields` fail-closed parsing doesn't by itself guarantee every config-derived path is canonicalized before being handed to `std::process::Command`; worth a targeted grep for config-sourced strings reaching process/exec construction.

3. **Sandbox escape via PATH manipulation (CVE-2026-24763) + arbitrary file read via unchecked output path (CVE-2026-25475, `isValidMedia()` accepting `../`, `~/`, absolute paths).** *Sources: github.com/advisories/GHSA-r8g4-86fx-92mq; sentinelone.com/vulnerability-database/cve-2026-25475.* These are textbook instances of harw's own **M1** ("check-then-use / TOCTOU, default-allow after success path") and **M5** ("unbounded input at a trust boundary") lenses. harw note: `BwrapExecutor::discover()` already finds `bwrap` "never via `PATH`" (averts the first class). No specific "agent emits a file path that gets read back" tool was audited in this pass — recommend explicitly adding "any tool that turns agent/model output into a filesystem path" to the M1/M5 gap-hunt checklist for `harw-tool-fs`, `harw-tool-doc`, `harw-tool-browser`.

4. **"Localhost is trusted" (ClawJacked, no CVE id in sources found).** A malicious webpage opened a cross-origin WebSocket to `ws://localhost:<port>` (same-origin policy doesn't cover WS) and brute-forced the local Gateway password with no rate limit, auto-registering as a trusted device. *Source: thehackernews.com, Feb 2026.* Lesson: a loopback bind is not identity; it still needs auth + rate limiting, or better, a transport a browser can't address at all. harw note: `harw-web`'s local surface binds a **Unix domain socket** authenticated via `SO_PEERCRED` (`harw-web/src/peer.rs`) — a browser physically cannot open this transport, so harw is structurally immune to this exact attack class today. This is a design invariant worth preserving explicitly (never swap the local control socket for a loopback TCP port without re-deriving this guarantee).

5. **"Claw Chain" — four chained CVEs, none alarming alone (TOCTOU sandbox-mount race → credential exposure → priv-esc → persistence; CVSS up to 9.6).** *Source: thehackernews.com/2026/05, cyera.com.* Lesson: audit sandbox mount/check-then-use sequences as an end-to-end kill chain, not as isolated findings — each step alone looks like normal agent behavior. harw note: this is precisely what `docs/planning/85-gap-hunt/patterns.md` M1 already describes ("Check first, then use (TOCTOU)"). Recommend running a **named, TOCTOU-focused gap-hunt-area pass** specifically over `harw-sandbox`/`harw-job-executor-bwrap`/`harw-job-linux` rather than relying on it surfacing in a general sweep — the withheld-`/run`/`/sys` comment in `executor.rs` shows the team already thinks this way; make it a recurring, explicitly-named review, not a one-off.

6. **Naming/scale claims — unconfirmed.** The upstream researcher's own open-questions list flags several numeric claims (star counts, "543 CVEs," "13K skills," "245,000 exposed instances") as SEO-farm-sourced or unverified against primary GHSA/NVD records. None of these are used as load-bearing facts above; flagged here only so this catalog doesn't get cited as having verified them.

## D. Top 5 adoptions for harw (value/effort order, each one round)

1. **Config/security self-audit check.** OpenClaw's `openclaw security audit` checks drift from safe defaults (loopback bind, token auth, pairing enabled, allowlists) in one command (docs.openclaw.ai/gateway/security). harw already has a `DoctorCheck` trait and `health_checks()` in `harw-cli/src/lifecycle.rs`. *Effort: low — add one new `DoctorCheck` implementation.* *Value: high — catches an accidentally-unsafe config before it ships, one command, no new subsystem.*

2. **Prove (and if needed close) that MCP-sourced tools cannot bypass the native tool-policy gate.** OpenClaw's explicit design lesson: "tools obtained from a connected MCP server are NOT a policy bypass — they pass through the exact same tool-profile/policy machinery as built-in tools" (docs.openclaw.ai/tools/mcp). `harw-mcp-client/src/tool_bridge.rs` and `harw-mcp-server/src/session.rs` were flagged as "unverified" by the sibling Claude Code research sheet too. *Effort: low — add an explicit test asserting an MCP-sourced `ToolCall` is evaluated by `harw-extension-api::allow_rules` exactly like a native one; fix if the test fails.* *Value: high — closes an entire policy-bypass class, independently corroborated as worth checking by two parallel research tracks.*

3. **Structured fallback-decision observability event, separate from any user-facing notice.** OpenClaw logs `model_fallback_decision` (from/to model, failure reason, outcome) distinct from the chat-facing "Model Fallback" notice, and explicitly scopes exhaustion alerts to be model-aware so an unrelated model's rate limit doesn't trigger a false alarm (docs.openclaw.ai/concepts/model-failover). harw's `credential_pool.rs`/`retry.rs` already classify errors and cooldown per entry; they likely don't yet emit one dedicated, queryable observability event for the decision itself. *Effort: low-medium — one new `harw-observe` metric/event emitted from the existing decision points.* *Value: medium-high — pure debuggability win with no behavior change, low risk.*

4. **Live device/pairing revocation (smallest slice of the already-designed PL-65 §4.4).** OpenClaw revokes a device by flipping a live-read flag with no restart, closing its open streams immediately (docs/concepts/architecture.md). harw's `NodeTransportServer`/`NodeVerifier::is_known` foundation is already stronger than OpenClaw's (post-quantum mutual auth), but the "revoke → `is_known` reads it live, in-flight streams get closed" behavior is speced in PL-65 §4.4 and not yet built. *Effort: medium — one focused slice, deliberately decoupled from the rest of the multi-round PL-65 rollout.* *Value: high — closes a real "stolen/compromised device" security gap using infrastructure that already exists.*

5. **Provider-authoritative usage/cost as primary source, local estimate only as fallback.** OpenClaw pulls quota/spend directly from each provider's usage API and labels its own token-estimate as a fallback only (docs.openclaw.ai/concepts/usage-tracking). harw's `harw-ops/src/usage.rs` and `harw-core/src/state_store.rs` currently track tokens only (flagged "teilweise" by the sibling Claude Code sheet too — independent corroboration from a second harness). *Effort: medium — needs one provider-usage-endpoint integration per provider family, can start with Anthropic's admin API.* *Value: medium-high, corroborated twice — real cost visibility rather than an estimate that can drift from the actual bill.*