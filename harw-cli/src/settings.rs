//! `harw settings` — Provider, Standardmodell, Freigaben und einzelne
//! Konfigurationswerte verwalten (Contract
//! `docs/design/config-scopes.md` §2).
//!
//! # Verantwortung
//! Dieses Modul führt die Unterbefehle von [`crate::cli::SettingsAction`]
//! aus und trägt das zeilenbasierte interaktive Menü für `harw settings`
//! ohne Unterbefehl. Es schreibt **ausschließlich** über
//! [`harw_config::ConfigWriter`] (Kommentare bleiben erhalten, `.bak.<n>`,
//! atomar) — mit Ausnahme von Provider-Dateien (`providers/<name>.toml`),
//! die als ganze [`harw_config::ProviderToml`]-Dokumente per `serde`
//! neu geschrieben werden, exakt wie im Onboarding-Wizard
//! (`crate::onboarding::persist_outcome`). Die dortigen Hilfsfunktionen
//! (`validate_provider_name`, das atomare Schreiben) sind **hier minimal
//! nachgebaut**, nicht wiederverwendet: `onboarding.rs` gehört einem anderen
//! Slice und wurde für diesen Auftrag nicht angefasst.
//!
//! # Scopes
//! `get`/`set`/`permissions` respektieren `--global` (Vorgabe,
//! `~/.harw/profiles/<p>/config.toml`) und `--project`
//! (`~/.harw/profiles/<p>/projects/<key>/settings.toml`, Projekt-Erkennung
//! über [`harw_home::discover_project`] ab dem aktuellen Arbeitsverzeichnis).
//! `provider`/`model default` wirken immer auf die globale Ebene (Profil),
//! wie der Onboarding-Wizard.
//!
//! # Löschen
//! `harw settings set <key>` **ohne** Wert löscht den punktgetrennten
//! Schlüssel aus der Ziel-Ebene (siehe [`unset_value`]). Tabellen auf dem
//! Pfad, die dadurch leer werden, fallen ebenfalls weg; alle übrigen
//! Kommentare und Formatierungen bleiben erhalten. Ein nicht gesetzter
//! Schlüssel ist kein Fehler — es wird nur ein Hinweis gedruckt und nichts
//! geschrieben.
//!
//! # Secrets
//! `--auth` akzeptiert ausschließlich eine [`harw_config::SecretRef`]
//! (`env:VAR`, `secrets:NAME`, …); ein Klartext-Wert wird abgelehnt und
//! verweist auf `harw auth`.
//!
//! # Validierung
//! Nach jedem Schreiben lädt [`print_validation_result`] die betroffene
//! Config-Kette neu (`harw_home::config_layers` +
//! `harw_config::discover_config` + `ResolvedConfig::validate`) und druckt ein knappes Ergebnis — im Stil
//! von `harw doctor` (`crate::doctor`), aber nicht fehlschlagend: die Datei
//! ist zu diesem Zeitpunkt bereits geschrieben.
//!
//! # Concurrency
//! Zustandslos; ein `harw settings`-Prozess pro Root-Space ist die
//! erwartete Nutzung. Keine eigene Synchronisation gegen parallele
//! Schreiber (wie `ConfigWriter` selbst).

use std::error::Error as StdError;
use std::fmt;
use std::io::{IsTerminal as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use toml_edit::{DocumentMut, Item, Table, value};

use harw_config::{ConfigWriter, ProviderToml, RuleKind, RuleToml, SecretRef, SettingScope};

use crate::cli::{
    SettingsAction, SettingsModelAction, SettingsPermissionsAction, SettingsProviderAction,
};

/// Monotoner Zähler für kollisionsfreie Temp-Dateinamen innerhalb dieses
/// Prozesses (Uniqueness kommt letztlich von `create_new`).
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Alle Fehler, die `harw settings` auslösen kann.
///
/// Handgeschrieben nach Projektkonvention (kein `anyhow`/`thiserror`):
/// `Display` ist die einzige menschenlesbare Quelle, `Debug` delegiert an
/// `Display`, `std::error::Error::source` verlinkt die gewrappte Ursache.
pub enum SettingsError {
    /// Root-Space-Auflösung oder Projekt-Erkennung schlug fehl.
    Home(harw_home::HomeError),
    /// Laden, Schreiben oder Validieren einer Config-Datei schlug fehl.
    Config(harw_config::ConfigError),
    /// Ein Dateisystemzugriff schlug fehl; `path` benennt das Ziel.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Eine Config- oder Provider-Datei ist kein gültiges bzw. serialisierbares TOML.
    Toml { path: PathBuf, reason: String },
    /// Ein Provider-Name enthält unzulässige Zeichen.
    InvalidProviderName { name: String },
    /// Der angegebene Provider existiert nicht.
    ProviderNotFound { name: String },
    /// Die angegebene API wird von keinem bekannten Adapter unterstützt.
    UnsupportedApi { api: String },
    /// Die Basis-URL ist keine gültige `http(s)`-Adresse.
    InvalidBaseUrl(String),
    /// `--auth` war ein Klartext-Wert statt einer Secret-Referenz.
    PlaintextAuthRejected { name: String },
    /// Runde 7, Teil L1: `--auth` zusammen mit `--no-auth` bzw.
    /// `--auth-header none`.
    ConflictingAuth { name: String },
    /// Ein ungültiger Freigabemodus wurde übergeben.
    InvalidMode { mode: String },
    /// Ein Regel-Index lag außerhalb der aktuellen Liste.
    InvalidRuleIndex { index: usize, len: usize },
    /// Der interaktive Schritt wurde vom Nutzer abgebrochen.
    Aborted,
}

impl fmt::Display for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Home(source) => write!(f, "{source}"),
            Self::Config(source) => write!(f, "{source}"),
            Self::Io { path, source } => {
                write!(
                    f,
                    "dateizugriff auf {} fehlgeschlagen: {source}",
                    path.display()
                )
            }
            Self::Toml { path, reason } => {
                write!(f, "toml-dokument {} ist ungültig: {reason}", path.display())
            }
            Self::InvalidProviderName { name } => write!(
                f,
                "ungültiger Provider-Name {name:?}: nur ASCII-Buchstaben, Ziffern, '-' und '_' sind erlaubt"
            ),
            Self::ProviderNotFound { name } => {
                write!(f, "Provider {name:?} ist nicht konfiguriert")
            }
            Self::UnsupportedApi { api } => write!(
                f,
                "nicht unterstützte Provider-API {api:?} (erwartet: openai-chat, openai-responses, anthropic-messages, ollama)"
            ),
            Self::InvalidBaseUrl(reason) => write!(f, "ungültige Basis-URL: {reason}"),
            Self::PlaintextAuthRejected { name } => write!(
                f,
                "--auth für Provider {name:?} muss eine Secret-Referenz sein (env:VAR, secrets:NAME, …), kein Klartext-Schlüssel; benutze `harw auth`, um Credentials sicher abzulegen"
            ),
            Self::ConflictingAuth { name } => write!(
                f,
                "Provider {name:?}: `--auth` widerspricht `--no-auth` bzw. `--auth-header none`; entweder einen Schlüssel angeben oder keinen"
            ),
            Self::InvalidMode { mode } => write!(
                f,
                "ungültiger Freigabemodus {mode:?}, erwartet ask|auto|full"
            ),
            Self::InvalidRuleIndex { index, len } => write!(
                f,
                "Regel-Index {index} liegt außerhalb der aktuellen Liste (Länge {len})"
            ),
            Self::Aborted => write!(f, "abgebrochen"),
        }
    }
}

impl fmt::Debug for SettingsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

