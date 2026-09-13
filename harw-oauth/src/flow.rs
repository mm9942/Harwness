//! Setup-Token-/OAuth-PKCE-Flow (Paste-Variante) gegen Anthropic.
//!
//! ## Ablauf
//! 1. [`crate::generate_pkce`] erzeugt Verifier + Challenge.
//! 2. [`authorize_url`] baut die URL; der Nutzer meldet sich im Browser an und
//!    fügt den zurückgegebenen `code` (ggf. `code#state`) ein.
//! 3. [`exchange_code`] tauscht `code` + `verifier` gegen den Setup-Token.
//!
//! ## Konfigurierbarkeit
//! Die Endpunkt-/Client-Konstanten sind die öffentlichen Claude-Code-Defaults,
//! **überschreibbar per Env-Variable** — so lässt sich der Flow ohne Rebuild an
//! einen geänderten Stand anpassen (siehe [`3.4` im Design-Spec]).
//!
//! | Env-Variable                 | Default |
//! |------------------------------|---------|
//! | `HARW_OAUTH_CLIENT_ID`       | `9d1c250a-e61b-44d9-88ed-5944d1962f5e` |
//! | `HARW_OAUTH_AUTHORIZE_URL`   | `https://claude.ai/oauth/authorize` |
//! | `HARW_OAUTH_TOKEN_URL`       | `https://console.anthropic.com/v1/oauth/token` |
//! | `HARW_OAUTH_REDIRECT_URI`    | `https://console.anthropic.com/oauth/code/callback` |
//! | `HARW_OAUTH_SCOPES`          | `org:create_api_key user:profile user:inference` |

use secrecy::SecretString;
use serde::Deserialize;

use crate::error::{OAuthError, OAuthResult};

/// Öffentliche Claude-Code-Client-ID (überschreibbar per Env).
const DEFAULT_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const DEFAULT_AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const DEFAULT_TOKEN_URL: &str = "https://console.anthropic.com/v1/oauth/token";
const DEFAULT_REDIRECT_URI: &str = "https://console.anthropic.com/oauth/code/callback";
const DEFAULT_SCOPES: &str = "org:create_api_key user:profile user:inference";

/// Liest eine Env-Variable oder liefert den Default (leere Werte zählen als
/// nicht gesetzt).
fn env_or(var: &str, default: &str) -> String {
    std::env::var(var)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_owned())
}

/// OAuth-Client-ID (Env `HARW_OAUTH_CLIENT_ID`).
#[must_use]
pub fn client_id() -> String {
    env_or("HARW_OAUTH_CLIENT_ID", DEFAULT_CLIENT_ID)
}

/// Redirect-URI der Paste-Variante (Env `HARW_OAUTH_REDIRECT_URI`).
#[must_use]
pub fn redirect_uri() -> String {
    env_or("HARW_OAUTH_REDIRECT_URI", DEFAULT_REDIRECT_URI)
}

/// Angeforderte Scopes (Env `HARW_OAUTH_SCOPES`).
#[must_use]
pub fn scopes() -> String {
    env_or("HARW_OAUTH_SCOPES", DEFAULT_SCOPES)
}

/// Prozent-kodiert einen Query-Wert (RFC 3986, unreserved bleiben erhalten).
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Baut die Authorize-URL für den PKCE-Paste-Flow.
///
/// # Arguments
/// - `challenge` (`&str`): `S256`-Code-Challenge aus [`crate::generate_pkce`].
/// - `state` (`&str`): CSRF-`state`, das der Callback zurückspiegeln muss.
///
/// # Returns
/// Die vollständige Authorize-URL zum Öffnen im Browser.
#[must_use]
pub fn authorize_url(challenge: &str, state: &str) -> String {
    let base = env_or("HARW_OAUTH_AUTHORIZE_URL", DEFAULT_AUTHORIZE_URL);
    format!(
        "{base}?code=true&response_type=code&client_id={client}&redirect_uri={redirect}\
         &scope={scope}&code_challenge={challenge}&code_challenge_method=S256&state={state}",
        client = encode(&client_id()),
        redirect = encode(&redirect_uri()),
        scope = encode(&scopes()),
        challenge = encode(challenge),
        state = encode(state),
    )
}

/// Token-Endpoint-Antwort (nur die für uns relevanten Felder).
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
}

/// Ersetzt providerseitige Fehlerdetails, die sensible Werte enthalten können.
fn redacted_token_exchange_body(_body: &str) -> String {
    "token exchange response body redacted".to_owned()
}

/// Trennt eine Callback-Eingabe der Form `code#state` in `(code, state)`.
///
/// Enthält die Eingabe kein `#`, wird `state` leer zurückgegeben.
#[must_use]
pub fn split_callback(input: &str) -> (String, String) {
    let trimmed = input.trim();
    match trimmed.split_once('#') {
        Some((code, state)) => (code.to_owned(), state.to_owned()),
        None => (trimmed.to_owned(), String::new()),
    }
}

