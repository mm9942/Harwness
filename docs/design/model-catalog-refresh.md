# Provider model refresh and persistent setup

> Status: implemented · Last reviewed: 2026-09-24

The bundled provider lists were refreshed from https://models.dev/api.json on
2026-09-11. Both interactive onboarding and `harw catalog` load current lists
through the models.dev cache. The cache lives at `<HARW_HOME>/cache/models_dev.json`
and expires after one hour. `harw catalog --refresh` bypasses that TTL.
Refresh happens on access, not through a background daemon.

Provider aliases map harness IDs to upstream IDs (including Cloudflare,
Together, Fireworks, Moonshot, Zhipu, DashScope and Gemini). Explicitly deprecated
models and models without text output are excluded. Local Ollama/LM Studio,
custom endpoints and Foundry deployments retain their installation-specific
fallbacks; models.dev is not an inventory of those deployments or account access.

Responses are parsed before replacing the cache. On network or parse failure,
the last valid cache remains usable; without one, the bundled catalog remains.
Future cache timestamps do not prevent refresh. Runtime behavior profiles and
credentials are not supplied by the remote catalog.

Setup persists provider and model definitions, defaults and completion flags in
the active profile. API model IDs remain unchanged in TOML; filenames encode
path separators and percent signs. Repeating setup with the same credential
reference and endpoint does not duplicate the credential-pool entry. Repository
configs inherit omitted setup defaults and onboarding state; explicit project
values still take precedence. Cancelling the terminal picker aborts setup.

Regression tests cover namespaced model persistence and reload, repeated setup,
repository overrides, provider aliases, cache refresh, malformed responses and
offline fallback.

The HTTP router also accepts existing profiles whose provider `models` list is
empty: it chooses the lexicographically first discovered model definition owned
by that provider. An explicit provider list retains precedence; the active
provider retains the harness default. Setup now writes the selected model into
the provider list and prints the persisted profile and selection on completion.
