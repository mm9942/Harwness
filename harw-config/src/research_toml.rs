//! `[research]` — Netzwerk- und Ressourcen-Policy für Recherche-Tools
//! (AP W1-21..24).
//!
//! Recherche-Tools (Doku-Lookup, Crate-Metadaten, Web-Fetch für externe
//! APIs) dürfen nur gegen eine explizite Allowlist von Hosts auflösen und
//! nur bis zu harten Limits für Antwortgröße und Wartezeit. Dieses Modul
//! definiert ausschließlich die deklarative Policy; die Durchsetzung
//! (tatsächliche Netzwerksperre, Timeout-Implementierung) liegt beim
//! Consumer.

use serde::{Deserialize, Serialize};

/// `[research]` — erlaubte Netzwerk-Hosts sowie Größen- und Zeit-Limits für
/// Recherche-Tools. Alle Felder haben hart-codierte Defaults, sodass eine
/// `config.toml` ohne `[research]` weiterhin gültig ist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResearchSection {
    /// Reine Hostnamen (kein Schema, kein Pfad), gegen die Recherche-Tools
    /// auflösen dürfen. Default deckt die Rust-Standard-Dokuquellen ab.
    #[serde(default = "default_allow_hosts")]
    pub network_allow_hosts: Vec<String>,
    /// Erlaubt Lesezugriff auf den lokalen Cargo-Registry-Cache
    /// (`~/.cargo/registry`) für Crate-Metadaten-Lookups ohne Netzwerk.
    #[serde(default = "default_true")]
    pub cargo_registry_read: bool,
    /// Obergrenze der Antwortgröße eines einzelnen Fetches in Bytes.
    #[serde(default = "default_fetch_bytes")]
    pub max_fetch_bytes: usize,
    /// Timeout eines einzelnen Fetches in Sekunden.
    #[serde(default = "default_fetch_timeout")]
    pub fetch_timeout_secs: u64,
    /// Wie lange ein gecachtes Fetch-Ergebnis wiederverwendet werden darf,
    /// bevor es erneut abgerufen wird.
    #[serde(default = "default_cache_ttl")]
    pub cache_ttl_secs: u64,
}

impl Default for ResearchSection {
    fn default() -> Self {
        Self {
            network_allow_hosts: default_allow_hosts(),
            cargo_registry_read: default_true(),
            max_fetch_bytes: default_fetch_bytes(),
            fetch_timeout_secs: default_fetch_timeout(),
            cache_ttl_secs: default_cache_ttl(),
        }
    }
}

impl ResearchSection {
    /// Prüft Invarianten, die reine Deserialisierung nicht ausdrücken kann.
    ///
    /// # Errors
    /// Liefert `Err(String)` mit einer menschenlesbaren Begründung, wenn
    /// `network_allow_hosts` leer ist oder einen leeren bzw. Schema-behafteten
    /// Eintrag enthält (z. B. `http://example.test` statt `example.test`),
    /// oder wenn `max_fetch_bytes` bzw. `fetch_timeout_secs` `0` ist.
    pub fn validate(&self) -> Result<(), String> {
        if self.network_allow_hosts.is_empty() {
            return Err("research.network_allow_hosts darf nicht leer sein".to_owned());
        }
        for host in &self.network_allow_hosts {
            let trimmed = host.trim();
            if trimmed.is_empty() {
                return Err("research.network_allow_hosts enthält einen leeren Eintrag".to_owned());
            }
            if trimmed.contains("://") {
                return Err(format!(
                    "research.network_allow_hosts erwartet reine Hostnamen ohne Schema-Präfix, gefunden {host:?}"
                ));
            }
        }
        if self.max_fetch_bytes == 0 {
            return Err("research.max_fetch_bytes muss größer als 0 sein".to_owned());
        }
        if self.fetch_timeout_secs == 0 {
            return Err("research.fetch_timeout_secs muss größer als 0 sein".to_owned());
        }
        Ok(())
    }
}

