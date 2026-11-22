//! Konto-Login per OpenID Connect (Device-Code-Flow, RFC 8628) fuer den
//! harw-Server.
//!
//! Diese Schicht ist reine Bibliothek: sie startet den Device-Flow bei einem
//! konfigurierten Aussteller, fragt den Token-Endpoint ab, prueft das
//! ID-Token (RS256 gegen das JWKS des Ausstellers, `iss`, `aud`, `exp`) und
//! liefert nur die Identitaet (`iss`, `sub`, `email`) zurueck. Access-/Refresh-
//! Tokens werden gar nicht erst gelesen; das ID-Token wird nur zum Pruefen
//! benutzt und nie geloggt oder zurueckgegeben.
//!
//! Die Zuordnung zum Owner/Tenant ist Sache des Aufrufers ([`OwnerPolicy`]):
//! nur ausdruecklich freigegebene Konten werden zugelassen (Default: deny).

use std::time::Duration;

use aws_lc_rs::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize;
use serde_json::Value;

use crate::error::{OAuthError, OAuthResult};

const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_DOCUMENT_BYTES: usize = 256 * 1024;
const MIN_INTERVAL_SECS: u64 = 5;
const MAX_FLOW_SECS: u64 = 15 * 60;

/// Konfigurierter OIDC-Aussteller mit den entdeckten Endpunkten.
#[derive(Clone, Debug)]
pub struct OidcProvider {
    /// Aussteller-URL (muss exakt dem `iss` des ID-Tokens entsprechen).
    pub issuer: String,
    /// Oeffentliche Client-ID (zugleich erwartetes `aud`).
    pub client_id: String,
    /// Angeforderte Scopes, z. B. `openid email`.
    pub scopes: String,
    device_authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
}

#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    device_authorization_endpoint: Option<String>,
    token_endpoint: String,
    jwks_uri: String,
}

/// HTTP-Client fuer den Login: keine Redirects, kurzes Timeout.
///
/// # Errors
/// [`OAuthError::Http`], wenn der Client nicht gebaut werden kann.
pub fn login_client() -> OAuthResult<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(OAuthError::from)
}

/// Entdeckt die Endpunkte eines Ausstellers (`/.well-known/openid-configuration`).
///
/// # Errors
/// [`OAuthError::MalformedInput`] bei Nicht-HTTPS, abweichendem `issuer` oder
/// fehlendem Device-Endpoint; [`OAuthError::Http`] bei Transportfehlern.
pub async fn discover(
    client: &reqwest::Client,
    issuer: &str,
    client_id: &str,
    scopes: &str,
) -> OAuthResult<OidcProvider> {
    let issuer = issuer.trim_end_matches('/');
    if !issuer.starts_with("https://") || client_id.trim().is_empty() {
        return Err(OAuthError::MalformedInput(
            "issuer must be https and client_id set".to_owned(),
        ));
    }
    let doc: Discovery = get_json(
        client,
        &format!("{issuer}/.well-known/openid-configuration"),
    )
    .await?;
    if doc.issuer.trim_end_matches('/') != issuer {
        return Err(OAuthError::MalformedInput(
            "issuer mismatch in discovery document".to_owned(),
        ));
    }
    let device = doc
        .device_authorization_endpoint
        .ok_or(OAuthError::MissingField("device_authorization_endpoint"))?;
    for url in [&device, &doc.token_endpoint, &doc.jwks_uri] {
        if !url.starts_with("https://") {
            return Err(OAuthError::MalformedInput(
                "discovery endpoints must be https".to_owned(),
            ));
        }
    }
    Ok(OidcProvider {
        issuer: issuer.to_owned(),
        client_id: client_id.to_owned(),
        scopes: scopes.to_owned(),
        device_authorization_endpoint: device,
        token_endpoint: doc.token_endpoint,
        jwks_uri: doc.jwks_uri,
    })
}

/// Laufender Device-Flow. Der `device_code` bleibt auf dem Server.
pub struct DeviceAuthorization {
    device_code: SecretString,
    /// Code, den der Nutzer im Browser eingibt.
    pub user_code: String,
    /// Seite, die der Nutzer oeffnet.
    pub verification_uri: String,
    /// Seite mit vorausgefuelltem Code, falls der Aussteller sie liefert.
    pub verification_uri_complete: Option<String>,
    /// Frist des Flows in Sekunden (hoechstens 15 Minuten).
    pub expires_in: u64,
    /// Mindestabstand zwischen Abfragen in Sekunden.
    pub interval: u64,
}

impl std::fmt::Debug for DeviceAuthorization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeviceAuthorization([redacted])")
    }
}

