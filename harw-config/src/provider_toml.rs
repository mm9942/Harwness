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
    /// Harte Obergrenze gleichzeitig in Flug befindlicher Requests an
    /// diesen Provider; `None` = unbegrenzt. Anders als `rate_limit`
    /// (reaktives Header-Pacing) ist dies ein rein client-seitiger
    /// Zähler, der zusätzliche Requests blockiert statt sie fehlschlagen
    /// zu lassen. `Some(0)` ist ungültig (siehe [`Self::validate`]) und
    /// würde jeden Request auf ewig blockieren.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<usize>,
    /// Optionaler Wert für den `originator`-HTTP-Header, den `harw` auf der
    /// Codex-/ChatGPT-Route (`harw-provider-http::codex`) an OpenAI sendet.
    /// `None` behält den harw-eigenen Default `"harw"` bei.
    ///
    /// **Wichtig:** Dieser Wert dient bei OpenAI ausschließlich der
    /// Client-Identifikation und wird bei jedem Request im Klartext
    /// mitgeschickt. Ihn auf den Wert des offiziellen Codex-CLI-Clients zu
    /// setzen, um wie dieser Client zu erscheinen, ist eine bewusste
    /// Entscheidung der Nutzerin/des Nutzers — sie kann im Widerspruch zu
    /// OpenAIs Nutzungsbedingungen stehen. `harw` erzwingt hier keine
    /// bestimmte Wahl, validiert den Wert aber (siehe [`Self::validate`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub originator: Option<String>,
}

/// Höchstlänge des `originator`-Felds (siehe [`ProviderToml::validate`]).
const MAX_ORIGINATOR_CHARS: usize = 64;

