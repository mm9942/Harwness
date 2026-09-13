//! Fehlertyp der HTTP-Provider-Brücke.
//!
//! Handgeschriebener Error-Enum (kein `anyhow`/`thiserror`) für
//! `harw-provider-http`. Deckt fehlende Defaults, unauflösbare Credentials,
//! HTTP-Transportfehler, Decode-Fehler und API-Fehlerantworten ab. Siehe
//! Brief "harw-provider-http" §3.

use std::fmt;

/// Bequemer Ergebnistyp der HTTP-Provider-Brücke.
pub type HttpProviderResult<T> = Result<T, HttpProviderError>;

/// Fehler der HTTP-Provider-Brücke zwischen `harw-provider` und
/// `harw_core::ModelProvider`.
///
/// # Description
/// Jede Variante trägt genug Kontext, um die Ursache ohne Blick in den
/// Quellcode zu verstehen. `Http` wickelt Transportfehler von `reqwest`,
/// `Api` transportiert eine nicht-erfolgreiche HTTP-Antwort mit Status und
/// Body. Der Body wird niemals über [`Display`] oder [`Debug`] ausgegeben.
pub enum HttpProviderError {
    /// Die Provider-Menge für den Router ist leer.
    EmptyProviderSet,
    /// Der konfigurierte Default-Provider ist in der Provider-Menge nicht
    /// vorhanden.
    DefaultProviderNotFound {
        /// Konfigurierte Provider-ID.
        name: String,
    },
    /// Ein für die Konstruktion nötiger Default fehlt in der Konfiguration.
    MissingDefault {
        /// Was fehlt, z. B. `"default_provider"` oder `"default_model"`.
        what: String,
    },
    /// Eine `SecretRef` konnte nicht zu einem Klartext-Wert aufgelöst werden.
    UnresolvedCredential {
        /// Kanonische Referenz-Form (z. B. `"env:OPENAI_API_KEY"`).
        reference: String,
        /// Grund des Fehlschlags (Variablen-/Datei-Fehler oder "unsupported").
        reason: String,
    },
    /// Eine Credential-Referenz verwendet einen Resolver, den dieser Provider
    /// nicht implementiert (`keyring:` oder `secrets:`).
    ///
    /// Die Referenz wird zur Diagnose mitgeführt, aber nicht ausgegeben: Sie
    /// kann neben dem Namen des Resolvers auch sensible Metadaten enthalten.
    UnsupportedCredentialReference { reference: String },
    /// Eine für den Provider nötige Umgebungsvariable ist nicht gesetzt.
    MissingEnv {
        /// Name der fehlenden Variable, z. B. `"ANTHROPIC_FOUNDRY_API_KEY"`.
        var: String,
    },
    /// Transportfehler aus `reqwest`.
    Http(reqwest::Error),
    /// Antwort konnte nicht in das erwartete Schema dekodiert werden.
    Decode(String),
    /// Nicht-erfolgreiche API-Antwort.
    Api {
        /// HTTP-Statuscode.
        status: u16,
        /// Roher Antwort-Body (für Diagnose).
        body: String,
    },
    /// Eine nicht-erfolgreiche Remote-Antwort ohne Roh-Body.
    ///
    /// `message` darf nur eine bereits bereinigte, begrenzte Diagnose sein;
    /// [`Self::remote_response`] bietet dafür den sicheren Konstruktionspfad.
    RemoteResponse {
        /// HTTP-Statuscode.
        status: u16,
        /// Begrenzte Request-ID des Gateways, sofern vorhanden.
        request_id: Option<String>,
        /// Bereinigte Provider-Diagnose, sofern vorhanden.
        message: Option<String>,
    },
    /// Der Provider hat HTTP 429 zurückgegeben (Rate-Limit überschritten).
    ///
    /// `retry_after` ist die empfohlene Wartezeit (aus `Retry-After`-Header
    /// oder Body-Regex `(?i)wait\s+(\d+)\s+seconds` extrahiert, Fallback 30 s).
    RateLimited {
        /// Empfohlene Wartezeit.
        retry_after: std::time::Duration,
        /// Rohtext der Fehlermeldung.
        message: String,
    },
}

