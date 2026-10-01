# Anthropic Claude lifecycle snapshot — 2026-09-30

Primary source: https://platform.claude.com/docs/en/about-claude/model-deprecations

This file captures time-sensitive Anthropic API lifecycle information separately from the general `anthropic.json` capability catalog.

## Current status

Anthropic defines Active, Legacy, Deprecated, and Retired model states. Deprecated models continue to work until their retirement date; retired-model requests fail.

### Deprecated

- `claude-mythos-preview`
  - deprecated: 2026-06-09
  - retirement: to be announced
  - recommended migration: `claude-mythos-5`

### Active models with documented earliest retirement bounds

These are **not retirement dates**. Anthropic currently marks the models Active and states only that retirement will be no sooner than the date shown.

- `claude-sonnet-4-5-20250929`: not sooner than 2026-09-29
- `claude-haiku-4-5-20251001`: not sooner than 2026-10-15
- `claude-opus-4-5-20251101`: not sooner than 2026-11-24
- `claude-opus-4-6`: not sooner than 2027-02-05
- `claude-sonnet-4-6`: not sooner than 2027-02-17
- `claude-opus-4-7`: not sooner than 2027-04-16
- `claude-opus-4-8`: not sooner than 2027-05-28
- `claude-fable-5`: not sooner than 2027-06-09
- `claude-mythos-5`: not sooner than 2027-06-09
- `claude-sonnet-5`: not sooner than 2027-06-30
- `claude-opus-5`: not sooner than 2027-07-24
- `claude-fable-5-1`: not sooner than 2027-09-01
- `claude-mythos-5-1`: not sooner than 2027-09-01
- `claude-opus-5-5`: not sooner than 2027-09-22
- `claude-sonnet-5-5`: not sooner than 2027-09-28

Because 2026-09-30 is already later than the earliest-retirement bound for Sonnet 4.5, Harwness must **not** infer that it is retired. The authoritative status remains Active until Anthropic publishes a deprecation/retirement event.

## Recently retired

- `claude-opus-4-1-20250805`
  - deprecated: 2026-06-05
  - retired: 2026-08-05
  - replacement: `claude-opus-4-8`
- `claude-sonnet-4-20250514`
  - deprecated: 2026-04-14
  - retired: 2026-06-15
  - replacement: `claude-sonnet-4-6`
- `claude-opus-4-20250514`
  - deprecated: 2026-04-14
  - retired: 2026-06-15
  - replacement: `claude-opus-4-8`
- `claude-3-haiku-20240307`
  - retired: 2026-04-20
  - replacement: `claude-haiku-4-5-20251001`
- `claude-3-5-haiku-20241022`
  - retired: 2026-02-19
  - replacement: `claude-haiku-4-5-20251001`
- `claude-3-7-sonnet-20250219`
  - retired: 2026-02-19
  - replacement: `claude-sonnet-4-6`

## Parameter lifecycle

For Claude Opus 4.7 and later, `temperature`, `top_p`, and `top_k` are deprecated. Non-default values return HTTP 400 for those models. Harwness provider adapters must therefore scope parameter compatibility by model generation instead of assuming a provider-wide request shape.

## Harwness implications

- Represent a provider lifecycle status independently of model capability metadata.
- Distinguish a true `retired_at` value from an `earliest_retirement_at` / support-floor date.
- Never infer retirement from an elapsed support-floor date.
- Carry recommended replacement IDs where Anthropic publishes them.
- Add model-scoped parameter compatibility to provider request validation.
- Retired Claude snapshots must not be returned by implicit routing.
