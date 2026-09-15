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
//! `harw_provider_http::discovery::resolve_provider_api_key` (env-/Klartext-
//! Referenzen, kein Home-gebundenes `file:`/`file-json:` — siehe dortige
//! Doku). Das Ergebnis wird nie geloggt.
//!
//! # Concurrency
//! Zustandslos; `harw models scan` baut für die Dauer des Befehls eine
//! Single-Thread-`tokio`-Laufzeit (wie `crate::connect`/`crate::auth`).

use std::error::Error as StdError;
use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use toml_edit::value;

use harw_config::ConfigWriter;
use harw_provider_http::discovery::{self, DiscoveredModel};

use crate::cli::{InternalAction, ModelsAction};

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
    Io { path: PathBuf, source: std::io::Error },
    /// Eine TOML-Datei ist kein gültiges bzw. serialisierbares Dokument.
    Toml { path: PathBuf, reason: String },
    /// Der angegebene Provider existiert nicht in der aufgelösten Konfiguration.
    ProviderNotFound { name: String },
    /// Ein unbekannter Stellen-Schlüssel wurde an `harw models internal` übergeben.
    UnknownPoint { point: String, valid: String },
}

impl fmt::Display for ModelsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Home(source) => write!(f, "{source}"),
            Self::Config(source) => write!(f, "{source}"),
            Self::Io { path, source } => {
                write!(f, "dateizugriff auf {} fehlgeschlagen: {source}", path.display())
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
    let home = crate::home::resolve_home(home_override)?;
    harw_home::ensure_home(&home).map_err(|error| error.to_string())?;
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
        }) => run_scan(home, provider, add, free_only),
        Some(ModelsAction::Internal { action }) => run_internal(home, action),
        Some(ModelsAction::Default { id }) => {
            set_default_model(home, &id)?;
            println!("Standardmodell auf {id:?} gesetzt.");
            Ok(())
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

    println!("== Provider ==");
    let mut provider_names: Vec<&String> = config.providers.keys().collect();
    provider_names.sort();
    if provider_names.is_empty() {
        println!("(keine Provider konfiguriert)");
    }
    for name in provider_names {
        let provider = &config.providers[name];
        let auth_ok = discovery::resolve_provider_api_key(name, provider, &config).is_some()
            || provider.auth_header.as_deref() == Some("none");
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
        by_provider.entry(model.provider.as_str()).or_default().push(model);
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
) -> Result<(), ModelsError> {
    let (config, profile) = load_config_and_profile(home)?;

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
    for (name, provider) in targets {
        let api_key = discovery::resolve_provider_api_key(name, provider, &config);
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
                    if add {
                        write_discovered_model_file(&models_dir, name, model)?;
                    }
                }
            }
            Err(error) => println!("{name}: {error}"),
        }
    }
    Ok(())
}

/// `true`, wenn `model` als kostenlos gilt: `:free`-Suffix in der ID, oder
/// mindestens ein gemeldeter Preis von `0`.
fn is_free_model(model: &DiscoveredModel) -> bool {
    model.id.ends_with(":free")
        || model.input_price_per_mtok.is_some_and(|price| price <= 0.0)
        || model.output_price_per_mtok.is_some_and(|price| price <= 0.0)
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

/// Legt `models/<id>.toml` im aktiven Profil an, sofern die Datei noch nicht
/// existiert (bestehende Dateien bleiben unverändert).
fn write_discovered_model_file(
    models_dir: &Path,
    provider_name: &str,
    model: &DiscoveredModel,
) -> Result<(), ModelsError> {
    std::fs::create_dir_all(models_dir).map_err(|source| ModelsError::Io {
        path: models_dir.to_path_buf(),
        source,
    })?;
    let path = models_dir.join(model_filename(&model.id));
    if path.exists() {
        return Ok(());
    }
    let toml_model = harw_config::ModelToml {
        id: model.id.clone(),
        name: None,
        provider: provider_name.to_owned(),
        aliases: Vec::new(),
        context_window: model.context_length,
        max_tokens: None,
        prompt_caching: None,
        reasoning: false,
        input_types: Vec::new(),
        capabilities: harw_config::ModelCapabilitiesToml {
            tool_use: model.supports_tools.unwrap_or(false),
            streaming: false,
            vision: false,
            json_mode: false,
        },
    };
    let rendered = toml::to_string_pretty(&toml_model).map_err(|error| ModelsError::Toml {
        path: path.clone(),
        reason: error.to_string(),
    })?;
    write_atomic(&path, rendered.as_bytes())
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
            let enabled = state == "on";
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
        let sub = ensure_table(table, point.key());
        sub.insert("model", value(model));
        if let Some(provider) = provider {
            sub.insert("provider", value(provider));
        } else {
            sub.remove("provider");
        }
    })
}

