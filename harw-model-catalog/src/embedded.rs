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
        Err(error) => {
            debug_assert!(false, "eingebettetes providers.toml parst nicht: {error}");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn embedded_catalog_has_current_snapshot() {
        let catalog = embedded_catalog();
        let openai = catalog.iter().find(|p| p.id == "openai").unwrap();
        assert!(openai.models.iter().any(|id| id == "gpt-6-astra"));
        let cloudflare = catalog.iter().find(|p| p.id == "cloudflare").unwrap();
        assert!(!cloudflare.models.is_empty());
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
    fn embedded_catalog_zai_has_glm_4_6() {
        let cat = embedded_catalog();
        let zai = cat.iter().find(|p| p.id == "zai").expect("zai present");
        assert_eq!(zai.default_model.as_deref(), Some("glm-4.6"));
        assert!(zai.models.iter().any(|m| m == "glm-4.6"));
    }

    #[test]
    fn embedded_catalog_moonshot_has_kimi() {
        let cat = embedded_catalog();
        let m = cat
            .iter()
            .find(|p| p.id == "moonshot")
            .expect("moonshot present");
        assert!(m.models.iter().any(|x| x == "kimi-k2.7-code"));
    }
}
