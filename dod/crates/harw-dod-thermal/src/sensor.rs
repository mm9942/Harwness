//! `ThermalSensor`: der `Sensor`-Trait-Impl dieser Crate (AW2-07-Brief).
//!
//! Siehe die Crate-Moduldoku (`lib.rs`) für Zweck, sysfs-Format,
//! Einheitenumrechnung, Zonenlabel und die Begründung, warum dieser Sensor
//! `Sensor` von Hand statt über `#[derive(harw_macros::SensorSource)]`
//! implementiert. Dieses Modul besitzt ausschließlich [`ThermalSensor`] und
//! die private Lese-/Umrechnungslogik dahinter; kein `std::fs`-Aufruf steht
//! in dieser Datei — jeder Zugriff läuft über `harw_dod_readfs`
//! (`parse_i64`, `read_first_line`, `glob::glob`).

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use harw_dod_cap::{Bound, Capability, ReadScope, SensorError, SensorHandle};
use harw_dod_readfs::ReadFsError;
use harw_dod_signals::{HostSample, Sensor, SensorReading};
use jiff::Timestamp;

/// Suffix-Glob-Muster für die Zonenverzeichnisse, relativ zur ersten
/// Bereichswurzel (siehe [`relative_pattern`] für die Begründung, warum das
/// volle Muster erst zur Laufzeit zusammengesetzt wird, statt fest verdrahtet
/// zu sein).
const ZONE_DIR_GLOB_SUFFIX: &str = "thermal_zone*";

/// Umrechnungsfaktor sysfs-Millidegree-Celsius → Grad Celsius (siehe
/// `lib.rs`-Moduldoku, Abschnitt „Einheitenumrechnung").
const MILLICELSIUS_PER_CELSIUS: f64 = 1000.0;

/// Fester Teil jedes emittierten Metriknamens; das sanitisierte Zonenlabel
/// wird direkt angehängt (siehe `lib.rs`-Moduldoku, Abschnitt
/// „Zonenlabel").
const METRIC_PREFIX: &str = "temperature_celsius_";

/// Obergrenze der Zeichen, die aus `type` oder dem Zonen-Verzeichnisnamen in
/// den Metriknamen übernommen werden — Schutz gegen unbegrenztes Wachstum
/// durch einen ungewöhnlichen `type`-Inhalt (Adversarial-Fall).
const MAX_LABEL_LEN: usize = 64;

/// Ersatzlabel, falls selbst der Zonen-Verzeichnisname nicht als UTF-8 lesbar
/// ist (praktisch unerreichbar bei sysfs-Zonennamen, aber total statt
/// `unwrap`).
const FALLBACK_ZONE_LABEL: &str = "zone";

/// Thermalzonen-Sensor: liest `temp` (und, für das Label, `type`) jeder
/// sichtbaren Zone unterhalb der Bereichswurzel.
///
/// # Description
/// Hält ausschließlich den gebundenen Griff; die gesamte Lese- und
/// Umrechnungslogik steht in [`Sensor::poll`]. Implementiert `Sensor` von
/// Hand statt über `#[derive(harw_macros::SensorSource)]` — siehe
/// `lib.rs`-Moduldoku, Abschnitt „Warum kein `#[derive(SensorSource)]`", für
/// die vollständige Begründung.
#[derive(Debug)]
pub struct ThermalSensor {
    handle: SensorHandle<Bound>,
}

impl ThermalSensor {
    /// Die Fähigkeit, die dieser Sensor beansprucht.
    ///
    /// # Description
    /// Maschinenlesbare Deklaration für das CI-Privilegienbudget-Gate,
    /// nachgebildet aus dem, was `#[derive(harw_macros::SensorSource)]` an
    /// derselben Stelle erzeugen würde (siehe `lib.rs`-Moduldoku).
    ///
    /// # Returns
    /// [`Capability::ReadSysfsThermal`], die einzige Fähigkeit dieses
    /// Sensors.
    ///
    /// # Errors
    /// Keine — eine `const`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::Capability;
    /// use harw_dod_thermal::ThermalSensor;
    ///
    /// assert_eq!(ThermalSensor::CAPABILITY, Capability::ReadSysfsThermal);
    /// ```
    pub const CAPABILITY: Capability = Capability::ReadSysfsThermal;

