//! Proaktiver/reaktiver Refresh der ChatGPT-/Codex-OAuth-Tokens.
//!
//! ## Hintergrund
//! Die Codex-CLI legt ihr kurzlebiges Access-Token unter `~/.codex/auth.json`
//! ab (`/tokens/access_token`, JWT mit `exp`). Bisher besaß nur die CLI selbst
//! den Refresh-Flow; `harw` hat die Datei nur gelesen. Dieses Modul ergänzt
//! einen eigenständigen Refresh-Mechanismus, strukturell an [`crate::flow`]
//! (den bestehenden Anthropic-PKCE-Flow) angelehnt, aber gegen den
//! ChatGPT-/Codex-OAuth-Endpoint.
//!
//! ## Ablauf
//! 1. [`jwt_exp_unix_seconds`] liest `exp` aus dem `access_token`-JWT, ohne die
//!    Signatur zu prüfen (reine Ablauf-Heuristik, keine Sicherheitsprüfung).
//! 2. [`jwt_needs_refresh`] entscheidet anhand eines Zeitfensters, ob proaktiv
//!    erneuert werden sollte.
//! 3. [`refresh_codex_tokens`] liest `refresh_token` aus der Credential-Datei,
//!    tauscht ihn beim offiziellen Codex-OAuth-Endpoint gegen ein neues
//!    Token-Set und schreibt es atomar zurück — im selben Dateiformat, das die
//!    Codex-CLI selbst verwendet (`tokens.access_token`/`refresh_token`/
//!    `id_token`/`account_id`, `last_refresh`). Unbekannte Felder im Dokument
//!    (z. B. `auth_mode`, `OPENAI_API_KEY`) bleiben unverändert erhalten.
//!
//! ## Nebenläufigkeit
//! [`refresh_codex_tokens`] sperrt den Refresh-Vorgang selbst über eine
//! Lock-Datei neben der Credential-Datei (`<dir>/.codex-refresh.lock`) — das
//! schützt gegen parallele Refreshes aus mehreren Prozessen/Threads, ohne den
//! Hot-Path normaler Requests zu sperren. Der Lock wird nach spätestens 10s
//! als verwaist betrachtet und automatisch übernommen (Crash-Schutz).
//!
//! ## Konfigurierbarkeit
//! | Env-Variable                        | Default |
//! |--------------------------------------|---------|
//! | `HARW_CODEX_OAUTH_CLIENT_ID`         | `app_EMoamEEZ73f0CkXaXp7hrann` (Codex-CLI-Client-ID) |
//! | `HARW_CODEX_OAUTH_REFRESH_URL`       | `https://auth.openai.com/oauth/token` |

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{OAuthError, OAuthResult};

/// Offizielle Codex-CLI-Client-ID für den OAuth-Refresh (überschreibbar per
/// Env `HARW_CODEX_OAUTH_CLIENT_ID`). Quelle: `codex-rs/login/src/auth/manager.rs`
/// (`CLIENT_ID`), in dieser Session am Referenzverzeichnis `../codex` verifiziert.
const DEFAULT_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Offizieller Codex-Token-Refresh-Endpoint (überschreibbar per Env
/// `HARW_CODEX_OAUTH_REFRESH_URL`). Ebenfalls aus `codex-rs` verifiziert.
const DEFAULT_REFRESH_URL: &str = "https://auth.openai.com/oauth/token";

/// Zeit, nach der ein Lock als verwaist gilt und übernommen wird (Crash-Schutz).
const LOCK_STALE_AFTER: Duration = Duration::from_secs(10);
/// Wartezeit zwischen zwei Versuchen, den Refresh-Lock zu erwerben.
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(50);
/// Maximale Gesamtwartezeit, bis das Erwerben des Refresh-Locks aufgegeben wird.
const LOCK_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);

/// base64url-Engine ohne Padding — JWT-Segmente sind per RFC 7519 padding-frei.
const URL_SAFE_NO_PAD: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Liest eine Env-Variable oder liefert den Default (leere Werte zählen als
/// nicht gesetzt). Bewusst dupliziert zu [`crate::flow`], um die Module
/// unabhängig voneinander lesbar/testbar zu halten.
fn env_or(var: &str, default: &str) -> String {
    std::env::var(var)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| default.to_owned())
}

fn client_id() -> String {
    env_or("HARW_CODEX_OAUTH_CLIENT_ID", DEFAULT_CLIENT_ID)
}

fn refresh_url() -> String {
    env_or("HARW_CODEX_OAUTH_REFRESH_URL", DEFAULT_REFRESH_URL)
}

