---
id: HP-COST
title: Kostenmodell — cache-bewusst, gestuft (Luna, GLM-5.3 Flash, AI Gateway)
status: proposed
date: 2026-09-27
tags: [cost, cache, pricing, placement, workers-ai, ai-gateway]
related:
  - README.md
  - gateway-contract.md
  - ../66-placement/README.md
  - ../70-decisions/DEC-003-provider-limits.md
---

> Workflow `cost-model-research`: 2 Sonnet-Sammler lasen über das Cloudflare-MCP
> 52 Modellpreise und 13 Gateway-Funktionen aus, danach entwarf ein Opus-Agent
> das Modell gegen den harw-Code.
>
> **Die Preise ändern sich.** Sie gehören in Daten mit Quelle und Datum, nie in
> Code. Stand der Preise: Dokumentabruf am 2026-09-27.
>
> **Warum GLM-5.3 Flash für große Aufträge passt:** Es ist am unempfindlichsten
> gegen kalten Cache. Sinkt die Trefferquote von 98,6 % auf 90 %, steigen die
> Kosten nur um den Faktor 1,33, bei Opus um 2,55 und bei Fable um 3,5. Dazu
> kommt der günstigste Output unter den billigen Modellen: Ab etwa 1,3 % Output
> bezogen auf die Input-Seite schlägt es Luna 5.6.

# Cache-aware, tiered cost model for harw: design note (Luna, cache prices, Cloudflare AI Gateway)

Date: 2026-09-27. Read-only pass. Prices were read with the Cloudflare docs MCP tool (`mcp__cloudflare__docs`); no Cloudflare account was touched.

Status marks in the tables:
- **V** means I re-read the number in this pass.
- **R** means it comes from the research JSON handed to this task and was not re-read.
- The model pages show no "last updated" banner in the retrieved chunks, so they are marked "n/s" and "retrieved 2026-09-27".

## 0. Summary

- **Luna is cheap mainly because of its cache prices.** GPT-5.6 Luna costs $0.20 input, $0.02 cache read (10 % of input), $0.25 cache write (125 % of input) and $1.20 output per 1M tokens.
  - At our measured 98.6 % cache-read share, 1M input-side tokens cost **$0.0225 to $0.0232** instead of $0.20.
  - Claude Opus 5.5 costs **$0.253 to $0.267** for the same volume (about 11× Luna). Without the cache it would be 20× Luna.
- **The cache-read price decides cost, not the list input price.**
  - Opus 5.5 warm ($0.25) is cheaper than Opus 5 warm ($0.56), even though its list input price is 80 % of Opus 5.
  - DeepSeek V4 Flash (@cf) warm ($0.020) is cheaper than Luna warm, although its list input price is 2.2× Luna's.
  - GLM-5.3 Flash, the "reference workhorse" in 66-placement, is **41 % more expensive than Luna on the input side** when warm, although its list input price is lower.
- **harw has no cost computation today.** What exists:
  - `Pricing` is `f32`, keyed per model and not per route. It has no cache-write rate and no tiers.
  - It is filled in only for the Anthropic, Z.ai and Mistral catalog files. OpenAI, including both Lunas, is `None`.
  - Four existing accounting bugs would give wrong numbers even after prices are added (section 2.1).
- **Proposal:**
  - A new ring-I crate `harw-cost` with exact integer money arithmetic (picodollars).
  - Usage normalized into disjoint buckets (fresh input, cache read, cache write, output), with the same inclusive-versus-separate subtraction rule AI Gateway uses.
  - Prices as data (`prices.toml` plus a config override), feeding session cost, cost ceilings, the placement score and work-driver/judge accounting.
- **AI Gateway** is worth adding as an *optional* backend profile, as a backstop (spend limits, logs), not as the source of truth. harw's own reservation-based limiter must be the primary control.

---

## 1. Price table (USD per 1M tokens)

**Prices change. They must live in data (`prices.toml` plus a config override) with `source` and `retrieved` on every row, never in Rust code.**

