//! `[dod]` — Grundsatz-Policy für die DoD-Kette (Definition-of-Done-Sensorik,
//! Eskalation, Warden) (Remediationsplan Teil B, Welle W3, AP `C-CFG`;
//! Konsumenten: `harw-dod-escalate`/`harw-escalator` `D-ESC`,
//! `harw-dod-warden`/`harw-warden` `D-WARDEN`, `harw-dod-sentinel` `D-SENTLIB`).
//!
//! Dieses Modul definiert ausschließlich die deklarative Policy, unter der
//! die DoD-Kette eskalieren darf: ob ein Kill-Kommando ausschließlich mit
//! menschlicher Freigabe ausgeführt werden darf
//! ([`DodSection::kill_requires_human`] — laut Plan-Entscheidung "Warden-
//! Automatik: Freeze/Release automatisch nach signiertem Proof; Kill nur mit
//! menschlicher Freigabe (Default aus)" **nicht verhandelbar**, siehe
//! [`DodSection::validate`]), ob Freeze/Release-Übergänge automatisch nach
//! einem signierten Proof erfolgen ([`DodSection::auto_freeze`]), wo die
//! Schlüssel für `SignedAuthorization`-Prüfung liegen
//! ([`DodSection::proof_key_dir`]), und welche cgroup-Pfadpräfixe die
//! Eskalationskette überhaupt adressieren darf
//! ([`DodSection::allowed_cgroup_prefixes`] — Design "Ziel-cgroup aus Record
//! + Präfix"/"cgroup-Präfix-Allowlist"). Durchsetzung liegt vollständig beim
//!   Consumer (Escalator/Warden-Binaries); dieses Modul beschreibt nur die
//!   Konfiguration und deren Invarianten.
//!
//! Defaults sind bewusst restriktiv (`Default = sicher/aus`):
//! `kill_requires_human = true`, `auto_freeze = true`, leere
//! cgroup-Präfixliste. Ein nicht vertrauter Repo-Layer darf diese Sektion
//! nur verengen
//! (`allowed_cgroup_prefixes` nur Schnittmenge, `auto_freeze` nur Richtung
//! `true`); `kill_requires_human` ist ohnehin in jeder Layer fest auf `true`
//! erzwungen (siehe unten) und `proof_key_dir` wird aus einem Repo-Layer
//! **nie** übernommen (Umlenkung auf einen fremden Schlüsselordner wäre eine
//! Rechteausweitung). Die Merge-Logik lebt in `harw_config::discovery`, nicht
//! hier.
//!
//! Keine Nebenläufigkeit: reiner Datentyp, `Send + Sync` über `derive`.
//! Fehler: [`DodSection::validate`] liefert `Result<(), String>` (gleiches
//! Muster wie `research_toml::ResearchSection::validate`).
//!
//! # Examples
//! ```rust
//! use harw_config::DodSection;
//!
//! let section: DodSection = toml::from_str("").unwrap();
//! assert!(section.kill_requires_human);
//! assert!(section.validate().is_ok());
//! ```

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// `[dod]` — Eskalations-Policy für die DoD-Kette: Kill-Freigabe,
/// Auto-Freeze, Schlüsselverzeichnis für Warden-Proofs und erlaubte
/// cgroup-Präfixe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DodSection {
    /// Muss laut Plan-Entscheidung ("Kill nur mit menschlicher Freigabe")
    /// immer `true` sein. Nicht verhandelbar: [`Self::validate`] lehnt
    /// `false` in jeder Layer ab, unabhängig davon, ob sie vertraut ist.
    #[serde(default = "default_true")]
    pub kill_requires_human: bool,
    /// Erlaubt automatisches Freeze/Release, sobald ein gültiger,
    /// signierter `WardenRequest`-Proof vorliegt. `true` (Default) ist die
    /// sichere Voreinstellung (siehe Modul-Doku); Abschalten reduziert den
    /// Automatisierungsgrad der DoD-Kette, nicht ihre Sicherheit — daher darf
    /// ein Repo-Layer diesen Wert nur *anheben*, nie absenken.
    #[serde(default = "default_true")]
    pub auto_freeze: bool,
    /// Verzeichnis, in dem `harw dod install` die `ProofKey`-Materialien
    /// ablegt bzw. aus dem der Warden sie liest. `None` (Default) heißt:
    /// Consumer nutzt seinen eigenen fest verdrahteten Standardpfad.
    #[serde(default)]
    pub proof_key_dir: Option<PathBuf>,
    /// cgroup-Pfadpräfixe, auf die Eskalationsaktionen (Freeze/Release/Kill)
    /// beschränkt sind. Leer (Default) heißt: keine cgroup adressierbar, bis
    /// der Consumer explizit Präfixe erhält.
    #[serde(default)]
    pub allowed_cgroup_prefixes: Vec<String>,
}

