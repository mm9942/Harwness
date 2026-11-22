//! "Codex direkt" im Standalone-Modus: ChatGPT-/Codex-Abo-Anmeldung auf dem Telefon.
//!
//! Gleicher Login wie die Codex-CLI (Client `app_EMoamEEZ73f0CkXaXp7hrann`, PKCE,
//! Loopback `http://localhost:1455/auth/callback`). Kotlin oeffnet die URL aus
//! [`CodexLocal::begin_sign_in`] im Browser, lauscht auf `127.0.0.1:1455` und reicht
//! das Request-Target der Rueckleitung (`/auth/callback?code=..&state=..`) an
//! [`CodexLocal::complete_sign_in`]. Rust tauscht den Code und legt ab:
//!
//! 1. eine verschluesselte Master-Kopie im Android-Keystore (Slot [`CREDENTIAL_SLOT`]),
//! 2. `<files_dir>/.codex/auth.json` (0600, Format der Codex-CLI), die der Provider
//!    `openai-responses` auf `chatgpt.com/backend-api/codex` liest und ueber
//!    `harw-oauth` (`refresh_codex_tokens`) selbst erneuert; er verlangt, dass
//!    `$HOME` = `files_dir` ist (siehe [`CodexLocalStatus::home_ready`]),
//! 3. die Provider-Route `codex` (`file-json:<files_dir>/.codex/auth.json#/tokens/access_token`).
//!
//! Tokens werden nie geloggt oder in Fehlertexten genannt.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use base64::Engine as _;
use secrecy::ExposeSecret;
use serde_json::Value;
use zeroize::Zeroizing;

use crate::home::{AppHome, create_private_dir, write_private_atomic};
use crate::secrets::{KeystoreSecretResolver, MAX_SECRET_BYTES, PlatformKeystore};
use crate::types::MobileError;

/// Keystore-Slot der Codex-Zugangsdaten (auth.json-Inhalt).
pub(crate) const CREDENTIAL_SLOT: &str = "codex-direct";
/// Id der Provider-Route (Dateiname ohne `.toml`).
pub(crate) const ROUTE_ID: &str = "codex";
const ROUTE_API: &str = "openai-responses";
const ROUTE_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const SCOPE: &str = "openid profile email offline_access";
/// Port der Loopback-Rueckleitung (wie die Codex-CLI).
const CALLBACK_PORT: u16 = 1455;
const CALLBACK_PATH: &str = "/auth/callback";
const MAX_TOKEN_BYTES: usize = 24 * 1024;
const MAX_TARGET_BYTES: usize = 8 * 1024;

/// Wie Kotlin die Rueckleitung abfangen soll.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CodexSignInStart {
    /// Authorize-URL fuer den Browser.
    pub url: String,
    /// Loopback-Port (127.0.0.1), auf dem Kotlin auf die Rueckleitung wartet.
    pub port: u16,
    /// Pfad der Rueckleitung.
    pub path: String,
}

/// Zustand der lokalen Codex-Anmeldung.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct CodexLocalStatus {
    /// Ein Konto ist auf diesem Telefon angemeldet.
    pub linked: bool,
    /// E-Mail des Kontos, falls bekannt.
    pub email: Option<String>,
    /// Ablauf des Access-Tokens (Unix-Sekunden), 0 wenn unbekannt.
    pub expires_at: u64,
    /// Die Route `codex` ist die aktive lokale Verbindung.
    pub active: bool,
    /// Aktives Modell der Route, sonst leer.
    pub model: String,
    /// `$HOME` des Prozesses zeigt auf `files_dir` (Voraussetzung des Providers).
    /// Sonst muss Kotlin `HOME` setzen (`Os.setenv`) und den Zustand neu lesen.
    pub home_ready: bool,
}

struct Pending {
    verifier: Zeroizing<String>,
    state: String,
}

/// Lokale Codex-Anmeldung (Keystore, auth.json, Provider-Route).
#[derive(uniffi::Object)]
pub struct CodexLocal {
    home: AppHome,
    secrets: KeystoreSecretResolver,
    lock: Mutex<()>,
    pending: Mutex<Option<Pending>>,
}

#[uniffi::export]
impl CodexLocal {
    /// Oeffnet (und scaffoldet) das App-Home unter `files_dir`.
    #[uniffi::constructor]
    pub fn new(
        files_dir: String,
        keystore: Arc<dyn PlatformKeystore>,
    ) -> Result<Arc<Self>, MobileError> {
        let home = AppHome::open(std::path::Path::new(&files_dir))?;
        Ok(Arc::new(Self {
            home,
            secrets: KeystoreSecretResolver::new(keystore),
            lock: Mutex::new(()),
            pending: Mutex::new(None),
        }))
    }

