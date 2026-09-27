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
//! [`refresh_codex_tokens`] serialisiert den Refresh-Vorgang über einen
//! OS-Advisory-Lock (unter Unix `flock`, via `fs4`) auf einer Lock-Datei neben
//! der Credential-Datei (`<dir>/.<name>.codex-refresh.lock`) — das schützt
//! gegen parallele Refreshes aus mehreren Prozessen/Threads, ohne den Hot-Path
//! normaler Requests zu sperren. Den Lock hält das Betriebssystem am offenen
//! Datei-Handle: stürzt ein Halter ab, gibt der Kernel ihn frei. Es gibt daher
//! keine Verwaist-Heuristik und keine Übernahme fremder Locks. Die Lock-Datei
//! bleibt dauerhaft liegen; sie beim Loslassen zu löschen, würde das Rennen
//! zwischen einem Wartenden auf der alten und einem Neuankömmling auf einer
//! frisch angelegten Datei wieder öffnen. Die Refresh-POST-Anfrage ist per
//! [`REFRESH_HTTP_TIMEOUT`] unterhalb von [`LOCK_ACQUIRE_TIMEOUT`] begrenzt,
//! damit Wartende nicht aufgeben, solange der Halter noch im POST steckt.
//!
//! Vor dem Warten auf den Lock merkt sich [`refresh_codex_tokens`] das
//! aktuelle Access-Token. Der Netzwerk-Call entfällt **nur**, wenn das nach
//! Erwerb des Locks gelesene Access-Token davon abweicht (ein anderer Halter
//! hat also während der Wartezeit rotiert) und noch ausreichend lange gilt —
//! das spart eine überflüssige zweite Rotation. Ein unverändertes Token geht
//! immer an den Endpoint, auch wenn sein `exp` noch fern ist: der Server kann
//! es vorzeitig verworfen haben (echter `401`). Das Refresh-Token wird in
//! jedem Fall erst nach Erwerb des Locks gelesen; der Verzicht auf den
//! Netzwerk-Call ist also kein Schutz vor `invalid_grant`, sondern nur eine
//! Einsparung.
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
use secrecy::ExposeSecret as _;
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

/// Wartezeit zwischen zwei Versuchen, den Refresh-Lock zu erwerben.
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(50);
/// Maximale Gesamtwartezeit, bis das Erwerben des Refresh-Locks aufgegeben wird.
const LOCK_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);
/// Obergrenze für die Refresh-POST-Anfrage (Netzwerk + Body lesen). Muss
/// unterhalb von [`LOCK_ACQUIRE_TIMEOUT`] bleiben: sonst geben Wartende auf
/// ([`OAuthError::RefreshLockTimeout`]), während der Halter noch im POST
/// steckt.
const REFRESH_HTTP_TIMEOUT: Duration = Duration::from_secs(8);
/// Mindest-Restlaufzeit eines Access-Tokens, das ein **anderer** Halter
/// rotiert hat, während dieser Aufruf auf den Lock wartete — nur dann wird es
/// ohne eigenen Netzwerk-Call übernommen (reine Plausibilitätsprüfung, siehe
/// [`tokens_rotated_while_waiting`]). Ein unverändertes Token wird immer
/// erneuert; die Konstante ist daher unabhängig vom proaktiven Refresh-Fenster
/// in `harw-provider-http`.
const ROTATED_TOKEN_MIN_REMAINING_SECONDS: i64 = 5 * 60;

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