impl StdError for SettingsError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Home(source) => Some(source),
            Self::Config(source) => Some(source),
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<harw_home::HomeError> for SettingsError {
    fn from(source: harw_home::HomeError) -> Self {
        Self::Home(source)
    }
}

impl From<harw_config::ConfigError> for SettingsError {
    fn from(source: harw_config::ConfigError) -> Self {
        Self::Config(source)
    }
}

/// Führt `harw settings [action]` aus.
///
/// # Arguments
/// - `home_override` (`Option<PathBuf>`): expliziter `--home`-Wert; siehe
///   [`crate::home::resolve_home`].
/// - `action` (`Option<SettingsAction>`): Unterbefehl, oder `None` für das
///   interaktive Menü.
///
/// # Errors
/// `String` mit menschenlesbarem Kontext — konsistent mit den übrigen
/// `harw-cli`-Subcommand-Läufern (siehe `crate::project_trust::run`).
pub fn run(home_override: Option<PathBuf>, action: Option<SettingsAction>) -> Result<(), String> {
    let home = crate::home::resolve_home(home_override)?;
    crate::home::ensure_home(&home).map_err(|error| error.to_string())?;
    execute(&home, action).map_err(|error| error.to_string())
}

/// Testbarer Kern von [`run`]: nimmt einen bereits aufgelösten Root-Space.
fn execute(home: &Path, action: Option<SettingsAction>) -> Result<(), SettingsError> {
    match action {
        None => run_interactive(home),
        Some(SettingsAction::Provider { action }) => run_provider(home, action),
        Some(SettingsAction::Model { action }) => run_model(home, action),
        Some(SettingsAction::Permissions { action }) => run_permissions(home, action),
        Some(SettingsAction::Get { key, scope }) => run_get(home, &key, scope.resolve()),
        Some(SettingsAction::Set { key, value, scope }) => {
            run_set(home, &key, value, scope.resolve())
        }
    }
}

// ---------------------------------------------------------------------
// Pfadauflösung
// ---------------------------------------------------------------------

/// `~/.harw/profiles/<aktives Profil>`.
fn active_profile_dir(home: &Path) -> Result<PathBuf, SettingsError> {
    let profile_name = harw_home::active_profile_name(home);
    Ok(harw_home::profile_dir(home, &profile_name)?)
}

/// `~/.harw/profiles/<p>/config.toml` (globale Ebene).
fn global_config_path(home: &Path) -> Result<PathBuf, SettingsError> {
    Ok(active_profile_dir(home)?.join("config.toml"))
}

/// `~/.harw/profiles/<p>/projects/<key>/settings.toml` (Projekt-Ebene),
/// Projekt-Erkennung ab dem aktuellen Arbeitsverzeichnis.
///
/// # Errors
/// [`SettingsError::Io`], wenn das Arbeitsverzeichnis nicht ermittelbar ist;
/// [`SettingsError::Home`], wenn Projekt-Erkennung oder Pfadauflösung
/// scheitern (siehe [`harw_home::discover_project`],
/// [`harw_home::project_settings_dir`]).
fn project_settings_path(home: &Path) -> Result<PathBuf, SettingsError> {
    let cwd = std::env::current_dir().map_err(|source| SettingsError::Io {
        path: PathBuf::from("."),
        source,
    })?;
    let project = harw_home::discover_project(&cwd, &[])?;
    let profile_name = harw_home::active_profile_name(home);
    let key = harw_home::project_key(&project.root);
    let dir = harw_home::project_settings_dir(home, &profile_name, &key)?;
    Ok(dir.join("settings.toml"))
}

/// Löst `scope` in den zu bearbeitenden Config-Pfad auf.
fn config_path_for_scope(home: &Path, scope: SettingScope) -> Result<PathBuf, SettingsError> {
    match scope {
        SettingScope::Global => global_config_path(home),
        SettingScope::Project => project_settings_path(home),
        SettingScope::Session => {
            // Die CLI-Grammatik kennt keinen Weg, `Session` anzufordern
            // (siehe `SettingsScopeArgs::resolve`); dieser Zweig ist daher
            // toter Code, bleibt aber exhaustiv statt eines `unreachable!`.
            Err(SettingsError::InvalidMode {
                mode: "session".to_owned(),
            })
        }
    }
}

/// `~/.harw/profiles/<p>/providers`.
fn providers_dir(home: &Path) -> Result<PathBuf, SettingsError> {
    Ok(active_profile_dir(home)?.join("providers"))
}

/// `~/.harw/profiles/<p>/providers/<name>.toml`, nach Validierung von `name`.
fn provider_path(home: &Path, name: &str) -> Result<PathBuf, SettingsError> {
    validate_provider_name(name)?;
    Ok(providers_dir(home)?.join(format!("{name}.toml")))
}

// ---------------------------------------------------------------------
// Validierung im Stil von `harw doctor`
// ---------------------------------------------------------------------

/// Lädt die Config-Kette neu und validiert sie, druckt ein knappes Ergebnis.
///
/// Schlägt die Validierung fehl, wird das nur gemeldet, nicht propagiert:
/// die betroffene Datei ist zu diesem Zeitpunkt bereits geschrieben (analog
/// zu einem separaten `harw doctor`-Aufruf danach).
fn print_validation_result(home: &Path) {
    let outcome = harw_home::config_layers(home)
        .map_err(SettingsError::from)
        .and_then(|layers| {
            harw_config::discover_config(&layers)
                .map_err(SettingsError::from)
                .map(|config| (layers, config))
        });
    match outcome {
        Ok((layers, config)) => match config.validate().and_then(|()| {
            // Agentendefinitionen reicht `harw-config` ungeparst weiter; ihr
            // Senken und die Auswahlprüfung gehören zur Validierung.
            harw_registry_defaults::ConfigAgents::from_config_validated(&config).map(|_| ())
        }) {
            Ok(()) => println!(
                "config_valid=true layers={} providers={} models={}",
                layers.len(),
                config.providers.len(),
                config.models.len()
            ),
            Err(error) => println!("config_valid=false reason={error}"),
        },
        Err(error) => println!("config_valid=false reason={error}"),
    }
}

// ---------------------------------------------------------------------
// `harw settings provider …`
// ---------------------------------------------------------------------

fn run_provider(home: &Path, action: SettingsProviderAction) -> Result<(), SettingsError> {
    match action {
        SettingsProviderAction::List => list_providers(home),
        SettingsProviderAction::Add {
            name,
            api,
            base_url,
            auth,
            models,
            auth_header,
            no_auth,
            allow_insecure_lan,
        } => {
            let options = ProviderAddOptions::default()
                .with_auth_header(auth_header)
                .with_no_auth(no_auth)
                .with_allow_insecure_lan(allow_insecure_lan);
            add_provider(
                home,
                &name,
                api.as_str(),
                &base_url,
                auth.as_deref(),
                models,
                &options,
            )?;
            print_validation_result(home);
            Ok(())
        }
        SettingsProviderAction::Remove { name } => {
            remove_provider(home, &name)?;
            print_validation_result(home);
            Ok(())
        }
        SettingsProviderAction::Enable { name } => {
            set_provider_enabled(home, &name, true)?;
            print_validation_result(home);
            Ok(())
        }
        SettingsProviderAction::Disable { name } => {
            set_provider_enabled(home, &name, false)?;
            print_validation_result(home);
            Ok(())
        }
    }
}

fn list_providers(home: &Path) -> Result<(), SettingsError> {
    let names = discover_provider_names(home)?;
    if names.is_empty() {
        println!("Keine Provider konfiguriert.");
        return Ok(());
    }
    for name in names {
        let provider = read_provider(home, &name)?;
        println!(
            "{name}\tapi={}\tbase_url={}\tenabled={}\tmodels={}",
            provider.api,
            provider.base_url,
            provider.enabled,
            provider.models.join(",")
        );
    }
    Ok(())
}

