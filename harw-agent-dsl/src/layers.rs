//! Schichten-Modell für Harwness Definitions-Auflösung (§4 DSL-Spec).
//!
//! Dieses Modul definiert [`DefinitionLayer`], die geordnete Hierarchie der
//! Definitionsquellen. Spätere Schichten (höhere Ordinalwerte) haben höhere
//! Priorität und dürfen frühere nur durch explizite Merge-Operationen verfeinern.
//!
//! # Schlüsseltypen
//! - [`DefinitionLayer`] — Herkunfts-Enum mit natürlicher Prioritätsordnung
//!
//! # Invarianten (§4)
//! - BuiltIn (0) < InstalledPack (1) < UserGlobal (2) < Workspace (3) < Project (4) < RunLocal (5)
//! - RunLocal-Definitionen werden nicht stillschweigend in TOML zurückgeschrieben.
//!
//! # Nebenläufigkeit
//! `DefinitionLayer` ist `Copy + Send + Sync + Ord`.

use serde::{Deserialize, Serialize};

/// Herkunftsschicht einer Definition, von niedrigster zu höchster Priorität (§4).
///
/// # Beschreibung
/// Die Prioritätsreihenfolge bestimmt, welche Definition bei Konflikten bevorzugt
/// wird. RunLocal (5) überschreibt BuiltIn (0) vollständig, sobald eine explizite
/// Merge-Operation vorhanden ist.
///
/// # Varianten
/// - `BuiltIn` — eingebettete Harwness-Definitionen
/// - `InstalledPack` — installierte Agent-Packs
/// - `UserGlobal` — benutzerglobale Definitionen (`~/.config/harwness/`)
/// - `Workspace` — Workspace-Definitionen (`.harwness/` im Workspace)
/// - `Project` — Projekt-Overlays (`.harwness/overlays/`)
/// - `RunLocal` — Laufzeit-Instanziierungs-Overrides (nicht in TOML persistiert)
///
/// # Beispiele
/// ```rust
/// use harw_agent_dsl::layers::DefinitionLayer;
///
/// assert!(DefinitionLayer::BuiltIn < DefinitionLayer::RunLocal);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefinitionLayer {
    /// Eingebettete Harwness-Definitionen (niedrigste Priorität).
    BuiltIn = 0,
    /// Installierte Agent-Packs (`~/.config/harwness/packs/`).
    InstalledPack = 1,
    /// Benutzerglobale Definitionen (`~/.config/harwness/`).
    UserGlobal = 2,
    /// Workspace-Definitionen (`<workspace>/.harwness/`).
    Workspace = 3,
    /// Projekt-Overlays (`<project>/.harwness/overlays/`).
    Project = 4,
    /// Laufzeit-Instanziierungs-Overrides (höchste Priorität; nicht persistiert).
    RunLocal = 5,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestResult;

    #[test]
    fn test_ordering_ascending() {
        assert!(DefinitionLayer::BuiltIn < DefinitionLayer::InstalledPack);
        assert!(DefinitionLayer::InstalledPack < DefinitionLayer::UserGlobal);
        assert!(DefinitionLayer::UserGlobal < DefinitionLayer::Workspace);
        assert!(DefinitionLayer::Workspace < DefinitionLayer::Project);
        assert!(DefinitionLayer::Project < DefinitionLayer::RunLocal);
        // transitiv: BuiltIn < RunLocal
        assert!(DefinitionLayer::BuiltIn < DefinitionLayer::RunLocal);
    }

    #[test]
    fn test_serde_snake_case() -> TestResult {
        let json = serde_json::to_string(&DefinitionLayer::UserGlobal)?;
        assert_eq!(json, "\"user_global\"");
        let back: DefinitionLayer = serde_json::from_str(&json)?;
        assert_eq!(back, DefinitionLayer::UserGlobal);
        Ok(())
    }

    #[test]
    fn test_copy_semantics() {
        let a = DefinitionLayer::Workspace;
        let b = a;
        assert_eq!(a, b);
    }
}
