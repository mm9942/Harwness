//! `CpuSensor` — handgeschriebene `Sensor`-Implementierung für `/proc/stat`.
//!
//! # Verantwortungsbereich
//! Siehe die Crate-Moduldoku (`crate`) für Format, die zwei bewussten
//! Entscheidungen (kumulative Zähler statt Rate; Aggregat statt je Kern) und
//! die Begründung, warum `#[derive(harw_macros::SensorSource)]` diesen Fall
//! nicht trägt. Dieses Modul besitzt ausschließlich [`CpuSensor`] und seine
//! private Spaltenzuordnung ([`parse_cpu_line`]).
//!
//! # Nebenläufigkeit
//! [`CpuSensor`] hält keinen veränderlichen Zustand: `Send + Sync`
//! automatisch, `poll` nimmt `&self`.
//!
//! # Fehler
//! [`harw_dod_cap::SensorError`] — siehe [`CpuSensor::poll`] für die
//! vollständige Zuordnung.

use std::borrow::Cow;

use harw_dod_cap::{Bound, Capability, SensorError, SensorHandle};
use harw_dod_readfs::ReadFsError;
use harw_dod_signals::{HostSample, Sensor, SensorReading};
use jiff::Timestamp;

/// Der Dateiname der Quelle relativ zur Wurzel des gebundenen [`ReadScope`]
/// (`harw_dod_cap::ReadScope`) dieses Sensors.
///
/// # Description
/// In Produktion ist die Scope-Wurzel `/proc`, sodass sich daraus
/// `/proc/stat` ergibt — [`harw_dod_cap::Capability::ReadProcStat::probe`]
/// nennt denselben Pfad informativ. In der Fixture-Harness ist die
/// Scope-Wurzel stattdessen `fixtures/<fall>/tree` bzw.
/// `fixtures/malformed/<fall>`; dieselbe relative Zusammensetzung liefert
/// dort `.../tree/stat` bzw. `.../<fall>/stat`.
const STAT_FILE_NAME: &str = "stat";

/// Mindestzahl numerischer Felder nach dem Label `cpu`, unterhalb derer die
/// Zeile als fehlerhaft gilt.
///
/// # Description
/// `user`, `nice`, `system`, `idle` sind die vier Felder, die `/proc/stat`
/// bereits in der ursprünglichen Kernel-2.4-Form trug, bevor `iowait`,
/// `irq`/`softirq` und `steal`/`guest`/`guest_nice` in späteren
/// Kernel-Versionen hinzukamen. Eine `cpu`-Zeile mit weniger als diesen vier
/// Feldern ist keine gültige `/proc/stat`-Ausgabe irgendeiner bekannten
/// Kernel-Version.
const MIN_CPU_FIELDS: usize = 4;

/// Die zehn bekannten Spaltennamen einer `cpu`-Aggregatzeile, in exakter
/// Reihenfolge, als `HostSample::metric`-Werte.
///
/// # Description
/// Bestimmt sowohl die Zuordnung Spalte → Metrikname als auch die
/// Obergrenze der Kardinalität dieses Sensors (siehe [`MAX_CARDINALITY`]
/// und die Crate-Moduldoku, Entscheidung 2). Enthält eine Zeile mit mehr als
/// zehn numerischen Feldern nach `cpu` (ein künftiger Kernel mit weiteren
/// Spalten), werden die überzähligen Felder stillschweigend ignoriert —
/// dieser Sensor erfindet für sie keinen Metriknamen, den er nicht kennt.
const FIELD_METRICS: [&str; 10] = [
    "user_jiffies",
    "nice_jiffies",
    "system_jiffies",
    "idle_jiffies",
    "iowait_jiffies",
    "irq_jiffies",
    "softirq_jiffies",
    "steal_jiffies",
    "guest_jiffies",
    "guest_nice_jiffies",
];

