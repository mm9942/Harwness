//! Materialisierung des eingebetteten Provider-Katalogs als Profil-Dateien.
//!
//! # Verantwortung
//! Dieses Modul schreibt für jeden Provider aus [`crate::embedded_catalog`] eine
//! `providers/<id>.toml` in ein Profilverzeichnis — **deaktiviert** und **ohne
//! Schlüssel**. Es besitzt nicht die Auswahl eines Providers (das leistet das
//! Onboarding) und löst keine Secrets auf.
//!
//! # Warum deaktiviert
//! Ein vorgesäter Provider ist eine Einladung, kein Zustand: er zeigt dem Nutzer
//! Endpunkt, Transport und Modellliste, und nennt in `auth` die Umgebungsvariable,
//! die er erwartet. Scharf wird er erst durch das Onboarding oder
//! `harw settings provider enable <id>`. Damit kann kein Provider mit
//! Platzhalter-URL oder fehlendem Credential versehentlich einen Request
//! auslösen.
//!
//! # Nebenläufigkeit
//! Zustandslos; die Funktion synchronisiert nicht gegen konkurrente Schreiber.
//! Ein `harw`-Prozess pro Root-Space ist die erwartete Nutzung.
//!
//! # Fehler
//! [`CatalogError::Io`] beim Anlegen des Verzeichnisses oder Schreiben einer
//! Datei, [`CatalogError::Parse`] wenn ein Katalogeintrag nicht serialisierbar
//! ist.

use std::path::{Path, PathBuf};

use harw_config::{OriginAllowlistToml, ProviderToml, SecretRef};

use crate::error::{CatalogError, CatalogResult};
use crate::spec::{AuthMethod, ProviderSpec};

/// Schreibt den eingebetteten Provider-Katalog als deaktivierte Profil-Dateien.
///
/// # Description
/// Für jeden Eintrag aus [`crate::embedded_catalog`] entsteht
/// `<profile_dir>/providers/<id>.toml` mit `enabled = false` und — sofern der
/// Katalog eine [`AuthMethod::ApiKey`] nennt — einer `env:`-Referenz auf deren
/// erste Umgebungsvariable. Eine bereits vorhandene Datei wird **nie**
/// überschrieben, damit eine vom Nutzer konfigurierte oder vom Onboarding
/// scharf geschaltete Fassung erhalten bleibt.
///
/// # Arguments
/// - `profile_dir` (`&Path`): Verzeichnis des aktiven Profils, üblicherweise
///   `~/.harw/profiles/<name>`.
///
/// # Returns
/// Die Pfade der tatsächlich neu geschriebenen Dateien, in Katalogreihenfolge.
/// Ein Re-Run auf einem vollständigen Profil liefert einen leeren Vektor.
///
/// # Errors
/// - [`CatalogError::Io`]: `providers/` nicht anlegbar oder eine Datei nicht
///   schreibbar.
/// - [`CatalogError::Parse`]: ein Katalogeintrag ließ sich nicht nach TOML
///   serialisieren.
///
/// # Concurrency
/// Kein Locking gegen konkurrente Schreiber; aus einem Prozess heraus sicher.
///
/// # Examples
/// ```rust,no_run
/// let profile = std::path::Path::new("/home/u/.harw/profiles/default");
/// let written = harw_model_catalog::seed_profile_providers(profile)?;
/// println!("{} Provider vorbereitet", written.len());
/// # Ok::<(), harw_model_catalog::CatalogError>(())
/// ```
pub fn seed_profile_providers(profile_dir: &Path) -> CatalogResult<Vec<PathBuf>> {
    let providers_dir = profile_dir.join("providers");
    std::fs::create_dir_all(&providers_dir)
        .map_err(|error| CatalogError::io(providers_dir.display().to_string(), error))?;

    let mut written = Vec::new();
    for spec in crate::embedded_catalog() {
        let target = providers_dir.join(format!("{}.toml", spec.id));
        if target.exists() {
            continue;
        }
        let contents = toml::to_string_pretty(&provider_toml_from(&spec)).map_err(|error| {
            CatalogError::Parse(format!(
                "Provider '{}' liess sich nicht serialisieren: {error}",
                spec.id
            ))
        })?;
        std::fs::write(&target, contents)
            .map_err(|error| CatalogError::io(target.display().to_string(), error))?;
        written.push(target);
    }
    Ok(written)
}

/// Bildet einen Katalogeintrag auf seine deaktivierte Profil-Repräsentation ab.
fn provider_toml_from(spec: &ProviderSpec) -> ProviderToml {
    ProviderToml {
        stream: None,
        name: spec.id.clone(),
        api: api_name(spec),
        base_url: spec.base_url.clone(),
        auth: env_auth_ref(spec),
        auth_header: None,
        api_key: None,
        headers: std::collections::HashMap::new(),
        models: spec.models.clone(),
        enabled: false,
        origin_allowlist: OriginAllowlistToml::default(),
        rate_limit: None,
        max_concurrency: None,
        originator: None,
        default_reasoning_effort: None,
        gateway_identity_headers: false,
    }
}

