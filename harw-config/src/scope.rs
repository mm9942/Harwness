//! `SettingScope` — die drei Lebensdauern einer Einstellung (Contract
//! `harw-scopes-contract.md` §2, Zeile A2).
//!
//! Eine Einstellung kann auf drei Ebenen mit steigender Präzedenz gelten:
//! `Global` (dauerhaft für den User, `~/.harw/config.toml`), `Project`
//! (dauerhaft pro Projekt, `~/.harw/profiles/<p>/projects/<key>/settings.toml`)
//! und `Session` (nur im Speicher, endet mit der Session). Bei einem
//! Modus-Konflikt gewinnt die Ebene mit der höchsten Präzedenz
//! (`Session > Project > Global`); bei Allow/Deny-Regeln gilt zusätzlich
//! „Deny gewinnt über alle Scopes hinweg“ — das ist Sache des Consumers
//! (`harw-extension-api`), nicht dieses Moduls.
//!
//! Dieses Modul kennt ausschließlich den Scope-Wert selbst: Ordnung nach
//! Präzedenz, textuelle Darstellung und `serde`-Kodierung. Es hat keine
//! Kenntnis von konkreten Speicherorten oder Regelinhalten.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::ConfigError;

/// Lebensdauer einer Einstellung, aufsteigend nach Präzedenz deklariert.
///
/// # Description
/// Die Deklarationsreihenfolge der Varianten (`Global`, `Project`,
/// `Session`) entspricht absichtlich der Präzedenzordnung: der
/// abgeleitete [`Ord`] sortiert Enum-Varianten nach Deklarationsposition,
/// wodurch `Global < Project < Session` ohne manuelle Implementierung gilt.
///
/// # Examples
/// ```rust
/// use harw_config::SettingScope;
///
/// assert!(SettingScope::Global < SettingScope::Project);
/// assert!(SettingScope::Project < SettingScope::Session);
/// assert_eq!(SettingScope::Session, SettingScope::Session.max(SettingScope::Global));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SettingScope {
    /// Dauerhaft für den User (`~/.harw/config.toml` bzw. aktives Profil).
    Global,
    /// Dauerhaft pro Projekt (autoritätsgewährend außerhalb des Repos).
    Project,
    /// Nur im Speicher, endet mit der Session.
    Session,
}

impl SettingScope {
    /// Gibt die kanonische Kleinschreib-Textform zurück (`"session"`,
    /// `"project"`, `"global"`).
    ///
    /// # Returns
    /// Ein statischer `&'static str`, identisch zur `serde`-Kodierung und zu
    /// [`FromStr`].
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            SettingScope::Session => "session",
            SettingScope::Project => "project",
            SettingScope::Global => "global",
        }
    }
}

impl fmt::Display for SettingScope {
    /// Schreibt die Kleinschreib-Textform (siehe [`SettingScope::as_str`]).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for SettingScope {
    type Err = ConfigError;

    /// Parst `"session"`, `"project"` oder `"global"` (exakte Kleinschreibung).
    ///
    /// # Errors
    /// - [`ConfigError::Invalid`]: `s` ist keiner der drei erlaubten Werte.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "session" => Ok(SettingScope::Session),
            "project" => Ok(SettingScope::Project),
            "global" => Ok(SettingScope::Global),
            other => Err(ConfigError::Invalid(format!(
                "unbekannter SettingScope {other:?}, erwartet session|project|global"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn test_precedence_ordering_global_lt_project_lt_session() {
        assert!(SettingScope::Global < SettingScope::Project);
        assert!(SettingScope::Project < SettingScope::Session);
        assert!(SettingScope::Global < SettingScope::Session);
        assert_eq!(
            SettingScope::Session.cmp(&SettingScope::Global),
            Ordering::Greater
        );
    }

    #[test]
    fn test_as_str_matches_from_str_round_trip() {
        for scope in [
            SettingScope::Session,
            SettingScope::Project,
            SettingScope::Global,
        ] {
            let parsed: SettingScope = scope.as_str().parse().expect("valid scope text");
            assert_eq!(parsed, scope);
        }
    }

    #[test]
    fn test_from_str_rejects_unknown_value() {
        let error = "world".parse::<SettingScope>().unwrap_err();
        assert!(error.to_string().contains("world"));
    }

    #[test]
    fn test_display_uses_lowercase_text() {
        assert_eq!(SettingScope::Global.to_string(), "global");
        assert_eq!(SettingScope::Project.to_string(), "project");
        assert_eq!(SettingScope::Session.to_string(), "session");
    }

    #[test]
    fn test_serde_uses_lowercase_encoding() {
        let encoded = serde_json::to_string(&SettingScope::Project).expect("serialize scope");
        assert_eq!(encoded, "\"project\"");
        let decoded: SettingScope = serde_json::from_str("\"session\"").expect("deserialize scope");
        assert_eq!(decoded, SettingScope::Session);
    }
}
