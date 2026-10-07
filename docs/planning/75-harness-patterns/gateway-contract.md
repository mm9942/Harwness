---
id: HP-GW
title: Contract between harw and a dedicated Cloudflare Worker in front of Workers AI
status: draft
date: 2026-09-27
tags: [harness-patterns, gateway, workers-ai, cache, placement]
related:
  - README.md
  - claude-code.md
  - ../70-decisions/DEC-003-provider-limits.md
---

# Contract: harw ↔ dedicated Cloudflare Worker in front of Workers AI

## Prerequisite
In production, harw requires a **dedicated Cloudflare Worker** in front of
Workers AI. It is the lower tier of placement:
- It holds one or more Workers AI bindings (lanes).
- It distributes requests across them.
- It derives session affinity itself.

harw does **not** model the individual bindings. To harw, the Worker is
**one** provider with capacity.

Account, binding, and Worker names are not kept in the repo.

## Two-tier placement
1. **harw** (placement engine) chooses:
   - the provider, i.e. this Worker;
   - model and role;
   - the prefix group.
2. **The Worker** chooses the binding or lane and an affinity that stays
   "sticky", and calls Workers AI.

## Request (harw → Worker)
- **API:** OpenAI-chat-compatible (`api = "openai-chat"`).
- **Configuration:**
  - `base_url` points to the Worker.
  - `gateway_identity_headers = true`.
- **Headers**, set only when a `RequestIdentity` is present, truncated to
  visible ASCII, at most 64 characters:
  - `x-harw-session`: root session of the agent tree.
  - `x-harw-agent`: ID of the agent.
  - `x-harw-role`: role, e.g. `root-orchestrator` or `worker`.
- harw deliberately **never** sets **`x-session-affinity`**. The Worker
  derives it itself. Source: `harw-provider-http/src/lib.rs`,
  `identity_headers`.
- **Capacity on the harw side (DEC-003):**
  - `max_concurrency` limits parallelism.
  - `[rate_limit]` models RPM and TPM.
  - Frontier models on Workers AI: 20 requests per minute per model and
    account with standard billing, 50 with AI Gateway credits.

## Response (Worker → harw)
- **Format:** OpenAI chat, with `usage`.
- **Cache tokens:** Cached input tokens are reported as a separate number.
  Only then can harw compute costs correctly, see the cost model.
- **429:** with `Retry-After`. harw then pauses and does not count the attempt.

## Planned extensions (optional, backward-compatible)
- **`x-harw-cache-affinity`** (request):
  - Names the **prefix group**, e.g. `session/role/prefix-hash`.
  - Short-lived workers with the same prefix, i.e. the same system prompt,
    the same tools, and the same repo context, thereby land on the same
    instance, and the cache stays warm.
  - If the header is not set, the Worker falls back to session plus agent.
  - It is set by the placement engine or the work driver.
- **`x-harw-lane` and `x-harw-affinity`** (response): They report which lane
  and which affinity key were used. This is for observability, since Logpush
  is not available in every account.
- **Capacity signals** (response, optional): remaining quota and reset time,
  in the same header families that `ProviderRateLimiter` already reads.

## Why this way (patterns)
- **Cache affinity follows the prefix, not the identity.** For cache-heavy
  workloads (around 98% cache reads), the cache-read price determines the
  cost. Every worker pays the full input price for a cold start.
- **Costs and usage come from harw itself** (usage per response, cost status)
  and not from Logpush.
