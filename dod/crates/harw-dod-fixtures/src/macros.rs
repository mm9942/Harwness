//! `sensor_suite!` — die Aufrufstelle, die alle Sensor-Crates wortgleich
//! (bis auf die optionale `empty_scope`-Angabe, siehe unten) benutzen.
//!
//! Enthält ausschließlich das Makro selbst; die gesamte Prüflogik steht in
//! [`crate::harness`] (siehe dortige Moduldoku für Details je Prüfung).

/// Erzeugt die Fixture-Testsuite für einen Sensor.
///
/// # Description
/// Expandiert zu einem privaten `mod`, das acht `#[test]`-Funktionen
/// enthält: die sieben Pflichtprüfungen aus der [`crate`]-Moduldoku
/// (Determinismus, Inhaltsfreiheit, Bereichsdichtheit, Verhalten bei leerer
/// Quelle, Redaktion, Kardinalität, Fehlerfall) sowie eine achte, optionale
/// Adversarial-Prüfung, die sich selbst überspringt, wenn
/// `fixtures/adversarial/` nicht existiert. Jede generierte Funktion gibt
/// `-> Result<(), $crate::error::HarnessViolation>` zurück und ist ein
/// dünner Aufruf der gleichnamigen `assert_*`-Funktion in
/// [`crate::harness`] als Tail-Expression — dort steht die eigentliche
/// Prüflogik, hier nur die Verdrahtung. `#[test]`-Funktionen akzeptieren
/// diese Rückgabeform genauso wie `()`: ein `Err` markiert den Test als
/// fehlgeschlagen und druckt den Fehler über `Debug`
/// ([`crate::error::HarnessViolation`] leitet `Debug` per
/// `#[derive(Debug)]` ab), ohne dass die geprüfte `assert_*`-Funktion selbst
/// paniken muss (Bible R087/R089).
///
/// # Bereichsdichtheit vs. leerer Bereich
/// Diese beiden Prüfungen waren bis Knoten K48 eine einzige Zusicherung
/// (siehe [`crate::harness`]-Moduldoku, Abschnitt „Bereichsdichtheit vs.
/// leerer Bereich"). Seither sind es zwei:
///
/// - **Bereichsdichtheit** ([`crate::harness::assert_scope_containment`])
///   läuft **immer**, für jeden Sensor, ohne Ausnahme und ohne Abschaltung —
///   sie ist die Sicherheitszusage des gesamten Teilbaums.
/// - **Verhalten bei leerer Quelle** ist die erklärte Eigenschaft eines
///   konkreten Sensors (siehe [`crate::harness::EmptyScopeExpectation`]) und
///   wird über das optionale Feld `empty_scope` **ausdrücklich** im
///   Testaufruf gesetzt — nie in einem stillen Vorgabewert. Wird es
///   weggelassen, gilt `EmptyScopeExpectation::SourceUnavailableIsError`
///   (das heutige, unveränderte Verhalten): keine der bereits gelandeten
///   Sensor-Crates muss deswegen eine Zeile ändern.
///
/// # Voraussetzung an `sensor`
/// Der als `sensor` benannte Typ muss
/// `harw_dod_signals::Sensor` sowie
/// `From<harw_dod_cap::SensorHandle<harw_dod_cap::Bound>>` implementieren.
/// Letzteres ist die einzige Konstruktionskonvention, die diese Crate allen
/// Sensor-Crates auferlegt — ein gebundener Griff hinein, eine Sensor-
/// Instanz heraus.
///
/// # Arguments (Makro-Felder, in dieser Reihenfolge)
/// - `sensor` (Typname): der zu prüfende Sensor-Typ.
/// - `capability` (Ausdruck vom Typ `harw_dod_cap::Capability`): die
///   Fähigkeit, mit der der Testgriff gebaut wird.
/// - `max_cardinality` (Ausdruck vom Typ `usize`): die vom Sensor-Autor
///   deklarierte Obergrenze verschiedener Labelkombinationen je Poll (siehe
///   [`crate::harness::assert_cardinality`] für die Zähldefinition und die
///   Begründung, warum dies ein einfacher `usize` und keine
///   `harw_observe::Cardinality` ist).
/// - `fixtures` (String-Literal): Pfad zum `fixtures/`-Verzeichnis, relativ
///   zur Crate-Wurzel (typischerweise `"fixtures"`).
/// - `empty_scope` (Ausdruck vom Typ
///   [`crate::harness::EmptyScopeExpectation`], **optional**): welche
///   Erwartung dieser Sensor an einen leeren `ReadScope` erklärt. Fehlt das
///   Feld, gilt `EmptyScopeExpectation::SourceUnavailableIsError`. Ein
///   Sensor, dessen Quelle legitim fehlen darf (z. B. `harw-dod-gpu`: keine
///   GPU im Host), setzt hier ausdrücklich
///   `EmptyScopeExpectation::EmptySourceIsNormal`.
///
/// Eine abschließende Komma ist erlaubt, aber nicht erforderlich.
///
/// # Returns
/// Kein Wert — das Makro erzeugt Item-Code (ein `mod` mit `#[test]`-
/// Funktionen), keinen Ausdruck. Jede generierte Funktion selbst gibt
/// `Result<(), $crate::error::HarnessViolation>` zurück (siehe oben).
///
/// # Errors
/// Kein `Result` des Makro-Aufrufs selbst. Eine verletzte Prüfung äußert
/// sich als `Err($crate::error::HarnessViolation)` aus der jeweiligen
/// generierten `#[test]`-Funktion, die `cargo test` als fehlgeschlagenen
/// Test meldet — nicht mehr als Panic (Bible R087/R089).
///
/// # Examples
/// ```rust,ignore
/// use harw_dod_cap::{Bound, Capability, SensorHandle};
/// use harw_dod_signals::{Sensor, SensorReading};
///
/// #[derive(Debug)]
/// struct ThermalSensor(SensorHandle<Bound>);
///
/// impl From<SensorHandle<Bound>> for ThermalSensor {
///     fn from(handle: SensorHandle<Bound>) -> Self {
///         Self(handle)
///     }
/// }
///
/// impl Sensor for ThermalSensor {
///     fn handle(&self) -> &SensorHandle<Bound> {
///         &self.0
///     }
///     fn poll(&self, now: jiff::Timestamp) -> Result<SensorReading, harw_dod_cap::SensorError> {
///         // … liest /sys/class/thermal über self.0.scope() …
///         # Ok(SensorReading::default())
///     }
/// }
///
/// #[cfg(test)]
/// mod tests {
///     use super::ThermalSensor;
///     use harw_dod_cap::Capability;
///
///     // `empty_scope` weggelassen: eine leere Quelle ist bei einem
///     // Thermalsensor ein Fehler (jeder Host hat Thermalzonen) — das ist
///     // der Vorgabewert.
///     harw_dod_fixtures::sensor_suite! {
///         sensor: ThermalSensor,
///         capability: Capability::ReadSysfsThermal,
///         max_cardinality: 16,
///         fixtures: "fixtures",
///     }
/// }
/// ```
///
/// Ein Sensor, dessen Quelle legitim fehlen darf, setzt `empty_scope`
/// ausdrücklich:
/// ```rust,ignore
/// use harw_dod_fixtures::harness::EmptyScopeExpectation;
///
/// harw_dod_fixtures::sensor_suite! {
///     sensor: GpuSensor,
///     capability: Capability::ReadSysfsDrm,
///     max_cardinality: MAX_CARDINALITY,
///     fixtures: "fixtures",
///     empty_scope: EmptyScopeExpectation::EmptySourceIsNormal,
/// }
/// ```
#[macro_export]
macro_rules! sensor_suite {
    (
        sensor: $sensor:ty,
        capability: $capability:expr,
        max_cardinality: $max_cardinality:expr,
        fixtures: $fixtures:literal,
        empty_scope: $empty_scope:expr $(,)?
    ) => {
        /// Generiert von `harw_dod_fixtures::sensor_suite!`. Siehe die
        /// Moduldokumentation von `harw_dod_fixtures` bzw.
        /// `harw_dod_fixtures::harness` für die Bedeutung jeder Prüfung.
        mod __harw_sensor_suite {
            #[allow(unused_imports)]
            use super::*;

            fn __fixtures_root() -> ::std::path::PathBuf {
                ::std::path::PathBuf::from(::core::concat!(
                    ::core::env!("CARGO_MANIFEST_DIR"),
                    "/",
                    $fixtures
                ))
            }

            fn __build(handle: ::harw_dod_cap::SensorHandle<::harw_dod_cap::Bound>) -> $sensor {
                <$sensor as ::core::convert::From<
                    ::harw_dod_cap::SensorHandle<::harw_dod_cap::Bound>,
                >>::from(handle)
            }

            #[test]
            fn test_sensor_suite_determinism()
            -> ::core::result::Result<(), $crate::error::HarnessViolation> {
                $crate::harness::assert_determinism(__build, $capability, &__fixtures_root())
            }

            #[test]
            fn test_sensor_suite_content_freedom()
            -> ::core::result::Result<(), $crate::error::HarnessViolation> {
                $crate::harness::assert_content_freedom(__build, $capability, &__fixtures_root())
            }

            #[test]
            fn test_sensor_suite_scope_containment()
            -> ::core::result::Result<(), $crate::error::HarnessViolation> {
                $crate::harness::assert_scope_containment(__build, $capability, &__fixtures_root())
            }

            #[test]
            fn test_sensor_suite_empty_scope_behavior()
            -> ::core::result::Result<(), $crate::error::HarnessViolation> {
                match $empty_scope {
                    $crate::harness::EmptyScopeExpectation::SourceUnavailableIsError => {
                        $crate::harness::assert_scope_tightness(__build, $capability)
                    }
                    $crate::harness::EmptyScopeExpectation::EmptySourceIsNormal => {
                        $crate::harness::assert_empty_scope_is_ok(__build, $capability)
                    }
                }
            }

            #[test]
            fn test_sensor_suite_redaction()
            -> ::core::result::Result<(), $crate::error::HarnessViolation> {
                $crate::harness::assert_redaction(__build, $capability, &__fixtures_root())
            }

            #[test]
            fn test_sensor_suite_cardinality()
            -> ::core::result::Result<(), $crate::error::HarnessViolation> {
                $crate::harness::assert_cardinality(
                    __build,
                    $capability,
                    &__fixtures_root(),
                    $max_cardinality,
                )
            }

            #[test]
            fn test_sensor_suite_error_case()
            -> ::core::result::Result<(), $crate::error::HarnessViolation> {
                $crate::harness::assert_error_case(__build, $capability, &__fixtures_root())
            }

            #[test]
            fn test_sensor_suite_adversarial_survives()
            -> ::core::result::Result<(), $crate::error::HarnessViolation> {
                $crate::harness::assert_adversarial_survives(
                    __build,
                    $capability,
                    &__fixtures_root(),
                )
            }
        }
    };

    (
        sensor: $sensor:ty,
        capability: $capability:expr,
        max_cardinality: $max_cardinality:expr,
        fixtures: $fixtures:literal $(,)?
    ) => {
        // Kein `empty_scope` angegeben: Vorgabewert ist das heutige, seit
        // K48 unveränderte Verhalten — eine leere Quelle ist ein Fehler.
        // Diese Weiterleitung ist der ganze Grund, warum keine der bereits
        // gelandeten Sensor-Crates eine Zeile ändern muss (siehe
        // `crate::sensor_suite!`-Moduldoku, Abschnitt „Bereichsdichtheit vs.
        // leerer Bereich").
        $crate::sensor_suite! {
            sensor: $sensor,
            capability: $capability,
            max_cardinality: $max_cardinality,
            fixtures: $fixtures,
            empty_scope: $crate::harness::EmptyScopeExpectation::SourceUnavailableIsError,
        }
    };
}