| Model (Cloudflare catalog id) | API | Context | Input | Cache read | Cache write | Output | Tiers | Src |
|---|---|---|---|---|---|---|---|---|
| openai/gpt-5.6-luna | Responses | 1.05M | 0.20 | 0.02 | 0.25 | 1.20 | flat | [L56] V |
| openai/gpt-6-luna | Responses, Chat | 1.05M | 0.10 / 0.20 | 0.01 / 0.02 | 0.125 / 0.25 | 0.50 / 0.75 | short / long, **threshold not published** | [L6] R |
| openai/gpt-5.6-sol | Responses | 1.05M | 2.00 | 0.25 | 3.125 | 10.00 | flat (a 50 %-off promo ended Sep 18) | [S56] R |
| openai/gpt-6-sol | Responses, Chat | 1.05M | 2.00 / 4.00 | 0.20 / 0.40 | 2.50 / 5.00 | 10.00 / 15.00 | short / long, threshold not published | [S6] V |
| anthropic/claude-opus-5.5 | Messages | 1M | 4.00 | 0.20 | 5.00 | 20.00 | flat | [O55] V |
| anthropic/claude-opus-5 (also 4.5–4.8) | Messages | 1M (4.5: 200k) | 5.00 | 0.50 | 6.25 | 25.00 | flat | [O5] V |
| anthropic/claude-sonnet-5 | Messages | 1M | 2.00 | 0.20 | 2.50 | 10.00 | flat | [SN5] V |
| anthropic/claude-haiku-4.5 | Messages | 200k | 1.00 | 0.10 | 1.25 | 5.00 | flat | [H45] V |
| anthropic/claude-fable-5.1 | Messages | 1M | 10.00 | 0.25 | 12.50 | 50.00 | flat | [F51] R |
| @cf/zai-org/glm-5.3-flash | Workers AI / OpenAI-compatible | 1.31M | 0.15 | 0.03 | not published | 0.50 | flat | [WAIP] V |
| @cf/zai-org/glm-5.2, glm-5.3 | Workers AI | 262k (5.2) | 1.40 | 0.26 | not published | 4.40 | flat | [WAIP] V |
| @cf/moonshotai/kimi-k2.6 | Workers AI | 262k | 0.95 | 0.16 | not published | 4.00 | flat | [WAIP] V |
| @cf/moonshotai/kimi-k2.7-code | Workers AI | 262k | 0.95 | 0.19 | not published | 4.00 | flat | [K27] R |
| @cf/deepseek-ai/deepseek-v4-flash-0731 | Workers AI | 1.05M | 0.44 | 0.014 | not published | 1.32 | flat | [DSF] R |
| minimax/m3 (tier example) | Chat, Anthropic Messages | 1M | 0.30 / 1.20 | 0.06 / 0.24 | not published | 1.20 / 4.80 | ≤512k / >512k | [MM3] V |
| xai/grok-4.6 (tier example) | Chat | 500k | 2.00 / 4.00 | 0.50 / 1.00 | not published | 6.00 / 12.00 | <200k / ≥200k | [GRK] R |

**Notes**
- **Same price direct or through Cloudflare.** Third-party inference through Cloudflare "is passed through with no markup — you pay the same per-token rates as you would directly with the provider". Cloudflare adds a 5 % fee on credits bought through Unified Billing ([UB], last updated Sep 23, 2026). So these rows also serve as direct-route prices.
- **Workers AI (@cf) models** are billed in Neurons: $0.011 per 1,000 Neurons, with 10,000 free Neurons per day ([WAIP], last updated Sep 17, 2026, per the R pass). No cache-write premium is published for them, so price writes at the input rate.
- **Cross-check against harw's own catalog.** `vendor_anthropic.rs` (sourced from platform.claude.com pricing, 2026-09-24) has Opus 5.5 at $4 / $20 / cache read $0.20 and Fable 5.1 at $10 / $50 / $0.25, which matches the table. It does *not* carry cache-write rates.
- **Data drift example.** `vendor_anthropic.rs` gives Sonnet 4.6 a 1M context; the Cloudflare page says 200k. The same model can differ by route, which is one more reason to key prices by route.
- **Zero data retention.** The Luna 5.6 and Opus 5.5 pages show no "Zero data retention" row, while Opus 4.5–4.8 show "Yes". Check this per model if the data path matters.

## 2. Cost model for harw

### 2.1 Current state (findings)

- **F1. No cost computation exists.**
  - A repo-wide grep finds no `cost_usd`, `total_cost` or `MicroUsd` in any Rust file.
  - `descriptor::Pricing` (`harw-model-catalog/src/descriptor.rs:282`) has three `f32` fields (input, output, cached input), is keyed per model and has no cache-write rate or tiers.
  - Its doc says "autoritative Preise leben in `harw-provider`", but no such table exists.
  - OpenAI rows are all `pricing: None`, including `gpt-5.6-luna` and `gpt-6-luna` (`vendor_openai.rs`). Anthropic prices are hard-coded tuples (`vendor_anthropic.rs:72`).
- **F2. Luna cache writes are missed.**
  - The Responses parser reads `input_tokens_details.cache_creation_input_tokens` (`harw-provider-http/src/lib.rs:3911`).
  - The Luna response sample on Cloudflare reports `input_tokens_details.cache_write_tokens` [L56].
  - Luna cache writes therefore land in "fresh input" and are billed $0.20 instead of $0.25. For Sol it is $2.00 instead of $3.125.
