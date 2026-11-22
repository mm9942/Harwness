//! "ChatGPT direkt" im Standalone-Modus: ChatGPT-Abo-Anmeldung auf dem Telefon.
//!
//! Die Kotlin-Seite fuehrt den Browser-/PKCE-Ablauf aus (`ChatgptSignIn.kt`) und
//! uebergibt die Zugangsdaten an [`ChatgptLocal::link`]. Rust legt sie
//!
//! 1. verschluesselt im Android-Keystore ab (Slot [`CREDENTIAL_SLOT`], Master-Kopie),
//! 2. im Format des harw-Plan-Stores als `<home>/secrets/chatgpt-plan.json` (0600) ab,
//!    damit der vorhandene Provider `openai-chatgpt-plan` (`harw-provider-http`) das
//!    Access-Token selbst erneuert, und
//! 3. schreibt die Provider-Route `chatgpt` (ohne Schluessel, Modelle leer).
//!
//! Modelle laden und aktivieren laufen ueber [`crate::ProviderStore`]
//! (`validate_route("chatgpt")`, `activate_route("chatgpt", modell)`).
//! Tokens werden nie geloggt oder in Fehlertexten genannt.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use secrecy::ExposeSecret;
use serde_json::Value;
use zeroize::Zeroizing;

use crate::home::{AppHome, create_private_dir, write_private_atomic};
use crate::provider::ChatgptProviderCredential;
use crate::secrets::{KeystoreSecretResolver, MAX_SECRET_BYTES, PlatformKeystore};
use crate::types::MobileError;

/// Keystore-Slot der ChatGPT-Zugangsdaten (JSON-Datensatz).
pub(crate) const CREDENTIAL_SLOT: &str = "chatgpt-direct";
/// Id der Provider-Route (Dateiname ohne `.toml`).
pub(crate) const ROUTE_ID: &str = "chatgpt";
const ROUTE_API: &str = "openai-chatgpt-plan";
const ROUTE_BASE_URL: &str = "https://api.openai.com/v1";
const CREDENTIAL_FILE: &str = "chatgpt-plan.json";
const MAX_TOKEN_BYTES: usize = 24 * 1024;
const REQUIRED_SCOPES: &[&str] = &["resource.invoke", "chatgpt.tokens.use.direct"];
const OFFLINE_SCOPES: &[&str] = &["openid", "offline_access"];

/// Zustand der lokalen ChatGPT-Anmeldung.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ChatgptLocalStatus {
    /// Ein Konto ist auf diesem Telefon angemeldet.
    pub linked: bool,
    /// OpenAI verlangt eine neue Anmeldung (Refresh-Token abgelehnt).
    pub needs_reauth: bool,
    /// E-Mail des Kontos, falls bekannt.
    pub email: Option<String>,
    /// Registrierter OAuth-Client, fuer eine erneute Anmeldung desselben Kontos.
    pub client_id: Option<String>,
    /// Konto-Kennung (`sub`), fuer die Pruefung bei erneuter Anmeldung.
    pub subject: Option<String>,
    /// Ablauf des Access-Tokens (Unix-Sekunden), 0 wenn unbekannt.
    pub expires_at: u64,
    /// Die Route `chatgpt` ist die aktive lokale Verbindung.
    pub active: bool,
    /// Aktives Modell der Route, sonst leer.
    pub model: String,
}

impl ChatgptLocalStatus {
    fn unlinked() -> Self {
        Self {
            linked: false,
            needs_reauth: false,
            email: None,
            client_id: None,
            subject: None,
            expires_at: 0,
            active: false,
            model: String::new(),
        }
    }
}

/// Lokale ChatGPT-Anmeldung (Keystore, Plan-Store-Datei, Provider-Route).
#[derive(uniffi::Object)]
pub struct ChatgptLocal {
    home: AppHome,
    secrets: KeystoreSecretResolver,
    lock: Mutex<()>,
}

