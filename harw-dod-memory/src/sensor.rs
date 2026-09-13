//! `MemorySensor` — handgeschriebene `Sensor`-Implementierung für
//! `/proc/meminfo`.
//!
//! # Verantwortungsbereich
//! Siehe die Crate-Moduldoku (`crate`) für das Dateiformat, die
//! Einheitenbehandlung (inklusive der `kB`-Ungenauigkeit des Kernels), die
//! Auswahlbegründung der fünf gemeldeten Felder und die Begründung, warum
//! `#[derive(harw_macros::SensorSource)]` diesen Fall nicht trägt. Dieses
//! Modul besitzt ausschließlich [`MemorySensor`] und seine private
//! Auswahl-/Umrechnungslogik ([`select_known_fields`],
//! [`parse_meminfo_value`]).
//!
//! # Nebenläufigkeit
//! [`MemorySensor`] hält keinen veränderlichen Zustand: `Send + Sync`
//! automatisch, `poll` nimmt `&self`.
//!
//! # Fehler
//! [`harw_dod_cap::SensorError`] — siehe [`MemorySensor::poll`] für die
//! vollständige Zuordnung.

use std::borrow::Cow;

use harw_dod_cap::{Bound, Capability, SensorError, SensorHandle};
use harw_dod_readfs::ReadFsError;
use harw_dod_signals::{HostSample, Sensor, SensorReading};
use jiff::Timestamp;

/// Der Dateiname der Quelle relativ zur Wurzel des gebundenen `ReadScope`
/// dieses Sensors.
///
/// # Description
/// In Produktion ist die Scope-Wurzel `/proc`, sodass sich daraus
/// `/proc/meminfo` ergibt — [`Capability::probe`] nennt für
/// [`Capability::ReadProcMeminfo`] denselben Pfad informativ. In
/// der Fixture-Harness ist die Scope-Wurzel stattdessen
/// `fixtures/<fall>/tree` bzw. `fixtures/malformed/<fall>`; dieselbe
/// relative Zusammensetzung liefert dort `.../tree/meminfo` bzw.
/// `.../<fall>/meminfo`.
const MEMINFO_FILE_NAME: &str = "meminfo";

/// Trennzeichen zwischen Schlüssel und Wert in `/proc/meminfo`
/// (`"MemTotal:       16316360 kB"`).
const KEY_VALUE_SEPARATOR: char = ':';

/// Das Einheitssuffix, das [`parse_meminfo_value`] als Kibibyte erkennt und
/// mit [`BYTES_PER_KIB`] in Bytes umrechnet.
const KIB_UNIT_SUFFIX: &str = "kB";

/// Umrechnungsfaktor Kibibyte → Bytes.
///
/// # Warum 1024 und nicht 1000
/// `/proc/meminfo` beschriftet seine Werte als `kB`, meint damit aber seit
/// Jahrzehnten **Kibibyte** (2^10 Bytes), nicht den SI-Kilobyte (1000
/// Bytes) — eine Ungenauigkeit der Kernel-Bezeichnung, die sich nie geändert
/// hat. Siehe die Crate-Moduldoku, Abschnitt „Einheitenbehandlung", für die
/// volle Begründung und die Warnung vor einer späteren „Korrektur" auf 1000,
/// die um 2,4 % danebenläge.
const BYTES_PER_KIB: u64 = 1024;

/// Die fünf ausgewählten `/proc/meminfo`-Schlüssel und ihr jeweiliger
/// `HostSample::metric`-Name, in der Reihenfolge, in der sie gemeldet
/// werden.
///
/// # Description
/// Bestimmt sowohl die Auswahl als auch die Obergrenze der Kardinalität
/// dieses Sensors (siehe [`MAX_CARDINALITY`] und die Crate-Moduldoku,
/// Abschnitt „Auswahl", für die Begründung jedes einzelnen Feldes). Die
/// Reihenfolge hier — nicht die Reihenfolge der Zeilen in der Quelldatei —
/// bestimmt die Reihenfolge der gemeldeten `HostSample`s: [`poll`] sucht
/// jeden Schlüssel unabhängig von seiner Position in der Datei.
///
/// [`poll`]: MemorySensor::poll
const KNOWN_FIELDS: [(&str, &str); 5] = [
    ("MemTotal", "mem_total_bytes"),
    ("MemFree", "mem_free_bytes"),
    ("MemAvailable", "mem_available_bytes"),
    ("SwapTotal", "swap_total_bytes"),
    ("SwapFree", "swap_free_bytes"),
];

