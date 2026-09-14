//! Fixture-Harness `sensor_suite!` für alle Sensoren (Knoten AW2-05).
//!
//! # Zweck
//! Vertragsknoten, nicht Arbeitsknoten: **elf Sensor-Crates** werden gleich
//! parallel gebaut, und jede erbt ihre Prüfungen wortgleich von dieser
//! Crate. Was hier festgelegt wird, wiederholt sich elfmal — eine Prüfung,
//! die hier fehlt, fehlt elfmal; eine, die hier falsch geschnitten ist, ist
//! elfmal falsch. Diese Crate besitzt zwei Dinge: das Makro
//! [`sensor_suite!`], das aus einer Sensor-Implementierung und einem
//! `fixtures/`-Verzeichnis acht `#[test]`-Funktionen erzeugt, und
//! [`capture::capture`], das ein `fixtures/`-Verzeichnis aus einem laufenden
//! System **schreibt**, damit niemand eine Fixture von Hand tippen muss.
//!
//! Der Fixture-Baum ist ein **echter Verzeichnisbaum**, kein Mock: ein
//! Sensor liest ihn über einen [`harw_dod_cap::ReadScope`], der auf ihn
//! zeigt — damit prüft jeder Test denselben Pfad, den der Betrieb nimmt,
//! inklusive [`harw_dod_cap::ReadScope::open`] und seiner
//! Symlink-Auflösung. Ein Mock am `ReadScope` vorbei prüfte den Sensor, nicht
//! die Naht zwischen Sensor und Dateisystem.
//!
//! # Die sieben Prüfungen
//! [`sensor_suite!`] erzeugt sieben Pflichttests plus eine achte, optionale
//! Adversarial-Prüfung. Die eigentliche Logik steht in [`harness`]; hier nur
//! die Übersicht mit Begründung.
//!
//! 1. **Determinismus** ([`harness::assert_determinism`]) — derselbe
//!    Fixture-Baum zweimal gepollt, mit demselben injizierten `now`, ergibt
//!    exakt dasselbe `SensorReading`, und dieses stimmt mit dem in
//!    `expect.json` hinterlegten Ergebnis überein. Die wichtigste der
//!    sieben: ohne sie ist kein Fixture-Vergleich aussagekräftig, und ein
//!    Sensor, der die Systemuhr selbst liest, fällt genau hier auf.
//! 2. **Inhaltsfreiheit** ([`harness::assert_content_freedom`]) — kein Feld
//!    eines emittierten `HostSample` oder `SecurityEvent` enthält den
//!    Rohtext der Quelle. Geprüft, indem eine erkennbare Markierung
//!    ([`harness::CONTENT_CANARY`]) in eine Arbeitskopie des Fixture-Baums
//!    geschrieben und im serialisierten Poll-Ergebnis danach gesucht wird.
//! 3. **Bereichsdichtheit** ([`harness::assert_scope_containment`]) — läuft
//!    **unbedingt, für jeden Sensor**: ein `ReadScope`, der auf ein echt
//!    leeres Unterverzeichnis zeigt, das im selben Baum liegt wie die
//!    echten Quelldateien eines Falls, darf diese Quelldateien nicht
//!    hergeben. Das ist die eigentliche Sicherheitszusage — *liest der
//!    Sensor etwas außerhalb seines `ReadScope`?* — und **keine**
//!    Abschaltung kennt (siehe Punkt 4 für die davon getrennte Frage, was
//!    eine leere Quelle *bedeutet*).
//! 4. **Verhalten bei leerer Quelle** ([`harness::assert_scope_tightness`]
//!    bzw. [`harness::assert_empty_scope_is_ok`], je nach
//!    [`harness::EmptyScopeExpectation`]) — eine vom Sensor-Autor
//!    **ausdrücklich im Testaufruf erklärte** Eigenschaft, keine universelle
//!    Wahrheit: `harw-dod-thermal` und `harw-dod-cpu` melden eine leere
//!    Quelle zu Recht als `Err(SourceUnavailable)` (jeder Host hat
//!    Thermalzonen bzw. `/proc/stat`); `harw-dod-gpu` muss dagegen
//!    `Ok(SensorReading::default())` melden, weil die meisten Hosts keine
//!    GPU haben und das der gesunde Normalfall ist, kein Fehler (Knoten
//!    K48). Beide Ausprägungen prüfen tatsächlich etwas — auch die
//!    Ok-Ausprägung lässt weder einen Fehler noch erfundene Werte
//!    durchgehen.
//! 5. **Redaktion** ([`harness::assert_redaction`]) — jedes Feld, das ein
//!    Pfad oder ein Name sein könnte, erscheint nur in freigegebener Form:
//!    der rohe, absolute Host-Pfad des Fixture-Baums selbst darf im
//!    Poll-Ergebnis nicht auftauchen.
//! 6. **Kardinalität** ([`harness::assert_cardinality`]) — die Anzahl
//!    verschiedener Labelkombinationen (verschiedene `metric`-Namen plus
//!    verschiedene `EventKind`-Varianten) bleibt unter der vom Sensor-Autor
//!    deklarierten Obergrenze. Ein Sensor, der pro Gerät ein eigenes Label
//!    erzeugt, sprengt sonst die Zeitreihendatenbank — und man merkt es erst
//!    im Betrieb.
//! 7. **Fehlerfall** ([`harness::assert_error_case`]) — jeder Fall unter
//!    `fixtures/malformed/` ergibt `MalformedSource` (nicht `Io`, nicht
//!    einen Panic), und die Fehlermeldung enthält den Dateiinhalt nicht.
//!
//! Zusätzlich, optional: **Adversarial** ([`harness::assert_adversarial_survives`])
//! — existiert `fixtures/adversarial/`, wird jeder Fall darin gepollt und
//! darf unter keinen Umständen panieren.
//!
//! # Die Verzeichniskonvention
//! ```text
//! harw-dod-<sensor>/fixtures/
//!   <fall-name>/
//!     tree/            # der nachgebildete Ausschnitt des Dateisystems
//!     expect.json      # das erwartete SensorReading (siehe fixture_io)
//!   malformed/
//!     <fall-name>/     # ein eigenständiger Baum, wie tree/ oben —
//!                      # kein expect.json, das Ergebnis ist immer ein Fehler
//!   adversarial/       # optional; gleiche Form wie malformed/
//!     <fall-name>/
//! ```
//! Jedes direkte Unterverzeichnis von `fixtures/` außer `malformed` und
//! `adversarial` ist ein **normaler Fall**: er braucht `tree/` und
//! `expect.json`. Jedes direkte Unterverzeichnis von `fixtures/malformed/`
//! bzw. `fixtures/adversarial/` ist selbst schon ein Baum — es gibt dort
//! keine zusätzliche `tree/`-Verschachtelung, weil zu diesen Fällen kein
//! `expect.json` gehört, das neben `tree/` stehen müsste.
//!
//! # Nebenläufigkeit
//! Alle Funktionen in [`harness`] und [`capture`] sind zustandslose freie
//! Funktionen: `Send + Sync`, ohne innere Veränderlichkeit. Parallele
//! Aufrufe (z. B. `cargo test` mit mehreren Testthreads) stören sich nicht,
//! solange sie nicht gleichzeitig in dasselbe Fixture-Verzeichnis schreiben
//! — was keine der `assert_*`-Funktionen tut; nur [`capture::capture`]
//! schreibt, und nur in das ihr übergebene `case_dir`.
//!
//! # Fehler
//! [`error::FixturesError`] — ausschließlich für [`capture::capture`] und
//! die interne `expect.json`-Kodierung ([`fixture_io`]). Die von
//! [`sensor_suite!`] generierten Tests geben kein `Result` zurück; eine
//! verletzte Prüfung äußert sich als Panic (fehlgeschlagene Assertion).
//!
//! # Examples
//! ```rust,ignore
//! use harw_dod_cap::{Bound, Capability, SensorHandle};
//! use harw_dod_signals::{Sensor, SensorReading};
//!
//! #[derive(Debug)]
//! struct ThermalSensor(SensorHandle<Bound>);
//!
//! impl From<SensorHandle<Bound>> for ThermalSensor {
//!     fn from(handle: SensorHandle<Bound>) -> Self {
//!         Self(handle)
//!     }
//! }
//!
//! impl Sensor for ThermalSensor {
//!     fn handle(&self) -> &SensorHandle<Bound> {
//!         &self.0
//!     }
//!     fn poll(&self, now: jiff::Timestamp) -> Result<SensorReading, harw_dod_cap::SensorError> {
//!         // … liest /sys/class/thermal über self.0.scope() …
//!         # Ok(SensorReading::default())
//!     }
//! }
//!
//! #[cfg(test)]
//! mod tests {
//!     use super::ThermalSensor;
//!     use harw_dod_cap::Capability;
//!
//!     harw_dod_fixtures::sensor_suite! {
//!         sensor: ThermalSensor,
//!         capability: Capability::ReadSysfsThermal,
//!         max_cardinality: 16,
//!         fixtures: "fixtures",
//!     }
//! }
//! ```

