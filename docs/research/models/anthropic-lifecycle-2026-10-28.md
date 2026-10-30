# Anthropic Claude API lifecycle audit — 2026-10-28

> Scope: Anthropic-operated Claude API / Anthropic-managed platforms. **Do not** project lifecycle dates onto partner-operated Bedrock or Google Cloud.
> Research snapshot; checked 2026-10-28 against official model-deprecations, model cards and pricing.

## Official sources

- https://platform.claude.com/docs/en/about-claude/model-deprecations
- https://platform.claude.com/docs/en/about-claude/pricing
- https://platform.claude.com/docs/en/models/sonnet-5-5/overview
- https://platform.claude.com/docs/en/models/sonnet-5/overview
- https://platform.claude.com/docs/en/release-notes/overview

## Confirmed lifecycle

| Anthropic API model ID | Status | Published deadline | Official replacement |
| --- | --- | --- | --- |
| `claude-sonnet-4-5-20250929` | Deprecated on 2026-09-30 | **2026-11-30** | `claude-sonnet-5-5` |
| `claude-mythos-preview` | Deprecated | **No retirement date published** | `claude-mythos-5` |

No additional newly announced October retirement deadline was found on the official Anthropic deprecations page as of this check. `claude-sonnet-5` is described as **Legacy** in its current model card; this is not equivalent to Deprecated and is **not** an announced retirement. It recommends migrating to `claude-sonnet-5-5`.

An **earliest-retirement/support-floor** date is not an executable cutoff. Keep `earliest_retirement_at` separate from `retired_at`, even after the support-floor date has passed.

## Pricing and aliases

Use first-party standard prices rather than accidentally capturing the pricing page's separate Batch table:

| Anthropic API model | Standard input / 1M | Standard output / 1M | Important note |
| --- | ---: | ---: | --- |
| `claude-opus-5-5` | $4 | $20 | Current Opus pricing |
| `claude-sonnet-5-5` | $2 | $10 | Cache reads $0.10/M, half Sonnet 5 cache-read rate |
| `claude-sonnet-5` | $2 | $10 | Legacy, not officially retired |
| `claude-haiku-5-5` | $0.10 | $0.50 | prompts up to 100,000 input tokens |
| `claude-haiku-5-5` | $0.50 | $2.50 | prompts over 100,000 input tokens |

The Batch API advertises **50% off input and output**, so Batch-table values are not standard prices. Cache write, long-context threshold, region and platform billing remain separate dimensions.

### Provider identity

- Anthropic-managed and partner-operated surfaces can expose similar IDs but different lifecycle calendars and regional prices.
- Never auto-rewrite an existing configured Sonnet 4.5 session to Sonnet 5.5; preflight replacement compatibility and keep the atomic switch boundary.

## Request compatibility

Current Claude generations have model-specific request semantics. Harwness should validate request compatibility by **model ID + API surface**, not merely by provider name.

## Harwness reconciliation

Current Harwness `dev` already includes `claude-sonnet-4-5-20250929` deprecation/retirement, `claude-mythos-preview` as deprecated without a retirement date, and a Haiku 5.5 support-floor record in `harw-model-catalog/src/lifecycle.rs`. Explicit live model choices are checked by `harw-ops/src/live_model.rs`. Those are **implemented**, not future proposals.

Follow-up checks:
1. Confirm all model-role and config fallback paths honour the same provider lifecycle source, rather than only a bundled partial table.
2. Validate Sonnet 5.5 migration on provider-level request parameters, cache/read cost and streaming/tool schemas.
3. Keep grandfathered explicit selections distinct from new deprecated selections if that policy is adopted, and verify it in tests.
4. Verify operationally that a rejected retired switch leaves active provider, model and persisted config unchanged.

No newly implemented runtime behavior is claimed by this research file.