- **F3. Aggregating mixed providers gives wrong totals.**
  - `TokenUsage::add` does `cache_separate |= other.cache_separate` (`harw-types/src/usage.rs:85`).
  - A session or child tree that mixes Anthropic (separate) and OpenAI or Workers AI (inclusive) rounds therefore double-counts the OpenAI cached tokens in `prompt_tokens()` and `uncached_input_tokens()`.
  - This happens after a `/model` switch, with a judge on another provider, or when child sessions are aggregated (`harw-core/src/session.rs:2076`, `child_controller.rs:2568`).
- **F4. `/usage` hit rate is wrong for Anthropic.**
  - It computes `cached / input_tokens` (`harw-ops/src/usage.rs:113`).
  - For Anthropic, `input_tokens` is only the uncached rest, so a real 98.6 % hit rate prints as about **7000 %**.
  - The correct formula is `read / prompt_tokens()`.
- **F5. Work-driver budgets count tokens, not dollars.** Only `token_budget: 2_000_000` exists (`harw-plan-bridge/src/work_driver.rs:136,152`); 66-placement lists "cost ceiling: missing".
- **F6. Where to store cost state.** `SessionStateSnapshot` is `deny_unknown_fields` with version 1 (`harw-core/src/state_store.rs:262`), so cost state belongs in the `SessionMeta` sidecar. That sidecar allows new fields with `#[serde(default)]`.
- **F7. The direct `cloudflare` route never sends `x-session-affinity`.** This is on purpose for the own-worker gateway (`lib.rs:3986`). But Cloudflare recommends that header to get Workers AI prefix caching [AFF]. Without it, direct @cf calls may lose the cache discount.

### 2.2 Harw-owned types (new crate `harw-cost`, ring I, dependencies `harw-types` and `serde` only)

```rust
pub struct RatePerMTok(pub u64);   // micro-USD per 1M tokens: $0.02 -> 20_000, $3.125 -> 3_125_000
pub struct PicoUsd(pub u128);      // tokens x RatePerMTok = picodollars, exact, order-independent
pub struct MicroUsd(pub u64);      // ceilings and display (matches 66-placement)

pub struct CacheRates { pub read: Option<RatePerMTok>, pub write: Option<RatePerMTok> } // None => input rate (AI Gateway rule [CC])
pub struct TokenRates { pub input: RatePerMTok, pub output: RatePerMTok, pub cache: CacheRates }
pub enum   TierThreshold { PromptTokensAbove(u64), Unpublished }            // GPT-6 Luna/Sol => Unpublished
pub struct PriceTier { pub when: TierThreshold, pub rates: TokenRates }
pub enum   BillingBasis { Metered, Surcharged { bps: u16 }, Subscription, Local } // Unified Billing = 500 bps
pub enum   ModelMatch { Exact(String), Prefix(String) }                    // like AI Gateway model_rule
pub struct PriceKey { pub route: ProviderId, pub model: ModelMatch }       // route-specific, not per model
pub struct ModelPrice { pub key: PriceKey, pub base: TokenRates, pub tiers: Vec<PriceTier>,
                        pub basis: BillingBasis, pub provenance: PriceProvenance }
pub struct PriceProvenance { pub origin: PriceOrigin /* Config|Embedded|Discovery */,
                             pub source: String, pub retrieved: DateYmd, pub valid_until: Option<DateYmd> }

pub enum   CacheAccounting { Inclusive, Separate }
pub struct UsageBreakdown { pub fresh_input: u64, pub cache_read: u64, pub cache_write: u64,
                            pub output: u64, pub reasoning: Option<u64> /* subset of output */ }
pub enum   CostBounds { Exact(PicoUsd), Range { lo: PicoUsd, hi: PicoUsd } }
pub struct CostBreakdown { pub fresh_input: PicoUsd, pub cache_read: PicoUsd, pub cache_write: PicoUsd,
                           pub output: PicoUsd, pub surcharge: PicoUsd, pub total: CostBounds }
pub enum   PricedUsage { Priced(CostBreakdown), GatewayCacheHit, Unpriced(UnpricedReason) }

pub fn normalize(u: &TokenUsage, acct: CacheAccounting) -> (UsageBreakdown, Option<UsageAnomaly>);
pub fn cost(p: &ModelPrice, u: &UsageBreakdown) -> CostBreakdown;
pub fn expected_cost(p: &ModelPrice, f: &UsageForecast) -> ExpectedCost { /* expected, worst_case */ }
```

