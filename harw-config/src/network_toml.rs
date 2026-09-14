//! `[network]` — grundlegende Netz-Policy für egress-fähige Werkzeuge/Rollen
//! (Remediationsplan Teil B, Welle W3, AP `C-CFG`; Konsumenten: `harw-egress`
//! `C-EGRESS`/`N-EGRESS`, `harw-tool-web` `N-WEB`, `harw-registry-defaults`
//! `RD` für `researcher-web`).
//!
//! Dieses Modul definiert ausschließlich die deklarative Policy: welche
//! Hosts Werkzeuge grundsätzlich ansprechen dürfen ([`NetworkSection::allow_hosts`]),
//! ob private/lokale Adressbereiche als Ziel erlaubt sind
//! ([`NetworkSection::allow_private`]), und welche zusätzlichen Hosts speziell
//! der `researcher-web`-Rolle offen stehen
//! ([`NetworkSection::researcher_web_hosts`] — Annahme A5 des Plans:
//! `researcher-web` verliert `fs.*`/`deps.source_*`, bleibt aber auf eine
//! eigene Web-Hostliste beschränkt). Durchsetzung (`harw_egress::EgressPolicy`,
//! Landlock/netns) liegt beim Consumer; dieses Modul beschreibt nur die
//! Konfiguration und deren Invarianten ([`NetworkSection::validate`]).
//!
//! Defaults sind bewusst restriktiv (`Default = sicher/aus`): leere
//! Allowlists, `allow_private = false`. Ein nicht vertrauter Repo-Layer darf
//! diese Sektion laut `docs/remediation/ledger/W3/C-CFG.md` nur verengen
//! (Schnittmenge der Hostlisten, `allow_private` nur Richtung `false`); die
//! Merge-Logik lebt in `harw_config::discovery`, nicht hier.
//!
//! Keine Nebenläufigkeit: reiner Datentyp, `Send + Sync` über `derive`.
//! Fehler: [`NetworkSection::validate`] liefert `Result<(), String>`
//! (gleiches Muster wie `research_toml::ResearchSection::validate`).
//!
//! # Examples
//! ```rust
//! use harw_config::NetworkSection;
//!
//! let section: NetworkSection = toml::from_str("").unwrap();
//! assert!(section.allow_hosts.is_empty());
//! assert!(!section.allow_private);
//! assert!(section.validate().is_ok());
//! ```

use serde::{Deserialize, Serialize};

/// `[network]` — Host-Allowlists und Grundsatzentscheidung zu privaten
/// Adressbereichen für egress-fähige Werkzeuge. Alle Felder haben
/// hart-codierte, restriktive Defaults, sodass eine `config.toml` ohne
/// `[network]` weiterhin gültig ist und keinerlei Netzzugriff freischaltet.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSection {
    /// Reine Hostnamen (kein Schema, kein Pfad), die egress-fähige Werkzeuge
    /// grundsätzlich ansprechen dürfen. Leer (Default) heißt: kein Host
    /// erlaubt.
    #[serde(default)]
    pub allow_hosts: Vec<String>,
    /// Erlaubt private/lokale Zieladressen (RFC1918, Loopback, Link-Local,
    /// ULA, CGNAT — Klassifikation bei `harw-egress::classify`). `false`
    /// (Default) ist die sichere Voreinstellung; SSRF-Ziele im eigenen Netz
    /// bleiben damit gesperrt, bis ausdrücklich freigegeben.
    #[serde(default)]
    pub allow_private: bool,
    /// Zusätzliche Hostnamen, ausschließlich für die `researcher-web`-Rolle
    /// (getrennt von `allow_hosts`, damit eine breitere Freigabe für andere
    /// Werkzeuge nicht implizit auch der Recherche-Rolle zufällt).
    #[serde(default)]
    pub researcher_web_hosts: Vec<String>,
}

impl NetworkSection {
    /// Prüft Invarianten, die reine Deserialisierung nicht ausdrücken kann.
    ///
    /// # Errors
    /// Liefert `Err(String)` mit einer menschenlesbaren Begründung, wenn
    /// `allow_hosts` oder `researcher_web_hosts` einen leeren Eintrag oder
    /// einen Eintrag mit Schema-Präfix (z. B. `https://example.test` statt
    /// `example.test`) enthalten — Hosts, keine URLs.
    pub fn validate(&self) -> Result<(), String> {
        validate_host_list("network.allow_hosts", &self.allow_hosts)?;
        validate_host_list("network.researcher_web_hosts", &self.researcher_web_hosts)?;
        Ok(())
    }
}

/// Gemeinsame Prüfung für Hostlisten dieser Sektion: kein leerer Eintrag,
/// kein Schema-Präfix.
fn validate_host_list(field: &str, hosts: &[String]) -> Result<(), String> {
    for host in hosts {
        let trimmed = host.trim();
        if trimmed.is_empty() {
            return Err(format!("{field} enthält einen leeren Eintrag"));
        }
        if trimmed.contains("://") {
            return Err(format!(
                "{field} erwartet reine Hostnamen ohne Schema-Präfix, gefunden {host:?}"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_network_section_defaults_from_empty_toml() {
        let section: NetworkSection = toml::from_str("").unwrap();
        assert!(section.allow_hosts.is_empty());
        assert!(!section.allow_private);
        assert!(section.researcher_web_hosts.is_empty());
        assert_eq!(section, NetworkSection::default());
    }

    #[test]
    fn test_network_section_full_toml_round_trip() {
        let src = r#"
            allow_hosts = ["docs.rs", "internal.example.test"]
            allow_private = true
            researcher_web_hosts = ["search.example.test"]
        "#;
        let section: NetworkSection = toml::from_str(src).unwrap();
        assert_eq!(
            section.allow_hosts,
            vec!["docs.rs".to_owned(), "internal.example.test".to_owned()]
        );
        assert!(section.allow_private);
        assert_eq!(
            section.researcher_web_hosts,
            vec!["search.example.test".to_owned()]
        );

        let encoded = toml::to_string(&section).unwrap();
        let decoded: NetworkSection = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded, section);
    }

    #[test]
    fn test_network_section_rejects_unknown_field() {
        let src = r#"
            allow_hosts = ["docs.rs"]
            allow_hots = ["docs.rs"]
        "#;
        let error = toml::from_str::<NetworkSection>(src).unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn test_validate_accepts_default_section() {
        assert!(NetworkSection::default().validate().is_ok());
    }

    #[test]
    fn test_validate_rejects_blank_allow_host_entry() {
        let section = NetworkSection {
            allow_hosts: vec!["   ".to_owned()],
            ..NetworkSection::default()
        };
        let error = section.validate().unwrap_err();
        assert!(error.contains("network.allow_hosts"));
    }

    #[test]
    fn test_validate_rejects_scheme_prefixed_researcher_web_host() {
        let section = NetworkSection {
            researcher_web_hosts: vec!["https://search.example.test".to_owned()],
            ..NetworkSection::default()
        };
        let error = section.validate().unwrap_err();
        assert!(error.contains("network.researcher_web_hosts"));
        assert!(error.contains("https://search.example.test"));
    }
}
