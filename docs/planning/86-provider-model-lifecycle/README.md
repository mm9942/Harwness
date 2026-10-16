# Provider-aware model lifecycle and retirement handling

> Status: planned
> Source date: 2026-09-30
> Base: `dev@23e224703120bc0e4f73dec028dfb29da8881329`

## Scope

Turn current provider deprecation notices into a concrete Harwness migration and lifecycle-handling work package. Initial providers: Mistral Serverless, OpenAI API, and Anthropic Claude API.

## CURRENT

Harwness already separates provider protocol, declared model metadata, runtime policy, and observed behavior in the model catalog. The catalog refresh path can consume models.dev and already excludes models marked deprecated there.

Relevant implemented references:

- `docs/design/model-catalog-v2.md`
- `docs/design/model-catalog-refresh.md`
- `docs/architecture/model-provider-routing.md`
- `docs/research/models/mistral.json`
- `docs/research/models/zai.json`

The bundled Z.AI research record currently contains GLM-5.2 as GA. That record describes Z.AI directly and must not be overwritten with Mistral-hosted pricing/lifecycle data: provider-specific lifecycle and pricing can differ for the same upstream model.

## PROVIDER NOTICE

Mistral Serverless API announced:

| Model on Mistral Serverless | Retirement | Replacement | Pricing note |
| --- | --- | --- | --- |
| `mistral-ocr-4-0` | 2026-09-30 | `mistral-ocr-4-1`, `mistral-ocr-4-latest`, `mistral-ocr-4` | $4 / 1,000 pages; Batch $2 / 1,000 pages; Document AI OCR 4.1 $5 / 1,000 pages |
| `labs-leanstral-1-5` | 2026-09-30 | none announced | Labs/experimental model retired |
| `zai-glm-5-2` | 2026-10-31 | `zai-glm-5-3`, `zai-glm-latest`, `zai-glm-5` | $1.40/M input, $0.14/M cached input, $4.40/M output |

Mistral also states a 50% Batch API discount across models.

## TARGET

Treat lifecycle as a property of a provider/model offering rather than only of an upstream model family.

Required behavior:

1. Provider-specific aliases resolve to canonical provider model IDs.
2. Retired offerings are never selected implicitly.
3. Deprecated offerings remain addressable only where an existing explicit configuration requires backward compatibility, with a warning before retirement and a hard failure after retirement.
4. Replacement metadata is machine-readable.
5. Catalog refresh preserves the last valid provider lifecycle state when remote refresh fails.
6. Pricing remains scoped to the provider offering; do not merge Mistral-hosted GLM pricing into the direct Z.AI record.
7. `*-latest` aliases may be exposed for user convenience, but persisted configuration should prefer pinned concrete IDs where reproducibility matters.

## DELTA

### D1 — Provider-offering lifecycle metadata

Extend the model-catalog representation (or add a provider-offering overlay) with fields equivalent to:

```rust
pub struct ProviderLifecycle {
    pub deprecated_at: Option<OffsetDateTime>,
    pub retired_at: Option<OffsetDateTime>,
    pub replacements: Vec<ModelId>,
}
```

Do not collapse upstream-model identity and provider-hosted identity.

### D2 — Alias mapping

For the Mistral provider, add/verify aliases:

- `mistral-ocr-4` -> current OCR 4.x provider target
- `mistral-ocr-4-latest` -> provider current target
- `zai-glm-5` -> current Mistral-hosted GLM 5.x target
- `zai-glm-latest` -> provider current target

Pinned aliases must not silently rewrite stored explicit model IDs.

### D3 — Retirement enforcement

Selection/router behavior:

- before retirement: explicit deprecated model allowed with warning;
- at/after retirement: implicit selection excluded;
- at/after retirement: explicit selection fails with a structured error containing replacement IDs;
- no transparent substitution during an active session.

This preserves Harwness' atomic model/provider switching semantics.

### D4 — Source refresh

Add a Mistral Serverless lifecycle source beside the existing research files. Keep direct-Z.AI metadata independent.

### D5 — Tests

Add regression coverage for:

- deprecated-but-not-retired explicit selection;
- retired implicit exclusion;
- retired explicit selection error with replacements;
- alias resolution;
- provider-specific pricing/lifecycle isolation;
- stale-cache fallback;
- no mutation of an active session when a retired model switch fails.

## MIGRATION

Immediate:

- stop using `mistral-ocr-4-0`;
- stop using `labs-leanstral-1-5`;
- migrate OCR defaults/configuration to `mistral-ocr-4-1` where present.

Before 2026-10-31:

- find Mistral-hosted `zai-glm-5-2` configurations;
- move pinned configurations to `zai-glm-5-3`;
- keep direct Z.AI `glm-5.2` untouched unless its own provider lifecycle changes.