**Why integers.** Placement scoring must be deterministic, and budget comparisons must not depend on the order of summation. Tokens × µUSD/MTok gives exact picodollars, so no floats appear in any public API.

### 2.3 Normalization per provider (the AI Gateway rule [CC])

| Wire format | Accounting | fresh_input | cache_read | cache_write | output |
|---|---|---|---|---|---|
| Anthropic Messages (direct or `/ai/v1/messages`) | Separate | `input_tokens` | `cache_read_input_tokens` | `cache_creation_input_tokens` | `output_tokens` (thinking included) |
| OpenAI Responses (Luna, Sol; direct or `/ai/v1/responses`) | Inclusive | input − read − write | `input_tokens_details.cached_tokens` | `input_tokens_details.cache_write_tokens` (fix F2), fallback `cache_creation_input_tokens` | `output_tokens` (reasoning is a subset) |
| OpenAI Chat, DashScope, Workers AI OpenAI-compatible | Inclusive | prompt − read − write | `prompt_tokens_details.cached_tokens` | `prompt_tokens_details.cache_creation_input_tokens` | `completion_tokens` |

**Rules**
- **Inclusive example.** For inclusive accounting AI Gateway computes fresh = 1000 − 600 − 200 = 200 [CC]. This is the golden test case.
- **Anomalies.** If read + write > input under Inclusive, record a `UsageAnomaly`, clamp fresh to 0 and log it.
- **Accounting per route is data.** Each route's accounting comes from its spec, with a config override `cache_accounting = "inclusive" | "separate"`. This is needed because OpenAI-compatible third parties differ. Pin every route with a fixture test.
- **Aggregate buckets, not wire usage.** Aggregate `UsageBreakdown` values (plain sums). `TokenUsage` stays as the wire record; this fixes F3.
- **Hit rate** is `cache_read / (fresh + read + write)`; this fixes F4.

### 2.4 Cost computation

1. **Pick the tier.** Use `prompt = fresh + read + write`, since the example tiers are defined by context size [MM3][GRK]. If a threshold is `Unpublished`, return `Range{lo: short tier, hi: long tier}`. Ceilings always use `hi` (fail closed).
2. **Add up the buckets:** `total = fresh×input + read×(read ?? input) + write×(write ?? input) + output×output`.
3. **Surcharge:** `total × bps / 10_000` for `Surcharged` (5 % Unified Billing [UB]).
4. **Zero-cost cases.**
   - An AI Gateway response-cache HIT (`cf-aig-cache-status: HIT`) is `GatewayCacheHit` and costs 0, even with custom costs set [CC][CACHE].
   - `Subscription` (the Codex/ChatGPT route in `codex.rs`) and `Local` have marginal cost 0. Also record a "list-price shadow cost" so they can be compared with metered routes.
5. **Guardrails.** When AI Gateway Guardrails is on, add a second call: `@cf/meta/llama-guard-3-8b`, billed as Workers AI token inference [AIGP]. Its price row is still missing.

### 2.5 Where the pieces live

| Piece | Crate (ring) |
|---|---|
| Types, `normalize`, `cost`, `expected_cost`, `PriceBook::lookup(route, model)` | **`harw-cost` (I)**, new, plus an entry in `arch-policy.toml` |
| `[pricing]` and `[cost_limit]` TOML schema and validation | `harw-config` (I), which depends on `harw-cost` (I→I is allowed) |
| Embedded price data `prices.toml` (`include_str!`, like `providers.toml`); building the `PriceBook` | `harw-model-catalog` (A); `descriptor::Pricing` becomes a derived, deprecated view |
| Normalization at the wire boundary, `CostLimiter`, `cf-aig-cache-status` | `harw-provider-http` (A) |
| Per-round cost in the turn meter and `UsageRound` | `harw-core` (A) |
| Cost totals in the `SessionMeta` sidecar | `harw-session-store` (I) |
| `/usage`, `/cost` and the headless `total_cost_usd` | `harw-ops` (A) |
| `CostCeiling`, expected cost in the score | `harw-placement-model` (I, planned), depends on `harw-cost` |

### 2.6 Price data source and override (DEC-025)

Precedence, highest first:
1. **Config.** `[pricing]` in `profiles/<p>/models/<m>.toml` or `providers/<p>.toml`, for contract or negotiated rates.
2. **Embedded `prices.toml`.** Every row has `source`, `retrieved` and an optional `valid_until` for promos such as the expired GPT-5.6 Sol discount.
3. **Discovery.** OpenRouter-style `pricing.prompt/completion` (`discovery.rs:57-61`). It has no cache fields, so cache is billed at the input rate, which is conservative.
4. **Unknown.** `Unpriced`: display "n/a"; a `Required` ceiling rejects (DEC-021).

