//! Fehlertyp für `harw-dod-readfs`.
//!
//! # Zweck
//! Bündelt jeden Fehler, den ein getypter Lesezugriff ([`crate::read`]) oder
//! ein Glob-Abgleich ([`crate::glob`]) erzeugen kann, in einer einzigen
//! Fehlermenge.
//!
//! # Verantwortungsbereich
//! Besitzt ausschließlich [`ReadFsError`]. Bereichs- und Symlink-Fehler
//! entstehen bereits in [`harw_dod_cap::SensorError`] (dort mit den
//! Varianten `OutsideScope`, `SourceUnavailable`, `MalformedSource`, `Io`)
//! und werden hier nur durchgereicht ([`ReadFsError::Scope`]); diese Crate
//! erfindet dafür keine eigenen Varianten — dieselbe Fehlerklasse soll nicht
//! zweimal unter unterschiedlichem Namen existieren.
//!
//! # Exportierte Typen
//! [`ReadFsError`] sowie `ReadFsResult<T>`, der vom
//! `#[derive(harw_macros::HarwError)]`-Makro erzeugte Alias für
//! `Result<T, ReadFsError>`.
//!
//! # Nebenläufigkeit
//! Reine Daten ohne innere Veränderlichkeit: `Send + Sync`, sicher aus jedem
//! Thread erzeugbar und lesbar.
//!
//! # Fehler
//! [`ReadFsError`] selbst — siehe die Dokumentation der einzelnen Varianten.
//!
//! # Inhaltsfreiheit
//! Keine Variante nennt einen aufgelösten Dateisystempfad, einen
//! Dateiinhalt oder eine gelesene Zeile. Die einzige Ausnahme:
//! [`ReadFsError::GlobPatternAbsolute`],
//! [`ReadFsError::GlobPatternTraversal`] und
//! [`ReadFsError::GlobLimitExceeded`] nennen das Glob-Muster — das ist
//! kein aufgelöster Pfad, sondern ein Literal, das der Aufrufer selbst im
//! eigenen Quellcode getippt hat, bevor irgendein Dateisystemzugriff
//! stattfand. Es verrät nichts über den Host, das der Aufrufer nicht schon
//! wüsste.
//!
//! # Examples
//! ```rust
//! use harw_dod_readfs::ReadFsError;
//!
//! let err = ReadFsError::TooLarge { limit: 1_048_576 };
//! assert!(err.to_string().contains("1048576"));
//! ```

use harw_macros::HarwError;

/// Fehler eines getypten Lesezugriffs oder Glob-Abgleichs unter einem
/// `ReadScope`.
///
/// # Description
/// Jede Variante trägt genug Kontext für die Diagnose, ohne den Inhalt der
/// betroffenen Quelle preiszugeben. [`Self::Scope`] reicht die
/// bereichsseitigen Fehler aus `harw-dod-cap` unverändert durch, damit `?`
/// in [`crate::read`] und [`crate::glob`] funktioniert, ohne dass diese
/// Crate deren Fehlerklassen dupliziert.
///
/// # Concurrency
/// `Send + Sync`; reine Daten ohne innere Veränderlichkeit.
#[derive(Debug, HarwError)]
pub enum ReadFsError {
    /// Der Bereich hat den Zugriff verweigert, die Quelle ist auf diesem
    /// Host nicht vorhanden, ihr Inhalt hat eine unerwartete Form, oder das
    /// Lesen ist am Dateisystem gescheitert.
    ///
    /// Deckt insbesondere den Fall ab, dass ein geparster oder gelesener
    /// Inhalt nicht der erwarteten Form entspricht ([`parse_i64`] /
    /// [`parse_u64`] konstruieren dafür ausdrücklich
    /// [`harw_dod_cap::SensorError::MalformedSource`]) — **nie** erscheint
    /// dabei der nicht parsbare Inhalt selbst in der Meldung.
    ///
    /// [`parse_i64`]: crate::read::parse_i64
    /// [`parse_u64`]: crate::read::parse_u64
    #[from]
    #[msg("Lesezugriff verweigert: {0}")]
    Scope(harw_dod_cap::SensorError),

    /// Der Inhalt überschreitet [`crate::read::MAX_READ_BYTES`]. Ein Sensor,
    /// der versehentlich auf eine beliebig große Datei zeigt, füllt damit
    /// nicht den Speicher.
    #[msg("Inhalt überschreitet die Größengrenze von {limit} Bytes")]
    TooLarge {
        /// Die durchgesetzte Grenze in Bytes. Die tatsächliche Dateigröße
        /// wird absichtlich nicht ermittelt — das würde die überlange Datei
        /// ja gerade vollständig einlesen, was diese Variante verhindern
        /// soll.
        limit: u64,
    },