    /// Kanonische Kennung dieses Sensors.
    ///
    /// # Description
    /// Rein informativ, analog zu dem, was
    /// `#[derive(harw_macros::SensorSource)]` als `Self::SENSOR_ID` erzeugen
    /// würde. Nicht zu verwechseln mit `harw_dod_cap::SensorHandle::id()`,
    /// der Kennung der konkreten Sensor-*Instanz*.
    ///
    /// # Returns
    /// `"thermal"`.
    ///
    /// # Errors
    /// Keine — eine `const`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_thermal::ThermalSensor;
    ///
    /// assert_eq!(ThermalSensor::SENSOR_ID, "thermal");
    /// ```
    pub const SENSOR_ID: &'static str = "thermal";
}

impl From<SensorHandle<Bound>> for ThermalSensor {
    /// Baut einen `ThermalSensor` aus einem bereits gebundenen Griff.
    ///
    /// # Description
    /// Die einzige von `harw_dod_fixtures::sensor_suite!` verlangte
    /// Konstruktionskonvention (siehe dessen Moduldoku): ein gebundener
    /// Griff hinein, eine Sensor-Instanz heraus.
    ///
    /// # Arguments
    /// - `handle` (`SensorHandle<Bound>`): der gebundene Griff, mit
    ///   [`Capability::ReadSysfsThermal`] und einem `ReadScope` auf die
    ///   Thermalzonen-Wurzel (real: `/sys/class/thermal`; in Tests: ein
    ///   Fixture-`tree/`-Verzeichnis).
    ///
    /// # Returns
    /// Einen einsatzbereiten `ThermalSensor`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::scope::AliasRoot;
    /// use harw_dod_cap::{Capability, ReadScope, SensorHandle};
    /// use harw_dod_thermal::ThermalSensor;
    /// use harw_types::SensorId;
    /// use std::path::PathBuf;
    ///
    /// // sysfs-Klasseneinträge sind Symlinks nach `/sys/devices/...` (F-005) —
    /// // `AliasRoot::sysfs_class` baut den einzigen Bereich, der solche Ziele
    /// // zulässt, statt der veralteten, für sysfs-Klassenwurzeln unsicheren
    /// // `ReadScope::from_roots`.
    /// let alias = AliasRoot::sysfs_class(PathBuf::from("/sys/class/thermal"))
    ///     .expect("gültige sysfs-Klassenwurzel");
    /// let scope = ReadScope::from_roots_and_aliases(Vec::new(), [alias]);
    /// let handle = SensorHandle::new(SensorId::from_str("thermal-0"), Capability::ReadSysfsThermal)
    ///     .bind(scope);
    /// let _sensor = ThermalSensor::from(handle);
    /// ```
    fn from(handle: SensorHandle<Bound>) -> Self {
        Self { handle }
    }
}