- Decimal values are written as strings and parsed into integer µUSD exactly.
- Rows whose `retrieved` date is older than N days produce a warning in `/usage` and in placement `explain`.

```toml
[[price]]
route = "openai"
model = "gpt-6-luna"
input = "0.10"
cache_read = "0.01"
cache_write = "0.125"
output = "0.50"
source = "https://developers.cloudflare.com/ai/models/openai/gpt-6-luna/"
retrieved = "2026-09-27"
[[price.tier]]
above_prompt_tokens = "unpublished"
input = "0.20"
cache_read = "0.02"
cache_write = "0.25"
output = "0.75"
```

### 2.7 Consumers

- **Session cost state.**
  - `UsageRound` gains `breakdown`, `priced: PricedUsage` and `price_ref` (key plus retrieved date), so costs can be audited and re-priced later.
  - `SessionMeta.cost: CostTotals { total: CostBounds, per_model: BTreeMap<..> }` uses `#[serde(default)]`.
  - `/usage` shows cost per model, the correct hit rate and "prices as of …". This closes item #5 of the claude-code.md matrix (`total_cost_usd`).
- **Budgets and spend limits.**
  - Add `[cost_limit]` with the same shape as AI Gateway spend limits: `limit_usd`, `window_secs`, `technique = fixed|sliding`, and a scope that either partitions or filters by provider, model, tenant, agent or role [SL].
  - Add a non-windowed `CostCeiling{per_request, per_session}` from 66-placement.
  - `CostLimiter` sits next to `ProviderBudgets`:
    - Before sending, it **reserves a worst case**: `prompt_est × max(input, write) + max_output × output`.
    - After the response, `reconcile` settles against the real cost, the same pattern as `BudgetPermit::reconcile` (`budget.rs:870`).
    - Within one process it is strictly consistent. AI Gateway limits are eventually consistent: "a burst of concurrent requests can briefly exceed the limit" [SL].
  - When the limit is hit:
    - A windowed limit waits only if the reset falls inside the DEC-003 wait budget; otherwise it returns `CostExhausted`.
    - A ceiling returns `CostExhausted`, and placement re-places onto a cheaper candidate. This is AI Gateway's "fall back to a cheaper model" pattern [SL].
- **Placement score.**
  - `ProviderOffer.price: Option<ModelPrice>`.
  - The filter checks worst-case cost ≤ `per_request` and ≤ what is left of the grant.
  - The score uses expected cost over a horizon of H turns:
    - warm: `H × (P·h·read + P·(1−h)·write + Δ·input + O·output)`
    - cold: `P·write + (H−1) × warm`
    - where P is the stable prefix, Δ the new tokens per turn, O the output per turn, and h the EWMA hit rate.
  - h comes from a `CacheWarmthLedger`:
    - It is keyed by (tenant, route, model, prefix group), never across tenants (66-placement §7).
    - It is fed by the observed `UsageBreakdown`s.
    - It carries a per-route TTL taken from data.
  - Switching model or provider pays `P × write_rate` once.
- **Work driver and judge.**
  - Add `cost_budget: Option<MicroUsd>` next to `token_budget`, and `BudgetUsageSnapshot.cost_used`.
  - `decide` stops on either budget.
  - Wave reports show cost per worker and per wave, with judge rounds tagged `role=judge`.
  - This makes DEC-002 ("reading is cheap, output is capped") measurable.

## 3. Cloudflare AI Gateway as an optional backend

**What it gives compared with native providers**
- **One ingress shape.** `POST /accounts/{id}/ai/v1/{chat/completions,responses,messages}` plus `/ai/run`, with provider/model ids such as `openai/gpt-5.6-luna` [REST].
  - harw already speaks all three wire formats, so this is mostly profile work: base URL, a Bearer token and `cf-aig-*` headers.
  - Luna 5.6 needs `openai-responses` [L56]. The embedded `cloudflare` provider is `openai-chat` only, so it needs sibling entries for Responses and Messages.
  - Dynamic routing still requires the legacy `/compat/chat/completions` [DR].
- **Server-side spend limits.** Fixed or sliding window, at most 20 rules per gateway, best-effort cost, 429 when exceeded [SL].
- **Estimated cost in logs and OTel** (`gen_ai.usage.cost`) [COST].
- **Custom costs including cache rates** (`cf-aig-custom-cost`) [CC].
- **Metadata tags.** `cf-aig-metadata`, at most 5 entries; `cf.*` keys are reserved [AIGL].
- **Response cache.** Exact match: 0 cost on a hit, TTL up to 1 month, 25 MB per request [AIGL][CACHE].
- **BYOK key storage** [BYOK].
- **Higher Workers AI limits.** 50 instead of 20 requests per minute per account and model for frontier @cf models when paid with credits [WAIL].

