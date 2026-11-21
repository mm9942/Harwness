//! OpenAI Sign in with ChatGPT credentials for a self-hosted Harw instance.

use std::fs::File;
use std::io::{self, Read as _};
use std::os::fd::AsFd as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aws_lc_rs::signature::{RSA_PKCS1_2048_8192_SHA256, RsaPublicKeyComponents};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use harw_fsutil::{
    OpenMode, ensure_owned_directory, ensure_private_regular, mkdir_at, open_beneath, open_dir_at,
    open_dir_nofollow, unlink_at,
};
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::codex_refresh::{RefreshLockGuard, acquire_token_file_lock};
use crate::store::write_private_file_atomically;

const ISSUER: &str = "https://auth.openai.com";
const JWKS_URL: &str = "https://auth.openai.com/.well-known/jwks.json";
const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
const REVOCATION_URL: &str = "https://auth.openai.com/api/accounts/oauth/revoke";
const RESOURCE: &str = "https://api.openai.com/v1";
const CREDENTIAL_FILE: &str = "chatgpt-plan.json";
const LOCK_FILE: &str = ".chatgpt-plan.lock";
const MAX_DOCUMENT_BYTES: usize = 128 * 1024;
const MAX_TOKEN_BYTES: usize = 24 * 1024;
const REFRESH_WINDOW_SECONDS: u64 = 60;
const HTTP_TIMEOUT: Duration = Duration::from_secs(8);

/// Credentials obtained through the mobile client's browser and PKCE flow.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatGptPlanCredentials {
    pub client_id: String,
    #[serde(deserialize_with = "deserialize_secret")]
    pub access_token: SecretString,
    #[serde(deserialize_with = "deserialize_secret")]
    pub refresh_token: SecretString,
    #[serde(deserialize_with = "deserialize_secret")]
    pub id_token: SecretString,
    pub scope: String,
    pub expires_at: u64,
    pub earliest_refresh_at: Option<u64>,
    pub subject: String,
    pub email: Option<String>,
}

fn deserialize_secret<'de, D: serde::Deserializer<'de>>(d: D) -> Result<SecretString, D::Error> {
    String::deserialize(d).map(SecretString::from)
}

/// A fixed diagnostic that never includes OAuth tokens or response bodies.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ChatGptPlanError {
    NotLinked,
    InvalidCredentials,
    DifferentRegistration,
    ReauthorizationRequired,
    RefreshNotAvailable,
    Storage,
    OpenAiUnavailable,
}

impl std::fmt::Display for ChatGptPlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NotLinked => "connect ChatGPT to this Harw server first",
            Self::InvalidCredentials => "invalid ChatGPT plan credentials",
            Self::DifferentRegistration => {
                "disconnect the current ChatGPT connection before linking another registration"
            }
            Self::ReauthorizationRequired => "reconnect ChatGPT to restore plan access",
            Self::RefreshNotAvailable => "ChatGPT plan access cannot renew yet",
            Self::Storage => "ChatGPT plan credential storage is unavailable",
            Self::OpenAiUnavailable => "the OpenAI sign-in service is unavailable",
        })
    }
}

impl std::error::Error for ChatGptPlanError {}

type PlanResult<T> = Result<T, ChatGptPlanError>;

/// One registration in this server's Harw home. The server owns all refreshes.
#[derive(Clone, Debug)]
pub struct ChatGptPlanStore {
    home: PathBuf,
}

impl ChatGptPlanStore {
    #[must_use]
    pub fn new(home: &Path) -> Self {
        Self {
            home: home.to_owned(),
        }
    }

    /// Reports whether this server has a stored registration.
    ///
    /// # Errors
    /// Returns an error if the local credential store is invalid or unreadable.
    pub async fn linked(&self) -> PlanResult<bool> {
        Ok(matches!(self.read()?, Some(StoredCredentials::Linked(_))))
    }

