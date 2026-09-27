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
//! Hot-Path normaler Requests zu sperren. Die Lock-Datei trägt einen
//! einmaligen Nonce als Inhalt; beim Loslassen entfernt ein Halter die Datei
//! nur, wenn dieser Nonce noch drinsteht — so löscht kein Halter jemals den
//! Lock, den inzwischen ein anderer Prozess übernommen hat. Ein Lock, der
//! älter als 10s ist, gilt als verwaist und wird per atomarem `rename` auf
//! einen eindeutigen Tombstone-Namen übernommen (nie per `remove_file`);
//! `rename` kann von zwei Wartenden nie beide gleichzeitig gewinnen, sodass
//! der Lock nie doppelt "erworben" wird. Die Refresh-POST-Anfrage selbst ist
//! auf deutlich unter 10s begrenzt (siehe [`REFRESH_HTTP_TIMEOUT`]), damit ein
//! noch aktiver Halter niemals als verwaist erscheint. Nach Erwerb des Locks
//! wird zusätzlich erneut geprüft, ob das Access-Token inzwischen von einem
//! anderen Halter bereits erneuert wurde; ist es das, entfällt der
//! Netzwerk-Call — sonst würde ein bereits rotiertes Refresh-Token erneut
//! gesendet, was der Endpoint als `invalid_grant` ablehnt und die ganze
//! Token-Familie invalidieren kann.
//!
//! ## Konfigurierbarkeit
//! | Env-Variable                        | Default |
//! |--------------------------------------|---------|
//! | `HARW_CODEX_OAUTH_CLIENT_ID`         | `app_EMoamEEZ73f0CkXaXp7hrann` (Codex-CLI-Client-ID) |
//! | `HARW_CODEX_OAUTH_REFRESH_URL`       | `https://auth.openai.com/oauth/token` |

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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
/// Obergrenze für die Refresh-POST-Anfrage (Netzwerk + Body lesen), deutlich
/// unterhalb von [`LOCK_STALE_AFTER`] — ein Halter, der die Anfrage noch
/// abwartet, darf niemals als verwaist erscheinen.
const REFRESH_HTTP_TIMEOUT: Duration = Duration::from_secs(8);
/// Fenster für die Re-Prüfung direkt nach Erwerb des Locks: läuft das gerade
/// gelesene Access-Token nicht innerhalb dieses Fensters ab, hat es
/// vermutlich bereits ein anderer Halter erneuert — der Netzwerk-Call entfällt
/// dann (siehe [`refresh_codex_tokens`]).
const LOCK_HOLDER_REFRESH_WINDOW_SECONDS: i64 = 5 * 60;

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

