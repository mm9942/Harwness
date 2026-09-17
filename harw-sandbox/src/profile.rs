//! Sandbox-Profil: bündelt die Modulwahl für eine Prozess-Sandbox.
//!
//! [`SandboxProfile`] ist ein vertrauenswürdiger Runtime-Input, der beim Aufbau
//! der Runtime aus Konfiguration und UI-Freigaben entsteht. Er ist **kein**
//! Tool-Argument und **kein** CLI-Flag. Ein Modell kann das Profil weder setzen
//! noch überschreiben.
//!
//! Der Standard bleibt [`SandboxProfile::Strict`]: hermetische Bubblewrap-Sandbox
//! ohne Toolchain, tmux-Socket oder Host-Zugriff.

use crate::{CargoSandboxProfile, TmuxSandboxProfile};

/// Bündelt die aktiven Sandbox-Module eines Ausführungsworkers.
///
/// Jede Variante aktiviert genau die Bindungen, die für ihr Profil nötig sind.
/// Der Standard ist [`SandboxProfile::Strict`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SandboxProfile {
    /// Strikte Bubblewrap-Sandbox: kein Cargo, kein tmux, kein Host.
    Strict,
    /// Isolierte Bubblewrap-Sandbox mit aktiviertem Cargo-Modul.
    Cargo(CargoSandboxProfile),
    /// Isolierte Bubblewrap-Sandbox mit einem einzelnen validierten tmux-Socket.
    Tmux(TmuxSandboxProfile),
    /// Lokale Host-Ausführung; nur nach UI-Approval möglich.
    Host,
}

impl Default for SandboxProfile {
    fn default() -> Self {
        Self::Strict
    }
}

impl SandboxProfile {
    /// `true`, wenn das Profil ein Cargo-Modul trägt.
    #[must_use]
    pub fn is_cargo(&self) -> bool {
        matches!(self, Self::Cargo(_))
    }

    /// `true`, wenn das Profil ein tmux-Modul trägt.
    #[must_use]
    pub fn is_tmux(&self) -> bool {
        matches!(self, Self::Tmux(_))
    }

    /// `true`, wenn das Profil direkte Host-Ausführung erlaubt.
    #[must_use]
    pub fn is_host(&self) -> bool {
        matches!(self, Self::Host)
    }

    /// `true`, wenn das Profil strikt hermetisch bleibt (keine Module).
    #[must_use]
    pub fn is_strict(&self) -> bool {
        matches!(self, Self::Strict)
    }

    /// Liefert das Cargo-Profil, falls vorhanden.
    #[must_use]
    pub fn cargo_profile(&self) -> Option<&CargoSandboxProfile> {
        match self {
            Self::Cargo(profile) => Some(profile),
            _ => None,
        }
    }

    /// Liefert das tmux-Profil, falls vorhanden.
    #[must_use]
    pub fn tmux_profile(&self) -> Option<&TmuxSandboxProfile> {
        match self {
            Self::Tmux(profile) => Some(profile),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_strict() {
        assert_eq!(SandboxProfile::default(), SandboxProfile::Strict);
        assert!(!SandboxProfile::Strict.is_cargo());
        assert!(!SandboxProfile::Strict.is_tmux());
        assert!(!SandboxProfile::Strict.is_host());
        assert!(SandboxProfile::Strict.cargo_profile().is_none());
        assert!(SandboxProfile::Strict.tmux_profile().is_none());
    }

    #[test]
    fn host_is_host() {
        assert!(SandboxProfile::Host.is_host());
        assert!(!SandboxProfile::Host.is_cargo());
        assert!(!SandboxProfile::Host.is_strict());
    }

    #[test]
    fn strict_is_strict() {
        assert!(SandboxProfile::Strict.is_strict());
        assert!(!SandboxProfile::Host.is_strict());
    }
}