/// Die deklarierte Obergrenze verschiedener Labelkombinationen je Poll.
///
/// # Description
/// Da dieser Sensor ausschließlich die fünf in [`KNOWN_FIELDS`] genannten
/// Schlüssel meldet — beide `Swap*`-Felder sind auch auf einem Host ohne
/// konfiguriertes Swap mit dem Wert `0` vorhanden, nicht als fehlende Zeile
/// (siehe Crate-Moduldoku) —, ist die Kardinalität unabhängig vom Host fest
/// auf die Länge von [`KNOWN_FIELDS`] begrenzt. Dieser Wert ist der
/// Fixture-seitige Spiegel, den [`harw_dod_fixtures::sensor_suite!`] als
/// `max_cardinality` erwartet.
///
/// # Examples
/// ```rust
/// assert_eq!(harw_dod_memory::sensor::MAX_CARDINALITY, 5);
/// ```
pub const MAX_CARDINALITY: usize = KNOWN_FIELDS.len();

/// Liest die Speicherstatistik aus `/proc/meminfo` — genau eine Quelle,
/// genau eine Fähigkeit ([`Capability::ReadProcMeminfo`]).
///
/// # Description
/// Siehe die Crate-Moduldoku für das Dateiformat, die Einheitenbehandlung
/// und die Auswahlbegründung der fünf gemeldeten Felder. Konstruiert
/// ausschließlich über [`From<SensorHandle<Bound>>`], die Konvention, die
/// [`harw_dod_fixtures::sensor_suite!`] von jedem Sensor-Typ verlangt.
#[derive(Debug)]
pub struct MemorySensor {
    handle: SensorHandle<Bound>,
}

impl MemorySensor {
    /// Die Fähigkeit, die dieser Sensor beansprucht.
    ///
    /// # Description
    /// Maschinenlesbare Deklaration, analog zur gleichnamigen Konstante, die
    /// `#[derive(harw_macros::SensorSource)]` für die anderen Sensor-Crates
    /// erzeugt (siehe Crate-Moduldoku, Abschnitt „Warum kein
    /// `#[derive(SensorSource)]`") — von Hand nachgezogen, damit ein
    /// Aufrufer, der über alle Sensor-Typen hinweg dieselbe Konvention
    /// erwartet, hier nicht auf eine Ausnahme stößt.
    ///
    /// # Returns
    /// [`Capability::ReadProcMeminfo`].
    ///
    /// # Errors
    /// Keine — eine `const`.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::Capability;
    /// use harw_dod_memory::MemorySensor;
    ///
    /// assert_eq!(MemorySensor::CAPABILITY, Capability::ReadProcMeminfo);
    /// ```
    pub const CAPABILITY: Capability = Capability::ReadProcMeminfo;

    /// Kanonische Kennung dieser Sensorart.
    ///
    /// # Description
    /// Ein fester Bezeichner für die Sensor*art* — unabhängig von der
    /// `harw_types::SensorId`, die eine konkrete Instanz über
    /// [`SensorHandle::new`] erhält.
    ///
    /// # Returns
    /// `"memory"`.
    ///
    /// # Errors
    /// Keine — eine `const`.
    ///
    /// # Examples
    /// ```rust
    /// assert_eq!(harw_dod_memory::MemorySensor::SENSOR_ID, "memory");
    /// ```
    pub const SENSOR_ID: &'static str = "memory";
}

