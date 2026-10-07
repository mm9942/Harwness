//! `harw models` — konfigurierte Provider/Modelle anzeigen, Modell-Discovery
//! gegen ihre `/models`-Endpunkte fahren und die internen Modellstellen
//! (Addendum C: Session-Titel, Kontext-Verdichtung, Speicher-Konsolidierung,
//! Traumreflexion, Explorer, Recherche) einzeln konfigurieren.
//!
//! # Verantwortung
//! Dieses Modul führt die Unterbefehle von [`crate::cli::ModelsAction`] aus.
//! Es liest die aufgelöste Konfiguration über `harw_home::config_layers` +
//! `harw_config::discover_config` (wie `harw doctor`/`crate::settings`) und
//! schreibt ausschließlich in die globale Ebene des aktiven Profils
//! (`~/.harw/profiles/<p>/config.toml`, `~/.harw/profiles/<p>/models/*.toml`).
//! Für `[internal_models]` fehlt `harw_config::ConfigWriter` ein
//! `remove_value`; diese Fälle (`internal reset`, `internal main`,
//! `internal openrouter-defaults`) editieren das Dokument direkt über
//! `toml_edit` und schreiben es mit einem lokal nachgebauten atomaren
//! Schreibpfad (siehe `write_atomic` unten) — analog zu
//! `crate::settings`/`crate::onboarding`, die denselben Pfad aus
//! Modul-Trennungsgründen jeweils lokal nachbauen, statt private Helfer
//! eines Schwestermoduls zu importieren.
//!
//! # Modell-Discovery
//! [`crate::models::run_scan`] fragt `harw_provider_http::discovery::list_models`
//! je konfiguriertem Provider ab; der API-Schlüssel kommt aus
//! `harw_provider_http::discovery::resolve_provider_api_key`. Der Scan erhält
//! dieselbe Home- und sealed-secret-Auflösung wie die Runtime. Das Ergebnis
//! wird nie geloggt.
//!
//! # Concurrency
//! Zustandslos; `harw models scan` baut für die Dauer des Befehls eine
//! Single-Thread-`tokio`-Laufzeit (wie `crate::connect`/`crate::auth`).

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error as StdError;
use std::fmt;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use toml_edit::value;

use harw_config::ConfigWriter;
use harw_provider_http::discovery::{self, DiscoveredModel};

use crate::cli::{InternalAction, ModelsAction, OnOff};

/// Monotoner Zähler für kollisionsfreie Temp-Dateinamen innerhalb dieses
/// Prozesses (Uniqueness kommt letztlich von `create_new`).
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Alle Fehler, die `harw models` auslösen kann.
///
/// Handgeschrieben nach Projektkonvention (kein `anyhow`/`thiserror`):
/// `Display` ist die einzige menschenlesbare Quelle, `Debug` delegiert an
/// `Display`, `std::error::Error::source` verlinkt die gewrappte Ursache.
pub enum ModelsError {
    /// Root-Space-Auflösung schlug fehl.
    Home(harw_home::HomeError),
    /// Laden oder Validieren einer Config-Datei schlug fehl.
    Config(harw_config::ConfigError),
    /// Ein Dateisystemzugriff schlug fehl; `path` benennt das Ziel.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Eine TOML-Datei ist kein gültiges bzw. serialisierbares Dokument.
    Toml {
        path: PathBuf,
        reason: String,
    },
    /// Der angegebene Provider existiert nicht in der aufgelösten Konfiguration.
    ProviderNotFound {
        name: String,
    },
    /// Ein unbekannter Stellen-Schlüssel wurde an `harw models internal` übergeben.
    UnknownPoint {
        point: String,
        valid: String,
    },
    /// Der versiegelte Secret-Store konnte für eine Modellabfrage nicht geöffnet werden.
    SecretStore {
        reason: String,
    },
    InvalidModelTarget {
        target: String,
    },
    ModelNotLive {
        provider: String,
        model: String,
    },
    ProviderDisabled {
        provider: String,
    },
}

impl fmt::Display for ModelsError {
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
            Self::ProviderNotFound { name } => {
                write!(f, "Provider {name:?} ist nicht konfiguriert")
            }
            Self::UnknownPoint { point, valid } => write!(
                f,
                "unbekannte interne Modellstelle {point:?}; gültige Schlüssel: {valid}"
            ),
            Self::SecretStore { reason } => {
                write!(
                    f,
                    "Secret-Store für Modellabfrage nicht verfügbar: {reason}"
                )
            }
            Self::InvalidModelTarget { target } => {
                write!(f, "Modellziel muss `provider/modell` sein: {target:?}")
            }
            Self::ModelNotLive { provider, model } => write!(
                f,
                "Modell {provider}/{model} ist nicht live entdeckt; zuerst `harw models scan {provider}` ausführen"
            ),
            Self::ProviderDisabled { provider } => {
                write!(f, "Provider {provider:?} ist deaktiviert")
            }
        }
    }
}

impl fmt::Debug for ModelsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self}")
    }
}

impl StdError for ModelsError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Home(source) => Some(source),
            Self::Config(source) => Some(source),
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<harw_home::HomeError> for ModelsError {
    fn from(source: harw_home::HomeError) -> Self {
        Self::Home(source)
    }
}

impl From<harw_config::ConfigError> for ModelsError {
    fn from(source: harw_config::ConfigError) -> Self {
        Self::Config(source)
    }
}

/// Führt `harw models [action]` aus.
///
/// # Arguments
/// - `home_override` (`Option<PathBuf>`): expliziter `--home`-Wert; siehe
///   [`crate::home::resolve_home`].
/// - `action` (`Option<ModelsAction>`): Unterbefehl, oder `None` für `list`.
///
/// # Errors
/// `String` mit menschenlesbarem Kontext — konsistent mit den übrigen
/// `harw-cli`-Subcommand-Läufern (siehe `crate::settings::run`).
pub fn run(home_override: Option<PathBuf>, action: Option<ModelsAction>) -> Result<(), String> {
    // Der Katalog braucht kein angelegtes Home und läuft über denselben Weg
    // wie der frühere Befehl `harw catalog`.
    if let Some(ModelsAction::Catalog { refresh }) = action {
        return crate::lifecycle::catalog(home_override, refresh);
    }
    let home = crate::home::resolve_home(home_override)?;
    crate::home::ensure_home(&home).map_err(|error| error.to_string())?;
    execute(&home, action).map_err(|error| error.to_string())
}

/// Testbarer Kern von [`run`]: nimmt einen bereits aufgelösten Root-Space.
fn execute(home: &Path, action: Option<ModelsAction>) -> Result<(), ModelsError> {
    match action {
        None | Some(ModelsAction::List) => run_list(home),
        Some(ModelsAction::Scan {
            provider,
            add,
            free_only,
            prune,
        }) => run_scan(home, provider, add, free_only, prune),
        Some(ModelsAction::Add {
            target: Some(target),
        }) => add_model(home, &target),
        Some(ModelsAction::Add { target: None }) => run_model_picker(home),
        Some(ModelsAction::Remove { target }) => delete_model(home, &target),
        Some(ModelsAction::Internal { action }) => run_internal(home, action),
        Some(ModelsAction::Default { id }) => {
            set_default_model(home, &id)?;
            println!("Standardmodell auf {id:?} gesetzt.");
            Ok(())
        }
        // [`run`] fängt den Katalog vorher ab; dieser Zweig hält nur das
        // `match` vollständig.
        Some(ModelsAction::Catalog { refresh }) => {
            crate::lifecycle::catalog(Some(home.to_path_buf()), refresh).map_err(|reason| {
                ModelsError::Io {
                    path: home.join("cache"),
                    source: std::io::Error::other(reason),
                }
            })
        }
    }
}

// ---------------------------------------------------------------------
// Pfadauflösung / Config laden
// ---------------------------------------------------------------------

/// Lädt die aufgelöste Konfiguration und das aktive Profil-Verzeichnis.
fn load_config_and_profile(
    home: &Path,
) -> Result<(harw_config::ResolvedConfig, PathBuf), ModelsError> {
    let layers = harw_home::config_layers(home)?;
    let config = harw_config::discover_config(&layers)?;
    let profile_name = harw_home::active_profile_name(home);
    let profile = harw_home::profile_dir(home, &profile_name)?;
    Ok((config, profile))
}

/// `~/.harw/profiles/<aktives Profil>/config.toml` (globale Ebene).
fn global_config_path(home: &Path) -> Result<PathBuf, ModelsError> {
    let profile_name = harw_home::active_profile_name(home);
    let profile = harw_home::profile_dir(home, &profile_name)?;
    Ok(profile.join("config.toml"))
}

// ---------------------------------------------------------------------
// `harw models list` (und Vorgabe ohne Unterbefehl)
// ---------------------------------------------------------------------

