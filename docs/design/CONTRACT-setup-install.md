# CONTRACT MASTER — Setup- & Install-Lebenszyklus

Dies ist die **verbindliche** Signatur-Referenz für den Contract-Fanout-Bau.
Jeder Datei-Agent implementiert exakt gegen die hier festgelegten Typen/Namen.
Abweichung = Integrationsbruch. Konventionen: kein `anyhow`/`thiserror`, kein
`unwrap`/`expect` im Prod-Pfad, handgeschriebene Error-Enums (Display, Debug
delegiert an Display, `std::error::Error::source`, `From`-Impls), Doku `//!`/`///`.

## Crate `harw-model-catalog`

Deps: `serde` (derive), `serde_json`, `toml`, `secrecy`, `harw-config` (path).
`reqwest` (features `json,rustls-tls`, `--no-default-features`) + `tokio` nur für
`models_dev.rs`.

### `src/spec.rs`
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderApi { OpenAiResponses, OpenAiChat, AnthropicMessages, Ollama }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "kebab-case")]
pub enum AuthMethod {
    ApiKey { env_vars: Vec<String> },
    LocalImport { sources: Vec<String> }, // Ids aus CredentialSource
    LocalBaseUrl,
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderSpec {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub api: ProviderApi,
    #[serde(default)] pub auth: Vec<AuthMethod>,
    #[serde(default)] pub default_model: Option<String>,
    #[serde(default)] pub featured: bool,
    #[serde(default)] pub models: Vec<String>,
}
```

### `src/sources.rs`
```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SourceKind { ApiKey, OAuthToken }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExtractRule { JsonPointer(String), EnvVar(String), WholeFile }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CredentialSource {
    pub id: String,        // "codex", "claude-cli"
    pub provider: String,  // Katalog-Provider-Id
    pub path: String,      // "~/.codex/auth.json" (Tilde erlaubt)
    pub extract: ExtractRule,
    pub kind: SourceKind,
}

pub struct DetectedCredential {
    pub source: CredentialSource,
    pub exists: bool,          // Datei/Feld vorhanden
    pub secret_ref: String,    // fertiger Ref-String, z.B. "file-json:/home/u/.codex/auth.json#/OPENAI_API_KEY"
}

pub fn embedded_sources() -> Vec<CredentialSource>;               // eingebettete Startquellen
pub fn detect_local_sources(provider: &str) -> Vec<DetectedCredential>; // read-only, fehlerfrei (leer bei nichts)
```

### `src/embedded.rs`
```rust
pub fn embedded_catalog() -> Vec<ProviderSpec>; // parse include_str!("providers.toml")
```
`providers.toml`: Tabelle `[[provider]]` mit Feldern von `ProviderSpec`. ~18
Provider (openai[responses], anthropic[anthropic-messages], openrouter/groq/
deepinfra/together/fireworks/xai/mistral/deepseek/moonshot/zhipu/cerebras/nebius/
perplexity/cloudflare [openai-chat], ollama/lmstudio [ollama/openai-chat, local],
custom).

### `src/error.rs`
```rust
pub type CatalogResult<T> = Result<T, CatalogError>;
pub enum CatalogError { Parse(String), Io { path: String, source: std::io::Error }, Http(String) }
```

### `src/models_dev.rs`
```rust
pub struct ModelsDevCache { /* … */ }
/// Liest ~/.harw/cache/models_dev.json (mtime-TTL 3600s), sonst Fetch von
/// https://models.dev/api.json; fail-open auf statischen Katalog.
pub fn enrich_models(cache_dir: &std::path::Path, catalog: &mut [ProviderSpec]) -> CatalogResult<()>;
```

### `src/lib.rs`
`pub mod spec; pub mod sources; pub mod embedded; pub mod models_dev; pub mod error;`
`pub use spec::*; pub use sources::*; pub use embedded::embedded_catalog; pub use error::{CatalogError, CatalogResult};`

## Crate `harw-config` (erweitern — bestehende Dateien)

### `src/auth_toml.rs`
- `SecretRef` erhält Variante `FileJson { path: String, pointer: String }`,
  Grammatik `file-json:PATH#/json/pointer` (Split am ersten `#`). `FromStr` +
  `Display`/`Serialize`/`Deserialize` konsistent zu bestehenden Varianten.
  `as_ref_string()` liefert `"file-json:{path}#{pointer}"`.
- `AuthConfig` erhält `#[serde(default)] pub credential_pool: HashMap<String, Vec<CredentialEntry>>`.
- Neu:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialEntry {
    pub secret: SecretRef,
    #[serde(default)] pub label: Option<String>,
    #[serde(default)] pub priority: u32,
    #[serde(default)] pub base_url: Option<String>,
}
```
Bestehende Tests dürfen nicht brechen; `credential_pool` default-leer.

### `src/harness_config.rs`
`HarnessConfig` erhält `#[serde(default)] pub config_version: u32` (Default 0).
`lib.rs` re-exportiert `CredentialEntry`.

## Crate `harw-install`

Deps: `harw-home` (path), `harw-config` (path), `toml_edit`, `serde`,
`serde_json`, `jiff`, `getrandom`. Ziel: keine schweren Runtime-Deps.

### `src/error.rs`
Enums `InstallError, ServiceError, DoctorError, UpdateError, MigrationError,
PathError` — je Display/Debug(delegiert)/Error::source/`From<std::io::Error>` wo
sinnvoll. `pub type XResult<T> = Result<T, XError>` je Enum.

### `src/platform.rs`
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Os { Linux, MacOs, Windows, Other }
#[derive(Debug, Clone)] pub struct Platform {
    pub os: Os, pub container: bool, pub wsl: bool, pub termux: bool, pub managed: bool }
