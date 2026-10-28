# OpenAI API lifecycle audit — 2026-10-28

> Provider scope: first-party **OpenAI API** only. Status: research snapshot, not executable catalog data.
> Verified against official documentation on 2026-10-28.
> Previous baseline: `openai-lifecycle-2026-10-14.md` and `openai-pricing-2026-10-14.md`.

## Primary sources

- https://developers.openai.com/api/docs/deprecations
- https://developers.openai.com/api/docs/models
- https://developers.openai.com/api/docs/pricing
- https://developers.openai.com/api/docs/models/compare

## Immediate change since the last audit

**2026-10-23 has passed.** OpenAI says these legacy API offers have now shut down:

| Model IDs / aliases (examples) | Shutdown | Provider-recommended replacement |
| --- | --- | --- |
| `gpt-3.5-turbo`, `gpt-3.5-turbo-0125` | 2026-10-23 | `gpt-5.6-terra` |
| `gpt-4`, `gpt-4-0613`, `gpt-4-turbo`, `gpt-4-turbo-2024-04-09` | 2026-10-23 | `gpt-5.6-sol` |
| `gpt-4.1-nano`, `gpt-4.1-nano-2025-04-14` | 2026-10-23 | `gpt-5.6-luna` |
| `gpt-4o-2024-05-13` | 2026-10-23 | `gpt-5.6-sol` |
| `o1`, `o1-2024-12-17`, `o3-mini`, `o3-mini-2025-01-31` | 2026-10-23 | `gpt-5.6-sol` |
| `o1-pro`, `o1-pro-2025-03-19` | 2026-10-23 | `gpt-5.6-sol`, with `reasoning.mode: pro` |
| `o4-mini`, `o4-mini-2025-04-16`, `ft-o4-mini-2025-04-16` | 2026-10-23 | `gpt-5.6-terra` |
| `gpt-image-1` | 2026-10-23 | `gpt-image-2.5-sunburst` or `gpt-image-2.5-flare` |

This is a representative operational list, not a replacement for the complete official model and fine-tune table. Fine-tuned derivatives follow the provider's base-model shutdown schedule. Do not assume `GPT-4o` as a family is entirely retired: `gpt-4o-2024-08-06` and `gpt-4o-2024-11-20` have a separate 2027-01-14 deadline.

## Upcoming milestones

| Deadline | Scope | Replacement / action |
| --- | --- | --- |
| **2026-10-31** | Existing OpenAI Evals become read-only | Migrate Evals workflow separately; not a model lifecycle entry |
| **2026-11-30** | `v1/prompts` reusable prompts and Evals dashboard/API shutdown | Provider capability/service lifecycle; not model-ID retirement |
| **2026-12-01** | `gpt-image-1-mini`, `gpt-image-1.5`, `chatgpt-image-latest` | `gpt-image-2.5-sunburst` / `gpt-image-2.5-flare` |
| **2026-12-11** | `gpt-5`/`gpt-5-2025-08-07`, `o3`/`o3-2025-04-16` | `gpt-5.6-sol` |
| **2026-12-11** | `gpt-5-mini`/`gpt-5-mini-2025-08-07` | `gpt-5.6-terra` |
| **2026-12-11** | `gpt-5-nano`/`gpt-5-nano-2025-08-07` | `gpt-5.6-luna` |
| **2026-12-11** | `gpt-5-pro` and `o3-pro` snapshots | `gpt-5.6-sol` with `reasoning.mode: pro` |
| **2027-01-06** | `tts-1`, `tts-1-hd`, `gpt-4o-mini-tts` and its 2025-03-20 / 2025-12-15 snapshots | `gpt-realtime-2.1-mini`; validate TTS-to-realtime API/protocol migration, not only the model string |
| **2027-01-14** | `gpt-4o-2024-08-06`, `gpt-4o-2024-11-20`, `gpt-4o-mini-2024-07-18`, `gpt-4.1-2025-04-14`, `gpt-4.1-mini-2025-04-14` | `gpt-6-luna`; fine-tunes follow base model |
| **2027-01-20** | Older realtime/audio families | `gpt-realtime-2.1`, `gpt-realtime-2.1-mini`, or `gpt-audio-1.5` as specified per model |
| **2027-02-26** | Older Whisper/transcription families | `gpt-live-transcribe` or `gpt-transcribe` depending on use case |
| **2027-04-01** | `gpt-5.3-codex` | `gpt-6-sol` |
| **2027-04-01** | `gpt-5.4-nano` and `gpt-5.4-nano-2026-03-17` | `gpt-6-luna` |
| **2027-04-01** | `gpt-5.1` and `gpt-5.1-2025-11-13` | `gpt-6-sol` |

## API discovery contract

OpenAI's official deprecation reference now documents `GET /v1/models` and `GET /v1/models/{model}` with optional **`shutdown_date`** (a `YYYY-MM-DD` calendar string or null). This is a direct provider signal suitable for an authenticated lifecycle refresh. Store its provenance and discovery time; do not assign UTC midnight to a date-only value or silently rewrite pinned IDs. Published replacement suggestions still require a curated source.

## Current model/pricing observations (not a new retirement)

The official current model catalog lists `gpt-6.1-terra` (not in the 2026-10-14 Harwness price snapshot), `gpt-6.1-sol` and `gpt-6-luna`. **Standard tier, short context, USD/1M tokens**:

| Model | Input | Cached input | 30m cache write | Output |
| --- | ---: | ---: | ---: | ---: |
| `gpt-6.1-sol` | $2 | $0.10 | $2.50 | $10 |
| `gpt-6.1-terra` | $1 | $0.05 | $1.25 | $5 |
| `gpt-6-luna` | $0.10 | $0.01 | $0.125 | $0.50 |

The 2026-10-14 Harwness snapshot's `gpt-6.1-sol` $1/$5 and `gpt-6-luna` $0.05/$0.25 values correspond to **Flex/Batch**, not Standard. Record pricing as `provider + offering + service tier + context band + cache operation + region`; the regional processing surcharge and long-context pricing differ. The official model listing is not an entitlement guarantee for a given account.

## Integration guardrails

- Harvest `shutdown_date` with provider credentials and keep a last-known-good cache, without logging credentials.
- Exclude shut-down offers from implicit selection and reject explicit retired selections atomically, with replacement hints.
- Normalize aliases and pinned snapshots separately; never infer a family-level shutdown from one retired snapshot.
- Store non-model lifecycle events (Evals, `v1/prompts`) in a distinct provider-service record.
- For TTS, realtime, transcription and images, test endpoint semantics, tool compatibility and modalities; a textual substitution alone is not sufficient.

No code changes or integration tests are claimed by this research snapshot.
