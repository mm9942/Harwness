//! Fehlertyp von `harw-dod-fixtures` (Knoten AW2-05).
//!
//! # Verantwortungsbereich
//! Bündelt jeden Fehler, den [`crate::capture::capture`] und die internen
//! `expect.json`-Helfer ([`crate::fixture_io`]) erzeugen können
//! ([`FixturesError`]), sowie — getrennt, weil semantisch etwas anderes —
//! jede verletzte Prüfung der acht `assert_*`-Funktionen aus [`crate::harness`]
//! ([`HarnessViolation`], Bible R087/R089: Prüf-Helfer dürfen nicht paniken).
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
//! # Abgrenzung `FixturesError` vs. `HarnessViolation`
//! [`FixturesError`] entsteht beim **Schreiben** eines Fixture-Baums
//! ([`crate::capture::capture`], [`crate::fixture_io`]). [`HarnessViolation`]
//! entsteht dagegen beim **Prüfen** eines bestehenden Fixture-Baums gegen
//! einen Sensor: jede der acht Prüfungen in [`crate::harness`] gibt bei einer
//! Verletzung ein `Err(HarnessViolation)` zurück statt zu paniken, mit vollem
//! Kontext (Fall-Pfad, [`harw_dod_cap::Capability`], Grund, ggf. gerenderter
//! Quellfehler).
//!
//! # Exportierte Typen
//! [`FixturesError`] sowie der vom `#[derive(harw_macros::HarwError)]`-Makro
//! erzeugte Alias `FixturesResult<T>`; [`HarnessViolation`] sowie der von
//! Hand geschriebene Alias [`HarnessResult<T>`] (siehe dort, warum das Makro
//! diesen Alias hier nicht automatisch erzeugt).
//!
//! # Nebenläufigkeit
//! Reine Daten ohne innere Veränderlichkeit: `Send + Sync`, sicher aus jedem
//! Thread erzeugbar und lesbar.
//!
//! # Examples
//! ```rust
//! use harw_dod_fixtures::error::{FixturesError, HarnessViolation};
//!
//! fn describe(err: &FixturesError) -> String {
//!     err.to_string()
//! }
//!
//! fn describe_violation(err: &HarnessViolation) -> String {
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

/// Verstoß gegen eine der acht Fixture-Prüfungen aus [`crate::harness`].
///
/// # Description
/// Jede `assert_*`-Funktion in [`crate::harness`] gibt bei einer verletzten
/// Prüfung ein `Err(HarnessViolation)` zurück, statt zu paniken (Bible
/// R087/R089: Prüf-Helfer dürfen nicht paniken). Zwei Varianten trennen zwei
/// verschiedene Ursachen: [`Self::Check`] für eine tatsächlich festgestellte
/// Verletzung (der geprüfte Sensor hat sich falsch verhalten — kein
/// Fremdfehler beteiligt), [`Self::Setup`] für einen internen Fehler der
/// Prüfung selbst (z. B. ein temporäres Verzeichnis ließ sich nicht anlegen,
/// `expect.json` war unlesbar, ein Poll ist mit einem
/// [`harw_dod_cap::SensorError`] gescheitert). Beide tragen den vollen
/// Kontext: den Fall-Pfad (als `String`, da ein `&Path` hier keine eigene
/// Lebensdauer über den Rückgabewert hinaus tragen könnte — bei den
/// Prüfungen ohne Fixture-Bezug, `assert_scope_tightness` und
/// `assert_empty_scope_is_ok`, ist es der Pfad des synthetischen leeren
/// Testverzeichnisses), die geprüfte [`harw_dod_cap::Capability`] und einen
/// menschenlesbaren Grund; [`Self::Setup`] zusätzlich den gerenderten
/// Quellfehler.
///
/// Anders als [`FixturesError`] unterliegt `HarnessViolation` **nicht** der
/// Inhaltsfreiheits-Pflicht von `harw_dod_cap::SensorError` — der Grund darf
/// beliebigen Diagnosetext tragen, denn er entsteht ausschließlich in Tests
/// und Diagnosewerkzeugen, nie im laufenden Sensorpfad.
///
/// # Arguments
/// Nicht zutreffend — kein Konstruktor außer den Varianten selbst, die jede
/// `assert_*`-Funktion in [`crate::harness`] an ihrer jeweiligen
/// Verletzungsstelle baut.
///
/// # Returns
/// Nicht zutreffend — dies ist ein Fehlertyp, keine Funktion.
///
/// # Errors
/// Nicht zutreffend — dies ist der Fehlertyp selbst.
///
/// # Examples
/// ```rust
/// use harw_dod_cap::Capability;
/// use harw_dod_fixtures::error::HarnessViolation;
///
/// let err = HarnessViolation::Check {
///     case: "fixtures/typical".to_owned(),
///     capability: Capability::ReadProcStat,
///     reason: "SensorReading weicht von expect.json ab".to_owned(),
/// };
/// assert!(err.to_string().contains("fixtures/typical"));
/// ```
#[derive(Debug, harw_macros::HarwError)]
pub enum HarnessViolation {
    /// Der geprüfte Sensor hat eine der acht Fixture-Prüfungen tatsächlich
    /// verletzt — kein Fremdfehler beteiligt.
    #[msg("Fall '{case}' ({capability:?}): {reason}")]
    Check {
        /// Pfad des geprüften Fixture-Falls (bzw. des synthetischen
        /// temporären Verzeichnisses bei den Prüfungen ohne Fixture-Bezug).
        case: String,
        /// Die geprüfte [`harw_dod_cap::Capability`].
        capability: harw_dod_cap::Capability,
        /// Menschenlesbare Beschreibung der Verletzung.
        reason: String,
    },

