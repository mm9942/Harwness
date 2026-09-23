//! `harw-provider-http` — echte HTTP-Brücke zwischen den OpenAI-Wire-Typen aus
//! `harw-provider` und dem provider-neutralen `harw_core::ModelProvider`-Trait.
//!
//! ## Verantwortung
//! Diese Crate besitzt die konkrete, netzwerkgestützte Implementierung des
//! Turn-Loop-Traits: sie übersetzt einen [`harw_core::ModelRequest`] in eine
//! `ResponsesRequest`, sendet `POST {base_url}/responses` mit
//! `Authorization: Bearer <key>` und projiziert die Antwort zurück in eine
//! [`harw_core::ModelResponse`].
//!
//! ## Nebenläufigkeit
//! [`OpenAiResponsesProvider`] ist `Send + Sync` (der `reqwest::Client` ist
//! intern geteilt und klont günstig). `respond` liefert ein `Box::pin`-Future
//! gemäß dem `ModelFuture`-Alias.
//!
//! ## Fehler
//! Konstruktions- und Auflösungsfehler laufen über [`HttpProviderError`];
//! Laufzeitfehler in `respond` werden in [`harw_core::ModelError`] übersetzt
//! (W4a / A-OAI: `Transient`/`Auth`/`QuotaExceeded`/`ContextLength`/`Timeout`,
//! siehe `error::model_error_for_status`).
//!
//! ## Credential-Pool (`[[credential_pool.<provider>]]`)
//! `provider.auth` (die einzelne `SecretRef`) — bei Anthropic zusätzlich die
//! implizite Umgebungs-Auflösung, wenn `provider.auth` fehlt — ist **immer**
//! das zuerst versuchte Credential. Ein nicht-leerer `auth.credential_pool`
//! (mehrere `CredentialEntry`, z. B. mehrere `openai`-Keys mit getrennten
//! Kontingenten) liefert nur **Failover-Kandidaten**, die erst nach einem
//! Auth-/Kontingent-Fehler des primären Credentials probiert werden, dann
//! aufsteigend nach `CredentialEntry::priority`, stabil nach Datei-Reihenfolge
//! (siehe [`harw_config::AuthConfig::credential_pool_ordered`]). Fehlt
//! `provider.auth`, wird stattdessen der erste Pool-Eintrag primär. Ist der
//! Pool leer oder für den Provider nicht konfiguriert, ändert sich nichts.
//! Schlägt der aktive Eintrag mit 401/403 oder Kontingent-Erschöpfung fehl,
//! wird er bounded (60 s) abgekühlt und derselbe Request **genau einmal**
//! mit dem nächsten Eintrag wiederholt (siehe `credential_pool`-Modul).
//!
//! ## Wire-Vertrag (W4a / A-OAI)
//! - **Tool-Ergebnisse** gehen ausschließlich über
//!   [`harw_core::envelope::render_tool_result`] auf den Wire (Trust-Hülle,
//!   Byte-Deckel `ModelRequest::tool_result_max_bytes`).
//! - **Datenblock** (`ModelRequest::data_block`): eigenes `user`-Item bzw.
//!   `user`-Nachricht **am Ende** des Inputs, also nach allen
//!   `function_call_output`-Items — der Verlaufspräfix bleibt cache-stabil.
//! - **Responses-API:** Reasoning-Items (`type: "reasoning"`) werden unverändert
//!   als `OpaqueReasoning{provider,..}` in `ModelResponse::reasoning` gemeldet;
//!   Items mit `encrypted_content` werden je Tool-Call-Runde provider-lokal
//!   gemerkt und vor den zugehörigen `function_call`-Items zurückgespielt
//!   (`store: false`, `include: ["reasoning.encrypted_content"]`).
//!   `status: "incomplete"` + `incomplete_details.reason` → `StopReason::MaxTokens`
//!   bzw. `ContentFilter`; unvollständige Tool-Calls werden verworfen.
//! - **Chat-Completions:** `reasoning_effort` (top-level), `max_completion_tokens`,
//!   `finish_reason` `stop|length|tool_calls|content_filter` → `StopReason`.
//! - Wiederholung vorübergehender Fehler: [`RetryingProvider`] (Modul [`retry`]).
//!
//! ## Sicherheit
//! Der API-Key liegt in `secrecy::SecretString` und wird ausschließlich beim
//! Setzen des `Authorization`-Headers via `ExposeSecret` offengelegt — niemals
//! geloggt.
//!
//! # Examples
//! ```rust,no_run
//! use harw_provider_http::{HttpProviderError, OpenAiResponsesProvider};
//! use secrecy::SecretString;
//!
//! # fn main() -> Result<(), HttpProviderError> {
//! let provider = OpenAiResponsesProvider::new(
//!     "https://api.openai.com/v1",
//!     "gpt-4o",
//!     SecretString::new("sk-...".into()),
//! )?;
//! # let _ = provider;
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod anthropic;
mod anthropic_caps;
pub mod cache_strategy;
pub mod discovery;
mod error;
pub mod rate_limiter;
pub mod retry;
pub mod routing;
mod sse;
mod text_tool_calls;
mod tool_names;

use error::{model_error_for_status, model_error_for_transport, retry_after_hint};
use harw_core::envelope::render_tool_result;
use harw_core::model::StopReason;
use harw_core::{
    ModelError, ModelFuture, ModelProvider, ModelRequest, ModelResponse, RequestIdentity,
    ToolCallResult,
};
use harw_protocol::{OpaqueReasoning, ResultTrust, TurnItem};
use harw_provider::openai::{ContentPart, InputItem, ReasoningConfig, ResponsesRequest, ToolDef};
use harw_sandbox::{EgressHost, EgressUrl};
use harw_tools::{FunctionToolSpec, JsonSchema, ToolCall, ToolName, ToolSpec};
use harw_types::{TokenUsage, ToolCallId};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

pub use anthropic::{
    ANTHROPIC_SUBSCRIPTION_TOKEN_WARNING, AnthropicCredential, AnthropicMessagesProvider,
    DEFAULT_ANTHROPIC_BASE_URL, anthropic_messages_url, build_messages_body,
    extract_anthropic_text,
};
pub use error::{HttpProviderError, HttpProviderResult};
pub use retry::{
    JitterSource, RetryDecision, RetryPolicy, RetrySleeper, RetryingProvider, SleepFuture,
    StdJitter, ThreadSleeper, retry_decision,
};
pub use routing::RoutingModelProvider;

mod codex;
mod credential_pool;

/// Synchronously resolves a `secrets:` credential reference.
///
/// Implementations must return a [`SecretString`] on success. On failure, the
/// returned string must be a safe diagnostic and must not contain the secret
/// value or other sensitive material.
pub trait SecretResolver {
    /// Resolves the identifier after the `secrets:` prefix.
    fn resolve(&self, reference: &str) -> Result<SecretString, String>;
}

/// Matches the current default agent timeout while ensuring a provider call
/// cannot wait indefinitely when constructed outside of an agent runtime.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const SECRET_RESOLVER_FAILURE_REASON: &str = "secret resolver failed";
const EMPTY_CREDENTIAL_REASON: &str = "credential is empty";
const INVALID_KEYRING_REFERENCE_REASON: &str =
    "invalid keyring reference (expected keyring:<service>/<account>)";
const KEYRING_FAILURE_REASON: &str =
    "keyring credential unavailable (no entry for service/account or system keyring not reachable)";
// Gründe für `file:`/`file-json:`-Fehler: bewusst ohne Pfad und ohne Inhalt,
// damit Fehlertexte (UI, Telegram, Logs) kein Datei-Orakel werden.
const FILE_CREDENTIAL_NO_HOME_REASON: &str =
    "file credentials require the harw home directory (see build_provider_with_home)";
const FILE_CREDENTIAL_OUTSIDE_SECRETS_REASON: &str =
    "file credential must be an absolute path below <home>/secrets";
const FILE_CREDENTIAL_OPEN_REASON: &str =
    "file credential could not be opened without following symlinks";
const FILE_CREDENTIAL_NOT_PRIVATE_REASON: &str = "file credential must be a regular file owned by the current user without group or other permissions";
const FILE_CREDENTIAL_READ_REASON: &str =
    "file credential is unreadable, larger than 64 KiB or not UTF-8";
const FILE_CREDENTIAL_JSON_REASON: &str = "file credential is not valid JSON";
const FILE_CREDENTIAL_POINTER_REASON: &str = "JSON pointer missing or not a string";
/// Obergrenze für eine Credential-Datei; Tokens und `credentials.json` sind klein.
const MAX_FILE_CREDENTIAL_BYTES: u64 = 64 * 1024;
/// Höchstzahl gefolgter Redirects innerhalb desselben Ursprungs (wie reqwests
/// Default `Policy::limited(10)`).
const MAX_REDIRECTS: usize = 10;
const REDIRECT_CROSS_ORIGIN_REASON: &str =
    "provider redirect to a different scheme, host or port was blocked";
const REDIRECT_LIMIT_REASON: &str = "provider redirect limit exceeded";
const INVALID_CREDENTIAL_HEADER_REASON: &str =
    "provider credential is not a valid HTTP header value";
/// Byte-Deckel je gerendertem Tool-Ergebnis, wenn der Request keinen setzt
/// (`ModelRequest::tool_result_max_bytes == None`). Für den ganzen Verlauf
/// gleich, damit bereits gesendete Ergebnisse bytegleich (cache-stabil) bleiben.
const DEFAULT_TOOL_RESULT_MAX_BYTES: usize = 256 * 1024;
/// Tool-Name im Envelope, wenn der Verlauf den zugehörigen Call nicht enthält.
const UNKNOWN_TOOL_NAME: &str = "unknown";
/// `include`-Wert der Responses-API für zustandslos wiederverwendbares Reasoning.
const INCLUDE_ENCRYPTED_REASONING: &str = "reasoning.encrypted_content";
/// Höchstzahl gemerkter Reasoning-Runden je Provider (FIFO).
const MAX_REASONING_REPLAY_ENTRIES: usize = 64;
/// Obergrenze für provider-gelieferte Grund-Strings in `StopReason::Other`.
const MAX_STOP_REASON_CHARS: usize = 64;

/// Quellen, aus denen Credential-Referenzen aufgelöst werden.
#[derive(Clone, Copy)]
struct SecretSources<'a> {
    /// Env-Layer aus den `.env`-Dateien der Konfigurations-Layer.
    env_layer: &'a BTreeMap<String, String>,
    /// Injizierter Resolver für `secrets:`.
    resolver: Option<&'a dyn SecretResolver>,
    /// harw-Home (`~/.harw` bzw. `HARW_HOME`). `file:`/`file-json:` werden nur
    /// unterhalb von `<home>/secrets/` gelesen; ohne Home schlagen sie fehl.
    /// Ausnahme: bekannte CLI-Credential-Dateien, siehe [`EXTERNAL_CLI_CREDENTIALS`].
    home: Option<&'a Path>,
    /// Endpoint des Providers, für den gerade aufgelöst wird. Bindet externe
    /// CLI-Credentials an ihren offiziellen Host; `None` erlaubt keine.
    endpoint: Option<&'a str>,
}

/// Bekannte Credential-Dateien anderer CLIs, die `harw onboard`/`harw auth
/// import` erkennt (`harw_model_catalog::detect_local_sources`).
///
/// Sie liegen außerhalb von `<home>/secrets` und werden nur gelesen, wenn Pfad
/// (relativ zu `$HOME`) **und** JSON-Pointer exakt passen und der Provider-
/// Endpoint `https://<host>` eines der offiziellen Hosts ist. Eine repo-lokale
/// Provider-TOML kann den Token damit nicht an einen fremden Host lenken.
const EXTERNAL_CLI_CREDENTIALS: &[(&str, &[&str], &[&str])] = &[
    (
        ".codex/auth.json",
        &["/OPENAI_API_KEY"],
        &["api.openai.com"],
    ),
    (
        ".codex/auth.json",
        &["/tokens/access_token"],
        &["chatgpt.com"],
    ),
    (
        ".claude/.credentials.json",
        &["/claudeAiOauth/accessToken"],
        &[anthropic::ANTHROPIC_API_HOST],
    ),
];

/// Baut den passenden [`ModelProvider`] aus der aufgelösten Konfiguration.
///
/// # Description
/// Wählt anhand des `api`-Felds des Default-Providers den Transport:
/// - `"anthropic-messages"` → **nativer** [`AnthropicMessagesProvider`]
///   (Claude-nativ). Credential- und Endpoint-Auflösung erfolgt env-first
///   (siehe unten).
/// - alle anderen (`"openai-responses"`/`"openai-chat"`/…) →
///   [`OpenAiResponsesProvider`] (unverändert).
///
/// ## Auflösung für `anthropic-messages`
/// 1. **Foundry** (`provider == "foundry"`): Base-URL aus
///    `ANTHROPIC_FOUNDRY_BASE_URL`, Key aus `ANTHROPIC_FOUNDRY_API_KEY`
///    (`x-api-key`). Beide Pflicht.
/// 2. **Anthropic-direkt** (sonst): `CLAUDE_CODE_OAUTH_TOKEN` → OAuth-Bearer;
///    sonst `ANTHROPIC_API_KEY` → `x-api-key`; sonst die `auth`-`SecretRef`
///    des Providers; sonst Fehler.
///
/// # Errors
/// - [`HttpProviderError::MissingDefault`]: fehlender Provider/Modell-Default.
/// - [`HttpProviderError::MissingEnv`]: fehlende Foundry-/Anthropic-Variable.
/// - [`HttpProviderError::UnresolvedCredential`]: `SecretRef` unauflösbar.
/// - [`HttpProviderError::UnsupportedCredentialReference`]: `secrets:` wird
///   ohne injizierten Resolver nicht aufgelöst.
///
/// Ohne Home-Verzeichnis schlagen `file:`/`file-json:`-Referenzen fail-closed
/// fehl; dafür [`build_provider_with_home`] verwenden.
///
/// # Concurrency
/// Reiner Aufbau; das Ergebnis ist `Send + Sync`.
pub fn build_provider(
    config: &harw_config::ResolvedConfig,
) -> HttpProviderResult<Box<dyn ModelProvider>> {
    build_provider_with_optional_resolver(config, None, None).map(|(provider, _)| provider)
}

/// Builds configured providers with an injected synchronous `secrets:` resolver.
///
/// Wie [`build_provider`] ohne Home: `file:`/`file-json:` schlagen fehl.
pub fn build_provider_with_resolver(
    config: &harw_config::ResolvedConfig,
    resolver: &dyn SecretResolver,
) -> HttpProviderResult<Box<dyn ModelProvider>> {
    build_provider_with_optional_resolver(config, Some(resolver), None)
        .map(|(provider, _)| provider)
}

/// Wie [`build_provider`], liefert zusätzlich eine [`ProviderLoadRegistry`]
/// mit den [`ProviderLoadControl`]-Handles aller Provider, die diese
/// Fähigkeit unterstützen (aktuell: alle OpenAI-kompatiblen Backends, siehe
/// [`ProviderLoadRegistry`]-Doku).
///
/// # Composition-Root-Hinweis
/// Gedacht für den Wiring-Punkt, der `harw_operations::context::ServiceMap`
/// zusammenbaut (siehe `harw-runtime/src/assembly.rs` bzw. `services.rs`):
/// `services.insert(registry)` macht die Handles für `harw-ops` erreichbar
/// (siehe `harw-ops::provider`/`harw-ops::status`, die `ctx.service::<
/// ProviderLoadRegistry>()` abfragen). `build_provider`/
/// `build_provider_with_resolver`/`build_provider_with_home` bleiben
/// unverändert (verwerfen die Registry) — bestehende Aufrufer sind von
/// dieser Erweiterung nicht betroffen.
pub fn build_provider_with_load_registry(
    config: &harw_config::ResolvedConfig,
) -> HttpProviderResult<(Box<dyn ModelProvider>, ProviderLoadRegistry)> {
    build_provider_with_optional_resolver(config, None, None)
}

/// Wie [`build_provider_with_load_registry`] mit injiziertem `secrets:`-Resolver.
pub fn build_provider_with_load_registry_and_resolver(
    config: &harw_config::ResolvedConfig,
    resolver: &dyn SecretResolver,
) -> HttpProviderResult<(Box<dyn ModelProvider>, ProviderLoadRegistry)> {
    build_provider_with_optional_resolver(config, Some(resolver), None)
}

/// Wie [`build_provider_with_load_registry`] mit bekanntem harw-Home (siehe
/// [`build_provider_with_home`] für die `file:`/`file-json:`-Auflösung).
pub fn build_provider_with_load_registry_and_home(
    config: &harw_config::ResolvedConfig,
    home: &Path,
    resolver: Option<&dyn SecretResolver>,
) -> HttpProviderResult<(Box<dyn ModelProvider>, ProviderLoadRegistry)> {
    build_provider_with_optional_resolver(config, resolver, Some(home))
}

/// Baut die konfigurierten Provider mit bekanntem harw-Home.
///
/// # Arguments
/// - `config`: aufgelöste Konfiguration.
/// - `home` (`&Path`): Root-Space (`~/.harw` bzw. `HARW_HOME`). `file:`- und
///   `file-json:`-Referenzen werden nur unterhalb von `<home>/secrets/`
///   gelesen: symlinkfrei (`open_dir_nofollow` + `open_beneath`), nur
///   reguläre Dateien des effektiven Nutzers ohne Gruppen-/Fremdrechte.
///   Ausnahme: allowlistete CLI-Credential-Dateien (`~/.codex/auth.json`,
///   `~/.claude/.credentials.json`, siehe `EXTERNAL_CLI_CREDENTIALS`) werden
///   auch außerhalb von `<home>/secrets` gelesen — aber nur, wenn Pfad **und**
///   JSON-Pointer exakt zur Allowlist passen und der aufrufende Provider den
///   offiziellen Host des jeweiligen CLI-Anbieters anspricht.
/// - `resolver`: optionaler `secrets:`-Resolver.
///
/// # Errors
/// Wie [`build_provider`].
pub fn build_provider_with_home(
    config: &harw_config::ResolvedConfig,
    home: &Path,
    resolver: Option<&dyn SecretResolver>,
) -> HttpProviderResult<Box<dyn ModelProvider>> {
    build_provider_with_optional_resolver(config, resolver, Some(home))
        .map(|(provider, _)| provider)
}

fn build_provider_with_optional_resolver(
    config: &harw_config::ResolvedConfig,
    resolver: Option<&dyn SecretResolver>,
    home: Option<&Path>,
) -> HttpProviderResult<(Box<dyn ModelProvider>, ProviderLoadRegistry)> {
    let sources = SecretSources {
        env_layer: &config.env_layer,
        resolver,
        home,
        endpoint: None,
    };
    let provider_name = config.harness.default_provider.as_deref().ok_or_else(|| {
        HttpProviderError::MissingDefault {
            what: "default_provider".to_owned(),
        }
    })?;
    let model = config.harness.default_model.as_deref().ok_or_else(|| {
        HttpProviderError::MissingDefault {
            what: "default_model".to_owned(),
        }
    })?;

    let mut providers = BTreeMap::new();
    let mut load_controls: ProviderLoadRegistry = BTreeMap::new();
    for (name, provider) in config
        .providers
        .iter()
        .filter(|(_, provider)| provider.enabled)
        .collect::<BTreeMap<_, _>>()
    {
        let backend = match build_named_provider(name, provider, config, model, sources) {
            Ok((backend, load_control)) => {
                if let Some(load_control) = load_control {
                    load_controls.insert(name.to_owned(), load_control);
                }
                backend
            }
            Err(error) if name != provider_name => Box::new(UnavailableProvider(error.to_string())),
            Err(error) => return Err(error),
        };
        providers.insert(name.to_owned(), backend);
    }

    Ok((
        Box::new(RoutingModelProvider::new(providers, provider_name)?),
        load_controls,
    ))
}

struct UnavailableProvider(String);

impl ModelProvider for UnavailableProvider {
    fn respond<'a>(&'a self, _request: ModelRequest) -> ModelFuture<'a> {
        Box::pin(async move { Err(ModelError::RequestFailed(self.0.clone())) })
    }
}

/// Constructs one configured backend with its provider-local defaults.
///
/// The default provider uses the harness-wide default model. Every other
/// provider uses its explicit model list, falling back to its discovered model
/// definitions in lexicographic order. This keeps
/// construction deterministic while [`ModelRequest::model_id`] remains the
/// request-time override.
/// Builds the [`RetryPolicy`] applied to every real HTTP-backed provider
/// (Anthropic and the OpenAI-compatible fallback) constructed by
/// [`build_named_provider`], so a timed-out or transient/rate-limited model
/// request is retried after a delay instead of failing the chat turn
/// immediately (see module doc, "Wiederholung vorübergehender Fehler").
///
/// `base_delay = 10s` is chosen because [`RetryPolicy::backoff_delay`] (retry_index
/// `0`) computes `cap = min(max_delay, base_delay * 2^0) = base_delay`, then
/// jitters to `half + half*unit` where `half = cap / 2` — i.e. the first
/// retry actually waits somewhere in `[5s, 10s)`, matching the desired
/// "retry after ~5 seconds" behaviour. `max_attempts: 3` allows one initial
/// attempt plus two retries; `max_delay: 20s` caps the (unused, since only
/// two retries occur) further exponential growth; `max_retry_after: 60s`
/// still honours a server-provided `Retry-After` header up to a minute.
fn network_retry_policy() -> RetryPolicy {
    RetryPolicy {
        max_attempts: 3,
        base_delay: Duration::from_secs(10),
        max_delay: Duration::from_secs(20),
        max_retry_after: Duration::from_secs(60),
    }
}

/// Rückgabe von [`build_named_provider`]: der gebaute Provider-Backend plus
/// dessen optionales [`ProviderLoadControl`]-Handle.
type NamedProviderBuild = (
    Box<dyn ModelProvider>,
    Option<std::sync::Arc<dyn ProviderLoadControl>>,
);

/// Builds one named provider backend, plus its [`ProviderLoadControl`] handle
/// when the backend supports it.
///
/// # Returns
/// `Some` load-control handle for both HTTP-backed branches: the
/// OpenAI-compatible branch (the concrete [`OpenAiResponsesProvider`] always
/// builds a [`DynamicConcurrencyLimiter`], see
/// [`OpenAiResponsesProvider::from_named_config`]-Doku) and the
/// `anthropic-messages` branch, where [`AnthropicMessagesProvider::configure_concurrency`]
/// installs the same [`DynamicConcurrencyLimiter`] type before the backend is
/// moved into [`retry::RetryingProvider`] (Anthropic-Parität for W6b's
/// rate-limit-visibility slice, previously OpenAI-only).
fn build_named_provider(
    provider_name: &str,
    provider: &harw_config::ProviderToml,
    config: &harw_config::ResolvedConfig,
    default_model: &str,
    sources: SecretSources<'_>,
) -> HttpProviderResult<NamedProviderBuild> {
    validate_endpoint(&provider.base_url)?;
    let sources = SecretSources {
        endpoint: Some(&provider.base_url),
        ..sources
    };
    if !matches!(
        provider.auth_header.as_deref(),
        None | Some("bearer" | "api-key" | "x-api-key" | "none")
    ) {
        return Err(HttpProviderError::Decode("unsupported auth_header".into()));
    }
    let model = if config.harness.default_provider.as_deref() == Some(provider_name) {
        default_model
    } else {
        provider
            .models
            .first()
            .map(String::as_str)
            .or_else(|| {
                config
                    .models
                    .values()
                    .filter(|model| model.provider == provider_name)
                    .map(|model| model.id.as_str())
                    .min()
            })
            .ok_or_else(|| HttpProviderError::MissingDefault {
                what: format!("first configured model for provider '{provider_name}'"),
            })?
    };

    if provider.api == "anthropic-messages" {
        // Credential-Pool liefert nur Failover-Kandidaten hinter dem
        // primären Credential (siehe Modul-Doku „Credential-Pool" und
        // `credential_pool`-Moduldoku). Jeder Pool-Eintrag wird nach
        // demselben Schema klassifiziert wie eine explizite `provider.auth`-
        // `SecretRef`: `auth_header == "bearer"` → OAuth-Bearer, sonst
        // Präfix-Erkennung über [`classify_anthropic_secret`].
        let auth_header_is_bearer = provider.auth_header.as_deref() == Some("bearer");
        let pool: Option<credential_pool::CredentialPool<AnthropicCredential>> =
            credential_pool::CredentialPool::from_auth_config(
                &config.auth,
                provider_name,
                sources,
                |secret| {
                    Ok(if auth_header_is_bearer {
                        AnthropicCredential::Bearer(secret)
                    } else {
                        classify_anthropic_secret(secret.expose_secret().to_owned())
                    })
                },
            )?;
        if let Some(pool) = &pool {
            for index in 0..pool.len() {
                if let Some(url) = &pool.entry(index).base_url {
                    validate_endpoint(url)?;
                }
            }
        }
        // `resolve_anthropic` ist der primäre Weg (explizite `provider.auth`
        // oder implizite Umgebungs-Auflösung) und wird immer zuerst
        // versucht. Gelingt er und ist ein Pool konfiguriert, wird das
        // Ergebnis als Index 0 vor die Pool-Einträge gestellt
        // (`prepend_primary`); scheitert er (kein `provider.auth`, keine
        // passende implizite Auflösung), übernimmt stattdessen der erste
        // Pool-Eintrag als primäres Credential — fehlt auch der, wird der
        // Fehler von `resolve_anthropic` durchgereicht.
        let primary = resolve_anthropic(provider_name, provider, sources, &env_nonempty);
        let (base_url, credential, pool) = match (primary, pool) {
            (Ok((base_url, credential)), Some(pool)) => {
                let pool = pool.prepend_primary(credential, format!("{provider_name}#primary"));
                tracing::debug!(
                    provider = provider_name,
                    failover_entries = pool.len() - 1,
                    "credential_pool.configured_as_failover"
                );
                let index = pool.select(None).unwrap_or(0);
                let entry = pool.entry(index);
                let selected_base_url = entry.base_url.clone().unwrap_or(base_url);
                (selected_base_url, entry.value.clone(), Some(pool))
            }
            (Ok((base_url, credential)), None) => (base_url, credential, None),
            (Err(_), Some(pool)) => {
                tracing::debug!(
                    provider = provider_name,
                    "credential_pool.primary_absent_first_pool_entry_promoted"
                );
                let index = pool.select(None).unwrap_or(0);
                let entry = pool.entry(index);
                let base_url = entry.base_url.clone().unwrap_or_else(|| {
                    if provider.base_url.trim().is_empty() {
                        DEFAULT_ANTHROPIC_BASE_URL.to_owned()
                    } else {
                        provider.base_url.clone()
                    }
                });
                let credential = entry.value.clone();
                (base_url, credential, Some(pool))
            }
            (Err(error), None) => return Err(error),
        };
        let mut backend =
            AnthropicMessagesProvider::from_base(&base_url, model.to_owned(), credential)?;
        backend.configure(
            provider_name,
            configured_headers(provider_name, &provider.headers, sources)?,
        );
        backend.configure_rate_limit(provider.rate_limit.clone());
        backend.configure_stream_policy(sse::StreamPolicy::from_config(
            provider_name,
            provider,
            config,
        ));
        backend.configure_credential_pool(pool);
        backend.configure_concurrency(provider.max_concurrency);
        // Beide `Arc`s werden geklont, *bevor* `backend` unten per Wert in
        // `RetryingProvider::new` verschoben wird — siehe
        // [`ProviderLoadHandle`]-Doku (identisches Muster zum
        // OpenAI-kompatiblen Zweig unten).
        let load_control: std::sync::Arc<dyn ProviderLoadControl> =
            std::sync::Arc::new(ProviderLoadHandle {
                provider_id: provider_name.to_owned(),
                concurrency_limiter: backend.concurrency_limiter(),
                rate_limiter: backend.rate_limiter_handle(),
            });
        return Ok((
            Box::new(RetryingProvider::new(backend, network_retry_policy())),
            Some(load_control),
        ));
    }

    let http_provider = OpenAiResponsesProvider::from_named_config(
        provider_name,
        provider,
        config,
        model,
        sources,
    )?;
    // Beide `Arc`s werden geklont, *bevor* `http_provider` unten per Wert in
    // `RetryingProvider::new` verschoben wird — siehe [`ProviderLoadHandle`]-Doku.
    let load_control: std::sync::Arc<dyn ProviderLoadControl> =
        std::sync::Arc::new(ProviderLoadHandle {
            provider_id: provider_name.to_owned(),
            concurrency_limiter: http_provider.concurrency_limiter(),
            rate_limiter: http_provider.rate_limiter_handle(),
        });
    Ok((
        Box::new(RetryingProvider::new(http_provider, network_retry_policy())),
        Some(load_control),
    ))
}

/// Löst Base-URL + Credential für den nativen Anthropic-Weg auf.
///
/// Reihenfolge: explizite `auth`-Referenz; sonst implizite Umgebungs-
/// Credentials, aber **nur** an gebundene Hosts (W1-06b):
/// - Anthropic-direkt: `CLAUDE_CODE_OAUTH_TOKEN`, dann `ANTHROPIC_API_KEY`,
///   nur wenn der Endpoint exakt `https://api.anthropic.com` (Port 443) ist.
/// - Foundry: `ANTHROPIC_FOUNDRY_API_KEY` nur, wenn der Endpoint dasselbe
///   Schema, denselben Host und Port wie `ANTHROPIC_FOUNDRY_BASE_URL` aus der
///   **Prozess**-Umgebung hat (nie aus dem Env-Layer, den ein Repo liefern kann).
///
/// `process_env` liest die Prozess-Umgebung (Produktion: [`env_nonempty`]);
/// als Parameter, damit Tests ohne `std::env::set_var` (in Edition 2024
/// `unsafe`) auskommen.
fn resolve_anthropic(
    provider_name: &str,
    provider: &harw_config::ProviderToml,
    sources: SecretSources<'_>,
    process_env: &dyn Fn(&str) -> Option<String>,
) -> HttpProviderResult<(String, AnthropicCredential)> {
    let env_layer = sources.env_layer;
    // Persisted setup is authoritative. Legacy environment values are only
    // fallbacks for old Foundry profiles that never stored an endpoint/key.
    if provider_name == "foundry" || provider_name.starts_with("foundry-") {
        let base_url = if provider.base_url.is_empty() || provider.base_url.contains('<') {
            harw_config::dotenv::resolve_env_ref("ANTHROPIC_FOUNDRY_BASE_URL", env_layer)
                .ok_or_else(|| HttpProviderError::MissingEnv {
                    var: "ANTHROPIC_FOUNDRY_BASE_URL".into(),
                })?
        } else {
            provider.base_url.clone()
        };
        let secret = if let Some(reference) = &provider.auth {
            resolve_secret(reference, sources)?
        } else {
            let env_endpoint = process_env("ANTHROPIC_FOUNDRY_BASE_URL");
            if !env_endpoint
                .as_deref()
                .is_some_and(|env_endpoint| same_origin(&base_url, env_endpoint))
            {
                return Err(HttpProviderError::MissingDefault {
                    what: format!(
                        "auth for provider '{provider_name}' (ANTHROPIC_FOUNDRY_API_KEY is only used when base_url matches ANTHROPIC_FOUNDRY_BASE_URL)"
                    ),
                });
            }
            let key = harw_config::dotenv::resolve_env_ref("ANTHROPIC_FOUNDRY_API_KEY", env_layer)
                .ok_or_else(|| HttpProviderError::MissingEnv {
                    var: "ANTHROPIC_FOUNDRY_API_KEY".into(),
                })?;
            SecretString::new(key.into())
        };
        let credential = if provider.auth_header.as_deref() == Some("bearer") {
            AnthropicCredential::Bearer(secret)
        } else {
            AnthropicCredential::ApiKey(secret)
        };
        return Ok((base_url, credential));
    }

    // 2. Anthropic-direkt — Base-URL aus Provider (Default falls leer).
    let base_url = if provider.base_url.trim().is_empty() {
        DEFAULT_ANTHROPIC_BASE_URL.to_owned()
    } else {
        provider.base_url.clone()
    };

    // 2a. Explizit im Provider konfigurierte `auth`-SecretRef **hat Vorrang**.
    //     Damit kann der Nutzer via `providers/anthropic.toml` gezielt einen
    //     bestimmten Credential-Refs wählen (z. B. `file-json:` auf Claude-Code-
    //     Credentials), auch wenn `ANTHROPIC_API_KEY` in der Umgebung mit einem
    //     Cloudflare-Gateway-Key belegt ist.
    if let Some(secret_ref) = &provider.auth {
        let secret = resolve_secret(secret_ref, sources)?;
        return Ok((
            base_url,
            if provider.auth_header.as_deref() == Some("bearer") {
                AnthropicCredential::Bearer(secret)
            } else {
                classify_anthropic_secret(secret.expose_secret().to_owned())
            },
        ));
    }
    // Implizite Umgebungs-Credentials gehen nur an den offiziellen API-Host —
    // sonst könnte ein repo-lokales `providers/anthropic.toml` mit fremder
    // `base_url` (und ohne `auth`) den Token des Nutzers abgreifen.
    if !endpoint_is_official_host(&base_url, anthropic::ANTHROPIC_API_HOST) {
        return Err(HttpProviderError::MissingDefault {
            what: format!(
                "auth for provider '{provider_name}' (CLAUDE_CODE_OAUTH_TOKEN/ANTHROPIC_API_KEY are only used for https://{})",
                anthropic::ANTHROPIC_API_HOST
            ),
        });
    }
    // 2b. Setup-Token / OAuth (Kompatibilität mit Claude Code).
    if let Some(token) = process_env("CLAUDE_CODE_OAUTH_TOKEN") {
        return Ok((base_url, classify_anthropic_secret(token)));
    }
    // 2c. Klassischer API-Key aus der Umgebung — mit Prefix-Detection:
    //    `sk-ant-oat…` = OAuth-Token (Bearer), sonst normaler API-Key (x-api-key).
    if let Some(key) = process_env("ANTHROPIC_API_KEY") {
        return Ok((base_url, classify_anthropic_secret(key)));
    }

    Err(HttpProviderError::MissingEnv {
        var: "ANTHROPIC_API_KEY".to_owned(),
    })
}

/// Klassifiziert einen Anthropic-Credential-String nach seinem Präfix.
///
/// - `sk-ant-oat…` → OAuth-Bearer-Token (Header `Authorization: Bearer …`).
/// - alles andere → klassischer API-Key (Header `x-api-key`).
///
/// Diese Detection ist notwendig, weil Nutzer OAuth-Tokens gelegentlich in
/// der `ANTHROPIC_API_KEY`-Env-Variable ablegen; ohne Prefix-Check würde der
/// Token als `x-api-key` gesendet und von Anthropic mit HTTP 401 abgelehnt.
fn classify_anthropic_secret(raw: String) -> AnthropicCredential {
    if raw.starts_with("sk-ant-oat") {
        AnthropicCredential::OAuth(SecretString::new(raw.into()))
    } else {
        AnthropicCredential::ApiKey(SecretString::new(raw.into()))
    }
}

