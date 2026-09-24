//! `[mode]` — Standard-Interaktionsmodus der Harness (AP W1-21..24).
//!
//! Der Modus steuert, mit welcher Grundhaltung eine neue Session startet
//! (reines Gespräch, Planungsmodus, Explorationsmodus, aktive
//! Arbeitsausführung oder Host-Shell-Arbeit). Dieses Modul kennt nur den
//! deklarierten String-Wert
//! aus `.harw/config.toml`; die Zuordnung zu konkretem Laufzeitverhalten
//! (z. B. welche Tools sichtbar sind) liegt beim Consumer, der `harw-config`
//! und die jeweiligen Modus-Implementierungen gemeinsam kennt.

use serde::{Deserialize, Serialize};

/// Erlaubte Werte für [`ModeSection::default`].
const ALLOWED_MODES: &[&str] = &["chat", "plan", "explore", "work", "shell"];

/// `[mode]` — Standard-Interaktionsmodus, mit dem eine neue Session
/// startet, sofern kein Aufruf-Kontext (CLI-Flag, Slash-Command) explizit
/// einen anderen Modus wählt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModeSection {
    /// Einer von `chat`, `plan`, `explore`, `work`, `shell`. Default: `chat`,
    /// damit eine neue Session als Gespräch beginnt; Planung, Exploration
    /// und Arbeit wählt man bewusst (`--mode`, `/mode`, `[mode] default`).
    #[serde(default = "default_mode")]
    pub default: String,
}

impl Default for ModeSection {
    fn default() -> Self {
        Self {
            default: default_mode(),
        }
    }
}

fn default_mode() -> String {
    "chat".to_owned()
}

impl ModeSection {
    /// Prüft, dass `default` einer der bekannten Modus-Namen ist.
    ///
    /// # Errors
    /// Liefert `Err(String)` mit einer menschenlesbaren Begründung, wenn
    /// `default` keiner von [`ALLOWED_MODES`] ist.
    pub fn validate(&self) -> Result<(), String> {
        if !ALLOWED_MODES.contains(&self.default.as_str()) {
            return Err(format!(
                "mode.default muss einer von {ALLOWED_MODES:?} sein, war {:?}",
                self.default
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_mode_section_defaults_from_empty_toml() -> TestResult {
        let section: ModeSection = toml::from_str("").map_err(ctx("leeres mode-toml parsen"))?;
        assert_eq!(section.default, "chat");
        assert_eq!(section, ModeSection::default());
        Ok(())
    }

    #[test]
    fn test_mode_section_full_toml_round_trip() -> TestResult {
        let src = r#"
            default = "plan"
        "#;
        let section: ModeSection = toml::from_str(src).map_err(ctx("mode-toml parsen"))?;
        assert_eq!(section.default, "plan");

        let encoded = toml::to_string(&section).map_err(ctx("mode-toml serialisieren"))?;
        let decoded: ModeSection =
            toml::from_str(&encoded).map_err(ctx("serialisiertes mode-toml re-parsen"))?;
        assert_eq!(decoded, section);
        Ok(())
    }

    #[test]
    fn test_mode_section_rejects_unknown_field() -> TestResult {
        let src = r#"
            default = "chat"
            defualt = "chat"
        "#;
        let Err(error) = toml::from_str::<ModeSection>(src) else {
            return Err(TestError::Unexpected(
                "unbekanntes Feld wurde nicht abgelehnt".to_owned(),
            ));
        };
        assert!(error.to_string().contains("unknown field"));
        Ok(())
    }

    #[test]
    fn test_validate_accepts_all_known_modes() {
        for mode in ALLOWED_MODES {
            let section = ModeSection {
                default: (*mode).to_owned(),
            };
            assert!(section.validate().is_ok(), "mode {mode:?} should validate");
        }
    }

    #[test]
    fn test_validate_rejects_unknown_mode() -> TestResult {
        let section = ModeSection {
            default: "sleepwalk".to_owned(),
        };
        let Err(error) = section.validate() else {
            return Err(TestError::Unexpected(
                "unbekannter Modus wurde nicht abgelehnt".to_owned(),
            ));
        };
        assert!(error.contains("sleepwalk"));
        Ok(())
    }

    #[test]
    fn test_validate_accepts_default_section() {
        assert!(ModeSection::default().validate().is_ok());
    }
}
