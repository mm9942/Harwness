//! Modell-Discovery für konfigurierte Provider (`harw models scan`).
//!
//! ## Verantwortung
//! Diese Datei besitzt die Abfrage der `/models`-Endpunkte konfigurierter
//! Provider (`openai-chat`/`openai-responses`/`ollama` → `{base_url}/models`,
//! `anthropic-messages` → `{base_url}/v1/models`) und die Übersetzung der
//! Antworten in provider-neutrale [`DiscoveredModel`]-Einträge. Sie
//! delegiert Credential-Auflösung an das bestehende `resolve_secret`/
//! `SecretSources`-Paar der Crate (siehe `crate::resolve_secret`) über
//! [`resolve_provider_api_key`].
//!
//! ## Nebenläufigkeit
//! [`list_models`] ist eine reine `async fn` ohne geteilten Zustand; sie baut
//! bei jedem Aufruf einen frischen `reqwest::Client` über `crate::http_client`
//! (geteilt-günstig, `Send + Sync`).
//!
//! ## Fehler
//! [`DiscoveryError`] klassifiziert Auth- (401/403), API- (andere Nicht-2xx),
//! Netzwerk- (Transportfehler) und Decode-Fehler (ungültiges JSON), sowie
//! nicht unterstützte Provider-APIs.
//!
//! ## Sicherheit
//! Der API-Schlüssel wird ausschließlich beim Setzen des Auth-Headers
//! verwendet und nie geloggt oder in einer Fehlermeldung ausgegeben.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use secrecy::ExposeSecret;
use serde_json::Value;

/// Zeitlimit für eine einzelne Discovery-Anfrage.
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(20);

/// Ein von einem Provider gemeldetes Modell, normalisiert über Provider-APIs
/// hinweg.
///
/// # Description
/// Preisangaben und Tool-Unterstützung sind optional, da nur OpenRouter-
/// kompatible Antworten (`pricing`, `supported_parameters`) sie liefern;
/// reine OpenAI-/Anthropic-Antworten liefern nur `id`.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveredModel {
    /// Modell-ID, wie vom Provider gemeldet (z. B. `"gpt-4o"`,
    /// `"nvidia/nemotron-3.5-lightning"`).
    pub id: String,
    /// Kontextfenster in Token, sofern gemeldet.
    pub context_length: Option<u64>,
    /// Eingabepreis in USD je Million Token, sofern gemeldet (aus
    /// `pricing.prompt`, einem Preis pro Token, hochgerechnet ×1_000_000).
    pub input_price_per_mtok: Option<f64>,
    /// Ausgabepreis in USD je Million Token, sofern gemeldet (aus
    /// `pricing.completion`, analog hochgerechnet).
    pub output_price_per_mtok: Option<f64>,
    /// `true`, wenn `supported_parameters` den Wert `"tools"` enthält;
    /// `None`, wenn das Feld fehlt (Provider meldet keine Auskunft).
    pub supports_tools: Option<bool>,
}

/// Fehler der Modell-Discovery gegen einen konfigurierten Provider.
pub enum DiscoveryError {
    /// Provider hat den API-Schlüssel abgelehnt (HTTP 401/403).
    Auth {
        /// HTTP-Statuscode der Antwort.
        status: u16,
        /// Kurze, menschenlesbare Zusatzinformation.
        detail: String,
    },
    /// Transportfehler (Verbindung, Timeout, TLS) vor Erhalt einer Antwort.
    Network {
        /// Kurze, menschenlesbare Zusatzinformation.
        detail: String,
    },
    /// Provider antwortete mit einem anderen Nicht-Erfolgsstatus.
    Api {
        /// HTTP-Statuscode der Antwort.
        status: u16,
        /// Kurze, menschenlesbare Zusatzinformation.
        detail: String,
    },
    /// Die Antwort konnte nicht als das erwartete JSON-Schema gelesen werden.
    Decode {
        /// Kurze, menschenlesbare Zusatzinformation.
        detail: String,
    },
    /// Der Provider verwendet eine API, für die keine Modell-Discovery
    /// implementiert ist.
    Unsupported {
        /// Der nicht unterstützte `api`-Wert aus der Provider-Konfiguration.
        api: String,
    },
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiscoveryError::Auth { status, detail } => {
                write!(f, "Anmeldung fehlgeschlagen (HTTP {status}): {detail}")
            }
            DiscoveryError::Network { detail } => {
                write!(f, "Netzwerkfehler: {detail}")
            }
            DiscoveryError::Api { status, detail } => {
                write!(f, "Provider antwortete mit Fehler (HTTP {status}): {detail}")
            }
            DiscoveryError::Decode { detail } => {
                write!(f, "Antwort konnte nicht gelesen werden: {detail}")
            }
            DiscoveryError::Unsupported { api } => {
                write!(f, "Modell-Discovery wird für die Provider-API '{api}' nicht unterstützt")
            }
        }
    }
}

impl fmt::Debug for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

impl std::error::Error for DiscoveryError {}