/// Liest eine Umgebungsvariable und behandelt leere Werte wie „nicht gesetzt".
fn env_nonempty(var: &str) -> Option<String> {
    std::env::var(var)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// `true`, wenn `endpoint` exakt `https://<official_host>` auf Port 443 ist.
///
/// Der Host wird über [`EgressUrl`] normalisiert (Kleinschreibung, IDNA,
/// abschließender Punkt entfernt); Subdomains, Suffixe und andere Ports
/// zählen nicht.
fn endpoint_is_official_host(endpoint: &str, official_host: &str) -> bool {
    EgressUrl::parse(endpoint).is_ok_and(|url| {
        url.is_https()
            && url.port() == 443
            && matches!(url.host(), EgressHost::Domain(host) if host == official_host)
    })
}

/// `true`, wenn beide URLs gültige [`EgressUrl`]s mit gleichem Schema, Host
/// und Port sind.
fn same_origin(left: &str, right: &str) -> bool {
    match (EgressUrl::parse(left), EgressUrl::parse(right)) {
        (Ok(left), Ok(right)) => {
            left.is_https() == right.is_https()
                && left.host() == right.host()
                && left.port() == right.port()
        }
        _ => false,
    }
}

/// Entscheidung der Redirect-Policy für einen einzelnen Redirect-Schritt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RedirectDecision {
    /// Gleicher Ursprung, Limit nicht erreicht.
    Follow,
    /// Schema, Host oder Port weicht vom Ursprung der Anfrage ab.
    RejectCrossOrigin,
    /// Mehr als [`MAX_REDIRECTS`] Redirects.
    RejectLimit,
}

/// Entscheidet, ob reqwest einem Redirect folgen darf.
///
/// # Description
/// reqwest entfernt bei Host-/Port-Wechsel nur `Authorization`, `Cookie`,
/// `Proxy-Authorization` und `WWW-Authenticate` (`reqwest 0.12.28
/// src/redirect.rs`, `remove_sensitive_headers`), **nicht** `x-api-key`/
/// `api-key`. Deshalb wird jeder Redirect abgelehnt, dessen Schema, Host oder
/// Port von einer der bisherigen URLs abweicht; Redirects auf demselben
/// Ursprung (z. B. Pfad-Normalisierung eines Gateways) bleiben erlaubt.
///
/// # Arguments
/// - `next`: Ziel des Redirects.
/// - `previous`: bisherige URLs der Kette; das erste Element ist die
///   ursprüngliche Anfrage (reqwest `Policy::redirect`, Kommentar zu
///   `PolicyKind::Limit`). Leer → fail closed.
fn redirect_decision(next: &reqwest::Url, previous: &[reqwest::Url]) -> RedirectDecision {
    let Some(origin) = previous.first() else {
        return RedirectDecision::RejectCrossOrigin;
    };
    let same_as_origin = |url: &reqwest::Url| {
        url.scheme() == origin.scheme()
            && url.host_str() == origin.host_str()
            && url.port_or_known_default() == origin.port_or_known_default()
    };
    if !same_as_origin(next) || !previous.iter().all(same_as_origin) {
        return RedirectDecision::RejectCrossOrigin;
    }
    if previous.len() > MAX_REDIRECTS {
        return RedirectDecision::RejectLimit;
    }
    RedirectDecision::Follow
}

/// Redirect-Policy aller Provider-Clients (siehe [`redirect_decision`]).
fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let decision = redirect_decision(attempt.url(), attempt.previous());
        match decision {
            RedirectDecision::Follow => attempt.follow(),
            RedirectDecision::RejectCrossOrigin => attempt.error(REDIRECT_CROSS_ORIGIN_REASON),
            RedirectDecision::RejectLimit => attempt.error(REDIRECT_LIMIT_REASON),
        }
    })
}

/// Baut den HTTP-Client eines Providers mit [`redirect_policy`].
///
/// # Errors
/// [`HttpProviderError::ClientBuild`], wenn `reqwest::ClientBuilder::build`
/// scheitert (z. B. TLS-Backend nicht initialisierbar). Kein `expect` mehr
/// (Bible R087/R165): Aufrufer entscheiden selbst, wie sie einen
/// Client-Aufbaufehler behandeln.
pub(crate) fn http_client() -> HttpProviderResult<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(redirect_policy())
        .build()
        .map_err(HttpProviderError::ClientBuild)
}

/// Baut einen als sensitiv markierten Header-Wert für ein Credential.
///
/// Sensitive Werte rendert `HeaderValue`s `Debug` als `Sensitive`; HTTP/2-
/// HPACK indiziert sie nicht. Der Fehler enthält den Wert nie.
pub(crate) fn sensitive_header_value(
    secret: &str,
) -> Result<reqwest::header::HeaderValue, ModelError> {
    let mut value = reqwest::header::HeaderValue::from_str(secret)
        .map_err(|_| ModelError::RequestFailed(INVALID_CREDENTIAL_HEADER_REASON.to_owned()))?;
    value.set_sensitive(true);
    Ok(value)
}

/// Wahl des Wire-Transports für einen OpenAI-kompatiblen Provider.
///
/// # Description
/// Bestimmt, welchen Endpoint und welches Body-Schema [`OpenAiResponsesProvider`]
/// in `respond` verwendet. `Responses` behält den bestehenden
/// `POST {base_url}/responses`-Pfad; `Chat` verwendet das klassische
/// `POST {base_url}/chat/completions` mit `messages`-Schema und
/// `choices[0].message.content`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// OpenAI Responses-API (`/responses`).
    Responses,
    /// OpenAI Chat-Completions-API (`/chat/completions`).
    Chat,
}

/// Sentinel-Permit-Zahl, die [`DynamicConcurrencyLimiter`] für „unbegrenzt"
/// verwendet — der von `tokio::sync::Semaphore` selbst erzwungene
/// Höchstwert (`usize::MAX >> 3`). `Option<usize>::None` wird intern immer
/// auf diesen Wert abgebildet, damit Wachstums-/Schrumpf-Vergleiche in
/// [`DynamicConcurrencyLimiter::set_target`] und
/// [`DynamicConcurrencyLimiterState::release`] ohne `Option`-Sonderfälle als
/// reiner `usize`-Vergleich auskommen.
const UNLIMITED_PERMITS: usize = tokio::sync::Semaphore::MAX_PERMITS;

/// Geteilter, interner Zustand von [`DynamicConcurrencyLimiter`].
///
/// Getrennt vom öffentlichen Typ und selbst hinter einem `Arc`, damit
/// [`ConcurrencyPermit`] ihn beim Erwerb per günstigem `Arc::clone`
/// referenzieren kann, ohne dass [`DynamicConcurrencyLimiter::acquire`]
/// einen `self: Arc<Self>`-Empfänger bräuchte (das läge außerhalb der auf
/// stable Rust unterstützten Empfänger-Typen) — `DynamicConcurrencyLimiter`
/// bleibt dadurch ein gewöhnlicher `&self`-Typ, den Aufrufer optional selbst
/// in ein `Arc` packen (wie es [`OpenAiResponsesProvider::concurrency_limiter`]
/// tut).
struct DynamicConcurrencyLimiterState {
    semaphore: std::sync::Arc<tokio::sync::Semaphore>,
    /// Permits, die aktuell insgesamt im Semaphore stecken (frei + im
    /// Umlauf). Wächst sofort in [`DynamicConcurrencyLimiter::set_target`].
    /// Schrumpft zweistufig: sofort für aktuell freie Permits (ebenfalls in
    /// [`DynamicConcurrencyLimiter::set_target`], per
    /// `Semaphore::forget_permits`), der Rest (Permits, die gerade in
    /// Benutzung waren) erst lazy bei Rückgabe in [`Self::release`].
    total_permits: AtomicUsize,
    /// Gewünschte Permit-Zahl. Kann während eines laufenden Schrumpfens
    /// kleiner sein als `total_permits`.
    target: AtomicUsize,
}

impl DynamicConcurrencyLimiterState {
    // Wird von `ConcurrencyPermit::drop` aufgerufen, wenn ein Permit
    // zurückgegeben wird. Schrumpft lazy in Richtung `target`, indem das
    // Permit statt zurückgegeben verworfen wird (`forget`), solange
    // `total_permits > target` gilt — nie mehr als ein Permit pro Aufruf,
    // laufende (bereits erworbene) Permits sind davon nie betroffen.
    fn release(&self, permit: tokio::sync::OwnedSemaphorePermit) {
        loop {
            let target = self.target.load(Ordering::SeqCst);
            let current_total = self.total_permits.load(Ordering::SeqCst);
            if current_total <= target {
                drop(permit);
                return;
            }
            if self
                .total_permits
                .compare_exchange(
                    current_total,
                    current_total - 1,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
            {
                permit.forget();
                return;
            }
            // Eine gleichzeitige Rückgabe/`set_target` hat `total_permits`
            // inzwischen verändert; mit dem neuen Stand erneut versuchen.
        }
    }
}

/// Elastischer Nebenläufigkeits-Limiter für [`OpenAiResponsesProvider`]
/// (siehe `provider.max_concurrency`, [`harw_config::ProviderToml::max_concurrency`]).
///
/// # Description
/// Kapselt ein `tokio::sync::Semaphore`, dessen effektive Permit-Zahl sich
/// zur Laufzeit ändern lässt, ohne laufende Requests jemals abzubrechen:
///
/// - **Vergrößern** ([`Self::set_target`] auf einen höheren Wert) wirkt
///   sofort: die zusätzlichen Permits werden direkt via
///   `Semaphore::add_permits` freigegeben, ein wartender Aufrufer bekommt
///   sein Permit ohne auf laufende Requests zu warten.
/// - **Verkleinern** wirkt zweistufig: `target` wird sofort auf den neuen,
///   kleineren Wert gesetzt, und [`Self::set_target`] zieht im selben
///   Aufruf sofort so viele aktuell *freie* Permits wie möglich direkt aus
///   dem Semaphore ab (`Semaphore::forget_permits`) — bei einem
///   unbegrenzten oder gerade wenig ausgelasteten Limiter greift die neue
///   Grenze dadurch sofort, nicht erst beim nächsten `acquire`/`release`.
///   Nur der Teil, der gerade tatsächlich in Benutzung ist, kann nicht
///   sofort entzogen werden und schrumpft weiterhin lazy: sobald so ein
///   Permit zurückgegeben wird, ruft [`ConcurrencyPermit::drop`]
///   [`DynamicConcurrencyLimiterState::release`] auf, das prüft, ob
///   `total_permits > target` gilt, und in diesem Fall das zurückgegebene
///   Permit per `OwnedSemaphorePermit::forget` verwirft (statt es ans
///   Semaphore zurückzugeben) — ein Permit weniger pro Rückgabe, bis
///   `total_permits == target` erreicht ist. Laufende Requests behalten ihr
///   eigenes Permit bis zum Ende und werden nie unterbrochen.
///
/// `None`/unbegrenzt wird als [`UNLIMITED_PERMITS`] (der von tokio selbst
/// erzwungene Höchstwert) modelliert statt als echter `enum`-Sonderfall —
/// das hält Wachstums-/Schrumpf-Vergleiche einheitlich (immer ein simpler
/// `usize`-Vergleich) und vermeidet doppelte Verzweigungen in
/// `acquire`/`set_target`/`release`.
///
/// # Concurrency
/// `Send + Sync`. `acquire` ist die einzige `await`-Stelle; `set_target` und
/// die Permit-Rückgabe (`release`) sind beide lock-frei über
/// `AtomicUsize`-CAS-Schleifen implementiert, kein `Mutex` auf dem Hot-Path.
/// Mehrere gleichzeitige `set_target`-Aufrufe sind sicher (CAS verhindert
/// doppeltes Zählen); mehrere gleichzeitige Permit-Rückgaben während eines
/// Schrumpfens sind ebenfalls sicher (jede Rückgabe schrumpft höchstens um
/// genau ein Permit).
pub struct DynamicConcurrencyLimiter {
    state: std::sync::Arc<DynamicConcurrencyLimiterState>,
}

impl std::fmt::Debug for DynamicConcurrencyLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DynamicConcurrencyLimiter")
            .field("target", &self.target())
            .field("available", &self.available())
            .finish()
    }
}

impl DynamicConcurrencyLimiter {
    /// Baut einen neuen Limiter.
    ///
    /// # Arguments
    /// - `initial` (`Option<usize>`): Anfangs-Ziel (siehe
    ///   [`harw_config::ProviderToml::max_concurrency`]); `None` heißt
    ///   praktisch unbegrenzt (siehe [`UNLIMITED_PERMITS`]).
    ///
    /// # Returns
    /// Einen Limiter mit `total_permits == target == initial` (bzw.
    /// [`UNLIMITED_PERMITS`] für `None`).
    #[must_use]
    pub fn new(initial: Option<usize>) -> Self {
        let permits = initial.unwrap_or(UNLIMITED_PERMITS).min(UNLIMITED_PERMITS);
        Self {
            state: std::sync::Arc::new(DynamicConcurrencyLimiterState {
                semaphore: std::sync::Arc::new(tokio::sync::Semaphore::new(permits)),
                total_permits: AtomicUsize::new(permits),
                target: AtomicUsize::new(permits),
            }),
        }
    }

    /// Erwirbt ein Permit; wartet, bis eines frei wird.
    ///
    /// # Returns
    /// Ein [`ConcurrencyPermit`]-Guard, der das Permit hält, bis er gedroppt
    /// wird (siehe [`DynamicConcurrencyLimiterState::release`] für das
    /// Verhalten beim Drop während eines laufenden Schrumpfens).
    ///
    /// # Errors
    /// - [`tokio::sync::AcquireError`]: das interne Semaphore wurde
    ///   geschlossen. `DynamicConcurrencyLimiter` schließt es selbst nie —
    ///   tritt praktisch nicht auf.
    ///
    /// # Concurrency
    /// Sicher von mehreren Tasks gleichzeitig aufrufbar; wartet kooperativ
    /// (kein Busy-Loop) über `tokio::sync::Semaphore`.
    pub async fn acquire(&self) -> Result<ConcurrencyPermit, tokio::sync::AcquireError> {
        let permit = std::sync::Arc::clone(&self.state.semaphore)
            .acquire_owned()
            .await?;
        Ok(ConcurrencyPermit {
            permit: Some(permit),
            state: std::sync::Arc::clone(&self.state),
        })
    }

    /// Setzt das Ziel neu. Wachsen wirkt sofort, Schrumpfen lazy (siehe
    /// Typ-Dokumentation).
    ///
    /// # Arguments
    /// - `new_target` (`Option<usize>`): neues Ziel; `None` heißt unbegrenzt.
    ///
    /// # Concurrency
    /// Lock-frei (CAS-Schleife über `total_permits`); sicher, wenn mehrere
    /// Aufrufer gleichzeitig `set_target` aufrufen.
    pub fn set_target(&self, new_target: Option<usize>) {
        let new_target = new_target
            .unwrap_or(UNLIMITED_PERMITS)
            .min(UNLIMITED_PERMITS);
        self.state.target.store(new_target, Ordering::SeqCst);
        loop {
            let current_total = self.state.total_permits.load(Ordering::SeqCst);
            if new_target == current_total {
                // Bereits auf Ziel — weder wachsen noch sofort schrumpfen
                // nötig.
                break;
            }
            if new_target > current_total {
                // Wachsen: sofort per `add_permits`, siehe Typ-Doku. CAS
                // gegen `total_permits`, damit ein gleichzeitiger
                // `set_target`/`release`-Aufruf nicht überschrieben wird.
                let delta = new_target - current_total;
                if self
                    .state
                    .total_permits
                    .compare_exchange(
                        current_total,
                        new_target,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    )
                    .is_ok()
                {
                    self.state.semaphore.add_permits(delta);
                    break;
                }
                // Ein gleichzeitiger Aufruf hat `total_permits` inzwischen
                // verändert; mit dem neuen Stand erneut versuchen.
            } else {
                // Schrumpfen, Stufe 1 (sofort): so viele FREIE Permits wie
                // möglich direkt aus dem Semaphore einziehen
                // (`Semaphore::forget_permits`, tokio 1.53 — entfernt bis zu
                // `excess` aktuell verfügbare Permits sofort und liefert die
                // tatsächlich entfernte Anzahl `forgotten <= excess`, ohne
                // auf laufende/erworbene Permits zu warten oder sie
                // anzutasten). Der Rest, falls `forgotten < excess` weil
                // gerade zu viele Permits in Benutzung waren, wird lazy bei
                // Rückgabe verworfen (siehe
                // [`DynamicConcurrencyLimiterState::release`]).
                //
                // Invariante: `total_permits` wird hier ausschließlich per
                // `fetch_sub(forgotten)` nachgeführt (kein CAS gegen den
                // zuvor gelesenen `current_total`), weil `forget_permits`
                // bereits unwiderruflich und atomar gegen den tatsächlichen
                // freien Bestand des Semaphores wirkt — die zurückgegebene
                // Anzahl ist unabhängig davon korrekt, ob `total_permits`
                // zwischenzeitlich durch ein gleichzeitiges `set_target`
                // (Wachsen) oder `release` (lazy Schrumpfen) verändert
                // wurde. Ein `fetch_sub` ist dafür ausreichend und race-frei,
                // da es die tatsächlich vergessenen Permits abzieht, egal
                // welchen Wert `total_permits` gerade hat; `total_permits`
                // fällt dadurch nie unter `target`, weil `forgotten` niemals
                // mehr als `excess = current_total - new_target` sein kann.
                let excess = current_total - new_target;
                let forgotten = self.state.semaphore.forget_permits(excess);
                if forgotten > 0 {
                    self.state
                        .total_permits
                        .fetch_sub(forgotten, Ordering::SeqCst);
                }
                break;
            }
        }
    }

    /// Aktuelles Ziel, oder `None` für unbegrenzt.
    #[must_use]
    pub fn target(&self) -> Option<usize> {
        let target = self.state.target.load(Ordering::SeqCst);
        (target != UNLIMITED_PERMITS).then_some(target)
    }

    /// Aktuell verfügbare (nicht im Umlauf befindliche) Permits.
    ///
    /// # Returns
    /// Für einen unbegrenzten Limiter ein sehr großer Wert (nahe
    /// [`UNLIMITED_PERMITS`]) statt `usize::MAX` — kein Sonderfall nötig,
    /// da praktisch nie erreicht.
    #[must_use]
    pub fn available(&self) -> usize {
        self.state.semaphore.available_permits()
    }
}

/// RAII-Guard für ein von [`DynamicConcurrencyLimiter::acquire`] erworbenes
/// Permit.
///
/// # Description
/// Hält intern ein `tokio::sync::OwnedSemaphorePermit`. Beim Drop
/// entscheidet [`DynamicConcurrencyLimiterState::release`], ob das Permit
/// normal ans Semaphore zurückgegeben wird, oder — falls der Limiter
/// inzwischen lazy schrumpft und noch mehr Permits im Umlauf sind als das
/// aktuelle Ziel erlaubt — verworfen wird, um die Kapazität dauerhaft (bis
/// zum nächsten Wachsen) zu reduzieren.
pub struct ConcurrencyPermit {
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
    state: std::sync::Arc<DynamicConcurrencyLimiterState>,
}

impl Drop for ConcurrencyPermit {
    fn drop(&mut self) {
        if let Some(permit) = self.permit.take() {
            self.state.release(permit);
        }
    }
}

/// Provider-neutraler Zustandsbericht über Nebenläufigkeit und Rate-Limit-Pacing
/// eines HTTP-Providers (W6b — UIA-Sichtbarkeit auf Provider-Concurrency/
/// Rate-Limit-Zustand).
///
/// # Description
/// Aggregiert die zwei unabhängigen Laufzeit-Schutzmechanismen eines Providers
/// in einer einzigen, textformatierbaren Momentaufnahme: das harte,
/// client-seitige Nebenläufigkeits-Limit ([`DynamicConcurrencyLimiter`]) und
/// das reaktive Header-Pacing ([`rate_limiter::ProviderRateLimiter`]). Gedacht
/// für `/status`/`/provider show` (Mensch) und als `ModelResponse`-Datenquelle
/// für ein `provider-concurrency`-Modell-Tool (UIA) — siehe
/// [`ProviderLoadControl`].
#[derive(Debug, Clone)]
pub struct ProviderLoadStatus {
    /// Kanonischer Provider-Name (Schlüssel in `harw_config::ResolvedConfig::providers`).
    pub provider: String,
    /// Aktuell konfiguriertes Nebenläufigkeits-Ziel, oder `None` für unbegrenzt
    /// (siehe [`DynamicConcurrencyLimiter::target`]).
    pub max_concurrency: Option<usize>,
    /// Aktuell freie (nicht im Umlauf befindliche) Permits (siehe
    /// [`DynamicConcurrencyLimiter::available`]). `usize::MAX`, wenn dieser
    /// Provider ohne installierten Limiter gebaut wurde (siehe
    /// [`OpenAiResponsesProvider::concurrency_limiter`]-Doku).
    pub available_permits: usize,
    /// Aktuell fällige Pacing-Wartezeit aus beobachteten Rate-Limit-Headern
    /// (siehe [`rate_limiter::ProviderRateLimiter::pending_wait`]); `None`,
    /// wenn kein Kontingent knapp ist oder der Pacer deaktiviert ist.
    pub rate_limit_wait: Option<Duration>,
    /// Gesamtzahl seit Provider-Konstruktion beobachteter HTTP-429-Antworten
    /// (siehe [`rate_limiter::ProviderRateLimiter::rate_limited_count`]).
    /// Wiederholte 429 sind das primäre Signal, die Concurrency für diesen
    /// Provider zu **senken** — nicht zu erhöhen.
    pub recent_rate_limited: u64,
}

/// Provider-neutrale Steuer- und Beobachtungsfläche für Nebenläufigkeit und
/// Rate-Limit-Zustand — die Schnittstelle, über die ein Ops-Layer (z. B.
/// `harw-ops`) einen konkreten HTTP-Provider beobachten und live anpassen
/// kann, ohne dessen konkreten Rust-Typ zu kennen.
///
/// # Description
/// `harw-ops` sieht Provider normalerweise nur als `Arc<dyn
/// harw_core::ModelProvider>` — ein reines Anfrage-Interface ohne
/// Introspektion. `ProviderLoadControl` ist der separate, additive Kanal
/// dafür: ein Registrierungs-Layer (Composition Root) legt für jeden
/// HTTP-Provider, der diese Fähigkeit unterstützt, einen
/// `Arc<dyn ProviderLoadControl>` in die `ServiceMap` (typischerweise unter
/// dem Typ [`ProviderLoadRegistry`]).
///
/// # Concurrency
/// Implementierungen müssen `Send + Sync` sein und dürfen intern beliebig oft
/// gleichzeitig aufgerufen werden (siehe [`OpenAiResponsesProvider`] und
/// [`ProviderLoadHandle`] — beide delegieren an lock-freie `Arc`-Zustände).
pub trait ProviderLoadControl: Send + Sync {
    /// Liefert eine Momentaufnahme des aktuellen Nebenläufigkeits-/
    /// Rate-Limit-Zustands.
    #[must_use]
    fn provider_status(&self) -> ProviderLoadStatus;

    /// Setzt das Nebenläufigkeits-Ziel neu (siehe
    /// [`DynamicConcurrencyLimiter::set_target`]: Wachsen wirkt sofort,
    /// Schrumpfen lazy, kein laufender Request wird abgebrochen).
    ///
    /// # Arguments
    /// - `target` (`Option<usize>`): neues Ziel; `None` heißt unbegrenzt.
    ///
    /// # Returns
    /// `true`, wenn ein Limiter installiert ist und die Änderung angewendet
    /// wurde; `false`, wenn dieser Provider ohne
    /// [`DynamicConcurrencyLimiter`] gebaut wurde (praktisch nur bei
    /// [`OpenAiResponsesProvider::new`]/[`OpenAiResponsesProvider::with_transport`]
    /// statt über die Konfiguration) — dann bleibt die Anfrage wirkungslos.
    fn set_max_concurrency(&self, target: Option<usize>) -> bool;
}

/// Registrierte [`ProviderLoadControl`]-Handles je Provider-Name.
///
/// # Description
/// Schlüssel ist derselbe kanonische Provider-Name, unter dem
/// `harw_config::ResolvedConfig::providers` und die interne
/// `RoutingModelProvider`-Provider-Map (siehe [`build_provider`]) den
/// Provider führen. Sowohl OpenAI-kompatible Backends
/// ([`OpenAiResponsesProvider`]) als auch der native Anthropic-Backend
/// ([`AnthropicMessagesProvider`]) füllen diese Registry (siehe
/// `build_named_provider`-Kommentar).
///
/// # Concurrency
/// `Send + Sync` (jeder Wert ist ein `Arc<dyn ProviderLoadControl>`, dessen
/// Trait `Send + Sync` als Supertrait fordert).
pub type ProviderLoadRegistry = BTreeMap<String, std::sync::Arc<dyn ProviderLoadControl>>;

/// Baut eine [`ProviderLoadStatus`] aus den drei Rohgrößen, die sowohl
/// [`OpenAiResponsesProvider`] als auch [`ProviderLoadHandle`] halten — hält
/// beide Implementierungen von [`ProviderLoadControl::provider_status`]
/// deckungsgleich, ohne Code zu duplizieren.
///
/// `pub(crate)`, weil [`crate::anthropic::AnthropicMessagesProvider`] sie
/// ebenfalls für [`crate::anthropic::AnthropicMessagesProvider::load_status`]
/// nutzt (siehe dort).
pub(crate) fn provider_load_status(
    provider_id: &str,
    concurrency_limiter: Option<&DynamicConcurrencyLimiter>,
    rate_limiter: &rate_limiter::ProviderRateLimiter,
) -> ProviderLoadStatus {
    ProviderLoadStatus {
        provider: provider_id.to_owned(),
        max_concurrency: concurrency_limiter.and_then(DynamicConcurrencyLimiter::target),
        available_permits: concurrency_limiter
            .map(DynamicConcurrencyLimiter::available)
            .unwrap_or(usize::MAX),
        rate_limit_wait: rate_limiter.pending_wait(),
        recent_rate_limited: rate_limiter.rate_limited_count(),
    }
}

/// Eigenständiger [`ProviderLoadControl`]-Handle, der nur die beiden bereits
/// unabhängig `Arc`-gehaltenen Zustände eines Providers referenziert
/// (`concurrency_limiter`, `rate_limiter`), statt den ganzen Provider zu
/// besitzen.
///
/// # Description
/// Nötig, weil [`build_named_provider`] den konkreten
/// [`OpenAiResponsesProvider`] am Ende in einen [`retry::RetryingProvider`]
/// verschiebt (`RetryingProvider::new(inner: P, ..)` nimmt `P` per Wert
/// entgegen) und ihn danach als `Box<dyn harw_core::ModelProvider>`
/// zurückgibt — der konkrete Typ ist für den Aufrufer damit nicht mehr
/// erreichbar. Beide vom Provider gehaltenen `Arc`s
/// ([`OpenAiResponsesProvider::concurrency_limiter`],
/// [`OpenAiResponsesProvider::rate_limiter_handle`]) werden deshalb **vor**
/// dieser Verschiebung geklont und hier separat gehalten.
///
/// # Concurrency
/// `Send + Sync` (beide Felder sind `Arc`s über bereits `Send + Sync`
/// gestaltete, lock-freie bzw. kurzzeitig gesperrte Zustände).
#[derive(Debug, Clone)]
struct ProviderLoadHandle {
    provider_id: String,
    concurrency_limiter: Option<std::sync::Arc<DynamicConcurrencyLimiter>>,
    rate_limiter: std::sync::Arc<rate_limiter::ProviderRateLimiter>,
}

impl ProviderLoadControl for ProviderLoadHandle {
    fn provider_status(&self) -> ProviderLoadStatus {
        provider_load_status(
            &self.provider_id,
            self.concurrency_limiter.as_deref(),
            &self.rate_limiter,
        )
    }

    fn set_max_concurrency(&self, target: Option<usize>) -> bool {
        match &self.concurrency_limiter {
            Some(limiter) => {
                limiter.set_target(target);
                true
            }
            None => false,
        }
    }
}

/// HTTP-Provider gegen die OpenAI-kompatible Responses-API.
///
/// # Description
/// Hält den geteilten `reqwest::Client`, die Basis-URL, den Modellnamen und
/// den geheimen API-Key. Implementiert [`harw_core::ModelProvider`], sodass der
/// Core-Turn-Loop echte Modell-Aufrufe absetzen kann.
///
/// # Concurrency
/// `Send + Sync`; kann hinter einem `Arc` von mehreren Threads genutzt werden.
pub struct OpenAiResponsesProvider {
    client: reqwest::Client,
    base_url: String,
    provider_id: String,
    model: String,
    api_key: SecretString,
    codex_route: Option<codex::CodexRoute>,
    auth_header: String,
    headers: reqwest::header::HeaderMap,
    transport: Transport,
    request_timeout: Duration,
    reasoning_replay: ReasoningReplay,
    /// Modell-ID → Prompt-Caching-Override (`ModelToml::prompt_caching`),
    /// befüllt aus `config.models` beim Bau über [`Self::from_named_config`].
    /// Leer, wenn der Provider über [`Self::new`]/[`Self::with_transport`]
    /// gebaut wurde — dann entscheidet [`cache_strategy::resolve_cache_strategy`]
    /// allein anhand von Provider-Name/Modell.
    cache_overrides: std::collections::HashMap<String, harw_config::PromptCachingMode>,
    /// Pro-Modell-Entscheidung für SSE-Streaming (siehe [`sse::StreamPolicy`]).
    stream_policy: sse::StreamPolicy,
    /// Client-seitiger Rate-Limiter (siehe [`rate_limiter::ProviderRateLimiter`]);
    /// standardmäßig deaktiviert (`ProviderRateLimiter::new(None)`).
    rate_limiter: std::sync::Arc<rate_limiter::ProviderRateLimiter>,
    /// Harte, zur Laufzeit elastisch verstellbare Nebenläufigkeitsgrenze für
    /// diesen Provider (siehe [`harw_config::ProviderToml::max_concurrency`]
    /// und [`DynamicConcurrencyLimiter`]); `None` heißt: kein Limiter
    /// installiert (nur bei [`Self::new`]/[`Self::with_transport`] — nie bei
    /// [`Self::from_named_config`], das immer einen Limiter baut, auch für
    /// `max_concurrency: None`, damit ein späterer Ops-Layer per
    /// [`DynamicConcurrencyLimiter::set_target`] auch ursprünglich
    /// unbegrenzte Provider nachträglich deckeln kann).
    ///
    /// Anders als [`Self::rate_limiter`] (reaktives Header-Pacing, wirkt auf
    /// den *nächsten* Request) blockiert dies zusätzliche Requests rein
    /// client-seitig, bevor sie überhaupt gesendet werden, sobald bereits
    /// `max_concurrency` Requests dieses Providers gleichzeitig in Flug
    /// sind. Gedacht für Backends mit begrenzter Parallelitätskapazität
    /// (z. B. einen Cloudflare Worker vor Workers AI), die bei zu vielen
    /// gleichzeitigen Chat-Turn-Requests (etwa durch Tool-Use-Fanout einer
    /// einzigen User-Runde) ins Stocken geraten.
    concurrency_limiter: Option<std::sync::Arc<DynamicConcurrencyLimiter>>,
    /// `auth.credential_pool[provider_id]`, falls nicht-leer konfiguriert
    /// (siehe Modul-Doku „Credential-Pool" und [`credential_pool`]).
    /// `None` heißt: dieser Provider nutzt ausschließlich `api_key`/`base_url`
    /// oben (unverändertes Verhalten ohne Pool).
    credential_pool: Option<std::sync::Arc<credential_pool::CredentialPool<SecretString>>>,
    /// Opt-in aus [`harw_config::ProviderToml::gateway_identity_headers`]
    /// (Standard `false`): sendet, wenn `true` **und** der aktuelle
    /// [`ModelRequest::identity`] gesetzt ist, zusätzlich zu den statischen
    /// `[headers]` die Header `x-harw-session`/`x-harw-agent`/`x-harw-role`
    /// an eigene Cloudflare-Worker/AI-Gateway-Endpunkte (siehe
    /// [`Self::authorized_request`], [`identity_headers`]). Andere Provider
    /// bleiben unberührt, solange dieses Feld `false` ist.
    gateway_identity_headers: bool,
}

/// Eine gemerkte Reasoning-Runde der Responses-API (W4a / A-OAI).
///
/// `blocks` sind die unveränderten `reasoning`-Output-Items mit
/// `encrypted_content`; `call_ids` die `function_call`s derselben Antwort.
#[derive(Debug, Clone, PartialEq)]
struct ReplayEntry {
    call_ids: Vec<String>,
    provider: String,
    model: String,
    blocks: Vec<Value>,
}

/// Provider-lokaler, begrenzter Speicher für zurückzuspielende Reasoning-Items.
///
/// # Description
/// `ConversationHistory` hat (Stand W3) keinen Träger für opake
/// Reasoning-Blöcke; bis A-LOOP `ModelResponse::reasoning` persistiert, merkt
/// sich der Provider die Blöcke je Tool-Call-Runde und spielt sie zurück,
/// sobald der Verlauf die zugehörigen `function_call`s enthält. Nur Einträge
/// desselben Providers **und** Modells werden verwendet.
///
/// # Concurrency
/// `Mutex` nur für kurze Kopier-/Einfügeoperationen, nie über `await` gehalten;
/// Poisoning wird toleriert (reiner Cache).
#[derive(Debug, Default)]
struct ReasoningReplay {
    entries: Mutex<VecDeque<ReplayEntry>>,
}

impl ReasoningReplay {
    // Merkt eine Runde; älteste Einträge fallen bei Überlauf heraus.
    fn remember(&self, entry: ReplayEntry) {
        if entry.call_ids.is_empty() || entry.blocks.is_empty() {
            return;
        }
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        while entries.len() >= MAX_REASONING_REPLAY_ENTRIES {
            entries.pop_front();
        }
        entries.push_back(entry);
    }

    // Liefert die Runden, deren Tool-Calls im Verlauf von `request` stehen.
    fn for_request(&self, provider: &str, model: &str, request: &ModelRequest) -> Vec<ReplayEntry> {
        let call_ids: BTreeSet<&str> = request
            .history
            .items()
            .iter()
            .filter_map(|item| match item {
                TurnItem::ToolCall(call) => Some(call.call_id.as_str()),
                _ => None,
            })
            .collect();
        if call_ids.is_empty() {
            return Vec::new();
        }
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries
            .iter()
            .filter(|entry| {
                entry.provider == provider
                    && entry.model == model
                    && entry
                        .call_ids
                        .iter()
                        .any(|id| call_ids.contains(id.as_str()))
            })
            .cloned()
            .collect()
    }
}