    /// Liest den Zustand und gleicht Keystore und auth.json ab: ein vom Provider
    /// erneuertes Token wandert in den Keystore, eine fehlende Datei wird aus dem
    /// Keystore wiederhergestellt.
    pub fn status(&self) -> Result<CodexLocalStatus, MobileError> {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        self.sync()?;
        self.read_status()
    }

    /// Beginnt die Anmeldung. Ein frueher begonnener Versuch wird verworfen.
    pub fn begin_sign_in(&self) -> Result<CodexSignInStart, MobileError> {
        let pkce = harw_oauth::generate_pkce();
        let state = harw_oauth::challenge_from_verifier(&format!("codex-state:{}", pkce.verifier));
        let redirect = redirect_uri();
        let url = format!(
            "{AUTHORIZE_URL}?response_type=code&client_id={}&redirect_uri={}&scope={}\
             &code_challenge={}&code_challenge_method=S256&id_token_add_organizations=true\
             &codex_cli_simplified_flow=true&state={}&originator=codex_cli_rs",
            encode(CLIENT_ID),
            encode(&redirect),
            encode(SCOPE),
            encode(&pkce.challenge),
            encode(&state),
        );
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(Pending {
            verifier: Zeroizing::new(pkce.verifier),
            state,
        });
        Ok(CodexSignInStart {
            url,
            port: CALLBACK_PORT,
            path: CALLBACK_PATH.to_owned(),
        })
    }

    /// Schliesst die Anmeldung mit dem Request-Target der Rueckleitung ab
    /// (`/auth/callback?code=..&state=..`). Darf nicht auf der Async-Runtime laufen.
    pub fn complete_sign_in(
        &self,
        callback_target: String,
    ) -> Result<CodexLocalStatus, MobileError> {
        let callback_target = Zeroizing::new(callback_target);
        guard_blocking()?;
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .ok_or_else(|| denied("Es laeuft keine Codex-Anmeldung. Starte sie erneut"))?;
        let (code, state) = parse_callback(&callback_target)?;
        if state != pending.state {
            return Err(denied(
                "Die Codex-Anmeldung passt nicht zum gestarteten Vorgang",
            ));
        }
        let code = Zeroizing::new(code);
        let body = Zeroizing::new(
            runtime()?
                .block_on(exchange(&code, &pending.verifier))
                .map_err(|()| MobileError::Session {
                    detail: "Die Codex-Anmeldung wurde nicht abgeschlossen. Versuche es erneut"
                        .into(),
                })?,
        );
        let document = Zeroizing::new(build_auth_json(&body)?);
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        self.secrets
            .store(CREDENTIAL_SLOT, Zeroizing::new(document.to_vec()))
            .map_err(|e| internal(e.diagnostic()))?;
        self.write_auth_file(&document)?;
        self.write_route()?;
        self.read_status()
    }

    /// Fragt die Modelle des angemeldeten Kontos ab (hoechstens 200 Ids).
    /// Darf nicht auf der Async-Runtime laufen.
    pub fn list_models(&self) -> Result<Vec<String>, MobileError> {
        guard_blocking()?;
        {
            let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
            self.sync()?;
            if !self.read_status()?.linked {
                return Err(denied("Codex ist nicht angemeldet"));
            }
        }
        self.discover_models()
    }

    /// Meldet auf diesem Telefon ab: Keystore, auth.json und Route werden entfernt.
    pub fn unlink(&self) -> Result<(), MobileError> {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.secrets
            .remove(CREDENTIAL_SLOT)
            .map_err(|e| internal(e.diagnostic()))?;
        for path in [
            self.auth_file(),
            self.auth_file().with_extension("json.tmp"),
        ] {
            remove_if_exists(&path, "could not remove the Codex credential file")?;
        }
        remove_if_exists(&self.route_file(), "could not remove the Codex route file")?;
        self.home.clear_active_route_if(ROUTE_ID)
    }
}

impl CodexLocal {
    #[cfg(not(feature = "local-host"))]
    fn discover_models(&self) -> Result<Vec<String>, MobileError> {
        Err(MobileError::Session {
            detail: crate::local::LOCAL_HOST_UNAVAILABLE.into(),
        })
    }

