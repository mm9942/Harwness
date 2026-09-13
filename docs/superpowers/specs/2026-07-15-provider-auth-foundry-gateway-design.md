# Provider-Auth & Foundry-Gateway — Design (Slice 1)

**Datum:** 2026-07-15
**Status:** Design freigegeben, bereit für Implementierungsplanung
**Scope:** Erste Scheibe eines größeren Vorhabens „Alternative Provider-Wege für Anthropic/OpenAI". Diese Scheibe liefert echten Anthropic-`/messages`-Transport, direkten Claude-Setup-Token/OAuth (`CLAUDE_CODE_OAUTH_TOKEN`), Codex-OAuth via `~/.codex/auth.json`, ein **Azure-Foundry-Gateway** (rein env-getrieben) und den `harw auth`-CLI-Baum.

## 1. Motivation & Ziel

Der Harness spricht heute real nur **OpenAI-kompatible** Endpoints (`harw-provider-http::OpenAiResponsesProvider`, Responses + Chat). `ProviderApi::AnthropicMessages` existiert im Katalog, fällt im Transport aber fälschlich auf den Chat-Pfad zurück — es gibt **keinen echten Anthropic-Wire**. Damit lässt sich weder Anthropic-direkt noch ein Cloud-Gateway zu Claude ansprechen.

Ziel dieser Scheibe:

1. **Ein** sauberer Anthropic-Messages-Transport, der drei Ziele bedient (Anthropic-direkt via API-Key **oder** OAuth-Setup-Token, und Azure-Foundry via `x-api-key`) — sie unterscheiden sich nur in `base_url` + Auth-Header.
2. **Zusätzliche Auth-Wege** neben dem API-Key: Claude-Setup-Token (langlebig, wiederverwendbar, direkt setzbar, hinterlegt als `CLAUDE_CODE_OAUTH_TOKEN`) und Codex-OAuth aus `~/.codex/auth.json`.
3. **Sofort testbar gegen echte Claude-Modelle** über Foundry, da Key + Base-URL bereits in der Umgebung stehen.

## 2. Nicht-Ziele (explizit außerhalb dieser Scheibe)

- **Entra-ID-OAuth für Foundry** (Client-Credentials-Flow). Foundry läuft hier ausschließlich über `x-api-key`. „Kein OAuth über Cloud" ist bewusste Vorgabe.
- **AWS Bedrock** (SigV4) und **GCP Vertex** (Google-JWT-OAuth) — eigener späterer Zyklus, hand-gerollte Signierung.
- **Token-Refresh** — nur langlebiger Setup-Token; bei Ablauf klare Fehlermeldung + erneuter Login.
- **Streaming** für den Anthropic-Transport — `respond()` bleibt non-streaming, wie der bestehende OpenAI-Pfad.
- **`chatgpt-account-id`-Feinheiten** für Codex-OAuth gegen die Responses-API — für diese Scheibe reicht Bearer-Token aus `~/.codex/auth.json`; die Account-Header-Nuance ist Follow-up.

## 3. Gesicherte externe Fakten (grounded)

### 3.1 Anthropic Messages-Wire
- Endpoint: `POST {base_url}/v1/messages`.
- Pflicht-Header: `anthropic-version: 2023-06-01`.
- **API-Key-Auth:** Header `x-api-key: <key>` (kein `Authorization`).
- **OAuth/Setup-Token-Auth:** Header `Authorization: Bearer <token>` **plus** `anthropic-beta: oauth-2025-04-20` (**kein** `x-api-key`). Der `oauth-2025-04-20`-Header ist für `/v1/messages` erforderlich.
- Body (relevanter Ausschnitt): `{ "model": <string>, "system": <string?>, "messages": [{role, content}], "max_tokens": <int> }`. Antworttext liegt in `content[]`-Blöcken vom Typ `text` (`content[].text`).
- Aktuelle Modell-IDs sind bare Strings, z. B. `claude-opus-4-8`, `claude-sonnet-5`, `claude-haiku-4-5`.