impl OpenAiResponsesProvider {
    /// Baut einen Provider aus expliziten Werten.
    ///
    /// # Arguments
    /// - `base_url` (`impl Into<String>`): Basis-URL ohne Endpoint-Suffix.
    /// - `model` (`impl Into<String>`): Modellname für das `model`-Feld.
    /// - `api_key` (`SecretString`): Bearer-Token, wird nie geloggt.
    ///
    /// # Returns
    /// Einen einsatzbereiten [`OpenAiResponsesProvider`].
    ///
    /// # Errors
    /// [`HttpProviderError::ClientBuild`] aus [`http_client`], wenn der
    /// geteilte `reqwest::Client` nicht gebaut werden kann.
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: SecretString,
    ) -> HttpProviderResult<Self> {
        Self::with_transport(base_url, model, api_key, Transport::Responses)
    }

    /// Baut einen Provider mit explizitem Transport.
    ///
    /// # Arguments
    /// - `base_url` (`impl Into<String>`): Basis-URL ohne Endpoint-Suffix.
    /// - `model` (`impl Into<String>`): Modellname für das `model`-Feld.
    /// - `api_key` (`SecretString`): Bearer-Token, wird nie geloggt.
    /// - `transport` ([`Transport`]): wählt Responses- oder Chat-Wire-Format.
    ///
    /// # Returns
    /// Einen einsatzbereiten [`OpenAiResponsesProvider`] für den gewählten
    /// Transport.
    ///
    /// # Errors
    /// [`HttpProviderError::ClientBuild`] aus [`http_client`], wenn der
    /// geteilte `reqwest::Client` nicht gebaut werden kann.
    pub fn with_transport(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: SecretString,
        transport: Transport,
    ) -> HttpProviderResult<Self> {
        Ok(Self {
            client: http_client()?,
            base_url: base_url.into(),
            provider_id: "openai".to_owned(),
            model: model.into(),
            api_key,
            codex_route: None,
            auth_header: "bearer".into(),
            headers: reqwest::header::HeaderMap::new(),
            transport,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            reasoning_replay: ReasoningReplay::default(),
            cache_overrides: std::collections::HashMap::new(),
            stream_policy: sse::StreamPolicy::default(),
            rate_limiter: std::sync::Arc::new(rate_limiter::ProviderRateLimiter::new(None)),
            concurrency_limiter: None,
            credential_pool: None,
            gateway_identity_headers: false,
        })
    }

    /// Baut einen Provider aus einer aufgelösten Konfiguration.
    ///
    /// # Description
    /// Liest `default_provider`/`default_model`, sucht den passenden
    /// [`harw_config::ProviderToml`] und löst dessen `auth`-`SecretRef` auf
    /// (`env:` → Umgebungsvariable, `file:` → getrimmter Dateiinhalt,
    /// `keyring:` → System-Keyring).
    ///
    /// # Errors
    /// - [`HttpProviderError::MissingDefault`]: wenn Provider, Modell, der
    ///   Provider-Eintrag selbst oder dessen `auth`-Feld fehlt.
    /// - [`HttpProviderError::UnresolvedCredential`]: wenn die `SecretRef`
    ///   nicht aufgelöst werden kann.
    /// - [`HttpProviderError::UnsupportedCredentialReference`]: wenn `secrets:`
    ///   ohne injizierten Resolver verwendet wird.
    /// - [`HttpProviderError::ClientBuild`]: aus [`Self::with_transport`], wenn
    ///   der geteilte `reqwest::Client` nicht gebaut werden kann.
    pub fn from_config(config: &harw_config::ResolvedConfig) -> HttpProviderResult<Self> {
        Self::from_config_with_optional_resolver(config, None)
    }

    /// Builds the configured OpenAI-compatible provider with an injected
    /// synchronous `secrets:` resolver.
    pub fn from_config_with_resolver(
        config: &harw_config::ResolvedConfig,
        resolver: &dyn SecretResolver,
    ) -> HttpProviderResult<Self> {
        Self::from_config_with_optional_resolver(config, Some(resolver))
    }

    fn from_config_with_optional_resolver(
        config: &harw_config::ResolvedConfig,
        resolver: Option<&dyn SecretResolver>,
    ) -> HttpProviderResult<Self> {
        let provider_name = config.harness.default_provider.as_deref().ok_or_else(|| {
            HttpProviderError::MissingDefault {
                what: "default_provider".to_owned(),
            }
        })?;
        let model = config.harness.default_model.as_deref().ok_or_else(|| {
            HttpProviderError::MissingDefault {
                what: "default_model".to_owned(),
            }
        })?;
        let provider = config.providers.get(provider_name).ok_or_else(|| {
            HttpProviderError::MissingDefault {
                what: format!("provider entry '{provider_name}'"),
            }
        })?;
        let sources = SecretSources {
            env_layer: &config.env_layer,
            resolver,
            home: None,
            endpoint: Some(&provider.base_url),
        };
        Self::from_named_config(provider_name, provider, config, model, sources)
    }

    /// Builds one OpenAI-compatible provider from its named configuration.
    ///
    /// `config` liefert `config.models` zum Befüllen von `cache_overrides`
    /// (jedes Modell dieses Providers mit gesetztem `prompt_caching`, unter
    /// seiner `id` **und** all seinen `aliases`) sowie `provider.rate_limit`
    /// zum Bau des [`rate_limiter::ProviderRateLimiter`]. `provider.max_concurrency`
    /// (siehe [`harw_config::ProviderToml::max_concurrency`]) baut
    /// [`Self::concurrency_limiter`] immer als frischen
    /// `Arc<`[`DynamicConcurrencyLimiter`]`>` über
    /// `DynamicConcurrencyLimiter::new(provider.max_concurrency)` — auch für
    /// `None` (praktisch unbegrenzt), damit ein späterer Ops-Layer über
    /// [`DynamicConcurrencyLimiter::set_target`] jederzeit eine Grenze
    /// setzen kann. `provider.auth` bleibt immer das
    /// primäre Credential; ist `config.auth.credential_pool` für
    /// `provider_name` nicht-leer, liefert er nur Failover-Kandidaten
    /// dahinter (siehe Modul-Doku „Credential-Pool"); Codex-Routen bleiben
    /// davon unberührt.
    fn from_named_config(
        provider_name: &str,
        provider: &harw_config::ProviderToml,
        config: &harw_config::ResolvedConfig,
        model: &str,
        sources: SecretSources<'_>,
    ) -> HttpProviderResult<Self> {
        let codex_route = codex::CodexRoute::from_provider(provider)?;
        let base_url = if codex_route.is_some() {
            codex::BASE_URL
        } else {
            &provider.base_url
        };
        let sources = SecretSources {
            endpoint: Some(base_url),
            ..sources
        };
        if provider.base_url.trim().is_empty() {
            return Err(HttpProviderError::Decode(format!(
                "provider '{provider_name}' has an empty base_url"
            )));
        }
        // Auch der öffentliche `from_config`-Pfad erzwingt https (http nur Loopback).
        validate_endpoint(&provider.base_url)?;
        let auth_header = provider.auth_header.as_deref().unwrap_or_else(|| {
            if provider_name == "foundry" || provider_name.starts_with("foundry-") {
                "api-key"
            } else if matches!(provider.api.as_str(), "ollama") {
                "none"
            } else {
                "bearer"
            }
        });
        // Primäres Credential: `provider.auth`, sonst bei `auth_header ==
        // "none"` ein leeres Secret (kein Credential nötig, z. B. Ollama).
        // Ein konfigurierter Credential-Pool ersetzt dies NICHT mehr — er
        // liefert nur Failover-Kandidaten hinter dem primären Credential
        // (siehe Modul-Doku „Credential-Pool"); fehlt das primäre
        // Credential, übernimmt stattdessen der erste Pool-Eintrag diese
        // Rolle. Codex-Routen haben ihre eigene OAuth-Auflösung
        // (`route.headers()`) und nehmen nie am Pool teil.
        let primary: Option<SecretString> = if let Some(reference) = &provider.auth {
            Some(resolve_secret(reference, sources)?)
        } else if auth_header == "none" {
            Some(SecretString::new(String::new().into()))
        } else {
            None
        };
        let pool: Option<credential_pool::CredentialPool<SecretString>> = if codex_route.is_none() {
            let pool = credential_pool::CredentialPool::from_auth_config(
                &config.auth,
                provider_name,
                sources,
                Ok,
            )?;
            if let Some(pool) = &pool {
                for index in 0..pool.len() {
                    if let Some(url) = &pool.entry(index).base_url {
                        validate_endpoint(url)?;
                    }
                }
            }
            pool
        } else {
            None
        };
        let (api_key, pool) = match (pool, primary) {
            (Some(pool), Some(primary_value)) => {
                let pool = pool.prepend_primary(primary_value, format!("{provider_name}#primary"));
                tracing::debug!(
                    provider = provider_name,
                    failover_entries = pool.len() - 1,
                    "credential_pool.configured_as_failover"
                );
                let index = pool.select(None).unwrap_or(0);
                let api_key = pool.entry(index).value.clone();
                (api_key, Some(pool))
            }
            (Some(pool), None) => {
                tracing::debug!(
                    provider = provider_name,
                    "credential_pool.primary_absent_first_pool_entry_promoted"
                );
                let index = pool.select(None).unwrap_or(0);
                let api_key = pool.entry(index).value.clone();
                (api_key, Some(pool))
            }
            (None, Some(primary_value)) => (primary_value, None),
            (None, None) => {
                return Err(HttpProviderError::MissingDefault {
                    what: format!("auth for provider '{provider_name}'"),
                });
            }
        };
        let mut http_provider = Self::with_transport(
            base_url.trim_end_matches('/').to_owned(),
            model.to_owned(),
            api_key,
            transport_from_api(&provider.api),
        )?;
        http_provider.auth_header = auth_header.to_owned();
        http_provider.codex_route = codex_route;
        http_provider.credential_pool = pool.map(std::sync::Arc::new);
        if !matches!(
            provider.api.as_str(),
            "openai-chat" | "openai-responses" | "ollama"
        ) {
            return Err(HttpProviderError::Decode(format!(
                "unsupported provider API '{}'",
                provider.api
            )));
        }
        if provider.api == "ollama" {
            http_provider.base_url = format!(
                "{}/v1",
                provider
                    .base_url
                    .trim_end_matches('/')
                    .trim_end_matches("/v1")
            );
            http_provider.transport = Transport::Chat;
        }
        http_provider.provider_id = provider_name.to_owned();
        http_provider.headers = configured_headers(provider_name, &provider.headers, sources)?;
        http_provider.gateway_identity_headers = provider.gateway_identity_headers;
        for model_entry in config
            .models
            .values()
            .filter(|m| m.provider == provider_name)
        {
            if let Some(mode) = model_entry.prompt_caching {
                http_provider
                    .cache_overrides
                    .insert(model_entry.id.clone(), mode);
                for alias in &model_entry.aliases {
                    http_provider.cache_overrides.insert(alias.clone(), mode);
                }
            }
        }
        http_provider.stream_policy =
            sse::StreamPolicy::from_config(provider_name, provider, config);
        http_provider.rate_limiter = std::sync::Arc::new(rate_limiter::ProviderRateLimiter::new(
            provider.rate_limit.clone(),
        ));
        http_provider.concurrency_limiter = Some(std::sync::Arc::new(
            DynamicConcurrencyLimiter::new(provider.max_concurrency),
        ));
        Ok(http_provider)
    }

    /// Liefert einen geteilten Zugriff auf den [`DynamicConcurrencyLimiter`]
    /// dieses Providers, falls einer installiert ist.
    ///
    /// # Description
    /// Schnittstelle für einen späteren Ops-Layer (z. B. `harw-ops`), der
    /// `target()`/`available()` beobachten und `set_target(...)` aufrufen
    /// will, um die Nebenläufigkeitsgrenze zur Laufzeit zu verändern (siehe
    /// [`DynamicConcurrencyLimiter`]-Typ-Doku: Wachsen sofort, Schrumpfen
    /// lazy, kein Abbruch laufender Requests). `None` nur bei Providern, die
    /// über [`Self::new`]/[`Self::with_transport`] statt
    /// [`Self::from_named_config`] gebaut wurden.
    ///
    /// # Returns
    /// `Some(&Arc<DynamicConcurrencyLimiter>)`, geklont über `Arc::clone`
    /// für den Aufrufer, oder `None`.
    ///
    /// # Concurrency
    /// Der zurückgegebene `Arc` ist `Send + Sync` und sicher von mehreren
    /// Threads/Tasks gleichzeitig nutzbar (siehe [`DynamicConcurrencyLimiter`]).
    #[must_use]
    pub fn concurrency_limiter(&self) -> Option<std::sync::Arc<DynamicConcurrencyLimiter>> {
        self.concurrency_limiter.as_ref().map(std::sync::Arc::clone)
    }

    /// Liefert einen geteilten Zugriff auf den [`rate_limiter::ProviderRateLimiter`]
    /// dieses Providers.
    ///
    /// # Description
    /// Schnittstelle für [`ProviderLoadHandle`] (und damit für einen späteren
    /// Ops-Layer): erlaubt das Ablesen von `pending_wait()`/`rate_limited_count()`,
    /// ohne dass der Aufrufer den ganzen Provider besitzen muss — insbesondere,
    /// wenn der Provider selbst bereits in einen `RetryingProvider` verpackt und
    /// dadurch als konkreter Typ nicht mehr erreichbar ist (siehe
    /// `build_named_provider`).
    ///
    /// # Returns
    /// Ein geklonter `Arc<ProviderRateLimiter>` (immer vorhanden — anders als
    /// [`Self::concurrency_limiter`] gibt es keinen `None`-Fall, jeder Provider
    /// hat einen Rate-Limiter, ggf. nur deaktiviert).
    ///
    /// # Concurrency
    /// Der zurückgegebene `Arc` ist `Send + Sync` und sicher von mehreren
    /// Threads/Tasks gleichzeitig nutzbar.
    #[must_use]
    pub fn rate_limiter_handle(&self) -> std::sync::Arc<rate_limiter::ProviderRateLimiter> {
        std::sync::Arc::clone(&self.rate_limiter)
    }

    /// Momentaufnahme von Nebenläufigkeits-/Rate-Limit-Zustand dieses Providers.
    ///
    /// # Returns
    /// Siehe [`ProviderLoadStatus`]. Identisch zu
    /// `<Self as ProviderLoadControl>::provider_status`; als eigene Methode
    /// nutzbar, ohne den Trait zu importieren.
    #[must_use]
    pub fn load_status(&self) -> ProviderLoadStatus {
        provider_load_status(
            &self.provider_id,
            self.concurrency_limiter.as_deref(),
            &self.rate_limiter,
        )
    }

    /// Resolves the model for one request after checking its provider affinity.
    ///
    /// A request without a provider ID (or with an empty one) remains compatible
    /// with the configured provider. A non-empty provider ID must match exactly;
    /// otherwise this provider must not route the request. Likewise, an absent or
    /// empty model ID retains the configured model default.
    fn selected_model<'a>(&'a self, request: &'a ModelRequest) -> Result<&'a str, ModelError> {
        if let Some(provider_id) = request.provider_id.as_ref()
            && !provider_id.as_str().is_empty()
            && provider_id.as_str() != self.provider_id.as_str()
        {
            return Err(ModelError::RequestFailed(format!(
                "request targets provider '{}' but this HTTP provider is configured for '{}'",
                provider_id, self.provider_id
            )));
        }

        Ok(request
            .model_id
            .as_ref()
            .filter(|model_id| !model_id.as_str().is_empty())
            .map_or(self.model.as_str(), |model_id| model_id.as_str()))
    }
}

/// Reject placeholder/project/request URLs before credentials are used.
///
/// # Description
/// Geprüft über [`EgressUrl::parse`] (WHATWG-Parser wie reqwest; nur
/// `http`/`https`, keine Userinfo, Host Pflicht). Zusätzlich:
/// - `https` ist Pflicht; `http` nur für Loopback-Hosts
///   ([`EgressHost::is_loopback`]: `localhost`, `*.localhost`, 127/8, `::1`),
///   z. B. lokales Ollama oder Test-Mocks;
/// - keine `<`/`>`-Platzhalter, keine Query, kein Fragment.
///
/// # Errors
/// [`HttpProviderError::Decode`] ohne Echo der Eingabe.
pub fn validate_endpoint(value: &str) -> HttpProviderResult<()> {
    let endpoint = EgressUrl::parse(value)
        .map_err(|_| HttpProviderError::Decode("invalid provider endpoint".into()))?;
    let url = endpoint.as_url();
    if value.contains(['<', '>'])
        || url.query().is_some()
        || url.fragment().is_some()
        || !(endpoint.is_https() || endpoint.host().is_loopback())
    {
        return Err(HttpProviderError::Decode("provider endpoint must be an HTTPS base URL (HTTP only for loopback hosts) without placeholders, credentials or query parameters".into()));
    }
    Ok(())
}

/// Resolves deterministic provider headers before any request can be sent.
///
/// Header mit Credential-Namen ([`harw_config::ProviderToml::is_sensitive_header_name`])
/// müssen eine `SecretRef` sein; sie wird wie `auth` aufgelöst und der Wert als
/// sensitiv markiert. Klartext wird abgelehnt (fail closed, auch wenn
/// `ProviderToml::validate` nicht aufgerufen wurde). Übrige Header bleiben
/// literal.
fn configured_headers(
    provider_name: &str,
    headers: &std::collections::HashMap<String, String>,
    sources: SecretSources<'_>,
) -> HttpProviderResult<reqwest::header::HeaderMap> {
    let mut resolved = reqwest::header::HeaderMap::new();
    for (raw_name, value) in headers.iter().collect::<BTreeMap<_, _>>() {
        let name =
            reqwest::header::HeaderName::from_bytes(raw_name.as_bytes()).map_err(|error| {
                HttpProviderError::Decode(format!(
                    "provider '{provider_name}' has an invalid header name: {error}"
                ))
            })?;
        let value = if harw_config::ProviderToml::is_sensitive_header_name(raw_name) {
            let reference = value.parse::<harw_config::SecretRef>().map_err(|_| {
                HttpProviderError::Decode(format!(
                    "provider '{provider_name}' header '{name}' carries credentials and must be a secret reference (env:/file:/file-json:/keyring:/secrets:)"
                ))
            })?;
            let secret = resolve_secret(&reference, sources)?;
            sensitive_header_value(secret.expose_secret()).map_err(|_| {
                HttpProviderError::Decode(format!(
                    "provider '{provider_name}' header '{name}' resolved to an invalid header value"
                ))
            })?
        } else {
            reqwest::header::HeaderValue::from_str(value).map_err(|error| {
                HttpProviderError::Decode(format!(
                    "provider '{provider_name}' has an invalid header value: {error}"
                ))
            })?
        };
        resolved.insert(name, value);
    }
    Ok(resolved)
}

/// Returns a bounded request ID from common OpenAI-compatible gateway headers.
fn provider_request_id(headers: &reqwest::header::HeaderMap) -> Option<String> {
    ["x-request-id", "request-id", "x-amzn-requestid"]
        .iter()
        .find_map(|name| headers.get(*name))
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(128).collect())
}

/// Renders a typed remote-response error without retaining an arbitrary body.
#[cfg(test)]
fn sanitized_provider_error(status: u16, request_id: Option<&str>, body: &str) -> String {
    HttpProviderError::remote_response(status, request_id.map(str::to_owned), body).to_string()
}

/// Extrahiert die empfohlene Wartezeit aus einem HTTP-429-Response.
///
/// # Beschreibung
/// Konsultiert in dieser Reihenfolge:
/// 1. `Retry-After`-Header: numerischer Wert (Sekunden) oder HTTP-Datum
///    (HTTP-Datum wird best-effort ignoriert, Fallback auf Body-Extraktion).
/// 2. Body-Text: Regex `(?i)wait\s+(\d+)\s+seconds?` (ohne externe `regex`-
///    Dependency via `str::find` / `str::split_whitespace`).
/// 3. Fallback: 30 Sekunden.
///
/// # Arguments
/// - `retry_after_header` (`Option<&str>`): Inhalt des `Retry-After`-Headers.
/// - `body` (`&str`): Rohtext des Antwort-Bodys.
///
/// # Returns
/// `std::time::Duration` mit der ermittelten Wartezeit.
#[must_use]
pub(crate) fn parse_retry_after(
    retry_after_header: Option<&str>,
    body: &str,
) -> std::time::Duration {
    const DEFAULT_SECS: u64 = 30;

    // 1. Retry-After-Header als numerischer Sekundenwert.
    if let Some(header_val) = retry_after_header {
        if let Ok(secs) = header_val.trim().parse::<u64>() {
            return std::time::Duration::from_secs(secs);
        }
        // HTTP-Date-Format: ignoriert, Fallback auf Body.
    }

    // 2. Body-Extraktion: suche "wait N seconds" (case-insensitive).
    //    Einfache Variante ohne externe Regex-Crate:
    //    suche "wait " (case-insensitive), dann extrahiere die folgende Zahl.
    let lower = body.to_ascii_lowercase();
    if let Some(pos) = lower.find("wait ") {
        let after_wait = &lower[pos + "wait ".len()..];
        let num_str: String = after_wait
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(secs) = num_str.parse::<u64>() {
            if secs > 0 {
                return std::time::Duration::from_secs(secs);
            }
        }
    }

    std::time::Duration::from_secs(DEFAULT_SECS)
}

/// Wählt den [`Transport`] anhand des `api`-Strings eines Providers.
///
/// `"openai-responses"` ergibt [`Transport::Responses`]; jeder andere Wert
/// (z. B. `"openai-chat"`) ergibt [`Transport::Chat`].
fn transport_from_api(api: &str) -> Transport {
    match api {
        "openai-responses" => Transport::Responses,
        _ => Transport::Chat,
    }
}

/// Löst eine [`harw_config::SecretRef`] zu ihrem Klartext-Wert auf.
///
/// Unterstützt `env:`, `file:`, `file-json:` und `keyring:`. Für `env:`-Refs wird
/// zusätzlich der `env_layer` aus `~/.harw/.env` als Fallback konsultiert:
/// Prozess-Umgebung gewinnt, wenn die Variable dort gesetzt und nicht leer
/// ist; andernfalls wird der Env-Layer konsultiert.
///
/// `keyring:` erwartet exakt `keyring:<service>/<account>` (genau ein `/`,
/// beide Teile nicht leer) und liest den Eintrag über das `keyring`-Crate aus
/// dem System-Keyring (macOS Keychain, Windows Credential Manager, Secret
/// Service). Der injizierte [`SecretResolver`] wird dafür bewusst **nicht**
/// befragt: sein Vertrag deckt nur den Bezeichner nach `secrets:` ab.
///
/// `secrets:` wird an den injizierten [`SecretResolver`] delegiert (er erhält
/// den Bezeichner ohne Präfix). Ohne Resolver schlägt die Auflösung fail-closed
/// mit [`HttpProviderError::UnsupportedCredentialReference`] fehl; Aufrufer
/// müssen dann einen Resolver über die `*_with_resolver`-Konstruktoren
/// injizieren oder auf `env:`/`file:`/`keyring:` ausweichen.
/// Resolver- und Keyring-Fehler werden auf feste Gründe abgebildet, damit weder
/// Secret-Werte noch Resolver-Diagnosen in Fehlertexte gelangen.
/// `file:`/`file-json:` lesen nur unterhalb von `<home>/secrets/` (siehe
/// [`read_private_secret_file`]); ihre Fehler nennen weder Pfad noch Inhalt.
/// Ausnahme für `file-json:`: liegt der Pfad außerhalb von `<home>/secrets`,
/// wird zusätzlich [`read_external_cli_credential`] versucht — sie akzeptiert
/// ausschließlich die in [`EXTERNAL_CLI_CREDENTIALS`] allowlisteten Pfad/
/// Pointer-Paare (`~/.codex/auth.json`, `~/.claude/.credentials.json`) und nur,
/// wenn `sources.endpoint` auf den jeweils offiziellen Host zeigt.
///
/// # Arguments
/// - `secret_ref` (`&harw_config::SecretRef`): Zu lösende Referenz.
/// - `sources` ([`SecretSources`]): Env-Layer, optionaler Resolver, optionales Home.
///
/// # Errors
/// - [`HttpProviderError::UnsupportedCredentialReference`]: `secrets:` ohne
///   injizierten [`SecretResolver`].
/// - [`HttpProviderError::UnresolvedCredential`]: Referenz nicht auflösbar
///   (Variable fehlt, Datei unsicher/unlesbar, Resolver- oder Keyring-Fehler,
///   ungültige `keyring:`-Form) oder aufgelöster Wert leer.
fn resolve_secret(
    secret_ref: &harw_config::SecretRef,
    sources: SecretSources<'_>,
) -> HttpProviderResult<SecretString> {
    use harw_config::SecretRef;
    let env_layer = sources.env_layer;
    let resolver = sources.resolver;
    let reference = diagnostic_reference(secret_ref);
    match secret_ref {
        SecretRef::Env(name) => {
            let value = harw_config::resolve_env_ref(name, env_layer).ok_or_else(|| {
                HttpProviderError::UnresolvedCredential {
                    reference: reference.clone(),
                    reason: format!(
                        "environment variable '{name}' not set in process environment or ~/.harw/.env"
                    ),
                }
            })?;
            validate_resolved_secret(reference, SecretString::new(value.into()))
        }
        SecretRef::File(path) => {
            let raw = read_private_secret_file(sources.home, path).map_err(|reason| {
                HttpProviderError::UnresolvedCredential {
                    reference: reference.clone(),
                    reason: reason.to_owned(),
                }
            })?;
            validate_resolved_secret(
                reference,
                SecretString::new(raw.expose_secret().trim().to_owned().into()),
            )
        }
        SecretRef::FileJson { path, pointer } => {
            let raw = read_private_secret_file(sources.home, path)
                .or_else(|reason| {
                    if reason == FILE_CREDENTIAL_OUTSIDE_SECRETS_REASON {
                        read_external_cli_credential(sources.endpoint, path, pointer)
                            .unwrap_or(Err(reason))
                    } else {
                        Err(reason)
                    }
                })
                .map_err(|reason| HttpProviderError::UnresolvedCredential {
                    reference: reference.clone(),
                    reason: reason.to_owned(),
                })?;
            let doc: serde_json::Value =
                serde_json::from_str(raw.expose_secret()).map_err(|_| {
                    HttpProviderError::UnresolvedCredential {
                        reference: reference.clone(),
                        reason: FILE_CREDENTIAL_JSON_REASON.to_owned(),
                    }
                })?;
            let value = doc
                .pointer(pointer)
                .and_then(|value| value.as_str())
                .ok_or_else(|| HttpProviderError::UnresolvedCredential {
                    reference: reference.clone(),
                    reason: FILE_CREDENTIAL_POINTER_REASON.to_owned(),
                })?;
            validate_resolved_secret(reference, SecretString::new(value.trim().to_owned().into()))
        }
        SecretRef::Secrets(name) => resolver
            .ok_or_else(|| HttpProviderError::UnsupportedCredentialReference {
                reference: reference.clone(),
            })?
            .resolve(name)
            .map_err(|_| HttpProviderError::UnresolvedCredential {
                reference: reference.clone(),
                reason: SECRET_RESOLVER_FAILURE_REASON.to_owned(),
            })
            .and_then(|secret| validate_resolved_secret(reference, secret)),
        SecretRef::Keyring(payload) => {
            let (service, account) = parse_keyring_reference(payload).ok_or_else(|| {
                HttpProviderError::UnresolvedCredential {
                    reference: reference.clone(),
                    reason: INVALID_KEYRING_REFERENCE_REASON.to_owned(),
                }
            })?;
            let entry = keyring::Entry::new(service, account).map_err(|_| {
                HttpProviderError::UnresolvedCredential {
                    reference: reference.clone(),
                    reason: KEYRING_FAILURE_REASON.to_owned(),
                }
            })?;
            let password =
                entry
                    .get_password()
                    .map_err(|_| HttpProviderError::UnresolvedCredential {
                        reference: reference.clone(),
                        reason: KEYRING_FAILURE_REASON.to_owned(),
                    })?;
            validate_resolved_secret(reference, SecretString::new(password.into()))
        }
    }
}

/// Löst das Credential eines konfigurierten Providers auf (Design
/// `doc.read_pdf` § „Anbindung → W2": `harw-cli`s Mistral-OCR-Installation
/// nutzt sie, um den Key des gewählten Mistral-Providers zu bekommen, ohne
/// den internen Aufbaupfad von [`build_provider`] zu duplizieren).
///
/// # Description
/// Fehlt `provider.auth`, ist das Ergebnis `Ok(None)` — kein Credential
/// konfiguriert, der Aufrufer entscheidet dann selbst (z. B. Provider
/// überspringen). Ist `auth` gesetzt, wird die `SecretRef` exakt wie im
/// internen Auflösungspfad (siehe [`build_named_provider`]) über
/// [`resolve_secret`] aufgelöst: `sources.endpoint` wird an
/// `provider.base_url` gebunden, damit endpoint-gebundene Referenzen (z. B.
/// `file-json:` auf fremde CLI-Credentials) nur an ihren offiziellen Host
/// gehen.
///
/// # Arguments
/// - `provider` (`&harw_config::ProviderToml`): Provider-Konfiguration,
///   deren `auth`-`SecretRef` (falls vorhanden) aufgelöst wird.
/// - `env_layer` (`&BTreeMap<String, String>`): Env-Layer aus den `.env`-
///   Dateien der Konfigurations-Layer (siehe [`SecretSources::env_layer`]).
/// - `home` (`Option<&Path>`): harw-Home für `file:`/`file-json:`-Referenzen;
///   `None` lässt diese fail-closed scheitern (siehe [`resolve_secret`]).
/// - `resolver` (`Option<&dyn SecretResolver>`): injizierter Resolver für
///   `secrets:`-Referenzen.
///
/// # Returns
/// `Ok(None)`, wenn `provider.auth` fehlt; sonst `Ok(Some(secret))` mit dem
/// aufgelösten Klartext-Credential.
///
/// # Errors
/// Wie [`resolve_secret`]: u. a. [`HttpProviderError::UnresolvedCredential`]
/// (Referenz nicht auflösbar) und
/// [`HttpProviderError::UnsupportedCredentialReference`] (`secrets:` ohne
/// injizierten Resolver).
///
/// # Concurrency
/// Reiner Aufbau ohne geteilten Zustand; beliebig parallel aufrufbar.
///
/// # Examples
/// ```rust,no_run
/// use harw_provider_http::resolve_provider_credential;
/// use std::collections::BTreeMap;
///
/// # fn example(provider: &harw_config::ProviderToml) -> harw_provider_http::HttpProviderResult<()> {
/// let env_layer = BTreeMap::new();
/// let credential = resolve_provider_credential(provider, &env_layer, None, None)?;
/// # let _ = credential;
/// # Ok(())
/// # }
/// ```
pub fn resolve_provider_credential(
    provider: &harw_config::ProviderToml,
    env_layer: &BTreeMap<String, String>,
    home: Option<&Path>,
    resolver: Option<&dyn SecretResolver>,
) -> HttpProviderResult<Option<SecretString>> {
    let Some(secret_ref) = &provider.auth else {
        return Ok(None);
    };
    let sources = SecretSources {
        env_layer,
        resolver,
        home,
        endpoint: Some(&provider.base_url),
    };
    resolve_secret(secret_ref, sources).map(Some)
}

/// Referenz-Form für Fehlertexte: Dateireferenzen ohne Pfad (kein Orakel für
/// Dateinamen/Home-Layout), alle anderen in kanonischer Form.
fn diagnostic_reference(secret_ref: &harw_config::SecretRef) -> String {
    match secret_ref {
        harw_config::SecretRef::File(_) => "file:<redacted>".to_owned(),
        harw_config::SecretRef::FileJson { .. } => "file-json:<redacted>".to_owned(),
        other => other.as_ref_string(),
    }
}

/// Liest eine Credential-Datei ausschließlich unterhalb von `<home>/secrets/`.
///
/// # Description
/// 1. Ohne Home → Fehler (fail closed).
/// 2. `raw_path` muss absolut sein und lexikalisch unter `<home>/secrets`
///    oder `<kanonisches home>/secrets` liegen (Onboarding und `harw auth`
///    schreiben kanonische Pfade); der Rest darf nur normale Komponenten
///    haben (kein `..`, nicht leer).
/// 3. `open_dir_nofollow(<secrets>)` (das Verzeichnis selbst darf kein Symlink
///    sein) und `open_beneath(.., rest)` (`RESOLVE_BENEATH |
///    RESOLVE_NO_SYMLINKS`: **kein** Glied darf ein Symlink sein — strenger als
///    `open_nofollow`, das Zwischenverzeichnisse auflösen würde).
/// 4. `ensure_private_regular`: reguläre Datei, Eigentümer = effektive UID,
///    `mode & 0o077 == 0`.
/// 5. Höchstens [`MAX_FILE_CREDENTIAL_BYTES`], UTF-8.
///
/// # Errors
/// Einer der statischen `FILE_CREDENTIAL_*`-Gründe, nie mit Pfad oder Inhalt.
fn read_private_secret_file(
    home: Option<&Path>,
    raw_path: &str,
) -> Result<SecretString, &'static str> {
    use std::io::Read as _;
    use std::os::fd::AsFd as _;

    let home = home.ok_or(FILE_CREDENTIAL_NO_HOME_REASON)?;
    let (secrets_dir, relative) = secret_file_location(home, Path::new(raw_path))
        .ok_or(FILE_CREDENTIAL_OUTSIDE_SECRETS_REASON)?;
    let root =
        harw_fsutil::open_dir_nofollow(&secrets_dir).map_err(|_| FILE_CREDENTIAL_OPEN_REASON)?;
    let file =
        harw_fsutil::open_beneath(root.as_fd(), &relative, harw_fsutil::OpenMode::read_only())
            .map_err(|_| FILE_CREDENTIAL_OPEN_REASON)?;
    harw_fsutil::ensure_private_regular(&file).map_err(|_| FILE_CREDENTIAL_NOT_PRIVATE_REASON)?;
    let mut contents = String::new();
    file.take(MAX_FILE_CREDENTIAL_BYTES + 1)
        .read_to_string(&mut contents)
        .map_err(|_| FILE_CREDENTIAL_READ_REASON)?;
    let secret = SecretString::new(contents.into());
    let read_bytes = u64::try_from(secret.expose_secret().len()).unwrap_or(u64::MAX);
    if read_bytes > MAX_FILE_CREDENTIAL_BYTES {
        return Err(FILE_CREDENTIAL_READ_REASON);
    }
    Ok(secret)
}

/// Liest eine allowlistete CLI-Credential-Datei (siehe
/// [`EXTERNAL_CLI_CREDENTIALS`]).
///
/// # Description
/// Nur [`resolve_secret`] ruft diese Funktion auf, und zwar ausschließlich als
/// Fallback für `file-json:`-Referenzen außerhalb von `<home>/secrets`. Die
/// Datei wird gelesen wie unter `<home>/secrets`: Elternverzeichnis ohne
/// Symlink (`open_dir_nofollow`), Datei ohne Symlink (`open_beneath` mit
/// `RESOLVE_NO_SYMLINKS`), reguläre Datei des effektiven Nutzers ohne
/// Gruppen-/Fremdrechte (`ensure_private_regular`), höchstens
/// [`MAX_FILE_CREDENTIAL_BYTES`].
///
/// # Arguments
/// - `endpoint` (`Option<&str>`): Base-URL des Providers, für den gerade
///   aufgelöst wird; `None` lehnt jede externe CLI-Credential-Datei ab.
/// - `raw_path` (`&str`): absoluter Pfad aus der `file-json:`-Referenz; muss
///   exakt `$HOME/<relative>` eines [`EXTERNAL_CLI_CREDENTIALS`]-Eintrags sein.
/// - `pointer` (`&str`): JSON-Pointer aus der Referenz; muss exakt in der
///   Pointer-Liste des passenden Allowlist-Eintrags enthalten sein.
///
/// # Returns
/// `None`, wenn `raw_path`/`pointer`/`endpoint` nicht zur Allowlist passen —
/// der Aufrufer behält dann seinen ursprünglichen `Err(reason)`. `Some(Ok(_))`
/// mit dem Dateiinhalt als [`SecretString`], falls die Datei gelesen werden
/// konnte; `Some(Err(reason))`, falls sie zur Allowlist passt, aber nicht
/// sicher lesbar ist (kein Symlink-freier Zugriff, falsche Rechte, zu groß).
///
/// # Concurrency
/// Rein lesend und zustandslos; sicher aus mehreren Threads parallel aufrufbar.
fn read_external_cli_credential(
    endpoint: Option<&str>,
    raw_path: &str,
    pointer: &str,
) -> Option<Result<SecretString, &'static str>> {
    use std::io::Read as _;
    use std::os::fd::AsFd as _;

    let endpoint = endpoint?;
    // ChatGPT credentials are bound to the Codex API path as well as its host.
    if pointer == "/tokens/access_token" && endpoint.trim_end_matches('/') != codex::BASE_URL {
        return None;
    }
    let user_home = std::env::var_os("HOME").filter(|home| !home.is_empty())?;
    let user_home = PathBuf::from(user_home);
    let path = Path::new(raw_path);
    let (relative, _, _) =
        EXTERNAL_CLI_CREDENTIALS
            .iter()
            .find(|(relative, pointers, hosts)| {
                path == user_home.join(relative)
                    && pointers.contains(&pointer)
                    && hosts
                        .iter()
                        .any(|host| endpoint_is_official_host(endpoint, host))
            })?;
    let full = user_home.join(relative);
    let (Some(parent), Some(file_name)) = (full.parent(), full.file_name()) else {
        return Some(Err(FILE_CREDENTIAL_OPEN_REASON));
    };
    Some((|| {
        let root =
            harw_fsutil::open_dir_nofollow(parent).map_err(|_| FILE_CREDENTIAL_OPEN_REASON)?;
        let file = harw_fsutil::open_beneath(
            root.as_fd(),
            Path::new(file_name),
            harw_fsutil::OpenMode::read_only(),
        )
        .map_err(|_| FILE_CREDENTIAL_OPEN_REASON)?;
        harw_fsutil::ensure_private_regular(&file)
            .map_err(|_| FILE_CREDENTIAL_NOT_PRIVATE_REASON)?;
        let mut contents = String::new();
        file.take(MAX_FILE_CREDENTIAL_BYTES + 1)
            .read_to_string(&mut contents)
            .map_err(|_| FILE_CREDENTIAL_READ_REASON)?;
        if contents.len() as u64 > MAX_FILE_CREDENTIAL_BYTES {
            return Err(FILE_CREDENTIAL_READ_REASON);
        }
        Ok(SecretString::new(contents.into()))
    })())
}

