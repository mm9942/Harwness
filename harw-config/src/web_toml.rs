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
}

impl Default for WebSection {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            port: 0,
            token_ttl_secs: default_token_ttl_secs(),
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

    #[test]
    fn test_web_section_defaults_from_empty_toml() {
        let section: WebSection = toml::from_str("").unwrap();
        assert_eq!(section.bind, "127.0.0.1");
        assert_eq!(section.port, 0);
        assert_eq!(section.token_ttl_secs, 900);
        assert_eq!(section, WebSection::default());
    }

    #[test]
    fn test_web_section_full_toml_round_trip() {
        let src = r#"
            bind = "::1"
            port = 8899
            token_ttl_secs = 120
        "#;
        let section: WebSection = toml::from_str(src).unwrap();
        assert_eq!(section.bind, "::1");
        assert_eq!(section.port, 8899);
        assert_eq!(section.token_ttl_secs, 120);

        let encoded = toml::to_string(&section).unwrap();
        let decoded: WebSection = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded, section);
    }

    #[test]
    fn test_web_section_rejects_unknown_field() {
        let src = r#"
            bind = "127.0.0.1"
            bnid = "127.0.0.1"
        "#;
        let error = toml::from_str::<WebSection>(src).unwrap_err();
        assert!(error.to_string().contains("unknown field"));
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
    fn test_validate_rejects_non_loopback_bind() {
        let section = WebSection {
            bind: "0.0.0.0".to_owned(),
            ..WebSection::default()
        };
        let error = section.validate().unwrap_err();
        assert!(error.contains("0.0.0.0"));
    }

    #[test]
    fn test_validate_rejects_zero_token_ttl() {
        let section = WebSection {
            token_ttl_secs: 0,
            ..WebSection::default()
        };
        let error = section.validate().unwrap_err();
        assert!(error.contains("token_ttl_secs"));
    }
}
