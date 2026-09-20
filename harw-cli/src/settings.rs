//! `harw settings` — Provider, Standardmodell, Freigaben und einzelne
//! Konfigurationswerte verwalten (Plan `nope-permissions-gibt-es-wild-lobster.md`
//! Schritt 8; Contract `harw-scopes-contract.md` §2, Zeile B5a).
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
//! # Secrets
//! `--auth` akzeptiert ausschließlich eine [`harw_config::SecretRef`]
//! (`env:VAR`, `secrets:NAME`, …); ein Klartext-Wert wird abgelehnt und
//! verweist auf `harw auth`.
//!
//! # Validierung
//! Nach jedem Schreiben lädt [`print_validation_result`] die betroffene
//! Config-Kette neu (`harw_home::config_layers` + `harw_config::discover_config`
//! + `ResolvedConfig::validate`) und druckt ein knappes Ergebnis — im Stil
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

use toml_edit::value;

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
    /// `settings set` ohne Wert (Löschen ist noch nicht implementiert).
    MissingValue { key: String },
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
            Self::MissingValue { key } => write!(
                f,
                "`harw settings set {key}` benötigt einen Wert; Löschen eines Schlüssels wird derzeit nicht unterstützt"
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
        Ok((layers, config)) => match config.validate() {
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
        } => {
            add_provider(home, &name, &api, &base_url, auth.as_deref(), models)?;
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

#[allow(clippy::too_many_arguments)]
fn add_provider(
    home: &Path,
    name: &str,
    api: &str,
    base_url: &str,
    auth: Option<&str>,
    models: Vec<String>,
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
    harw_provider_http::validate_endpoint(base_url)
        .map_err(|error| SettingsError::InvalidBaseUrl(error.to_string()))?;
    let auth_ref = parse_auth_ref(name, auth)?;

    let provider = ProviderToml {
        name: name.to_owned(),
        api: api.to_owned(),
        base_url: base_url.to_owned(),
        auth: auth_ref,
        auth_header: None,
        api_key: None,
        headers: std::collections::HashMap::new(),
        models,
        enabled: true,
        origin_allowlist: harw_config::OriginAllowlistToml::default(),
        rate_limit: None,
        max_concurrency: None,
    };
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
    writer.set_value("default_model", value(id));
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
    let Some(new_value) = new_value else {
        return Err(SettingsError::MissingValue {
            key: key.to_owned(),
        });
    };
    let path = config_path_for_scope(home, scope)?;
    let mut writer = ConfigWriter::open(&path)?;
    writer.set_value(key, value(new_value.as_str()));
    writer.save()?;
    print_validation_result(home);
    Ok(())
}

// ---------------------------------------------------------------------
// `harw settings permissions …` — derselbe Schreibpfad wie `/permissions`.
// ---------------------------------------------------------------------

fn run_permissions(home: &Path, action: SettingsPermissionsAction) -> Result<(), SettingsError> {
    match action {
        SettingsPermissionsAction::Get { scope } => print_permissions(home, scope.resolve()),
        SettingsPermissionsAction::SetMode { mode, scope } => {
            set_permissions_mode(home, &mode, scope.resolve())?;
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
    writer.set_default_mode(mode);
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
    writer.append_rule(kind, &rule);
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
    match writer.remove_rule(kind, index) {
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

    /// Isoliertes `HARW_HOME` für einen Test; nutzt ausschließlich
    /// `--home`-Style Overrides (kein Env-Mutieren), damit Tests parallel
    /// laufen können.
    fn temp_home() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("harw-home");
        harw_home::ensure_home(&home).expect("ensure_home");
        (dir, home)
    }

    #[test]
    fn test_add_provider_rejects_plaintext_auth() {
        let (_guard, home) = temp_home();
        let error = add_provider(
            &home,
            "acme",
            "openai-chat",
            "https://api.acme.test/v1",
            Some("sk-plain"),
            vec![],
        )
        .expect_err("plaintext auth must be rejected");
        assert!(matches!(error, SettingsError::PlaintextAuthRejected { .. }));
        assert!(error.to_string().contains("harw auth"));
    }

    #[test]
    fn test_add_provider_rejects_invalid_name() {
        let (_guard, home) = temp_home();
        let error = add_provider(
            &home,
            "../escape",
            "openai-chat",
            "https://api.acme.test/v1",
            None,
            vec![],
        )
        .expect_err("invalid provider name must be rejected");
        assert!(matches!(error, SettingsError::InvalidProviderName { .. }));
    }

    #[test]
    fn test_add_provider_rejects_unsupported_api() {
        let (_guard, home) = temp_home();
        let error = add_provider(
            &home,
            "acme",
            "made-up-api",
            "https://api.acme.test/v1",
            None,
            vec![],
        )
        .expect_err("unsupported api must be rejected");
        assert!(matches!(error, SettingsError::UnsupportedApi { .. }));
    }

    #[test]
    fn test_provider_roundtrip_add_list_disable_enable_remove() {
        let (_guard, home) = temp_home();
        add_provider(
            &home,
            "acme",
            "openai-chat",
            "https://api.acme.test/v1",
            Some("env:ACME_KEY"),
            vec!["acme-large".to_owned()],
        )
        .expect("add provider");

        let names = discover_provider_names(&home).expect("list providers");
        assert_eq!(names, vec!["acme".to_owned()]);

        let provider = read_provider(&home, "acme").expect("read provider");
        assert!(provider.enabled);
        assert_eq!(provider.base_url, "https://api.acme.test/v1");
        assert_eq!(provider.models, vec!["acme-large".to_owned()]);

        set_provider_enabled(&home, "acme", false).expect("disable provider");
        assert!(
            !read_provider(&home, "acme")
                .expect("reread provider")
                .enabled
        );

        set_provider_enabled(&home, "acme", true).expect("enable provider");
        assert!(
            read_provider(&home, "acme")
                .expect("reread provider")
                .enabled
        );

        remove_provider(&home, "acme").expect("remove provider");
        assert!(
            discover_provider_names(&home)
                .expect("list after remove")
                .is_empty()
        );
    }

    #[test]
    fn test_remove_missing_provider_is_reported() {
        let (_guard, home) = temp_home();
        let error = remove_provider(&home, "ghost").expect_err("missing provider must error");
        assert!(matches!(error, SettingsError::ProviderNotFound { .. }));
    }

    #[test]
    fn test_set_default_model_round_trips_through_global_config() {
        let (_guard, home) = temp_home();
        set_default_model(&home, "gpt-5.4").expect("set default model");

        let path = global_config_path(&home).expect("global config path");
        let writer = ConfigWriter::open(&path).expect("reopen");
        assert_eq!(
            writer.get_value("default_model"),
            Some("gpt-5.4".to_owned())
        );
    }

    #[test]
    fn test_permissions_mode_rejects_unknown_value() {
        let (_guard, home) = temp_home();
        let error = set_permissions_mode(&home, "yolo", SettingScope::Global)
            .expect_err("unknown mode must be rejected");
        assert!(matches!(error, SettingsError::InvalidMode { .. }));
    }

    #[test]
    fn test_permissions_allow_deny_roundtrip_and_remove_by_index() {
        let (_guard, home) = temp_home();
        set_permissions_mode(&home, "auto", SettingScope::Global).expect("set mode");
        append_permissions_rule(
            &home,
            RuleKind::Allow,
            "shell.exec".to_owned(),
            Some("cargo check".to_owned()),
            SettingScope::Global,
        )
        .expect("append allow rule");
        append_permissions_rule(
            &home,
            RuleKind::Deny,
            "fs.write".to_owned(),
            None,
            SettingScope::Global,
        )
        .expect("append deny rule");

        let path = global_config_path(&home).expect("global config path");
        let section = load_permissions_section(&path).expect("load permissions section");
        assert_eq!(section.default_mode.as_deref(), Some("auto"));
        assert_eq!(section.allow.len(), 1);
        assert_eq!(section.allow[0].tool, "shell.exec");
        assert_eq!(section.deny.len(), 1);

        remove_permissions_rule(&home, RuleKind::Allow, 0, SettingScope::Global)
            .expect("remove allow rule");
        let section = load_permissions_section(&path).expect("reload permissions section");
        assert!(section.allow.is_empty());
        assert_eq!(section.deny.len(), 1);
    }

    #[test]
    fn test_remove_permissions_rule_rejects_out_of_range_index() {
        let (_guard, home) = temp_home();
        let error = remove_permissions_rule(&home, RuleKind::Allow, 3, SettingScope::Global)
            .expect_err("out-of-range index must be rejected");
        assert!(matches!(
            error,
            SettingsError::InvalidRuleIndex { index: 3, len: 0 }
        ));
    }

    #[test]
    fn test_get_set_generic_dotted_key_round_trips_at_global_scope() {
        let (_guard, home) = temp_home();
        run_set(
            &home,
            "policy_profile",
            Some("strict".to_owned()),
            SettingScope::Global,
        )
        .expect("set dotted key");

        let path = global_config_path(&home).expect("global config path");
        let writer = ConfigWriter::open(&path).expect("reopen");
        assert_eq!(
            writer.get_value("policy_profile"),
            Some("strict".to_owned())
        );
    }

    #[test]
    fn test_set_without_value_is_rejected() {
        let (_guard, home) = temp_home();
        let error = run_set(&home, "policy_profile", None, SettingScope::Global)
            .expect_err("missing value must be rejected");
        assert!(matches!(error, SettingsError::MissingValue { .. }));
    }

    #[test]
    fn test_project_scope_writes_under_project_settings_dir() {
        let (_guard, home) = temp_home();
        // `project_settings_path` läuft über `discover_project` ab dem
        // tatsächlichen Arbeitsverzeichnis des Testprozesses; da jeder
        // Ordner mindestens `ProjectKind::Directory` ergibt, muss dieser
        // Aufruf immer einen Pfad liefern.
        let path = project_settings_path(&home).expect("resolve project settings path");
        assert!(path.starts_with(home.join("profiles")));
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("settings.toml")
        );
    }
}