/// Zerlegt `path` in (`<home>/secrets`-Verzeichnis, relativer Rest), falls der
/// Pfad lexikalisch darunter liegt; siehe [`read_private_secret_file`].
fn secret_file_location(home: &Path, path: &Path) -> Option<(PathBuf, PathBuf)> {
    if !path.is_absolute() {
        return None;
    }
    let mut candidates = vec![home.join("secrets")];
    if let Ok(canonical_home) = home.canonicalize() {
        candidates.push(canonical_home.join("secrets"));
    }
    candidates.into_iter().find_map(|secrets_dir| {
        let relative = path.strip_prefix(&secrets_dir).ok()?.to_path_buf();
        let only_normal = relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)));
        (only_normal && !relative.as_os_str().is_empty()).then_some((secrets_dir, relative))
    })
}

/// Zerlegt die Nutzlast einer `keyring:`-Referenz in (`service`, `account`).
///
/// Akzeptiert nur exakt `service/account` mit genau einem `/` und zwei
/// nicht-leeren Teilen; alles andere ergibt `None`.
fn parse_keyring_reference(payload: &str) -> Option<(&str, &str)> {
    let (service, account) = payload.split_once('/')?;
    (!service.is_empty() && !account.is_empty() && !account.contains('/'))
        .then_some((service, account))
}

fn validate_resolved_secret(
    reference: String,
    secret: SecretString,
) -> HttpProviderResult<SecretString> {
    if secret.expose_secret().trim().is_empty() {
        return Err(HttpProviderError::UnresolvedCredential {
            reference,
            reason: EMPTY_CREDENTIAL_REASON.to_owned(),
        });
    }
    Ok(secret)
}

/// Liefert das Parameter-Schema eines Function-Tools in der Form, die der
/// Provider für das gesetzte `strict`-Flag erwartet.
///
/// # Description
/// OpenAI-kompatible Provider lehnen ein Tool mit `strict: true` ab, wenn
/// `required` nicht jeden Key aus `properties` enthält (HTTP 400,
/// „'required' is required to be supplied and to be an array including every
/// key in properties"). Für strikte Tools wird das Schema deshalb über
/// [`JsonSchema::into_strict`] normalisiert: optionale Felder werden nullable
/// und wandern nach `required`. Nicht-strikte Tools gehen unverändert auf den
/// Wire.
///
/// # Arguments
/// - `spec` (`&FunctionToolSpec`): die Tool-Spezifikation.
///
/// # Returns
/// Das zu serialisierende Parameter-Schema.
fn strict_parameters(spec: &FunctionToolSpec) -> JsonSchema {
    if spec.strict {
        spec.parameters.clone().into_strict()
    } else {
        spec.parameters.clone()
    }
}

/// Übersetzt die dem Modell angebotenen [`ToolSpec`]s in
/// `ResponsesRequest`-`ToolDef`s (`{type:"function", name, description,
/// parameters, strict}`).
///
/// # Description
/// Aktuell existiert nur `ToolSpec::Function`. Die `strict`-Flagge der
/// [`harw_tools::FunctionToolSpec`] wird 1:1 übernommen (nicht wie
/// [`ToolDef::function`] hart auf `true` gesetzt), damit nicht-strikte
/// Tool-Definitionen korrekt auf den Wire kommen.
fn build_responses_tools(tools: &[ToolSpec]) -> Vec<ToolDef> {
    let names = tool_names::ToolNameCodec::for_tools(tools);
    tools
        .iter()
        .map(|spec| match spec {
            ToolSpec::Function(f) => {
                let parameters = serde_json::to_value(strict_parameters(f))
                    .unwrap_or_else(|_| serde_json::json!({}));
                ToolDef {
                    kind: "function".to_owned(),
                    name: names.encode(f.name.as_str()),
                    description: Some(f.description.clone()),
                    parameters,
                    strict: f.strict,
                }
            }
        })
        .collect()
}

/// Übersetzt einen [`ModelRequest`] in eine `ResponsesRequest`.
///
/// # Description
/// System-Prompt und Instruction-Fragmente werden zum `instructions`-Feld
/// zusammengefasst; der Verlauf wird auf `InputItem`-Nachrichten projiziert:
/// `User`/`Assistant` → `Message`, `ToolCall` → `InputItem::FunctionCall`
/// (`call_id`/`name`/`arguments` — Argumente als JSON-String, Wire-Format der
/// Responses-API), `ToolResult` → `InputItem::FunctionCallOutput`.
/// `request.tools` wird — sofern nicht leer — via [`build_responses_tools`]
/// auf `req.tools` abgebildet.
///
/// Ist `request.reasoning_effort` gesetzt, wird es via
/// [`map_effort_to_openai`] auf den OpenAI-Wire-Wert abgebildet und in
/// `req.reasoning` als [`ReasoningConfig`] mit `summary: None` hinterlegt;
/// ist es `None`, bleibt `req.reasoning` unverändert `None`. Dieser Pfad wird
/// ausschließlich für [`Transport::Responses`] verwendet — `build_chat_body`
/// (Chat-Completions) bleibt unangetastet.
///
/// # Arguments
/// - `model` (`&str`): Modellname für das `model`-Feld.
/// - `request` (`&ModelRequest`): Quelle für System-Prompt, Fragmente,
///   Verlauf, Tools und optionalen `reasoning_effort`.
///
/// # Returns
/// Eine vollständig befüllte [`ResponsesRequest`], bereit zur Serialisierung.
fn build_request(model: &str, request: &ModelRequest) -> ResponsesRequest {
    let renderer = ToolResultRenderer::new(request);
    // Interne Namen wie `fs.read` verletzen `^[a-zA-Z0-9_-]+$` der API.
    let names = tool_names::ToolNameCodec::for_request(request);
    let mut input = Vec::new();
    for message in request.history.to_model_messages() {
        match message {
            harw_core::ModelMessage::User { text } => {
                input.push(InputItem::Message {
                    role: "user".to_owned(),
                    content: vec![ContentPart::InputText { text }],
                });
            }
            harw_core::ModelMessage::Assistant { text } => {
                input.push(InputItem::Message {
                    role: "assistant".to_owned(),
                    content: vec![ContentPart::OutputText { text }],
                });
            }
            harw_core::ModelMessage::ToolCall {
                call_id,
                name,
                arguments,
            } => {
                input.push(InputItem::FunctionCall {
                    call_id: call_id.to_string(),
                    name: names.encode(&name),
                    arguments: arguments.to_string(),
                });
            }
            harw_core::ModelMessage::ToolResult { call_id, result } => {
                let output = renderer.render(&call_id, &result);
                input.push(InputItem::FunctionCallOutput {
                    call_id: call_id.to_string(),
                    output,
                });
            }
        }
    }
    if let Some(data_block) = non_empty_data_block(request) {
        input.push(InputItem::Message {
            role: "user".to_owned(),
            content: vec![ContentPart::InputText {
                text: data_block.to_owned(),
            }],
        });
    }

    let mut req = ResponsesRequest::new(model.to_owned(), input);
    req.stream = false;
    req.store = false;
    if !request.tools.is_empty() {
        req.tools = build_responses_tools(&request.tools);
    }
    let mut instructions = request.system_prompt.clone();
    for fragment in &request.instruction_fragments {
        instructions.push('\n');
        instructions.push_str(fragment);
    }
    if !instructions.is_empty() {
        req.instructions = Some(instructions);
    }
    if let Some(effort) = request.reasoning_effort {
        req.reasoning = Some(ReasoningConfig {
            effort: Some(map_effort_to_openai(effort).to_owned()),
            summary: Some("auto".to_owned()),
        });
    }
    req
}

/// Liefert den nicht-leeren Datenblock eines Requests.
fn non_empty_data_block(request: &ModelRequest) -> Option<&str> {
    request
        .data_block
        .as_deref()
        .filter(|data_block| !data_block.trim().is_empty())
}

/// Rendert Tool-Ergebnisse des Verlaufs über die Trust-Hülle (W3 C-PROTO).
///
/// # Description
/// `ModelMessage::ToolResult` trägt weder Tool-Namen noch Trust-Klasse; beide
/// werden einmal je Request aus `ConversationHistory::items` je `call_id`
/// gesammelt. Fehlt der Call, heißt das Tool [`UNKNOWN_TOOL_NAME`]; fehlt das
/// Ergebnis-Item, gilt der sichere Default `Untrusted`.
struct ToolResultRenderer {
    names: BTreeMap<String, String>,
    trusts: BTreeMap<String, ResultTrust>,
    max_bytes: usize,
}

impl ToolResultRenderer {
    // Sammelt Namen/Trust aus dem Verlauf und den Byte-Deckel aus dem Request.
    fn new(request: &ModelRequest) -> Self {
        let mut names = BTreeMap::new();
        let mut trusts = BTreeMap::new();
        for item in request.history.items() {
            match item {
                TurnItem::ToolCall(call) => {
                    names.insert(call.call_id.as_str().to_owned(), call.tool_name.clone());
                }
                TurnItem::ToolResult(result) => {
                    trusts.insert(result.call_id.as_str().to_owned(), result.trust);
                }
                _ => {}
            }
        }
        Self {
            names,
            trusts,
            max_bytes: request
                .tool_result_max_bytes
                .unwrap_or(DEFAULT_TOOL_RESULT_MAX_BYTES),
        }
    }

    // Wire-Text eines Tool-Ergebnisses (Envelope bei `Untrusted`).
    fn render(&self, call_id: &ToolCallId, result: &ToolCallResult) -> String {
        let tool = self
            .names
            .get(call_id.as_str())
            .map_or(UNKNOWN_TOOL_NAME, String::as_str);
        let trust = self
            .trusts
            .get(call_id.as_str())
            .copied()
            .unwrap_or_default();
        render_tool_result(tool, trust, result, self.max_bytes).text
    }
}

/// Baut den vollständigen `/responses`-Body inkl. Reasoning-Replay.
///
/// # Description
/// Serialisiert [`build_request`] und ergänzt, was `ResponsesRequest` (in
/// `harw-provider`) nicht modelliert:
/// - gemerkte Reasoning-Items unmittelbar **vor** dem ersten `function_call`
///   ihrer Runde (unverändert, in Originalreihenfolge);
/// - `max_output_tokens` aus `ModelRequest::max_output_tokens`;
/// - `include: ["reasoning.encrypted_content"]`, wenn Reasoning angefordert ist.
///
/// # Errors
/// `serde_json::Error`, falls die Serialisierung scheitert.
fn build_responses_body(
    model: &str,
    request: &ModelRequest,
    replay: &[ReplayEntry],
) -> Result<Value, serde_json::Error> {
    let mut body = serde_json::to_value(build_request(model, request))?;
    if !replay.is_empty() {
        if let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) {
            let items = std::mem::take(input);
            let mut emitted = vec![false; replay.len()];
            for item in items {
                let function_call_id = (item.get("type").and_then(Value::as_str)
                    == Some("function_call"))
                .then(|| item.get("call_id").and_then(Value::as_str))
                .flatten();
                if let Some(call_id) = function_call_id {
                    for (entry, done) in replay.iter().zip(emitted.iter_mut()) {
                        if !*done && entry.call_ids.iter().any(|id| id == call_id) {
                            *done = true;
                            input.extend(entry.blocks.iter().cloned());
                        }
                    }
                }
                input.push(item);
            }
        }
    }
    if let Some(max_output_tokens) = request.max_output_tokens {
        body["max_output_tokens"] = Value::from(max_output_tokens);
    }
    if request.reasoning_effort.is_some() {
        body["include"] = serde_json::json!([INCLUDE_ENCRYPTED_REASONING]);
    }
    Ok(body)
}

/// Kürzt einen provider-gelieferten Grund auf ein unkritisches Token.
fn bounded_reason(reason: &str) -> String {
    reason
        .chars()
        .filter(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
        .take(MAX_STOP_REASON_CHARS)
        .collect()
}

/// Sammelt `refusal`-Parts der `message`-Items einer Responses-Antwort.
fn extract_responses_refusal(body: &Value) -> Option<String> {
    let output = body.get("output")?.as_array()?;
    let refusal: String = output
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("message"))
        .filter_map(|item| item.get("content").and_then(Value::as_array))
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("refusal"))
        .filter_map(|part| part.get("refusal").and_then(Value::as_str))
        .collect();
    (!refusal.is_empty()).then_some(refusal)
}

/// Projiziert eine nicht-gestreamte Responses-Antwort auf den W3-Vertrag.
///
/// # Description
/// - `status` fehlt → wie `completed`; `failed`/`cancelled` → `RequestFailed`.
/// - `incomplete`: `incomplete_details.reason` `max_output_tokens` →
///   [`StopReason::MaxTokens`], `content_filter` → [`StopReason::ContentFilter`],
///   sonst `Other`. Teiltext bleibt erhalten; `function_call`-Items werden
///   verworfen (Argumente können abgeschnitten sein — nie ausführen).
/// - sonst: Tool-Calls → `ToolUse`; nur Refusal-Parts → `Refusal`; Text → `EndTurn`;
///   gar nichts → `EmptyResponse`.
/// - `reasoning`-Items → `OpaqueReasoning{provider, model, blocks}` (verbatim);
///   Items mit `encrypted_content` einer Tool-Call-Runde → [`ReplayEntry`].
///
/// # Errors
/// `RequestFailed` bei `failed`/`cancelled` oder defekten Tool-Calls;
/// `EmptyResponse` bei leerer, vollständiger Antwort.
fn interpret_responses(
    body: &Value,
    provider: &str,
    model: &str,
) -> Result<(ModelResponse, Option<ReplayEntry>), ModelError> {
    let status = body
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("completed");
    if matches!(status, "failed" | "cancelled") {
        return Err(ModelError::RequestFailed(format!(
            "provider reported response status '{status}'"
        )));
    }
    let incomplete = (status == "incomplete").then(|| {
        body.pointer("/incomplete_details/reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    });
    let output = body
        .get("output")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let text = extract_assistant_text(body);
    let refusal = extract_responses_refusal(body);
    let tool_calls = if incomplete.is_some() {
        let dropped = output
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
            .count();
        if dropped > 0 {
            tracing::warn!(dropped, "dropping tool calls of an incomplete response");
        }
        Vec::new()
    } else {
        extract_openai_tool_calls(body, Transport::Responses)
            .map_err(|error| ModelError::RequestFailed(error.to_string()))?
    };
    let stop = match incomplete {
        Some("max_output_tokens") => StopReason::MaxTokens,
        Some("content_filter") => StopReason::ContentFilter,
        Some(other) => StopReason::Other(bounded_reason(other)),
        None if !tool_calls.is_empty() => StopReason::ToolUse,
        None if text.is_none() && refusal.is_some() => StopReason::Refusal {
            detail: refusal.clone(),
        },
        None => StopReason::EndTurn,
    };
    if incomplete.is_none() && text.is_none() && tool_calls.is_empty() && refusal.is_none() {
        return Err(ModelError::EmptyResponse);
    }

    let blocks: Vec<Value> = output
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
        .cloned()
        .collect();
    let replayable: Vec<Value> = blocks
        .iter()
        .filter(|block| {
            block
                .get("encrypted_content")
                .and_then(Value::as_str)
                .is_some_and(|content| !content.is_empty())
        })
        .cloned()
        .collect();
    let replay = (!tool_calls.is_empty() && !replayable.is_empty()).then(|| ReplayEntry {
        call_ids: tool_calls
            .iter()
            .map(|call| call.id.as_str().to_owned())
            .collect(),
        provider: provider.to_owned(),
        model: model.to_owned(),
        blocks: replayable,
    });
    let reasoning = (!blocks.is_empty()).then(|| OpaqueReasoning {
        provider: provider.to_owned(),
        model: model.to_owned(),
        blocks,
    });

    Ok((
        ModelResponse {
            message: text,
            tool_calls,
            usage: extract_openai_usage(body, Transport::Responses),
            stop,
            reasoning,
        },
        replay,
    ))
}

/// Projiziert eine nicht-gestreamte Chat-Completions-Antwort auf den W3-Vertrag.
///
/// # Description
/// `choices[0].finish_reason`: `length` → `MaxTokens`, `content_filter` →
/// `ContentFilter` (beide: Tool-Calls verworfen, Teiltext bleibt);
/// vorhandene Tool-Calls → `ToolUse` (auch bei `stop`, manche Gateways);
/// nur `message.refusal` → `Refusal`; `stop`/`tool_calls`/fehlend → `EndTurn`;
/// unbekannt → `Other`.
///
/// `choices[0].message.reasoning_content` (DeepSeek/Kimi/GLM-Konvention):
/// vorhanden und nicht leer/nur Whitespace → `OpaqueReasoning{provider, model,
/// blocks: [{"type": "reasoning_content", "text": ...}]}`; fehlt das Feld
/// oder ist es leer, bleibt `reasoning: None` (kein Verhaltensbruch für
/// Provider ohne dieses Feld).
///
/// Ein führender `<think>...</think>`-Block bzw. ein verwaister führender
/// `</think>`-Marker in `content` wird immer (auch ohne Tool-Calls)
/// entfernt; sein Text landet — sofern `reasoning_content` fehlt — als
/// `{"type": "think", "text": ...}`-Block in `reasoning` (siehe
/// [`text_tool_calls::strip_leading_think`]).
///
/// Manche GLM-5.x/Kimi-Gateways senden Tool-Calls als Text in `content`
/// (`<tool_call>...`) statt strukturiert in `message.tool_calls`. Bot der
/// Request `offered_tools` an und blieb `message.tool_calls` leer, versucht
/// [`text_tool_calls::parse_text_tool_calls`] den Text zu parsen; gelingt
/// das (jeder `<tool_call>`-Name in `offered_tools`, alle Segmente
/// vertrauenswürdig parsbar), werden daraus synthetische `ToolCall`s mit
/// `call_text_<n>`-IDs — sonst bleibt die Antwort unverändert (kein Raten).
///
/// # Arguments
/// - `body` (`&Value`): der geparste JSON-Antwortkörper.
/// - `provider` (`&str`): Provider-ID für `OpaqueReasoning::provider`.
/// - `model` (`&str`): Modellname für `OpaqueReasoning::model`.
/// - `offered_tools` (`&[&str]`): die für diesen Request tatsächlich
///   angebotenen, Wire-kodierten Tool-Namen (leer, wenn keine Tools
///   angeboten wurden) — Fail-closed-Grenze für [`text_tool_calls`].
///
/// # Errors
/// `RequestFailed` bei defekten Tool-Calls; `EmptyResponse`, wenn eine nicht
/// abgeschnittene Antwort weder Text, Tool-Calls noch Refusal enthält.
fn interpret_chat(
    body: &Value,
    provider: &str,
    model: &str,
    offered_tools: &[&str],
) -> Result<ModelResponse, ModelError> {
    let choice = body.pointer("/choices/0");
    let finish = choice
        .and_then(|choice| choice.get("finish_reason"))
        .and_then(Value::as_str);
    let raw_text = extract_chat_content(body);
    let refusal = choice
        .and_then(|choice| choice.pointer("/message/refusal"))
        .and_then(Value::as_str)
        .filter(|refusal| !refusal.is_empty())
        .map(str::to_owned);
    let truncated = matches!(finish, Some("length" | "content_filter"));
    let mut tool_calls = if truncated {
        let dropped = choice
            .and_then(|choice| choice.pointer("/message/tool_calls"))
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        if dropped > 0 {
            tracing::warn!(
                dropped,
                "dropping tool calls of a truncated chat completion"
            );
        }
        Vec::new()
    } else {
        extract_openai_tool_calls(body, Transport::Chat)
            .map_err(|error| ModelError::RequestFailed(error.to_string()))?
    };

    // Immer, günstig: einen führenden <think>-Block bzw. einen verwaisten
    // führenden </think>-Marker aus dem sichtbaren Text entfernen —
    // unabhängig davon, ob unten Text-Tool-Calls gefunden werden.
    let (mut text, think_text) = match raw_text {
        Some(raw) => {
            let (remaining, think) = text_tool_calls::strip_leading_think(&raw);
            let remaining = if remaining.is_empty() {
                None
            } else {
                Some(remaining)
            };
            (remaining, think)
        }
        None => (None, None),
    };

    // GLM-5.x/Kimi-Gateways senden Tool-Calls gelegentlich als Text in
    // `content` statt strukturiert in `tool_calls`. Nur aktiv, wenn
    // strukturierte Tool-Calls fehlen, der Request tatsächlich Tools
    // angeboten hat, und der (bereits think-bereinigte) Text einen
    // `<tool_call>`-Marker enthält — sonst bleibt die Antwort unverändert.
    if !truncated
        && tool_calls.is_empty()
        && !offered_tools.is_empty()
        && let Some(candidate) = text.as_deref()
        && candidate.contains("<tool_call>")
        && let Some((remaining, parsed)) =
            text_tool_calls::parse_text_tool_calls(candidate, offered_tools)
    {
        tracing::debug!(count = parsed.len(), "parsed text-embedded tool calls");
        tool_calls = parsed
            .into_iter()
            .enumerate()
            .map(|(index, call)| ToolCall {
                id: ToolCallId::from_str(format!("call_text_{index}")),
                name: ToolName::new(call.name),
                arguments: call.arguments,
            })
            .collect();
        text = if remaining.is_empty() {
            None
        } else {
            Some(remaining)
        };
    }

    let stop = match finish {
        Some("length") => StopReason::MaxTokens,
        Some("content_filter") => StopReason::ContentFilter,
        _ if !tool_calls.is_empty() => StopReason::ToolUse,
        _ if text.is_none() && refusal.is_some() => StopReason::Refusal {
            detail: refusal.clone(),
        },
        None | Some("stop" | "tool_calls" | "function_call") => StopReason::EndTurn,
        Some(other) => StopReason::Other(bounded_reason(other)),
    };
    if !truncated && text.is_none() && tool_calls.is_empty() && refusal.is_none() {
        return Err(ModelError::EmptyResponse);
    }
    let field_reasoning = choice
        .and_then(|choice| choice.pointer("/message/reasoning_content"))
        .and_then(Value::as_str)
        .filter(|content| !content.trim().is_empty());
    let reasoning = if let Some(reasoning_content) = field_reasoning {
        Some(OpaqueReasoning {
            provider: provider.to_owned(),
            model: model.to_owned(),
            blocks: vec![serde_json::json!({
                "type": "reasoning_content",
                "text": reasoning_content,
            })],
        })
    } else {
        think_text.map(|think_text| OpaqueReasoning {
            provider: provider.to_owned(),
            model: model.to_owned(),
            blocks: vec![serde_json::json!({
                "type": "think",
                "text": think_text,
            })],
        })
    };
    Ok(ModelResponse {
        message: text,
        tool_calls,
        usage: extract_openai_usage(body, Transport::Chat),
        stop,
        reasoning,
    })
}

/// Liest einen Header als eigenen String (für Werte, die `response.text()` überleben).
fn header_string(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Übersetzt das interne Reasoning-Effort-Level in den OpenAI-Wire-Wert für
/// `reasoning.effort` (Responses-API).
///
/// # Description
/// OpenAI unterstützt im Gegensatz zu Anthropic bereits ein eigenes
/// `"minimal"`-Level — daher 1:1-Mapping aller vier Werte, kein Zusammenlegen
/// wie beim Anthropic-Pendant.
///
/// # Arguments
/// - `effort` (`harw_types::ReasoningEffort`): internes Effort-Level.
///
/// # Returns
/// Den passenden OpenAI-Wire-String (`"minimal"`/`"low"`/`"medium"`/`"high"`).
fn map_effort_to_openai(effort: harw_types::ReasoningEffort) -> &'static str {
    match effort {
        harw_types::ReasoningEffort::Minimal => "minimal",
        harw_types::ReasoningEffort::Low => "low",
        harw_types::ReasoningEffort::Medium => "medium",
        harw_types::ReasoningEffort::High => "high",
        harw_types::ReasoningEffort::Xhigh => "xhigh",
        harw_types::ReasoningEffort::Max => "max",
    }
}

/// Extrahiert den Assistant-Text aus einer nicht-gestreamten Responses-Antwort.
///
/// Sammelt alle `output_text`-Parts der `message`-Items im `output`-Array.
fn extract_assistant_text(body: &Value) -> Option<String> {
    let output = body.get("output")?.as_array()?;
    let mut text = String::new();
    for item in output {
        if item.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let Some(parts) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for part in parts {
            if part.get("type").and_then(Value::as_str) == Some("output_text") {
                if let Some(chunk) = part.get("text").and_then(Value::as_str) {
                    text.push_str(chunk);
                }
            }
        }
    }
    if text.is_empty() { None } else { Some(text) }
}

/// Übersetzt die dem Modell angebotenen [`ToolSpec`]s in das
/// Chat-Completions-Wire-Format des `tools`-Arrays:
/// `{type:"function", function:{name, description, parameters, strict}}`.
fn build_chat_tools(tools: &[ToolSpec]) -> Vec<Value> {
    let names = tool_names::ToolNameCodec::for_tools(tools);
    tools
        .iter()
        .map(|spec| match spec {
            ToolSpec::Function(f) => serde_json::json!({
                "type": "function",
                "function": {
                    "name": names.encode(f.name.as_str()),
                    "description": f.description,
                    "parameters": strict_parameters(f),
                    "strict": f.strict,
                },
            }),
        })
        .collect()
}

/// Baut den `chat/completions`-Request-Body aus einem [`ModelRequest`].
///
/// # Description
/// Erzeugt das JSON `{model, messages: [{role, content}], tools?}`. Das erste
/// Element ist – sofern `system_prompt` (plus Instruction-Fragmente) nicht
/// leer ist – eine `system`-Nachricht; danach folgt der projizierte Verlauf:
/// - `User` → `role: "user"`.
/// - `Assistant` → `role: "assistant"`.
/// - `ToolCall` → `role: "assistant"` mit `tool_calls: [{id, type:"function",
///   function:{name, arguments}}]` (`arguments` als JSON-String — das
///   Chat-Completions-Wire-Format verlangt einen String, keinen
///   `serde_json::Value`).
/// - `ToolResult` → `role: "tool"` mit `tool_call_id` und dem Ergebnistext.
///
/// `request.tools` wird — sofern nicht leer — via [`build_chat_tools`] auf
/// `body["tools"]` abgebildet. Diese Funktion ist rein und I/O-frei, daher
/// direkt testbar.
///
/// # Arguments
/// - `request` (`&ModelRequest`): Quelle für System-Prompt, Fragmente, Verlauf und Tools.
/// - `model` (`&str`): Modellname für das `model`-Feld.
///
/// # Returns
/// Ein [`serde_json::Value`]-Objekt, direkt als Request-Body serialisierbar.
fn build_chat_body(request: &ModelRequest, model: &str) -> Value {
    let renderer = ToolResultRenderer::new(request);
    let names = tool_names::ToolNameCodec::for_request(request);
    let mut messages: Vec<Value> = Vec::new();

    let mut system = request.system_prompt.clone();
    for fragment in &request.instruction_fragments {
        system.push('\n');
        system.push_str(fragment);
    }
    if !system.is_empty() {
        messages.push(serde_json::json!({ "role": "system", "content": system }));
    }

    for message in request.history.to_model_messages() {
        match message {
            harw_core::ModelMessage::User { text } => {
                messages.push(serde_json::json!({ "role": "user", "content": text }));
            }
            harw_core::ModelMessage::Assistant { text } => {
                messages.push(serde_json::json!({ "role": "assistant", "content": text }));
            }
            harw_core::ModelMessage::ToolCall {
                call_id,
                name,
                arguments,
            } => {
                push_chat_tool_call(
                    &mut messages,
                    serde_json::json!({
                        "id": call_id.as_str(),
                        "type": "function",
                        "function": {
                            "name": names.encode(&name),
                            "arguments": arguments.to_string(),
                        },
                    }),
                );
            }
            harw_core::ModelMessage::ToolResult { call_id, result } => {
                let content = renderer.render(&call_id, &result);
                messages.push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": call_id.as_str(),
                    "content": content,
                }));
            }
        }
    }
    if let Some(data_block) = non_empty_data_block(request) {
        messages.push(serde_json::json!({ "role": "user", "content": data_block }));
    }

    let mut body = serde_json::json!({ "model": model, "messages": messages });
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(build_chat_tools(&request.tools));
    }
    if let Some(effort) = request.reasoning_effort {
        body["reasoning_effort"] = Value::from(map_effort_to_openai(effort));
    }
    if let Some(max_output_tokens) = request.max_output_tokens {
        body["max_completion_tokens"] = Value::from(max_output_tokens);
    }
    body
}

/// Hängt einen `tool_calls`-Eintrag an die laufende Assistant-Nachricht an oder
/// beginnt eine neue.
///
/// # Description
/// Beantwortet das Modell einen Turn mit mehreren Tool-Calls, stehen diese im
/// Verlauf als aufeinanderfolgende `ModelMessage::ToolCall`-Einträge, gefolgt
/// von allen Ergebnissen. Das Chat-Completions-Wire-Format verlangt dafür
/// **eine** Assistant-Nachricht mit mehreren `tool_calls`, unmittelbar gefolgt
/// von je einer `tool`-Nachricht. Eine eigene Assistant-Nachricht pro Call
/// würde der Provider mit HTTP 400 ablehnen („An assistant message with
/// 'tool_calls' must be followed by tool messages responding to each
/// 'tool_call_id'"). Diese Funktion fasst deshalb zusammen, was zum selben
/// Modell-Turn gehört; ein dazwischenliegendes Tool-Ergebnis oder ein
/// Textbeitrag beendet die Gruppe von selbst, weil die letzte Nachricht dann
/// kein `tool_calls`-Array mehr trägt.
///
/// # Arguments
/// - `messages` (`&mut Vec<Value>`): der bisher aufgebaute Nachrichtenverlauf.
/// - `entry` (`Value`): der `{id, type, function}`-Eintrag dieses Calls.
fn push_chat_tool_call(messages: &mut Vec<Value>, entry: Value) {
    let open_group = messages
        .last_mut()
        .and_then(|message| message.get_mut("tool_calls"))
        .and_then(Value::as_array_mut);
    match open_group {
        Some(calls) => calls.push(entry),
        None => messages.push(serde_json::json!({
            "role": "assistant",
            "content": Value::Null,
            "tool_calls": [entry],
        })),
    }
}

/// Extrahiert `choices[0].message.content` aus einer Chat-Completions-Antwort.
///
/// Liefert `None`, wenn das Feld fehlt, kein String ist oder leer ist.
fn extract_chat_content(body: &Value) -> Option<String> {
    let content = body
        .get("choices")?
        .as_array()?
        .first()?
        .get("message")?
        .get("content")?
        .as_str()?;
    if content.is_empty() {
        None
    } else {
        Some(content.to_owned())
    }
}

/// Extrahiert Tool-Calls aus einer nicht-gestreamten Responses-API-Antwort.
///
/// # Description
/// Durchsucht `output[]` nach Items vom Typ `function_call`
/// (`call_id`/`name`/`arguments` — `arguments` ist im Responses-Wire-Format
/// ein JSON-String und wird hier zurück in ein `serde_json::Value` geparst).
/// Fehlende oder nicht-stringförmige Kennungen bzw. Tool-Namen sowie fehlende
/// oder nicht parsebare Argumente werden fail-closed an den Aufrufer
/// zurückgegeben; sie dürfen nicht in einen anderen JSON-Wert umgedeutet
/// werden.
fn extract_responses_tool_calls(body: &Value) -> Result<Vec<ToolCall>, ToolCallExtractionError> {
    let Some(output) = body.get("output").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut calls = Vec::new();
    for item in output {
        if item.get("type").and_then(Value::as_str) == Some("function_call") {
            let call_id = item
                .get("call_id")
                .and_then(Value::as_str)
                .ok_or(ToolCallExtractionError::InvalidCallId)?;
            let name = item
                .get("name")
                .and_then(Value::as_str)
                .ok_or(ToolCallExtractionError::InvalidToolName)?;
            let arguments = parse_tool_call_arguments(item.get("arguments"))?;
            calls.push(ToolCall {
                id: ToolCallId::from_str(call_id),
                name: ToolName::new(name),
                arguments,
            });
        }
    }
    Ok(calls)
}

/// Extrahiert Tool-Calls aus einer nicht-gestreamten Chat-Completions-Antwort.
///
/// # Description
/// Liest `choices[0].message.tool_calls[]`
/// (`id`/`function.name`/`function.arguments` — `arguments` ist ein
/// JSON-String, geparst analog zu [`extract_responses_tool_calls`]). Fehlende
/// oder falsch typisierte `id`, `function` und `function.name`-Felder werden
/// fail-closed zurückgewiesen.
fn extract_chat_tool_calls(body: &Value) -> Result<Vec<ToolCall>, ToolCallExtractionError> {
    let Some(tool_calls) = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("tool_calls"))
        .and_then(Value::as_array)
    else {
        return Ok(Vec::new());
    };
    let mut calls = Vec::new();
    for call in tool_calls {
        let id = call
            .get("id")
            .and_then(Value::as_str)
            .ok_or(ToolCallExtractionError::InvalidCallId)?;
        let function = call
            .get("function")
            .and_then(Value::as_object)
            .ok_or(ToolCallExtractionError::InvalidFunction)?;
        let name = function
            .get("name")
            .and_then(Value::as_str)
            .ok_or(ToolCallExtractionError::InvalidToolName)?;
        let arguments = parse_tool_call_arguments(function.get("arguments"))?;
        calls.push(ToolCall {
            id: ToolCallId::from_str(id),
            name: ToolName::new(name),
            arguments,
        });
    }
    Ok(calls)
}

/// A provider supplied a tool call whose structure or argument payload cannot
/// be trusted.
///
/// This intentionally retains no provider-controlled string, so it is safe to
/// render at the model-provider boundary.
///
/// # Naming
/// Named per offending field (`InvalidCallId`/`InvalidFunction`/
/// `InvalidToolName`/`MalformedArguments`) rather than a uniform
/// `MissingOrInvalid*` scheme, so no single prefix or suffix is shared by
/// every variant (`clippy::enum_variant_names`). Each variant still covers
/// both "field absent" and "field has the wrong type/shape" — see
/// [`std::fmt::Display`] for the exact wording surfaced to callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolCallExtractionError {
    InvalidCallId,
    InvalidFunction,
    InvalidToolName,
    MalformedArguments,
}

