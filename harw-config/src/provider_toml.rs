use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::auth_toml::SecretRef;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderToml {
    pub name: String,
    pub api: String,
    pub base_url: String,
    /// Bevorzugtes Feld: eine `SecretRef` (`env:`/`file:`/`keyring:`/`secrets:`),
    /// niemals ein literaler Schlüssel.
    #[serde(default)]
    pub auth: Option<SecretRef>,
    /// Credential transport: bearer, api-key, x-api-key or none.
    /// Omitted preserves the transport's compatible default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_header: Option<String>,
    /// DEPRECATED: literaler API-Key im Klartext. Wird weiterhin geparst
    /// (Rückwärtskompatibilität), aber `harw doctor` markiert jedes
    /// nicht-leere Vorkommen als `ConfigError::PlaintextSecret`.
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub origin_allowlist: OriginAllowlistToml,
}

impl ProviderToml {
    /// `true`, wenn dieser Provider noch den veralteten `api_key`-Klartext
    /// verwendet und daher von `harw doctor` als Verstoß markiert werden muss.
    #[must_use]
    pub fn has_plaintext_secret(&self) -> bool {
        self.api_key.as_ref().is_some_and(|k| !k.is_empty())
    }
}

/// Welche Agenten/Channels über diesen Provider routen dürfen. Leer/fehlend
/// bedeutet "keine Einschränkung".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginAllowlistToml {
    #[serde(default)]
    pub agents: Vec<String>,
    #[serde(default)]
    pub channels: Vec<String>,
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_with_auth_secretref() {
        let src = r#"
            name = "openai"
            api = "openai-chat"
            base_url = "https://api.openai.com/v1"
            auth = "env:OPENAI_API_KEY"
        "#;
        let provider: ProviderToml = toml::from_str(src).unwrap();
        assert!(!provider.has_plaintext_secret());
    }

    #[test]
    fn test_provider_legacy_api_key_flagged() {
        let src = r#"
            name = "old-style"
            api = "openai-chat"
            base_url = "https://api.openai.com/v1"
            api_key = "sk-literal-value-here"
        "#;
        let provider: ProviderToml = toml::from_str(src).unwrap();
        assert!(provider.has_plaintext_secret());
    }

    #[test]
    fn test_provider_rejects_misspelled_auth_field() {
        let src = r#"
            name = "openai"
            api = "openai-chat"
            base_url = "https://api.openai.com/v1"
            auht = "env:OPENAI_API_KEY"
        "#;

        let error = toml::from_str::<ProviderToml>(src).unwrap_err();
        assert!(error.to_string().contains("unknown field `auht`"));
    }

    #[test]
    fn test_provider_rejects_misspelled_api_field() {
        let src = r#"
            name = "openai"
            ap = "openai-chat"
            base_url = "https://api.openai.com/v1"
        "#;

        let error = toml::from_str::<ProviderToml>(src).unwrap_err();
        assert!(error.to_string().contains("unknown field `ap`"));
    }
}
