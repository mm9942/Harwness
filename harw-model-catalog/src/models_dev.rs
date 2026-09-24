//! models.dev-Anreicherung des Provider-Katalogs.
//!
//! Spezifikationsquelle: `docs/design/CONTRACT-setup-install.md`,
//! Abschnitt „Crate `harw-model-catalog` / `src/models_dev.rs`".
//!
//! # Verantwortung
//! Dieses Modul reichert die `models`-Liste jeder [`ProviderSpec`] mit den bei
//! models.dev gelisteten Modell-Ids an. Es besitzt ausschließlich die
//! Cache-Verwaltung (`cache_dir/models_dev.json`, mtime-TTL 3600s), den
//! blockierenden HTTP-Fetch und das Merge in den Katalog. Das Parsen des
//! statischen Katalogs sowie die Provider-Definitionen delegiert es an
//! [`crate::spec`] / [`crate::embedded`].
//!
//! # Nebenläufigkeit
//! Alle Funktionen sind synchron und blockierend (`reqwest::blocking`). Sie
//! akquirieren keine Locks und starten keine Threads; der Cache wird atomar
//! (Schreiben in temporäre Datei + Rename) aktualisiert.
//!
//! # Fehler
//! Erzeugt [`CatalogError::Io`] für lokale Datei-Operationen und
//! [`CatalogError::Parse`] für ungültiges JSON. Netzfehler sind **fail-open**:
//! ist ein Cache vorhanden, wird dieser genutzt, andernfalls bleibt der Katalog
//! unverändert und es wird `Ok(())` zurückgegeben.
//!
//! # Examples
//! ```rust,no_run
//! use std::path::Path;
//! use harw_model_catalog::embedded_catalog;
//! use harw_model_catalog::models_dev::enrich_models;
//!
//! let mut catalog = embedded_catalog();
//! // Nutzt Cache oder holt frische Daten von models.dev; niemals ein harter Fehler bei Netzausfall.
//! enrich_models(Path::new("/home/u/.harw/cache"), &mut catalog).ok();
//! ```

use std::collections::BTreeMap;
use std::fs;
use std::fs::OpenOptions;
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use serde::Deserialize;

use crate::error::{CatalogError, CatalogResult};
use crate::spec::ProviderSpec;

/// Basisname der Cache-Datei innerhalb von `cache_dir`.
const CACHE_FILE_NAME: &str = "models_dev.json";

/// URL der models.dev-Katalog-API.
const MODELS_DEV_URL: &str = "https://models.dev/api.json";

/// TTL des Caches in Sekunden: jüngere Dateien werden ohne Netzabruf genutzt.
const CACHE_TTL_SECS: u64 = 3600;

/// HTTP-Timeout für den Fetch von models.dev.
const HTTP_TIMEOUT_SECS: u64 = 15;

/// Prozesslokale Sequenz für kollisionsfreie Cache-Temporärdateien.
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Eintrag eines einzelnen Modells innerhalb eines models.dev-Providers.
///
/// # Description
/// models.dev liefert je Modell ein Objekt mit vielen optionalen Feldern. Für
/// die Anreicherung werden Modell-ID, Ausgabemodalitäten und Status genutzt;
/// alle weiteren Felder bleiben unberücksichtigt.
#[derive(Debug, Clone, Deserialize)]
struct ModelEntry {
    #[serde(default)]
    modalities: Option<ModelModalities>,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ModelModalities {
    #[serde(default)]
    output: Vec<String>,
}

/// Ein Provider-Eintrag der models.dev-API.
///
/// # Description
/// Bildet das Teilschema `{ "models": { "<model-id>": { … } } }` ab. Weitere
/// Provider-Felder werden bewusst ignoriert.
#[derive(Debug, Clone, Deserialize)]
struct ModelsDevProvider {
    /// Abbildung Modell-Id → Filter-Metadaten.
    #[serde(default)]
    models: BTreeMap<String, ModelEntry>,
}

/// Deserialisierter und indizierter models.dev-Katalog.
///
/// # Description
/// Kapselt das Top-Level-Dictionary `provider-id → ModelsDevProvider` der
/// models.dev-API und stellt einen sortierten Zugriff auf die Modell-Ids je
/// Provider bereit.
#[derive(Debug, Clone, Deserialize)]
pub struct ModelsDevCache {
    /// Abbildung Provider-Id → Provider-Eintrag.
    #[serde(flatten)]
    providers: BTreeMap<String, ModelsDevProvider>,
}

impl ModelsDevCache {
    /// Parst rohes models.dev-JSON in einen indizierten Cache.
    ///
    /// # Arguments
    /// - `raw` (`&str`): der vollständige JSON-Body der models.dev-API.
    ///
    /// # Returns
    /// Ein [`ModelsDevCache`] mit allen erkannten Providern.
    ///
    /// # Errors
    /// - [`CatalogError::Parse`]: wenn `raw` kein gültiges models.dev-JSON ist.
    pub fn from_json(raw: &str) -> CatalogResult<Self> {
        serde_json::from_str(raw).map_err(|e| CatalogError::Parse(e.to_string()))
    }

