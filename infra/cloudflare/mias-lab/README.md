# mias-lab Cloudflare AI Worker

`mias-lab` is the Harwness-facing Cloudflare Worker in front of Workers AI.
It complements the Kimi-specific worker: this Worker is intentionally small and
optimized for cache reuse, bounded concurrency, and the existing Harwness
`openai-chat` provider contract.

## Request path

1. Harwness sends `POST /v1/chat/completions` with its normal OpenAI-chat body.
2. The front Worker authenticates the bearer/API key and chooses one Durable
   Object named by model.
3. The model Durable Object enforces a model-specific in-flight limit, bounds
   its waiting queue, and coalesces identical non-streaming requests already in
   flight.
4. The Durable Object calls the single `AI` binding through AI Gateway
   `mias-lab`.
5. Exact-response caching is enabled only for non-streaming requests without
   tools. Streaming/tool requests still get Workers AI prefix-cache affinity.

## Why one Durable Object per model

Concurrency is a shared capacity property of a model, not of an individual
Worker isolate. Isolate-local counters cannot enforce it. A single global
Durable Object would serialize unrelated models, so
`MODEL_LANES.idFromName(model)` uses the model as the natural coordination
boundary.

Default in-flight limits:

- Kimi K2.6/K2.7 and GLM 5.2/5.3 families: 2
- GPT-OSS-120B and DeepSeek V4: 3
- other allowlisted models: 6

The queue is bounded to 32 waiters per model with a 15 second admission wait.
When pressure exceeds that budget the Worker returns `429` plus
`Retry-After`.

## Caching

Two different caches are intentionally used:

- **AI Gateway exact-response cache**: default TTL 1800 seconds. Enabled only
  for non-streaming requests without tools. A canonical SHA-256 cache key omits
  Harwness identity metadata, so identical model requests can be reused across
  agents without fragmenting the cache.
- **Workers AI prefix cache**: every inference sends `x-session-affinity`. The
  preferred source is `x-harw-cache-affinity`; until Harwness emits that
  planned header, the Worker falls back to `x-harw-session` +
  `x-harw-agent`, then to a stable hash of model/system/first-user content.

The Worker explicitly sends `cf-aig-collect-log-payload: false`. AI Gateway
can retain request metadata/metrics without storing prompt and response
payloads.

## Cloudflare resources

- Worker name: `mias-lab`
- AI Gateway: `mias-lab`
- Workers AI binding: `AI`
- Durable Object binding: `MODEL_LANES` -> `ModelLane`
- Secret: `GATEWAY_API_KEY`

Deploy:

```sh
cd infra/cloudflare/mias-lab
npm install
npx wrangler secret put GATEWAY_API_KEY
npx wrangler deploy
```

The account currently exposes Workers at `*.mm29942.workers.dev`, therefore
the default endpoint after deployment is expected to be:

```text
https://mias-lab.mm29942.workers.dev/v1
```

## Harwness provider

Copy `harw-provider.example.toml` into the active profile's `providers/`
directory, normally as `cf-worker.toml`, and set `MIA_LAB_API_KEY` in the
profile environment/secret layer.

Keep client-side `max_concurrency` as a first line of defence. The Durable
Object limit is the shared backstop across Harwness processes and Worker
isolates, not a replacement for Harwness placement/rate-limit accounting.

## Adaptive concurrency (draft implementation)

The model-scoped Durable Object now starts with the existing conservative limits (frontier: 2, DeepSeek V4/GPT-OSS-120B: 3, others: 6). When requests overlap it expands the local admitted in-flight limit up to a bounded cap (10/12/16 respectively); quiet traffic settles back after a minute. A shared Workers AI capacity response cuts the limit and applies a 20-second admission cooldown instead of fanning out across Gateway IDs. This is **not** an increase in account/model quota: max values are deployment guardrails requiring measured production tuning.

All 19 existing backend Gateway IDs remain eligible as sticky primaries; only transient route-local errors can use alternate Gateways, with a capped attempt budget (3, expanding to 6 under high demand). Full-response caching continues to be restricted to non-streaming, tool-free requests; prompt-prefix caching and `x-session-affinity` are unchanged. Billing remains **postpaid**.

Run `npm run check && npm test` in `infra/cloudflare/mias-lab/` before deploying. Evaluate request success, 429/3040, first-token latency, queue waits, in-flight peak and `input_cached_tokens/input_tokens` against baseline. Roll back if failure rate or cache efficiency regresses; keep this change in a draft PR until production-like integration tests and versioned rollout are verified. File-level coding isolation belongs to Harwness job/worktree orchestration, not this model/Gateway limiter.