/// Erzwingt das Hauptmodell für `point`: legt eine leere
/// `[internal_models.<point>]`-Tabelle an (`model`/`provider` entfernt),
/// die `harw_config::resolve_internal_model` als `MainModel` liest.
fn set_internal_main(home: &Path, point: harw_config::InternalModelPoint) -> Result<(), ModelsError> {
    mutate_internal_models_table(home, |table| {
        let sub = ensure_table(table, point.key());
        sub.remove("model");
        sub.remove("provider");
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
    })
}

/// Setzt `[internal_models] use_openrouter_defaults = true|false`.
fn set_openrouter_defaults(home: &Path, enabled: bool) -> Result<(), ModelsError> {
    mutate_internal_models_table(home, |table| {
        table.insert("use_openrouter_defaults", value(enabled));
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
    mutate: impl FnOnce(&mut toml_edit::Table),
) -> Result<(), ModelsError> {
    let path = global_config_path(home)?;
    let mut doc = open_document(&path)?;
    let root = doc.as_table_mut();
    let table = ensure_table(root, "internal_models");
    mutate(table);
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
    content.parse::<toml_edit::DocumentMut>().map_err(|error| ModelsError::Toml {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })
}

/// Stellt sicher, dass `table[key]` eine Tabelle ist, und gibt eine
/// veränderliche Referenz darauf zurück. Minimal nachgebaut aus
/// `harw_config::writer::ensure_table` (privat in der Schwester-Crate).
fn ensure_table<'a>(table: &'a mut toml_edit::Table, key: &str) -> &'a mut toml_edit::Table {
    if !matches!(table.get(key), Some(item) if item.is_table()) {
        table.insert(key, toml_edit::Item::Table(toml_edit::Table::new()));
    }
    match table.get_mut(key).and_then(toml_edit::Item::as_table_mut) {
        Some(table) => table,
        None => unreachable!("key {key:?} was just normalised to a table"),
    }
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
    writer.set_value("default_model", value(id));
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

    fn temp_home() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("harw-home");
        harw_home::ensure_home(&home).expect("ensure_home");
        (dir, home)
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
    fn test_parse_point_rejects_unknown_key_and_lists_valid_ones() {
        let error = parse_point("does-not-exist").expect_err("unknown point must be rejected");
        let message = error.to_string();
        assert!(message.contains("session_title"));
        assert!(message.contains("does-not-exist"));
    }

    #[test]
    fn test_set_default_model_round_trips_through_global_config() {
        let (_guard, home) = temp_home();
        set_default_model(&home, "gpt-5.4").expect("set default model");

        let path = global_config_path(&home).expect("global config path");
        let writer = ConfigWriter::open(&path).expect("reopen");
        assert_eq!(writer.get_value("default_model"), Some("gpt-5.4".to_owned()));
    }

    #[test]
    fn test_internal_set_main_reset_round_trip() {
        let (_guard, home) = temp_home();
        let point = harw_config::InternalModelPoint::Explorer;

        set_internal_choice(&home, point, "nvidia/nemotron-3-super-120b-a12b", Some("openrouter"))
            .expect("set internal choice");
        let path = global_config_path(&home).expect("global config path");
        let writer = ConfigWriter::open(&path).expect("reopen after set");
        assert_eq!(
            writer.get_value("internal_models.explorer.model"),
            Some("nvidia/nemotron-3-super-120b-a12b".to_owned())
        );
        assert_eq!(
            writer.get_value("internal_models.explorer.provider"),
            Some("openrouter".to_owned())
        );

        set_internal_main(&home, point).expect("force main model");
        let writer = ConfigWriter::open(&path).expect("reopen after main");
        assert_eq!(writer.get_value("internal_models.explorer.model"), None);
        assert_eq!(writer.get_value("internal_models.explorer.provider"), None);

        reset_internal_choice(&home, point).expect("reset choice");
        let content = std::fs::read_to_string(&path).expect("read config after reset");
        assert!(!content.contains("[internal_models.explorer]"));
    }

    #[test]
    fn test_openrouter_defaults_toggle_round_trips() {
        let (_guard, home) = temp_home();
        set_openrouter_defaults(&home, false).expect("disable openrouter defaults");
        let path = global_config_path(&home).expect("global config path");
        let writer = ConfigWriter::open(&path).expect("reopen");
        assert_eq!(
            writer.get_value("internal_models.use_openrouter_defaults"),
            Some("false".to_owned())
        );
    }

    #[test]
    fn test_run_scan_reports_unknown_provider() {
        let (_guard, home) = temp_home();
        let error = execute(
            &home,
            Some(ModelsAction::Scan {
                provider: Some("ghost".to_owned()),
                add: false,
                free_only: false,
            }),
        )
        .expect_err("unknown provider must error");
        assert!(matches!(error, ModelsError::ProviderNotFound { .. }));
    }

    #[test]
    fn test_run_list_without_providers_succeeds() {
        let (_guard, home) = temp_home();
        execute(&home, None).expect("list without providers must still succeed");
    }
}
