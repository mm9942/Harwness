# Design: Multi-Provider-Onboarding mit ratatui-Setup

**Stand:** 2026-07-15 · **Scope-Entscheidung:** Katalog + TUI, Auth = **API-Key +
Multi-Key-Pool + Wiederverwendung lokaler Auth-Dateien** (`~/.codex`, `~/.claude`,
…). Live-Nutzung reiner OAuth-Token-Quellen bleibt einer Folge-Iteration
vorbehalten (Erkennung/Import schon jetzt).

## Context

Das aktuelle `harw-cli/src/onboarding.rs` richtet **einen** Provider (openai) über
zeilenbasierte Prompts ein. Ziel ist „alle möglichen Provider mit TUI-gestütztem
Setup": ein durchsuchbarer Provider-Katalog, ein zweistufiger ratatui-Picker
(Provider → Auth → Modell) und Multi-Key-Credential-Rotation — nach dem Vorbild
von hermes und openclaw.

## Evidenz (kondensiert)

- **hermes** (`~/.hermes`): `models.dev`-Katalog (**152 Provider**, 4-stufiger
  Cache mem→disk-TTL-1h→Netz→stale-grace, `models_dev.py:240`); statisches
  `PROVIDER_REGISTRY` (`auth.py:176`) mit `id/name/auth_type/inference_base_url/
  api_key_env_vars`; zweistufiger TUI-Picker `_open_model_picker` (`cli.py:7743`,
  stage `provider`→`model`); Auth zweigeteilt `providers{}` (OAuth) vs.
  `credential_pool{}` (Multi-Key-Rotation, `credential_pool.py:1365`).
- **openclaw** (`inspirations/openclaw`): Provider = deklarative Manifeste
  `extensions/<id>/openclaw.plugin.json` (`modelCatalog`, `setup.envVars`,
  `providerAuthChoices`); Core-Fallback `provider-env-vars.ts:22`; grouped
  Auth-Picker `promptAuthChoiceGrouped` (`auth-choice-prompt.ts:69`) mit Sentinels
  More/Custom/Keep/Skip; Flow `runSetupModelAuthStep` (`setup.model-auth.ts:115`)
  fehlertolerant (Re-Prompt statt Abbruch); Discovery hybrid statisch/dynamisch,
  key-gated + TTL-Cache + Static-Fallback; Multi-Key `apiKeys[]` Round-Robin
  (`api-key-rotation.ts`); Precedence **Env → Config-SecretRef → Auth-Store**.

## Entscheidung: Katalog-Strategie

**Eingebetteter kuratierter Katalog als Basis (offline, deterministisch) +
optionale models.dev-Anreicherung der Modell-Listen.**

- `providers.toml` wird **in das Binary eingebettet** (`include_str!`) — ~18
  gängige, API-Key-basierte Provider. Funktioniert offline, kein harter
  Netzwerk-Zwang, reproduzierbar.
- `models.dev` (optional, key-gated) reichert nur die **Modell-Listen** an und
  wird nach `~/.harw/cache/models_dev.json` gecacht (mtime-TTL 1h, Static-Fallback
  bei Offline/Fehler) — genau das hermes-Muster. Kann per Flag/Config abgeschaltet
  werden.

Begründung: „alle Provider" braucht Reichweite (models.dev), aber ein
standalone-Harness darf beim Setup nicht am Netz hängen — der eingebettete
Katalog garantiert einen funktionierenden Offline-Pfad.

## Datenmodell

### Provider-Katalog (neues Crate `harw-model-catalog`)

```rust
pub enum ProviderApi { OpenAiResponses, OpenAiChat, AnthropicMessages, Ollama }

pub enum AuthMethod {
    ApiKey { env_vars: Vec<String> }, // Precedence-Reihenfolge
    LocalImport { sources: Vec<String> }, // vorhandene lokale Auth-Dateien (s.u.)
    LocalBaseUrl,                     // ollama/lmstudio: kein Secret
    Custom,                           // beliebiger kompatibler Endpoint
}

pub struct ProviderSpec {
    pub id: String,            // "openrouter"
    pub name: String,          // "OpenRouter"
    pub base_url: String,
    pub api: ProviderApi,
    pub auth: Vec<AuthMethod>,
    pub default_model: Option<String>,
    pub featured: bool,        // Kurzliste im Picker
    pub models: Vec<String>,   // statischer Fallback; models.dev ergänzt
}

pub fn embedded_catalog() -> Vec<ProviderSpec>;              // include_str!("providers.toml")
pub fn enrich_from_models_dev(cache_dir: &Path, key: Option<&SecretString>) -> ...; // optional, TTL-Cache
```

