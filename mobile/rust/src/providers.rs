//! Provider routes of the on-device harw home.
//!
//! Each route is one `<providers_dir>/<id>.toml` file that deserialises as
//! `harw_config::ProviderToml`. The API key never touches the file: the file
//! carries `auth = "secrets:route-<id>"` and the encrypted blob lives in the
//! Kotlin keystore slot `route-<id>`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use harw_config::{ProviderToml, SecretRef};
use secrecy::{ExposeSecret, SecretString};
use zeroize::Zeroizing;

use crate::home::{AppHome, LOCAL_ROUTE_ID, write_private_atomic};
use crate::secrets::{KeystoreSecretResolver, PlatformKeystore, SecretError, route_slot};
use crate::types::{
    AppHomeInfo, MobileError, ProviderDefaults, ProviderKind, ProviderRoute, ProviderValidation,
    ValidationOutcome,
};

/// Most routes the app keeps.
pub const MAX_ROUTES: usize = 32;
/// Most model ids returned to the UI.
pub const MAX_LISTED_MODELS: usize = 200;
/// Most distinct model ids counted from one discovery.
pub const MAX_DISCOVERED_MODELS: usize = 2000;
/// Largest secret accepted from the UI, in bytes.
pub const MAX_SECRET_INPUT_BYTES: usize = 8 * 1024;

const MAX_ID_LEN: usize = 32;
const MAX_IMPORT_MODELS: usize = 200;
const MAX_URL_LEN: usize = 512;
const MAX_MODEL_LEN: usize = 256;
const MAX_ROUTE_FILE_BYTES: u64 = 64 * 1024;

const API_ANTHROPIC: &str = "anthropic-messages";
const API_OPENAI_RESPONSES: &str = "openai-responses";
const API_OPENAI_CHAT: &str = "openai-chat";
const API_LOCAL_GGUF: &str = "local-gguf";

/// Suggested defaults for a provider family.
#[uniffi::export]
pub fn provider_defaults(kind: ProviderKind) -> ProviderDefaults {
    let (base_url, api, secret_required, suggested_model) = match kind {
        ProviderKind::Anthropic => ("https://api.anthropic.com/v1", API_ANTHROPIC, true, ""),
        ProviderKind::OpenAi => ("https://api.openai.com/v1", API_OPENAI_RESPONSES, true, ""),
        ProviderKind::DashScope => (
            "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
            API_OPENAI_CHAT,
            true,
            "qwen3-max",
        ),
        ProviderKind::OpenRouter => ("https://openrouter.ai/api/v1", API_OPENAI_CHAT, true, ""),
        ProviderKind::OpenAiCompatible => ("", API_OPENAI_CHAT, false, ""),
        ProviderKind::LocalGguf => ("", API_LOCAL_GGUF, false, ""),
    };
    ProviderDefaults {
        base_url: base_url.to_owned(),
        api: api.to_owned(),
        secret_required,
        suggested_model: suggested_model.to_owned(),
    }
}

/// Makes a downloaded or imported model the active provider ("On this device").
///
/// Fails unless the model file is complete and readable. The next
/// standalone-mode open (`open_local`) loads it.
#[uniffi::export]
pub fn activate_local_model(
    files_dir: String,
    models_dir: String,
    model_id: String,
) -> Result<(), MobileError> {
    let home = AppHome::open(std::path::Path::new(&files_dir))?;
    crate::models::model_path(models_dir.clone(), model_id.clone())?;
    check_model(&model_id, false).map_err(|message| MobileError::Session { detail: message })?;
    home.set_local_models_dir(&models_dir)?;
    home.set_active_route(LOCAL_ROUTE_ID, &model_id)
}

/// Model id when the on-device model is the active provider, else `None`.
#[uniffi::export]
pub fn active_local_model(files_dir: String) -> Result<Option<String>, MobileError> {
    AppHome::open(std::path::Path::new(&files_dir))?.active_local_model()
}

/// Provider routes, their secrets and the active selection.
#[derive(uniffi::Object)]
pub struct ProviderStore {
    home: AppHome,
    secrets: KeystoreSecretResolver,
    write_lock: Mutex<()>,
}

#[uniffi::export]
impl ProviderStore {
    /// Opens (and scaffolds) the app home below `files_dir`.
    #[uniffi::constructor]
    pub fn new(
        files_dir: String,
        keystore: Arc<dyn PlatformKeystore>,
    ) -> Result<Arc<Self>, MobileError> {
        let home = AppHome::open(std::path::Path::new(&files_dir))?;
        Ok(Arc::new(Self::from_parts(home, keystore)))
    }

    /// Paths and active selection of the app home.
    pub fn home_info(&self) -> Result<AppHomeInfo, MobileError> {
        self.home.info()
    }