    /// Installs a registration after verifying the OpenAI ID and access tokens.
    /// Callers check authorization to manage this server before calling this.
    /// The browser callback's nonce has already been checked on the phone.
    ///
    /// # Errors
    /// Returns an error on invalid tokens, a different existing registration,
    /// or an unavailable identity provider or credential store.
    pub async fn store_verified(&self, record: &ChatGptPlanCredentials) -> PlanResult<()> {
        let now = unix_now();
        if !record_is_valid(record) || record.expires_at <= now {
            return Err(ChatGptPlanError::InvalidCredentials);
        }
        let client = oauth_client()?;
        let keys: Jwks = read_json(client.get(JWKS_URL).send().await.map_err(network)?).await?;
        let id = verify_jwt(
            &keys,
            record.id_token.expose_secret(),
            &record.client_id,
            now,
        )?;
        if id.sub != record.subject || id.email != record.email {
            return Err(ChatGptPlanError::InvalidCredentials);
        }
        let access = verify_jwt(&keys, record.access_token.expose_secret(), RESOURCE, now)?;
        if access.client_id.as_deref() != Some(&record.client_id)
            || !access.scope.as_deref().is_some_and(has_plan_scope)
        {
            return Err(ChatGptPlanError::InvalidCredentials);
        }
        let mut record = record.clone();
        record.expires_at = record.expires_at.min(access.exp);
        self.install(&record).await
    }

    async fn install(&self, record: &ChatGptPlanCredentials) -> PlanResult<()> {
        let dir = self.open_secrets(true)?.ok_or(ChatGptPlanError::Storage)?;
        let _lock = self.lock(&dir).await?;
        if let Some(current) = read_credentials(&dir)?
            && current.registration() != (record.subject.as_str(), record.client_id.as_str())
        {
            return Err(ChatGptPlanError::DifferentRegistration);
        }
        self.persist(record)
    }

    /// Gets a usable bearer token and rotates it near expiration when allowed.
    ///
    /// # Errors
    /// Returns an error if no authorized plan is linked, or renewal fails and
    /// the stored access token is no longer usable.
    pub async fn access_token(&self) -> PlanResult<SecretString> {
        let current = active_credentials(self.read()?)?;
        let now = unix_now();
        if !refresh_due(&current, now) {
            return usable_token(current, now);
        }
        // A model turn can be cancelled after OpenAI has spent the old refresh
        // token. The storage transaction must finish even if the caller drops
        // its wait for the replacement.
        let store = self.clone();
        tokio::spawn(async move { store.refresh(TOKEN_URL).await })
            .await
            .map_err(network)?
    }

    async fn refresh(&self, endpoint: &str) -> PlanResult<SecretString> {
        let dir = self
            .open_secrets(false)?
            .ok_or(ChatGptPlanError::NotLinked)?;
        let _lock = self.lock(&dir).await?;
        let current = active_credentials(read_credentials(&dir)?)?;
        let now = unix_now();
        // The previous lock holder may already have installed a new token.
        if !refresh_due(&current, now) {
            return usable_token(current, now);
        }

        let result = refresh_tokens(oauth_client()?, endpoint, &current).await;
        match result {
            Ok(next) => {
                self.persist(&next)?;
                Ok(next.access_token)
            }
            Err(ChatGptPlanError::OpenAiUnavailable) if current.expires_at > unix_now() => {
                Ok(current.access_token)
            }
            Err(ChatGptPlanError::ReauthorizationRequired) => {
                self.write_document(&json!({
                    "client_id": current.client_id,
                    "subject": current.subject,
                    "needs_reauthorization": true,
                }))?;
                Err(ChatGptPlanError::ReauthorizationRequired)
            }
            Err(error) => Err(error),
        }
    }