fn run_list(home: &Path) -> Result<(), ModelsError> {
    let (config, _profile) = load_config_and_profile(home)?;
    let resolver = crate::secret_store::open_configured_secret_resolver(home, &config)
        .map_err(|reason| ModelsError::SecretStore { reason })?;
    let resolver = resolver
        .as_ref()
        .map(|resolver| resolver as &dyn harw_provider_http::SecretResolver);

    println!("== Provider ==");
    let mut provider_names: Vec<&String> = config.providers.keys().collect();
    provider_names.sort();
    if provider_names.is_empty() {
        println!("(keine Provider konfiguriert)");
    }
    for name in provider_names {
        let provider = &config.providers[name];
        let auth_ok = discovery::resolve_provider_api_key(name, provider, &config, Some(home), resolver)
                .is_some()
                || provider.auth_header.as_deref() == Some("none")
                // Runde 7, Teil L1: lokale Provider ohne `auth` brauchen keinen Schlüssel.
                || (provider.auth.is_none() && provider.is_local());
        println!(
            "{name}\tapi={}\thost={}\tauth={}\tenabled={}",
            provider.api,
            host_only(&provider.base_url),
            if auth_ok { "ok" } else { "fehlt" },
            provider.enabled
        );
    }

    println!("== Modelle ==");
    if config.models.is_empty() {
        println!("(keine Modelle konfiguriert)");
    }
    let mut by_provider: std::collections::BTreeMap<&str, Vec<&harw_config::ModelToml>> =
        std::collections::BTreeMap::new();
    for model in config.models.values() {
        by_provider
            .entry(model.provider.as_str())
            .or_default()
            .push(model);
    }
    for (provider_name, mut models) in by_provider {
        models.sort_by(|left, right| left.id.cmp(&right.id));
        println!("[{provider_name}]");
        for model in models {
            let is_default = config.harness.default_model.as_deref() == Some(model.id.as_str());
            println!(
                "  {}{}",
                model.id,
                if is_default { " [Standard]" } else { "" }
            );
        }
    }

    println!();
    print_internal_points(&config);
    Ok(())
}

/// Reduziert eine Basis-URL auf ihren Host-Anteil für die Übersichtsausgabe
/// (kein Pfad, keine Zugangsdaten).
fn host_only(base_url: &str) -> &str {
    let without_scheme = base_url.split("://").nth(1).unwrap_or(base_url);
    without_scheme.split('/').next().unwrap_or(without_scheme)
}

// ---------------------------------------------------------------------
// `harw models scan`
// ---------------------------------------------------------------------

fn run_scan(
    home: &Path,
    provider_filter: Option<String>,
    add: bool,
    free_only: bool,
    prune: bool,
) -> Result<(), ModelsError> {
    let (config, profile) = load_config_and_profile(home)?;
    let resolver = crate::secret_store::open_configured_secret_resolver(home, &config)
        .map_err(|reason| ModelsError::SecretStore { reason })?;
    let resolver = resolver
        .as_ref()
        .map(|resolver| resolver as &dyn harw_provider_http::SecretResolver);

    let mut targets: Vec<(&String, &harw_config::ProviderToml)> = match &provider_filter {
        Some(name) => {
            let provider = config
                .providers
                .get(name)
                .ok_or_else(|| ModelsError::ProviderNotFound { name: name.clone() })?;
            vec![(name, provider)]
        }
        None => config
            .providers
            .iter()
            .filter(|(_, provider)| provider.enabled)
            .collect(),
    };
    targets.sort_by(|left, right| left.0.cmp(right.0));

    if targets.is_empty() {
        println!("Keine aktivierten Provider konfiguriert.");
        return Ok(());
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| ModelsError::Io {
            path: PathBuf::from("<tokio-runtime>"),
            source,
        })?;

    let models_dir = profile.join("models");
    if add {
        println!("--add ist nicht mehr nötig: erfolgreiche Scans synchronisieren Modell-Dateien.");
    }
    for (name, provider) in targets {
        let api_key =
            discovery::resolve_provider_api_key(name, provider, &config, Some(home), resolver);
        match runtime.block_on(discovery::list_models(name, provider, api_key.as_deref())) {
            Ok(models) => {
                let filtered: Vec<DiscoveredModel> = if free_only {
                    models.into_iter().filter(is_free_model).collect()
                } else {
                    models
                };
                println!("{name}: verbunden ({} Modelle)", filtered.len());
                for model in &filtered {
                    print_discovered_model(model);
                }
                if filtered.is_empty() {
                    // Ein Provider, der 0 Modelle meldet, ist fast immer ein
                    // Auth-/Endpunkt-Problem (siehe Modul-Doku) — niemals als
                    // "der Live-Stand ist jetzt leer" interpretieren und
                    // dementsprechend nichts löschen.
                    println!("  0 Modelle gemeldet — nichts entfernt; Anmeldung/Endpunkt prüfen.");
                    add_catalog_fallback_models(&profile, name)?;
                    continue;
                }
                let protected = protected_model_ids(&config, name);
                sync_provider_model_list(&profile, name, &filtered, prune, &protected)?;
                sync_discovered_model_files(&models_dir, name, &filtered, prune, &protected)?;
            }
            Err(error) => println!("{name}: {error}"),
        }
    }
    Ok(())
}

/// Grund, warum `--prune` eine Modell-ID nicht löschen darf.
///
/// Handgeschrieben nach Projektkonvention (kein `anyhow`/`thiserror`);
/// [`fmt::Display`] liefert den in der CLI-Ausgabe verwendeten Text.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProtectionReason {
    /// Aktiv als `default_model`, `uia_model`, `uia_worker_model`,
    /// `session.title_model` oder eine interne Modellstelle
    /// (`internal_models.*`) in Verwendung.
    InUse,
    /// Von einem anderen geladenen Provider oder einer Agent-Definition
    /// referenziert, obwohl die Modell-ID formal einem anderen Provider
    /// zugeordnet ist (siehe Modul-Doku: `providers/foundry.toml` kann
    /// `openai/gpt-5.6-terra` referenzieren).
    Referenced(String),
}

impl fmt::Display for ProtectionReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtectionReason::InUse => write!(formatter, "in Verwendung"),
            ProtectionReason::Referenced(source) => {
                write!(formatter, "referenziert von {source}")
            }
        }
    }
}

/// Sammelt die Modell-IDs, die `--prune` niemals löschen darf — unabhängig
/// davon, ob der gerade gescannte Provider (`provider_name`) sie noch meldet.
///
/// Zwei Quellen werden zusammengeführt:
/// 1. **In Verwendung**: `default_model` (unter `default_provider`),
///    `uia_model` sowie `uia_worker_model` (beide unter `uia_provider`,
///    sonst `default_provider`), das veraltete `session.title_model`
///    (Fallback auf `default_provider`) sowie jede gesetzte interne
///    Modellstelle (`internal_models.*`, siehe
///    [`harw_config::InternalModelPoint::ALL`]).
/// 2. **Cross-Referenz**: jede Modell-ID, die ein *anderer* geladener
///    Provider in seiner eigenen `models`-Auswahl führt, oder die eine
///    geladene Agent-Definition (`AgentToml::models`) referenziert. Das
///    deckt den Fall ab, dass `providers/foundry.toml` `models =
///    ["gpt-5.6-terra"]` führt, obwohl `models/gpt-5.6-terra.toml` formal
///    dem Provider `openai` gehört — ein Prune von `openai` darf diese ID
///    nicht löschen, sonst bricht der Start an der hängenden Referenz.
///    `provider_name`s eigene Auswahlliste zählt hier bewusst nicht mit,
///    sonst würde Prune der Auswahlliste selbst nie greifen (siehe
///    `sync_provider_model_list`, das genau diese Liste durchläuft).
fn protected_model_ids(
    config: &harw_config::ResolvedConfig,
    provider_name: &str,
) -> BTreeMap<String, ProtectionReason> {
    let mut protected = BTreeMap::new();
    let harness = &config.harness;
    let default_provider = harness.default_provider.as_deref();

    if default_provider == Some(provider_name) {
        if let Some(id) = &harness.default_model {
            protected.insert(id.clone(), ProtectionReason::InUse);
        }
        if let Some(id) = &harness.session.title_model {
            protected.insert(id.clone(), ProtectionReason::InUse);
        }
    }

    let uia_provider = harness.uia_provider.as_deref().or(default_provider);
    if uia_provider == Some(provider_name) {
        if let Some(id) = &harness.uia_model {
            protected.insert(id.clone(), ProtectionReason::InUse);
        }
    }

    // uia_worker_model ist an uia_provider gekoppelt (dieselbe Or-Kette wie
    // beim uia_model-Block oben, nicht default_provider direkt): geschützt
    // wird die ID nur, wenn ihr im Katalog geführter Provider (der gerade
    // gescannte `provider_name`) mit dem effektiven `uia_provider`
    // übereinstimmt. Zeigt `uia_worker_model` inkonsistent auf ein anderes
    // Provider-Katalog-Modell, greift dieser Block nicht — das ist reine
    // Prune-Schutzlogik, die Ablehnung der Inkonsistenz selbst passiert an
    // anderer Stelle.
    if uia_provider == Some(provider_name) {
        if let Some(id) = &harness.uia_worker_model {
            protected.insert(id.clone(), ProtectionReason::InUse);
        }
    }

    for point in harw_config::InternalModelPoint::ALL {
        let Some(choice) = harness.internal_models.choice(point) else {
            continue;
        };
        let choice_provider = choice.provider.as_deref().or(default_provider);
        if choice_provider == Some(provider_name) {
            if let Some(id) = &choice.model {
                protected.insert(id.clone(), ProtectionReason::InUse);
            }
        }
    }

    let mut other_provider_names: Vec<&String> = config.providers.keys().collect();
    other_provider_names.sort();
    for name in other_provider_names {
        if name == provider_name {
            continue;
        }
        for id in &config.providers[name].models {
            protected
                .entry(id.clone())
                .or_insert_with(|| ProtectionReason::Referenced(name.clone()));
        }
    }

    let mut agent_names: Vec<&String> = config.agents.keys().collect();
    agent_names.sort();
    for name in agent_names {
        for id in &config.agents[name].models {
            protected
                .entry(id.clone())
                .or_insert_with(|| ProtectionReason::Referenced(format!("{name} (Agent)")));
        }
    }

    protected
}