impl Platform { pub fn detect() -> Self; } // reine Env/Datei-Sonden, kein Fehler
```
Sonden: `/.dockerenv`, `/proc/1/cgroup`, `WSL`-Env/`/proc/version`, `TERMUX_VERSION`,
`HARW_MANAGED`/Nix.

### `src/pathscope.rs`
```rust
pub struct PathScope { root: std::path::PathBuf }
impl PathScope {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self;
    pub fn resolve(&self, rel: &str) -> Result<std::path::PathBuf, PathError>; // lehnt .. / absolute / Symlink-Ausbruch ab
    pub fn root(&self) -> &std::path::Path;
}
```

### `src/context.rs`
```rust
#[non_exhaustive] pub enum InstallMethod { Cargo, Homebrew, Standalone { release_dir: std::path::PathBuf }, Unknown }
pub struct InstallContext { pub method: InstallMethod, pub exe: std::path::PathBuf }
impl InstallContext { pub fn detect() -> Result<Self, InstallError>; } // std::env::current_exe + Pfad-/Env-Marker
```

### `src/service.rs`
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum ServiceKind { Systemd, Launchd, Schtasks, Unsupported }
#[derive(Debug, Clone)] pub struct ServiceSpec {
    pub name: String, pub exec: Vec<String>, pub working_dir: std::path::PathBuf,
    pub env: Vec<(String, String)>, pub restart_sec: u32 }
#[derive(Debug, Clone)] pub enum ServiceStatus { Running, Stopped, NotInstalled }
pub trait ServiceManager {
    fn kind(&self) -> ServiceKind;
    fn render_unit(&self, spec: &ServiceSpec) -> String;             // REINE Funktion
    fn install(&self, spec: &ServiceSpec) -> Result<(), ServiceError>;
    fn status(&self, name: &str) -> Result<ServiceStatus, ServiceError>;
    fn uninstall(&self, name: &str) -> Result<(), ServiceError>;
}
pub fn detect_service_manager(p: &crate::platform::Platform) -> Box<dyn ServiceManager>;
```
`service_systemd.rs`/`service_launchd.rs`/`service_schtasks.rs`: je ein Struct,
das `ServiceManager` implementiert; `render_unit` als reine Funktion (Unit/Plist/
Task-XML), `install`/`uninstall` schreiben Datei + rufen `systemctl --user`/
`launchctl`/`schtasks`. Ein `UnsupportedServiceManager` (in service.rs) liefert
`ServiceError::Unsupported` bei install/uninstall.

### `src/doctor.rs`
```rust
pub enum CheckOutcome { Ok(String), Warn(String), Fail(String) }
pub trait DoctorCheck { fn id(&self) -> &str; fn run(&self) -> CheckOutcome; }
pub fn default_checks(home: &std::path::Path) -> Vec<Box<dyn DoctorCheck>>; // system/os, bwrap, service-manager, home-perms, update-status
pub fn run_all(checks: &[Box<dyn DoctorCheck>]) -> Vec<(String, CheckOutcome)>;
```