/// Sortierte Liste aller `<name>` mit `providers/<name>.toml`.
fn discover_provider_names(home: &Path) -> Result<Vec<String>, SettingsError> {
    let dir = providers_dir(home)?;
    let mut names = Vec::new();
    match std::fs::read_dir(&dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|source| SettingsError::Io {
                    path: dir.clone(),
                    source,
                })?;
                if let Some(name) = entry
                    .file_name()
                    .to_str()
                    .and_then(|file_name| file_name.strip_suffix(".toml"))
                {
                    names.push(name.to_owned());
                }
            }
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => return Err(SettingsError::Io { path: dir, source }),
    }
    names.sort();
    Ok(names)
}

fn read_provider(home: &Path, name: &str) -> Result<ProviderToml, SettingsError> {
    let path = provider_path(home, name)?;
    let content = std::fs::read_to_string(&path).map_err(|source| SettingsError::Io {
        path: path.clone(),
        source,
    })?;
    toml::from_str(&content).map_err(|error| SettingsError::Toml {
        path,
        reason: error.to_string(),
    })
}

fn write_provider(home: &Path, provider: &ProviderToml) -> Result<(), SettingsError> {
    let path = provider_path(home, &provider.name)?;
    let rendered = toml::to_string_pretty(provider).map_err(|error| SettingsError::Toml {
        path: path.clone(),
        reason: error.to_string(),
    })?;
    write_atomic(&path, rendered.as_bytes())
}

/// Runde 7, Teil L1/L8: Zusatzoptionen von `harw provider add`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProviderAddOptions {
    /// Ausdrücklicher Credential-Transport (`bearer`/`x-api-key`/`api-key`/`none`).
    auth_header: Option<String>,
    /// `--no-auth`: kein Schlüssel, `auth_header = "none"`.
    no_auth: bool,
    /// `--allow-insecure-lan`: `http` zu privaten LAN-IPs erlauben.
    allow_insecure_lan: bool,
}

impl ProviderAddOptions {
    /// Setzt den Credential-Transport (`None` = Vorgabe).
    #[must_use]
    pub(crate) fn with_auth_header(mut self, auth_header: Option<impl Into<String>>) -> Self {
        self.auth_header = auth_header.map(Into::into);
        self
    }

    /// Schaltet `--no-auth`.
    #[must_use]
    pub(crate) fn with_no_auth(mut self, no_auth: bool) -> Self {
        self.no_auth = no_auth;
        self
    }

    /// Schaltet `--allow-insecure-lan`.
    #[must_use]
    pub(crate) fn with_allow_insecure_lan(mut self, allow_insecure_lan: bool) -> Self {
        self.allow_insecure_lan = allow_insecure_lan;
        self
    }
}

/// Legt `providers/<name>.toml` an bzw. überschreibt sie.
///
/// Runde 7, Teil L1/L5: Für lokale Endpunkte (Loopback bzw. LAN mit
/// `--allow-insecure-lan`) ohne `--auth` wird `auth_header = "none"`
/// geschrieben und `max_concurrency = 1` vorbelegt.
///
/// # Errors
/// Ungültiger Name, unbekannte API, abgelehnte Basis-URL, Klartext-`--auth`,
/// widersprüchliche Auth-Angaben oder ein Schreibfehler.
fn add_provider(
    home: &Path,
    name: &str,
    api: &str,
    base_url: &str,
    auth: Option<&str>,
    models: Vec<String>,
    options: &ProviderAddOptions,
) -> Result<(), SettingsError> {
    validate_provider_name(name)?;
    if !matches!(
        api,
        "openai-chat" | "openai-responses" | "anthropic-messages" | "ollama"
    ) {
        return Err(SettingsError::UnsupportedApi {
            api: api.to_owned(),
        });
    }
    harw_provider_http::validate_endpoint_with(base_url, options.allow_insecure_lan)
        .map_err(|error| SettingsError::InvalidBaseUrl(error.to_string()))?;
    let auth_ref = parse_auth_ref(name, auth)?;
    let auth_header = if options.no_auth {
        Some("none".to_owned())
    } else {
        options.auth_header.clone()
    };
    if auth_ref.is_some() && auth_header.as_deref() == Some("none") {
        return Err(SettingsError::ConflictingAuth {
            name: name.to_owned(),
        });
    }

    let mut provider = ProviderToml {
        stream: None,
        name: name.to_owned(),
        api: api.to_owned(),
        base_url: base_url.to_owned(),
        auth: auth_ref,
        auth_header: None,
        api_key: None,
        originator: None,
        headers: std::collections::HashMap::new(),
        models,
        enabled: true,
        origin_allowlist: harw_config::OriginAllowlistToml::default(),
        rate_limit: None,
        max_concurrency: None,
        default_reasoning_effort: None,
        gateway_identity_headers: false,
        request_timeout_secs: None,
        stream_idle_timeout_secs: None,
        retry_timeouts: None,
        max_tokens_field: None,
        send_reasoning_effort: None,
        strict_tools: None,
        parallel_tool_calls: None,
        allow_insecure_lan: options.allow_insecure_lan,
    };
    provider.auth_header = auth_header;
    if provider.is_local() {
        if provider.auth.is_none() && provider.auth_header.is_none() {
            provider.auth_header = Some("none".to_owned());
        }
        provider.max_concurrency = Some(1);
    }
    write_provider(home, &provider)
}

/// Parst `--auth` strikt als [`SecretRef`]; ein Klartext-Wert wird
/// abgelehnt (siehe [`SettingsError::PlaintextAuthRejected`]).
fn parse_auth_ref(
    provider_name: &str,
    auth: Option<&str>,
) -> Result<Option<SecretRef>, SettingsError> {
    match auth {
        None => Ok(None),
        Some(raw) if raw.trim().is_empty() => Ok(None),
        Some(raw) => {
            raw.parse::<SecretRef>()
                .map(Some)
                .map_err(|_| SettingsError::PlaintextAuthRejected {
                    name: provider_name.to_owned(),
                })
        }
    }
}

fn remove_provider(home: &Path, name: &str) -> Result<(), SettingsError> {
    let path = provider_path(home, name)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            Err(SettingsError::ProviderNotFound {
                name: name.to_owned(),
            })
        }
        Err(source) => Err(SettingsError::Io { path, source }),
    }
}

fn set_provider_enabled(home: &Path, name: &str, enabled: bool) -> Result<(), SettingsError> {
    let path = provider_path(home, name)?;
    if !path.exists() {
        return Err(SettingsError::ProviderNotFound {
            name: name.to_owned(),
        });
    }
    let mut provider = read_provider(home, name)?;
    provider.enabled = enabled;
    write_provider(home, &provider)
}