static LOCK_NONCE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Erzeugt einen pro Aufruf eindeutigen Nonce-String (Prozess-ID + Nanosekunden
/// + Zähler), der als Inhalt einer frisch erworbenen Lock-Datei dient. Damit
/// kann sowohl [`RefreshLockGuard`] beim Loslassen als auch eine Übernahme
/// erkennen, ob eine Lock-Datei noch demselben Halter gehört.
fn lock_nonce() -> String {
    let counter = LOCK_NONCE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    format!("{}-{nanos}-{counter}", std::process::id())
}

/// Hält den Refresh-Lock, bis sie gedroppt wird.
struct RefreshLockGuard {
    path: PathBuf,
    /// Nonce, die beim Erwerb in die Lock-Datei geschrieben wurde. Nur wenn
    /// die Datei beim Drop noch genau diesen Inhalt trägt, gehört sie noch
    /// uns — sonst hat sie inzwischen ein anderer Prozess übernommen (siehe
    /// [`acquire_refresh_lock`]), und wir dürfen sie nicht entfernen.
    nonce: String,
}

impl Drop for RefreshLockGuard {
    fn drop(&mut self) {
        match std::fs::read_to_string(&self.path) {
            Ok(content) if content == self.nonce => {
                let _ = std::fs::remove_file(&self.path);
            }
            // Andere Fälle (fremder Inhalt, Datei bereits verschwunden, I/O-
            // Fehler beim Lesen): nicht anfassen. Ein fremder Lock darf nicht
            // gelöscht werden; ein bereits verschwundener braucht nichts mehr.
            _ => {}
        }
    }
}

/// Übernimmt einen als verwaist erkannten Lock, indem er atomar auf einen
/// eindeutigen Tombstone-Namen umbenannt wird, statt ihn per `remove_file` zu
/// löschen. `rename` kann von der ursprünglichen Quelle aus nur für genau
/// einen Aufrufer erfolgreich sein — für alle anderen, die dieselbe verwaiste
/// Datei gesehen haben, schlägt ihr eigener `rename`-Versuch mit `NotFound`
/// fehl (die Quelle existiert dann schon nicht mehr). So kann ein einzelner
/// verwaister Lock nie von zwei Wartenden gleichzeitig "erworben" werden, wie
/// es mit einem bloßen `remove_file` möglich wäre. Der Tombstone selbst wird
/// direkt danach best-effort aufgeräumt; niemand hält mehr eine Referenz
/// darauf.
///
/// # Returns
/// `true`, wenn dieser Aufruf die Übernahme gewonnen hat (der Aufrufer darf
/// als Nächstes `create_new` versuchen); `false`, wenn ein anderer Wartender
/// bereits gewonnen hat (der Aufrufer soll die Schleife einfach erneut
/// durchlaufen).
fn take_over_stale_lock(lock_path: &Path) -> bool {
    let file_name = lock_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("codex-refresh.lock");
    let tombstone = lock_path.with_file_name(format!("{file_name}.stale-{}", lock_nonce()));
    let won = std::fs::rename(lock_path, &tombstone).is_ok();
    if won {
        let _ = std::fs::remove_file(&tombstone);
    }
    won
}

/// Erwirbt den dateibasierten Refresh-Lock neben `auth_json_path`.
///
/// Sperrt nur den Refresh-Vorgang selbst (nicht normale Requests). Ein Lock,
/// der älter als [`LOCK_STALE_AFTER`] ist, wird als verwaist betrachtet und
/// per [`take_over_stale_lock`] übernommen, damit ein abgestürzter Prozess
/// nicht dauerhaft blockiert.
async fn acquire_refresh_lock(auth_json_path: &Path) -> OAuthResult<RefreshLockGuard> {
    use std::io::Write as _;

    let lock_path = lock_path_for(auth_json_path);
    let deadline = Instant::now() + LOCK_ACQUIRE_TIMEOUT;
    loop {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(mut file) => {
                let nonce = lock_nonce();
                // Best-effort: scheitert das Schreiben (z. B. Platte voll),
                // bleibt der Lock trotzdem exklusiv gehalten (`create_new` hat
                // bereits zugeschlagen); nur der Nonce-Abgleich beim Drop
                // degradiert dann konservativ (siehe `RefreshLockGuard::drop`).
                let _ = file.write_all(nonce.as_bytes());
                let _ = file.flush();
                return Ok(RefreshLockGuard {
                    path: lock_path,
                    nonce,
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if is_lock_stale(&lock_path) {
                    let _ = take_over_stale_lock(&lock_path);
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

static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Erzeugt einen eindeutigen Temp-Pfad neben `path` (PID + Nanosekunden +
/// Zähler), damit `create_new` keine kollidierenden Namen sieht.
fn temporary_credential_path(path: &Path) -> PathBuf {
    let counter = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("auth.json");
    path.with_file_name(format!(
        ".{file_name}.tmp-refresh-{}-{nanos}-{counter}",
        std::process::id()
    ))
}

/// Legt eine neue, exklusiv (`create_new`) angelegte Temp-Datei mit `0600`
/// unter Unix an. `create_new` verweigert sowohl eine bereits existierende
/// reguläre Datei als auch einen bereits existierenden Symlink (`O_EXCL`) —
/// anders als `create(true).truncate(true)` folgt es also nie einem
/// untergeschobenen Symlink und übernimmt nie die Rechte einer
/// Alt-/Fremddatei. Die `mode(0o600)` beim Öffnen gilt nur für neu angelegte
/// Dateien; `set_permissions` danach setzt sie zusätzlich explizit auf dem
/// offenen Handle (doppelte Absicherung, falls die Open-Flags je nach
/// Plattform/Umask abweichen).
fn create_restricted_temp(path: &Path) -> OAuthResult<(std::fs::File, PathBuf)> {
    for _ in 0..16 {
        let temporary_path = temporary_credential_path(path);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }

        match options.open(&temporary_path) {
            Ok(file) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
                }
                return Ok((file, temporary_path));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a unique temporary Codex credential file",
    )
    .into())
}

/// Schreibt `document` atomar (eindeutige Temp-Datei + rename, `0600` unter
/// Unix) nach `path` zurück, gefolgt von `fsync` der Datei und des
/// Verzeichnisses.
fn write_credential_document(path: &Path, document: &Value) -> OAuthResult<()> {
    use std::io::Write as _;

    let parent = path.parent().ok_or_else(|| {
        OAuthError::CodexCredentialFile("credential path has no parent directory".to_owned())
    })?;
    let (file, temp_path) = create_restricted_temp(path)?;

    let write_result = (|| -> OAuthResult<()> {
        let mut file = file;
        let serialized = serde_json::to_string_pretty(document).map_err(|error| {
            OAuthError::CodexCredentialFile(format!(
                "could not serialize refreshed tokens: {error}"
            ))
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
    unix_seconds_to_rfc3339(unix_seconds_now())
}

fn document_access_token(document: &Value) -> Option<&str> {
    document
        .pointer("/tokens/access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

/// Prüft direkt nach Erwerb des Refresh-Locks, ob das gerade gelesene
/// Access-Token bereits außerhalb von [`LOCK_HOLDER_REFRESH_WINDOW_SECONDS`]
/// abläuft — dann hat es vermutlich schon ein anderer Halter erneuert,
/// während dieser Aufruf auf den Lock wartete, und der Netzwerk-Call entfällt.
///
/// Liefert `None` (also: trotzdem erneuern), wenn `exp` nicht bestimmbar ist —
/// nur eine positiv bestätigte, ausreichend ferne Ablaufzeit gilt als
/// "bereits frisch". Ein nicht dekodierbares Token darf nie stillschweigend
/// als frisch durchgehen.
fn already_fresh_tokens(document: &Value) -> Option<RefreshedCodexTokens> {
    let access_token = document_access_token(document)?;
    let exp = jwt_exp_unix_seconds(access_token)?;
    if exp.saturating_sub(unix_seconds_now()) <= LOCK_HOLDER_REFRESH_WINDOW_SECONDS {
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
/// (deutlich unter [`LOCK_STALE_AFTER`]), damit ein noch aktiver Lock-Halter
/// nie als verwaist erscheint (siehe Modul-Dokumentation). `url` als Parameter
/// statt direkt [`refresh_url`] zu lesen, hält die Funktion isoliert testbar
/// (z. B. gegen einen lokalen Test-Listener), ohne Umgebungsvariablen in
/// Tests zu mutieren.
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
/// Die neu ausgestellten Tokens (Access-/Refresh-Token, optionale Account-ID).
///
/// # Errors
/// - [`OAuthError::CodexCredentialFile`][]: Datei nicht lesbar, kein `tokens`-Objekt.
/// - [`OAuthError::MissingField`][]: kein `refresh_token` in der Datei, oder
///   die Antwort enthielt kein `access_token`.
/// - [`OAuthError::TokenExchange`][]: nicht-erfolgreiche HTTP-Antwort; der Body
///   wird aus Sicherheitsgründen nicht in den Fehler übernommen.
/// - [`OAuthError::Http`][]: Transportfehler, einschließlich Überschreitung
///   von [`REFRESH_HTTP_TIMEOUT`].
/// - [`OAuthError::RefreshLockTimeout`][]: der Refresh-Lock konnte nicht
///   innerhalb des Zeitlimits erworben werden.
/// - [`OAuthError::TokenStoreIo`][]: I/O-Fehler beim atomaren Zurückschreiben.
///
/// # Concurrency
/// `async`; serialisiert parallele Refreshes über einen Datei-Lock neben
/// `auth_json_path` (siehe Modul-Dokumentation). Treibt eine per
/// [`REFRESH_HTTP_TIMEOUT`] begrenzte HTTP-Anfrage — außer, das nach Erwerb
/// des Locks erneut gelesene Access-Token ist bereits frisch, weil ein
/// anderer Halter zwischenzeitlich erneuert hat; dann entfällt der
/// Netzwerk-Call ([`already_fresh_tokens`]).
pub async fn refresh_codex_tokens(
    client: &reqwest::Client,
    auth_json_path: &Path,
) -> OAuthResult<RefreshedCodexTokens> {
    let _lock = acquire_refresh_lock(auth_json_path).await?;

    let mut document = read_credential_document(auth_json_path)?;

    // Re-Prüfung nach Erwerb des Locks: hat ein anderer Halter, der während
    // unserer Wartezeit den Lock hielt, bereits erneuert, entfällt der
    // Netzwerk-Call — sonst würde das (jetzt rotierte) alte Refresh-Token
    // erneut gesendet und vom Endpoint als `invalid_grant` abgelehnt.
    if let Some(fresh) = already_fresh_tokens(&document) {
        return Ok(fresh);
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

    let (status, body) = send_refresh_request(client, &refresh_url(), &payload).await?;
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
    fn test_refresh_http_timeout_is_well_below_lock_stale_after() {
        // Kernvoraussetzung von Fund #1(a): der Refresh-POST muss deutlich
        // unter der Verwaist-Schwelle liegen, sonst könnte ein noch aktiver
        // Halter selbst als verwaist erscheinen.
        assert!(REFRESH_HTTP_TIMEOUT < LOCK_STALE_AFTER);
    }

    #[tokio::test]
    async fn test_send_refresh_request_times_out_well_before_lock_looks_stale() -> TestResult {
        // Simuliert einen extrem langsamen Refresh-Endpoint: der Listener
        // nimmt die Verbindung an, antwortet aber nie, bis der Test sie
        // freigibt. Der eigene Timeout muss lange vor `LOCK_STALE_AFTER`
        // zuschlagen (siehe Fund #1(a)).
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").map_err(ctx("bind mock server"))?;
        let addr = listener.local_addr().map_err(ctx("mock address"))?;
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let server = std::thread::spawn(move || -> TestResult {
            let (_stream, _) = listener.accept().map_err(ctx("accept mock request"))?;
            let _ = release_rx.recv_timeout(Duration::from_secs(30));
            Ok(())
        });

        let client = reqwest::Client::new();
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
            elapsed < LOCK_STALE_AFTER,
            "refresh timeout must fire well before the lock would look stale"
        );
        Ok(())
    }

    #[test]
    fn test_already_fresh_tokens_none_when_access_token_undecodable() {
        // Ein nicht dekodierbares Token darf niemals stillschweigend als
        // "bereits frisch" durchgehen (Fund #1(d)) — sonst würde ein nötiger
        // Refresh übersprungen.
        let document = serde_json::json!({
            "tokens": {
                "access_token": "not-a-jwt",
                "refresh_token": "some-refresh",
            }
        });
        assert!(already_fresh_tokens(&document).is_none());
    }

    #[test]
    fn test_already_fresh_tokens_none_when_expiring_soon() -> TestResult {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(ctx("Systemzeit vor UNIX_EPOCH"))?
            .as_secs() as i64;
        let token = make_jwt_with_exp(now + 30);
        let document = serde_json::json!({
            "tokens": {
                "access_token": token,
                "refresh_token": "some-refresh",
            }
        });
        assert!(already_fresh_tokens(&document).is_none());
        Ok(())
    }

    #[test]
    fn test_already_fresh_tokens_some_when_exp_far_in_future() -> TestResult {
        use secrecy::ExposeSecret as _;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(ctx("Systemzeit vor UNIX_EPOCH"))?
            .as_secs() as i64;
        let token = make_jwt_with_exp(now + 3_600);
        let document = serde_json::json!({
            "tokens": {
                "access_token": token,
                "refresh_token": "still-valid-refresh",
                "account_id": "acct-1",
            }
        });
        let fresh = already_fresh_tokens(&document).ok_or(TestError::Unexpected(
            "expected an already-fresh token".to_owned(),
        ))?;
        assert_eq!(fresh.access_token.expose_secret(), token.as_str());
        assert_eq!(fresh.refresh_token.expose_secret(), "still-valid-refresh");
        assert_eq!(fresh.account_id, Some("acct-1".to_owned()));
        Ok(())
    }

    #[tokio::test]
    async fn test_refresh_codex_tokens_skips_network_call_when_already_fresh() -> TestResult {
        use secrecy::ExposeSecret as _;

        let home = temp_home("already-fresh");
        let _ = std::fs::remove_dir_all(&home);
        let path = home.join("auth.json");
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(ctx("Systemzeit vor UNIX_EPOCH"))?
            .as_secs() as i64;
        let fresh_access_token = make_jwt_with_exp(now + 3_600);
        write_auth_json(&path, &fresh_access_token, "still-valid-refresh")?;

        let client = reqwest::Client::new();
        let started = Instant::now();
        let tokens = refresh_codex_tokens(&client, &path).await;
        let elapsed = started.elapsed();
        let _ = std::fs::remove_dir_all(&home);

        let tokens = tokens.map_err(ctx(
            "ein bereits frisches Token darf den Refresh nicht scheitern lassen",
        ))?;
        assert_eq!(tokens.access_token.expose_secret(), fresh_access_token.as_str());
        assert!(
            elapsed < Duration::from_secs(5),
            "an already-fresh token must short-circuit without a real network round trip"
        );
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
        assert!(!lock_path_for(&auth_path).exists());

        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    #[tokio::test]
    async fn test_refresh_lock_guard_drop_does_not_remove_a_takenover_lock() -> TestResult {
        // Fund #1: der Guard darf beim Drop niemals eine Lock-Datei löschen,
        // die inzwischen ein anderer Prozess übernommen hat (anderer Inhalt =
        // anderer Nonce).
        let home = temp_home("lock-guard-takeover");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).map_err(ctx("Testverzeichnis anlegen"))?;
        let auth_path = home.join("auth.json");
        std::fs::write(&auth_path, "{}").map_err(ctx("auth.json schreiben"))?;

        let guard = acquire_refresh_lock(&auth_path)
            .await
            .map_err(ctx("lock acquired"))?;
        let lock_path = lock_path_for(&auth_path);

        // Simuliert eine Übernahme durch einen anderen Prozess: derselbe
        // Pfad, aber ein frischer, fremder Nonce-Inhalt.
        std::fs::write(&lock_path, "someone-elses-nonce")
            .map_err(ctx("Lock-Datei mit fremdem Nonce überschreiben"))?;

        drop(guard);

        assert!(
            lock_path.exists(),
            "drop must not remove a lock now owned by someone else"
        );
        let content = std::fs::read_to_string(&lock_path).map_err(ctx("Lock-Datei lesen"))?;
        assert_eq!(content, "someone-elses-nonce");

        let _ = std::fs::remove_dir_all(&home);
        Ok(())
    }

    #[test]
    fn test_take_over_stale_lock_is_won_by_exactly_one_racer() -> TestResult {
        // Fund #1: zwei Wartende, die dieselbe verwaiste Lock-Datei sehen,
        // dürfen sie nie beide "erwerben" (das alte `remove_file`-Verhalten
        // erlaubte genau das). `rename` von derselben Quelle aus kann für
        // höchstens einen Aufrufer gelingen.
        let home = temp_home("takeover-race");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).map_err(ctx("Testverzeichnis anlegen"))?;
        let lock_path = home.join(".auth.json.codex-refresh.lock");

        for _ in 0..64 {
            std::fs::write(&lock_path, "orphaned").map_err(ctx("Lock-Datei anlegen"))?;

            let path_a = lock_path.clone();
            let path_b = lock_path.clone();
            let thread_a = std::thread::spawn(move || take_over_stale_lock(&path_a));
            let thread_b = std::thread::spawn(move || take_over_stale_lock(&path_b));

            let won_a = thread_a
                .join()
                .map_err(|_| TestError::Unexpected("racer A panicked".to_owned()))?;
            let won_b = thread_b
                .join()
                .map_err(|_| TestError::Unexpected("racer B panicked".to_owned()))?;

            assert_ne!(
                won_a, won_b,
                "exactly one racer must win the takeover of the same stale lock"
            );
            assert!(
                !lock_path.exists(),
                "after the takeover the original stale lock path must be gone"
            );
        }

        let _ = std::fs::remove_dir_all(&home);
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