/// Prüft den vom Nutzer eingefügten Callback-State gegen den erwarteten State.
///
/// State-Werte werden absichtlich nicht in den Fehlertext aufgenommen.
fn validate_callback_state(supplied_state: &str, expected_state: &str) -> OAuthResult<()> {
    if expected_state.is_empty() {
        return Err(OAuthError::MalformedInput(
            "missing expected callback state".to_owned(),
        ));
    }
    if supplied_state.is_empty() {
        return Err(OAuthError::MalformedInput(
            "missing callback state".to_owned(),
        ));
    }
    if supplied_state != expected_state {
        return Err(OAuthError::MalformedInput(
            "callback state does not match expected state".to_owned(),
        ));
    }
    Ok(())
}

/// Tauscht einen Authorization-Code gegen den Setup-Token (Bearer/OAuth).
///
/// # Arguments
/// - `client` (`&reqwest::Client`): geteilter HTTP-Client.
/// - `code` (`&str`): der vom Nutzer eingefügte Authorization-Code.
/// - `supplied_state` (`&str`): das aus `code#state` eingefügte `state`.
/// - `expected_state` (`&str`): das für diesen Flow erzeugte `state`.
/// - `verifier` (`&str`): der PKCE-`code_verifier`.
///
/// # Returns
/// Den `access_token` als [`SecretString`].
///
/// # Errors
/// - [`OAuthError::MalformedInput`][]: fehlender oder nicht passender State.
/// - [`OAuthError::TokenExchange`][]: nicht-erfolgreiche HTTP-Antwort; der
///   Antwort-Body wird aus Sicherheitsgründen nicht in den Fehler übernommen.
/// - [`OAuthError::Http`][]: Transportfehler.
/// - [`OAuthError::MissingField`][]: Antwort ohne `access_token`.
///
/// # Concurrency
/// `async`; treibt genau eine HTTP-Anfrage.
pub async fn exchange_code(
    client: &reqwest::Client,
    code: &str,
    supplied_state: &str,
    expected_state: &str,
    verifier: &str,
) -> OAuthResult<SecretString> {
    validate_callback_state(supplied_state, expected_state)?;

    let payload = serde_json::json!({
        "grant_type": "authorization_code",
        "code": code,
        "state": supplied_state,
        "client_id": client_id(),
        "redirect_uri": redirect_uri(),
        "code_verifier": verifier,
    });

    let response = client
        .post(env_or("HARW_OAUTH_TOKEN_URL", DEFAULT_TOKEN_URL))
        .header("content-type", "application/json")
        .json(&payload)
        .send()
        .await?;

    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        return Err(OAuthError::TokenExchange {
            status: status.as_u16(),
            body: redacted_token_exchange_body(&body),
        });
    }

    let parsed: TokenResponse = serde_json::from_str(&body)
        .map_err(|error| OAuthError::MalformedInput(format!("invalid token JSON: {error}")))?;
    let token = parsed
        .access_token
        .ok_or(OAuthError::MissingField("access_token"))?;
    Ok(SecretString::new(token.into_boxed_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_authorize_url_contains_pkce_and_state() {
        let url = authorize_url("CHAL123", "STATE456");
        assert!(url.contains("code_challenge=CHAL123"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=STATE456"));
        assert!(url.contains("response_type=code"));
    }

    #[test]
    fn test_split_callback_with_hash() {
        assert_eq!(
            split_callback("  abc123#xyz789  "),
            ("abc123".to_owned(), "xyz789".to_owned())
        );
    }

    #[test]
    fn test_split_callback_without_hash() {
        assert_eq!(
            split_callback("onlycode"),
            ("onlycode".to_owned(), String::new())
        );
    }

    #[test]
    fn test_callback_state_must_match_expected_state() {
        assert!(validate_callback_state("expected", "expected").is_ok());

        let error = validate_callback_state("pasted", "expected").unwrap_err();
        assert_eq!(
            error.to_string(),
            "malformed callback input: callback state does not match expected state"
        );
    }

    #[test]
    fn test_callback_state_is_required() {
        let missing_state = validate_callback_state("", "expected").unwrap_err();
        assert_eq!(
            missing_state.to_string(),
            "malformed callback input: missing callback state"
        );

        let missing_expected = validate_callback_state("pasted", "").unwrap_err();
        assert_eq!(
            missing_expected.to_string(),
            "malformed callback input: missing expected callback state"
        );
    }

    #[test]
    fn test_callback_state_errors_do_not_include_state_values() {
        let error = validate_callback_state("secret-pasted-state", "secret-expected-state")
            .unwrap_err()
            .to_string();
        assert!(!error.contains("secret-pasted-state"));
        assert!(!error.contains("secret-expected-state"));
    }

    #[test]
    fn test_token_exchange_body_is_redacted() {
        let secret_body = r#"{"error":"invalid_grant","access_token":"provider-secret"}"#;
        let error = OAuthError::TokenExchange {
            status: 400,
            body: redacted_token_exchange_body(secret_body),
        };

        assert_eq!(
            redacted_token_exchange_body(secret_body),
            "token exchange response body redacted"
        );
        assert!(matches!(
            &error,
            OAuthError::TokenExchange { status: 400, .. }
        ));

        let display = error.to_string();
        assert!(display.contains("HTTP 400"));
        assert!(!display.contains(secret_body));
        assert!(!format!("{error:?}").contains(secret_body));
    }

    #[test]
    fn test_encode_preserves_unreserved() {
        assert_eq!(encode("aA0-_.~"), "aA0-_.~");
        assert_eq!(encode("a b"), "a%20b");
        assert_eq!(encode("x:y/z"), "x%3Ay%2Fz");
    }
}
