//! `[web]` — Bind- und Token-Policy für die eingebettete Web-UI
//! (Remediationsplan Teil B, Welle W3, AP `C-CFG`; Konsument: `harw-web`
//! `WB-SRV`/`WB-COMP`).
//!
//! Dieses Modul definiert ausschließlich die deklarative Policy für den
//! lokalen Web-Server: die Bind-Adresse ([`WebSection::bind`] — laut Design
//! "harw-web serviert Next-Export eingebettet auf 127.0.0.1:ephemer" **nur
//! Loopback**, siehe [`WebSection::validate`]), den Port
//! ([`WebSection::port`] — `0` = vom Betriebssystem zugewiesener freier
//! Port), und die Gültigkeitsdauer des URL-Fragment-Launch-Tokens
//! ([`WebSection::token_ttl_secs`]). Durchsetzung (TCP-Bind, Bearer-Prüfung,
//! Host/Origin/Sec-Fetch-Site-Header-Prüfung) liegt vollständig beim
//! Consumer; dieses Modul beschreibt nur die Konfiguration und deren
//! Invarianten.
//!
//! Defaults sind bewusst restriktiv (`Default = sicher/aus`): Bind ist immer
//! Loopback, Port `0` (ephemer statt eines vorhersagbaren Standardports).
//! Laut `docs/remediation/ledger/W3/C-CFG.md` gilt **"Web-Bind nicht vom
//! Repo"**: ein nicht vertrauter Repo-Layer darf `[web]` überhaupt nicht
//! beeinflussen — weder verengend noch erweiternd. Die Merge-Logik (die
//! diese Sektion aus einem Repo-Layer schlicht ignoriert) lebt in
//! `harw_config::discovery`, nicht hier.
//!
//! Keine Nebenläufigkeit: reiner Datentyp, `Send + Sync` über `derive`.
//! Fehler: [`WebSection::validate`] liefert `Result<(), String>` (gleiches
//! Muster wie `research_toml::ResearchSection::validate`).
//!
//! # Examples
//! ```rust
//! use harw_config::WebSection;
//!
//! let section: WebSection = toml::from_str("").unwrap();
//! assert_eq!(section.bind, "127.0.0.1");
//! assert!(section.validate().is_ok());
//! ```

use serde::{Deserialize, Serialize};

/// `[web]` — Bind-Adresse, Port und Token-Lebensdauer der eingebetteten
/// Web-UI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebSection {
    /// Bind-Adresse ohne Port. Muss laut [`Self::validate`] exakt `127.0.0.1`
    /// oder `::1` sein — nie eine öffentliche oder Wildcard-Adresse.
    #[serde(default = "default_bind")]
    pub bind: String,
    /// TCP-Port. `0` (Default) heißt: das Betriebssystem wählt einen freien
    /// Port (ephemer), damit der Port nicht vorhersagbar ist.
    #[serde(default)]
    pub port: u16,
    /// Gültigkeitsdauer des URL-Fragment-Launch-Tokens in Sekunden, bevor es
    /// als `Authorization: Bearer`-Token nicht mehr akzeptiert wird.
    #[serde(default = "default_token_ttl_secs")]
    pub token_ttl_secs: u64,
    /// `[web.search]` — Backend des Agent-Werkzeugs `web.search`.
    #[serde(default)]
    pub search: WebSearchToml,
}

/// `[web.search]` — Such-Backend für `web.search`.
///
/// `provider` ist `duckduckgo` (Default, ohne Schlüssel), `brave`, `tavily`
/// oder `searxng`. Der API-Schlüssel wird nie in der Datei abgelegt, sondern
/// aus der Umgebungsvariable `api_key_env` gelesen; `endpoint` ist nur für
/// SearXNG nötig (Basis-URL der Instanz).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebSearchToml {
    #[serde(default = "default_search_provider")]
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(default = "default_search_max_results")]
    pub max_results: u8,
}

impl Default for WebSearchToml {
    fn default() -> Self {
        Self {
            provider: default_search_provider(),
            endpoint: None,
            api_key_env: None,
            max_results: default_search_max_results(),
        }
    }
}

