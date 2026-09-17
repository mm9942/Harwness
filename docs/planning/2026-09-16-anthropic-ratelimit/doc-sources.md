# Quellenverzeichnis

## Primärquelle (offiziell)
- Anthropic Rate Limits (platform.claude.com): https://platform.claude.com/docs/en/api/rate-limits
  — Org-weite RPM/ITPM/OTPM-Limits, Response-Header-Tabelle, Spend-Cap-Fehler
  ohne `retry-after`, Token-Bucket-Algorithmus.
- Anthropic Legal & Compliance (bereits im Code referenziert):
  https://code.claude.com/docs/en/legal-and-compliance

## Sekundärquellen (Community/Presse, Stand 2026, zur Einordnung der
Durchsetzungs-Timeline, nicht als alleiniger Beleg zu werten)
- https://dev.to/mcrolly/anthropic-kills-claude-subscription-access-for-third-party-tools-like-openclaw-what-it-means-for-3ipc
- https://gigazine.net/gsc_news/en/20260220-anthropic-third-party-block/
- https://www.sovereignmagazine.com/article/anthropic-blocks-openclaw-claude-subscriptions
- https://kkm-mako.com/en/blog/articles/claude-subscription-third-party-tools-blocked/
- https://decodethefuture.org/en/anthropic-blocks-third-party-tools/
- https://geol.ai/briefing/anthropic-blocks-thirdparty-agent-harnesses-for-claude-subscriptions-apr-4-2026-what-it-changes-for
- https://help.apiyi.com/en/anthropic-claude-subscription-third-party-tools-openclaw-policy-en.html
- https://kersai.com/anthropic-killed-third-party-claude-access-heres-every-workaround-that-still-works/
- https://github.com/anthropics/claude-code/issues/33820 (Rate-Limit-Header
  für Hooks/Statuslines)
- https://github.com/anthropics/claude-code/issues/55333
  (`anthropic-ratelimit-unified-5h-*`-Header)

## Interne Quellen (Codebasis, bereits gelesen/verifiziert für diesen Plan)
- `harw-provider-http/src/anthropic.rs:1-120,680-800`
- `harw-provider-http/src/rate_limiter.rs` (komplett)
- `harw-provider-http/src/retry.rs` (komplett)
- `harw-core/src/model.rs:535-661,1027-1105`
- `harw-core/src/turn_loop.rs:1590-1649`
- `harw-tui/src/app.rs:3460-3620`
- `harw-config/src/provider_toml.rs:35-268`
- `harw-cli/src/auth.rs:1-100`
- `docs/remediation/ledger/W3/C-MODEL.md:300-359`
