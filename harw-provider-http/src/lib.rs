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
//! Laufzeitfehler in `respond` werden in [`harw_core::ModelError`] übersetzt.
//!
//! ## Sicherheit
//! Der API-Key liegt in `secrecy::SecretString` und wird ausschließlich beim
//! Setzen des `Authorization`-Headers via `ExposeSecret` offengelegt — niemals
//! geloggt.
//!
//! # Examples
//! ```rust,no_run
//! use harw_provider_http::OpenAiResponsesProvider;
//! use secrecy::SecretString;
//!
//! let provider = OpenAiResponsesProvider::new(
//!     "https://api.openai.com/v1",
//!     "gpt-4o",
//!     SecretString::new("sk-...".into()),
//! );
//! ```

#![forbid(unsafe_code)]

pub mod anthropic;
mod error;
pub mod routing;

use harw_core::{
    ModelError, ModelFuture, ModelProvider, ModelRequest, ModelResponse, ToolCallResult,
};
use harw_provider::openai::{ContentPart, InputItem, ReasoningConfig, ResponsesRequest, ToolDef};
use harw_tools::{FunctionToolSpec, JsonSchema, ToolCall, ToolName, ToolSpec};
use harw_types::{TokenUsage, ToolCallId};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

pub use anthropic::{
    AnthropicCredential, AnthropicMessagesProvider, DEFAULT_ANTHROPIC_BASE_URL,
    anthropic_messages_url, build_messages_body, extract_anthropic_text,
};
pub use error::{HttpProviderError, HttpProviderResult};
pub use routing::RoutingModelProvider;

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
const INVALID_KEYRING_REFERENCE_REASON: &str = "invalid keyring reference";
const KEYRING_FAILURE_REASON: &str = "keyring credential unavailable";

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
/// # Concurrency
/// Reiner Aufbau; das Ergebnis ist `Send + Sync`.
pub fn build_provider(
    config: &harw_config::ResolvedConfig,
) -> HttpProviderResult<Box<dyn ModelProvider>> {
    build_provider_with_optional_resolver(config, None)
}

/// Builds configured providers with an injected synchronous `secrets:` resolver.
pub fn build_provider_with_resolver(
    config: &harw_config::ResolvedConfig,
    resolver: &dyn SecretResolver,
) -> HttpProviderResult<Box<dyn ModelProvider>> {
    build_provider_with_optional_resolver(config, Some(resolver))
}

fn build_provider_with_optional_resolver(
    config: &harw_config::ResolvedConfig,
    resolver: Option<&dyn SecretResolver>,
) -> HttpProviderResult<Box<dyn ModelProvider>> {
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
    for (name, provider) in config
        .providers
        .iter()
        .filter(|(_, provider)| provider.enabled)
        .collect::<BTreeMap<_, _>>()
    {
        let backend = match build_named_provider(name, provider, config, model, resolver) {
            Ok(backend) => backend,
            Err(error) if name != provider_name => Box::new(UnavailableProvider(error.to_string())),
            Err(error) => return Err(error),
        };
        providers.insert(name.to_owned(), backend);
    }

    Ok(Box::new(RoutingModelProvider::new(
        providers,
        provider_name,
    )?))
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
fn build_named_provider(
    provider_name: &str,
    provider: &harw_config::ProviderToml,
    config: &harw_config::ResolvedConfig,
    default_model: &str,
    resolver: Option<&dyn SecretResolver>,
) -> HttpProviderResult<Box<dyn ModelProvider>> {
    validate_endpoint(&provider.base_url)?;
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
        let (base_url, credential) =
            resolve_anthropic(provider_name, provider, &config.env_layer, resolver)?;
        let mut backend =
            AnthropicMessagesProvider::from_base(&base_url, model.to_owned(), credential);
        backend.configure(
            provider_name,
            configured_headers(provider_name, &provider.headers)?,
        );
        return Ok(Box::new(backend));
    }

    Ok(Box::new(OpenAiResponsesProvider::from_named_config(
        provider_name,
        provider,
        model,
        &config.env_layer,
        resolver,
    )?))
}