fn default_allow_hosts() -> Vec<String> {
    vec![
        "docs.rs".to_owned(),
        "crates.io".to_owned(),
        "doc.rust-lang.org".to_owned(),
        "static.crates.io".to_owned(),
    ]
}
fn default_true() -> bool {
    true
}
fn default_fetch_bytes() -> usize {
    1_048_576
}
fn default_fetch_timeout() -> u64 {
    20
}
fn default_cache_ttl() -> u64 {
    3_600
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_research_section_defaults_from_empty_toml() -> TestResult {
        let section: ResearchSection = toml::from_str("").map_err(ctx("leeres TOML parsen"))?;
        assert_eq!(
            section.network_allow_hosts,
            vec![
                "docs.rs".to_owned(),
                "crates.io".to_owned(),
                "doc.rust-lang.org".to_owned(),
                "static.crates.io".to_owned(),
            ]
        );
        assert!(section.cargo_registry_read);
        assert_eq!(section.max_fetch_bytes, 1_048_576);
        assert_eq!(section.fetch_timeout_secs, 20);
        assert_eq!(section.cache_ttl_secs, 3_600);
        assert_eq!(section, ResearchSection::default());
        Ok(())
    }

    #[test]
    fn test_research_section_full_toml_round_trip() -> TestResult {
        let src = r#"
            network_allow_hosts = ["docs.rs", "internal.example.test"]
            cargo_registry_read = false
            max_fetch_bytes = 2048
            fetch_timeout_secs = 5
            cache_ttl_secs = 60
        "#;
        let section: ResearchSection = toml::from_str(src).map_err(ctx("TOML parsen"))?;
        assert_eq!(
            section.network_allow_hosts,
            vec!["docs.rs".to_owned(), "internal.example.test".to_owned()]
        );
        assert!(!section.cargo_registry_read);
        assert_eq!(section.max_fetch_bytes, 2048);
        assert_eq!(section.fetch_timeout_secs, 5);
        assert_eq!(section.cache_ttl_secs, 60);

        let encoded = toml::to_string(&section).map_err(ctx("TOML serialisieren"))?;
        let decoded: ResearchSection =
            toml::from_str(&encoded).map_err(ctx("serialisiertes TOML parsen"))?;
        assert_eq!(decoded, section);
        Ok(())
    }

    #[test]
    fn test_research_section_rejects_unknown_field() -> TestResult {
        let src = r#"
            max_fetch_bytes = 2048
            max_ftech_bytes = 2048
        "#;
        let outcome = toml::from_str::<ResearchSection>(src);
        let Err(error) = outcome else {
            return Err(TestError::Unexpected(
                "unbekanntes Feld muss abgelehnt werden".to_owned(),
            ));
        };
        assert!(error.to_string().contains("unknown field"));
        Ok(())
    }

    #[test]
    fn test_validate_rejects_empty_allow_hosts() -> TestResult {
        let section = ResearchSection {
            network_allow_hosts: Vec::new(),
            ..ResearchSection::default()
        };
        let outcome = section.validate();
        let Err(error) = outcome else {
            return Err(TestError::Unexpected(
                "leere network_allow_hosts müssen abgelehnt werden".to_owned(),
            ));
        };
        assert!(error.contains("network_allow_hosts"));
        Ok(())
    }

    #[test]
    fn test_validate_rejects_blank_host_entry() {
        let section = ResearchSection {
            network_allow_hosts: vec!["docs.rs".to_owned(), "   ".to_owned()],
            ..ResearchSection::default()
        };
        assert!(section.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_host_with_scheme_prefix() -> TestResult {
        let section = ResearchSection {
            network_allow_hosts: vec!["http://docs.rs".to_owned()],
            ..ResearchSection::default()
        };
        let outcome = section.validate();
        let Err(error) = outcome else {
            return Err(TestError::Unexpected(
                "Host mit Schema-Präfix muss abgelehnt werden".to_owned(),
            ));
        };
        assert!(error.contains("http://docs.rs"));
        Ok(())
    }

    #[test]
    fn test_validate_rejects_zero_max_fetch_bytes() -> TestResult {
        let section = ResearchSection {
            max_fetch_bytes: 0,
            ..ResearchSection::default()
        };
        let outcome = section.validate();
        let Err(error) = outcome else {
            return Err(TestError::Unexpected(
                "max_fetch_bytes = 0 muss abgelehnt werden".to_owned(),
            ));
        };
        assert!(error.contains("max_fetch_bytes"));
        Ok(())
    }

    #[test]
    fn test_validate_rejects_zero_fetch_timeout() -> TestResult {
        let section = ResearchSection {
            fetch_timeout_secs: 0,
            ..ResearchSection::default()
        };
        let outcome = section.validate();
        let Err(error) = outcome else {
            return Err(TestError::Unexpected(
                "fetch_timeout_secs = 0 muss abgelehnt werden".to_owned(),
            ));
        };
        assert!(error.contains("fetch_timeout_secs"));
        Ok(())
    }

    #[test]
    fn test_validate_accepts_default_section() {
        assert!(ResearchSection::default().validate().is_ok());
    }
}