/// Validiert einen Provider-Namen, bevor er als Datei-Komponente verwendet
/// wird. Minimal nachgebaut aus `crate::onboarding::validate_provider_name`
/// (siehe Modul-Doku): dasselbe Zeichen-Set, damit Provider-Dateien aus
/// beiden Pfaden austauschbar bleiben.
fn validate_provider_name(name: &str) -> Result<(), SettingsError> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(SettingsError::InvalidProviderName {
            name: name.to_owned(),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------
// `harw settings model default …`
// ---------------------------------------------------------------------

fn run_model(home: &Path, action: SettingsModelAction) -> Result<(), SettingsError> {
    match action {
        SettingsModelAction::Default { id } => {
            set_default_model(home, &id)?;
            print_validation_result(home);
            Ok(())
        }
    }
}

/// Setzt `default_model` in der globalen Profil-Config — dieselbe Ebene, in
/// die `crate::onboarding` schreibt.
fn set_default_model(home: &Path, id: &str) -> Result<(), SettingsError> {
    let path = global_config_path(home)?;
    let mut writer = ConfigWriter::open(&path)?;
    writer.set_value("default_model", value(id))?;
    writer.save()?;
    Ok(())
}

// ---------------------------------------------------------------------
// `harw settings get|set …`
// ---------------------------------------------------------------------

fn run_get(home: &Path, key: &str, scope: SettingScope) -> Result<(), SettingsError> {
    let path = config_path_for_scope(home, scope)?;
    let writer = ConfigWriter::open(&path)?;
    match writer.get_value(key) {
        Some(text) => println!("{key} = {text}"),
        None => println!("{key} ist nicht gesetzt"),
    }
    Ok(())
}

fn run_set(
    home: &Path,
    key: &str,
    new_value: Option<String>,
    scope: SettingScope,
) -> Result<(), SettingsError> {
    let path = config_path_for_scope(home, scope)?;
    let Some(new_value) = new_value else {
        return run_unset(home, key, &path);
    };
    let mut writer = ConfigWriter::open(&path)?;
    writer.set_value(key, value(new_value.as_str()))?;
    writer.save()?;
    print_validation_result(home);
    Ok(())
}

/// Ergebnis von [`unset_value`].
#[derive(Debug, PartialEq, Eq)]
enum UnsetOutcome {
    /// Der Schlüssel wurde entfernt; `pruned` nennt die dadurch leer
    /// gewordenen und ebenfalls entfernten Tabellen (tiefste zuerst,
    /// punktgetrennt).
    Removed { pruned: Vec<String> },
    /// Der Schlüssel war in der Datei nicht gesetzt; nichts wurde geschrieben.
    NotSet,
}

/// `harw settings set <key>` ohne Wert: löscht `key` aus `path` und druckt
/// das Ergebnis. Ein fehlender Schlüssel ist kein Fehler.
///
/// # Errors
/// Siehe [`unset_value`].
fn run_unset(home: &Path, key: &str, path: &Path) -> Result<(), SettingsError> {
    match unset_value(path, key)? {
        UnsetOutcome::NotSet => {
            println!(
                "{key} ist in {} nicht gesetzt; nichts zu löschen",
                path.display()
            );
        }
        UnsetOutcome::Removed { pruned } => {
            println!("{key} aus {} gelöscht", path.display());
            for table in &pruned {
                println!("leere Tabelle [{table}] entfernt");
            }
            print_validation_result(home);
        }
    }
    Ok(())
}

/// Entfernt den punktgetrennten Schlüssel `key` aus der TOML-Datei `path`
/// und räumt dadurch leer gewordene Eltern-Tabellen auf.
///
/// # Description
/// Geschrieben wird ausschließlich über [`ConfigWriter`] (Kommentare
/// bleiben erhalten, Backup, atomar, Validierung). Weil
/// [`ConfigWriter::remove_value`] leere Zwischentabellen bewusst stehen
/// lässt und das Dokument nicht nach außen reicht, ermittelt diese Funktion
/// die zu entfernenden Eltern-Tabellen an einer zweiten, nur gelesenen
/// Kopie des Dokuments (identische Navigation: nur echte Tabellen, keine
/// Inline-Tabellen — wie `get_value`/`remove_value`) und entfernt sie
/// danach ebenfalls über den Writer. Aufgeräumt wird von innen nach außen
/// und nur, solange eine Tabelle wirklich leer ist.
///
/// # Returns
/// [`UnsetOutcome::NotSet`], wenn `key` (oder ein Elternteil davon) fehlt
/// bzw. ein Elternteil keine Tabelle ist — die Datei bleibt dann unberührt.
///
/// # Errors
/// - [`SettingsError::Io`]: die Datei ist vorhanden, aber nicht lesbar.
/// - [`SettingsError::Toml`]: die Datei ist kein gültiges TOML.
/// - [`SettingsError::Config`]: Öffnen, Validieren oder Schreiben über
///   [`ConfigWriter`] schlug fehl.
fn unset_value(path: &Path, key: &str) -> Result<UnsetOutcome, SettingsError> {
    let mut shadow = read_toml_document(path)?;
    let segments: Vec<&str> = key.split('.').collect();
    if !remove_from_table(shadow.as_table_mut(), &segments) {
        return Ok(UnsetOutcome::NotSet);
    }

    let mut pruned = Vec::new();
    for depth in (1..segments.len()).rev() {
        let parent = &segments[..depth];
        let is_empty = table_at(shadow.as_table(), parent).is_some_and(Table::is_empty);
        if !is_empty {
            break;
        }
        remove_from_table(shadow.as_table_mut(), parent);
        pruned.push(parent.join("."));
    }

    let mut writer = ConfigWriter::open(path)?;
    if !writer.remove_value(key) {
        // Die Datei hat sich zwischen den beiden Lesevorgängen geändert;
        // dann gibt es nichts mehr zu löschen.
        return Ok(UnsetOutcome::NotSet);
    }
    for parent in &pruned {
        writer.remove_value(parent);
    }
    writer.save()?;
    Ok(UnsetOutcome::Removed { pruned })
}

/// Liest `path` als formatierungserhaltendes TOML-Dokument; eine fehlende
/// Datei ergibt ein leeres Dokument (wie [`ConfigWriter::open`]).
///
/// # Errors
/// [`SettingsError::Io`] bei Lesefehlern außer „nicht gefunden“,
/// [`SettingsError::Toml`] bei ungültigem TOML.
fn read_toml_document(path: &Path) -> Result<DocumentMut, SettingsError> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DocumentMut::new());
        }
        Err(source) => {
            return Err(SettingsError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    content
        .parse::<DocumentMut>()
        .map_err(|error| SettingsError::Toml {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })
}

/// Navigiert über echte Tabellen (keine Inline-Tabellen) zu `segments`.
fn table_at<'a>(table: &'a Table, segments: &[&str]) -> Option<&'a Table> {
    segments.iter().try_fold(table, |current, segment| {
        current.get(segment).and_then(Item::as_table)
    })
}

/// Entfernt das Element unter `segments`; `true`, wenn es existierte.
/// Spiegelt die Navigation von [`ConfigWriter::remove_value`].
fn remove_from_table(table: &mut Table, segments: &[&str]) -> bool {
    match segments {
        [] => false,
        [key] => table.remove(key).is_some(),
        [key, rest @ ..] => table
            .get_mut(key)
            .and_then(Item::as_table_mut)
            .is_some_and(|child| remove_from_table(child, rest)),
    }
}

// ---------------------------------------------------------------------
// `harw settings permissions …` — derselbe Schreibpfad wie `/permissions`.
// ---------------------------------------------------------------------

fn run_permissions(home: &Path, action: SettingsPermissionsAction) -> Result<(), SettingsError> {
    match action {
        SettingsPermissionsAction::Get { scope } => print_permissions(home, scope.resolve()),
        SettingsPermissionsAction::SetMode { mode, scope } => {
            set_permissions_mode(home, mode.as_str(), scope.resolve())?;
            print_validation_result(home);
            Ok(())
        }
        SettingsPermissionsAction::Allow {
            tool,
            pattern,
            scope,
        } => {
            append_permissions_rule(home, RuleKind::Allow, tool, pattern, scope.resolve())?;
            print_validation_result(home);
            Ok(())
        }
        SettingsPermissionsAction::Deny {
            tool,
            pattern,
            scope,
        } => {
            append_permissions_rule(home, RuleKind::Deny, tool, pattern, scope.resolve())?;
            print_validation_result(home);
            Ok(())
        }
        SettingsPermissionsAction::Unallow { index, scope } => {
            remove_permissions_rule(home, RuleKind::Allow, index, scope.resolve())?;
            print_validation_result(home);
            Ok(())
        }
        SettingsPermissionsAction::Undeny { index, scope } => {
            remove_permissions_rule(home, RuleKind::Deny, index, scope.resolve())?;
            print_validation_result(home);
            Ok(())
        }
    }
}

