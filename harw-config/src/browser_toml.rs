//! `[browser]` — Aktivierungs- und Limits-Policy für das Browser-Werkzeug
//! (Remediationsplan Teil B, Welle W3, AP `C-CFG`; Konsumenten:
//! `harw-browser`/`harw-tool-browser` `C-BROWSER`, `harw-browser-thirtyfour`
//! `B-ADAPT`, `harw-tool-browser` `B-TOOL`).
//!
//! Dieses Modul definiert ausschließlich die deklarative Policy: ob das
//! Browser-Werkzeug überhaupt registriert wird ([`BrowserSection::enabled`]),
//! auf welche Origins es beschränkt ist ([`BrowserSection::allowed_origins`]),
//! wie viele Aktionen eine Session maximal ausführen darf
//! ([`BrowserSection::max_actions`]), und woher `geckodriver` geladen werden
//! darf ([`BrowserSection::geckodriver_path`],
//! [`BrowserSection::geckodriver_sha256`] — Design "B-ADAPT: kein
//! `WebDriver::managed`, geckodriver gepinnt + SHA-256"). Durchsetzung
//! (Same-Origin-Prüfung nach jeder Aktion, Prozessstart in `bwrap
//! --unshare-net`) liegt beim Consumer; hier wird nur beschrieben und
//! geprüft ([`BrowserSection::validate`]).
//!
//! Defaults sind bewusst restriktiv (`Default = sicher/aus`): `enabled =
//! false`, leere Origin-Allowlist. Ein nicht vertrauter Repo-Layer darf diese
//! Sektion laut `docs/remediation/ledger/W3/C-CFG.md` nur verengen
//! (`enabled` nur `true` → `false`, `allowed_origins`/`max_actions` nur
//! kleiner); `geckodriver_path`/`geckodriver_sha256` werden aus einem Repo-
//! Layer **nie** übernommen (Umlenkung auf ein fremdes Binary wäre eine
//! Rechteausweitung, kein Verengen). Die Merge-Logik lebt in
//! `harw_config::discovery`, nicht hier.
//!
//! Keine Nebenläufigkeit: reiner Datentyp, `Send + Sync` über `derive`.
//! Fehler: [`BrowserSection::validate`] liefert `Result<(), String>`
//! (gleiches Muster wie `research_toml::ResearchSection::validate`).
//!
//! # Examples
//! ```rust
//! use harw_config::BrowserSection;
//!
//! let section: BrowserSection = toml::from_str("").unwrap();
//! assert!(!section.enabled);
//! assert!(section.validate().is_ok());
//! ```

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// `[browser]` — Freischaltung, Origin-Allowlist, Aktionslimit und
/// `geckodriver`-Pinning für das Browser-Werkzeug.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserSection {
    /// Schaltet das Browser-Werkzeug frei. `false` (Default) muss die
    /// Registrierung beim Consumer vollständig entfernen (gleiche Semantik
    /// wie `tools.plan.enabled`).
    #[serde(default)]
    pub enabled: bool,
    /// Same-Origin-Allowlist (vollständige Origins inkl. Schema, z. B.
    /// `https://intranet.example.test`, keine reinen Hostnamen — im
    /// Unterschied zu `[network].allow_hosts`). Leer (Default) heißt: keine
    /// Origin erlaubt.
    #[serde(default)]
    pub allowed_origins: Vec<String>,
    /// Obergrenze der Browser-Aktionen (Klicks, Eingaben, Navigationen) je
    /// Session. Klein vorbelegt, um eine außer Kontrolle geratene Session zu
    /// begrenzen, bevor `harw-tool-browser` (W5 `B-TOOL`) eigene
    /// Journal-/Zeitlimits durchsetzt.
    #[serde(default = "default_max_actions")]
    pub max_actions: u32,
    /// Absoluter Pfad zum gepinnten `geckodriver`-Binary. Muss zusammen mit
    /// [`Self::geckodriver_sha256`] gesetzt werden (beide oder keins) — ein
    /// Pfad ohne Hash-Pinning wäre ein Supply-Chain-Loch.
    #[serde(default)]
    pub geckodriver_path: Option<PathBuf>,
    /// SHA-256-Digest (64 Hex-Zeichen, klein geschrieben) des unter
    /// [`Self::geckodriver_path`] erwarteten Binarys.
    #[serde(default)]
    pub geckodriver_sha256: Option<String>,
}