#[uniffi::export]
impl ChatgptLocal {
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
        }))
    }

    /// Liest den Zustand. Gleicht dabei Keystore und Plan-Store-Datei ab: ein vom
    /// Provider erneuertes Token wandert zurueck in den Keystore, eine fehlende
    /// Datei wird aus dem Keystore wiederhergestellt.
    pub fn status(&self) -> Result<ChatgptLocalStatus, MobileError> {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        self.sync()?;
        self.read_status()
    }

    /// Speichert die Zugangsdaten und legt die Route `chatgpt` an.
    pub fn link(
        &self,
        credential: ChatgptProviderCredential,
    ) -> Result<ChatgptLocalStatus, MobileError> {
        let credential = Zeroizing::new(credential);
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        validate(&credential)?;
        let bytes = Zeroizing::new(
            serde_json::to_vec(&*credential)
                .map_err(|_| denied("Die ChatGPT-Zugangsdaten konnten nicht kodiert werden"))?,
        );
        if bytes.len() > MAX_SECRET_BYTES {
            return Err(denied("Die ChatGPT-Zugangsdaten sind zu gross"));
        }
        self.secrets
            .store(CREDENTIAL_SLOT, Zeroizing::new(bytes.to_vec()))
            .map_err(|e| internal(e.diagnostic()))?;
        self.write_plan_file(&bytes)?;
        self.ensure_route()?;
        self.read_status()
    }

    /// Fragt die Modelle des angemeldeten Kontos bei OpenAI ab (hoechstens 200 Ids).
    /// Darf nicht auf der Async-Runtime laufen.
    pub fn list_models(&self) -> Result<Vec<String>, MobileError> {
        if tokio::runtime::Handle::try_current().is_ok() {
            return Err(internal(
                "model discovery must not run on the async runtime",
            ));
        }
        {
            let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
            self.sync()?;
            if !self.read_status()?.linked {
                return Err(denied("ChatGPT ist nicht angemeldet"));
            }
        }
        self.discover_models()
    }

    /// Meldet auf diesem Telefon ab: Keystore, Datei und Route werden entfernt.
    /// Eine Widerrufsanfrage an OpenAI gibt es nicht; das Konto laesst sich in
    /// den ChatGPT-Einstellungen trennen.
    pub fn unlink(&self) -> Result<(), MobileError> {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        self.secrets
            .remove(CREDENTIAL_SLOT)
            .map_err(|e| internal(e.diagnostic()))?;
        for path in [
            self.plan_file(),
            self.plan_file().with_extension("json.tmp"),
        ] {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(internal("could not remove the ChatGPT credential file")),
            }
        }
        match std::fs::remove_file(self.route_file()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(internal("could not remove the ChatGPT route file")),
        }
        self.home.clear_active_route_if(ROUTE_ID)
    }
}

impl ChatgptLocal {
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
            .map_err(|_| internal("could not read the ChatGPT route file"))?;
        let provider: harw_config::ProviderToml =
            toml::from_str(&text).map_err(|_| internal("the ChatGPT route file is invalid"))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| internal("could not start the discovery runtime"))?;
        let models = runtime
            .block_on(harw_provider_http::discovery::list_models_with_home(
                ROUTE_ID,
                &provider,
                None,
                Some(self.home.home()),
            ))
            .map_err(|_| MobileError::Session {
                detail: "ChatGPT-Modelle konnten nicht geladen werden".into(),
            })?;
        let mut seen = HashSet::new();
        Ok(models
            .into_iter()
            .map(|model| model.id)
            .filter(|id| id.chars().count() <= 256 && seen.insert(id.clone()))
            .take(200)
            .collect())
    }

    fn plan_file(&self) -> PathBuf {
        self.home.home().join("secrets").join(CREDENTIAL_FILE)
    }

    fn route_file(&self) -> PathBuf {
        self.home.providers_dir().join(format!("{ROUTE_ID}.toml"))
    }

    fn write_plan_file(&self, bytes: &[u8]) -> Result<(), MobileError> {
        create_private_dir(&self.home.home().join("secrets"))?;
        write_private_atomic(&self.plan_file(), bytes)
    }

    fn ensure_route(&self) -> Result<(), MobileError> {
        let path = self.route_file();
        if path.exists() {
            return Ok(());
        }
        let mut table = toml::Table::new();
        table.insert("name".into(), toml::Value::String(ROUTE_ID.into()));
        table.insert("api".into(), toml::Value::String(ROUTE_API.into()));
        table.insert(
            "base_url".into(),
            toml::Value::String(ROUTE_BASE_URL.into()),
        );
        table.insert("models".into(), toml::Value::Array(Vec::new()));
        let text = toml::to_string(&table).map_err(|_| internal("could not render the route"))?;
        write_private_atomic(&path, text.as_bytes())
    }

    /// Keystore und Plan-Store-Datei abgleichen (Aufrufer haelt das Lock).
    fn sync(&self) -> Result<(), MobileError> {
        let stored = self.secrets.has(CREDENTIAL_SLOT);
        let file = match std::fs::read(self.plan_file()) {
            Ok(bytes) if bytes.len() <= MAX_SECRET_BYTES => Some(Zeroizing::new(bytes)),
            Ok(_) => None,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(_) => return Err(internal("could not read the ChatGPT credential file")),
        };
        match (stored, file) {
            (true, None) => {
                let record = self
                    .secrets
                    .resolve_slot(CREDENTIAL_SLOT)
                    .map_err(|e| internal(e.diagnostic()))?;
                self.write_plan_file(record.expose_secret().as_bytes())?;
                self.ensure_route()?;
            }
            (_, Some(bytes)) => {
                // Nur ein vollstaendiger, gueltiger Datensatz ersetzt die Master-Kopie.
                if parse_doc(&bytes).is_some_and(|doc| doc.linked) {
                    let same = self
                        .secrets
                        .resolve_slot(CREDENTIAL_SLOT)
                        .is_ok_and(|current| current.expose_secret().as_bytes() == trimmed(&bytes));
                    if !same {
                        self.secrets
                            .store(CREDENTIAL_SLOT, Zeroizing::new(trimmed(&bytes).to_vec()))
                            .map_err(|e| internal(e.diagnostic()))?;
                    }
                }
            }
            (false, None) => {}
        }
        Ok(())
    }

    fn read_status(&self) -> Result<ChatgptLocalStatus, MobileError> {
        let Ok(bytes) = std::fs::read(self.plan_file()) else {
            return Ok(ChatgptLocalStatus::unlinked());
        };
        let Some(doc) = parse_doc(&Zeroizing::new(bytes)) else {
            return Ok(ChatgptLocalStatus::unlinked());
        };
        let (active, model) = match self.home.active_route()? {
            Some((route, model)) if route == ROUTE_ID => (true, model),
            _ => (false, String::new()),
        };
        Ok(ChatgptLocalStatus {
            linked: true,
            needs_reauth: !doc.linked,
            email: doc.email,
            client_id: Some(doc.client_id),
            subject: Some(doc.subject),
            expires_at: doc.expires_at,
            active,
            model,
        })
    }
}