/// Löst den API-Schlüssel eines konfigurierten Providers auf, sofern eine
/// `auth`-`SecretRef` gesetzt ist.
///
/// # Description
/// Verwendet denselben optionalen sealed-secret-Resolver und dasselbe
/// harw-Home wie die Runtime. Ein Provider mit `auth = "secrets:…"` oder
/// `file:`-Credentials muss auch mit `harw models scan` auffindbar sein.
/// Nicht auflösbare Referenzen führen zu `None`, nicht zu einem Fehler — der
/// Aufrufer meldet fehlende Auth separat.
///
/// # Arguments
/// - `provider_name`: Name des Providers, nur für Diagnose-Logging.
/// - `provider` (`&harw_config::ProviderToml`): Konfigurationseintrag mit
///   optionaler `auth`-`SecretRef`.
/// - `config` (`&harw_config::ResolvedConfig`): liefert den Env-Layer für
///   `env:`-Referenzen.
///
/// # Returns
/// Den Klartext-Schlüssel, wenn eine `auth`-Referenz gesetzt und auflösbar
/// ist; sonst `None`. Der Wert wird nie geloggt.
#[must_use]
pub fn resolve_provider_api_key(
    provider_name: &str,
    provider: &harw_config::ProviderToml,
    config: &harw_config::ResolvedConfig,
    home: Option<&Path>,
    resolver: Option<&dyn crate::SecretResolver>,
) -> Option<String> {
    let reference = provider.auth.as_ref()?;
    let sources = crate::SecretSources {
        env_layer: &config.env_layer,
        resolver,
        home,
        endpoint: Some(&provider.base_url),
    };
    match crate::resolve_secret(reference, sources) {
        Ok(secret) => Some(secret.expose_secret().to_owned()),
        Err(error) => {
            tracing::debug!(
                provider = provider_name,
                error = %error,
                "Konnte API-Schlüssel für Provider nicht auflösen"
            );
            None
        }
    }
}

/// Fragt die Modelliste eines konfigurierten Providers ab.
///
/// # Description
/// Wählt anhand von `provider.api` den `/models`-Endpunkt:
/// - `openai-chat`/`openai-responses`: `GET {base_url}/models`.
/// - `ollama`: `GET {base_url}/models`, wobei `base_url` wie in
///   `OpenAiResponsesProvider::from_named_config` auf ein einzelnes
///   nachgestelltes `/v1` normalisiert wird.
/// - `anthropic-messages`: `GET {base_url}/v1/models` mit `x-api-key` +
///   `anthropic-version`.
///
/// Andere `api`-Werte liefern [`DiscoveryError::Unsupported`].
///
/// # Arguments
/// - `provider_name`: Name des Providers, für Fehlermeldungen.
/// - `provider` (`&harw_config::ProviderToml`): liefert `api` und `base_url`.
/// - `api_key`: aufgelöster Klartext-Schlüssel (z. B. aus
///   [`resolve_provider_api_key`]); `None` sendet die Anfrage ohne
///   Auth-Header (z. B. lokales Ollama).
///
/// # Errors
/// - [`DiscoveryError::Auth`]: HTTP 401/403.
/// - [`DiscoveryError::Api`]: andere Nicht-Erfolgsstatus.
/// - [`DiscoveryError::Network`]: Verbindungs-/Timeout-Fehler.
/// - [`DiscoveryError::Decode`]: Antwort ist kein gültiges JSON.
/// - [`DiscoveryError::Unsupported`]: `provider.api` ohne Discovery-Pfad.
///
/// # Concurrency
/// Reine `async fn`; sicher aus mehreren Tasks parallel aufrufbar.
pub async fn list_models(
    provider_name: &str,
    provider: &harw_config::ProviderToml,
    api_key: Option<&str>,
) -> Result<Vec<DiscoveredModel>, DiscoveryError> {
    let base = provider.base_url.trim_end_matches('/');
    let url = match provider.api.as_str() {
        "openai-chat" | "openai-responses" => format!("{base}/models"),
        "ollama" => {
            let normalized = format!("{}/v1", base.trim_end_matches("/v1"));
            format!("{normalized}/models")
        }
        "anthropic-messages" => {
            // Direct Anthropic configurations commonly use either the API
            // origin or an already versioned `/v1` base URL.  Do not turn
            // the latter into the invalid `/v1/v1/models` path.
            let api_base = base.strip_suffix("/v1").unwrap_or(base);
            format!("{api_base}/v1/models")
        }
        other => {
            return Err(DiscoveryError::Unsupported {
                api: other.to_owned(),
            });
        }
    };

    let client = crate::http_client();
    let mut request = client.get(&url).timeout(DISCOVERY_TIMEOUT);
    if provider.api == "anthropic-messages" {
        if let Some(key) = api_key {
            request = request
                .header("x-api-key", key)
                .header("anthropic-version", crate::anthropic::ANTHROPIC_VERSION);
        }
    } else if let Some(key) = api_key {
        request = request.bearer_auth(key);
    }

    let response = request.send().await.map_err(|error| DiscoveryError::Network {
        detail: classify_transport_detail(&error),
    })?;

    let status = response.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(DiscoveryError::Auth {
            status: status.as_u16(),
            detail: format!("Provider '{provider_name}' hat den API-Schlüssel abgelehnt"),
        });
    }
    if !status.is_success() {
        return Err(DiscoveryError::Api {
            status: status.as_u16(),
            detail: format!("Provider '{provider_name}' antwortete mit Status {status}"),
        });
    }

    let body: Value = response.json().await.map_err(|_error| DiscoveryError::Decode {
        detail: format!("Antwort von Provider '{provider_name}' ist kein gültiges JSON"),
    })?;
    Ok(parse_models_response(&body))
}