/// Ohne `prune`: lässt die TUI-Auswahl unverändert und meldet nur, welche
/// bereits gewählten IDs der Provider nicht mehr führt ("nicht mehr
/// gemeldet"). Mit `prune`: entfernt genau diese nicht mehr gemeldeten IDs —
/// außer sie stehen in `protected` (dann "behalten (in Verwendung)"). Neue
/// Live-Modelle werden nie automatisch gewählt; dafür dienen `harw models
/// add` und der Picker. Bei leerem `discovered` (sollte den Aufrufer nie
/// erreichen, siehe [`run_scan`]) wird zur Sicherheit ebenfalls nichts
/// verändert.
fn sync_provider_model_list(
    profile: &Path,
    provider_name: &str,
    discovered: &[DiscoveredModel],
    prune: bool,
    protected: &BTreeMap<String, ProtectionReason>,
) -> Result<(), ModelsError> {
    if discovered.is_empty() {
        return Ok(());
    }
    if !provider_name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(ModelsError::Toml {
            path: profile.join("providers"),
            reason: "ungültiger Provider-Name für Modell-Synchronisation".to_owned(),
        });
    }
    let path = profile
        .join("providers")
        .join(format!("{provider_name}.toml"));
    let document = open_document(&path)?;
    let provider: harw_config::ProviderToml =
        toml::from_str(&document.to_string()).map_err(|error| ModelsError::Toml {
            path: path.clone(),
            reason: error.to_string(),
        })?;
    let live = discovered
        .iter()
        .map(|model| model.id.as_str())
        .collect::<BTreeSet<_>>();

    if !prune {
        for id in &provider.models {
            if !live.contains(id.as_str()) {
                println!("  nicht mehr gemeldet (Auswahl): {id}");
            }
        }
        return Ok(());
    }

    let mut ids = BTreeSet::new();
    for id in &provider.models {
        if live.contains(id.as_str()) {
            ids.insert(id.clone());
        } else if let Some(reason) = protected.get(id.as_str()) {
            println!("  behalten ({reason}): {id}");
            ids.insert(id.clone());
        } else {
            println!("  entfernt (Auswahl): {id}");
        }
    }
    write_provider_model_list(&path, ids)
}

fn write_provider_model_list(
    path: &Path,
    ids: impl IntoIterator<Item = String>,
) -> Result<(), ModelsError> {
    let mut document = open_document(path)?;
    let mut models = toml_edit::Array::new();
    for id in ids.into_iter().collect::<BTreeSet<_>>() {
        models.push(id);
    }
    document["models"] = toml_edit::value(models);
    write_atomic(path, document.to_string().as_bytes())
}

fn parse_model_target(target: &str) -> Result<(&str, &str), ModelsError> {
    let Some((provider, model)) = target.split_once('/') else {
        return Err(ModelsError::InvalidModelTarget {
            target: target.to_owned(),
        });
    };
    if provider.is_empty() || model.is_empty() {
        return Err(ModelsError::InvalidModelTarget {
            target: target.to_owned(),
        });
    }
    Ok((provider, model))
}

fn provider_path(profile: &Path, provider: &str) -> PathBuf {
    profile.join("providers").join(format!("{provider}.toml"))
}

/// `true`, wenn `base_url` erkennbar ein unkonfigurierter Platzhalter ist
/// (z. B. `cf-worker`: `https://<dein-worker>.example/v1`, `custom`:
/// `https://example.invalid/v1`, siehe `harw-model-catalog/src/providers.toml`).
/// Für einen solchen Endpunkt gibt es keinen echten Live-Host, gegen den ein
/// Katalog-Fallback sinnvoll wäre.
fn has_placeholder_base_url(base_url: &str) -> bool {
    base_url.contains('<') || base_url.contains(".example/") || base_url.contains("example.invalid")
}

/// Ergänzt nach einem Scan mit 0 gemeldeten Live-Modellen (siehe [`run_scan`])
/// die Provider-Auswahl (`providers/<name>.toml`, Feld `models`) additiv um
/// die im eingebetteten Katalog (`harw_model_catalog::embedded_catalog`)
/// geführten Modell-IDs dieses Providers, sofern sie dort noch nicht
/// enthalten sind. Löscht niemals etwas. Ohne passenden Katalog-Eintrag oder
/// bei einem erkennbaren Platzhalter-`base_url` (siehe
/// [`has_placeholder_base_url`]) passiert nichts.
fn add_catalog_fallback_models(profile: &Path, provider_name: &str) -> Result<(), ModelsError> {
    let Some(catalog_entry) = harw_model_catalog::embedded_catalog()
        .into_iter()
        .find(|entry| entry.id == provider_name)
    else {
        return Ok(());
    };

    let path = provider_path(profile, provider_name);
    let document = open_document(&path)?;
    let provider: harw_config::ProviderToml =
        toml::from_str(&document.to_string()).map_err(|error| ModelsError::Toml {
            path: path.clone(),
            reason: error.to_string(),
        })?;
    if has_placeholder_base_url(&provider.base_url) {
        return Ok(());
    }

    let mut ids = provider.models.iter().cloned().collect::<BTreeSet<_>>();
    let mut added_any = false;
    for id in &catalog_entry.models {
        if ids.insert(id.clone()) {
            println!("  hinzugefügt (Katalog, nicht live bestätigt): {id}");
            added_any = true;
        }
    }
    if added_any {
        write_provider_model_list(&path, ids)?;
    }
    Ok(())
}

fn add_model(home: &Path, target: &str) -> Result<(), ModelsError> {
    let (provider_name, model_id) = parse_model_target(target)?;
    let (config, profile) = load_config_and_profile(home)?;
    let provider =
        config
            .providers
            .get(provider_name)
            .ok_or_else(|| ModelsError::ProviderNotFound {
                name: provider_name.to_owned(),
            })?;
    if !provider.enabled {
        return Err(ModelsError::ProviderDisabled {
            provider: provider_name.to_owned(),
        });
    }
    if !config
        .models
        .values()
        .any(|model| model.provider == provider_name && model.id == model_id)
    {
        return Err(ModelsError::ModelNotLive {
            provider: provider_name.to_owned(),
            model: model_id.to_owned(),
        });
    }
    let mut selected = provider.models.iter().cloned().collect::<BTreeSet<_>>();
    selected.insert(model_id.to_owned());
    write_provider_model_list(&provider_path(&profile, provider_name), selected)?;
    println!("{provider_name}/{model_id} hinzugefügt.");
    Ok(())
}

fn delete_model(home: &Path, target: &str) -> Result<(), ModelsError> {
    let (provider_name, model_id) = parse_model_target(target)?;
    let (config, profile) = load_config_and_profile(home)?;
    let provider =
        config
            .providers
            .get(provider_name)
            .ok_or_else(|| ModelsError::ProviderNotFound {
                name: provider_name.to_owned(),
            })?;
    let mut selected = provider.models.iter().cloned().collect::<BTreeSet<_>>();
    selected.remove(model_id);
    write_provider_model_list(&provider_path(&profile, provider_name), selected)?;
    let model_path = profile.join("models").join(model_filename(model_id));
    if let Ok(metadata) = fs::symlink_metadata(&model_path) {
        if metadata.file_type().is_file() && !metadata.file_type().is_symlink() {
            fs::remove_file(&model_path).map_err(|source| ModelsError::Io {
                path: model_path,
                source,
            })?;
        }
    }
    println!("{provider_name}/{model_id} entfernt.");
    Ok(())
}

fn run_model_picker(home: &Path) -> Result<(), ModelsError> {
    let (config, profile) = load_config_and_profile(home)?;
    let mut providers = config
        .providers
        .iter()
        .filter(|(_, provider)| provider.enabled)
        .map(|(name, provider)| {
            let mut models = config
                .models
                .values()
                .filter(|model| model.provider == *name)
                .map(|model| model.id.clone())
                .collect::<Vec<_>>();
            if models.is_empty() {
                models = provider.models.clone();
            }
            harw_tui::ModelPickerProvider {
                id: name.clone(),
                models,
                selected: provider.models.iter().cloned().collect(),
            }
        })
        .collect::<Vec<_>>();
    providers.sort_by(|left, right| left.id.cmp(&right.id));
    if providers.is_empty() {
        println!("Keine aktivierten Provider konfiguriert.");
        return Ok(());
    }
    if let Some(outcome) =
        harw_tui::run_model_picker(providers).map_err(|error| ModelsError::Toml {
            path: profile.join("providers"),
            reason: error.to_string(),
        })?
    {
        let count = outcome.models.len();
        write_provider_model_list(&provider_path(&profile, &outcome.provider), outcome.models)?;
        println!("{}: {count} Modelle gewählt.", outcome.provider);
    }
    Ok(())
}

/// `true`, wenn `model` als kostenlos gilt: `:free`-Suffix in der ID, oder
/// mindestens ein gemeldeter Preis von `0`.
fn is_free_model(model: &DiscoveredModel) -> bool {
    model.id.ends_with(":free")
        || model.input_price_per_mtok.is_some_and(|price| price <= 0.0)
        || model
            .output_price_per_mtok
            .is_some_and(|price| price <= 0.0)
}

