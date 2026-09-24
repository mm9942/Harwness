# Design: Multi-Provider Onboarding with a ratatui Setup Screen

> Status: implemented · Last reviewed: 2026-09-24

**Scope decision**: catalog + TUI, auth = **API key + multi-key pool + reuse of
local auth files** (`~/.codex`, `~/.claude`, …). Live use of pure OAuth token
sources was left for a follow-up (detection/import was already in scope).

This document is kept as the design record; the pieces it describes are built —
see the file list at the end and the status notes inline. It predates the
`matrix-game-master` orchestrator work and other later additions and does not
cover them.

## Context

The original `harw-cli/src/onboarding.rs` set up a single provider (OpenAI)
through line-based prompts. The goal was "any provider, with TUI-guided
setup": a searchable provider catalog, a two-stage ratatui picker
(provider → auth → model), and multi-key credential rotation.

## Decision: catalog strategy

**An embedded, curated catalog as the base (offline, deterministic), plus
optional models.dev enrichment of the model lists.**

- `providers.toml` is **embedded in the binary** (`include_str!`) — around 18
  common API-key-based providers. Works offline, no hard network dependency,
  reproducible.
- `models.dev` (optional, key-gated) enriches only the **model lists** and is
  cached to `~/.harw/cache/models_dev.json` (mtime TTL of 1h, a static
  fallback when offline or on error).

Rationale: supporting "any provider" needs reach (models.dev), but a
standalone harness must not depend on the network during setup — the
embedded catalog guarantees a working offline path.

## Data model

### Provider catalog (`harw-model-catalog`)

```rust
pub enum ProviderApi { OpenAiResponses, OpenAiChat, AnthropicMessages, Ollama }

pub enum AuthMethod {
    ApiKey { env_vars: Vec<String> }, // precedence order
    LocalImport { sources: Vec<String> }, // existing local auth files (see below)
    LocalBaseUrl,                     // ollama/lmstudio: no secret
    Custom,                           // any compatible endpoint
}

pub struct ProviderSpec {
    pub id: String,            // "openrouter"
    pub name: String,          // "OpenRouter"
    pub base_url: String,
    pub api: ProviderApi,
    pub auth: Vec<AuthMethod>,
    pub default_model: Option<String>,
    pub featured: bool,        // short list in the picker
    pub models: Vec<String>,   // static fallback; models.dev supplements it
}

pub fn embedded_catalog() -> Vec<ProviderSpec>;              // include_str!("providers.toml")
pub fn enrich_from_models_dev(cache_dir: &Path, key: Option<&SecretString>) -> ...; // optional, TTL cache
```

Curated base (~18): openai, anthropic, openrouter, groq, deepinfra, together,
fireworks, xai, mistral, deepseek, moonshot (kimi), zhipu (glm), cerebras,
nebius, perplexity, cloudflare, ollama (local), lmstudio (local), plus
`custom`.

### Credential store (`auth.toml`, in `harw-config/src/auth_toml.rs`)

`AuthConfig` keeps `credentials: HashMap<String, SecretRef>` and adds:

```rust
#[serde(default)]
pub credential_pool: HashMap<String, Vec<CredentialEntry>>, // key = provider id

pub struct CredentialEntry {
    pub secret: SecretRef,          // env:/file: — never plaintext
    #[serde(default)] pub label: Option<String>,
    #[serde(default)] pub priority: u32,
    #[serde(default)] pub base_url: Option<String>,
}
```

Resolution follows **precedence env → local import source → SecretRef pool →
(later) a dedicated store**, with round-robin across equal-priority entries.

### Local auth sources (`harw-model-catalog/src/sources.rs`)

Instead of (or in addition to) a manually entered API key, setup can **reuse
existing local auth files** — exactly what `~/.codex/auth.json`,
`~/.claude/.credentials.json`, and similar files are for. An embedded source
table:

