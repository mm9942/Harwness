# Mistral Serverless model retirement handling

> Status: planned
> Source date: 2026-09-30
> Base: `dev@23e224703120bc0e4f73dec028dfb29da8881329`

## Scope

Turn the 2026-09-30 Mistral Serverless deprecation notice into a concrete Harwness migration and lifecycle-handling work package.

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