    /// Unlinks locally and attempts to revoke the old renewable session.
    /// Returns `false` if OpenAI did not confirm revocation.
    ///
    /// # Errors
    /// Returns an error if local removal could not be completed.
    pub async fn disconnect(&self) -> PlanResult<bool> {
        let Some(dir) = self.open_secrets(false)? else {
            return Ok(true);
        };
        let _lock = self.lock(&dir).await?;
        let Some(record) = read_credentials(&dir)? else {
            return Ok(true);
        };
        unlink_at(dir.as_fd(), Path::new(CREDENTIAL_FILE)).map_err(storage)?;
        dir.sync_all().map_err(storage)?;
        let StoredCredentials::Linked(record) = record else {
            return Ok(true);
        };
        let Ok(client) = oauth_client() else {
            return Ok(false);
        };
        let response = client
            .post(REVOCATION_URL)
            .form(&[
                ("token", record.refresh_token.expose_secret()),
                ("token_type_hint", "refresh_token"),
                ("client_id", record.client_id.as_str()),
            ])
            .send()
            .await;
        Ok(response.is_ok_and(|response| response.status() == reqwest::StatusCode::OK))
    }

    fn read(&self) -> PlanResult<Option<StoredCredentials>> {
        self.open_secrets(false)?
            .as_ref()
            .map_or(Ok(None), read_credentials)
    }

    fn open_secrets(&self, create: bool) -> PlanResult<Option<File>> {
        let home = match open_dir_nofollow(&self.home) {
            Ok(dir) => File::from(dir),
            Err(error) if !create && error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(storage(error)),
        };
        ensure_owned_directory(&home).map_err(storage)?;
        let name = Path::new("secrets");
        if create && mkdir_at(home.as_fd(), name, 0o700).map_err(storage)? {
            home.sync_all().map_err(storage)?;
        }
        let dir = match open_dir_at(home.as_fd(), name) {
            Ok(dir) => dir,
            Err(error) if !create && error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(storage(error)),
        };
        ensure_owned_directory(&dir).map_err(storage)?;
        Ok(Some(dir))
    }

    async fn lock(&self, dir: &File) -> PlanResult<RefreshLockGuard> {
        let name = Path::new(LOCK_FILE);
        let mode = OpenMode {
            create: true,
            ..OpenMode::read_write()
        };
        let file = open_beneath(dir.as_fd(), name, mode).map_err(storage)?;
        ensure_private_regular(&file).map_err(storage)?;
        acquire_token_file_lock(file, &self.home.join("secrets").join(name))
            .await
            .map_err(storage)
    }

    fn persist(&self, record: &ChatGptPlanCredentials) -> PlanResult<()> {
        self.write_document(&json!({
            "client_id": record.client_id,
            "access_token": record.access_token.expose_secret(),
            "refresh_token": record.refresh_token.expose_secret(),
            "id_token": record.id_token.expose_secret(),
            "scope": record.scope,
            "expires_at": record.expires_at,
            "earliest_refresh_at": record.earliest_refresh_at,
            "subject": record.subject,
            "email": record.email,
        }))
    }