### `src/update.rs`
```rust
pub struct VersionInfo { pub latest_version: String, pub last_checked_at: jiff::Timestamp, pub dismissed_version: Option<String> }
pub struct UpdateChecker { home: std::path::PathBuf }
impl UpdateChecker {
    pub fn new(home: impl Into<std::path::PathBuf>) -> Self;
    pub fn read(&self) -> Result<Option<VersionInfo>, UpdateError>;   // ~/.harw/version.json
    pub fn write(&self, info: &VersionInfo) -> Result<(), UpdateError>;
    pub fn dismiss(&self, version: &str) -> Result<(), UpdateError>;
    pub fn is_stale(&self, now: jiff::Timestamp, ttl_secs: i64) -> bool; // 20h-Throttle
}
```

### `src/migration.rs`
```rust
pub trait ConfigMigration { fn from_version(&self) -> u32; fn apply(&self, doc: &mut toml_edit::DocumentMut) -> Result<(), MigrationError>; }
pub struct MigrationRunner { migrations: Vec<Box<dyn ConfigMigration>>, latest: u32 }
impl MigrationRunner {
    pub fn new(migrations: Vec<Box<dyn ConfigMigration>>, latest: u32) -> Self;
    /// Liest config.toml, wendet Migrationen mit from_version >= current an,
    /// schreibt Backup config.toml.bak.<n>, persistiert config_version = latest.
    pub fn run(&self, config_path: &std::path::Path) -> Result<u32, MigrationError>;
}
```

### `src/uninstall.rs`
```rust
#[non_exhaustive] #[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UninstallScope { Service, State, Workspace, Binary }
pub struct CleanupPlan { pub removals: Vec<std::path::PathBuf>, pub preserved: Vec<std::path::PathBuf> }
pub fn plan(home: &std::path::Path, scopes: &[UninstallScope]) -> CleanupPlan; // reine Berechnung
pub fn execute(plan: &CleanupPlan, dry_run: bool) -> Result<(), InstallError>;
```

### `src/lib.rs`
Alle Module `pub mod …;` + Re-Exports der Haupttypen.

## Crate `harw-provider-http` (erweitern `src/lib.rs`)

- `ProviderApi`-abhängige Transportwahl: bestehender `/responses`-Pfad bleibt für
  `OpenAiResponses`; neuer `/chat/completions`-Pfad für `OpenAiChat`
  (Standard-`messages`-Schema, `choices[0].message.content`).
- `from_config` liest zusätzlich `config.harness.default_provider`-`ProviderToml.api`
  (String) → Transportwahl; Credential-Auflösung erweitert um `file-json:`-Ref.
- Keine Signaturänderung an `ModelProvider::respond`.

## Crate `harw-tui` (neue Datei `src/setup.rs`)

```rust
pub enum SetupStage { Provider, Auth, Model, Done }
pub struct SetupApp { /* stage, providers, filter, selected, scroll, chosen_* */ }
pub struct SetupOutcome {
    pub provider_id: String, pub base_url: String, pub api: String,
    pub model: String, pub secret_ref: Option<String>,
}
impl SetupApp {
    pub fn new(catalog: Vec<harw_model_catalog::ProviderSpec>) -> Self;
    // I/O-freie Übergänge, testbar:
    pub fn on_key(&mut self, key: crossterm::event::KeyEvent);
    pub fn outcome(&self) -> Option<&SetupOutcome>;
}
pub fn run_setup(catalog: Vec<harw_model_catalog::ProviderSpec>) -> Result<Option<SetupOutcome>, TuiError>;
```
`lib.rs`: `pub mod setup; pub use setup::{SetupApp, SetupOutcome, run_setup};`
`harw-tui` bekommt Dep `harw-model-catalog` (path).

## Crate `harw-cli` (Integration — Welle C)

- `cli.rs`: neue Subcommands `Completion{shell}`, `Update{--check}`,
  `Service{Install|Status|Uninstall, --dry-run}`, `Uninstall{--scope, --dry-run, --yes}`,
  `Catalog{--refresh}`.
- `completion.rs`: `clap_complete::generate` gegen `Cli::command()`.
- `main.rs`: Dispatch der neuen Commands an `harw-install`-Funktionen.
- `onboarding.rs`: interaktiv `harw_tui::run_setup(embedded_catalog())` →
  `SetupOutcome` schreiben (providers/models/credential_pool); nicht-interaktiv
  unverändert Defaults.
- `chat.rs`: Provideraufbau nutzt `credential_pool` (erste Priorität) mit
  Fallback auf bestehende Auflösung.

Deps ergänzen: `harw-cli` += `harw-install`, `harw-model-catalog`, `clap_complete`.

## Verifikations-Kommando (Integrator, nach jeder Welle)
```
RUSTC_WRAPPER= CARGO_TARGET_DIR=target cargo check --workspace --tests
```
