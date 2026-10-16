# Migration gates — October 2026

Priority 1: Check configured OpenAI October 23 legacy models. Move to published GPT-5.6 replacements; validate tool schemas, token costs and API compatibility.

Priority 2: Before October 31 replace Mistral-hosted `zai-glm-5-2` with `zai-glm-5-3`. Do not change direct Z.AI.

Priority 3: Before November 30 replace Anthropic `claude-sonnet-4-5-20250929` with `claude-sonnet-5-5`.

Also track October 14 announcement: GPT-4o and GPT-4.1 snapshots retire January 14, 2027, replacement `gpt-6-luna`.

Tests must keep existing and new explicit deprecated configurations distinct, preserve atomic model switching, and avoid assuming UTC midnight on provider date-only notices.
