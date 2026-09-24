# Provider Auth & Foundry Gateway — Design (Slice 1)

> Status: implemented · Last reviewed: 2026-09-24

**Scope:** the first slice of a larger "alternative provider paths for
Anthropic/OpenAI" effort. This slice delivers real Anthropic `/messages`
transport, direct Claude setup-token/OAuth
(`CLAUDE_CODE_OAUTH_TOKEN`), Codex OAuth via `~/.codex/auth.json`, an
**Azure Foundry gateway** (env-driven), and the `harw auth` CLI tree.

As of this review, this slice is implemented: `harw-oauth` exists as a
crate (with a PKCE flow, a token store, and — beyond this slice's original
scope — a Codex token-refresh module), `harw-provider-http` has an
`anthropic.rs` module with `AnthropicMessagesProvider`/`AnthropicCredential`,
`harw-model-catalog/src/providers.toml` has a `foundry` provider entry, and
`harw-cli/src/auth.rs` implements the `harw auth` command tree. Section
content below is kept as the original design rationale; treat "implemented"
as the overall status and cross-check specifics against the listed files
before relying on exact detail.

## 1. Motivation & goal

At the time of writing, the harness only spoke to real,
**OpenAI-compatible** endpoints (`harw-provider-http::OpenAiResponsesProvider`,
Responses + Chat). `ProviderApi::AnthropicMessages` existed in the catalog
but incorrectly fell back to the chat transport wire-side — there was no
real Anthropic wire. That made it impossible to reach Anthropic directly or
a cloud gateway to Claude.

Goal of this slice:

1. **One** clean Anthropic-Messages transport serving three destinations
   (Anthropic-direct via API key **or** OAuth setup token, and Azure
   Foundry via `x-api-key`) — they differ only in `base_url` + auth header.
2. **Additional auth paths** beyond the API key: a Claude setup token
   (long-lived, reusable, settable directly, stored as
   `CLAUDE_CODE_OAUTH_TOKEN`) and Codex OAuth from `~/.codex/auth.json`.
3. **Immediately testable against real Claude models** via Foundry, given
   a key and base URL already present in the environment.

## 2. Non-goals (explicitly out of scope for this slice)

- **Entra ID OAuth for Foundry** (client-credentials flow). Foundry runs
  here exclusively via `x-api-key`; "no OAuth over cloud" is a deliberate
  constraint for this slice.
- **AWS Bedrock** (SigV4) and **GCP Vertex** (Google JWT OAuth) — a later
  cycle, hand-rolled signing.
- **Token refresh** for the Anthropic setup token — long-lived token only;
  on expiry, a clear error and a fresh login. (Note: Codex token refresh
  was later added, in `harw-oauth/src/codex_refresh.rs` — see the status
  note above.)
- **Streaming** for the Anthropic transport — `respond()` stays
  non-streaming, matching the existing OpenAI path.
- **`chatgpt-account-id` details** for Codex OAuth against the Responses
  API — a bearer token from `~/.codex/auth.json` is enough for this slice;
  the account-header nuance is a follow-up.

## 3. Grounded external facts

### 3.1 Anthropic Messages wire
- Endpoint: `POST {base_url}/v1/messages`.
- Required header: `anthropic-version: 2023-06-01`.
- **API-key auth:** header `x-api-key: <key>` (no `Authorization`).
- **OAuth/setup-token auth:** header `Authorization: Bearer <token>`
  **plus** `anthropic-beta: oauth-2025-04-20` (**no** `x-api-key`). The
  `oauth-2025-04-20` header is required for `/v1/messages`.
- Body (relevant excerpt): `{ "model": <string>, "system": <string?>, "messages": [{role, content}], "max_tokens": <int> }`.
  Response text is in `content[]` blocks of type `text`
  (`content[].text`).
- Current model IDs are bare strings, e.g. `claude-opus-4-8`,
  `claude-sonnet-5`, `claude-haiku-4-5`.

### 3.2 Azure Foundry → Claude
- Base-URL form: `https://<resource>.services.ai.azure.com/anthropic`.
- Endpoint: `POST {base}/v1/messages` — **identical** Anthropic-Messages
  schema.
- Auth (this slice): `x-api-key: <foundry-key>` + `anthropic-version:
  2023-06-01`.
- `model` = deployment name (equal to the model ID string on Foundry, for a
  default deploy).

### 3.3 Environment shape (illustrative)

A working setup needs, in the environment:

- `ANTHROPIC_FOUNDRY_API_KEY` — the Foundry key, sent as `x-api-key`.
- `ANTHROPIC_FOUNDRY_BASE_URL` — of the form
  `https://<your-resource>.services.ai.azure.com/anthropic/`. **Watch for
  a trailing slash** — `normalize_base_url` (§5.2) exists specifically to
  handle it.
- `ANTHROPIC_API_KEY`, `OPENAI_API_KEY` for the direct paths;
  `CLAUDE_CODE_OAUTH_TOKEN` for the setup-token path.

These are ordinary environment variables an operator sets for their own
Azure AI Foundry resource and API keys — nothing here is specific to any
particular deployment, and no real resource name, key, or account is
implied by this document.

### 3.4 Point to verify at implementation time

The exact PKCE constants of the Claude setup-token flow — `client_id`,
authorize URL, token endpoint, scopes, redirect/`code` callback — are
Claude Code internals and need to be pinned against the current upstream
behavior at implementation time (via targeted research), not hard-coded
from memory. Grounded and safe to use as-is: the
`anthropic-beta: oauth-2025-04-20` header and the bearer-auth path.

## 4. Architecture — affected crates

| Crate | Change |
|---|---|
| `harw-provider-http` | `AnthropicMessagesProvider` (reqwest, hand-rolled, no SDK). Factory `build_provider(config) -> Result<Box<dyn ModelProvider>, HttpProviderError>`, choosing the OpenAI vs. Anthropic transport by `provider.api`. Extended credential resolution (env-first). |
| `harw-oauth` | PKCE setup-token flow (paste), token store (0600 file), credential classification. Slim deps: `reqwest`, `sha2`, `base64`, `rand`, `secrecy`, `serde`/`serde_json`. Own `error.rs`. |
| `harw-provider` | Auth-marker vocabulary extended with Anthropic auth kinds; `AzureFoundryProviderMarker`. |
| `harw-model-catalog` | `providers.toml`: a `foundry` provider entry; `anthropic` auth methods extended with the setup token/`CLAUDE_CODE_OAUTH_TOKEN`. `sources.rs`: the Claude setup-token env var as a source. |
| `harw-cli` | Subcommand tree `harw auth { login, token, import, status }` and its dispatch. |

Deliberate boundary: auth-header construction lives **in the transport**
(`harw-provider-http`), matching the existing `transport_from_api`
pattern. `harw-provider` stays SDK-/reqwest-free (the identity layer).

## 5. Core component — `AnthropicMessagesProvider`

```rust
// harw-provider-http
pub enum AnthropicCredential {
    /// Anthropic API key or Foundry key → header `x-api-key`.
    ApiKey(SecretString),
    /// Setup token/OAuth → header `Authorization: Bearer` + `anthropic-beta: oauth-2025-04-20`.
    OAuth(SecretString),
}

pub struct AnthropicMessagesProvider {
    client: reqwest::Client,
    base_url: String,   // already normalized (no trailing slash)
    model: String,
    credential: AnthropicCredential,
}
```

`impl ModelProvider for AnthropicMessagesProvider`:
1. URL = `format!("{}/v1/messages", self.base_url)`.
2. Body via `build_messages_body(&self.model, &request)` (pure, no I/O,
   directly testable): system prompt + instruction fragments → `system`;
   history → `messages[]` (`User`→`user`, `Assistant`→`assistant`,
   `ToolResult`→a `user`/`tool_result` block; a bare `ToolCall` is skipped,
   matching the OpenAI path). `max_tokens` with a sensible default (e.g.
   16000, under the HTTP timeout limit; non-streaming).
3. Set headers (see 5.1).
4. Response: `extract_anthropic_text(&value)` joins `content[]` entries
   where `type == "text"` into a string; empty → `ModelError::EmptyResponse`.
5. Error codes: `!status.is_success()` →
   `HttpProviderError::Api { status, body }` → `ModelError::RequestFailed`.

### 5.1 Header schema

| Credential | Headers |
|---|---|
| `ApiKey` | `x-api-key: <secret>`, `anthropic-version: 2023-06-01`, `content-type: application/json` |
| `OAuth` | `Authorization: Bearer <secret>`, `anthropic-beta: oauth-2025-04-20`, `anthropic-version: 2023-06-01`, `content-type: application/json` |

The secret is exposed only via `ExposeSecret` at the point headers are set,
never logged.

### 5.2 Base-URL normalization

`normalize_base_url(raw) -> String`: trims one or more trailing `/`s, so
`…/anthropic/` and `…/anthropic` both become `…/anthropic` and `/v1/messages`
appends cleanly. Its own unit test covers the trailing-slash case.