**Trade-offs**
- **Cost.** Unified Billing adds +5 % on credits [UB].
- **Silent repricing risk.**
  - Unified-Billing-shaped endpoints consult only the *default*-alias BYOK key; otherwise they fall through silently to Unified Billing [BYOK].
  - Set `cf-aig-no-wholesale: true` (or `byok_only`) so this fails closed with a 400.
  - harw's `BillingBasis` must match the endpoint shape it actually used.
- **Response cache is of little use for agent turns.** Every turn's body differs, so the value stays in the provider's prefix cache, not the gateway cache. The gateway cache is useful for idempotent judge re-runs and retries.
- **Latency.** There is an extra hop. DLP response scanning buffers the whole response, which breaks streaming time-to-first-token. Guardrails adds a second, billed inference call [AIGP].
- **Data path.** Code and prompts pass through Cloudflare. Payloads are logged by default, so harw should send `cf-aig-collect-log-payload: false` by default [LOG]. Zero data retention varies per model (§1).
- **Source of truth.** Gateway cost is "best-effort estimation" [SL][COST]. harw stays authoritative; the gateway numbers are only a cross-check.
- **Terms of service.** Provider ToS plus Cloudflare terms both apply.

**How DEC-003 interacts with gateway limits**
- **Two-level buckets map directly onto harw's.**
  - The Unified Billing cap is 200 requests per 60 s per gateway; it does not apply to BYOK [AIGL]. This becomes the provider-wide `[rate_limit] requests_per_minute`.
  - The @cf paid-model limit of 20 (standard) or 50 (credits) requests per minute per account and model [WAIL] becomes the model-level `[rate_limit]`.
  - `ProviderBudgets` already takes both buckets per request.
- **These are account- or gateway-wide limits, while harw's buckets are per process.**
  - Several harw processes or nodes must split them (ledger, DEC-023).
  - In line with DEC-003, harw must not auto-shard across several gateways to multiply the 200/60 s cap. That would be an operator decision, and the per-account model limits would still apply.
- **One status code, three meanings.** A gateway 429 can mean rate limiting, the Unified Billing cap, or an exhausted spend limit.
  - DEC-003 treats 429 as "wait". A spend-limit 429 can last until the window resets.
  - harw's 5-minute wait budget then runs out, and it must map to `CostExhausted` and a re-place, not a retry storm.
  - Recommendation: set harw's own cost limits *below* the gateway spend limits so the gateway only acts as a backstop.
  - Open question: is a spend-limit 429 distinguishable by body or header?
- **Header pacing only works if the gateway forwards `x-ratelimit-*` / `anthropic-ratelimit-*`.** This is unverified. Until it is confirmed, use `mode = "budget"` on gateway routes.
- **Concurrency.** `max_concurrency` should be at most rpm × average turn duration in minutes. For example, 20 rpm with 60 s turns allows at most about 20 workers.

## 4. Concrete numbers: the cache lever at 98.6 % cache reads

Input-side tokens = fresh + read + write. The 98.6 % share was measured in this session (input to this task). The remaining 1.4 % is priced two ways:
- "lo": billed as fresh input;
- "hi": billed as cache writes.

All figures are computed from §1. Add 5 % when paid through Unified Billing credits.

| Model | No cache | 98.6 % lo | 98.6 % hi | Lever | Compared with Luna 5.6 |
|---|---|---|---|---|---|
| gpt-5.6-luna | 0.2000 | **0.0225** | 0.0232 | 8.9× | 1.0× |
| gpt-6-luna short / long | 0.10 / 0.20 | 0.0113 / 0.0225 | 0.0116 / 0.0232 | 8.9× | 0.5–1.0× |
| deepseek-v4-flash (@cf) | 0.4400 | 0.0200 | 0.0200 | 22× | 0.89× |
| glm-5.3-flash (@cf) | 0.1500 | 0.0317 | 0.0317 | 4.7× | 1.41× |
| kimi-k2.7-code (@cf) | 0.9500 | 0.2006 | 0.2006 | 4.7× | 8.9× |
| claude-sonnet-5 / gpt-6-sol short | 2.0000 | 0.2252 | 0.2322 | 8.9× | 10.0× |
| claude-opus-5.5 | 4.0000 | **0.2532** | 0.2672 | 15.8× | 11.2× |
| gpt-5.6-sol | 2.0000 | 0.2745 | 0.2903 | 7.3× | 12.2× |
| claude-opus-5 | 5.0000 | 0.5630 | 0.5805 | 8.9× | 25.0× |
| claude-fable-5.1 | 10.0000 | 0.3865 | 0.4215 | 25.9× | 17.2× |

