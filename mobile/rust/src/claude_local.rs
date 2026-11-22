//! "Claude direkt" im Standalone-Modus: Claude-Abo-Anmeldung auf dem Telefon.
//!
//! Ablauf (wie `harw auth` im CLI, Paste-Variante des Anthropic-PKCE-Flows):
//!
//! 1. [`ClaudeLocal::begin_sign_in`] erzeugt PKCE-Paar und State (nur im Speicher)
//!    und liefert die Authorize-URL; Kotlin oeffnet sie im Browser.
//! 2. Der Nutzer kopiert den angezeigten Code (`code#state`) zurueck in die App,
//!    [`ClaudeLocal::complete_sign_in`] tauscht ihn ueber `harw-oauth` gegen das
//!    Setup-/OAuth-Token (`sk-ant-oat...`).
//! 3. Alternativ nimmt [`ClaudeLocal::link_token`] ein von `claude setup-token`
//!    erzeugtes Token direkt entgegen.
//!
//! Das Token liegt ausschliesslich im Android-Keystore (Slot `route-claude`, wie
//! jedes Routen-Secret); die Route `claude` (`auth = "secrets:route-claude"`)
//! verweist nur darauf. Es gibt keine Klartextdatei. Modelle laden/aktivieren
//! laufen wie bei ChatGPT ueber [`ClaudeLocal::list_models`] und
//! `ProviderStore::activate_route("claude", modell)`. Tokens werden nie geloggt.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use secrecy::ExposeSecret;
use zeroize::Zeroizing;

use crate::home::{AppHome, write_private_atomic};
use crate::secrets::{KeystoreSecretResolver, PlatformKeystore, route_slot};
use crate::types::MobileError;

/// Id der Provider-Route (Dateiname ohne `.toml`).
pub(crate) const ROUTE_ID: &str = "claude";
const ROUTE_API: &str = "anthropic-messages";
const ROUTE_BASE_URL: &str = "https://api.anthropic.com/v1";
const MAX_TOKEN_BYTES: usize = 8 * 1024;
const MAX_PASTE_BYTES: usize = 4 * 1024;

/// Zustand der lokalen Claude-Anmeldung.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ClaudeLocalStatus {
    /// Ein Token ist auf diesem Telefon gespeichert.
    pub linked: bool,
    /// Die Route `claude` ist die aktive lokale Verbindung.
    pub active: bool,
    /// Aktives Modell der Route, sonst leer.
    pub model: String,
    /// Eine Anmeldung wurde begonnen und wartet auf den eingefuegten Code.
    pub sign_in_pending: bool,
}

struct Pending {
    verifier: Zeroizing<String>,
    state: String,
}

/// Lokale Claude-Anmeldung (Keystore, Provider-Route).
#[derive(uniffi::Object)]
pub struct ClaudeLocal {
    home: AppHome,
    secrets: KeystoreSecretResolver,
    lock: Mutex<()>,
    pending: Mutex<Option<Pending>>,
}

#[uniffi::export]
impl ClaudeLocal {
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

    /// Liest den Zustand.
    pub fn status(&self) -> Result<ClaudeLocalStatus, MobileError> {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        self.read_status()
    }

    /// Beginnt die Anmeldung und liefert die Authorize-URL fuer den Browser.
    /// Ein frueher begonnener Versuch wird verworfen.
    pub fn begin_sign_in(&self) -> Result<String, MobileError> {
        let pkce = harw_oauth::generate_pkce();
        let state = harw_oauth::challenge_from_verifier(&format!("state:{}", pkce.verifier));
        let url = harw_oauth::authorize_url(&pkce.challenge, &state);
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(Pending {
            verifier: Zeroizing::new(pkce.verifier),
            state,
        });
        Ok(url)
    }

    /// Schliesst die Anmeldung mit dem eingefuegten Code (`code#state`) ab.
    /// Darf nicht auf der Async-Runtime laufen.
    pub fn complete_sign_in(&self, pasted: String) -> Result<ClaudeLocalStatus, MobileError> {
        let pasted = Zeroizing::new(pasted);
        guard_blocking()?;
        if pasted.trim().is_empty() || pasted.len() > MAX_PASTE_BYTES {
            return Err(denied("Der eingefuegte Code ist leer oder ungueltig"));
        }
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .ok_or_else(|| denied("Es laeuft keine Claude-Anmeldung. Starte sie erneut"))?;
        let (code, supplied_state) = harw_oauth::split_callback(pasted.trim());
        let code = Zeroizing::new(code);
        let runtime = runtime()?;
        let token = runtime
            .block_on(async {
                let client = harw_oauth::oidc_login::login_client()?;
                harw_oauth::exchange_code(
                    &client,
                    &code,
                    &supplied_state,
                    &pending.state,
                    &pending.verifier,
                )
                .await
            })
            .map_err(|_| MobileError::Session {
                detail: "Die Claude-Anmeldung wurde nicht abgeschlossen. Versuche es erneut".into(),
            })?;
        self.link_secret(token.expose_secret())
    }