## 6. Credential resolution (factory `build_provider`)

`build_provider(config: &ResolvedConfig) -> Result<Box<dyn ModelProvider>, HttpProviderError>`:

1. Read `default_provider` + `default_model` from `config.harness`; fetch
   the provider entry.
2. Branch on `provider.api`:
   - `"anthropic-messages"` → the Anthropic path (below).
   - otherwise (`"openai-responses"`/`"openai-chat"`/...) → the existing
     `OpenAiResponsesProvider::from_config` path, unchanged.

**Anthropic path — resolution precedence:**

1. **Foundry** — when `provider.id`/`name == "foundry"`:
   - Base URL = `env:ANTHROPIC_FOUNDRY_BASE_URL` (normalized). Missing →
     `HttpProviderError::MissingEnv { var: "ANTHROPIC_FOUNDRY_BASE_URL" }`.
   - Key = `env:ANTHROPIC_FOUNDRY_API_KEY` →
     `AnthropicCredential::ApiKey`. Missing →
     `HttpProviderError::MissingEnv { var: "ANTHROPIC_FOUNDRY_API_KEY" }`.
2. **Anthropic-direct** — otherwise (`provider.id == "anthropic"`, base
   default `https://api.anthropic.com`):
   - `$CLAUDE_CODE_OAUTH_TOKEN` set → `AnthropicCredential::OAuth`.
   - otherwise resolve `provider.auth`'s `SecretRef` (supports
     `env:`/`file:`/`file-json:`; `secrets:`/`keyring:` remain
     unsupported, as elsewhere today) →
     `AnthropicCredential::ApiKey`. If no `auth` is set, fall back to
     `env:ANTHROPIC_API_KEY`.

**OpenAI/Codex path (wire unchanged, resolution extended):** the `openai`
provider can source its key via
`file-json:~/.codex/auth.json#/tokens/access_token` (Codex OAuth) or
`#/OPENAI_API_KEY` (Codex API key) — both already covered by `sources.rs`
plus the existing `resolve_secret` (`file-json`) logic. `harw auth import
codex` registers the matching `SecretRef`.

## 7. `harw-oauth` — setup-token flow & store

### 7.1 PKCE paste flow (`login`)
1. `code_verifier` = 32 random bytes → base64url (no padding).
2. `code_challenge` = base64url(SHA-256(code_verifier)) (S256).
3. Build the authorize URL (constants pinned at implementation time) and
   print it; the user logs in via the browser and pastes the returned
   `code` (possibly `code#state`) into stdin.
4. Token exchange: `POST` to the token endpoint with
   `grant_type=authorization_code`, `code`, `code_verifier`, `client_id`,
   `redirect_uri` → JSON with `access_token` (the long-lived setup token)
   plus optional `expires_at`.
5. Return `access_token` as a `SecretString`.

### 7.2 Token store
- `save_token(home, secret) -> SecretRef`: writes
  `<home>/secrets/anthropic-oauth.token`, `0o600` on Unix, returns a
  `file:<abs>` `SecretRef` (same pattern as the onboarding secret-file
  writer).
- Registered in `auth.toml` (`credentials`/`credential_pool`) matching the
  existing outcome-persistence pattern.
- The CLI also prints `export CLAUDE_CODE_OAUTH_TOKEN=<token>` to stderr,
  so the user can use the env-first path immediately.

### 7.3 Errors (`harw-oauth::error`)

A hand-written enum, including: `PkceExchange { status: u16, body: String }`,
`MalformedCallback(String)`, `TokenStoreIo(std::io::Error)` (with `From`),
`MissingField(&'static str)`. `Display`/`Debug`/`std::error::Error`/`source()`
per the project's standard error convention; no `anyhow`/`thiserror`.

## 8. `harw auth` CLI

A `Command::Auth { action: AuthAction }` variant with its dispatch
implemented in `harw-cli/src/auth.rs`:

```
harw auth login  anthropic            # PKCE paste flow → store token + print export
harw auth token  anthropic [--stdin]  # set a token directly (alternative to an API key)
harw auth import codex                 # detect ~/.codex/auth.json, register a SecretRef for openai
harw auth status                       # show which sources exist per provider (no secrets)
```

- `token --stdin`: reads the token from stdin (never as an argument, never
  logged), stores it as in §7.2.