**How sensitive this is to the hit rate** (hi case, $ per 1M input-side tokens):

| Model | 98.6 % | 95 % | 90 % | 80 % | 90 % vs 98.6 % |
|---|---|---|---|---|---|
| Luna 5.6 | 0.023 | 0.032 | 0.043 | 0.066 | 1.85× |
| Opus 5.5 | 0.27 | 0.44 | 0.68 | 1.16 | 2.55× |
| Fable 5.1 | 0.42 | 0.86 | 1.48 | 2.70 | 3.5× |
| GLM-5.3 Flash | 0.032 | 0.036 | 0.042 | 0.054 | 1.33× |

The write premium makes cache warmth worth most on flagship models.

**Output decides between the cheap models.**
- GLM-5.3 Flash beats Luna 5.6 only above **1.31 %** output-to-input-side tokens ($0.50 vs $1.20 output).
- DeepSeek V4 Flash beats Luna 5.6 below **2.13 %**.
- GPT-6 Luna in the short tier beats both, but its tier threshold is not published.

**Illustrations** (the output ratio and prefix size are assumptions):
- **A long run with 100M input-side and 1M output tokens:**
  - Luna 5.6: $3.45 to $3.52
  - GLM-5.3 Flash: $3.67
  - DeepSeek V4 Flash: $3.32
  - Sonnet 5: $32.52
  - Opus 5.5: $45.32 (**$420 without the cache**)
  - Fable 5.1: $88.65
- **One judge call per DEC-002 (30k warm prefix plus 256 output tokens):**
  - Luna 5.6: $0.0009
  - Opus 5.5: $0.0111

## 5. Roadmap (small rounds, one agent per file, agents never build)

1. **C1: vocabulary and data.**
   - Work: `harw-cost` (types, `normalize`, `cost`), `prices.toml` with `source`/`retrieved`, the `arch-policy.toml` entry, a DEC-025 note, and fix F2 (the `cache_write_tokens` path).
   - Tests:
     - the AI Gateway inclusive golden case (1000/600/200/100 → 200 fresh);
     - the Anthropic separate case;
     - tier selection, and an `Unpublished` threshold returning a range;
     - the 500 bps surcharge;
     - exactness of picodollar sums;
     - parsing the Luna Responses fixture.
2. **C2: session cost state.**
   - Work: `UsageRound.breakdown` and `priced`, cost totals in the `SessionMeta` sidecar, aggregation over buckets (fix F3), `/usage` cost and hit rate (fix F4), and the `[pricing]` override with its precedence.
   - Tests: a mixed Anthropic plus OpenAI session total, the hit-rate formula, and a sidecar serde round trip.
3. **C3: cost ceilings.**
   - Work: the `[cost_limit]` schema, `CostLimiter` (reserve worst case, then reconcile), `cost_budget` in the work driver, judge tagging, and a DEC-003 amendment (a cost limit is not a 429 wait; the gateway spend limit is a backstop).
   - Tests: reserve/reconcile, windowed fixed versus sliding, and `CostExhausted` → `decide` stops.
4. **C4: placement cost score** (with 66-placement P2).
   - Work: `expected_cost` with `CacheForecast`, `CacheWarmthLedger` (tenant-scoped, TTL, EWMA), the switch penalty, fail closed on unknown prices, and `x-session-affinity` for the direct `cloudflare` route (fix F7).
   - Tests: determinism, "warm Opus 5.5 < cold Opus 5", and no cache affinity across tenants.
5. **C5 (optional): AI Gateway profile.**
   - Work: provider entries for `/ai/v1/responses` and `/ai/v1/messages`, `cf-aig-metadata` (tenant/agent/role, at most 5), `cf-aig-collect-log-payload: false`, `cf-aig-no-wholesale` when BYOK is intended, HIT → 0, and `BillingBasis::Surcharged(500)` for Unified Billing.
   - Tests: header construction and HIT pricing.

Central build after each round, in cloud settings (`CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0`), in this order:
1. `cargo fmt --all`
2. `cargo clippy --workspace -- -D warnings`
3. tests including doc tests
4. `cargo run -q -p xtask -- gates` (the `arch` gate must pass for the new crate)
5. `cargo deny check`
6. `make -C dod clippy test`
7. `actionlint`

