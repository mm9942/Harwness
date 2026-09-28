//! Auth-Konfiguration und der winzige, saubere Bearer-Helper.
//!
//! Note 08 §1.1/§4.3: die Bearer-Mechanik ist klein — `Bearer {token}` in den
//! Authorization-Header. Der Schmerz kam beim letzten Mal aus dem
//! OAuth/Login-Drumherum, das hier bewusst rausoperiert ist.
//!
//! Tokens tragen `secrecy::SecretString` (Note 11: SecretString-Pflicht), sodass
//! ein versehentliches `Debug` auf die Registry keine Keys leakt. Sobald ein
//! Token in [`HeaderMap`] als fertiger `Bearer …`-Header landet, redigiert
//! `HeaderMap`s handgeschriebenes `Debug` die Werte sicherheitsrelevanter
//! Header (siehe [`SENSITIVE_HEADER_NAMES`]) ebenfalls.

use std::sync::Arc;

use secrecy::{ExposeSecret, SecretString};

use crate::error::{ProviderError, ProviderResult};

/// Header-Namen, deren Werte in `Debug`-Ausgaben nie im Klartext auftauchen
/// dürfen (Note 08 §4.5, Note 11 SecretString-Pflicht).
const SENSITIVE_HEADER_NAMES: [&str; 4] = [
    "authorization",
    "x-api-key",
    "chatgpt-account-id",
    "cookie",
];

/// Minimaler, dependency-freier Header-Container.
///
/// Note 08 §4.2 skizziert `http::HeaderMap`; um `harw-provider` frei von
/// `http`/`reqwest` zu halten (höchste Stabilität, kein Runtime-SDK in der
/// Identitäts-Schicht), benutzen wir hier eine schlanke eigene Form. Der echte
/// HTTP-Client (eigenes Crate) übersetzt das in `http::HeaderMap`.
///
/// `Debug` ist bewusst handgeschrieben statt abgeleitet: die Werte
/// sicherheitsrelevanter Header (siehe [`SENSITIVE_HEADER_NAMES`]) werden
/// dabei redigiert, damit ein `?headers`-Log oder ein Debug-Print keinen
/// Bearer-Token oder API-Key im Klartext ausgibt.
#[derive(Clone, Default)]
pub struct HeaderMap {
    entries: Vec<(String, String)>,
}

impl std::fmt::Debug for HeaderMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut map = f.debug_map();
        for (name, value) in &self.entries {
            if SENSITIVE_HEADER_NAMES.contains(&name.as_str()) {
                map.entry(name, &"[REDACTED]");
            } else {
                map.entry(name, value);
            }
        }
        map.finish()
    }
}

impl HeaderMap {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fügt einen Header ein. Header-Namen werden case-insensitiv (lowercase)
    /// gehalten — wie es HTTP/2 verlangt.
    pub fn insert(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.entries
            .push((name.into().to_ascii_lowercase(), value.into()));
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        let needle = name.to_ascii_lowercase();
        self.entries
            .iter()
            .find(|(k, _)| *k == needle)
            .map(|(_, v)| v.as_str())
    }