fn print_permissions(home: &Path, scope: SettingScope) -> Result<(), SettingsError> {
    let path = config_path_for_scope(home, scope)?;
    let writer = ConfigWriter::open(&path)?;
    let mode = writer
        .get_value("permissions.default_mode")
        .unwrap_or_else(|| "(nicht gesetzt)".to_owned());
    println!("scope={scope} default_mode={mode}");
    // `get_value` liest nur skalare Werte; Allow-/Deny-Listen werden über die
    // vollständige `PermissionsSection` gelesen, damit Index-Angaben für
    // `unallow`/`undeny` stimmen.
    let section = load_permissions_section(&path)?;
    print_rule_list("allow", &section.allow);
    print_rule_list("deny", &section.deny);
    Ok(())
}

fn print_rule_list(label: &str, rules: &[RuleToml]) {
    if rules.is_empty() {
        println!("{label}: (keine Regeln)");
        return;
    }
    for (index, rule) in rules.iter().enumerate() {
        match &rule.pattern {
            Some(pattern) => println!("{label}[{index}] tool={} pattern={pattern}", rule.tool),
            None => println!("{label}[{index}] tool={}", rule.tool),
        }
    }
}

fn load_permissions_section(path: &Path) -> Result<harw_config::PermissionsSection, SettingsError> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(harw_config::PermissionsSection::default());
        }
        Err(source) => {
            return Err(SettingsError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    #[derive(serde::Deserialize)]
    struct Document {
        #[serde(default)]
        permissions: harw_config::PermissionsSection,
    }
    let document: Document = toml::from_str(&content).map_err(|error| SettingsError::Toml {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    Ok(document.permissions)
}

fn set_permissions_mode(home: &Path, mode: &str, scope: SettingScope) -> Result<(), SettingsError> {
    if !matches!(mode, "ask" | "auto" | "full") {
        return Err(SettingsError::InvalidMode {
            mode: mode.to_owned(),
        });
    }
    let path = config_path_for_scope(home, scope)?;
    let mut writer = ConfigWriter::open(&path)?;
    writer.set_default_mode(mode)?;
    writer.save()?;
    Ok(())
}

fn append_permissions_rule(
    home: &Path,
    kind: RuleKind,
    tool: String,
    pattern: Option<String>,
    scope: SettingScope,
) -> Result<(), SettingsError> {
    let path = config_path_for_scope(home, scope)?;
    let rule = RuleToml { tool, pattern };
    let mut writer = ConfigWriter::open(&path)?;
    writer.append_rule(kind, &rule)?;
    writer.save()?;
    Ok(())
}

fn remove_permissions_rule(
    home: &Path,
    kind: RuleKind,
    index: usize,
    scope: SettingScope,
) -> Result<(), SettingsError> {
    let path = config_path_for_scope(home, scope)?;
    let section = load_permissions_section(&path)?;
    let len = match kind {
        RuleKind::Allow => section.allow.len(),
        RuleKind::Deny => section.deny.len(),
    };
    let mut writer = ConfigWriter::open(&path)?;
    match writer.remove_rule(kind, index)? {
        Some(_) => {
            writer.save()?;
            Ok(())
        }
        None => Err(SettingsError::InvalidRuleIndex { index, len }),
    }
}

// ---------------------------------------------------------------------
// Interaktives Menü
// ---------------------------------------------------------------------

/// Zeilenbasiertes Menü (kein ratatui), im Stil von `crate::onboarding`'s
/// nicht-interaktivem Prompt-Pfad. Jeder Schritt ist über eine leere
/// Eingabe oder `n`/`nein` bei der Bestätigung abbrechbar.
fn run_interactive(home: &Path) -> Result<(), SettingsError> {
    loop {
        println!();
        println!("== harw settings ==");
        println!("1) Provider verwalten");
        println!("2) Standardmodell setzen");
        println!("3) Freigaben (permissions)");
        println!("4) Session-Titel-Modell");
        println!("0) Beenden");
        print!("Auswahl: ");
        flush_stdout()?;
        let choice = read_line()?;
        match choice.trim() {
            "1" => report_step(interactive_provider(home)),
            "2" => report_step(interactive_default_model(home)),
            "3" => report_step(interactive_permissions(home)),
            "4" => report_step(interactive_title_model(home)),
            "0" | "" => return Ok(()),
            other => println!("Unbekannte Auswahl: {other}"),
        }
    }
}

/// Meldet einen abgebrochenen interaktiven Schritt als Hinweis statt als
/// Fehlschlag des gesamten Menüs; alle anderen Fehler werden gedruckt und
/// das Menü läuft weiter (ein Tippfehler soll nicht die ganze Sitzung
/// beenden).
fn report_step(result: Result<(), SettingsError>) {
    match result {
        Ok(()) => {}
        Err(SettingsError::Aborted) => println!("Abgebrochen."),
        Err(error) => println!("Fehler: {error}"),
    }
}

fn interactive_provider(home: &Path) -> Result<(), SettingsError> {
    println!("-- Provider --");
    println!("1) Liste");
    println!("2) Hinzufügen");
    println!("3) Aktivieren");
    println!("4) Deaktivieren");
    println!("5) Entfernen");
    print!("Auswahl (leer = zurück): ");
    flush_stdout()?;
    match read_line()?.trim() {
        "1" => list_providers(home),
        "2" => interactive_add_provider(home),
        "3" => {
            let name = prompt_required("Provider-Name")?;
            confirm_and_run(&format!("Provider {name:?} aktivieren"), || {
                set_provider_enabled(home, &name, true)
            })
        }
        "4" => {
            let name = prompt_required("Provider-Name")?;
            confirm_and_run(&format!("Provider {name:?} deaktivieren"), || {
                set_provider_enabled(home, &name, false)
            })
        }
        "5" => {
            let name = prompt_required("Provider-Name")?;
            confirm_and_run(&format!("Provider {name:?} entfernen"), || {
                remove_provider(home, &name)
            })
        }
        _ => Ok(()),
    }
}

fn interactive_add_provider(home: &Path) -> Result<(), SettingsError> {
    let name = prompt_required("Provider-Name")?;
    let api = prompt_default(
        "API (openai-chat/openai-responses/anthropic-messages/ollama)",
        "openai-chat",
    )?;
    let base_url = prompt_required("Basis-URL")?;
    let auth = prompt_optional("Secret-Referenz (env:VAR/secrets:NAME, leer = keine)")?;
    let models_raw = prompt_optional("Modell-IDs, kommagetrennt (leer = keine)")?;
    let models = models_raw
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();

    println!(
        "Zusammenfassung: name={name} api={api} base_url={base_url} auth={} models={models:?}",
        auth.as_deref().unwrap_or("(keine)")
    );
    confirm_and_run("Provider so anlegen", || {
        add_provider(
            home,
            &name,
            &api,
            &base_url,
            auth.as_deref(),
            models.clone(),
            &ProviderAddOptions::default(),
        )
    })?;
    print_validation_result(home);
    Ok(())
}

fn interactive_default_model(home: &Path) -> Result<(), SettingsError> {
    let id = prompt_required("Modell-ID")?;
    confirm_and_run(&format!("Standardmodell auf {id:?} setzen"), || {
        set_default_model(home, &id)
    })?;
    print_validation_result(home);
    Ok(())
}

fn interactive_permissions(home: &Path) -> Result<(), SettingsError> {
    println!("-- Freigaben --");
    println!("1) Anzeigen");
    println!("2) Standardmodus setzen");
    println!("3) Allow-Regel hinzufügen");
    println!("4) Deny-Regel hinzufügen");
    print!("Auswahl (leer = zurück): ");
    flush_stdout()?;
    match read_line()?.trim() {
        "1" => print_permissions(home, prompt_scope()?),
        "2" => {
            let mode = prompt_required("Modus (ask/auto/full)")?;
            let scope = prompt_scope()?;
            confirm_and_run(
                &format!("Standardmodus auf {mode:?} setzen ({scope})"),
                || set_permissions_mode(home, &mode, scope),
            )
        }
        "3" => {
            let tool = prompt_required("Werkzeug")?;
            let pattern = prompt_optional("Muster (leer = keines)")?;
            let scope = prompt_scope()?;
            confirm_and_run(&format!("Allow-Regel {tool:?} anlegen ({scope})"), || {
                append_permissions_rule(home, RuleKind::Allow, tool.clone(), pattern.clone(), scope)
            })
        }
        "4" => {
            let tool = prompt_required("Werkzeug")?;
            let pattern = prompt_optional("Muster (leer = keines)")?;
            let scope = prompt_scope()?;
            confirm_and_run(&format!("Deny-Regel {tool:?} anlegen ({scope})"), || {
                append_permissions_rule(home, RuleKind::Deny, tool.clone(), pattern.clone(), scope)
            })
        }
        _ => Ok(()),
    }
}

/// Session-Titel-Modell: generischer Schlüssel `session.title_model` in der
/// globalen Ebene. **Annahme**: der genaue Schlüsselname gehört zum
/// Titel-Generierungs-Slice (`harw-runtime/src/session_title.rs`, Contract
/// §5 Zeile B6), der außerhalb dieses Auftrags liegt; bis dieser Slice das
/// Feld in `HarnessConfig`/`SessionSection` deklariert, meldet die
/// Nachschreib-Validierung ggf. `config_valid=false` — siehe Bericht.
fn interactive_title_model(home: &Path) -> Result<(), SettingsError> {
    let id = prompt_required("Modell-ID für Session-Titel")?;
    confirm_and_run(&format!("Session-Titel-Modell auf {id:?} setzen"), || {
        run_set(
            home,
            "session.title_model",
            Some(id.clone()),
            SettingScope::Global,
        )
    })
}

fn prompt_scope() -> Result<SettingScope, SettingsError> {
    match prompt_default("Ebene (global/project)", "global")?.as_str() {
        "project" => Ok(SettingScope::Project),
        _ => Ok(SettingScope::Global),
    }
}

/// Zeigt `summary`, fragt eine Bestätigung ab und führt `action` nur bei
/// `j`/`ja`/`y`/`yes` aus; jede andere Eingabe (inklusive leer) bricht ab.
fn confirm_and_run(
    summary: &str,
    action: impl FnOnce() -> Result<(), SettingsError>,
) -> Result<(), SettingsError> {
    print!("{summary} — bestätigen? [j/N]: ");
    flush_stdout()?;
    let answer = read_line()?;
    match answer.trim().to_ascii_lowercase().as_str() {
        "j" | "ja" | "y" | "yes" => action(),
        _ => Err(SettingsError::Aborted),
    }
}

fn prompt_required(label: &str) -> Result<String, SettingsError> {
    // Ursprünglich als `loop` geschrieben, aber beide Zweige enden mit
    // `return` — die Schleife lief also nie ein zweites Mal (clippy:
    // `never_loop`). Die Absicht dieser Funktion ist, bei leerer Eingabe
    // sofort abzubrechen (siehe Meldung „abgebrochen: leere Eingabe"),
    // nicht erneut nach einem gültigen Wert zu fragen — passend zum
    // Abbruchverhalten von `confirm_and_run` bei ungültiger Bestätigung.
    // Daher hier der gerade Ablauf ohne Schleife, kein erneutes Nachfragen.
    print!("{label}: ");
    flush_stdout()?;
    let line = read_line()?;
    let trimmed = line.trim();
    if trimmed.is_empty() {
        println!("(abgebrochen: leere Eingabe)");
        return Err(SettingsError::Aborted);
    }
    Ok(trimmed.to_owned())
}

fn prompt_optional(label: &str) -> Result<Option<String>, SettingsError> {
    print!("{label}: ");
    flush_stdout()?;
    let line = read_line()?;
    let trimmed = line.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_owned()))
    }
}