#[derive(Deserialize)]
struct DeviceResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    verification_uri_complete: Option<String>,
    expires_in: u64,
    interval: Option<u64>,
}

/// Startet den Device-Flow.
///
/// # Errors
/// [`OAuthError::TokenExchange`] bei Fehlerstatus, sonst Transport-/Formatfehler.
pub async fn start_device_flow(
    client: &reqwest::Client,
    provider: &OidcProvider,
) -> OAuthResult<DeviceAuthorization> {
    let response = client
        .post(&provider.device_authorization_endpoint)
        .form(&[
            ("client_id", provider.client_id.as_str()),
            ("scope", provider.scopes.as_str()),
        ])
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        return Err(redacted(status.as_u16()));
    }
    let body: DeviceResponse = read_json(response).await?;
    if body.device_code.is_empty() || body.user_code.is_empty() {
        return Err(OAuthError::MissingField("device_code"));
    }
    let https = |url: &str| url.starts_with("https://");
    if !https(&body.verification_uri)
        || body
            .verification_uri_complete
            .as_deref()
            .is_some_and(|url| !https(url))
    {
        return Err(OAuthError::MalformedInput(
            "verification uri must be https".to_owned(),
        ));
    }
    Ok(DeviceAuthorization {
        device_code: SecretString::from(body.device_code),
        user_code: body.user_code,
        verification_uri: body.verification_uri,
        verification_uri_complete: body.verification_uri_complete,
        expires_in: body.expires_in.min(MAX_FLOW_SECS),
        interval: body.interval.unwrap_or(MIN_INTERVAL_SECS).max(MIN_INTERVAL_SECS),
    })
}

/// Verifizierte Konto-Identitaet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OidcIdentity {
    /// Aussteller.
    pub issuer: String,
    /// Stabile Konto-ID des Ausstellers.
    pub subject: String,
    /// E-Mail, nur wenn der Aussteller sie als verifiziert meldet.
    pub verified_email: Option<String>,
}

/// Ergebnis einer Abfrage des Device-Flows.
#[derive(Debug, PartialEq, Eq)]
pub enum DevicePoll {
    /// Nutzer hat noch nicht bestaetigt.
    Pending,
    /// Zu schnell gefragt; Intervall verlaengern.
    SlowDown,
    /// Nutzer hat abgelehnt.
    Denied,
    /// Frist abgelaufen.
    Expired,
    /// Angemeldet; ID-Token geprueft.
    Done(OidcIdentity),
}

#[derive(Deserialize)]
struct TokenBody {
    id_token: Option<String>,
    error: Option<String>,
}

/// Fragt den Token-Endpoint einmal ab und prueft ein ggf. erhaltenes ID-Token.
///
/// # Errors
/// Transport- und Formatfehler sowie ein ungueltiges ID-Token
/// ([`OAuthError::MalformedInput`]); Fehlerdetails enthalten nie Token.
pub async fn poll_device_flow(
    client: &reqwest::Client,
    provider: &OidcProvider,
    flow: &DeviceAuthorization,
) -> OAuthResult<DevicePoll> {
    let response = client
        .post(&provider.token_endpoint)
        .form(&[
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("device_code", flow.device_code.expose_secret()),
            ("client_id", provider.client_id.as_str()),
        ])
        .send()
        .await?;
    let status = response.status();
    let body: TokenBody = read_json_any(response).await?;
    if !status.is_success() {
        return match body.error.as_deref() {
            Some("authorization_pending") => Ok(DevicePoll::Pending),
            Some("slow_down") => Ok(DevicePoll::SlowDown),
            Some("access_denied") => Ok(DevicePoll::Denied),
            Some("expired_token") => Ok(DevicePoll::Expired),
            _ => Err(redacted(status.as_u16())),
        };
    }
    let id_token = SecretString::from(body.id_token.ok_or(OAuthError::MissingField("id_token"))?);
    let jwks: Jwks = get_json(client, &provider.jwks_uri).await?;
    let identity = verify_id_token(
        &jwks,
        id_token.expose_secret(),
        &provider.issuer,
        &provider.client_id,
        unix_now(),
    )?;
    Ok(DevicePoll::Done(identity))
}

/// Freigabeliste der Konten, die sich am Gateway anmelden duerfen.
///
/// Ein Konto passt, wenn `iss` und `sub` ODER die verifizierte E-Mail zu einem
/// Eintrag passen. Eine leere Liste laesst niemanden zu.
#[derive(Clone, Debug, Default)]
pub struct OwnerPolicy {
    /// Eintraege `iss|sub` oder `email:<adresse>` (Gross/Klein egal bei E-Mail).
    pub allowed: Vec<String>,
}