/// Nicht geheime Felder der Plan-Store-Datei.
struct PlanDoc {
    /// `false`: nur noch `needs_reauthorization`.
    linked: bool,
    client_id: String,
    subject: String,
    email: Option<String>,
    expires_at: u64,
}

fn trimmed(bytes: &[u8]) -> &[u8] {
    bytes.trim_ascii()
}

fn parse_doc(bytes: &[u8]) -> Option<PlanDoc> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let object = value.as_object()?;
    let client_id = object.get("client_id")?.as_str()?.to_owned();
    let subject = object.get("subject")?.as_str()?.to_owned();
    if object
        .get("needs_reauthorization")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Some(PlanDoc {
            linked: false,
            client_id,
            subject,
            email: None,
            expires_at: 0,
        });
    }
    object.get("access_token")?.as_str()?;
    object.get("refresh_token")?.as_str()?;
    Some(PlanDoc {
        linked: true,
        client_id,
        subject,
        email: object
            .get("email")
            .and_then(Value::as_str)
            .map(str::to_owned),
        expires_at: object
            .get("expires_at")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
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

fn token_ok(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= MAX_TOKEN_BYTES
        && token.bytes().all(|b| b.is_ascii_graphic())
}

/// Gleiche Regeln wie der Plan-Store (`harw-oauth`), damit der Provider die Datei annimmt.
fn validate(credential: &ChatgptProviderCredential) -> Result<(), MobileError> {
    let invalid =
        || denied("Die ChatGPT-Anmeldung lieferte ungueltige oder unzureichende Zugangsdaten");
    let client_ok = credential
        .client_id
        .strip_prefix("oaiapp_")
        .is_some_and(|id| {
            !id.is_empty()
                && id.len() <= 256
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        });
    let scopes: Vec<&str> = credential.scope.split_ascii_whitespace().collect();
    let now = jiff::Timestamp::now().as_second();
    let expires = i64::try_from(credential.expires_at).ok();
    let id_parts = credential.id_token.split('.').count();
    if !client_ok
        || credential.subject.is_empty()
        || credential.subject.len() > 256
        || credential.email.as_ref().is_some_and(|e| e.len() > 512)
        || credential.scope.len() > 1024
        || !REQUIRED_SCOPES
            .iter()
            .chain(OFFLINE_SCOPES)
            .all(|s| scopes.contains(s))
        || ![
            &credential.access_token,
            &credential.refresh_token,
            &credential.id_token,
        ]
        .into_iter()
        .all(|t| token_ok(t))
        || id_parts != 3
        || expires.is_none_or(|e| e <= now)
    {
        return Err(invalid());
    }
    Ok(())
}