    pub fn iter(&self) -> std::slice::Iter<'_, (String, String)> {
        self.entries.iter()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Auth-Tiers (Note 08 / Note 12 §4). Eine Variante, ein Pfad — kein
/// Multi-Variant-Login-Enum à la `CodexAuth`.
#[derive(Clone, Debug)]
pub enum ProviderAuth {
    ApiKey(ApiKeyConfig),
    StaticBearer(StaticBearerConfig),
    #[cfg(feature = "chatgpt-oauth")]
    ChatGptOAuth(ChatGptOAuthConfig),
    None,
}

impl ProviderAuth {
    /// Hängt die passenden Auth-Header an. Das Token wird lokal in den
    /// `Bearer …`-String gegossen und landet zwar als `String`-Wert in
    /// [`HeaderMap`], deren handgeschriebenes `Debug` diesen Wert aber
    /// redigiert, sodass er nicht auseinandergeloggt wird (Note 08 §4.5).
    pub fn add_auth_headers(&self, headers: &mut HeaderMap) {
        match self {
            Self::ApiKey(cfg) => {
                BearerAuth::from_secret(cfg.api_key.clone()).add_auth_headers(headers);
            }
            Self::StaticBearer(cfg) => {
                BearerAuth::from_secret(cfg.access_token.clone()).add_auth_headers(headers);
                if let Some(account) = &cfg.account_id {
                    headers.insert("chatgpt-account-id", account.to_string());
                }
            }
            #[cfg(feature = "chatgpt-oauth")]
            Self::ChatGptOAuth(_) => {
                // Token-Auflösung passiert im OAuth-Crate; hier kein Header.
            }
            Self::None => {}
        }
    }
}

/// API-Key-Konfiguration.
#[derive(Clone, Debug)]
pub struct ApiKeyConfig {
    pub api_key: SecretString,
}

/// Statisches Bearer-Token (Note 12 §4). `account_id` bleibt optional.
#[derive(Clone, Debug)]
pub struct StaticBearerConfig {
    pub access_token: SecretString,
    pub account_id: Option<Arc<str>>,
}

/// ChatGPT-OAuth-Konfiguration (nur unter Feature `chatgpt-oauth`).
#[cfg(feature = "chatgpt-oauth")]
#[derive(Clone, Debug)]
pub struct ChatGptOAuthConfig {
    pub auth_store_path: std::path::PathBuf,
    pub account_id: Option<Arc<str>>,
}

/// Trait-Form des Auth-Headers (Note 08 §4.2).
pub trait AuthProvider: Send + Sync {
    fn add_auth_headers(&self, headers: &mut HeaderMap);
}

/// Der winzige, saubere Bearer-Helper (Note 08 §4.3).
pub struct BearerAuth {
    pub token: SecretString,
}

impl BearerAuth {
    /// Liest das Token aus einer ENV-Variable. Leer == nicht-da (Note 08 §1.3).
    pub fn from_env(var: &str) -> ProviderResult<Self> {
        std::env::var(var)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .map(|t| Self {
                token: SecretString::new(t),
            })
            .ok_or_else(|| ProviderError::MissingEnvVar(var.to_owned()))
    }

    /// Setzt das Token direkt (z.B. aus Config).
    #[must_use]
    pub fn from_static(token: impl Into<String>) -> Self {
        Self {
            token: SecretString::new(token.into()),
        }
    }

    /// Übernimmt ein bereits vorhandenes Secret ohne Re-Allokation.
    #[must_use]
    pub fn from_secret(token: SecretString) -> Self {
        Self { token }
    }
}

impl AuthProvider for BearerAuth {
    fn add_auth_headers(&self, headers: &mut HeaderMap) {
        headers.insert(
            "authorization",
            format!("Bearer {}", self.token.expose_secret()),
        );
    }
}

#[cfg(test)]
mod tests {
    use secrecy::SecretString;

    use super::{
        ApiKeyConfig, AuthProvider, BearerAuth, HeaderMap, ProviderAuth, StaticBearerConfig,
    };
    use crate::test_support::TestResult;

    #[test]
    fn debug_redacts_bearer_token_from_authorization_header() -> TestResult {
        let mut headers = HeaderMap::new();
        BearerAuth::from_static("tok-secret-123").add_auth_headers(&mut headers);
        let rendered = format!("{headers:?}");
        assert!(!rendered.contains("tok-secret-123"));
        assert!(rendered.contains("REDACTED"));
        Ok(())
    }

    #[test]
    fn debug_redacts_api_key_via_provider_auth() -> TestResult {
        let auth = ProviderAuth::ApiKey(ApiKeyConfig {
            api_key: SecretString::new("sk-test-123".to_owned()),
        });
        let mut headers = HeaderMap::new();
        auth.add_auth_headers(&mut headers);
        let rendered = format!("{headers:?}");
        assert!(!rendered.contains("sk-test-123"));
        Ok(())
    }

    #[test]
    fn debug_redacts_bearer_token_and_account_id_from_static_bearer() -> TestResult {
        let auth = ProviderAuth::StaticBearer(StaticBearerConfig {
            access_token: SecretString::new("tok-abc".to_owned()),
            account_id: Some(std::sync::Arc::from("acct-42")),
        });
        let mut headers = HeaderMap::new();
        auth.add_auth_headers(&mut headers);
        let rendered = format!("{headers:?}");
        assert!(!rendered.contains("tok-abc"));
        assert!(!rendered.contains("acct-42"));
        Ok(())
    }

    #[test]
    fn debug_keeps_non_sensitive_header_values_readable() -> TestResult {
        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", "req-99");
        let rendered = format!("{headers:?}");
        assert!(rendered.contains("req-99"));
        Ok(())
    }

    #[test]
    fn get_still_returns_full_value_despite_redacted_debug() -> TestResult {
        let mut headers = HeaderMap::new();
        BearerAuth::from_static("tok-full-value").add_auth_headers(&mut headers);
        assert_eq!(headers.get("authorization"), Some("Bearer tok-full-value"));
        Ok(())
    }
}