impl From<SensorHandle<Bound>> for MemorySensor {
    /// Baut einen `MemorySensor` aus einem bereits gebundenen Griff.
    ///
    /// # Description
    /// Die einzige Konstruktionskonvention, die
    /// [`harw_dod_fixtures::sensor_suite!`] von jedem Sensor-Typ verlangt
    /// (siehe dessen Moduldoku, Abschnitt „Voraussetzung an `sensor`").
    ///
    /// # Arguments
    /// - `handle` (`SensorHandle<Bound>`): der gebundene Griff, dessen
    ///   Fähigkeit üblicherweise [`MemorySensor::CAPABILITY`] ist (nicht von
    ///   diesem Konstruktor erzwungen — die Fähigkeit steht bereits im
    ///   Griff, bevor er hier ankommt).
    ///
    /// # Returns
    /// Einen `MemorySensor`, der über `handle.scope()` liest.
    ///
    /// # Errors
    /// Keine — total.
    ///
    /// # Examples
    /// ```rust
    /// use harw_dod_cap::{Capability, ReadScope, SensorHandle};
    /// use harw_dod_memory::MemorySensor;
    /// use harw_types::SensorId;
    /// use std::path::PathBuf;
    ///
    /// let scope = ReadScope::from_roots([PathBuf::from("/proc")]);
    /// let handle = SensorHandle::new(SensorId::from_str("memory-0"), Capability::ReadProcMeminfo)
    ///     .bind(scope);
    /// let _sensor = MemorySensor::from(handle);
    /// ```
    fn from(handle: SensorHandle<Bound>) -> Self {
        Self { handle }
    }
}

impl Sensor for MemorySensor {
    /// Der gebundene Griff dieses Sensors.
    ///
    /// # Returns
    /// Referenz auf den bei der Konstruktion übergebenen
    /// `SensorHandle<Bound>`.
    ///
    /// # Errors
    /// Keine.
    fn handle(&self) -> &SensorHandle<Bound> {
        &self.handle
    }

    /// Liest `/proc/meminfo` einmal aus und meldet die fünf ausgewählten
    /// Speicherkennzahlen in Bytes.
    ///
    /// # Description
    /// Baut den Quellpfad als `scope.roots().next().join("meminfo")` (siehe
    /// [`MEMINFO_FILE_NAME`]) und liest ihn über
    /// [`harw_dod_readfs::read_key_values`] — nie direktes `std::fs`. Sucht
    /// darin, unabhängig von der Zeilenposition in der Datei, jeden der
    /// fünf in [`KNOWN_FIELDS`] genannten Schlüssel, rechnet seinen Wert
    /// über [`parse_meminfo_value`] in Bytes um und meldet ihn als
    /// `HostSample` unter dem zugehörigen Metriknamen. Jede nicht in
    /// [`KNOWN_FIELDS`] genannte Zeile — auf einem modernen Kernel über
    /// fünfzig Stück — wird stillschweigend übergangen (siehe die
    /// Crate-Moduldoku, Abschnitt „Auswahl").
    ///
    /// # Arguments
    /// - `now` (`jiff::Timestamp`): injizierte Zeit für
    ///   `HostSample::observed_at`. Die Systemuhr wird an keiner Stelle
    ///   gelesen.
    ///
    /// # Returns
    /// Ein [`SensorReading`] mit genau fünf Samples (bei vollständig
    /// wohlgeformter Quelle) und keinen Events (`/proc/meminfo` erzeugt
    /// keine Sicherheitsereignisse).
    ///
    /// # Errors
    /// - [`SensorError::SourceUnavailable`]: der Lesebereich hat keine
    ///   Wurzel, oder `meminfo` existiert unterhalb der Wurzel nicht (der
    ///   Fall eines leeren oder unpassenden Bereichs — siehe die
    ///   Scope-Dichtheit-Prüfung der Fixture-Harness).
    /// - [`SensorError::MalformedSource`]: mindestens einer der fünf
    ///   ausgewählten Schlüssel fehlt vollständig (z. B. eine Zeile ohne
    ///   `:`, die [`harw_dod_readfs::read_key_values`] deshalb überspringt,
    ///   oder eine leere Datei), sein Wert ist nicht als gültige Zahl
    ///   parsbar, sein Einheitssuffix ist weder `kB` noch abwesend, oder der
    ///   Inhalt überschreitet `harw_dod_readfs::MAX_READ_BYTES`. **Der nicht
    ///   passende Zeileninhalt erscheint in keinem dieser Fälle in der
    ///   Fehlermeldung.**
    /// - [`SensorError::OutsideScope`] / [`SensorError::Io`]: unverändert
    ///   durchgereicht, falls `read_key_values` einen anderen
    ///   Bereichsfehler als „nicht gefunden" meldet.
    ///
    /// # Examples
    /// Siehe die Crate-Moduldoku für ein vollständiges Beispiel.
    fn poll(&self, now: Timestamp) -> Result<SensorReading, SensorError> {
        let scope = self.handle.scope();
        let Some(root) = scope.roots().next() else {
            return Err(SensorError::SourceUnavailable);
        };
        let path = root.join(MEMINFO_FILE_NAME);

        let pairs = match harw_dod_readfs::read_key_values(scope, &path, KEY_VALUE_SEPARATOR) {
            Ok(pairs) => pairs,
            Err(ReadFsError::Scope(SensorError::Io(io_err)))
                if io_err.kind() == std::io::ErrorKind::NotFound =>
            {
                return Err(SensorError::SourceUnavailable);
            }
            Err(ReadFsError::Scope(inner)) => return Err(inner),
            Err(ReadFsError::TooLarge { .. }) => return Err(SensorError::MalformedSource),
            Err(ReadFsError::GlobPatternAbsolute { .. } | ReadFsError::GlobPatternTraversal { .. }) => {
                // read_key_values ruft nie glob() auf; unerreichbar, aber
                // erschöpfend abgedeckt, damit eine künftige ReadFsError-
                // Variante hier nicht still verworfen wird.
                return Err(SensorError::MalformedSource);
            }
        };

        let fields = select_known_fields(&pairs)?;
        let samples = fields
            .into_iter()
            .map(|(metric, bytes)| HostSample {
                sensor: self.handle.id().clone(),
                observed_at: now,
                metric: Cow::Borrowed(metric),
                value: bytes as f64,
            })
            .collect();

        Ok(SensorReading {
            samples,
            events: Vec::new(),
        })
    }
}