### 3.2 Azure Foundry → Claude
- Base-URL-Form: `https://<resource>.services.ai.azure.com/anthropic`.
- Endpoint: `POST {base}/v1/messages` — **identisches** Anthropic-Messages-Schema.
- Auth (diese Scheibe): `x-api-key: <foundry-key>` + `anthropic-version: 2023-06-01`.
- `model` = Deployment-Name (bei Foundry gleich dem Modell-ID-String, wenn Default-Deploy).

### 3.3 Vorhandene Umgebung (verifiziert)
- `ANTHROPIC_FOUNDRY_API_KEY` gesetzt (Key, wird als `x-api-key` gesendet).
- `ANTHROPIC_FOUNDRY_BASE_URL = https://mias-research-lab-resource.services.ai.azure.com/anthropic/` — **Achtung Trailing-Slash**.
- `ANTHROPIC_API_KEY`, `OPENAI_API_KEY` gesetzt. `CLAUDE_CODE_OAUTH_TOKEN` (noch) nicht.
- Beide Foundry-Variablen sind in der `.zshrc` des Nutzers gesetzt.

### 3.4 Zu verifizierender Punkt (Implementierungszeit)
Die exakten PKCE-Konstanten des Claude-Setup-Token-Flows — `client_id`, Authorize-URL, Token-Endpoint, Scopes, Redirect/`code`-Callback — sind Claude-Code-Interna und **werden zur Implementierungszeit gegen den aktuellen Stand gepinnt** (via gezieltem Doc-Research-Agenten), nicht aus dem Gedächtnis hart gesetzt. Gesichert und hart verwendbar: der `anthropic-beta: oauth-2025-04-20`-Header und der Bearer-Auth-Pfad.

## 4. Architektur — betroffene Crates

| Crate | Änderung |
|---|---|
| `harw-provider-http` | **NEU** `AnthropicMessagesProvider` (reqwest, hand-gerollt, kein SDK). **NEU** Factory `build_provider(config) -> Result<Box<dyn ModelProvider>, HttpProviderError>`, wählt OpenAI- vs. Anthropic-Transport nach `provider.api`. Erweiterte Credential-Auflösung (Env-first). |
| `harw-oauth` | **NEUE Crate** — PKCE-Setup-Token-Flow (Paste), Token-Store (0600-Datei), Credential-Klassifikation. Schlanke Deps: `reqwest`, `sha2`, `base64`, `rand`, `secrecy`, `serde`/`serde_json`. Eigenes `error.rs`. |
| `harw-provider` | `SghAuth`/Marker um Anthropic-Auth-Arten erweitern; `AzureFoundryProviderMarker`. |
| `harw-model-catalog` | `providers.toml`: `foundry`-Provider-Eintrag; `anthropic`-Auth-Methoden um Setup-Token/`CLAUDE_CODE_OAUTH_TOKEN` erweitern. `sources.rs`: Claude-Setup-Token-Env als Quelle. |
| `harw-cli` | **NEU** Subcommand-Baum `harw auth { login, token, import, status }` + Dispatch. |

Bewusste Abgrenzung: Der Auth-Header-Aufbau lebt **im Transport** (`harw-provider-http`), passend zum bestehenden `transport_from_api`-Muster. `harw-provider` bleibt SDK-/reqwest-frei (Identitäts-Schicht).

## 5. Kernkomponente — `AnthropicMessagesProvider`

```rust
// harw-provider-http
pub enum AnthropicCredential {
    /// Anthropic-API-Key oder Foundry-Key → Header `x-api-key`.
    ApiKey(SecretString),
    /// Setup-Token/OAuth → Header `Authorization: Bearer` + `anthropic-beta: oauth-2025-04-20`.
    OAuth(SecretString),
}

pub struct AnthropicMessagesProvider {
    client: reqwest::Client,
    base_url: String,   // bereits normalisiert (ohne Trailing-Slash)
    model: String,
    credential: AnthropicCredential,
}
```