fn print_discovered_model(model: &DiscoveredModel) {
    let context = model
        .context_length
        .map(|ctx| format!(" ctx={ctx}"))
        .unwrap_or_default();
    let price = match (model.input_price_per_mtok, model.output_price_per_mtok) {
        (Some(input), Some(output)) => format!(" in=${input:.2}/Mtok out=${output:.2}/Mtok"),
        _ => String::new(),
    };
    let tools = match model.supports_tools {
        Some(true) => " tools=ja",
        Some(false) => " tools=nein",
        None => "",
    };
    println!("  {}{context}{price}{tools}", model.id);
}

/// Synchronisiert die Modell-Dateien eines Providers mit dessen erfolgreicher
/// Live-Antwort. Vorhandene und neue IDs erhalten immer (auch ohne `prune`)
/// eine frisch gerenderte Datei — das ist das "ADD". Ohne `prune` bleiben
/// nicht mehr gemeldete Dateien unangetastet und werden nur als "nicht mehr
/// gemeldet" ausgegeben. Mit `prune` werden sie gelöscht, außer ihre
/// Modell-ID steht in `protected` (dann "behalten (in Verwendung)"). Fremde
/// Provider, nicht lesbare TOML-Dateien und Symlinks bleiben absichtlich
/// unangetastet. Bei leerem `discovered` (sollte den Aufrufer nie erreichen,
/// siehe [`run_scan`]) wird zur Sicherheit ebenfalls nichts verändert.
fn sync_discovered_model_files(
    models_dir: &Path,
    provider_name: &str,
    discovered: &[DiscoveredModel],
    prune: bool,
    protected: &BTreeMap<String, ProtectionReason>,
) -> Result<(), ModelsError> {
    if discovered.is_empty() {
        return Ok(());
    }
    let live: BTreeMap<&str, &DiscoveredModel> = discovered
        .iter()
        .map(|model| (model.id.as_str(), model))
        .collect();
    let mut existing = BTreeMap::<String, PathBuf>::new();

    match fs::read_dir(models_dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|source| ModelsError::Io {
                    path: models_dir.to_path_buf(),
                    source,
                })?;
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
                    continue;
                }
                let file_type = entry.file_type().map_err(|source| ModelsError::Io {
                    path: path.clone(),
                    source,
                })?;
                if file_type.is_symlink() {
                    println!("  übersprungen (Symlink): {}", path.display());
                    continue;
                }
                let raw = fs::read_to_string(&path).map_err(|source| ModelsError::Io {
                    path: path.clone(),
                    source,
                })?;
                let model: harw_config::ModelToml = match toml::from_str(&raw) {
                    Ok(model) => model,
                    Err(_) => {
                        println!(
                            "  übersprungen (ungültiges Modell-TOML): {}",
                            path.display()
                        );
                        continue;
                    }
                };
                if model.provider != provider_name {
                    continue;
                }
                if live.contains_key(model.id.as_str()) {
                    existing.entry(model.id).or_insert(path);
                } else if !prune {
                    println!("  nicht mehr gemeldet: {}", path.display());
                } else if let Some(reason) = protected.get(model.id.as_str()) {
                    println!("  behalten ({reason}): {}", path.display());
                } else {
                    fs::remove_file(&path).map_err(|source| ModelsError::Io {
                        path: path.clone(),
                        source,
                    })?;
                    println!("  entfernt: {}", path.display());
                }
            }
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(ModelsError::Io {
                path: models_dir.to_path_buf(),
                source,
            });
        }
    }

    for model in live.values() {
        let path = existing
            .get(model.id.as_str())
            .cloned()
            .unwrap_or_else(|| models_dir.join(model_filename(&model.id)));
        write_discovered_model_file(&path, provider_name, model)?;
    }
    Ok(())
}

/// Rendert einen durch einen erfolgreichen Provider-Scan bestätigten
/// Modell-Eintrag atomar an seinen Zielpfad.
fn write_discovered_model_file(
    path: &Path,
    provider_name: &str,
    model: &DiscoveredModel,
) -> Result<(), ModelsError> {
    let models_dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(models_dir).map_err(|source| ModelsError::Io {
        path: models_dir.to_path_buf(),
        source,
    })?;
    // Runde 7, Teil L3: Meldet der Server kein Kontextfenster (bzw. keine
    // Werkzeug-Auskunft), bleibt ein von Hand eingetragener Wert der
    // bisherigen Datei erhalten, statt beim erneuten Scan gelöscht zu werden.
    let previous = fs::read_to_string(path)
        .ok()
        .and_then(|content| toml::from_str::<harw_config::ModelToml>(&content).ok());
    let context_window = model.context_length.or_else(|| {
        previous
            .as_ref()
            .and_then(|previous| previous.context_window)
    });
    let tool_calling = model.supports_tools.or_else(|| {
        previous
            .as_ref()
            .and_then(|previous| previous.capabilities.tool_calling)
    });
    let toml_model = harw_config::ModelToml {
        stream: None,
        rate_limit: None,
        id: model.id.clone(),
        name: None,
        provider: provider_name.to_owned(),
        aliases: Vec::new(),
        context_window,
        max_tokens: None,
        prompt_caching: None,
        reasoning: false,
        input_types: Vec::new(),
        capabilities: harw_config::ModelCapabilitiesToml {
            tool_use: model.supports_tools.unwrap_or(false),
            streaming: false,
            vision: false,
            json_mode: false,
            // Runde 7, Teil L7: `supported_parameters` ohne `"tools"` →
            // `tool_calling = false`; der Provider bietet dann keine Werkzeuge an.
            tool_calling,
            // Unknown: images are sent and a model without image input answers
            // with a visible server error (see `ModelCapabilitiesToml::image_input`).
            image_input: None,
        },
        default_reasoning_effort: None,
    };
    let rendered = toml::to_string_pretty(&toml_model).map_err(|error| ModelsError::Toml {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    write_atomic(path, rendered.as_bytes())
}

/// Encode API identifiers as a single collision-free filename component.
/// Minimal nachgebaut aus `crate::onboarding::model_filename` (siehe
/// Modul-Doku): `onboarding.rs` gehört einem anderen Slice-Eigentümer.
fn model_filename(id: &str) -> String {
    let mut name = String::new();
    for byte in id.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.') {
            name.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            let _ = write!(name, "%{byte:02X}");
        }
    }
    name.push_str(".toml");
    name
}

// ---------------------------------------------------------------------
// `harw models internal …`
// ---------------------------------------------------------------------

fn run_internal(home: &Path, action: Option<InternalAction>) -> Result<(), ModelsError> {
    match action {
        None | Some(InternalAction::Show) => {
            let (config, _profile) = load_config_and_profile(home)?;
            print_internal_points(&config);
            Ok(())
        }
        Some(InternalAction::Set {
            point,
            model,
            provider,
        }) => {
            let point = parse_point(&point)?;
            set_internal_choice(home, point, &model, provider.as_deref())?;
            println!("{}: Modell auf {model:?} gesetzt.", point.key());
            Ok(())
        }
        Some(InternalAction::Main { point }) => {
            let point = parse_point(&point)?;
            set_internal_main(home, point)?;
            println!("{}: erzwingt das Hauptmodell.", point.key());
            Ok(())
        }
        Some(InternalAction::Reset { point }) => {
            let point = parse_point(&point)?;
            reset_internal_choice(home, point)?;
            println!("{}: explizite Wahl entfernt.", point.key());
            Ok(())
        }
        Some(InternalAction::OpenrouterDefaults { state }) => {
            let enabled = state == OnOff::On;
            set_openrouter_defaults(home, enabled)?;
            println!("use_openrouter_defaults = {enabled}");
            Ok(())
        }
    }
}

/// Parst einen Stellen-Schlüssel; der Fehler listet alle gültigen Schlüssel.
fn parse_point(raw: &str) -> Result<harw_config::InternalModelPoint, ModelsError> {
    harw_config::InternalModelPoint::parse(raw).ok_or_else(|| ModelsError::UnknownPoint {
        point: raw.to_owned(),
        valid: harw_config::InternalModelPoint::ALL
            .iter()
            .map(|point| point.key())
            .collect::<Vec<_>>()
            .join(", "),
    })
}

/// Druckt jede interne Modellstelle mit ihrer effektiven Auflösung
/// (`harw_config::resolve_internal_model`) und Quelle; verwendet von
/// `harw models list` und `harw models internal show`.
fn print_internal_points(config: &harw_config::ResolvedConfig) {
    println!("== Interne Modellstellen ==");
    for point in harw_config::InternalModelPoint::ALL {
        let resolved = harw_config::resolve_internal_model(config, point);
        let target = if resolved.is_main_model() {
            "Hauptmodell".to_owned()
        } else {
            format!(
                "{}/{}",
                resolved.provider.as_deref().unwrap_or("?"),
                resolved.model.as_deref().unwrap_or("?")
            )
        };
        let source = match resolved.source {
            harw_config::InternalModelSource::Explicit => "explizit",
            harw_config::InternalModelSource::OpenRouterDefault => "OpenRouter-Standard",
            harw_config::InternalModelSource::MainModel => "Hauptmodell",
        };
        println!(
            "{}\t{}\t{target}\tquelle={source}",
            point.key(),
            point.description()
        );
    }
    if !harw_config::openrouter_available(config) {
        println!(
            "Tipp: `harw settings provider add openrouter` bzw. Einrichtung erneut ausführen, \
um günstige NVIDIA-Nemotron-Modelle für interne Aufgaben zu nutzen."
        );
    }
}

