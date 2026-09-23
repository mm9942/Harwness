//! Provider-Katalog-Spezifikationstypen.
//!
//! Diese Modul-Datei implementiert die im Contract-Master
//! (`docs/design/CONTRACT-setup-install.md`, Abschnitt „Crate
//! `harw-model-catalog` / `src/spec.rs`") festgelegten, serde-getriebenen
//! Kern-Datenstrukturen des Modellkatalogs.
//!
//! # Verantwortung
//! Dieses Modul besitzt ausschließlich die Deklaration der reinen
//! Daten-Typen [`ProviderApi`], [`AuthMethod`] und [`ProviderSpec`]. Es
//! delegiert das Laden/Parsen an `embedded.rs` und die Fehlerbehandlung an
//! `error.rs`.
//!
//! # Exportierte Typen
//! - [`ProviderApi`] — Transport-/API-Familie eines Providers (kebab-case serde).
//! - [`AuthMethod`] — internally-tagged Authentifizierungsverfahren.
//! - [`ProviderSpec`] — vollständige Beschreibung eines Katalog-Providers.
//!
//! # Concurrency
//! Alle Typen sind reine Wert-Typen (`Send + Sync`, keine inneren Locks,
//! keine geteilten Zustände). Sie sind gefahrlos zwischen Threads teilbar und
//! per `Clone` kopierbar.
//!
//! # Fehler
//! Dieses Modul erzeugt selbst keine Fehler; De-/Serialisierungsfehler werden
//! von den aufrufenden Modulen über den crate-weiten Fehlertyp behandelt.
//!
//! # Examples
//! ```
//! use harw_model_catalog::spec::{ProviderApi, ProviderSpec};
//!
//! let spec = ProviderSpec {
//!     id: "openai".to_owned(),
//!     name: "OpenAI".to_owned(),
//!     base_url: "https://api.openai.com/v1".to_owned(),
//!     api: ProviderApi::OpenAiResponses,
//!     auth: Vec::new(),
//!     default_model: None,
//!     featured: true,
//!     models: Vec::new(),
//! };
//! assert_eq!(spec.api, ProviderApi::OpenAiResponses);
//! ```

use serde::{Deserialize, Serialize};

/// Transport- bzw. API-Familie, über die ein Provider angesprochen wird.
///
/// # Description
/// Bestimmt, welcher Request-/Response-Pfad für einen Provider verwendet wird
/// (z. B. der OpenAI-`/responses`-Pfad gegenüber dem klassischen Chat-Pfad).
/// Contract-Quelle: `src/spec.rs` im Contract-Master.
///
/// # Concurrency
/// `Copy`-Wert ohne inneren Zustand; uneingeschränkt thread-sicher.
///
/// # Examples
/// ```
/// use harw_model_catalog::spec::ProviderApi;
///
/// let json = serde_json::to_string(&ProviderApi::AnthropicMessages).unwrap();
/// assert_eq!(json, "\"anthropic-messages\"");
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderApi {
    /// OpenAI-`/responses`-Transport.
    #[serde(rename = "openai-responses")]
    OpenAiResponses,
    /// Klassischer OpenAI-`/chat/completions`-Transport.
    #[serde(rename = "openai-chat")]
    OpenAiChat,
    /// Anthropic-`/messages`-Transport.
    #[serde(rename = "anthropic-messages")]
    AnthropicMessages,
    /// Lokaler Ollama-Transport.
    #[serde(rename = "ollama")]
    Ollama,
}

/// Verfahren, mit dem Zugangsdaten für einen Provider beschafft werden.
///
/// # Description
/// Internally-tagged Enum (serde-Feld `method`, kebab-case). Jede Variante
/// beschreibt eine mögliche Herkunft der Authentifizierung. Contract-Quelle:
/// `src/spec.rs` im Contract-Master.
///
/// # Concurrency
/// Reiner Wert-Typ ohne geteilten Zustand; thread-sicher.
///
/// # Examples
/// ```
/// use harw_model_catalog::spec::AuthMethod;
///
/// let auth = AuthMethod::ApiKey { env_vars: vec!["OPENAI_API_KEY".to_owned()] };
/// let json = serde_json::to_string(&auth).unwrap();
/// assert!(json.contains("\"method\":\"api-key\""));
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "kebab-case")]
pub enum AuthMethod {
    /// API-Key aus einer der genannten Umgebungsvariablen.
    ApiKey {
        /// Kandidaten-Umgebungsvariablen, in Reihenfolge der Priorität.
        env_vars: Vec<String>,
    },
    /// Import lokaler Zugangsdaten anhand von `CredentialSource`-Ids.
    LocalImport {
        /// Ids aus `CredentialSource`, die als Quelle dienen.
        sources: Vec<String>,
    },
    /// Lokaler Provider ohne Key, nur über eine Basis-URL erreichbar.
    LocalBaseUrl,
    /// Benutzerdefiniertes Verfahren (manuelle Konfiguration).
    Custom,
}