Kuratierte Basis (~18): openai, anthropic, openrouter, groq, deepinfra, together,
fireworks, xai, mistral, deepseek, moonshot(kimi), zhipu(glm), cerebras, nebius,
perplexity, cloudflare, ollama(local), lmstudio(local), + `custom`.

### Credential-Store (`auth.toml` erweitern in `harw-config/auth_toml.rs`)

`AuthConfig` behält `credentials: HashMap<String, SecretRef>` und bekommt:

```rust
#[serde(default)]
pub credential_pool: HashMap<String, Vec<CredentialEntry>>, // key = provider id

pub struct CredentialEntry {
    pub secret: SecretRef,          // env:/file: — nie Klartext
    #[serde(default)] pub label: Option<String>,
    #[serde(default)] pub priority: u32,
    #[serde(default)] pub base_url: Option<String>,
}
```

Auflösung mit **Precedence Env → lokale Import-Quelle → SecretRef-Pool → (später)
Store** und Round-Robin über gleichrangige Einträge (analog `api-key-rotation.ts`).

### Lokale Auth-Quellen (`credential_sources`, hermes-Muster)

Statt (oder zusätzlich zu) einem manuell eingegebenen API-Key darf das Setup
**vorhandene lokale Auth-Dateien wiederverwenden** — genau dafür existieren
`~/.codex/auth.json`, `~/.claude/.credentials.json` usw. Eingebettete Quell-Tabelle:

```rust
pub enum ExtractRule {
    JsonPointer(String), // z. B. "/OPENAI_API_KEY" oder "/tokens/access_token"
    EnvVar(String),      // Wert steht in einer Env-Var, die die Datei setzt
    WholeFile,           // gesamte (getrimmte) Datei ist das Secret
}

pub struct CredentialSource {
    pub id: String,        // "codex", "claude-cli", "gh-cli"
    pub provider: String,  // auf welchen Katalog-Provider es passt
    pub path: String,      // "~/.codex/auth.json"
    pub extract: ExtractRule,
    pub kind: SourceKind,  // ApiKey | OAuthToken
}

pub fn detect_local_sources(catalog_provider: &str) -> Vec<DetectedCredential>;
```

Bekannte Startquellen: `~/.codex/auth.json` (`/OPENAI_API_KEY` oder OAuth-`tokens`),
`~/.claude/.credentials.json` (Anthropic-OAuth), `~/.config/gh/hosts.yml` (nur wo
sinnvoll), sowie generische `env:`-Kandidaten aus `provider-env-vars`.

**Import-Semantik:** `detect_local_sources` liest die Datei read-only und
extrahiert per `ExtractRule`. Beim Bestätigen im Picker wird **kein Wert kopiert
oder umgeschrieben**, sondern ein `CredentialEntry` mit einem präzisen
`SecretRef` in den Pool geschrieben — dazu wird der `SecretRef`-Grammatik ein
neuer Zeiger-Typ **`file-json:PATH#/json/pointer`** hinzugefügt (liest genau ein
JSON-Feld, statt der Ganzdatei), damit Rotation/Token-Refresh der Quelle
weiterhin greift. `kind = OAuthToken` wird erkannt und markiert: nutzbar, sobald
die passende Bridge-API (`chatgpt-backend`/`anthropic-oauth`) implementiert ist;
`kind = ApiKey` ist sofort über den bestehenden `openai-chat`/`responses`-Pfad
verwendbar.

## Provider-Bridge erweitern (`harw-provider-http`) — WICHTIG

Der aktuelle Adapter spricht nur die OpenAI-**Responses**-API (`/responses`).
Die meisten Drittanbieter (OpenRouter, Groq, DeepInfra, xAI, …) sprechen
**`/chat/completions`**. Daher:

- `harw-provider-http` bekommt einen zweiten Transport `openai-chat`
  (`POST {base_url}/chat/completions`, Standard-Message-Schema), gewählt über
  `ProviderApi` aus dem Katalog.
- `from_config` wählt Transport nach dem `api`-Feld des Providers.
- (Anthropic-Messages/Ollama bleiben als spätere Erweiterung markiert; Katalog
  trägt sie schon, Bridge implementiert sie in einer Folge-Iteration.)