    /// App-managed routes, sorted by id.
    pub fn routes(&self) -> Result<Vec<ProviderRoute>, MobileError> {
        let active = self.home.active_route()?.map(|(provider, _)| provider);
        let mut routes = Vec::new();
        for (stem, provider) in self.read_all()? {
            let base_url = provider.base_url.clone();
            routes.push(ProviderRoute {
                kind: derive_kind(&provider.api, &base_url),
                default_model: provider.models.first().cloned().unwrap_or_default(),
                has_secret: provider.auth.is_some() && self.secrets.has(&route_slot(&stem)),
                active: active.as_deref() == Some(stem.as_str()),
                base_url,
                id: stem,
            });
        }
        if let Some(model) = self.home.active_local_model()? {
            routes.push(ProviderRoute {
                id: LOCAL_ROUTE_ID.to_owned(),
                kind: ProviderKind::LocalGguf,
                base_url: String::new(),
                default_model: model,
                has_secret: false,
                active: true,
            });
            routes.sort_by(|a, b| a.id.cmp(&b.id));
        }
        Ok(routes)
    }

    /// Validates a route against its endpoint and persists it when usable.
    pub fn save_route(
        &self,
        route: ProviderRoute,
        secret: Vec<u8>,
    ) -> Result<ProviderValidation, MobileError> {
        let secret = Zeroizing::new(secret);
        ensure_outside_runtime()?;
        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let existing = self.read_all()?;
        let prepared = match self.prepare(&route, &secret, &existing) {
            Ok(prepared) => prepared,
            Err(message) => return Ok(invalid_route(message)),
        };
        let mut validation = discover_blocking(
            &prepared.id,
            &prepared.provider,
            prepared.key.as_ref().map(|key| key.expose_secret()),
        )?;
        if persists(validation.outcome) {
            if let Some(bytes) = prepared.new_secret {
                self.secrets
                    .store(&route_slot(&prepared.id), bytes)
                    .map_err(secret_failure)?;
            }
            write_private_atomic(&self.route_path(&prepared.id), prepared.text.as_bytes())?;
            validation.saved = true;
        }
        Ok(validation)
    }

    /// Stores a route copied from a catalog without a secret and without a
    /// network check. Refuses to replace an existing route. `models` are kept
    /// in order (the first is the default); the key is entered later.
    pub fn import_route(
        &self,
        route: ProviderRoute,
        models: Vec<String>,
    ) -> Result<(), MobileError> {
        let session = |detail: &str| MobileError::Session {
            detail: detail.to_owned(),
        };
        let clean = clean_route(&route).map_err(|message| MobileError::Session { detail: message })?;
        if models.len() > MAX_IMPORT_MODELS {
            return Err(session("too many models to import"));
        }
        for model in &models {
            check_model(model, false).map_err(|message| MobileError::Session { detail: message })?;
        }
        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let existing = self.read_all()?;
        if existing.iter().any(|(stem, _)| *stem == clean.id) {
            return Err(session("a provider route with this id already exists"));
        }
        if existing.len() >= MAX_ROUTES {
            return Err(session("too many provider routes"));
        }
        let (text, _) = render_route(&clean, false).map_err(|message| MobileError::Session { detail: message })?;
        let mut table: toml::Table =
            toml::from_str(&text).map_err(|_| internal("the rendered provider route is invalid"))?;
        let mut list: Vec<String> = Vec::new();
        if !clean.model.is_empty() {
            list.push(clean.model.clone());
        }
        for model in models {
            if !list.contains(&model) {
                list.push(model);
            }
        }
        table.insert(
            "models".to_owned(),
            toml::Value::Array(list.into_iter().map(toml::Value::String).collect()),
        );
        let rendered = toml::to_string(&table)
            .map_err(|_| internal("could not render the provider route file"))?;
        toml::from_str::<ProviderToml>(&rendered)
            .map_err(|_| internal("the rendered provider route is invalid"))?;
        write_private_atomic(&self.route_path(&clean.id), rendered.as_bytes())?;
        Ok(())
    }

    /// Re-checks a stored route against its endpoint. Persists nothing.
    pub fn validate_route(&self, route_id: String) -> Result<ProviderValidation, MobileError> {
        ensure_outside_runtime()?;
        let provider = self.load_route(&route_id)?.ok_or_else(unknown_route)?;
        let key = match provider.auth {
            Some(_) => match self.secrets.resolve_slot(&route_slot(&route_id)) {
                Ok(key) => Some(key),
                Err(_) => {
                    return Ok(invalid_route(
                        "the stored API key could not be read".to_owned(),
                    ));
                }
            },
            None => None,
        };
        discover_blocking(
            &route_id,
            &provider,
            key.as_ref().map(|key| key.expose_secret()),
        )
    }

