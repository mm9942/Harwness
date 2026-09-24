//! Nativer Anthropic-Messages-Transport (Claude-nativ) — alternativer Weg zum
//! OpenAI-kompatiblen Pfad.
//!
//! ## Verantwortung
//! Diese Datei besitzt die konkrete, netzwerkgestützte Umsetzung des
//! Anthropic-`/v1/messages`-Schemas für Claude-Modelle. Derselbe Transport
//! bedient **drei Ziele**, die sich nur in Endpoint-URL und Auth-Header
//! unterscheiden:
//! 1. Anthropic-direkt mit API-Key (`x-api-key`).
//! 2. Anthropic-direkt mit OAuth-/Setup-Token (`Authorization: Bearer` +
//!    `anthropic-beta: oauth-2025-04-20`).
//! 3. Azure-AI-Foundry-Gateway zu Claude mit `x-api-key`.
//!
//! Es ist der **native** Weg (echtes Messages-Wire-Schema), nicht der
//! OpenAI-Chat-Shim.
//!
//! ## Nebenläufigkeit
//! [`AnthropicMessagesProvider`] ist `Send + Sync` (`reqwest::Client` teilt
//! sich intern und klont günstig). `respond` liefert ein `Box::pin`-Future.
//! `build_named_provider` (in `lib.rs`) installiert über
//! [`AnthropicMessagesProvider::configure_concurrency`] denselben
//! [`crate::DynamicConcurrencyLimiter`] wie der OpenAI-kompatible Pfad —
//! ein Permit pro Request, gehalten bis der Response-Body vollständig
//! gelesen ist — sowie über [`AnthropicMessagesProvider::configure_rate_limit`]
//! denselben [`crate::rate_limiter::ProviderRateLimiter`] (Header-Pacing plus
//! HTTP-429-Zähler via `record_rate_limited`). Beide sind über
//! `impl `[`crate::ProviderLoadControl`]` for AnthropicMessagesProvider`
//! beobacht- und live verstellbar (Anthropic-Parität zu
//! [`crate::OpenAiResponsesProvider`], W6b).
//!
//! ## Sicherheit
//! Das Credential liegt in `secrecy::SecretString` und wird ausschließlich beim
//! Setzen des Auth-Headers via `ExposeSecret` offengelegt — niemals geloggt.

use crate::tool_names::ToolNameCodec;
use harw_core::model::StopReason;
use harw_core::{
    ModelError, ModelFuture, ModelProvider, ModelRequest, ModelResponse, ToolCallResult,
};
use harw_protocol::OpaqueReasoning;
use harw_tools::{ToolCall, ToolName, ToolSpec};
use harw_types::{TokenUsage, ToolCallId};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

const INVALID_CUSTOM_HEADERS_ERROR: &str = "invalid ANTHROPIC_CUSTOM_HEADERS configuration";

/// Pflicht-Version des Anthropic-Wire-Protokolls.
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Beta-Header, der für OAuth-/Setup-Token-Auth gegen `/v1/messages` nötig ist.
pub const ANTHROPIC_OAUTH_BETA: &str = "oauth-2025-04-20";
/// Standard-Basis eines direkten Anthropic-Providers.
pub const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";
/// Offizieller Host der Anthropic-API (Host von [`DEFAULT_ANTHROPIC_BASE_URL`]).
/// Nur an diesen Host gehen implizite Umgebungs-Credentials
/// (`CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`).
pub(crate) const ANTHROPIC_API_HOST: &str = "api.anthropic.com";
/// Default-Ausgabe-Token-Obergrenze, falls die Anfrage keine vorgibt
/// (`ModelRequest::max_output_tokens == None`). Wird wie ein explizit
/// angeforderter Wert über `anthropic_caps::clamp_max_tokens` auf das
/// Ausgabe-Limit des Modells geklemmt.
pub const DEFAULT_MAX_TOKENS: u32 = 16_384;
/// Byte-Deckel je gerendertem Tool-Ergebnis im Messages-Body, wenn der
/// Request keinen setzt (`ModelRequest::tool_result_max_bytes == None`).
pub const DEFAULT_TOOL_RESULT_MAX_BYTES: usize = 64 * 1024;

/// Warnhinweis für die Nutzung eines Abo-OAuth-/Setup-Tokens (Claude Free/Pro/Max)
/// mit `harw` statt eines Console-API-Keys.
///
/// # Description
/// Laut Anthropics Nutzungsbedingungen ist Abo-OAuth für Claude Code und
/// native Anthropic-Apps vorgesehen; Drittanbieter-Tools sollen API-Keys aus
/// der Claude Console nutzen. Nach Sekundärquellen (Presse/Community, Stand
/// 2026; keine direkt verifizierbare Anthropic-Primärquelle mit Datum) hat
/// Anthropic diese Regel zwischen Januar und April 2026 stufenweise
/// **serverseitig durchgesetzt** — Drittanbieter-Harnesses wie `harw` können
/// dadurch bereits ohne Vorwarnung mit Ablehnungen/Rate-Limits statt nur
/// einer ToS-Warnung konfrontiert sein. Diese Konstante beschreibt den
/// berichteten Durchsetzungsstand so vorsichtig wie die Beleglage es
/// zulässt, nicht als bestätigte Tatsache.
///
/// Quellen: <https://code.claude.com/docs/en/legal-and-compliance>,
/// <https://platform.claude.com/docs/en/api/rate-limits>,
/// <https://dev.to/mcrolly/anthropic-kills-claude-subscription-access-for-third-party-tools-like-openclaw-what-it-means-for-3ipc>,
/// <https://gigazine.net/gsc_news/en/20260220-anthropic-third-party-block/>,
/// <https://www.sovereignmagazine.com/article/anthropic-blocks-openclaw-claude-subscriptions>,
/// <https://geol.ai/briefing/anthropic-blocks-thirdparty-agent-harnesses-for-claude-subscriptions-apr-4-2026-what-it-changes-for>
pub const ANTHROPIC_SUBSCRIPTION_TOKEN_WARNING: &str = "Hinweis: Du nutzt ein Abo-OAuth-/Setup-Token (Claude Free/Pro/Max) statt eines API-Keys. \
Laut Anthropics Nutzungsbedingungen ist Abo-OAuth für Claude Code und native Anthropic-Apps vorgesehen; \
Drittanbieter-Tools sollen API-Keys aus der Claude Console nutzen. \
Nach mehreren Presse-/Community-Berichten (Stand 2026, keine bestätigte Anthropic-Primärquelle mit Datum) \
setzt Anthropic diese Regel seit Anfang 2026 stufenweise serverseitig durch – \
Drittanbieter-Tools wie harw können daher schon jetzt ohne Vorwarnung abgelehnt oder limitiert werden, \
nicht erst durch eine künftige Änderung. \
Nutzung auf eigene Gefahr. Stabil: API-Key (platform.claude.com). \
Quellen: https://code.claude.com/docs/en/legal-and-compliance, https://platform.claude.com/docs/en/api/rate-limits";

/// Art des Anthropic-Credentials und damit des Auth-Header-Schemas.
///
/// # Description
/// - [`AnthropicCredential::ApiKey`] → Header `x-api-key` (Anthropic-API-Key
///   **oder** Foundry-Deployment-Key).
/// - [`AnthropicCredential::OAuth`] → Header `Authorization: Bearer` **plus**
///   `anthropic-beta: oauth-2025-04-20` (Setup-Token / OAuth).
#[derive(Clone)]
pub enum AnthropicCredential {
    /// Statischer API-Key (Anthropic oder Foundry). Header `x-api-key`.
    ApiKey(SecretString),
    /// OAuth-/Setup-Token. Header `Authorization: Bearer` + oauth-beta.
    OAuth(SecretString),
    /// Microsoft Entra bearer token, without Anthropic OAuth beta headers.
    Bearer(SecretString),
}

/// HTTP-Provider gegen die native Anthropic-Messages-API.
///
/// # Description
/// Hält den geteilten `reqwest::Client`, die **vollständige** Endpoint-URL
/// (`…/v1/messages`), den Modellnamen und das geheime Credential.
/// Implementiert [`harw_core::ModelProvider`].
///
/// # Concurrency
/// `Send + Sync`; hinter einem `Arc` von mehreren Threads nutzbar.
pub struct AnthropicMessagesProvider {
    client: reqwest::Client,
    /// Vollständige Endpoint-URL, z. B. `https://api.anthropic.com/v1/messages`
    /// oder `https://<res>.services.ai.azure.com/anthropic/v1/messages`.
    messages_url: String,
    provider_id: String,
    model: String,
    credential: AnthropicCredential,
    max_tokens: u32,
    request_timeout: Duration,
    /// Runde 7, Teil L4: Leerlauf-Zeitlimit gestreamter Antworten (siehe
    /// [`Self::configure_timeouts`]).
    stream_idle_timeout: Duration,
    configured_headers: Option<reqwest::header::HeaderMap>,
    /// Client-seitiger Rate-Limiter (siehe
    /// [`crate::rate_limiter::ProviderRateLimiter`]); standardmäßig
    /// deaktiviert (`ProviderRateLimiter::new(None)`) — dieser Konstruktionsweg
    /// hat keinen Zugriff auf `harw_config::ProviderToml::rate_limit`.
    rate_limiter: std::sync::Arc<crate::rate_limiter::ProviderRateLimiter>,
    /// Harte, zur Laufzeit elastisch verstellbare Nebenläufigkeitsgrenze
    /// (siehe [`harw_config::ProviderToml::max_concurrency`] und
    /// [`crate::DynamicConcurrencyLimiter`]); `None` nur bei [`Self::new`]/
    /// [`Self::from_base`] (kein Limiter installiert). `build_named_provider`
    /// installiert über [`Self::configure_concurrency`] immer einen Limiter
    /// — auch für `max_concurrency: None` (unbegrenzt) — damit ein späterer
    /// Ops-Layer per [`crate::DynamicConcurrencyLimiter::set_target`] auch
    /// ursprünglich unbegrenzte Provider nachträglich deckeln kann. Anders
    /// als `rate_limiter` (reaktives Header-Pacing) blockiert dies
    /// zusätzliche Requests rein client-seitig, bevor sie überhaupt gesendet
    /// werden.
    concurrency_limiter: Option<Arc<crate::DynamicConcurrencyLimiter>>,
    /// `auth.credential_pool[provider_id]`, falls nicht-leer konfiguriert
    /// (siehe `crate::credential_pool`-Moduldoku „Credential-Pool"). `None`
    /// heißt: dieser Provider nutzt ausschließlich `credential`/`messages_url`
    /// oben (unverändertes Verhalten ohne Pool).
    credential_pool: Option<Arc<crate::credential_pool::CredentialPool<AnthropicCredential>>>,
    /// Pro-Modell-Entscheidung für SSE-Streaming (siehe [`crate::sse::StreamPolicy`]).
    stream_policy: crate::sse::StreamPolicy,
    /// Client-seitige RPM/TPM-Budgets (siehe [`crate::budget`]); leer (ohne
    /// Wirkung) bis `build_named_provider` sie über
    /// [`Self::configure_budgets`] setzt.
    budgets: crate::budget::ProviderBudgets,
}

