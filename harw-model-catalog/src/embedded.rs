//! Eingebetteter Provider-Katalog.
//!
//! Lädt die im Binary eingebettete `providers.toml` (via `include_str!`) und
//! liefert sie als `Vec<ProviderSpec>`. Der eingebettete Katalog ist die
//! offline-Basis; `models_dev::enrich_models` kann die Modell-Listen zur
//! Laufzeit anreichern.

use serde::Deserialize;

use crate::spec::ProviderSpec;

/// Rohtext der eingebetteten Katalog-Datei.
const EMBEDDED: &str = include_str!("providers.toml");

/// Deserialisierungs-Hülle für die `[[provider]]`-Tabellen.
#[derive(Debug, Deserialize)]
struct Catalog {
    #[serde(default)]
    provider: Vec<ProviderSpec>,
}

/// Gibt den eingebetteten Provider-Katalog zurück.
///
/// # Returns
/// Alle in `providers.toml` deklarierten [`ProviderSpec`]-Einträge. Bei einem
/// (durch Tests ausgeschlossenen) Parse-Fehler des eingebetteten TOML wird eine
/// leere Liste zurückgegeben, statt zu paniken.
///
/// # Examples
/// ```
/// let catalog = harw_model_catalog::embedded::embedded_catalog();
/// assert!(catalog.iter().any(|p| p.id == "openai"));
/// ```
#[must_use]
pub fn embedded_catalog() -> Vec<ProviderSpec> {
    match toml::from_str::<Catalog>(EMBEDDED) {
        Ok(catalog) => catalog.provider,
        // Diese Crate hat keine `tracing`-Abhängigkeit (Cargo.toml geprüft),
        // daher hier ersatzlos kein Logging statt `debug_assert!(false, …)`
        // (Bible R087/R165: kein Panic, auch nicht in Debug-Builds). Der
        // Release-Fallback (leere Liste) bleibt unverändert; Parsbarkeit ist
        // über `embedded_catalog_parses_and_is_nonempty` abgedeckt.
        Err(_error) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult};

    #[test]
    fn embedded_catalog_parses_and_is_nonempty() {
        let catalog = embedded_catalog();
        assert!(
            catalog.len() >= 15,
            "eingebetteter Katalog sollte ~18 Provider haben, war {}",
            catalog.len()
        );
    }

    #[test]
    fn embedded_catalog_contains_core_providers() {
        let catalog = embedded_catalog();
        for id in ["openai", "anthropic", "openrouter", "ollama", "custom"] {
            assert!(catalog.iter().any(|p| p.id == id), "fehlt: {id}");
        }
    }

    #[test]
    fn embedded_catalog_has_current_snapshot() -> TestResult {
        let catalog = embedded_catalog();
        let openai = catalog
            .iter()
            .find(|p| p.id == "openai")
            .ok_or(TestError::Missing("openai provider"))?;
        assert!(openai.models.iter().any(|id| id == "gpt-6-astra"));
        let cloudflare = catalog
            .iter()
            .find(|p| p.id == "cloudflare")
            .ok_or(TestError::Missing("cloudflare provider"))?;
        assert!(!cloudflare.models.is_empty());
        Ok(())
    }

    #[test]
    fn embedded_catalog_contains_expected_providers() {
        let cat = embedded_catalog();
        for id in [
            "openai",
            "anthropic",
            "xai",
            "mistral",
            "deepseek",
            "zai",
            "moonshot",
            "dashscope",
            "cerebras",
            "gemini",
            "groq",
            "together",
            "fireworks",
            "deepinfra",
        ] {
            assert!(cat.iter().any(|p| p.id == id), "missing provider: {id}");
        }
    }

    #[test]
    fn embedded_catalog_zai_has_glm_4_6() -> TestResult {
        let cat = embedded_catalog();
        let zai = cat
            .iter()
            .find(|p| p.id == "zai")
            .ok_or(TestError::Missing("zai provider"))?;
        assert_eq!(zai.default_model.as_deref(), Some("glm-4.6"));
        assert!(zai.models.iter().any(|m| m == "glm-4.6"));
        Ok(())
    }

    #[test]
    fn embedded_catalog_moonshot_has_kimi() -> TestResult {
        let cat = embedded_catalog();
        let m = cat
            .iter()
            .find(|p| p.id == "moonshot")
            .ok_or(TestError::Missing("moonshot provider"))?;
        assert!(m.models.iter().any(|x| x == "kimi-k2.7-code"));
        Ok(())
    }
}