    /// Removes the route file and its secret, and deselects it when active.
    pub fn delete_route(&self, route_id: String) -> Result<(), MobileError> {
        check_id(&route_id).map_err(|_| unknown_route())?;
        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if self.load_route(&route_id)?.is_some() {
            match std::fs::remove_file(self.route_path(&route_id)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(internal("could not remove the provider route file")),
            }
        }
        self.secrets
            .remove(&route_slot(&route_id))
            .map_err(secret_failure)?;
        self.home.clear_active_route_if(&route_id)
    }

    /// Makes a route and model the active selection.
    pub fn activate_route(&self, route_id: String, model: String) -> Result<(), MobileError> {
        check_id(&route_id).map_err(|_| unknown_route())?;
        check_model(&model, false).map_err(|message| MobileError::Session { detail: message })?;
        let _guard = self
            .write_lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if self.load_route(&route_id)?.is_none() {
            return Err(unknown_route());
        }
        let path = self.route_path(&route_id);
        let text = std::fs::read_to_string(&path)
            .map_err(|_| internal("could not read the provider route file"))?;
        let mut table: toml::Table = toml::from_str(&text)
            .map_err(|_| internal("the provider route file is not valid TOML"))?;
        table.insert(
            "models".to_owned(),
            toml::Value::Array(vec![toml::Value::String(model.clone())]),
        );
        let rendered = toml::to_string(&table)
            .map_err(|_| internal("could not render the provider route file"))?;
        toml::from_str::<ProviderToml>(&rendered)
            .map_err(|_| internal("the rendered provider route is invalid"))?;
        write_private_atomic(&path, rendered.as_bytes())?;
        self.home.set_active_route(&route_id, &model)
    }
}

impl ProviderStore {
    pub(crate) fn from_parts(home: AppHome, keystore: Arc<dyn PlatformKeystore>) -> Self {
        Self {
            home,
            secrets: KeystoreSecretResolver::new(keystore),
            write_lock: Mutex::new(()),
        }
    }

    #[cfg(test)]
    pub(crate) fn home(&self) -> &AppHome {
        &self.home
    }

    pub(crate) fn resolver(&self) -> &KeystoreSecretResolver {
        &self.secrets
    }

    fn route_path(&self, id: &str) -> PathBuf {
        self.home.providers_dir().join(format!("{id}.toml"))
    }

    /// Reads one app-managed route file. `None` when absent or foreign.
    fn load_route(&self, id: &str) -> Result<Option<ProviderToml>, MobileError> {
        if check_id(id).is_err() {
            return Ok(None);
        }
        Ok(read_route_file(&self.route_path(id), id))
    }

    /// All app-managed routes as `(stem, config)`, sorted, at most `MAX_ROUTES`.
    fn read_all(&self) -> Result<Vec<(String, ProviderToml)>, MobileError> {
        let dir = self.home.providers_dir();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return Err(internal("could not list the provider routes")),
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .take(MAX_ROUTES * 2)
            .collect();
        paths.sort();
        let mut routes = Vec::new();
        for path in paths {
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
                continue;
            };
            if let Some(provider) = read_route_file(&path, &stem) {
                routes.push((stem, provider));
            }
            if routes.len() >= MAX_ROUTES {
                break;
            }
        }
        Ok(routes)
    }

    /// Validates the UI input and renders the route file. Err is a user message.
    fn prepare(
        &self,
        route: &ProviderRoute,
        secret: &[u8],
        existing: &[(String, ProviderToml)],
    ) -> Result<Prepared, String> {
        let clean = clean_route(route)?;
        let given = clean_secret(secret)?;
        if !existing.iter().any(|(stem, _)| *stem == clean.id) && existing.len() >= MAX_ROUTES {
            return Err("too many provider routes".to_owned());
        }
        let slot = route_slot(&clean.id);
        let (key, new_secret, with_auth) = match given {
            Some(bytes) => {
                let text = std::str::from_utf8(&bytes)
                    .map_err(|_| "the API key must be UTF-8 text".to_owned())?;
                (Some(SecretString::from(text.to_owned())), Some(bytes), true)
            }
            None if self.secrets.has(&slot) => {
                let key = self
                    .secrets
                    .resolve_slot(&slot)
                    .map_err(|e: SecretError| e.diagnostic().to_owned())?;
                (Some(key), None, true)
            }
            None if clean.kind == ProviderKind::OpenAiCompatible => (None, None, false),
            None => return Err("an API key is required for this provider".to_owned()),
        };
        let (text, provider) = render_route(&clean, with_auth)?;
        Ok(Prepared {
            id: clean.id,
            text,
            provider,
            key,
            new_secret,
        })
    }
}

struct Prepared {
    id: String,
    text: String,
    provider: ProviderToml,
    key: Option<SecretString>,
    new_secret: Option<Zeroizing<Vec<u8>>>,
}

struct CleanRoute {
    id: String,
    kind: ProviderKind,
    base_url: String,
    model: String,
}

fn internal(detail: &str) -> MobileError {
    MobileError::Internal {
        detail: detail.to_owned(),
    }
}