impl AnthropicMessagesProvider {
    /// Baut einen Provider aus einer bereits zusammengesetzten Endpoint-URL.
    ///
    /// # Arguments
    /// - `messages_url` (`impl Into<String>`): vollständige `/v1/messages`-URL.
    /// - `model` (`impl Into<String>`): Modell-/Deployment-Name für `model`.
    /// - `credential` ([`AnthropicCredential`]): bestimmt das Auth-Header-Schema.
    ///
    /// # Returns
    /// Einen einsatzbereiten [`AnthropicMessagesProvider`] mit
    /// [`DEFAULT_MAX_TOKENS`].
    ///
    /// # Errors
    /// [`super::HttpProviderError::ClientBuild`] aus [`super::http_client`],
    /// wenn der geteilte `reqwest::Client` nicht gebaut werden kann.
    pub fn new(
        messages_url: impl Into<String>,
        model: impl Into<String>,
        credential: AnthropicCredential,
    ) -> super::HttpProviderResult<Self> {
        Ok(Self {
            client: super::http_client()?,
            messages_url: messages_url.into(),
            provider_id: "anthropic".to_owned(),
            model: model.into(),
            credential,
            max_tokens: DEFAULT_MAX_TOKENS,
            request_timeout: super::DEFAULT_REQUEST_TIMEOUT,
            stream_idle_timeout: super::DEFAULT_REQUEST_TIMEOUT,
            configured_headers: None,
            rate_limiter: std::sync::Arc::new(crate::rate_limiter::ProviderRateLimiter::new(None)),
            concurrency_limiter: None,
            credential_pool: None,
            stream_policy: crate::sse::StreamPolicy::default(),
            budgets: crate::budget::ProviderBudgets::default(),
        })
    }

    /// Baut einen Provider aus Basis-URL + Modell + Credential.
    ///
    /// # Description
    /// Setzt die Endpoint-URL über [`anthropic_messages_url`] zusammen (inkl.
    /// Trailing-Slash- und doppel-`/v1`-Normalisierung).
    ///
    /// # Errors
    /// Siehe [`Self::new`].
    pub fn from_base(
        base_url: &str,
        model: impl Into<String>,
        credential: AnthropicCredential,
    ) -> super::HttpProviderResult<Self> {
        Self::new(anthropic_messages_url(base_url), model, credential)
    }

    pub(crate) fn configure(&mut self, id: &str, headers: reqwest::header::HeaderMap) {
        self.provider_id = id.to_owned();
        self.configured_headers = Some(headers);
    }

    /// Setzt den Rate-Limiter aus der Provider-Konfiguration
    /// (`harw_config::ProviderToml::rate_limit`); aufgerufen von
    /// `build_named_provider` im Anthropic-Zweig, da `AnthropicMessagesProvider`
    /// selbst keinen `from_named_config`-Konstruktionsweg besitzt.
    /// Setzt den Credential-Pool (siehe `crate::credential_pool`-Moduldoku
    /// „Credential-Pool"); aufgerufen von `build_named_provider` im
    /// Anthropic-Zweig, wenn `auth.credential_pool[provider_id]` nicht-leer
    /// ist. `None` lässt das Feld unverändert `None` (kein Pool, unverändertes
    /// Verhalten über `self.credential`/`self.messages_url`).
    pub(crate) fn configure_credential_pool(
        &mut self,
        pool: Option<crate::credential_pool::CredentialPool<AnthropicCredential>>,
    ) {
        self.credential_pool = pool.map(Arc::new);
    }

    pub(crate) fn configure_stream_policy(&mut self, policy: crate::sse::StreamPolicy) {
        self.stream_policy = policy;
    }

    /// Setzt die client-seitigen RPM/TPM-Budgets (siehe [`crate::budget`]);
    /// aufgerufen von `build_named_provider` im Anthropic-Zweig mit dem
    /// Ergebnis von `ProviderBudgetRegistry::configure_provider`.
    pub(crate) fn configure_budgets(&mut self, budgets: crate::budget::ProviderBudgets) {
        self.budgets = budgets;
    }

    /// Runde 7, Teil L4: Setzt Request- und Streaming-Leerlauf-Zeitlimit
    /// (aus `ProviderToml::effective_request_timeout_secs` bzw.
    /// `effective_stream_idle_timeout_secs`).
    ///
    /// # Arguments
    /// - `request_timeout`: Gesamtlimit nicht gestreamter Requests bzw.
    ///   Wartezeit bis zu den Headern gestreamter Requests.
    /// - `stream_idle_timeout`: höchste Pause zwischen zwei Stream-Chunks.
    pub(crate) fn configure_timeouts(
        &mut self,
        request_timeout: Duration,
        stream_idle_timeout: Duration,
    ) {
        self.request_timeout = request_timeout;
        self.stream_idle_timeout = stream_idle_timeout;
    }

    pub(crate) fn configure_rate_limit(&mut self, rate_limit: Option<harw_config::RateLimitToml>) {
        self.rate_limiter =
            std::sync::Arc::new(crate::rate_limiter::ProviderRateLimiter::new(rate_limit));
    }

    /// Installiert einen [`crate::DynamicConcurrencyLimiter`] aus
    /// `harw_config::ProviderToml::max_concurrency`; aufgerufen von
    /// `build_named_provider` im Anthropic-Zweig — analog zu
    /// `OpenAiResponsesProvider::from_named_config`, das ebenfalls immer
    /// einen Limiter installiert, auch für `max_concurrency: None`
    /// (unbegrenzt), damit ein späterer Ops-Layer die Grenze nachträglich
    /// per [`crate::DynamicConcurrencyLimiter::set_target`] setzen kann.
    pub(crate) fn configure_concurrency(&mut self, max_concurrency: Option<usize>) {
        self.concurrency_limiter = Some(Arc::new(crate::DynamicConcurrencyLimiter::new(
            max_concurrency,
        )));
    }

    /// Liefert einen geteilten Zugriff auf den installierten
    /// [`crate::DynamicConcurrencyLimiter`], falls einer via
    /// [`Self::configure_concurrency`] gesetzt wurde.
    ///
    /// # Returns
    /// `Some(&Arc<DynamicConcurrencyLimiter>)`, geklont über `Arc::clone` für
    /// den Aufrufer, oder `None`, wenn dieser Provider über [`Self::new`]/
    /// [`Self::from_base`] statt `build_named_provider` gebaut wurde.
    #[must_use]
    pub fn concurrency_limiter(&self) -> Option<Arc<crate::DynamicConcurrencyLimiter>> {
        self.concurrency_limiter.as_ref().map(Arc::clone)
    }

    /// Liefert einen geteilten Zugriff auf den
    /// [`crate::rate_limiter::ProviderRateLimiter`] dieses Providers.
    ///
    /// # Returns
    /// Ein geklonter `Arc<ProviderRateLimiter>` (immer vorhanden — jeder
    /// Provider hat einen Rate-Limiter, ggf. nur deaktiviert).
    #[must_use]
    pub fn rate_limiter_handle(&self) -> Arc<crate::rate_limiter::ProviderRateLimiter> {
        Arc::clone(&self.rate_limiter)
    }

    /// Momentaufnahme von Nebenläufigkeits-/Rate-Limit-Zustand dieses
    /// Providers. Siehe [`crate::ProviderLoadStatus`]. Identisch zu
    /// `<Self as crate::ProviderLoadControl>::provider_status`; als eigene
    /// Methode nutzbar, ohne den Trait zu importieren.
    #[must_use]
    pub fn load_status(&self) -> crate::ProviderLoadStatus {
        crate::provider_load_status(
            &self.provider_id,
            self.concurrency_limiter.as_deref(),
            &self.rate_limiter,
        )
        .with_budgets(&self.budgets)
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

    /// Liefert Credential + vollständige `/v1/messages`-URL für einen Versuch.
    ///
    /// # Description
    /// `credential_idx` wählt (falls `Some` und [`Self::credential_pool`]
    /// gesetzt) einen konkreten Pool-Eintrag; dessen `base_url`-Override
    /// wird über [`anthropic_messages_url`] zur vollständigen Endpoint-URL
    /// normalisiert, wenn gesetzt, sonst bleibt `self.messages_url` die
    /// URL. Ohne Pool oder mit `credential_idx: None` liefert dies
    /// unverändert `self.credential`/`self.messages_url` — das bisherige
    /// Verhalten ohne Credential-Pool.
    fn request_target(&self, credential_idx: Option<usize>) -> (&AnthropicCredential, String) {
        match (credential_idx, &self.credential_pool) {
            (Some(index), Some(pool)) => {
                let entry = pool.entry(index);
                let url = match &entry.base_url {
                    Some(base) => anthropic_messages_url(base),
                    None => self.messages_url.clone(),
                };
                (&entry.value, url)
            }
            _ => (&self.credential, self.messages_url.clone()),
        }
    }
}

/// Setzt aus einer Basis-URL die vollständige `/v1/messages`-Endpoint-URL
/// zusammen.
///
/// # Description
/// Normalisiert die Eingabe robust gegen die zwei realen Formen:
/// - Foundry: `https://<res>.services.ai.azure.com/anthropic/` (Trailing-Slash)
/// - Anthropic-Katalog: `https://api.anthropic.com/v1` (bereits `/v1`)
///
/// Trailing-`/` werden entfernt; ein bereits vorhandenes `/v1`-Suffix wird
/// abgeschnitten, bevor `/v1/messages` angehängt wird — so entsteht nie
/// `//v1/messages` oder `/v1/v1/messages`.
///
/// # Examples
/// ```
/// use harw_provider_http::anthropic_messages_url;
/// assert_eq!(
///     anthropic_messages_url("https://x.services.ai.azure.com/anthropic/"),
///     "https://x.services.ai.azure.com/anthropic/v1/messages"
/// );
/// assert_eq!(
///     anthropic_messages_url("https://api.anthropic.com/v1"),
///     "https://api.anthropic.com/v1/messages"
/// );
/// ```
#[must_use]
pub fn anthropic_messages_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    let root = trimmed.strip_suffix("/v1").unwrap_or(trimmed);
    let root = root.trim_end_matches('/');
    format!("{root}/v1/messages")
}

/// Parses the optional custom-header environment variable without accepting
/// malformed or control-character-containing header data.
fn parse_anthropic_custom_headers(
    raw: &str,
) -> Result<Vec<(reqwest::header::HeaderName, reqwest::header::HeaderValue)>, ModelError> {
    let mut headers = Vec::new();
    for header_spec in raw.split(',') {
        let header_spec = header_spec.trim();
        let Some((name, value)) = header_spec.split_once(':') else {
            return Err(ModelError::RequestFailed(
                INVALID_CUSTOM_HEADERS_ERROR.to_owned(),
            ));
        };
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() || value.contains('\r') || value.contains('\n') {
            return Err(ModelError::RequestFailed(
                INVALID_CUSTOM_HEADERS_ERROR.to_owned(),
            ));
        }
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| ModelError::RequestFailed(INVALID_CUSTOM_HEADERS_ERROR.to_owned()))?;
        let value = reqwest::header::HeaderValue::from_str(value)
            .map_err(|_| ModelError::RequestFailed(INVALID_CUSTOM_HEADERS_ERROR.to_owned()))?;
        headers.push((name, value));
    }
    Ok(headers)
}

fn anthropic_custom_headers()
-> Result<Option<Vec<(reqwest::header::HeaderName, reqwest::header::HeaderValue)>>, ModelError> {
    match std::env::var("ANTHROPIC_CUSTOM_HEADERS") {
        Ok(raw) if raw.trim().is_empty() => Ok(None),
        Ok(raw) => parse_anthropic_custom_headers(&raw).map(Some),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(ModelError::RequestFailed(
            INVALID_CUSTOM_HEADERS_ERROR.to_owned(),
        )),
    }
}

/// Übersetzt die dem Modell angebotenen [`ToolSpec`]s in das Anthropic-
/// Wire-Format des `tools`-Arrays: `{name, description, input_schema}`.
///
/// # Description
/// Aktuell existiert nur `ToolSpec::Function`; die `input_schema` ist die
/// JSON-Schema-Repräsentation der Tool-Parameter ([`harw_tools::JsonSchema`]
/// via `serde_json::to_value`). Ein Serialisierungsfehler ist praktisch
/// ausgeschlossen (reine Daten-Struktur ohne fehlbare Typen) — im
/// unwahrscheinlichen Fehlerfall wird ein leeres Objekt-Schema verwendet,
/// statt zu paniken.
fn build_anthropic_tools(tools: &[ToolSpec], names: &ToolNameCodec) -> Vec<Value> {
    tools
        .iter()
        .map(|spec| match spec {
            ToolSpec::Function(f) => {
                let input_schema =
                    serde_json::to_value(&f.parameters).unwrap_or_else(|_| serde_json::json!({}));
                serde_json::json!({
                    "name": names.encode(f.name.as_str()),
                    "description": f.description,
                    "input_schema": input_schema,
                })
            }
        })
        .collect()
}