    /// Speichert ein per `claude setup-token` erzeugtes Token (`sk-ant-oat...`).
    pub fn link_token(&self, token: String) -> Result<ClaudeLocalStatus, MobileError> {
        let token = Zeroizing::new(token);
        self.link_secret(token.trim())
    }

    /// Fragt die Modelle bei Anthropic ab (hoechstens 200 Ids).
    /// Darf nicht auf der Async-Runtime laufen.
    pub fn list_models(&self) -> Result<Vec<String>, MobileError> {
        guard_blocking()?;
        let token = {
            let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
            self.secrets
                .resolve_slot(&route_slot(ROUTE_ID))
                .map_err(|_| denied("Claude ist nicht angemeldet"))?
        };
        self.discover_models(token.expose_secret())
    }

    /// Meldet auf diesem Telefon ab: Keystore-Eintrag und Route werden entfernt.
    pub fn unlink(&self) -> Result<(), MobileError> {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        *self.pending.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.secrets
            .remove(&route_slot(ROUTE_ID))
            .map_err(|e| internal(e.diagnostic()))?;
        match std::fs::remove_file(self.route_file()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(internal("could not remove the Claude route file")),
        }
        self.home.clear_active_route_if(ROUTE_ID)
    }
}

impl ClaudeLocal {
    fn link_secret(&self, token: &str) -> Result<ClaudeLocalStatus, MobileError> {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        if !token.starts_with("sk-ant-oat")
            || token.len() > MAX_TOKEN_BYTES
            || !token.bytes().all(|b| b.is_ascii_graphic())
        {
            return Err(denied(
                "Das Token ist kein Claude-Abo-Token (erwartet: sk-ant-oat...)",
            ));
        }
        self.secrets
            .store(&route_slot(ROUTE_ID), Zeroizing::new(token.as_bytes().to_vec()))
            .map_err(|e| internal(e.diagnostic()))?;
        self.write_route()?;
        self.read_status()
    }

    #[cfg(not(feature = "local-host"))]
    fn discover_models(&self, _token: &str) -> Result<Vec<String>, MobileError> {
        Err(MobileError::Session {
            detail: crate::local::LOCAL_HOST_UNAVAILABLE.into(),
        })
    }

    #[cfg(feature = "local-host")]
    fn discover_models(&self, token: &str) -> Result<Vec<String>, MobileError> {
        use std::collections::HashSet;

        let text = std::fs::read_to_string(self.route_file())
            .map_err(|_| internal("could not read the Claude route file"))?;
        let provider: harw_config::ProviderToml =
            toml::from_str(&text).map_err(|_| internal("the Claude route file is invalid"))?;
        let models = runtime()?
            .block_on(harw_provider_http::discovery::list_models_with_home(
                ROUTE_ID,
                &provider,
                Some(token),
                Some(self.home.home()),
            ))
            .map_err(|_| MobileError::Session {
                detail: "Claude-Modelle konnten nicht geladen werden".into(),
            })?;
        let mut seen = HashSet::new();
        Ok(models
            .into_iter()
            .map(|model| model.id)
            .filter(|id| id.chars().count() <= 256 && seen.insert(id.clone()))
            .take(200)
            .collect())
    }

    fn route_file(&self) -> PathBuf {
        self.home.providers_dir().join(format!("{ROUTE_ID}.toml"))
    }

    fn write_route(&self) -> Result<(), MobileError> {
        let path = self.route_file();
        if path.exists() {
            return Ok(());
        }
        let mut table = toml::Table::new();
        table.insert("name".into(), toml::Value::String(ROUTE_ID.into()));
        table.insert("api".into(), toml::Value::String(ROUTE_API.into()));
        table.insert("base_url".into(), toml::Value::String(ROUTE_BASE_URL.into()));
        table.insert(
            "auth".into(),
            toml::Value::String(format!("secrets:{}", route_slot(ROUTE_ID))),
        );
        table.insert("models".into(), toml::Value::Array(Vec::new()));
        let text = toml::to_string(&table).map_err(|_| internal("could not render the route"))?;
        write_private_atomic(&path, text.as_bytes())
    }

    fn read_status(&self) -> Result<ClaudeLocalStatus, MobileError> {
        let linked = self.secrets.has(&route_slot(ROUTE_ID));
        let (active, model) = match self.home.active_route()? {
            Some((route, model)) if linked && route == ROUTE_ID => (true, model),
            _ => (false, String::new()),
        };
        let sign_in_pending = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some();
        Ok(ClaudeLocalStatus {
            linked,
            active,
            model,
            sign_in_pending,
        })
    }
}

fn guard_blocking() -> Result<(), MobileError> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(internal("Claude sign-in must not run on the async runtime"));
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