pub mod capture;
pub mod capture_manifest;
pub mod error;
pub mod harness;
mod macros;

mod fixture_io;

pub use error::{FixturesError, FixturesResult};

// Bewusst **kein** `pub use capture::capture;`: dieser Crate-Wurzel hat mit
// `pub mod capture;` bereits einen Typ-Namensraum-Eintrag `capture` (das
// Modul); ein zusätzlicher Wert-Namensraum-Eintrag gleichen Namens (die
// Funktion) wäre laut Sprachregel zulässig — exakt dasselbe Muster wie
// `syn::parse` —, aber `harw-dod-readfs` hat genau diesen Re-Export aus
// genau demselben Grund unterlassen, der hier gilt: diese Crate darf kein
// `cargo` ausführen, um es zu verifizieren (siehe deren `lib.rs`-Moduldoku,
// Abschnitt „Warum `glob::glob` nicht am Crate-Wurzel re-exportiert ist").
// [`capture::capture`] bleibt deshalb nur über den Modulpfad erreichbar.
pub use capture::CaptureReport;

/// Die feste `SensorId`, mit der die Fixture-Harness jeden Testgriff baut.
///
/// # Description
/// Identisch für jeden Aufruf von [`harness`] und [`capture::capture`] —
/// unabhängig vom konkreten Sensor. Eine von Hand geschriebene
/// `expect.json` (statt über [`capture::capture`] erzeugt) muss exakt diesen
/// Bezeichner in jedem `reading.samples[].sensor`- bzw.
/// `reading.events[].sensor`-Feld tragen, sonst schlägt der Vergleich in
/// [`harness::assert_determinism`] fehl, weil das erwartete `SensorReading`
/// eine andere `SensorId` trägt als das tatsächlich gepollte.
///
/// # Examples
/// ```rust
/// assert_eq!(harw_dod_fixtures::FIXTURE_SENSOR_ID, "harw-dod-fixtures-suite");
/// ```
pub const FIXTURE_SENSOR_ID: &str = "harw-dod-fixtures-suite";