fn unknown_route() -> MobileError {
    MobileError::Session {
        detail: "unknown provider route".to_owned(),
    }
}

fn secret_failure(error: SecretError) -> MobileError {
    MobileError::Internal {
        detail: error.diagnostic().to_owned(),
    }
}

fn ensure_outside_runtime() -> Result<(), MobileError> {
    if tokio::runtime::Handle::try_current().is_ok() {
        return Err(internal(
            "provider validation must not run on the async runtime",
        ));
    }
    Ok(())
}

fn invalid_route(detail: String) -> ProviderValidation {
    ProviderValidation {
        outcome: ValidationOutcome::InvalidRoute,
        http_status: None,
        model_count: 0,
        models: Vec::new(),
        detail: Some(detail),
        saved: false,
    }
}

fn outcome_only(
    outcome: ValidationOutcome,
    status: Option<u16>,
    detail: &str,
) -> ProviderValidation {
    ProviderValidation {
        outcome,
        http_status: status,
        model_count: 0,
        models: Vec::new(),
        detail: Some(detail.to_owned()),
        saved: false,
    }
}

fn persists(outcome: ValidationOutcome) -> bool {
    matches!(
        outcome,
        ValidationOutcome::Valid
            | ValidationOutcome::DiscoveryUnsupported
            | ValidationOutcome::NotChecked
    )
}

/// `^[a-z0-9][a-z0-9-]{0,31}$`
fn check_id(id: &str) -> Result<(), String> {
    let bytes = id.as_bytes();
    let ok = match bytes.split_first() {
        Some((first, rest)) => {
            bytes.len() <= MAX_ID_LEN
                && (first.is_ascii_lowercase() || first.is_ascii_digit())
                && rest
                    .iter()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
        }
        None => false,
    };
    if ok {
        Ok(())
    } else {
        Err("the route id must be 1-32 lowercase letters, digits or dashes".to_owned())
    }
}

fn check_model(model: &str, allow_empty: bool) -> Result<(), String> {
    if model.is_empty() {
        return if allow_empty {
            Ok(())
        } else {
            Err("a model is required".to_owned())
        };
    }
    if model.chars().count() > MAX_MODEL_LEN
        || model.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err("the model id must be 1-256 characters without spaces".to_owned());
    }
    Ok(())
}

fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1")
}

fn check_base_url(raw: &str, kind: ProviderKind) -> Result<String, String> {
    let trimmed = raw.trim().trim_end_matches('/');
    let url = if trimmed.is_empty() {
        provider_defaults(kind).base_url
    } else {
        trimmed.to_owned()
    };
    if url.is_empty() {
        return Err("a base URL is required for this provider".to_owned());
    }
    if url.len() > MAX_URL_LEN {
        return Err("the base URL is too long".to_owned());
    }
    if url.contains('#') || url.contains('?') {
        return Err("the base URL must not contain a query or fragment".to_owned());
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("the base URL must not contain spaces".to_owned());
    }
    let uri: http::Uri = url
        .parse()
        .map_err(|_| "the base URL is not a valid URL".to_owned())?;
    let authority = uri
        .authority()
        .ok_or_else(|| "the base URL needs a host".to_owned())?;
    if authority.as_str().contains('@') {
        return Err("the base URL must not contain credentials".to_owned());
    }
    if uri.query().is_some() {
        return Err("the base URL must not contain a query or fragment".to_owned());
    }
    match uri.scheme_str() {
        Some("https") => Ok(url),
        Some("http")
            if kind == ProviderKind::OpenAiCompatible && is_loopback_host(authority.host()) =>
        {
            Ok(url)
        }
        Some("http") => Err("the base URL must use https".to_owned()),
        _ => Err("the base URL must be an https URL".to_owned()),
    }
}

fn clean_route(route: &ProviderRoute) -> Result<CleanRoute, String> {
    if route.kind == ProviderKind::LocalGguf {
        return Err("the on-device model is chosen on the Models screen".to_owned());
    }
    if route.id == LOCAL_ROUTE_ID {
        return Err("this route id is reserved for the on-device model".to_owned());
    }
    check_id(&route.id)?;
    let base_url = check_base_url(&route.base_url, route.kind)?;
    check_model(&route.default_model, true)?;
    Ok(CleanRoute {
        id: route.id.clone(),
        kind: route.kind,
        base_url,
        model: route.default_model.clone(),
    })
}

/// Trims the UI secret. `None` when it is empty.
fn clean_secret(secret: &[u8]) -> Result<Option<Zeroizing<Vec<u8>>>, String> {
    if secret.len() > MAX_SECRET_INPUT_BYTES {
        return Err("the API key is too long".to_owned());
    }
    let text =
        std::str::from_utf8(secret).map_err(|_| "the API key must be UTF-8 text".to_owned())?;
    let trimmed = text.trim();
    if trimmed.chars().any(char::is_control) {
        return Err("the API key must not contain control characters".to_owned());
    }
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(Some(Zeroizing::new(trimmed.as_bytes().to_vec())))
}