impl Sensor for ThermalSensor {
    /// Der gebundene Griff dieses Sensors.
    ///
    /// # Description
    /// Reine Referenz-Rückgabe; siehe `harw_dod_signals::Sensor::handle`.
    ///
    /// # Returns
    /// Referenz auf den bei [`ThermalSensor::from`] übergebenen
    /// `SensorHandle`.
    ///
    /// # Errors
    /// Keine.
    ///
    /// # Examples
    /// Siehe [`ThermalSensor::from`].
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    /// Liest alle sichtbaren Thermalzonen einmal aus.
    ///
    /// # Description
    /// Ermittelt zuerst die Zonenverzeichnisse unterhalb der ersten
    /// Bereichswurzel (`thermal_zone*`, siehe [`relative_pattern`] für die
    /// Begründung, warum das Muster zur Laufzeit gebaut wird statt fest
    /// verdrahtet zu sein). Für jede Zone: liest `temp` (Millidegree Celsius)
    /// und rechnet in Grad Celsius um (siehe `lib.rs`-Moduldoku, Abschnitt
    /// „Einheitenumrechnung"); liest `type` für das Zonenlabel, mit
    /// Ersatzwert bei Fehlen (siehe [`zone_label`]). Jede Zone erzeugt genau
    /// ein `HostSample`, dessen `metric` das Zonenlabel trägt.
    ///
    /// # Arguments
    /// - `now` (`jiff::Timestamp`): injizierte Zeit, unverändert in jedes
    ///   `HostSample::observed_at` übernommen. Diese Funktion liest nie die
    ///   Systemuhr.
    ///
    /// # Returns
    /// Ein `SensorReading` mit einem `HostSample` je sichtbarer Zone und
    /// keinen Events. Nie leer bei `Ok` — eine leere Zonenmenge ist
    /// [`SensorError::SourceUnavailable`], kein leeres `Ok`.
    ///
    /// # Errors
    /// - [`SensorError::SourceUnavailable`]: keine Zonenverzeichnisse
    ///   sichtbar (leerer Bereich oder ein Host ohne Thermalzonen — aus
    ///   Sensorsicht ununterscheidbar).
    /// - [`SensorError::MalformedSource`]: eine sichtbare Zone hat keine
    ///   `temp`-Datei, eine leere `temp`-Datei, oder einen nicht als `i64`
    ///   parsbaren Inhalt. Bricht den gesamten Abruf ab, statt die defekte
    ///   Zone stillschweigend auszulassen: `SensorReading` trägt kein
    ///   Fehlerfeld je Sample, und ein Konsument, der Temperaturwerte für
    ///   Schwellwertentscheidungen nutzt, soll eine unvollständige Zonenmenge
    ///   nie mit „alles in Ordnung" verwechseln können.
    /// - [`SensorError::Io`]: ein Betriebssystemfehler beim Lesen, der
    ///   keiner der beiden obigen Fälle ist (z. B. Berechtigung verweigert).
    ///
    /// # Examples
    /// ```rust,no_run
    /// use harw_dod_cap::scope::AliasRoot;
    /// use harw_dod_cap::{Capability, ReadScope, SensorHandle};
    /// use harw_dod_signals::Sensor;
    /// use harw_dod_thermal::ThermalSensor;
    /// use harw_types::SensorId;
    /// use std::path::PathBuf;
    ///
    /// let alias = AliasRoot::sysfs_class(PathBuf::from("/sys/class/thermal"))
    ///     .expect("gültige sysfs-Klassenwurzel");
    /// let scope = ReadScope::from_roots_and_aliases(Vec::new(), [alias]);
    /// let handle = SensorHandle::new(SensorId::from_str("thermal-0"), Capability::ReadSysfsThermal)
    ///     .bind(scope);
    /// let sensor = ThermalSensor::from(handle);
    /// let reading = sensor.poll(jiff::Timestamp::UNIX_EPOCH)?;
    /// # Ok::<(), harw_dod_cap::SensorError>(())
    /// ```
    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        let scope = self.handle.scope();
        let zones = zone_dirs(scope)?;
        if zones.is_empty() {
            return Err(SensorError::SourceUnavailable);
        }

        let mut samples = Vec::with_capacity(zones.len());
        for zone_dir in &zones {
            let millicelsius = read_zone_millicelsius(scope, zone_dir)?;
            let celsius = millicelsius as f64 / MILLICELSIUS_PER_CELSIUS;
            let label = zone_label(scope, zone_dir);

            samples.push(HostSample {
                sensor: self.handle.id().clone(),
                observed_at: now,
                metric: Cow::Owned(format!("{METRIC_PREFIX}{label}")),
                value: celsius,
            });
        }