/// Findet jeden der fünf in [`KNOWN_FIELDS`] genannten Schlüssel in `pairs`
/// und rechnet seinen Wert in Bytes um.
///
/// # Description
/// `pairs` ist das Ergebnis von [`harw_dod_readfs::read_key_values`]: je
/// Zeile ein `(Schlüssel, Wert)`-Paar in Dateireihenfolge, ohne Prüfung von
/// Vollständigkeit oder Wertform (siehe dessen Moduldoku). Diese Funktion
/// durchsucht `pairs` für jeden bekannten Schlüssel **unabhängig von seiner
/// Position** in der Datei — der `MemAvailable`-Wert etwa steht in einem
/// echten `/proc/meminfo` nicht am Anfang, sondern in der Mitte der Datei,
/// und diese Funktion muss ihn dort trotzdem korrekt zuordnen. Fehlt einer
/// der fünf Schlüssel vollständig (keine passende Zeile in `pairs` — etwa
/// weil ihre Zeile keinen `:` trug und deshalb schon von `read_key_values`
/// übersprungen wurde, oder weil die Datei leer war), gilt die gesamte
/// Quelle als fehlerhaft geformt: die fünf ausgewählten Felder sind laut
/// Crate-Moduldoku auf jedem unterstützten Host durchgehend vorhanden, ihr
/// Fehlen ist kein normaler Robustheitsfall wie eine zusätzliche unbekannte
/// Zeile, sondern ein Anzeichen einer beschädigten Quelle.
///
/// # Arguments
/// - `pairs` (`&[(String, String)]`): die `(Schlüssel, Wert)`-Paare von
///   `/proc/meminfo`, wie von [`harw_dod_readfs::read_key_values`]
///   geliefert.
///
/// # Returns
/// Die fünf `(Metrikname, Bytes)`-Paare, in der Reihenfolge von
/// [`KNOWN_FIELDS`].
///
/// # Errors
/// [`SensorError::MalformedSource`], wenn einer der fünf Schlüssel in
/// `pairs` fehlt, oder wenn [`parse_meminfo_value`] seinen Wert ablehnt. Der
/// nicht passende Zeileninhalt erscheint nie in der Fehlermeldung —
/// [`SensorError::MalformedSource`] ist inhaltsfrei.
fn select_known_fields(pairs: &[(String, String)]) -> Result<Vec<(&'static str, u64)>, SensorError> {
    KNOWN_FIELDS
        .iter()
        .map(|&(key, metric)| {
            let raw = pairs
                .iter()
                .find(|(candidate, _)| candidate == key)
                .map(|(_, value)| value.as_str())
                .ok_or(SensorError::MalformedSource)?;
            parse_meminfo_value(raw).map(|bytes| (metric, bytes))
        })
        .collect()
}