    /// Liefert die sortierten Modell-Ids für einen Provider, falls vorhanden.
    ///
    /// # Arguments
    /// - `provider_id` (`&str`): Katalog-Provider-Id (entspricht dem
    ///   models.dev-Schlüssel).
    ///
    /// # Returns
    /// `Some(Vec<String>)` mit den sortierten Modell-Ids, sonst `None`.
    pub fn models_for(&self, provider_id: &str) -> Option<Vec<String>> {
        self.providers
            .get(models_dev_provider_id(provider_id))
            .map(|p| {
                p.models
                    .iter()
                    .filter(|(_, model)| {
                        model.status.as_deref() != Some("deprecated")
                            && model
                                .modalities
                                .as_ref()
                                .is_none_or(|m| m.output.iter().any(|v| v == "text"))
                    })
                    .map(|(id, _)| id.clone())
                    .collect()
            })
    }
}

/// Reichert den Katalog in-place mit models.dev-Modell-Ids an.
///
/// # Description
/// Liest `cache_dir/models_dev.json`. Ist die Datei jünger als
/// [`CACHE_TTL_SECS`], wird ihr Inhalt verwendet. Andernfalls wird
/// `https://models.dev/api.json` blockierend abgerufen (Timeout
/// [`HTTP_TIMEOUT_SECS`]) und atomar in den Cache geschrieben. Für jeden
/// Provider im `catalog`, dessen `id` in den models.dev-Daten vorkommt, wird
/// `ProviderSpec.models` durch die dort gelisteten (sortierten) Modell-Ids
/// ersetzt.
///
/// Netzfehler sind **fail-open**: existiert ein (auch veralteter) Cache, wird
/// dieser genutzt; existiert keiner, bleibt `catalog` unverändert und die
/// Funktion gibt `Ok(())` zurück.
///
/// # Arguments
/// - `cache_dir` (`&Path`): Verzeichnis für die Cache-Datei; wird bei Bedarf
///   angelegt.
/// - `catalog` (`&mut [ProviderSpec]`): der anzureichernde Provider-Katalog.
///
/// # Returns
/// `Ok(())` bei erfolgreicher Anreicherung oder fail-open bei Netzausfall.
///
/// # Errors
/// - [`CatalogError::Io`][]: bei nicht behebbaren Datei-Operationen (Anlegen des
///   Cache-Verzeichnisses, Schreiben/Lesen des Caches).
/// - [`CatalogError::Parse`][]: wenn der Cache-Inhalt kein gültiges JSON ist.
///
/// # Concurrency
/// Blockierend und synchron; keine Locks, keine Threads. Der Cache wird atomar
/// via temporärer Datei + Rename aktualisiert.
///
/// # Examples
/// ```rust,no_run
/// use std::path::Path;
/// use harw_model_catalog::embedded_catalog;
/// use harw_model_catalog::models_dev::enrich_models;
///
/// let mut catalog = embedded_catalog();
/// enrich_models(Path::new("/home/u/.harw/cache"), &mut catalog).ok();
/// ```
pub fn enrich_models(cache_dir: &Path, catalog: &mut [ProviderSpec]) -> CatalogResult<()> {
    let cache = match load_cache(cache_dir, false)? {
        Some(cache) => cache,
        None => return Ok(()),
    };
    apply_cache(&cache, catalog);
    Ok(())
}

/// Refreshes model lists even when the on-disk cache has not expired.
pub fn refresh_models(cache_dir: &Path, catalog: &mut [ProviderSpec]) -> CatalogResult<()> {
    if let Some(cache) = load_cache(cache_dir, true)? {
        apply_cache(&cache, catalog);
    }
    Ok(())
}

/// Beschafft den models.dev-Cache aus lokaler Datei oder via Netz.
///
/// Gibt `Ok(None)` zurück, wenn weder ein gültiger Cache vorliegt noch ein
/// Fetch möglich ist (fail-open ohne Katalogänderung).
fn load_cache(cache_dir: &Path, force: bool) -> CatalogResult<Option<ModelsDevCache>> {
    load_cache_with_fetch(cache_dir, force, fetch_remote)
}

fn load_cache_with_fetch(
    cache_dir: &Path,
    force: bool,
    fetch: impl FnOnce() -> CatalogResult<String>,
) -> CatalogResult<Option<ModelsDevCache>> {
    let cache_path = cache_dir.join(CACHE_FILE_NAME);

    let fresh = cache_is_fresh(&cache_path)?;
    let cached = if cache_exists(&cache_path)? {
        ModelsDevCache::from_json(&read_cache(&cache_path)?).ok()
    } else {
        None
    };
    if fresh && !force && cached.is_some() {
        return Ok(cached);
    }
    match fetch().and_then(|raw| {
        let parsed = ModelsDevCache::from_json(&raw)?;
        // Never replace a valid cache with malformed data.
        write_cache_atomic(cache_dir, &cache_path, &raw).ok();
        Ok(parsed)
    }) {
        Ok(cache) => Ok(Some(cache)),
        Err(_) => Ok(cached),
    }
}

/// Prüft, ob die Cache-Datei existiert und jünger als die TTL ist.
fn cache_is_fresh(cache_path: &Path) -> CatalogResult<bool> {
    let meta = match fs::symlink_metadata(cache_path) {
        Ok(meta) => meta,
        Err(source) if source.kind() == ErrorKind::NotFound => return Ok(false),
        Err(source) => return Err(io_error(cache_path, source)),
    };
    reject_symlink(cache_path, &meta)?;
    let Ok(modified) = meta.modified() else {
        return Ok(false);
    };
    match SystemTime::now().duration_since(modified) {
        Ok(age) => Ok(age < Duration::from_secs(CACHE_TTL_SECS)),
        // Future timestamps must not suppress refresh indefinitely.
        Err(_) => Ok(false),
    }
}

/// Liest den Cache-Inhalt als String.
fn read_cache(cache_path: &Path) -> CatalogResult<String> {
    reject_path_symlink(cache_path)?;
    fs::read_to_string(cache_path).map_err(|source| io_error(cache_path, source))
}

/// Holt den models.dev-Katalog blockierend über HTTP.
fn fetch_remote() -> CatalogResult<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| CatalogError::Http(e.to_string()))?;
    let resp = client
        .get(MODELS_DEV_URL)
        .send()
        .map_err(|e| CatalogError::Http(e.to_string()))?;
    let resp = resp
        .error_for_status()
        .map_err(|e| CatalogError::Http(e.to_string()))?;
    resp.text().map_err(|e| CatalogError::Http(e.to_string()))
}