impl Default for DodSection {
    fn default() -> Self {
        Self {
            kill_requires_human: true,
            auto_freeze: true,
            proof_key_dir: None,
            allowed_cgroup_prefixes: Vec::new(),
        }
    }
}

impl DodSection {
    /// Prüft Invarianten, die reine Deserialisierung nicht ausdrücken kann.
    ///
    /// # Errors
    /// Liefert `Err(String)` mit einer menschenlesbaren Begründung, wenn
    /// `kill_requires_human == false` (in jeder Layer, unabhängig von
    /// Vertrauensstatus — Kill ohne menschliche Freigabe ist per
    /// Plan-Entscheidung nie zulässig), oder wenn `allowed_cgroup_prefixes`
    /// einen leeren Eintrag enthält.
    pub fn validate(&self) -> Result<(), String> {
        if !self.kill_requires_human {
            return Err(
                "dod.kill_requires_human muss immer true sein (Warden-Kill nur mit \
                 menschlicher Freigabe)"
                    .to_owned(),
            );
        }
        for prefix in &self.allowed_cgroup_prefixes {
            if prefix.trim().is_empty() {
                return Err("dod.allowed_cgroup_prefixes enthält einen leeren Eintrag".to_owned());
            }
        }
        Ok(())
    }
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_dod_section_defaults_from_empty_toml() -> TestResult {
        let section: DodSection = toml::from_str("").map_err(ctx("parse toml"))?;
        assert!(section.kill_requires_human);
        assert!(section.auto_freeze);
        assert_eq!(section.proof_key_dir, None);
        assert!(section.allowed_cgroup_prefixes.is_empty());
        assert_eq!(section, DodSection::default());
        Ok(())
    }

    #[test]
    fn test_dod_section_full_toml_round_trip() -> TestResult {
        let src = r#"
            kill_requires_human = true
            auto_freeze = false
            proof_key_dir = "/var/lib/harw/dod/keys"
            allowed_cgroup_prefixes = ["/sys/fs/cgroup/harw.slice/"]
        "#;
        let section: DodSection = toml::from_str(src).map_err(ctx("parse toml"))?;
        assert!(section.kill_requires_human);
        assert!(!section.auto_freeze);
        assert_eq!(
            section.proof_key_dir,
            Some(PathBuf::from("/var/lib/harw/dod/keys"))
        );
        assert_eq!(
            section.allowed_cgroup_prefixes,
            vec!["/sys/fs/cgroup/harw.slice/".to_owned()]
        );

        let encoded = toml::to_string(&section).map_err(ctx("encode toml"))?;
        let decoded: DodSection = toml::from_str(&encoded).map_err(ctx("parse encoded toml"))?;
        assert_eq!(decoded, section);
        Ok(())
    }

    #[test]
    fn test_dod_section_rejects_unknown_field() -> TestResult {
        let src = r#"
            kill_requires_human = true
            kill_reqiures_human = true
        "#;
        let Err(error) = toml::from_str::<DodSection>(src) else {
            return Err(TestError::Unexpected(
                "unknown field must be rejected".to_string(),
            ));
        };
        assert!(error.to_string().contains("unknown field"));
        Ok(())
    }

    #[test]
    fn test_validate_accepts_default_section() {
        assert!(DodSection::default().validate().is_ok());
    }

    #[test]
    fn test_validate_rejects_kill_requires_human_false() -> TestResult {
        let section = DodSection {
            kill_requires_human: false,
            ..DodSection::default()
        };
        let Err(error) = section.validate() else {
            return Err(TestError::Unexpected(
                "kill_requires_human = false must be rejected".to_string(),
            ));
        };
        assert!(error.contains("kill_requires_human"));
        Ok(())
    }

    #[test]
    fn test_validate_rejects_blank_cgroup_prefix() -> TestResult {
        let section = DodSection {
            allowed_cgroup_prefixes: vec!["   ".to_owned()],
            ..DodSection::default()
        };
        let Err(error) = section.validate() else {
            return Err(TestError::Unexpected(
                "blank cgroup prefix must be rejected".to_string(),
            ));
        };
        assert!(error.contains("allowed_cgroup_prefixes"));
        Ok(())
    }

    #[test]
    fn test_validate_accepts_disabled_auto_freeze_with_human_kill() {
        let section = DodSection {
            auto_freeze: false,
            ..DodSection::default()
        };
        assert!(section.validate().is_ok());
    }
}