/// Setzt `[internal_models.<point>] model = …` (und optional `provider`).
fn set_internal_choice(
    home: &Path,
    point: harw_config::InternalModelPoint,
    model: &str,
    provider: Option<&str>,
) -> Result<(), ModelsError> {
    mutate_internal_models_table(home, |table| {
        let sub = ensure_table(table, point.key())?;
        sub.insert("model", value(model));
        if let Some(provider) = provider {
            sub.insert("provider", value(provider));
        } else {
            sub.remove("provider");
        }
        Ok(())
    })
}

/// Erzwingt das Hauptmodell für `point`: legt eine leere
/// `[internal_models.<point>]`-Tabelle an (`model`/`provider` entfernt),
/// die `harw_config::resolve_internal_model` als `MainModel` liest.
fn set_internal_main(
    home: &Path,
    point: harw_config::InternalModelPoint,
) -> Result<(), ModelsError> {
    mutate_internal_models_table(home, |table| {
        let sub = ensure_table(table, point.key())?;
        sub.remove("model");
        sub.remove("provider");
        Ok(())
    })
}

/// Entfernt `[internal_models.<point>]` vollständig; die Auflösung fällt auf
/// den OpenRouter-Standard bzw. das Hauptmodell zurück.
fn reset_internal_choice(
    home: &Path,
    point: harw_config::InternalModelPoint,
) -> Result<(), ModelsError> {
    mutate_internal_models_table(home, |table| {
        table.remove(point.key());
        Ok(())
    })
}

/// Setzt `[internal_models] use_openrouter_defaults = true|false`.
fn set_openrouter_defaults(home: &Path, enabled: bool) -> Result<(), ModelsError> {
    mutate_internal_models_table(home, |table| {
        table.insert("use_openrouter_defaults", value(enabled));
        Ok(())
    })
}

/// Öffnet die globale Profil-`config.toml` als `toml_edit`-Dokument, stellt
/// sicher, dass `[internal_models]` eine Tabelle ist, übergibt sie an
/// `mutate` und schreibt das Ergebnis atomar zurück.
///
/// `harw_config::ConfigWriter` bietet kein `remove_value` für beliebige
/// Pfade (siehe Moduldoku); diese Funktion editiert das Dokument daher
/// direkt, exakt wie `ConfigWriter::set_value`/`ensure_table` intern
/// arbeiten (`harw-config/src/writer.rs`), nur ohne die dortige
/// `[permissions]`-spezifische Validierung — hier unnötig, da
/// `[internal_models]` kein Feld von `PermissionsSection` berührt.
fn mutate_internal_models_table(
    home: &Path,
    mutate: impl FnOnce(&mut toml_edit::Table) -> Result<(), ModelsError>,
) -> Result<(), ModelsError> {
    let path = global_config_path(home)?;
    let mut doc = open_document(&path)?;
    let root = doc.as_table_mut();
    let table = ensure_table(root, "internal_models")?;
    mutate(table)?;
    write_atomic(&path, doc.to_string().as_bytes())
}

/// Liest `path` als `toml_edit::DocumentMut`; eine fehlende Datei wird als
/// leeres Dokument behandelt (wie `ConfigWriter::open`).
fn open_document(path: &Path) -> Result<toml_edit::DocumentMut, ModelsError> {
    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(ModelsError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    content
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| ModelsError::Toml {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })
}

/// Stellt sicher, dass `table[key]` eine Tabelle ist, und gibt eine
/// veränderliche Referenz darauf zurück. Minimal nachgebaut aus
/// `harw_config::writer::ensure_table` (privat in der Schwester-Crate).
///
/// # Errors
/// [`ModelsError::Config`] mit [`harw_config::ConfigError::WriterShapeMismatch`],
/// falls `table[key]` unmittelbar nach dem Normalisieren auf eine Tabelle
/// dennoch nicht als Tabelle gelesen werden kann (interner Zustandsfehler).
fn ensure_table<'a>(
    table: &'a mut toml_edit::Table,
    key: &str,
) -> Result<&'a mut toml_edit::Table, ModelsError> {
    if !matches!(table.get(key), Some(item) if item.is_table()) {
        table.insert(key, toml_edit::Item::Table(toml_edit::Table::new()));
    }
    table
        .get_mut(key)
        .and_then(toml_edit::Item::as_table_mut)
        .ok_or_else(|| {
            ModelsError::from(harw_config::ConfigError::WriterShapeMismatch {
                key: key.to_owned(),
                expected: "table",
            })
        })
}

// ---------------------------------------------------------------------
// `harw models default …` — derselbe Schreibpfad wie `harw settings model default`.
// ---------------------------------------------------------------------

/// Setzt `default_model` in der globalen Profil-Config. Minimal nachgebaut
/// aus `crate::settings::set_default_model` (siehe Moduldoku): dasselbe
/// Verhalten, ohne einen privaten Helfer des Schwestermoduls zu importieren.
fn set_default_model(home: &Path, id: &str) -> Result<(), ModelsError> {
    let path = global_config_path(home)?;
    let mut writer = ConfigWriter::open(&path)?;
    writer.set_value("default_model", value(id))?;
    writer.save()?;
    Ok(())
}

// ---------------------------------------------------------------------
// Atomares Schreiben (siehe Modul-Doku).
// ---------------------------------------------------------------------