## ACCEPTANCE CRITERIA

- No bundled or discovered Mistral Serverless default selects the two models retired on 2026-09-30.
- Mistral-hosted `zai-glm-5-2` is represented as retiring on 2026-10-31 with replacement IDs.
- Direct Z.AI GLM metadata remains provider-distinct.
- Failed switches to retired models are atomic and leave session state unchanged.
- Replacement suggestions are surfaced in CLI/TUI model-selection errors.
- Tests demonstrate lifecycle behavior independent of network availability.

## IMPLEMENTATION STATUS

Planning only in this PR. No runtime behavior is claimed as implemented.

## REFERENCES

- Provider notice captured in `docs/research/models/mistral-serverless-deprecations-2026-09-30.md`.
- Existing Harwness model architecture: `docs/design/model-catalog-v2.md`.
- Existing refresh behavior: `docs/design/model-catalog-refresh.md`.
- Existing atomic switch semantics: `docs/architecture/model-provider-routing.md`.


## OPENAI SNAPSHOT — 2026-09-30

Official OpenAI API deprecations show several near-term deadlines that should be represented in provider-scoped lifecycle metadata:

- 2026-10-01: `gpt-5.4-cyber` -> `gpt-5.6-cyber`.
- 2026-10-23: multiple legacy GPT/o-series snapshots and fine-tuned families shut down.
- 2026-11-30: reusable prompt objects / `v1/prompts` shut down (provider-capability lifecycle, not model lifecycle).
- 2026-12-01: older GPT Image models shut down.
- 2026-12-11: older GPT-5 and o3 snapshots shut down.
- 2027-01-20: legacy audio/realtime families shut down.
- 2027-02-26: older transcription families shut down.

Additionally, `gpt-3.5-turbo-instruct`, `babbage-002`, `davinci-002`, and `gpt-3.5-turbo-1106` were shut down on 2026-09-28 and must not remain implicit candidates.

Full snapshot: `docs/research/models/openai-lifecycle-2026-09-30.md`.

OpenAI lifecycle policy also has different minimum notice windows for GA, specialized GA variants, and Preview models. The catalog should therefore preserve lifecycle class/risk rather than treating every model identifier identically.

## ANTHROPIC SNAPSHOT — 2026-09-30

Anthropic currently marks `claude-mythos-preview` Deprecated, with migration to `claude-mythos-5`; its retirement date is still to be announced.

Anthropic also publishes "not sooner than" dates for Active models. These are support-floor dates, **not retirement dates**. For example, `claude-sonnet-4-5-20250929` remains Active even though its published floor (2026-09-29) has elapsed. Harwness must never infer retirement merely because a support-floor date is in the past.

Recently retired snapshots include Claude Opus 4.1, Claude Sonnet 4, Claude Opus 4, Claude 3.7 Sonnet, Claude 3.5 Haiku, and Claude 3 Haiku; their documented replacements are captured in the snapshot.

Anthropic also deprecates request parameters by model generation: `temperature`, `top_p`, and `top_k` are deprecated for Claude Opus 4.7 and later, and non-default values can return HTTP 400. Provider request compatibility therefore needs model-scoped parameter rules.

Full snapshot: `docs/research/models/anthropic-lifecycle-2026-09-30.md`.

## GENERALIZED DATA MODEL DELTA

The initial lifecycle overlay should distinguish at least:

```rust
pub struct ProviderLifecycle {
    pub deprecated_at: Option<OffsetDateTime>,
    pub retired_at: Option<OffsetDateTime>,
    pub earliest_retirement_at: Option<OffsetDateTime>,
    pub replacements: Vec<ModelId>,
}
```

Semantics:

- `deprecated_at`: provider has formally announced deprecation.
- `retired_at`: provider has announced a hard shutdown/retirement timestamp.
- `earliest_retirement_at`: support floor / "not sooner than" date; passing it does **not** change status by itself.
- `replacements`: provider-published migration targets.

Provider/API capabilities that are not model IDs (for example OpenAI `v1/prompts`) should use a sibling lifecycle record instead of being forced into `ModelDescriptor`.

Model-specific request compatibility should also be represented independently from lifecycle so adapters can reject unsupported/deprecated parameter combinations before network dispatch.

## SCHEDULED MAINTENANCE

A recurring two-week provider lifecycle review is configured for OpenAI, Anthropic, and Mistral. Each run should:

1. re-check official provider lifecycle/deprecation sources;
2. compare against the current Harwness `dev` branch;
3. update provider lifecycle snapshots;
4. update/create a Draft PR against `dev`;
5. never infer retirement from aliases, stale catalog data, or elapsed support-floor dates.
