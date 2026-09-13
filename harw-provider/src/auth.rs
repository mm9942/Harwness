//! Auth-Konfiguration und der winzige, saubere Bearer-Helper.
//!
//! Note 08 §1.1/§4.3: die Bearer-Mechanik ist klein — `Bearer {token}` in den
//! Authorization-Header. Der Schmerz kam beim letzten Mal aus dem
//! OAuth/Login-Drumherum, das hier bewusst rausoperiert ist.
//!
//! Tokens tragen `secrecy::SecretString` (Note 11: SecretString-Pflicht), sodass
//! ein versehentliches `Debug` auf die Registry keine Keys leakt.

use std::sync::Arc;

use secrecy::{ExposeSecret, SecretString};

use crate::error::{ProviderError, ProviderResult};

/// Minimaler, dependency-freier Header-Container.
///
/// Note 08 §4.2 skizziert `http::HeaderMap`; um `harw-provider` frei von
/// `http`/`reqwest` zu halten (höchste Stabilität, kein Runtime-SDK in der
/// Identitäts-Schicht), benutzen wir hier eine schlanke eigene Form. Der echte
/// HTTP-Client (eigenes Crate) übersetzt das in `http::HeaderMap`.
#[derive(Clone, Debug, Default)]
pub struct HeaderMap {
    entries: Vec<(String, String)>,
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
pub enum SghAuth {
    ApiKey(ApiKeyConfig),
    StaticBearer(StaticBearerConfig),
    #[cfg(feature = "chatgpt-oauth")]
    ChatGptOAuth(ChatGptOAuthConfig),
    None,
}

impl SghAuth {
    /// Hängt die passenden Auth-Header an. Das Token wird lokal in den
    /// `Bearer …`-String gegossen und landet nie als `String`-Feld irgendwo,
    /// das später auseinandergeloggt wird (Note 08 §4.5).
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