    fn write_document(&self, document: &Value) -> PlanResult<()> {
        let data = serde_json::to_vec(document).map_err(storage)?;
        write_private_file_atomically(&self.home.join("secrets").join(CREDENTIAL_FILE), &data)
            .map_err(storage)
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredCredentials {
    Linked(ChatGptPlanCredentials),
    NeedsReauthorization(Reauthorization),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Reauthorization {
    subject: String,
    client_id: String,
    needs_reauthorization: bool,
}

impl StoredCredentials {
    fn registration(&self) -> (&str, &str) {
        match self {
            Self::Linked(record) => (&record.subject, &record.client_id),
            Self::NeedsReauthorization(record) => (&record.subject, &record.client_id),
        }
    }

    fn is_valid(&self) -> bool {
        match self {
            Self::Linked(record) => record_is_valid(record),
            Self::NeedsReauthorization(record) => record.needs_reauthorization,
        }
    }
}

fn active_credentials(record: Option<StoredCredentials>) -> PlanResult<ChatGptPlanCredentials> {
    match record {
        Some(StoredCredentials::Linked(record)) => Ok(record),
        Some(StoredCredentials::NeedsReauthorization(_)) => {
            Err(ChatGptPlanError::ReauthorizationRequired)
        }
        None => Err(ChatGptPlanError::NotLinked),
    }
}

fn read_credentials(dir: &File) -> PlanResult<Option<StoredCredentials>> {
    let file = match open_beneath(
        dir.as_fd(),
        Path::new(CREDENTIAL_FILE),
        OpenMode::read_only(),
    ) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(storage(error)),
    };
    ensure_private_regular(&file).map_err(storage)?;
    let mut data = Vec::new();
    file.take(MAX_DOCUMENT_BYTES as u64 + 1)
        .read_to_end(&mut data)
        .map_err(storage)?;
    if data.len() > MAX_DOCUMENT_BYTES {
        return Err(ChatGptPlanError::Storage);
    }
    let record: StoredCredentials = serde_json::from_slice(&data).map_err(storage)?;
    if !record.is_valid() {
        return Err(ChatGptPlanError::Storage);
    }
    Ok(Some(record))
}

fn storage<T>(_error: T) -> ChatGptPlanError {
    ChatGptPlanError::Storage
}
fn network<T>(_error: T) -> ChatGptPlanError {
    ChatGptPlanError::OpenAiUnavailable
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn has_plan_scope(scope: &str) -> bool {
    ["resource.invoke", "chatgpt.tokens.use.direct"]
        .iter()
        .all(|required| {
            scope
                .split_ascii_whitespace()
                .any(|value| value == *required)
        })
}

fn valid_token(token: &SecretString) -> bool {
    let token = token.expose_secret();
    !token.is_empty()
        && token.len() <= MAX_TOKEN_BYTES
        && token.bytes().all(|byte| byte.is_ascii_graphic())
}

fn record_is_valid(record: &ChatGptPlanCredentials) -> bool {
    record.client_id.strip_prefix("oaiapp_").is_some_and(|id| {
        !id.is_empty()
            && id.len() <= 256
            && id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    }) && !record.subject.is_empty()
        && record.subject.len() <= 256
        && record.email.as_ref().is_none_or(|email| email.len() <= 512)
        && record.scope.len() <= 1024
        && has_plan_scope(&record.scope)
        && [
            &record.access_token,
            &record.refresh_token,
            &record.id_token,
        ]
        .into_iter()
        .all(valid_token)
}

fn usable_token(current: ChatGptPlanCredentials, now: u64) -> PlanResult<SecretString> {
    if current.expires_at <= now {
        return Err(ChatGptPlanError::RefreshNotAvailable);
    }
    Ok(current.access_token)
}

fn refresh_due(current: &ChatGptPlanCredentials, now: u64) -> bool {
    current.expires_at <= now.saturating_add(REFRESH_WINDOW_SECONDS)
        && current
            .earliest_refresh_at
            .is_none_or(|earliest| earliest <= now)
}

fn oauth_client() -> PlanResult<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(network)
}

async fn read_json<T: DeserializeOwned>(response: reqwest::Response) -> PlanResult<T> {
    if !response.status().is_success() {
        return Err(ChatGptPlanError::OpenAiUnavailable);
    }
    read_json_body(response).await
}

async fn read_json_body<T: DeserializeOwned>(mut response: reqwest::Response) -> PlanResult<T> {
    let mut data = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(network)? {
        if data.len().saturating_add(chunk.len()) > MAX_DOCUMENT_BYTES {
            return Err(ChatGptPlanError::OpenAiUnavailable);
        }
        data.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&data).map_err(network)
}

#[derive(Deserialize)]
struct RefreshResponse {
    #[serde(deserialize_with = "deserialize_secret")]
    access_token: SecretString,
    #[serde(deserialize_with = "deserialize_secret")]
    refresh_token: SecretString,
    expires_in: u64,
    token_type: String,
    scope: Option<String>,
    earliest_refresh_at: Option<Value>,
}

async fn refresh_tokens(
    client: reqwest::Client,
    endpoint: &str,
    current: &ChatGptPlanCredentials,
) -> PlanResult<ChatGptPlanCredentials> {
    let response = client
        .post(endpoint)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", current.client_id.as_str()),
            ("refresh_token", current.refresh_token.expose_secret()),
            ("resource", RESOURCE),
        ])
        .send()
        .await
        .map_err(network)?;
    if response.status().is_client_error() {
        let body: Value = read_json_body(response).await?;
        let code = body
            .get("error")
            .and_then(Value::as_str)
            .or_else(|| body.pointer("/error/code").and_then(Value::as_str));
        return Err(match code {
            Some(
                "invalid_grant"
                | "invalid_refresh_token"
                | "token_expired"
                | "refresh_token_expired"
                | "refresh_token_invalidated"
                | "refresh_token_reused",
            ) => ChatGptPlanError::ReauthorizationRequired,
            _ => ChatGptPlanError::OpenAiUnavailable,
        });
    }
    let refreshed: RefreshResponse = read_json(response).await?;
    let mut next = current.clone();
    next.access_token = refreshed.access_token;
    next.refresh_token = refreshed.refresh_token;
    next.expires_at = unix_now().saturating_add(refreshed.expires_in);
    next.earliest_refresh_at = refreshed.earliest_refresh_at.as_ref().and_then(|value| {
        value.as_u64().or_else(|| {
            let timestamp = value.as_str()?.parse::<jiff::Timestamp>().ok()?;
            u64::try_from(timestamp.as_second()).ok()
        })
    });
    if let Some(scope) = refreshed.scope {
        next.scope = scope;
    }
    if !refreshed.token_type.eq_ignore_ascii_case("bearer")
        || refreshed.expires_in == 0
        || !record_is_valid(&next)
    {
        return Err(ChatGptPlanError::ReauthorizationRequired);
    }
    Ok(next)
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kid: String,
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
    kid: String,
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
    client_id: Option<String>,
    scope: Option<String>,
}