/// Löst Base-URL + Credential für den nativen Anthropic-Weg env-first auf.
fn resolve_anthropic(
    provider_name: &str,
    provider: &harw_config::ProviderToml,
    env_layer: &std::collections::BTreeMap<String, String>,
    resolver: Option<&dyn SecretResolver>,
) -> HttpProviderResult<(String, AnthropicCredential)> {
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
            resolve_secret(reference, env_layer, resolver)?
        } else {
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
        let secret = resolve_secret(secret_ref, env_layer, resolver)?;
        return Ok((
            base_url,
            if provider.auth_header.as_deref() == Some("bearer") {
                AnthropicCredential::Bearer(secret)
            } else {
                classify_anthropic_secret(secret.expose_secret().to_owned())
            },
        ));
    }
    // 2b. Setup-Token / OAuth env-first (Kompatibilität mit Claude Code).
    if let Some(token) = env_nonempty("CLAUDE_CODE_OAUTH_TOKEN") {
        return Ok((base_url, classify_anthropic_secret(token)));
    }
    // 2c. Klassischer API-Key aus der Umgebung — mit Prefix-Detection:
    //    `sk-ant-oat…` = OAuth-Token (Bearer), sonst normaler API-Key (x-api-key).
    if let Some(key) = env_nonempty("ANTHROPIC_API_KEY") {
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
    auth_header: String,
    headers: reqwest::header::HeaderMap,
    transport: Transport,
    request_timeout: Duration,
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
    #[must_use]
    pub fn new(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: SecretString,
    ) -> Self {
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
    #[must_use]
    pub fn with_transport(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: SecretString,
        transport: Transport,
    ) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.into(),
            provider_id: "openai".to_owned(),
            model: model.into(),
            api_key,
            auth_header: "bearer".into(),
            headers: reqwest::header::HeaderMap::new(),
            transport,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
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
        Self::from_named_config(provider_name, provider, model, &config.env_layer, resolver)
    }

    /// Builds one OpenAI-compatible provider from its named configuration.
    fn from_named_config(
        provider_name: &str,
        provider: &harw_config::ProviderToml,
        model: &str,
        env_layer: &BTreeMap<String, String>,
        resolver: Option<&dyn SecretResolver>,
    ) -> HttpProviderResult<Self> {
        if provider.base_url.trim().is_empty() {
            return Err(HttpProviderError::Decode(format!(
                "provider '{provider_name}' has an empty base_url"
            )));
        }
        let auth_header = provider.auth_header.as_deref().unwrap_or_else(|| {
            if provider_name == "foundry" || provider_name.starts_with("foundry-") {
                "api-key"
            } else if matches!(provider.api.as_str(), "ollama") {
                "none"
            } else {
                "bearer"
            }
        });
        let api_key = if let Some(reference) = &provider.auth {
            resolve_secret(reference, env_layer, resolver)?
        } else if auth_header == "none" {
            SecretString::new(String::new().into())
        } else {
            return Err(HttpProviderError::MissingDefault {
                what: format!("auth for provider '{provider_name}'"),
            });
        };
        let mut http_provider = Self::with_transport(
            provider.base_url.trim_end_matches('/').to_owned(),
            model.to_owned(),
            api_key,
            transport_from_api(&provider.api),
        );
        http_provider.auth_header = auth_header.to_owned();
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
        http_provider.headers = configured_headers(provider_name, &provider.headers)?;
        Ok(http_provider)
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
pub fn validate_endpoint(value: &str) -> HttpProviderResult<()> {
    let url = reqwest::Url::parse(value)
        .map_err(|_| HttpProviderError::Decode("invalid provider endpoint".into()))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || value.contains(['<', '>'])
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(HttpProviderError::Decode("provider endpoint must be an HTTP(S) base URL without placeholders, credentials or query parameters".into()));
    }
    Ok(())
}

/// Resolves deterministic provider headers before any request can be sent.
fn configured_headers(
    provider_name: &str,
    headers: &std::collections::HashMap<String, String>,
) -> HttpProviderResult<reqwest::header::HeaderMap> {
    let mut resolved = reqwest::header::HeaderMap::new();
    for (name, value) in headers.iter().collect::<BTreeMap<_, _>>() {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|error| {
            HttpProviderError::Decode(format!(
                "provider '{provider_name}' has an invalid header name: {error}"
            ))
        })?;
        let value = reqwest::header::HeaderValue::from_str(value).map_err(|error| {
            HttpProviderError::Decode(format!(
                "provider '{provider_name}' has an invalid header value: {error}"
            ))
        })?;
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
/// ist; andernfalls wird der Env-Layer konsultiert. `keyring:` erwartet exakt
/// `service/account`; `secrets:` wird an den injizierten Resolver delegiert.
///
/// # Arguments
/// - `secret_ref` (`&harw_config::SecretRef`): Zu lösende Referenz.
/// - `env_layer` (`&BTreeMap<String, String>`): Geladener Env-Layer aus
///   `~/.harw/.env` (aus [`harw_config::ResolvedConfig::env_layer`]).
fn resolve_secret(
    secret_ref: &harw_config::SecretRef,
    env_layer: &std::collections::BTreeMap<String, String>,
    resolver: Option<&dyn SecretResolver>,
) -> HttpProviderResult<SecretString> {
    use harw_config::SecretRef;
    let reference = secret_ref.as_ref_string();
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
            let raw = std::fs::read_to_string(path).map_err(|error| {
                HttpProviderError::UnresolvedCredential {
                    reference: reference.clone(),
                    reason: format!("file unreadable: {error}"),
                }
            })?;
            validate_resolved_secret(reference, SecretString::new(raw.trim().to_owned().into()))
        }
        SecretRef::FileJson { path, pointer } => {
            let raw = std::fs::read_to_string(path).map_err(|error| {
                HttpProviderError::UnresolvedCredential {
                    reference: reference.clone(),
                    reason: format!("file unreadable: {error}"),
                }
            })?;
            let doc: serde_json::Value = serde_json::from_str(&raw).map_err(|error| {
                HttpProviderError::UnresolvedCredential {
                    reference: reference.clone(),
                    reason: format!("invalid JSON: {error}"),
                }
            })?;
            let value = doc
                .pointer(pointer)
                .and_then(|value| value.as_str())
                .ok_or_else(|| HttpProviderError::UnresolvedCredential {
                    reference: reference.clone(),
                    reason: format!("JSON pointer '{pointer}' missing or not a string"),
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

/// Parses a `keyring:` payload in the required `service/account` form.
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
    tools
        .iter()
        .map(|spec| match spec {
            ToolSpec::Function(f) => {
                let parameters = serde_json::to_value(strict_parameters(f))
                    .unwrap_or_else(|_| serde_json::json!({}));
                ToolDef {
                    kind: "function".to_owned(),
                    name: f.name.as_str().to_owned(),
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
                    name,
                    arguments: arguments.to_string(),
                });
            }
            harw_core::ModelMessage::ToolResult { call_id, result } => {
                let output = match result {
                    ToolCallResult::Success { value } => value.to_string(),
                    ToolCallResult::Error { message } => message,
                };
                input.push(InputItem::FunctionCallOutput {
                    call_id: call_id.to_string(),
                    output,
                });
            }
        }
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
            summary: None,
        });
    }
    req
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
    tools
        .iter()
        .map(|spec| match spec {
            ToolSpec::Function(f) => serde_json::json!({
                "type": "function",
                "function": {
                    "name": f.name.as_str(),
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
                            "name": name,
                            "arguments": arguments.to_string(),
                        },
                    }),
                );
            }
            harw_core::ModelMessage::ToolResult { call_id, result } => {
                let content = match result {
                    ToolCallResult::Success { value } => value.to_string(),
                    ToolCallResult::Error { message } => message,
                };
                messages.push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": call_id.as_str(),
                    "content": content,
                }));
            }
        }
    }

    let mut body = serde_json::json!({ "model": model, "messages": messages });
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(build_chat_tools(&request.tools));
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
    let (input_key, output_key, reasoning_path, cached_path) = match transport {
        Transport::Responses => (
            "input_tokens",
            "output_tokens",
            ["output_tokens_details", "reasoning_tokens"],
            ["input_tokens_details", "cached_tokens"],
        ),
        Transport::Chat => (
            "prompt_tokens",
            "completion_tokens",
            ["completion_tokens_details", "reasoning_tokens"],
            ["prompt_tokens_details", "cached_tokens"],
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

    TokenUsage {
        input_tokens,
        output_tokens,
        reasoning_tokens,
        cached_tokens,
    }
}

impl ModelProvider for OpenAiResponsesProvider {
    fn respond<'a>(&'a self, request: ModelRequest) -> ModelFuture<'a> {
        Box::pin(async move {
            let model = self.selected_model(&request)?;
            let (url, wire) = match self.transport {
                Transport::Responses => {
                    tracing::debug!(model, "sending responses request");
                    (
                        format!("{}/responses", self.base_url),
                        serde_json::to_value(build_request(model, &request))?,
                    )
                }
                Transport::Chat => {
                    tracing::debug!(model, "sending chat request");
                    (
                        format!("{}/chat/completions", self.base_url),
                        build_chat_body(&request, model),
                    )
                }
            };

            let builder = self.client.post(&url).headers(self.headers.clone());
            let builder = match self.auth_header.as_str() {
                "none" => builder,
                "api-key" | "x-api-key" => {
                    builder.header(self.auth_header.as_str(), self.api_key.expose_secret())
                }
                _ => builder.bearer_auth(self.api_key.expose_secret()),
            };
            let response = builder
                .json(&wire)
                .timeout(self.request_timeout)
                .send()
                .await
                .map_err(|error| {
                    ModelError::RequestFailed(HttpProviderError::from(error).to_string())
                })?;

            let status = response.status();
            let retry_after_header = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned);
            let request_id = provider_request_id(response.headers());
            let body = response.text().await.map_err(|error| {
                ModelError::RequestFailed(HttpProviderError::from(error).to_string())
            })?;

            if status.as_u16() == 429 {
                let retry_after = parse_retry_after(retry_after_header.as_deref(), &body);
                return Err(ModelError::RateLimited {
                    retry_after_secs: retry_after.as_secs(),
                    message: sanitized_provider_error(
                        status.as_u16(),
                        request_id.as_deref(),
                        &body,
                    ),
                });
            }

            if !status.is_success() {
                return Err(ModelError::RequestFailed(sanitized_provider_error(
                    status.as_u16(),
                    request_id.as_deref(),
                    &body,
                )));
            }

            let value: Value = serde_json::from_str(&body)?;
            let text = match self.transport {
                Transport::Responses => extract_assistant_text(&value),
                Transport::Chat => extract_chat_content(&value),
            };
            let tool_calls = extract_openai_tool_calls(&value, self.transport)
                .map_err(|error| ModelError::RequestFailed(error.to_string()))?;
            if text.is_none() && tool_calls.is_empty() {
                return Err(ModelError::EmptyResponse);
            }

            Ok(ModelResponse {
                message: text,
                tool_calls,
                usage: extract_openai_usage(&value, self.transport),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc::{self, Receiver};
    use std::thread;

    fn mock_chat_server(request_count: usize) -> (String, Receiver<Value>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let base_url = format!("http://{}", listener.local_addr().expect("mock address"));
        let (sender, receiver) = mpsc::channel();
        let handle = thread::spawn(move || {
            for _ in 0..request_count {
                let (mut stream, _) = listener.accept().expect("accept mock request");
                let mut bytes = Vec::new();
                let mut buffer = [0_u8; 4096];
                let (header_end, content_length) = loop {
                    let read = stream.read(&mut buffer).expect("read mock request");
                    assert!(
                        read > 0,
                        "mock client closed connection before request completed"
                    );
                    bytes.extend_from_slice(&buffer[..read]);
                    let Some(header_end) =
                        bytes.windows(4).position(|window| window == b"\r\n\r\n")
                    else {
                        continue;
                    };
                    let header_end = header_end + 4;
                    let headers = std::str::from_utf8(&bytes[..header_end])
                        .expect("mock request headers are UTF-8");
                    let content_length = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .expect("content length header")
                        .parse::<usize>()
                        .expect("numeric content length");
                    break (header_end, content_length);
                };
                while bytes.len() < header_end + content_length {
                    let read = stream.read(&mut buffer).expect("read mock request body");
                    assert!(
                        read > 0,
                        "mock client closed connection before body completed"
                    );
                    bytes.extend_from_slice(&buffer[..read]);
                }
                let body: Value =
                    serde_json::from_slice(&bytes[header_end..header_end + content_length])
                        .expect("mock request JSON");
                sender.send(body).expect("report mock request");
                stream
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 44\r\nconnection: close\r\n\r\n{\"choices\":[{\"message\":{\"content\":\"mock\"}}]}",
                    )
                    .expect("write mock response");
            }
        });
        (base_url, receiver, handle)
    }

    fn mock_json_response_server(response: Value) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let base_url = format!("http://{}", listener.local_addr().expect("mock address"));
        let response = serde_json::to_vec(&response).expect("serialize mock response");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept mock request");
            let mut request_buffer = [0_u8; 4096];
            // Der Mock verwirft die Anfrage bewusst — er antwortet immer
            // dasselbe. Gelesen werden muss trotzdem, sonst schließt der Server
            // die Verbindung, bevor der Client seine Anfrage losgeworden ist.
            // Die gelesene Menge wird ausdrücklich verworfen (`let _ =`) statt
            // ignoriert: `read` liefert bei einem Socket regelmäßig weniger als
            // den Puffer, und ein `read_exact` würde hier auf 4096 Bytes warten,
            // die nie kommen.
            let _read = stream.read(&mut request_buffer).expect("read mock request");
            let headers = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                response.len()
            );
            stream
                .write_all(headers.as_bytes())
                .expect("write response headers");
            stream.write_all(&response).expect("write response body");
        });
        (base_url, handle)
    }

    async fn assert_respond_rejects_tool_call(
        transport: Transport,
        response: Value,
        expected_message: &str,
    ) {
        let (base_url, server) = mock_json_response_server(response);
        let provider = OpenAiResponsesProvider::with_transport(
            base_url,
            "gpt-test",
            SecretString::new("sk-secret".into()),
            transport,
        );

        let error = provider
            .respond(request_with_ids(None, None))
            .await
            .expect_err("malformed tool call must fail closed");
        let ModelError::RequestFailed(message) = error else {
            panic!("invalid tool-call arguments must be a request failure");
        };
        assert_eq!(message, expected_message);
        assert!(!message.contains("private-provider-arguments"));
        server.join().expect("mock server completes");
    }

    fn configured_provider(
        name: &str,
        base_url: String,
        models: Vec<&str>,
        auth_env: &str,
    ) -> harw_config::ProviderToml {
        harw_config::ProviderToml {
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
        }
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
    fn injected_resolver_enables_secrets_references_for_openai_and_anthropic() {
        let resolver = FakeSecretResolver {
            result: Ok(SecretString::new("resolved-secret".into())),
        };

        let config = secrets_config("openai-chat");
        let provider = OpenAiResponsesProvider::from_config_with_resolver(&config, &resolver)
            .expect("injected resolver constructs OpenAI-compatible provider");
        assert_eq!(provider.api_key.expose_secret(), "resolved-secret");

        let anthropic = secrets_provider_config("anthropic-messages");
        let (_, credential) = resolve_anthropic(
            "anthropic",
            &anthropic,
            &std::collections::BTreeMap::new(),
            Some(&resolver),
        )
        .expect("injected resolver constructs Anthropic credential");
        let AnthropicCredential::ApiKey(secret) = credential else {
            panic!("ordinary resolved secret must be an Anthropic API key");
        };
        assert_eq!(secret.expose_secret(), "resolved-secret");

        build_provider_with_resolver(&config, &resolver)
            .expect("injected resolver constructs configured provider router");
    }

    #[test]
    fn build_provider_without_resolver_rejects_secrets_references() {
        let error = match build_provider(&secrets_config("openai-chat")) {
            Ok(_) => panic!("secrets references remain fail-closed without a resolver"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            HttpProviderError::UnsupportedCredentialReference { .. }
        ));
    }

    #[test]
    fn resolver_failure_diagnostic_does_not_include_secret_value() {
        let secret = "resolver-private-secret";
        let resolver = FakeSecretResolver {
            result: Err(format!("resolver unavailable: {secret}")),
        };
        let error = match build_provider_with_resolver(&secrets_config("openai-chat"), &resolver) {
            Ok(_) => panic!("resolver failure must fail construction"),
            Err(error) => error,
        };

        assert!(matches!(
            &error,
            HttpProviderError::UnresolvedCredential { reason, .. }
                if reason == SECRET_RESOLVER_FAILURE_REASON
        ));
        assert!(!error.to_string().contains(secret));
    }

    #[test]
    fn resolver_failure_diagnostic_is_redacted_for_anthropic() {
        let secret = "anthropic-resolver-private-secret";
        let resolver = FakeSecretResolver {
            result: Err(format!("resolver unavailable: {secret}")),
        };
        let provider = secrets_provider_config("anthropic-messages");
        let error = match resolve_anthropic(
            "anthropic",
            &provider,
            &std::collections::BTreeMap::new(),
            Some(&resolver),
        ) {
            Ok(_) => panic!("resolver failure must fail Anthropic construction"),
            Err(error) => error,
        };

        assert!(matches!(
            &error,
            HttpProviderError::UnresolvedCredential { reason, .. }
                if reason == SECRET_RESOLVER_FAILURE_REASON
        ));
        assert!(!error.to_string().contains(secret));
    }

    #[test]
    fn empty_resolver_credentials_are_rejected_for_openai_and_anthropic() {
        for credential in ["", " \t\n"] {
            let resolver = FakeSecretResolver {
                result: Ok(SecretString::new(credential.to_owned().into())),
            };
            let config = secrets_config("openai-chat");
            let openai_error =
                match OpenAiResponsesProvider::from_config_with_resolver(&config, &resolver) {
                    Ok(_) => panic!("empty OpenAI resolver credential must fail construction"),
                    Err(error) => error,
                };
            assert!(matches!(
                openai_error,
                HttpProviderError::UnresolvedCredential { reason, .. }
                    if reason == EMPTY_CREDENTIAL_REASON
            ));

            let provider = secrets_provider_config("anthropic-messages");
            let anthropic_error = match resolve_anthropic(
                "anthropic",
                &provider,
                &std::collections::BTreeMap::new(),
                Some(&resolver),
            ) {
                Ok(_) => panic!("empty Anthropic resolver credential must fail construction"),
                Err(error) => error,
            };
            assert!(matches!(
                anthropic_error,
                HttpProviderError::UnresolvedCredential { reason, .. }
                    if reason == EMPTY_CREDENTIAL_REASON
            ));
        }
    }

    #[tokio::test]
    async fn router_accepts_onboarded_secondary_provider_with_model_files() {
        let (url, requests, server) = mock_chat_server(1);
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
            let model =
                serde_json::from_value(serde_json::json!({"id": id, "provider": owner})).unwrap();
            config.models.insert(id.into(), model);
        }
        let router = build_provider(&config).expect("onboarding model files must suffice");
        router
            .respond(request_with_ids(None, Some("secondary")))
            .await
            .unwrap();
        let request = requests.recv().unwrap();
        assert_eq!(
            request.get("model").and_then(Value::as_str),
            Some("a-model")
        );
        server.join().unwrap();
    }

    #[tokio::test]
    async fn build_provider_routes_named_backends_to_distinct_endpoints_and_models() {
        let (primary_url, primary_requests, primary_server) = mock_chat_server(1);
        let (secondary_url, secondary_requests, secondary_server) = mock_chat_server(2);
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

        let provider = build_provider(&config).expect("construct configured router");
        provider
            .respond(request_with_ids(Some("primary-request-model"), None))
            .await
            .expect("default provider response");
        provider
            .respond(request_with_ids(None, Some("secondary")))
            .await
            .expect("secondary fallback response");
        provider
            .respond(request_with_ids(
                Some("secondary-request-model"),
                Some("secondary"),
            ))
            .await
            .expect("secondary override response");

        assert_eq!(
            primary_requests
                .recv()
                .expect("primary request")
                .get("model")
                .and_then(Value::as_str),
            Some("primary-request-model")
        );
        assert_eq!(
            secondary_requests
                .recv()
                .expect("secondary fallback request")
                .get("model")
                .and_then(Value::as_str),
            Some("secondary-fallback")
        );
        assert_eq!(
            secondary_requests
                .recv()
                .expect("secondary override request")
                .get("model")
                .and_then(Value::as_str),
            Some("secondary-request-model")
        );
        primary_server
            .join()
            .expect("primary mock server completes");
        secondary_server
            .join()
            .expect("secondary mock server completes");
    }

    fn request_with_ids(model_id: Option<&str>, provider_id: Option<&str>) -> ModelRequest {
        ModelRequest {
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history: harw_core::ConversationHistory::new(),
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: model_id.map(Into::into),
            provider_id: provider_id.map(Into::into),
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
    fn test_new_sets_fields() {
        let provider = OpenAiResponsesProvider::new(
            "https://example.test/v1",
            "gpt-test",
            SecretString::new("sk-secret".into()),
        );
        assert_eq!(provider.base_url, "https://example.test/v1");
        assert_eq!(provider.model, "gpt-test");
        assert_eq!(provider.api_key.expose_secret(), "sk-secret");
        assert_eq!(provider.request_timeout, DEFAULT_REQUEST_TIMEOUT);
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
    fn test_resolve_secret_rejects_secrets_reference_without_disclosing_it() {
        let reference = "tenant/provider-token-with-secret-metadata";
        let error = resolve_secret(
            &harw_config::SecretRef::Secrets(reference.to_owned()),
            &std::collections::BTreeMap::new(),
            None,
        )
        .expect_err("secrets references are unsupported by the HTTP provider");

        assert!(matches!(
            &error,
            HttpProviderError::UnsupportedCredentialReference { .. }
        ));
        assert!(!error.to_string().contains(reference));
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
    fn test_provider_request_id_prefers_openai_header_and_bounds_value() {
        let mut headers = reqwest::header::HeaderMap::new();
        let long_request_id = "r".repeat(140);
        headers.insert(
            "x-request-id",
            long_request_id.parse().expect("valid header value"),
        );
        headers.insert(
            "request-id",
            reqwest::header::HeaderValue::from_static("fallback"),
        );

        let request_id = provider_request_id(&headers).expect("request ID");

        assert_eq!(request_id.len(), 128);
        assert!(request_id.chars().all(|character| character == 'r'));
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
    fn test_new_defaults_to_responses_transport() {
        let provider = OpenAiResponsesProvider::new(
            "https://example.test/v1",
            "gpt-test",
            SecretString::new("sk-secret".into()),
        );
        assert_eq!(provider.transport, Transport::Responses);
    }

    #[test]
    fn test_selected_model_uses_requested_model_for_compatible_provider() {
        let provider = OpenAiResponsesProvider::new(
            "https://example.test/v1",
            "configured-model",
            SecretString::new("sk-secret".into()),
        );
        let request = request_with_ids(Some("requested-model"), Some("openai"));

        assert_eq!(
            provider.selected_model(&request).unwrap(),
            "requested-model"
        );
    }

    #[test]
    fn test_selected_model_preserves_configured_default_for_empty_identifiers() {
        let provider = OpenAiResponsesProvider::new(
            "https://example.test/v1",
            "configured-model",
            SecretString::new("sk-secret".into()),
        );
        let request = request_with_ids(Some(""), Some(""));

        assert_eq!(
            provider.selected_model(&request).unwrap(),
            "configured-model"
        );
    }

    #[test]
    fn test_selected_model_rejects_provider_mismatch() {
        let provider = OpenAiResponsesProvider::new(
            "https://example.test/v1",
            "configured-model",
            SecretString::new("sk-secret".into()),
        );
        let request = request_with_ids(Some("requested-model"), Some("anthropic"));

        assert!(matches!(
            provider.selected_model(&request),
            Err(ModelError::RequestFailed(message))
                if message.contains("anthropic") && message.contains("openai")
        ));
    }

    #[tokio::test]
    async fn test_respond_rejects_provider_mismatch_before_http_request() {
        let provider = OpenAiResponsesProvider::new(
            "http://127.0.0.1:1/v1",
            "configured-model",
            SecretString::new("sk-secret".into()),
        );

        let error = provider
            .respond(request_with_ids(Some("requested-model"), Some("anthropic")))
            .await
            .expect_err("a request for another provider must fail before HTTP");

        assert!(matches!(
            error,
            ModelError::RequestFailed(message)
                if message.contains("anthropic") && message.contains("openai")
        ));
    }

    #[test]
    fn test_build_chat_body_shape() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("Hi there");
        let request = ModelRequest {
            system_prompt: "You are terse.".to_owned(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
        };

        let body = build_chat_body(&request, "gpt-4o-mini");
        assert_eq!(
            body.get("model").and_then(Value::as_str),
            Some("gpt-4o-mini")
        );
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .expect("messages array");
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
    }

    #[test]
    fn test_build_chat_body_omits_empty_system() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("Only user");
        let request = ModelRequest {
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
        };

        let body = build_chat_body(&request, "m");
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .expect("messages array");
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].get("role").and_then(Value::as_str),
            Some("user")
        );
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
                input_tokens: 12,
                output_tokens: 34,
                reasoning_tokens: Some(7),
                cached_tokens: Some(3),
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
                input_tokens: 20,
                output_tokens: 5,
                reasoning_tokens: Some(2),
                cached_tokens: Some(1),
            }
        );
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
    fn test_build_request_sets_reasoning_when_effort_present() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("Hi there");
        let request = ModelRequest {
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: Some(harw_types::ReasoningEffort::Minimal),
            model_id: None,
            provider_id: None,
        };

        let req = build_request("gpt-test", &request);
        let reasoning = req.reasoning.expect("reasoning must be set");
        assert_eq!(reasoning.effort.as_deref(), Some("minimal"));
        assert_eq!(reasoning.summary, None);
    }

    #[test]
    fn test_build_request_omits_reasoning_when_effort_absent() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("Hi there");
        let request = ModelRequest {
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
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
    fn test_build_responses_tools_strict_true_required_includes_every_property() {
        let tools = vec![tool_spec_with_optional_field(true)];
        let defs = build_responses_tools(&tools);
        assert_eq!(defs.len(), 1);
        assert!(defs[0].strict);
        let required = defs[0]
            .parameters
            .get("required")
            .and_then(Value::as_array)
            .expect("required array present");
        let required: Vec<&str> = required.iter().filter_map(Value::as_str).collect();
        assert!(required.contains(&"path"));
        assert!(required.contains(&"max_bytes"));
        assert_eq!(required.len(), 2);
    }

    #[test]
    fn test_build_responses_tools_strict_false_leaves_parameters_unchanged() {
        let tools = vec![tool_spec_with_optional_field(false)];
        let defs = build_responses_tools(&tools);
        assert_eq!(defs.len(), 1);
        assert!(!defs[0].strict);
        let required = defs[0]
            .parameters
            .get("required")
            .and_then(Value::as_array)
            .expect("required array present");
        let required: Vec<&str> = required.iter().filter_map(Value::as_str).collect();
        assert_eq!(required, vec!["path"]);
        assert!(
            defs[0]
                .parameters
                .get("additionalProperties")
                .is_none()
        );
    }

    #[test]
    fn test_build_chat_tools_strict_true_required_includes_every_property() {
        let tools = vec![tool_spec_with_optional_field(true)];
        let defs = build_chat_tools(&tools);
        assert_eq!(defs.len(), 1);
        let required = defs[0]["function"]["parameters"]["required"]
            .as_array()
            .expect("required array present");
        let required: Vec<&str> = required.iter().filter_map(Value::as_str).collect();
        assert!(required.contains(&"path"));
        assert!(required.contains(&"max_bytes"));
        assert_eq!(required.len(), 2);
        assert_eq!(
            defs[0]["function"]["parameters"]["additionalProperties"],
            Value::Bool(false)
        );
    }

    #[test]
    fn test_build_chat_tools_strict_false_leaves_parameters_unchanged() {
        let tools = vec![tool_spec_with_optional_field(false)];
        let defs = build_chat_tools(&tools);
        assert_eq!(defs.len(), 1);
        let required = defs[0]["function"]["parameters"]["required"]
            .as_array()
            .expect("required array present");
        let required: Vec<&str> = required.iter().filter_map(Value::as_str).collect();
        assert_eq!(required, vec!["path"]);
        assert!(defs[0]["function"]["parameters"]["additionalProperties"].is_null());
    }

    #[test]
    fn test_build_request_includes_tools_when_present() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("weather?");
        let request = ModelRequest {
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: vec![sample_tool_spec()],
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
        };
        let req = build_request("gpt-test", &request);
        assert_eq!(req.tools.len(), 1);
        assert_eq!(req.tools[0].kind, "function");
        assert_eq!(req.tools[0].name, "get_weather");
    }

    #[test]
    fn test_build_request_maps_tool_call_to_function_call_item() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_tool_call(
            harw_types::ToolCallId::from_str("call_1"),
            "get_weather",
            serde_json::json!({"location": "Paris"}),
        );
        let request = ModelRequest {
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
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
                let parsed: Value = serde_json::from_str(arguments).expect("valid JSON");
                assert_eq!(
                    parsed.get("location").and_then(Value::as_str),
                    Some("Paris")
                );
            }
            other => panic!("expected FunctionCall item, got {other:?}"),
        }
    }

    #[test]
    fn test_build_chat_body_includes_tools_when_present() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("weather?");
        let request = ModelRequest {
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: vec![sample_tool_spec()],
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
        };
        let body = build_chat_body(&request, "gpt-4o-mini");
        let tools = body
            .get("tools")
            .and_then(Value::as_array)
            .expect("tools array");
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
    }

    #[test]
    fn test_build_chat_body_omits_tools_when_empty() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("hi");
        let request = ModelRequest {
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
        };
        let body = build_chat_body(&request, "gpt-4o-mini");
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn test_build_chat_body_maps_tool_call_to_tool_calls_array() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_tool_call(
            harw_types::ToolCallId::from_str("call_1"),
            "get_weather",
            serde_json::json!({"location": "Paris"}),
        );
        let request = ModelRequest {
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
        };
        let body = build_chat_body(&request, "gpt-4o-mini");
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .expect("messages");
        assert_eq!(
            messages[0].get("role").and_then(Value::as_str),
            Some("assistant")
        );
        let tool_calls = messages[0]
            .get("tool_calls")
            .and_then(Value::as_array)
            .expect("tool_calls array");
        assert_eq!(
            tool_calls[0].get("id").and_then(Value::as_str),
            Some("call_1")
        );
        assert_eq!(
            tool_calls[0].get("type").and_then(Value::as_str),
            Some("function")
        );
        let function = tool_calls[0].get("function").expect("function object");
        assert_eq!(
            function.get("name").and_then(Value::as_str),
            Some("get_weather")
        );
        let arguments_str = function
            .get("arguments")
            .and_then(Value::as_str)
            .expect("arguments string");
        let parsed: Value = serde_json::from_str(arguments_str).expect("valid JSON");
        assert_eq!(
            parsed.get("location").and_then(Value::as_str),
            Some("Paris")
        );
    }

    #[test]
    fn test_build_chat_body_maps_tool_result_to_tool_role_with_call_id() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_tool_result(
            harw_types::ToolCallId::from_str("call_1"),
            ToolCallResult::success(serde_json::json!({"temp": 20})),
            5,
        );
        let request = ModelRequest {
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
        };
        let body = build_chat_body(&request, "gpt-4o-mini");
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .expect("messages");
        assert_eq!(
            messages[0].get("role").and_then(Value::as_str),
            Some("tool")
        );
        assert_eq!(
            messages[0].get("tool_call_id").and_then(Value::as_str),
            Some("call_1")
        );
    }

    #[test]
    fn test_build_chat_body_groups_parallel_tool_calls_into_one_assistant_message() {
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
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
        };
        let body = build_chat_body(&request, "gpt-4o-mini");
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .expect("messages");

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
            .expect("tool_calls array");
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
    }

    #[test]
    fn test_build_chat_body_single_tool_call_stays_one_assistant_message() {
        let mut history = harw_core::ConversationHistory::new();
        history.push_tool_call(
            harw_types::ToolCallId::from_str("call_1"),
            "get_weather",
            serde_json::json!({"location": "Paris"}),
        );
        let request = ModelRequest {
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
        };
        let body = build_chat_body(&request, "gpt-4o-mini");
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .expect("messages");
        assert_eq!(messages.len(), 1);
        let tool_calls = messages[0]
            .get("tool_calls")
            .and_then(Value::as_array)
            .expect("tool_calls array");
        assert_eq!(tool_calls.len(), 1);
    }

    #[test]
    fn test_build_chat_body_separate_tool_rounds_stay_separate_assistant_messages() {
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
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
        };
        let body = build_chat_body(&request, "gpt-4o-mini");
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .expect("messages");

        // call_a, result_a, call_b, result_b: four separate messages, no grouping.
        assert_eq!(messages.len(), 4);
        assert_eq!(
            messages[0].get("role").and_then(Value::as_str),
            Some("assistant")
        );
        let first_calls = messages[0]
            .get("tool_calls")
            .and_then(Value::as_array)
            .expect("first tool_calls array");
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
            .expect("second tool_calls array");
        assert_eq!(second_calls.len(), 1);
        assert_eq!(
            messages[3].get("role").and_then(Value::as_str),
            Some("tool")
        );
    }

    #[test]
    fn test_extract_responses_tool_calls_parses_function_call_items() {
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
            .expect("valid Responses tool-call arguments");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id.as_str(), "call_1");
        assert_eq!(calls[0].name.as_str(), "get_weather");
        assert_eq!(
            calls[0].arguments.get("location").and_then(Value::as_str),
            Some("Paris")
        );
    }

    #[test]
    fn test_extract_chat_tool_calls_parses_message_tool_calls() {
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
            .expect("valid Chat tool-call arguments");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id.as_str(), "call_1");
        assert_eq!(calls[0].name.as_str(), "get_weather");
        assert_eq!(
            calls[0].arguments.get("location").and_then(Value::as_str),
            Some("Paris")
        );
    }

    #[test]
    fn test_extract_responses_tool_calls_rejects_malformed_item_with_valid_sibling() {
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
            let error = extract_openai_tool_calls(&body, Transport::Responses)
                .expect_err("a malformed function_call must reject its valid sibling");
            assert_eq!(error, expected_error);
        }
    }

    #[test]
    fn test_extract_chat_tool_calls_rejects_malformed_item_with_valid_sibling() {
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
            let error = extract_openai_tool_calls(&body, Transport::Chat)
                .expect_err("a malformed tool_calls item must reject its valid sibling");
            assert_eq!(error, expected_error);
        }
    }

    #[test]
    fn test_extract_openai_tool_calls_empty_when_absent() {
        assert!(
            extract_openai_tool_calls(&serde_json::json!({"output": []}), Transport::Responses)
                .expect("empty Responses output is valid")
                .is_empty()
        );
        assert!(
            extract_openai_tool_calls(&serde_json::json!({"choices": []}), Transport::Chat)
                .expect("empty Chat choices are valid")
                .is_empty()
        );
    }

    #[test]
    fn test_extract_openai_tool_calls_preserves_empty_object_arguments() {
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
            .expect("empty object is valid Responses arguments");
        let chat_calls = extract_openai_tool_calls(&chat_body, Transport::Chat)
            .expect("empty object is valid Chat arguments");

        assert_eq!(responses_calls[0].arguments, serde_json::json!({}));
        assert_eq!(chat_calls[0].arguments, serde_json::json!({}));
    }

    #[tokio::test]
    async fn test_respond_rejects_malformed_responses_tool_call_arguments() {
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
        .await;
    }

    #[tokio::test]
    async fn test_respond_rejects_missing_responses_tool_call_arguments() {
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
        .await;
    }

    #[tokio::test]
    async fn test_respond_rejects_malformed_chat_tool_call_arguments() {
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
        .await;
    }

    #[tokio::test]
    async fn test_respond_rejects_missing_chat_tool_call_arguments() {
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
        .await;
    }

    #[tokio::test]
    async fn test_respond_maps_malformed_responses_tool_call_to_generic_model_error() {
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
        .await;
    }

    #[tokio::test]
    async fn test_respond_maps_malformed_chat_tool_call_to_generic_model_error() {
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
        .await;
    }

    // Echter Netzwerk-Test: braucht einen gültigen OPENAI_API_KEY und
    // Netzwerkzugang. Deshalb `#[ignore]` — niemals im Default-Lauf.
    #[tokio::test]
    #[ignore = "requires real OPENAI_API_KEY and network access"]
    async fn test_respond_live() {
        let key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY set for live test");
        let provider = OpenAiResponsesProvider::new(
            harw_provider::openai::DEFAULT_OPENAI_BASE_URL,
            "gpt-4o-mini",
            SecretString::new(key.into()),
        );
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("Say hello in one word.");
        let request = ModelRequest {
            system_prompt: "You are terse.".to_owned(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: None,
            model_id: None,
            provider_id: None,
        };
        let response = provider.respond(request).await.expect("live respond");
        assert!(response.message.is_some());
    }
}