    #[cfg(feature = "local-host")]
    fn discover_models(&self) -> Result<Vec<String>, MobileError> {
        use std::collections::HashSet;

        let text = std::fs::read_to_string(self.route_file())
            .map_err(|_| internal("could not read the Codex route file"))?;
        let provider: harw_config::ProviderToml =
            toml::from_str(&text).map_err(|_| internal("the Codex route file is invalid"))?;
        let models = runtime()?
            .block_on(harw_provider_http::discovery::list_models_with_home(
                ROUTE_ID,
                &provider,
                None,
                Some(self.home.home()),
            ))
            .map_err(|_| MobileError::Session {
                detail: "Codex-Modelle konnten nicht geladen werden".into(),
            })?;
        let mut seen = HashSet::new();
        Ok(models
            .into_iter()
            .map(|model| model.id)
            .filter(|id| id.chars().count() <= 256 && seen.insert(id.clone()))
            .take(200)
            .collect())
    }

    fn codex_dir(&self) -> PathBuf {
        self.home.files_dir().join(".codex")
    }

    fn auth_file(&self) -> PathBuf {
        self.codex_dir().join("auth.json")
    }

    fn route_file(&self) -> PathBuf {
        self.home.providers_dir().join(format!("{ROUTE_ID}.toml"))
    }

    fn write_auth_file(&self, bytes: &[u8]) -> Result<(), MobileError> {
        create_private_dir(&self.codex_dir())?;
        write_private_atomic(&self.auth_file(), bytes)
    }

    fn write_route(&self) -> Result<(), MobileError> {
        let path = self.route_file();
        if path.exists() {
            return Ok(());
        }
        let auth = format!(
            "file-json:{}#/tokens/access_token",
            self.auth_file().to_string_lossy()
        );
        let mut table = toml::Table::new();
        table.insert("name".into(), toml::Value::String(ROUTE_ID.into()));
        table.insert("api".into(), toml::Value::String(ROUTE_API.into()));
        table.insert(
            "base_url".into(),
            toml::Value::String(ROUTE_BASE_URL.into()),
        );
        table.insert("auth".into(), toml::Value::String(auth));
        table.insert("models".into(), toml::Value::Array(Vec::new()));
        let text = toml::to_string(&table).map_err(|_| internal("could not render the route"))?;
        write_private_atomic(&path, text.as_bytes())
    }

    /// Keystore und auth.json abgleichen (Aufrufer haelt das Lock).
    fn sync(&self) -> Result<(), MobileError> {
        let stored = self.secrets.has(CREDENTIAL_SLOT);
        let file = match std::fs::read(self.auth_file()) {
            Ok(bytes) if bytes.len() <= MAX_SECRET_BYTES => Some(Zeroizing::new(bytes)),
            Ok(_) => None,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(internal("could not read the Codex credential file")),
        };
        match (stored, file) {
            (true, None) => {
                let record = self
                    .secrets
                    .resolve_slot(CREDENTIAL_SLOT)
                    .map_err(|e| internal(e.diagnostic()))?;
                self.write_auth_file(record.expose_secret().as_bytes())?;
                self.write_route()?;
            }
            (_, Some(bytes)) => {
                if parse_doc(&bytes).is_some() {
                    let trimmed = bytes.trim_ascii();
                    let same = self
                        .secrets
                        .resolve_slot(CREDENTIAL_SLOT)
                        .is_ok_and(|current| current.expose_secret().as_bytes() == trimmed);
                    if !same {
                        self.secrets
                            .store(CREDENTIAL_SLOT, Zeroizing::new(trimmed.to_vec()))
                            .map_err(|e| internal(e.diagnostic()))?;
                    }
                }
            }
            (false, None) => {}
        }
        Ok(())
    }

    fn read_status(&self) -> Result<CodexLocalStatus, MobileError> {
        let home_ready = std::env::var_os("HOME")
            .is_some_and(|home| std::path::Path::new(&home) == self.home.files_dir());
        let doc = std::fs::read(self.auth_file())
            .ok()
            .map(Zeroizing::new)
            .and_then(|bytes| parse_doc(&bytes));
        let Some(doc) = doc else {
            return Ok(CodexLocalStatus {
                linked: false,
                email: None,
                expires_at: 0,
                active: false,
                model: String::new(),
                home_ready,
            });
        };
        let (active, model) = match self.home.active_route()? {
            Some((route, model)) if route == ROUTE_ID => (true, model),
            _ => (false, String::new()),
        };
        Ok(CodexLocalStatus {
            linked: true,
            email: doc.email,
            expires_at: doc.expires_at,
            active,
            model,
            home_ready,
        })
    }
}