/// Hängt einen Content-Block an die letzte Nachricht derselben Rolle an oder
/// beginnt eine neue Nachricht.
///
/// # Description
/// Die Messages-API verlangt abwechselnde Rollen: mehrere `tool_use`-Blöcke
/// eines Modell-Turns gehören in **eine** Assistant-Nachricht, die zugehörigen
/// `tool_result`-Blöcke in **eine** darauffolgende User-Nachricht. Im Verlauf
/// stehen sie dagegen als einzelne Einträge (erst alle Calls, dann alle
/// Ergebnisse), weil der Turn-Loop parallele Calls so protokolliert. Diese
/// Funktion führt beides wieder zusammen. Nachrichten mit Text-Content
/// (`content` als String) werden nie erweitert, sodass ein Textbeitrag eine
/// Gruppe zuverlässig beendet.
///
/// # Arguments
/// - `messages` (`&mut Vec<Value>`): der bisher aufgebaute Nachrichtenverlauf.
/// - `role` (`&str`): `"assistant"` für `tool_use`, `"user"` für `tool_result`.
/// - `block` (`Value`): der anzuhängende Content-Block.
fn push_content_block(messages: &mut Vec<Value>, role: &str, block: Value) {
    let open_group = messages
        .last_mut()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some(role))
        .and_then(|message| message.get_mut("content"))
        .and_then(Value::as_array_mut);
    match open_group {
        Some(content) => content.push(block),
        None => messages.push(serde_json::json!({ "role": role, "content": [block] })),
    }
}

/// Baut den Anthropic-Messages-Request-Body aus einem [`ModelRequest`].
///
/// # Description
/// System-Prompt + Instruction-Fragmente werden zum `system`-Feld
/// zusammengefasst; der Verlauf wird auf `messages[]` projiziert:
/// - `User` → `user` mit reinem Text-Content.
/// - `Assistant` → `assistant` mit reinem Text-Content.
/// - `ToolCall` → `assistant`-Message mit einem `tool_use`-Content-Block
///   (`{"type":"tool_use","id":<call_id>,"name":<tool_name>,"input":<args>}`).
/// - `ToolResult` → `user`-Message mit einem `tool_result`-Content-Block
///   (`{"type":"tool_result","tool_use_id":<call_id>,"content":<text>}`),
///   plus `"is_error":true` bei `Err`. `<text>` entsteht — wie im
///   OpenAI-kompatiblen Pfad — ausschließlich über
///   [`harw_core::envelope::render_tool_result`] (Trust-Hülle für
///   `Untrusted`, Byte-Deckel `request.tool_result_max_bytes`, sonst
///   [`DEFAULT_TOOL_RESULT_MAX_BYTES`]).
///
/// `request.tools` wird — sofern nicht leer — via [`build_anthropic_tools`]
/// auf `body["tools"]` abgebildet; bei leerem Tool-Set bleibt das Feld unset
/// (bestehendes Verhalten für Requests ohne Tools). Rein und I/O-frei, daher
/// direkt testbar.
///
/// Ist `request.reasoning_effort` `Some(effort)`, schlägt die Funktion das
/// Modell in der Capability-Tabelle [`crate::anthropic_caps`] nach
/// ([`anthropic_caps::lookup`]):
/// - Akzeptiert das Modell `thinking: {"type": "adaptive"}`
///   ([`anthropic_caps::ThinkingSupport::Adaptive`]), wird dieses Top-Level-
///   Feld gesetzt (aktuelles Anthropic-Wire-Schema — NICHT das veraltete
///   `{"type": "enabled", "budget_tokens": N}`-Schema, das der Harness mangels
///   ableitbarem Token-Budget nicht sendet).
/// - Liefert [`anthropic_caps::effort_wire_value`] für dieses Modell einen
///   Wert, wird zusätzlich `output_config: {"effort": <Wert>}` gesetzt — das
///   ist unabhängig vom `thinking`-Feld möglich (manche Modelle akzeptieren
///   `effort` ohne `adaptive`-Thinking).
/// - Unbekannte Modelle (`lookup` liefert `None`) und Modelle ohne die
///   jeweilige Fähigkeit bekommen das betreffende Feld gar nicht (fail
///   closed — ein falsches `thinking`- oder `effort`-Feld erzeugt HTTP 400).
///
/// Ist `request.reasoning_effort` `None`, bleiben beide Felder unset.
///
/// Die Ausgabe-Obergrenze ist `request.max_output_tokens`, sofern gesetzt
/// und > 0, sonst der Provider-Default `max_tokens`. Sie wird über
/// [`anthropic_caps::clamp_max_tokens`] auf das Ausgabe-Limit des Modells
/// geklemmt, bevor sie auf `body["max_tokens"]` landet; für unbekannte
/// Modelle bleibt der Wert unverändert.
///
/// # Arguments
/// - `model` (`&str`): Modell-/Deployment-Name für das `model`-Feld.
/// - `max_tokens` (`u32`): Provider-Default der Ausgabe-Token-Obergrenze
///   (vor Clamping), greift nur ohne `request.max_output_tokens`.
/// - `request` (`&ModelRequest`): Quelle für System-Prompt, Fragmente,
///   Verlauf, Tools und optionales `reasoning_effort`.
///
/// # Returns
/// Ein [`serde_json::Value`]-Objekt, direkt als Request-Body serialisierbar.
#[must_use]
pub fn build_messages_body(model: &str, max_tokens: u32, request: &ModelRequest) -> Value {
    let mut system = request.system_prompt.clone();
    for fragment in &request.instruction_fragments {
        system.push('\n');
        system.push_str(fragment);
    }

    // Interne Namen wie `fs.read` verletzen Anthropics Namensmuster.
    let names = ToolNameCodec::for_request(request);
    let mut renderer = super::ToolResultRenderer::new(request);
    if request.tool_result_max_bytes.is_none() {
        renderer.max_bytes = DEFAULT_TOOL_RESULT_MAX_BYTES;
    }
    let mut messages: Vec<Value> = Vec::new();
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
                push_content_block(
                    &mut messages,
                    "assistant",
                    serde_json::json!({
                        "type": "tool_use",
                        "id": call_id.as_str(),
                        "name": names.encode(&name),
                        "input": arguments,
                    }),
                );
            }
            harw_core::ModelMessage::ToolResult { call_id, result } => {
                let is_error = matches!(result, ToolCallResult::Error { .. });
                let content = renderer.render(&call_id, &result);
                let mut block = serde_json::json!({
                    "type": "tool_result",
                    "tool_use_id": call_id.as_str(),
                    "content": content,
                });
                if is_error {
                    block["is_error"] = Value::Bool(true);
                }
                push_content_block(&mut messages, "user", block);
            }
        }
    }

    let requested = request
        .max_output_tokens
        .filter(|tokens| *tokens > 0)
        .unwrap_or(max_tokens);
    let max_tokens = crate::anthropic_caps::clamp_max_tokens(model, requested);
    let mut body = serde_json::json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": messages,
    });
    if !system.is_empty() {
        body["system"] = Value::String(system);
    }
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(build_anthropic_tools(&request.tools, &names));
    }
    if let Some(effort) = request.reasoning_effort
        && let Some(caps) = crate::anthropic_caps::lookup(model)
    {
        if caps.thinking == crate::anthropic_caps::ThinkingSupport::Adaptive {
            body["thinking"] = serde_json::json!({"type": "adaptive"});
        }
        // Minimal → thinking (falls gesetzt) bleibt aktiv, aber
        // output_config.effort wird weggelassen; das Modell wählt den
        // Denkaufwand selbst. effort_wire_value liefert dafür bereits `None`.
        if let Some(level) = crate::anthropic_caps::effort_wire_value(caps, effort) {
            body["output_config"] = serde_json::json!({"effort": level});
        }
        // Unbekannte Modelle (lookup == None) bekommen weder thinking noch
        // output_config — fail closed, siehe Modul-Doku von anthropic_caps.
    }
    body
}

/// Extrahiert den Assistant-Text aus einer nicht-gestreamten Messages-Antwort.
///
/// # Description
/// Sammelt alle `content[]`-Blöcke vom Typ `text` und verbindet ihren `text`.
///
/// # Returns
/// `Some(text)` oder `None`, wenn kein nicht-leerer Text vorliegt.
#[must_use]
pub fn extract_anthropic_text(body: &Value) -> Option<String> {
    let content = body.get("content")?.as_array()?;
    let mut text = String::new();
    for block in content {
        if block.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(chunk) = block.get("text").and_then(Value::as_str) {
                text.push_str(chunk);
            }
        }
    }
    if text.is_empty() { None } else { Some(text) }
}

/// Extrahiert die vom Modell angeforderten Tool-Calls aus einer
/// nicht-gestreamten Messages-Antwort.
///
/// # Description
/// Durchsucht `content[]` nach Blöcken vom Typ `tool_use` und mappt
/// `id → call_id`, `name → name`, `input → arguments`. Blöcke mit fehlendem
/// `id` oder `name` müssen als Strings vorhanden sein; unvollständige
/// `tool_use`-Blöcke werden fail-closed abgelehnt.
///
/// # Arguments
/// - `body` (`&Value`): der bereits geparste JSON-Response-Body.
///
/// # Returns
/// Einen `Vec<ToolCall>`, leer wenn kein `content`-Array oder keine
/// `tool_use`-Blöcke vorhanden sind. Eine fehlerhafte `tool_use`-Antwort mit
/// fehlendem oder nicht-stringförmigem `id`/`name` oder fehlendem bzw.
/// nicht-objektförmigem `input` wird als ungültige Provider-Antwort
/// fail-closed abgelehnt.
/// Fehler beim fail-closed Parsen eines Anthropic-`tool_use`-Blocks.
#[derive(Debug, PartialEq, Eq)]
pub enum AnthropicToolCallParseError {
    MissingId,
    MissingName,
    InvalidInput,
}

impl fmt::Display for AnthropicToolCallParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::MissingId => "Anthropic tool-use id must be a string",
            Self::MissingName => "Anthropic tool-use name must be a string",
            Self::InvalidInput => "Anthropic tool-use input must be an object",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for AnthropicToolCallParseError {}

pub fn extract_anthropic_tool_calls(
    body: &Value,
) -> Result<Vec<ToolCall>, AnthropicToolCallParseError> {
    let Some(content) = body.get("content").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut calls = Vec::new();
    for block in content {
        if block.get("type").and_then(Value::as_str) != Some("tool_use") {
            continue;
        }

        let Some(arguments) = block
            .get("input")
            .filter(|value| value.is_object())
            .cloned()
        else {
            return Err(AnthropicToolCallParseError::InvalidInput);
        };
        let Some(id) = block.get("id").and_then(Value::as_str) else {
            return Err(AnthropicToolCallParseError::MissingId);
        };
        let Some(name) = block.get("name").and_then(Value::as_str) else {
            return Err(AnthropicToolCallParseError::MissingName);
        };
        calls.push(ToolCall {
            id: ToolCallId::from_str(id.to_owned()),
            name: ToolName::new(name.to_owned()),
            arguments,
        });
    }
    Ok(calls)
}