/// Schreibt `content` atomar nach `path`: ein `0600`-Temp-Nachbar wird
/// angelegt, geschrieben, synchronisiert und über `path` umbenannt. Minimal
/// nachgebaut aus `crate::settings::write_atomic`/`crate::onboarding::write_file`
/// (siehe Modul-Doku).
fn write_atomic(path: &Path, content: &[u8]) -> Result<(), ModelsError> {
    let parent = path.parent().ok_or_else(|| ModelsError::Io {
        path: path.to_path_buf(),
        source: std::io::Error::other("Zielpfad hat kein Elternverzeichnis"),
    })?;
    std::fs::create_dir_all(parent).map_err(|source| ModelsError::Io {
        path: parent.to_path_buf(),
        source,
    })?;

    let temp_path = parent.join(format!(
        ".models-{}-{}.tmp",
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
        return Err(ModelsError::Io {
            path: temp_path,
            source,
        });
    }
    std::fs::rename(&temp_path, path).map_err(|source| {
        let _ = std::fs::remove_file(&temp_path);
        ModelsError::Io {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn temp_home() -> TestResult<(tempfile::TempDir, PathBuf)> {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let home = dir.path().join("harw-home");
        harw_home::ensure_home(&home).map_err(ctx("ensure_home"))?;
        Ok((dir, home))
    }

    /// Runde 7, Teil L3/L7: `harw provider scan` schreibt das gemeldete
    /// Kontextfenster und `tool_calling`; ohne neue Angabe bleibt ein
    /// vorhandener Wert erhalten.
    #[test]
    fn test_scan_writes_context_window_and_keeps_manual_value() -> TestResult {
        let dir = tempfile::tempdir().map_err(ctx("tempdir"))?;
        let path = dir.path().join("models").join("qwen3-8b.toml");
        let scanned = DiscoveredModel {
            id: "qwen3-8b".to_owned(),
            context_length: Some(32_768),
            input_price_per_mtok: None,
            output_price_per_mtok: None,
            supports_tools: Some(false),
        };
        write_discovered_model_file(&path, "lmstudio", &scanned).map_err(ctx("first scan"))?;
        let written: harw_config::ModelToml =
            toml::from_str(&fs::read_to_string(&path).map_err(ctx("read model"))?)
                .map_err(ctx("parse model"))?;
        assert_eq!(written.context_window, Some(32_768));
        assert_eq!(written.capabilities.tool_calling, Some(false));

        let rescanned = DiscoveredModel {
            context_length: None,
            supports_tools: None,
            ..scanned
        };
        write_discovered_model_file(&path, "lmstudio", &rescanned).map_err(ctx("rescan"))?;
        let kept: harw_config::ModelToml =
            toml::from_str(&fs::read_to_string(&path).map_err(ctx("reread model"))?)
                .map_err(ctx("reparse model"))?;
        assert_eq!(kept.context_window, Some(32_768));
        assert_eq!(kept.capabilities.tool_calling, Some(false));
        Ok(())
    }

    #[test]
    fn test_host_only_strips_scheme_and_path() {
        assert_eq!(host_only("https://api.acme.test/v1"), "api.acme.test");
        assert_eq!(host_only("http://localhost:11434"), "localhost:11434");
    }

    #[test]
    fn test_model_filename_percent_encodes_slashes() {
        assert_ne!(model_filename("a/b"), model_filename("a%2Fb"));
        assert!(!model_filename("../../escape").contains('/'));
        assert!(model_filename("nvidia/nemotron-3.5-lightning").ends_with(".toml"));
    }

    #[test]
    fn add_then_delete_changes_only_the_selected_provider_models() -> TestResult {
        let (_guard, home) = temp_home()?;
        let profile = home.join("profiles/default");
        let providers = profile.join("providers");
        let models = profile.join("models");
        std::fs::create_dir_all(&providers).map_err(ctx("providers dir"))?;
        std::fs::create_dir_all(&models).map_err(ctx("models dir"))?;
        std::fs::write(
            providers.join("acme.toml"),
            "name = \"acme\"\napi = \"openai-chat\"\nbase_url = \"https://api.example.test/v1\"\nauth = \"env:ACME_TOKEN\"\nenabled = true\nmodels = []\n",
        )
        .map_err(ctx("provider"))?;
        let live = harw_config::ModelToml {
            stream: None,
            rate_limit: None,
            id: "model/with-slash".to_owned(),
            name: None,
            provider: "acme".to_owned(),
            aliases: Vec::new(),
            context_window: None,
            max_tokens: None,
            prompt_caching: None,
            reasoning: false,
            input_types: Vec::new(),
            capabilities: harw_config::ModelCapabilitiesToml::default(),
            default_reasoning_effort: None,
        };
        let cache_path = models.join(model_filename(&live.id));
        std::fs::write(
            &cache_path,
            toml::to_string(&live).map_err(ctx("serialize model"))?,
        )
        .map_err(ctx("model cache"))?;

        add_model(&home, "acme/model/with-slash").map_err(ctx("add live model"))?;
        let provider: harw_config::ProviderToml = toml::from_str(
            &std::fs::read_to_string(providers.join("acme.toml")).map_err(ctx("read provider"))?,
        )
        .map_err(ctx("parse provider"))?;
        assert_eq!(provider.models, ["model/with-slash"]);

        delete_model(&home, "acme/model/with-slash").map_err(ctx("delete model"))?;
        let provider: harw_config::ProviderToml = toml::from_str(
            &std::fs::read_to_string(providers.join("acme.toml")).map_err(ctx("read provider"))?,
        )
        .map_err(ctx("parse provider"))?;
        assert!(provider.models.is_empty());
        assert!(!cache_path.exists());
        Ok(())
    }

    #[test]
    fn test_is_free_model_matches_free_suffix_and_zero_price() {
        let free_suffix = DiscoveredModel {
            id: "nvidia/nemotron-3.5-lightning:free".to_owned(),
            context_length: None,
            input_price_per_mtok: None,
            output_price_per_mtok: None,
            supports_tools: None,
        };
        assert!(is_free_model(&free_suffix));

        let zero_price = DiscoveredModel {
            id: "acme/free-tier".to_owned(),
            context_length: None,
            input_price_per_mtok: Some(0.0),
            output_price_per_mtok: Some(0.0),
            supports_tools: None,
        };
        assert!(is_free_model(&zero_price));

        let paid = DiscoveredModel {
            id: "acme/paid".to_owned(),
            context_length: None,
            input_price_per_mtok: Some(1.5),
            output_price_per_mtok: Some(3.0),
            supports_tools: None,
        };
        assert!(!is_free_model(&paid));
    }

    #[test]
    fn scan_sync_replaces_live_models_and_removes_only_missing_provider_models() -> TestResult {
        let (_guard, home) = temp_home()?;
        let models_dir = home.join("profiles/default/models");
        std::fs::create_dir_all(&models_dir).map_err(ctx("models dir"))?;
        let old = harw_config::ModelToml {
            stream: None,
            rate_limit: None,
            id: "gone".to_owned(),
            name: None,
            provider: "acme".to_owned(),
            aliases: Vec::new(),
            context_window: None,
            max_tokens: None,
            prompt_caching: None,
            reasoning: false,
            input_types: Vec::new(),
            capabilities: harw_config::ModelCapabilitiesToml::default(),
            default_reasoning_effort: None,
        };
        let other = harw_config::ModelToml {
            stream: None,
            rate_limit: None,
            provider: "other".to_owned(),
            ..old.clone()
        };
        std::fs::write(
            models_dir.join("gone.toml"),
            toml::to_string(&old).map_err(ctx("serialize stale model"))?,
        )
        .map_err(ctx("write stale model"))?;
        std::fs::write(
            models_dir.join("other.toml"),
            toml::to_string(&other).map_err(ctx("serialize other provider model"))?,
        )
        .map_err(ctx("write other provider model"))?;

        let live = DiscoveredModel {
            id: "current".to_owned(),
            context_length: Some(262_144),
            input_price_per_mtok: None,
            output_price_per_mtok: None,
            supports_tools: Some(true),
        };
        sync_discovered_model_files(&models_dir, "acme", &[live], true, &BTreeMap::new())
            .map_err(ctx("sync models"))?;

        assert!(!models_dir.join("gone.toml").exists());
        assert!(models_dir.join("other.toml").exists());
        let current: harw_config::ModelToml = toml::from_str(
            &std::fs::read_to_string(models_dir.join("current.toml"))
                .map_err(ctx("read current"))?,
        )
        .map_err(ctx("parse current"))?;
        assert_eq!(current.provider, "acme");
        assert_eq!(current.context_window, Some(262_144));
        assert!(current.capabilities.tool_use);
        Ok(())
    }

    #[test]
    fn scan_sync_updates_the_tui_provider_model_list_without_touching_auth() -> TestResult {
        let (_guard, home) = temp_home()?;
        let profile = home.join("profiles/default");
        let providers = profile.join("providers");
        std::fs::create_dir_all(&providers).map_err(ctx("providers dir"))?;
        let path = providers.join("acme.toml");
        std::fs::write(
            &path,
            "# keep this comment\nname = \"acme\"\napi = \"openai-chat\"\nbase_url = \"https://api.example.test/v1\"\nauth = \"file:/home/test/.harw/secrets/acme.key\"\nmodels = [\"stale\", \"zeta\"]\n",
        )
        .map_err(ctx("write provider"))?;
        let models = [
            DiscoveredModel {
                id: "zeta".to_owned(),
                context_length: None,
                input_price_per_mtok: None,
                output_price_per_mtok: None,
                supports_tools: None,
            },
            DiscoveredModel {
                id: "alpha".to_owned(),
                context_length: None,
                input_price_per_mtok: None,
                output_price_per_mtok: None,
                supports_tools: None,
            },
        ];

        sync_provider_model_list(&profile, "acme", &models, true, &BTreeMap::new())
            .map_err(ctx("sync provider list"))?;

        let content = std::fs::read_to_string(&path).map_err(ctx("read provider"))?;
        assert!(content.contains("# keep this comment"));
        assert!(content.contains("auth = \"file:/home/test/.harw/secrets/acme.key\""));
        let provider: harw_config::ProviderToml =
            toml::from_str(&content).map_err(ctx("parse provider"))?;
        assert_eq!(provider.models, ["zeta"]);
        Ok(())
    }

    /// Baut das Provider-Datei-Fixture für die `--prune`-Regressionstests:
    /// eine `models`-Auswahl mit einem veralteten und einem noch verwendeten
    /// (protected) Eintrag, plus die passenden `models/*.toml`-Caches.
    fn prune_fixture(home: &Path) -> TestResult<(PathBuf, PathBuf, PathBuf, PathBuf)> {
        let profile = home.join("profiles/default");
        let providers = profile.join("providers");
        let models_dir = profile.join("models");
        std::fs::create_dir_all(&providers).map_err(ctx("providers dir"))?;
        std::fs::create_dir_all(&models_dir).map_err(ctx("models dir"))?;
        let provider_path = providers.join("acme.toml");
        std::fs::write(
            &provider_path,
            "name = \"acme\"\napi = \"openai-chat\"\nbase_url = \"https://api.example.test/v1\"\nauth = \"env:ACME_TOKEN\"\nenabled = true\nmodels = [\"stale-model\", \"gpt-5.6-terra\"]\n",
        )
        .map_err(ctx("write provider"))?;

        let stale = harw_config::ModelToml {
            stream: None,
            rate_limit: None,
            id: "stale-model".to_owned(),
            name: None,
            provider: "acme".to_owned(),
            aliases: Vec::new(),
            context_window: None,
            max_tokens: None,
            prompt_caching: None,
            reasoning: false,
            input_types: Vec::new(),
            capabilities: harw_config::ModelCapabilitiesToml::default(),
            default_reasoning_effort: None,
        };
        let default_model = harw_config::ModelToml {
            stream: None,
            rate_limit: None,
            id: "gpt-5.6-terra".to_owned(),
            ..stale.clone()
        };
        let stale_path = models_dir.join(model_filename(&stale.id));
        let default_path = models_dir.join(model_filename(&default_model.id));
        std::fs::write(
            &stale_path,
            toml::to_string(&stale).map_err(ctx("serialize stale cache"))?,
        )
        .map_err(ctx("stale cache"))?;
        std::fs::write(
            &default_path,
            toml::to_string(&default_model).map_err(ctx("serialize default cache"))?,
        )
        .map_err(ctx("default cache"))?;

        Ok((profile, provider_path, stale_path, default_path))
    }

    #[test]
    fn test_sync_functions_delete_nothing_when_discovered_is_empty() -> TestResult {
        // Regression: ein Provider-Scan, der 0 Modelle meldet (z. B. wegen
        // eines Auth-Problems), darf weder die Auswahl noch den Datei-Cache
        // leerräumen — selbst wenn `--prune` gesetzt ist.
        let (_guard, home) = temp_home()?;
        let (profile, provider_path, stale_path, default_path) = prune_fixture(&home)?;
        let models_dir = profile.join("models");
        let protected: BTreeMap<String, ProtectionReason> = BTreeMap::new();

        sync_provider_model_list(&profile, "acme", &[], true, &protected)
            .map_err(ctx("empty sync of provider list must succeed"))?;
        sync_discovered_model_files(&models_dir, "acme", &[], true, &protected)
            .map_err(ctx("empty sync of model files must succeed"))?;

        let provider: harw_config::ProviderToml =
            toml::from_str(&std::fs::read_to_string(&provider_path).map_err(ctx("read provider"))?)
                .map_err(ctx("parse provider"))?;
        // Bei leerem `discovered` kehrt `sync_provider_model_list` sofort
        // zurück (siehe Funktionskommentar), ohne die Datei anzufassen — die
        // Auswahl bleibt exakt in der Reihenfolge erhalten, in der das
        // Fixture sie geschrieben hat (nicht alphabetisch sortiert).
        assert_eq!(provider.models, ["stale-model", "gpt-5.6-terra"]);
        assert!(stale_path.exists());
        assert!(default_path.exists());
        Ok(())
    }

    #[test]
    fn test_sync_without_prune_adds_live_but_deletes_nothing() -> TestResult {
        let (_guard, home) = temp_home()?;
        let (profile, provider_path, stale_path, default_path) = prune_fixture(&home)?;
        let models_dir = profile.join("models");
        let live = [DiscoveredModel {
            id: "new-model".to_owned(),
            context_length: None,
            input_price_per_mtok: None,
            output_price_per_mtok: None,
            supports_tools: None,
        }];
        let protected: BTreeMap<String, ProtectionReason> = BTreeMap::new();

        sync_provider_model_list(&profile, "acme", &live, false, &protected)
            .map_err(ctx("sync provider list without prune"))?;
        sync_discovered_model_files(&models_dir, "acme", &live, false, &protected)
            .map_err(ctx("sync model files without prune"))?;

        let provider: harw_config::ProviderToml =
            toml::from_str(&std::fs::read_to_string(&provider_path).map_err(ctx("read provider"))?)
                .map_err(ctx("parse provider"))?;
        // Ohne `--prune` schreibt `sync_provider_model_list` die Auswahl
        // nicht zurück (nur "nicht mehr gemeldet"-Meldungen) — die
        // ursprüngliche, unsortierte Fixture-Reihenfolge bleibt erhalten.
        assert_eq!(provider.models, ["stale-model", "gpt-5.6-terra"]);
        assert!(
            stale_path.exists(),
            "ohne --prune bleibt Alt-Cache erhalten"
        );
        assert!(default_path.exists());
        assert!(models_dir.join(model_filename("new-model")).exists());
        Ok(())
    }

    #[test]
    fn test_sync_with_prune_removes_unreferenced_but_keeps_protected() -> TestResult {
        let (_guard, home) = temp_home()?;
        let (profile, provider_path, stale_path, default_path) = prune_fixture(&home)?;
        let models_dir = profile.join("models");
        let mut protected = BTreeMap::new();
        protected.insert("gpt-5.6-terra".to_owned(), ProtectionReason::InUse);
        // Live-Antwort meldet nur ein drittes, bisher unbekanntes Modell —
        // "stale-model" fehlt (nicht mehr live) und "gpt-5.6-terra" fehlt
        // ebenfalls, ist aber `protected` und muss trotzdem überleben.
        let live = [DiscoveredModel {
            id: "keep-alive".to_owned(),
            context_length: None,
            input_price_per_mtok: None,
            output_price_per_mtok: None,
            supports_tools: None,
        }];

        sync_provider_model_list(&profile, "acme", &live, true, &protected)
            .map_err(ctx("sync provider list with prune"))?;
        sync_discovered_model_files(&models_dir, "acme", &live, true, &protected)
            .map_err(ctx("sync model files with prune"))?;

        let provider: harw_config::ProviderToml =
            toml::from_str(&std::fs::read_to_string(&provider_path).map_err(ctx("read provider"))?)
                .map_err(ctx("parse provider"))?;
        assert!(!provider.models.contains(&"stale-model".to_owned()));
        assert!(provider.models.contains(&"gpt-5.6-terra".to_owned()));
        assert!(
            !stale_path.exists(),
            "unreferenziertes Modell wird entfernt"
        );
        assert!(default_path.exists(), "default_model bleibt erhalten");
        Ok(())
    }

    #[test]
    fn test_protected_model_ids_collects_default_uia_session_and_internal_models() {
        let mut config = harw_config::ResolvedConfig::default();
        config.harness.default_provider = Some("acme".to_owned());
        config.harness.default_model = Some("gpt-5.6-terra".to_owned());
        config.harness.session.title_model = Some("gpt-5.6-nano".to_owned());
        config.harness.uia_provider = Some("other".to_owned());
        config.harness.uia_model = Some("uia-only-model".to_owned());
        config.harness.uia_worker_model = Some("uia-worker-only-model".to_owned());
        config.harness.internal_models.set_choice(
            harw_config::InternalModelPoint::Explorer,
            Some(harw_config::InternalModelChoice {
                provider: None,
                model: Some("explorer-model".to_owned()),
            }),
        );
        config.harness.internal_models.set_choice(
            harw_config::InternalModelPoint::Research,
            Some(harw_config::InternalModelChoice {
                provider: Some("other".to_owned()),
                model: Some("research-model".to_owned()),
            }),
        );

        let acme_protected = protected_model_ids(&config, "acme");
        assert!(acme_protected.contains_key("gpt-5.6-terra"));
        assert!(acme_protected.contains_key("gpt-5.6-nano"));
        assert!(acme_protected.contains_key("explorer-model"));
        assert!(!acme_protected.contains_key("uia-only-model"));
        assert!(!acme_protected.contains_key("uia-worker-only-model"));
        assert!(!acme_protected.contains_key("research-model"));
        assert_eq!(
            acme_protected.get("gpt-5.6-terra"),
            Some(&ProtectionReason::InUse)
        );

        let other_protected = protected_model_ids(&config, "other");
        assert!(other_protected.contains_key("uia-only-model"));
        assert!(other_protected.contains_key("uia-worker-only-model"));
        assert!(other_protected.contains_key("research-model"));
        assert!(!other_protected.contains_key("gpt-5.6-terra"));
        assert_eq!(
            other_protected.get("uia-worker-only-model"),
            Some(&ProtectionReason::InUse)
        );
    }

    #[test]
    fn test_protected_model_ids_ignores_uia_worker_model_when_its_provider_does_not_match_uia_provider()
     {
        // `uia_worker_model` hat kein eigenes Provider-Feld (siehe
        // `harw-config/src/harness_config.rs`: "Der Provider ist hier
        // bewusst nicht separat wählbar — er muss zwingend mit dem
        // effektiven `uia_provider` übereinstimmen"). Im Katalog gehört die
        // ID hier formal zu Provider `mismatch` (dort in `models` gelistet),
        // während der effektive `uia_provider` `other` ist — eine
        // inkonsistente Konfiguration. Die eigentliche
        // Kopplungsprüfung/Ablehnung dafür passiert an anderer Stelle; diese
        // Katalog-Schutzlogik darf die ID beim Scan von `mismatch` aber
        // nicht fälschlich vor Löschung schützen, sonst bleibt ein
        // verwaistes Modell für immer im Katalog von `mismatch` hängen.
        let mut config = harw_config::ResolvedConfig::default();
        config.harness.uia_provider = Some("other".to_owned());
        config.harness.uia_worker_model = Some("mismatched-worker-model".to_owned());
        config.providers.insert(
            "mismatch".to_owned(),
            test_provider_toml(
                "https://mismatch.example.com/v1",
                &["mismatched-worker-model"],
            ),
        );

        let mismatch_protected = protected_model_ids(&config, "mismatch");
        assert!(!mismatch_protected.contains_key("mismatched-worker-model"));

        // Gegenprobe: beim Scan des tatsächlich effektiven `uia_provider`
        // ("other") greift der Block wie vorgesehen und schützt die ID.
        let other_protected = protected_model_ids(&config, "other");
        assert!(other_protected.contains_key("mismatched-worker-model"));
        assert_eq!(
            other_protected.get("mismatched-worker-model"),
            Some(&ProtectionReason::InUse)
        );
    }

    /// Baut eine minimale `ProviderToml` für Tests, die nur die `models`-Auswahl
    /// und den `base_url` benötigen.
    fn test_provider_toml(base_url: &str, models: &[&str]) -> harw_config::ProviderToml {
        harw_config::ProviderToml {
            stream: None,
            name: "test".to_owned(),
            api: "openai-chat".to_owned(),
            base_url: base_url.to_owned(),
            auth: None,
            auth_header: None,
            api_key: None,
            originator: None,
            headers: std::collections::HashMap::new(),
            models: models.iter().map(|id| (*id).to_owned()).collect(),
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
            allow_insecure_lan: false,
        }
    }

    /// Baut eine minimale `AgentToml` für Tests, die nur `models` benötigen.
    fn test_agent_toml(models: &[&str]) -> harw_config::AgentToml {
        harw_config::AgentToml {
            name: "test-agent".to_owned(),
            role: "worker".to_owned(),
            description: String::new(),
            system_file: None,
            providers: Vec::new(),
            models: models.iter().map(|id| (*id).to_owned()).collect(),
            skills: Vec::new(),
            suggestions: harw_config::AgentSuggestionsToml::default(),
            primary_provider: None,
            secondary_providers: Vec::new(),
            timeout_seconds: 120,
            max_retries: 2,
        }
    }

    #[test]
    fn test_protected_model_ids_keeps_ids_referenced_by_another_provider() {
        // Realfall aus der Modul-Doku: `providers/foundry.toml` führt
        // `models = ["gpt-5.6-terra"]`, obwohl `gpt-5.6-terra.toml` formal
        // dem Provider `openai` gehört. Ein Prune von `openai` darf diese ID
        // nicht löschen.
        let mut config = harw_config::ResolvedConfig::default();
        config.providers.insert(
            "foundry".to_owned(),
            test_provider_toml("https://foundry.example.com/v1", &["gpt-5.6-terra"]),
        );

        let protected = protected_model_ids(&config, "openai");
        assert_eq!(
            protected.get("gpt-5.6-terra"),
            Some(&ProtectionReason::Referenced("foundry".to_owned()))
        );
    }

    #[test]
    fn test_protected_model_ids_ignores_the_scanned_providers_own_model_list() {
        // Die eigene Auswahlliste des gerade gescannten Providers darf nicht
        // als Cross-Referenz zählen, sonst würde Prune der Auswahlliste nie
        // greifen.
        let mut config = harw_config::ResolvedConfig::default();
        config.providers.insert(
            "openai".to_owned(),
            test_provider_toml("https://api.openai.com/v1", &["gpt-5.6-terra"]),
        );

        let protected = protected_model_ids(&config, "openai");
        assert!(!protected.contains_key("gpt-5.6-terra"));
    }

    #[test]
    fn test_protected_model_ids_keeps_ids_referenced_by_an_agent_definition() {
        let mut config = harw_config::ResolvedConfig::default();
        config
            .agents
            .insert("planner".to_owned(), test_agent_toml(&["gpt-5.6-terra"]));

        let protected = protected_model_ids(&config, "openai");
        assert_eq!(
            protected.get("gpt-5.6-terra"),
            Some(&ProtectionReason::Referenced("planner (Agent)".to_owned()))
        );
    }

    #[test]
    fn test_sync_provider_model_list_reports_referenced_reason_in_output() -> TestResult {
        let (_guard, home) = temp_home()?;
        let profile = home.join("profiles/default");
        let providers = profile.join("providers");
        std::fs::create_dir_all(&providers).map_err(ctx("providers dir"))?;
        std::fs::write(
            providers.join("openai.toml"),
            "name = \"openai\"\napi = \"openai-chat\"\nbase_url = \"https://api.openai.com/v1\"\nauth = \"env:OPENAI_TOKEN\"\nmodels = [\"gpt-5.6-terra\"]\n",
        )
        .map_err(ctx("write provider"))?;
        let mut protected = BTreeMap::new();
        protected.insert(
            "gpt-5.6-terra".to_owned(),
            ProtectionReason::Referenced("foundry".to_owned()),
        );
        // Ein nicht-leeres `discovered` ist Voraussetzung dafür, dass
        // `sync_provider_model_list` überhaupt in den Prune-Zweig läuft
        // (leeres `discovered` bricht immer sicherheitshalber früh ab).
        // "gpt-5.6-terra" selbst ist bewusst NICHT live, um die
        // Cross-Referenz-Protection zu erzwingen.
        let live = [DiscoveredModel {
            id: "some-other-live-model".to_owned(),
            context_length: None,
            input_price_per_mtok: None,
            output_price_per_mtok: None,
            supports_tools: None,
        }];

        sync_provider_model_list(&profile, "openai", &live, true, &protected)
            .map_err(ctx("sync provider list with cross-reference protection"))?;

        let provider: harw_config::ProviderToml = toml::from_str(
            &std::fs::read_to_string(providers.join("openai.toml"))
                .map_err(ctx("read provider"))?,
        )
        .map_err(ctx("parse provider"))?;
        assert_eq!(provider.models, ["gpt-5.6-terra"]);
        Ok(())
    }

    #[test]
    fn test_has_placeholder_base_url_detects_known_placeholder_patterns() {
        assert!(has_placeholder_base_url("https://<dein-worker>.example/v1"));
        assert!(has_placeholder_base_url("https://example.invalid/v1"));
        assert!(!has_placeholder_base_url("https://api.openai.com/v1"));
    }

    #[test]
    fn test_add_catalog_fallback_models_adds_unconfigured_catalog_ids_without_removing()
    -> TestResult {
        let (_guard, home) = temp_home()?;
        let profile = home.join("profiles/default");
        let providers = profile.join("providers");
        std::fs::create_dir_all(&providers).map_err(ctx("providers dir"))?;
        std::fs::write(
            providers.join("openai.toml"),
            "name = \"openai\"\napi = \"openai-chat\"\nbase_url = \"https://api.openai.com/v1\"\nauth = \"env:OPENAI_TOKEN\"\nmodels = [\"kept-existing-model\"]\n",
        )
        .map_err(ctx("write provider"))?;

        add_catalog_fallback_models(&profile, "openai").map_err(ctx("add catalog fallback"))?;

        let provider: harw_config::ProviderToml = toml::from_str(
            &std::fs::read_to_string(providers.join("openai.toml"))
                .map_err(ctx("read provider"))?,
        )
        .map_err(ctx("parse provider"))?;
        assert!(provider.models.contains(&"kept-existing-model".to_owned()));
        assert!(
            provider.models.len() > 1,
            "Katalog-Modelle für openai müssen additiv ergänzt werden"
        );
        Ok(())
    }

    #[test]
    fn test_add_catalog_fallback_models_skips_unknown_provider_and_placeholder_base_url()
    -> TestResult {
        let (_guard, home) = temp_home()?;
        let profile = home.join("profiles/default");
        let providers = profile.join("providers");
        std::fs::create_dir_all(&providers).map_err(ctx("providers dir"))?;
        std::fs::write(
            providers.join("cf-worker.toml"),
            "name = \"cf-worker\"\napi = \"openai-chat\"\nbase_url = \"https://<dein-worker>.example/v1\"\nauth = \"env:CF_TOKEN\"\nmodels = []\n",
        )
        .map_err(ctx("write provider"))?;
        std::fs::write(
            providers.join("ghost.toml"),
            "name = \"ghost\"\napi = \"openai-chat\"\nbase_url = \"https://ghost.example.com/v1\"\nauth = \"env:GHOST_TOKEN\"\nmodels = []\n",
        )
        .map_err(ctx("write provider"))?;

        add_catalog_fallback_models(&profile, "cf-worker")
            .map_err(ctx("placeholder base_url must not error"))?;
        add_catalog_fallback_models(&profile, "ghost")
            .map_err(ctx("unknown catalog entry must not error"))?;

        let cf_worker: harw_config::ProviderToml = toml::from_str(
            &std::fs::read_to_string(providers.join("cf-worker.toml"))
                .map_err(ctx("read provider"))?,
        )
        .map_err(ctx("parse provider"))?;
        assert!(
            cf_worker.models.is_empty(),
            "Platzhalter-base_url darf keine Katalog-Modelle hinzufügen"
        );

        let ghost: harw_config::ProviderToml = toml::from_str(
            &std::fs::read_to_string(providers.join("ghost.toml")).map_err(ctx("read provider"))?,
        )
        .map_err(ctx("parse provider"))?;
        assert!(
            ghost.models.is_empty(),
            "unbekannter Provider ohne Katalog-Eintrag darf nichts hinzufügen"
        );
        Ok(())
    }

    #[test]
    fn test_parse_point_rejects_unknown_key_and_lists_valid_ones() -> TestResult {
        let result = parse_point("does-not-exist");
        let Err(error) = result else {
            return Err(TestError::Unexpected(
                "unknown point must be rejected".into(),
            ));
        };
        let message = error.to_string();
        assert!(message.contains("session_title"));
        assert!(message.contains("does-not-exist"));
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
    fn test_internal_set_main_reset_round_trip() -> TestResult {
        let (_guard, home) = temp_home()?;
        let point = harw_config::InternalModelPoint::Explorer;

        set_internal_choice(
            &home,
            point,
            "nvidia/nemotron-3-super-120b-a12b",
            Some("openrouter"),
        )
        .map_err(ctx("set internal choice"))?;
        let path = global_config_path(&home).map_err(ctx("global config path"))?;
        let writer = ConfigWriter::open(&path).map_err(ctx("reopen after set"))?;
        assert_eq!(
            writer.get_value("internal_models.explorer.model"),
            Some("nvidia/nemotron-3-super-120b-a12b".to_owned())
        );
        assert_eq!(
            writer.get_value("internal_models.explorer.provider"),
            Some("openrouter".to_owned())
        );

        set_internal_main(&home, point).map_err(ctx("force main model"))?;
        let writer = ConfigWriter::open(&path).map_err(ctx("reopen after main"))?;
        assert_eq!(writer.get_value("internal_models.explorer.model"), None);
        assert_eq!(writer.get_value("internal_models.explorer.provider"), None);

        reset_internal_choice(&home, point).map_err(ctx("reset choice"))?;
        let content = std::fs::read_to_string(&path).map_err(ctx("read config after reset"))?;
        assert!(!content.contains("[internal_models.explorer]"));
        Ok(())
    }

    #[test]
    fn test_openrouter_defaults_toggle_round_trips() -> TestResult {
        let (_guard, home) = temp_home()?;
        set_openrouter_defaults(&home, false).map_err(ctx("disable openrouter defaults"))?;
        let path = global_config_path(&home).map_err(ctx("global config path"))?;
        let writer = ConfigWriter::open(&path).map_err(ctx("reopen"))?;
        assert_eq!(
            writer.get_value("internal_models.use_openrouter_defaults"),
            Some("false".to_owned())
        );
        Ok(())
    }

    #[test]
    fn test_run_scan_reports_unknown_provider() -> TestResult {
        let (_guard, home) = temp_home()?;
        let result = execute(
            &home,
            Some(ModelsAction::Scan {
                provider: Some("ghost".to_owned()),
                add: false,
                free_only: false,
                prune: false,
            }),
        );
        let Err(error) = result else {
            return Err(TestError::Unexpected("unknown provider must error".into()));
        };
        assert!(matches!(error, ModelsError::ProviderNotFound { .. }));
        Ok(())
    }

    #[test]
    fn test_run_list_without_providers_succeeds() -> TestResult {
        let (_guard, home) = temp_home()?;
        execute(&home, None).map_err(ctx("list without providers must still succeed"))?;
        Ok(())
    }
}
