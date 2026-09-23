//! `[permissions]` — persistente Freigabe-Policy (Contract
//! `harw-scopes-contract.md` §2/§5 Zeile A2; Plan
//! `nope-permissions-gibt-es-wild-lobster.md` Schritt 4).
//!
//! `default_mode` liegt üblicherweise in der User-Ebene
//! (`~/.harw/config.toml`), `[[permissions.allow]]`/`[[permissions.deny]]`
//! sowie `extra_roots` üblicherweise in der Projekt-Ebene (`.harw/config.toml`
//! außerhalb des Repos). Welche Datei welchen Teil dieser Sektion schreibt,
//! entscheidet der Aufrufer (`harw-config::writer::ConfigWriter`); dieses
//! Modul kennt nur das Schema und dessen Invarianten.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Erlaubte Werte für [`PermissionsSection::default_mode`].
const ALLOWED_MODES: &[&str] = &["ask", "auto", "full"];

/// Untere Grenze für [`PermissionsSection::approval_timeout_secs`] (10 s).
const MIN_TIMEOUT_SECS: u64 = 10;

/// Obere Grenze für [`PermissionsSection::approval_timeout_secs`] (24 h).
const MAX_TIMEOUT_SECS: u64 = 86_400;

/// Höchstzahl zusätzlicher Arbeitswurzeln (`extra_roots`), analog zur
/// Grenze aus `harw-sandbox` (Contract §5 Zeile A8).
const MAX_EXTRA_ROOTS: usize = 8;

/// `[permissions]` — Freigabemodus, Timeout, Allow/Deny-Regeln und
/// zusätzliche Arbeitswurzeln.
///
/// # Description
/// Reine Deserialisierung akzeptiert absichtlich mehr als
/// [`PermissionsSection::validate`] erlaubt (z. B. beliebige
/// `default_mode`-Strings), damit `toml_edit`-Bearbeitungen und
/// Zwischenzustände nicht am Parser scheitern. Die Invarianten werden erst
/// beim Übernehmen in die Laufzeit bzw. vor dem Schreiben
/// (`ConfigWriter::save`) geprüft.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PermissionsSection {
    /// Standard-Freigabemodus: `ask`, `auto` oder `full`. `None` bedeutet
    /// „von dieser Ebene nicht gesetzt“.
    #[serde(default)]
    pub default_mode: Option<String>,
    /// Timeout in Sekunden, nach dem eine offene Freigabeanfrage automatisch
    /// abgelehnt wird. `None` bedeutet „von dieser Ebene nicht gesetzt“.
    #[serde(default)]
    pub approval_timeout_secs: Option<u64>,
    /// Regeln, die eine Anfrage ohne Rückfrage erlauben.
    #[serde(default)]
    pub allow: Vec<RuleToml>,
    /// Regeln, die eine Anfrage immer ablehnen (gewinnt scope-übergreifend
    /// über `allow`, siehe Contract §2 — Durchsetzung liegt bei
    /// `harw-extension-api`).
    #[serde(default)]
    pub deny: Vec<RuleToml>,
    /// Zusätzliche, dauerhaft gemerkte Arbeitswurzeln (`/add-workdir …
    /// merken`, Plan Schritt 6). Müssen kanonisch-absolute Pfade sein.
    #[serde(default)]
    pub extra_roots: Vec<PathBuf>,
}

/// Eine einzelne Allow-/Deny-Regel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleToml {
    /// Werkzeugname, gegen den die Regel greift (z. B. `"shell.exec"`).
    pub tool: String,
    /// Optionales Muster (Shell-Präfix bzw. Pfad-Glob je nach `tool`); die
    /// genaue Semantik gehört `harw-extension-api::allow_rules`.
    #[serde(default)]
    pub pattern: Option<String>,
}

impl RuleToml {
    /// Prüft, dass `tool` nicht leer ist.
    ///
    /// # Errors
    /// Liefert `Err(String)` mit einer menschenlesbaren Begründung, wenn
    /// `tool` (nach Trimmen) leer ist.
    pub fn validate(&self) -> Result<(), String> {
        if self.tool.trim().is_empty() {
            return Err("permissions-Regel: tool darf nicht leer sein".to_owned());
        }
        Ok(())
    }
}