/// Extrahiert die Token-Nutzung aus einer nicht-gestreamten Messages-Antwort.
///
/// # Description
/// Liest das Top-Level-`usage`-Objekt der Anthropic-Messages-Antwort:
/// `input_tokens` und `output_tokens` direkt, `cached_tokens` aus
/// `cache_read_input_tokens`. Anthropic liefert keine separaten
/// Reasoning-Token-Zähler — Extended-Thinking-Tokens sind bereits in
/// `output_tokens` enthalten, daher bleibt `reasoning_tokens` immer `None`.
///
/// Ein fehlendes `usage`-Objekt oder fehlende Einzelfelder führen zu `0`
/// (bzw. `None` für `cached_tokens`), nie zu einem Panic — passend zum
/// restlichen Parsing-Stil dieser Datei (`Option`-Kombinatoren statt
/// `unwrap`/`expect`).
///
/// # Arguments
/// - `body` (`&Value`): der bereits geparste JSON-Response-Body.
///
/// # Returns
/// Ein [`TokenUsage`] mit den extrahierten (oder default-mäßig genullten)
/// Werten. `cached_tokens` ist `None` genau dann, wenn
/// `cache_read_input_tokens` im Body fehlt — ein explizit übermittelter
/// Wert von `0` ist ein gültiger, gemessener Wert und bleibt `Some(0)`.
#[must_use]
pub fn extract_anthropic_usage(body: &Value) -> TokenUsage {
    let input_tokens = body
        .get("usage")
        .and_then(|usage| usage.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output_tokens = body
        .get("usage")
        .and_then(|usage| usage.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cached_tokens = body
        .get("usage")
        .and_then(|usage| usage.get("cache_read_input_tokens"))
        .and_then(Value::as_u64);
    let cache_write_tokens = body
        .get("usage")
        .and_then(|usage| usage.get("cache_creation_input_tokens"))
        .and_then(Value::as_u64);

    TokenUsage {
        input_tokens,
        output_tokens,
        reasoning_tokens: None,
        cached_tokens,
        cache_write_tokens,
        cache_separate: true,
    }
}

/// Übersetzt Anthropics Top-Level-`stop_reason` in [`StopReason`].
///
/// # Description
/// Liest `body.stop_reason` (String) einer nicht-gestreamten
/// Messages-Antwort und mappt sie auf die provider-neutrale Variante.
/// Ein fehlender oder unbekannter Wert wird nicht verworfen: bekannte
/// Anthropic-Werte werden 1:1 gemappt, ein unbekannter String landet in
/// [`StopReason::Other`], und ein komplett fehlendes Feld fällt auf
/// [`StopReason::EndTurn`] zurück (Default-Verhalten bei regulärer Antwort).
/// `model_context_window_exceeded` (Eingabe + Ausgabe haben das
/// Kontextfenster erreicht) wird zu [`StopReason::ContextWindowExceeded`],
/// damit der Turn-Loop eine Notfall-Kompaktierung auslösen kann.
///
/// # Arguments
/// - `body` (`&Value`): der bereits geparste JSON-Response-Body.
///
/// # Returns
/// Die gemappte [`StopReason`].
#[must_use]
pub fn extract_anthropic_stop_reason(body: &Value) -> StopReason {
    match body.get("stop_reason").and_then(Value::as_str) {
        Some("end_turn") => StopReason::EndTurn,
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("stop_sequence") => StopReason::StopSequence,
        Some("pause_turn") => StopReason::PauseTurn,
        Some("refusal") => StopReason::Refusal { detail: None },
        Some("model_context_window_exceeded") => StopReason::ContextWindowExceeded,
        Some(other) => StopReason::Other(other.to_owned()),
        None => StopReason::EndTurn,
    }
}

/// Extrahiert opake Denkblöcke (`thinking`/`redacted_thinking`) aus einer
/// nicht-gestreamten Messages-Antwort (G-015).
///
/// # Description
/// Sammelt alle `content[]`-Blöcke vom Typ `thinking` oder
/// `redacted_thinking` unverändert (als rohe `serde_json::Value`) in
/// Aufrufreihenfolge, damit sie im nächsten Tool-Loop-Turn byte-identisch
/// zurückgespielt werden können.
///
/// # Arguments
/// - `body` (`&Value`): der bereits geparste JSON-Response-Body.
/// - `model` (`&str`): die für den Request verwendete Modell-ID, zur
///   Provenance-Markierung im zurückgegebenen [`OpaqueReasoning`].
///
/// # Returns
/// `Some(OpaqueReasoning)` mit `provider: "anthropic"`, wenn mindestens ein
/// Denkblock vorhanden ist; sonst `None` (kein Extended Thinking in der
/// Antwort).
#[must_use]
pub fn extract_anthropic_reasoning(body: &Value, model: &str) -> Option<OpaqueReasoning> {
    let content = body.get("content")?.as_array()?;
    let blocks: Vec<Value> = content
        .iter()
        .filter(|block| {
            matches!(
                block.get("type").and_then(Value::as_str),
                Some("thinking") | Some("redacted_thinking")
            )
        })
        .cloned()
        .collect();
    if blocks.is_empty() {
        None
    } else {
        Some(OpaqueReasoning {
            provider: "anthropic".to_owned(),
            model: model.to_owned(),
            blocks,
        })
    }
}

/// Setzt die Auth-Header des Credentials; alle Credential-Werte sind als
/// sensitiv markiert (`x-api-key` und OAuth-`authorization` über
/// [`super::sensitive_header_value`], `bearer_auth` in reqwest selbst).
fn apply_anthropic_credential(
    builder: reqwest::RequestBuilder,
    credential: &AnthropicCredential,
) -> Result<reqwest::RequestBuilder, ModelError> {
    Ok(match credential {
        AnthropicCredential::ApiKey(secret) => builder.header(
            "x-api-key",
            super::sensitive_header_value(secret.expose_secret())?,
        ),
        AnthropicCredential::Bearer(secret) => builder.bearer_auth(secret.expose_secret()),
        AnthropicCredential::OAuth(secret) => builder
            .header(
                "authorization",
                super::sensitive_header_value(&format!("Bearer {}", secret.expose_secret()))?,
            )
            .header("anthropic-beta", ANTHROPIC_OAUTH_BETA),
    })
}

/// Übersetzt eine nicht-erfolgreiche Anthropic-Antwort in den
/// `ModelError`-Vertrag.
///
/// # Description
/// Nutzt dieselbe Klassifikation wie der OpenAI-kompatible Pfad
/// ([`crate::error::model_error_for_status`] plus
/// [`crate::error::retry_after_hint`] für `retry-after-ms`/`Retry-After`/
/// Body-Hinweis): 529 (overloaded) und 408/5xx → `Transient`, 401/403 →
/// `Auth`, Kontextlängen-/Kontingent-Fehler → `ContextLength`/
/// `QuotaExceeded`, übrige 4xx → `RequestFailed`. Einzige Abweichung: ein
/// kurzfristiges 429 wird wie bisher als [`ModelError::RateLimited`]
/// gemeldet (ebenfalls retryable); ohne Wartehinweis gilt der Fallback von
/// [`super::parse_retry_after`] (30 s).
///
/// # Arguments
/// - `status` (`u16`): HTTP-Status.
/// - `request_id` (`Option<&str>`): begrenzte Gateway-Request-ID.
/// - `retry_after` (`Option<&str>`): Wert des `Retry-After`-Headers.
/// - `retry_after_ms` (`Option<&str>`): Wert des `retry-after-ms`-Headers.
/// - `body` (`&str`): unvertrauenswürdiger Antwort-Body (nie im Ergebnis).
///
/// # Returns
/// Die passende [`ModelError`]-Variante.
fn anthropic_error_for_status(
    status: u16,
    request_id: Option<&str>,
    retry_after: Option<&str>,
    retry_after_ms: Option<&str>,
    body: &str,
) -> ModelError {
    let hint = crate::error::retry_after_hint(retry_after, retry_after_ms, body);
    match crate::error::model_error_for_status(status, request_id, hint, body) {
        ModelError::Transient {
            status: Some(429),
            retry_after_secs,
            message,
        } => ModelError::RateLimited {
            retry_after_secs: retry_after_secs
                .unwrap_or_else(|| super::parse_retry_after(retry_after, body).as_secs()),
            message,
        },
        other => other,
    }
}

impl AnthropicMessagesProvider {
    /// Sendet **einen** Versuch mit dem durch `credential_idx` gewählten
    /// Credential (siehe [`Self::request_target`]). Der eigentliche Körper
    /// von [`ModelProvider::respond`] vor Einführung des Credential-Pools —
    /// unverändert bis auf die Credential-/URL-Auswahl und die explizite
    /// 401/403-Unterscheidung, damit [`ModelProvider::respond`] denselben
    /// Versuch mit einem anderen Pool-Eintrag wiederholen kann.
    ///
    /// # Errors
    /// Siehe [`ModelProvider::respond`]; zusätzlich [`ModelError::Auth`] bei
    /// HTTP 401/403 (zuvor Teil von [`ModelError::RequestFailed`] — die
    /// Unterscheidung ist nötig, damit `credential_pool::should_failover`
    /// einen ungültigen/entzogenen Schlüssel erkennen kann). Fehlerstatus und
    /// Transportfehler werden wie im OpenAI-kompatiblen Pfad klassifiziert
    /// (siehe [`anthropic_error_for_status`] bzw.
    /// [`crate::error::model_error_for_transport`]): 429 → `RateLimited`,
    /// 408/5xx/529 und Verbindungsfehler → `Transient`, Zeitüberschreitung →
    /// `Timeout` — alle vom `RetryingProvider` wiederholbar.
    async fn respond_once(
        &self,
        request: ModelRequest,
        credential_idx: Option<usize>,
    ) -> Result<ModelResponse, ModelError> {
        let (credential, messages_url) = self.request_target(credential_idx);
        let model = self.selected_model(&request)?;
        let strategy =
            crate::cache_strategy::resolve_cache_strategy(&self.provider_id, model, None);
        let mut wire = build_messages_body(model, self.max_tokens, &request);
        crate::cache_strategy::apply_messages_cache_control(&mut wire, strategy);
        let stream_sink = request
            .stream
            .as_ref()
            .filter(|_| self.stream_policy.enabled(model));
        if stream_sink.is_some() {
            wire["stream"] = Value::Bool(true);
        }
        tracing::debug!(
            model,
            strategy = strategy.label(),
            "sending anthropic messages request"
        );

        let mut builder = self
            .client
            .post(&messages_url)
            .header("content-type", "application/json")
            .header("anthropic-version", ANTHROPIC_VERSION);

        // Optionaler Passthrough zusätzlicher Header (z. B. für Cloudflare AI
        // Gateway: `cf-aig-authorization: Bearer …`). Format aus der Env-Var
        // `ANTHROPIC_CUSTOM_HEADERS` — kompatibel mit Anthropic's offiziellem
        // Claude-Code-Client. Mehrere Header via `,` getrennt.
        let custom_headers = if let Some(headers) = &self.configured_headers {
            Some(
                headers
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect::<Vec<_>>(),
            )
        } else {
            anthropic_custom_headers()?
        };

        builder = apply_anthropic_credential(builder, credential)?;
        if let Some(custom_headers) = custom_headers {
            for (name, value) in custom_headers {
                builder = builder.header(name, value);
            }
        }

        // Eingabe-Schätzung nur berechnen, wenn Budgets oder der Header-Pacer
        // sie tatsächlich brauchen (Serialisierung des Wire-Bodys).
        let estimated_input = if self.budgets.is_empty() && !self.rate_limiter.is_enabled() {
            0
        } else {
            crate::budget::estimate_wire_tokens(&wire)
        };
        // Client-seitige RPM/TPM-Budgets (siehe [`crate::budget`]) und
        // Header-Pacing zuerst: eine Wartepause darf keinen
        // Nebenläufigkeits-Slot belegen, sonst blockiert ein wartender Request
        // andere, die sofort senden dürften. Fail-fast, wenn schon die
        // Eingabe-Schätzung ein Minutenlimit sprengt. Der Budget-Permit lebt
        // bis zum Abgleich mit der tatsächlichen Nutzung unten.
        let budget_permit = if self.budgets.is_empty() {
            crate::budget::BudgetPermit::empty()
        } else {
            let max_output = wire
                .get("max_tokens")
                .and_then(Value::as_u64)
                .unwrap_or_else(|| u64::from(self.max_tokens));
            self.budgets
                .acquire(model, estimated_input, max_output)
                .await?
        };
        self.rate_limiter
            .wait_for_slot_with_estimate(estimated_input)
            .await
            .map_err(crate::rate_budget_error)?;
        // Hartes Nebenläufigkeits-Limit (siehe [`Self::concurrency_limiter`]):
        // blockiert, bis ein Slot frei wird, statt fehlzuschlagen. Der Guard
        // bleibt bis zum Ende dieser Funktion (also bis der Response-Body
        // vollständig gelesen ist) im Scope, damit die Grenze wirklich in
        // Flug befindliche Requests zählt — gleiches Muster wie
        // `OpenAiResponsesProvider::respond_once`.
        let _concurrency_permit = match &self.concurrency_limiter {
            Some(limiter) => Some(limiter.acquire().await.map_err(|_| {
                ModelError::RequestFailed(
                    "internal error: provider concurrency semaphore was closed".to_owned(),
                )
            })?),
            None => None,
        };
        // Transportfehler laufen über dieselbe Klassifikation wie der
        // OpenAI-kompatible Pfad: Timeout → `Timeout`, Verbindungs-/Sendefehler
        // → `Transient` (beide vom `RetryingProvider` wiederholbar).
        // Runde 7, Teil L4: gestreamt begrenzt `request_timeout` nur die
        // Wartezeit bis zu den Headern (siehe `send_with_timeout`).
        let response = crate::send_with_timeout(
            builder.json(&wire),
            self.request_timeout,
            stream_sink.is_some(),
        )
        .await?;

        self.rate_limiter.observe_headers(response.headers());
        let status = response.status();
        let retry_after_header = super::header_string(response.headers(), "retry-after");
        let retry_after_ms_header = super::header_string(response.headers(), "retry-after-ms");
        let request_id = super::provider_request_id(response.headers());
        if status.is_success()
            && let Some(sink) = stream_sink
        {
            let mut accumulator = crate::sse::AnthropicStreamAccumulator::default();
            crate::sse::read_sse(response, Some(self.stream_idle_timeout), |frame| {
                accumulator.push(&frame, Some(sink))
            })
            .await?;
            let value = accumulator.finish()?;
            let response = self.interpret_body(&request, model, &value)?;
            budget_permit.reconcile(&response.usage);
            return Ok(response);
        }
        let body = response
            .text()
            .await
            .map_err(|error| crate::error::model_error_for_transport(error, true))?;

        if !status.is_success() {
            if status.as_u16() == 429 {
                // W6b — UIA-Sichtbarkeit: zählt jede beobachtete 429-Antwort
                // dieses Providers (siehe
                // `rate_limiter::ProviderRateLimiter::record_rate_limited`).
                self.rate_limiter.record_rate_limited();
                // Sperrt alle Budget-Buckets dieses Requests (auch für bereits
                // Wartende) für die `Retry-After`-Dauer.
                let hint = crate::error::retry_after_hint(
                    retry_after_header.as_deref(),
                    retry_after_ms_header.as_deref(),
                    &body,
                );
                self.budgets.penalize(model, hint.map(Duration::from_secs));
            }
            let error = anthropic_error_for_status(
                status.as_u16(),
                request_id.as_deref(),
                retry_after_header.as_deref(),
                retry_after_ms_header.as_deref(),
                &body,
            );
            tracing::debug!(
                status = status.as_u16(),
                retryable = error.is_retryable(),
                "anthropic provider returned an error status"
            );
            return Err(error);
        }

        let value: Value = serde_json::from_str(&body)?;
        let response = self.interpret_body(&request, model, &value)?;
        budget_permit.reconcile(&response.usage);
        Ok(response)
    }

    /// Projiziert einen (ggf. aus SSE rekonstruierten) Messages-Body auf eine
    /// [`ModelResponse`].
    fn interpret_body(
        &self,
        request: &ModelRequest,
        model: &str,
        value: &Value,
    ) -> Result<ModelResponse, ModelError> {
        let text = extract_anthropic_text(value);
        let names = ToolNameCodec::for_request(request);
        let tool_calls = extract_anthropic_tool_calls(value)
            .map_err(|_| {
                ModelError::RequestFailed(
                    "Anthropic response contained a tool-use block with invalid input".to_owned(),
                )
            })?
            .into_iter()
            .map(|call| ToolCall {
                name: ToolName::new(names.decode(call.name.as_str())),
                ..call
            })
            .collect::<Vec<_>>();
        if text.is_none() && tool_calls.is_empty() {
            return Err(ModelError::EmptyResponse);
        }

        Ok(ModelResponse {
            message: text,
            tool_calls,
            usage: extract_anthropic_usage(value),
            stop: extract_anthropic_stop_reason(value),
            reasoning: extract_anthropic_reasoning(value, model),
        })
    }
}

impl ModelProvider for AnthropicMessagesProvider {
    /// Sendet einen Modell-Request an diesen Provider und liefert die Antwort.
    ///
    /// # Description
    /// Delegiert an [`Self::respond_once`]. Ist [`Self::credential_pool`]
    /// gesetzt, wird zuerst dessen bevorzugter, nicht abkühlender Eintrag
    /// gewählt. Schlägt dieser Versuch mit `credential_pool::should_failover`
    /// fehl (401/403 oder Kontingent-Erschöpfung) **und** hat der Pool mehr
    /// als einen Eintrag, wird der verwendete Eintrag bounded (siehe
    /// `credential_pool::DEFAULT_COOLDOWN`) abgekühlt und derselbe Request
    /// **genau einmal** mit dem nächsten nicht abkühlenden Eintrag
    /// wiederholt — nie öfter. Ohne Pool bleibt das Verhalten unverändert:
    /// ein einziger Versuch mit `self.credential`.
    fn respond<'a>(&'a self, request: ModelRequest) -> ModelFuture<'a> {
        Box::pin(async move {
            let Some(pool) = self.credential_pool.as_ref() else {
                return self.respond_once(request, None).await;
            };
            let primary_idx = pool.select(None);
            match self.respond_once(request.clone(), primary_idx).await {
                Ok(response) => Ok(response),
                Err(error) => {
                    if pool.len() <= 1 || !crate::credential_pool::should_failover(&error) {
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

impl crate::ProviderLoadControl for AnthropicMessagesProvider {
    /// Siehe [`Self::load_status`].
    fn provider_status(&self) -> crate::ProviderLoadStatus {
        self.load_status()
    }

    /// Siehe [`crate::DynamicConcurrencyLimiter::set_target`]: wirkt sofort
    /// beim Wachsen, lazy beim Schrumpfen. `false`, wenn dieser Provider
    /// ohne [`Self::configure_concurrency`] gebaut wurde.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

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

    fn test_provider() -> TestResult<AnthropicMessagesProvider> {
        AnthropicMessagesProvider::new(
            "https://example.test/v1/messages",
            "configured-model",
            AnthropicCredential::ApiKey(SecretString::new("sk-secret".into())),
        )
        .map_err(ctx("AnthropicMessagesProvider::new"))
    }

    #[test]
    fn test_parse_anthropic_custom_headers_accepts_gateway_headers() -> TestResult {
        let headers = parse_anthropic_custom_headers(
            "cf-aig-authorization: Bearer gateway-token, cf-aig-metadata: tenant=test",
        )
        .map_err(ctx("valid gateway headers"))?;

        assert_eq!(headers.len(), 2);
        assert_eq!(headers[0].0.as_str(), "cf-aig-authorization");
        assert_eq!(
            headers[0].1.to_str().map_err(ctx("header value to_str"))?,
            "Bearer gateway-token"
        );
        assert_eq!(headers[1].0.as_str(), "cf-aig-metadata");
        assert_eq!(
            headers[1].1.to_str().map_err(ctx("header value to_str"))?,
            "tenant=test"
        );
        Ok(())
    }

    #[test]
    fn test_parse_anthropic_custom_headers_rejects_malformed_entry_redacted() -> TestResult {
        let Err(error) = parse_anthropic_custom_headers("cf-aig-authorization") else {
            return Err(TestError::Unexpected(
                "malformed header entry must be rejected".to_owned(),
            ));
        };

        assert!(matches!(
            error,
            ModelError::RequestFailed(message)
                if message == INVALID_CUSTOM_HEADERS_ERROR
        ));
        Ok(())
    }

    #[test]
    fn test_parse_anthropic_custom_headers_rejects_crlf_injection_without_echoing_value()
    -> TestResult {
        let malicious_value = "Bearer gateway-token\r\nX-Injected: true";
        let Err(error) =
            parse_anthropic_custom_headers(&format!("cf-aig-authorization: {malicious_value}"))
        else {
            return Err(TestError::Unexpected(
                "CRLF-injected header entry must be rejected".to_owned(),
            ));
        };

        assert!(matches!(
            error,
            ModelError::RequestFailed(message)
                if message == INVALID_CUSTOM_HEADERS_ERROR
                    && !message.contains("gateway-token")
                    && !message.contains("X-Injected")
        ));
        Ok(())
    }

    #[test]
    fn test_selected_model_uses_requested_model_for_compatible_provider() -> TestResult {
        let provider = test_provider()?;
        let request = request_with_ids(Some("requested-model"), Some("anthropic"));

        assert_eq!(
            provider
                .selected_model(&request)
                .map_err(ctx("selected_model"))?,
            "requested-model"
        );
        Ok(())
    }

    #[test]
    fn test_provider_uses_default_bounded_request_timeout() -> TestResult {
        let provider = test_provider()?;

        assert_eq!(
            provider.request_timeout,
            super::super::DEFAULT_REQUEST_TIMEOUT
        );
        Ok(())
    }

    #[test]
    fn test_selected_model_preserves_configured_default_for_empty_identifiers() -> TestResult {
        let provider = test_provider()?;
        let request = request_with_ids(Some(""), Some(""));

        assert_eq!(
            provider
                .selected_model(&request)
                .map_err(ctx("selected_model"))?,
            "configured-model"
        );
        Ok(())
    }

    #[test]
    fn test_selected_model_rejects_provider_mismatch() -> TestResult {
        let provider = test_provider()?;
        let request = request_with_ids(Some("requested-model"), Some("openai"));

        assert!(matches!(
            provider.selected_model(&request),
            Err(ModelError::RequestFailed(message))
                if message.contains("openai") && message.contains("anthropic")
        ));
        Ok(())
    }

    #[test]
    fn test_anthropic_messages_url_foundry_trailing_slash() {
        assert_eq!(
            anthropic_messages_url("https://x.services.ai.azure.com/anthropic/"),
            "https://x.services.ai.azure.com/anthropic/v1/messages"
        );
    }

    #[test]
    fn test_anthropic_messages_url_strips_existing_v1() {
        assert_eq!(
            anthropic_messages_url("https://api.anthropic.com/v1"),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn test_anthropic_messages_url_plain_root() {
        assert_eq!(
            anthropic_messages_url("https://api.anthropic.com"),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn test_anthropic_messages_url_multiple_trailing_slashes() {
        assert_eq!(
            anthropic_messages_url("https://api.anthropic.com/v1///"),
            "https://api.anthropic.com/v1/messages"
        );
    }

    #[test]
    fn test_build_messages_body_shape() -> TestResult {
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

        let body = build_messages_body("claude-sonnet-5", 1024, &request);
        assert_eq!(
            body.get("model").and_then(Value::as_str),
            Some("claude-sonnet-5")
        );
        assert_eq!(body.get("max_tokens").and_then(Value::as_u64), Some(1024));
        assert_eq!(
            body.get("system").and_then(Value::as_str),
            Some("You are terse.")
        );
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("messages array"))?;
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].get("role").and_then(Value::as_str),
            Some("user")
        );
        assert_eq!(
            messages[0].get("content").and_then(Value::as_str),
            Some("Hi there")
        );
        Ok(())
    }

    #[test]
    fn test_build_messages_body_omits_empty_system() {
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
        let body = build_messages_body("m", 256, &request);
        assert!(body.get("system").is_none());
    }

    #[test]
    fn test_build_messages_body_with_reasoning_effort_sets_thinking_and_output_config() {
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
            reasoning_effort: Some(harw_types::ReasoningEffort::High),
            model_id: None,
            provider_id: None,
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
            cancel: None,
            identity: None,
        };

        let body = build_messages_body("claude-opus-4-8", 1024, &request);
        assert_eq!(body["thinking"]["type"].as_str(), Some("adaptive"));
        assert_eq!(body["output_config"]["effort"].as_str(), Some("high"));
    }

    #[test]
    fn test_build_messages_body_omits_reasoning_for_unsupported_model() {
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

        let body = build_messages_body("anthropic-deployment-alias", 1024, &request);

        assert!(body.get("thinking").is_none());
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn test_build_messages_body_without_reasoning_effort_omits_thinking_and_output_config() {
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

        let body = build_messages_body("claude-opus-4-8", 1024, &request);
        assert!(body.get("thinking").is_none());
        assert!(body.get("output_config").is_none());
    }

    /// Baut eine [`ModelRequest`] mit gegebenem `reasoning_effort`, sonst
    /// minimalem Inhalt — Hilfsfunktion für die Capability-Wiring-Tests unten.
    fn request_with_effort(effort: Option<harw_types::ReasoningEffort>) -> ModelRequest {
        let mut history = harw_core::ConversationHistory::new();
        history.push_user_text("hi");
        ModelRequest {
            stream: None,
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: Vec::new(),
            context_assembly: Default::default(),
            reasoning_effort: effort,
            model_id: None,
            provider_id: None,
            data_block: None,
            max_output_tokens: None,
            tool_result_max_bytes: None,
            cancel: None,
            identity: None,
        }
    }

    #[test]
    fn test_build_messages_body_known_model_with_thinking_gets_field_and_clamped_max_tokens() {
        let request = request_with_effort(Some(harw_types::ReasoningEffort::High));

        // claude-opus-5 unterstützt adaptive thinking + effort und erlaubt
        // höchstens 128k Ausgabe-Tokens (siehe anthropic_caps::MODELS).
        let body = build_messages_body("claude-opus-5", 500_000, &request);

        assert_eq!(body["thinking"]["type"].as_str(), Some("adaptive"));
        assert_eq!(body["output_config"]["effort"].as_str(), Some("high"));
        assert_eq!(
            body.get("max_tokens").and_then(Value::as_u64),
            Some(128_000)
        );
    }

    #[test]
    fn test_build_messages_body_opus_5_5_and_fable_5_1_use_adaptive_effort_without_budget() {
        // Opus 5.5 und Fable 5.1: adaptive thinking (immer an) + effort,
        // kein Legacy-`budget_tokens`, Ausgabe auf 128k geklemmt.
        let request = request_with_effort(Some(harw_types::ReasoningEffort::Xhigh));
        for model in ["claude-opus-5-5", "claude-fable-5-1"] {
            let body = build_messages_body(model, 500_000, &request);
            assert_eq!(
                body["thinking"]["type"].as_str(),
                Some("adaptive"),
                "{model}"
            );
            assert!(body["thinking"].get("budget_tokens").is_none(), "{model}");
            assert_eq!(
                body["output_config"]["effort"].as_str(),
                Some("xhigh"),
                "{model}"
            );
            assert_eq!(
                body.get("max_tokens").and_then(Value::as_u64),
                Some(128_000),
                "{model}"
            );
        }
    }

    #[test]
    fn test_build_messages_body_model_without_thinking_support_omits_thinking_field() {
        let request = request_with_effort(Some(harw_types::ReasoningEffort::High));

        // claude-sonnet-4-5-20250929 kennt nur den Legacy-Modus (ExtendedOnly)
        // und akzeptiert `output_config.effort` gar nicht (caps.effort == false).
        let body = build_messages_body("claude-sonnet-4-5-20250929", 1024, &request);

        assert!(body.get("thinking").is_none());
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn test_build_messages_body_extended_only_model_can_still_get_effort_without_thinking() {
        let request = request_with_effort(Some(harw_types::ReasoningEffort::High));

        // claude-opus-4-5-20251101 ist ExtendedOnly (kein adaptive-thinking),
        // akzeptiert laut Tabelle aber `output_config.effort` unabhängig davon.
        let body = build_messages_body("claude-opus-4-5-20251101", 1024, &request);

        assert!(body.get("thinking").is_none());
        assert_eq!(body["output_config"]["effort"].as_str(), Some("high"));
    }

    #[test]
    fn test_build_messages_body_unknown_model_uses_conservative_fallback() {
        let request = request_with_effort(Some(harw_types::ReasoningEffort::High));

        let body = build_messages_body("totally-unknown-deployment", 900_000, &request);

        // Fail closed: keine Reasoning-Felder für unbekannte Modelle.
        assert!(body.get("thinking").is_none());
        assert!(body.get("output_config").is_none());
        // Kein Modell-Limit bekannt → max_tokens bleibt unangetastet.
        assert_eq!(
            body.get("max_tokens").and_then(Value::as_u64),
            Some(900_000)
        );
    }

    #[test]
    fn test_extract_anthropic_text_joins_blocks() {
        let body = serde_json::json!({
            "content": [
                { "type": "thinking", "text": "ignore" },
                { "type": "text", "text": "Hello, " },
                { "type": "text", "text": "world" }
            ]
        });
        assert_eq!(
            extract_anthropic_text(&body).as_deref(),
            Some("Hello, world")
        );
    }

    #[test]
    fn test_extract_anthropic_text_none_when_empty() {
        let body = serde_json::json!({ "content": [] });
        assert_eq!(extract_anthropic_text(&body), None);
    }

    #[test]
    fn test_extract_anthropic_usage_full_object() {
        let body = serde_json::json!({
            "usage": {
                "input_tokens": 123,
                "output_tokens": 456,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 78
            }
        });
        let usage = extract_anthropic_usage(&body);
        assert_eq!(usage.input_tokens, 123);
        assert_eq!(usage.output_tokens, 456);
        assert_eq!(usage.cached_tokens, Some(78));
        assert_eq!(usage.reasoning_tokens, None);
    }

    #[test]
    fn test_extract_anthropic_usage_missing_object_defaults() {
        let body = serde_json::json!({ "content": [] });
        // Anthropic meldet Cache-Tokens stets getrennt von `input_tokens`;
        // die Semantik-Markierung bleibt auch ohne `usage`-Objekt gesetzt.
        assert_eq!(
            extract_anthropic_usage(&body),
            TokenUsage {
                cache_separate: true,
                ..TokenUsage::default()
            }
        );
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

    #[test]
    fn test_build_messages_body_includes_tools_when_present() -> TestResult {
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

        let body = build_messages_body("claude-sonnet-5", 256, &request);
        let tools = body
            .get("tools")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("tools array"))?;
        assert_eq!(tools.len(), 1);
        assert_eq!(
            tools[0].get("name").and_then(Value::as_str),
            Some("get_weather")
        );
        assert_eq!(
            tools[0].get("description").and_then(Value::as_str),
            Some("Get the current weather")
        );
        assert_eq!(
            tools[0]
                .get("input_schema")
                .and_then(|s| s.get("type"))
                .and_then(Value::as_str),
            Some("object")
        );
        Ok(())
    }

    #[test]
    fn test_build_messages_body_sanitizes_dotted_tool_name_in_tools_and_history() -> TestResult {
        let mut history = harw_core::ConversationHistory::new();
        let call_id =
            harw_types::ToolCallId::try_from_str("call-1").map_err(ctx("valid call id"))?;
        history.push_tool_call(call_id, "fs.read", serde_json::json!({"path": "/tmp"}));
        let dotted_tool = ToolSpec::Function(harw_tools::FunctionToolSpec {
            name: ToolName::new("fs.read"),
            description: "Reads a file".to_owned(),
            parameters: harw_tools::JsonSchema {
                schema_type: Some(harw_tools::JsonSchemaType::Object),
                ..Default::default()
            },
            strict: false,
        });
        let request = ModelRequest {
            stream: None,
            system_prompt: String::new(),
            instruction_fragments: Vec::new(),
            context: Vec::new(),
            history,
            tools: vec![dotted_tool],
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

        let body = build_messages_body("claude-sonnet-5", 256, &request);

        let tools = body
            .get("tools")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("tools array"))?;
        assert_eq!(
            tools[0].get("name").and_then(Value::as_str),
            Some("fs_read")
        );

        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("messages array"))?;
        let tool_use_block = messages[0]
            .get("content")
            .and_then(Value::as_array)
            .and_then(|content| content.first())
            .ok_or(TestError::Missing("tool_use content block"))?;
        assert_eq!(
            tool_use_block.get("type").and_then(Value::as_str),
            Some("tool_use")
        );
        assert_eq!(
            tool_use_block.get("name").and_then(Value::as_str),
            Some("fs_read")
        );
        Ok(())
    }

    #[test]
    fn test_build_messages_body_omits_tools_when_empty() {
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
        let body = build_messages_body("m", 256, &request);
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn test_build_messages_body_maps_tool_call_to_tool_use_block() -> TestResult {
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
        let body = build_messages_body("m", 256, &request);
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("messages"))?;
        assert_eq!(messages.len(), 1);
        assert_eq!(
            messages[0].get("role").and_then(Value::as_str),
            Some("assistant")
        );
        let content = messages[0]
            .get("content")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("content array"))?;
        assert_eq!(
            content[0].get("type").and_then(Value::as_str),
            Some("tool_use")
        );
        assert_eq!(content[0].get("id").and_then(Value::as_str), Some("call_1"));
        assert_eq!(
            content[0].get("name").and_then(Value::as_str),
            Some("get_weather")
        );
        assert_eq!(
            content[0]
                .get("input")
                .and_then(|i| i.get("location"))
                .and_then(Value::as_str),
            Some("Paris")
        );
        Ok(())
    }

    #[test]
    fn test_build_messages_body_maps_tool_result_ok_to_tool_result_block() -> TestResult {
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
        let body = build_messages_body("m", 256, &request);
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("messages"))?;
        assert_eq!(
            messages[0].get("role").and_then(Value::as_str),
            Some("user")
        );
        let content = messages[0]
            .get("content")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("content array"))?;
        assert_eq!(
            content[0].get("type").and_then(Value::as_str),
            Some("tool_result")
        );
        assert_eq!(
            content[0].get("tool_use_id").and_then(Value::as_str),
            Some("call_1")
        );
        assert!(content[0].get("is_error").is_none());
        Ok(())
    }

    #[test]
    fn test_build_messages_body_maps_tool_result_err_sets_is_error() -> TestResult {
        let mut history = harw_core::ConversationHistory::new();
        history.push_tool_result(
            harw_types::ToolCallId::from_str("call_1"),
            ToolCallResult::error("boom"),
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
        let body = build_messages_body("m", 256, &request);
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("messages"))?;
        let content = messages[0]
            .get("content")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("content array"))?;
        // Untrusted (Default von `push_tool_result`) ⇒ Trust-Hülle um die
        // Fehlermeldung (gemeinsamer Renderer, W3 C-PROTO).
        let rendered = content[0]
            .get("content")
            .and_then(Value::as_str)
            .ok_or(TestError::Missing("tool_result content"))?;
        assert!(rendered.starts_with(harw_core::envelope::UNTRUSTED_BEGIN_PREFIX));
        assert!(rendered.contains("status=error"));
        assert!(rendered.contains("| boom"));
        assert_eq!(
            content[0].get("is_error").and_then(Value::as_bool),
            Some(true)
        );
        Ok(())
    }

    fn request_with_history(history: harw_core::ConversationHistory) -> ModelRequest {
        ModelRequest {
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
        }
    }

    fn first_tool_result_content(body: &Value) -> TestResult<String> {
        body.get("messages")
            .and_then(Value::as_array)
            .and_then(|messages| messages.first())
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
            .and_then(|blocks| blocks.first())
            .and_then(|block| block.get("content"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or(TestError::Missing("tool_result content"))
    }

    #[test]
    fn test_build_messages_body_caps_tool_result_at_default_budget() -> TestResult {
        let mut history = harw_core::ConversationHistory::new();
        history.push_tool_result(
            harw_types::ToolCallId::from_str("call_big"),
            ToolCallResult::success(Value::String("x".repeat(200 * 1024))),
            5,
        );
        let body = build_messages_body("m", 256, &request_with_history(history));
        let content = first_tool_result_content(&body)?;
        assert!(content.len() <= DEFAULT_TOOL_RESULT_MAX_BYTES);
        assert!(content.contains("[truncated: showing"));
        assert!(content.contains(harw_core::envelope::UNTRUSTED_END_PREFIX));
        Ok(())
    }

    #[test]
    fn test_build_messages_body_caps_tool_result_at_request_hint() -> TestResult {
        let mut history = harw_core::ConversationHistory::new();
        history.push_tool_result(
            harw_types::ToolCallId::from_str("call_big"),
            ToolCallResult::success(Value::String("y".repeat(16 * 1024))),
            5,
        );
        let request = request_with_history(history).with_tool_result_max_bytes(Some(2_048));
        let body = build_messages_body("m", 256, &request);
        let content = first_tool_result_content(&body)?;
        assert!(content.len() <= 2_048);
        assert!(content.contains("[truncated: showing"));
        Ok(())
    }

    #[test]
    fn test_build_messages_body_small_tool_result_is_not_truncated() -> TestResult {
        let mut history = harw_core::ConversationHistory::new();
        history.push_tool_result(
            harw_types::ToolCallId::from_str("call_small"),
            ToolCallResult::success(serde_json::json!({"temp": 20})),
            5,
        );
        let body = build_messages_body("m", 256, &request_with_history(history));
        let content = first_tool_result_content(&body)?;
        assert!(content.contains(r#"| {"temp":20}"#));
        assert!(!content.contains("[truncated:"));
        Ok(())
    }

    #[test]
    fn test_build_messages_body_honors_request_max_output_tokens() {
        let request = request_with_history(harw_core::ConversationHistory::new())
            .with_max_output_tokens(Some(2_000));
        let body = build_messages_body("totally-unknown-deployment", DEFAULT_MAX_TOKENS, &request);
        assert_eq!(body.get("max_tokens").and_then(Value::as_u64), Some(2_000));
    }

    #[test]
    fn test_build_messages_body_request_max_output_tokens_is_clamped_by_caps() {
        let request = request_with_history(harw_core::ConversationHistory::new())
            .with_max_output_tokens(Some(u32::MAX));
        let body = build_messages_body("claude-opus-5", DEFAULT_MAX_TOKENS, &request);
        let expected = crate::anthropic_caps::clamp_max_tokens("claude-opus-5", u32::MAX);
        assert!(
            expected < u32::MAX,
            "claude-opus-5 must have a known output cap"
        );
        assert_eq!(
            body.get("max_tokens").and_then(Value::as_u64),
            Some(u64::from(expected))
        );
    }

    #[test]
    fn test_build_messages_body_zero_or_missing_max_output_uses_default() {
        for hint in [None, Some(0)] {
            let request = request_with_history(harw_core::ConversationHistory::new())
                .with_max_output_tokens(hint);
            let body =
                build_messages_body("totally-unknown-deployment", DEFAULT_MAX_TOKENS, &request);
            assert_eq!(
                body.get("max_tokens").and_then(Value::as_u64),
                Some(u64::from(DEFAULT_MAX_TOKENS)),
                "hint {hint:?}"
            );
        }
        assert_eq!(DEFAULT_MAX_TOKENS, 16_384);
    }

    #[test]
    fn test_extract_anthropic_stop_reason_maps_context_window_exceeded() {
        let body = serde_json::json!({"stop_reason": "model_context_window_exceeded"});
        assert_eq!(
            extract_anthropic_stop_reason(&body),
            StopReason::ContextWindowExceeded
        );
        let other = serde_json::json!({"stop_reason": "something_new"});
        assert_eq!(
            extract_anthropic_stop_reason(&other),
            StopReason::Other("something_new".to_owned())
        );
    }

    #[test]
    fn test_build_messages_body_groups_parallel_tool_calls_and_results_alternate_roles()
    -> TestResult {
        let mut history = harw_core::ConversationHistory::new();
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
        let body = build_messages_body("m", 256, &request);
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("messages"))?;

        // one grouped assistant message, one grouped user (tool_result) message.
        assert_eq!(messages.len(), 2);

        assert_eq!(
            messages[0].get("role").and_then(Value::as_str),
            Some("assistant")
        );
        let assistant_content = messages[0]
            .get("content")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("assistant content array"))?;
        assert_eq!(assistant_content.len(), 2);
        assert_eq!(
            assistant_content[0].get("type").and_then(Value::as_str),
            Some("tool_use")
        );
        assert_eq!(
            assistant_content[0].get("id").and_then(Value::as_str),
            Some("call_a")
        );
        assert_eq!(
            assistant_content[1].get("id").and_then(Value::as_str),
            Some("call_b")
        );

        assert_eq!(
            messages[1].get("role").and_then(Value::as_str),
            Some("user")
        );
        let user_content = messages[1]
            .get("content")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("user content array"))?;
        assert_eq!(user_content.len(), 2);
        assert_eq!(
            user_content[0].get("type").and_then(Value::as_str),
            Some("tool_result")
        );
        assert_eq!(
            user_content[0].get("tool_use_id").and_then(Value::as_str),
            Some("call_a")
        );
        assert_eq!(
            user_content[1].get("tool_use_id").and_then(Value::as_str),
            Some("call_b")
        );
        Ok(())
    }

    #[test]
    fn test_build_messages_body_grouped_tool_result_keeps_is_error_flag() -> TestResult {
        let mut history = harw_core::ConversationHistory::new();
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
            ToolCallResult::error("boom"),
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
        let body = build_messages_body("m", 256, &request);
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("messages"))?;
        let user_content = messages[1]
            .get("content")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("user content array"))?;
        assert!(user_content[0].get("is_error").is_none());
        assert_eq!(
            user_content[1].get("is_error").and_then(Value::as_bool),
            Some(true)
        );
        Ok(())
    }

    #[test]
    fn test_build_messages_body_assistant_text_message_does_not_absorb_tool_use() -> TestResult {
        let mut history = harw_core::ConversationHistory::new();
        history.push_assistant_text("here you go", None);
        history.push_tool_call(
            harw_types::ToolCallId::from_str("call_a"),
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
        let body = build_messages_body("m", 256, &request);
        let messages = body
            .get("messages")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("messages"))?;

        // Text message keeps a string `content`; the tool_use block must land in
        // a separate, new assistant message rather than being appended to it.
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[0].get("role").and_then(Value::as_str),
            Some("assistant")
        );
        assert!(
            messages[0]
                .get("content")
                .ok_or(TestError::Missing("content"))?
                .is_string()
        );

        assert_eq!(
            messages[1].get("role").and_then(Value::as_str),
            Some("assistant")
        );
        let content = messages[1]
            .get("content")
            .and_then(Value::as_array)
            .ok_or(TestError::Missing("content array"))?;
        assert_eq!(
            content[0].get("type").and_then(Value::as_str),
            Some("tool_use")
        );
        Ok(())
    }

    #[test]
    fn test_extract_anthropic_tool_calls_parses_tool_use_blocks() -> TestResult {
        let body = serde_json::json!({
            "content": [
                { "type": "text", "text": "Let me check." },
                {
                    "type": "tool_use",
                    "id": "toolu_01",
                    "name": "get_weather",
                    "input": {"location": "Paris"}
                }
            ]
        });
        let calls = extract_anthropic_tool_calls(&body).map_err(ctx("valid tool-use input"))?;
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id.as_str(), "toolu_01");
        assert_eq!(calls[0].name.as_str(), "get_weather");
        assert_eq!(
            calls[0].arguments.get("location").and_then(Value::as_str),
            Some("Paris")
        );
        Ok(())
    }

    #[test]
    fn test_extract_anthropic_tool_calls_empty_when_no_tool_use() -> TestResult {
        let body = serde_json::json!({ "content": [{ "type": "text", "text": "hi" }] });
        assert!(
            extract_anthropic_tool_calls(&body)
                .map_err(ctx("no tool-use blocks"))?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn test_extract_anthropic_tool_calls_rejects_missing_id_without_partial_calls() {
        let body = serde_json::json!({
            "content": [
                {
                    "type": "tool_use",
                    "id": "toolu_valid",
                    "name": "valid_tool",
                    "input": {}
                },
                { "type": "tool_use", "name": "missing_id", "input": {} }
            ]
        });

        assert!(matches!(
            extract_anthropic_tool_calls(&body),
            Err(AnthropicToolCallParseError::MissingId)
        ));
    }

    #[test]
    fn test_extract_anthropic_tool_calls_rejects_missing_name_without_partial_calls() {
        let body = serde_json::json!({
            "content": [
                {
                    "type": "tool_use",
                    "id": "toolu_valid",
                    "name": "valid_tool",
                    "input": {}
                },
                { "type": "tool_use", "id": "missing_name", "input": {} }
            ]
        });

        assert!(matches!(
            extract_anthropic_tool_calls(&body),
            Err(AnthropicToolCallParseError::MissingName)
        ));
    }

    #[test]
    fn test_extract_anthropic_tool_calls_rejects_non_string_id_and_name() {
        for (field, expected_error) in [
            ("id", AnthropicToolCallParseError::MissingId),
            ("name", AnthropicToolCallParseError::MissingName),
        ] {
            let body = serde_json::json!({
                "content": [
                    {
                        "type": "tool_use",
                        "id": "toolu_valid",
                        "name": "valid_tool",
                        "input": {}
                    },
                    {
                        "type": "tool_use",
                        "id": "toolu_invalid",
                        "name": "invalid_tool",
                        "input": {}
                    }
                ]
            });
            let mut body = body;
            body["content"][1][field] = serde_json::json!(42);

            assert!(matches!(
                extract_anthropic_tool_calls(&body),
                Err(error) if error == expected_error
            ));
        }
    }

    #[test]
    fn test_extract_anthropic_tool_calls_rejects_missing_input() {
        let body = serde_json::json!({
            "content": [{"type": "tool_use", "id": "toolu_01", "name": "get_weather"}]
        });

        assert!(extract_anthropic_tool_calls(&body).is_err());
    }

    #[test]
    fn test_extract_anthropic_tool_calls_rejects_null_input() {
        let body = serde_json::json!({
            "content": [{
                "type": "tool_use",
                "id": "toolu_01",
                "name": "get_weather",
                "input": null
            }]
        });

        assert!(extract_anthropic_tool_calls(&body).is_err());
    }

    #[test]
    fn test_extract_anthropic_tool_calls_rejects_scalar_input() {
        let body = serde_json::json!({
            "content": [{
                "type": "tool_use",
                "id": "toolu_01",
                "name": "get_weather",
                "input": "Paris"
            }]
        });

        assert!(extract_anthropic_tool_calls(&body).is_err());
    }

    #[test]
    fn test_extract_anthropic_tool_calls_accepts_empty_object_input() -> TestResult {
        let body = serde_json::json!({
            "content": [{
                "type": "tool_use",
                "id": "toolu_01",
                "name": "get_weather",
                "input": {}
            }]
        });

        let calls =
            extract_anthropic_tool_calls(&body).map_err(ctx("empty object is valid input"))?;
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0]
                .arguments
                .as_object()
                .is_some_and(|object| object.is_empty())
        );
        Ok(())
    }

    #[test]
    fn anthropic_credential_headers_are_marked_sensitive() -> TestResult {
        let secret = "sk-ant-sensitive-header-value";
        for (credential, header_name) in [
            (
                AnthropicCredential::ApiKey(SecretString::new(secret.into())),
                "x-api-key",
            ),
            (
                AnthropicCredential::OAuth(SecretString::new(secret.into())),
                "authorization",
            ),
            (
                AnthropicCredential::Bearer(SecretString::new(secret.into())),
                "authorization",
            ),
        ] {
            let builder = super::super::http_client()
                .map_err(ctx("http_client"))?
                .post("https://api.anthropic.com/v1/messages");
            let request = apply_anthropic_credential(builder, &credential)
                .map_err(ctx("credential header"))?
                .build()
                .map_err(ctx("request builds"))?;
            let value = request
                .headers()
                .get(header_name)
                .ok_or(TestError::Missing(header_name))?;
            assert!(value.is_sensitive(), "{header_name}");
            assert!(!format!("{:?}", request.headers()).contains(secret));
        }
        Ok(())
    }

    #[test]
    fn official_anthropic_host_matches_default_base_url() -> TestResult {
        assert_eq!(
            reqwest::Url::parse(DEFAULT_ANTHROPIC_BASE_URL)
                .map_err(ctx("default base URL"))?
                .host_str(),
            Some(ANTHROPIC_API_HOST)
        );
        Ok(())
    }

    // ── Nebenläufigkeit & Rate-Limit-Sichtbarkeit (W6b-Parität für Anthropic) ──

    #[test]
    fn set_max_concurrency_applies_immediately_to_available_permits() -> TestResult {
        let mut provider = test_provider()?;
        provider.configure_concurrency(None);

        assert!(
            crate::ProviderLoadControl::set_max_concurrency(&provider, Some(2)),
            "provider built with configure_concurrency always has a limiter"
        );
        let status = crate::ProviderLoadControl::provider_status(&provider);
        assert_eq!(status.max_concurrency, Some(2));
        assert_eq!(status.available_permits, 2);
        Ok(())
    }

    #[test]
    fn provider_load_control_reports_false_without_configured_limiter() -> TestResult {
        let provider = test_provider()?;
        assert!(!crate::ProviderLoadControl::set_max_concurrency(
            &provider,
            Some(2)
        ));
        Ok(())
    }

    /// Startet einen Mock-HTTP-Server, der pro Verbindung einen eigenen
    /// Thread spawnt, die aktuell gleichzeitig offenen Verbindungen zählt,
    /// den beobachteten Höchststand (`peak`) trackt, künstlich verzögert und
    /// dann eine minimale, gültige Anthropic-Messages-Antwort zurückgibt.
    /// Gleiches Muster wie `lib.rs::tests::mock_concurrency_probe_server`,
    /// hier lokal dupliziert, weil dieses Modul keinen Zugriff auf das
    /// private Test-Fixture von `lib.rs` hat.
    fn mock_anthropic_concurrency_probe_server(
        request_count: usize,
    ) -> TestResult<(
        String,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        std::thread::JoinHandle<TestResult>,
    )> {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").map_err(ctx("bind mock server"))?;
        let base_url = format!(
            "http://{}/v1/messages",
            listener.local_addr().map_err(ctx("mock address"))?
        );
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let peak_for_thread = Arc::clone(&peak);
        let handle = thread::spawn(move || -> TestResult {
            let mut connection_handles = Vec::with_capacity(request_count);
            for _ in 0..request_count {
                let (mut stream, _) = listener.accept().map_err(ctx("accept mock request"))?;
                let in_flight = Arc::clone(&in_flight);
                let peak = Arc::clone(&peak_for_thread);
                connection_handles.push(thread::spawn(move || -> TestResult {
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
                    let body = br#"{"content":[{"type":"text","text":"mock"}]}"#;
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
                handle.join().map_err(|_| {
                    TestError::Unexpected("mock connection handler panicked".to_owned())
                })??;
            }
            Ok(())
        });
        Ok((base_url, peak, handle))
    }

    #[tokio::test]
    async fn respond_honors_max_concurrency_hard_cap() -> TestResult {
        const MAX_CONCURRENCY: usize = 2;
        const REQUEST_COUNT: usize = 5;

        let (base_url, peak, server) = mock_anthropic_concurrency_probe_server(REQUEST_COUNT)?;

        let mut provider = AnthropicMessagesProvider::new(
            base_url,
            "configured-model",
            AnthropicCredential::ApiKey(SecretString::new("sk-secret".into())),
        )
        .map_err(ctx("AnthropicMessagesProvider::new"))?;
        provider.configure_concurrency(Some(MAX_CONCURRENCY));
        let provider = Arc::new(provider);

        let mut handles = Vec::with_capacity(REQUEST_COUNT);
        for _ in 0..REQUEST_COUNT {
            let provider = Arc::clone(&provider);
            handles.push(tokio::spawn(async move {
                provider
                    .respond(request_with_ids(None, None))
                    .await
                    .map(|_| ())
                    .map_err(ctx("mock anthropic response parses"))
            }));
        }
        for handle in handles {
            handle.await.map_err(ctx("respond task completes"))??;
        }
        server
            .join()
            .map_err(|_| TestError::Unexpected("mock server thread panicked".to_owned()))??;

        assert!(
            peak.load(std::sync::atomic::Ordering::SeqCst) <= MAX_CONCURRENCY,
            "observed more than {MAX_CONCURRENCY} requests in flight simultaneously"
        );
        Ok(())
    }

    #[tokio::test]
    async fn respond_records_rate_limited_count_on_429() -> TestResult {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").map_err(ctx("bind mock server"))?;
        let base_url = format!(
            "http://{}/v1/messages",
            listener.local_addr().map_err(ctx("mock address"))?
        );
        let server = thread::spawn(move || -> TestResult {
            let (mut stream, _) = listener.accept().map_err(ctx("accept mock request"))?;
            let mut request_buffer = [0_u8; 4096];
            let _read = stream
                .read(&mut request_buffer)
                .map_err(ctx("read mock request"))?;
            let body =
                br#"{"type":"error","error":{"type":"rate_limit_error","message":"rate limited"}}"#;
            let headers = format!(
                "HTTP/1.1 429 Too Many Requests\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\nretry-after: 1\r\n\r\n",
                body.len()
            );
            stream
                .write_all(headers.as_bytes())
                .map_err(ctx("write mock response headers"))?;
            stream
                .write_all(body)
                .map_err(ctx("write mock response body"))?;
            Ok(())
        });

        let provider = AnthropicMessagesProvider::new(
            base_url,
            "configured-model",
            AnthropicCredential::ApiKey(SecretString::new("sk-secret".into())),
        )
        .map_err(ctx("AnthropicMessagesProvider::new"))?;

        assert_eq!(provider.rate_limiter_handle().rate_limited_count(), 0);
        let response = provider.respond(request_with_ids(None, None)).await;
        let Err(error) = response else {
            return Err(TestError::Unexpected(
                "429 must surface as an error".to_owned(),
            ));
        };
        assert!(matches!(error, ModelError::RateLimited { .. }));
        assert_eq!(provider.rate_limiter_handle().rate_limited_count(), 1);

        server
            .join()
            .map_err(|_| TestError::Unexpected("mock server thread panicked".to_owned()))??;

        Ok(())
    }

    #[test]
    fn test_anthropic_error_for_status_529_overloaded_is_transient() {
        let body = r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        let error = anthropic_error_for_status(529, Some("req-1"), None, None, body);
        assert!(matches!(
            error,
            ModelError::Transient {
                status: Some(529),
                retry_after_secs: None,
                ..
            }
        ));
        assert!(error.is_retryable());
    }

    #[test]
    fn test_anthropic_error_for_status_503_is_transient_with_retry_after() {
        let error = anthropic_error_for_status(503, None, Some("4"), None, "upstream down");
        assert!(matches!(
            error,
            ModelError::Transient {
                status: Some(503),
                retry_after_secs: Some(4),
                ..
            }
        ));
        assert!(error.is_retryable());
        assert!(!error.to_string().contains("upstream down"));
    }

    #[test]
    fn test_anthropic_error_for_status_429_uses_retry_after_ms() {
        let body =
            r#"{"type":"error","error":{"type":"rate_limit_error","message":"rate limited"}}"#;
        let error = anthropic_error_for_status(429, None, Some("30"), Some("1500"), body);
        assert!(matches!(
            error,
            ModelError::RateLimited {
                retry_after_secs: 2,
                ..
            }
        ));
        assert!(error.is_retryable());
    }

    #[test]
    fn test_anthropic_error_for_status_429_without_hint_falls_back_to_default() {
        let error = anthropic_error_for_status(429, None, None, None, "{}");
        assert!(matches!(
            error,
            ModelError::RateLimited {
                retry_after_secs: 30,
                ..
            }
        ));
    }

    #[test]
    fn test_anthropic_error_for_status_auth_and_invalid_are_not_retryable() {
        let auth = anthropic_error_for_status(401, None, None, None, "{}");
        assert!(matches!(auth, ModelError::Auth { .. }));
        assert!(!auth.is_retryable());
        let forbidden = anthropic_error_for_status(403, None, None, None, "{}");
        assert!(matches!(forbidden, ModelError::Auth { .. }));
        let invalid = anthropic_error_for_status(
            400,
            None,
            None,
            None,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad"}}"#,
        );
        assert!(matches!(invalid, ModelError::RequestFailed(_)));
        assert!(!invalid.is_retryable());
    }

    #[tokio::test]
    async fn respond_maps_request_timeout_to_retryable_timeout() -> TestResult {
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind("127.0.0.1:0").map_err(ctx("bind mock server"))?;
        let base_url = format!(
            "http://{}/v1/messages",
            listener.local_addr().map_err(ctx("mock address"))?
        );
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let server = thread::spawn(move || -> TestResult {
            // Verbindung annehmen, aber nie antworten, bis der Test fertig ist.
            let (_stream, _) = listener.accept().map_err(ctx("accept mock request"))?;
            let _ = release_rx.recv_timeout(Duration::from_secs(10));
            Ok(())
        });

        let mut provider = AnthropicMessagesProvider::new(
            base_url,
            "configured-model",
            AnthropicCredential::ApiKey(SecretString::new("sk-secret".into())),
        )
        .map_err(ctx("AnthropicMessagesProvider::new"))?;
        provider.request_timeout = Duration::from_millis(100);

        let response = provider.respond(request_with_ids(None, None)).await;
        let _ = release_tx.send(());
        server
            .join()
            .map_err(|_| TestError::Unexpected("mock server thread panicked".to_owned()))??;

        let Err(error) = response else {
            return Err(TestError::Unexpected(
                "stalled server must surface as an error".to_owned(),
            ));
        };
        assert!(
            matches!(error, ModelError::Timeout { .. }),
            "expected Timeout, got {error:?}"
        );
        assert!(error.is_retryable());
        Ok(())
    }
}
