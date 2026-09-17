//! Fehlertyp der HTTP-Provider-Brücke.
//!
//! Handgeschriebener Error-Enum (kein `anyhow`/`thiserror`) für
//! `harw-provider-http`. Deckt fehlende Defaults, unauflösbare Credentials,
//! HTTP-Transportfehler, Decode-Fehler und API-Fehlerantworten ab. Siehe
//! Brief "harw-provider-http" §3.
//!
//! ## W4a / A-OAI: Klassifikation in den `ModelError`-Vertrag
//! [`model_error_for_status`] und [`model_error_for_transport`] übersetzen
//! HTTP-Status bzw. `reqwest`-Transportfehler in die W3-Varianten von
//! [`harw_core::ModelError`] (`Transient`, `Auth`, `QuotaExceeded`,
//! `ContextLength`, `Timeout`, `Truncated`). Nur `Transient`/`Timeout` sind
//! laut [`harw_core::ModelError::is_retryable`] wiederholbar; Kontingent-,
//! Auth- und Kontextlängenfehler werden deshalb bewusst **nicht** als
//! `Transient` gemeldet. Alle Meldungen laufen über
//! [`HttpProviderError::remote_response`] und enthalten nie den Roh-Body.

use harw_core::ModelError;
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
    /// Der Warte-Timer zwischen zwei Wiederholungsversuchen konnte nicht
    /// gestartet werden (W4a / A-OAI, [`crate::retry::ThreadSleeper`]).
    TimerUnavailable {
        /// Statische, inhaltsfreie Begründung.
        reason: String,
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
            Self::TimerUnavailable { reason } => {
                write!(f, "retry backoff timer unavailable: {reason}")
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

/// Fehlercodes im OpenAI-Fehlerobjekt (`error.code`/`error.type`), die ein
/// erschöpftes Kontingent statt eines kurzfristigen Rate-Limits melden.
const QUOTA_ERROR_CODES: &[&str] = &[
    "insufficient_quota",
    "billing_hard_limit_reached",
    "billing_not_active",
    "quota_exceeded",
];

/// Fehlercodes, die eine zu lange Eingabe melden.
const CONTEXT_LENGTH_ERROR_CODES: &[&str] = &["context_length_exceeded", "string_above_max_length"];

/// Obergrenze für einen aus `Retry-After` gelesenen Wert, bevor er in
/// Sekunden gespeichert wird (verhindert Überläufe bei absurden Headern).
const MAX_RETRY_AFTER_HINT_SECS: u64 = 24 * 60 * 60;

/// Liest `error.code` bzw. `error.type` aus einem JSON-Fehler-Body.
fn provider_error_code(body: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    ["/error/code", "/error/type", "/code", "/type"]
        .iter()
        .find_map(|pointer| value.pointer(pointer).and_then(serde_json::Value::as_str))
        .map(str::to_ascii_lowercase)
}

/// Ermittelt eine vom Provider empfohlene Wartezeit in Sekunden.
///
/// # Description
/// Reihenfolge: `retry-after-ms` (Millisekunden, aufgerundet), `Retry-After`
/// als ganze Sekunden, danach der Body-Hinweis „wait N seconds“
/// (ASCII-case-insensitiv). HTTP-Datumsangaben in `Retry-After` werden
/// ignoriert. Ohne Hinweis `None` — **kein** erfundener Default, damit
/// [`crate::retry::RetryingProvider`] dann sein eigenes Backoff wählt.
///
/// # Arguments
/// - `retry_after` (`Option<&str>`): Wert des `Retry-After`-Headers.
/// - `retry_after_ms` (`Option<&str>`): Wert des `retry-after-ms`-Headers.
/// - `body` (`&str`): Antwort-Body (nur gelesen, nie zurückgegeben).
///
/// # Returns
/// Wartezeit in Sekunden, gedeckelt auf 24 h, oder `None`.
#[must_use]
pub(crate) fn retry_after_hint(
    retry_after: Option<&str>,
    retry_after_ms: Option<&str>,
    body: &str,
) -> Option<u64> {
    if let Some(millis) = retry_after_ms.and_then(|value| value.trim().parse::<u64>().ok()) {
        return Some(millis.div_ceil(1000).min(MAX_RETRY_AFTER_HINT_SECS));
    }
    if let Some(secs) = retry_after.and_then(|value| value.trim().parse::<u64>().ok()) {
        return Some(secs.min(MAX_RETRY_AFTER_HINT_SECS));
    }
    let lower = body.to_ascii_lowercase();
    let position = lower.find("wait ")?;
    let digits: String = lower[position + "wait ".len()..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits
        .parse::<u64>()
        .ok()
        .filter(|secs| *secs > 0)
        .map(|secs| secs.min(MAX_RETRY_AFTER_HINT_SECS))
}

/// Übersetzt eine nicht-erfolgreiche HTTP-Antwort in den `ModelError`-Vertrag.
///
/// # Description
/// | Status | Bedingung | Variante |
/// |---|---|---|
/// | 429 | `error.code/type` ∈ Kontingent-Codes | `QuotaExceeded` |
/// | 429 | sonst | `Transient{status:429, retry_after_secs}` |
/// | 401, 403 | — | `Auth` |
/// | 400, 413 | Kontextlängen-Code oder Meldung „context length“ | `ContextLength` |
/// | 408, 500–599 (inkl. 529) | — | `Transient{status, retry_after_secs}` |
/// | sonst | — | `RequestFailed` |
///
/// Die Meldung ist stets die bereinigte Diagnose aus
/// [`HttpProviderError::remote_response`] (nur strukturierte, begrenzte
/// Felder, kein Roh-Body).
///
/// # Arguments
/// - `status` (`u16`): HTTP-Status.
/// - `request_id` (`Option<&str>`): begrenzte Gateway-Request-ID.
/// - `retry_after_secs` (`Option<u64>`): siehe [`retry_after_hint`].
/// - `body` (`&str`): unvertrauenswürdiger Antwort-Body.
///
/// # Returns
/// Die passende [`ModelError`]-Variante.
#[must_use]
pub(crate) fn model_error_for_status(
    status: u16,
    request_id: Option<&str>,
    retry_after_secs: Option<u64>,
    body: &str,
) -> ModelError {
    let message =
        HttpProviderError::remote_response(status, request_id.map(str::to_owned), body).to_string();
    let code = provider_error_code(body);
    let code_in = |codes: &[&str]| code.as_deref().is_some_and(|code| codes.contains(&code));
    match status {
        429 if code_in(QUOTA_ERROR_CODES) => ModelError::QuotaExceeded { message },
        429 => ModelError::Transient {
            status: Some(status),
            retry_after_secs,
            message,
        },
        401 | 403 => ModelError::Auth { message },
        400 | 413
            if code_in(CONTEXT_LENGTH_ERROR_CODES)
                || message.to_ascii_lowercase().contains("context length") =>
        {
            ModelError::ContextLength { message }
        }
        408 | 500..=599 => ModelError::Transient {
            status: Some(status),
            retry_after_secs,
            message,
        },
        _ => ModelError::RequestFailed(message),
    }
}

/// Übersetzt einen `reqwest`-Transportfehler in den `ModelError`-Vertrag.
///
/// # Description
/// - Redirect-Ablehnung (W1-06b-Policy) → `RequestFailed` (nie retryable:
///   eine Wiederholung würde dieselbe Cross-Origin-Weiterleitung treffen).
/// - Zeitüberschreitung → `Timeout`.
/// - Verbindungsaufbau gescheitert → `Transient{status: None}`.
/// - Abbruch beim Lesen des Bodys (`body_phase = true`) → `Truncated`.
/// - sonst → `RequestFailed`.
///
/// # Arguments
/// - `error` (`reqwest::Error`): Transportfehler (übernommen).
/// - `body_phase` (`bool`): `true`, wenn der Fehler beim Lesen des Bodys auftrat.
///
/// # Returns
/// Die passende [`ModelError`]-Variante; die Meldung nennt keine Credentials
/// (reqwest rendert keine Header).
#[must_use]
pub(crate) fn model_error_for_transport(error: reqwest::Error, body_phase: bool) -> ModelError {
    let is_redirect = error.is_redirect();
    let is_timeout = error.is_timeout();
    let is_connect = error.is_connect();
    let message = HttpProviderError::from(error).to_string();
    if is_redirect {
        ModelError::RequestFailed(message)
    } else if is_timeout {
        ModelError::Timeout { message }
    } else if is_connect {
        ModelError::Transient {
            status: None,
            retry_after_secs: None,
            message,
        }
    } else if body_phase {
        ModelError::Truncated { message }
    } else {
        ModelError::RequestFailed(message)
    }
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
    use super::{HttpProviderError, model_error_for_status, retry_after_hint};
    use harw_core::ModelError;

    #[test]
    fn test_model_error_for_status_classification_table() {
        let quota = r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota","code":"insufficient_quota"}}"#;
        let rate = r#"{"error":{"message":"Rate limit reached","type":"requests","code":"rate_limit_exceeded"}}"#;
        let context = r#"{"error":{"message":"too long","code":"context_length_exceeded"}}"#;

        assert!(matches!(
            model_error_for_status(429, None, Some(7), rate),
            ModelError::Transient {
                status: Some(429),
                retry_after_secs: Some(7),
                ..
            }
        ));
        assert!(matches!(
            model_error_for_status(429, None, Some(7), quota),
            ModelError::QuotaExceeded { .. }
        ));
        assert!(matches!(
            model_error_for_status(401, None, None, "{}"),
            ModelError::Auth { .. }
        ));
        assert!(matches!(
            model_error_for_status(403, None, None, "{}"),
            ModelError::Auth { .. }
        ));
        assert!(matches!(
            model_error_for_status(400, None, None, context),
            ModelError::ContextLength { .. }
        ));
        assert!(matches!(
            model_error_for_status(400, None, None, rate),
            ModelError::RequestFailed(_)
        ));
        for status in [408_u16, 500, 502, 503, 529] {
            assert!(
                matches!(
                    model_error_for_status(status, None, None, "{}"),
                    ModelError::Transient { status: Some(got), .. } if got == status
                ),
                "status {status} must be transient"
            );
        }
        assert!(matches!(
            model_error_for_status(404, None, None, "{}"),
            ModelError::RequestFailed(_)
        ));
    }

    #[test]
    fn test_model_error_for_status_retryability_matches_contract() {
        let quota = r#"{"error":{"code":"insufficient_quota"}}"#;
        assert!(model_error_for_status(429, None, None, "{}").is_retryable());
        assert!(model_error_for_status(529, None, None, "{}").is_retryable());
        assert!(!model_error_for_status(429, None, None, quota).is_retryable());
        assert!(!model_error_for_status(400, None, None, "{}").is_retryable());
        assert!(!model_error_for_status(401, None, None, "{}").is_retryable());
    }

    #[test]
    fn test_model_error_for_status_does_not_echo_raw_body() {
        let error = model_error_for_status(503, None, None, "upstream secret-token raw");
        assert!(!error.to_string().contains("secret-token"));
    }

    #[test]
    fn test_retry_after_hint_sources_and_order() {
        assert_eq!(retry_after_hint(Some("12"), None, ""), Some(12));
        assert_eq!(retry_after_hint(Some("12"), Some("1500"), ""), Some(2));
        assert_eq!(
            retry_after_hint(None, None, "Please wait 9 seconds."),
            Some(9)
        );
        assert_eq!(
            retry_after_hint(Some("Tue, 16 Jul 2026 12:00:00 GMT"), None, "no hint"),
            None
        );
        assert_eq!(retry_after_hint(None, None, ""), None);
        assert_eq!(
            retry_after_hint(Some("99999999"), None, ""),
            Some(24 * 60 * 60)
        );
    }

    #[test]
    fn test_timer_unavailable_display_names_reason() {
        let error = HttpProviderError::TimerUnavailable {
            reason: "thread spawn failed".to_owned(),
        };
        assert_eq!(
            error.to_string(),
            "retry backoff timer unavailable: thread spawn failed"
        );
    }

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