`impl ModelProvider for AnthropicMessagesProvider`:
1. URL = `format!("{}/v1/messages", self.base_url)`.
2. Body via `build_messages_body(&self.model, &request)` (rein, I/O-frei, direkt testbar): System-Prompt + Instruction-Fragmente → `system`; History → `messages[]` (`User`→`user`, `Assistant`→`assistant`, `ToolResult`→`user`/`tool_result`-Block; reine `ToolCall` übersprungen — analog zum OpenAI-Pfad). `max_tokens` mit sinnvollem Default (z. B. 16000, unter HTTP-Timeout-Grenze; non-streaming).
3. Header setzen (siehe 5.1).
4. Antwort: `extract_anthropic_text(&value)` sammelt `content[]` mit `type == "text"` zu einem String; leer → `ModelError::EmptyResponse`.
5. Fehlercodes: `!status.is_success()` → `HttpProviderError::Api { status, body }` → `ModelError::RequestFailed`.

### 5.1 Header-Schema

| Credential | Header |
|---|---|
| `ApiKey` | `x-api-key: <secret>`, `anthropic-version: 2023-06-01`, `content-type: application/json` |
| `OAuth` | `Authorization: Bearer <secret>`, `anthropic-beta: oauth-2025-04-20`, `anthropic-version: 2023-06-01`, `content-type: application/json` |

Das Secret wird ausschließlich via `ExposeSecret` beim Header-Setzen offengelegt, nie geloggt.

### 5.2 Base-URL-Normalisierung

`normalize_base_url(raw) -> String`: trimmt ein oder mehrere Trailing-`/`, sodass `…/anthropic/` und `…/anthropic` beide zu `…/anthropic` werden und `/v1/messages` sauber angehängt wird. Eigener Unit-Test mit Trailing-Slash-Fall.

## 6. Credential-Auflösung (Factory `build_provider`)

`build_provider(config: &ResolvedConfig) -> Result<Box<dyn ModelProvider>, HttpProviderError>`:

1. `default_provider` + `default_model` aus `config.harness` lesen; Provider-Eintrag holen.
2. Verzweigung nach `provider.api`:
   - `"anthropic-messages"` → Anthropic-Pfad (unten).
   - sonst (`"openai-responses"`/`"openai-chat"`/…) → bestehender `OpenAiResponsesProvider::from_config`-Pfad, unverändert.

**Anthropic-Pfad — Auflösungs-Präzedenz:**

1. **Foundry** — wenn `provider.id`/`name == "foundry"`:
   - Base-URL = `env:ANTHROPIC_FOUNDRY_BASE_URL` (normalisiert). Fehlt sie → `HttpProviderError::MissingEnv { var: "ANTHROPIC_FOUNDRY_BASE_URL" }`.
   - Key = `env:ANTHROPIC_FOUNDRY_API_KEY` → `AnthropicCredential::ApiKey`. Fehlt er → `HttpProviderError::MissingEnv { var: "ANTHROPIC_FOUNDRY_API_KEY" }`.
2. **Anthropic-direkt** — sonst (`provider.id == "anthropic"`, Base default `https://api.anthropic.com`):
   - `$CLAUDE_CODE_OAUTH_TOKEN` gesetzt → `AnthropicCredential::OAuth`.
   - sonst `provider.auth`-`SecretRef` auflösen (unterstützt `env:`/`file:`/`file-json:`; `secrets:`/`keyring:` bleiben wie heute nicht unterstützt) → `AnthropicCredential::ApiKey`. Wenn kein `auth` gesetzt ist, Fallback auf `env:ANTHROPIC_API_KEY`.

**OpenAI/Codex-Pfad (unverändert im Wire, erweiterte Auflösung):** Der `openai`-Provider kann seinen Key via `file-json:~/.codex/auth.json#/tokens/access_token` (Codex-OAuth) oder `#/OPENAI_API_KEY` (Codex-API-Key) beziehen — beides bereits durch `sources.rs` + bestehende `resolve_secret`-Logik (`file-json`) abgedeckt. `harw auth import codex` registriert die passende SecretRef.

## 7. `harw-oauth` — Setup-Token-Flow & Store

