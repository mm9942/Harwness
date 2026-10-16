# Anthropic lifecycle audit — 2026-10-14

Official lifecycle: https://platform.claude.com/docs/de/about-claude/model-deprecations
Current models: https://platform.claude.com/docs/en/models/overview
Prices: https://platform.claude.com/docs/en/about-claude/pricing

## Newly confirmed

- **Claude Sonnet 4.5**, API ID `claude-sonnet-4-5-20250929`, was deprecated **2026-09-30** and is scheduled to retire **2026-11-30**. Official replacement: `claude-sonnet-5-5`. The 2026-09-30 Harwness snapshot incorrectly lists it as Active; that historical assertion is superseded here.
- **Claude Haiku 5.5**, ID `claude-haiku-5-5`, released **2026-10-07**, is Active, with retirement **not sooner than 2027-10-07**. A support-floor date is NOT an announced retirement.
- `claude-mythos-preview` remains deprecated; replacement `claude-mythos-5`, retirement date not yet announced.

## Pricing and ID semantics

Haiku 5.5 costs **$0.10 / $0.50** input/output per million tokens for prompts up to 100k tokens, **$0.50 / $2.50** above 100k. Opus 5.5 costs **$4 / $20**, Sonnet 5.5 **$2 / $10**. Sonnet 5's introductory $2/$10 became its permanent price. Cache rates and routing-region premiums differ.

Dateless Claude 4.6+ IDs are **pinned snapshots**, not evergreen aliases. Earlier convenience aliases such as `claude-sonnet-4-5` resolve to dated snapshots.

Bedrock and Google Cloud are partner-operated and may have independent model retirement schedules. Do not copy Anthropic first-party dates across platforms.

**Status:** research-only; no runtime change.