        Ok(SensorReading {
            samples,
            events: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------
// Interne Helfer
// ---------------------------------------------------------------------

// Baut ein bereichsrelatives Glob-Muster aus `root` und `suffix`, statt den
// vollen Pfad fest zu verdrahten. `harw_dod_readfs::glob::glob` beginnt seine
// Suche laut eigener Moduldoku immer bei `/` und nutzt den `ReadScope` nur
// als nachträglichen Filter — ein zur Kompilierzeit fester Pfad wie
// `"sys/class/thermal/thermal_zone*/temp"` fände deshalb in einer
// Fixture-Prüfung (Bereichswurzel = ein `fixtures/<fall>/tree`-Verzeichnis)
// nie etwas, weil dort nie gesucht würde. Aus `root` (der tatsächlichen
// ersten Bereichswurzel, egal ob `/sys/class/thermal` in Produktion oder ein
// Fixture-`tree/` im Test) plus `suffix` gebaut, führt dieselbe Suche in
// beiden Fällen an dieselbe Stelle. `None`, wenn `root` nicht absolut ist —
// ein entartetes, in der Praxis unerreichbares Konfigurationsproblem.
fn relative_pattern(root: &Path, suffix: &str) -> Option<String> {
    let relative = root.strip_prefix("/").ok()?;
    let relative = relative.to_string_lossy();
    if relative.is_empty() {
        Some(suffix.to_owned())
    } else {
        Some(format!("{relative}/{suffix}"))
    }
}

// Findet alle Zonenverzeichnisse innerhalb der ersten Bereichswurzel von
// `scope`, sortiert (siehe `glob::glob`s eigene Garantie). Ein `scope` ohne
// Wurzeln oder mit nicht-absoluter Wurzel liefert eine leere Liste statt
// eines Fehlers — `poll` bildet das auf `SensorError::SourceUnavailable` ab.
fn zone_dirs(scope: &ReadScope) -> Result<Vec<PathBuf>, SensorError> {
    let Some(root) = scope.roots().next() else {
        return Ok(Vec::new());
    };
    let Some(pattern) = relative_pattern(root, ZONE_DIR_GLOB_SUFFIX) else {
        return Ok(Vec::new());
    };
    harw_dod_readfs::glob::glob(scope, &pattern).map_err(map_readfs_err)
}

// Liest den Millidegree-Celsius-Rohwert einer Zone. Eine fehlende
// `temp`-Datei bildet **nicht** auf `SensorError::Io` ab, sondern auf
// `SensorError::MalformedSource`: `zone_dir` ist bereits als Treffer von
// `zone_dirs` bekannt, das Zonenverzeichnis existiert also — nur die darin
// erwartete Datei fehlt. Das ist per Definition eine Quelle mit unerwarteter
// Form, nicht eine gänzlich abwesende Quelle.
fn read_zone_millicelsius(scope: &ReadScope, zone_dir: &Path) -> Result<i64, SensorError> {
    let temp_path = zone_dir.join("temp");
    match harw_dod_readfs::parse_i64(scope, &temp_path) {
        Ok(value) => Ok(value),
        Err(ReadFsError::Scope(SensorError::Io(io_err)))
            if io_err.kind() == std::io::ErrorKind::NotFound =>
        {
            Err(SensorError::MalformedSource)
        }
        Err(err) => Err(map_readfs_err(err)),
    }
}

// Bestimmt das Label einer Zone: der erste, getrimmte Zeileninhalt ihrer
// `type`-Datei, oder — falls diese fehlt, leer ist oder aus einem anderen
// Grund nicht lesbar ist — der Zonen-Verzeichnisname selbst. Ein
// fehlgeschlagenes Lesen von `type` lässt den gesamten Zonen-Messwert
// bewusst nicht scheitern: `type` ist Beschriftung, keine Kerndaten (anders
// als `temp`, siehe `read_zone_millicelsius`).
fn zone_label(scope: &ReadScope, zone_dir: &Path) -> String {
    let dir_name = zone_dir
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or(FALLBACK_ZONE_LABEL);

    let type_path = zone_dir.join("type");
    let raw = harw_dod_readfs::read_first_line(scope, &type_path)
        .ok()
        .filter(|line| !line.trim().is_empty())
        .unwrap_or_else(|| dir_name.to_owned());

    sanitize_label(&raw)
}

// Reduziert ein aus sysfs gelesenes oder aus einem Verzeichnisnamen
// stammendes Label auf sichere Metriknamen-Zeichen (`[a-z0-9_]`), gekürzt auf
// `MAX_LABEL_LEN` Zeichen. Adversarial-Schutz: eine `type`-Datei mit
// beliebigem Inhalt darf den Metriknamen weder unbegrenzt wachsen lassen
// noch Zeichen einschleusen, die als Metrikname untypisch wären.
fn sanitize_label(raw: &str) -> String {
    let sanitized: String = raw
        .trim()
        .chars()
        .take(MAX_LABEL_LEN)
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();

    if sanitized.is_empty() {
        FALLBACK_ZONE_LABEL.to_owned()
    } else {
        sanitized
    }
}

// Bildet einen `harw_dod_readfs::ReadFsError` auf `SensorError` ab. Analog
// zur privaten `map_readfs_err`-Hilfsfunktion, die
// `#[derive(harw_macros::SensorSource)]` je Sensor generiert (siehe
// `harw-macros/src/sensor_source.rs`) — hier von Hand nachgebildet, weil
// dieser Sensor den Trait von Hand implementiert (siehe `lib.rs`-Moduldoku).
fn map_readfs_err(err: ReadFsError) -> SensorError {
    match err {
        ReadFsError::Scope(inner) => inner,
        ReadFsError::TooLarge { .. }
        | ReadFsError::GlobPatternAbsolute { .. }
        | ReadFsError::GlobPatternTraversal { .. }
        // Eine überschrittene Glob-Grenze (`harw_dod_readfs::glob::MAX_GLOB_COMPONENTS`/
        // `MAX_GLOB_CANDIDATES`) beschreibt, wie bei `TooLarge`, eine Quelle mit
        // unerwarteter Form (ungewöhnlich tiefe/breite Zonenstruktur), nicht einen
        // Fehler dieses Werkzeugs — dieselbe Abbildung wie die drei Geschwister
        // oben (C-SCOPE-Nachfolge, F-005-Register).
        | ReadFsError::GlobLimitExceeded { .. } => SensorError::MalformedSource,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use harw_dod_cap::{Capability, ReadScope, SensorError, SensorHandle};
    use harw_dod_signals::Sensor;
    use harw_types::SensorId;
    use jiff::Timestamp;

    use super::ThermalSensor;

    /// Das `fixtures/`-Wurzelverzeichnis dieser Crate.
    fn fixtures_root() -> PathBuf {
        PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures"))
    }

    /// Baut einen `ThermalSensor`, dessen `ReadScope` genau `root` umfasst.
    /// Nicht über `harw_dod_fixtures::build_handle` (das ist `pub(crate)`
    /// dort) — die eigenen Zusatzprüfungen unten brauchen keine exakte
    /// `SensorId`, nur `sensor_suite!` (siehe unten) verlangt die fixe
    /// `harw_dod_fixtures::FIXTURE_SENSOR_ID`, und baut ihren Testgriff dafür
    /// selbst.
    fn build_sensor(root: PathBuf) -> ThermalSensor {
        let scope = ReadScope::from_roots([root]);
        let handle = SensorHandle::new(
            SensorId::from_str("thermal-test"),
            Capability::ReadSysfsThermal,
        )
        .bind(scope);
        ThermalSensor::from(handle)
    }

    #[test]
    fn test_millicelsius_is_converted_to_celsius() {
        let sensor = build_sensor(fixtures_root().join("one-zone/tree"));
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("one-zone Fixture muss erfolgreich pollen");

        assert_eq!(reading.samples.len(), 1);
        // Epsilon-Vergleich statt `assert_eq!` auf `f64`: 45000/1000 = 45.0 ist
        // hier zwar exakt darstellbar, ein direkter `==`-Vergleich auf
        // Gleitkommazahlen ist aber unabhängig davon ein Muster, das diese
        // Crate nicht etablieren will.
        assert!(
            (reading.samples[0].value - 45.0).abs() < f64::EPSILON,
            "45000 Millidegree müssen 45.0 °C ergeben, war {}",
            reading.samples[0].value
        );
    }

    #[test]
    fn test_multiple_zones_yield_distinct_metric_labels() {
        let sensor = build_sensor(fixtures_root().join("multi-zone/tree"));
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("multi-zone Fixture muss erfolgreich pollen");

        assert_eq!(reading.samples.len(), 2);
        assert_ne!(
            reading.samples[0].metric, reading.samples[1].metric,
            "zwei Zonen müssen unterschiedliche Metriknamen tragen"
        );
    }

    #[test]
    fn test_zone_without_type_file_falls_back_to_directory_name_label() {
        let sensor = build_sensor(fixtures_root().join("zone-without-type/tree"));
        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("eine Zone ohne type-Datei muss trotzdem gemeldet werden");

        assert_eq!(reading.samples.len(), 1);
        assert!(
            reading.samples[0].metric.contains("thermal_zone0"),
            "Ersatzlabel muss den Zonen-Verzeichnisnamen enthalten: {}",
            reading.samples[0].metric
        );
    }

    #[test]
    fn test_non_numeric_temp_content_is_malformed_source_without_leaking_content() {
        let sensor = build_sensor(fixtures_root().join("malformed/non-numeric"));
        let err = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect_err("nicht-numerischer Inhalt muss scheitern");

        assert!(matches!(err, SensorError::MalformedSource));
        let rendered = format!("{err}{err:?}");
        assert!(
            !rendered.contains("not-a-number"),
            "Fehlermeldung darf den gelesenen Inhalt nicht enthalten: {rendered}"
        );
    }

    /// Der Ordner der echten Pi-Captures (`C-FIXT`, `harw-dod-fixtures`),
    /// relativ zum eigenen Crate-Wurzelverzeichnis dieser Crate erreicht —
    /// die Captures leben nicht in dieser Crate, sondern werden über den
    /// Geschwister-Pfad `../harw-dod-fixtures/captures/rpi5-6.18` referenziert
    /// (Workspace-Geschwisterlayout, wie `path = "../harw-dod-cap"` in
    /// `Cargo.toml`).
    fn rpi5_captures_dir() -> PathBuf {
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../harw-dod-fixtures/captures/rpi5-6.18"
        ))
    }

    /// Regressionstest für F-005/F-202 mit einer echten, auf diesem
    /// Raspberry Pi 5 erhobenen Capture (`thermal.json`): `thermal_zone0`
    /// erscheint dort als echter Symlink nach `/sys/devices/virtual/...`
    /// (nicht als synthetisches Testverzeichnis). Vor C-SCOPE hätte
    /// `ReadScope::from_roots` diesen Treffer verworfen
    /// (`SensorError::SourceUnavailable`, das Kernproblem von F-005); mit
    /// [`harw_dod_cap::scope::AliasRoot`] löst der Sensor die reale
    /// Symlink-Kette auf und liest den echten erfassten Wert.
    #[test]
    fn test_poll_reads_real_pi_capture_through_alias_scope_regression_f005() {
        let manifest_path = rpi5_captures_dir().join("thermal.json");
        let manifest = harw_dod_fixtures::capture_manifest::load(&manifest_path)
            .expect("captures/rpi5-6.18/thermal.json muss ladbar sein");

        let tmp = tempfile::tempdir().expect("tempdir für die Materialisierung");
        harw_dod_fixtures::capture_manifest::materialize(&manifest, tmp.path())
            .expect("materialize muss die echte Symlink-Struktur anlegen");

        // Dieselbe Beziehung wie in Produktion (`AliasRoot::sysfs_class`:
        // declared = Klassenpfad, resolved_prefix = `/sys/devices`), nur mit
        // einer Tempdir-Wurzel statt der realen `/`-Wurzel, damit der Test
        // ohne echten sysfs-Zugriff läuft.
        let declared = tmp.path().join("sys/class/thermal");
        let resolved_prefix = tmp.path().join("sys/devices");
        let alias = harw_dod_cap::scope::AliasRoot::new(declared, resolved_prefix)
            .expect("AliasRoot::new mit Tempdir-Wurzeln");
        let scope = ReadScope::from_roots_and_aliases(Vec::new(), [alias]);
        let handle = SensorHandle::new(
            SensorId::from_str("thermal-alias-capture-test"),
            Capability::ReadSysfsThermal,
        )
        .bind(scope);
        let sensor = ThermalSensor::from(handle);

        let reading = sensor
            .poll(Timestamp::UNIX_EPOCH)
            .expect("Alias-Scope muss die reale Pi-Capture über den Symlink lesen");

        assert_eq!(reading.samples.len(), 1);
        assert!(
            (reading.samples[0].value - 69.95).abs() < f64::EPSILON,
            "69950 Millidegree aus der echten Capture müssen 69.95 °C ergeben, war {}",
            reading.samples[0].value
        );
    }

    harw_dod_fixtures::sensor_suite! {
        sensor: ThermalSensor,
        capability: Capability::ReadSysfsThermal,
        max_cardinality: 16,
        fixtures: "fixtures",
    }
}
