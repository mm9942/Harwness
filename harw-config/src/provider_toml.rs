use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::auth_toml::SecretRef;
use crate::error::{ConfigError, ConfigResult};

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
    /// Client-seitiges Rate-Limiting für diesen Provider; `None` = kein
    /// Override (deaktiviert, siehe [`RateLimitToml::default`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<RateLimitToml>,
}

impl ProviderToml {
    /// `true`, wenn dieser Provider noch den veralteten `api_key`-Klartext
    /// verwendet und daher von `harw doctor` als Verstoß markiert werden muss.
    #[must_use]
    pub fn has_plaintext_secret(&self) -> bool {
        self.api_key.as_ref().is_some_and(|k| !k.is_empty())
    }

    /// `true`, wenn ein Header dieses Namens Credentials trägt und daher nur
    /// als [`SecretRef`] konfiguriert werden darf.
    ///
    /// Regel (ASCII-case-insensitiv): der Name enthält `authorization` (deckt
    /// `authorization`, `proxy-authorization`, `cf-aig-authorization` ab),
    /// endet auf `-key` (`x-api-key`, `api-key`) oder enthält `token`.
    /// `harw-provider-http` nutzt dieselbe Regel, um solche Header aufzulösen
    /// und als sensitiv zu markieren.
    #[must_use]
    pub fn is_sensitive_header_name(name: &str) -> bool {
        let name = name.trim().to_ascii_lowercase();
        name.contains("authorization") || name.ends_with("-key") || name.contains("token")
    }

    /// Prüft Invarianten, die die TOML-Deserialisierung nicht ausdrücken kann.
    ///
    /// Derzeit: Header mit Credential-Namen (siehe
    /// [`Self::is_sensitive_header_name`]) müssen eine gültige [`SecretRef`]
    /// (`env:`/`file:`/`file-json:`/`keyring:`/`secrets:`) sein, nie Klartext.
    ///
    /// # Errors
    /// [`ConfigError::PlaintextSecret`] mit Header-Namen als Feld; der Wert
    /// erscheint nie im Fehler. Bei mehreren Verstößen wird der lexikographisch
    /// kleinste Header-Name gemeldet (deterministisch trotz `HashMap`).
    pub fn validate(&self) -> ConfigResult<()> {
        let mut headers: Vec<(&String, &String)> = self.headers.iter().collect();
        headers.sort_by(|left, right| left.0.cmp(right.0));
        for (name, value) in headers {
            if Self::is_sensitive_header_name(name) && value.parse::<SecretRef>().is_err() {
                return Err(ConfigError::PlaintextSecret {
                    file: format!("providers/{}.toml", self.name),
                    field: format!("headers.{name}"),
                });
            }
        }
        Ok(())
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

/// Default-Sicherheitsmarge für client-seitiges Rate-Limiting in Prozent.
fn default_safety_margin_pct() -> u8 {
    10
}

/// Client-seitiges Rate-Limiting-Konfiguration für einen Provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimitToml {
    /// `true` aktiviert client-seitiges Rate-Limiting/Throttling.
    #[serde(default)]
    pub enabled: bool,
    /// Sicherheitsmarge (Prozent) unterhalb des vom Provider gemeldeten
    /// Limits, die eingehalten wird, bevor gewartet wird.
    #[serde(default = "default_safety_margin_pct")]
    pub safety_margin_pct: u8,
}

impl Default for RateLimitToml {
    fn default() -> Self {
        Self {
            enabled: false,
            safety_margin_pct: default_safety_margin_pct(),
        }
    }
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

    fn provider_with_headers(headers: &[(&str, &str)]) -> ProviderToml {
        let src = r#"
            name = "gateway"
            api = "openai-chat"
            base_url = "https://gateway.example/v1"
            auth = "env:GATEWAY_KEY"
        "#;
        let mut provider: ProviderToml = toml::from_str(src).unwrap();
        provider.headers = headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        provider
    }

    #[test]
    fn test_validate_rejects_plaintext_credential_headers() {
        for name in [
            "authorization",
            "Authorization",
            "cf-aig-authorization",
            "x-api-key",
            "API-KEY",
            "x-auth-token",
            "X-Session-Token-Id",
        ] {
            let provider = provider_with_headers(&[(name, "Bearer plaintext-header-secret")]);
            let error = provider.validate().expect_err(name);
            assert!(
                matches!(&error, ConfigError::PlaintextSecret { field, .. }
                    if *field == format!("headers.{name}")),
                "{name}: {error}"
            );
            assert!(!error.to_string().contains("plaintext-header-secret"));
        }
    }

    #[test]
    fn test_validate_accepts_secret_ref_credential_headers_and_plain_other_headers() {
        let provider = provider_with_headers(&[
            ("authorization", "env:GATEWAY_BEARER"),
            ("x-api-key", "secrets:gateway/api-key"),
            ("cf-aig-token", "file:/home/mia/.harw/secrets/cf.token"),
            ("x-provider-marker", "plain-value"),
            ("keyboard", "not-a-credential"),
        ]);
        provider.validate().expect("secret refs and ordinary headers are valid");
    }

    #[test]
    fn test_validate_rejects_malformed_secret_ref_and_reports_smallest_name() {
        let provider = provider_with_headers(&[
            ("x-api-key", "env:"),
            ("authorization", "unknown:value"),
        ]);
        let error = provider.validate().unwrap_err();
        assert!(matches!(
            error,
            ConfigError::PlaintextSecret { ref field, .. } if field == "headers.authorization"
        ));
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

    #[test]
    fn test_provider_with_rate_limit_section_parses_and_defaults_margin() {
        let src = r#"
            name = "openai"
            api = "openai-chat"
            base_url = "https://api.openai.com/v1"

            [rate_limit]
            enabled = true
        "#;
        let provider: ProviderToml = toml::from_str(src).unwrap();
        let rate_limit = provider.rate_limit.expect("rate_limit section present");
        assert!(rate_limit.enabled);
        assert_eq!(rate_limit.safety_margin_pct, 10);
    }

    #[test]
    fn test_provider_without_rate_limit_section_is_none() {
        let src = r#"
            name = "openai"
            api = "openai-chat"
            base_url = "https://api.openai.com/v1"
        "#;
        let provider: ProviderToml = toml::from_str(src).unwrap();
        assert!(provider.rate_limit.is_none());
    }
}