```rust
pub enum ExtractRule {
    JsonPointer(String), // e.g. "/OPENAI_API_KEY" or "/tokens/access_token"
    EnvVar(String),      // the value is in an env var the file sets
    WholeFile,           // the entire (trimmed) file is the secret
}

pub struct CredentialSource {
    pub id: String,        // "codex", "claude-cli", "gh-cli"
    pub provider: String,  // which catalog provider it matches
    pub path: String,      // "~/.codex/auth.json"
    pub extract: ExtractRule,
    pub kind: SourceKind,  // ApiKey | OAuthToken
}

pub fn detect_local_sources(catalog_provider: &str) -> Vec<DetectedCredential>;
```

Known starter sources: `~/.codex/auth.json` (`/OPENAI_API_KEY` or an OAuth
`tokens` object), `~/.claude/.credentials.json` (Anthropic OAuth),
`~/.config/gh/hosts.yml` (only where relevant), plus generic `env:`
candidates from each provider's declared env vars.

**Import semantics**: `detect_local_sources` reads the file read-only and
extracts via `ExtractRule`. Confirming a match in the picker **copies or
rewrites nothing** — instead it writes a `CredentialEntry` with a precise
`SecretRef` into the pool, using a `SecretRef` pointer kind
**`file-json:PATH#/json/pointer`** (reads exactly one JSON field rather than
the whole file), so rotation/refresh of the source still works. `kind =
OAuthToken` is detected and marked: usable once the matching bridge API
(`chatgpt-backend`/`anthropic-oauth`) is implemented; `kind = ApiKey` is
immediately usable through the existing `openai-chat`/`openai-responses`
path.

## Provider bridge (`harw-provider-http`)

The adapter speaks both the OpenAI-style **Responses** API (`/responses`)
and **Chat Completions** (`/chat/completions`, `openai-chat`), chosen per
provider via the `ProviderApi` field from the catalog — most third-party
providers (OpenRouter, Groq, DeepInfra, xAI, and others) speak
`chat/completions`. Anthropic Messages and Ollama are separate, existing
integrations in the same provider layer.

## The ratatui setup screen (`harw-tui/src/setup.rs`)

A `SetupApp`, alongside the main chat app:

- **Stage 1 — provider**: a searchable list (featured entries first, then
  `More…`, `Custom`, `Keep current`, `Skip` as sentinels), fuzzy-filters as
  you type, with viewport scrolling. Configured providers show a
  "(configured)" suffix.
- **Stage 2 — auth**: depends on the provider's `AuthMethod`. The picker
  lists **detected local sources** first (`detect_local_sources`, e.g. "use
  from ~/.codex/auth.json"), then "enter an API key" (masked), then a
  base-URL confirmation for local providers. An error re-prompts rather than
  aborting.
- **Stage 3 — model**: a list from the catalog (plus models.dev, when a key
  is available), with the default preselected.
- Result: `SetupOutcome { provider_id, base_url, api, model, secret_ref }`.
  An I/O-free, testable state machine (`stage`, `selected`, `filter`,
  `scroll`), driven entirely through `SetupApp::on_key`.

## Wiring (`onboarding.rs`)

- Interactive: `harw_tui::run_setup(catalog)` → `SetupOutcome` → writes
  `providers/<id>.toml`, `models/<id>.toml`, a `credential_pool` entry in
  `auth.toml`, sets `default_provider`/`default_model` and
  `onboarding.seen`.
- Non-interactive (`HARW_ONBOARD_NONINTERACTIVE`): unchanged, pure defaults
  (openai).
- Key persistence as before: an `env:` ref is preferred; direct entry falls
  back to a `file:` ref under `<home>/secrets/<id>.key` (chmod 600).

## Files

`harw-model-catalog/` (`lib.rs`, `spec.rs`, `models_dev.rs`, `sources.rs`,
`providers.toml`, `error.rs`); `harw-config/src/auth_toml.rs`
(`credential_pool`, the `file-json:` `SecretRef` kind); `harw-provider-http/`
(the `openai-chat` transport, transport selection by `api`); `harw-tui/src/`
(`setup.rs`, the `SetupApp`/`SetupOutcome`/`run_setup` exports);
`harw-cli/src/onboarding.rs` (the TUI wiring).

## Security / conventions

Secrets are stored only as `env:`/`file:`/`file-json:` references, never as
plaintext in TOML, never logged. Hand-written error enums (no
anyhow/thiserror). A `models.dev` fetch fails open to the static fallback.
