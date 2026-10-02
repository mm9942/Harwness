# CONTRACT MASTER — Setup & Install Lifecycle

> Status: implemented · Last reviewed: 2026-09-24

This is the **binding** signature reference for the provider-catalog, setup and
install crates. Conventions: no `anyhow`/`thiserror` (hand-written error enums
instead), no `unwrap`/`expect` on the production path, hand-written error enums
(`Display`, `Debug` delegating to `Display`, `std::error::Error::source`,
`From` impls), docs via `//!`/`///`.

## Crate `harw-model-catalog`

Deps: `serde` (derive), `serde_json`, `toml`, `secrecy`, `harw-config` (path).
`reqwest` (features `json,rustls-tls`, `--no-default-features`) + `tokio` only
for `models_dev.rs`.

### `src/spec.rs`
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderApi { OpenAiResponses, OpenAiChat, AnthropicMessages, Ollama }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "kebab-case")]
pub enum AuthMethod {
    ApiKey { env_vars: Vec<String> },
    LocalImport { sources: Vec<String> }, // ids from CredentialSource
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
    pub provider: String,  // catalog provider id
    pub path: String,      // "~/.codex/auth.json" (tilde allowed)
    pub extract: ExtractRule,
    pub kind: SourceKind,
}

pub struct DetectedCredential {
    pub source: CredentialSource,
    pub exists: bool,          // file/field present
    pub secret_ref: String,    // finished ref string, e.g. "file-json:~/.codex/auth.json#/OPENAI_API_KEY"
}

pub fn embedded_sources() -> Vec<CredentialSource>;                    // embedded starter sources
pub fn detect_local_sources(provider: &str) -> Vec<DetectedCredential>; // read-only, infallible (empty when nothing found)
```

### `src/embedded.rs`
```rust
pub fn embedded_catalog() -> Vec<ProviderSpec>; // parse include_str!("providers.toml")
```
`providers.toml`: a `[[provider]]` table with `ProviderSpec` fields. Roughly 18
providers (openai[responses], anthropic[anthropic-messages], openrouter/groq/
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
/// Reads ~/.harw/cache/models_dev.json (mtime TTL 3600s), otherwise fetches
/// https://models.dev/api.json; fails open to the static catalog.
pub fn enrich_models(cache_dir: &std::path::Path, catalog: &mut [ProviderSpec]) -> CatalogResult<()>;
```

### `src/lib.rs`
`pub mod spec; pub mod sources; pub mod embedded; pub mod models_dev; pub mod error;`
`pub use spec::*; pub use sources::*; pub use embedded::embedded_catalog; pub use error::{CatalogError, CatalogResult};`

## Crate `harw-config` (extends existing files)

### `src/auth_toml.rs`
- `SecretRef` gets a `FileJson { path: String, pointer: String }` variant, with
  grammar `file-json:PATH#/json/pointer` (split on the first `#`). `FromStr` +
  `Display`/`Serialize`/`Deserialize` are consistent with the existing variants.
  `as_ref_string()` produces `"file-json:{path}#{pointer}"`.
- `AuthConfig` gets `#[serde(default)] pub credential_pool: HashMap<String, Vec<CredentialEntry>>`.
- New type:
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialEntry {
    pub secret: SecretRef,
    #[serde(default)] pub label: Option<String>,
    #[serde(default)] pub priority: u32,
    #[serde(default)] pub base_url: Option<String>,
}
```
Existing tests must not break; `credential_pool` defaults to empty.

### `src/harness_config.rs`
`HarnessConfig` gets `#[serde(default)] pub config_version: u32` (default 0).
`lib.rs` re-exports `CredentialEntry`.

## Crate `harw-install`

Deps: `harw-home` (path), `harw-config` (path), `toml_edit`, `serde`,
`serde_json`, `jiff`, `getrandom`. Goal: no heavy runtime deps.

### `src/error.rs`
Enums `InstallError, ServiceError, DoctorError, UpdateError, MigrationError,
PathError` — each with `Display`/`Debug` (delegated)/`Error::source`/
`From<std::io::Error>` where it makes sense. `pub type XResult<T> = Result<T, XError>` per enum.

### `src/platform.rs`
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Os { Linux, MacOs, Windows, Other }
#[derive(Debug, Clone)] pub struct Platform {
    pub os: Os, pub container: bool, pub wsl: bool, pub termux: bool, pub managed: bool }