impl Default for BrowserSection {
    fn default() -> Self {
        Self {
            enabled: false,
            allowed_origins: Vec::new(),
            max_actions: default_max_actions(),
            geckodriver_path: None,
            geckodriver_sha256: None,
        }
    }
}

impl BrowserSection {
    /// Prüft Invarianten, die reine Deserialisierung nicht ausdrücken kann.
    ///
    /// # Errors
    /// Liefert `Err(String)` mit einer menschenlesbaren Begründung, wenn:
    /// - `enabled = true`, aber `allowed_origins` leer ist (ein freigeschaltetes
    ///   Werkzeug ohne jede erlaubte Origin kann nie etwas tun),
    /// - `allowed_origins` einen leeren Eintrag oder einen Eintrag ohne
    ///   Schema-Präfix (`scheme://…`) enthält — Origins, keine reinen Hosts,
    /// - `max_actions == 0`,
    /// - genau eines von `geckodriver_path`/`geckodriver_sha256` gesetzt ist
    ///   (Pinning verlangt beide zusammen),
    /// - `geckodriver_sha256` gesetzt, aber keine 64-stellige, klein
    ///   geschriebene Hex-Zeichenkette ist.
    pub fn validate(&self) -> Result<(), String> {
        if self.enabled && self.allowed_origins.is_empty() {
            return Err(
                "browser.enabled=true erfordert mindestens eine Origin in browser.allowed_origins"
                    .to_owned(),
            );
        }
        for origin in &self.allowed_origins {
            let trimmed = origin.trim();
            if trimmed.is_empty() {
                return Err("browser.allowed_origins enthält einen leeren Eintrag".to_owned());
            }
            if !trimmed.contains("://") {
                return Err(format!(
                    "browser.allowed_origins erwartet vollständige Origins mit Schema \
                     (z. B. https://host), gefunden {origin:?}"
                ));
            }
        }
        if self.max_actions == 0 {
            return Err("browser.max_actions muss größer als 0 sein".to_owned());
        }
        match (&self.geckodriver_path, &self.geckodriver_sha256) {
            (Some(_), None) => {
                return Err(
                    "browser.geckodriver_path ohne browser.geckodriver_sha256 ist nicht \
                     erlaubt (Pinning verlangt beide)"
                        .to_owned(),
                );
            }
            (None, Some(_)) => {
                return Err(
                    "browser.geckodriver_sha256 ohne browser.geckodriver_path ist nicht \
                     erlaubt (Pinning verlangt beide)"
                        .to_owned(),
                );
            }
            _ => {}
        }
        if let Some(digest) = &self.geckodriver_sha256 {
            let valid = digest.len() == 64
                && digest.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
            if !valid {
                return Err(format!(
                    "browser.geckodriver_sha256 muss aus 64 klein geschriebenen Hex-Zeichen \
                     bestehen, gefunden {digest:?}"
                ));
            }
        }
        Ok(())
    }
}