/// Die deklarierte Obergrenze verschiedener Labelkombinationen je Poll.
///
/// # Description
/// Da dieser Sensor ausschließlich die `cpu`-Aggregatzeile liest (siehe
/// Crate-Moduldoku, Entscheidung 2), ist die Kardinalität unabhängig von der
/// Kernzahl des Hosts fest auf die Länge von [`FIELD_METRICS`] begrenzt.
/// Dieser Wert ist der Fixture-seitige Spiegel, den
/// [`harw_dod_fixtures::sensor_suite!`] als `max_cardinality` erwartet.
///
/// # Examples
/// ```rust
/// assert_eq!(harw_dod_cpu::sensor::MAX_CARDINALITY, 10);
/// ```
pub const MAX_CARDINALITY: usize = FIELD_METRICS.len();

/// Liest die CPU-Zeitzähler aus `/proc/stat` — genau eine Quelle, genau eine
/// Fähigkeit ([`Capability::ReadProcStat`]).
///
/// # Description
/// Siehe die Crate-Moduldoku für das Dateiformat und die zwei bewussten
/// Entscheidungen (kumulative Zähler statt Rate; Aggregat statt je Kern).
/// Konstruiert ausschließlich über [`From<SensorHandle<Bound>>`], die
/// Konvention, die [`harw_dod_fixtures::sensor_suite!`] von jedem
/// Sensor-Typ verlangt.
#[derive(Debug)]
pub struct CpuSensor {
    handle: SensorHandle<Bound>,
}

impl CpuSensor {
    /// Die Fähigkeit, die dieser Sensor beansprucht.
    ///
    /// # Description
    /// Maschinenlesbare Deklaration, analog zur gleichnamigen Konstante, die
    /// `#[derive(harw_macros::SensorSource)]` für die anderen zehn
    /// Sensor-Crates erzeugt (siehe Crate-Moduldoku, Abschnitt „Warum kein
    /// `#[derive(harw_macros::SensorSource)]`") — von Hand nachgezogen, damit
    /// ein Aufrufer, der über alle elf Sensor-Typen hinweg dieselbe
    /// Konvention erwartet, hier nicht auf eine Ausnahme stößt.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::Capability;
    /// use harw_dod_cpu::CpuSensor;
    ///
    /// assert_eq!(CpuSensor::CAPABILITY, Capability::ReadProcStat);
    /// ```
    pub const CAPABILITY: Capability = Capability::ReadProcStat;

    /// Kanonische Kennung dieser Sensorart.
    ///
    /// # Description
    /// Ein fester Bezeichner für die Sensor*art* — unabhängig von der
    /// `harw_types::SensorId`, die eine konkrete Instanz über
    /// [`SensorHandle::new`] erhält.
    ///
    /// # Examples
    /// ```rust
    /// assert_eq!(harw_dod_cpu::CpuSensor::SENSOR_ID, "cpu");
    /// ```
    pub const SENSOR_ID: &'static str = "cpu";
}

impl From<SensorHandle<Bound>> for CpuSensor {
    /// Baut einen `CpuSensor` aus einem bereits gebundenen Griff.
    ///
    /// # Description
    /// Die einzige Konstruktionskonvention, die
    /// [`harw_dod_fixtures::sensor_suite!`] von jedem Sensor-Typ verlangt
    /// (siehe dessen Moduldoku, Abschnitt „Voraussetzung an `sensor`").
    ///
    /// # Arguments
    /// - `handle` (`SensorHandle<Bound>`): der gebundene Griff, dessen
    ///   Fähigkeit üblicherweise [`CpuSensor::CAPABILITY`] ist (nicht von
    ///   diesem Konstruktor erzwungen — die Fähigkeit steht bereits im
    ///   Griff, bevor er hier ankommt).
    ///
    /// # Returns
    /// Einen `CpuSensor`, der über `handle.scope()` liest.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::{Capability, ReadScope, SensorHandle};
    /// use harw_dod_cpu::CpuSensor;
    /// use harw_types::SensorId;
    /// use std::path::PathBuf;
    ///
    /// let scope = ReadScope::from_roots([PathBuf::from("/proc")]);
    /// let handle = SensorHandle::new(SensorId::from_str("cpu-0"), Capability::ReadProcStat)
    ///     .bind(scope);
    /// let _sensor = CpuSensor::from(handle);
    /// ```
    fn from(handle: SensorHandle<Bound>) -> Self {
        Self { handle }
    }
}