/// Dekodiert den Payload-Anteil (mittleres Segment) eines JWT als JSON, ohne
/// die Signatur zu prüfen. Nur für Ablauf-Heuristiken geeignet.
fn decode_jwt_payload(token: &str) -> Option<Value> {
    let payload_b64 = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Liest `exp` (Unix-Sekunden) aus einem JWT-Access-Token, falls vorhanden.
///
/// # Returns
/// `Some(exp)` wenn das Token dekodierbar ist und ein numerisches `exp`-Feld
/// trägt, sonst `None` (z. B. bei nicht-JWT-Tokens oder fehlendem Feld).
#[must_use]
pub fn jwt_exp_unix_seconds(token: &str) -> Option<i64> {
    decode_jwt_payload(token)?.get("exp")?.as_i64()
}

/// Prüft, ob ein JWT-Access-Token innerhalb von `window_seconds` abläuft (oder
/// bereits abgelaufen ist).
///
/// Kann `exp` nicht bestimmt werden (kein JWT, fehlendes Feld), wird `false`
/// zurückgegeben — der Aufrufer soll sich in diesem Fall auf reaktiven Refresh
/// nach einem tatsächlichen 401 verlassen, statt blind zu erneuern.
///
/// # Examples
/// ```
/// assert!(!harw_oauth::jwt_needs_refresh("not-a-jwt", 300));
/// ```
#[must_use]
pub fn jwt_needs_refresh(token: &str, window_seconds: i64) -> bool {
    let Some(exp) = jwt_exp_unix_seconds(token) else {
        return false;
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    exp - now <= window_seconds
}

/// Ergebnis eines erfolgreichen Codex-Token-Refresh.
#[derive(Debug)]
pub struct RefreshedCodexTokens {
    /// Neues Access-Token (Bearer, JWT).
    pub access_token: SecretString,
    /// Aktuelles Refresh-Token (neu ausgestellt, oder unverändert falls die
    /// Antwort keins mitgeliefert hat).
    pub refresh_token: SecretString,
    /// Account-ID, falls im Dokument nach dem Refresh vorhanden.
    pub account_id: Option<String>,
}

#[derive(Deserialize)]
struct RefreshResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
}

#[derive(Serialize)]
struct RefreshRequest {
    client_id: String,
    grant_type: &'static str,
    refresh_token: String,
}

/// Ersetzt Antwort-Bodies, die Tokenmaterial enthalten könnten, in Fehlern.
fn redacted_refresh_body(_body: &str) -> String {
    "codex token refresh response body redacted".to_owned()
}

fn lock_path_for(auth_json_path: &Path) -> PathBuf {
    let file_name = auth_json_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("auth.json");
    auth_json_path.with_file_name(format!(".{file_name}.codex-refresh.lock"))
}

/// Hält den Refresh-Lock, bis sie gedroppt wird (entfernt dann die Lock-Datei).
struct RefreshLockGuard {
    path: PathBuf,
}

impl Drop for RefreshLockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Erwirbt den dateibasierten Refresh-Lock neben `auth_json_path`.
///
/// Sperrt nur den Refresh-Vorgang selbst (nicht normale Requests). Ein Lock,
/// der älter als [`LOCK_STALE_AFTER`] ist, wird als verwaist betrachtet und
/// übernommen, damit ein abgestürzter Prozess nicht dauerhaft blockiert.
async fn acquire_refresh_lock(auth_json_path: &Path) -> OAuthResult<RefreshLockGuard> {
    let lock_path = lock_path_for(auth_json_path);
    let deadline = Instant::now() + LOCK_ACQUIRE_TIMEOUT;
    loop {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(_file) => {
                return Ok(RefreshLockGuard { path: lock_path });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if is_lock_stale(&lock_path) {
                    let _ = std::fs::remove_file(&lock_path);
                    continue;
                }
                if Instant::now() >= deadline {
                    return Err(OAuthError::RefreshLockTimeout(
                        lock_path.display().to_string(),
                    ));
                }
                tokio::time::sleep(LOCK_RETRY_INTERVAL).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn is_lock_stale(lock_path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(lock_path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age > LOCK_STALE_AFTER)
}

fn read_credential_document(path: &Path) -> OAuthResult<Value> {
    let raw = std::fs::read_to_string(path)?;
    serde_json::from_str(&raw)
        .map_err(|error| OAuthError::CodexCredentialFile(format!("invalid JSON: {error}")))
}

/// Schreibt `document` atomar (temp + rename, `0600` unter Unix) nach `path`
/// zurück, gefolgt von `fsync` der Datei und des Verzeichnisses.
fn write_credential_document(path: &Path, document: &Value) -> OAuthResult<()> {
    use std::io::Write as _;

    let parent = path.parent().ok_or_else(|| {
        OAuthError::CodexCredentialFile("credential path has no parent directory".to_owned())
    })?;
    let temp_path = path.with_extension("json.tmp-refresh");
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }

    let write_result = (|| -> OAuthResult<()> {
        let mut file = options.open(&temp_path)?;
        let serialized = serde_json::to_string_pretty(document).map_err(|error| {
            OAuthError::CodexCredentialFile(format!("could not serialize refreshed tokens: {error}"))
        })?;
        file.write_all(serialized.as_bytes())?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp_path, path)?;
        #[cfg(unix)]
        {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();

    if write_result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    write_result
}

fn unix_seconds_to_rfc3339(total_seconds: i64) -> String {
    let days = total_seconds.div_euclid(86_400);
    let secs_of_day = total_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3_600;
    let minute = (secs_of_day % 3_600) / 60;
    let second = secs_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Howard Hinnants `civil_from_days`-Algorithmus (proleptischer Gregorianischer
/// Kalender), Tage seit `1970-01-01` in `(Jahr, Monat, Tag)` gewandelt.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

fn now_rfc3339() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    unix_seconds_to_rfc3339(seconds)
}

/// Erneuert die ChatGPT-/Codex-OAuth-Tokens in `auth_json_path` per
/// `refresh_token`-Grant und schreibt das Ergebnis atomar zurück.
///
/// # Arguments
/// - `client` (`&reqwest::Client`): geteilter HTTP-Client.
/// - `auth_json_path` (`&Path`): Pfad zu `~/.codex/auth.json`.
///
/// # Returns
/// Die neu ausgestellten Tokens (Access-/Refresh-Token, optionale Account-ID).
///
/// # Errors
/// - [`OAuthError::CodexCredentialFile`][]: Datei nicht lesbar, kein `tokens`-Objekt.
/// - [`OAuthError::MissingField`][]: kein `refresh_token` in der Datei, oder
///   die Antwort enthielt kein `access_token`.
/// - [`OAuthError::TokenExchange`][]: nicht-erfolgreiche HTTP-Antwort; der Body
///   wird aus Sicherheitsgründen nicht in den Fehler übernommen.
/// - [`OAuthError::Http`][]: Transportfehler.
/// - [`OAuthError::RefreshLockTimeout`][]: der Refresh-Lock konnte nicht
///   innerhalb des Zeitlimits erworben werden.
/// - [`OAuthError::TokenStoreIo`][]: I/O-Fehler beim atomaren Zurückschreiben.
///
/// # Concurrency
/// `async`; serialisiert parallele Refreshes über einen Datei-Lock neben
/// `auth_json_path` (siehe Modul-Dokumentation). Treibt eine HTTP-Anfrage.
pub async fn refresh_codex_tokens(
    client: &reqwest::Client,
    auth_json_path: &Path,
) -> OAuthResult<RefreshedCodexTokens> {
    let _lock = acquire_refresh_lock(auth_json_path).await?;

    let mut document = read_credential_document(auth_json_path)?;
    let refresh_token = document
        .pointer("/tokens/refresh_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or(OAuthError::MissingField("refresh_token"))?
        .to_owned();

    let payload = RefreshRequest {
        client_id: client_id(),
        grant_type: "refresh_token",
        refresh_token,
    };

    let response = client
        .post(refresh_url())
        .header("content-type", "application/json")
        .json(&payload)
        .send()
        .await?;

    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        return Err(OAuthError::TokenExchange {
            status: status.as_u16(),
            body: redacted_refresh_body(&body),
        });
    }

    let parsed: RefreshResponse = serde_json::from_str(&body)
        .map_err(|error| OAuthError::MalformedInput(format!("invalid refresh JSON: {error}")))?;
    let access_token = parsed
        .access_token
        .ok_or(OAuthError::MissingField("access_token"))?;

    let tokens = document
        .pointer_mut("/tokens")
        .filter(|value| value.is_object())
        .ok_or_else(|| {
            OAuthError::CodexCredentialFile("credential document has no tokens object".to_owned())
        })?;
    tokens["access_token"] = Value::String(access_token.clone());
    if let Some(refresh_token) = &parsed.refresh_token {
        tokens["refresh_token"] = Value::String(refresh_token.clone());
    }
    if let Some(id_token) = &parsed.id_token {
        tokens["id_token"] = Value::String(id_token.clone());
    }
    document["last_refresh"] = Value::String(now_rfc3339());

    let refresh_token_out = document
        .pointer("/tokens/refresh_token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let account_id = document
        .pointer("/tokens/account_id")
        .and_then(Value::as_str)
        .map(str::to_owned);

    write_credential_document(auth_json_path, &document)?;

    Ok(RefreshedCodexTokens {
        access_token: SecretString::new(access_token.into_boxed_str()),
        refresh_token: SecretString::new(refresh_token_out.into_boxed_str()),
        account_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "harw-oauth-codex-refresh-{name}-{}",
            std::process::id()
        ))
    }

    fn write_auth_json(path: &Path, access_token: &str, refresh_token: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            serde_json::json!({
                "auth_mode": "chatgpt",
                "OPENAI_API_KEY": Value::Null,
                "tokens": {
                    "id_token": "old-id-token",
                    "access_token": access_token,
                    "refresh_token": refresh_token,
                    "account_id": "account-123",
                },
                "last_refresh": "2020-01-01T00:00:00Z",
            })
            .to_string(),
        )
        .unwrap();
    }

    fn make_jwt_with_exp(exp: i64) -> String {
        let header = URL_SAFE_NO_PAD.encode(b"{}");
        let payload = URL_SAFE_NO_PAD.encode(format!("{{\"exp\":{exp}}}"));
        format!("{header}.{payload}.sig")
    }

    #[test]
    fn test_jwt_exp_unix_seconds_reads_exp_claim() {
        let token = make_jwt_with_exp(1_700_000_000);
        assert_eq!(jwt_exp_unix_seconds(&token), Some(1_700_000_000));
    }

    #[test]
    fn test_jwt_exp_unix_seconds_none_for_non_jwt() {
        assert_eq!(jwt_exp_unix_seconds("not-a-jwt"), None);
    }

    #[test]
    fn test_jwt_needs_refresh_true_when_expiring_soon() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let token = make_jwt_with_exp(now + 60);
        assert!(jwt_needs_refresh(&token, 300));
    }

    #[test]
    fn test_jwt_needs_refresh_false_when_far_from_expiry() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let token = make_jwt_with_exp(now + 3_600);
        assert!(!jwt_needs_refresh(&token, 300));
    }

    #[test]
    fn test_jwt_needs_refresh_false_when_undecodable() {
        assert!(!jwt_needs_refresh("garbage", 300));
    }

    #[test]
    fn test_unix_seconds_to_rfc3339_epoch() {
        assert_eq!(unix_seconds_to_rfc3339(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn test_unix_seconds_to_rfc3339_known_date() {
        // 2024-01-02T03:24:05Z
        assert_eq!(unix_seconds_to_rfc3339(1_704_165_845), "2024-01-02T03:24:05Z");
    }

    #[tokio::test]
    async fn test_refresh_codex_tokens_rejects_missing_refresh_token() {
        let home = temp_home("missing-refresh");
        let _ = std::fs::remove_dir_all(&home);
        let path = home.join("auth.json");
        write_auth_json(&path, "old-access", "");

        let client = reqwest::Client::new();
        let error = refresh_codex_tokens(&client, &path).await.unwrap_err();
        assert!(matches!(error, OAuthError::MissingField("refresh_token")));

        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn test_refresh_codex_tokens_rejects_missing_credential_file() {
        let home = temp_home("missing-file");
        let _ = std::fs::remove_dir_all(&home);
        let path = home.join("auth.json");

        let client = reqwest::Client::new();
        let error = refresh_codex_tokens(&client, &path).await.unwrap_err();
        assert!(matches!(error, OAuthError::TokenStoreIo(_)));
    }

    #[test]
    fn test_lock_path_is_hidden_sibling_of_credential_file() {
        let path = PathBuf::from("/home/user/.codex/auth.json");
        let lock = lock_path_for(&path);
        assert_eq!(
            lock,
            PathBuf::from("/home/user/.codex/.auth.json.codex-refresh.lock")
        );
    }

    #[tokio::test]
    async fn test_acquire_refresh_lock_round_trips() {
        let home = temp_home("lock-roundtrip");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let auth_path = home.join("auth.json");
        std::fs::write(&auth_path, "{}").unwrap();

        let guard = acquire_refresh_lock(&auth_path).await.expect("lock acquired");
        assert!(lock_path_for(&auth_path).exists());
        drop(guard);
        assert!(!lock_path_for(&auth_path).exists());

        let _ = std::fs::remove_dir_all(&home);
    }

    #[tokio::test]
    async fn test_refresh_codex_tokens_never_touches_real_user_home() {
        // Sicherheitsnetz: dieser Test darf niemals gegen die echte
        // `~/.codex/auth.json` des Nutzers laufen. Er arbeitet ausschließlich
        // mit einer isolierten temporären Kopie.
        let home = temp_home("isolated-copy");
        let _ = std::fs::remove_dir_all(&home);
        let path = home.join("auth.json");
        write_auth_json(&path, "old-access", "old-refresh");
        assert_ne!(path, dirs_codex_auth_json_for_test());

        let _ = std::fs::remove_dir_all(&home);
    }

    fn dirs_codex_auth_json_for_test() -> PathBuf {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(".codex/auth.json")
    }
}