impl fmt::Display for HttpProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyProviderSet => {
                write!(f, "cannot construct provider router: provider set is empty")
            }
            Self::DefaultProviderNotFound { name } => write!(
                f,
                "cannot construct provider router: configured default provider not found: {name}"
            ),
            Self::MissingDefault { what } => {
                write!(f, "missing required configuration default: {what}")
            }
            Self::UnresolvedCredential { reference, reason } => {
                write!(f, "could not resolve credential '{reference}': {reason}")
            }
            Self::UnsupportedCredentialReference { .. } => {
                write!(f, "unsupported credential reference (keyring:/secrets:)")
            }
            Self::MissingEnv { var } => {
                write!(f, "required environment variable not set: {var}")
            }
            Self::Http(error) => write!(f, "HTTP transport failed: {error}"),
            Self::Decode(reason) => write!(f, "failed to decode provider response: {reason}"),
            Self::Api { status, body } => {
                let _ = body;
                write!(f, "provider returned HTTP {status}")
            }
            Self::RemoteResponse {
                status,
                request_id,
                message,
            } => {
                write!(f, "provider returned HTTP {status}")?;
                if let Some(request_id) = request_id.as_deref().filter(|id| !id.is_empty()) {
                    if let Some(request_id) = sanitize_diagnostic(request_id) {
                        write!(f, " (request_id: {request_id})")?;
                    }
                }
                if let Some(message) = message.as_deref().and_then(sanitize_diagnostic) {
                    write!(f, ": {message}")?;
                }
                Ok(())
            }
            Self::RateLimited {
                retry_after,
                message,
            } => {
                write!(
                    f,
                    "rate limited by provider — retry after {}s: {message}",
                    retry_after.as_secs()
                )
            }
        }
    }
}

impl HttpProviderError {
    /// Creates a remote-response error from an untrusted response body.
    ///
    /// Only documented JSON message fields are retained; arbitrary response
    /// bodies, echoed prompts, and HTML are discarded.
    pub fn remote_response(status: u16, request_id: Option<String>, body: &str) -> Self {
        let message = serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|value| {
                value
                    .pointer("/error/message")
                    .or_else(|| value.get("message"))
                    .or_else(|| value.pointer("/error/detail"))
                    .and_then(serde_json::Value::as_str)
                    .and_then(sanitize_diagnostic)
            });
        Self::RemoteResponse {
            status,
            request_id: request_id.and_then(|id| sanitize_diagnostic(&id)),
            message,
        }
    }
}

fn sanitize_diagnostic(value: &str) -> Option<String> {
    const MAX_DIAGNOSTIC_CHARS: usize = 512;
    let mut sanitized = String::with_capacity(value.len().min(MAX_DIAGNOSTIC_CHARS));
    let mut previous_was_whitespace = false;
    for character in value.chars().take(MAX_DIAGNOSTIC_CHARS) {
        if character.is_control() || character.is_whitespace() {
            if !previous_was_whitespace {
                sanitized.push(' ');
                previous_was_whitespace = true;
            }
        } else {
            sanitized.push(character);
            previous_was_whitespace = false;
        }
    }
    let sanitized = sanitized.trim();
    (!sanitized.is_empty()).then(|| sanitized.to_owned())
}

impl fmt::Debug for HttpProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

impl std::error::Error for HttpProviderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Http(error) => Some(error),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for HttpProviderError {
    fn from(error: reqwest::Error) -> Self {
        Self::Http(error)
    }
}

#[cfg(test)]
mod tests {
    use super::HttpProviderError;

    #[test]
    fn unsupported_credential_reference_does_not_disclose_reference() {
        let error = HttpProviderError::UnsupportedCredentialReference {
            reference: "keyring:service/api-key-with-secret-material".to_owned(),
        };
        let rendered = error.to_string();
        assert!(rendered.contains("unsupported credential reference"));
        assert!(!rendered.contains("service/api-key-with-secret-material"));
    }

    #[test]
    fn remote_response_discards_unstructured_body() {
        let error = HttpProviderError::remote_response(
            502,
            Some("gateway-request-7".to_owned()),
            "upstream token=super-secret and raw HTML",
        );
        let rendered = error.to_string();
        assert!(rendered.contains("HTTP 502"));
        assert!(rendered.contains("gateway-request-7"));
        assert!(!rendered.contains("super-secret"));
        assert!(!rendered.contains("raw HTML"));
    }

    #[test]
    fn remote_response_keeps_only_bounded_structured_message() {
        let error = HttpProviderError::remote_response(
            429,
            None,
            r#"{"error":{"message":"  retry\n shortly  "},"body":"secret"}"#,
        );
        assert_eq!(
            error.to_string(),
            "provider returned HTTP 429: retry shortly"
        );
    }

    #[test]
    fn legacy_api_error_also_does_not_disclose_body() {
        let error = HttpProviderError::Api {
            status: 500,
            body: "response-secret".to_owned(),
        };
        assert_eq!(error.to_string(), "provider returned HTTP 500");
    }

    #[test]
    fn empty_provider_set_has_typed_router_error() {
        assert_eq!(
            HttpProviderError::EmptyProviderSet.to_string(),
            "cannot construct provider router: provider set is empty"
        );
    }

    #[test]
    fn missing_configured_default_provider_has_typed_router_error() {
        let error = HttpProviderError::DefaultProviderNotFound {
            name: "missing-provider".to_owned(),
        };
        assert_eq!(
            error.to_string(),
            "cannot construct provider router: configured default provider not found: missing-provider"
        );
    }
}