impl PermissionsSection {
    /// Prüft Invarianten, die reine Deserialisierung nicht ausdrücken kann.
    ///
    /// # Errors
    /// Liefert `Err(String)` mit einer menschenlesbaren Begründung, wenn:
    /// - `default_mode` gesetzt, aber keiner von `ask`/`auto`/`full` ist;
    /// - `approval_timeout_secs` gesetzt, aber außerhalb `10..=86400` liegt;
    /// - eine `allow`- oder `deny`-Regel ein leeres `tool` hat;
    /// - `extra_roots` mehr als 8 Einträge hat, oder ein Eintrag relativ ist.
    pub fn validate(&self) -> Result<(), String> {
        if let Some(mode) = &self.default_mode {
            if !ALLOWED_MODES.contains(&mode.as_str()) {
                return Err(format!(
                    "permissions.default_mode muss einer von {ALLOWED_MODES:?} sein, war {mode:?}"
                ));
            }
        }
        if let Some(secs) = self.approval_timeout_secs {
            if !(MIN_TIMEOUT_SECS..=MAX_TIMEOUT_SECS).contains(&secs) {
                return Err(format!(
                    "permissions.approval_timeout_secs muss zwischen {MIN_TIMEOUT_SECS} und {MAX_TIMEOUT_SECS} liegen, war {secs}"
                ));
            }
        }
        for rule in self.allow.iter().chain(self.deny.iter()) {
            rule.validate()?;
        }
        if self.extra_roots.len() > MAX_EXTRA_ROOTS {
            return Err(format!(
                "permissions.extra_roots erlaubt höchstens {MAX_EXTRA_ROOTS} Einträge, waren {}",
                self.extra_roots.len()
            ));
        }
        for root in &self.extra_roots {
            if !root.is_absolute() {
                return Err(format!(
                    "permissions.extra_roots erwartet absolute Pfade, war {root:?}"
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestError, TestResult, ctx};

    #[test]
    fn test_default_section_is_empty_and_valid() {
        let section = PermissionsSection::default();
        assert!(section.default_mode.is_none());
        assert!(section.approval_timeout_secs.is_none());
        assert!(section.allow.is_empty());
        assert!(section.deny.is_empty());
        assert!(section.extra_roots.is_empty());
        assert!(section.validate().is_ok());
    }

    #[test]
    fn test_full_toml_round_trip() -> TestResult {
        let src = r#"
            default_mode = "auto"
            approval_timeout_secs = 60
            extra_roots = ["/home/mia/scratch"]

            [[allow]]
            tool = "shell.exec"
            pattern = "cargo check"

            [[deny]]
            tool = "fs.write"
        "#;
        let section: PermissionsSection = toml::from_str(src).map_err(ctx("parse toml"))?;
        assert_eq!(section.default_mode.as_deref(), Some("auto"));
        assert_eq!(section.approval_timeout_secs, Some(60));
        assert_eq!(section.allow.len(), 1);
        assert_eq!(section.allow[0].tool, "shell.exec");
        assert_eq!(section.allow[0].pattern.as_deref(), Some("cargo check"));
        assert_eq!(section.deny.len(), 1);
        assert_eq!(section.deny[0].tool, "fs.write");
        assert!(section.deny[0].pattern.is_none());
        assert_eq!(
            section.extra_roots,
            vec![PathBuf::from("/home/mia/scratch")]
        );
        assert!(section.validate().is_ok());

        let encoded = toml::to_string(&section).map_err(ctx("encode toml"))?;
        let decoded: PermissionsSection =
            toml::from_str(&encoded).map_err(ctx("parse encoded toml"))?;
        assert_eq!(decoded, section);
        Ok(())
    }

    #[test]
    fn test_validate_rejects_unknown_default_mode() -> TestResult {
        let section = PermissionsSection {
            default_mode: Some("yolo".to_owned()),
            ..PermissionsSection::default()
        };
        let Err(error) = section.validate() else {
            return Err(TestError::Unexpected(
                "unknown default_mode must be rejected".to_string(),
            ));
        };
        assert!(error.contains("default_mode"));
        Ok(())
    }

    #[test]
    fn test_validate_rejects_timeout_below_minimum() {
        let section = PermissionsSection {
            approval_timeout_secs: Some(5),
            ..PermissionsSection::default()
        };
        assert!(section.validate().is_err());
    }

    #[test]
    fn test_validate_rejects_timeout_above_maximum() {
        let section = PermissionsSection {
            approval_timeout_secs: Some(86_401),
            ..PermissionsSection::default()
        };
        assert!(section.validate().is_err());
    }

    #[test]
    fn test_validate_accepts_timeout_bounds_inclusive() {
        let low = PermissionsSection {
            approval_timeout_secs: Some(10),
            ..PermissionsSection::default()
        };
        let high = PermissionsSection {
            approval_timeout_secs: Some(86_400),
            ..PermissionsSection::default()
        };
        assert!(low.validate().is_ok());
        assert!(high.validate().is_ok());
    }

    #[test]
    fn test_validate_rejects_empty_rule_tool() -> TestResult {
        let section = PermissionsSection {
            allow: vec![RuleToml {
                tool: "   ".to_owned(),
                pattern: None,
            }],
            ..PermissionsSection::default()
        };
        let Err(error) = section.validate() else {
            return Err(TestError::Unexpected(
                "empty rule tool must be rejected".to_string(),
            ));
        };
        assert!(error.contains("tool"));
        Ok(())
    }

    #[test]
    fn test_validate_rejects_relative_extra_root() -> TestResult {
        let section = PermissionsSection {
            extra_roots: vec![PathBuf::from("relative/path")],
            ..PermissionsSection::default()
        };
        let Err(error) = section.validate() else {
            return Err(TestError::Unexpected(
                "relative extra_root must be rejected".to_string(),
            ));
        };
        assert!(error.contains("extra_roots"));
        Ok(())
    }

    #[test]
    fn test_validate_rejects_more_than_eight_extra_roots() -> TestResult {
        let section = PermissionsSection {
            extra_roots: (0..9)
                .map(|n| PathBuf::from(format!("/root/{n}")))
                .collect(),
            ..PermissionsSection::default()
        };
        let Err(error) = section.validate() else {
            return Err(TestError::Unexpected(
                "more than eight extra_roots must be rejected".to_string(),
            ));
        };
        assert!(error.contains("extra_roots"));
        Ok(())
    }

    #[test]
    fn test_validate_accepts_exactly_eight_extra_roots() {
        let section = PermissionsSection {
            extra_roots: (0..8)
                .map(|n| PathBuf::from(format!("/root/{n}")))
                .collect(),
            ..PermissionsSection::default()
        };
        assert!(section.validate().is_ok());
    }

    #[test]
    fn test_rejects_unknown_field() {
        let src = r#"
            defualt_mode = "auto"
        "#;
        assert!(toml::from_str::<PermissionsSection>(src).is_err());
    }
}
