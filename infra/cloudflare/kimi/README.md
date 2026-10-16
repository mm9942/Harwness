# Kimi Cloudflare AI Worker

This directory mirrors the live Kimi Worker used by Harwness.

## Routing

The Worker keeps its existing OpenAI Chat Completions / Responses adapters,
tool mapping, reasoning translation, streaming, and telemetry, but routing is
gateway-first:

- one normal Workers AI binding (`AI`) is sufficient for the active route pool;
- 19 AI Gateway IDs form a deterministic sticky ring;
- affinity is derived from the existing Harwness/session affinity contract;
- route-local Gateway limits and transient 5xx errors may fail over across at
  most three gateway IDs;
- Workers AI shared capacity/account limits do not fan out across the ring;
- GLM 5.3 remains intentionally attributable to `more-exessive-work`, but
  now uses the normal `AI` binding rather than a special capacity binding.

The additional historical AI bindings remain configured in Cloudflare for
rollback compatibility, but they do not represent independent Workers AI
account quota.

## Gateway pool

`default`, `claw`, `clawd`, `cyberclaw`, `whisper`,
`sgh-chatbot`, `agentic`, `more-exessive-work`,
`new-worker-of-the-day`, `new-worker-of-the-day-5`,
`new-worker-of-the-week`, `new-worker-of-the-month`,
`new-worker-of-the-year`, `new-worker-of-the-century`,
`new-worker-of-the-millennium`, `new-worker-of-the-eon`,
`new-worker-of-the-eternity`, `new-worker-of-the-universe`,
`new-worker-of-the-multiverse`.

The `new-worker-*` family is configured with a 1800s Gateway cache TTL,
cache invalidation on update, two 250ms exponential retries, payload logging
disabled by Worker request headers, and no artificial Gateway RPM throttle.

## Live deployment

At the time this mirror was created, the live Worker deployment was version
`9e36d79b-72ea-4fdc-9d0c-d07023e60db7`, deployment
`3c689734-fef7-4984-b81c-53520855b363`.

Worker invocation logs are enabled and query strings are redacted. Cloudflare
did not enable trace sampling even though it was requested through the Worker
configuration API, so traces should be treated as unavailable until verified.