/// Baut den `SensorHandle<Bound>`, mit dem jede Prüfung ihre Sensor-Instanz
/// erzeugt.
///
/// Nicht `pub`: interner Helfer, geteilt zwischen [`harness`] und
/// [`capture`]. Verwendet immer [`FIXTURE_SENSOR_ID`] als `SensorId` — der
/// konkrete Bezeichner ist für keine der acht Prüfungen bedeutungstragend,
/// nur `capability` und `scope` sind es.
pub(crate) fn build_handle(
    capability: harw_dod_cap::Capability,
    scope: harw_dod_cap::ReadScope,
) -> harw_dod_cap::SensorHandle<harw_dod_cap::Bound> {
    harw_dod_cap::SensorHandle::new(
        harw_types::SensorId::from_str(FIXTURE_SENSOR_ID),
        capability,
    )
    .bind(scope)
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_fixture_sensor_id_is_stable() {
        assert_eq!(super::FIXTURE_SENSOR_ID, "harw-dod-fixtures-suite");
    }

    #[test]
    fn test_build_handle_carries_given_capability_and_scope() {
        let scope =
            harw_dod_cap::ReadScope::from_roots([std::path::PathBuf::from("/tmp/does-not-matter")]);
        let handle = super::build_handle(harw_dod_cap::Capability::ReadProcStat, scope.clone());
        assert_eq!(handle.capability(), harw_dod_cap::Capability::ReadProcStat);
        assert_eq!(handle.scope(), &scope);
        assert_eq!(handle.id().as_str(), super::FIXTURE_SENSOR_ID);
    }
}
