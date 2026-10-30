# Mistral lifecycle audit — 2026-10-28

> Scope: Mistral-operated API / Mistral-hosted offerings. Provider-hosted third-party models remain distinct from their upstream vendor records.
> Research snapshot checked 2026-10-28.

## Official sources

- https://docs.mistral.ai/getting-started/models/models_overview
- https://docs.mistral.ai/getting-started/models/deprecated_models
- https://docs.mistral.ai/deployment/ai-studio/tier
- https://mistral.ai/pricing
- Customer deprecation notice received 2026-09-30 and recorded in `mistral-serverless-deprecations-2026-09-30.md`

## Immediate lifecycle gate

The provider notice recorded in this repository says:

| Mistral-hosted model | Retirement | Replacement |
| --- | --- | --- |
| `mistral-ocr-4-0` | 2026-09-30 | `mistral-ocr-4-1`, `mistral-ocr-4-latest`, `mistral-ocr-4` |
| `labs-leanstral-1-5` | 2026-09-30 | none announced |
| `zai-glm-5-2` | **2026-10-31** | `zai-glm-5-3`, `zai-glm-latest`, `zai-glm-5` |

As of 2026-10-28, **Mistral-hosted `zai-glm-5-2` has three days remaining before its published retirement date**. This is the highest-priority migration gate in this audit.

Do **not** transfer this deadline or Mistral pricing onto direct Z.AI `glm-5.2`. Harwness already keys lifecycle records by `(provider, model)`, and its bundled table intentionally has no direct-Z.AI entry for this Mistral retirement.

## Current first-party model surface

The official model overview currently advertises Mistral 3 / Ministral 3 families and specialist models including OCR 3, Voxtral and Codestral. The provider's model-overview page also exposes versioned aliases; treat aliases as provider-specific discovery metadata rather than rewriting persisted pins.

The canonical [Models page](https://docs.mistral.ai/models) lists Devstral 2 (`devstral-2512`) under deprecated and retired models; it is not an active-model example. Its exact retirement instant remains unverified here.

The public deprecation table remains useful for older first-party model families. Harwness should ingest official deprecation status without assuming that a model absent from the overview has a particular retirement instant unless Mistral publishes one.

## Pricing

Mistral pricing is provider-offering-specific. Preserve pricing in the Mistral provider record and do not merge it into upstream model identity.

From the September customer notice:
- Mistral-hosted GLM 5.2 / replacement GLM 5.3: **$1.40/M input, $0.14/M cached input, $4.40/M output**.
- Mistral OCR 4.1: **$4 / 1,000 pages**, Batch **$2 / 1,000 pages**.
- Document AI with OCR 4.1: **$5 / 1,000 pages**.
- Mistral states a **50% Batch API discount** for models.

For general first-party model prices, use the current Mistral pricing page at refresh time; do not freeze a global price onto an upstream family because Mistral-hosted and third-party surfaces can differ.

## Harwness reconciliation

Current `dev` has a real `harw-model-catalog::lifecycle` table containing:
- retired Mistral OCR 4.0 -> OCR 4.1;
- retired Leanstral 1.5;
- Mistral-hosted `zai-glm-5-2` -> `zai-glm-5-3`, retirement 2026-10-31;
- explicit lifecycle enforcement that rejects retired choices without silent substitution.

The direct Z.AI namespace is intentionally absent from these Mistral lifecycle entries. Preserve that separation.

## Migration gate — before 2026-10-31

1. Search resolved configs, defaults and internal-role pins for provider=`mistral` + model=`zai-glm-5-2`.
2. Migrate those pins to `zai-glm-5-3` after compatibility validation.
3. Do not touch direct Z.AI `glm-5.2` solely because of this Mistral notice.
4. Preserve active-session atomicity: no transparent substitution during an in-flight session.
5. Verify that stale discovery/cache data cannot reintroduce the retired offering as an implicit route after the cutoff.

No newly implemented runtime behavior is claimed by this research file.
