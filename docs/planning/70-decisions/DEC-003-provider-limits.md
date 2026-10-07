---
id: DEC-003
title: Respect provider limits instead of circumventing them
status: accepted
date: 2026-09-27
tags: [decision, work-driver, rate-limits, provider]
related:
  - ../90-migration-ledger/MIGRATION_LEDGER.md
  - ../../harw-config/src/provider_toml.rs
  - ../../harw-provider-http/src/rate_limiter.rs
  - ../../harw-provider-http/src/retry.rs
  - ../../harw-plan-bridge/src/work_driver.rs
---

# DEC-003 — Respect provider limits instead of circumventing them

## Decision
The work driver adheres to the provider limits configured by the user
(`max_concurrency`, `rate_limit.max_concurrent`) and to the quotas reported
by the provider at runtime (rate-limit headers, HTTP 429). All workers of a
provider share its in-process instance, including the
`ProviderRateLimiter`; the parallelism actually permitted for a wave is
additionally capped via `effective_parallel` in `WorkDriveInput`. An HTTP 429
is not a worker error but a wait signal with its own backoff budget.

## Why
- TPM/RPM quotas are limits contractually agreed by the user; interpreting
  them generously or circumventing them through multiple provider instances
  violates the ToS and in effect acts like a DDoS against one's own account.
- `max_concurrency` (provider-wide) and `rate_limit.max_concurrent`
  (budget bucket per model) are independent limits that apply at the same
  time — the smaller one wins; neither limit may be exceeded by parallel
  work-driver waves.
- A proactive pacer (`ProviderRateLimiter`) is cheaper than reactive 429
  handling: it reads the quota headers reported by the provider and waits
  briefly before a quota is exhausted, instead of running into the wall
  first.
- 429 means "wait a moment", not "the job has failed"; a dedicated wait
  budget (default 5 min, exponential backoff up to 60 s) prevents both
  giving up prematurely and waiting endlessly.
- `effective_parallel` is always the minimum of the spec limit and the
  provider limit reported by the caller — so the work driver cannot run a
  wave wider than the provider allows, even if the spec would permit more
  parallelism.

## Consequences
- Each provider client is instantiated exactly once per process and shared
  by all workers; no worker may open its own unthrottled client.
- Wave planning must know `effective_parallel` before enqueueing, otherwise
  the provider quota risks being oversubscribed by workers started at the
  same time.
- Trade-off: strictly respecting the limits costs throughput compared to
  more aggressive scheduling — accepted, because exceeding quotas provokes
  hard errors, lockouts, or extra costs.
- Future provider integrations must connect their rate-limit header family
  (Anthropic, OpenAI/DashScope style, `retry-after`) to
  `ProviderRateLimiter`, otherwise the pacer remains ineffective for them
  and only reactive 429 backoff is left.

## Implementation R15
Proactive pacing now runs through the trait method
`ModelProvider::pacing_wait()` with a default of `None`, so existing
providers remain unchanged. HTTP providers report the maximum of the wait
time of their `ProviderRateLimiter` (quota headers or 429 cooldown) and the
preview of the configured budget (`preview_wait`). Wrapper providers pass
the value through; the router queries its default backend provider. The work
driver pauses before each wave chunk by the reported wait time, instead of
reacting only once a 429 arrives.

## Where in the code
- `harw-config/src/provider_toml.rs` — `ProviderToml::max_concurrency`,
  validation against `max_concurrency = 0`; `rate_limit.max_concurrent`.
- `harw-provider-http/src/rate_limiter.rs` — `ProviderRateLimiter`,
  `wait_for_slot`, `wait_for_slot_with_estimate`, `observe_headers`.
- `harw-provider-http/src/retry.rs` — `RetryPolicy::rate_limit_budget`,
  `rate_limit_backoff_delay`, `rate_limit_decision`, `ModelError::RateLimited`.
- `harw-plan-bridge/src/work_driver.rs` — `WorkDriveInput::effective_parallel`.
- `harw-core/src/model.rs` — `ModelProvider::pacing_wait` (default `None`).

## Related
- [DEC-004 No parallel builds](DEC-004-no-parallel-builds.md)
- [DEC-005 Small scopes, many waves](DEC-005-small-scopes-waves.md)
- [DEC-007 Worker rights](DEC-007-worker-rights.md)