    /// Das Glob-Muster begann mit `/`. Nur bereichsrelative Muster sind
    /// erlaubt: ein absolutes Muster wäre ein Weg, unabhängig vom Bereich zu
    /// formulieren, was gelistet werden soll, bevor überhaupt ein Treffer
    /// geprüft wird.
    #[msg("Glob-Muster '{pattern}' ist absolut; erlaubt sind nur bereichsrelative Muster")]
    GlobPatternAbsolute {
        /// Das abgelehnte Muster, unverändert wie übergeben. Ein vom
        /// Aufrufer selbst getipptes Literal, kein aufgelöster
        /// Dateisystempfad (siehe Modul-Dokumentation, Abschnitt
        /// „Inhaltsfreiheit").
        pattern: String,
    },

    /// Das Glob-Muster enthielt eine `..`-Komponente. Ohne diese Ablehnung
    /// könnte ein Muster über ein Elternverzeichnis aus dem Bereich heraus
    /// zeigen, noch bevor die abschließende `allows`-Prüfung greift.
    #[msg("Glob-Muster '{pattern}' enthält '..' und könnte den Bereich verlassen")]
    GlobPatternTraversal {
        /// Das abgelehnte Muster, unverändert wie übergeben. Ein vom
        /// Aufrufer selbst getipptes Literal, kein aufgelöster
        /// Dateisystempfad (siehe Modul-Dokumentation, Abschnitt
        /// „Inhaltsfreiheit").
        pattern: String,
    },

    /// Ein Glob-Abgleich hat eine seiner Grenzen überschritten
    /// ([`crate::glob::MAX_GLOB_COMPONENTS`] für die Musterlänge,
    /// [`crate::glob::MAX_GLOB_CANDIDATES`] für gleichzeitig verfolgte
    /// Kandidaten). Schützt vor unbegrenzter Arbeit über sysfs-Schleifen
    /// (`…/subsystem/…`) oder riesige Verzeichnisse.
    #[msg("Glob-Muster '{pattern}' überschreitet die Grenze für {limit_name} ({limit})")]
    GlobLimitExceeded {
        /// Das Muster, unverändert wie übergeben (vom Aufrufer getipptes
        /// Literal, siehe Modul-Dokumentation, Abschnitt „Inhaltsfreiheit").
        pattern: String,
        /// Welche Grenze griff: `"components"` oder `"candidates"`.
        limit_name: &'static str,
        /// Der Wert der überschrittenen Grenze. Die tatsächliche Anzahl wird
        /// bewusst nicht genannt: sie würde die Größe eines Host-Verzeichnisses
        /// preisgeben.
        limit: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Die Größengrenze steht in der Meldung, damit der Aufrufer versteht,
    /// woran der Aufruf gescheitert ist.
    #[test]
    fn test_too_large_message_contains_limit() {
        let err = ReadFsError::TooLarge { limit: 1_048_576 };
        let message = err.to_string();
        assert!(message.contains("1048576"), "unerwartet: {message}");
    }

    /// Ein absolutes Muster ist ein vom Aufrufer getipptes Literal und darf
    /// deshalb in der Meldung stehen (siehe Modul-Dokumentation).
    #[test]
    fn test_glob_pattern_absolute_message_contains_pattern() {
        let err = ReadFsError::GlobPatternAbsolute {
            pattern: "/etc/passwd".to_owned(),
        };
        let message = err.to_string();
        assert!(message.contains("/etc/passwd"), "unerwartet: {message}");
    }

    /// Ein Muster mit `..` ist ebenfalls ein vom Aufrufer getipptes Literal
    /// und darf deshalb in der Meldung stehen.
    #[test]
    fn test_glob_pattern_traversal_message_contains_pattern() {
        let err = ReadFsError::GlobPatternTraversal {
            pattern: "sub/../etc/passwd".to_owned(),
        };
        let message = err.to_string();
        assert!(
            message.contains("sub/../etc/passwd"),
            "unerwartet: {message}"
        );
    }

    /// Die Grenzverletzung nennt Muster, Grenzname und Grenzwert.
    #[test]
    fn test_glob_limit_exceeded_message_contains_pattern_and_limit() {
        let err = ReadFsError::GlobLimitExceeded {
            pattern: "sys/block/*/stat".to_owned(),
            limit_name: "candidates",
            limit: 4096,
        };
        assert_eq!(
            err.to_string(),
            "Glob-Muster 'sys/block/*/stat' überschreitet die Grenze für candidates (4096)"
        );
    }

    /// `#[from]` erzeugt die Konvertierung, die `?` in `crate::read` und
    /// `crate::glob` braucht, und `Scope` bleibt beim jeweils konkreten
    /// `SensorError` unterscheidbar.
    #[test]
    fn test_from_sensor_error_maps_to_scope_variant() {
        let err = ReadFsError::from(harw_dod_cap::SensorError::OutsideScope);
        assert!(matches!(
            err,
            ReadFsError::Scope(harw_dod_cap::SensorError::OutsideScope)
        ));
    }

    /// `source()` verweist auf den gekapselten `SensorError`, damit eine
    /// Fehlerkette (`std::error::Error::source`) nicht bei `ReadFsError`
    /// abreißt.
    #[test]
    fn test_scope_variant_source_is_inner_sensor_error() {
        use std::error::Error as _;

        let err = ReadFsError::from(harw_dod_cap::SensorError::SourceUnavailable);
        assert!(err.source().is_some(), "source() muss die Ursache liefern");
    }
}