impl WebSearchToml {
    /// Prüft Provider-Namen, Endpoint-Pflicht für SearXNG und die
    /// Ergebnisobergrenze (1–20).
    ///
    /// # Errors
    /// `Err(String)` mit Begründung.
    pub fn validate(&self) -> Result<(), String> {
        match self.provider.as_str() {
            "duckduckgo" | "brave" | "tavily" => {}
            "searxng" if self.endpoint.is_some() => {}
            "searxng" => return Err("web.search.endpoint ist für searxng Pflicht".to_owned()),
            other => {
                return Err(format!(
                    "web.search.provider muss duckduckgo|brave|tavily|searxng sein, gefunden {other:?}"
                ));
            }
        }
        if !(1..=20).contains(&self.max_results) {
            return Err("web.search.max_results muss zwischen 1 und 20 liegen".to_owned());
        }
        Ok(())
    }
}

fn default_search_provider() -> String {
    "duckduckgo".to_owned()
}

fn default_search_max_results() -> u8 {
    8
}

impl Default for WebSection {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            port: 0,
            token_ttl_secs: default_token_ttl_secs(),
            search: WebSearchToml::default(),
        }
    }
}

impl WebSection {
    /// Prüft Invarianten, die reine Deserialisierung nicht ausdrücken kann.
    ///
    /// # Errors
    /// Liefert `Err(String)` mit einer menschenlesbaren Begründung, wenn
    /// `bind` weder `127.0.0.1` noch `::1` ist, oder wenn `token_ttl_secs`
    /// `0` ist.
    pub fn validate(&self) -> Result<(), String> {
        if self.bind != "127.0.0.1" && self.bind != "::1" {
            return Err(format!(
                "web.bind muss Loopback sein (127.0.0.1 oder ::1), gefunden {:?}",
                self.bind
            ));
        }
        if self.token_ttl_secs == 0 {
            return Err("web.token_ttl_secs muss größer als 0 sein".to_owned());
        }
        self.search.validate()?;
        Ok(())
    }
}

fn default_bind() -> String {
    "127.0.0.1".to_owned()
}
fn default_token_ttl_secs() -> u64 {
    900
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_web_section_defaults_from_empty_toml() -> TestResult {
        let section: WebSection = toml::from_str("").map_err(ctx("parse toml"))?;
        assert_eq!(section.bind, "127.0.0.1");
        assert_eq!(section.port, 0);
        assert_eq!(section.token_ttl_secs, 900);
        assert_eq!(section, WebSection::default());
        Ok(())
    }

    #[test]
    fn test_web_section_full_toml_round_trip() -> TestResult {
        let src = r#"
            bind = "::1"
            port = 8899
            token_ttl_secs = 120
        "#;
        let section: WebSection = toml::from_str(src).map_err(ctx("parse toml"))?;
        assert_eq!(section.bind, "::1");
        assert_eq!(section.port, 8899);
        assert_eq!(section.token_ttl_secs, 120);

        let encoded = toml::to_string(&section).map_err(ctx("encode toml"))?;
        let decoded: WebSection = toml::from_str(&encoded).map_err(ctx("parse encoded toml"))?;
        assert_eq!(decoded, section);
        Ok(())
    }

    #[test]
    fn test_web_section_rejects_unknown_field() -> TestResult {
        let src = r#"
            bind = "127.0.0.1"
            bnid = "127.0.0.1"
        "#;
        let Err(error) = toml::from_str::<WebSection>(src) else {
            return Err(TestError::Unexpected(
                "unknown field must be rejected".to_string(),
            ));
        };
        assert!(error.to_string().contains("unknown field"));
        Ok(())
    }

    #[test]
    fn test_validate_accepts_default_section() {
        assert!(WebSection::default().validate().is_ok());
    }

    #[test]
    fn test_validate_accepts_ipv6_loopback() {
        let section = WebSection {
            bind: "::1".to_owned(),
            ..WebSection::default()
        };
        assert!(section.validate().is_ok());
    }

    #[test]
    fn test_validate_rejects_non_loopback_bind() -> TestResult {
        let section = WebSection {
            bind: "0.0.0.0".to_owned(),
            ..WebSection::default()
        };
        let Err(error) = section.validate() else {
            return Err(TestError::Unexpected(
                "non-loopback bind must be rejected".to_string(),
            ));
        };
        assert!(error.contains("0.0.0.0"));
        Ok(())
    }

    #[test]
    fn test_validate_rejects_zero_token_ttl() -> TestResult {
        let section = WebSection {
            token_ttl_secs: 0,
            ..WebSection::default()
        };
        let Err(error) = section.validate() else {
            return Err(TestError::Unexpected(
                "zero token_ttl_secs must be rejected".to_string(),
            ));
        };
        assert!(error.contains("token_ttl_secs"));
        Ok(())
    }
}
