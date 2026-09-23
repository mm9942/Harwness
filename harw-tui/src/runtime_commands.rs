//! Laufzeit-Hilfen für Slash-Kommandos der TUI (Welle W2d-1/A, Befund
//! `w4-tui-control.md` §5).
//!
//! # Zweck
//! Bündelt drei kleine, rechterelevante Bausteine, die der Slash-Pfad der TUI
//! braucht, damit er dieselben Grenzen einhält wie die übrigen Flächen:
//! - [`caller_tier`] — die Berechtigungsstufe des Aufrufers kommt aus dem
//!   vertrauenswürdigen [`Principal`], nicht aus einer Konstante.
//! - [`slash_service_map`] — die Dienste für Slash-Kommandos kommen aus
//!   [`RuntimeServices::service_map`] mit [`ServiceSurface::Slash`].
//! - [`validate_tool_toggle`] — `/tools on|off <name>` darf die Decke der
//!   Session (Basis ∩ Modus, `AgentSession::mode_ceiling`) nie erweitern und
//!   meldet Tippfehler als Fehler statt als Erfolg.
//!
//! # Typen
//! - [`ToolToggleError`] — abgelehnter Tool-Umschaltwunsch.
//!
//! # Nebenläufigkeit
//! Alle Funktionen sind rein lesend und zustandslos; sie arbeiten auf
//! geliehenen Referenzen und sind aus jedem Thread aufrufbar.
//!
//! # Fehler
//! Nur [`validate_tool_toggle`] kann fehlschlagen, mit [`ToolToggleError`].
//! Die Fehlermeldungen nennen ausschließlich den Tool-Namen.
//!
//! # Beispiel
//! ```rust,ignore
//! let ceiling = session.mode_ceiling();
//! validate_tool_toggle(&ceiling, &tool_snapshot, "shell.exec", true)?;
//! ```

use harw_core::activation::SessionActivation;
use harw_extension_api::ToolName;
use harw_operations::ServiceMap;
use harw_runtime::{RuntimeServices, ServiceSurface};
use harw_types::{PermissionTier, Principal};

/// Liefert die Berechtigungsstufe des Aufrufers.
///
/// # Description
/// Die Stufe stammt aus dem an der Eingangsgrenze gebauten [`Principal`]
/// (`Principal::tier`) — nie aus einer fest verdrahteten Annahme.
///
/// # Arguments
/// - `principal` (`&Principal`): der authentifizierte Aufrufer.
///
/// # Returns
/// Die zugeteilte [`PermissionTier`].
///
/// Vertrag: Aufrufer folgt in W2d-2/D5 (app.rs).
#[must_use]
pub(crate) fn caller_tier(principal: &Principal) -> PermissionTier {
    principal.tier()
}

/// Liefert die Dienste, die Slash-Kommandos sehen dürfen.
///
/// # Description
/// Delegiert an [`RuntimeServices::service_map`] mit
/// [`ServiceSurface::Slash`], damit Slash-Kommando und Modell-Werkzeug aus
/// derselben Montage bedient werden.
///
/// # Arguments
/// - `services` (`&RuntimeServices`): die Laufzeitdienste des Prozesses.
///
/// # Returns
/// Eine frische [`ServiceMap`] für die Slash-Fläche.
///
/// Vertrag: Aufrufer folgt in W2d-2/D5 (app.rs).
#[must_use]
pub(crate) fn slash_service_map(services: &RuntimeServices) -> ServiceMap {
    services.service_map(ServiceSurface::Slash)
}

/// Abgelehnter Wunsch, ein Tool per `/tools on|off` umzuschalten.
///
/// # Description
/// Jede Variante trägt den angefragten Tool-Namen, damit die TUI eine
/// konkrete Meldung anzeigen kann.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ToolToggleError {
    /// Der Name ist in der Session nicht registriert (z. B. Tippfehler).
    UnknownTool {
        /// Angefragter Tool-Name.
        name: String,
    },
    /// Das Einschalten würde die Decke der Session (Basis ∩ Modus) erweitern.
    BeyondCeiling {
        /// Angefragter Tool-Name.
        name: String,
    },
}

impl std::fmt::Display for ToolToggleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownTool { name } => {
                write!(
                    f,
                    "unbekanntes Werkzeug '{name}': in dieser Session nicht registriert"
                )
            }
            Self::BeyondCeiling { name } => write!(
                f,
                "Werkzeug '{name}' kann nicht eingeschaltet werden: die Decke dieser Session (Basis ∩ Modus) erlaubt es nicht"
            ),
        }
    }
}

impl std::error::Error for ToolToggleError {}