/// `true`, wenn `value` nicht leer ist und ausschließlich druckbare ASCII-
/// Zeichen (0x20–0x7E) enthält — also ohne Steuerzeichen (Tab, Zeilenumbruch, …)
/// und ohne Nicht-ASCII-Zeichen.
fn is_printable_ascii(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii() && !c.is_ascii_control())
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
    /// - [`ConfigError::PlaintextSecret`]: Header mit Credential-Namen ist
    ///   Klartext statt einer [`SecretRef`]; der Wert erscheint nie im
    ///   Fehler. Bei mehreren Verstößen wird der lexikographisch kleinste
    ///   Header-Name gemeldet (deterministisch trotz `HashMap`).
    /// - [`ConfigError::Invalid`]: `max_concurrency` ist auf `Some(0)`
    ///   gesetzt, was jeden Request an diesen Provider für immer blockieren
    ///   würde (fast sicher ein Tippfehler statt beabsichtigtes Verhalten).
    /// - [`ConfigError::Invalid`]: `originator` ist gesetzt, aber leer, länger
    ///   als [`MAX_ORIGINATOR_CHARS`] Zeichen oder enthält Nicht-ASCII-/
    ///   Steuerzeichen (siehe [`is_printable_ascii`]).
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
        if self.max_concurrency == Some(0) {
            return Err(ConfigError::Invalid(format!(
                "provider '{}': max_concurrency = 0 would block every request forever; omit the field for unlimited concurrency or set it to a positive value",
                self.name
            )));
        }
        if let Some(originator) = &self.originator {
            if originator.len() > MAX_ORIGINATOR_CHARS || !is_printable_ascii(originator) {
                return Err(ConfigError::Invalid(format!(
                    "provider '{}': originator must be non-empty, printable ASCII (no control characters) and at most {MAX_ORIGINATOR_CHARS} characters",
                    self.name
                )));
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

    #[test]
    fn test_provider_with_max_concurrency_round_trips() {
        let src = r#"
            name = "workers-ai"
            api = "openai-chat"
            base_url = "https://gateway.example/v1"
            max_concurrency = 3
        "#;
        let provider: ProviderToml = toml::from_str(src).unwrap();
        assert_eq!(provider.max_concurrency, Some(3));
        provider.validate().expect("max_concurrency = 3 is valid");
    }

    #[test]
    fn test_provider_without_max_concurrency_is_none() {
        let src = r#"
            name = "openai"
            api = "openai-chat"
            base_url = "https://api.openai.com/v1"
        "#;
        let provider: ProviderToml = toml::from_str(src).unwrap();
        assert!(provider.max_concurrency.is_none());
        provider.validate().expect("absent max_concurrency is valid (unbounded)");
    }

    #[test]
    fn test_provider_with_max_concurrency_one_round_trips() {
        let src = r#"
            name = "workers-ai"
            api = "openai-chat"
            base_url = "https://gateway.example/v1"
            max_concurrency = 1
        "#;
        let provider: ProviderToml = toml::from_str(src).unwrap();
        assert_eq!(provider.max_concurrency, Some(1));
        provider
            .validate()
            .expect("max_concurrency = 1 is the strictest valid value (fully serialized)");
    }

    #[test]
    fn test_provider_with_max_concurrency_and_rate_limit_both_set_no_interaction() {
        let src = r#"
            name = "workers-ai"
            api = "openai-chat"
            base_url = "https://gateway.example/v1"
            max_concurrency = 4

            [rate_limit]
            enabled = true
            safety_margin_pct = 20
        "#;
        let provider: ProviderToml = toml::from_str(src).unwrap();
        assert_eq!(provider.max_concurrency, Some(4));
        let rate_limit = provider
            .rate_limit
            .clone()
            .expect("rate_limit section present alongside max_concurrency");
        assert!(rate_limit.enabled);
        assert_eq!(rate_limit.safety_margin_pct, 20);
        provider
            .validate()
            .expect("max_concurrency and rate_limit are independent and both valid together");
    }

    #[test]
    fn test_provider_without_originator_is_none_and_omitted_on_serialize() {
        let src = r#"
            name = "openai"
            api = "openai-responses"
            base_url = "https://api.openai.com/v1"
        "#;
        let provider: ProviderToml = toml::from_str(src).unwrap();
        assert!(provider.originator.is_none());
        provider
            .validate()
            .expect("absent originator is valid (keeps the harw default)");
        assert!(!toml::to_string(&provider).unwrap().contains("originator"));
    }

    #[test]
    fn test_provider_with_originator_round_trips() {
        let src = r#"
            name = "openai"
            api = "openai-responses"
            base_url = "https://chatgpt.com/backend-api/codex"
            originator = "codex_cli_rs"
        "#;
        let provider: ProviderToml = toml::from_str(src).unwrap();
        assert_eq!(provider.originator.as_deref(), Some("codex_cli_rs"));
        provider.validate().expect("printable ASCII originator is valid");
    }

    #[test]
    fn test_validate_rejects_empty_originator() {
        let mut provider = provider_with_headers(&[]);
        provider.originator = Some(String::new());
        let error = provider.validate().unwrap_err();
        assert!(matches!(error, ConfigError::Invalid(ref msg) if msg.contains("originator")));
    }

    #[test]
    fn test_validate_rejects_originator_with_control_characters() {
        let mut provider = provider_with_headers(&[]);
        provider.originator = Some("bad\nvalue".to_owned());
        let error = provider.validate().unwrap_err();
        assert!(matches!(error, ConfigError::Invalid(ref msg) if msg.contains("originator")));
    }

    #[test]
    fn test_validate_rejects_originator_with_non_ascii() {
        let mut provider = provider_with_headers(&[]);
        provider.originator = Some("härw".to_owned());
        let error = provider.validate().unwrap_err();
        assert!(matches!(error, ConfigError::Invalid(ref msg) if msg.contains("originator")));
    }

    #[test]
    fn test_validate_rejects_originator_over_max_length() {
        let mut provider = provider_with_headers(&[]);
        provider.originator = Some("a".repeat(65));
        let error = provider.validate().unwrap_err();
        assert!(matches!(error, ConfigError::Invalid(ref msg) if msg.contains("originator")));
    }

    #[test]
    fn test_validate_accepts_originator_at_max_length() {
        let mut provider = provider_with_headers(&[]);
        provider.originator = Some("a".repeat(64));
        provider
            .validate()
            .expect("originator at exactly the length cap is valid");
    }

    #[test]
    fn test_validate_rejects_max_concurrency_zero() {
        let src = r#"
            name = "workers-ai"
            api = "openai-chat"
            base_url = "https://gateway.example/v1"
            max_concurrency = 0
        "#;
        let provider: ProviderToml = toml::from_str(src).unwrap();
        let error = provider.validate().unwrap_err();
        assert!(matches!(error, ConfigError::Invalid(ref msg) if msg.contains("max_concurrency")));
    }
}