impl Sensor for CpuSensor {
    /// Der gebundene Griff dieses Sensors.
    ///
    /// # Returns
    /// Referenz auf den bei der Konstruktion übergebenen
    /// `SensorHandle<Bound>`.
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    /// Liest `/proc/stat` einmal aus und meldet die zehn kumulativen
    /// Zeitzähler der `cpu`-Aggregatzeile.
    ///
    /// # Description
    /// Baut den Quellpfad als `scope.roots().next().join("stat")` (siehe
    /// [`STAT_FILE_NAME`]) und liest ihn über
    /// [`harw_dod_readfs::read_line_fields`] — nie direktes `std::fs`.
    /// Sucht darin die Zeile, deren erstes Feld exakt `"cpu"` ist (nicht
    /// `"cpu0"` o. Ä. — siehe Crate-Moduldoku, Entscheidung 2), und meldet
    /// deren restliche Felder als bis zu zehn `HostSample`s, benannt nach
    /// [`FIELD_METRICS`]. `now` wird unverändert in jedes `HostSample`
    /// durchgereicht; die Systemuhr wird an keiner Stelle gelesen.
    ///
    /// # Arguments
    /// - `now` (`jiff::Timestamp`): injizierte Zeit für
    ///   `HostSample::observed_at`.
    ///
    /// # Returns
    /// Ein [`SensorReading`] mit bis zu zehn Samples und keinen Events
    /// (`/proc/stat` erzeugt keine Sicherheitsereignisse).
    ///
    /// # Errors
    /// - [`SensorError::SourceUnavailable`]: der Lesebereich hat keine
    ///   Wurzel, oder `stat` existiert unterhalb der Wurzel nicht (der Fall
    ///   eines leeren oder unpassenden Bereichs — siehe die Scope-
    ///   Dichtheit-Prüfung der Fixture-Harness).
    /// - [`SensorError::MalformedSource`]: keine Zeile beginnt mit exakt
    ///   `"cpu"`, die gefundene `cpu`-Zeile hat weniger als
    ///   [`MIN_CPU_FIELDS`] numerische Felder, eines ihrer Felder ist keine
    ///   gültige `u64`-Zahl, oder der Inhalt überschreitet
    ///   `harw_dod_readfs::MAX_READ_BYTES`. **Der nicht passende Zeileninhalt
    ///   erscheint in keinem dieser Fälle in der Fehlermeldung.**
    /// - [`SensorError::OutsideScope`] / [`SensorError::Io`]: unverändert
    ///   durchgereicht, falls `read_line_fields` einen anderen
    ///   Bereichsfehler als „nicht gefunden" meldet.
    ///
    /// # Examples
    /// Siehe die Crate-Moduldoku für ein vollständiges Beispiel.
    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        let scope = self.handle.scope();
        let Some(root) = scope.roots().next() else {
            return Err(SensorError::SourceUnavailable);
        };
        let path = root.join(STAT_FILE_NAME);

        let rows = match harw_dod_readfs::read_line_fields(scope, &path) {
            Ok(rows) => rows,
            Err(ReadFsError::Scope(SensorError::Io(io_err)))
                if io_err.kind() == std::io::ErrorKind::NotFound =>
            {
                return Err(SensorError::SourceUnavailable);
            }
            Err(ReadFsError::Scope(inner)) => return Err(inner),
            Err(ReadFsError::TooLarge { .. }) => return Err(SensorError::MalformedSource),
            Err(
                ReadFsError::GlobPatternAbsolute { .. }
                | ReadFsError::GlobPatternTraversal { .. }
                | ReadFsError::GlobLimitExceeded { .. },
            ) => {
                // read_line_fields ruft nie glob() auf; unerreichbar, aber
                // erschöpfend abgedeckt, damit eine künftige ReadFsError-
                // Variante hier nicht still verworfen wird.
                return Err(SensorError::MalformedSource);
            }
        };