/// Nicht geheime Felder der auth.json.
struct AuthDoc {
    email: Option<String>,
    expires_at: u64,
}

fn parse_doc(bytes: &[u8]) -> Option<AuthDoc> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let tokens = value.get("tokens")?;
    let access = tokens.get("access_token")?.as_str()?;
    tokens.get("refresh_token")?.as_str()?;
    let id = tokens.get("id_token")?.as_str()?;
    Some(AuthDoc {
        email: jwt_payload(id)
            .and_then(|p| p.get("email").and_then(Value::as_str).map(str::to_owned)),
        expires_at: harw_oauth::jwt_exp_unix_seconds(access)
            .and_then(|e| u64::try_from(e).ok())
            .unwrap_or(0),
    })
}

fn jwt_payload(token: &str) -> Option<Value> {
    let part = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(part.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Tauscht den Code beim Codex-Token-Endpunkt; liefert den Antwort-Body.
/// Fehler tragen bewusst keine Details (Body kann Tokens enthalten).
async fn exchange(code: &str, verifier: &str) -> Result<String, ()> {
    let client = harw_oauth::oidc_login::login_client().map_err(|_| ())?;
    let redirect = redirect_uri();
    let response = client
        .post(TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect.as_str()),
            ("client_id", CLIENT_ID),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .map_err(|_| ())?;
    if !response.status().is_success() {
        return Err(());
    }
    response.text().await.map_err(|_| ())
}

/// Baut die auth.json (Codex-CLI-Format) aus der Token-Antwort.
fn build_auth_json(body: &str) -> Result<Vec<u8>, MobileError> {
    let invalid = || denied("Die Codex-Anmeldung lieferte unvollstaendige Zugangsdaten");
    let value: Value = serde_json::from_str(body).map_err(|_| invalid())?;
    let field = |name: &str| {
        value
            .get(name)
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty() && t.len() <= MAX_TOKEN_BYTES)
            .filter(|t| t.bytes().all(|b| b.is_ascii_graphic()))
            .map(str::to_owned)
    };
    let (Some(id), Some(access), Some(refresh)) = (
        field("id_token"),
        field("access_token"),
        field("refresh_token"),
    ) else {
        return Err(invalid());
    };
    if id.split('.').count() != 3 {
        return Err(invalid());
    }
    let account_id = jwt_payload(&id).and_then(|p| {
        p.pointer("/https:~1~1api.openai.com~1auth/chatgpt_account_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    let document = serde_json::json!({
        "auth_mode": "chatgpt",
        "OPENAI_API_KEY": Value::Null,
        "tokens": {
            "id_token": id,
            "access_token": access,
            "refresh_token": refresh,
            "account_id": account_id,
        },
        "last_refresh": jiff::Timestamp::now().to_string(),
    });
    serde_json::to_vec(&document).map_err(|_| invalid())
}

fn redirect_uri() -> String {
    format!("http://localhost:{CALLBACK_PORT}{CALLBACK_PATH}")
}

/// Zerlegt `/auth/callback?code=..&state=..` in `(code, state)`.
fn parse_callback(target: &str) -> Result<(String, String), MobileError> {
    let bad = || denied("Die Codex-Rueckleitung ist ungueltig oder wurde abgelehnt");
    if target.len() > MAX_TARGET_BYTES {
        return Err(bad());
    }
    let (path, query) = target.split_once('?').ok_or_else(bad)?;
    if path != CALLBACK_PATH {
        return Err(bad());
    }
    let (mut code, mut state) = (None, None);
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match key {
            "code" => code = Some(decode(value).ok_or_else(bad)?),
            "state" => state = Some(decode(value).ok_or_else(bad)?),
            _ => {}
        }
    }
    match (code, state) {
        (Some(code), Some(state)) if !code.is_empty() => Ok((code, state)),
        _ => Err(bad()),
    }
}

fn decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

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

fn remove_if_exists(path: &std::path::Path, detail: &str) -> Result<(), MobileError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(internal(detail)),
    }
}

fn guard_blocking() -> Result<(), MobileError> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(internal("Codex sign-in must not run on the async runtime"));
    }
    Ok(())
}

fn runtime() -> Result<tokio::runtime::Runtime, MobileError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| internal("could not start the sign-in runtime"))
}

fn denied(detail: &str) -> MobileError {
    MobileError::Denied {
        detail: detail.to_owned(),
    }
}

fn internal(detail: &str) -> MobileError {
    MobileError::Internal {
        detail: detail.to_owned(),
    }
}