impl Platform { pub fn detect() -> Self; } // pure env/file probes, no error
```
Probes: `/.dockerenv`, `/proc/1/cgroup`, `WSL` env/`/proc/version`, `TERMUX_VERSION`,
`HARW_MANAGED`/Nix.

### `src/pathscope.rs`
```rust
pub struct PathScope { root: std::path::PathBuf }
impl PathScope {
    pub fn new(root: impl Into<std::path::PathBuf>) -> Self;
    pub fn resolve(&self, rel: &str) -> Result<std::path::PathBuf, PathError>; // rejects .. / absolute / symlink escape
    pub fn root(&self) -> &std::path::Path;
}
```

### `src/context.rs`
```rust
#[non_exhaustive] pub enum InstallMethod { Cargo, Homebrew, Standalone { release_dir: std::path::PathBuf }, Unknown }
pub struct InstallContext { pub method: InstallMethod, pub exe: std::path::PathBuf }
impl InstallContext { pub fn detect() -> Result<Self, InstallError>; } // std::env::current_exe + path/env markers
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
    fn render_unit(&self, spec: &ServiceSpec) -> String;             // PURE function
    fn install(&self, spec: &ServiceSpec) -> Result<(), ServiceError>;
    fn status(&self, name: &str) -> Result<ServiceStatus, ServiceError>;
    fn uninstall(&self, name: &str) -> Result<(), ServiceError>;
}
pub fn detect_service_manager(p: &crate::platform::Platform) -> Box<dyn ServiceManager>;
```
`service_systemd.rs`/`service_launchd.rs`/`service_schtasks.rs`: each a struct
implementing `ServiceManager`; `render_unit` is a pure function (unit/plist/
task-XML), `install`/`uninstall` write a file and call `systemctl --user`/
`launchctl`/`schtasks`. An `UnsupportedServiceManager` (in service.rs) returns
`ServiceError::Unsupported` on install/uninstall.

### `src/doctor.rs`
```rust
pub enum CheckOutcome { Ok(String), Warn(String), Fail(String) }
pub trait DoctorCheck { fn id(&self) -> &str; fn run(&self) -> CheckOutcome; }
pub fn default_checks(home: &std::path::Path) -> Vec<Box<dyn DoctorCheck>>; // system/os, bwrap, service manager, home perms, update status
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
    pub fn is_stale(&self, now: jiff::Timestamp, ttl_secs: i64) -> bool; // 20h throttle
}
```

### `src/migration.rs`
```rust
pub struct ConfigDocument { /* wraps toml_edit::DocumentMut; get_* / set_* / remove via segment paths, fail closed */ }
pub trait ConfigMigration { fn from_version(&self) -> u32; fn apply(&self, doc: &mut ConfigDocument) -> Result<(), MigrationError>; }
pub struct MigrationRunner { migrations: Vec<Box<dyn ConfigMigration>>, latest: u32 }
impl MigrationRunner {
    pub fn new(migrations: Vec<Box<dyn ConfigMigration>>, latest: u32) -> Self;
    /// Reads config.toml, applies migrations with from_version >= current,
    /// writes a config.toml.bak.<n> backup, persists config_version = latest.
    pub fn run(&self, config_path: &std::path::Path) -> Result<u32, MigrationError>;
}
```

### `src/uninstall.rs`
```rust
#[non_exhaustive] #[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UninstallScope { Service, State, Workspace, Binary }
pub struct CleanupPlan { pub removals: Vec<std::path::PathBuf>, pub preserved: Vec<std::path::PathBuf> }
pub fn plan(home: &std::path::Path, scopes: &[UninstallScope]) -> CleanupPlan; // pure computation
pub fn execute(plan: &CleanupPlan, dry_run: bool) -> Result<(), InstallError>;
```

### `src/lib.rs`
All modules `pub mod …;` plus re-exports of the main types.

## Crate `harw-provider-http` (extends `src/lib.rs`)

- `ProviderApi`-dependent transport selection: the existing `/responses` path
  stays for `OpenAiResponses`; a new `/chat/completions` path handles
  `OpenAiChat` (standard `messages` schema, `choices[0].message.content`).
- `from_config` additionally reads `config.harness.default_provider`'s
  `ProviderToml.api` (a string) to pick the transport; credential resolution
  is extended to understand `file-json:` refs.
- No signature change to `ModelProvider::respond`.

## Crate `harw-tui` (`src/setup.rs`)

```rust
pub enum SetupStage { Provider, Auth, Model, Done }
pub struct SetupApp { /* stage, providers, filter, selected, scroll, chosen_* */ }
pub struct SetupOutcome {
    pub provider_id: String, pub base_url: String, pub api: String,
    pub model: String, pub secret_ref: Option<String>,
}
impl SetupApp {
    pub fn new(catalog: Vec<harw_model_catalog::ProviderSpec>) -> Self;
    // I/O-free transitions, testable:
    pub fn on_key(&mut self, key: crossterm::event::KeyEvent);
    pub fn outcome(&self) -> Option<&SetupOutcome>;
}
pub fn run_setup(catalog: Vec<harw_model_catalog::ProviderSpec>) -> Result<Option<SetupOutcome>, TuiError>;
```
`lib.rs`: `pub mod setup; pub use setup::{SetupApp, SetupOutcome, run_setup};`
`harw-tui` depends on `harw-model-catalog` (path).

## Crate `harw-cli` (integration)

- `cli.rs`: subcommands `Completion{shell}`, `Update{--check}`,
  `Service{Install|Status|Uninstall, --dry-run}`, `Uninstall{--scope, --dry-run, --yes}`,
  `Catalog{--refresh}`.
- `completion.rs`: `clap_complete::generate` against `Cli::command()`.
- `main.rs`: dispatches the new commands to `harw-install` functions.
- `onboarding.rs`: interactively runs `harw_tui::run_setup(embedded_catalog())`
  and writes the resulting `SetupOutcome` (providers/models/credential_pool);
  non-interactive mode keeps its existing defaults.
- `chat.rs`: provider construction uses `credential_pool` first, falling back
  to the existing resolution path.

Additional deps: `harw-cli` += `harw-install`, `harw-model-catalog`, `clap_complete`.

## Verification command

```
RUSTC_WRAPPER= CARGO_TARGET_DIR=target cargo check --workspace --tests
```