fn api_for(kind: ProviderKind) -> String {
    provider_defaults(kind).api
}

fn render_route(clean: &CleanRoute, with_auth: bool) -> Result<(String, ProviderToml), String> {
    let mut table = toml::Table::new();
    table.insert("name".to_owned(), toml::Value::String(clean.id.clone()));
    table.insert("api".to_owned(), toml::Value::String(api_for(clean.kind)));
    table.insert(
        "base_url".to_owned(),
        toml::Value::String(clean.base_url.clone()),
    );
    if with_auth {
        table.insert(
            "auth".to_owned(),
            toml::Value::String(format!("secrets:{}", route_slot(&clean.id))),
        );
    }
    let models = if clean.model.is_empty() {
        Vec::new()
    } else {
        vec![toml::Value::String(clean.model.clone())]
    };
    table.insert("models".to_owned(), toml::Value::Array(models));
    let text =
        toml::to_string(&table).map_err(|_| "could not render the provider route".to_owned())?;
    let provider = toml::from_str::<ProviderToml>(&text)
        .map_err(|_| "the rendered provider route is invalid".to_owned())?;
    Ok((text, provider))
}

/// Parses a route file and keeps it only when the app manages it.
fn read_route_file(path: &std::path::Path, stem: &str) -> Option<ProviderToml> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_ROUTE_FILE_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let provider: ProviderToml = toml::from_str(&text).ok()?;
    let managed = check_id(stem).is_ok()
        && provider.name == stem
        && match &provider.auth {
            None => true,
            Some(SecretRef::Secrets(slot)) => *slot == route_slot(stem),
            Some(_) => false,
        };
    managed.then_some(provider)
}

fn host_of(base_url: &str) -> String {
    base_url
        .parse::<http::Uri>()
        .ok()
        .and_then(|uri| uri.host().map(str::to_ascii_lowercase))
        .unwrap_or_default()
}

fn derive_kind(api: &str, base_url: &str) -> ProviderKind {
    match api {
        API_ANTHROPIC => ProviderKind::Anthropic,
        API_OPENAI_RESPONSES => ProviderKind::OpenAi,
        API_OPENAI_CHAT => {
            let host = host_of(base_url);
            if host.ends_with("openrouter.ai") {
                ProviderKind::OpenRouter
            } else if host.contains("dashscope") {
                ProviderKind::DashScope
            } else {
                ProviderKind::OpenAiCompatible
            }
        }
        _ => ProviderKind::OpenAiCompatible,
    }
}

#[cfg(not(feature = "local-host"))]
fn discover_blocking(
    _id: &str,
    _provider: &ProviderToml,
    _key: Option<&str>,
) -> Result<ProviderValidation, MobileError> {
    Ok(outcome_only(
        ValidationOutcome::NotChecked,
        None,
        "provider validation needs the local-host build",
    ))
}

#[cfg(feature = "local-host")]
fn discover_blocking(
    id: &str,
    provider: &ProviderToml,
    key: Option<&str>,
) -> Result<ProviderValidation, MobileError> {
    ensure_outside_runtime()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| internal("could not start the validation runtime"))?;
    Ok(runtime.block_on(discover(id, provider, key)))
}

#[cfg(feature = "local-host")]
async fn discover(id: &str, provider: &ProviderToml, key: Option<&str>) -> ProviderValidation {
    use std::collections::HashSet;

    use harw_provider_http::discovery::{DiscoveryError, list_models};

    match list_models(id, provider, key).await {
        Ok(models) => {
            let mut seen = HashSet::new();
            let ids: Vec<String> = models
                .into_iter()
                .map(|model| model.id)
                .filter(|model| model.chars().count() <= MAX_MODEL_LEN)
                .filter(|model| seen.insert(model.clone()))
                .take(MAX_DISCOVERED_MODELS)
                .collect();
            ProviderValidation {
                outcome: ValidationOutcome::Valid,
                http_status: None,
                model_count: u32::try_from(ids.len()).unwrap_or(u32::MAX),
                models: ids.into_iter().take(MAX_LISTED_MODELS).collect(),
                detail: None,
                saved: false,
            }
        }
        Err(DiscoveryError::Auth { status, .. }) => outcome_only(
            ValidationOutcome::Rejected,
            Some(status),
            "the provider rejected the API key",
        ),
        Err(DiscoveryError::Api { status, .. }) => {
            let host = host_of(&provider.base_url);
            let official = host == "api.openai.com" || host == "api.anthropic.com";
            if matches!(status, 404 | 405 | 501) && !official {
                outcome_only(
                    ValidationOutcome::DiscoveryUnsupported,
                    Some(status),
                    "the provider does not list its models",
                )
            } else {
                outcome_only(
                    ValidationOutcome::Failed,
                    Some(status),
                    "the provider answered with an error",
                )
            }
        }
        Err(DiscoveryError::Network { .. }) => outcome_only(
            ValidationOutcome::Unreachable,
            None,
            "the provider could not be reached",
        ),
        Err(DiscoveryError::Decode { .. } | DiscoveryError::Unsupported { .. }) => outcome_only(
            ValidationOutcome::Failed,
            None,
            "the provider answered with an unexpected response",
        ),
    }
}