/// Vollständige Beschreibung eines Providers im Modellkatalog.
///
/// # Description
/// Aggregiert Identität, Endpunkt, Transportwahl, Authentifizierungsoptionen
/// und Modell-Liste eines Providers. Optionale Felder tragen `#[serde(default)]`,
/// sodass minimale TOML-/JSON-Einträge ausreichen. Contract-Quelle:
/// `src/spec.rs` im Contract-Master.
///
/// # Concurrency
/// Reiner Wert-Typ; `Send + Sync`, per `Clone` kopierbar, thread-sicher.
///
/// # Examples
/// ```
/// use harw_model_catalog::spec::{ProviderApi, ProviderSpec};
///
/// let toml = r#"
/// id = "ollama"
/// name = "Ollama"
/// base_url = "http://localhost:11434"
/// api = "ollama"
/// "#;
/// let spec: ProviderSpec = toml::from_str(toml).unwrap();
/// assert_eq!(spec.api, ProviderApi::Ollama);
/// assert!(!spec.featured);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderSpec {
    /// Eindeutige Provider-Id (z. B. `"openai"`).
    pub id: String,
    /// Menschenlesbarer Anzeigename.
    pub name: String,
    /// Basis-URL des Provider-Endpunkts.
    pub base_url: String,
    /// Transport-/API-Familie des Providers.
    pub api: ProviderApi,
    /// Unterstützte Authentifizierungsverfahren (Default: leer).
    #[serde(default)]
    pub auth: Vec<AuthMethod>,
    /// Standardmodell, falls definiert (Default: `None`).
    #[serde(default)]
    pub default_model: Option<String>,
    /// Ob der Provider hervorgehoben angezeigt wird (Default: `false`).
    #[serde(default)]
    pub featured: bool,
    /// Bekannte Modell-Ids dieses Providers (Default: leer).
    #[serde(default)]
    pub models: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    #[test]
    fn test_provider_api_kebab_case_serde() -> TestResult {
        // Jede Variante muss kebab-case serialisieren und wieder zurückparsen.
        let cases = [
            (ProviderApi::OpenAiResponses, "\"openai-responses\""),
            (ProviderApi::OpenAiChat, "\"openai-chat\""),
            (ProviderApi::AnthropicMessages, "\"anthropic-messages\""),
            (ProviderApi::Ollama, "\"ollama\""),
        ];
        for (variant, expected) in cases {
            let json = serde_json::to_string(&variant)?;
            assert_eq!(json, expected);
            let back: ProviderApi = serde_json::from_str(&json)?;
            assert_eq!(back, variant);
        }
        Ok(())
    }

    #[test]
    fn test_auth_method_tagged_serde() -> TestResult {
        let auth = AuthMethod::ApiKey {
            env_vars: vec!["OPENAI_API_KEY".to_owned(), "OPENAI_KEY".to_owned()],
        };
        let json = serde_json::to_string(&auth)?;
        assert!(
            json.contains("\"method\":\"api-key\""),
            "tag present: {json}"
        );
        assert!(json.contains("OPENAI_API_KEY"));
        let back: AuthMethod = serde_json::from_str(&json)?;
        assert_eq!(back, auth);
        Ok(())
    }

    #[test]
    fn test_auth_method_unit_variants_tagged_serde() -> TestResult {
        let local = AuthMethod::LocalBaseUrl;
        let json = serde_json::to_string(&local)?;
        assert_eq!(json, "{\"method\":\"local-base-url\"}");
        let back: AuthMethod = serde_json::from_str(&json)?;
        assert_eq!(back, local);

        let custom: AuthMethod = serde_json::from_str("{\"method\":\"custom\"}")?;
        assert_eq!(custom, AuthMethod::Custom);
        Ok(())
    }

    #[test]
    fn test_provider_spec_toml_roundtrip() -> TestResult {
        let original = ProviderSpec {
            id: "openai".to_owned(),
            name: "OpenAI".to_owned(),
            base_url: "https://api.openai.com/v1".to_owned(),
            api: ProviderApi::OpenAiResponses,
            auth: vec![AuthMethod::ApiKey {
                env_vars: vec!["OPENAI_API_KEY".to_owned()],
            }],
            default_model: Some("gpt-4o".to_owned()),
            featured: true,
            models: vec!["gpt-4o".to_owned(), "gpt-4o-mini".to_owned()],
        };
        let serialized = toml::to_string(&original)?;
        let back: ProviderSpec = toml::from_str(&serialized)?;
        assert_eq!(back, original);
        Ok(())
    }

    #[test]
    fn test_provider_spec_toml_defaults() -> TestResult {
        // Nur Pflichtfelder gesetzt; alle #[serde(default)]-Felder greifen.
        let toml = r#"
            id = "ollama"
            name = "Ollama"
            base_url = "http://localhost:11434"
            api = "ollama"
        "#;
        let spec: ProviderSpec = toml::from_str(toml)?;
        assert_eq!(spec.id, "ollama");
        assert_eq!(spec.api, ProviderApi::Ollama);
        assert!(spec.auth.is_empty());
        assert_eq!(spec.default_model, None);
        assert!(!spec.featured);
        assert!(spec.models.is_empty());
        Ok(())
    }
}