    /// Ein interner Vorbereitungs- oder Lesevorgang der Prüfung selbst ist
    /// gescheitert (z. B. Fixture-Baum kopieren, `expect.json` lesen, Sensor
    /// meldet einen [`harw_dod_cap::SensorError`] beim Poll).
    ///
    /// Kein `#[from]`: `#[derive(harw_macros::HarwError)]` erlaubt `#[from]`
    /// nur auf einfeldrigen Tupel-Varianten, diese Variante trägt aber
    /// zusätzlich `case`, `capability` und `reason` — der Quellfehler wird
    /// deshalb gerendert (`to_string()`) statt als eigener Typ eingebettet.
    #[msg("Fall '{case}' ({capability:?}): {reason}: {source}")]
    Setup {
        /// Pfad des geprüften Fixture-Falls (bzw. des synthetischen
        /// temporären Verzeichnisses).
        case: String,
        /// Die geprüfte [`harw_dod_cap::Capability`].
        capability: harw_dod_cap::Capability,
        /// Was versucht wurde, als der Fremdfehler auftrat.
        reason: String,
        /// Gerenderter Quellfehler (`to_string()`).
        source: String,
    },
}

/// Ergebnis einer der acht Fixture-Prüfungen aus [`crate::harness`].
///
/// # Description
/// Von Hand geschrieben, weil `HarnessViolation` nicht auf `Error` endet und
/// `#[derive(harw_macros::HarwError)]` den `<Prefix>Result<T>`-Alias nur für
/// Enum-Namen erzeugt, die das tun (siehe die Doku von
/// `#[proc_macro_derive(HarwError, ...)]` in `harw-macros`). Der
/// Standard-Typparameter `T = ()` deckt den weit überwiegenden Fall ab, in
/// dem eine `assert_*`-Funktion außer Erfolg/Misserfolg nichts zurückgibt.
pub type HarnessResult<T = ()> = Result<T, HarnessViolation>;

#[cfg(test)]
mod tests {
    use std::error::Error as _;

    use super::{FixturesError, FixturesResult, HarnessResult, HarnessViolation};
    use crate::test_support::{TestError, TestResult};

    #[test]
    fn test_io_variant_display_contains_prefix() {
        let source = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let err = FixturesError::from(source);
        assert!(err.to_string().starts_with("fixture I/O failed:"));
    }

    #[test]
    fn test_json_variant_source_is_inner_error() -> TestResult {
        let Err(source) = serde_json::from_str::<serde_json::Value>("not json") else {
            return Err(TestError::Unexpected("Err erwartet".into()));
        };
        let err = FixturesError::from(source);
        assert!(err.source().is_some());
        Ok(())
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
            Err(FixturesError::from(
                harw_dod_cap::SensorError::SourceUnavailable,
            ))
        }

        assert!(always_fails().is_err());
    }

    #[test]
    fn test_harness_violation_check_display_contains_case_capability_and_reason() {
        let err = HarnessViolation::Check {
            case: "fixtures/typical".to_owned(),
            capability: harw_dod_cap::Capability::ReadProcStat,
            reason: "Determinismus verletzt".to_owned(),
        };
        let rendered = err.to_string();
        assert!(rendered.contains("fixtures/typical"));
        assert!(rendered.contains("ReadProcStat"));
        assert!(rendered.contains("Determinismus verletzt"));
    }

    #[test]
    fn test_harness_violation_check_source_is_none() {
        let err = HarnessViolation::Check {
            case: "x".to_owned(),
            capability: harw_dod_cap::Capability::ReadProcStat,
            reason: "y".to_owned(),
        };
        assert!(err.source().is_none());
    }

    #[test]
    fn test_harness_violation_setup_display_contains_source() -> TestResult {
        let err = HarnessViolation::Setup {
            case: "fixtures/typical".to_owned(),
            capability: harw_dod_cap::Capability::ReadProcStat,
            reason: "expect.json unlesbar".to_owned(),
            source: "kaputte Datei".to_owned(),
        };
        let rendered = err.to_string();
        if !rendered.contains("kaputte Datei") {
            return Err(TestError::Unexpected(format!(
                "Setup-Meldung enthält den Quellfehler nicht: {rendered}"
            )));
        }
        Ok(())
    }

    #[test]
    fn test_harness_violation_setup_source_is_none() {
        // `source` ist hier ein String-Feld, kein #[from]: das Makro
        // verlinkt std::error::Error::source() nur bei #[from]-Varianten.
        let err = HarnessViolation::Setup {
            case: "x".to_owned(),
            capability: harw_dod_cap::Capability::ReadProcStat,
            reason: "y".to_owned(),
            source: "z".to_owned(),
        };
        assert!(err.source().is_none());
    }

    #[test]
    fn test_harness_result_alias_carries_harness_violation() {
        fn always_fails() -> HarnessResult {
            Err(HarnessViolation::Check {
                case: "x".to_owned(),
                capability: harw_dod_cap::Capability::ReadProcStat,
                reason: "y".to_owned(),
            })
        }

        assert!(always_fails().is_err());
    }
}
