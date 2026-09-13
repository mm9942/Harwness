//! Fehlertyp für `harw-code-graph`.
//!
//! Deckt alle Fehlerfälle beim Einlesen des Cargo-Workspace (`workspace.rs`),
//! beim Parsen des Lockfiles (`lockfile.rs`) und bei der Auflösung lokaler
//! Registry-Quellverzeichnisse (`registry_locator.rs`) ab. Siehe AP
//! W1-14..17 ("harw-code-graph") für die vollständige Varianten-Spezifikation.

use harw_macros::HarwError;

/// Alle Fehlerfälle des `harw-code-graph`-Crates.
///
/// `#[derive(HarwError)]` erzeugt `Display`, `std::error::Error` und die
/// `From`-Impls für die `#[from]`-Varianten; zusätzlich wird der Typalias
/// `CodeGraphResult<T>` generiert (siehe `harw-macros`).
#[derive(Debug, HarwError)]
pub enum CodeGraphError {
    /// Datei-I/O beim Lesen eines Manifests, Lockfiles oder Registry-Verzeichnisses.
    #[from]
    Io(std::io::Error),

    /// Ein Manifest oder Lockfile konnte nicht als TOML geparst werden.
    #[from]
    Toml(toml::de::Error),

    /// Unter dem angegebenen Pfad existiert kein lesbares `Cargo.toml`.
    #[msg("Cargo-Manifest nicht gefunden: '{path}'")]
    ManifestMissing {
        /// Der erwartete Manifest-Pfad.
        path: String,
    },

    /// Das Wurzel-Manifest enthält keinen `[workspace]`-Abschnitt.
    #[msg("Manifest '{path}' enthält keinen [workspace]-Abschnitt")]
    NoWorkspaceSection {
        /// Pfad des geprüften Wurzel-Manifests.
        path: String,
    },

    /// Ein referenziertes Workspace-Member wurde im Graphen nicht gefunden.
    #[msg("Workspace-Member '{name}' wurde nicht gefunden")]
    MemberMissing {
        /// Der gesuchte Crate-Name.
        name: String,
    },

    /// Der interne Abhängigkeitsgraph enthält einen Zyklus; die Ebenen-
    /// Berechnung (Kahn) konnte keinen weiteren Fortschritt machen.
    #[msg("Abhängigkeitszyklus erkannt: {crates:?}")]
    CycleDetected {
        /// Die Namen der am Zyklus beteiligten (nicht auflösbaren) Crates.
        crates: Vec<String>,
    },

    /// Weder `CARGO_HOME` noch `HOME` sind aus der Umgebung auflösbar.
    #[msg("weder CARGO_HOME noch HOME sind in der Umgebung verfügbar")]
    CargoHomeUnavailable,

    /// Für das angefragte Crate/Version-Paar existiert kein lokales
    /// Registry-Quellverzeichnis unter `$CARGO_HOME/registry/src`.
    #[msg("keine lokale Registry-Quelle für '{crate_name}' Version '{version}' gefunden")]
    RegistrySourceNotFound {
        /// Name des gesuchten Crates.
        crate_name: String,
        /// Gesuchte Version des Crates.
        version: String,
    },

    /// Ein Pfad ist ungültig — z. B. ein nicht unterstütztes Glob-Muster in
    /// `[workspace].members`, oder ein Ziel, das per `..`-Traversal den
    /// erlaubten Wurzelordner verlässt.
    #[msg("ungültiger Pfad: '{path}'")]
    InvalidPath {
        /// Der beanstandete Pfad bzw. das beanstandete Muster.
        path: String,
    },
}