/// Builds the model provider of the active route.
#[cfg(feature = "local-host")]
pub(crate) fn build_model_provider(
    home: &AppHome,
    resolver: &KeystoreSecretResolver,
) -> Result<Arc<dyn harw_core::ModelProvider>, MobileError> {
    match home.active_route()? {
        Some((_, model)) if !model.is_empty() => {}
        _ => {
            return Err(MobileError::Session {
                detail: "no active provider route".to_owned(),
            });
        }
    }
    let layers = harw_home::config_layers_report_at(home.home(), home.workspace())
        .map_err(|e| MobileError::Internal {
            detail: e.to_string(),
        })?
        .layers;
    let config = harw_config::discover_config(&layers).map_err(|e| MobileError::Internal {
        detail: e.to_string(),
    })?;
    let (provider, _load) = harw_provider_http::build_provider_with_load_registry_and_home(
        &config,
        home.home(),
        Some(resolver),
    )
    .map_err(|e| MobileError::Internal {
        detail: e.to_string(),
    })?;
    Ok(Arc::from(provider))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::test_support::MemoryKeystore;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn route(id: &str, kind: ProviderKind, url: &str, model: &str) -> ProviderRoute {
        ProviderRoute {
            id: id.to_owned(),
            kind,
            base_url: url.to_owned(),
            default_model: model.to_owned(),
            has_secret: false,
            active: false,
        }
    }

    type StoreFixture = (tempfile::TempDir, Arc<MemoryKeystore>, Arc<ProviderStore>);

    fn store() -> Result<StoreFixture, Box<dyn std::error::Error + Send + Sync>> {
        let dir = tempfile::tempdir()?;
        let keystore = MemoryKeystore::new();
        let files = dir.path().to_str().ok_or("non-utf8 temp path")?.to_owned();
        let store = ProviderStore::new(files, keystore.clone())?;
        Ok((dir, keystore, store))
    }

    #[test]
    fn id_validation() {
        for ok in ["a", "0", "my-route-1", &"a".repeat(32)] {
            assert!(check_id(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-a", "A", "a_b", "a.b", "a/b", "../x", &"a".repeat(33)] {
            assert!(check_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn url_validation() {
        let compat = ProviderKind::OpenAiCompatible;
        let cases: [(&str, ProviderKind, bool); 12] = [
            ("https://api.openai.com/v1/", ProviderKind::OpenAi, true),
            ("", ProviderKind::Anthropic, true),
            ("", compat, false),
            ("http://127.0.0.1:8080/v1", compat, true),
            ("http://localhost:11434/v1", compat, true),
            ("http://[::1]:8080/v1", compat, true),
            ("http://example.com/v1", compat, false),
            ("http://127.0.0.1/v1", ProviderKind::OpenAi, false),
            ("https://user@example.com/v1", compat, false),
            ("https://example.com/v1?x=1", compat, false),
            ("https://example.com/v1#frag", compat, false),
            ("ftp://example.com/v1", compat, false),
        ];
        for (url, kind, ok) in cases {
            assert_eq!(check_base_url(url, kind).is_ok(), ok, "{url}");
        }
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL_LEN));
        assert!(check_base_url(&long, compat).is_err());
        assert_eq!(
            check_base_url("https://api.openai.com/v1/", ProviderKind::OpenAi)
                .ok()
                .as_deref(),
            Some("https://api.openai.com/v1")
        );
    }

    #[test]
    fn model_validation() {
        assert!(check_model("", true).is_ok());
        assert!(check_model("", false).is_err());
        assert!(check_model("qwen3-max", false).is_ok());
        assert!(check_model("a b", true).is_err());
        assert!(check_model("a\nb", true).is_err());
        assert!(check_model(&"m".repeat(257), true).is_err());
    }

    #[test]
    fn secret_validation() -> TestResult {
        assert!(clean_secret(b"  ").map_err(|e| e.to_string())?.is_none());
        let trimmed = clean_secret(b"  sk-1 \n").map_err(|e| e.to_string())?;
        assert_eq!(trimmed.as_deref().map(Vec::as_slice), Some(&b"sk-1"[..]));
        assert!(clean_secret(b"sk\r\n1").is_err());
        assert!(clean_secret(b"sk\x001").is_err());
        assert!(clean_secret(&[0xff, 0xfe]).is_err());
        assert!(clean_secret(&vec![b'a'; MAX_SECRET_INPUT_BYTES + 1]).is_err());
        Ok(())
    }

    #[test]
    fn rendered_toml_parses_with_secret_ref() -> TestResult {
        let clean = clean_route(&route("my-route", ProviderKind::OpenAi, "", "gpt-x"))
            .map_err(|e| e.to_string())?;
        let (text, provider) = render_route(&clean, true)?;
        let reparsed: ProviderToml = toml::from_str(&text)?;
        assert_eq!(reparsed.name, "my-route");
        assert_eq!(reparsed.api, API_OPENAI_RESPONSES);
        assert_eq!(reparsed.models, vec!["gpt-x".to_owned()]);
        assert!(matches!(
            provider.auth,
            Some(SecretRef::Secrets(ref slot)) if slot == "route-my-route"
        ));
        let (_, without) = render_route(&clean, false)?;
        assert!(without.auth.is_none());
        Ok(())
    }

    #[test]
    fn kind_derivation() {
        assert_eq!(
            derive_kind(API_ANTHROPIC, "https://x.test"),
            ProviderKind::Anthropic
        );
        assert_eq!(
            derive_kind(API_OPENAI_RESPONSES, "https://x.test"),
            ProviderKind::OpenAi
        );
        assert_eq!(
            derive_kind(API_OPENAI_CHAT, "https://openrouter.ai/api/v1"),
            ProviderKind::OpenRouter
        );
        assert_eq!(
            derive_kind(API_OPENAI_CHAT, "https://dashscope-intl.aliyuncs.com/x"),
            ProviderKind::DashScope
        );
        assert_eq!(
            derive_kind(API_OPENAI_CHAT, "http://127.0.0.1:1/v1"),
            ProviderKind::OpenAiCompatible
        );
    }

    #[test]
    fn defaults_table() {
        assert!(
            provider_defaults(ProviderKind::OpenAiCompatible)
                .base_url
                .is_empty()
        );
        assert_eq!(
            provider_defaults(ProviderKind::DashScope).suggested_model,
            "qwen3-max"
        );
        assert!(provider_defaults(ProviderKind::Anthropic).secret_required);
    }

    #[test]
    fn invalid_input_is_reported_not_saved() -> TestResult {
        let (_dir, keystore, store) = store()?;
        let bad = route("Bad Id", ProviderKind::OpenAi, "", "");
        let outcome = store.save_route(bad, b"sk".to_vec())?;
        assert_eq!(outcome.outcome, ValidationOutcome::InvalidRoute);
        assert!(!outcome.saved);
        let no_key = route("ok", ProviderKind::OpenAi, "", "");
        let outcome = store.save_route(no_key, Vec::new())?;
        assert_eq!(outcome.outcome, ValidationOutcome::InvalidRoute);
        assert_eq!(
            outcome.detail.as_deref(),
            Some("an API key is required for this provider")
        );
        assert!(keystore.stored("route-ok").is_none());
        assert!(store.routes()?.is_empty());
        Ok(())
    }

    #[test]
    fn foreign_provider_files_are_ignored() -> TestResult {
        let (_dir, _keystore, store) = store()?;
        let dir = store.home().providers_dir();
        std::fs::create_dir_all(&dir)?;
        let body = "api = \"openai-chat\"\nbase_url = \"https://x.test/v1\"\n";
        std::fs::write(
            dir.join("other.toml"),
            format!("name = \"different\"\n{body}"),
        )?;
        std::fs::write(
            dir.join("envkey.toml"),
            format!("name = \"envkey\"\nauth = \"env:SOME_KEY\"\n{body}"),
        )?;
        std::fs::write(dir.join("notes.txt"), "x")?;
        std::fs::write(
            dir.join("mine.toml"),
            format!("name = \"mine\"\nauth = \"secrets:route-mine\"\n{body}"),
        )?;
        let routes = store.routes()?;
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].id, "mine");
        assert!(!routes[0].has_secret);
        Ok(())
    }

    #[test]
    fn unknown_route_errors() -> TestResult {
        let (_dir, _keystore, store) = store()?;
        assert!(matches!(
            store.activate_route("nope".to_owned(), "m".to_owned()),
            Err(MobileError::Session { .. })
        ));
        assert!(matches!(
            store.validate_route("nope".to_owned()),
            Err(MobileError::Session { .. })
        ));
        assert!(matches!(
            store.delete_route("../x".to_owned()),
            Err(MobileError::Session { .. })
        ));
        Ok(())
    }

    #[cfg(not(feature = "local-host"))]
    #[test]
    fn save_without_feature_is_not_checked_and_saved() -> TestResult {
        let (_dir, keystore, store) = store()?;
        let outcome = store.save_route(
            route("main", ProviderKind::OpenAi, "", "gpt-x"),
            b"sk-test".to_vec(),
        )?;
        assert_eq!(outcome.outcome, ValidationOutcome::NotChecked);
        assert!(outcome.saved);
        assert_eq!(
            keystore.stored("route-main").as_deref(),
            Some(&b"sk-test"[..])
        );
        let routes = store.routes()?;
        assert_eq!(routes.len(), 1);
        assert!(routes[0].has_secret);
        assert_eq!(routes[0].default_model, "gpt-x");
        Ok(())
    }

    #[cfg(not(feature = "local-host"))]
    #[test]
    fn delete_removes_file_slot_and_active() -> TestResult {
        let (_dir, keystore, store) = store()?;
        store.save_route(
            route("main", ProviderKind::OpenAi, "", "gpt-x"),
            b"sk-test".to_vec(),
        )?;
        store.activate_route("main".to_owned(), "gpt-y".to_owned())?;
        assert!(
            store
                .routes()?
                .iter()
                .any(|r| r.active && r.default_model == "gpt-y")
        );
        store.delete_route("main".to_owned())?;
        assert!(store.routes()?.is_empty());
        assert!(keystore.stored("route-main").is_none());
        assert!(store.home().active_route()?.is_none());
        Ok(())
    }

    #[cfg(feature = "local-host")]
    mod network {
        use super::*;
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::mpsc;
        use std::time::Duration;

        /// Serves canned answers; each request text goes to the channel.
        fn serve(
            status: u16,
            body: &'static str,
        ) -> Result<(u16, mpsc::Receiver<String>), std::io::Error> {
            let listener = TcpListener::bind("127.0.0.1:0")?;
            let port = listener.local_addr()?.port();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { break };
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let mut request = Vec::new();
                    let mut chunk = [0u8; 1024];
                    while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                        match stream.read(&mut chunk) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => request.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let reply = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(reply.as_bytes());
                    if tx
                        .send(String::from_utf8_lossy(&request).into_owned())
                        .is_err()
                    {
                        break;
                    }
                }
            });
            Ok((port, rx))
        }

        fn local_route(port: u16, model: &str) -> ProviderRoute {
            route(
                "local",
                ProviderKind::OpenAiCompatible,
                &format!("http://127.0.0.1:{port}/v1"),
                model,
            )
        }

        #[test]
        fn valid_discovery_saves_and_activates() -> TestResult {
            let (port, rx) = serve(
                200,
                r#"{"object":"list","data":[{"id":"m1"},{"id":"m2"},{"id":"m1"}]}"#,
            )?;
            let (_dir, keystore, store) = store()?;
            let outcome = store.save_route(local_route(port, "m1"), b"sk-test".to_vec())?;
            assert_eq!(outcome.outcome, ValidationOutcome::Valid);
            assert_eq!(outcome.model_count, 2);
            assert_eq!(outcome.models, vec!["m1".to_owned(), "m2".to_owned()]);
            assert!(outcome.saved);
            let request = rx
                .recv_timeout(Duration::from_secs(5))?
                .to_ascii_lowercase();
            assert!(request.contains("authorization: bearer sk-test"));
            assert_eq!(
                keystore.stored("route-local").as_deref(),
                Some(&b"sk-test"[..])
            );
            assert_eq!(store.routes()?.len(), 1);
            store.activate_route("local".to_owned(), "m1".to_owned())?;
            let _model = build_model_provider(store.home(), store.resolver())?;
            Ok(())
        }

        #[test]
        fn rejected_key_persists_nothing() -> TestResult {
            let (port, _rx) = serve(401, r#"{"error":"no"}"#)?;
            let (_dir, keystore, store) = store()?;
            let outcome = store.save_route(local_route(port, ""), b"sk-bad".to_vec())?;
            assert_eq!(outcome.outcome, ValidationOutcome::Rejected);
            assert_eq!(outcome.http_status, Some(401));
            assert!(!outcome.saved);
            assert!(store.routes()?.is_empty());
            assert!(keystore.stored("route-local").is_none());
            Ok(())
        }

        #[test]
        fn missing_listing_is_unsupported_but_saved() -> TestResult {
            let (port, _rx) = serve(404, "{}")?;
            let (_dir, _keystore, store) = store()?;
            let outcome = store.save_route(local_route(port, "m1"), b"sk-test".to_vec())?;
            assert_eq!(outcome.outcome, ValidationOutcome::DiscoveryUnsupported);
            assert!(outcome.saved);
            assert_eq!(store.routes()?.len(), 1);
            Ok(())
        }

        #[test]
        fn delete_removes_file_slot_and_active() -> TestResult {
            let (port, _rx) = serve(404, "{}")?;
            let (_dir, keystore, store) = store()?;
            store.save_route(local_route(port, "m1"), b"sk-test".to_vec())?;
            store.activate_route("local".to_owned(), "m1".to_owned())?;
            store.delete_route("local".to_owned())?;
            assert!(store.routes()?.is_empty());
            assert!(keystore.stored("route-local").is_none());
            assert!(store.home().active_route()?.is_none());
            Ok(())
        }
    }
}