fn verify_jwt(keys: &Jwks, token: &str, audience: &str, now: u64) -> PlanResult<Claims> {
    let invalid = ChatGptPlanError::InvalidCredentials;
    let (signed, signature) = token.rsplit_once('.').ok_or(invalid)?;
    let (header, payload) = signed.split_once('.').ok_or(invalid)?;
    let header: JwtHeader =
        serde_json::from_slice(&decode_segment(header)?).map_err(|_| invalid)?;
    if header.alg != "RS256" {
        return Err(invalid);
    }
    let key = keys
        .keys
        .iter()
        .find(|key| {
            key.kid == header.kid
                && key.kty == "RSA"
                && key.alg.as_deref().is_none_or(|alg| alg == "RS256")
                && key.usage.as_deref().is_none_or(|usage| usage == "sig")
        })
        .ok_or(invalid)?;
    let key = RsaPublicKeyComponents {
        n: decode_segment(key.n.as_deref().ok_or(invalid)?)?,
        e: decode_segment(key.e.as_deref().ok_or(invalid)?)?,
    };
    key.verify(
        &RSA_PKCS1_2048_8192_SHA256,
        signed.as_bytes(),
        &decode_segment(signature)?,
    )
    .map_err(|_| invalid)?;
    let claims: Claims = serde_json::from_slice(&decode_segment(payload)?).map_err(|_| invalid)?;
    let audience_matches = match &claims.aud {
        Value::String(value) => value == audience,
        Value::Array(values) => {
            values.iter().any(|value| value.as_str() == Some(audience))
                && (values.len() == 1 || claims.azp.as_deref() == Some(audience))
        }
        _ => false,
    };
    if !audience_matches
        || claims.iss != ISSUER
        || claims.exp <= now
        || claims.sub.is_empty()
        || claims.nbf.is_some_and(|nbf| nbf > now.saturating_add(30))
    {
        return Err(invalid);
    }
    Ok(claims)
}