/// Beschreibt einen `reqwest`-Transportfehler kurz, ohne die Ziel-URL oder
/// Credentials preiszugeben.
fn classify_transport_detail(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "Zeitüberschreitung beim Verbindungsaufbau".to_owned()
    } else if error.is_connect() {
        "Verbindung konnte nicht aufgebaut werden".to_owned()
    } else {
        "Anfrage konnte nicht gesendet werden".to_owned()
    }
}

/// Parst eine `{"data": [...]}`-Modellliste (OpenAI-/OpenRouter-/Anthropic-
/// Schema) in [`DiscoveredModel`]-Einträge.
///
/// # Description
/// Reine, netzwerkfreie Hilfsfunktion für Tests und [`list_models`]. Einträge
/// ohne lesbares `id`-Feld werden übersprungen; alle anderen Felder sind
/// best-effort (`None`, wenn abwesend oder vom falschen Typ).
fn parse_models_response(body: &Value) -> Vec<DiscoveredModel> {
    body.get("data")
        .and_then(Value::as_array)
        .map(|entries| entries.iter().filter_map(parse_one_model).collect())
        .unwrap_or_default()
}

/// Parst ein einzelnes Modell-Objekt aus einer `/models`-Antwort.
fn parse_one_model(entry: &Value) -> Option<DiscoveredModel> {
    let id = entry.get("id")?.as_str()?.to_owned();
    let context_length = entry.get("context_length").and_then(Value::as_u64);
    let (input_price_per_mtok, output_price_per_mtok) = entry
        .get("pricing")
        .map(|pricing| {
            let input = pricing
                .get("prompt")
                .and_then(Value::as_str)
                .and_then(|raw| raw.parse::<f64>().ok())
                .map(|per_token| per_token * 1_000_000.0);
            let output = pricing
                .get("completion")
                .and_then(Value::as_str)
                .and_then(|raw| raw.parse::<f64>().ok())
                .map(|per_token| per_token * 1_000_000.0);
            (input, output)
        })
        .unwrap_or((None, None));
    let supports_tools = entry.get("supported_parameters").and_then(Value::as_array).map(
        |parameters| {
            parameters
                .iter()
                .any(|value| value.as_str() == Some("tools"))
        },
    );
    Some(DiscoveredModel {
        id,
        context_length,
        input_price_per_mtok,
        output_price_per_mtok,
        supports_tools,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_parse_models_response_openai_shape_yields_bare_ids() {
        let body = json!({"data": [{"id": "gpt-4o"}, {"id": "gpt-4o-mini"}]});
        let models = parse_models_response(&body);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "gpt-4o");
        assert_eq!(models[0].context_length, None);
        assert_eq!(models[0].input_price_per_mtok, None);
        assert_eq!(models[0].supports_tools, None);
    }

    #[test]
    fn test_parse_models_response_openrouter_pricing_converts_to_per_million() {
        let body = json!({"data": [{
            "id": "nvidia/nemotron-3.5-lightning",
            "context_length": 262_144,
            "pricing": {"prompt": "0.00000008", "completion": "0.0000002"},
            "supported_parameters": ["tools", "temperature"],
        }]});
        let models = parse_models_response(&body);
        assert_eq!(models.len(), 1);
        let model = &models[0];
        assert_eq!(model.id, "nvidia/nemotron-3.5-lightning");
        assert_eq!(model.context_length, Some(262_144));
        assert!((model.input_price_per_mtok.unwrap() - 0.08).abs() < 1e-9);
        assert!((model.output_price_per_mtok.unwrap() - 0.2).abs() < 1e-9);
        assert_eq!(model.supports_tools, Some(true));
    }

    #[test]
    fn test_parse_models_response_anthropic_shape_yields_bare_ids() {
        let body = json!({"data": [{"id": "claude-sonnet-4-6", "type": "model"}]});
        let models = parse_models_response(&body);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "claude-sonnet-4-6");
        assert_eq!(models[0].supports_tools, None);
    }

    #[test]
    fn test_parse_models_response_missing_data_yields_empty() {
        let body = json!({"object": "list"});
        assert!(parse_models_response(&body).is_empty());
    }

    #[test]
    fn test_parse_models_response_skips_entries_without_id() {
        let body = json!({"data": [{"name": "no id here"}, {"id": "valid"}]});
        let models = parse_models_response(&body);
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "valid");
    }

    #[test]
    fn test_parse_models_response_supported_parameters_without_tools_is_false() {
        let body = json!({"data": [{"id": "x", "supported_parameters": ["temperature"]}]});
        let models = parse_models_response(&body);
        assert_eq!(models[0].supports_tools, Some(false));
    }
}
