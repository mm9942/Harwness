//! `SourceFault`: warum eine einzelne Quelle übersprungen wurde.
//!
//! # Zweck
//! Ein Abruf liest viele Quellen: jedes konfigurierte `Cargo.lock` und je
//! gesperrter Crate mehrere RustSec-Dateien. Eine kaputte Quelle darf den
//! Abruf nicht blenden (PL-100 WP0.15, Vorbild `harw-dod-scanreport`): sie
//! wird übersprungen und gezählt. [`SourceFault`] benennt den Grund, damit
//! die Tests den richtigen Pfad prüfen können, statt nur „irgendein Fehler“.
//!
//! # Verantwortungsbereich
//! Besitzt ausschließlich [`SourceFault`]. Der Typ verlässt diese Crate
//! nie: [`crate::sensor::AdvisorySensor`] zählt ihn in einem `HostSample`
//! und meldet ein `SensorDegraded`, der Aufrufer sieht nur
//! [`harw_dod_cap::SensorError`].
//!
//! # Inhaltsfreiheit
//! Keine Variante trägt Daten. Weder ein Pfad noch ein Dateiinhalt, ein
//! Crate-Name oder eine Versionsangabe erscheint in einer Meldung: was
//! geloggt wird, verlässt den Host.
//!
//! # Nebenläufigkeit
//! `Copy`-Wert ohne innere Veränderlichkeit; `Send + Sync`.
//!
//! # Fehler
//! Der Typ ist selbst der Fehler; er hat keine eigenen Fehlerfälle.
//!
//! # Examples
//! ```rust,ignore
//! // Crate-intern: eine Datei ohne Front-Matter.
//! let parsed = crate::advisory::parse_advisory("# Titel", AdvisoryFormat::Markdown);
//! assert!(matches!(parsed, Err(SourceFault::FrontMatter)));
//! ```

use harw_macros::HarwError;

/// Grund, aus dem eine einzelne Quelle (Lockfile oder Advisory-Datei)
/// übersprungen wurde.
///
/// # Description
/// Feldlos und inhaltsfrei, siehe Moduldoku. Die ersten fünf Varianten
/// betreffen das Lesen ([`crate::read::read_regular_file`]), die übrigen
/// die Deutung des gelesenen Inhalts.
///
/// # Concurrency
/// `Copy`, `Send + Sync`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, HarwError)]
pub(crate) enum SourceFault {
    /// Der aufgelöste Pfad liegt außerhalb des Lesebereichs (etwa ein
    /// Symlink nach außen).
    #[msg("source path resolves outside the sensor read scope")]
    OutsideScope,
    /// Die Quelle ließ sich nicht auflösen, öffnen oder lesen (fehlt, keine
    /// Rechte, E/A-Fehler).
    #[msg("source could not be opened or read")]
    Unreadable,
    /// Die Quelle ist keine reguläre Datei (Verzeichnis, FIFO, Gerät).
    #[msg("source is not a regular file")]
    NotRegularFile,
    /// Die Quelle überschreitet `harw_dod_readfs::MAX_READ_BYTES`.
    #[msg("source exceeds the size limit")]
    TooLarge,
    /// Die Quelle ist kein gültiges UTF-8.
    #[msg("source is not valid UTF-8")]
    NotUtf8,
    /// Das Lockfile ist kein TOML der erwarteten Form.
    #[msg("lockfile has an unexpected shape")]
    LockfileShape,
    /// Die Markdown-Datei beginnt nicht mit einem geschlossenen
    /// TOML-Front-Matter-Block.
    #[msg("advisory has no TOML front matter")]
    FrontMatter,
    /// Die Advisory hat keinen (nicht leeren) Titel.
    #[msg("advisory has no title")]
    Title,
    /// Das Advisory-TOML hat nicht die erwartete Form (Syntax, fehlende
    /// Pflichtfelder, fehlender `[versions]`-Abschnitt, Titel in der
    /// Front-Matter einer Markdown-Datei).
    #[msg("advisory has an unexpected shape")]
    AdvisoryShape,
    /// Die Advisory-ID ist keine schlichte Kennung (`[A-Za-z0-9-]`, höchstens
    /// [`crate::advisory::MAX_ID_LEN`] Zeichen).
    #[msg("advisory id is not a plain identifier")]
    AdvisoryId,
    /// Die Advisory nennt ein anderes Paket als das Verzeichnis, in dem sie
    /// liegt.
    #[msg("advisory names a different package")]
    PackageMismatch,
    /// Eine Versionsangabe aus `[versions]` ist nicht lesbar oder wird nicht
    /// unterstützt (Wildcard, mehr als zwei Vergleiche, leerer Bereich).
    #[msg("advisory version range is not supported")]
    VersionRange,
}

#[cfg(test)]
mod tests {
    use super::SourceFault;

    #[test]
    fn test_messages_are_content_free_and_distinct() {
        let all = [
            SourceFault::OutsideScope,
            SourceFault::Unreadable,
            SourceFault::NotRegularFile,
            SourceFault::TooLarge,
            SourceFault::NotUtf8,
            SourceFault::LockfileShape,
            SourceFault::FrontMatter,
            SourceFault::Title,
            SourceFault::AdvisoryShape,
            SourceFault::AdvisoryId,
            SourceFault::PackageMismatch,
            SourceFault::VersionRange,
        ];
        let messages: std::collections::BTreeSet<String> =
            all.iter().map(ToString::to_string).collect();
        assert_eq!(messages.len(), all.len());
        for message in &messages {
            assert!(message.is_ascii());
            assert!(!message.contains('/'));
        }
    }
}