fn decode_segment(segment: &str) -> PlanResult<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(segment)
        .map_err(|_| ChatGptPlanError::InvalidCredentials)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write as _};
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt as _;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn example() -> ChatGptPlanCredentials {
        ChatGptPlanCredentials {
            client_id: "oaiapp_fixture".into(),
            access_token: "initial-access".into(),
            refresh_token: "initial-refresh".into(),
            id_token: "initial-id".into(),
            scope: "openid offline_access resource.invoke chatgpt.tokens.use.direct".into(),
            expires_at: unix_now().saturating_add(600),
            earliest_refresh_at: None,
            subject: "fixture-subject".into(),
            email: None,
        }
    }

    #[test]
    fn verifies_a_signed_identity_and_rejects_wrong_audience_or_expired_tokens() -> TestResult {
        // Signed once with a throwaway RSA test key. Only the public key is retained.
        let modulus = concat!(
            "yGk_tOafYG0wccGs3NuCR0Ydljekg8EUiRFHarJ08QdqicyLPN2TSDmtdIVNPVNSaYAp7PmQN1Fce5XvgBCirOZQS6cILRJu4Ow_0VV0e",
            "OddCbkGh0ep7RFeI0XRIVkVmMhBPaD64lN0YNC42ym96bEl7iF4SYtgu4L3D_3KOowgt6D2fu185vC9QldbRCW7-sZZd9Lq95a1dbJqBR",
            "VPXaBcO2EMQgNqhl5fi9vcUPPA7Ce0k_YQEh1rUZGe9ExLFnZd2GgsGkRpGBIcAnOrk55ywwKr6OV_6nDB-sMlSy_XPcqBWiQuM0CJ0gh",
            "oLxTdH_rs54UtaOCAJraTIAPo7w",
        );
        let token = concat!(
            "eyJhbGciOiJSUzI1NiIsImtpZCI6ImZpeHR1cmUifQ.eyJpc3MiOiJodHRwczovL2F1dGgub3BlbmFpLmNvbSIsImF1ZCI6Im9haWFwcF",
            "9maXh0dXJlIiwic3ViIjoidGVzdC1zdWJqZWN0IiwiZXhwIjoyMDAwMDAxMDAwLCJlbWFpbCI6ImZpeHR1cmVAZXhhbXBsZS5pbnZhbGl",
            "kIn0.kw_7-gDfZS8UZZPFpVcOm3e8bjCa76DacW4g8ercJRgCo1J9Z57CghsuJkiIjc-wKdpr_9XVyZ40nmT6w6z22nCapBh33M39o3JK",
            "krL9HzJ8wJ5ZFJ0eJB74lsByCHTUJG9R0qLAT9Lg_pB6ztAC0q6BzKmn6GKMd766ge-FpIp8JNZE7D9wryOZ0gxYI2EzEUO_BHnZ7TQXH",
            "lIIgQ65-UmjNmB3V-VTBrxDjX7f7GE-LpJzKDctpbGwHNnxyaXzOOVXBcxi64a9_tBI0EBIyjbcGF_EU3Oa6zjB4IFtbw5XTMh-5WtfgC",
            "7eQJ9vYdOG4udEWGFiEFwvwV5OLy7EcA",
        );
        let keys = Jwks {
            keys: vec![Jwk {
                kid: "fixture".into(),
                kty: "RSA".into(),
                alg: Some("RS256".into()),
                usage: Some("sig".into()),
                n: Some(modulus.into()),
                e: Some("AQAB".into()),
            }],
        };
        let claims = verify_jwt(&keys, token, "oaiapp_fixture", 2_000_000_000)?;
        assert_eq!(claims.sub, "test-subject");
        assert_eq!(claims.email.as_deref(), Some("fixture@example.invalid"));
        assert!(verify_jwt(&keys, token, "oaiapp_wrong", 2_000_000_000).is_err());
        assert!(verify_jwt(&keys, token, "oaiapp_fixture", 2_000_001_000).is_err());
        let (signed, _) = token
            .rsplit_once('.')
            .ok_or_else(|| io::Error::other("invalid fixture"))?;
        let unsigned = format!("{signed}.AA");
        assert!(verify_jwt(&keys, &unsigned, "oaiapp_fixture", 2_000_000_000).is_err());
        Ok(())
    }

    #[tokio::test]
    async fn unlinked_store_has_no_access_token() -> TestResult {
        let home = harw_test_support::unique_tmp_created("chatgpt-plan", "unlinked")?;
        let store = ChatGptPlanStore::new(&home);
        assert!(!store.linked().await?);
        assert!(matches!(
            store.access_token().await,
            Err(ChatGptPlanError::NotLinked)
        ));
        assert!(store.disconnect().await?);
        std::fs::remove_dir_all(home)?;
        Ok(())
    }

    #[tokio::test]
    async fn refuses_another_registration_and_persists_the_current_one_privately() -> TestResult {
        let home = harw_test_support::unique_tmp_created("chatgpt-plan", "register")?;
        let store = ChatGptPlanStore::new(&home);
        let mut credential = example();
        store.install(&credential).await?;
        credential.client_id = "oaiapp_different".into();
        assert_eq!(
            store.install(&credential).await,
            Err(ChatGptPlanError::DifferentRegistration)
        );
        assert!(ChatGptPlanStore::new(&home).linked().await?);
        assert!(store.access_token().await?.expose_secret() == "initial-access");
        assert_eq!(
            std::fs::metadata(home.join("secrets").join(CREDENTIAL_FILE))?
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        std::fs::remove_dir_all(home)?;
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_refreshes_use_the_rotated_set_without_reusing_the_old_token() -> TestResult
    {
        let home = harw_test_support::unique_tmp_created("chatgpt-plan", "rotate")?;
        let store = ChatGptPlanStore::new(&home);
        let mut credential = example();
        credential.expires_at = unix_now().saturating_sub(1);
        store.install(&credential).await?;

        let listener = TcpListener::bind("127.0.0.1:0")?;
        let endpoint = format!("http://{}/token", listener.local_addr()?);
        let exchange = std::thread::spawn(move || -> io::Result<String> {
            let (mut stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(3)))?;
            let mut reader = BufReader::new(&mut stream);
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line)?;
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().map_err(io::Error::other)?;
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body)?;
            let reply = r#"{"access_token":"replacement-access","refresh_token":"replacement-refresh","expires_in":3600,"token_type":"Bearer"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            )?;
            String::from_utf8(body).map_err(io::Error::other)
        });

        let (first, second) = tokio::join!(store.refresh(&endpoint), store.refresh(&endpoint));
        assert!(first?.expose_secret() == "replacement-access");
        assert!(second?.expose_secret() == "replacement-access");
        let persisted = active_credentials(store.read()?)?;
        assert!(persisted.refresh_token.expose_secret() == "replacement-refresh");
        let form = exchange
            .join()
            .map_err(|_| io::Error::other("test token exchange panicked"))??;
        assert!(form.contains("grant_type=refresh_token"));
        assert!(form.contains("client_id=oaiapp_fixture"));
        assert!(form.contains("refresh_token=initial-refresh"));
        assert!(form.contains("resource=https%3A%2F%2Fapi.openai.com%2Fv1"));
        assert!(!form.contains("scope"));
        std::fs::remove_dir_all(home)?;
        Ok(())
    }
}