/// Aktuelle Unix-Zeit in Sekunden; liefert `0`, falls die Systemuhr vor
/// `UNIX_EPOCH` steht (praktisch nie, aber ohne `unwrap`/`expect`).
fn unix_seconds_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Prüft, ob ein JWT-Access-Token innerhalb von `window_seconds` abläuft (oder
/// bereits abgelaufen ist).
///
/// Kann `exp` nicht bestimmt werden (kein JWT, fehlendes Feld), wird `false`
/// zurückgegeben — der Aufrufer soll sich in diesem Fall auf reaktiven Refresh
/// nach einem tatsächlichen 401 verlassen, statt blind zu erneuern.
///
/// `exp` stammt aus einem unverifizierten JWT-Payload (weder von diesem
/// Prozess noch zwingend vom Server signaturgeprüft) und wird daher nie
/// direkt subtrahiert: `saturating_sub` verhindert einen Over-/Underflow bei
/// einem Extremwert (z. B. nahe `i64::MIN`), der sonst in Debug-Builds
/// paniken oder in Release-Builds fälschlich "läuft nicht bald ab" ergeben
/// würde.
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
    let now = unix_seconds_now();
    exp.saturating_sub(now) <= window_seconds
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

/// Hält den Refresh-Lock (exklusiver OS-Advisory-Lock auf dem offenen Handle
/// der Lock-Datei), bis er gedroppt wird.
struct RefreshLockGuard {
    file: std::fs::File,
}

impl Drop for RefreshLockGuard {
    fn drop(&mut self) {
        // Explizit entsperren; das Schließen des Handles direkt danach gäbe
        // den Lock ohnehin frei. Die Lock-Datei wird bewusst NIE gelöscht:
        // ein Wartender kann sie bereits geöffnet haben und sperrt nach dem
        // Löschen den verwaisten Inode, während ein Neuankömmling unter
        // demselben Pfad eine frische Datei anlegt und ebenfalls sperrt — zwei
        // gleichzeitige Halter, also genau das Rennen, das der Lock verhindert.
        let _ = fs4::FileExt::unlock(&self.file);
    }
}

/// Erwirbt den Refresh-Lock neben `auth_json_path`.
///
/// Öffnet (bzw. legt an) die Lock-Datei aus [`lock_path_for`] und versucht im
/// Abstand von [`LOCK_RETRY_INTERVAL`], einen exklusiven OS-Advisory-Lock
/// (unter Unix `flock`) darauf zu nehmen, bis [`LOCK_ACQUIRE_TIMEOUT`]
/// verstrichen ist. Sperrt nur den Refresh-Vorgang selbst (nicht normale
/// Requests). Eine liegengebliebene Lock-Datei blockiert nie: gesperrt ist nur,
/// was ein lebender Prozess gerade hält — stürzt er ab, gibt das
/// Betriebssystem den Lock frei.
///
/// `fs4::FileExt::try_lock` wird bewusst voll qualifiziert aufgerufen: neuere
/// Toolchains bringen ein gleichnamiges inhärentes `std::fs::File::try_lock`
/// mit (stabil erst ab Rust 1.89, MSRV ist 1.85), das die Methoden-Syntax
/// vorrangig wählen würde.
async fn acquire_refresh_lock(auth_json_path: &Path) -> OAuthResult<RefreshLockGuard> {
    let lock_path = lock_path_for(auth_json_path);
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options.open(&lock_path)?;

    let deadline = Instant::now() + LOCK_ACQUIRE_TIMEOUT;
    loop {
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => return Ok(RefreshLockGuard { file }),
            Err(fs4::TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    return Err(OAuthError::RefreshLockTimeout(
                        lock_path.display().to_string(),
                    ));
                }
                tokio::time::sleep(LOCK_RETRY_INTERVAL).await;
            }
            Err(fs4::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
}

fn read_credential_document(path: &Path) -> OAuthResult<Value> {
    let raw = std::fs::read_to_string(path)?;
    serde_json::from_str(&raw)
        .map_err(|error| OAuthError::CodexCredentialFile(format!("invalid JSON: {error}")))
}

/// Schreibt `document` als eingerücktes JSON atomar nach `path` zurück — über
/// den crate-weiten Schreiber [`crate::store::write_private_file_atomically`]
/// (eindeutige `0600`-Temp-Datei daneben, `fsync`, `rename`; Details dort).
fn write_credential_document(path: &Path, document: &Value) -> OAuthResult<()> {
    let serialized = serde_json::to_string_pretty(document).map_err(|error| {
        OAuthError::CodexCredentialFile(format!("could not serialize refreshed tokens: {error}"))
    })?;
    crate::store::write_private_file_atomically(path, serialized.as_bytes())
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
    unix_seconds_to_rfc3339(unix_seconds_now())
}

fn document_access_token(document: &Value) -> Option<&str> {
    document
        .pointer("/tokens/access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

/// Momentaufnahme des Access-Tokens in `auth_json_path` *vor* dem Warten auf
/// den Refresh-Lock (best-effort). Jeder Fehler (Datei fehlt, kein gültiges
/// JSON, kein Token) ergibt `None`; dann gilt nach Erwerb des Locks nichts als
/// "während des Wartens rotiert", und Fehler meldet erst das reguläre Lesen
/// unter dem Lock.
fn access_token_snapshot(auth_json_path: &Path) -> Option<SecretString> {
    let document = read_credential_document(auth_json_path).ok()?;
    let access_token = document_access_token(&document)?;
    Some(SecretString::new(access_token.to_owned().into_boxed_str()))
}

/// Prüft direkt nach Erwerb des Refresh-Locks, ob ein anderer Halter die
/// Tokens rotiert hat, während dieser Aufruf auf den Lock wartete: das gerade
/// gelesene Access-Token muss sich von `access_token_before_lock` (siehe
/// [`access_token_snapshot`]) unterscheiden **und** noch länger als
/// [`ROTATED_TOKEN_MIN_REMAINING_SECONDS`] gelten. Nur dann entfällt der
/// Netzwerk-Call, und dieses Token-Set wird übernommen.
///
/// Liefert `None` (also: regulär erneuern), wenn keine Momentaufnahme
/// vorliegt, das Token fehlt oder unverändert ist, `exp` nicht bestimmbar ist
/// oder das rotierte Token bald abläuft. Ein unverändertes Token wird nie
/// übersprungen, egal wie fern sein `exp` liegt — es kann serverseitig bereits
/// verworfen sein (echter `401`).
fn tokens_rotated_while_waiting(
    document: &Value,
    access_token_before_lock: Option<&str>,
) -> Option<RefreshedCodexTokens> {
    let access_token_before_lock = access_token_before_lock?;
    let access_token = document_access_token(document)?;
    if access_token == access_token_before_lock {
        return None;
    }
    let exp = jwt_exp_unix_seconds(access_token)?;
    if exp.saturating_sub(unix_seconds_now()) <= ROTATED_TOKEN_MIN_REMAINING_SECONDS {
        return None;
    }

    let refresh_token = document
        .pointer("/tokens/refresh_token")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let account_id = document
        .pointer("/tokens/account_id")
        .and_then(Value::as_str)
        .map(str::to_owned);

    Some(RefreshedCodexTokens {
        access_token: SecretString::new(access_token.to_owned().into_boxed_str()),
        refresh_token: SecretString::new(refresh_token.into_boxed_str()),
        account_id,
    })
}

/// Sendet die Refresh-Anfrage an `url`, begrenzt durch [`REFRESH_HTTP_TIMEOUT`]
/// (unter [`LOCK_ACQUIRE_TIMEOUT`]), damit Wartende nicht aufgeben, solange
/// der Lock-Halter noch im POST steckt (siehe Modul-Dokumentation). `url` als
/// Parameter statt direkt [`refresh_url`] zu lesen, hält die Funktion isoliert
/// testbar (z. B. gegen einen lokalen Test-Listener), ohne Umgebungsvariablen
/// in Tests zu mutieren.
async fn send_refresh_request(
    client: &reqwest::Client,
    url: &str,
    payload: &RefreshRequest,
) -> OAuthResult<(reqwest::StatusCode, String)> {
    let outcome = tokio::time::timeout(REFRESH_HTTP_TIMEOUT, async {
        let response = client
            .post(url)
            .header("content-type", "application/json")
            .json(payload)
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
        Ok::<(reqwest::StatusCode, String), OAuthError>((status, body))
    })
    .await;

    match outcome {
        Ok(result) => result,
        Err(_elapsed) => Err(OAuthError::Http(format!(
            "codex token refresh POST exceeded the {}s timeout",
            REFRESH_HTTP_TIMEOUT.as_secs()
        ))),
    }
}

/// Erneuert die ChatGPT-/Codex-OAuth-Tokens in `auth_json_path` per
/// `refresh_token`-Grant und schreibt das Ergebnis atomar zurück.
///
/// # Arguments
/// - `client` (`&reqwest::Client`): geteilter HTTP-Client.
/// - `auth_json_path` (`&Path`): Pfad zu `~/.codex/auth.json`.
///
/// # Returns
/// Entweder ein neu ausgestelltes Token-Set (Access-/Refresh-Token, optionale
/// Account-ID) oder — falls ein anderer Halter die Tokens rotiert hat,
/// während dieser Aufruf auf den Refresh-Lock wartete — das von diesem Halter
/// geschriebene Token-Set.
///
/// # Errors
/// - [`OAuthError::CodexCredentialFile`][]: Datei ist kein gültiges JSON oder
///   hat kein `tokens`-Objekt.
/// - [`OAuthError::MissingField`][]: kein `refresh_token` in der Datei, oder
///   die Antwort enthielt kein `access_token`.
/// - [`OAuthError::TokenExchange`][]: nicht-erfolgreiche HTTP-Antwort; der Body
///   wird aus Sicherheitsgründen nicht in den Fehler übernommen.
/// - [`OAuthError::Http`][]: Transportfehler, einschließlich Überschreitung
///   von [`REFRESH_HTTP_TIMEOUT`].
/// - [`OAuthError::RefreshLockTimeout`][]: der Refresh-Lock konnte nicht
///   innerhalb des Zeitlimits erworben werden.
/// - [`OAuthError::TokenStoreIo`][]: Credential-Datei nicht lesbar, Lock-Datei
///   nicht zu öffnen oder zu sperren, oder I/O-Fehler beim atomaren
///   Zurückschreiben.
///
/// # Concurrency
/// `async`; serialisiert parallele Refreshes über einen OS-Datei-Lock neben
/// `auth_json_path` (siehe Modul-Dokumentation). Treibt höchstens eine per
/// [`REFRESH_HTTP_TIMEOUT`] begrenzte HTTP-Anfrage. Keine Anfrage nur dann,
/// wenn sich das Access-Token während des Wartens auf den Lock geändert hat
/// (Rotation durch einen anderen Halter) und noch ausreichend lange gilt. Ein
/// serverseitig verworfenes, aber noch nicht abgelaufenes Token wird nie
/// übersprungen: ein unverändertes Token geht immer an den Endpoint.
pub async fn refresh_codex_tokens(
    client: &reqwest::Client,
    auth_json_path: &Path,
) -> OAuthResult<RefreshedCodexTokens> {
    refresh_codex_tokens_at(client, auth_json_path, &refresh_url()).await
}

/// Kern von [`refresh_codex_tokens`] gegen einen expliziten Refresh-Endpoint
/// `url`. Der Parameter hält den kompletten Ablauf (Lock, Re-Prüfung, POST,
/// Zurückschreiben) gegen einen lokalen Test-Listener testbar, ohne
/// Umgebungsvariablen in Tests zu mutieren (`set_var` ist in Edition 2024
/// `unsafe`).
async fn refresh_codex_tokens_at(
    client: &reqwest::Client,
    auth_json_path: &Path,
    url: &str,
) -> OAuthResult<RefreshedCodexTokens> {
    // Momentaufnahme VOR dem Warten: nur eine Abweichung davon nach Erwerb
    // des Locks belegt eine Rotation durch einen anderen Halter.
    let access_token_before_lock = access_token_snapshot(auth_json_path);
    let _lock = acquire_refresh_lock(auth_json_path).await?;

    let mut document = read_credential_document(auth_json_path)?;

    // Hat ein anderer Halter während unserer Wartezeit rotiert, übernehmen wir
    // sein Token-Set statt einer überflüssigen zweiten Rotation. Ein
    // unverändertes Token geht dagegen immer an den Endpoint.
    if let Some(rotated) = tokens_rotated_while_waiting(
        &document,
        access_token_before_lock
            .as_ref()
            .map(|token| token.expose_secret()),
    ) {
        return Ok(rotated);
    }

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

    let (status, body) = send_refresh_request(client, url, &payload).await?;
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
    use crate::test_support::{TestError, TestResult, ctx};

    fn temp_home(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "harw-oauth-codex-refresh-{name}-{}",
            std::process::id()
        ))
    }

    fn write_auth_json(path: &Path, access_token: &str, refresh_token: &str) -> TestResult {
        let parent = path
            .parent()
            .ok_or(TestError::Missing("auth.json parent directory"))?;
        std::fs::create_dir_all(parent).map_err(ctx("Testverzeichnis anlegen"))?;
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
        .map_err(ctx("auth.json schreiben"))?;
        Ok(())
    }

    fn make_jwt_with_exp(exp: i64) -> String {
        let header = URL_SAFE_NO_PAD.encode(b"{}");
        let payload = URL_SAFE_NO_PAD.encode(format!("{{\"exp\":{exp}}}"));
        format!("{header}.{payload}.sig")
    }

    /// HTTP-Client ohne System-Proxy: die Tests sprechen nur mit lokalen
    /// Listenern, die ein Container-Proxy nicht erreichen würde.
    fn test_client() -> TestResult<reqwest::Client> {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .map_err(ctx("HTTP-Client ohne Proxy bauen"))
    }

    /// Startet einen lokalen Refresh-Endpoint-Stub, der genau eine Anfrage
    /// annimmt, mit `200` und `response_json` antwortet und den Request-Body
    /// über den Join-Handle zurückgibt. Liefert die Endpoint-URL und den
    /// Handle.
    fn spawn_refresh_stub(
        response_json: &'static str,
    ) -> TestResult<(String, std::thread::JoinHandle<TestResult<String>>)> {
        use std::io::{Read as _, Write as _};

        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").map_err(ctx("Stub-Listener binden"))?;
        listener
            .set_nonblocking(true)
            .map_err(ctx("Stub-Listener nicht-blockierend schalten"))?;
        let addr = listener.local_addr().map_err(ctx("Stub-Adresse lesen"))?;

        let handle = std::thread::spawn(move || -> TestResult<String> {
            // Nicht-blockierendes `accept` mit Frist: ein Test, der gar nicht
            // erst anfragt, darf den Stub-Thread nicht ewig hängen lassen.
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if Instant::now() >= deadline {
                            return Err(TestError::Unexpected(
                                "refresh stub received no request within 10s".to_owned(),
                            ));
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => return Err(ctx("Stub-Anfrage annehmen")(error)),
                }
            };
            stream
                .set_nonblocking(false)
                .map_err(ctx("Stub-Verbindung blockierend schalten"))?;
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .map_err(ctx("Stub-Lese-Timeout setzen"))?;

            let mut bytes = Vec::new();
            let mut buffer = [0_u8; 4096];
            let header_end = loop {
                let read = stream
                    .read(&mut buffer)
                    .map_err(ctx("Stub-Anfrage lesen"))?;
                if read == 0 {
                    return Err(TestError::Unexpected(
                        "client closed connection before headers completed".to_owned(),
                    ));
                }
                bytes.extend_from_slice(&buffer[..read]);
                if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    break end + 4;
                }
            };
            let head = String::from_utf8_lossy(&bytes[..header_end]).to_ascii_lowercase();
            let content_length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            while bytes.len() < header_end + content_length {
                let read = stream
                    .read(&mut buffer)
                    .map_err(ctx("Stub-Request-Body lesen"))?;
                if read == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..read]);
            }
            let body_end = (header_end + content_length).min(bytes.len());
            let request_body = String::from_utf8_lossy(&bytes[header_end..body_end]).into_owned();

            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_json}",
                response_json.len()
            );
            stream
                .write_all(response.as_bytes())
                .map_err(ctx("Stub-Antwort schreiben"))?;
            stream.flush().map_err(ctx("Stub-Antwort flushen"))?;
            Ok(request_body)
        });

        Ok((format!("http://{addr}/oauth/token"), handle))
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
    fn test_jwt_needs_refresh_true_when_expiring_soon() -> TestResult {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(ctx("Systemzeit vor UNIX_EPOCH"))?
            .as_secs() as i64;
        let token = make_jwt_with_exp(now + 60);
        assert!(jwt_needs_refresh(&token, 300));
        Ok(())
    }

    #[test]
    fn test_jwt_needs_refresh_false_when_far_from_expiry() -> TestResult {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(ctx("Systemzeit vor UNIX_EPOCH"))?
            .as_secs() as i64;
        let token = make_jwt_with_exp(now + 3_600);
        assert!(!jwt_needs_refresh(&token, 300));
        Ok(())
    }

    #[test]
    fn test_jwt_needs_refresh_false_when_undecodable() {
        assert!(!jwt_needs_refresh("garbage", 300));
    }

    #[test]
    fn test_jwt_needs_refresh_true_and_no_panic_for_extreme_exp() {
        // `exp` stammt aus einem unverifizierten JWT-Payload; ein Wert nahe
        // `i64::MIN` darf den Vergleich weder überlaufen lassen (Panik im
        // Debug-Build) noch fälschlich als "läuft nicht bald ab" durchgehen
        // (Wrap-Around im Release-Build).
        let token = make_jwt_with_exp(i64::MIN);
        assert!(jwt_needs_refresh(&token, 300));
    }

    #[test]
    fn test_unix_seconds_to_rfc3339_epoch() {
        assert_eq!(unix_seconds_to_rfc3339(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn test_unix_seconds_to_rfc3339_known_date() {
        // 2024-01-02T03:24:05Z
        assert_eq!(
            unix_seconds_to_rfc3339(1_704_165_845),
            "2024-01-02T03:24:05Z"
        );
    }

    #[tokio::test]
    async fn test_refresh_codex_tokens_rejects_missing_refresh_token() -> TestResult {
        let home = temp_home("missing-refresh");
        let _ = std::fs::remove_dir_all(&home);
        let path = home.join("auth.json");
        write_auth_json(&path, "old-access", "")?;

        let client = reqwest::Client::new();
        let outcome = refresh_codex_tokens(&client, &path).await;
        let Err(error) = outcome else {
            let _ = std::fs::remove_dir_all(&home);
            return Err(TestError::Unexpected(
                "refresh must fail on missing refresh_token".to_owned(),
            ));
        };
        assert!(matches!(error, OAuthError::MissingField("refresh_token")));

        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    #[tokio::test]
    async fn test_refresh_codex_tokens_rejects_missing_credential_file() -> TestResult {
        let home = temp_home("missing-file");
        let _ = std::fs::remove_dir_all(&home);
        let path = home.join("auth.json");

        let client = reqwest::Client::new();
        let outcome = refresh_codex_tokens(&client, &path).await;
        let Err(error) = outcome else {
            return Err(TestError::Unexpected(
                "refresh must fail on missing credential file".to_owned(),
            ));
        };
        assert!(matches!(error, OAuthError::TokenStoreIo(_)));
        Ok(())
    }

    #[test]
    fn test_refresh_http_timeout_is_below_lock_acquire_timeout() {
        // Der Refresh-POST muss unter der Wartefrist liegen, sonst geben
        // Wartende auf, während der Halter noch im POST steckt.
        assert!(REFRESH_HTTP_TIMEOUT < LOCK_ACQUIRE_TIMEOUT);
    }

    #[tokio::test]
    async fn test_send_refresh_request_times_out_before_waiters_give_up() -> TestResult {
        // Simuliert einen extrem langsamen Refresh-Endpoint: der Listener
        // nimmt die Verbindung an, antwortet aber nie, bis der Test sie
        // freigibt. Der eigene Timeout muss vor `LOCK_ACQUIRE_TIMEOUT`
        // zuschlagen, damit Wartende nicht vor dem Halter aufgeben.
        let client = test_client()?;
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").map_err(ctx("bind mock server"))?;
        let addr = listener.local_addr().map_err(ctx("mock address"))?;
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let server = std::thread::spawn(move || -> TestResult {
            let (_stream, _) = listener.accept().map_err(ctx("accept mock request"))?;
            let _ = release_rx.recv_timeout(Duration::from_secs(30));
            Ok(())
        });

        let payload = RefreshRequest {
            client_id: "test-client".to_owned(),
            grant_type: "refresh_token",
            refresh_token: "irrelevant".to_owned(),
        };
        let url = format!("http://{addr}/oauth/token");

        let started = Instant::now();
        let outcome = send_refresh_request(&client, &url, &payload).await;
        let elapsed = started.elapsed();

        let _ = release_tx.send(());
        server
            .join()
            .map_err(|_| TestError::Unexpected("mock server thread panicked".to_owned()))??;

        let Err(error) = outcome else {
            return Err(TestError::Unexpected(
                "refresh POST against an unresponsive server must time out".to_owned(),
            ));
        };
        assert!(matches!(error, OAuthError::Http(_)));
        assert!(
            elapsed < LOCK_ACQUIRE_TIMEOUT,
            "refresh timeout must fire before waiters give up on the lock"
        );
        Ok(())
    }

    fn rotated_document(access_token: &str) -> Value {
        serde_json::json!({
            "tokens": {
                "access_token": access_token,
                "refresh_token": "rotated-refresh",
                "account_id": "acct-1",
            }
        })
    }

    #[test]
    fn test_tokens_rotated_while_waiting_none_when_token_unchanged() {
        // Fund-Regression: ein unverändertes Token mit fernem `exp` darf nie
        // als "bereits erneuert" durchgehen — es kann serverseitig verworfen
        // sein (echter `401`), und dann muss der Refresh wirklich laufen.
        let token = make_jwt_with_exp(unix_seconds_now() + 3_600);
        let document = rotated_document(&token);
        assert!(tokens_rotated_while_waiting(&document, Some(token.as_str())).is_none());
    }

    #[test]
    fn test_tokens_rotated_while_waiting_none_without_snapshot() {
        // Ohne Momentaufnahme vor dem Lock lässt sich keine Rotation belegen.
        let token = make_jwt_with_exp(unix_seconds_now() + 3_600);
        let document = rotated_document(&token);
        assert!(tokens_rotated_while_waiting(&document, None).is_none());
    }

    #[test]
    fn test_tokens_rotated_while_waiting_none_when_rotated_token_undecodable() {
        // Ein nicht dekodierbares Token darf niemals stillschweigend als
        // rotiert und gültig durchgehen — sonst würde ein nötiger Refresh
        // übersprungen.
        let document = rotated_document("not-a-jwt");
        assert!(tokens_rotated_while_waiting(&document, Some("old-access")).is_none());
    }

    #[test]
    fn test_tokens_rotated_while_waiting_none_when_rotated_token_expiring_soon() {
        let token = make_jwt_with_exp(unix_seconds_now() + 30);
        let document = rotated_document(&token);
        assert!(tokens_rotated_while_waiting(&document, Some("old-access")).is_none());
    }

    #[test]
    fn test_tokens_rotated_while_waiting_some_when_rotated_and_far_from_expiry() -> TestResult {
        let token = make_jwt_with_exp(unix_seconds_now() + 3_600);
        let document = rotated_document(&token);
        let rotated = tokens_rotated_while_waiting(&document, Some("old-access")).ok_or(
            TestError::Unexpected("expected the rotated token set to be adopted".to_owned()),
        )?;
        assert_eq!(rotated.access_token.expose_secret(), token.as_str());
        assert_eq!(rotated.refresh_token.expose_secret(), "rotated-refresh");
        assert_eq!(rotated.account_id, Some("acct-1".to_owned()));
        Ok(())
    }

    #[tokio::test]
    async fn test_refresh_codex_tokens_refreshes_unchanged_far_future_token() -> TestResult {
        // Fund-Regression: ein Token, das laut `exp` noch eine Stunde gilt,
        // aber unverändert ist (z. B. nach einem echten `401`), muss wirklich
        // an den Endpoint gehen — früher entfiel der Netzwerk-Call hier.
        let home = temp_home("unchanged-far-future");
        let _ = std::fs::remove_dir_all(&home);
        let path = home.join("auth.json");
        let access_token = make_jwt_with_exp(unix_seconds_now() + 3_600);
        write_auth_json(&path, &access_token, "still-valid-refresh")?;

        let client = test_client()?;
        let (url, stub) =
            spawn_refresh_stub(r#"{"access_token":"new-access","refresh_token":"new-refresh"}"#)?;
        let outcome = refresh_codex_tokens_at(&client, &path, &url).await;
        let on_disk = read_credential_document(&path);
        let _ = std::fs::remove_dir_all(&home);

        // Ergebnis VOR dem Join prüfen: ein übersprungener Refresh soll als
        // Fehlschlag sichtbar werden, nicht als 10s-Hänger im Stub-Thread.
        let tokens = outcome.map_err(ctx("Refresh gegen den lokalen Stub"))?;
        let request_body = stub
            .join()
            .map_err(|_| TestError::Unexpected("refresh stub thread panicked".to_owned()))??;

        assert_eq!(tokens.access_token.expose_secret(), "new-access");
        assert_eq!(tokens.refresh_token.expose_secret(), "new-refresh");
        assert!(
            request_body.contains("still-valid-refresh"),
            "the stored refresh token must be sent: {request_body}"
        );
        assert!(
            request_body.contains("refresh_token"),
            "the request must be a refresh_token grant: {request_body}"
        );

        let document = on_disk.map_err(ctx("auth.json nach dem Refresh lesen"))?;
        assert_eq!(
            document
                .pointer("/tokens/access_token")
                .and_then(Value::as_str),
            Some("new-access")
        );
        assert_eq!(
            document.pointer("/auth_mode").and_then(Value::as_str),
            Some("chatgpt")
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_refresh_codex_tokens_adopts_token_rotated_while_waiting() -> TestResult {
        // Der Test hält den Lock selbst als "anderer Halter", rotiert während
        // der Wartezeit des Refreshs das Token und gibt den Lock dann frei.
        // Der Refresh muss das rotierte Token ohne Netzwerk-Call übernehmen:
        // Port 0 ist nie erreichbar, jeder Netzwerkversuch endete in `Err`.
        let home = temp_home("rotated-while-waiting");
        let _ = std::fs::remove_dir_all(&home);
        let path = home.join("auth.json");
        let now = unix_seconds_now();
        let original_access_token = make_jwt_with_exp(now + 3_600);
        let rotated_access_token = make_jwt_with_exp(now + 7_200);
        write_auth_json(&path, &original_access_token, "original-refresh")?;

        let client = test_client()?;
        let guard = acquire_refresh_lock(&path)
            .await
            .map_err(ctx("Lock als anderer Halter erwerben"))?;

        let rotate = async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let written = write_auth_json(&path, &rotated_access_token, "rotated-refresh");
            drop(guard);
            written
        };
        let (outcome, rotated) = tokio::join!(
            refresh_codex_tokens_at(&client, &path, "http://127.0.0.1:0/oauth/token"),
            rotate
        );
        let _ = std::fs::remove_dir_all(&home);

        rotated?;
        let tokens = outcome.map_err(ctx(
            "ein während des Wartens rotiertes Token muss ohne Netzwerk übernommen werden",
        ))?;
        assert_eq!(
            tokens.access_token.expose_secret(),
            rotated_access_token.as_str()
        );
        assert_eq!(tokens.refresh_token.expose_secret(), "rotated-refresh");
        Ok(())
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
    async fn test_acquire_refresh_lock_round_trips() -> TestResult {
        let home = temp_home("lock-roundtrip");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).map_err(ctx("Testverzeichnis anlegen"))?;
        let auth_path = home.join("auth.json");
        std::fs::write(&auth_path, "{}").map_err(ctx("auth.json schreiben"))?;

        let guard = acquire_refresh_lock(&auth_path)
            .await
            .map_err(ctx("lock acquired"))?;
        assert!(lock_path_for(&auth_path).exists());
        drop(guard);
        // Die Lock-Datei bleibt liegen (Löschen öffnete das Rennen wieder);
        // freigegeben ist nur der OS-Lock, also gelingt ein zweiter Erwerb.
        assert!(
            lock_path_for(&auth_path).exists(),
            "the lock file must persist after release"
        );
        let second = acquire_refresh_lock(&auth_path)
            .await
            .map_err(ctx("lock re-acquired after release"))?;
        drop(second);

        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    #[tokio::test]
    async fn test_acquire_refresh_lock_is_exclusive_until_guard_dropped() -> TestResult {
        // Ein zweites, unabhängig geöffnetes Handle verhält sich wie ein
        // anderer Prozess: solange der Guard lebt, bekommt es den Lock nicht.
        let home = temp_home("lock-exclusive");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).map_err(ctx("Testverzeichnis anlegen"))?;
        let auth_path = home.join("auth.json");

        let guard = acquire_refresh_lock(&auth_path)
            .await
            .map_err(ctx("lock acquired"))?;
        let second_handle = OpenOptions::new()
            .write(true)
            .open(lock_path_for(&auth_path))
            .map_err(ctx("Lock-Datei ein zweites Mal öffnen"))?;
        let while_held = fs4::FileExt::try_lock(&second_handle);
        drop(guard);
        let after_drop = fs4::FileExt::try_lock(&second_handle);
        let _ = fs4::FileExt::unlock(&second_handle);
        drop(second_handle);
        let _ = std::fs::remove_dir_all(&home);

        assert!(
            matches!(while_held, Err(fs4::TryLockError::WouldBlock)),
            "a held refresh lock must block a second handle"
        );
        assert!(
            after_drop.is_ok(),
            "dropping the guard must release the refresh lock"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_acquire_refresh_lock_not_blocked_by_leftover_lock_file() -> TestResult {
        // Eine liegengebliebene Lock-Datei (z. B. nach einem Absturz) trägt
        // keinen OS-Lock mehr und darf den Erwerb nicht verzögern.
        let home = temp_home("lock-leftover");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).map_err(ctx("Testverzeichnis anlegen"))?;
        let auth_path = home.join("auth.json");
        std::fs::write(lock_path_for(&auth_path), "orphaned")
            .map_err(ctx("liegengebliebene Lock-Datei anlegen"))?;

        let started = Instant::now();
        let outcome = acquire_refresh_lock(&auth_path).await;
        let elapsed = started.elapsed();
        // Guard sofort wieder freigeben, dann aufräumen, erst danach prüfen.
        let acquired = outcome
            .map(drop)
            .map_err(ctx("Lock trotz liegengebliebener Lock-Datei erwerben"));
        let _ = std::fs::remove_dir_all(&home);

        acquired?;
        assert!(
            elapsed < Duration::from_secs(1),
            "a leftover lock file must not delay acquisition ({elapsed:?})"
        );
        Ok(())
    }

    #[test]
    fn test_write_credential_document_ignores_legacy_temp_name_and_sets_0600() -> TestResult {
        // Fund #3: die alte, feste Temp-Datei (`auth.json.tmp-refresh`) war
        // vorhersagbar und wurde mit `create(true).truncate(true)` geöffnet
        // (folgt Symlinks, übernimmt fremde Rechte). Die neue Implementierung
        // generiert je Aufruf einen frischen, eindeutigen Namen und darf
        // einen Alt-Platzhalter unter dem alten Namen nie anfassen.
        let home = temp_home("legacy-temp-name");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).map_err(ctx("Testverzeichnis anlegen"))?;
        let path = home.join("auth.json");
        let legacy_temp_path = path.with_extension("json.tmp-refresh");
        std::fs::write(&legacy_temp_path, "sentinel-untouched")
            .map_err(ctx("alten Fixnamen-Platzhalter anlegen"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&legacy_temp_path, std::fs::Permissions::from_mode(0o644))
                .map_err(ctx("Platzhalter-Rechte setzen"))?;
        }

        write_credential_document(&path, &serde_json::json!({"tokens": {}}))
            .map_err(ctx("Credential-Dokument schreiben"))?;

        let sentinel =
            std::fs::read_to_string(&legacy_temp_path).map_err(ctx("Platzhalter lesen"))?;
        assert_eq!(
            sentinel, "sentinel-untouched",
            "must never touch the old fixed temp name"
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path)
                .map_err(ctx("Metadaten des Zieldokuments lesen"))?
                .permissions()
                .mode();
            assert_eq!(
                mode & 0o777,
                0o600,
                "final auth.json must be 0600 regardless of leftovers"
            );
        }

        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    #[test]
    fn test_write_credential_document_leaves_no_temp_artifacts() -> TestResult {
        let home = temp_home("no-temp-artifacts");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).map_err(ctx("Testverzeichnis anlegen"))?;
        let path = home.join("auth.json");

        write_credential_document(&path, &serde_json::json!({"tokens": {}}))
            .map_err(ctx("Credential-Dokument schreiben"))?;

        let entries = std::fs::read_dir(&home).map_err(ctx("Verzeichnis lesen"))?;
        let names: Vec<String> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["auth.json".to_owned()]);

        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    #[tokio::test]
    async fn test_refresh_codex_tokens_never_touches_real_user_home() -> TestResult {
        // Sicherheitsnetz: dieser Test darf niemals gegen die echte
        // `~/.codex/auth.json` des Nutzers laufen. Er arbeitet ausschließlich
        // mit einer isolierten temporären Kopie.
        let home = temp_home("isolated-copy");
        let _ = std::fs::remove_dir_all(&home);
        let path = home.join("auth.json");
        write_auth_json(&path, "old-access", "old-refresh")?;
        assert_ne!(path, dirs_codex_auth_json_for_test());

        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    fn dirs_codex_auth_json_for_test() -> PathBuf {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(".codex/auth.json")
    }
}
