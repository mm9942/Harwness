//! Fehlertyp von `harw-dod-fixtures` (Knoten AW2-05).
//!
//! # Verantwortungsbereich
//! Bündelt jeden Fehler, den [`crate::capture::capture`] und die internen
//! `expect.json`-Helfer ([`crate::fixture_io`]) erzeugen können. Die
//! generierten Prüfungen aus [`crate::sensor_suite!`] selbst geben **keinen**
//! `Result` zurück — ein Testfehler dort äußert sich als fehlgeschlagene
//! Assertion (Panic), nicht als `Err`-Wert, weil `#[test]`-Funktionen genau
//! das erwarten.
//!
//! # Abgrenzung zu `harw_dod_cap::SensorError`
//! Diese Crate ist kein Sensor und unterliegt deshalb **nicht** der
//! Inhaltsfreiheits-Pflicht von `SensorError` (Contract-Master Abschnitt F):
//! `FixturesError` ist ein reines Entwicklungswerkzeug-Fehlerbild für
//! [`crate::capture::capture`], das nie in den Pfad eines laufenden Sensors
//! gerät. Ein von einem Sensor beim Capture-Poll gelieferter
//! `harw_dod_cap::SensorError` wird trotzdem verlustfrei durchgereicht
//! ([`FixturesError::Sensor`]), damit `?` funktioniert.
//!
//! # Exportierte Typen
//! [`FixturesError`] sowie der vom `#[derive(harw_macros::HarwError)]`-Makro
//! erzeugte Alias `FixturesResult<T>`.
//!
//! # Nebenläufigkeit
//! Reine Daten ohne innere Veränderlichkeit: `Send + Sync`, sicher aus jedem
//! Thread erzeugbar und lesbar.
//!
//! # Examples
//! ```rust
//! use harw_dod_fixtures::error::FixturesError;
//!
//! fn describe(err: &FixturesError) -> String {
//!     err.to_string()
//! }
//! ```

/// Fehler dieser Crate.
///
/// # Description
/// Drei durchgereichte Fehlerquellen von [`crate::capture::capture`]: Datei-
/// system-I/O beim Kopieren des Fixture-Baums, JSON-Kodierung von
/// `expect.json` und ein vom capturierten Sensor selbst gemeldeter
/// [`harw_dod_cap::SensorError`]. Jede Variante ist ein `#[from]`-Ein-Feld-
/// Tupel, damit `?` in [`crate::capture`] und [`crate::fixture_io`]
/// funktioniert, ohne dass diese Crate die drei Fehlerklassen dupliziert.
///
/// # Arguments
/// Nicht zutreffend — kein Konstruktor außer den `From`-Impls der einzelnen
/// Varianten (siehe `# Errors`).
///
/// # Returns
/// Nicht zutreffend — dies ist ein Fehlertyp, keine Funktion.
///
/// # Errors
/// - [`Self::Io`]: ein Lese- oder Schreibvorgang auf dem Fixture-Baum oder
///   auf `expect.json` ist am Dateisystem gescheitert.
/// - [`Self::Json`]: `expect.json` ließ sich nicht serialisieren oder
///   deserialisieren.
/// - [`Self::Sensor`]: der zu capturierende Sensor selbst hat beim
///   Probe-Poll während [`crate::capture::capture`] einen
///   [`harw_dod_cap::SensorError`] gemeldet.
///
/// # Examples
/// ```rust
/// use harw_dod_fixtures::error::FixturesError;
///
/// let err: FixturesError = harw_dod_cap::SensorError::MalformedSource.into();
/// assert!(matches!(err, FixturesError::Sensor(_)));
/// ```
#[derive(Debug, harw_macros::HarwError)]
pub enum FixturesError {
    /// Ein Lese- oder Schreibvorgang auf dem Fixture-Baum oder auf
    /// `expect.json` ist am Dateisystem gescheitert.
    #[msg("fixture I/O failed: {0}")]
    #[from]
    Io(std::io::Error),

    /// `expect.json` ließ sich nicht serialisieren oder deserialisieren.
    #[msg("fixture JSON encoding failed: {0}")]
    #[from]
    Json(serde_json::Error),

    /// Der zu capturierende Sensor hat beim Probe-Poll während
    /// [`crate::capture::capture`] einen Fehler gemeldet.
    #[msg("sensor poll during capture failed: {0}")]
    #[from]
    Sensor(harw_dod_cap::SensorError),
}

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::{FixturesError, FixturesResult};

    #[test]
    fn test_io_variant_display_contains_prefix() {
        let source = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let err = FixturesError::from(source);
        assert!(err.to_string().starts_with("fixture I/O failed:"));
    }

    #[test]
    fn test_json_variant_source_is_inner_error() {
        let source = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let err = FixturesError::from(source);
        assert!(err.source().is_some());
    }

    #[test]
    fn test_sensor_variant_wraps_sensor_error() {
        let err = FixturesError::from(harw_dod_cap::SensorError::MalformedSource);
        assert!(matches!(
            err,
            FixturesError::Sensor(harw_dod_cap::SensorError::MalformedSource)
        ));
    }

    #[test]
    fn test_fixtures_result_alias_carries_fixtures_error() {
        fn always_fails() -> FixturesResult<()> {
            Err(FixturesError::from(harw_dod_cap::SensorError::SourceUnavailable))
        }

        assert!(always_fails().is_err());
    }
}