/// Parst einen `/proc/meminfo`-Wert und rechnet ihn — falls er das Suffix
/// `kB` trägt — in Bytes um.
///
/// # Description
/// Erkennt zwei Formen (siehe die Crate-Moduldoku, Abschnitt
/// „Einheitenbehandlung", für die vollständige Begründung):
///
/// - **Ein Feld** (`"<zahl>"`): keine Einheit angegeben. Der Zahlenwert wird
///   **unverändert**, ohne Multiplikation, zurückgegeben — diese Funktion
///   erfindet keinen Umrechnungsfaktor für eine Einheit, die die Quelle
///   nicht nennt.
/// - **Zwei Felder** (`"<zahl> kB"`): der Zahlenwert wird mit
///   [`BYTES_PER_KIB`] (**1024**, nicht 1000 — der Kernel meint mit `kB`
///   Kibibyte) in Bytes umgerechnet.
///
/// Jede andere Form — kein Feld, mehr als zwei Felder, ein Einheitssuffix
/// ungleich `kB`, ein nicht-numerischer Zahlenteil, oder ein Ergebnis, das
/// beim Umrechnen `u64::MAX` überschreiten würde — gilt als fehlerhaft
/// geformt.
///
/// # Arguments
/// - `raw` (`&str`): der getrimmte Wertanteil einer `/proc/meminfo`-Zeile,
///   wie von [`harw_dod_readfs::read_key_values`] geliefert (z. B.
///   `"16316360 kB"` oder `"0"`).
///
/// # Returns
/// Der Wert in Bytes (mit `kB`-Suffix) bzw. unverändert (ohne Suffix).
///
/// # Errors
/// [`SensorError::MalformedSource`] für jede Form, die oben nicht als
/// gültig beschrieben ist. Der nicht parsbare Inhalt erscheint nie in der
/// Fehlermeldung.
fn parse_meminfo_value(raw: &str) -> Result<u64, SensorError> {
    let mut tokens = raw.split_whitespace();
    let Some(number) = tokens.next() else {
        return Err(SensorError::MalformedSource);
    };
    let unit = tokens.next();
    if tokens.next().is_some() {
        // Mehr als zwei durch Leerraum getrennte Felder: unerwartete Form.
        return Err(SensorError::MalformedSource);
    }

    let value: u64 = number.parse().map_err(|_| SensorError::MalformedSource)?;

    match unit {
        None => Ok(value),
        Some(suffix) if suffix == KIB_UNIT_SUFFIX => {
            value.checked_mul(BYTES_PER_KIB).ok_or(SensorError::MalformedSource)
        }
        Some(_) => Err(SensorError::MalformedSource),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs_from(entries: &[(&str, &str)]) -> Vec<(String, String)> {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn test_select_known_fields_maps_middle_of_file_value_correctly() {
        // MemAvailable steht hier absichtlich nicht an erster Stelle — der
        // Test belegt, dass die Zuordnung über den Schlüssel erfolgt, nicht
        // über die Zeilenposition.
        let pairs = pairs_from(&[
            ("Buffers", "412080 kB"),
            ("MemTotal", "16316360 kB"),
            ("Cached", "3877300 kB"),
            ("MemAvailable", "9321236 kB"),
            ("MemFree", "2549408 kB"),
            ("SwapTotal", "2097148 kB"),
            ("SwapFree", "1048574 kB"),
        ]);

        let fields = select_known_fields(&pairs).expect("vollständige Felder müssen geparst werden");

        let mem_available = fields
            .iter()
            .find(|(metric, _)| *metric == "mem_available_bytes")
            .expect("mem_available_bytes muss enthalten sein");
        assert_eq!(mem_available.1, 9_321_236 * 1024);
    }

    #[test]
    fn test_select_known_fields_ignores_unknown_lines() {
        let pairs = pairs_from(&[
            ("MemTotal", "1024 kB"),
            ("MemFree", "512 kB"),
            ("MemAvailable", "768 kB"),
            ("SwapTotal", "0 kB"),
            ("SwapFree", "0 kB"),
            ("HugePages_Total", "0"),
            ("Dirty", "128 kB"),
        ]);

        let fields = select_known_fields(&pairs).expect("unbekannte Zeilen dürfen nicht scheitern");
        assert_eq!(fields.len(), MAX_CARDINALITY);
    }

    #[test]
    fn test_select_known_fields_rejects_missing_known_key() {
        // SwapFree fehlt vollständig (z. B. weil ihre Zeile keinen ':' trug
        // und deshalb schon von read_key_values übersprungen wurde).
        let pairs = pairs_from(&[
            ("MemTotal", "1024 kB"),
            ("MemFree", "512 kB"),
            ("MemAvailable", "768 kB"),
            ("SwapTotal", "0 kB"),
        ]);

        let err = select_known_fields(&pairs).expect_err("fehlender Schlüssel muss scheitern");
        assert!(matches!(err, SensorError::MalformedSource));
    }

    #[test]
    fn test_select_known_fields_rejects_empty_input() {
        let pairs: Vec<(String, String)> = Vec::new();
        let err = select_known_fields(&pairs).expect_err("leere Paarliste muss scheitern");
        assert!(matches!(err, SensorError::MalformedSource));
    }

    #[test]
    fn test_parse_meminfo_value_converts_kib_to_bytes() {
        assert_eq!(parse_meminfo_value("16316360 kB").unwrap(), 16_316_360 * 1024);
    }

    #[test]
    fn test_parse_meminfo_value_handles_value_without_unit_suffix() {
        // Der wichtigste Robustheitsfall für die Einheitenbehandlung: eine
        // Zeile ohne " kB" (wie "HugePages_Total: 0" in einem echten
        // /proc/meminfo) darf nicht scheitern und wird unverändert, ohne
        // Multiplikation, übernommen.
        assert_eq!(parse_meminfo_value("0").unwrap(), 0);
        assert_eq!(parse_meminfo_value("42").unwrap(), 42);
    }

    #[test]
    fn test_parse_meminfo_value_rejects_non_numeric_content_without_leaking_it() {
        let err = parse_meminfo_value("not-a-number kB")
            .expect_err("nicht-numerischer Wert muss scheitern");
        assert!(matches!(err, SensorError::MalformedSource));
        assert!(!err.to_string().contains("not-a-number"));
    }

    #[test]
    fn test_parse_meminfo_value_rejects_unknown_unit_suffix() {
        let err = parse_meminfo_value("1024 MB").expect_err("unbekannte Einheit muss scheitern");
        assert!(matches!(err, SensorError::MalformedSource));
    }

    #[test]
    fn test_parse_meminfo_value_rejects_empty_value() {
        let err = parse_meminfo_value("").expect_err("leerer Wert muss scheitern");
        assert!(matches!(err, SensorError::MalformedSource));
    }

    #[test]
    fn test_parse_meminfo_value_rejects_too_many_fields() {
        let err = parse_meminfo_value("1 2 kB").expect_err("zu viele Felder müssen scheitern");
        assert!(matches!(err, SensorError::MalformedSource));
    }

    #[test]
    fn test_max_cardinality_matches_known_fields_length() {
        assert_eq!(MAX_CARDINALITY, 5);
        assert_eq!(MAX_CARDINALITY, KNOWN_FIELDS.len());
    }

    #[test]
    fn test_capability_and_sensor_id_constants() {
        assert_eq!(MemorySensor::CAPABILITY, Capability::ReadProcMeminfo);
        assert_eq!(MemorySensor::SENSOR_ID, "memory");
    }
}