fn default_max_actions() -> u32 {
    20
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_sha256() -> String {
        "a".repeat(64)
    }

    #[test]
    fn test_browser_section_defaults_from_empty_toml() {
        let section: BrowserSection = toml::from_str("").unwrap();
        assert!(!section.enabled);
        assert!(section.allowed_origins.is_empty());
        assert_eq!(section.max_actions, 20);
        assert_eq!(section.geckodriver_path, None);
        assert_eq!(section.geckodriver_sha256, None);
        assert_eq!(section, BrowserSection::default());
    }

    #[test]
    fn test_browser_section_full_toml_round_trip() {
        let src = format!(
            r#"
            enabled = true
            allowed_origins = ["https://intranet.example.test"]
            max_actions = 5
            geckodriver_path = "/opt/geckodriver/geckodriver"
            geckodriver_sha256 = "{}"
        "#,
            valid_sha256()
        );
        let section: BrowserSection = toml::from_str(&src).unwrap();
        assert!(section.enabled);
        assert_eq!(
            section.allowed_origins,
            vec!["https://intranet.example.test".to_owned()]
        );
        assert_eq!(section.max_actions, 5);
        assert_eq!(
            section.geckodriver_path,
            Some(PathBuf::from("/opt/geckodriver/geckodriver"))
        );
        assert_eq!(section.geckodriver_sha256.as_deref(), Some(valid_sha256().as_str()));

        let encoded = toml::to_string(&section).unwrap();
        let decoded: BrowserSection = toml::from_str(&encoded).unwrap();
        assert_eq!(decoded, section);
    }

    #[test]
    fn test_browser_section_rejects_unknown_field() {
        let src = r#"
            enabled = true
            enalbed = true
        "#;
        let error = toml::from_str::<BrowserSection>(src).unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[test]
    fn test_validate_accepts_default_section() {
        assert!(BrowserSection::default().validate().is_ok());
    }

    #[test]
    fn test_validate_rejects_enabled_without_allowed_origins() {
        let section = BrowserSection {
            enabled: true,
            ..BrowserSection::default()
        };
        let error = section.validate().unwrap_err();
        assert!(error.contains("allowed_origins"));
    }

    #[test]
    fn test_validate_rejects_host_only_origin_without_scheme() {
        let section = BrowserSection {
            enabled: true,
            allowed_origins: vec!["intranet.example.test".to_owned()],
            ..BrowserSection::default()
        };
        let error = section.validate().unwrap_err();
        assert!(error.contains("intranet.example.test"));
    }

    #[test]
    fn test_validate_rejects_zero_max_actions() {
        let section = BrowserSection {
            max_actions: 0,
            ..BrowserSection::default()
        };
        let error = section.validate().unwrap_err();
        assert!(error.contains("max_actions"));
    }

    #[test]
    fn test_validate_rejects_geckodriver_path_without_sha256() {
        let section = BrowserSection {
            geckodriver_path: Some(PathBuf::from("/opt/geckodriver/geckodriver")),
            ..BrowserSection::default()
        };
        let error = section.validate().unwrap_err();
        assert!(error.contains("geckodriver_sha256"));
    }

    #[test]
    fn test_validate_rejects_geckodriver_sha256_without_path() {
        let section = BrowserSection {
            geckodriver_sha256: Some(valid_sha256()),
            ..BrowserSection::default()
        };
        let error = section.validate().unwrap_err();
        assert!(error.contains("geckodriver_path"));
    }

    #[test]
    fn test_validate_rejects_malformed_sha256() {
        let section = BrowserSection {
            geckodriver_path: Some(PathBuf::from("/opt/geckodriver/geckodriver")),
            geckodriver_sha256: Some("not-a-hash".to_owned()),
            ..BrowserSection::default()
        };
        let error = section.validate().unwrap_err();
        assert!(error.contains("geckodriver_sha256"));
    }

    #[test]
    fn test_validate_rejects_uppercase_sha256() {
        let section = BrowserSection {
            geckodriver_path: Some(PathBuf::from("/opt/geckodriver/geckodriver")),
            geckodriver_sha256: Some("A".repeat(64)),
            ..BrowserSection::default()
        };
        assert!(section.validate().is_err());
    }

    #[test]
    fn test_validate_accepts_matched_geckodriver_pinning() {
        let section = BrowserSection {
            enabled: true,
            allowed_origins: vec!["https://intranet.example.test".to_owned()],
            geckodriver_path: Some(PathBuf::from("/opt/geckodriver/geckodriver")),
            geckodriver_sha256: Some(valid_sha256()),
            ..BrowserSection::default()
        };
        assert!(section.validate().is_ok());
    }
}