impl std::fmt::Display for ToolCallExtractionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidCallId => {
                formatter.write_str("provider returned a tool call with missing or invalid call ID")
            }
            Self::InvalidFunction => formatter
                .write_str("provider returned a tool call with missing or invalid function"),
            Self::InvalidToolName => formatter
                .write_str("provider returned a tool call with missing or invalid tool name"),
            Self::MalformedArguments => formatter
                .write_str("provider returned a tool call with missing or invalid arguments"),
        }
    }
}

impl std::error::Error for ToolCallExtractionError {}

/// Parses a provider's tool-call argument string without retaining its value
/// on failure. Valid JSON values, including `{}`, are preserved unchanged.
fn parse_tool_call_arguments(value: Option<&Value>) -> Result<Value, ToolCallExtractionError> {
    let arguments = value
        .and_then(Value::as_str)
        .ok_or(ToolCallExtractionError::MalformedArguments)?;
    serde_json::from_str(arguments).map_err(|_| ToolCallExtractionError::MalformedArguments)
}

/// Extrahiert Tool-Calls aus einer OpenAI-Antwort, transport-abhängig.
///
/// # Description
/// Wählt zwischen [`extract_responses_tool_calls`] (Responses-API,
/// `output[]`) und [`extract_chat_tool_calls`] (Chat-Completions,
/// `choices[0].message.tool_calls[]`) anhand des [`Transport`].
///
/// # Arguments
/// - `body` (`&Value`): der geparste JSON-Antwortkörper.
/// - `transport` (`Transport`): wählt das Feldnamen-Schema der Quell-API.
///
/// # Returns
/// Einen `Vec<ToolCall>`, leer wenn keine Tool-Calls in der Antwort stehen.
/// Tool-Calls with missing or malformed arguments return an error instead.
fn extract_openai_tool_calls(
    body: &Value,
    transport: Transport,
) -> Result<Vec<ToolCall>, ToolCallExtractionError> {
    match transport {
        Transport::Responses => extract_responses_tool_calls(body),
        Transport::Chat => extract_chat_tool_calls(body),
    }
}

/// Extrahiert die Token-Nutzung aus einer OpenAI-Antwort in [`TokenUsage`].
///
/// # Description
/// Liest das `usage`-Objekt der Antwort und projiziert es transport-abhängig:
/// - [`Transport::Responses`]: `input_tokens`/`output_tokens` direkt am
///   `usage`-Objekt; `reasoning_tokens` verschachtelt unter
///   `output_tokens_details.reasoning_tokens`; `cached_tokens` verschachtelt
///   unter `input_tokens_details.cached_tokens`.
/// - [`Transport::Chat`]: `prompt_tokens`/`completion_tokens` als Input/Output;
///   `reasoning_tokens` verschachtelt unter
///   `completion_tokens_details.reasoning_tokens`; `cached_tokens`
///   verschachtelt unter `prompt_tokens_details.cached_tokens`.
///
/// Fehlt das gesamte `usage`-Objekt oder eines der Pflichtfelder
/// `input_tokens`/`output_tokens`, wird `0` angenommen (`.as_u64().unwrap_or(0)`).
/// Die optionalen Detail-Felder (`reasoning_tokens`, `cached_tokens`) bleiben
/// hingegen `None`, wenn der jeweilige Pfad fehlt — das unterscheidet
/// "nicht gemessen" (`None`) von "gemessen als 0" (`Some(0)`).
///
/// # Arguments
/// - `body` (`&Value`): der geparste JSON-Antwortkörper.
/// - `transport` (`Transport`): wählt das Feldnamen-Schema der Quell-API.
///
/// # Returns
/// Eine vollständig befüllte [`TokenUsage`]; nie ein Fehler.
fn extract_openai_usage(body: &Value, transport: Transport) -> TokenUsage {
    let usage = body.get("usage");
    let (input_key, output_key, reasoning_path, cached_path, cache_write_path) = match transport {
        Transport::Responses => (
            "input_tokens",
            "output_tokens",
            ["output_tokens_details", "reasoning_tokens"],
            ["input_tokens_details", "cached_tokens"],
            ["input_tokens_details", "cache_creation_input_tokens"],
        ),
        Transport::Chat => (
            "prompt_tokens",
            "completion_tokens",
            ["completion_tokens_details", "reasoning_tokens"],
            ["prompt_tokens_details", "cached_tokens"],
            ["prompt_tokens_details", "cache_creation_input_tokens"],
        ),
    };

    let input_tokens = usage
        .and_then(|value| value.get(input_key))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = usage
        .and_then(|value| value.get(output_key))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let reasoning_tokens = usage
        .and_then(|value| value.get(reasoning_path[0]))
        .and_then(|nested| nested.get(reasoning_path[1]))
        .and_then(Value::as_u64);
    let cached_tokens = usage
        .and_then(|value| value.get(cached_path[0]))
        .and_then(|nested| nested.get(cached_path[1]))
        .and_then(Value::as_u64);
    // DashScope liefert `cache_creation_input_tokens` unter
    // `prompt_tokens_details`; die Responses-API kennt das Feld nach
    // aktuellem Stand nicht (bleibt dann `None`, siehe Kontrakt).
    let cache_write_tokens = usage
        .and_then(|value| value.get(cache_write_path[0]))
        .and_then(|nested| nested.get(cache_write_path[1]))
        .and_then(Value::as_u64);

    TokenUsage {
        cache_separate: false,
        input_tokens,
        output_tokens,
        reasoning_tokens,
        cached_tokens,
        cache_write_tokens,
    }
}

/// Höchstlänge eines sanitierten Identity-Header-Werts (siehe
/// [`identity_headers`]).
const MAX_IDENTITY_HEADER_CHARS: usize = 64;

/// Reduziert `value` auf sichtbares, druckbares ASCII (`0x21..=0x7E`, also
/// ohne Leerzeichen und ohne Steuerzeichen) und kürzt danach auf höchstens
/// [`MAX_IDENTITY_HEADER_CHARS`] Zeichen.
///
/// Reine String-Hilfsfunktion für [`identity_headers`] — kein `HeaderValue`-
/// Aufbau hier, damit sie ohne `reqwest` isoliert testbar bleibt.
fn sanitize_identity_header_value(value: &str) -> String {
    value
        .chars()
        .filter(|c| matches!(*c as u32, 0x21..=0x7E))
        .take(MAX_IDENTITY_HEADER_CHARS)
        .collect()
}

/// Baut die optionalen Gateway-Identity-Header (`x-harw-session`,
/// `x-harw-agent`, `x-harw-role`) aus einer [`RequestIdentity`].
///
/// # Description
/// Nur für Provider mit aktiviertem
/// [`harw_config::ProviderToml::gateway_identity_headers`] relevant (siehe
/// [`OpenAiResponsesProvider::authorized_request`]). Jeder Wert wird über
/// [`sanitize_identity_header_value`] auf sichtbares ASCII reduziert und auf
/// 64 Zeichen gekürzt; wird der Wert dadurch leer oder lehnt
/// `HeaderValue::from_str` ihn danach trotzdem ab, wird genau dieser Header
/// übersprungen (`tracing::debug!`) statt den Request scheitern zu lassen —
/// die Identity-Header sind rein additiv und dürfen nie einen sonst gültigen
/// Request verhindern. `x-session-affinity` wird bewusst nie gesetzt; der
/// Cloudflare Worker leitet die Affinität selbst aus `session`+`agent` ab.
///
/// # Returns
/// Eine [`reqwest::header::HeaderMap`] mit 0 bis 3 Einträgen.
fn identity_headers(identity: &RequestIdentity) -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    for (name, raw_value) in [
        ("x-harw-session", identity.session.as_str()),
        ("x-harw-agent", identity.agent.as_str()),
        ("x-harw-role", identity.role.as_str()),
    ] {
        let sanitized = sanitize_identity_header_value(raw_value);
        if sanitized.is_empty() {
            tracing::debug!(header = name, "gateway_identity_headers.skip_empty_value");
            continue;
        }
        match reqwest::header::HeaderValue::from_str(&sanitized) {
            Ok(header_value) => {
                headers.insert(reqwest::header::HeaderName::from_static(name), header_value);
            }
            Err(_) => {
                tracing::debug!(header = name, "gateway_identity_headers.skip_invalid_value");
            }
        }
    }
    headers
}

impl OpenAiResponsesProvider {
    /// Baut den POST-Request mit konfigurierten Headern und Credential.
    ///
    /// Alle Credential-Header sind sensitiv markiert: `bearer_auth` tut das in
    /// reqwest selbst (`header_sensitive(.., true)`), `api-key`/`x-api-key`
    /// über [`sensitive_header_value`].
    ///
    /// `api_key` kommt vom Aufrufer (siehe [`Self::credential_for`]) statt
    /// immer `self.api_key` zu lesen, damit ein aktiver
    /// `credential_pool`-Eintrag denselben Header-Aufbau ohne Duplikation
    /// nutzen kann.
    ///
    /// `identity` ist [`ModelRequest::identity`] des aktuellen Requests. Ist
    /// [`Self::gateway_identity_headers`] `true` **und** `identity` `Some`,
    /// werden zusätzlich `x-harw-session`/`x-harw-agent`/`x-harw-role`
    /// gesetzt (siehe [`identity_headers`]) — aber nie mit einem Namen, den
    /// die statischen `[headers]` (`self.headers`) bereits belegen; die
    /// statische Konfiguration gewinnt immer. Die Codex-Route (`self.
    /// codex_route`) bleibt davon unberührt und kehrt unverändert vor dieser
    /// Ergänzung zurück.
    async fn authorized_request(
        &self,
        url: &str,
        api_key: &SecretString,
        identity: Option<&RequestIdentity>,
    ) -> Result<reqwest::RequestBuilder, ModelError> {
        let builder = self.client.post(url).headers(self.headers.clone());
        if let Some(route) = &self.codex_route {
            return Ok(builder.headers(route.headers(&self.client).await?));
        }
        let mut builder = match self.auth_header.as_str() {
            "none" => builder,
            "api-key" | "x-api-key" => builder.header(
                self.auth_header.as_str(),
                sensitive_header_value(api_key.expose_secret())?,
            ),
            _ => builder.bearer_auth(api_key.expose_secret()),
        };
        if self.gateway_identity_headers
            && let Some(identity) = identity
        {
            for (name, value) in identity_headers(identity).iter() {
                if !self.headers.contains_key(name) {
                    builder = builder.header(name, value.clone());
                }
            }
        }
        Ok(builder)
    }

    /// Liefert API-Key + Basis-URL für einen Versuch.
    ///
    /// # Description
    /// `credential_idx` wählt (falls `Some` und [`Self::credential_pool`]
    /// gesetzt) einen konkreten Pool-Eintrag; dessen `base_url`-Override
    /// gewinnt, wenn gesetzt, sonst bleibt `self.base_url` die Basis. Ohne
    /// Pool oder mit `credential_idx: None` liefert dies unverändert
    /// `self.api_key`/`self.base_url` — das bisherige Verhalten ohne
    /// Credential-Pool.
    fn credential_for(&self, credential_idx: Option<usize>) -> (&SecretString, &str) {
        match (credential_idx, &self.credential_pool) {
            (Some(index), Some(pool)) => {
                let entry = pool.entry(index);
                (
                    &entry.value,
                    entry.base_url.as_deref().unwrap_or(self.base_url.as_str()),
                )
            }
            _ => (&self.api_key, self.base_url.as_str()),
        }
    }

    /// Sendet **einen** Versuch mit dem durch `credential_idx` gewählten
    /// Credential. Der eigentliche Körper von [`ModelProvider::respond`] vor
    /// Einführung des Credential-Pools — unverändert bis auf die
    /// Credential-/Basis-URL-Auswahl über [`Self::credential_for`], damit
    /// [`ModelProvider::respond`] denselben Versuch mit einem anderen
    /// Pool-Eintrag wiederholen kann, ohne Wire-Aufbau, Gates oder
    /// Antwort-Interpretation zu duplizieren.
    ///
    /// # Errors
    /// Siehe [`ModelProvider::respond`].
    async fn respond_once(
        &self,
        request: ModelRequest,
        credential_idx: Option<usize>,
    ) -> Result<ModelResponse, ModelError> {
        let (api_key, base_url) = self.credential_for(credential_idx);
        let model = self.selected_model(&request)?;
        // Für `authorized_request` (Gateway-Identity-Header, siehe
        // [`Self::gateway_identity_headers`]) — geliehen von `request`, das
        // bis nach dem Response-Parsing im Scope bleibt.
        let identity = request.identity();
        let (url, mut wire) = match self.transport {
            Transport::Responses => {
                let replay = self
                    .reasoning_replay
                    .for_request(&self.provider_id, model, &request);
                tracing::debug!(
                    model,
                    replayed_reasoning_rounds = replay.len(),
                    "sending responses request"
                );
                (
                    format!("{base_url}/responses"),
                    build_responses_body(model, &request, &replay)?,
                )
            }
            Transport::Chat => {
                let strategy = cache_strategy::resolve_cache_strategy(
                    &self.provider_id,
                    model,
                    self.cache_overrides.get(model).copied(),
                );
                let mut body = build_chat_body(&request, model);
                cache_strategy::apply_chat_cache_control(&mut body, strategy);
                tracing::debug!(model, strategy = strategy.label(), "sending chat request");
                (format!("{base_url}/chat/completions"), body)
            }
        };

        if self.codex_route.is_some() {
            codex::prepare_body(&mut wire);
        }
        // Natives SSE-Streaming (Codex streamt ohnehin, siehe oben).
        let stream_sink = request
            .stream
            .as_ref()
            .filter(|_| self.codex_route.is_none() && self.stream_policy.enabled(model));
        if stream_sink.is_some() {
            wire["stream"] = Value::Bool(true);
            if self.transport == Transport::Chat {
                wire["stream_options"] = serde_json::json!({ "include_usage": true });
            }
        }

        // Hartes Nebenläufigkeits-Limit (siehe [`Self::concurrency_limiter`]):
        // blockiert, bis ein Slot frei wird, statt fehlzuschlagen. Der
        // Guard bleibt bis zum Ende dieses async-Blocks (also bis der
        // Response-Body vollständig gelesen/geparst ist) im Scope, damit
        // die Grenze wirklich in Flug befindliche Requests zählt, nicht
        // nur abgesetzte.
        let _concurrency_permit = match &self.concurrency_limiter {
            Some(limiter) => Some(limiter.acquire().await.map_err(|_| {
                ModelError::RequestFailed(
                    "internal error: provider concurrency semaphore was closed".to_owned(),
                )
            })?),
            None => None,
        };
        self.rate_limiter.wait_for_slot().await;
        // Codex-Route: bei einem tatsächlichen 401 genau einmal
        // reaktiv erneuern und den Request genau einmal wiederholen —
        // kein zweiter Refresh-Versuch nach erneutem 401 (siehe
        // `codex::CodexRoute::refresh`).
        let mut codex_refreshed_after_401 = false;
        let response = loop {
            let builder = self.authorized_request(&url, api_key, identity).await?;
            let response = builder
                .json(&wire)
                .timeout(self.request_timeout)
                .send()
                .await
                .map_err(|error| model_error_for_transport(error, false))?;

            if response.status() == reqwest::StatusCode::UNAUTHORIZED
                && !codex_refreshed_after_401
                && let Some(route) = &self.codex_route
            {
                codex_refreshed_after_401 = true;
                if route.refresh(&self.client).await.is_ok() {
                    continue;
                }
            }
            break response;
        };

        self.rate_limiter.observe_headers(response.headers());
        let status = response.status();
        let retry_after = header_string(response.headers(), "retry-after");
        let retry_after_ms = header_string(response.headers(), "retry-after-ms");
        let request_id = provider_request_id(response.headers());
        let value: Value = if status.is_success() && self.codex_route.is_some() {
            codex::read_response(response, request.stream.as_ref()).await?
        } else if status.is_success()
            && let Some(sink) = stream_sink
        {
            read_native_stream(response, self.transport, sink).await?
        } else {
            let body = response
                .text()
                .await
                .map_err(|error| model_error_for_transport(error, true))?;

            if !status.is_success() {
                if status.as_u16() == 429 {
                    // W6b — UIA-Sichtbarkeit: zählt jede beobachtete 429-Antwort
                    // dieses Providers, unabhängig davon, ob sie unten als
                    // `QuotaExceeded` oder `Transient{status: Some(429)}`
                    // übersetzt wird (siehe `rate_limiter::ProviderRateLimiter::
                    // record_rate_limited`).
                    self.rate_limiter.record_rate_limited();
                }
                let hint =
                    retry_after_hint(retry_after.as_deref(), retry_after_ms.as_deref(), &body);
                let error =
                    model_error_for_status(status.as_u16(), request_id.as_deref(), hint, &body);
                tracing::debug!(
                    status = status.as_u16(),
                    retryable = error.is_retryable(),
                    "provider returned an error status"
                );
                return Err(error);
            }

            serde_json::from_str(&body)?
        };
        let names = tool_names::ToolNameCodec::for_request(&request);
        let mut response = match self.transport {
            Transport::Responses => {
                let (response, replay) = interpret_responses(&value, &self.provider_id, model)?;
                if let Some(entry) = replay {
                    self.reasoning_replay.remember(entry);
                }
                response
            }
            Transport::Chat => {
                // Wire-kodierte Namen der für diesen Request angebotenen
                // Tools — exakt das, was das Modell im `tools`-Array
                // gesehen hat (siehe `build_chat_tools`). Grenze für
                // `text_tool_calls::parse_text_tool_calls` (text-
                // eingebettete Tool-Calls bei GLM-5.x/Kimi-Gateways).
                let offered_wire_names: Vec<String> = request
                    .tools
                    .iter()
                    .map(|spec| match spec {
                        ToolSpec::Function(function) => names.encode(function.name.as_str()),
                    })
                    .collect();
                let offered_tools: Vec<&str> =
                    offered_wire_names.iter().map(String::as_str).collect();
                interpret_chat(&value, &self.provider_id, model, &offered_tools)?
            }
        };
        for call in &mut response.tool_calls {
            call.name = ToolName::new(names.decode(call.name.as_str()));
        }
        Ok(response)
    }
}

/// Liest eine gestreamte Responses-/Chat-Antwort und rekonstruiert daraus den
/// nicht-gestreamten Body; Deltas gehen live an `sink`.
async fn read_native_stream(
    response: reqwest::Response,
    transport: Transport,
    sink: &harw_core::StreamSink,
) -> Result<Value, ModelError> {
    match transport {
        Transport::Chat => {
            let mut accumulator = sse::ChatStreamAccumulator::default();
            sse::read_sse(response, |frame| accumulator.push(&frame, Some(sink))).await?;
            accumulator.finish()
        }
        Transport::Responses => {
            let mut terminal = None;
            sse::read_sse(response, |frame| {
                if frame.data.trim().is_empty() || frame.data.trim() == "[DONE]" {
                    return Ok(false);
                }
                let event: Value = serde_json::from_str(&frame.data)?;
                terminal = sse::responses_event(&event, Some(sink))?;
                Ok(terminal.is_some())
            })
            .await?;
            terminal.ok_or_else(|| ModelError::Truncated {
                message: "response stream ended before its terminal event".into(),
            })
        }
    }
}

impl ProviderLoadControl for OpenAiResponsesProvider {
    fn provider_status(&self) -> ProviderLoadStatus {
        self.load_status()
    }

    fn set_max_concurrency(&self, target: Option<usize>) -> bool {
        match &self.concurrency_limiter {
            Some(limiter) => {
                limiter.set_target(target);
                true
            }
            None => false,
        }
    }
}

