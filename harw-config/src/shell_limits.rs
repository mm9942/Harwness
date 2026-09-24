//! `[shell]` — Zeitlimits für `shell.exec` (Runde 5, Teil N).
//!
//! # Beschreibung
//! | Schlüssel | Vorgabe | erlaubt | Bedeutung |
//! |---|---|---|---|
//! | `max_timeout_secs` | 900 | 30–3600 | Obergrenze für das vom Modell angegebene `timeout_secs` eines `shell.exec`-Aufrufs (Sandbox, Host und Host-Mode-Anfrage) |
//!
//! Die Vorgabe **ohne** `timeout_secs` bleibt 30 s; Build-/Test-Befehle
//! (`cargo`, `make`, `npm`, `pytest`, `go`, …) bekommen ohne Angabe 600 s —
//! beides gedeckelt durch `max_timeout_secs` (siehe
//! `harw_tool_shell::exec`). Ungültige Werte werden in den erlaubten Bereich
//! **geklemmt**, nie abgelehnt.
//!
//! # Merge-Regel
//! Nur vertraute Layer (Home **und** Profil) setzen frei — auch nach oben.
//! Ein nicht vertrautes Projekt darf die Obergrenze nur **senken**
//! (`crate::merge`, `merge_shell_limits`); ein Erhöhungsversuch wird
//! ignoriert und als `ScopeDiagnostic` gemeldet.
//!
//! # Nebenläufigkeit
//! Reine Datentypen und Funktionen.

use serde::{Deserialize, Serialize};

/// Vorgabe für `[shell] max_timeout_secs` (15 min).
pub const DEFAULT_SHELL_MAX_TIMEOUT_SECS: u64 = 900;
/// Erlaubter Bereich für `[shell] max_timeout_secs`.
pub const SHELL_MAX_TIMEOUT_SECS_RANGE: (u64, u64) = (30, 3600);

/// `[shell]` — Zeitlimits für `shell.exec`, wie sie in der TOML stehen.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellToml {
    /// Obergrenze für `timeout_secs` eines einzelnen `shell.exec`-Aufrufs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_timeout_secs: Option<u64>,
}

impl ShellToml {
    /// Die wirksame Obergrenze in Sekunden: Vorgabe 900, geklemmt auf
    /// 30–3600.
    #[must_use]
    pub fn effective_max_timeout_secs(&self) -> u64 {
        self.max_timeout_secs
            .unwrap_or(DEFAULT_SHELL_MAX_TIMEOUT_SECS)
            .clamp(
                SHELL_MAX_TIMEOUT_SECS_RANGE.0,
                SHELL_MAX_TIMEOUT_SECS_RANGE.1,
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_is_fifteen_minutes() {
        assert_eq!(ShellToml::default().effective_max_timeout_secs(), 900);
    }

    #[test]
    fn test_out_of_range_values_are_clamped() {
        let low = ShellToml {
            max_timeout_secs: Some(5),
        };
        assert_eq!(low.effective_max_timeout_secs(), 30);
        let high = ShellToml {
            max_timeout_secs: Some(99_999),
        };
        assert_eq!(high.effective_max_timeout_secs(), 3600);
    }
}
