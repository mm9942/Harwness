//! Grammatik von `harw lens`: Wissensindex bauen und Status anzeigen.

use clap::Subcommand;

use super::values::LensSource;

/// Aktionen des `harw lens`-Subcommands.
#[derive(Debug, Subcommand)]
pub enum LensAction {
    /// Baut oder aktualisiert den/die Lens-Index/-Indizes für das aktive
    /// Profil.
    ///
    /// Baut genau die `(index, sichtbarkeit)`-Paare, die `lens.ask`
    /// tatsächlich befragt (`harw_tool_lens::scope::KNOWN_SELECTORS`):
    /// `docs.design` aus `docs/` im aktuellen Arbeitsverzeichnis sowie
    /// `knowledge.palace` aus dem Memory Palace des aktiven Profils. Der
    /// Bau ist inkrementell (nur neue/geänderte Chunks werden eingebettet,
    /// siehe `harw_lens_source::build_index`) — ein wiederholter Aufruf
    /// ohne `--force` ist deshalb günstig und der richtige Weg, einen
    /// **veralteten** Index (`harw lens status` zeigt sein Alter) nach
    /// Quelländerungen aufzufrischen.
    Build {
        /// Nur diese Quellmenge bauen: `docs` (Design-/Architekturdokumente
        /// aus `docs/`) oder `knowledge` (Memory-Palace-Artefakte des
        /// aktiven Profils). Ohne Angabe werden beide gebaut.
        #[arg(long, value_name = "NAME", value_enum)]
        source: Option<LensSource>,
        /// Bereits gebaute Indexdaten (samt Embedding-Cache) vor dem Bau
        /// verwerfen, statt inkrementell weiterzubauen — ein garantiert
        /// vollständiger Neuaufbau, z. B. zur Fehlersuche.
        #[arg(long)]
        force: bool,
    },
    /// Zeigt, welche von `lens.ask` befragten Indizes existieren: Modell,
    /// Dimension, Lokalität, Chunker-Fassung, Chunkanzahl und Alter — samt
    /// eines Hinweises, wenn ein Index fehlt oder sein Alter auf mögliche
    /// Veraltung gegenüber den Quellen hindeutet.
    Status,
}