### 7.1 PKCE-Paste-Flow (`login`)
1. `code_verifier` = zufällige 32 Bytes → base64url (ohne Padding).
2. `code_challenge` = base64url(SHA-256(code_verifier)) (S256).
3. Authorize-URL bauen (Konstanten gepinnt zur Impl-Zeit) und ausgeben; Nutzer meldet sich im Browser an und fügt den zurückgegebenen `code` (ggf. `code#state`) in `stdin` ein.
4. Token-Exchange: `POST` an Token-Endpoint mit `grant_type=authorization_code`, `code`, `code_verifier`, `client_id`, `redirect_uri` → JSON mit `access_token` (langlebiger Setup-Token) + optional `expires_at`.
5. `access_token` als `SecretString` zurückgeben.

### 7.2 Token-Store
- `save_token(home, secret) -> SecretRef`: schreibt `<home>/secrets/anthropic-oauth.token`, unter Unix `0o600`, gibt `file:<abs>`-SecretRef zurück (gleiches Muster wie `onboarding::write_secret_file`).
- Registrierung in `auth.toml` (`credentials`/`credential_pool`) analog zu `persist_outcome`.
- CLI druckt zusätzlich `export CLAUDE_CODE_OAUTH_TOKEN=<token>` nach `stderr`, damit der Nutzer den Env-first-Pfad sofort nutzen kann.

### 7.3 Fehler (`harw-oauth::error`, an `rust-error-designer` delegiert)
Hand-geschriebenes Enum, u. a.: `PkceExchange { status: u16, body: String }`, `MalformedCallback(String)`, `TokenStoreIo(std::io::Error)` (mit `From`), `MissingField(&'static str)`. `Display`/`Debug`/`std::error::Error`/`source()` wie in `CLAUDE.md` vorgeschrieben; kein `anyhow`/`thiserror`.

## 8. `harw auth`-CLI

Neuer `Command::Auth { action: AuthAction }` in `harw-cli/src/cli.rs`; Dispatch in `main.rs`/`lifecycle.rs`.

```
harw auth login  anthropic            # PKCE-Paste-Flow → Token speichern + export drucken
harw auth token  anthropic [--stdin]  # Token direkt setzen (Alternative zum API-Key)
harw auth import codex                 # ~/.codex/auth.json erkennen, SecretRef für openai registrieren
harw auth status                       # vorhandene Quellen je Provider anzeigen (ohne Secrets)
```

- `token --stdin`: liest Token aus `stdin` (nie als Arg, nie geloggt), speichert wie 7.2.
- `status`: prüft nur **Vorhandensein** (env-var gesetzt? Token-Datei existiert? `auth.toml`-Eintrag da?) und gibt eine Tabelle aus — niemals der Wert. Deckt `CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`, `ANTHROPIC_FOUNDRY_API_KEY`/`_BASE_URL`, `OPENAI_API_KEY`, `~/.codex/auth.json`, `~/.claude/.credentials.json` ab.

## 9. Katalog & Marker

- `providers.toml`: neuer Eintrag
  ```toml
  [[provider]]
  id = "foundry"
  name = "Azure AI Foundry (Claude)"
  base_url = "env:ANTHROPIC_FOUNDRY_BASE_URL"   # Platzhalter; real aus Env zur Laufzeit
  api = "anthropic-messages"
  default_model = "claude-sonnet-5"
  featured = true
  auth = [{ method = "api-key", env_vars = ["ANTHROPIC_FOUNDRY_API_KEY"] }]
  models = ["claude-opus-4-8", "claude-sonnet-5", "claude-haiku-4-5"]
  ```
  (Die `base_url` im Katalog dient nur der Anzeige; die Factory liest die echte URL aus `ANTHROPIC_FOUNDRY_BASE_URL`.)
- `anthropic`-Eintrag: Auth-Methoden um Setup-Token/`CLAUDE_CODE_OAUTH_TOKEN` ergänzen (zusätzliche `env_vars`-Kandidaten + `local-import`-Quelle `claude-setup-token`).
- `sources.rs`: Quelle `claude-setup-token` (Env `CLAUDE_CODE_OAUTH_TOKEN`, `SourceKind::OAuthToken`).
- `harw-provider/marker.rs`: `AzureFoundryProviderMarker` (+ `ProviderMarker`-Impl `NAME = "foundry"`); Anthropic-OAuth-Auth-Marker sichtbar machen (bestehendes `ChatGptOAuthAuth`-Muster als Vorlage).