impl ModelProvider for OpenAiResponsesProvider {
    /// Sendet einen Modell-Request an diesen Provider und liefert die Antwort.
    ///
    /// # Description
    /// Delegiert den eigentlichen Versuch an [`Self::respond_once`]. Ist
    /// [`Self::credential_pool`] gesetzt (siehe Modul-Doku
    /// „Credential-Pool"), wird zuerst dessen bevorzugter, nicht
    /// abkühlender Eintrag gewählt. Schlägt dieser Versuch mit
    /// [`credential_pool::should_failover`] fehl (401/403 oder
    /// Kontingent-Erschöpfung) **und** hat der Pool mehr als einen Eintrag,
    /// wird der verwendete Eintrag für [`credential_pool::DEFAULT_COOLDOWN`]
    /// abgekühlt und derselbe Request **genau einmal** mit dem nächsten
    /// nicht abkühlenden Eintrag wiederholt — nie öfter. Ohne Pool bleibt
    /// das Verhalten unverändert: ein einziger Versuch mit `self.api_key`.
    ///
    /// # Errors
    /// - [`ModelError::RequestFailed`]: das Nebenläufigkeits-Semaphore wurde
    ///   geschlossen (interner Fehler), der HTTP-Request selbst schlug fehl,
    ///   oder die Antwort konnte nicht dekodiert werden.
    /// - [`ModelError::RateLimited`]: der Provider antwortete mit HTTP 429.
    /// - [`ModelError::EmptyResponse`]: der Provider lieferte keine nutzbare
    ///   Antwort.
    ///
    /// # Concurrency
    /// `Send + Sync`; sicher von mehreren Threads/Tasks gleichzeitig
    /// aufrufbar. Serialisiert zusätzliche Requests ausschließlich über
    /// `concurrency_limiter` (falls gesetzt), niemals über einen
    /// exklusiven Lock auf `self`.
    fn respond<'a>(&'a self, request: ModelRequest) -> ModelFuture<'a> {
        Box::pin(async move {
            let Some(pool) = self.credential_pool.as_ref() else {
                return self.respond_once(request, None).await;
            };
            let primary_idx = pool.select(None);
            match self.respond_once(request.clone(), primary_idx).await {
                Ok(response) => Ok(response),
                Err(error) => {
                    if pool.len() <= 1 || !credential_pool::should_failover(&error) {
                        return Err(error);
                    }
                    let Some(used_idx) = primary_idx else {
                        return Err(error);
                    };
                    pool.mark_cooldown(used_idx);
                    tracing::warn!(
                        provider = %self.provider_id,
                        credential_label = %pool.entry(used_idx).label,
                        error = %error,
                        "credential_pool.entry_cooldown"
                    );
                    match pool.select(Some(used_idx)) {
                        Some(next_idx) => {
                            tracing::info!(
                                provider = %self.provider_id,
                                credential_label = %pool.entry(next_idx).label,
                                "credential_pool.failover_retry"
                            );
                            self.respond_once(request, Some(next_idx)).await
                        }
                        None => Err(error),
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc::{self, Receiver};
    use std::thread;

    /// Wandelt einen fehlgeschlagenen Thread-Join (Panic im Hintergrund-Thread)
    /// in einen [`TestError`] um, statt ihn zu unwrappen.
    fn join_thread_error(_payload: Box<dyn std::any::Any + Send>) -> TestError {
        TestError::Unexpected("Hintergrund-Thread ist paniert".to_owned())
    }

    #[test]
    fn anthropic_subscription_token_warning_is_non_empty_and_cites_source() {
        assert!(!ANTHROPIC_SUBSCRIPTION_TOKEN_WARNING.trim().is_empty());
        assert!(
            ANTHROPIC_SUBSCRIPTION_TOKEN_WARNING
                .contains("https://code.claude.com/docs/en/legal-and-compliance")
        );
    }

    fn mock_chat_server(
        request_count: usize,
    ) -> TestResult<(String, Receiver<Value>, thread::JoinHandle<TestResult<()>>)> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(ctx("bind mock server"))?;
        let base_url = format!(
            "http://{}",
            listener.local_addr().map_err(ctx("mock address"))?
        );
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || -> TestResult<()> {
            for _ in 0..request_count {
                let (mut stream, _) = listener.accept().map_err(ctx("accept mock request"))?;
                let mut bytes = Vec::new();
                let mut buffer = [0_u8; 4096];
                let (header_end, content_length) = loop {
                    let read = stream.read(&mut buffer).map_err(ctx("read mock request"))?;
                    if read == 0 {
                        return Err(TestError::Unexpected(
                            "mock client closed connection before request completed".to_owned(),
                        ));
                    }
                    bytes.extend_from_slice(&buffer[..read]);
                    let Some(header_end) =
                        bytes.windows(4).position(|window| window == b"\r\n\r\n")
                    else {
                        continue;
                    };
                    let header_end = header_end + 4;
                    let headers = std::str::from_utf8(&bytes[..header_end])
                        .map_err(ctx("mock request headers are UTF-8"))?;
                    let content_length = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .ok_or(TestError::Missing("content length header"))?
                        .parse::<usize>()
                        .map_err(ctx("numeric content length"))?;
                    break (header_end, content_length);
                };
                while bytes.len() < header_end + content_length {
                    let read = stream
                        .read(&mut buffer)
                        .map_err(ctx("read mock request body"))?;
                    if read == 0 {
                        return Err(TestError::Unexpected(
                            "mock client closed connection before body completed".to_owned(),
                        ));
                    }
                    bytes.extend_from_slice(&buffer[..read]);
                }
                let body: Value =
                    serde_json::from_slice(&bytes[header_end..header_end + content_length])
                        .map_err(ctx("mock request JSON"))?;
                sender.send(body).map_err(ctx("report mock request"))?;
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 44\r\nconnection: close\r\n\r\n{\"choices\":[{\"message\":{\"content\":\"mock\"}}]}",
                    )
                    .map_err(ctx("write mock response"))?;
            }
            Ok(())
        });
        Ok((base_url, receiver, handle))
    }

    fn mock_json_response_server(
        response: Value,
    ) -> TestResult<(String, thread::JoinHandle<TestResult<()>>)> {
        let listener = TcpListener::bind("127.0.0.1:0").map_err(ctx("bind mock server"))?;
        let base_url = format!(
            "http://{}",
            listener.local_addr().map_err(ctx("mock address"))?
        );
        let response = serde_json::to_vec(&response).map_err(ctx("serialize mock response"))?;
        let handle = thread::spawn(move || -> TestResult<()> {
            let (mut stream, _) = listener.accept().map_err(ctx("accept mock request"))?;
            let mut request_buffer = [0_u8; 4096];
            // Der Mock verwirft die Anfrage bewusst — er antwortet immer
            // dasselbe. Gelesen werden muss trotzdem, sonst schließt der Server
            // die Verbindung, bevor der Client seine Anfrage losgeworden ist.
            // Die gelesene Menge wird ausdrücklich verworfen (`let _ =`) statt
            // ignoriert: `read` liefert bei einem Socket regelmäßig weniger als
            // den Puffer, und ein `read_exact` würde hier auf 4096 Bytes warten,
            // die nie kommen.
            let _read = stream
                .read(&mut request_buffer)
                .map_err(ctx("read mock request"))?;
            let headers = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                response.len()
            );
            stream
                .write_all(headers.as_bytes())
                .map_err(ctx("write response headers"))?;
            stream
                .write_all(&response)
                .map_err(ctx("write response body"))?;
            Ok(())
        });
        Ok((base_url, handle))
    }

    /// Startet einen Mock-HTTP-Server, der pro Verbindung einen eigenen
    /// Thread spawnt (statt seriell zu akzeptieren wie [`mock_chat_server`]),
    /// die aktuell gleichzeitig offenen Verbindungen zählt, den beobachteten
    /// Höchststand (`peak`) trackt und jede Anfrage künstlich verzögert,
    /// bevor sie beantwortet wird. Damit lässt sich beweisen, dass
    /// `max_concurrency` clientseitig wirklich gleichzeitige Requests
    /// begrenzt, statt nur Requests seriell abzuarbeiten.
    fn mock_concurrency_probe_server(
        request_count: usize,
    ) -> TestResult<(
        String,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        thread::JoinHandle<TestResult<()>>,
    )> {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let listener = TcpListener::bind("127.0.0.1:0").map_err(ctx("bind mock server"))?;
        let base_url = format!(
            "http://{}",
            listener.local_addr().map_err(ctx("mock address"))?
        );
        let in_flight = std::sync::Arc::new(AtomicUsize::new(0));
        let peak = std::sync::Arc::new(AtomicUsize::new(0));
        let peak_for_thread = std::sync::Arc::clone(&peak);
        let handle = thread::spawn(move || -> TestResult<()> {
            let mut connection_handles = Vec::with_capacity(request_count);
            for _ in 0..request_count {
                let (mut stream, _) = listener.accept().map_err(ctx("accept mock request"))?;
                let in_flight = std::sync::Arc::clone(&in_flight);
                let peak = std::sync::Arc::clone(&peak_for_thread);
                connection_handles.push(thread::spawn(move || -> TestResult<()> {
                    let mut request_buffer = [0_u8; 4096];
                    let _read = stream
                        .read(&mut request_buffer)
                        .map_err(ctx("read mock request"))?;
                    let current = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(current, Ordering::SeqCst);
                    // Künstliche Latenz, damit gleichzeitig eingehende
                    // Requests sich zeitlich überlappen können, sofern der
                    // Client sie überhaupt gleichzeitig absetzt.
                    thread::sleep(Duration::from_millis(80));
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                    let body = br#"{"choices":[{"message":{"content":"mock"}}]}"#;
                    let headers = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                        body.len()
                    );
                    stream
                        .write_all(headers.as_bytes())
                        .map_err(ctx("write mock response headers"))?;
                    stream
                        .write_all(body)
                        .map_err(ctx("write mock response body"))?;
                    Ok(())
                }));
            }
            for handle in connection_handles {
                handle.join().map_err(join_thread_error)??;
            }
            Ok(())
        });
        Ok((base_url, peak, handle))
    }

    #[tokio::test]
    async fn respond_honors_max_concurrency_hard_cap() -> TestResult {
        const MAX_CONCURRENCY: usize = 2;
        const REQUEST_COUNT: usize = 5;

        let (base_url, peak, server) = mock_concurrency_probe_server(REQUEST_COUNT)?;
        let mut provider =
            configured_provider("capped", base_url, vec!["gpt-test"], "CAPPED_PROVIDER_KEY");
        provider.max_concurrency = Some(MAX_CONCURRENCY);
        provider
            .validate()
            .map_err(ctx("max_concurrency = 2 is valid"))?;

        let env_layer =
            BTreeMap::from([("CAPPED_PROVIDER_KEY".to_owned(), "sk-secret".to_owned())]);
        let mut config = harw_config::ResolvedConfig::default();
        config.harness.default_provider = Some("capped".to_owned());
        config.harness.default_model = Some("gpt-test".to_owned());
        config
            .providers
            .insert("capped".to_owned(), provider.clone());

        let http_provider = std::sync::Arc::new(
            OpenAiResponsesProvider::from_named_config(
                "capped",
                &provider,
                &config,
                "gpt-test",
                test_sources(&env_layer, None, None),
            )
            .map_err(ctx("provider builds with max_concurrency configured"))?,
        );

        let mut handles = Vec::with_capacity(REQUEST_COUNT);
        for _ in 0..REQUEST_COUNT {
            let http_provider = std::sync::Arc::clone(&http_provider);
            handles.push(tokio::spawn(async move {
                http_provider
                    .respond(request_with_ids(None, None))
                    .await
                    .map_err(ctx("mock chat response parses"))
            }));
        }
        for handle in handles {
            handle.await.map_err(ctx("respond task completes"))??;
        }
        server.join().map_err(join_thread_error)??;

        assert!(
            peak.load(std::sync::atomic::Ordering::SeqCst) <= MAX_CONCURRENCY,
            "observed more than {MAX_CONCURRENCY} requests in flight simultaneously"
        );
        Ok(())
    }

    #[tokio::test]
    async fn dynamic_limiter_set_target_grows_immediately_without_waiting_for_existing_permits()
    -> TestResult {
        let limiter = std::sync::Arc::new(DynamicConcurrencyLimiter::new(Some(2)));
        let permit1 = limiter
            .acquire()
            .await
            .map_err(ctx("permit 1 acquires immediately"))?;
        let permit2 = limiter
            .acquire()
            .await
            .map_err(ctx("permit 2 acquires immediately"))?;
        assert_eq!(limiter.available(), 0);
        assert_eq!(limiter.target(), Some(2));

        // Grow while both original permits are still held.
        limiter.set_target(Some(4));
        assert_eq!(limiter.target(), Some(4));

        // The two new permits must be grantable right away — growth never
        // waits for the pre-existing in-flight permits to be released.
        let permit3 = tokio::time::timeout(Duration::from_millis(200), limiter.acquire())
            .await
            .map_err(ctx(
                "permit 3 must be available immediately after growing, without waiting",
            ))?
            .map_err(ctx("acquire succeeds"))?;
        let permit4 = tokio::time::timeout(Duration::from_millis(200), limiter.acquire())
            .await
            .map_err(ctx(
                "permit 4 must be available immediately after growing, without waiting",
            ))?
            .map_err(ctx("acquire succeeds"))?;

        assert_eq!(limiter.available(), 0);
        drop(permit1);
        drop(permit2);
        drop(permit3);
        drop(permit4);
        assert_eq!(
            limiter.available(),
            4,
            "all four permits must be free again after every guard dropped"
        );
        Ok(())
    }

    #[tokio::test]
    async fn dynamic_limiter_set_target_shrink_from_unbounded_shrinks_free_permits_immediately() {
        // The most important real-world case: a provider that started with
        // no configured max_concurrency (`None` -> `UNLIMITED_PERMITS`) gets
        // its target lowered live (e.g. UIA reacting to a 429). Since
        // nothing is in flight yet, every "excess" permit is free, so
        // `forget_permits` must remove it right away instead of only lazily
        // on the next release.
        let limiter = std::sync::Arc::new(DynamicConcurrencyLimiter::new(None));
        assert_eq!(limiter.target(), None);

        limiter.set_target(Some(2));
        assert_eq!(limiter.target(), Some(2));
        assert_eq!(
            limiter.available(),
            2,
            "shrinking an unbounded, idle limiter must apply immediately, not lazily"
        );
    }

    #[tokio::test]
    async fn dynamic_limiter_set_target_shrink_with_some_free_permits_forgets_only_the_free_ones()
    -> TestResult {
        // 1 of 4 permits busy, 3 free. Shrinking to 2 must immediately
        // forget exactly one of the three free permits (3 free - 1 needed
        // headroom for the still-busy permit = 2 to forget down to target),
        // leaving `available() == 1` right away; the remaining shrink (the
        // busy permit itself) only happens lazily once it is released.
        let limiter = std::sync::Arc::new(DynamicConcurrencyLimiter::new(Some(4)));
        let permit1 = limiter
            .acquire()
            .await
            .map_err(ctx("permit 1 acquires immediately"))?;
        assert_eq!(limiter.available(), 3);

        limiter.set_target(Some(2));
        assert_eq!(limiter.target(), Some(2));
        assert_eq!(
            limiter.available(),
            1,
            "the two truly free excess permits must be forgotten immediately"
        );

        drop(permit1);
        assert_eq!(
            limiter.available(),
            2,
            "releasing the last busy permit completes the lazy part of the shrink"
        );
        Ok(())
    }

    #[tokio::test]
    async fn dynamic_limiter_set_target_repeated_shrink_and_grow_stays_consistent() {
        let limiter = std::sync::Arc::new(DynamicConcurrencyLimiter::new(Some(8)));
        assert_eq!(limiter.available(), 8);

        limiter.set_target(Some(3));
        assert_eq!(limiter.available(), 3, "idle limiter shrinks immediately");

        limiter.set_target(Some(6));
        assert_eq!(limiter.available(), 6, "growing always applies immediately");

        limiter.set_target(Some(1));
        assert_eq!(
            limiter.available(),
            1,
            "shrinking again from an idle state is immediate"
        );

        limiter.set_target(None);
        assert_eq!(
            limiter.available(),
            UNLIMITED_PERMITS,
            "growing to unbounded restores the full sentinel capacity"
        );
        assert_eq!(limiter.target(), None);
    }

    #[tokio::test]
    async fn dynamic_limiter_set_target_shrink_waits_until_enough_permits_forgotten() -> TestResult
    {
        let limiter = std::sync::Arc::new(DynamicConcurrencyLimiter::new(Some(4)));
        let permit1 = limiter
            .acquire()
            .await
            .map_err(ctx("permit 1 acquires immediately"))?;
        let permit2 = limiter
            .acquire()
            .await
            .map_err(ctx("permit 2 acquires immediately"))?;
        let permit3 = limiter
            .acquire()
            .await
            .map_err(ctx("permit 3 acquires immediately"))?;
        let permit4 = limiter
            .acquire()
            .await
            .map_err(ctx("permit 4 acquires immediately"))?;
        assert_eq!(limiter.available(), 0);

        limiter.set_target(Some(2));
        assert_eq!(
            limiter.target(),
            Some(2),
            "target changes immediately even though capacity has not shrunk yet"
        );
        assert_eq!(
            limiter.available(),
            0,
            "shrinking must never revoke or wait on the four already in-flight permits"
        );

        let waiter_limiter = std::sync::Arc::clone(&limiter);
        let waiter = tokio::spawn(async move { waiter_limiter.acquire().await });

        // Let the waiter task run far enough to register as pending on the
        // semaphore before any permit is released.
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !waiter.is_finished(),
            "waiter must still be blocked before any of the four in-flight requests finished"
        );

        // All four original requests "finish" (return their permit) — none
        // of them was ever aborted. The first two returns are only enough to
        // shrink total capacity from 4 down to the new target of 2; they are
        // forgotten rather than handed to the waiter.
        drop(permit1);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !waiter.is_finished(),
            "waiter must still be blocked after only one permit was lazily forgotten"
        );
        drop(permit2);
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert!(
            !waiter.is_finished(),
            "waiter must still be blocked after only two permits were lazily forgotten \
             (capacity has now reached the shrunk target, but is still fully held)"
        );

        // The third return is a real release: capacity is already at target,
        // so this permit goes straight to the waiting acquirer.
        drop(permit3);
        let waiter_permit = tokio::time::timeout(Duration::from_millis(200), waiter)
            .await
            .map_err(ctx(
                "waiter must resolve once capacity has shrunk to the target",
            ))?
            .map_err(ctx("waiter task completes without panicking"))?
            .map_err(ctx("waiter acquires a permit"))?;

        // The fourth return is also a real release now.
        drop(permit4);
        assert!(
            limiter.available() <= 2,
            "available() must never exceed the shrunk target of 2"
        );
        assert_eq!(
            limiter.available(),
            1,
            "one free permit plus one held by the waiter"
        );
        drop(waiter_permit);
        assert_eq!(
            limiter.available(),
            2,
            "capacity has fully settled at the shrunk target once everything is released"
        );
        Ok(())
    }

    #[tokio::test]
    async fn respond_hard_cap_never_overshoots_while_shrinking_target() -> TestResult {
        const INITIAL_MAX_CONCURRENCY: usize = 4;
        const SHRUNK_TARGET: usize = 2;
        const REQUEST_COUNT: usize = 6;

        let (base_url, peak, server) = mock_concurrency_probe_server(REQUEST_COUNT)?;
        let mut provider = configured_provider(
            "elastic",
            base_url,
            vec!["gpt-test"],
            "ELASTIC_PROVIDER_KEY",
        );
        provider.max_concurrency = Some(INITIAL_MAX_CONCURRENCY);
        provider
            .validate()
            .map_err(ctx("max_concurrency = 4 is valid"))?;

        let env_layer =
            BTreeMap::from([("ELASTIC_PROVIDER_KEY".to_owned(), "sk-secret".to_owned())]);
        let mut config = harw_config::ResolvedConfig::default();
        config.harness.default_provider = Some("elastic".to_owned());
        config.harness.default_model = Some("gpt-test".to_owned());
        config
            .providers
            .insert("elastic".to_owned(), provider.clone());

        let http_provider = std::sync::Arc::new(
            OpenAiResponsesProvider::from_named_config(
                "elastic",
                &provider,
                &config,
                "gpt-test",
                test_sources(&env_layer, None, None),
            )
            .map_err(ctx("provider builds with max_concurrency configured"))?,
        );
        let limiter = http_provider
            .concurrency_limiter()
            .ok_or(TestError::Missing(
                "from_named_config always installs a limiter, even for a finite max_concurrency",
            ))?;
        assert_eq!(limiter.target(), Some(INITIAL_MAX_CONCURRENCY));

        let mut handles = Vec::with_capacity(REQUEST_COUNT);
        for _ in 0..REQUEST_COUNT {
            let http_provider = std::sync::Arc::clone(&http_provider);
            handles.push(tokio::spawn(async move {
                http_provider
                    .respond(request_with_ids(None, None))
                    .await
                    .map_err(ctx("mock chat response parses"))
            }));
        }

        // Shrink the target while the first wave of (up to) four requests is
        // still in flight. Shrinking must never let the number of
        // concurrently held permits exceed the original hard cap — it only
        // ever withholds *future* grants, it never revokes an already
        // in-flight request's permit.
        tokio::time::sleep(Duration::from_millis(20)).await;
        limiter.set_target(Some(SHRUNK_TARGET));

        for handle in handles {
            handle.await.map_err(ctx("respond task completes"))??;
        }
        server.join().map_err(join_thread_error)??;

        assert!(
            peak.load(std::sync::atomic::Ordering::SeqCst) <= INITIAL_MAX_CONCURRENCY,
            "observed more than {INITIAL_MAX_CONCURRENCY} requests in flight simultaneously, \
             even while the target was shrinking"
        );
        assert_eq!(
            limiter.available(),
            SHRUNK_TARGET,
            "capacity must have fully settled to the shrunk target once every request returned"
        );
        Ok(())
    }

    #[tokio::test]
    async fn dynamic_limiter_unbounded_never_waits() -> TestResult {
        let limiter = std::sync::Arc::new(DynamicConcurrencyLimiter::new(None));
        assert_eq!(
            limiter.target(),
            None,
            "None must mean unbounded, not a numeric cap"
        );

        // Acquiring far more permits than any realistic max_concurrency
        // would allow must never block, proving `None` behaves exactly as
        // before this feature existed (no client-side cap at all).
        let mut permits = Vec::with_capacity(1000);
        for _ in 0..1000 {
            let permit = tokio::time::timeout(Duration::from_millis(50), limiter.acquire())
                .await
                .map_err(ctx("unbounded limiter must never block on acquire"))?
                .map_err(ctx("acquire succeeds"))?;
            permits.push(permit);
        }
        drop(permits);
        Ok(())
    }

    #[tokio::test]
    async fn test_with_transport_never_installs_concurrency_limiter() -> TestResult {
        let provider = OpenAiResponsesProvider::with_transport(
            "http://127.0.0.1:0",
            "gpt-test",
            SecretString::new("sk-secret".into()),
            Transport::Chat,
        )
        .map_err(ctx("with_transport"))?;
        assert!(
            provider.concurrency_limiter.is_none(),
            "with_transport() must never configure a hard concurrency cap on its own"
        );

        // Confirm the absent limiter never blocks/panics on the respond() path:
        // a plain with_transport() provider must behave exactly as before this
        // feature existed.
        let (base_url, receiver, server) = mock_chat_server(1)?;
        let provider = OpenAiResponsesProvider::with_transport(
            base_url,
            "gpt-test",
            SecretString::new("sk-secret".into()),
            Transport::Chat,
        )
        .map_err(ctx("with_transport"))?;
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            provider.respond(request_with_ids(None, None)),
        )
        .await
        .map_err(ctx(
            "respond() must not block when no concurrency limiter is configured",
        ))?;
        assert!(response.is_ok(), "unexpected error: {response:?}");
        let _sent_body = receiver
            .recv()
            .map_err(ctx("mock server observed one request"))?;
        server.join().map_err(join_thread_error)??;
        Ok(())
    }

    #[tokio::test]
    async fn test_respond_releases_permit_after_transport_error() -> TestResult {
        // A listener that is bound and then immediately dropped frees the
        // port but leaves nothing accepting connections, so every request
        // against it fails fast with a connection-refused transport error.
        // This proves the concurrency permit acquired in `respond()` is
        // released on the error path (via normal Rust drop semantics on the
        // early `?` return), not just on the happy path: if it leaked, the
        // single-slot semaphore below would starve every request after the
        // first failure and the final successful request would deadlock.
        let unreachable_listener =
            TcpListener::bind("127.0.0.1:0").map_err(ctx("bind ephemeral port for closure"))?;
        let unreachable_url = format!(
            "http://{}",
            unreachable_listener
                .local_addr()
                .map_err(ctx("ephemeral address"))?
        );
        drop(unreachable_listener);

        let mut provider = OpenAiResponsesProvider::with_transport(
            unreachable_url,
            "gpt-test",
            SecretString::new("sk-secret".into()),
            Transport::Chat,
        )
        .map_err(ctx("with_transport"))?;
        provider.concurrency_limiter =
            Some(std::sync::Arc::new(DynamicConcurrencyLimiter::new(Some(1))));

        const FAILED_REQUESTS: usize = 3;
        for attempt in 0..FAILED_REQUESTS {
            let inner = tokio::time::timeout(
                Duration::from_secs(5),
                provider.respond(request_with_ids(None, None)),
            )
            .await
            .map_err(|_| {
                TestError::Unexpected(format!(
                    "attempt {attempt}: must not deadlock while acquiring the permit"
                ))
            })?;
            let Err(error) = inner else {
                return Err(TestError::Unexpected(format!(
                    "attempt {attempt}: connecting to a closed port must fail"
                )));
            };
            assert!(
                matches!(error, ModelError::Transient { .. }),
                "attempt {attempt}: expected a connect-transient error, got {error:?}"
            );
        }

        // The single permit must have been returned after each failure above;
        // a fresh request against a real mock server must still succeed
        // promptly instead of hanging on an exhausted semaphore.
        let (base_url, receiver, server) = mock_chat_server(1)?;
        provider.base_url = base_url;
        let response = tokio::time::timeout(
            Duration::from_secs(5),
            provider.respond(request_with_ids(None, None)),
        )
        .await
        .map_err(ctx(
            "permit must have been released by every prior failed request",
        ))?
        .map_err(ctx("mock chat response parses"))?;
        assert_eq!(response.message.as_deref(), Some("mock"));
        let _sent_body = receiver
            .recv()
            .map_err(ctx("mock server observed one request"))?;
        server.join().map_err(join_thread_error)??;
        Ok(())
    }

    async fn assert_respond_rejects_tool_call(
        transport: Transport,
        response: Value,
        expected_message: &str,
    ) -> TestResult {
        let (base_url, server) = mock_json_response_server(response)?;
        let provider = OpenAiResponsesProvider::with_transport(
            base_url,
            "gpt-test",
            SecretString::new("sk-secret".into()),
            transport,
        )
        .map_err(ctx("with_transport"))?;

        let Err(error) = provider.respond(request_with_ids(None, None)).await else {
            return Err(TestError::Unexpected(
                "malformed tool call must fail closed".to_owned(),
            ));
        };
        let ModelError::RequestFailed(message) = error else {
            return Err(TestError::Unexpected(
                "invalid tool-call arguments must be a request failure".to_owned(),
            ));
        };
        assert_eq!(message, expected_message);
        assert!(!message.contains("private-provider-arguments"));
        server.join().map_err(join_thread_error)??;
        Ok(())
    }

    fn configured_provider(
        name: &str,
        base_url: String,
        models: Vec<&str>,
        auth_env: &str,
    ) -> harw_config::ProviderToml {
        harw_config::ProviderToml {
            stream: None,
            name: name.to_owned(),
            api: "openai-chat".to_owned(),
            base_url,
            auth: Some(harw_config::SecretRef::Env(auth_env.to_owned())),
            auth_header: None,
            api_key: None,
            headers: HashMap::from([("x-provider-marker".to_owned(), name.to_owned())]),
            models: models.into_iter().map(str::to_owned).collect(),
            enabled: true,
            origin_allowlist: harw_config::OriginAllowlistToml::default(),
            rate_limit: None,
            max_concurrency: None,
            originator: None,
            default_reasoning_effort: None,
            gateway_identity_headers: false,
        }
    }

    fn test_sources<'a>(
        env_layer: &'a BTreeMap<String, String>,
        resolver: Option<&'a dyn SecretResolver>,
        home: Option<&'a Path>,
    ) -> SecretSources<'a> {
        SecretSources {
            env_layer,
            resolver,
            home,
            endpoint: None,
        }
    }

    fn no_process_env(_name: &str) -> Option<String> {
        None
    }

    struct FakeSecretResolver {
        result: Result<SecretString, String>,
    }

    impl SecretResolver for FakeSecretResolver {
        fn resolve(&self, _reference: &str) -> Result<SecretString, String> {
            match &self.result {
                Ok(secret) => Ok(SecretString::new(secret.expose_secret().to_owned().into())),
                Err(reason) => Err(reason.clone()),
            }
        }
    }

    fn secrets_provider_config(api: &str) -> harw_config::ProviderToml {
        let mut provider = configured_provider(
            "secrets-provider",
            "https://example.test/v1".to_owned(),
            vec!["model"],
            "unused",
        );
        provider.api = api.to_owned();
        provider.auth = Some(harw_config::SecretRef::Secrets(
            "tenant/provider-token".to_owned(),
        ));
        provider
    }

    fn secrets_config(api: &str) -> harw_config::ResolvedConfig {
        let mut config = harw_config::ResolvedConfig::default();
        config.harness.default_provider = Some("secrets-provider".to_owned());
        config.harness.default_model = Some("model".to_owned());
        config
            .providers
            .insert("secrets-provider".to_owned(), secrets_provider_config(api));
        config
    }

    #[test]
    fn injected_resolver_enables_secrets_references_for_openai_and_anthropic() -> TestResult {
        let resolver = FakeSecretResolver {
            result: Ok(SecretString::new("resolved-secret".into())),
        };

        let config = secrets_config("openai-chat");
        let provider = OpenAiResponsesProvider::from_config_with_resolver(&config, &resolver)
            .map_err(ctx(
                "injected resolver constructs OpenAI-compatible provider",
            ))?;
        assert_eq!(provider.api_key.expose_secret(), "resolved-secret");

        let anthropic = secrets_provider_config("anthropic-messages");
        let env_layer = BTreeMap::new();
        let (_, credential) = resolve_anthropic(
            "anthropic",
            &anthropic,
            test_sources(&env_layer, Some(&resolver), None),
            &no_process_env,
        )
        .map_err(ctx("injected resolver constructs Anthropic credential"))?;
        let AnthropicCredential::ApiKey(secret) = credential else {
            return Err(TestError::Unexpected(
                "ordinary resolved secret must be an Anthropic API key".to_owned(),
            ));
        };
        assert_eq!(secret.expose_secret(), "resolved-secret");

        build_provider_with_resolver(&config, &resolver).map_err(ctx(
            "injected resolver constructs configured provider router",
        ))?;
        Ok(())
    }

    #[test]
    fn resolve_provider_credential_returns_none_without_auth() -> TestResult {
        let mut provider = configured_provider(
            "openai-chat-no-auth",
            "https://example.test/v1".to_owned(),
            vec!["model"],
            "UNUSED_KEY",
        );
        provider.auth = None;
        let env_layer = BTreeMap::new();

        let credential = resolve_provider_credential(&provider, &env_layer, None, None)
            .map_err(ctx("provider without configured auth must still resolve"))?;
        assert!(credential.is_none());
        Ok(())
    }

    #[test]
    fn resolve_provider_credential_resolves_env_reference() -> TestResult {
        let provider = configured_provider(
            "openai-chat-provider",
            "https://example.test/v1".to_owned(),
            vec!["model"],
            "DOC_READ_PDF_PROVIDER_KEY",
        );
        let env_layer = BTreeMap::from([(
            "DOC_READ_PDF_PROVIDER_KEY".to_owned(),
            "sk-secret".to_owned(),
        )]);

        let credential = resolve_provider_credential(&provider, &env_layer, None, None)
            .map_err(ctx("env-layer credential must resolve"))?
            .ok_or(TestError::Missing("resolved provider credential"))?;
        assert_eq!(credential.expose_secret(), "sk-secret");
        Ok(())
    }

    #[test]
    fn build_provider_without_resolver_rejects_secrets_references() -> TestResult {
        let error = match build_provider(&secrets_config("openai-chat")) {
            Ok(_) => {
                return Err(TestError::Unexpected(
                    "secrets references remain fail-closed without a resolver".to_owned(),
                ));
            }
            Err(error) => error,
        };

        assert!(matches!(
            error,
            HttpProviderError::UnsupportedCredentialReference { .. }
        ));
        Ok(())
    }

    #[test]
    fn resolver_failure_diagnostic_does_not_include_secret_value() -> TestResult {
        let secret = "resolver-private-secret";
        let resolver = FakeSecretResolver {
            result: Err(format!("resolver unavailable: {secret}")),
        };
        let error = match build_provider_with_resolver(&secrets_config("openai-chat"), &resolver) {
            Ok(_) => {
                return Err(TestError::Unexpected(
                    "resolver failure must fail construction".to_owned(),
                ));
            }
            Err(error) => error,
        };

        assert!(matches!(
            &error,
            HttpProviderError::UnresolvedCredential { reason, .. }
                if reason == SECRET_RESOLVER_FAILURE_REASON
        ));
        assert!(!error.to_string().contains(secret));
        Ok(())
    }

    #[test]
    fn resolver_failure_diagnostic_is_redacted_for_anthropic() -> TestResult {
        let secret = "anthropic-resolver-private-secret";
        let resolver = FakeSecretResolver {
            result: Err(format!("resolver unavailable: {secret}")),
        };
        let provider = secrets_provider_config("anthropic-messages");
        let env_layer = BTreeMap::new();
        let error = match resolve_anthropic(
            "anthropic",
            &provider,
            test_sources(&env_layer, Some(&resolver), None),
            &no_process_env,
        ) {
            Ok(_) => {
                return Err(TestError::Unexpected(
                    "resolver failure must fail Anthropic construction".to_owned(),
                ));
            }
            Err(error) => error,
        };

        assert!(matches!(
            &error,
            HttpProviderError::UnresolvedCredential { reason, .. }
                if reason == SECRET_RESOLVER_FAILURE_REASON
        ));
        assert!(!error.to_string().contains(secret));
        Ok(())
    }

    #[test]
    fn empty_resolver_credentials_are_rejected_for_openai_and_anthropic() -> TestResult {
        for credential in ["", " \t\n"] {
            let resolver = FakeSecretResolver {
                result: Ok(SecretString::new(credential.to_owned().into())),
            };
            let config = secrets_config("openai-chat");
            let openai_error =
                match OpenAiResponsesProvider::from_config_with_resolver(&config, &resolver) {
                    Ok(_) => {
                        return Err(TestError::Unexpected(
                            "empty OpenAI resolver credential must fail construction".to_owned(),
                        ));
                    }
                    Err(error) => error,
                };
            assert!(matches!(
                openai_error,
                HttpProviderError::UnresolvedCredential { reason, .. }
                    if reason == EMPTY_CREDENTIAL_REASON
            ));

            let provider = secrets_provider_config("anthropic-messages");
            let env_layer = BTreeMap::new();
            let anthropic_error = match resolve_anthropic(
                "anthropic",
                &provider,
                test_sources(&env_layer, Some(&resolver), None),
                &no_process_env,
            ) {
                Ok(_) => {
                    return Err(TestError::Unexpected(
                        "empty Anthropic resolver credential must fail construction".to_owned(),
                    ));
                }
                Err(error) => error,
            };
            assert!(matches!(
                anthropic_error,
                HttpProviderError::UnresolvedCredential { reason, .. }
                    if reason == EMPTY_CREDENTIAL_REASON
            ));
        }
        Ok(())
    }

    #[tokio::test]
    async fn router_accepts_onboarded_secondary_provider_with_model_files() -> TestResult {
        let (url, requests, server) = mock_chat_server(1)?;
        let mut config = harw_config::ResolvedConfig::default();
        config.harness.default_provider = Some("primary".into());
        config.harness.default_model = Some("primary-model".into());
        for name in ["primary", "secondary"] {
            config.providers.insert(
                name.into(),
                configured_provider(name, url.clone(), vec![], "TEST_KEY"),
            );
        }
        config
            .env_layer
            .insert("TEST_KEY".into(), "test-key".into());
        for (id, owner) in [
            ("aaa-other", "primary"),
            ("z-model", "secondary"),
            ("a-model", "secondary"),
        ] {
            let model = serde_json::from_value(serde_json::json!({"id": id, "provider": owner}))
                .map_err(ctx("model onboarding JSON"))?;
            config.models.insert(id.into(), model);
        }
        let router = build_provider(&config).map_err(ctx("onboarding model files must suffice"))?;
        router
            .respond(request_with_ids(None, Some("secondary")))
            .await
            .map_err(ctx("respond succeeds"))?;
        let request = requests
            .recv()
            .map_err(ctx("mock server observed one request"))?;
        assert_eq!(
            request.get("model").and_then(Value::as_str),
            Some("a-model")
        );
        server.join().map_err(join_thread_error)??;
        Ok(())
    }

    #[tokio::test]
    async fn build_provider_routes_named_backends_to_distinct_endpoints_and_models() -> TestResult {
        let (primary_url, primary_requests, primary_server) = mock_chat_server(1)?;
        let (secondary_url, secondary_requests, secondary_server) = mock_chat_server(2)?;
        let mut config = harw_config::ResolvedConfig::default();
        config.harness.default_provider = Some("primary".to_owned());
        config.harness.default_model = Some("global-default-model".to_owned());
        config.providers.insert(
            "primary".to_owned(),
            configured_provider(
                "primary",
                primary_url,
                vec!["ignored-primary-fallback"],
                "PRIMARY_KEY",
            ),
        );
        config.providers.insert(
            "secondary".to_owned(),
            configured_provider(
                "secondary",
                secondary_url,
                vec!["secondary-fallback"],
                "SECONDARY_KEY",
            ),
        );
        config
            .env_layer
            .insert("PRIMARY_KEY".to_owned(), "primary-secret".to_owned());
        config
            .env_layer
            .insert("SECONDARY_KEY".to_owned(), "secondary-secret".to_owned());

        let provider = build_provider(&config).map_err(ctx("construct configured router"))?;
        provider
            .respond(request_with_ids(Some("primary-request-model"), None))
            .await
            .map_err(ctx("default provider response"))?;
        provider
            .respond(request_with_ids(None, Some("secondary")))
            .await
            .map_err(ctx("secondary fallback response"))?;
        provider
            .respond(request_with_ids(
                Some("secondary-request-model"),
                Some("secondary"),
            ))
            .await
            .map_err(ctx("secondary override response"))?;

        assert_eq!(
            primary_requests
                .recv()
                .map_err(ctx("primary request"))?
                .get("model")
                .and_then(Value::as_str),
            Some("primary-request-model")
        );
        assert_eq!(
            secondary_requests
                .recv()
                .map_err(ctx("secondary fallback request"))?
                .get("model")
                .and_then(Value::as_str),
            Some("secondary-fallback")
        );
        assert_eq!(
            secondary_requests
                .recv()
                .map_err(ctx("secondary override request"))?
                .get("model")
                .and_then(Value::as_str),
            Some("secondary-request-model")
        );
        primary_server.join().map_err(join_thread_error)??;
        secondary_server.join().map_err(join_thread_error)??;
        Ok(())
    }

    #[test]
    fn build_provider_with_load_registry_exposes_a_handle_per_openai_provider() -> TestResult {
        let mut config = harw_config::ResolvedConfig::default();
        config.harness.default_provider = Some("primary".to_owned());
        config.harness.default_model = Some("a-model".to_owned());
        config.providers.insert(
            "primary".to_owned(),
            configured_provider(
                "primary",
                "https://primary.example.com".to_owned(),
                vec!["a-model"],
                "PRIMARY_KEY",
            ),
        );
        config
            .env_layer
            .insert("PRIMARY_KEY".to_owned(), "primary-secret".to_owned());

        let (_provider, registry) = build_provider_with_load_registry(&config)
            .map_err(ctx("construct configured provider"))?;

        let handle = registry.get("primary").ok_or(TestError::Missing(
            "openai-compatible provider must expose a ProviderLoadControl handle",
        ))?;
        let status = handle.provider_status();
        assert_eq!(status.provider, "primary");
        assert_eq!(
            status.max_concurrency, None,
            "unconfigured max_concurrency stays unlimited"
        );
        assert_eq!(status.recent_rate_limited, 0, "no 429 observed yet");
        Ok(())
    }

    #[test]
    fn provider_load_control_set_max_concurrency_applies_immediately() -> TestResult {
        let mut config = harw_config::ResolvedConfig::default();
        config.harness.default_provider = Some("primary".to_owned());
        config.harness.default_model = Some("a-model".to_owned());
        config.providers.insert(
            "primary".to_owned(),
            configured_provider(
                "primary",
                "https://primary.example.com".to_owned(),
                vec!["a-model"],
                "PRIMARY_KEY",
            ),
        );
        config
            .env_layer
            .insert("PRIMARY_KEY".to_owned(), "primary-secret".to_owned());

        let (_provider, registry) = build_provider_with_load_registry(&config)
            .map_err(ctx("construct configured provider"))?;
        let handle = registry
            .get("primary")
            .ok_or(TestError::Missing("handle registered"))?;

        let applied = handle.set_max_concurrency(Some(2));
        assert!(applied, "OpenAI-compatible provider always has a limiter");
        let status = handle.provider_status();
        assert_eq!(status.max_concurrency, Some(2));
        assert_eq!(status.available_permits, 2);
        Ok(())
    }

    #[test]
    fn provider_load_registry_has_entry_for_anthropic_messages_provider() -> TestResult {
        let mut config = harw_config::ResolvedConfig::default();
        config.harness.default_provider = Some("claude".to_owned());
        config.harness.default_model = Some("claude-model".to_owned());
        let mut provider = configured_provider(
            "claude",
            "https://api.anthropic.com".to_owned(),
            vec!["claude-model"],
            "CLAUDE_KEY",
        );
        provider.api = "anthropic-messages".to_owned();
        config.providers.insert("claude".to_owned(), provider);
        config
            .env_layer
            .insert("CLAUDE_KEY".to_owned(), "claude-secret".to_owned());

        let (_provider, registry) = build_provider_with_load_registry(&config)
            .map_err(ctx("construct configured provider"))?;
        let handle = registry.get("claude").ok_or(TestError::Missing(
            "anthropic-messages provider now exposes a ProviderLoadControl handle",
        ))?;
        let status = handle.provider_status();
        assert_eq!(status.provider, "claude");
        assert_eq!(
            status.max_concurrency, None,
            "unconfigured max_concurrency stays unlimited"
        );
        assert_eq!(status.recent_rate_limited, 0, "no 429 observed yet");
        Ok(())
    }

    #[test]
    fn anthropic_provider_load_control_set_max_concurrency_applies_immediately() -> TestResult {
        let mut config = harw_config::ResolvedConfig::default();
        config.harness.default_provider = Some("claude".to_owned());
        config.harness.default_model = Some("claude-model".to_owned());
        let mut provider = configured_provider(
            "claude",
            "https://api.anthropic.com".to_owned(),
            vec!["claude-model"],
            "CLAUDE_KEY",
        );
        provider.api = "anthropic-messages".to_owned();
        config.providers.insert("claude".to_owned(), provider);
        config
            .env_layer
            .insert("CLAUDE_KEY".to_owned(), "claude-secret".to_owned());

        let (_provider, registry) = build_provider_with_load_registry(&config)
            .map_err(ctx("construct configured provider"))?;
        let handle = registry
            .get("claude")
            .ok_or(TestError::Missing("handle registered"))?;

        let applied = handle.set_max_concurrency(Some(2));
        assert!(applied, "Anthropic-Provider always has a limiter");
        let status = handle.provider_status();
        assert_eq!(status.max_concurrency, Some(2));
        assert_eq!(status.available_permits, 2);
        Ok(())
    }

    fn request_with_ids(model_id: Option<&str>, provider_id: Option<&str>) -> ModelRequest {
        ModelRequest {
            stream: None,
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history: harw_core::ConversationHistory::new(),
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: model_id.map(Into::into),
            provider_id: provider_id.map(Into::into),
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
            cancel: None,
            identity: None,
        }
    }

    // ── parse_retry_after ────────────────────────────────────────────────────

    #[test]
    fn test_parse_retry_after_numeric_header_wins() {
        let duration = parse_retry_after(Some("42"), "some body text");
        assert_eq!(duration.as_secs(), 42);
    }

    #[test]
    fn test_parse_retry_after_body_wait_n_seconds_extracted() {
        let body = r#"{"error":{"message":"Please wait 33 seconds before retrying."}}"#;
        let duration = parse_retry_after(None, body);
        assert_eq!(duration.as_secs(), 33);
    }

    #[test]
    fn test_parse_retry_after_body_case_insensitive() {
        let body = "Rate limit exceeded. WAIT 17 seconds.";
        let duration = parse_retry_after(None, body);
        assert_eq!(duration.as_secs(), 17);
    }

    #[test]
    fn test_parse_retry_after_fallback_30s_when_no_hint() {
        let duration = parse_retry_after(None, "no useful information here");
        assert_eq!(duration.as_secs(), 30);
    }

    #[test]
    fn test_parse_retry_after_non_numeric_header_falls_back_to_body() {
        // HTTP-Date format header → ignored, body extracted instead.
        let body = "Please wait 10 seconds.";
        let duration = parse_retry_after(Some("Tue, 16 Jul 2026 12:00:00 GMT"), body);
        assert_eq!(duration.as_secs(), 10);
    }

    #[test]
    fn test_parse_retry_after_real_anthropic_body() {
        let body = r#"{"error":{"code":"RateLimitReached","message":"Rate limit of 8000 per 60s exceeded for UserByModelByMinuteOutputTokens. Please wait 33 seconds before retrying.","details":"Rate limit of 8000 per 60s exceeded for UserByModelByMinuteOutputTokens. Please wait 33 seconds before retrying."}}"#;
        let duration = parse_retry_after(None, body);
        assert_eq!(duration.as_secs(), 33);
    }

    #[test]
    fn test_new_sets_fields() -> TestResult {
        let provider = OpenAiResponsesProvider::new(
            "https://example.test/v1",
            "gpt-test",
            SecretString::new("sk-secret".into()),
        )
        .map_err(ctx("OpenAiResponsesProvider::new"))?;
        assert_eq!(provider.base_url, "https://example.test/v1");
        assert_eq!(provider.model, "gpt-test");
        assert_eq!(provider.api_key.expose_secret(), "sk-secret");
        assert_eq!(provider.request_timeout, DEFAULT_REQUEST_TIMEOUT);
        Ok(())
    }

    #[test]
    fn provider_load_status_reflects_recorded_rate_limits_without_a_concurrency_limiter()
    -> TestResult {
        // `OpenAiResponsesProvider::new` (unlike `from_named_config`) never
        // installs a `DynamicConcurrencyLimiter` — `load_status`/
        // `ProviderLoadControl::set_max_concurrency` must degrade cleanly:
        // `available_permits == usize::MAX`, `set_max_concurrency` returns
        // `false`, but the 429 counter still works (it lives on the
        // always-present `ProviderRateLimiter`, not on the limiter).
        let provider = OpenAiResponsesProvider::new(
            "https://example.test/v1",
            "gpt-test",
            SecretString::new("sk-secret".into()),
        )
        .map_err(ctx("OpenAiResponsesProvider::new"))?;
        let status = provider.load_status();
        assert_eq!(status.max_concurrency, None);
        assert_eq!(status.available_permits, usize::MAX);
        assert_eq!(status.recent_rate_limited, 0);

        provider.rate_limiter_handle().record_rate_limited();
        provider.rate_limiter_handle().record_rate_limited();
        let status = ProviderLoadControl::provider_status(&provider);
        assert_eq!(status.recent_rate_limited, 2);

        assert!(
            !ProviderLoadControl::set_max_concurrency(&provider, Some(1)),
            "no limiter installed via ::new — must not silently succeed"
        );
        Ok(())
    }

    #[test]
    fn test_sanitized_provider_error_uses_only_bounded_structured_message() {
        let private_body = format!(
            r#"{{"error":{{"message":"{}","prompt":"must not escape"}}}}"#,
            "x".repeat(532)
        );

        let error = sanitized_provider_error(503, None, &private_body);

        assert_eq!(error.len(), "provider returned HTTP 503: ".len() + 512);
        assert!(!error.contains("must not escape"));
    }

    #[test]
    fn test_sanitized_provider_error_rejects_unstructured_body() {
        assert_eq!(
            sanitized_provider_error(502, None, "<html>private gateway response</html>"),
            "provider returned HTTP 502"
        );
    }

    #[test]
    fn test_parse_keyring_reference_accepts_exact_service_account_form() {
        assert_eq!(
            parse_keyring_reference("harwness/openai"),
            Some(("harwness", "openai"))
        );
    }

    #[test]
    fn test_parse_keyring_reference_rejects_invalid_forms() {
        for payload in [
            "",
            "service",
            "/account",
            "service/",
            "service/account/extra",
        ] {
            assert_eq!(parse_keyring_reference(payload), None, "{payload:?}");
        }
    }

    #[test]
    fn test_read_external_cli_credential_none_endpoint_returns_none() {
        // Ohne Endpoint darf niemals eine externe CLI-Credential-Datei
        // gelesen werden, unabhängig von Pfad/Pointer.
        assert!(read_external_cli_credential(None, "/anything", "/anything").is_none());
    }

    #[test]
    fn test_read_external_cli_credential_non_official_host_returns_none() -> TestResult {
        let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) else {
            // Ohne HOME kann der Allowlist-Pfad nicht gebildet werden; das ist
            // hier irrelevant, weil der Host schon vorher ablehnt, aber die
            // Funktion selbst liest HOME zuerst.
            return Ok(());
        };
        let path = PathBuf::from(home).join(".codex/auth.json");
        let raw_path = path.to_str().ok_or(TestError::Missing("utf8 test path"))?;

        let result = read_external_cli_credential(
            Some("https://evil.example/v1"),
            raw_path,
            "/tokens/access_token",
        );

        assert!(result.is_none());
        Ok(())
    }

    #[test]
    fn test_read_external_cli_credential_wrong_pointer_returns_none() -> TestResult {
        let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) else {
            return Ok(());
        };
        let path = PathBuf::from(home).join(".codex/auth.json");
        let raw_path = path.to_str().ok_or(TestError::Missing("utf8 test path"))?;

        let result = read_external_cli_credential(
            Some("https://api.openai.com/v1"),
            raw_path,
            "/not/an/allowlisted/pointer",
        );

        assert!(result.is_none());
        Ok(())
    }

    #[test]
    fn test_codex_chatgpt_access_token_cannot_be_used_for_openai_api() -> TestResult {
        let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) else {
            return Ok(());
        };
        let path = PathBuf::from(home).join(".codex/auth.json");
        let raw_path = path.to_str().ok_or(TestError::Missing("utf8 test path"))?;

        let result = read_external_cli_credential(
            Some("https://api.openai.com/v1"),
            raw_path,
            "/tokens/access_token",
        );

        assert!(result.is_none());
        Ok(())
    }

    #[test]
    fn test_read_external_cli_credential_wrong_path_returns_none() -> TestResult {
        let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) else {
            return Ok(());
        };
        let path = PathBuf::from(home).join("not-an-allowlisted-file.json");
        let raw_path = path.to_str().ok_or(TestError::Missing("utf8 test path"))?;

        let result = read_external_cli_credential(
            Some("https://api.openai.com/v1"),
            raw_path,
            "/tokens/access_token",
        );

        assert!(result.is_none());
        Ok(())
    }

    #[test]
    fn test_resolve_secret_rejects_secrets_reference_without_disclosing_it() -> TestResult {
        let reference = "tenant/provider-token-with-secret-metadata";
        let env_layer = BTreeMap::new();
        let Err(error) = resolve_secret(
            &harw_config::SecretRef::Secrets(reference.to_owned()),
            test_sources(&env_layer, None, None),
        ) else {
            return Err(TestError::Unexpected(
                "secrets references are unsupported by the HTTP provider".to_owned(),
            ));
        };

        assert!(matches!(
            &error,
            HttpProviderError::UnsupportedCredentialReference { .. }
        ));
        assert!(!error.to_string().contains(reference));
        Ok(())
    }

    /// Fake-Resolver, der alle angefragten Bezeichner mitschreibt.
    struct RecordingSecretResolver {
        value: &'static str,
        seen: std::cell::RefCell<Vec<String>>,
    }

    impl SecretResolver for RecordingSecretResolver {
        fn resolve(&self, reference: &str) -> Result<SecretString, String> {
            self.seen.borrow_mut().push(reference.to_owned());
            Ok(SecretString::new(self.value.to_owned().into()))
        }
    }

    #[test]
    fn test_resolve_secret_routes_secrets_reference_to_resolver_without_prefix() -> TestResult {
        let resolver = RecordingSecretResolver {
            value: "resolved-via-fake",
            seen: std::cell::RefCell::new(Vec::new()),
        };
        let env_layer = BTreeMap::new();

        let secret = resolve_secret(
            &harw_config::SecretRef::Secrets("tenant/provider-token".to_owned()),
            test_sources(&env_layer, Some(&resolver), None),
        )
        .map_err(ctx("secrets reference resolves through injected resolver"))?;

        assert_eq!(secret.expose_secret(), "resolved-via-fake");
        assert_eq!(
            *resolver.seen.borrow(),
            vec!["tenant/provider-token".to_owned()]
        );
        Ok(())
    }

    #[test]
    fn test_resolve_secret_rejects_invalid_keyring_reference_without_resolver_call() -> TestResult {
        let resolver = RecordingSecretResolver {
            value: "must-not-be-used",
            seen: std::cell::RefCell::new(Vec::new()),
        };
        let env_layer = BTreeMap::new();

        for payload in ["service", "service/", "/account", "a/b/c"] {
            let Err(error) = resolve_secret(
                &harw_config::SecretRef::Keyring(payload.to_owned()),
                test_sources(&env_layer, Some(&resolver), None),
            ) else {
                return Err(TestError::Unexpected(format!(
                    "invalid keyring reference {payload:?} must fail"
                )));
            };
            assert!(
                matches!(
                    &error,
                    HttpProviderError::UnresolvedCredential { reason, .. }
                        if reason == INVALID_KEYRING_REFERENCE_REASON
                ),
                "{payload:?}"
            );
        }
        assert!(resolver.seen.borrow().is_empty());
        Ok(())
    }

    #[test]
    fn test_resolve_secret_missing_keyring_entry_is_redacted_and_skips_resolver() -> TestResult {
        // Mock-Backend statt echtem System-Keyring: jede neue `Entry` ist leer,
        // `get_password` liefert daher `NoEntry`.
        keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        let resolver = RecordingSecretResolver {
            value: "must-not-be-used",
            seen: std::cell::RefCell::new(Vec::new()),
        };
        let env_layer = BTreeMap::new();

        let Err(error) = resolve_secret(
            &harw_config::SecretRef::Keyring("harwness/openai".to_owned()),
            test_sources(&env_layer, Some(&resolver), None),
        ) else {
            return Err(TestError::Unexpected(
                "missing keyring entry must fail".to_owned(),
            ));
        };

        assert!(matches!(
            &error,
            HttpProviderError::UnresolvedCredential { reason, .. }
                if reason == KEYRING_FAILURE_REASON
        ));
        assert!(!error.to_string().contains("must-not-be-used"));
        assert!(resolver.seen.borrow().is_empty());
        Ok(())
    }

    #[test]
    fn test_sanitized_provider_error_preserves_status_and_request_id() {
        let error = sanitized_provider_error(
            503,
            Some("req_123"),
            r#"{"error":{"message":"upstream unavailable\nretry shortly"}}"#,
        );

        assert_eq!(
            error,
            "provider returned HTTP 503 (request_id: req_123): upstream unavailable retry shortly"
        );
    }

    #[test]
    fn test_provider_request_id_prefers_openai_header_and_bounds_value() -> TestResult {
        let mut headers = reqwest::header::HeaderMap::new();
        let long_request_id = "r".repeat(140);
        headers.insert(
            "x-request-id",
            long_request_id.parse().map_err(ctx("valid header value"))?,
        );
        headers.insert(
            "request-id",
            reqwest::header::HeaderValue::from_static("fallback"),
        );

        let request_id = provider_request_id(&headers).ok_or(TestError::Missing("request ID"))?;

        assert_eq!(request_id.len(), 128);
        assert!(request_id.chars().all(|character| character == 'r'));
        Ok(())
    }

    #[test]
    fn test_extract_assistant_text_joins_output_parts() {
        let body = serde_json::json!({
            "output": [
                { "type": "reasoning" },
                {
                    "type": "message",
                    "content": [
                        { "type": "output_text", "text": "Hello, " },
                        { "type": "output_text", "text": "world" }
                    ]
                }
            ]
        });
        assert_eq!(
            extract_assistant_text(&body).as_deref(),
            Some("Hello, world")
        );
    }

    #[test]
    fn test_extract_assistant_text_none_when_empty() {
        let body = serde_json::json!({ "output": [] });
        assert_eq!(extract_assistant_text(&body), None);
    }

    #[test]
    fn test_transport_from_api_responses() {
        assert_eq!(transport_from_api("openai-responses"), Transport::Responses);
    }

    #[test]
    fn test_transport_from_api_defaults_to_chat() {
        assert_eq!(transport_from_api("openai-chat"), Transport::Chat);
        assert_eq!(transport_from_api("anthropic-messages"), Transport::Chat);
        assert_eq!(transport_from_api(""), Transport::Chat);
    }

    #[test]
    fn test_new_defaults_to_responses_transport() -> TestResult {
        let provider = OpenAiResponsesProvider::new(
            "https://example.test/v1",
            "gpt-test",
            SecretString::new("sk-secret".into()),
        )
        .map_err(ctx("OpenAiResponsesProvider::new"))?;
        assert_eq!(provider.transport, Transport::Responses);
        Ok(())
    }

    #[test]
    fn test_selected_model_uses_requested_model_for_compatible_provider() -> TestResult {
        let provider = OpenAiResponsesProvider::new(
            "https://example.test/v1",
            "configured-model",
            SecretString::new("sk-secret".into()),
        )
        .map_err(ctx("OpenAiResponsesProvider::new"))?;
        let request = request_with_ids(Some("requested-model"), Some("openai"));

        assert_eq!(
            provider
                .selected_model(&request)
                .map_err(ctx("selected_model succeeds"))?,
            "requested-model"
        );
        Ok(())
    }

    #[test]
    fn test_selected_model_preserves_configured_default_for_empty_identifiers() -> TestResult {
        let provider = OpenAiResponsesProvider::new(
            "https://example.test/v1",
            "configured-model",
            SecretString::new("sk-secret".into()),
        )
        .map_err(ctx("OpenAiResponsesProvider::new"))?;
        let request = request_with_ids(Some(""), Some(""));

        assert_eq!(
            provider
                .selected_model(&request)
                .map_err(ctx("selected_model succeeds"))?,
            "configured-model"
        );
        Ok(())
    }

    #[test]
    fn test_selected_model_rejects_provider_mismatch() -> TestResult {
        let provider = OpenAiResponsesProvider::new(
            "https://example.test/v1",
            "configured-model",
            SecretString::new("sk-secret".into()),
        )
        .map_err(ctx("OpenAiResponsesProvider::new"))?;
        let request = request_with_ids(Some("requested-model"), Some("anthropic"));

        assert!(matches!(
            provider.selected_model(&request),
            Err(ModelError::RequestFailed(message))
                if message.contains("anthropic") && message.contains("openai")
        ));
        Ok(())
    }

    #[tokio::test]
    async fn test_respond_rejects_provider_mismatch_before_http_request() -> TestResult {
        let provider = OpenAiResponsesProvider::new(
            "http://127.0.0.1:1/v1",
            "configured-model",
            SecretString::new("sk-secret".into()),
        )
        .map_err(ctx("OpenAiResponsesProvider::new"))?;

        let Err(error) = provider
            .respond(request_with_ids(Some("requested-model"), Some("anthropic")))
            .await
        else {
            return Err(TestError::Unexpected(
                "a request for another provider must fail before HTTP".to_owned(),
            ));
        };

        assert!(matches!(
            error,
            ModelError::RequestFailed(message)
                if message.contains("anthropic") && message.contains("openai")
        ));
        Ok(())
    }

    #[test]
    fn test_build_chat_body_shape() -> TestResult {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("Hi there");
        let request = ModelRequest {
            stream: None,
            system_prompt: "You are terse.".to_owned(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
            cancel: None,
            identity: None,
        };

        let body = build_chat_body(&request, "gpt-4o-mini");
        assert_eq!(
            body.get("model").and_then(Value::as_str),
            Some("gpt-4o-mini")
        );
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("messages array"))?;
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[0].get("role").and_then(Value::as_str),
            Some("system")
        );
        assert_eq!(
            messages[0].get("content").and_then(Value::as_str),
            Some("You are terse.")
        );
        assert_eq!(
            messages[1].get("role").and_then(Value::as_str),
            Some("user")
        );
        assert_eq!(
            messages[1].get("content").and_then(Value::as_str),
            Some("Hi there")
        );
        Ok(())
    }

    #[test]
    fn test_build_chat_body_omits_empty_system() -> TestResult {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("Only user");
        let request = ModelRequest {
            stream: None,
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
            cancel: None,
            identity: None,
        };

        let body = build_chat_body(&request, "m");
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("messages array"))?;
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].get("role").and_then(Value::as_str),
            Some("user")
        );
        Ok(())
    }

    #[test]
    fn test_extract_chat_content_reads_first_choice() {
        let body = serde_json::json!({
            "choices": [
                { "message": { "role": "assistant", "content": "Hello world" } }
            ]
        });
        assert_eq!(extract_chat_content(&body).as_deref(), Some("Hello world"));
    }

    #[test]
    fn test_interpret_chat_parses_reasoning_content_into_opaque_reasoning() -> TestResult {
        let body = serde_json::json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "Hello world",
                    "reasoning_content": "Let me think about this step by step."
                }
            }]
        });
        let response = interpret_chat(&body, "dashscope", "qwen-thinking", &[])
            .map_err(ctx("interpret_chat must succeed"))?;
        let reasoning = response
            .reasoning
            .ok_or(TestError::Missing("reasoning must be present"))?;
        assert_eq!(reasoning.provider, "dashscope");
        assert_eq!(reasoning.model, "qwen-thinking");
        assert_eq!(
            reasoning.blocks,
            vec![serde_json::json!({
                "type": "reasoning_content",
                "text": "Let me think about this step by step."
            })]
        );
        Ok(())
    }

    #[test]
    fn test_interpret_chat_reasoning_none_when_field_absent() -> TestResult {
        let body = serde_json::json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "Hello world"
                }
            }]
        });
        let response = interpret_chat(&body, "dashscope", "qwen-plain", &[])
            .map_err(ctx("interpret_chat must succeed"))?;
        assert!(response.reasoning.is_none());
        Ok(())
    }

    #[test]
    fn test_interpret_chat_reasoning_none_when_field_blank() -> TestResult {
        let body = serde_json::json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "Hello world",
                    "reasoning_content": "   "
                }
            }]
        });
        let response = interpret_chat(&body, "dashscope", "qwen-plain", &[])
            .map_err(ctx("interpret_chat must succeed"))?;
        assert!(response.reasoning.is_none());
        Ok(())
    }

    #[test]
    fn test_interpret_chat_recovers_text_embedded_tool_call_when_offered() -> TestResult {
        let body = serde_json::json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "<tool_call>fs.read{\"path\":\"harw-cli/src/main.rs\"}"
                }
            }]
        });
        let response = interpret_chat(&body, "glm-gateway", "glm-5", &["fs.read"])
            .map_err(ctx("interpret_chat must succeed"))?;
        assert!(response.message.is_none());
        assert_eq!(response.tool_calls.len(), 1);
        assert_eq!(response.tool_calls[0].id.as_str(), "call_text_0");
        assert_eq!(response.tool_calls[0].name.as_str(), "fs.read");
        assert_eq!(
            response.tool_calls[0].arguments,
            serde_json::json!({"path": "harw-cli/src/main.rs"})
        );
        assert!(matches!(response.stop, StopReason::ToolUse));
        Ok(())
    }

    #[test]
    fn test_interpret_chat_leaves_text_tool_call_as_plain_text_when_not_offered() -> TestResult {
        let body = serde_json::json!({
            "choices": [{
                "finish_reason": "stop",
                "message": {
                    "role": "assistant",
                    "content": "<tool_call>fs.read{\"path\":\"harw-cli/src/main.rs\"}"
                }
            }]
        });
        // Kein Tool angeboten (`offered_tools` leer) -- der Text bleibt
        // unverändert die finale Antwort, kein synthetischer Tool-Call.
        let response = interpret_chat(&body, "glm-gateway", "glm-5", &[])
            .map_err(ctx("interpret_chat must succeed"))?;
        assert_eq!(
            response.message.as_deref(),
            Some("<tool_call>fs.read{\"path\":\"harw-cli/src/main.rs\"}")
        );
        assert!(response.tool_calls.is_empty());
        Ok(())
    }

    #[test]
    fn test_extract_chat_content_none_when_missing() {
        let body = serde_json::json!({ "choices": [] });
        assert_eq!(extract_chat_content(&body), None);
    }

    #[test]
    fn test_extract_openai_usage_responses_transport() {
        let body = serde_json::json!({
            "usage": {
                "input_tokens": 12,
                "output_tokens": 34,
                "output_tokens_details": { "reasoning_tokens": 7 },
                "input_tokens_details": { "cached_tokens": 3 }
            }
        });
        let usage = extract_openai_usage(&body, Transport::Responses);
        assert_eq!(
            usage,
            TokenUsage {
                cache_separate: false,
                input_tokens: 12,
                output_tokens: 34,
                reasoning_tokens: Some(7),
                cached_tokens: Some(3),
                cache_write_tokens: None,
            }
        );
    }

    #[test]
    fn test_extract_openai_usage_chat_transport() {
        let body = serde_json::json!({
            "usage": {
                "prompt_tokens": 20,
                "completion_tokens": 5,
                "completion_tokens_details": { "reasoning_tokens": 2 },
                "prompt_tokens_details": { "cached_tokens": 1 }
            }
        });
        let usage = extract_openai_usage(&body, Transport::Chat);
        assert_eq!(
            usage,
            TokenUsage {
                cache_separate: false,
                input_tokens: 20,
                output_tokens: 5,
                reasoning_tokens: Some(2),
                cached_tokens: Some(1),
                cache_write_tokens: None,
            }
        );
    }

    #[test]
    fn test_extract_openai_usage_dashscope_cache_creation_tokens() {
        let body = serde_json::json!({
            "usage": {
                "prompt_tokens": 1200,
                "completion_tokens": 40,
                "prompt_tokens_details": {
                    "cached_tokens": 0,
                    "cache_creation_input_tokens": 1024
                }
            }
        });
        let usage = extract_openai_usage(&body, Transport::Chat);
        assert_eq!(usage.cache_write_tokens, Some(1024));
        assert_eq!(usage.cached_tokens, Some(0));
    }

    #[test]
    fn test_extract_openai_usage_missing_usage_object_falls_back_to_default() {
        let body = serde_json::json!({ "output": [] });
        assert_eq!(
            extract_openai_usage(&body, Transport::Responses),
            TokenUsage::default()
        );
        assert_eq!(
            extract_openai_usage(&body, Transport::Chat),
            TokenUsage::default()
        );
    }

    #[test]
    fn test_map_effort_to_openai_all_variants() {
        assert_eq!(
            map_effort_to_openai(harw_types::ReasoningEffort::Minimal),
            "minimal"
        );
        assert_eq!(
            map_effort_to_openai(harw_types::ReasoningEffort::Low),
            "low"
        );
        assert_eq!(
            map_effort_to_openai(harw_types::ReasoningEffort::Medium),
            "medium"
        );
        assert_eq!(
            map_effort_to_openai(harw_types::ReasoningEffort::High),
            "high"
        );
    }

    #[test]
    fn test_build_request_sets_reasoning_when_effort_present() -> TestResult {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("Hi there");
        let request = ModelRequest {
            stream: None,
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: Some(harw_types::ReasoningEffort::Minimal),
            model_id: None,
            provider_id: None,
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
            cancel: None,
            identity: None,
        };

        let req = build_request("gpt-test", &request);
        let reasoning = req
            .reasoning
            .ok_or(TestError::Missing("reasoning must be set"))?;
        assert_eq!(reasoning.effort.as_deref(), Some("minimal"));
        assert_eq!(reasoning.summary.as_deref(), Some("auto"));
        Ok(())
    }

    #[test]
    fn test_build_request_reasoning_summary_serializes_as_auto() -> TestResult {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("Hi there");
        let request = ModelRequest {
            stream: None,
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: Some(harw_types::ReasoningEffort::High),
            model_id: None,
            provider_id: None,
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
            cancel: None,
            identity: None,
        };

        let req = build_request("gpt-test", &request);
        let json = serde_json::to_value(&req).map_err(ctx("request must serialize"))?;
        assert_eq!(
            json.pointer("/reasoning/summary"),
            Some(&Value::from("auto"))
        );
        Ok(())
    }

    #[test]
    fn test_build_request_omits_reasoning_when_effort_absent() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("Hi there");
        let request = ModelRequest {
            stream: None,
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
            cancel: None,
            identity: None,
        };

        let req = build_request("gpt-test", &request);
        assert!(req.reasoning.is_none());
    }

    fn sample_tool_spec() -> ToolSpec {
        ToolSpec::Function(harw_tools::FunctionToolSpec {
            name: ToolName::new("get_weather"),
            description: "Get the current weather".to_owned(),
            parameters: harw_tools::JsonSchema {
                schema_type: Some(harw_tools::JsonSchemaType::Object),
                ..Default::default()
            },
            strict: false,
        })
    }

    // Erzeugt ein Function-Tool mit einem Pflicht- und einem optionalen
    // Property, um `strict_parameters` gegen ein Schema mit echter Lücke
    // zwischen `properties` und `required` zu testen.
    fn tool_spec_with_optional_field(strict: bool) -> ToolSpec {
        let mut properties = std::collections::BTreeMap::new();
        properties.insert(
            "path".to_owned(),
            harw_tools::JsonSchema {
                schema_type: Some(harw_tools::JsonSchemaType::String),
                ..Default::default()
            },
        );
        properties.insert(
            "max_bytes".to_owned(),
            harw_tools::JsonSchema {
                schema_type: Some(harw_tools::JsonSchemaType::Integer),
                ..Default::default()
            },
        );
        ToolSpec::Function(harw_tools::FunctionToolSpec {
            name: ToolName::new("read_file"),
            description: "Read a file".to_owned(),
            parameters: harw_tools::JsonSchema {
                schema_type: Some(harw_tools::JsonSchemaType::Object),
                properties: Some(properties),
                required: Some(vec!["path".to_owned()]),
                ..Default::default()
            },
            strict,
        })
    }

    #[test]
    fn test_build_responses_tools_strict_true_required_includes_every_property() -> TestResult {
        let tools = vec![tool_spec_with_optional_field(true)];
        let defs = build_responses_tools(&tools);
        assert_eq!(defs.len(), 1);
        assert!(defs[0].strict);
        let required = defs[0]
            .parameters
            .get("required")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("required array present"))?;
        let required: Vec<&str> = required.iter().filter_map(Value::as_str).collect();
        assert!(required.contains(&"path"));
        assert!(required.contains(&"max_bytes"));
        assert_eq!(required.len(), 2);
        Ok(())
    }

    #[test]
    fn test_build_responses_tools_strict_false_leaves_parameters_unchanged() -> TestResult {
        let tools = vec![tool_spec_with_optional_field(false)];
        let defs = build_responses_tools(&tools);
        assert_eq!(defs.len(), 1);
        assert!(!defs[0].strict);
        let required = defs[0]
            .parameters
            .get("required")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("required array present"))?;
        let required: Vec<&str> = required.iter().filter_map(Value::as_str).collect();
        assert_eq!(required, vec!["path"]);
        assert!(defs[0].parameters.get("additionalProperties").is_none());
        Ok(())
    }

    #[test]
    fn test_build_chat_tools_strict_true_required_includes_every_property() -> TestResult {
        let tools = vec![tool_spec_with_optional_field(true)];
        let defs = build_chat_tools(&tools);
        assert_eq!(defs.len(), 1);
        let required = defs[0]["function"]["parameters"]["required"]
            .as_array()
            .ok_or(TestError::Missing("required array present"))?;
        let required: Vec<&str> = required.iter().filter_map(Value::as_str).collect();
        assert!(required.contains(&"path"));
        assert!(required.contains(&"max_bytes"));
        assert_eq!(required.len(), 2);
        assert_eq!(
            defs[0]["function"]["parameters"]["additionalProperties"],
            Value::Bool(false)
        );
        Ok(())
    }

    #[test]
    fn test_build_chat_tools_strict_false_leaves_parameters_unchanged() -> TestResult {
        let tools = vec![tool_spec_with_optional_field(false)];
        let defs = build_chat_tools(&tools);
        assert_eq!(defs.len(), 1);
        let required = defs[0]["function"]["parameters"]["required"]
            .as_array()
            .ok_or(TestError::Missing("required array present"))?;
        let required: Vec<&str> = required.iter().filter_map(Value::as_str).collect();
        assert_eq!(required, vec!["path"]);
        assert!(defs[0]["function"]["parameters"]["additionalProperties"].is_null());
        Ok(())
    }

    #[test]
    fn test_build_request_includes_tools_when_present() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("weather?");
        let request = ModelRequest {
            stream: None,
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: vec![sample_tool_spec()],
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
            cancel: None,
            identity: None,
        };
        let req = build_request("gpt-test", &request);
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.tools[0].kind, "function");
        assert_eq!(req.tools[0].name, "get_weather");
    }

    #[test]
    fn test_build_request_maps_tool_call_to_function_call_item() -> TestResult {
        let mut history = harw_core::ConversationHistory::new();
        history.push_tool_call(
            harw_types::ToolCallId::from_str("call_1"),
            "get_weather",
            serde_json::json!({"location": "Paris"}),
        );
        let request = ModelRequest {
            stream: None,
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
            cancel: None,
            identity: None,
        };
        let req = build_request("gpt-test", &request);
        assert_eq!(req.input.len(), 1);
        match &req.input[0] {
            harw_provider::openai::InputItem::FunctionCall {
                call_id,
                name,
                arguments,
            } => {
                assert_eq!(call_id, "call_1");
                assert_eq!(name, "get_weather");
                let parsed: Value = serde_json::from_str(arguments).map_err(ctx("valid JSON"))?;
                assert_eq!(
                    parsed.get("location").and_then(Value::as_str),
                    Some("Paris")
                );
            }
            other => {
                return Err(TestError::Unexpected(format!(
                    "expected FunctionCall item, got {other:?}"
                )));
            }
        }
        Ok(())
    }

    #[test]
    fn test_build_chat_body_includes_tools_when_present() -> TestResult {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("weather?");
        let request = ModelRequest {
            stream: None,
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: vec![sample_tool_spec()],
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
            cancel: None,
            identity: None,
        };
        let body = build_chat_body(&request, "gpt-4o-mini");
        let tools = body
            .get("tools")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("tools array"))?;
        assert_eq!(tools.len(), 1);
        assert_eq!(
            tools[0].get("type").and_then(Value::as_str),
            Some("function")
        );
        assert_eq!(
            tools[0]
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str),
            Some("get_weather")
        );
        Ok(())
    }

    #[test]
    fn test_build_chat_body_omits_tools_when_empty() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("hi");
        let request = ModelRequest {
            stream: None,
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
            cancel: None,
            identity: None,
        };
        let body = build_chat_body(&request, "gpt-4o-mini");
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn test_build_chat_body_maps_tool_call_to_tool_calls_array() -> TestResult {
        let mut history = harw_core::ConversationHistory::new();
        history.push_tool_call(
            harw_types::ToolCallId::from_str("call_1"),
            "get_weather",
            serde_json::json!({"location": "Paris"}),
        );
        let request = ModelRequest {
            stream: None,
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
            cancel: None,
            identity: None,
        };
        let body = build_chat_body(&request, "gpt-4o-mini");
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("messages"))?;
        assert_eq!(
            messages[0].get("role").and_then(Value::as_str),
            Some("assistant")
        );
        let tool_calls = messages[0]
            .get("tool_calls")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("tool_calls array"))?;
        assert_eq!(
            tool_calls[0].get("id").and_then(Value::as_str),
            Some("call_1")
        );
        assert_eq!(
            tool_calls[0].get("type").and_then(Value::as_str),
            Some("function")
        );
        let function = tool_calls[0]
            .get("function")
            .ok_or(TestError::Missing("function object"))?;
        assert_eq!(
            function.get("name").and_then(Value::as_str),
            Some("get_weather")
        );
        let arguments_str = function
            .get("arguments")
            .and_then(Value::as_str)
            .ok_or(TestError::Missing("arguments string"))?;
        let parsed: Value = serde_json::from_str(arguments_str).map_err(ctx("valid JSON"))?;
        assert_eq!(
            parsed.get("location").and_then(Value::as_str),
            Some("Paris")
        );
        Ok(())
    }

    #[test]
    fn test_build_chat_body_maps_tool_result_to_tool_role_with_call_id() -> TestResult {
        let mut history = harw_core::ConversationHistory::new();
        history.push_tool_result(
            harw_types::ToolCallId::from_str("call_1"),
            ToolCallResult::success(serde_json::json!({"temp": 20})),
            5,
        );
        let request = ModelRequest {
            stream: None,
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
            cancel: None,
            identity: None,
        };
        let body = build_chat_body(&request, "gpt-4o-mini");
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("messages"))?;
        assert_eq!(
            messages[0].get("role").and_then(Value::as_str),
            Some("tool")
        );
        assert_eq!(
            messages[0].get("tool_call_id").and_then(Value::as_str),
            Some("call_1")
        );
        Ok(())
    }

    #[test]
    fn test_build_chat_body_groups_parallel_tool_calls_into_one_assistant_message() -> TestResult {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("what's the weather in Paris and Berlin?");
        history.push_tool_call(
            harw_types::ToolCallId::from_str("call_a"),
            "get_weather",
            serde_json::json!({"location": "Paris"}),
        );
        history.push_tool_call(
            harw_types::ToolCallId::from_str("call_b"),
            "get_weather",
            serde_json::json!({"location": "Berlin"}),
        );
        history.push_tool_result(
            harw_types::ToolCallId::from_str("call_a"),
            ToolCallResult::success(serde_json::json!({"temp": 20})),
            5,
        );
        history.push_tool_result(
            harw_types::ToolCallId::from_str("call_b"),
            ToolCallResult::success(serde_json::json!({"temp": 15})),
            5,
        );
        let request = ModelRequest {
            stream: None,
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
            cancel: None,
            identity: None,
        };
        let body = build_chat_body(&request, "gpt-4o-mini");
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("messages"))?;

        // user, one grouped assistant message, two tool-result messages.
        assert_eq!(messages.len(), 4);
        assert_eq!(
            messages[0].get("role").and_then(Value::as_str),
            Some("user")
        );

        assert_eq!(
            messages[1].get("role").and_then(Value::as_str),
            Some("assistant")
        );
        let tool_calls = messages[1]
            .get("tool_calls")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("tool_calls array"))?;
        assert_eq!(tool_calls.len(), 2);
        assert_eq!(
            tool_calls[0].get("id").and_then(Value::as_str),
            Some("call_a")
        );
        assert_eq!(
            tool_calls[1].get("id").and_then(Value::as_str),
            Some("call_b")
        );

        assert_eq!(
            messages[2].get("role").and_then(Value::as_str),
            Some("tool")
        );
        assert_eq!(
            messages[2].get("tool_call_id").and_then(Value::as_str),
            Some("call_a")
        );
        assert_eq!(
            messages[3].get("role").and_then(Value::as_str),
            Some("tool")
        );
        assert_eq!(
            messages[3].get("tool_call_id").and_then(Value::as_str),
            Some("call_b")
        );
        Ok(())
    }

    #[test]
    fn test_build_chat_body_single_tool_call_stays_one_assistant_message() -> TestResult {
        let mut history = harw_core::ConversationHistory::new();
        history.push_tool_call(
            harw_types::ToolCallId::from_str("call_1"),
            "get_weather",
            serde_json::json!({"location": "Paris"}),
        );
        let request = ModelRequest {
            stream: None,
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
            cancel: None,
            identity: None,
        };
        let body = build_chat_body(&request, "gpt-4o-mini");
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("messages"))?;
        assert_eq!(messages.len(), 1);
        let tool_calls = messages[0]
            .get("tool_calls")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("tool_calls array"))?;
        assert_eq!(tool_calls.len(), 1);
        Ok(())
    }

    #[test]
    fn test_build_chat_body_separate_tool_rounds_stay_separate_assistant_messages() -> TestResult {
        let mut history = harw_core::ConversationHistory::new();
        history.push_tool_call(
            harw_types::ToolCallId::from_str("call_a"),
            "get_weather",
            serde_json::json!({"location": "Paris"}),
        );
        history.push_tool_result(
            harw_types::ToolCallId::from_str("call_a"),
            ToolCallResult::success(serde_json::json!({"temp": 20})),
            5,
        );
        history.push_tool_call(
            harw_types::ToolCallId::from_str("call_b"),
            "get_weather",
            serde_json::json!({"location": "Berlin"}),
        );
        history.push_tool_result(
            harw_types::ToolCallId::from_str("call_b"),
            ToolCallResult::success(serde_json::json!({"temp": 15})),
            5,
        );
        let request = ModelRequest {
            stream: None,
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
            cancel: None,
            identity: None,
        };
        let body = build_chat_body(&request, "gpt-4o-mini");
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("messages"))?;

        // call_a, result_a, call_b, result_b: four separate messages, no grouping.
        assert_eq!(messages.len(), 4);
        assert_eq!(
            messages[0].get("role").and_then(Value::as_str),
            Some("assistant")
        );
        let first_calls = messages[0]
            .get("tool_calls")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("first tool_calls array"))?;
        assert_eq!(first_calls.len(), 1);
        assert_eq!(
            messages[1].get("role").and_then(Value::as_str),
            Some("tool")
        );
        assert_eq!(
            messages[2].get("role").and_then(Value::as_str),
            Some("assistant")
        );
        let second_calls = messages[2]
            .get("tool_calls")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("second tool_calls array"))?;
        assert_eq!(second_calls.len(), 1);
        assert_eq!(
            messages[3].get("role").and_then(Value::as_str),
            Some("tool")
        );
        Ok(())
    }

    #[test]
    fn test_extract_responses_tool_calls_parses_function_call_items() -> TestResult {
        let body = serde_json::json!({
            "output": [
                { "type": "reasoning" },
                {
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "get_weather",
                    "arguments": "{\"location\":\"Paris\"}"
                }
            ]
        });
        let calls = extract_openai_tool_calls(&body, Transport::Responses)
            .map_err(ctx("valid Responses tool-call arguments"))?;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id.as_str(), "call_1");
        assert_eq!(calls[0].name.as_str(), "get_weather");
        assert_eq!(
            calls[0].arguments.get("location").and_then(Value::as_str),
            Some("Paris")
        );
        Ok(())
    }

    #[test]
    fn test_extract_chat_tool_calls_parses_message_tool_calls() -> TestResult {
        let body = serde_json::json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "get_weather",
                            "arguments": "{\"location\":\"Paris\"}"
                        }
                    }]
                }
            }]
        });
        let calls = extract_openai_tool_calls(&body, Transport::Chat)
            .map_err(ctx("valid Chat tool-call arguments"))?;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id.as_str(), "call_1");
        assert_eq!(calls[0].name.as_str(), "get_weather");
        assert_eq!(
            calls[0].arguments.get("location").and_then(Value::as_str),
            Some("Paris")
        );
        Ok(())
    }

    #[test]
    fn test_extract_responses_tool_calls_rejects_malformed_item_with_valid_sibling() -> TestResult {
        let valid_sibling = serde_json::json!({
            "type": "function_call",
            "call_id": "call_valid",
            "name": "get_weather",
            "arguments": "{}"
        });
        let malformed_items = [
            (
                serde_json::json!({
                    "type": "function_call",
                    "name": "get_weather",
                    "arguments": "{}"
                }),
                ToolCallExtractionError::InvalidCallId,
            ),
            (
                serde_json::json!({
                    "type": "function_call",
                    "call_id": 1,
                    "name": "get_weather",
                    "arguments": "{}"
                }),
                ToolCallExtractionError::InvalidCallId,
            ),
            (
                serde_json::json!({
                    "type": "function_call",
                    "call_id": "call_malformed",
                    "arguments": "{}"
                }),
                ToolCallExtractionError::InvalidToolName,
            ),
            (
                serde_json::json!({
                    "type": "function_call",
                    "call_id": "call_malformed",
                    "name": 1,
                    "arguments": "{}"
                }),
                ToolCallExtractionError::InvalidToolName,
            ),
        ];

        for (malformed_item, expected_error) in malformed_items {
            let body = serde_json::json!({
                "output": [valid_sibling, malformed_item]
            });
            let Err(error) = extract_openai_tool_calls(&body, Transport::Responses) else {
                return Err(TestError::Unexpected(
                    "a malformed function_call must reject its valid sibling".to_owned(),
                ));
            };
            assert_eq!(error, expected_error);
        }
        Ok(())
    }

    #[test]
    fn test_extract_chat_tool_calls_rejects_malformed_item_with_valid_sibling() -> TestResult {
        let valid_sibling = serde_json::json!({
            "id": "call_valid",
            "function": { "name": "get_weather", "arguments": "{}" }
        });
        let malformed_calls = [
            (
                serde_json::json!({
                    "function": { "name": "get_weather", "arguments": "{}" }
                }),
                ToolCallExtractionError::InvalidCallId,
            ),
            (
                serde_json::json!({
                    "id": 1,
                    "function": { "name": "get_weather", "arguments": "{}" }
                }),
                ToolCallExtractionError::InvalidCallId,
            ),
            (
                serde_json::json!({ "id": "call_malformed" }),
                ToolCallExtractionError::InvalidFunction,
            ),
            (
                serde_json::json!({ "id": "call_malformed", "function": "not-an-object" }),
                ToolCallExtractionError::InvalidFunction,
            ),
            (
                serde_json::json!({
                    "id": "call_malformed",
                    "function": { "arguments": "{}" }
                }),
                ToolCallExtractionError::InvalidToolName,
            ),
            (
                serde_json::json!({
                    "id": "call_malformed",
                    "function": { "name": 1, "arguments": "{}" }
                }),
                ToolCallExtractionError::InvalidToolName,
            ),
        ];

        for (malformed_call, expected_error) in malformed_calls {
            let body = serde_json::json!({
                "choices": [{
                    "message": { "tool_calls": [valid_sibling, malformed_call] }
                }]
            });
            let Err(error) = extract_openai_tool_calls(&body, Transport::Chat) else {
                return Err(TestError::Unexpected(
                    "a malformed tool_calls item must reject its valid sibling".to_owned(),
                ));
            };
            assert_eq!(error, expected_error);
        }
        Ok(())
    }

    #[test]
    fn test_extract_openai_tool_calls_empty_when_absent() -> TestResult {
        assert!(
            extract_openai_tool_calls(&serde_json::json!({"output": []}), Transport::Responses)
                .map_err(ctx("empty Responses output is valid"))?
                .is_empty()
        );
        assert!(
            extract_openai_tool_calls(&serde_json::json!({"choices": []}), Transport::Chat)
                .map_err(ctx("empty Chat choices are valid"))?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn test_extract_openai_tool_calls_preserves_empty_object_arguments() -> TestResult {
        let responses_body = serde_json::json!({
            "output": [{
                "type": "function_call",
                "call_id": "call_responses",
                "name": "get_weather",
                "arguments": "{}"
            }]
        });
        let chat_body = serde_json::json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "call_chat",
                        "function": { "name": "get_weather", "arguments": "{}" }
                    }]
                }
            }]
        });

        let responses_calls = extract_openai_tool_calls(&responses_body, Transport::Responses)
            .map_err(ctx("empty object is valid Responses arguments"))?;
        let chat_calls = extract_openai_tool_calls(&chat_body, Transport::Chat)
            .map_err(ctx("empty object is valid Chat arguments"))?;

        assert_eq!(responses_calls[0].arguments, serde_json::json!({}));
        assert_eq!(chat_calls[0].arguments, serde_json::json!({}));
        Ok(())
    }

    #[tokio::test]
    async fn test_respond_rejects_malformed_responses_tool_call_arguments() -> TestResult {
        assert_respond_rejects_tool_call(
            Transport::Responses,
            serde_json::json!({
                "output": [{
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "get_weather",
                    "arguments": "private-provider-arguments:not-json"
                }]
            }),
            "provider returned a tool call with missing or invalid arguments",
        )
        .await
    }

    #[tokio::test]
    async fn test_respond_rejects_missing_responses_tool_call_arguments() -> TestResult {
        assert_respond_rejects_tool_call(
            Transport::Responses,
            serde_json::json!({
                "output": [{
                    "type": "function_call",
                    "call_id": "call_1",
                    "name": "get_weather"
                }]
            }),
            "provider returned a tool call with missing or invalid arguments",
        )
        .await
    }

    #[tokio::test]
    async fn test_respond_rejects_malformed_chat_tool_call_arguments() -> TestResult {
        assert_respond_rejects_tool_call(
            Transport::Chat,
            serde_json::json!({
                "choices": [{
                    "message": {
                        "tool_calls": [{
                            "id": "call_1",
                            "function": {
                                "name": "get_weather",
                                "arguments": "private-provider-arguments:not-json"
                            }
                        }]
                    }
                }]
            }),
            "provider returned a tool call with missing or invalid arguments",
        )
        .await
    }

    #[tokio::test]
    async fn test_respond_rejects_missing_chat_tool_call_arguments() -> TestResult {
        assert_respond_rejects_tool_call(
            Transport::Chat,
            serde_json::json!({
                "choices": [{
                    "message": {
                        "tool_calls": [{
                            "id": "call_1",
                            "function": { "name": "get_weather" }
                        }]
                    }
                }]
            }),
            "provider returned a tool call with missing or invalid arguments",
        )
        .await
    }

    #[tokio::test]
    async fn test_respond_maps_malformed_responses_tool_call_to_generic_model_error() -> TestResult
    {
        assert_respond_rejects_tool_call(
            Transport::Responses,
            serde_json::json!({
                "output": [{
                    "type": "function_call",
                    "name": "get_weather",
                    "arguments": "{}"
                }]
            }),
            "provider returned a tool call with missing or invalid call ID",
        )
        .await
    }

    #[tokio::test]
    async fn test_respond_maps_malformed_chat_tool_call_to_generic_model_error() -> TestResult {
        assert_respond_rejects_tool_call(
            Transport::Chat,
            serde_json::json!({
                "choices": [{
                    "message": {
                        "tool_calls": [{
                            "id": "call_1",
                            "function": "not-an-object"
                        }]
                    }
                }]
            }),
            "provider returned a tool call with missing or invalid function",
        )
        .await
    }

    // ── W1-06b: Endpoint, Env-Fallback, file:-Secrets, Header, Redirects ─────

    #[test]
    fn validate_endpoint_rejects_http_for_non_loopback_hosts() -> TestResult {
        for endpoint in [
            "http://api.openai.com/v1",
            "http://gateway.example/v1",
            "http://192.168.1.10:11434",
            "http://10.0.0.1/v1",
            "http://localhost.evil.example/v1",
        ] {
            let Err(error) = validate_endpoint(endpoint) else {
                return Err(TestError::Unexpected(format!(
                    "{endpoint}: expected rejection"
                )));
            };
            assert!(matches!(error, HttpProviderError::Decode(_)), "{endpoint}");
            assert!(!error.to_string().contains(endpoint), "{endpoint}");
        }
        Ok(())
    }

    #[test]
    fn validate_endpoint_allows_https_and_http_loopback() -> TestResult {
        for endpoint in [
            "https://api.openai.com/v1",
            "https://gateway.example:8443/anthropic/",
            "http://127.0.0.1:11434",
            "http://localhost:11434/v1",
            "http://LOCALHOST./v1",
            "http://ollama.localhost:11434",
            "http://[::1]:8080/v1",
        ] {
            validate_endpoint(endpoint)
                .map_err(|error| TestError::Unexpected(format!("{endpoint}: {error}")))?;
        }
        Ok(())
    }

    #[test]
    fn validate_endpoint_rejects_userinfo_query_fragment_and_placeholders() {
        for endpoint in [
            "https://user:pw@api.openai.com/v1",
            "https://user@api.openai.com/v1",
            "https://api.openai.com/v1?key=abc",
            "https://api.openai.com/v1#frag",
            "https://<resource>.services.ai.azure.com/anthropic",
            "https://example.test/<deployment>",
            "ftp://api.openai.com/v1",
            "api.openai.com/v1",
            "",
        ] {
            assert!(validate_endpoint(endpoint).is_err(), "{endpoint:?}");
        }
    }

    #[test]
    fn build_provider_rejects_plain_http_gateway_before_resolving_credentials() -> TestResult {
        let mut config = harw_config::ResolvedConfig::default();
        config.harness.default_provider = Some("gateway".to_owned());
        config.harness.default_model = Some("model".to_owned());
        config.providers.insert(
            "gateway".to_owned(),
            configured_provider(
                "gateway",
                "http://gateway.example/v1".to_owned(),
                vec!["model"],
                "GATEWAY_KEY",
            ),
        );
        let error = match build_provider(&config) {
            Ok(_) => {
                return Err(TestError::Unexpected(
                    "plain-http non-loopback endpoint must be rejected".to_owned(),
                ));
            }
            Err(error) => error,
        };
        assert!(matches!(error, HttpProviderError::Decode(_)));
        Ok(())
    }

    fn anthropic_provider_without_auth(base_url: &str) -> harw_config::ProviderToml {
        harw_config::ProviderToml {
            stream: None,
            name: "anthropic".to_owned(),
            api: "anthropic-messages".to_owned(),
            base_url: base_url.to_owned(),
            auth: None,
            auth_header: None,
            api_key: None,
            headers: HashMap::new(),
            models: vec!["claude-test".to_owned()],
            enabled: true,
            origin_allowlist: harw_config::OriginAllowlistToml::default(),
            rate_limit: None,
            max_concurrency: None,
            originator: None,
            default_reasoning_effort: None,
            gateway_identity_headers: false,
        }
    }

    #[test]
    fn default_anthropic_base_url_is_the_official_host() {
        assert!(endpoint_is_official_host(
            DEFAULT_ANTHROPIC_BASE_URL,
            anthropic::ANTHROPIC_API_HOST
        ));
    }

    #[test]
    fn implicit_anthropic_env_credentials_are_not_sent_to_foreign_hosts() -> TestResult {
        let env_layer = BTreeMap::new();
        let lookups = std::cell::RefCell::new(Vec::<String>::new());
        let process_env = |name: &str| {
            lookups.borrow_mut().push(name.to_owned());
            (name == "CLAUDE_CODE_OAUTH_TOKEN").then(|| "sk-ant-oat-env-token".to_owned())
        };

        for foreign in [
            "https://evil.example",
            "https://api.anthropic.com.evil.example/v1",
            "https://evil.example/api.anthropic.com/v1",
            "https://api.anthropic.com:8443/v1",
            "https://eu.api.anthropic.com/v1",
            "http://127.0.0.1:9/v1",
        ] {
            let error = match resolve_anthropic(
                "anthropic",
                &anthropic_provider_without_auth(foreign),
                test_sources(&env_layer, None, None),
                &process_env,
            ) {
                Ok(_) => {
                    return Err(TestError::Unexpected(format!(
                        "{foreign}: implicit env credential must not be used"
                    )));
                }
                Err(error) => error,
            };
            assert!(
                matches!(error, HttpProviderError::MissingDefault { .. }),
                "{foreign}"
            );
            assert!(!error.to_string().contains("sk-ant-oat-env-token"));
        }
        assert!(
            lookups.borrow().is_empty(),
            "foreign hosts must not even read the process environment"
        );

        for official in [
            "https://api.anthropic.com",
            "https://api.anthropic.com/v1",
            "https://API.Anthropic.COM./v1",
            "https://api.anthropic.com:443",
        ] {
            let (_, credential) = match resolve_anthropic(
                "anthropic",
                &anthropic_provider_without_auth(official),
                test_sources(&env_layer, None, None),
                &process_env,
            ) {
                Ok(resolved) => resolved,
                Err(error) => {
                    return Err(TestError::Unexpected(format!("{official}: {error}")));
                }
            };
            assert!(
                matches!(credential, AnthropicCredential::OAuth(_)),
                "{official}"
            );
        }
        Ok(())
    }

    #[test]
    fn implicit_foundry_env_key_requires_matching_process_env_endpoint() -> TestResult {
        let mut env_layer = BTreeMap::new();
        env_layer.insert(
            "ANTHROPIC_FOUNDRY_API_KEY".to_owned(),
            "foundry-layer-key".to_owned(),
        );
        let bound_env = |name: &str| {
            (name == "ANTHROPIC_FOUNDRY_BASE_URL")
                .then(|| "https://RES.services.ai.azure.com/anthropic".to_owned())
        };
        let matching =
            anthropic_provider_without_auth("https://res.services.ai.azure.com/anthropic/");
        let foreign = anthropic_provider_without_auth("https://evil.example/anthropic/");

        let resolved = resolve_anthropic(
            "foundry",
            &matching,
            test_sources(&env_layer, None, None),
            &bound_env,
        );
        assert!(matches!(resolved, Ok((_, AnthropicCredential::ApiKey(_)))));

        let bound: &dyn Fn(&str) -> Option<String> = &bound_env;
        let unbound: &dyn Fn(&str) -> Option<String> = &no_process_env;
        for (provider, process_env) in [(&foreign, bound), (&matching, unbound)] {
            let error = match resolve_anthropic(
                "foundry-prod",
                provider,
                test_sources(&env_layer, None, None),
                process_env,
            ) {
                Ok(_) => {
                    return Err(TestError::Unexpected(
                        "unbound Foundry endpoint must not receive the env key".to_owned(),
                    ));
                }
                Err(error) => error,
            };
            assert!(matches!(error, HttpProviderError::MissingDefault { .. }));
            assert!(!error.to_string().contains("foundry-layer-key"));
        }
        Ok(())
    }

    fn write_file_with_mode(path: &Path, contents: &str, mode: u32) -> TestResult {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, contents).map_err(ctx("write credential fixture"))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(ctx("chmod credential fixture"))?;
        Ok(())
    }

    fn home_with_secrets() -> TestResult<(tempfile::TempDir, PathBuf)> {
        let home = tempfile::tempdir().map_err(ctx("temporary home"))?;
        let secrets = home.path().join("secrets");
        std::fs::create_dir(&secrets).map_err(ctx("create secrets directory"))?;
        Ok((home, secrets))
    }

    fn file_ref(path: &Path) -> TestResult<harw_config::SecretRef> {
        Ok(harw_config::SecretRef::File(
            path.to_str()
                .ok_or(TestError::Missing("UTF-8 fixture path"))?
                .to_owned(),
        ))
    }

    /// Löst `reference` auf und liefert (Grund, gerenderter Fehler).
    fn file_credential_error(
        reference: &harw_config::SecretRef,
        home: Option<&Path>,
    ) -> TestResult<(String, String)> {
        let env_layer = BTreeMap::new();
        let error = match resolve_secret(reference, test_sources(&env_layer, None, home)) {
            Ok(_) => {
                return Err(TestError::Unexpected(
                    "file credential must be rejected".to_owned(),
                ));
            }
            Err(error) => error,
        };
        let rendered = error.to_string();
        let HttpProviderError::UnresolvedCredential { reason, .. } = error else {
            return Err(TestError::Unexpected(format!(
                "unexpected error kind: {rendered}"
            )));
        };
        Ok((reason, rendered))
    }

    #[test]
    fn file_credentials_below_home_secrets_are_resolved() -> TestResult {
        let (home, secrets) = home_with_secrets()?;
        let token = secrets.join("provider.token");
        write_file_with_mode(&token, "  file-token-value\n", 0o600)?;
        let nested_dir = secrets.join("nested");
        std::fs::create_dir(&nested_dir).map_err(ctx("create nested directory"))?;
        let json = nested_dir.join("credentials.json");
        write_file_with_mode(&json, r#"{"oauth":{"access":" json-token "}}"#, 0o400)?;

        let env_layer = BTreeMap::new();
        let sources = test_sources(&env_layer, None, Some(home.path()));
        let secret =
            resolve_secret(&file_ref(&token)?, sources).map_err(ctx("private token file"))?;
        assert_eq!(secret.expose_secret(), "file-token-value");

        let json_ref = harw_config::SecretRef::FileJson {
            path: json
                .to_str()
                .ok_or(TestError::Missing("UTF-8 fixture path"))?
                .to_owned(),
            pointer: "/oauth/access".to_owned(),
        };
        let secret = resolve_secret(&json_ref, sources).map_err(ctx("private JSON credential"))?;
        assert_eq!(secret.expose_secret(), "json-token");
        Ok(())
    }

    #[test]
    fn file_credentials_outside_home_secrets_are_rejected_without_path_or_content() -> TestResult {
        let (home, secrets) = home_with_secrets()?;
        let outside = home.path().join("outside.token");
        write_file_with_mode(&outside, "outside-secret-value", 0o600)?;
        let sibling_dir = home.path().join("secrets-evil");
        std::fs::create_dir(&sibling_dir).map_err(ctx("create sibling directory"))?;
        let sibling = sibling_dir.join("x.token");
        write_file_with_mode(&sibling, "outside-secret-value", 0o600)?;

        let traversal = format!("{}/../outside.token", secrets.display());
        let references = [
            file_ref(&outside)?,
            file_ref(&sibling)?,
            file_ref(&secrets)?,
            harw_config::SecretRef::File(traversal),
            harw_config::SecretRef::File("secrets/provider.token".to_owned()),
            harw_config::SecretRef::FileJson {
                path: outside
                    .to_str()
                    .ok_or(TestError::Missing("UTF-8 fixture path"))?
                    .to_owned(),
                pointer: "/token".to_owned(),
            },
        ];
        for reference in &references {
            let (reason, rendered) = file_credential_error(reference, Some(home.path()))?;
            assert_eq!(reason, FILE_CREDENTIAL_OUTSIDE_SECRETS_REASON, "{rendered}");
            assert!(!rendered.contains(home.path().to_str().ok_or(TestError::Missing("UTF-8"))?));
            assert!(!rendered.contains("outside-secret-value"));
        }

        let (reason, rendered) = file_credential_error(&file_ref(&outside)?, None)?;
        assert_eq!(reason, FILE_CREDENTIAL_NO_HOME_REASON);
        assert!(!rendered.contains("outside.token"));
        Ok(())
    }

    #[test]
    fn file_credentials_through_symlinks_are_rejected() -> TestResult {
        use std::os::unix::fs::symlink;

        let (home, secrets) = home_with_secrets()?;
        let outside = home.path().join("outside.token");
        write_file_with_mode(&outside, "symlinked-secret-value", 0o600)?;
        let outside_dir = home.path().join("outside-dir");
        std::fs::create_dir(&outside_dir).map_err(ctx("create outside directory"))?;
        write_file_with_mode(
            &outside_dir.join("t.token"),
            "symlinked-secret-value",
            0o600,
        )?;

        let final_link = secrets.join("link.token");
        symlink(&outside, &final_link).map_err(ctx("create final-component symlink"))?;
        let dir_link = secrets.join("linked-dir");
        symlink(&outside_dir, &dir_link).map_err(ctx("create intermediate symlink"))?;

        for path in [final_link, dir_link.join("t.token")] {
            let (reason, rendered) = file_credential_error(&file_ref(&path)?, Some(home.path()))?;
            assert_eq!(reason, FILE_CREDENTIAL_OPEN_REASON, "{rendered}");
            assert!(!rendered.contains("symlinked-secret-value"));
            assert!(!rendered.contains("link.token"));
            assert!(!rendered.contains("linked-dir"));
        }

        // `<home>/secrets` selbst als Symlink.
        let other_home = tempfile::tempdir().map_err(ctx("second temporary home"))?;
        symlink(&outside_dir, other_home.path().join("secrets")).map_err(ctx("secrets symlink"))?;
        let via_link = other_home.path().join("secrets").join("t.token");
        let (reason, _) = file_credential_error(&file_ref(&via_link)?, Some(other_home.path()))?;
        assert_eq!(reason, FILE_CREDENTIAL_OPEN_REASON);
        Ok(())
    }

    #[test]
    fn file_credentials_with_group_or_other_permissions_are_rejected() -> TestResult {
        let (home, secrets) = home_with_secrets()?;
        for (name, mode) in [("world.token", 0o644), ("group.token", 0o640)] {
            let path = secrets.join(name);
            write_file_with_mode(&path, "shared-secret-value", mode)?;
            let (reason, rendered) = file_credential_error(&file_ref(&path)?, Some(home.path()))?;
            assert_eq!(reason, FILE_CREDENTIAL_NOT_PRIVATE_REASON, "{name}");
            assert!(!rendered.contains("shared-secret-value"));
            assert!(!rendered.contains(name));
        }
        Ok(())
    }

    #[test]
    fn file_json_credential_errors_do_not_echo_content() -> TestResult {
        let (home, secrets) = home_with_secrets()?;
        let path = secrets.join("broken.json");
        write_file_with_mode(&path, "{not json broken-secret-value", 0o600)?;
        let reference = harw_config::SecretRef::FileJson {
            path: path
                .to_str()
                .ok_or(TestError::Missing("UTF-8 fixture path"))?
                .to_owned(),
            pointer: "/token".to_owned(),
        };
        let (reason, rendered) = file_credential_error(&reference, Some(home.path()))?;
        assert_eq!(reason, FILE_CREDENTIAL_JSON_REASON);
        assert!(!rendered.contains("broken-secret-value"));
        assert!(!rendered.contains("broken.json"));
        Ok(())
    }

    #[test]
    fn oversized_file_credentials_are_rejected() -> TestResult {
        let (home, secrets) = home_with_secrets()?;
        let path = secrets.join("huge.token");
        let limit = usize::try_from(MAX_FILE_CREDENTIAL_BYTES).map_err(ctx("limit fits usize"))?;
        write_file_with_mode(&path, &"x".repeat(limit + 1), 0o600)?;
        let (reason, _) = file_credential_error(&file_ref(&path)?, Some(home.path()))?;
        assert_eq!(reason, FILE_CREDENTIAL_READ_REASON);
        Ok(())
    }

    #[test]
    fn build_provider_with_home_enables_file_credentials_that_build_provider_rejects() -> TestResult
    {
        let (home, secrets) = home_with_secrets()?;
        let token = secrets.join("gateway.key");
        write_file_with_mode(&token, "gateway-file-key", 0o600)?;
        let mut provider = configured_provider(
            "gateway",
            "https://gateway.example/v1".to_owned(),
            vec!["model"],
            "unused",
        );
        provider.auth = Some(file_ref(&token)?);
        let mut config = harw_config::ResolvedConfig::default();
        config.harness.default_provider = Some("gateway".to_owned());
        config.harness.default_model = Some("model".to_owned());
        config.providers.insert("gateway".to_owned(), provider);

        build_provider_with_home(&config, home.path(), None)
            .map_err(ctx("file credential below <home>/secrets"))?;
        let error = match build_provider(&config) {
            Ok(_) => {
                return Err(TestError::Unexpected(
                    "file credentials require a home".to_owned(),
                ));
            }
            Err(error) => error,
        };
        assert!(matches!(
            &error,
            HttpProviderError::UnresolvedCredential { reason, .. }
                if reason == FILE_CREDENTIAL_NO_HOME_REASON
        ));
        assert!(!error.to_string().contains("gateway.key"));
        Ok(())
    }

    #[test]
    fn configured_credential_headers_reject_plaintext_and_resolve_secret_refs() -> TestResult {
        let mut env_layer = BTreeMap::new();
        env_layer.insert(
            "HARW_TEST_GATEWAY_HEADER_TOKEN_W106B".to_owned(),
            "Bearer resolved-gateway-token".to_owned(),
        );
        let sources = test_sources(&env_layer, None, None);

        let plaintext = HashMap::from([(
            "cf-aig-authorization".to_owned(),
            "Bearer plaintext-gateway-token".to_owned(),
        )]);
        let Err(error) = configured_headers("gateway", &plaintext, sources) else {
            return Err(TestError::Unexpected(
                "plaintext credential header must be rejected".to_owned(),
            ));
        };
        assert!(!error.to_string().contains("plaintext-gateway-token"));

        let referenced = HashMap::from([
            (
                "cf-aig-authorization".to_owned(),
                "env:HARW_TEST_GATEWAY_HEADER_TOKEN_W106B".to_owned(),
            ),
            ("x-provider-marker".to_owned(), "gateway".to_owned()),
        ]);
        let headers = configured_headers("gateway", &referenced, sources)
            .map_err(ctx("secret ref header"))?;
        let credential = headers
            .get("cf-aig-authorization")
            .ok_or(TestError::Missing("credential header"))?;
        assert_eq!(credential.as_bytes(), b"Bearer resolved-gateway-token");
        assert!(credential.is_sensitive());
        assert!(
            !headers
                .get("x-provider-marker")
                .ok_or(TestError::Missing("marker"))?
                .is_sensitive()
        );
        assert!(!format!("{headers:?}").contains("resolved-gateway-token"));
        Ok(())
    }

    #[tokio::test]
    async fn openai_credential_headers_are_marked_sensitive() -> TestResult {
        for (auth_header, header_name) in [
            ("api-key", "api-key"),
            ("x-api-key", "x-api-key"),
            ("bearer", "authorization"),
        ] {
            let mut provider = OpenAiResponsesProvider::new(
                "https://example.test/v1",
                "model",
                SecretString::new("sk-sensitive-header-value".into()),
            )
            .map_err(ctx("OpenAiResponsesProvider::new"))?;
            provider.auth_header = auth_header.to_owned();
            let api_key = provider.api_key.clone();
            let request = provider
                .authorized_request("https://example.test/v1/chat/completions", &api_key, None)
                .await
                .map_err(ctx("credential header"))?
                .build()
                .map_err(ctx("request builds"))?;
            let value = request
                .headers()
                .get(header_name)
                .ok_or(TestError::Unexpected(format!(
                    "missing header {header_name}"
                )))?;
            assert!(value.is_sensitive(), "{auth_header}");
            assert!(!format!("{:?}", request.headers()).contains("sk-sensitive-header-value"));
        }
        Ok(())
    }

    fn test_identity(session: &str, agent: &str, role: &str) -> RequestIdentity {
        RequestIdentity {
            session: session.to_owned(),
            agent: agent.to_owned(),
            role: role.to_owned(),
        }
    }

    #[test]
    fn test_identity_headers_builds_three_headers() -> TestResult {
        let identity = test_identity("sess-123", "agent-x", "worker");
        let headers = identity_headers(&identity);
        assert_eq!(headers.len(), 3);
        assert_eq!(
            headers
                .get("x-harw-session")
                .ok_or(TestError::Missing("x-harw-session"))?,
            "sess-123"
        );
        assert_eq!(
            headers
                .get("x-harw-agent")
                .ok_or(TestError::Missing("x-harw-agent"))?,
            "agent-x"
        );
        assert_eq!(
            headers
                .get("x-harw-role")
                .ok_or(TestError::Missing("x-harw-role"))?,
            "worker"
        );
        Ok(())
    }

    #[test]
    fn test_identity_headers_strips_non_ascii_and_control_chars() -> TestResult {
        let identity = test_identity("sess\n123\t äöü", "agent", "role");
        let headers = identity_headers(&identity);
        assert_eq!(
            headers
                .get("x-harw-session")
                .ok_or(TestError::Missing("x-harw-session"))?,
            "sess123"
        );
        Ok(())
    }

    #[test]
    fn test_identity_headers_truncates_overlong_value_to_64_chars() -> TestResult {
        let long_session = "s".repeat(100);
        let identity = test_identity(&long_session, "agent", "role");
        let headers = identity_headers(&identity);
        let value = headers
            .get("x-harw-session")
            .ok_or(TestError::Missing("x-harw-session"))?;
        assert_eq!(value.len(), 64);
        assert_eq!(
            value.to_str().map_err(ctx("header value is ASCII"))?,
            "s".repeat(64)
        );
        Ok(())
    }

    #[test]
    fn test_identity_headers_skips_empty_value_after_sanitization() {
        // Nur Whitespace/Steuerzeichen — nach der Sanitisierung leer, also
        // wird der Header übersprungen statt einen leeren Wert zu senden.
        let identity = test_identity("sess", "   \n\t  ", "role");
        let headers = identity_headers(&identity);
        assert_eq!(headers.len(), 2);
        assert!(headers.get("x-harw-agent").is_none());
        assert!(headers.get("x-harw-session").is_some());
        assert!(headers.get("x-harw-role").is_some());
    }

    #[test]
    fn test_sanitize_identity_header_value_keeps_only_visible_ascii() {
        assert_eq!(sanitize_identity_header_value("abc-123_XYZ"), "abc-123_XYZ");
        assert_eq!(sanitize_identity_header_value("a b\tc\n"), "abc");
        assert_eq!(sanitize_identity_header_value("héllo"), "hllo");
        assert_eq!(sanitize_identity_header_value(""), "");
    }

    #[tokio::test]
    async fn authorized_request_adds_identity_headers_when_flag_enabled() -> TestResult {
        let mut provider = OpenAiResponsesProvider::new(
            "https://example.test/v1",
            "model",
            SecretString::new("sk-identity-headers-test".into()),
        )
        .map_err(ctx("OpenAiResponsesProvider::new"))?;
        provider.gateway_identity_headers = true;
        let api_key = provider.api_key.clone();
        let identity = test_identity("sess-abc", "agent-b", "planner");
        let request = provider
            .authorized_request(
                "https://example.test/v1/chat/completions",
                &api_key,
                Some(&identity),
            )
            .await
            .map_err(ctx("request builds"))?
            .build()
            .map_err(ctx("request builds"))?;
        assert_eq!(
            request
                .headers()
                .get("x-harw-session")
                .ok_or(TestError::Missing("x-harw-session"))?,
            "sess-abc"
        );
        assert_eq!(
            request
                .headers()
                .get("x-harw-agent")
                .ok_or(TestError::Missing("x-harw-agent"))?,
            "agent-b"
        );
        assert_eq!(
            request
                .headers()
                .get("x-harw-role")
                .ok_or(TestError::Missing("x-harw-role"))?,
            "planner"
        );
        assert!(request.headers().get("x-session-affinity").is_none());
        Ok(())
    }

    #[tokio::test]
    async fn authorized_request_omits_identity_headers_when_flag_disabled() -> TestResult {
        let provider = OpenAiResponsesProvider::new(
            "https://example.test/v1",
            "model",
            SecretString::new("sk-identity-headers-test".into()),
        )
        .map_err(ctx("OpenAiResponsesProvider::new"))?;
        assert!(!provider.gateway_identity_headers);
        let api_key = provider.api_key.clone();
        let identity = test_identity("sess-abc", "agent-b", "planner");
        let request = provider
            .authorized_request(
                "https://example.test/v1/chat/completions",
                &api_key,
                Some(&identity),
            )
            .await
            .map_err(ctx("request builds"))?
            .build()
            .map_err(ctx("request builds"))?;
        assert!(request.headers().get("x-harw-session").is_none());
        Ok(())
    }

    #[tokio::test]
    async fn authorized_request_static_header_wins_over_identity_header() -> TestResult {
        let mut provider = OpenAiResponsesProvider::new(
            "https://example.test/v1",
            "model",
            SecretString::new("sk-identity-headers-test".into()),
        )
        .map_err(ctx("OpenAiResponsesProvider::new"))?;
        provider.gateway_identity_headers = true;
        provider.headers.insert(
            reqwest::header::HeaderName::from_static("x-harw-session"),
            reqwest::header::HeaderValue::from_static("static-configured-value"),
        );
        let api_key = provider.api_key.clone();
        let identity = test_identity("sess-from-request", "agent-b", "planner");
        let request = provider
            .authorized_request(
                "https://example.test/v1/chat/completions",
                &api_key,
                Some(&identity),
            )
            .await
            .map_err(ctx("request builds"))?
            .build()
            .map_err(ctx("request builds"))?;
        // Statischer `[headers]`-Wert gewinnt; der Identity-Header wird nicht
        // zusätzlich gesendet (kein doppelter `x-harw-session`-Header).
        let values: Vec<_> = request.headers().get_all("x-harw-session").iter().collect();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0], "static-configured-value");
        // Header ohne statische Kollision werden weiterhin ergänzt.
        assert_eq!(
            request
                .headers()
                .get("x-harw-agent")
                .ok_or(TestError::Missing("x-harw-agent"))?,
            "agent-b"
        );
        Ok(())
    }

    #[test]
    fn invalid_credential_header_value_is_rejected_without_echo() -> TestResult {
        let Err(error) = sensitive_header_value("line\nbreak-secret") else {
            return Err(TestError::Unexpected("CR/LF rejected".to_owned()));
        };
        let ModelError::RequestFailed(message) = error else {
            return Err(TestError::Unexpected(
                "invalid header value must be a request failure".to_owned(),
            ));
        };
        assert!(!message.contains("break-secret"));
        Ok(())
    }

    #[test]
    fn redirect_decision_follows_only_same_origin_within_limit() -> TestResult {
        let url = |value: &str| reqwest::Url::parse(value).map_err(ctx("test URL"));
        let origin = vec![url("https://api.example.test/v1/messages")?];

        let same = [
            "https://api.example.test/v1/messages/",
            "https://API.example.test:443/other",
        ];
        for next in same {
            assert_eq!(
                redirect_decision(&url(next)?, &origin),
                RedirectDecision::Follow,
                "{next}"
            );
        }

        let cross = [
            "https://evil.example/v1/messages",
            "https://sub.api.example.test/v1/messages",
            "https://api.example.test:8443/v1/messages",
            "http://api.example.test/v1/messages",
            "http://api.example.test:443/v1/messages",
        ];
        for next in cross {
            assert_eq!(
                redirect_decision(&url(next)?, &origin),
                RedirectDecision::RejectCrossOrigin,
                "{next}"
            );
        }

        assert_eq!(
            redirect_decision(&url("https://api.example.test/x")?, &[]),
            RedirectDecision::RejectCrossOrigin
        );
        let tainted_chain = vec![
            url("https://api.example.test/a")?,
            url("https://evil.example/b")?,
        ];
        assert_eq!(
            redirect_decision(&url("https://api.example.test/c")?, &tainted_chain),
            RedirectDecision::RejectCrossOrigin
        );

        let chain = |len: usize| -> TestResult<Vec<reqwest::Url>> {
            Ok(vec![url("https://api.example.test/hop")?; len])
        };
        assert_eq!(
            redirect_decision(
                &url("https://api.example.test/next")?,
                &chain(MAX_REDIRECTS)?
            ),
            RedirectDecision::Follow
        );
        assert_eq!(
            redirect_decision(
                &url("https://api.example.test/next")?,
                &chain(MAX_REDIRECTS + 1)?
            ),
            RedirectDecision::RejectLimit
        );
        Ok(())
    }

    #[tokio::test]
    async fn cross_port_redirect_is_not_followed_with_api_key() -> TestResult {
        let target = TcpListener::bind("127.0.0.1:0").map_err(ctx("bind redirect target"))?;
        target
            .set_nonblocking(true)
            .map_err(ctx("non-blocking redirect target"))?;
        let target_url = format!(
            "http://{}/v1/chat/completions",
            target.local_addr().map_err(ctx("target address"))?
        );

        let listener = TcpListener::bind("127.0.0.1:0").map_err(ctx("bind redirecting server"))?;
        let base_url = format!(
            "http://{}/v1",
            listener.local_addr().map_err(ctx("server address"))?
        );
        let server = thread::spawn(move || -> TestResult<()> {
            let (mut stream, _) = listener.accept().map_err(ctx("accept provider request"))?;
            let mut request = [0_u8; 4096];
            let _read = stream
                .read(&mut request)
                .map_err(ctx("read provider request"))?;
            let response = format!(
                "HTTP/1.1 307 Temporary Redirect\r\nlocation: {target_url}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
            stream
                .write_all(response.as_bytes())
                .map_err(ctx("write redirect response"))?;
            Ok(())
        });

        let mut provider = OpenAiResponsesProvider::with_transport(
            base_url,
            "model",
            SecretString::new("sk-redirect-secret".into()),
            Transport::Chat,
        )
        .map_err(ctx("with_transport"))?;
        provider.auth_header = "x-api-key".to_owned();
        let Err(error) = provider.respond(request_with_ids(None, None)).await else {
            return Err(TestError::Unexpected(
                "cross-origin redirect must fail".to_owned(),
            ));
        };
        let ModelError::RequestFailed(message) = error else {
            return Err(TestError::Unexpected(
                "redirect rejection must be a request failure".to_owned(),
            ));
        };
        assert!(!message.contains("sk-redirect-secret"));
        server.join().map_err(join_thread_error)??;
        assert!(
            matches!(target.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock),
            "redirect target must never be contacted"
        );
        Ok(())
    }

    // Echter Netzwerk-Test: braucht einen gültigen OPENAI_API_KEY und
    // Netzwerkzugang. Deshalb `#[ignore]` — niemals im Default-Lauf.
    #[tokio::test]
    #[ignore = "requires real OPENAI_API_KEY and network access"]
    async fn test_respond_live() -> TestResult {
        let key =
            std::env::var("OPENAI_API_KEY").map_err(ctx("OPENAI_API_KEY set for live test"))?;
        let provider = OpenAiResponsesProvider::new(
            harw_provider::openai::DEFAULT_OPENAI_BASE_URL,
            "gpt-4o-mini",
            SecretString::new(key.into()),
        )
        .map_err(ctx("OpenAiResponsesProvider::new"))?;
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("Say hello in one word.");
        let request = ModelRequest {
            stream: None,
            system_prompt: "You are terse.".to_owned(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
            cancel: None,
            identity: None,
        };
        let response = provider
            .respond(request)
            .await
            .map_err(ctx("live respond"))?;
        assert!(response.message.is_some());
        Ok(())
    }
}

// Test-Fehlertyp (Bible R087/R165/R182), nur für Tests.
#[cfg(test)]
mod test_support;