/// Liefert den serde-Namen des Transports (`"openai-responses"`, …).
///
/// `ProviderApi` trägt seine kanonischen Namen in den serde-Attributen; sie hier
/// erneut als `match` zu schreiben wäre eine zweite Wahrheit. Der Umweg über
/// `serde_json` liest deshalb genau den Namen, den auch `ProviderToml::api`
/// beim Laden erwartet.
fn api_name(spec: &ProviderSpec) -> String {
    serde_json::to_value(spec.api)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "openai-chat".to_owned())
}

/// Erste im Katalog genannte Umgebungsvariable als `env:`-Referenz.
///
/// Provider ohne [`AuthMethod::ApiKey`] — lokale Endpunkte
/// (`AuthMethod::LocalBaseUrl`) und manuell einzurichtende
/// (`AuthMethod::Custom`) — liefern `None`.
fn env_auth_ref(spec: &ProviderSpec) -> Option<SecretRef> {
    spec.auth.iter().find_map(|method| match method {
        AuthMethod::ApiKey { env_vars } => {
            env_vars.first().map(|name| SecretRef::Env(name.clone()))
        }
        AuthMethod::LocalImport { .. } | AuthMethod::LocalBaseUrl | AuthMethod::Custom => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    fn temporary_profile() -> TestResult<PathBuf> {
        let dir = std::env::temp_dir().join(format!(
            "harw-seed-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(ctx("Systemzeit nach 1970"))?
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    #[test]
    fn test_seed_writes_one_file_per_catalog_provider() -> TestResult {
        let profile = temporary_profile()?;

        let written = seed_profile_providers(&profile)?;

        assert_eq!(written.len(), crate::embedded_catalog().len());
        for spec in crate::embedded_catalog() {
            assert!(
                profile
                    .join("providers")
                    .join(format!("{}.toml", spec.id))
                    .is_file(),
                "providers/{}.toml fehlt",
                spec.id
            );
        }

        std::fs::remove_dir_all(&profile)?;
        Ok(())
    }

    #[test]
    fn test_every_seeded_provider_is_disabled_and_carries_no_literal_key() -> TestResult {
        let profile = temporary_profile()?;

        seed_profile_providers(&profile)?;

        for spec in crate::embedded_catalog() {
            let path = profile.join("providers").join(format!("{}.toml", spec.id));
            let provider: ProviderToml = toml::from_str(&std::fs::read_to_string(&path)?)?;
            assert!(!provider.enabled, "{} darf nicht aktiv sein", spec.id);
            assert!(
                provider.api_key.is_none(),
                "{} trägt einen Klartextschlüssel",
                spec.id
            );
            provider.validate().map_err(|error| TestError::Context {
                context: "provider validate fehlgeschlagen",
                source: format!("{}: {error}", spec.id),
            })?;
        }

        std::fs::remove_dir_all(&profile)?;
        Ok(())
    }

    #[test]
    fn test_seeded_api_key_provider_gets_env_reference() -> TestResult {
        let profile = temporary_profile()?;

        seed_profile_providers(&profile)?;

        let anthropic: ProviderToml = toml::from_str(&std::fs::read_to_string(
            profile.join("providers").join("anthropic.toml"),
        )?)?;

        assert_eq!(anthropic.api, "anthropic-messages");
        assert!(matches!(anthropic.auth, Some(SecretRef::Env(_))));
        assert!(!anthropic.models.is_empty());

        std::fs::remove_dir_all(&profile)?;
        Ok(())
    }

    /// Lokale und manuell einzurichtende Provider haben keine Umgebungsvariable,
    /// auf die sich eine Referenz sinnvoll beziehen könnte.
    #[test]
    fn test_local_and_custom_providers_are_seeded_without_auth() -> TestResult {
        let profile = temporary_profile()?;

        seed_profile_providers(&profile)?;

        for id in ["ollama", "lmstudio", "custom", "cf-worker"] {
            let provider: ProviderToml = toml::from_str(&std::fs::read_to_string(
                profile.join("providers").join(format!("{id}.toml")),
            )?)?;
            assert!(
                provider.auth.is_none(),
                "{id} sollte keine auth-Referenz tragen"
            );
        }

        std::fs::remove_dir_all(&profile)?;
        Ok(())
    }

    #[test]
    fn test_seed_never_overwrites_an_existing_provider_file() -> TestResult {
        let profile = temporary_profile()?;
        let providers = profile.join("providers");
        std::fs::create_dir_all(&providers)?;
        let mine = providers.join("openai.toml");
        let contents = "name = \"openai\"\napi = \"openai-responses\"\nbase_url = \"https://example.invalid/v1\"\nenabled = true\n";
        std::fs::write(&mine, contents)?;

        let written = seed_profile_providers(&profile)?;

        assert!(!written.contains(&mine));
        assert_eq!(
            std::fs::read_to_string(&mine)?,
            contents,
            "eine vorhandene Provider-Datei darf nie überschrieben werden"
        );

        std::fs::remove_dir_all(&profile)?;
        Ok(())
    }

    #[test]
    fn test_rerun_writes_nothing() -> TestResult {
        let profile = temporary_profile()?;

        seed_profile_providers(&profile)?;
        let second = seed_profile_providers(&profile)?;

        assert!(second.is_empty());

        std::fs::remove_dir_all(&profile)?;
        Ok(())
    }
}