/// Prüft, ob `/tools on|off <tool>` zulässig ist, bevor die Aktivierung
/// verändert wird.
///
/// # Description
/// - Ist `tool` nicht in `known_tools` enthalten, ist das ein Fehler — auch
///   beim Ausschalten, damit Tippfehler nicht still als Erfolg gelten.
/// - Beim Einschalten muss die Decke das Tool erlauben
///   ([`SessionActivation::is_tool_enabled`]); `/tools` darf weder Verbote der
///   Agent-Definition (Basis) noch des aktiven Modus aufheben.
/// - Das Ausschalten eines bekannten Tools verengt nur und ist immer zulässig.
///
/// # Arguments
/// - `ceiling` (`&SessionActivation`): Decke der Session = Basis-Aktivierung ∩
///   Modus-Aktivierung (`AgentSession::mode_ceiling`).
/// - `known_tools` (`&[(String, bool)]`): `(name, aktuell_aktiv)`-Paare aller
///   registrierten Tools; nur die Namen werden ausgewertet.
/// - `tool` (`&str`): angefragter Tool-Name.
/// - `enable` (`bool`): `true` für `on`, `false` für `off`.
///
/// # Returns
/// `Ok(())`, wenn die Änderung angewendet werden darf.
///
/// # Errors
/// - [`ToolToggleError::UnknownTool`], wenn `tool` nicht registriert ist.
/// - [`ToolToggleError::BeyondCeiling`], wenn `enable` gesetzt ist und die
///   Decke das Tool nicht erlaubt.
///
/// # Concurrency
/// Rein lesend; sicher aus jedem Thread.
pub(crate) fn validate_tool_toggle(
    ceiling: &SessionActivation,
    known_tools: &[(String, bool)],
    tool: &str,
    enable: bool,
) -> Result<(), ToolToggleError> {
    if !known_tools.iter().any(|(name, _)| name == tool) {
        return Err(ToolToggleError::UnknownTool {
            name: tool.to_owned(),
        });
    }
    if enable && !ceiling.is_tool_enabled(&ToolName::new(tool)) {
        return Err(ToolToggleError::BeyondCeiling {
            name: tool.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use harw_core::activation::ToolProfile;
    use harw_types::{IngressSurface, PrincipalKind};

    fn known() -> Vec<(String, bool)> {
        vec![
            ("fs.read".to_owned(), true),
            ("shell.exec".to_owned(), false),
        ]
    }

    /// Decke: nichts erlaubt außer `fs.read`.
    fn ceiling() -> SessionActivation {
        let mut act = SessionActivation::new(ToolProfile::Minimal);
        act.enable_tool(ToolName::new("fs.read"));
        act
    }

    #[test]
    fn test_caller_tier_returns_principal_tier() {
        let p = Principal::trusted_ingress(
            PrincipalKind::Human,
            "mia",
            IngressSurface::Tui,
            PermissionTier::Maintainer,
        );
        assert_eq!(caller_tier(&p), PermissionTier::Maintainer);
        assert_eq!(
            caller_tier(&p.child_of("explorer")),
            PermissionTier::Operator
        );
    }

    #[test]
    fn test_validate_tool_toggle_unknown_name_is_err() {
        let expected = Err(ToolToggleError::UnknownTool {
            name: "fs.raed".to_owned(),
        });
        assert_eq!(
            validate_tool_toggle(&ceiling(), &known(), "fs.raed", true),
            expected
        );
        assert_eq!(
            validate_tool_toggle(&ceiling(), &known(), "fs.raed", false),
            expected
        );
    }

    #[test]
    fn test_validate_tool_toggle_disable_known_tool_is_ok() {
        assert_eq!(
            validate_tool_toggle(&ceiling(), &known(), "fs.read", false),
            Ok(())
        );
        assert_eq!(
            validate_tool_toggle(&ceiling(), &known(), "shell.exec", false),
            Ok(())
        );
    }

    #[test]
    fn test_validate_tool_toggle_enable_forbidden_by_ceiling_is_err() {
        assert_eq!(
            validate_tool_toggle(&ceiling(), &known(), "shell.exec", true),
            Err(ToolToggleError::BeyondCeiling {
                name: "shell.exec".to_owned()
            })
        );
    }

    #[test]
    fn test_validate_tool_toggle_enable_allowed_by_ceiling_is_ok() {
        assert_eq!(
            validate_tool_toggle(&ceiling(), &known(), "fs.read", true),
            Ok(())
        );
    }

    #[test]
    fn test_tool_toggle_error_display_names_tool() {
        let unknown = ToolToggleError::UnknownTool {
            name: "fs.raed".to_owned(),
        };
        let beyond = ToolToggleError::BeyondCeiling {
            name: "shell.exec".to_owned(),
        };
        assert!(unknown.to_string().contains("fs.raed"));
        assert!(beyond.to_string().contains("shell.exec"));
        assert_ne!(unknown.to_string(), beyond.to_string());
    }
}