fn prompt_default(label: &str, default: &str) -> Result<String, SettingsError> {
    print!("{label} [{default}]: ");
    flush_stdout()?;
    let line = read_line()?;
    let trimmed = line.trim();
    if trimmed.is_empty() {
        Ok(default.to_owned())
    } else {
        Ok(trimmed.to_owned())
    }
}

fn read_line() -> Result<String, SettingsError> {
    let mut buffer = String::new();
    std::io::stdin()
        .read_line(&mut buffer)
        .map_err(|source| SettingsError::Io {
            path: PathBuf::from("<stdin>"),
            source,
        })?;
    if buffer.is_empty() && !std::io::stdin().is_terminal() {
        // EOF auf einem Nicht-TTY (Pipes/Tests): wie eine leere Zeile
        // behandeln, statt endlos zu blockieren.
        return Ok(String::new());
    }
    Ok(buffer)
}

fn flush_stdout() -> Result<(), SettingsError> {
    std::io::stdout()
        .flush()
        .map_err(|source| SettingsError::Io {
            path: PathBuf::from("<stdout>"),
            source,
        })
}

// ---------------------------------------------------------------------
// Atomares Schreiben für Provider-Dateien (siehe Modul-Doku).
// ---------------------------------------------------------------------

/// Schreibt `content` atomar nach `path`: ein `0600`-Temp-Nachbar wird
/// angelegt, geschrieben, synchronisiert und über `path` umbenannt. Minimal
/// nachgebaut aus `crate::onboarding::write_file`/`secret_temp_file` (siehe
/// Modul-Doku) — `onboarding.rs` selbst wird nicht verändert.
fn write_atomic(path: &Path, content: &[u8]) -> Result<(), SettingsError> {
    let parent = path.parent().ok_or_else(|| SettingsError::Io {
        path: path.to_path_buf(),
        source: std::io::Error::other("Zielpfad hat kein Elternverzeichnis"),
    })?;
    std::fs::create_dir_all(parent).map_err(|source| SettingsError::Io {
        path: parent.to_path_buf(),
        source,
    })?;

    let temp_path = parent.join(format!(
        ".settings-{}-{}.tmp",
        std::process::id(),
        TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let write_result = options
        .open(&temp_path)
        .and_then(|mut file| file.write_all(content).and_then(|()| file.sync_all()));
    if let Err(source) = write_result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(SettingsError::Io {
            path: temp_path,
            source,
        });
    }
    std::fs::rename(&temp_path, path).map_err(|source| {
        let _ = std::fs::remove_file(&temp_path);
        SettingsError::Io {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    /// Isoliertes `HARW_HOME` für einen Test; nutzt ausschließlich
    /// `--home`-Style Overrides (kein Env-Mutieren), damit Tests parallel
    /// laufen können.
    fn temp_home() -> TestResult<(tempfile::TempDir, PathBuf)> {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = dir.path().join("harw-home");
        harw_home::ensure_home(&home).map_err(ctx("ensure_home"))?;
        Ok((dir, home))
    }

    #[test]
    fn test_add_provider_rejects_plaintext_auth() -> TestResult {
        let (_guard, home) = temp_home()?;
        let result = add_provider(
            &home,
            "acme",
            "openai-chat",
            "https://api.acme.test/v1",
            Some("sk-plain"),
            vec![],
            &ProviderAddOptions::default(),
        );
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "plaintext auth must be rejected".into(),
            ));
        };
        assert!(matches!(error, SettingsError::PlaintextAuthRejected { .. }));
        assert!(error.to_string().contains("harw auth"));
        Ok(())
    }

    /// Runde 7, Teil L1/L5: `--no-auth` schreibt `auth_header = "none"`;
    /// ein Loopback-Provider bekommt `max_concurrency = 1` und baut danach
    /// ohne Schlüssel.
    #[test]
    fn test_add_provider_no_auth_for_local_vllm() -> TestResult {
        let (_guard, home) = temp_home()?;
        add_provider(
            &home,
            "vllm",
            "openai-chat",
            "http://localhost:8000/v1",
            None,
            vec!["qwen3".to_owned()],
            &ProviderAddOptions::default().with_no_auth(true),
        )
        .map_err(ctx("add local provider"))?;
        let provider = read_provider(&home, "vllm").map_err(ctx("read provider"))?;
        assert_eq!(provider.auth, None);
        assert_eq!(provider.auth_header.as_deref(), Some("none"));
        assert_eq!(provider.max_concurrency, Some(1));

        let mut config = harw_config::ResolvedConfig::default();
        config.harness.default_provider = Some("vllm".to_owned());
        config.harness.default_model = Some("qwen3".to_owned());
        config.providers.insert("vllm".to_owned(), provider);
        harw_provider_http::build_provider(&config).map_err(ctx("keyless provider builds"))?;

        // Ohne `--no-auth` wirkt dieselbe Vorgabe für Loopback ohne `--auth`.
        add_provider(
            &home,
            "lmstudio",
            "openai-chat",
            "http://127.0.0.1:1234/v1",
            None,
            vec![],
            &ProviderAddOptions::default(),
        )
        .map_err(ctx("add lmstudio"))?;
        let lmstudio = read_provider(&home, "lmstudio").map_err(ctx("read lmstudio"))?;
        assert_eq!(lmstudio.auth_header.as_deref(), Some("none"));
        Ok(())
    }

    #[test]
    fn test_add_provider_auth_header_and_conflicts() -> TestResult {
        let (_guard, home) = temp_home()?;
        add_provider(
            &home,
            "gw",
            "openai-chat",
            "https://gw.example/v1",
            Some("env:GW_KEY"),
            vec![],
            &ProviderAddOptions::default().with_auth_header(Some("x-api-key")),
        )
        .map_err(ctx("add gateway"))?;
        let gateway = read_provider(&home, "gw").map_err(ctx("read gateway"))?;
        assert_eq!(gateway.auth_header.as_deref(), Some("x-api-key"));
        assert_eq!(gateway.max_concurrency, None);

        let result = add_provider(
            &home,
            "bad",
            "openai-chat",
            "http://localhost:8000/v1",
            Some("env:X"),
            vec![],
            &ProviderAddOptions::default().with_no_auth(true),
        );
        assert!(matches!(result, Err(SettingsError::ConflictingAuth { .. })));

        let lan = add_provider(
            &home,
            "lan",
            "openai-chat",
            "http://192.168.1.20:8000/v1",
            None,
            vec![],
            &ProviderAddOptions::default(),
        );
        assert!(matches!(lan, Err(SettingsError::InvalidBaseUrl(_))));
        add_provider(
            &home,
            "lan",
            "openai-chat",
            "http://192.168.1.20:8000/v1",
            None,
            vec![],
            &ProviderAddOptions::default().with_allow_insecure_lan(true),
        )
        .map_err(ctx("LAN with opt-in"))?;
        let lan = read_provider(&home, "lan").map_err(ctx("read lan"))?;
        assert!(lan.allow_insecure_lan);
        assert_eq!(lan.max_concurrency, Some(1));
        Ok(())
    }

    #[test]
    fn test_add_provider_rejects_invalid_name() -> TestResult {
        let (_guard, home) = temp_home()?;
        let result = add_provider(
            &home,
            "../escape",
            "openai-chat",
            "https://api.acme.test/v1",
            None,
            vec![],
            &ProviderAddOptions::default(),
        );
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "invalid provider name must be rejected".into(),
            ));
        };
        assert!(matches!(error, SettingsError::InvalidProviderName { .. }));
        Ok(())
    }

    #[test]
    fn test_add_provider_rejects_unsupported_api() -> TestResult {
        let (_guard, home) = temp_home()?;
        let result = add_provider(
            &home,
            "acme",
            "made-up-api",
            "https://api.acme.test/v1",
            None,
            vec![],
            &ProviderAddOptions::default(),
        );
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "unsupported api must be rejected".into(),
            ));
        };
        assert!(matches!(error, SettingsError::UnsupportedApi { .. }));
        Ok(())
    }

    #[test]
    fn test_provider_roundtrip_add_list_disable_enable_remove() -> TestResult {
        let (_guard, home) = temp_home()?;
        add_provider(
            &home,
            "acme",
            "openai-chat",
            "https://api.acme.test/v1",
            Some("env:ACME_KEY"),
            vec!["acme-large".to_owned()],
            &ProviderAddOptions::default(),
        )
        .map_err(ctx("add provider"))?;

        let names = discover_provider_names(&home).map_err(ctx("list providers"))?;
        assert_eq!(names, vec!["acme".to_owned()]);

        let provider = read_provider(&home, "acme").map_err(ctx("read provider"))?;
        assert!(provider.enabled);
        assert_eq!(provider.base_url, "https://api.acme.test/v1");
        assert_eq!(provider.models, vec!["acme-large".to_owned()]);

        set_provider_enabled(&home, "acme", false).map_err(ctx("disable provider"))?;
        assert!(
            !read_provider(&home, "acme")
                .map_err(ctx("reread provider"))?
                .enabled
        );

        set_provider_enabled(&home, "acme", true).map_err(ctx("enable provider"))?;
        assert!(
            read_provider(&home, "acme")
                .map_err(ctx("reread provider"))?
                .enabled
        );

        remove_provider(&home, "acme").map_err(ctx("remove provider"))?;
        assert!(
            discover_provider_names(&home)
                .map_err(ctx("list after remove"))?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn test_remove_missing_provider_is_reported() -> TestResult {
        let (_guard, home) = temp_home()?;
        let result = remove_provider(&home, "ghost");
        let Err(error) = result else {
            return Err(TestError::Unexpected("missing provider must error".into()));
        };
        assert!(matches!(error, SettingsError::ProviderNotFound { .. }));
        Ok(())
    }

    #[test]
    fn test_set_default_model_round_trips_through_global_config() -> TestResult {
        let (_guard, home) = temp_home()?;
        set_default_model(&home, "gpt-5.4").map_err(ctx("set default model"))?;

        let path = global_config_path(&home).map_err(ctx("global config path"))?;
        let writer = ConfigWriter::open(&path).map_err(ctx("reopen"))?;
        assert_eq!(
            writer.get_value("default_model"),
            Some("gpt-5.4".to_owned())
        );
        Ok(())
    }

    #[test]
    fn test_permissions_mode_rejects_unknown_value() -> TestResult {
        let (_guard, home) = temp_home()?;
        let result = set_permissions_mode(&home, "yolo", SettingScope::Global);
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "unknown mode must be rejected".into(),
            ));
        };
        assert!(matches!(error, SettingsError::InvalidMode { .. }));
        Ok(())
    }

    #[test]
    fn test_permissions_allow_deny_roundtrip_and_remove_by_index() -> TestResult {
        let (_guard, home) = temp_home()?;
        set_permissions_mode(&home, "auto", SettingScope::Global).map_err(ctx("set mode"))?;
        append_permissions_rule(
            &home,
            RuleKind::Allow,
            "shell.exec".to_owned(),
            Some("cargo check".to_owned()),
            SettingScope::Global,
        )
        .map_err(ctx("append allow rule"))?;
        append_permissions_rule(
            &home,
            RuleKind::Deny,
            "fs.write".to_owned(),
            None,
            SettingScope::Global,
        )
        .map_err(ctx("append deny rule"))?;

        let path = global_config_path(&home).map_err(ctx("global config path"))?;
        let section = load_permissions_section(&path).map_err(ctx("load permissions section"))?;
        assert_eq!(section.default_mode.as_deref(), Some("auto"));
        assert_eq!(section.allow.len(), 1);
        assert_eq!(section.allow[0].tool, "shell.exec");
        assert_eq!(section.deny.len(), 1);

        remove_permissions_rule(&home, RuleKind::Allow, 0, SettingScope::Global)
            .map_err(ctx("remove allow rule"))?;
        let section = load_permissions_section(&path).map_err(ctx("reload permissions section"))?;
        assert!(section.allow.is_empty());
        assert_eq!(section.deny.len(), 1);
        Ok(())
    }

    #[test]
    fn test_remove_permissions_rule_rejects_out_of_range_index() -> TestResult {
        let (_guard, home) = temp_home()?;
        let result = remove_permissions_rule(&home, RuleKind::Allow, 3, SettingScope::Global);
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "out-of-range index must be rejected".into(),
            ));
        };
        assert!(matches!(
            error,
            SettingsError::InvalidRuleIndex { index: 3, len: 0 }
        ));
        Ok(())
    }

    #[test]
    fn test_get_set_generic_dotted_key_round_trips_at_global_scope() -> TestResult {
        let (_guard, home) = temp_home()?;
        run_set(
            &home,
            "policy_profile",
            Some("strict".to_owned()),
            SettingScope::Global,
        )
        .map_err(ctx("set dotted key"))?;

        let path = global_config_path(&home).map_err(ctx("global config path"))?;
        let writer = ConfigWriter::open(&path).map_err(ctx("reopen"))?;
        assert_eq!(
            writer.get_value("policy_profile"),
            Some("strict".to_owned())
        );
        Ok(())
    }

    /// Schreibt `content` als globale Config und liefert deren Pfad.
    fn write_global_config(home: &Path, content: &str) -> TestResult<PathBuf> {
        let path = global_config_path(home).map_err(ctx("global config path"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(ctx("create profile dir"))?;
        }
        std::fs::write(&path, content).map_err(ctx("write config"))?;
        Ok(path)
    }

    #[test]
    fn test_set_without_value_deletes_key() -> TestResult {
        let (_guard, home) = temp_home()?;
        run_set(
            &home,
            "policy_profile",
            Some("strict".to_owned()),
            SettingScope::Global,
        )
        .map_err(ctx("set key"))?;
        run_set(&home, "policy_profile", None, SettingScope::Global).map_err(ctx("unset key"))?;

        let path = global_config_path(&home).map_err(ctx("global config path"))?;
        let writer = ConfigWriter::open(&path).map_err(ctx("reopen"))?;
        assert_eq!(writer.get_value("policy_profile"), None);
        Ok(())
    }

    #[test]
    fn test_unset_prunes_empty_parents_and_keeps_comments() -> TestResult {
        let (_guard, home) = temp_home()?;
        let path = write_global_config(
            &home,
            "# Kopfkommentar bleibt\n\
             policy_profile = \"strict\" # Zeilenkommentar bleibt\n\
             \n\
             [session]\n\
             title_model = \"m1\"\n\
             \n\
             [outer]\n\
             # Kommentar an keep\n\
             keep = 1\n\
             \n\
             [outer.inner.deep]\n\
             x = 1\n",
        )?;

        let outcome = unset_value(&path, "outer.inner.deep.x").map_err(ctx("unset deep"))?;
        assert_eq!(
            outcome,
            UnsetOutcome::Removed {
                pruned: vec!["outer.inner.deep".to_owned(), "outer.inner".to_owned()],
            }
        );
        let outcome = unset_value(&path, "session.title_model").map_err(ctx("unset session"))?;
        assert_eq!(
            outcome,
            UnsetOutcome::Removed {
                pruned: vec!["session".to_owned()],
            }
        );

        let content = std::fs::read_to_string(&path).map_err(ctx("read back"))?;
        assert!(content.contains("# Kopfkommentar bleibt"), "{content}");
        assert!(content.contains("# Zeilenkommentar bleibt"), "{content}");
        assert!(content.contains("# Kommentar an keep"), "{content}");
        assert!(content.contains("[outer]"), "{content}");
        assert!(!content.contains("[outer.inner"), "{content}");
        assert!(!content.contains("[session]"), "{content}");

        let writer = ConfigWriter::open(&path).map_err(ctx("reopen"))?;
        assert_eq!(writer.get_value("outer.keep"), Some("1".to_owned()));
        assert_eq!(
            writer.get_value("policy_profile"),
            Some("strict".to_owned())
        );
        Ok(())
    }

    #[test]
    fn test_unset_prunes_dotted_key_parents() -> TestResult {
        let (_guard, home) = temp_home()?;
        let path = write_global_config(&home, "a.b.c = 1\nother = 2\n")?;
        let outcome = unset_value(&path, "a.b.c").map_err(ctx("unset dotted"))?;
        assert_eq!(
            outcome,
            UnsetOutcome::Removed {
                pruned: vec!["a.b".to_owned(), "a".to_owned()],
            }
        );
        let content = std::fs::read_to_string(&path).map_err(ctx("read back"))?;
        assert_eq!(content.trim(), "other = 2");
        Ok(())
    }

    #[test]
    fn test_unset_missing_key_is_not_an_error_and_leaves_file_untouched() -> TestResult {
        let (_guard, home) = temp_home()?;
        let original = "# Kommentar\npolicy_profile = \"strict\"\n\n[session]\n";
        let path = write_global_config(&home, original)?;

        for key in [
            "missing",
            "session.missing",
            "nowhere.at.all",
            "policy_profile.child",
            "",
        ] {
            let outcome = unset_value(&path, key).map_err(ctx("unset missing"))?;
            assert_eq!(outcome, UnsetOutcome::NotSet, "key {key:?}");
        }
        run_set(&home, "missing", None, SettingScope::Global).map_err(ctx("run_set unset"))?;

        let content = std::fs::read_to_string(&path).map_err(ctx("read back"))?;
        assert_eq!(content, original);
        Ok(())
    }

    #[test]
    fn test_unset_without_config_file_is_not_set() -> TestResult {
        let (_guard, home) = temp_home()?;
        let path = home.join("nirgends").join("config.toml");
        let outcome = unset_value(&path, "policy_profile").map_err(ctx("unset"))?;
        assert_eq!(outcome, UnsetOutcome::NotSet);
        assert!(!path.exists());
        Ok(())
    }

    #[test]
    fn test_project_scope_writes_under_project_settings_dir() -> TestResult {
        let (_guard, home) = temp_home()?;
        // `project_settings_path` läuft über `discover_project` ab dem
        // tatsächlichen Arbeitsverzeichnis des Testprozesses; da jeder
        // Ordner mindestens `ProjectKind::Directory` ergibt, muss dieser
        // Aufruf immer einen Pfad liefern.
        let path = project_settings_path(&home).map_err(ctx("resolve project settings path"))?;
        assert!(path.starts_with(home.join("profiles")));
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("settings.toml")
        );
        Ok(())
    }
}