## ratatui-Setup-Screen (`harw-tui`)

Neuer `SetupApp` neben dem bestehenden `ChatApp`:

- **Stufe 1 — Provider:** durchsuchbare Liste (featured oben, dann `More…`,
  `Custom`, `Keep current`, `Skip` als Sentinels), Fuzzy-Filter beim Tippen,
  Viewport-Scrolling (analog `_compute_model_picker_viewport`). Konfigurierte
  Provider mit Suffix „(konfiguriert)".
- **Stufe 2 — Auth:** je nach `AuthMethod` des Providers. Der Picker listet
  **zuerst erkannte lokale Quellen** (`detect_local_sources`, z. B. „Aus
  ~/.codex/auth.json übernehmen"), dann „API-Key eingeben" (maskiert), dann
  base_url-Bestätigung (local). Fehler → Re-Prompt, kein Abbruch.
- **Stufe 3 — Modell:** Liste aus Katalog (+ models.dev, wenn Key vorhanden),
  Default vorausgewählt.
- Rückgabe: `SetupOutcome { provider_id, base_url, api, model, secret_ref }`.
  I/O-frei testbare Zustandsmaschine (`stage`, `selected`, `filter`, `scroll`).

## Wiring (`onboarding.rs`)

- Interaktiv: `harw_tui::run_setup(catalog)` → `SetupOutcome` → schreibt
  `providers/<id>.toml`, `models/<id>.toml`, `credential_pool`-Eintrag in
  `auth.toml`, setzt `default_provider/default_model` + `onboarding.seen`.
- Nicht-interaktiv (`HARW_ONBOARD_NONINTERACTIVE`): unverändert reine Defaults
  (openai) — bestehende Tests bleiben grün.
- Key-Persistenz wie bisher: `env:`-Ref bevorzugt; bei Direkteingabe
  `file:`-Ref auf `<home>/secrets/<id>.key` (chmod 600).

## Phasen

0. **`harw-model-catalog`-Crate** + eingebettetes `providers.toml` (~18) +
   `ProviderSpec`/`AuthMethod`/`ProviderApi`, Tests.
1. **`auth.toml`-Erweiterung** (`credential_pool` + `CredentialEntry`) +
   `SecretRef`-Erweiterung um `file-json:PATH#/pointer` + Resolver mit Precedence
   & Round-Robin + `credential_sources`/`detect_local_sources` (liest
   `~/.codex/auth.json`, `~/.claude/.credentials.json` read-only).
2. **`harw-provider-http`**: `openai-chat`-Transport, Transport-Wahl per `api`.
3. **`harw-tui` `SetupApp`**: zweistufiger Picker, I/O-freie State-Machine + Tests.
4. **`onboarding.rs`-Wiring** + `models.dev`-Cache (optional, TTL, Fallback).
5. Rust-Auto-Agenten (error/test/doc) + Workspace-Sweep.

## Kritische Dateien

Neu: `harw-model-catalog/` (lib.rs, spec.rs, models_dev.rs, providers.toml, error.rs).
Ändern: `harw-config/src/auth_toml.rs` (credential_pool), `harw-provider-http/src/lib.rs`
(+chat-Transport), `harw-tui/src/` (+`setup.rs`/`SetupApp`, lib.rs-Export),
`harw-cli/src/onboarding.rs` (TUI-Wiring), Root `Cargo.toml` (+Member).

## Sicherheit / Konventionen

Secrets nur als `env:`/`file:`-Refs, nie Klartext in TOML, nie geloggt,
`zeroize` für Key-Bytes; handgeschriebene Error-Enums (kein anyhow/thiserror);
`cargo add` statt Pins; `models.dev`-Fetch fail-open auf Static-Fallback.

## Verifikation (E2E)

```sh
cargo build -p harw-cli --release
# nicht-interaktiv weiterhin grün:
HARW_HOME=/tmp/harw-cat HARW_ONBOARD_NONINTERACTIVE=1 ./target/release/harw onboard
# Katalog geladen (Unit): cargo test -p harw-model-catalog
# chat-Transport gegen einen /chat/completions-Provider (Key gesetzt):
#   HARW_OPENROUTER_KEY=... harw  → echte Antwort
cargo test --workspace
```
Interaktiver TUI-Picker manuell (TTY); State-Machine per Unit-Test.