/// Schreibt den Cache atomar: temporäre Datei im selben Verzeichnis + Rename.
fn write_cache_atomic(cache_dir: &Path, cache_path: &Path, raw: &str) -> CatalogResult<()> {
    fs::create_dir_all(cache_dir).map_err(|source| io_error(cache_dir, source))?;
    reject_path_symlink(cache_path)?;

    let (tmp_path, mut tmp_file) = create_cache_tempfile(cache_dir)?;
    tmp_file
        .write_all(raw.as_bytes())
        .map_err(|source| io_error(&tmp_path, source))?;
    drop(tmp_file);

    // Der Zielpfad darf nicht durch einen Symlink ersetzt worden sein, bevor
    // die atomare Ersetzung stattfindet.
    reject_path_symlink(cache_path)?;
    fs::rename(&tmp_path, cache_path).map_err(|source| io_error(cache_path, source))
}

/// Erzeugt eine neue temporäre Datei im Cache-Verzeichnis, ohne bestehende
/// Artefakte zu öffnen oder zu überschreiben.
fn create_cache_tempfile(cache_dir: &Path) -> CatalogResult<(PathBuf, fs::File)> {
    for _ in 0..128 {
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let tmp_path = cache_dir.join(format!(
            ".{CACHE_FILE_NAME}.{}.{}.tmp",
            std::process::id(),
            sequence
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
        {
            Ok(file) => return Ok((tmp_path, file)),
            Err(source) if source.kind() == ErrorKind::AlreadyExists => continue,
            Err(source) => return Err(io_error(&tmp_path, source)),
        }
    }

    Err(io_error(
        cache_dir,
        io::Error::new(
            ErrorKind::AlreadyExists,
            "could not create a unique models.dev cache temporary file",
        ),
    ))
}

/// Prüft, ob ein vorhandenes Cache-Artefakt ein Symlink ist.
fn reject_path_symlink(path: &Path) -> CatalogResult<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => reject_symlink(path, &meta),
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error(path, source)),
    }
}