        let fields = parse_cpu_line(&rows)?;
        let samples = fields
            .into_iter()
            .map(|(metric, value)| HostSample {
                sensor: self.handle.id().clone(),
                observed_at: now,
                metric: Cow::Borrowed(metric),
                value: value as f64,
            })
            .collect();

        Ok(SensorReading {
            samples,
            events: Vec::new(),
        })
    }
}

/// Findet die `cpu`-Aggregatzeile in `rows` und ordnet ihre Felder den
/// bekannten Spaltennamen zu.
///
/// # Description
/// `rows` ist das Ergebnis von [`harw_dod_readfs::read_line_fields`]: je
/// Zeile ein Vektor ihrer leerraumgetrennten Felder, in Datei- und
/// Spaltenreihenfolge, ohne Prüfung der Feldzahl (siehe dessen Moduldoku).
/// Diese Funktion sucht darin die **eine** Zeile, deren erstes Feld exakt
/// `"cpu"` ist — nicht `"cpu0"`, `"cpu1"`, … (siehe Crate-Moduldoku,
/// Entscheidung 2) — und ordnet ihre restlichen Felder positionsweise
/// [`FIELD_METRICS`] zu. Überzählige Felder über die zehn bekannten Namen
/// hinaus werden stillschweigend ignoriert (siehe [`FIELD_METRICS`]-Doku);
/// das ist der Robustheitsfall, den ein Kernel mit zusätzlichen Spalten
/// auslöst.
///
/// # Arguments
/// - `rows` (`&[Vec<String>]`): die Zeilen von `/proc/stat`, wie von
///   [`harw_dod_readfs::read_line_fields`] geliefert.
///
/// # Returns
/// Die gefundenen `(Metrikname, Wert)`-Paare, in Spaltenreihenfolge —
/// mindestens [`MIN_CPU_FIELDS`], höchstens [`FIELD_METRICS`]`.len()`
/// Einträge.
///
/// # Errors
/// [`SensorError::MalformedSource`], wenn keine Zeile mit erstem Feld
/// `"cpu"` existiert, die gefundene Zeile weniger als [`MIN_CPU_FIELDS`]
/// numerische Felder hat, oder eines dieser Felder keine gültige `u64`-Zahl
/// ist. Der nicht passende Zeileninhalt erscheint nie in der Fehlermeldung
/// — [`SensorError::MalformedSource`] ist inhaltsfrei.
fn parse_cpu_line(rows: &[Vec<String>]) -> Result<Vec<(&'static str, u64)>, SensorError> {
    let aggregate = rows
        .iter()
        .find(|fields| fields.first().map(String::as_str) == Some("cpu"))
        .ok_or(SensorError::MalformedSource)?;

    let values = &aggregate[1..];
    if values.len() < MIN_CPU_FIELDS {
        return Err(SensorError::MalformedSource);
    }

    FIELD_METRICS
        .iter()
        .zip(values.iter())
        .map(|(metric, raw)| {
            raw.trim()
                .parse::<u64>()
                .map(|value| (*metric, value))
                .map_err(|_| SensorError::MalformedSource)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows_from(lines: &[&str]) -> Vec<Vec<String>> {
        lines
            .iter()
            .map(|line| line.split_whitespace().map(str::to_owned).collect())
            .collect()
    }

    #[test]
    fn test_parse_cpu_line_maps_middle_column_not_just_the_first() {
        let rows = rows_from(&[
            "cpu  2255 34 2290 22625563 6290 127 456 0 0 0",
            "cpu0 1132 34 1441 11311718 3675 127 438 0 0 0",
        ]);
        let fields = parse_cpu_line(&rows).expect("wohlgeformte cpu-Zeile muss geparst werden");

        // "system" ist das dritte gemeldete Feld (Index 2) — der Wert aus
        // der Mitte der Zeile, nicht der erste. Nur so belegt der Test, dass
        // die Spaltenzuordnung wirklich stimmt und nicht zufällig, weil
        // Position 0 und 1 übereinstimmen.
        assert_eq!(fields[2], ("system_jiffies", 2290));
        assert_eq!(fields[0], ("user_jiffies", 2255));
    }

    #[test]
    fn test_parse_cpu_line_ignores_per_core_lines() {
        let rows = rows_from(&[
            "cpu  10 20 30 40",
            "cpu0 1 2 3 4",
            "cpu1 1 2 3 4",
        ]);
        let fields = parse_cpu_line(&rows).expect("aggregatzeile muss gefunden werden");
        assert_eq!(
            fields,
            vec![
                ("user_jiffies", 10),
                ("nice_jiffies", 20),
                ("system_jiffies", 30),
                ("idle_jiffies", 40),
            ]
        );
    }

    #[test]
    fn test_parse_cpu_line_accepts_extra_trailing_columns() {
        // Der wichtigste Robustheitsfall: ein künftiger Kernel hängt weitere
        // Spalten an. Der Parser darf daran nicht scheitern.
        let rows = rows_from(&["cpu 1 2 3 4 5 6 7 8 9 10 11 12 13"]);
        let fields = parse_cpu_line(&rows).expect("zusätzliche Spalten dürfen nicht scheitern");
        assert_eq!(fields.len(), FIELD_METRICS.len());
        assert_eq!(fields[9], ("guest_nice_jiffies", 10));
    }

    #[test]
    fn test_parse_cpu_line_accepts_minimum_four_fields() {
        let rows = rows_from(&["cpu 1 2 3 4"]);
        let fields = parse_cpu_line(&rows).expect("vier Felder sind die historische Untergrenze");
        assert_eq!(fields.len(), 4);
    }

    #[test]
    fn test_parse_cpu_line_rejects_too_few_fields() {
        let rows = rows_from(&["cpu 1 2 3"]);
        let err = parse_cpu_line(&rows).expect_err("drei Felder unterschreiten die Untergrenze");
        assert!(matches!(err, SensorError::MalformedSource));
        // Inhaltsfreiheit auch im Fehlerfall zu weniger Spalten: der
        // Zeileninhalt darf in der Meldung nicht auftauchen.
        assert!(!err.to_string().contains("cpu 1 2 3"));
    }

    #[test]
    fn test_parse_cpu_line_rejects_non_numeric_field_without_leaking_content() {
        let rows = rows_from(&["cpu 1 not-a-number 3 4"]);
        let err = parse_cpu_line(&rows).expect_err("nicht-numerischer Wert muss scheitern");
        assert!(matches!(err, SensorError::MalformedSource));
        assert!(!err.to_string().contains("not-a-number"));
    }

    #[test]
    fn test_parse_cpu_line_rejects_missing_aggregate_line() {
        let rows = rows_from(&["cpu0 1 2 3 4", "intr 123 0 0"]);
        let err = parse_cpu_line(&rows).expect_err("ohne cpu-Zeile muss es scheitern");
        assert!(matches!(err, SensorError::MalformedSource));
    }

    #[test]
    fn test_parse_cpu_line_rejects_empty_input() {
        let rows: Vec<Vec<String>> = Vec::new();
        let err = parse_cpu_line(&rows).expect_err("leere Zeilenliste muss scheitern");
        assert!(matches!(err, SensorError::MalformedSource));
    }

    #[test]
    fn test_max_cardinality_matches_field_metrics_length() {
        assert_eq!(MAX_CARDINALITY, 10);
        assert_eq!(MAX_CARDINALITY, FIELD_METRICS.len());
    }

    #[test]
    fn test_capability_and_sensor_id_constants() {
        assert_eq!(CpuSensor::CAPABILITY, Capability::ReadProcStat);
        assert_eq!(CpuSensor::SENSOR_ID, "cpu");
    }
}