## 10. Fehlerbehandlung (Erweiterungen)

`HttpProviderError` +Varianten: `MissingEnv { var: String }`, `MissingOAuthToken`. Bestehende `Api { status, body }`/`UnresolvedCredential`/`MissingDefault` bleiben. Alle hand-geschrieben; `From`-Impls für Fremd-Fehler, `source()` verlinkt. Delegation an `rust-error-designer`, sobald `error.rs` berührt wird.

## 11. Teststrategie (Delegation an `rust-test-designer`)

Reine, netzfreie Unit-Tests:
- `build_messages_body`: Shape für System+History; `ToolResult`-Mapping; leerer System-Prompt weggelassen.
- `extract_anthropic_text`: mehrere `text`-Blöcke verbunden; `None` bei leer.
- Header-Schema-Wahl je `AnthropicCredential` (Assertion auf gesetzte Header-Namen; Secret nie im Klartext im Test-Log).
- `normalize_base_url`: Trailing-Slash-Fall (`…/anthropic/` → `…/anthropic`), Mehrfach-Slash, ohne Slash.
- **PKCE-Vektor:** fixer `code_verifier` → erwartete `code_challenge` (bekannter S256-Testvektor).
- Auflösungs-Präzedenz: Foundry-Env vorhanden → Foundry-Credential; `CLAUDE_CODE_OAUTH_TOKEN` vorhanden → OAuth; sonst API-Key. (Env-Manipulation im Test unter `#![forbid(unsafe_code)]`/Edition 2024 beachten — Auflösungslogik so faktorisieren, dass sie eine Env-Lookup-Funktion als Parameter nimmt, statt `std::env` direkt zu lesen; das macht sie ohne `set_var` testbar.)
- Live-Integrationstests `#[ignore]` mit Kommentar: echtes Anthropic (`ANTHROPIC_API_KEY`) und echtes Foundry (`ANTHROPIC_FOUNDRY_*`).

Mindestens ein Integrationstest pro Auth-Weg (mit Stub-HTTP oder `#[ignore]`-Live).

## 12. Doku (Delegation an `rust-doc-writer`)

`//!`-Modul-Header + `///`-Item-Doku für alle neuen `pub`-Items (`AnthropicMessagesProvider`, `AnthropicCredential`, `build_provider`, `harw-oauth`-Oberfläche, CLI-Auth-Kommandos) gemäß `CLAUDE.md`-Standard. `cargo doc --no-deps` grün.

## 13. Abnahmekriterien

1. `cargo check`/`clippy --tests -- -D warnings`/`test` grün (netzfreie Tests).
2. `harw auth status` zeigt korrekt die vorhandenen Quellen (Foundry-Key/-Base, `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`) ohne Secret-Ausgabe.
3. Ein `#[ignore]`-Live-Test gegen Foundry (`ANTHROPIC_FOUNDRY_*`) liefert eine nicht-leere Claude-Antwort (manuell mit `--ignored` verifizierbar).
4. Direkter Anthropic-Weg über `ANTHROPIC_API_KEY` funktioniert (Live-`#[ignore]`).
5. `harw auth token anthropic --stdin` speichert Token 0600 und druckt die `export`-Zeile; anschließend nutzt der Anthropic-Transport den Token env-first.

## 14. Offene Punkte / Follow-ups

- PKCE-Konstanten zur Impl-Zeit pinnen (siehe 3.4).
- Entra-ID-Auth für Foundry (Entra-only-Modelle).
- Bedrock/Vertex (hand-gerollte Signierung).
- Codex-OAuth `chatgpt-account-id`-Header für die Responses-API.
- Token-Refresh.
- Git-Repo ist aktuell nicht initialisiert (leeres `.git`) — Design-Commit nachholen, sobald `git init` erfolgt ist.