**Open questions**
- The token threshold between GPT-6 Luna/Sol short and long pricing is not published.
- Is a spend-limit 429 distinguishable from other 429s?
- Does the gateway pass rate-limit headers through?
- Do Workers AI OpenAI-compatible responses report cache writes, and is their accounting inclusive?
- Is the native OpenAI field name for cache writes `cache_write_tokens`? It is only confirmed in the Cloudflare sample.
- The llama-guard-3-8b price (for Guardrails) is missing.
- The kimi-k3 and gpt-5.6-terra prices were not fetched.

**Sources** (retrieved 2026-09-27; "last updated" where the page shows it)
- [L56] https://developers.cloudflare.com/ai/models/openai/gpt-5.6-luna/
- [L6] https://developers.cloudflare.com/ai/models/openai/gpt-6-luna/
- [S56] https://developers.cloudflare.com/ai/models/openai/gpt-5.6-sol/
- [S6] https://developers.cloudflare.com/ai/models/openai/gpt-6-sol/
- [O55] https://developers.cloudflare.com/ai/models/anthropic/claude-opus-5.5/
- [O5] https://developers.cloudflare.com/ai/models/anthropic/claude-opus-5/
- [SN5] https://developers.cloudflare.com/ai/models/anthropic/claude-sonnet-5/
- [H45] https://developers.cloudflare.com/ai/models/anthropic/claude-haiku-4.5/
- [F51] https://developers.cloudflare.com/ai/models/anthropic/claude-fable-5.1/
- [K27] https://developers.cloudflare.com/workers-ai/models/kimi-k2.7-code/
- [DSF] https://developers.cloudflare.com/workers-ai/models/deepseek-v4-flash-0731/
- [MM3] https://developers.cloudflare.com/ai/models/minimax/m3/
- [GRK] https://developers.cloudflare.com/ai/models/xai/grok-4.6/
- [WAIP] https://developers.cloudflare.com/workers-ai/platform/pricing/ (Sep 17, 2026)
- [WAIL] https://developers.cloudflare.com/workers-ai/platform/limits/ (Sep 17, 2026)
- [CC] https://developers.cloudflare.com/ai-gateway/configuration/custom-costs/ (Sep 9, 2026)
- [UB] https://developers.cloudflare.com/ai-gateway/features/unified-billing/ (Sep 23, 2026)
- [AIGP] https://developers.cloudflare.com/ai-gateway/reference/pricing/ (Sep 24, 2026)
- [AIGL] https://developers.cloudflare.com/ai-gateway/reference/limits/ (Sep 24, 2026)
- [SL] https://developers.cloudflare.com/ai-gateway/features/spend-limits/ (Sep 9, 2026)
- [CACHE] https://developers.cloudflare.com/ai-gateway/features/caching/ (Aug 27, 2026, R)
- [COST] https://developers.cloudflare.com/ai-gateway/observability/costs/ (Sep 24, 2026, R)
- [LOG] https://developers.cloudflare.com/ai-gateway/observability/logging/ (Sep 24, 2026, R)
- [BYOK] https://developers.cloudflare.com/ai-gateway/configuration/bring-your-own-keys/ (Sep 22, 2026, R)
- [DR] https://developers.cloudflare.com/ai-gateway/features/dynamic-routing/ (Aug 7, 2026, R)
- [REST] https://developers.cloudflare.com/changelog/post/2026-05-21-rest-api/
- [AFF] https://developers.cloudflare.com/changelog/post/2026-03-19-kimi-k2-5-workers-ai/ and https://developers.cloudflare.com/changelog/post/2026-03-11-nemotron-3-super-workers-ai/

This was a read-only pass: no files were created or edited, no tests were added, no cargo or rustc was run, and nothing was changed in any Cloudflare account. The central build has nothing new to run. The note fits best as the DEC-025 draft next to `/home/user/Harwness/docs/planning/66-placement/README.md`.

Key code paths:
- `/home/user/Harwness/harw-types/src/usage.rs`
- `/home/user/Harwness/harw-provider-http/src/lib.rs`
- `/home/user/Harwness/harw-provider-http/src/budget.rs`
- `/home/user/Harwness/harw-ops/src/usage.rs`
- `/home/user/Harwness/harw-model-catalog/src/descriptor.rs`
- `/home/user/Harwness/harw-model-catalog/src/vendor_openai.rs`
- `/home/user/Harwness/harw-model-catalog/src/vendor_anthropic.rs`
- `/home/user/Harwness/harw-core/src/state_store.rs`
- `/home/user/Harwness/harw-plan-bridge/src/work_driver.rs`
- `/home/user/Harwness/docs/planning/70-decisions/DEC-003-provider-limits.md`
- `/home/user/Harwness/docs/planning/75-harness-patterns/gateway-contract.md`