fn reject_symlink(path: &Path, meta: &fs::Metadata) -> CatalogResult<()> {
    if meta.file_type().is_symlink() {
        return Err(io_error(
            path,
            io::Error::new(
                ErrorKind::InvalidInput,
                "symlinked models.dev cache artifact",
            ),
        ));
    }
    Ok(())
}

fn cache_exists(cache_path: &Path) -> CatalogResult<bool> {
    match fs::symlink_metadata(cache_path) {
        Ok(meta) => {
            reject_symlink(cache_path, &meta)?;
            Ok(true)
        }
        Err(source) if source.kind() == ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error(cache_path, source)),
    }
}

fn io_error(path: &Path, source: io::Error) -> CatalogError {
    CatalogError::Io {
        path: path.display().to_string(),
        source,
    }
}

/// Maps harness provider names to the upstream catalog namespace.
fn models_dev_provider_id(id: &str) -> &str {
    match id {
        "together" => "togetherai",
        "fireworks" => "fireworks-ai",
        "moonshot" => "moonshotai",
        "zhipu" => "zhipuai",
        "cloudflare" => "cloudflare-workers-ai",
        "dashscope" => "alibaba",
        "gemini" => "google",
        _ => id,
    }
}

/// Ersetzt `models` jeder passenden [`ProviderSpec`] durch die models.dev-Ids.
fn apply_cache(cache: &ModelsDevCache, catalog: &mut [ProviderSpec]) {
    for provider in catalog.iter_mut() {
        // These lists depend on the user's installed models or deployments.
        if matches!(
            provider.id.as_str(),
            "ollama" | "lmstudio" | "vllm" | "custom" | "foundry"
        ) {
            continue;
        }
        if let Some(models) = cache.models_for(&provider.id) {
            if !models.is_empty() {
                provider.models = models;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{ProviderApi, ProviderSpec};
    use crate::test_support::{TestError, TestResult};

    fn test_cache_dir(label: &str) -> TestResult<PathBuf> {
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "harw-model-catalog-{label}-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// Baut eine minimale ProviderSpec für Tests.
    fn spec(id: &str, models: Vec<String>) -> ProviderSpec {
        ProviderSpec {
            id: id.to_owned(),
            name: id.to_owned(),
            base_url: "https://example.test".to_owned(),
            api: ProviderApi::OpenAiChat,
            auth: Vec::new(),
            default_model: None,
            featured: false,
            models,
        }
    }

    const SAMPLE: &str = r#"{
        "openai": { "models": { "gpt-4o": {}, "gpt-4o-mini": {} } },
        "anthropic": { "models": { "claude-3-5-sonnet": {} } },
        "empty": { "models": {} }
    }"#;

    #[test]
    fn refresh_bypasses_fresh_cache_and_invalid_response_preserves_it() -> TestResult {
        let dir = test_cache_dir("refresh")?;
        let path = dir.join(CACHE_FILE_NAME);
        fs::write(&path, SAMPLE)?;
        // Trap statt Panik: fetch darf bei frischem Cache (force=false) nicht
        // aufgerufen werden. Der Aufruf-Flag wird danach geprüft.
        let fetch_called = std::cell::Cell::new(false);
        load_cache_with_fetch(&dir, false, || {
            fetch_called.set(true);
            Err(CatalogError::Http("fetch must not be called".into()))
        })?;
        assert!(!fetch_called.get(), "fresh cache must avoid network");

        let fresh = r#"{"openai":{"models":{"new-model":{}}}}"#;
        let cache = load_cache_with_fetch(&dir, true, || Ok(fresh.into()))?
            .ok_or(TestError::Missing("cache after refresh"))?;
        assert_eq!(
            cache
                .models_for("openai")
                .ok_or(TestError::Missing("openai models"))?,
            ["new-model"]
        );
        let cache = load_cache_with_fetch(&dir, true, || Ok("invalid".into()))?
            .ok_or(TestError::Missing("cache after invalid response"))?;
        assert_eq!(
            cache
                .models_for("openai")
                .ok_or(TestError::Missing("openai models"))?,
            ["new-model"]
        );
        assert_eq!(fs::read_to_string(&path)?, fresh);
        let cache =
            load_cache_with_fetch(&dir, true, || Err(CatalogError::Http("offline".into())))?
                .ok_or(TestError::Missing("cache after offline fetch"))?;
        assert_eq!(
            cache
                .models_for("openai")
                .ok_or(TestError::Missing("openai models"))?,
            ["new-model"]
        );
        fs::write(&path, "broken cache")?;
        let cache = load_cache_with_fetch(&dir, false, || Ok(fresh.into()))?
            .ok_or(TestError::Missing("cache after broken-cache refresh"))?;
        assert_eq!(
            cache
                .models_for("openai")
                .ok_or(TestError::Missing("openai models"))?,
            ["new-model"]
        );
        fs::remove_dir_all(dir)?;
        Ok(())
    }

    #[test]
    fn aliases_and_text_filter_apply_to_provider_lists() -> TestResult {
        let cache = ModelsDevCache::from_json(
            r#"{
            "cloudflare-workers-ai": {"models": {
                "@cf/current": {"modalities": {"output": ["text"]}},
                "image-only": {"modalities": {"output": ["image"]}},
                "retired": {"status": "deprecated"}
            }},
            "togetherai": {"models": {"vendor/model": {}}},
            "lmstudio": {"models": {"not-installed": {}}}
        }"#,
        )?;
        let mut catalog = vec![
            spec("cloudflare", vec![]),
            spec("together", vec![]),
            spec("lmstudio", vec!["installed".into()]),
        ];
        apply_cache(&cache, &mut catalog);
        assert_eq!(catalog[0].models, ["@cf/current"]);
        assert_eq!(catalog[1].models, ["vendor/model"]);
        assert_eq!(catalog[2].models, ["installed"]);
        Ok(())
    }

    #[test]
    fn test_from_json_parses_provider_models() -> TestResult {
        let cache = ModelsDevCache::from_json(SAMPLE)?;
        let mut openai = cache
            .models_for("openai")
            .ok_or(TestError::Missing("openai models"))?;
        openai.sort();
        assert_eq!(openai, vec!["gpt-4o".to_owned(), "gpt-4o-mini".to_owned()]);
        assert_eq!(
            cache.models_for("anthropic"),
            Some(vec!["claude-3-5-sonnet".to_owned()])
        );
        assert_eq!(cache.models_for("missing"), None);
        Ok(())
    }

    #[test]
    fn test_from_json_rejects_invalid_json() -> TestResult {
        let Err(err) = ModelsDevCache::from_json("{ not json") else {
            return Err(TestError::Unexpected("Err erwartet (invalid json)".into()));
        };
        assert!(matches!(err, CatalogError::Parse(_)));
        Ok(())
    }

    #[test]
    fn test_apply_cache_replaces_matching_models() -> TestResult {
        let cache = ModelsDevCache::from_json(SAMPLE)?;
        let mut catalog = vec![
            spec("openai", vec!["stale".to_owned()]),
            spec("unknown", vec!["keep".to_owned()]),
            spec("empty", vec!["keep-empty".to_owned()]),
        ];
        apply_cache(&cache, &mut catalog);

        let mut openai_models = catalog[0].models.clone();
        openai_models.sort();
        assert_eq!(
            openai_models,
            vec!["gpt-4o".to_owned(), "gpt-4o-mini".to_owned()]
        );
        // Unbekannter Provider bleibt unverändert.
        assert_eq!(catalog[1].models, vec!["keep".to_owned()]);
        // Leere models.dev-Liste überschreibt bestehende Werte nicht.
        assert_eq!(catalog[2].models, vec!["keep-empty".to_owned()]);
        Ok(())
    }

    #[test]
    fn test_enrich_models_uses_fresh_cache_no_network() -> TestResult {
        let dir = test_cache_dir("fresh-cache")?;
        let cache_path = dir.join(CACHE_FILE_NAME);
        std::fs::write(&cache_path, SAMPLE)?;

        let mut catalog = vec![spec("openai", vec!["stale".to_owned()])];
        enrich_models(&dir, &mut catalog)?;

        let mut models = catalog[0].models.clone();
        models.sort();
        assert_eq!(models, vec!["gpt-4o".to_owned(), "gpt-4o-mini".to_owned()]);

        std::fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_enrich_models_rejects_symlinked_cache_without_reading_target() -> TestResult {
        use std::os::unix::fs::symlink;

        let dir = test_cache_dir("cache-symlink")?;
        let target = dir.join("outside-cache.json");
        std::fs::write(&target, SAMPLE)?;
        let cache_path = dir.join(CACHE_FILE_NAME);
        symlink(&target, &cache_path)?;

        let mut catalog = vec![spec("openai", vec!["stale".to_owned()])];
        let Err(err) = enrich_models(&dir, &mut catalog) else {
            return Err(TestError::Unexpected(
                "Err erwartet (symlinked cache must be rejected)".into(),
            ));
        };

        assert!(matches!(err, CatalogError::Io { .. }));
        assert_eq!(catalog[0].models, vec!["stale".to_owned()]);
        assert_eq!(std::fs::read_to_string(&target)?, SAMPLE);

        std::fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_write_cache_avoids_fixed_symlinked_temp_artifact() -> TestResult {
        use std::os::unix::fs::symlink;

        let dir = test_cache_dir("temp-symlink")?;
        let cache_path = dir.join(CACHE_FILE_NAME);
        let target = dir.join("outside-temp.json");
        std::fs::write(&target, "do not overwrite")?;
        let legacy_temp_path = cache_path.with_extension("json.tmp");
        symlink(&target, &legacy_temp_path)?;

        write_cache_atomic(&dir, &cache_path, SAMPLE)?;

        assert_eq!(std::fs::read_to_string(&cache_path)?, SAMPLE);
        assert_eq!(std::fs::read_to_string(&target)?, "do not overwrite");
        assert!(
            std::fs::symlink_metadata(&legacy_temp_path)?
                .file_type()
                .is_symlink()
        );

        std::fs::remove_dir_all(&dir).ok();
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn test_write_cache_rejects_symlinked_cache_without_replacing_target() -> TestResult {
        use std::os::unix::fs::symlink;

        let dir = test_cache_dir("write-cache-symlink")?;
        let target = dir.join("outside-cache.json");
        std::fs::write(&target, "do not replace")?;
        let cache_path = dir.join(CACHE_FILE_NAME);
        symlink(&target, &cache_path)?;

        let Err(err) = write_cache_atomic(&dir, &cache_path, SAMPLE) else {
            return Err(TestError::Unexpected(
                "Err erwartet (symlinked cache destination must be rejected)".into(),
            ));
        };

        assert!(matches!(err, CatalogError::Io { .. }));
        assert_eq!(std::fs::read_to_string(&target)?, "do not replace");
        assert!(
            std::fs::symlink_metadata(&cache_path)?
                .file_type()
                .is_symlink()
        );

        std::fs::remove_dir_all(&dir).ok();
        Ok(())
    }
}