- `status`: checks only **presence** (env var set? token file exists?
  `auth.toml` entry present?) and prints a table — never the value. Covers
  `CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`,
  `ANTHROPIC_FOUNDRY_API_KEY`/`_BASE_URL`, `OPENAI_API_KEY`,
  `~/.codex/auth.json`, `~/.claude/.credentials.json`.

## 9. Catalog & markers

- `providers.toml`: a new entry
  ```toml
  [[provider]]
  id = "foundry"
  name = "Azure AI Foundry (Claude)"
  base_url = "env:ANTHROPIC_FOUNDRY_BASE_URL"   # placeholder; the real URL comes from the env at runtime
  api = "anthropic-messages"
  default_model = "claude-sonnet-5"
  featured = true
  auth = [{ method = "api-key", env_vars = ["ANTHROPIC_FOUNDRY_API_KEY"] }]
  models = ["claude-opus-4-8", "claude-sonnet-5", "claude-haiku-4-5"]
  ```
  (The catalog's `base_url` is display-only; the factory reads the real URL
  from `ANTHROPIC_FOUNDRY_BASE_URL`.)
- `anthropic` entry: auth methods extended with the setup
  token/`CLAUDE_CODE_OAUTH_TOKEN` (additional `env_vars` candidates plus a
  `local-import` source, `claude-setup-token`).
- `sources.rs`: a `claude-setup-token` source (env
  `CLAUDE_CODE_OAUTH_TOKEN`, `SourceKind::OAuthToken`).
- `harw-provider/marker.rs`: `AzureFoundryProviderMarker` (+
  `ProviderMarker` impl, `NAME = "foundry"`); an Anthropic-OAuth auth
  marker made visible, following the existing ChatGPT-OAuth-auth marker
  pattern.

## 10. Error handling (extensions)

`HttpProviderError` gains variants: `MissingEnv { var: String }`,
`MissingOAuthToken`. Existing `Api { status, body }`/`UnresolvedCredential`/
`MissingDefault` stay. All hand-written; `From` impls for foreign errors,
`source()` linked.

## 11. Test strategy

Pure, network-free unit tests:
- `build_messages_body`: shape for system+history; `ToolResult` mapping;
  an empty system prompt omitted.
- `extract_anthropic_text`: multiple `text` blocks joined; `None` when
  empty.
- Header-schema choice per `AnthropicCredential` (assert on the header
  names set; the secret never appears in plaintext in a test log).
- `normalize_base_url`: the trailing-slash case (`…/anthropic/` →
  `…/anthropic`), multiple slashes, no slash.
- **PKCE test vector:** a fixed `code_verifier` → the expected
  `code_challenge` (a known S256 test vector).
- Resolution precedence: Foundry env present → Foundry credential;
  `CLAUDE_CODE_OAUTH_TOKEN` present → OAuth; otherwise API key. (Resolution
  logic is factored to take an env-lookup function as a parameter rather
  than reading `std::env` directly, so it's testable without mutating
  process environment.)
- Live integration tests marked `#[ignore]`: real Anthropic
  (`ANTHROPIC_API_KEY`) and real Foundry (`ANTHROPIC_FOUNDRY_*`).

At least one integration test per auth path (stub HTTP or `#[ignore]`
live).

## 12. Documentation

`//!` module headers and `///` item docs for every new `pub` item
(`AnthropicMessagesProvider`, `AnthropicCredential`, `build_provider`, the
`harw-oauth` surface, the CLI auth commands), per the project's doc
standard. `cargo doc --no-deps` clean.

## 13. Acceptance criteria

1. `cargo check`/`clippy --tests -- -D warnings`/`test` clean (network-free
   tests).
2. `harw auth status` correctly shows the existing sources (Foundry
   key/base, `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`) without printing
   secrets.
3. An `#[ignore]` live test against Foundry (`ANTHROPIC_FOUNDRY_*`)
   returns a non-empty Claude response (verifiable manually with
   `--ignored`).
4. The direct Anthropic path via `ANTHROPIC_API_KEY` works (live,
   `#[ignore]`).
5. `harw auth token anthropic --stdin` stores the token at `0600` and
   prints the `export` line; the Anthropic transport then uses the token
   env-first.

## 14. Open items / follow-ups

- Pin the PKCE constants at implementation time (see §3.4).
- Entra ID auth for Foundry (Entra-only models).
- Bedrock/Vertex (hand-rolled signing).
- Codex OAuth's `chatgpt-account-id` header for the Responses API.
- Anthropic setup-token refresh (Codex-side refresh has since landed in
  `harw-oauth/src/codex_refresh.rs`; the Anthropic setup token remains
  long-lived-only per §2).