impl OwnerPolicy {
    /// Ob `identity` freigegeben ist.
    #[must_use]
    pub fn allows(&self, identity: &OidcIdentity) -> bool {
        let by_subject = format!("{}|{}", identity.issuer, identity.subject);
        self.allowed.iter().any(|entry| {
            let entry = entry.trim();
            entry == by_subject
                || identity.verified_email.as_deref().is_some_and(|email| {
                    entry
                        .strip_prefix("email:")
                        .is_some_and(|wanted| wanted.eq_ignore_ascii_case(email))
                })
        })
    }
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kid: Option<String>,
    kty: String,
    alg: Option<String>,
    #[serde(rename = "use")]
    usage: Option<String>,
    n: Option<String>,
    e: Option<String>,
}

#[derive(Deserialize)]
struct JwtHeader {
    alg: String,
    kid: Option<String>,
}

#[derive(Deserialize)]
struct Claims {
    iss: String,
    aud: Value,
    exp: u64,
    nbf: Option<u64>,
    sub: String,
    azp: Option<String>,
    email: Option<String>,
    email_verified: Option<bool>,
}

fn verify_id_token(
    keys: &Jwks,
    token: &str,
    issuer: &str,
    audience: &str,
    now: u64,
) -> OAuthResult<OidcIdentity> {
    let bad = || OAuthError::MalformedInput("invalid id token".to_owned());
    let (signed, signature) = token.rsplit_once('.').ok_or_else(bad)?;
    let (header, payload) = signed.split_once('.').ok_or_else(bad)?;
    let header: JwtHeader = serde_json::from_slice(&decode(header)?).map_err(|_| bad())?;
    if header.alg != "RS256" {
        return Err(bad());
    }
    let key = keys
        .keys
        .iter()
        .find(|key| {
            key.kty == "RSA"
                && key.kid == header.kid
                && key.alg.as_deref().is_none_or(|alg| alg == "RS256")
                && key.usage.as_deref().is_none_or(|usage| usage == "sig")
        })
        .ok_or_else(bad)?;
    let components = RsaPublicKeyComponents {
        n: decode(key.n.as_deref().ok_or_else(bad)?)?,
        e: decode(key.e.as_deref().ok_or_else(bad)?)?,
    };
    components
        .verify(
            &RSA_PKCS1_2048_8192_SHA256,
            signed.as_bytes(),
            &decode(signature)?,
        )
        .map_err(|_| bad())?;
    let claims: Claims = serde_json::from_slice(&decode(payload)?).map_err(|_| bad())?;
    let audience_ok = match &claims.aud {
        Value::String(value) => value == audience,
        Value::Array(values) => {
            values.iter().any(|value| value.as_str() == Some(audience))
                && (values.len() == 1 || claims.azp.as_deref() == Some(audience))
        }
        _ => false,
    };
    if !audience_ok
        || claims.iss.trim_end_matches('/') != issuer.trim_end_matches('/')
        || claims.exp <= now
        || claims.sub.is_empty()
        || claims.nbf.is_some_and(|nbf| nbf > now.saturating_add(30))
    {
        return Err(bad());
    }
    let verified_email = match (claims.email, claims.email_verified) {
        (Some(email), Some(true)) => Some(email),
        _ => None,
    };
    Ok(OidcIdentity {
        issuer: issuer.trim_end_matches('/').to_owned(),
        subject: claims.sub,
        verified_email,
    })
}

fn decode(segment: &str) -> OAuthResult<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(segment)
        .map_err(|_| OAuthError::MalformedInput("invalid id token".to_owned()))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn redacted(status: u16) -> OAuthError {
    OAuthError::TokenExchange {
        status,
        body: "oidc response body redacted".to_owned(),
    }
}

async fn get_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
) -> OAuthResult<T> {
    let response = client.get(url).send().await?;
    if !response.status().is_success() {
        return Err(redacted(response.status().as_u16()));
    }
    read_json(response).await
}

async fn read_json<T: serde::de::DeserializeOwned>(response: reqwest::Response) -> OAuthResult<T> {
    if !response.status().is_success() {
        return Err(redacted(response.status().as_u16()));
    }
    read_json_any(response).await
}

/// Liest einen begrenzten JSON-Body unabhaengig vom Status.
async fn read_json_any<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
) -> OAuthResult<T> {
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if data.len().saturating_add(chunk.len()) > MAX_DOCUMENT_BYTES {
            return Err(OAuthError::MalformedInput("response too large".to_owned()));
        }
        data.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&data)
        .map_err(|_| OAuthError::MalformedInput("invalid response document".to_owned()))
}
